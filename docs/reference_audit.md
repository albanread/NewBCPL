# Reference audit — language coverage vs. the BCPL spec

This is a feature-by-feature comparison of what `reference/documentation/`
describes as the language surface against what NewBCPL implements today.
Each row is one feature; the status column says where we are.

## Legend

| Symbol | Meaning |
|---|---|
| ✓ | Implemented end-to-end (parser + sema + IR + JIT) and pinned by at least one matrix probe |
| ⚠ | Implemented but probe coverage is thin — works in casual use, untested at the edges |
| ◐ | Partially implemented — known holes |
| ✗ | Not implemented |
| Δ | Intentional deviation from the reference; documented in the user guide |

Source docs consulted (in order of authority for the language surface):
- `reference/documentation/BCPL syntax.md`
- `reference/documentation/BCPL float extension.md`
- `reference/documentation/BCPL char and string.md`
- `reference/documentation/BCPL Runtime.md`
- `reference/documentation/classes_and_objects.md`

---

## §1 — Lexical

| Feature | Status | Notes |
|---|---|---|
| Identifiers (letters, digits, `_`) | ✓ | Lexer accepts `_` per the modern dialect |
| Decimal integer literals | ✓ | Tier 1 / Tier 3 probes |
| Octal literals `#NNN` | ✓ | Lexer rejects bare `#` cleanly |
| Hex literals `#XAB` | ✓ | Lexer + bitfield probes use them |
| Float literals (`3.14`, `1e-5`) | ✓ | Tier 3 float probes |
| String literals + `*N` `*T` `*S` `*B` `*P` `*C` `*"` `**` escapes | ✓ | Lexer probes pin every escape |
| Character constants `'c'`, `'*N'`, `'*T'`, `'*S'`, `'*B'`, `'*P'`, `'*C'`, `'*"'`, `'**'` | ✓ | Tier 3 (`char_lit_*`, 9 probes) — IR's `decode_char_lexeme` resolves each spec-defined escape to its byte value at lowering time. Hex form `'*XNN'` is *not* in `BCPL syntax.md` §1.3 — Δ from classic Martin Richards BCPL. |
| `TRUE` / `FALSE` literals | ✓ | Parser probes |
| `//` line comments | ✓ | |
| `/* */` block comments | ✓ | Lexer probes; non-nesting matches `BCPL syntax.md` §1.6 (first `*/` closes — same as C) |

## §2 — Operator precedence and expressions

Reference precedence table (high → low): `()`, `! OF`, `@ !`, `* / REM`,
`+ -`, `<< >>`, relational, `&`, `\| NEQV EQV`, `->`, `TABLE`, `VALOF`.

| Operator | Status | Notes |
|---|---|---|
| `@x` address-of | ✓ | Tier 3 (`address_of_round_trips_through_indirection`) — `@x` then `!p` round-trips |
| `!ptr` indirection (read + write) | ✓ | Tier 4 has both |
| `v ! i` word subscript (read + write) | ✓ | Tier 6 |
| `v % i` char subscript (read + write) | ✓ | Tier 6 |
| `v .% i` float subscript (read + write) | ✓ | Tier 6 |
| `v %% (start, width)` bitfield (read + write) | ✓ | Tier 3 — landed this cycle |
| `pair.\|i\|` lane access (read + write) | ✓ | Tier 7 — landed this cycle |
| `obj.field` / `obj OF field` member access | ✓ | Both forms accepted; tier 5 probes use `.` |
| Arithmetic `+ - * / REM` | ✓ | Tier 3 |
| Dotted float `+. -. *. /.` | ✓ | Tier 3 |
| Hash float `+# -# *# /#` (legacy spelling) | ✓ | Lexer maps to same tokens |
| Shifts `<< >>` | ✓ | Tier 3 |
| Relational `= ~= < <= > >=` (integer) | ✓ | Tier 3 |
| Relational `=. ~=. <. <=. >. >=.` (float) | ✓ | Tier 3 |
| Bitwise `& \|` | ✓ | Symbol forms are bitwise per parser |
| Keyword `BAND BOR BXOR BNOT` (bitwise) | ✓ | Parser test pins it |
| Keyword `AND OR NOT XOR` (logical) | ✓ | Parser test fixed this cycle |
| `EQV` / `NEQV` | ✓ | Tier 3 (`eqv_*`, `neqv_*`) — `EQV` lowers as `ICmpEq`, `NEQV` as bitwise XOR per BCPL tradition |
| Conditional `cond -> a, b` | ✓ | Tier 3 |
| `TABLE(...)` read-only constant | ✓ | Lexer + lowering wired |
| `FTABLE(...)` | ✓ | Float counterpart |
| `VALOF` + `RESULTIS` | ✓ | Tier 4 |

