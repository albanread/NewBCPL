// The address of a Rust function-item is exactly the symbol's
// runtime entry point, which is what we hand to MCJIT. The
// `direct cast of function item to integer` warning suggests using
// `addr_of!` or a typed function-pointer first; for our purposes
// the direct cast is fine and considerably less verbose.
#![allow(unpredictable_function_pointer_comparisons)]

//! BCPL builtin runtime functions.
//!
//! Exposed with C ABI and `#[no_mangle]` so LLVM-emitted code can
//! call them directly. Each routine returns `i64` even when it
//! conceptually returns nothing — that matches BCPL convention. The
//! float-returning helpers (`FSIN`, `FRND`, ...) return `f64`.
//!
//! Strings come in as `*const u8` — a UTF-8 / ASCII byte sequence
//! produced by our compiler's read-only data segment. The reference
//! BCPL runtime uses UTF-32; we'll cross that bridge if/when the
//! corpus actually exercises non-ASCII paths.

use std::ffi::CStr;
use std::io::Read as _;
use std::io::Write as _;
use std::sync::Mutex;

/// Optional callback installed by the GUI driver so console output
/// can be redirected away from the host-process stdout. When set,
/// every byte produced by WRITES / WRITEN / WRITEC / NEWLINE / WRITEF
/// / FWRITE flows through this closure instead of `std::io::stdout`.
/// The callback runs on whichever thread the writing builtin is
/// called from — typically the JIT thread — so any cross-thread
/// marshalling is the callback's job.
type ConsoleCallback = Box<dyn Fn(&[u8]) + Send + Sync + 'static>;

static CONSOLE_CALLBACK: Mutex<Option<ConsoleCallback>> = Mutex::new(None);

/// Install a function that receives every byte the BCPL console
/// builtins would otherwise write to stdout. Pass `None` to remove
/// the callback and restore stdout-direct writes. Subsequent calls
/// replace the previous callback.
pub fn set_console_write_callback<F>(f: Option<F>)
where
    F: Fn(&[u8]) + Send + Sync + 'static,
{
    let mut slot = CONSOLE_CALLBACK
        .lock()
        .expect("CONSOLE_CALLBACK mutex poisoned");
    *slot = f.map(|cb| -> ConsoleCallback { Box::new(cb) });
}

// ─── primitive I/O ────────────────────────────────────────────────

/// `WRITES("foo*N")` — print a null-terminated UTF-8 string. The
/// string lives in the LLVM module's read-only data segment; sema
/// already cooked the BCPL `*N` / `*T` etc. escapes when emitting
/// the global.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn WRITES(s: *const u8) -> i64 {
    if s.is_null() {
        return 0;
    }
    let cstr = unsafe { CStr::from_ptr(s as *const i8) };
    write_bytes(cstr.to_bytes());
    0
}

/// `WRITEN(n)` — print a signed integer in decimal.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn WRITEN(n: i64) -> i64 {
    let s = format!("{n}");
    write_bytes(s.as_bytes());
    0
}

/// `WRITEC(c)` — print a single character (low byte of `c`).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn WRITEC(c: i64) -> i64 {
    let byte = (c & 0xff) as u8;
    write_bytes(&[byte]);
    0
}

/// `NEWLINE()` — print a single newline.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn NEWLINE() -> i64 {
    write_bytes(b"\n");
    0
}

/// `FWRITE(f)` — print a double in the reference's `%f` style.
/// Reference name is `FWRITE`; the corpus also uses it heavily.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn FWRITE(f: f64) -> i64 {
    let s = format!("{f}");
    write_bytes(s.as_bytes());
    0
}

/// `RDCH()` — read one byte from stdin; -1 on EOF.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn RDCH() -> i64 {
    let mut buf = [0u8; 1];
    match std::io::stdin().read(&mut buf) {
        Ok(0) | Err(_) => -1,
        Ok(_) => buf[0] as i64,
    }
}

/// `FINISH` — terminate the program cleanly. Inside our JIT host
/// this exits the *host* process — that's deliberate: BCPL `FINISH`
/// is "stop the program," and the JIT'd program is the host's only
/// purpose at that moment. `test-folder` runs each program in its
/// own subprocess so this is harmless.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn FINISH() -> i64 {
    let _ = std::io::stdout().flush();
    std::process::exit(0);
}

/// Stub that fills any vtable slot whose class doesn't supply a
/// method body — most commonly the default `CREATE` / `RELEASE`
/// that classes inherit without overriding. Returns 0 (BCPL routine
/// convention). Without this, a virtual call into such a slot
/// jumps to address 0 and segfaults; with it the call is a clean
/// no-op. The JIT-side vtable patcher in `newbcpl-llvm/lib.rs`
/// writes this address into every entry whose `defining_class`
/// is `None`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __newbcpl_default_method() -> i64 {
    0
}

/// `GC()` — request a full collection right now. Useful for
/// tests and benchmarks; in normal use the allocator triggers
/// collection on a heap-pressure threshold so most programs
/// never need to call this. Returns 0 by BCPL convention.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn GC() -> i64 {
    crate::gc::collect();
    0
}

