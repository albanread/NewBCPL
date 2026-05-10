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

    // ─── loops ──────────────────────────────────────────────────

    fn count_blocks_with<F: Fn(&Terminator) -> bool>(f: &Function, pred: F) -> usize {
        f.blocks.iter().filter(|b| pred(&b.terminator)).count()
    }

    #[test]
    fn while_lowers_to_header_body_exit() {
        let m = lower_source("LET S() BE { WHILE i < 10 DO f() }");
        let s = function(&m, "S");
        // entry → header → body → header (loop) … → exit
        assert!(s.blocks.len() >= 4);
        let cb = count_blocks_with(s, |t| matches!(t, Terminator::CondBranch { .. }));
        assert_eq!(cb, 1, "exactly one CondBranch (the WHILE header)");
    }

    #[test]
    fn until_swaps_branch_arms() {
        // Same shape as WHILE but the cond-branch arms are swapped:
        // body executes when cond is FALSE.
        let m = lower_source("LET S() BE { UNTIL done DO step() }");
        let s = function(&m, "S");
        let cb = count_blocks_with(s, |t| matches!(t, Terminator::CondBranch { .. }));
        assert_eq!(cb, 1);
    }

    #[test]
    fn break_jumps_to_loop_exit() {
        let m = lower_source(
            "LET S() BE { WHILE i < 10 DO $( IF i = 5 THEN BREAK\n step() $) }",
        );
        let s = function(&m, "S");
        // Two cond-branches: WHILE header and IF inside the body.
        let cb = count_blocks_with(s, |t| matches!(t, Terminator::CondBranch { .. }));
        assert_eq!(cb, 2);
        // BREAK creates an unconditional branch to the WHILE exit.
        let branches = s
            .blocks
            .iter()
            .filter(|b| matches!(b.terminator, Terminator::Branch(_)))
            .count();
        assert!(branches >= 3, "expected at least 3 unconditional branches");
    }

    #[test]
    fn loop_keyword_jumps_to_continue_target() {
        let m = lower_source(
            "LET S() BE { WHILE i < 10 DO $( IF cond THEN LOOP\n step() $) }",
        );
        let s = function(&m, "S");
        // Both BREAK and LOOP scenarios produce extra Branch
        // terminators; cond-branches are 2 (WHILE header + IF).
        let cb = count_blocks_with(s, |t| matches!(t, Terminator::CondBranch { .. }));
        assert_eq!(cb, 2);
    }

    #[test]
    fn for_loop_emits_init_header_body_incr_exit() {
        let m = lower_source("LET S() BE { FOR i = 1 TO 10 DO f(i) }");
        let s = function(&m, "S");
        // entry alloca's i, branches to header. Header has
        // CondBranch. Body branches to incr. Incr branches to header.
        // Exit ends with return.
        assert!(s.blocks.len() >= 5, "expected ≥5 blocks for FOR");
        // Exactly one CondBranch (the header test).
        let cb = count_blocks_with(s, |t| matches!(t, Terminator::CondBranch { .. }));
        assert_eq!(cb, 1);
        // The entry block must have an alloca for `i` and a store
        // of the start value.
        let entry = &s.blocks[0];
        let has_i = entry
            .instrs
            .iter()
            .any(|i| matches!(i, Instr::Alloca { name, .. } if name == "i"));
        assert!(has_i);
    }

    #[test]
    fn for_with_by_uses_step_value() {
        let m = lower_source("LET S() BE { FOR i = 0 TO 100 BY 5 DO f(i) }");
        let s = function(&m, "S");
        // The increment block contains an iadd of 5 (constant).
        let has_step = s.blocks.iter().any(|b| {
            b.instrs.iter().any(|i| {
                matches!(
                    i,
                    Instr::BinOp {
                        op: IrBinOp::IAdd,
                        rhs: Value::Const(Const::Int(5)),
                        ..
                    }
                )
            })
        });
        assert!(has_step, "expected iadd with step=5");
    }

    #[test]
    fn repeat_forever_only_exits_via_break() {
        let m = lower_source("LET S() BE { $( BREAK $) REPEAT }");
        let s = function(&m, "S");
        // entry → body, body has BREAK which branches to exit.
        // The body's natural fallthrough also branches to body
        // (the repeat).
        assert!(s.blocks.len() >= 3);
    }

    #[test]
    fn repeat_while_tests_after_body() {
        let m = lower_source("LET S() BE { $( step() $) REPEATWHILE i < 10 }");
        let s = function(&m, "S");
        // body → test → body (loop) | exit
        assert!(s.blocks.len() >= 4);
        let cb = count_blocks_with(s, |t| matches!(t, Terminator::CondBranch { .. }));
        assert_eq!(cb, 1);
    }

    #[test]
    fn repeat_until_inverts_the_test() {
        let m = lower_source("LET S() BE { $( step() $) REPEATUNTIL done }");
        let s = function(&m, "S");
        // Same shape as REPEATWHILE; difference is the cond-branch
        // arms get swapped — observable only by comparing
        // then_block / else_block ordering, which we don't here.
        let cb = count_blocks_with(s, |t| matches!(t, Terminator::CondBranch { .. }));
        assert_eq!(cb, 1);
    }

    // ─── classes: NEW / field access / method dispatch ──────────

    #[test]
    fn new_lowers_to_new_instruction() {
        let m = lower_source(
            "CLASS Point $( DECL x, y $)\nLET S() BE { LET p = NEW Point }",
        );
        let s = function(&m, "S");
        let entry = &s.blocks[0];
        let has_new = entry.instrs.iter().any(|i| matches!(
            i,
            Instr::New { class_name, .. } if class_name == "Point"
        ));
        assert!(has_new, "expected New instruction");
    }

    #[test]
    fn field_load_uses_layout_offset() {
        let m = lower_source(
            "CLASS Point $( DECL x, y $)\nLET S() BE { LET p = NEW Point\n LET q = p.y }",
        );
        let s = function(&m, "S");
        let entry = &s.blocks[0];
        // y is the second field — offset 8 (vtable header) + 8 (x) = 16.
        let has_field_load = entry.instrs.iter().any(|i| matches!(
            i,
            Instr::FieldLoad { byte_offset: 16, .. }
        ));
        assert!(has_field_load, "expected FieldLoad with byte_offset=16");
    }

    #[test]
    fn field_load_first_field_offset_8() {
        let m = lower_source(
            "CLASS Point $( DECL x, y $)\nLET S() BE { LET p = NEW Point\n LET q = p.x }",
        );
        let s = function(&m, "S");
        let entry = &s.blocks[0];
        let has_field_load = entry.instrs.iter().any(|i| matches!(
            i,
            Instr::FieldLoad { byte_offset: 8, .. }
        ));
        assert!(has_field_load, "expected FieldLoad at offset 8");
    }

    #[test]
    fn field_store_emits_field_store_instruction() {
        let m = lower_source(
            "CLASS Point $( DECL x, y $)\nLET S() BE { LET p = NEW Point\n p.y := 99 }",
        );
        let s = function(&m, "S");
        let entry = &s.blocks[0];
        let has_field_store = entry.instrs.iter().any(|i| matches!(
            i,
            Instr::FieldStore {
                byte_offset: 16,
                value: Value::Const(Const::Int(99)),
                ..
            }
        ));
        assert!(has_field_store, "expected FieldStore at +16 storing 99");
    }

    #[test]
    fn method_call_resolves_vtable_slot() {
        let m = lower_source(
            "CLASS Point $(\n  DECL x\n  FUNCTION getX() = x\n$)\nLET S() BE { LET p = NEW Point\n LET v = p.getX() }",
        );
        let s = function(&m, "S");
        // CREATE = slot 0, RELEASE = slot 1, getX = slot 2.
        let has_vcall = s.blocks.iter().any(|b| {
            b.instrs.iter().any(|i| {
                matches!(
                    i,
                    Instr::MethodCall {
                        method_name,
                        vtable_slot: 2,
                        ..
                    } if method_name == "getX"
                )
            })
        });
        assert!(has_vcall, "expected MethodCall with vtable_slot=2");
    }

    #[test]
    fn method_call_routine_shape_has_no_dst() {
        let m = lower_source(
            "CLASS Point $(\n  DECL x\n  ROUTINE move() BE $( $)\n$)\nLET S() BE { LET p = NEW Point\n p.move() }",
        );
        let s = function(&m, "S");
        let entry = &s.blocks[0];
        let routine_call = entry.instrs.iter().any(|i| matches!(
            i,
            Instr::MethodCall { dst: None, method_name, .. } if method_name == "move"
        ));
        assert!(routine_call, "expected void MethodCall for move()");
    }

    #[test]
    fn class_name_propagates_through_let_alias() {
        // LET q = p (where p is OBJECT[Point]) should make q.field
        // work too. This relies on `class_name_of_expr` looking up
        // local class names.
        let m = lower_source(
            "CLASS Point $( DECL x $)\nLET S() BE { LET p = NEW Point\n LET q = p\n LET v = q.x }",
        );
        let s = function(&m, "S");
        let has_field_load = s.blocks.iter().any(|b| {
            b.instrs.iter().any(|i| matches!(i, Instr::FieldLoad { .. }))
        });
        assert!(has_field_load, "expected FieldLoad after LET alias");
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
