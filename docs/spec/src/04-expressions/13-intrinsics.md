+++
title = "Intrinsic Expressions"
weight = 13
template = "spec/page.html"
+++

# Intrinsic Expressions

{{ rule(id="4.13:1", cat="normative") }}

An intrinsic expression is a [builtin](@/02-lexical-structure/05-builtins.md) that appears in expression position and produces a value.

{{ rule(id="4.13:2", cat="normative") }}

```ebnf
intrinsic = "@" IDENT "(" [ intrinsic_arg { "," intrinsic_arg } ] ")" ;
intrinsic_arg = expression | type ;
```

{{ rule(id="4.13:2a", cat="normative") }}

Intrinsics **MAY** accept expressions, types, or a combination of both as arguments, depending on the specific intrinsic.

{{ rule(id="4.13:3", cat="normative") }}

Each intrinsic has a fixed signature specifying the number and types of arguments it accepts.

{{ rule(id="4.13:4", cat="legality-rule") }}

It is a compile-time error to call an intrinsic with the wrong number of arguments.

{{ rule(id="4.13:5", cat="legality-rule") }}

It is a compile-time error to use an unknown intrinsic name.

## Quick Reference

{{ rule(id="4.13:5a", cat="informative") }}

This Quick Reference documents every source-spelled intrinsic name the compiler
recognizes. The tables below group names that may appear in any expression
position (expression intrinsics), only inside a `checked` block (unchecked
intrinsics, specified in §9.2), or only as an internal checked bridge. Rule
4.13:5b separately records frontend-reserved names and test infrastructure.
The abort intrinsics `@panic`, `@assert`, `@assert_eq`, and `@assert_ne` are
specified normatively in 4.13:5c–5g.
This inventory is kept in sync with the compiler's
source-intrinsic recognition paths: the RIR type-intrinsic forms, the
pre-interned names in `crates/rue-air/src/sema/known_symbols.rs`, and the
semantic dispatch on them. A name absent from those paths is rejected as an
unknown intrinsic (rule 4.13:5), so every intrinsic the compiler recognizes
**MUST** appear either in a table or in the reserved-name notes below.

Expression intrinsics (usable in any expression position):

| Intrinsic | Purpose | Arguments | Return Type |
|-----------|---------|-----------|-------------|
| `@dbg` | Print debug output | 1 expression (int, bool, or string) | `()` |
| `@panic` | Abort with a message (§4.13:5c) | 0–1 expressions (text message) | `!` |
| `@assert` | Abort unless a condition holds (§4.13:5d) | 1 expression (`bool`), 1 optional expression (text message) | `()` |
| `@assert_eq` | Abort unless two values are equal, reporting both (§4.13:5f) | 2 expressions (one type, comparable with `==`) | `()` |
| `@assert_ne` | Abort unless two values differ, reporting both (§4.13:5f) | 2 expressions (one type, comparable with `==`) | `()` |
| `@size_of` | Get type size in bytes | 1 type | `i32` |
| `@align_of` | Get type alignment in bytes | 1 type | `i32` |
| `@require_droppable` | Enforce the owning-container element-type gate | 1 type | `()` |
| `@require_trivially_droppable` | Enforce the by-copy container-read element-type gate | 1 type | `()` |
| `@int_max` | Largest value of an integer type (§4.13:126) | 1 type (integer) | that integer type |
| `@int_min` | Smallest value of an integer type (§4.13:126) | 1 type (integer) | that integer type |
| `@offset_of` | Get a struct field's byte offset | 1 type, 1 field name | `u64` |
| `@intCast` | Convert between integer types | 1 expression (integer) | inferred integer type |
| `@bitCast` | Reinterpret an integer's bits at the same width (§4.13:118) | 1 expression (integer) | inferred integer type of the same width |
| `@wrapping_add` | Wrapping (modular) addition (§4.13:97) | 2 expressions (same integer type) | that integer type |
| `@wrapping_sub` | Wrapping (modular) subtraction (§4.13:97) | 2 expressions (same integer type) | that integer type |
| `@wrapping_mul` | Wrapping (modular) multiplication (§4.13:97) | 2 expressions (same integer type) | that integer type |
| `@to_string` | Format an integer as its decimal `StrBuf` (requires a lexical `@import("std")` in the file; §3.7:22) | 1 expression (any integer) | `StrBuf` |
| `@drop` | Run a value's drop glue (skipping moved-out sub-places) and consume it (RUE-187) | 1 expression (any type) | `()` |
| `@read_line` | Read line from stdin | none | `Option(StrBuf)` |
| `@parse_i32` | Parse text to i32 | 1 expression (any text rung) | `Option(i32)` |
| `@parse_i64` | Parse text to i64 | 1 expression (any text rung) | `Option(i64)` |
| `@parse_u32` | Parse text to u32 | 1 expression (any text rung) | `Option(u32)` |
| `@parse_u64` | Parse text to u64 | 1 expression (any text rung) | `Option(u64)` |
| `@random_u32` | Generate random u32 | none | `u32` |
| `@random_u64` | Generate random u64 | none | `u64` |
| `@arg_count` | Number of command-line arguments (incl. `argv[0]`) | none | `u64` |
| `@arg_len` | Byte length of argument `i` (0 out of range) | 1 expression (`u64` index) | `u64` |
| `@env_count` | Number of environment entries | none | `u64` |
| `@env_len` | Byte length of environment entry `i` (0 out of range) | 1 expression (`u64` index) | `u64` |
| `@target_arch` | Get target architecture | none | `Arch` |
| `@target_os` | Get target OS | none | `Os` |
| `@target_data_model` | Get target C data model | none | `DataModel` |
| `@import` | Import module | 1 expression (string literal) | module type |

Unchecked intrinsics (only valid inside a `checked` block; see §9.2 for their
full semantics):

