# The NewBCPL Programming Language

*A user's guide, in the spirit of K&R.*

---

## Preface

NewBCPL is BCPL as it might have been had it not been quietly retired
in the 1980s. The look of the source is unchanged: terse keywords,
section brackets, operators in place of methods, no decorative type
annotations. The semantics, however, have caught up to a machine that
has floating-point registers, SIMD lanes, a garbage collector, and a
graphical display.

This guide assumes you have written some BCPL before, or at least some
language in the Algol family. The examples are short, complete, and
runnable. The text is meant to be read in order; each chapter relies on
the one before it.

The compiler is `newbcpl-driver`. A source file conventionally has the
extension `.bcl`. The simplest invocation is

```
newbcpl-driver run hello.bcl
```

The driver scans `./modules-active/` for any helper modules, links
them, JIT-compiles your file, and calls its `START` routine.

---

## Chapter 1 — A Tutorial Introduction

### 1.1 Getting Started

Here is the traditional first program.

```bcpl
LET START() BE $(
    WRITES("Hello, world*N")
$)
```

The keywords `LET` and `BE` introduce a routine; `START` is the
entrypoint the runtime looks for. The section brackets `$(` and `$)`
group the body, exactly as `BEGIN` and `END` would in Algol or as `{`
and `}` would in C. NewBCPL also accepts `{` and `}` as direct synonyms;
they nest and pair like their `$(` cousins.

`WRITES` prints a string. The two characters `*N` inside the string are
the BCPL escape for newline. The full escape table is

```
    *N   newline           *T   tab
    *S   space             *B   backspace
    *P   form feed         *C   carriage return
    *"   double quote      **   asterisk
```

There is no `\` escape. The `*` is the workhorse; doubled `**` gives
you a literal asterisk.

### 1.2 Variables and Arithmetic

A variable is introduced with `LET`. Every variable has a type, but the
type is inferred from its first value — the compiler always knows; the
programmer does not have to say. The same idea governs the rest of the
language: it looks untyped, but it is secretly typed.

```bcpl
LET START() BE $(
    LET a = 10
    LET b = 3
    LET q = a / b           // integer divide: q = 3
    LET r = a REM b         // remainder:      r = 1
    WRITEF("q=%d r=%d*N", q, r)
$)
```

`WRITEF` is the printf of BCPL. The specifiers it understands are

```
    %d %i %N   signed decimal       %x   lowercase hex
    %X         uppercase hex        %o   octal
    %c         character byte       %s   null-terminated string
    %f %F      double precision     %%   literal '%'
```

Variants `WRITEF1` through `WRITEF7` are also available, taking an
explicit argument count; the unsuffixed form picks the right arity from
the call.

### 1.3 Floating-Point

A literal with a decimal point or an exponent is a FLOAT.

```bcpl
LET START() BE $(
    LET pi   = 3.14159
    LET area = pi *. 4.0 *. 4.0           // dotted * is float multiply
    FWRITE(area)
    NEWLINE()
$)
```

The dotted operators (`+.`, `-.`, `*.`, `/.`, `=.`, `~=.`, `<.`, `<=.`,
`>.`, `>=.`) make the FLOAT meaning explicit. They are not required when
both operands are already FLOAT — sema sees the types and selects FADD
over IADD on its own — but the dotted form is the way to *insist*. The
older convention writes `+#`, `-#`, `*#`, `/#`; both spellings tokenise
to the same operator.

Conversions are explicit and named:

```
    FLOAT(n)    integer → double
    FIX(f)      double  → integer (truncate toward zero)
    TRUNC(f)    same as FIX
    ENTIER(f)   floor
    FSQRT(f)    square root
    FSIN, FCOS, FTAN, FABS, FLOG, FEXP    libm equivalents
```

If you assign a FLOAT to an INT variable the compiler emits a
truncation instruction and warns; it never refuses. WORD is the
universal escape hatch — a binding declared or coerced to WORD accepts
anything and codegen treats it as a 64-bit register.

### 1.4 Conditional Execution

The two surface forms are familiar:

```bcpl
IF n < 0 THEN n := 0 - n           // ELSE optional
TEST n < 0 THEN WRITES("neg*N")    // ELSE required
              ELSE WRITES("pos*N")
UNLESS done DO step()              // UNLESS is the negation of IF
```

`IF` and `TEST` are surface synonyms; they differ only in whether the
`ELSE` branch is mandatory. The keyword between condition and body is
`THEN` (or, traditionally, `DO`); the parser accepts either. There is
*no* `OR` else-marker — `OR` is a binary operator only.

The expression form is the conditional ternary:

```bcpl
LET sign = x < 0 -> -1, x > 0 -> 1, 0
```

`cond -> then-expr, else-expr` reads "if cond then this, else that."
It associates to the right, so it stacks naturally as above.

### 1.5 Loops

Five shapes cover everything.

```bcpl
WHILE p DO p := p!next             // top-test
UNTIL done DO step()               // top-test, negated
FOR i = 1 TO 10 BY 2 DO use(i)     // counted; BY defaults to 1
body REPEAT                        // bottom-test, no condition
body REPEATWHILE cond              // bottom-test
body REPEATUNTIL cond              // bottom-test, negated
```

The postfix `REPEAT` / `REPEATWHILE` / `REPEATUNTIL` attach to *any*
statement, not just blocks, so

```bcpl
n := n - 1 REPEATUNTIL n = 0
```

is a perfectly ordinary loop. Inside any loop, `BREAK` quits and `LOOP`
goes round again. The names are the BCPL ones; they are *not* `break`
and `continue`.

For collections — vectors, lists, strings, SIMD packs — use `FOREACH`:

```bcpl
LET xs = LIST(1, 2, 3, 5, 8)
FOREACH x IN xs DO WRITEN(x)

LET pts = LIST(PAIR(1,2), PAIR(3,4))
FOREACH (a, b) IN pts DO WRITEF("(%d,%d) ", a, b)
```

The parenthesised form unpacks each element's SIMD lanes into the
named bindings. The number of names must match the lane count of the
element type.

### 1.6 Routines and Functions

A routine performs an effect; a function returns a value. The BCPL
syntax marks the distinction in the binder, not in the call site.

