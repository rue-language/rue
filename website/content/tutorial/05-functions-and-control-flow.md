+++
title = "Functions and Control Flow"
weight = 5
template = "tutorial/page.html"
+++

# Functions and Control Flow

Rue is expression-oriented: blocks, `if`, and `match` all produce values, and a
function's body is one big expression. This chapter shows functions and the
four control-flow constructs, `if`, `while`, `loop`, and `for`, then uses
`match` for the first time.

```rue run
const std = @import("std");

fn classify(n: i32) -> str {
    if n % 15 == 0 {
        "FizzBuzz"
    } else if n % 3 == 0 {
        "Fizz"
    } else if n % 5 == 0 {
        "Buzz"
    } else {
        "number"
    }
}

fn main() -> i32 {
    let mut i = 1;
    while i <= 15 {
        let label = classify(i);
        if label == "number" {
            println(@to_string(i));
        } else {
            println(label);
        }
        i += 1;
    }
    0
}
```

```text
1
2
Fizz
4
Buzz
Fizz
7
8
Fizz
Buzz
11
Fizz
13
14
FizzBuzz
```

## Functions

A function is declared with `fn`, a parameter list with types, and a return
type after `->`:

```rue run
const std = @import("std");

fn add(a: i32, b: i32) -> i32 {
    a + b
}

fn main() -> i32 {
    println(@to_string(add(3, 4)));
    0
}
```

```text
7
```

The body's final expression, written without a semicolon, is the function's
value. Add a semicolon and it becomes a statement whose value is discarded,
which is a type error in a function that promises an `i32`.

`return` exits early with a value:

```rue run
const std = @import("std");

fn absolute(n: i32) -> i32 {
    if n < 0 {
        return -n;
    }
    n
}

fn main() -> i32 {
    println(@to_string(absolute(-42)));
    println(@to_string(absolute(17)));
    0
}
```

```text
42
17
```

A function with no `-> Type` returns the unit value `()`, which is Rue's way of
saying "nothing":

```rue run
fn greet() {
    println("hello from greet");
}

fn main() -> i32 {
    greet();
    0
}
```

```text
hello from greet
```

Functions can be declared in any order. `main` can call a function defined
below it, and functions can call themselves:

```rue run
const std = @import("std");

fn main() -> i32 {
    println(@to_string(factorial(10)));
    0
}

fn factorial(n: i64) -> i64 {
    if n <= 1 { 1 } else { n * factorial(n - 1) }
}
```

```text
3628800
```

Parameters are immutable inside the function. If you try to assign to one, the
compiler suggests the fix, which chapter 8 explains:

```rue compile-fail E0203
fn bump(x: i32) {
    x = x + 1;
}

fn main() -> i32 {
    bump(1);
    0
}
```

```text
error: [E0203]: cannot assign to immutable variable 'x'
  = help: consider making parameter `x` inout: `inout x: i32`
```

## `if` is an expression

Because `if` produces a value, you can bind it or return it directly:

```rue run
const std = @import("std");

fn max(a: i32, b: i32) -> i32 {
    if a > b { a } else { b }
}

fn main() -> i32 {
    let bigger = max(10, 20);
    let sign = if bigger > 0 { "positive" } else { "not positive" };
    println(@to_string(bigger) + " is " + sign);
    0
}
```

```text
20 is positive
```

Both branches must have the same type, and an `if` used as a value must have
an `else`:

```rue compile-fail E0206
fn main() -> i32 {
    let x = if true { 1 } else { false };
    0
}
```

The condition must be a `bool`. Rue does not treat `0` as false.

## Loops

`while` repeats while a condition holds:

```rue run
const std = @import("std");

fn main() -> i32 {
    let mut sum = 0;
    let mut i = 1;
    while i <= 10 {
        sum += i;
        i += 1;
    }
    println(@to_string(sum));
    0
}
```

```text
55
```

`loop` repeats forever until a `break`. It is the natural shape for "read
until done":

```rue run
const std = @import("std");

fn main() -> i32 {
    let mut n = 27;
    let mut steps = 0;
    loop {
        if n == 1 {
            break;
        }
        n = if n % 2 == 0 { n / 2 } else { 3 * n + 1 };
        steps += 1;
    }
    println("27 reaches 1 in " + @to_string(steps) + " steps");
    0
}
```

```text
27 reaches 1 in 111 steps
```

`continue` skips to the next iteration of the innermost loop.

`for` walks the elements of an array (and, as you will see later, the bytes or
characters of a string):

```rue run
const std = @import("std");

fn main() -> i32 {
    let squares = [1, 4, 9, 16, 25];
    let mut total = 0;
    for x in squares {
        total += x;
    }
    println(@to_string(total));
    0
}
```

```text
55
```

## `match`

`match` compares a value against a list of patterns and runs the first arm that
fits. It is an expression too:

```rue run
const std = @import("std");

fn day_name(day: i32) -> str {
    match day {
        0 => "Sunday",
        6 => "Saturday",
        _ => "a weekday",
    }
}

fn main() -> i32 {
    println(day_name(0));
    println(day_name(3));
    0
}
```

```text
Sunday
a weekday
```

`_` matches anything. A `match` must be *exhaustive*: every possible value of
the scrutinee must hit some arm, which for an integer means you need the `_`
case. `match` really comes into its own with enums, which is the subject of
chapter 7.

## Blocks

Finally, a bare block `{ ... }` is an expression whose value is its last
expression. Bindings inside it are scoped to it:

```rue run
const std = @import("std");

fn main() -> i32 {
    let area = {
        let w = 6;
        let h = 7;
        w * h
    };
    println(@to_string(area));
    0
}
```

```text
42
```

You now have enough to write real logic. Next, a way to group data: structs.
