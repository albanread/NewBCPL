//! Tier 2 of `docs/test_matrix.md` — sema positives.
//!
//! Each probe targets a sema rule by its observable consequence:
//! the wrong type hint would route to the wrong codegen op
//! (integer add instead of float add, vec-length helper instead
//! of list-length helper, etc.), so a regression here surfaces as
//! a stdout mismatch even though sema itself has no user-visible
//! output.
//!
//! Tier 1 covers REJECTION; this covers ACCEPTANCE + correct
//! downstream behaviour. The split is deliberate — sema bugs that
//! make a valid program produce garbage are different in kind from
//! sema bugs that fail to flag malformed input.

use newbcpl_tests::expect_stdout as expect;

// ─── FLET float inference (manifesto §1) ──────────────────────────

#[test]
fn flet_with_float_literals() {
    // `FLET` carries the float hint forward. Initialiser must
    // itself be float-typed; sema's FLET-override changes
    // the binding's hint but doesn't coerce an int literal
    // value at store time (that's a separate sema gap — the
    // matrix has no probe for it yet, but `FLOAT(n)` is the
    // explicit coercion that does work).
    expect(
        "flet_with_float_literals",
        "LET START() BE $(\n  FLET x = 5.0\n  FLET y = 2.0\n  FWRITE(x +. y)\n$)\n",
        "7",
    );
}

#[test]
fn flet_coerces_int_literal_to_float() {
    // `FLET x = 5` — the FLET binding's hint overrides Int→Float.
    // Store-time coercion in emit (sitofp i64→f64) lets the slot
    // round-trip the value as a float.
    expect(
        "flet_coerces_int_literal_to_float",
        "LET START() BE $(\n  FLET x = 5\n  FWRITE(x)\n$)\n",
        "5",
    );
}

#[test]
fn flet_chain_propagates_float() {
    expect(
        "flet_chain_propagates_float",
        "LET START() BE $(\n  FLET a = 1.5\n  FLET b = a *. 2.0\n  FLET c = b +. 0.5\n  FWRITE(c)\n$)\n",
        "3.5",
    );
}

// ─── MANIFEST constants (compile-time substitution) ──────────────

#[test]
fn manifest_substitutes_into_arithmetic() {
    // `MANIFEST { SIZE = 10 }` — every reference to SIZE
    // becomes the literal 10 at lower time. Sema must record
    // the value; lowering substitutes inline.
    expect(
        "manifest_substitutes_into_arithmetic",
        "MANIFEST { SIZE = 10 }\nLET START() BE $(\n  WRITEN(SIZE * SIZE)\n$)\n",
        "100",
    );
}

#[test]
fn manifest_drives_vec_allocation_size() {
    // The classic use: `LET v = VEC SIZE` where SIZE comes
    // from a MANIFEST. Both the allocation and `LEN(v)` see
    // the substituted value.
    expect(
        "manifest_drives_vec_allocation_size",
        "MANIFEST { SIZE = 7 }\nLET START() BE $(\n  LET v = VEC SIZE\n  WRITEN(LEN(v))\n$)\n",
        "7",
    );
}

#[test]
fn multiple_manifests_in_one_block() {
    expect(
        "multiple_manifests_in_one_block",
        "MANIFEST { LOW = 1\n MID = 50\n HIGH = 99 }\nLET START() BE $(\n  WRITEN(LOW) WRITES(\"*S\")\n  WRITEN(MID) WRITES(\"*S\")\n  WRITEN(HIGH)\n$)\n",
        "1 50 99",
    );
}

#[test]
fn manifest_arithmetic_constants_fold_at_lower_time() {
    // MANIFEST values are substituted as integer literals;
    // arithmetic on them is regular runtime IR addition,
    // not a sema-level constant fold. Verifies the right
    // value flows through, not the fold step.
    expect(
        "manifest_arithmetic_constants_fold_at_lower_time",
        "MANIFEST { A = 3\n B = 4 }\nLET START() BE $( WRITEN(A * A + B * B) $)\n",
        "25",
    );
}

// ─── AS annotation overrides inferred hint (manifesto §2) ────────

#[test]
fn as_integer_annotation_compiles() {
    // `LET x AS INTEGER = expr` — the annotation hint should
    // win; we just need the program to compile and print the
    // right value (sema doesn't surface the hint visibly).
    expect(
        "as_integer_annotation_compiles",
        "LET START() BE $(\n  LET x AS INTEGER = 42\n  WRITEN(x + 1)\n$)\n",
        "43",
    );
}