```bcpl
LET greet(who) BE $(                   // routine: ends with BE stmt
    WRITES("Hello, ")
    WRITES(who)
    WRITES("*N")
$)

LET square(x) = x * x                  // function: ends with = expr

LET sum_to(n) = VALOF $(               // function whose body is a block
    LET total = 0
    FOR i = 1 TO n DO total := total + i
    RESULTIS total
$)
```

`VALOF` is the expression form of a block: it evaluates statements
until one of them executes `RESULTIS expr`, then yields that
expression's value. A function whose body needs more than one statement
is the canonical use of `VALOF`.

Mutual recursion is written with `AND`:

```bcpl
LET even(n) = n = 0 -> TRUE, odd(n - 1)
AND odd(n)  = n = 0 -> FALSE, even(n - 1)
```

The parser disambiguates the declaration-tail `AND` from the
expression-level logical `AND` by looking ahead for the
`AND <identifier> (` shape — the form a mutual binding must use.
Plain expressions like `a AND b` or `a AND b + c` still parse as
logical AND. A consecutive pair of top-level `LET`s without the
`AND` connector also works as mutual recursion — forward references
resolve through sema's preregistration pass regardless of source
order.

The compiler accepts `LET name(...) = expr` and `LET name(...) BE stmt`
as a single declaration form. It also recognises the longer keyword
forms `FUNCTION name(...) = expr` and `ROUTINE name(...) BE stmt`; they
mean the same thing and are interchangeable.

Parameters may carry an optional `AS Type` annotation:

```bcpl
LET print_distance(p AS Point) BE WRITEN(p.distance_from_origin())
```

The annotation gives sema the parameter's class identity inside the
body. Without it, `p` is just a word and `p.method()` falls back to
runtime name-keyed dispatch (which works, but loses compile-time
visibility checks and direct-dispatch routing). Annotations apply
to function parameters, routine parameters, and class-method
parameters — anywhere a parameter list appears.

---

## Chapter 2 — Types and Operators

### 2.1 The Register-Class Lattice

Types in NewBCPL correspond to register classes on the target machine,
not to algebraic categories. Each type names a way of holding a value.

```
    INT, WORD       64-bit integer in a general register
    FLOAT           64-bit double in a floating-point register
    STRING          pointer to UTF-8 bytes in the heap
    PAIR            2 × 64-bit ints, packed into one 128-bit V-reg
    FPAIR           2 × doubles, packed into one 128-bit V-reg
    QUAD, FQUAD     4-lane equivalents, 256-bit
    OCT, FOCT       8-lane equivalents, 512-bit (SVE / AVX-512)
    VEC, FVEC       heap-allocated word / float arrays
    LIST            heap-allocated singly-linked cons cells
    OBJECT          heap-allocated class instance
```

WORD is the escape hatch: any value whose precise type sema cannot
prove is WORD, and arithmetic on WORDs is integer arithmetic. A
classical 1974 BCPL program is, in this dialect, a program where every
binding has type WORD; it runs unchanged.

Type is fixed at first assignment. Subsequent assignments coerce when
possible: FLOAT ← INT is silent (SCVTF), INT ← FLOAT warns (FCVTZS).
You may pin the inference explicitly:

```bcpl
LET x AS FLOAT  = 0                    // FLOAT, even though 0 is INT
LET p AS ^STRING = "hello"             // pointer to a string
LET xs AS ^LIST OF INTEGER = LIST(1, 2)
```

`AS` annotations are documentation; sema verifies them when present.
`^` means "pointer to"; `OF` chains a container's element type.

### 2.2 Operators

The full ladder of binary operators, in *decreasing* precedence:

```
    postfix      f(args)  v!i  v%i  v.%i  v%%(s,w)  obj.field  obj OF field
    unary        -x  ~x  BNOT x  NOT x  !x  @x  %x  HD x  TL x  REST x  LEN x
    multiplicative  *  /  REM       *.  /.        (FLOAT)
    additive       +  -             +.  -.        (FLOAT)
    shift          <<  >>
    relational     =  ~=  <  <=  >  >=     =. ~=. <. <=. >. >=.   (FLOAT)
    and            &  BAND  AND                   (BAND bitwise; AND logical)
    or             |  BOR  BXOR  OR  XOR  EQV  NEQV
    conditional    cond -> then-expr, else-expr
```

A few notes worth memorising:

- `=` is equality; assignment is `:=`. They are different tokens.
- `NOT` is *logical* (returns 0 or 1); `BNOT` and `~` flip every bit.
  Similarly `AND` / `OR` / `XOR` are logical; `BAND` / `BOR` / `BXOR`
  are bitwise. The symbol `&` is bitwise (matching C); `|` is bitwise.
- `REM` is integer remainder. There is no floating-point remainder.
- The character-subscript operator `%` and the byte-deref prefix `%`
  look alike; context disambiguates. `v % i` reads a byte from a
  packed-byte vector; `% p` dereferences a byte pointer.
- The bitfield operator is `v %% (start, width)`. It reads `width`
  bits starting at bit `start`.
- The SIMD lane operator is `pair . | n |`. The lane index is between
  the pipes. Inside the pipes, `|` is the closing delimiter, not the
  bitwise-or operator; the parser handles this automatically.

### 2.3 The Subscript Family

BCPL distinguishes vector kinds by the subscript operator, not by the
vector's declared type.

```bcpl
LET v = VEC 10                         // word vector, indices 0..10
v!3 := 42                              // word write
LET w = v!3                            // word read

LET bytes = VEC 4                      // packed bytes via %
bytes % 0 := 'A'

LET fs = FVEC 8                        // float vector
fs .% 2 := 3.14                        // float subscript
```

The classic identity `v!i = !(v + i)` still holds: `!` is "indirect
through pointer." `@x` is "address of x." `% p` and `.% p` are the byte
and float pointer-deref forms.

### 2.4 SIMD Packs

PAIR, FPAIR, QUAD, FQUAD, OCT, and FOCT are *value* types: they live
in a single vector register, not on the heap.

