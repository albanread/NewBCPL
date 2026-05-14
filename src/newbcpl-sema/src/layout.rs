//! Object layouts for NewBCPL classes.
//!
//! Sema records each class's *abstract* metadata — field names, type
//! hints, method signatures, inheritance — and this module turns it
//! into the *concrete* picture codegen needs:
//!
//! - byte offsets for every field within an instance,
//! - vtable slot assignments (CREATE at slot 0, RELEASE at slot 1,
//!   user methods sequential after; overrides keep their parent's
//!   slot),
//! - the pointer-offset array (`ptroffs`) the GC traces to find live
//!   references inside an instance,
//! - total instance size including the vtable header,
//! - whether the class declared `RELEASE` (so codegen can wire it as
//!   the GC finalizer per `newbcpl-runtime/src/gc.rs`).
//!
//! Every NewBCPL value is a single 64-bit word, so layout is simple:
//! the vtable pointer occupies offset 0 (8 bytes), and each
//! `DECL` / `LET` / `FLET` field takes 8 bytes thereafter. SIMD
//! values larger than a word, when stored as a class member, are
//! boxed — the field holds a pointer.
//!
//! This module is pure: the public `compute_layouts` function takes
//! the class table sema built up and returns a vector of
//! `ClassLayout`s with no further dependencies.

use std::collections::HashMap;

use newbcpl_parser::TypeHint;

use crate::ClassInfo;

/// Concrete byte-level layout for a single class.
#[derive(Debug, Clone)]
pub struct ClassLayout {
    pub class_name: String,
    /// Direct parent class for single inheritance (`CLASS Sub EXTENDS
    /// Base`). Lets IR lowering resolve `SUPER.method()` to the
    /// parent class's mangled name without re-walking the sema
    /// `ClassInfo` table.
    pub extends: Option<String>,
    /// Total bytes per instance, including the leading vtable
    /// pointer at offset 0.
    pub instance_size: usize,
    /// Every field, including those inherited from ancestors. Fields
    /// from the most-distant ancestor come first; the current class's
    /// new fields come last. Each entry's `offset` is bytes from the
    /// start of the instance.
    pub fields: Vec<FieldLayout>,
    /// Vtable in slot order. Slot 0 is always `CREATE`, slot 1 is
    /// always `RELEASE`; subsequent slots are user-declared methods
    /// in declaration order, with overrides keeping the original
    /// slot. A `defining_class` of `None` means "synthesize a default
    /// no-op" — used for classes that don't declare CREATE / RELEASE.
    pub vtable: Vec<VtableEntry>,
    /// Byte offsets within an instance where the GC must follow
    /// pointers — the input to NewCP's TypeDesc `ptroffs[]` array.
    /// Sorted ascending. Excludes the vtable pointer at offset 0
    /// (the GC handles that uniformly).
    pub ptr_offsets: Vec<usize>,
    /// Whether `RELEASE` is user-declared in this class or any
    /// ancestor. Codegen wires this method as the TypeDesc finalizer.
    pub has_release: bool,
    /// Whether the class is declared `MANAGED` (manifesto §5).
    pub managed: bool,
}

#[derive(Debug, Clone)]
pub struct FieldLayout {
    pub name: String,
    pub hint: TypeHint,
    pub offset: usize,
    /// The class that owns this field — either the current class or
    /// an ancestor it was inherited from.
    pub defining_class: String,
    /// For class-typed fields, the class the field's value belongs to,
    /// when sema can prove it (propagated from `ClassFieldInfo`).
    /// Lets IR lower chained access (`obj.inner.method()`) resolve
    /// without re-running sema's resolver.
    pub class_name: Option<String>,
}

#[derive(Debug, Clone)]
pub struct VtableEntry {
    pub slot: usize,
    pub method_name: String,
    /// The class providing the method body in this slot. `None`
    /// means synthesize a default no-op (slot 0 / 1 only — for
    /// classes that don't declare CREATE / RELEASE).
    pub defining_class: Option<String>,
    /// For methods that return a class instance, the class name
    /// (propagated from `ClassMethodInfo::result_class_name`). Lets
    /// IR lower resolve `obj.getInner().method()`.
    pub result_class: Option<String>,
}

