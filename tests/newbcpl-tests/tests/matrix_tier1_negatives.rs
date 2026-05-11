//! Tier 1 of `docs/test_matrix.md` — lexical & syntactic
//! REJECTION corpus. Each probe is a small malformed `.bcl`
//! fragment paired with a diagnostic substring that the lexer /
//! parser must produce.
//!
//! Why this tier matters: a sema rule isn't real until something
//! enforces it. The "you must reject this" probes are the dual
//! of the positive corpus — without them, a future refactor could
//! quietly start accepting bad input and we'd find out the hard
//! way (a confused codegen panic, or worse, a successful run that
//! computes garbage).
//!
//! The runner uses `expect_reject(name, subcommand, source,
//! stderr_substring)` with the `run` subcommand. `run`
//! surfaces parse / sema errors as a non-zero exit + stderr
//! diagnostic — `dump-ast` / `dump-tokens` put the error in
//! their stdout dump artifact instead, which can't fail the
//! subprocess. Each probe checks that:
//!
//!   1. the subprocess exits non-zero,
//!   2. its stderr contains the expected diagnostic substring.
//!
//! The substring need not be the whole message — partial match
//! keeps the probes robust against wording tweaks.

use newbcpl_tests::expect_reject;

// ─── Lexer rejections ──────────────────────────────────────────────

#[test]
fn unterminated_string_rejected() {
    // Unclosed double-quote string literal. The lexer must
    // surface a clear "unterminated" diagnostic before the
    // parser sees an incoherent token stream.
    expect_reject(
        "unterminated_string_rejected",
        "run",
        "LET START() BE $( WRITES(\"hello $)\n",
        "newline in string",
    );
}

// ─── Parser rejections ────────────────────────────────────────────

#[test]
fn empty_let_has_no_initialiser_rejected() {
    // `LET name` with no `=` and no comma is meaningless at
    // statement / module scope (inside a class it would be a
    // field declaration, but at this scope it's malformed).
    expect_reject(
        "empty_let_has_no_initialiser_rejected",
        "run",
        "LET START() BE $( LET x $)\n",
        "parse",
    );
}

#[test]
fn class_header_without_body_rejected() {
    // `CLASS Foo` with no body bracket. Reported as
    // "expected `$(` or `{` after CLASS header".
    expect_reject(
        "class_header_without_body_rejected",
        "run",
        "CLASS Foo\nLET START() BE $( $)\n",
        "CLASS header",
    );
}

#[test]
fn routine_without_body_rejected() {
    // `LET START()` with no `=` and no `BE` after the
    // parameter list. Reported as "expected `=` or `BE` after
    // parameter list".
    expect_reject(
        "routine_without_body_rejected",
        "run",
        "LET START()\n",
        "parameter list",
    );
}

#[test]
fn foreach_without_in_rejected() {
    // `FOREACH name DO ...` (missing the `IN iter` clause).
    expect_reject(
        "foreach_without_in_rejected",
        "run",
        "LET START() BE $(\n  LET xs = LIST(1, 2)\n  FOREACH e DO $( WRITEN(e) $)\n$)\n",
        "FOREACH",
    );
}

#[test]
fn unbalanced_bcpl_brackets_rejected() {
    // Opening `$(` without a matching `$)`. The block parser
    // must complain rather than silently accept the
    // truncated body.
    expect_reject(
        "unbalanced_bcpl_brackets_rejected",
        "run",
        "LET START() BE $( WRITES(\"oops\")\n",
        "parse",
    );
}

#[test]
fn test_keyword_without_else_rejected() {
    // `TEST cond THEN body` requires an `ELSE` branch — that's
    // the whole reason `TEST` exists alongside `IF`. The error
    // message names the rule explicitly.
    expect_reject(
        "test_keyword_without_else_rejected",
        "run",
        "LET START() BE $( TEST 1 = 1 THEN WRITES(\"yes\") $)\n",
        "ELSE",
    );
}

#[test]
fn switchon_without_block_rejected() {
    // `SWITCHON expr INTO` must be followed by `$(` or `{`.
    expect_reject(
        "switchon_without_block_rejected",
        "run",
        "LET START() BE $( SWITCHON 1 INTO CASE 1: WRITES(\"a\") $)\n",
        "SWITCHON",
    );
}

#[test]
fn let_count_mismatch_rejected() {
    // `LET a, b, c = expr1, expr2` — three names, two
    // expressions, not a 1→N destructuring shape. Real
    // mismatch that the parser must flag.
    expect_reject(
        "let_count_mismatch_rejected",
        "run",
        "LET START() BE $( LET a, b, c = 1, 2 $)\n",
        "LET binding",
    );
}

#[test]
fn for_without_to_clause_rejected() {
    // `FOR i = 0` with no `TO end` — incomplete loop header.
    expect_reject(
        "for_without_to_clause_rejected",
        "run",
        "LET START() BE $( FOR i = 0 DO WRITES(\"x\") $)\n",
        "TO",
    );
}

#[test]
fn class_member_unknown_kind_rejected() {
    // Inside a class, only `DECL` / `LET` / `FLET` / `ROUTINE`
    // / `FUNCTION` / visibility tags are valid members.
    // Anything else must produce the "expected class member"
    // diagnostic.
    expect_reject(
        "class_member_unknown_kind_rejected",
        "run",
        "CLASS Foo $(\n  WHILE 1 DO $( $)\n$)\nLET START() BE $( $)\n",
        "class member",
    );
}

#[test]
fn virtual_without_method_keyword_rejected() {
    // `VIRTUAL` and `FINAL` prefix method declarations only.
    // `VIRTUAL DECL x` makes no sense.
    expect_reject(
        "virtual_without_method_keyword_rejected",
        "run",
        "CLASS Foo $(\n  VIRTUAL DECL x\n$)\nLET START() BE $( $)\n",
        "VIRTUAL",
    );
}

#[test]
fn manifest_without_bindings_rejected() {
    // `MANIFEST` requires a `$(` / `{` block of bindings.
    expect_reject(
        "manifest_without_bindings_rejected",
        "run",
        "MANIFEST\nLET START() BE $( $)\n",
        "parse",
    );
}

#[test]
fn comma_separated_targets_need_assign_rejected() {
    // `a, b, c` at statement position with no `:=` is a list
    // of expressions evaluated for nothing — not a valid
    // statement. The parser surfaces this as
    // "expected `:=` after comma-separated targets".
    expect_reject(
        "comma_separated_targets_need_assign_rejected",
        "run",
        "LET START() BE $(\n  LET a = 1\n  LET b = 2\n  a, b\n$)\n",
        ":=",
    );
}
