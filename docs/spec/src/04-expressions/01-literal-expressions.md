# Literal Expressions

r[4.1.1#normative]
A literal expression evaluates to a constant value.

## Integer Literals

r[4.1.2#normative]
An integer literal is a sequence of decimal digits that evaluates to an integer value.

r[4.1.3#normative]
Integer literals default to type `i32` unless the context requires a different type.

r[4.1.4]
```rue
fn main() -> i32 {
    0       // zero
    42      // positive integer
    255     // maximum u8 value
}
```

## Boolean Literals

r[4.1.5#normative]
The boolean literals are `true` and `false`, both of type `bool`.

r[4.1.6]
```rue
fn main() -> i32 {
    let a = true;
    let b = false;
    if a { 1 } else { 0 }
}
```
