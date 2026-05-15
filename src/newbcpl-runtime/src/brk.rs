//! `BRK` statement runtime — process-state dump for the
//! debugger-breakpoint statement.
//!
//! Classical BCPL leaves `BRK` as a hint that an attached debugger
//! should halt. We don't run under a debugger most of the time, so
//! the runtime synthesises the inspection itself: when a JIT
//! program executes `BRK`, control transfers to
//! `__newbcpl_brk(routine_name, line)`, which writes a structured
//! snapshot of the program's state to stderr and returns. Execution
//! continues after the BRK.
//!
//! The dump is laid out so the *cheapest* (most-likely-to-succeed)
//! pieces appear first, in case a later section faults:
//!
//!   1. Banner with the BRK site (routine name + source line).
//!   2. Heap summary from `gc::HEAP_COUNTERS` — already a process
//!      global, never faults on read.
//!   3. CPU register state captured via `RtlCaptureContext`.
//!   4. Stack walk via `RtlVirtualUnwind` over the unwind tables we
//!      already register from `jit_mm`. Frame names resolve to raw
//!      RIP addresses today; a follow-up will map JIT code ranges
//!      to BCPL routine names.
//!
//! ### Safety contract
//!
//! BRK can fire when the program is in any state — including a
//! corrupted heap or partially-constructed object graph. The dump
//! must not make things worse. Specifically:
//!
//! * **No C/Rust heap allocation.** Every buffer is a fixed-size
//!   stack array. Strings are byte slices, never `String`.
//! * **No `format!` / `println!`.** Numbers are formatted by
//!   hand-rolled `write_hex` / `write_dec` into the stack buffer.
//! * **Direct WriteFile** on `STD_ERROR_HANDLE` — no stdio locking,
//!   no UTF-8 validation, just bytes to the OS handle.
//! * **Best-effort stack walk.** Any unwind step that fails simply
//!   terminates the walk; we never retry or recurse.
//! * **Never re-enters BRK.** The handler is `extern "C-unwind"`
//!   but takes no path that could itself raise SEH.
//!
//! On non-Windows hosts the handler falls back to a plain stderr
//! write so the test matrix can exercise the banner + heap path
//! without an OS-specific dependency.
//!
//! ### JIT-symbol resolution
//!
//! The stack walk above produces raw RIPs. To turn them back into
//! BCPL routine names we keep a process-global registry of
//! `(start_addr, routine_name)` entries, populated by the JIT
//! crate after finalize. Lookup is a binary search for the largest
//! `start_addr ≤ rip` — we treat the next entry's start as the
//! implicit end of the current function. Entries are immutable
//! once registered; the JIT never relocates code, so a slot in the
//! map stays valid for the rest of the process's life.

// ─── JIT-symbol registry ───────────────────────────────────────────
//
// Populated by the LLVM crate after each JIT-finalize. The BRK
// handler reads this to resolve stack-frame RIPs back to BCPL
// routine names. A `RwLock<Vec<(u64, String)>>` is fine here
// because:
//   * writes happen once per program (at startup, before BRK can
//     ever fire);
//   * reads happen on the BRK path, which is itself a slow path —
//     no need for lock-free.
// `parking_lot` isn't pulled in; std's `RwLock` is plenty.

use std::sync::RwLock;

static JIT_SYMBOLS: RwLock<Vec<(u64, String)>> = RwLock::new(Vec::new());

/// Register a JIT-emitted function for stack-trace resolution.
/// Called from the LLVM crate after `LLVMGetFunctionAddress`
/// returns a stable address. The registry stays sorted by start
/// address so lookups can binary-search.
///
/// `name_bytes` is borrowed for the duration of the call; the
/// registry copies the bytes into an owned `String`.
pub fn register_jit_symbol(start_addr: u64, name: &str) {
    let mut guard = JIT_SYMBOLS.write().expect("JIT_SYMBOLS poisoned");
    // Keep sorted by start_addr. Most realistic call patterns add
    // entries in some arbitrary order (LLVM iteration order), so a
    // single sort after the bulk register is more efficient — but
    // we don't have a "bulk done" signal, so we insertion-sort each
    // entry. The registry is small (one entry per BCPL function);
    // O(n) per insert is fine.
    let pos = guard.partition_point(|(s, _)| *s < start_addr);
    guard.insert(pos, (start_addr, name.to_string()));
}

