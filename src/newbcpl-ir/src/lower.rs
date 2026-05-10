//! AST → IR lowering.
//!
//! Bootstrap subset: top-level `LET R(...) BE stmt` routines and
//! `LET F(...) = expr` functions; `LET` locals; integer / float
//! literals; arithmetic and relational binary operators; unary
//! `-` and `~`; routine and function calls; `IF / ELSE`. Everything
//! else (loops, classes, lists, SIMD, member access, ...) lowers to
//! a `(?)` placeholder load that codegen will skip — sema warnings
//! already fired for unsupported forms.
//!
//! Locals use the standard `alloca` + `load`/`store` pattern so the
//! IR stays free of phi nodes; LLVM mem2reg promotes the slots to
//! registers.

use std::collections::HashMap;

use newbcpl_parser::{
    BinaryOp, Block, Decl, Expr, FunctionDecl, LetDecl, Program, RoutineDecl, Stmt, UnaryOp,
};
use newbcpl_sema::{SemaOutput, TypeHint};

use crate::ir::*;

/// Lower a typed AST plus its sema output into an IR module. The
/// caller must have run `newbcpl_sema::analyze(&program)` first so
/// expressions carry their hints.
pub fn lower(program: &Program, sema: &SemaOutput, module_name: &str) -> Module {
    let mut lowerer = Lowerer::new();
    for decl in &program.items {
        match decl {
            Decl::Routine(r) => lowerer.lower_routine(r),
            Decl::Function(f) => lowerer.lower_function(f),
            // Top-level decls that don't produce IR functions yet
            // (GET / MANIFEST / STATIC / GLOBAL / CLASS) are simply
            // skipped here; class layouts come from sema.
            _ => {}
        }
    }
    Module {
        name: module_name.to_string(),
        functions: lowerer.functions,
        layouts: sema.layouts.clone(),
    }
}

#[derive(Default)]
struct Lowerer {
    functions: Vec<Function>,
    current: Option<Builder>,
}

/// Per-function state during lowering.
struct Builder {
    function: Function,
    next_value: u32,
    next_block: u32,
    /// Block currently receiving instructions. When we terminate it
    /// and switch to a new one, this updates.
    current_block: BlockId,
    /// Lexical scope stack: name → slot ValueId.
    scopes: Vec<HashMap<String, ValueId>>,
}

impl Builder {
    fn new(name: &str) -> Self {
        let entry = BlockId(0);
        let function = Function {
            name: name.to_string(),
            params: Vec::new(),
            return_hint: TypeHint::Word,
            blocks: vec![BasicBlock {
                id: entry,
                label: "entry".to_string(),
                instrs: Vec::new(),
                terminator: Terminator::Unreachable,
            }],
            entry,
        };
        Self {
            function,
            next_value: 0,
            next_block: 1,
            current_block: entry,
            scopes: vec![HashMap::new()],
        }
    }

    fn alloc_value(&mut self) -> ValueId {
        let id = ValueId(self.next_value);
        self.next_value += 1;
        id
    }

    fn alloc_block(&mut self, label: &str) -> BlockId {
        let id = BlockId(self.next_block);
        self.next_block += 1;
        self.function.blocks.push(BasicBlock {
            id,
            label: label.to_string(),
            instrs: Vec::new(),
            terminator: Terminator::Unreachable,
        });
        id
    }

    fn emit(&mut self, instr: Instr) {
        let block = self
            .function
            .blocks
            .iter_mut()
            .find(|b| b.id == self.current_block)
            .expect("current block must exist");
        block.instrs.push(instr);
    }

    fn terminate(&mut self, t: Terminator) {
        let block = self
            .function
            .blocks
            .iter_mut()
            .find(|b| b.id == self.current_block)
            .expect("current block must exist");
        block.terminator = t;
    }

    fn switch_to(&mut self, block: BlockId) {
        self.current_block = block;
    }

    fn push_scope(&mut self) {
        self.scopes.push(HashMap::new());
    }

    fn pop_scope(&mut self) {
        self.scopes.pop();
        if self.scopes.is_empty() {
            self.scopes.push(HashMap::new());
        }
    }

