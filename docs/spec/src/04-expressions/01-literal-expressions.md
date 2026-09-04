+++
title = "Literal Expressions"
weight = 1
template = "spec/page.html"
+++

# Literal Expressions

{{ rule(id="4.1:1", cat="dynamic-semantics") }}

A literal expression evaluates to a constant value.

## Integer Literals

{{ rule(id="4.1:2", cat="dynamic-semantics") }}

An integer literal is a sequence of decimal digits that evaluates to an integer value.

{{ rule(id="4.1:3", cat="normative") }}

Integer literals default to type `i32` unless the context requires a different type.

{{ rule(id="4.1:4") }}

```rue
fn main() -> i32 {
    @dbg(0);      // zero
    @dbg(42);     // positive integer
    @dbg(255);    // maximum u8 value
    0
}
```

## Float Literals

{{ rule(id="4.1:13", cat="dynamic-semantics") }}

A float literal (2.1:29) evaluates to the floating-point value obtained by
rounding its exact written decimal value to the nearest value of its type, ties
to even (3.12:9).

{{ rule(id="4.1:14", cat="normative") }}

A float literal has type `comptime_float` and takes a concrete floating-point
type from its context; with no such context its type is `f64` (3.12:7,
3.12:8). Unlike an integer literal it is never given an integer type, however
integral its value.

{{ rule(id="4.1:15") }}

```rue
fn main() -> i32 {
    let a = 2.5;         // f64 by default
    let b: f32 = 2.5;    // f32 by annotation
    @dbg(a);
    @dbg(b);
    0
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

{{ rule(id="4.1:8", cat="dynamic-semantics") }}

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

A string literal is a sequence of characters enclosed in double quotes. Without a contextual text type, its type is the stable core `str` view; a context may promote it to another text rung such as `Str(N)` or the explicitly imported standard-library `StrBuf`.

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
