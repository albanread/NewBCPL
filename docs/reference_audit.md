# Reference audit — language coverage vs. the BCPL spec

This is a feature-by-feature comparison of what `reference/documentation/`
describes as the language surface against what NewBCPL implements today.
Each row is one feature; the status column says where we are.

## Legend

| Symbol | Meaning |
|---|---|
| ✓ | Implemented end-to-end (parser + sema + IR + JIT) and pinned by at least one matrix probe |
| ⚠ | Implemented but probe coverage is thin — works in casual use, untested at the edges |
| ◐ | Partially implemented — known holes |
| ✗ | Not implemented |
| Δ | Intentional deviation from the reference; documented in the user guide |

Source docs consulted (in order of authority for the language surface):
- `reference/documentation/BCPL syntax.md`
- `reference/documentation/BCPL float extension.md`
- `reference/documentation/BCPL char and string.md`
- `reference/documentation/BCPL Runtime.md`
- `reference/documentation/classes_and_objects.md`

---

## §1 — Lexical

| Feature | Status | Notes |
|---|---|---|
| Identifiers (letters, digits, `_`) | ✓ | Lexer accepts `_` per the modern dialect |
| Decimal integer literals | ✓ | Tier 1 / Tier 3 probes |
| Octal literals `#NNN` | ✓ | Lexer rejects bare `#` cleanly |
| Hex literals `#XAB` | ✓ | Lexer + bitfield probes use them |
| Float literals (`3.14`, `1e-5`) | ✓ | Tier 3 float probes |
| String literals + `*N` `*T` `*S` `*B` `*P` `*C` `*"` `**` escapes | ✓ | Lexer probes pin every escape |
| Character constants `'c'`, `'*N'`, `'*X41'` | ⚠ | Lexed; basic use works; no probe pinning Unicode code points |
| `TRUE` / `FALSE` literals | ✓ | Parser probes |
| `//` line comments | ✓ | |
| `/* */` block comments | ✓ | Lexer probes; nested form noted but not deeply tested |

## §2 — Operator precedence and expressions

Reference precedence table (high → low): `()`, `! OF`, `@ !`, `* / REM`,
`+ -`, `<< >>`, relational, `&`, `\| NEQV EQV`, `->`, `TABLE`, `VALOF`.

| Operator | Status | Notes |
|---|---|---|
| `@x` address-of | ⚠ | Parsed as `UnaryOp::AddressOf`; lowering returns a Word; corpus-shape only — no dedicated probe |
| `!ptr` indirection (read + write) | ✓ | Tier 4 has both |
| `v ! i` word subscript (read + write) | ✓ | Tier 6 |
| `v % i` char subscript (read + write) | ✓ | Tier 6 |
| `v .% i` float subscript (read + write) | ✓ | Tier 6 |
| `v %% (start, width)` bitfield (read + write) | ✓ | Tier 3 — landed this cycle |
| `pair.\|i\|` lane access (read + write) | ✓ | Tier 7 — landed this cycle |
| `obj.field` / `obj OF field` member access | ✓ | Both forms accepted; tier 5 probes use `.` |
| Arithmetic `+ - * / REM` | ✓ | Tier 3 |
| Dotted float `+. -. *. /.` | ✓ | Tier 3 |
| Hash float `+# -# *# /#` (legacy spelling) | ✓ | Lexer maps to same tokens |
| Shifts `<< >>` | ✓ | Tier 3 |
| Relational `= ~= < <= > >=` (integer) | ✓ | Tier 3 |
| Relational `=. ~=. <. <=. >. >=.` (float) | ✓ | Tier 3 |
| Bitwise `& \|` | ✓ | Symbol forms are bitwise per parser |
| Keyword `BAND BOR BXOR BNOT` (bitwise) | ✓ | Parser test pins it |
| Keyword `AND OR NOT XOR` (logical) | ✓ | Parser test fixed this cycle |
| `EQV` / `NEQV` | ⚠ | Parser handles them; no behavioural probe |
| Conditional `cond -> a, b` | ✓ | Tier 3 |
| `TABLE(...)` read-only constant | ✓ | Lexer + lowering wired |
| `FTABLE(...)` | ✓ | Float counterpart |
| `VALOF` + `RESULTIS` | ✓ | Tier 4 |

## §3 — Commands

