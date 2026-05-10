//! `newbcpl-driver` — phase-visible compiler driver.
//!
//! Mirrors the surface of `newcp-driver`: each compiler phase has its own
//! subcommand that produces a stable textual artifact suitable for review
//! and regression testing. The intent is that no phase is "internal"; the
//! pipeline can be inspected at any point.

use std::env;
use std::path::Path;
use std::path::PathBuf;
use std::process::ExitCode;

const COMMANDS: &[(&str, &str)] = &[
    ("bootstrap", "report on the loader bootstrap state"),
    ("dump-tokens <path>", "print the token stream for a .bcl source file"),
    ("dump-ast <path>", "print the AST"),
    ("dump-sema <path>", "print the sema-decorated tree (bindings, classes, layouts, warnings)"),
    ("dump-cfg <path>", "print the control-flow graph (not implemented yet)"),
    ("dump-ir <path>", "print the typed IR"),
    ("dump-llvm <path>", "print LLVM IR (via Inkwell)"),
    ("dump-asm <path>", "print native x86_64-pc-windows-msvc assembly"),
    ("dump-heap", "snapshot the runtime heap (not implemented yet)"),
    ("run <path>", "JIT-compile and run the program's START routine"),
    (
        "test-folder <dir> [report]",
        "compile + JIT every *.bcl in <dir>; write per-test results to [report] (default: test-results.txt)",
    ),
];

fn main() -> ExitCode {
    let mut args = env::args().skip(1);
    let Some(command) = args.next() else {
        print_usage();
        return ExitCode::SUCCESS;
    };

    match command.as_str() {
        "bootstrap" => {
            println!("{}", newbcpl_loader::bootstrap_report());
            ExitCode::SUCCESS
        }
        "dump-tokens" => match args.next() {
            Some(path_arg) => {
                let path = PathBuf::from(path_arg);
                println!("{}", newbcpl_lexer::dump_tokens(&path));
                ExitCode::SUCCESS
            }
            None => {
                eprintln!("dump-tokens: missing source path");
                print_usage();
                ExitCode::from(2)
            }
        },
        "dump-ast" => match args.next() {
            Some(path_arg) => {
                let path = PathBuf::from(path_arg);
                println!("{}", newbcpl_parser::dump_ast(&path));
                ExitCode::SUCCESS
            }
            None => {
                eprintln!("dump-ast: missing source path");
                print_usage();
                ExitCode::from(2)
            }
        },
        "dump-sema" => match args.next() {
            Some(path_arg) => {
                let path = PathBuf::from(path_arg);
                println!("{}", newbcpl_sema::dump_sema(&path));
                ExitCode::SUCCESS
            }
            None => {
                eprintln!("dump-sema: missing source path");
                print_usage();
                ExitCode::from(2)
            }
        },
        "dump-ir" => match args.next() {
            Some(path_arg) => {
                let path = PathBuf::from(path_arg);
                println!("{}", newbcpl_ir::dump_ir(&path));
                ExitCode::SUCCESS
            }
            None => {
                eprintln!("dump-ir: missing source path");
                print_usage();
                ExitCode::from(2)
            }
        },
        "dump-llvm" => match args.next() {
            Some(path_arg) => {
                let path = PathBuf::from(path_arg);
                println!("{}", newbcpl_llvm::dump_llvm(&path));
                ExitCode::SUCCESS
            }
            None => {
                eprintln!("dump-llvm: missing source path");
                print_usage();
                ExitCode::from(2)
            }
        },
        "dump-asm" => match args.next() {
            Some(path_arg) => {
                let path = PathBuf::from(path_arg);
                println!("{}", newbcpl_llvm::dump_asm(&path));
                ExitCode::SUCCESS
            }
            None => {
                eprintln!("dump-asm: missing source path");
                print_usage();
                ExitCode::from(2)
            }
        },
        "run" => match args.next() {
            Some(path_arg) => {
                let path = PathBuf::from(path_arg);
                match newbcpl_llvm::run(&path) {
                    Ok(rc) => {
                        // BCPL routines return WORD by convention;
                        // typical programs return 0. Surface the
                        // value so scripts can branch on it.
                        if rc != 0 {
                            eprintln!("[run] START returned {rc}");
                        }
                        ExitCode::SUCCESS
                    }
                    Err(e) => {
                        eprintln!("run: {e}");
                        ExitCode::from(1)
                    }
                }
            }
            None => {
                eprintln!("run: missing source path");
                print_usage();
                ExitCode::from(2)
            }
        },
        "test-folder" => match args.next() {
            Some(folder_arg) => {
                let folder = PathBuf::from(folder_arg);
                // Remaining args: `start=N`, `stop=N`, `grep=text`,
                // or a bare path becomes the report path. `grep`
                // pre-filters to files whose source contains the
                // literal substring (case-sensitive); `start` /
                // `stop` then index into the filtered list.
                let mut start: Option<usize> = None;
                let mut stop: Option<usize> = None;
                let mut grep: Option<String> = None;
                let mut report: Option<PathBuf> = None;
                for arg in args {
                    if let Some(v) = arg.strip_prefix("start=") {
                        match v.parse::<usize>() {
                            Ok(n) => start = Some(n),
                            Err(_) => {
                                eprintln!("test-folder: bad start=N value `{v}`");
                                return ExitCode::from(2);
                            }
                        }
                    } else if let Some(v) = arg.strip_prefix("stop=") {
                        match v.parse::<usize>() {
                            Ok(n) => stop = Some(n),
                            Err(_) => {
                                eprintln!("test-folder: bad stop=N value `{v}`");
                                return ExitCode::from(2);
                            }
                        }
                    } else if let Some(v) = arg.strip_prefix("grep=") {
                        grep = Some(v.to_string());
                    } else {
                        report = Some(PathBuf::from(arg));
                    }
                }
                let report =
                    report.unwrap_or_else(|| PathBuf::from("test-results.txt"));
                run_test_folder(&folder, &report, start, stop, grep.as_deref())
            }
            None => {
                eprintln!("test-folder: missing folder path");
                print_usage();
                ExitCode::from(2)
            }
        },
        "dump-cfg" => {
            let _path = args.next().map(PathBuf::from);
            eprintln!(
                "{command}: phase not implemented yet — see docs/manifesto.md for sequencing"
            );
            ExitCode::from(64)
        }
        "dump-heap" => {
            eprintln!("dump-heap: runtime not implemented yet");
            ExitCode::from(64)
        }
        "--help" | "-h" | "help" => {
            print_usage();
            ExitCode::SUCCESS
        }
        other => {
            eprintln!("unknown command: {other}\n");
            print_usage();
            ExitCode::from(2)
        }
    }
}

