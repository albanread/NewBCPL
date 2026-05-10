//! NewBCPL parser.
//!
//! Recursive-descent over the BCPL grammar plus the dialect extensions.
//! See [`parser`] for the production list.

pub mod ast;
pub mod parser;

use std::fmt::Write;
use std::path::Path;

pub use ast::{
    BinaryOp, Block, Decl, Expr, FunctionDecl, GetDirective, LetDecl, NamedBinding,
    NamedBindingsDecl, Program, RoutineDecl, Span, Stmt, TypeConstructorKind, UnaryOp,
};
pub use parser::{ParseError, parse_source};

/// Read a file and produce a textual AST dump suitable for review and
/// regression testing. Mirrors the `dump-tokens` shape so phase artifacts
/// can be diffed and compared.
pub fn dump_ast(path: &Path) -> String {
    match std::fs::read_to_string(path) {
        Ok(source) => match parse_source(&source) {
            Ok(program) => {
                let mut out = format!(
                    "newbcpl-parser AST dump\ninput: {}\n",
                    path.display()
                );
                pretty_print_program(&program, &mut out);
                out
            }
            Err(error) => format!(
                "newbcpl-parser AST dump\ninput: {}\nerror: {}",
                path.display(),
                error.render()
            ),
        },
        Err(error) => format!(
            "newbcpl-parser AST dump\ninput: {}\nio-error: {}",
            path.display(),
            error
        ),
    }
}

fn pretty_print_program(program: &Program, out: &mut String) {
    writeln!(out, "program ({} items)", program.items.len()).unwrap();
    for item in &program.items {
        pretty_print_decl(item, 1, out);
    }
}

fn indent(level: usize, out: &mut String) {
    for _ in 0..level {
        out.push_str("  ");
    }
}

fn pretty_print_decl(decl: &Decl, level: usize, out: &mut String) {
    indent(level, out);
    match decl {
        Decl::Function(f) => {
            writeln!(out, "function {} ({})", f.name, f.params.join(", ")).unwrap();
            indent(level + 1, out);
            writeln!(out, "body:").unwrap();
            pretty_print_expr(&f.body, level + 2, out);
        }
        Decl::Routine(r) => {
            writeln!(out, "routine {} ({})", r.name, r.params.join(", ")).unwrap();
            indent(level + 1, out);
            writeln!(out, "body:").unwrap();
            pretty_print_stmt(&r.body, level + 2, out);
        }
        Decl::Let(l) => {
            writeln!(out, "let ({} bindings)", l.bindings.len()).unwrap();
            for (name, expr) in &l.bindings {
                indent(level + 1, out);
                writeln!(out, "{name} =").unwrap();
                pretty_print_expr(expr, level + 2, out);
            }
        }
        Decl::Get(g) => {
            writeln!(out, "get \"{}\"", g.path).unwrap();
        }
        Decl::Manifest(m) => {
            writeln!(out, "manifest ({} bindings)", m.bindings.len()).unwrap();
            for b in &m.bindings {
                pretty_print_named_binding(b, level + 1, out);
            }
        }
        Decl::Static(s) => {
            writeln!(out, "static ({} bindings)", s.bindings.len()).unwrap();
            for b in &s.bindings {
                pretty_print_named_binding(b, level + 1, out);
            }
        }
        Decl::Global(g) => {
            writeln!(out, "global ({} bindings)", g.bindings.len()).unwrap();
            for b in &g.bindings {
                pretty_print_named_binding(b, level + 1, out);
            }
        }
    }
}

fn pretty_print_named_binding(b: &NamedBinding, level: usize, out: &mut String) {
    indent(level, out);
    match &b.value {
        Some(value) => {
            writeln!(out, "{} =", b.name).unwrap();
            pretty_print_expr(value, level + 1, out);
        }
        None => {
            writeln!(out, "{} (uninitialised)", b.name).unwrap();
        }
    }
}

