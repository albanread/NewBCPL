# NewBCPL module system

> Sister doc to [manifesto §6 — iGui is the runtime](manifesto.md). §6 fixes
> the process model (UI thread is main, one address space, GLOBALS is the
> shared-module ABI). This doc fills in the module mechanics, the CLI, and
> the script model.
>
> Sections below split into **Current implementation status** (what
> works in the tree today) and **For future consideration** (the full
> design discussed during the manifesto / module-system threads, kept
> intact for when we come back to it). Current first, future second.

## Current implementation status

The minimum-viable loader works end-to-end in the driver: write a
module as a `.bcl` file, drop it in the active-modules folder, and a
program `/run`'d through `newbcpl-driver run` can call into it.

### What works

- **A module is a `.bcl` file with no `START` routine.** A program is
  a `.bcl` file with one. The driver doesn't ask for either marker
  beyond that; the parser doesn't need new keywords.
- **The active-modules folder.** `./modules-active/` by default,
  `$NEWBCPL_MODULES_ACTIVE` to override. Every `*.bcl` file inside is
  loaded automatically — alphabetical order — before the program runs.
  A missing or empty folder is fine; no modules are loaded.
- **Mangling.** Every top-level `LET` / `AND` in a module file gets
  renamed at LLVM-emit time to `<stem>_<name>`. So `maths.bcl`'s
  `LET sq(x) = x * x` becomes the symbol `maths_sq`. The program
  calls it under that mangled name; classical BCPL standard-library
  built-ins (`WRITES`, `WRITEN`, `WRITEC`, `NEWLINE`, `WRITEF`, …)
  stay bare because they're host-process Rust functions registered
  through `add_global_mapping`, not BCPL modules.
- **Cross-module calls.** Module A calling module B works regardless
  of alphabetical load order or recursion direction. The loader
  links every module into the program's LLVM module before creating
  the JIT engine, so all inter-module references are resolved by
  LLVM's linker — no address threading, no order constraint.
- **One JIT engine per `run`.** Modules + program live in one Inkwell
  `Module`, one `ExecutionEngine`. The engine drops when `run`
  returns. No hot reload yet.

### File layout reference

```
NewBCPL/
├── modules-active/          ← scanned by driver at run time
│   ├── geom.bcl             ← module: no START, top-levels mangled to geom_*
│   └── maths.bcl            ← module: no START, top-levels mangled to maths_*
└── examples/
    └── use-both.bcl         ← program: has START, calls geom_* and maths_*
```

Running it:

```sh
cargo run --target x86_64-pc-windows-msvc -p newbcpl-driver -- run examples/use-both.bcl
```

Output (abridged):

```
[loader] module geom: 3 functions linked
[loader] module maths: 8 functions linked
Direct program-to-module calls:
  maths_cube(3)            = 27
  geom_area_of_rect(4, 5)  = 20
Module-to-module call (geom calls maths):
  geom_area_of_square(7)   = 49
Backwards module call (maths calls geom):
  maths_hypot_squared(3, 4) = 25
```

### How to write a module

```bcpl
// modules-active/maths.bcl

LET sq(x) = x * x
LET cube(x) = x * x * x
LET abs2(x) = x < 0 -> 0 - x, x
LET sum_to(n) = VALOF $(
    LET total = 0
    FOR i = 1 TO n DO total := total + i
    RESULTIS total
$)
```

A program calls them as `maths_sq(5)`, `maths_cube(2)`, etc. No
`GET`, no `GLOBAL { ... }`, no `EXPORT` — every top-level routine in
a module file is exported automatically (private helpers don't exist
in the MVP convention).

### Known limitations of the MVP

- **No `EXPORT` qualifier.** Every top-level routine in a module is
  exported. Private helpers come back when the `EXPORT` keyword
  lands (see "For future consideration" below).
- **No `DECLARE MODULE`** — module name is always the filename stem.
- **No hot reload.** Modules live for the lifetime of the `run`
  invocation. To pick up a source change, re-run the driver.
- **No `module_init` / `module_finalise`.** Modules are just bags of
  pure functions. State management lands with the private-word
  scheme in the full design.
- **No version metadata, no `/ensure`.**
- **No CLI.** The driver's `run <path>` is the only entrypoint. The
  TRIPOS-flavoured CLI window described in "For future consideration"
  is intentionally deferred — we don't need it to build out a library.
- **No resident Rust modules through the loader.** `WRITES` / `WRITEN`
  / etc. are still registered directly in the LLVM crate via
  `add_global_mapping`, not through a `NativeModuleArtifact` factory.
  The wrapper falls into "future" territory; the existing direct
  registration keeps classical-BCPL programs working today.
- **Module-resident classes are untested.** The vtable-patch loop
  now aggregates layouts across linked modules, but no current
  module declares a class. First class-shipping module will need
  the integration verified.

### Why linking instead of per-module engines

The full design (see below) describes a per-module `ExecutionEngine`
with `add_global_mapping` threading addresses between engines. We
tried it; cross-module calls fail at finalize because each engine
only sees its own functions + built-ins. The link-based approach —
collect every module's LLVM IR, rename top-level functions, link
into the program's `Module`, then create one engine — drops every
order constraint and resolves mutual recursion natively. The
tradeoff is no per-module isolation: we can't reclaim a single
module's JIT pages independently. For the "load-at-startup, never
unload" model that's exactly the right tradeoff. Per-module
isolation comes back when we want hot reload, and at that point ORC
JIT's lazy resolution becomes the right answer rather than retrofitting
MCJIT.

