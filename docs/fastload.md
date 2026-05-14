# Fastload: cached module bitcode

> Cousin doc to [module_system.md](module_system.md). The module system
> defines *which* `.bcl` files load and *how* they're addressable.
> Fastload defines *how quickly* they can be brought up from disk on
> the second-and-later runs.

## Why

Every `newbcpl-driver gui` / `run` invocation re-traverses
[`modules-active/`](../modules-active/), and for each `.bcl` file
re-runs the full front-end: lex → parse → sema → IR-lower → LLVM
emit → auto-prefix rename. The output is a `Module<'ctx>` that's
identical bit-for-bit across runs as long as the source hasn't
changed.

Fastload caches that `Module<'ctx>` as serialised LLVM bitcode in a
sibling `.bclb` file. A clean second run skips the entire front end
and goes straight from the on-disk bitcode to `link_in_module`. MCJIT
finalize is the only remaining significant cost, and it's small
enough that it doesn't repay further caching effort yet.

This isn't an optimisation for tight inner loops; it's an
optimisation for *boot latency*. A library that grows from a handful
of modules to a few dozen would otherwise add proportional source-
parse time to every Run. Fastload makes that grow-the-library cost
amortise across exactly one user-visible compile.

## Current pipeline (per module)

For each `.bcl` in the active-modules folder, every Run does:

  1. **Read source from disk.**
  2. **Lex** (`newbcpl-lexer::lex_source`).
  3. **Parse → AST** (`newbcpl-parser::parse_source`).
  4. **Sema** (`newbcpl-sema::analyze`).
  5. **IR lower** (`newbcpl-ir::lower`).
  6. **LLVM emit** (`newbcpl-llvm::emit`).
  7. **Auto-prefix rename** (`LLVMSetValueName2` per
     `ir.functions[i].name`).
  8. **`link_in_module`** into the program's LLVM module.
  9. **MCJIT finalize** — happens implicitly at the first
     `get_function_address` call.

Fastload elides steps 1–7. Step 8 stays (cheap), step 9 stays
(actual code generation; the real work).

## Cache point: post-emit, post-rename bitcode

Earlier cache points were considered and rejected:

- **AST or sema-decorated AST**: skips less work, costs a custom
  serialisation format that has to evolve with our types.
- **`newbcpl-ir::Module`**: skips a bit more, still needs a custom
  format, and the LLVM emit pass is fast enough that the gain is
  marginal.
- **Native object code**: skips MCJIT too, but needs a real linker
  + position-independent code; the savings are smaller than the
  complexity. Deferred indefinitely.

LLVM bitcode wins because LLVM owns the serialisation, the format is
stable across our minor LLVM bumps, and post-rename bitcode is a
drop-in `link_in_module` input. We freeze the auto-prefix mangling
*into* the cache, so loading it produces the same final symbol
names the source path would have.

## File format: `.bclb`

One file per cached module, sibling to its source:

    modules-active/maths.bcl     ← edited by user, source of truth
    modules-active/maths.bclb    ← build artifact, gitignored

Format inside the `.bclb`:

```
offset  size   field
─────── ────── ───────────────────────────────────────────────────────
0       4      magic            "BCLB" (0x42 0x43 0x4C 0x42)
4       4      format_version   u32 LE — start at 1
8       4      compiler_version u32 LE — manual bump constant
12      4      llvm_version     u32 LE — inkwell's LLVM major
16      32     source_hash      BLAKE3 of the .bcl bytes
48      4      module_name_len  u32 LE
52      N      module_name      utf-8 bytes, no NUL
52+N    4      bitcode_len      u32 LE
56+N    M      bitcode          LLVM bitcode payload
```

Total fixed header overhead is ~96 bytes; the bitcode dominates
file size. All multi-byte integers are little-endian. The header is
self-describing enough to detect a stale cache without parsing the
bitcode payload.

Why each field exists:

- **magic** — distinguish from arbitrary bytes; refuse to load files
  that don't claim to be `.bclb`.
- **format_version** — bump when the header layout itself changes.
  Distinct from the compiler version because we may evolve the
  layout without changing what the bitcode means.
- **compiler_version** — a `const BCLB_COMPILER_VERSION: u32` in
  `newbcpl-llvm`, bumped by hand whenever the IR shape or auto-
  prefix mangling changes. Forces a cache rebuild without needing
  a hash of the source tree.
- **llvm_version** — inkwell exposes the LLVM major version at
  compile time; rebuilds the whole cache when we bump LLVM.
- **source_hash** — BLAKE3 of the `.bcl` source bytes. If the
  source has changed, the cache is stale regardless of any other
  field.
- **module_name** — for sanity-check logging; the loader confirms
  the cached name matches the module the loader is asking for.

## Loader integration

In [`newbcpl-llvm::run_program_ir`](../src/newbcpl-llvm/src/lib.rs)
the per-module compile-and-link loop becomes:

```rust
for mpath in &paths {
    let stem = mpath.file_stem()…;
    let module = match try_load_cached_module(&context, mpath, stem)? {
        Some(m) => {
            eprintln!("[loader] module {stem}: loaded from cache");
            m
        }
        None => {
            let m = compile_from_source(&context, mpath, stem)?;
            let _ = write_cache_module(mpath, stem, &m);  // best-effort
            eprintln!("[loader] module {stem}: compiled + cached");
            m
        }
    };
    program_module.link_in_module(module)?;
}
```