| Intrinsic | Purpose | Arguments | Return Type |
|-----------|---------|-----------|-------------|
| `@syscall` | Direct system call | 1–7 expressions (`u64`) | `i64` |
| `@raw` | `const` pointer to a place | 1 place expression | `ptr const T` |
| `@raw_mut` | `mut` pointer to a place | 1 place expression | `ptr mut T` |
| `@field_ptr` | `mut` pointer to a struct field place | 1 field-access expression | `ptr mut F` |
| `@ptr_read` | Read through a pointer | 1 expression (`ptr const T`/`ptr mut T`) | `T` |
| `@ptr_write` | Write through a pointer | 2 expressions (`ptr mut T`, `T`) | `()` |
| `@ptr_read_unaligned` | Read through a possibly unaligned pointer (§9.2) | 1 expression (`ptr const T`/`ptr mut T`) | `T` |
| `@ptr_write_unaligned` | Write through a possibly unaligned pointer (§9.2) | 2 expressions (`ptr mut T`, `T`) | `()` |
| `@ptr_offset` | Pointer arithmetic | 2 expressions (`ptr T`, integer) | `ptr T` |
| `@ptr_to_int` | Pointer to integer | 1 expression (pointer) | `u64` |
| `@int_to_ptr` | Integer to pointer | 1 expression (`u64`) | inferred `ptr mut T` |
| `@alloc` | Allocate physical bytes with alignment (§9.2) | 2 expressions (`u64` size, `u64` align) | `ptr mut u8` |
| `@alloc_zeroed` | Allocate zero-filled physical bytes (§9.2) | 2 expressions (`u64` size, `u64` align) | `ptr mut u8` |
| `@free` | Free an allocated block (§9.2) | 3 expressions (`ptr mut u8`, `u64` size, `u64` align) | `()` |
| `@realloc` | Resize an allocated block, possibly moving it (§9.2) | 4 expressions (`ptr mut u8`, `u64` old size, `u64` align, `u64` new size) | `ptr mut u8` |
| `@resize` | Resize an allocated block in place only (§9.2) | 4 expressions (`ptr mut u8`, `u64` old size, `u64` align, `u64` new size) | `bool` |
| `@byte_copy` | Copy `size` non-overlapping bytes | 3 expressions (`ptr mut u8`, `ptr const u8`/`ptr mut u8`, `u64`) | `()` |
| `@byte_move` | Copy `size` possibly overlapping bytes (§9.2) | 3 expressions (`ptr mut u8`, `ptr const u8`/`ptr mut u8`, `u64`) | `()` |
| `@byte_set` | Fill `size` bytes with a byte | 3 expressions (`ptr mut u8`, `u8`, `u64`) | `()` |
| `@arg_ptr` | Pointer to argument `i`'s bytes (null out of range) | 1 expression (`u64` index) | `ptr mut u8` |
| `@env_ptr` | Pointer to environment entry `i`'s bytes (null out of range) | 1 expression (`u64` index) | `ptr mut u8` |

Internal checked bridge (recognized only as the direct expression of the single
trailing `yield checked { ... }` in a trusted standard-library accessor body;
see rule [6.6:7](@/06-items/06-borrow-accessors.md)):

| Intrinsic | Purpose | Arguments | Result |
|-----------|---------|-----------|--------|
| `@place` | Internal pointer-to-place accessor bridge; not user-invocable | 1 `@ptr_offset(<self field chain>, ...)` expression | operand pointer; the enclosing `yield` treats its pointee as the accessor place |

{{ rule(id="4.13:5b", cat="informative") }}

The compiler frontend reserves the names `@cast` and `@test_preview_gate`.
`@cast` is not a valid conversion intrinsic; its rejection is specified in
4.13:5e. The `@test_preview_gate()` intrinsic is a zero-argument no-op used only
to test the preview-feature gating machinery (`--preview test_infra`); it is
test infrastructure, not a language feature.

{{ rule(id="4.13:5c", cat="dynamic-semantics") }}

`@panic(msg?: text)` has type `!`, writes `panic: <msg>` (or `panic` without a
message) to standard error, and terminates with status 101 without returning.
It participates in never coercion (3.4:2), so it may appear wherever a value
of any type is expected.

{{ rule(id="4.13:5d", cat="dynamic-semantics") }}

`@assert(cond: bool, msg?: text)` requires a boolean condition and has type
`()`. A false condition terminates exactly as `@panic` (using `panic: <msg>`
when a message is supplied, or `assertion failed` otherwise), with status 101;
a true condition has no effect.

{{ rule(id="4.13:5e", cat="legality-rule") }}

`@cast` is reserved but not a valid conversion intrinsic. A call to it **MUST**
be rejected at compile time with a diagnostic directing the programmer to
`@intCast`.

{{ rule(id="4.13:5f", cat="dynamic-semantics") }}

`@assert_eq(left, right)` and `@assert_ne(left, right)` each take exactly two
expressions and have type `()`. Both operands **MUST** have the same type, and
that type **MUST** be one `==` accepts (4.3:3); it is a compile-time error
otherwise. Each operand is evaluated exactly once, left before right, and read
without being consumed — the same borrowing `==` performs (4.3:3f). The
assertion holds when `left == right` does, for `@assert_eq`, and when
`left != right` does, for `@assert_ne`. When it holds the intrinsic has no
effect. When it does not, the program writes
`panic: assertion failed: left == right` (for `@assert_eq`) or
`panic: assertion failed: left != right` (for `@assert_ne`) to standard error
and terminates with status 101, exactly as `@assert` does.

{{ rule(id="4.13:5g", cat="informative") }}