```bcpl
LET v = PAIR(3, 4)                     // <2 x i64>
LET w = PAIR(1, 2)
LET sum = v + w                        // one SIMD add: PAIR(4, 6)
LET first = sum.|0|                    // extract lane 0: 4
```

Equality on packs is lane-wise: `v = w` returns true iff every lane
agrees. Inequality (`~=`) is the obvious negation.

A `FOREACH (a, b) IN list_of_pairs DO ...` destructures each element
into its lanes — see §1.5.

### 2.5 Heap Vectors and Lists

`VEC k` allocates k+1 words on the heap and returns a pointer to slot
0; `LEN v` returns the length stored one word *before* the data. `FVEC
k` is the float-vector analogue. `TABLE(a, b, c)` makes a read-only
integer table whose size is the count of initialisers; `FTABLE` is the
float version.

Lists are real cons cells:

```bcpl
LET xs = LIST(10, 20, 30)
WRITEN(HD xs)         // 10  — head value
LET rest = TL xs      // shares nodes; rest is the list (20, 30)
LET n = LEN xs        // 3   — O(1), maintained on each append
```

Lists are heterogeneous: each cell carries a small type tag so an atom
can be an integer, a float, a string pointer, a packed PAIR, or a
pointer to another list. `CONCAT(a, b)` returns a fresh list with
copies of a's atoms followed by copies of b's. `MANIFESTLIST(...)` is
a compile-time-constant list literal.

### 2.6 Strings

A string literal is a pointer to UTF-8 bytes in the program's
read-only data segment. The `*` escapes (see §1.1) are cooked at
compile time. Strings are not arrays of characters in source — to
inspect a byte, use `s % i`.

```bcpl
LET msg = "Hello, world*N"
WRITES(msg)
LET first = msg % 0                    // 'H' = 72
```

---

## Chapter 3 — Statements and Control

### 3.1 Blocks and Declarations

A block is `$( ... $)` or `{ ... }`. Inside a block, `LET` declarations
may appear anywhere, not only at the top — each binding is visible from
its declaration to the end of the enclosing block.

```bcpl
$(
    LET a = 1
    WRITEN(a)              // 1
    LET b = 2
    WRITEN(a + b)          // 3
$)
```

### 3.2 Assignment

`:=` is the assignment operator. The left side may be a bare name, a
subscripted vector, a member access, or a pointer indirection.

```bcpl
v!3      := 42
obj.x    := 100
SELF.y   := other.y
!ptr     := value
% bptr   := 'A'
```

Multiple targets can be assigned in parallel from a parenthesised list
of values:

```bcpl
LET a = 1
LET b = 2
a, b := b, a                           // swap; a is now 2, b is now 1
```

The count of targets must equal the count of values. Single-RHS
destructure for SIMD packs:

```bcpl
LET hi, lo = pair                      // pair.|0|, pair.|1|
```

### 3.3 SWITCHON

```bcpl
SWITCHON c INTO $(
    CASE 0:                            // multiple labels permitted
    CASE 1:
        WRITES("low*N")
        ENDCASE
    CASE 2:
        WRITES("two*N")
        ENDCASE
    DEFAULT:
        WRITES("other*N")
        ENDCASE
$)
```

`ENDCASE` is the case-terminator; cases do not fall through unless you
omit it. `CASE 0: CASE 1:` (no body between them) does fall through:
both labels share whatever body follows.

### 3.4 Non-Local Control

The classical jump statements:

```
    RETURN      leave a routine
    RESULTIS e  leave the current VALOF with value e
    FINISH      terminate the entire program
    BREAK       leave the nearest loop
    LOOP        next iteration of the nearest loop
    ENDCASE     leave the current SWITCHON arm
    GOTO label  unconditional jump to label:
    BRK         debugger breakpoint (no operand)
```

A label is `name:` standing on its own as a statement. `GOTO` is the
escape valve for situations that resist the structured forms; in
practice you will rarely need it.

`BRK` is more useful than a classical BCPL "debugger breakpoint" —
the runtime synthesises a structured snapshot of the program's
state and writes it to stderr, then returns and execution continues.
A typical dump:

```
=== BRK in routine `divide` at line 17 ===
heap:    live=14336 bytes  blocks=42  peak=14336 bytes
context: rip=00007FF6C807EC7F  rsp=000000A4F1FFEBE0  rbp=000000A4F1FFEC00
         rax=000000000000002A rbx=0000000000000000 rcx=00007FF6C8023C70
         ...
stack:
  #0  rip=0000024C73170041  in helper
  #1  rip=0000024C7317006C  in START
  #2  rip=00007FF6C801703E
  ...
=== END BRK ===
```

The handler uses direct `WriteFile` to `STD_ERROR_HANDLE` with
fixed stack buffers — no heap allocation, no `format!`, robust
against a corrupted GC heap — so it's safe to drop a `BRK` into a
program that's misbehaving and read the snapshot. Stack frames that
fall inside JIT-d code resolve to their BCPL routine names; frames
in host, runtime, or OS code stay as raw addresses.

### 3.5 Compile-Time and Static Storage

```bcpl
MANIFEST $(
    LIMIT = 100
    PI    = 3.14159
$)

STATIC $(
    counter = 0
    name    = "anonymous"
$)
```

A `MANIFEST` value is substituted at every reference site; it has no
runtime address. A `STATIC` has a single address whose lifetime is the
whole program. A `GLOBAL` introduces a module-scope binding — visible
from every routine in the file, and from other modules through the
loader's symbol table:

```bcpl
GLOBAL counter = 0
GLOBAL $(
    cursor = 0
    flags  = 0
$)
```

Each binding becomes a single named slot. Reads and writes route
through the symbol; cross-module references resolve at link time the
same way function calls do. The single-line form (`GLOBAL counter = 0`)
and the block form are interchangeable.

The classic `GLOBALS $( name : 42; ... $)` form — slot-pinning into a
shared pointer vector — is **not** carried in NewBCPL. The loader's
symbol table already does the cross-module job, so the global-vector
machinery would be redundant; the compiler rejects `GLOBALS` and the
`name : K` slot syntax with a parse error pointing at `GLOBAL` instead.

### 3.6 `GET` Directives

