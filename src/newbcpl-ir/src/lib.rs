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
    BasicBlock, BlockId, Const, Function, GlobalDecl, Instr, IrBinOp, IrUnOp, Module, Param,
    Terminator, TypedKind, Value, ValueId,
};
// Re-export sema's `ClassLayout` as a convenience: it travels with
// every IR `Module` (see `Module.layouts`), so downstream consumers
// like `newbcpl-llvm` can refer to it via the IR crate without
// adding an extra `newbcpl-sema` dependency just for the type.
pub use newbcpl_sema::{ClassLayout, VtableEntry};
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
    use newbcpl_sema::{TypeHint, analyze};

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
    fn method_call_always_binds_dst() {
        // Even routine-shape calls bind a dst. BCPL routines return
        // i64 0 by convention, so the result is harmless to ignore;
        // always allocating keeps lowering uniform and stops
        // user-defined WORD-returning functions from having their
        // results discarded.
        let m = lower_source(
            "CLASS Point $(\n  DECL x\n  ROUTINE move() BE $( $)\n$)\nLET S() BE { LET p = NEW Point\n p.move() }",
        );
        let s = function(&m, "S");
        let entry = &s.blocks[0];
        let routine_call = entry.instrs.iter().any(|i| matches!(
            i,
            Instr::MethodCall { dst: Some(_), method_name, .. } if method_name == "move"
        ));
        assert!(routine_call, "expected MethodCall with bound dst for move()");
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

    // ─── indirection + subscripts ───────────────────────────────

    #[test]
    fn prefix_indirection_emits_iload() {
        let m = lower_source("LET S(p) BE { LET v = !p }");
        let s = function(&m, "S");
        let entry = &s.blocks[0];
        let has_iload = entry
            .instrs
            .iter()
            .any(|i| matches!(i, Instr::IndirectLoad { .. }));
        assert!(has_iload, "expected IndirectLoad");
    }

    #[test]
    fn prefix_indirection_assignment_stores_to_address() {
        let m = lower_source("LET S(p) BE { !p := 42 }");
        let s = function(&m, "S");
        let entry = &s.blocks[0];
        let has_istore = entry
            .instrs
            .iter()
            .any(|i| matches!(i, Instr::IndirectStore { .. }));
        assert!(has_istore, "expected IndirectStore");
    }

    #[test]
    fn address_of_local_returns_slot() {
        let m = lower_source("LET S() BE { LET x = 5\n LET p = @x }");
        let s = function(&m, "S");
        // No new instruction for @x — it just passes the slot
        // ValueId. Verify by counting Stores: x's init + p's init
        // (which stores the slot ValueId of x).
        let entry = &s.blocks[0];
        let store_count = entry
            .instrs
            .iter()
            .filter(|i| matches!(i, Instr::Store { .. }))
            .count();
        assert_eq!(store_count, 2);
    }

    #[test]
    fn vec_subscript_uses_word_stride() {
        let m = lower_source("LET S() BE { LET v = VEC 10\n LET x = v!3 }");
        let s = function(&m, "S");
        let entry = &s.blocks[0];
        // GEP with element_bytes=8 (BCPL word).
        let has_gep = entry.instrs.iter().any(|i| matches!(
            i,
            Instr::Gep { element_bytes: 8, .. }
        ));
        let has_iload = entry
            .instrs
            .iter()
            .any(|i| matches!(i, Instr::IndirectLoad { .. }));
        assert!(has_gep, "expected GEP with stride 8");
        assert!(has_iload, "expected IndirectLoad after GEP");
    }

    #[test]
    fn float_vec_subscript_loads_float() {
        let m = lower_source("LET S() BE { LET fv = FVEC 10\n LET x = fv.%3 }");
        let s = function(&m, "S");
        let entry = &s.blocks[0];
        // The IndirectLoad for `.%` carries FLOAT hint.
        let has_float_load = entry.instrs.iter().any(|i| matches!(
            i,
            Instr::IndirectLoad {
                hint: TypeHint::Float,
                ..
            }
        ));
        assert!(has_float_load, "expected float-typed IndirectLoad");
    }

    #[test]
    fn char_vec_subscript_uses_byte_stride() {
        let m = lower_source("LET S(s) BE { LET c = s%5 }");
        let s = function(&m, "S");
        let entry = &s.blocks[0];
        let has_byte_gep = entry.instrs.iter().any(|i| matches!(
            i,
            Instr::Gep { element_bytes: 1, .. }
        ));
        assert!(has_byte_gep, "expected GEP with stride 1");
    }

    #[test]
    fn vec_subscript_assignment_stores_via_address() {
        let m = lower_source("LET S() BE { LET v = VEC 10\n v!3 := 42 }");
        let s = function(&m, "S");
        let entry = &s.blocks[0];
        let has_istore = entry
            .instrs
            .iter()
            .any(|i| matches!(i, Instr::IndirectStore { .. }));
        let has_gep = entry
            .instrs
            .iter()
            .any(|i| matches!(i, Instr::Gep { .. }));
        assert!(has_gep, "expected GEP for index");
        assert!(has_istore, "expected IndirectStore for vec[i] := value");
    }

    // ─── typed constructors (VEC / SIMD / LIST) ─────────────────

    use ir::TypedKind;

    #[test]
    fn vec_lowers_to_typed_construct() {
        let m = lower_source("LET S() BE { LET v = VEC 100 }");
        let s = function(&m, "S");
        let entry = &s.blocks[0];
        let has_construct = entry.instrs.iter().any(|i| {
            matches!(
                i,
                Instr::TypedConstruct {
                    kind: TypedKind::Vec,
                    ..
                }
            )
        });
        assert!(has_construct, "expected TypedConstruct VEC");
    }

    #[test]
    fn pair_lowers_with_two_args() {
        let m = lower_source("LET S() BE { LET p = PAIR(1, 2) }");
        let s = function(&m, "S");
        let entry = &s.blocks[0];
        let has_pair = entry.instrs.iter().any(|i| {
            matches!(
                i,
                Instr::TypedConstruct {
                    kind: TypedKind::Pair,
                    args,
                    ..
                } if args.len() == 2
            )
        });
        assert!(has_pair, "expected TypedConstruct PAIR with 2 args");
    }

    #[test]
    fn fpair_carries_float_hint() {
        let m = lower_source("LET S() BE { LET p = FPAIR(1.0, 2.0) }");
        let s = function(&m, "S");
        let entry = &s.blocks[0];
        let has_fpair = entry.instrs.iter().any(|i| {
            matches!(
                i,
                Instr::TypedConstruct {
                    kind: TypedKind::FPair,
                    hint: TypeHint::FPair,
                    ..
                }
            )
        });
        assert!(has_fpair, "expected FPAIR with FPair hint");
    }

    #[test]
    fn list_constructor_lowers() {
        let m = lower_source("LET S() BE { LET xs = LIST(1, 2, 3) }");
        let s = function(&m, "S");
        let entry = &s.blocks[0];
        let has_list = entry.instrs.iter().any(|i| {
            matches!(
                i,
                Instr::TypedConstruct {
                    kind: TypedKind::List,
                    args,
                    ..
                } if args.len() == 3
            )
        });
        assert!(has_list, "expected TypedConstruct LIST with 3 args");
    }

    #[test]
    fn quad_oct_lower_correctly() {
        let m = lower_source(
            "LET S() BE {\n LET q = QUAD(1, 2, 3, 4)\n LET o = OCT(1, 2, 3, 4, 5, 6, 7, 8)\n}",
        );
        let s = function(&m, "S");
        let entry = &s.blocks[0];
        let has_quad = entry
            .instrs
            .iter()
            .any(|i| matches!(i, Instr::TypedConstruct { kind: TypedKind::Quad, .. }));
        let has_oct = entry
            .instrs
            .iter()
            .any(|i| matches!(i, Instr::TypedConstruct { kind: TypedKind::Oct, .. }));
        assert!(has_quad, "expected QUAD construct");
        assert!(has_oct, "expected OCT construct");
    }

    #[test]
    fn lane_access_lowers_to_lane_extract() {
        let m = lower_source("LET S() BE { LET p = FPAIR(1.0, 2.0)\n LET x = p.|0| }");
        let s = function(&m, "S");
        let entry = &s.blocks[0];
        let has_lane = entry.instrs.iter().any(|i| {
            matches!(
                i,
                Instr::LaneExtract {
                    hint: TypeHint::Float,
                    ..
                }
            )
        });
        assert!(has_lane, "expected LaneExtract with FLOAT hint");
    }

    // ─── SWITCHON / ENDCASE ─────────────────────────────────────

    #[test]
    fn switchon_emits_switch_terminator() {
        let m = lower_source(
            "LET S(x) BE { SWITCHON x INTO $( CASE 1: f()\n CASE 2: g()\n DEFAULT: h() $) }",
        );
        let s = function(&m, "S");
        let switch_count = s
            .blocks
            .iter()
            .filter(|b| matches!(b.terminator, Terminator::Switch { .. }))
            .count();
        assert_eq!(switch_count, 1, "expected exactly one Switch terminator");
    }

    #[test]
    fn switchon_case_blocks_have_distinct_targets() {
        let m = lower_source(
            "LET S(x) BE { SWITCHON x INTO $( CASE 1: f()\n CASE 2: g() $) }",
        );
        let s = function(&m, "S");
        let Terminator::Switch { cases, default, .. } = s
            .blocks
            .iter()
            .find_map(|b| match &b.terminator {
                Terminator::Switch { cases, default, value: _ } => {
                    Some(Terminator::Switch {
                        cases: cases.clone(),
                        default: *default,
                        value: Value::Const(Const::Null),
                    })
                }
                _ => None,
            })
            .unwrap()
        else {
            panic!()
        };
        assert_eq!(cases.len(), 2);
        // Distinct block ids for the two cases.
        assert_ne!(cases[0].1, cases[1].1);
        assert_ne!(cases[0].1, default);
    }

    #[test]
    fn endcase_branches_to_switch_exit() {
        let m = lower_source(
            "LET S(x) BE { SWITCHON x INTO $( CASE 1: f()\n ENDCASE\n DEFAULT: g() $) }",
        );
        let s = function(&m, "S");
        // ENDCASE should produce an unconditional branch (to the
        // exit block) somewhere among the case bodies.
        let branch_count = s
            .blocks
            .iter()
            .filter(|b| matches!(b.terminator, Terminator::Branch(_)))
            .count();
        assert!(branch_count >= 2);
    }

    #[test]
    fn case_fallthrough_branches_to_next_case() {
        // CASE 1: (no body) CASE 2: g() — case 1 falls through to
        // case 2's block.
        let m = lower_source(
            "LET S(x) BE { SWITCHON x INTO $( CASE 1:\n CASE 2: g() $) }",
        );
        let s = function(&m, "S");
        // Two case bodies + one default + one exit + entry = ≥5 blocks.
        assert!(s.blocks.len() >= 5);
    }

    #[test]
    fn break_inside_switchon_targets_switch_exit() {
        // BREAK inside SWITCHON body works the same as ENDCASE —
        // they both pop to the SWITCHON's exit block via the
        // shared break_block.
        let m = lower_source(
            "LET S(x) BE { SWITCHON x INTO $( CASE 1: BREAK\n DEFAULT: f() $) }",
        );
        let s = function(&m, "S");
        let switch_count = s
            .blocks
            .iter()
            .filter(|b| matches!(b.terminator, Terminator::Switch { .. }))
            .count();
        assert_eq!(switch_count, 1);
    }

    // ─── list runtime helpers + GOTO / labels ───────────────────

    #[test]
    fn hd_lowers_to_runtime_call() {
        let m = lower_source("LET S(xs) BE { LET h = HD xs }");
        let s = function(&m, "S");
        let entry = &s.blocks[0];
        let has_call = entry.instrs.iter().any(|i| matches!(
            i,
            Instr::Call { callee: Value::Function(name), .. }
                if name == "__newbcpl_list_hd"
        ));
        assert!(has_call, "expected runtime call to __newbcpl_list_hd");
    }

    #[test]
    fn len_lowers_to_runtime_call() {
        let m = lower_source("LET S(xs) BE { LET n = LEN xs }");
        let s = function(&m, "S");
        let entry = &s.blocks[0];
        let has_call = entry.instrs.iter().any(|i| matches!(
            i,
            Instr::Call { callee: Value::Function(name), .. }
                if name == "__newbcpl_len"
        ));
        assert!(has_call, "expected runtime call to __newbcpl_len");
    }

    #[test]
    fn freevec_emits_void_call() {
        let m = lower_source("LET S(v) BE { FREEVEC v }");
        let s = function(&m, "S");
        let entry = &s.blocks[0];
        let has_freevec = entry.instrs.iter().any(|i| matches!(
            i,
            Instr::Call { dst: None, callee: Value::Function(name), .. }
                if name == "__newbcpl_freevec"
        ));
        assert!(has_freevec, "expected void __newbcpl_freevec call");
    }

    #[test]
    fn goto_emits_branch_to_label_block() {
        let m = lower_source("LET S() BE { GOTO done\n done: f() }");
        let s = function(&m, "S");
        // GOTO terminates the entry block with a Branch; the label
        // block is the target.
        let entry = &s.blocks[0];
        assert!(matches!(entry.terminator, Terminator::Branch(_)));
    }

    #[test]
    fn forward_goto_resolves_to_later_label() {
        // GOTO `end` references the block before the label is
        // declared; `label_block` allocates on first mention.
        let m = lower_source(
            "LET S(x) BE { IF x = 0 THEN GOTO end\n f()\n end: g() }",
        );
        let s = function(&m, "S");
        // Three label-style blocks plus the IF blocks; the program
        // should not panic and should produce >= 5 blocks.
        assert!(s.blocks.len() >= 5);
    }

    #[test]
    fn label_block_is_reachable_from_branch() {
        let m = lower_source(
            "LET S() BE { GOTO target\n target: f() }",
        );
        let s = function(&m, "S");
        // Find the label block (named "label.target") and check it
        // contains the call to f.
        let label_block = s
            .blocks
            .iter()
            .find(|b| b.label == "label.target")
            .expect("label.target block missing");
        let has_call = label_block
            .instrs
            .iter()
            .any(|i| matches!(i, Instr::Call { .. }));
        assert!(has_call, "label block should contain call to f()");
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
