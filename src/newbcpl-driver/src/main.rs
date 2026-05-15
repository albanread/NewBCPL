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
        "gui <path>",
        "open the iGui frame (bedit + log view); Program ▸ Run / Ctrl+R JITs <path>; console output goes to the log view (Windows only)",
    ),
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
                // Active-modules folder: env override wins, else
                // `./modules-active/` next to the cwd. A missing
                // folder is fine — newbcpl_llvm::run_with_active_folder
                // just loads nothing.
                let modules_dir = env::var_os("NEWBCPL_MODULES_ACTIVE")
                    .map(PathBuf::from)
                    .unwrap_or_else(|| PathBuf::from("modules-active"));
                let modules_arg = if modules_dir.is_dir() {
                    Some(modules_dir.as_path())
                } else {
                    None
                };
                match newbcpl_llvm::run_with_active_folder(&path, modules_arg) {
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
        "gui" => match args.next() {
            Some(path_arg) => run_gui(PathBuf::from(path_arg)),
            None => {
                eprintln!("gui: missing source path");
                print_usage();
                ExitCode::from(2)
            }
        },
        "test-folder" => match args.next() {
            Some(folder_arg) => {
                let folder = PathBuf::from(folder_arg);
                // Remaining args: `start=N`, `stop=N`, `grep=text`,
                // `skip=text`, or a bare path becomes the report
                // path. `grep` keeps only files whose source
                // contains the substring; `skip` (which may repeat)
                // drops files whose source contains it. Use `skip`
                // to quarantine out-of-scope tests like SDL2 — they
                // exit the corpus's denominator before the sweep
                // even starts.
                let mut start: Option<usize> = None;
                let mut stop: Option<usize> = None;
                let mut grep: Option<String> = None;
                let mut skip: Vec<String> = Vec::new();
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
                    } else if let Some(v) = arg.strip_prefix("skip=") {
                        skip.push(v.to_string());
                    } else {
                        report = Some(PathBuf::from(arg));
                    }
                }
                let report =
                    report.unwrap_or_else(|| PathBuf::from("test-results.txt"));
                run_test_folder(&folder, &report, start, stop, grep.as_deref(), &skip)
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

/// Open the iGui frame and route `Program ▸ Run` (or `Ctrl+R`) at
/// the JIT pipeline. The UI thread owns the frame, bedit, and the
/// log view; a worker thread runs the language side — it installs a
/// console-write callback so `WRITES` / `WRITEN` / `WRITEF` / etc.
/// drain into the log view, then loops on `iGui::channels::next_event`
/// waiting for a `Menu` event carrying `RUN_MENU_CMD_ID`.
///
/// Each Run dispatches `newbcpl_llvm::run_with_active_folder` against
/// the path the user gave on the command line. The active-modules
/// folder is loaded the same way as in headless `run`. Output
/// streams to the log view via the callback; once `START` returns
/// the worker logs the result and waits for the next Run.
///
/// Currently Windows-only because iGui is Windows-only.
#[cfg(windows)]
fn run_gui(program_path: PathBuf) -> ExitCode {
    use newbcpl_runtime::builtins::set_console_write_callback;
    use newbcpl_runtime::igui;

    // Resolve active-modules folder the same way `run` does.
    let modules_dir = env::var_os("NEWBCPL_MODULES_ACTIVE")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("modules-active"));

    // The worker thread captures the program path and the modules
    // folder. It runs on a background OS thread spawned by iGui::run.
    let worker_path = program_path.clone();
    let worker = move || {
        gui_language_worker(worker_path, modules_dir);
    };

    // Install a console-write callback before iGui::run starts so any
    // very-early output goes to the log view. Buffers bytes into
    // lines and pushes each line (newline-terminated) into the log
    // view. UTF-8 partial sequences sitting in the buffer when no
    // newline arrives stay there until the next call — fine for
    // the corpus we care about (ASCII-dominant BCPL source).
    install_gui_console_callback();

    // Pre-load the program file into bedit so the user's Ctrl+S
    // writes back to the path the loader actually runs. Without
    // this, bedit opens empty and an edit-save-run cycle silently
    // diverges (Ctrl+S → "save as" prompt with a different path,
    // while Run still reads the original).
    igui::bedit_set_startup_file(program_path.clone());

    // Install the compile-check closure that F7 invokes. Runs the
    // full front-end pipeline (lex → parse → sema) and returns
    // every error as a `Diagnostic` with start + end position so
    // bedit can paint a wavy underline under the offending span.
    // A single fatal LexError or ParseError is reported alone (later
    // phases need a valid AST to run); sema errors are accumulated.
    igui::install_checker(check_source_for_gui);

    // Optional: drop a one-time banner so users see the log view
    // working even before they hit Run.
    igui::log_append(&format!(
        "newbcpl-driver gui — program: {}",
        program_path.display()
    ));
    igui::log_append("Press Ctrl+R or pick Program ▸ Run to execute.");
    igui::log_append("Press F7 to check, F8 to jump to next diagnostic.");

    match igui::run(Some(worker)) {
        Ok(code) => {
            if code != 0 {
                eprintln!("[gui] frame exited with code {code}");
            }
            // Remove the callback so any post-frame output (unlikely
            // — process is about to exit) goes back to stdout.
            set_console_write_callback::<fn(&[u8])>(None);
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("gui: {e}");
            ExitCode::from(1)
        }
    }
}

