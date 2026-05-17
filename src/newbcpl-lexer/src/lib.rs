//! NewBCPL lexer.
//!
//! Tokenises the BCPL dialect described in
//! `reference/documentation/BCPL syntax.md` and the dotted-float extension
//! in `reference/documentation/BCPL float extension.md`. The lexer produces
//! a flat stream of `Token` values plus stable diagnostics in `LexError`.
//!
//! Design points worth noting:
//!
//! - Section brackets are written `$(` and `$)`. The TX-2 / Apple BCPL
//!   tradition also accepts `{` and `}` as direct synonyms; both are
//!   retained verbatim in the lexeme so a downstream pretty-printer can
//!   reproduce the source style.
//! - Strings use the BCPL `*` escape convention, not C-style `\`. Inside
//!   string and character literals, `*N`/`*T`/`*S`/`*B`/`*P`/`*C`/`*"`/`**`
//!   are the recognised escapes. Multiplication never appears inside a
//!   string, so the dual role of `*` is unambiguous to the lexer.
//! - Numbers come in three bases: decimal, octal `#777`, and hex `#X1A` /
//!   `#x1a`. A decimal token is reclassified as `Real` if it carries a `.`
//!   fraction or an `e`/`E` exponent.
//! - Dotted operators (`+.`, `-.`, `*.`, `/.`, `=.`, `~=.`, `<.`, `<=.`,
//!   `>.`, `>=.`, `.%`) are emitted as single symbol tokens so the parser
//!   can dispatch on them directly.

use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourcePosition {
    pub line: usize,
    pub column: usize,
    pub offset: usize,
}

