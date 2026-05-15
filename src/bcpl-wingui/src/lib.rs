//! BCPL-side bindings to the wingui retro-graphics framework.
//!
//! This crate provides `extern "C-unwind"` shims that JIT-emitted
//! BCPL code calls into through the runtime's builtin table. Each
//! shim is the Rust side of a class method declared on the BCPL
//! side (`Window`, `App`, ...) or a procedural verb in the FB-
//! compat shim. Strings come across as NUL-terminated UTF-8
//! pointers (matching how `WRITES` and friends already pass them);
//! IDs are 64-bit words downcast to `i32` for the underlying
//! wingui API.
//!
//! Architecture stack (Rust side):
//!
//! ```text
//!   bcpl-wingui      <-- THIS CRATE: BCPL ABI adapter
//!     |
//!     v
//!   wingui-rs        <-- language-neutral typed wrappers
//!     |
//!     v
//!   wingui.dll       <-- Win32 + D3D renderer (E:\multiwingui)
//! ```
//!
//! Ported from `E:/NewFB/src/newfb-wingui/src/lib.rs` (the
//! sister-compiler reference) — same registry / event-queue /
//! hosting pattern, simplified for BCPL: single window, no
//! GC-mutator marking (BCPL's GC story differs), polled event
//! queue surfacing button IDs as i64 values (no string events).

#![cfg_attr(not(target_os = "windows"), allow(dead_code))]

#[cfg(target_os = "windows")]
use std::ffi::{CStr, CString};
#[cfg(target_os = "windows")]
use std::sync::Mutex;

// ─── Registry ─────────────────────────────────────────────────────
//
// In-memory model of the (single) window the BCPL program is
// building up. `window_define` resets the registry; each control-
// adder (button / label) appends. `window_run` snapshots the
// registry into a JSON spec and hands it to
// `super_terminal_run_hosted_app`.

#[cfg(target_os = "windows")]
#[derive(Clone)]
enum ChildNode {
    /// `Window.button(btn_id, label)` — a clickable button. When
    /// clicked, the framework fires an event whose name we set to
    /// `"btn_<id>"`; the event-queue handler decodes that back to
    /// `btn_id` so BCPL sees the numeric id the program registered.
    Button { id: i32, label: String },
    /// `Window.label(text)` — a static text node.
    Label { id: i32, text: String },
    /// `Window.canvas(id, w, h)` — an RGBA drawing surface. The
    /// spec emits this as an `"rgba-pane"` node with the requested
    /// dimensions; the runtime allocates a backing texture and
    /// resolves the node-id back to a `SuperTerminalPaneId` we use
    /// for vector-draw calls.
    Canvas { id: u32, width: u32, height: u32 },
}

#[cfg(target_os = "windows")]
struct MainWindow {
    title: String,
    children: Vec<ChildNode>,
}

/// One declared canvas pane plus its resolved runtime id.
/// Populated when `Window.canvas(id, w, h)` is called; the
/// `runtime_pane_id` slot stays `None` until `resolve_canvas_panes`
/// asks the host for the matching `SuperTerminalPaneId`. After
/// resolution, drawing primitives can be dispatched directly.
#[cfg(target_os = "windows")]
struct CanvasPaneEntry {
    pane_id: u32,
    width: u32,
    height: u32,
    runtime_pane_id: Option<wingui_rs::super_terminal::SuperTerminalPaneId>,
}

/// Drawing operations queued before `run()` captures the UI ctx.
/// Drained from `on_setup` after the host has resolved each pane
/// to a runtime id. Lets BCPL programs describe a chart layout
/// linearly before the blocking `run()` call.
#[cfg(target_os = "windows")]
#[derive(Clone)]
enum PendingCanvasDraw {
    Clear {
        pane_id: u32,
        color: [f32; 4],
    },
    Primitive {
        pane_id: u32,
        primitive: wingui_rs::super_terminal::WinguiVectorPrimitive,
    },
}

#[cfg(target_os = "windows")]
struct Registry {
    main: Option<MainWindow>,
    canvas_panes: Vec<CanvasPaneEntry>,
    pending_canvas_draws: Vec<PendingCanvasDraw>,
}

#[cfg(target_os = "windows")]
impl Registry {
    const fn new() -> Self {
        Self {
            main: None,
            canvas_panes: Vec::new(),
            pending_canvas_draws: Vec::new(),
        }
    }
}

#[cfg(target_os = "windows")]
static REGISTRY: Mutex<Registry> = Mutex::new(Registry::new());

/// Numeric button-click queue. Each native UI event fires with an
/// `"event"` JSON field whose value follows the `"btn_<id>"`
/// convention this crate sets up at button-definition time. The
/// `on_event` callback parses the id and pushes onto this queue;
/// BCPL drains via `bcpl_wingui_poll_event()`.
#[cfg(target_os = "windows")]
static EVENT_QUEUE: Mutex<std::collections::VecDeque<i32>> =
    Mutex::new(std::collections::VecDeque::new());

