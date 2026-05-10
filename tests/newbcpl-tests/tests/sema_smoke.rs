//! End-to-end smoke test: parse a real .bcl file from the reference
//! corpus, run sema, and check that the inferred binding hints match
//! what we'd expect from reading the source.

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

fn analyze_file(name: &str) -> Option<newbcpl_sema::SemaOutput> {
    let root = reference_bcl_root()?;
    let path = root.join(name);
    let source = std::fs::read_to_string(&path).ok()?;
    let program = newbcpl_parser::parse_source(&source)
        .unwrap_or_else(|e| panic!("parse {} -> {}", path.display(), e.render()));
    Some(analyze(&program))
}

fn binding_hint(out: &newbcpl_sema::SemaOutput, name: &str) -> Option<TypeHint> {
    out.bindings
        .iter()
        .rev()
        .find(|b| b.name == name)
        .map(|b| b.hint)
}

#[test]
fn basic_int_test_locals() {
    let Some(out) = analyze_file("basic_int_test.bcl") else {
        eprintln!("skipping: reference not present");
        return;
    };
    // The file does `LET I = 42` inside a routine.
    assert_eq!(binding_hint(&out, "I"), Some(TypeHint::Int));
}

#[test]
fn class1_records_class_and_constructor() {
    let Some(out) = analyze_file("class1.bcl") else {
        return;
    };
    // CLASS Point is declared.
    assert!(out.classes.iter().any(|c| c.name == "Point"));
    // `LET p = NEW Point(50,75)` — p should be OBJECT [Point].
    let p = out
        .bindings
        .iter()
        .find(|b| b.name == "p")
        .expect("missing p binding");
    assert_eq!(p.hint, TypeHint::Object);
    assert_eq!(p.class_name.as_deref(), Some("Point"));
}