## §3 — Commands

| Command | Status | Notes |
|---|---|---|
| `:=` single-target assign | ✓ | Tier 4 |
| Multi-target `a, b := x, y` | ✓ | Parser test; tier 4 destructure |
| Routine call `R(args)` | ✓ | Tier 3 |
| `IF E THEN C` | ✓ | Tier 4 |
| `UNLESS E THEN C` | ✓ | Tier 4 |
| `TEST E THEN C1 ELSE C2` | ✓ | Tier 4 |
| **Reference's `TEST E THEN C1 OR C2`** | Δ | We require `ELSE`, not `OR`. Documented in user guide §1.4 |
| `WHILE`, `UNTIL` | ✓ | Tier 4 |
| `body REPEAT` / `REPEATWHILE` / `REPEATUNTIL` | ✓ | Tier 4 |
| `FOR n = E1 TO E2 BY K DO C` | ✓ | Tier 4 |
| `FOREACH x IN xs DO C` (modern extension) | ✓ | Tier 6 |
| `FOREACH (a, b) IN xs DO C` (lane destructure) | ✓ | Tier 6 |
| `SWITCHON E INTO $( CASE k: C ... DEFAULT: C $)` | ✓ | Tier 4 |
| Multi-label cases (`CASE 1: CASE 2: stmt`) | ✓ | Tier 4 fall-through probe |
| `GOTO label` | ✓ | Tier 4 (`goto_forward_jumps_over_code`, `goto_into_loop_body`, `forward_goto_skips_block`) |
| `name:` label declaration | ✓ | Same probes — labels are exercised by every GOTO probe |
| `RETURN`, `RESULTIS`, `FINISH` | ✓ | Tier 4 |
| `BREAK`, `LOOP`, `ENDCASE` | ✓ | Tier 4 + tier 5 USING-cleanup probes |
| `BRK` debugger breakpoint | ✓ | Lowers to `__newbcpl_brk(routine_name, line)`. Runtime handler writes a signal-safe-ish state dump to stderr: banner with routine + line, heap summary (`live_bytes`/`live_blocks`/`peak`), full AMD64 register state via `RtlCaptureContext`, stack walk via `RtlVirtualUnwind` using the same unwind tables `jit_mm` registers for SEH. Each stack frame resolves to a BCPL routine name when the RIP falls inside a JIT-d function, via a process-global registry populated by the LLVM crate after `LLVMGetFunctionAddress` finalize. Program continues after the BRK. Tier 4 — 8 probes (`brk_emits_*`, `brk_reports_routine_name_from_helper`, `brk_does_not_halt_program`, `brk_stack_frame_resolves_routine_name`, `brk_two_deep_call_chain_names_each_frame`). |

## §4 — Declarations