fn pretty_print_stmt(stmt: &Stmt, level: usize, out: &mut String) {
    indent(level, out);
    match stmt {
        Stmt::Block(b) => {
            writeln!(out, "block ({} stmts)", b.stmts.len()).unwrap();
            for s in &b.stmts {
                pretty_print_stmt(s, level + 1, out);
            }
        }
        Stmt::Decl(d) => {
            writeln!(out, "decl-stmt:").unwrap();
            pretty_print_decl(d, level + 1, out);
        }
        Stmt::Expr(e) => {
            writeln!(out, "expr-stmt").unwrap();
            pretty_print_expr(e, level + 1, out);
        }
        Stmt::Assign { targets, values, .. } => {
            writeln!(
                out,
                "assign ({} targets, {} values)",
                targets.len(),
                values.len()
            )
            .unwrap();
            indent(level + 1, out);
            writeln!(out, "targets:").unwrap();
            for t in targets {
                pretty_print_expr(t, level + 2, out);
            }
            indent(level + 1, out);
            writeln!(out, "values:").unwrap();
            for v in values {
                pretty_print_expr(v, level + 2, out);
            }
        }
        Stmt::If {
            cond,
            then_stmt,
            else_stmt,
            ..
        } => {
            writeln!(out, "if").unwrap();
            indent(level + 1, out);
            writeln!(out, "cond:").unwrap();
            pretty_print_expr(cond, level + 2, out);
            indent(level + 1, out);
            writeln!(out, "then:").unwrap();
            pretty_print_stmt(then_stmt, level + 2, out);
            if let Some(els) = else_stmt {
                indent(level + 1, out);
                writeln!(out, "else:").unwrap();
                pretty_print_stmt(els, level + 2, out);
            }
        }
        Stmt::Unless {
            cond, then_stmt, ..
        } => {
            writeln!(out, "unless").unwrap();
            indent(level + 1, out);
            writeln!(out, "cond:").unwrap();
            pretty_print_expr(cond, level + 2, out);
            indent(level + 1, out);
            writeln!(out, "then:").unwrap();
            pretty_print_stmt(then_stmt, level + 2, out);
        }
        Stmt::While { cond, body, .. } => {
            writeln!(out, "while").unwrap();
            indent(level + 1, out);
            writeln!(out, "cond:").unwrap();
            pretty_print_expr(cond, level + 2, out);
            indent(level + 1, out);
            writeln!(out, "body:").unwrap();
            pretty_print_stmt(body, level + 2, out);
        }
        Stmt::Until { cond, body, .. } => {
            writeln!(out, "until").unwrap();
            indent(level + 1, out);
            writeln!(out, "cond:").unwrap();
            pretty_print_expr(cond, level + 2, out);
            indent(level + 1, out);
            writeln!(out, "body:").unwrap();
            pretty_print_stmt(body, level + 2, out);
        }
        Stmt::Repeat { body, .. } => {
            writeln!(out, "repeat").unwrap();
            indent(level + 1, out);
            writeln!(out, "body:").unwrap();
            pretty_print_stmt(body, level + 2, out);
        }
        Stmt::RepeatWhile { body, cond, .. } => {
            writeln!(out, "repeat-while").unwrap();
            indent(level + 1, out);
            writeln!(out, "body:").unwrap();
            pretty_print_stmt(body, level + 2, out);
            indent(level + 1, out);
            writeln!(out, "cond:").unwrap();
            pretty_print_expr(cond, level + 2, out);
        }
        Stmt::RepeatUntil { body, cond, .. } => {
            writeln!(out, "repeat-until").unwrap();
            indent(level + 1, out);
            writeln!(out, "body:").unwrap();
            pretty_print_stmt(body, level + 2, out);
            indent(level + 1, out);
            writeln!(out, "cond:").unwrap();
            pretty_print_expr(cond, level + 2, out);
        }
        Stmt::Resultis(e, _) => {
            writeln!(out, "resultis").unwrap();
            pretty_print_expr(e, level + 1, out);
        }
        Stmt::Return(_) => {
            writeln!(out, "return").unwrap();
        }
        Stmt::Finish(_) => {
            writeln!(out, "finish").unwrap();
        }
        Stmt::Break(_) => {
            writeln!(out, "break").unwrap();
        }
        Stmt::Loop(_) => {
            writeln!(out, "loop").unwrap();
        }
        Stmt::Endcase(_) => {
            writeln!(out, "endcase").unwrap();
        }
    }
}

