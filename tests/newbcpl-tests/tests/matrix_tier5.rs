//! Tier 5 of `docs/test_matrix.md` — classes & methods.
//!
//! This is the tier the recent class-shape bugs lived in (LET-vs-DECL
//! fields, ROUTINE-`=`-expr methods, default-RELEASE-slot null calls,
//! `BE { ... }` class bodies). Each probe is a small `.bcl` program
//! that exercises one cell of the matrix and asserts on captured
//! stdout.
//!
//! How the runner works:
//!
//! * Each probe is a `(name, source, expected_stdout)` triple.
//! * The runner writes the source to a temp file, runs
//!   `newbcpl-driver run <file>` as a subprocess, and compares the
//!   captured stdout to the expected string.
//! * `newbcpl-driver` is a bin-only crate so `CARGO_BIN_EXE_*`
//!   isn't set — we resolve the path by walking up from the
//!   test binary's location (`target/<profile>/deps/<test>.exe`
//!   → `target/<profile>/newbcpl-driver[.exe]`).
//! * Each probe is its own `#[test]` so `cargo test --list` and
//!   per-test reporting work naturally.
//!
//! Adding a probe: write a new `#[test] fn cellname()` that calls
//! `expect(name, source, expected)` with a one-line description of
//! the cell it covers. Bugs that get fixed should land here as a
//! regression row in the matrix — see `docs/test_matrix.md`.

use newbcpl_tests::expect_stdout as expect;

// ─── Class shape axis ──────────────────────────────────────────────
//
// `CLASS Name $( ... $)`     -- classic BCPL bracket
// `CLASS Name { ... }`       -- C-style brace
// `CLASS Name BE { ... }`    -- variant with explicit `BE` marker

#[test]
fn class_shape_bcpl_brackets() {
    // The canonical bracket form. If this regresses, *everything*
    // class-shaped regresses.
    expect(
        "class_shape_bcpl_brackets",
        "CLASS P $(\n  DECL x\n  ROUTINE CREATE(ix) BE $( SELF.x := ix $)\n$)\nLET START() BE $(\n  LET p = NEW P(7)\n  WRITEN(p.x)\n$)\n",
        "7",
    );
}

#[test]
fn class_shape_c_braces() {
    expect(
        "class_shape_c_braces",
        "CLASS P {\n  DECL x\n  ROUTINE CREATE(ix) BE { SELF.x := ix }\n}\nLET START() BE {\n  LET p = NEW P(11)\n  WRITEN(p.x)\n}\n",
        "11",
    );
}

#[test]
fn class_shape_be_marker() {
    // `CLASS Name BE { ... }` — landed in commit a11a6ec.
    expect(
        "class_shape_be_marker",
        "CLASS P BE {\n  DECL x\n  ROUTINE CREATE(ix) BE { SELF.x := ix }\n}\nLET START() BE {\n  LET p = NEW P(13)\n  WRITEN(p.x)\n}\n",
        "13",
    );
}

// ─── Field declaration form axis ──────────────────────────────────
//
// `DECL x, y`            -- classic field declaration
// `LET x, y`             -- LET-style (no init), equivalent to DECL
// `LET x = expr`         -- LET-style with initialiser (no init yet)
// `FLET x = 0.0`         -- float field initialiser

#[test]
fn field_decl_classic() {
    expect(
        "field_decl_classic",
        "CLASS P $(\n  DECL x, y\n  ROUTINE CREATE(ix, iy) BE $( SELF.x := ix\n SELF.y := iy $)\n$)\nLET START() BE $(\n  LET p = NEW P(1, 2)\n  WRITEN(p.x)\n  WRITES(\"*S\")\n  WRITEN(p.y)\n$)\n",
        "1 2",
    );
}

#[test]
fn field_decl_let_no_init() {
    // `LET x, y` as field decls (no initialiser). Landed in
    // commit 799fa94.
    expect(
        "field_decl_let_no_init",
        "CLASS P $(\n  LET x, y\n  ROUTINE CREATE(ix, iy) BE $( SELF.x := ix\n SELF.y := iy $)\n$)\nLET START() BE $(\n  LET p = NEW P(3, 4)\n  WRITEN(p.x)\n  WRITES(\"*S\")\n  WRITEN(p.y)\n$)\n",
        "3 4",
    );
}

// ─── Method body shape axis ───────────────────────────────────────
//
// `ROUTINE name(p) BE stmt`     -- classic statement-bodied
// `FUNCTION name(p) = expr`     -- classic expression-bodied
// `ROUTINE name(p) = expr`      -- swapped (we accept this)
// `FUNCTION name(p) BE stmt`    -- swapped (we accept this)
// `LET name(p) BE stmt`         -- LET-style routine method
// `LET name(p) = expr`          -- LET-style function method

#[test]
fn method_routine_be_stmt() {
    expect(
        "method_routine_be_stmt",
        "CLASS P $(\n  DECL x\n  ROUTINE CREATE(ix) BE $( SELF.x := ix $)\n  ROUTINE setX(nx) BE $( SELF.x := nx $)\n$)\nLET START() BE $(\n  LET p = NEW P(5)\n  p.setX(9)\n  WRITEN(p.x)\n$)\n",
        "9",
    );
}

