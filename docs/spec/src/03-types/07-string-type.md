+++
title = "String Types"
weight = 7
template = "spec/page.html"
+++

# String Types

{{ rule(id="3.7:1", cat="normative") }}

The core type `str` represents a read-only byte-string view. The fixed-capacity
type `Str(N)` and the standard-library type `std.strbuf.StrBuf` are its buffer
counterparts.

{{ rule(id="3.7:57", cat="informative") }}

ADR-0043 names this growable-string type `StrBuf` (the string analog of the
growable collection `ArrayBuf`), framing it as the *growable rung* of the
string trio (`str` / `Str(N)` / `StrBuf`) rather than a blessed built-in.

{{ rule(id="3.7:2", cat="normative") }}

A `StrBuf` value is available only through an explicit import of the trusted
standard-library module. It occupies three machine words — a pointer to the string data,
the length in bytes, and the allocated capacity in bytes — so `@size_of(StrBuf)`
is `24` on a 64-bit target. A `StrBuf` produced by an operation
that allocates (`@to_string`, 3.7:22, or `+`, 3.7:25) has a capacity greater
than zero and owns a heap buffer of that size.

{{ rule(id="3.7:3", cat="normative") }}

String literals are stored in read-only memory, have static lifetime, and infer
to the first-class core type `str` unless a trusted `StrBuf` or `Str(N)` context
requires materialization.

{{ rule(id="3.7:4") }}

```rue
fn main() -> i32 {
    let s = "hello";
    0
}
```

## Representation and Ownership

{{ rule(id="3.7:39", cat="normative") }}

`StrBuf` is a move type (an affine type), not a `Copy` type (see
[Move Semantics](@/03-types/08-move-semantics.md)). Assigning a `StrBuf` to
another binding, passing it by value, or returning it moves it; the source
binding is left invalid, and using a moved `StrBuf` is a compile-time error
(E0205). Because `StrBuf` is not a `Copy` type, a `@copy` struct may not have a
`StrBuf` field. Unlike a `linear` type, a `StrBuf` need not be explicitly
consumed: an unused `StrBuf` is dropped implicitly at the end of its scope.

{{ rule(id="3.7:40", cat="dynamic-semantics") }}

A `StrBuf` owns its heap allocation and has a destructor (see
[Destructors](@/03-types/09-destructors.md)). When a `StrBuf` is dropped: if its
capacity is zero (a literal) no action is taken; if its capacity is greater than
zero the owned heap buffer is freed. Because a move transfers ownership, the
buffer is freed exactly once — at the drop of the final owner — and never at a
moved-from binding.

{{ rule(id="3.7:41", cat="informative") }}

The capacity of a heap-allocated `StrBuf`, and the growth strategy that chooses
it, are implementation-defined (1.3:6). Capacity is nonetheless *observable*:
`capacity()` (§3.10:11) returns the exact allocated capacity, and the value it
returns is the implementation-defined quantity itself, not a normalized one. A
program that only reads the length and byte content of a `StrBuf` is portable
across conforming implementations; one whose result depends on `capacity()` — or
on how much a single append grows it — depends on an implementation-defined
choice this implementation documents in §3.10:24 and may change in a later
release. The mutable-string extension
([Mutable Strings](@/03-types/10-mutable-strings.md), section 3.10) builds on
this same three-word representation.

{{ rule(id="3.7:42") }}

```rue
const std = @import("std");
const StrBuf = std.strbuf.StrBuf;

fn main() -> i32 {
    let left: StrBuf = "foo";
    let right: StrBuf = "bar";
    let a = left + right;      // heap-allocated: capacity > 0
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

## StrBuf Debugging

{{ rule(id="3.7:12", cat="normative") }}

The `@dbg` intrinsic accepts any text rung and prints its content followed by a newline.

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

A `StrBuf` is a byte string: its contents are conventionally UTF-8 but are not
required to be valid UTF-8 (see ADR-0035). Byte access therefore operates on the
raw bytes and never inspects UTF-8 character boundaries.

{{ rule(id="3.7:16", cat="normative") }}

Indexing a `StrBuf` with an integer, `s[i]`, evaluates to the byte at byte
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

The method `s.substring(start, len)` returns a new `StrBuf` containing the byte
range `[start, start + len)` copied from `s`. Because `StrBuf` is a byte string,
any byte range is permitted; the range need not fall on UTF-8 character
boundaries. The receiver `s` is borrowed, not consumed.

{{ rule(id="3.7:20", cat="dynamic-semantics") }}

If `start + len` is greater than `s.len()` (or the addition overflows),
`s.substring(start, len)` traps (index out of bounds).

{{ rule(id="3.7:21") }}

```rue
const std = @import("std");
const StrBuf = std.strbuf.StrBuf;

