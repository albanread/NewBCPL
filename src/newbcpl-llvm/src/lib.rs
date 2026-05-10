//! NewBCPL LLVM emit + JIT.
//!
//! Lowers `newbcpl-ir::Module` to LLVM IR via Inkwell (LLVM 22),
//! produces both the textual LLVM IR (for `dump-llvm`) and the
//! native assembly (for `dump-asm`). Targets `x86_64-pc-windows-msvc`
//! by default; the JIT entry point arrives in a follow-up.
//!
//! See `emit::emit` for the IR-to-LLVM walker.

pub mod emit;

use std::path::Path;

use inkwell::OptimizationLevel;
use inkwell::context::Context;
use inkwell::execution_engine::JitFunction;
use inkwell::targets::{
    CodeModel, FileType, InitializationConfig, RelocMode, Target, TargetMachine,
};

use newbcpl_ir::Module as IrModule;

/// Lex / parse / sema / lower the file, emit LLVM IR, and return
/// it as a textual artifact suitable for `dump-llvm`.
pub fn dump_llvm(path: &Path) -> String {
    match build_ir(path) {
        Ok(ir) => {
            let context = Context::create();
            let module = emit::emit(&context, &ir);
            format!(
                "newbcpl-llvm dump\ninput: {}\n\n{}",
                path.display(),
                module.print_to_string().to_string()
            )
        }
        Err(error) => format!(
            "newbcpl-llvm dump\ninput: {}\nerror: {}",
            path.display(),
            error
        ),
    }
}

/// Same pipeline as `dump_llvm`, but runs the LLVM module through a
/// `TargetMachine` to produce native assembly text.
pub fn dump_asm(path: &Path) -> String {
    match build_ir(path) {
        Ok(ir) => {
            let context = Context::create();
            let module = emit::emit(&context, &ir);

            // Initialise the x86 target backend (the family we
            // target — x86_64-pc-windows-msvc).
            Target::initialize_x86(&InitializationConfig::default());

            let triple = TargetMachine::get_default_triple();
            module.set_triple(&triple);

            let target = match Target::from_triple(&triple) {
                Ok(t) => t,
                Err(e) => {
                    return format!(
                        "newbcpl-llvm asm\ninput: {}\nfrom_triple error: {}",
                        path.display(),
                        e.to_string()
                    );
                }
            };

            let target_machine = match target.create_target_machine(
                &triple,
                "generic",
                "",
                OptimizationLevel::Default,
                RelocMode::Default,
                CodeModel::Default,
            ) {
                Some(tm) => tm,
                None => {
                    return format!(
                        "newbcpl-llvm asm\ninput: {}\ncreate_target_machine failed",
                        path.display()
                    );
                }
            };

            let buf = match target_machine
                .write_to_memory_buffer(&module, FileType::Assembly)
            {
                Ok(b) => b,
                Err(e) => {
                    return format!(
                        "newbcpl-llvm asm\ninput: {}\nwrite_to_memory_buffer error: {}",
                        path.display(),
                        e.to_string()
                    );
                }
            };

            let asm = String::from_utf8_lossy(buf.as_slice()).to_string();
            format!(
                "newbcpl-llvm asm\ninput: {}\ntarget: {}\n\n{}",
                path.display(),
                triple.as_str().to_string_lossy(),
                asm
            )
        }
        Err(error) => format!(
            "newbcpl-llvm asm\ninput: {}\nerror: {}",
            path.display(),
            error
        ),
    }
}