/// Stub for non-Windows builds — iGui only targets Windows.
#[cfg(not(windows))]
fn run_gui(_program_path: PathBuf) -> ExitCode {
    eprintln!("gui: iGui is Windows-only; rebuild on x86_64-pc-windows-msvc");
    ExitCode::from(64)
}

/// Buffer console bytes by line and ship complete lines into the
/// Run the front-end pipeline (lex → parse → sema) over the
/// current editor buffer and return every error as a `Diagnostic`.
/// Plumbed in by `run_gui` via `igui::install_checker`; bedit's
/// F7 binding calls this whenever the user asks for a check, and
/// the result populates the paint-time squiggle layer and the
/// status-line message.
///
/// Lex / parse errors are fatal at their own phase: a syntax
/// problem produces a single diagnostic and we stop there because
/// sema can't run on a malformed AST. Sema errors are bulk-
/// collected; pulling them out of the `SemaOutput.errors` channel
/// (the same hard-error channel that gates IR/codegen on `run`).
#[cfg(windows)]
fn check_source_for_gui(source: &str) -> Vec<newbcpl_runtime::igui::Diagnostic> {
    use newbcpl_runtime::igui::Diagnostic;

    let mut out: Vec<Diagnostic> = Vec::new();

    // `parse_source` runs the lexer internally and converts
    // `LexError` → `ParseError` via `ParseError::from_lex`, so a
    // single failure path covers both phases. Sema only runs on a
    // successful parse — a malformed AST would crash sema, and
    // the user's actionable signal is the parse error anyway.
    let program = match newbcpl_parser::parse_source(source) {
        Ok(p) => p,
        Err(e) => {
            let s = e.span.start;
            let en = e.span.end;
            out.push(Diagnostic {
                line: s.line,
                column: s.column,
                end_line: en.line,
                end_column: en.column,
                message: format!("parse: {}", e.message),
            });
            return out;
        }
    };

    let sema_out = newbcpl_sema::analyze(&program);
    for err in &sema_out.errors {
        let s = err.span.start;
        let en = err.span.end;
        out.push(Diagnostic {
            line: s.line,
            column: s.column,
            end_line: en.line,
            end_column: en.column,
            message: format!("sema: {}", err.message),
        });
    }
    out
}

