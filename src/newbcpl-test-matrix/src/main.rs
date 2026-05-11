//! Probe-matrix generator.
//!
//! Reads a manifest of probe rows from a `.matrix` text file and
//! writes a Rust integration-test file containing one
//! `probe!` / `probe_contains!` / `reject!` macro invocation per
//! row. The output file goes into `tests/newbcpl-tests/tests/`
//! and is registered in that crate's `Cargo.toml` like any
//! handwritten test file.
//!
//! Manifest grammar (one row per non-blank, non-comment line):
//!
//!   <kind> <name> ::= <source> ==> <expected>
//!
//! Where:
//!   * `<kind>` is `probe` / `probe_contains` / `reject`.
//!   * `<name>` is a Rust identifier — becomes the `#[test]`
//!     function name and the temp-file stem.
//!   * `<source>` is the BCPL fixture source. `\n` escapes
//!     produce real newlines; `\"` for embedded quotes; `\\`
//!     for backslashes.
//!   * `<expected>` is the expected stdout (for `probe`),
//!     stdout substring (for `probe_contains`), or stderr
//!     substring (for `reject`).
//!
//! Comments start with `#`. Blank lines are ignored.
//!
//! Usage:
//!   cargo run -p newbcpl-test-matrix -- \
//!     <manifest.matrix> <output.rs> [--header "use newbcpl_tests::*;"]
//!
//! The output's `use` line defaults to importing all three
//! macros from `newbcpl_tests`. Override with `--header` if
//! the consumer crate uses a different import path.

use std::path::{Path, PathBuf};

#[derive(Debug)]
struct Row {
    kind: Kind,
    name: String,
    source: String,
    expected: String,
    /// 1-based line number in the manifest. Used for error
    /// reporting; doesn't end up in the emitted Rust.
    line_no: usize,
}

#[derive(Debug, Clone, Copy)]
enum Kind {
    Probe,
    ProbeContains,
    Reject,
}

impl Kind {
    fn macro_name(self) -> &'static str {
        match self {
            Kind::Probe => "probe",
            Kind::ProbeContains => "probe_contains",
            Kind::Reject => "reject",
        }
    }
}

fn main() -> std::process::ExitCode {
    let args: Vec<String> = std::env::args().collect();
    let mut header: Option<String> = None;
    let mut positional: Vec<PathBuf> = Vec::new();
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--header" => {
                i += 1;
                if i >= args.len() {
                    eprintln!("--header needs a value");
                    return std::process::ExitCode::from(2);
                }
                header = Some(args[i].clone());
                i += 1;
            }
            "-h" | "--help" => {
                print_usage();
                return std::process::ExitCode::SUCCESS;
            }
            other => {
                positional.push(PathBuf::from(other));
                i += 1;
            }
        }
    }
    if positional.len() != 2 {
        eprintln!("expected <manifest> <output>; got {} positional args", positional.len());
        print_usage();
        return std::process::ExitCode::from(2);
    }
    let manifest_path = &positional[0];
    let output_path = &positional[1];

    let manifest = match std::fs::read_to_string(manifest_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("cannot read manifest {}: {e}", manifest_path.display());
            return std::process::ExitCode::from(1);
        }
    };

    let rows = match parse_manifest(&manifest) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("{e}");
            return std::process::ExitCode::from(1);
        }
    };

    let header = header.unwrap_or_else(|| {
        "use newbcpl_tests::{probe, probe_contains, reject};".to_string()
    });

    let body = render(&rows, &header, manifest_path);
    if let Err(e) = std::fs::write(output_path, body) {
        eprintln!("cannot write {}: {e}", output_path.display());
        return std::process::ExitCode::from(1);
    }
    eprintln!(
        "[matrix] wrote {} probes to {}",
        rows.len(),
        output_path.display()
    );
    std::process::ExitCode::SUCCESS
}

fn print_usage() {
    eprintln!(
        "newbcpl-test-matrix — generate probe test files from a manifest\n\n\
USAGE:\n  newbcpl-test-matrix <manifest.matrix> <output.rs> [--header <use-line>]\n\n\
MANIFEST grammar (one row per line):\n  <kind> <name> ::= <source> ==> <expected>\n\n\
  kind    = probe | probe_contains | reject\n  name    = Rust identifier (becomes the #[test] fn name)\n  source  = BCPL fixture source; supports \\n / \\\" / \\\\ escapes\n  expected= expected text (full match for probe, substring for the other two)\n\n\
Comments start with `#`. Blank lines are ignored.\n"
    );
}