Inside a test image the same failure is additionally reported as structured
data: a record naming the failing kind, the intrinsic's source location, and
both operands rendered as `expected` and `actual`. Ordinary builds are
unaffected — the report goes to a descriptor a test runner supplies, and its
absence is not an error. See
[docs/process/test-events.md](https://github.com/rue-language/rue/blob/trunk/docs/process/test-events.md)
for that surface, which is a tooling contract rather than a language rule.

{{ rule(id="4.13:5h") }}

```rue
fn main() -> i32 {
    @assert_eq(1 + 1, 2);
    @assert_ne(1, 2);
    42
}
```

## `@dbg`

{{ rule(id="4.13:6", cat="normative") }}

The `@dbg` intrinsic prints a value to standard output for debugging purposes.
It *borrows* its argument: the value is read but not consumed, so a non-`Copy`
argument (such as a `StrBuf`) remains valid after the `@dbg` call and is dropped
by its owner at the end of the enclosing scope, exactly as if the `@dbg` call had
not occurred.

{{ rule(id="4.13:7", cat="normative") }}

`@dbg` accepts exactly one argument of integer, boolean, or string type.

{{ rule(id="4.13:8", cat="normative") }}

`@dbg` prints the value followed by a newline character.

{{ rule(id="4.13:8a", cat="normative") }}

The textual form `@dbg` prints for its argument is determined by the argument's
type: an integer is printed in base 10, with a leading `-` when a signed integer
is negative and no sign otherwise; a boolean is printed as `true` or `false`; a
`StrBuf` is printed as its exact bytes, byte-for-byte, with no quoting or
escaping (mirroring `print`, 3.7). The newline of 4.13:8 follows this text.

{{ rule(id="4.13:9", cat="normative") }}

The return type of `@dbg` is `()`.

{{ rule(id="4.13:10") }}

```rue
fn main() -> i32 {
    @dbg(42);           // prints: 42
    @dbg(-17);          // prints: -17
    @dbg(true);         // prints: true
    @dbg(false);        // prints: false
    @dbg(10 + 5);       // prints: 15
    @dbg("hello");      // prints: hello
    0
}
```

{{ rule(id="4.13:11") }}

`@dbg` is useful for inspecting values during development:

```rue
fn factorial(n: i32) -> i32 {
    @dbg(n);  // trace each call
    if n <= 1 {
        1
    } else {
        n * factorial(n - 1)
    }
}

fn main() -> i32 {
    factorial(5)
}
```

## `@size_of`

{{ rule(id="4.13:12", cat="normative") }}

The `@size_of` intrinsic returns the size of a type in bytes.

{{ rule(id="4.13:13", cat="normative") }}

`@size_of` accepts exactly one argument, which **MUST** be a type.

{{ rule(id="4.13:14", cat="normative") }}

The return type of `@size_of` is `i32`.

{{ rule(id="4.13:15", cat="normative") }}

The value returned by `@size_of` is determined at compile time.

{{ rule(id="4.13:16") }}

```rue
fn main() -> i32 {
    @size_of(i32)     // 4 (i32's natural byte width, observed under the compact layout)
}
```

{{ rule(id="4.13:17") }}

```rue
struct Point { x: i32, y: i32 }

fn main() -> i32 {
    // 8 under the compact layout (ADR-0052): two four-byte i32 fields, no padding.
    @size_of(Point)   // 8
}
```

## `@align_of`

{{ rule(id="4.13:18", cat="normative") }}

The `@align_of` intrinsic returns the alignment of a type in bytes.

{{ rule(id="4.13:19", cat="normative") }}

`@align_of` accepts exactly one argument, which **MUST** be a type.

{{ rule(id="4.13:20", cat="normative") }}

The return type of `@align_of` is `i32`.

{{ rule(id="4.13:21", cat="normative") }}

The value returned by `@align_of` is determined at compile time.

{{ rule(id="4.13:22", cat="informative") }}

`@align_of(T)` reports the alignment the implementation has chosen for `T`
under the layout in effect for the compilation (1.3:6, 3.6:12); it *observes*
that choice and does not guarantee a particular value. Under the compact layout
(ADR-0052) each scalar has its natural alignment — `@align_of(i32)` observes
`4`, `@align_of(bool)` observes `1`, `@align_of(i64)` observes `8` — and a
struct's alignment is that of its most-aligned field, so a portable program must
not assume a particular value such as `8`. Whichever layout is in effect, the
size of a type is always a multiple of its alignment (3.6:8).

{{ rule(id="4.13:23") }}

```rue
fn main() -> i32 {
    @align_of(i32)    // 4 (i32's natural alignment, observed under the compact layout)
}
```

## `@int_max` and `@int_min`

{{ rule(id="4.13:126", cat="normative") }}

The `@int_max` intrinsic returns the largest value representable in an integer
type, and the `@int_min` intrinsic returns the smallest. The bounds follow the
two's-complement ranges of §3.1: for an unsigned type of width *w* they are
`2^w - 1` and `0`; for a signed type they are `2^(w-1) - 1` and `-2^(w-1)`.

{{ rule(id="4.13:127", cat="normative") }}

`@int_max` and `@int_min` each accept exactly one argument, which **MUST** be
an integer type (§3.1).

{{ rule(id="4.13:128", cat="normative") }}

The result type of `@int_max(T)` and `@int_min(T)` is `T` itself. This is the
only result type that can represent every bound exactly: `@int_max(u64)`
exceeds every signed type, and `@int_min(i64)` is below every unsigned type.

{{ rule(id="4.13:129", cat="legality-rule") }}

It is a compile-time error (`E0702`) to apply `@int_max` or `@int_min` to a
type argument that is not an integer type.

{{ rule(id="4.13:130", cat="normative") }}

The value of `@int_max(T)` and `@int_min(T)` is determined at compile time,
and — unlike `@size_of` and `@align_of` — the integer-bounds intrinsics are
**comptime-evaluable** (4.14:29): the bounds depend only on the identity of
`T`, never on layout, so they may appear in `const` initializers and
`comptime` argument positions.

{{ rule(id="4.13:131") }}

```rue
fn main() -> i32 {
    if @int_max(u8) == 255 && @int_min(i8) == -128 {
        0
    } else {
        1
    }
}
```

{{ rule(id="4.13:132", cat="informative") }}

Because the result is typed at the queried type, the intrinsics compose with
generic `comptime T` code: overflow predicates such as
`a > @int_max(T) - b` need no per-type constants, which is the motivating use
(generic checked/saturating arithmetic, RUE-694).

## `@offset_of`

{{ rule(id="4.13:91", cat="normative") }}

The `@offset_of` intrinsic returns the byte offset of a field within a struct
type, mirroring Rust's `core::mem.offset_of!`.

{{ rule(id="4.13:92", cat="normative") }}

`@offset_of` accepts exactly two arguments: the first **MUST** be a struct type,
and the second **MUST** be the name of one of that struct's fields.

{{ rule(id="4.13:93", cat="normative") }}

The return type of `@offset_of` is `u64`.

{{ rule(id="4.13:94", cat="normative") }}

The value returned by `@offset_of` is the offset the compiler assigns to the
field under the layout it chooses for the struct, determined at compile time.
Because the value comes from the compiler's own layout rather than a
hand-computed constant, `@offset_of` remains correct even if the struct layout
is implementation-defined. Under the compact layout each field is placed at the
lowest offset satisfying its alignment after the preceding fields, so an offset
accounts for both the preceding fields' sizes and any alignment padding (§3.6).

{{ rule(id="4.13:95", cat="legality-rule") }}

It is a compile-time error to apply `@offset_of` to a non-struct type, or to
name a field that the struct does not declare.

{{ rule(id="4.13:96") }}

```rue
struct Mixed { a: i32, b: i64, c: bool }

fn main() -> i32 {
    let off_a: u64 = @offset_of(Mixed, a);   // 0
    let off_b: u64 = @offset_of(Mixed, b);   // 8  (i64 field at its eight-byte alignment)
    let off_c: u64 = @offset_of(Mixed, c);   // 16 (bool after the i64)
    let sum: u64 = off_a + off_b + off_c;    // 24
    @intCast(sum)
}
```

## `@intCast`

{{ rule(id="4.13:24", cat="normative") }}

The `@intCast` intrinsic converts an integer value from one integer type to another.

{{ rule(id="4.13:25", cat="normative") }}

`@intCast` accepts exactly one argument, which **MUST** be an integer type (any of `i8`, `i16`, `i32`, `i64`, `u8`, `u16`, `u32`, `u64`).

{{ rule(id="4.13:26", cat="normative") }}

The target type of the conversion is inferred from the context where `@intCast` is used.

{{ rule(id="4.13:27", cat="legality-rule") }}

It is a compile-time error if the target type cannot be inferred or is not an integer type.

{{ rule(id="4.13:28", cat="dynamic-semantics") }}

If the source value cannot be exactly represented in the target type, a runtime panic occurs.

{{ rule(id="4.13:29") }}

```rue
fn main() -> i32 {
    let x: i32 = 100;
    let y: u8 = @intCast(x);  // OK: 100 fits in u8
    @intCast(y)               // Convert back to i32
}
```

{{ rule(id="4.13:30") }}

```rue
fn takes_u8(x: u8) -> u8 { x }

fn main() -> i32 {
    let x: i32 = 50;
    takes_u8(@intCast(x));    // Target type inferred from parameter
    0
}
```

{{ rule(id="4.13:31") }}

```rue
// This panics at runtime: 256 doesn't fit in u8
fn main() -> i32 {
    let x: i32 = 256;
    let y: u8 = @intCast(x);  // panic: integer cast overflow
    0
}
```

{{ rule(id="4.13:32") }}

```rue
// This panics at runtime: negative values don't fit in unsigned types
fn main() -> i32 {
    let x: i32 = -1;
    let y: u32 = @intCast(x); // panic: integer cast overflow
    0
}
```

## `@bitCast`

{{ rule(id="4.13:118", cat="normative") }}

The `@bitCast` intrinsic reinterprets an integer value's bits at another
integer type of the same width. It is the bit-preserving counterpart to
`@intCast`, which is value-preserving: `@intCast` keeps the number and rejects
the representations that do not fit, while `@bitCast` keeps the representation
and lets the number change.

{{ rule(id="4.13:119", cat="normative") }}

`@bitCast` accepts exactly one argument, which **MUST** be of an integer type
(any of `i8`, `i16`, `i32`, `i64`, `u8`, `u16`, `u32`, `u64`). The target type
is inferred from the context where `@bitCast` is used, exactly as `@intCast`'s
target type is (4.13:26).

{{ rule(id="4.13:120", cat="legality-rule") }}

It is a compile-time error if the target type cannot be inferred, is not an
integer type, or is an integer type whose width differs from the argument's
width (E0950). A reinterpretation neither invents nor discards bits, so it is
defined only between the same-width pairs — `i8`/`u8`, `i16`/`u16`,
`i32`/`u32`, and `i64`/`u64` — in either direction, and between an integer type
and itself. Converting between widths is `@intCast`'s job.

{{ rule(id="4.13:121", cat="dynamic-semantics") }}

For a source type of width `N` bits, the value of `@bitCast(x)` is the value
whose `N`-bit two's-complement representation is identical to that of `x`.
Equivalently, for a target type `T`: when the shared width is `N` and the
argument's value is `x`, the result is `x` if `x` is in range for `T`, and
otherwise `x - 2^N` when `T` is signed and `x + 2^N` when `T` is unsigned.

{{ rule(id="4.13:122", cat="dynamic-semantics") }}

`@bitCast` never traps. Every value of the source type has a representation at
the target type — the same representation — so no operand is rejected, in
contrast to the overflow trap of 4.13:28. In particular `@bitCast` is the
operation that moves a `u64` whose top bit is set into an `i64`, which
`@intCast` traps on.

{{ rule(id="4.13:123", cat="normative") }}

`@bitCast` is an involution on each same-width pair: for every value `x` of an
integer type `S` and same-width type `T`, reinterpreting `x` at `T` and then
reinterpreting that result back at `S` yields `x`.

{{ rule(id="4.13:124", cat="normative") }}

In a compile-time context, `@bitCast` is evaluated exactly as `@intCast` is: it
introduces no new constant-evaluation rule, so an invocation that `@intCast`
would not fold into a constant is likewise not a constant expression.

{{ rule(id="4.13:125") }}

```rue
fn main() -> i32 {
    // The u64 values above i64::MAX have no @intCast into i64 at all;
    // @bitCast moves them across, and back, unchanged.
    let big: u64 = 9223372036854775808;   // 1 << 63
    let signed: i64 = @bitCast(big);
    @dbg(signed);                          // -9223372036854775808
    let back: u64 = @bitCast(signed);
    @dbg(back);                            // 9223372036854775808

    // Narrower pairs reinterpret the same way.
    let byte: u8 = 255;
    let sbyte: i8 = @bitCast(byte);
    @dbg(sbyte);                           // -1
    0
}
```

## `@read_line`

{{ rule(id="4.13:33", cat="normative") }}

The `@read_line` intrinsic reads a line of text from standard input.

{{ rule(id="4.13:34", cat="normative") }}

`@read_line` accepts no arguments.

{{ rule(id="4.13:35", cat="normative") }}

The return type of `@read_line` is the trusted standard `Option` specialized at `StrBuf` — the producer-nominal enum declared by `std.option.Option` (`std/option.rue`, ADR-0038), instantiated as `Option(StrBuf)`. The intrinsic yields this exact standard specialization in every context: bare as the operand of the `?` operator (§4.15), as the initializer of an annotated `let`, as the scrutinee of a `match`, or as a freestanding expression. Surrounding context never *selects* which nominal the intrinsic produces; when an annotation or `match` is present it only *checks* that the intrinsic's standard `Option(StrBuf)` matches, and any other type — including a user-defined enum that repeats the `Some`/`None` shape under a different producer — is an ordinary type error (E0702). Because the standard `Option` is a toolchain guarantee, `@read_line` has this type even in a program that does not import `std` lexically; the compiler roots the trusted-module demand itself, and an absent standard library is a toolchain-integrity error rather than a language state.

{{ rule(id="4.13:36", cat="dynamic-semantics") }}

`@read_line` reads bytes from standard input until a newline character (`\n`) is encountered or end-of-file is reached.

{{ rule(id="4.13:37", cat="dynamic-semantics") }}

On a successful read the result is `Some(line)`, where the `line` `StrBuf` does **not** include the trailing newline character.

{{ rule(id="4.13:38", cat="dynamic-semantics") }}

If end-of-file is reached with some data read, the partial line is returned as `Some(line)`.

{{ rule(id="4.13:39", cat="dynamic-semantics") }}

If end-of-file is reached with no data read, the result is `None` (this is not an error; a read-until-end-of-input loop terminates by observing `None`).

{{ rule(id="4.13:40", cat="informative") }}

If a read error occurs, a runtime panic occurs with the message "input error".
If allocation or capacity growth fails while constructing the returned
`StrBuf`, the allocation-failure rules of §8.6 apply. (These behaviors are
documented but not tested here, as the failures cannot be reliably simulated
through portable source-level input.)

{{ rule(id="4.13:41") }}

```rue
const std = @import("std");
const Opt = std.option.Option(std.strbuf.StrBuf);

fn main() -> i32 {
    @dbg("What is your name?");
    match @read_line() {
        Opt.Some(name) => @dbg(name),
        Opt.None => @dbg("(no input)"),
    }
    0
}
```

{{ rule(id="4.13:42") }}

Reading every line until end-of-input:

```rue
const std = @import("std");
const Opt = std.option.Option(std.strbuf.StrBuf);

fn main() -> i32 {
    loop {
        let line: Opt = @read_line();
        match line {
            Opt.None => break,
            Opt.Some(text) => @dbg(text),
        }
    }
    0
}
```

## Integer Parsing Intrinsics

{{ rule(id="4.13:43", cat="normative") }}

The integer parsing intrinsics convert a string to an integer value.

{{ rule(id="4.13:44", cat="normative") }}

Each parsing intrinsic returns the trusted standard `Option` specialized at its target integer type `T` — the producer-nominal enum declared by `std.option.Option` (`std/option.rue`, ADR-0038):
- `@parse_i32` returns `Option(i32)`
- `@parse_i64` returns `Option(i64)`
- `@parse_u32` returns `Option(u32)`
- `@parse_u64` returns `Option(u64)`

The intrinsic yields this exact standard specialization in every context — bare as the operand of the `?` operator (§4.15), as the initializer of an annotated `let`, as the scrutinee of a `match`, or as a freestanding expression. Surrounding context never *selects* the nominal; when present it only *checks* that the intrinsic's standard `Option(T)` matches, and any other type — including a user-defined `Some`/`None` lookalike under a different producer — is an ordinary type error (E0702). As with `@read_line` (rule 4.13:35), the compiler roots the trusted standard `Option` itself, so a parsing intrinsic has this type even without a lexical `std` import.

{{ rule(id="4.13:45", cat="normative") }}

Each parsing intrinsic accepts exactly one argument, which **MUST** be one of
the text types `str`, `Str(N)`, or `StrBuf`.

{{ rule(id="4.13:46", cat="normative") }}

The string argument is borrowed, not consumed. The original string remains valid after parsing.

{{ rule(id="4.13:47", cat="normative") }}

A successful parse yields `Some(n)`. The parsed string is parsed successfully when it matches the following grammar:

```ebnf
integer_string = [ "-" ] digit { digit } ;
digit = "0" | "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" ;
```

{{ rule(id="4.13:48", cat="legality-rule") }}

Leading minus signs are only allowed for signed types (`@parse_i32`, `@parse_i64`); a negative value for an unsigned type is a parse failure (yields `None`).

{{ rule(id="4.13:49", cat="dynamic-semantics") }}

The result is `None` (a recoverable parse failure, not a panic) if:
- The string is empty
- The string contains non-digit characters (other than an optional leading minus)
- The value overflows the target type
- A negative value is parsed for an unsigned type

{{ rule(id="4.13:50") }}

```rue
const std = @import("std");

fn main() -> i32 {
    let Opt = std.option.Option(i32);
    match @parse_i32("42") {
        Opt.Some(n) => n,   // returns 42
        Opt.None => 0,
    }
}
```

{{ rule(id="4.13:51") }}

```rue
const std = @import("std");

fn main() -> i32 {
    let Opt = std.option.Option(i32);
    match @parse_i32("-17") {
        Opt.Some(n) => n,   // returns -17
        Opt.None => 0,
    }
}
```

{{ rule(id="4.13:52") }}

```rue
const std = @import("std");

fn main() -> i32 {
    let Opt = std.option.Option(i32);
    let s = "42";
    // StrBuf is borrowed, not consumed
    let parsed: Opt = @parse_i32(s);
    @dbg(s);  // s is still valid
    match parsed {
        Opt.Some(n) => n,
        Opt.None => 0,
    }
}
```

{{ rule(id="4.13:53") }}

```rue
// An invalid character is a recoverable failure: `None`, not a panic.
const std = @import("std");

fn main() -> i32 {
    let Opt = std.option.Option(i32);
    match @parse_i32("12abc") {
        Opt.Some(n) => n,
        Opt.None => -1,   // taken: "12abc" is not an integer
    }
}
```

{{ rule(id="4.13:54") }}

```rue
// A negative value for an unsigned type is a recoverable failure: `None`.
const std = @import("std");

fn main() -> i32 {
    let Opt = std.option.Option(u32);
    match @parse_u32("-17") {
        Opt.Some(n) => @intCast(n),
        Opt.None => 0,   // taken: "-17" is negative
    }
}
```

## `@random_u32`

{{ rule(id="4.13:55", cat="normative") }}

The `@random_u32` intrinsic generates a random unsigned 32-bit integer.

{{ rule(id="4.13:56", cat="normative") }}

`@random_u32` accepts no arguments.

{{ rule(id="4.13:57", cat="normative") }}

The return type of `@random_u32` is `u32`.

{{ rule(id="4.13:58", cat="dynamic-semantics") }}

Each call to `@random_u32` returns a non-deterministic value using a platform-provided cryptographically-secure entropy source.

{{ rule(id="4.13:59", cat="dynamic-semantics") }}

If the platform entropy source is unavailable or fails, a runtime panic occurs.

{{ rule(id="4.13:60") }}

```rue
fn main() -> i32 {
    let secret: u32 = (@random_u32() % 100) + 1;  // Random number 1-100
    @dbg(secret);
    0
}
```

{{ rule(id="4.13:61") }}

Using `@random_u32` in a guessing game:

```rue
const std = @import("std");
const StrBuf = std.strbuf.StrBuf;

fn main() -> i32 {
    let OptStr = std.option.Option(StrBuf);
    let OptU32 = std.option.Option(u32);
    let secret: u32 = (@random_u32() % 100) + 1;  // 1-100
    @dbg("Guess the number between 1 and 100!");

    let mut guesses = 0;
    loop {
        let input: OptStr = @read_line();
        match input {
            OptStr.None => break,          // end of input
            OptStr.Some(text) => {
                match @parse_u32(text) {
                    OptU32.Some(guess) => {
                        guesses = guesses + 1;
                        if guess < secret {
                            @dbg("Too low!");
                        } else if guess > secret {
                            @dbg("Too high!");
                        } else {
                            @dbg("You got it!");
                            break;
                        }
                    },
                    OptU32.None => @dbg("not a number, try again"),
                }
            },
        }
    }

    @intCast(guesses)
}
```

## `@random_u64`

{{ rule(id="4.13:62", cat="normative") }}

The `@random_u64` intrinsic behaves identically to `@random_u32` but returns a random unsigned 64-bit integer.

{{ rule(id="4.13:63", cat="normative") }}

`@random_u64` accepts no arguments.

{{ rule(id="4.13:64", cat="normative") }}

The return type of `@random_u64` is `u64`.

{{ rule(id="4.13:65") }}

```rue
fn main() -> i32 {
    let large_random = @random_u64();
    @dbg(large_random);
    0
}
```

## `@arg_count`, `@arg_len`, `@arg_ptr`

{{ rule(id="4.13:103", cat="normative") }}

The `@arg_count`, `@arg_len`, and `@arg_ptr` intrinsics expose the command-line
arguments the platform loader supplied to the process at entry. They are the
low-level surface on which `std.env` builds its owned-`StrBuf` accessors; the
argument vector is a fixed process input, so a program observes the same
arguments for the whole of its execution.

{{ rule(id="4.13:104", cat="normative") }}

`@arg_count` accepts no arguments and returns a `u64`: the number of
command-line arguments, including `argv[0]` (the program invocation path). A
process is always launched with at least one argument, so the result is at
least `1`.

{{ rule(id="4.13:105", cat="normative") }}

`@arg_len` accepts one `u64` index and returns a `u64`: the length in bytes of
argument `i`, not counting any terminator. `@arg_ptr` accepts one `u64` index
and returns `ptr mut u8`, a pointer to the first of that argument's bytes.
Because `@arg_ptr` yields a raw pointer, it may appear only inside a `checked`
block (§9.2), exactly like `@alloc`; `@arg_count` and `@arg_len` impose no such
requirement.

{{ rule(id="4.13:106", cat="dynamic-semantics") }}

When the index passed to `@arg_len` or `@arg_ptr` is greater than or equal to
`@arg_count()`, `@arg_len` returns `0` and `@arg_ptr` returns a null pointer.
For an in-range index, the `@arg_ptr` pointer addresses exactly `@arg_len(i)`
readable bytes. Argument bytes are not interpreted as UTF-8 (ADR-0035).

{{ rule(id="4.13:107") }}

```rue
fn main() -> i32 {
    // A program invoked with no extra arguments still sees argv[0].
    @dbg(@arg_count() >= 1);            // true
    0
}
```

## `@env_count`, `@env_len`, `@env_ptr`

{{ rule(id="4.13:108", cat="normative") }}

The `@env_count`, `@env_len`, and `@env_ptr` intrinsics expose the process
environment the platform loader supplied at entry, as a sequence of
`KEY=VALUE` byte strings. They mirror the `@arg_*` intrinsics and back
`std.env`'s environment lookups.

{{ rule(id="4.13:109", cat="normative") }}

`@env_count` accepts no arguments and returns a `u64`: the number of
environment entries. `@env_len` accepts one `u64` index and returns a `u64`:
the length in bytes of environment entry `i`. `@env_ptr` accepts one `u64`
index and returns `ptr mut u8`, a pointer to the first of that entry's bytes;
like `@arg_ptr`, it may appear only inside a `checked` block (§9.2), while
`@env_count` and `@env_len` may appear in any expression position.

{{ rule(id="4.13:110", cat="dynamic-semantics") }}

When the index passed to `@env_len` or `@env_ptr` is greater than or equal to
`@env_count()`, `@env_len` returns `0` and `@env_ptr` returns a null pointer.
For an in-range index, the `@env_ptr` pointer addresses exactly `@env_len(i)`
readable bytes, which encode one `KEY=VALUE` pair. Environment bytes are not
interpreted as UTF-8 (ADR-0035).

{{ rule(id="4.13:111") }}

```rue
fn main() -> i32 {
    // The environment is captured once; entries can be scanned by index.
    let mut i: u64 = 0;
    while i < @env_count() {
        i = i + 1;
    }
    0
}
```

## `@target_arch`

{{ rule(id="4.13:66", cat="normative") }}

The `@target_arch` intrinsic returns the target architecture as an `Arch` enum value.

{{ rule(id="4.13:67", cat="normative") }}

`@target_arch` accepts no arguments.

{{ rule(id="4.13:68", cat="normative") }}

The return type of `@target_arch` is `Arch`.

{{ rule(id="4.13:69", cat="normative") }}

The `Arch` enum is a built-in enum with the following variants:
- `Arch.X86_64` - x86-64 architecture
- `Arch.Aarch64` - ARM64/AArch64 architecture

{{ rule(id="4.13:70", cat="normative") }}

The value returned by `@target_arch` is determined at compile time based on the compilation target.

{{ rule(id="4.13:71") }}

```rue
fn main() -> i32 {
    match @target_arch() {
        Arch.X86_64 => 1,
        Arch.Aarch64 => 2,
    }
}
```

## `@target_os`

{{ rule(id="4.13:72", cat="normative") }}

The `@target_os` intrinsic returns the target operating system as an `Os` enum value.

{{ rule(id="4.13:73", cat="normative") }}

`@target_os` accepts no arguments.

{{ rule(id="4.13:74", cat="normative") }}

The return type of `@target_os` is `Os`.

{{ rule(id="4.13:75", cat="normative") }}

The `Os` enum is a built-in enum with the following variants:
- `Os.Linux` - Linux operating system
- `Os.Macos` - macOS operating system

{{ rule(id="4.13:76", cat="normative") }}

The value returned by `@target_os` is determined at compile time based on the compilation target.

{{ rule(id="4.13:77") }}

```rue
fn main() -> i32 {
    match @target_os() {
        Os.Linux => 1,
        Os.Macos => 2,
    }
}
```

{{ rule(id="4.13:78") }}

Combining `@target_arch` and `@target_os` for platform-specific code:

```rue
fn main() -> i32 {
    match @target_arch() {
        Arch.X86_64 => {
            match @target_os() {
                Os.Linux => 99,
                Os.Macos => 88,
            }
        },
        Arch.Aarch64 => {
            match @target_os() {
                Os.Linux => 77,
                Os.Macos => 66,
            }
        },
    }
}
```

## `@target_data_model`

{{ rule(id="4.13:112", cat="normative") }}

The `@target_data_model` intrinsic returns the compilation target's C data
model — the width convention the target's C ABI assigns to `int`, `long`, and
pointers, which selects the widths of the `std.c` transparent scalar aliases
(ADR-0064 Amendment 1) — as a `DataModel` enum value.

{{ rule(id="4.13:113", cat="normative") }}

`@target_data_model` accepts no arguments.

{{ rule(id="4.13:114", cat="normative") }}

The return type of `@target_data_model` is `DataModel`.

{{ rule(id="4.13:115", cat="normative") }}

The `DataModel` enum is a built-in enum with the following variants:
- `DataModel.Ilp32` - `int`, `long`, and pointers are 32-bit
- `DataModel.Llp64` - `long long` and pointers are 64-bit; `long` remains 32-bit
- `DataModel.Lp64` - `long` and pointers are 64-bit

{{ rule(id="4.13:116", cat="normative") }}

The value returned by `@target_data_model` is determined at compile time based
on the compilation target. Every target the reference compiler currently
supports (the x86-64 and AArch64 Linux/macOS targets reachable through
`@target_arch`/`@target_os`) is `Lp64`.

{{ rule(id="4.13:117") }}

```rue
fn main() -> i32 {
    match @target_data_model() {
        DataModel.Lp64 => 1,
        DataModel.Llp64 => 2,
        DataModel.Ilp32 => 3,
    }
}
```

## `@import`

{{ rule(id="4.13:79", cat="normative") }}

The `@import` intrinsic imports a module from another source file.

{{ rule(id="4.13:80", cat="normative") }}

`@import` accepts exactly one argument, which **MUST** be a string literal specifying the module path.

{{ rule(id="4.13:81", cat="normative") }}

The return type of `@import` is a module struct type containing all `pub` declarations from the imported file.

{{ rule(id="4.13:82", cat="normative") }}

Module path resolution is defined authoritatively by rule
[10.2:1](@/10-modules/02-import-resolution.md). The reserved specifier
`"std"` follows rule [10.2:6](@/10-modules/02-import-resolution.md), not
importer-relative resolution. For any other path, a `.rue`-suffixed `P` selects
exactly the file `{P}` relative to the importing file's directory, while
extensionless `P` selects only the facade `{P}/_{basename}.rue`; rule
[10.2:2](@/10-modules/02-import-resolution.md) defines that single base
directory. These spellings are extension-sensitive: neither spelling falls
back to the other, and no file/facade ambiguity is diagnosed.

{{ rule(id="4.13:83", cat="legality-rule") }}

It is a compile-time error if the module path does not resolve to an existing file.

{{ rule(id="4.13:84", cat="legality-rule") }}

It is a compile-time error to pass a non-string-literal argument to `@import`.

{{ rule(id="4.13:85") }}

```rue
// math.rue
pub fn add(a: i32, b: i32) -> i32 { a + b }
pub fn sub(a: i32, b: i32) -> i32 { a - b }
fn helper() -> i32 { 42 }  // private, not exported

// main.rue
fn main() -> i32 {
    let math = @import("math.rue");
    math.add(1, 2)  // returns 3
}
```

{{ rule(id="4.13:86") }}

Private declarations (those without `pub`) are not visible to importers:

```rue
// main.rue
fn main() -> i32 {
    let math = @import("math.rue");
    // math.helper()  // Error: `helper` is not visible
    0
}
```

{{ rule(id="4.13:87") }}

The imported module can be bound to any name:

```rue
fn main() -> i32 {
    let m = @import("math.rue");
    m.add(1, 2)
}
```

{{ rule(id="4.13:88") }}

Nested paths are supported for importing from subdirectories:

```rue
fn main() -> i32 {
    let strings = @import("utils/strings.rue");
    0
}
```

{{ rule(id="4.13:89", cat="legality-rule") }}

It is a compile-time error (E0713) if a relative import path's normalized
candidate falls outside the project root (the root file's directory); see
rule [10.2:7](@/10-modules/02-import-resolution.md). Module identity is
project-root-relative, so a file outside the root can receive no identity.

{{ rule(id="4.13:133", cat="legality-rule") }}

It is a compile-time error (E0714) if an import path is empty or absolute;
see rule [10.2:8](@/10-modules/02-import-resolution.md). `@import` resolves a
relative path against the importing file's directory, so neither shape names
a module: the empty path names nothing, and an absolute path is anchored to a
machine rather than to the importing file.

{{ rule(id="4.13:90", cat="legality-rule") }}

It is a compile-time error to access a member of a module that is not
declared in the imported file. Module membership is per-file: even though all
compiled files share one global function namespace, a declaration from some
other file is not a member of the module and **MUST NOT** be reachable
through it.

## `@wrapping_add`, `@wrapping_sub`, `@wrapping_mul`

{{ rule(id="4.13:97", cat="normative") }}

The `@wrapping_add`, `@wrapping_sub`, and `@wrapping_mul` intrinsics each take
exactly two operands and compute, respectively, the sum, difference, and
product of those operands with two's-complement wraparound instead of the
overflow trap of the `+`, `-`, and `*` operators. Each is a non-trapping
counterpart to the corresponding checked operator.

{{ rule(id="4.13:98", cat="normative") }}

Both operands and the result of a wrapping-arithmetic intrinsic share a single
integer type, established by the same equality-and-integer constraints as the
checked `+`, `-`, and `*` operators (§4.2). It is a compile-time error to
apply one to a non-integer operand, and it is a compile-time error for the two
operands to have differing integer types.

{{ rule(id="4.13:99", cat="normative") }}

The intrinsics are defined for every integer type — the signed widths `i8`,
`i16`, `i32`, `i64` and the unsigned widths `u8`, `u16`, `u32`, `u64`. For a
result type of width `N` bits, the value of the intrinsic is the true
mathematical result reduced modulo `2^N` and interpreted as a two's-complement
value of the result type. Equivalently, the result is congruent to the exact
mathematical result modulo `2^N` and lies within the range of the result type.

{{ rule(id="4.13:100", cat="dynamic-semantics") }}

A wrapping-arithmetic intrinsic never traps: for every combination of operand
values, including those for which the checked operator would trap on overflow,
it produces the reduced value defined by 4.13:99. Because the low `N` bits of a
two's-complement product do not depend on the signedness of the operands,
`@wrapping_mul` yields the same bit pattern for a signed and an unsigned type
of the same width.

{{ rule(id="4.13:101", cat="normative") }}

In a compile-time context, a wrapping-arithmetic intrinsic is evaluated exactly
as `@intCast` is: the intrinsics introduce no new constant-evaluation rule, so
an invocation that `@intCast` would not fold into a constant is likewise not a
constant expression.

{{ rule(id="4.13:102") }}

```rue
fn main() -> i32 {
    // 127 + 1 wraps to the minimum i8 value.
    let hi: i8 = 127;
    let one: i8 = 1;
    @dbg(@wrapping_add(hi, one));      // -128

    // 0 - 1 wraps to the maximum u8 value.
    let zero: u8 = 0;
    let uone: u8 = 1;
    @dbg(@wrapping_sub(zero, uone));   // 255

    // The FNV-1a hashing step that overflows u64.
    let hash: u64 = 14695981039346656037;
    let prime: u64 = 1099511628211;
    @dbg(@wrapping_mul(hash, prime));  // 12638153115695167455

    0
}
```