```bcpl
GET "constants.bcl"
GET "geom"           // resolves to modules-active/geom.bcl if absent locally
```

`GET "name"` splices the declarations of another source file into the
current compilation unit. It is the way to share **compile-time
information** — `MANIFEST` constants, `CLASS` declarations, helper
`LET` declarations — between files. Runtime function calls don't need
it: cross-module calls resolve through the loader's symbol table on
their own (see Chapter 6).

Path resolution tries three places in order:

1. **Absolute path** — used verbatim.
2. **Sibling file** — relative to the directory of the source file
   doing the GET. The `.bcl` extension is added if you didn't write
   one (`GET "constants"` and `GET "constants.bcl"` find the same
   file).
3. **Modules-active fallback** — `modules-active/<name>.bcl`. This
   makes a module file double as a header: `GET "geom"` from any
   program imports `modules-active/geom.bcl`'s declarations into the
   current compilation unit, while `geom`'s runtime functions are
   still linked separately by the module loader.

Cyclic includes are detected by a depth cap; the compiler errors with
a clear diagnostic rather than recursing forever. Missing files error
with `GET "..." : file not found` and the search locations tried.

Note that `GET` is for compile-time information, modules-active is
for runtime linking. The two cover orthogonal axes and you'll often
use both — `GET "geom"` to see Geom's MANIFEST constants and CLASS
declarations at compile time, while the running program's `geom_*`
functions are linked through the module loader.

---

## Chapter 4 — Classes and Objects

NewBCPL has classes because modern programs have things that are
naturally objects — windows, file handles, parsers. The shape is
deliberately minimal.

```bcpl
CLASS Point $(
    DECL x, y                          // two integer fields

    ROUTINE CREATE(initialX, initialY) BE $(
        x := initialX
        y := initialY
    $)

    FUNCTION getX() = x                // single-expression method
    FUNCTION getY() = VALOF $( RESULTIS y $)

    ROUTINE moveTo(newX, newY) BE $(
        x := newX
        y := newY
    $)
$)

LET START() BE $(
    LET p = NEW Point(3, 4)
    WRITEN(p.getX())
    p.moveTo(10, 20)
$)
```

A class body declares fields with `DECL` (or `LET x, y` as a shorthand)
and methods with `ROUTINE` / `FUNCTION` (or the unified `LET` form).
The class layout reserves offset 0 for the vtable pointer; declared
fields start at offset 8.

`NEW Class(args)` allocates an instance on the GC heap and calls its
`CREATE` method with the given arguments. The `.` operator is method
or field access. Inside a method body, `SELF` refers to the receiver
and `SUPER` to the immediate parent class's slot of the same name.

### 4.1 Inheritance and Virtuality

```bcpl
CLASS Coloured EXTENDS Point $(
    DECL r, g, b

    ROUTINE CREATE(x0, y0, R, G, B) BE $(
        SUPER.CREATE(x0, y0)
        r := R; g := G; b := B
    $)

    VIRTUAL FUNCTION describe() = VALOF $(
        WRITEF("(%d,%d,%d)*N", r, g, b)
        RESULTIS 0
    $)
$)
```

A `VIRTUAL` method occupies a vtable slot; subclasses may override
it freely. A `FINAL` method **may not be overridden** — sema rejects
the offending subclass at compile time with a diagnostic naming
both the method and the defining class. The walk covers the full
inheritance chain, so `Base.m` being FINAL forbids `Sub.m`,
`SubSub.m`, etc. Visibility headers `PUBLIC:`, `PRIVATE:`, and
`PROTECTED:` switch the access level of subsequent members until
the next header; the default is `PUBLIC`. PRIVATE / PROTECTED are
sema-enforced at every member-access site — the access site's
enclosing class is checked against the member's defining class, and
`PROTECTED` extends access to descendant classes.

A method dispatched through `obj.method()` where `obj`'s class
isn't known statically (an un-annotated parameter, or a value
flowing through a generic container) resolves through a runtime
name-keyed lookup. Each class emits a `(vtable_addr,
method_names)` pair the runtime indexes by the instance's inline
vtable pointer — typed and untyped dispatch produce the same
behavioural result, the typed form just resolves at compile time.

### 4.2 Deterministic Cleanup with `USING`

The garbage collector handles ordinary memory: an unreachable object's
storage is reclaimed at some later collect, and if the class defines
a `RELEASE` method the collector runs it as a finaliser. That suffices
for things like windows where "released a moment later" is fine.

For resources where ordering matters — file handles, locks,
transactions, prepared statements — `RELEASE` needs to run *now*, not
whenever the GC next runs. The construct for that is `USING`:

```bcpl
CLASS File $(
    DECL handle

    ROUTINE CREATE(path) BE $( handle := host_open(path) $)
    ROUTINE RELEASE()    BE $( host_close(handle) $)

    ROUTINE writeLine(s) BE $( host_write(handle, s) $)
$)

LET render() BE $(
    USING f = NEW File("log.txt") DO $(
        f.writeLine("hello")
    $)
    // f.RELEASE() has already run here.
$)
```

`USING name = expr DO body` binds the value of `expr` to `name` for
the duration of `body`, then calls `name.RELEASE()` exactly once at
scope exit. The cleanup runs on every way out of `body` —
fall-through, `RETURN`, `RESULTIS`, `FINISH`, `BREAK`, `LOOP`, and
`ENDCASE` all release every USING they escape, innermost first.

Nesting works the way you'd expect:

```bcpl
USING tx = NEW Transaction(db) DO
    USING stmt = tx.prepare("INSERT INTO …") DO
        stmt.bind(args)
// stmt.RELEASE() runs first, then tx.RELEASE(); both before falling
// out of the surrounding scope.
```

The `MANAGED` keyword on a class declaration is accepted but advisory
— it documents intent ("this class should usually be inside a USING")
without enforcing it. Plain classes work in `USING` too; any class
with a `RELEASE` method is eligible.

---

## Chapter 5 — Memory

NewBCPL looks unmanaged but is secretly collected. The user never
writes `getvec` / `freevec` to balance a `VEC` allocation.

### 5.1 What Is on the Heap

- `VEC k` and `FVEC k` allocations — GC-tracked word and float
  vectors.