fn main() -> i32 {
    let s: StrBuf = "café";
    let tail = s.substring(3, 2);   // the two bytes of 'é'
    @dbg(tail.len());               // 2
    0
}
```

## Integer Formatting

{{ rule(id="3.7:22", cat="normative") }}

The intrinsic `@to_string(n)` takes an argument of any integer type (`i8`,
`i16`, `i32`, `i64`, `u8`, `u16`, `u32`, or `u64`) or of either floating-point
type (`f32`, `f64`; the text it produces for one is 3.12:40–3.12:42) and returns
a new, heap-allocated `StrBuf` containing the decimal representation of `n`
(see ADR-0035). The argument keeps its own type; a bare integer literal argument
is inferred to be `i32` (the default integer type). Using `@to_string` requires
the enclosing file to import the standard library lexically
(`const std = @import("std");`), because the intrinsic names `StrBuf` in its
result type without rooting the trusted-module demand itself. In a file with no
such import, a use of `@to_string` is rejected with E0204 (unknown type
`StrBuf`) — the import must be present, but it need not be otherwise used, and
the name `StrBuf` need not be brought into scope. This distinguishes
`@to_string` from `@read_line` (§4.13:35) and the `@parse_*` intrinsics
(§4.13:44), which do root the trusted module themselves and so have their
`Option` result type even without a lexical `std` import.

{{ rule(id="3.7:23", cat="dynamic-semantics") }}

`@to_string(n)` formats the entire range of the argument's type, including
`i64::MIN` and `u64::MAX`. The value is formatted according to its type's
signedness: an unsigned value with its high bit set formats as its unsigned
magnitude, never as a negative number. A negative signed value is prefixed with
a single `-`; a zero value formats as `0`.

{{ rule(id="3.7:24") }}

```rue
const std = @import("std");

fn main() -> i32 {
    @dbg(@to_string(42));    // 42
    @dbg(@to_string(-5));    // -5
    0
}
```

## Concatenation

{{ rule(id="3.7:25", cat="normative") }}

When both operands of the `+` operator are `StrBuf`, `s1 + s2` evaluates to a
new, heap-allocated `StrBuf` whose bytes are the bytes of `s1` followed by the
bytes of `s2` (see ADR-0035). Both operands are borrowed, not consumed, and
remain usable afterwards.

{{ rule(id="3.7:26", cat="legality-rule") }}

The `+` operator requires both operands to have the same type. Mixing a `StrBuf`
and an integer (for example `s + 1`) is a type error; there is no implicit
conversion between `StrBuf` and integers.

{{ rule(id="3.7:27") }}

```rue
const std = @import("std");
const StrBuf = std.strbuf.StrBuf;

fn main() -> i32 {
    let left: StrBuf = "Hello, ";
    let right: StrBuf = "world!";
    let greeting = left + right;
    @dbg(greeting);   // Hello, world!
    0
}
```

## Output

{{ rule(id="3.7:35", cat="normative") }}

The free function `print(s)` takes any text rung and writes its raw bytes to
standard output, adding nothing. Unlike `@dbg`, it does not append a newline and
does not apply any debug formatting. The argument `s` is borrowed, not consumed,
and remains usable afterwards. This internal non-consuming access does not
change the call syntax: the source argument is unmarked (`print(s)`, not
`print(borrow s)`).

{{ rule(id="3.7:36", cat="normative") }}

The free function `println(s)` takes any text rung and writes its raw bytes to
standard output followed by a single newline (`U+000A`). The argument `s` is
borrowed, not consumed. Together with `@to_string` and `+`, `println` composes
line-oriented output; there is no formatting or interpolation syntax. As with
`print`, the internal borrow does not add a source-level argument mode: the
argument is unmarked.

{{ rule(id="3.7:37", cat="dynamic-semantics") }}

`print(s)` and `println(s)` write exactly the bytes of the text value `s`, in
order, without transformation. The output is byte-for-byte identical to the
text's contents (the only difference between the two is the single trailing
newline `println` adds). Writing an empty text value writes no bytes (for
`print`) or a lone newline (for `println`).

{{ rule(id="3.7:38") }}

```rue
const std = @import("std");

fn main() -> i32 {
    print("hello");                       // hello
    print(" world");                      // hello world   (no newline yet)
    println("");                          // hello world\n
    let prefix: std.strbuf.StrBuf = "value is ";
    println(prefix + @to_string(42));       // value is 42\n
    0
}
```

## Search

{{ rule(id="3.7:28", cat="normative") }}

The method `s.contains(borrow needle)` returns `true` if and only if the bytes of the
text view `needle` occur as a contiguous subsequence of the bytes of `s`. The
comparison is byte-level and does not inspect UTF-8 character boundaries. The
empty needle is contained in every string. The receiver `s` is borrowed, not
consumed.

{{ rule(id="3.7:29", cat="normative") }}

The method `s.starts_with(borrow prefix)` returns `true` if and only if the bytes of
the text view `prefix` are a prefix of the bytes of `s`. The comparison is
byte-level. The empty prefix matches every string. The receiver `s` is borrowed,
not consumed.

{{ rule(id="3.7:30") }}

```rue
const std = @import("std");
const StrBuf = std.strbuf.StrBuf;

