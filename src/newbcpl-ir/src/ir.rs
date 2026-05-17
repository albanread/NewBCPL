//! NewBCPL typed IR.
//!
//! Sits between the typed AST (`newbcpl-parser` + `newbcpl-sema`) and
//! LLVM IR emission. Designed to be LLVM-friendly:
//!
//! - Each function is a CFG of basic blocks. Blocks end in a single
//!   terminator. No fallthrough — control flow is explicit.
//! - Locals are stack slots produced by `Alloca`; reading a local is
//!   an explicit `Load`, writing is a `Store`. This avoids the need
//!   for phi nodes in the front-end IR; LLVM's mem2reg pass promotes
//!   the slots to registers later.
//! - Every `ValueId` is single-assignment. Mutation of a source-level
//!   variable goes through stores to its slot.
//! - Every instruction that produces a value records its `TypeHint`
//!   so codegen can pick LLVM types directly without re-deriving.
//!
//! Object layouts come along from sema as `ClassLayout` records;
//! they're passed to codegen alongside the Module.

use newbcpl_sema::ClassLayout;
use newbcpl_sema::TypeHint;

/// Every `Module` corresponds to one .bcl translation unit.
#[derive(Debug, Clone)]
pub struct Module {
    pub name: String,
    pub functions: Vec<Function>,
    pub layouts: Vec<ClassLayout>,
    /// `GLOBAL`-declared bindings — each becomes a module-level
    /// LLVM `@<name>` global. The optional integer is the
    /// initializer when sema can constant-fold it; `None` falls
    /// back to a zero-init slot.
    pub globals: Vec<GlobalDecl>,
    /// `ASM { }` procedure definitions.  Each is emitted as a
    /// `module asm` blob plus a matching `declare` in the LLVM IR.
    pub asm_procs: Vec<new_asm::AsmProc>,
}

impl Module {
    /// True if `name` is an ASM-body procedure (not a regular LLVM
    /// function).  Used by the JIT missing-symbol check to avoid
    /// flagging legitimate `declare`s whose bodies live in `module asm`.
    pub fn is_asm_proc(&self, name: &str) -> bool {
        self.asm_procs.iter().any(|p| p.name == name)
    }
}

/// One `GLOBAL` binding promoted to a module-level slot.
#[derive(Debug, Clone)]
pub struct GlobalDecl {
    pub name: String,
    pub initial: Option<i64>,
}

/// Stable, monotonically-allocated identifier for a value produced
/// inside a function. Renders as `%N` in dumps.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ValueId(pub u32);

/// Stable identifier for a basic block within a function. Renders
/// as `bb<N>` in dumps.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct BlockId(pub u32);

#[derive(Debug, Clone)]
pub struct Function {
    pub name: String,
    /// Parameters in declaration order. Each carries its own
    /// alloca'd slot so the body can store into it on entry.
    pub params: Vec<Param>,
    /// Inferred result hint from sema. Routines are `Word`.
    pub return_hint: TypeHint,
    pub blocks: Vec<BasicBlock>,
    pub entry: BlockId,
}

#[derive(Debug, Clone)]
pub struct Param {
    pub name: String,
    pub hint: TypeHint,
    /// Stack slot allocated for this parameter at function entry,
    /// so the body sees it through the same Load/Store dance as
    /// local LET bindings.
    pub slot: ValueId,
    /// SSA value representing the incoming parameter; the entry
    /// block stores this into `slot`.
    pub in_value: ValueId,
}

#[derive(Debug, Clone)]
pub struct BasicBlock {
    pub id: BlockId,
    pub label: String,
    pub instrs: Vec<Instr>,
    pub terminator: Terminator,
}