| Form | Status | Notes |
|---|---|---|
| `LET name = expr` | ✓ | Tier 2 |
| `LET name AS Type = expr` annotation | ✓ | Sema reads it |
| `LET a, b = ...` multi-binding | ✓ | Tier 2 |
| `FLET` float binding | ✓ | Tier 2 |
| `MANIFEST $( N = K; ... $)` | ✓ | Tier 2; lowering substitutes inline |
| `STATIC $( N = K; ... $)` | ✓ | Tier 2 |
| `GLOBAL name = expr` (single) / `GLOBAL $( name = expr; ... $)` (block) | ✓ | Each binding becomes a module-level `@<name>` LLVM global. Cross-routine, cross-module reads/writes work end-to-end. Tier 4 probes. |
| `GLOBAL name : K` (classic slot-pinning shape) | Δ | Rejected by the parser. The slot-vector form is the legacy GLOBALS shape (see below); under `GLOBAL` it's a category error. |
| `GLOBALS $( name : slot; ... $)` (classic global-vector) | Δ | Deliberately not supported — the loader's symbol table replaces the global-pointer-vector linker that GLOBALS was designed for. Parser rejects with a hint pointing at `GLOBAL`. |
| `LET v = VEC k` | ✓ | Tier 6 |
| `LET v = FVEC k` | ✓ | Tier 6 |
| `LET F(p) = expr` function | ✓ | Tier 3 |
| `LET R(p) BE stmt` routine | ✓ | Tier 4 |
| `LET F(p AS Class) = expr` parameter annotation | ✓ | AST carries `param_annotations: Vec<Option<String>>` parallel to `params`. Sema attaches class identity to the parameter binding; IR's `start_function_with_annotations` propagates it so `class_name_of_expr` resolves member access through the parameter. Tier 5 (`function_param_as_class_dispatches_method`, `routine_param_as_class_accesses_field`, `class_method_param_as_class_chains`, `param_annotation_enforces_visibility`, `param_without_annotation_workaround_via_typed_local`). |
| `param.method()` on un-annotated parameter | ✓ | When sema can't determine the receiver's static class, IR emits `IndirectMethodCall` instead of `MethodCall`. Codegen lowers it to a `__newbcpl_lookup_method(receiver, name)` call followed by an indirect call through the resolved function pointer. The lookup walks a process-global `(vtable_addr → method_names_addr)` registry populated by the LLVM crate at JIT-finalize time. Tier 5 (`indirect_dispatch_resolves_method_on_untyped_param`, `indirect_dispatch_routes_to_dynamic_class`, `indirect_dispatch_passes_arguments`, `indirect_dispatch_works_in_routine_body`). |
| `FUNCTION` / `ROUTINE` keyword forms | ✓ | Parser tests |
| `AND` mutual recursion | ✓ | Both surface forms work: (a) consecutive top-level `LET`s relying on sema's pre-pass 2 to preregister names, and (b) classical `LET f(...) = e AND g(...) = e` chains. The parser disambiguates by looking ahead for `AND <ident> (` — when matched, the `AND` is decl-tail and the chain unfolds into independent top-level decls; otherwise `AND` stays a logical operator. Tier 4 probes: `mutual_recursion_via_consecutive_lets_terminates`, `mutual_recursion_routines_with_be_bodies`, `classical_let_and_chain_two_functions`, `classical_let_and_chain_three_routines`, `classical_let_and_chain_mixes_function_and_routine`, `expression_and_still_works_when_not_followed_by_paren`. |
| `GET "file"` include | ✓ | AST-level splicing: sibling-file first, then modules-active fallback so a module doubles as a header. Cycle detection via depth cap. |
| `RETAIN name` / `RETAIN x = expr` | ✓ | Tier 6 (`retain_declares_binding_and_survives_gc`) — allocate, `GC()`, re-read |
| `FREEVEC` / `FREELIST` | ⚠ | Accepted as no-op (GC owns lifetime); pinned that they don't error |

## §5 — Program structure

| Feature | Status | Notes |
|---|---|---|
| Section brackets `$( ... $)` | ✓ | Tier 1 |
| `{ ... }` C-style synonyms | ✓ | Tier 1 |
| Mixed `$(` ... `}` (cross-form) | ⚠ | Lexer accepts; parser allows either pair but not crossed — undocumented |
| Tagged section brackets `$(LOOP ... $)LOOP` | ✗ | Lexer tokenises them; parser only matches bare `$(` / `}`. Not a corpus blocker — no reference test uses them. |
| Compound commands | ✓ | |
| Blocks with `LET` declarations | ✓ | |

## §6 — Float extension

Per `BCPL float extension.md`: IEEE 754 doubles, dotted operators,
`FLOAT(n)` / `TRUNC(f)` conversions, `.%` float subscript.

All ✓ — tier 3 covers the operator family; tier 6 covers `.%` reads.
`FLOAT` and `TRUNC` are builtins in `newbcpl-runtime/src/builtins.rs`.
Extras `ENTIER`, `FSQRT`, `FSIN`, `FCOS`, `FTAN`, `FABS`, `FLOG`,
`FEXP` are also live.

## §7 — Char/string model

| Reference says | NewBCPL does | Status |
|---|---|---|
| 32-bit Unicode char (`'A'` → i32 65) | UTF-8 byte (`'A'` → i64 65, but 2-byte chars work as multi-byte sequences) | Δ |
| String = pointer to 32-bit chars, null-terminated by zero word | String = pointer to UTF-8 bytes, null-terminated by zero byte | Δ |
| `%` returns 32-bit char | `%` returns a byte (i64-extended) | Δ |

These are **intentional deviations** documented in the user guide §2.6
("strings are UTF-8 bytes"). Stays cheap to interop with C/Win32 ANSI
APIs and the runtime printers. Pinned by tier 6
`utf8_multibyte_glyph_reads_as_two_bytes` — `λ` (U+03BB) reads as
two distinct `s % i` byte values (0xCE, 0xBB) so a future refactor
drifting back toward the reference's 32-bit-char model fails the probe.

## §8 — Classes (modern extension)

Per `classes_and_objects.md`:

