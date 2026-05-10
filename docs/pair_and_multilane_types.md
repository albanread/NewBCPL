# PAIR and the multi-lane SIMD types

NewBCPL inherits the reference compiler's SIMD-lane value types. They are
**packed values**, not heap objects — each one fits in a fixed-size
register-class chunk. The lane widths and totals are *normative*: changing
them would break ABI compatibility with the reference's runtime and the
binary layout that `WRITEF`'s `%P`/`%Q`/`%R` specifiers assume.

## Authoritative widths

| Type     | Lane shape       | Total bits | Native register | NEON arrangement |
|----------|------------------|-----------:|-----------------|------------------|
| `PAIR`   | 2 × i32          | **64**     | one i64 / D-reg | 2S (signed)      |
| `FPAIR`  | 2 × f32          | **64**     | one i64 / D-reg | 2S (float)       |
| `QUAD`   | 4 × i16          | **64**     | D-reg           | 4H               |
| `OCT`    | 8 × i8           | **64**     | D-reg           | 8B               |
| `FQUAD`  | 4 × f32          | **128**    | one Q-reg       | 4S (float)       |
| `FOCT`   | 8 × f32          | **256**    | two Q-regs      | 2×4S             |

Sources: [reference/book/NewBCPL_Types.md](../reference/book/NewBCPL_Types.md)
("PAIR — A value type representing two 32-bit integers packed into a 64-bit
value") and
[reference/docs/implement_vectors.md](../reference/docs/implement_vectors.md)
(the NEON arrangement table).

## Why this matters for our LLVM lowering

PAIR/FPAIR/QUAD/OCT all fit in **one i64 word**. Lowering them as wider
LLVM vector types breaks every place that treats a "BCPL word" as 64 bits:

- A `LET p = PAIR(10, 20)` stored into an i64 slot must occupy exactly
  8 bytes — storing `<2 x i64>` (16 bytes) clobbers the next slot.
- A `LIST(PAIR(...), PAIR(...), ...)` allocates one i64 per element; a
  wider PAIR representation overruns the buffer.
- `WRITEF`'s `%P` / `%Q` / `%R` specifiers in the reference runtime
  unpack the i64 argument as `2 × i32` / `4 × i16` / `8 × i8`. If the
  caller passes a wider vector, the format reader sees garbage.
- Arithmetic on PAIRs (`pair1 + pair2`) is **lane-wise on i32 halves**,
  not on a 128-bit integer. The IR-to-LLVM lowering is `add <2 x i32>`
  via a temporary cast, not a 64-bit `iadd`.

## Concrete bit layout

`PAIR(low, high)` packs into a 64-bit word with `low` in bits 0..31 and
`high` in bits 32..63, both stored as signed two's-complement i32.
Construction:

```
packed = ((i64)(i32)low) & 0xFFFFFFFF | ((i64)(i32)high) << 32
```

Lane access (`pair.|0|` / `pair.|1|`) is sign-aware:

```
lane0 = (i32)packed                       → sign-extended back to i64 in arithmetic
lane1 = (i32)(packed >> 32)               → arithmetic shift right
```

`QUAD` and `OCT` apply the same scheme with 16-bit and 8-bit lanes
respectively (4 / 8 lanes total). `FPAIR` packs two `f32`s by their bit
patterns into the same i64 layout, accessed through bitcasts.

`FQUAD` is the first type that genuinely needs an LLVM vector — `<4 x
f32>` in a Q register. `FOCT` is `<8 x f32>` (256 bits, two Q registers).

## Implementation status (NewBCPL)

- IR currently models PAIR/FPAIR/QUAD/OCT as `<N x i64>` (wrong width).
  This works for *some* operations through coercion bandaids in
  `emit::as_int_word` and `emit::pack_vector_to_word`, but breaks
  arithmetic and list/vec storage as soon as multiple PAIRs are alive.
- The structural fix is to have `build_simd_vector` dispatch by
  `TypedKind`: produce a packed `i64` for the four 64-bit-total
  shapes, `<4 x f32>` for FQUAD, `<8 x f32>` for FOCT (or two Q-shaped
  halves on Windows ABI). Lane access becomes bit-shift / bitcast for
  the packed shapes; vector `extractelement` for the wider ones.
- Until that lands, `LIST(PAIR(...), ...)` and `FOREACH (a, b) IN
  list-of-pairs` rely on the band-aid pack-on-store path in
  `emit_vec_construct`.
