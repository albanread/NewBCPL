//! NewBCPL runtime: BCPL standard library, GC, iGui host.
//!
//! Currently exposes:
//!
//! - [`gc`] — precise mark-sweep tracing collector ported from NewCP's
//!   `gc.rs`. See `docs/manifesto.md` §5.
//! - [`igui`] (Windows only) — integrated GUI: an MDI frame backed by
//!   Direct2D + DirectWrite, plus the `bedit` fail-safe BCPL editor.
//!   Borrowed from NewCormanLisp's `igui` slice, which itself
//!   descends from NewCP's. See `igui::run` to start the GUI thread,
//!   `igui::install_checker` to wire up a compile-check closure, and
//!   `igui::cp_exports` for the language-neutral C-ABI surface.
//!
//! Forthcoming:
//!
//! - the BCPL builtin surface — `WRITES`, `WRITEF`, `WRITEN`, `WRITEC`,
//!   `FREEVEC`, `FLOAT`, `TRUNC`, etc. (see
//!   `reference/documentation/BCPL Runtime.md`).
//! - lists — doubly-anchored singly-linked, freelisted, GC-traced via
//!   per-variant TypeDescs (see `docs/manifesto.md` §5 and
//!   `reference/runtime/ListDataTypes.h` for the existing layout).
//! - BCPL-facing iGui shims (the analogue of CL's `lisp_shims`) once
//!   the BCPL JIT lands.

pub mod builtins;
pub mod gc;

#[cfg(windows)]
pub mod igui;

#[cfg(windows)]
pub mod igui_builtins;

/// BCPL `Sound_*` / `Music_*` runtime — game-focused SFX synth and
/// ABC → MIDI playback, backed by NewAudio. Slot bookkeeping and
/// synthesis work on every target; live waveOut / midiOut playback
/// is Windows-only. Mirrors the surface NewFB ships in
/// `newfb-runtime/src/audio.rs`.
pub mod audio;

/// `BRK` statement runtime — the signal-safe state dumper invoked
/// when user code reaches a `BRK` debugger breakpoint. Kept in its
/// own module so the Win32 import surface stays out of the much
/// hotter `builtins.rs`.
pub mod brk;
