# wingui — BCPL surface design

A proposal for what NewFB's window / graphics commands should look
like when ported into NewBCPL. The shared
[`wingui-rs` Rust crate](../../NewFB/docs/wingui-port-architecture.md)
already exists at `E:\NewFB\src\wingui-rs\` — it talks to the
`multiwingui` C++ DLL and presents a typed Rust API every sibling
compiler binds against. **This document is about the BCPL-level
verbs the user types in their `.bcl` source**, not the Rust glue
underneath.

## How this relates to existing iGui

NewBCPL already ships `iGui_*` builtins (Direct2D + DirectWrite,
the GUI bedit and the log view use them). Those are an embedded
framework — the iGui frame is the host, and a BCPL program plugs
its child windows into it.

`wingui` is different: it's a *retro-graphics framework* — sprite
surfaces, indexed-colour palettes, vector draw, audio. The two
coexist:

* **iGui** is the editor's GUI host: bedit, log view, menus,
  developer-facing chrome.
* **wingui** is what a BCPL **program** uses to draw its own UI —
  the game, the visualiser, the small windowed app a user writes.

This document covers wingui's surface only. iGui stays as is.

---

## NewFB verb inventory (the source material)

After scanning `E:\NewFB\bas\demo\*.bas`, the verb tree groups into:

### Window structure

| FB verb | Effect |
|---|---|
| `WINDOW DEFINE id, title, x, y, w, h` ... `END WINDOW` | Declare a window with its controls inside a block. |
| `WINDOW SHOW id` | Make the window visible. |
| `WINDOW CLOSE id` | Close one window (event loop keeps running). |
| `WINDOW SHUTDOWN` | Tear down everything, exit the GUI. |
| `WINDOW EVENT(win, ctl)` → status | Pump events; populate `win`, `ctl` by reference. |

### Controls (declared inside a `WINDOW DEFINE` block)

`WINDOW BUTTON`, `WINDOW LABEL`, `WINDOW CANVAS`, `WINDOW CHECKBOX`,
`WINDOW COMBOBOX`, `WINDOW PROGRESS`, `WINDOW MATRIX`, `WINDOW POPUP`
— each takes `id, ..., x, y, w, h`.

### Canvas drawing (between `WINDOW CANVAS BEGIN` / `END`)

`COLOR r, g, b, a` · `FILL r, g, b, a` · `PAPER r, g, b, a` ·
`CLEAR` · `NOFILL` · `LINEWIDTH n` · `LINE x1, y1, x2, y2` ·
`CIRCLE cx, cy, r` · `RECTANGLE x, y, w, h` ·
`ARC cx, cy, r, start_deg, end_deg, fill_flag` ·
`TEXT x, y, "string"` · `FILLAREA x, y, w, h` ·
`PATH BEGIN` ... `PATH END` (with `MOVETO`, `LINETO`).

### Image / sprite verbs

`WINDOW IMAGE CREATE id, w, h` ·
`WINDOW IMAGE BEGIN id` ... `WINDOW IMAGE END` ·
`WINDOW IMAGE DESTROY id` ·
`BLIT id, x, y` · `BLITSCALE id, x, y, w, h` · `BLITFLIP` · `BLITSOLID`.

### Fullscreen graphics (alternative to windowed)

`SCREEN id, w, h, bits` · `SCREENCLOSE` · `SCREENSAVE` · `SCREENTITLE`.

### Audio

`SOUND freq, ms` · `MUSIC "..."` (ABC notation).

### Misc

`SLEEP seconds` · `APPNAME "..."` · `BRK`.

---

## Two BCPL design options

BCPL has no compound verbs. `WINDOW DEFINE` is two tokens to FB's
parser; to a BCPL parser it's two identifiers in a row. We need a
shape that fits BCPL's grammar.

### Option A — Procedural (mirrors the FB shape 1:1)

Translate each compound verb to a snake-cased procedure name; the
declarative block becomes explicit `WIN_BEGIN` / `WIN_END` calls.

```bcpl
LET START() BE $(
  WIN_DEFINE(1, "Arc Demo", 100, 100, 520, 480)
    WIN_CANVAS(10, 10, 10, 500, 440)
    WIN_BUTTON(20, "Quit", 400, 450, 80, 24)
  WIN_END()

  WIN_SHOW(1)
  draw_arcs()

  LET running = TRUE
  WHILE running DO $(
    SLEEP(50)
    LET win = ?
    LET ctl = ?
    IF WIN_EVENT(@win, @ctl) THEN $(
      IF win = 1 & (ctl = 0 | ctl = 20) THEN running := FALSE
    $)
  $)
  WIN_SHUTDOWN()
$)

