//! Recursive-descent parser for NewBCPL.
//!
//! Currently parses:
//!
//! - top-level function and routine declarations
//!   (`LET F(p1, p2) = expr` and `LET R(p1, p2) BE stmt`)
//! - top-level and stmt-level `LET` bindings (single and multi)
//! - blocks delimited by either `$( $)` or `{ }` (interchangeable)
//! - statements: `:=` assignment, `IF` / `UNLESS` / `TEST`, `WHILE` /
//!   `UNTIL` / postfix `REPEAT` / `REPEATWHILE` / `REPEATUNTIL`,
//!   `RESULTIS`, `RETURN`, `FINISH`, `BREAK`, `LOOP`, `ENDCASE`
//! - expressions: identifiers, literals (int, real, char, string, bool,
//!   null), parenthesised expressions, function calls, full BCPL operator
//!   precedence ladder (unary `-` `~` `!` `@` `%`; postfix call,
//!   subscript family `!` `%` `%%` `.%`, member access `.` and `OF`;
//!   binary `*` `/` `REM` `+` `-` and dotted variants; shifts; relational
//!   `=` `~=` `<` `<=` `>` `>=` and dotted variants; `&`; `|`, `EQV`,
//!   `NEQV`; conditional `cond -> then, else`); `VALOF` blocks
//!
//! Defers to subsequent turns:
//!
//! - `FOR ... TO ... BY ... DO`, `SWITCHON / CASE / DEFAULT`
//! - `MANIFEST`, `STATIC`, `GLOBAL`, `VEC`, `GET`
//! - classes, lists, FOREACH, NEW, `MANAGED`
//! - labels and `GOTO`

use crate::ast::*;
use newbcpl_lexer::{LexError, SourceSpan, Token, TokenKind, lex_source};

#[derive(Debug, Clone)]
pub struct ParseError {
    pub message: String,
    pub span: SourceSpan,
}

impl ParseError {
    fn new(message: impl Into<String>, span: SourceSpan) -> Self {
        Self {
            message: message.into(),
            span,
        }
    }

    pub(crate) fn from_lex(error: LexError) -> Self {
        Self {
            message: format!("lex: {}", error.message),
            span: error.span,
        }
    }

    pub fn render(&self) -> String {
        format!(
            "{} at {}:{}",
            self.message, self.span.start.line, self.span.start.column
        )
    }
}

pub fn parse_source(source: &str) -> Result<Program, ParseError> {
    let tokens = lex_source(source).map_err(ParseError::from_lex)?;
    Parser::new(tokens).parse_program()
}

pub(crate) struct Parser {
    tokens: Vec<Token>,
    pos: usize,
}

impl Parser {
    fn new(tokens: Vec<Token>) -> Self {
        Self { tokens, pos: 0 }
    }

    fn peek(&self) -> &Token {
        &self.tokens[self.pos]
    }

    fn is_at_end(&self) -> bool {
        self.peek().kind == TokenKind::Eof
    }

    fn check_kw(&self, name: &str) -> bool {
        let t = self.peek();
        t.kind == TokenKind::Keyword && t.lexeme == name
    }

    fn check_sym(&self, name: &str) -> bool {
        let t = self.peek();
        t.kind == TokenKind::Symbol && t.lexeme == name
    }

    fn eat(&mut self) -> Token {
        let token = self.tokens[self.pos].clone();
        if token.kind != TokenKind::Eof {
            self.pos += 1;
        }
        token
    }

    fn expect_sym(&mut self, name: &str) -> Result<Token, ParseError> {
        if self.check_sym(name) {
            Ok(self.eat())
        } else {
            let span = self.peek().span;
            let lex = self.peek().lexeme.clone();
            Err(ParseError::new(
                format!("expected `{name}`, got `{lex}`"),
                span,
            ))
        }
    }

    fn eat_identifier(&mut self) -> Result<Token, ParseError> {
        let token = self.peek().clone();
        if token.kind == TokenKind::Identifier {
            self.pos += 1;
            Ok(token)
        } else {
            Err(ParseError::new(
                format!("expected identifier, got `{}`", token.lexeme),
                token.span,
            ))
        }
    }

    // ───────────────────────── top-level ─────────────────────────

