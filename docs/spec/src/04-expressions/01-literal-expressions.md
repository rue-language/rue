# Literal Expressions

r[4.1:1#normative]
A literal expression evaluates to a constant value.

## Integer Literals

r[4.1:2#normative]
An integer literal is a sequence of decimal digits that evaluates to an integer value.

r[4.1:3#normative]
Integer literals default to type `i32` unless the context requires a different type.

r[4.1:4]
```rue
fn main() -> i32 {
    0       // zero
    42      // positive integer
    255     // maximum u8 value
}
```

## Boolean Literals

r[4.1:5#normative]
The boolean literals are `true` and `false`, both of type `bool`.

r[4.1:6]
```rue
fn main() -> i32 {
    let a = true;
    let b = false;
    if a { 1 } else { 0 }
}
```

## Unit Literal

r[4.1:7#normative]
The unit literal `()` is an expression of type `()`.

r[4.1:8#normative]
The unit literal evaluates to the single value of the unit type.

r[4.1:9]
```rue
fn returns_unit() -> () {
    ()
}

fn main() -> i32 {
    let u = ();
    returns_unit();
    0
}
```

## String Literals

r[4.1:10#normative]
A string literal is a sequence of characters enclosed in double quotes, of type `String`.

r[4.1:11#normative]
String literals support escape sequences: `\\` for a backslash and `\"` for a double quote.

r[4.1:12]
```rue
fn main() -> i32 {
    let a = "hello";
    let b = "world";
    let c = "with \"quotes\"";
    0
}
```