AND draw_arcs() BE $(
  CANVAS_BEGIN(1, 10)
    PAPER(30, 30, 45, 255)
    CLEAR()
    COLOR(200, 200, 200, 255)
    FILL(255, 80, 80, 255)
    ARC(80, 90, 60, 0, 90, 1)
  CANVAS_END()
$)
```

**Pros**: Shortest path from a FB sample to a BCPL sample — line-
for-line translation. Verbs read like classical BCPL. No
inheritance, no allocation; pure procedure calls.

**Cons**: Pollutes the global name space with ~30 short verbs
(`COLOR`, `FILL`, `ARC`, `TEXT`...) that may collide with user
variables. The "current canvas" between `CANVAS_BEGIN` and
`CANVAS_END` is implicit global state — non-reentrant.

### Option B — Class-oriented (idiomatic modern BCPL)

Lean on the class machinery we've spent the year building.
`USING` gives deterministic cleanup; method dispatch handles the
"current canvas" without globals.

```bcpl
LET START() BE $(
  USING app = NEW App("Arc Demo") DO $(
    LET w AS Window = app.window(1, "Arc Demo", 100, 100, 520, 480)
    w.canvas(10, 10, 10, 500, 440)
    w.button(20, "Quit", 400, 450, 80, 24)
    w.show()

    draw_arcs(w)

    LET ev AS Event = NEW Event
    WHILE app.next_event(ev) DO $(
      IF ev.window = 1 & (ev.control = 0 | ev.control = 20) THEN
        BREAK
    $)
  $)
$)

AND draw_arcs(w AS Window) BE $(
  USING c = w.canvas_paint(10) DO $(
    c.paper(30, 30, 45, 255)
    c.clear()
    c.color(200, 200, 200, 255)
    c.fill(255, 80, 80, 255)
    c.arc(80, 90, 60, 0, 90, 1)
  $)
$)
```

**Pros**: No global state — every drawing op is on an explicit
canvas handle. `USING` cleans up windows, canvases, images
automatically. The `Window` / `Canvas` / `Image` types document
themselves; sema's class-identity propagation catches
`canvas.button(...)` typos. Composes well with the rest of the
language's modern surface (param annotations, FINAL, etc.).

**Cons**: A real port of `02_canvas_arc.bas` is more lines
because every action needs an explicit receiver. Class layouts
need to be defined per object type; the binding crate must thread
them through.

### A pragmatic third option — A on top of B

A class layer for the people who want it; thin procedural
wrappers for the people who want to copy-paste FB samples.
`WIN_DEFINE(...)` becomes a one-liner around `NEW Window(...)` +
a thread-local "current window". The classes are the real API;
the procedural verbs are an FB-compat shim. Both work from the
same `wingui-rs` underneath.

```bcpl
// Class form (preferred for new code)
LET w AS Window = NEW Window(1, "Title", 100, 100, 320, 200)
w.button(10, "OK", 20, 20, 80, 24)
w.show()

// Procedural form (FB-compat; same semantics)
WIN_DEFINE(1, "Title", 100, 100, 320, 200)
  WIN_BUTTON(10, "OK", 20, 20, 80, 24)
WIN_END()
WIN_SHOW(1)
```

---

## Naming convention

Whichever option we pick, the names need a clear story:

* **Class names** are CamelCase: `Window`, `Canvas`, `Image`,
  `Event`, `App`. Matches the existing class examples in tier 5
  probes.
* **Method names** are lowercase: `w.show()`, `c.arc(...)`. Same
  as the existing class examples (`p.getX()`,
  `c.distance_from_origin()`).
* **Procedural shim names** are SCREAMING_SNAKE_CASE prefixed by
  the subsystem: `WIN_DEFINE`, `WIN_BUTTON`, `WIN_SHOW`,
  `CANVAS_BEGIN`, `CANVAS_END`. The prefix avoids colliding with
  user code (which by convention uses lower-case or local
  spellings like `START`).
* **Drawing verbs** (`COLOR`, `FILL`, `ARC`, ...) are
  unprefixed — they're frequent enough that the noise of a
  prefix would dominate. Users get them only when they
  explicitly `GET "wingui_draw.bcl"` (a header that declares
  them), so casual programs avoid the namespace pollution.

---

## Header layout

The bindings ship as a `modules-active/` module so the loader's
symbol resolution pulls them in:

```
modules-active/
  wingui.bcl              // class declarations + procedural shim
  wingui_draw.bcl         // drawing verbs alone (COLOR, ARC, ...)
                          //   GET this when you want them
  wingui_audio.bcl        // SOUND, MUSIC, etc. — optional GET
```

The split lets a program that only wants a button-and-label form
pull in `wingui.bcl` without the canvas surface that comes with
`wingui_draw.bcl`.

Each module declares its classes (`CLASS Window $(...)`) and
forwards each method to the runtime's `__newbcpl_wingui_*`
builtin via direct calls. The procedural shim names are
defined in `wingui.bcl` as plain `LET` routines that call the
class-method path.

---

## Event loop shape

This is the design's most opinionated decision. FB does:

```basic
DO WHILE running
  status = WINDOW EVENT(win, ctl)
  IF status = 1 THEN ...
  SLEEP 0.05