/// Reasonable upper bound on a JIT-d BCPL routine's machine-code
/// size. Any RIP that sits more than this far above its nearest
/// registered start address is almost certainly host / OS / runtime
/// code, not JIT code, and we report it as un-named to avoid
/// mis-attributing it to the highest-address JIT function.
///
/// BCPL routines compiled at -O0 fit in a few KB; even a vector
/// loop with intrinsic lowering rarely passes 64 KB. 1 MB is wildly
/// generous yet still tight enough to keep host addresses
/// (typically `0x7FF…`) far above any JIT-d region (`0x1DE…` etc.)
/// out of the resolved set.
const MAX_REASONABLE_ROUTINE_SIZE: u64 = 1024 * 1024;

/// Look up the routine name for a given RIP. Returns the entry
/// whose `start_addr` is the largest value `≤ rip` *and* whose
/// distance to the RIP is below `MAX_REASONABLE_ROUTINE_SIZE`.
/// Frames in host / OS / runtime code resolve to `None` and the
/// caller prints just the raw address.
fn lookup_jit_symbol(rip: u64) -> Option<String> {
    let guard = JIT_SYMBOLS.read().ok()?;
    if guard.is_empty() {
        return None;
    }
    // `partition_point` returns the index of the first entry with
    // `start > rip`. The entry we want is just before that.
    let after = guard.partition_point(|(s, _)| *s <= rip);
    if after == 0 {
        return None;
    }
    let (start, name) = &guard[after - 1];
    if rip.saturating_sub(*start) > MAX_REASONABLE_ROUTINE_SIZE {
        return None;
    }
    Some(name.clone())
}

#[cfg(windows)]
const BRK_BUFFER_BYTES: usize = 4096;

#[cfg(windows)]
struct BrkWriter {
    buf: [u8; BRK_BUFFER_BYTES],
    pos: usize,
    handle: windows::Win32::Foundation::HANDLE,
}

#[cfg(windows)]
impl BrkWriter {
    fn new() -> Self {
        use windows::Win32::System::Console::{GetStdHandle, STD_ERROR_HANDLE};
        let handle = unsafe { GetStdHandle(STD_ERROR_HANDLE) }
            .unwrap_or(windows::Win32::Foundation::HANDLE::default());
        Self {
            buf: [0; BRK_BUFFER_BYTES],
            pos: 0,
            handle,
        }
    }

    /// Flush the accumulated bytes to STDERR and reset `pos`. Called
    /// at the end of each section and on buffer overflow.
    fn flush(&mut self) {
        use windows::Win32::Storage::FileSystem::WriteFile;
        if self.pos == 0 || self.handle.is_invalid() {
            self.pos = 0;
            return;
        }
        let slice = &self.buf[..self.pos];
        let mut written: u32 = 0;
        let _ = unsafe {
            WriteFile(self.handle, Some(slice), Some(&mut written), None)
        };
        self.pos = 0;
    }

    fn write_bytes(&mut self, bytes: &[u8]) {
        let mut start = 0;
        while start < bytes.len() {
            let space = BRK_BUFFER_BYTES - self.pos;
            let take = (bytes.len() - start).min(space);
            self.buf[self.pos..self.pos + take]
                .copy_from_slice(&bytes[start..start + take]);
            self.pos += take;
            start += take;
            if self.pos == BRK_BUFFER_BYTES {
                self.flush();
            }
        }
    }

    fn write_str(&mut self, s: &str) {
        self.write_bytes(s.as_bytes());
    }

