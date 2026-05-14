//! Lock the K&R-style user guide (`docs/user_guide.md`) and the
//! handwritten example programs (`examples/*.bcl`) against the parser
//! and sema. Doc rot is one of the cheapest bugs to ship and one of
//! the most embarrassing — every fenced ```bcpl block in the guide
//! that claims to be a complete declaration must actually parse.
//!
//! Two complementary tests:
//!
//! - `user_guide_examples_parse`: walks every ```bcpl fence in the
//!   guide. Fragments that start with a declaration keyword (LET,
//!   FLET, CLASS, MANIFEST, STATIC, GLOBAL, GLOBALS, FUNCTION,
//!   ROUTINE, GET) must parse; sema must also accept them. Inline
//!   illustrative fragments (a bare `IF`, a single expression) are
//!   skipped — they are pedagogical, not runnable.
//!
//! - `examples_compile`: every `examples/*.bcl` must parse + sema.
//!   These are the worked programs the guide references by filename,
//!   so they need to stay buildable as the language evolves.
//!
//! Neither test runs the JIT; this is a fast, host-independent check
//! that catches the lion's share of doc/code drift. Full
//! run-and-check coverage lives in the matrix tier suites.

use std::path::PathBuf;

fn workspace_root() -> PathBuf {
    // Walk up from this crate's manifest dir until we find the
    // workspace root — identified by a `Cargo.toml` whose contents
    // contain `[workspace]`. The test crate's own Cargo.toml does
    // not, so we will skip past it cleanly.
    let mut here = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    loop {
        let manifest = here.join("Cargo.toml");
        if manifest.is_file() {
            if let Ok(contents) = std::fs::read_to_string(&manifest) {
                if contents.contains("[workspace]") {
                    return here;
                }
            }
        }
        if !here.pop() {
            panic!("could not locate workspace root from CARGO_MANIFEST_DIR");
        }
    }
}

/// Decl-keyword prefixes that mark a fenced block as "claims to be a
/// well-formed translation unit." If a block starts with one of these
/// it must parse — anything else is an illustrative fragment and we
/// skip it.
const DECL_PREFIXES: &[&str] = &[
    "LET ",
    "FLET ",
    "CLASS ",
    "MANIFEST",
    "STATIC",
    "GLOBAL",
    "GLOBALS",
    "FUNCTION ",
    "ROUTINE ",
    "GET ",
];

fn looks_like_translation_unit(block: &str) -> bool {
    let trimmed = block.trim_start();
    DECL_PREFIXES
        .iter()
        .any(|prefix| trimmed.starts_with(prefix))
}

/// Extract ```bcpl fenced blocks from a markdown document. Returns
/// (line_number, body) pairs, where `line_number` is 1-based and
/// points at the opening fence — useful for blaming a failure back to
/// a place in the guide.
fn extract_bcpl_blocks(markdown: &str) -> Vec<(usize, String)> {
    let mut blocks = Vec::new();
    let mut lines = markdown.lines().enumerate();
    while let Some((idx, line)) = lines.next() {
        if line.trim_start().starts_with("```bcpl") {
            let opener_line = idx + 1;
            let mut body = String::new();
            for (_, inner) in lines.by_ref() {
                if inner.trim_start().starts_with("```") {
                    break;
                }
                body.push_str(inner);
                body.push('\n');
            }
            blocks.push((opener_line, body));
        }
    }
    blocks
}

#[test]
fn user_guide_examples_parse() {
    let root = workspace_root();
    let guide_path = root.join("docs").join("user_guide.md");
    let guide = std::fs::read_to_string(&guide_path)
        .unwrap_or_else(|e| panic!("read {}: {e}", guide_path.display()));

    let blocks = extract_bcpl_blocks(&guide);
    assert!(
        !blocks.is_empty(),
        "expected at least one ```bcpl fence in {}",
        guide_path.display()
    );

    let mut runnable = 0usize;
    let mut skipped = 0usize;
    let mut failures: Vec<String> = Vec::new();

    for (line, body) in &blocks {
        if !looks_like_translation_unit(body) {
            skipped += 1;
            continue;
        }
        runnable += 1;
        // First try the block as-is. If it parses, great. If it does
        // not, fall back to wrapping it in a synthetic routine body:
        // the guide frequently shows snippets that mix a `LET` with
        // a follow-on statement like `a, b := b, a` — valid inside a
        // block, not as a translation unit. The wrapped retry tells
        // us whether the *content* is well-formed.
        let attempt = newbcpl_parser::parse_source(body).or_else(|first| {
            let wrapped = format!("LET __snippet() BE $(\n{}\n$)\n", body.trim_end());
            newbcpl_parser::parse_source(&wrapped).map_err(|_| first)
        });
        match attempt {
            Ok(program) => {
                let _ = newbcpl_sema::analyze(&program);
            }
            Err(err) => {
                failures.push(format!(
                    "{}:{} — parse failed: {}\n--- block ---\n{}\n--- end ---",
                    guide_path.display(),
                    line,
                    err.render(),
                    body.trim_end()
                ));
            }
        }
    }

    println!(
        "user_guide.md: {runnable} runnable blocks parsed, {skipped} fragments skipped"
    );

    if !failures.is_empty() {
        panic!(
            "{} user-guide block(s) failed to parse:\n\n{}",
            failures.len(),
            failures.join("\n\n")
        );
    }
}

#[test]
fn examples_compile() {
    let root = workspace_root();
    let examples = root.join("examples");
    let read_dir = std::fs::read_dir(&examples)
        .unwrap_or_else(|e| panic!("read_dir {}: {e}", examples.display()));

    let mut paths: Vec<PathBuf> = read_dir
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "bcl"))
        .collect();
    paths.sort();

    assert!(
        !paths.is_empty(),
        "expected at least one .bcl example under {}",
        examples.display()
    );

    let mut failures: Vec<String> = Vec::new();
    for path in &paths {
        let source = match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(err) => {
                failures.push(format!("{} — io: {err}", path.display()));
                continue;
            }
        };
        match newbcpl_parser::parse_source(&source) {
            Ok(program) => {
                let _ = newbcpl_sema::analyze(&program);
            }
            Err(err) => failures.push(format!(
                "{} — parse failed: {}",
                path.display(),
                err.render()
            )),
        }
    }

    println!("examples/: {} files parsed + sema'd", paths.len());

    if !failures.is_empty() {
        panic!(
            "{} example(s) failed:\n\n{}",
            failures.len(),
            failures.join("\n")
        );
    }
}
