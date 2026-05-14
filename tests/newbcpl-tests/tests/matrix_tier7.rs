//! Tier 7 of `docs/test_matrix.md` — SIMD lane types.
//!
//! The widths in `docs/pair_and_multilane_types.md` are
//! normative. These probes lock in:
//!
//!   * PAIR / FPAIR / QUAD / OCT — pack into a single i64 word;
//!     lane access uses sign-aware shift-extract.
//!   * FQUAD — `<4 x f32>`, lane access via `extractelement`.
//!   * FOREACH-destructuring on lists of these types unpacks the
//!     lanes per iteration.
//!
//! Each probe constructs a value, accesses one or more lanes, and
//! prints what came back. The output is small and stable.

use newbcpl_tests::expect_stdout as expect;

// ─── PAIR construction + lane access ──────────────────────────────

#[test]
fn pair_construct_and_extract_lane_zero() {
    expect(
        "pair_construct_and_extract_lane_zero",
        "LET START() BE $(\n  LET p = PAIR(10, 20)\n  WRITEN(p.|0|)\n$)\n",
        "10",
    );
}

#[test]
fn pair_extract_lane_one() {
    expect(
        "pair_extract_lane_one",
        "LET START() BE $(\n  LET p = PAIR(10, 20)\n  WRITEN(p.|1|)\n$)\n",
        "20",
    );
}

#[test]
fn pair_negative_lane_sign_extends() {
    // PAIR lanes are 32-bit *signed* — the unpack must
    // sign-extend, not zero-extend.
    expect(
        "pair_negative_lane_sign_extends",
        "LET START() BE $(\n  LET p = PAIR(-3, -7)\n  WRITEN(p.|0|)\n  WRITES(\"*S\")\n  WRITEN(p.|1|)\n$)\n",
        "-3 -7",
    );
}

#[test]
fn pair_zero_lanes_round_trip() {
    expect(
        "pair_zero_lanes_round_trip",
        "LET START() BE $(\n  LET p = PAIR(0, 0)\n  WRITEN(p.|0|)\n  WRITES(\"*S\")\n  WRITEN(p.|1|)\n$)\n",
        "0 0",
    );
}

#[test]
fn pair_extreme_values_at_lane_boundary() {
    // The i32 lane bounds — confirms truncation packs them and
    // sign-extension unpacks them.
    expect(
        "pair_extreme_values_at_lane_boundary",
        "LET START() BE $(\n  LET p = PAIR(2147483647, -2147483648)\n  WRITEN(p.|0|) WRITES(\"*S\") WRITEN(p.|1|)\n$)\n",
        "2147483647 -2147483648",
    );
}

// ─── PAIR via LET destructure ─────────────────────────────────────

#[test]
fn pair_let_destructure() {
    // `LET a, b = pair_expr` is the destructuring shape — same
    // unpack-lanes machinery FOREACH uses.
    expect(
        "pair_let_destructure",
        "LET START() BE $(\n  LET p = PAIR(31, 41)\n  LET a, b = p\n  WRITEN(a) WRITES(\"*S\") WRITEN(b)\n$)\n",
        "31 41",
    );
}

// ─── QUAD construction + lane access ──────────────────────────────

#[test]
fn quad_construct_and_extract_each_lane() {
    expect(
        "quad_construct_and_extract_each_lane",
        "LET START() BE $(\n  LET q = QUAD(1, 2, 3, 4)\n  WRITEN(q.|0|) WRITES(\"*S\")\n  WRITEN(q.|1|) WRITES(\"*S\")\n  WRITEN(q.|2|) WRITES(\"*S\")\n  WRITEN(q.|3|)\n$)\n",
        "1 2 3 4",
    );
}

#[test]
fn quad_negative_lane_sign_extends() {
    // QUAD lanes are 16-bit signed.
    expect(
        "quad_negative_lane_sign_extends",
        "LET START() BE $(\n  LET q = QUAD(-1, -2, -3, -4)\n  WRITEN(q.|0|) WRITES(\"*S\")\n  WRITEN(q.|1|) WRITES(\"*S\")\n  WRITEN(q.|2|) WRITES(\"*S\")\n  WRITEN(q.|3|)\n$)\n",
        "-1 -2 -3 -4",
    );
}

// ─── OCT construction + lane access ───────────────────────────────

#[test]
fn oct_construct_and_extract_lanes() {
    expect(
        "oct_construct_and_extract_lanes",
        "LET START() BE $(\n  LET o = OCT(1, 2, 3, 4, 5, 6, 7, 8)\n  WRITEN(o.|0|) WRITES(\"*S\")\n  WRITEN(o.|3|) WRITES(\"*S\")\n  WRITEN(o.|7|)\n$)\n",
        "1 4 8",
    );
}

#[test]
fn oct_negative_lane_sign_extends() {
    // OCT lanes are 8-bit signed.
    expect(
        "oct_negative_lane_sign_extends",
        "LET START() BE $(\n  LET o = OCT(-1, -2, -3, -4, -5, -6, -7, -8)\n  WRITEN(o.|0|) WRITES(\"*S\")\n  WRITEN(o.|7|)\n$)\n",
        "-1 -8",
    );
}

// ─── FOREACH destructuring on a list of pairs ─────────────────────

