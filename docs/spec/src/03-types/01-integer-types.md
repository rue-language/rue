# Integer Types

## Signed Integer Types

r[3.1.1#normative]
A signed integer type is one of: `i8`, `i16`, `i32`, or `i64`.

r[3.1.2#normative]
The type `i8` represents signed integers in the range [-128, 127].

r[3.1.3#normative]
The type `i16` represents signed integers in the range [-32768, 32767].

r[3.1.4#normative]
The type `i32` represents signed integers in the range [-2147483648, 2147483647].

r[3.1.5#normative]
The type `i64` represents signed integers in the range [-9223372036854775808, 9223372036854775807].

r[3.1.6#normative]
Signed integer arithmetic that overflows causes a runtime panic.

r[3.1.7]
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

r[3.1.8#normative]
An unsigned integer type is one of: `u8`, `u16`, `u32`, or `u64`.

r[3.1.9#normative]
The type `u8` represents unsigned integers in the range [0, 255].

r[3.1.10#normative]
The type `u16` represents unsigned integers in the range [0, 65535].

r[3.1.11#normative]
The type `u32` represents unsigned integers in the range [0, 4294967295].

r[3.1.12#normative]
The type `u64` represents unsigned integers in the range [0, 18446744073709551615].

r[3.1.13#normative]
Unsigned integer arithmetic that overflows causes a runtime panic.

## Integer Literal Type Inference

r[3.1.14#normative]
An integer literal without explicit type annotation defaults to type `i32`.

r[3.1.15#normative]
When an integer literal appears in a context where the expected type is known (e.g., assignment to a typed variable), the literal is inferred to have that type.

r[3.1.16]
```rue
fn main() -> i32 {
    let x = 42;           // x has type i32 (default)
    let y: i64 = 100;     // 100 is inferred as i64
    0
}
```

## Integer Literal Range Validation

r[3.1.17#normative]
A compile-time error occurs when an integer literal value exceeds the representable range of its target type.

r[3.1.18]
```rue
fn main() -> i32 {
    let x: i8 = 128;           // error: literal out of range for i8
    let y: u8 = 256;           // error: literal out of range for u8
    let z: i32 = 9999999999;   // error: literal out of range for i32
    0
}
```