#[test]
fn method_function_eq_expr() {
    expect(
        "method_function_eq_expr",
        "CLASS P $(\n  DECL x\n  ROUTINE CREATE(ix) BE $( SELF.x := ix $)\n  FUNCTION getX() = SELF.x\n$)\nLET START() BE $(\n  LET p = NEW P(17)\n  WRITEN(p.getX())\n$)\n",
        "17",
    );
}

#[test]
fn method_routine_eq_expr_swap() {
    // `ROUTINE foo() = expr` — accepted alongside FUNCTION =,
    // landed in commit a11a6ec. The corpus's `test_visibility.bcl`
    // uses this form.
    expect(
        "method_routine_eq_expr_swap",
        "CLASS P $(\n  DECL x\n  ROUTINE CREATE(ix) BE $( SELF.x := ix $)\n  ROUTINE getX() = SELF.x\n$)\nLET START() BE $(\n  LET p = NEW P(19)\n  WRITEN(p.getX())\n$)\n",
        "19",
    );
}

#[test]
fn method_let_routine_form() {
    // `LET m(p) BE stmt` — LET-style method, landed in
    // commit 799fa94.
    expect(
        "method_let_routine_form",
        "CLASS P $(\n  LET x\n  LET init(ix) BE { SELF.x := ix }\n  LET getX() = SELF.x\n$)\nLET START() BE $(\n  LET p = NEW P\n  p.init(21)\n  WRITEN(p.getX())\n$)\n",
        "21",
    );
}

// ─── Vtable slot defaults ─────────────────────────────────────────
//
// Every class has implicit CREATE (slot 0) and RELEASE (slot 1).
// Classes that don't declare a body for those slots used to dispatch
// through a null pointer. The runtime's `__newbcpl_default_method`
// stub now fills the unbound entries.

#[test]
fn default_release_does_not_segfault() {
    // The bug: `obj.RELEASE()` on a class with no RELEASE
    // jumped to address 0 → SIGSEGV. Now lands on the no-op stub.
    // Landed in commit 799fa94.
    expect(
        "default_release_does_not_segfault",
        "CLASS P $(\n  DECL x\n  ROUTINE CREATE(ix) BE $( SELF.x := ix $)\n$)\nLET START() BE $(\n  LET p = NEW P(23)\n  p.RELEASE()\n  WRITES(\"ok\")\n$)\n",
        "ok",
    );
}

#[test]
fn no_explicit_create_still_constructs() {
    // `NEW P` on a class with no CREATE method: the implicit
    // default-method slot returns 0, and the GC-allocated block
    // is zero-initialised, so reading any field yields 0.
    expect(
        "no_explicit_create_still_constructs",
        "CLASS P $(\n  DECL x\n$)\nLET START() BE $(\n  LET p = NEW P\n  WRITEN(p.x)\n$)\n",
        "0",
    );
}

// ─── Cross-method reference / SELF resolution ────────────────────

#[test]
fn bare_field_name_inside_method() {
    // `x := initialX` (no `SELF.` prefix) inside a method body
    // resolves to a SELF-relative field store. Manifesto §2.
    expect(
        "bare_field_name_inside_method",
        "CLASS P $(\n  DECL x\n  ROUTINE CREATE(ix) BE $( x := ix $)\n  FUNCTION getX() = x\n$)\nLET START() BE $(\n  LET p = NEW P(25)\n  WRITEN(p.getX())\n$)\n",
        "25",
    );
}

#[test]
fn method_calls_sibling_method() {
    // A method dispatches another method on `SELF`. Exercises
    // the vtable load + indirect call inside a method body.
    expect(
        "method_calls_sibling_method",
        "CLASS P $(\n  DECL x\n  ROUTINE CREATE(ix) BE $( SELF.x := ix $)\n  FUNCTION raw() = SELF.x\n  FUNCTION doubled() = SELF.raw() + SELF.raw()\n$)\nLET START() BE $(\n  LET p = NEW P(27)\n  WRITEN(p.doubled())\n$)\n",
        "54",
    );
}

#[test]
fn multiple_instances_isolate_state() {
    // Two instances of the same class don't share field
    // storage. Tests that `NEW Class` produces distinct heap
    // blocks (commit 10b74e8 — GC heap allocation).
    expect(
        "multiple_instances_isolate_state",
        "CLASS P $(\n  DECL x\n  ROUTINE CREATE(ix) BE $( SELF.x := ix $)\n  FUNCTION getX() = SELF.x\n$)\nLET START() BE $(\n  LET a = NEW P(31)\n  LET b = NEW P(41)\n  WRITEN(a.getX())\n  WRITES(\"*S\")\n  WRITEN(b.getX())\n$)\n",
        "31 41",
    );
}

// ─── Class-typed chains ───────────────────────────────────────────
//
// Sema must propagate class identity through `.field` and `.method()`
// hops so the next hop can dispatch correctly. Until this landed,
// `o.inner.getValue()` lost the class of `o.inner` at the second
// hop, type_of() returned WORD, codegen dispatched via the wrong
// builtin path, and the program crashed at runtime.