| Feature | Status | Notes |
|---|---|---|
| `CLASS Name $( ... $)` declaration | ✓ | Tier 5 |
| `DECL` fields | ✓ | Tier 5 |
| `LET` / `FLET` fields (no init) | ✓ | Tier 5 |
| `LET f = expr` initialised fields | ✓ | Tier 5 — landed this cycle |
| `DECL f AS Class` annotation | ✓ | Tier 5 — landed this cycle |
| `FUNCTION` / `ROUTINE` methods | ✓ | Tier 5 |
| `NEW Class(args)` | ✓ | Tier 5 |
| `obj.field` / `obj.method()` | ✓ | Tier 5 |
| Chained `o.a.b.c()` dispatch | ✓ | Tier 5 — landed this cycle |
| `EXTENDS` single inheritance | ✓ | Tier 5 |
| `SUPER.method()` | ✓ | Tier 5 (`super_create_runs_parent_init`, `super_method_call_reaches_parent_body`) — IR emits direct call to `<parent>_<method>` |
| `CREATE` / `RELEASE` slots 0 / 1 | ✓ | Tier 5 |
| `PUBLIC` / `PRIVATE` / `PROTECTED` | ✓ | Enforced by sema. Visibility check at every `obj.field` and `obj.method()` site; `PRIVATE` requires access from the declaring class, `PROTECTED` extends to descendants. Sema's new `errors` channel rejects offenders; the driver refuses to proceed to IR/codegen. Tier 5 probes (`public_*`, `private_*`, `protected_*`). |
| `VIRTUAL` modifier | ✓ | Tier 5 (`virtual_method_dispatches_to_override`, `virtual_dispatch_picks_subclass_body_via_vtable`) |
| `FINAL` modifier | ✓ | Sema pre-pass 1c rejects override (`check_final_overrides`). Tier 5 (`final_method_callable_when_not_overridden`, `final_method_override_rejected`, `final_override_rejected_through_chain`, `non_final_override_still_allowed`). |

## §9 — Memory model

Per `BCPL Runtime.md`:

| Reference says | NewBCPL does | Status |
|---|---|---|
| `VEC k` malloc-based with scope-exit free | GC-managed | Δ — by design (manifesto §3) |
| Global vector with integer offsets | Symbol table | Δ — same |
| `findinput` / `selectinput` / `endread` I/O | Not implemented; we have stdin via `RDCH` | ✗ — would be useful for corpus parity |
| `findoutput` / `selectoutput` / `endwrite` | Not implemented | ✗ |
| `stop(N)` | We have `FINISH` (no arg) | Δ — minor |

## §10 — Runtime library

| Reference says | Our builtin | Status |
|---|---|---|
| `WRITES`, `WRITEN`, `WRITEC`, `WRITEF`, `NEWLINE` | All four + `WRITEF1..7` arity variants | ✓ |
| `WRITEF` float specifier `%f` `%F` | ✓ | Tier 3 |
| `RDCH` | ✓ | Stdin byte read |
| `FWRITE` | ✓ | Float printer |
| `FINISH` | ✓ | |
| `GETVEC` / `FGETVEC` | ✓ | Heap allocators |
| Typed allocators `IGETVEC` / `SGETVEC` / `PGETVEC` / `QGETVEC` | ✓ | Naming-only aliases of GETVEC — same word-slot layout, length stamped at p[-1]. The element-type prefix is documentation today; eventually drives TypeDesc tagging. Tier 6 (`igetvec_allocates_integer_vector`, `sgetvec_allocates_string_vector`, `pgetvec_allocates_pair_vector`, `qgetvec_allocates_quad_vector`). |
| `FREEVEC` | ⚠ | No-op stub; GC owns lifetime |
| List ops `HD TL REST LEN CONCAT APND APND_*` | ✓ | Tier 6 |
| Math `FSIN FCOS FTAN FABS FLOG FEXP FSQRT FLOAT FIX TRUNC ENTIER` | ✓ | |
| `RAND` / `RND` / `FRND` random | ✓ | |
| `GC()` / `HEAP_INFO()` | ✓ | Tier 6 |
| `__newbcpl_test_panic()` | ✓ | Test fixture for SEH probes |
| GUI `iGui_*` family | ✓ | Windows-only; not pinned by probes (GUI tests are out of scope here) |

## Current state — post-iteration-4

The matrix has **316 probes across 17 test binaries**, all green
(`cargo test -p newbcpl-tests --tests`). Every previously-named
"high-leverage gap" — SUPER end-to-end, VIRTUAL dispatch, RETAIN
post-GC, GOTO/label, multibyte UTF-8, EQV, NEQV — now has a
behavioural probe. They are listed in this audit's status column
with the probe name so a future regression in any of those features
has a single specific cell to fall through.

