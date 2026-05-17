//! NewBCPL semantic analysis.
//!
//! Bootstrap shape per `docs/manifesto.md` §1 *Looks untyped, secretly
//! typed*: walk the AST once, attach a register-class type hint to every
//! binding, never error on type grounds.
//!
//! The hint vocabulary is the §2 *Be close to the machine* lattice — each
//! variant names a concrete register class so codegen can pick FADD vs
//! IADD, scalar vs SIMD, etc. without re-deriving anything.
//!
//! What this bootstrap does:
//!
//! - Tracks bindings introduced by `LET` / `FLET` / `STATIC` / `MANIFEST`
//!   / `GLOBAL[S]` / class members.
//! - Infers each binding's hint from its initialiser (`LET x = 3.14` →
//!   FLOAT; `LET x = LIST(…)` → LIST; `LET x = NEW Foo()` → OBJECT;
//!   `LET x = vec!i` → WORD).
//! - Flow-types operator results (`a + b` is FLOAT iff both sides are
//!   FLOAT; otherwise INT).
//! - Warns on assignments that would emit implicit FCVTZS truncation
//!   (FLOAT → INT). Never errors.
//! - Records visited classes and their members for later phases.
//!
//! What this bootstrap does NOT do (yet):
//!
//! - Track function signatures or method bodies separately. Calls are
//!   currently treated as returning WORD unless the callee resolves to
//!   a known intrinsic (e.g. `FLOAT(x)` → FLOAT, `LEN x` → INT).
//! - Resolve member accesses through a class hierarchy. `obj.field` is
//!   currently WORD. Sema will gain class-aware member typing once we
//!   need it for codegen.
//! - Validate `MANAGED` linear-type rules (no aliasing, no list
//!   storage). That's a separate pass once the parser-level discovery
//!   here is solid.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::Path;

use newbcpl_parser::{
    BinaryOp, Block, ClassDecl, ClassMember, ClassMemberKind, ClassMethodBody, Decl, Expr,
    LetDecl, LetKind, Program, Span, Stmt, TypeConstructorKind, UnaryOp, Visibility,
};

pub use newbcpl_parser::TypeHint;

pub mod layout;
pub use layout::{ClassLayout, FieldLayout, VtableEntry};

/// Stable, comparable identifier for a binding. Codegen uses these
/// rather than name-based string lookup so multiple references to the
/// same variable land on a single storage slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SymbolId(pub u32);

impl SymbolId {
    pub fn raw(self) -> u32 {
        self.0
    }
}

impl std::fmt::Display for SymbolId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "s{}", self.0)
    }
}

