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
        // Stack-alloca placeholder: [size x i8] for the instance.
        // Class Point has size 24 (vtable header + 2 word fields).
        assert!(text.contains("alloca [24 x i8]"));
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
        // The CREATE method is called with the new instance as the
        // first argument and 42 as the second.
        assert!(text.contains("call i64 @CREATE"));
        // Receiver pointer is passed.
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
}
