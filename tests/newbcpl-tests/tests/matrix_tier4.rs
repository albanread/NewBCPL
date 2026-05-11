//! Tier 4 of `docs/test_matrix.md` — statements.
//!
//! Every control-flow construct in our BCPL dialect. Each probe
//! exercises one shape and asserts on a small output that
//! identifies which path the runtime actually took.

use newbcpl_tests::expect_stdout as expect;

// ─── Conditionals: IF / UNLESS / TEST ─────────────────────────────

#[test]
fn if_then_taken() {
    expect(
        "if_then_taken",
        "LET START() BE $( IF 1 = 1 THEN WRITES(\"y\") $)\n",
        "y",
    );
}

#[test]
fn if_then_skipped() {
    expect(
        "if_then_skipped",
        "LET START() BE $(\n  IF 1 = 2 THEN WRITES(\"y\")\n  WRITES(\"end\")\n$)\n",
        "end",
    );
}

#[test]
fn unless_inverts_condition() {
    // UNLESS = IF NOT — the body fires when the condition is
    // false.
    expect(
        "unless_inverts_condition",
        "LET START() BE $( UNLESS 1 = 2 DO WRITES(\"y\") $)\n",
        "y",
    );
}

#[test]
fn test_then_else_true() {
    // `TEST cond THEN body ELSE other` — always one or the
    // other, never neither, never both.
    expect(
        "test_then_else_true",
        "LET START() BE $( TEST 3 < 4 THEN WRITES(\"lt\") ELSE WRITES(\"ge\") $)\n",
        "lt",
    );
}

#[test]
fn test_then_else_false() {
    expect(
        "test_then_else_false",
        "LET START() BE $( TEST 4 < 3 THEN WRITES(\"lt\") ELSE WRITES(\"ge\") $)\n",
        "ge",
    );
}

#[test]
fn if_else_chain() {
    expect(
        "if_else_chain",
        "LET START() BE $(\n  IF 1 = 2 THEN WRITES(\"a\")\n  ELSE IF 2 = 2 THEN WRITES(\"b\")\n  ELSE WRITES(\"c\")\n$)\n",
        "b",
    );
}

// ─── Loops: WHILE / UNTIL ─────────────────────────────────────────

#[test]
fn while_iterates_until_false() {
    expect(
        "while_iterates_until_false",
        "LET START() BE $(\n  LET i = 0\n  WHILE i < 5 DO $(\n    WRITEN(i) WRITES(\"*S\")\n    i := i + 1\n  $)\n$)\n",
        "0 1 2 3 4 ",
    );
}

#[test]
fn while_zero_iterations() {
    // Body doesn't run if cond starts false.
    expect(
        "while_zero_iterations",
        "LET START() BE $(\n  WHILE 1 = 2 DO WRITES(\"x\")\n  WRITES(\"done\")\n$)\n",
        "done",
    );
}

#[test]
fn until_iterates_while_false() {
    expect(
        "until_iterates_while_false",
        "LET START() BE $(\n  LET i = 0\n  UNTIL i = 3 DO $(\n    WRITEN(i) WRITES(\"*S\")\n    i := i + 1\n  $)\n$)\n",
        "0 1 2 ",
    );
}

// ─── REPEAT family ────────────────────────────────────────────────

#[test]
fn repeat_while_runs_at_least_once() {
    // Do-while: body before test.
    expect(
        "repeat_while_runs_at_least_once",
        "LET START() BE $(\n  LET i = 0\n  $(\n    WRITEN(i) WRITES(\"*S\")\n    i := i + 1\n  $) REPEATWHILE i < 3\n$)\n",
        "0 1 2 ",
    );
}

#[test]
fn repeat_until_runs_at_least_once() {
    expect(
        "repeat_until_runs_at_least_once",
        "LET START() BE $(\n  LET i = 0\n  $(\n    WRITEN(i) WRITES(\"*S\")\n    i := i + 1\n  $) REPEATUNTIL i = 4\n$)\n",
        "0 1 2 3 ",
    );
}

// ─── FOR with step ────────────────────────────────────────────────

#[test]
fn for_default_step_is_one() {
    expect(
        "for_default_step_is_one",
        "LET START() BE $(\n  FOR i = 1 TO 4 DO $( WRITEN(i) WRITES(\"*S\") $)\n$)\n",
        "1 2 3 4 ",
    );
}

#[test]
fn for_explicit_step_by_two() {
    expect(
        "for_explicit_step_by_two",
        "LET START() BE $(\n  FOR i = 0 TO 10 BY 2 DO $( WRITEN(i) WRITES(\"*S\") $)\n$)\n",
        "0 2 4 6 8 10 ",
    );
}

#[test]
fn for_zero_iterations_if_start_above_end() {
    expect(
        "for_zero_iterations_if_start_above_end",
        "LET START() BE $(\n  FOR i = 5 TO 3 DO $( WRITEN(i) $)\n  WRITES(\"done\")\n$)\n",
        "done",
    );
}

// ─── BREAK and LOOP ───────────────────────────────────────────────

#[test]
fn break_exits_innermost_loop() {
    expect(
        "break_exits_innermost_loop",
        "LET START() BE $(\n  FOR i = 0 TO 100 DO $(\n    IF i = 3 THEN BREAK\n    WRITEN(i) WRITES(\"*S\")\n  $)\n  WRITES(\"end\")\n$)\n",
        "0 1 2 end",
    );
}