    fn declare_local(&mut self, name: &str, slot: ValueId) {
        if let Some(top) = self.scopes.last_mut() {
            top.insert(name.to_string(), slot);
        }
    }

    fn lookup_local(&self, name: &str) -> Option<ValueId> {
        for frame in self.scopes.iter().rev() {
            if let Some(&slot) = frame.get(name) {
                return Some(slot);
            }
        }
        None
    }

    fn alloca(&mut self, name: &str, hint: TypeHint) -> ValueId {
        let slot = self.alloc_value();
        self.emit(Instr::Alloca {
            dst: slot,
            hint,
            name: name.to_string(),
        });
        slot
    }
}

impl Lowerer {
    fn new() -> Self {
        Self::default()
    }

    fn b(&mut self) -> &mut Builder {
        self.current.as_mut().expect("no current function")
    }

    fn lower_routine(&mut self, r: &RoutineDecl) {
        self.start_function(&r.name, &r.params, TypeHint::Word);
        self.lower_stmt(&r.body);
        // If the body fell through without an explicit RETURN, emit
        // one for routines (no return value).
        let cur = self.b().current_block;
        let unterminated = self
            .b()
            .function
            .blocks
            .iter()
            .find(|b| b.id == cur)
            .is_some_and(|b| matches!(b.terminator, Terminator::Unreachable));
        if unterminated {
            self.b().terminate(Terminator::Return(None));
        }
        self.finish_function();
    }

    fn lower_function(&mut self, f: &FunctionDecl) {
        let return_hint = f.body.hint();
        self.start_function(&f.name, &f.params, return_hint);
        let value = self.lower_expr(&f.body);
        self.b().terminate(Terminator::Return(Some(value)));
        self.finish_function();
    }

    fn start_function(&mut self, name: &str, params: &[String], return_hint: TypeHint) {
        let mut b = Builder::new(name);
        b.function.return_hint = return_hint;
        // Allocate parameter slots in the entry block. Each parameter
        // gets a stack slot the body sees through Load/Store, plus
        // an `in_value` representing the incoming SSA value (which
        // codegen materialises from the calling convention).
        for p in params {
            let in_value = b.alloc_value();
            let slot = b.alloca(p, TypeHint::Word);
            b.emit(Instr::Store {
                slot,
                value: Value::Local(in_value),
            });
            b.declare_local(p, slot);
            b.function.params.push(Param {
                name: p.clone(),
                hint: TypeHint::Word,
                slot,
                in_value,
            });
        }
        self.current = Some(b);
    }

    fn finish_function(&mut self) {
        if let Some(b) = self.current.take() {
            self.functions.push(b.function);
        }
    }

    // ─── statements ─────────────────────────────────────────────

    fn lower_stmt(&mut self, s: &Stmt) {
        match s {
            Stmt::Block(block) => self.lower_block(block),
            Stmt::Decl(Decl::Let(l)) => self.lower_let_stmt(l),
            Stmt::Decl(_) => {
                // GET / STATIC / GLOBAL / MANIFEST / CLASS inside a
                // routine body — observed by sema, no IR effect.
            }
            Stmt::Expr(e) => {
                let _ = self.lower_expr(e);
            }
            Stmt::Assign { targets, values, .. } => self.lower_assign(targets, values),
            Stmt::If {
                cond,
                then_stmt,
                else_stmt,
                ..
            } => self.lower_if(cond, then_stmt, else_stmt.as_deref()),
            Stmt::Unless {
                cond, then_stmt, ..
            } => {
                // UNLESS C THEN S  ≡  IF NOT C THEN S
                self.lower_unless(cond, then_stmt);
            }
            Stmt::Return(_) => {
                let return_hint = self.b().function.return_hint;
                let ret_value = if return_hint == TypeHint::Word {
                    None
                } else {
                    // Bare RETURN in a function body — unspecified
                    // value. Treat as null/zero.
                    Some(Value::Const(Const::Int(0)))
                };
                self.b().terminate(Terminator::Return(ret_value));
                let dead = self.b().alloc_block("after.return");
                self.b().switch_to(dead);
            }
            Stmt::Resultis(expr, _) => {
                let value = self.lower_expr(expr);
                self.b().terminate(Terminator::Return(Some(value)));
                let dead = self.b().alloc_block("after.resultis");
                self.b().switch_to(dead);
            }
            Stmt::Finish(_) => {
                self.b().terminate(Terminator::Return(None));
                let dead = self.b().alloc_block("after.finish");
                self.b().switch_to(dead);
            }
            // Loops / SWITCHON / FOREACH / labels / RETAIN / etc.
            // are not yet lowered — they fall through and are simply
            // observed but produce no IR. Subsequent IR-grow chunks
            // pick them up.
            _ => {}
        }
    }

