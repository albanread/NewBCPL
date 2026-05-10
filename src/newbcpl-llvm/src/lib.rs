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