    fn parse_program(&mut self) -> Result<Program, ParseError> {
        let mut items = Vec::new();
        while !self.is_at_end() {
            if self.check_sym(";") {
                self.eat();
                continue;
            }
            items.push(self.parse_decl()?);
        }
        Ok(Program { items })
    }

    fn parse_decl(&mut self) -> Result<Decl, ParseError> {
        if self.check_kw("LET") {
            self.parse_let_decl()
        } else {
            let span = self.peek().span;
            let lex = self.peek().lexeme.clone();
            Err(ParseError::new(
                format!("expected declaration, got `{lex}`"),
                span,
            ))
        }
    }

    fn parse_let_decl(&mut self) -> Result<Decl, ParseError> {
        let let_token = self.eat();
        let start = let_token.span.start;
        let first_name = self.eat_identifier()?.lexeme;

        if self.check_sym("(") {
            self.eat();
            let mut params = Vec::new();
            if !self.check_sym(")") {
                params.push(self.eat_identifier()?.lexeme);
                while self.check_sym(",") {
                    self.eat();
                    params.push(self.eat_identifier()?.lexeme);
                }
            }
            self.expect_sym(")")?;
            if self.check_sym("=") {
                self.eat();
                let body = self.parse_expr()?;
                let end = body.span().end;
                return Ok(Decl::Function(FunctionDecl {
                    name: first_name,
                    params,
                    body,
                    span: SourceSpan { start, end },
                }));
            }
            if self.check_kw("BE") {
                self.eat();
                let body = self.parse_stmt()?;
                let end = body.span().end;
                return Ok(Decl::Routine(RoutineDecl {
                    name: first_name,
                    params,
                    body: Box::new(body),
                    span: SourceSpan { start, end },
                }));
            }
            let span = self.peek().span;
            let lex = self.peek().lexeme.clone();
            return Err(ParseError::new(
                format!("expected `=` or `BE` after parameter list, got `{lex}`"),
                span,
            ));
        }

        // Plain binding: LET n1, n2, ... = e1, e2, ...
        let mut names = vec![first_name];
        while self.check_sym(",") {
            self.eat();
            names.push(self.eat_identifier()?.lexeme);
        }
        self.expect_sym("=")?;
        let mut exprs = vec![self.parse_expr()?];
        while self.check_sym(",") {
            self.eat();
            exprs.push(self.parse_expr()?);
        }
        if names.len() != exprs.len() {
            let span = exprs.last().map(|e| e.span()).unwrap_or(let_token.span);
            return Err(ParseError::new(
                format!(
                    "LET binding has {} names but {} expressions",
                    names.len(),
                    exprs.len()
                ),
                span,
            ));
        }
        let bindings: Vec<(String, Expr)> = names.into_iter().zip(exprs).collect();
        let end = bindings
            .last()
            .map(|(_, e)| e.span().end)
            .unwrap_or(let_token.span.end);
        Ok(Decl::Let(LetDecl {
            bindings,
            span: SourceSpan { start, end },
        }))
    }

    // ───────────────────────── statements ─────────────────────────

    fn parse_stmt(&mut self) -> Result<Stmt, ParseError> {
        let stmt = self.parse_basic_stmt()?;
        // Postfix loop forms attach to any statement.
        if self.check_kw("REPEAT") {
            let kw = self.eat();
            let span = SourceSpan {
                start: stmt.span().start,
                end: kw.span.end,
            };
            return Ok(Stmt::Repeat {
                body: Box::new(stmt),
                span,
            });
        }
        if self.check_kw("REPEATWHILE") {
            self.eat();
            let cond = self.parse_expr()?;
            let span = SourceSpan {
                start: stmt.span().start,
                end: cond.span().end,
            };
            return Ok(Stmt::RepeatWhile {
                body: Box::new(stmt),
                cond,
                span,
            });
        }
        if self.check_kw("REPEATUNTIL") {
            self.eat();
            let cond = self.parse_expr()?;
            let span = SourceSpan {
                start: stmt.span().start,
                end: cond.span().end,
            };
            return Ok(Stmt::RepeatUntil {
                body: Box::new(stmt),
                cond,
                span,
            });
        }
        Ok(stmt)
    }