/// iGui log view. Mutates a process-wide buffer guarded by a mutex;
/// keyed by thread isn't needed because writes serialise through the
/// console callback's `Mutex` anyway.
#[cfg(windows)]
fn install_gui_console_callback() {
    use newbcpl_runtime::builtins::set_console_write_callback;
    use newbcpl_runtime::igui;
    use std::sync::Mutex;

    static LINE_BUFFER: Mutex<Vec<u8>> = Mutex::new(Vec::new());

    set_console_write_callback(Some(move |bytes: &[u8]| {
        let mut buf = LINE_BUFFER.lock().expect("LINE_BUFFER poisoned");
        for &b in bytes {
            if b == b'\n' {
                // Flush the accumulated bytes as one log line. UTF-8
                // is preserved because we never split a codepoint.
                let line = String::from_utf8_lossy(&buf).into_owned();
                igui::log_append(&line);
                buf.clear();
            } else if b == b'\r' {
                // Ignore — WRITES may emit `*N` as just `\n`, but
                // formatted writes sometimes carry CR. Either way,
                // the log view doesn't want them.
            } else {
                buf.push(b);
            }
        }
    }));
}

/// Run on the iGui language thread. Drains the event mailbox; on
/// `Menu(RUN_MENU_CMD_ID)`, JIT-runs the program and reports the
/// result back into the log view. Loops until `FrameClose`.
#[cfg(windows)]
fn gui_language_worker(program_path: PathBuf, modules_dir: PathBuf) {
    use newbcpl_runtime::igui::{self, channels::IGuiEvent};

    igui::log_append(&format!(
        "[gui-worker] ready — modules folder: {}",
        if modules_dir.is_dir() {
            modules_dir.display().to_string()
        } else {
            format!("{} (missing — running with no modules)", modules_dir.display())
        }
    ));

    loop {
        // -1 ms blocks indefinitely; we don't need polling because
        // every action is event-driven.
        match igui::channels::next_event(-1) {
            None => continue,
            Some(IGuiEvent::FrameClose) => {
                igui::log_append("[gui-worker] frame closed; worker exiting");
                return;
            }
            Some(IGuiEvent::Menu { item_id, .. })
                if item_id as u16 == igui::RUN_MENU_CMD_ID =>
            {
                gui_run_program(&program_path, &modules_dir);
            }
            Some(_) => {
                // Other events are not handled in v0 — bedit owns
                // its own keystrokes on the UI thread, and nothing
                // else here cares about resize / focus / etc.
            }
        }
    }
}

