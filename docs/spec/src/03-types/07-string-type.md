+++
title = "String Type"
weight = 7
template = "spec/page.html"
+++

# String Type

{{ rule(id="3.7:1", cat="normative") }}

The type `String` represents an immutable sequence of UTF-8 encoded bytes.

{{ rule(id="3.7:2", cat="normative") }}

A `String` value is a fat pointer consisting of a pointer to the string data and the length in bytes.

{{ rule(id="3.7:3", cat="normative") }}

String literals are stored in read-only memory and have static lifetime.

{{ rule(id="3.7:4") }}

```rue
fn main() -> i32 {
    let s = "hello";
    0
}
```

## String Literals

{{ rule(id="3.7:5", cat="normative") }}

A string literal is a sequence of characters enclosed in double quotes (`"`).

{{ rule(id="3.7:6", cat="normative") }}

String literals support the following escape sequences:

| Escape | Meaning |
|--------|---------|
| `\\` | Backslash |
| `\"` | Double quote |
| `\n` | Newline (line feed, U+000A) |
| `\t` | Horizontal tab (U+0009) |
| `\r` | Carriage return (U+000D) |
| `\0` | Null character (U+0000) |

{{ rule(id="3.7:7", cat="normative") }}

An invalid escape sequence in a string literal is a compile-time error.

{{ rule(id="3.7:8") }}

```rue
fn main() -> i32 {
    let a = "hello world";
    let b = "with \"quotes\"";
    let c = "with \\ backslash";
    let d = "line1\nline2";   // newline
    let e = "col1\tcol2";     // tab
    0
}
```

## String Equality

{{ rule(id="3.7:9", cat="normative") }}

Strings support the equality operators `==` and `!=`.

{{ rule(id="3.7:10", cat="normative") }}

Two strings are equal if they have the same length and identical byte content.

{{ rule(id="3.7:11") }}

```rue
fn main() -> i32 {
    let a = "hello";
    let b = "hello";
    let c = "world";
    if a == b && a != c {
        0
    } else {
        1
    }
}
```

## String Debugging

{{ rule(id="3.7:12", cat="normative") }}

The `@dbg` intrinsic accepts a `String` argument and prints its content followed by a newline.

{{ rule(id="3.7:13") }}

```rue
fn main() -> i32 {
    let msg = "Hello, world!";
    @dbg(msg);
    0
}
```

## Limitations

{{ rule(id="3.7:14", cat="informative") }}

The current implementation does not support:
- String concatenation
- String indexing or slicing
- Pattern matching on strings
- Mutable strings

These features may be added in future versions.