    fn parse_basic_stmt(&mut self) -> Result<Stmt, ParseError> {
        if self.check_sym("$(") || self.check_sym("{") {
            return self.parse_block();
        }
        if self.check_kw("LET") {
            return Ok(Stmt::Decl(self.parse_let_decl()?));
        }
        if self.check_kw("IF") {
            return self.parse_if();
        }
        if self.check_kw("UNLESS") {
            return self.parse_unless();
        }
        if self.check_kw("TEST") {
            return self.parse_test();
        }
        if self.check_kw("WHILE") {
            return self.parse_while();
        }
        if self.check_kw("UNTIL") {
            return self.parse_until();
        }
        if self.check_kw("RESULTIS") {
            let kw = self.eat();
            let value = self.parse_expr()?;
            let span = SourceSpan {
                start: kw.span.start,
                end: value.span().end,
            };
            return Ok(Stmt::Resultis(value, span));
        }
        if self.check_kw("RETURN") {
            return Ok(Stmt::Return(self.eat().span));
        }
        if self.check_kw("FINISH") {
            return Ok(Stmt::Finish(self.eat().span));
        }
        if self.check_kw("BREAK") {
            return Ok(Stmt::Break(self.eat().span));
        }
        if self.check_kw("LOOP") {
            return Ok(Stmt::Loop(self.eat().span));
        }
        if self.check_kw("ENDCASE") {
            return Ok(Stmt::Endcase(self.eat().span));
        }

        // Expression-or-assignment fall-through.
        let first = self.parse_expr()?;
        if self.check_sym(":=") {
            return self.parse_assign_after_first(first);
        }
        if self.check_sym(",") {
            // Multi-target assignment: lvalue1, lvalue2 := rhs1, rhs2
            let mut targets = vec![first];
            while self.check_sym(",") {
                self.eat();
                targets.push(self.parse_expr()?);
            }
            if self.check_sym(":=") {
                return self.parse_assign_after_targets(targets);
            }
            // Just a comma-separated expression list with no `:=`
            // makes no sense as a statement; flag as an error.
            let span = self.peek().span;
            let lex = self.peek().lexeme.clone();
            return Err(ParseError::new(
                format!("expected `:=` after comma-separated targets, got `{lex}`"),
                span,
            ));
        }
        Ok(Stmt::Expr(first))
    }

    fn parse_assign_after_first(&mut self, first: Expr) -> Result<Stmt, ParseError> {
        self.eat(); // :=
        let value = self.parse_expr()?;
        let span = SourceSpan {
            start: first.span().start,
            end: value.span().end,
        };
        Ok(Stmt::Assign {
            targets: vec![first],
            values: vec![value],
            span,
        })
    }

    fn parse_assign_after_targets(&mut self, targets: Vec<Expr>) -> Result<Stmt, ParseError> {
        self.eat(); // :=
        let mut values = vec![self.parse_expr()?];
        while self.check_sym(",") {
            self.eat();
            values.push(self.parse_expr()?);
        }
        if values.len() != targets.len() {
            let span = values.last().map(|e| e.span()).unwrap_or(targets[0].span());
            return Err(ParseError::new(
                format!(
                    "multi-assignment has {} targets but {} values",
                    targets.len(),
                    values.len()
                ),
                span,
            ));
        }
        let span = SourceSpan {
            start: targets[0].span().start,
            end: values.last().unwrap().span().end,
        };
        Ok(Stmt::Assign {
            targets,
            values,
            span,
        })
    }

    fn parse_block(&mut self) -> Result<Stmt, ParseError> {
        let open = self.eat();
        let close_lex = match open.lexeme.as_str() {
            "$(" => "$)",
            "{" => "}",
            other => {
                return Err(ParseError::new(
                    format!("internal: unexpected block opener `{other}`"),
                    open.span,
                ));
            }
        };
        let mut stmts = Vec::new();
        loop {
            if self.is_at_end() {
                return Err(ParseError::new(
                    format!("unterminated block — expected `{close_lex}`"),
                    open.span,
                ));
            }
            if self.check_sym(close_lex) {
                let close = self.eat();
                return Ok(Stmt::Block(Block {
                    stmts,
                    span: SourceSpan {
                        start: open.span.start,
                        end: close.span.end,
                    },
                }));
            }
            if self.check_sym(";") {
                self.eat();
                continue;
            }
            stmts.push(self.parse_stmt()?);
        }
    }

