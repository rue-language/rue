# String Type

r[3.7.1#normative]
The type `String` represents an immutable sequence of UTF-8 encoded bytes.

r[3.7.2#normative]
A `String` value is a fat pointer consisting of a pointer to the string data and the length in bytes.

r[3.7.3#normative]
String literals are stored in read-only memory and have static lifetime.

r[3.7.4]
```rue
fn main() -> i32 {
    let s = "hello";
    0
}
```

## String Literals

r[3.7.5#normative]
A string literal is a sequence of characters enclosed in double quotes (`"`).

r[3.7.6#normative]
String literals support the following escape sequences:

| Escape | Meaning |
|--------|---------|
| `\\` | Backslash |
| `\"` | Double quote |

r[3.7.7#normative]
An invalid escape sequence in a string literal is a compile-time error.

r[3.7.8]
```rue
fn main() -> i32 {
    let a = "hello world";
    let b = "with \"quotes\"";
    let c = "with \\ backslash";
    0
}
```

## String Equality

r[3.7.9#normative]
Strings support the equality operators `==` and `!=`.

r[3.7.10#normative]
Two strings are equal if they have the same length and identical byte content.

r[3.7.11]
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

r[3.7.12#normative]
The `@dbg` intrinsic accepts a `String` argument and prints its content followed by a newline.

r[3.7.13]
```rue
fn main() -> i32 {
    let msg = "Hello, world!";
    @dbg(msg);
    0
}
```

## Limitations

r[3.7.14#informative]
The current implementation does not support:
- String concatenation
- String indexing or slicing
- Pattern matching on strings
- Mutable strings

These features may be added in future versions.