/// `SuperTerminalClientContext` captured during `on_setup` /
/// `on_event`. Required for live updates to the running window
/// (publish_ui_json). Reset to `None` before each `run` and after
/// the run returns.
#[cfg(target_os = "windows")]
struct SendableCtx(*mut wingui_rs::super_terminal::SuperTerminalClientContext);

#[cfg(target_os = "windows")]
unsafe impl Send for SendableCtx {}

#[cfg(target_os = "windows")]
static UI_CTX: Mutex<Option<SendableCtx>> = Mutex::new(None);

/// Set of button ids whose click should close the window
/// programmatically. Populated from BCPL via
/// `bcpl_wingui_close_on(id)` before `run()`. Without this,
/// clicking a "Quit" button only queues an event — the window
/// itself stays open until the user closes it via the title bar.
/// Standard editor / app convention is "Quit closes immediately",
/// so most demos register the button id of their Quit button.
#[cfg(target_os = "windows")]
static CLOSE_TRIGGERS: Mutex<Vec<i32>> = Mutex::new(Vec::new());

// ─── Small helpers ────────────────────────────────────────────────

#[cfg(target_os = "windows")]
fn read_cstr_or_empty(p: *const u8) -> String {
    if p.is_null() {
        return String::new();
    }
    let cs = unsafe { CStr::from_ptr(p as *const i8) };
    cs.to_string_lossy().into_owned()
}

/// Extract the `"event"` field's string value from a wingui-emitted
/// JSON payload. Hand-rolled rather than going through serde_json
/// to keep the callback path allocation-light. Returns `None` for
/// any parse failure — the callback then drops the event silently.
#[cfg(target_os = "windows")]
fn extract_event_name(payload: &str) -> Option<String> {
    let needle = "\"event\"";
    let after_key = payload.find(needle)? + needle.len();
    let rest = payload[after_key..].trim_start_matches([' ', '\t', ':', '"']);
    let mut value = String::new();
    let mut prev_escape = false;
    for c in rest.chars() {
        match c {
            '"' if !prev_escape => return Some(value),
            '\\' if !prev_escape => prev_escape = true,
            _ => {
                prev_escape = false;
                value.push(c);
            }
        }
    }
    None
}

#[cfg(target_os = "windows")]
fn parse_button_id_from_event(name: &str) -> Option<i32> {
    name.strip_prefix("btn_").and_then(|s| s.parse().ok())
}

/// `D3DCompileFromFile` insists on relative paths anchored at cwd —
/// see the comment in NewFB's `anchor_cwd_for_shaders`. We chdir to
/// the exe directory (where build.rs stages the `shaders/` tree)
/// before calling `run_hosted_app`, then never restore (run blocks
/// for the program's lifetime).
#[cfg(target_os = "windows")]
fn anchor_cwd_for_shaders() {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let candidate = dir.join("shaders").join("text_grid.hlsl");
            if candidate.is_file() {
                let _ = std::env::set_current_dir(dir);
            }
        }
    }
}

// ─── Spec assembly ────────────────────────────────────────────────

#[cfg(target_os = "windows")]
fn build_spec(main: &MainWindow) -> wingui_rs::spec::SpecNode {
    use wingui_rs::spec::SpecNode;
    let kids: Vec<SpecNode> = main
        .children
        .iter()
        .map(|c| match c {
            ChildNode::Button { id, label } => SpecNode::new("button")
                .id(format!("btn_{id}"))
                .text(label.clone())
                .event(format!("btn_{id}")),
            ChildNode::Label { id, text } => SpecNode::new("text")
                .id(format!("lbl_{id}"))
                .text(text.clone()),
            ChildNode::Canvas { id, width, height } => SpecNode::new("rgba-pane")
                .id(format!("canvas_pane_{id}"))
                .width(*width as i64)
                .height(*height as i64),
        })
        .collect();
    let body = SpecNode::new("stack")
        .id("root")
        .gap(8)
        .padding(16)
        .children(kids);
    SpecNode::new("window")
        .id("main")
        .title(main.title.clone())
        .body(body)
}

// ─── BCPL-facing FFI shims ────────────────────────────────────────

/// `wingui_version_packed()` — returns the wingui DLL's version
/// packed into one 64-bit word: `(major << 32) | (minor << 16) |
/// patch`. Returns 0 if the version probe fails.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn bcpl_wingui_version_packed() -> i64 {
    #[cfg(target_os = "windows")]
    {
        let v = wingui_rs::version();
        let major = (v.major as i64 & 0xFFFF) << 32;
        let minor = (v.minor as i64 & 0xFFFF) << 16;
        let patch = v.patch as i64 & 0xFFFF;
        major | minor | patch
    }
    #[cfg(not(target_os = "windows"))]
    {
        0
    }
}

/// `wingui_is_available()` — 1 when the DLL is loaded and reports
/// ready, 0 otherwise.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn bcpl_wingui_is_available() -> i64 {
    #[cfg(target_os = "windows")]
    {
        if wingui_rs::native_available() {
            1
        } else {
            0
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        0
    }
}