    /// `IF` and `TEST` produce the same `Stmt::If` shape: a condition,
    /// a then-branch, and an optional else-branch. The keywords differ
    /// only in whether the ELSE branch is required:
    ///   `IF cond THEN body`            — else optional
    ///   `IF cond THEN body ELSE other` — else taken when present
    ///   `TEST cond THEN body ELSE other` — else required
    /// `OR` is a binary operator only; it is not an else-marker.
    fn parse_if(&mut self) -> Result<Stmt, ParseError> {
        let kw = self.eat(); // IF
        let cond = self.parse_expr()?;
        // THEN is canonical; DO is also accepted to be lenient with
        // `IF c DO stmt` from older BCPL.
        if self.check_kw("THEN") || self.check_kw("DO") {
            self.eat();
        }
        let then_stmt = self.parse_stmt()?;
        let mut end = then_stmt.span().end;
        let else_stmt = if self.check_kw("ELSE") {
            self.eat();
            let stmt = self.parse_stmt()?;
            end = stmt.span().end;
            Some(Box::new(stmt))
        } else {
            None
        };
        Ok(Stmt::If {
            cond,
            then_stmt: Box::new(then_stmt),
            else_stmt,
            span: SourceSpan {
                start: kw.span.start,
                end,
            },
        })
    }

    fn parse_unless(&mut self) -> Result<Stmt, ParseError> {
        let kw = self.eat();
        let cond = self.parse_expr()?;
        if self.check_kw("THEN") || self.check_kw("DO") {
            self.eat();
        }
        let then_stmt = self.parse_stmt()?;
        let span = SourceSpan {
            start: kw.span.start,
            end: then_stmt.span().end,
        };
        Ok(Stmt::Unless {
            cond,
            then_stmt: Box::new(then_stmt),
            span,
        })
    }

    fn parse_test(&mut self) -> Result<Stmt, ParseError> {
        let kw = self.eat(); // TEST
        let cond = self.parse_expr()?;
        if self.check_kw("THEN") {
            self.eat();
        }
        let then_stmt = self.parse_stmt()?;
        if !self.check_kw("ELSE") {
            let span = self.peek().span;
            let lex = self.peek().lexeme.clone();
            return Err(ParseError::new(
                format!("expected `ELSE` after TEST … THEN …, got `{lex}`"),
                span,
            ));
        }
        self.eat();
        let else_stmt = self.parse_stmt()?;
        let span = SourceSpan {
            start: kw.span.start,
            end: else_stmt.span().end,
        };
        Ok(Stmt::If {
            cond,
            then_stmt: Box::new(then_stmt),
            else_stmt: Some(Box::new(else_stmt)),
            span,
        })
    }

    fn parse_while(&mut self) -> Result<Stmt, ParseError> {
        let kw = self.eat();
        let cond = self.parse_expr()?;
        if self.check_kw("DO") {
            self.eat();
        }
        let body = self.parse_stmt()?;
        let span = SourceSpan {
            start: kw.span.start,
            end: body.span().end,
        };
        Ok(Stmt::While {
            cond,
            body: Box::new(body),
            span,
        })
    }

    fn parse_until(&mut self) -> Result<Stmt, ParseError> {
        let kw = self.eat();
        let cond = self.parse_expr()?;
        if self.check_kw("DO") {
            self.eat();
        }
        let body = self.parse_stmt()?;
        let span = SourceSpan {
            start: kw.span.start,
            end: body.span().end,
        };
        Ok(Stmt::Until {
            cond,
            body: Box::new(body),
            span,
        })
    }

    // ───────────────────────── expressions ─────────────────────────
    //
    // Precedence climbing. `parse_expr` is the public entry point;
    // `parse_binary_at(min)` handles every binary operator at or above
    // precedence `min`. Conditional `cond -> then, else` and the
    // postfix call / subscript / member access live in their own
    // helpers because their shapes don't fit the binary mould.