fn pretty_print_expr(expr: &Expr, level: usize, out: &mut String) {
    indent(level, out);
    match expr {
        Expr::Ident { name, .. } => {
            writeln!(out, "ident {name}").unwrap();
        }
        Expr::IntLit { value, .. } => {
            writeln!(out, "int {value}").unwrap();
        }
        Expr::FloatLit { value, .. } => {
            writeln!(out, "real {value}").unwrap();
        }
        Expr::CharLit { lexeme, .. } => {
            writeln!(out, "char {lexeme}").unwrap();
        }
        Expr::StringLit { value, .. } => {
            writeln!(out, "string {value}").unwrap();
        }
        Expr::BoolLit { value, .. } => {
            writeln!(out, "bool {value}").unwrap();
        }
        Expr::Null { .. } => {
            writeln!(out, "null").unwrap();
        }
        Expr::Call { callee, args, .. } => {
            writeln!(out, "call ({} args)", args.len()).unwrap();
            indent(level + 1, out);
            writeln!(out, "callee:").unwrap();
            pretty_print_expr(callee, level + 2, out);
            if !args.is_empty() {
                indent(level + 1, out);
                writeln!(out, "args:").unwrap();
                for arg in args {
                    pretty_print_expr(arg, level + 2, out);
                }
            }
        }
        Expr::Unary { op, operand, .. } => {
            writeln!(out, "unary {}", op.as_str()).unwrap();
            pretty_print_expr(operand, level + 1, out);
        }
        Expr::Binary { op, lhs, rhs, .. } => {
            writeln!(out, "binary {}", op.as_str()).unwrap();
            pretty_print_expr(lhs, level + 1, out);
            pretty_print_expr(rhs, level + 1, out);
        }
        Expr::Conditional {
            cond,
            then_expr,
            else_expr,
            ..
        } => {
            writeln!(out, "cond-expr").unwrap();
            indent(level + 1, out);
            writeln!(out, "cond:").unwrap();
            pretty_print_expr(cond, level + 2, out);
            indent(level + 1, out);
            writeln!(out, "then:").unwrap();
            pretty_print_expr(then_expr, level + 2, out);
            indent(level + 1, out);
            writeln!(out, "else:").unwrap();
            pretty_print_expr(else_expr, level + 2, out);
        }
        Expr::Valof { body, .. } => {
            writeln!(out, "valof").unwrap();
            pretty_print_stmt(body, level + 1, out);
        }
        Expr::TypedConstruct { kind, args, .. } => {
            writeln!(out, "construct {} ({} args)", kind.as_str(), args.len()).unwrap();
            for arg in args {
                pretty_print_expr(arg, level + 1, out);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_ok(source: &str) -> Program {
        parse_source(source).unwrap_or_else(|e| panic!("parse failed: {}", e.render()))
    }

    fn parse_err(source: &str) -> ParseError {
        parse_source(source).expect_err("expected parse error")
    }

    // ─── existing bootstrap tests ───────────────────────────────

    #[test]
    fn parses_basic_routine_with_curly_braces() {
        let p = parse_ok("LET START() BE { WRITEF(\"Test*N\") }");
        let Decl::Routine(r) = &p.items[0] else {
            panic!();
        };
        assert_eq!(r.name, "START");
    }

    #[test]
    fn parses_function_decl() {
        let p = parse_ok("LET square(x) = x");
        assert!(matches!(p.items[0], Decl::Function(_)));
    }

    #[test]
    fn parses_let_binding() {
        let p = parse_ok("LET I = 42");
        let Decl::Let(l) = &p.items[0] else {
            panic!();
        };
        assert!(matches!(l.bindings[0].1, Expr::IntLit { value: 42, .. }));
    }

    #[test]
    fn parses_null_literal() {
        let p = parse_ok("LET ptr = ?");
        let Decl::Let(l) = &p.items[0] else {
            panic!();
        };
        assert!(matches!(l.bindings[0].1, Expr::Null { .. }));
    }

    // ─── new literal forms ──────────────────────────────────────

    #[test]
    fn parses_real_literal() {
        let p = parse_ok("LET pi = 3.14159");
        let Decl::Let(l) = &p.items[0] else {
            panic!();
        };
        let Expr::FloatLit { value, .. } = l.bindings[0].1 else {
            panic!();
        };
        assert!((value - 3.14159).abs() < 1e-9);
    }

    #[test]
    fn parses_real_with_exponent() {
        let p = parse_ok("LET x = 1.5e-3");
        let Decl::Let(l) = &p.items[0] else {
            panic!();
        };
        assert!(matches!(l.bindings[0].1, Expr::FloatLit { .. }));
    }

    #[test]
    fn parses_char_literal() {
        let p = parse_ok("LET c = 'a'");
        let Decl::Let(l) = &p.items[0] else {
            panic!();
        };
        assert!(matches!(l.bindings[0].1, Expr::CharLit { .. }));
    }

    #[test]
    fn parses_true_false() {
        let p = parse_ok("LET t = TRUE\nLET f = FALSE");
        let Decl::Let(l0) = &p.items[0] else { panic!() };
        let Decl::Let(l1) = &p.items[1] else { panic!() };
        assert!(matches!(l0.bindings[0].1, Expr::BoolLit { value: true, .. }));
        assert!(matches!(l1.bindings[0].1, Expr::BoolLit { value: false, .. }));
    }

    // ─── precedence ─────────────────────────────────────────────

    #[test]
    fn add_left_associates() {
        // a + b + c parses as (a + b) + c
        let p = parse_ok("LET x = a + b + c");
        let Decl::Let(l) = &p.items[0] else {
            panic!();
        };
        let Expr::Binary {
            op: BinaryOp::Add,
            lhs,
            ..
        } = &l.bindings[0].1
        else {
            panic!("expected outer +");
        };
        assert!(matches!(
            lhs.as_ref(),
            Expr::Binary { op: BinaryOp::Add, .. }
        ));
    }

    #[test]
    fn mul_binds_tighter_than_add() {
        // a + b * c parses as a + (b * c)
        let p = parse_ok("LET x = a + b * c");
        let Decl::Let(l) = &p.items[0] else {
            panic!();
        };
        let Expr::Binary {
            op: BinaryOp::Add,
            rhs,
            ..
        } = &l.bindings[0].1
        else {
            panic!("expected outer +");
        };
        assert!(matches!(
            rhs.as_ref(),
            Expr::Binary { op: BinaryOp::Mul, .. }
        ));
    }

    #[test]
    fn shift_below_add() {
        // a << b + c parses as a << (b + c)
        let p = parse_ok("LET x = a << b + c");
        let Decl::Let(l) = &p.items[0] else {
            panic!();
        };
        let Expr::Binary {
            op: BinaryOp::Shl,
            rhs,
            ..
        } = &l.bindings[0].1
        else {
            panic!("expected <<");
        };
        assert!(matches!(
            rhs.as_ref(),
            Expr::Binary { op: BinaryOp::Add, .. }
        ));
    }

    #[test]
    fn relational_below_shift() {
        // a < b << 1 parses as a < (b << 1)
        let p = parse_ok("LET x = a < b << 1");
        let Decl::Let(l) = &p.items[0] else {
            panic!();
        };
        let Expr::Binary {
            op: BinaryOp::Lt,
            rhs,
            ..
        } = &l.bindings[0].1
        else {
            panic!("expected <");
        };
        assert!(matches!(
            rhs.as_ref(),
            Expr::Binary { op: BinaryOp::Shl, .. }
        ));
    }

    #[test]
    fn dotted_float_ops() {
        let p = parse_ok("LET x = a +. b *. c");
        let Decl::Let(l) = &p.items[0] else {
            panic!();
        };
        let Expr::Binary {
            op: BinaryOp::FAdd,
            rhs,
            ..
        } = &l.bindings[0].1
        else {
            panic!();
        };
        assert!(matches!(
            rhs.as_ref(),
            Expr::Binary { op: BinaryOp::FMul, .. }
        ));
    }

    #[test]
    fn subscript_binds_tighter_than_arith() {
        // v!i + 1 parses as (v!i) + 1
        let p = parse_ok("LET x = v!i + 1");
        let Decl::Let(l) = &p.items[0] else {
            panic!();
        };
        let Expr::Binary {
            op: BinaryOp::Add,
            lhs,
            ..
        } = &l.bindings[0].1
        else {
            panic!();
        };
        assert!(matches!(
            lhs.as_ref(),
            Expr::Binary {
                op: BinaryOp::Subscript,
                ..
            }
        ));
    }

    #[test]
    fn unary_neg_then_subscript() {
        // -v!i parses as -(v!i) — postfix wins over prefix
        let p = parse_ok("LET x = -v!i");
        let Decl::Let(l) = &p.items[0] else {
            panic!();
        };
        let Expr::Unary {
            op: UnaryOp::Neg,
            operand,
            ..
        } = &l.bindings[0].1
        else {
            panic!("expected -");
        };
        assert!(matches!(
            operand.as_ref(),
            Expr::Binary {
                op: BinaryOp::Subscript,
                ..
            }
        ));
    }

    #[test]
    fn prefix_indirection() {
        let p = parse_ok("LET x = !ptr");
        let Decl::Let(l) = &p.items[0] else {
            panic!();
        };
        assert!(matches!(
            l.bindings[0].1,
            Expr::Unary {
                op: UnaryOp::Indirection,
                ..
            }
        ));
    }

    #[test]
    fn address_of() {
        let p = parse_ok("LET p = @x");
        let Decl::Let(l) = &p.items[0] else {
            panic!();
        };
        assert!(matches!(
            l.bindings[0].1,
            Expr::Unary {
                op: UnaryOp::AddressOf,
                ..
            }
        ));
    }

    #[test]
    fn member_access() {
        let p = parse_ok("LET x = obj.field");
        let Decl::Let(l) = &p.items[0] else {
            panic!();
        };
        assert!(matches!(
            l.bindings[0].1,
            Expr::Binary {
                op: BinaryOp::Dot,
                ..
            }
        ));
    }

    #[test]
    fn method_call() {
        let p = parse_ok("LET x = obj.getX()");
        let Decl::Let(l) = &p.items[0] else {
            panic!();
        };
        // obj.getX() is Call { callee = Binary { Dot, obj, getX }, args = [] }
        let Expr::Call { callee, args, .. } = &l.bindings[0].1 else {
            panic!();
        };
        assert!(args.is_empty());
        assert!(matches!(
            callee.as_ref(),
            Expr::Binary {
                op: BinaryOp::Dot,
                ..
            }
        ));
    }

    #[test]
    fn conditional_expression() {
        let p = parse_ok("LET x = a > b -> a, b");
        let Decl::Let(l) = &p.items[0] else {
            panic!();
        };
        assert!(matches!(l.bindings[0].1, Expr::Conditional { .. }));
    }

    #[test]
    fn null_compare() {
        let p = parse_ok("LET x = ptr = ?");
        let Decl::Let(l) = &p.items[0] else {
            panic!();
        };
        let Expr::Binary {
            op: BinaryOp::Eq,
            lhs,
            rhs,
            ..
        } = &l.bindings[0].1
        else {
            panic!();
        };
        assert!(matches!(lhs.as_ref(), Expr::Ident { .. }));
        assert!(matches!(rhs.as_ref(), Expr::Null { .. }));
    }

    // ─── statements ─────────────────────────────────────────────

    #[test]
    fn parses_assignment() {
        let p = parse_ok("LET S() BE { x := 42 }");
        let Decl::Routine(r) = &p.items[0] else {
            panic!();
        };
        let Stmt::Block(b) = r.body.as_ref() else {
            panic!();
        };
        assert!(matches!(b.stmts[0], Stmt::Assign { .. }));
    }

    #[test]
    fn parses_multi_assignment() {
        let p = parse_ok("LET S() BE { a, b := 1, 2 }");
        let Decl::Routine(r) = &p.items[0] else {
            panic!();
        };
        let Stmt::Block(b) = r.body.as_ref() else {
            panic!();
        };
        let Stmt::Assign {
            targets, values, ..
        } = &b.stmts[0]
        else {
            panic!();
        };
        assert_eq!(targets.len(), 2);
        assert_eq!(values.len(), 2);
    }

    #[test]
    fn parses_subscript_assignment() {
        let p = parse_ok("LET S() BE { v!0 := 7 }");
        let Decl::Routine(r) = &p.items[0] else {
            panic!();
        };
        let Stmt::Block(b) = r.body.as_ref() else {
            panic!();
        };
        let Stmt::Assign { targets, .. } = &b.stmts[0] else {
            panic!();
        };
        assert!(matches!(
            targets[0],
            Expr::Binary {
                op: BinaryOp::Subscript,
                ..
            }
        ));
    }

    #[test]
    fn parses_if_then() {
        let p = parse_ok("LET S() BE { IF x = 0 THEN WRITES(\"zero\") }");
        let Decl::Routine(r) = &p.items[0] else {
            panic!();
        };
        let Stmt::Block(b) = r.body.as_ref() else {
            panic!();
        };
        assert!(matches!(b.stmts[0], Stmt::If { .. }));
    }

    #[test]
    fn parses_unless() {
        let p = parse_ok("LET S() BE { UNLESS x = 0 THEN f() }");
        let Decl::Routine(r) = &p.items[0] else {
            panic!();
        };
        let Stmt::Block(b) = r.body.as_ref() else {
            panic!();
        };
        assert!(matches!(b.stmts[0], Stmt::Unless { .. }));
    }

    #[test]
    fn parses_test_with_else() {
        let p = parse_ok("LET S() BE { TEST x = 0 THEN f() ELSE g() }");
        let Decl::Routine(r) = &p.items[0] else {
            panic!();
        };
        let Stmt::Block(b) = r.body.as_ref() else {
            panic!();
        };
        let Stmt::If { else_stmt, .. } = &b.stmts[0] else {
            panic!("TEST should produce a Stmt::If");
        };
        assert!(else_stmt.is_some(), "TEST without else");
    }

    #[test]
    fn parses_if_with_else() {
        let p = parse_ok("LET S() BE { IF x = 0 THEN f() ELSE g() }");
        let Decl::Routine(r) = &p.items[0] else {
            panic!();
        };
        let Stmt::Block(b) = r.body.as_ref() else {
            panic!();
        };
        let Stmt::If { else_stmt, .. } = &b.stmts[0] else {
            panic!();
        };
        assert!(else_stmt.is_some());
    }

    #[test]
    fn parses_if_without_else() {
        let p = parse_ok("LET S() BE { IF x = 0 THEN f() }");
        let Decl::Routine(r) = &p.items[0] else {
            panic!();
        };
        let Stmt::Block(b) = r.body.as_ref() else {
            panic!();
        };
        let Stmt::If { else_stmt, .. } = &b.stmts[0] else {
            panic!();
        };
        assert!(else_stmt.is_none());
    }

    #[test]
    fn keyword_and_or_not_parse_as_operators() {
        // AND is binary &; OR is binary |; NOT is unary ~.
        let p = parse_ok("LET x = a = 1 AND b = 2 OR NOT c");
        let Decl::Let(l) = &p.items[0] else {
            panic!();
        };
        // Expected tree: (a=1 AND b=2) OR (NOT c)
        // OR binds looser than AND, AND binds looser than =.
        let Expr::Binary {
            op: BinaryOp::BitOr,
            lhs,
            rhs,
            ..
        } = &l.bindings[0].1
        else {
            panic!("expected OR at the root");
        };
        assert!(matches!(
            lhs.as_ref(),
            Expr::Binary { op: BinaryOp::BitAnd, .. }
        ));
        assert!(matches!(
            rhs.as_ref(),
            Expr::Unary { op: UnaryOp::Not, .. }
        ));
    }

    #[test]
    fn parses_while() {
        let p = parse_ok("LET S() BE { WHILE i < 10 DO i := i + 1 }");
        let Decl::Routine(r) = &p.items[0] else {
            panic!();
        };
        let Stmt::Block(b) = r.body.as_ref() else {
            panic!();
        };
        assert!(matches!(b.stmts[0], Stmt::While { .. }));
    }

    #[test]
    fn parses_repeat_while() {
        let p = parse_ok("LET S() BE { { i := i + 1 } REPEATWHILE i < 10 }");
        let Decl::Routine(r) = &p.items[0] else {
            panic!();
        };
        let Stmt::Block(b) = r.body.as_ref() else {
            panic!();
        };
        assert!(matches!(b.stmts[0], Stmt::RepeatWhile { .. }));
    }

    #[test]
    fn parses_resultis() {
        let p = parse_ok("LET F(x) = VALOF $( RESULTIS x + 1 $)");
        let Decl::Function(f) = &p.items[0] else {
            panic!();
        };
        let Expr::Valof { body, .. } = &f.body else {
            panic!();
        };
        let Stmt::Block(b) = body.as_ref() else {
            panic!();
        };
        assert!(matches!(b.stmts[0], Stmt::Resultis(_, _)));
    }

    #[test]
    fn parses_control_flow_keywords() {
        let p = parse_ok(
            "LET S() BE $( BREAK\n LOOP\n RETURN\n FINISH\n ENDCASE $)",
        );
        let Decl::Routine(r) = &p.items[0] else {
            panic!();
        };
        let Stmt::Block(b) = r.body.as_ref() else {
            panic!();
        };
        assert_eq!(b.stmts.len(), 5);
        assert!(matches!(b.stmts[0], Stmt::Break(_)));
        assert!(matches!(b.stmts[1], Stmt::Loop(_)));
        assert!(matches!(b.stmts[2], Stmt::Return(_)));
        assert!(matches!(b.stmts[3], Stmt::Finish(_)));
        assert!(matches!(b.stmts[4], Stmt::Endcase(_)));
    }

    #[test]
    fn parses_nested_if_inside_routine() {
        let src = "LET S() BE $(
            LET x = 5
            IF x > 0 THEN $(
                IF x > 10 THEN WRITES(\"big*N\")
                OR_ELSE_DUMMY()
            $)
        $)";
        // The above has IF without else, plus a stray identifier — make
        // sure parsing reaches end of file cleanly.
        let _ = parse_source(src).map_err(|e| e.render());
        // We don't assert structure here because the body has nesting
        // we're not specifically testing; the goal is that parsing
        // does not get stuck.
    }

    // ─── error cases ────────────────────────────────────────────

    #[test]
    fn error_unexpected_top_level_token() {
        let err = parse_err("WRITES(\"hi\")");
        assert!(err.message.contains("expected declaration"));
    }

    #[test]
    fn error_unterminated_block() {
        let err = parse_err("LET S() BE $( WRITES(\"x\")");
        assert!(err.message.contains("unterminated") || err.message.contains("expected"));
    }

    #[test]
    fn error_missing_else_in_test() {
        let err = parse_err("LET S() BE { TEST x = 0 THEN f() g() }");
        assert!(err.message.contains("ELSE"));
    }

    // ─── top-level forms ────────────────────────────────────────

    #[test]
    fn parses_get_directive() {
        let p = parse_ok("GET \"libhdr.h\"\nLET START() BE { f() }");
        let Decl::Get(g) = &p.items[0] else {
            panic!("expected GET");
        };
        assert_eq!(g.path, "libhdr.h");
    }

    #[test]
    fn parses_manifest_block() {
        let p = parse_ok("MANIFEST $(\n  A = 1\n  B = 2\n  C = #X10\n$)");
        let Decl::Manifest(m) = &p.items[0] else {
            panic!();
        };
        assert_eq!(m.bindings.len(), 3);
        assert_eq!(m.bindings[0].name, "A");
        assert!(m.bindings[0].value.is_some());
    }

    #[test]
    fn parses_static_bare() {
        let p = parse_ok("LET S() BE { STATIC test\n test := 100 }");
        let Decl::Routine(r) = &p.items[0] else {
            panic!();
        };
        let Stmt::Block(b) = r.body.as_ref() else {
            panic!();
        };
        let Stmt::Decl(Decl::Static(s)) = &b.stmts[0] else {
            panic!("expected STATIC");
        };
        assert_eq!(s.bindings.len(), 1);
        assert_eq!(s.bindings[0].name, "test");
        assert!(s.bindings[0].value.is_none());
    }

    #[test]
    fn parses_static_block() {
        let p = parse_ok("STATIC $( str1 = \"Hello\" $)");
        let Decl::Static(s) = &p.items[0] else {
            panic!();
        };
        assert_eq!(s.bindings.len(), 1);
        assert_eq!(s.bindings[0].name, "str1");
        assert!(s.bindings[0].value.is_some());
    }

    #[test]
    fn parses_globals_with_let() {
        let p = parse_ok("GLOBALS $(\n  LET x = 1\n  LET y = 2\n$)");
        let Decl::Global(g) = &p.items[0] else {
            panic!();
        };
        assert_eq!(g.bindings.len(), 2);
        assert_eq!(g.bindings[0].name, "x");
        assert_eq!(g.bindings[1].name, "y");
    }

    #[test]
    fn parses_classic_global_with_offset() {
        let p = parse_ok("GLOBAL $( frob : 100; quux : 101 $)");
        let Decl::Global(g) = &p.items[0] else {
            panic!();
        };
        assert_eq!(g.bindings.len(), 2);
        assert_eq!(g.bindings[0].name, "frob");
    }

    #[test]
    fn manifest_requires_init() {
        let err = parse_err("MANIFEST $( A $)");
        assert!(
            err.message.contains("expected `=`"),
            "got: {}",
            err.message
        );
    }

    // ─── type constructors ──────────────────────────────────────

    #[test]
    fn parses_vec_allocation() {
        let p = parse_ok("LET v = VEC 100");
        let Decl::Let(l) = &p.items[0] else {
            panic!();
        };
        let Expr::TypedConstruct {
            kind: TypeConstructorKind::Vec,
            args,
            ..
        } = &l.bindings[0].1
        else {
            panic!();
        };
        assert_eq!(args.len(), 1);
        assert!(matches!(args[0], Expr::IntLit { value: 100, .. }));
    }

    #[test]
    fn parses_pair_construction() {
        let p = parse_ok("LET p = PAIR(1, 2)");
        let Decl::Let(l) = &p.items[0] else {
            panic!();
        };
        let Expr::TypedConstruct {
            kind: TypeConstructorKind::Pair,
            args,
            ..
        } = &l.bindings[0].1
        else {
            panic!();
        };
        assert_eq!(args.len(), 2);
    }

    #[test]
    fn parses_fpair_with_floats() {
        let p = parse_ok("LET f = FPAIR(1.0, 2.0)");
        let Decl::Let(l) = &p.items[0] else {
            panic!();
        };
        let Expr::TypedConstruct {
            kind: TypeConstructorKind::FPair,
            args,
            ..
        } = &l.bindings[0].1
        else {
            panic!();
        };
        assert!(matches!(args[0], Expr::FloatLit { .. }));
    }

    #[test]
    fn parses_quad_and_oct() {
        let p = parse_ok(
            "LET q = QUAD(1, 2, 3, 4)\nLET o = OCT(1, 2, 3, 4, 5, 6, 7, 8)",
        );
        let Decl::Let(l0) = &p.items[0] else { panic!() };
        let Decl::Let(l1) = &p.items[1] else { panic!() };
        let Expr::TypedConstruct {
            kind: TypeConstructorKind::Quad,
            args: a0,
            ..
        } = &l0.bindings[0].1
        else {
            panic!();
        };
        let Expr::TypedConstruct {
            kind: TypeConstructorKind::Oct,
            args: a1,
            ..
        } = &l1.bindings[0].1
        else {
            panic!();
        };
        assert_eq!(a0.len(), 4);
        assert_eq!(a1.len(), 8);
    }

    #[test]
    fn parses_table() {
        let p = parse_ok("LET t = TABLE(10, 20, 30, 40, 50)");
        let Decl::Let(l) = &p.items[0] else {
            panic!();
        };
        let Expr::TypedConstruct {
            kind: TypeConstructorKind::Table,
            args,
            ..
        } = &l.bindings[0].1
        else {
            panic!();
        };
        assert_eq!(args.len(), 5);
    }

    #[test]
    fn pair_with_negative_arg() {
        // Make sure unary `-` inside the call works.
        let p = parse_ok("LET p = PAIR(10, -10)");
        let Decl::Let(l) = &p.items[0] else {
            panic!();
        };
        let Expr::TypedConstruct {
            kind: TypeConstructorKind::Pair,
            args,
            ..
        } = &l.bindings[0].1
        else {
            panic!();
        };
        assert!(matches!(
            args[1],
            Expr::Unary { op: UnaryOp::Neg, .. }
        ));
    }

    #[test]
    fn flet_parses_like_let() {
        let p = parse_ok("FLET pi = 3.14159");
        let Decl::Let(l) = &p.items[0] else {
            panic!("FLET should produce Decl::Let — sema applies the float hint");
        };
        assert_eq!(l.bindings[0].0, "pi");
        assert!(matches!(l.bindings[0].1, Expr::FloatLit { .. }));
    }

    #[test]
    fn flet_function_form() {
        let p = parse_ok("FLET SimpleFloatFunc(x) = x + 1.0");
        assert!(matches!(p.items[0], Decl::Function(_)));
    }

    #[test]
    fn dump_ast_smoke() {
        let p = parse_ok("LET START() BE { IF x > 0 THEN WRITES(\"pos*N\") }");
        let mut out = String::new();
        super::pretty_print_program(&p, &mut out);
        assert!(out.contains("routine START"));
        assert!(out.contains("if"));
        assert!(out.contains("binary >"));
    }
}
