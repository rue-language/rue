+++
title = "String Type"
weight = 7
template = "spec/page.html"
+++

# String Type

{{ rule(id="3.7:1", cat="normative") }}

The type `String` represents an immutable sequence of UTF-8 encoded bytes.

{{ rule(id="3.7:2", cat="normative") }}

A `String` value occupies three machine words — a pointer to the string data,
the length in bytes, and the allocated capacity in bytes — so `@size_of(String)`
is `24` on a 64-bit target. A string literal has a capacity of zero and its
pointer refers to read-only memory (3.7:3); a `String` produced by an operation
that allocates (`@to_string`, 3.7:22, or `+`, 3.7:25) has a capacity greater
than zero and owns a heap buffer of that size.

{{ rule(id="3.7:3", cat="normative") }}

String literals are stored in read-only memory and have static lifetime.

{{ rule(id="3.7:4") }}

```rue
fn main() -> i32 {
    let s = "hello";
    0
}
```

## Representation and Ownership

{{ rule(id="3.7:39", cat="normative") }}

`String` is a move type (an affine type), not a `Copy` type (see
[Move Semantics](@/03-types/08-move-semantics.md)). Assigning a `String` to
another binding, passing it by value, or returning it moves it; the source
binding is left invalid, and using a moved `String` is a compile-time error
(E0205). Because `String` is not a `Copy` type, a `@copy` struct may not have a
`String` field. Unlike a `linear` type, a `String` need not be explicitly
consumed: an unused `String` is dropped implicitly at the end of its scope.

{{ rule(id="3.7:40", cat="dynamic-semantics") }}

A `String` owns its heap allocation and has a destructor (see
[Destructors](@/03-types/09-destructors.md)). When a `String` is dropped: if its
capacity is zero (a literal) no action is taken; if its capacity is greater than
zero the owned heap buffer is freed. Because a move transfers ownership, the
buffer is freed exactly once — at the drop of the final owner — and never at a
moved-from binding.

{{ rule(id="3.7:41", cat="informative") }}

The capacity of a heap-allocated `String`, and the growth strategy that chooses
it, are implementation-defined (1.3:6): a conforming program observes only the
length and byte content of a `String`, never its exact capacity. The
mutable-string extension ([Mutable Strings](@/03-types/10-mutable-strings.md),
section 3.10) builds on this same three-word representation.

{{ rule(id="3.7:42") }}