#[derive(Debug, Clone)]
pub enum Instr {
    /// Pure constant materialisation. Most uses inline constants
    /// directly in `Value::Const`; this exists for cases where
    /// codegen prefers a named slot.
    Const {
        dst: ValueId,
        value: Const,
        hint: TypeHint,
    },
    /// Allocate a stack slot for a local or parameter.
    Alloca {
        dst: ValueId,
        hint: TypeHint,
        name: String,
    },
    /// Load from a stack slot.
    Load {
        dst: ValueId,
        slot: ValueId,
        hint: TypeHint,
    },
    /// Store to a stack slot.
    Store {
        slot: ValueId,
        value: Value,
    },
    /// Binary operator. The op encodes int / float family already so
    /// codegen doesn't need to look at operand hints again.
    BinOp {
        dst: ValueId,
        op: IrBinOp,
        lhs: Value,
        rhs: Value,
        hint: TypeHint,
    },
    UnaryOp {
        dst: ValueId,
        op: IrUnOp,
        operand: Value,
        hint: TypeHint,
    },
    /// Direct call. `callee` is either a `Value::Function(name)` for
    /// a known function or a `Value::Local(...)` for an indirect call.
    Call {
        dst: Option<ValueId>,
        callee: Value,
        args: Vec<Value>,
        hint: TypeHint,
    },
    /// `NEW Class(args)` — heap-allocate a new instance of `class`
    /// (codegen calls the GC allocator with the class's TypeDesc),
    /// then invoke `CREATE` on the new object with `args`. Result
    /// is the instance pointer.
    New {
        dst: ValueId,
        class_name: String,
        args: Vec<Value>,
    },
    /// Load a field from a class instance. `byte_offset` comes from
    /// the class layout; codegen emits a GEP + load.
    FieldLoad {
        dst: ValueId,
        base: Value,
        byte_offset: usize,
        hint: TypeHint,
    },
    /// Store a value into a class instance field.
    FieldStore {
        base: Value,
        byte_offset: usize,
        value: Value,
    },
    /// `obj.method(args)` — virtual method dispatch. The receiver's
    /// class layout assigns each method a stable `vtable_slot`.
    /// Codegen loads the vtable from the instance, indexes by slot,
    /// loads the method pointer, and emits an indirect call (with
    /// the receiver as the implicit first argument). `class_name`
    /// is the receiver's static class — codegen needs it to pick
    /// the right `@Class.vtable` global at the call site (and to
    /// know the function-pointer signature for the indirect call).
    MethodCall {
        dst: Option<ValueId>,
        receiver: Value,
        class_name: String,
        vtable_slot: usize,
        method_name: String,
        args: Vec<Value>,
        hint: TypeHint,
    },
    /// Type-erased method dispatch. Used when sema / IR can't
    /// determine the receiver's static class — typically an
    /// untyped routine parameter `LET draw(shape) BE shape.render()`.
    /// Codegen lowers this to a `__newbcpl_lookup_method(receiver,
    /// "<method_name>")` call followed by an indirect call through
    /// the returned function pointer. The runtime helper walks the
    /// receiver's `TypeDesc.method_names` to find the matching
    /// vtable slot. Args are passed verbatim to the resolved
    /// method, with the receiver prepended as the implicit first
    /// argument.
    IndirectMethodCall {
        dst: Option<ValueId>,
        receiver: Value,
        method_name: String,
        args: Vec<Value>,
        hint: TypeHint,
    },
    /// `!ptr` — load the value at an address. Used for both prefix
    /// `!ptr` and the result of subscript-family lowering
    /// (`v!i` / `v%i` / `v.%i`) after the GEP step. `byte_width`
    /// records the load width: 8 for word / float / pointer loads,
    /// 1 for the char-subscript path (`v%i` / `%ptr`) so codegen
    /// emits `load i8 + zext to i64`. Default 8 keeps every existing
    /// caller correct; the byte path opts in explicitly.
    IndirectLoad {
        dst: ValueId,
        addr: Value,
        hint: TypeHint,
        byte_width: u32,
    },
    /// `!ptr := value` — store a value at an address. `byte_width`
    /// mirrors the load: 1 for byte stores (`%ptr := v` and `v%i :=
    /// v`), 8 otherwise. Codegen truncates the source value to i8
    /// when narrowing.
    IndirectStore {
        addr: Value,
        value: Value,
        byte_width: u32,
    },
    /// Read from a `GLOBAL`-declared module-level variable by name.
    /// Codegen emits `load i64, ptr @<name>`.
    GlobalLoad {
        dst: ValueId,
        name: String,
        hint: TypeHint,
    },
    /// Write to a `GLOBAL`-declared module-level variable by name.
    /// Codegen emits `store i64 <value>, ptr @<name>`.
    GlobalStore {
        name: String,
        value: Value,
    },
    /// `base + index * element_bytes` — pointer arithmetic for
    /// subscripts. `element_bytes` is 8 for word vectors (`v!i`),
    /// 1 for char vectors (`v%i`), 8 for float vectors (`v.%i`,
    /// where each element is a 64-bit double).
    Gep {
        dst: ValueId,
        base: Value,
        index: Value,
        element_bytes: usize,
    },
    /// Typed constructor — `VEC k`, `FVEC k`, `LIST(a, b, c)`,
    /// `PAIR(a, b)`, `TABLE(...)`, etc. Codegen specialises on
    /// `kind`: scalar SIMD types stay in V-registers, vectors and
    /// lists call into the runtime to allocate. The IR keeps the
    /// abstract shape — kind + args — so the same node lowers to
    /// the right runtime call.
    TypedConstruct {
        dst: ValueId,
        kind: TypedKind,
        args: Vec<Value>,
        hint: TypeHint,
    },
    /// `pair.|n|` — extract a single lane from a SIMD value. The
    /// lane index is a runtime value (codegen still picks the
    /// `extractelement` form when it's a constant). `kind`
    /// records the source operand's SIMD shape so codegen knows
    /// whether to emit `extractelement` (for FQUAD / FOCT, which
    /// live in real LLVM vectors) or a sign-aware bit-shift (for
    /// the PAIR / FPAIR / QUAD / OCT family, which all pack into
    /// a single 64-bit word per the reference's ABI — see
    /// `docs/pair_and_multilane_types.md`).
    LaneExtract {
        dst: ValueId,
        vector: Value,
        lane: Value,
        kind: TypedKind,
        hint: TypeHint,
    },
    /// `pair.|n| := value` — produce a new SIMD value identical to
    /// `vector` except lane `lane` is replaced by `value`. The IR
    /// shape mirrors `LaneExtract`; the lowerer is responsible for
    /// storing the result back into the original lvalue when the
    /// source-level lhs of the lane access is a binding.
    LaneInsert {
        dst: ValueId,
        vector: Value,
        lane: Value,
        value: Value,
        kind: TypedKind,
    },
}

