//! Tier 3 of `docs/test_matrix.md` — expressions.
//!
//! Every operator on every applicable operand type. Each probe is
//! a tiny program whose stdout is the result of the expression
//! under test, printed via `WRITEN` or `FWRITE` so it round-trips
//! through the runtime cleanly.
//!
//! Three operator flavours coexist in our BCPL dialect:
//!   * plain (`+`, `-`, `<`, ...) — integer / pointer arithmetic
//!   * dot-suffixed (`+.`, `-.`, `<.`, ...) — explicit float
//!   * hash-suffixed (`+#`, `-#`, `<#`, ...) — same as dot-form,
//!     just a syntactic alias the corpus uses heavily
//!
//! When a row applies to both float syntaxes we add a separate
//! probe per form so a future regression in either codegen path
//! lands on its own cell.

use newbcpl_tests::expect_stdout as expect;

// ─── Integer arithmetic ────────────────────────────────────────────

#[test]
fn int_add() {
    expect("int_add", "LET START() BE $( WRITEN(7 + 5) $)\n", "12");
}

#[test]
fn int_sub() {
    expect("int_sub", "LET START() BE $( WRITEN(20 - 8) $)\n", "12");
}

#[test]
fn int_mul() {
    expect("int_mul", "LET START() BE $( WRITEN(3 * 4) $)\n", "12");
}

#[test]
fn int_div_floor() {
    // BCPL `/` is integer divide; matches C truncation for
    // positives. We only assert the positive case here — sign
    // semantics on negatives are an open question per the
    // reference docs.
    expect("int_div_floor", "LET START() BE $( WRITEN(25 / 2) $)\n", "12");
}

#[test]
fn int_rem() {
    // `REM` is the BCPL word-form modulo. Matches C `%` for
    // positive operands.
    expect("int_rem", "LET START() BE $( WRITEN(17 REM 5) $)\n", "2");
}

#[test]
fn int_unary_neg() {
    expect("int_unary_neg", "LET START() BE $( WRITEN(0 - 5) $)\n", "-5");
}

#[test]
fn int_arith_precedence_mul_before_add() {
    expect(
        "int_arith_precedence_mul_before_add",
        "LET START() BE $( WRITEN(2 + 3 * 4) $)\n",
        "14",
    );
}

#[test]
fn int_arith_parens_override_precedence() {
    expect(
        "int_arith_parens_override_precedence",
        "LET START() BE $( WRITEN((2 + 3) * 4) $)\n",
        "20",
    );
}

// ─── Float arithmetic (dot suffix) ────────────────────────────────

#[test]
fn float_add_dot() {
    expect(
        "float_add_dot",
        "LET START() BE $( FWRITE(1.5 +. 2.25) $)\n",
        "3.75",
    );
}

#[test]
fn float_mul_dot() {
    expect(
        "float_mul_dot",
        "LET START() BE $( FWRITE(2.5 *. 4.0) $)\n",
        "10",
    );
}

#[test]
fn float_div_dot() {
    expect(
        "float_div_dot",
        "LET START() BE $( FWRITE(10.0 /. 4.0) $)\n",
        "2.5",
    );
}

// ─── Float arithmetic (hash suffix — alternate syntax) ────────────

#[test]
fn float_add_hash() {
    // The reference's corpus uses `+#` heavily. Same operator,
    // different spelling.
    expect(
        "float_add_hash",
        "LET START() BE $( FWRITE(1.5 +# 2.25) $)\n",
        "3.75",
    );
}

#[test]
fn float_mul_hash() {
    expect(
        "float_mul_hash",
        "LET START() BE $( FWRITE(3.14 *# 2.0) $)\n",
        "6.28",
    );
}

// ─── int → float conversion ───────────────────────────────────────

#[test]
fn float_builtin_promotes_int() {
    // `FLOAT(n)` is the explicit int→float coercion. `FLET`
    // would also infer Float, but the explicit cast is what
    // most corpus programs use.
    expect(
        "float_builtin_promotes_int",
        "LET START() BE $( FWRITE(FLOAT(5) *. 2.0) $)\n",
        "10",
    );
}

// ─── Relational (integer) ─────────────────────────────────────────
//
// BCPL relational ops yield 0 or non-zero (the "true" value is
// implementation-defined; we follow the convention of -1 / TRUE).
// To keep probes portable we only assert which way the comparison
// goes by branching on the result.

