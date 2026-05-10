//! NewBCPL typed IR.
//!
//! Sits between the typed AST (`newbcpl-parser` + `newbcpl-sema`) and
//! LLVM IR emission. See [`ir`] for the data structures and
//! [`lower`] for the AST → IR walker.
//!
//! Driver entry point is [`dump_ir`]: read a .bcl file, lex, parse,
//! analyze, lower, render. Stable text output for review and
//! regression testing.

pub mod ir;
pub mod lower;
pub mod print;

use std::path::Path;

pub use ir::{
    BasicBlock, BlockId, Const, Function, Instr, IrBinOp, IrUnOp, Module, Param, Terminator,
    Value, ValueId,
};
pub use lower::lower;

/// Read a .bcl file, run the front-end pipeline (lex → parse →
/// sema → lower), and return a textual dump of the resulting IR.
pub fn dump_ir(path: &Path) -> String {
    match std::fs::read_to_string(path) {
        Ok(source) => match newbcpl_parser::parse_source(&source) {
            Ok(program) => {
                let sema = newbcpl_sema::analyze(&program);
                let module_name = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("module");
                let module = lower(&program, &sema, module_name);
                format!(
                    "newbcpl-ir IR dump\ninput: {}\n\n{}",
                    path.display(),
                    print::render(&module)
                )
            }
            Err(error) => format!(
                "newbcpl-ir IR dump\ninput: {}\nparse error: {}",
                path.display(),
                error.render()
            ),
        },
        Err(error) => format!(
            "newbcpl-ir IR dump\ninput: {}\nio-error: {}",
            path.display(),
            error
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use newbcpl_parser::parse_source;
    use newbcpl_sema::analyze;

    fn lower_source(source: &str) -> Module {
        let program = parse_source(source).expect("parse");
        let sema = analyze(&program);
        lower(&program, &sema, "test")
    }

    fn function<'a>(m: &'a Module, name: &str) -> &'a Function {
        m.functions
            .iter()
            .find(|f| f.name == name)
            .unwrap_or_else(|| panic!("no function named {name}"))
    }

    #[test]
    fn empty_routine_returns_void() {
        let m = lower_source("LET S() BE { }");
        let s = function(&m, "S");
        // Entry block ends in `return` with no value.
        let entry = &s.blocks[s.entry.0 as usize];
        assert!(matches!(entry.terminator, Terminator::Return(None)));
    }

    #[test]
    fn routine_with_local_uses_alloca_and_store() {
        let m = lower_source("LET S() BE { LET x = 42 }");
        let s = function(&m, "S");
        let entry = &s.blocks[0];
        let has_alloca = entry
            .instrs
            .iter()
            .any(|i| matches!(i, Instr::Alloca { name, .. } if name == "x"));
        let has_store = entry
            .instrs
            .iter()
            .any(|i| matches!(i, Instr::Store { value: Value::Const(Const::Int(42)), .. }));
        assert!(has_alloca, "expected alloca for x");
        assert!(has_store, "expected store of 42");
    }

    #[test]
    fn function_returns_body_value() {
        let m = lower_source("LET answer() = 42");
        let f = function(&m, "answer");
        let entry = &f.blocks[0];
        // Function has no locals introduced by the body — just a
        // direct return of the constant 42.
        assert!(matches!(
            entry.terminator,
            Terminator::Return(Some(Value::Const(Const::Int(42))))
        ));
    }

    #[test]
    fn arithmetic_lowers_to_iadd() {
        let m = lower_source("LET S() BE { LET y = 1 + 2 }");
        let s = function(&m, "S");
        let entry = &s.blocks[0];
        let has_iadd = entry.instrs.iter().any(|i| {
            matches!(
                i,
                Instr::BinOp {
                    op: IrBinOp::IAdd,
                    ..
                }
            )
        });
        assert!(has_iadd, "expected iadd binop");
    }

    #[test]
    fn float_arithmetic_lowers_to_fadd() {
        let m = lower_source("LET pi() = 3.14 + 0.001");
        let f = function(&m, "pi");
        let entry = &f.blocks[0];
        let has_fadd = entry.instrs.iter().any(|i| {
            matches!(
                i,
                Instr::BinOp {
                    op: IrBinOp::FAdd,
                    ..
                }
            )
        });
        assert!(has_fadd, "expected fadd binop");
    }

    #[test]
    fn ident_load_after_let_binding() {
        let m = lower_source("LET S() BE { LET x = 1\n LET y = x + 1 }");
        let s = function(&m, "S");
        let entry = &s.blocks[0];
        // We should see a Load of x's slot (as part of evaluating
        // `x + 1`) and a corresponding iadd.
        let has_load = entry.instrs.iter().any(|i| matches!(i, Instr::Load { .. }));
        assert!(has_load, "expected load for x");
    }

    #[test]
    fn if_else_creates_three_extra_blocks() {
        let m = lower_source(
            "LET S() BE { IF x = 0 THEN f() ELSE g() }",
        );
        let s = function(&m, "S");
        // Entry, then.body, else.body, merge — at least 4 blocks.
        assert!(s.blocks.len() >= 4, "expected ≥4 blocks for if/else");
        let has_cond_branch = s
            .blocks
            .iter()
            .any(|b| matches!(b.terminator, Terminator::CondBranch { .. }));
        assert!(has_cond_branch, "expected a cond-branch terminator");
    }

    #[test]
    fn if_without_else_branches_to_merge() {
        let m = lower_source("LET S() BE { IF x = 0 THEN f() }");
        let s = function(&m, "S");
        // Entry, then.body, else.body (empty), merge — 4 blocks.
        let cond_count = s
            .blocks
            .iter()
            .filter(|b| matches!(b.terminator, Terminator::CondBranch { .. }))
            .count();
        assert_eq!(cond_count, 1);
    }

    #[test]
    fn relational_returns_int() {
        let m = lower_source("LET S() BE { LET b = x < 10 }");
        let s = function(&m, "S");
        let entry = &s.blocks[0];
        let has_icmp = entry.instrs.iter().any(|i| {
            matches!(
                i,
                Instr::BinOp {
                    op: IrBinOp::ICmpLt,
                    ..
                }
            )
        });
        assert!(has_icmp, "expected icmp.lt");
    }

    #[test]
    fn parameter_gets_alloca_and_store() {
        let m = lower_source("LET S(x, y) BE { }");
        let s = function(&m, "S");
        assert_eq!(s.params.len(), 2);
        let entry = &s.blocks[0];
        // Two allocas + two stores from the in_value to the slot.
        let alloca_count = entry
            .instrs
            .iter()
            .filter(|i| matches!(i, Instr::Alloca { .. }))
            .count();
        assert_eq!(alloca_count, 2);
    }

    #[test]
    fn call_with_args_emits_call_instruction() {
        let m = lower_source("LET S() BE { WRITES(\"hi*N\") }");
        let s = function(&m, "S");
        let entry = &s.blocks[0];
        let has_call = entry.instrs.iter().any(|i| {
            matches!(
                i,
                Instr::Call { callee: Value::Function(name), .. } if name == "WRITES"
            )
        });
        assert!(has_call, "expected call to WRITES");
    }

    #[test]
    fn assign_emits_store_to_existing_slot() {
        let m = lower_source("LET S() BE { LET x = 0\n x := 42 }");
        let s = function(&m, "S");
        let entry = &s.blocks[0];
        let store_count = entry
            .instrs
            .iter()
            .filter(|i| matches!(i, Instr::Store { .. }))
            .count();
        // Two stores: the LET initialisation and the := assignment.
        assert_eq!(store_count, 2);
    }

    #[test]
    fn return_terminates_block_and_continues_in_dead_block() {
        let m = lower_source("LET S() BE { RETURN\n WRITES(\"unreachable\") }");
        let s = function(&m, "S");
        // Entry block terminates with Return; subsequent statements
        // land in a dead block (after.return).
        let returns = s
            .blocks
            .iter()
            .filter(|b| matches!(b.terminator, Terminator::Return(_)))
            .count();
        assert!(returns >= 1);
    }

    #[test]
    fn module_carries_class_layouts() {
        let m = lower_source(
            "CLASS Point $( DECL x, y $)\nLET S() BE { LET p = NEW Point }",
        );
        assert_eq!(m.layouts.len(), 1);
        assert_eq!(m.layouts[0].class_name, "Point");
    }

    #[test]
    fn dump_smoke() {
        let m = lower_source("LET S() BE { LET y = 1 + 2 }");
        let text = print::render(&m);
        assert!(text.contains("function S"));
        assert!(text.contains("alloca"));
        assert!(text.contains("iadd"));
        assert!(text.contains("return"));
    }
}