#[test]
fn loop_skips_to_next_iteration() {
    // `LOOP` is BCPL's continue.
    expect(
        "loop_skips_to_next_iteration",
        "LET START() BE $(\n  FOR i = 0 TO 5 DO $(\n    IF i = 2 THEN LOOP\n    WRITEN(i) WRITES(\"*S\")\n  $)\n$)\n",
        "0 1 3 4 5 ",
    );
}

#[test]
fn break_only_exits_inner_when_nested() {
    expect(
        "break_only_exits_inner_when_nested",
        "LET START() BE $(\n  FOR i = 0 TO 1 DO $(\n    FOR j = 0 TO 5 DO $(\n      IF j = 2 THEN BREAK\n      WRITEN(j) WRITES(\"*S\")\n    $)\n    WRITES(\"|*S\")\n  $)\n$)\n",
        "0 1 | 0 1 | ",
    );
}

// ─── VALOF / RESULTIS ─────────────────────────────────────────────

#[test]
fn valof_returns_resultis_value() {
    expect(
        "valof_returns_resultis_value",
        "LET pick(n) = VALOF $(\n  IF n < 10 THEN RESULTIS 100\n  RESULTIS 200\n$)\nLET START() BE $(\n  WRITEN(pick(3))\n  WRITES(\"*S\")\n  WRITEN(pick(30))\n$)\n",
        "100 200",
    );
}

#[test]
fn valof_short_circuits_after_resultis() {
    // RESULTIS exits the VALOF immediately; statements after
    // shouldn't run.
    expect(
        "valof_short_circuits_after_resultis",
        "LET get() = VALOF $(\n  RESULTIS 7\n  WRITES(\"unreachable\")\n  RESULTIS 99\n$)\nLET START() BE $( WRITEN(get()) $)\n",
        "7",
    );
}

// ─── SWITCHON ─────────────────────────────────────────────────────

#[test]
fn switchon_matches_case() {
    expect(
        "switchon_matches_case",
        "LET START() BE $(\n  LET n = 2\n  SWITCHON n INTO $(\n    CASE 1: WRITES(\"one\") ENDCASE\n    CASE 2: WRITES(\"two\") ENDCASE\n    CASE 3: WRITES(\"three\") ENDCASE\n    DEFAULT: WRITES(\"?\") ENDCASE\n  $)\n$)\n",
        "two",
    );
}

#[test]
fn switchon_falls_through_to_default() {
    expect(
        "switchon_falls_through_to_default",
        "LET START() BE $(\n  LET n = 99\n  SWITCHON n INTO $(\n    CASE 1: WRITES(\"one\") ENDCASE\n    DEFAULT: WRITES(\"def\") ENDCASE\n  $)\n$)\n",
        "def",
    );
}

#[test]
fn switchon_endcase_jumps_to_end() {
    // After ENDCASE inside CASE 2, control jumps to the end of
    // the SWITCHON block — not to CASE 3.
    expect(
        "switchon_endcase_jumps_to_end",
        "LET START() BE $(\n  SWITCHON 2 INTO $(\n    CASE 1: WRITES(\"a\") ENDCASE\n    CASE 2: WRITES(\"b\") ENDCASE\n    CASE 3: WRITES(\"c\") ENDCASE\n  $)\n  WRITES(\"end\")\n$)\n",
        "bend",
    );
}

// ─── GOTO + labels ────────────────────────────────────────────────

#[test]
fn forward_goto_skips_block() {
    expect(
        "forward_goto_skips_block",
        "LET START() BE $(\n  WRITES(\"a\")\n  GOTO done\n  WRITES(\"unreachable\")\n  done:\n  WRITES(\"b\")\n$)\n",
        "ab",
    );
}

// ─── Nested blocks ─────────────────────────────────────────────────

#[test]
fn nested_blocks_inherit_outer_scope() {
    expect(
        "nested_blocks_inherit_outer_scope",
        "LET START() BE $(\n  LET x = 10\n  $(\n    LET y = 20\n    WRITEN(x + y)\n  $)\n$)\n",
        "30",
    );
}

#[test]
fn nested_blocks_shadow_outer_name() {
    // Inner `x` shadows outer `x`; after the inner block ends
    // we see the outer again.
    expect(
        "nested_blocks_shadow_outer_name",
        "LET START() BE $(\n  LET x = 1\n  $(\n    LET x = 99\n    WRITEN(x) WRITES(\"*S\")\n  $)\n  WRITEN(x)\n$)\n",
        "99 1",
    );
}

// ─── Mixed-shape control flow ─────────────────────────────────────

#[test]
fn while_inside_if() {
    expect(
        "while_inside_if",
        "LET START() BE $(\n  IF 1 = 1 THEN $(\n    LET i = 0\n    WHILE i < 3 DO $( WRITEN(i) i := i + 1 $)\n  $)\n$)\n",
        "012",
    );
}

#[test]
fn if_inside_for() {
    expect(
        "if_inside_for",
        "LET START() BE $(\n  FOR i = 0 TO 5 DO $(\n    IF i = 3 THEN WRITES(\"!\") ELSE WRITEN(i)\n  $)\n$)\n",
        "012!45",
    );
}