### Driver surface today

- `newbcpl-driver run <path>` — scans the active-modules folder,
  links every `*.bcl` module into the program, JITs the lot,
  invokes `START`. Returns `START`'s value as the exit code line.
- All other driver subcommands (`dump-tokens`, `dump-ast`, … `run`,
  `test-folder`) are unchanged; only `run` integrates the loader.

---

## For future consideration

The rest of this document captures the *full* design — TRIPOS / RISC OS
heritage, manifest format, `EXPORT` keyword and `DECLARE MODULE`,
CLI commands, scripts, hot reload, slot-shadowing, resident modules
through the loader, and the open questions. None of this is wired up
yet; it's the roadmap, not the current behaviour. Read it when
planning the next slice; ignore it when writing code that has to
work today.

## Heritage

NewBCPL inherits its module-loading model from three systems, in this order
of influence:

- **TRIPOS** (Cambridge, Martin Richards, 1976). Single address space, BCPL
  global vector as the live dispatch table, command binaries loaded
  on-demand into that vector via `LoadSeg`, freed via `UnLoadSeg`. The
  CLI was a read–parse–dispatch loop with `EXECUTE` for scripts.
- **AmigaDOS 1.x** (MetaComCo, 1985). A direct TRIPOS port; we inherit the
  `1>` prompt aesthetic, the `RUN` vs `EXECUTE` split, and the `ASSIGN`
  namespace style (deferred — see open questions).
- **RISC OS modules** (Acorn, 1987). Header-described relocatable modules
  with a `(title, help, command-table, init, finalise, service handler)`
  shape, a managed module heap with `*RMTidy` for compaction, a per-module
  private word (R12) that survives across calls, and the `*Modules` /
  `*Help` / `*RMLoad` / `*RMKill` / `*RMReinit` / `*RMEnsure` verbs.

What we keep: TRIPOS's "the GLOBALS slot *is* the API" model, AmigaDOS's
command vocabulary, RISC OS's `RMEnsure` distinction (load only if this or
a newer version isn't already present), and the per-module private word.
What we drop: preemptive multitasking, process-numbered prompts, the RISC
OS SWI chunk-numbering scheme (we name slots, we don't number them).

## Concepts

- **Module.** A unit that contributes named entries to the live runtime's
  export tables and stays installed across user-program reloads. Two
  shapes:
  - **Resident** — written in Rust and statically linked into the runtime
    binary. The standard library lives here: `console` (WRITES/WRITEF/…),
    `heap` (GC inspection commands), `io` (file primitives). Resident
    modules are listed in the loader's bootstrap manifest and registered
    at frame creation.
  - **JIT-compiled** — a BCPL source file loaded by `/load path` (or
    pre-loaded by the startup script). The compiler emits a JIT image;
    the loader reads its export directory and installs the entries.
  Both shapes share one descriptor (`HostedModuleArtifact`) and one
  export table (`ExportDirectory`). The CLI and the JIT linker treat
  them identically.
- **Program vs module is `START` vs not-`START`.** A BCPL file is a
  *program* if it defines a `START` routine; everything else is a
  *module*. There is no other distinguishing declarator required.
  Programs are loaded by `/run path` (or by a script line); the loader
  calls `START` with argv and tears the program down on return. Modules
  are loaded by `/load path` (or by the active-folder scan or a startup
  script); the loader reads their export directory and installs the
  entries. Programs do not contribute to the export tables, cannot
  install or unload modules, and do not persist. Bedit gains a `Ctrl+R`
  shortcut that injects `/run <currentfile>` into the CLI and submits.
  A file with both `START` and `EXPORT` declarations is a sema error —
  it cannot be both at once.
- **`DECLARE MODULE <name>`** is an optional, top-of-file declarator
  that overrides the module's runtime name. Without it, the name
  defaults to the source-file stem (`calc.b` ⇒ module `calc`). The
  declarator is for cases where two source files want to register as
  the same module across reloads (e.g. `calc-v2.b` declaring
  `DECLARE MODULE calc` to slot into the running `calc`'s place) or
  where the user wants the runtime name to differ from the file path.
  Resident Rust modules carry their name in `native_module_artifact()`
  and ignore this entirely.
- **Programs `GET`, they do not import.** BCPL has no formal `IMPORT`
  declaration. `GET "header.h"` is textual inclusion of declarations
  into the compilation unit — useful for sharing MANIFEST constants,
  inline helper LETs, or class declarations between files. Headers are
  *not required* for module-API access: the compiler resolves unbound
  names against the loader's live symbol table (see "Name resolution"
  below). Classical BCPL programs with `GET "console.h"` keep compiling
  unchanged; new programs can just call `WRITES` directly. There is no
  automatic dependency resolution and no automatic loading — the user
  curates the loaded set by hand, and the compiler reads it.
- **Exports are auto-prefixed for JIT-compiled modules.** A BCPL source
  file that declares `DECLARE MODULE foo` (or that defaults to
  filename-stem `foo`) registers every `EXPORT name` in the loader's
  slot table under the literal key `foo_name`. Inside `foo`'s source,
  bare `name` references resolve transparently back to that mangled
  key; outside callers write `foo_name` directly. The mangling is the
  namespace mechanism — no `.`-overloading, no two-level lookup, no
  parser changes.
- **Resident Rust modules register literal names.** The runtime
  standard library (`console`, `string`, `io`, `heap`, `iGui`) ships
  its exports under bare names — `WRITES`, `WRITEF`, `FREEVEC`,
  `findinput`, `iGui_open_child`, etc. — through whatever names its
  `native_module_artifact()` factory declares. The auto-prefix rule
  applies only to JIT-compiled BCPL. This asymmetry is deliberate:
  it preserves the classical-BCPL principle (§3 of the manifesto) that
  `WRITES "hi"` keeps working without per-file `console_` prefixes,
  while user-written libraries get clean namespacing.
- **GLOBALS vector.** The flat indirection table BCPL source addresses
  via `GLOBAL { NAME : slot }` declarations. Each slot is one word. At
  load time the loader resolves a module's declared slot numbers against
  its export table and patches them into the live vector. Source addresses
  by integer; runtime and CLI address by name.
- **Export directory.** A sorted list of `ExportEntry { name, kind }`
  per module. **Commands are one kind alongside `Routine`** — they sit
  in the same directory, not a separate table. The CLI builds its
  dispatch table by walking every loaded module's directory and
  filtering on `kind == Command`. In source, exports are declared by a
  single `EXPORT` qualifier on the declaration itself (`EXPORT LET …`,
  `EXPORT COMMAND …`, `EXPORT MANIFEST …`, `EXPORT STATIC …`) — see
  "JIT-compiled module source shape" below.
- **Private word.** A single word of per-module state the runtime hands
  the module's command routines as their first argument. Typically a
  pointer to a state vector allocated in `module_init` and freed in
  `module_finalise`. Survives user-program reloads.

## Loader veneer

The loader is a thin BCPL-flavoured rename of NewCP's
[`LoaderSession`](../../NewCP/NewCP/src/newcp-loader/src/lib.rs:285).
The mechanics carry over verbatim; the surface vocabulary changes to match
BCPL idiom.

**Carries over verbatim:**

- `LoaderSession` with `next_generation: u64`, `materialized_modules`,
  `retired_materializations`, `active_executable_images`,
  `retired_executable_images`, `active_execution_scopes`,
  `quiescent_epoch`. Same field shapes, same names.
- `MaterializedModuleRecord { name, generation, path, stamp,
  has_executable_image }`. One per successful load. NewCP's `imports`
  field is dropped — BCPL modules don't declare imports (see the
  "programs GET, they do not import" concept above).