| Command | Status | Notes |
|---|---|---|
| `:=` single-target assign | ✓ | Tier 4 |
| Multi-target `a, b := x, y` | ✓ | Parser test; tier 4 destructure |
| Routine call `R(args)` | ✓ | Tier 3 |
| `IF E THEN C` | ✓ | Tier 4 |
| `UNLESS E THEN C` | ✓ | Tier 4 |
| `TEST E THEN C1 ELSE C2` | ✓ | Tier 4 |
| **Reference's `TEST E THEN C1 OR C2`** | Δ | We require `ELSE`, not `OR`. Documented in user guide §1.4 |
| `WHILE`, `UNTIL` | ✓ | Tier 4 |
| `body REPEAT` / `REPEATWHILE` / `REPEATUNTIL` | ✓ | Tier 4 |
| `FOR n = E1 TO E2 BY K DO C` | ✓ | Tier 4 |
| `FOREACH x IN xs DO C` (modern extension) | ✓ | Tier 6 |
| `FOREACH (a, b) IN xs DO C` (lane destructure) | ✓ | Tier 6 |
| `SWITCHON E INTO $( CASE k: C ... DEFAULT: C $)` | ✓ | Tier 4 |
| Multi-label cases (`CASE 1: CASE 2: stmt`) | ✓ | Tier 4 fall-through probe |
| `GOTO label` | ⚠ | Parses + lowers via label_block; no end-to-end probe |
| `name:` label declaration | ⚠ | Same; works through GOTO but no isolated probe |
| `RETURN`, `RESULTIS`, `FINISH` | ✓ | Tier 4 |
| `BREAK`, `LOOP`, `ENDCASE` | ✓ | Tier 4 + tier 5 USING-cleanup probes |
| `BRK` debugger breakpoint | ⚠ | Parses; not lowered |

## §4 — Declarations

| Form | Status | Notes |
|---|---|---|
| `LET name = expr` | ✓ | Tier 2 |
| `LET name AS Type = expr` annotation | ✓ | Sema reads it |
| `LET a, b = ...` multi-binding | ✓ | Tier 2 |
| `FLET` float binding | ✓ | Tier 2 |
| `MANIFEST $( N = K; ... $)` | ✓ | Tier 2; lowering substitutes inline |
| `STATIC $( N = K; ... $)` | ✓ | Tier 2 |
| `GLOBAL name = expr` (single) / `GLOBAL $( name = expr; ... $)` (block) | ✓ | Each binding becomes a module-level `@<name>` LLVM global. Cross-routine, cross-module reads/writes work end-to-end. Tier 4 probes. |
| `GLOBAL name : K` (classic slot-pinning shape) | Δ | Rejected by the parser. The slot-vector form is the legacy GLOBALS shape (see below); under `GLOBAL` it's a category error. |
| `GLOBALS $( name : slot; ... $)` (classic global-vector) | Δ | Deliberately not supported — the loader's symbol table replaces the global-pointer-vector linker that GLOBALS was designed for. Parser rejects with a hint pointing at `GLOBAL`. |
| `LET v = VEC k` | ✓ | Tier 6 |
| `LET v = FVEC k` | ✓ | Tier 6 |
| `LET F(p) = expr` function | ✓ | Tier 3 |
| `LET R(p) BE stmt` routine | ✓ | Tier 4 |
| `FUNCTION` / `ROUTINE` keyword forms | ✓ | Parser tests |
| `AND` mutual recursion | ✓ | Parser test for `LET ... AND ...`; runtime check thin |
| `GET "file"` include | ✓ | AST-level splicing: sibling-file first, then modules-active fallback so a module doubles as a header. Cycle detection via depth cap. |
| `RETAIN name` / `RETAIN x = expr` | ⚠ | Parses; no probe verifying it keeps a value past scope |
| `FREEVEC` / `FREELIST` | ⚠ | Accepted as no-op (GC owns lifetime); pinned that they don't error |

## §5 — Program structure

| Feature | Status | Notes |
|---|---|---|
| Section brackets `$( ... $)` | ✓ | Tier 1 |
| `{ ... }` C-style synonyms | ✓ | Tier 1 |
| Mixed `$(` ... `}` (cross-form) | ⚠ | Lexer accepts; parser allows either pair but not crossed — undocumented |
| Tagged section brackets `$(LOOP ... $)LOOP` | ✗ | Lexer tokenises them; parser only matches bare `$(` / `}`. Not a corpus blocker — no reference test uses them. |
| Compound commands | ✓ | |
| Blocks with `LET` declarations | ✓ | |

## §6 — Float extension

