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

use newbcpl_tests::{expect_reject, expect_stdout as expect, expect_stdout_contains};

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

// ─── Typed allocators (IGETVEC / SGETVEC / PGETVEC / QGETVEC) ────
//
// Naming-only aliases of GETVEC — the element-type prefix tells
// the reader what the vector is for (Integer / String / Pair /
// Quad) but the underlying storage is identical: one word-slot
// per element, length stamped at p[-1], `p!i` reads / writes
// slot i. Each typed form gets one probe to pin it as live so a
// regression in `builtins.rs`'s registration table catches it.

#[test]
fn igetvec_allocates_integer_vector() {
    expect(
        "igetvec_allocates_integer_vector",
        "LET START() BE $(\n  LET v = IGETVEC(5)\n  FOR i = 0 TO 4 DO v!i := i + 100\n  WRITEN(v!2) WRITES(\"*S\") WRITEN(LEN(v))\n$)\n",
        "102 5",
    );
}

#[test]
fn sgetvec_allocates_string_vector() {
    // Holds string pointers — each slot can store the address of a
    // distinct string literal.
    expect(
        "sgetvec_allocates_string_vector",
        "LET START() BE $(\n  LET v = SGETVEC(3)\n  v!0 := \"alpha\"\n  v!1 := \"beta\"\n  v!2 := \"gamma\"\n  WRITES(v!1)\n$)\n",
        "beta",
    );
}

#[test]
fn pgetvec_allocates_pair_vector() {
    // Pair vector: each slot is one packed pair (word).
    // We just exercise allocation + subscript here; pair encoding
    // is covered in tier 7's lane probes.
    expect(
        "pgetvec_allocates_pair_vector",
        "LET START() BE $(\n  LET v = PGETVEC(4)\n  WRITEN(LEN(v))\n$)\n",
        "4",
    );
}