/// One file's outcome from `test-folder`. The phase records *how far*
/// the program got through the pipeline; `Run` means the JIT actually
/// executed `START`. `Crash` is reserved for subprocess kills (panics
/// that abort instead of returning a clean error).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Parse,
    Sema,
    Ir,
    Llvm,
    Run,
    Crash,
}

impl Phase {
    fn name(self) -> &'static str {
        match self {
            Phase::Parse => "parse",
            Phase::Sema => "sema",
            Phase::Ir => "ir",
            Phase::Llvm => "llvm",
            Phase::Run => "run",
            Phase::Crash => "crash",
        }
    }
}

struct TestOutcome {
    file: PathBuf,
    pass: bool,
    failed_at: Option<Phase>,
    exit_code: i32,
    stdout: String,
    stderr: String,
    duration_ms: u128,
}

/// Map a `run`-subcommand stderr line to the phase it implicates.
/// The driver's `run` command prefixes its errors with the layer
/// that failed (`run: parse: ...`, `run: io: ...`, etc.); when the
/// prefix isn't recognised we treat it as the run phase itself
/// (e.g. an LLVM-emit panic surfaces as a non-zero exit with no
/// known prefix).
fn classify_run_stderr(stderr: &str, exit_code_present: bool) -> Phase {
    let first_line = stderr.lines().next().unwrap_or("").trim();
    let body = first_line.strip_prefix("run:").map(str::trim).unwrap_or(first_line);
    if body.starts_with("io:") || body.starts_with("parse:") {
        Phase::Parse
    } else if body.starts_with("sema:") {
        Phase::Sema
    } else if body.starts_with("ir:") {
        Phase::Ir
    } else if body.starts_with("create_jit_execution_engine")
        || body.starts_with("get_function")
        || body.starts_with("emit:")
        || body.starts_with("llvm:")
    {
        Phase::Llvm
    } else if !exit_code_present {
        Phase::Crash
    } else {
        Phase::Run
    }
}