- `ExecutionScope` pinning of generations: while any scope is open, no
  retired image can drop. `note_quiescent_point` advances the epoch only
  when no scope is open.
- `RetiredImageDropPredicate` — the GC's veto over reclaiming a retired
  image. The default predicate checks whether any heap block still tags
  a `TypeDesc` inside the retired image (see manifesto §5).
- Process-global `RETAINED_IMAGES` pool on session drop, so JIT pages
  outlive the session if any heap block still points into them.
- `ActiveExecutableImage { module_name, generation, export_addresses:
  HashMap<String, usize>, method_addresses: HashMap<String, usize>,
  image: OwnedJitModule }`. The `export_addresses` map is the realized
  GLOBALS lookup — name to JIT-emitted address.
- The load pipeline, minus the dependency phase: read source → parse
  → sema → codegen → JIT → materialize → register exports → run init.
  NewCP's `SourceModuleGraph` / `dependency_edges` /
  `initialization_order` machinery is dropped — every `/load` resolves
  exactly one file. If that file refers to a name another module owns,
  the GLOBALS slot is either already populated (call succeeds) or
  empty (call fails at run time). No transitive loading.

**Renamed for BCPL:**

| NewCP                                | NewBCPL                          | Why                                                                |
|--------------------------------------|----------------------------------|--------------------------------------------------------------------|
| `ExportKind::Procedure`              | `ExportKind::Routine`            | BCPL keyword is `ROUTINE`, not `PROCEDURE`.                        |
| `ExportKind::Type`                   | *(dropped)*                      | BCPL has no user-declared types as exports.                        |
| `HostedModuleArtifact.imports`       | *(dropped)*                      | BCPL has no formal imports; the field would always be empty.       |
| `SourceModuleGraph` and friends      | *(dropped)*                      | No dependency tree — every `/load` is one file, full stop.         |
| `init_routine: String`               | `init_routine: String`           | Unchanged. Convention: name is `module_init`.                      |
| n/a                                  | `private_word: Word`             | RISC OS R12 analogue; first arg to command routines.               |
| `RustCommandHandlerSpec`             | same                             | Unchanged — already language-neutral.                              |