fn parse_manifest(text: &str) -> Result<Vec<Row>, String> {
    let mut rows = Vec::new();
    for (idx, raw_line) in text.lines().enumerate() {
        let line_no = idx + 1;
        let line = raw_line.trim_start();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        // <kind> <name> ::= <source> ==> <expected>
        let kind_end = line
            .find(char::is_whitespace)
            .ok_or_else(|| format!("line {line_no}: expected kind"))?;
        let kind_str = &line[..kind_end];
        let kind = match kind_str {
            "probe" => Kind::Probe,
            "probe_contains" => Kind::ProbeContains,
            "reject" => Kind::Reject,
            other => {
                return Err(format!(
                    "line {line_no}: unknown kind `{other}` (expected probe / probe_contains / reject)",
                ));
            }
        };
        let rest = line[kind_end..].trim_start();
        let name_end = rest
            .find(char::is_whitespace)
            .ok_or_else(|| format!("line {line_no}: expected name after kind"))?;
        let name = rest[..name_end].to_string();
        if !is_valid_ident(&name) {
            return Err(format!(
                "line {line_no}: `{name}` isn't a valid Rust identifier (probe names become fn names)",
            ));
        }
        let rest = rest[name_end..].trim_start();
        let body = rest
            .strip_prefix("::=")
            .ok_or_else(|| {
                format!("line {line_no}: expected `::=` after name")
            })?
            .trim_start();
        let arrow = body
            .find("==>")
            .ok_or_else(|| {
                format!("line {line_no}: expected `==>` between source and expected text")
            })?;
        let source = decode_escapes(body[..arrow].trim(), line_no)?;
        let expected = decode_escapes(body[arrow + 3..].trim(), line_no)?;
        rows.push(Row {
            kind,
            name,
            source,
            expected,
            line_no,
        });
    }
    Ok(rows)
}

fn is_valid_ident(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Decode the small backslash-escape set we support in
/// manifest payloads. Anything else passes through unchanged
/// (so the BCPL `*N` escape doesn't get touched).
fn decode_escapes(s: &str, line_no: usize) -> Result<String, String> {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        let next = chars.next().ok_or_else(|| {
            format!("line {line_no}: trailing backslash in payload")
        })?;
        match next {
            'n' => out.push('\n'),
            't' => out.push('\t'),
            'r' => out.push('\r'),
            '\\' => out.push('\\'),
            '"' => out.push('"'),
            other => {
                return Err(format!(
                    "line {line_no}: unknown escape `\\{other}` (supported: \\n \\t \\r \\\\ \\\")",
                ));
            }
        }
    }
    Ok(out)
}

fn render(rows: &[Row], header: &str, manifest_path: &Path) -> String {
    use std::fmt::Write as _;
    let manifest_display = manifest_path.display();
    let mut out = String::new();
    let _ = writeln!(
        out,
        "//! GENERATED by `newbcpl-test-matrix` from `{manifest_display}`.\n\
//! Edit the manifest, not this file — regenerate with\n\
//!   `cargo run -p newbcpl-test-matrix -- <manifest> <this-file>`."
    );
    let _ = writeln!(out);
    let _ = writeln!(out, "{header}");
    let _ = writeln!(out);
    for row in rows {
        let macro_name = row.kind.macro_name();
        let _ = writeln!(out, "// manifest line {}", row.line_no);
        let _ = writeln!(out, "{}!(", macro_name);
        let _ = writeln!(out, "    {} =>", row.name);
        let _ = writeln!(out, "    {} =>", rust_str_literal(&row.source));
        let _ = writeln!(out, "    {}", rust_str_literal(&row.expected));
        let _ = writeln!(out, ");");
        let _ = writeln!(out);
    }
    out
}

/// Render a Rust string literal that produces the given content
/// byte-for-byte. We escape `\`, `"`, and control characters;
/// everything else passes through. Newlines become `\n` so the
/// generated file stays one-row-per-probe readable.
fn rust_str_literal(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            c if (c as u32) < 0x20 => {
                use std::fmt::Write as _;
                let _ = write!(out, "\\u{{{:x}}}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_simple_probe_row() {
        let m = "probe int_add ::= LET START() BE $( WRITEN(7 + 5) $)\\n ==> 12\n";
        let rows = parse_manifest(m).expect("parse");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].name, "int_add");
        assert!(rows[0].source.ends_with('\n'));
        assert_eq!(rows[0].expected, "12");
        assert!(matches!(rows[0].kind, Kind::Probe));
    }

    #[test]
    fn skips_blank_lines_and_comments() {
        let m = "\n# this is a comment\n\nprobe a ::= x ==> y\n# another\nreject b ::= z ==> err\n";
        let rows = parse_manifest(m).expect("parse");
        assert_eq!(rows.len(), 2);
        assert!(matches!(rows[0].kind, Kind::Probe));
        assert!(matches!(rows[1].kind, Kind::Reject));
    }

    #[test]
    fn rejects_unknown_kind() {
        let m = "wibble bad ::= x ==> y\n";
        let err = parse_manifest(m).unwrap_err();
        assert!(err.contains("unknown kind"));
    }

    #[test]
    fn rejects_invalid_identifier() {
        let m = "probe 1bad ::= x ==> y\n";
        let err = parse_manifest(m).unwrap_err();
        assert!(err.contains("isn't a valid Rust identifier"));
    }

    #[test]
    fn decodes_escapes() {
        let s = decode_escapes(r#"hello\nworld\\\""#, 1).unwrap();
        assert_eq!(s, "hello\nworld\\\"");
    }

    #[test]
    fn renders_a_probe_to_the_macro_form() {
        let rows = vec![Row {
            kind: Kind::Probe,
            name: "ex".to_string(),
            source: "LET START() BE $( WRITEN(1) $)\n".to_string(),
            expected: "1".to_string(),
            line_no: 1,
        }];
        let out = render(&rows, "use newbcpl_tests::*;", Path::new("x.matrix"));
        assert!(out.contains("probe!("));
        assert!(out.contains("ex =>"));
        assert!(out.contains("WRITEN(1)"));
    }
}
