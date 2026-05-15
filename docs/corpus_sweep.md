# Corpus sweep — May 2026

Full sweep of `reference/tests/bcl_tests/` (856 `.bcl` files) against
the current pipeline. Compares against the previous 100-file baseline
captured in `test-results.txt`.

## Headline

```
total:   856
passed:  451   (52.7 %)
failed:  405   (47.3 %)
elapsed: 97.4 s
```

Failures by reported phase:

| Phase  | Count |
|--------|-------|
| parse  |    88 |
| sema   |     2 |
| run    |   310 |
| crash  |     5 |

The previous baseline was 68/100 (68 %) on the first 100 alphabetical
files — not the full corpus, so the numbers aren't directly
comparable. But the trajectory is positive: ~half the failures we see
now are categorisable as "deliberate by design" (GLOBALS rejected,
visibility violations) or "out-of-scope" (SDL2 builtins, no-START
library files); see below.

## Failure buckets

### Deliberate rejections — not bugs

| Bucket | Count | Source |
|--------|-------|--------|
| `GLOBALS` slot-pinning rejected | 28 | parser refuses the legacy global-vector form (see `docs/reference_audit.md`) |
| visibility violations | 2 | `PRIVATE` / `PROTECTED` access tests that *should* be rejected — they're testing our enforcement |

**32 of 405 failures are working as designed.** Subtracting these gives
an effective denominator of 824 and an adjusted pass rate of
**451 / 824 = 54.7 %**.

### "Not a program" — should be skipped

| Bucket | Count |
|--------|-------|
| `no START function declared` | 29 |

These are header/library `.bcl` files meant to be `GET`-ed by another
program, not run directly. `test-folder` doesn't currently distinguish
them. A `// :no-start` comment or filename-prefix convention could let
the sweep skip them with a separate "library" tally. Subtracting these
brings the denominator to 795 and the rate to **451 / 795 = 56.7 %**.

### Easy unblocks — would land big wins

#### 1. Lowercase builtin lookup (≈ 80 files)

Top missing-builtin names — sorted, picked the obvious ones:

```
31 × writef
11 × MIN
10 × JOIN
10 × OCTS
 8 × QUADS
 6 × TIMER_START
 6 × TIMER_END
 6 × TIMER_DISPLAY
 5 × MAX
 5 × SUM
```

The 31 `writef` failures are particularly stark — the runtime exposes
`WRITEF`, source uses `writef`. The user guide says "identifiers may
be either case but lower-case is the usual style" — so the runtime
should accept both spellings for builtins.

Cleanest fix: register every builtin in the runtime symbol table
under both UPPERCASE and lowercase, pointing at the same address.
~20 LOC change in `newbcpl-runtime::builtins`. Would unblock probably
50+ corpus files in one go.

#### 2. `libhdr.h` resolution (53 files)

```
53 × GET "...": file not found
```

Mostly `GET "libhdr.h"`. The reference tree ships one at
`reference/tests/include/libhdr.h`, but it uses the `GLOBAL $( name :
slot $)` form we reject. Two paths:

* **Adapter file**: ship a NewBCPL-flavoured `libhdr.bcl` in
  `modules-active/` with the MANIFEST constants from the legacy file
  but no slot-pinning. `GET "libhdr.h"` would still fail (no `.bcl`
  extension), so we'd want either to extend GET's resolution to try
  `.h` too, or rename the tests' GETs.
* **Search path**: teach the driver to honour an `NEWBCPL_GET_PATH`
  env var so corpus runs can add `reference/tests/include` to the
  GET search list. Combined with a small adapter `libhdr.bcl` so the
  legacy file isn't actually parsed, this unblocks the bucket.

The first path is simpler and more self-contained.

#### 3. Missing simple builtins (≈ 40 files)

`MIN`, `MAX`, `SUM`, `JOIN`, `LENGTH`, `TIMER_START` etc. Most are
one-liners in the runtime. Some (`SUM`, `JOIN`) operate on lists; the
list runtime already supports the walk. `TIMER_*` is wall-clock
introspection — trivial.

#### 4. SIMD pairwise reducers (≈ 22 files)

```
15 × PAIRWISE_MIN
 5 × PAIRWISE_ADD
 4 × PAIRWISE_MAX
```