The retained `ExportKind` set is `{ Routine, Command, Constant, Variable
}`. `Constant` covers `MANIFEST { K = 42 }` declarations; `Variable`
covers exported `STATIC` data slots. `Command` is dispatchable from the
CLI; `Routine` is callable from BCPL through the GLOBALS vector. A module
can export any mix; there is no convention that says a `Command` cannot
also be exposed as a `Routine` (just register it twice under different
names if you want both, or have the routine wrap the command's argv shape).

**Resident-module factory.** Every Rust-resident module exposes one
function:

```rust
// in newbcpl-runtime/src/console.rs
pub fn native_module_artifact() -> NativeModuleArtifact {
    NativeModuleArtifact::new(
        HostedModuleArtifact::new(
            "console",
            ExportDirectory::new(vec![
                ExportEntry::routine("WRITES"),
                ExportEntry::routine("WRITEF"),
                ExportEntry::routine("WRITEC"),
                ExportEntry::routine("WRITEN"),
                ExportEntry::routine("NEWLINE"),
                ExportEntry::command("tee_log",
                    "Mirror console output to the log view."),
            ]),
            "module_init",
            "Rust-hosted console I/O.",
            vec![/* RustCommandHandlerSpec for tee_log */],
        ),
        vec![
            NativeExportBinding::routine("WRITES",
                writes as *const () as usize),
            NativeExportBinding::routine("WRITEF",
                writef as *const () as usize),
            // ...
            NativeExportBinding::command("tee_log",
                tee_log_handler as *const () as usize),
        ],
    )
}
```

The loader's bootstrap calls every resident module's
`native_module_artifact()` once at frame creation and registers them.
From then on, JIT-compiled BCPL code resolves `WRITES` against the
loader's symbol table exactly the way it would for any other module.

**JIT-compiled module source shape.** A BCPL source file is a module
whenever it doesn't define a `START` routine. The optional
`DECLARE MODULE` declarator overrides the runtime module name; absent
that, the name defaults to the source-file stem. Every `EXPORT` is
registered in the loader's slot table under the mangled key
`<module>_<export-name>`. Inside the module, code can call exports by
their bare name; outside code uses the mangled form.

```bcpl
DECLARE MODULE calc                  // optional; defaults to filename stem

EXPORT MANIFEST module_version = "1.0"
EXPORT MANIFEST module_help    = "Stack-based desk calculator."

LET module_init() = VALOF $(
    LET state = VEC 4
    state!0 := 0
    RESULTIS state
$)

LET module_finalise(state) BE FREEVEC(state)

// Procedure/function exports.
// Bare names inside this file; mangled `calc_add` / `calc_sub` to
// outside callers.
EXPORT LET add(state, x) = state!0 + x
EXPORT LET sub(state, x) = state!0 - x

// A CLI command — takes (argv, argc), has a sibling _help MANIFEST.
// Outside it's `calc_show`; inside this module, just `show`.
EXPORT COMMAND show(state, argv, argc) BE WRITEF("acc=%d*N", state!0)
EXPORT MANIFEST show_help = "Print the accumulator."

// Internal helper — no EXPORT, no GLOBALS slot, file-local only.
LET clamp(x, lo, hi) = x < lo -> lo, x > hi -> hi, x
```

A program calling into this module:

```bcpl
LET START() BE $(
    calc_add(0, 3)         // resolves via loader → slot for `calc_add`
    WRITES("Hello*N")      // resolves via loader → resident `WRITES` (in console)
$)
```

No `GET`, no `GLOBAL { ... }` — the compiler queries the loader for
both `calc_add` and `WRITES` and emits direct slot-indexed indirect
calls.

Notes on the shape:

- **No `START` ⇒ module.** The parser does not need a top-level
  declarator to know what kind of file it's looking at. The presence or
  absence of `START` is the signal.
- **`DECLARE MODULE name`** is optional. With it, the module registers
  under `name`. Without it, the module registers under the source-file
  stem. A file with `START` *and* `DECLARE MODULE` is a sema error.
- **`EXPORT` is a single qualifier** on whatever it prefixes. The
  compiler reads the inner declarator to decide the export kind:
  - `EXPORT LET name(...) = ...` and `EXPORT ROUTINE name(...) BE ...`
    → `ExportKind::Routine`.
  - `EXPORT COMMAND name(argv, argc) BE ...` → `ExportKind::Command`.
    The signature is fixed (the CLI fills `argv`/`argc` from the typed
    line); a sibling `EXPORT MANIFEST <name>_help = "…"` carries the
    help string `/help <module> <name>` prints.
  - `EXPORT MANIFEST name = expr` → `ExportKind::Constant`.
  - `EXPORT STATIC name = expr` → `ExportKind::Variable`.
- **Auto-prefix mangling.** Every export is registered under
  `<module>_<name>` in the loader's slot table. Inside the module's
  source, bare references to its own exports resolve transparently.
  Outside callers — and the CLI — see only the mangled form.
- **Version is a MANIFEST constant by convention.** Modules that want
  `/ensure foo 1.2` to work declare `EXPORT MANIFEST module_version =
  "1.0"`. The loader reads this from the export directory (under the
  mangled key `foo_module_version`) after JIT. Modules that don't
  declare one are version-less; `/ensure` against them always reloads.
- **Module-level help** is the same convention: `EXPORT MANIFEST
  module_help = "..."`. `/help <module>` reads this.
- **No `GLOBAL { name : N }` declarations needed.** The loader picks
  slot numbers; the compiler queries the loader to find them. The
  `GLOBAL` declaration is still accepted in source as an explicit pin
  for ABI-stable cases (e.g. when a resident-stdlib equivalent in the
  reference compiler used slot 200 and we want to match), but ordinary
  modules don't write any.
- `module_init` and `module_finalise` are looked up by literal name in
  the JIT image, *not* the mangled form — no `EXPORT` qualifier needed,
  and they don't appear in the export directory. Missing them is fine.
- `EXPORT` on an `AND`-chained binding (`EXPORT LET f(...) = ... AND
  g(...) = ...`) exports only the head of the chain; subsequent clauses
  need their own qualifier if they're meant to be exported. (Open
  question — see below.)