#[test]
fn chain_field_then_method() {
    // `o.inner.getValue()` — field access returning an object,
    // then method call on the returned object.
    expect(
        "chain_field_then_method",
        "CLASS Inner $(\n  DECL value\n  ROUTINE CREATE(v) BE SELF.value := v\n  FUNCTION getValue() = SELF.value\n$)\nCLASS Outer $(\n  DECL inner\n  ROUTINE CREATE(v) BE SELF.inner := NEW Inner(v)\n$)\nLET START() BE $(\n  LET o = NEW Outer(42)\n  WRITEN(o.inner.getValue())\n$)\n",
        "42",
    );
}

#[test]
fn chain_method_then_method() {
    // `o.getInner().getValue()` — method returning an object,
    // then method call on the result.
    expect(
        "chain_method_then_method",
        "CLASS Inner $(\n  DECL value\n  ROUTINE CREATE(v) BE SELF.value := v\n  FUNCTION getValue() = SELF.value\n$)\nCLASS Outer $(\n  DECL inner\n  ROUTINE CREATE(v) BE SELF.inner := NEW Inner(v)\n  FUNCTION getInner() = SELF.inner\n$)\nLET START() BE $(\n  LET o = NEW Outer(99)\n  WRITEN(o.getInner().getValue())\n$)\n",
        "99",
    );
}

#[test]
fn chain_via_decl_as_class_annotation() {
    // `DECL inner AS Inner` — the bare DECL field form with an AS
    // annotation. The parser used to reject `AS` after a DECL name;
    // this probe pins the post-fix behaviour. Same forward-reference
    // shape as `chain_via_as_class_annotation` (Outer above Inner),
    // resolved by the AS-refinement pass.
    expect(
        "chain_via_decl_as_class_annotation",
        "CLASS Outer $(\n  DECL inner AS Inner\n  ROUTINE CREATE(v) BE SELF.inner := NEW Inner(v)\n$)\nCLASS Inner $(\n  DECL value\n  ROUTINE CREATE(v) BE SELF.value := v\n  FUNCTION getValue() = SELF.value\n$)\nLET START() BE $(\n  LET o = NEW Outer(456)\n  WRITEN(o.inner.getValue())\n$)\n",
        "456",
    );
}

#[test]
fn chain_via_as_class_annotation() {
    // The field's class identity comes from an explicit `AS Inner`
    // annotation on the class member declaration rather than a
    // CREATE-time `SELF.field := NEW Inner(...)` back-fill. The
    // forward-reference form — Outer declared above Inner — has to
    // work because the AS-resolution pass runs after every class is
    // registered.
    expect(
        "chain_via_as_class_annotation",
        "CLASS Outer $(\n  LET inner AS Inner = ?\n  ROUTINE CREATE(v) BE SELF.inner := NEW Inner(v)\n$)\nCLASS Inner $(\n  DECL value\n  ROUTINE CREATE(v) BE SELF.value := v\n  FUNCTION getValue() = SELF.value\n$)\nLET START() BE $(\n  LET o = NEW Outer(123)\n  WRITEN(o.inner.getValue())\n$)\n",
        "123",
    );
}

// ─── USING (scope-deterministic RELEASE) ─────────────────────────
//
// `USING name = expr DO body` binds the value of `expr` to `name`,
// runs `body`, and then calls `name.RELEASE()` at scope exit. This
// replaces the linear-type MANAGED machinery from earlier in the
// project history. The probes here pin the surface behaviour:
//
//   - fall-through cleanup runs the RELEASE method,
//   - RETURN / RESULTIS / FINISH from inside the body still release,
//   - nested USINGs release innermost-first,
//   - a method call on the binding inside the body sees the right
//     receiver class.

#[test]
fn using_fall_through_runs_release() {
    // RELEASE is called when the body falls off the end. Visible
    // because the RELEASE method prints a marker the test asserts on.
    expect(
        "using_fall_through_runs_release",
        "CLASS R $(\n  ROUTINE CREATE() BE WRITES(\"open*N\")\n  ROUTINE RELEASE() BE WRITES(\"close*N\")\n$)\nLET START() BE $(\n  USING r = NEW R DO WRITES(\"work*N\")\n  WRITES(\"after*N\")\n$)\n",
        "open\nwork\nclose\nafter\n",
    );
}

#[test]
fn using_release_runs_before_early_return() {
    // A RETURN from inside the body must still close the resource.
    // The expected output ends with `close` from RELEASE, then nothing
    // — `after` would only print on fall-through.
    expect(
        "using_release_runs_before_early_return",
        "CLASS R $(\n  ROUTINE CREATE() BE WRITES(\"open*N\")\n  ROUTINE RELEASE() BE WRITES(\"close*N\")\n$)\nLET START() BE $(\n  USING r = NEW R DO $(\n    WRITES(\"work*N\")\n    RETURN\n  $)\n  WRITES(\"after*N\")\n$)\n",
        "open\nwork\nclose\n",
    );
}

#[test]
fn using_release_runs_before_finish() {
    // FINISH terminates the program; cleanup still runs first so the
    // user sees `close` before the program exits.
    expect(
        "using_release_runs_before_finish",
        "CLASS R $(\n  ROUTINE CREATE() BE WRITES(\"open*N\")\n  ROUTINE RELEASE() BE WRITES(\"close*N\")\n$)\nLET START() BE $(\n  USING r = NEW R DO $(\n    WRITES(\"work*N\")\n    FINISH\n  $)\n$)\n",
        "open\nwork\nclose\n",
    );
}