/// IR-level constructor kinds, mirroring the parser's
/// `TypeConstructorKind` 1:1 so codegen sees the same vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TypedKind {
    Vec,
    FVec,
    Table,
    FTable,
    Pair,
    FPair,
    Quad,
    FQuad,
    Oct,
    FOct,
    List,
    ManifestList,
}

impl TypedKind {
    pub fn as_str(self) -> &'static str {
        match self {
            TypedKind::Vec => "VEC",
            TypedKind::FVec => "FVEC",
            TypedKind::Table => "TABLE",
            TypedKind::FTable => "FTABLE",
            TypedKind::Pair => "PAIR",
            TypedKind::FPair => "FPAIR",
            TypedKind::Quad => "QUAD",
            TypedKind::FQuad => "FQUAD",
            TypedKind::Oct => "OCT",
            TypedKind::FOct => "FOCT",
            TypedKind::List => "LIST",
            TypedKind::ManifestList => "MANIFESTLIST",
        }
    }
}

#[derive(Debug, Clone)]
pub enum Terminator {
    /// Return from the function. `Some(v)` for functions; routines
    /// return `None`.
    Return(Option<Value>),
    /// Unconditional branch.
    Branch(BlockId),
    /// Conditional branch on `cond` (treated as nonzero = true).
    CondBranch {
        cond: Value,
        then_block: BlockId,
        else_block: BlockId,
    },
    /// `SWITCHON value INTO ...` — multi-target dispatch. Each entry
    /// in `cases` is a `(constant, target)` pair: when `value`
    /// matches the constant, control flows to `target`. If nothing
    /// matches, control flows to `default`. Codegen emits LLVM's
    /// `switch` instruction directly when all case constants are
    /// `Const::Int`; otherwise falls back to a chain of CondBranches.
    Switch {
        value: Value,
        cases: Vec<(Value, BlockId)>,
        default: BlockId,
    },
    /// Marker for blocks reached only via fallthrough that we never
    /// expect to execute (e.g. dead block after `RETURN`). Codegen
    /// emits an `unreachable` instruction.
    Unreachable,
}