The parser emits a `SourceExportKind` per declared name (mirroring
NewCP's `SourceExportKind`); sema validates kinds against declared types
and slot numbers; the loader builds the runtime `ExportDirectory` from
the parser's output and patches `export_addresses` after JIT.

## Module lifecycle

| Verb                    | What happens                                                                                                                                                                  |
|-------------------------|-------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| `/load path`            | Read source, sema-check via `newbcpl-sema` on UI thread, JIT on language thread, allocate generation, look up `module_init`, call it on language thread, install GLOBALS slots, register exports, return to prompt. |
| `/unload name`          | Clear the module's GLOBALS slots, drop its commands, close module-owned windows, call `module_finalise(state)`, retire the executable image (subject to drop predicate).      |
| `/reinit name`          | `/unload` then `/load` of the same path. The private word is not preserved — modules that want sticky state must serialise it themselves.                                     |
| `/ensure name v`        | If `name` is not loaded, or its version is older than `v`, `/load` its known file. Otherwise no-op. Mirrors RISC OS `*RMEnsure`.                                              |
| `/modules`              | List installed modules: name, version, kind (resident vs JIT), command count, first line of help.                                                                              |
| `/help [m [c]]`         | No args: list built-ins. One arg: print module-level help. Two args: print command help (from `ExportEntry`'s help text).                                                     |
| `/tidy`                 | Force `drive_quiescent_collection` — reclaim any retired generations whose `collect_after_quiescent_epoch` has passed and whose drop predicate clears them.                   |

Modules are loaded **only** by the CLI or by a startup script. A running
BCPL program cannot `/load` or `/unload` anything; the verbs are not
exposed as BCPL routines. Modules form the runtime support library;
programs are the things that *use* it.

## Name resolution

The compiler resolves each unbound identifier against four scopes in
order:

1. **Local lexical scope.** Parameters, `LET`s in enclosing blocks.
2. **Own-module exports** (if compiling a module). Bare `name` inside
   module `foo` resolves to the mangled key `foo_name` if `foo` has an
   `EXPORT … name`. The mangling is transparent to the source.
3. **Loader symbol table.** The process-wide name → slot map populated
   by every loaded module — resident modules register literal names
   (`WRITES`, `FREEVEC`, …); JIT modules register mangled names
   (`calc_add`, `colour_console_print`, …). One flat namespace.
4. **Sema error.** `unbound name 'foo': not declared in this file and
   no loaded module exports it; /modules to list, /load to install`.

Inside a module, calls to other-module exports must be written under
their mangled name: `foo`'s code calling `bar`'s `helper` writes
`bar_helper(...)`, not `bar.helper(...)`. The mangling *is* the
qualification.

The compiler's lookup happens at JIT time, so it depends on the
loader's current state — modules a program needs must be loaded before
the program is `/run`. Failures here are clear sema errors; they don't
become silent dynamic dispatch problems. Bedit's `install_checker`
closure gets a reference to the loader so the IDE shows the same
errors the JIT would.

`GET "file.h"` retains its classical meaning — textual inclusion of
the file's contents into the compilation unit. Use it for sharing
MANIFEST constants, inline helper LETs, or type declarations across
files. It is *not* required for module-API access; the compiler queries
the loader for that.

## GLOBALS, physically

The reference compiler proposes [a symbol-table replacement for the
global vector](../reference/documentation/BCPL%20Runtime.md#23-modernising-the-global-vector).
NewBCPL keeps the *symbolic* part of that and the *vector* of TRIPOS:

- One process-wide GLOBALS vector, allocated by the runtime at startup.
  Default 4096 slots. Slot 0 is reserved.
- Each slot has a name in a parallel name table. Resident modules
  register literal names at bootstrap; JIT modules register mangled
  names (`<module>_<export>`) on `/load`.
- The loader allocates slot numbers; source does not pick them. A
  `GLOBAL { name : N }` declaration is honoured as an explicit pin
  (useful for ABI-stable resident-stdlib entries where the slot number
  is part of the contract), but ordinary user code doesn't need to
  write one — the compiler queries the loader and gets back whatever
  slot the loader chose.
- After name → slot resolution, codegen emits direct loads through the
  slot index. No HashMap lookup at call time; the lookup happens once
  at compile-time per call site.
- `iGui_replace_global(name, fn_ptr)` is the runtime helper for dynamic
  install — modules normally don't need it because the export-directory
  path installs slots automatically, but it stays available for modules
  that want to swap in a tracing version of a routine on demand.
- **Slot-shadowing on duplicate install.** Loading two modules that
  register the same mangled key is allowed; the second wins, but
  `/unload`ing it restores the first. With mangling in place this is
  now naturally rare — it only fires when (a) two `DECLARE MODULE foo`
  files both declare `EXPORT name`, or (b) a JIT module deliberately
  uses a literal name that clashes with a resident export. Both are
  intentional — case (a) is debug-shadows-release, case (b) is
  override-the-stdlib-for-this-session.
- **Shadow warnings.** Case (b) — shadowing a resident-stdlib export
  from JIT BCPL — always logs to the log view:
  `[loader] WRITES: JIT module 'tee_writes' shadows resident 'console'`.
  Case (a) is silent by default (it's the intended workflow for tracing
  builds) but emits the same form of message under a
  `--warn-module-shadowing` driver flag.

When the running program declares `GLOBAL { my_helper : 300 }` and writes
to slot 300, the loader does *not* persist that write across program
reloads — `/run` and bedit's `Ctrl+R` both clear program-owned slots on
program teardown. Only module-installed slots survive.

## The CLI window

`cli` is the third iGui-owned MDI child class, sibling to `bedit` and
`log_view`. It registers in `iGui::child::register_classes`, owns the
`Tools → CLI / Ctrl+Shift+C` accelerator (next free `MENU_CMD_ID` after
the log view), and stays open for the lifetime of the frame.

The window has two regions: a scroll-back area showing prior commands
and their output, and a one-line input field at the bottom. The prompt
is `1>` — Amiga heritage. Output from `WRITES`/`WRITEF` calls inside
modules echoes to the CLI scroll-back when stdout is bound to the CLI
(the default for `/run` and for module commands).

### One BCPL execution at a time

The CLI executes one BCPL action at a time. While a module command or a
running program is on the language thread, additional input lines are
**queued visibly**: each pending line appears above the prompt as

```
Q: /run benchmark.b
Q: /modules
1>
```

When the current action returns, the queue drains in order. Built-in
commands (everything starting with `/` except `/run` and `/execute`) bypass
the queue — they're CLI-thread operations that don't touch the JIT and
can run while a program is executing. `/modules`, `/help`, `/clear`,
`/tidy` therefore stay responsive even when a program is busy.

Abort: typing `Ctrl+Break` in the CLI sets a cancellation flag the JIT
emits checks for at back-edges (open question: detailed mechanism in
manifesto §6 follow-up). On v1, an abortable program is a nice-to-have;
on v0, "wedged" means closing the frame.

### Dispatch

1. Empty lines are ignored.
2. First token = verb. If it starts with `/`, it's a CLI built-in (see
   below) and runs on the UI thread.
3. Otherwise the CLI looks up `<verb>` in the loader's slot table —
   the same lookup the compiler uses for source-level name resolution.
   The verb is the mangled name: a JIT module declared
   `DECLARE MODULE calc` exporting `EXPORT COMMAND show(...)` is typed
   as `calc_show`. Resident modules expose literal names.
   - Match with `kind == Command`: dispatch on the language thread,
     blocking the prompt until it returns.
   - Match with `kind == Routine`: also dispatch — every exported
     routine is a candidate command, the CLI parses argv against the
     routine's declared signature. A routine taking `(state, argv,
     argc)` gets the CLI's parsed argv vector; a routine taking
     `(state, x)` gets `argv!0` parsed as integer.
   - Match with another kind (`Constant`, `Variable`): print the value.
   - No match: print `unknown command: <verb>; /modules for installed,
     /help for built-ins`.
4. Command return value becomes `$?` for the next prompt and the next
   script line.

The mangled-name dispatch is deliberate — the CLI doesn't split
`calc_show` into "module `calc`, command `show`". That keeps the
parser trivial and matches the RISC OS `OS_File` / `Wimp_Initialise`
style. If you forget which module exports a verb, `/modules` shows
every loaded module with its exports listed under their mangled names.

### Built-in commands

Built-ins are not modules. They live in the CLI and cannot be unloaded.

| Built-in              | Behaviour                                                                                              |
|-----------------------|--------------------------------------------------------------------------------------------------------|
| `/help [m [c]]`       | As above.                                                                                              |
| `/modules`            | List installed modules.                                                                                |
| `/load <path>`        | Load a module file. Path is interpreted relative to the working directory.                             |
| `/unload <name>`      | Unload by module name.                                                                                 |
| `/reinit <name>`      | Unload + load, same file.                                                                              |
| `/ensure <name> <v>`  | Conditional load. Useful in scripts: "make sure `foo` ≥ 1.2 is present before I `/run` the program".   |
| `/install <path>`     | Copy a module file into the active-modules folder (see below). Does not load it; takes effect at next boot, or follow with `/load`. |
| `/remove <name>`      | Unload + delete the module file from the active-modules folder. The library copy under `modules/` is untouched. |
| `/tidy`               | Run `drive_quiescent_collection`.                                                                      |
| `/run <path> [args…]` | Load the file as a *program* (not a module), call `START` with `args`, tear it down on return.         |
| `/execute <path>`     | Read the file as a CLI script and feed its lines through this same loop. Synchronous; see below.       |
| `/echo <text>`        | Print `text`. Honoured inside scripts.                                                                 |
| `/cd <dir>`           | Change the CLI's working directory (affects relative paths in `/load`, `/run`, `/execute`).            |
| `/dir [pattern]`      | List the working directory (defaults to `*`).                                                          |
| `/clear`              | Clear the CLI scroll-back. The log view is unaffected.                                                 |
| `/quit`               | Close the frame. Same as picking Close from the system menu.                                           |

The `/` prefix is so module-exported commands can use the naked form.
`/load` is unambiguously the built-in; `load` is whatever module last
installed it, or an error.

## Installation: the active-modules folder

BCPL's legacy is manual curation. The user decides which modules are
present; the runtime does not. There is no automatic dependency
resolution and no module package manager. Two ways to make a module
available at boot:

1. **Drop the file into the active-modules folder.** The runtime scans
   one folder at boot and `/load`s every BCPL source file inside, in
   alphabetical order, after resident modules have registered. Default
   path is `./modules-active/` (relative to the runtime binary); the
   `NEWBCPL_MODULES_ACTIVE` environment variable overrides. Files
   ending in `.b` are loaded; anything else is ignored.
2. **Write a startup script.** If `./startup.script` (or
   `$NEWBCPL_STARTUP`) exists, the CLI executes it after the active
   folder scan but before the first prompt. The script can `/load`
   files from anywhere on the filesystem (one-offs that don't live in
   the active folder), `/ensure` version requirements, `/echo` a
   welcome banner, or `/run` a program directly.

The two paths compose: the active folder is the "default set"; the
startup script tunes it. A common pattern is to keep a separate
**library** folder (`./modules/`, by convention only — the runtime does
not look here) where every module the user might want lives, and let
the startup script populate the active folder by copying or symlinking
from there. The built-in `/install <path>` does the copy step from the
CLI; `/remove <name>` does the reverse.

### Boot sequence

```
1. iGui frame opens; CLI, bedit, log_view register their WndClasses.
2. LoaderSession::new() — resident Rust modules register their
   NativeModuleArtifact: console, string, io, heap, iGui.
3. Active-folder scan: for each `*.b` in $NEWBCPL_MODULES_ACTIVE
   (default `./modules-active/`), in alphabetical order, run /load.
   A load failure is logged but does not abort the boot.
4. Startup script: if $NEWBCPL_STARTUP (default `./startup.script`)
   exists, run /execute on it. A script failure is logged.
5. CLI prompt appears at `1>`.
```

The order matters: resident modules first (so `WRITES` is available
when a folder-loaded module's `module_init` wants to log), then the
folder (the curated default set), then the script (which can override
or extend). A folder-loaded module that wants to take effect *before*
the startup script runs cannot — but a script that runs *before* the
folder is not something we provide; the folder is the curated default
and the script is the fine-tuning.

### Deployment recipes

- **Library distribution.** Ship a folder of `.b` files. The user
  copies them into their `modules-active/`. Next boot they're all
  loaded. No script needed.
- **App distribution.** Ship the program file + the modules it depends
  on + a `startup.script`. The script runs `/load mod1.b`, `/load
  mod2.b`, … and ends with `/run main.b`. The user invokes
  `newbcpl-driver --startup my-app/startup.script` (or sets
  `NEWBCPL_STARTUP`); the frame opens, the script loads modules and
  runs the program, the user sees the program's UI.
- **Library + app, sharing the active folder.** The user already has
  their preferred modules in `modules-active/`. The app's
  `startup.script` only loads the modules it needs that aren't already
  there (using `/ensure name version`), then `/run`s. Multiple apps
  share the same active set without re-loading common modules.

Modules are not versioned by filename. Two `.b` files in the active
folder both registering as module `calc` (whether by filename stem or
by explicit `DECLARE MODULE calc`) would conflict — the second
overrides the first (manifest §6 slot-shadowing). The library-folder
convention is the lever for keeping versions distinct on disk.

## EXECUTE scripts

A script is a plain text file of CLI commands, one per line. Lines
starting with `;` are comments. `/execute foo.script` runs them
synchronously; the prompt does not return until the script finishes.
There is no `/run` analogue for scripts — they don't background.

Substitution: a script's command-line arguments are reachable as `<1>`,
`<2>`, … inside lines, expanded before parsing. `<*>` is the whole
argument list. This is the TRIPOS / AmigaDOS pattern; we deliberately
do not import AmigaDOS's `.KEY` / `.BRA` / `.KET` template-rebinding
machinery because it's load-bearing only when you're trying to make
scripts look like the commands they call, and our CLI scripts are short.

Control flow: `IF $? = 0 ...` and `SKIP <label>` / `LAB <label>` are
the only constructs. No loops, no expressions beyond integer equality.
A script that wants real logic should be a module with a one-shot
command.

## Module-owned state

The private word is the module's foothold across user-program reloads.
Three rules:

- **The module owns the allocation.** `module_init` returns it;
  `module_finalise` releases it. The runtime never inspects its shape
  and never frees it.
- **The runtime keeps the pointer alive.** It lives in the module's
  `MaterializedModuleRecord`, so the GC roots-out from there.
- **It's the first argument to every command routine.** A command that
  doesn't need state takes `(state, _, _)` and ignores `state`; a
  command that does threads it through normal BCPL field access.

When a module is unloaded, the sequence is: clear GLOBALS slots →
close module-owned windows → call `module_finalise(state)` → retire
the executable image. The retired image cannot drop until quiescence
*and* the `RetiredImageDropPredicate` clears it (the GC will veto if
any heap block still tags a `TypeDesc` inside the image).

## Module-owned windows

Per manifesto §6, modules can open MDI children via `iGui_open_child`.
The runtime tags each open child with the module that opened it. On
`/unload`, the runtime closes those children before calling
`module_finalise`. A module that wants a window to outlive its module
needs a service-handler pattern that re-anchors ownership in a longer-
lived module; "detach this window from me" is not a primitive.

## First slice of resident modules

Initial bootstrap manifest, all `ModuleKind::ResidentRust`:

| Module    | Exports (literal names, no auto-prefix)                                                  | Commands                                                       |
|-----------|------------------------------------------------------------------------------------------|----------------------------------------------------------------|
| `console` | `WRITES`, `WRITEF`, `WRITEC`, `WRITEN`, `NEWLINE`, `READS`, `READC`                      | `tee_log` (mirror stdout to log view)                          |
| `string`  | `strcmp`, `strcat`, `strcpy`, `strlen`, `atoi`, `itoa`                                   | —                                                              |
| `io`      | `findinput`, `findoutput`, `selectoutput`, `endread`, `endwrite`, `readn`, `writen`      | —                                                              |
| `heap`    | —                                                                                        | `/gc` (force collect), `/heaps` (list), `/heap-stats`, `/find-vec` |
| `iGui`    | `iGui_open_child`, `iGui_close_child`, `iGui_set_title`, `iGui_log`, batch primitives    | `tools` (list iGui-owned tool windows)                         |

Resident modules register literal export names through their
`native_module_artifact()` factory; the JIT auto-prefix rule does
*not* apply to them. So a BCPL program writes `WRITES("hi*N")` and
`iGui_open_child(...)` — the names users have seen since the 1974
manual and since the iGui module landed, respectively. The GC itself
stays a Rust primitive (manifesto §5); only its inspection surface is
BCPL-reachable via the `heap` module. `iGui` is a resident module
because its C-ABI entry points already live in the runtime crate
([cp_exports.rs](../src/newbcpl-runtime/src/igui/cp_exports.rs));
wrapping them in a `NativeModuleArtifact` factory is mechanical.

## Open questions

1. **Service / event subscription.** RISC OS modules subscribe to a
   service-handler vector for system events. Do we need an analogue for
   modules listening to `iGui` theme changes, focus events, or `Ctrl+Break`
   notifications without owning a window? Probably yes eventually; not in v1.
2. **ASSIGN namespace.** AmigaDOS's `ASSIGN LIBS: SYS:libs` style is useful
   for module search paths. Defer until we have more than five modules to
   actually search for.
3. **Detaching module-owned windows.** Cleanest answer is probably "no
   detach — if a window outlives the module that opened it, the module
   didn't really own it; refactor to a service-handler pattern". Punt to
   when we have a real example.
4. **Slot-shadowing scope.** With auto-prefix mangling in place,
   slot-shadowing only fires when two modules deliberately register
   the same mangled key — `DECLARE MODULE foo` declared by two files,
   or a JIT module choosing a literal export name that clashes with a
   resident-stdlib entry. The stack-shadow rule (second wins,
   restored on unload) covers both intentional cases. Open: do we
   ever need a hard-collision mode (refuse the second install
   instead of shadowing)? Probably not in v1 — the warning on
   resident-shadow is loud enough — but revisit if real cases
   surface accidental collisions despite the mangling.
5. **`EXPORT` on `AND`-chained bindings.** `EXPORT LET f(...) = ... AND
   g(...) = ...` — does the qualifier apply to the whole mutual-recursion
   group or only to `f`? Lean toward "only the head; later clauses
   re-qualify if needed" because it's the rule that composes with the
   rest of the qualifier system (`STATIC`, `PUBLIC` etc.). But a list of
   names that need exporting in lockstep is awkward to write that way,
   so we may want an `EXPORT $( f, g, h $)` group form too. Decide
   alongside the parser work.
6. **Aborting a running BCPL action.** `Ctrl+Break` in the CLI posts a
   cancellation flag the JIT checks at back-edges. v0 is "no abort, close
   the frame if wedged"; v1 needs the back-edge checks. Decide alongside
   the JIT lowering.
7. **Module version comparison for `/ensure`.** Single decimal (`1.2`),
   semver triple, or lexicographic string? Lean toward `MAJOR.MINOR`
   decimals with numeric comparison, both fields required.
8. **The BCPL source side of dynamic GLOBALS install.** Manifest-driven
   `EXPORT` covers the static case. For runtime install (e.g. a module
   that swaps in a tracing version of a routine on demand), do we expose
   a built-in `INSTALL "name" = expr` form, an ordinary function call,
   or both? Same as manifesto §6 open question 2.
9. **Active-folder load order.** Alphabetical is simple and predictable
   but couples module names to load priority. RISC OS handled this with
   an explicit boot file listing modules in order; AmigaDOS's
   `Startup-Sequence` was equivalent. Our startup script already plays
   that role for the precise-order case, so alphabetical for the folder
   is probably fine — modules whose order matters belong in the script.
   Revisit if real cases force the issue.
10. **`/install` and `/remove` scope.** Are these *just* file copies, or
    do they also trigger an immediate `/load` / `/unload`? Lean toward
    install = copy only (so the user can stage a change without
    activating it), and explicit `/load` / `/reinit` to make it live.
    `/remove` includes `/unload` for symmetry with how the user thinks.
