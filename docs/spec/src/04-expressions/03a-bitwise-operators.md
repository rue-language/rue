# Bitwise Operators

Bitwise operators perform bit-level operations on integer values.

## Binary Bitwise Operators

r[4.3a:1#normative]
Binary bitwise operators take two operands of the same integer type and produce a result of that type.

| Operator | Name | Description |
|----------|------|-------------|
| `&` | Bitwise AND | Sets each bit if both operand bits are 1 |
| `\|` | Bitwise OR | Sets each bit if either operand bit is 1 |
| `^` | Bitwise XOR | Sets each bit if exactly one operand bit is 1 |

r[4.3a:2]
```rue
fn main() -> i32 {
    let a: i32 = 0b1100;
    let b: i32 = 0b1010;
    let and = a & b;   // 0b1000 = 8
    let or = a | b;    // 0b1110 = 14
    let xor = a ^ b;   // 0b0110 = 6
    and
}
```

## Bitwise NOT

r[4.3a:3#normative]
The bitwise NOT operator `~` inverts all bits of its operand.

r[4.3a:4#normative]
Bitwise NOT takes a single integer operand and produces a result of the same type.

r[4.3a:5]
```rue
fn main() -> i32 {
    let a: i32 = 0b0101;
    let not_a = ~a;   // All bits inverted
    0
}
```

## Shift Operators

r[4.3a:6#normative]
Shift operators move bits left or right by a specified number of positions.

| Operator | Name | Description |
|----------|------|-------------|
| `<<` | Left Shift | Shifts bits left, filling with zeros |
| `>>` | Right Shift | Shifts bits right |

r[4.3a:7#normative]
For left shift (`<<`), vacated bit positions are filled with zeros.

r[4.3a:8#normative]
For right shift (`>>`), the behavior depends on the signedness of the operand type:
- For unsigned types, vacated bit positions are filled with zeros (logical shift).
- For signed types, vacated bit positions are filled with copies of the sign bit (arithmetic shift).

r[4.3a:9#normative]
The shift amount operand shall have the same type as the value being shifted.

r[4.3a:10#normative]
If the shift amount is greater than or equal to the bit width of the type, the behavior is defined as shifting by the amount modulo the bit width. For example, shifting an `i32` by 33 positions is equivalent to shifting by 1 position.

r[4.3a:11]
```rue
fn main() -> i32 {
    let x: i32 = 1;
    let left = x << 4;    // 16 (binary: 10000)
    let right = left >> 2; // 4  (binary: 100)
    right
}
```

## Operator Precedence

r[4.3a:12#normative]
Bitwise operator precedence (highest to lowest within this group):
1. `~` (bitwise NOT, unary)
2. `<<`, `>>` (shift operators)
3. `&` (bitwise AND)
4. `^` (bitwise XOR)
5. `|` (bitwise OR)

r[4.3a:13#normative]
Shift operators have higher precedence than arithmetic operators. Bitwise AND, XOR, and OR have lower precedence than comparison operators.

r[4.3a:14#normative]
Parentheses can be used to override the default precedence.

r[4.3a:15]
```rue
fn main() -> i32 {
    let a: i32 = 1 | 2 & 3;   // = 1 | (2 & 3) = 1 | 2 = 3
    let b: i32 = (1 | 2) & 3; // = 3 & 3 = 3
    a
}
```

## Associativity

r[4.3a:16#normative]
All binary bitwise operators are left-associative.

r[4.3a:17]
```rue
fn main() -> i32 {
    let x: i32 = 1 << 2 << 1;  // = (1 << 2) << 1 = 4 << 1 = 8
    x
}
```

## Type Checking

r[4.3a:18#normative]
Bitwise operators are only valid for integer types (`i8`, `i16`, `i32`, `i64`, `u8`, `u16`, `u32`, `u64`).

r[4.3a:19#normative]
Using bitwise operators on boolean or other non-integer types is a compile-time error.
