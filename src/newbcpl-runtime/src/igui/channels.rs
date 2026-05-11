//! Event mailbox: GUI thread → language thread.
//!
//! A bounded MPSC queue carrying typed `IGuiEvent` values. Producers
//! are Win32 message handlers on the GUI thread (and, later, the
//! surface executor when it answers synchronous queries). Consumer
//! is the language thread, which calls `next_event` from
//! `iGui.NextEvent`.

#![cfg(windows)]

use std::collections::{HashSet, VecDeque};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender, TrySendError};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// Stable enum tags exported to CP as `iGui.Ev*` constants.
pub mod kind {
    pub const NONE: i64 = 0;
    pub const KEY: i64 = 1;
    pub const CHAR: i64 = 2;
    pub const MOUSE: i64 = 3;
    pub const FOCUS: i64 = 4;
    pub const RESIZE: i64 = 5;
    pub const PAINT: i64 = 6;
    pub const CLOSE: i64 = 7;
    pub const FRAME_CLOSE: i64 = 8;
    pub const MENU: i64 = 9;
    pub const THEME_CHANGE: i64 = 10;
    pub const DPI_CHANGE: i64 = 11;
    pub const SURFACE_REPLY: i64 = 12;
    pub const TICK: i64 = 13;
}

/// Mouse-event sub-kinds packed into the `mouse_op` field. Each is a
/// distinct value (not a bitmask) so the language side can match
/// directly.
pub mod mouse_op {
    pub const MOVE: i64 = 0;
    pub const LEFT_DOWN: i64 = 1;
    pub const LEFT_UP: i64 = 2;
    pub const RIGHT_DOWN: i64 = 3;
    pub const RIGHT_UP: i64 = 4;
    pub const MIDDLE_DOWN: i64 = 5;
    pub const MIDDLE_UP: i64 = 6;
    pub const WHEEL: i64 = 7;
}

/// Modifier-key bits as a packed `i64`. Matches Win32 GetKeyState bit
/// layout where convenient; CP code reads the named bits via
/// `iGui.Mod*` constants.
pub mod modifier {
    pub const SHIFT: i64 = 1 << 0;
    pub const CONTROL: i64 = 1 << 1;
    pub const ALT: i64 = 1 << 2;
    pub const WIN: i64 = 1 << 3;
    pub const CAPS: i64 = 1 << 4;
}

/// All input and lifecycle events flow as one of these structs.
/// Specialised carriers per kind keep the variant fields self-describing
/// without a tagged-union ABI on the wire.
#[derive(Debug, Clone)]
pub enum IGuiEvent {
    Key {
        child_id: i64,
        vkey: i64,
        scancode: i64,
        mods: i64,
        repeat: i64,
        down: bool,
        time_ms: i64,
    },
    Char {
        child_id: i64,
        codepoint: i64,
        mods: i64,
        time_ms: i64,
    },
    Mouse {
        child_id: i64,
        x: i64,
        y: i64,
        op: i64, // mouse_op::*
        button: i64,
        mods: i64,
        wheel_delta: i64,
        wheel_lines: i64,
        time_ms: i64,
    },
    Focus {
        child_id: i64,
        gained: bool,
    },
    Resize {
        child_id: i64,
        width: i64,
        height: i64,
    },
    Close {
        child_id: i64,
    },
    FrameClose,
    ThemeChange,
    DpiChange {
        child_id: i64,
        dpi_x: i64, // ×100 (e.g. 192 means 192 dpi; ×100 reserves room for fractional later)
        dpi_y: i64,
    },
    Menu {
        menu_id: i64,
        item_id: i64,
    },
    /// Animation tick. Fires from a Win32 timer running on a child's
    /// render host; Win32 auto-coalesces queued WM_TIMERs so the
    /// language thread sees at most one tick per child per drain
    /// cycle even if it lags.
    Tick {
        child_id: i64,
        time_ms: i64,
    },
}

struct Mailbox {
    tx: SyncSender<IGuiEvent>,
    rx: Mutex<Receiver<IGuiEvent>>,
}

const CAPACITY: usize = 1024;

static MAILBOX: OnceLock<Mailbox> = OnceLock::new();

pub fn install() {
    MAILBOX.get_or_init(|| {
        let (tx, rx) = sync_channel(CAPACITY);
        Mailbox {
            tx,
            rx: Mutex::new(rx),
        }
    });
}

