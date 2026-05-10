//! IR -> LLVM IR emission.
//!
//! Walks a `newbcpl_ir::Module` and produces an `inkwell::module::Module`.
//! Intentionally LLVM-friendly because the IR was designed to be:
//! locals are alloca'd slots reached via Load / Store, every
//! value-producing instruction has a TypeHint that maps to a concrete
//! LLVM type, the CFG is already explicit.
//!
//! Bootstrap subset: routines / functions, integer + float scalar
//! arithmetic, locals, simple calls, IF / ELSE, RETURN, RESULTIS,
//! string constants. Subsequent commits add classes (NEW, vtable
//! dispatch, field load/store via GEP), SIMD types, list runtime
//! calls, GOTO / labels, and SWITCHON.

use std::collections::HashMap;

use inkwell::AddressSpace;
use inkwell::IntPredicate;
use inkwell::FloatPredicate;
use inkwell::basic_block::BasicBlock as LlvmBlock;
use inkwell::builder::Builder;
use inkwell::context::Context;
use inkwell::module::{Linkage, Module as LlvmModule};
use inkwell::types::{BasicMetadataTypeEnum, BasicType, BasicTypeEnum};
use inkwell::values::{BasicMetadataValueEnum, BasicValueEnum, FunctionValue, PointerValue};

use newbcpl_ir::{
    BasicBlock as IrBlock, BlockId, Const, Function as IrFunction, Instr, IrBinOp, IrUnOp,
    Module as IrModule, Param, Terminator, Value, ValueId,
};
use newbcpl_sema::TypeHint;

/// Top-level entry: produce a finalised LLVM module from our typed
/// IR. The caller owns the `Context`; the returned `LlvmModule`
/// borrows from it.
pub fn emit<'ctx>(context: &'ctx Context, ir: &IrModule) -> LlvmModule<'ctx> {
    let mut emitter = Emitter::new(context, &ir.name);
    emitter.emit_module(ir);
    emitter.module
}

struct Emitter<'ctx> {
    context: &'ctx Context,
    module: LlvmModule<'ctx>,
    builder: Builder<'ctx>,

    /// Each IR ValueId maps to a concrete LLVM value. For an Alloca
    /// it's a `PointerValue`; for arithmetic results it's an
    /// `IntValue` / `FloatValue`. Stored as the `BasicValueEnum`
    /// supertype.
    value_map: HashMap<ValueId, BasicValueEnum<'ctx>>,
    /// Each IR BlockId maps to its LLVM basic block.
    block_map: HashMap<BlockId, LlvmBlock<'ctx>>,
    /// Functions declared in this module by source-level name.
    /// Includes both BCPL-declared ones and externs we materialise on
    /// first encounter.
    by_name: HashMap<String, FunctionValue<'ctx>>,
    /// String literal pool — coalesces duplicate literals so each
    /// distinct string emits one global.
    string_pool: HashMap<String, PointerValue<'ctx>>,
    /// Counter for anonymous string globals.
    string_counter: u32,
}

impl<'ctx> Emitter<'ctx> {
    fn new(context: &'ctx Context, name: &str) -> Self {
        let module = context.create_module(name);
        Self {
            context,
            module,
            builder: context.create_builder(),
            value_map: HashMap::new(),
            block_map: HashMap::new(),
            by_name: HashMap::new(),
            string_pool: HashMap::new(),
            string_counter: 0,
        }
    }

    fn emit_module(&mut self, ir: &IrModule) {
        // Pass 1: declare every BCPL function so calls can resolve.
        for f in &ir.functions {
            self.declare_function(f);
        }
        // Pass 2: emit each body. Per-function maps reset between
        // functions since ValueIds and BlockIds are function-local.
        for f in &ir.functions {
            self.emit_function(f);
        }
    }

    // ─── declarations ───────────────────────────────────────────