#[test]
fn nested_using_releases_innermost_first() {
    // Two nested USINGs: the inner resource is closed before the
    // outer one. Mirrors how Python's `with` / C#'s `using` nest.
    expect(
        "nested_using_releases_innermost_first",
        "CLASS A $(\n  ROUTINE CREATE() BE WRITES(\"open-A*N\")\n  ROUTINE RELEASE() BE WRITES(\"close-A*N\")\n$)\nCLASS B $(\n  ROUTINE CREATE() BE WRITES(\"open-B*N\")\n  ROUTINE RELEASE() BE WRITES(\"close-B*N\")\n$)\nLET START() BE $(\n  USING a = NEW A DO\n    USING b = NEW B DO\n      WRITES(\"work*N\")\n$)\n",
        "open-A\nopen-B\nwork\nclose-B\nclose-A\n",
    );
}

#[test]
fn using_binding_supports_method_calls_in_body() {
    // The USING binding is just a normal local — methods on it work,
    // class identity propagates, and RELEASE fires on exit.
    expect(
        "using_binding_supports_method_calls_in_body",
        "CLASS R $(\n  DECL x\n  ROUTINE CREATE(v) BE SELF.x := v\n  FUNCTION value() = SELF.x\n  ROUTINE RELEASE() BE $( $)\n$)\nLET START() BE $(\n  USING r = NEW R(42) DO WRITEN(r.value())\n$)\n",
        "42",
    );
}

// ─── USING cleanup on control transfer ────────────────────────────
//
// `BREAK` / `LOOP` / `ENDCASE` that escape a USING scope must fire
// RELEASE just like RETURN does. The probes above pinned the
// function-exit case (RETURN, FINISH); these pin the loop-exit and
// switchon-exit cases.

#[test]
fn break_out_of_using_runs_release() {
    // BREAK exits the surrounding loop. The USING inside the loop
    // body must still run RELEASE before the branch fires.
    expect(
        "break_out_of_using_runs_release",
        "CLASS R $(\n  ROUTINE CREATE() BE WRITES(\"open*N\")\n  ROUTINE RELEASE() BE WRITES(\"close*N\")\n$)\nLET START() BE $(\n  FOR i = 1 TO 3 DO $(\n    USING r = NEW R DO $(\n      WRITES(\"work*N\")\n      BREAK\n    $)\n  $)\n  WRITES(\"after*N\")\n$)\n",
        "open\nwork\nclose\nafter\n",
    );
}

#[test]
fn loop_through_using_runs_release_each_iteration() {
    // LOOP jumps back to the loop header. The USING inside the body
    // must release on each iteration where LOOP fires, then a fresh
    // one is created on the next iteration.
    expect(
        "loop_through_using_runs_release_each_iteration",
        "CLASS R $(\n  DECL n\n  ROUTINE CREATE(i) BE $( SELF.n := i\n WRITEN(i)\n WRITES(\"-open*N\") $)\n  ROUTINE RELEASE() BE $( WRITEN(SELF.n)\n WRITES(\"-close*N\") $)\n$)\nLET START() BE $(\n  FOR i = 1 TO 2 DO $(\n    USING r = NEW R(i) DO LOOP\n  $)\n$)\n",
        "1-open\n1-close\n2-open\n2-close\n",
    );
}

#[test]
fn endcase_through_using_runs_release() {
    // ENDCASE exits the enclosing SWITCHON. A USING inside a case
    // body must run RELEASE before the branch to the switchon exit.
    expect(
        "endcase_through_using_runs_release",
        "CLASS R $(\n  ROUTINE CREATE() BE WRITES(\"open*N\")\n  ROUTINE RELEASE() BE WRITES(\"close*N\")\n$)\nLET START() BE $(\n  LET k = 1\n  SWITCHON k INTO $(\n    CASE 1:\n      USING r = NEW R DO $(\n        WRITES(\"work*N\")\n        ENDCASE\n      $)\n    DEFAULT:\n      WRITES(\"default*N\")\n      ENDCASE\n  $)\n  WRITES(\"after*N\")\n$)\n",
        "open\nwork\nclose\nafter\n",
    );
}

#[test]
fn break_releases_inner_using_only() {
    // Two nested USINGs, BREAK from the inner — the inner releases
    // (it's inside the loop frame), the outer is *outside* the
    // loop frame and stays alive until its own fall-through. Order
    // of events: open-outer, open-inner, work, close-inner (BREAK),
    // after, close-outer (fall-through).
    expect(
        "break_releases_inner_using_only",
        "CLASS A $(\n  ROUTINE CREATE() BE WRITES(\"open-A*N\")\n  ROUTINE RELEASE() BE WRITES(\"close-A*N\")\n$)\nCLASS B $(\n  ROUTINE CREATE() BE WRITES(\"open-B*N\")\n  ROUTINE RELEASE() BE WRITES(\"close-B*N\")\n$)\nLET START() BE $(\n  USING a = NEW A DO $(\n    FOR i = 1 TO 3 DO $(\n      USING b = NEW B DO $(\n        WRITES(\"work*N\")\n        BREAK\n      $)\n    $)\n    WRITES(\"after-loop*N\")\n  $)\n$)\n",
        "open-A\nopen-B\nwork\nclose-B\nafter-loop\nclose-A\n",
    );
}

