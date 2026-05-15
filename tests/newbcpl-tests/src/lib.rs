//! Shared helpers for newbcpl integration tests.
//!
//! The "matrix" probe runners ([`matrix_tier5`], [`matrix_tier6`],
//! [`matrix_tier1_negatives`] under `tests/`) all spawn the JIT
//! driver as a subprocess and compare captured output. The helpers
//! here keep that machinery in one place.

use std::path::PathBuf;
use std::process::{Command, Output};

/// Resolve the JIT driver path by walking up from the test binary.
/// Cargo places integration-test binaries at
/// `target/<profile>/deps/<name>-<hash>[.exe]`; the driver lives
/// at `target/<profile>/newbcpl-driver[.exe]` (one directory up).
/// This avoids the `CARGO_BIN_EXE_*` env var, which would require
/// `newbcpl-driver` to expose a `lib` target.
pub fn driver_path() -> PathBuf {
    let mut p = std::env::current_exe().expect("current_exe");
    p.pop(); // remove the test binary
    p.pop(); // remove `deps`
    let name = if cfg!(windows) {
        "newbcpl-driver.exe"
    } else {
        "newbcpl-driver"
    };
    p.push(name);
    p
}

/// Write `source` to a temp `.bcl` file named after `tag`, run the
/// given driver subcommand against it, and return the subprocess
/// output. The temp file is removed before the helper returns so
/// callers don't have to.
pub fn run_driver(tag: &str, subcommand: &str, source: &str) -> Output {
    let tmp = std::env::temp_dir().join(format!("newbcpl-{tag}.bcl"));
    std::fs::write(&tmp, source).expect("write probe fixture");
    let output = Command::new(driver_path())
        .arg(subcommand)
        .arg(&tmp)
        .output()
        .expect("spawn newbcpl-driver");
    let _ = std::fs::remove_file(&tmp);
    output
}

/// Run a probe through `run` and assert its captured stdout equals
/// `expected`. Panics with both stdout and stderr on mismatch so a
/// failure points at exactly which cell of the matrix regressed.
pub fn expect_stdout(name: &str, source: &str, expected: &str) {
    let output = run_driver(name, "run", source);
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(
        output.status.success(),
        "probe `{name}` did not exit 0\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}"
    );
    assert_eq!(
        stdout, expected,
        "probe `{name}` produced unexpected stdout\n--- stderr ---\n{stderr}"
    );
}

/// Run a probe through `run` and assert its stdout *contains*
/// `expected_substring`. Use this when the program's output is
/// large or has unstable fields (timestamps, addresses) and the
/// stable signal is one identifying phrase.
pub fn expect_stdout_contains(name: &str, source: &str, expected_substring: &str) {
    let output = run_driver(name, "run", source);
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(
        output.status.success(),
        "probe `{name}` did not exit 0\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}"
    );
    assert!(
        stdout.contains(expected_substring),
        "probe `{name}` stdout missing substring `{expected_substring}`\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}"
    );
}

/// Run a probe through `run` and assert it exits 0, its stdout
/// equals `expected_stdout`, and its stderr contains every entry in
/// `stderr_substrings`. Used by BRK probes — the snapshot dump goes
/// to stderr while the program's regular WRITES output stays on
/// stdout, so both channels need to be asserted.
pub fn expect_stdout_and_stderr_contains(
    name: &str,
    source: &str,
    expected_stdout: &str,
    stderr_substrings: &[&str],
) {
    let output = run_driver(name, "run", source);
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(
        output.status.success(),
        "probe `{name}` did not exit 0\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}"
    );
    assert_eq!(
        stdout, expected_stdout,
        "probe `{name}` unexpected stdout\n--- stderr ---\n{stderr}"
    );
    for needle in stderr_substrings {
        assert!(
            stderr.contains(*needle),
            "probe `{name}` stderr missing `{needle}`\n--- stderr ---\n{stderr}"
        );
    }
}

/// Run a probe through a phase subcommand (`dump-ast`,
/// `dump-tokens`, `dump-sema`) and assert it FAILS with the given
/// substring in stderr. Tier 1 negative-corpus probes use this to
/// pin "you must reject this" guarantees.
pub fn expect_reject(name: &str, subcommand: &str, source: &str, stderr_substring: &str) {
    let output = run_driver(name, subcommand, source);
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(
        !output.status.success(),
        "probe `{name}` was expected to fail but exit code was {:?}\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}",
        output.status.code()
    );
    assert!(
        stderr.contains(stderr_substring),
        "probe `{name}` stderr missing substring `{stderr_substring}`\n--- stderr ---\n{stderr}"
    );
}

// ─── Declarative probe macros ─────────────────────────────────────
//
// Each macro expands to a `#[test]` function calling the matching
// expectation helper above. The test name doubles as the temp-file
// stem so failures point at the cell that regressed.
//
// Usage:
//
//   probe!(int_add => "LET START() BE $( WRITEN(7 + 5) $)" => "12");
//
// optionally with a doc-comment that explains what the cell is
// for — the comment travels through to the generated function so
// `cargo doc` and grep across the test crate both surface it.

/// Stdout exact-match probe. Compiles to:
/// ```ignore
/// #[test]
/// fn $name() { expect_stdout(stringify!($name), $source, $expected); }
/// ```
#[macro_export]
macro_rules! probe {
    ($name:ident => $source:expr => $expected:expr) => {
        #[test]
        fn $name() {
            $crate::expect_stdout(stringify!($name), $source, $expected);
        }
    };
    (
        $(#[$attr:meta])*
        $name:ident => $source:expr => $expected:expr
    ) => {
        $(#[$attr])*
        #[test]
        fn $name() {
            $crate::expect_stdout(stringify!($name), $source, $expected);
        }
    };
}

/// Stdout-contains probe — for outputs with unstable fields
/// (addresses, counters). Same shape as `probe!` but asserts the
/// expected text appears *somewhere* in stdout.
#[macro_export]
macro_rules! probe_contains {
    ($name:ident => $source:expr => $substring:expr) => {
        #[test]
        fn $name() {
            $crate::expect_stdout_contains(stringify!($name), $source, $substring);
        }
    };
}

/// Negative probe — asserts the program is rejected with the
/// expected diagnostic substring on stderr. Uses the `run`
/// subcommand because parse / sema errors land there.
#[macro_export]
macro_rules! reject {
    ($name:ident => $source:expr => $stderr_substring:expr) => {
        #[test]
        fn $name() {
            $crate::expect_reject(stringify!($name), "run", $source, $stderr_substring);
        }
    };
}
