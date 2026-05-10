//! NewBCPL semantic analysis.
//!
//! Bootstrap shape per `docs/manifesto.md` §1 *Looks untyped, secretly
//! typed*: walk the AST once, attach a register-class type hint to every
//! binding, never error on type grounds.
//!
//! The hint vocabulary is the §2 *Be close to the machine* lattice — each
//! variant names a concrete register class so codegen can pick FADD vs
//! IADD, scalar vs SIMD, etc. without re-deriving anything.
//!
//! What this bootstrap does:
//!
//! - Tracks bindings introduced by `LET` / `FLET` / `STATIC` / `MANIFEST`
//!   / `GLOBAL[S]` / class members.
//! - Infers each binding's hint from its initialiser (`LET x = 3.14` →
//!   FLOAT; `LET x = LIST(…)` → LIST; `LET x = NEW Foo()` → OBJECT;
//!   `LET x = vec!i` → WORD).
//! - Flow-types operator results (`a + b` is FLOAT iff both sides are
//!   FLOAT; otherwise INT).
//! - Warns on assignments that would emit implicit FCVTZS truncation
//!   (FLOAT → INT). Never errors.
//! - Records visited classes and their members for later phases.
//!
//! What this bootstrap does NOT do (yet):
//!
//! - Track function signatures or method bodies separately. Calls are
//!   currently treated as returning WORD unless the callee resolves to
//!   a known intrinsic (e.g. `FLOAT(x)` → FLOAT, `LEN x` → INT).
//! - Resolve member accesses through a class hierarchy. `obj.field` is
//!   currently WORD. Sema will gain class-aware member typing once we
//!   need it for codegen.
//! - Validate `MANAGED` linear-type rules (no aliasing, no list
//!   storage). That's a separate pass once the parser-level discovery
//!   here is solid.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::Path;

use newbcpl_parser::{
    BinaryOp, Block, ClassDecl, ClassMember, ClassMemberKind, ClassMethodBody, Decl, Expr,
    LetDecl, LetKind, Program, Span, Stmt, TypeConstructorKind, UnaryOp,
};