    fn parse_expr(&mut self) -> Result<Expr, ParseError> {
        // Conditional sits at the bottom of the ladder per the BCPL
        // spec: `cond -> then, else`. Right-associative.
        let cond = self.parse_binary_at(0)?;
        if self.check_sym("->") {
            self.eat();
            let then_expr = self.parse_expr()?;
            self.expect_sym(",")?;
            let else_expr = self.parse_expr()?;
            let span = SourceSpan {
                start: cond.span().start,
                end: else_expr.span().end,
            };
            return Ok(Expr::Conditional {
                cond: Box::new(cond),
                then_expr: Box::new(then_expr),
                else_expr: Box::new(else_expr),
                span,
            });
        }
        Ok(cond)
    }

    fn parse_binary_at(&mut self, min_prec: u8) -> Result<Expr, ParseError> {
        let mut lhs = self.parse_unary()?;
        loop {
            let Some((op, prec)) = self.peek_binary_op() else {
                break;
            };
            if prec < min_prec {
                break;
            }
            self.eat();
            // All BCPL binary ops in this set are left-associative:
            // climb on the right with prec + 1.
            let rhs = self.parse_binary_at(prec + 1)?;
            let span = SourceSpan {
                start: lhs.span().start,
                end: rhs.span().end,
            };
            lhs = Expr::Binary {
                op,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
                span,
            };
        }
        Ok(lhs)
    }

    /// Look at the next token and decide whether it begins a binary
    /// operator, returning its `BinaryOp` and precedence. Postfix
    /// operators (call, subscript, member access) are handled by
    /// `parse_postfix` and are not reported here.
    fn peek_binary_op(&self) -> Option<(BinaryOp, u8)> {
        let t = self.peek();
        match (t.kind, t.lexeme.as_str()) {
            // Multiplicative (precedence 7)
            (TokenKind::Symbol, "*") => Some((BinaryOp::Mul, 7)),
            (TokenKind::Symbol, "/") => Some((BinaryOp::Div, 7)),
            (TokenKind::Keyword, "REM") => Some((BinaryOp::Rem, 7)),
            (TokenKind::Symbol, "*.") => Some((BinaryOp::FMul, 7)),
            (TokenKind::Symbol, "/.") => Some((BinaryOp::FDiv, 7)),
            // Additive (precedence 6)
            (TokenKind::Symbol, "+") => Some((BinaryOp::Add, 6)),
            (TokenKind::Symbol, "-") => Some((BinaryOp::Sub, 6)),
            (TokenKind::Symbol, "+.") => Some((BinaryOp::FAdd, 6)),
            (TokenKind::Symbol, "-.") => Some((BinaryOp::FSub, 6)),
            // Shifts (precedence 5)
            (TokenKind::Symbol, "<<") => Some((BinaryOp::Shl, 5)),
            (TokenKind::Symbol, ">>") => Some((BinaryOp::Shr, 5)),
            // Relational (precedence 4)
            (TokenKind::Symbol, "=") => Some((BinaryOp::Eq, 4)),
            (TokenKind::Symbol, "~=") => Some((BinaryOp::Ne, 4)),
            (TokenKind::Symbol, "<") => Some((BinaryOp::Lt, 4)),
            (TokenKind::Symbol, "<=") => Some((BinaryOp::Le, 4)),
            (TokenKind::Symbol, ">") => Some((BinaryOp::Gt, 4)),
            (TokenKind::Symbol, ">=") => Some((BinaryOp::Ge, 4)),
            (TokenKind::Symbol, "=.") => Some((BinaryOp::FEq, 4)),
            (TokenKind::Symbol, "~=.") => Some((BinaryOp::FNe, 4)),
            (TokenKind::Symbol, "<.") => Some((BinaryOp::FLt, 4)),
            (TokenKind::Symbol, "<=.") => Some((BinaryOp::FLe, 4)),
            (TokenKind::Symbol, ">.") => Some((BinaryOp::FGt, 4)),
            (TokenKind::Symbol, ">=.") => Some((BinaryOp::FGe, 4)),
            // Bitwise / logical AND (precedence 3). `AND` is the
            // word-form synonym for `&` per the reference lexer; the
            // declaration-tail use (`LET f = ... AND g = ...`) is a
            // separate form handled at LET-parse time, not here.
            (TokenKind::Symbol, "&") => Some((BinaryOp::BitAnd, 3)),
            (TokenKind::Keyword, "AND") => Some((BinaryOp::BitAnd, 3)),
            // Bitwise / logical OR + EQV / NEQV (precedence 2). `OR`
            // is the word-form synonym for `|`.
            (TokenKind::Symbol, "|") => Some((BinaryOp::BitOr, 2)),
            (TokenKind::Keyword, "OR") => Some((BinaryOp::BitOr, 2)),
            (TokenKind::Keyword, "EQV") => Some((BinaryOp::Eqv, 2)),
            (TokenKind::Keyword, "NEQV") => Some((BinaryOp::Neqv, 2)),
            _ => None,
        }
    }