- `LIST(...)` — every cons cell is a GC object.
- `NEW Class(...)` — every instance is a GC object.
- String literals — read-only, *not* heap; they live in the program's
  data segment.

### 5.2 What Is Not

- Scalars (INT, FLOAT, WORD): registers and stack slots.
- SIMD packs (PAIR, FPAIR, QUAD, FQUAD, OCT, FOCT): single vector
  registers.
- `TABLE(...)` / `FTABLE(...)`: read-only constants in the data segment.

### 5.3 Lifetimes

The GC is a precise mark-sweep collector, stop-the-world,
single-threaded. It runs automatically when heap pressure crosses a
threshold; you can request a cycle explicitly with `GC()` and a status
dump with `HEAP_INFO()`.

The `RETAIN` statement pins a binding past its natural scope:

```bcpl
LET buf = VEC 1024
RETAIN buf                             // GC will not reclaim buf
```

`RETAIN name = expr` declares and pins in one step. For the manual
counterpart, `FREEVEC v` and `FREELIST l` are accepted but currently
no-ops — the GC is the policy. They remain in the language so classical
programs that call them compile unchanged.

### 5.4 Pointers

`@x` is the address of `x`. `!p` dereferences a word pointer; `%p`
dereferences a byte pointer; `.%p` dereferences a float pointer. The
null pointer literal is `?`. Pointers are integers under the skin —
all the classical bit-tricks still work; the GC does not look at them.

---

## Chapter 6 — Programs and Modules

A `.bcl` file that defines a `START` routine is a *program*. A `.bcl`
file with no `START` is a *module*. There is no other declarator.

### 6.1 Modules

Modules live in the active-modules folder, which is `./modules-active/`
by default, or whatever `$NEWBCPL_MODULES_ACTIVE` points at. Every
`.bcl` file inside it is loaded automatically in alphabetical order,
before the program is run. A module's top-level routines are
automatically prefixed with the module's filename stem; nothing else
needs to be declared.

```bcpl
// modules-active/maths.bcl
LET sq(x)   = x * x
LET cube(x) = x * x * x
LET clamp(x, lo, hi) = x < lo -> lo, x > hi -> hi, x
```

```bcpl
// examples/use-maths.bcl
LET START() BE $(
    WRITEN(maths_sq(7))            // calls into maths.bcl
    NEWLINE()
    WRITEN(maths_clamp(50, 0, 10))
    NEWLINE()
$)
```

The mangled name (`<stem>_<routine>`) is the namespace. Inside the
module's own source the routines are still called by their bare names;
from outside they are accessed under the mangled form. Modules may
call modules: cross-module references resolve at link time, so load
order between modules does not matter — backward and forward references
both succeed.

### 6.2 The Standard Library

These names are always available; they are Rust-resident built-ins
registered by the runtime, not BCPL modules.

```
    I/O:           WRITES  WRITEN  WRITEC  NEWLINE  WRITEF  RDCH  FWRITE
                   FINISH
    Allocation:    GETVEC  FGETVEC  PAIRS  FPAIRS  FREEVEC
    Lists:         HD  TL  TAIL  REST  CONCAT  LEN  APND  APND_FLOAT
                   APND_STRING  APND_OBJECT  APND_PAIR
    Math:          FSIN  FCOS  FTAN  FABS  FLOG  FEXP  FSQRT
                   FLOAT  FIX  TRUNC  ENTIER
    Random:        RAND  RND  FRND
    GC:            GC  HEAP_INFO
    GUI (Win):     iGui_OpenChild  iGui_CloseChild  iGui_BeginBatch
                   iGui_SubmitBatch  iGui_Clear  iGui_FillRect
                   iGui_StrokeRect  iGui_FillCircle  iGui_DrawLine
                   iGui_DrawText  iGui_NextEvent  iGui_Quit
```

The `igui_*` helper module (lowercase, in `modules-active/`) wraps the
raw `iGui_*` builtins to hide their `(ptr, len)` open-array convention.

### 6.3 A GUI Program

The runtime ships with a Direct2D / DirectWrite GUI on Windows. A
program opens its own window, paints into it, and consumes events from
a single mailbox.

```bcpl
LET START() BE $(
    LET win = igui_open("Shapes")
    igui_begin(win)
    igui_clear(0.10, 0.12, 0.16, 1.0)
    igui_fill_rect(40.0, 40.0, 140.0, 140.0, 0.92, 0.30, 0.30, 1.0)
    igui_text("Hello from BCPL!", 40.0, 200.0, 22.0, 1.0, 1.0, 1.0, 1.0)
    igui_submit()
$)
```

Colour components and coordinates are floats — the underlying ABI uses
XMM registers and integers would arrive in the wrong place. Use `1.0`,
not `1`.

For an interactive program, loop on `igui_next_event(...)` and dispatch
on its returned kind. See `examples/click-counter.bcl` and
`examples/event-pump.bcl` in the workspace; the event-kind table lives
in the comments at the top of `event-pump.bcl`.

---

## Chapter 7 — A Tour Through the Driver

The compiler is structured as a pipeline of phases, each of which can
be dumped to inspect what it produced.

```
    newbcpl-driver dump-tokens   foo.bcl
    newbcpl-driver dump-ast      foo.bcl
    newbcpl-driver dump-sema     foo.bcl
    newbcpl-driver dump-cfg      foo.bcl
    newbcpl-driver dump-ir       foo.bcl
    newbcpl-driver dump-llvm     foo.bcl
    newbcpl-driver dump-asm      foo.bcl
    newbcpl-driver dump-heap     foo.bcl
    newbcpl-driver run           foo.bcl
    newbcpl-driver gui           foo.bcl     (Windows only)
```

`run` is the only one that executes anything; the rest are read-only
introspection. When a program goes wrong, `dump-sema` is usually the
right next step: it lists every binding sema saw with its inferred
type, every function's inferred return type, every class's layout, and
every non-fatal warning.

---

## Appendix A — Reserved Words

The lexer keeps the following identifiers as keywords. They cannot be
used as variable names.

