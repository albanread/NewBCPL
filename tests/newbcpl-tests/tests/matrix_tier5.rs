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