// ─── Field initialisers run at NEW ────────────────────────────────
//
// `CLASS Holder $( LET held = NEW Inner $)` registers `held` in the
// layout, but the initialiser expression has to be lowered too —
// otherwise `held` reads zero forever and only an explicit CREATE
// can populate it. Sema injects a synthetic CREATE entry when
// initialisers exist; IR lowering emits the matching `<Class>_CREATE`
// (or prepends stores to a user-written CREATE).

#[test]
fn field_initialiser_runs_at_new_no_user_create() {
    // `LET x = 42` inside a class without an explicit CREATE — the
    // synthesised CREATE runs the initialiser on every `NEW`.
    expect(
        "field_initialiser_runs_at_new_no_user_create",
        "CLASS P $(\n  LET x = 42\n$)\nLET START() BE $(\n  LET p = NEW P\n  WRITEN(p.x)\n$)\n",
        "42",
    );
}

#[test]
fn field_initialisers_prepended_to_user_create() {
    // A user CREATE coexisting with field initialisers — the
    // initialisers must run *before* the user body sees SELF, so
    // CREATE can override or read them.
    expect(
        "field_initialisers_prepended_to_user_create",
        "CLASS P $(\n  LET x = 7\n  LET y = 3\n  ROUTINE CREATE() BE SELF.y := SELF.y + SELF.x\n$)\nLET START() BE $(\n  LET p = NEW P\n  WRITEN(p.x) WRITES(\"*S\") WRITEN(p.y)\n$)\n",
        "7 10",
    );
}

#[test]
fn class_typed_field_initialiser_resolves_chain() {
    // The initialiser allocates a fresh `Inner`. After NEW, the chain
    // `outer.inner.value` works because (a) the initialiser stored
    // the Inner, and (b) sema's AS-resolution pass tagged the field
    // with class identity for the chain dispatch.
    expect(
        "class_typed_field_initialiser_resolves_chain",
        "CLASS Inner $(\n  DECL value\n  ROUTINE CREATE() BE SELF.value := 100\n$)\nCLASS Outer $(\n  LET inner = NEW Inner\n$)\nLET START() BE $(\n  LET o = NEW Outer\n  WRITEN(o.inner.value)\n$)\n",
        "100",
    );
}

// ─── Two-field accessors / setters ────────────────────────────────

#[test]
fn setter_then_getter() {
    // The shape `class1.bcl` exercised — set fields, then read
    // them back; covers the field-offset stability story.
    expect(
        "setter_then_getter",
        "CLASS Point $(\n  DECL x, y\n  ROUTINE CREATE(ix, iy) BE $( SELF.x := ix\n SELF.y := iy $)\n  ROUTINE set(nx, ny) BE $( SELF.x := nx\n SELF.y := ny $)\n  FUNCTION getX() = SELF.x\n  FUNCTION getY() = SELF.y\n$)\nLET START() BE $(\n  LET p = NEW Point(10, 20)\n  WRITEN(p.getX())\n  WRITES(\"*S\")\n  WRITEN(p.getY())\n  WRITES(\"*S\")\n  p.set(33, 44)\n  WRITEN(p.getX())\n  WRITES(\"*S\")\n  WRITEN(p.getY())\n$)\n",
        "10 20 33 44",
    );
}

// ─── SUPER dispatch ───────────────────────────────────────────────
//
// `SUPER.method()` inside a subclass dispatches to the parent class's
// implementation, not the subclass's. Used to call the parent's
// CREATE from a subclass CREATE, or chain into a parent's override.

#[test]
fn super_create_runs_parent_init() {
    // The subclass CREATE explicitly calls SUPER.CREATE, which runs
    // the parent's initialiser. Both fields should land populated.
    expect(
        "super_create_runs_parent_init",
        "CLASS Base $(\n  DECL a\n  ROUTINE CREATE(ia) BE SELF.a := ia\n$)\nCLASS Sub EXTENDS Base $(\n  DECL b\n  ROUTINE CREATE(ia, ib) BE $(\n    SUPER.CREATE(ia)\n    SELF.b := ib\n  $)\n$)\nLET START() BE $(\n  LET s = NEW Sub(10, 20)\n  WRITEN(s.a) WRITES(\"*S\") WRITEN(s.b)\n$)\n",
        "10 20",
    );
}

#[test]
fn super_method_call_reaches_parent_body() {
    // Subclass overrides `tag`, but `tag_via_super` calls the
    // parent's version explicitly. The parent's body returns 1,
    // the subclass's returns 2 — we want to see both in order.
    expect(
        "super_method_call_reaches_parent_body",
        "CLASS Base $(\n  FUNCTION tag() = 1\n$)\nCLASS Sub EXTENDS Base $(\n  FUNCTION tag() = 2\n  FUNCTION tag_via_super() = SUPER.tag()\n$)\nLET START() BE $(\n  LET s = NEW Sub\n  WRITEN(s.tag()) WRITES(\"*S\") WRITEN(s.tag_via_super())\n$)\n",
        "2 1",
    );
}

// ─── VIRTUAL override + dynamic dispatch ──────────────────────────
//
// A `VIRTUAL` method declared in the parent and overridden in the
// child must dispatch to the child's body when called on a child
// instance — even through a base-class reference path.

