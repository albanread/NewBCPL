//! Frame-level "Tools" and "Program" menus + the keyboard accelerator
//! table for the built-in editor windows and the run command.
//!
//! `bedit` and `log_view` hang off a `Tools` submenu. The `Run`
//! command sits on its own `Program` submenu (no submenu items
//! beyond `Run` for now). Keeping the wiring here means the menu
//! bar carries every entry whatever the language thread does.

#![cfg(windows)]

use windows::core::PCWSTR;
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreateAcceleratorTableW, CreateMenu, CreatePopupMenu, ACCEL, FCONTROL, FSHIFT,
    FVIRTKEY, HACCEL, HMENU, MF_POPUP, MF_STRING,
};

use super::bedit;
use super::log_view;

/// WM_COMMAND id for `Program ▸ Run` / `Ctrl+R`. The frame WndProc
/// pushes this through the user-menu path (`IGuiEvent::Menu`) so a
/// language-thread worker installed by the driver picks it up and
/// JIT-runs the program. The id sits in the same range as
/// `bedit::MENU_CMD_ID` / `log_view::MENU_CMD_ID` (0x30xx) to keep
/// the built-in command ids together.
pub const RUN_MENU_CMD_ID: u16 = 0x3002;

/// Append a `Tools` submenu to `bar` (bedit + log view).
pub fn append_tools_menu(bar: HMENU) {
    let popup = match unsafe { CreatePopupMenu() } {
        Ok(p) => p,
        Err(e) => {
            eprintln!("[tools-menu] CreatePopupMenu failed: {e}");
            return;
        }
    };

    let bedit_item: Vec<u16> = "bedit\tCtrl+Shift+E\0".encode_utf16().collect();
    if let Err(e) = unsafe {
        AppendMenuW(
            popup,
            MF_STRING,
            bedit::MENU_CMD_ID as usize,
            PCWSTR(bedit_item.as_ptr()),
        )
    } {
        eprintln!("[tools-menu] append bedit: {e}");
    }

    let log_item: Vec<u16> = "Log\tCtrl+Shift+L\0".encode_utf16().collect();
    if let Err(e) = unsafe {
        AppendMenuW(
            popup,
            MF_STRING,
            log_view::MENU_CMD_ID as usize,
            PCWSTR(log_item.as_ptr()),
        )
    } {
        eprintln!("[tools-menu] append log: {e}");
    }

    let title: Vec<u16> = "&Tools\0".encode_utf16().collect();
    if let Err(e) = unsafe {
        AppendMenuW(
            bar,
            MF_POPUP,
            popup.0 as usize,
            PCWSTR(title.as_ptr()),
        )
    } {
        eprintln!("[tools-menu] append popup: {e}");
    }
}

/// Append a `Program` submenu to `bar` with the `Run` command. The
/// command id is `RUN_MENU_CMD_ID`, which the frame WndProc routes
/// to the language thread via `IGuiEvent::Menu`.
pub fn append_program_menu(bar: HMENU) {
    let popup = match unsafe { CreatePopupMenu() } {
        Ok(p) => p,
        Err(e) => {
            eprintln!("[tools-menu] CreatePopupMenu (Program) failed: {e}");
            return;
        }
    };

    let run_item: Vec<u16> = "&Run\tCtrl+R\0".encode_utf16().collect();
    if let Err(e) = unsafe {
        AppendMenuW(
            popup,
            MF_STRING,
            RUN_MENU_CMD_ID as usize,
            PCWSTR(run_item.as_ptr()),
        )
    } {
        eprintln!("[tools-menu] append Run: {e}");
    }

    let title: Vec<u16> = "&Program\0".encode_utf16().collect();
    if let Err(e) = unsafe {
        AppendMenuW(
            bar,
            MF_POPUP,
            popup.0 as usize,
            PCWSTR(title.as_ptr()),
        )
    } {
        eprintln!("[tools-menu] append Program popup: {e}");
    }
}

/// Build a stand-alone menu bar with Program and Tools submenus.
/// Used at frame startup when no language-thread menu has been set.
pub fn build_default_menu_bar() -> Option<HMENU> {
    let bar = unsafe { CreateMenu() }.ok()?;
    append_program_menu(bar);
    append_tools_menu(bar);
    Some(bar)
}

/// Frame-level accelerator table:
///   Ctrl+R         → Program ▸ Run
///   Ctrl+Shift+E   → bedit
///   Ctrl+Shift+L   → log view
/// All dispatch via `WM_COMMAND` to their respective MENU_CMD_IDs,
/// which the frame WndProc routes appropriately.
pub fn build_accelerator_table() -> Option<HACCEL> {
    let entries = [
        ACCEL {
            fVirt: FCONTROL | FVIRTKEY,
            key: b'R' as u16,
            cmd: RUN_MENU_CMD_ID,
        },
        ACCEL {
            fVirt: FCONTROL | FSHIFT | FVIRTKEY,
            key: b'E' as u16,
            cmd: bedit::MENU_CMD_ID,
        },
        ACCEL {
            fVirt: FCONTROL | FSHIFT | FVIRTKEY,
            key: b'L' as u16,
            cmd: log_view::MENU_CMD_ID,
        },
    ];
    unsafe { CreateAcceleratorTableW(&entries) }
        .ok()
        .filter(|h| !h.is_invalid())
}
