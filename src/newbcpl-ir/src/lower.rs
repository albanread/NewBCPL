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
    BinaryOp, Block, ClassDecl, ClassMemberKind, ClassMethod, ClassMethodBody, Decl, Expr,
    FunctionDecl, LetDecl, Program, RoutineDecl, Stmt, SwitchCase, TypeConstructorKind, UnaryOp,
};
use newbcpl_sema::{ClassLayout, SemaOutput, TypeHint};

use crate::ir::*;

/// Lower a typed AST plus its sema output into an IR module. The
/// caller must have run `newbcpl_sema::analyze(&program)` first so
/// expressions carry their hints.
pub fn lower(program: &Program, sema: &SemaOutput, module_name: &str) -> Module {
    let mut lowerer = Lowerer::new(&sema.layouts, &sema.manifests);
    for decl in &program.items {
        match decl {
            Decl::Routine(r) => lowerer.lower_routine(r),
            Decl::Function(f) => lowerer.lower_function(f),
            Decl::Class(c) => lowerer.lower_class(c),
            // Top-level decls that don't produce IR functions
            // (GET / MANIFEST / STATIC / GLOBAL) are skipped.
            _ => {}
        }
    }
    Module {
        name: module_name.to_string(),
        functions: lowerer.functions,
        layouts: sema.layouts.clone(),
    }
}

struct Lowerer<'a> {
    functions: Vec<Function>,
    current: Option<Builder>,
    layouts: &'a [ClassLayout],
    /// `MANIFEST` constants from sema. Lookup in `lower_ident` for
    /// inline substitution — the BCPL convention treats a MANIFEST
    /// as a compile-time integer, not a runtime binding.
    manifests: &'a std::collections::HashMap<String, i64>,
    /// Set while lowering a class method body. Allows bare-field
    /// identifiers (`x` inside `Point.set`) to resolve as
    /// SELF-relative field accesses, and lets `class_name_of_expr`
    /// recognise SELF and SUPER as having the surrounding class.
    current_class: Option<String>,
}

/// Per-function state during lowering.
struct Builder {
    function: Function,
    next_value: u32,
    next_block: u32,
    /// Block currently receiving instructions. When we terminate it
    /// and switch to a new one, this updates.
    current_block: BlockId,
    /// Lexical scope stack: name → (slot, optional class name).
    /// The class name lets `obj.field` and `obj.method(...)` resolve
    /// through the layout / vtable tables.
    scopes: Vec<HashMap<String, LocalInfo>>,
    /// Stack of currently-active control-flow frames so `BREAK` /
    /// `LOOP` / `ENDCASE` know which scope to target. WHILE / UNTIL /
    /// FOR / REPEAT push a frame with `continue_block` set; SWITCHON
    /// pushes one with `continue_block` = None (since `LOOP` skips
    /// past it to the enclosing loop). The innermost frame is the
    /// BREAK / ENDCASE target; the innermost *loop* frame is the
    /// LOOP target.
    frames: Vec<Frame>,
    /// Source-level labels — `name:` declarations and `GOTO name`
    /// references — share blocks. Forward references work because
    /// `label_block` creates the block on first mention; the
    /// declaration site just terminates the current block with a
    /// branch to it and switches in.
    labels: HashMap<String, BlockId>,
    /// Stack of currently-open VALOF expressions. Each frame
    /// records the slot RESULTIS stores into and the block it
    /// branches to on exit. `RESULTIS expr` inside a VALOF stores
    /// to the innermost frame and branches; outside, it returns
    /// from the function (legacy fallback — sema already warns).
    valofs: Vec<ValofFrame>,
}

#[derive(Debug, Clone, Copy)]
struct ValofFrame {
    result_slot: ValueId,
    exit_block: BlockId,
}

#[derive(Debug, Clone)]
struct LocalInfo {
    slot: ValueId,
    /// Class name if this binding holds an OBJECT instance, so
    /// member access can route through the class layout. Set when
    /// the LET initialiser is `NEW Foo()` or another Ident whose
    /// own class is known.
    class_name: Option<String>,
}

