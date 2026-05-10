//! Sweep sema over every .bcl file the parser accepts and report
//! aggregate counts: how many files analyse, total bindings inferred,
//! and the breakdown of inferred type hints.
//!
//! Gated behind `NEWBCPL_SEMA_CORPUS=1` so the default test run stays
//! fast.
//!
//! Run with:
//!   NEWBCPL_SEMA_CORPUS=1 cargo test -p newbcpl-tests sema_full_corpus -- --nocapture

use std::path::PathBuf;

use newbcpl_sema::{TypeHint, analyze};

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

#[test]
fn sema_full_corpus() {
    if std::env::var("NEWBCPL_SEMA_CORPUS").ok().as_deref() != Some("1") {
        eprintln!("skipping: set NEWBCPL_SEMA_CORPUS=1 to run the sema sweep");
        return;
    }
    let Some(root) = reference_bcl_root() else {
        eprintln!("skipping: reference/tests/bcl_tests/ not present");
        return;
    };

    let mut analysed = 0usize;
    let mut total_bindings = 0usize;
    let mut total_warnings = 0usize;
    let mut total_classes = 0usize;
    let mut by_hint: std::collections::HashMap<TypeHint, usize> =
        std::collections::HashMap::new();

    for entry in std::fs::read_dir(&root).expect("read_dir bcl_tests") {
        let Ok(entry) = entry else { continue };
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        if path.extension().is_none_or(|e| e != "bcl") {
            continue;
        }
        let Ok(source) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(program) = newbcpl_parser::parse_source(&source) else {
            continue;
        };
        let result = analyze(&program);
        analysed += 1;
        total_bindings += result.bindings.len();
        total_warnings += result.warnings.len();
        total_classes += result.classes.len();
        for b in &result.bindings {
            *by_hint.entry(b.hint).or_insert(0) += 1;
        }
    }

    println!(
        "sema-corpus: {analysed} files, {total_bindings} bindings, {total_classes} classes, {total_warnings} warnings"
    );
    let mut buckets: Vec<_> = by_hint.into_iter().collect();
    buckets.sort_by(|a, b| b.1.cmp(&a.1));
    println!("Hint distribution:");
    for (hint, count) in buckets {
        println!("  {count:6}  {}", hint.as_str());
    }
}