/// Slot 0 is reserved for `CREATE`, slot 1 for `RELEASE`. These are
/// per-class lifecycle methods invoked by the runtime / GC.
const SLOT_CREATE: usize = 0;
const SLOT_RELEASE: usize = 1;

/// Word size — every BCPL field is one 64-bit word.
const WORD_BYTES: usize = 8;

/// Vtable pointer occupies the first word of every instance.
const VTABLE_HEADER_BYTES: usize = WORD_BYTES;

/// True when this hint represents a heap pointer the GC must trace.
/// Other hints (Word, Int, Float, Pair, etc.) are values stored
/// inline — the GC ignores them.
fn hint_is_pointer(h: TypeHint) -> bool {
    matches!(
        h,
        TypeHint::String
            | TypeHint::List
            | TypeHint::Vec
            | TypeHint::FVec
            | TypeHint::Object
    )
}

/// Public entry point. Walks the class table and produces a
/// `ClassLayout` for each class, with parents resolved before
/// subclasses so inherited fields and vtable slots are propagated.
///
/// The returned vector is sorted by class name for deterministic
/// dump-sema output; callers that want source-order can re-sort.
pub fn compute_layouts(classes: &HashMap<String, ClassInfo>) -> Vec<ClassLayout> {
    let mut layouts: HashMap<String, ClassLayout> = HashMap::new();
    let mut names: Vec<&String> = classes.keys().collect();
    names.sort();
    for name in names {
        if !layouts.contains_key(name) {
            compute_one(name, classes, &mut layouts);
        }
    }
    let mut result: Vec<ClassLayout> = layouts.into_values().collect();
    result.sort_by(|a, b| a.class_name.cmp(&b.class_name));
    result
}