#[test]
fn as_pointer_annotation_compiles() {
    // `LET p AS ^STRING = \"hello\"`. The `^` is a
    // POINTER-TO marker; sema strips it and keeps STRING as
    // the base hint.
    expect(
        "as_pointer_annotation_compiles",
        "LET START() BE $(\n  LET p AS ^STRING = \"hello\"\n  WRITES(p)\n$)\n",
        "hello",
    );
}

#[test]
fn as_list_of_integer_annotation_compiles() {
    // The reference's nested generic shape:
    // `^LIST OF INTEGER`. Sema reads pointer levels with `^`,
    // then strips ` OF X` element tail.
    expect(
        "as_list_of_integer_annotation_compiles",
        "LET START() BE $(\n  LET xs AS ^LIST OF INTEGER = LIST(1, 2, 3)\n  WRITEN(HD(xs))\n  WRITES(\"*S\")\n  WRITEN(HD(TL(xs)))\n$)\n",
        "1 2",
    );
}

#[test]
fn valof_as_integer_annotation_accepted() {
    // The `VALOF AS Type $(...)` shape from
    // `test_visibility.bcl` / `Ttestmap2.bcl`. Sema accepts
    // the annotation; ignoring it doesn't change semantics
    // because RESULTIS carries the runtime value.
    expect(
        "valof_as_integer_annotation_accepted",
        "LET sq(n) = VALOF AS INTEGER $( RESULTIS n * n $)\nLET START() BE $(\n  WRITEN(sq(9))\n$)\n",
        "81",
    );
}

// ─── Scope rules ──────────────────────────────────────────────────

#[test]
fn nested_let_inherits_outer_bindings() {
    expect(
        "nested_let_inherits_outer_bindings",
        "LET START() BE $(\n  LET outer = 100\n  $(\n    LET inner = 5\n    WRITEN(outer + inner)\n  $)\n$)\n",
        "105",
    );
}

#[test]
fn inner_let_shadows_outer_within_block() {
    expect(
        "inner_let_shadows_outer_within_block",
        "LET START() BE $(\n  LET x = 1\n  $(\n    LET x = 99\n    WRITEN(x) WRITES(\"*S\")\n  $)\n  WRITEN(x)\n$)\n",
        "99 1",
    );
}

#[test]
fn for_loop_variable_is_block_scoped() {
    // FOR's `i` is scoped to the loop body; outer `i` keeps
    // its original value after the loop exits.
    expect(
        "for_loop_variable_is_block_scoped",
        "LET START() BE $(\n  LET i = 999\n  FOR i = 0 TO 3 DO $( WRITEN(i) WRITES(\"*S\") $)\n  WRITEN(i)\n$)\n",
        "0 1 2 3 999",
    );
}

#[test]
fn function_locals_dont_leak_into_caller() {
    expect(
        "function_locals_dont_leak_into_caller",
        "LET helper() = VALOF $( LET temp = 42\n RESULTIS temp $)\nLET START() BE $(\n  LET temp = 5\n  WRITEN(helper()) WRITES(\"*S\")\n  WRITEN(temp)\n$)\n",
        "42 5",
    );
}

// ─── Class field type hints flowing through accesses ─────────────

#[test]
fn class_field_word_default_holds_int() {
    // `DECL x` reserves a word slot; sema's default hint for
    // a class field is Word. Writes / reads through methods
    // preserve the value.
    expect(
        "class_field_word_default_holds_int",
        "CLASS B $(\n  DECL n\n  ROUTINE CREATE(v) BE $( SELF.n := v $)\n  FUNCTION get() = SELF.n\n$)\nLET START() BE $(\n  LET b = NEW B(31415)\n  WRITEN(b.get())\n$)\n",
        "31415",
    );
}

#[test]
fn new_propagates_class_hint_to_let_binding() {
    // The `class_name_of_expr` propagation: `LET p = NEW Foo`
    // remembers `p`'s class so `p.field` resolves through the
    // class layout. Without this, `.x` field access wouldn't
    // find the right offset.
    expect(
        "new_propagates_class_hint_to_let_binding",
        "CLASS Pt $(\n  DECL x\n  ROUTINE CREATE(v) BE $( SELF.x := v $)\n$)\nLET START() BE $(\n  LET p = NEW Pt(77)\n  WRITEN(p.x)\n$)\n",
        "77",
    );
}

