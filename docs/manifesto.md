# NewBCPL — vision and principles

NewBCPL is a recreation of BCPL as it might have evolved if the language had not been quietly abandoned in the 1980s. The aesthetic stays — terse keywords, section brackets, operators over methods, no ceremony — and the semantics catch up to the machines we actually have.

The five principles below are load-bearing. Every design decision should be checkable against them. Features that don't pass don't belong in NewBCPL.

## 1. Looks untyped, secretly typed

Classic BCPL was genuinely typeless: a word was a word, integers and addresses were interchangeable, everything ran in one register file. NewBCPL keeps the *appearance* — no annotations in source, no type variables, no `INT x` ceremony — but every expression has a determinate type at compile time. The compiler always knows; the user never has to.

- Type is fixed at first assignment. `LET x = 3.14` makes x FLOAT for its scope.
- Subsequent assignments coerce when possible (FLOAT← INT silent, INT← FLOAT warns).
- WORD is the universal escape hatch. `LET x = ? AS WORD` opts out of inference.
- Annotations are documentation, never required. Sema verifies them when present.
- Sema never errors on type grounds. There is always a fallback to WORD / integer codegen.

Inference is local and predictable. No Hindley-Milner, no unification, no polymorphism. Sema walks the tree once, attaches a type to every expression, and codegen reads it.

## 2. Be close to the machine

Types exist *because* modern machines have different register files. They are register-class hints, not academic categories.

| Language | ARM64 register | LLVM type |
|---|---|---|
| INT, WORD | X-reg, 64 bit | `i64` |
| FLOAT | D-reg, 64 bit FP | `double` |
| PAIR | V-reg, 128 bit, 2 × i64 | `<2 x i64>` |
| FPAIR | V-reg, 128 bit, 2 × f64 | `<2 x double>` |
| QUAD | 256 bit, 2 V-regs / SVE | `<4 x i64>` |
| FQUAD | 256 bit | `<4 x double>` |
| OCT | 512 bit (SVE) | `<8 x i64>` |
| FOCT | 512 bit | `<8 x double>` |

The same lattice maps to SSE / AVX on x86_64. LLVM does the lowering; we emit the right vector type.

Operators on these types lower to single SIMD instructions. `(a, b) + (c, d)` on FPAIRs is one `fadd <2 x double>`. Lane access via `|` is `extractelement`.

Coercions are real machine instructions: INT→FLOAT is SCVTF, FLOAT→INT is FCVTZS, INT↔WORD and INT↔POINTER are free. When sema warns about an implicit conversion, it is warning about emitting an instruction the user did not write.

**Corollary:** NewBCPL does not add types the machine does not have. No arbitrary-precision integers as a primitive. No algebraic data types. No `Option<T>`. If it does not correspond to a register class or a small fixed-shape memory layout, it is a library, not a primitive.

## 3. Classic BCPL just works

A program written in the 1974 Richards-manual subset of BCPL must compile under NewBCPL and produce executable behaviour identical to a classic compiler's. This is enforced by the WORD-fallback property:

- Vectors stay typeless. `LET v = VEC 100; v!0 := some_int; v!0 := some_pointer` is legal.
- Word + Word is integer add. Bag-of-bits semantics; the compiler does not second-guess.
- Classic operators (`!`, `@`, `%`, `%%`, `:=`, `->`) keep their classic meaning.
- The full Richards-1974 grammar parses unchanged.
- Section brackets `$( $)` and `{ }` are interchangeable.

Modern features layer on top. They do not displace the base language.

## 4. The MOST BCPL way

When adding a feature, prefer the form that matches the language's existing aesthetic:

