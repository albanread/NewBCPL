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

use crate::igui::{channels, cp_exports, text_view, window};

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

/// `iGui_NextEventFor(target_child_id, *kind, *child, *time, *p1,
///                    *p2, *p3, *p4, timeout_ms) -> 1 / 0`. Like
/// `iGui_NextEvent` but waits only for events targeting
/// `target_child_id` or "global" events (`FrameClose`,
/// `ThemeChange`, `Menu`). Events for other windows are stashed for
/// future consumers — they survive into the next consumer's view
/// instead of being silently dropped at the BCPL level.
#[unsafe(no_mangle)]
#[allow(clippy::too_many_arguments)]
pub unsafe extern "C" fn iGui_NextEventFor(
    target_child: i64,
    out_kind: *mut i64,
    out_child: *mut i64,
    out_time: *mut i64,
    out_p1: *mut i64,
    out_p2: *mut i64,
    out_p3: *mut i64,
    out_p4: *mut i64,
    timeout_ms: i64,
) -> i64 {
    let Some(ev) = channels::next_event_for(target_child, timeout_ms) else {
        return 0;
    };
    cp_exports::write_event(
        ev, out_kind, out_child, out_time, out_p1, out_p2, out_p3, out_p4,
    );
    1
}

/// `iGui_DiscardStashedEvents()` — drop every event currently
/// parked in the event stash by prior `iGui_NextEventFor` calls.
/// Useful when the program transitions modes (closes one window,
/// opens another) and doesn't want backed-up events from the old
/// state. Does not touch the channel itself — events still arriving
/// continue to flow.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn iGui_DiscardStashedEvents() -> i64 {
    channels::discard_stashed_events();
    0
}

/// `iGui_FilterOnWindow(child_id)` — add a window id to the
/// persistent event filter. After the first call, `iGui_NextEvent`
/// returns only events for windows in the filter (plus "global"
/// events like FrameClose). The filter is additive; call once per
/// window the program cares about, typically right after
/// `iGui_OpenChild` / `iGui_OpenText`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn iGui_FilterOnWindow(child_id: i64) -> i64 {
    channels::filter_on_window(child_id);
    0
}

/// `iGui_UnfilterWindow(child_id)` — remove a window id from the
/// persistent filter. Use when a window closes so its (now-stale)
/// id doesn't keep gating which events come through.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn iGui_UnfilterWindow(child_id: i64) -> i64 {
    channels::unfilter_window(child_id);
    0
}

/// `iGui_ClearFilter()` — empty the persistent filter so subsequent
/// `iGui_NextEvent` calls return every event. The driver also calls
/// this automatically at the start of each JIT-run so one
/// program's filter doesn't bleed into the next.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn iGui_ClearFilter() -> i64 {
    channels::clear_filter();
    0
}

// ─── Text-pane (terminal-grid) builtins ─────────────────────────────
//
// A text pane is a monospaced character-cell MDI child with a
// software grid + cursor — distinct from the graphics-batch
// `OpenChild` surface (which is for shapes / text rendered via
// Direct2D) and distinct from the log view (which is a scroll-back
// for diagnostic output).
//
// User programs use it as a console: `Open` returns a child id,
// `WriteStr` / `WriteChar` / `Newline` append at the caret,
// `SetCursor` / `Clear` / `ScrollUp` reposition or erase, `SetPen`
// changes foreground / background colour (packed RGBA u32).

/// Open a text pane MDI child, write its id to `*out_id`, return 1.
/// `title` is NUL-terminated.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn iGui_OpenText(title: *const u8, out_id: *mut i64) -> i64 {
    if title.is_null() || out_id.is_null() {
        return 0;
    }
    let title_str = unsafe { read_cstr(title) };
    match window::open_text_child(&title_str) {
        Some(id) => {
            unsafe {
                *out_id = id;
            }
            1
        }
        None => 0,
    }
}

/// `iGui_TextWriteStr(id, text)` — append `text` (NUL-terminated,
/// UTF-8) at the current caret. Each LF advances to a fresh row;
/// TAB rounds to the next 8-column stop.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn iGui_TextWriteStr(id: i64, text: *const u8) -> i64 {
    if text.is_null() {
        return 0;
    }
    let s = unsafe { read_cstr(text) };
    text_view::write_str(id, &s) as i64
}

/// `iGui_TextWriteChar(id, codepoint)` — append one Unicode
/// codepoint (low 32 bits) at the caret.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn iGui_TextWriteChar(id: i64, codepoint: i64) -> i64 {
    text_view::write_char(id, (codepoint & 0xFFFF_FFFF) as u32) as i64
}

/// `iGui_TextNewline(id)` — carriage-return + line-feed at the caret.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn iGui_TextNewline(id: i64) -> i64 {
    text_view::newline_cmd(id) as i64
}

/// `iGui_TextSetCursor(id, row, col)` — move the caret. Rows and
/// columns are 0-based.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn iGui_TextSetCursor(id: i64, row: i64, col: i64) -> i64 {
    text_view::set_cursor(id, row as u32, col as u32) as i64
}

/// `iGui_TextClear(id)` — clear the entire grid, reset caret to (0, 0).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn iGui_TextClear(id: i64) -> i64 {
    text_view::clear_all_cmd(id) as i64
}

/// `iGui_TextClearEol(id)` — clear from caret to end of line.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn iGui_TextClearEol(id: i64) -> i64 {
    text_view::clear_to_eol_cmd(id) as i64
}

/// `iGui_TextClearEos(id)` — clear from caret to end of screen.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn iGui_TextClearEos(id: i64) -> i64 {
    text_view::clear_to_eos_cmd(id) as i64
}

/// `iGui_TextScrollUp(id, n)` — scroll the grid up by `n` rows,
/// inserting blank rows at the bottom.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn iGui_TextScrollUp(id: i64, n: i64) -> i64 {
    text_view::scroll_up_cmd(id, n.max(0) as u32) as i64
}

/// `iGui_TextSetPen(id, fg, bg)` — set foreground / background
/// colours for subsequent writes. Each colour is a packed
/// `0xAARRGGBB` u32 (low 32 bits of the BCPL i64 argument).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn iGui_TextSetPen(id: i64, fg: i64, bg: i64) -> i64 {
    text_view::set_pen(
        id,
        (fg & 0xFFFF_FFFF) as u32,
        (bg & 0xFFFF_FFFF) as u32,
    ) as i64
}

/// `iGui_TextResetPen(id)` — restore the default fg / bg.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn iGui_TextResetPen(id: i64) -> i64 {
    text_view::reset_pen(id) as i64
}

/// `iGui_TextShowCaret(id, visible)` — `visible` non-zero shows the
/// blinking caret block; zero hides it.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn iGui_TextShowCaret(id: i64, visible: i64) -> i64 {
    text_view::set_caret_visible(id, visible != 0) as i64
}

/// Read a NUL-terminated UTF-8 / ASCII byte sequence into an owned
/// String. Used by the text-pane shims that take BCPL string args.
unsafe fn read_cstr(p: *const u8) -> String {
    if p.is_null() {
        return String::new();
    }
    let mut len = 0usize;
    while unsafe { *p.add(len) } != 0 {
        len += 1;
    }
    let bytes = unsafe { std::slice::from_raw_parts(p, len) };
    String::from_utf8_lossy(bytes).into_owned()
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