#[test]
fn qgetvec_allocates_quad_vector() {
    expect(
        "qgetvec_allocates_quad_vector",
        "LET START() BE $(\n  LET v = QGETVEC(2)\n  WRITEN(LEN(v))\n$)\n",
        "2",
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
fn vec_of_class_pointers_round_trip() {
    // Round-trip `NEW P(...)` pointers through a VEC, read them
    // back, dereference fields. The reader uses an explicit
    // `LET p AS P = ps!i` annotation — sema can't track class
    // identity through a subscript on its own (the VEC is
    // polymorphic at the type level), so the annotation is what
    // tells sema the slot's static class is `P`. Without the
    // annotation, `p.x` would fail to resolve at member-access
    // time.
    expect(
        "vec_of_class_pointers_round_trip",
        "CLASS P $(\n  DECL x\n  ROUTINE CREATE(ix) BE $( SELF.x := ix $)\n$)\nLET START() BE $(\n  LET ps = VEC 4\n  FOR i = 0 TO 3 DO $( ps!i := NEW P(i * 10) $)\n  FOR i = 0 TO 3 DO $(\n    LET p AS P = ps!i\n    WRITEN(p.x) WRITES(\"*S\")\n  $)\n$)\n",
        "0 10 20 30 ",
    );
}

// ─── Class-typed field GC tracing ─────────────────────────────────
//
// When sema infers the class of a DECL field (via a `SELF.field :=
// NEW Inner(...)` back-fill, an `AS Class` annotation on a LET form,
// or a direct `LET f = NEW Foo()` initialiser), that field's byte
// offset has to land in the layout's `ptr_offsets` so the GC traces
// through it. If it doesn't, the inner instance is unreachable
// through the outer's roots and gets swept, leaving outer.field
// pointing at freed memory.

#[test]
fn declared_field_back_filled_is_traced() {
    // `DECL inner` is declared with no class hint at parse time. Sema
    // back-fills the class identity during method-body analysis when
    // it sees `SELF.inner := NEW Inner(...)`. The layout pass then
    // adds the field's offset to `ptr_offsets`. An explicit `GC()`
    // mid-program proves the trace path: if the field weren't traced,
    // `inner` would be swept and `o.inner.value` would read freed
    // (zeroed) memory.
    expect(
        "declared_field_back_filled_is_traced",
        "CLASS Inner $(\n  DECL value\n  ROUTINE CREATE(v) BE SELF.value := v\n$)\nCLASS Outer $(\n  DECL inner\n  ROUTINE CREATE(v) BE SELF.inner := NEW Inner(v)\n$)\nLET START() BE $(\n  LET o = NEW Outer(42)\n  GC()\n  WRITEN(o.inner.value)\n$)\n",
        "42",
    );
}

#[test]
fn as_annotated_let_field_is_traced() {
    // The LET-form field with an `AS Class` annotation —
    // `LET inner AS Inner = ?` — must also reach `ptr_offsets`. The
    // class_name comes from the AS-resolution pass, not from a NEW
    // initialiser. Same GC test: collect, then dereference.
    expect(
        "as_annotated_let_field_is_traced",
        "CLASS Outer $(\n  LET inner AS Inner = ?\n  ROUTINE CREATE(v) BE SELF.inner := NEW Inner(v)\n$)\nCLASS Inner $(\n  DECL value\n  ROUTINE CREATE(v) BE SELF.value := v\n$)\nLET START() BE $(\n  LET o = NEW Outer(99)\n  GC()\n  WRITEN(o.inner.value)\n$)\n",
        "99",
    );
}

#[test]
fn traced_field_survives_alloc_pressure() {
    // Same as `declared_field_back_filled_is_traced` but with real
    // alloc pressure between the bind and the GC: a 256-cell VEC of
    // garbage allocations between the outer and the collect. Forces
    // the GC to actually walk the mark phase rather than no-op.
    expect(
        "traced_field_survives_alloc_pressure",
        "CLASS Inner $(\n  DECL value\n  ROUTINE CREATE(v) BE SELF.value := v\n$)\nCLASS Outer $(\n  DECL inner\n  ROUTINE CREATE(v) BE SELF.inner := NEW Inner(v)\n$)\nLET START() BE $(\n  LET o = NEW Outer(31415)\n  FOR i = 1 TO 256 DO $(\n    LET garbage = VEC 8\n    garbage!0 := i\n  $)\n  GC()\n  WRITEN(o.inner.value)\n$)\n",
        "31415",
    );
}

#[test]
fn deep_chain_survives_collection() {
    // Three-level chain — a → b → c. The middle and innermost
    // objects are only reachable through their parent's field. If
    // ptr_offsets is wrong at any level, the chain breaks and the
    // final WRITEN reads through a dangling pointer.
    expect(
        "deep_chain_survives_collection",
        "CLASS C $(\n  DECL leaf\n  ROUTINE CREATE(v) BE SELF.leaf := v\n$)\nCLASS B $(\n  DECL mid\n  ROUTINE CREATE(v) BE SELF.mid := NEW C(v)\n$)\nCLASS A $(\n  DECL top\n  ROUTINE CREATE(v) BE SELF.top := NEW B(v)\n$)\nLET START() BE $(\n  LET a = NEW A(777)\n  GC()\n  WRITEN(a.top.mid.leaf)\n$)\n",
        "777",
    );
}

// ─── SEH unwind through JIT frames ────────────────────────────────
//
// Calling a runtime helper that raises `panic!` must unwind cleanly
// back through the JIT frame to the host process's default panic
// handler — stderr gets the standard "thread '...' panicked at ..."
// line, exit code is the normal panic value (101 on Linux, hex
// `8000_0003` on Windows). Without the SEH machinery (`uwtable=2` on
// every JIT'd function, custom MCJIT memory manager that registers
// `.pdata` with `RtlAddFunctionTable`, runtime helpers declared
// `extern "C-unwind"`), the panic would corrupt the JIT frame's
// stack and fast-fail with STATUS_STACK_BUFFER_OVERRUN
// (0xC0000409) — a process abort with no panic message at all.
//
// Asserting on the substring "panicked at" is what discriminates a
// graceful unwind from a corruption crash: the panic message only
// reaches stderr if the unwinder walked back through the JIT frame
// and into the Rust runtime's default hook.

#[test]
fn runtime_panic_unwinds_through_jit() {
    expect_reject(
        "runtime_panic_unwinds_through_jit",
        "run",
        "LET START() BE $( __newbcpl_test_panic() $)\n",
        "deliberate panic from runtime helper",
    );
}

#[test]
fn runtime_panic_unwinds_through_nested_call() {
    // Two JIT frames between the panic and the host: `START` calls
    // `provoke`, which calls `__newbcpl_test_panic`. Both JIT frames
    // need their unwind info registered for the panic to land.
    expect_reject(
        "runtime_panic_unwinds_through_nested_call",
        "run",
        "LET provoke() = __newbcpl_test_panic()\nLET START() BE $( provoke() $)\n",
        "deliberate panic from runtime helper",
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

// ─── RETAIN ───────────────────────────────────────────────────────
//
// `RETAIN x = expr` declares `x` and pins it past its natural scope.
// In our GC model the binding is a stack root for as long as the
// scope holds it; the probe pins that an explicit `GC()` cycle
// doesn't reclaim a retained object.

#[test]
fn retain_declares_binding_and_survives_gc() {
    expect(
        "retain_declares_binding_and_survives_gc",
        "CLASS P $(\n  DECL n\n  ROUTINE CREATE(v) BE SELF.n := v\n$)\nLET START() BE $(\n  RETAIN p = NEW P(999)\n  GC()\n  WRITEN(p.n)\n$)\n",
        "999",
    );
}

// ─── UTF-8 string indexing via % ──────────────────────────────────
//
// Our `%` operator reads a byte (i64-extended), per the user guide
// §2.6's note that strings are UTF-8 bytes. A multibyte glyph like
// `λ` (U+03BB) encodes to two bytes (0xCE 0xBB) and reads as two
// distinct `s % i` values — pinning the UTF-8 convention so a
// future refactor doesn't drift toward the reference's 32-bit-char
// model without us noticing.

#[test]
fn utf8_multibyte_glyph_reads_as_two_bytes() {
    // λ = 0xCE 0xBB; bytes 2 and 3 are the start of the next glyph
    // (we follow with 'a' to give bytes 2 a known value).
    expect(
        "utf8_multibyte_glyph_reads_as_two_bytes",
        "LET START() BE $(\n  LET s = \"λa\"\n  WRITEN(s % 0) WRITES(\"*S\")\n  WRITEN(s % 1) WRITES(\"*S\")\n  WRITEN(s % 2)\n$)\n",
        "206 187 97",
    );
}