#[derive(Debug, Clone, Copy)]
struct Frame {
    /// `BREAK` (and `ENDCASE` inside a SWITCHON) jumps here.
    break_block: BlockId,
    /// `LOOP` jumps here. For WHILE / UNTIL / FOR this is the
    /// header block; for REPEAT-family loops it's the body block
    /// (so `LOOP` re-enters at the start of the iteration). `None`
    /// for SWITCHON frames — `LOOP` walks past to the enclosing
    /// loop (or sema has already warned).
    continue_block: Option<BlockId>,
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
            frames: Vec::new(),
            labels: HashMap::new(),
            valofs: Vec::new(),
        }
    }

    /// Look up or allocate the block for a source-level label.
    /// Forward references work transparently — the block is reserved
    /// the first time it's mentioned, by either declaration or GOTO.
    fn label_block(&mut self, name: &str) -> BlockId {
        if let Some(&id) = self.labels.get(name) {
            return id;
        }
        let id = self.alloc_block(&format!("label.{name}"));
        self.labels.insert(name.to_string(), id);
        id
    }

    fn innermost_break(&self) -> Option<BlockId> {
        self.frames.last().map(|f| f.break_block)
    }

    fn innermost_continue(&self) -> Option<BlockId> {
        self.frames
            .iter()
            .rev()
            .find_map(|f| f.continue_block)
    }

    fn current_block_terminator(&self) -> Option<&Terminator> {
        self.function
            .blocks
            .iter()
            .find(|b| b.id == self.current_block)
            .map(|b| &b.terminator)
    }

    /// Whether the current block has an Unreachable placeholder
    /// terminator — i.e. nothing has terminated it yet.
    fn current_open(&self) -> bool {
        matches!(self.current_block_terminator(), Some(Terminator::Unreachable))
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

    fn declare_local(&mut self, name: &str, slot: ValueId, class_name: Option<String>) {
        if let Some(top) = self.scopes.last_mut() {
            top.insert(name.to_string(), LocalInfo { slot, class_name });
        }
    }

    fn lookup_local(&self, name: &str) -> Option<&LocalInfo> {
        for frame in self.scopes.iter().rev() {
            if let Some(info) = frame.get(name) {
                return Some(info);
            }
        }
        None
    }

    fn lookup_local_slot(&self, name: &str) -> Option<ValueId> {
        self.lookup_local(name).map(|i| i.slot)
    }

    fn lookup_local_class(&self, name: &str) -> Option<String> {
        self.lookup_local(name).and_then(|i| i.class_name.clone())
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

impl<'a> Lowerer<'a> {
    fn new(
        layouts: &'a [ClassLayout],
        manifests: &'a std::collections::HashMap<String, i64>,
    ) -> Self {
        Self {
            functions: Vec::new(),
            current: None,
            layouts,
            manifests,
            current_class: None,
        }
    }

    fn b(&mut self) -> &mut Builder {
        self.current.as_mut().expect("no current function")
    }

    fn lower_routine(&mut self, r: &RoutineDecl) {
        self.start_function(&r.name, &r.params, TypeHint::Word);
        self.lower_stmt(&r.body);
        // If the body fell through without an explicit RETURN, emit
        // one for routines (no return value).
        if self.b().current_open() {
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

    /// Walk a `CLASS` declaration and emit each method as a regular
    /// IR function with name `{class}_{method}`. The implicit
    /// receiver `SELF` becomes the first parameter (typed OBJECT).
    /// Field initialisers inside the class body are not yet
    /// lowered as IR — the layout pass already records them; CREATE
    /// is responsible for explicit initialisation.
    fn lower_class(&mut self, c: &ClassDecl) {
        for member in &c.members {
            if let ClassMemberKind::Method(m) = &member.kind {
                self.lower_method(&c.name, m);
            }
        }
    }

    fn lower_method(&mut self, class_name: &str, m: &ClassMethod) {
        let mangled = mangle_method(class_name, &m.name);
        // Build the method's parameter list with SELF as the first
        // implicit param. Real BCPL params follow.
        let mut params: Vec<String> = Vec::with_capacity(m.params.len() + 1);
        params.push("SELF".to_string());
        params.extend(m.params.iter().cloned());

        let return_hint = match &m.body {
            ClassMethodBody::Routine(_) => TypeHint::Word,
            ClassMethodBody::Function(e) => e.hint(),
        };
        self.start_function(&mangled, &params, return_hint);

        // Tag the SELF binding with the current class so member
        // access through SELF resolves to the right field offsets.
        if let Some(b) = self.current.as_mut() {
            if let Some(info) = b
                .scopes
                .last_mut()
                .and_then(|s| s.get_mut("SELF"))
            {
                info.class_name = Some(class_name.to_string());
            }
        }
        self.current_class = Some(class_name.to_string());

        match &m.body {
            ClassMethodBody::Routine(stmt) => {
                self.lower_stmt(stmt);
                if self.b().current_open() {
                    self.b().terminate(Terminator::Return(None));
                }
            }
            ClassMethodBody::Function(expr) => {
                let v = self.lower_expr(expr);
                self.b().terminate(Terminator::Return(Some(v)));
            }
        }

        self.current_class = None;
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
            b.declare_local(p, slot, None);
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
                if let Some(frame) = self.b().valofs.last().copied() {
                    // RESULTIS inside a VALOF: stash the value in
                    // the result slot and branch to the VALOF's
                    // exit block. Subsequent statements lower into
                    // a fresh dead block.
                    self.b().emit(Instr::Store {
                        slot: frame.result_slot,
                        value,
                    });
                    self.b()
                        .terminate(Terminator::Branch(frame.exit_block));
                } else {
                    // Fallback: outside any VALOF, treat RESULTIS
                    // like a function-return. Sema has already
                    // warned about this shape.
                    self.b().terminate(Terminator::Return(Some(value)));
                }
                let dead = self.b().alloc_block("after.resultis");
                self.b().switch_to(dead);
            }
            Stmt::Finish(_) => {
                self.b().terminate(Terminator::Return(None));
                let dead = self.b().alloc_block("after.finish");
                self.b().switch_to(dead);
            }
            Stmt::While { cond, body, .. } => self.lower_while(cond, body, /*invert=*/ false),
            Stmt::Until { cond, body, .. } => self.lower_while(cond, body, /*invert=*/ true),
            Stmt::Repeat { body, .. } => self.lower_repeat_forever(body),
            Stmt::RepeatWhile { body, cond, .. } => {
                self.lower_repeat_with_cond(body, cond, /*invert=*/ false)
            }
            Stmt::RepeatUntil { body, cond, .. } => {
                self.lower_repeat_with_cond(body, cond, /*invert=*/ true)
            }
            Stmt::For {
                name,
                start,
                end,
                step,
                body,
                ..
            } => self.lower_for(name, start, end, step.as_ref(), body),
            Stmt::ForEach {
                names,
                annotation,
                iter,
                body,
                ..
            } => self.lower_foreach(names, annotation.as_deref(), iter, body),
            Stmt::Break(_) => {
                if let Some(target) = self.b().innermost_break() {
                    self.b().terminate(Terminator::Branch(target));
                    let dead = self.b().alloc_block("after.break");
                    self.b().switch_to(dead);
                }
                // BREAK outside any frame is sema-flagged; emit nothing.
            }
            Stmt::Loop(_) => {
                if let Some(target) = self.b().innermost_continue() {
                    self.b().terminate(Terminator::Branch(target));
                    let dead = self.b().alloc_block("after.loop");
                    self.b().switch_to(dead);
                }
            }
            Stmt::Endcase(_) => {
                // ENDCASE jumps out of the enclosing SWITCHON. We
                // reuse the same `break_block` slot — sema has
                // already verified this only fires inside SWITCHON.
                if let Some(target) = self.b().innermost_break() {
                    self.b().terminate(Terminator::Branch(target));
                    let dead = self.b().alloc_block("after.endcase");
                    self.b().switch_to(dead);
                }
            }
            Stmt::Switchon {
                scrutinee,
                cases,
                default,
                ..
            } => self.lower_switchon(scrutinee, cases, default.as_deref()),
            Stmt::Goto { label, .. } => {
                let target = self.b().label_block(label);
                self.b().terminate(Terminator::Branch(target));
                let dead = self.b().alloc_block("after.goto");
                self.b().switch_to(dead);
            }
            Stmt::Label { name, .. } => {
                let target = self.b().label_block(name);
                // Branch in from whatever the predecessor was, then
                // switch into the label block so subsequent
                // statements lower into it.
                if self.b().current_open() {
                    self.b().terminate(Terminator::Branch(target));
                }
                self.b().switch_to(target);
            }
            // SWITCHON / FOREACH / labels / RETAIN etc. — subsequent
            // IR-grow chunks lower these.
            _ => {}
        }
    }

    /// `WHILE c DO s` lowers to:
    ///
    ///   pred → header
    ///   header: cond, br c ? body : exit
    ///   body: lower(s), br header
    ///   exit: ...
    ///
    /// `UNTIL c DO s` is identical with the cond-branch arms swapped
    /// (use `invert=true`).
    fn lower_while(&mut self, cond: &Expr, body: &Stmt, invert: bool) {
        let header = self.b().alloc_block("while.header");
        let body_block = self.b().alloc_block("while.body");
        let exit = self.b().alloc_block("while.end");

        // Predecessor → header.
        self.b().terminate(Terminator::Branch(header));
        self.b().switch_to(header);
        let cond_value = self.lower_expr(cond);
        let (then_block, else_block) = if invert {
            (exit, body_block)
        } else {
            (body_block, exit)
        };
        self.b().terminate(Terminator::CondBranch {
            cond: cond_value,
            then_block,
            else_block,
        });

        self.b().switch_to(body_block);
        self.b().frames.push(Frame {
            break_block: exit,
            continue_block: Some(header),
        });
        self.lower_stmt(body);
        self.b().frames.pop();
        if self.b().current_open() {
            self.emit_safepoint();
            self.b().terminate(Terminator::Branch(header));
        }
        self.b().switch_to(exit);
    }

    /// `body REPEAT` — infinite loop. `BREAK` is the only exit.
    fn lower_repeat_forever(&mut self, body: &Stmt) {
        let body_block = self.b().alloc_block("repeat.body");
        let exit = self.b().alloc_block("repeat.end");

        self.b().terminate(Terminator::Branch(body_block));
        self.b().switch_to(body_block);
        self.b().frames.push(Frame {
            break_block: exit,
            continue_block: Some(body_block),
        });
        self.lower_stmt(body);
        self.b().frames.pop();
        if self.b().current_open() {
            self.emit_safepoint();
            self.b().terminate(Terminator::Branch(body_block));
        }
        self.b().switch_to(exit);
    }

    /// `body REPEATWHILE c` (do-while) and `body REPEATUNTIL c` (do-
    /// until). The body executes once before the test.
    fn lower_repeat_with_cond(&mut self, body: &Stmt, cond: &Expr, invert: bool) {
        let body_block = self.b().alloc_block("repeat.body");
        let test = self.b().alloc_block("repeat.test");
        let exit = self.b().alloc_block("repeat.end");

        self.b().terminate(Terminator::Branch(body_block));
        self.b().switch_to(body_block);
        // LOOP inside a do-while jumps to the test (next iteration's
        // condition); BREAK exits.
        self.b().frames.push(Frame {
            break_block: exit,
            continue_block: Some(test),
        });
        self.lower_stmt(body);
        self.b().frames.pop();
        if self.b().current_open() {
            self.b().terminate(Terminator::Branch(test));
        }
        self.b().switch_to(test);
        let cond_value = self.lower_expr(cond);
        let (then_block, else_block) = if invert {
            (exit, body_block)
        } else {
            (body_block, exit)
        };
        // Poll a safepoint at the test block — every iteration of
        // a do-while flows through here before deciding whether
        // to take the back-edge to body_block. An extra poll on
        // the loop-exit path is harmless.
        self.emit_safepoint();
        self.b().terminate(Terminator::CondBranch {
            cond: cond_value,
            then_block,
            else_block,
        });
        self.b().switch_to(exit);
    }

    /// `FOR i = e1 TO e2 [BY e3] DO body` lowers to:
    ///
    ///   pred:        alloca i, store e1, br header
    ///   header:      load i, icmp.le e2, br body : exit
    ///   body:        lower(body), br incr
    ///   incr:        load i, iadd e3, store, br header
    ///   exit:        ...
    fn lower_for(
        &mut self,
        name: &str,
        start: &Expr,
        end: &Expr,
        step: Option<&Expr>,
        body: &Stmt,
    ) {
        // Initialise the loop variable in the predecessor block so
        // the LET-style scoping works through Load/Store like every
        // other local.
        let start_v = self.lower_expr(start);
        let i_slot = self.b().alloca(name, TypeHint::Int);
        self.b().emit(Instr::Store {
            slot: i_slot,
            value: start_v,
        });
        self.b().push_scope();
        self.b().declare_local(name, i_slot, None);

        let header = self.b().alloc_block("for.header");
        let body_block = self.b().alloc_block("for.body");
        let incr = self.b().alloc_block("for.incr");
        let exit = self.b().alloc_block("for.end");

        self.b().terminate(Terminator::Branch(header));
        self.b().switch_to(header);
        // i <= end
        let i_dst = self.b().alloc_value();
        self.b().emit(Instr::Load {
            dst: i_dst,
            slot: i_slot,
            hint: TypeHint::Int,
        });
        let end_v = self.lower_expr(end);
        let cmp = self.b().alloc_value();
        self.b().emit(Instr::BinOp {
            dst: cmp,
            op: IrBinOp::ICmpLe,
            lhs: Value::Local(i_dst),
            rhs: end_v,
            hint: TypeHint::Int,
        });
        self.b().terminate(Terminator::CondBranch {
            cond: Value::Local(cmp),
            then_block: body_block,
            else_block: exit,
        });

        self.b().switch_to(body_block);
        self.b().frames.push(Frame {
            break_block: exit,
            continue_block: Some(incr),
        });
        self.lower_stmt(body);
        self.b().frames.pop();
        if self.b().current_open() {
            self.b().terminate(Terminator::Branch(incr));
        }

        self.b().switch_to(incr);
        let i_load = self.b().alloc_value();
        self.b().emit(Instr::Load {
            dst: i_load,
            slot: i_slot,
            hint: TypeHint::Int,
        });
        let step_v = match step {
            Some(s) => self.lower_expr(s),
            None => Value::Const(Const::Int(1)),
        };
        let i_next = self.b().alloc_value();
        self.b().emit(Instr::BinOp {
            dst: i_next,
            op: IrBinOp::IAdd,
            lhs: Value::Local(i_load),
            rhs: step_v,
            hint: TypeHint::Int,
        });
        self.b().emit(Instr::Store {
            slot: i_slot,
            value: Value::Local(i_next),
        });
        // Cooperative GC poll on the FOR back-edge — see
        // `emit_safepoint`. Without this, a tight pure-arithmetic
        // FOR loop with no body allocations and no callees could
        // run forever without parking, blocking any concurrent
        // collect that needs to scan our stack.
        self.emit_safepoint();
        self.b().terminate(Terminator::Branch(header));

        self.b().switch_to(exit);
        self.b().pop_scope();
    }

    /// `FOREACH name IN iter DO body`
    /// — or its destructuring form `FOREACH (n0, n1[, ...]) IN iter DO body`.
    ///
    /// Two iteration shapes, picked from the iterable's type
    /// hint:
    ///   - **VEC / FVEC**: `i = 0..__newbcpl_len(iter)`, each
    ///     `element = iter ! i`. Length lives at `*(iter - 8)`.
    ///   - **LIST / MANIFESTLIST**: walk the linked
    ///     `ListHeader → head → next → next → ...` chain.
    ///     Each iteration loads `cursor.value` (offset 8 of a
    ///     `ListAtom`) and steps via `cursor.next` (offset 16).
    ///     The header's `head` lives at offset 16 of a
    ///     `ListHeader`.
    ///
    /// Destructuring: when `names.len() > 1` the element is a
    /// SIMD-lane-packed value (PAIR ⇒ 2 lanes, QUAD ⇒ 4, OCT ⇒ 8).
    /// We emit a sign-aware lane unpack into N i64 locals — see
    /// `unpack_lanes`. Reference: `test_foreach_destructuring.bcl`
    /// — `FOREACH (x, y) IN list-of-pairs` binds `x = element.|0|,
    /// y = element.|1|` per step, and similar for wider lanes.
    fn lower_foreach(
        &mut self,
        names: &[String],
        _annotation: Option<&str>,
        iter: &Expr,
        body: &Stmt,
    ) {
        if names.is_empty() {
            return;
        }
        if matches!(iter.hint(), TypeHint::List) {
            return self.lower_foreach_list(names, iter, body);
        }
        // Lower the iterable once, store in a stable slot so the
        // length call and the per-iteration subscripts share it.
        let iter_v = self.lower_expr(iter);
        let iter_slot = self.b().alloca("foreach.iter", TypeHint::Vec);
        self.b().emit(Instr::Store {
            slot: iter_slot,
            value: iter_v,
        });

        // Length: `__newbcpl_len(iter)` is the BCPL convention.
        let iter_load = self.b().alloc_value();
        self.b().emit(Instr::Load {
            dst: iter_load,
            slot: iter_slot,
            hint: TypeHint::Vec,
        });
        let len_dst = self.b().alloc_value();
        self.b().emit(Instr::Call {
            dst: Some(len_dst),
            callee: Value::Function("__newbcpl_len".to_string()),
            args: vec![Value::Local(iter_load)],
            hint: TypeHint::Int,
        });

        // Loop header: i = 0; while i < len { body; i++ }.
        let i_slot = self.b().alloca("foreach.idx", TypeHint::Int);
        self.b().emit(Instr::Store {
            slot: i_slot,
            value: Value::Const(Const::Int(0)),
        });
        let header = self.b().alloc_block("foreach.header");
        let body_block = self.b().alloc_block("foreach.body");
        let incr = self.b().alloc_block("foreach.incr");
        let exit = self.b().alloc_block("foreach.end");

        self.b().terminate(Terminator::Branch(header));
        self.b().switch_to(header);
        let i_dst = self.b().alloc_value();
        self.b().emit(Instr::Load {
            dst: i_dst,
            slot: i_slot,
            hint: TypeHint::Int,
        });
        let cmp = self.b().alloc_value();
        self.b().emit(Instr::BinOp {
            dst: cmp,
            op: IrBinOp::ICmpLt,
            lhs: Value::Local(i_dst),
            rhs: Value::Local(len_dst),
            hint: TypeHint::Int,
        });
        self.b().terminate(Terminator::CondBranch {
            cond: Value::Local(cmp),
            then_block: body_block,
            else_block: exit,
        });

        self.b().switch_to(body_block);
        self.b().push_scope();
        // Compute element address: GEP iter + i * 8 (word stride).
        let iter_load2 = self.b().alloc_value();
        self.b().emit(Instr::Load {
            dst: iter_load2,
            slot: iter_slot,
            hint: TypeHint::Vec,
        });
        let i_load = self.b().alloc_value();
        self.b().emit(Instr::Load {
            dst: i_load,
            slot: i_slot,
            hint: TypeHint::Int,
        });
        let elem_addr = self.b().alloc_value();
        self.b().emit(Instr::Gep {
            dst: elem_addr,
            base: Value::Local(iter_load2),
            index: Value::Local(i_load),
            element_bytes: 8,
        });
        let elem = self.b().alloc_value();
        self.b().emit(Instr::IndirectLoad {
            dst: elem,
            addr: Value::Local(elem_addr),
            hint: TypeHint::Word,
        });

        // Bind names. With one name, the element is the binding.
        // With more, lane-unpack the element into each name's slot
        // (PAIR=2 lanes×i32, QUAD=4 lanes×i16, OCT=8 lanes×i8).
        if names.len() == 1 {
            let slot = self.b().alloca(&names[0], TypeHint::Word);
            self.b().emit(Instr::Store {
                slot,
                value: Value::Local(elem),
            });
            self.b().declare_local(&names[0], slot, None);
        } else {
            self.unpack_lanes(names, Value::Local(elem));
        }

        self.b().frames.push(Frame {
            break_block: exit,
            continue_block: Some(incr),
        });
        self.lower_stmt(body);
        self.b().frames.pop();
        if self.b().current_open() {
            self.b().terminate(Terminator::Branch(incr));
        }
        self.b().pop_scope();

        // Increment block.
        self.b().switch_to(incr);
        let i_now = self.b().alloc_value();
        self.b().emit(Instr::Load {
            dst: i_now,
            slot: i_slot,
            hint: TypeHint::Int,
        });
        let i_next = self.b().alloc_value();
        self.b().emit(Instr::BinOp {
            dst: i_next,
            op: IrBinOp::IAdd,
            lhs: Value::Local(i_now),
            rhs: Value::Const(Const::Int(1)),
            hint: TypeHint::Int,
        });
        self.b().emit(Instr::Store {
            slot: i_slot,
            value: Value::Local(i_next),
        });
        // FOREACH-vec back-edge safepoint poll.
        self.emit_safepoint();
        self.b().terminate(Terminator::Branch(header));

        self.b().switch_to(exit);
    }

    /// FOREACH over a real linked list (a `ListHeader` chain).
    /// Walks `header.head → atom → atom.next → ...` until the
    /// cursor goes null. Per iteration loads `atom.value` (a
    /// 64-bit word) and binds it to the name(s).
    ///
    /// Atom / header field offsets are fixed to match the C
    /// layout in `reference/runtime/ListDataTypes.h` mirrored
    /// in `newbcpl-runtime/builtins.rs`:
    ///   ListAtom   { i32 type @0, i32 pad @4, i64 value @8,
    ///                ptr  next  @16 } — total 24 bytes
    ///   ListHeader { i32 type @0, i32 contains_literals @4,
    ///                i64 length @8, ptr head @16, ptr tail @24 }
    ///                — total 32 bytes
    fn lower_foreach_list(&mut self, names: &[String], iter: &Expr, body: &Stmt) {
        const ATOM_VALUE_OFFSET: i64 = 8;
        const ATOM_NEXT_OFFSET: i64 = 16;
        const HEADER_HEAD_OFFSET: i64 = 16;

        // header pointer in a stable slot.
        let header_v = self.lower_expr(iter);
        let header_slot = self.b().alloca("foreach.list.hdr", TypeHint::List);
        self.b().emit(Instr::Store {
            slot: header_slot,
            value: header_v,
        });

        // cursor = header.head — a load via `Gep(base=header,
        // index=16, stride=1)`. The IR's Gep takes
        // `base + index * element_bytes`, so passing
        // `index=HEADER_HEAD_OFFSET, element_bytes=1` lands on
        // the head field exactly.
        let header_load = self.b().alloc_value();
        self.b().emit(Instr::Load {
            dst: header_load,
            slot: header_slot,
            hint: TypeHint::List,
        });
        let head_field = self.b().alloc_value();
        self.b().emit(Instr::Gep {
            dst: head_field,
            base: Value::Local(header_load),
            index: Value::Const(Const::Int(HEADER_HEAD_OFFSET)),
            element_bytes: 1,
        });
        let initial_head = self.b().alloc_value();
        self.b().emit(Instr::IndirectLoad {
            dst: initial_head,
            addr: Value::Local(head_field),
            hint: TypeHint::List,
        });

        // cursor slot — holds the current atom pointer.
        let cursor_slot = self.b().alloca("foreach.list.cur", TypeHint::List);
        self.b().emit(Instr::Store {
            slot: cursor_slot,
            value: Value::Local(initial_head),
        });

        let header_block = self.b().alloc_block("foreach.list.header");
        let body_block = self.b().alloc_block("foreach.list.body");
        let incr = self.b().alloc_block("foreach.list.next");
        let exit = self.b().alloc_block("foreach.list.end");

        self.b().terminate(Terminator::Branch(header_block));
        self.b().switch_to(header_block);
        let cursor_load = self.b().alloc_value();
        self.b().emit(Instr::Load {
            dst: cursor_load,
            slot: cursor_slot,
            hint: TypeHint::List,
        });
        // `cursor != 0` — coerce-friendly. The IR's icmp.ne
        // emit goes through `as_int_word`, which handles
        // pointer operands by ptrtoint-ing them.
        let cmp = self.b().alloc_value();
        self.b().emit(Instr::BinOp {
            dst: cmp,
            op: IrBinOp::ICmpNe,
            lhs: Value::Local(cursor_load),
            rhs: Value::Const(Const::Int(0)),
            hint: TypeHint::Int,
        });
        self.b().terminate(Terminator::CondBranch {
            cond: Value::Local(cmp),
            then_block: body_block,
            else_block: exit,
        });

        // Body: load atom.value, bind to name(s), run body.
        self.b().switch_to(body_block);
        self.b().push_scope();
        let cur_for_value = self.b().alloc_value();
        self.b().emit(Instr::Load {
            dst: cur_for_value,
            slot: cursor_slot,
            hint: TypeHint::List,
        });
        let value_field = self.b().alloc_value();
        self.b().emit(Instr::Gep {
            dst: value_field,
            base: Value::Local(cur_for_value),
            index: Value::Const(Const::Int(ATOM_VALUE_OFFSET)),
            element_bytes: 1,
        });
        let elem = self.b().alloc_value();
        self.b().emit(Instr::IndirectLoad {
            dst: elem,
            addr: Value::Local(value_field),
            hint: TypeHint::Word,
        });

        if names.len() == 1 {
            let slot = self.b().alloca(&names[0], TypeHint::Word);
            self.b().emit(Instr::Store {
                slot,
                value: Value::Local(elem),
            });
            self.b().declare_local(&names[0], slot, None);
        } else {
            self.unpack_lanes(names, Value::Local(elem));
        }

        self.b().frames.push(Frame {
            break_block: exit,
            continue_block: Some(incr),
        });
        self.lower_stmt(body);
        self.b().frames.pop();
        if self.b().current_open() {
            self.b().terminate(Terminator::Branch(incr));
        }
        self.b().pop_scope();

        // Step: cursor = cursor.next.
        self.b().switch_to(incr);
        let cur_for_next = self.b().alloc_value();
        self.b().emit(Instr::Load {
            dst: cur_for_next,
            slot: cursor_slot,
            hint: TypeHint::List,
        });
        let next_field = self.b().alloc_value();
        self.b().emit(Instr::Gep {
            dst: next_field,
            base: Value::Local(cur_for_next),
            index: Value::Const(Const::Int(ATOM_NEXT_OFFSET)),
            element_bytes: 1,
        });
        let next = self.b().alloc_value();
        self.b().emit(Instr::IndirectLoad {
            dst: next,
            addr: Value::Local(next_field),
            hint: TypeHint::List,
        });
        self.b().emit(Instr::Store {
            slot: cursor_slot,
            value: Value::Local(next),
        });
        // FOREACH-list back-edge safepoint poll.
        self.emit_safepoint();
        self.b().terminate(Terminator::Branch(header_block));

        self.b().switch_to(exit);
    }

    /// Unpack the lanes of a SIMD-packed value `elem` into N named
    /// locals. The packed shapes the destructuring grammar
    /// supports map directly to standard BCPL SIMD widths:
    /// 2 names ⇒ PAIR (two i32 lanes), 4 names ⇒ QUAD (four i16
    /// lanes), 8 names ⇒ OCT (eight i8 lanes). All extracted
    /// values are sign-extended to i64 word-shape so the body
    /// can use them in normal arithmetic. Anything else (e.g.
    /// 3 or 5 names) falls back to extracting `lane = (elem >>
    /// (lane_bits * i)) & lane_mask` with no special width — a
    /// best-effort that produces *some* reasonable bindings
    /// rather than zeros, with a warning that a future sema
    /// pass will reject the mismatch.
    fn unpack_lanes(&mut self, names: &[String], elem: Value) {
        let lane_bits: u32 = match names.len() {
            2 => 32,
            4 => 16,
            8 => 8,
            _ => 64 / names.len().max(1) as u32,
        };
        let total_bits = 64u32;
        for (i, n) in names.iter().enumerate() {
            // Extract lane i: (elem << (top_pad)) >> (top_pad + low_drop)
            // with arithmetic shifts, so the lane is sign-extended
            // into a full i64 word.
            let low_drop = (lane_bits as u64) * i as u64;
            let top_pad = total_bits as u64 - lane_bits as u64 - low_drop;
            let mut current = elem.clone();
            if top_pad > 0 {
                let shifted = self.b().alloc_value();
                self.b().emit(Instr::BinOp {
                    dst: shifted,
                    op: IrBinOp::Shl,
                    lhs: current.clone(),
                    rhs: Value::Const(Const::Int(top_pad as i64)),
                    hint: TypeHint::Int,
                });
                current = Value::Local(shifted);
            }
            let pulled_down = self.b().alloc_value();
            self.b().emit(Instr::BinOp {
                dst: pulled_down,
                op: IrBinOp::Shr, // arithmetic — sign-extends
                lhs: current,
                rhs: Value::Const(Const::Int((top_pad + low_drop) as i64)),
                hint: TypeHint::Int,
            });
            let slot = self.b().alloca(n, TypeHint::Word);
            self.b().emit(Instr::Store {
                slot,
                value: Value::Local(pulled_down),
            });
            self.b().declare_local(n, slot, None);
        }
    }

    /// `SWITCHON value INTO $( CASE k1: ... CASE k2: ... DEFAULT: ... $)`.
    ///
    /// Each parsed `SwitchCase` may carry multiple labels that share
    /// one body block (`CASE 1: CASE 2: stmt` produces values=[1,2],
    /// body=[stmt]). Adjacent fall-through cases (the parser records
    /// `CASE 1:` with no body, then `CASE 2:` with the actual stmt)
    /// land here as separate `SwitchCase` records — we lower them
    /// in source order, with each case's block branching to the
    /// next case's block on fallthrough. ENDCASE jumps to a shared
    /// exit block.
    fn lower_switchon(
        &mut self,
        scrutinee: &Expr,
        cases: &[SwitchCase],
        default: Option<&[Stmt]>,
    ) {
        let scrutinee_v = self.lower_expr(scrutinee);
        // One block per case, in source order. The default block
        // exists even when the user didn't write DEFAULT — codegen
        // either lowers it as a no-op or as the explicit body.
        let case_blocks: Vec<BlockId> = cases
            .iter()
            .enumerate()
            .map(|(i, _)| self.b().alloc_block(&format!("switch.case{i}")))
            .collect();
        let default_block = self.b().alloc_block("switch.default");
        let exit = self.b().alloc_block("switch.end");

        // Build the switch table. Each label expression in each case
        // points at the same case block.
        let mut table: Vec<(Value, BlockId)> = Vec::new();
        for (case, &block) in cases.iter().zip(case_blocks.iter()) {
            for v in &case.values {
                let label_v = self.lower_expr(v);
                table.push((label_v, block));
            }
        }
        self.b().terminate(Terminator::Switch {
            value: scrutinee_v,
            cases: table,
            default: default_block,
        });

        // Push a SWITCHON frame so ENDCASE / BREAK target `exit`.
        self.b().frames.push(Frame {
            break_block: exit,
            continue_block: None,
        });

        // Lower each case body. If a case falls through (no terminator
        // when we finish), branch to the next case's block — or to
        // the default block if this was the last case.
        for (i, case) in cases.iter().enumerate() {
            self.b().switch_to(case_blocks[i]);
            for stmt in &case.body {
                self.lower_stmt(stmt);
            }
            if self.b().current_open() {
                let next = case_blocks
                    .get(i + 1)
                    .copied()
                    .unwrap_or(default_block);
                self.b().terminate(Terminator::Branch(next));
            }
        }

        // The default block — body if present, otherwise just falls
        // through to exit.
        self.b().switch_to(default_block);
        if let Some(body) = default {
            for stmt in body {
                self.lower_stmt(stmt);
            }
        }
        if self.b().current_open() {
            self.b().terminate(Terminator::Branch(exit));
        }

        self.b().frames.pop();
        self.b().switch_to(exit);
    }

    fn lower_block(&mut self, block: &Block) {
        self.b().push_scope();
        for s in &block.stmts {
            self.lower_stmt(s);
        }
        self.b().pop_scope();
    }

    fn lower_let_stmt(&mut self, l: &LetDecl) {
        // Destructuring shape: `LET a, b = single_pair_expr`. The
        // parser cloned the RHS into every binding's expr slot
        // and set `destructure = true`. Lower-time semantics:
        // evaluate the RHS once, then lane-unpack into each name
        // (same machinery FOREACH destructuring uses).
        if l.destructure && l.bindings.len() > 1 {
            let names: Vec<String> =
                l.bindings.iter().map(|(n, _)| n.clone()).collect();
            let rhs = &l.bindings[0].1;
            let elem = self.lower_expr(rhs);
            self.unpack_lanes(&names, elem);
            return;
        }
        for (name, init) in &l.bindings {
            // Capture the class name (if any) before lowering, so
            // the LET binding can record it. Lowering a `NEW Foo()`
            // produces a fresh ValueId but doesn't return the class
            // name; we read it from the AST shape.
            let class_name = self.class_name_of_expr(init);
            let value = self.lower_expr(init);
            let slot = self.b().alloca(name, init.hint());
            self.b().emit(Instr::Store { slot, value });
            self.b().declare_local(name, slot, class_name);
        }
    }

    fn lower_assign(&mut self, targets: &[Expr], values: &[Expr]) {
        for (target, value) in targets.iter().zip(values.iter()) {
            let v = self.lower_expr(value);
            match target {
                Expr::Ident { name, .. } => {
                    if let Some(slot) = self.b().lookup_local_slot(name) {
                        self.b().emit(Instr::Store { slot, value: v });
                    } else if let Some(class_name) = self.current_class.clone() {
                        // Bare-field assignment inside a class
                        // method: `x := initialX` → field store
                        // through SELF when `x` is a field of the
                        // surrounding class.
                        if let Some(offset) =
                            self.lookup_field_offset(&class_name, name)
                        {
                            let base = self.load_self();
                            self.b().emit(Instr::FieldStore {
                                base,
                                byte_offset: offset,
                                value: v,
                            });
                        }
                    }
                }
                Expr::Binary {
                    op: BinaryOp::Dot,
                    lhs,
                    rhs,
                    ..
                } => {
                    if let (Some(class_name), Expr::Ident { name: field, .. }) =
                        (self.class_name_of_expr(lhs), rhs.as_ref())
                    {
                        let base = self.lower_expr(lhs);
                        if let Some(offset) = self.lookup_field_offset(&class_name, field)
                        {
                            self.b().emit(Instr::FieldStore {
                                base,
                                byte_offset: offset,
                                value: v,
                            });
                        }
                    }
                }
                Expr::Binary {
                    op,
                    lhs,
                    rhs,
                    ..
                } if subscript_stride_and_hint(*op).is_some() => {
                    // `v!i := value`, `v%i := value`, `v.%i := value`.
                    // Compute the address via GEP and emit an
                    // IndirectStore — symmetric with the rvalue path.
                    let stride = subscript_stride_and_hint(*op).unwrap().0;
                    let addr = self.lower_subscript_address(lhs, rhs, stride);
                    self.b().emit(Instr::IndirectStore { addr, value: v });
                }
                Expr::Unary {
                    op: UnaryOp::Indirection,
                    operand,
                    ..
                } => {
                    // `!ptr := value`.
                    let addr = self.lower_expr(operand);
                    self.b().emit(Instr::IndirectStore { addr, value: v });
                }
                Expr::Unary {
                    op: UnaryOp::CharIndirection,
                    operand,
                    ..
                } => {
                    // `%ptr := value` — codegen handles the byte-store.
                    let addr = self.lower_expr(operand);
                    self.b().emit(Instr::IndirectStore { addr, value: v });
                }
                _ => {
                    // Other lvalue forms (lane access, bitfield write)
                    // not yet lowered.
                }
            }
        }
    }

    /// Best-effort: return the class name an AST expression
    /// evaluates to, when lowering knows. Used by LET-binding
    /// recording and member access resolution.
    fn class_name_of_expr(&self, expr: &Expr) -> Option<String> {
        match expr {
            Expr::New { class_name, .. } => Some(class_name.clone()),
            Expr::Ident { name, .. } => {
                // SELF / SUPER inside a class method resolve to the
                // surrounding class so `SELF.field` and `SELF.m()`
                // work.
                if (name == "SELF" || name == "SUPER") && self.current_class.is_some() {
                    return self.current_class.clone();
                }
                self.current
                    .as_ref()
                    .and_then(|b| b.lookup_local_class(name))
            }
            _ => None,
        }
    }

    /// Look up a field's byte offset by class name + field name.
    /// Walks the layout's `fields` list (already inheritance-flat
    /// from sema's compute_layouts).
    fn lookup_field_offset(&self, class_name: &str, field: &str) -> Option<usize> {
        let layout = self.layouts.iter().find(|l| l.class_name == class_name)?;
        layout
            .fields
            .iter()
            .find(|f| f.name == field)
            .map(|f| f.offset)
    }

    /// Look up a method's vtable slot by class name + method name.
    fn lookup_method_slot(&self, class_name: &str, method: &str) -> Option<usize> {
        let layout = self.layouts.iter().find(|l| l.class_name == class_name)?;
        layout
            .vtable
            .iter()
            .find(|v| v.method_name == method)
            .map(|v| v.slot)
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
        if self.b().current_open() {
            self.b().terminate(Terminator::Branch(merge));
        }

        self.b().switch_to(else_block);
        if let Some(els) = else_stmt {
            self.lower_stmt(els);
        }
        if self.b().current_open() {
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
        if self.b().current_open() {
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
            Expr::New {
                class_name, args, ..
            } => self.lower_new(class_name, args),
            Expr::TypedConstruct {
                kind, args, ..
            } => self.lower_typed_construct(*kind, args, e.hint()),
            Expr::Valof { body, .. } => self.lower_valof(body, e.hint()),
        }
    }

    fn lower_typed_construct(
        &mut self,
        kind: TypeConstructorKind,
        args: &[Expr],
        hint: TypeHint,
    ) -> Value {
        let arg_values: Vec<Value> = args.iter().map(|a| self.lower_expr(a)).collect();
        let dst = self.b().alloc_value();
        self.b().emit(Instr::TypedConstruct {
            dst,
            kind: typed_kind(kind),
            args: arg_values,
            hint,
        });
        Value::Local(dst)
    }

    /// `VALOF stmt` — lowers to a slot + exit-block pair. RESULTIS
    /// inside the body stores into the slot and branches to exit;
    /// the VALOF expression's value is the slot's contents read at
    /// the exit block.
    fn lower_valof(&mut self, body: &Stmt, hint: TypeHint) -> Value {
        let result_slot = self.b().alloca("valof.result", hint);
        let exit_block = self.b().alloc_block("valof.end");
        self.b().valofs.push(ValofFrame {
            result_slot,
            exit_block,
        });
        self.lower_stmt(body);
        // If the body falls through without a RESULTIS, the slot is
        // never written — match BCPL's "undefined result" by leaving
        // the slot's zero-init in place and branching to exit.
        if self.b().current_open() {
            self.b().terminate(Terminator::Branch(exit_block));
        }
        self.b().valofs.pop();
        self.b().switch_to(exit_block);
        let dst = self.b().alloc_value();
        self.b().emit(Instr::Load {
            dst,
            slot: result_slot,
            hint,
        });
        Value::Local(dst)
    }

    fn lower_new(&mut self, class_name: &str, args: &[Expr]) -> Value {
        let arg_values: Vec<Value> = args.iter().map(|a| self.lower_expr(a)).collect();
        let dst = self.b().alloc_value();
        self.b().emit(Instr::New {
            dst,
            class_name: class_name.to_string(),
            args: arg_values,
        });
        Value::Local(dst)
    }

    fn lower_ident(&mut self, name: &str, hint: TypeHint) -> Value {
        if let Some(slot) = self.b().lookup_local_slot(name) {
            let dst = self.b().alloc_value();
            self.b().emit(Instr::Load { dst, slot, hint });
            return Value::Local(dst);
        }
        // MANIFEST constants substitute their integer value inline
        // — sema recorded these. Treat as a compile-time literal
        // rather than a function reference.
        if let Some(&v) = self.manifests.get(name) {
            return Value::Const(Const::Int(v));
        }
        // Inside a class method body, an unrecognised bare name may
        // be a field on `SELF`. Resolve through the surrounding
        // class layout and emit a SELF-relative FieldLoad.
        if let Some(class_name) = self.current_class.clone() {
            if let Some(offset) = self.lookup_field_offset(&class_name, name) {
                let self_v = self.load_self();
                let dst = self.b().alloc_value();
                self.b().emit(Instr::FieldLoad {
                    dst,
                    base: self_v,
                    byte_offset: offset,
                    hint,
                });
                return Value::Local(dst);
            }
        }
        // Unknown name — assume it's a function reference that
        // will be resolved at link time.
        Value::Function(name.to_string())
    }

    /// Emit a `__newbcpl_safepoint()` call with no result. Inserted
    /// at every loop back-edge so a long-running tight loop with
    /// no allocations and no callees still parks for a concurrent
    /// collector. Pairs with the function-entry poll emitted by
    /// `newbcpl-llvm`'s `emit_safepoint_poll`.
    fn emit_safepoint(&mut self) {
        self.b().emit(Instr::Call {
            dst: None,
            callee: Value::Function("__newbcpl_safepoint".to_string()),
            args: Vec::new(),
            hint: TypeHint::Word,
        });
    }

    /// Load the implicit `SELF` parameter as a Value. Only callable
    /// while lowering a class method; unwraps because the entry
    /// block always allocates SELF first via `start_method`.
    fn load_self(&mut self) -> Value {
        let slot = self
            .b()
            .lookup_local_slot("SELF")
            .expect("SELF slot must exist inside a class method");
        let dst = self.b().alloc_value();
        self.b().emit(Instr::Load {
            dst,
            slot,
            hint: TypeHint::Object,
        });
        Value::Local(dst)
    }

    fn lower_binary(&mut self, op: BinaryOp, lhs: &Expr, rhs: &Expr, hint: TypeHint) -> Value {
        // SIMD lane access `pair.|n|`.
        if matches!(op, BinaryOp::LaneAccess) {
            let vector = self.lower_expr(lhs);
            let lane = self.lower_expr(rhs);
            // Map the source operand's TypeHint to a SIMD kind so
            // codegen can pick packed-i64 bit-shift vs LLVM
            // extractelement. Default to PAIR for unknown — the
            // common case and the conservative fallback.
            let kind = simd_kind_from_hint(lhs.hint())
                .unwrap_or(crate::ir::TypedKind::Pair);
            let dst = self.b().alloc_value();
            self.b().emit(Instr::LaneExtract {
                dst,
                vector,
                lane,
                kind,
                hint,
            });
            return Value::Local(dst);
        }
        // Subscript family: `v ! i` / `v % i` / `v .% i` lower to
        // GEP + IndirectLoad. The element stride and result hint
        // depend on the subscript variant.
        if let Some((stride, load_hint)) = subscript_stride_and_hint(op) {
            let addr = self.lower_subscript_address(lhs, rhs, stride);
            let dst = self.b().alloc_value();
            self.b().emit(Instr::IndirectLoad {
                dst,
                addr,
                hint: load_hint,
            });
            return Value::Local(dst);
        }
        // Member access (`obj.field`) lowers through the class
        // layout, not the generic binary-op path. RHS is an Ident.
        if matches!(op, BinaryOp::Dot | BinaryOp::Of) {
            if let (Some(class_name), Expr::Ident { name: field, .. }) =
                (self.class_name_of_expr(lhs), rhs)
            {
                let base = self.lower_expr(lhs);
                if let Some(offset) = self.lookup_field_offset(&class_name, field) {
                    let dst = self.b().alloc_value();
                    self.b().emit(Instr::FieldLoad {
                        dst,
                        base,
                        byte_offset: offset,
                        hint,
                    });
                    return Value::Local(dst);
                }
            }
            // Class unknown or field missing — fall through to null.
            return Value::Const(Const::Null);
        }

        let lhs_v = self.lower_expr(lhs);
        let rhs_v = self.lower_expr(rhs);
        let lhs_h = lhs.hint();
        let rhs_h = rhs.hint();
        let Some(ir_op) = binop_to_ir(op, lhs_h, rhs_h) else {
            // Subscript family, lane access — not yet IR-lowered.
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
        // BCPL list / vector keyword operators lower to runtime
        // calls. The runtime helper names line up with NewCP's
        // convention so the GC and ABI shapes match.
        if let Some(default_name) = unary_runtime_helper(op) {
            // `LEN` is the one operator where the runtime helper
            // differs by operand shape — vectors carry their
            // length one word *before* the data pointer, while
            // lists hold it in a `ListHeader` field. Dispatch
            // here so the call site lands on the right address;
            // see `__newbcpl_len` and `__newbcpl_list_len` in
            // newbcpl-runtime/builtins.rs. `HD`/`TL`/`REST` are
            // already list-shaped and have no vector form.
            let runtime_name = if matches!(op, UnaryOp::Len)
                && matches!(operand.hint(), TypeHint::List)
            {
                "__newbcpl_list_len"
            } else {
                default_name
            };
            let arg = self.lower_expr(operand);
            // FREEVEC / FREELIST don't produce a useful value — emit
            // the call with no result slot.
            let dst = if matches!(op, UnaryOp::FreeVec | UnaryOp::FreeList) {
                None
            } else {
                Some(self.b().alloc_value())
            };
            self.b().emit(Instr::Call {
                dst,
                callee: Value::Function(runtime_name.to_string()),
                args: vec![arg],
                hint,
            });
            return match dst {
                Some(d) => Value::Local(d),
                None => Value::Unit,
            };
        }
        match op {
            UnaryOp::Indirection => {
                // `!ptr` — load a word from address ptr.
                let addr = self.lower_expr(operand);
                let dst = self.b().alloc_value();
                self.b().emit(Instr::IndirectLoad {
                    dst,
                    addr,
                    hint: TypeHint::Word,
                });
                Value::Local(dst)
            }
            UnaryOp::AddressOf => {
                // `@x` — for a local Ident, the alloca'd slot IS the
                // address (LLVM-style). Other forms (`@v!i`,
                // `@obj.field`) need GEP-style address compute,
                // deferred for now.
                if let Expr::Ident { name, .. } = operand {
                    if let Some(slot) = self.b().lookup_local_slot(name) {
                        return Value::Local(slot);
                    }
                }
                Value::Const(Const::Null)
            }
            UnaryOp::CharIndirection => {
                // `%ptr` — load a single byte from address ptr,
                // zero-extended to a word. Codegen emits `i8 load
                // -> zext i64`.
                let addr = self.lower_expr(operand);
                let dst = self.b().alloc_value();
                self.b().emit(Instr::IndirectLoad {
                    dst,
                    addr,
                    hint: TypeHint::Int,
                });
                Value::Local(dst)
            }
            _ => {
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
        }
    }

    /// Compute an element address for `base SUBSCRIPT index`. Used
    /// by the subscript family for both rvalue loads and lvalue
    /// stores. `element_bytes` controls the stride (1 for chars,
    /// 8 for words / floats / pointers).
    fn lower_subscript_address(
        &mut self,
        base: &Expr,
        index: &Expr,
        element_bytes: usize,
    ) -> Value {
        let base_v = self.lower_expr(base);
        let index_v = self.lower_expr(index);
        let dst = self.b().alloc_value();
        self.b().emit(Instr::Gep {
            dst,
            base: base_v,
            index: index_v,
            element_bytes,
        });
        Value::Local(dst)
    }

    fn lower_call(&mut self, callee: &Expr, args: &[Expr], hint: TypeHint) -> Value {
        // Method dispatch: callee is `obj.methodName`. When sema /
        // class lookup tells us the receiver's class and that class
        // has the named method in its vtable, lower as a MethodCall
        // — codegen emits the vtable load + indirect call.
        if let Expr::Binary {
            op: BinaryOp::Dot,
            lhs,
            rhs,
            ..
        } = callee
        {
            if let (Some(class_name), Expr::Ident { name: method, .. }) =
                (self.class_name_of_expr(lhs), rhs.as_ref())
            {
                if let Some(slot) = self.lookup_method_slot(&class_name, method) {
                    let receiver = self.lower_expr(lhs);
                    let arg_values: Vec<Value> =
                        args.iter().map(|a| self.lower_expr(a)).collect();
                    // Always capture the call's result. Routines
                    // return i64 0 by the BCPL convention so callers
                    // that ignore the value see a sensible zero;
                    // functions return their inferred type.
                    let dst = self.b().alloc_value();
                    self.b().emit(Instr::MethodCall {
                        dst: Some(dst),
                        receiver,
                        class_name: class_name.clone(),
                        vtable_slot: slot,
                        method_name: method.clone(),
                        args: arg_values,
                        hint,
                    });
                    return Value::Local(dst);
                }
            }
        }

        let callee_v = self.lower_expr(callee);
        let arg_values: Vec<Value> = args.iter().map(|a| self.lower_expr(a)).collect();
        // Always capture the call's result. The hint tells us the
        // *type* of what comes back, not whether to discard it — a
        // user-defined function returning WORD still has a meaningful
        // value, and BCPL routines return i64 0 by convention so
        // ignoring it is harmless.
        let dst = self.b().alloc_value();
        self.b().emit(Instr::Call {
            dst: Some(dst),
            callee: callee_v,
            args: arg_values,
            hint,
        });
        Value::Local(dst)
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

/// Map a SIMD-flavoured `TypeHint` to its IR `TypedKind`. Returns
/// `None` for non-SIMD hints — callers that hit `None` should pick
/// a sensible default (lane-access only ever fires on SIMD-typed
/// expressions, but sema may have been less specific than ideal,
/// so the IR layer keeps a conservative fallback).
fn simd_kind_from_hint(h: TypeHint) -> Option<crate::ir::TypedKind> {
    use crate::ir::TypedKind;
    Some(match h {
        TypeHint::Pair => TypedKind::Pair,
        TypeHint::FPair => TypedKind::FPair,
        TypeHint::Quad => TypedKind::Quad,
        TypeHint::FQuad => TypedKind::FQuad,
        TypeHint::Oct => TypedKind::Oct,
        TypeHint::FOct => TypedKind::FOct,
        _ => return None,
    })
}

/// Symbol name for a class method as it appears in the LLVM
/// module / vtable patch table. We use the simple `Class_method`
/// scheme; collisions are impossible because BCPL identifiers
/// reject `_`-only names. Re-used by JIT post-finalize patching
/// when populating the vtable globals.
pub fn mangle_method(class_name: &str, method_name: &str) -> String {
    format!("{class_name}_{method_name}")
}

/// Map a list / vector keyword operator to its runtime helper
/// symbol name. Codegen emits an extern call to these from the
/// `__newbcpl_*` runtime API; matches the NewCP-derived GC and
/// list-data conventions described in
/// `reference/runtime/ListDataTypes.h`.
fn unary_runtime_helper(op: UnaryOp) -> Option<&'static str> {
    Some(match op {
        UnaryOp::Hd => "__newbcpl_list_hd",
        UnaryOp::Tl => "__newbcpl_list_tl",
        UnaryOp::Rest => "__newbcpl_list_rest",
        UnaryOp::Len => "__newbcpl_len",
        UnaryOp::FreeVec => "__newbcpl_freevec",
        UnaryOp::FreeList => "__newbcpl_freelist",
        _ => return None,
    })
}

fn typed_kind(k: TypeConstructorKind) -> crate::ir::TypedKind {
    use crate::ir::TypedKind;
    match k {
        TypeConstructorKind::Vec => TypedKind::Vec,
        TypeConstructorKind::FVec => TypedKind::FVec,
        TypeConstructorKind::Table => TypedKind::Table,
        TypeConstructorKind::FTable => TypedKind::FTable,
        TypeConstructorKind::Pair => TypedKind::Pair,
        TypeConstructorKind::FPair => TypedKind::FPair,
        TypeConstructorKind::Quad => TypedKind::Quad,
        TypeConstructorKind::FQuad => TypedKind::FQuad,
        TypeConstructorKind::Oct => TypedKind::Oct,
        TypeConstructorKind::FOct => TypedKind::FOct,
        TypeConstructorKind::List => TypedKind::List,
        TypeConstructorKind::ManifestList => TypedKind::ManifestList,
    }
}

/// Map a subscript-family `BinaryOp` to its `(element_bytes, load_hint)`
/// pair. Word vectors (`v!i`) use 8-byte stride and load WORD; char
/// vectors (`v%i`) use 1-byte stride and load INT (zero-extended);
/// float vectors (`v.%i`) use 8-byte stride and load FLOAT.
/// Returns `None` for non-subscript binary ops so callers can route
/// them to the regular binop path.
fn subscript_stride_and_hint(op: BinaryOp) -> Option<(usize, TypeHint)> {
    Some(match op {
        BinaryOp::Subscript => (8, TypeHint::Word),
        BinaryOp::CharSubscript => (1, TypeHint::Int),
        BinaryOp::FloatSubscript => (8, TypeHint::Float),
        _ => return None,
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