#[test]
fn virtual_method_dispatches_to_override() {
    // Direct call on a Sub instance: should see the subclass's body.
    expect(
        "virtual_method_dispatches_to_override",
        "CLASS Base $(\n  VIRTUAL FUNCTION speak() = 100\n$)\nCLASS Sub EXTENDS Base $(\n  FUNCTION speak() = 200\n$)\nLET START() BE $(\n  LET s = NEW Sub\n  WRITEN(s.speak())\n$)\n",
        "200",
    );
}

#[test]
fn virtual_dispatch_picks_subclass_body_via_vtable() {
    // Variant of `virtual_method_dispatches_to_override` that pins
    // the inheritance chain end-to-end: Base.code returns 1,
    // Sub.code returns 7. Both instances live in the same scope,
    // both dispatched via the same `.code()` call shape — the
    // vtable must route each to its dynamic class's body, not the
    // static call site's expectation.
    //
    // (A fuller probe that passes Sub through a routine typed
    // `AS Base` is blocked on the parser not accepting `AS` on
    // parameters yet — recorded in the reference audit.)
    expect(
        "virtual_dispatch_picks_subclass_body_via_vtable",
        "CLASS Base $(\n  VIRTUAL FUNCTION code() = 1\n$)\nCLASS Sub EXTENDS Base $(\n  FUNCTION code() = 7\n$)\nLET START() BE $(\n  LET b = NEW Base\n  LET s = NEW Sub\n  WRITEN(b.code()) WRITES(\"*S\") WRITEN(s.code())\n$)\n",
        "1 7",
    );
}

// ─── FINAL — override-rejection enforcement ───────────────────────
//
// A `FINAL` method on a parent class cannot be overridden by any
// descendant. Sema's pre-pass 1c walks every subclass's method list
// against its inheritance chain and rejects same-name methods whose
// ancestor counterpart is `FINAL`. The error is routed through the
// `errors` channel (not `warnings`), so the driver refuses to
// proceed to IR/codegen.

#[test]
fn final_method_callable_when_not_overridden() {
    // FINAL is just a constraint, not a behaviour change — a class
    // with a FINAL method should still work normally when nobody
    // tries to override it. Sub inherits Seal.imprint via vtable.
    expect(
        "final_method_callable_when_not_overridden",
        "CLASS Seal $(\n  FINAL FUNCTION imprint() = 7\n$)\nCLASS Sub EXTENDS Seal $(\n  DECL spare\n$)\nLET START() BE $(\n  LET s = NEW Sub\n  WRITEN(s.imprint())\n$)\n",
        "7",
    );
}

#[test]
fn final_method_override_rejected() {
    // Subclass tries to override a FINAL method — sema rejects with
    // a diagnostic naming both the method and the defining class.
    use newbcpl_tests::expect_reject;
    expect_reject(
        "final_method_override_rejected",
        "run",
        "CLASS Seal $(\n  FINAL FUNCTION imprint() = 7\n$)\nCLASS Sub EXTENDS Seal $(\n  FUNCTION imprint() = 9\n$)\nLET START() BE $(\n  LET s = NEW Sub\n  WRITEN(s.imprint())\n$)\n",
        "FINAL",
    );
}

#[test]
fn final_override_rejected_through_chain() {
    // Two-deep chain: Base has FINAL m, Mid extends Base, Sub
    // extends Mid and tries to override. The check walks the
    // ancestor chain, not just the direct parent.
    use newbcpl_tests::expect_reject;
    expect_reject(
        "final_override_rejected_through_chain",
        "run",
        "CLASS Base $(\n  FINAL FUNCTION m() = 1\n$)\nCLASS Mid EXTENDS Base $(\n  DECL placeholder\n$)\nCLASS Sub EXTENDS Mid $(\n  FUNCTION m() = 2\n$)\nLET START() BE $(\n  LET s = NEW Sub\n  WRITEN(s.m())\n$)\n",
        "FINAL",
    );
}

#[test]
fn non_final_override_still_allowed() {
    // A FINAL constraint on `m` doesn't accidentally lock down `n`.
    // Sub overrides only the non-FINAL method; sema accepts.
    expect(
        "non_final_override_still_allowed",
        "CLASS Base $(\n  FINAL FUNCTION sealed() = 1\n  FUNCTION open() = 2\n$)\nCLASS Sub EXTENDS Base $(\n  FUNCTION open() = 99\n$)\nLET START() BE $(\n  LET s = NEW Sub\n  WRITEN(s.sealed()) WRITES(\"*S\") WRITEN(s.open())\n$)\n",
        "1 99",
    );
}

// ─── Parameter type annotations `LET f(p AS Class)` ──────────────
//
// A function/routine parameter annotated `AS Class` binds the
// parameter with class identity from inside the body. This lets
// the receiver's methods and fields dispatch / visibility-check
// the same way a class-typed local does. Without the annotation
// the parameter is a bare word and methods fall back to dynamic
// vtable resolution; with it, sema can do the static work.
//
// The shape lives in `param_annotations: Vec<Option<String>>` on
// `FunctionDecl` / `RoutineDecl` / `ClassMethod`, parallel to the
// `params: Vec<String>` list. Sema's body-analysis pass reads the
// annotation through `class_name_from_annotation` and attaches the
// class identity to the parameter binding.

