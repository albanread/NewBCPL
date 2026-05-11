//! BCPL-callable iGui wrappers.
//!
//! [`crate::igui::cp_exports`] exposes the iGui surface under dotted
//! symbol names (`iGui.OpenChild`, `iGui.EmitFillRect`, …) for
//! compatibility with the NewCP / NewCormanLisp link conventions.
//! BCPL identifiers can't contain `.`, so this module re-exports
//! the surface under underscore-named C-ABI symbols
//! (`iGui_OpenChild`, `iGui_FillRect`, …) the JIT can resolve.
//!
//! All function signatures are tuned to BCPL's natural ABI:
//!
//! - i64 for IDs and counts
//! - f64 for coordinates and colour components
//! - `*const u8` for strings, `*mut i64` for VAR outputs
//!
//! For each wrapper, the matching declaration must exist in
//! `newbcpl-llvm::emit::declare_extern` so the JIT emits the right
//! LLVM types and the Win64 calling convention passes f64 args in
//! XMM registers as the Rust functions expect.

#![cfg(windows)]
#![allow(non_snake_case)]

use crate::igui::cp_exports;

/// `iGui_OpenChild(title, *out_id) -> 1 on success`. `title` is a
/// NUL-terminated UTF-8 string; cp_exports scans for the NUL so the
/// length argument we pass it is irrelevant.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn iGui_OpenChild(title: *const u8, out_id: *mut i64) -> i64 {
    cp_exports::igui_open_child(title, 0, out_id) as i64
}

/// `iGui_CloseChild(id) -> 1 on success`. Looks the window up in
/// the registry and tears it down on the GUI thread.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn iGui_CloseChild(id: i64) -> i64 {
    cp_exports::igui_close_child(id) as i64
}

/// `iGui_SetTitle(id, title)`. Same NUL-terminated string convention.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn iGui_SetTitle(id: i64, title: *const u8) -> i64 {
    cp_exports::igui_set_title(id, title, 0);
    0
}

/// `iGui_BeginBatch(id)` — start collecting draw commands for the
/// given window. Subsequent `Emit*` calls go into this batch until
/// `iGui_SubmitBatch` flushes it onto the GUI thread.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn iGui_BeginBatch(id: i64) -> i64 {
    cp_exports::igui_begin_batch(id);
    0
}

/// `iGui_SubmitBatch() -> 1 on success`. Hands the open batch off
/// to the GUI thread for execution against the window's render
/// target.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn iGui_SubmitBatch() -> i64 {
    cp_exports::igui_submit_batch() as i64
}

/// `iGui_Clear(r, g, b, a)` — fill the entire window with the given
/// colour. Colour components are 0.0..=1.0.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn iGui_Clear(r: f64, g: f64, b: f64, a: f64) -> i64 {
    cp_exports::igui_emit_clear(r, g, b, a);
    0
}

/// `iGui_FillRect(x0, y0, x1, y1, r, g, b, a)` — fill the rectangle
/// with the given colour. Square corners (corner radius 0).
#[unsafe(no_mangle)]
#[allow(clippy::too_many_arguments)]
pub unsafe extern "C" fn iGui_FillRect(
    x0: f64,
    y0: f64,
    x1: f64,
    y1: f64,
    r: f64,
    g: f64,
    b: f64,
    a: f64,
) -> i64 {
    cp_exports::igui_emit_fill_rect(x0, y0, x1, y1, 0.0, r, g, b, a);
    0
}

/// `iGui_StrokeRect(x0, y0, x1, y1, thickness, r, g, b, a)` — draw
/// the outline of the rectangle. `thickness` is the full pen width
/// (we halve it for cp_exports' half-thickness convention).
#[unsafe(no_mangle)]
#[allow(clippy::too_many_arguments)]
pub unsafe extern "C" fn iGui_StrokeRect(
    x0: f64,
    y0: f64,
    x1: f64,
    y1: f64,
    thickness: f64,
    r: f64,
    g: f64,
    b: f64,
    a: f64,
) -> i64 {
    cp_exports::igui_emit_stroke_rect(x0, y0, x1, y1, 0.0, thickness * 0.5, r, g, b, a);
    0
}

