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
    let stdout = std::io::stdout();
    let mut handle = stdout.lock();
    let _ = write!(handle, "{n}");
    let _ = handle.flush();
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
    let stdout = std::io::stdout();
    let mut handle = stdout.lock();
    let _ = write!(handle, "{f}");
    let _ = handle.flush();
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
    let stdout = std::io::stdout();
    let mut h = stdout.lock();

    let mut i = 0;
    let mut used = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'%' && i + 1 < bytes.len() {
            let spec = bytes[i + 1];
            i += 2;
            if spec == b'%' {
                let _ = h.write_all(b"%");
                continue;
            }
            if used >= args.len() {
                let _ = write!(h, "%{}", spec as char);
                continue;
            }
            let a = args[used];
            used += 1;
            match spec {
                b'd' | b'i' | b'N' => {
                    let _ = write!(h, "{a}");
                }
                b'x' => {
                    let _ = write!(h, "{:x}", a as u64);
                }
                b'X' => {
                    let _ = write!(h, "{:016X}", a as u64);
                }
                b'o' => {
                    let _ = write!(h, "{:o}", a as u64);
                }
                b'c' => {
                    let _ = h.write_all(&[(a & 0xff) as u8]);
                }
                b's' => {
                    if a == 0 {
                        let _ = h.write_all(b"(null)");
                    } else {
                        let s = unsafe { CStr::from_ptr(a as *const i8) };
                        let _ = h.write_all(s.to_bytes());
                    }
                }
                b'f' | b'F' => {
                    let f = f64::from_bits(a as u64);
                    let _ = write!(h, "{f}");
                }
                other => {
                    let _ = write!(h, "%{}", other as char);
                    used -= 1; // unknown specifier doesn't consume the arg
                }
            }
        } else {
            let _ = h.write_all(&[b]);
            i += 1;
        }
    }
    let _ = h.flush();
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

/// Allocate a fresh, zeroed slab of `(n_words + 1) * 8` bytes,
/// store `n_words` into the first slot, and return a pointer one
/// word past the start so subscripts `p!0..p!(n-1)` and
/// `__newbcpl_len(p)` (which reads `p[-1]`) both work.
fn alloc_vec_words(n_words: i64) -> *mut i64 {
    let n = n_words.max(0) as usize;
    let mut buf = vec![0i64; n + 1].into_boxed_slice();
    buf[0] = n_words;
    let raw = Box::leak(buf).as_mut_ptr();
    unsafe { raw.add(1) }
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

/// `__newbcpl_len(p)` — read the BCPL length header that lives one
/// word *before* the data pointer. Mirrors the NewCP / reference
/// runtime layout. Returns 0 for null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __newbcpl_len(p: *const i64) -> i64 {
    if p.is_null() {
        return 0;
    }
    unsafe { *p.offset(-1) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn __newbcpl_freevec(_p: *mut i64) -> i64 {
    0
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn __newbcpl_freelist(_p: *mut i64) -> i64 {
    0
}

/// List ABI placeholder: the reference uses a header struct;
/// without GC integration we treat a list as a contiguous run
/// of words and `hd` reads slot 0. `tl` returns a pointer one
/// word past the head. `rest(p, n)` skips n words.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __newbcpl_list_hd(p: *const i64) -> i64 {
    if p.is_null() {
        return 0;
    }
    unsafe { *p }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn __newbcpl_list_tl(p: *mut i64) -> *mut i64 {
    if p.is_null() {
        return std::ptr::null_mut();
    }
    unsafe { p.add(1) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn __newbcpl_list_rest(p: *mut i64, n: i64) -> *mut i64 {
    if p.is_null() {
        return std::ptr::null_mut();
    }
    unsafe { p.add(n.max(0) as usize) }
}

// Function-call-form aliases of the list helpers. BCPL programs
// can write either the prefix operators (`HD list`, `TL list`) or
// the function-call form (`HD(list)`, `TAIL(list)`) — sema /
// lowering treats the function-call form as a free function so we
// expose the same addresses under those names.

#[unsafe(no_mangle)]
pub unsafe extern "C" fn HD(p: *const i64) -> i64 {
    unsafe { __newbcpl_list_hd(p) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn TL(p: *mut i64) -> *mut i64 {
    unsafe { __newbcpl_list_tl(p) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn TAIL(p: *mut i64) -> *mut i64 {
    unsafe { __newbcpl_list_tl(p) }
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
        vec![
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
            builtin!(__newbcpl_freevec),
            builtin!(__newbcpl_freelist),
            builtin!(__newbcpl_list_hd),
            builtin!(__newbcpl_list_tl),
            builtin!(__newbcpl_list_rest),
            Builtin {
                name: "__newbcpl_new_rec",
                address: crate::gc::__newbcpl_new_rec as *const () as usize,
            },
        ]
    })
}

/// Returns true if `name` is a builtin known to the runtime. The
/// LLVM JIT layer uses this to detect missing symbols *before*
/// running, so an unresolved builtin produces a clean error rather
/// than a SIGSEGV when the JIT'd code calls a null pointer.
pub fn is_builtin(name: &str) -> bool {
    builtin_addresses().iter().any(|b| b.name == name)
}