/// Push from the GUI thread. If the queue is full, drop the new event
/// and log; spamming during a wedged language thread should not block
/// the message pump.
pub fn push(ev: IGuiEvent) {
    let Some(mb) = MAILBOX.get() else {
        return;
    };
    match mb.tx.try_send(ev) {
        Ok(()) => {}
        Err(TrySendError::Full(_)) => {
            // Dropping is correct: the GUI thread cannot block on the
            // language thread, and a stalled consumer means whatever
            // we just lost is the least of the user's problems.
            eprintln!("[igui] event mailbox full, dropping event");
        }
        Err(TrySendError::Disconnected(_)) => {
            // Receiver gone; mailbox is being torn down. Silently ignore.
        }
    }
}

/// Stash of events that arrived but didn't match the current
/// per-window consumer's interest. Drained ahead of the channel by
/// every consumer, so events for window A queued during a
/// `next_event_for(window_B, …)` call resurface the next time
/// anyone asks for window-A events.
///
/// The language thread is the only consumer, so the stash doesn't
/// need cross-process visibility — but it lives in a Mutex anyway
/// because `next_event_for` may be called from any frame on the
/// JIT call stack and we never want to keep the lock across a
/// blocking `recv`.
static EVENT_STASH: Mutex<VecDeque<IGuiEvent>> = Mutex::new(VecDeque::new());

/// Persistent set of windows the language thread is "interested in".
/// When non-empty, `next_event` returns only events whose
/// `child_id` is in the set (plus "global" events that have no
/// `child_id` — see `matches_target`). When empty, `next_event`
/// returns every event (the default, equivalent to the pre-filter
/// behaviour).
///
/// Manipulated by `filter_on_window` / `unfilter_window` /
/// `clear_filter`. The driver clears the set at the start of each
/// JIT-run so one program's filter never leaks into the next.
static EVENT_FILTER: OnceLock<Mutex<HashSet<i64>>> = OnceLock::new();

fn filter_lock() -> &'static Mutex<HashSet<i64>> {
    EVENT_FILTER.get_or_init(|| Mutex::new(HashSet::new()))
}

pub fn filter_on_window(child_id: i64) {
    if let Ok(mut filter) = filter_lock().lock() {
        filter.insert(child_id);
    }
}

pub fn unfilter_window(child_id: i64) {
    if let Ok(mut filter) = filter_lock().lock() {
        filter.remove(&child_id);
    }
}

pub fn clear_filter() {
    if let Ok(mut filter) = filter_lock().lock() {
        filter.clear();
    }
}

/// Does `ev` belong to one of the registered-interest windows (or
/// is it a "global" event)? Used by `next_event` when the filter
/// is non-empty.
fn matches_filter(ev: &IGuiEvent, filter: &HashSet<i64>) -> bool {
    match ev {
        IGuiEvent::FrameClose | IGuiEvent::ThemeChange => true,
        IGuiEvent::Menu { .. } => true,
        IGuiEvent::Key { child_id, .. }
        | IGuiEvent::Char { child_id, .. }
        | IGuiEvent::Mouse { child_id, .. }
        | IGuiEvent::Focus { child_id, .. }
        | IGuiEvent::Resize { child_id, .. }
        | IGuiEvent::Close { child_id }
        | IGuiEvent::DpiChange { child_id, .. }
        | IGuiEvent::Tick { child_id, .. } => filter.contains(child_id),
    }
}

