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
    AsmProcDecl, BinaryOp, Block, ClassDecl, ClassMemberKind, ClassMethod, ClassMethodBody, Decl,
    Expr, FunctionDecl, LetDecl, Program, RoutineDecl, Span, Stmt, SwitchCase,
    TypeConstructorKind, UnaryOp,
};
use newbcpl_sema::{ClassLayout, SemaOutput, TypeHint};

use crate::ir::*;

/// Lower a typed AST plus its sema output into an IR module. The
/// caller must have run `newbcpl_sema::analyze(&program)` first so
/// expressions carry their hints.
pub fn lower(program: &Program, sema: &SemaOutput, module_name: &str) -> Module {
    let mut lowerer = Lowerer::new(&sema.layouts, &sema.manifests, &sema.globals);
    for decl in &program.items {
        match decl {
            Decl::Routine(r) => lowerer.lower_routine(r),
            Decl::Function(f) => lowerer.lower_function(f),
            Decl::Class(c) => lowerer.lower_class(c),
            Decl::AsmProc(a) => lowerer.lower_asm_proc(a),
            // Top-level decls that don't produce IR functions
            // (GET / MANIFEST / STATIC / GLOBAL) are skipped — GLOBALs
            // are surfaced as `Module::globals` (collected below) for
            // codegen to emit as LLVM module-level variables.
            _ => {}
        }
    }
    let mut globals: Vec<GlobalDecl> = sema
        .globals
        .iter()
        .map(|(name, init)| GlobalDecl {
            name: name.clone(),
            initial: *init,
        })
        .collect();
    // Deterministic order — `cargo test` outputs and `dump-llvm`
    // both benefit from a stable sort.
    globals.sort_by(|a, b| a.name.cmp(&b.name));
    Module {
        name: module_name.to_string(),
        functions: lowerer.functions,
        layouts: sema.layouts.clone(),
        globals,
        asm_procs: lowerer.asm_procs,
    }
}

struct Lowerer<'a> {
    functions: Vec<Function>,
    asm_procs: Vec<new_asm::AsmProc>,
    current: Option<Builder>,
    layouts: &'a [ClassLayout],
    /// `MANIFEST` constants from sema. Lookup in `lower_ident` for
    /// inline substitution — the BCPL convention treats a MANIFEST
    /// as a compile-time integer, not a runtime binding.
    manifests: &'a std::collections::HashMap<String, i64>,
    /// `GLOBAL` bindings from sema. When an identifier hits this
    /// set, lowering emits `GlobalLoad` / `GlobalStore` against the
    /// module-level `@<name>` slot instead of treating it as a
    /// stack-local or an unbound extern.
    globals: &'a std::collections::HashMap<String, Option<i64>>,
    /// Set while lowering a class method body. Allows bare-field
    /// identifiers (`x` inside `Point.set`) to resolve as
    /// SELF-relative field accesses, and lets `class_name_of_expr`
    /// recognise SELF and SUPER as having the surrounding class.
    current_class: Option<String>,
    /// Stack of active USING cleanups, innermost last. Each entry
    /// records what to call on scope exit. Function-exit terminators
    /// (RETURN / RESULTIS / FINISH) walk this stack and emit a
    /// RELEASE method call for every active scope they're escaping.
    /// Fall-through cleanup is handled by the USING statement itself
    /// without consulting this stack. BREAK / LOOP do not currently
    /// fire cleanups; this is a known v1 limitation.
    using_cleanups: Vec<UsingCleanup>,
}

#[derive(Debug, Clone)]
struct UsingCleanup {
    name: String,
    class_name: String,
    span: Span,
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
    /// `Lowerer::using_cleanups.len()` at the moment this frame was
    /// pushed. A `BREAK` / `LOOP` / `ENDCASE` that targets this frame
    /// must fire every USING cleanup added since — that's the slice
    /// `[cleanups_at_entry..]`. Lets control-transfer statements
    /// release every scope they're escaping without popping anything
    /// from the stack itself (the structural USING block will pop on
    /// its own way out).
    cleanups_at_entry: usize,
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

    fn innermost_break_frame(&self) -> Option<Frame> {
        self.frames.last().copied()
    }

    /// Innermost frame whose `continue_block` is set — i.e. the
    /// closest enclosing loop. SWITCHON frames have `continue_block`
    /// = None and `LOOP` walks past them, so this can return a
    /// further-out frame than `innermost_break_frame`. Used for
    /// cleanup-walk on `LOOP`.
    fn innermost_continue_frame(&self) -> Option<Frame> {
        self.frames
            .iter()
            .rev()
            .find(|f| f.continue_block.is_some())
            .copied()
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
        globals: &'a std::collections::HashMap<String, Option<i64>>,
    ) -> Self {
        Self {
            functions: Vec::new(),
            asm_procs: Vec::new(),
            current: None,
            layouts,
            manifests,
            globals,
            current_class: None,
            using_cleanups: Vec::new(),
        }
    }

    fn b(&mut self) -> &mut Builder {
        self.current.as_mut().expect("no current function")
    }

    fn lower_routine(&mut self, r: &RoutineDecl) {
        self.start_function_with_annotations(
            &r.name,
            &r.params,
            &r.param_annotations,
            TypeHint::Word,
        );
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
        self.start_function_with_annotations(
            &f.name,
            &f.params,
            &f.param_annotations,
            return_hint,
        );
        let value = self.lower_expr(&f.body);
        self.b().terminate(Terminator::Return(Some(value)));
        self.finish_function();
    }

    /// Walk a `CLASS` declaration and emit each method as a regular
    /// IR function with name `{class}_{method}`. The implicit
    /// receiver `SELF` becomes the first parameter (typed OBJECT).
    ///
    /// Field initialisers (`LET f = expr` / `FLET f = expr` inside
    /// the class body) are prepended to CREATE's body — every
    /// initialiser becomes a `SELF.field := expr` store that runs
    /// before any user CREATE code. If the class has initialisers
    /// but no user CREATE, we synthesise a `<Class>_CREATE(self)`
    /// whose only purpose is to run those stores. Sema injects a
    /// matching synthetic ClassMethodInfo so the layout's vtable
    /// slot 0 gets wired to the synthesised function.
    fn lower_class(&mut self, c: &ClassDecl) {
        let initialisers = collect_field_initialisers(c);
        let mut user_create_present = false;
        for member in &c.members {
            if let ClassMemberKind::Method(m) = &member.kind {
                if m.name == "CREATE" {
                    user_create_present = true;
                    self.lower_method(&c.name, m, &initialisers);
                } else {
                    self.lower_method(&c.name, m, &[]);
                }
            }
        }
        if !user_create_present && !initialisers.is_empty() {
            self.lower_synthetic_create(&c.name, c.span, &initialisers);
        }
    }

