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
}

#[cfg(target_os = "windows")]
struct MainWindow {
    title: String,
    children: Vec<ChildNode>,
}

#[cfg(target_os = "windows")]
struct Registry {
    main: Option<MainWindow>,
}

#[cfg(target_os = "windows")]
impl Registry {
    const fn new() -> Self {
        Self { main: None }
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
    /// request_stop through.
    unsafe extern "C" fn on_setup(
        ctx: *mut SuperTerminalClientContext,
        _user_data: *mut std::ffi::c_void,
    ) -> i32 {
        if !ctx.is_null() {
            if let Ok(mut g) = UI_CTX.lock() {
                *g = Some(SendableCtx(ctx));
            }
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
    ]
}
