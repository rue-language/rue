+++
title = "Integer Types"
weight = 1
template = "spec/page.html"
+++

# Integer Types

## Signed Integer Types

{{ rule(id="3.1:1", cat="normative") }}

A signed integer type is one of: `i8`, `i16`, `i32`, or `i64`.

{{ rule(id="3.1:2", cat="normative") }}

The type `i8` represents signed integers in the range [-128, 127].

{{ rule(id="3.1:3", cat="normative") }}

The type `i16` represents signed integers in the range [-32768, 32767].

{{ rule(id="3.1:4", cat="normative") }}

The type `i32` represents signed integers in the range [-2147483648, 2147483647].

{{ rule(id="3.1:5", cat="normative") }}

The type `i64` represents signed integers in the range [-9223372036854775808, 9223372036854775807].

{{ rule(id="3.1:6", cat="dynamic-semantics") }}

Signed integer arithmetic that overflows **MUST** cause a runtime panic.

{{ rule(id="3.1:7") }}

```rue
fn main() -> i32 {
    let a: i8 = 127;
    let b: i16 = 32767;
    let c: i32 = 2147483647;
    let d: i64 = 9223372036854775807;
    0
}
```

## Unsigned Integer Types

{{ rule(id="3.1:8", cat="normative") }}

An unsigned integer type is one of: `u8`, `u16`, `u32`, or `u64`.

{{ rule(id="3.1:9", cat="normative") }}

The type `u8` represents unsigned integers in the range [0, 255].

{{ rule(id="3.1:10", cat="normative") }}

The type `u16` represents unsigned integers in the range [0, 65535].

{{ rule(id="3.1:11", cat="normative") }}

The type `u32` represents unsigned integers in the range [0, 4294967295].

{{ rule(id="3.1:12", cat="normative") }}

The type `u64` represents unsigned integers in the range [0, 18446744073709551615].

{{ rule(id="3.1:13", cat="dynamic-semantics") }}

Unsigned integer arithmetic that overflows **MUST** cause a runtime panic.

## Pointer-Width Integer Types

{{ rule(id="3.1:21", cat="normative") }}

A pointer-width integer type is one of: `usize` (unsigned) or `isize` (signed). These types have the width of a machine address on the target platform.

{{ rule(id="3.1:22", cat="normative") }}

All supported targets are 64-bit, so `usize` has the same representation, range, and overflow behavior as `u64`, and `isize` has the same representation, range, and overflow behavior as `i64`. The names resolve to the same types and may be used interchangeably wherever a type is named (let bindings, function parameters, return types, struct fields).

{{ rule(id="3.1:23") }}

```rue
fn get(a: [i32; 3], i: usize) -> i32 { a[i] }

fn main() -> i32 {
    let arr: [i32; 3] = [4, 5, 6];
    let i: usize = 2;
    let d: isize = -1;
    if d == -1 { get(arr, i) } else { 0 }
}
```

## Integer Literal Type Inference

{{ rule(id="3.1:14", cat="normative") }}

An integer literal without explicit type annotation defaults to type `i32`.

{{ rule(id="3.1:15", cat="normative") }}

When an integer literal appears in a context where the expected type is known (e.g., assignment to a typed variable), the literal is inferred to have that type.

{{ rule(id="3.1:16") }}

```rue
fn main() -> i32 {
    let x = 42;           // x has type i32 (default)
    let y: i64 = 100;     // 100 is inferred as i64
    0
}
```

## Integer Literal Range Validation

{{ rule(id="3.1:17", cat="legality-rule") }}

A compiler **MUST** reject programs where an integer literal value exceeds the representable range of its target type.

{{ rule(id="3.1:18", cat="normative") }}

When an integer literal is the operand of a unary negation operator, and the negated value would be representable in the target signed integer type, the expression is valid even if the literal value itself exceeds the positive range of that type. This allows the minimum value of each signed integer type to be written as a negated literal.

{{ rule(id="3.1:19") }}

```rue
fn main() -> i32 {
    let a: i8 = -128;                       // valid: -128 is in i8 range
    let b: i8 = 128;                        // error: 128 exceeds i8 range
    let c: i32 = -2147483648;               // valid: i32 minimum value
    let d: i64 = -9223372036854775808;      // valid: i64 minimum value
    0
}
```

{{ rule(id="3.1:20") }}

```rue
fn main() -> i32 {
    let x: i8 = 128;           // error: literal out of range for i8
    let y: u8 = 256;           // error: literal out of range for u8
    let z: i32 = 9999999999;   // error: literal out of range for i32
    0
}
```

## Literal Base Independence

{{ rule(id="3.1:24", cat="normative") }}

The written base of an integer literal (decimal, hexadecimal, octal, or binary; see 2.1) does not affect its semantics: the literal denotes the same value as its decimal spelling, and type inference (3.1:14, 3.1:15) and range validation (3.1:17) apply identically regardless of base.

{{ rule(id="3.1:25") }}

```rue
fn main() -> i32 {
    let a: u64 = 0xFFFF_FFFF_FFFF_FFFF;  // valid: u64 maximum value
    let b: u8 = 0xFF;                    // valid: 255 fits in u8
    let c = 0x10;                        // c has type i32 (default), value 16
    let d: i32 = 0xFFFF_FFFF_FFFF_FFFF;  // error: out of range for i32
    0
}
```