    fn declare_function(&mut self, f: &IrFunction) -> FunctionValue<'ctx> {
        if let Some(&existing) = self.by_name.get(&f.name) {
            return existing;
        }
        let return_type = self.return_type_for(f.return_hint);
        let param_types: Vec<BasicMetadataTypeEnum> = f
            .params
            .iter()
            .map(|p| self.basic_type_for(p.hint).into())
            .collect();
        let fn_type = match return_type {
            Some(t) => t.fn_type(&param_types, false),
            None => self.context.void_type().fn_type(&param_types, false),
        };
        let fv = self.module.add_function(&f.name, fn_type, None);
        self.by_name.insert(f.name.clone(), fv);
        fv
    }

    /// Declare an extern function on demand. Used when the IR calls
    /// an unresolved name — typically a BCPL builtin (WRITES,
    /// WRITEN, NEWLINE, ...) or a runtime helper (__newbcpl_*).
    /// Signatures default to `i64 fn(i64, ..., i64)` — most BCPL
    /// builtins fit this. Special cases are added as we encounter
    /// real divergences.
    fn declare_extern(&mut self, name: &str, arg_count: usize) -> FunctionValue<'ctx> {
        if let Some(&existing) = self.by_name.get(name) {
            return existing;
        }
        let i64_t = self.context.i64_type();
        let ptr_t = self.context.ptr_type(AddressSpace::default());
        let fn_type = match name {
            // Known string-arg builtins.
            "WRITES" | "WRITEF" => {
                let args: Vec<BasicMetadataTypeEnum> = std::iter::once(ptr_t.into())
                    .chain(std::iter::repeat(i64_t.into()).take(arg_count.saturating_sub(1)))
                    .collect();
                i64_t.fn_type(&args, /* is_var_args = */ name == "WRITEF")
            }
            // Default: i64 fn(i64, ..., i64).
            _ => {
                let args: Vec<BasicMetadataTypeEnum> =
                    (0..arg_count).map(|_| i64_t.into()).collect();
                i64_t.fn_type(&args, false)
            }
        };
        let fv = self
            .module
            .add_function(name, fn_type, Some(Linkage::External));
        self.by_name.insert(name.to_string(), fv);
        fv
    }

    // ─── functions ──────────────────────────────────────────────

    fn emit_function(&mut self, f: &IrFunction) {
        let fv = self.by_name[&f.name];
        // Reset per-function state.
        self.value_map.clear();
        self.block_map.clear();

        // Allocate every basic block up front so any Branch /
        // CondBranch / Switch can resolve forward references.
        for block in &f.blocks {
            let llvm_block = self.context.append_basic_block(fv, &block.label);
            self.block_map.insert(block.id, llvm_block);
        }

        // The entry block runs the parameter alloca / store sequence
        // we emitted from lower.rs; bind each `in_value` to the
        // corresponding LLVM parameter so Store instructions for
        // them resolve correctly.
        let entry = self.block_map[&f.entry];
        self.builder.position_at_end(entry);
        for (i, p) in f.params.iter().enumerate() {
            self.bind_parameter(fv, i, p);
        }

        // Emit each block in source order.
        for block in &f.blocks {
            self.emit_block(block);
        }
    }

    fn bind_parameter(&mut self, fv: FunctionValue<'ctx>, idx: usize, p: &Param) {
        let llvm_param = fv
            .get_nth_param(idx as u32)
            .expect("parameter index in range");
        self.value_map.insert(p.in_value, llvm_param);
    }

    fn emit_block(&mut self, block: &IrBlock) {
        let llvm_block = self.block_map[&block.id];
        self.builder.position_at_end(llvm_block);
        for instr in &block.instrs {
            self.emit_instr(instr);
        }
        self.emit_terminator(&block.terminator);
    }

    // ─── instructions ───────────────────────────────────────────

    fn emit_instr(&mut self, instr: &Instr) {
        match instr {
            Instr::Const { dst, value, hint } => {
                let v = self.lower_const(value, *hint);
                self.value_map.insert(*dst, v);
            }
            Instr::Alloca { dst, hint, name } => {
                let ty = self.basic_type_for(*hint);
                let slot = self
                    .builder
                    .build_alloca(ty, name)
                    .expect("alloca");
                self.value_map.insert(*dst, slot.into());
            }
            Instr::Load { dst, slot, hint } => {
                let slot_ptr = self.lookup(*slot).into_pointer_value();
                let ty = self.basic_type_for(*hint);
                let loaded = self
                    .builder
                    .build_load(ty, slot_ptr, "load")
                    .expect("load");
                self.value_map.insert(*dst, loaded);
            }
            Instr::Store { slot, value } => {
                let slot_ptr = self.lookup(*slot).into_pointer_value();
                let v = self.lower_value(value);
                self.builder.build_store(slot_ptr, v).expect("store");
            }
            Instr::BinOp {
                dst,
                op,
                lhs,
                rhs,
                hint: _,
            } => {
                let l = self.lower_value(lhs);
                let r = self.lower_value(rhs);
                let result = self.lower_binop(*op, l, r);
                self.value_map.insert(*dst, result);
            }
            Instr::UnaryOp {
                dst,
                op,
                operand,
                hint: _,
            } => {
                let v = self.lower_value(operand);
                let result = self.lower_unop(*op, v);
                self.value_map.insert(*dst, result);
            }
            Instr::Call {
                dst,
                callee,
                args,
                hint,
            } => {
                let callee_fn = self.resolve_callee(callee, args.len());
                let llvm_args: Vec<BasicMetadataValueEnum> =
                    args.iter().map(|a| self.lower_value(a).into()).collect();
                let call_site = self
                    .builder
                    .build_call(callee_fn, &llvm_args, "call")
                    .expect("call");
                if let Some(d) = dst {
                    use inkwell::values::ValueKind;
                    match call_site.try_as_basic_value() {
                        ValueKind::Basic(rv) => {
                            self.value_map.insert(*d, rv);
                        }
                        ValueKind::Instruction(_) => {
                            // Function returned void but the IR
                            // expected a result — synthesize a zero
                            // of the right type so downstream uses
                            // don't panic.
                            let z = self.zero(*hint);
                            self.value_map.insert(*d, z);
                        }
                    }
                }
            }
            // Forms not yet emitted — tracked separately so subsequent
            // commits can switch them on incrementally. Emitting nothing
            // means uses of the result land at `Value::Const(Const::Null)`
            // in our IR (sema's WORD-fallback also covers most cases).
            _ => {}
        }
    }

    fn emit_terminator(&mut self, t: &Terminator) {
        match t {
            Terminator::Return(None) => {
                // Routines: BCPL convention is that they "return
                // WORD" — emit `ret i64 0` so the LLVM type matches
                // what the function signature declared.
                let i64_t = self.context.i64_type();
                let zero = i64_t.const_zero();
                self.builder
                    .build_return(Some(&zero))
                    .expect("ret routine");
            }
            Terminator::Return(Some(v)) => {
                let val = self.lower_value(v);
                self.builder.build_return(Some(&val)).expect("ret value");
            }
            Terminator::Branch(b) => {
                let target = self.block_map[b];
                self.builder
                    .build_unconditional_branch(target)
                    .expect("br");
            }
            Terminator::CondBranch {
                cond,
                then_block,
                else_block,
            } => {
                let c = self.lower_value(cond);
                let i1 = self.truthify(c);
                let then_bb = self.block_map[then_block];
                let else_bb = self.block_map[else_block];
                self.builder
                    .build_conditional_branch(i1, then_bb, else_bb)
                    .expect("br cond");
            }
            Terminator::Unreachable => {
                self.builder.build_unreachable().expect("unreachable");
            }
            // Switch terminator: deferred to the next emit chunk.
            Terminator::Switch { default, .. } => {
                let target = self.block_map[default];
                self.builder
                    .build_unconditional_branch(target)
                    .expect("switch placeholder");
            }
        }
    }

    // ─── value lowering ─────────────────────────────────────────

    fn lower_value(&mut self, v: &Value) -> BasicValueEnum<'ctx> {
        match v {
            Value::Const(c) => self.lower_const(c, TypeHint::Word),
            Value::Local(id) => self.lookup(*id),
            Value::Function(name) => self
                .by_name
                .get(name)
                .copied()
                .unwrap_or_else(|| self.declare_extern(name, 0))
                .as_global_value()
                .as_pointer_value()
                .into(),
            Value::Unit => self.context.i64_type().const_zero().into(),
        }
    }

    fn lower_const(&mut self, c: &Const, hint: TypeHint) -> BasicValueEnum<'ctx> {
        match c {
            Const::Int(v) => self.context.i64_type().const_int(*v as u64, true).into(),
            Const::Float(v) => self.context.f64_type().const_float(*v).into(),
            Const::Bool(b) => self
                .context
                .i64_type()
                .const_int(if *b { 1 } else { 0 }, false)
                .into(),
            Const::Null => {
                // Null → typed zero so subsequent uses don't crash.
                // Pointer-shaped hints get a null pointer; everything
                // else gets an integer zero.
                self.zero(hint)
            }
            Const::String(raw) => {
                // The lexeme arrives wrapped in quotes; codegen pool
                // stores the cooked bytes.
                let cooked = cook_bcpl_string(raw);
                self.intern_string(&cooked).into()
            }
        }
    }

    fn lookup(&self, id: ValueId) -> BasicValueEnum<'ctx> {
        *self
            .value_map
            .get(&id)
            .unwrap_or_else(|| panic!("undefined IR value {id:?}"))
    }

    fn resolve_callee(&mut self, callee: &Value, arg_count: usize) -> FunctionValue<'ctx> {
        match callee {
            Value::Function(name) => {
                if let Some(&fv) = self.by_name.get(name) {
                    fv
                } else {
                    self.declare_extern(name, arg_count)
                }
            }
            // Indirect calls (Local pointing at a function pointer)
            // are not yet supported — fall back to declaring a
            // placeholder extern so the module still verifies.
            _ => self.declare_extern("__newbcpl_indirect", arg_count),
        }
    }

    /// Convert a value to an i1 boolean for use in a CondBranch.
    /// Integer values get `value != 0`; float values get
    /// `value != 0.0`; pointers get `value != null`.
    fn truthify(&self, v: BasicValueEnum<'ctx>) -> inkwell::values::IntValue<'ctx> {
        match v {
            BasicValueEnum::IntValue(iv) => {
                let zero = iv.get_type().const_zero();
                self.builder
                    .build_int_compare(IntPredicate::NE, iv, zero, "tobool")
                    .expect("icmp")
            }
            BasicValueEnum::FloatValue(fv) => {
                let zero = fv.get_type().const_zero();
                self.builder
                    .build_float_compare(FloatPredicate::ONE, fv, zero, "tobool")
                    .expect("fcmp")
            }
            BasicValueEnum::PointerValue(pv) => {
                let null = pv.get_type().const_null();
                self.builder
                    .build_int_compare(IntPredicate::NE, pv, null, "tobool")
                    .expect("icmp ptr")
            }
            // Vectors / structs as branch conditions don't make sense
            // for BCPL; treat as true.
            _ => self.context.bool_type().const_int(1, false),
        }
    }

    /// Materialise a zero / null of the given hint's LLVM type.
    fn zero(&self, hint: TypeHint) -> BasicValueEnum<'ctx> {
        match self.basic_type_for(hint) {
            BasicTypeEnum::IntType(t) => t.const_zero().into(),
            BasicTypeEnum::FloatType(t) => t.const_zero().into(),
            BasicTypeEnum::PointerType(t) => t.const_null().into(),
            BasicTypeEnum::VectorType(t) => t.const_zero().into(),
            other => panic!("no zero for type {:?}", other.print_to_string()),
        }
    }

    fn intern_string(&mut self, cooked: &str) -> PointerValue<'ctx> {
        if let Some(&p) = self.string_pool.get(cooked) {
            return p;
        }
        let global_name = format!(".str.{}", self.string_counter);
        self.string_counter += 1;
        let v = self
            .builder
            .build_global_string_ptr(cooked, &global_name)
            .expect("global string")
            .as_pointer_value();
        self.string_pool.insert(cooked.to_string(), v);
        v
    }

    // ─── binop / unop dispatch ──────────────────────────────────

    fn lower_binop(
        &self,
        op: IrBinOp,
        lhs: BasicValueEnum<'ctx>,
        rhs: BasicValueEnum<'ctx>,
    ) -> BasicValueEnum<'ctx> {
        // Integer ops dispatch on int operand variants; float ops on
        // float operands. The IR has already chosen the family via
        // sema's hints, so each branch is pure mapping.
        match op {
            IrBinOp::IAdd => self
                .builder
                .build_int_add(lhs.into_int_value(), rhs.into_int_value(), "iadd")
                .unwrap()
                .into(),
            IrBinOp::ISub => self
                .builder
                .build_int_sub(lhs.into_int_value(), rhs.into_int_value(), "isub")
                .unwrap()
                .into(),
            IrBinOp::IMul => self
                .builder
                .build_int_mul(lhs.into_int_value(), rhs.into_int_value(), "imul")
                .unwrap()
                .into(),
            IrBinOp::IDiv => self
                .builder
                .build_int_signed_div(lhs.into_int_value(), rhs.into_int_value(), "idiv")
                .unwrap()
                .into(),
            IrBinOp::IRem => self
                .builder
                .build_int_signed_rem(lhs.into_int_value(), rhs.into_int_value(), "irem")
                .unwrap()
                .into(),
            IrBinOp::FAdd => self
                .builder
                .build_float_add(lhs.into_float_value(), rhs.into_float_value(), "fadd")
                .unwrap()
                .into(),
            IrBinOp::FSub => self
                .builder
                .build_float_sub(lhs.into_float_value(), rhs.into_float_value(), "fsub")
                .unwrap()
                .into(),
            IrBinOp::FMul => self
                .builder
                .build_float_mul(lhs.into_float_value(), rhs.into_float_value(), "fmul")
                .unwrap()
                .into(),
            IrBinOp::FDiv => self
                .builder
                .build_float_div(lhs.into_float_value(), rhs.into_float_value(), "fdiv")
                .unwrap()
                .into(),
            IrBinOp::BitAnd => self
                .builder
                .build_and(lhs.into_int_value(), rhs.into_int_value(), "and")
                .unwrap()
                .into(),
            IrBinOp::BitOr => self
                .builder
                .build_or(lhs.into_int_value(), rhs.into_int_value(), "or")
                .unwrap()
                .into(),
            IrBinOp::BitXor => self
                .builder
                .build_xor(lhs.into_int_value(), rhs.into_int_value(), "xor")
                .unwrap()
                .into(),
            IrBinOp::Shl => self
                .builder
                .build_left_shift(lhs.into_int_value(), rhs.into_int_value(), "shl")
                .unwrap()
                .into(),
            IrBinOp::Shr => self
                .builder
                .build_right_shift(
                    lhs.into_int_value(),
                    rhs.into_int_value(),
                    /* sign_extend = */ true,
                    "shr",
                )
                .unwrap()
                .into(),
            IrBinOp::ICmpEq
            | IrBinOp::ICmpNe
            | IrBinOp::ICmpLt
            | IrBinOp::ICmpLe
            | IrBinOp::ICmpGt
            | IrBinOp::ICmpGe => {
                let pred = match op {
                    IrBinOp::ICmpEq => IntPredicate::EQ,
                    IrBinOp::ICmpNe => IntPredicate::NE,
                    IrBinOp::ICmpLt => IntPredicate::SLT,
                    IrBinOp::ICmpLe => IntPredicate::SLE,
                    IrBinOp::ICmpGt => IntPredicate::SGT,
                    IrBinOp::ICmpGe => IntPredicate::SGE,
                    _ => unreachable!(),
                };
                let bit = self
                    .builder
                    .build_int_compare(pred, lhs.into_int_value(), rhs.into_int_value(), "icmp")
                    .unwrap();
                // BCPL relational ops produce a WORD (0 or 1), not
                // an i1 — zero-extend so the result fits the rest of
                // the integer arithmetic.
                self.builder
                    .build_int_z_extend(bit, self.context.i64_type(), "zext")
                    .unwrap()
                    .into()
            }
            IrBinOp::FCmpEq
            | IrBinOp::FCmpNe
            | IrBinOp::FCmpLt
            | IrBinOp::FCmpLe
            | IrBinOp::FCmpGt
            | IrBinOp::FCmpGe => {
                let pred = match op {
                    IrBinOp::FCmpEq => FloatPredicate::OEQ,
                    IrBinOp::FCmpNe => FloatPredicate::ONE,
                    IrBinOp::FCmpLt => FloatPredicate::OLT,
                    IrBinOp::FCmpLe => FloatPredicate::OLE,
                    IrBinOp::FCmpGt => FloatPredicate::OGT,
                    IrBinOp::FCmpGe => FloatPredicate::OGE,
                    _ => unreachable!(),
                };
                let bit = self
                    .builder
                    .build_float_compare(
                        pred,
                        lhs.into_float_value(),
                        rhs.into_float_value(),
                        "fcmp",
                    )
                    .unwrap();
                self.builder
                    .build_int_z_extend(bit, self.context.i64_type(), "zext")
                    .unwrap()
                    .into()
            }
        }
    }

    fn lower_unop(&self, op: IrUnOp, operand: BasicValueEnum<'ctx>) -> BasicValueEnum<'ctx> {
        match op {
            IrUnOp::INeg => self
                .builder
                .build_int_neg(operand.into_int_value(), "ineg")
                .unwrap()
                .into(),
            IrUnOp::FNeg => self
                .builder
                .build_float_neg(operand.into_float_value(), "fneg")
                .unwrap()
                .into(),
            IrUnOp::Not => self
                .builder
                .build_not(operand.into_int_value(), "not")
                .unwrap()
                .into(),
        }
    }

    // ─── type mapping ───────────────────────────────────────────

    fn basic_type_for(&self, hint: TypeHint) -> BasicTypeEnum<'ctx> {
        match hint {
            TypeHint::Word | TypeHint::Int | TypeHint::Unknown => {
                self.context.i64_type().into()
            }
            TypeHint::Float => self.context.f64_type().into(),
            TypeHint::String
            | TypeHint::Object
            | TypeHint::List
            | TypeHint::Vec
            | TypeHint::FVec
            | TypeHint::Function
            | TypeHint::Null => self.context.ptr_type(AddressSpace::default()).into(),
            TypeHint::Pair => self.context.i64_type().vec_type(2).into(),
            TypeHint::FPair => self.context.f64_type().vec_type(2).into(),
            TypeHint::Quad => self.context.i64_type().vec_type(4).into(),
            TypeHint::FQuad => self.context.f64_type().vec_type(4).into(),
            TypeHint::Oct => self.context.i64_type().vec_type(8).into(),
            TypeHint::FOct => self.context.f64_type().vec_type(8).into(),
        }
    }

    /// `None` means the function returns void (used only when the
    /// inferred return hint is itself unmaterialisable). Right now
    /// every BCPL routine returns WORD by convention, so this is
    /// always `Some`.
    fn return_type_for(&self, hint: TypeHint) -> Option<BasicTypeEnum<'ctx>> {
        Some(self.basic_type_for(hint))
    }
}

/// Strip a BCPL string lexeme's outer quotes and decode the dialect's
/// `*`-prefix escape sequences (`*N` → newline, `*T` → tab, `*S` →
/// space, `**` → `*`, `*"` → `"`, etc.).
pub(crate) fn cook_bcpl_string(raw: &str) -> String {
    let bytes = raw.as_bytes();
    if bytes.len() < 2 || bytes[0] != b'"' || bytes[bytes.len() - 1] != b'"' {
        return raw.to_string();
    }
    let inner = &raw[1..raw.len() - 1];
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '*' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') | Some('N') => out.push('\n'),
            Some('t') | Some('T') => out.push('\t'),
            Some('s') | Some('S') => out.push(' '),
            Some('b') | Some('B') => out.push('\u{08}'),
            Some('p') | Some('P') => out.push('\u{0C}'),
            Some('c') | Some('C') => out.push('\r'),
            Some('"') => out.push('"'),
            Some('*') => out.push('*'),
            Some(other) => {
                // Unknown escape — keep the `*` and the following
                // char verbatim so the diagnostic stays visible.
                out.push('*');
                out.push(other);
            }
            None => out.push('*'),
        }
    }
    out
}