/// `Window.CREATE(id, title)` — reset the in-progress window spec
/// to a fresh definition with the given title. `id` is currently
/// ignored (single-window W2 scope); multi-window comes when
/// super_terminal_create_window's binding lands.
///
/// # Safety
/// `title_ptr` must be null or a NUL-terminated UTF-8 byte
/// sequence valid for the duration of the call.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn bcpl_wingui_window_define(
    _id: i64,
    title_ptr: *const u8,
) {
    #[cfg(target_os = "windows")]
    {
        let title = read_cstr_or_empty(title_ptr);
        if let Ok(mut g) = REGISTRY.lock() {
            g.main = Some(MainWindow {
                title,
                children: Vec::new(),
            });
            // Reset canvas state — the previous run's panes and
            // pending draws don't apply to the new window.
            g.canvas_panes.clear();
            g.pending_canvas_draws.clear();
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = (_id, title_ptr);
    }
}

/// `Window.button(btn_id, label)` — append a button control.
/// `btn_id` is the BCPL caller's chosen numeric id; clicks on this
/// button surface through `poll_event()` as exactly this value.
///
/// # Safety
/// `label_ptr` must be null or a NUL-terminated UTF-8 byte
/// sequence.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn bcpl_wingui_window_button(
    btn_id: i64,
    label_ptr: *const u8,
) {
    #[cfg(target_os = "windows")]
    {
        let label = read_cstr_or_empty(label_ptr);
        if let Ok(mut g) = REGISTRY.lock() {
            if let Some(main) = g.main.as_mut() {
                main.children.push(ChildNode::Button {
                    id: btn_id as i32,
                    label,
                });
            }
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = (btn_id, label_ptr);
    }
}

/// `Window.label(lbl_id, text)` — append a static-text control.
///
/// # Safety
/// `text_ptr` must be null or a NUL-terminated UTF-8 byte sequence.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn bcpl_wingui_window_label(
    lbl_id: i64,
    text_ptr: *const u8,
) {
    #[cfg(target_os = "windows")]
    {
        let text = read_cstr_or_empty(text_ptr);
        if let Ok(mut g) = REGISTRY.lock() {
            if let Some(main) = g.main.as_mut() {
                main.children.push(ChildNode::Label {
                    id: lbl_id as i32,
                    text,
                });
            }
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = (lbl_id, text_ptr);
    }
}

/// `App.run()` / `Window.show()` — block until the window closes.
/// Builds the JSON spec from the registry, calls
/// `super_terminal_run_hosted_app`, and returns when the user
/// closes the window or `bcpl_wingui_close()` is called.
///
/// Returns:
///   *  0 on clean shutdown,
///   * -1 if the registry has no window defined,
///   * -2 if the host call itself fails.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn bcpl_wingui_window_run() -> i64 {
    #[cfg(target_os = "windows")]
    {
        run_window_now() as i64
    }
    #[cfg(not(target_os = "windows"))]
    {
        -1
    }
}

/// `Window.close()` — programmatically request the running window
/// stop. Equivalent to the user clicking the close button. The
/// `window_run` call returns shortly after. Safe to call from any
/// thread (the request goes through super_terminal's own queue).
#[unsafe(no_mangle)]
pub extern "C-unwind" fn bcpl_wingui_window_close() {
    #[cfg(target_os = "windows")]
    {
        let ctx_ptr = {
            let g = match UI_CTX.lock() {
                Ok(g) => g,
                Err(_) => return,
            };
            match g.as_ref() {
                Some(c) => c.0,
                None => return,
            }
        };
        if !ctx_ptr.is_null() {
            unsafe {
                wingui_rs::super_terminal::super_terminal_request_stop(ctx_ptr, 0)
            };
        }
    }
}

/// `App.close_on(btn_id)` — register a button id so that clicking
/// it ALSO requests the window stop. Without registration,
/// clicking just queues the event and the user has to close via
/// the title bar; with registration, the click both queues the
/// event AND closes the window. Standard "Quit button closes
/// immediately" UX. Idempotent — registering the same id twice
/// is harmless.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn bcpl_wingui_close_on(btn_id: i64) {
    #[cfg(target_os = "windows")]
    {
        let id = btn_id as i32;
        if let Ok(mut g) = CLOSE_TRIGGERS.lock() {
            if !g.contains(&id) {
                g.push(id);
            }
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = btn_id;
    }
}

/// `App.poll_event()` — return the id of the next pending button
/// click, or 0 if the queue is empty. (Button ids are positive
/// by convention; 0 is the sentinel for "no event".) Drains one
/// event per call so a loop polling until 0 covers everything
/// that fired since the last drain.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn bcpl_wingui_poll_event() -> i64 {
    #[cfg(target_os = "windows")]
    {
        if let Ok(mut q) = EVENT_QUEUE.lock() {
            return q.pop_front().map(|id| id as i64).unwrap_or(0);
        }
        0
    }
    #[cfg(not(target_os = "windows"))]
    {
        0
    }
}

