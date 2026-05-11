//! Tier 6 of `docs/test_matrix.md` — GC & runtime.
//!
//! These probes drive each landed runtime feature end-to-end
//! through the JIT and assert on user-visible output. Lower-level
//! GC invariants (mark-sweep cycles, multi-thread alloc, safepoint
//! park) are unit-tested in `newbcpl-runtime`; this file targets
//! the JIT-side surfaces a BCPL program actually touches:
//!
//! * `NEW Class` allocates on the GC heap.
//! * `VEC` / `FVEC` / `PAIRS` / `LIST` allocate with the right
//!   length-header convention.
//! * `LIST(...)` builds a real linked chain that `HD`, `TL`,
//!   `LEN`, `APND`, `CONCAT`, `FOREACH` all see the same shape of.
//! * `GC()` and `HEAP_INFO()` builtins do what they advertise.
//! * `FINISH` exits the JIT'd program cleanly.
//!
//! When a future runtime change regresses any of these, the
//! matching probe fails with a clean stdout diff pointing at the
//! cell.

use newbcpl_tests::{expect_stdout as expect, expect_stdout_contains};

// ─── Allocation surfaces ───────────────────────────────────────────

#[test]
fn new_class_round_trips_a_value() {
    // NEW Class → __newbcpl_alloc_rec(size) → BlockHeader stamped
    // with a runtime-interned TypeDesc. The value we wrote should
    // come back unchanged.
    expect(
        "new_class_round_trips_a_value",
        "CLASS P $(\n  DECL x\n  ROUTINE CREATE(ix) BE $( SELF.x := ix $)\n$)\nLET START() BE $(\n  LET p = NEW P(42)\n  WRITEN(p.x)\n$)\n",
        "42",
    );
}

#[test]
fn vec_holds_its_length_at_negative_one() {
    // `LEN(v)` reads `*(v - 8)`. Our emitter allocates `k+1` cells
    // and writes the length into slot 0; the returned pointer is
    // one word past it.
    expect(
        "vec_holds_its_length_at_negative_one",
        "LET START() BE $(\n  LET v = VEC 7\n  WRITEN(LEN(v))\n$)\n",
        "7",
    );
}

#[test]
fn vec_subscript_reads_what_was_written() {
    expect(
        "vec_subscript_reads_what_was_written",
        "LET START() BE $(\n  LET v = VEC 3\n  v!0 := 100\n  v!1 := 200\n  v!2 := 300\n  WRITEN(v!0)\n  WRITES(\"*S\")\n  WRITEN(v!1)\n  WRITES(\"*S\")\n  WRITEN(v!2)\n$)\n",
        "100 200 300",
    );
}

#[test]
fn vec_init_list_reads_each_slot() {
    // `VEC [a, b, c]` and `VEC(a, b, c)` should both populate.
    expect(
        "vec_init_list_reads_each_slot",
        "LET START() BE $(\n  LET v = VEC [11, 22, 33]\n  WRITEN(v!0)\n  WRITES(\"*S\")\n  WRITEN(v!1)\n  WRITES(\"*S\")\n  WRITEN(v!2)\n  WRITES(\"*S\")\n  WRITEN(LEN(v))\n$)\n",
        "11 22 33 3",
    );
}

#[test]
fn fvec_holds_floats() {
    expect(
        "fvec_holds_floats",
        "LET START() BE $(\n  LET v = FVEC 3\n  v!0 := 1.5\n  v!1 := 2.5\n  v!2 := 3.5\n  FWRITE(v!0)\n  WRITES(\"*S\")\n  FWRITE(v!1)\n  WRITES(\"*S\")\n  FWRITE(v!2)\n$)\n",
        "1.5 2.5 3.5",
    );
}

// ─── List runtime (ListHeader + ListAtom chain) ────────────────────

#[test]
fn list_len_counts_appends() {
    // `__newbcpl_list_new_empty` returns an empty header; each
    // `APND` bumps `length` and tacks an `ATOM_INT` atom on.
    expect(
        "list_len_counts_appends",
        "LET START() BE $(\n  LET xs = LIST(1, 2, 3)\n  WRITEN(LEN(xs))\n$)\n",
        "3",
    );
}