    fn parse_unary(&mut self) -> Result<Expr, ParseError> {
        // Prefix unary operators bind tighter than any binary op but
        // looser than the postfix forms.
        if self.check_sym("-") {
            let kw = self.eat();
            let operand = self.parse_unary()?;
            let span = SourceSpan {
                start: kw.span.start,
                end: operand.span().end,
            };
            return Ok(Expr::Unary {
                op: UnaryOp::Neg,
                operand: Box::new(operand),
                span,
            });
        }
        if self.check_sym("~") || self.check_kw("NOT") {
            let kw = self.eat();
            let operand = self.parse_unary()?;
            let span = SourceSpan {
                start: kw.span.start,
                end: operand.span().end,
            };
            return Ok(Expr::Unary {
                op: UnaryOp::Not,
                operand: Box::new(operand),
                span,
            });
        }
        if self.check_sym("!") {
            let kw = self.eat();
            let operand = self.parse_unary()?;
            let span = SourceSpan {
                start: kw.span.start,
                end: operand.span().end,
            };
            return Ok(Expr::Unary {
                op: UnaryOp::Indirection,
                operand: Box::new(operand),
                span,
            });
        }
        if self.check_sym("@") {
            let kw = self.eat();
            let operand = self.parse_unary()?;
            let span = SourceSpan {
                start: kw.span.start,
                end: operand.span().end,
            };
            return Ok(Expr::Unary {
                op: UnaryOp::AddressOf,
                operand: Box::new(operand),
                span,
            });
        }
        if self.check_sym("%") {
            let kw = self.eat();
            let operand = self.parse_unary()?;
            let span = SourceSpan {
                start: kw.span.start,
                end: operand.span().end,
            };
            return Ok(Expr::Unary {
                op: UnaryOp::CharIndirection,
                operand: Box::new(operand),
                span,
            });
        }
        self.parse_postfix()
    }

    /// Postfix forms: function calls, subscript family, member access.
    /// All bind tighter than any prefix unary or binary operator.
    fn parse_postfix(&mut self) -> Result<Expr, ParseError> {
        let mut expr = self.parse_atom()?;
        loop {
            if self.check_sym("(") {
                self.eat();
                let mut args = Vec::new();
                if !self.check_sym(")") {
                    args.push(self.parse_expr()?);
                    while self.check_sym(",") {
                        self.eat();
                        args.push(self.parse_expr()?);
                    }
                }
                let close = self.expect_sym(")")?;
                let span = SourceSpan {
                    start: expr.span().start,
                    end: close.span.end,
                };
                expr = Expr::Call {
                    callee: Box::new(expr),
                    args,
                    span,
                };
                continue;
            }

            // Subscript family — all are infix and have one expression
            // on the right. Note: the RHS uses `parse_unary` rather than
            // a full precedence-climb, so that `v!i + 1` parses as
            // `(v!i) + 1` (subscript binds tighter than `+`).
            let infix_subscript = if self.check_sym("!") {
                Some(BinaryOp::Subscript)
            } else if self.check_sym("%%") {
                Some(BinaryOp::Bitfield)
            } else if self.check_sym("%") {
                Some(BinaryOp::CharSubscript)
            } else if self.check_sym(".%") {
                Some(BinaryOp::FloatSubscript)
            } else {
                None
            };
            if let Some(op) = infix_subscript {
                self.eat();
                let rhs = self.parse_unary()?;
                let span = SourceSpan {
                    start: expr.span().start,
                    end: rhs.span().end,
                };
                expr = Expr::Binary {
                    op,
                    lhs: Box::new(expr),
                    rhs: Box::new(rhs),
                    span,
                };
                continue;
            }

            // Member access: `obj.field` — RHS must be an identifier.
            if self.check_sym(".") {
                self.eat();
                let field = self.eat_identifier()?;
                let rhs = Expr::Ident {
                    name: field.lexeme,
                    span: field.span,
                };
                let span = SourceSpan {
                    start: expr.span().start,
                    end: field.span.end,
                };
                expr = Expr::Binary {
                    op: BinaryOp::Dot,
                    lhs: Box::new(expr),
                    rhs: Box::new(rhs),
                    span,
                };
                continue;
            }

            // OF (classic field access). RHS is an identifier.
            if self.check_kw("OF") {
                self.eat();
                let field = self.eat_identifier()?;
                let rhs = Expr::Ident {
                    name: field.lexeme,
                    span: field.span,
                };
                let span = SourceSpan {
                    start: expr.span().start,
                    end: field.span.end,
                };
                expr = Expr::Binary {
                    op: BinaryOp::Of,
                    lhs: Box::new(expr),
                    rhs: Box::new(rhs),
                    span,
                };
                continue;
            }

            break;
        }
        Ok(expr)
    }