// ─── Canvas drawing — RGBA surface ────────────────────────────────
//
// A canvas pane is declared by `Window.canvas(id, w, h)` (or the
// `WIN_CANVAS` procedural alias). The spec emits an `"rgba-pane"`
// node; after `super_terminal_run_hosted_app` starts, `on_setup`
// asks the host for the matching `SuperTerminalPaneId` via
// `super_terminal_resolve_pane_id_utf8` and parks it on the
// registry entry. Drawing primitives (CLEAR / FILL / FRAME / LINE
// / CIRCLE) are queued onto `pending_canvas_draws` while the
// runtime ctx is missing, then drained from `on_setup` after the
// resolve pass.
//
// Coordinates are integer pixels (BCPL passes i64; we cast to
// f32 inside the shim). Colours are 0-255 byte components (FB
// convention); we normalise to 0.0-1.0 f32 for wingui's vector
// primitives. Alpha defaults to fully opaque — programs that want
// transparency call the `*_alpha` variants (TBD).

#[cfg(target_os = "windows")]
fn rgba_bytes_to_f32(r: i64, g: i64, b: i64) -> [f32; 4] {
    [
        (r.clamp(0, 255) as f32) / 255.0,
        (g.clamp(0, 255) as f32) / 255.0,
        (b.clamp(0, 255) as f32) / 255.0,
        1.0,
    ]
}

#[cfg(target_os = "windows")]
fn lookup_canvas_pane(
    pane_id: u32,
) -> Option<(wingui_rs::super_terminal::SuperTerminalPaneId, u32, u32)> {
    let g = REGISTRY.lock().ok()?;
    let entry = g.canvas_panes.iter().find(|p| p.pane_id == pane_id)?;
    let rt = entry.runtime_pane_id?;
    Some((rt, entry.width, entry.height))
}

#[cfg(target_os = "windows")]
fn current_ui_ctx() -> Option<*mut wingui_rs::super_terminal::SuperTerminalClientContext> {
    let g = UI_CTX.lock().ok()?;
    Some(g.as_ref()?.0)
}

/// Submit a single vector primitive to a canvas pane. If the
/// runtime ctx isn't live yet (pre-run) or the pane hasn't
/// resolved, queue the primitive onto `pending_canvas_draws` for
/// the on_setup drain pass.
#[cfg(target_os = "windows")]
fn submit_canvas_primitive(
    pane_id: u32,
    prim: wingui_rs::super_terminal::WinguiVectorPrimitive,
) {
    let ctx_opt = current_ui_ctx();
    let pane_opt = lookup_canvas_pane(pane_id);
    let ctx_alive = ctx_opt.map(|c| !c.is_null()).unwrap_or(false);
    if !ctx_alive || pane_opt.is_none() {
        if let Ok(mut g) = REGISTRY.lock() {
            g.pending_canvas_draws.push(PendingCanvasDraw::Primitive {
                pane_id,
                primitive: prim,
            });
        }
        return;
    }
    let ctx = ctx_opt.unwrap();
    let rt_pane = pane_opt.unwrap().0;
    let _ = unsafe {
        wingui_rs::super_terminal::super_terminal_vector_draw(
            ctx,
            rt_pane,
            0,
            wingui_rs::super_terminal::rgba_content_buffer::PERSISTENT,
            wingui_rs::super_terminal::rgba_blit::ALPHA_OVER,
            0,
            std::ptr::null::<f32>(),
            &prim,
            1,
        )
    };
}

/// Same shape as `submit_canvas_primitive` but for a CLEAR — the
/// only "primitive" that wipes the surface rather than compositing
/// on top. Goes through `super_terminal_vector_draw` with the
/// `clear_before=1` flag and an OPAQUE blit so the colour fully
/// replaces what was there.
#[cfg(target_os = "windows")]
fn submit_canvas_clear(pane_id: u32, color: [f32; 4]) {
    let ctx_opt = current_ui_ctx();
    let pane_opt = lookup_canvas_pane(pane_id);
    let ctx_alive = ctx_opt.map(|c| !c.is_null()).unwrap_or(false);
    if !ctx_alive || pane_opt.is_none() {
        if let Ok(mut g) = REGISTRY.lock() {
            g.pending_canvas_draws
                .push(PendingCanvasDraw::Clear { pane_id, color });
        }
        return;
    }
    let ctx = ctx_opt.unwrap();
    let rt_pane = pane_opt.unwrap().0;
    let _ = unsafe {
        wingui_rs::super_terminal::super_terminal_vector_draw(
            ctx,
            rt_pane,
            0,
            wingui_rs::super_terminal::rgba_content_buffer::PERSISTENT,
            wingui_rs::super_terminal::rgba_blit::OPAQUE,
            1,
            color.as_ptr(),
            std::ptr::null(),
            0,
        )
    };
}

