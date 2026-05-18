# NewBCPL

**Under development, and incomplete**
This is a JIT not designed to create executable files yet.

NewBCPL is a recreation of the modern BCPL dialect prototyped at [github.com/albanread/NBCPL](https://github.com/albanread/NBCPL). Built on Rust + LLVM, targeted at `x86_64-pc-windows-msvc`, with an integrated Direct2D + DirectWrite GUI in the spirit of [NewCP's iGui](../NewCP/NewCP/README.md).

The design contract is in [`docs/manifesto.md`](docs/manifesto.md); the K&R-style user guide is [`docs/user_guide.md`](docs/user_guide.md). The reference implementation under `reference/` is the language specification this workspace builds against.

## Status

End-to-end pipeline. Source → lex → parse → sema → IR → LLVM emit → MCJIT → run, with a precise mark-sweep GC, a module loader, a Direct2D / DirectWrite GUI, and an integrated bedit editor that JITs the current buffer (`newbcpl-driver gui`).

### What works

- **`newbcpl-lexer`** — full BCPL surface (classic and dotted-float operators, section brackets, `*`-escapes, `#`/`#X` numbers, `%%` bitfield, `?` null literal).
- **`newbcpl-parser`** — recursive-descent over the dialect: `LIST` / `MANIFESTLIST`, `CLASS` / `EXTENDS` / `VIRTUAL` / `FINAL` / `MANAGED` / `SUPER`, `FOREACH` with lane destructuring, `AS` annotations (including per-parameter `LET f(p AS Class)`), `RETAIN`, multi-target assignment, `VEC(…)` / `FVEC(…)` paren-init, `FUNCTION` / `ROUTINE` keyword forms, classical `LET f(…) = e AND g(…) = e` mutual recursion with three-token lookahead to disambiguate from logical `AND`.
- **`newbcpl-sema`** — register-class type inference per manifesto §1. Walks the tree once, attaches a hint to every expression, never errors on type grounds. Class layouts (fields, vtable slots, pointer-offset arrays) computed up front. `AS` annotations propagate through opaque reads and now through routine / function / method parameters. Hard-diagnostic channel rejects `PRIVATE` / `PROTECTED` visibility violations and `FINAL` override attempts; the driver refuses to proceed to codegen on sema errors.
- **`newbcpl-ir` / `newbcpl-llvm`** — typed lowering then codegen via Inkwell (LLVM 22). MCJIT today, with a custom memory manager that captures `.pdata` / `.xdata` / `.text` sections and registers Windows SEH unwind tables via `RtlAddFunctionTable` so a Rust panic from a runtime helper unwinds cleanly back through any depth of JIT frames. `NEW Class`, vtables, `FieldLoad` / `FieldStore`, `VALOF`, every loop form, `SWITCHON`, `GEP` / indirection / lane access, PAIR-as-`<2 x i64>`, SIMD packs, real cons-cell lists, cooperative GC safepoints at function entry and every loop back-edge. Method dispatch has both a fast statically-resolved `MethodCall` path and a name-keyed `IndirectMethodCall` path for un-annotated receivers; the indirect path consults a process-global `(vtable → method_names)` registry populated at JIT-finalize.
- **`newbcpl-runtime`** — precise mark-sweep tracing GC (port of NewCP's `gc.rs`): per-thread TLABs, stop-the-world via safepoints, sentinel-terminated pointer-offset arrays per type, finalizer support. Allocation-rate auto-trigger, explicit `GC()` and `HEAP_INFO()` builtins. Runtime-side `TypeDesc` interning lets `collect()` survive JIT runs. Includes the standard library (`WRITES`/`WRITEF`/`WRITEN`/`WRITEC`, `FLOAT`/`FIX`/`TRUNC`/`ENTIER`, `FSIN`/`FCOS`/…/`FSQRT`, list ops, `RAND`/`RND`/`FRND`, typed allocators `IGETVEC`/`SGETVEC`/`PGETVEC`/`QGETVEC`) and the Direct2D + DirectWrite GUI (`iGui_*` builtins for windows, draw batches, text panes, event mailbox). `BRK` is a real statement here: lowers to `__newbcpl_brk(routine_name, line)` which writes a signal-safe-ish state dump (banner / heap summary / AMD64 register state / stack walk via `RtlVirtualUnwind` with BCPL routine names resolved against the JIT symbol registry) to stderr without halting the program.
- **`newbcpl-loader`** — module discovery in `./modules-active/` (or `$NEWBCPL_MODULES_ACTIVE`). `<stem>_<routine>` name mangling; backward and forward cross-module references resolve at link time. Ships `igui` / `maths` / `geom` modules.
- **`newbcpl-driver`** — all phase dumps live: `dump-tokens`, `dump-ast`, `dump-sema`, `dump-ir`, `dump-llvm`, `dump-asm`. `run` JITs and executes. `gui` opens the bedit + log frame and `Ctrl+R` runs the active buffer. `test-folder <dir>` JITs every `.bcl` under a directory and emits a report.

### Tests

`cargo test --workspace` is green: 316 probes across 17 integration-test binaries — lexer / parser / sema unit tests, plus the eight-tier synthetic probe matrix (`tests/newbcpl-tests/tests/matrix_tier{1..7}.rs`, `matrix_extra.rs`, `matrix_generated.rs`). `tests/user_guide.rs` re-parses every fenced ```bcpl block in `docs/user_guide.md` and every `examples/*.bcl` on each run so the docs cannot rot silently. The matrix is **the** quality gate; every spec row in `docs/reference_audit.md` points at the probe(s) that pin its behaviour.

The reference 857-file corpus (`reference/tests/bcl_tests/`) is a *coverage signal*, not a regression gate. Eight iterations of bug-fixing brought it from 451 → 539 passing (59.3 % → 70.9 % effective after dropping out-of-scope tests). See `docs/corpus_sweep.md` for the per-iteration journal. With the systematic gaps now closed, remaining failures are individual test-file issues (undefined user functions, parser quirks, output mismatches) — interesting one at a time but no longer the right macro signal.

### Resource cleanup

The deterministic-cleanup story is `USING name = expr DO body`, which
binds `name` for the body and then calls `name.RELEASE()` at scope
exit. Cleanup runs on every way out — fall-through, `RETURN`,
`RESULTIS`, `FINISH`, `BREAK`, `LOOP`, `ENDCASE` — innermost-first, so
nested USINGs release in stack order. This replaces the earlier
MANAGED linear-type design: the GC handles "don't leak", USING handles
"release in order". The `MANAGED` keyword still parses for backward
compatibility but is advisory now; any class with a `RELEASE` method
works in a USING block.

### Known gaps

- ORC v2 alongside MCJIT — a separate backend, parked while v1 is
  hardened.
- `GOTO` does not yet fire USING cleanups (BREAK/LOOP/ENDCASE do).
- Per-frame BCPL line numbers in `BRK` — frames currently show the
  routine name; the source-line mapping needs an IR→linemap pass
  threaded through to the JIT debug table.
- Rare `STATUS_ACCESS_VIOLATION` on `newbcpl-runtime` test-process
  teardown (a static-destructor ordering issue, not a test-time race).

## Workspace layout

Mirrors [NewCP](../NewCP/NewCP/README.md):

```
src/
  newbcpl-lexer/        full BCPL surface
  newbcpl-parser/       recursive descent + dialect extensions
  newbcpl-sema/         register-class inference, class layouts
  newbcpl-ir/           typed lowering
  newbcpl-llvm/         Inkwell codegen, MCJIT
  newbcpl-runtime/      GC, stdlib, Direct2D/DirectWrite GUI
  newbcpl-loader/       module discovery & link
  newbcpl-driver/       phase-visible CLI
  newbcpl-test-matrix/  generator for the synthetic probe matrix
tests/
  newbcpl-tests/        eight-tier matrix + corpus sweeps + doc lock
docs/
  manifesto.md          design contract
  user_guide.md         K&R-style language guide
  test_matrix.md        the eight-tier coverage spec
examples/               worked programs referenced by the guide
modules-active/         loaded automatically on every run
reference/              original NBCPL — the language spec
```

## Driver

```
newbcpl-driver dump-tokens  <path>
newbcpl-driver dump-ast     <path>
newbcpl-driver dump-sema    <path>
newbcpl-driver dump-ir      <path>
newbcpl-driver dump-llvm    <path>
newbcpl-driver dump-asm     <path>
newbcpl-driver run          <path>
newbcpl-driver gui          <path>    (Windows only — bedit + log view)
newbcpl-driver test-folder  <dir> [report]
newbcpl-driver bootstrap                (loader status)
```

## Lineage

The original NBCPL is developed at [github.com/albanread/NBCPL](https://github.com/albanread/NBCPL), targeting Apple Silicon ARM64. NewBCPL is its Windows-first, LLVM-backed successor.

 