#[derive(Debug, Clone)]
pub enum Value {
    Const(Const),
    /// SSA local — a value produced by a previous instruction.
    Local(ValueId),
    /// Reference to a function by source-level name. Codegen
    /// resolves to the actual function pointer (intra-module link
    /// or extern symbol).
    Function(String),
    /// `Unit` — for routine call results that don't produce a value.
    /// Codegen ignores this when it appears as a function arg
    /// (shouldn't happen in well-formed programs).
    Unit,
}

#[derive(Debug, Clone)]
pub enum Const {
    Int(i64),
    Float(f64),
    Bool(bool),
    Null,
    /// Raw lexeme including surrounding quotes and unprocessed
    /// `*`-escape sequences. Codegen cooks the escapes when
    /// emitting the string-table entry.
    String(String),
}

/// IR binary operations. The op variant encodes integer-vs-float
/// family choice that sema's flow analysis settled on, so codegen
/// just maps directly to the corresponding LLVM instruction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IrBinOp {
    // Integer arithmetic
    IAdd,
    ISub,
    IMul,
    IDiv,
    IRem,
    // Float arithmetic
    FAdd,
    FSub,
    FMul,
    FDiv,
    // Bitwise / logical
    BitAnd,
    BitOr,
    BitXor,
    Shl,
    Shr,
    // Integer relational (result is Int 0 or 1)
    ICmpEq,
    ICmpNe,
    ICmpLt,
    ICmpLe,
    ICmpGt,
    ICmpGe,
    // Float relational
    FCmpEq,
    FCmpNe,
    FCmpLt,
    FCmpLe,
    FCmpGt,
    FCmpGe,
}

impl IrBinOp {
    pub fn as_str(self) -> &'static str {
        match self {
            IrBinOp::IAdd => "iadd",
            IrBinOp::ISub => "isub",
            IrBinOp::IMul => "imul",
            IrBinOp::IDiv => "idiv",
            IrBinOp::IRem => "irem",
            IrBinOp::FAdd => "fadd",
            IrBinOp::FSub => "fsub",
            IrBinOp::FMul => "fmul",
            IrBinOp::FDiv => "fdiv",
            IrBinOp::BitAnd => "and",
            IrBinOp::BitOr => "or",
            IrBinOp::BitXor => "xor",
            IrBinOp::Shl => "shl",
            IrBinOp::Shr => "shr",
            IrBinOp::ICmpEq => "icmp.eq",
            IrBinOp::ICmpNe => "icmp.ne",
            IrBinOp::ICmpLt => "icmp.lt",
            IrBinOp::ICmpLe => "icmp.le",
            IrBinOp::ICmpGt => "icmp.gt",
            IrBinOp::ICmpGe => "icmp.ge",
            IrBinOp::FCmpEq => "fcmp.eq",
            IrBinOp::FCmpNe => "fcmp.ne",
            IrBinOp::FCmpLt => "fcmp.lt",
            IrBinOp::FCmpLe => "fcmp.le",
            IrBinOp::FCmpGt => "fcmp.gt",
            IrBinOp::FCmpGe => "fcmp.ge",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IrUnOp {
    /// Integer / word negation.
    INeg,
    /// Float negation.
    FNeg,
    /// Bitwise NOT.
    Not,
}

impl IrUnOp {
    pub fn as_str(self) -> &'static str {
        match self {
            IrUnOp::INeg => "ineg",
            IrUnOp::FNeg => "fneg",
            IrUnOp::Not => "not",
        }
    }
}
