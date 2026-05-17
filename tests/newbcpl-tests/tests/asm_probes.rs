//! End-to-end probes for the `ASM { … }` extension.
//!
//! Each probe JITs a tiny program that defines an ASM procedure,
//! calls it, prints the result, and asserts the captured stdout.
//! Exercises the full pipeline: lexer (`ASM` keyword + bare-`#`
//! tolerance for asm comments), parser (`scan_asm_body` brace
//! balancing), sema (Function vs Routine kind), IR (`AsmProc`
//! payload), LLVM (module asm blob + `declare` for type-checked
//! calls), and the MCJIT assembler that links the body symbol
//! against its declare.
//!
//! Bodies are emitted verbatim — there is no `#name` substitution.
//! Authors reference Win64 ABI registers directly: `rcx`, `rdx`,
//! `r8`, `r9`, `qword ptr [rsp+40+8N]` for integer/pointer slots
//! 0..; `xmm0`..`xmm3`, `xmmword ptr [rsp+40+8N]` for float slots
//! when the parameter is annotated `AS FLOAT`.

use newbcpl_tests::expect_stdout as expect;

// ─── Integer ABI ──────────────────────────────────────────────────

#[test]
fn function_returns_a_word() {
    // `= ASM` puts the body in a value-producing function. Two i64
    // params land in rcx (slot 0) and rdx (slot 1); the result must
    // be left in rax for Win64.
    expect(
        "asm_function_returns_a_word",
        "LET fastmul(a, b) = ASM {\n  mov rax, rcx\n  imul rax, rdx\n  ret\n}\nLET START() BE $(\n  WRITEN(fastmul(6, 7))\n$)\n",
        "42",
    );
}

#[test]
fn routine_runs_without_returning() {
    // `BE ASM` puts the body in a no-result routine. Sema must
    // type it as Routine, not Function — the body never touches rax
    // so the caller has nothing meaningful to read back.
    expect(
        "asm_routine_runs_without_returning",
        "LET sink(x) BE ASM {\n  ret\n}\nLET START() BE $(\n  sink(99)\n  WRITES(\"done\")\n$)\n",
        "done",
    );
}

#[test]
fn five_params_use_the_shadow_space() {
    // Win64 puts the first four args in rcx/rdx/r8/r9 and spills
    // the rest above the 32-byte shadow home. Slot 4 lives at
    // [rsp+40]. Sum all five back together to prove every slot
    // was reachable.
    expect(
        "asm_five_params_use_the_shadow_space",
        "LET sum5(a, b, c, d, e) = ASM {\n  mov rax, rcx\n  add rax, rdx\n  add rax, r8\n  add rax, r9\n  add rax, qword ptr [rsp+40]\n  ret\n}\nLET START() BE $(\n  WRITEN(sum5(1, 2, 4, 8, 16))\n$)\n",
        "31",
    );
}

// ─── Float ABI ────────────────────────────────────────────────────

#[test]
fn float_function_returns_a_double() {
    // `AS FLOAT` on each parameter routes them through XMM (slot 0
    // → xmm0, slot 1 → xmm1). The trailing `AS FLOAT` after the
    // parameter list does the same for the return value: f64 in
    // xmm0. We add in place — no leading load needed because `a`
    // already lives in xmm0, the return register.
    expect(
        "asm_float_function_returns_a_double",
        "LET fadd(a AS FLOAT, b AS FLOAT) AS FLOAT = ASM {\n  addsd xmm0, xmm1\n  ret\n}\nLET START() BE $(\n  FWRITE(fadd(1.5, 2.25))\n$)\n",
        "3.75",
    );
}

// ─── Lexer corner cases ───────────────────────────────────────────

#[test]
fn body_with_local_label_assembles_cleanly() {
    // Local labels in the body must survive `scan_asm_body` (the
    // brace counter walks tokens) and `build_module_asm_string`
    // must keep them at column 0 so GAS doesn't reject them.
    expect(
        "asm_body_with_local_label_assembles_cleanly",
        "LET sumn(n) = ASM {\n  xor rax, rax\n.loop:\n  add rax, rcx\n  dec rcx\n  jnz .loop\n  ret\n}\nLET START() BE $(\n  WRITEN(sumn(10))\n$)\n",
        "55",
    );
}

#[test]
fn hash_comment_inside_body_survives() {
    // A bare `#` followed by a non-digit used to be a lexer error
    // (the number-literal scanner expected a digit). The ASM
    // extension relaxed that so GAS-style line comments inside an
    // ASM body parse without complaint. The body is emitted
    // verbatim, so the comment survives all the way to the
    // assembler (which knows what to do with it).
    expect(
        "asm_hash_comment_inside_body_survives",
        "LET answer() = ASM {\n  mov rax, 42  # the answer\n  ret\n}\nLET START() BE $(\n  WRITEN(answer())\n$)\n",
        "42",
    );
}