/// Pop the next event, honouring the persistent filter set if one
/// is configured. `timeout_ms < 0` blocks indefinitely.
///
/// Semantics depend on `EVENT_FILTER`:
///
///  - filter empty:  return the next event from stash, then channel
///                   (the pre-filter "any event" behaviour).
///  - filter non-empty:  return the next event whose `child_id` is
///                   in the filter (or a global event like
///                   `FrameClose`); other events park in the stash
///                   so they survive a later `clear_filter` call.
pub fn next_event(timeout_ms: i64) -> Option<IGuiEvent> {
    // Snapshot the filter once so we don't hold its lock across a
    // potentially-blocking `recv`. Cloning a HashSet of i64 is
    // cheap for the small per-program sets we expect.
    let filter_snapshot: Option<HashSet<i64>> = filter_lock()
        .lock()
        .ok()
        .map(|f| f.clone())
        .filter(|f| !f.is_empty());

    // Drain stash first. With a filter, walk for a match. Without,
    // pop the head.
    {
        let mut stash = EVENT_STASH.lock().expect("EVENT_STASH poisoned");
        match &filter_snapshot {
            None => {
                if let Some(ev) = stash.pop_front() {
                    return Some(ev);
                }
            }
            Some(filter) => {
                for i in 0..stash.len() {
                    if matches_filter(&stash[i], filter) {
                        return stash.remove(i);
                    }
                }
            }
        }
    }

    // Then the channel, with the same matching policy. Non-matching
    // events park in the stash; the loop continues, time-bounded by
    // the overall deadline.
    let mb = MAILBOX.get()?;
    let rx = mb.rx.lock().ok()?;
    let deadline = if timeout_ms < 0 {
        None
    } else {
        Some(Instant::now() + Duration::from_millis(timeout_ms as u64))
    };
    loop {
        let ev = match deadline {
            None => rx.recv().ok()?,
            Some(deadline) => {
                let now = Instant::now();
                if now >= deadline {
                    return None;
                }
                rx.recv_timeout(deadline - now).ok()?
            }
        };
        match &filter_snapshot {
            None => return Some(ev),
            Some(filter) => {
                if matches_filter(&ev, filter) {
                    return Some(ev);
                }
                if let Ok(mut stash) = EVENT_STASH.lock() {
                    stash.push_back(ev);
                }
            }
        }
    }
}

/// Wait for an event whose `child_id` matches `target` (or for a
/// "global" event with no child — `FrameClose`, `ThemeChange`, or
/// `Menu`), parking any non-matching events into `EVENT_STASH` so
/// later consumers still see them. `timeout_ms < 0` blocks
/// indefinitely; the timeout is overall wall-clock so consuming
/// non-matching events doesn't reset it.
pub fn next_event_for(target: i64, timeout_ms: i64) -> Option<IGuiEvent> {
    // 1. Look for an already-stashed matching event.
    if let Ok(mut stash) = EVENT_STASH.lock() {
        for i in 0..stash.len() {
            if matches_target(&stash[i], target) {
                return stash.remove(i);
            }
        }
    }

    // 2. Receive from the channel; stash non-matching, return on
    //    match. Bounded by the overall deadline so the caller's
    //    timeout is honoured across any number of stash-parks.
    let mb = MAILBOX.get()?;
    let rx = mb.rx.lock().ok()?;
    let deadline = if timeout_ms < 0 {
        None
    } else {
        Some(Instant::now() + Duration::from_millis(timeout_ms as u64))
    };
    loop {
        let ev = match deadline {
            None => rx.recv().ok()?,
            Some(deadline) => {
                let now = Instant::now();
                if now >= deadline {
                    return None;
                }
                rx.recv_timeout(deadline - now).ok()?
            }
        };
        if matches_target(&ev, target) {
            return Some(ev);
        }
        if let Ok(mut stash) = EVENT_STASH.lock() {
            stash.push_back(ev);
        }
    }
}

/// Drop every event currently in the stash. Useful when a program
/// transitions modes (e.g. closes one window and opens another) and
/// wants a clean slate.
pub fn discard_stashed_events() {
    if let Ok(mut stash) = EVENT_STASH.lock() {
        stash.clear();
    }
}

/// Does `ev` belong to the consumer that asked for `target`?
/// Per-window events match when their `child_id` equals `target`.
/// "Global" events (`FrameClose`, `ThemeChange`, `Menu` with
/// `menu_id == 0`) match every target so a program in a
/// `next_event_for` loop still sees them and can react.
fn matches_target(ev: &IGuiEvent, target: i64) -> bool {
    match ev {
        IGuiEvent::FrameClose | IGuiEvent::ThemeChange => true,
        IGuiEvent::Menu { .. } => true,
        IGuiEvent::Key { child_id, .. }
        | IGuiEvent::Char { child_id, .. }
        | IGuiEvent::Mouse { child_id, .. }
        | IGuiEvent::Focus { child_id, .. }
        | IGuiEvent::Resize { child_id, .. }
        | IGuiEvent::Close { child_id }
        | IGuiEvent::DpiChange { child_id, .. }
        | IGuiEvent::Tick { child_id, .. } => *child_id == target,
    }
}