/// `HEAP_INFO()` — print a one-screen summary of the GC's state
/// to stdout. Intended for interactive debugging and quick
/// instrumentation in tests; use `gc::snapshot()` from Rust for
/// programmatic access.
///
/// The shape mirrors the reference's `print_runtime_metrics` —
/// allocation counts, live working set, collection cycles, and
/// the current threshold the allocator will trigger at. Nothing
/// here changes program state.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn HEAP_INFO() -> i64 {
    use std::sync::atomic::Ordering;
    let c = &crate::gc::HEAP_COUNTERS;
    let alloc_blocks = c.alloc_blocks_lifetime.load(Ordering::Acquire);
    let alloc_bytes = c.alloc_bytes_lifetime.load(Ordering::Acquire);
    let free_blocks = c.free_blocks_lifetime.load(Ordering::Acquire);
    let free_bytes = c.free_bytes_lifetime.load(Ordering::Acquire);
    let live_blocks = c.live_blocks.load(Ordering::Acquire);
    let live_bytes = c.live_bytes.load(Ordering::Acquire);
    let peak_live = c.peak_live_bytes.load(Ordering::Acquire);
    let cycles = c.collect_cycles.load(Ordering::Acquire);
    let last_reclaimed = c.collect_last_reclaimed_bytes.load(Ordering::Acquire);
    let bytes_since = c.alloc_bytes_since_collect.load(Ordering::Acquire);
    let threshold = c.collect_threshold.load(Ordering::Acquire);
    let clusters = c.cluster_count.load(Ordering::Acquire);
    let modules = c.module_root_count.load(Ordering::Acquire);
    let threads = c.registered_threads.load(Ordering::Acquire);

    let stdout = std::io::stdout();
    let mut h = stdout.lock();
    let _ = writeln!(h, "=== newbcpl GC heap info ===");
    let _ = writeln!(
        h,
        "  allocations:  {alloc_blocks:>10} blocks  {alloc_bytes:>14} bytes (lifetime)"
    );
    let _ = writeln!(
        h,
        "  freed:        {free_blocks:>10} blocks  {free_bytes:>14} bytes (lifetime)"
    );
    let _ = writeln!(
        h,
        "  live:         {live_blocks:>10} blocks  {live_bytes:>14} bytes  (peak {peak_live} bytes)"
    );
    let _ = writeln!(
        h,
        "  collects:     {cycles:>10} cycles  last reclaimed {last_reclaimed} bytes"
    );
    let _ = writeln!(
        h,
        "  trigger:      {bytes_since}/{threshold} bytes since last collect"
    );
    let _ = writeln!(
        h,
        "  clusters: {clusters}  module roots: {modules}  registered threads: {threads}"
    );
    let _ = h.flush();
    0
}

// ─── WRITEF / WRITEF1..WRITEF7 ────────────────────────────────────