fn main() -> i32 {
    let h: StrBuf = "hello";
    let needle: StrBuf = "ell";
    let prefix: StrBuf = "he";
    let other: StrBuf = "lo";
    @dbg(h.contains(borrow needle));      // true
    @dbg(h.starts_with(borrow prefix));   // true
    @dbg(h.starts_with(borrow other));    // false
    0
}
```

## Character Iteration

{{ rule(id="3.7:31", cat="normative") }}

The character view `s.chars()` yields the Unicode scalar values of a `StrBuf`,
decoding its bytes as UTF-8. It is used as the iterable of a `for` loop (see
[Loop Expressions](@/04-expressions/08-loop-expressions.md)), which binds each
scalar value as a `u32` in ascending byte order.

{{ rule(id="3.7:32", cat="dynamic-semantics") }}

Decoding through `s.chars()` is strict: a byte sequence that is not well-formed
UTF-8 (an ill-formed, truncated, overlong, or surrogate sequence) traps at
runtime when it is decoded. Because a `StrBuf` is a byte string that may hold
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
const std = @import("std");
const StrBuf = std.strbuf.StrBuf;

fn main() -> i32 {
    let s: StrBuf = "café";
    let mut count = 0;
    for c in s.chars() {
        @dbg(c);          // 99, 97, 102, 233 (the last is é = U+00E9)
        count = count + 1;
    }
    count  // 4 scalar values (though the string is 5 bytes)
}
```

## The `str` Type


{{ rule(id="3.7:43", cat="normative") }}

The type `str` is the byte-string slice type: it is `[u8]` (a read-only slice of
bytes) carrying the same byte-string convention as `StrBuf` (§3.7:15) — its
contents are conventionally UTF-8 but are not required to be valid UTF-8. A `str`
value is a two-word view `{ptr, len}`: a pointer to the bytes and a byte length.

{{ rule(id="3.7:44", cat="normative") }}

A string literal has type `str` unless an expected string-buffer type
contextualizes it as `StrBuf` or `Str(N)`. A `str` literal is *static-backed* and
*first-class*: its bytes reside in read-only data that cannot dangle, so the
`str` value is `@copy`, storable in a binding or a struct field, reassignable,
returnable from a function, and passable as an argument. A first-class `str`
value originates only from a string literal or from another first-class `str`;
in particular a string *buffer* (`StrBuf` or `Str(N)`, §3.7:49) and a borrowed
`str` *view* (§3.7:59) are never themselves first-class `str` values.

{{ rule(id="3.7:45", cat="normative") }}

For a `str` value `s`, `s.len()` evaluates to the length of `s` in bytes as a
`u64`. The operation is `O(1)`.

{{ rule(id="3.7:46", cat="normative") }}

Indexing a `str` with an integer, `s[i]`, evaluates to the byte at byte offset
`i` as a value of type `u8`. Like `StrBuf` byte access (§3.7:16) it operates on
the raw bytes and never inspects UTF-8 character boundaries. The operation is
`O(1)`.

{{ rule(id="3.7:47", cat="dynamic-semantics") }}

If the index `i` is greater than or equal to `s.len()`, evaluating `s[i]` traps
(index out of bounds), terminating the program the same way an out-of-bounds
array or `StrBuf` index does.

{{ rule(id="3.7:48") }}

```rue
fn describe(s: str) -> u8 {
    s[0]              // first byte
}

fn main() -> i32 {
    let s: str = "hello";
    @dbg(s.len());    // 5
    @dbg(s[0]);       // 104 ('h')
    @dbg(describe("hi"));  // 104
    0
}
```

### First-class `str` versus borrowed views

{{ rule(id="3.7:58", cat="normative") }}

Rue distinguishes two string capabilities structurally, with no provenance
tracking (ADR-0043): a *first-class* `str` (§3.7:44) — a static-backed value that
may be copied, stored, returned, and rebound — and a second-class *view* spelled
`borrow str` (shared) or `inout str` (exclusive). A view is a fat pointer over a
buffer's bytes that is valid only in argument position; it may be read (`.len()`,
byte indexing) or re-borrowed, but it cannot escape the call by being returned,
stored in a struct field, or bound past its argument scope.