/// Single-shot JIT-and-run on the language thread. Snapshots bedit's
/// live buffer (saved or not) and JITs that text — the user's
/// in-editor edits run immediately on Ctrl+R, no Save required.
/// All console output goes through the installed callback into the
/// log view.
#[cfg(windows)]
fn gui_run_program(program_path: &Path, modules_dir: &Path) {
    use newbcpl_runtime::igui;

    // Reset the event subsystem so the new program starts from a
    // clean state regardless of what the previous run did. Clears
    // the persistent window-filter set + discards stashed events
    // — without this, filters and events from the prior Run would
    // bleed into the next.
    igui::channels::clear_filter();
    igui::channels::discard_stashed_events();

    igui::log_append("---");

    // Use the launch-time filename stem as the IR module name so the
    // emitted IR looks the same whether sourced from buffer or disk
    // (and matches what dump-ir / dump-llvm would produce on the file).
    let module_name = program_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("program");

    let source = match igui::bedit_snapshot_buffer() {
        Some(s) => {
            igui::log_append(&format!(
                "[gui-worker] running bedit buffer ({} bytes; module name '{}')",
                s.len(),
                module_name
            ));
            s
        }
        None => {
            igui::log_append("[gui-worker] no bedit buffer available; nothing to run");
            return;
        }
    };

    let modules_arg = if modules_dir.is_dir() {
        Some(modules_dir)
    } else {
        None
    };
    match newbcpl_llvm::run_source_with_active_folder(&source, module_name, modules_arg) {
        Ok(code) => {
            igui::log_append(&format!("[gui-worker] START returned {code}"));
        }
        Err(e) => {
            igui::log_append(&format!("[gui-worker] error: {e}"));
        }
    }

    // Refresh the editor's inline diagnostics from the same source.
    // A successful run with sema-clean code clears the squiggles;
    // a failed run lights them up, even if the user hadn't pressed
    // F7. The check runs over the buffer text bedit currently has,
    // so the error positions line up with what's on screen.
    igui::bedit_run_check();
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
    skip_needles: &[String],
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

    // Drop files whose source contains any `skip=` substring. Used
    // to quarantine out-of-scope tests like the SDL2 family — they
    // never run on this dialect's Direct2D path, so excluding them
    // from the denominator gives a cleaner effective-pass-rate
    // number.
    let skipped_count = if !skip_needles.is_empty() {
        let before = files.len();
        files.retain(|p| {
            let body = match fs::read_to_string(p) {
                Ok(s) => s,
                Err(_) => return true,
            };
            !skip_needles.iter().any(|n| body.contains(n.as_str()))
        });
        before - files.len()
    } else {
        0
    };

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

        // Bound each test to a wall-clock timeout — some corpus
        // files have infinite loops or read RDCH and would block
        // the whole sweep indefinitely. Close stdin too so
        // RDCH-from-stdin returns immediately (-1, EOF) instead of
        // blocking on user input.
        match run_one_with_timeout(&exe, file, std::time::Duration::from_secs(5)) {
            Ok(o) => {
                outcome.exit_code = o.exit_code;
                outcome.stdout = o.stdout;
                outcome.stderr = o.stderr;
                if o.timed_out {
                    outcome.pass = false;
                    outcome.failed_at = Some(Phase::Crash);
                    if outcome.stderr.is_empty() {
                        outcome.stderr =
                            "test timed out (>5s); subprocess killed".to_string();
                    }
                } else if o.exit_code != 0 {
                    outcome.pass = false;
                    outcome.failed_at =
                        Some(classify_run_stderr(&outcome.stderr, o.exit_code != -1));
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

    let report = build_report(folder, &outcomes, total_ms, skipped_count, skip_needles);
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

fn build_report(
    folder: &Path,
    outcomes: &[TestOutcome],
    total_ms: u128,
    skipped_count: usize,
    skip_needles: &[String],
) -> String {
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
    if skipped_count > 0 {
        let needles = skip_needles.join("`, `");
        let _ = writeln!(
            s,
            "# skipped: {skipped_count}  (source contained `{needles}`)"
        );
    }
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

/// One subprocess outcome captured by `run_one_with_timeout`.
struct TimedOutput {
    exit_code: i32,
    stdout: String,
    stderr: String,
    timed_out: bool,
}

/// Run `<driver> run <file>` with `stdin` closed and a wall-clock
/// limit. On timeout, kill the child and return what we managed to
/// capture. Used by `test-folder` so a single hanging corpus file
/// can't stall the whole sweep.
///
/// Polling via `try_wait` rather than a worker thread keeps the
/// dependency surface to std only. 50 ms poll cadence — overhead
/// is negligible against the ~50 ms typical per-test JIT time.
fn run_one_with_timeout(
    exe: &Path,
    file: &Path,
    timeout: std::time::Duration,
) -> std::io::Result<TimedOutput> {
    use std::io::Read;
    use std::process::Stdio;
    let mut child = std::process::Command::new(exe)
        .arg("run")
        .arg(file)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let start = std::time::Instant::now();
    let (status, timed_out) = loop {
        match child.try_wait()? {
            Some(s) => break (Some(s), false),
            None => {
                if start.elapsed() > timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    break (None, true);
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
        }
    };
    let mut stdout = String::new();
    if let Some(mut s) = child.stdout.take() {
        let _ = s.read_to_string(&mut stdout);
    }
    let mut stderr = String::new();
    if let Some(mut s) = child.stderr.take() {
        let _ = s.read_to_string(&mut stderr);
    }
    let exit_code = match status {
        Some(s) => s.code().unwrap_or(-1),
        None => -1,
    };
    Ok(TimedOutput {
        exit_code,
        stdout,
        stderr,
        timed_out,
    })
}