    fn parse_atom(&mut self) -> Result<Expr, ParseError> {
        let tok = self.peek().clone();
        match tok.kind {
            TokenKind::Identifier => {
                self.pos += 1;
                Ok(Expr::Ident {
                    name: tok.lexeme,
                    span: tok.span,
                })
            }
            TokenKind::Integer => {
                self.pos += 1;
                let value = parse_integer_lexeme(&tok.lexeme).ok_or_else(|| {
                    ParseError::new(format!("invalid integer `{}`", tok.lexeme), tok.span)
                })?;
                Ok(Expr::IntLit {
                    value,
                    span: tok.span,
                })
            }
            TokenKind::Real => {
                self.pos += 1;
                let value: f64 = tok.lexeme.parse().map_err(|_| {
                    ParseError::new(format!("invalid real `{}`", tok.lexeme), tok.span)
                })?;
                Ok(Expr::FloatLit {
                    value,
                    span: tok.span,
                })
            }
            TokenKind::String => {
                self.pos += 1;
                Ok(Expr::StringLit {
                    value: tok.lexeme,
                    span: tok.span,
                })
            }
            TokenKind::Character => {
                self.pos += 1;
                Ok(Expr::CharLit {
                    lexeme: tok.lexeme,
                    span: tok.span,
                })
            }
            TokenKind::Keyword if tok.lexeme == "TRUE" => {
                self.pos += 1;
                Ok(Expr::BoolLit {
                    value: true,
                    span: tok.span,
                })
            }
            TokenKind::Keyword if tok.lexeme == "FALSE" => {
                self.pos += 1;
                Ok(Expr::BoolLit {
                    value: false,
                    span: tok.span,
                })
            }
            TokenKind::Keyword if tok.lexeme == "VALOF" => {
                self.pos += 1;
                let body = self.parse_stmt()?;
                let span = SourceSpan {
                    start: tok.span.start,
                    end: body.span().end,
                };
                Ok(Expr::Valof {
                    body: Box::new(body),
                    span,
                })
            }
            TokenKind::Symbol if tok.lexeme == "?" => {
                self.pos += 1;
                Ok(Expr::Null { span: tok.span })
            }
            TokenKind::Symbol if tok.lexeme == "(" => {
                self.pos += 1;
                let inner = self.parse_expr()?;
                self.expect_sym(")")?;
                Ok(inner)
            }
            _ => Err(ParseError::new(
                format!("expected expression, got `{}`", tok.lexeme),
                tok.span,
            )),
        }
    }
}

fn parse_integer_lexeme(s: &str) -> Option<i64> {
    if let Some(rest) = s.strip_prefix('#') {
        if let Some(hex) = rest.strip_prefix(['X', 'x']) {
            return i64::from_str_radix(hex, 16).ok();
        }
        return i64::from_str_radix(rest, 8).ok();
    }
    s.parse::<i64>().ok()
}
