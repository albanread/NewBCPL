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
    Parser::new(tokens, source).parse_program()
}

pub(crate) struct Parser {
    tokens: Vec<Token>,
    pos: usize,
    /// Raw source bytes — used by `scan_asm_body` to extract the body
    /// text by offset rather than reconstructing it from tokens.
    source: String,
    /// Nesting depth of `.|…|` lane-access brackets currently open.
    /// While > 0 the binary-operator dispatcher refuses to consume `|`
    /// (which would otherwise eat the closing delimiter as a logical
    /// OR), so lane indices like `f.|i+1|` parse as expected.
    lane_depth: u32,
    /// Decls produced as side effects of parsing a chained
    /// declaration form like `LET f(...) = ... AND g(...) = ...`.
    /// `parse_let_decl` returns the first binding and pushes the
    /// rest here in source order; `parse_program` drains this
    /// buffer before its next `parse_decl()` call so the AND chain
    /// surfaces as independent top-level decls (matching how
    /// sema's pre-pass 2 preregisters every function name regardless).
    pending_decls: std::collections::VecDeque<Decl>,
}

impl Parser {
    fn new(tokens: Vec<Token>, source: &str) -> Self {
        Self {
            tokens,
            pos: 0,
            source: source.to_string(),
            lane_depth: 0,
            pending_decls: std::collections::VecDeque::new(),
        }
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

    /// Lookahead helper for the classical BCPL mutual-recursion
    /// declaration tail. Returns true iff the parser is currently
    /// looking at the three-token sequence `AND <identifier> (`,
    /// which is the only shape used by the `LET f(...) = ... AND
    /// g(...) = ...` form. `peek_binary_op` uses this to refuse to
    /// hand back `AND` as a logical operator at that position so the
    /// enclosing `parse_let_decl` can consume the `AND` itself. The
    /// expression form `expr AND name(args)` is rare enough that the
    /// false-positive cost is acceptable; users wanting that shape
    /// can write `expr AND (name(args))` to defeat the lookahead.
    fn looks_like_decl_tail_and(&self) -> bool {
        if !self.check_kw("AND") {
            return false;
        }
        let after_and = self.pos + 1;
        let Some(name_tok) = self.tokens.get(after_and) else {
            return false;
        };
        if name_tok.kind != TokenKind::Identifier {
            return false;
        }
        let Some(paren_tok) = self.tokens.get(after_and + 1) else {
            return false;
        };
        paren_tok.kind == TokenKind::Symbol && paren_tok.lexeme == "("
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
            // Drain any sibling decls a chained `LET … AND …` left
            // pending before consuming the next token. Each AND in a
            // mutual-recursion chain pushes another `Decl::Function`
            // or `Decl::Routine` here in source order.
            while let Some(d) = self.pending_decls.pop_front() {
                items.push(d);
            }
            if self.check_sym(";") {
                self.eat();
                continue;
            }
            items.push(self.parse_decl()?);
        }
        // Edge case: a chained LET appeared at the very tail of the
        // program — drain any decls that weren't picked up by the
        // loop body's check above.
        while let Some(d) = self.pending_decls.pop_front() {
            items.push(d);
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
        if self.check_kw("CLASS") {
            return self.parse_class_decl();
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

    /// `GLOBAL` declarations. Two surface shapes:
    ///   `GLOBAL name = expr`                              — single binding
    ///   `GLOBAL $( name = expr; name = expr; ... $)`      — batch
    ///
    /// Each binding becomes a module-scope (scope 0) variable backed
    /// by an LLVM module-level `@global`. Cross-module references
    /// resolve through the loader's symbol table — no fixed-offset
    /// slot vector. The classic `name : K` slot-pinning syntax is
    /// rejected; users wanting that should know it isn't available
    /// (it was the *GLOBALS* form, which we don't carry).
    ///
    /// `GLOBALS` (plural) is the legacy form: a single shared
    /// pointer vector indexed by integer offsets. NewBCPL has a real
    /// loader and doesn't need it, so we reject the keyword with a
    /// hint pointing at the modern form. The keyword is still
    /// reserved in the lexer so this diagnostic fires reliably.
    fn parse_global_decl(&mut self) -> Result<Decl, ParseError> {
        let kw = self.eat();
        if kw.lexeme == "GLOBALS" {
            return Err(ParseError::new(
                "GLOBALS (the classic slot-pinning global-vector form) is \
                 not supported in NewBCPL — the loader's symbol table \
                 already provides cross-module name resolution. Use \
                 `GLOBAL name = expr` (single) or \
                 `GLOBAL $( name = expr; ... $)` (block) instead.",
                kw.span,
            ));
        }
        let bindings = if self.check_sym("$(") || self.check_sym("{") {
            self.parse_named_bindings_block_with_options(
                /*init_required=*/ true,
                /*allow_colon=*/ false,
            )?
        } else {
            // Single-line form: `GLOBAL name = expr`.
            let name_tok = self.eat_identifier()?;
            self.expect_sym("=")?;
            let value = self.parse_expr()?;
            let end = value.span().end;
            vec![NamedBinding {
                name: name_tok.lexeme.clone(),
                value: Some(value),
                span: SourceSpan {
                    start: name_tok.span.start,
                    end,
                },
            }]
        };
        let span = SourceSpan {
            start: kw.span.start,
            end: bindings
                .last()
                .map(|b| b.span.end)
                .unwrap_or(kw.span.end),
        };
        Ok(Decl::Global(NamedBindingsDecl { bindings, span }))
    }

    /// `CLASS Name [EXTENDS Base] [MANAGED] $( ... $)`. The MANAGED
    /// keyword can appear before or after EXTENDS; we accept either.
    fn parse_class_decl(&mut self) -> Result<Decl, ParseError> {
        let kw = self.eat();
        let name = self.eat_identifier()?.lexeme;
        let mut extends = None;
        let mut managed = false;
        loop {
            if self.check_kw("EXTENDS") && extends.is_none() {
                self.eat();
                extends = Some(self.eat_identifier()?.lexeme);
                continue;
            }
            if self.check_kw("MANAGED") && !managed {
                self.eat();
                managed = true;
                continue;
            }
            break;
        }
        // Some reference programs write `CLASS Foo BE { ... }` —
        // the `BE` is the same keyword used as a routine-body
        // opener, repurposed here as a class-body marker. The
        // body bracket (`{` or `$(`) follows. We accept the
        // optional `BE` and then proceed with the normal opener
        // dispatch.
        if self.check_kw("BE") {
            self.eat();
        }
        if !self.check_sym("$(") && !self.check_sym("{") {
            let span = self.peek().span;
            let lex = self.peek().lexeme.clone();
            return Err(ParseError::new(
                format!("expected `$(` or `{{` after CLASS header, got `{lex}`"),
                span,
            ));
        }
        let open = self.eat();
        let close_lex = if open.lexeme == "$(" { "$)" } else { "}" };

        let mut current_visibility = Visibility::Public;
        let mut members: Vec<ClassMember> = Vec::new();
        // Loop body sets end_span before break; the initial value is
        // overwritten and never observed, so the compiler is right to
        // flag it. Use MaybeUninit-style "we'll assign before read."
        let end_span;

        loop {
            while self.check_sym(";") {
                self.eat();
            }
            if self.is_at_end() {
                return Err(ParseError::new(
                    format!("unterminated CLASS body — expected `{close_lex}`"),
                    open.span,
                ));
            }
            if self.check_sym(close_lex) {
                end_span = self.eat().span.end;
                break;
            }

            // Visibility section header: `PUBLIC:`, `PRIVATE:`, `PROTECTED:`.
            if (self.check_kw("PUBLIC")
                || self.check_kw("PRIVATE")
                || self.check_kw("PROTECTED"))
                && self.lookahead_is_colon()
            {
                let kw = self.eat();
                self.expect_sym(":")?;
                current_visibility = match kw.lexeme.as_str() {
                    "PUBLIC" => Visibility::Public,
                    "PRIVATE" => Visibility::Private,
                    "PROTECTED" => Visibility::Protected,
                    _ => unreachable!(),
                };
                continue;
            }

            members.push(self.parse_class_member(current_visibility)?);
        }

        Ok(Decl::Class(ClassDecl {
            name,
            extends,
            managed,
            members,
            span: SourceSpan {
                start: kw.span.start,
                end: end_span,
            },
        }))
    }

    /// True if the next non-current token is a `:` symbol (used to
    /// detect `PUBLIC:` style section headers without consuming the
    /// keyword).
    fn lookahead_is_colon(&self) -> bool {
        let next_pos = (self.pos + 1).min(self.tokens.len() - 1);
        let t = &self.tokens[next_pos];
        t.kind == TokenKind::Symbol && t.lexeme == ":"
    }

    /// Like `lookahead_is_colon`, but true only if the colon is `:`,
    /// NOT `:=`. The lexer emits `:=` as a single token, so this is
    /// just `lookahead_is_colon` written explicitly for clarity at
    /// the label-vs-assignment call site.
    fn lookahead_is_colon_not_assign(&self) -> bool {
        self.lookahead_is_colon()
    }

    /// If the next token is `AS`, consume `AS TypeIdent` and discard
    /// it. Used at LET binding sites; sema wires the annotation back
    /// in when type-annotation handling lands.
    /// Old API kept for callers that don't capture the annotation
    /// (FOREACH, VALOF) — same behaviour, return value discarded.
    fn skip_optional_as_annotation(&mut self) {
        let _ = self.parse_optional_as_annotation();
    }

    /// Parse an optional `AS Type` annotation and return its
    /// canonical string form (e.g. `"INTEGER"`, `"^STRING"`,
    /// `"^LIST OF INTEGER"`). Returns `None` when the next token
    /// isn't `AS`. Sema turns the string into a `TypeHint` via
    /// `type_hint_from_annotation`.
    ///
    /// Grammar (matching reference / corpus usage):
    ///   `AS` ty
    ///   ty   ::= '^' ty               -- pointer-to (also `POINTER TO`)
    ///          | base ('OF' ty)?      -- base + optional element chain
    ///   base ::= IDENT or any type keyword
    fn parse_optional_as_annotation(&mut self) -> Option<String> {
        if !self.check_kw("AS") {
            return None;
        }
        self.eat(); // AS
        let mut out = String::new();
        self.consume_type_annotation(&mut out);
        if out.is_empty() {
            None
        } else {
            Some(out)
        }
    }

    /// Body of an `AS` clause. Appends a canonical rendering of
    /// the type expression to `out`. Bails out at the first
    /// non-type token so the caller sees `=` / `,` / `IN` / `DO`
    /// / `BE` where it expects them.
    fn consume_type_annotation(&mut self, out: &mut String) {
        // Leading pointer markers — any number of `^` symbols or
        // `POINTER TO` keyword pairs, normalised to `^` in the
        // canonical string so sema only has to recognise one
        // form. Sema reads each leading `^` as one POINTER-TO
        // level.
        loop {
            if self.check_sym("^") {
                self.eat();
                out.push('^');
                continue;
            }
            if self.check_kw("POINTER") {
                self.eat();
                if self.check_kw("TO") {
                    self.eat();
                }
                out.push('^');
                continue;
            }
            break;
        }
        if !self.consume_type_base(out) {
            return;
        }
        while self.check_kw("OF") {
            self.eat();
            out.push_str(" OF ");
            loop {
                if self.check_sym("^") {
                    self.eat();
                    out.push('^');
                    continue;
                }
                if self.check_kw("POINTER") {
                    self.eat();
                    if self.check_kw("TO") {
                        self.eat();
                    }
                    out.push('^');
                    continue;
                }
                break;
            }
            if !self.consume_type_base(out) {
                return;
            }
        }
    }

    /// Consume one base type-name (identifier or recognised
    /// type keyword) and append its lexeme to `out`. Returns
    /// whether anything was consumed.
    fn consume_type_base(&mut self, out: &mut String) -> bool {
        let t = self.peek();
        match (t.kind, t.lexeme.as_str()) {
            (TokenKind::Identifier, _) => {
                out.push_str(&t.lexeme);
                self.pos += 1;
                true
            }
            (TokenKind::Keyword, "INTEGER")
            | (TokenKind::Keyword, "INT")
            | (TokenKind::Keyword, "FLOAT")
            | (TokenKind::Keyword, "WORD")
            | (TokenKind::Keyword, "STRING")
            | (TokenKind::Keyword, "ANY")
            | (TokenKind::Keyword, "LIST")
            | (TokenKind::Keyword, "VECTOR")
            | (TokenKind::Keyword, "OBJECT")
            | (TokenKind::Keyword, "PAIR")
            | (TokenKind::Keyword, "FPAIR")
            | (TokenKind::Keyword, "QUAD")
            | (TokenKind::Keyword, "FQUAD")
            | (TokenKind::Keyword, "OCT")
            | (TokenKind::Keyword, "FOCT")
            | (TokenKind::Keyword, "CHAR")
            | (TokenKind::Keyword, "BYTE") => {
                out.push_str(&t.lexeme);
                self.pos += 1;
                true
            }
            _ => false,
        }
    }

    fn parse_class_member(
        &mut self,
        visibility: Visibility,
    ) -> Result<ClassMember, ParseError> {
        // VIRTUAL and FINAL prefix method declarations.
        let mut is_virtual = false;
        let mut is_final = false;
        loop {
            if self.check_kw("VIRTUAL") && !is_virtual {
                self.eat();
                is_virtual = true;
                continue;
            }
            if self.check_kw("FINAL") && !is_final {
                self.eat();
                is_final = true;
                continue;
            }
            break;
        }

        if self.check_kw("ROUTINE") || self.check_kw("FUNCTION") {
            return self.parse_class_method(visibility, is_virtual, is_final);
        }
        // VIRTUAL / FINAL with no method keyword is a parse error.
        if is_virtual || is_final {
            let span = self.peek().span;
            let lex = self.peek().lexeme.clone();
            return Err(ParseError::new(
                format!(
                    "expected ROUTINE or FUNCTION after VIRTUAL/FINAL in class, got `{lex}`"
                ),
                span,
            ));
        }

        if self.check_kw("DECL") {
            return self.parse_class_field(visibility);
        }
        if self.check_kw("LET") {
            // Inside a class body, `LET` has three shapes:
            //   `LET x, y, z`                 — field declarations
            //                                   (equivalent to `DECL x, y, z`)
            //   `LET name = expr`             — initialised member
            //   `LET name(params) = expr`     — function-form method
            //   `LET name(params) BE stmt`    — routine-form method
            // We peek past the leading `LET name (, name)*` to see
            // whether `=`, `(`, or `AS` follows. If none of those, it's
            // a field-style declaration.
            if self.lookahead_is_field_let() {
                return self.parse_class_let_fields(visibility);
            }
            let start = self.peek().span.start;
            // `parse_let_decl` returns `Decl::Let` for value
            // bindings, `Decl::Function` for `LET name(...) = expr`,
            // and `Decl::Routine` for `LET name(...) BE stmt`.
            // Inside a class body the function / routine shapes are
            // methods; the value shape is an initialised member.
            // (The pre-existing class lowering called
            // `unreachable!("LET parses to Decl::Let")` here — that
            // was wrong for class methods written with the LET
            // function/routine grammar, blocking
            // `LET getX() = x` and `LET init(...) BE ...` style.)
            return match self.parse_let_decl()? {
                Decl::Let(let_decl) => {
                    let span = SourceSpan {
                        start,
                        end: let_decl.span.end,
                    };
                    Ok(ClassMember {
                        visibility,
                        kind: ClassMemberKind::Let(let_decl),
                        span,
                    })
                }
                Decl::Function(f) => {
                    let span = SourceSpan { start, end: f.span.end };
                    Ok(ClassMember {
                        visibility,
                        kind: ClassMemberKind::Method(ClassMethod {
                            name: f.name,
                            params: f.params,
                            param_annotations: f.param_annotations,
                            is_virtual,
                            is_final,
                            body: ClassMethodBody::Function(f.body),
                            span,
                        }),
                        span,
                    })
                }
                Decl::Routine(r) => {
                    let span = SourceSpan { start, end: r.span.end };
                    Ok(ClassMember {
                        visibility,
                        kind: ClassMemberKind::Method(ClassMethod {
                            name: r.name,
                            params: r.params,
                            param_annotations: r.param_annotations,
                            is_virtual,
                            is_final,
                            body: ClassMethodBody::Routine(r.body),
                            span,
                        }),
                        span,
                    })
                }
                other => unreachable!(
                    "parse_let_decl returned unexpected variant: {:?}",
                    other
                ),
            };
        }
        if self.check_kw("FLET") {
            return self.parse_class_flet_member(visibility);
        }

        let span = self.peek().span;
        let lex = self.peek().lexeme.clone();
        Err(ParseError::new(
            format!("expected class member (DECL / LET / FLET / ROUTINE / FUNCTION), got `{lex}`"),
            span,
        ))
    }

    /// Peek-only: is the current `LET`-led token sequence a class
    /// field declaration (no initialiser, no params)? Walks past
    /// the names without consuming, returns true iff the token
    /// after the last name is NOT `=`, `(`, or `AS` — i.e. the
    /// declaration ends with the name list. Restores nothing
    /// (this is a pure read of `self.tokens[self.pos..]`).
    fn lookahead_is_field_let(&self) -> bool {
        // Tokens: LET name (, name)* TERMINATOR ?
        let mut i = self.pos + 1; // skip LET
        loop {
            let t = self.tokens.get(i);
            let Some(t) = t else { return true };
            if t.kind != TokenKind::Identifier {
                return false;
            }
            i += 1;
            let next = self.tokens.get(i);
            match next {
                Some(t) if t.kind == TokenKind::Symbol && t.lexeme == "," => {
                    i += 1;
                    continue;
                }
                Some(t) if t.kind == TokenKind::Symbol && t.lexeme == "=" => return false,
                Some(t) if t.kind == TokenKind::Symbol && t.lexeme == "(" => return false,
                Some(t) if t.kind == TokenKind::Keyword && t.lexeme == "AS" => return false,
                _ => return true,
            }
        }
    }

    /// `LET x, y` inside a class body — field declarations equivalent
    /// to `DECL x, y`. Sema lays them out alongside DECL-introduced
    /// fields. The leading `LET` is consumed, then a comma-separated
    /// list of identifiers.
    fn parse_class_let_fields(
        &mut self,
        visibility: Visibility,
    ) -> Result<ClassMember, ParseError> {
        let kw = self.eat(); // LET
        let mut names = Vec::new();
        let mut annotations: Vec<Option<String>> = Vec::new();
        names.push(self.eat_identifier()?.lexeme);
        annotations.push(self.parse_optional_as_annotation());
        while self.check_sym(",") {
            self.eat();
            names.push(self.eat_identifier()?.lexeme);
            annotations.push(self.parse_optional_as_annotation());
        }
        let span = SourceSpan {
            start: kw.span.start,
            end: self.tokens[self.pos.saturating_sub(1)].span.end,
        };
        Ok(ClassMember {
            visibility,
            kind: ClassMemberKind::Fields { names, annotations },
            span,
        })
    }

    fn parse_class_field(&mut self, visibility: Visibility) -> Result<ClassMember, ParseError> {
        let kw = self.eat(); // DECL
        let mut names = Vec::new();
        let mut annotations: Vec<Option<String>> = Vec::new();
        names.push(self.eat_identifier()?.lexeme);
        annotations.push(self.parse_optional_as_annotation());
        while self.check_sym(",") {
            self.eat();
            names.push(self.eat_identifier()?.lexeme);
            annotations.push(self.parse_optional_as_annotation());
        }
        // Use the last identifier's span as a pragmatic end.
        let span = SourceSpan {
            start: kw.span.start,
            end: self.tokens[self.pos.saturating_sub(1)].span.end,
        };
        Ok(ClassMember {
            visibility,
            kind: ClassMemberKind::Fields { names, annotations },
            span,
        })
    }

    fn parse_class_flet_member(
        &mut self,
        visibility: Visibility,
    ) -> Result<ClassMember, ParseError> {
        let kw = self.eat(); // FLET
        let name_tok = self.eat_identifier()?;
        // Two shapes inside a class:
        //   FLET x        — uninitialised member
        //   FLET x = e    — initialised member
        let value = if self.check_sym("=") {
            self.eat();
            Some(self.parse_expr()?)
        } else {
            None
        };
        let end = value
            .as_ref()
            .map(|v| v.span().end)
            .unwrap_or(name_tok.span.end);
        let binding = NamedBinding {
            name: name_tok.lexeme,
            value,
            span: SourceSpan {
                start: name_tok.span.start,
                end,
            },
        };
        Ok(ClassMember {
            visibility,
            kind: ClassMemberKind::FLet(binding),
            span: SourceSpan {
                start: kw.span.start,
                end,
            },
        })
    }

    fn parse_class_method(
        &mut self,
        visibility: Visibility,
        is_virtual: bool,
        is_final: bool,
    ) -> Result<ClassMember, ParseError> {
        let kw = self.eat(); // ROUTINE or FUNCTION
        let is_function = kw.lexeme == "FUNCTION";
        let name = self.eat_identifier()?.lexeme;
        self.expect_sym("(")?;
        let mut params = Vec::new();
        let mut param_annotations: Vec<Option<String>> = Vec::new();
        if !self.check_sym(")") {
            params.push(self.eat_identifier()?.lexeme);
            param_annotations.push(self.parse_optional_as_annotation());
            while self.check_sym(",") {
                self.eat();
                params.push(self.eat_identifier()?.lexeme);
                param_annotations.push(self.parse_optional_as_annotation());
            }
        }
        self.expect_sym(")")?;

        // Body shape: classic BCPL pairs ROUTINE with `BE stmt`
        // and FUNCTION with `= expr`, but the reference's corpus
        // also writes `ROUTINE foo() = expr` and
        // `FUNCTION foo() BE stmt`. Accept either tail after
        // either keyword — both forms exist in production code,
        // and there's no semantic difference once the body is in
        // place. The `$(` / `{` implicit-block form (no `BE`)
        // is *not* accepted: every routine body must announce
        // itself with `BE` or `=`.
        let body = if self.check_sym("=") {
            self.eat();
            ClassMethodBody::Function(self.parse_expr()?)
        } else if self.check_kw("BE") {
            self.eat();
            ClassMethodBody::Routine(Box::new(self.parse_stmt()?))
        } else {
            let span = self.peek().span;
            let lex = self.peek().lexeme.clone();
            let kw_name = if is_function { "FUNCTION" } else { "ROUTINE" };
            return Err(ParseError::new(
                format!("expected `=` or `BE` after {kw_name} parameters, got `{lex}`"),
                span,
            ));
        };

        let end = match &body {
            ClassMethodBody::Function(e) => e.span().end,
            ClassMethodBody::Routine(s) => s.span().end,
        };
        let span = SourceSpan {
            start: kw.span.start,
            end,
        };
        Ok(ClassMember {
            visibility,
            kind: ClassMemberKind::Method(ClassMethod {
                name,
                params,
                param_annotations,
                is_virtual,
                is_final,
                body,
                span,
            }),
            span,
        })
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
        self.parse_named_bindings_block_with_options(init_required, /*allow_colon=*/ true)
    }

    /// Like `parse_named_bindings_block` but with a knob for whether
    /// the slot-pinning `name : K` syntax is allowed. GLOBAL passes
    /// `allow_colon=false` so that classic-BCPL slot syntax produces
    /// a clear diagnostic (the slot-vector form is GLOBALS, which we
    /// don't support).
    fn parse_named_bindings_block_with_options(
        &mut self,
        init_required: bool,
        allow_colon: bool,
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
            // GLOBALS lets users write `LET name = expr` (or `FLET …`)
            // — strip the leading binder keyword. Sema later picks up
            // FLET vs LET for the float-typing hint.
            if self.check_kw("LET") || self.check_kw("FLET") {
                self.eat();
            }
            let name_tok = self.eat_identifier()?;
            if self.check_sym(":") && !allow_colon {
                let span = self.peek().span;
                return Err(ParseError::new(
                    format!(
                        "`{}: K` slot-pinning is the classic GLOBALS form; \
                         NewBCPL replaces the global vector with the loader's \
                         symbol table. Use `{name} = expr` instead.",
                        name_tok.lexeme,
                        name = name_tok.lexeme,
                    ),
                    span,
                ));
            }
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

    /// Classical BCPL mutual-recursion declaration tail. After a
    /// `LET name(params) = body` or `LET name(params) BE body` has
    /// been parsed, peek for the chain continuation
    /// `AND name(params) = body` / `AND name(params) BE body` and
    /// push each sibling onto `pending_decls`. The shared scope
    /// semantics (each routine can call the others) are already
    /// satisfied by sema's pre-pass 2, which preregisters every
    /// function name before any body is analysed — so we just need
    /// to surface each sibling as an independent top-level decl.
    fn consume_mutual_recursion_chain(&mut self) -> Result<(), ParseError> {
        while self.looks_like_decl_tail_and() {
            self.eat(); // AND
            let start = self.peek().span.start;
            let name = self.eat_identifier()?.lexeme;
            self.expect_sym("(")?;
            let mut params = Vec::new();
            let mut param_annotations: Vec<Option<String>> = Vec::new();
            if !self.check_sym(")") {
                params.push(self.eat_identifier()?.lexeme);
                param_annotations.push(self.parse_optional_as_annotation());
                while self.check_sym(",") {
                    self.eat();
                    params.push(self.eat_identifier()?.lexeme);
                    param_annotations.push(self.parse_optional_as_annotation());
                }
            }
            self.expect_sym(")")?;
            // Optional return-type annotation: `… AS Type =`. Only
            // meaningful for ASM procedures (where it picks the
            // return register). Regular functions / routines parse
            // it for grammar uniformity but ignore the result —
            // sema infers their return type from the body.
            let return_annotation = self.parse_optional_as_annotation();
            if self.check_sym("=") {
                self.eat();
                if self.check_kw("ASM") {
                    self.eat();
                    let (body, end) = self.scan_asm_body()?;
                    self.pending_decls.push_back(Decl::AsmProc(AsmProcDecl {
                        name,
                        params,
                        param_annotations,
                        return_annotation,
                        is_function: true,
                        body,
                        span: SourceSpan { start, end },
                    }));
                } else {
                    let body = self.parse_expr()?;
                    let end = body.span().end;
                    self.pending_decls.push_back(Decl::Function(FunctionDecl {
                        name,
                        params,
                        param_annotations,
                        body,
                        span: SourceSpan { start, end },
                    }));
                }
            } else if self.check_kw("BE") {
                self.eat();
                if self.check_kw("ASM") {
                    self.eat();
                    let (body, end) = self.scan_asm_body()?;
                    self.pending_decls.push_back(Decl::AsmProc(AsmProcDecl {
                        name,
                        params,
                        param_annotations,
                        return_annotation: None,
                        is_function: false,
                        body,
                        span: SourceSpan { start, end },
                    }));
                } else {
                    let body = self.parse_stmt()?;
                    let end = body.span().end;
                    self.pending_decls.push_back(Decl::Routine(RoutineDecl {
                        name,
                        params,
                        param_annotations,
                        body: Box::new(body),
                        span: SourceSpan { start, end },
                    }));
                }
            } else {
                let span = self.peek().span;
                let lex = self.peek().lexeme.clone();
                return Err(ParseError::new(
                    format!(
                        "expected `=` or `BE` after `AND {name}(params)`, got `{lex}`"
                    ),
                    span,
                ));
            }
        }
        Ok(())
    }

    /// Scan an `ASM { … }` body after the `ASM` keyword has been consumed.
    ///
    /// Expects `{`, then walks tokens with a depth counter until the
    /// matching `}` so that nested `{…}` (e.g. macro bodies a future
    /// preprocessor might emit) survive verbatim. Returns
    /// `(body_text, closing_brace_end_position)` where `body_text` is
    /// the raw source slice between the braces — not a reconstruction
    /// from tokens, because we need to preserve the user's exact
    /// whitespace and any incidental punctuation for the assembler.
    ///
    /// The BCPL lexer still tokenises the body bytes (we walk those
    /// tokens to find the closing brace). Two constraints follow:
    ///
    /// 1. The body must be lexer-clean: an unmatched `"` or `*N`
    ///    outside a string literal still errors at lex time.
    /// 2. `//` and `/* … */` BCPL comments inside the body survive
    ///    into the assembler — GAS Intel syntax accepts `//` as a
    ///    line comment so this is usually harmless. Use `;` for
    ///    NASM-style comments if you prefer; GAS accepts that too.
    fn scan_asm_body(&mut self) -> Result<(String, newbcpl_lexer::SourcePosition), ParseError> {
        let open = self.expect_sym("{")?;
        let body_start = open.span.end.offset;
        let mut depth = 1usize;
        let mut close_start = body_start;
        let mut end_pos = open.span.end;

        while !self.is_at_end() {
            let tok = self.peek().clone();
            match tok.lexeme.as_str() {
                "{" => {
                    depth += 1;
                    end_pos = tok.span.end;
                    self.eat();
                }
                "}" => {
                    depth -= 1;
                    if depth == 0 {
                        close_start = tok.span.start.offset;
                        end_pos = tok.span.end;
                        self.eat();
                        break;
                    }
                    end_pos = tok.span.end;
                    self.eat();
                }
                _ => {
                    end_pos = tok.span.end;
                    self.eat();
                }
            }
        }

        if depth != 0 {
            let span = self.peek().span;
            return Err(ParseError::new(
                "unterminated ASM body — missing closing `}`",
                span,
            ));
        }

        let body = self.source[body_start..close_start].to_string();
        Ok((body, end_pos))
    }

    fn parse_let_decl(&mut self) -> Result<Decl, ParseError> {
        let let_token = self.eat();
        let kind = if let_token.lexeme == "FLET" {
            LetKind::FLet
        } else {
            LetKind::Let
        };
        let start = let_token.span.start;
        let first_name = self.eat_identifier()?.lexeme;

        if self.check_sym("(") {
            self.eat();
            let mut params = Vec::new();
            let mut param_annotations: Vec<Option<String>> = Vec::new();
            if !self.check_sym(")") {
                params.push(self.eat_identifier()?.lexeme);
                param_annotations.push(self.parse_optional_as_annotation());
                while self.check_sym(",") {
                    self.eat();
                    params.push(self.eat_identifier()?.lexeme);
                    param_annotations.push(self.parse_optional_as_annotation());
                }
            }
            self.expect_sym(")")?;
            // Optional return-type annotation: `… AS Type =`. See
            // the matching note in `consume_mutual_recursion_chain`
            // — regular functions / routines ignore it, ASM procs
            // use it to pick the return register class.
            let return_annotation = self.parse_optional_as_annotation();
            if self.check_sym("=") {
                self.eat();
                if self.check_kw("ASM") {
                    self.eat();
                    let (body, end) = self.scan_asm_body()?;
                    return Ok(Decl::AsmProc(AsmProcDecl {
                        name: first_name,
                        params,
                        param_annotations,
                        return_annotation,
                        is_function: true,
                        body,
                        span: SourceSpan { start, end },
                    }));
                }
                let body = self.parse_expr()?;
                let end = body.span().end;
                let first = Decl::Function(FunctionDecl {
                    name: first_name,
                    params,
                    param_annotations,
                    body,
                    span: SourceSpan { start, end },
                });
                self.consume_mutual_recursion_chain()?;
                return Ok(first);
            }
            if self.check_kw("BE") {
                self.eat();
                if self.check_kw("ASM") {
                    self.eat();
                    let (body, end) = self.scan_asm_body()?;
                    return Ok(Decl::AsmProc(AsmProcDecl {
                        name: first_name,
                        params,
                        param_annotations,
                        return_annotation: None,
                        is_function: false,
                        body,
                        span: SourceSpan { start, end },
                    }));
                }
                let body = self.parse_stmt()?;
                let end = body.span().end;
                let first = Decl::Routine(RoutineDecl {
                    name: first_name,
                    params,
                    param_annotations,
                    body: Box::new(body),
                    span: SourceSpan { start, end },
                });
                self.consume_mutual_recursion_chain()?;
                return Ok(first);
            }
            let span = self.peek().span;
            let lex = self.peek().lexeme.clone();
            return Err(ParseError::new(
                format!("expected `=` or `BE` after parameter list, got `{lex}`"),
                span,
            ));
        }

        // Plain binding: LET n1, n2, ... = e1, e2, ...
        // Each name may carry an optional `AS TypeIdent` annotation
        // which sema reads as a hint (manifesto §2). The parser
        // captures the type-expression's canonical string form so
        // sema can map it to a `TypeHint` without re-parsing.
        let mut names = vec![first_name];
        let mut annotations: Vec<Option<String>> = vec![self.parse_optional_as_annotation()];
        while self.check_sym(",") {
            self.eat();
            names.push(self.eat_identifier()?.lexeme);
            annotations.push(self.parse_optional_as_annotation());
        }
        self.expect_sym("=")?;
        let mut exprs = vec![self.parse_expr()?];
        while self.check_sym(",") {
            self.eat();
            exprs.push(self.parse_expr()?);
        }
        // Destructuring assignment: `LET a, b = single_pair` — N
        // names with one RHS unpacks the RHS's lanes into each
        // name. Same shape as `FOREACH (a, b) IN list-of-pairs`
        // but on a plain binding. We pair each name with a clone
        // of the RHS here; lower-time treats this as a single
        // evaluation + lane-extract per binding, gated by
        // `LetDecl.destructure`.
        let mut destructure = false;
        let bindings: Vec<(String, Expr)> = if names.len() != exprs.len() {
            if exprs.len() == 1 && names.len() > 1 {
                destructure = true;
                let only = exprs.into_iter().next().expect("exprs len 1");
                names
                    .into_iter()
                    .map(|n| (n, only.clone()))
                    .collect()
            } else {
                let span = exprs
                    .last()
                    .map(|e| e.span())
                    .unwrap_or(let_token.span);
                return Err(ParseError::new(
                    format!(
                        "LET binding has {} names but {} expressions",
                        names.len(),
                        exprs.len()
                    ),
                    span,
                ));
            }
        } else {
            names.into_iter().zip(exprs).collect()
        };
        let end = bindings
            .last()
            .map(|(_, e)| e.span().end)
            .unwrap_or(let_token.span.end);
        Ok(Decl::Let(LetDecl {
            bindings,
            annotations,
            destructure,
            span: SourceSpan { start, end },
            kind,
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
        // Label declaration: `name:` where the next token is a single
        // colon (NOT `:=`). We commit to label-shape only if the
        // lookahead matches, so this never disturbs an `ident := …`.
        if self.peek().kind == TokenKind::Identifier
            && self.lookahead_is_colon_not_assign()
        {
            let name_tok = self.eat();
            let colon = self.eat();
            return Ok(Stmt::Label {
                name: name_tok.lexeme,
                span: SourceSpan {
                    start: name_tok.span.start,
                    end: colon.span.end,
                },
            });
        }
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
        if self.check_kw("FOR") {
            return self.parse_for();
        }
        if self.check_kw("FOREACH") {
            return self.parse_foreach();
        }
        if self.check_kw("SWITCHON") {
            return self.parse_switchon();
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
        if self.check_kw("BRK") {
            return Ok(Stmt::Brk(self.eat().span));
        }
        if self.check_kw("GOTO") {
            let kw = self.eat();
            let label_tok = self.eat_identifier()?;
            return Ok(Stmt::Goto {
                label: label_tok.lexeme,
                span: SourceSpan {
                    start: kw.span.start,
                    end: label_tok.span.end,
                },
            });
        }
        if self.check_kw("RETAIN") {
            let kw = self.eat();
            let name_tok = self.eat_identifier()?;
            // Optional `= expr` makes RETAIN both declare and mark.
            let value = if self.check_sym("=") {
                self.eat();
                Some(self.parse_expr()?)
            } else {
                None
            };
            let end = value
                .as_ref()
                .map(|e| e.span().end)
                .unwrap_or(name_tok.span.end);
            return Ok(Stmt::Retain {
                name: name_tok.lexeme,
                value,
                span: SourceSpan {
                    start: kw.span.start,
                    end,
                },
            });
        }
        if self.check_kw("USING") {
            // `USING name = expr DO stmt`
            // Scope-deterministic resource form. The RELEASE method
            // on the bound value is called at scope exit; this binds
            // tightly enough to read like an ordinary statement
            // declarator, no `BE` ceremony required.
            let kw = self.eat();
            let name_tok = self.eat_identifier()?;
            self.expect_sym("=")?;
            let value = self.parse_expr()?;
            // `DO` is the keyword between expression and body. We
            // accept `THEN` too for symmetry with the rest of the
            // language, but `DO` is the documented spelling.
            if self.check_kw("DO") || self.check_kw("THEN") {
                self.eat();
            }
            let body = self.parse_stmt()?;
            let end = body.span().end;
            return Ok(Stmt::Using {
                name: name_tok.lexeme,
                value,
                body: Box::new(body),
                span: SourceSpan {
                    start: kw.span.start,
                    end,
                },
            });
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

    /// `FOR name = start TO end [BY step] DO body`.
    fn parse_for(&mut self) -> Result<Stmt, ParseError> {
        let kw = self.eat();
        let name = self.eat_identifier()?.lexeme;
        self.expect_sym("=")?;
        let start_expr = self.parse_expr()?;
        if !self.check_kw("TO") {
            let span = self.peek().span;
            let lex = self.peek().lexeme.clone();
            return Err(ParseError::new(
                format!("expected `TO` in FOR loop, got `{lex}`"),
                span,
            ));
        }
        self.eat();
        let end_expr = self.parse_expr()?;
        let step_expr = if self.check_kw("BY") {
            self.eat();
            Some(self.parse_expr()?)
        } else {
            None
        };
        if self.check_kw("DO") {
            self.eat();
        }
        let body = self.parse_stmt()?;
        let span = SourceSpan {
            start: kw.span.start,
            end: body.span().end,
        };
        Ok(Stmt::For {
            name,
            start: start_expr,
            end: end_expr,
            step: step_expr,
            body: Box::new(body),
            span,
        })
    }

    /// `FOREACH name [, name2] [AS Type] IN iterable DO body`
    /// — or its parenthesised destructuring form
    /// `FOREACH (name1, name2[, ...]) IN list-of-pairs DO body`.
    ///
    /// The parenthesised form is SIMD-lane unpack sugar (per the
    /// reference's `test_foreach_destructuring.bcl`): each list
    /// element is a packed value (PAIR / FPAIR / QUAD / ...) and
    /// the names bind to its lanes in order — `FOREACH (x, y) IN
    /// list-of-pairs` binds `x = element.|0|, y = element.|1|`
    /// per iteration. Sema validates that the name count matches
    /// the element type's lane count; lowering emits a
    /// `LaneExtract` per name. The non-parenthesised form keeps
    /// its original meaning (single-name iteration; the optional
    /// second name was an old `(idx, val)` shape).
    fn parse_foreach(&mut self) -> Result<Stmt, ParseError> {
        let kw = self.eat();
        let mut names: Vec<String> = Vec::new();
        if self.check_sym("(") {
            // Parenthesised destructuring: collect every comma-
            // separated identifier until the closing paren. Empty
            // parens (`FOREACH ()`) are ill-formed and rejected by
            // sema, but the parser accepts them so the diagnostic
            // can be produced later with a better source span.
            self.eat();
            if !self.check_sym(")") {
                names.push(self.eat_identifier()?.lexeme);
                while self.check_sym(",") {
                    self.eat();
                    names.push(self.eat_identifier()?.lexeme);
                }
            }
            if !self.check_sym(")") {
                let span = self.peek().span;
                let lex = self.peek().lexeme.clone();
                return Err(ParseError::new(
                    format!(
                        "expected `,` or `)` in FOREACH destructuring, got `{lex}`"
                    ),
                    span,
                ));
            }
            self.eat(); // ')'
        } else {
            names.push(self.eat_identifier()?.lexeme);
            if self.check_sym(",") {
                self.eat();
                names.push(self.eat_identifier()?.lexeme);
            }
        }
        let annotation = if self.check_kw("AS") {
            self.eat();
            // The full type-expression grammar is more involved
            // (POINTER TO X, OF X, etc.); for now accept a single
            // identifier which covers `INTEGER`, `FLOAT`, `WORD`, etc.
            Some(self.eat_identifier()?.lexeme)
        } else {
            None
        };
        if !self.check_kw("IN") {
            let span = self.peek().span;
            let lex = self.peek().lexeme.clone();
            return Err(ParseError::new(
                format!("expected `IN` in FOREACH, got `{lex}`"),
                span,
            ));
        }
        self.eat();
        let iter = self.parse_expr()?;
        if self.check_kw("DO") {
            self.eat();
        }
        let body = self.parse_stmt()?;
        let span = SourceSpan {
            start: kw.span.start,
            end: body.span().end,
        };
        Ok(Stmt::ForEach {
            names,
            annotation,
            iter,
            body: Box::new(body),
            span,
        })
    }

    /// `SWITCHON expr INTO $( CASE … : … ; DEFAULT : … $)`. Statements
    /// are accumulated until the next `CASE` / `DEFAULT` / closing
    /// bracket. Adjacent `CASE label:` headers preceding any statement
    /// are recorded as separate cases with empty bodies — the parser
    /// preserves source shape, sema/codegen interpret fall-through.
    fn parse_switchon(&mut self) -> Result<Stmt, ParseError> {
        let kw = self.eat();
        let scrutinee = self.parse_expr()?;
        if self.check_kw("INTO") {
            self.eat();
        }
        if !self.check_sym("$(") && !self.check_sym("{") {
            let span = self.peek().span;
            let lex = self.peek().lexeme.clone();
            return Err(ParseError::new(
                format!("expected `$(` or `{{` after SWITCHON, got `{lex}`"),
                span,
            ));
        }
        let open = self.eat();
        let close_lex = if open.lexeme == "$(" { "$)" } else { "}" };

        let mut cases: Vec<SwitchCase> = Vec::new();
        let mut default: Option<Vec<Stmt>> = None;
        // Loop body sets end_span before break; the initial value is
        // overwritten and never observed, so the compiler is right to
        // flag it. Use MaybeUninit-style "we'll assign before read."
        let end_span;

        loop {
            // Skip stray separators inside the SWITCHON body.
            while self.check_sym(";") {
                self.eat();
            }
            if self.is_at_end() {
                return Err(ParseError::new(
                    format!("unterminated SWITCHON — expected `{close_lex}`"),
                    open.span,
                ));
            }
            if self.check_sym(close_lex) {
                end_span = self.eat().span.end;
                break;
            }

            if self.check_kw("CASE") {
                let case_kw = self.eat();
                let value = self.parse_expr()?;
                self.expect_sym(":")?;
                let mut body = Vec::new();
                while !self.is_at_end()
                    && !self.check_kw("CASE")
                    && !self.check_kw("DEFAULT")
                    && !self.check_sym(close_lex)
                {
                    if self.check_sym(";") {
                        self.eat();
                        continue;
                    }
                    body.push(self.parse_stmt()?);
                }
                let case_end = body
                    .last()
                    .map(|s| s.span().end)
                    .unwrap_or(case_kw.span.end);
                cases.push(SwitchCase {
                    values: vec![value],
                    body,
                    span: SourceSpan {
                        start: case_kw.span.start,
                        end: case_end,
                    },
                });
                continue;
            }

            if self.check_kw("DEFAULT") {
                self.eat();
                self.expect_sym(":")?;
                let mut body = Vec::new();
                while !self.is_at_end()
                    && !self.check_kw("CASE")
                    && !self.check_kw("DEFAULT")
                    && !self.check_sym(close_lex)
                {
                    if self.check_sym(";") {
                        self.eat();
                        continue;
                    }
                    body.push(self.parse_stmt()?);
                }
                default = Some(body);
                continue;
            }

            let span = self.peek().span;
            let lex = self.peek().lexeme.clone();
            return Err(ParseError::new(
                format!("expected CASE, DEFAULT or `{close_lex}` in SWITCHON, got `{lex}`"),
                span,
            ));
        }

        Ok(Stmt::Switchon {
            scrutinee,
            cases,
            default,
            span: SourceSpan {
                start: kw.span.start,
                end: end_span,
            },
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
                hint: unknown_hint(),
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
                hint: unknown_hint(),
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
            // Multiplicative (precedence 7). Float-flavoured forms
            // accept either `.` or `#` as the trailing marker —
            // both are tokenised as one symbol by the lexer.
            (TokenKind::Symbol, "*") => Some((BinaryOp::Mul, 7)),
            (TokenKind::Symbol, "/") => Some((BinaryOp::Div, 7)),
            (TokenKind::Keyword, "REM") => Some((BinaryOp::Rem, 7)),
            (TokenKind::Symbol, "*.") | (TokenKind::Symbol, "*#") => {
                Some((BinaryOp::FMul, 7))
            }
            (TokenKind::Symbol, "/.") | (TokenKind::Symbol, "/#") => {
                Some((BinaryOp::FDiv, 7))
            }
            // Additive (precedence 6)
            (TokenKind::Symbol, "+") => Some((BinaryOp::Add, 6)),
            (TokenKind::Symbol, "-") => Some((BinaryOp::Sub, 6)),
            (TokenKind::Symbol, "+.") | (TokenKind::Symbol, "+#") => {
                Some((BinaryOp::FAdd, 6))
            }
            (TokenKind::Symbol, "-.") | (TokenKind::Symbol, "-#") => {
                Some((BinaryOp::FSub, 6))
            }
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
            (TokenKind::Symbol, "=.") | (TokenKind::Symbol, "=#") => {
                Some((BinaryOp::FEq, 4))
            }
            (TokenKind::Symbol, "~=.") | (TokenKind::Symbol, "~=#") => {
                Some((BinaryOp::FNe, 4))
            }
            (TokenKind::Symbol, "<.") | (TokenKind::Symbol, "<#") => {
                Some((BinaryOp::FLt, 4))
            }
            (TokenKind::Symbol, "<=.") | (TokenKind::Symbol, "<=#") => {
                Some((BinaryOp::FLe, 4))
            }
            (TokenKind::Symbol, ">.") | (TokenKind::Symbol, ">#") => {
                Some((BinaryOp::FGt, 4))
            }
            (TokenKind::Symbol, ">=.") | (TokenKind::Symbol, ">=#") => {
                Some((BinaryOp::FGe, 4))
            }
            // Precedence 3 — AND family:
            //   `&` symbol form → bitwise (`BitAnd`), matches C convention
            //   `BAND` keyword → bitwise
            //   `AND` keyword  → *logical* (returns 0/1), unless the
            //     token sequence `AND <ident> (` indicates a classical
            //     BCPL mutual-recursion declaration tail
            //     (`LET f(...) = ... AND g(...) = ...`). In that case
            //     we stop expression parsing here so the enclosing
            //     `parse_let_decl` can consume the `AND` and parse the
            //     second declaration. Users wanting `expr AND fn(x)`
            //     as a logical expression need parens: `expr AND (fn(x))`.
            (TokenKind::Symbol, "&") => Some((BinaryOp::BitAnd, 3)),
            (TokenKind::Keyword, "BAND") => Some((BinaryOp::BitAnd, 3)),
            (TokenKind::Keyword, "AND") if !self.looks_like_decl_tail_and() => {
                Some((BinaryOp::LogAnd, 3))
            }
            // Precedence 2 — OR / XOR / EQV family:
            //   `|` symbol form → bitwise (`BitOr`); inside a `.|…|`
            //     lane bracket the `|` is the closing delimiter, not
            //     an operator — see `lane_depth`.
            //   `BOR` / `BXOR` / `NEQV` keywords → bitwise
            //   `OR` / `XOR` keywords            → logical
            //   `EQV` is the single-value equality test (lowered to
            //     `==`); kept for back-compat.
            (TokenKind::Symbol, "|") if self.lane_depth == 0 => Some((BinaryOp::BitOr, 2)),
            (TokenKind::Keyword, "BOR") => Some((BinaryOp::BitOr, 2)),
            (TokenKind::Keyword, "BXOR") => Some((BinaryOp::BitXor, 2)),
            (TokenKind::Keyword, "OR") => Some((BinaryOp::LogOr, 2)),
            (TokenKind::Keyword, "XOR") => Some((BinaryOp::LogXor, 2)),
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
                hint: unknown_hint(),
            });
        }
        // Bitwise NOT: `~` symbol form, or `BNOT` keyword form. Flips
        // every bit of the operand.
        if self.check_sym("~") || self.check_kw("BNOT") {
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
                hint: unknown_hint(),
            });
        }
        // Logical NOT: `NOT` keyword. Returns 1 if the operand is 0,
        // else 0. Note this is *not* the same as `BNOT` / `~`.
        if self.check_kw("NOT") {
            let kw = self.eat();
            let operand = self.parse_unary()?;
            let span = SourceSpan {
                start: kw.span.start,
                end: operand.span().end,
            };
            return Ok(Expr::Unary {
                op: UnaryOp::LogNot,
                operand: Box::new(operand),
                span,
                hint: unknown_hint(),
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
                hint: unknown_hint(),
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
                hint: unknown_hint(),
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
                hint: unknown_hint(),
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
                hint: unknown_hint(),
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
                    hint: unknown_hint(),
                };
                continue;
            }

            // `%%` (bitfield access) is special: the RHS is a paren'd
            // `(start, width)` pair, e.g. `m %% (0, 8)`. Handle it
            // before the regular subscript family so the comma is
            // consumed correctly.
            if self.check_sym("%%") {
                self.eat();
                self.expect_sym("(")?;
                let start_arg = self.parse_expr()?;
                let width_arg = if self.check_sym(",") {
                    self.eat();
                    Some(self.parse_expr()?)
                } else {
                    None
                };
                let close = self.expect_sym(")")?;
                let span = SourceSpan {
                    start: expr.span().start,
                    end: close.span.end,
                };
                // Pack both args into the existing TypedConstruct shape:
                // `expr %% (start, width)` becomes a Call-shaped node
                // whose callee is a Binary { Bitfield, target, start }
                // and arg is `width`. Cleaner: new Expr variant. For
                // now reuse Binary by encoding (start, width) as a
                // Conditional-style triple — but that's hacky. Instead
                // synthesise a Call where the callee is the bitfield
                // expr and the args are the indices.
                //
                // Simplest correct shape that doesn't require a new
                // AST variant: nest the width into the rhs as a
                // Binary { Bitfield, start, width } — i.e. record
                // `target %% (start, width)` as
                //   Binary { Bitfield, target, Binary { Bitfield, start, width } }
                // when width is given, and
                //   Binary { Bitfield, target, start }
                // when it isn't. Sema unwraps. Documenting this in
                // the AST when sema lands.
                let rhs = match width_arg {
                    Some(w) => {
                        let inner_span = SourceSpan {
                            start: start_arg.span().start,
                            end: w.span().end,
                        };
                        Expr::Binary {
                            op: BinaryOp::Bitfield,
                            lhs: Box::new(start_arg),
                            rhs: Box::new(w),
                            span: inner_span,
                            hint: unknown_hint(),
                        }
                    }
                    None => start_arg,
                };
                expr = Expr::Binary {
                    op: BinaryOp::Bitfield,
                    lhs: Box::new(expr),
                    rhs: Box::new(rhs),
                    span,
                    hint: unknown_hint(),
                };
                continue;
            }

            // Subscript family — all are infix and have one expression
            // on the right. Note: the RHS uses `parse_unary` rather than
            // a full precedence-climb, so that `v!i + 1` parses as
            // `(v!i) + 1` (subscript binds tighter than `+`).
            let infix_subscript = if self.check_sym("!") {
                Some(BinaryOp::Subscript)
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
                    hint: unknown_hint(),
                };
                continue;
            }

            // `obj.field` (member access) or `pair.|n|` (SIMD lane).
            // The disambiguation is the token that follows the dot.
            if self.check_sym(".") {
                self.eat();
                if self.check_sym("|") {
                    // SIMD lane access: `expr . | index | …`
                    self.eat(); // opening |
                    self.lane_depth += 1;
                    let idx_result = self.parse_expr();
                    self.lane_depth -= 1;
                    let idx = idx_result?;
                    let close = self.expect_sym("|")?;
                    let span = SourceSpan {
                        start: expr.span().start,
                        end: close.span.end,
                    };
                    expr = Expr::Binary {
                        op: BinaryOp::LaneAccess,
                        lhs: Box::new(expr),
                        rhs: Box::new(idx),
                        span,
                        hint: unknown_hint(),
                    };
                    continue;
                }
                let field = self.eat_identifier()?;
                let rhs = Expr::Ident {
                    name: field.lexeme,
                    span: field.span,
                    hint: unknown_hint(),
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
                    hint: unknown_hint(),
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
                    hint: unknown_hint(),
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
                    hint: unknown_hint(),
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
                    hint: unknown_hint(),
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
                    hint: unknown_hint(),
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
                    hint: unknown_hint(),
                })
            }
            TokenKind::String => {
                self.pos += 1;
                Ok(Expr::StringLit {
                    value: tok.lexeme,
                    span: tok.span,
                    hint: unknown_hint(),
                })
            }
            TokenKind::Character => {
                self.pos += 1;
                Ok(Expr::CharLit {
                    lexeme: tok.lexeme,
                    span: tok.span,
                    hint: unknown_hint(),
                })
            }
            TokenKind::Keyword if tok.lexeme == "TRUE" => {
                self.pos += 1;
                Ok(Expr::BoolLit {
                    value: true,
                    span: tok.span,
                    hint: unknown_hint(),
                })
            }
            TokenKind::Keyword if tok.lexeme == "FALSE" => {
                self.pos += 1;
                Ok(Expr::BoolLit {
                    value: false,
                    span: tok.span,
                    hint: unknown_hint(),
                })
            }
            TokenKind::Keyword if tok.lexeme == "VALOF" || tok.lexeme == "FVALOF" => {
                self.pos += 1;
                // `VALOF AS Type $(...)` — optional return-type
                // annotation. Sema uses the hint to seed the
                // enclosing function's result type when the
                // RESULTIS expressions are otherwise inscrutable.
                // We just skip past it for now; richer wiring
                // (annotation → Expr::Valof.hint) lands when sema
                // grows a `valof_annotation` reader.
                self.skip_optional_as_annotation();
                let body = self.parse_stmt()?;
                let span = SourceSpan {
                    start: tok.span.start,
                    end: body.span().end,
                };
                Ok(Expr::Valof {
                    body: Box::new(body),
                    span,
                    hint: unknown_hint(),
                })
            }
            TokenKind::Keyword
                if matches!(tok.lexeme.as_str(), "VEC" | "FVEC") =>
            {
                // Two shapes:
                //   `VEC k`         — single size; allocates a vector
                //                     of `k+1` words.
                //   `VEC [e1, e2, …]` — inline initialiser; the size
                //                       comes from the element count.
                self.pos += 1;
                let kind = if tok.lexeme == "VEC" {
                    TypeConstructorKind::Vec
                } else {
                    TypeConstructorKind::FVec
                };
                if self.check_sym("[") {
                    self.eat();
                    let mut args = Vec::new();
                    if !self.check_sym("]") {
                        args.push(self.parse_expr()?);
                        while self.check_sym(",") {
                            self.eat();
                            if self.check_sym("]") {
                                break;
                            }
                            args.push(self.parse_expr()?);
                        }
                    }
                    let close = self.expect_sym("]")?;
                    return Ok(Expr::TypedConstruct {
                        kind,
                        args,
                        span: SourceSpan {
                            start: tok.span.start,
                            end: close.span.end,
                        },
                        hint: unknown_hint(),
                    });
                }
                // Paren-initialiser form: `VEC(a, b, c)` —
                // semantically equivalent to `VEC [a, b, c]`.
                // The reference's corpus mixes both syntaxes; the
                // bracket form is the original BCPL, the paren
                // form a later sugar. Trailing comma tolerated
                // (`VEC(a, b,)` and `VEC()`).
                if self.check_sym("(") {
                    self.eat();
                    let mut args = Vec::new();
                    if !self.check_sym(")") {
                        args.push(self.parse_expr()?);
                        while self.check_sym(",") {
                            self.eat();
                            if self.check_sym(")") {
                                break;
                            }
                            args.push(self.parse_expr()?);
                        }
                    }
                    let close = self.expect_sym(")")?;
                    return Ok(Expr::TypedConstruct {
                        kind,
                        args,
                        span: SourceSpan {
                            start: tok.span.start,
                            end: close.span.end,
                        },
                        hint: unknown_hint(),
                    });
                }
                let size = self.parse_unary()?;
                let span = SourceSpan {
                    start: tok.span.start,
                    end: size.span().end,
                };
                Ok(Expr::TypedConstruct {
                    kind,
                    args: vec![size],
                    span,
                    hint: unknown_hint(),
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
                Ok(Expr::TypedConstruct { kind, args, span,
                    hint: unknown_hint(),
                })
            }
            // Intrinsic conversion functions written as keywords:
            // FLOAT(n), TRUNC(f), FIX(f), FSQRT(f), ENTIER(f). Treat
            // them as if the keyword were an identifier so the postfix
            // call handling parses `(args)` naturally.
            TokenKind::Keyword
                if matches!(
                    tok.lexeme.as_str(),
                    "FLOAT" | "TRUNC" | "FIX" | "FSQRT" | "ENTIER" | "TYPE" | "TYPEOF"
                ) =>
            {
                self.pos += 1;
                Ok(Expr::Ident {
                    name: tok.lexeme,
                    span: tok.span,
                    hint: unknown_hint(),
                })
            }
            // SELF and SUPER — surface as identifier-shaped expressions
            // so member access (`SELF.x`, `SUPER.method(...)`) parses
            // through the existing postfix `.` handling. Sema gives
            // them their object-receiver semantics.
            TokenKind::Keyword if matches!(tok.lexeme.as_str(), "SELF" | "SUPER") => {
                self.pos += 1;
                Ok(Expr::Ident {
                    name: tok.lexeme,
                    span: tok.span,
                    hint: unknown_hint(),
                })
            }
            // `NEW Class` or `NEW Class(args)` — heap object construction.
            TokenKind::Keyword if tok.lexeme == "NEW" => {
                self.pos += 1;
                let class_tok = self.eat_identifier()?;
                let mut args = Vec::new();
                let mut end = class_tok.span.end;
                if self.check_sym("(") {
                    self.eat();
                    if !self.check_sym(")") {
                        args.push(self.parse_expr()?);
                        while self.check_sym(",") {
                            self.eat();
                            if self.check_sym(")") {
                                break;
                            }
                            args.push(self.parse_expr()?);
                        }
                    }
                    let close = self.expect_sym(")")?;
                    end = close.span.end;
                }
                Ok(Expr::New {
                    class_name: class_tok.lexeme,
                    args,
                    span: SourceSpan {
                        start: tok.span.start,
                        end,
                    },
                    hint: unknown_hint(),
                })
            }
            TokenKind::Symbol if tok.lexeme == "?" => {
                self.pos += 1;
                Ok(Expr::Null { span: tok.span,
                    hint: unknown_hint(),
                })
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