    /// Emit the IR function `<Class>_CREATE(self)` whose body is just
    /// the field-initialiser stores. Used when the class declares
    /// `LET f = expr` members but no explicit `CREATE` routine — sema
    /// injected a synthetic ClassMethodInfo so the layout treats the
    /// slot as defined; this is the matching IR function the vtable
    /// patcher binds it to.
    fn lower_synthetic_create(
        &mut self,
        class_name: &str,
        class_span: Span,
        initialisers: &[(String, &Expr)],
    ) {
        let mangled = mangle_method(class_name, "CREATE");
        let params = vec!["SELF".to_string()];
        self.start_function(&mangled, &params, TypeHint::Word);
        if let Some(b) = self.current.as_mut() {
            if let Some(info) = b.scopes.last_mut().and_then(|s| s.get_mut("SELF")) {
                info.class_name = Some(class_name.to_string());
            }
        }
        self.current_class = Some(class_name.to_string());
        self.emit_field_initialisers(class_name, initialisers);
        if self.b().current_open() {
            self.b().terminate(Terminator::Return(None));
        }
        self.current_class = None;
        let _ = class_span;
        self.finish_function();
    }

    /// Emit `SELF.field := expr` for every entry in `initialisers`,
    /// in source order. The current function must already have a
    /// SELF binding in scope (set up by `start_function` and tagged
    /// with the class name).
    fn emit_field_initialisers(
        &mut self,
        class_name: &str,
        initialisers: &[(String, &Expr)],
    ) {
        for (name, init) in initialisers {
            let value = self.lower_expr(init);
            if let Some(offset) = self.lookup_field_offset(class_name, name) {
                let self_v = self.load_self();
                self.b().emit(Instr::FieldStore {
                    base: self_v,
                    byte_offset: offset,
                    value,
                });
            }
        }
    }

