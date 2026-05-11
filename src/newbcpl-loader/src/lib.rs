//! NewBCPL loader: bootstrap + module session.
//!
//! Stub. Will mirror [NewCP's `LoaderSession`](../../../../NewCP/NewCP/src/newcp-loader/src/lib.rs)
//! shape — active/retired generations, monotonic `next_generation`,
//! `ExecutionScope` pinning, `quiescent_epoch`, `RetiredImageDropPredicate`
//! hook for the GC veto over reclaiming retired JIT pages.
//!
//! BCPL-flavoured veneer: rename `ExportKind::Procedure` → `Routine`,
//! drop `ExportKind::Type` (BCPL has no user types as exports), drop
//! `HostedModuleArtifact.imports` and the whole `SourceModuleGraph` /
//! dependency-resolution phase (BCPL has no formal imports — only `GET`
//! for textual header inclusion; runtime binding is through the GLOBALS
//! vector by name). Keep `Constant` / `Variable` / `Command`. Add a
//! per-module `private_word` (RISC OS R12 analogue) handed to command
//! routines as their first argument.
//!
//! Resident Rust modules (the standard library: `console`, `string`,
//! `io`, `heap`, `iGui`) expose `native_module_artifact() ->
//! NativeModuleArtifact` factories the bootstrap registers at frame
//! creation. JIT-compiled BCPL modules go through the parse → sema →
//! codegen → JIT → materialize → register pipeline — one file at a
//! time, no transitive resolution. Both shapes share one
//! `ExportDirectory`.
//!
//! Boot sequence: resident modules → scan active-modules folder
//! (`./modules-active/` by default, `NEWBCPL_MODULES_ACTIVE` env override)
//! → run startup script (`./startup.script`, `NEWBCPL_STARTUP` env
//! override) → show CLI prompt. The user curates the loaded set by hand;
//! there is no automatic dependency resolution.
//!
//! See [`docs/module_system.md`](../../../../docs/module_system.md) for
//! the full design.

pub fn bootstrap_report() -> String {
    "newbcpl-loader bootstrap: not yet implemented".to_string()
}
