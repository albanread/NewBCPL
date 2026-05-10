//! NewBCPL AST nodes.
//!
//! Mirrors the *kinds* of nodes in `reference/AST.h` but flattened into
//! Rust enums and owned-data structs. Grows incrementally as the parser
//! learns more of the grammar.
//!
//! Spans are reused from `newbcpl_lexer::SourceSpan` so the whole
//! pipeline shares one notion of source location.

use newbcpl_lexer::SourceSpan;

pub type Span = SourceSpan;

#[derive(Debug, Clone)]
pub struct Program {
    pub items: Vec<Decl>,
}

#[derive(Debug, Clone)]
pub enum Decl {
    Function(FunctionDecl),
    Routine(RoutineDecl),
    Let(LetDecl),
    /// `GET "filename"` include directive.
    Get(GetDirective),
    /// `MANIFEST $( name = expr; ... $)` — compile-time constants.
    Manifest(NamedBindingsDecl),
    /// `STATIC name` (uninitialised) or `STATIC $( name = expr; ... $)`.
    Static(NamedBindingsDecl),
    /// `GLOBAL $( name : offset; ... $)` (classic offset form) and
    /// `GLOBALS $( LET name = expr; ... $)` (modern dialect form) both
    /// land here. The distinction (offset vs initialiser) is sema's
    /// concern; the parser just records each binding's optional value.
    Global(NamedBindingsDecl),
}