/// Build a JIT execution engine for the program at `path` and call
/// its top-level `START` routine. Builtin addresses (WRITES, WRITEN,
/// WRITEC, NEWLINE) are registered up-front so the JIT'd code can
/// reach them.
///
/// Returns the value `START` produced — typically 0 by BCPL
/// convention. Errors during compilation, linking, or execution
/// surface as `Err(String)` so the driver can print them.
pub fn run(path: &Path) -> Result<i64, String> {
    let ir = build_ir(path)?;
    let context = Context::create();
    let module = emit::emit(&context, &ir);

    // MCJIT initialisation. `Default` optimisation runs mem2reg /
    // simple folding — enough to make the loops we already emit
    // look like the assembly we showed earlier.
    let exec_engine = module
        .create_jit_execution_engine(OptimizationLevel::Default)
        .map_err(|e| format!("create_jit_execution_engine: {}", e.to_string()))?;

    // Register every builtin's host-process address with the JIT
    // by symbol name. We can't rely on the dynamic linker finding
    // them — this binary is the JIT host, so we hand the addresses
    // over directly.
    for builtin in newbcpl_runtime::builtins::builtin_addresses() {
        if let Some(fv) = module.get_function(builtin.name) {
            exec_engine.add_global_mapping(&fv, builtin.address);
        }
    }

    // Catch unbound externs *before* execution. Any function the
    // module declares without a body (linkage = external, no entry
    // basic block) and that we did not just register a mapping for
    // would otherwise be called at address 0 and segfault. Surface
    // it as a clean diagnostic instead.
    let mut missing: Vec<String> = Vec::new();
    let mut fopt = module.get_first_function();
    while let Some(f) = fopt {
        if f.count_basic_blocks() == 0 {
            let name = f.get_name().to_string_lossy().into_owned();
            // Skip LLVM intrinsics (`llvm.memset.*` etc.) — those
            // are resolved by LLVM itself, not by our table.
            if !name.starts_with("llvm.")
                && !newbcpl_runtime::builtins::is_builtin(&name)
            {
                missing.push(name);
            }
        }
        fopt = f.get_next_function();
    }
    if !missing.is_empty() {
        return Err(format!("missing builtin: {}", missing.join(", ")));
    }

    // ─── vtable patch loop (NewCP-style for MCJIT) ──────────────
    //
    // For each class layout, look up the @Class.vtable global's
    // runtime storage address and write the JIT'd method addresses
    // into each slot. We have to do this from Rust because MCJIT's
    // RuntimeDyld does not reliably apply function-pointer
    // relocations to constant data globals — the slots stay zero
    // if you encode them as constant initialisers.
    //
    // `LLVMGetGlobalValueAddress` gives us the vtable storage;
    // `LLVMGetPointerToGlobal` resolves a method's compiled address
    // (more reliable than name-based `get_function_address` for
    // non-exported / mangled methods, per NewCP's findings).
    use inkwell::llvm_sys::execution_engine::{
        LLVMGetGlobalValueAddress, LLVMGetPointerToGlobal,
    };
    use inkwell::values::AsValueRef;
    use std::ffi::CString;
    for layout in &ir.layouts {
        if layout.vtable.is_empty() {
            continue;
        }
        let vt_name = match CString::new(format!("{}.vtable", layout.class_name)) {
            Ok(s) => s,
            Err(_) => continue,
        };
        let vt_addr = unsafe {
            LLVMGetGlobalValueAddress(
                exec_engine.as_mut_ptr(),
                vt_name.as_ptr(),
            )
        };
        if vt_addr == 0 {
            // Vtable global was DCE'd by an LLVM pass. Skip — any
            // virtual call into this class will read zero and the
            // missing-builtin check above won't catch it; tests
            // that depend on it will print zeros instead of method
            // results.
            continue;
        }
        let vt_ptr = vt_addr as *mut usize;
        for entry in &layout.vtable {
            let owner = match &entry.defining_class {
                Some(c) => c,
                // Default CREATE / RELEASE for classes that don't
                // declare them: leave the slot at zero — virtual
                // calls into these are harmless because nobody
                // generates one (sema only emits MethodCall for
                // declared methods).
                None => continue,
            };
            let method_symbol = format!("{owner}_{}", entry.method_name);
            let fv = match module.get_function(&method_symbol) {
                Some(f) => f,
                // The method's body lives in a different module
                // (cross-module dispatch isn't wired yet) — skip.
                None => continue,
            };
            let fn_addr = unsafe {
                LLVMGetPointerToGlobal(
                    exec_engine.as_mut_ptr(),
                    fv.as_value_ref(),
                )
            } as usize;
            if fn_addr == 0 {
                continue;
            }
            unsafe {
                vt_ptr.add(entry.slot).write(fn_addr);
            }
        }
    }

    // Locate START. Every BCPL program declares one; if it isn't
    // there, the program is malformed for execution purposes.
    let start_fn = module
        .get_function("START")
        .ok_or_else(|| "no START function declared".to_string())?;

    // Safety: the function takes no args and returns i64 by our
    // BCPL-routine ABI convention.
    let start: JitFunction<unsafe extern "C" fn() -> i64> = unsafe {
        exec_engine
            .get_function("START")
            .map_err(|e| format!("get_function START: {}", e.to_string()))?
    };
    let _ = start_fn; // suppress unused; we used .get_function for the name lookup
    let result = unsafe { start.call() };
    Ok(result)
}