    fn lower_block(&mut self, block: &Block) {
        self.b().push_scope();
        for s in &block.stmts {
            self.lower_stmt(s);
        }
        self.b().pop_scope();
    }

    fn lower_let_stmt(&mut self, l: &LetDecl) {
        for (name, init) in &l.bindings {
            let value = self.lower_expr(init);
            let slot = self.b().alloca(name, init.hint());
            self.b().emit(Instr::Store { slot, value });
            self.b().declare_local(name, slot);
        }
    }

    fn lower_assign(&mut self, targets: &[Expr], values: &[Expr]) {
        for (target, value) in targets.iter().zip(values.iter()) {
            let v = self.lower_expr(value);
            // Only simple-name lvalues are lowered here; subscripts,
            // member access, and indirection fall through (sema has
            // already type-checked them, codegen handles them later).
            if let Expr::Ident { name, .. } = target {
                if let Some(slot) = self.b().lookup_local(name) {
                    self.b().emit(Instr::Store { slot, value: v });
                }
            }
        }
    }

    fn lower_if(&mut self, cond: &Expr, then_stmt: &Stmt, else_stmt: Option<&Stmt>) {
        let cond_value = self.lower_expr(cond);
        let then_block = self.b().alloc_block("if.then");
        let else_block = self.b().alloc_block("if.else");
        let merge = self.b().alloc_block("if.end");

        self.b().terminate(Terminator::CondBranch {
            cond: cond_value,
            then_block,
            else_block,
        });

        self.b().switch_to(then_block);
        self.lower_stmt(then_stmt);
        // If the then-branch fell through without terminating, jump
        // to the merge block.
        let cur = self.b().current_block;
        let needs_merge = self
            .b()
            .function
            .blocks
            .iter()
            .find(|b| b.id == cur)
            .is_some_and(|b| matches!(b.terminator, Terminator::Unreachable));
        if needs_merge {
            self.b().terminate(Terminator::Branch(merge));
        }

        self.b().switch_to(else_block);
        if let Some(els) = else_stmt {
            self.lower_stmt(els);
        }
        let cur = self.b().current_block;
        let needs_merge = self
            .b()
            .function
            .blocks
            .iter()
            .find(|b| b.id == cur)
            .is_some_and(|b| matches!(b.terminator, Terminator::Unreachable));
        if needs_merge {
            self.b().terminate(Terminator::Branch(merge));
        }

        self.b().switch_to(merge);
    }

    fn lower_unless(&mut self, cond: &Expr, then_stmt: &Stmt) {
        let cond_value = self.lower_expr(cond);
        // Negate: !cond goes to the body; cond goes to merge.
        let dst = self.b().alloc_value();
        self.b().emit(Instr::UnaryOp {
            dst,
            op: IrUnOp::Not,
            operand: cond_value,
            hint: TypeHint::Int,
        });
        let body_block = self.b().alloc_block("unless.body");
        let merge = self.b().alloc_block("unless.end");
        self.b().terminate(Terminator::CondBranch {
            cond: Value::Local(dst),
            then_block: body_block,
            else_block: merge,
        });
        self.b().switch_to(body_block);
        self.lower_stmt(then_stmt);
        let cur = self.b().current_block;
        let needs_merge = self
            .b()
            .function
            .blocks
            .iter()
            .find(|b| b.id == cur)
            .is_some_and(|b| matches!(b.terminator, Terminator::Unreachable));
        if needs_merge {
            self.b().terminate(Terminator::Branch(merge));
        }
        self.b().switch_to(merge);
    }

    // ─── expressions ────────────────────────────────────────────