#[derive(Debug, Clone)]
pub struct BindingInfo {
    /// Stable, name-independent identifier. Allocated by sema in
    /// declaration order. Codegen looks bindings up by this ID.
    pub id: SymbolId,
    pub name: String,
    pub hint: TypeHint,
    /// For `Object` bindings, the class name they were created from
    /// (best effort — `NEW Foo()` produces `Some("Foo")`; `LET b = a`
    /// propagates `a`'s class).
    pub class_name: Option<String>,
    /// True when `class_name` resolves to a class declared `MANAGED`.
    /// Manifesto §5 — these instances are linear, cannot be aliased
    /// or stored in containers; sema emits warnings on violations.
    pub is_managed: bool,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct ClassInfo {
    pub name: String,
    pub extends: Option<String>,
    pub managed: bool,
    pub fields: Vec<ClassFieldInfo>,
    pub methods: Vec<ClassMethodInfo>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct ClassFieldInfo {
    pub name: String,
    pub hint: TypeHint,
    /// For class-typed fields, the class this field's value belongs
    /// to (when sema can prove it). Populated from `LET f = NEW Foo()`
    /// initialisers, `AS Class` annotations, and a second pass over
    /// CREATE-body assignments. Lets chained member access such as
    /// `obj.inner.getValue()` resolve through the second hop.
    pub class_name: Option<String>,
    pub visibility: Visibility,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct ClassMethodInfo {
    pub name: String,
    pub kind: FunctionKind,
    pub params: Vec<String>,
    pub result: TypeHint,
    /// For methods that return a class instance (`FUNCTION m() = SELF.inner`
    /// where `inner` is class-typed, or `= NEW Foo()`), the class name.
    /// Enables chained dispatch like `obj.getInner().method()`.
    pub result_class_name: Option<String>,
    pub is_virtual: bool,
    pub is_final: bool,
    pub visibility: Visibility,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct SemaWarning {
    pub message: String,
    pub span: Span,
}

impl SemaWarning {
    pub fn render(&self) -> String {
        format!(
            "{} at {}:{}",
            self.message, self.span.start.line, self.span.start.column
        )
    }
}

/// A hard sema diagnostic. Unlike `SemaWarning`, the driver refuses
/// to proceed to IR/codegen when any are present. Reserved for
/// violations that aren't *type* questions (which sema never errors
/// on, per the manifesto) but *meaning* questions — e.g. accessing
/// a `PRIVATE` member from outside its class. The user guide
/// promises these are enforced; sema is where the enforcement lives.
#[derive(Debug, Clone)]
pub struct SemaError {
    pub message: String,
    pub span: Span,
}

impl SemaError {
    pub fn render(&self) -> String {
        format!(
            "{} at {}:{}",
            self.message, self.span.start.line, self.span.start.column
        )
    }
}

#[derive(Debug, Clone, Default)]
pub struct SemaOutput {
    /// Every binding sema observed, in declaration order.
    pub bindings: Vec<BindingInfo>,
    /// Every class declared.
    pub classes: Vec<ClassInfo>,
    /// Every function / routine sema saw, with its inferred signature.
    pub functions: Vec<FunctionInfo>,
    /// Concrete object layouts — field offsets, vtable slots,
    /// pointer-offset arrays. One per declared class. Computed
    /// after the main walk so all `ClassInfo` records (including
    /// inferred method results) are available.
    pub layouts: Vec<ClassLayout>,
    /// `MANIFEST` constants — name → integer value. Lowering uses
    /// this to substitute the literal value at every reference site
    /// (BCPL convention: a MANIFEST is a compile-time constant, not
    /// a real binding with a runtime address).
    pub manifests: std::collections::HashMap<String, i64>,
    /// Names declared with `GLOBAL`. Each becomes an LLVM
    /// module-level `@<name>` global with the given initialiser.
    /// IR lowering consults this set so reads/writes of a global
    /// emit `GlobalLoad` / `GlobalStore` instead of falling through
    /// to the unbound-extern path. Maps name → initial integer
    /// value when the initialiser is a compile-time constant; `None`
    /// otherwise (codegen leaves the slot zero-initialised then).
    pub globals: std::collections::HashMap<String, Option<i64>>,
    /// Non-fatal diagnostics. Sema never fails on type grounds, so
    /// every interesting observation lands here.
    pub warnings: Vec<SemaWarning>,
    /// Hard diagnostics — the driver refuses to JIT a program when
    /// these are non-empty. Currently populated only by visibility
    /// enforcement (`PRIVATE` / `PROTECTED` member access).
    pub errors: Vec<SemaError>,
}

#[derive(Debug, Clone)]
pub struct FunctionInfo {
    pub name: String,
    pub kind: FunctionKind,
    pub params: Vec<String>,
    /// Inferred result type. For routines this is `Word` (BCPL
    /// routines do not produce a value); for functions it is the
    /// hint of the body expression, threading through any VALOF /
    /// RESULTIS chain.
    pub result: TypeHint,
    /// For functions that return a class instance, the class name.
    /// Lets callers of the function reason about the result's class
    /// the same way they would for a `NEW Foo()` expression.
    pub result_class_name: Option<String>,
    pub span: Span,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FunctionKind {
    /// `LET F(x) = expr` — produces a value.
    Function,
    /// `LET R(x) BE stmt` — produces no value.
    Routine,
}

pub fn analyze(program: &Program) -> SemaOutput {
    let mut sema = Sema::new();
    sema.analyze_program(program);
    let layouts = layout::compute_layouts(&sema.classes);
    SemaOutput {
        bindings: sema.binding_log,
        classes: sema.class_log,
        functions: sema.function_log,
        layouts,
        manifests: sema.manifests,
        globals: sema.globals,
        warnings: sema.warnings,
        errors: sema.errors,
    }
}

pub fn dump_sema(path: &Path) -> String {
    match std::fs::read_to_string(path) {
        Ok(source) => match newbcpl_parser::parse_source(&source) {
            Ok(program) => {
                let result = analyze(&program);
                let mut out = format!(
                    "newbcpl-sema dump\ninput: {}\n",
                    path.display()
                );
                writeln!(out, "functions ({}):", result.functions.len()).unwrap();
                for f in &result.functions {
                    let kind = match f.kind {
                        FunctionKind::Function => "FUNCTION",
                        FunctionKind::Routine => "ROUTINE",
                    };
                    writeln!(
                        out,
                        "  {:>4}:{:<3}  {kind:<8} {}({}) -> {}",
                        f.span.start.line,
                        f.span.start.column,
                        f.name,
                        f.params.join(", "),
                        f.result.as_str(),
                    )
                    .unwrap();
                }
                writeln!(out, "bindings ({}):", result.bindings.len()).unwrap();
                for b in &result.bindings {
                    let class = match &b.class_name {
                        Some(c) => format!(" [{c}]"),
                        None => String::new(),
                    };
                    let managed = if b.is_managed { " M" } else { "" };
                    writeln!(
                        out,
                        "  {:>4}:{:<3}  {:>5}  {:<12} {}{class}{managed}",
                        b.span.start.line,
                        b.span.start.column,
                        format!("{}", b.id),
                        b.hint.as_str(),
                        b.name,
                    )
                    .unwrap();
                }
                if !result.layouts.is_empty() {
                    writeln!(out, "layouts ({}):", result.layouts.len()).unwrap();
                    for l in &result.layouts {
                        let managed = if l.managed { " MANAGED" } else { "" };
                        let release = if l.has_release { " has-RELEASE" } else { "" };
                        writeln!(
                            out,
                            "  {}{managed}{release}  size={} bytes  ptroffs={:?}",
                            l.class_name, l.instance_size, l.ptr_offsets,
                        )
                        .unwrap();
                        for f in &l.fields {
                            let from = if f.defining_class != l.class_name {
                                format!(" (from {})", f.defining_class)
                            } else {
                                String::new()
                            };
                            writeln!(
                                out,
                                "    +{:<3}  {:<8} {}{from}",
                                f.offset,
                                f.hint.as_str(),
                                f.name
                            )
                            .unwrap();
                        }
                        for v in &l.vtable {
                            let provider = match &v.defining_class {
                                Some(c) => c.as_str(),
                                None => "(default)",
                            };
                            writeln!(
                                out,
                                "    slot {}  {:<14}  {}",
                                v.slot, v.method_name, provider,
                            )
                            .unwrap();
                        }
                    }
                }
                writeln!(out, "classes ({}):", result.classes.len()).unwrap();
                for c in &result.classes {
                    let extends = match &c.extends {
                        Some(e) => format!(" extends {e}"),
                        None => String::new(),
                    };
                    let managed = if c.managed { " MANAGED" } else { "" };
                    writeln!(
                        out,
                        "  {}{extends}{managed}  ({} fields, {} methods)",
                        c.name,
                        c.fields.len(),
                        c.methods.len()
                    )
                    .unwrap();
                    for f in &c.fields {
                        let vis = match f.visibility {
                            Visibility::Public => "pub ",
                            Visibility::Private => "priv ",
                            Visibility::Protected => "prot ",
                        };
                        writeln!(out, "    {vis}field {} : {}", f.name, f.hint.as_str())
                            .unwrap();
                    }
                    for m in &c.methods {
                        let vis = match m.visibility {
                            Visibility::Public => "pub ",
                            Visibility::Private => "priv ",
                            Visibility::Protected => "prot ",
                        };
                        let kind = match m.kind {
                            FunctionKind::Function => "FUNCTION",
                            FunctionKind::Routine => "ROUTINE",
                        };
                        let virt = if m.is_virtual { "virtual " } else { "" };
                        let final_ = if m.is_final { "final " } else { "" };
                        writeln!(
                            out,
                            "    {vis}{virt}{final_}{kind} {}({}) -> {}",
                            m.name,
                            m.params.join(", "),
                            m.result.as_str()
                        )
                        .unwrap();
                    }
                }
                writeln!(out, "warnings ({}):", result.warnings.len()).unwrap();
                for w in &result.warnings {
                    writeln!(out, "  {}", w.render()).unwrap();
                }
                out
            }
            Err(error) => format!(
                "newbcpl-sema dump\ninput: {}\nparse error: {}",
                path.display(),
                error.render()
            ),
        },
        Err(error) => format!(
            "newbcpl-sema dump\ninput: {}\nio-error: {}",
            path.display(),
            error
        ),
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Walker
// ─────────────────────────────────────────────────────────────────────────────

struct Sema {
    /// Monotonic counter for `SymbolId`. Allocated once per binding
    /// in `declare`, never reused.
    next_symbol_id: u32,
    /// Stack of scope frames. Each frame maps name → hint. Newer
    /// frames shadow older ones.
    scopes: Vec<HashMap<String, BindingInfo>>,
    /// Class table by name.
    classes: HashMap<String, ClassInfo>,
    /// Function table by name. Used by the call-site type lookup so
    /// `LET y = f(x)` can take y's hint from f's inferred result type.
    functions: HashMap<String, FunctionInfo>,
    /// Append-only log of every binding seen, in source order.
    binding_log: Vec<BindingInfo>,
    /// Append-only log of every class seen, in source order.
    class_log: Vec<ClassInfo>,
    /// Append-only log of every function / routine seen.
    function_log: Vec<FunctionInfo>,
    /// Stack of currently-open `VALOF` blocks. Each frame collects the
    /// `RESULTIS` expression hints inside that block; on exit the
    /// frame is popped and merged into a single `TypeHint`. Empty when
    /// we're not inside any VALOF — `RESULTIS` in that case warns.
    valof_results: Vec<Vec<TypeHint>>,
    /// `MANIFEST` constants — name → integer value. Lowering uses
    /// this to substitute the literal value at every reference site.
    manifests: HashMap<String, i64>,
    /// `GLOBAL` bindings — name → optional compile-time int
    /// initialiser. IR lowering emits an LLVM module-level global
    /// for each entry; reads/writes route through it instead of
    /// trying to resolve a local slot.
    globals: HashMap<String, Option<i64>>,
    /// How many loop bodies (WHILE / UNTIL / FOR / FOREACH / REPEAT
    /// family) are currently open. `BREAK` / `LOOP` warn when 0.
    loop_depth: u32,
    /// How many SWITCHON bodies are currently open. `ENDCASE` warns
    /// when 0.
    switchon_depth: u32,
    /// Set while analysing a class method body so visibility checks
    /// can answer "is this access happening from inside class X?".
    /// `None` outside any class body — top-level routines and free
    /// functions. Visibility checks reject `PRIVATE` / `PROTECTED`
    /// accesses when `current_class` doesn't match the member's
    /// declaring class (or a descendant, for `PROTECTED`).
    current_class: Option<String>,
    warnings: Vec<SemaWarning>,
    errors: Vec<SemaError>,
}

impl Sema {
    fn new() -> Self {
        Self {
            next_symbol_id: 0,
            scopes: vec![HashMap::new()],
            classes: HashMap::new(),
            functions: HashMap::new(),
            binding_log: Vec::new(),
            class_log: Vec::new(),
            function_log: Vec::new(),
            valof_results: Vec::new(),
            manifests: HashMap::new(),
            globals: HashMap::new(),
            loop_depth: 0,
            switchon_depth: 0,
            current_class: None,
            warnings: Vec::new(),
            errors: Vec::new(),
        }
    }

    fn alloc_symbol_id(&mut self) -> SymbolId {
        let id = SymbolId(self.next_symbol_id);
        self.next_symbol_id += 1;
        id
    }

    fn push_scope(&mut self) {
        self.scopes.push(HashMap::new());
    }

    fn pop_scope(&mut self) {
        self.scopes.pop();
        if self.scopes.is_empty() {
            // Defensive: never empty the stack completely.
            self.scopes.push(HashMap::new());
        }
    }

    fn declare(&mut self, name: &str, hint: TypeHint, class_name: Option<String>, span: Span) {
        let is_managed = class_name
            .as_deref()
            .map(|c| self.class_is_managed(c))
            .unwrap_or(false);
        let info = BindingInfo {
            id: self.alloc_symbol_id(),
            name: name.to_string(),
            hint,
            class_name: class_name.clone(),
            is_managed,
            span,
        };
        self.binding_log.push(info.clone());
        if let Some(top) = self.scopes.last_mut() {
            top.insert(name.to_string(), info);
        }
    }

    fn class_is_managed(&self, class: &str) -> bool {
        self.classes.get(class).map(|c| c.managed).unwrap_or(false)
    }

    /// Best-effort: return true when an expression evaluates to a
    /// MANAGED-class instance. Kept available for any future diagnostic
    /// that wants the signal; the linearity warnings that originally
    /// called it were retired when `USING` blocks landed.
    #[allow(dead_code)]
    fn expr_is_managed(&self, e: &Expr) -> bool {
        match e {
            Expr::Ident { name, .. } => self
                .lookup(name)
                .map(|info| info.is_managed)
                .unwrap_or(false),
            Expr::New { class_name, .. } => self.class_is_managed(class_name),
            _ => false,
        }
    }

    fn lookup(&self, name: &str) -> Option<&BindingInfo> {
        for frame in self.scopes.iter().rev() {
            if let Some(info) = frame.get(name) {
                return Some(info);
            }
        }
        None
    }

    fn warn(&mut self, message: impl Into<String>, span: Span) {
        self.warnings.push(SemaWarning {
            message: message.into(),
            span,
        });
    }

    fn error(&mut self, message: impl Into<String>, span: Span) {
        self.errors.push(SemaError {
            message: message.into(),
            span,
        });
    }

    /// Reject any method on `c` that overrides an ancestor's method
    /// marked `FINAL`. Walks `c.extends` chain looking for a method
    /// with the same name; if one is found with `is_final == true`,
    /// emits a hard `SemaError` pointing at the offending override
    /// site. The error message names both classes so the user sees
    /// exactly which inheritance hop introduces the conflict.
    fn check_final_overrides(&mut self, c: &ClassDecl) {
        // The class hasn't necessarily been refined yet, so walk the
        // AST's members directly rather than `self.classes`. We do
        // need the ancestor's method list, so we use `self.classes`
        // for the lookup direction.
        let Some(parent_name) = c.extends.as_ref() else {
            return;
        };
        for member in &c.members {
            let ClassMemberKind::Method(method) = &member.kind else {
                continue;
            };
            // Walk ancestors and look for a same-name method.
            let mut current = Some(parent_name.clone());
            while let Some(ancestor_name) = current {
                let Some(ancestor) = self.classes.get(&ancestor_name).cloned() else {
                    break;
                };
                if let Some(ancestor_method) =
                    ancestor.methods.iter().find(|m| m.name == method.name)
                {
                    if ancestor_method.is_final {
                        self.error(
                            format!(
                                "cannot override FINAL method `{}` from class `{}`",
                                method.name, ancestor_name
                            ),
                            method.span,
                        );
                    }
                    // Found the method (FINAL or not) — first ancestor
                    // with this name wins; stop the walk regardless.
                    break;
                }
                current = ancestor.extends.clone();
            }
        }
    }

    /// True iff `candidate` is `ancestor` or transitively extends it.
    /// Walks the class table's `extends` chain. Used by visibility
    /// checks to grant `PROTECTED` access from any subclass.
    fn is_class_or_descendant_of(&self, candidate: &str, ancestor: &str) -> bool {
        let mut cur = Some(candidate.to_string());
        while let Some(name) = cur {
            if name == ancestor {
                return true;
            }
            cur = self.classes.get(&name).and_then(|c| c.extends.clone());
        }
        false
    }

    /// Enforce `PUBLIC` / `PRIVATE` / `PROTECTED` at a member-access
    /// site. The defining-class declares the member's visibility;
    /// `self.current_class` is the class whose method body the
    /// access is occurring inside (or `None` for top-level code).
    ///
    /// - `PUBLIC` — always allowed.
    /// - `PRIVATE` — only when `current_class == defining_class`.
    /// - `PROTECTED` — `current_class` is `defining_class` or any
    ///   descendant. Mirrors C++ / classical OO semantics.
    fn check_member_visibility(
        &mut self,
        defining_class: &str,
        member_name: &str,
        visibility: Visibility,
        span: Span,
    ) {
        if matches!(visibility, Visibility::Public) {
            return;
        }
        let access = self.current_class.clone();
        let allowed = match visibility {
            Visibility::Public => true,
            Visibility::Private => access.as_deref() == Some(defining_class),
            Visibility::Protected => match access.as_deref() {
                Some(ac) => self.is_class_or_descendant_of(ac, defining_class),
                None => false,
            },
        };
        if allowed {
            return;
        }
        let vis_word = match visibility {
            Visibility::Private => "private",
            Visibility::Protected => "protected",
            Visibility::Public => unreachable!(),
        };
        let from = match access {
            Some(c) => format!("from class `{c}`"),
            None => "from top-level code".to_string(),
        };
        self.error(
            format!(
                "`{defining_class}.{member_name}` is {vis_word} — cannot access {from}"
            ),
            span,
        );
    }

    /// Built-in MANIFEST constants that sema pre-seeds before
    /// analysing the program. Used for atom-type tags (`TYPE_INT`,
    /// `TYPE_STRING`, …) so SWITCHON case labels resolve to integer
    /// literals — code can't write `CASE atom_type:` against a
    /// runtime call, the case discriminant must be compile-time.
    /// Values mirror `newbcpl-runtime::builtins::ATOM_*`. If the
    /// program redefines one its `Decl::Manifest` handler wins
    /// (overwrites the prelude entry).
    fn seed_builtin_manifests(&mut self) {
        const PRELUDE: &[(&str, i64)] = &[
            ("TYPE_INT", 1),
            ("TYPE_FLOAT", 2),
            ("TYPE_STRING", 3),
            ("TYPE_LIST", 4),
            ("TYPE_OBJECT", 5),
            ("TYPE_PAIR", 6),
        ];
        for (name, value) in PRELUDE {
            self.manifests.insert((*name).to_string(), *value);
        }
    }

    fn analyze_program(&mut self, program: &Program) {
        self.seed_builtin_manifests();
        // Pre-pass 1: register classes so any LET that does
        // `NEW Foo()` can resolve the name even if Foo is declared
        // later in the file.
        for decl in &program.items {
            if let Decl::Class(c) = decl {
                self.register_class(c);
            }
        }
        // Pre-pass 1b: now that every class has a record, resolve any
        // `AS Class` annotation on a class member's LET-initialiser
        // into the field's `class_name`. Forward references
        // (`CLASS Outer $( LET inner AS Inner $)` declared above
        // `Inner`) only work because this pass runs after every class
        // has been registered.
        for decl in &program.items {
            if let Decl::Class(c) = decl {
                self.refine_class_field_annotations(c);
            }
        }
        // Pre-pass 1c: enforce `FINAL` — a subclass cannot override a
        // method that an ancestor marked `FINAL`. We do this after
        // every class is registered so the inheritance walk has
        // complete information. Errors are routed through the same
        // hard-diagnostic channel as visibility violations, so the
        // driver refuses to proceed to IR/codegen.
        for decl in &program.items {
            if let Decl::Class(c) = decl {
                self.check_final_overrides(c);
            }
        }
        // Pre-pass 2: register functions / routines with placeholder
        // result hints so forward calls (`g()` referenced before
        // `g`'s body is seen) resolve. Real inference happens during
        // the main walk and overwrites the placeholder.
        for decl in &program.items {
            self.preregister_functions_in_decl(decl);
        }
        // Main pass.
        for decl in &program.items {
            self.analyze_decl(decl);
        }
    }

    /// After every class is in `self.classes`, walk one class's members
    /// and back-fill `class_name` on any LET-form field whose AST
    /// annotation names a known class. The initial `register_class`
    /// pass only sets `class_name` from direct `Expr::New` evidence
    /// because forward-referenced classes aren't yet registered
    /// there.
    fn refine_class_field_annotations(&mut self, c: &ClassDecl) {
        for m in &c.members {
            match &m.kind {
                ClassMemberKind::Let(let_decl) => {
                    for (idx, (field_name, _)) in let_decl.bindings.iter().enumerate() {
                        let Some(Some(annotation)) = let_decl.annotations.get(idx) else {
                            continue;
                        };
                        let Some(class_name) =
                            self.class_name_from_annotation(annotation)
                        else {
                            continue;
                        };
                        self.set_field_class_if_unset(&c.name, field_name, &class_name);
                    }
                }
                ClassMemberKind::Fields { names, annotations } => {
                    for (idx, field_name) in names.iter().enumerate() {
                        let Some(Some(annotation)) = annotations.get(idx) else {
                            continue;
                        };
                        let Some(class_name) =
                            self.class_name_from_annotation(annotation)
                        else {
                            continue;
                        };
                        self.set_field_class_if_unset(&c.name, field_name, &class_name);
                    }
                }
                _ => {}
            }
        }
    }

    /// First-write-wins assignment of `class_name` onto a field's
    /// `ClassFieldInfo`, mirrored into the parallel `class_log` entry
    /// so dump-sema reflects the refined identity.
    fn set_field_class_if_unset(&mut self, class_name: &str, field_name: &str, value: &str) {
        if let Some(class_info) = self.classes.get_mut(class_name) {
            if let Some(f) = class_info.fields.iter_mut().find(|f| f.name == *field_name) {
                if f.class_name.is_none() {
                    f.class_name = Some(value.to_string());
                }
            }
        }
        if let Some(class_log_entry) = self.class_log.iter_mut().find(|ci| ci.name == class_name) {
            if let Some(f) = class_log_entry
                .fields
                .iter_mut()
                .find(|f| f.name == *field_name)
            {
                if f.class_name.is_none() {
                    f.class_name = Some(value.to_string());
                }
            }
        }
    }

    fn preregister_functions_in_decl(&mut self, decl: &Decl) {
        match decl {
            Decl::Function(f) => {
                let info = FunctionInfo {
                    name: f.name.clone(),
                    kind: FunctionKind::Function,
                    params: f.params.clone(),
                    result: TypeHint::Unknown,
                    result_class_name: None,
                    span: f.span,
                };
                self.functions.insert(f.name.clone(), info);
            }
            Decl::Routine(r) => {
                let info = FunctionInfo {
                    name: r.name.clone(),
                    kind: FunctionKind::Routine,
                    params: r.params.clone(),
                    result: TypeHint::Word,
                    result_class_name: None,
                    span: r.span,
                };
                self.functions.insert(r.name.clone(), info);
            }
            Decl::AsmProc(a) => {
                // `LET f(…) = ASM { … }` is a value-producing function;
                // `LET f(…) BE ASM { … }` is a no-return routine. Both
                // report TypeHint::Word for the return value — the body
                // is opaque to sema, so we trust the FFI declaration the
                // IR/LLVM pass will emit.
                let kind = if a.is_function {
                    FunctionKind::Function
                } else {
                    FunctionKind::Routine
                };
                let info = FunctionInfo {
                    name: a.name.clone(),
                    kind,
                    params: a.params.clone(),
                    result: TypeHint::Word,
                    result_class_name: None,
                    span: a.span,
                };
                self.functions.insert(a.name.clone(), info);
            }
            _ => {}
        }
    }

    fn register_class(&mut self, c: &ClassDecl) {
        // Walk members once to collect field hints. Methods are
        // recorded with their declared shape; their result hints are
        // refined later when the body is actually analysed.
        let mut fields: Vec<ClassFieldInfo> = Vec::new();
        let mut methods: Vec<ClassMethodInfo> = Vec::new();
        for m in &c.members {
            match &m.kind {
                ClassMemberKind::Fields { names, annotations: _ } => {
                    // `DECL x AS Class` carries the annotation, but
                    // resolving it requires the full class table.
                    // Defer that to `refine_class_field_annotations`
                    // (the same post-pass that resolves LET-form
                    // annotations) — that way forward references like
                    // `CLASS Outer $( DECL inner AS Inner $)` declared
                    // above `Inner` still work.
                    for n in names {
                        fields.push(ClassFieldInfo {
                            name: n.clone(),
                            // DECL has no initialiser — bag-of-bits Word
                            // unless a class annotation refines it later.
                            hint: TypeHint::Word,
                            class_name: None,
                            visibility: m.visibility,
                            span: m.span,
                        });
                    }
                }
                ClassMemberKind::Let(let_decl) => {
                    for (name, expr) in &let_decl.bindings {
                        // Direct evidence only at this stage: `LET f = NEW
                        // Foo()`. Annotation- and assignment-based
                        // inference happen post-hoc in
                        // `refine_class_field_identities` once the full
                        // class table is built.
                        let class_name = match expr {
                            Expr::New { class_name, .. } => Some(class_name.clone()),
                            _ => None,
                        };
                        fields.push(ClassFieldInfo {
                            name: name.clone(),
                            hint: literal_hint(expr),
                            class_name,
                            visibility: m.visibility,
                            span: m.span,
                        });
                    }
                }
                ClassMemberKind::FLet(b) => {
                    let hint = b
                        .value
                        .as_ref()
                        .map(literal_hint)
                        .unwrap_or(TypeHint::Float);
                    fields.push(ClassFieldInfo {
                        name: b.name.clone(),
                        hint: if hint == TypeHint::Word || hint == TypeHint::Int {
                            TypeHint::Float
                        } else {
                            hint
                        },
                        // FLET fields are always FLOAT — no class identity.
                        class_name: None,
                        visibility: m.visibility,
                        span: m.span,
                    });
                }
                ClassMemberKind::Method(method) => {
                    methods.push(ClassMethodInfo {
                        name: method.name.clone(),
                        kind: if matches!(method.body, ClassMethodBody::Function(_)) {
                            FunctionKind::Function
                        } else {
                            FunctionKind::Routine
                        },
                        params: method.params.clone(),
                        // Refined later in analyze_class_member.
                        result: TypeHint::Unknown,
                        result_class_name: None,
                        is_virtual: method.is_virtual,
                        is_final: method.is_final,
                        visibility: m.visibility,
                        span: method.span,
                    });
                }
            }
        }
        // If the class has any field initialisers (`LET f = expr` /
        // `FLET f = expr` members) but the user didn't write CREATE,
        // inject a synthetic CREATE entry so the layout marks slot 0
        // as defined. IR lowering will emit the matching synthetic
        // `<Class>_CREATE(self)` whose body runs the initialisers,
        // and the vtable patcher will wire slot 0 to it. Without this,
        // a SUPER.CREATE() in a subclass would dispatch to the no-op
        // default-method stub.
        let has_user_create = methods.iter().any(|m| m.name == "CREATE");
        let has_initialisers = c.members.iter().any(|m| match &m.kind {
            ClassMemberKind::Let(let_decl) => !let_decl.bindings.is_empty(),
            ClassMemberKind::FLet(b) => b.value.is_some(),
            _ => false,
        });
        if has_initialisers && !has_user_create {
            methods.push(ClassMethodInfo {
                name: "CREATE".to_string(),
                kind: FunctionKind::Routine,
                params: Vec::new(),
                result: TypeHint::Word,
                result_class_name: None,
                is_virtual: false,
                is_final: false,
                visibility: Visibility::Public,
                span: c.span,
            });
        }

        let info = ClassInfo {
            name: c.name.clone(),
            extends: c.extends.clone(),
            managed: c.managed,
            fields,
            methods,
            span: c.span,
        };
        self.class_log.push(info.clone());
        self.classes.insert(c.name.clone(), info);
    }

    fn analyze_decl(&mut self, decl: &Decl) {
        match decl {
            Decl::Function(f) => {
                self.declare(&f.name, TypeHint::Function, None, f.span);
                self.push_scope();
                for (idx, p) in f.params.iter().enumerate() {
                    // Per-parameter `AS Class` annotation: if present
                    // and the type resolves to a registered class,
                    // attach the class identity to the parameter
                    // binding. Inside the body, sema's
                    // `class_name_of_expr` reads this class_name from
                    // the binding so `p.method()` dispatches against
                    // `Class`'s vtable and `p.field` runs the same
                    // visibility check that a class-typed local does.
                    let class_name = f
                        .param_annotations
                        .get(idx)
                        .and_then(|a| a.as_deref())
                        .and_then(|a| self.class_name_from_annotation(a));
                    self.declare(p, TypeHint::Word, class_name, f.span);
                }
                let body_hint = self.type_of(&f.body);
                let body_class = self.class_of_expr(&f.body);
                self.pop_scope();
                let info = FunctionInfo {
                    name: f.name.clone(),
                    kind: FunctionKind::Function,
                    params: f.params.clone(),
                    result: body_hint,
                    result_class_name: body_class,
                    span: f.span,
                };
                self.functions.insert(f.name.clone(), info.clone());
                self.function_log.push(info);
            }
            Decl::Routine(r) => {
                self.declare(&r.name, TypeHint::Function, None, r.span);
                self.push_scope();
                for (idx, p) in r.params.iter().enumerate() {
                    let class_name = r
                        .param_annotations
                        .get(idx)
                        .and_then(|a| a.as_deref())
                        .and_then(|a| self.class_name_from_annotation(a));
                    self.declare(p, TypeHint::Word, class_name, r.span);
                }
                self.analyze_stmt(&r.body);
                self.pop_scope();
                let info = FunctionInfo {
                    name: r.name.clone(),
                    kind: FunctionKind::Routine,
                    params: r.params.clone(),
                    result: TypeHint::Word,
                    result_class_name: None,
                    span: r.span,
                };
                self.functions.insert(r.name.clone(), info.clone());
                self.function_log.push(info);
            }
            Decl::Let(l) => self.analyze_let(l),
            Decl::Get(_) => {}
            Decl::Manifest(m) => {
                for b in &m.bindings {
                    let hint = b
                        .value
                        .as_ref()
                        .map(|e| self.type_of(e))
                        .unwrap_or(TypeHint::Int);
                    self.declare(&b.name, hint, None, b.span);
                    // Record the literal value so lowering can
                    // substitute it inline. The parser emits `-1`
                    // as `Unary { Neg, IntLit 1 }` rather than a
                    // negative literal token, so we fold the
                    // common `-N` / `+N` shapes back to a single
                    // integer here. Anything more complex (`1+2`)
                    // would need full const-evaluation; not worth
                    // it until the corpus demands it.
                    if let Some(folded) = b.value.as_ref().and_then(fold_int_literal) {
                        self.manifests.insert(b.name.clone(), folded);
                    }
                }
            }
            Decl::Static(s) => {
                for b in &s.bindings {
                    let hint = b
                        .value
                        .as_ref()
                        .map(|e| self.type_of(e))
                        .unwrap_or(TypeHint::Word);
                    self.declare(&b.name, hint, None, b.span);
                }
            }
            Decl::Global(g) => {
                for b in &g.bindings {
                    let hint = b
                        .value
                        .as_ref()
                        .map(|e| self.type_of(e))
                        .unwrap_or(TypeHint::Word);
                    self.declare(&b.name, hint, None, b.span);
                    // Record the initial value when it's a
                    // compile-time integer constant — IR lowering
                    // sets that as the LLVM `@global`'s initializer.
                    // Anything more complex falls back to a
                    // zero-initialised slot plus a CREATE-time write
                    // in lowering (handled by a synthesised entry
                    // routine if/when we add one).
                    let init = match &b.value {
                        Some(Expr::IntLit { value, .. }) => Some(*value),
                        _ => None,
                    };
                    self.globals.insert(b.name.clone(), init);
                }
            }
            Decl::Class(c) => self.analyze_class_body(c),
            Decl::AsmProc(a) => {
                self.declare(&a.name, TypeHint::Function, None, a.span);
                let kind = if a.is_function {
                    FunctionKind::Function
                } else {
                    FunctionKind::Routine
                };
                self.function_log.push(FunctionInfo {
                    name: a.name.clone(),
                    kind,
                    params: a.params.clone(),
                    result: TypeHint::Word,
                    result_class_name: None,
                    span: a.span,
                });
            }
        }
    }

    fn analyze_let(&mut self, l: &LetDecl) {
        for (i, (name, expr)) in l.bindings.iter().enumerate() {
            let mut hint = self.type_of(expr);
            // FLET overrides scalar inference to FLOAT when the literal
            // evidence is otherwise neutral (manifesto §1).
            if matches!(l.kind, LetKind::FLet)
                && matches!(hint, TypeHint::Int | TypeHint::Word | TypeHint::Unknown)
            {
                hint = TypeHint::Float;
            }
            // Manifesto §2 ("looks untyped, secretly typed"):
            // an explicit `AS Type` annotation overrides the
            // inferred hint. The parser stores annotations
            // parallel to bindings in `l.annotations`.
            let mut class_name = self.class_name_of(expr);
            if let Some(Some(ann)) = l.annotations.get(i) {
                if let Some(annotated) = type_hint_from_annotation(ann) {
                    hint = annotated;
                } else if let Some(annotated_class) = self.class_name_from_annotation(ann) {
                    // `LET p AS Foo = ps!i` — the annotation names a
                    // known class. Class identity is metadata: we
                    // record `class_name` so member access
                    // (`p.field`, `p.method()`) can resolve, but we
                    // leave the slot's hint as whatever the
                    // initialiser produced (typically `Word` from a
                    // subscript read on a polymorphic VEC). Flipping
                    // the hint to `Object` would change the slot's
                    // LLVM type from i64 to ptr and break the
                    // round-trip — the value coming out of `ps!i` is
                    // a Word-shaped read. See the
                    // `vec_of_class_pointers_round_trip` probe in
                    // `tests/newbcpl-tests/tests/matrix_tier6.rs`.
                    if class_name.is_none() {
                        class_name = Some(annotated_class);
                    }
                }
            }
            self.declare(name, hint, class_name, l.span);
        }
    }

    /// Convenience wrapper for `class_of_expr` — kept under its
    /// original name because the manifesto §5 aliasing checks
    /// (`is_managed` propagation) read better that way.
    fn class_name_of(&self, expr: &Expr) -> Option<String> {
        self.class_of_expr(expr)
    }

    /// If an `AS Type` annotation strips down to a name that matches
    /// a known class, return that class name. The stripping rules
    /// mirror `type_hint_from_annotation`: leading `^` pointer-to
    /// markers are dropped, an ` OF tail` suffix is trimmed, and the
    /// remainder is matched against `self.classes` verbatim. Used to
    /// recover class identity through reads that sema can't see
    /// through on its own — `LET p AS Foo = ps!i` is the canonical
    /// case.
    fn class_name_from_annotation(&self, annotation: &str) -> Option<String> {
        let mut s = annotation;
        while let Some(rest) = s.strip_prefix('^') {
            s = rest;
        }
        let base = match s.split_once(" OF ") {
            Some((head, _tail)) => head.trim(),
            None => s.trim(),
        };
        if self.classes.contains_key(base) {
            Some(base.to_string())
        } else {
            None
        }
    }

    fn analyze_class_body(&mut self, c: &ClassDecl) {
        // Class bodies introduce method scopes. Members themselves
        // are tracked by `class_log`; per-member variable declarations
        // get their own scope only inside method bodies.
        for m in &c.members {
            self.analyze_class_member(c, m);
        }
    }

    fn analyze_class_member(&mut self, c: &ClassDecl, m: &ClassMember) {
        match &m.kind {
            ClassMemberKind::Fields { .. } | ClassMemberKind::Let(_) | ClassMemberKind::FLet(_) => {
                // Field hints were collected during `register_class`.
                // Nothing to bind in the surrounding scope here.
            }
            ClassMemberKind::Method(method) => {
                self.push_scope();
                // SELF is the receiver — we know its class name. SUPER
                // is the parent's "view" of the same object, so it
                // gets the parent's class name when there is one.
                self.declare("SELF", TypeHint::Object, Some(c.name.clone()), method.span);
                self.declare(
                    "SUPER",
                    TypeHint::Object,
                    c.extends.clone(),
                    method.span,
                );
                for (idx, p) in method.params.iter().enumerate() {
                    let class_name = method
                        .param_annotations
                        .get(idx)
                        .and_then(|a| a.as_deref())
                        .and_then(|a| self.class_name_from_annotation(a));
                    self.declare(p, TypeHint::Word, class_name, method.span);
                }
                // Mark this body as "inside class c" so visibility
                // checks can answer "where is this access from?".
                let saved_class = self.current_class.replace(c.name.clone());
                let (result_hint, result_class) = match &method.body {
                    ClassMethodBody::Routine(s) => {
                        self.analyze_stmt(s);
                        (TypeHint::Word, None)
                    }
                    ClassMethodBody::Function(e) => {
                        let hint = self.type_of(e);
                        let class_name = self.class_of_expr(e);
                        (hint, class_name)
                    }
                };
                self.current_class = saved_class;
                self.pop_scope();
                // Refine the previously-recorded method's result hint
                // and result class identity (the latter lets chained
                // dispatch like `obj.getInner().method()` resolve).
                if let Some(class_info) = self.classes.get_mut(&c.name) {
                    for mi in class_info.methods.iter_mut() {
                        if mi.name == method.name && mi.span == method.span {
                            mi.result = result_hint;
                            mi.result_class_name = result_class.clone();
                            break;
                        }
                    }
                }
                // Mirror the refinement into `class_log` so dump-sema
                // surfaces the inferred result.
                if let Some(class_log_entry) =
                    self.class_log.iter_mut().find(|ci| ci.name == c.name)
                {
                    for mi in class_log_entry.methods.iter_mut() {
                        if mi.name == method.name && mi.span == method.span {
                            mi.result = result_hint;
                            mi.result_class_name = result_class.clone();
                            break;
                        }
                    }
                }
            }
        }
    }

    /// Look up a field by walking the class's inheritance chain. Returns
    /// the field's hint if found, `None` otherwise.
    fn lookup_field(&self, class_name: &str, field: &str) -> Option<TypeHint> {
        self.lookup_field_owner(class_name, field).map(|(_, info)| info.hint)
    }

    /// As `lookup_field`, but also reports which class in the
    /// inheritance chain actually declared the field. Visibility
    /// checks key off the defining class — `PRIVATE`/`PROTECTED` are
    /// relative to it, not to the receiver's runtime class.
    fn lookup_field_owner(
        &self,
        class_name: &str,
        field: &str,
    ) -> Option<(String, ClassFieldInfo)> {
        let mut current = Some(class_name.to_string());
        while let Some(name) = current {
            if let Some(class) = self.classes.get(&name) {
                if let Some(f) = class.fields.iter().find(|f| f.name == field) {
                    return Some((name, f.clone()));
                }
                current = class.extends.clone();
            } else {
                return None;
            }
        }
        None
    }

    /// As `lookup_method`, but reports which class actually declared
    /// the method body. Visibility checks consult this defining
    /// class, not the receiver's static class — a `PRIVATE` method
    /// in `Base` stays private even when called through a `Sub`
    /// instance.
    fn lookup_method_owner(
        &self,
        class_name: &str,
        method: &str,
    ) -> Option<(String, ClassMethodInfo)> {
        let mut current = Some(class_name.to_string());
        while let Some(name) = current {
            if let Some(class) = self.classes.get(&name) {
                if let Some(m) = class.methods.iter().find(|m| m.name == method) {
                    return Some((name, m.clone()));
                }
                current = class.extends.clone();
            } else {
                return None;
            }
        }
        None
    }

    /// Look up a method by walking the inheritance chain. Returns the
    /// method's full info (so the caller can use both kind and result).
    fn lookup_method(&self, class_name: &str, method: &str) -> Option<ClassMethodInfo> {
        let mut current = Some(class_name.to_string());
        while let Some(name) = current {
            if let Some(class) = self.classes.get(&name) {
                if let Some(m) = class.methods.iter().find(|m| m.name == method) {
                    return Some(m.clone());
                }
                current = class.extends.clone();
            } else {
                return None;
            }
        }
        None
    }

    /// Look up a field's *class identity* on a class (walks the
    /// inheritance chain). Returns the class name the field's value
    /// belongs to, when sema has proof — populated from
    /// `LET f = NEW Foo()`, `AS Class` annotations on class members, or
    /// SELF-assignment back-fills. Returns `None` for fields of
    /// non-class type (Word, Int, Float, …) and for class-typed fields
    /// where sema couldn't determine the class.
    fn lookup_field_class(&self, class_name: &str, field: &str) -> Option<String> {
        let mut current = Some(class_name.to_string());
        while let Some(name) = current {
            if let Some(class) = self.classes.get(&name) {
                if let Some(f) = class.fields.iter().find(|f| f.name == field) {
                    return f.class_name.clone();
                }
                current = class.extends.clone();
            } else {
                return None;
            }
        }
        None
    }

    /// Best-effort: return the class name an expression evaluates to,
    /// when sema knows. Drives `obj.field` / `obj.method()` resolution
    /// and chained dispatch (`a.b.c.d()`). Recurses through:
    ///
    /// - identifier lookup (binding class)
    /// - `NEW Class(...)`
    /// - `obj.field` — looks up `field`'s class_name on `class_of(obj)`
    /// - `obj.method(args)` — looks up `method`'s result_class_name
    /// - direct call `f(args)` — uses the function's result_class_name
    fn class_of_expr(&self, e: &Expr) -> Option<String> {
        match e {
            Expr::Ident { name, .. } => self.lookup(name).and_then(|info| info.class_name.clone()),
            Expr::New { class_name, .. } => Some(class_name.clone()),
            Expr::Binary {
                op: BinaryOp::Dot | BinaryOp::Of,
                lhs,
                rhs,
                ..
            } => {
                let receiver_class = self.class_of_expr(lhs)?;
                if let Expr::Ident { name: field, .. } = rhs.as_ref() {
                    return self.lookup_field_class(&receiver_class, field);
                }
                None
            }
            Expr::Call { callee, .. } => match callee.as_ref() {
                Expr::Binary {
                    op: BinaryOp::Dot,
                    lhs,
                    rhs,
                    ..
                } => {
                    let receiver_class = self.class_of_expr(lhs)?;
                    if let Expr::Ident { name: method, .. } = rhs.as_ref() {
                        return self
                            .lookup_method(&receiver_class, method)
                            .and_then(|m| m.result_class_name);
                    }
                    None
                }
                Expr::Ident { name, .. } => self
                    .functions
                    .get(name)
                    .and_then(|f| f.result_class_name.clone()),
                _ => None,
            },
            _ => None,
        }
    }

    fn analyze_stmt(&mut self, stmt: &Stmt) {
        match stmt {
            Stmt::Block(b) => self.analyze_block(b),
            Stmt::Decl(d) => self.analyze_decl(d),
            Stmt::Expr(e) => {
                let _ = self.type_of(e);
            }
            Stmt::Assign { targets, values, .. } => self.analyze_assign(targets, values),
            Stmt::If {
                cond,
                then_stmt,
                else_stmt,
                ..
            } => {
                let _ = self.type_of(cond);
                self.analyze_stmt(then_stmt);
                if let Some(els) = else_stmt {
                    self.analyze_stmt(els);
                }
            }
            Stmt::Unless {
                cond, then_stmt, ..
            } => {
                let _ = self.type_of(cond);
                self.analyze_stmt(then_stmt);
            }
            Stmt::While { cond, body, .. } | Stmt::Until { cond, body, .. } => {
                let _ = self.type_of(cond);
                self.loop_depth += 1;
                self.analyze_stmt(body);
                self.loop_depth -= 1;
            }
            Stmt::Repeat { body, .. } => {
                self.loop_depth += 1;
                self.analyze_stmt(body);
                self.loop_depth -= 1;
            }
            Stmt::RepeatWhile { body, cond, .. } | Stmt::RepeatUntil { body, cond, .. } => {
                self.loop_depth += 1;
                self.analyze_stmt(body);
                self.loop_depth -= 1;
                let _ = self.type_of(cond);
            }
            Stmt::For {
                name,
                start,
                end,
                step,
                body,
                span,
            } => {
                let start_hint = self.type_of(start);
                let _ = self.type_of(end);
                if let Some(s) = step {
                    let _ = self.type_of(s);
                }
                self.push_scope();
                // Loop variable matches the start expression's family;
                // step / end may be float, but FOR is typically integer.
                let lv_hint = if start_hint == TypeHint::Float {
                    TypeHint::Float
                } else {
                    TypeHint::Int
                };
                self.declare(name, lv_hint, None, *span);
                self.loop_depth += 1;
                self.analyze_stmt(body);
                self.loop_depth -= 1;
                self.pop_scope();
            }
            Stmt::ForEach {
                names,
                annotation,
                iter,
                body,
                span,
            } => {
                let iter_hint = self.type_of(iter);
                self.push_scope();
                // Element hint heuristic: if there's an explicit
                // annotation, honour it; otherwise default to WORD
                // (lists are heterogeneous so we can't be specific).
                let element_hint = annotation
                    .as_deref()
                    .and_then(map_annotation_to_hint)
                    .unwrap_or(match iter_hint {
                        TypeHint::FVec => TypeHint::Float,
                        _ => TypeHint::Word,
                    });
                for n in names {
                    self.declare(n, element_hint, None, *span);
                }
                self.loop_depth += 1;
                self.analyze_stmt(body);
                self.loop_depth -= 1;
                self.pop_scope();
            }
            Stmt::Switchon {
                scrutinee,
                cases,
                default,
                ..
            } => {
                let _ = self.type_of(scrutinee);
                self.switchon_depth += 1;
                for case in cases {
                    for v in &case.values {
                        let _ = self.type_of(v);
                    }
                    for s in &case.body {
                        self.analyze_stmt(s);
                    }
                }
                if let Some(body) = default {
                    for s in body {
                        self.analyze_stmt(s);
                    }
                }
                self.switchon_depth -= 1;
            }
            Stmt::Resultis(e, span) => {
                let hint = self.type_of(e);
                if let Some(frame) = self.valof_results.last_mut() {
                    frame.push(hint);
                } else {
                    self.warn(
                        "RESULTIS outside any VALOF block — has no effect",
                        *span,
                    );
                }
            }
            Stmt::Retain { value: Some(v), name, span, .. } => {
                let hint = self.type_of(v);
                let class_name = self.class_name_of(v);
                self.declare(name, hint, class_name, *span);
            }
            Stmt::Using {
                name,
                value,
                body,
                span,
            } => {
                let hint = self.type_of(value);
                let class_name = self.class_name_of(value);
                // The USING binding is visible only inside `body`,
                // mirroring how `LET x = ...` inside a block scopes
                // to that block. We push a scope so the name doesn't
                // leak.
                self.push_scope();
                self.declare(name, hint, class_name, *span);
                self.analyze_stmt(body);
                self.pop_scope();
            }
            Stmt::Break(span) => {
                if self.loop_depth == 0 {
                    self.warn("BREAK outside any loop body — has no effect", *span);
                }
            }
            Stmt::Loop(span) => {
                if self.loop_depth == 0 {
                    self.warn(
                        "LOOP outside any loop body — has no effect",
                        *span,
                    );
                }
            }
            Stmt::Endcase(span) => {
                if self.switchon_depth == 0 {
                    self.warn(
                        "ENDCASE outside any SWITCHON — has no effect",
                        *span,
                    );
                }
            }
            Stmt::Return(_)
            | Stmt::Finish(_)
            | Stmt::Brk(_)
            | Stmt::Goto { .. }
            | Stmt::Label { .. }
            | Stmt::Retain { value: None, .. } => {}
        }
    }

    fn analyze_block(&mut self, b: &Block) {
        // Each block starts a new scope per BCPL convention: a `LET`
        // inside a `$( … $)` is visible only until the closing bracket.
        self.push_scope();
        for s in &b.stmts {
            self.analyze_stmt(s);
        }
        self.pop_scope();
    }

    fn analyze_assign(&mut self, targets: &[Expr], values: &[Expr]) {
        for (t, v) in targets.iter().zip(values.iter()) {
            let target_hint = self.type_of(t);
            let value_hint = self.type_of(v);
            self.check_coercion(target_hint, value_hint, v.span());
            // Back-fill the receiver class's field-class identity for
            // assignments of the shape `SELF.field := <class-typed-expr>`.
            // Field types are declared as bag-of-bits Word; their class
            // identity is discoverable only from the values assigned to
            // them, and CREATE-time SELF assignments are the canonical
            // place to look. This unlocks chained access like
            // `outer.inner.method()` once codegen consumes
            // ClassFieldInfo::class_name through layouts.
            self.back_fill_field_class_from_self_assign(t, v);
        }
    }

    /// True when an assignment `obj.field := value` would store a
    /// MANAGED instance into a non-MANAGED holder's field. Both the
    /// If `target` is `SELF.field` and `value` has a known class
    /// identity, write that class onto the field's `ClassFieldInfo`
    /// (and the parallel `class_log` entry sema uses for dump-sema).
    /// First write wins — explicit `LET f = NEW Foo()` initialisers
    /// already populated their slot during `register_class` and we
    /// don't overwrite. The SELF binding carries the receiver class
    /// in its `BindingInfo`, so this works without sema tracking
    /// `current_class` separately.
    fn back_fill_field_class_from_self_assign(&mut self, target: &Expr, value: &Expr) {
        let Expr::Binary {
            op: BinaryOp::Dot,
            lhs,
            rhs,
            ..
        } = target
        else {
            return;
        };
        let Expr::Ident { name: receiver_name, .. } = lhs.as_ref() else {
            return;
        };
        if receiver_name != "SELF" {
            return;
        }
        let Expr::Ident { name: field, .. } = rhs.as_ref() else {
            return;
        };
        let Some(class_name) = self.lookup(receiver_name).and_then(|b| b.class_name.clone())
        else {
            return;
        };
        let Some(value_class) = self.class_of_expr(value) else {
            return;
        };
        if let Some(class) = self.classes.get_mut(&class_name) {
            if let Some(f) = class.fields.iter_mut().find(|f| f.name == *field) {
                if f.class_name.is_none() {
                    f.class_name = Some(value_class.clone());
                }
            }
        }
        if let Some(class_log_entry) = self.class_log.iter_mut().find(|ci| ci.name == class_name) {
            if let Some(f) = class_log_entry
                .fields
                .iter_mut()
                .find(|f| f.name == *field)
            {
                if f.class_name.is_none() {
                    f.class_name = Some(value_class);
                }
            }
        }
    }

    /// Compare an assignment's target hint against the value's hint
    /// and emit a warning if the compiler would silently insert a
    /// real machine instruction the user did not write. INT↔WORD
    /// is free; FLOAT← INT is silent (SCVTF); INT← FLOAT warns
    /// (FCVTZS truncation); cross-family non-pointer mismatches warn.
    fn check_coercion(&mut self, target: TypeHint, value: TypeHint, span: Span) {
        if target == value {
            return;
        }
        // Free coercions:
        if matches!(target, TypeHint::Word) || matches!(value, TypeHint::Word) {
            return;
        }
        // A NULL-typed binding (initialised with `?`) is a typeless
        // pointer slot until something is assigned. Assigning any
        // pointer-shaped value to it is a feature, not a coercion
        // we should warn about — sema absorbs the new shape silently.
        if matches!(target, TypeHint::Null) {
            return;
        }
        if matches!(value, TypeHint::Null)
            && matches!(
                target,
                TypeHint::String
                    | TypeHint::List
                    | TypeHint::Vec
                    | TypeHint::FVec
                    | TypeHint::Object
            )
        {
            return;
        }
        if matches!(value, TypeHint::Unknown) || matches!(target, TypeHint::Unknown) {
            return;
        }
        // FLOAT ← INT is silent (SCVTF emitted, no precision lost).
        if target == TypeHint::Float && value == TypeHint::Int {
            return;
        }
        // INT ← FLOAT is the headline truncation case.
        if target == TypeHint::Int && value == TypeHint::Float {
            self.warn(
                "implicit FLOAT → INT conversion (FCVTZS truncates toward zero)",
                span,
            );
            return;
        }
        // Other cross-family assignments — warn but don't reject.
        self.warn(
            format!(
                "assignment loses type information ({} := {})",
                target.as_str(),
                value.as_str()
            ),
            span,
        );
    }

    /// The heart of inference: compute a TypeHint for any expression
    /// and store it on the expression's `hint` Cell so downstream
    /// phases (IR, codegen, dump-ast) can read it without re-running
    /// sema. Always returns *something* — even `Unknown` is a known
    /// value.
    fn type_of(&mut self, expr: &Expr) -> TypeHint {
        let hint = self.compute_type_of(expr);
        expr.set_hint(hint);
        hint
    }

    fn compute_type_of(&mut self, expr: &Expr) -> TypeHint {
        match expr {
            Expr::IntLit { .. } => TypeHint::Int,
            Expr::FloatLit { .. } => TypeHint::Float,
            Expr::StringLit { .. } => TypeHint::String,
            Expr::CharLit { .. } => TypeHint::Int,
            Expr::BoolLit { .. } => TypeHint::Int,
            Expr::Null { .. } => TypeHint::Null,
            Expr::Ident { name, .. } => match self.lookup(name) {
                Some(info) => info.hint,
                None => TypeHint::Unknown,
            },
            Expr::Call { callee, args, .. } => {
                for a in args {
                    let _ = self.type_of(a);
                }
                let _ = self.type_of(callee);
                if let Expr::Ident { name, .. } = callee.as_ref() {
                    // Builtin return-type hints. Without these,
                    // a call to (say) `CONCAT(a, b)` infers as
                    // Word and a subsequent `LEN(c)` routes to
                    // the vec-length helper instead of the
                    // list-length helper — see the
                    // `list_concat_combines_two_chains` probe
                    // in `tests/newbcpl-tests/tests/matrix_tier6.rs`.
                    match name.as_str() {
                        // Float-returning math.
                        "FLOAT" | "FSQRT" | "FSIN" | "FCOS" | "FTAN"
                        | "FABS" | "FLOG" | "FEXP" | "FRND" | "RND" => {
                            return TypeHint::Float;
                        }
                        // Int-returning queries.
                        "TRUNC" | "FIX" | "ENTIER" | "LEN" | "RAND"
                        | "__newbcpl_len" | "__newbcpl_list_len" | "RDCH" => {
                            return TypeHint::Int;
                        }
                        // String-returning queries.
                        "TYPE" | "TYPEOF" => return TypeHint::String,
                        // List-returning builtins. `HD` is omitted
                        // — it returns a single atom's value
                        // (typically Word), not a list.
                        "CONCAT" | "TL" | "TAIL" | "REST"
                        | "__newbcpl_list_tl" | "__newbcpl_list_rest"
                        | "__newbcpl_list_new_empty" => {
                            return TypeHint::List;
                        }
                        // Vec / pair-array allocators return a
                        // word-shaped pointer the caller treats
                        // as a vector. Hinting Vec routes
                        // `LEN` / `FOREACH` through the right
                        // path (index-walk, `*(p-8)` length).
                        "GETVEC" | "FGETVEC" | "PAIRS" | "FPAIRS"
                        | "__newbcpl_alloc_rec" => {
                            return TypeHint::Vec;
                        }
                        _ => {}
                    }
                    if let Some(info) = self.functions.get(name) {
                        return match info.kind {
                            FunctionKind::Routine => TypeHint::Word,
                            FunctionKind::Function => info.result,
                        };
                    }
                }
                // Method call: callee is `obj.methodName`. Resolve via
                // the class table and take the method's inferred
                // result hint.
                if let Expr::Binary {
                    op: BinaryOp::Dot,
                    lhs,
                    rhs,
                    span: dot_span,
                    ..
                } = callee.as_ref()
                {
                    if let (Some(class_name), Expr::Ident { name: method, .. }) =
                        (self.class_of_expr(lhs), rhs.as_ref())
                    {
                        if let Some((defining_class, method_info)) =
                            self.lookup_method_owner(&class_name, method)
                        {
                            self.check_member_visibility(
                                &defining_class,
                                method,
                                method_info.visibility,
                                *dot_span,
                            );
                            return match method_info.kind {
                                FunctionKind::Routine => TypeHint::Word,
                                FunctionKind::Function => method_info.result,
                            };
                        }
                    }
                }
                TypeHint::Word
            }
            Expr::Unary { op, operand, .. } => {
                let operand_hint = self.type_of(operand);
                match op {
                    UnaryOp::Neg => operand_hint,
                    UnaryOp::Not => TypeHint::Int,
                    UnaryOp::LogNot => TypeHint::Int,
                    UnaryOp::Indirection => TypeHint::Word,
                    UnaryOp::AddressOf => TypeHint::Word,
                    UnaryOp::CharIndirection => TypeHint::Int,
                    UnaryOp::Hd | UnaryOp::Tl | UnaryOp::Rest => match operand_hint {
                        TypeHint::List => TypeHint::Word, // element type unknown
                        TypeHint::Vec => TypeHint::Word,
                        TypeHint::FVec => TypeHint::Float,
                        _ => TypeHint::Word,
                    },
                    UnaryOp::Len => TypeHint::Int,
                    UnaryOp::FreeVec | UnaryOp::FreeList => TypeHint::Word,
                }
            }
            Expr::Binary { op, lhs, rhs, span: dot_span, .. } => {
                // Member access: resolve `obj.field` through the class
                // table when sema knows obj's class. The RHS is
                // syntactically an identifier (the parser enforces it),
                // so type_of(rhs) would just look it up as a top-level
                // name — skip that and route through `lookup_field`.
                if matches!(op, BinaryOp::Dot | BinaryOp::Of) {
                    let _ = self.type_of(lhs);
                    if let (Some(class_name), Expr::Ident { name: field, .. }) =
                        (self.class_of_expr(lhs), rhs.as_ref())
                    {
                        if let Some((defining_class, field_info)) =
                            self.lookup_field_owner(&class_name, field)
                        {
                            self.check_member_visibility(
                                &defining_class,
                                field,
                                field_info.visibility,
                                *dot_span,
                            );
                            return field_info.hint;
                        }
                        // Fall back to method lookup so `obj.method`
                        // (without a call) still has a sensible hint.
                        // No visibility check here — `obj.method` as a
                        // value is a function-pointer reference, which
                        // we don't model fully; the call site catches
                        // the violation when it fires.
                        if let Some(_) = self.lookup_method(&class_name, field) {
                            return TypeHint::Function;
                        }
                    }
                    return TypeHint::Word;
                }
                let lhs_hint = self.type_of(lhs);
                let rhs_hint = self.type_of(rhs);
                self.binary_result(*op, lhs_hint, rhs_hint, lhs, rhs)
            }
            Expr::Conditional {
                cond,
                then_expr,
                else_expr,
                ..
            } => {
                let _ = self.type_of(cond);
                let then_hint = self.type_of(then_expr);
                let else_hint = self.type_of(else_expr);
                if then_hint == else_hint {
                    then_hint
                } else if then_hint == TypeHint::Unknown {
                    else_hint
                } else if else_hint == TypeHint::Unknown {
                    then_hint
                } else {
                    TypeHint::Word
                }
            }
            Expr::Valof { body, .. } => {
                self.valof_results.push(Vec::new());
                self.analyze_stmt(body);
                let collected = self
                    .valof_results
                    .pop()
                    .expect("valof frame must exist");
                merge_hints(&collected)
            }
            Expr::TypedConstruct { kind, args, .. } => {
                for a in args {
                    let _ = self.type_of(a);
                }
                match kind {
                    TypeConstructorKind::Vec => TypeHint::Vec,
                    TypeConstructorKind::FVec => TypeHint::FVec,
                    TypeConstructorKind::Table => TypeHint::Vec,
                    TypeConstructorKind::FTable => TypeHint::FVec,
                    TypeConstructorKind::Pair => TypeHint::Pair,
                    TypeConstructorKind::FPair => TypeHint::FPair,
                    TypeConstructorKind::Quad => TypeHint::Quad,
                    TypeConstructorKind::FQuad => TypeHint::FQuad,
                    TypeConstructorKind::Oct => TypeHint::Oct,
                    TypeConstructorKind::FOct => TypeHint::FOct,
                    TypeConstructorKind::List | TypeConstructorKind::ManifestList => {
                        TypeHint::List
                    }
                }
            }
            Expr::New { class_name: _, args, .. } => {
                for a in args {
                    let _ = self.type_of(a);
                }
                TypeHint::Object
            }
        }
    }

    /// Result type of a binary operator. The headline rule per
    /// manifesto §1: `+`, `-`, `*`, `/` produce FLOAT iff both sides
    /// are FLOAT, otherwise INT. The dotted variants (`+.`, `-.`,
    /// etc.) are *assertions* — they warn if either side is not
    /// FLOAT, then still produce FLOAT.
    fn binary_result(
        &mut self,
        op: BinaryOp,
        lhs: TypeHint,
        rhs: TypeHint,
        _lhs_expr: &Expr,
        rhs_expr: &Expr,
    ) -> TypeHint {
        use BinaryOp::*;
        match op {
            Add | Sub | Mul | Div | Rem => {
                if lhs == TypeHint::Float && rhs == TypeHint::Float {
                    TypeHint::Float
                } else if lhs.is_float_family() || rhs.is_float_family() {
                    // SIMD types: result is the wider float family
                    // when both sides match; otherwise fall back to
                    // INT and let codegen / sema-warning handle it.
                    if lhs == rhs {
                        lhs
                    } else {
                        TypeHint::Int
                    }
                } else {
                    TypeHint::Int
                }
            }
            FAdd | FSub | FMul | FDiv => {
                if !lhs.is_float_family() && lhs != TypeHint::Word && lhs != TypeHint::Unknown {
                    self.warn(
                        format!(
                            "dotted float operator on non-FLOAT lhs ({})",
                            lhs.as_str()
                        ),
                        rhs_expr.span(),
                    );
                }
                if !rhs.is_float_family() && rhs != TypeHint::Word && rhs != TypeHint::Unknown {
                    self.warn(
                        format!(
                            "dotted float operator on non-FLOAT rhs ({})",
                            rhs.as_str()
                        ),
                        rhs_expr.span(),
                    );
                }
                TypeHint::Float
            }
            Eq | Ne | Lt | Le | Gt | Ge | FEq | FNe | FLt | FLe | FGt | FGe => TypeHint::Int,
            BitAnd | BitOr | BitXor | Eqv | Neqv | Shl | Shr => TypeHint::Int,
            LogAnd | LogOr | LogXor => TypeHint::Int,
            Subscript => TypeHint::Word,
            CharSubscript => TypeHint::Int,
            FloatSubscript => TypeHint::Float,
            Bitfield => TypeHint::Int,
            Dot | Of => TypeHint::Word,
            LaneAccess => match lhs {
                TypeHint::FPair | TypeHint::FQuad | TypeHint::FOct => TypeHint::Float,
                _ => TypeHint::Int,
            },
        }
    }
}

/// Cheap, scope-free hint inference for a literal-shaped expression —
/// used by `register_class` to populate field hints during the
/// pre-pass, before bindings are known. Only literals and bare type-
/// constructors get a precise hint; anything else falls back to
/// `Word` and is refined later if class members ever participate in
/// flow inference.
/// Constant-fold the common compile-time integer shapes used in
/// `MANIFEST $( name = expr $)` initialisers: bare `IntLit`, an
/// unary `-N` (which the parser emits as `Unary { Neg, IntLit N }`,
/// not as a negative literal), `+N`, and bool literals (which BCPL
/// treats as 0 / 1). Anything more complex returns `None`; the
/// MANIFEST then falls through to its runtime-binding path and
/// produces a `missing builtin: <name>` diagnostic at JIT time —
/// the right outcome when sema can't see through the initialiser.
fn fold_int_literal(e: &Expr) -> Option<i64> {
    match e {
        Expr::IntLit { value, .. } => Some(*value),
        Expr::BoolLit { value, .. } => Some(if *value { 1 } else { 0 }),
        Expr::Unary {
            op: UnaryOp::Neg,
            operand,
            ..
        } => fold_int_literal(operand).map(|v| -v),
        _ => None,
    }
}

fn literal_hint(e: &Expr) -> TypeHint {
    match e {
        Expr::IntLit { .. } => TypeHint::Int,
        Expr::FloatLit { .. } => TypeHint::Float,
        Expr::StringLit { .. } => TypeHint::String,
        Expr::CharLit { .. } => TypeHint::Int,
        Expr::BoolLit { .. } => TypeHint::Int,
        Expr::Null { .. } => TypeHint::Null,
        Expr::TypedConstruct { kind, .. } => match kind {
            TypeConstructorKind::Vec => TypeHint::Vec,
            TypeConstructorKind::FVec => TypeHint::FVec,
            TypeConstructorKind::Table => TypeHint::Vec,
            TypeConstructorKind::FTable => TypeHint::FVec,
            TypeConstructorKind::Pair => TypeHint::Pair,
            TypeConstructorKind::FPair => TypeHint::FPair,
            TypeConstructorKind::Quad => TypeHint::Quad,
            TypeConstructorKind::FQuad => TypeHint::FQuad,
            TypeConstructorKind::Oct => TypeHint::Oct,
            TypeConstructorKind::FOct => TypeHint::FOct,
            TypeConstructorKind::List | TypeConstructorKind::ManifestList => TypeHint::List,
        },
        Expr::New { .. } => TypeHint::Object,
        _ => TypeHint::Word,
    }
}

/// Merge the hints contributed by a set of `RESULTIS` expressions in
/// the same VALOF block. If they all agree the result is precise; if
/// they disagree we widen to `Word` rather than picking one
/// arbitrarily — matches manifesto §1's "fall back to WORD when
/// inference can't decide."
fn merge_hints(hints: &[TypeHint]) -> TypeHint {
    let mut iter = hints.iter().copied().filter(|h| *h != TypeHint::Unknown);
    let Some(first) = iter.next() else {
        return TypeHint::Word;
    };
    if iter.all(|h| h == first) {
        first
    } else {
        TypeHint::Word
    }
}

fn map_annotation_to_hint(annotation: &str) -> Option<TypeHint> {
    Some(match annotation {
        "INT" | "INTEGER" => TypeHint::Int,
        "FLOAT" | "REAL" => TypeHint::Float,
        "STRING" => TypeHint::String,
        "WORD" => TypeHint::Word,
        "PAIR" => TypeHint::Pair,
        "FPAIR" => TypeHint::FPair,
        "QUAD" => TypeHint::Quad,
        "FQUAD" => TypeHint::FQuad,
        "OCT" => TypeHint::Oct,
        "FOCT" => TypeHint::FOct,
        "LIST" => TypeHint::List,
        "VEC" => TypeHint::Vec,
        "FVEC" => TypeHint::FVec,
        _ => return None,
    })
}

/// Resolve a canonicalised `AS Type` annotation string from the
/// parser into a `TypeHint`. Handles the richer shapes the corpus
/// uses:
///
///   - `INTEGER`, `FLOAT`, ...           → matched by the base.
///   - `^STRING`, `^LIST OF INTEGER` ... → each leading `^` is a
///     POINTER-TO marker. We strip them all and map the base.
///     This is correct for our heap-shape model where lists,
///     vectors, strings, and objects are already pointer-shaped
///     at the value level — extra pointer levels collapse to
///     the base type's hint.
///   - `LIST OF INTEGER`                 → element-type info
///     after ` OF ` is recorded by the parser but ignored here;
///     when richer typing lands sema will descend into it.
///
/// Returns `None` when the base isn't a type sema recognises,
/// in which case the inferred hint stays as-is.
pub(crate) fn type_hint_from_annotation(annotation: &str) -> Option<TypeHint> {
    // Strip leading pointer-to markers.
    let mut s = annotation;
    while let Some(rest) = s.strip_prefix('^') {
        s = rest;
    }
    // Peel off the `OF tail` if present — only the base interests
    // us today.
    let base = match s.split_once(" OF ") {
        Some((head, _tail)) => head.trim(),
        None => s.trim(),
    };
    map_annotation_to_hint(base)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn analyze_str(source: &str) -> SemaOutput {
        let program = newbcpl_parser::parse_source(source)
            .unwrap_or_else(|e| panic!("parse failed: {}", e.render()));
        analyze(&program)
    }

    fn binding_hint(out: &SemaOutput, name: &str) -> TypeHint {
        out.bindings
            .iter()
            .rev()
            .find(|b| b.name == name)
            .unwrap_or_else(|| panic!("no binding named {name}"))
            .hint
    }

    #[test]
    fn literal_int() {
        let out = analyze_str("LET x = 42");
        assert_eq!(binding_hint(&out, "x"), TypeHint::Int);
    }

    #[test]
    fn literal_float() {
        let out = analyze_str("LET pi = 3.14159");
        assert_eq!(binding_hint(&out, "pi"), TypeHint::Float);
    }

    #[test]
    fn literal_string() {
        let out = analyze_str("LET s = \"hi*N\"");
        assert_eq!(binding_hint(&out, "s"), TypeHint::String);
    }

    #[test]
    fn flet_overrides_int_to_float() {
        let out = analyze_str("FLET x = 0");
        assert_eq!(binding_hint(&out, "x"), TypeHint::Float);
    }

    #[test]
    fn typed_constructors() {
        let out = analyze_str(
            "LET v = VEC 100\nLET p = PAIR(1, 2)\nLET fp = FPAIR(1.0, 2.0)\nLET xs = LIST(1, 2, 3)",
        );
        assert_eq!(binding_hint(&out, "v"), TypeHint::Vec);
        assert_eq!(binding_hint(&out, "p"), TypeHint::Pair);
        assert_eq!(binding_hint(&out, "fp"), TypeHint::FPair);
        assert_eq!(binding_hint(&out, "xs"), TypeHint::List);
    }

    #[test]
    fn new_object() {
        let out = analyze_str("CLASS Point $( DECL x, y $)\nLET p = NEW Point");
        assert_eq!(binding_hint(&out, "p"), TypeHint::Object);
        let info = out.bindings.iter().find(|b| b.name == "p").unwrap();
        assert_eq!(info.class_name.as_deref(), Some("Point"));
    }

    #[test]
    fn arith_int_float_dispatch() {
        // `a + b` where both are INT → result is INT.
        // `a + b` where both are FLOAT → result is FLOAT.
        let out = analyze_str("LET a = 1\nLET b = 2\nLET c = a + b");
        assert_eq!(binding_hint(&out, "c"), TypeHint::Int);

        let out = analyze_str("LET a = 1.0\nLET b = 2.0\nLET c = a + b");
        assert_eq!(binding_hint(&out, "c"), TypeHint::Float);
    }

    #[test]
    fn arith_mixed_int_word_falls_back_to_int() {
        // `vec!i + 1` — vec subscript is WORD, 1 is INT → INT result.
        let out = analyze_str("LET v = VEC 10\nLET x = v!0 + 1");
        assert_eq!(binding_hint(&out, "x"), TypeHint::Int);
    }

    #[test]
    fn dotted_float_op_on_int_warns() {
        let out = analyze_str("LET a = 1\nLET b = 2\nLET c = a +. b");
        assert!(
            !out.warnings.is_empty(),
            "expected at least one float-on-int warning"
        );
        assert!(
            out.warnings
                .iter()
                .any(|w| w.message.contains("dotted float operator")),
            "warning message should mention dotted float operator: {:?}",
            out.warnings
        );
    }

    #[test]
    fn relational_returns_int() {
        let out = analyze_str("LET a = 1\nLET b = 2\nLET c = a < b");
        assert_eq!(binding_hint(&out, "c"), TypeHint::Int);
    }

    #[test]
    fn vec_subscript_is_word() {
        let out = analyze_str("LET v = VEC 10\nLET x = v!0");
        assert_eq!(binding_hint(&out, "x"), TypeHint::Word);
    }

    #[test]
    fn float_subscript_is_float() {
        let out = analyze_str("LET fv = FVEC 10\nLET x = fv.%0");
        assert_eq!(binding_hint(&out, "x"), TypeHint::Float);
    }

    #[test]
    fn pair_lane_access_is_int() {
        let out = analyze_str("LET p = PAIR(1, 2)\nLET x = p.|0|");
        assert_eq!(binding_hint(&out, "x"), TypeHint::Int);
    }

    #[test]
    fn fpair_lane_access_is_float() {
        let out = analyze_str("LET p = FPAIR(1.0, 2.0)\nLET x = p.|0|");
        assert_eq!(binding_hint(&out, "x"), TypeHint::Float);
    }

    #[test]
    fn assigning_float_to_int_warns() {
        let out = analyze_str("LET S() BE { LET i = 0\n LET f = 3.14\n i := f }");
        assert!(
            out.warnings
                .iter()
                .any(|w| w.message.contains("FLOAT → INT")),
            "expected truncation warning, got: {:?}",
            out.warnings
        );
    }

    #[test]
    fn assigning_int_to_float_is_silent() {
        let out = analyze_str("LET S() BE { LET f = 1.0\n LET i = 0\n f := i }");
        assert!(
            out.warnings.is_empty(),
            "INT → FLOAT should be silent (SCVTF): {:?}",
            out.warnings
        );
    }

    #[test]
    fn null_assignable_to_pointer_targets() {
        let out = analyze_str("LET S() BE { LET s = \"hi\"\n s := ? }");
        assert!(out.warnings.is_empty(), "Null → STRING should be silent: {:?}", out.warnings);
    }

    #[test]
    fn null_target_accepts_any_pointer_shape_silently() {
        // The reverse: a binding starts as `?` (NULL) and is later
        // assigned a real pointer. NULL is the empty pointer slot —
        // assigning into it is a feature, not a coercion.
        let out = analyze_str(
            "CLASS T $( DECL x $)\nLET S() BE { LET ptr = ?\n ptr := NEW T }",
        );
        assert!(
            out.warnings.is_empty(),
            "NULL := OBJECT should be silent: {:?}",
            out.warnings
        );
    }

    #[test]
    fn classes_recorded() {
        let out = analyze_str(
            "CLASS Animal $( DECL name $)\nCLASS Dog EXTENDS Animal $( DECL breed $)\nCLASS Window MANAGED $( DECL handle $)",
        );
        assert_eq!(out.classes.len(), 3);
        let dog = out.classes.iter().find(|c| c.name == "Dog").unwrap();
        assert_eq!(dog.extends.as_deref(), Some("Animal"));
        let window = out.classes.iter().find(|c| c.name == "Window").unwrap();
        assert!(window.managed);
    }

    #[test]
    fn for_loop_introduces_scoped_binding() {
        let out = analyze_str("LET S() BE { FOR i = 1 TO 10 DO f(i) }");
        assert_eq!(binding_hint(&out, "i"), TypeHint::Int);
    }

    #[test]
    fn foreach_with_annotation() {
        let out = analyze_str("LET S() BE { FOREACH C AS INTEGER IN s DO f(C) }");
        assert_eq!(binding_hint(&out, "C"), TypeHint::Int);
    }

    #[test]
    fn intrinsic_calls_have_known_results() {
        let out = analyze_str(
            "LET a = FLOAT(42)\nLET b = TRUNC(3.14)\nLET c = LEN xs\nLET t = TYPE(x)",
        );
        assert_eq!(binding_hint(&out, "a"), TypeHint::Float);
        assert_eq!(binding_hint(&out, "b"), TypeHint::Int);
        assert_eq!(binding_hint(&out, "c"), TypeHint::Int);
        assert_eq!(binding_hint(&out, "t"), TypeHint::String);
    }

    // ─── function signatures & VALOF threading ──────────────────

    fn function_info<'a>(out: &'a SemaOutput, name: &str) -> &'a FunctionInfo {
        out.functions
            .iter()
            .find(|f| f.name == name)
            .unwrap_or_else(|| panic!("no function {name}"))
    }

    #[test]
    fn function_decl_records_result_from_body_expression() {
        let out = analyze_str("LET square(x) = x");
        let f = function_info(&out, "square");
        assert_eq!(f.kind, FunctionKind::Function);
        assert_eq!(f.params, vec!["x".to_string()]);
        // x is a parameter (Word) — body is an Ident referring to it,
        // so the result hint is Word.
        assert_eq!(f.result, TypeHint::Word);
    }

    #[test]
    fn function_with_int_literal_body_returns_int() {
        let out = analyze_str("LET answer() = 42");
        let f = function_info(&out, "answer");
        assert_eq!(f.result, TypeHint::Int);
    }

    #[test]
    fn function_with_float_literal_body_returns_float() {
        let out = analyze_str("LET pi() = 3.14159");
        let f = function_info(&out, "pi");
        assert_eq!(f.result, TypeHint::Float);
    }

    #[test]
    fn routine_records_word_result() {
        let out = analyze_str("LET S() BE { f() }");
        let f = function_info(&out, "S");
        assert_eq!(f.kind, FunctionKind::Routine);
        assert_eq!(f.result, TypeHint::Word);
    }

    #[test]
    fn valof_threads_resultis_back() {
        // Body: VALOF $( RESULTIS 3.14 $) — function returns FLOAT.
        let out = analyze_str("LET f() = VALOF $( RESULTIS 3.14 $)");
        let f = function_info(&out, "f");
        assert_eq!(f.result, TypeHint::Float);
    }

    #[test]
    fn valof_with_multiple_resultis_same_type() {
        let out = analyze_str(
            "LET f(x) = VALOF $(\n  IF x > 0 THEN RESULTIS 1\n  RESULTIS 2\n$)",
        );
        let f = function_info(&out, "f");
        assert_eq!(f.result, TypeHint::Int);
    }

    #[test]
    fn valof_with_mixed_resultis_widens_to_word() {
        let out = analyze_str(
            "LET f(x) = VALOF $(\n  IF x > 0 THEN RESULTIS 1\n  RESULTIS 3.14\n$)",
        );
        let f = function_info(&out, "f");
        assert_eq!(f.result, TypeHint::Word);
    }

    #[test]
    fn call_site_uses_function_result_hint() {
        // y's binding hint should pick up that f() returns FLOAT.
        let out = analyze_str("LET f() = 3.14\nLET y = f()");
        assert_eq!(binding_hint(&out, "y"), TypeHint::Float);
    }

    #[test]
    fn forward_reference_resolves_to_unknown_then_real() {
        // g is called before g's body is analysed. With the pre-pass,
        // g is registered as Unknown; after the main pass, g's result
        // is Int. The first analyse sees Unknown — that's fine, the
        // dependent binding gets re-derived as the program is walked.
        // Here we just check the second case: a binding declared after
        // f's body sees the proper hint.
        let out = analyze_str("LET f() = 1\nLET y = f()");
        assert_eq!(binding_hint(&out, "y"), TypeHint::Int);
    }

    #[test]
    fn nested_valof_blocks() {
        let out = analyze_str(
            "LET outer() = VALOF $(\n  LET inner = VALOF $( RESULTIS 1 $)\n  RESULTIS inner + 1\n$)",
        );
        let f = function_info(&out, "outer");
        assert_eq!(f.result, TypeHint::Int);
        assert_eq!(binding_hint(&out, "inner"), TypeHint::Int);
    }

    // ─── class-aware member access ──────────────────────────────

    #[test]
    fn class_records_decl_fields_as_word() {
        let out = analyze_str("CLASS Point $( DECL x, y $)");
        let c = out
            .classes
            .iter()
            .find(|c| c.name == "Point")
            .expect("Point class");
        assert_eq!(c.fields.len(), 2);
        assert!(c.fields.iter().all(|f| f.hint == TypeHint::Word));
    }

    #[test]
    fn class_records_let_field_with_inferred_hint() {
        let out = analyze_str("CLASS Counter $(\n  LET count = 0\n  LET label = \"hi\"\n$)");
        let c = out
            .classes
            .iter()
            .find(|c| c.name == "Counter")
            .unwrap();
        let count = c.fields.iter().find(|f| f.name == "count").unwrap();
        let label = c.fields.iter().find(|f| f.name == "label").unwrap();
        assert_eq!(count.hint, TypeHint::Int);
        assert_eq!(label.hint, TypeHint::String);
    }

    #[test]
    fn class_flet_field_default_is_float() {
        let out = analyze_str("CLASS Point $(\n  FLET x\n  FLET y = 0.0\n$)");
        let c = out.classes.iter().find(|c| c.name == "Point").unwrap();
        assert_eq!(c.fields.len(), 2);
        for f in &c.fields {
            assert_eq!(f.hint, TypeHint::Float);
        }
    }

    #[test]
    fn member_access_resolves_through_class_table() {
        let out = analyze_str(
            "CLASS Point $(\n  DECL x, y\n  LET color = 0\n$)\nLET p = NEW Point\nLET cx = p.color",
        );
        // p inferred as OBJECT[Point]; p.color is the Int field.
        assert_eq!(binding_hint(&out, "p"), TypeHint::Object);
        assert_eq!(binding_hint(&out, "cx"), TypeHint::Int);
    }

    #[test]
    fn member_access_walks_inheritance_chain() {
        let out = analyze_str(
            "CLASS Animal $( LET legs = 4 $)\nCLASS Dog EXTENDS Animal $( DECL breed $)\nLET d = NEW Dog\nLET n = d.legs",
        );
        // legs is inherited from Animal — Dog → Animal → field found.
        assert_eq!(binding_hint(&out, "n"), TypeHint::Int);
    }

    #[test]
    fn method_call_uses_class_method_result() {
        let out = analyze_str(
            "CLASS Point $(\n  FUNCTION getX() = 3.14\n$)\nLET p = NEW Point\nLET x = p.getX()",
        );
        assert_eq!(binding_hint(&out, "x"), TypeHint::Float);
    }

    #[test]
    fn unknown_field_falls_back_to_word() {
        let out = analyze_str("CLASS Point $( DECL x $)\nLET p = NEW Point\nLET q = p.absent");
        assert_eq!(binding_hint(&out, "q"), TypeHint::Word);
    }

    #[test]
    fn class_method_table_records_signatures() {
        let out = analyze_str(
            "CLASS Point $(\n  FUNCTION getX() = 1.0\n  ROUTINE move(dx, dy) BE $( $)\n  VIRTUAL ROUTINE bark() BE $( $)\n$)",
        );
        let c = out.classes.iter().find(|c| c.name == "Point").unwrap();
        assert_eq!(c.methods.len(), 3);
        let getx = c.methods.iter().find(|m| m.name == "getX").unwrap();
        assert_eq!(getx.kind, FunctionKind::Function);
        assert_eq!(getx.result, TypeHint::Float);
        let mv = c.methods.iter().find(|m| m.name == "move").unwrap();
        assert_eq!(mv.kind, FunctionKind::Routine);
        let bark = c.methods.iter().find(|m| m.name == "bark").unwrap();
        assert!(bark.is_virtual);
    }

    #[test]
    fn self_inside_method_resolves_to_own_class() {
        let out = analyze_str(
            "CLASS Point $(\n  LET x = 0\n  FUNCTION getX() = SELF.x\n$)",
        );
        let c = out.classes.iter().find(|c| c.name == "Point").unwrap();
        let getx = c.methods.iter().find(|m| m.name == "getX").unwrap();
        // SELF is OBJECT[Point], SELF.x looks up the Int field — so
        // the method's body type (and result) is Int.
        assert_eq!(getx.result, TypeHint::Int);
    }

    // ─── control-flow validity warnings ─────────────────────────

    fn warning_count_matching(out: &SemaOutput, needle: &str) -> usize {
        out.warnings
            .iter()
            .filter(|w| w.message.contains(needle))
            .count()
    }

    #[test]
    fn break_outside_loop_warns() {
        let out = analyze_str("LET S() BE { BREAK }");
        assert_eq!(warning_count_matching(&out, "BREAK outside"), 1);
    }

    #[test]
    fn break_inside_while_does_not_warn() {
        let out = analyze_str("LET S() BE { WHILE i < 10 DO $( BREAK $) }");
        assert_eq!(warning_count_matching(&out, "BREAK outside"), 0);
    }

    #[test]
    fn loop_outside_loop_warns() {
        let out = analyze_str("LET S() BE { LOOP }");
        assert_eq!(warning_count_matching(&out, "LOOP outside"), 1);
    }

    #[test]
    fn loop_inside_for_does_not_warn() {
        let out = analyze_str("LET S() BE { FOR i = 1 TO 10 DO LOOP }");
        assert_eq!(warning_count_matching(&out, "LOOP outside"), 0);
    }

    #[test]
    fn break_inside_foreach_does_not_warn() {
        let out = analyze_str("LET S() BE { FOREACH e IN xs DO BREAK }");
        assert_eq!(warning_count_matching(&out, "BREAK outside"), 0);
    }

    #[test]
    fn endcase_outside_switchon_warns() {
        let out = analyze_str("LET S() BE { ENDCASE }");
        assert_eq!(warning_count_matching(&out, "ENDCASE outside"), 1);
    }

    #[test]
    fn endcase_inside_switchon_does_not_warn() {
        let out = analyze_str(
            "LET S() BE { SWITCHON x INTO $( CASE 1: ENDCASE\n DEFAULT: f() $) }",
        );
        assert_eq!(warning_count_matching(&out, "ENDCASE outside"), 0);
    }

    #[test]
    fn resultis_outside_valof_warns() {
        // RESULTIS in a routine body is meaningless.
        let out = analyze_str("LET S() BE { RESULTIS 0 }");
        assert_eq!(warning_count_matching(&out, "RESULTIS outside"), 1);
    }

    #[test]
    fn resultis_inside_valof_silent() {
        let out = analyze_str("LET F() = VALOF $( RESULTIS 1 $)");
        assert_eq!(warning_count_matching(&out, "RESULTIS outside"), 0);
    }

    #[test]
    fn nested_loop_break_targets_inner() {
        // Both BREAKs are inside a loop, neither warns.
        let out = analyze_str(
            "LET S() BE { WHILE x DO $( WHILE y DO BREAK\n BREAK $) }",
        );
        assert_eq!(warning_count_matching(&out, "BREAK outside"), 0);
    }

    // ─── per-expression hint storage on AST ────────────────────

    #[test]
    fn ast_expr_hint_is_unknown_before_sema_runs() {
        // Parse without running sema. Every expression's hint should
        // start at `Unknown` — sema's job is to fill them in.
        let program = newbcpl_parser::parse_source("LET x = 3.14")
            .expect("parse");
        let Decl::Let(l) = &program.items[0] else { panic!() };
        let (_, expr) = &l.bindings[0];
        assert_eq!(expr.hint(), TypeHint::Unknown);
    }

    #[test]
    fn ast_expr_hint_set_by_sema_walk() {
        // After sema, every expression visited gets its hint stamped.
        let program = newbcpl_parser::parse_source("LET x = 3.14\nLET y = x + 1.0")
            .expect("parse");
        let _ = analyze(&program);

        // x: FloatLit → Float
        let Decl::Let(l0) = &program.items[0] else { panic!() };
        let (_, e0) = &l0.bindings[0];
        assert_eq!(e0.hint(), TypeHint::Float);

        // y: Binary { Add, Ident x (Float), FloatLit 1.0 } → Float
        let Decl::Let(l1) = &program.items[1] else { panic!() };
        let (_, e1) = &l1.bindings[0];
        assert_eq!(e1.hint(), TypeHint::Float);
        let Expr::Binary { lhs, rhs, .. } = e1 else {
            panic!()
        };
        assert_eq!(lhs.hint(), TypeHint::Float); // looked up x
        assert_eq!(rhs.hint(), TypeHint::Float); // 1.0
    }

    #[test]
    fn ast_hints_pick_up_subscript_results() {
        let program = newbcpl_parser::parse_source(
            "LET v = VEC 10\nLET fv = FVEC 10\nLET a = v!0\nLET b = fv.%0",
        )
        .expect("parse");
        let _ = analyze(&program);
        let Decl::Let(l_a) = &program.items[2] else { panic!() };
        let Decl::Let(l_b) = &program.items[3] else { panic!() };
        // v!0 → Word (vector elements are typeless words)
        assert_eq!(l_a.bindings[0].1.hint(), TypeHint::Word);
        // fv.%0 → Float
        assert_eq!(l_b.bindings[0].1.hint(), TypeHint::Float);
    }

    #[test]
    fn ast_hint_for_nested_call_callee() {
        // The callee inside `f(x)` has its own hint stamped, even
        // when the parser model wraps it in a Call.
        let program =
            newbcpl_parser::parse_source("LET f() = 3.14\nLET y = f()").expect("parse");
        let _ = analyze(&program);
        let Decl::Let(l) = &program.items[1] else { panic!() };
        let Expr::Call { callee, .. } = &l.bindings[0].1 else {
            panic!()
        };
        // Callee is the ident `f`; sema looks up its binding → FUNCTION.
        assert_eq!(callee.hint(), TypeHint::Function);
    }

    // ─── MANAGED keyword (advisory only post-USING) ────────────
    //
    // The linearity / no-aliasing / no-capture checks that used to
    // live here were retired when `USING name = expr DO body` landed.
    // The keyword still parses, the class still flags `managed` in
    // its layout, but sema no longer emits warnings for aliasing or
    // container capture — the deterministic-cleanup story is now at
    // the use site (`USING`) rather than the declaration site.

    #[test]
    fn binding_info_records_is_managed_flag() {
        let out = analyze_str(
            "CLASS Window MANAGED $( DECL h $)\nLET w = NEW Window",
        );
        let w = out
            .bindings
            .iter()
            .find(|b| b.name == "w")
            .expect("missing w binding");
        assert!(w.is_managed);
        assert_eq!(w.class_name.as_deref(), Some("Window"));
    }

    #[test]
    fn dump_sema_smoke() {
        let prog = newbcpl_parser::parse_source(
            "LET START() BE { LET pi = 3.14\n LET v = VEC 100 }",
        )
        .unwrap();
        let result = analyze(&prog);
        // Render through the public surface (just sanity-check shape).
        assert!(result.bindings.iter().any(|b| b.name == "pi" && b.hint == TypeHint::Float));
        assert!(result.bindings.iter().any(|b| b.name == "v" && b.hint == TypeHint::Vec));
    }
}