LOOP
```

— polled at ~20 Hz. Easy to understand but burns CPU and adds
latency between event and response.

Three BCPL options:

**E1 — Direct port of the polled loop.**  Cheapest. Hands the
user a `WIN_EVENT(@win, @ctl)` or `app.next_event(ev)` that
returns a status code. Sleep is the program's responsibility.
Matches FB exactly.

**E2 — Blocking event read.**  `app.wait_event()` blocks until
the next event arrives, returns an `Event` value. No sleep
needed. Closer to how Win32 message loops work natively. Adds
"how do I get periodic work done?" as a question the user has to
answer (timer event from the framework).

**E3 — Callback registration.**  `app.on_click(button_id, body)`
registers a closure-like routine to fire when the event arrives.
BCPL has function pointers; this is feasible. Best ergonomics
but requires the most runtime support and changes the
**imperative** flavour of BCPL.

I'd suggest **E1 for the first cut** (matches FB, easiest to
explain) with **E2 as a follow-up addition** (an `app.wait_event`
method that internally blocks). E3 is a longer-term aspiration —
once `BLOCK` / `RUNQ` (BCPL coroutines, if we ever add them) lands.

---

## Recommendation

**Go with the pragmatic third option — class API as the
foundation, procedural shim on top.** Six reasons:

1. The class form is forward-compatible with everything else
   we're building (USING, FINAL, param annotations, indirect
   dispatch). New BCPL code should look like new BCPL code.
2. The procedural shim is one extra ~200-line `wingui.bcl`
   module — small. It pays for itself the first time you want to
   paste an FB demo into BCPL and see it run.
3. Naming convention story is clean: `Window` / `w.show()` for
   the modern surface; `WIN_DEFINE` / `WIN_SHOW` for the legacy
   verbs.
4. Drawing verbs (`COLOR`, `FILL`, `ARC`, ...) stay unprefixed
   under both forms — they're frequent enough that the ergonomics
   demand it, and the optional-GET pattern keeps casual programs
   clean.
5. E1 event loop (polled `app.next_event`) is the simplest first
   cut; nothing in the design painted us into a corner for E2
   later.
6. The shared `wingui-rs` crate stays language-agnostic; the
   BCPL-specific layer is a thin module that maps both forms to
   the same underlying `Session` calls.

---

## Implementation status (live)

| Phase | Status |
|---|---|
| **W1** — crate scaffold + version probe + class form in-source | ✅ landed |
| **W2** — port hosting code (window-define, button, show, run, close) | pending |
| **W3** — procedural shim (`WIN_DEFINE` etc.) + port two FB demos | pending |
| **W4** — audio + screen-mode verbs | pending |
| **W5** — event-loop iteration to E2 (blocking) | pending |

### W1 deliverable

* `src/bcpl-wingui` crate (path-dep on
  `../../../NewFB/src/wingui-rs`). Two builtins
  registered: `bcpl_wingui_version_packed`,
  `bcpl_wingui_is_available`.
* `examples/wingui_hello.bcl` demonstrates the class form
  (`NEW App(...)`, `app.is_available()`, `app.version()`,
  `NEW Window(...)`) and runs end-to-end through the JIT,
  the FFI shim, `wingui-rs`, and `wingui.dll`.
* Output proves the bridge: `wingui available? 1`, version
  reported from the actual DLL.

### Known gap: classes in `modules-active/`

The original plan put the class definitions in
`modules-active/wingui.bcl` so user programs could write
`NEW App(...)` without an inline declaration. The loader's
name-prefix pass clashes with vtable globals: every function
gets prefixed `<stem>_<name>` (so `App_CREATE` →
`wingui_App_CREATE`), but `@App.vtable` references the
un-prefixed names. Result: "Linking globals named
`'App.vtable'`: symbol multiply defined!".

This isn't a wingui-specific issue — it would hit any class
declared in a modules-active file. For W1 we sidestep by
keeping the class definitions inline in the demo source.
Fixing properly needs the loader to either skip prefixing for
class methods or co-rename vtable references; that's a separate
piece of work tracked as its own follow-up.

## Next turns deliver

| Phase | Work |
|---|---|
| **W1** | `bcpl-wingui` crate scaffold + builtin registry |
| **W2** | `Window` / `Canvas` / `Image` classes in `wingui.bcl`; one demo (`examples/wingui_hello.bcl`) |
| **W3** | Procedural shim (`WIN_DEFINE` etc.); port two FB demos verbatim |
| **W4** | Audio + screen-mode verbs |
| **W5** | Event-loop iteration to E2 (blocking) once the basics are solid |

Estimated complexity: W1 + W2 in one session, W3 in a second,
W4–W5 separate sessions of their own. Lock in the language shape
first so we don't rebuild it after W2.