    fn lower_expr(&mut self, e: &Expr) -> Value {
        match e {
            Expr::IntLit { value, .. } => Value::Const(Const::Int(*value)),
            Expr::FloatLit { value, .. } => Value::Const(Const::Float(*value)),
            Expr::BoolLit { value, .. } => Value::Const(Const::Bool(*value)),
            Expr::StringLit { value, .. } => Value::Const(Const::String(value.clone())),
            Expr::Null { .. } => Value::Const(Const::Null),
            Expr::CharLit { lexeme, .. } => {
                // For now, keep char literals as their lexeme via the
                // string-table; codegen can decode the BCPL escape.
                Value::Const(Const::String(lexeme.clone()))
            }
            Expr::Ident { name, .. } => self.lower_ident(name, e.hint()),
            Expr::Binary { op, lhs, rhs, .. } => {
                self.lower_binary(*op, lhs, rhs, e.hint())
            }
            Expr::Unary { op, operand, .. } => self.lower_unary(*op, operand, e.hint()),
            Expr::Call { callee, args, .. } => self.lower_call(callee, args, e.hint()),
            Expr::Conditional {
                cond,
                then_expr,
                else_expr,
                ..
            } => self.lower_conditional(cond, then_expr, else_expr, e.hint()),
            // Forms not yet lowered — return a typed null/zero so
            // downstream uses don't crash. Sema warnings already
            // fired for whatever real handling these need.
            _ => Value::Const(Const::Null),
        }
    }

    fn lower_ident(&mut self, name: &str, hint: TypeHint) -> Value {
        if let Some(slot) = self.b().lookup_local(name) {
            let dst = self.b().alloc_value();
            self.b().emit(Instr::Load { dst, slot, hint });
            Value::Local(dst)
        } else {
            // Unknown name — assume it's a function reference that
            // will be resolved at link time.
            Value::Function(name.to_string())
        }
    }

    fn lower_binary(&mut self, op: BinaryOp, lhs: &Expr, rhs: &Expr, hint: TypeHint) -> Value {
        let lhs_v = self.lower_expr(lhs);
        let rhs_v = self.lower_expr(rhs);
        let lhs_h = lhs.hint();
        let rhs_h = rhs.hint();
        let Some(ir_op) = binop_to_ir(op, lhs_h, rhs_h) else {
            // Subscript family, member access, lane access etc. are
            // not yet IR-lowered; fall through to a placeholder.
            return Value::Const(Const::Null);
        };
        let dst = self.b().alloc_value();
        self.b().emit(Instr::BinOp {
            dst,
            op: ir_op,
            lhs: lhs_v,
            rhs: rhs_v,
            hint,
        });
        Value::Local(dst)
    }

    fn lower_unary(&mut self, op: UnaryOp, operand: &Expr, hint: TypeHint) -> Value {
        let v = self.lower_expr(operand);
        let Some(ir_op) = unop_to_ir(op, operand.hint()) else {
            return Value::Const(Const::Null);
        };
        let dst = self.b().alloc_value();
        self.b().emit(Instr::UnaryOp {
            dst,
            op: ir_op,
            operand: v,
            hint,
        });
        Value::Local(dst)
    }

    fn lower_call(&mut self, callee: &Expr, args: &[Expr], hint: TypeHint) -> Value {
        let callee_v = self.lower_expr(callee);
        let arg_values: Vec<Value> = args.iter().map(|a| self.lower_expr(a)).collect();
        let dst = if hint == TypeHint::Word {
            // Routine-shape call: discard the result. Codegen still
            // emits a call instruction, just doesn't bind a result.
            None
        } else {
            Some(self.b().alloc_value())
        };
        self.b().emit(Instr::Call {
            dst,
            callee: callee_v,
            args: arg_values,
            hint,
        });
        match dst {
            Some(d) => Value::Local(d),
            None => Value::Unit,
        }
    }

