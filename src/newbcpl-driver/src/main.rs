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
    ("dump-ast <path>", "print the AST (not implemented yet)"),
    ("dump-sema <path>", "print the bound IR (not implemented yet)"),
    ("dump-cfg <path>", "print the control-flow graph (not implemented yet)"),
    ("dump-ir <path>", "print the typed IR (not implemented yet)"),
    ("dump-llvm <path>", "print LLVM IR (not implemented yet)"),
    ("dump-asm <path>", "print final native assembly (not implemented yet)"),
    ("dump-heap", "snapshot the runtime heap (not implemented yet)"),
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
