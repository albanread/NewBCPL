# NewBCPL Test Strategy — language-spec coverage

Drafted against the BCPL dialect surface implemented in this workspace
(the reference NBCPL's superset: classes, SIMD lane types, GC, real
linked lists, FOREACH destructuring). Borrows structure from
[NewCP's `docs/test_matrix.md`](../../NewCP/NewCP/docs/test_matrix.md)
because the premise is the same: a workspace without an IDE to dogfood
needs systematic synthetic coverage more than it needs a depth-search
of any one feature.

## The premise

Three observations, identical in shape to NewCP's:

1. **Most bugs we've hit are obvious feature combinations no test
   touched.** The PAIR-as-`<2 x i64>` representation gap, the FOREACH
   list-vs-vec dispatch split, the vtable-default-slot null-call, the
   `current_y`-cross-scope panic — all surfaced by running real corpus
   files, not by targeted probes. The harness reveals them; the cost
   of writing a probe to catch each one *before* the corpus does is
   nearly zero.

2. **The JIT hides bugs that don't fire.** A vtable slot that nobody
   calls reads as zero forever. A type-mismatched store into a list
   atom silently writes the low 32 bits and the program still prints
   a plausible number. Tests that don't assert on output are weak.

3. **Our corpus is the reference's old test suite — not ours.** The
   857-file `reference/tests/bcl_tests/` is canonical but mixed: real
   tests, library files without `START`, debug printouts, dialect
   experiments. Pass count there is a moving target, not a coverage
   metric. We need a synthetic matrix where every cell is ours,
   tagged with the feature it exercises.

## Eight tiers

### Tier 1 — Lexical & syntactic acceptance

**Purpose**: confirm the lexer + parser accept every well-formed
surface construct in our BCPL dialect.

The matrix:

| Axis             | Values |
|------------------|--------|
| Number literal   | decimal, hex `#X1A`, octal `#777`, float `3.14`, float exponent `1.5e3` |
| Char literal     | ASCII `'a'`, escape `'*N'`, hex `'*X41'`, unicode |
| String literal   | ASCII, escape (`*N`, `*T`, `*S`, `*"`, `**`), multi-line |
| Operator form    | each arithmetic op in plain (`+`), float-dot (`+.`), and float-hash (`+#`) flavours; each comparison op in those three flavours |
| Subscript form   | `v!i` (word), `v%i` (byte), `v.%i` (float) |
| Lane access      | `pair.|0|`, `quad.|2|`, `oct.|7|`, runtime index |
| Conditional expr | `c -> a, b`, nested ternary |
| Block delimiters | `$( ... $)`, `{ ... }`, mixed |
| Comment          | `//` line, `/* ... */` block, nested block |

**Negative corpus**: malformed identifiers (digit-leading), unclosed
strings, unbalanced brackets, reserved-word-as-identifier, malformed
number suffixes. Each paired with an expected diagnostic substring.

Cheap (~50 fixtures), bug-class-wide coverage.

### Tier 2 — Sema (type hints, scopes, MANIFEST, classes)

**Purpose**: the "secretly typed" half of the manifesto.

The Cartesian product:

| Axis              | Values |
|-------------------|--------|
| Binding form      | `LET x = expr`, `FLET x = expr`, `LET x AS Type = expr`, `MANIFEST { x = n }`, `STATIC { x = n }`, `GLOBAL { x : 42 }`, `LET x, y = pair` (destructure) |
| Type hint source  | initialiser-inferred, `FLET`-overridden, `AS`-annotation-overridden, MANIFEST substitution |
| Scope             | local (function), nested (block), class field, class method, module-level |
| Annotation shape  | plain identifier (`INTEGER`), pointer (`^STRING`, `POINTER TO X`), generic (`LIST OF INTEGER`, `^LIST OF VECTOR OF INTEGER`) |
| Class layout      | DECL-only fields, LET-only fields, mixed, single-inheritance EXTENDS chain, MANAGED |
| Method declaration| `ROUTINE name() BE stmt`, `FUNCTION name() = expr`, `ROUTINE name() = expr`, `FUNCTION name() BE stmt`, `LET name(...) BE stmt`, `LET name(...) = expr`, VIRTUAL / FINAL prefix |

**Negative sema fixtures**: a binding-aliased MANAGED instance
(manifesto §5), a CLASS with a method-name collision, a FOREACH
destructuring with wrong arity for the element type — each paired
with an expected warning substring.

### Tier 3 — Expressions

**Purpose**: every operator on every operand type.

- **Arithmetic** (`+ - * / REM`): each combination of {int, float, pair,
  pointer-as-word} × the integer-flavour vs the two float-flavour
  spellings (`+.`, `+#`). Each cell asserts both compile-time success
  and a numeric result.
- **Relational** (`= ~= < <= > >=` and float-flavoured `.` / `#`
  forms): comparable cells include int×int, float×float, pair×pair
  (lane-wise equality), pointer×NULL, vector-pointer×0.
- **Bitwise / logical** (`& | ^ << >> NOT EQV NEQV`): width and sign
  edge cases.
- **Subscript family** (`!`, `%`, `.%`): each element type, on a vector
  produced by `VEC k`, `VEC [a, b, c]`, `VEC(a, b, c)`, `PAIRS k`,
  `FPAIRS k`, `LIST(...)`.
- **SIMD lane access** (`pair.|n|`): each kind (PAIR / FPAIR / QUAD /
  FQUAD / OCT / FOCT) × constant lane index × runtime lane index.
- **Conditional expression** (`c -> a, b`): scalar, float, pair,
  pointer; nested; side-effecting then/else branches.

### Tier 4 — Statements

**Purpose**: every statement form × every control-flow shape.

- **Assignment**: scalar / pair (`LET a, b = pair` destructure) /
  vector / list / class field (via `obj.field := v` and via `SELF.x`
  inside method).
- **IF / UNLESS / TEST**: dead branches, constant-condition folding,
  nested.
- **WHILE / UNTIL / REPEAT / REPEATWHILE / REPEATUNTIL**: empty
  body, body with BREAK / LOOP, deeply nested with BREAK from inner.
- **FOR i = a TO b [BY c]**: positive step, negative step, step
  that doesn't divide range evenly.
- **FOREACH**:
  - over VEC (index-walk path)
  - over LIST (linked-walk path)
  - destructuring on list-of-PAIRs `(a, b)`
  - destructuring on list-of-QUADs `(a, b, c, d)`
  - empty iterable
  - early BREAK
- **SWITCHON / CASE / DEFAULT / ENDCASE**: integer cases, character
  cases, sparse vs dense, ENDCASE inside nested CASE.
- **GOTO / labels**: forward, backward, into a nested block, across
  loops (must respect frame).
- **RETURN / RESULTIS / FINISH**: from mid-VALOF, from inside loop,
  early termination.
- **Mutual recursion**: classical `LET f(...) = e AND g(...) = e`
  chain (parser disambiguates `AND <ident> (` from the
  precedence-3 logical operator); also the consecutive-LETs form
  that relies on sema's preregistration pass.
- **BRK**: signal-safe state dump — banner with routine + line,
  heap summary, AMD64 register state via `RtlCaptureContext`,
  stack walk via `RtlVirtualUnwind` with frames resolved to BCPL
  routine names. Program continues after BRK; verify by output
  that only appears after the statement runs.

### Tier 5 — Classes & methods — *where bugs cluster*

This is the tier the recent class-shape bugs lived in (LET-vs-DECL,
ROUTINE-`=`-expr, default-RELEASE-slot, BE-class-body). Aggressive
enumeration goes here.

| Axis             | Values |
|------------------|--------|
| Field decl form  | `DECL x, y`, `LET x, y`, `LET x = init`, `FLET x = 0.0`, mixed |
| Method form      | `ROUTINE m() BE stmt`, `FUNCTION m() = expr`, `ROUTINE m() = expr`, `FUNCTION m() BE stmt`, `LET m(...) BE stmt`, `LET m(...) = expr`, with / without `VIRTUAL`, with / without `FINAL` |
| Class shape      | bare `CLASS Name $(...)`, `CLASS Name { ... }`, `CLASS Name BE { ... }`, `EXTENDS Base`, `MANAGED`, `EXTENDS Base MANAGED` |
| Vtable slot      | declared CREATE, default CREATE, declared RELEASE, default RELEASE, declared method overriding inherited slot, declared method extending vtable |
| Dispatch site    | direct call (`obj.m()`), nested call (`obj.m1().m2()`), CREATE-from-NEW, RELEASE-on-scope-exit, method-on-`SELF`, method-on-`SUPER` |
| Cross-method ref | bare field name inside method body (SELF-relative), explicit `SELF.x`, sibling method call |

**High-value non-Cartesian cases**:

- **Vtable slot 0 / 1 defaults**: every class has implicit CREATE and
  RELEASE; the runtime's `__newbcpl_default_method` stub must fill the
  unbound slots. Test: `NEW Foo` on a class with no CREATE, `obj.RELEASE()`
  on a class with no RELEASE.
- **Inherited methods**: `CLASS B EXTENDS A` with A.foo and no B.foo
  — vtable B.foo slot must contain A_foo's address.
- **Field offset stability under inheritance**: B's fields land after
  A's; offsets in B's methods must read through the inherited prefix.
- **FINAL enforcement**: subclass methods that override a FINAL
  ancestor are rejected by sema; the diagnostic names both the
  method and the defining class. Walk covers the full inheritance
  chain (Base FINAL → Mid → Sub override is also rejected).
- **PRIVATE / PROTECTED enforcement**: visibility checked at every
  `obj.field` and `obj.method()` site against the access-site's
  enclosing class.
- **Parameter type annotations**: `LET f(p AS Class) = ...`
  attaches class identity to the parameter binding so member access
  through `p` resolves statically; visibility checks fire as if `p`
  were a class-typed local.
- **Indirect method dispatch**: un-annotated receivers resolve
  through `__newbcpl_lookup_method(receiver, name)` at runtime —
  the same `helper(obj)` should route to different classes' methods
  based on what gets passed in (polymorphic shape).

### Tier 6 — GC & runtime

**Purpose**: prove the heap behaves like the type system says it does.

- **Allocation surfaces**: `NEW Class`, `VEC k`, `VEC [...]`, `VEC(...)`,
  `FVEC k`, `LIST(...)`, `MANIFESTLIST(...)`, `PAIRS k`, `FPAIRS k`,
  `GETVEC(n)`, `FGETVEC(n)`. Each must:
  - Return a non-null pointer.
  - Be zero-initialised (where applicable).
  - Carry a correct length header (where applicable — VEC family).
  - Increment `HEAP_COUNTERS.alloc_blocks_lifetime`.
- **`__newbcpl_alloc_rec` TypeDesc interning**: distinct payload sizes
  produce distinct interned TypeDescs; the same size reuses one.
- **`collect()` cycles**:
  - Threshold trigger fires inside `__newbcpl_new_rec` when
    `alloc_bytes_since_collect >= collect_threshold`.
  - Threshold adapts to `max(INITIAL, live_bytes * 2)` after each
    cycle.
  - `GC()` builtin from JIT'd code triggers an explicit cycle.
  - Conservative stack scan keeps a still-referenced VEC alive
    across a cycle; the cycle reclaims an unreferenced VEC.
  - `collect()` is safe to call after `run()` returns
    ([jit_typedesc_lifetime.md](jit_typedesc_lifetime.md) Fix B).
- **Cooperative scheduling**: function-entry and loop back-edge
  safepoint polls park the mutator when `SAFEPOINT_REQUESTED` is set.
- **List runtime**: `APND` / `APND_FLOAT` / `APND_STRING` / `APND_PAIR`
  produces a chain whose head matches the first appended value;
  `CONCAT` builds a fresh list whose atoms equal `a`'s then `b`'s;
  `TL` shares the existing chain (O(1)); `LEN` is O(1).

### Tier 7 — SIMD lane types

The widths documented in [pair_and_multilane_types.md](pair_and_multilane_types.md)
are normative. Test:

| Type   | Lane shape       | Storage | Lane-extract spell  |
|--------|------------------|---------|---------------------|
| PAIR   | 2 × i32          | i64     | `(packed << pad) >> total` arithmetic |
| FPAIR  | 2 × f32 (bit-cast)| i64     | int extract → f32 bitcast → f64 widen |
| QUAD   | 4 × i16          | i64     | same shift recipe with lane_bits=16 |
| OCT    | 8 × i8           | i64     | same with lane_bits=8 |
| FQUAD  | 4 × f32          | `<4 x f32>` | LLVM `extractelement` |
| FOCT   | 8 × f32          | `<8 x f32>` | LLVM `extractelement` |

Probes per type: construct via `KIND(a, b, ...)`, lane-access each
position with constant index, lane-access with runtime index, store
into a list-of-KIND, FOREACH destructure out, do element-wise
arithmetic (when implemented).

### Tier 8 — Integration & corpus

The reference's 857-file corpus is the integration tier. Coverage
metrics:

- **Overall pass rate**: target 80%+ on the corpus subset that has a
  `START` routine.
- **Class subset**: every `grep=CLASS` test that doesn't use exotic
  Pascal-style headers (already filtered).
- **FOREACH subset**: every `grep=FOREACH` test runs end-to-end.
- **GC stress**: tests that run long enough to trigger at least one
  auto-collect (currently 4 MB allocations).

Corpus pass count is tracked per-session in commit messages; the
absolute number is less interesting than the trend.

## Infrastructure

What we'd build to support this:

1. **Probe generator** — a Rust binary in the workspace
   (`src/newbcpl-test-matrix/`) that reads a small DSL describing one
   row in the matrix and emits a `.bcl` fixture into
   `tests/matrix/Tier{N}/`. Each fixture is paired with an expected
   output string. A single Rust `#[test]` per fixture runs it through
   `newbcpl_llvm::run` and asserts stdout match.

2. **`test-folder` harness extension**. The existing
   `newbcpl-driver test-folder` already supports `start=N stop=M
   grep=text`. Add `manifest=path/to/file.txt` so the harness can
   read a list of fixture names + expected outputs from a manifest,
   and compare captured stdout to expected. This turns the corpus
   sweep into a regression suite.

3. **Negative-test runner**. A `tests/negative/` directory with
   `.bcl` fixtures plus a sibling `.expected_error` text file
   containing a diagnostic substring. One Rust test iterates the
   directory.

4. **Coverage report**. Tag each probe with the tier and feature
   axis it exercises. CI artifact renders a coverage matrix as
   plain text — every cell coloured by probe count. Don't gate on
   it, just make blind spots visible.

5. **Optimization-disabled lane**. Right now MCJIT runs at
   `OptimizationLevel::Default` (mem2reg + simple folding). Add an
   `OptimizationLevel::None` lane for the matrix probes — DCE has
   already hidden one runtime bug (a stray reference in the
   vtable test), and any future ABI-shape regression will hide
   the same way.

## Phasing

Given finite time, the order:

1. **First**: Tier 5 (class × method × dispatch). That's where every
   recent class-shape bug has been. Probe generator + ~50 fixtures.
   Probably 1 sitting of work; catches the next round of bugs in
   the same neighbourhood.

2. **Then**: Tier 6 (GC) corner cases. Each runtime change we've
   landed (vtable patch, safepoint polls, threshold trigger,
   list runtime) gets a regression probe so a future refactor
   that breaks it surfaces immediately.

3. **Then**: Tier 1's negative corpus (lex + parse rejection).
   Cheap to write, high signal — every parse rule we have gets
   a "you must reject this" guardrail.

4. **In parallel with feature work**: when a feature lands, add
   its matrix row simultaneously. The pair-destructuring tests
   we already have are this pattern.

5. **Wishlist**: Tier 8's manifest-driven corpus runner. Becomes
   real when we get past ~85% on the corpus and want to lock in
   the exact passing set.

## What's landed so far

- The eight-tier matrix has **316 probes** across 17 integration-
  test binaries. `cargo test -p newbcpl-tests --tests` runs them all
  in a few seconds; every spec row in
  [reference_audit.md](reference_audit.md) names the probe(s) that
  pin it. Tier 5 has grown most this cycle (class-shape, FINAL,
  visibility, param annotations, indirect dispatch — bug-cluster
  tier as predicted).
- The `test-folder` harness:
  `newbcpl-driver test-folder reference/tests/bcl_tests [start=N stop=M] [grep=text] [skip=text] [report-path]`.
  Subprocess-per-file with a per-test timeout; classifies failures
  by phase (parse / sema / IR / LLVM / run / crash); captures
  stdout for passes and stderr for failures into a text report.
  `skip=` (repeatable) quarantines out-of-scope tests — `SDL2_` is
  the standard exclusion for the Direct2D-only dialect.
- The corpus journal in
  [corpus_sweep.md](corpus_sweep.md) tracks each iteration's pass
  rate; eight sweeps brought 451 → 539 (59.3 % → 70.9 %
  effective).
- Document trail: this matrix, [reference_audit.md](reference_audit.md),
  [corpus_sweep.md](corpus_sweep.md), [user_guide.md](user_guide.md),
  [pair_and_multilane_types.md](pair_and_multilane_types.md),
  [jit_typedesc_lifetime.md](jit_typedesc_lifetime.md),
  [manifesto.md](manifesto.md).

The matrix-generator binary is **not** built yet — this document is
the spec for it. The matrix has been growing well organically;
generation is an optimisation for when the manual approach gets
expensive.

## Maintenance principles

- **Probe-per-cell, generated from a manifest.** Adding a feature
  means adding rows, not handwriting N probes.
- **Tag every probe with the tier × feature axis it covers.**
  Coverage report is the artifact.
- **One regression probe per fixed bug, but write the row too.**
  The bugs we've fixed (PAIR-as-i64, vtable default slots, AS
  annotations, LET-fields) each correspond to a matrix row that
  was empty. Backfill those rows so the next bug in the same
  neighbourhood is impossible.
- **Don't gate on coverage.** Gate on green tests. Coverage is a
  planning tool.

## What this is *not*

- Not property-based / fuzz testing. The bug-class signal isn't
  there — see the rationale above.
- Not parity testing against the reference NBCPL. Our dialect is a
  superset (GC, real lists, runtime-interned TypeDescs) and we
  don't reproduce the reference's runtime bit-for-bit. The corpus
  is a workload, not an oracle.
- Not formal verification. BCPL semantics are documented well
  enough that the matrix *is* the verification, the same way
  CP's is for NewCP.

---

**TL;DR**: Tier 5 (class × method × dispatch) goes first because
that's where every recent bug has been; Tier 6 (GC) regression
probes second so the heap integration doesn't quietly regress; Tier
1 negative corpus third because it's cheap and the sema rules
deserve guardrails. The probe-generator binary is the unblocker —
without it, every new test is hand-written and the matrix shape
gets lost in noise.