#[test]
fn list_hd_returns_first_element() {
    expect(
        "list_hd_returns_first_element",
        "LET START() BE $(\n  LET xs = LIST(10, 20, 30)\n  WRITEN(HD(xs))\n$)\n",
        "10",
    );
}

#[test]
fn list_tl_skips_one_element() {
    expect(
        "list_tl_skips_one_element",
        "LET START() BE $(\n  LET xs = LIST(7, 8, 9)\n  WRITEN(HD(TL(xs)))\n$)\n",
        "8",
    );
}

#[test]
fn apnd_grows_an_empty_list() {
    expect(
        "apnd_grows_an_empty_list",
        "LET START() BE $(\n  LET xs = LIST()\n  APND(xs, 11)\n  APND(xs, 22)\n  APND(xs, 33)\n  WRITEN(LEN(xs))\n  WRITES(\"*S\")\n  WRITEN(HD(xs))\n$)\n",
        "3 11",
    );
}

#[test]
fn foreach_walks_list_chain_in_order() {
    // FOREACH over a LIST takes the linked-walk path (header.head
    // → atom.next chain), not the index path used for VEC.
    expect(
        "foreach_walks_list_chain_in_order",
        "LET START() BE $(\n  LET xs = LIST(1, 2, 3, 4)\n  FOREACH e IN xs DO $(\n    WRITEN(e)\n    WRITES(\"*S\")\n  $)\n$)\n",
        "1 2 3 4 ",
    );
}

#[test]
fn foreach_walks_vec_by_index() {
    // FOREACH over a VEC takes the index-walk path
    // (`i = 0..__newbcpl_len(v)`, `v!i`).
    expect(
        "foreach_walks_vec_by_index",
        "LET START() BE $(\n  LET v = VEC 4\n  v!0 := 5\n  v!1 := 6\n  v!2 := 7\n  v!3 := 8\n  FOREACH e IN v DO $(\n    WRITEN(e)\n    WRITES(\"*S\")\n  $)\n$)\n",
        "5 6 7 8 ",
    );
}

// ─── Builtins for visible heap state ────────────────────────────────

#[test]
fn gc_returns_zero_and_keeps_going() {
    // `GC()` from JIT'd code triggers a full collect and returns
    // 0. Subsequent allocations should still work.
    expect(
        "gc_returns_zero_and_keeps_going",
        "CLASS P $(\n  DECL x\n  ROUTINE CREATE(ix) BE $( SELF.x := ix $)\n$)\nLET START() BE $(\n  LET p1 = NEW P(1)\n  GC()\n  LET p2 = NEW P(2)\n  WRITEN(p2.x)\n$)\n",
        "2",
    );
}

#[test]
fn heap_info_prints_its_header() {
    // HEAP_INFO writes a multi-line summary to stdout. The exact
    // numbers vary per run (timestamps, addresses, alloc count
    // depends on internals), but the header line is stable.
    expect_stdout_contains(
        "heap_info_prints_its_header",
        "LET START() BE $(\n  HEAP_INFO()\n$)\n",
        "=== newbcpl GC heap info ===",
    );
}

#[test]
fn heap_info_after_alloc_shows_block() {
    // After allocating a class instance, the snapshot's
    // "allocations:" line must reflect at least one block. We
    // assert on the stable phrase rather than the count.
    expect_stdout_contains(
        "heap_info_after_alloc_shows_block",
        "CLASS P $(\n  DECL x, y, z\n  ROUTINE CREATE(ix) BE $( SELF.x := ix $)\n$)\nLET START() BE $(\n  LET p = NEW P(0)\n  HEAP_INFO()\n$)\n",
        "allocations:",
    );
}

// ─── Multi-allocation isolation ────────────────────────────────────

