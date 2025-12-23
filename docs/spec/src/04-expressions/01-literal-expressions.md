+++
title = "Literal Expressions"
weight = 1
template = "spec/page.html"
+++

# Literal Expressions

{{ rule(id="4.1:1", cat="normative") }}

A literal expression evaluates to a constant value.

## Integer Literals

{{ rule(id="4.1:2", cat="normative") }}

An integer literal is a sequence of decimal digits that evaluates to an integer value.

{{ rule(id="4.1:3", cat="normative") }}

Integer literals default to type `i32` unless the context requires a different type.

{{ rule(id="4.1:4") }}

```rue
fn main() -> i32 {
    0       // zero
    42      // positive integer
    255     // maximum u8 value
}
```

## Boolean Literals

{{ rule(id="4.1:5", cat="normative") }}

The boolean literals are `true` and `false`, both of type `bool`.

{{ rule(id="4.1:6") }}

```rue
fn main() -> i32 {
    let a = true;
    let b = false;
    if a { 1 } else { 0 }
}
```

## Unit Literal

{{ rule(id="4.1:7", cat="normative") }}

The unit literal `()` is an expression of type `()`.

{{ rule(id="4.1:8", cat="normative") }}

The unit literal evaluates to the single value of the unit type.

{{ rule(id="4.1:9") }}

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

{{ rule(id="4.1:10", cat="normative") }}

A string literal is a sequence of characters enclosed in double quotes, of type `String`.

{{ rule(id="4.1:11", cat="normative") }}

String literals support escape sequences: `\\` for a backslash and `\"` for a double quote.

{{ rule(id="4.1:12") }}

```rue
fn main() -> i32 {
    let a = "hello";
    let b = "world";
    let c = "with \"quotes\"";
    0
}
```