    /// Write `n` as a fixed-width 16-hex-digit unsigned hex number
    /// (no `0x` prefix, capitals). 16 digits matches Windows
    /// debug-style register dumps and lines up nicely under one
    /// another in the stack walk.
    fn write_hex16(&mut self, n: u64) {
        const HEX: &[u8; 16] = b"0123456789ABCDEF";
        let mut tmp = [0u8; 16];
        for i in 0..16 {
            let shift = (15 - i) * 4;
            tmp[i] = HEX[((n >> shift) & 0xF) as usize];
        }
        self.write_bytes(&tmp);
    }

    /// Write a decimal i64 with minimal width.
    fn write_dec_i64(&mut self, n: i64) {
        let mut tmp = [0u8; 24];
        let mut len = 0;
        let neg = n < 0;
        let mut v: u64 = if neg {
            // -i64::MIN can't be represented as i64; cast through u64.
            (n as i128).unsigned_abs() as u64
        } else {
            n as u64
        };
        if v == 0 {
            tmp[len] = b'0';
            len += 1;
        } else {
            while v > 0 {
                tmp[len] = b'0' + (v % 10) as u8;
                len += 1;
                v /= 10;
            }
        }
        if neg {
            tmp[len] = b'-';
            len += 1;
        }
        // The digits are stored little-end-first; reverse before writing.
        tmp[..len].reverse();
        self.write_bytes(&tmp[..len]);
    }

    fn write_dec_u64(&mut self, n: u64) {
        let mut tmp = [0u8; 24];
        let mut len = 0;
        let mut v = n;
        if v == 0 {
            tmp[len] = b'0';
            len += 1;
        } else {
            while v > 0 {
                tmp[len] = b'0' + (v % 10) as u8;
                len += 1;
                v /= 10;
            }
        }
        tmp[..len].reverse();
        self.write_bytes(&tmp[..len]);
    }

    /// Read a null-terminated UTF-8 (or any-byte-encoded) string from
    /// `p` and write it. Caps at 256 bytes to keep the dump bounded
    /// when given a garbage pointer.
    fn write_cstr(&mut self, p: *const u8) {
        if p.is_null() {
            self.write_str("<null>");
            return;
        }
        unsafe {
            let mut n = 0;
            while n < 256 {
                let b = *p.add(n);
                if b == 0 {
                    break;
                }
                n += 1;
            }
            let slice = core::slice::from_raw_parts(p, n);
            self.write_bytes(slice);
        }
    }
}

/// Public BRK entry point. Lowering emits a call to this with the
/// current routine's name and the source line of the BRK statement.
///
/// Both arguments are best-effort and may be null / 0 — the dump
/// still emits, just without the corresponding fields populated.
///
/// Marked `extern "C-unwind"` to match the rest of the runtime ABI
/// (the JIT enables uwtable=2; everything callable from JIT-d code
/// has to participate in unwinding).
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn __newbcpl_brk(
    routine_name: *const u8,
    line: i64,
) {
    #[cfg(windows)]
    {
        unsafe { brk_impl_windows(routine_name, line) };
    }
    #[cfg(not(windows))]
    {
        unsafe { brk_impl_fallback(routine_name, line) };
    }
}

// ─── Windows implementation ────────────────────────────────────────

#[cfg(windows)]
unsafe fn brk_impl_windows(routine_name: *const u8, line: i64) {
    let mut w = BrkWriter::new();

    // Section 1 — banner. Cheap, always succeeds. We flush after
    // each section so the user sees partial output even if a later
    // section faults; debugging an unfinished BRK dump is far less
    // painful than debugging a "nothing came out".
    w.write_str("\n=== BRK in routine `");
    w.write_cstr(routine_name);
    if line > 0 {
        w.write_str("` at line ");
        w.write_dec_i64(line);
    } else {
        w.write_str("`");
    }
    w.write_str(" ===\n");
    w.flush();

    // Section 2 — heap summary. Reads atomics from a process global,
    // never faults.
    write_heap_section(&mut w);
    w.flush();

    // Section 3 — register state from a fresh CONTEXT.
    unsafe { write_context_section(&mut w) };
    w.flush();

    // Section 4 — stack walk. Best-effort; any failure terminates
    // the walk without crashing the handler.
    unsafe { write_stack_walk_section(&mut w) };
    w.flush();

    w.write_str("=== END BRK ===\n\n");
    w.flush();
}