The pair/quad/oct types support per-lane access; pairwise reducers
that fold across lanes don't exist yet. Each is a small codegen
addition. The reference has them as runtime helpers; we'd add the
same.

#### 5. SDL2 graphics builtins — out of scope (≈ 50 files)

Every `SDL2_*` failure (50+ instances of various names). The
reference NBCPL had an SDL2 graphics path; NewBCPL took the
Direct2D / DirectWrite GUI path instead. These tests will never run
on the current iGui surface without an SDL2 shim. Reasonable to
quarantine — skip the sweep for files that import `SDL2_*` and tally
separately.

### Other failures (~135)

Mix of:
- Parser syntactic gaps (60 — `expected expression got >`, `expected
  declaration got AND`, etc.). Each is its own targeted patch.
- Run-time mismatches — programs that parse fine and ought to run
  but produce unexpected output (the report's `> ...` panes show
  what stdout was vs what the test expected). These need
  case-by-case attention; many are likely small bugs in builtins or
  the runtime.
- 5 crashes — all `exit -1`, meaning the per-test timeout fired. The
  list (`simple_control_flow_test.bcl`, `test_repeatuntil.bcl`,
  `test_repeatwhile.bcl`, `test_veneer_simple.bcl`,
  `working_cleanup_test.bcl`) suggests infinite-loop bugs in our
  loop lowering for *specific* shapes. Worth a focused look.

## Projection

If we landed **just** items 1–3 from "easy unblocks":

| Action | Estimated unblocks |
|--------|--------------------|
| Lowercase builtin aliases | ~80 |
| `libhdr.h` adapter + path | ~53 |
| `MIN` / `MAX` / `SUM` / `JOIN` / `LENGTH` / `TIMER_*` | ~40 |
| **Subtotal** | **~173** |

Best-case pass count: **~624 / 856 = 73 %**, or **~624 / 795 = 78 %**
excluding library files.

## Recommendation

In order of leverage-per-hour:

1. **Lowercase builtin aliases.** 20 LOC; one commit; unblocks the
   single largest bucket. Low risk (additive registration, no name
   collisions).
2. **`libhdr.h` adapter in `modules-active`.** Provides the canonical
   MANIFEST constants the corpus expects; ~30 lines of BCPL.
3. **Add `MIN` / `MAX` / `SUM` / `JOIN` / `LENGTH` / `TIMER_*` builtins.**
   Each is a runtime helper; the IR side already calls into named
   builtins, so it's purely additive work.
4. **Quarantine SDL2 tests** with a header-comment annotation; sweep
   reports them separately.
5. **Investigate the 5 timeout crashes** — likely small bugs in a
   specific loop shape's lowering.

After 1–3 land we'd be at ~78 % effective pass rate. After 4 the
quarantine clears the SDL2 noise. Then 5 is a focused bug hunt.

---

## Iteration 2 — May 2026, after first unblocks landed

After landing the three easy-unblock tracks:

* Lowercase aliases on every builtin (`writef` resolves to
  `WRITEF`).
* `modules-active/libhdr.bcl` adapter + `.h → .bcl` path fallback.
* `MIN` / `MAX` / `ABS` / `LENGTH` / `TIMER_START` / `TIMER_END` /
  `TIMER_DISPLAY` builtins.
* Sema fold for `-N` in MANIFEST initialisers (discovered while
  wiring `ENDSTREAMCH = -1`).

The sweep landed at:

```
total:   856
passed:  508   (59.3 %)   ↑ from 451 (52.7 %)  →  +57 unblocks
failed:  348             ↓ from 405
elapsed: 100.6 s
```

Failures by phase:

| Phase  | Before | After |
|--------|--------|-------|
| parse  | 88     | 88    |
| sema   | 2      | 2     |
| run    | 310    | 250   |
| crash  | 5      | 8     |

The parse total didn't move (we didn't touch parser gaps), `run`
fell by 60 (the unblock), and `crash` ticked up by 3 — files that
previously failed at parse now get further and time out on
infinite-loop bugs in specific loop shapes.

`no START function declared` jumped from 29 to 45 — the libhdr
adapter let more "library" files parse through to the
load-check, where they fail because they aren't programs.