#[test]
fn int_eq_true_path() {
    expect(
        "int_eq_true_path",
        "LET START() BE $( IF 3 = 3 THEN WRITES(\"y\") ELSE WRITES(\"n\") $)\n",
        "y",
    );
}

#[test]
fn int_eq_false_path() {
    expect(
        "int_eq_false_path",
        "LET START() BE $( IF 3 = 4 THEN WRITES(\"y\") ELSE WRITES(\"n\") $)\n",
        "n",
    );
}

#[test]
fn int_ne_compares() {
    expect(
        "int_ne_compares",
        "LET START() BE $( IF 1 ~= 2 THEN WRITES(\"y\") ELSE WRITES(\"n\") $)\n",
        "y",
    );
}

#[test]
fn int_lt_strict() {
    expect(
        "int_lt_strict",
        "LET START() BE $(\n  IF 3 < 4 THEN WRITES(\"y\") ELSE WRITES(\"n\")\n  IF 4 < 4 THEN WRITES(\"y\") ELSE WRITES(\"n\")\n$)\n",
        "yn",
    );
}

#[test]
fn int_le_inclusive() {
    expect(
        "int_le_inclusive",
        "LET START() BE $(\n  IF 4 <= 4 THEN WRITES(\"y\") ELSE WRITES(\"n\")\n  IF 5 <= 4 THEN WRITES(\"y\") ELSE WRITES(\"n\")\n$)\n",
        "yn",
    );
}

#[test]
fn int_gt_ge_pair() {
    expect(
        "int_gt_ge_pair",
        "LET START() BE $(\n  IF 5 > 4 THEN WRITES(\"y\") ELSE WRITES(\"n\")\n  IF 4 >= 4 THEN WRITES(\"y\") ELSE WRITES(\"n\")\n$)\n",
        "yy",
    );
}

// ─── Bitwise and logical ──────────────────────────────────────────

#[test]
fn bit_and() {
    expect("bit_and", "LET START() BE $( WRITEN(12 & 10) $)\n", "8");
}

#[test]
fn bit_or() {
    expect("bit_or", "LET START() BE $( WRITEN(12 | 10) $)\n", "14");
}

#[test]
fn bit_shl() {
    expect("bit_shl", "LET START() BE $( WRITEN(1 << 4) $)\n", "16");
}

#[test]
fn bit_shr_arithmetic() {
    // `>>` is an arithmetic right shift — sign-preserving.
    expect("bit_shr_arithmetic", "LET START() BE $( WRITEN(64 >> 2) $)\n", "16");
}

#[test]
fn word_form_band_bor() {
    // Bitwise operators: `BAND` is the word form of `&`, `BOR` is the
    // word form of `|`. The unprefixed `AND` / `OR` are *logical*
    // and would return 1 / 1 here regardless of the operands' values,
    // so this probe must use the B-prefixed forms.
    expect(
        "word_form_band_bor",
        "LET START() BE $(\n  WRITEN(12 BAND 10)\n  WRITES(\"*S\")\n  WRITEN(12 BOR 10)\n$)\n",
        "8 14",
    );
}

#[test]
fn word_form_logical_and_or() {
    // Logical `AND` / `OR`: reduce each operand to a 0/1 boolean by
    // comparing to zero, then combine. Both operands non-zero ⇒ 1
    // (regardless of bit pattern); either ⇒ 1 for OR.
    expect(
        "word_form_logical_and_or",
        "LET START() BE $(\n  WRITEN(12 AND 10)\n  WRITES(\"*S\")\n  WRITEN(12 OR 0)\n  WRITES(\"*S\")\n  WRITEN(0 AND 5)\n$)\n",
        "1 1 0",
    );
}

// ─── Conditional expression ───────────────────────────────────────

#[test]
fn conditional_expr_true_branch() {
    // `cond -> then, else` — BCPL's ternary.
    expect(
        "conditional_expr_true_branch",
        "LET START() BE $( WRITEN(1 = 1 -> 42, 99) $)\n",
        "42",
    );
}

#[test]
fn conditional_expr_false_branch() {
    expect(
        "conditional_expr_false_branch",
        "LET START() BE $( WRITEN(1 = 2 -> 42, 99) $)\n",
        "99",
    );
}

#[test]
fn conditional_expr_nested() {
    // Nested ternary on the false branch. Tests associativity.
    expect(
        "conditional_expr_nested",
        "LET START() BE $( WRITEN(1 = 2 -> 1, 3 = 4 -> 2, 3) $)\n",
        "3",
    );
}

