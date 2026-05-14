# NewBCPL

NewBCPL is a recreation of the modern BCPL dialect prototyped at [github.com/albanread/NBCPL](https://github.com/albanread/NBCPL). Built on Rust + LLVM, targeted at `x86_64-pc-windows-msvc`, with an integrated Direct2D + DirectWrite GUI in the spirit of [NewCP's iGui](../NewCP/NewCP/README.md).

The design contract is in [`docs/manifesto.md`](docs/manifesto.md); the K&R-style user guide is [`docs/user_guide.md`](docs/user_guide.md). The reference implementation under `reference/` is the language specification this workspace builds against.

## Status

End-to-end pipeline. Source → lex → parse → sema → IR → LLVM emit → MCJIT → run, with a precise mark-sweep GC, a module loader, a Direct2D / DirectWrite GUI, and an integrated bedit editor that JITs the current buffer (`newbcpl-driver gui`).

### What works

- **`newbcpl-lexer`** — full BCPL surface (classic and dotted-float operators, section brackets, `*`-escapes, `#`/`#X` numbers, `%%` bitfield, `?` null literal).
- **`newbcpl-parser`** — recursive-descent over the dialect: `LIST` / `MANIFESTLIST`, `CLASS` / `EXTENDS` / `VIRTUAL` / `FINAL` / `MANAGED` / `SUPER`, `FOREACH` with lane destructuring, `AS` annotations, `RETAIN`, multi-target assignment, `VEC(…)` / `FVEC(…)` paren-init, `FUNCTION` / `ROUTINE` keyword forms, mutual `AND`.
- **`newbcpl-sema`** — register-class type inference per manifesto §1. Walks the tree once, attaches a hint to every expression, never errors on type grounds. Class layouts (fields, vtable slots, pointer-offset arrays) computed up front. `AS` annotations propagate through opaque reads.
- **`newbcpl-ir` / `newbcpl-llvm`** — typed lowering then codegen via Inkwell (LLVM 22). MCJIT today. `NEW Class`, vtables, `FieldLoad` / `FieldStore`, `VALOF`, every loop form, `SWITCHON`, `GEP` / indirection / lane access, PAIR-as-`<2 x i64>`, SIMD packs, real cons-cell lists, cooperative GC safepoints at function entry and every loop back-edge.
- **`newbcpl-runtime`** — precise mark-sweep tracing GC (port of NewCP's `gc.rs`): per-thread TLABs, stop-the-world via safepoints, sentinel-terminated pointer-offset arrays per type, finalizer support. Allocation-rate auto-trigger, explicit `GC()` and `HEAP_INFO()` builtins. Runtime-side `TypeDesc` interning lets `collect()` survive JIT runs. Includes the standard library (`WRITES`/`WRITEF`/`WRITEN`/`WRITEC`, `FLOAT`/`FIX`/`TRUNC`/`ENTIER`, `FSIN`/`FCOS`/…/`FSQRT`, list ops, `RAND`/`RND`/`FRND`) and the Direct2D + DirectWrite GUI (`iGui_*` builtins for windows, draw batches, text panes, event mailbox).
- **`newbcpl-loader`** — module discovery in `./modules-active/` (or `$NEWBCPL_MODULES_ACTIVE`). `<stem>_<routine>` name mangling; backward and forward cross-module references resolve at link time. Ships `igui` / `maths` / `geom` modules.
- **`newbcpl-driver`** — all phase dumps live: `dump-tokens`, `dump-ast`, `dump-sema`, `dump-ir`, `dump-llvm`, `dump-asm`. `run` JITs and executes. `gui` opens the bedit + log frame and `Ctrl+R` runs the active buffer. `test-folder <dir>` JITs every `.bcl` under a directory and emits a report.

### Tests

`cargo test --workspace` is green across 33 binaries — lexer / parser / sema unit tests, plus the eight-tier synthetic probe matrix (`tests/newbcpl-tests/tests/matrix_tier{1..7}.rs`, `matrix_extra.rs`, `matrix_generated.rs`). `tests/user_guide.rs` re-parses every fenced ```bcpl block in `docs/user_guide.md` and every `examples/*.bcl` on each run so the docs cannot rot silently.

The reference 857-file corpus (`reference/tests/bcl_tests/`) is still a moving target — pass count there is a coverage signal, not a regression gate.

### Known gaps

- ORC v2 alongside MCJIT, in step with NewCP's migration.
- `MANAGED` linear-type enforcement (no aliasing, no list storage) — discovery in sema works; the verifier pass is still to come.
- Class-aware member typing — `obj.field` is currently `Word` in sema; codegen has the layouts it needs but sema does not yet propagate field types back into hints.

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

The original NBCPL is developed at [github.com/albanread/NBCPL](https://github.com/albanread/NBCPL), targeting Apple Silicon ARM64. NewBCPL is its Windows-first, LLVM-backed successor by the same author. Sister project: [NewCP](../NewCP/NewCP/README.md), recreating Component Pascal on the same toolchain.

The author wrote production BCPL on TRIPOS / 68000 minicomputers; the surface aesthetic comes from that era. NewBCPL is not affiliated with any historical BCPL implementation.
