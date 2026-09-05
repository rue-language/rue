+++
title = "Enums and Matching"
weight = 7
template = "tutorial/page.html"
+++

# Enums and Matching

An enum is a type with a fixed set of *variants*. A value of the type is exactly
one of them, and a variant can carry data. Together with `match`, enums are
how Rue represents "one of several shapes", including the standard library's
`Option`.

```rue run
const std = @import("std");

enum Shape {
    Circle(i32),
    Rectangle(i32, i32),
    Point,
}

fn area(borrow s: Shape) -> i32 {
    match s {
        Shape.Circle(r) => 3 * r * r,
        Shape.Rectangle(w, h) => w * h,
        Shape.Point => 0,
    }
}

fn main() -> i32 {
    let shapes = [Shape.Circle(2), Shape.Rectangle(3, 4), Shape.Point];
    for s in shapes {
        println(@to_string(area(borrow s)));
    }
    0
}
```

```text
12
12
0
```

## Simple enums

The simplest enum is a list of names. Variants are spelled `Type.Variant`:

```rue run
const std = @import("std");

enum Direction {
    North,
    South,
    East,
    West,
}

fn degrees(d: Direction) -> i32 {
    match d {
        Direction.North => 0,
        Direction.East => 90,
        Direction.South => 180,
        Direction.West => 270,
    }
}

fn main() -> i32 {
    println(@to_string(degrees(Direction.West)));
    0
}
```

```text
270
```

## Exhaustiveness

A `match` on an enum must cover every variant. Forget one and the compiler
names it:

```rue compile-fail E0600
enum Direction {
    North,
    South,
    East,
    West,
}

fn degrees(d: Direction) -> i32 {
    match d {
        Direction.North => 0,
        Direction.East => 90,
    }
}

fn main() -> i32 {
    degrees(Direction.North)
}
```

```text
error: [E0600]: match is not exhaustive
  = help: missing variants: South, West
```

This is the feature that makes enums worth using. When you add a variant, every
`match` that fails to handle it stops compiling, and the compiler tells you
where. A `_` arm opts out of that protection for the variants it swallows, so
use it only when the remaining cases really are all the same.

## Variants with data

A variant can carry values, listed in parentheses. Matching on such a variant
binds names to the payload:

```rue run
const std = @import("std");

enum Command {
    Move(i32, i32),
    Say(str),
    Quit,
}

fn describe(borrow c: Command) -> str {
    match c {
        Command.Move(dx, dy) => "move",
        Command.Say(text) => text,
        Command.Quit => "quit",
    }
}

fn main() -> i32 {
    let commands = [Command.Say("hello"), Command.Move(1, -1), Command.Quit];
    for c in commands {
        println(describe(borrow c));
    }
    0
}
```

```text
hello
move
quit
```

Enums are the natural type for state machines:

```rue run
const std = @import("std");

enum Light {
    Red,
    Yellow,
    Green,
}

fn next(current: Light) -> Light {
    match current {
        Light.Red => Light.Green,
        Light.Green => Light.Yellow,
        Light.Yellow => Light.Red,
    }
}

fn seconds(light: Light) -> i32 {
    match light {
        Light.Red => 30,
        Light.Yellow => 5,
        Light.Green => 25,
    }
}

fn main() -> i32 {
    let mut light = Light.Red;
    let mut total = 0;
    let mut i = 0;
    while i < 6 {
        total += seconds(light);
        light = next(light);
        i += 1;
    }
    println("two full cycles take " + @to_string(total) + " seconds");
    0
}
```

```text
two full cycles take 120 seconds
```

## `Option`: a value or nothing

The most important enum in Rue is not built into the language. It is defined
in the standard library:

```rue skip
pub fn Option(comptime T: type) -> type {
    enum {
        Some(T),
        None,
    }
}
```

`Option` is a function that takes a type and returns an enum type. Calling
`std.option.Option(i64)` gives you "an `i64` or nothing". Because the full path
is long, programs usually bind it to a short name with `const`:

```rue run
const std = @import("std");
const OptI64 = std.option.Option(i64);

fn find_first_even(xs: [i64; 5]) -> OptI64 {
    for x in xs {
        if x % 2 == 0 {
            return OptI64.Some(x);
        }
    }
    OptI64.None
}

fn report(found: OptI64) {
    match found {
        OptI64.Some(n) => println("first even: " + @to_string(n)),
        OptI64.None => println("no even numbers"),
    }
}

fn main() -> i32 {
    report(find_first_even([1, 3, 6, 7, 8]));
    report(find_first_even([1, 3, 5, 7, 9]));
    0
}
```

```text
first even: 6
no even numbers
```

`Option` is how Rue says "this might not be there" without null pointers or
sentinel values. A function that returns `Option(T)` cannot be used as if it
returned `T`; the caller has to `match`, and the compiler holds it to that.

## The `?` operator

Matching on every `Option` gets tedious when all you want to do is give up if
it is `None`. The postfix `?` operator does exactly that: in a function that
returns an `Option`, `expr?` produces the `Some` payload, or returns `None`
from the whole function immediately.

```rue run
const std = @import("std");
const OptI64 = std.option.Option(i64);

fn half(n: i64) -> OptI64 {
    if n % 2 == 0 { OptI64.Some(n / 2) } else { OptI64.None }
}

fn quarter(n: i64) -> OptI64 {
    let h = half(n)?;
    half(h)
}

fn main() -> i32 {
    match quarter(12) {
        OptI64.Some(q) => println("quarter of 12 is " + @to_string(q)),
        OptI64.None => println("12 has no quarter"),
    }
    match quarter(6) {
        OptI64.Some(q) => println("quarter of 6 is " + @to_string(q)),
        OptI64.None => println("6 has no quarter"),
    }
    0
}
```

```text
quarter of 12 is 3
6 has no quarter
```

Every `?` is a place the function might return early, and it is visible on the
line. `?` only works inside a function whose return type is an `Option` (or,
as chapter 11 shows, a `Result`):

```rue compile-fail E0503
const std = @import("std");

fn main() -> i32 {
    let line = @read_line()?;
    0
}
```

```text
error: [E0503]: the `?` operator can only be used in a function that returns an `Option` (found return type `i32`)
```

Enums and structs are the data-modelling half of Rue. The next chapter is
about the other half: who owns a value, and who is allowed to touch it.
