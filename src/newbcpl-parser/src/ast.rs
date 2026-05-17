//! NewBCPL AST nodes.
//!
//! Mirrors the *kinds* of nodes in `reference/AST.h` but flattened into
//! Rust enums and owned-data structs. Grows incrementally as the parser
//! learns more of the grammar.
//!
//! Spans are reused from `newbcpl_lexer::SourceSpan` so the whole
//! pipeline shares one notion of source location.

use std::cell::Cell;

use newbcpl_lexer::SourceSpan;

pub type Span = SourceSpan;

/// Register-class hint per `docs/manifesto.md` §2.
///
/// This enum is the shared vocabulary between sema and codegen for
/// "what kind of value lives here." Sema fills the hint in on every
/// `Expr` during its walk; later phases read `Expr::hint()` directly
/// instead of re-deriving.
///
/// `Word` is the universal escape hatch: any classic-BCPL expression
/// without strong type evidence stays `Word`, and codegen treats it
/// as a generic 64-bit integer in an X-register.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TypeHint {
    /// Untyped 64-bit word. The default and the universal escape hatch.
    Word,
    /// Signed 64-bit integer (X-register, `i64`).
    Int,
    /// IEEE-754 double-precision (D-register, `double`).
    Float,
    /// Pointer to a heap-allocated UTF-8 string.
    String,
    /// `?` null literal — coerces to any pointer-shaped target.
    Null,
    /// 128-bit V-register, `<2 x i64>`.
    Pair,
    /// 128-bit V-register, `<2 x double>`.
    FPair,
    /// 256-bit, `<4 x i64>`.
    Quad,
    /// 256-bit, `<4 x double>`.
    FQuad,
    /// 512-bit (SVE), `<8 x i64>`.
    Oct,
    /// 512-bit (SVE), `<8 x double>`.
    FOct,
    /// Heap-allocated cons-cell list (heterogeneous capable).
    List,
    /// Heap-allocated word vector.
    Vec,
    /// Heap-allocated float vector.
    FVec,
    /// Heap-allocated object instance. Class identity is recorded in
    /// the sema-side `BindingInfo` / `ClassInfo` separately.
    Object,
    /// Function value (callable).
    Function,
    /// Sema didn't determine the type. Codegen treats this as `Word`.
    Unknown,
}

impl Default for TypeHint {
    fn default() -> Self {
        TypeHint::Unknown
    }
}

impl TypeHint {
    pub fn as_str(self) -> &'static str {
        match self {
            TypeHint::Word => "WORD",
            TypeHint::Int => "INT",
            TypeHint::Float => "FLOAT",
            TypeHint::String => "STRING",
            TypeHint::Null => "NULL",
            TypeHint::Pair => "PAIR",
            TypeHint::FPair => "FPAIR",
            TypeHint::Quad => "QUAD",
            TypeHint::FQuad => "FQUAD",
            TypeHint::Oct => "OCT",
            TypeHint::FOct => "FOCT",
            TypeHint::List => "LIST",
            TypeHint::Vec => "VEC",
            TypeHint::FVec => "FVEC",
            TypeHint::Object => "OBJECT",
            TypeHint::Function => "FUNCTION",
            TypeHint::Unknown => "?",
        }
    }

    /// Lives in a floating-point register family (D-register or
    /// NEON / SVE V-register holding floats).
    pub fn is_float_family(self) -> bool {
        matches!(
            self,
            TypeHint::Float | TypeHint::FPair | TypeHint::FQuad | TypeHint::FOct | TypeHint::FVec
        )
    }

    /// Both sides being this type means an integer-family op
    /// (X-register or NEON / SVE integer lanes).
    pub fn is_int_family(self) -> bool {
        matches!(
            self,
            TypeHint::Int
                | TypeHint::Word
                | TypeHint::Pair
                | TypeHint::Quad
                | TypeHint::Oct
                | TypeHint::Vec
        )
    }
}

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
    /// `CLASS Name [EXTENDS Base] [MANAGED] $( … $)` — see manifesto §5.
    Class(ClassDecl),
    /// `LET name(params) = ASM { … }` or `LET name(params) BE ASM { … }`.
    AsmProc(AsmProcDecl),
}