/// Walk `*.bcl` files in `folder` (non-recursive), spawn ourselves
/// once per file with `run` and capture stdout/stderr/exit. The
/// `run` subcommand already drives the full pipeline (parse → sema
/// → IR → LLVM emit → JIT → execute), so one subprocess per file
/// is enough; the failing phase is inferred from the stderr prefix.
///
/// `start` and `stop` are 1-based inclusive indices into the sorted
/// file list — handy for sweeping in chunks rather than running
/// hundreds of subprocesses in one go.
fn run_test_folder(
    folder: &Path,
    report_path: &Path,
    start: Option<usize>,
    stop: Option<usize>,
    grep: Option<&str>,
) -> ExitCode {
    use std::fs;
    use std::process::Command;
    use std::time::Instant;

    let read_dir = match fs::read_dir(folder) {
        Ok(rd) => rd,
        Err(e) => {
            eprintln!("test-folder: cannot read {}: {e}", folder.display());
            return ExitCode::from(1);
        }
    };

    let mut files: Vec<PathBuf> = read_dir
        .filter_map(|r| r.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "bcl"))
        .collect();
    files.sort();

    // Pre-filter by content substring before applying start/stop.
    if let Some(needle) = grep {
        files.retain(|p| {
            fs::read_to_string(p)
                .ok()
                .is_some_and(|s| s.contains(needle))
        });
    }

    if files.is_empty() {
        let scope = grep
            .map(|g| format!(" matching grep=`{g}`"))
            .unwrap_or_default();
        eprintln!(
            "test-folder: no *.bcl files in {}{scope}",
            folder.display()
        );
        return ExitCode::from(1);
    }

    // 1-based, inclusive. `start=1 stop=10` runs the first 10 files.
    // Out-of-range indices clamp to the corpus size.
    let total_in_corpus = files.len();
    let start_idx = start.unwrap_or(1).max(1);
    let stop_idx = stop.unwrap_or(total_in_corpus).min(total_in_corpus);
    if start_idx > stop_idx {
        eprintln!(
            "test-folder: empty range start={start_idx} stop={stop_idx} (corpus has {total_in_corpus})",
        );
        return ExitCode::from(2);
    }
    let slice = &files[(start_idx - 1)..stop_idx];

    let exe = match env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("test-folder: cannot locate current exe: {e}");
            return ExitCode::from(1);
        }
    };

    let mut outcomes: Vec<TestOutcome> = Vec::with_capacity(slice.len());
    let started = Instant::now();

    for (j, file) in slice.iter().enumerate() {
        let global_idx = start_idx + j;
        let t0 = Instant::now();
        let mut outcome = TestOutcome {
            file: file.clone(),
            pass: true,
            failed_at: None,
            exit_code: 0,
            stdout: String::new(),
            stderr: String::new(),
            duration_ms: 0,
        };

        match Command::new(&exe).arg("run").arg(file).output() {
            Ok(o) => {
                outcome.exit_code = o.status.code().unwrap_or(-1);
                outcome.stdout = String::from_utf8_lossy(&o.stdout).into_owned();
                outcome.stderr = String::from_utf8_lossy(&o.stderr).into_owned();
                if !o.status.success() {
                    outcome.pass = false;
                    outcome.failed_at =
                        Some(classify_run_stderr(&outcome.stderr, o.status.code().is_some()));
                }
            }
            Err(e) => {
                outcome.pass = false;
                outcome.failed_at = Some(Phase::Crash);
                outcome.stderr = format!("subprocess spawn failed: {e}");
            }
        }

        outcome.duration_ms = t0.elapsed().as_millis();
        let mark = if outcome.pass { "PASS" } else { "FAIL" };
        let name = outcome.file.file_name().unwrap_or_default().to_string_lossy();
        let phase_str = outcome
            .failed_at
            .map(Phase::name)
            .map(|p| format!(" ({p})"))
            .unwrap_or_default();
        eprintln!("[{global_idx}/{total_in_corpus}] {mark}  {name}{phase_str}");
        outcomes.push(outcome);
    }

    let total_ms = started.elapsed().as_millis();
    let passed = outcomes.iter().filter(|o| o.pass).count();
    let failed = outcomes.len() - passed;

    let report = build_report(folder, &outcomes, total_ms);
    if let Err(e) = fs::write(report_path, &report) {
        eprintln!("test-folder: cannot write {}: {e}", report_path.display());
        return ExitCode::from(1);
    }

    eprintln!();
    eprintln!("=== test-folder summary ===");
    eprintln!("corpus:  {}", folder.display());
    eprintln!(
        "range:   start={start_idx} stop={stop_idx} (of {total_in_corpus})",
    );
    eprintln!("ran:     {}", outcomes.len());
    eprintln!("passed:  {passed}");
    eprintln!("failed:  {failed}");
    eprintln!("elapsed: {:.2}s", total_ms as f64 / 1000.0);
    eprintln!("report:  {}", report_path.display());

    if failed == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}