Per `BCPL float extension.md`: IEEE 754 doubles, dotted operators,
`FLOAT(n)` / `TRUNC(f)` conversions, `.%` float subscript.

All ✓ — tier 3 covers the operator family; tier 6 covers `.%` reads.
`FLOAT` and `TRUNC` are builtins in `newbcpl-runtime/src/builtins.rs`.
Extras `ENTIER`, `FSQRT`, `FSIN`, `FCOS`, `FTAN`, `FABS`, `FLOG`,
`FEXP` are also live.

## §7 — Char/string model

| Reference says | NewBCPL does | Status |
|---|---|---|
| 32-bit Unicode char (`'A'` → i32 65) | UTF-8 byte (`'A'` → i64 65, but 2-byte chars work as multi-byte sequences) | Δ |
| String = pointer to 32-bit chars, null-terminated by zero word | String = pointer to UTF-8 bytes, null-terminated by zero byte | Δ |
| `%` returns 32-bit char | `%` returns a byte (i64-extended) | Δ |

These are **intentional deviations** documented in the user guide §2.6
("strings are UTF-8 bytes"). Stays cheap to interop with C/Win32 ANSI
APIs and the runtime printers. Not a gap, but worth pinning in a probe
that uses a multibyte UTF-8 sequence so the convention is unambiguous.

## §8 — Classes (modern extension)

Per `classes_and_objects.md`:

| Feature | Status | Notes |
|---|---|---|
| `CLASS Name $( ... $)` declaration | ✓ | Tier 5 |
| `DECL` fields | ✓ | Tier 5 |
| `LET` / `FLET` fields (no init) | ✓ | Tier 5 |
| `LET f = expr` initialised fields | ✓ | Tier 5 — landed this cycle |
| `DECL f AS Class` annotation | ✓ | Tier 5 — landed this cycle |
| `FUNCTION` / `ROUTINE` methods | ✓ | Tier 5 |
| `NEW Class(args)` | ✓ | Tier 5 |
| `obj.field` / `obj.method()` | ✓ | Tier 5 |
| Chained `o.a.b.c()` dispatch | ✓ | Tier 5 — landed this cycle |
| `EXTENDS` single inheritance | ✓ | Tier 5 |
| `SUPER.method()` | ⚠ | Parser + sema know SUPER; method-dispatch through it lacks a probe verifying parent's body runs |
| `CREATE` / `RELEASE` slots 0 / 1 | ✓ | Tier 5 |
| `PUBLIC` / `PRIVATE` / `PROTECTED` | ✓ | Enforced by sema. Visibility check at every `obj.field` and `obj.method()` site; `PRIVATE` requires access from the declaring class, `PROTECTED` extends to descendants. Sema's new `errors` channel rejects offenders; the driver refuses to proceed to IR/codegen. Tier 5 probes (`public_*`, `private_*`, `protected_*`). |
| `VIRTUAL` / `FINAL` modifiers | ⚠ | Parsed and surfaced in vtable; no override-via-VIRTUAL probe |

## §9 — Memory model

Per `BCPL Runtime.md`:

| Reference says | NewBCPL does | Status |
|---|---|---|
| `VEC k` malloc-based with scope-exit free | GC-managed | Δ — by design (manifesto §3) |
| Global vector with integer offsets | Symbol table | Δ — same |
| `findinput` / `selectinput` / `endread` I/O | Not implemented; we have stdin via `RDCH` | ✗ — would be useful for corpus parity |
| `findoutput` / `selectoutput` / `endwrite` | Not implemented | ✗ |
| `stop(N)` | We have `FINISH` (no arg) | Δ — minor |

## §10 — Runtime library

| Reference says | Our builtin | Status |
|---|---|---|
| `WRITES`, `WRITEN`, `WRITEC`, `WRITEF`, `NEWLINE` | All four + `WRITEF1..7` arity variants | ✓ |
| `WRITEF` float specifier `%f` `%F` | ✓ | Tier 3 |
| `RDCH` | ✓ | Stdin byte read |
| `FWRITE` | ✓ | Float printer |
| `FINISH` | ✓ | |
| `GETVEC` / `FGETVEC` | ✓ | Heap allocators |
| `FREEVEC` | ⚠ | No-op stub; GC owns lifetime |
| List ops `HD TL REST LEN CONCAT APND APND_*` | ✓ | Tier 6 |
| Math `FSIN FCOS FTAN FABS FLOG FEXP FSQRT FLOAT FIX TRUNC ENTIER` | ✓ | |
| `RAND` / `RND` / `FRND` random | ✓ | |
| `GC()` / `HEAP_INFO()` | ✓ | Tier 6 |
| `__newbcpl_test_panic()` | ✓ | Test fixture for SEH probes |
| GUI `iGui_*` family | ✓ | Windows-only; not pinned by probes (GUI tests are out of scope here) |

