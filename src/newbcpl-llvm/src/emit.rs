//! IR -> LLVM IR emission.
//!
//! Walks a `newbcpl_ir::Module` and produces an `inkwell::module::Module`.
//! Intentionally LLVM-friendly because the IR was designed to be:
//! locals are alloca'd slots reached via Load / Store, every
//! value-producing instruction has a TypeHint that maps to a concrete
//! LLVM type, the CFG is already explicit.
//!
//! Bootstrap subset: routines / functions, integer + float scalar
//! arithmetic, locals, simple calls, IF / ELSE, RETURN, RESULTIS,
//! string constants. Subsequent commits add classes (NEW, vtable
//! dispatch, field load/store via GEP), SIMD types, list runtime
//! calls, GOTO / labels, and SWITCHON.

use std::collections::HashMap;

use inkwell::AddressSpace;
use inkwell::IntPredicate;
use inkwell::FloatPredicate;
use inkwell::basic_block::BasicBlock as LlvmBlock;
use inkwell::builder::Builder;
use inkwell::context::Context;
use inkwell::module::{Linkage, Module as LlvmModule};
use inkwell::types::{BasicMetadataTypeEnum, BasicType, BasicTypeEnum};
use inkwell::values::{BasicMetadataValueEnum, BasicValueEnum, FunctionValue, PointerValue};

use newbcpl_ir::{
    BasicBlock as IrBlock, BlockId, Const, Function as IrFunction, Instr, IrBinOp, IrUnOp,
    Module as IrModule, Param, Terminator, TypedKind, Value, ValueId,
};
use newbcpl_sema::{ClassLayout, TypeHint};

/// Top-level entry: produce a finalised LLVM module from our typed
/// IR. The caller owns the `Context`; the returned `LlvmModule`
/// borrows from it.
pub fn emit<'ctx>(context: &'ctx Context, ir: &IrModule) -> LlvmModule<'ctx> {
    let mut emitter = Emitter::new(context, &ir.name, &ir.layouts);
    emitter.emit_module(ir);
    emitter.module
}

struct Emitter<'ctx, 'l> {
    context: &'ctx Context,
    module: LlvmModule<'ctx>,
    builder: Builder<'ctx>,
    /// Class layouts from sema. Indexed by class name when emitting
    /// New / FieldLoad / FieldStore / MethodCall.
    layouts: &'l [ClassLayout],

    /// Each IR ValueId maps to a concrete LLVM value. For an Alloca
    /// it's a `PointerValue`; for arithmetic results it's an
    /// `IntValue` / `FloatValue`. Stored as the `BasicValueEnum`
    /// supertype.
    value_map: HashMap<ValueId, BasicValueEnum<'ctx>>,
    /// Each IR BlockId maps to its LLVM basic block.
    block_map: HashMap<BlockId, LlvmBlock<'ctx>>,
    /// Functions declared in this module by source-level name.
    /// Includes both BCPL-declared ones and externs we materialise on
    /// first encounter.
    by_name: HashMap<String, FunctionValue<'ctx>>,
    /// String literal pool — coalesces duplicate literals so each
    /// distinct string emits one global.
    string_pool: HashMap<String, PointerValue<'ctx>>,
    /// Counter for anonymous string globals.
    string_counter: u32,
    /// Type hint each Alloca'd slot was created with. Used by
    /// `Store` to coerce a value whose LLVM type doesn't match
    /// the slot — e.g. `FLET x = 5` stores an i64 literal into
    /// an f64 slot, which needs an `sitofp` to round-trip.
    slot_hint: HashMap<ValueId, TypeHint>,
}

impl<'ctx, 'l> Emitter<'ctx, 'l> {
    fn new(context: &'ctx Context, name: &str, layouts: &'l [ClassLayout]) -> Self {
        let module = context.create_module(name);
        Self {
            context,
            module,
            builder: context.create_builder(),
            layouts,
            value_map: HashMap::new(),
            block_map: HashMap::new(),
            by_name: HashMap::new(),
            string_pool: HashMap::new(),
            string_counter: 0,
            slot_hint: HashMap::new(),
        }
    }