#[test]
fn function_param_as_class_dispatches_method() {
    // A function that takes a Point and reads its method —
    // dispatch should work because the param is class-typed.
    expect(
        "function_param_as_class_dispatches_method",
        "CLASS Point $(\n  DECL x\n  ROUTINE CREATE(ix) BE SELF.x := ix\n  FUNCTION value() = SELF.x\n$)\nLET show(p AS Point) = p.value()\nLET START() BE $(\n  LET q = NEW Point(123)\n  WRITEN(show(q))\n$)\n",
        "123",
    );
}

#[test]
fn routine_param_as_class_accesses_field() {
    // A routine takes a Point and reads/prints the bare field
    // (not via accessor method) — proves class identity flows
    // through enough to resolve `.x` to the right slot.
    expect(
        "routine_param_as_class_accesses_field",
        "CLASS Point $(\n  DECL x\n  ROUTINE CREATE(ix) BE SELF.x := ix\n$)\nLET dump(p AS Point) BE WRITEN(p.x)\nLET START() BE $(\n  LET q = NEW Point(55)\n  dump(q)\n$)\n",
        "55",
    );
}

#[test]
fn class_method_param_as_class_chains() {
    // Class method takes another class as a param and calls a
    // method on it — the chain `arg.method()` works the same way
    // it does for a local binding. Pins that param annotations
    // also work on methods, not just top-level routines.
    expect(
        "class_method_param_as_class_chains",
        "CLASS Inner $(\n  DECL value\n  ROUTINE CREATE(v) BE SELF.value := v\n  FUNCTION getValue() = SELF.value\n$)\nCLASS Outer $(\n  FUNCTION sum_with(other AS Inner) = other.getValue() + 1000\n$)\nLET START() BE $(\n  LET inner = NEW Inner(7)\n  LET outer = NEW Outer\n  WRITEN(outer.sum_with(inner))\n$)\n",
        "1007",
    );
}

#[test]
fn param_annotation_enforces_visibility() {
    // The class-typed param is subject to the same visibility
    // checks as a class-typed local. Trying to read a PRIVATE
    // field from a routine that has no class context — even with
    // an `AS Foo` param — is rejected by sema.
    use newbcpl_tests::expect_reject;
    expect_reject(
        "param_annotation_enforces_visibility",
        "run",
        "CLASS Foo $(\n  PRIVATE:\n  DECL secret\n  PUBLIC:\n  ROUTINE CREATE(s) BE SELF.secret := s\n$)\nLET peek(p AS Foo) = p.secret\nLET START() BE $(\n  LET f = NEW Foo(99)\n  WRITEN(peek(f))\n$)\n",
        "private",
    );
}

#[test]
fn param_without_annotation_workaround_via_typed_local() {
    // Pre-iteration workaround: when the param annotation form
    // wasn't yet wired, users assigned to a class-typed local
    // before calling. This still works (param_AS_Class is the
    // direct form, this is the indirect-via-local form).
    expect(
        "param_without_annotation_workaround_via_typed_local",
        "CLASS Box $(\n  DECL n\n  ROUTINE CREATE(i) BE SELF.n := i\n  FUNCTION peek() = SELF.n\n$)\nLET unbox(b) = VALOF $(\n  LET typed AS Box = b\n  RESULTIS typed.peek()\n$)\nLET START() BE $(\n  LET b = NEW Box(13)\n  WRITEN(unbox(b))\n$)\n",
        "13",
    );
}

// ─── Name-keyed dynamic dispatch (un-annotated receivers) ──────
//
// When sema / IR can't determine the receiver's static class — most
// commonly because the receiver is a routine parameter without an
// `AS Class` annotation — codegen emits an `IndirectMethodCall`
// that resolves through `__newbcpl_lookup_method` at runtime.
// The helper keys off the instance's inline vtable pointer (offset
// 0) into a process-global `(vtable_addr → method_names_addr)`
// registry the LLVM crate populates at JIT-finalize time. These
// probes pin the path end-to-end, including the polymorphic cases
// where the same `obj.method()` shape dispatches to different
// classes depending on what gets passed in.

#[test]
fn indirect_dispatch_resolves_method_on_untyped_param() {
    // Same source the corpus's many `param.method()` patterns hit.
    // Without runtime name-keyed dispatch this would crash with
    // `missing builtin: __newbcpl_indirect`; with it, the method
    // resolves through `__newbcpl_lookup_method`.
    expect(
        "indirect_dispatch_resolves_method_on_untyped_param",
        "CLASS Box $(\n  DECL n\n  ROUTINE CREATE(i) BE SELF.n := i\n  FUNCTION peek() = SELF.n\n$)\nLET unbox(b) = b.peek()\nLET START() BE $(\n  LET b = NEW Box(42)\n  WRITEN(unbox(b))\n$)\n",
        "42",
    );
}

#[test]
fn indirect_dispatch_routes_to_dynamic_class() {
    // The classic polymorphic shape: one helper, multiple classes,
    // each with a same-named method. Without static class info
    // the dispatch must route to the receiver's actual class.
    expect(
        "indirect_dispatch_routes_to_dynamic_class",
        "CLASS Cat $(\n  FUNCTION speak() = 100\n$)\nCLASS Dog $(\n  FUNCTION speak() = 200\n$)\nLET say(a) = a.speak()\nLET START() BE $(\n  LET c = NEW Cat\n  LET d = NEW Dog\n  WRITEN(say(c)) WRITES(\"*S\") WRITEN(say(d))\n$)\n",
        "100 200",
    );
}