#[test]
fn foreach_pair_destructure_walks_chain() {
    // `FOREACH (a, b) IN list-of-pairs` — the unpack-lanes
    // path tied through the list-walker. Mirrors the corpus's
    // `test_foreach_destructuring.bcl`.
    expect(
        "foreach_pair_destructure_walks_chain",
        "LET START() BE $(\n  LET points = LIST(PAIR(10, 20), PAIR(30, 40), PAIR(50, 60))\n  FOREACH (x, y) IN points DO $(\n    WRITEN(x) WRITES(\"*S\")\n    WRITEN(y) WRITES(\"|*S\")\n  $)\n$)\n",
        "10 20| 30 40| 50 60| ",
    );
}

#[test]
fn foreach_pair_destructure_empty_list() {
    expect(
        "foreach_pair_destructure_empty_list",
        "LET START() BE $(\n  LET empty = LIST()\n  FOREACH (x, y) IN empty DO $( WRITES(\"unreachable\") $)\n  WRITES(\"done\")\n$)\n",
        "done",
    );
}

// ─── PAIR field inside a class ────────────────────────────────────

#[test]
fn class_field_holding_pair_round_trips() {
    // A PAIR stored into a class field, read back through a
    // method. Exercises the field-store / field-load path with
    // a packed-i64 SIMD value.
    expect(
        "class_field_holding_pair_round_trips",
        "CLASS Box $(\n  DECL p\n  ROUTINE CREATE(ip) BE $( SELF.p := ip $)\n  FUNCTION first() = SELF.p.|0|\n  FUNCTION second() = SELF.p.|1|\n$)\nLET START() BE $(\n  LET b = NEW Box(PAIR(7, 11))\n  WRITEN(b.first()) WRITES(\"*S\")\n  WRITEN(b.second())\n$)\n",
        "7 11",
    );
}

// ─── Lane access with computed (runtime) index ────────────────────

#[test]
fn pair_runtime_lane_index_zero() {
    // Lane index is a runtime expression, not a constant. Our
    // emit's lane-extract uses `(packed << pad) >> (pad +
    // low_drop)` where the shift amounts come from the lane
    // index — so it must work for non-constant indices too.
    expect(
        "pair_runtime_lane_index_zero",
        "LET START() BE $(\n  LET p = PAIR(42, 84)\n  LET i = 0\n  WRITEN(p.|i|)\n$)\n",
        "42",
    );
}

#[test]
fn pair_runtime_lane_index_one() {
    expect(
        "pair_runtime_lane_index_one",
        "LET START() BE $(\n  LET p = PAIR(42, 84)\n  LET i = 1\n  WRITEN(p.|i|)\n$)\n",
        "84",
    );
}

// ─── Lane writes ───────────────────────────────────────────────────
//
// `pair.|i| := value` produces a new packed value identical to the
// old one except lane `i` is replaced. The lvalue can be a binding
// (write back to the slot) or a SELF-relative class field.

#[test]
fn pair_lane_write_constant_index() {
    expect(
        "pair_lane_write_constant_index",
        "LET START() BE $(\n  LET p = PAIR(10, 20)\n  p.|0| := 33\n  WRITEN(p.|0|) WRITES(\"*S\") WRITEN(p.|1|)\n$)\n",
        "33 20",
    );
}

#[test]
fn pair_lane_write_high_lane() {
    expect(
        "pair_lane_write_high_lane",
        "LET START() BE $(\n  LET p = PAIR(10, 20)\n  p.|1| := 99\n  WRITEN(p.|0|) WRITES(\"*S\") WRITEN(p.|1|)\n$)\n",
        "10 99",
    );
}

#[test]
fn pair_lane_write_runtime_index() {
    // Lane index is a runtime value. The mask/shift codegen path
    // builds the shift dynamically; this pins that it works.
    expect(
        "pair_lane_write_runtime_index",
        "LET START() BE $(\n  LET p = PAIR(10, 20)\n  LET i = 1\n  p.|i| := 77\n  WRITEN(p.|0|) WRITES(\"*S\") WRITEN(p.|1|)\n$)\n",
        "10 77",
    );
}

#[test]
fn pair_lane_write_into_field() {
    // The lvalue is `SELF.p.|0|` — write through a class field
    // rather than a local binding. Tests the SELF.field branch of
    // `write_back_simd_lvalue`.
    expect(
        "pair_lane_write_into_field",
        "CLASS Box $(\n  DECL p\n  ROUTINE CREATE(ip) BE SELF.p := ip\n  ROUTINE setLane0(v) BE SELF.p.|0| := v\n  FUNCTION lane0() = SELF.p.|0|\n  FUNCTION lane1() = SELF.p.|1|\n$)\nLET START() BE $(\n  LET b = NEW Box(PAIR(1, 2))\n  b.setLane0(50)\n  WRITEN(b.lane0()) WRITES(\"*S\") WRITEN(b.lane1())\n$)\n",
        "50 2",
    );
}

#[test]
fn quad_lane_write_preserves_other_lanes() {
    // QUAD is 4 × i16 packed. Writing one lane must not affect the
    // others. With 16-bit lanes, the mask/shift arithmetic is
    // different per lane index — pin that all lanes work.
    expect(
        "quad_lane_write_preserves_other_lanes",
        "LET START() BE $(\n  LET q = QUAD(1, 2, 3, 4)\n  q.|2| := 99\n  WRITEN(q.|0|) WRITES(\"*S\") WRITEN(q.|1|) WRITES(\"*S\") WRITEN(q.|2|) WRITES(\"*S\") WRITEN(q.|3|)\n$)\n",
        "1 2 99 4",
    );
}