/// Walk the declared canvas panes and resolve each one's runtime
/// pane id via `super_terminal_resolve_pane_id_utf8`. Called from
/// on_setup immediately after the framework signals it's ready to
/// accept commands. After this returns, primitives can dispatch
/// directly via `super_terminal_vector_draw`.
#[cfg(target_os = "windows")]
fn resolve_canvas_panes(
    ctx: *mut wingui_rs::super_terminal::SuperTerminalClientContext,
) {
    use wingui_rs::super_terminal::{
        super_terminal_resolve_pane_id_utf8, SuperTerminalPaneId,
    };
    let pending: Vec<u32> = match REGISTRY.lock() {
        Ok(g) => g
            .canvas_panes
            .iter()
            .filter(|p| p.runtime_pane_id.is_none())
            .map(|p| p.pane_id)
            .collect(),
        Err(_) => return,
    };
    let mut resolved: Vec<(u32, SuperTerminalPaneId)> = Vec::new();
    for pid in pending {
        let node_id = format!("canvas_pane_{}", pid);
        let Ok(cstr) = CString::new(node_id) else {
            continue;
        };
        let mut out = SuperTerminalPaneId::default();
        let ok = unsafe {
            super_terminal_resolve_pane_id_utf8(ctx, cstr.as_ptr(), &mut out)
        };
        if ok != 0 && out.value != 0 {
            resolved.push((pid, out));
        }
    }
    if let Ok(mut g) = REGISTRY.lock() {
        for (pid, rt) in resolved {
            if let Some(p) = g.canvas_panes.iter_mut().find(|p| p.pane_id == pid) {
                p.runtime_pane_id = Some(rt);
            }
        }
    }
}

/// Drain queued canvas draws now the ctx + runtime pane ids are
/// live. Ops whose pane id never resolved (id typo etc.) are
/// silently dropped.
#[cfg(target_os = "windows")]
fn drain_pending_canvas_draws(
    ctx: *mut wingui_rs::super_terminal::SuperTerminalClientContext,
) {
    if ctx.is_null() {
        return;
    }
    let pending: Vec<PendingCanvasDraw> = match REGISTRY.lock() {
        Ok(mut g) => std::mem::take(&mut g.pending_canvas_draws),
        Err(_) => return,
    };
    for op in pending {
        match op {
            PendingCanvasDraw::Clear { pane_id, color } => {
                let Some((rt_pane, _, _)) = lookup_canvas_pane(pane_id) else {
                    continue;
                };
                let _ = unsafe {
                    wingui_rs::super_terminal::super_terminal_vector_draw(
                        ctx,
                        rt_pane,
                        0,
                        wingui_rs::super_terminal::rgba_content_buffer::PERSISTENT,
                        wingui_rs::super_terminal::rgba_blit::OPAQUE,
                        1,
                        color.as_ptr(),
                        std::ptr::null(),
                        0,
                    )
                };
            }
            PendingCanvasDraw::Primitive { pane_id, primitive } => {
                let Some((rt_pane, _, _)) = lookup_canvas_pane(pane_id) else {
                    continue;
                };
                let _ = unsafe {
                    wingui_rs::super_terminal::super_terminal_vector_draw(
                        ctx,
                        rt_pane,
                        0,
                        wingui_rs::super_terminal::rgba_content_buffer::PERSISTENT,
                        wingui_rs::super_terminal::rgba_blit::ALPHA_OVER,
                        0,
                        std::ptr::null::<f32>(),
                        &primitive,
                        1,
                    )
                };
            }
        }
    }
}

// ─── Canvas FFI shims ─────────────────────────────────────────────

/// `Window.canvas(id, w, h)` — declare an RGBA canvas pane. The id
/// is the BCPL caller's chosen handle; subsequent CLEAR / FILL /
/// LINE / etc. calls reference this same id. Width and height are
/// the pane's backing-texture dimensions in pixels.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn bcpl_wingui_window_canvas(
    id: i64,
    width: i64,
    height: i64,
) {
    #[cfg(target_os = "windows")]
    {
        let pid = (id.max(0)) as u32;
        let w = width.max(1) as u32;
        let h = height.max(1) as u32;
        if let Ok(mut g) = REGISTRY.lock() {
            if let Some(main) = g.main.as_mut() {
                main.children.push(ChildNode::Canvas {
                    id: pid,
                    width: w,
                    height: h,
                });
            }
            g.canvas_panes.retain(|p| p.pane_id != pid);
            g.canvas_panes.push(CanvasPaneEntry {
                pane_id: pid,
                width: w,
                height: h,
                runtime_pane_id: None,
            });
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = (id, width, height);
    }
}

/// `Window.canvas_clear(canvas_id, r, g, b)` — wipe a canvas to a
/// solid RGB colour. Alpha is forced to opaque.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn bcpl_wingui_canvas_clear(
    canvas_id: i64,
    r: i64,
    g: i64,
    b: i64,
) {
    #[cfg(target_os = "windows")]
    {
        let pid = canvas_id.max(0) as u32;
        let color = rgba_bytes_to_f32(r, g, b);
        submit_canvas_clear(pid, color);
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = (canvas_id, r, g, b);
    }
}

