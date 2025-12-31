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

## `@dbg`

{{ rule(id="4.13:6", cat="normative") }}

The `@dbg` intrinsic prints a value to standard output for debugging purposes.

{{ rule(id="4.13:7", cat="normative") }}

`@dbg` accepts exactly one argument of integer, boolean, or string type.

{{ rule(id="4.13:8", cat="normative") }}

`@dbg` prints the value followed by a newline character.

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
    @size_of(i32)     // 8 (one 8-byte slot)
}
```

{{ rule(id="4.13:17") }}

```rue
struct Point { x: i32, y: i32 }

fn main() -> i32 {
    @size_of(Point)   // 16 (two 8-byte slots)
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

{{ rule(id="4.13:22", cat="normative") }}

All types in Rue currently have 8-byte alignment.

{{ rule(id="4.13:23") }}

```rue
fn main() -> i32 {
    @align_of(i32)    // 8
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

## `@read_line`

{{ rule(id="4.13:33", cat="normative") }}

The `@read_line` intrinsic reads a line of text from standard input.

{{ rule(id="4.13:34", cat="normative") }}

`@read_line` accepts no arguments.

{{ rule(id="4.13:35", cat="normative") }}

The return type of `@read_line` is `String`.

{{ rule(id="4.13:36", cat="dynamic-semantics") }}

`@read_line` reads bytes from standard input until a newline character (`\n`) is encountered or end-of-file is reached.

{{ rule(id="4.13:37", cat="dynamic-semantics") }}

The returned `String` does **not** include the trailing newline character.

{{ rule(id="4.13:38", cat="dynamic-semantics") }}

If end-of-file is reached with some data read, the partial line is returned.

{{ rule(id="4.13:39", cat="dynamic-semantics") }}

If end-of-file is reached with no data read, a runtime panic occurs with the message "unexpected end of input".

{{ rule(id="4.13:40", cat="informative") }}

If a read error occurs, a runtime panic occurs with the message "input error". (This behavior is documented but not tested, as I/O errors cannot be reliably simulated in portable test environments.)

{{ rule(id="4.13:41") }}

```rue
fn main() -> i32 {
    @dbg("What is your name?");
    let name = @read_line();
    @dbg("Hello, ");
    @dbg(name);
    0
}
```

{{ rule(id="4.13:42") }}

Reading multiple lines:

```rue
fn main() -> i32 {
    let line1 = @read_line();  // First line
    let line2 = @read_line();  // Second line
    @dbg(line1);
    @dbg(line2);
    0
}
```

## Integer Parsing Intrinsics

{{ rule(id="4.13:43", cat="normative") }}

The integer parsing intrinsics convert a string to an integer value.

{{ rule(id="4.13:44", cat="normative") }}

The following parsing intrinsics are available:
- `@parse_i32` returns `i32`
- `@parse_i64` returns `i64`
- `@parse_u32` returns `u32`
- `@parse_u64` returns `u64`

{{ rule(id="4.13:45", cat="normative") }}

Each parsing intrinsic accepts exactly one argument, which **MUST** be of type `String`.

{{ rule(id="4.13:46", cat="normative") }}

The string argument is borrowed, not consumed. The original string remains valid after parsing.

{{ rule(id="4.13:47", cat="normative") }}

The parsed string must match the following grammar:

```ebnf
integer_string = [ "-" ] digit { digit } ;
digit = "0" | "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" ;
```

{{ rule(id="4.13:48", cat="legality-rule") }}

Leading minus signs are only allowed for signed types (`@parse_i32`, `@parse_i64`).

{{ rule(id="4.13:49", cat="dynamic-semantics") }}

A runtime panic occurs if:
- The string is empty
- The string contains non-digit characters (other than an optional leading minus)
- The value overflows the target type
- A negative value is parsed for an unsigned type

{{ rule(id="4.13:50") }}

```rue
fn main() -> i32 {
    let s = "42";
    let n = @parse_i32(s);
    n  // returns 42
}
```

{{ rule(id="4.13:51") }}

```rue
fn main() -> i32 {
    let s = "-17";
    let n = @parse_i32(s);
    n  // returns -17
}
```

{{ rule(id="4.13:52") }}

```rue
fn main() -> i32 {
    let s = "42";
    // String is borrowed, not consumed
    let n = @parse_i32(s);
    @dbg(s);  // s is still valid
    n
}
```

{{ rule(id="4.13:53") }}

```rue
// This panics at runtime: invalid character
fn main() -> i32 {
    let s = "12abc";
    let n = @parse_i32(s);  // panic: invalid character
    n
}
```

{{ rule(id="4.13:54") }}

```rue
// This panics at runtime: negative for unsigned
fn main() -> i32 {
    let s = "-17";
    let n: u32 = @parse_u32(s);  // panic: negative value for unsigned type
    @intCast(n)
}
```