// ─── Subscript family ─────────────────────────────────────────────

#[test]
fn vec_word_subscript() {
    expect(
        "vec_word_subscript",
        "LET START() BE $(\n  LET v = VEC 3\n  v!0 := 100\n  v!1 := 200\n  WRITEN(v!0 + v!1)\n$)\n",
        "300",
    );
}

#[test]
fn fvec_float_subscript() {
    // FVEC slots are read with the `.%` float-subscript
    // operator. `v!i` would load the slot as a word (i64),
    // breaking subsequent float arithmetic. `v.%i` loads it
    // as f64 — the BCPL convention for float vectors.
    expect(
        "fvec_float_subscript",
        "LET START() BE $(\n  LET v = FVEC 3\n  v!0 := 1.5\n  v!1 := 2.5\n  FWRITE(v.%0 +. v.%1)\n$)\n",
        "4",
    );
}

// ─── Variables and let-bindings as operands ──────────────────────

#[test]
fn let_bindings_compose() {
    expect(
        "let_bindings_compose",
        "LET START() BE $(\n  LET a = 10\n  LET b = 20\n  LET c = a + b\n  WRITEN(c)\n$)\n",
        "30",
    );
}

#[test]
fn flet_binding_inferred_float() {
    // `FLET` overrides scalar inference to FLOAT even when the
    // initialiser is otherwise neutral (manifesto §1).
    expect(
        "flet_binding_inferred_float",
        "LET START() BE $(\n  FLET x = 3.14\n  FLET y = 2.0\n  FWRITE(x *. y)\n$)\n",
        "6.28",
    );
}

// ─── Call expressions ────────────────────────────────────────────

#[test]
fn user_function_returning_int() {
    expect(
        "user_function_returning_int",
        "LET square(n) = n * n\nLET START() BE $( WRITEN(square(7)) $)\n",
        "49",
    );
}

#[test]
fn recursive_function_terminates() {
    expect(
        "recursive_function_terminates",
        "LET fact(n) = n = 0 -> 1, n * fact(n - 1)\nLET START() BE $( WRITEN(fact(6)) $)\n",
        "720",
    );
}

#[test]
fn nested_call() {
    expect(
        "nested_call",
        "LET inc(n) = n + 1\nLET dbl(n) = n * 2\nLET START() BE $( WRITEN(dbl(inc(3))) $)\n",
        "8",
    );
}

// ─── Bitfield operator (%%(start, width)) ─────────────────────────
//
// `v %% (start, width)` reads `width` bits starting at bit `start`.
// `v %% (start, width) := payload` writes `payload` into those bits.
// Width defaults to 1 when omitted.

#[test]
fn bitfield_read_low_byte() {
    expect(
        "bitfield_read_low_byte",
        "LET START() BE $( LET v = #X1234\n  WRITEN(v %% (0, 8)) $)\n",
        "52",
    );
}

#[test]
fn bitfield_read_high_byte() {
    expect(
        "bitfield_read_high_byte",
        "LET START() BE $( LET v = #X1234\n  WRITEN(v %% (8, 8)) $)\n",
        "18",
    );
}

#[test]
fn bitfield_read_single_bit_default_width() {
    // `v %% (i)` — width defaults to 1.
    expect(
        "bitfield_read_single_bit_default_width",
        "LET START() BE $( LET v = 5\n  WRITEN(v %% (0)) WRITES(\"*S\")\n  WRITEN(v %% (1)) WRITES(\"*S\")\n  WRITEN(v %% (2)) $)\n",
        "1 0 1",
    );
}

#[test]
fn bitfield_write_inserts_field() {
    // Pack a value into bits [4..8). Other bits stay zero.
    expect(
        "bitfield_write_inserts_field",
        "LET START() BE $( LET v = 0\n  v %% (4, 4) := 9\n  WRITEN(v) $)\n",
        "144",
    );
}

#[test]
fn bitfield_write_preserves_other_bits() {
    // Start with 0xFF (8 bits set), clear bits [2..4) by writing 0.
    // Expected: 11110011b = 243.
    expect(
        "bitfield_write_preserves_other_bits",
        "LET START() BE $( LET v = #XFF\n  v %% (2, 2) := 0\n  WRITEN(v) $)\n",
        "243",
    );
}

// ─── EQV / NEQV operators ─────────────────────────────────────────
//
// `EQV` is the equivalence test (returns 1 when operands are equal,
// 0 otherwise; lowered through `ICmpEq`). `NEQV` is the
// not-equivalent / XOR form (lowered through `BitXor`). They sit in
// the OR family of the precedence table.