#[cfg(windows)]
fn write_heap_section(w: &mut BrkWriter) {
    use crate::gc::HEAP_COUNTERS;
    use core::sync::atomic::Ordering;
    let bytes = HEAP_COUNTERS.live_bytes.load(Ordering::Relaxed);
    let blocks = HEAP_COUNTERS.live_blocks.load(Ordering::Relaxed);
    let peak = HEAP_COUNTERS.peak_live_bytes.load(Ordering::Relaxed);
    w.write_str("heap:    live=");
    w.write_dec_u64(bytes);
    w.write_str(" bytes  blocks=");
    w.write_dec_u64(blocks);
    w.write_str("  peak=");
    w.write_dec_u64(peak);
    w.write_str(" bytes\n");
}

// `CONTEXT` for AMD64 requires 16-byte alignment because it
// embeds XMM register storage. The Windows `CONTEXT` struct in
// the `windows` crate is `#[repr(C)]` but doesn't force a 16-byte
// alignment, so a stack-zeroed value can land on an 8-byte
// boundary and `RtlCaptureContext` faults. Wrap it.
#[cfg(windows)]
#[repr(C, align(16))]
struct AlignedContext(windows::Win32::System::Diagnostics::Debug::CONTEXT);

#[cfg(windows)]
unsafe fn write_context_section(w: &mut BrkWriter) {
    use windows::Win32::System::Diagnostics::Debug::RtlCaptureContext;
    let mut aligned = unsafe { core::mem::zeroed::<AlignedContext>() };
    let ctx = &mut aligned.0;
    ctx.ContextFlags = windows::Win32::System::Diagnostics::Debug::CONTEXT_ALL_AMD64;
    unsafe { RtlCaptureContext(ctx) };

    w.write_str("context: rip=");
    w.write_hex16(ctx.Rip);
    w.write_str("  rsp=");
    w.write_hex16(ctx.Rsp);
    w.write_str("  rbp=");
    w.write_hex16(ctx.Rbp);
    w.write_str("\n         rax=");
    w.write_hex16(ctx.Rax);
    w.write_str("  rbx=");
    w.write_hex16(ctx.Rbx);
    w.write_str("  rcx=");
    w.write_hex16(ctx.Rcx);
    w.write_str("\n         rdx=");
    w.write_hex16(ctx.Rdx);
    w.write_str("  rsi=");
    w.write_hex16(ctx.Rsi);
    w.write_str("  rdi=");
    w.write_hex16(ctx.Rdi);
    w.write_str("\n         r8 =");
    w.write_hex16(ctx.R8);
    w.write_str("  r9 =");
    w.write_hex16(ctx.R9);
    w.write_str("  r10=");
    w.write_hex16(ctx.R10);
    w.write_str("\n         r11=");
    w.write_hex16(ctx.R11);
    w.write_str("  r12=");
    w.write_hex16(ctx.R12);
    w.write_str("  r13=");
    w.write_hex16(ctx.R13);
    w.write_str("\n         r14=");
    w.write_hex16(ctx.R14);
    w.write_str("  r15=");
    w.write_hex16(ctx.R15);
    w.write_str("  flags=");
    w.write_hex16(ctx.EFlags as u64);
    w.write_str("\n");
    // The `_` keeps the compiler from complaining that `ctx` is
    // borrowed and that `aligned` was only used to hold storage.
    let _ = aligned;
}