### Remaining failure buckets

Top missing-builtin names after iteration 2:

```
25 × PAIRWISE_MIN / MAX / ADD  — SIMD lane-fold reducers
22 × TYPE / TYPE_STRING / AS_STRING  — list atom introspection
18 × OCTS / QUADS  — SIMD pack constructors (different spelling than our keywords)
16 × JOIN / SUM  — list reducers
70 × SDL2_*  — out of scope, Direct2D path instead
```

### Effective pass rate

Subtracting deliberate rejections (28 GLOBALS), library files
without START (45), the 2 visibility-violation tests, and the
out-of-scope SDL2 family (~50 files distinguished by missing
SDL2_* builtins):

```
effective = 508 / (856 - 28 - 45 - 2 - 50) = 508 / 731 = 69.5 %
```

### Next iteration

The highest-leverage adds for a third round:

1. **PAIRWISE_MIN / MAX / ADD** — 25 files. We have lane operators;
   pairwise fold of a SIMD pack into a scalar is a thin codegen
   addition.
2. **TYPE / TYPE_STRING / AS_STRING** — 22 files. List atom tags
   are already encoded in the runtime; expose introspection +
   converters.
3. **OCTS / QUADS as builtin functions** — 18 files. Probably
   either alias to OCT/QUAD or add as separate builtins. Needs
   investigation of corpus usage.
4. **JOIN / SUM list reducers** — 16 files. Walk + accumulate.

Estimated land: ~80 more files would push us toward 67 % raw / 80 %
effective.

---

## Iteration 3 — pluralised SIMD allocators, pairwise reducers, atom-introspection casts

```
total:   856
passed:  524   (61.2 %)   ↑ from 508 (59.3 %)  →  +16 unblocks
failed:  332             ↓ from 348
elapsed: 99.8 s
```

Six runtime adds + one IR rewrite:

* **`OCTS(n)` / `QUADS(n)` / `FOCTS(n)` / `FQUADS(n)`.** Pluralised
  allocators for vectors of SIMD packs — corpus convention is the
  plural keyword, matching the existing `PAIRS` / `FPAIRS`. Same
  contract: one 64-bit slot per pack, length-prefixed.

* **`PAIRWISE_MIN` / `PAIRWISE_MAX` / `PAIRWISE_ADD`.** Fold the two
  i32 lanes of a PAIR into a single scalar. The natural reduction
  step after lane-wise SIMD work.

* **TYPE-tag MANIFEST prelude.** `TYPE_INT` = 1, `TYPE_FLOAT` = 2,
  `TYPE_STRING` = 3, `TYPE_LIST` = 4, `TYPE_OBJECT` = 5,
  `TYPE_PAIR` = 6. Pre-seeded into `sema.manifests` so
  `SWITCHON T INTO $( CASE TYPE_STRING: ... $)` resolves at
  compile time. Values mirror the runtime's `ATOM_*` tags.

* **`TYPE(list)` runtime helper.** Returns the head atom's type tag
  for "what kind of list is this" probes.

* **`AS_INT` / `AS_FLOAT` / `AS_STRING` as compile-time casts.**
  These are bit-reinterpretation operations, not runtime helpers —
  BCPL is typeless on the wire so the cast doesn't change bits,
  just sema's TypeHint. IR rewrites `AS_T(x)` calls into the
  argument directly; codegen reads the surrounding context for
  load shape. Avoids the x86-64 ABI mismatch that would have
  occurred declaring `AS_FLOAT` as `extern "C" fn(i64) -> f64`
  (return value would land in XMM0 but the JIT thinks i64 in RAX).

### Remaining failure buckets

```
70 × SDL2_*  — out-of-scope graphics
16 × JOIN / SUM  — list-with-separator and pair-vector add (real work)
 9 × SGETVEC / QGETVEC / PGETVEC / IGETVEC  — typed allocator variants
~135 × misc parse + run gaps
```

### Effective pass rate

```
effective = 524 / (856 - 28 - 45 - 2 - 70) = 524 / 711 = 73.7 %
```

### Where things stand