/// An ASM procedure or routine declaration.
///
/// The body is the raw source text between the `{` and `}` delimiters,
/// preserved verbatim. There is no parameter-name substitution: the
/// author writes Win64 ABI registers (`rcx`, `rdx`, `r8`, `r9`,
/// `xmm0`, …) directly. The parameter list still matters because it
/// determines the matching LLVM `declare`'s argument types, which in
/// turn pin down which slot — and therefore which Win64 register —
/// each parameter arrives in.
#[derive(Debug, Clone)]
pub struct AsmProcDecl {
    pub name: String,
    pub params: Vec<String>,
    /// Optional `AS Type` annotations in the same positions as
    /// `params`. Drives the LLVM `declare`'s argument register class
    /// (integer vs XMM vs YMM) per `annotation_to_asm_type` in the IR
    /// lowering pass.
    pub param_annotations: Vec<Option<String>>,
    /// Optional `AS Type` annotation on the return value, parsed
    /// from the `LET name(params) AS Type = ASM { … }` form. `None`
    /// means the function returns a plain Word (i64 in `rax`); the
    /// only non-`Word` types that travel out of an ASM proc cleanly
    /// are `FLOAT` (f64 in `xmm0`), `FQUAD` (`<4 x f32>` in `xmm0`),
    /// and `FOCT` (`<8 x f32>` in `ymm0`). Ignored for `BE ASM`
    /// routines — they have no return value.
    pub return_annotation: Option<String>,
    /// `true` = `= ASM { }` (function, returns a value in rax/xmm0).
    /// `false` = `BE ASM { }` (routine, return value ignored).
    pub is_function: bool,
    /// Raw Intel-syntax text between `{` and `}`, emitted verbatim
    /// into the `module asm` blob by `new_asm::build_module_asm_string`.
    pub body: String,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct ClassDecl {
    pub name: String,
    pub extends: Option<String>,
    /// `MANAGED` keyword — opts the class into linear / RAII semantics.
    pub managed: bool,
    pub members: Vec<ClassMember>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct ClassMember {
    pub visibility: Visibility,
    pub kind: ClassMemberKind,
    pub span: Span,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Visibility {
    /// Default; everything is PUBLIC unless a `PRIVATE:` /
    /// `PROTECTED:` modifier overrides.
    Public,
    Private,
    Protected,
}

#[derive(Debug, Clone)]
pub enum ClassMemberKind {
    /// `DECL x, y, z` — uninitialised member variables. Each name may
    /// carry an optional `AS Type` annotation, parallel to `names`.
    /// The annotation strings use the same canonical form as
    /// `LetDecl::annotations` (`"INTEGER"`, `"^STRING"`, `"Window"`).
    /// `None` for un-annotated names — still the common case for the
    /// plain `DECL x, y` form.
    Fields {
        names: Vec<String>,
        annotations: Vec<Option<String>>,
    },
    /// `LET name = expr` — initialised member variable.
    Let(LetDecl),
    /// `FLET name` — uninitialised float member; `FLET name = expr`
    /// — initialised float member. Re-uses `LetDecl` shape with the
    /// value either present or absent (recorded as `NamedBinding`).
    FLet(NamedBinding),
    /// A method — function-form (`= expr`) or routine-form (`BE stmt`).
    Method(ClassMethod),
}

#[derive(Debug, Clone)]
pub struct ClassMethod {
    pub name: String,
    pub params: Vec<String>,
    /// Per-parameter `AS Type` annotation, parallel to `params`.
    /// See `FunctionDecl::param_annotations`.
    pub param_annotations: Vec<Option<String>>,
    /// `VIRTUAL ROUTINE` / `VIRTUAL FUNCTION`.
    pub is_virtual: bool,
    /// `FINAL ROUTINE` / `FINAL FUNCTION`.
    pub is_final: bool,
    pub body: ClassMethodBody,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub enum ClassMethodBody {
    /// `BE stmt`.
    Routine(Box<Stmt>),
    /// `= expr`.
    Function(Expr),
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
    /// Per-parameter `AS Type` annotation, parallel to `params`
    /// (same length). `None` for un-annotated parameters — the
    /// common case. The string is the canonicalised type-expression
    /// form, e.g. `"INTEGER"`, `"^STRING"`, `"Window"`. Mirrors the
    /// shape of `LetDecl.annotations` so sema's class-identity
    /// resolution code can re-use the same canonicalisation path.
    pub param_annotations: Vec<Option<String>>,
    pub body: Expr,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct RoutineDecl {
    pub name: String,
    pub params: Vec<String>,
    /// Per-parameter `AS Type` annotation, parallel to `params`.
    /// See `FunctionDecl::param_annotations`.
    pub param_annotations: Vec<Option<String>>,
    pub body: Box<Stmt>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct LetDecl {
    pub bindings: Vec<(String, Expr)>,
    /// Per-binding `AS` type annotation, parallel to `bindings`
    /// (same length, indexes line up). `None` for un-annotated
    /// bindings — the common case. The string is the
    /// canonicalised form of the type expression, e.g.
    /// `"INTEGER"`, `"^STRING"`, `"^LIST OF INTEGER"`. Sema
    /// reads this in `type_hint_from_annotation` to seed the
    /// binding's hint instead of inferring from the initialiser
    /// alone — manifesto §2 ("looks untyped, secretly typed").
    pub annotations: Vec<Option<String>>,
    /// True when the binding is a destructuring shape:
    /// `LET a, b = single_pair_expr` (one RHS, N names). Lower
    /// evaluates the RHS once and lane-unpacks it into each
    /// name's slot — same semantics as `FOREACH (a, b) IN ...`.
    /// Every `bindings[i].1` is a clone of the same RHS so
    /// downstream walkers that don't care about destructuring
    /// still see a well-formed (name, expr) pair per binding.
    pub destructure: bool,
    pub span: Span,
    /// Which binder keyword was used. `LET` is the default; `FLET`
    /// signals to sema that the right-hand sides should be inferred
    /// as `FLOAT` when the literal evidence is otherwise neutral
    /// (manifesto §1).
    pub kind: LetKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LetKind {
    Let,
    FLet,
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
    /// `FOR name = start TO end [BY step] DO body`.
    For {
        name: String,
        start: Expr,
        end: Expr,
        step: Option<Expr>,
        body: Box<Stmt>,
        span: Span,
    },
    /// `FOREACH name [, name] [AS Type] IN iterable DO body`. The
    /// optional second name supports map-style destructuring (key,
    /// value); the optional `AS` annotation hints the element type.
    ForEach {
        names: Vec<String>,
        annotation: Option<String>,
        iter: Expr,
        body: Box<Stmt>,
        span: Span,
    },
    /// `SWITCHON expr INTO $( CASE … : ; CASE … : … ; DEFAULT : … $)`.
    /// `cases` preserves source order; each entry groups any number of
    /// adjacent `CASE label:` lines that share a single body. `default`
    /// is the optional `DEFAULT:` body.
    Switchon {
        scrutinee: Expr,
        cases: Vec<SwitchCase>,
        default: Option<Vec<Stmt>>,
        span: Span,
    },
    Resultis(Expr, Span),
    Return(Span),
    Finish(Span),
    Break(Span),
    Loop(Span),
    Endcase(Span),
    /// `BRK` — debugger breakpoint statement (no operand).
    Brk(Span),
    /// `GOTO label` — unconditional jump. Target is a label name.
    Goto { label: String, span: Span },
    /// `name:` — label declaration. The labelled statement that
    /// follows lives as a separate `Stmt` in the surrounding block.
    Label { name: String, span: Span },
    /// `RETAIN x` (mark existing) or `RETAIN x = expr` (declare and
    /// mark). Tells the GC / SAMM that this binding outlives its
    /// natural scope.
    Retain {
        name: String,
        value: Option<Expr>,
        span: Span,
    },
    /// `USING name = expr DO stmt` — scope-deterministic resource form.
    /// Binds `name` to the value of `expr`, runs `body`, and then calls
    /// `name.RELEASE()` exactly once at scope exit. Mirrors Python's
    /// `with`, C#'s `using`, Java's try-with-resources. RELEASE runs
    /// on fall-through and on `RETURN` / `RESULTIS` / `FINISH` exits
    /// from within `body`; cleanup-around-`BREAK`/`LOOP` is a
    /// follow-up (today they bypass it — sema warns when it sees one
    /// inside a USING body).
    Using {
        name: String,
        value: Expr,
        body: Box<Stmt>,
        span: Span,
    },
}

#[derive(Debug, Clone)]
pub struct Block {
    pub stmts: Vec<Stmt>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct SwitchCase {
    /// All `CASE label:` lines that fall through to this body, in source
    /// order. A bare fall-through case (`CASE 1:` followed immediately by
    /// `CASE 2:`) is recorded as a separate `SwitchCase` with an empty
    /// `body`; the parser does not collapse them.
    pub values: Vec<Expr>,
    pub body: Vec<Stmt>,
    pub span: Span,
}

/// Every variant carries a `hint: Cell<TypeHint>` that sema fills
/// in during its walk and that downstream phases read via
/// [`Expr::hint`]. The default is `TypeHint::Unknown`; codegen treats
/// that as `Word`. The `Cell` gives sema interior mutability without
/// requiring a `&mut` borrow of the AST.
#[derive(Debug, Clone)]
pub enum Expr {
    Ident {
        name: String,
        span: Span,
        hint: Cell<TypeHint>,
    },
    IntLit {
        value: i64,
        span: Span,
        hint: Cell<TypeHint>,
    },
    /// IEEE-754 double-precision. Stored as the bit pattern so
    /// the parser does not lose precision through the `f64` round-trip.
    FloatLit {
        value: f64,
        span: Span,
        hint: Cell<TypeHint>,
    },
    /// Single character; lexeme retained including surrounding quotes
    /// so a pretty-printer can round-trip the source style.
    CharLit {
        lexeme: String,
        span: Span,
        hint: Cell<TypeHint>,
    },
    /// Raw string lexeme including surrounding `"` and `*`-escape
    /// sequences. Sema cooks the escapes when needed.
    StringLit {
        value: String,
        span: Span,
        hint: Cell<TypeHint>,
    },
    /// True / False as integer-valued literals (TRUE = 1, FALSE = 0).
    BoolLit {
        value: bool,
        span: Span,
        hint: Cell<TypeHint>,
    },
    /// `?` — null pointer literal.
    Null {
        span: Span,
        hint: Cell<TypeHint>,
    },
    Call {
        callee: Box<Expr>,
        args: Vec<Expr>,
        span: Span,
        hint: Cell<TypeHint>,
    },
    Unary {
        op: UnaryOp,
        operand: Box<Expr>,
        span: Span,
        hint: Cell<TypeHint>,
    },
    Binary {
        op: BinaryOp,
        lhs: Box<Expr>,
        rhs: Box<Expr>,
        span: Span,
        hint: Cell<TypeHint>,
    },
    /// `cond -> then_expr, else_expr`
    Conditional {
        cond: Box<Expr>,
        then_expr: Box<Expr>,
        else_expr: Box<Expr>,
        span: Span,
        hint: Cell<TypeHint>,
    },
    /// `VALOF stmt` — yields the value passed to `RESULTIS`.
    Valof {
        body: Box<Stmt>,
        span: Span,
        hint: Cell<TypeHint>,
    },
    /// Typed constructor — covers heap allocation (`VEC k`, `FVEC k`),
    /// SIMD primitives (`PAIR`/`FPAIR`/`QUAD`/`FQUAD`/`OCT`/`FOCT`),
    /// and table literals (`TABLE`/`FTABLE`). All expressed as a kind
    /// plus a list of arguments so a single AST node and a single
    /// codegen path lower them.
    TypedConstruct {
        kind: TypeConstructorKind,
        args: Vec<Expr>,
        span: Span,
        hint: Cell<TypeHint>,
    },
    /// `NEW Class(args)` — heap-allocate an object and call its
    /// `CREATE`. The argument list is empty for `NEW Class`.
    New {
        class_name: String,
        args: Vec<Expr>,
        span: Span,
        hint: Cell<TypeHint>,
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
    /// `LIST(e1, e2, …)` — heap-allocated, GC-managed sequence; can mix
    /// types (heterogeneous via per-atom tags) per `docs/manifesto.md`.
    List,
    /// `MANIFESTLIST(e1, e2, …)` — read-only literal-list constant.
    ManifestList,
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
            TypeConstructorKind::List => "LIST",
            TypeConstructorKind::ManifestList => "MANIFESTLIST",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnaryOp {
    /// `-x` — arithmetic negation.
    Neg,
    /// `~x` / `BNOT x` — bitwise NOT (every bit flipped).
    Not,
    /// `NOT x` — logical NOT. Returns 1 if `x` is 0, else 0.
    LogNot,
    /// `!x` — pointer dereference (load the word at address x).
    Indirection,
    /// `@x` — address-of x.
    AddressOf,
    /// `%x` — character-pointer dereference (load the byte at address x).
    CharIndirection,
    /// `HD x` — first element / head of a list.
    Hd,
    /// `TL x` — destructive tail of a list.
    Tl,
    /// `REST x` — non-destructive tail of a list.
    Rest,
    /// `LEN x` — number of elements in a list / vector / string.
    Len,
    /// `FREEVEC x` — free a heap-allocated vector.
    FreeVec,
    /// `FREELIST x` — free a list.
    FreeList,
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
    // Bitwise (every bit independently)
    BitAnd,
    BitOr,
    BitXor,
    /// Equivalence (single-value equality, lowered as `==`).
    /// Kept for back-compat — programs that really want bitwise
    /// XNOR can write it as `BNOT (a BXOR b)`.
    Eqv,
    /// XOR — bitwise. Alias for `BXOR`. Both lower the same way.
    Neqv,
    // Logical (truthiness-based, return 0 or 1)
    LogAnd,
    LogOr,
    LogXor,
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
    /// `pair.|n|` — SIMD lane access. The RHS is the lane index.
    LaneAccess,
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
            | Expr::Null { span, .. }
            | Expr::Call { span, .. }
            | Expr::Unary { span, .. }
            | Expr::Binary { span, .. }
            | Expr::Conditional { span, .. }
            | Expr::Valof { span, .. }
            | Expr::TypedConstruct { span, .. }
            | Expr::New { span, .. } => *span,
        }
    }

    /// Read the sema-attached register-class hint for this expression.
    /// Returns `TypeHint::Unknown` until sema has run.
    pub fn hint(&self) -> TypeHint {
        match self {
            Expr::Ident { hint, .. }
            | Expr::IntLit { hint, .. }
            | Expr::FloatLit { hint, .. }
            | Expr::CharLit { hint, .. }
            | Expr::StringLit { hint, .. }
            | Expr::BoolLit { hint, .. }
            | Expr::Null { hint, .. }
            | Expr::Call { hint, .. }
            | Expr::Unary { hint, .. }
            | Expr::Binary { hint, .. }
            | Expr::Conditional { hint, .. }
            | Expr::Valof { hint, .. }
            | Expr::TypedConstruct { hint, .. }
            | Expr::New { hint, .. } => hint.get(),
        }
    }

    /// Sema's writer side: stamp a register-class hint onto this
    /// expression in place. Cell gives interior mutability so sema
    /// can take `&Expr` rather than `&mut Expr`, which keeps
    /// traversal shapes simple.
    pub fn set_hint(&self, h: TypeHint) {
        match self {
            Expr::Ident { hint, .. }
            | Expr::IntLit { hint, .. }
            | Expr::FloatLit { hint, .. }
            | Expr::CharLit { hint, .. }
            | Expr::StringLit { hint, .. }
            | Expr::BoolLit { hint, .. }
            | Expr::Null { hint, .. }
            | Expr::Call { hint, .. }
            | Expr::Unary { hint, .. }
            | Expr::Binary { hint, .. }
            | Expr::Conditional { hint, .. }
            | Expr::Valof { hint, .. }
            | Expr::TypedConstruct { hint, .. }
            | Expr::New { hint, .. } => hint.set(h),
        }
    }
}

/// Convenience constructor: a fresh `Cell<TypeHint>` initialised to
/// `Unknown`. Used by every parser construction site so sema has an
/// existing slot to fill.
pub fn unknown_hint() -> Cell<TypeHint> {
    Cell::new(TypeHint::Unknown)
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
            | Stmt::RepeatUntil { span, .. }
            | Stmt::For { span, .. }
            | Stmt::ForEach { span, .. }
            | Stmt::Switchon { span, .. } => *span,
            Stmt::Resultis(_, s)
            | Stmt::Return(s)
            | Stmt::Finish(s)
            | Stmt::Break(s)
            | Stmt::Loop(s)
            | Stmt::Endcase(s)
            | Stmt::Brk(s) => *s,
            Stmt::Goto { span, .. }
            | Stmt::Label { span, .. }
            | Stmt::Retain { span, .. }
            | Stmt::Using { span, .. } => *span,
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
            Decl::Class(c) => c.span,
            Decl::AsmProc(a) => a.span,
        }
    }
}

impl UnaryOp {
    pub fn as_str(self) -> &'static str {
        match self {
            UnaryOp::Neg => "-",
            UnaryOp::Not => "BNOT",
            UnaryOp::LogNot => "NOT",
            UnaryOp::Indirection => "!",
            UnaryOp::AddressOf => "@",
            UnaryOp::CharIndirection => "%",
            UnaryOp::Hd => "HD",
            UnaryOp::Tl => "TL",
            UnaryOp::Rest => "REST",
            UnaryOp::Len => "LEN",
            UnaryOp::FreeVec => "FREEVEC",
            UnaryOp::FreeList => "FREELIST",
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
            BinaryOp::BitAnd => "BAND",
            BinaryOp::BitOr => "BOR",
            BinaryOp::BitXor => "BXOR",
            BinaryOp::Eqv => "EQV",
            BinaryOp::Neqv => "NEQV",
            BinaryOp::LogAnd => "AND",
            BinaryOp::LogOr => "OR",
            BinaryOp::LogXor => "XOR",
            BinaryOp::Shl => "<<",
            BinaryOp::Shr => ">>",
            BinaryOp::Subscript => "!",
            BinaryOp::Bitfield => "%%",
            BinaryOp::CharSubscript => "%",
            BinaryOp::FloatSubscript => ".%",
            BinaryOp::Dot => ".",
            BinaryOp::Of => "OF",
            BinaryOp::LaneAccess => ".|",
        }
    }
}