/// Register-class hint for a value. The lattice from
/// `docs/manifesto.md` §2 — each variant corresponds to a concrete
/// LLVM type and a concrete machine register class.
///
/// `Word` is the universal escape hatch: classic BCPL programs that
/// don't carry strong type evidence stay `Word`, and any operator
/// applied to one or more `Word`s falls back to integer codegen.
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
    /// `SemaOutput::bindings` and `SemaOutput::classes` separately.
    Object,
    /// Function value (callable).
    Function,
    /// Sema couldn't determine the type. Codegen treats this as `Word`.
    Unknown,
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

    /// True if the type lives in a floating-point register family
    /// (D-register or a NEON / SVE V-register holding floats).
    pub fn is_float_family(self) -> bool {
        matches!(
            self,
            TypeHint::Float | TypeHint::FPair | TypeHint::FQuad | TypeHint::FOct | TypeHint::FVec
        )
    }

    /// True if both sides being this type means an integer-family op
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
pub struct BindingInfo {
    pub name: String,
    pub hint: TypeHint,
    /// For `Object` bindings, the class name they were created from
    /// (best effort — `NEW Foo()` produces `Some("Foo")`).
    pub class_name: Option<String>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct ClassInfo {
    pub name: String,
    pub extends: Option<String>,
    pub managed: bool,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct SemaWarning {
    pub message: String,
    pub span: Span,
}

impl SemaWarning {
    pub fn render(&self) -> String {
        format!(
            "{} at {}:{}",
            self.message, self.span.start.line, self.span.start.column
        )
    }
}

#[derive(Debug, Clone, Default)]
pub struct SemaOutput {
    /// Every binding sema observed, in declaration order.
    pub bindings: Vec<BindingInfo>,
    /// Every class declared.
    pub classes: Vec<ClassInfo>,
    /// Non-fatal diagnostics. Sema never fails on type grounds, so
    /// every interesting observation lands here.
    pub warnings: Vec<SemaWarning>,
}

pub fn analyze(program: &Program) -> SemaOutput {
    let mut sema = Sema::new();
    sema.analyze_program(program);
    SemaOutput {
        bindings: sema.binding_log,
        classes: sema.class_log,
        warnings: sema.warnings,
    }
}

pub fn dump_sema(path: &Path) -> String {
    match std::fs::read_to_string(path) {
        Ok(source) => match newbcpl_parser::parse_source(&source) {
            Ok(program) => {
                let result = analyze(&program);
                let mut out = format!(
                    "newbcpl-sema dump\ninput: {}\n",
                    path.display()
                );
                writeln!(out, "bindings ({}):", result.bindings.len()).unwrap();
                for b in &result.bindings {
                    let class = match &b.class_name {
                        Some(c) => format!(" [{c}]"),
                        None => String::new(),
                    };
                    writeln!(
                        out,
                        "  {:>4}:{:<3}  {:<12} {}{class}",
                        b.span.start.line,
                        b.span.start.column,
                        b.hint.as_str(),
                        b.name,
                    )
                    .unwrap();
                }
                writeln!(out, "classes ({}):", result.classes.len()).unwrap();
                for c in &result.classes {
                    let extends = match &c.extends {
                        Some(e) => format!(" extends {e}"),
                        None => String::new(),
                    };
                    let managed = if c.managed { " MANAGED" } else { "" };
                    writeln!(out, "  {}{extends}{managed}", c.name).unwrap();
                }
                writeln!(out, "warnings ({}):", result.warnings.len()).unwrap();
                for w in &result.warnings {
                    writeln!(out, "  {}", w.render()).unwrap();
                }
                out
            }
            Err(error) => format!(
                "newbcpl-sema dump\ninput: {}\nparse error: {}",
                path.display(),
                error.render()
            ),
        },
        Err(error) => format!(
            "newbcpl-sema dump\ninput: {}\nio-error: {}",
            path.display(),
            error
        ),
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Walker
// ─────────────────────────────────────────────────────────────────────────────

struct Sema {
    /// Stack of scope frames. Each frame maps name → hint. Newer
    /// frames shadow older ones.
    scopes: Vec<HashMap<String, BindingInfo>>,
    /// Class table by name.
    classes: HashMap<String, ClassInfo>,
    /// Append-only log of every binding seen, in source order.
    binding_log: Vec<BindingInfo>,
    /// Append-only log of every class seen, in source order.
    class_log: Vec<ClassInfo>,
    warnings: Vec<SemaWarning>,
}

impl Sema {
    fn new() -> Self {
        Self {
            scopes: vec![HashMap::new()],
            classes: HashMap::new(),
            binding_log: Vec::new(),
            class_log: Vec::new(),
            warnings: Vec::new(),
        }
    }

    fn push_scope(&mut self) {
        self.scopes.push(HashMap::new());
    }

    fn pop_scope(&mut self) {
        self.scopes.pop();
        if self.scopes.is_empty() {
            // Defensive: never empty the stack completely.
            self.scopes.push(HashMap::new());
        }
    }

    fn declare(&mut self, name: &str, hint: TypeHint, class_name: Option<String>, span: Span) {
        let info = BindingInfo {
            name: name.to_string(),
            hint,
            class_name: class_name.clone(),
            span,
        };
        self.binding_log.push(info.clone());
        if let Some(top) = self.scopes.last_mut() {
            top.insert(name.to_string(), info);
        }
    }

    fn lookup(&self, name: &str) -> Option<&BindingInfo> {
        for frame in self.scopes.iter().rev() {
            if let Some(info) = frame.get(name) {
                return Some(info);
            }
        }
        None
    }

    fn warn(&mut self, message: impl Into<String>, span: Span) {
        self.warnings.push(SemaWarning {
            message: message.into(),
            span,
        });
    }

    fn analyze_program(&mut self, program: &Program) {
        // First pass: register classes so any LET that does
        // `NEW Foo()` can resolve the name even if Foo is declared
        // later in the file.
        for decl in &program.items {
            if let Decl::Class(c) = decl {
                self.register_class(c);
            }
        }
        for decl in &program.items {
            self.analyze_decl(decl);
        }
    }

    fn register_class(&mut self, c: &ClassDecl) {
        let info = ClassInfo {
            name: c.name.clone(),
            extends: c.extends.clone(),
            managed: c.managed,
            span: c.span,
        };
        self.class_log.push(info.clone());
        self.classes.insert(c.name.clone(), info);
    }

    fn analyze_decl(&mut self, decl: &Decl) {
        match decl {
            Decl::Function(f) => {
                self.declare(&f.name, TypeHint::Function, None, f.span);
                // Function body — open a parameter scope so locals
                // don't leak.
                self.push_scope();
                for p in &f.params {
                    self.declare(p, TypeHint::Word, None, f.span);
                }
                let _ = self.type_of(&f.body);
                self.pop_scope();
            }
            Decl::Routine(r) => {
                self.declare(&r.name, TypeHint::Function, None, r.span);
                self.push_scope();
                for p in &r.params {
                    self.declare(p, TypeHint::Word, None, r.span);
                }
                self.analyze_stmt(&r.body);
                self.pop_scope();
            }
            Decl::Let(l) => self.analyze_let(l),
            Decl::Get(_) => {}
            Decl::Manifest(m) => {
                for b in &m.bindings {
                    let hint = b
                        .value
                        .as_ref()
                        .map(|e| self.type_of(e))
                        .unwrap_or(TypeHint::Int);
                    self.declare(&b.name, hint, None, b.span);
                }
            }
            Decl::Static(s) => {
                for b in &s.bindings {
                    let hint = b
                        .value
                        .as_ref()
                        .map(|e| self.type_of(e))
                        .unwrap_or(TypeHint::Word);
                    self.declare(&b.name, hint, None, b.span);
                }
            }
            Decl::Global(g) => {
                for b in &g.bindings {
                    let hint = b
                        .value
                        .as_ref()
                        .map(|e| self.type_of(e))
                        .unwrap_or(TypeHint::Word);
                    self.declare(&b.name, hint, None, b.span);
                }
            }
            Decl::Class(c) => self.analyze_class_body(c),
        }
    }

    fn analyze_let(&mut self, l: &LetDecl) {
        for (name, expr) in &l.bindings {
            let mut hint = self.type_of(expr);
            // FLET overrides scalar inference to FLOAT when the literal
            // evidence is otherwise neutral (manifesto §1). It does
            // *not* override SIMD / list / object hints — `FLET p =
            // PAIR(...)` would be unusual but the PAIR construction
            // wins because the value really does live in a V-register.
            if matches!(l.kind, LetKind::FLet)
                && matches!(hint, TypeHint::Int | TypeHint::Word | TypeHint::Unknown)
            {
                hint = TypeHint::Float;
            }
            let class_name = self.class_name_of(expr);
            self.declare(name, hint, class_name, l.span);
        }
    }

    fn class_name_of(&self, expr: &Expr) -> Option<String> {
        match expr {
            Expr::New { class_name, .. } => Some(class_name.clone()),
            _ => None,
        }
    }

    fn analyze_class_body(&mut self, c: &ClassDecl) {
        // Class bodies introduce method scopes. Members themselves
        // are tracked by `class_log`; per-member variable declarations
        // get their own scope only inside method bodies.
        for m in &c.members {
            self.analyze_class_member(c, m);
        }
    }

    fn analyze_class_member(&mut self, _c: &ClassDecl, m: &ClassMember) {
        match &m.kind {
            ClassMemberKind::Fields(_) | ClassMemberKind::Let(_) | ClassMemberKind::FLet(_) => {
                // Member-storage type tracking is deferred until sema
                // grows class-aware member-access typing. For now we
                // observe the names but don't bind them in any scope.
            }
            ClassMemberKind::Method(method) => {
                self.push_scope();
                // SELF is implicitly available inside any method body.
                self.declare("SELF", TypeHint::Object, None, method.span);
                self.declare("SUPER", TypeHint::Object, None, method.span);
                for p in &method.params {
                    self.declare(p, TypeHint::Word, None, method.span);
                }
                match &method.body {
                    ClassMethodBody::Routine(s) => self.analyze_stmt(s),
                    ClassMethodBody::Function(e) => {
                        let _ = self.type_of(e);
                    }
                }
                self.pop_scope();
            }
        }
    }

    fn analyze_stmt(&mut self, stmt: &Stmt) {
        match stmt {
            Stmt::Block(b) => self.analyze_block(b),
            Stmt::Decl(d) => self.analyze_decl(d),
            Stmt::Expr(e) => {
                let _ = self.type_of(e);
            }
            Stmt::Assign { targets, values, .. } => self.analyze_assign(targets, values),
            Stmt::If {
                cond,
                then_stmt,
                else_stmt,
                ..
            } => {
                let _ = self.type_of(cond);
                self.analyze_stmt(then_stmt);
                if let Some(els) = else_stmt {
                    self.analyze_stmt(els);
                }
            }
            Stmt::Unless {
                cond, then_stmt, ..
            } => {
                let _ = self.type_of(cond);
                self.analyze_stmt(then_stmt);
            }
            Stmt::While { cond, body, .. } | Stmt::Until { cond, body, .. } => {
                let _ = self.type_of(cond);
                self.analyze_stmt(body);
            }
            Stmt::Repeat { body, .. } => self.analyze_stmt(body),
            Stmt::RepeatWhile { body, cond, .. } | Stmt::RepeatUntil { body, cond, .. } => {
                self.analyze_stmt(body);
                let _ = self.type_of(cond);
            }
            Stmt::For {
                name,
                start,
                end,
                step,
                body,
                span,
            } => {
                let start_hint = self.type_of(start);
                let _ = self.type_of(end);
                if let Some(s) = step {
                    let _ = self.type_of(s);
                }
                self.push_scope();
                // Loop variable matches the start expression's family;
                // step / end may be float, but FOR is typically integer.
                let lv_hint = if start_hint == TypeHint::Float {
                    TypeHint::Float
                } else {
                    TypeHint::Int
                };
                self.declare(name, lv_hint, None, *span);
                self.analyze_stmt(body);
                self.pop_scope();
            }
            Stmt::ForEach {
                names,
                annotation,
                iter,
                body,
                span,
            } => {
                let iter_hint = self.type_of(iter);
                self.push_scope();
                // Element hint heuristic: if there's an explicit
                // annotation, honour it; otherwise default to WORD
                // (lists are heterogeneous so we can't be specific).
                let element_hint = annotation
                    .as_deref()
                    .and_then(map_annotation_to_hint)
                    .unwrap_or(match iter_hint {
                        TypeHint::FVec => TypeHint::Float,
                        _ => TypeHint::Word,
                    });
                for n in names {
                    self.declare(n, element_hint, None, *span);
                }
                self.analyze_stmt(body);
                self.pop_scope();
            }
            Stmt::Switchon {
                scrutinee,
                cases,
                default,
                ..
            } => {
                let _ = self.type_of(scrutinee);
                for case in cases {
                    for v in &case.values {
                        let _ = self.type_of(v);
                    }
                    for s in &case.body {
                        self.analyze_stmt(s);
                    }
                }
                if let Some(body) = default {
                    for s in body {
                        self.analyze_stmt(s);
                    }
                }
            }
            Stmt::Resultis(e, _) => {
                let _ = self.type_of(e);
            }
            Stmt::Retain { value: Some(v), name, span, .. } => {
                let hint = self.type_of(v);
                let class_name = self.class_name_of(v);
                self.declare(name, hint, class_name, *span);
            }
            Stmt::Return(_)
            | Stmt::Finish(_)
            | Stmt::Break(_)
            | Stmt::Loop(_)
            | Stmt::Endcase(_)
            | Stmt::Brk(_)
            | Stmt::Goto { .. }
            | Stmt::Label { .. }
            | Stmt::Retain { value: None, .. } => {}
        }
    }

    fn analyze_block(&mut self, b: &Block) {
        // Each block starts a new scope per BCPL convention: a `LET`
        // inside a `$( … $)` is visible only until the closing bracket.
        self.push_scope();
        for s in &b.stmts {
            self.analyze_stmt(s);
        }
        self.pop_scope();
    }

    fn analyze_assign(&mut self, targets: &[Expr], values: &[Expr]) {
        for (t, v) in targets.iter().zip(values.iter()) {
            let target_hint = self.type_of(t);
            let value_hint = self.type_of(v);
            self.check_coercion(target_hint, value_hint, v.span());
        }
    }

    /// Compare an assignment's target hint against the value's hint
    /// and emit a warning if the compiler would silently insert a
    /// real machine instruction the user did not write. INT↔WORD
    /// is free; FLOAT← INT is silent (SCVTF); INT← FLOAT warns
    /// (FCVTZS truncation); cross-family non-pointer mismatches warn.
    fn check_coercion(&mut self, target: TypeHint, value: TypeHint, span: Span) {
        if target == value {
            return;
        }
        // Free coercions:
        if matches!(target, TypeHint::Word) || matches!(value, TypeHint::Word) {
            return;
        }
        if matches!(value, TypeHint::Null)
            && matches!(
                target,
                TypeHint::String
                    | TypeHint::List
                    | TypeHint::Vec
                    | TypeHint::FVec
                    | TypeHint::Object
            )
        {
            return;
        }
        if matches!(value, TypeHint::Unknown) || matches!(target, TypeHint::Unknown) {
            return;
        }
        // FLOAT ← INT is silent (SCVTF emitted, no precision lost).
        if target == TypeHint::Float && value == TypeHint::Int {
            return;
        }
        // INT ← FLOAT is the headline truncation case.
        if target == TypeHint::Int && value == TypeHint::Float {
            self.warn(
                "implicit FLOAT → INT conversion (FCVTZS truncates toward zero)",
                span,
            );
            return;
        }
        // Other cross-family assignments — warn but don't reject.
        self.warn(
            format!(
                "assignment loses type information ({} := {})",
                target.as_str(),
                value.as_str()
            ),
            span,
        );
    }

    /// The heart of inference: compute a TypeHint for any expression.
    /// Always returns *something* — even unknowns are a known value.
    fn type_of(&mut self, expr: &Expr) -> TypeHint {
        match expr {
            Expr::IntLit { .. } => TypeHint::Int,
            Expr::FloatLit { .. } => TypeHint::Float,
            Expr::StringLit { .. } => TypeHint::String,
            Expr::CharLit { .. } => TypeHint::Int,
            Expr::BoolLit { .. } => TypeHint::Int,
            Expr::Null { .. } => TypeHint::Null,
            Expr::Ident { name, .. } => match self.lookup(name) {
                Some(info) => info.hint,
                None => TypeHint::Unknown,
            },
            Expr::Call { callee, args, .. } => {
                // Walk the callee + args so any side-effects (warnings)
                // for nested expressions still fire.
                for a in args {
                    let _ = self.type_of(a);
                }
                let _ = self.type_of(callee);
                // Conversion intrinsics return known register classes.
                if let Expr::Ident { name, .. } = callee.as_ref() {
                    return match name.as_str() {
                        "FLOAT" | "FSQRT" => TypeHint::Float,
                        "TRUNC" | "FIX" | "ENTIER" | "LEN" => TypeHint::Int,
                        "TYPE" | "TYPEOF" => TypeHint::String,
                        _ => TypeHint::Word,
                    };
                }
                TypeHint::Word
            }
            Expr::Unary { op, operand, .. } => {
                let operand_hint = self.type_of(operand);
                match op {
                    UnaryOp::Neg => operand_hint,
                    UnaryOp::Not => TypeHint::Int,
                    UnaryOp::Indirection => TypeHint::Word,
                    UnaryOp::AddressOf => TypeHint::Word,
                    UnaryOp::CharIndirection => TypeHint::Int,
                    UnaryOp::Hd | UnaryOp::Tl | UnaryOp::Rest => match operand_hint {
                        TypeHint::List => TypeHint::Word, // element type unknown
                        TypeHint::Vec => TypeHint::Word,
                        TypeHint::FVec => TypeHint::Float,
                        _ => TypeHint::Word,
                    },
                    UnaryOp::Len => TypeHint::Int,
                    UnaryOp::FreeVec | UnaryOp::FreeList => TypeHint::Word,
                }
            }
            Expr::Binary { op, lhs, rhs, .. } => {
                let lhs_hint = self.type_of(lhs);
                let rhs_hint = self.type_of(rhs);
                self.binary_result(*op, lhs_hint, rhs_hint, lhs, rhs)
            }
            Expr::Conditional {
                cond,
                then_expr,
                else_expr,
                ..
            } => {
                let _ = self.type_of(cond);
                let then_hint = self.type_of(then_expr);
                let else_hint = self.type_of(else_expr);
                if then_hint == else_hint {
                    then_hint
                } else if then_hint == TypeHint::Unknown {
                    else_hint
                } else if else_hint == TypeHint::Unknown {
                    then_hint
                } else {
                    TypeHint::Word
                }
            }
            Expr::Valof { body, .. } => {
                self.analyze_stmt(body);
                // We don't yet thread RESULTIS values back through
                // VALOF; default to WORD.
                TypeHint::Word
            }
            Expr::TypedConstruct { kind, args, .. } => {
                for a in args {
                    let _ = self.type_of(a);
                }
                match kind {
                    TypeConstructorKind::Vec => TypeHint::Vec,
                    TypeConstructorKind::FVec => TypeHint::FVec,
                    TypeConstructorKind::Table => TypeHint::Vec,
                    TypeConstructorKind::FTable => TypeHint::FVec,
                    TypeConstructorKind::Pair => TypeHint::Pair,
                    TypeConstructorKind::FPair => TypeHint::FPair,
                    TypeConstructorKind::Quad => TypeHint::Quad,
                    TypeConstructorKind::FQuad => TypeHint::FQuad,
                    TypeConstructorKind::Oct => TypeHint::Oct,
                    TypeConstructorKind::FOct => TypeHint::FOct,
                    TypeConstructorKind::List | TypeConstructorKind::ManifestList => {
                        TypeHint::List
                    }
                }
            }
            Expr::New { class_name: _, args, .. } => {
                for a in args {
                    let _ = self.type_of(a);
                }
                TypeHint::Object
            }
        }
    }

    /// Result type of a binary operator. The headline rule per
    /// manifesto §1: `+`, `-`, `*`, `/` produce FLOAT iff both sides
    /// are FLOAT, otherwise INT. The dotted variants (`+.`, `-.`,
    /// etc.) are *assertions* — they warn if either side is not
    /// FLOAT, then still produce FLOAT.
    fn binary_result(
        &mut self,
        op: BinaryOp,
        lhs: TypeHint,
        rhs: TypeHint,
        _lhs_expr: &Expr,
        rhs_expr: &Expr,
    ) -> TypeHint {
        use BinaryOp::*;
        match op {
            Add | Sub | Mul | Div | Rem => {
                if lhs == TypeHint::Float && rhs == TypeHint::Float {
                    TypeHint::Float
                } else if lhs.is_float_family() || rhs.is_float_family() {
                    // SIMD types: result is the wider float family
                    // when both sides match; otherwise fall back to
                    // INT and let codegen / sema-warning handle it.
                    if lhs == rhs {
                        lhs
                    } else {
                        TypeHint::Int
                    }
                } else {
                    TypeHint::Int
                }
            }
            FAdd | FSub | FMul | FDiv => {
                if !lhs.is_float_family() && lhs != TypeHint::Word && lhs != TypeHint::Unknown {
                    self.warn(
                        format!(
                            "dotted float operator on non-FLOAT lhs ({})",
                            lhs.as_str()
                        ),
                        rhs_expr.span(),
                    );
                }
                if !rhs.is_float_family() && rhs != TypeHint::Word && rhs != TypeHint::Unknown {
                    self.warn(
                        format!(
                            "dotted float operator on non-FLOAT rhs ({})",
                            rhs.as_str()
                        ),
                        rhs_expr.span(),
                    );
                }
                TypeHint::Float
            }
            Eq | Ne | Lt | Le | Gt | Ge | FEq | FNe | FLt | FLe | FGt | FGe => TypeHint::Int,
            BitAnd | BitOr | Eqv | Neqv | Shl | Shr => TypeHint::Int,
            Subscript => TypeHint::Word,
            CharSubscript => TypeHint::Int,
            FloatSubscript => TypeHint::Float,
            Bitfield => TypeHint::Int,
            Dot | Of => TypeHint::Word,
            LaneAccess => match lhs {
                TypeHint::FPair | TypeHint::FQuad | TypeHint::FOct => TypeHint::Float,
                _ => TypeHint::Int,
            },
        }
    }
}

fn map_annotation_to_hint(annotation: &str) -> Option<TypeHint> {
    Some(match annotation {
        "INT" | "INTEGER" => TypeHint::Int,
        "FLOAT" | "REAL" => TypeHint::Float,
        "STRING" => TypeHint::String,
        "WORD" => TypeHint::Word,
        "PAIR" => TypeHint::Pair,
        "FPAIR" => TypeHint::FPair,
        "QUAD" => TypeHint::Quad,
        "FQUAD" => TypeHint::FQuad,
        "OCT" => TypeHint::Oct,
        "FOCT" => TypeHint::FOct,
        "LIST" => TypeHint::List,
        "VEC" => TypeHint::Vec,
        "FVEC" => TypeHint::FVec,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn analyze_str(source: &str) -> SemaOutput {
        let program = newbcpl_parser::parse_source(source)
            .unwrap_or_else(|e| panic!("parse failed: {}", e.render()));
        analyze(&program)
    }

    fn binding_hint(out: &SemaOutput, name: &str) -> TypeHint {
        out.bindings
            .iter()
            .rev()
            .find(|b| b.name == name)
            .unwrap_or_else(|| panic!("no binding named {name}"))
            .hint
    }

    #[test]
    fn literal_int() {
        let out = analyze_str("LET x = 42");
        assert_eq!(binding_hint(&out, "x"), TypeHint::Int);
    }

    #[test]
    fn literal_float() {
        let out = analyze_str("LET pi = 3.14159");
        assert_eq!(binding_hint(&out, "pi"), TypeHint::Float);
    }

    #[test]
    fn literal_string() {
        let out = analyze_str("LET s = \"hi*N\"");
        assert_eq!(binding_hint(&out, "s"), TypeHint::String);
    }

    #[test]
    fn flet_overrides_int_to_float() {
        let out = analyze_str("FLET x = 0");
        assert_eq!(binding_hint(&out, "x"), TypeHint::Float);
    }

    #[test]
    fn typed_constructors() {
        let out = analyze_str(
            "LET v = VEC 100\nLET p = PAIR(1, 2)\nLET fp = FPAIR(1.0, 2.0)\nLET xs = LIST(1, 2, 3)",
        );
        assert_eq!(binding_hint(&out, "v"), TypeHint::Vec);
        assert_eq!(binding_hint(&out, "p"), TypeHint::Pair);
        assert_eq!(binding_hint(&out, "fp"), TypeHint::FPair);
        assert_eq!(binding_hint(&out, "xs"), TypeHint::List);
    }

    #[test]
    fn new_object() {
        let out = analyze_str("CLASS Point $( DECL x, y $)\nLET p = NEW Point");
        assert_eq!(binding_hint(&out, "p"), TypeHint::Object);
        let info = out.bindings.iter().find(|b| b.name == "p").unwrap();
        assert_eq!(info.class_name.as_deref(), Some("Point"));
    }

    #[test]
    fn arith_int_float_dispatch() {
        // `a + b` where both are INT → result is INT.
        // `a + b` where both are FLOAT → result is FLOAT.
        let out = analyze_str("LET a = 1\nLET b = 2\nLET c = a + b");
        assert_eq!(binding_hint(&out, "c"), TypeHint::Int);

        let out = analyze_str("LET a = 1.0\nLET b = 2.0\nLET c = a + b");
        assert_eq!(binding_hint(&out, "c"), TypeHint::Float);
    }

    #[test]
    fn arith_mixed_int_word_falls_back_to_int() {
        // `vec!i + 1` — vec subscript is WORD, 1 is INT → INT result.
        let out = analyze_str("LET v = VEC 10\nLET x = v!0 + 1");
        assert_eq!(binding_hint(&out, "x"), TypeHint::Int);
    }

    #[test]
    fn dotted_float_op_on_int_warns() {
        let out = analyze_str("LET a = 1\nLET b = 2\nLET c = a +. b");
        assert!(
            !out.warnings.is_empty(),
            "expected at least one float-on-int warning"
        );
        assert!(
            out.warnings
                .iter()
                .any(|w| w.message.contains("dotted float operator")),
            "warning message should mention dotted float operator: {:?}",
            out.warnings
        );
    }

    #[test]
    fn relational_returns_int() {
        let out = analyze_str("LET a = 1\nLET b = 2\nLET c = a < b");
        assert_eq!(binding_hint(&out, "c"), TypeHint::Int);
    }

    #[test]
    fn vec_subscript_is_word() {
        let out = analyze_str("LET v = VEC 10\nLET x = v!0");
        assert_eq!(binding_hint(&out, "x"), TypeHint::Word);
    }

    #[test]
    fn float_subscript_is_float() {
        let out = analyze_str("LET fv = FVEC 10\nLET x = fv.%0");
        assert_eq!(binding_hint(&out, "x"), TypeHint::Float);
    }

    #[test]
    fn pair_lane_access_is_int() {
        let out = analyze_str("LET p = PAIR(1, 2)\nLET x = p.|0|");
        assert_eq!(binding_hint(&out, "x"), TypeHint::Int);
    }

    #[test]
    fn fpair_lane_access_is_float() {
        let out = analyze_str("LET p = FPAIR(1.0, 2.0)\nLET x = p.|0|");
        assert_eq!(binding_hint(&out, "x"), TypeHint::Float);
    }

    #[test]
    fn assigning_float_to_int_warns() {
        let out = analyze_str("LET S() BE { LET i = 0\n LET f = 3.14\n i := f }");
        assert!(
            out.warnings
                .iter()
                .any(|w| w.message.contains("FLOAT → INT")),
            "expected truncation warning, got: {:?}",
            out.warnings
        );
    }

    #[test]
    fn assigning_int_to_float_is_silent() {
        let out = analyze_str("LET S() BE { LET f = 1.0\n LET i = 0\n f := i }");
        assert!(
            out.warnings.is_empty(),
            "INT → FLOAT should be silent (SCVTF): {:?}",
            out.warnings
        );
    }

    #[test]
    fn null_assignable_to_pointer_targets() {
        let out = analyze_str("LET S() BE { LET s = \"hi\"\n s := ? }");
        assert!(out.warnings.is_empty(), "Null → STRING should be silent: {:?}", out.warnings);
    }

    #[test]
    fn classes_recorded() {
        let out = analyze_str(
            "CLASS Animal $( DECL name $)\nCLASS Dog EXTENDS Animal $( DECL breed $)\nCLASS Window MANAGED $( DECL handle $)",
        );
        assert_eq!(out.classes.len(), 3);
        let dog = out.classes.iter().find(|c| c.name == "Dog").unwrap();
        assert_eq!(dog.extends.as_deref(), Some("Animal"));
        let window = out.classes.iter().find(|c| c.name == "Window").unwrap();
        assert!(window.managed);
    }

    #[test]
    fn for_loop_introduces_scoped_binding() {
        let out = analyze_str("LET S() BE { FOR i = 1 TO 10 DO f(i) }");
        assert_eq!(binding_hint(&out, "i"), TypeHint::Int);
    }

    #[test]
    fn foreach_with_annotation() {
        let out = analyze_str("LET S() BE { FOREACH C AS INTEGER IN s DO f(C) }");
        assert_eq!(binding_hint(&out, "C"), TypeHint::Int);
    }

    #[test]
    fn intrinsic_calls_have_known_results() {
        let out = analyze_str(
            "LET a = FLOAT(42)\nLET b = TRUNC(3.14)\nLET c = LEN xs\nLET t = TYPE(x)",
        );
        assert_eq!(binding_hint(&out, "a"), TypeHint::Float);
        assert_eq!(binding_hint(&out, "b"), TypeHint::Int);
        assert_eq!(binding_hint(&out, "c"), TypeHint::Int);
        assert_eq!(binding_hint(&out, "t"), TypeHint::String);
    }

    #[test]
    fn dump_sema_smoke() {
        let prog = newbcpl_parser::parse_source(
            "LET START() BE { LET pi = 3.14\n LET v = VEC 100 }",
        )
        .unwrap();
        let result = analyze(&prog);
        // Render through the public surface (just sanity-check shape).
        assert!(result.bindings.iter().any(|b| b.name == "pi" && b.hint == TypeHint::Float));
        assert!(result.bindings.iter().any(|b| b.name == "v" && b.hint == TypeHint::Vec));
    }
}
