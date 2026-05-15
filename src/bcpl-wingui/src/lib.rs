//! BCPL-side bindings to the wingui retro-graphics framework.
//!
//! This crate provides `extern "C-unwind"` shims that JIT-emitted
//! BCPL code calls into through the runtime's builtin table. Each
//! shim is the Rust side of a class method declared in
//! `modules-active/wingui.bcl` (or a procedural verb in the
//! FB-compat shim). Strings come across as NUL-terminated UTF-8
//! pointers (matching how `WRITES` and friends already pass them);
//! everything else is a 64-bit word.
//!
//! Sits on top of the language-neutral `wingui-rs` crate from the
//! NewFB workspace (see `docs/wingui_bcpl_design.md` and
//! `../../../NewFB/docs/wingui-port-architecture.md`). The Rust
//! side of the layer cake:
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
//! ## Phase 5e equivalent — scope of the first cut
//!
//! Just enough to:
//!   * confirm linkage to `wingui.dll` works from a JIT-launched
//!     BCPL process,
//!   * declare class skeletons in `modules-active/wingui.bcl` that
//!     parse and sema-clean,
//!   * give the user a hello-window demo path that runs through
//!     the JIT without erroring.
//!
//! The actual window-popping path through
//! `super_terminal_run_hosted_app` lands in W2, ported from
//! `newfb-wingui`'s `run_window_now`. This commit ships the
//! foundation: crate, builtins registered, BCPL classes declared,
//! demo file in place, matrix green.

#![cfg_attr(not(target_os = "windows"), allow(dead_code))]

/// `wingui_version_packed()` — returns the wingui DLL's version
/// packed into one 64-bit word: `(major << 32) | (minor << 16) |
/// patch`. Used by the demo to confirm the DLL is reachable from a
/// BCPL JIT'd program before any window-opening machinery comes
/// online. Returns 0 if the version probe fails (unsupported host
/// or DLL not loadable).
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

/// `wingui_is_available()` — returns 1 when the wingui DLL is
/// loaded and responding, 0 otherwise. Demos can branch on this
/// to print a clear "wingui not available on this host" message
/// rather than crashing in the JIT.
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

/// Public list of `(name, fn_ptr_as_usize)` pairs the BCPL runtime's
/// builtin table picks up from `register_builtins()`. Keeping the
/// addresses behind a single accessor matches how the runtime's
/// existing builtin registry works — see
/// `newbcpl_runtime::builtins::builtin_addresses()`.
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
    ]
}