#[test]
fn let_alias_propagates_class_hint() {
    // `LET q = p` where p is OBJECT[Foo] should carry the
    // class forward — otherwise q.x would have no layout to
    // look up. Sema's `class_name_of_expr(Ident)` handles
    // this by looking up the source binding's class.
    expect(
        "let_alias_propagates_class_hint",
        "CLASS Pt $(\n  DECL x\n  ROUTINE CREATE(v) BE $( SELF.x := v $)\n$)\nLET START() BE $(\n  LET p = NEW Pt(88)\n  LET q = p\n  WRITEN(q.x)\n$)\n",
        "88",
    );
}

#[test]
fn self_carries_class_in_method_body() {
    // Inside a method body, `SELF` and bare-field references
    // both resolve through the class layout. Verifying both
    // forms produce the same offsets.
    expect(
        "self_carries_class_in_method_body",
        "CLASS Pt $(\n  DECL x, y\n  ROUTINE CREATE(a, b) BE $( SELF.x := a\n y := b $)\n  FUNCTION sum() = SELF.x + y\n$)\nLET START() BE $(\n  LET p = NEW Pt(10, 20)\n  WRITEN(p.sum())\n$)\n",
        "30",
    );
}

// ─── List hint for FOREACH dispatch ───────────────────────────────

#[test]
fn list_constructor_hint_picks_list_foreach() {
    // FOREACH dispatches on `iter.hint()`. A `LIST(...)`
    // initialiser must come back with TypeHint::List so the
    // linked-walk path runs, not the index-walk path. The
    // observable signal: FOREACH on a LIST visits every
    // element in head→tail order without crashing on
    // `__newbcpl_len` reading vec-conventions.
    expect(
        "list_constructor_hint_picks_list_foreach",
        "LET START() BE $(\n  LET xs = LIST(10, 20, 30, 40)\n  FOREACH e IN xs DO $( WRITEN(e) WRITES(\"*S\") $)\n$)\n",
        "10 20 30 40 ",
    );
}

#[test]
fn vec_constructor_hint_picks_vec_foreach() {
    // Mirror of the above for VEC.
    expect(
        "vec_constructor_hint_picks_vec_foreach",
        "LET START() BE $(\n  LET v = VEC 3\n  v!0 := 100\n  v!1 := 200\n  v!2 := 300\n  FOREACH e IN v DO $( WRITEN(e) WRITES(\"*S\") $)\n$)\n",
        "100 200 300 ",
    );
}

// ─── User-function return type flows back to the call site ────────

#[test]
fn user_function_int_return_used_in_arithmetic() {
    // Sema infers the user fn's return hint from the body's
    // expression. Caller adds the result to an integer; if
    // the hint were wrong, the add would route through the
    // float path and the output bit pattern would shift.
    expect(
        "user_function_int_return_used_in_arithmetic",
        "LET seven() = 7\nLET START() BE $( WRITEN(seven() + 100) $)\n",
        "107",
    );
}

#[test]
fn user_function_float_return_used_in_arithmetic() {
    expect(
        "user_function_float_return_used_in_arithmetic",
        "LET pi() = 3.14\nLET START() BE $( FWRITE(pi() *. 2.0) $)\n",
        "6.28",
    );
}

// ─── Multiple bindings in one LET ─────────────────────────────────

#[test]
fn parallel_let_bindings_evaluate_left_to_right() {
    // `LET a, b = 1, 2`. Names parallel-bind to RHS exprs.
    // Sema must declare both names with the right types.
    expect(
        "parallel_let_bindings_evaluate_left_to_right",
        "LET START() BE $(\n  LET a, b = 1, 2\n  WRITEN(a + b)\n$)\n",
        "3",
    );
}

#[test]
fn parallel_let_destructures_pair() {
    // The 1-RHS-N-names destructuring shape. Sema flags it
    // via `LetDecl.destructure` so lower knows to lane-unpack.
    expect(
        "parallel_let_destructures_pair",
        "LET START() BE $(\n  LET p = PAIR(101, 202)\n  LET a, b = p\n  WRITEN(a) WRITES(\"*S\") WRITEN(b)\n$)\n",
        "101 202",
    );
}