#[test]
fn indirect_dispatch_passes_arguments() {
    // The method takes arguments — the IR's indirect path must
    // wire each through to the resolved function.
    expect(
        "indirect_dispatch_passes_arguments",
        "CLASS Adder $(\n  DECL base\n  ROUTINE CREATE(b) BE SELF.base := b\n  FUNCTION plus(x, y) = SELF.base + x + y\n$)\nLET sum_call(a) = a.plus(10, 20)\nLET START() BE $(\n  LET adder = NEW Adder(100)\n  WRITEN(sum_call(adder))\n$)\n",
        "130",
    );
}

#[test]
fn indirect_dispatch_works_in_routine_body() {
    // Routines with `BE stmt` (no return value) — the dispatch
    // path needs to handle the void-return case too.
    expect(
        "indirect_dispatch_works_in_routine_body",
        "GLOBAL trace = 0\nCLASS Setter $(\n  FUNCTION write(v) = VALOF $( trace := v\n RESULTIS 0 $)\n$)\nLET poke(s, v) BE s.write(v)\nLET START() BE $(\n  LET s = NEW Setter\n  poke(s, 99)\n  WRITEN(trace)\n$)\n",
        "99",
    );
}

// ─── Visibility enforcement (PUBLIC / PRIVATE / PROTECTED) ────────
//
// Sema rejects accesses that violate the declared visibility. The
// access-site's class identity (the class whose method body the
// access is happening inside) is checked against the member's
// defining class. PUBLIC always passes; PRIVATE requires identity;
// PROTECTED allows the defining class or any descendant. Top-level
// code (outside any class) can only reach PUBLIC members.

#[test]
fn public_field_accessible_from_outside() {
    expect(
        "public_field_accessible_from_outside",
        "CLASS P $(\n  PUBLIC:\n  DECL x\n  ROUTINE CREATE(ix) BE SELF.x := ix\n$)\nLET START() BE $(\n  LET p = NEW P(5)\n  WRITEN(p.x)\n$)\n",
        "5",
    );
}

#[test]
fn private_field_rejected_from_outside() {
    use newbcpl_tests::expect_reject;
    expect_reject(
        "private_field_rejected_from_outside",
        "run",
        "CLASS P $(\n  PRIVATE:\n  DECL secret\n  PUBLIC:\n  ROUTINE CREATE(s) BE SELF.secret := s\n$)\nLET START() BE $(\n  LET p = NEW P(42)\n  WRITEN(p.secret)\n$)\n",
        "private",
    );
}

#[test]
fn private_field_accessible_from_inside() {
    // Same class accessing its own private field — fine.
    expect(
        "private_field_accessible_from_inside",
        "CLASS P $(\n  PRIVATE:\n  DECL secret\n  PUBLIC:\n  ROUTINE CREATE(s) BE SELF.secret := s\n  FUNCTION reveal() = SELF.secret\n$)\nLET START() BE $(\n  LET p = NEW P(77)\n  WRITEN(p.reveal())\n$)\n",
        "77",
    );
}

#[test]
fn protected_field_rejected_from_outside() {
    use newbcpl_tests::expect_reject;
    expect_reject(
        "protected_field_rejected_from_outside",
        "run",
        "CLASS P $(\n  PROTECTED:\n  DECL value\n  PUBLIC:\n  ROUTINE CREATE(v) BE SELF.value := v\n$)\nLET START() BE $(\n  LET p = NEW P(1)\n  WRITEN(p.value)\n$)\n",
        "protected",
    );
}

#[test]
fn protected_field_accessible_in_subclass() {
    // Sub extends Base and reads Base's PROTECTED field from its
    // own method — allowed.
    expect(
        "protected_field_accessible_in_subclass",
        "CLASS Base $(\n  PROTECTED:\n  DECL value\n  PUBLIC:\n  ROUTINE CREATE(v) BE SELF.value := v\n$)\nCLASS Sub EXTENDS Base $(\n  PUBLIC:\n  FUNCTION peek() = SELF.value\n$)\nLET START() BE $(\n  LET s = NEW Sub(11)\n  WRITEN(s.peek())\n$)\n",
        "11",
    );
}

#[test]
fn private_method_rejected_from_outside() {
    use newbcpl_tests::expect_reject;
    expect_reject(
        "private_method_rejected_from_outside",
        "run",
        "CLASS P $(\n  PRIVATE:\n  FUNCTION helper() = 1\n  PUBLIC:\n  ROUTINE CREATE() BE $( $)\n$)\nLET START() BE $(\n  LET p = NEW P\n  WRITEN(p.helper())\n$)\n",
        "private",
    );
}

#[test]
fn private_method_callable_through_public_wrapper() {
    // Same class calling its own private method via a public
    // wrapper — fine. Demonstrates the standard helper-method
    // pattern.
    expect(
        "private_method_callable_through_public_wrapper",
        "CLASS P $(\n  PRIVATE:\n  FUNCTION compute() = 99\n  PUBLIC:\n  FUNCTION result() = SELF.compute()\n$)\nLET START() BE $(\n  LET p = NEW P\n  WRITEN(p.result())\n$)\n",
        "99",
    );
}
