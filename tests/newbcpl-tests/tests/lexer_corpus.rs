//! Lex every .bcl file in `reference/tests/bcl_tests/` and report any
//! failures as a single test summary. This is gated behind the
//! `NEWBCPL_LEX_CORPUS=1` environment variable so the default test run
//! stays fast.
//!
//! Run with:
//!   NEWBCPL_LEX_CORPUS=1 cargo test -p newbcpl-tests lex_full_corpus -- --nocapture
//! or on Windows PowerShell:
//!   $env:NEWBCPL_LEX_CORPUS=1; cargo test -p newbcpl-tests lex_full_corpus -- --nocapture

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

#[test]
fn lex_full_corpus() {
    if std::env::var("NEWBCPL_LEX_CORPUS").ok().as_deref() != Some("1") {
        eprintln!("skipping: set NEWBCPL_LEX_CORPUS=1 to run the corpus sweep");
        return;
    }
    let Some(root) = reference_bcl_root() else {
        eprintln!("skipping: reference/tests/bcl_tests/ not present");
        return;
    };

    let mut total = 0usize;
    let mut ok = 0usize;
    let mut failures: Vec<(PathBuf, String)> = Vec::new();

    for entry in std::fs::read_dir(&root).expect("read_dir bcl_tests") {
        let Ok(entry) = entry else { continue };
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(ext) = path.extension() else { continue };
        if ext != "bcl" {
            continue;
        }
        total += 1;
        let source = match std::fs::read_to_string(&path) {
            Ok(s) => s,
            Err(e) => {
                failures.push((path.clone(), format!("io: {e}")));
                continue;
            }
        };
        match newbcpl_lexer::lex_source(&source) {
            Ok(_) => ok += 1,
            Err(e) => failures.push((path.clone(), e.render())),
        }
    }

    println!("corpus: {ok}/{total} lex cleanly");
    if !failures.is_empty() {
        let mut shown = 0;
        for (path, err) in &failures {
            if shown >= 20 {
                println!("  … {} more failures elided", failures.len() - shown);
                break;
            }
            println!("  FAIL {} -> {}", path.display(), err);
            shown += 1;
        }
        panic!("{}/{} files failed to lex", failures.len(), total);
    }
}