Across the three iterations we've gone from **451 → 524** raw passes
(52.7 % → 61.2 %), or **54.7 % → 73.7 %** on the effective denominator.
The dominant remaining bucket is SDL2_* (out of scope by design;
candidate for explicit quarantine). After that the residual is
small individual implementation tasks (`JOIN`, `SUM`, typed
`*GETVEC`, parser syntactic gaps) — each its own focused piece of
work, no big systematic wins left in the sweep.

### Conclusion

The "easy unblocks" path has plateaued. To push further we'd be
into:

1. **SDL2 quarantine** — pure classification work, gets us a
   cleaner effective-rate number.
2. **`JOIN` / `SUM`** — real implementations; ~16 files but
   non-trivial.
3. **Parser syntactic gaps** — investigated case-by-case; 60+
   files but each is its own pattern.
4. **5 timeout-crashes** — focused bug hunt in loop lowering.

None of these is a 50-file win. The corpus sweep has done its job
as a coverage signal; further investment is targeted bug-fixing,
not bulk unblock.

---

## Iteration 4 — SDL2 quarantined, SUM / JOIN landed

```
total:    835   (corpus minus 21 SDL2 files)
passed:   531   (63.6 %)
failed:   304
skipped:   21   (source contained `SDL2_`)
elapsed: 116.6 s
```

Two pieces:

* **`SUM(v1, v2)`** — word-by-word integer addition into a fresh
  VEC of the same length. Used by SIMD-pair workloads where each
  pair is stored as two consecutive int slots; the function is
  agnostic of the pair shape and just walks words.

* **`JOIN(list, separator)`** — concatenate every atom in `list`
  with `separator` between elements. Returns a fresh
  null-terminated UTF-8 buffer on the GC heap. Atom dispatch:
  strings copy verbatim, floats use Rust's default formatting,
  everything else (ints, packed pairs) renders as decimal i64.

Plus a driver knob:

* **`test-folder skip=SUBSTR ...`** — drops files whose source
  contains the substring before the sweep runs. Multiple `skip=`
  flags compose. We use `skip=SDL2_` for the standard corpus
  sweep — NewBCPL's GUI surface is Direct2D / DirectWrite (manifesto
  §3); the SDL2 path was never in scope, and those 21 files were
  cluttering the failure landscape without telling us anything
  actionable. They're reported separately in the `# skipped:`
  line so the count isn't lost.

### Top remaining failure buckets

Top missing-builtin names after iteration 4:

```
4 × __newbcpl_indirect        # internal stub; indirect-call fallback
3 × IGETVEC / PGETVEC / QGETVEC / SGETVEC   # typed-allocator variants
3 × BCPL_FREE_LIST
3 × vec1 / vec2               # likely undefined user functions
2 × MAKEPAIR / FLTOFX / FILE_OPEN_*  # misc
```

No bucket above 4. The dominant failure shape is now small
individual problems — parser syntactic gaps and runtime
mismatches one file at a time — not systematic gaps.

### Where things stand

Across eight sweep iterations the journey is:

| Sweep | Pass | % raw | Effective % * |
|-------|------|-------|---------------|
| 1     | 451  | 52.7  | 59.3          |
| 2     | 508  | 59.3  | 66.8          |
| 3     | 524  | 61.2  | 68.9          |
| 4     | 524  | 61.2  | 68.9          |
| 5     | 531  | 63.6  | 69.9          |
| 6     | 534  | 63.9  | 70.3          |
| 7     | 537  | 64.3  | 70.7          |
| 8     | 539  | 64.6  | **70.9**      |

\* Effective denominator = 856 − 28 GLOBALS − 45 no-START − 2
visibility-violation − 21 SDL2 = 760.

Sweep 6 picked up param-annotation-using shapes. Sweep 7 picked up
the typed-allocator bucket (`IGETVEC` / `SGETVEC` / `PGETVEC` /
`QGETVEC` added as GETVEC aliases). Sweep 8 eliminated the
`__newbcpl_indirect` bucket entirely by implementing name-keyed
dynamic dispatch — untyped `param.method()` now resolves through a
runtime `__newbcpl_lookup_method` helper keyed by the instance's
inline vtable pointer.

The corpus sweep has done its job: it surfaced systematic gaps,
each of which got addressed with a focused patch. What's left is
a long tail of one-off bugs that need targeted investigation,
not bulk unblock.
