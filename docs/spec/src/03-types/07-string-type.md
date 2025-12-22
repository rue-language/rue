# String Type

r[3.7:1#normative]
The type `String` represents a sequence of UTF-8 encoded bytes.

r[3.7:2#normative]
A `String` value is a tuple consisting of a pointer to the string data, the length in bytes, and a capacity field.

r[3.7:3#normative]
String literals are stored in read-only memory and are represented with a capacity of -1.

r[3.7:4]
```rue
fn main() -> i32 {
    let s = "hello";
    0
}
```

## String Literals

r[3.7:5#normative]
A string literal is a sequence of characters enclosed in double quotes (`"`).

r[3.7:6#normative]
String literals support the following escape sequences:

| Escape | Meaning |
|--------|---------|
| `\\` | Backslash |
| `\"` | Double quote |

r[3.7:7#normative]
An invalid escape sequence in a string literal is a compile-time error.

r[3.7:8]
```rue
fn main() -> i32 {
    let a = "hello world";
    let b = "with \"quotes\"";
    let c = "with \\ backslash";
    0
}
```

## String Equality

r[3.7:9#normative]
Strings support the equality operators `==` and `!=`.

r[3.7:10#normative]
Two strings are equal if they have the same length and identical byte content.

r[3.7:11]
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

r[3.7:12#normative]
The `@dbg` intrinsic accepts a `String` argument and prints its content followed by a newline.

r[3.7:13]
```rue
fn main() -> i32 {
    let msg = "Hello, world!";
    @dbg(msg);
    0
}
```

## String Copy Semantics

r[3.7:14#normative]
Strings have copy semantics. When a string is assigned to another variable, a deep copy is made.

r[3.7:15#normative]
The original and copied strings are independent; modifications to one do not affect the other.

r[3.7:16]
```rue
fn main() -> i32 {
    let a = "hello";
    let b = a;  // b is a copy of a
    // a and b are independent
    0
}
```

## String Concatenation

r[3.7:17#normative]
The `+` operator concatenates two strings, producing a new heap-allocated string.

r[3.7:18#normative]
The result of string concatenation is a new string with capacity greater than or equal to the combined length.

r[3.7:19]
```rue
fn main() -> i32 {
    let a = "hello";
    let b = " world";
    let c = a + b;  // c is "hello world"
    0
}
```

## Scope-Based Cleanup

r[3.7:20#normative]
Heap-allocated strings are automatically freed when they go out of scope.

r[3.7:21#normative]
String literals (with capacity -1) are not freed, as they reside in read-only memory.

r[3.7:22]
```rue
fn main() -> i32 {
    {
        let s = "hello" + " world";  // heap-allocated
        // s is used here
    }  // s is freed here
    0
}
```

## Limitations

r[3.7:23#informative]
The current implementation does not support:
- String indexing or slicing
- Pattern matching on strings

These features may be added in future versions.