- **Operators over methods.** `HD x`, `TL x`, `x!i`, `obj.m()`, `LIST(a,b,c)`, `NEW Point(1,2)`. Not `x.head()` or `list.append(y)`.
- **Terse keywords.** `LET`, `BE`, `VALOF`, `RESULTIS` over `function` / `return` / etc.
- **Section brackets** — `$( $)` canonical, `{ }` accepted.
- **No mandatory annotations.** They exist for documentation only.
- **Mutual recursion via `AND`.** `LET f() = ... AND g() = ...`.
- **No exceptions.** Errors return values or `RESULTIS 0`.
- **`VALOF`** is the expression form. Not lambda.

If a feature reads like Java, Python, or Rust written in BCPL syntax, redesign it.

## 5. Looks unmanaged, secretly collected

Classic BCPL had no automatic heap management — `getvec` / `freevec` by hand. NewBCPL adds heap-using constructs (LIST, NEW Foo, dynamic strings) and manages them automatically. The user never writes `RETAIN` / `RELEASE` for ordinary data.

- **Tracing mark-sweep GC, ported from NewCP** (see [`NewCP/docs/garbage_collection.md`](../../NewCP/NewCP/docs/garbage_collection.md)). Precise marking via per-type pointer-offset arrays, stop-the-world, single-threaded for now.
- **Default classes are GC'd.** `RELEASE` runs as a best-effort finalizer.
- **`MANAGED` linear types for resources.** A `CLASS Foo MANAGED $( ... $)` is stack-bound, cannot be aliased, cannot be stored in a list. `RELEASE` runs deterministically on scope exit. Used for OS resources — file handles, windows, Direct2D objects.
- **Lists keep the reference design.** Doubly-anchored singly-linked, freelisted, with dual-path codegen for literal vs dynamic. Per-variant TypeDescs replace the inline atom type tag.
- **Vectors stay manual.** `VEC` lives in stack frames; classic programs need no heap and the GC stays out of their way.

Reference counting was rejected: atomic refcount updates on every assignment hammer SIMD-heavy numerical code, and cycles need a tracing collector anyway. SAMM (the reference's scope-bound automatic free) was rejected: scope is the wrong granularity for ownership once aliasing exists, and the reference's bloom-filter double-free detector confirms it in practice.

## What this is not

- Not literal compatibility with the reference NBCPL compiler. The reference is the spec; its bugs are not.
- Not memory-safe. Pointers are pointers. GC and `MANAGED` make heap saner; you can still segfault.
- Not a teaching language. The 1974 manual exists.
- Not a research project. Conservative principles. Ship something good.
- Not a portability target for ARM64-specific code. Direct2D and DirectWrite host the GUI on Windows; macOS / Linux are not GUI targets.

## Concrete tech stack

- Rust 2024 edition, Cargo workspace mirroring NewCP's shape.
- LLVM 22 via Inkwell 0.9.
- MCJIT today, ORC v2 later (alongside NewCP's migration).
- Direct2D + DirectWrite for `iGui`, `x86_64-pc-windows-msvc` only.
- Mark-sweep GC adapted from NewCP's `gc.rs`.
- Phase-visible compiler driver: `dump-tokens`, `dump-ast`, `dump-sema`, `dump-cfg`, `dump-ir`, `dump-llvm`, `dump-asm`, `dump-heap`.

## Open questions

1. **Concurrency primitives.** The reference reserves `SEND` / `ACCEPT` / `REMANAGE` keywords. Their semantics are unclear and they are out of scope until single-threaded NewBCPL is solid.

## Lineage

NewBCPL recreates the modern BCPL dialect prototyped at [github.com/albanread/NBCPL](https://github.com/albanread/NBCPL), built on Rust + LLVM, targeted at `x86_64-pc-windows-msvc`, with an integrated Direct2D / DirectWrite GUI in the spirit of [NewCP's iGui](../../NewCP/NewCP/README.md). The reference checked out under `reference/` is the language specification; this document is the design contract.

The author wrote production BCPL on TRIPOS / 68000 minicomputers; the surface aesthetic comes from that lineage. NewBCPL is not affiliated with any historical BCPL implementation.