`try_load_cached_module` checks:

  1. Sibling `.bclb` exists alongside the `.bcl`.
  2. Magic + format_version + compiler_version + llvm_version match.
  3. `source_hash` matches the current `.bcl`'s BLAKE3.
  4. `module_name` matches the stem we're asking for.

Any mismatch ⇒ return `None` (cache miss); the caller falls back
to source compile and overwrites the cache.

`write_cache_module` is best-effort: failures (read-only filesystem,
out of disk) log a warning but don't fail the run.

**One-time at boot, source otherwise.** Fastload only applies to
the boot-time active-folder scan. Anything loaded *after* boot
(future dynamic loads from a CLI, hot reload, etc.) always goes
through source — no cache lookup, no cache write. This keeps the
cache machinery scoped to the one pass it most helps.

## Driver surface

Transparent by default. Three additions:

- **`newbcpl-driver fastload [<dir>]`** — pre-warm the cache without
  running anything. Compiles every `.bcl` in `<dir>` (or
  `modules-active/`) and writes its sibling `.bclb`. Useful for
  CI / "build a ship-able library" workflows.
- **`newbcpl-driver cache-clean [<dir>]`** — delete every `.bclb`
  under `<dir>`. Useful when something is wrong and the user wants
  to start clean.
- **`--no-cache` flag on `gui` and `run`** — skip the cache lookup
  + write; always compile from source. Useful for testing the
  source path or when debugging the cache machinery itself.

The `[loader]` log line gains a per-module annotation so the user
can see what happened:

    [loader] module maths: 8 functions linked (from cache)
    [loader] module geom: 3 functions linked (compiled + cached)
    [loader] module igui: 26 functions linked (compiled, cache miss: source_hash)

## What is *not* cached

- **Programs.** A program is a `.bcl` with `START`. Programs run via
  `gui` come from bedit's live buffer (not the on-disk file); cache
  wouldn't apply. Programs run via `run` come from disk, but they're
  one-shot — caching the whole-program LLVM module saves only the
  small program-side compile, while complicating the rebuild story
  for the file the user is most actively editing. Skip.
- **Runtime loads.** Any future `/load` from a CLI, hot-reload, or
  GLOBAL-module install path goes through source. Cache machinery is
  boot-only.
- **Cross-module dependency edges.** Modules reference each other
  (e.g. `geom` calls `maths_sq`), but the cache invalidation rule is
  per-module-source-hash only. If editing module B somehow
  invalidates the bitcode for module A, the user sees it the same
  way they would have today: as a link-time or run-time error when
  module A calls into B. We don't attempt transitive cache
  invalidation. This is consistent with the loader's "one batch,
  one shot" model.

## Failure handling

- **Cache file missing.** Cache miss, fall back to source, write
  fresh cache.
- **Cache file corrupt or header invalid.** Cache miss with reason
  logged; overwrite with a fresh cache after the source compile.
- **Cache write fails** (read-only filesystem, disk full). Log
  warning; the run continues using the in-memory module. The next
  boot will retry the cache write.
- **Bitcode parses but link_in_module rejects it** (very unlikely
  short of bit-rot). Fall back to source-compile, overwrite cache.

Silent fallback is correct because the cache is a build artifact,
not a contract. The behaviour the user observes is identical to
the source path; only the timing differs.

## Open / deferred design questions

1. **Compiler-version bumping.** A manual `const
   BCLB_COMPILER_VERSION: u32 = N;` constant in `newbcpl-llvm`, bumped
   by hand on every change that affects IR shape or mangling. A
   future automatic scheme (hash the IR-emitter source, etc.) would
   reduce the chance of stale caches surviving a refactor, but
   would also force a cache rebuild on noise commits. Manual stays.
2. **Cache for builtin modules.** When (or if) we ship resident
   modules through the same loader path the BCPL modules use,
   caching their LLVM IR would have to handle the
   `NativeModuleArtifact` flavour too. For v0, native modules
   register via `add_global_mapping` and don't touch this codepath.
3. **Concurrent cache writes.** Two `newbcpl-driver` processes
   running against the same `modules-active/` would race on cache
   writes. POSIX rename-and-replace gives atomicity; on Windows the
   file lock is annoying. Defer until it's a real problem; the
   typical workflow is one driver at a time.
4. **Per-module mtime cross-check.** Hashing the source bytes is
   the correct invalidation criterion, but it costs a BLAKE3 pass on
   every Run even when the cache is fresh. An optional mtime check
   (cached mtime in the header, skip the hash when mtime matches)
   would short-circuit the common case. Worth adding once the
   modules-active set grows past a few dozen.

## Implementation sketch

Roughly where new code goes, when we get to it:

- **`newbcpl-llvm::cache`** (new module, ~200 lines): `BclbHeader`
  struct + `read_header`, `write_header`, `try_load_cached_module`,
  `write_cache_module`. Uses `blake3` crate for the source hash.
- **`newbcpl-llvm::run_program_ir`** (existing): swap the
  per-module compile + rename block for `try_load_cached_module`-
  then-fallback as sketched above.
- **`newbcpl-driver`**: `fastload` and `cache-clean` subcommands;
  `--no-cache` flag plumbed into `run_with_active_folder`.
- **Cargo**: `blake3 = "1"` added to `newbcpl-llvm`'s `Cargo.toml`.

Roughly 200 + 50 + 50 lines of new code, plus a couple of integration
tests (compile a module twice; second time the `.bclb` exists and
the function output matches the first; touching the source
invalidates the cache).
