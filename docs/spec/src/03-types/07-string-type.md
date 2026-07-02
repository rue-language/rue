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

## Byte Access

{{ rule(id="3.7:15", cat="normative") }}

A `String` is a byte string: its contents are conventionally UTF-8 but are not
required to be valid UTF-8 (see ADR-0035). Byte access therefore operates on the
raw bytes and never inspects UTF-8 character boundaries.

{{ rule(id="3.7:16", cat="normative") }}

Indexing a `String` with an integer, `s[i]`, evaluates to the byte at byte
offset `i` as a value of type `u8`. The operation is `O(1)`.

{{ rule(id="3.7:17", cat="dynamic-semantics") }}

If the index `i` is greater than or equal to `s.len()`, evaluating `s[i]` traps
(index out of bounds), terminating the program the same way an out-of-bounds
array index does.

{{ rule(id="3.7:18") }}

```rue
fn main() -> i32 {
    let s = "café";   // 5 bytes: 'c' 'a' 'f' 0xC3 0xA9
    @dbg(s[0]);        // 99  ('c')
    @dbg(s[3]);        // 195 (0xC3)
    @dbg(s[4]);        // 169 (0xA9)
    0
}
```

{{ rule(id="3.7:19", cat="normative") }}

The method `s.substring(start, len)` returns a new `String` containing the byte
range `[start, start + len)` copied from `s`. Because `String` is a byte string,
any byte range is permitted; the range need not fall on UTF-8 character
boundaries. The receiver `s` is borrowed, not consumed.

{{ rule(id="3.7:20", cat="dynamic-semantics") }}

If `start + len` is greater than `s.len()` (or the addition overflows),
`s.substring(start, len)` traps (index out of bounds).

{{ rule(id="3.7:21") }}

```rue
fn main() -> i32 {
    let s = "café";
    let tail = s.substring(3, 2);   // the two bytes of 'é'
    @dbg(tail.len());               // 2
    0
}
```

## Limitations

{{ rule(id="3.7:14", cat="informative") }}

The current implementation does not support:
- String concatenation
- Slicing with range syntax (`s[a..b]`); use `s.substring(start, len)` instead
- Decoding bytes into Unicode scalar values (e.g. iterating `chars`)
- Pattern matching on strings

These features may be added in future versions.
