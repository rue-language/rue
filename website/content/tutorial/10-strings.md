+++
title = "Strings and Text"
weight = 10
template = "tutorial/page.html"
+++

# Strings and Text

Text in Rue follows the same ladder as arrays. A string literal is a `str`, a
read-only view of bytes baked into the program. A `StrBuf` is a growable,
heap-owned string from the standard library. `println` accepts both, `+`
produces a `StrBuf`, and reading input gives you a `StrBuf`.

```rue run
const std = @import("std");
const StrBuf = std.strbuf.StrBuf;

fn shout(borrow s: StrBuf) -> StrBuf {
    let mut out = StrBuf.new();
    for b in s.clone() {
        if b >= 97 && b <= 122 {
            out.push(b - 32);
        } else {
            out.push(b);
        }
    }
    out
}

fn main() -> i32 {
    let greeting: StrBuf = "hello, rue";
    println(shout(borrow greeting));
    println("length: " + @to_string(greeting.len()));
    0
}
```

```text
HELLO, RUE
length: 10
```

## `str` and `StrBuf`

A literal is a `str`. Giving it a `StrBuf` type, or using it where a `StrBuf`
is expected, copies it into a fresh heap buffer:

```rue run
const std = @import("std");
const StrBuf = std.strbuf.StrBuf;

fn main() -> i32 {
    let literal = "static text";           // str
    let owned: StrBuf = "heap text";        // StrBuf, copied from the literal
    let built = "built: " + @to_string(7);  // StrBuf, from concatenation
    println(literal);
    println(owned);
    println(built);
    0
}
```

```text
static text
heap text
built: 7
```

`StrBuf` is a move type with a destructor, like `ArrayBuf`. Passing one by
value gives it away; pass `borrow` to keep it. When a standard-library function
takes a `StrBuf` by value and you want to keep yours, hand it a `clone()`.

## Concatenation

`+` joins strings. Either side may be a `str` or a `StrBuf`, and the result is
a new `StrBuf`. Combined with `@to_string`, this is how every message in this
tutorial is built:

```rue run
const std = @import("std");

fn main() -> i32 {
    let name = "Rue";
    let version = 0;
    println("Hello from " + name + " " + @to_string(version) + "!");
    0
}
```

```text
Hello from Rue 0!
```

To grow a string in place, use `push` for a byte or `push_str` for text:

```rue run
const std = @import("std");
const StrBuf = std.strbuf.StrBuf;

fn main() -> i32 {
    let mut line = StrBuf.new();
    let mut i = 1;
    while i <= 5 {
        if i > 1 {
            line.push_str(", ");
        }
        line.push_str(@to_string(i));
        i += 1;
    }
    println(line);
    0
}
```

```text
1, 2, 3, 4, 5
```

## Bytes and characters

A `StrBuf` is a sequence of bytes, and `for` visits them as `u8`. For ASCII
text that is usually what you want; a byte compared against `48` through `57`
is a digit, `32` is a space, and so on.

```rue run
const std = @import("std");
const StrBuf = std.strbuf.StrBuf;

fn main() -> i32 {
    let text: StrBuf = "a1b22c333";
    let mut digits = 0;
    for b in text {
        if b >= 48 && b <= 57 {
            digits += 1;
        }
    }
    println("digits: " + @to_string(digits));
    0
}
```

```text
digits: 6
```

Rue strings are UTF-8. To visit Unicode characters rather than bytes, iterate
`s.chars()`, which yields each scalar value as a `u32`:

```rue run
const std = @import("std");
const StrBuf = std.strbuf.StrBuf;

fn main() -> i32 {
    let word: StrBuf = "héllo";
    let mut bytes = 0;
    let mut chars = 0;
    for _ in word.clone() {
        bytes += 1;
    }
    for _ in word.chars() {
        chars += 1;
    }
    println(@to_string(bytes) + " bytes, " + @to_string(chars) + " characters");
    0
}
```

```text
6 bytes, 5 characters
```

`chars()` traps if the bytes are not valid UTF-8; `chars_lossy()` substitutes
the replacement character instead.

## Reading input

`@read_line()` reads one line from standard input and returns
`Option(StrBuf)`: `Some(line)` without the trailing newline, or `None` at end
of input. That makes "process every line" a `loop` with a `match`:

```rue run stdin="one\ntwo\nthree\n"
const std = @import("std");
const StrBuf = std.strbuf.StrBuf;
const OptLine = std.option.Option(StrBuf);

fn main() -> i32 {
    let mut count = 0;
    loop {
        let line = match @read_line() {
            OptLine.Some(l) => l,
            OptLine.None => break,
        };
        count += 1;
        println(@to_string(count) + ": " + line);
    }
    println("read " + @to_string(count) + " lines");
    0
}
```

```bash
printf 'one\ntwo\nthree\n' | scripts/rue exec lines.rue
```

```text
1: one
2: two
3: three
read 3 lines
```

## Parsing numbers

`@parse_i64` turns a `StrBuf` into `Option(i64)`: `Some` for a well-formed
integer, `None` for anything else. There is no exception to catch and no
magic zero to check for.

```rue run stdin="12\nforty\n30\n"
const std = @import("std");
const StrBuf = std.strbuf.StrBuf;
const OptLine = std.option.Option(StrBuf);
const OptI64 = std.option.Option(i64);

fn main() -> i32 {
    let mut sum: i64 = 0;
    loop {
        let line = match @read_line() {
            OptLine.Some(l) => l,
            OptLine.None => break,
        };
        match @parse_i64(line) {
            OptI64.Some(n) => {
                sum += n;
            },
            OptI64.None => println("skipping a line that is not a number"),
        }
    }
    println("sum = " + @to_string(sum));
    0
}
```

```text
skipping a line that is not a number
sum = 42
```

## Helpers in `std.strings`

`std.strings` collects the usual text utilities: `trim`, `split`, `lines`,
`find`, `replace`, `join`, `starts_with_byte`, `repeat`, and more. `StrBuf`
itself has `len`, `is_empty`, `starts_with`, `ends_with`, `contains`,
`substring`, and `find_str`.

```rue run
const std = @import("std");
const StrBuf = std.strbuf.StrBuf;

fn main() -> i32 {
    let padded: StrBuf = "   rue   ";
    let trimmed = std.strings.trim(padded.clone());
    println("[" + trimmed + "]");

    let path: StrBuf = "docs/spec/index.md";
    if path.ends_with(borrow ".md") {
        println("markdown");
    }
    println(std.strings.replace(borrow path, borrow "/", borrow "::"));
    println(std.strings.repeat("ab", 3));
    0
}
```

```text
[rue]
markdown
docs::spec::index.md
ababab
```

Functions that only read a string take it `borrow`, and the call site says so
even when the argument is a literal. Functions that take a `StrBuf` by value,
like `trim` and `repeat`, consume it, which is why `padded` is cloned first.

These are ordinary Rue functions in `std/strings.rue` and `std/strbuf.rue`,
and reading them is a good way to see idiomatic Rue. The next chapter turns
to what happens when things go wrong.
