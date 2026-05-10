//! Parse every .bcl file in `reference/tests/bcl_tests/` and report a
//! summary of pass / fail counts. Gated behind `NEWBCPL_PARSE_CORPUS=1`
//! so the default test run stays fast.
//!
//! Run with:
//!   NEWBCPL_PARSE_CORPUS=1 cargo test -p newbcpl-tests parse_full_corpus -- --nocapture
//!   $env:NEWBCPL_PARSE_CORPUS=1; cargo test -p newbcpl-tests parse_full_corpus -- --nocapture
//!
//! The parser is still incomplete (no FOR / SWITCHON / classes / lists /
//! VEC / MANIFEST / etc.) so we expect a substantial number of files to
//! fail. The test reports the number rather than asserting a target.

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
fn parse_full_corpus() {
    if std::env::var("NEWBCPL_PARSE_CORPUS").ok().as_deref() != Some("1") {
        eprintln!("skipping: set NEWBCPL_PARSE_CORPUS=1 to run the corpus sweep");
        return;
    }
    let Some(root) = reference_bcl_root() else {
        eprintln!("skipping: reference/tests/bcl_tests/ not present");
        return;
    };

    let mut total = 0usize;
    let mut ok = 0usize;
    let mut errors: std::collections::HashMap<String, usize> = std::collections::HashMap::new();

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
            Err(_) => continue,
        };
        match newbcpl_parser::parse_source(&source) {
            Ok(_) => ok += 1,
            Err(e) => {
                // Bucket by leading keyword of the error message so we
                // can see which productions are missing most.
                let key = e
                    .message
                    .split_whitespace()
                    .take(4)
                    .collect::<Vec<_>>()
                    .join(" ");
                *errors.entry(key).or_insert(0) += 1;
            }
        }
    }

    println!(
        "parser-corpus: {ok}/{total} files parse cleanly ({:.1}%)",
        100.0 * (ok as f64) / (total as f64)
    );
    let mut buckets: Vec<_> = errors.into_iter().collect();
    buckets.sort_by(|a, b| b.1.cmp(&a.1));
    println!("Top error buckets (missing productions):");
    for (msg, count) in buckets.iter().take(15) {
        println!("  {count:4} × {msg}");
    }
}