impl SourcePosition {
    fn start() -> Self {
        Self {
            line: 1,
            column: 1,
            offset: 0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceSpan {
    pub start: SourcePosition,
    pub end: SourcePosition,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenKind {
    Keyword,
    Identifier,
    Integer,
    Real,
    Character,
    String,
    Symbol,
    Eof,
}

impl TokenKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Keyword => "keyword",
            Self::Identifier => "identifier",
            Self::Integer => "integer",
            Self::Real => "real",
            Self::Character => "character",
            Self::String => "string",
            Self::Symbol => "symbol",
            Self::Eof => "eof",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    pub kind: TokenKind,
    pub lexeme: String,
    pub span: SourceSpan,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LexError {
    pub message: String,
    pub span: SourceSpan,
}

impl LexError {
    fn new(message: impl Into<String>, span: SourceSpan) -> Self {
        Self {
            message: message.into(),
            span,
        }
    }

    pub fn render(&self) -> String {
        format!(
            "{} at {}:{}",
            self.message, self.span.start.line, self.span.start.column
        )
    }
}

/// Reserved words. Comparison is case-sensitive per the canonical BCPL
/// convention (keywords are upper-case; identifiers may be either case
/// but the convention is lower-case).
const KEYWORDS: &[&str] = &[
    // 1974 Richards core
    "LET", "AND", "BE", "VALOF", "RESULTIS",
    "MANIFEST", "STATIC", "GLOBAL", "GLOBALS", "VEC", "TABLE", "OF",
    "IF", "UNLESS", "TEST", "THEN", "ELSE", "OR", "DO",
    "WHILE", "UNTIL", "REPEAT", "REPEATWHILE", "REPEATUNTIL",
    "FOR", "TO", "BY",
    "SWITCHON", "INTO", "CASE", "DEFAULT", "ENDCASE",
    "GOTO", "RETURN", "FINISH", "BREAK", "LOOP",
    "TRUE", "FALSE",
    // Logical (truthiness-based, return 0/1)
    "NOT", "XOR",
    // Bitwise (operate on every bit)
    "BAND", "BOR", "BXOR", "BNOT",
    // Remaining classics: REM is integer remainder, EQV/NEQV are
    // bitwise xnor/xor kept for backward compatibility (NEQV is
    // a synonym for BXOR; EQV is a single-value equality test
    // historically expressed bitwise).
    "REM", "EQV", "NEQV",
    "GET",
    // Dialect extensions: see reference/documentation/BCPL float extension.md
    // and reference/documentation/classes_and_objects.md
    "FLET", "FSTATIC", "FVEC", "FTABLE", "FVALOF",
    "FUNCTION", "ROUTINE",
    "CLASS", "EXTENDS", "DECL", "NEW",
    "VIRTUAL", "FINAL", "MANAGED",
    "PUBLIC", "PRIVATE", "PROTECTED",
    "SELF", "SUPER",
    "RETAIN", "FREEVEC", "FREELIST",
    "USING",
    "FLOAT", "TRUNC", "FIX", "FSQRT", "ENTIER",
    "FOREACH", "IN",
    "LIST", "MANIFESTLIST",
    "HD", "TL", "REST",
    "LEN", "TYPEOF", "TYPE",
    "AS", "POINTER",
    "DEFER", "BRK",
    "PAIR", "FPAIR", "QUAD", "FQUAD", "OCT", "FOCT",
    // Inline / procedure-body assembly.
    "ASM",
];

fn is_keyword(word: &str) -> bool {
    KEYWORDS.iter().any(|kw| *kw == word)
}

fn is_ident_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_'
}

fn is_ident_continue(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '.'
}

/// Lex a complete source buffer into a vector of tokens (plus a final
/// `Eof`). Halts on the first error.
pub fn lex_source(source: &str) -> Result<Vec<Token>, LexError> {
    let mut lexer = Lexer::new(source);
    let mut tokens = Vec::new();
    loop {
        let token = lexer.next_token()?;
        let is_eof = token.kind == TokenKind::Eof;
        tokens.push(token);
        if is_eof {
            return Ok(tokens);
        }
    }
}

/// Read a file and produce a textual token dump. The format mirrors
/// `newcp-driver dump-tokens` so the two compilers' phase artifacts can be
/// diffed and reviewed in the same pipeline.
pub fn dump_tokens(path: &Path) -> String {
    match std::fs::read_to_string(path) {
        Ok(source_text) => match lex_source(&source_text) {
            Ok(tokens) => {
                let rendered = if tokens.is_empty() {
                    "<none>".to_string()
                } else {
                    tokens
                        .iter()
                        .map(render_token)
                        .collect::<Vec<_>>()
                        .join("\n")
                };

                format!(
                    "newbcpl-lexer token dump\ninput: {}\ntoken-count: {}\n{}",
                    path.display(),
                    tokens.len(),
                    rendered
                )
            }
            Err(error) => format!(
                "newbcpl-lexer token dump\ninput: {}\nerror: {}",
                path.display(),
                error.render()
            ),
        },
        Err(error) => format!(
            "newbcpl-lexer token dump\ninput: {}\nio-error: {}",
            path.display(),
            error
        ),
    }
}

fn render_token(token: &Token) -> String {
    let SourceSpan { start, end } = token.span;
    format!(
        "{:>4}:{:<3} - {:>4}:{:<3} {:<10} {}",
        start.line,
        start.column,
        end.line,
        end.column,
        token.kind.as_str(),
        escape_lexeme(&token.lexeme)
    )
}

fn escape_lexeme(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\x{:02x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

struct Lexer<'a> {
    source: &'a str,
    bytes: &'a [u8],
    pos: SourcePosition,
}

impl<'a> Lexer<'a> {
    fn new(source: &'a str) -> Self {
        // Tolerate a UTF-8 BOM at the head of the file; many editors add it
        // silently and the dialect treats source as ASCII otherwise.
        let bom: &[u8] = b"\xEF\xBB\xBF";
        let bytes = source.as_bytes();
        let mut pos = SourcePosition::start();
        if bytes.starts_with(bom) {
            pos.offset = bom.len();
        }
        Self {
            source,
            bytes,
            pos,
        }
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos.offset).copied()
    }

    fn peek_at(&self, lookahead: usize) -> Option<u8> {
        self.bytes.get(self.pos.offset + lookahead).copied()
    }

    fn advance(&mut self) -> Option<u8> {
        let byte = self.peek()?;
        self.pos.offset += 1;
        if byte == b'\n' {
            self.pos.line += 1;
            self.pos.column = 1;
        } else {
            self.pos.column += 1;
        }
        Some(byte)
    }

    fn span_from(&self, start: SourcePosition) -> SourceSpan {
        SourceSpan {
            start,
            end: self.pos,
        }
    }

    fn slice_from(&self, start: usize) -> &'a str {
        &self.source[start..self.pos.offset]
    }

    fn skip_trivia(&mut self) -> Result<(), LexError> {
        loop {
            match self.peek() {
                Some(b) if b.is_ascii_whitespace() => {
                    self.advance();
                }
                Some(b'/') if self.peek_at(1) == Some(b'/') => {
                    while let Some(b) = self.peek() {
                        if b == b'\n' {
                            break;
                        }
                        self.advance();
                    }
                }
                Some(b'/') if self.peek_at(1) == Some(b'*') => {
                    let start = self.pos;
                    self.advance(); // /
                    self.advance(); // *
                    loop {
                        match self.peek() {
                            None => {
                                return Err(LexError::new(
                                    "unterminated /* … */ comment",
                                    self.span_from(start),
                                ));
                            }
                            Some(b'*') if self.peek_at(1) == Some(b'/') => {
                                self.advance();
                                self.advance();
                                break;
                            }
                            Some(_) => {
                                self.advance();
                            }
                        }
                    }
                }
                _ => return Ok(()),
            }
        }
    }

    fn next_token(&mut self) -> Result<Token, LexError> {
        self.skip_trivia()?;
        let start = self.pos;
        let Some(byte) = self.peek() else {
            return Ok(Token {
                kind: TokenKind::Eof,
                lexeme: String::new(),
                span: self.span_from(start),
            });
        };

        match byte {
            b'a'..=b'z' | b'A'..=b'Z' | b'_' => self.lex_identifier_or_keyword(start),
            b'0'..=b'9' => self.lex_decimal_number(start),
            b'#' => self.lex_hash_number(start),
            b'"' => self.lex_string(start),
            b'\'' => self.lex_character(start),
            b'$' => self.lex_dollar(start),
            _ => self.lex_symbol(start),
        }
    }

    fn lex_identifier_or_keyword(&mut self, start: SourcePosition) -> Result<Token, LexError> {
        let start_offset = self.pos.offset;
        debug_assert!(matches!(self.peek(), Some(b) if is_ident_start(b as char)));
        self.advance();
        // Identifiers do not include `.` — that is reserved for member
        // access (`obj.field`) and for the dotted-float operator family.
        while let Some(b) = self.peek() {
            let c = b as char;
            if c.is_ascii_alphanumeric() || c == '_' {
                self.advance();
            } else {
                break;
            }
        }
        let _ = is_ident_continue; // referenced only by docs above
        let lexeme = self.slice_from(start_offset).to_string();
        let kind = if is_keyword(&lexeme) {
            TokenKind::Keyword
        } else {
            TokenKind::Identifier
        };
        Ok(Token {
            kind,
            lexeme,
            span: self.span_from(start),
        })
    }

    fn lex_decimal_number(&mut self, start: SourcePosition) -> Result<Token, LexError> {
        let start_offset = self.pos.offset;
        let mut is_real = false;

        while let Some(b) = self.peek() {
            if b.is_ascii_digit() {
                self.advance();
            } else {
                break;
            }
        }

        // Fractional part: a `.` followed by a digit. We must NOT consume a
        // `.` followed by a non-digit, because that would swallow the `.%`
        // operator or a member access like `obj.field`.
        if self.peek() == Some(b'.')
            && self.peek_at(1).map(|b| b.is_ascii_digit()).unwrap_or(false)
        {
            is_real = true;
            self.advance(); // .
            while let Some(b) = self.peek() {
                if b.is_ascii_digit() {
                    self.advance();
                } else {
                    break;
                }
            }
        }

        // Exponent.
        if matches!(self.peek(), Some(b'e' | b'E')) {
            let mut lookahead = 1;
            if matches!(self.peek_at(lookahead), Some(b'+' | b'-')) {
                lookahead += 1;
            }
            if self
                .peek_at(lookahead)
                .map(|b| b.is_ascii_digit())
                .unwrap_or(false)
            {
                is_real = true;
                self.advance(); // e/E
                if matches!(self.peek(), Some(b'+' | b'-')) {
                    self.advance();
                }
                while let Some(b) = self.peek() {
                    if b.is_ascii_digit() {
                        self.advance();
                    } else {
                        break;
                    }
                }
            }
        }

        let lexeme = self.slice_from(start_offset).to_string();
        let kind = if is_real {
            TokenKind::Real
        } else {
            TokenKind::Integer
        };
        Ok(Token {
            kind,
            lexeme,
            span: self.span_from(start),
        })
    }

    /// `#777` (octal) or `#X1A` / `#x1a` (hex). We keep the leading `#` so a
    /// pretty-printer can round-trip the original syntax.
    fn lex_hash_number(&mut self, start: SourcePosition) -> Result<Token, LexError> {
        let start_offset = self.pos.offset;
        self.advance(); // #
        let is_hex = matches!(self.peek(), Some(b'X' | b'x'));
        // Bare `#` not followed by a digit or hex prefix — emit it as
        // a Symbol so that incidental `#` characters inside an ASM
        // body tokenise cleanly. The parser captures the body text
        // via source offsets (not by reconstructing it from tokens),
        // so the Symbol is consumed immediately by `scan_asm_body`'s
        // brace counter and the raw bytes survive into the assembler
        // verbatim. A common use is the GAS-style line-comment
        // `# this is a comment` inside an ASM body.
        if !is_hex && !matches!(self.peek(), Some(b'0'..=b'9')) {
            return Ok(Token {
                kind: TokenKind::Symbol,
                lexeme: self.slice_from(start_offset).to_string(),
                span: self.span_from(start),
            });
        }
        if is_hex {
            self.advance();
            let mut saw_digit = false;
            while let Some(b) = self.peek() {
                if b.is_ascii_hexdigit() {
                    self.advance();
                    saw_digit = true;
                } else {
                    break;
                }
            }
            if !saw_digit {
                return Err(LexError::new(
                    "hex number `#X` requires at least one hex digit",
                    self.span_from(start),
                ));
            }
        } else {
            let mut saw_digit = false;
            while let Some(b) = self.peek() {
                if matches!(b, b'0'..=b'7') {
                    self.advance();
                    saw_digit = true;
                } else if matches!(b, b'8' | b'9') {
                    return Err(LexError::new(
                        format!("digit `{}` is not valid in an octal literal", b as char),
                        self.span_from(start),
                    ));
                } else {
                    break;
                }
            }
            if !saw_digit {
                return Err(LexError::new(
                    "octal number `#` requires at least one octal digit",
                    self.span_from(start),
                ));
            }
        }
        let lexeme = self.slice_from(start_offset).to_string();
        Ok(Token {
            kind: TokenKind::Integer,
            lexeme,
            span: self.span_from(start),
        })
    }

    fn lex_string(&mut self, start: SourcePosition) -> Result<Token, LexError> {
        let start_offset = self.pos.offset;
        self.advance(); // opening "
        loop {
            match self.peek() {
                None => {
                    return Err(LexError::new(
                        "unterminated string literal",
                        self.span_from(start),
                    ));
                }
                Some(b'\n') => {
                    return Err(LexError::new(
                        "newline in string literal — close the string before the line break",
                        self.span_from(start),
                    ));
                }
                Some(b'"') => {
                    self.advance();
                    let lexeme = self.slice_from(start_offset).to_string();
                    return Ok(Token {
                        kind: TokenKind::String,
                        lexeme,
                        span: self.span_from(start),
                    });
                }
                Some(b'*') => {
                    // BCPL `*` escape. Consume `*` plus the following byte
                    // verbatim. A trailing `*` with nothing after it is a
                    // hard error, the same as in the reference compiler.
                    self.advance();
                    if self.peek().is_none() {
                        return Err(LexError::new(
                            "unterminated `*` escape in string literal",
                            self.span_from(start),
                        ));
                    }
                    self.advance();
                }
                Some(_) => {
                    self.advance();
                }
            }
        }
    }

    fn lex_character(&mut self, start: SourcePosition) -> Result<Token, LexError> {
        let start_offset = self.pos.offset;
        self.advance(); // opening '
        // Body: either an escape `*x` or a single non-quote byte.
        match self.peek() {
            None => {
                return Err(LexError::new(
                    "unterminated character literal",
                    self.span_from(start),
                ));
            }
            Some(b'*') => {
                self.advance();
                if self.peek().is_none() {
                    return Err(LexError::new(
                        "unterminated `*` escape in character literal",
                        self.span_from(start),
                    ));
                }
                self.advance();
            }
            Some(b'\'') => {
                return Err(LexError::new(
                    "empty character literal `''`",
                    self.span_from(start),
                ));
            }
            Some(b'\n') => {
                return Err(LexError::new(
                    "newline in character literal",
                    self.span_from(start),
                ));
            }
            Some(_) => {
                self.advance();
            }
        }
        if self.peek() != Some(b'\'') {
            return Err(LexError::new(
                "character literal missing closing `'`",
                self.span_from(start),
            ));
        }
        self.advance();
        let lexeme = self.slice_from(start_offset).to_string();
        Ok(Token {
            kind: TokenKind::Character,
            lexeme,
            span: self.span_from(start),
        })
    }

    /// `$(` or `$)`, optionally with a trailing tag identifier so that
    /// `$(LOOP … $)LOOP` round-trips as the two lexemes `$(LOOP` and
    /// `$)LOOP`. A stray `$` not followed by a bracket is a hard error.
    fn lex_dollar(&mut self, start: SourcePosition) -> Result<Token, LexError> {
        let start_offset = self.pos.offset;
        self.advance(); // $
        match self.peek() {
            Some(b'(') | Some(b')') => {
                self.advance();
                // Optional tag, attached to the bracket.
                if self.peek().map(|b| is_ident_start(b as char)).unwrap_or(false) {
                    while let Some(b) = self.peek() {
                        if (b as char).is_ascii_alphanumeric() || b == b'_' {
                            self.advance();
                        } else {
                            break;
                        }
                    }
                }
                let lexeme = self.slice_from(start_offset).to_string();
                Ok(Token {
                    kind: TokenKind::Symbol,
                    lexeme,
                    span: self.span_from(start),
                })
            }
            _ => Err(LexError::new(
                "stray `$` — expected `$(` or `$)`",
                self.span_from(start),
            )),
        }
    }

    fn lex_symbol(&mut self, start: SourcePosition) -> Result<Token, LexError> {
        let start_offset = self.pos.offset;
        let byte = self.peek().expect("lex_symbol called at EOF");

        // BCPL's float-flavoured operators accept either `.` or `#`
        // as the trailing marker — the reference uses `*#` / `+#`
        // / etc. heavily, while older sources prefer `*.` / `+.`.
        // Both produce the same token so the parser doesn't care.
        let is_float_marker = |b: u8| matches!(b, b'.' | b'#');

        // A small table-driven dispatcher. We list multi-byte forms longest-
        // first within each leading byte so that `<=.` wins over `<=`, etc.
        match byte {
            b'+' => {
                self.advance();
                if self.peek().is_some_and(is_float_marker) {
                    self.advance();
                }
            }
            b'-' => {
                self.advance();
                match self.peek() {
                    Some(b) if is_float_marker(b) => {
                        self.advance();
                    }
                    Some(b'>') => {
                        self.advance();
                    }
                    _ => {}
                }
            }
            b'*' => {
                self.advance();
                if self.peek().is_some_and(is_float_marker) {
                    self.advance();
                }
            }
            b'/' => {
                // `//` and `/* */` were already handled by skip_trivia, so
                // a `/` here is genuinely the division operator.
                self.advance();
                if self.peek().is_some_and(is_float_marker) {
                    self.advance();
                }
            }
            b'.' => {
                self.advance();
                if self.peek() == Some(b'%') {
                    self.advance();
                }
            }
            b'=' => {
                self.advance();
                if self.peek().is_some_and(is_float_marker) {
                    self.advance();
                }
            }
            b'~' => {
                self.advance();
                if self.peek() == Some(b'=') {
                    self.advance();
                    if self.peek().is_some_and(is_float_marker) {
                        self.advance();
                    }
                }
            }
            b'<' => {
                self.advance();
                match self.peek() {
                    Some(b'<') => {
                        self.advance();
                    }
                    Some(b'=') => {
                        self.advance();
                        if self.peek().is_some_and(is_float_marker) {
                            self.advance();
                        }
                    }
                    Some(b) if is_float_marker(b) => {
                        self.advance();
                    }
                    _ => {}
                }
            }
            b'>' => {
                self.advance();
                match self.peek() {
                    Some(b'>') => {
                        self.advance();
                    }
                    Some(b'=') => {
                        self.advance();
                        if self.peek().is_some_and(is_float_marker) {
                            self.advance();
                        }
                    }
                    Some(b) if is_float_marker(b) => {
                        self.advance();
                    }
                    _ => {}
                }
            }
            b':' => {
                self.advance();
                if self.peek() == Some(b'=') {
                    self.advance();
                }
            }
            b'%' => {
                // `%`  on its own is character indirection (prefix
                // `%v` / infix `v%i`); `%%` is the bitfield operator. The
                // parser handles the prefix-vs-infix split based on
                // context, so the lexer only needs to distinguish lengths.
                self.advance();
                if self.peek() == Some(b'%') {
                    self.advance();
                }
            }
            b'!' | b'@' | b'&' | b'|' | b'^'
            | b'(' | b')' | b'[' | b']' | b'{' | b'}'
            | b',' | b';' | b'?' | b'\\' => {
                self.advance();
            }
            other => {
                self.advance();
                return Err(LexError::new(
                    format!("unexpected character `{}` (0x{:02x})", other as char, other),
                    self.span_from(start),
                ));
            }
        }

        let lexeme = self.slice_from(start_offset).to_string();
        Ok(Token {
            kind: TokenKind::Symbol,
            lexeme,
            span: self.span_from(start),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds_and_lexemes(source: &str) -> Vec<(TokenKind, String)> {
        lex_source(source)
            .expect("lex_source error")
            .into_iter()
            .filter(|t| t.kind != TokenKind::Eof)
            .map(|t| (t.kind, t.lexeme))
            .collect()
    }

    #[test]
    fn classic_richards_keywords() {
        let toks = lex_source("LET x = 1").unwrap();
        assert_eq!(toks[0].kind, TokenKind::Keyword);
        assert_eq!(toks[0].lexeme, "LET");
        assert_eq!(toks[1].kind, TokenKind::Identifier);
        assert_eq!(toks[1].lexeme, "x");
        assert_eq!(toks[2].kind, TokenKind::Symbol);
        assert_eq!(toks[2].lexeme, "=");
        assert_eq!(toks[3].kind, TokenKind::Integer);
        assert_eq!(toks[3].lexeme, "1");
    }

    #[test]
    fn dotted_float_operators() {
        let pairs = kinds_and_lexemes("a +. b -. c *. d /. e");
        let lexemes: Vec<&str> = pairs.iter().map(|(_, s)| s.as_str()).collect();
        assert_eq!(
            lexemes,
            vec!["a", "+.", "b", "-.", "c", "*.", "d", "/.", "e"]
        );
    }

    #[test]
    fn dotted_relational_operators() {
        let pairs = kinds_and_lexemes("a =. b ~=. c <. d <=. e >. f >=. g");
        let lexemes: Vec<&str> = pairs.iter().map(|(_, s)| s.as_str()).collect();
        assert_eq!(
            lexemes,
            vec!["a", "=.", "b", "~=.", "c", "<.", "d", "<=.", "e", ">.", "f", ">=.", "g"]
        );
    }

    #[test]
    fn float_vector_indirection() {
        let pairs = kinds_and_lexemes("V .% E");
        let lexemes: Vec<&str> = pairs.iter().map(|(_, s)| s.as_str()).collect();
        assert_eq!(lexemes, vec!["V", ".%", "E"]);
    }

    #[test]
    fn assignment_and_arrow() {
        let pairs = kinds_and_lexemes("a := b -> c, d");
        let lexemes: Vec<&str> = pairs.iter().map(|(_, s)| s.as_str()).collect();
        assert_eq!(lexemes, vec!["a", ":=", "b", "->", "c", ",", "d"]);
    }

    #[test]
    fn shifts() {
        let pairs = kinds_and_lexemes("a << 1 >> b");
        let lexemes: Vec<&str> = pairs.iter().map(|(_, s)| s.as_str()).collect();
        assert_eq!(lexemes, vec!["a", "<<", "1", ">>", "b"]);
    }

    #[test]
    fn section_brackets_canonical() {
        let pairs = kinds_and_lexemes("$( foo $)");
        let lexemes: Vec<&str> = pairs.iter().map(|(_, s)| s.as_str()).collect();
        assert_eq!(lexemes, vec!["$(", "foo", "$)"]);
    }

    #[test]
    fn section_brackets_curly() {
        let pairs = kinds_and_lexemes("{ foo }");
        let lexemes: Vec<&str> = pairs.iter().map(|(_, s)| s.as_str()).collect();
        assert_eq!(lexemes, vec!["{", "foo", "}"]);
    }

    #[test]
    fn tagged_section_brackets() {
        let pairs = kinds_and_lexemes("$(LOOP foo $)LOOP");
        let lexemes: Vec<&str> = pairs.iter().map(|(_, s)| s.as_str()).collect();
        assert_eq!(lexemes, vec!["$(LOOP", "foo", "$)LOOP"]);
    }

    #[test]
    fn octal_and_hex() {
        let pairs = kinds_and_lexemes("#777 #X1A #xff #X0");
        let lexemes: Vec<&str> = pairs.iter().map(|(_, s)| s.as_str()).collect();
        assert_eq!(lexemes, vec!["#777", "#X1A", "#xff", "#X0"]);
    }

    #[test]
    fn octal_rejects_eight_or_nine() {
        let err = lex_source("#789").unwrap_err();
        assert!(err.message.contains("octal"));
    }

    #[test]
    fn real_literal_with_fraction_and_exponent() {
        let pairs = kinds_and_lexemes("3.14 0.5 1e10 1.5e-3 2E+5");
        let lexemes: Vec<&str> = pairs.iter().map(|(_, s)| s.as_str()).collect();
        assert_eq!(lexemes, vec!["3.14", "0.5", "1e10", "1.5e-3", "2E+5"]);
        // First, third, and fifth tokens are reals; check kinds explicitly.
        let toks = lex_source("3.14 1e10 2E+5").unwrap();
        assert_eq!(toks[0].kind, TokenKind::Real);
        assert_eq!(toks[1].kind, TokenKind::Real);
        assert_eq!(toks[2].kind, TokenKind::Real);
    }

    #[test]
    fn integer_then_dot_percent_does_not_eat_the_dot() {
        // `v!10.%i` mixes an integer index against a vector base; the `.%`
        // must remain a single operator and the `10` an integer.
        let pairs = kinds_and_lexemes("v!10.%i");
        let lexemes: Vec<&str> = pairs.iter().map(|(_, s)| s.as_str()).collect();
        assert_eq!(lexemes, vec!["v", "!", "10", ".%", "i"]);
    }

    #[test]
    fn string_with_bcpl_escapes() {
        let toks = lex_source(r#" "Hello*N world*T" "#).unwrap();
        assert_eq!(toks[0].kind, TokenKind::String);
        assert_eq!(toks[0].lexeme, "\"Hello*N world*T\"");
    }

    #[test]
    fn string_with_doubled_asterisk() {
        let toks = lex_source(r#" "**stars**" "#).unwrap();
        assert_eq!(toks[0].kind, TokenKind::String);
        assert_eq!(toks[0].lexeme, "\"**stars**\"");
    }

    #[test]
    fn character_literals() {
        let toks = lex_source("'a' '*N' '*''").unwrap();
        assert_eq!(toks[0].lexeme, "'a'");
        assert_eq!(toks[1].lexeme, "'*N'");
        assert_eq!(toks[2].lexeme, "'*''");
    }

    #[test]
    fn line_and_block_comments() {
        let toks = lex_source(
            r#"
            // a line comment
            LET /* a block comment */ x = 1
            "#,
        )
        .unwrap();
        let kinds_and_lex: Vec<_> = toks
            .iter()
            .filter(|t| t.kind != TokenKind::Eof)
            .map(|t| t.lexeme.as_str())
            .collect();
        assert_eq!(kinds_and_lex, vec!["LET", "x", "=", "1"]);
    }

    #[test]
    fn unterminated_block_comment() {
        let err = lex_source("LET x = /* nope ").unwrap_err();
        assert!(err.message.contains("unterminated"));
    }

    #[test]
    fn unterminated_string() {
        let err = lex_source(" \"oops ").unwrap_err();
        assert!(err.message.contains("unterminated"));
    }

    #[test]
    fn span_tracks_lines_and_columns() {
        let src = "LET\n  x = 1";
        let toks = lex_source(src).unwrap();
        // LET starts at 1:1
        assert_eq!(toks[0].span.start.line, 1);
        assert_eq!(toks[0].span.start.column, 1);
        // x is on line 2, column 3
        let x = toks.iter().find(|t| t.lexeme == "x").unwrap();
        assert_eq!(x.span.start.line, 2);
        assert_eq!(x.span.start.column, 3);
    }

    #[test]
    fn percent_and_double_percent() {
        let pairs = kinds_and_lexemes("a %% (0,8) := %v");
        let lexemes: Vec<&str> = pairs.iter().map(|(_, s)| s.as_str()).collect();
        assert_eq!(
            lexemes,
            vec!["a", "%%", "(", "0", ",", "8", ")", ":=", "%", "v"]
        );
    }

    #[test]
    fn utf8_bom_is_tolerated() {
        let mut src = String::from("\u{FEFF}");
        src.push_str("LET x = 1");
        let toks = lex_source(&src).unwrap();
        // BOM consumed silently; first real token still reports column 1.
        assert_eq!(toks[0].kind, TokenKind::Keyword);
        assert_eq!(toks[0].lexeme, "LET");
        assert_eq!(toks[0].span.start.column, 1);
    }

    #[test]
    fn class1_smoke() {
        // Lift a representative CLASS body from
        // reference/tests/bcl_tests/class1.bcl and ensure we lex it.
        let src = r#"
CLASS Point $(
    DECL x, y
    ROUTINE CREATE(initialX, initialY) BE $(
        x := initialX
        y := initialY
    $)
    FUNCTION getX() = VALOF $( RESULTIS x $)
$)
"#;
        let toks = lex_source(src).expect("class1 should lex");
        // Spot-check: the first non-whitespace token is the CLASS keyword
        // and the file ends with a section-close.
        let non_eof: Vec<&Token> = toks.iter().filter(|t| t.kind != TokenKind::Eof).collect();
        assert_eq!(non_eof.first().unwrap().lexeme, "CLASS");
        assert_eq!(non_eof.last().unwrap().lexeme, "$)");
    }
}