fn build_report(folder: &Path, outcomes: &[TestOutcome], total_ms: u128) -> String {
    use std::collections::BTreeMap;
    use std::fmt::Write as _;

    let total = outcomes.len();
    let passed = outcomes.iter().filter(|o| o.pass).count();
    let failed = total - passed;

    let mut by_phase: BTreeMap<&'static str, usize> = BTreeMap::new();
    for o in outcomes.iter().filter(|o| !o.pass) {
        let key = o.failed_at.map(Phase::name).unwrap_or("unknown");
        *by_phase.entry(key).or_default() += 1;
    }

    let mut s = String::new();
    let _ = writeln!(s, "# newbcpl test-folder report");
    let _ = writeln!(s, "# corpus:  {}", folder.display());
    let _ = writeln!(s, "# total:   {total}");
    let _ = writeln!(s, "# passed:  {passed}");
    let _ = writeln!(s, "# failed:  {failed}");
    let _ = writeln!(s, "# elapsed: {:.2}s", total_ms as f64 / 1000.0);
    let _ = writeln!(s);
    let _ = writeln!(s, "## failures by phase");
    for (phase, n) in &by_phase {
        let _ = writeln!(s, "{phase:>8}  {n}");
    }
    let _ = writeln!(s);

    let _ = writeln!(s, "## passing tests ({passed})");
    for o in outcomes.iter().filter(|o| o.pass) {
        let name = o.file.file_name().unwrap().to_string_lossy();
        let _ = writeln!(s, "--- {name}  ({} ms)", o.duration_ms);
        // Show what the JIT'd program printed. Cap at 20 lines so a
        // chatty test doesn't blow up the report.
        let lines: Vec<&str> = o.stdout.lines().collect();
        if lines.is_empty() {
            let _ = writeln!(s, "    (no output)");
        } else {
            for line in lines.iter().take(20) {
                let _ = writeln!(s, "    > {line}");
            }
            if lines.len() > 20 {
                let _ = writeln!(s, "    ... ({} more lines)", lines.len() - 20);
            }
        }
    }
    let _ = writeln!(s);

    let _ = writeln!(s, "## failing tests ({failed})");
    for o in outcomes.iter().filter(|o| !o.pass) {
        let name = o.file.file_name().unwrap().to_string_lossy();
        let phase = o.failed_at.map(Phase::name).unwrap_or("unknown");
        let _ = writeln!(s, "--- {name}  ({phase}, exit {})", o.exit_code);
        for line in o.stderr.lines().take(8) {
            let _ = writeln!(s, "    | {line}");
        }
    }

    s
}

fn print_usage() {
    eprintln!("newbcpl-driver — phase-visible compiler driver");
    eprintln!();
    eprintln!("USAGE:");
    eprintln!("    newbcpl-driver <command> [args...]");
    eprintln!();
    eprintln!("COMMANDS:");
    let max = COMMANDS.iter().map(|(c, _)| c.len()).max().unwrap_or(0);
    for (cmd, blurb) in COMMANDS {
        eprintln!("    {:width$}    {}", cmd, blurb, width = max);
    }
    let _ = Path::new(""); // import touched for future use
}