fn build_ir(path: &Path) -> Result<IrModule, String> {
    let source = std::fs::read_to_string(path).map_err(|e| format!("io: {e}"))?;
    let program = newbcpl_parser::parse_source(&source)
        .map_err(|e| format!("parse: {}", e.render()))?;
    let sema = newbcpl_sema::analyze(&program);
    let module_name = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("module");
    Ok(newbcpl_ir::lower(&program, &sema, module_name))
}

#[cfg(test)]
mod tests {
    use super::*;
    use inkwell::context::Context;

    fn emit_text(source: &str) -> String {
        let program = newbcpl_parser::parse_source(source).expect("parse");
        let sema = newbcpl_sema::analyze(&program);
        let ir = newbcpl_ir::lower(&program, &sema, "test");
        let context = Context::create();
        let module = emit::emit(&context, &ir);
        module.print_to_string().to_string()
    }

    #[test]
    fn empty_routine_emits_function_with_ret_zero() {
        let text = emit_text("LET S() BE { }");
        assert!(text.contains("define i64 @S()"));
        assert!(text.contains("ret i64 0"));
    }

    #[test]
    fn function_returning_int_literal() {
        let text = emit_text("LET answer() = 42");
        assert!(text.contains("define i64 @answer()"));
        assert!(text.contains("ret i64 42"));
    }

    #[test]
    fn function_with_int_param_and_arithmetic() {
        let text = emit_text("LET inc(x) = x + 1");
        // Parameter is i64; body is alloca + store + load + add + ret.
        assert!(text.contains("define i64 @inc(i64"));
        assert!(text.contains("add i64"));
    }

    #[test]
    fn float_function_returns_double() {
        let text = emit_text("LET pi() = 3.14");
        assert!(text.contains("define double @pi()"));
    }

    #[test]
    fn extern_writes_declared_with_pointer_arg() {
        let text = emit_text("LET S() BE { WRITES(\"hi*N\") }");
        // WRITES gets declared on first call.
        assert!(text.contains("declare i64 @WRITES("));
        // The `hi*N` literal is cooked to `hi\n` and stored in a
        // global string.
        assert!(text.contains("hi\\0A"));
    }

    #[test]
    fn if_else_emits_three_blocks_and_branch() {
        let text = emit_text("LET S(x) BE { IF x = 0 THEN f() ELSE g() }");
        assert!(text.contains("br i1"));
        assert!(text.contains("if.then"));
        assert!(text.contains("if.else"));
        assert!(text.contains("if.end"));
    }

    #[test]
    fn relational_results_zero_extend_to_word() {
        let text = emit_text("LET cmp(a, b) = a < b");
        assert!(text.contains("icmp slt"));
        assert!(text.contains("zext i1"));
    }

    // ─── loops, switchon, GEP, lane extract ─────────────────────

    #[test]
    fn while_loop_emits_loop_blocks() {
        let text = emit_text("LET S(n) BE { WHILE n < 10 DO n := n + 1 }");
        assert!(text.contains("while.header"));
        assert!(text.contains("while.body"));
        assert!(text.contains("while.end"));
        assert!(text.contains("br i1"));
    }

    #[test]
    fn for_loop_emits_canonical_cfg() {
        let text = emit_text("LET S() BE { FOR i = 1 TO 10 DO f() }");
        assert!(text.contains("for.header"));
        assert!(text.contains("for.body"));
        assert!(text.contains("for.incr"));
        assert!(text.contains("for.end"));
    }

    #[test]
    fn valof_with_resultis_threads_through() {
        let text = emit_text(
            "LET sum(n) = VALOF $(\n LET acc = 0\n FOR i = 1 TO n DO acc := acc + i\n RESULTIS acc\n$)",
        );
        // The function returns the loaded VALOF result.
        assert!(text.contains("valof.result"));
        assert!(text.contains("valof.end"));
        assert!(text.contains("ret i64"));
        // FOR loop bodies are present too.
        assert!(text.contains("for.header"));
    }