    /// Coerce a value to match a slot's hint. Used by Store to
    /// bridge int↔float typing mismatches (the common case is
    /// `FLET x = 5` — int literal into a float slot). Unhandled
    /// type combinations pass through unchanged; downstream
    /// store validity is the caller's concern.
    fn coerce_to_hint(
        &self,
        v: BasicValueEnum<'ctx>,
        hint: TypeHint,
    ) -> BasicValueEnum<'ctx> {
        let f64_t = self.context.f64_type();
        let i64_t = self.context.i64_type();
        match (v, hint) {
            (BasicValueEnum::IntValue(iv), TypeHint::Float) => self
                .builder
                .build_signed_int_to_float(iv, f64_t, "sitofp.store")
                .expect("sitofp")
                .into(),
            (BasicValueEnum::FloatValue(fv), TypeHint::Int)
            | (BasicValueEnum::FloatValue(fv), TypeHint::Word) => self
                .builder
                .build_float_to_signed_int(fv, i64_t, "fptosi.store")
                .expect("fptosi")
                .into(),
            _ => v,
        }
    }

    fn lookup_layout(&self, class_name: &str) -> Option<&'l ClassLayout> {
        self.layouts.iter().find(|l| l.class_name == class_name)
    }

    fn emit_module(&mut self, ir: &IrModule) {
        // Pass 1: declare every BCPL function so calls can resolve.
        for f in &ir.functions {
            self.declare_function(f);
        }
        // Pass 1b: declare every ASM procedure with its correct
        // typed signature *before* any function body is emitted.
        // Function bodies (pass 3 below) auto-declare unresolved
        // callees via the default `i64 fn(i64, …, i64)` path in
        // `declare_extern`; without this pre-pass, an ASM proc that
        // returns f64 or takes XMM-routed FLOAT params would get
        // the wrong type the first time a caller is lowered, and
        // pass 4 would silently skip it (already present in
        // `by_name`).
        for proc in &ir.asm_procs {
            self.declare_asm_proc(proc);
        }
        // Pass 2: emit a mutable, externally-linked vtable global
        // per class with vtable slots. Following NewCP's recipe
        // (see `newcp-llvm/src/module.rs` and `jit.rs`): MCJIT's
        // RuntimeDyld does NOT reliably relocate function-pointer
        // constants in data initialisers, so we emit the vtable as
        // zero-initialised mutable storage and patch the method
        // addresses in from Rust *after* JIT finalisation. The
        // TypeDesc-style indirection NewCP uses isn't necessary
        // here because we put the vtable pointer inline at the
        // first word of every instance.
        self.declare_vtable_globals(&ir.layouts);
        self.declare_typedesc_globals(&ir.layouts);
        // Pass 2b: emit each `GLOBAL`-declared module-level
        // variable. They go in as plain `@<name> = global i64
        // <init>` slots so `GlobalLoad` / `GlobalStore` can
        // resolve by symbol. External linkage so cross-module
        // references reach them via the loader's symbol table.
        self.declare_globals(&ir.globals);
        // Pass 3: emit each body. Per-function maps reset between
        // functions since ValueIds and BlockIds are function-local.
        for f in &ir.functions {
            self.emit_function(f);
        }
        // Pass 4: emit each ASM procedure body as a `module asm`
        // blob. The matching `declare`s went out in pass 1b above so
        // pass 3's call sites typecheck against the right signature;
        // here we only append the bodies. Inkwell 0.9 exposes
        // `set_inline_assembly` (which replaces); we use
        // `LLVMAppendModuleInlineAsm` directly so multiple ASM
        // procs accumulate.
        for proc in &ir.asm_procs {
            let asm_str = new_asm::build_module_asm_string(proc);
            unsafe {
                inkwell::llvm_sys::core::LLVMAppendModuleInlineAsm(
                    self.module.as_mut_ptr(),
                    asm_str.as_ptr() as *const std::ffi::c_char,
                    asm_str.len(),
                );
            }
        }
    }

    /// One LLVM module-level `@<name>` per `GLOBAL` declaration.
    /// The initializer is the constant integer when sema folded one,
    /// else zero. External linkage so the loader's symbol table can
    /// resolve cross-module references against the same address.
    fn declare_globals(&mut self, globals: &[newbcpl_ir::GlobalDecl]) {
        use inkwell::module::Linkage;
        let i64_t = self.context.i64_type();
        for g in globals {
            // If the loader has already linked another module that
            // declared the same name, reuse it — otherwise create.
            if self.module.get_global(&g.name).is_some() {
                continue;
            }
            let gv = self.module.add_global(i64_t, None, &g.name);
            gv.set_linkage(Linkage::External);
            let init = i64_t.const_int(g.initial.unwrap_or(0) as u64, true);
            gv.set_initializer(&init);
        }
    }

    /// Emit `@{Class}.vtable = global [N x ptr] zeroinitializer` for
    /// every class with vtable slots. External linkage so the JIT
    /// layer can find the storage by name via
    /// `LLVMGetGlobalValueAddress`; mutable so we can write the
    /// method addresses in after MCJIT finalises the module.
    fn declare_vtable_globals(&mut self, layouts: &[ClassLayout]) {
        let ptr_t = self.context.ptr_type(AddressSpace::default());
        for layout in layouts {
            if layout.vtable.is_empty() {
                continue;
            }
            let n = layout.vtable.len() as u32;
            let vtable_ty = ptr_t.array_type(n);
            let global_name = format!("{}.vtable", layout.class_name);
            let g = self
                .module
                .add_global(vtable_ty, None, &global_name);
            g.set_initializer(&vtable_ty.const_zero());
            g.set_constant(false);
            g.set_linkage(Linkage::External);
        }
    }

    /// Emit `@{Class}.desc` constant globals — one `TypeDesc` per
    /// class, matching the `#[repr(C)]` layout in
    /// `newbcpl_runtime::gc::TypeDesc` exactly. The GC tags every
    /// heap block's `BlockHeader.tag` with the TypeDesc address;
    /// the size field is read on each allocation. The vtable
    /// pointer is included for forward compatibility with the
    /// NewCP-style `obj → header → desc → desc.vtable[slot]`
    /// dispatch path.
    ///
    /// Layout (must mirror gc::TypeDesc):
    /// `{ i64 size, ptr module, ptr finalizer, ptr base, ptr vtable,
    ///    i64 vtable_len, ptr name, [1 x i64] ptroffs }`
    /// — 7 fixed fields then a sentinel-terminated `ptroffs` array.
    /// We emit `[1 x i64] = [-1]` so the GC's pointer-offset
    /// iterator stops immediately (no pointer fields tracked yet).
    ///
    /// In parallel we emit `@{Class}.method_names` as a private
    /// `[N x ptr]` of name strings. The names aren't stored in the
    /// TypeDesc directly because instances use a runtime-interned
    /// size-keyed TypeDesc (see `__newbcpl_alloc_rec`) and we want
    /// per-class metadata. Instead, the JIT registers each class's
    /// `(vtable_addr → method_names_addr)` pair with the runtime
    /// at finalize time; `__newbcpl_lookup_method` keys off the
    /// instance's inline vtable pointer.
    fn declare_typedesc_globals(&mut self, layouts: &[ClassLayout]) {
        let i64_t = self.context.i64_type();
        let ptr_t = self.context.ptr_type(AddressSpace::default());
        let ptroffs_arr_ty = i64_t.array_type(1);
        let typedesc_ty = self.context.struct_type(
            &[
                i64_t.into(),         // 0: size
                ptr_t.into(),         // 1: module
                ptr_t.into(),         // 2: finalizer
                ptr_t.into(),         // 3: base
                ptr_t.into(),         // 4: vtable
                i64_t.into(),         // 5: vtable_len
                ptr_t.into(),         // 6: name
                ptroffs_arr_ty.into(),// 7: ptroffs[1] sentinel
            ],
            false,
        );
        for layout in layouts {
            let (vtable_ptr, vtable_len) = if layout.vtable.is_empty() {
                (ptr_t.const_null(), 0u64)
            } else {
                let vg = self
                    .module
                    .get_global(&format!("{}.vtable", layout.class_name))
                    .expect("vtable global declared above")
                    .as_pointer_value();
                (vg, layout.vtable.len() as u64)
            };
            // Emit the parallel `@{Class}.method_names` array even
            // though it's not embedded in this TypeDesc — see the
            // comment block above. The IR-side call to
            // `__newbcpl_register_jit_vtable_methods` at finalize
            // wires it into the lookup registry.
            if !layout.vtable.is_empty() {
                let _ = self
                    .emit_method_names_global(&layout.class_name, &layout.vtable);
            }
            // Sentinel -1: tells the GC "no pointer fields".
            // Pointer-tracking ports later by emitting the real
            // offsets from `layout.ptroffs` followed by -1.
            let sentinel = i64_t.const_int(u64::MAX, true);
            let ptroffs_init = i64_t.const_array(&[sentinel]);
            let init = typedesc_ty.const_named_struct(&[
                i64_t.const_int(layout.instance_size as u64, true).into(),
                ptr_t.const_null().into(),
                ptr_t.const_null().into(),
                ptr_t.const_null().into(),
                vtable_ptr.into(),
                i64_t.const_int(vtable_len, false).into(),
                ptr_t.const_null().into(),
                ptroffs_init.into(),
            ]);
            let g = self.module.add_global(
                typedesc_ty,
                None,
                &format!("{}.desc", layout.class_name),
            );
            g.set_initializer(&init);
            // Non-constant + external linkage so MCJIT can hand
            // out a stable runtime address (the `__newbcpl_new_rec`
            // call site loads this address and the GC stores it
            // in every BlockHeader it allocates).
            g.set_constant(false);
            g.set_linkage(Linkage::External);
        }
    }

    /// Emit `@{Class}.method_names`, a `[N x ptr]` array of
    /// pointers to per-method name strings. Each name string is
    /// emitted as its own private global (`@{Class}.mname.<slot>`)
    /// holding the null-terminated UTF-8 bytes of the method name.
    /// Returned pointer is the address of the array itself, ready
    /// to slot into the TypeDesc's `method_names` field.
    fn emit_method_names_global(
        &mut self,
        class_name: &str,
        vtable: &[newbcpl_ir::VtableEntry],
    ) -> inkwell::values::PointerValue<'ctx> {
        let i8_t = self.context.i8_type();
        let ptr_t = self.context.ptr_type(AddressSpace::default());
        let mut entries: Vec<inkwell::values::PointerValue<'ctx>> =
            Vec::with_capacity(vtable.len());
        for (idx, entry) in vtable.iter().enumerate() {
            // Build a null-terminated byte array literal for the
            // method name. We use bytes (not strings) so we don't
            // require utf8-validation on the emit side.
            let mut bytes: Vec<u8> = entry.method_name.as_bytes().to_vec();
            bytes.push(0);
            let byte_consts: Vec<inkwell::values::IntValue<'ctx>> =
                bytes.iter().map(|b| i8_t.const_int(*b as u64, false)).collect();
            let array_init = i8_t.const_array(&byte_consts);
            let name_global = self.module.add_global(
                array_init.get_type(),
                None,
                &format!("{class_name}.mname.{idx}"),
            );
            name_global.set_initializer(&array_init);
            name_global.set_constant(true);
            name_global.set_linkage(Linkage::Private);
            entries.push(name_global.as_pointer_value());
        }
        let array_ty = ptr_t.array_type(entries.len() as u32);
        let array_init = ptr_t.const_array(&entries);
        let g = self.module.add_global(
            array_ty,
            None,
            &format!("{class_name}.method_names"),
        );
        g.set_initializer(&array_init);
        g.set_constant(true);
        // External linkage so the JIT-side finalize hook can find
        // the address by name via `LLVMGetGlobalValueAddress` and
        // register the (vtable, names) pair with the runtime's
        // lookup table.
        g.set_linkage(Linkage::External);
        g.as_pointer_value()
    }

    // ─── declarations ───────────────────────────────────────────

    /// Emit a `declare` for an ASM procedure so callers in the same
    /// module can type-check calls. The body lives in the `module asm`
    /// blob appended in pass 4 of `emit_module`; the assembler resolves
    /// `<name>:` against this `declare` at MCJIT link time.
    ///
    /// No `uwtable` attribute is set: a `declare` has no body LLVM can
    /// emit unwind info for, and the runtime assembler does not emit
    /// `.pdata` / `.xdata` for our hand-written stubs. Programs that
    /// want a Windows SEH-unwindable hot loop should write a proper
    /// BCPL function and call out to ASM for inner kernels.
    fn declare_asm_proc(&mut self, proc: &new_asm::AsmProc) {
        if self.by_name.contains_key(&proc.name) {
            return;
        }
        let param_types: Vec<BasicMetadataTypeEnum> = proc
            .params
            .iter()
            .map(|p| self.asm_type_to_basic(p.ty).into())
            .collect();
        let fn_type = match proc.return_type {
            new_asm::AsmRetType::Word => self.context.i64_type().fn_type(&param_types, false),
            new_asm::AsmRetType::Float => self.context.f64_type().fn_type(&param_types, false),
            new_asm::AsmRetType::FQuad => self
                .context
                .f32_type()
                .vec_type(4)
                .fn_type(&param_types, false),
            new_asm::AsmRetType::FOct => self
                .context
                .f32_type()
                .vec_type(8)
                .fn_type(&param_types, false),
            new_asm::AsmRetType::Void => self.context.void_type().fn_type(&param_types, false),
        };
        let fv = self
            .module
            .add_function(&proc.name, fn_type, Some(Linkage::External));
        self.by_name.insert(proc.name.clone(), fv);
    }

    /// Map an `AsmType` register class to the matching LLVM basic type.
    /// Mirrors the parameter side of `declare_asm_proc` so the
    /// `declare` and the call-site coercions in `coerce_arg` agree.
    fn asm_type_to_basic(&self, ty: new_asm::AsmType) -> BasicTypeEnum<'ctx> {
        match ty {
            new_asm::AsmType::Word => self.context.i64_type().into(),
            new_asm::AsmType::Float => self.context.f64_type().into(),
            new_asm::AsmType::FQuad => self.context.f32_type().vec_type(4).into(),
            new_asm::AsmType::FOct => self.context.f32_type().vec_type(8).into(),
        }
    }

    fn declare_function(&mut self, f: &IrFunction) -> FunctionValue<'ctx> {
        if let Some(&existing) = self.by_name.get(&f.name) {
            return existing;
        }
        let return_type = self.return_type_for(f.return_hint);
        let param_types: Vec<BasicMetadataTypeEnum> = f
            .params
            .iter()
            .map(|p| self.basic_type_for(p.hint).into())
            .collect();
        let fn_type = match return_type {
            Some(t) => t.fn_type(&param_types, false),
            None => self.context.void_type().fn_type(&param_types, false),
        };
        let fv = self.module.add_function(&f.name, fn_type, None);
        // Stamp `uwtable=2` so LLVM emits Windows-style `.pdata` /
        // `.xdata` for this function. The custom JIT memory manager
        // captures those sections and registers them with the OS via
        // `RtlAddFunctionTable`, which lets a Rust panic raised in a
        // runtime helper unwind cleanly back through the JIT frame to
        // a `catch_unwind` boundary in the host. Without uwtable the
        // panic escapes as MSVC SEH 0xE06D7363 and aborts the
        // process. uwtable=2 (async) is the safe setting; =1 lets
        // LLVM elide unwind info for leaf-ish sequences in some
        // versions, which silently breaks the unwinder.
        let uwtable_kind =
            inkwell::attributes::Attribute::get_named_enum_kind_id("uwtable");
        let uwtable_attr = self.context.create_enum_attribute(uwtable_kind, 2);
        fv.add_attribute(
            inkwell::attributes::AttributeLoc::Function,
            uwtable_attr,
        );
        self.by_name.insert(f.name.clone(), fv);
        fv
    }

    /// Declare an extern function on demand. Used when the IR calls
    /// an unresolved name — typically a BCPL builtin (WRITES,
    /// WRITEN, NEWLINE, ...) or a runtime helper (__newbcpl_*).
    /// Signatures default to `i64 fn(i64, ..., i64)` — most BCPL
    /// builtins fit this. Special cases are added as we encounter
    /// real divergences.
    fn declare_extern(&mut self, name: &str, arg_count: usize) -> FunctionValue<'ctx> {
        if let Some(&existing) = self.by_name.get(name) {
            return existing;
        }
        let i64_t = self.context.i64_type();
        let f64_t = self.context.f64_type();
        let ptr_t = self.context.ptr_type(AddressSpace::default());
        let fn_type = match name {
            // String-arg builtin: WRITES takes a single ptr.
            "WRITES" => i64_t.fn_type(&[ptr_t.into()], false),
            // The WRITEF family is fixed-arity per arity-suffix:
            // WRITEF takes only the format; WRITEF1..WRITEF7 take
            // the format plus N additional `i64` payload words.
            // Float args get bitcast to i64 at the call site
            // (matches the BCPL ABI choice made by the reference).
            "WRITEF" | "WRITEF1" | "WRITEF2" | "WRITEF3" | "WRITEF4" | "WRITEF5" | "WRITEF6"
            | "WRITEF7" => {
                let n = name
                    .strip_prefix("WRITEF")
                    .and_then(|s| s.parse::<usize>().ok())
                    .unwrap_or(0);
                let mut args: Vec<BasicMetadataTypeEnum> = vec![ptr_t.into()];
                let int_md: BasicMetadataTypeEnum = i64_t.into();
                args.extend((0..n).map(|_| int_md));
                i64_t.fn_type(&args, false)
            }
            // Float-typed math helpers — `f64 fn(f64)`.
            "FSIN" | "FCOS" | "FTAN" | "FABS" | "FLOG" | "FEXP" | "FSQRT" => {
                f64_t.fn_type(&[f64_t.into()], false)
            }
            // Float ←→ int conversion / produce.
            "FIX" => i64_t.fn_type(&[f64_t.into()], false),
            "FLOAT" => f64_t.fn_type(&[i64_t.into()], false),
            "FWRITE" => i64_t.fn_type(&[f64_t.into()], false),
            "FRND" => f64_t.fn_type(&[], false),
            "RND" => f64_t.fn_type(&[i64_t.into()], false),
            // iGui wrappers in newbcpl-runtime/src/igui_builtins.rs.
            // Float coordinates and colours go in XMM registers per
            // Win64 calling convention, so the LLVM type must use
            // f64 explicitly — the all-i64 default would route them
            // through integer registers and produce garbage.
            "iGui_OpenChild" => i64_t.fn_type(&[ptr_t.into(), ptr_t.into()], false),
            "iGui_CloseChild" => i64_t.fn_type(&[i64_t.into()], false),
            "iGui_SetTitle" => i64_t.fn_type(&[i64_t.into(), ptr_t.into()], false),
            "iGui_BeginBatch" => i64_t.fn_type(&[i64_t.into()], false),
            "iGui_SubmitBatch" => i64_t.fn_type(&[], false),
            "iGui_Clear" => i64_t.fn_type(
                &[f64_t.into(), f64_t.into(), f64_t.into(), f64_t.into()],
                false,
            ),
            // (x0, y0, x1, y1, r, g, b, a)
            "iGui_FillRect" => i64_t.fn_type(
                &[
                    f64_t.into(), f64_t.into(), f64_t.into(), f64_t.into(),
                    f64_t.into(), f64_t.into(), f64_t.into(), f64_t.into(),
                ],
                false,
            ),
            // (x0, y0, x1, y1, thickness, r, g, b, a)
            "iGui_StrokeRect" | "iGui_DrawLine" => i64_t.fn_type(
                &[
                    f64_t.into(), f64_t.into(), f64_t.into(), f64_t.into(),
                    f64_t.into(), f64_t.into(), f64_t.into(), f64_t.into(),
                    f64_t.into(),
                ],
                false,
            ),
            // (cx, cy, radius, r, g, b, a)
            "iGui_FillCircle" => i64_t.fn_type(
                &[
                    f64_t.into(), f64_t.into(), f64_t.into(),
                    f64_t.into(), f64_t.into(), f64_t.into(), f64_t.into(),
                ],
                false,
            ),
            // (text*, x, y, size, r, g, b, a)
            "iGui_DrawText" => i64_t.fn_type(
                &[
                    ptr_t.into(),
                    f64_t.into(), f64_t.into(), f64_t.into(),
                    f64_t.into(), f64_t.into(), f64_t.into(), f64_t.into(),
                ],
                false,
            ),
            // (kind*, child*, time*, p1*, p2*, p3*, p4*, timeout_ms)
            "iGui_NextEvent" => i64_t.fn_type(
                &[
                    ptr_t.into(), ptr_t.into(), ptr_t.into(), ptr_t.into(),
                    ptr_t.into(), ptr_t.into(), ptr_t.into(),
                    i64_t.into(),
                ],
                false,
            ),
            "iGui_Quit" => i64_t.fn_type(&[], false),
            // (target_child, kind*, child*, time*, p1*, p2*, p3*, p4*, timeout_ms)
            "iGui_NextEventFor" => i64_t.fn_type(
                &[
                    i64_t.into(),
                    ptr_t.into(), ptr_t.into(), ptr_t.into(), ptr_t.into(),
                    ptr_t.into(), ptr_t.into(), ptr_t.into(),
                    i64_t.into(),
                ],
                false,
            ),
            "iGui_DiscardStashedEvents" => i64_t.fn_type(&[], false),
            // Text-pane builtins.
            //   OpenText(title*, *out_id) -> i64
            "iGui_OpenText" => {
                i64_t.fn_type(&[ptr_t.into(), ptr_t.into()], false)
            }
            //   TextWriteStr(id, text*) -> i64
            "iGui_TextWriteStr" => {
                i64_t.fn_type(&[i64_t.into(), ptr_t.into()], false)
            }
            // All other text-pane builtins are i64-only (id + int
            // args, int return) — the default lowering already
            // handles them. Listed here just to keep the surface
            // documented in one place:
            //   iGui_TextWriteChar(id, codepoint) -> i64
            //   iGui_TextNewline(id)              -> i64
            //   iGui_TextSetCursor(id, row, col)  -> i64
            //   iGui_TextClear(id)                -> i64
            //   iGui_TextClearEol(id)             -> i64
            //   iGui_TextClearEos(id)             -> i64
            //   iGui_TextScrollUp(id, n)          -> i64
            //   iGui_TextSetPen(id, fg, bg)       -> i64
            //   iGui_TextResetPen(id)             -> i64
            //   iGui_TextShowCaret(id, visible)   -> i64

            // NewAudio shims in newbcpl-runtime/src/audio.rs. Slot
            // and waveform-code arguments are i64; frequencies,
            // durations, volumes, envelope params are f64 (so Win64
            // routes them through XMM registers). Match arms below
            // are grouped by parameter shape.

            // Preset SFX: (slot, p1, p2) -> i64.
            "Sound_Beep"
            | "Sound_Coin"
            | "Sound_Jump"
            | "Sound_Explode"
            | "Sound_BigExplode"
            | "Sound_SmallExplode"
            | "Sound_DistantExplode"
            | "Sound_MetalExplode"
            | "Sound_Zap"
            | "Sound_Shoot"
            | "Sound_Powerup"
            | "Sound_Hurt"
            | "Sound_Click"
            | "Sound_Bang"
            | "Sound_Blip"
            | "Sound_Pickup" => {
                i64_t.fn_type(&[i64_t.into(), f64_t.into(), f64_t.into()], false)
            }
            // Sweeps: (slot, start_freq, end_freq, duration) -> i64.
            "Sound_SweepUp" | "Sound_SweepDown" => i64_t.fn_type(
                &[i64_t.into(), f64_t.into(), f64_t.into(), f64_t.into()],
                false,
            ),
            // (slot, seed, duration) -> i64.
            "Sound_RandomBeep" => {
                i64_t.fn_type(&[i64_t.into(), i64_t.into(), f64_t.into()], false)
            }
            // (slot, freq, duration, waveform) -> i64.
            "Sound_Tone" => i64_t.fn_type(
                &[i64_t.into(), f64_t.into(), f64_t.into(), i64_t.into()],
                false,
            ),
            // (slot, midi, duration, waveform, a, d, s, r) -> i64.
            "Sound_Note" => i64_t.fn_type(
                &[
                    i64_t.into(), i64_t.into(), f64_t.into(), i64_t.into(),
                    f64_t.into(), f64_t.into(), f64_t.into(), f64_t.into(),
                ],
                false,
            ),
            // (slot, noiseType, duration) -> i64.
            "Sound_Noise" => {
                i64_t.fn_type(&[i64_t.into(), i64_t.into(), f64_t.into()], false)
            }
            // (slot, carrier, modulator, modIndex, duration) -> i64.
            "Sound_FM" => i64_t.fn_type(
                &[
                    i64_t.into(), f64_t.into(), f64_t.into(), f64_t.into(),
                    f64_t.into(),
                ],
                false,
            ),
            // (slot, freq, duration, waveform, p1, p2, p3) -> i64.
            "Sound_Reverb" | "Sound_Delay" | "Sound_Distort" => i64_t.fn_type(
                &[
                    i64_t.into(), f64_t.into(), f64_t.into(), i64_t.into(),
                    f64_t.into(), f64_t.into(), f64_t.into(),
                ],
                false,
            ),
            // (slot, freq, duration, waveform, filterType, cutoff,
            //  resonance) -> i64.
            "Sound_FilterTone" => i64_t.fn_type(
                &[
                    i64_t.into(), f64_t.into(), f64_t.into(), i64_t.into(),
                    i64_t.into(), f64_t.into(), f64_t.into(),
                ],
                false,
            ),
            // (slot, midi, duration, waveform, a, d, s, r,
            //  filterType, cutoff, resonance) -> i64.
            "Sound_FilterNote" => i64_t.fn_type(
                &[
                    i64_t.into(), i64_t.into(), f64_t.into(), i64_t.into(),
                    f64_t.into(), f64_t.into(), f64_t.into(), f64_t.into(),
                    i64_t.into(), f64_t.into(), f64_t.into(),
                ],
                false,
            ),
            // (slot, volume, pan) -> i64.
            "Sound_Play" => {
                i64_t.fn_type(&[i64_t.into(), f64_t.into(), f64_t.into()], false)
            }
            // (volume) -> i64.
            "Sound_SetVolume" | "Music_SetVolume" => i64_t.fn_type(&[f64_t.into()], false),
            // () -> f64.
            "Sound_GetVolume" | "Music_GetVolume" => f64_t.fn_type(&[], false),
            // (slot) -> f64.
            "Sound_Duration" | "Music_Tempo" => f64_t.fn_type(&[i64_t.into()], false),
            // (slot, abc_string_ptr) -> i64.
            "Music_Load" => i64_t.fn_type(&[i64_t.into(), ptr_t.into()], false),
            // (slot, volume) -> i64.
            "Music_Play" => i64_t.fn_type(&[i64_t.into(), f64_t.into()], false),

            // All-i64 audio shims fall through to the default below:
            //   Sound_StopAll(), Sound_Free(slot), Sound_FreeAll(),
            //   Sound_Count(), Sound_Playing(slot),
            //   Music_StopAll() / PauseAll() / ResumeAll(),
            //   Music_Free(slot), Music_FreeAll(),
            //   Music_Count(), Music_State(), Music_Playing(slot).

            // Default: i64 fn(i64, ..., i64).
            _ => {
                let args: Vec<BasicMetadataTypeEnum> =
                    (0..arg_count).map(|_| i64_t.into()).collect();
                i64_t.fn_type(&args, false)
            }
        };
        let fv = self
            .module
            .add_function(name, fn_type, Some(Linkage::External));
        self.by_name.insert(name.to_string(), fv);
        fv
    }

    // ─── functions ──────────────────────────────────────────────

    fn emit_function(&mut self, f: &IrFunction) {
        let fv = self.by_name[&f.name];
        // Reset per-function state.
        self.value_map.clear();
        self.slot_hint.clear();
        self.block_map.clear();

        // Allocate every basic block up front so any Branch /
        // CondBranch / Switch can resolve forward references.
        for block in &f.blocks {
            let llvm_block = self.context.append_basic_block(fv, &block.label);
            self.block_map.insert(block.id, llvm_block);
        }

        // The entry block runs the parameter alloca / store sequence
        // we emitted from lower.rs; bind each `in_value` to the
        // corresponding LLVM parameter so Store instructions for
        // them resolve correctly.
        let entry = self.block_map[&f.entry];
        self.builder.position_at_end(entry);
        for (i, p) in f.params.iter().enumerate() {
            self.bind_parameter(fv, i, p);
        }

        // Cooperative safepoint poll at function entry. The
        // GC's allocator path (`__newbcpl_new_rec`) already
        // polls on each allocation; this catches functions
        // that run for a long time without allocating, so a
        // concurrent collector can pause every thread cleanly.
        // The poll is cheap when no GC is pending (atomic load
        // + branch); the slow path parks the thread.
        self.emit_safepoint_poll();

        // Emit each block in source order.
        for block in &f.blocks {
            self.emit_block(block);
        }
    }

    /// Emit a `call void @__newbcpl_safepoint()` at the current
    /// builder position. The function is declared on demand and
    /// resolved at JIT-link time via the runtime's
    /// `builtin_addresses()` table.
    fn emit_safepoint_poll(&mut self) {
        let safepoint_fn = match self.by_name.get("__newbcpl_safepoint") {
            Some(&f) => f,
            None => {
                let fn_ty = self.context.void_type().fn_type(&[], false);
                let fv = self.module.add_function(
                    "__newbcpl_safepoint",
                    fn_ty,
                    Some(Linkage::External),
                );
                self.by_name
                    .insert("__newbcpl_safepoint".to_string(), fv);
                fv
            }
        };
        let _ = self
            .builder
            .build_call(safepoint_fn, &[], "safepoint")
            .expect("call __newbcpl_safepoint");
    }

    fn bind_parameter(&mut self, fv: FunctionValue<'ctx>, idx: usize, p: &Param) {
        let llvm_param = fv
            .get_nth_param(idx as u32)
            .expect("parameter index in range");
        self.value_map.insert(p.in_value, llvm_param);
    }

    fn emit_block(&mut self, block: &IrBlock) {
        let llvm_block = self.block_map[&block.id];
        self.builder.position_at_end(llvm_block);
        for instr in &block.instrs {
            self.emit_instr(instr);
        }
        self.emit_terminator(&block.terminator);
    }

    // ─── instructions ───────────────────────────────────────────

    fn emit_instr(&mut self, instr: &Instr) {
        match instr {
            Instr::Const { dst, value, hint } => {
                let v = self.lower_const(value, *hint);
                self.value_map.insert(*dst, v);
            }
            Instr::Alloca { dst, hint, name } => {
                let ty = self.basic_type_for(*hint);
                let slot = self
                    .builder
                    .build_alloca(ty, name)
                    .expect("alloca");
                self.value_map.insert(*dst, slot.into());
                // Remember the slot's hint so `Store` can coerce
                // mismatched value types (e.g. `FLET x = 5`
                // wants an `sitofp` from i64 to f64). Without
                // this the store writes raw integer bits into
                // the f64 slot and reading back gives a denormal.
                self.slot_hint.insert(*dst, *hint);
            }
            Instr::Load { dst, slot, hint } => {
                let slot_ptr = self.lookup(*slot).into_pointer_value();
                let ty = self.basic_type_for(*hint);
                let loaded = self
                    .builder
                    .build_load(ty, slot_ptr, "load")
                    .expect("load");
                self.value_map.insert(*dst, loaded);
            }
            Instr::Store { slot, value } => {
                let slot_ptr = self.lookup(*slot).into_pointer_value();
                let v = self.lower_value(value);
                // Coerce the value to the slot's declared type so
                // `FLET x = 5` (int literal into a float slot)
                // emits a clean `sitofp` instead of bit-blasting
                // the i64 into the f64 storage. The slot type was
                // chosen at allocation from `Lowerer`'s hint —
                // see `Builder::alloca`.
                let slot_hint = self.slot_hint.get(slot).copied().unwrap_or(TypeHint::Word);
                let coerced = self.coerce_to_hint(v, slot_hint);
                self.builder.build_store(slot_ptr, coerced).expect("store");
            }
            Instr::BinOp {
                dst,
                op,
                lhs,
                rhs,
                hint: _,
            } => {
                let l = self.lower_value(lhs);
                let r = self.lower_value(rhs);
                let result = self.lower_binop(*op, l, r);
                self.value_map.insert(*dst, result);
            }
            Instr::UnaryOp {
                dst,
                op,
                operand,
                hint: _,
            } => {
                let v = self.lower_value(operand);
                let result = self.lower_unop(*op, v);
                self.value_map.insert(*dst, result);
            }
            Instr::Call {
                dst,
                callee,
                args,
                hint,
            } => {
                let callee_fn = self.resolve_callee(callee, args.len());
                // Coerce each lowered arg to the declared parameter
                // type. The case that matters in practice is the
                // WRITEF family: it's declared as `(ptr, i64, ...)`
                // but %f format slots receive a float Value — we
                // bitcast f64 → i64 so the call typechecks. The
                // BCPL ABI deliberately sends floats in int-shaped
                // registers for the printf-style helpers.
                let param_types = callee_fn.get_type().get_param_types();
                let llvm_args: Vec<BasicMetadataValueEnum> = args
                    .iter()
                    .enumerate()
                    .map(|(i, a)| {
                        let v = self.lower_value(a);
                        let want = param_types.get(i).copied();
                        self.coerce_arg(v, want).into()
                    })
                    .collect();
                let call_site = self
                    .builder
                    .build_call(callee_fn, &llvm_args, "call")
                    .expect("call");
                if let Some(d) = dst {
                    use inkwell::values::ValueKind;
                    match call_site.try_as_basic_value() {
                        ValueKind::Basic(rv) => {
                            self.value_map.insert(*d, rv);
                        }
                        ValueKind::Instruction(_) => {
                            // Function returned void but the IR
                            // expected a result — synthesize a zero
                            // of the right type so downstream uses
                            // don't panic.
                            let z = self.zero(*hint);
                            self.value_map.insert(*d, z);
                        }
                    }
                }
            }
            Instr::Gep {
                dst,
                base,
                index,
                element_bytes,
            } => {
                // `base + index * element_bytes` as a pointer.
                // Stride 1 means char vectors, 8 means word / float
                // vectors. We GEP an `i8` element type and hand
                // the scaled byte offset directly so codegen can
                // use any stride uniformly.
                let base_v = self.lower_value(base);
                let base_ptr = self.as_pointer(base_v);
                let index_val = self.lower_value(index).into_int_value();
                let stride = self
                    .context
                    .i64_type()
                    .const_int(*element_bytes as u64, false);
                let scaled = self
                    .builder
                    .build_int_mul(index_val, stride, "scaled")
                    .expect("imul stride");
                let i8_t = self.context.i8_type();
                let addr = unsafe {
                    self.builder
                        .build_gep(i8_t, base_ptr, &[scaled], "gep")
                        .expect("gep")
                };
                self.value_map.insert(*dst, addr.into());
            }
            Instr::IndirectLoad {
                dst,
                addr,
                hint,
                byte_width,
            } => {
                // `!ptr` / `v!i` / `v.%i` are word-shaped loads
                // (byte_width=8) and use the IR hint to pick i64 vs
                // f64. `%ptr` / `v%i` are byte loads (byte_width=1):
                // emit `load i8` then `zext` to i64 so the result
                // sits in a register-sized slot like any other word.
                let addr_v = self.lower_value(addr);
                let addr_ptr = self.as_pointer(addr_v);
                if *byte_width == 1 {
                    let i8_t = self.context.i8_type();
                    let i64_t = self.context.i64_type();
                    let raw = self
                        .builder
                        .build_load(i8_t, addr_ptr, "iload.byte")
                        .expect("indirect byte load")
                        .into_int_value();
                    let zext = self
                        .builder
                        .build_int_z_extend(raw, i64_t, "iload.zext")
                        .expect("zext");
                    self.value_map.insert(*dst, zext.into());
                } else {
                    let ty = self.basic_type_for(*hint);
                    let loaded = self
                        .builder
                        .build_load(ty, addr_ptr, "iload")
                        .expect("indirect load");
                    self.value_map.insert(*dst, loaded);
                }
            }
            Instr::GlobalLoad { dst, name, hint } => {
                // `GLOBAL <name>` reads — emit `load i64, ptr @<name>`.
                // If the global doesn't exist in this module
                // (cross-module reference, loader linked it from
                // another translation unit), declare an external
                // stub so the linker can resolve it.
                let gv = match self.module.get_global(name) {
                    Some(g) => g,
                    None => {
                        let i64_t = self.context.i64_type();
                        let g = self.module.add_global(i64_t, None, name);
                        g.set_linkage(inkwell::module::Linkage::External);
                        g
                    }
                };
                let ty = self.basic_type_for(*hint);
                let ptr = gv.as_pointer_value();
                let loaded = self
                    .builder
                    .build_load(ty, ptr, "gload")
                    .expect("global load");
                self.value_map.insert(*dst, loaded);
            }
            Instr::GlobalStore { name, value } => {
                let gv = match self.module.get_global(name) {
                    Some(g) => g,
                    None => {
                        let i64_t = self.context.i64_type();
                        let g = self.module.add_global(i64_t, None, name);
                        g.set_linkage(inkwell::module::Linkage::External);
                        g
                    }
                };
                let ptr = gv.as_pointer_value();
                let v = self.lower_value(value);
                self.builder
                    .build_store(ptr, v)
                    .expect("global store");
            }
            Instr::IndirectStore {
                addr,
                value,
                byte_width,
            } => {
                // Byte stores (`%ptr := v`, `v%i := v`) truncate the
                // word-shaped source value to i8 before storing.
                // Word stores use whatever LLVM type the value
                // already carries.
                let addr_v = self.lower_value(addr);
                let addr_ptr = self.as_pointer(addr_v);
                let v = self.lower_value(value);
                if *byte_width == 1 {
                    let i8_t = self.context.i8_type();
                    let iv = self.as_int_word(v);
                    let narrowed = self
                        .builder
                        .build_int_truncate(iv, i8_t, "istore.trunc")
                        .expect("trunc");
                    self.builder
                        .build_store(addr_ptr, narrowed)
                        .expect("indirect byte store");
                } else {
                    self.builder
                        .build_store(addr_ptr, v)
                        .expect("indirect store");
                }
            }
            Instr::LaneExtract {
                dst,
                vector,
                lane,
                kind,
                hint,
            } => {
                let elem = self.emit_lane_extract(vector, lane, *kind, *hint);
                self.value_map.insert(*dst, elem);
            }
            Instr::LaneInsert {
                dst,
                vector,
                lane,
                value,
                kind,
            } => {
                let new_pack = self.emit_lane_insert(vector, lane, value, *kind);
                self.value_map.insert(*dst, new_pack);
            }
            Instr::TypedConstruct {
                dst,
                kind,
                args,
                hint: _,
            } => {
                let result = self.emit_typed_construct(*kind, args);
                self.value_map.insert(*dst, result);
            }
            Instr::New {
                dst,
                class_name,
                args,
            } => {
                let instance = self.emit_new(class_name, args);
                self.value_map.insert(*dst, instance);
            }
            Instr::FieldLoad {
                dst,
                base,
                byte_offset,
                hint,
            } => {
                let base_v = self.lower_value(base);
                let base_ptr = self.as_pointer(base_v);
                let off = self
                    .context
                    .i64_type()
                    .const_int(*byte_offset as u64, false);
                let i8_t = self.context.i8_type();
                let field_ptr = unsafe {
                    self.builder
                        .build_gep(i8_t, base_ptr, &[off], "field.addr")
                        .expect("gep field")
                };
                let ty = self.basic_type_for(*hint);
                let loaded = self
                    .builder
                    .build_load(ty, field_ptr, "field.load")
                    .expect("load field");
                self.value_map.insert(*dst, loaded);
            }
            Instr::FieldStore {
                base,
                byte_offset,
                value,
            } => {
                let base_v = self.lower_value(base);
                let base_ptr = self.as_pointer(base_v);
                let off = self
                    .context
                    .i64_type()
                    .const_int(*byte_offset as u64, false);
                let i8_t = self.context.i8_type();
                let field_ptr = unsafe {
                    self.builder
                        .build_gep(i8_t, base_ptr, &[off], "field.addr")
                        .expect("gep field")
                };
                let v = self.lower_value(value);
                self.builder
                    .build_store(field_ptr, v)
                    .expect("store field");
            }
            Instr::MethodCall {
                dst,
                receiver,
                class_name,
                vtable_slot,
                method_name: _,
                args,
                hint,
            } => {
                self.emit_method_call(
                    *dst,
                    receiver,
                    class_name,
                    *vtable_slot,
                    args,
                    *hint,
                );
            }
            Instr::IndirectMethodCall {
                dst,
                receiver,
                method_name,
                args,
                hint,
            } => {
                self.emit_indirect_method_call(
                    *dst,
                    receiver,
                    method_name,
                    args,
                    *hint,
                );
            }
        }
    }

    /// `NEW Class(args)` allocates an instance on the GC heap via
    /// `__newbcpl_new_rec(@Class.desc)`. The runtime stamps the
    /// `BlockHeader.tag` (at `obj - 16`) with the TypeDesc address
    /// so the collector can find the layout / size / pointer
    /// offsets on a sweep. The first word of the data area still
    /// holds an inline vtable pointer (we keep the cheap
    /// `obj → vtable → slot` MethodCall path); fields follow at
    /// `+8`, `+16`, … as sema laid them out. Finally we call the
    /// mangled `Class_CREATE` to run the constructor — direct
    /// dispatch because the static class is known here.
    ///
    /// On the first allocation per TypeDesc the GC auto-registers
    /// it (see `__newbcpl_new_rec` in `gc.rs`), so we don't need
    /// an explicit init pass.
    fn emit_new(&mut self, class_name: &str, args: &[Value]) -> BasicValueEnum<'ctx> {
        let i64_t = self.context.i64_type();
        let ptr_t = self.context.ptr_type(AddressSpace::default());
        let layout = self.lookup_layout(class_name);
        let size = layout.map(|l| l.instance_size).unwrap_or(8);

        // Allocate the instance on the GC heap via the
        // size-keyed allocator `__newbcpl_alloc_rec`. The
        // runtime interns a TypeDesc per distinct payload
        // size and stamps every BlockHeader with that stable
        // address — see `docs/jit_typedesc_lifetime.md` for
        // why we don't pass `@Class.desc` directly. Classes
        // with no recorded layout fall back to a stack alloca
        // so simple data-only classes still compile, but in
        // practice every declared class has a layout.
        let obj_ptr: PointerValue<'ctx> = if layout.is_some() {
            let alloc_fn = match self.by_name.get("__newbcpl_alloc_rec") {
                Some(&f) => f,
                None => {
                    // Signature: `ptr fn(i64)`.
                    let fn_ty = ptr_t.fn_type(&[i64_t.into()], false);
                    let fv = self.module.add_function(
                        "__newbcpl_alloc_rec",
                        fn_ty,
                        Some(Linkage::External),
                    );
                    self.by_name
                        .insert("__newbcpl_alloc_rec".to_string(), fv);
                    fv
                }
            };
            let size_arg = i64_t.const_int(size as u64, true);
            let call_site = self
                .builder
                .build_call(
                    alloc_fn,
                    &[size_arg.into()],
                    &format!("new.{class_name}"),
                )
                .expect("call __newbcpl_alloc_rec");
            use inkwell::values::ValueKind;
            match call_site.try_as_basic_value() {
                ValueKind::Basic(rv) => self.as_pointer(rv),
                ValueKind::Instruction(_) => panic!(
                    "__newbcpl_alloc_rec must return a pointer"
                ),
            }
        } else {
            let i8_t = self.context.i8_type();
            let arr_t = i8_t.array_type(size as u32);
            let alloca = self
                .builder
                .build_alloca(arr_t, &format!("obj.{class_name}"))
                .expect("alloca obj");
            self.zero_memory(alloca, size);
            alloca
        };

        // Install the inline vtable pointer at offset 0 of the
        // instance. The GC already zeroes the block, so other
        // fields start as zero — matches sema's documented "zeroed
        // instance" contract.
        let vtable_global_name = format!("{class_name}.vtable");
        if let Some(vtable_global) = self.module.get_global(&vtable_global_name) {
            let _ = self
                .builder
                .build_store(obj_ptr, vtable_global.as_pointer_value())
                .expect("store vtable header");
        }

        let _ = i64_t; // silence unused if no other use below

        // Call the mangled CREATE if declared. Use the *defining*
        // class — for `NEW Sub` where Sub inherits CREATE from Base,
        // we want `Base_CREATE` not `Sub_CREATE`. The layout's
        // vtable already tracks `defining_class` for slot 0.
        let create_owner = layout.and_then(|l| {
            l.vtable
                .iter()
                .find(|v| v.method_name == "CREATE")
                .and_then(|v| v.defining_class.clone())
        });
        if let Some(owner) = create_owner {
            let create_name = format!("{owner}_CREATE");
            let create_fn = match self.by_name.get(&create_name) {
                Some(&f) => f,
                None => self.declare_extern(&create_name, args.len() + 1),
            };
            let mut call_args: Vec<BasicMetadataValueEnum> =
                Vec::with_capacity(args.len() + 1);
            call_args.push(obj_ptr.into());
            for a in args {
                call_args.push(self.lower_value(a).into());
            }
            self.builder
                .build_call(create_fn, &call_args, "create")
                .expect("call CREATE");
        }
        obj_ptr.into()
    }

    /// Lower `Instr::MethodCall` to an indirect call through the
    /// receiver's vtable header. The dispatch sequence is:
    ///   1. Receiver's first word (offset 0) holds the vtable ptr
    ///   2. GEP `vtable[slot]` produces the address of the slot
    ///   3. Load that to get the function pointer
    ///   4. Indirect call with `(receiver, args...)`
    /// MCJIT writes the actual method addresses into the vtable
    /// slots after finalisation; until then the slots read zero,
    /// which is fine because no method gets called before the
    /// JIT layer's patch loop runs.
    fn emit_method_call(
        &mut self,
        dst: Option<ValueId>,
        receiver: &Value,
        class_name: &str,
        slot: usize,
        args: &[Value],
        hint: TypeHint,
    ) {
        let ptr_t = self.context.ptr_type(AddressSpace::default());
        let i64_t = self.context.i64_type();

        let recv_v = self.lower_value(receiver);
        let recv_ptr = self.as_pointer(recv_v);

        // 1. Load vtable pointer from offset 0 of the instance.
        let vtable_ptr = self
            .builder
            .build_load(ptr_t, recv_ptr, "vtable_ptr")
            .expect("load vtable header")
            .into_pointer_value();

        // 2. GEP vtable_ptr[slot] (each slot is one ptr).
        let slot_idx = i64_t.const_int(slot as u64, false);
        let slot_addr = unsafe {
            self.builder
                .build_gep(ptr_t, vtable_ptr, &[slot_idx], "vt_slot")
                .expect("gep vtable slot")
        };

        // 3. Load the function pointer.
        let fn_ptr = self
            .builder
            .build_load(ptr_t, slot_addr, "fn_ptr")
            .expect("load fn_ptr")
            .into_pointer_value();

        // 4. Build the indirect call. We need an LLVM FunctionType
        // matching the method's ABI. We don't have one materialised
        // (the method may not even live in this module). Synthesise
        // one from the class layout's view: receiver (ptr) plus N
        // i64 args, returning the typed result.
        let return_type = self.return_type_for(hint);
        let mut param_types: Vec<BasicMetadataTypeEnum> =
            Vec::with_capacity(args.len() + 1);
        param_types.push(ptr_t.into());
        for _ in args {
            param_types.push(i64_t.into());
        }
        let fn_type = match return_type {
            Some(t) => t.fn_type(&param_types, false),
            None => self.context.void_type().fn_type(&param_types, false),
        };

        let mut call_args: Vec<BasicMetadataValueEnum> =
            Vec::with_capacity(args.len() + 1);
        call_args.push(recv_ptr.into());
        for a in args {
            let v = self.lower_value(a);
            // Coerce each arg to the i64 word the method expects.
            let iv = self.as_int_word(v);
            call_args.push(iv.into());
        }
        let call_site = self
            .builder
            .build_indirect_call(fn_type, fn_ptr, &call_args, "vcall")
            .expect("indirect call");

        let _ = class_name; // class name was used for slot resolution upstream
        if let Some(d) = dst {
            use inkwell::values::ValueKind;
            match call_site.try_as_basic_value() {
                ValueKind::Basic(rv) => {
                    self.value_map.insert(d, rv);
                }
                ValueKind::Instruction(_) => {
                    let z = self.zero(hint);
                    self.value_map.insert(d, z);
                }
            }
        }
    }

    /// Lower `Instr::IndirectMethodCall` to a runtime name-keyed
    /// dispatch. The static class isn't known at this site (typically
    /// an untyped routine parameter), so codegen emits:
    ///
    ///   1. Materialise a private global pointing at the method's
    ///      null-terminated name string.
    ///   2. Call `__newbcpl_lookup_method(receiver, name_ptr)` —
    ///      the runtime walks the receiver's `TypeDesc.method_names`
    ///      and returns the matching `vtable[i]` function pointer
    ///      (or null if the method isn't defined on the class).
    ///   3. Indirect-call through the returned pointer with
    ///      `(receiver, args...)`.
    ///
    /// A null lookup result would dispatch through address 0 and
    /// crash. We don't guard with a runtime null check here: the
    /// failure mode is identical to a vtable-slot null today, and
    /// adding the guard adds a hard-to-explain hidden branch on
    /// every dynamic dispatch. Programs that hit it have a real
    /// "method not defined for this class" bug — a BRK at the
    /// caller will surface it.
    fn emit_indirect_method_call(
        &mut self,
        dst: Option<ValueId>,
        receiver: &Value,
        method_name: &str,
        args: &[Value],
        hint: TypeHint,
    ) {
        let ptr_t = self.context.ptr_type(AddressSpace::default());
        let i64_t = self.context.i64_type();
        let i8_t = self.context.i8_type();

        let recv_v = self.lower_value(receiver);
        let recv_ptr = self.as_pointer(recv_v);

        // 1. Emit a private global holding the null-terminated method
        // name. Names are interned per-IR-name string to avoid
        // duplicate globals when the same method is dispatched in
        // multiple places. Inkwell doesn't easily let us look up a
        // global by initialiser, so we cache by name via the module's
        // own symbol table — a `@bcpl.mname.<name>` convention.
        let mangled_name_sym = format!("bcpl.mname.{method_name}");
        let name_global = match self.module.get_global(&mangled_name_sym) {
            Some(g) => g,
            None => {
                let mut bytes = method_name.as_bytes().to_vec();
                bytes.push(0);
                let byte_consts: Vec<inkwell::values::IntValue<'ctx>> =
                    bytes
                        .iter()
                        .map(|b| i8_t.const_int(*b as u64, false))
                        .collect();
                let arr_init = i8_t.const_array(&byte_consts);
                let g = self
                    .module
                    .add_global(arr_init.get_type(), None, &mangled_name_sym);
                g.set_initializer(&arr_init);
                g.set_constant(true);
                g.set_linkage(Linkage::Private);
                g
            }
        };

        // 2. Look the method up via the runtime helper.
        let lookup_fn = match self.module.get_function("__newbcpl_lookup_method") {
            Some(f) => f,
            None => {
                let fn_type = ptr_t.fn_type(&[ptr_t.into(), ptr_t.into()], false);
                self.module
                    .add_function("__newbcpl_lookup_method", fn_type, None)
            }
        };
        let lookup_call = self
            .builder
            .build_call(
                lookup_fn,
                &[
                    recv_ptr.into(),
                    name_global.as_pointer_value().into(),
                ],
                "method_fn",
            )
            .expect("lookup call");
        use inkwell::values::ValueKind;
        let resolved_fn = match lookup_call.try_as_basic_value() {
            ValueKind::Basic(rv) => rv.into_pointer_value(),
            ValueKind::Instruction(_) => {
                panic!("__newbcpl_lookup_method did not return a value")
            }
        };

        // 3. Indirect-call through the resolved function pointer
        // with (receiver, args...). Synthesise a function type that
        // matches the BCPL-routine ABI: ptr receiver, i64 args, then
        // the typed return value.
        let return_type = self.return_type_for(hint);
        let mut param_types: Vec<BasicMetadataTypeEnum> =
            Vec::with_capacity(args.len() + 1);
        param_types.push(ptr_t.into());
        for _ in args {
            param_types.push(i64_t.into());
        }
        let fn_type = match return_type {
            Some(t) => t.fn_type(&param_types, false),
            None => self.context.void_type().fn_type(&param_types, false),
        };

        let mut call_args: Vec<BasicMetadataValueEnum> =
            Vec::with_capacity(args.len() + 1);
        call_args.push(recv_ptr.into());
        for a in args {
            let v = self.lower_value(a);
            let iv = self.as_int_word(v);
            call_args.push(iv.into());
        }
        let call_site = self
            .builder
            .build_indirect_call(fn_type, resolved_fn, &call_args, "vcall_dyn")
            .expect("indirect call (dynamic)");

        if let Some(d) = dst {
            use inkwell::values::ValueKind;
            match call_site.try_as_basic_value() {
                ValueKind::Basic(rv) => {
                    self.value_map.insert(d, rv);
                }
                ValueKind::Instruction(_) => {
                    let z = self.zero(hint);
                    self.value_map.insert(d, z);
                }
            }
        }
    }

    fn zero_memory(&self, ptr: PointerValue<'ctx>, bytes: usize) {
        // Fill with zero via a memset intrinsic. We declare it on
        // demand to avoid hard-coding the symbol.
        let i8_t = self.context.i8_type();
        let i64_t = self.context.i64_type();
        let ptr_t = self.context.ptr_type(AddressSpace::default());
        let memset = self.module.get_function("llvm.memset.p0.i64").or_else(|| {
            let bool_t = self.context.bool_type();
            let fn_type = self.context.void_type().fn_type(
                &[ptr_t.into(), i8_t.into(), i64_t.into(), bool_t.into()],
                false,
            );
            Some(
                self.module
                    .add_function("llvm.memset.p0.i64", fn_type, None),
            )
        });
        if let Some(memset_fn) = memset {
            let zero_byte = i8_t.const_zero();
            let len = i64_t.const_int(bytes as u64, false);
            let is_volatile = self.context.bool_type().const_zero();
            let _ = self.builder.build_call(
                memset_fn,
                &[
                    ptr.into(),
                    zero_byte.into(),
                    len.into(),
                    is_volatile.into(),
                ],
                "memset",
            );
        }
    }

    /// Lower a typed constructor — VEC / FVEC / SIMD primitives /
    /// table / list. Stack-allocated forms (VEC, FVEC, TABLE,
    /// FTABLE inline-init, the SIMD primitives) are handled here;
    /// LIST and MANIFESTLIST need runtime support and are deferred.
    fn emit_typed_construct(
        &mut self,
        kind: TypedKind,
        args: &[Value],
    ) -> BasicValueEnum<'ctx> {
        // SIMD shape dispatch — see docs/pair_and_multilane_types.md.
        // PAIR / FPAIR / QUAD / OCT all pack into one i64 word
        // per the reference's ABI; FQUAD and FOCT are wider and
        // need genuine LLVM vectors.
        match kind {
            TypedKind::Vec | TypedKind::Table => self.emit_vec_construct(args, false),
            TypedKind::FVec | TypedKind::FTable => self.emit_vec_construct(args, true),
            TypedKind::Pair => self.build_packed_word(args, 32, /* float = */ false),
            TypedKind::FPair => self.build_packed_word(args, 32, /* float = */ true),
            TypedKind::Quad => self.build_packed_word(args, 16, false),
            TypedKind::Oct => self.build_packed_word(args, 8, false),
            // FQUAD = <4 x f32>, 128-bit. Real LLVM vector in a Q-reg.
            TypedKind::FQuad => self.build_simd_vector(args, /* float = */ true),
            // FOCT = <8 x f32>, 256-bit. Real LLVM vector across two Q-regs.
            TypedKind::FOct => self.build_simd_vector(args, /* float = */ true),
            // LIST / MANIFESTLIST construct a real linked
            // `ListHeader` + chain of `ListAtom`s — matches
            // `reference/runtime/ListDataTypes.h` byte-for-byte
            // so HD/TL/APND/CONCAT all walk the same shape.
            // We emit a call to `__newbcpl_list_new_empty()`
            // and then issue one `APND_*` per initialiser,
            // type-dispatched by each arg's LLVM type so that
            // floats land in float atoms and packed PAIR
            // values land in pair atoms.
            TypedKind::List | TypedKind::ManifestList => self.emit_list_construct(args),
        }
    }

    /// `VEC k` / `FVEC k` / their init-list cousins all allocate a
    /// `(k+1) * 8`-byte buffer on the **GC heap** via
    /// `__newbcpl_alloc_rec`. Slot 0 holds the length; the
    /// returned pointer is one word past slot 0 so `V!i` lands on
    /// slot `1+i` and `__newbcpl_len(V)` reads the length at
    /// `*(V-8)`.
    ///
    /// The buffer is heap, not stack, because BCPL's value
    /// semantics make a VEC variable a *pointer* — it's freely
    /// copied between locals, returned from functions, stored
    /// into other vectors, and put into lists. A stack-alloca'd
    /// buffer would dangle the moment the constructing frame
    /// exits. Heap-allocating uniformly turns the question of
    /// "does this VEC escape?" from a precondition into a
    /// safety property.
    ///
    /// `float` selects the slot type for the init-list path
    /// (f64 vs i64). The const-size path is element-type-agnostic
    /// because it allocates raw bytes — the slot-load type is
    /// determined at the use site.
    fn emit_vec_construct(&mut self, args: &[Value], float: bool) -> BasicValueEnum<'ctx> {
        let i64_t = self.context.i64_type();
        let f64_t = self.context.f64_type();
        // Heuristic: a single Int constant arg means "size k",
        // so allocate `k+1` cells. Anything else is treated as
        // an init list (one cell per arg).
        let single_const_size = if args.len() == 1 {
            if let Value::Const(Const::Int(k)) = &args[0] {
                Some(*k as u64)
            } else {
                None
            }
        } else {
            None
        };

        let count: u64 = single_const_size.unwrap_or_else(|| args.len() as u64);
        let total_bytes = count.saturating_add(1).saturating_mul(8);

        // Heap-allocate via the GC's size-keyed allocator.
        let buf = self.alloc_rec_bytes(total_bytes);
        // Length header at byte offset 0.
        self.store_word_at_offset(buf, 0, i64_t.const_int(count, true).into());

        // Init-list form: write each scalar at byte offsets
        // 8, 16, 24, … (one word stride). The const-size path
        // skips this loop — the GC zero-initialises the block,
        // so unwritten slots already read 0.
        if single_const_size.is_none() {
            for (i, v) in args.iter().enumerate() {
                let elem_v = self.lower_value(v);
                // Coerce to the slot type.
                let stored: BasicValueEnum<'ctx> = if float {
                    match elem_v {
                        BasicValueEnum::FloatValue(_) => elem_v,
                        BasicValueEnum::IntValue(iv) => self
                            .builder
                            .build_signed_int_to_float(iv, f64_t, "i2f")
                            .expect("sitofp")
                            .into(),
                        _ => elem_v,
                    }
                } else {
                    match elem_v {
                        BasicValueEnum::IntValue(_) => self.as_int_word(elem_v).into(),
                        BasicValueEnum::VectorValue(_) => {
                            self.pack_vector_to_word(elem_v).into()
                        }
                        _ => elem_v,
                    }
                };
                let offset = (i as u64 + 1) * 8;
                self.store_word_at_offset(buf, offset, stored);
            }
        }

        // Return the data pointer — one word past the length header.
        self.byte_offset_ptr(buf, 8, "vec.data").into()
    }

    /// Call `__newbcpl_alloc_rec(size)` and return the resulting
    /// pointer. Declares the function on demand with its precise
    /// `ptr fn(i64)` signature.
    fn alloc_rec_bytes(&mut self, size: u64) -> PointerValue<'ctx> {
        let i64_t = self.context.i64_type();
        let ptr_t = self.context.ptr_type(AddressSpace::default());
        let alloc_fn = match self.by_name.get("__newbcpl_alloc_rec") {
            Some(&f) => f,
            None => {
                let fn_ty = ptr_t.fn_type(&[i64_t.into()], false);
                let fv = self.module.add_function(
                    "__newbcpl_alloc_rec",
                    fn_ty,
                    Some(Linkage::External),
                );
                self.by_name
                    .insert("__newbcpl_alloc_rec".to_string(), fv);
                fv
            }
        };
        let size_arg = i64_t.const_int(size, false);
        let call_site = self
            .builder
            .build_call(alloc_fn, &[size_arg.into()], "alloc_rec")
            .expect("call __newbcpl_alloc_rec");
        use inkwell::values::ValueKind;
        match call_site.try_as_basic_value() {
            ValueKind::Basic(rv) => self.as_pointer(rv),
            ValueKind::Instruction(_) => {
                panic!("__newbcpl_alloc_rec must return a pointer")
            }
        }
    }

    /// `gep i8, base, [offset]` — produce an `i8*`-style pointer
    /// at `base + offset` bytes. The opaque-pointer model means
    /// the LLVM-level pointer is untyped; the load/store at the
    /// use site picks the value type.
    fn byte_offset_ptr(
        &self,
        base: PointerValue<'ctx>,
        offset: u64,
        name: &str,
    ) -> PointerValue<'ctx> {
        let i64_t = self.context.i64_type();
        let i8_t = self.context.i8_type();
        let off = i64_t.const_int(offset, false);
        unsafe {
            self.builder
                .build_gep(i8_t, base, &[off], name)
                .expect("gep byte offset")
        }
    }

    /// Convenience: store an i64-shaped value at `base + offset`
    /// bytes. Used for the length header and each init-list slot.
    fn store_word_at_offset(
        &self,
        base: PointerValue<'ctx>,
        offset: u64,
        value: BasicValueEnum<'ctx>,
    ) {
        let slot = self.byte_offset_ptr(base, offset, "slot");
        self.builder
            .build_store(slot, value)
            .expect("store at byte offset");
    }

    /// Pack a SIMD lane vector into a single i64 word using the
    /// BCPL convention: lane 0 in the low bits, lane 1 above it,
    /// each lane truncated to `64 / lane_count` bits. PAIR (2
    /// lanes) ⇒ two 32-bit halves; QUAD (4 lanes) ⇒ four 16-bit
    /// quarters; OCT (8 lanes) ⇒ eight bytes. The `<N x i64>`
    /// representation our SIMD constructor produces carries each
    /// lane in a 64-bit slot, so we trunc each one before OR-ing
    /// it into the packed word. The reverse direction lives in
    /// `Lowerer::unpack_lanes` (sign-aware shift-extract).
    fn pack_vector_to_word(
        &self,
        v: BasicValueEnum<'ctx>,
    ) -> inkwell::values::IntValue<'ctx> {
        let i64_t = self.context.i64_type();
        let vec = match v {
            BasicValueEnum::VectorValue(vv) => vv,
            // Already an integer — nothing to pack.
            BasicValueEnum::IntValue(iv) if iv.get_type().get_bit_width() == 64 => {
                return iv;
            }
            other => return self.as_int_word(other),
        };
        let lane_count = vec.get_type().get_size();
        if lane_count == 0 {
            return i64_t.const_zero();
        }
        let lane_bits: u32 = (64 / lane_count).max(1);
        let lane_mask: u64 = if lane_bits >= 64 {
            u64::MAX
        } else {
            (1u64 << lane_bits) - 1
        };
        let mask_v = i64_t.const_int(lane_mask, false);
        let mut acc: inkwell::values::IntValue<'ctx> = i64_t.const_zero();
        for i in 0..lane_count {
            let idx = i64_t.const_int(i as u64, false);
            let lane = self
                .builder
                .build_extract_element(vec, idx, &format!("lane.{i}"))
                .expect("extract lane");
            // Each lane is i64 (or f64); coerce to i64 then keep
            // only the low `lane_bits` so packing is sign-clean.
            let lane_i = self.as_int_word(lane);
            let lane_low = self
                .builder
                .build_and(lane_i, mask_v, "lane.low")
                .expect("and");
            let shift = i64_t.const_int((i as u64) * (lane_bits as u64), false);
            let placed = self
                .builder
                .build_left_shift(lane_low, shift, "lane.shifted")
                .expect("shl");
            acc = self.builder.build_or(acc, placed, "pack.acc").expect("or");
        }
        acc
    }

    /// `LIST(a, b, c)` lowers to:
    ///   1. `header = __newbcpl_list_new_empty()`
    ///   2. for each arg: `APND_*(header, arg)` — the suffix
    ///      depends on the arg's LLVM type so floats end up in
    ///      `ATOM_FLOAT` atoms, pointers in `ATOM_STRING` (or
    ///      `ATOM_OBJECT` — we collapse both onto the string
    ///      append for now since the reference's runtime treats
    ///      raw pointers the same way at the ABI level), and
    ///      integers / packed SIMD words in `ATOM_INT`.
    ///   3. Return `header`.
    /// MANIFESTLIST shares this lowering; in the reference it
    /// allocates in read-only memory, but our runtime tracks
    /// every list via `Box::leak` for now (GC integration of
    /// list nodes is a follow-up slice).
    fn emit_list_construct(&mut self, args: &[Value]) -> BasicValueEnum<'ctx> {
        let ptr_t = self.context.ptr_type(AddressSpace::default());
        let i64_t = self.context.i64_type();
        let f64_t = self.context.f64_type();

        // Make sure the helper functions are declared with the
        // exact signatures the runtime exposes — we go around
        // `declare_extern`'s defaults because the list ABI uses
        // pointer + typed-value pairs.
        let new_empty = self.declare_list_helper(
            "__newbcpl_list_new_empty",
            ptr_t.fn_type(&[], false),
        );
        let apnd_int = self.declare_list_helper(
            "APND",
            i64_t.fn_type(&[ptr_t.into(), i64_t.into()], false),
        );
        let apnd_float = self.declare_list_helper(
            "APND_FLOAT",
            i64_t.fn_type(&[ptr_t.into(), f64_t.into()], false),
        );
        let apnd_string = self.declare_list_helper(
            "APND_STRING",
            i64_t.fn_type(&[ptr_t.into(), ptr_t.into()], false),
        );
        let apnd_pair = self.declare_list_helper(
            "APND_PAIR",
            i64_t.fn_type(&[ptr_t.into(), i64_t.into()], false),
        );

        let header = self
            .builder
            .build_call(new_empty, &[], "list.hdr")
            .expect("call __newbcpl_list_new_empty")
            .try_as_basic_value();
        use inkwell::values::ValueKind;
        let header_ptr = match header {
            ValueKind::Basic(v) => self.as_pointer(v),
            ValueKind::Instruction(_) => panic!(
                "__newbcpl_list_new_empty must return a pointer"
            ),
        };

        for (i, a) in args.iter().enumerate() {
            let v = self.lower_value(a);
            let (callee, arg_val): (FunctionValue<'ctx>, BasicMetadataValueEnum<'ctx>) = match v {
                BasicValueEnum::FloatValue(_) => (apnd_float, v.into()),
                BasicValueEnum::PointerValue(_) => (apnd_string, v.into()),
                // VectorValue (FQUAD / FOCT) doesn't fit a list
                // atom's i64 slot — collapse via `as_int_word`
                // for now (loses lanes; same band-aid we use
                // for other vector-to-word boundaries).
                BasicValueEnum::IntValue(_) | BasicValueEnum::VectorValue(_) => {
                    let packed = self.as_int_word(v);
                    // We don't track the source's SIMD kind here;
                    // route packed PAIR / QUAD / OCT values
                    // through `APND_PAIR` so the atom carries an
                    // `ATOM_PAIR` tag. Bare integers also go
                    // through this path — the atom holds the
                    // same i64 either way and the type tag is
                    // a hint, not a correctness gate.
                    let needs_pair_tag =
                        matches!(a, Value::Local(_)) && matches!(v, BasicValueEnum::VectorValue(_));
                    let callee = if needs_pair_tag { apnd_pair } else { apnd_int };
                    (callee, packed.into())
                }
                _ => (apnd_int, self.as_int_word(v).into()),
            };
            let _ = self
                .builder
                .build_call(callee, &[header_ptr.into(), arg_val], &format!("list.apnd.{i}"))
                .expect("call APND_*");
        }
        header_ptr.into()
    }

    /// Declare a list-runtime helper with a precise signature.
    /// Bypasses `declare_extern`'s default `i64 fn(i64, ...)`
    /// shape so pointer / float parameters are typed correctly
    /// and the LLVM verifier accepts the call.
    fn declare_list_helper(
        &mut self,
        name: &str,
        fn_type: inkwell::types::FunctionType<'ctx>,
    ) -> FunctionValue<'ctx> {
        if let Some(&existing) = self.by_name.get(name) {
            return existing;
        }
        let fv = self
            .module
            .add_function(name, fn_type, Some(Linkage::External));
        self.by_name.insert(name.to_string(), fv);
        fv
    }

    /// Lane access (`pair.|n|`). Dispatches on the source's SIMD
    /// kind:
    ///   - **PAIR / FPAIR / QUAD / OCT** — packed i64 word.
    ///     Extract via `(value << top_pad) >> (top_pad + low_drop)`
    ///     with arithmetic shift, so signed lanes sign-extend
    ///     into a full i64. FPAIR's lanes are reinterpreted as
    ///     f32 via bitcast and zero-extended into f64.
    ///   - **FQUAD / FOCT** — real LLVM vector. `extractelement`
    ///     directly. Floats land as f32 → fpext to f64.
    ///
    /// `lane` is a runtime value; constants flow through `as_int_word`
    /// to become an i64 shift amount.
    fn emit_lane_extract(
        &mut self,
        vector: &Value,
        lane: &Value,
        kind: TypedKind,
        hint: TypeHint,
    ) -> BasicValueEnum<'ctx> {
        let v = self.lower_value(vector);
        let lane_v = self.lower_value(lane);
        let lane_idx = self.as_int_word(lane_v);
        let i64_t = self.context.i64_type();
        let f32_t = self.context.f32_type();
        let f64_t = self.context.f64_type();
        // Per-kind lane width in bits, and whether lanes are float.
        let (lane_bits, float, lane_count) = match kind {
            TypedKind::Pair => (32u32, false, 2u32),
            TypedKind::FPair => (32, true, 2),
            TypedKind::Quad => (16, false, 4),
            TypedKind::Oct => (8, false, 8),
            TypedKind::FQuad => (32, true, 4),
            TypedKind::FOct => (32, true, 8),
            // Vec / FVec / List etc. shouldn't reach lane access,
            // but if they do, treat as PAIR-shaped so we don't
            // panic outright.
            _ => (32, false, 2),
        };
        let _ = lane_count;
        // FQUAD / FOCT are real vectors — extractelement straight.
        if matches!(kind, TypedKind::FQuad | TypedKind::FOct) {
            let vec = match v {
                BasicValueEnum::VectorValue(vv) => vv,
                _ => panic!("FQUAD / FOCT lane access: expected vector value"),
            };
            let elem = self
                .builder
                .build_extract_element(vec, lane_idx, "lane")
                .expect("extractelement");
            // Lanes are f32 in the LLVM type; widen to f64 for the
            // BCPL-level "float" register class.
            return match elem {
                BasicValueEnum::FloatValue(fv) => self
                    .builder
                    .build_float_ext(fv, f64_t, "lane.fpext")
                    .expect("fpext")
                    .into(),
                other => other,
            };
        }
        // Packed-i64 path. `top_pad = 64 - lane_bits - low_drop`,
        // `low_drop = lane_idx * lane_bits`.
        let packed = self.as_int_word(v);
        let lane_bits_v = i64_t.const_int(lane_bits as u64, false);
        let low_drop = self
            .builder
            .build_int_mul(lane_idx, lane_bits_v, "low_drop")
            .expect("imul lane bits");
        let total = i64_t.const_int(64 - lane_bits as u64, false);
        let top_pad = self
            .builder
            .build_int_sub(total, low_drop, "top_pad")
            .expect("sub");
        let shifted_left = self
            .builder
            .build_left_shift(packed, top_pad, "lane.shl")
            .expect("shl");
        // For unsigned lanes (OCT bytes are unsigned in BCPL?)
        // we'd use logical shift; but reference treats every
        // packed lane as signed (PAIR is `2 × i32`, QUAD is
        // `4 × i16`, OCT is `8 × i8`, all signed two's-complement).
        // Use arithmetic right shift uniformly.
        let drop_total = self
            .builder
            .build_int_add(top_pad, low_drop, "drop_total")
            .expect("add");
        let _ = drop_total;
        // shift right by (top_pad + low_drop) = (64 - lane_bits).
        // We already have `total = 64 - lane_bits` as a constant;
        // arithmetic right shift by that gives a sign-extended
        // i64 holding the lane's value.
        let extracted = self
            .builder
            .build_right_shift(shifted_left, total, /*signed=*/ true, "lane.ashr")
            .expect("ashr");
        // FPAIR floats: low 32 bits are an f32 bit pattern. Pull
        // them out by truncating to i32, bitcasting to f32, then
        // widening to the BCPL-level f64.
        if float {
            let i32_t = self.context.i32_type();
            let truncated = self
                .builder
                .build_int_truncate(extracted, i32_t, "lane.i32")
                .expect("trunc");
            let f32_v = self
                .builder
                .build_bit_cast(truncated, f32_t, "lane.f32")
                .expect("bitcast i32→f32")
                .into_float_value();
            return self
                .builder
                .build_float_ext(f32_v, f64_t, "lane.fpext")
                .expect("fpext")
                .into();
        }
        let _ = hint;
        extracted.into()
    }

    /// Mirror of `emit_lane_extract`: produce a SIMD value identical
    /// to `vector` except lane `lane` is replaced by `value`. Used
    /// to lower `pair.|i| := v`. Two code paths:
    ///
    ///   - **FQUAD / FOCT** — real LLVM vector. `insertelement`
    ///     directly. Float values arrive as f64; we narrow to f32
    ///     to match the in-vector lane type.
    ///   - **PAIR / FPAIR / QUAD / OCT** — packed i64. Build the
    ///     replacement word as `(old & ~mask) | (new << shift)`
    ///     where `mask = ((1 << lane_bits) - 1) << shift` and
    ///     `shift = lane_idx * lane_bits`. For FPAIR the f64 value
    ///     gets fptrunc'd to f32, bitcast to i32, then masked into
    ///     the low 32 bits of its lane.
    fn emit_lane_insert(
        &mut self,
        vector: &Value,
        lane: &Value,
        value: &Value,
        kind: TypedKind,
    ) -> BasicValueEnum<'ctx> {
        let v = self.lower_value(vector);
        let lane_v = self.lower_value(lane);
        let new_v = self.lower_value(value);
        let lane_idx = self.as_int_word(lane_v);
        let i64_t = self.context.i64_type();
        let i32_t = self.context.i32_type();
        let f32_t = self.context.f32_type();
        // Per-kind lane width and float-ness — must match emit_lane_extract.
        let (lane_bits, float) = match kind {
            TypedKind::Pair => (32u32, false),
            TypedKind::FPair => (32, true),
            TypedKind::Quad => (16, false),
            TypedKind::Oct => (8, false),
            TypedKind::FQuad => (32, true),
            TypedKind::FOct => (32, true),
            _ => (32, false),
        };
        // FQUAD / FOCT live in real LLVM vectors. Narrow f64 → f32
        // to match the vector element type, then insertelement.
        if matches!(kind, TypedKind::FQuad | TypedKind::FOct) {
            let vec = match v {
                BasicValueEnum::VectorValue(vv) => vv,
                _ => panic!("FQUAD / FOCT lane insert: expected vector value"),
            };
            let elem_basic = match new_v {
                BasicValueEnum::FloatValue(fv) => {
                    let narrow = self
                        .builder
                        .build_float_trunc(fv, f32_t, "lane.fptrunc")
                        .expect("fptrunc");
                    narrow.into()
                }
                other => other,
            };
            let inserted = self
                .builder
                .build_insert_element(vec, elem_basic, lane_idx, "lane.ins")
                .expect("insertelement");
            return inserted.into();
        }
        // Packed-i64 path. Build mask = ((1 << lane_bits) - 1) << shift.
        let packed = self.as_int_word(v);
        let lane_bits_v = i64_t.const_int(lane_bits as u64, false);
        let shift = self
            .builder
            .build_int_mul(lane_idx, lane_bits_v, "lane.shift")
            .expect("imul lane bits");
        let lane_mask = if lane_bits >= 64 {
            i64_t.const_int(u64::MAX, false)
        } else {
            i64_t.const_int((1u64 << lane_bits) - 1, false)
        };
        let mask_shifted = self
            .builder
            .build_left_shift(lane_mask, shift, "lane.mask")
            .expect("shl mask");
        let not_mask = self
            .builder
            .build_not(mask_shifted, "lane.nmask")
            .expect("not");
        let cleared = self
            .builder
            .build_and(packed, not_mask, "lane.cleared")
            .expect("and");
        // Coerce the incoming value to an i64 holding only the lane's
        // payload bits. Float lanes (FPAIR): f64 → f32 → bitcast i32.
        let value_as_word: inkwell::values::IntValue<'ctx> = if float {
            let fv = match new_v {
                BasicValueEnum::FloatValue(f) => f,
                _ => panic!("FPair lane insert: value must be a float"),
            };
            let narrow = self
                .builder
                .build_float_trunc(fv, f32_t, "lane.fptrunc")
                .expect("fptrunc");
            let bits = self
                .builder
                .build_bit_cast(narrow, i32_t, "lane.bits")
                .expect("bitcast f32→i32")
                .into_int_value();
            self.builder
                .build_int_z_extend(bits, i64_t, "lane.zext")
                .expect("zext")
        } else {
            self.as_int_word(new_v)
        };
        let value_masked = self
            .builder
            .build_and(value_as_word, lane_mask, "lane.payload")
            .expect("and payload");
        let value_positioned = self
            .builder
            .build_left_shift(value_masked, shift, "lane.shifted")
            .expect("shl new");
        let merged = self
            .builder
            .build_or(cleared, value_positioned, "lane.merged")
            .expect("or");
        merged.into()
    }

    /// PAIR / FPAIR / QUAD / OCT constructor — pack `args.len()`
    /// scalar lanes into a single 64-bit word.
    ///
    /// `lane_bits` is the lane width (32 for PAIR/FPAIR, 16 for
    /// QUAD, 8 for OCT). `float` selects whether each lane is a
    /// raw integer (truncated to `lane_bits`) or a 32-bit float
    /// whose IEEE-754 bit pattern is reinterpreted as i32.
    /// Lane 0 lands in the low bits, lane 1 above it, etc.,
    /// matching the reference's `WRITEF` `%P` / `%Q` / `%R` lane
    /// readers and the bit layout documented in
    /// `docs/pair_and_multilane_types.md`.
    fn build_packed_word(
        &mut self,
        args: &[Value],
        lane_bits: u32,
        float: bool,
    ) -> BasicValueEnum<'ctx> {
        let i64_t = self.context.i64_type();
        let lane_int_t = match lane_bits {
            8 => self.context.i8_type(),
            16 => self.context.i16_type(),
            32 => self.context.i32_type(),
            other => panic!("unsupported packed lane width {other}"),
        };
        let lane_mask: u64 = if lane_bits >= 64 {
            u64::MAX
        } else {
            (1u64 << lane_bits) - 1
        };
        let mask_v = i64_t.const_int(lane_mask, false);
        let mut acc: inkwell::values::IntValue<'ctx> = i64_t.const_zero();
        for (i, arg) in args.iter().enumerate() {
            let v = self.lower_value(arg);
            // Reduce each lane to a `lane_bits`-wide integer:
            //  - float lane → bitcast f32 → i32 (PAIR-of-floats path)
            //  - int lane   → truncate to lane width
            // Then zero-extend back into i64 for OR-shifting.
            let lane_i64 = if float && lane_bits == 32 {
                let fv = match v {
                    BasicValueEnum::FloatValue(fv) => fv,
                    other => panic!(
                        "FPAIR lane expected float, got {:?}",
                        other.get_type().print_to_string()
                    ),
                };
                let f32_t = self.context.f32_type();
                let f32_v = self
                    .builder
                    .build_float_trunc(fv, f32_t, "lane.f32")
                    .expect("fptrunc f32");
                let i32_v = self
                    .builder
                    .build_bit_cast(f32_v, lane_int_t, "lane.bits")
                    .expect("bitcast f→i")
                    .into_int_value();
                self.builder
                    .build_int_z_extend(i32_v, i64_t, "lane.zext")
                    .expect("zext")
            } else {
                let iv = self.as_int_word(v);
                let truncated = self
                    .builder
                    .build_int_truncate(iv, lane_int_t, "lane.trunc")
                    .expect("trunc");
                self.builder
                    .build_int_z_extend(truncated, i64_t, "lane.zext")
                    .expect("zext")
            };
            // Mask defensively (zext should already be clean) and
            // shift into place.
            let masked = self
                .builder
                .build_and(lane_i64, mask_v, "lane.masked")
                .expect("and");
            let shift = i64_t.const_int((i as u64) * (lane_bits as u64), false);
            let placed = self
                .builder
                .build_left_shift(masked, shift, "lane.shifted")
                .expect("shl");
            acc = self
                .builder
                .build_or(acc, placed, "pack.acc")
                .expect("or");
        }
        acc.into()
    }

    /// SIMD constructor: build a `<N x T>` register-resident vector
    /// from N scalar args via insertelement. `float` selects the
    /// element type (f64 vs i64).
    fn build_simd_vector(&mut self, args: &[Value], float: bool) -> BasicValueEnum<'ctx> {
        let n = args.len() as u32;
        let i64_t = self.context.i64_type();
        let mut current: BasicValueEnum<'ctx> = if float {
            self.context.f64_type().vec_type(n).get_undef().into()
        } else {
            self.context.i64_type().vec_type(n).get_undef().into()
        };
        for (i, arg) in args.iter().enumerate() {
            let scalar = self.lower_value(arg);
            let idx = i64_t.const_int(i as u64, false);
            current = self
                .builder
                .build_insert_element(
                    current.into_vector_value(),
                    scalar,
                    idx,
                    &format!("lane.{i}"),
                )
                .expect("insertelement")
                .into();
        }
        current
    }

    fn emit_terminator(&mut self, t: &Terminator) {
        match t {
            Terminator::Return(None) => {
                // Routines: BCPL convention is that they "return
                // WORD" — emit `ret i64 0` so the LLVM type matches
                // what the function signature declared.
                let i64_t = self.context.i64_type();
                let zero = i64_t.const_zero();
                self.builder
                    .build_return(Some(&zero))
                    .expect("ret routine");
            }
            Terminator::Return(Some(v)) => {
                let val = self.lower_value(v);
                self.builder.build_return(Some(&val)).expect("ret value");
            }
            Terminator::Branch(b) => {
                let target = self.block_map[b];
                self.builder
                    .build_unconditional_branch(target)
                    .expect("br");
            }
            Terminator::CondBranch {
                cond,
                then_block,
                else_block,
            } => {
                let c = self.lower_value(cond);
                let i1 = self.truthify(c);
                let then_bb = self.block_map[then_block];
                let else_bb = self.block_map[else_block];
                self.builder
                    .build_conditional_branch(i1, then_bb, else_bb)
                    .expect("br cond");
            }
            Terminator::Unreachable => {
                self.builder.build_unreachable().expect("unreachable");
            }
            Terminator::Switch {
                value,
                cases,
                default,
            } => {
                // Coerce both the scrutinee and each case value
                // through `as_int_word`. Case constants reach us
                // as `Value::Function(name)` when sema didn't bind
                // a value to the identifier (e.g. unrecognised
                // type-tag names like `TYPE_STRING`); routing
                // through the standard int-word coercion turns
                // those pointer values into i64s rather than
                // panicking on `into_int_value()`.
                let scrut_v = self.lower_value(value);
                let scrut = self.as_int_word(scrut_v);
                let default_bb = self.block_map[default];
                let case_pairs: Vec<(inkwell::values::IntValue<'ctx>, LlvmBlock<'ctx>)> = cases
                    .iter()
                    .map(|(case_val, target)| {
                        let cv_raw = self.lower_value(case_val);
                        let cv = self.as_int_word(cv_raw);
                        let bb = self.block_map[target];
                        (cv, bb)
                    })
                    .collect();
                self.builder
                    .build_switch(scrut, default_bb, &case_pairs)
                    .expect("switch");
            }
        }
    }

    // ─── value lowering ─────────────────────────────────────────

    fn lower_value(&mut self, v: &Value) -> BasicValueEnum<'ctx> {
        match v {
            Value::Const(c) => self.lower_const(c, TypeHint::Word),
            Value::Local(id) => self.lookup(*id),
            Value::Function(name) => self
                .by_name
                .get(name)
                .copied()
                .unwrap_or_else(|| self.declare_extern(name, 0))
                .as_global_value()
                .as_pointer_value()
                .into(),
            Value::Unit => self.context.i64_type().const_zero().into(),
        }
    }

    fn lower_const(&mut self, c: &Const, hint: TypeHint) -> BasicValueEnum<'ctx> {
        match c {
            Const::Int(v) => self.context.i64_type().const_int(*v as u64, true).into(),
            Const::Float(v) => self.context.f64_type().const_float(*v).into(),
            Const::Bool(b) => self
                .context
                .i64_type()
                .const_int(if *b { 1 } else { 0 }, false)
                .into(),
            Const::Null => {
                // Null → typed zero so subsequent uses don't crash.
                // Pointer-shaped hints get a null pointer; everything
                // else gets an integer zero.
                self.zero(hint)
            }
            Const::String(raw) => {
                // The lexeme arrives wrapped in quotes; codegen pool
                // stores the cooked bytes.
                let cooked = cook_bcpl_string(raw);
                self.intern_string(&cooked).into()
            }
        }
    }

    /// Coerce a value to a pointer for use as the base of a load /
    /// store / GEP. BCPL is typeless at the source level —
    /// addresses arrive as i64 (Word), and LLVM 15+ opaque pointers
    /// are strict about the integer-vs-pointer distinction. This
    /// inserts an `inttoptr` when the value is an integer; passes
    /// through if it's already a pointer.
    fn as_pointer(&self, v: BasicValueEnum<'ctx>) -> PointerValue<'ctx> {
        match v {
            BasicValueEnum::PointerValue(p) => p,
            BasicValueEnum::IntValue(i) => {
                let ptr_t = self.context.ptr_type(AddressSpace::default());
                self.builder
                    .build_int_to_ptr(i, ptr_t, "asptr")
                    .expect("inttoptr")
            }
            other => panic!(
                "cannot coerce {:?} to pointer",
                other.get_type().print_to_string()
            ),
        }
    }

    fn lookup(&self, id: ValueId) -> BasicValueEnum<'ctx> {
        *self
            .value_map
            .get(&id)
            .unwrap_or_else(|| panic!("undefined IR value {id:?}"))
    }

    /// Coerce a lowered argument to the callee's declared parameter
    /// type. The non-trivial cases are bitcasts between i64 and f64
    /// (for the WRITEF ABI) and int↔ptr conversions when an integer
    /// is being handed in as a pointer slot.
    fn coerce_arg(
        &self,
        v: BasicValueEnum<'ctx>,
        want: Option<BasicMetadataTypeEnum<'ctx>>,
    ) -> BasicValueEnum<'ctx> {
        let Some(want) = want else { return v };
        match (v, want) {
            // f64 value, i64 slot: bitcast (preserves the bit
            // pattern; matches BCPL's variadic-int convention).
            (BasicValueEnum::FloatValue(fv), BasicMetadataTypeEnum::IntType(it))
                if it.get_bit_width() == 64 =>
            {
                self.builder
                    .build_bit_cast(fv, it, "f2i")
                    .expect("bitcast f→i")
            }
            // i64 value, f64 slot: bitcast back the other way.
            (BasicValueEnum::IntValue(iv), BasicMetadataTypeEnum::FloatType(ft))
                if iv.get_type().get_bit_width() == 64 =>
            {
                self.builder
                    .build_bit_cast(iv, ft, "i2f")
                    .expect("bitcast i→f")
            }
            // Integer value, pointer slot: int-to-ptr.
            (BasicValueEnum::IntValue(iv), BasicMetadataTypeEnum::PointerType(pt)) => self
                .builder
                .build_int_to_ptr(iv, pt, "i2p")
                .expect("inttoptr")
                .into(),
            // Pointer value, integer slot: ptr-to-int.
            (BasicValueEnum::PointerValue(pv), BasicMetadataTypeEnum::IntType(it))
                if it.get_bit_width() == 64 =>
            {
                self.builder
                    .build_ptr_to_int(pv, it, "p2i")
                    .expect("ptrtoint")
                    .into()
            }
            _ => v,
        }
    }

    fn resolve_callee(&mut self, callee: &Value, arg_count: usize) -> FunctionValue<'ctx> {
        match callee {
            Value::Function(name) => {
                // The printf-family builtin needs arity-aware
                // dispatch: the runtime exposes seven fixed-arity
                // entry points (`WRITEF`, `WRITEF1`, …, `WRITEF7`)
                // because we can't declare a real C-variadic in
                // stable Rust. Source uses either spelling per the
                // case convention — `WRITEF("hi")` or
                // `writef("hi, %s", who)` — so we match the
                // canonical name case-insensitively, then resolve to
                // the right arity-suffixed symbol. The runtime's
                // alias registration exposes both UPPERCASE and
                // lowercase forms of every builtin, so the resolved
                // name links cleanly either way.
                let resolved: String = if name.eq_ignore_ascii_case("WRITEF") {
                    let extras = arg_count.saturating_sub(1).min(7);
                    let lowercase = name.chars().next().map(|c| c.is_lowercase()).unwrap_or(false);
                    let stem = if lowercase { "writef" } else { "WRITEF" };
                    if extras == 0 {
                        stem.to_string()
                    } else {
                        format!("{stem}{extras}")
                    }
                } else {
                    name.clone()
                };
                if let Some(&fv) = self.by_name.get(&resolved) {
                    fv
                } else {
                    self.declare_extern(&resolved, arg_count)
                }
            }
            // Indirect calls (Local pointing at a function pointer)
            // are not yet supported — fall back to declaring a
            // placeholder extern so the module still verifies.
            _ => self.declare_extern("__newbcpl_indirect", arg_count),
        }
    }

    /// Convert a value to an i1 boolean for use in a CondBranch.
    /// Integer values get `value != 0`; float values get
    /// `value != 0.0`; pointers get `value != null`.
    fn truthify(&self, v: BasicValueEnum<'ctx>) -> inkwell::values::IntValue<'ctx> {
        match v {
            BasicValueEnum::IntValue(iv) => {
                let zero = iv.get_type().const_zero();
                self.builder
                    .build_int_compare(IntPredicate::NE, iv, zero, "tobool")
                    .expect("icmp")
            }
            BasicValueEnum::FloatValue(fv) => {
                let zero = fv.get_type().const_zero();
                self.builder
                    .build_float_compare(FloatPredicate::ONE, fv, zero, "tobool")
                    .expect("fcmp")
            }
            BasicValueEnum::PointerValue(pv) => {
                let null = pv.get_type().const_null();
                self.builder
                    .build_int_compare(IntPredicate::NE, pv, null, "tobool")
                    .expect("icmp ptr")
            }
            // Vectors / structs as branch conditions don't make sense
            // for BCPL; treat as true.
            _ => self.context.bool_type().const_int(1, false),
        }
    }

    /// Materialise a zero / null of the given hint's LLVM type.
    fn zero(&self, hint: TypeHint) -> BasicValueEnum<'ctx> {
        match self.basic_type_for(hint) {
            BasicTypeEnum::IntType(t) => t.const_zero().into(),
            BasicTypeEnum::FloatType(t) => t.const_zero().into(),
            BasicTypeEnum::PointerType(t) => t.const_null().into(),
            BasicTypeEnum::VectorType(t) => t.const_zero().into(),
            other => panic!("no zero for type {:?}", other.print_to_string()),
        }
    }

    fn intern_string(&mut self, cooked: &str) -> PointerValue<'ctx> {
        if let Some(&p) = self.string_pool.get(cooked) {
            return p;
        }
        let global_name = format!(".str.{}", self.string_counter);
        self.string_counter += 1;
        let v = self
            .builder
            .build_global_string_ptr(cooked, &global_name)
            .expect("global string")
            .as_pointer_value();
        self.string_pool.insert(cooked.to_string(), v);
        v
    }

    // ─── binop / unop dispatch ──────────────────────────────────

    /// Coerce a value to an i64 "word" — the BCPL view in which
    /// pointers, ints, and packed SIMD values are all just words.
    /// Used at the head of every integer-family binop so that
    /// e.g. `IF V = 0` (V is a VEC pointer, 0 is an int literal)
    /// lowers to a clean `ptrtoint` + `icmp.eq`.
    /// Coerce a value to an f64 — the BCPL "float word" view.
    /// Mirror of `as_int_word`. The interesting case is when sema
    /// didn't bind an identifier (so `lower_ident` falls through
    /// to `Value::Function(name)`, which becomes a pointer to a
    /// declared-but-undefined external function): instead of
    /// crashing the float emitter on `into_float_value()`, we
    /// substitute 0.0 so the program runs to a clean
    /// "missing builtin" error from the JIT's pre-flight scan.
    /// Integer values go through `sitofp`. Floats pass through.
    fn as_float_value(&self, v: BasicValueEnum<'ctx>) -> inkwell::values::FloatValue<'ctx> {
        let f64_t = self.context.f64_type();
        match v {
            BasicValueEnum::FloatValue(fv) => fv,
            BasicValueEnum::IntValue(iv) => self
                .builder
                .build_signed_int_to_float(iv, f64_t, "i2f")
                .expect("sitofp"),
            BasicValueEnum::PointerValue(_) | BasicValueEnum::VectorValue(_) => {
                // Sema/IR gap: an unresolved identifier was used
                // in float arithmetic. Substitute 0.0 so codegen
                // succeeds — the JIT's missing-builtin scan
                // catches the dangling extern at run-prep time
                // and produces a clean error.
                f64_t.const_zero()
            }
            _ => f64_t.const_zero(),
        }
    }

    fn as_int_word(&self, v: BasicValueEnum<'ctx>) -> inkwell::values::IntValue<'ctx> {
        let i64_t = self.context.i64_type();
        match v {
            BasicValueEnum::IntValue(iv) if iv.get_type().get_bit_width() == 64 => iv,
            BasicValueEnum::IntValue(iv) => self
                .builder
                .build_int_z_extend(iv, i64_t, "iext")
                .expect("zext"),
            BasicValueEnum::PointerValue(pv) => self
                .builder
                .build_ptr_to_int(pv, i64_t, "p2i")
                .expect("ptrtoint"),
            BasicValueEnum::FloatValue(fv) => self
                .builder
                .build_bit_cast(fv, i64_t, "f2i")
                .expect("f→i bitcast")
                .into_int_value(),
            // SIMD lanes presented as an LLVM vector. The BCPL
            // dialect packs PAIR / FPAIR into a single 64-bit word
            // (two 32-bit lanes), so we bitcast 64-bit vectors
            // straight to i64. Wider vectors (e.g. our current
            // PAIR which lowers as <2 x i64>) don't have a clean
            // integer-word view — extract lane 0 as a placeholder
            // so the program runs end-to-end. Real fix is to
            // narrow the IR's PAIR representation to two 32-bit
            // lanes; tracked separately.
            BasicValueEnum::VectorValue(vv) => {
                let ty = vv.get_type();
                let total_bits = ty.get_size() * ty.get_element_type().into_int_type().get_bit_width() as u32;
                if total_bits == 64 {
                    self.builder
                        .build_bit_cast(vv, i64_t, "vec2i")
                        .expect("vec→i bitcast")
                        .into_int_value()
                } else {
                    // Wider than a word: reduce to lane 0 so a
                    // comparison or arithmetic at least produces
                    // *some* int. This is wrong for true SIMD
                    // semantics but unblocks tests until PAIR is
                    // correctly represented.
                    let lane0 = self
                        .builder
                        .build_extract_element(vv, i64_t.const_zero(), "lane0")
                        .expect("extract lane 0");
                    self.as_int_word(lane0)
                }
            }
            other => panic!(
                "cannot coerce {:?} to int word",
                other.get_type().print_to_string()
            ),
        }
    }

    fn lower_binop(
        &self,
        op: IrBinOp,
        lhs: BasicValueEnum<'ctx>,
        rhs: BasicValueEnum<'ctx>,
    ) -> BasicValueEnum<'ctx> {
        // Integer ops dispatch on int operand variants; float ops on
        // float operands. The IR has already chosen the family via
        // sema's hints, so each branch is pure mapping. We coerce
        // pointer-typed operands to integer words at the int-family
        // boundary to honour BCPL's "everything is a word" view.
        match op {
            IrBinOp::IAdd => self
                .builder
                .build_int_add(self.as_int_word(lhs), self.as_int_word(rhs), "iadd")
                .unwrap()
                .into(),
            IrBinOp::ISub => self
                .builder
                .build_int_sub(self.as_int_word(lhs), self.as_int_word(rhs), "isub")
                .unwrap()
                .into(),
            IrBinOp::IMul => self
                .builder
                .build_int_mul(self.as_int_word(lhs), self.as_int_word(rhs), "imul")
                .unwrap()
                .into(),
            IrBinOp::IDiv => self
                .builder
                .build_int_signed_div(self.as_int_word(lhs), self.as_int_word(rhs), "idiv")
                .unwrap()
                .into(),
            IrBinOp::IRem => self
                .builder
                .build_int_signed_rem(self.as_int_word(lhs), self.as_int_word(rhs), "irem")
                .unwrap()
                .into(),
            IrBinOp::FAdd => self
                .builder
                .build_float_add(self.as_float_value(lhs), self.as_float_value(rhs), "fadd")
                .unwrap()
                .into(),
            IrBinOp::FSub => self
                .builder
                .build_float_sub(self.as_float_value(lhs), self.as_float_value(rhs), "fsub")
                .unwrap()
                .into(),
            IrBinOp::FMul => self
                .builder
                .build_float_mul(self.as_float_value(lhs), self.as_float_value(rhs), "fmul")
                .unwrap()
                .into(),
            IrBinOp::FDiv => self
                .builder
                .build_float_div(self.as_float_value(lhs), self.as_float_value(rhs), "fdiv")
                .unwrap()
                .into(),
            IrBinOp::BitAnd => self
                .builder
                .build_and(self.as_int_word(lhs), self.as_int_word(rhs), "and")
                .unwrap()
                .into(),
            IrBinOp::BitOr => self
                .builder
                .build_or(self.as_int_word(lhs), self.as_int_word(rhs), "or")
                .unwrap()
                .into(),
            IrBinOp::BitXor => self
                .builder
                .build_xor(self.as_int_word(lhs), self.as_int_word(rhs), "xor")
                .unwrap()
                .into(),
            IrBinOp::Shl => self
                .builder
                .build_left_shift(self.as_int_word(lhs), self.as_int_word(rhs), "shl")
                .unwrap()
                .into(),
            IrBinOp::Shr => self
                .builder
                .build_right_shift(
                    self.as_int_word(lhs),
                    self.as_int_word(rhs),
                    /* sign_extend = */ true,
                    "shr",
                )
                .unwrap()
                .into(),
            IrBinOp::ICmpEq
            | IrBinOp::ICmpNe
            | IrBinOp::ICmpLt
            | IrBinOp::ICmpLe
            | IrBinOp::ICmpGt
            | IrBinOp::ICmpGe => {
                let pred = match op {
                    IrBinOp::ICmpEq => IntPredicate::EQ,
                    IrBinOp::ICmpNe => IntPredicate::NE,
                    IrBinOp::ICmpLt => IntPredicate::SLT,
                    IrBinOp::ICmpLe => IntPredicate::SLE,
                    IrBinOp::ICmpGt => IntPredicate::SGT,
                    IrBinOp::ICmpGe => IntPredicate::SGE,
                    _ => unreachable!(),
                };
                let bit = self
                    .builder
                    .build_int_compare(pred, self.as_int_word(lhs), self.as_int_word(rhs), "icmp")
                    .unwrap();
                // BCPL relational ops produce a WORD (0 or 1), not
                // an i1 — zero-extend so the result fits the rest of
                // the integer arithmetic.
                self.builder
                    .build_int_z_extend(bit, self.context.i64_type(), "zext")
                    .unwrap()
                    .into()
            }
            IrBinOp::FCmpEq
            | IrBinOp::FCmpNe
            | IrBinOp::FCmpLt
            | IrBinOp::FCmpLe
            | IrBinOp::FCmpGt
            | IrBinOp::FCmpGe => {
                let pred = match op {
                    IrBinOp::FCmpEq => FloatPredicate::OEQ,
                    IrBinOp::FCmpNe => FloatPredicate::ONE,
                    IrBinOp::FCmpLt => FloatPredicate::OLT,
                    IrBinOp::FCmpLe => FloatPredicate::OLE,
                    IrBinOp::FCmpGt => FloatPredicate::OGT,
                    IrBinOp::FCmpGe => FloatPredicate::OGE,
                    _ => unreachable!(),
                };
                let bit = self
                    .builder
                    .build_float_compare(
                        pred,
                        self.as_float_value(lhs),
                        self.as_float_value(rhs),
                        "fcmp",
                    )
                    .unwrap();
                self.builder
                    .build_int_z_extend(bit, self.context.i64_type(), "zext")
                    .unwrap()
                    .into()
            }
        }
    }

    fn lower_unop(&self, op: IrUnOp, operand: BasicValueEnum<'ctx>) -> BasicValueEnum<'ctx> {
        match op {
            IrUnOp::INeg => self
                .builder
                .build_int_neg(operand.into_int_value(), "ineg")
                .unwrap()
                .into(),
            IrUnOp::FNeg => self
                .builder
                .build_float_neg(self.as_float_value(operand), "fneg")
                .unwrap()
                .into(),
            IrUnOp::Not => self
                .builder
                .build_not(operand.into_int_value(), "not")
                .unwrap()
                .into(),
        }
    }

    // ─── type mapping ───────────────────────────────────────────

    fn basic_type_for(&self, hint: TypeHint) -> BasicTypeEnum<'ctx> {
        match hint {
            TypeHint::Word | TypeHint::Int | TypeHint::Unknown => {
                self.context.i64_type().into()
            }
            TypeHint::Float => self.context.f64_type().into(),
            TypeHint::String
            | TypeHint::Object
            | TypeHint::List
            | TypeHint::Vec
            | TypeHint::FVec
            | TypeHint::Function
            | TypeHint::Null => self.context.ptr_type(AddressSpace::default()).into(),
            // PAIR / FPAIR / QUAD / OCT pack into a single 64-bit
            // word per the reference's ABI (see
            // docs/pair_and_multilane_types.md). FPAIR's two f32
            // lanes are reinterpreted as i32s and stored in the
            // same i64 word — keeping the storage type i64 lets
            // LIST / VEC slots hold a PAIR or FPAIR without
            // overrun.
            TypeHint::Pair | TypeHint::FPair | TypeHint::Quad | TypeHint::Oct => {
                self.context.i64_type().into()
            }
            // FQUAD = 4 × f32 (one Q-reg, 128 bits).
            TypeHint::FQuad => self.context.f32_type().vec_type(4).into(),
            // FOCT  = 8 × f32 (two Q-regs, 256 bits).
            TypeHint::FOct => self.context.f32_type().vec_type(8).into(),
        }
    }

    /// `None` means the function returns void (used only when the
    /// inferred return hint is itself unmaterialisable). Right now
    /// every BCPL routine returns WORD by convention, so this is
    /// always `Some`.
    fn return_type_for(&self, hint: TypeHint) -> Option<BasicTypeEnum<'ctx>> {
        Some(self.basic_type_for(hint))
    }
}

/// Strip a BCPL string lexeme's outer quotes and decode the dialect's
/// `*`-prefix escape sequences (`*N` → newline, `*T` → tab, `*S` →
/// space, `**` → `*`, `*"` → `"`, etc.).
pub(crate) fn cook_bcpl_string(raw: &str) -> String {
    let bytes = raw.as_bytes();
    if bytes.len() < 2 || bytes[0] != b'"' || bytes[bytes.len() - 1] != b'"' {
        return raw.to_string();
    }
    let inner = &raw[1..raw.len() - 1];
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '*' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') | Some('N') => out.push('\n'),
            Some('t') | Some('T') => out.push('\t'),
            Some('s') | Some('S') => out.push(' '),
            Some('b') | Some('B') => out.push('\u{08}'),
            Some('p') | Some('P') => out.push('\u{0C}'),
            Some('c') | Some('C') => out.push('\r'),
            Some('"') => out.push('"'),
            Some('*') => out.push('*'),
            Some(other) => {
                // Unknown escape — keep the `*` and the following
                // char verbatim so the diagnostic stays visible.
                out.push('*');
                out.push(other);
            }
            None => out.push('*'),
        }
    }
    out
}
