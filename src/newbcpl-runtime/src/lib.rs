//! NewBCPL runtime: BCPL standard library, GC, iGui host.
//!
//! Currently exposes:
//!
//! - [`gc`] — precise mark-sweep tracing collector ported from NewCP's
//!   `gc.rs`. See `docs/manifesto.md` §5.
//!
//! Forthcoming:
//!
//! - the BCPL builtin surface — `WRITES`, `WRITEF`, `WRITEN`, `WRITEC`,
//!   `FREEVEC`, `FLOAT`, `TRUNC`, etc. (see
//!   `reference/documentation/BCPL Runtime.md`).
//! - lists — doubly-anchored singly-linked, freelisted, GC-traced via
//!   per-variant TypeDescs (see `docs/manifesto.md` §5 and
//!   `reference/runtime/ListDataTypes.h` for the existing layout).
//! - `iGui` integrated GUI on `x86_64-pc-windows-msvc`, backed by Direct2D
//!   + DirectWrite, mirroring NewCP's `iGui` slice.

pub mod gc;
