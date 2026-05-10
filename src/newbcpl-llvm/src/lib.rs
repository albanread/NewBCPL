//! NewBCPL LLVM emit + JIT.
//!
//! Stub. Will lower `newbcpl-ir` to LLVM IR via Inkwell (LLVM 22), JIT
//! through MCJIT initially (matching NewCP's current model) and migrate to
//! ORC v2 alongside it. Dumped by `dump-llvm` and `dump-asm`.