```rue
fn main() -> i32 {
    let a = "foo" + "bar";   // heap-allocated: capacity > 0
    let b = a;               // 'a' is moved into 'b'
    // let c = a;            // ERROR (E0205): use of moved value 'a'
    @dbg(b);                 // foobar
    0
}                            // 'b' is dropped: its heap buffer is freed once
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

## Integer Formatting

{{ rule(id="3.7:22", cat="normative") }}

The intrinsic `@to_string(n)` takes an argument of any integer type (`i8`,
`i16`, `i32`, `i64`, `u8`, `u16`, `u32`, or `u64`) and returns a new,
heap-allocated `String` containing the base-10 decimal representation of `n`
(see ADR-0035). The argument keeps its own type; a bare integer literal argument
is inferred to be `i32` (the default integer type).

{{ rule(id="3.7:23", cat="dynamic-semantics") }}

`@to_string(n)` formats the entire range of the argument's type, including
`i64::MIN` and `u64::MAX`. The value is formatted according to its type's
signedness: an unsigned value with its high bit set formats as its unsigned
magnitude, never as a negative number. A negative signed value is prefixed with
a single `-`; a zero value formats as `0`.

{{ rule(id="3.7:24") }}

```rue
fn main() -> i32 {
    @dbg(@to_string(42));    // 42
    @dbg(@to_string(-5));    // -5
    0
}
```

## Concatenation

{{ rule(id="3.7:25", cat="normative") }}

When both operands of the `+` operator are `String`, `s1 + s2` evaluates to a
new, heap-allocated `String` whose bytes are the bytes of `s1` followed by the
bytes of `s2` (see ADR-0035). Both operands are borrowed, not consumed, and
remain usable afterwards.

{{ rule(id="3.7:26", cat="legality-rule") }}

The `+` operator requires both operands to have the same type. Mixing a `String`
and an integer (for example `s + 1`) is a type error; there is no implicit
conversion between `String` and integers.

{{ rule(id="3.7:27") }}

```rue
fn main() -> i32 {
    let greeting = "Hello, " + "world!";
    @dbg(greeting);   // Hello, world!
    0
}
```

## Output

{{ rule(id="3.7:35", cat="normative") }}

The free function `print(s)` takes a `String` and writes its raw bytes to
standard output, adding nothing. Unlike `@dbg`, it does not append a newline and
does not apply any debug formatting. The argument `s` is borrowed, not consumed,
and remains usable afterwards.

{{ rule(id="3.7:36", cat="normative") }}

The free function `println(s)` takes a `String` and writes its raw bytes to
standard output followed by a single newline (`U+000A`). The argument `s` is
borrowed, not consumed. Together with `@to_string` and `+`, `println` composes
line-oriented output; there is no formatting or interpolation syntax.

{{ rule(id="3.7:37", cat="dynamic-semantics") }}

`print(s)` and `println(s)` write exactly the bytes of `s`, in order, without
transformation: because a `String` is a byte string, the output is byte-for-byte
identical to the string's contents (the only difference between the two is the
single trailing newline `println` adds). Writing an empty `String` writes no
bytes (for `print`) or a lone newline (for `println`).

{{ rule(id="3.7:38") }}

```rue
fn main() -> i32 {
    print("hello");                       // hello
    print(" world");                      // hello world   (no newline yet)
    println("");                          // hello world\n
    println("value is " + @to_string(42)); // value is 42\n
    0
}
```

## Search

{{ rule(id="3.7:28", cat="normative") }}

The method `s.contains(needle)` returns `true` if and only if the bytes of the
`String` `needle` occur as a contiguous subsequence of the bytes of `s`. The
comparison is byte-level and does not inspect UTF-8 character boundaries. The
empty needle is contained in every string. The receiver `s` is borrowed, not
consumed.

{{ rule(id="3.7:29", cat="normative") }}

The method `s.starts_with(prefix)` returns `true` if and only if the bytes of
the `String` `prefix` are a prefix of the bytes of `s`. The comparison is
byte-level. The empty prefix matches every string. The receiver `s` is borrowed,
not consumed.

{{ rule(id="3.7:30") }}

```rue
fn main() -> i32 {
    let h = "hello";
    @dbg(h.contains("ell"));      // true
    @dbg(h.starts_with("he"));    // true
    @dbg(h.starts_with("lo"));    // false
    0
}
```

## Character Iteration

{{ rule(id="3.7:31", cat="normative") }}

The character view `s.chars()` yields the Unicode scalar values of a `String`,
decoding its bytes as UTF-8. It is used as the iterable of a `for` loop (see
[Loop Expressions](@/04-expressions/08-loop-expressions.md)), which binds each
scalar value as a `u32` in ascending byte order.

{{ rule(id="3.7:32", cat="dynamic-semantics") }}

Decoding through `s.chars()` is strict: a byte sequence that is not well-formed
UTF-8 (an ill-formed, truncated, overlong, or surrogate sequence) traps at
runtime when it is decoded. Because a `String` is a byte string that may hold
arbitrary bytes, this "trap, don't corrupt" behavior at the decode boundary is
where invalidity is caught.

{{ rule(id="3.7:34", cat="dynamic-semantics") }}

The lossy character view `s.chars_lossy()` yields the same Unicode scalar values
as `s.chars()` for well-formed UTF-8, but instead of trapping it substitutes the
Unicode replacement scalar `U+FFFD` (decimal `65533`) for each maximal subpart
of an ill-formed subsequence and continues. Lossiness is explicit: `chars_lossy`
is the only way to decode without trapping, so silent corruption is never the
default. Like `chars`, it is used as the iterable of a `for` loop and binds each
scalar value as a `u32`.

{{ rule(id="3.7:33") }}

```rue
fn main() -> i32 {
    let s = "café";
    let mut count = 0;
    for c in s.chars() {
        @dbg(c);          // 99, 97, 102, 233 (the last is é = U+00E9)
        count = count + 1;
    }
    count  // 4 scalar values (though the string is 5 bytes)
}
```

## Limitations

{{ rule(id="3.7:14", cat="informative") }}

The current implementation does not support:
- Slicing with range syntax (`s[a..b]`); use `s.substring(start, len)` instead
- Pattern matching on strings

These features may be added in future versions.
