//! Extra probes across tiers — written declaratively with the
//! `probe!` / `probe_contains!` / `reject!` macros from
//! `newbcpl_tests`. Each line is one cell of the matrix.
//!
//! When the manifesto-driven probe-generator binary lands these
//! will move into a per-tier file emitted from a DSL. Today the
//! macro form is enough — adding a row is one line.

use newbcpl_tests::{probe, probe_contains, reject};

// ─── Tier 3 — expression edge cases ────────────────────────────────

probe!(
    int_zero_minus_negative_is_positive =>
    "LET START() BE $( WRITEN(0 - -5) $)\n" =>
    "5"
);

probe!(
    multiple_unary_negs_cancel =>
    "LET START() BE $( WRITEN(- - - 7) $)\n" =>
    "-7"
);

probe!(
    long_arithmetic_chain_left_associates =>
    "LET START() BE $( WRITEN(20 - 5 - 3 - 1) $)\n" =>
    "11"
);

probe!(
    int_div_truncates_toward_zero =>
    "LET START() BE $(\n  WRITEN(7 / 2)\n  WRITES(\"*S\")\n  WRITEN(-7 / 2)\n$)\n" =>
    "3 -3"
);

probe!(
    bit_shr_preserves_sign_bit =>
    "LET START() BE $(\n  WRITEN(-8 >> 1)\n$)\n" =>
    "-4"
);

probe!(
    bit_xor_via_neqv =>
    // BCPL spells bitwise XOR as `NEQV` (the bitwise
    // non-equivalence operator). `^` is reserved for the
    // pointer-type prefix in AS annotations.
    "LET START() BE $( WRITEN(12 NEQV 10) $)\n" =>
    "6"
);

probe!(
    relational_chained_in_arithmetic =>
    // BCPL's relational ops produce 0 / -1 (or 0 / 1; we
    // zero-extend the icmp result). The probe avoids that
    // ambiguity by branching, but here we use the value
    // directly to add to a constant.
    "LET START() BE $(\n  LET truth = (3 < 4) * 0 + 7\n  WRITEN(truth)\n$)\n" =>
    "7"
);

probe!(
    conditional_expr_inside_arithmetic =>
    "LET START() BE $(\n  LET x = 5\n  WRITEN((x > 0 -> x, 0 - x) + 100)\n$)\n" =>
    "105"
);

// ─── Tier 4 — control-flow edge cases ─────────────────────────────

probe!(
    nested_for_loops_iteration_count =>
    "LET START() BE $(\n  LET total = 0\n  FOR i = 1 TO 3 DO $(\n    FOR j = 1 TO 4 DO $(\n      total := total + 1\n    $)\n  $)\n  WRITEN(total)\n$)\n" =>
    "12"
);

probe!(
    while_with_compound_condition =>
    "LET START() BE $(\n  LET a = 0\n  LET b = 10\n  WHILE a < b DO $(\n    a := a + 1\n    b := b - 1\n  $)\n  WRITEN(a) WRITES(\"*S\") WRITEN(b)\n$)\n" =>
    "5 5"
);

probe!(
    for_loop_after_resultis_returns_correct_value =>
    // VALOF returns immediately on RESULTIS; a FOR inside it
    // up to the RESULTIS should accumulate normally.
    "LET sum_to(n) = VALOF $(\n  LET total = 0\n  FOR i = 1 TO n DO total := total + i\n  RESULTIS total\n$)\nLET START() BE $( WRITEN(sum_to(10)) $)\n" =>
    "55"
);

probe!(
    nested_switchon_dispatches_correctly =>
    "LET cat(n) = VALOF $(\n  SWITCHON n INTO $(\n    CASE 1: CASE 2: RESULTIS 100\n    CASE 3: RESULTIS 200\n    DEFAULT: RESULTIS 999\n  $)\n$)\nLET START() BE $(\n  WRITEN(cat(1)) WRITES(\"*S\")\n  WRITEN(cat(2)) WRITES(\"*S\")\n  WRITEN(cat(3)) WRITES(\"*S\")\n  WRITEN(cat(7))\n$)\n" =>
    "100 100 200 999"
);