fn compute_one(
    name: &str,
    classes: &HashMap<String, ClassInfo>,
    out: &mut HashMap<String, ClassLayout>,
) {
    let class = match classes.get(name) {
        Some(c) => c,
        None => return,
    };

    // Resolve parent recursively so inherited state is ready before
    // we extend it. A missing parent (EXTENDS Foo where Foo wasn't
    // declared) is silently treated as no parent — sema upstream
    // doesn't reject this and we don't either.
    let parent_layout = match &class.extends {
        Some(parent_name) => {
            if !out.contains_key(parent_name) {
                compute_one(parent_name, classes, out);
            }
            out.get(parent_name).cloned()
        }
        None => None,
    };

    let mut fields: Vec<FieldLayout>;
    let mut vtable: Vec<VtableEntry>;
    let mut ptr_offsets: Vec<usize>;
    let mut next_offset: usize;
    let inherited_release: bool;

    if let Some(p) = parent_layout {
        // Inherited block comes first; this class's new fields
        // append after.
        fields = p.fields.clone();
        vtable = p.vtable.clone();
        ptr_offsets = p.ptr_offsets.clone();
        next_offset = p.instance_size;
        inherited_release = p.has_release;
    } else {
        // Root class — start with the vtable header and the two
        // reserved lifecycle slots.
        fields = Vec::new();
        vtable = vec![
            VtableEntry {
                slot: SLOT_CREATE,
                method_name: "CREATE".to_string(),
                defining_class: None,
                result_class: None,
            },
            VtableEntry {
                slot: SLOT_RELEASE,
                method_name: "RELEASE".to_string(),
                defining_class: None,
                result_class: None,
            },
        ];
        ptr_offsets = Vec::new();
        next_offset = VTABLE_HEADER_BYTES;
        inherited_release = false;
    }

    // Append this class's fields, recording GC pointer offsets along
    // the way. Each field is exactly one word.
    for f in &class.fields {
        let offset = next_offset;
        if hint_is_pointer(f.hint) || f.class_name.is_some() {
            // Class-typed fields hold a pointer to a heap-allocated
            // instance — the GC must trace them even when the
            // declared hint is bare Word (DECL fields back-filled
            // with class identity from CREATE assignments).
            ptr_offsets.push(offset);
        }
        fields.push(FieldLayout {
            name: f.name.clone(),
            hint: f.hint,
            offset,
            defining_class: class.name.clone(),
            class_name: f.class_name.clone(),
        });
        next_offset += WORD_BYTES;
    }

    // Map existing slots so override detection is O(1). Walk the
    // declared methods: existing name → override the parent's slot;
    // new name → take the next free slot.
    let mut slot_by_method: HashMap<String, usize> = HashMap::new();
    for v in &vtable {
        slot_by_method.insert(v.method_name.clone(), v.slot);
    }
    let mut next_slot = vtable.iter().map(|v| v.slot + 1).max().unwrap_or(2);

    let mut declared_release = inherited_release;
    for m in &class.methods {
        if m.name == "RELEASE" {
            declared_release = true;
        }
        match slot_by_method.get(&m.name) {
            Some(&existing_slot) => {
                // Override: keep the slot, update defining_class.
                // The override gets to refine result_class (a subclass
                // may return a more-specific class than the parent).
                if let Some(entry) = vtable.iter_mut().find(|v| v.slot == existing_slot) {
                    entry.defining_class = Some(class.name.clone());
                    if m.result_class_name.is_some() {
                        entry.result_class = m.result_class_name.clone();
                    }
                }
            }
            None => {
                vtable.push(VtableEntry {
                    slot: next_slot,
                    method_name: m.name.clone(),
                    defining_class: Some(class.name.clone()),
                    result_class: m.result_class_name.clone(),
                });
                slot_by_method.insert(m.name.clone(), next_slot);
                next_slot += 1;
            }
        }
    }

    // Final slot order — already correct, but enforce for paranoia.
    vtable.sort_by_key(|v| v.slot);

    out.insert(
        class.name.clone(),
        ClassLayout {
            class_name: class.name.clone(),
            extends: class.extends.clone(),
            instance_size: next_offset,
            fields,
            vtable,
            ptr_offsets,
            has_release: declared_release,
            managed: class.managed,
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analyze;

    fn analyze_str(source: &str) -> crate::SemaOutput {
        let program = newbcpl_parser::parse_source(source).expect("parse");
        analyze(&program)
    }

    fn layout<'a>(out: &'a crate::SemaOutput, name: &str) -> &'a ClassLayout {
        out.layouts
            .iter()
            .find(|l| l.class_name == name)
            .unwrap_or_else(|| panic!("no layout for {name}"))
    }

    #[test]
    fn root_class_reserves_create_and_release_slots() {
        let out = analyze_str("CLASS Foo $( DECL x $)");
        let l = layout(&out, "Foo");
        assert_eq!(l.vtable[0].slot, 0);
        assert_eq!(l.vtable[0].method_name, "CREATE");
        assert!(l.vtable[0].defining_class.is_none());
        assert_eq!(l.vtable[1].slot, 1);
        assert_eq!(l.vtable[1].method_name, "RELEASE");
        assert!(l.vtable[1].defining_class.is_none());
    }

    #[test]
    fn fields_take_word_slots_after_vtable_pointer() {
        let out = analyze_str("CLASS Point $( DECL x, y $)");
        let l = layout(&out, "Point");
        assert_eq!(l.fields.len(), 2);
        assert_eq!(l.fields[0].name, "x");
        assert_eq!(l.fields[0].offset, 8); // after vtable header
        assert_eq!(l.fields[1].name, "y");
        assert_eq!(l.fields[1].offset, 16);
        // Vtable pointer (8) + two fields (16) = 24 bytes.
        assert_eq!(l.instance_size, 24);
    }

    #[test]
    fn pointer_typed_field_records_ptroffs() {
        let out = analyze_str(
            "CLASS Container $( DECL count\n LET label = \"hi\"\n LET items = LIST(1, 2, 3) $)",
        );
        let l = layout(&out, "Container");
        // Field offsets: count at 8 (Word, no ptroff), label at 16
        // (String, ptroff), items at 24 (List, ptroff).
        assert_eq!(l.ptr_offsets, vec![16, 24]);
    }

    #[test]
    fn user_methods_take_slots_two_onwards() {
        let out = analyze_str(
            "CLASS Counter $(\n  ROUTINE inc() BE $( $)\n  FUNCTION get() = 0\n$)",
        );
        let l = layout(&out, "Counter");
        // Slots 0 and 1 are CREATE / RELEASE; user methods follow.
        let inc = l.vtable.iter().find(|v| v.method_name == "inc").unwrap();
        let get = l.vtable.iter().find(|v| v.method_name == "get").unwrap();
        assert!(inc.slot >= 2);
        assert!(get.slot >= 2);
        assert_ne!(inc.slot, get.slot);
        assert_eq!(inc.defining_class.as_deref(), Some("Counter"));
    }

    #[test]
    fn user_create_overrides_synthesized_default() {
        let out = analyze_str(
            "CLASS Foo $(\n  ROUTINE CREATE(x) BE $( $)\n$)",
        );
        let l = layout(&out, "Foo");
        let create = l.vtable.iter().find(|v| v.slot == 0).unwrap();
        assert_eq!(create.method_name, "CREATE");
        assert_eq!(create.defining_class.as_deref(), Some("Foo"));
    }

    #[test]
    fn user_release_sets_has_release_flag() {
        let out = analyze_str(
            "CLASS Window MANAGED $(\n  DECL h\n  ROUTINE RELEASE() BE $( $)\n$)",
        );
        let l = layout(&out, "Window");
        assert!(l.has_release);
        assert!(l.managed);
        let release = l.vtable.iter().find(|v| v.slot == 1).unwrap();
        assert_eq!(release.defining_class.as_deref(), Some("Window"));
    }

    #[test]
    fn subclass_inherits_parent_fields_then_appends_its_own() {
        let out = analyze_str(
            "CLASS Animal $( DECL legs $)\nCLASS Dog EXTENDS Animal $( DECL breed $)",
        );
        let dog = layout(&out, "Dog");
        // Parent field first (legs at 8), then this class's field
        // (breed at 16).
        assert_eq!(dog.fields[0].name, "legs");
        assert_eq!(dog.fields[0].offset, 8);
        assert_eq!(dog.fields[0].defining_class, "Animal");
        assert_eq!(dog.fields[1].name, "breed");
        assert_eq!(dog.fields[1].offset, 16);
        assert_eq!(dog.fields[1].defining_class, "Dog");
        assert_eq!(dog.instance_size, 24);
    }

    #[test]
    fn override_keeps_parent_slot_marks_subclass_provider() {
        let out = analyze_str(
            "CLASS Animal $(\n  VIRTUAL ROUTINE makeSound() BE $( $)\n$)\nCLASS Dog EXTENDS Animal $(\n  ROUTINE makeSound() BE $( $)\n$)",
        );
        let dog = layout(&out, "Dog");
        let parent_slot = dog
            .vtable
            .iter()
            .find(|v| v.method_name == "makeSound")
            .unwrap();
        // The slot from Animal is preserved; defining_class is now Dog.
        assert_eq!(parent_slot.defining_class.as_deref(), Some("Dog"));
        // Only one entry for makeSound — no duplicate slot from override.
        let count = dog
            .vtable
            .iter()
            .filter(|v| v.method_name == "makeSound")
            .count();
        assert_eq!(count, 1);
    }

    #[test]
    fn ptr_offsets_inherit_from_parent() {
        let out = analyze_str(
            "CLASS Animal $( LET name = \"\" $)\nCLASS Dog EXTENDS Animal $( LET breed = \"\" $)",
        );
        let dog = layout(&out, "Dog");
        // Two STRING fields → two ptroffs at 8 and 16.
        assert_eq!(dog.ptr_offsets, vec![8, 16]);
    }
}
