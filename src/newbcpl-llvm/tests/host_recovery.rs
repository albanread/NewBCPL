//! Integration tests for the host-side `catch_unwind` boundary
//! around START. A panic raised in a runtime helper called from
//! JIT'd BCPL must unwind cleanly back through any depth of JIT
//! frames, get caught by the boundary, and surface as an `Err`
//! return — the host process stays alive and can run a second JIT
//! invocation afterwards.
//!
//! These are in-process tests rather than subprocess probes
//! because the "process stays alive" property is the entire point:
//! we need to verify the same host can call `run_source_with_active_folder`
//! a second time after a panic and have it succeed.

/// Force serial execution of every test in this binary. The JIT
/// engine and `RtlAddFunctionTable` registrations are process-wide;
/// running these in parallel would race on global state.
fn jit_lock() -> std::sync::MutexGuard<'static, ()> {
    use std::sync::{Mutex, OnceLock};
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[test]
fn panic_in_runtime_helper_caught_by_host() {
    let _guard = jit_lock();
    let source = "LET START() BE $( __newbcpl_test_panic() $)\n";
    let result = newbcpl_llvm::run_source_with_active_folder(source, "panic_test", None);
    let err = result.expect_err("panic must surface as Err");
    assert!(
        err.contains("JIT panic"),
        "expected `JIT panic` substring, got: {err}"
    );
    assert!(
        err.contains("deliberate panic from runtime helper"),
        "expected helper's panic message in err, got: {err}"
    );
}

#[test]
fn host_survives_panic_and_keeps_running() {
    let _guard = jit_lock();
    // First JIT: panics.
    let panic_source = "LET START() BE $( __newbcpl_test_panic() $)\n";
    let _ = newbcpl_llvm::run_source_with_active_folder(panic_source, "first_panic", None);
    // Second JIT: must succeed in the same host process. If the
    // previous panic corrupted MCJIT state or the SEH function
    // table, this call would crash.
    let ok_source = "LET START() = 7\n";
    let result = newbcpl_llvm::run_source_with_active_folder(ok_source, "after_panic", None);
    assert_eq!(
        result,
        Ok(7),
        "host failed to recover from previous panic: {result:?}"
    );
}

#[test]
fn normal_run_returns_ok_value() {
    let _guard = jit_lock();
    // Sanity check: the recovery wrapping doesn't break the happy
    // path. A program that returns 42 still returns Ok(42).
    let source = "LET START() = 42\n";
    let result = newbcpl_llvm::run_source_with_active_folder(source, "happy", None);
    assert_eq!(result, Ok(42));
}