    fn lower_conditional(
        &mut self,
        cond: &Expr,
        then_expr: &Expr,
        else_expr: &Expr,
        hint: TypeHint,
    ) -> Value {
        // `cond -> a, b` lowers to: alloca a slot, branch on cond,
        // each arm computes its expression and stores into the slot,
        // then the merge block loads the slot.
        let slot = self.b().alloca("cond.tmp", hint);
        let cond_v = self.lower_expr(cond);
        let then_block = self.b().alloc_block("cond.then");
        let else_block = self.b().alloc_block("cond.else");
        let merge = self.b().alloc_block("cond.end");
        self.b().terminate(Terminator::CondBranch {
            cond: cond_v,
            then_block,
            else_block,
        });
        self.b().switch_to(then_block);
        let then_v = self.lower_expr(then_expr);
        self.b().emit(Instr::Store {
            slot,
            value: then_v,
        });
        self.b().terminate(Terminator::Branch(merge));
        self.b().switch_to(else_block);
        let else_v = self.lower_expr(else_expr);
        self.b().emit(Instr::Store {
            slot,
            value: else_v,
        });
        self.b().terminate(Terminator::Branch(merge));
        self.b().switch_to(merge);
        let dst = self.b().alloc_value();
        self.b().emit(Instr::Load { dst, slot, hint });
        Value::Local(dst)
    }
}

/// Map an AST `BinaryOp` plus its operand hints to the corresponding
/// IR `IrBinOp`. Returns `None` for ops the IR doesn't yet implement
/// (subscript family, member access, lane access).
fn binop_to_ir(op: BinaryOp, lhs: TypeHint, rhs: TypeHint) -> Option<IrBinOp> {
    use BinaryOp::*;
    let both_float = lhs.is_float_family() && rhs.is_float_family();
    Some(match op {
        Add => {
            if both_float {
                IrBinOp::FAdd
            } else {
                IrBinOp::IAdd
            }
        }
        Sub => {
            if both_float {
                IrBinOp::FSub
            } else {
                IrBinOp::ISub
            }
        }
        Mul => {
            if both_float {
                IrBinOp::FMul
            } else {
                IrBinOp::IMul
            }
        }
        Div => {
            if both_float {
                IrBinOp::FDiv
            } else {
                IrBinOp::IDiv
            }
        }
        Rem => IrBinOp::IRem,
        FAdd => IrBinOp::FAdd,
        FSub => IrBinOp::FSub,
        FMul => IrBinOp::FMul,
        FDiv => IrBinOp::FDiv,
        Eq => {
            if both_float {
                IrBinOp::FCmpEq
            } else {
                IrBinOp::ICmpEq
            }
        }
        Ne => {
            if both_float {
                IrBinOp::FCmpNe
            } else {
                IrBinOp::ICmpNe
            }
        }
        Lt => {
            if both_float {
                IrBinOp::FCmpLt
            } else {
                IrBinOp::ICmpLt
            }
        }
        Le => {
            if both_float {
                IrBinOp::FCmpLe
            } else {
                IrBinOp::ICmpLe
            }
        }
        Gt => {
            if both_float {
                IrBinOp::FCmpGt
            } else {
                IrBinOp::ICmpGt
            }
        }
        Ge => {
            if both_float {
                IrBinOp::FCmpGe
            } else {
                IrBinOp::ICmpGe
            }
        }
        FEq => IrBinOp::FCmpEq,
        FNe => IrBinOp::FCmpNe,
        FLt => IrBinOp::FCmpLt,
        FLe => IrBinOp::FCmpLe,
        FGt => IrBinOp::FCmpGt,
        FGe => IrBinOp::FCmpGe,
        BitAnd => IrBinOp::BitAnd,
        BitOr => IrBinOp::BitOr,
        Eqv => IrBinOp::ICmpEq, // EQV is "is the same boolean" → equality
        Neqv => IrBinOp::BitXor, // NEQV is XOR (parity-style)
        Shl => IrBinOp::Shl,
        Shr => IrBinOp::Shr,
        // Subscript family + member access aren't lowered yet.
        Subscript | Bitfield | CharSubscript | FloatSubscript | Dot | Of | LaneAccess => {
            return None;
        }
    })
}

fn unop_to_ir(op: UnaryOp, operand: TypeHint) -> Option<IrUnOp> {
    Some(match op {
        UnaryOp::Neg => {
            if operand.is_float_family() {
                IrUnOp::FNeg
            } else {
                IrUnOp::INeg
            }
        }
        UnaryOp::Not => IrUnOp::Not,
        // Indirection / AddressOf / CharIndirection / Hd / Tl / Rest /
        // Len / FreeVec / FreeList aren't lowered yet.
        _ => return None,
    })
}

