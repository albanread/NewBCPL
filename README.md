# NewBCPL

NewBCPL is a recreation of the modern BCPL dialect prototyped at [github.com/albanread/NBCPL](https://github.com/albanread/NBCPL). Built on Rust + LLVM, targeted at `x86_64-pc-windows-msvc`, with an integrated Direct2D + DirectWrite GUI in the spirit of [NewCP's iGui](../NewCP/NewCP/README.md).

The design contract is in [`docs/manifesto.md`](docs/manifesto.md). The reference implementation under `reference/` is the language specification; this workspace is the new build.

## Status (2026-05-10)

Bootstrap phase. The lexer is solid, a precise mark-sweep GC is in place, the rest of the pipeline is stubs.

### What works

- **`newbcpl-lexer`** — tokenises the full BCPL surface (classic operators, dotted-float operators, section brackets `$( $)` / `{ }`, `*`-prefix string escapes, `#` octal / `#X` hex, `%%` bitfield, `?` null literal). 23 unit tests + 5 integration tests against real `.bcl` programs from the reference. 831 / 857 of the reference corpus lex cleanly; the remaining 26 are buggy source files (`*#` typos, unterminated `"*"` strings, stray UTF-8 bytes).
- **`newbcpl-runtime`** — precise mark-sweep tracing GC ported from NewCP's `gc.rs`. Per-thread TLABs, stop-the-world via cooperative safepoints, sentinel-terminated pointer-offset arrays per type, finalizer support. 4 self-tests pass. Same on-wire layout as NewCP so improvements port via mechanical diff.
- **`newbcpl-driver`** — minimal driver. `dump-tokens` works end-to-end.

### What doesn't (yet)

Parser, sema, IR, LLVM emit, runtime builtins (WRITES / WRITEF / FREEVEC / FLOAT / TRUNC etc.), list and object support, `iGui`. Each is a stub crate.

## Workspace layout

Mirrors [NewCP](../NewCP/NewCP/README.md):

```
src/
  newbcpl-lexer/     working
  newbcpl-parser/    stub
  newbcpl-sema/      stub
  newbcpl-ir/        stub
  newbcpl-llvm/      stub (Inkwell wired but disabled until codegen lands)
  newbcpl-runtime/   GC working; standard library + iGui forthcoming
  newbcpl-loader/    stub
  newbcpl-driver/    dump-tokens working
tests/
  newbcpl-tests/     lexer smoke + corpus sweep
docs/
  manifesto.md       design contract — five principles
reference/           the original NBCPL, treated as the language spec
```

## Driver surface

Each phase exposes a stable textual dump. As phases come online, the corresponding subcommand goes live.

- `newbcpl-driver dump-tokens <path>` — works
- `newbcpl-driver dump-ast | dump-sema | dump-cfg | dump-ir | dump-llvm | dump-asm` — not yet

## What's next

1. **Parser** — recursive descent over the BCPL grammar plus the dialect extensions: `LIST` / `MANIFESTLIST`, `CLASS` / `NEW` / `EXTENDS` / `VIRTUAL` / `SUPER`, dotted float operators, `?`-as-null, `FOREACH`, `MANAGED` linear classes.
2. **Sema** — register-class type inference per manifesto §1. Walks the tree once, attaches a hint to every expression, never errors on type grounds.
3. **IR + LLVM emit** — typed lowering, then codegen via Inkwell (LLVM 22). MCJIT today, ORC v2 alongside NewCP's migration.
4. **Runtime builtins** — `WRITES`, `WRITEF`, `WRITEN`, `WRITEC`, `FREEVEC`, `FLOAT`, `TRUNC`, list ops, string ops.
5. **iGui slice** — Direct2D + DirectWrite host, reusing NewCP's `iGui` crate shape where it makes sense.

## Lineage

The original NBCPL is developed at [github.com/albanread/NBCPL](https://github.com/albanread/NBCPL), targeting Apple Silicon ARM64. NewBCPL is its Windows-first, LLVM-backed successor by the same author. Sister project: [NewCP](../NewCP/NewCP/README.md), recreating Component Pascal on the same toolchain.

The author wrote production BCPL on TRIPOS / 68000 minicomputers; the surface aesthetic comes from that era. NewBCPL is not affiliated with any historical BCPL implementation.