/// `Window.canvas_fill(canvas_id, x, y, w, h, r, g, b)` — filled
/// rectangle. Composites over what's already on the surface.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn bcpl_wingui_canvas_fill(
    canvas_id: i64,
    x: i64,
    y: i64,
    w: i64,
    h: i64,
    r: i64,
    g: i64,
    b: i64,
) {
    #[cfg(target_os = "windows")]
    {
        let pid = canvas_id.max(0) as u32;
        let prim = wingui_rs::super_terminal::WinguiVectorPrimitive::rect_filled(
            x as f32,
            y as f32,
            w as f32,
            h as f32,
            rgba_bytes_to_f32(r, g, b),
        );
        submit_canvas_primitive(pid, prim);
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = (canvas_id, x, y, w, h, r, g, b);
    }
}

/// `Window.canvas_frame(canvas_id, x, y, w, h, stroke, r, g, b)` —
/// stroked rectangle. `stroke` is the full line thickness in
/// pixels; the renderer halves it internally.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn bcpl_wingui_canvas_frame(
    canvas_id: i64,
    x: i64,
    y: i64,
    w: i64,
    h: i64,
    stroke: i64,
    r: i64,
    g: i64,
    b: i64,
) {
    #[cfg(target_os = "windows")]
    {
        let pid = canvas_id.max(0) as u32;
        let prim = wingui_rs::super_terminal::WinguiVectorPrimitive::rect_stroked(
            x as f32,
            y as f32,
            w as f32,
            h as f32,
            stroke.max(1) as f32,
            rgba_bytes_to_f32(r, g, b),
        );
        submit_canvas_primitive(pid, prim);
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = (canvas_id, x, y, w, h, stroke, r, g, b);
    }
}

/// `Window.canvas_line(canvas_id, x0, y0, x1, y1, stroke, r, g, b)`
/// — straight line between two points.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn bcpl_wingui_canvas_line(
    canvas_id: i64,
    x0: i64,
    y0: i64,
    x1: i64,
    y1: i64,
    stroke: i64,
    r: i64,
    g: i64,
    b: i64,
) {
    #[cfg(target_os = "windows")]
    {
        let pid = canvas_id.max(0) as u32;
        let prim = wingui_rs::super_terminal::WinguiVectorPrimitive::line(
            x0 as f32,
            y0 as f32,
            x1 as f32,
            y1 as f32,
            stroke.max(1) as f32,
            rgba_bytes_to_f32(r, g, b),
        );
        submit_canvas_primitive(pid, prim);
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = (canvas_id, x0, y0, x1, y1, stroke, r, g, b);
    }
}

/// `Window.canvas_circle(canvas_id, cx, cy, radius, r, g, b)` —
/// filled circle centred on (cx, cy).
#[unsafe(no_mangle)]
pub extern "C-unwind" fn bcpl_wingui_canvas_circle(
    canvas_id: i64,
    cx: i64,
    cy: i64,
    radius: i64,
    r: i64,
    g: i64,
    b: i64,
) {
    #[cfg(target_os = "windows")]
    {
        let pid = canvas_id.max(0) as u32;
        let prim = wingui_rs::super_terminal::WinguiVectorPrimitive::circle_filled(
            cx as f32,
            cy as f32,
            radius.max(1) as f32,
            rgba_bytes_to_f32(r, g, b),
        );
        submit_canvas_primitive(pid, prim);
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = (canvas_id, cx, cy, radius, r, g, b);
    }
}

// ─── Hosting entry point ──────────────────────────────────────────