    fn lower_method(
        &mut self,
        class_name: &str,
        m: &ClassMethod,
        prepend_initialisers: &[(String, &Expr)],
    ) {
        let mangled = mangle_method(class_name, &m.name);
        // Build the method's parameter list with SELF as the first
        // implicit param. Real BCPL params follow. The SELF slot
        // takes no annotation (its class identity is patched in
        // separately below); user params carry their own.
        let mut params: Vec<String> = Vec::with_capacity(m.params.len() + 1);
        params.push("SELF".to_string());
        params.extend(m.params.iter().cloned());
        let mut param_annotations: Vec<Option<String>> =
            Vec::with_capacity(m.params.len() + 1);
        param_annotations.push(None); // SELF
        param_annotations.extend(m.param_annotations.iter().cloned());

        let return_hint = match &m.body {
            ClassMethodBody::Routine(_) => TypeHint::Word,
            ClassMethodBody::Function(e) => e.hint(),
        };
        self.start_function_with_annotations(
            &mangled,
            &params,
            &param_annotations,
            return_hint,
        );

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

        // Field initialisers run before the user's CREATE body so the
        // user can see initialised slots from CREATE. Only CREATE
        // receives this prepend — see `lower_class`.
        if !prepend_initialisers.is_empty() {
            self.emit_field_initialisers(class_name, prepend_initialisers);
        }

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

    /// Lower an `ASM { }` procedure declaration.
    ///
    /// No IR function is created — the body goes directly into the
    /// `asm_procs` list, which `newbcpl-llvm` will emit as a
    /// `module asm` blob plus a matching `declare`.
    fn lower_asm_proc(&mut self, a: &AsmProcDecl) {
        let params = a
            .params
            .iter()
            .zip(a.param_annotations.iter())
            .map(|(name, ann)| new_asm::AsmParam {
                name: name.clone(),
                ty: annotation_to_asm_type(ann.as_deref()),
            })
            .collect();
        let return_type = if a.is_function {
            annotation_to_asm_ret_type(a.return_annotation.as_deref())
        } else {
            new_asm::AsmRetType::Void
        };
        self.asm_procs.push(new_asm::AsmProc {
            name: a.name.clone(),
            params,
            return_type,
            body: a.body.clone(),
        });
    }

    fn start_function(&mut self, name: &str, params: &[String], return_hint: TypeHint) {
        self.start_function_with_annotations(name, params, &[], return_hint);
    }

    /// Variant of `start_function` that propagates per-parameter
    /// `AS Class` annotations onto the local binding so that
    /// `class_name_of_expr` returns the right class identity for
    /// the parameter inside the body. The annotation slice is
    /// parallel to `params` (same length); a `None` (or a missing
    /// trailing entry) means the parameter has no class identity
    /// and stays a bare Word.
    fn start_function_with_annotations(
        &mut self,
        name: &str,
        params: &[String],
        annotations: &[Option<String>],
        return_hint: TypeHint,
    ) {
        // Pre-resolve any class-annotation strings to canonical
        // class names so `declare_local` can attach them directly.
        // Non-class annotations (`INTEGER`, `^STRING`, …) and names
        // that aren't in this compilation's layouts table resolve
        // to `None` and the parameter stays a Word — matching the
        // pre-existing un-annotated behaviour for backward compat.
        let resolved: Vec<Option<String>> = params
            .iter()
            .enumerate()
            .map(|(idx, _)| {
                annotations
                    .get(idx)
                    .and_then(|a| a.as_deref())
                    .and_then(|a| self.class_name_from_annotation(a))
            })
            .collect();

        let mut b = Builder::new(name);
        b.function.return_hint = return_hint;
        for (idx, p) in params.iter().enumerate() {
            let in_value = b.alloc_value();
            let slot = b.alloca(p, TypeHint::Word);
            b.emit(Instr::Store {
                slot,
                value: Value::Local(in_value),
            });
            b.declare_local(p, slot, resolved.get(idx).and_then(|c| c.clone()));
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
                // RELEASE every active USING binding before exiting.
                self.emit_using_cleanups_to_function_exit();
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
                    // a fresh dead block. We don't run USING cleanups
                    // here because RESULTIS-inside-VALOF stays in the
                    // same function frame; the USING in question is
                    // either inside the VALOF body (cleaned by the
                    // structural fall-through path) or outside it
                    // (still live afterwards).
                    self.b().emit(Instr::Store {
                        slot: frame.result_slot,
                        value,
                    });
                    self.b()
                        .terminate(Terminator::Branch(frame.exit_block));
                } else {
                    // Fallback: outside any VALOF, treat RESULTIS
                    // like a function-return — release everything.
                    self.emit_using_cleanups_to_function_exit();
                    self.b().terminate(Terminator::Return(Some(value)));
                }
                let dead = self.b().alloc_block("after.resultis");
                self.b().switch_to(dead);
            }
            Stmt::Finish(_) => {
                self.emit_using_cleanups_to_function_exit();
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
                if let Some(frame) = self.b().innermost_break_frame() {
                    // Fire every USING cleanup pushed since this
                    // frame was entered — innermost-first.
                    self.emit_using_cleanups_from(frame.cleanups_at_entry);
                    self.b().terminate(Terminator::Branch(frame.break_block));
                    let dead = self.b().alloc_block("after.break");
                    self.b().switch_to(dead);
                }
                // BREAK outside any frame is sema-flagged; emit nothing.
            }
            Stmt::Loop(_) => {
                if let Some(frame) = self.b().innermost_continue_frame() {
                    self.emit_using_cleanups_from(frame.cleanups_at_entry);
                    if let Some(target) = frame.continue_block {
                        self.b().terminate(Terminator::Branch(target));
                        let dead = self.b().alloc_block("after.loop");
                        self.b().switch_to(dead);
                    }
                }
            }
            Stmt::Endcase(_) => {
                // ENDCASE jumps out of the enclosing SWITCHON. We
                // reuse the same `break_block` slot — sema has
                // already verified this only fires inside SWITCHON.
                if let Some(frame) = self.b().innermost_break_frame() {
                    self.emit_using_cleanups_from(frame.cleanups_at_entry);
                    self.b().terminate(Terminator::Branch(frame.break_block));
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
            Stmt::Using {
                name,
                value,
                body,
                span,
            } => self.lower_using(name, value, body, *span),
            Stmt::Retain {
                name,
                value: Some(init),
                ..
            } => {
                // `RETAIN x = expr` — declares `x` and pins it past
                // its natural scope. In our GC model the binding is
                // already tracked as a stack root for as long as the
                // current scope holds it; the explicit "pin" matters
                // chiefly across scope boundaries (returning from
                // VALOF, etc.), which we approximate by lowering the
                // same as `LET x = expr`. The user-visible behaviour
                // — `x.field` reads through the value, surviving an
                // explicit `GC()` while in scope — is preserved.
                let class_name = self.class_name_of_expr(init);
                let value = self.lower_expr(init);
                let slot_hint = init.hint();
                let slot = self.b().alloca(name, slot_hint);
                self.b().emit(Instr::Store { slot, value });
                self.b().declare_local(name, slot, class_name);
            }
            Stmt::Retain { value: None, .. } => {
                // `RETAIN x` (mark existing) — no IR effect. The
                // binding is already a stack root in our model.
            }
            Stmt::Brk(span) => {
                // `BRK` debugger breakpoint — emit a call to the
                // runtime's `__newbcpl_brk(name, line)` handler.
                // `name` is the mangled name of the function the
                // BRK fired inside; `line` is the source line of
                // the BRK statement itself. Both are best-effort
                // and the handler tolerates null / 0. After the
                // handler returns we drop back into the regular
                // flow — BRK is a snapshot, not a halt.
                let routine_name = self.b().function.name.clone();
                let name_value = Value::Const(Const::String(routine_name));
                let line_value =
                    Value::Const(Const::Int(span.start.line as i64));
                self.b().emit(Instr::Call {
                    dst: None,
                    callee: Value::Function("__newbcpl_brk".to_string()),
                    args: vec![name_value, line_value],
                    hint: TypeHint::Word,
                });
            }
        }
    }

    /// Lower `USING name = expr DO body` as:
    ///
    ///   LET name = expr       (alloca + Store + declare_local)
    ///   <body>
    ///   name.RELEASE()        (synthesised method call)
    ///
    /// Plus a cleanup record on `self.using_cleanups` while body is
    /// being lowered — function-exit terminators (RETURN / RESULTIS /
    /// FINISH) consult that stack so an early exit still releases
    /// every active scope. The cleanup record is popped before the
    /// fall-through RELEASE so we don't double-fire.
    fn lower_using(&mut self, name: &str, value: &Expr, body: &Stmt, span: Span) {
        // Resolve the value's class up front — the receiver of the
        // synthesised RELEASE call needs it. If we can't determine
        // the class (sema couldn't see through the expression), the
        // RELEASE call has no vtable slot to dispatch through and
        // we emit the binding without cleanup. Sema's job to warn.
        let class_name = self.class_name_of_expr(value);
        let v = self.lower_expr(value);
        let slot_hint = value.hint();
        let slot = self.b().alloca(name, slot_hint);
        self.b().emit(Instr::Store { slot, value: v });
        self.b().declare_local(name, slot, class_name.clone());

        let cleanup = class_name.map(|class_name| UsingCleanup {
            name: name.to_string(),
            class_name,
            span,
        });
        if let Some(c) = cleanup.clone() {
            self.using_cleanups.push(c);
        }
        self.lower_stmt(body);
        if cleanup.is_some() {
            self.using_cleanups.pop();
        }
        // Fall-through cleanup: only emit if the body did not already
        // terminate the block (e.g. through RETURN). `current_open()`
        // distinguishes a still-live insertion point from a dead one.
        if let Some(c) = cleanup {
            if self.b().current_open() {
                self.emit_release_call(&c);
            }
        }
    }

    /// Synthesise `name.RELEASE()` as an IR method call. Mirrors
    /// what `lower_call` would emit for an explicit source-level call.
    fn emit_release_call(&mut self, cleanup: &UsingCleanup) {
        let Some(slot_hint_slot) = self.b().lookup_local_slot(&cleanup.name) else {
            return;
        };
        // Load the binding's current value (the heap pointer) and
        // dispatch RELEASE through the class's vtable. Slot 1 is the
        // reserved RELEASE slot; classes without an explicit RELEASE
        // get the runtime's no-op default-method stub bound there at
        // module-load time, so this is always safe.
        let receiver_value = {
            let dst = self.b().alloc_value();
            self.b().emit(Instr::Load {
                dst,
                slot: slot_hint_slot,
                hint: TypeHint::Object,
            });
            Value::Local(dst)
        };
        let Some(slot) = self.lookup_method_slot(&cleanup.class_name, "RELEASE") else {
            return;
        };
        let dst = self.b().alloc_value();
        self.b().emit(Instr::MethodCall {
            dst: Some(dst),
            receiver: receiver_value,
            class_name: cleanup.class_name.clone(),
            vtable_slot: slot,
            method_name: "RELEASE".to_string(),
            args: Vec::new(),
            hint: TypeHint::Word,
        });
        let _ = cleanup.span;
    }

    /// Emit RELEASE calls for every active USING scope, innermost
    /// first. Called from RETURN / RESULTIS / FINISH lowering so an
    /// early function exit still releases.
    fn emit_using_cleanups_to_function_exit(&mut self) {
        self.emit_using_cleanups_from(0);
    }

    /// Emit RELEASE for every cleanup in
    /// `using_cleanups[start..]`, innermost-first. Used by control
    /// transfers (BREAK / LOOP / ENDCASE) that escape some frames but
    /// not all — `start` is the target frame's `cleanups_at_entry`.
    /// Function-exit terminators pass `0` to fire everything.
    fn emit_using_cleanups_from(&mut self, start: usize) {
        // Clone the slice so iteration is independent of any mutation
        // emit_release_call could trigger. Reverse so innermost-first
        // (the slice ends with the innermost cleanup).
        let cleanups: Vec<UsingCleanup> = self
            .using_cleanups
            .iter()
            .skip(start)
            .rev()
            .cloned()
            .collect();
        for c in cleanups {
            self.emit_release_call(&c);
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
        let cleanups_at_entry = self.using_cleanups.len();
        self.b().frames.push(Frame {
            break_block: exit,
            continue_block: Some(header),
            cleanups_at_entry,
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
        let cleanups_at_entry = self.using_cleanups.len();
        self.b().frames.push(Frame {
            break_block: exit,
            continue_block: Some(body_block),
            cleanups_at_entry,
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
        let cleanups_at_entry = self.using_cleanups.len();
        self.b().frames.push(Frame {
            break_block: exit,
            continue_block: Some(test),
            cleanups_at_entry,
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
        // Pick the loop-continue comparator from the step's sign:
        //   `FOR i = 5 TO 1 BY -1`  iterates while `i >= end`
        //   `FOR i = 1 TO 5 [BY n]` iterates while `i <= end`
        // We only inspect literal / unary-neg-literal steps for
        // sign; a runtime-valued step falls back to `<=`. That
        // matches the BCPL convention (constant steps are by far
        // the common case; runtime-negative steps in the corpus
        // are rare enough to handle when they arise).
        let comparator = if step_is_negative_literal(step) {
            IrBinOp::ICmpGe
        } else {
            IrBinOp::ICmpLe
        };
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
            op: comparator,
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
        let cleanups_at_entry = self.using_cleanups.len();
        self.b().frames.push(Frame {
            break_block: exit,
            continue_block: Some(incr),
            cleanups_at_entry,
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
            byte_width: 8,
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

        let cleanups_at_entry = self.using_cleanups.len();
        self.b().frames.push(Frame {
            break_block: exit,
            continue_block: Some(incr),
            cleanups_at_entry,
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
            byte_width: 8,
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
            byte_width: 8,
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

        let cleanups_at_entry = self.using_cleanups.len();
        self.b().frames.push(Frame {
            break_block: exit,
            continue_block: Some(incr),
            cleanups_at_entry,
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
            byte_width: 8,
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
        let cleanups_at_entry = self.using_cleanups.len();
        self.b().frames.push(Frame {
            break_block: exit,
            continue_block: None,
            cleanups_at_entry,
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
        for (i, (name, init)) in l.bindings.iter().enumerate() {
            // Capture the class name (if any) before lowering, so
            // the LET binding can record it. Lowering a `NEW Foo()`
            // produces a fresh ValueId but doesn't return the class
            // name; we read it from the AST shape. If sema can't
            // see through the initialiser (e.g. `ps!i` is a
            // subscript on a polymorphic VEC), an explicit `AS Foo`
            // annotation overrides — `Foo` is looked up against the
            // known class layouts. See the
            // `vec_of_class_pointers_round_trip` probe in
            // `tests/newbcpl-tests/tests/matrix_tier6.rs`.
            let mut class_name = self.class_name_of_expr(init);
            if class_name.is_none() {
                if let Some(Some(ann)) = l.annotations.get(i) {
                    if let Some(named) = self.class_name_from_annotation(ann) {
                        class_name = Some(named);
                    }
                }
            }
            let value = self.lower_expr(init);
            // FLET overrides the slot's hint to Float when the
            // initialiser is a neutral integer scalar. Emit's
            // Store path will sitofp the value to match.
            let slot_hint = if matches!(l.kind, newbcpl_parser::LetKind::FLet)
                && matches!(
                    init.hint(),
                    TypeHint::Int | TypeHint::Word | TypeHint::Unknown
                )
            {
                TypeHint::Float
            } else {
                init.hint()
            };
            let slot = self.b().alloca(name, slot_hint);
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
                    } else if self.globals.contains_key(name) {
                        // GLOBAL binding: store to the module-level
                        // slot via `@<name>`. The symbol must already
                        // exist in the LLVM module — codegen emits
                        // it once per Module::globals entry up
                        // front, before any function lowering.
                        self.b().emit(Instr::GlobalStore {
                            name: name.clone(),
                            value: v,
                        });
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
                    // Stride 1 is the byte-subscript form — IndirectStore
                    // carries the width so codegen truncates to i8.
                    let stride = subscript_stride_and_hint(*op).unwrap().0;
                    let addr = self.lower_subscript_address(lhs, rhs, stride);
                    self.b().emit(Instr::IndirectStore {
                        addr,
                        value: v,
                        byte_width: stride as u32,
                    });
                }
                Expr::Unary {
                    op: UnaryOp::Indirection,
                    operand,
                    ..
                } => {
                    // `!ptr := value`.
                    let addr = self.lower_expr(operand);
                    self.b().emit(Instr::IndirectStore {
                        addr,
                        value: v,
                        byte_width: 8,
                    });
                }
                Expr::Unary {
                    op: UnaryOp::CharIndirection,
                    operand,
                    ..
                } => {
                    // `%ptr := value` — byte-store.
                    let addr = self.lower_expr(operand);
                    self.b().emit(Instr::IndirectStore {
                        addr,
                        value: v,
                        byte_width: 1,
                    });
                }
                Expr::Binary {
                    op: BinaryOp::Bitfield,
                    lhs,
                    rhs,
                    ..
                } => {
                    // `v %% (start, width) := payload`
                    let (start_expr, width_expr) = bitfield_split(rhs);
                    self.lower_bitfield_write(lhs, start_expr, width_expr, v);
                }
                Expr::Binary {
                    op: BinaryOp::LaneAccess,
                    lhs,
                    rhs,
                    ..
                } => {
                    // `pair.|i| := v` — SIMD packs are values, not
                    // pointers, so writing a lane means: load the
                    // packed value, build a new packed value with
                    // that lane replaced, and store the result back
                    // into the original lvalue. For v1 we only
                    // support the case where the SIMD value lives in
                    // an Ident's slot or a SELF-relative field — the
                    // common cases that show up in source.
                    let kind = simd_kind_from_hint(lhs.hint())
                        .unwrap_or(crate::ir::TypedKind::Pair);
                    let vector = self.lower_expr(lhs);
                    let lane = self.lower_expr(rhs);
                    let dst = self.b().alloc_value();
                    self.b().emit(Instr::LaneInsert {
                        dst,
                        vector,
                        lane,
                        value: v,
                        kind,
                    });
                    let new_pack = Value::Local(dst);
                    // Store the rebuilt pack back where lhs lived.
                    self.write_back_simd_lvalue(lhs, new_pack);
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
    /// recording and member access resolution. Recurses through:
    ///
    /// - identifier lookup (SELF / SUPER / LET-binding class)
    /// - `NEW Class(...)`
    /// - `obj.field` — looks the field up in `layouts` and returns its
    ///   `class_name` if sema recorded one
    /// - `obj.method(args)` — looks the method up in `layouts.vtable`
    ///   and returns its `result_class` if sema recorded one
    fn class_name_of_expr(&self, expr: &Expr) -> Option<String> {
        match expr {
            Expr::New { class_name, .. } => Some(class_name.clone()),
            Expr::Ident { name, .. } => {
                // SELF resolves to the current class; SUPER resolves
                // to its parent for member-access purposes. This is
                // what makes `SUPER.field` reach an inherited field
                // (via the parent's layout) and what teaches
                // `class_name_of_expr` on `SUPER.foo` to return the
                // parent's field-class — so chains rooted at SUPER
                // line up correctly.
                if name == "SELF" && self.current_class.is_some() {
                    return self.current_class.clone();
                }
                if name == "SUPER" {
                    return self
                        .current_class
                        .as_ref()
                        .and_then(|c| self.parent_class_of(c));
                }
                self.current
                    .as_ref()
                    .and_then(|b| b.lookup_local_class(name))
            }
            Expr::Binary {
                op: BinaryOp::Dot,
                lhs,
                rhs,
                ..
            } => {
                let receiver_class = self.class_name_of_expr(lhs)?;
                if let Expr::Ident { name: field, .. } = rhs.as_ref() {
                    let layout = self
                        .layouts
                        .iter()
                        .find(|l| l.class_name == receiver_class)?;
                    return layout
                        .fields
                        .iter()
                        .find(|f| f.name == *field)?
                        .class_name
                        .clone();
                }
                None
            }
            Expr::Call { callee, .. } => {
                if let Expr::Binary {
                    op: BinaryOp::Dot,
                    lhs,
                    rhs,
                    ..
                } = callee.as_ref()
                {
                    let receiver_class = self.class_name_of_expr(lhs)?;
                    if let Expr::Ident { name: method, .. } = rhs.as_ref() {
                        let layout = self
                            .layouts
                            .iter()
                            .find(|l| l.class_name == receiver_class)?;
                        return layout
                            .vtable
                            .iter()
                            .find(|v| v.method_name == *method)?
                            .result_class
                            .clone();
                    }
                }
                None
            }
            _ => None,
        }
    }

    /// Resolve an `AS Type` annotation string to a known class name.
    /// Mirrors the stripping logic sema uses: drop leading `^`
    /// pointer-to markers and any ` OF tail`, then look the
    /// remainder up against the IR's `layouts` table. Returns `None`
    /// for non-class annotations (`INT`, `FLOAT`, …) and for class
    /// names that aren't in this compilation unit.
    fn class_name_from_annotation(&self, annotation: &str) -> Option<String> {
        let mut s = annotation;
        while let Some(rest) = s.strip_prefix('^') {
            s = rest;
        }
        let base = match s.split_once(" OF ") {
            Some((head, _tail)) => head.trim(),
            None => s.trim(),
        };
        if self.layouts.iter().any(|l| l.class_name == base) {
            Some(base.to_string())
        } else {
            None
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

    /// The immediate parent of `class_name` (the `EXTENDS` target),
    /// or `None` if the class has no parent or doesn't appear in our
    /// layout table.
    fn parent_class_of(&self, class_name: &str) -> Option<String> {
        self.layouts
            .iter()
            .find(|l| l.class_name == class_name)?
            .extends
            .clone()
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
        // UNLESS body runs only when `cond` is zero. We must NOT
        // bitwise-NOT the value: classical BCPL truthiness treats any
        // non-zero value as true, so `NOT 1` is `~1 == -2` which is
        // also truthy — branching on that would always enter the body.
        // Instead swap the branch arms so a truthy `cond` skips the
        // body and a zero `cond` enters it.
        let body_block = self.b().alloc_block("unless.body");
        let merge = self.b().alloc_block("unless.end");
        self.b().terminate(Terminator::CondBranch {
            cond: cond_value,
            then_block: merge,
            else_block: body_block,
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
                // Decode the BCPL char literal to its integer byte
                // value at IR time. `BCPL syntax.md` §1.4 says a
                // character constant evaluates to the integer code of
                // the character; our user guide §2.6 fixes that to a
                // UTF-8 byte for our dialect. Escape forms per §1.3:
                //   *N / *n → 10   *T / *t → 9    *S / *s → 32
                //   *B / *b → 8    *P / *p → 12   *C / *c → 13
                //   *"      → 34   **      → 42
                // Anything else is a literal one-byte body.
                Value::Const(Const::Int(decode_char_lexeme(lexeme)))
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
        // GLOBAL bindings: read from the module-level slot. Sema
        // populated `self.globals` from every `GLOBAL <name> = expr`
        // / `GLOBAL $( name = expr; ... $)` declaration. Codegen
        // emits `load i64, ptr @<name>`.
        if self.globals.contains_key(name) {
            let dst = self.b().alloc_value();
            self.b().emit(Instr::GlobalLoad {
                dst,
                name: name.to_string(),
                hint,
            });
            return Value::Local(dst);
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

    /// Lower a bitfield read: `(value >> start) & mask` where
    /// `mask = (1 << width) - 1`. Width=None defaults to 1 (single
    /// bit). The implementation composes existing IR BinOps so no
    /// new instruction variant is needed.
    fn lower_bitfield_read(
        &mut self,
        value_expr: &Expr,
        start_expr: &Expr,
        width_expr: Option<&Expr>,
    ) -> Value {
        let val = self.lower_expr(value_expr);
        let start = self.lower_expr(start_expr);
        let mask = self.compute_bitfield_mask(width_expr);
        let shifted = self.b().alloc_value();
        self.b().emit(Instr::BinOp {
            dst: shifted,
            op: IrBinOp::Shr,
            lhs: val,
            rhs: start,
            hint: TypeHint::Int,
        });
        let masked = self.b().alloc_value();
        self.b().emit(Instr::BinOp {
            dst: masked,
            op: IrBinOp::BitAnd,
            lhs: Value::Local(shifted),
            rhs: mask,
            hint: TypeHint::Int,
        });
        Value::Local(masked)
    }

    /// Lower a bitfield write: `v %% (start, width) := payload`
    /// becomes `v := (v & ~(mask << start)) | ((payload & mask) << start)`.
    /// Like lane writes, the lvalue must be an Ident or a SELF /
    /// receiver field access — anything else is silently ignored.
    fn lower_bitfield_write(
        &mut self,
        value_expr: &Expr,
        start_expr: &Expr,
        width_expr: Option<&Expr>,
        payload: Value,
    ) {
        let old = self.lower_expr(value_expr);
        let start = self.lower_expr(start_expr);
        let mask = self.compute_bitfield_mask(width_expr);
        // mask_shifted = mask << start
        let mask_shifted = self.b().alloc_value();
        self.b().emit(Instr::BinOp {
            dst: mask_shifted,
            op: IrBinOp::Shl,
            lhs: mask.clone(),
            rhs: start.clone(),
            hint: TypeHint::Int,
        });
        // not_mask = ~mask_shifted  (XOR with -1 gives bitwise NOT;
        // we don't have a unary IR-NOT here that takes a Value, so
        // synthesise via XOR with -1).
        let not_mask = self.b().alloc_value();
        self.b().emit(Instr::BinOp {
            dst: not_mask,
            op: IrBinOp::BitXor,
            lhs: Value::Local(mask_shifted),
            rhs: Value::Const(Const::Int(-1)),
            hint: TypeHint::Int,
        });
        // cleared = old & not_mask
        let cleared = self.b().alloc_value();
        self.b().emit(Instr::BinOp {
            dst: cleared,
            op: IrBinOp::BitAnd,
            lhs: old,
            rhs: Value::Local(not_mask),
            hint: TypeHint::Int,
        });
        // payload_masked = payload & mask
        let payload_masked = self.b().alloc_value();
        self.b().emit(Instr::BinOp {
            dst: payload_masked,
            op: IrBinOp::BitAnd,
            lhs: payload,
            rhs: mask,
            hint: TypeHint::Int,
        });
        // payload_shifted = payload_masked << start
        let payload_shifted = self.b().alloc_value();
        self.b().emit(Instr::BinOp {
            dst: payload_shifted,
            op: IrBinOp::Shl,
            lhs: Value::Local(payload_masked),
            rhs: start,
            hint: TypeHint::Int,
        });
        // merged = cleared | payload_shifted
        let merged = self.b().alloc_value();
        self.b().emit(Instr::BinOp {
            dst: merged,
            op: IrBinOp::BitOr,
            lhs: Value::Local(cleared),
            rhs: Value::Local(payload_shifted),
            hint: TypeHint::Int,
        });
        // Store back. Reuse the SIMD write-back machinery — it
        // handles the same Ident / SELF.field / obj.field cases.
        self.write_back_simd_lvalue(value_expr, Value::Local(merged));
    }

    /// `mask = (1 << width) - 1`. If `width_expr` is a literal we
    /// constant-fold; otherwise emit the runtime computation.
    fn compute_bitfield_mask(&mut self, width_expr: Option<&Expr>) -> Value {
        match width_expr {
            None => Value::Const(Const::Int(1)),
            Some(Expr::IntLit { value, .. }) => {
                let w = (*value).clamp(0, 63) as u32;
                let mask = if w >= 64 {
                    -1i64
                } else {
                    ((1u64 << w) - 1) as i64
                };
                Value::Const(Const::Int(mask))
            }
            Some(expr) => {
                let width = self.lower_expr(expr);
                let one = self.b().alloc_value();
                self.b().emit(Instr::BinOp {
                    dst: one,
                    op: IrBinOp::Shl,
                    lhs: Value::Const(Const::Int(1)),
                    rhs: width,
                    hint: TypeHint::Int,
                });
                let mask = self.b().alloc_value();
                self.b().emit(Instr::BinOp {
                    dst: mask,
                    op: IrBinOp::ISub,
                    lhs: Value::Local(one),
                    rhs: Value::Const(Const::Int(1)),
                    hint: TypeHint::Int,
                });
                Value::Local(mask)
            }
        }
    }

    /// Store a rebuilt SIMD pack back where the lane-write's lhs
    /// lived. The lhs of `pair.|i|` is the SIMD value-producing
    /// expression — to make lane writes work the source must be an
    /// lvalue we can write back to. Currently supported lvalues:
    ///   - `Ident name` — store to the binding's slot.
    ///   - `SELF.field` / class-receiver `obj.field` — emit a
    ///     `FieldStore` through the receiver pointer.
    /// Anything else is silently ignored: sema doesn't currently
    /// reject `(a + b).|0| := v` and codegen can't recover the
    /// lvalue. Real source rarely hits this case; we can sharpen
    /// the diagnostic later.
    fn write_back_simd_lvalue(&mut self, lhs: &Expr, new_pack: Value) {
        match lhs {
            Expr::Ident { name, .. } => {
                if let Some(slot) = self.b().lookup_local_slot(name) {
                    self.b().emit(Instr::Store {
                        slot,
                        value: new_pack,
                    });
                } else if let Some(class_name) = self.current_class.clone() {
                    if let Some(offset) = self.lookup_field_offset(&class_name, name) {
                        let base = self.load_self();
                        self.b().emit(Instr::FieldStore {
                            base,
                            byte_offset: offset,
                            value: new_pack,
                        });
                    }
                }
            }
            Expr::Binary {
                op: BinaryOp::Dot,
                lhs: receiver,
                rhs,
                ..
            } => {
                if let (Some(class_name), Expr::Ident { name: field, .. }) =
                    (self.class_name_of_expr(receiver), rhs.as_ref())
                {
                    let base = self.lower_expr(receiver);
                    if let Some(offset) = self.lookup_field_offset(&class_name, field) {
                        self.b().emit(Instr::FieldStore {
                            base,
                            byte_offset: offset,
                            value: new_pack,
                        });
                    }
                }
            }
            _ => {}
        }
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
        // Bitfield read: `v %% (start, width)` — the parser packed
        // (start, width) into a nested `Binary { Bitfield, start,
        // width }` when width was given. A bare `v %% (start)`
        // collapses to `Binary { Bitfield, v, start }` with no
        // inner Bitfield. We unwrap and lower as a shift + mask
        // chain.
        if matches!(op, BinaryOp::Bitfield) {
            let (start_expr, width_expr) = bitfield_split(rhs);
            return self.lower_bitfield_read(lhs, start_expr, width_expr);
        }
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
        // GEP + IndirectLoad. The element stride drives both the
        // address calculation and the load width — stride 1 means
        // byte load + zero-extend, stride 8 means word/float load.
        if let Some((stride, load_hint)) = subscript_stride_and_hint(op) {
            let addr = self.lower_subscript_address(lhs, rhs, stride);
            let dst = self.b().alloc_value();
            self.b().emit(Instr::IndirectLoad {
                dst,
                addr,
                hint: load_hint,
                byte_width: stride as u32,
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
        // Logical AND/OR/XOR: reduce each operand to 0/1 via `!= 0`,
        // then combine bitwise. Yields a clean 0/1 even when the
        // operands are large bit patterns (so `1 AND 2` ≠ 0 but
        // both truthy → result 1; bitwise `1 BAND 2 = 0`).
        if matches!(op, BinaryOp::LogAnd | BinaryOp::LogOr | BinaryOp::LogXor) {
            let zero = Value::Const(Const::Int(0));
            let lhs_bool = self.b().alloc_value();
            self.b().emit(Instr::BinOp {
                dst: lhs_bool,
                op: IrBinOp::ICmpNe,
                lhs: lhs_v,
                rhs: zero.clone(),
                hint: TypeHint::Int,
            });
            let rhs_bool = self.b().alloc_value();
            self.b().emit(Instr::BinOp {
                dst: rhs_bool,
                op: IrBinOp::ICmpNe,
                lhs: rhs_v,
                rhs: zero,
                hint: TypeHint::Int,
            });
            let ir_op = match op {
                BinaryOp::LogAnd => IrBinOp::BitAnd,
                BinaryOp::LogOr => IrBinOp::BitOr,
                BinaryOp::LogXor => IrBinOp::BitXor,
                _ => unreachable!(),
            };
            let dst = self.b().alloc_value();
            self.b().emit(Instr::BinOp {
                dst,
                op: ir_op,
                lhs: Value::Local(lhs_bool),
                rhs: Value::Local(rhs_bool),
                hint: TypeHint::Int,
            });
            return Value::Local(dst);
        }
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
                    byte_width: 8,
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
                // zero-extended to a word. byte_width=1 tells
                // codegen to emit `load i8 + zext i64`.
                let addr = self.lower_expr(operand);
                let dst = self.b().alloc_value();
                self.b().emit(Instr::IndirectLoad {
                    dst,
                    addr,
                    hint: TypeHint::Int,
                    byte_width: 1,
                });
                Value::Local(dst)
            }
            UnaryOp::LogNot => {
                // Logical NOT: `NOT x` returns 1 if x is 0, else 0.
                // Lowered as `x == 0`, which produces a clean 0/1
                // result. This is the truth-correct counterpart to
                // `BNOT` / `~`, which flips every bit and is *not*
                // suitable for boolean negation (`BNOT 1` is `-2`,
                // still truthy in BCPL).
                let v = self.lower_expr(operand);
                let dst = self.b().alloc_value();
                self.b().emit(Instr::BinOp {
                    dst,
                    op: IrBinOp::ICmpEq,
                    lhs: v,
                    rhs: Value::Const(Const::Int(0)),
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
        // `AS_INT(x)` / `AS_FLOAT(x)` / `AS_STRING(x)` are
        // bit-reinterpretation casts, not runtime helpers. BCPL is
        // typeless on the wire — every value is a 64-bit word — so
        // the cast doesn't change the bits, just sema's idea of the
        // value's TypeHint. We rewrite the call into the argument
        // directly; downstream codegen reads the surrounding
        // context (e.g. `FWRITE(AS_FLOAT(s))`) for the float-vs-int
        // load. Routing through a real `extern "C"` runtime function
        // would have an x86-64 ABI mismatch on AS_FLOAT (XMM0 return
        // vs RAX) — sidestepping the call entirely keeps the
        // semantics correct without ABI gymnastics.
        if let Expr::Ident { name, .. } = callee {
            if matches!(name.as_str(), "AS_INT" | "AS_FLOAT" | "AS_STRING") {
                if let Some(arg) = args.first() {
                    return self.lower_expr(arg);
                }
                // Zero-arg `AS_*()` shouldn't happen in real code;
                // give it a sensible zero.
                return Value::Const(Const::Int(0));
            }
        }
        // SUPER.method(args) — static dispatch to the *parent*'s
        // implementation, bypassing the vtable. C++-style semantics:
        // even if the dynamic class overrides `method`, a SUPER call
        // must reach the parent's body, otherwise SUPER.CREATE in a
        // subclass would recurse into its own CREATE. We emit a
        // direct `<parent>_<method>` call with SELF as the implicit
        // first argument.
        if let Expr::Binary {
            op: BinaryOp::Dot,
            lhs,
            rhs,
            ..
        } = callee
        {
            let receiver_is_super = matches!(
                lhs.as_ref(),
                Expr::Ident { name, .. } if name == "SUPER"
            );
            if receiver_is_super {
                if let (Some(parent), Expr::Ident { name: method, .. }) = (
                    self.current_class
                        .as_ref()
                        .and_then(|c| self.parent_class_of(c)),
                    rhs.as_ref(),
                ) {
                    let receiver = self.load_self();
                    let mut call_args: Vec<Value> = Vec::with_capacity(args.len() + 1);
                    call_args.push(receiver);
                    for a in args {
                        call_args.push(self.lower_expr(a));
                    }
                    let dst = self.b().alloc_value();
                    self.b().emit(Instr::Call {
                        dst: Some(dst),
                        callee: Value::Function(mangle_method(&parent, method)),
                        args: call_args,
                        hint,
                    });
                    return Value::Local(dst);
                }
            }
        }
        // Method dispatch: callee is `obj.methodName`. Three paths,
        // tried in order:
        //
        //   1. Static class known + method in vtable → `MethodCall`,
        //      direct vtable-slot dispatch.
        //   2. Static class known but method not in vtable, OR
        //      static class unknown → `IndirectMethodCall`, runtime
        //      name-based lookup via `__newbcpl_lookup_method`.
        //   3. Callee isn't `obj.method` shape → fall through to the
        //      generic indirect `Call` path below.
        if let Expr::Binary {
            op: BinaryOp::Dot,
            lhs,
            rhs,
            ..
        } = callee
        {
            if let Expr::Ident { name: method, .. } = rhs.as_ref() {
                let receiver_class = self.class_name_of_expr(lhs);
                let static_slot = receiver_class
                    .as_deref()
                    .and_then(|c| self.lookup_method_slot(c, method));

                if let (Some(class_name), Some(slot)) =
                    (receiver_class.clone(), static_slot)
                {
                    // Path 1 — direct vtable-slot dispatch.
                    let receiver = self.lower_expr(lhs);
                    let arg_values: Vec<Value> =
                        args.iter().map(|a| self.lower_expr(a)).collect();
                    let dst = self.b().alloc_value();
                    self.b().emit(Instr::MethodCall {
                        dst: Some(dst),
                        receiver,
                        class_name,
                        vtable_slot: slot,
                        method_name: method.clone(),
                        args: arg_values,
                        hint,
                    });
                    return Value::Local(dst);
                }
                // Path 2 — emit a runtime name-based dispatch. We
                // reach this branch when:
                //   * `class_name_of_expr` returns None (receiver
                //     is an untyped parameter or unknown alias), or
                //   * the static class doesn't carry this method
                //     name in its vtable (cross-class dispatch
                //     through a typed-as-base reference whose
                //     declared class lacks the override).
                let receiver = self.lower_expr(lhs);
                let arg_values: Vec<Value> =
                    args.iter().map(|a| self.lower_expr(a)).collect();
                let dst = self.b().alloc_value();
                self.b().emit(Instr::IndirectMethodCall {
                    dst: Some(dst),
                    receiver,
                    method_name: method.clone(),
                    args: arg_values,
                    hint,
                });
                return Value::Local(dst);
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
        BitXor => IrBinOp::BitXor,
        Eqv => IrBinOp::ICmpEq, // EQV is "is the same boolean" → equality
        Neqv => IrBinOp::BitXor, // NEQV is XOR (alias of BXOR)
        // Logical ops are handled before binop_to_ir is called — see
        // `lower_binary` — because they need to reduce each operand
        // to a 0/1 boolean first. They never reach this point.
        LogAnd | LogOr | LogXor => return None,
        Shl => IrBinOp::Shl,
        Shr => IrBinOp::Shr,
        // Subscript family + member access aren't lowered yet.
        Subscript | Bitfield | CharSubscript | FloatSubscript | Dot | Of | LaneAccess => {
            return None;
        }
    })
}

/// True iff the FOR-loop step is a compile-time-known negative
/// integer — either `-3` (Unary{Neg, IntLit}) or directly an
/// `IntLit` with a negative value. Used by `lower_for` to pick
/// `i >= end` instead of `i <= end` for the loop-continue test.
fn step_is_negative_literal(step: Option<&Expr>) -> bool {
    let Some(expr) = step else {
        return false;
    };
    match expr {
        Expr::IntLit { value, .. } => *value < 0,
        Expr::Unary {
            op: UnaryOp::Neg,
            operand,
            ..
        } => matches!(operand.as_ref(), Expr::IntLit { value, .. } if *value > 0),
        _ => false,
    }
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

/// Split the rhs of a bitfield expression into (start, width). The
/// parser packs `(start, width)` into a nested `Binary { Bitfield,
/// start, width }` when the width is explicit; a bare `(start)`
/// collapses to just `start`. Returns the inner expressions in
/// source order; `width` is `None` when omitted (the language
/// defaults that to one bit).
/// Map an `AS Type` annotation string to the ABI register class used
/// by ASM-procedure parameters. Packed-SIMD scalar types (PAIR, FPAIR,
/// QUAD, etc.) travel as `i64` words in BCPL's calling convention, so
/// they fold to `Word` here. Unknown / missing annotations default to
/// `Word` — that gives a plain integer-register slot, which is what
/// you want for pointers and untyped data.
///
/// Matching is case-insensitive: the parser preserves the source
/// spelling so `AS FLOAT` and `AS Float` produce different annotation
/// strings, but the canonical type name does not depend on case.
fn annotation_to_asm_type(ann: Option<&str>) -> new_asm::AsmType {
    let Some(s) = ann else {
        return new_asm::AsmType::Word;
    };
    if s.eq_ignore_ascii_case("FLOAT") {
        new_asm::AsmType::Float
    } else if s.eq_ignore_ascii_case("FQUAD") {
        new_asm::AsmType::FQuad
    } else if s.eq_ignore_ascii_case("FOCT") {
        new_asm::AsmType::FOct
    } else {
        new_asm::AsmType::Word
    }
}

/// Map a return-type annotation string to the matching ABI return
/// register class. Same case-insensitive matching rules as the
/// parameter variant; the additional `Void` case is reached only
/// from the `BE ASM` (no-return) lowering path, so it never appears
/// as a user-written annotation here.
fn annotation_to_asm_ret_type(ann: Option<&str>) -> new_asm::AsmRetType {
    let Some(s) = ann else {
        return new_asm::AsmRetType::Word;
    };
    if s.eq_ignore_ascii_case("FLOAT") {
        new_asm::AsmRetType::Float
    } else if s.eq_ignore_ascii_case("FQUAD") {
        new_asm::AsmRetType::FQuad
    } else if s.eq_ignore_ascii_case("FOCT") {
        new_asm::AsmRetType::FOct
    } else {
        new_asm::AsmRetType::Word
    }
}

/// Decode a BCPL character-literal lexeme (with the surrounding
/// quotes still attached, e.g. `'A'`, `'*N'`, `'**'`) to its
/// integer byte value. The eight escape forms are listed in
/// `BCPL syntax.md` §1.3 and apply to character constants per §1.4;
/// anything else is the literal byte of the body. UTF-8 multibyte
/// glyphs can't appear in a char literal — the lexer's
/// `lex_character` only consumes a single body byte, so a multibyte
/// sequence would have failed to lex before reaching us.
fn decode_char_lexeme(lexeme: &str) -> i64 {
    // Strip the opening `'` and trailing `'`; what's left is the
    // body (1 byte for a plain char, 2 bytes for an escape).
    let bytes = lexeme.as_bytes();
    if bytes.len() < 3 || bytes[0] != b'\'' || bytes[bytes.len() - 1] != b'\'' {
        // Malformed lexeme — return 0 rather than panic. Sema/lex
        // should have rejected this earlier; we don't want a bad
        // lexeme to crash codegen.
        return 0;
    }
    let body = &bytes[1..bytes.len() - 1];
    match body {
        [b'*', b'n'] | [b'*', b'N'] => 10,
        [b'*', b't'] | [b'*', b'T'] => 9,
        [b'*', b's'] | [b'*', b'S'] => 32,
        [b'*', b'b'] | [b'*', b'B'] => 8,
        [b'*', b'p'] | [b'*', b'P'] => 12,
        [b'*', b'c'] | [b'*', b'C'] => 13,
        [b'*', b'"'] => 34,
        [b'*', b'*'] => 42,
        [b'*', b'\''] => 39,
        // Single literal byte — return as i64.
        [c] => *c as i64,
        // Unrecognised escape — return the second byte as-is so we
        // don't silently corrupt the value. A future extension might
        // add more escapes here.
        [b'*', c] => *c as i64,
        _ => 0,
    }
}

fn bitfield_split(rhs: &Expr) -> (&Expr, Option<&Expr>) {
    if let Expr::Binary {
        op: BinaryOp::Bitfield,
        lhs,
        rhs: inner_rhs,
        ..
    } = rhs
    {
        (lhs.as_ref(), Some(inner_rhs.as_ref()))
    } else {
        (rhs, None)
    }
}

/// Extract every `LET name = expr` and `FLET name = expr` field
/// initialiser declared in a class body, in source order. The
/// returned list pairs the field name with a reference to the
/// initialiser expression. Uninitialised forms (`DECL x`, plain
/// `LET x, y` field declarations via `ClassMemberKind::Fields`,
/// or `FLET x` without `=`) are skipped — they have no initial
/// value to lower.
fn collect_field_initialisers(c: &ClassDecl) -> Vec<(String, &Expr)> {
    let mut out: Vec<(String, &Expr)> = Vec::new();
    for member in &c.members {
        match &member.kind {
            ClassMemberKind::Let(let_decl) => {
                for (name, expr) in &let_decl.bindings {
                    out.push((name.clone(), expr));
                }
            }
            ClassMemberKind::FLet(b) => {
                if let Some(expr) = &b.value {
                    out.push((b.name.clone(), expr));
                }
            }
            _ => {}
        }
    }
    out
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
        // LogNot is handled in `lower_unary` before this function is
        // called, because it needs an `ICmpEq` rather than a unary
        // IR op (single-operand → produce 0/1).
        UnaryOp::LogNot => return None,
        // Indirection / AddressOf / CharIndirection / Hd / Tl / Rest /
        // Len / FreeVec / FreeList aren't lowered yet.
        _ => return None,
    })
}