#[test]
fn many_allocations_do_not_overlap() {
    // Eight distinct `NEW P` allocations in sequence: each must
    // produce a heap block whose field reads back the value
    // CREATE wrote. If the allocator ever returned an
    // overlapping block, the values would smear together.
    //
    // Why eight LET bindings and not a `FOR i = 0 TO 19` loop?
    // The looping shape `LET ps = VEC 20; ps!i := NEW P(...)`
    // round-trips the class pointer through a VEC slot. Reading
    // `LET p = ps!i; p.x` then needs sema to know `p`'s class
    // — which it can't infer through a subscript today. That
    // gap is its own ticket; see the `#[ignore]`'d
    // `vec_of_class_pointers_round_trip` probe below.
    expect(
        "many_allocations_do_not_overlap",
        "CLASS P $(\n  DECL x\n  ROUTINE CREATE(ix) BE $( SELF.x := ix $)\n$)\nLET START() BE $(\n  LET a = NEW P(11)\n  LET b = NEW P(22)\n  LET c = NEW P(33)\n  LET d = NEW P(44)\n  LET e = NEW P(55)\n  LET f = NEW P(66)\n  LET g = NEW P(77)\n  LET h = NEW P(88)\n  WRITEN(a.x) WRITES(\"*S\")\n  WRITEN(b.x) WRITES(\"*S\")\n  WRITEN(c.x) WRITES(\"*S\")\n  WRITEN(d.x) WRITES(\"*S\")\n  WRITEN(e.x) WRITES(\"*S\")\n  WRITEN(f.x) WRITES(\"*S\")\n  WRITEN(g.x) WRITES(\"*S\")\n  WRITEN(h.x)\n$)\n",
        "11 22 33 44 55 66 77 88",
    );
}

#[test]
#[ignore = "sema doesn't track class through subscripts — `LET p = ps!i; p.x` can't resolve p's class. Tracked as a Tier 2 sema gap."]
fn vec_of_class_pointers_round_trip() {
    // The shape that surfaces the gap above: store NEW P(...)
    // pointers in a VEC, read them back, dereference fields.
    // Fixes when sema gains class-tracking through subscripts
    // (or `LET p AS P = ps!i` propagates the annotation to the
    // class-name slot of LocalInfo).
    expect(
        "vec_of_class_pointers_round_trip",
        "CLASS P $(\n  DECL x\n  ROUTINE CREATE(ix) BE $( SELF.x := ix $)\n$)\nLET START() BE $(\n  LET ps = VEC 4\n  FOR i = 0 TO 3 DO $( ps!i := NEW P(i * 10) $)\n  FOR i = 0 TO 3 DO $(\n    LET p = ps!i\n    WRITEN(p.x) WRITES(\"*S\")\n  $)\n$)\n",
        "0 10 20 30 ",
    );
}

// ─── Termination ──────────────────────────────────────────────────

#[test]
fn finish_terminates_cleanly() {
    // FINISH flushes stdout and exits the JIT'd process (which is
    // the test subprocess here). Any output before FINISH must
    // reach us; anything after must NOT.
    expect(
        "finish_terminates_cleanly",
        "LET START() BE $(\n  WRITES(\"before\")\n  FINISH\n  WRITES(\"after\")\n$)\n",
        "before",
    );
}

// ─── List heterogeneity ────────────────────────────────────────────

#[test]
fn list_concat_combines_two_chains() {
    expect(
        "list_concat_combines_two_chains",
        "LET START() BE $(\n  LET a = LIST(1, 2)\n  LET b = LIST(3, 4, 5)\n  LET c = CONCAT(a, b)\n  WRITEN(LEN(c))\n  WRITES(\"*S\")\n  FOREACH e IN c DO $(\n    WRITEN(e)\n    WRITES(\"*S\")\n  $)\n$)\n",
        "5 1 2 3 4 5 ",
    );
}

#[test]
fn list_concat_walked_through_hd_tl_chain() {
    // Lower-level alternative that avoids the `LEN(c)` /
    // `FOREACH e IN c` paths affected by the sema return-type
    // gap. Walks `c` directly via HD/TL — those work because
    // they're explicit calls into the list runtime.
    expect(
        "list_concat_walked_through_hd_tl_chain",
        "LET START() BE $(\n  LET a = LIST(1, 2)\n  LET b = LIST(3, 4)\n  LET c = CONCAT(a, b)\n  WRITEN(HD(c)) WRITES(\"*S\")\n  WRITEN(HD(TL(c))) WRITES(\"*S\")\n  WRITEN(HD(TL(TL(c)))) WRITES(\"*S\")\n  WRITEN(HD(TL(TL(TL(c)))))\n$)\n",
        "1 2 3 4",
    );
}
