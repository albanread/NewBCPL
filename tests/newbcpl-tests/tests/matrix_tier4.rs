//! Tier 4 of `docs/test_matrix.md` — statements.
//!
//! Every control-flow construct in our BCPL dialect. Each probe
//! exercises one shape and asserts on a small output that
//! identifies which path the runtime actually took.

use newbcpl_tests::{
    expect_reject, expect_stdout as expect, expect_stdout_and_stderr_contains,
};

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

// ─── GOTO and labels ──────────────────────────────────────────────
//
// `GOTO label` is BCPL's unconditional jump; `name:` declares a
// label. Rare in modern code but the parser lowers them and the
// audit flagged them as probe-thin.

#[test]
fn goto_forward_jumps_over_code() {
    expect(
        "goto_forward_jumps_over_code",
        "LET START() BE $(\n  WRITES(\"before*N\")\n  GOTO skip\n  WRITES(\"skipped*N\")\nskip:\n  WRITES(\"after*N\")\n$)\n",
        "before\nafter\n",
    );
}

#[test]
fn goto_into_loop_body() {
    // GOTO into the middle of a loop body. Reaches the label,
    // then falls through to the next iteration.
    expect(
        "goto_into_loop_body",
        "LET START() BE $(\n  LET n = 0\n  GOTO mid\n  WRITES(\"unreached*N\")\nmid:\n  WRITEN(n)\n$)\n",
        "0",
    );
}

// ─── GLOBAL declarations ──────────────────────────────────────────
//
// `GLOBAL name = expr` (single) and `GLOBAL $( name = expr; ... $)`
// (block) declare module-scope bindings backed by LLVM module-level
// globals. Reads/writes route through `@<name>`; cross-routine
// visibility is the headline property.

#[test]
fn global_single_form_writes_visible_in_start() {
    expect(
        "global_single_form_writes_visible_in_start",
        "GLOBAL counter = 0\nLET START() BE $(\n  counter := 42\n  WRITEN(counter)\n$)\n",
        "42",
    );
}

#[test]
fn global_block_form_writes_visible_in_start() {
    expect(
        "global_block_form_writes_visible_in_start",
        "GLOBAL $(\n  a = 1\n  b = 2\n$)\nLET START() BE $(\n  a := 10\n  b := 20\n  WRITEN(a) WRITES(\"*S\") WRITEN(b)\n$)\n",
        "10 20",
    );
}

#[test]
fn global_seen_from_separate_routine() {
    // A GLOBAL is visible from any routine in the module — that's
    // the point. The increment routine modifies the slot;
    // START reads it.
    expect(
        "global_seen_from_separate_routine",
        "GLOBAL count = 0\nLET bump() BE count := count + 1\nLET START() BE $(\n  bump()\n  bump()\n  bump()\n  WRITEN(count)\n$)\n",
        "3",
    );
}

#[test]
fn globals_slot_form_rejected() {
    // Plural `GLOBALS` (classic slot-vector form) is not supported.
    // Parse error pointing users at `GLOBAL`.
    expect_reject(
        "globals_slot_form_rejected",
        "run",
        "GLOBALS $( wrch : 8 $)\nLET START() BE WRITES(\"hi\")\n",
        "GLOBALS",
    );
}

#[test]
fn global_colon_slot_syntax_rejected() {
    // The `name : K` slot form is the GLOBALS shape. Even under
    // `GLOBAL`, we reject `:` so users don't accidentally rely on
    // unimplemented slot pinning.
    expect_reject(
        "global_colon_slot_syntax_rejected",
        "run",
        "GLOBAL $( foo : 8 $)\nLET START() BE WRITES(\"hi\")\n",
        "slot-pinning",
    );
}

// ─── GET directive ────────────────────────────────────────────────
//
// `GET "name"` is textual-by-spirit, AST-by-implementation: each
// declaration in the included file is spliced into the consumer at
// the GET site. Two resolution paths — sibling file (relative to
// the GET-issuing source) and modules-active fallback (the same
// folder runtime symbol resolution uses). Cycle protection via a
// depth cap.