    #[test]
    fn switchon_emits_llvm_switch() {
        let text = emit_text(
            "LET S(x) BE { SWITCHON x INTO $( CASE 1: f()\n CASE 2: g()\n DEFAULT: h() $) }",
        );
        assert!(text.contains("switch i64"));
        assert!(text.contains("switch.case0"));
        assert!(text.contains("switch.case1"));
        assert!(text.contains("switch.default"));
    }

    #[test]
    fn vec_subscript_emits_gep_plus_load() {
        let text = emit_text("LET S() BE { LET v = VEC 10\n LET a = v!3 }");
        // GEP with i8 element type carries the byte offset.
        assert!(text.contains("getelementptr"));
        assert!(text.contains("load i64"));
    }

    #[test]
    fn vec_subscript_assign_emits_gep_plus_store() {
        let text = emit_text("LET S() BE { LET v = VEC 10\n v!3 := 42 }");
        assert!(text.contains("getelementptr"));
        assert!(text.contains("store i64 42"));
    }

    #[test]
    fn float_subscript_loads_double() {
        let text = emit_text("LET S() BE { LET fv = FVEC 10\n LET a = fv.%3 }");
        assert!(text.contains("load double"));
    }

    #[test]
    fn prefix_indirection_emits_load_of_word() {
        let text = emit_text("LET S(p) BE { LET a = !p }");
        assert!(text.contains("load i64"));
    }

    #[test]
    fn prefix_indirection_assignment_emits_store() {
        let text = emit_text("LET S(p) BE { !p := 42 }");
        assert!(text.contains("store i64 42"));
    }

    // ─── classes: NEW + field load/store ────────────────────────

    #[test]
    fn new_class_allocates_instance() {
        let text = emit_text(
            "CLASS Point $( DECL x, y $)\nLET S() BE { LET p = NEW Point }",
        );
        // `NEW Class` allocates on the GC heap via
        // `__newbcpl_alloc_rec(size)`. The runtime interns a
        // TypeDesc per distinct payload size and stamps every
        // BlockHeader with that stable address — see
        // `docs/jit_typedesc_lifetime.md`. The size argument is
        // sema's `instance_size` (24 for Point: 8 vtable header
        // + 2 word fields).
        assert!(text.contains("@__newbcpl_alloc_rec"));
        assert!(text.contains("i64 24"));
    }

    #[test]
    fn field_load_uses_byte_offset_from_layout() {
        let text = emit_text(
            "CLASS Point $( DECL x, y $)\nLET S() BE { LET p = NEW Point\n LET v = p.y }",
        );
        // Field y is at offset 16 (vtable header + 8 for x).
        assert!(text.contains("getelementptr"));
        assert!(text.contains("i64 16"));
        assert!(text.contains("load i64"));
    }

    #[test]
    fn field_store_emits_gep_plus_store() {
        let text = emit_text(
            "CLASS Point $( DECL x, y $)\nLET S() BE { LET p = NEW Point\n p.x := 99 }",
        );
        assert!(text.contains("getelementptr"));
        // x is the first field at offset 8.
        assert!(text.contains("i64 8"));
        assert!(text.contains("store i64 99"));
    }

    #[test]
    fn class_with_create_emits_call() {
        let text = emit_text(
            "CLASS Foo $(\n  DECL x\n  ROUTINE CREATE(ix) BE $( SELF.x := ix $)\n$)\nLET S() BE { LET f = NEW Foo(42) }",
        );
        // CREATE is now called via its mangled `{Class}_CREATE`
        // symbol so multiple classes can each have their own.
        // The receiver pointer is the first argument; 42 is the
        // second.
        assert!(text.contains("call i64 @Foo_CREATE"));
        assert!(text.contains("i64 42"));
    }

