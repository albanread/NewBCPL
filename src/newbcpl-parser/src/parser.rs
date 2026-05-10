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
        if self.check_kw("LET") || self.check_kw("FLET") {
            // FLET is the float-typed LET. The parser treats it
            // identically; sema marks the resulting binding(s) as FLOAT
            // hint. Inside class bodies, FLET also serves as an
            // uninitialised member declaration (`FLET x`); that form is
            // handled when class parsing lands in the next chunk.
            return self.parse_let_decl();
        }
        if self.check_kw("GET") {
            return self.parse_get_decl();
        }
        if self.check_kw("MANIFEST") {
            return self.parse_manifest_decl();
        }
        if self.check_kw("STATIC") {
            return self.parse_static_decl();
        }
        if self.check_kw("GLOBAL") || self.check_kw("GLOBALS") {
            return self.parse_global_decl();
        }
        let span = self.peek().span;
        let lex = self.peek().lexeme.clone();
        Err(ParseError::new(
            format!("expected declaration, got `{lex}`"),
            span,
        ))
    }

    /// `GET "filename"` — include directive. The path is the raw lexeme
    /// of the string literal *with* its surrounding quotes stripped;
    /// `*`-escapes are not cooked here (sema or the file resolver does it).
    fn parse_get_decl(&mut self) -> Result<Decl, ParseError> {
        let kw = self.eat();
        let tok = self.peek().clone();
        if tok.kind != TokenKind::String {
            return Err(ParseError::new(
                format!("expected string literal after GET, got `{}`", tok.lexeme),
                tok.span,
            ));
        }
        self.pos += 1;
        let path = strip_quotes(&tok.lexeme).to_string();
        let span = SourceSpan {
            start: kw.span.start,
            end: tok.span.end,
        };
        Ok(Decl::Get(GetDirective { path, span }))
    }

    /// `MANIFEST $( name = expr; name = expr; ... $)`.
    fn parse_manifest_decl(&mut self) -> Result<Decl, ParseError> {
        let kw = self.eat();
        let bindings = self.parse_named_bindings_block(true)?;
        let span = SourceSpan {
            start: kw.span.start,
            end: bindings
                .last()
                .map(|b| b.span.end)
                .unwrap_or(kw.span.end),
        };
        Ok(Decl::Manifest(NamedBindingsDecl { bindings, span }))
    }

    /// Two forms:
    ///   `STATIC name`                           — single bare declaration
    ///   `STATIC $( name = expr; ... $)`         — block of initialised statics
    fn parse_static_decl(&mut self) -> Result<Decl, ParseError> {
        let kw = self.eat();
        let bindings = if self.check_sym("$(") || self.check_sym("{") {
            self.parse_named_bindings_block(false)?
        } else {
            // Bare `STATIC name` — record one binding with no initializer.
            let name_tok = self.eat_identifier()?;
            vec![NamedBinding {
                name: name_tok.lexeme,
                value: None,
                span: name_tok.span,
            }]
        };
        let span = SourceSpan {
            start: kw.span.start,
            end: bindings
                .last()
                .map(|b| b.span.end)
                .unwrap_or(kw.span.end),
        };
        Ok(Decl::Static(NamedBindingsDecl { bindings, span }))
    }

    /// `GLOBAL $( name : offset; ... $)` (classic) or
    /// `GLOBALS $( LET name = expr; ... $)` (dialect). Both forms share
    /// the same AST node; only the binding values' meaning differs.
    fn parse_global_decl(&mut self) -> Result<Decl, ParseError> {
        let kw = self.eat();
        let bindings = self.parse_named_bindings_block(false)?;
        let span = SourceSpan {
            start: kw.span.start,
            end: bindings
                .last()
                .map(|b| b.span.end)
                .unwrap_or(kw.span.end),
        };
        Ok(Decl::Global(NamedBindingsDecl { bindings, span }))
    }

    /// Parse a `$( … $)` or `{ … }` block of named bindings used by
    /// `MANIFEST`, `STATIC`, `GLOBAL`, and `GLOBALS`. Each binding is one
    /// of:
    ///   `name = expr`        (MANIFEST, STATIC initialised, GLOBALS-with-LET-stripped)
    ///   `name : expr`        (GLOBAL classic offset)
    ///   `name`               (STATIC bare, when `init_required` is false)
    ///   `LET name = expr`    (GLOBALS modern form; the LET is consumed and ignored)
    ///
    /// Bindings are separated by `;` or by a newline (any `;` is consumed
    /// silently). When `init_required` is true, an initialiser is
    /// mandatory (MANIFEST never omits values).
    fn parse_named_bindings_block(
        &mut self,
        init_required: bool,
    ) -> Result<Vec<NamedBinding>, ParseError> {
        let open = self.eat();
        let close_lex = match open.lexeme.as_str() {
            "$(" => "$)",
            "{" => "}",
            other => {
                return Err(ParseError::new(
                    format!("expected `$(` or `{{` to open block, got `{other}`"),
                    open.span,
                ));
            }
        };
        let mut bindings = Vec::new();
        loop {
            // Consume any number of separators (`;`).
            while self.check_sym(";") {
                self.eat();
            }
            if self.is_at_end() {
                return Err(ParseError::new(
                    format!("unterminated block — expected `{close_lex}`"),
                    open.span,
                ));
            }
            if self.check_sym(close_lex) {
                self.eat();
                return Ok(bindings);
            }
            // GLOBALS lets users write `LET name = expr` — strip the LET.
            if self.check_kw("LET") {
                self.eat();
            }
            let name_tok = self.eat_identifier()?;
            let value = if self.check_sym("=") || self.check_sym(":") {
                self.eat();
                Some(self.parse_expr()?)
            } else if init_required {
                let span = self.peek().span;
                let lex = self.peek().lexeme.clone();
                return Err(ParseError::new(
                    format!(
                        "expected `=` after `{}`, got `{lex}`",
                        name_tok.lexeme
                    ),
                    span,
                ));
            } else {
                None
            };
            let end = value
                .as_ref()
                .map(|e| e.span().end)
                .unwrap_or(name_tok.span.end);
            bindings.push(NamedBinding {
                name: name_tok.lexeme,
                value,
                span: SourceSpan {
                    start: name_tok.span.start,
                    end,
                },
            });
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
        if self.check_kw("LET")
            || self.check_kw("FLET")
            || self.check_kw("GET")
            || self.check_kw("MANIFEST")
            || self.check_kw("STATIC")
            || self.check_kw("GLOBAL")
            || self.check_kw("GLOBALS")
        {
            return Ok(Stmt::Decl(self.parse_decl()?));
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

        // Word-form unary operators: HD x, TL x, REST x, LEN x,
        // FREEVEC x, FREELIST x. All take a single operand and bind
        // tighter than any binary op, in line with the BCPL convention.
        let unary_kw = match self.peek().lexeme.as_str() {
            _ if self.peek().kind != TokenKind::Keyword => None,
            "HD" => Some(UnaryOp::Hd),
            "TL" => Some(UnaryOp::Tl),
            "REST" => Some(UnaryOp::Rest),
            "LEN" => Some(UnaryOp::Len),
            "FREEVEC" => Some(UnaryOp::FreeVec),
            "FREELIST" => Some(UnaryOp::FreeList),
            _ => None,
        };
        if let Some(op) = unary_kw {
            let kw = self.eat();
            let operand = self.parse_unary()?;
            let span = SourceSpan {
                start: kw.span.start,
                end: operand.span().end,
            };
            return Ok(Expr::Unary {
                op,
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
            TokenKind::Keyword if tok.lexeme == "VALOF" || tok.lexeme == "FVALOF" => {
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
            TokenKind::Keyword
                if matches!(tok.lexeme.as_str(), "VEC" | "FVEC") =>
            {
                // `VEC k` / `FVEC k` — single size argument, no parens.
                self.pos += 1;
                let kind = if tok.lexeme == "VEC" {
                    TypeConstructorKind::Vec
                } else {
                    TypeConstructorKind::FVec
                };
                let size = self.parse_unary()?;
                let span = SourceSpan {
                    start: tok.span.start,
                    end: size.span().end,
                };
                Ok(Expr::TypedConstruct {
                    kind,
                    args: vec![size],
                    span,
                })
            }
            TokenKind::Keyword
                if matches!(
                    tok.lexeme.as_str(),
                    "PAIR" | "FPAIR" | "QUAD" | "FQUAD" | "OCT" | "FOCT"
                    | "TABLE" | "FTABLE" | "LIST" | "MANIFESTLIST"
                ) =>
            {
                // `PAIR(a, b)`, `LIST(a, b, c)`, etc. — paren'd args.
                self.pos += 1;
                let kind = match tok.lexeme.as_str() {
                    "PAIR" => TypeConstructorKind::Pair,
                    "FPAIR" => TypeConstructorKind::FPair,
                    "QUAD" => TypeConstructorKind::Quad,
                    "FQUAD" => TypeConstructorKind::FQuad,
                    "OCT" => TypeConstructorKind::Oct,
                    "FOCT" => TypeConstructorKind::FOct,
                    "TABLE" => TypeConstructorKind::Table,
                    "FTABLE" => TypeConstructorKind::FTable,
                    "LIST" => TypeConstructorKind::List,
                    "MANIFESTLIST" => TypeConstructorKind::ManifestList,
                    _ => unreachable!(),
                };
                self.expect_sym("(")?;
                let mut args = Vec::new();
                if !self.check_sym(")") {
                    args.push(self.parse_expr()?);
                    while self.check_sym(",") {
                        self.eat();
                        // Allow a trailing comma:
                        // `LIST(1, 2, 3,)` is valid in the dialect.
                        if self.check_sym(")") {
                            break;
                        }
                        args.push(self.parse_expr()?);
                    }
                }
                let close = self.expect_sym(")")?;
                let span = SourceSpan {
                    start: tok.span.start,
                    end: close.span.end,
                };
                Ok(Expr::TypedConstruct { kind, args, span })
            }
            // Intrinsic conversion functions written as keywords:
            // FLOAT(n), TRUNC(f), FIX(f), FSQRT(f), ENTIER(f). Treat
            // them as if the keyword were an identifier so the postfix
            // call handling parses `(args)` naturally.
            TokenKind::Keyword
                if matches!(
                    tok.lexeme.as_str(),
                    "FLOAT" | "TRUNC" | "FIX" | "FSQRT" | "ENTIER"
                ) =>
            {
                self.pos += 1;
                Ok(Expr::Ident {
                    name: tok.lexeme,
                    span: tok.span,
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

/// The lexer keeps string lexemes with their surrounding `"` quotes;
/// for `GET "foo"` we want just the inner text. `*`-escapes are *not*
/// cooked here — sema or the file resolver does that — so the path we
/// hand back is exactly what was inside the quotes.
fn strip_quotes(s: &str) -> &str {
    let bytes = s.as_bytes();
    if bytes.len() >= 2 && bytes[0] == b'"' && bytes[bytes.len() - 1] == b'"' {
        &s[1..s.len() - 1]
    } else {
        s
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
