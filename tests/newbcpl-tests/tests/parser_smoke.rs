//! Smoke-test the parser against the same hand-picked .bcl programs we
//! use for the lexer. The corpus is at `reference/tests/bcl_tests/`.

use std::path::PathBuf;

fn reference_bcl_root() -> Option<PathBuf> {
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

fn parse_or_panic(path: &std::path::Path) {
    let source = std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("read {}: {}", path.display(), e));
    match newbcpl_parser::parse_source(&source) {
        Ok(_) => {}
        Err(e) => panic!("parse {} -> {}", path.display(), e.render()),
    }
}

#[test]
fn parses_basic_test() {
    let Some(root) = reference_bcl_root() else {
        eprintln!("skipping: reference/tests/bcl_tests/ not present");
        return;
    };
    parse_or_panic(&root.join("basic_test.bcl"));
}

#[test]
fn parses_basic_int_test() {
    let Some(root) = reference_bcl_root() else {
        eprintln!("skipping: reference/tests/bcl_tests/ not present");
        return;
    };
    parse_or_panic(&root.join("basic_int_test.bcl"));
}