/// `WRITEF` and its arity-suffixed siblings are the BCPL printf.
/// Format spec subset (matches the reference):
///   %d %i %N — signed decimal
///   %x       — lowercase hex
///   %X       — 16-wide uppercase hex
///   %o       — octal
///   %c       — single character
///   %s       — null-terminated string
///   %f %F    — double (the i64 arg's bits reinterpreted as f64)
///   %%       — literal '%'
/// Any other `%X` is printed as-is. The `*N` / `*T` escapes have
/// already been baked into the literal bytes by sema.
fn writef_impl(format: *const u8, args: &[i64]) {
    if format.is_null() {
        write_bytes(b"(null format)");
        return;
    }
    let cstr = unsafe { CStr::from_ptr(format as *const i8) };
    let bytes = cstr.to_bytes();
    // Build into an in-memory buffer so the whole formatted line
    // reaches `write_bytes` as one chunk — that's important for the
    // GUI callback's line-buffer flush logic, and harmless for the
    // stdout path where we'd be locking once anyway.
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len() + args.len() * 8);

    let mut i = 0;
    let mut used = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'%' && i + 1 < bytes.len() {
            let spec = bytes[i + 1];
            i += 2;
            if spec == b'%' {
                out.push(b'%');
                continue;
            }
            if used >= args.len() {
                let _ = write!(out, "%{}", spec as char);
                continue;
            }
            let a = args[used];
            used += 1;
            match spec {
                b'd' | b'i' | b'N' => {
                    let _ = write!(out, "{a}");
                }
                b'x' => {
                    let _ = write!(out, "{:x}", a as u64);
                }
                b'X' => {
                    let _ = write!(out, "{:016X}", a as u64);
                }
                b'o' => {
                    let _ = write!(out, "{:o}", a as u64);
                }
                b'c' => {
                    out.push((a & 0xff) as u8);
                }
                b's' => {
                    if a == 0 {
                        out.extend_from_slice(b"(null)");
                    } else {
                        let s = unsafe { CStr::from_ptr(a as *const i8) };
                        out.extend_from_slice(s.to_bytes());
                    }
                }
                b'f' | b'F' => {
                    let f = f64::from_bits(a as u64);
                    let _ = write!(out, "{f}");
                }
                other => {
                    let _ = write!(out, "%{}", other as char);
                    used -= 1; // unknown specifier doesn't consume the arg
                }
            }
        } else {
            out.push(b);
            i += 1;
        }
    }
    write_bytes(&out);
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn WRITEF(fmt: *const u8) -> i64 {
    writef_impl(fmt, &[]);
    0
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn WRITEF1(fmt: *const u8, a1: i64) -> i64 {
    writef_impl(fmt, &[a1]);
    0
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn WRITEF2(fmt: *const u8, a1: i64, a2: i64) -> i64 {
    writef_impl(fmt, &[a1, a2]);
    0
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn WRITEF3(fmt: *const u8, a1: i64, a2: i64, a3: i64) -> i64 {
    writef_impl(fmt, &[a1, a2, a3]);
    0
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn WRITEF4(fmt: *const u8, a1: i64, a2: i64, a3: i64, a4: i64) -> i64 {
    writef_impl(fmt, &[a1, a2, a3, a4]);
    0
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn WRITEF5(
    fmt: *const u8,
    a1: i64,
    a2: i64,
    a3: i64,
    a4: i64,
    a5: i64,
) -> i64 {
    writef_impl(fmt, &[a1, a2, a3, a4, a5]);
    0
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn WRITEF6(
    fmt: *const u8,
    a1: i64,
    a2: i64,
    a3: i64,
    a4: i64,
    a5: i64,
    a6: i64,
) -> i64 {
    writef_impl(fmt, &[a1, a2, a3, a4, a5, a6]);
    0
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn WRITEF7(
    fmt: *const u8,
    a1: i64,
    a2: i64,
    a3: i64,
    a4: i64,
    a5: i64,
    a6: i64,
    a7: i64,
) -> i64 {
    writef_impl(fmt, &[a1, a2, a3, a4, a5, a6, a7]);
    0
}

// ─── float math (libm-equivalent) ─────────────────────────────────

#[unsafe(no_mangle)]
pub unsafe extern "C" fn FSIN(x: f64) -> f64 {
    x.sin()
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn FCOS(x: f64) -> f64 {
    x.cos()
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn FTAN(x: f64) -> f64 {
    x.tan()
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn FABS(x: f64) -> f64 {
    x.abs()
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn FLOG(x: f64) -> f64 {
    x.ln()
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn FEXP(x: f64) -> f64 {
    x.exp()
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn FIX(x: f64) -> i64 {
    x.trunc() as i64
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn FSQRT(x: f64) -> f64 {
    x.sqrt()
}

/// `FLOAT(n)` — explicit int-to-float conversion. The reference's
/// `FLOAT` is a built-in coercion (`(double)n`); BCPL programs use
/// it whenever a float result is wanted from integer arithmetic.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn FLOAT(n: i64) -> f64 {
    n as f64
}

// ─── random number generation ────────────────────────────────────

/// Tiny deterministic PRNG: xorshift64 seeded from the address of a
/// static. Adequate for the corpus's RND/RAND/FRND uses (mostly
/// "give me variety," not statistical work). Reseed via setting the
/// inner state if a corpus test ever needs reproducibility.
static RNG: Mutex<u64> = Mutex::new(0x9E3779B97F4A7C15);

fn next_u64() -> u64 {
    let mut s = RNG.lock().unwrap();
    let mut x = *s;
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    *s = x;
    x
}

/// `RAND(max)` — uniform integer in `[0, max]` (inclusive both ends),
/// matching the reference. `max <= 0` yields 0.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn RAND(max_val: i64) -> i64 {
    if max_val <= 0 {
        return 0;
    }
    let span = (max_val as u64).wrapping_add(1);
    (next_u64() % span) as i64
}

/// `FRND()` — uniform double in `[0, 1)`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn FRND() -> f64 {
    // Build a [0, 1) double from the top 53 bits.
    let bits = next_u64() >> 11;
    bits as f64 / (1u64 << 53) as f64
}

/// `RND(max)` — uniform double in `[0, max)` (per the reference's
/// loose contract).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn RND(max_val: i64) -> f64 {
    if max_val <= 0 {
        return 0.0;
    }
    FRND() * (max_val as f64)
}

// ─── list runtime (real linked-list shape) ──────────────────────
//
// Mirrors `reference/runtime/ListDataTypes.h` so the layout the JIT
// emits and the layout the runtime expects agree byte-for-byte.
// Allocations currently come from `Box::leak` (same shape as our
// vector / pair stubs); routing list nodes through the GC heap
// alongside class instances is a clearly-labelled follow-up.

/// One node of a BCPL list. The `type` tag describes which member of
/// the value union is live (`ATOM_INT`, `ATOM_FLOAT`, `ATOM_PAIR`,
/// `ATOM_OBJECT`, ...). The reference reserves slot 0 for the
/// header (`ATOM_SENTINEL`) so a stray walk through `next` past the
/// last data node lands on the header rather than wild memory.
#[repr(C)]
pub struct ListAtom {
    pub type_tag: i32,
    pub pad: i32,
    /// Untagged 64-bit value slot. Holds an `i64` for `ATOM_INT`,
    /// the bit pattern of an `f64` for `ATOM_FLOAT`, or a raw
    /// pointer for `ATOM_STRING` / `ATOM_OBJECT` / `ATOM_LIST_POINTER`.
    /// PAIR/QUAD/OCT (all 64-bit packed) round-trip through here too.
    pub value: i64,
    pub next: *mut ListAtom,
}

/// The header that every list points to. `head` / `tail` are atoms,
/// `length` is maintained on each append for O(1) `LEN`. The `type`
/// field is always `ATOM_SENTINEL` so code walking through a chain
/// can detect the header bookend.
#[repr(C)]
pub struct ListHeader {
    pub type_tag: i32,
    pub contains_literals: i32,
    pub length: i64,
    pub head: *mut ListAtom,
    pub tail: *mut ListAtom,
}

/// Atom type tags — must match `reference/runtime/ListDataTypes.h`.
#[allow(dead_code)]
pub const ATOM_SENTINEL: i32 = 0;
pub const ATOM_INT: i32 = 1;
pub const ATOM_FLOAT: i32 = 2;
pub const ATOM_STRING: i32 = 3;
#[allow(dead_code)]
pub const ATOM_LIST_POINTER: i32 = 4;
pub const ATOM_OBJECT: i32 = 5;
pub const ATOM_PAIR: i32 = 6;

fn leak_list_header() -> *mut ListHeader {
    let hdr = Box::new(ListHeader {
        type_tag: ATOM_SENTINEL,
        contains_literals: 0,
        length: 0,
        head: std::ptr::null_mut(),
        tail: std::ptr::null_mut(),
    });
    Box::leak(hdr) as *mut ListHeader
}

fn leak_atom(type_tag: i32, value: i64) -> *mut ListAtom {
    let atom = Box::new(ListAtom {
        type_tag,
        pad: 0,
        value,
        next: std::ptr::null_mut(),
    });
    Box::leak(atom) as *mut ListAtom
}

/// Create a fresh empty list. JIT-emitted `LIST(...)` construction
/// calls this once, then issues an `APND_*` per initialiser.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __newbcpl_list_new_empty() -> *mut ListHeader {
    leak_list_header()
}

fn append_atom(hdr: *mut ListHeader, atom: *mut ListAtom) {
    if hdr.is_null() || atom.is_null() {
        return;
    }
    unsafe {
        let h = &mut *hdr;
        if h.head.is_null() {
            h.head = atom;
            h.tail = atom;
        } else {
            (*h.tail).next = atom;
            h.tail = atom;
        }
        h.length += 1;
    }
}

/// `APND(list, n)` — append an integer atom to `list`. The same
/// entry point handles boolean / character / packed-word values
/// since BCPL treats every word identically at the ABI.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn APND(hdr: *mut ListHeader, value: i64) -> i64 {
    append_atom(hdr, leak_atom(ATOM_INT, value));
    0
}

/// Float-typed append (BCPL `FPND` in the reference; aliased to
/// `APND_FLOAT` for our emit's per-arg type dispatch). The value
/// comes in as `f64`; we reinterpret-store its bits in the atom's
/// `i64` value slot.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn APND_FLOAT(hdr: *mut ListHeader, value: f64) -> i64 {
    append_atom(hdr, leak_atom(ATOM_FLOAT, value.to_bits() as i64));
    0
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn APND_STRING(hdr: *mut ListHeader, ptr: *const u8) -> i64 {
    append_atom(hdr, leak_atom(ATOM_STRING, ptr as i64));
    0
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn APND_OBJECT(hdr: *mut ListHeader, ptr: *const u8) -> i64 {
    append_atom(hdr, leak_atom(ATOM_OBJECT, ptr as i64));
    0
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn APND_PAIR(hdr: *mut ListHeader, packed: i64) -> i64 {
    append_atom(hdr, leak_atom(ATOM_PAIR, packed));
    0
}

/// `CONCAT(a, b)` — return a fresh list whose atoms are a copy of
/// `a` followed by a copy of `b`. The reference's
/// `BCPL_CONCAT_LISTS` is destructive (rewires `a.tail.next`);
/// we copy for safety since neither header is GC-tracked yet.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn CONCAT(a: *mut ListHeader, b: *mut ListHeader) -> *mut ListHeader {
    let result = leak_list_header();
    for src in [a, b] {
        if src.is_null() {
            continue;
        }
        let mut cur = unsafe { (*src).head };
        while !cur.is_null() {
            let (tt, v) = unsafe { ((*cur).type_tag, (*cur).value) };
            append_atom(result, leak_atom(tt, v));
            cur = unsafe { (*cur).next };
        }
    }
    result
}

// ─── vector / list runtime helpers ───────────────────────────────
//
// BCPL convention: a vector of N words is allocated as N+1 words.
// The length sits at offset `-1` (one word *before* the returned
// data pointer); `__newbcpl_len` reads it.
//
// `PAIR` is a *value*, not a heap object — it's two i32 lanes
// packed into one i64 word and lives in a register. `PAIRS(N)` is
// just an array of N such words intended for SIMD operations
// (16-byte aligned in the reference; here we get 8-byte alignment
// from `vec![i64]`, which is fine for unaligned vector loads on
// modern x86_64 but a known TODO for proper alignment).
//
// These implementations leak today — they're unblocking stubs
// until the GC integration lands.

/// Allocate a fresh, zeroed slab of `(n_words + 1) * 8` bytes on
/// the **GC heap** via the size-keyed allocator, store `n_words`
/// into the first slot, and return a pointer one word past the
/// start so subscripts `p!0..p!(n-1)` and `__newbcpl_len(p)`
/// (which reads `p[-1]`) both work.
///
/// Heap allocation is correct even when the caller is a JIT'd
/// routine — see `__newbcpl_alloc_rec` for the runtime-side
/// TypeDesc-interning that keeps `BlockHeader.tag` pointers
/// stable across JIT engine drops.
fn alloc_vec_words(n_words: i64) -> *mut i64 {
    let n = n_words.max(0) as usize;
    let total_bytes = (n + 1) * std::mem::size_of::<i64>();
    let raw = unsafe { __newbcpl_alloc_rec(total_bytes as i64) } as *mut i64;
    // The GC zero-initialises the block; we just stamp the length.
    unsafe {
        *raw = n_words;
        raw.add(1)
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn GETVEC(n_words: i64) -> *mut i64 {
    alloc_vec_words(n_words)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn FGETVEC(n_words: i64) -> *mut i64 {
    alloc_vec_words(n_words)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PAIRS(n_pairs: i64) -> *mut i64 {
    alloc_vec_words(n_pairs)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn FPAIRS(n_pairs: i64) -> *mut i64 {
    alloc_vec_words(n_pairs)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn FREEVEC(_p: *mut i64) -> i64 {
    // Leak — proper free needs the GC's metadata. Tests don't
    // assert on memory pressure yet, so this is fine for now.
    0
}

/// `__newbcpl_len(p)` — vector length, read from the word *before*
/// the data pointer (BCPL convention). Used for VEC / FVEC /
/// PAIRS. Lists go through `__newbcpl_list_len` because their
/// length lives at a different offset inside a `ListHeader`.
/// Returns 0 for null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __newbcpl_len(p: *const i64) -> i64 {
    if p.is_null() {
        return 0;
    }
    unsafe { *p.offset(-1) }
}

/// `__newbcpl_list_len(header)` — length of a real `ListHeader`,
/// O(1) (the length is maintained on every append). Returns 0 for
/// null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __newbcpl_list_len(hdr: *const ListHeader) -> i64 {
    if hdr.is_null() {
        return 0;
    }
    unsafe { (*hdr).length }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn __newbcpl_freevec(_p: *mut i64) -> i64 {
    0
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn __newbcpl_freelist(_p: *mut i64) -> i64 {
    0
}

// ─── runtime-interned TypeDescs for safe `collect()` ─────────────
//
// See `docs/jit_typedesc_lifetime.md` for the long form. Short
// version: TypeDesc constants in the JIT module's data section
// die when the ExecutionEngine drops, but BlockHeader.tag pointers
// in the GC heap survive — calling `collect()` afterwards reads
// freed memory. Fix B: keep TypeDescs on the runtime side, hand
// their stable address to `__newbcpl_new_rec`. This indirection
// is `__newbcpl_alloc_rec(size)` — the JIT'd `NEW Class` site
// passes the static instance size from the class layout, the
// runtime interns a TypeDesc per size, and the BlockHeader's tag
// points into newbcpl-runtime statics that live for the whole
// process.

/// Layout-frozen mirror of `gc::TypeDesc` plus a single i64 that
/// serves as the `ptroffs` sentinel. Allocating these via
/// `Box::leak` gives a stable address that the GC can stamp into
/// `BlockHeader.tag` without worrying about JIT engine drops.
#[repr(C)]
struct RuntimeTypeDesc {
    size: isize,
    module: *const u8,
    finalizer: *const u8,
    base: *const u8,
    vtable: *const u8,
    vtable_len: u64,
    name: *const u32,
    /// `[isize; 1]` immediately after the TypeDesc fields, set to
    /// `-1` so the GC's pointer-offset iterator stops without
    /// reading any further memory. No traced fields today.
    ptroffs_sentinel: isize,
}

unsafe impl Sync for RuntimeTypeDesc {}
unsafe impl Send for RuntimeTypeDesc {}

fn intern_typedesc_for_size(size: usize) -> *const crate::gc::TypeDesc {
    use std::collections::HashMap;
    use std::sync::Mutex;
    use std::sync::OnceLock;

    // The map stores the leaked TypeDesc address as a `usize` so
    // the inner type stays `Send + Sync`. We cast back to a
    // `*const TypeDesc` on the way out.
    static CACHE: OnceLock<Mutex<HashMap<usize, usize>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let mut guard = cache.lock().expect("typedesc cache poisoned");
    if let Some(&existing) = guard.get(&size) {
        return existing as *const crate::gc::TypeDesc;
    }
    let boxed = Box::new(RuntimeTypeDesc {
        size: size as isize,
        module: std::ptr::null(),
        finalizer: std::ptr::null(),
        base: std::ptr::null(),
        vtable: std::ptr::null(),
        vtable_len: 0,
        name: std::ptr::null(),
        ptroffs_sentinel: -1,
    });
    let leaked = Box::leak(boxed) as *const RuntimeTypeDesc as *const crate::gc::TypeDesc;
    guard.insert(size, leaked as usize);
    leaked
}

/// `__newbcpl_alloc_rec(size)` — heap-allocate a record of `size`
/// payload bytes via the GC. Takes a plain integer instead of a
/// JIT-emitted TypeDesc address so the BlockHeader's tag points
/// to runtime-interned storage that survives JIT engine drops.
/// The JIT'd `NEW Class` site passes
/// `layout.instance_size` from sema.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __newbcpl_alloc_rec(size: i64) -> *mut u8 {
    let size = size.max(0) as usize;
    let td = intern_typedesc_for_size(size);
    unsafe { crate::gc::__newbcpl_new_rec(td) }
}

/// `HD(list)` — read the value of the first atom. Returns 0 if
/// the list is null or empty. The atom's `type_tag` is ignored
/// here; BCPL treats every value as a 64-bit word at the call
/// site, with the caller responsible for interpretation (`HD` of
/// a list-of-pairs is an i64-packed PAIR, etc.).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __newbcpl_list_hd(hdr: *const ListHeader) -> i64 {
    if hdr.is_null() {
        return 0;
    }
    let head = unsafe { (*hdr).head };
    if head.is_null() {
        return 0;
    }
    unsafe { (*head).value }
}

/// `TL(list)` — return a fresh header whose contents are every
/// atom after the head, sharing the same nodes. The original
/// list is unmodified. Returns null for empty / null input.
///
/// Sharing nodes is a deliberate choice: BCPL `tl` is the
/// constant-time list spine — copying every node would change
/// O(1) into O(n). When the GC migration of list nodes lands,
/// the sharing is still safe because we mark via the head
/// pointer's reachability.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __newbcpl_list_tl(hdr: *mut ListHeader) -> *mut ListHeader {
    if hdr.is_null() {
        return std::ptr::null_mut();
    }
    let head = unsafe { (*hdr).head };
    if head.is_null() {
        return std::ptr::null_mut();
    }
    let next = unsafe { (*head).next };
    if next.is_null() {
        // Empty tail — return an empty list header so callers
        // can chain `TL(TL(...))` without null checks.
        return leak_list_header();
    }
    let new_hdr = leak_list_header();
    unsafe {
        (*new_hdr).head = next;
        (*new_hdr).tail = (*hdr).tail;
        (*new_hdr).length = (*hdr).length - 1;
    }
    new_hdr
}

/// `REST(list, n)` — skip the first `n` atoms. Same sharing
/// strategy as `TL`. `n <= 0` returns the original; null in →
/// null out.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __newbcpl_list_rest(hdr: *mut ListHeader, n: i64) -> *mut ListHeader {
    if hdr.is_null() {
        return std::ptr::null_mut();
    }
    if n <= 0 {
        return hdr;
    }
    let mut cur = unsafe { (*hdr).head };
    let mut skipped = 0i64;
    while !cur.is_null() && skipped < n {
        cur = unsafe { (*cur).next };
        skipped += 1;
    }
    let new_hdr = leak_list_header();
    unsafe {
        (*new_hdr).head = cur;
        (*new_hdr).tail = (*hdr).tail;
        (*new_hdr).length = (*hdr).length - skipped;
    }
    new_hdr
}

// Function-call-form aliases of the list helpers. BCPL programs
// can write either the prefix operators (`HD list`, `TL list`) or
// the function-call form (`HD(list)`, `TAIL(list)`) — sema /
// lowering treats the function-call form as a free function so we
// expose the same addresses under those names.

#[unsafe(no_mangle)]
pub unsafe extern "C" fn HD(hdr: *const ListHeader) -> i64 {
    unsafe { __newbcpl_list_hd(hdr) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn TL(hdr: *mut ListHeader) -> *mut ListHeader {
    unsafe { __newbcpl_list_tl(hdr) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn TAIL(hdr: *mut ListHeader) -> *mut ListHeader {
    unsafe { __newbcpl_list_tl(hdr) }
}

/// `SPLIT(s, delim)` — placeholder. Real implementation needs the
/// list runtime; for now return a null pointer so callers see an
/// empty list and can at least proceed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn SPLIT(_s: *const u8, _delim: *const u8) -> *mut i64 {
    std::ptr::null_mut()
}

// `__newbcpl_new_rec` lives in `gc.rs` (the real GC-aware record
// allocator). We re-export its address through `builtin_addresses()`
// below so the JIT layer can register the symbol uniformly.

// ─── helpers ─────────────────────────────────────────────────────

fn write_bytes(bytes: &[u8]) {
    // GUI mode installs a callback that routes bytes to a console
    // window; in that case we skip the host stdout entirely so test
    // captures don't see double output. Without a callback (the
    // normal headless `run`), fall through to stdout.
    let cb = CONSOLE_CALLBACK
        .lock()
        .expect("CONSOLE_CALLBACK mutex poisoned");
    if let Some(cb) = cb.as_ref() {
        cb(bytes);
        return;
    }
    drop(cb);
    let stdout = std::io::stdout();
    let mut handle = stdout.lock();
    let _ = handle.write_all(bytes);
    let _ = handle.flush();
}

// ─── builtin-address table ───────────────────────────────────────

/// One entry per C-ABI builtin: symbol name + address as `usize`.
/// Stored as `usize` to keep the table `Send + Sync` so it can be
/// memoised inside a `OnceLock`. The JIT layer transmutes back to
/// a function pointer when registering with MCJIT.
#[derive(Debug, Clone, Copy)]
pub struct Builtin {
    pub name: &'static str,
    pub address: usize,
}

macro_rules! builtin {
    ($name:ident) => {
        Builtin {
            name: stringify!($name),
            address: $name as *const () as usize,
        }
    };
}

/// Table of every C-ABI builtin. The LLVM-emit JIT path uses this
/// to register addresses with MCJIT up front, so symbol resolution
/// doesn't depend on the host process's dynamic linker finding the
/// symbols by name.
pub fn builtin_addresses() -> &'static [Builtin] {
    use std::sync::OnceLock;
    static TABLE: OnceLock<Vec<Builtin>> = OnceLock::new();
    TABLE.get_or_init(|| {
        #[allow(unused_mut)]
        let mut v = vec![
            builtin!(WRITES),
            builtin!(WRITEN),
            builtin!(WRITEC),
            builtin!(NEWLINE),
            builtin!(FWRITE),
            builtin!(RDCH),
            builtin!(FINISH),
            builtin!(WRITEF),
            builtin!(WRITEF1),
            builtin!(WRITEF2),
            builtin!(WRITEF3),
            builtin!(WRITEF4),
            builtin!(WRITEF5),
            builtin!(WRITEF6),
            builtin!(WRITEF7),
            builtin!(FSIN),
            builtin!(FCOS),
            builtin!(FTAN),
            builtin!(FABS),
            builtin!(FLOG),
            builtin!(FEXP),
            builtin!(FIX),
            builtin!(FSQRT),
            builtin!(FLOAT),
            builtin!(HD),
            builtin!(TL),
            builtin!(TAIL),
            builtin!(RAND),
            builtin!(FRND),
            builtin!(RND),
            builtin!(GETVEC),
            builtin!(FGETVEC),
            builtin!(PAIRS),
            builtin!(FPAIRS),
            builtin!(FREEVEC),
            builtin!(SPLIT),
            builtin!(__newbcpl_len),
            builtin!(__newbcpl_list_len),
            builtin!(__newbcpl_freevec),
            builtin!(__newbcpl_freelist),
            builtin!(__newbcpl_list_hd),
            builtin!(__newbcpl_list_tl),
            builtin!(__newbcpl_list_rest),
            builtin!(__newbcpl_list_new_empty),
            builtin!(APND),
            builtin!(APND_FLOAT),
            builtin!(APND_STRING),
            builtin!(APND_OBJECT),
            builtin!(APND_PAIR),
            builtin!(CONCAT),
            Builtin {
                name: "__newbcpl_new_rec",
                address: crate::gc::__newbcpl_new_rec as *const () as usize,
            },
            Builtin {
                name: "__newbcpl_safepoint",
                address: crate::gc::__newbcpl_safepoint as *const () as usize,
            },
            builtin!(__newbcpl_alloc_rec),
            Builtin {
                name: "__newbcpl_collect",
                address: crate::gc::__newbcpl_collect as *const () as usize,
            },
            builtin!(GC),
            builtin!(HEAP_INFO),
            builtin!(__newbcpl_default_method),
        ];
        #[cfg(windows)]
        {
            use crate::igui_builtins as g;
            v.push(Builtin {
                name: "iGui_OpenChild",
                address: g::iGui_OpenChild as *const () as usize,
            });
            v.push(Builtin {
                name: "iGui_CloseChild",
                address: g::iGui_CloseChild as *const () as usize,
            });
            v.push(Builtin {
                name: "iGui_SetTitle",
                address: g::iGui_SetTitle as *const () as usize,
            });
            v.push(Builtin {
                name: "iGui_BeginBatch",
                address: g::iGui_BeginBatch as *const () as usize,
            });
            v.push(Builtin {
                name: "iGui_SubmitBatch",
                address: g::iGui_SubmitBatch as *const () as usize,
            });
            v.push(Builtin {
                name: "iGui_Clear",
                address: g::iGui_Clear as *const () as usize,
            });
            v.push(Builtin {
                name: "iGui_FillRect",
                address: g::iGui_FillRect as *const () as usize,
            });
            v.push(Builtin {
                name: "iGui_StrokeRect",
                address: g::iGui_StrokeRect as *const () as usize,
            });
            v.push(Builtin {
                name: "iGui_FillCircle",
                address: g::iGui_FillCircle as *const () as usize,
            });
            v.push(Builtin {
                name: "iGui_DrawLine",
                address: g::iGui_DrawLine as *const () as usize,
            });
            v.push(Builtin {
                name: "iGui_DrawText",
                address: g::iGui_DrawText as *const () as usize,
            });
            v.push(Builtin {
                name: "iGui_NextEvent",
                address: g::iGui_NextEvent as *const () as usize,
            });
            v.push(Builtin {
                name: "iGui_Quit",
                address: g::iGui_Quit as *const () as usize,
            });
            v.push(Builtin {
                name: "iGui_NextEventFor",
                address: g::iGui_NextEventFor as *const () as usize,
            });
            v.push(Builtin {
                name: "iGui_DiscardStashedEvents",
                address: g::iGui_DiscardStashedEvents as *const () as usize,
            });
            v.push(Builtin {
                name: "iGui_FilterOnWindow",
                address: g::iGui_FilterOnWindow as *const () as usize,
            });
            v.push(Builtin {
                name: "iGui_UnfilterWindow",
                address: g::iGui_UnfilterWindow as *const () as usize,
            });
            v.push(Builtin {
                name: "iGui_ClearFilter",
                address: g::iGui_ClearFilter as *const () as usize,
            });
            // Text-pane (terminal-grid) builtins.
            v.push(Builtin {
                name: "iGui_OpenText",
                address: g::iGui_OpenText as *const () as usize,
            });
            v.push(Builtin {
                name: "iGui_TextWriteStr",
                address: g::iGui_TextWriteStr as *const () as usize,
            });
            v.push(Builtin {
                name: "iGui_TextWriteChar",
                address: g::iGui_TextWriteChar as *const () as usize,
            });
            v.push(Builtin {
                name: "iGui_TextNewline",
                address: g::iGui_TextNewline as *const () as usize,
            });
            v.push(Builtin {
                name: "iGui_TextSetCursor",
                address: g::iGui_TextSetCursor as *const () as usize,
            });
            v.push(Builtin {
                name: "iGui_TextClear",
                address: g::iGui_TextClear as *const () as usize,
            });
            v.push(Builtin {
                name: "iGui_TextClearEol",
                address: g::iGui_TextClearEol as *const () as usize,
            });
            v.push(Builtin {
                name: "iGui_TextClearEos",
                address: g::iGui_TextClearEos as *const () as usize,
            });
            v.push(Builtin {
                name: "iGui_TextScrollUp",
                address: g::iGui_TextScrollUp as *const () as usize,
            });
            v.push(Builtin {
                name: "iGui_TextSetPen",
                address: g::iGui_TextSetPen as *const () as usize,
            });
            v.push(Builtin {
                name: "iGui_TextResetPen",
                address: g::iGui_TextResetPen as *const () as usize,
            });
            v.push(Builtin {
                name: "iGui_TextShowCaret",
                address: g::iGui_TextShowCaret as *const () as usize,
            });
        }
        v
    })
}

/// Returns true if `name` is a builtin known to the runtime. The
/// LLVM JIT layer uses this to detect missing symbols *before*
/// running, so an unresolved builtin produces a clean error rather
/// than a SIGSEGV when the JIT'd code calls a null pointer.
pub fn is_builtin(name: &str) -> bool {
    builtin_addresses().iter().any(|b| b.name == name)
}

// ─── heap-manager exercise tests ─────────────────────────────────
//
// Ported in spirit from `reference/tests/cpp_tests/test_heap_manager.cpp`.
// The reference uses a singleton C++ `HeapManager` with typed allocators
// (`allocVec`, `allocString`, `allocObject`); ours is a precise
// mark-sweep GC with `TypeDesc`-tagged blocks. We exercise the same
// invariants: allocations are non-null, aligned, zero-initialised,
// hold their payload across read/write, and the heap counters advance.
#[cfg(test)]
mod heap_tests {
    use super::*;
    use crate::gc;

    /// All heap tests share the global GC state (`gc::HEAP_COUNTERS`,
    /// the allocator, the live-block list) with `gc::tests::*`. Cargo
    /// runs `#[test]`s in parallel by default; one test firing a
    /// stop-the-world collect while another is mid-allocation in an
    /// unregistered TLAB produced sporadic STATUS_ACCESS_VIOLATION on
    /// Windows. Serialise through the same `lock_tests_global()`
    /// guard that `gc::tests` uses so heap_tests and gc::tests can't
    /// race against each other.
    fn heap_test_lock() -> std::sync::MutexGuard<'static, ()> {
        crate::gc::lock_tests_global()
    }

    /// Build a `TypeDesc` for a `size`-byte data block. Allocated
    /// once via `Box::leak` so its address is stable across the
    /// test (the GC stamps every block's tag with this pointer).
    /// We include the `ptroffs` sentinel (-1) so the GC's pointer
    /// iterator stops immediately — these test blocks contain no
    /// traced fields.
    fn leak_typedesc(size: isize) -> *const gc::TypeDesc {
        // Layout: TypeDesc fields + one i64 sentinel.
        #[repr(C)]
        struct TypeDescPlusSentinel {
            size: isize,
            module: *const u8,
            finalizer: *const u8,
            base: *const u8,
            vtable: *const u8,
            vtable_len: u64,
            name: *const u32,
            ptroffs_sentinel: isize,
        }
        let boxed = Box::new(TypeDescPlusSentinel {
            size,
            module: std::ptr::null(),
            finalizer: std::ptr::null(),
            base: std::ptr::null(),
            vtable: std::ptr::null(),
            vtable_len: 0,
            name: std::ptr::null(),
            ptroffs_sentinel: -1,
        });
        Box::leak(boxed) as *const _ as *const gc::TypeDesc
    }

    #[test]
    fn vector_allocation_writes_and_reads_back() {
        let _guard = heap_test_lock();
        // Mirrors `test_vector_allocation` — allocate 10 words,
        // fill with a pattern, verify integrity. Our convention
        // puts the length one word *before* the data pointer, so
        // we also confirm `__newbcpl_len` returns the right value.
        let n: i64 = 10;
        let v = unsafe { GETVEC(n) };
        assert!(!v.is_null(), "GETVEC must not return null");
        assert_eq!(
            v as usize % std::mem::align_of::<i64>(),
            0,
            "data pointer must be 8-byte aligned"
        );
        for i in 0..n {
            unsafe { *v.offset(i as isize) = i * 2 };
        }
        for i in 0..n {
            assert_eq!(
                unsafe { *v.offset(i as isize) },
                i * 2,
                "write/read mismatch at slot {i}"
            );
        }
        assert_eq!(
            unsafe { __newbcpl_len(v) },
            n,
            "length header must match the requested word count"
        );
    }

    #[test]
    fn object_allocation_is_zero_initialised_and_holds_data() {
        let _guard = heap_test_lock();
        // Mirrors `test_object_allocation`: a fresh GC block of
        // 64 bytes should arrive zeroed, accept arbitrary writes,
        // and return them unchanged.
        let td = leak_typedesc(64);
        let raw = unsafe { gc::__newbcpl_new_rec(td) };
        assert!(!raw.is_null(), "__newbcpl_new_rec must not return null");
        let bytes = unsafe { std::slice::from_raw_parts(raw, 64) };
        assert!(
            bytes.iter().all(|&b| b == 0),
            "GC block must arrive zero-initialised"
        );
        let words = raw as *mut u64;
        unsafe {
            *words.add(0) = 0xDEAD_BEEF_CAFE_BABE;
            *words.add(1) = 42;
        }
        assert_eq!(
            unsafe { *words.add(0) },
            0xDEAD_BEEF_CAFE_BABE,
            "wide write must persist"
        );
        assert_eq!(
            unsafe { *words.add(1) },
            42,
            "second-slot write must persist"
        );
    }

    #[test]
    fn alloc_pressure_auto_triggers_collect() {
        let _guard = heap_test_lock();
        // Lower the auto-trigger threshold so the test doesn't
        // need to allocate the production threshold's worth of
        // memory (4 MB) to fire one cycle. We restore the
        // production value at the end so other tests in the
        // shared process see the normal threshold.
        let old_threshold = gc::HEAP_COUNTERS
            .collect_threshold
            .swap(64 * 1024, std::sync::atomic::Ordering::AcqRel);
        let cycles_before = gc::HEAP_COUNTERS
            .collect_cycles
            .load(std::sync::atomic::Ordering::Acquire);

        // Allocate well past the lowered threshold. Each block
        // is a few hundred bytes, so a few hundred allocations
        // crosses 64 KiB comfortably and the allocator path
        // should trigger at least one cycle.
        let td = leak_typedesc(256);
        for _ in 0..512 {
            let p = unsafe { gc::__newbcpl_new_rec(td) };
            assert!(!p.is_null(), "allocator must keep returning blocks");
            // Touch the block to keep it from being optimised away.
            unsafe { *(p as *mut u64) = 0xABCD };
        }
        let cycles_after = gc::HEAP_COUNTERS
            .collect_cycles
            .load(std::sync::atomic::Ordering::Acquire);
        assert!(
            cycles_after > cycles_before,
            "allocator did not trigger any collect cycles: \
             {cycles_before} → {cycles_after}"
        );

        // Restore the production threshold.
        gc::HEAP_COUNTERS
            .collect_threshold
            .store(old_threshold, std::sync::atomic::Ordering::Release);
    }

    #[test]
    fn many_allocations_succeed_and_remain_writeable() {
        let _guard = heap_test_lock();
        // Mirrors the throughput tests: allocate N blocks of a
        // size that's large enough to exercise both the cluster
        // bump and the BlockHeader bookkeeping, then write a
        // unique pattern to each and verify it on read-back.
        //
        // We don't assert against the global heap counters —
        // the counters are process-wide and shared with the
        // other GC tests in this crate (`multi_thread_alloc_no_crash`,
        // `alloc_collect_alloc`), which run in parallel and
        // can register / drop mutators between snapshots.
        // The integrity check is what proves the GC actually
        // gave us distinct, writeable, non-overlapping memory.
        let td = leak_typedesc(48);
        const N: usize = 64;
        let mut blocks: Vec<*mut u8> = Vec::with_capacity(N);
        for i in 0..N {
            let p = unsafe { gc::__newbcpl_new_rec(td) };
            assert!(!p.is_null(), "out-of-memory in test harness");
            // Stamp a unique pattern derived from the index so
            // each block is identifiable.
            unsafe {
                *(p as *mut u64) = 0xA000_0000_0000_0000 | (i as u64);
            }
            blocks.push(p);
        }
        // Read back: each block must hold its own stamp, so any
        // overlap in the allocator surfaces as a mismatch.
        for (i, &p) in blocks.iter().enumerate() {
            assert_eq!(
                unsafe { *(p as *const u64) },
                0xA000_0000_0000_0000 | (i as u64),
                "block {i} corrupted (allocator overlap?)"
            );
        }
    }

    #[test]
    fn list_append_grows_length_and_preserves_value() {
        let _guard = heap_test_lock();
        // Spirit of the reference's list-data tests: a fresh
        // `ListHeader` starts empty; each `APND` bumps the
        // length by 1 and links a new `ATOM_INT` atom; `HD`
        // returns the first appended value.
        let hdr = unsafe { __newbcpl_list_new_empty() };
        assert!(!hdr.is_null());
        assert_eq!(unsafe { __newbcpl_list_len(hdr) }, 0);
        unsafe {
            APND(hdr, 10);
            APND(hdr, 20);
            APND(hdr, 30);
        }
        assert_eq!(unsafe { __newbcpl_list_len(hdr) }, 3);
        assert_eq!(unsafe { __newbcpl_list_hd(hdr) }, 10);
        // Walk the chain by hand to confirm node ordering.
        let mut cur = unsafe { (*hdr).head };
        let expected = [10i64, 20, 30];
        for want in &expected {
            assert!(!cur.is_null(), "chain ended too soon");
            assert_eq!(unsafe { (*cur).value }, *want);
            assert_eq!(unsafe { (*cur).type_tag }, ATOM_INT);
            cur = unsafe { (*cur).next };
        }
        assert!(cur.is_null(), "chain longer than appended count");
    }

    #[test]
    fn list_tl_shares_tail_nodes() {
        let _guard = heap_test_lock();
        // `TL` returns a new header that re-uses the existing
        // atom chain — sharing is intentional and O(1). After
        // `TL`, `HD` of the result is the original list's second
        // element.
        let hdr = unsafe { __newbcpl_list_new_empty() };
        unsafe {
            APND(hdr, 100);
            APND(hdr, 200);
            APND(hdr, 300);
        }
        let tail = unsafe { __newbcpl_list_tl(hdr) };
        assert!(!tail.is_null());
        assert_eq!(unsafe { __newbcpl_list_len(tail) }, 2);
        assert_eq!(unsafe { __newbcpl_list_hd(tail) }, 200);
        // Original is unmodified.
        assert_eq!(unsafe { __newbcpl_list_len(hdr) }, 3);
        assert_eq!(unsafe { __newbcpl_list_hd(hdr) }, 100);
    }

    #[test]
    fn list_concat_produces_combined_chain() {
        let _guard = heap_test_lock();
        // `CONCAT` makes a fresh header whose atoms copy from
        // `a` and then `b`. Both inputs survive unchanged.
        let a = unsafe { __newbcpl_list_new_empty() };
        unsafe {
            APND(a, 1);
            APND(a, 2);
        }
        let b = unsafe { __newbcpl_list_new_empty() };
        unsafe {
            APND(b, 3);
            APND(b, 4);
            APND(b, 5);
        }
        let c = unsafe { CONCAT(a, b) };
        assert_eq!(unsafe { __newbcpl_list_len(c) }, 5);
        let mut cur = unsafe { (*c).head };
        for want in 1..=5i64 {
            assert_eq!(unsafe { (*cur).value }, want);
            cur = unsafe { (*cur).next };
        }
        assert_eq!(unsafe { __newbcpl_list_len(a) }, 2);
        assert_eq!(unsafe { __newbcpl_list_len(b) }, 3);
    }

    #[test]
    fn float_appends_round_trip_through_atom_value() {
        let _guard = heap_test_lock();
        // `APND_FLOAT` reinterprets the double's bits into the
        // atom's `i64` value slot. Reading them back as `f64`
        // bits must restore the original number — confirms the
        // bit-cast direction and atom-tag bookkeeping.
        let hdr = unsafe { __newbcpl_list_new_empty() };
        unsafe {
            APND_FLOAT(hdr, 3.5);
            APND_FLOAT(hdr, -2.25);
        }
        let head = unsafe { (*hdr).head };
        let second = unsafe { (*head).next };
        assert_eq!(unsafe { (*head).type_tag }, ATOM_FLOAT);
        assert_eq!(unsafe { (*second).type_tag }, ATOM_FLOAT);
        assert_eq!(
            f64::from_bits(unsafe { (*head).value } as u64),
            3.5,
        );
        assert_eq!(
            f64::from_bits(unsafe { (*second).value } as u64),
            -2.25,
        );
    }
}
