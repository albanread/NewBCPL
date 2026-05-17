//! NewBCPL parser.
//!
//! Recursive-descent over the BCPL grammar plus the dialect extensions.
//! See [`parser`] for the production list.

pub mod ast;
pub mod parser;

use std::fmt::Write;
use std::path::Path;

pub use ast::{
    AsmProcDecl, BinaryOp, Block, ClassDecl, ClassMember, ClassMemberKind, ClassMethod,
    ClassMethodBody, Decl, Expr, FunctionDecl, GetDirective, LetDecl, LetKind, NamedBinding,
    NamedBindingsDecl, Program, RoutineDecl, Span, Stmt, SwitchCase, TypeConstructorKind,
    TypeHint, UnaryOp, Visibility, unknown_hint,
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
        Decl::Class(c) => {
            write!(out, "class {}", c.name).unwrap();
            if let Some(base) = &c.extends {
                write!(out, " extends {base}").unwrap();
            }
            if c.managed {
                write!(out, " MANAGED").unwrap();
            }
            writeln!(out, " ({} members)", c.members.len()).unwrap();
            for m in &c.members {
                pretty_print_class_member(m, level + 1, out);
            }
        }
        Decl::AsmProc(a) => {
            let kind = if a.is_function { "asm-function" } else { "asm-procedure" };
            writeln!(out, "{kind} {} ({})", a.name, a.params.join(", ")).unwrap();
        }
    }
}