#[cfg(target_os = "windows")]
fn run_window_now() -> i32 {
    use wingui_rs::super_terminal::{
        event_type, super_terminal_request_stop, super_terminal_run_hosted_app,
        HostedEventFn, HostedSetupFn, SuperTerminalClientContext, SuperTerminalEvent,
        SuperTerminalHostedAppDesc, SuperTerminalRunResult,
    };

    // Snapshot the registry into a JSON spec while we hold the lock,
    // then release it for the (blocking) hosted-app call.
    let (spec_json, title) = {
        let g = match REGISTRY.lock() {
            Ok(g) => g,
            Err(_) => return -1,
        };
        let Some(main) = g.main.as_ref() else {
            return -1;
        };
        (build_spec(main).to_json_string(), main.title.clone())
    };

    let title_c = CString::new(title).unwrap_or_else(|_| CString::new("BCPL wingui").unwrap());
    let font = CString::new("Consolas").unwrap();
    anchor_cwd_for_shaders();
    let shader = CString::new("shaders/text_grid.hlsl").unwrap();
    let spec_c = match CString::new(spec_json) {
        Ok(c) => c,
        Err(_) => return -1,
    };

    /// Called once before any events. Captures the
    /// `SuperTerminalClientContext*` so `close()` (which may be
    /// called from a worker thread) has something to push a
    /// request_stop through. Then resolves the runtime ids of any
    /// declared canvas panes and drains the pre-run drawing queue
    /// — pre-run primitives are queued because the panes haven't
    /// resolved yet; on_setup is the first moment they can.
    unsafe extern "C" fn on_setup(
        ctx: *mut SuperTerminalClientContext,
        _user_data: *mut std::ffi::c_void,
    ) -> i32 {
        if !ctx.is_null() {
            if let Ok(mut g) = UI_CTX.lock() {
                *g = Some(SendableCtx(ctx));
            }
            resolve_canvas_panes(ctx);
            drain_pending_canvas_draws(ctx);
        }
        1
    }

    /// Fires for every native-UI event. Native-UI events carry a
    /// JSON payload; we extract the `"event"` field's value and,
    /// if it matches our `btn_<id>` convention, decode the id and
    /// queue it. Close requests stop the host.
    unsafe extern "C" fn on_event(
        ctx: *mut SuperTerminalClientContext,
        event: *const SuperTerminalEvent,
        _user_data: *mut std::ffi::c_void,
    ) {
        if !ctx.is_null() {
            if let Ok(mut g) = UI_CTX.lock() {
                if g.is_none() {
                    *g = Some(SendableCtx(ctx));
                }
            }
        }
        if event.is_null() || ctx.is_null() {
            return;
        }
        let event = unsafe { &*event };

        if event.ty == event_type::NATIVE_UI {
            // The NativeUiEvent variant of the union starts with a
            // u64 window_id (8 bytes), then a 512-byte NUL-
            // terminated UTF-8 payload buffer. Skip the window_id.
            let payload_base = unsafe { event._payload.as_ptr().add(1) as *const u8 };
            let mut len = 0usize;
            while len < 512 {
                if unsafe { *payload_base.add(len) } == 0 {
                    break;
                }
                len += 1;
            }
            let bytes = unsafe { std::slice::from_raw_parts(payload_base, len) };
            if let Ok(text) = std::str::from_utf8(bytes) {
                if let Some(name) = extract_event_name(text) {
                    if let Some(btn_id) = parse_button_id_from_event(&name) {
                        if let Ok(mut q) = EVENT_QUEUE.lock() {
                            q.push_back(btn_id);
                        }
                        // Honour `close_on(btn_id)` registrations.
                        // The click both fires the event for BCPL
                        // to inspect AND closes the window so the
                        // user sees an immediate response.
                        let should_close = CLOSE_TRIGGERS
                            .lock()
                            .map(|g| g.contains(&btn_id))
                            .unwrap_or(false);
                        if should_close {
                            unsafe { super_terminal_request_stop(ctx, 0) };
                        }
                    }
                }
            }
        }

        if matches!(
            event.ty,
            t if t == event_type::CLOSE_REQUESTED
                || t == event_type::WINDOW_CLOSED
                || t == event_type::HOST_STOPPING,
        ) {
            unsafe { super_terminal_request_stop(ctx, 0) };
        }
    }

    let setup: HostedSetupFn = Some(on_setup);
    let on_event_fn: HostedEventFn = Some(on_event);

    let desc = SuperTerminalHostedAppDesc {
        title_utf8: title_c.as_ptr(),
        columns: 80,
        rows: 24,
        flags: 0,
        command_queue_capacity: 256,
        event_queue_capacity: 256,
        font_family_utf8: font.as_ptr(),
        font_pixel_height: 18,
        dpi_scale: 1.0,
        text_shader_path_utf8: shader.as_ptr(),
        initial_ui_json_utf8: spec_c.as_ptr(),
        target_frame_ms: 16,
        auto_request_present: 0,
        user_data: std::ptr::null_mut(),
        setup,
        on_event: on_event_fn,
        on_frame: None,
        shutdown: None,
    };

    // Clear stale ctx from a previous run. The close-trigger set
    // stays — programs register before run() and we want those
    // registrations to apply to THIS run; the BCPL caller is
    // responsible for `close_on` ordering relative to `run`.
    if let Ok(mut g) = UI_CTX.lock() {
        *g = None;
    }

    let mut result = SuperTerminalRunResult::default();
    let ok = unsafe { super_terminal_run_hosted_app(&desc, &mut result) };

    // Drop the ctx — invalid post-run.
    if let Ok(mut g) = UI_CTX.lock() {
        *g = None;
    }

    if ok == 0 {
        let msg = unsafe { CStr::from_ptr(result.message_utf8.as_ptr()) }
            .to_string_lossy()
            .into_owned();
        eprintln!("bcpl-wingui: run_hosted_app failed: {msg}");
        return -2;
    }
    result.exit_code as i32
}

// ─── Public list of (name, fn_ptr_as_usize) ────────────────────────