probe!(
    if_then_else_inside_for_body =>
    "LET START() BE $(\n  LET sum = 0\n  FOR i = 1 TO 10 DO\n    IF i REM 2 = 0 THEN sum := sum + i\n    ELSE sum := sum - i\n  WRITEN(sum)\n$)\n" =>
    "5"
);

// ─── Tier 5 — class wiring corners ─────────────────────────────────

probe!(
    class_with_only_decl_fields_no_method =>
    "CLASS B $(\n  DECL a, b\n$)\nLET START() BE $(\n  LET x = NEW B\n  WRITES(\"ok\")\n$)\n" =>
    "ok"
);

probe!(
    method_taking_three_arguments =>
    // Single-arg and double-arg methods are covered in tier5;
    // this fills the 3-arg cell.
    "CLASS T $(\n  DECL a, b, c\n  ROUTINE CREATE(x, y, z) BE $( SELF.a := x\n SELF.b := y\n SELF.c := z $)\n  FUNCTION sum() = SELF.a + SELF.b + SELF.c\n$)\nLET START() BE $(\n  LET t = NEW T(11, 22, 33)\n  WRITEN(t.sum())\n$)\n" =>
    "66"
);

probe!(
    method_returning_class_field =>
    // The classic accessor pattern, isolated.
    "CLASS B $(\n  DECL v\n  ROUTINE CREATE(x) BE $( SELF.v := x $)\n  FUNCTION getV() = SELF.v\n$)\nLET START() BE $(\n  LET b = NEW B(123)\n  WRITEN(b.getV())\n$)\n" =>
    "123"
);

probe!(
    method_mutates_own_field_via_self =>
    "CLASS Counter $(\n  DECL n\n  ROUTINE CREATE() BE $( SELF.n := 0 $)\n  ROUTINE inc() BE $( SELF.n := SELF.n + 1 $)\n  FUNCTION get() = SELF.n\n$)\nLET START() BE $(\n  LET c = NEW Counter\n  c.inc()\n  c.inc()\n  c.inc()\n  WRITEN(c.get())\n$)\n" =>
    "3"
);

// ─── Tier 6 — runtime corners ──────────────────────────────────────

probe!(
    pairs_array_holds_pair_per_slot =>
    // Use `PAIRS(3)` with explicit parens — the bare
    // `PAIRS 3` form would parse as `LET ps = PAIRS` (a
    // function-reference binding) followed by `3` as an
    // orphan expression statement. Function calls require
    // parens in our grammar; only the typed-construct
    // keywords (VEC, PAIR, QUAD, LIST, ...) accept the
    // `KIND k` shape via their dedicated parser branches.
    "LET START() BE $(\n  LET ps = PAIRS(3)\n  ps!0 := PAIR(1, 2)\n  ps!1 := PAIR(3, 4)\n  ps!2 := PAIR(5, 6)\n  WRITEN(LEN(ps))\n$)\n" =>
    "3"
);

probe!(
    list_of_strings_holds_pointers =>
    // String literal pointers stored as list atoms with
    // ATOM_STRING tag.
    "LET START() BE $(\n  LET xs = LIST(\"first\", \"second\", \"third\")\n  WRITEN(LEN(xs))\n$)\n" =>
    "3"
);

probe!(
    gc_called_twice_in_a_row_is_idempotent =>
    "CLASS P $( DECL x\n  ROUTINE CREATE(ix) BE $( SELF.x := ix $)\n$)\nLET START() BE $(\n  LET p = NEW P(42)\n  GC()\n  GC()\n  GC()\n  WRITES(\"ok\")\n$)\n" =>
    "ok"
);

probe!(
    foreach_breaks_out_early =>
    // Tier 4 covers BREAK in FOR; this one confirms BREAK
    // works inside a FOREACH-list body too.
    "LET START() BE $(\n  LET xs = LIST(10, 20, 30, 40, 50)\n  FOREACH e IN xs DO $(\n    IF e = 30 THEN BREAK\n    WRITEN(e) WRITES(\"*S\")\n  $)\n  WRITES(\"end\")\n$)\n" =>
    "10 20 end"
);