The binding of an `inout str` view cannot be reassigned as a whole value
(E0210). Whole assignment would rebind the view header rather than mutate the
caller-owned bytes, and the caller's concrete `StrBuf` or `Str(N)` storage need
not have the representation of the value being assigned. Exclusive byte-level
mutation, when provided by the string interface, is distinct from rebinding the
second-class view itself.

{{ rule(id="3.7:59", cat="legality-rule") }}

A string *buffer* — `StrBuf` or `Str(N)` — or a borrowed `str` view used where a
*first-class* `str` value is required (a bare `str` parameter argument, a `str`
binding, a `str` return value, or a `str` struct field) is a compile-time error:
a buffer's bytes live in caller-owned local or heap storage and a view aliases a
borrow's scope, so either escaping as a first-class `str` would dangle once the
storage is released. Passing a buffer is reported as E0495 (a bare `str`
parameter suggests `borrow str`); laundering a view is reported as E0497.

{{ rule(id="3.7:60", cat="normative") }}

A `StrBuf` or `Str(N)` value coerces only to a `borrow str` or `inout str` view,
never to a first-class `str`. An `inout str` view — an *exclusive* view — further
requires *local* provenance: its operand must be a `StrBuf`/`Str(N)` buffer the
caller owns. A first-class or static-backed `str` value is not a legal `inout
str` operand (E0496), because its bytes are immutable read-only data and, being
`@copy`, one static buffer could be reached through two roots that per-root
exclusivity cannot see.

## The `Str(N)` Type


{{ rule(id="3.7:49", cat="normative") }}

The type `Str(N)` is the fixed-capacity string type: it is `[u8; N]` (an inline
buffer of `N` bytes, with no heap allocation) carrying the same byte-string
convention as `StrBuf` (§3.7:15), plus a byte length. The capacity `N` is a
compile-time constant (an integer literal or a `const`), so `Str(N)` is a
value type parameterized by `N`, the string analogue of the fixed array
`[T; N]`. A `Str(N)` value stores up to `N` bytes together with its current
byte length.

{{ rule(id="3.7:50", cat="normative") }}

Where a `Str(N)` is expected, a string literal whose UTF-8 byte length is at
most `N` has type `Str(N)`. Such a value is `@copy`, storable in a binding or a
struct field, reassignable, returnable from a function, and passable as an
argument.

{{ rule(id="3.7:51", cat="legality-rule") }}

Constructing a `Str(N)` from a string literal whose UTF-8 byte length exceeds
`N` is a compile-time error (E0492). Because `Str(N)` has a fixed capacity and
no heap, an over-long literal cannot be stored, and the fit is checked at
compile time.

{{ rule(id="3.7:52", cat="normative") }}

For a `Str(N)` value `s`, `s.len()` evaluates to the current byte length of `s`
as a `u64`, which is at most `N`. The operation is `O(1)`.

{{ rule(id="3.7:53", cat="normative") }}

Indexing a `Str(N)` with an integer, `s[i]`, evaluates to the byte at byte
offset `i` as a value of type `u8`. Like `StrBuf` byte access (§3.7:16) it
operates on the raw bytes and never inspects UTF-8 character boundaries. The
operation is `O(1)`.

{{ rule(id="3.7:54", cat="dynamic-semantics") }}

If the index `i` is greater than or equal to `s.len()`, evaluating `s[i]` traps
(index out of bounds), terminating the program the same way an out-of-bounds
array, `StrBuf`, or `str` index does.

{{ rule(id="3.7:55", cat="normative") }}

A `Str(N)` value is readable through a borrowed `str` view (§3.7:60): it coerces
to a `borrow str` (or `inout str`) over its bytes, so `borrow s` may be passed to
a `borrow str` parameter and read through the `str` interface (`.len()`, byte
indexing). This makes `borrow str` the universal read interface over the fixed
(`Str(N)`), growable (`StrBuf`), and static-literal string rungs. A `Str(N)` does
*not* coerce to a first-class `str` (§3.7:59): a bare `str` parameter would let
the buffer's bytes escape, so `borrow str` is used instead.

{{ rule(id="3.7:56") }}

```rue
fn len_of(borrow s: str) -> u64 {
    s.len()               // read a Str(N) through the borrowed str view
}

fn main() -> i32 {
    let s: Str(8) = "hello";   // fits in 8 bytes
    @dbg(s.len());             // 5
    @dbg(s[0]);                // 104 ('h')
    @dbg(len_of(borrow s));    // 5   (borrowed as `borrow str`)
    // let bad: Str(3) = "hello";  // ERROR (E0492): 5 bytes exceed capacity 3
    // let x: str = s;             // ERROR (E0495): a buffer is not a first-class str
    0
}
```

## Limitations

{{ rule(id="3.7:14", cat="informative") }}

The current implementation does not support:
- Slicing with range syntax (`s[a..b]`); use `s.substring(start, len)` instead
- Pattern matching on strings

These features may be added in future versions.
