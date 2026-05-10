//! NewBCPL loader: bootstrap + module session.
//!
//! Stub. Will mirror NewCP's `LoaderSession` shape — active/retired
//! generations, drop-predicate hook for hot reload, native-module
//! registration so Rust-hosted runtime modules and BCPL modules look the
//! same to the loader.

pub fn bootstrap_report() -> String {
    "newbcpl-loader bootstrap: not yet implemented".to_string()
}
