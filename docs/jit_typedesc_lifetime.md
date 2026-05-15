# Why `collect()` isn't currently safe across a JIT run

This is the architectural issue that the
`jit_run_advances_heap_block_counter` test in `newbcpl-llvm/src/lib.rs`
deliberately works around. Calling `newbcpl_runtime::gc::collect()`
after `newbcpl_llvm::run()` returns will dereference freed memory and
crash. This document spells out exactly why, and the two viable fixes.

## The lifecycle mismatch

```
┌──────── newbcpl_llvm::run() ───────────────────────┐
│                                                    │
│  1. emit::emit(&context, &ir)                      │
│      └─ adds @Point.desc constant to the LLVM      │
│         module's data section.                     │
│                                                    │
│  2. module.create_jit_execution_engine()           │
│      └─ ExecutionEngine takes ownership of the     │
│         module. MCJIT allocates JIT-managed        │
│         memory for the data section; @Point.desc   │
│         now sits at some runtime address X.        │
│                                                    │
│  3. JIT'd START runs:                              │
│      - calls __newbcpl_new_rec(X)                  │
│      - GC heap grows by one BlockHeader            │
│        whose `tag` field == X (the address of      │
│        @Point.desc *inside JIT memory*).           │
│      - START returns; the JIT'd code drops every   │
│        local reference to the instance.            │
│                                                    │
│  4. exec_engine drops ◀─── HERE BE DRAGONS         │
│      └─ MCJIT frees its allocations, including     │
│         the data section. Address X is now         │
│         invalid memory.                            │
└────────────────────────────────────────────────────┘

  GC heap (lives in newbcpl-runtime statics):
   ┌───────────────────────────┐
   │ BlockHeader { tag: X, ... } │  ← tag is now dangling
   │ <Point payload bytes>      │
   └───────────────────────────┘
```

The GC heap **outlives** the JIT engine that produced the TypeDescs
its blocks reference. The mutator's `Drop` on process exit eventually
runs `collect_log_snapshot` and various stat queries; an explicit
`collect()` call walks every block and dereferences `tag` to read
`TypeDesc.size`, `TypeDesc.ptroffs`, etc. Both paths read freed
memory after step 4.

## Why we don't see this every run

Today the test `jit_run_advances_heap_block_counter` works because:

- It only reads the `alloc_blocks_lifetime` counter (a plain `u64`
  that the allocator increments and forgets — never re-reads the
  TypeDesc).
- It does not call `collect()`.
- The OS reclaims the heap memory at process exit without a sweep.

The crash that surfaced when I added the post-`run()` collect() call
landed exactly on the dangling-tag deref.

## Fix A: keep the JIT engine alive while its TypeDescs are referenced

This is what NewCP does. The runtime maintains a registry of "retired
JIT images" — engines whose `run()` has returned but whose data
sections must stay mapped because the GC heap still holds blocks
tagged with TypeDescs from them. A retired image is droppable only
when `module_has_no_live_blocks(module_name)` returns true (the
function already exists in `gc.rs:1344`).

Concretely:

1. `emit_module` calls `__newbcpl_register_module_named(name, ...)`
   for every emitted module so the GC tracks the module's identity.
2. Each `@Class.desc` global registers itself with the GC tagged
   with that module name on first allocation.
3. `run()` does **not** drop `exec_engine` directly; instead it
   moves it into a thread-local or static `RETIRED_IMAGES` pool.
4. At every `collect()` (or on a periodic timer), the runtime
   scans `RETIRED_IMAGES`, drops any whose modules report
   `module_has_no_live_blocks == true`.

Pros:
- The TypeDesc and the vtable both stay live, so `collect()` can
  trace fields and `MethodCall` keeps working even on objects
  that outlive their constructing JIT call.
- Long-running drivers (e.g. an iGui process) accumulate retired
  images bounded by their actual live-block count, not by the
  number of `run()` invocations.

Cons:
- Plumbing change in `run()` and a process-wide pool.
- The pool must be Send + Sync if multiple JIT runs ever
  cross threads (today they don't — single-threaded JIT host).

## Fix B: copy TypeDescs out of the JIT module

At the moment we *only* read TypeDesc.size in the alloc path, and
we don't currently use TypeDesc.vtable for dispatch (that's the
inline vtable header at the instance's offset 0). Method dispatch
goes `obj → vtable_ptr → slot`; nothing follows `BlockHeader.tag`
back to the TypeDesc except `collect()`'s sweep.

So we could:

1. At emit time, build a Rust-side `TypeDesc` (a `Box::leak`'d
   constant per class) populated from the same sema layout data.
2. Pass that runtime-side TypeDesc address to `__newbcpl_new_rec`
   from the JIT'd `NEW Class` call (instead of the JIT-module
   `@Class.desc` address).
3. The `@Class.desc` global in the LLVM module becomes redundant
   for allocation; we keep it only if some future MethodCall
   path wants to walk through it.

Pros:
- Drop the JIT engine freely after `run()` — TypeDescs live as
  long as the runtime statics.
- Smaller change set than Fix A.

Cons:
- Two TypeDescs per class (Rust-side + JIT-side) until we delete
  the JIT-side copy. Mild duplication.
- If we ever start using `TypeDesc.vtable` for dispatch (NewCP's
  full design), the runtime-side copy needs its `vtable` field
  patched after the JIT engine writes the method addresses into
  `@Class.vtable` — extra plumbing.

## Recommendation

**Fix B first** (smaller, decouples the simpler half), **Fix A as
a follow-up** if and when long-running processes show retired-image
memory pressure. The test that motivated this — JIT-allocate then
collect — is unlocked by Fix B alone, since collect()'s sweep only
needs `TypeDesc.size`, which a runtime-side copy provides directly.

## Status

- The constraint is currently documented inline in the JIT
  integration test (`jit_run_advances_heap_block_counter`).
- `module_has_no_live_blocks` exists and is unit-tested in `gc.rs`
  (Fix A's enabling primitive is already there).
- Neither fix is implemented yet.

## Related: the JIT vtable registry

A separate mechanism, but with overlapping lifetime concerns:
`__newbcpl_lookup_method` (the runtime helper that resolves
un-annotated `obj.method()` calls) keys off the instance's inline
vtable pointer — `instance[0]` is the address of
`@<Class>.vtable`, a global emitted by the LLVM crate. At
JIT-finalize, the crate registers each class's
`(vtable_addr, method_names_addr, count)` triple in a
process-global `VTABLE_METHOD_REGISTRY` so the helper can find the
parallel `@<Class>.method_names` array.

This registry has the same JIT-drop hazard as the TypeDesc story:
if we ever drop and rebuild the JIT engine, vtable globals get
new addresses but the registry still points at the old ones. The
fix shape is identical — either retain the engine module (Fix A
analogue) or have the LLVM crate clear and re-register on each
build (Fix B analogue). Today we leak the engine, so the registry
entries stay valid for the lifetime of the process. The note here
is to keep both parallel sub-systems flagged when retirement
support lands.