```
    LET   AND   BE   VALOF   RESULTIS
    MANIFEST   STATIC   GLOBAL   GLOBALS   VEC   TABLE   OF
    IF   UNLESS   TEST   THEN   ELSE   OR   DO
    WHILE   UNTIL   REPEAT   REPEATWHILE   REPEATUNTIL
    FOR   TO   BY
    SWITCHON   INTO   CASE   DEFAULT   ENDCASE
    GOTO   RETURN   FINISH   BREAK   LOOP
    TRUE   FALSE
    NOT   XOR
    BAND   BOR   BXOR   BNOT
    REM   EQV   NEQV
    GET
    FLET   FSTATIC   FVEC   FTABLE   FVALOF
    FUNCTION   ROUTINE
    CLASS   EXTENDS   DECL   NEW
    VIRTUAL   FINAL   MANAGED
    PUBLIC   PRIVATE   PROTECTED
    SELF   SUPER
    RETAIN   FREEVEC   FREELIST   USING
    FLOAT   TRUNC   FIX   FSQRT   ENTIER
    FOREACH   IN
    LIST   MANIFESTLIST
    HD   TL   REST
    LEN   TYPEOF   TYPE
    AS   POINTER
    DEFER   BRK
    PAIR   FPAIR   QUAD   FQUAD   OCT   FOCT
```

Keywords are upper-case by convention. Identifiers may be either case
but lower-case is the usual style.

---

## Appendix B — Grammar Sketch

This is the surface grammar in approximate BNF; consult the parser
(`src/newbcpl-parser/src/parser.rs`) for the authoritative version.

```
program        ::= decl*
decl           ::= let-decl | get | manifest | static | global | class

let-decl       ::= ("LET" | "FLET") binder
binder         ::= name "(" params? ")" ("=" expr | "BE" stmt)
                 | name ( "AS" type )? ("," name ("AS" type)?)* "=" expr ("," expr)*

class          ::= "CLASS" Name ("EXTENDS" Name)? "MANAGED"? "BE"? block-of-members
member         ::= visibility? ("VIRTUAL"|"FINAL")*
                   ( "DECL" name-list
                   | "LET" name-list
                   | "FLET" name ("=" expr)?
                   | "ROUTINE" name "(" params? ")" "BE" stmt
                   | "FUNCTION" name "(" params? ")" "=" expr
                   | "LET" name "(" params? ")" ("=" expr | "BE" stmt) )

stmt           ::= block
                 | "IF" expr "THEN"? stmt ("ELSE" stmt)?
                 | "UNLESS" expr "THEN"? stmt
                 | "TEST" expr "THEN"? stmt "ELSE" stmt
                 | "WHILE" expr "DO"? stmt
                 | "UNTIL" expr "DO"? stmt
                 | "FOR" name "=" expr "TO" expr ("BY" expr)? "DO"? stmt
                 | "FOREACH" (name ("," name)? | "(" name-list ")")
                       ("AS" type)? "IN" expr "DO"? stmt
                 | "SWITCHON" expr "INTO"? "$(" case* default? "$)"
                 | "RESULTIS" expr  | "RETURN"  | "FINISH"
                 | "BREAK"   | "LOOP"   | "ENDCASE"   | "BRK"
                 | "GOTO" name | name ":"
                 | "RETAIN" name ("=" expr)?
                 | "USING" name "=" expr ("DO"|"THEN")? stmt
                 | lvalue ("," lvalue)* ":=" expr ("," expr)*
                 | expr
                 | stmt "REPEAT"
                 | stmt "REPEATWHILE" expr
                 | stmt "REPEATUNTIL" expr

expr           ::= conditional
conditional    ::= binary ("->" expr "," expr)?
binary         ::= unary ( op binary )*
unary          ::= ("-"|"~"|"BNOT"|"NOT"|"!"|"@"|"%"|"HD"|"TL"|"REST"
                   |"LEN"|"FREEVEC"|"FREELIST") unary
                 | postfix
postfix        ::= atom ( "(" args? ")"
                        | "!" unary | "%" unary | ".%" unary
                        | "%%" "(" expr ("," expr)? ")"
                        | "." (name | "|" expr "|")
                        | "OF" name )*
atom           ::= name | number | string | char | "TRUE" | "FALSE" | "?"
                 | "(" expr ")"
                 | "VALOF" ("AS" type)? stmt
                 | ("VEC"|"FVEC") ( "[" args? "]" | "(" args? ")" | unary )
                 | ("PAIR"|"FPAIR"|"QUAD"|"FQUAD"|"OCT"|"FOCT"
                   |"TABLE"|"FTABLE"|"LIST"|"MANIFESTLIST") "(" args? ")"
                 | "NEW" Name ("(" args? ")")?
```

---

## Appendix C — A Slightly Longer Example

The following program counts word frequencies in a string, illustrating
classes, lists, and the loop forms working together.

```bcpl
CLASS Counter $(
    DECL word, count

    ROUTINE CREATE(w) BE $(
        word  := w
        count := 1
    $)

    FUNCTION getWord()  = word
    FUNCTION getCount() = count

    ROUTINE bump() BE count := count + 1
$)

LET find(c, w) = VALOF $(
    FOREACH e IN c DO
        IF strcmp(e.getWord(), w) = 0 THEN RESULTIS e
    RESULTIS ?                                 // null
$)

LET tally(words) = VALOF $(
    LET counters = LIST()
    FOREACH w IN words DO $(
        LET existing = find(counters, w)
        TEST existing = ?
            THEN APND_OBJECT(counters, NEW Counter(w))
            ELSE existing.bump()
    $)
    RESULTIS counters
$)

LET START() BE $(
    LET ws = LIST("ham", "eggs", "ham", "spam", "ham", "eggs")
    LET cs = tally(ws)
    FOREACH c IN cs DO
        WRITEF("%s: %d*N", c.getWord(), c.getCount())
$)
```

The output is

```
    ham: 3
    eggs: 2
    spam: 1
```

---

## Appendix D — Extended Multimedia Support

The runtime ships a slot-based audio surface backed by NewAudio: game
SFX presets, custom oscillator / noise / FM synthesis, baked-in
effect chains, and ABC-notation music. Synthesis and parsing work on
every target; live `waveOut` / `midiOut` playback is Windows-only,
and on other targets `play` calls degrade to silent no-ops while the
slot tables still respond consistently.

