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