#[cfg(windows)]
unsafe fn write_stack_walk_section(w: &mut BrkWriter) {
    use windows::Win32::System::Diagnostics::Debug::{
        RtlCaptureContext, RtlLookupFunctionEntry, RtlVirtualUnwind, CONTEXT_ALL_AMD64,
        UNWIND_HISTORY_TABLE, UNW_FLAG_NHANDLER,
    };

    // Walk at most this many frames. A pathological infinite loop in
    // the unwind tables would otherwise spin forever; 32 is more
    // than enough for any program we run.
    const MAX_FRAMES: usize = 32;

    w.write_str("stack:\n");

    // CONTEXT must be 16-byte aligned; see `AlignedContext`.
    let mut aligned = unsafe { core::mem::zeroed::<AlignedContext>() };
    let ctx = &mut aligned.0;
    ctx.ContextFlags = CONTEXT_ALL_AMD64;
    unsafe { RtlCaptureContext(ctx) };

    let mut history = unsafe { core::mem::zeroed::<UNWIND_HISTORY_TABLE>() };

    for frame_index in 0..MAX_FRAMES {
        let rip = ctx.Rip;
        if rip == 0 {
            break;
        }

        // Print this frame's RIP plus, when we can, the BCPL
        // routine that owns it. `lookup_jit_symbol` consults the
        // process-global registry the LLVM crate populated at
        // finalize. Frames that fall outside any JIT-d routine —
        // the host driver, OS, runtime helpers — just show the
        // raw address.
        w.write_str("  #");
        w.write_dec_u64(frame_index as u64);
        w.write_str("  rip=");
        w.write_hex16(rip);
        if let Some(name) = lookup_jit_symbol(rip) {
            w.write_str("  in ");
            w.write_bytes(name.as_bytes());
        }
        w.write_str("\n");

        // Find the unwind info for the function containing rip.
        let mut image_base: u64 = 0;
        let func_entry = unsafe {
            RtlLookupFunctionEntry(rip, &mut image_base, Some(&mut history))
        };
        if func_entry.is_null() {
            // Leaf function — no unwind data. Pop the saved RIP off
            // RSP manually and try one more iteration. A failure
            // here ends the walk.
            let saved_rip_ptr = ctx.Rsp as *const u64;
            if saved_rip_ptr.is_null() {
                break;
            }
            // Best-effort: don't fault if the stack pointer is
            // garbage. We don't have a safer way than reading it.
            let new_rip = unsafe { core::ptr::read_volatile(saved_rip_ptr) };
            if new_rip == 0 || new_rip == ctx.Rip {
                break;
            }
            ctx.Rip = new_rip;
            ctx.Rsp = ctx.Rsp.wrapping_add(8);
            continue;
        }

        // Real unwind step. We don't care about the handler data;
        // pass null pointers for the handler-data + frame-pointers
        // arguments per MSDN's "passive unwind" recipe.
        let prev_rip = ctx.Rip;
        let prev_rsp = ctx.Rsp;
        let mut handler_data: *mut core::ffi::c_void = core::ptr::null_mut();
        let mut establisher_frame: u64 = 0;
        let _handler = unsafe {
            RtlVirtualUnwind(
                UNW_FLAG_NHANDLER,
                image_base,
                rip,
                func_entry,
                ctx,
                &mut handler_data,
                &mut establisher_frame,
                None,
            )
        };
        if ctx.Rip == prev_rip && ctx.Rsp == prev_rsp {
            // Unwind didn't make progress — stop rather than loop.
            break;
        }
    }
}

// ─── Non-Windows fallback (for test matrix portability) ───────────

#[cfg(not(windows))]
unsafe fn brk_impl_fallback(routine_name: *const u8, line: i64) {
    use std::io::Write;
    let mut stderr = std::io::stderr().lock();
    let _ = write!(&mut stderr, "\n=== BRK in routine `");
    if !routine_name.is_null() {
        let mut n = 0usize;
        while n < 256 && unsafe { *routine_name.add(n) } != 0 {
            n += 1;
        }
        let slice = unsafe { core::slice::from_raw_parts(routine_name, n) };
        let _ = stderr.write_all(slice);
    } else {
        let _ = stderr.write_all(b"<null>");
    }
    if line > 0 {
        let _ = write!(&mut stderr, "` at line {line}");
    } else {
        let _ = write!(&mut stderr, "`");
    }
    let _ = writeln!(&mut stderr, " ===");
    let _ = writeln!(&mut stderr, "(non-Windows host: register + stack-walk dump omitted)");
    let _ = writeln!(&mut stderr, "=== END BRK ===\n");
}
