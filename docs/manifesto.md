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
- **`USING` for deterministic cleanup.** `USING name = expr DO body` binds `name` for `body` and runs `name.RELEASE()` on every way out — fall-through, `RETURN`, `RESULTIS`, `FINISH`, `BREAK`, `LOOP`, `ENDCASE`. Innermost-first when nested. This is the scope-deterministic cleanup story for OS resources (file handles, windows, Direct2D objects). The `MANAGED` keyword still parses on a class declaration but is **advisory now** — once a GC exists, "don't leak" and "release in order" are separate concerns and only the latter needs language support. Any class with a `RELEASE` method works in a `USING`.
- **Lists keep the reference design.** Doubly-anchored singly-linked, freelisted, with dual-path codegen for literal vs dynamic. Per-variant TypeDescs replace the inline atom type tag.
- **Vectors stay manual.** `VEC` lives in stack frames; classic programs need no heap and the GC stays out of their way.

Reference counting was rejected: atomic refcount updates on every assignment hammer SIMD-heavy numerical code, and cycles need a tracing collector anyway. SAMM (the reference's scope-bound automatic free) was rejected: scope is the wrong granularity for ownership once aliasing exists, and the reference's bloom-filter double-free detector confirms it in practice.

## 6. iGui is the runtime

NewBCPL does not ship a compiler that hosts an optional GUI library. It ships a GUI that hosts a compiler. The same process edits source, JITs it, runs it, and renders its windows.

- **UI thread is the main thread.** `igui::run` is the process entrypoint. It registers WndClasses, creates the MDI frame, and only then spawns a *language thread* via the worker closure it takes. Process lifetime equals frame lifetime; a hard fault in the language thread closes user windows but the frame, bedit, and the log view stay up so the user can edit, recheck, and reload.
- **Two threads, narrow channels.** UI thread owns every HWND, runs the message pump, paints. Language thread runs sema, the JIT, and JIT-emitted code. They communicate only through `igui::channels` (typed events GUI → language) and through the per-child `batch` / `text_view` command queues (commands language → GUI). No shared HWNDs, no Win32 calls from the language thread.
- **Sema is the checker.** `newbcpl-sema` runs on the UI thread when bedit asks for diagnostics — typing is cheap, sema is cheap, and the language thread may be busy in user code. The driver installs the closure with `igui::install_checker` at startup; bedit calls it on F7, on save, and on focus loss.
- **Whole programs only.** BCPL has no snippet REPL. To run anything the user picks a program file in bedit; the runtime resolves its `GET` includes, sema-checks the bundle, JITs it as one unit, and invokes its entrypoint on the language thread. Replacing the loaded program tears down its JIT memory and any windows it opened — the frame and the shared modules stay.
- **The loader symbol table is the shared-module mechanism.** Classic BCPL's `GLOBALS` was a flat indirection table; the original design ported it as the shared-module ABI. Once the loader landed, the symbol table replaced the slot-vector entirely — `GLOBALS $( name : slot $)` is **rejected** by the parser with a hint pointing at `GLOBAL` (the modern named-binding form). A separately-JITted compilation unit registers its entrypoints with the loader by name; the BCPL runtime support library (`WRITES`, `WRITEF`, file I/O, heap inspection) is Rust-resident modules registered at bootstrap under literal names. JIT-compiled BCPL modules auto-prefix their exports with the module name (`calc_add`, `colour_console_print`), turning the mangling convention into the namespace mechanism without any new syntax. The compiler resolves unbound names by querying the loader's live symbol table — `GET "header.h"` keeps its classical textual-include meaning and is also the bridge: a `GET "foo"` falls back to the modules-active folder when the sibling-file path doesn't resolve, so a module file can double as its own header. Programs are not modules: they cannot install or unload modules, and they run one at a time. There is no automatic dependency resolution: the user curates the loaded set by dropping files into the active-modules folder or by writing a startup script. The CLI window is the gatekeeper for both module lifecycle (`/load`, `/unload`, `/reinit`, `/ensure`, `/install`, `/remove`) and program execution (`/run`, `/execute`); see [module_system.md](module_system.md) for the export-directory shape, the loader veneer over NewCP's `LoaderSession`, the active-modules folder, and the TRIPOS-flavoured CLI.

This decision settles the window-ownership taxonomy that has been implicit since the iGui port:

- **iGui-owned windows.** The MDI frame, `bedit`, the log view. Created by `igui::run`; the language thread cannot close them; they outlive any user program.
- **Program-owned windows.** Opened by the currently-loaded user program via `iGui.OpenChild`. Closed automatically when the program is unloaded or replaced.
- **Module-owned windows.** Opened by a module registered with the loader. Closed when that module is unloaded or replaced. A logger module that opens a docked panel keeps its panel as long as it remains registered.

Reload semantics follow from the taxonomy: choosing a new program in bedit closes program-owned windows, releases the previous program's JIT memory, then loads + JITs + invokes the new program. The user sees the frame, bedit, the log view, and any module-owned panels stay put; the application area changes. This is why iGui exists at all — to provide a stable shell the user's program plugs into, not a window the user's program owns.

## What this is not

- Not literal compatibility with the reference NBCPL compiler. The reference is the spec; its bugs are not.
- Not memory-safe. Pointers are pointers. GC and `USING` make heap saner; you can still segfault.
- Not a teaching language. The 1974 manual exists.
- Not a research project. Conservative principles. Ship something good.
- Not a portability target for ARM64-specific code. Direct2D and DirectWrite host the GUI on Windows; macOS / Linux are not GUI targets.

## Concrete tech stack

- Rust 2024 edition, Cargo workspace mirroring NewCP's shape.
- LLVM 22 via Inkwell 0.9.
- MCJIT today, ORC v2 later (alongside NewCP's migration).
- Direct2D + DirectWrite for `iGui`, `x86_64-pc-windows-msvc` only — see section 6 for the iGui-as-runtime contract.
- Mark-sweep GC adapted from NewCP's `gc.rs`.
- Phase-visible compiler driver: `dump-tokens`, `dump-ast`, `dump-sema`, `dump-cfg`, `dump-ir`, `dump-llvm`, `dump-asm`, `dump-heap`.

## Open questions

1. **Concurrency primitives.** The reference reserves `SEND` / `ACCEPT` / `REMANAGE` keywords. Their semantics are unclear and they are out of scope until single-threaded NewBCPL is solid.
2. **Dynamic module registration from BCPL source.** The manifest-driven path in [module_system.md](module_system.md) covers the common case (modules declare exports, the loader registers them at startup). The dynamic case is still open: BCPL source that wants to install a symbol from within a running routine needs a primitive — a built-in `INSTALL "name" = expr`, an ordinary function call, or both? Decide alongside the JIT-side ABI.
3. **JIT engine retirement.** Both `TypeDesc` lifetime and the JIT-vtable registry (`__newbcpl_lookup_method`'s back-end) rely on the JIT engine being leaked for the process's lifetime. Long-running embeddings that want to drop and rebuild the engine need either a registry-clear hook or runtime-side persistent copies. See [jit_typedesc_lifetime.md](jit_typedesc_lifetime.md) for the shape and the parallel concerns.

## Lineage

NewBCPL recreates the modern BCPL dialect prototyped at [github.com/albanread/NBCPL](https://github.com/albanread/NBCPL), built on Rust + LLVM, targeted at `x86_64-pc-windows-msvc`, with an integrated Direct2D / DirectWrite GUI in the spirit of [NewCP's iGui](../../NewCP/NewCP/README.md). The reference checked out under `reference/` is the language specification; this document is the design contract.

The author wrote production BCPL on TRIPOS / 68000 minicomputers; the surface aesthetic comes from that lineage. NewBCPL is not affiliated with any historical BCPL implementation.