/// `iGui_FillCircle(cx, cy, radius, r, g, b, a)` — solid disc.
#[unsafe(no_mangle)]
#[allow(clippy::too_many_arguments)]
pub unsafe extern "C" fn iGui_FillCircle(
    cx: f64,
    cy: f64,
    radius: f64,
    r: f64,
    g: f64,
    b: f64,
    a: f64,
) -> i64 {
    cp_exports::igui_emit_fill_circle(cx, cy, radius, r, g, b, a);
    0
}

/// `iGui_DrawLine(x0, y0, x1, y1, thickness, r, g, b, a)`.
#[unsafe(no_mangle)]
#[allow(clippy::too_many_arguments)]
pub unsafe extern "C" fn iGui_DrawLine(
    x0: f64,
    y0: f64,
    x1: f64,
    y1: f64,
    thickness: f64,
    r: f64,
    g: f64,
    b: f64,
    a: f64,
) -> i64 {
    cp_exports::igui_emit_draw_line(x0, y0, x1, y1, thickness * 0.5, r, g, b, a);
    0
}

/// `iGui_NextEvent(out_kind, out_child, out_time, out_p1, out_p2,
///                out_p3, out_p4, timeout_ms) -> 1 on event, 0 on
/// timeout`. The seven `out_*` pointers receive a packed flattening
/// of whatever `IGuiEvent` variant arrived; field semantics are
/// documented next to `iGui.NextEvent` in `cp_exports.rs` (one row
/// per kind). `timeout_ms < 0` blocks indefinitely.
///
/// Kind constants (see `igui::channels::kind`):
///
/// | kind         | value |
/// |--------------|-------|
/// | NONE         | 0     |
/// | KEY          | 1     |
/// | CHAR         | 2     |
/// | MOUSE        | 3     |
/// | FOCUS        | 4     |
/// | RESIZE       | 5     |
/// | PAINT        | 6     |
/// | CLOSE        | 7     |
/// | FRAME_CLOSE  | 8     |
/// | MENU         | 9     |
/// | THEME_CHANGE | 10    |
/// | DPI_CHANGE   | 11    |
/// | SURFACE_REPLY| 12    |
/// | TICK         | 13    |
#[unsafe(no_mangle)]
#[allow(clippy::too_many_arguments)]
pub unsafe extern "C" fn iGui_NextEvent(
    out_kind: *mut i64,
    out_child: *mut i64,
    out_time: *mut i64,
    out_p1: *mut i64,
    out_p2: *mut i64,
    out_p3: *mut i64,
    out_p4: *mut i64,
    timeout_ms: i64,
) -> i64 {
    cp_exports::igui_next_event(
        out_kind, out_child, out_time, out_p1, out_p2, out_p3, out_p4, timeout_ms,
    ) as i64
}

/// `iGui_Quit()` — programmatically close the iGui frame. Posts
/// `WM_CLOSE` to the frame window; the GUI thread tears down on its
/// own message loop's terms. The JIT'd caller usually wants to
/// return from `START` right after.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn iGui_Quit() -> i64 {
    cp_exports::igui_quit();
    0
}

/// `iGui_DrawText(text, x, y, size, r, g, b, a)` — draw `text` with
/// a sensible default font (Segoe UI, regular weight, no wrapping).
/// Strings are NUL-terminated.
#[unsafe(no_mangle)]
#[allow(clippy::too_many_arguments)]
pub unsafe extern "C" fn iGui_DrawText(
    text: *const u8,
    x: f64,
    y: f64,
    size: f64,
    r: f64,
    g: f64,
    b: f64,
    a: f64,
) -> i64 {
    cp_exports::igui_emit_draw_text_run(
        text, 0,            // text + ignored len
        x, y,               // origin
        size,               // font size
        std::ptr::null(), 0, // family (NULL → "Segoe UI") + ignored len
        400,                // weight: regular
        0,                  // style: normal
        0,                  // stretch: normal
        std::ptr::null(), 0, // locale (NULL → "en-us") + ignored len
        0.0,                // max_width (0 → no wrap)
        0,                  // alignment: leading
        0,                  // trimming: none
        r, g, b, a,
    );
    0
}
