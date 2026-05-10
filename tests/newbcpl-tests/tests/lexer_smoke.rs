//! Smoke-test the lexer against a hand-picked slice of real .bcl programs
//! from the old NBCPL reference. The corpus is at
//! `reference/tests/bcl_tests/` and contains 850+ files; we only check a
//! representative sample here so the suite stays fast.

use std::path::PathBuf;

fn reference_bcl_root() -> Option<PathBuf> {
    // Walk upward from CARGO_MANIFEST_DIR until we find a `reference/`
    // sibling. This makes the test work whether `cargo test` is invoked
    // from the workspace root or from inside the crate.
    let mut here = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    loop {
        let candidate = here.join("reference").join("tests").join("bcl_tests");
        if candidate.is_dir() {
            return Some(candidate);
        }
        if !here.pop() {
            return None;
        }
    }
}

fn lex_or_panic(path: &std::path::Path) {
    let source = std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("read {}: {}", path.display(), e));
    match newbcpl_lexer::lex_source(&source) {
        Ok(_) => {}
        Err(e) => panic!("lex {} -> {}", path.display(), e.render()),
    }
}

#[test]
fn lexes_basic_test() {
    let Some(root) = reference_bcl_root() else {
        eprintln!("skipping: reference/tests/bcl_tests/ not present");
        return;
    };
    lex_or_panic(&root.join("basic_test.bcl"));
}

#[test]
fn lexes_basic_int_test() {
    let Some(root) = reference_bcl_root() else {
        eprintln!("skipping: reference/tests/bcl_tests/ not present");
        return;
    };
    lex_or_panic(&root.join("basic_int_test.bcl"));
}

#[test]
fn lexes_class1() {
    let Some(root) = reference_bcl_root() else {
        eprintln!("skipping: reference/tests/bcl_tests/ not present");
        return;
    };
    lex_or_panic(&root.join("class1.bcl"));
}

#[test]
fn lexes_basic_float_test() {
    let Some(root) = reference_bcl_root() else {
        eprintln!("skipping: reference/tests/bcl_tests/ not present");
        return;
    };
    lex_or_panic(&root.join("basic_float_test.bcl"));
}

#[test]
fn lexes_writes() {
    let Some(root) = reference_bcl_root() else {
        eprintln!("skipping: reference/tests/bcl_tests/ not present");
        return;
    };
    lex_or_panic(&root.join("WRITES.bcl"));
}