The user-facing names live in `modules-active/audio.bcl`. After the
loader's `<stem>_<routine>` mangling they appear as `audio_coin`,
`audio_play`, `audio_music_load`, … The wrappers forward to the raw
`Sound_*` / `Music_*` runtime builtins; either spelling works in
source.

### D.1 The Slot Model

A *slot* is a small integer the program picks for itself. Each
synthesis call (`audio_coin`, `audio_tone`, …) registers a buffer at
its slot; `audio_play(slot, volume, pan)` plays it. Rebinding a slot
frees the previous buffer first, so

```bcpl
audio_coin(1, 1.0, 0.4)         // slot 1 holds a coin SFX
audio_play(1, 1.0, 0.0)         // ...play it...
audio_zap(1, 880.0, 0.2)        // ...then replace it with a zap
```

leaks nothing across the rebinding. Music slots are independent of
sound slots; `audio_music_load(10, ...)` does not collide with
`audio_coin(10, ...)`.

Volumes are floats in `[0, 1]`; clipped at the boundaries. Pan is
`-1.0` (left) to `1.0` (right), `0.0` centred. Like the GUI surface,
audio arguments must be float literals — `1.0`, not `1` — because
the Win64 ABI routes them through XMM registers.

### D.2 SFX Presets

Each preset takes `(slot, p1, duration)` and renders into the slot.

```
    audio_beep(slot, frequency,   dur)     // pure tone
    audio_coin(slot, pitch,       dur)     // bright pickup chime
    audio_jump(slot, power,       dur)     // platformer hop
    audio_explode(slot, size,     dur)
    audio_big_explode(slot, size, dur)
    audio_small_explode(slot, intensity, dur)
    audio_distant_explode(slot, distance, dur)
    audio_metal_explode(slot, shrapnel,   dur)
    audio_zap(slot, frequency,    dur)     // laser zap
    audio_shoot(slot, power,      dur)
    audio_powerup(slot, intensity, dur)
    audio_hurt(slot, severity,    dur)
    audio_click(slot, sharpness,  dur)     // UI click
    audio_bang(slot, intensity,   dur)
    audio_blip(slot, pitch,       dur)
    audio_pickup(slot, brightness, dur)
```

Sweep helpers take an extra frequency argument:

```
    audio_sweep_up(slot,   start_hz, end_hz, dur)
    audio_sweep_down(slot, start_hz, end_hz, dur)
```

`audio_random_beep(slot, seed, dur)` is deterministic: the same seed
reproduces the same chime, useful as a per-object cue.

### D.3 Custom Synthesis

For programs that want finer control than the presets give:

```
    audio_tone(slot, freq, dur, waveform)
    audio_note(slot, midi, dur, waveform,
               attack, decay, sustain, release)
    audio_noise(slot, noise_type, dur)
    audio_fm(slot, carrier_hz, modulator_hz, mod_index, dur)
```

`waveform` is an integer code: `0` Sine, `1` Square, `2` Sawtooth,
`3` Triangle, `4` Noise, `5` Pulse. `noise_type` is `0` White, `1`
Pink, `2` Brown. `midi` is a standard MIDI pitch number (60 = middle
C). ADSR envelope values are seconds for attack / decay / release;
sustain is a `[0, 1]` level.

### D.4 Effect Chains

These render a tone and bake an effect into the buffer at
registration time. Live per-voice effects are a future addition.

```
    audio_reverb(slot, freq, dur, wf, room_size, damping, wet)
    audio_delay(slot,  freq, dur, wf, delay_time, feedback, mix)
    audio_distort(slot, freq, dur, wf, drive, tone_color, level)
    audio_filter_tone(slot, freq, dur, wf, filter_type, cutoff, resonance)
    audio_filter_note(slot, midi, dur, wf,
                      a, d, s, r,
                      filter_type, cutoff, resonance)
```

`filter_type` codes: `0` None, `1` LowPass, `2` HighPass, `3`
BandPass. `room_size`, `damping`, `wet`, `feedback`, `mix`, `drive`,
`tone_color`, `level`, `resonance` are all unit-interval floats;
`delay_time` is seconds; `cutoff` is in Hz.

### D.5 Playback and Control

```
    audio_play(slot, volume, pan)        -> 0 on success
    audio_stop_all()                     -> 0
    audio_free(slot)                     -> 0 or AUDIO_ERR_UNKNOWN_SLOT
    audio_free_all()                     -> 0
    audio_set_volume(level)              -> 0   (SFX bus, [0,1])
    audio_get_volume()                   -> f64
    audio_count()                        -> populated slot count
    audio_playing(slot)                  -> 1 if a voice is active
    audio_duration(slot)                 -> seconds, 0.0 if empty
```

`audio_play` returns `0` on success, `2` for an unknown slot, `3` if
no audio device is available. The slot bank is unaffected by
`stop_all` — replay with `audio_play` later.

### D.6 Music — ABC Notation

```
    audio_music_load(slot, abc_string)   -> 0 or AUDIO_ERR_PARSE
    audio_music_play(slot, volume)       -> 0 or unknown-slot / no-device
    audio_music_stop_all()
    audio_music_pause_all()
    audio_music_resume_all()
    audio_music_free(slot)
    audio_music_free_all()
    audio_music_set_volume(level)        -> 0   (music bus)
    audio_music_get_volume()             -> f64
    audio_music_count()                  -> populated tune count
    audio_music_state()                  -> 0 stopped, 1 playing, 2 paused
    audio_music_playing(slot)            -> 1 if the tune is active
    audio_music_tempo(slot)              -> BPM, 0.0 if unknown
```

ABC strings must fit on one source line (BCPL forbids newlines
inside string literals); use `*N` between header lines and the body.
The parser is forgiving — most ABC tunes from `abcnotation.com` load
without modification.

### D.7 A Sound Program

```bcpl
LET START() BE $(
    audio_set_volume(0.8)

    audio_coin(1, 1.0, 0.4)
    audio_play(1, 1.0, 0.0)
    SLEEP(1000)

    audio_music_load(10,
      "X:1*NT:Lead*NM:4/4*NL:1/8*NQ:1/4=160*NK:C*NC E G c | c G E C |")
    audio_music_play(10, 1.0)
    SLEEP(4000)
$)
```

