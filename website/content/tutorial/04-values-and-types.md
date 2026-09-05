+++
title = "Values and Types"
weight = 4
template = "tutorial/page.html"
+++

# Values and Types

Rue is statically typed: every value has a type the compiler knows before the
program runs. This chapter covers the basic ones and the two habits you will
use constantly: `let` bindings and building strings for output.

```rue run
const std = @import("std");

fn main() -> i32 {
    let answer: i32 = 42;
    let big: i64 = 1000000000000;
    let byte: u8 = 255;
    let ready = true;

    println("answer = " + @to_string(answer));
    println("big = " + @to_string(big));
    println("byte = " + @to_string(byte));
    if ready {
        println("ready");
    }
    0
}
```

```text
answer = 42
big = 1000000000000
byte = 255
ready
```

## Integers

Rue has signed integers `i8`, `i16`, `i32`, `i64` and unsigned integers `u8`,
`u16`, `u32`, `u64`. The number is the width in bits. Signed integers can be
negative; unsigned ones cannot.

There are no implicit conversions between them, not even from a narrower type
to a wider one. Mixing widths is a compile error, and you convert explicitly
with `@intCast`:

```rue run
const std = @import("std");

fn main() -> i32 {
    let small: i32 = 1000;
    let wide: i64 = @intCast(small);
    println(@to_string(wide * 1000000));
    0
}
```

```text
1000000000
```

`@intCast` checks that the value fits. Converting `300` to a `u8` is a runtime
error, not a silent truncation to `44`.

Arithmetic is checked too. If an operation overflows its type, the program
stops with an error rather than wrapping around:

```rue run exit=101
fn main() -> i32 {
    let mut x: u8 = 250;
    println("adding 10 to 250 as a u8");
    x = x + 10;
    println("unreachable");
    0
}
```

```text
adding 10 to 250 as a u8
```

The program prints the first line, then exits with status `101` and the
message `error: integer overflow` on standard error. Rue calls this a *trap*.
Chapter 11 has more to say about them.

## Booleans

`bool` is `true` or `false`. Comparisons produce booleans, and `if` requires
one. There is no truthiness: an integer is not a boolean.

```rue run
fn main() -> i32 {
    let n = 7;
    let odd = n % 2 == 1;
    if odd {
        println("odd");
    } else {
        println("even");
    }
    0
}
```

```text
odd
```

## Type inference

You rarely need to write types. The compiler infers a binding's type from its
initializer, and an integer literal with no other context becomes an `i32`:

```rue run
const std = @import("std");

fn main() -> i32 {
    let x = 42;       // i32
    let y = true;     // bool
    let z: u64 = 42;  // the annotation picks u64 instead
    println(@to_string(x) + " " + @to_string(z));
    if y {
        println("y");
    }
    0
}
```

```text
42 42
y
```

## Mutability

Bindings are immutable by default. `let mut` makes one assignable:

```rue run
const std = @import("std");

fn main() -> i32 {
    let mut count = 0;
    count = count + 1;
    count += 1;      // compound assignment does the same thing
    println("count = " + @to_string(count));
    0
}
```

```text
count = 2
```

Assigning to a plain `let` is a compile error:

```rue compile-fail E0203
fn main() -> i32 {
    let x = 42;
    x = 43;
    x
}
```

```text
error: [E0203]: cannot assign to immutable variable 'x'
```

## Floats

`f32` and `f64` are IEEE-754 floating point. A literal with a decimal point is a
float, and integers and floats do not mix without a conversion:

```rue run
const std = @import("std");

fn main() -> i32 {
    let half: f64 = 0.5;
    println(@to_string(half * 3.0));
    0
}
```

```text
1.5
```

## Strings and output

You have already used `"..."` literals with `println`. A string literal has the
type `str`: a read-only view of some bytes. To build a string at runtime, for
example to put a number in a message, you need the standard library's growable
string type, `StrBuf`. That is what `@to_string` returns, and `+` joins strings
into a new `StrBuf`:

```rue run
const std = @import("std");

fn main() -> i32 {
    let width = 3;
    let height = 4;
    let message = "area = " + @to_string(width * height);
    println(message);
    0
}
```

```text
area = 12
```

This is why most programs in this tutorial start with
`const std = @import("std");`. Rue has no prelude: nothing from the standard
library is in scope until you import it, and `@to_string` produces a standard
library type. Leave the import out and the compiler tells you exactly that:

```rue compile-fail E0204
fn main() -> i32 {
    println("n = " + @to_string(42));
    0
}
```

```text
error: [E0204]: unknown type 'StrBuf'
```

There are no format strings in Rue. Concatenation with `+` and `@to_string` is
the whole formatting story for now, and it is enough for everything in this
tutorial. Booleans have no `@to_string`; branch on them, or use
`std.fmt.bool_to_string`:

```rue run
const std = @import("std");

fn main() -> i32 {
    let done = false;
    println("done = " + std.fmt.bool_to_string(done));
    0
}
```

```text
done = false
```

Chapter 10 comes back to strings in more depth. For now: literals are `str`,
built strings are `StrBuf`, and `println` accepts either.