#[derive(Debug, Clone)]
pub struct GetDirective {
    pub path: String,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct NamedBindingsDecl {
    pub bindings: Vec<NamedBinding>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct NamedBinding {
    pub name: String,
    /// `None` for `STATIC name` (declaration without initialiser);
    /// `Some` for `name = expr`, `name : expr`, or `LET name = expr`.
    pub value: Option<Expr>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct FunctionDecl {
    pub name: String,
    pub params: Vec<String>,
    pub body: Expr,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct RoutineDecl {
    pub name: String,
    pub params: Vec<String>,
    pub body: Box<Stmt>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct LetDecl {
    pub bindings: Vec<(String, Expr)>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub enum Stmt {
    Block(Block),
    Decl(Decl),
    Expr(Expr),
    Assign {
        targets: Vec<Expr>,
        values: Vec<Expr>,
        span: Span,
    },
    /// `IF cond THEN body` (`else_stmt` = None) or
    /// `IF cond THEN body ELSE other` / `TEST cond THEN body ELSE other`
    /// (`else_stmt` = Some). `IF` and `TEST` are surface synonyms for the
    /// same shape; the keyword distinction is dropped at parse time. See
    /// docs/manifesto.md §3 — `OR` is a binary operator only, not an
    /// else-marker.
    If {
        cond: Expr,
        then_stmt: Box<Stmt>,
        else_stmt: Option<Box<Stmt>>,
        span: Span,
    },
    Unless {
        cond: Expr,
        then_stmt: Box<Stmt>,
        span: Span,
    },
    While {
        cond: Expr,
        body: Box<Stmt>,
        span: Span,
    },
    Until {
        cond: Expr,
        body: Box<Stmt>,
        span: Span,
    },
    Repeat {
        body: Box<Stmt>,
        span: Span,
    },
    RepeatWhile {
        body: Box<Stmt>,
        cond: Expr,
        span: Span,
    },
    RepeatUntil {
        body: Box<Stmt>,
        cond: Expr,
        span: Span,
    },
    Resultis(Expr, Span),
    Return(Span),
    Finish(Span),
    Break(Span),
    Loop(Span),
    Endcase(Span),
}

#[derive(Debug, Clone)]
pub struct Block {
    pub stmts: Vec<Stmt>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub enum Expr {
    Ident { name: String, span: Span },
    IntLit { value: i64, span: Span },
    /// IEEE-754 double-precision. Stored as the bit pattern so
    /// the parser does not lose precision through the `f64` round-trip.
    FloatLit { value: f64, span: Span },
    /// Single character; lexeme retained including surrounding quotes
    /// so a pretty-printer can round-trip the source style.
    CharLit { lexeme: String, span: Span },
    /// Raw string lexeme including surrounding `"` and `*`-escape
    /// sequences. Sema cooks the escapes when needed.
    StringLit { value: String, span: Span },
    /// True / False as integer-valued literals (TRUE = 1, FALSE = 0).
    BoolLit { value: bool, span: Span },
    /// `?` — null pointer literal.
    Null { span: Span },
    Call {
        callee: Box<Expr>,
        args: Vec<Expr>,
        span: Span,
    },
    Unary {
        op: UnaryOp,
        operand: Box<Expr>,
        span: Span,
    },
    Binary {
        op: BinaryOp,
        lhs: Box<Expr>,
        rhs: Box<Expr>,
        span: Span,
    },
    /// `cond -> then_expr, else_expr`
    Conditional {
        cond: Box<Expr>,
        then_expr: Box<Expr>,
        else_expr: Box<Expr>,
        span: Span,
    },
    /// `VALOF stmt` — yields the value passed to `RESULTIS`.
    Valof { body: Box<Stmt>, span: Span },
    /// Typed constructor — covers heap allocation (`VEC k`, `FVEC k`),
    /// SIMD primitives (`PAIR`/`FPAIR`/`QUAD`/`FQUAD`/`OCT`/`FOCT`),
    /// and table literals (`TABLE`/`FTABLE`). All expressed as a kind
    /// plus a list of arguments so a single AST node and a single
    /// codegen path lower them.
    TypedConstruct {
        kind: TypeConstructorKind,
        args: Vec<Expr>,
        span: Span,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TypeConstructorKind {
    /// `VEC k` — heap-allocated word vector with `k+1` elements.
    Vec,
    /// `FVEC k` — heap-allocated float vector with `k+1` elements.
    FVec,
    /// `TABLE(e1, e2, ...)` — static integer table.
    Table,
    /// `FTABLE(e1, e2, ...)` — static float table.
    FTable,
    /// `PAIR(a, b)` — V-register-resident integer pair (`<2 x i64>`).
    Pair,
    /// `FPAIR(a, b)` — V-register-resident float pair (`<2 x double>`).
    FPair,
    /// `QUAD(a, b, c, d)` — `<4 x i64>`.
    Quad,
    /// `FQUAD(a, b, c, d)` — `<4 x double>`.
    FQuad,
    /// `OCT(a..h)` — `<8 x i64>` (SVE-targeted).
    Oct,
    /// `FOCT(a..h)` — `<8 x double>`.
    FOct,
}

impl TypeConstructorKind {
    pub fn as_str(self) -> &'static str {
        match self {
            TypeConstructorKind::Vec => "VEC",
            TypeConstructorKind::FVec => "FVEC",
            TypeConstructorKind::Table => "TABLE",
            TypeConstructorKind::FTable => "FTABLE",
            TypeConstructorKind::Pair => "PAIR",
            TypeConstructorKind::FPair => "FPAIR",
            TypeConstructorKind::Quad => "QUAD",
            TypeConstructorKind::FQuad => "FQUAD",
            TypeConstructorKind::Oct => "OCT",
            TypeConstructorKind::FOct => "FOCT",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnaryOp {
    /// `-x` — arithmetic negation.
    Neg,
    /// `~x` — bitwise / logical NOT (the dialect uses one symbol for both).
    Not,
    /// `!x` — pointer dereference (load the word at address x).
    Indirection,
    /// `@x` — address-of x.
    AddressOf,
    /// `%x` — character-pointer dereference (load the byte at address x).
    CharIndirection,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinaryOp {
    // Integer arithmetic
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    // Float arithmetic (dotted variants)
    FAdd,
    FSub,
    FMul,
    FDiv,
    // Integer / general relational
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    // Float relational (dotted variants)
    FEq,
    FNe,
    FLt,
    FLe,
    FGt,
    FGe,
    // Logical / bitwise
    BitAnd,
    BitOr,
    Eqv,
    Neqv,
    // Shifts
    Shl,
    Shr,
    // Indirection / subscript family
    /// `v ! i` — vector subscript, equivalent to `*(v+i)`.
    Subscript,
    /// `v %% i` — bitfield access.
    Bitfield,
    /// `v % i` — character-vector subscript.
    CharSubscript,
    /// `v .% i` — float-vector subscript.
    FloatSubscript,
    /// `obj . field` — member access.
    Dot,
    /// `obj OF field` — classic BCPL field access (kept for compatibility).
    Of,
}

impl Expr {
    pub fn span(&self) -> Span {
        match self {
            Expr::Ident { span, .. }
            | Expr::IntLit { span, .. }
            | Expr::FloatLit { span, .. }
            | Expr::CharLit { span, .. }
            | Expr::StringLit { span, .. }
            | Expr::BoolLit { span, .. }
            | Expr::Null { span }
            | Expr::Call { span, .. }
            | Expr::Unary { span, .. }
            | Expr::Binary { span, .. }
            | Expr::Conditional { span, .. }
            | Expr::Valof { span, .. }
            | Expr::TypedConstruct { span, .. } => *span,
        }
    }
}

impl Stmt {
    pub fn span(&self) -> Span {
        match self {
            Stmt::Block(b) => b.span,
            Stmt::Decl(d) => d.span(),
            Stmt::Expr(e) => e.span(),
            Stmt::Assign { span, .. }
            | Stmt::If { span, .. }
            | Stmt::Unless { span, .. }
            | Stmt::While { span, .. }
            | Stmt::Until { span, .. }
            | Stmt::Repeat { span, .. }
            | Stmt::RepeatWhile { span, .. }
            | Stmt::RepeatUntil { span, .. } => *span,
            Stmt::Resultis(_, s)
            | Stmt::Return(s)
            | Stmt::Finish(s)
            | Stmt::Break(s)
            | Stmt::Loop(s)
            | Stmt::Endcase(s) => *s,
        }
    }
}

impl Decl {
    pub fn span(&self) -> Span {
        match self {
            Decl::Function(f) => f.span,
            Decl::Routine(r) => r.span,
            Decl::Let(l) => l.span,
            Decl::Get(g) => g.span,
            Decl::Manifest(m) => m.span,
            Decl::Static(s) => s.span,
            Decl::Global(g) => g.span,
        }
    }
}

impl UnaryOp {
    pub fn as_str(self) -> &'static str {
        match self {
            UnaryOp::Neg => "-",
            UnaryOp::Not => "~",
            UnaryOp::Indirection => "!",
            UnaryOp::AddressOf => "@",
            UnaryOp::CharIndirection => "%",
        }
    }
}

impl BinaryOp {
    pub fn as_str(self) -> &'static str {
        match self {
            BinaryOp::Add => "+",
            BinaryOp::Sub => "-",
            BinaryOp::Mul => "*",
            BinaryOp::Div => "/",
            BinaryOp::Rem => "REM",
            BinaryOp::FAdd => "+.",
            BinaryOp::FSub => "-.",
            BinaryOp::FMul => "*.",
            BinaryOp::FDiv => "/.",
            BinaryOp::Eq => "=",
            BinaryOp::Ne => "~=",
            BinaryOp::Lt => "<",
            BinaryOp::Le => "<=",
            BinaryOp::Gt => ">",
            BinaryOp::Ge => ">=",
            BinaryOp::FEq => "=.",
            BinaryOp::FNe => "~=.",
            BinaryOp::FLt => "<.",
            BinaryOp::FLe => "<=.",
            BinaryOp::FGt => ">.",
            BinaryOp::FGe => ">=.",
            BinaryOp::BitAnd => "&",
            BinaryOp::BitOr => "|",
            BinaryOp::Eqv => "EQV",
            BinaryOp::Neqv => "NEQV",
            BinaryOp::Shl => "<<",
            BinaryOp::Shr => ">>",
            BinaryOp::Subscript => "!",
            BinaryOp::Bitfield => "%%",
            BinaryOp::CharSubscript => "%",
            BinaryOp::FloatSubscript => ".%",
            BinaryOp::Dot => ".",
            BinaryOp::Of => "OF",
        }
    }
}