`SLEEP(ms)` is a portable runtime builtin; the program waits out
each cue before returning. `examples/sound-test.bcl` is the same
shape, slightly fuller. The Rust-side surface lives in
`src/newbcpl-runtime/src/audio.rs` and tracks NewFB's
`newfb-runtime/src/audio.rs` byte-for-byte at the engine level.

---

## Appendix E — ASM Procedures

A procedure body that is `ASM { … }` instead of a BCPL expression
or statement contains raw Win64 Intel-syntax assembly. The driver
appends it to the LLVM module as a `module asm` blob, MCJIT
assembles and links it, and BCPL call sites bind to the resulting
symbol through an LLVM `declare` whose argument and return types
follow the declared parameter list.

There is no parameter-name substitution. The author writes the
Win64 ABI registers directly. The compiler's only job is to wire
the LLVM `declare` so the call sites pass arguments in the right
registers and read the return value from the right place.

### E.1 Syntax

```
    LET  name(p0, p1, …) [AS RetType]  =  ASM { body }   // function
    LET  name(p0, p1, …)               BE ASM { body }   // routine
```

Each parameter may carry an optional `AS Type` annotation
(`FLOAT`, `FQUAD`, `FOCT`). Anything else — or no annotation —
makes the parameter a plain Word (integer / pointer / packed
SIMD scalar). The trailing `AS RetType` annotation, allowed only
on the `=` (function) form, picks the return-value register;
omit it and the function returns a Word in `rax`.

The body is the raw source text between the braces, preserved
verbatim — whitespace and incidental punctuation pass through to
the assembler unchanged. `//` and `;` line comments inside the
body work the way GAS Intel syntax expects them to.

### E.2 The Win64 ABI in One Table

Parameter slots are numbered from zero. Each slot's annotation
picks the register file the value travels in:

```
    Slot | Word (int/ptr)            | Float (f64) / FQuad     | FOct
    -----+---------------------------+-------------------------+--------
    0    | rcx                       | xmm0                    | ymm0
    1    | rdx                       | xmm1                    | ymm1
    2    | r8                        | xmm2                    | ymm2
    3    | r9                        | xmm3                    | ymm3
    4+   | qword ptr [rsp+40+8N]     | xmmword ptr [rsp+40+8N] | ymmword ptr [rsp+40+8N]
```

Return values: Word goes in `rax`; Float in `xmm0`; FQuad
(`<4 x f32>`) in `xmm0`; FOct (`<8 x f32>`) in `ymm0`. Routines
(`BE ASM`) return nothing — the caller never inspects `rax`.

Caller-saved registers (`rax`, `rcx`, `rdx`, `r8`–`r11`,
`xmm0`–`xmm5`) are free to clobber. Callee-saved registers
(`rbx`, `rsi`, `rdi`, `r12`–`r15`, `xmm6`–`xmm15`) must be
preserved across the call — if the body touches any, push and
pop them yourself.

### E.3 Three Worked Examples

A two-word integer multiply that returns through `rax`:

```bcpl
LET fastmul(a, b) = ASM {
    mov rax, rcx
    imul rax, rdx
    ret
}
```

A no-return routine that takes one word and silently returns:

```bcpl
LET sink(x) BE ASM {
    ret
}
```

A floating-point add whose return value travels through `xmm0`:

```bcpl
LET fadd(a AS FLOAT, b AS FLOAT) AS FLOAT = ASM {
    addsd xmm0, xmm1
    ret
}
```

In every case the BCPL call site looks like a normal function or
routine call — `fastmul(6, 7)`, `sink(99)`, `fadd(1.5, 2.25)`.

### E.4 Five-Plus Parameters

The first four slots live in registers. Slot 4 lives at
`[rsp+40]`, slot 5 at `[rsp+48]`, and so on — the 32-byte
shadow-home space at `[rsp+0]`..`[rsp+24]` is reserved by Win64
for the callee's use, and arguments start above it. Sum of five
words:

```bcpl
LET sum5(a, b, c, d, e) = ASM {
    mov rax, rcx
    add rax, rdx
    add rax, r8
    add rax, r9
    add rax, qword ptr [rsp+40]
    ret
}
```

### E.5 Labels and Loops

Local labels survive the brace-balanced body scanner. Lines that
end in `:` stay at column 0 in the emitted asm; instructions are
indented. A naive counted-loop summing 1..n:

```bcpl
LET sumn(n) = ASM {
    xor rax, rax
.loop:
    add rax, rcx
    dec rcx
    jnz .loop
    ret
}
```

### E.6 What This Buys You

ASM procedures are an escape hatch — most BCPL programs never
need one. They are useful when:

- A SIMD kernel needs a specific instruction sequence LLVM does
  not pick (manual `vbroadcast` / `vfmadd231ps` chains, etc.).
- You want to read or write a control-flow register (`mxcsr`,
  `xcr0`) the BCPL surface has no spelling for.
- A profile pins a hot inner loop and the IR is leaving cycles
  on the table.

For everything else, write the BCPL — the JIT is good at the
common cases and the ASM bodies are opaque to sema, so a typo
in the body is reported by the assembler, not by the compiler.

The end-to-end pipeline is in `src/new-asm` (the shared register
type / module-asm builder used by every NewLang sibling), with
the BCPL wiring split across `src/newbcpl-parser/src/parser.rs`
(`scan_asm_body`), `src/newbcpl-ir/src/lower.rs`
(`lower_asm_proc` / `annotation_to_asm_type`), and
`src/newbcpl-llvm/src/emit.rs` (`declare_asm_proc`, pass 1b for
declares, pass 4 for bodies). The integration probes are
`tests/newbcpl-tests/tests/asm_probes.rs`; the example programs
are `examples/asm-smoke.bcl`, `examples/asm-routine.bcl`, and
`examples/asm-float.bcl`.

---

*Read this guide once for shape, then again with the compiler running.
NewBCPL is small enough to fit in one head, and that is the point.*