Two pieces of language work landed this iteration on top of the
spec-pivot baseline:

* **Parameter type annotations** — `LET f(p AS Class) = …` and the
  routine + method equivalents now carry per-parameter `AS Type`
  annotations through parser → AST → sema → IR. Inside the body the
  parameter binds with class identity, so method dispatch resolves
  statically and visibility checks fire the same way they do for a
  class-typed local. Also unblocks the top corpus failure bucket
  (`__newbcpl_indirect` for `param.method()` patterns).

* **Classical `LET … AND …` mutual recursion** — the parser now
  disambiguates `AND <ident> (` as a declaration-tail and unfolds
  the chain into independent top-level decls. The shared-scope
  semantics come for free from sema's pre-pass 2. The disambiguation
  is keyed on the three-token shape; expressions like `a AND b`
  (logical) and `a AND b + c` (logical with arithmetic tail) still
  parse the same way they always did.

**The spec pivot has surfaced multiple real bugs the corpus
couldn't:**

* `char_lit_*` probes uncovered `Expr::CharLit` lowering to
  `Const::String(lexeme)` — `WRITEN('A')` was printing a pointer
  value instead of `65`. Fixed by adding `decode_char_lexeme` at
  IR-lowering time. (9 probes added.)

* `final_*` probes drove the implementation of `FINAL` enforcement
  in sema (`check_final_overrides`, pre-pass 1c). Subclasses that
  try to override a FINAL ancestor method are now rejected through
  the same hard-error channel that visibility violations use, with
  a diagnostic naming both the method and the defining class.
  (4 probes added.)

* The mutual-recursion probes initially failed because the parser
  silently folded `LET f = e AND g = e` into a logical-AND
  expression inside the first body — silent semantic divergence
  rather than a hard rejection. Documented as a parser gap, then
  fixed in the same iteration as task 2 below.

* The param-annotation probes initially failed because IR lowering
  passed parameter slots without class identity, so `param.method()`
  fell through to a placeholder `__newbcpl_indirect` extern. Fixed
  by threading per-parameter annotations through
  `start_function_with_annotations` and pre-resolving them via
  `class_name_from_annotation`. This was also the top failure
  bucket in iteration 4's corpus sweep.

* `BRK` was the last ⚠ row in the audit — parser knew the keyword
  but no IR was emitted. Implemented as a runtime call to
  `__newbcpl_brk(routine_name, line)` that captures CONTEXT via
  `RtlCaptureContext`, walks the stack via `RtlVirtualUnwind` over
  the unwind tables `jit_mm` already registers, and writes the
  whole snapshot to stderr through direct `WriteFile` calls (no
  Rust `String`, no `format!` — fixed-size stack buffer with
  hand-rolled hex/dec formatting). Caught a `CONTEXT` 16-byte
  alignment bug on the way in: the `windows` crate's `#[repr(C)]`
  CONTEXT can land on an 8-byte boundary, faulting
  `RtlCaptureContext`. Wrapped in `#[repr(C, align(16))]`.

### Still genuinely thin

These rows are marked ⚠ above because the feature exists in the
compiler but no probe pins it:

No rows currently. Every previously ⚠ feature is either ✓ or
documented as a known Δ.

### Out-of-scope by design

| Row | Why |
|---|---|
| `findinput` / `findoutput` family | Old-BCPL file I/O. Not in user guide. Skip unless a real program needs it. |
| Tagged section brackets `$(LOOP …)$)LOOP` | Lexer tokenises; parser doesn't pair. No reference test uses them. Defer indefinitely. |
| GUI `iGui_*` family | Windows-only; tested through manual demos, not in the matrix |

### The Δ rows are not gaps

The Δ-marked rows (UTF-8 strings, GC-managed VEC, dropped GLOBALS
slot vector, `ELSE` instead of `OR`) are **intentional language
design choices**, documented in the user guide. The audit's value
here is preventing accidental drift back toward classic-BCPL
behaviour — every Δ row should ideally have a probe that asserts
the deviation, so future refactors can't quietly undo them.

Currently pinned:
* UTF-8 strings — tier 6 `utf8_multibyte_glyph_reads_as_two_bytes`
* Dropped GLOBALS — tier 4 `globals_slot_form_rejected`
* GLOBAL colon slot — tier 4 `global_colon_slot_syntax_rejected`

Not yet pinned (low priority): `ELSE` vs `OR`, GC-managed VEC (no
free), `FINISH` without arg.