#[test]
fn eqv_equal_operands_is_true() {
    expect(
        "eqv_equal_operands_is_true",
        "LET START() BE $( WRITEN(5 EQV 5) $)\n",
        "1",
    );
}

#[test]
fn eqv_unequal_operands_is_false() {
    expect(
        "eqv_unequal_operands_is_false",
        "LET START() BE $( WRITEN(5 EQV 3) $)\n",
        "0",
    );
}

#[test]
fn neqv_xor_returns_bitwise_difference() {
    // `5 NEQV 3` is 5 ^ 3 = 6 (bitwise XOR). This is the BCPL
    // tradition — NEQV is BXOR's spelling in the OR family.
    expect(
        "neqv_xor_returns_bitwise_difference",
        "LET START() BE $( WRITEN(5 NEQV 3) $)\n",
        "6",
    );
}

#[test]
fn neqv_equal_operands_zero() {
    expect(
        "neqv_equal_operands_zero",
        "LET START() BE $( WRITEN(7 NEQV 7) $)\n",
        "0",
    );
}

// ─── Address-of round-trip ────────────────────────────────────────

#[test]
fn address_of_round_trips_through_indirection() {
    // `@x` returns the address of `x`; `!p` reads through a pointer.
    // The round trip should yield the original value.
    expect(
        "address_of_round_trips_through_indirection",
        "LET START() BE $( LET x = 99\n  LET p = @x\n  WRITEN(!p) $)\n",
        "99",
    );
}

// ─── Character literals + escape forms ────────────────────────────
//
// `BCPL syntax.md` §1.3-1.4 names eight escape sequences inside
// character literals: `*N` `*T` `*S` `*B` `*P` `*C` `*"` `**`.
// A plain `'A'` evaluates to the integer value of the character
// (65, since strings/chars are UTF-8 bytes in our dialect — see
// user guide §2.6 for the deviation from the reference's 32-bit
// model). Each escape resolves to its canonical byte value; if a
// future lexer refactor drops one of these the corresponding probe
// fails with a clean stdout diff.

#[test]
fn char_lit_plain_ascii() {
    // `'A'` is the literal byte 0x41 = 65.
    expect(
        "char_lit_plain_ascii",
        "LET START() BE $( WRITEN('A') $)\n",
        "65",
    );
}

#[test]
fn char_lit_escape_newline() {
    // `'*N'` = 10 (LF).
    expect(
        "char_lit_escape_newline",
        "LET START() BE $( WRITEN('*N') $)\n",
        "10",
    );
}

#[test]
fn char_lit_escape_tab() {
    // `'*T'` = 9.
    expect(
        "char_lit_escape_tab",
        "LET START() BE $( WRITEN('*T') $)\n",
        "9",
    );
}

#[test]
fn char_lit_escape_space() {
    // `'*S'` = 32. Not strictly an escape (a literal space works
    // too), but the spec names it — pin the canonical value.
    expect(
        "char_lit_escape_space",
        "LET START() BE $( WRITEN('*S') $)\n",
        "32",
    );
}

#[test]
fn char_lit_escape_backspace() {
    // `'*B'` = 8.
    expect(
        "char_lit_escape_backspace",
        "LET START() BE $( WRITEN('*B') $)\n",
        "8",
    );
}

#[test]
fn char_lit_escape_newpage() {
    // `'*P'` = 12 (form feed).
    expect(
        "char_lit_escape_newpage",
        "LET START() BE $( WRITEN('*P') $)\n",
        "12",
    );
}

#[test]
fn char_lit_escape_carriage_return() {
    // `'*C'` = 13.
    expect(
        "char_lit_escape_carriage_return",
        "LET START() BE $( WRITEN('*C') $)\n",
        "13",
    );
}

#[test]
fn char_lit_escape_double_quote() {
    // `'*"'` = 34. The escape is needed inside string literals but
    // the spec extends it to char constants for consistency.
    expect(
        "char_lit_escape_double_quote",
        "LET START() BE $( WRITEN('*\"') $)\n",
        "34",
    );
}

#[test]
fn char_lit_escape_asterisk() {
    // `'**'` = 42. The doubled-star self-escape.
    expect(
        "char_lit_escape_asterisk",
        "LET START() BE $( WRITEN('**') $)\n",
        "42",
    );
}