#[test]
fn get_pulls_manifest_from_sibling_file() {
    use std::fs;
    use std::path::PathBuf;
    let dir = std::env::temp_dir().join("newbcpl-get-sibling");
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("mkdir");
    let header: PathBuf = dir.join("constants.bcl");
    fs::write(&header, "MANIFEST $( PI = 314 $)\n").expect("write header");
    let main: PathBuf = dir.join("main.bcl");
    fs::write(
        &main,
        "GET \"constants.bcl\"\nLET START() BE $( WRITEN(PI) $)\n",
    )
    .expect("write main");
    let output = std::process::Command::new(newbcpl_tests::driver_path())
        .arg("run")
        .arg(&main)
        .output()
        .expect("spawn");
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(
        output.status.success(),
        "GET sibling run failed\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}"
    );
    assert!(
        stdout.contains("314"),
        "expected `314` in stdout, got: {stdout}"
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn get_pulls_manifest_from_modules_active() {
    use std::fs;
    use std::path::PathBuf;
    let root = std::env::temp_dir().join("newbcpl-get-modules");
    let _ = fs::remove_dir_all(&root);
    let modules = root.join("modules-active");
    fs::create_dir_all(&modules).expect("mkdir modules");
    let header: PathBuf = modules.join("sharedconstants.bcl");
    fs::write(&header, "MANIFEST $( ANSWER = 42 $)\n").expect("write header");
    let main: PathBuf = root.join("main.bcl");
    fs::write(
        &main,
        "GET \"sharedconstants\"\nLET START() BE $( WRITEN(ANSWER) $)\n",
    )
    .expect("write main");
    let output = std::process::Command::new(newbcpl_tests::driver_path())
        .arg("run")
        .arg(&main)
        .env("NEWBCPL_MODULES_ACTIVE", &modules)
        .output()
        .expect("spawn");
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(
        output.status.success(),
        "GET modules-active run failed\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}"
    );
    assert!(
        stdout.contains("42"),
        "expected `42` in stdout, got: {stdout}"
    );
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn get_missing_file_rejected() {
    expect_reject(
        "get_missing_file_rejected",
        "run",
        "GET \"does_not_exist.bcl\"\nLET START() BE WRITES(\"hi\")\n",
        "file not found",
    );
}

// ─── Mutual recursion via `LET … AND …` ──────────────────────────
//
// Two surface forms work today:
//   1. Consecutive top-level `LET name(...) = body` declarations —
//      forward references resolve through sema's pre-pass 2.
//   2. Classical `LET name(...) = body AND name(...) = body` chains
//      — the parser disambiguates `AND` followed by `<ident> (` as
//      a declaration-tail and unfolds the chain into independent
//      top-level decls. Semantically identical to form 1.
//
// Form 2 is the classical BCPL syntax and the form Martin Richards'
// reference programs use; form 1 is what the manifesto-era code
// adopted. Both round-trip through the same IR.

#[test]
fn mutual_recursion_via_consecutive_lets_terminates() {
    // Form 1 — consecutive LETs.
    // is_even / is_odd ping-pong toward 0. For n=5 we expect
    // is_even=0 (5 is odd), is_odd=1.
    expect(
        "mutual_recursion_via_consecutive_lets_terminates",
        "LET is_even(n) = n = 0 -> 1, is_odd(n - 1)\nLET is_odd(n) = n = 0 -> 0, is_even(n - 1)\nLET START() BE $(\n  WRITEN(is_even(0)) WRITES(\"*S\")\n  WRITEN(is_even(4)) WRITES(\"*S\")\n  WRITEN(is_even(5)) WRITES(\"*S\")\n  WRITEN(is_odd(5))\n$)\n",
        "1 1 0 1",
    );
}

#[test]
fn mutual_recursion_routines_with_be_bodies() {
    // Form 1 with ROUTINE bodies (`BE stmt`) and a shared GLOBAL.
    expect(
        "mutual_recursion_routines_with_be_bodies",
        "GLOBAL trace = 0\nLET step_a(n) BE $(\n  trace := trace + 100 + n\n  IF n > 0 THEN step_b(n - 1)\n$)\nLET step_b(n) BE $(\n  trace := trace + n\n  IF n > 0 THEN step_a(n - 1)\n$)\nLET START() BE $(\n  step_a(2)\n  WRITEN(trace)\n$)\n",
        "203",
    );
}

#[test]
fn classical_let_and_chain_two_functions() {
    // Form 2 — classical `LET f(n) = e AND g(n) = e` chain.
    // The parser disambiguates the `AND <ident> (` shape as
    // declaration-tail and emits two `Decl::Function` entries.
    // Same is_even / is_odd ping-pong as the consecutive-LETs probe.
    expect(
        "classical_let_and_chain_two_functions",
        "LET is_even(n) = n = 0 -> 1, is_odd(n - 1)\nAND is_odd(n) = n = 0 -> 0, is_even(n - 1)\nLET START() BE $(\n  WRITEN(is_even(0)) WRITES(\"*S\")\n  WRITEN(is_even(4)) WRITES(\"*S\")\n  WRITEN(is_even(5)) WRITES(\"*S\")\n  WRITEN(is_odd(5))\n$)\n",
        "1 1 0 1",
    );
}

#[test]
fn classical_let_and_chain_three_routines() {
    // Three-deep chain — exercise the loop in
    // `consume_mutual_recursion_chain`.
    expect(
        "classical_let_and_chain_three_routines",
        "GLOBAL out = 0\nLET a(n) BE $( out := out + 1\n  IF n > 0 THEN b(n - 1) $)\nAND b(n) BE $( out := out + 10\n  IF n > 0 THEN c(n - 1) $)\nAND c(n) BE $( out := out + 100\n  IF n > 0 THEN a(n - 1) $)\nLET START() BE $(\n  a(3)\n  WRITEN(out)\n$)\n",
        // a→b→c→a: 1, 10, 100, 1 = 112
        "112",
    );
}

#[test]
fn classical_let_and_chain_mixes_function_and_routine() {
    // Mixed bodies inside one chain — one `=` function and one
    // `BE` routine. The two halves of the chain dispatch through
    // each other.
    expect(
        "classical_let_and_chain_mixes_function_and_routine",
        "GLOBAL collected = 0\nLET note(n) BE collected := collected + n\nLET sum_via(n) = n = 0 -> 0, n + sum_via(n - 1)\nAND tag_via(n) BE $( note(n)\n  IF n > 0 THEN tag_via(n - 1) $)\nLET START() BE $(\n  WRITEN(sum_via(4))\n  WRITES(\"*S\")\n  tag_via(3)\n  WRITEN(collected)\n$)\n",
        // sum_via(4) = 4+3+2+1 = 10; tag_via accumulates 3+2+1+0 = 6
        "10 6",
    );
}

#[test]
fn expression_and_still_works_when_not_followed_by_paren() {
    // Regression: `expr AND ident` (no `(` follow) must still
    // parse as logical AND, not be mistaken for a decl tail.
    // Pins that the disambiguation is keyed on the three-token
    // `AND <ident> (` shape.
    expect(
        "expression_and_still_works_when_not_followed_by_paren",
        "LET START() BE $(\n  LET a = 1\n  LET b = 1\n  TEST a AND b THEN WRITES(\"both\") ELSE WRITES(\"not\")\n$)\n",
        "both",
    );
}


// ─── BRK — debugger-breakpoint state dump ─────────────────────────
//
// `BRK` is the classical BCPL debugger hint. Our runtime synthesises
// a signal-safe-ish state dump: routine name + source line, heap
// summary, full AMD64 register set, and a stack walk via
// `RtlVirtualUnwind` (we already register unwind tables from the JIT
// memory manager). Output goes to stderr — stdout still carries the
// program's own WRITES output — and the program continues after the
// BRK statement.

#[test]
fn brk_emits_banner_with_routine_name_and_line() {
    expect_stdout_and_stderr_contains(
        "brk_emits_banner_with_routine_name_and_line",
        "LET START() BE $(\n  WRITES(\"before*N\")\n  BRK\n  WRITES(\"after*N\")\n$)\n",
        "before\nafter\n",
        &[
            "=== BRK in routine `START`",
            "at line 3",
            "=== END BRK ===",
        ],
    );
}

#[test]
fn brk_emits_heap_summary() {
    // The heap section always emits, even if live=0.
    expect_stdout_and_stderr_contains(
        "brk_emits_heap_summary",
        "LET START() BE $(\n  BRK\n  WRITES(\"ok\")\n$)\n",
        "ok",
        &["heap:", "live=", "blocks=", "peak="],
    );
}

#[test]
fn brk_emits_register_state() {
    // The context section reports RIP/RSP/RBP plus the GPRs and
    // flags. We assert on the section header and a couple of the
    // register names — actual values are unstable.
    expect_stdout_and_stderr_contains(
        "brk_emits_register_state",
        "LET START() BE $(\n  BRK\n  WRITES(\"ok\")\n$)\n",
        "ok",
        &["context:", "rip=", "rsp=", "rax=", "r15="],
    );
}

#[test]
fn brk_emits_stack_walk() {
    // The stack section walks frames via RtlVirtualUnwind. We
    // can't assert on specific frame addresses, but we assert at
    // least one frame entry was emitted.
    expect_stdout_and_stderr_contains(
        "brk_emits_stack_walk",
        "LET START() BE $(\n  BRK\n  WRITES(\"ok\")\n$)\n",
        "ok",
        &["stack:", "#0", "rip="],
    );
}

#[test]
fn brk_reports_routine_name_from_helper() {
    // BRK fires inside a helper routine, not START. The banner
    // should name the helper so the user sees where the snapshot
    // was taken, not just where the program lives.
    expect_stdout_and_stderr_contains(
        "brk_reports_routine_name_from_helper",
        "LET helper() BE $(\n  WRITES(\"in helper*N\")\n  BRK\n$)\nLET START() BE $(\n  helper()\n  WRITES(\"done*N\")\n$)\n",
        "in helper\ndone\n",
        &["=== BRK in routine `helper`"],
    );
}

#[test]
fn brk_does_not_halt_program() {
    // BRK is a snapshot, not FINISH — the program continues and
    // exits 0. Helper checks that by demanding stdout has output
    // that only appears *after* the BRK statement.
    expect_stdout_and_stderr_contains(
        "brk_does_not_halt_program",
        "LET START() BE $(\n  WRITEN(1)\n  BRK\n  WRITEN(2)\n  BRK\n  WRITEN(3)\n$)\n",
        "123",
        &["=== BRK", "=== END BRK ==="],
    );
}

#[test]
fn brk_stack_frame_resolves_routine_name() {
    // After Phase A's JIT-symbol registration, frames in JIT-d code
    // resolve to BCPL routine names. helper calls into BRK; the
    // stack walk should report at least one frame "in helper" and
    // at least one "in START". Other frames (host driver, OS) stay
    // as raw RIPs.
    expect_stdout_and_stderr_contains(
        "brk_stack_frame_resolves_routine_name",
        "LET helper() BE $(\n  BRK\n$)\nLET START() BE $(\n  helper()\n  WRITES(\"ok\")\n$)\n",
        "ok",
        &["in helper", "in START"],
    );
}

#[test]
fn brk_two_deep_call_chain_names_each_frame() {
    // Three nested routines, BRK in the innermost. All three names
    // should appear in the stack walk so the user can trace the
    // call chain from the BRK site upward.
    expect_stdout_and_stderr_contains(
        "brk_two_deep_call_chain_names_each_frame",
        "LET inner() BE BRK\nLET middle() BE inner()\nLET START() BE $(\n  middle()\n  WRITES(\"ok\")\n$)\n",
        "ok",
        &["in inner", "in middle", "in START"],
    );
}

#[test]
fn get_cycle_rejected() {
    // Two files that GET each other — the depth cap fires.
    use std::fs;
    let dir = std::env::temp_dir().join("newbcpl-get-cycle");
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("mkdir");
    fs::write(dir.join("a.bcl"), "GET \"b.bcl\"\n").expect("write a");
    fs::write(dir.join("b.bcl"), "GET \"a.bcl\"\n").expect("write b");
    let main = dir.join("main.bcl");
    fs::write(&main, "GET \"a.bcl\"\nLET START() BE WRITES(\"unreached\")\n")
        .expect("write main");
    let output = std::process::Command::new(newbcpl_tests::driver_path())
        .arg("run")
        .arg(&main)
        .output()
        .expect("spawn");
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(
        !output.status.success(),
        "expected cycle to fail; stderr: {stderr}"
    );
    assert!(
        stderr.contains("cyclic include") || stderr.contains("nesting exceeded"),
        "expected cycle diagnostic, got: {stderr}"
    );
    let _ = fs::remove_dir_all(&dir);
}