## Identified gaps, ranked by leverage

### High-leverage (real holes)

1. **`SUPER.method()` end-to-end probe.** Sema knows SUPER; we have no test that proves the parent's method body actually runs. If the vtable patch logic misroutes SUPER calls, no probe would catch it. *Add: 1 probe, 5 min.*

2. **`VIRTUAL` / `FINAL` override probe.** Parser + layout know about these; no probe pins that a `VIRTUAL` method is actually dispatched dynamically (i.e. a subclass override runs through a base-class pointer). *Add: 1 probe, 5 min.*

3. **`RETAIN` end-to-end probe.** `RETAIN x` is supposed to prevent the GC from reclaiming `x`. We need a probe that allocates, retains, GCs, then re-reads. *Add: 1 probe, 10 min.*

4. **`GLOBAL` slot-pinning behavioural probe.** Classic BCPL programs use `GLOBAL $( name : 42 $)` to link modules through fixed offsets. We parse it; if it doesn't actually link, corpus tests will silently produce wrong values. *Add: 1 probe + investigation.*

5. **`GOTO` / label end-to-end probe.** Same shape — parses, lowers, no behavioural test. *Add: 1 probe, 5 min.*

### Medium-leverage (advisory)

6. **`findinput` / `findoutput` family.** Old-BCPL file I/O. Not in user guide. Skip unless the corpus blocks on it.

### Low-leverage (cleanup)

9. **Tagged section brackets `$(LOOP …)$)LOOP`.** Lexer tokenises; parser doesn't pair. No reference test uses them. *Defer indefinitely.*

10. **Char-model UTF-8 vs 32-bit divergence.** Already documented in user guide as our convention. Add a multibyte UTF-8 probe to pin the behaviour so future refactors don't drift it.

11. **`EQV` / `NEQV` behavioural probes.** Operators parse; no test asserts the output. *Add: 2 probes, 5 min.*

## Action plan

For the **basic-feature sweep** the user asked for, items 1–5 plus
10–11 are the right batch. They're all "feature exists in code but
isn't pinned by tests" — exactly the matrix gap. Doing them all
together is one commit, ~9 probes, mostly mechanical.

Items 6, 7, 8 are real *implementation* work and should be separate.

The Δ rows are not gaps — they're language design choices that the
user guide already explains. The audit's value here is making sure
we don't accidentally drift back toward the reference behaviour.

## Sweep results (post-audit pass)

The basic-feature sweep ran and found three real bugs the audit had
listed as "thin" — exactly the kind of thing this exercise was for:

* **`SUPER.method()` didn't dispatch.** `SUPER` was treated as just
  an identifier; the call fell through to a generic name lookup and
  the JIT reported `missing builtin: SUPER`. Fix: `class_name_of_expr`
  returns the parent class for `SUPER`, and `lower_call` emits a
  *direct* call to `<parent>_<method>` instead of vtable dispatch
  (so SUPER.CREATE can't recurse into its own CREATE). `ClassLayout`
  now carries `extends` so the IR can look up the parent.

* **`%ptr` / `v%i` did word loads.** Marked "KNOWN GAP" in
  `emit.rs`: the IR's `IndirectLoad` hint said INT but the codegen
  emitted `load i64`, returning eight bytes worth of garbage from a
  single-byte index. Fix: `IndirectLoad` and `IndirectStore` now
  carry an explicit `byte_width`; `byte_width=1` emits `load i8 +
  zext i64` (read) or truncate-to-i8 + store (write).

* **`RETAIN x = expr` didn't declare the binding.** IR comment said
  "subsequent chunks lower these" — never happened. Fix: lower as
  equivalent to `LET x = expr`; the GC tracks the stack root the
  same way, satisfying the user-visible "survives a GC()" contract.

* **`LET f(p AS Class)` parameter annotation doesn't parse.**
  Discovered while writing the SUPER probe. Not in scope for the
  sweep — recorded as a separate gap.

* **`GLOBAL $( name : 42 $)`** confirmed as a real implementation
  gap (not a probe gap). Updated status to ✗.

After the sweep, the matrix gains ~16 probes across tiers 3, 4, 5,
6 and the workspace stays at 33 binaries green.