// ─── Tier 7 — SIMD edge cases ──────────────────────────────────────

probe!(
    pair_with_zero_lane =>
    "LET START() BE $(\n  LET p = PAIR(0, 99)\n  WRITEN(p.|0|) WRITES(\"*S\")\n  WRITEN(p.|1|)\n$)\n" =>
    "0 99"
);

probe!(
    quad_with_mixed_signs =>
    "LET START() BE $(\n  LET q = QUAD(1, -2, 3, -4)\n  WRITEN(q.|0|) WRITES(\"*S\")\n  WRITEN(q.|1|) WRITES(\"*S\")\n  WRITEN(q.|2|) WRITES(\"*S\")\n  WRITEN(q.|3|)\n$)\n" =>
    "1 -2 3 -4"
);

probe!(
    pair_assignment_then_lane_read =>
    "LET START() BE $(\n  LET p = PAIR(0, 0)\n  p := PAIR(77, 88)\n  WRITEN(p.|0|) WRITES(\"*S\") WRITEN(p.|1|)\n$)\n" =>
    "77 88"
);

probe!(
    quad_lane_arithmetic =>
    // Extract lanes and add them — each lane is sign-extended
    // to i64 so the sum is a normal integer.
    "LET START() BE $(\n  LET q = QUAD(10, 20, 30, 40)\n  WRITEN(q.|0| + q.|1| + q.|2| + q.|3|)\n$)\n" =>
    "100"
);

probe!(
    pair_in_user_function_round_trip =>
    "LET swap(p) = PAIR(p.|1|, p.|0|)\nLET START() BE $(\n  LET p = PAIR(3, 7)\n  LET q = swap(p)\n  WRITEN(q.|0|) WRITES(\"*S\") WRITEN(q.|1|)\n$)\n" =>
    "7 3"
);

// ─── More Tier 1 negatives ────────────────────────────────────────

reject!(
    let_in_method_position_without_param_list =>
    // `LET name` at statement scope with nothing else is an
    // error — needs at least one initialiser.
    "LET START() BE $( LET a $)\n" =>
    "parse"
);

reject!(
    new_without_class_name_rejected =>
    "LET START() BE $( LET p = NEW $)\n" =>
    "parse"
);

reject!(
    case_outside_switchon_rejected =>
    "LET START() BE $( CASE 1: WRITES(\"x\") $)\n" =>
    "parse"
);

reject!(
    write_destination_without_value_rejected =>
    // `x :=` with nothing after the assign operator.
    "LET START() BE $(\n  LET x = 0\n  x :=\n$)\n" =>
    "parse"
);

// ─── Lowercase builtin spellings ──────────────────────────────────
//
// The user guide §1.1 says identifiers may be either case but
// "lower-case is the usual style". The runtime registers every
// builtin under both UPPERCASE and lowercase, so source written in
// the natural lowercase style resolves without the user knowing
// about a casing convention.

#[test]
fn lowercase_writes_resolves() {
    newbcpl_tests::expect_stdout(
        "lowercase_writes_resolves",
        "LET START() BE $( writes(\"ok\") $)\n",
        "ok",
    );
}

#[test]
fn lowercase_writen_resolves() {
    newbcpl_tests::expect_stdout(
        "lowercase_writen_resolves",
        "LET START() BE $( writen(42) $)\n",
        "42",
    );
}

#[test]
fn lowercase_writef_arity_dispatch() {
    // `writef` needs the arity-aware WRITEF1..7 trick to work
    // through the lowercase alias. The IR-side resolver matches
    // the name case-insensitively and picks the correct
    // arity-suffixed entry point.
    newbcpl_tests::expect_stdout(
        "lowercase_writef_arity_dispatch",
        "LET START() BE $( writef(\"%d %d*N\", 7, 11) $)\n",
        "7 11\n",
    );
}
