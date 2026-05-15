//! iGui — integrated GUI for NewBCPL.
//!
//! Direct-rendered MDI frame using Direct2D and DirectWrite. Borrowed
//! from the sister NewCormanLisp repo (E:/CL/NewCormanLisp/src/
//! ncl-runtime/src/igui), which itself ported it from NewCP's iGui
//! slice (E:/NewCP/NewCP/src/newcp-runtime/src/igui). The renderer
//! architecture and event-mailbox design carry over verbatim. The
//! integration layer changes per host: CL added a `lisp_shims`
//! module exposing the surface to its tagged-Word ABI; NewBCPL
//! reaches the surface either through the language-neutral
//! `extern "C"` entry points in `cp_exports` or through a future
//! `bcpl_shims` glue when the BCPL JIT lands.
//!
//! Architecture:
//!   - `window` runs an MDI frame on a dedicated GUI thread.
//!   - `child` manages MDI children; each child owns a render
//!     surface (D3D swap chain wrapped by D2D).
//!   - `channels` is a bounded MPSC mailbox carrying typed
//!     `IGuiEvent` values from the GUI thread to the language
//!     thread.
//!   - `batch` queues `SurfaceCmd` draw operations for execution by
//!     `executor` on the GUI thread.
//!   - `bedit` and `log_view` are built-in MDI children for a
//!     BCPL-flavoured editor and a log overlay; `tools_menu` wires
//!     their menu and accelerators into the frame.
//!
//! Phase 1 scope: open an MDI frame + MDI client, initialize D3D11 /
//! Direct2D / DirectWrite, paint a solid color into the MDI client
//! area during `WM_PAINT`, and exit cleanly on `WM_CLOSE` /
//! `WM_DESTROY`. The BCPL-side bindings (NEXT-EVENT / OPEN-CHILD /
//! ...) ride on top in a follow-up commit.

#![cfg(windows)]

pub mod batch;
pub(crate) mod bedit;
pub mod channels;
mod child;
pub mod cp_exports;
mod cursor;
mod d2d;
mod d3d;
mod dwrite;
mod executor;
mod font_metrics;
mod log_view;
mod menu;
mod registry;
mod renderer;
mod replies;
pub(crate) mod rope_buffer;
pub mod system_colors;
pub(crate) mod text_view;
mod tools_menu;
pub(crate) mod window;

pub use bedit::{
    install_checker, run_check_in_singleton as bedit_run_check,
    set_startup_file as bedit_set_startup_file,
    snapshot_buffer as bedit_snapshot_buffer, Diagnostic,
};
pub use log_view::append as log_append;
pub use tools_menu::RUN_MENU_CMD_ID;
pub use window::run;

/// Errors surfaced from iGui startup. Phase 1 keeps this lossy on purpose;
/// every variant carries enough text to diagnose without a debugger.
#[derive(Debug)]
pub enum IGuiError {
    Win32(String),
    D3D(String),
    D2D(String),
    DWrite(String),
}

impl std::fmt::Display for IGuiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            IGuiError::Win32(msg) => write!(f, "iGui: Win32: {msg}"),
            IGuiError::D3D(msg) => write!(f, "iGui: D3D: {msg}"),
            IGuiError::D2D(msg) => write!(f, "iGui: D2D: {msg}"),
            IGuiError::DWrite(msg) => write!(f, "iGui: DirectWrite: {msg}"),
        }
    }
}

impl std::error::Error for IGuiError {}

/// Phase 1 paints this slate gray into the MDI client area so we can see
/// the renderer is actually running. Will be replaced once the surface
/// executor lands and children own their own colors.
pub(crate) const PHASE1_BACKGROUND: [f32; 4] = [0.18, 0.20, 0.23, 1.0];