fn pretty_print_class_member(m: &ClassMember, level: usize, out: &mut String) {
    indent(level, out);
    let vis = match m.visibility {
        Visibility::Public => "public",
        Visibility::Private => "private",
        Visibility::Protected => "protected",
    };
    match &m.kind {
        ClassMemberKind::Fields { names, annotations } => {
            let parts: Vec<String> = names
                .iter()
                .zip(annotations.iter())
                .map(|(n, ann)| match ann {
                    Some(t) => format!("{n} AS {t}"),
                    None => n.clone(),
                })
                .collect();
            writeln!(out, "{vis} decl {}", parts.join(", ")).unwrap();
        }
        ClassMemberKind::Let(let_decl) => {
            writeln!(out, "{vis} let ({} bindings)", let_decl.bindings.len()).unwrap();
            for (name, expr) in &let_decl.bindings {
                indent(level + 1, out);
                writeln!(out, "{name} =").unwrap();
                pretty_print_expr(expr, level + 2, out);
            }
        }
        ClassMemberKind::FLet(b) => match &b.value {
            Some(value) => {
                writeln!(out, "{vis} flet {} =", b.name).unwrap();
                pretty_print_expr(value, level + 1, out);
            }
            None => {
                writeln!(out, "{vis} flet {} (uninitialised)", b.name).unwrap();
            }
        },
        ClassMemberKind::Method(m) => {
            let kw = if matches!(m.body, ClassMethodBody::Function(_)) {
                "function"
            } else {
                "routine"
            };
            let mut prefix = String::new();
            if m.is_virtual {
                prefix.push_str("virtual ");
            }
            if m.is_final {
                prefix.push_str("final ");
            }
            writeln!(
                out,
                "{vis} {prefix}{kw} {} ({})",
                m.name,
                m.params.join(", ")
            )
            .unwrap();
            indent(level + 1, out);
            writeln!(out, "body:").unwrap();
            match &m.body {
                ClassMethodBody::Routine(s) => pretty_print_stmt(s, level + 2, out),
                ClassMethodBody::Function(e) => pretty_print_expr(e, level + 2, out),
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
        Stmt::For {
            name,
            start,
            end,
            step,
            body,
            ..
        } => {
            writeln!(out, "for {name}").unwrap();
            indent(level + 1, out);
            writeln!(out, "from:").unwrap();
            pretty_print_expr(start, level + 2, out);
            indent(level + 1, out);
            writeln!(out, "to:").unwrap();
            pretty_print_expr(end, level + 2, out);
            if let Some(step) = step {
                indent(level + 1, out);
                writeln!(out, "by:").unwrap();
                pretty_print_expr(step, level + 2, out);
            }
            indent(level + 1, out);
            writeln!(out, "body:").unwrap();
            pretty_print_stmt(body, level + 2, out);
        }
        Stmt::ForEach {
            names,
            annotation,
            iter,
            body,
            ..
        } => {
            write!(out, "foreach {}", names.join(", ")).unwrap();
            if let Some(ann) = annotation {
                write!(out, " : {ann}").unwrap();
            }
            writeln!(out).unwrap();
            indent(level + 1, out);
            writeln!(out, "in:").unwrap();
            pretty_print_expr(iter, level + 2, out);
            indent(level + 1, out);
            writeln!(out, "body:").unwrap();
            pretty_print_stmt(body, level + 2, out);
        }
        Stmt::Switchon {
            scrutinee,
            cases,
            default,
            ..
        } => {
            writeln!(
                out,
                "switchon ({} cases{})",
                cases.len(),
                if default.is_some() { ", default" } else { "" }
            )
            .unwrap();
            indent(level + 1, out);
            writeln!(out, "scrutinee:").unwrap();
            pretty_print_expr(scrutinee, level + 2, out);
            for case in cases {
                indent(level + 1, out);
                writeln!(out, "case ({} labels):", case.values.len()).unwrap();
                for v in &case.values {
                    pretty_print_expr(v, level + 2, out);
                }
                indent(level + 2, out);
                writeln!(out, "body ({} stmts):", case.body.len()).unwrap();
                for s in &case.body {
                    pretty_print_stmt(s, level + 3, out);
                }
            }
            if let Some(body) = default {
                indent(level + 1, out);
                writeln!(out, "default ({} stmts):", body.len()).unwrap();
                for s in body {
                    pretty_print_stmt(s, level + 2, out);
                }
            }
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
        Stmt::Brk(_) => {
            writeln!(out, "brk").unwrap();
        }
        Stmt::Goto { label, .. } => {
            writeln!(out, "goto {label}").unwrap();
        }
        Stmt::Label { name, .. } => {
            writeln!(out, "label {name}:").unwrap();
        }
        Stmt::Retain { name, value, .. } => match value {
            Some(expr) => {
                writeln!(out, "retain {name} =").unwrap();
                pretty_print_expr(expr, level + 1, out);
            }
            None => {
                writeln!(out, "retain {name}").unwrap();
            }
        },
        Stmt::Using { name, value, body, .. } => {
            writeln!(out, "using {name} =").unwrap();
            pretty_print_expr(value, level + 1, out);
            indent(level, out);
            writeln!(out, "do:").unwrap();
            pretty_print_stmt(body, level + 1, out);
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
        Expr::New {
            class_name, args, ..
        } => {
            writeln!(out, "new {class_name} ({} args)", args.len()).unwrap();
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
        // Per the manifesto / user guide: `AND` / `OR` / `NOT` are the
        // *logical* operators (LogAnd / LogOr / LogNot). The bitwise
        // forms are `BAND` / `BOR` / `BNOT` (or `&` / `|` / `~`).
        let p = parse_ok("LET x = a = 1 AND b = 2 OR NOT c");
        let Decl::Let(l) = &p.items[0] else {
            panic!();
        };
        // Expected tree: (a=1 AND b=2) OR (NOT c)
        // OR binds looser than AND, AND binds looser than =.
        let Expr::Binary {
            op: BinaryOp::LogOr,
            lhs,
            rhs,
            ..
        } = &l.bindings[0].1
        else {
            panic!("expected logical OR at the root");
        };
        assert!(matches!(
            lhs.as_ref(),
            Expr::Binary { op: BinaryOp::LogAnd, .. }
        ));
        assert!(matches!(
            rhs.as_ref(),
            Expr::Unary { op: UnaryOp::LogNot, .. }
        ));
    }

    #[test]
    fn symbol_and_or_not_parse_as_bitwise() {
        // The symbol/keyword split: `&` / `|` / `~` and `BAND` / `BOR` /
        // `BNOT` are the *bitwise* forms. Same precedence skeleton as
        // the logical test above.
        let p = parse_ok("LET x = a = 1 BAND b = 2 BOR BNOT c");
        let Decl::Let(l) = &p.items[0] else {
            panic!();
        };
        let Expr::Binary {
            op: BinaryOp::BitOr,
            lhs,
            rhs,
            ..
        } = &l.bindings[0].1
        else {
            panic!("expected bitwise OR at the root");
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
    fn parses_global_block_form() {
        // `GLOBAL $( name = expr; ... $)` is the batch form for
        // module-scope bindings. Each becomes a single LLVM
        // `@<name>` global in IR/LLVM emit.
        let p = parse_ok("GLOBAL $(\n  x = 1\n  y = 2\n$)");
        let Decl::Global(g) = &p.items[0] else {
            panic!();
        };
        assert_eq!(g.bindings.len(), 2);
        assert_eq!(g.bindings[0].name, "x");
        assert_eq!(g.bindings[1].name, "y");
    }

    #[test]
    fn parses_global_single_form() {
        // `GLOBAL name = expr` is the single-line form, equivalent
        // to a one-entry block. Common shape for the typical
        // "one global per declaration" use.
        let p = parse_ok("GLOBAL counter = 0");
        let Decl::Global(g) = &p.items[0] else {
            panic!();
        };
        assert_eq!(g.bindings.len(), 1);
        assert_eq!(g.bindings[0].name, "counter");
    }

    #[test]
    fn globals_keyword_rejected() {
        // The plural `GLOBALS` (classic slot-vector form) is not
        // supported in NewBCPL — the loader's symbol table replaces
        // it. Parser produces a clear pointer toward `GLOBAL`.
        let err = parse_err("GLOBALS $( wrch : 8 $)");
        assert!(
            err.message.contains("GLOBALS"),
            "expected GLOBALS-rejection message, got: {}",
            err.message
        );
    }

    #[test]
    fn global_slot_colon_form_rejected() {
        // `GLOBAL $( name : K $)` is the legacy GLOBALS slot syntax;
        // under `GLOBAL` it's a category error. The parser points
        // users at `=`.
        let err = parse_err("GLOBAL $( frob : 100 $)");
        assert!(
            err.message.contains("slot-pinning"),
            "expected slot-pinning rejection, got: {}",
            err.message
        );
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

    // ─── lists, list ops, conversion intrinsics ─────────────────

    #[test]
    fn parses_list_constructor() {
        let p = parse_ok("LET xs = LIST(1, 2, 3)");
        let Decl::Let(l) = &p.items[0] else {
            panic!();
        };
        let Expr::TypedConstruct {
            kind: TypeConstructorKind::List,
            args,
            ..
        } = &l.bindings[0].1
        else {
            panic!();
        };
        assert_eq!(args.len(), 3);
    }

    #[test]
    fn parses_manifestlist() {
        let p = parse_ok("LET xs = MANIFESTLIST(1, 2, 3)");
        let Decl::Let(l) = &p.items[0] else {
            panic!();
        };
        assert!(matches!(
            l.bindings[0].1,
            Expr::TypedConstruct {
                kind: TypeConstructorKind::ManifestList,
                ..
            }
        ));
    }

    #[test]
    fn parses_heterogeneous_list() {
        // Manifesto §5: lists may mix types.
        let p = parse_ok("LET xs = LIST(1, \"two\", 3.0, ?)");
        let Decl::Let(l) = &p.items[0] else {
            panic!();
        };
        let Expr::TypedConstruct { args, .. } = &l.bindings[0].1 else {
            panic!();
        };
        assert_eq!(args.len(), 4);
        assert!(matches!(args[0], Expr::IntLit { .. }));
        assert!(matches!(args[1], Expr::StringLit { .. }));
        assert!(matches!(args[2], Expr::FloatLit { .. }));
        assert!(matches!(args[3], Expr::Null { .. }));
    }

    #[test]
    fn parses_hd_tl_rest_len() {
        let p = parse_ok(
            "LET h = HD xs\nLET t = TL xs\nLET r = REST xs\nLET n = LEN xs",
        );
        let ops: Vec<UnaryOp> = p
            .items
            .iter()
            .map(|d| {
                let Decl::Let(l) = d else { panic!() };
                let Expr::Unary { op, .. } = l.bindings[0].1 else {
                    panic!()
                };
                op
            })
            .collect();
        assert_eq!(
            ops,
            vec![UnaryOp::Hd, UnaryOp::Tl, UnaryOp::Rest, UnaryOp::Len]
        );
    }

    #[test]
    fn parses_freevec_freelist() {
        let p = parse_ok("LET S() BE { FREEVEC v\n FREELIST xs }");
        let Decl::Routine(r) = &p.items[0] else {
            panic!();
        };
        let Stmt::Block(b) = r.body.as_ref() else {
            panic!();
        };
        let Stmt::Expr(Expr::Unary { op: o0, .. }) = &b.stmts[0] else {
            panic!();
        };
        let Stmt::Expr(Expr::Unary { op: o1, .. }) = &b.stmts[1] else {
            panic!();
        };
        assert_eq!(*o0, UnaryOp::FreeVec);
        assert_eq!(*o1, UnaryOp::FreeList);
    }

    #[test]
    fn parses_float_trunc_intrinsics() {
        let p = parse_ok("LET a = FLOAT(42)\nLET b = TRUNC(3.14)");
        // These parse as Call expressions whose callee is an Ident.
        let Decl::Let(l0) = &p.items[0] else { panic!() };
        let Decl::Let(l1) = &p.items[1] else { panic!() };
        let Expr::Call {
            callee: c0, args: a0, ..
        } = &l0.bindings[0].1
        else {
            panic!();
        };
        let Expr::Call {
            callee: c1, args: a1, ..
        } = &l1.bindings[0].1
        else {
            panic!();
        };
        assert!(matches!(c0.as_ref(), Expr::Ident { name, .. } if name == "FLOAT"));
        assert!(matches!(c1.as_ref(), Expr::Ident { name, .. } if name == "TRUNC"));
        assert_eq!(a0.len(), 1);
        assert_eq!(a1.len(), 1);
    }

    #[test]
    fn hd_binds_tighter_than_arith() {
        // HD xs + 1 parses as (HD xs) + 1
        let p = parse_ok("LET x = HD xs + 1");
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
            Expr::Unary { op: UnaryOp::Hd, .. }
        ));
    }

    // ─── FOR / FOREACH / SWITCHON ───────────────────────────────

    #[test]
    fn parses_for_to() {
        let p = parse_ok("LET S() BE { FOR i = 1 TO 10 DO f(i) }");
        let Decl::Routine(r) = &p.items[0] else {
            panic!();
        };
        let Stmt::Block(b) = r.body.as_ref() else {
            panic!();
        };
        let Stmt::For {
            name,
            step,
            ..
        } = &b.stmts[0]
        else {
            panic!();
        };
        assert_eq!(name, "i");
        assert!(step.is_none());
    }

    #[test]
    fn parses_for_to_by() {
        let p = parse_ok("LET S() BE { FOR i = 0 TO 100 BY 5 DO f(i) }");
        let Decl::Routine(r) = &p.items[0] else {
            panic!();
        };
        let Stmt::Block(b) = r.body.as_ref() else {
            panic!();
        };
        let Stmt::For { step, .. } = &b.stmts[0] else {
            panic!();
        };
        assert!(step.is_some());
    }

    #[test]
    fn parses_foreach_simple() {
        let p = parse_ok("LET S() BE { FOREACH e IN xs DO f(e) }");
        let Decl::Routine(r) = &p.items[0] else {
            panic!();
        };
        let Stmt::Block(b) = r.body.as_ref() else {
            panic!();
        };
        let Stmt::ForEach {
            names,
            annotation,
            ..
        } = &b.stmts[0]
        else {
            panic!();
        };
        assert_eq!(names, &vec!["e".to_string()]);
        assert!(annotation.is_none());
    }

    #[test]
    fn parses_foreach_with_annotation() {
        let p = parse_ok("LET S() BE { FOREACH C AS INTEGER IN S DO f(C) }");
        let Decl::Routine(r) = &p.items[0] else {
            panic!();
        };
        let Stmt::Block(b) = r.body.as_ref() else {
            panic!();
        };
        let Stmt::ForEach { annotation, .. } = &b.stmts[0] else {
            panic!();
        };
        assert_eq!(annotation, &Some("INTEGER".to_string()));
    }

    #[test]
    fn parses_foreach_pair_destructuring() {
        let p = parse_ok("LET S() BE { FOREACH k, v IN m DO f(k, v) }");
        let Decl::Routine(r) = &p.items[0] else {
            panic!();
        };
        let Stmt::Block(b) = r.body.as_ref() else {
            panic!();
        };
        let Stmt::ForEach { names, .. } = &b.stmts[0] else {
            panic!();
        };
        assert_eq!(names.len(), 2);
        assert_eq!(names[0], "k");
        assert_eq!(names[1], "v");
    }

    #[test]
    fn parses_switchon() {
        let p = parse_ok(
            "LET S() BE $(\n  SWITCHON x INTO $(\n    CASE 1: f()\n    CASE 2:\n    CASE 3: g()\n    DEFAULT: h()\n  $)\n$)",
        );
        let Decl::Routine(r) = &p.items[0] else {
            panic!();
        };
        let Stmt::Block(b) = r.body.as_ref() else {
            panic!();
        };
        let Stmt::Switchon {
            cases,
            default,
            ..
        } = &b.stmts[0]
        else {
            panic!();
        };
        assert_eq!(cases.len(), 3);
        // CASE 2: had no body, so its `body` is empty.
        assert!(cases[1].body.is_empty());
        // CASE 3 has the body f() (well, g()).
        assert_eq!(cases[2].body.len(), 1);
        assert!(default.is_some());
    }

    #[test]
    fn for_loop_with_compound_body() {
        let src = "LET S() BE $(
            FOR i = 1 TO 10 DO $(
                WRITES(\"hi*N\")
                IF i > 5 THEN BREAK
            $)
        $)";
        let p = parse_ok(src);
        let Decl::Routine(r) = &p.items[0] else {
            panic!();
        };
        let Stmt::Block(b) = r.body.as_ref() else {
            panic!();
        };
        assert!(matches!(b.stmts[0], Stmt::For { .. }));
    }

    // ─── classes ────────────────────────────────────────────────

    #[test]
    fn parses_simple_class() {
        let p = parse_ok(
            "CLASS Point $(\n  DECL x, y\n  ROUTINE CREATE(initialX, initialY) BE $( x := initialX\n y := initialY $)\n  FUNCTION getX() = x\n$)",
        );
        let Decl::Class(c) = &p.items[0] else {
            panic!();
        };
        assert_eq!(c.name, "Point");
        assert!(c.extends.is_none());
        assert!(!c.managed);
        assert_eq!(c.members.len(), 3);
        // 0: DECL x, y
        assert!(matches!(c.members[0].kind, ClassMemberKind::Fields { .. }));
        // 1: ROUTINE CREATE
        assert!(matches!(c.members[1].kind, ClassMemberKind::Method(_)));
        // 2: FUNCTION getX
        assert!(matches!(c.members[2].kind, ClassMemberKind::Method(_)));
    }

    #[test]
    fn parses_class_with_extends() {
        let p = parse_ok(
            "CLASS ColorPoint EXTENDS Point $(\n  DECL color\n  ROUTINE CREATE(x, y, c) BE $( SELF.x := x $)\n$)",
        );
        let Decl::Class(c) = &p.items[0] else {
            panic!();
        };
        assert_eq!(c.extends, Some("Point".to_string()));
    }

    #[test]
    fn parses_managed_class() {
        let p = parse_ok(
            "CLASS Window MANAGED $(\n  DECL handle\n  ROUTINE RELEASE() BE $( WRITES(\"closing*N\") $)\n$)",
        );
        let Decl::Class(c) = &p.items[0] else {
            panic!();
        };
        assert!(c.managed);
    }

    #[test]
    fn parses_virtual_method() {
        let p = parse_ok(
            "CLASS Animal $(\n  VIRTUAL ROUTINE makeSound() BE $( WRITES(\"...*N\") $)\n$)",
        );
        let Decl::Class(c) = &p.items[0] else {
            panic!();
        };
        let ClassMemberKind::Method(m) = &c.members[0].kind else {
            panic!();
        };
        assert!(m.is_virtual);
        assert!(!m.is_final);
    }

    #[test]
    fn parses_visibility_sections() {
        let p = parse_ok(
            "CLASS BankAccount $(\n  PUBLIC:\n    FUNCTION getBalance() = balance\n  PRIVATE:\n    DECL balance\n$)",
        );
        let Decl::Class(c) = &p.items[0] else {
            panic!();
        };
        assert_eq!(c.members.len(), 2);
        assert_eq!(c.members[0].visibility, Visibility::Public);
        assert_eq!(c.members[1].visibility, Visibility::Private);
    }

    #[test]
    fn parses_flet_uninitialised_member() {
        let p = parse_ok("CLASS Point $(\n  FLET x\n  FLET y = 0.0\n$)");
        let Decl::Class(c) = &p.items[0] else {
            panic!();
        };
        let ClassMemberKind::FLet(b0) = &c.members[0].kind else {
            panic!();
        };
        let ClassMemberKind::FLet(b1) = &c.members[1].kind else {
            panic!();
        };
        assert_eq!(b0.name, "x");
        assert!(b0.value.is_none());
        assert_eq!(b1.name, "y");
        assert!(b1.value.is_some());
    }

    #[test]
    fn parses_new_expression() {
        let p = parse_ok("LET p = NEW Point(50, 75)");
        let Decl::Let(l) = &p.items[0] else {
            panic!();
        };
        let Expr::New { class_name, args, .. } = &l.bindings[0].1 else {
            panic!();
        };
        assert_eq!(class_name, "Point");
        assert_eq!(args.len(), 2);
    }

    #[test]
    fn parses_new_no_args() {
        let p = parse_ok("LET p = NEW Shape");
        let Decl::Let(l) = &p.items[0] else {
            panic!();
        };
        let Expr::New { class_name, args, .. } = &l.bindings[0].1 else {
            panic!();
        };
        assert_eq!(class_name, "Shape");
        assert!(args.is_empty());
    }

    #[test]
    fn parses_self_super() {
        let p = parse_ok("LET S() BE { SELF.x := 1\n SUPER.move(0, 0) }");
        let Decl::Routine(r) = &p.items[0] else {
            panic!();
        };
        let Stmt::Block(b) = r.body.as_ref() else {
            panic!();
        };
        // First stmt: SELF.x := 1 → assignment with target Binary{Dot, SELF, x}
        let Stmt::Assign { targets, .. } = &b.stmts[0] else {
            panic!();
        };
        let Expr::Binary {
            op: BinaryOp::Dot,
            lhs,
            ..
        } = &targets[0]
        else {
            panic!();
        };
        assert!(matches!(
            lhs.as_ref(),
            Expr::Ident { name, .. } if name == "SELF"
        ));
        // Second stmt: SUPER.move(0, 0) → call on Binary{Dot, SUPER, move}
        let Stmt::Expr(Expr::Call { callee, args, .. }) = &b.stmts[1] else {
            panic!();
        };
        let Expr::Binary {
            op: BinaryOp::Dot,
            lhs,
            ..
        } = callee.as_ref()
        else {
            panic!();
        };
        assert!(matches!(
            lhs.as_ref(),
            Expr::Ident { name, .. } if name == "SUPER"
        ));
        assert_eq!(args.len(), 2);
    }

    #[test]
    fn parses_lane_access() {
        let p = parse_ok("LET x = fpair.|0|");
        let Decl::Let(l) = &p.items[0] else {
            panic!();
        };
        let Expr::Binary {
            op: BinaryOp::LaneAccess,
            lhs,
            rhs,
            ..
        } = &l.bindings[0].1
        else {
            panic!();
        };
        assert!(matches!(lhs.as_ref(), Expr::Ident { .. }));
        assert!(matches!(rhs.as_ref(), Expr::IntLit { value: 0, .. }));
    }

    // ─── mop-up: GOTO / labels / RETAIN / BRK / TYPE / AS ───────

    #[test]
    fn parses_goto_and_label() {
        let src = "LET S() BE $(
            IF x > 0 THEN GOTO positive
            GOTO negative
            positive:
                f()
            negative:
                g()
        $)";
        let p = parse_ok(src);
        let Decl::Routine(r) = &p.items[0] else {
            panic!();
        };
        let Stmt::Block(b) = r.body.as_ref() else {
            panic!();
        };
        let labels: Vec<_> = b
            .stmts
            .iter()
            .filter_map(|s| match s {
                Stmt::Label { name, .. } => Some(name.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(labels, vec!["positive".to_string(), "negative".to_string()]);
        let gotos: Vec<_> = b
            .stmts
            .iter()
            .filter_map(|s| match s {
                Stmt::Goto { label, .. } => Some(label.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(gotos, vec!["negative".to_string()]);
    }

    #[test]
    fn label_does_not_break_assignment() {
        // `x := 0` must parse as Assign, NOT as label `x` + stray `=` + 0.
        let p = parse_ok("LET S() BE { x := 0 }");
        let Decl::Routine(r) = &p.items[0] else {
            panic!();
        };
        let Stmt::Block(b) = r.body.as_ref() else {
            panic!();
        };
        assert!(matches!(b.stmts[0], Stmt::Assign { .. }));
    }

    #[test]
    fn parses_retain_bare_and_init() {
        let p = parse_ok(
            "LET S() BE { RETAIN counter\n RETAIN p3 = NEW MyClass() }",
        );
        let Decl::Routine(r) = &p.items[0] else {
            panic!();
        };
        let Stmt::Block(b) = r.body.as_ref() else {
            panic!();
        };
        let Stmt::Retain {
            name: n0, value: v0, ..
        } = &b.stmts[0]
        else {
            panic!();
        };
        assert_eq!(n0, "counter");
        assert!(v0.is_none());
        let Stmt::Retain {
            name: n1, value: v1, ..
        } = &b.stmts[1]
        else {
            panic!();
        };
        assert_eq!(n1, "p3");
        assert!(matches!(v1, Some(Expr::New { .. })));
    }

    #[test]
    fn parses_brk() {
        let p = parse_ok("LET S() BE { BRK }");
        let Decl::Routine(r) = &p.items[0] else {
            panic!();
        };
        let Stmt::Block(b) = r.body.as_ref() else {
            panic!();
        };
        assert!(matches!(b.stmts[0], Stmt::Brk(_)));
    }

    #[test]
    fn parses_type_macro() {
        let p = parse_ok("LET t = TYPE(x)");
        let Decl::Let(l) = &p.items[0] else {
            panic!();
        };
        let Expr::Call { callee, args, .. } = &l.bindings[0].1 else {
            panic!();
        };
        assert!(matches!(callee.as_ref(), Expr::Ident { name, .. } if name == "TYPE"));
        assert_eq!(args.len(), 1);
    }

    #[test]
    fn parses_let_with_as_annotation() {
        // The AS annotation is parsed and discarded for now.
        let p = parse_ok("LET x AS INTEGER = 42");
        let Decl::Let(l) = &p.items[0] else {
            panic!();
        };
        assert_eq!(l.bindings[0].0, "x");
        assert!(matches!(l.bindings[0].1, Expr::IntLit { value: 42, .. }));
    }

    #[test]
    fn parses_let_multi_with_as_annotations() {
        let p = parse_ok("LET a AS INTEGER, b AS FLOAT = 1, 2.0");
        let Decl::Let(l) = &p.items[0] else {
            panic!();
        };
        assert_eq!(l.bindings.len(), 2);
        assert_eq!(l.bindings[0].0, "a");
        assert_eq!(l.bindings[1].0, "b");
    }

    // ─── small forms: VEC[…] inline-init, %% (start, width) ─────

    #[test]
    fn parses_vec_inline_init() {
        let p = parse_ok("LET w = VEC [10, 20, 30]");
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
        assert_eq!(args.len(), 3);
    }

    #[test]
    fn parses_bitfield_access() {
        let p = parse_ok("LET S() BE { x := m %% (0, 8) }");
        let Decl::Routine(r) = &p.items[0] else {
            panic!();
        };
        let Stmt::Block(b) = r.body.as_ref() else {
            panic!();
        };
        let Stmt::Assign { values, .. } = &b.stmts[0] else {
            panic!();
        };
        // values[0] = m %% (0, 8)
        let Expr::Binary {
            op: BinaryOp::Bitfield,
            ..
        } = &values[0]
        else {
            panic!("expected Bitfield");
        };
    }

    #[test]
    fn parses_bitfield_assignment_target() {
        // The bits.bcl pattern: m %% (0, 8) := 212
        let p = parse_ok("LET S() BE { m %% (0, 8) := 212 }");
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
            Expr::Binary { op: BinaryOp::Bitfield, .. }
        ));
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
