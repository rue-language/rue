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

{{ rule(id="3.1:6", cat="normative") }}

Signed integer arithmetic that overflows causes a runtime panic.

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

{{ rule(id="3.1:13", cat="normative") }}

Unsigned integer arithmetic that overflows causes a runtime panic.

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

{{ rule(id="3.1:17", cat="normative") }}

A compile-time error occurs when an integer literal value exceeds the representable range of its target type.

{{ rule(id="3.1:18") }}

```rue
fn main() -> i32 {
    let x: i8 = 128;           // error: literal out of range for i8
    let y: u8 = 256;           // error: literal out of range for u8
    let z: i32 = 9999999999;   // error: literal out of range for i32
    0
}
```