    #[test]
    fn dump_llvm_smoke() {
        // End-to-end: write a tiny program to a temp file, run
        // dump_llvm, and check the header / body.
        let tmp = std::env::temp_dir().join("newbcpl_llvm_smoke.bcl");
        std::fs::write(&tmp, "LET S() BE { LET y = 1 + 2 }").unwrap();
        let dump = dump_llvm(&tmp);
        assert!(dump.contains("newbcpl-llvm dump"));
        assert!(dump.contains("define i64 @S()"));
        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn every_function_polls_safepoint() {
        // Cooperative-GC plumbing: the IR-emit pass inserts a
        // `__newbcpl_safepoint()` call at the top of every JIT'd
        // function so the collector can pause threads that
        // never allocate. Confirm the call shows up in both the
        // top-level routine and a class method body — START and
        // Foo_CREATE both need to be parkable.
        let text = emit_text(
            "CLASS Foo $(\n  DECL x\n  ROUTINE CREATE(ix) BE $( SELF.x := ix $)\n$)\nLET S() BE { LET f = NEW Foo(42) }",
        );
        let safepoint_calls = text.matches("call void @__newbcpl_safepoint()").count();
        assert!(
            safepoint_calls >= 2,
            "expected at least one safepoint call per function (START and Foo_CREATE), got {safepoint_calls}\n{text}"
        );
        assert!(text.contains("declare void @__newbcpl_safepoint()"));
    }

    #[test]
    fn jit_run_advances_heap_block_counter() {
        // End-to-end proof that JIT-emitted `NEW Class` flows
        // through the GC: compile a program that creates three
        // class instances, run it, and check the global block
        // counter advanced by at least three.
        let tmp = std::env::temp_dir().join("newbcpl_jit_alloc.bcl");
        std::fs::write(
            &tmp,
            "CLASS Point $(\n  DECL x, y\n  ROUTINE CREATE(ix, iy) BE $( SELF.x := ix\n SELF.y := iy $)\n$)\nLET START() BE $(\n LET a = NEW Point(1, 2)\n LET b = NEW Point(3, 4)\n LET c = NEW Point(5, 6)\n$)",
        )
        .unwrap();
        let before = newbcpl_runtime::gc::snapshot();
        let blocks_before = before
            .mutators
            .iter()
            .map(|m| m.alloc_blocks_lifetime)
            .sum::<u64>();
        run(&tmp).expect("JIT run should succeed");
        let after = newbcpl_runtime::gc::snapshot();
        let blocks_after = after
            .mutators
            .iter()
            .map(|m| m.alloc_blocks_lifetime)
            .sum::<u64>();
        assert!(
            blocks_after >= blocks_before + 3,
            "JIT'd START allocated three Point instances but the heap counter \
             only moved {} → {} (expected ≥ +3)",
            blocks_before, blocks_after
        );
        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn collect_after_jit_run_does_not_crash() {
        // Fix B in `docs/jit_typedesc_lifetime.md`: TypeDescs
        // are interned by `__newbcpl_alloc_rec` on the runtime
        // side, so they survive the JIT engine drop. After
        // `run()` returns we can safely walk the heap with
        // `collect()` and start a fresh JIT run on top of it.
        //
        // The previous incarnation of this test crashed in
        // `collect()` because `BlockHeader.tag` pointed into the
        // JIT module's freed data section. With `__newbcpl_alloc_rec`
        // in place, every tag points into a `Box::leak`'d
        // `RuntimeTypeDesc` that lives for the process lifetime.
        let tmp = std::env::temp_dir().join("newbcpl_jit_collect.bcl");
        std::fs::write(
            &tmp,
            "CLASS Point $(\n  DECL x, y\n  ROUTINE CREATE(ix, iy) BE $( SELF.x := ix\n SELF.y := iy $)\n$)\nLET START() BE $(\n LET a = NEW Point(1, 2)\n LET b = NEW Point(3, 4)\n LET c = NEW Point(5, 6)\n$)",
        )
        .unwrap();
        run(&tmp).expect("first JIT run should succeed");
        // collect() walks every BlockHeader; if any tag pointed
        // into freed JIT memory this would access-violation.
        newbcpl_runtime::gc::collect();
        // Heap must remain usable for subsequent JIT runs.
        run(&tmp).expect("post-collect JIT run should succeed");
        newbcpl_runtime::gc::collect();
        let _ = std::fs::remove_file(&tmp);
    }
}