/// Returned by `newbcpl-runtime`'s builtin registry to register
/// every wingui shim with one call. Names match the C-ABI symbols
/// exactly; the JIT resolves BCPL call sites against this table.
pub fn builtin_addresses() -> Vec<(&'static str, usize)> {
    vec![
        (
            "bcpl_wingui_version_packed",
            bcpl_wingui_version_packed as *const () as usize,
        ),
        (
            "bcpl_wingui_is_available",
            bcpl_wingui_is_available as *const () as usize,
        ),
        (
            "bcpl_wingui_window_define",
            bcpl_wingui_window_define as *const () as usize,
        ),
        (
            "bcpl_wingui_window_button",
            bcpl_wingui_window_button as *const () as usize,
        ),
        (
            "bcpl_wingui_window_label",
            bcpl_wingui_window_label as *const () as usize,
        ),
        (
            "bcpl_wingui_window_run",
            bcpl_wingui_window_run as *const () as usize,
        ),
        (
            "bcpl_wingui_window_close",
            bcpl_wingui_window_close as *const () as usize,
        ),
        (
            "bcpl_wingui_close_on",
            bcpl_wingui_close_on as *const () as usize,
        ),
        (
            "bcpl_wingui_poll_event",
            bcpl_wingui_poll_event as *const () as usize,
        ),
        // ─── Canvas surface (W4) ────────────────────────────────
        (
            "bcpl_wingui_window_canvas",
            bcpl_wingui_window_canvas as *const () as usize,
        ),
        (
            "bcpl_wingui_canvas_clear",
            bcpl_wingui_canvas_clear as *const () as usize,
        ),
        (
            "bcpl_wingui_canvas_fill",
            bcpl_wingui_canvas_fill as *const () as usize,
        ),
        (
            "bcpl_wingui_canvas_frame",
            bcpl_wingui_canvas_frame as *const () as usize,
        ),
        (
            "bcpl_wingui_canvas_line",
            bcpl_wingui_canvas_line as *const () as usize,
        ),
        (
            "bcpl_wingui_canvas_circle",
            bcpl_wingui_canvas_circle as *const () as usize,
        ),
        // ─── Procedural shim (FB-compat surface) ─────────────────
        //
        // Same function pointers as the class-form `bcpl_wingui_*`
        // entries above, exposed under SCREAMING_SNAKE_CASE names
        // that mirror NewFB's compound verbs (`WINDOW DEFINE` →
        // `WIN_DEFINE` etc.). Registering as runtime builtins
        // bypasses the loader's module-prefix pass; programs call
        // `WIN_DEFINE(...)` from any source file without a `GET` or
        // class scaffolding.
        //
        // Mapping table (FB compound verb ↔ BCPL procedure):
        //
        //   WINDOW DEFINE id, title       →  WIN_DEFINE(id, title)
        //   WINDOW BUTTON id, label       →  WIN_BUTTON(id, label)
        //   WINDOW LABEL  id, text        →  WIN_LABEL (id, text)
        //   WINDOW RUN                    →  WIN_RUN()
        //   WINDOW CLOSE                  →  WIN_CLOSE()
        //   (close-on-event registration) →  WIN_CLOSE_ON(btn_id)
        //   (post-run event drain)        →  WIN_POLL() -> btn_id
        //   (DLL probes)                  →  WIN_VERSION() / WIN_AVAILABLE()
        (
            "WIN_DEFINE",
            bcpl_wingui_window_define as *const () as usize,
        ),
        (
            "WIN_BUTTON",
            bcpl_wingui_window_button as *const () as usize,
        ),
        (
            "WIN_LABEL",
            bcpl_wingui_window_label as *const () as usize,
        ),
        (
            "WIN_RUN",
            bcpl_wingui_window_run as *const () as usize,
        ),
        (
            "WIN_CLOSE",
            bcpl_wingui_window_close as *const () as usize,
        ),
        (
            "WIN_CLOSE_ON",
            bcpl_wingui_close_on as *const () as usize,
        ),
        (
            "WIN_POLL",
            bcpl_wingui_poll_event as *const () as usize,
        ),
        (
            "WIN_VERSION",
            bcpl_wingui_version_packed as *const () as usize,
        ),
        (
            "WIN_AVAILABLE",
            bcpl_wingui_is_available as *const () as usize,
        ),
        // ─── Canvas procedural shim ─────────────────────────────
        //
        //   WINDOW CANVAS id, w, h        →  WIN_CANVAS(id, w, h)
        //   PANE CLEAR id, r, g, b         →  CANVAS_CLEAR(id, r, g, b)
        //   PANE FILL  id, x,y,w,h, r,g,b  →  CANVAS_FILL (id, x,y,w,h, r,g,b)
        //   PANE FRAME id, x,y,w,h, s, r,g,b → CANVAS_FRAME(id, x,y,w,h, s, r,g,b)
        //   PANE LINE  id, x0,y0,x1,y1, s, r,g,b → CANVAS_LINE(id, x0,y0,x1,y1, s, r,g,b)
        //   PANE CIRCLE id, cx,cy,r, r,g,b → CANVAS_CIRCLE(id, cx,cy,r, r,g,b)
        (
            "WIN_CANVAS",
            bcpl_wingui_window_canvas as *const () as usize,
        ),
        (
            "CANVAS_CLEAR",
            bcpl_wingui_canvas_clear as *const () as usize,
        ),
        (
            "CANVAS_FILL",
            bcpl_wingui_canvas_fill as *const () as usize,
        ),
        (
            "CANVAS_FRAME",
            bcpl_wingui_canvas_frame as *const () as usize,
        ),
        (
            "CANVAS_LINE",
            bcpl_wingui_canvas_line as *const () as usize,
        ),
        (
            "CANVAS_CIRCLE",
            bcpl_wingui_canvas_circle as *const () as usize,
        ),
    ]
}
