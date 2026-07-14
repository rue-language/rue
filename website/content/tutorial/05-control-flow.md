+++
title = "Control Flow"
weight = 5
template = "tutorial/page.html"
+++

# Control Flow

Rue has three main control flow constructs: `if`, `while`, and `match`.

## If Expressions

`if` is an expression, not a statement—it returns a value:

```rue check
const std = @import("std");

fn max(a: i32, b: i32) -> i32 {
    if a > b { a } else { b }
}

fn main() -> i32 {
    let bigger = max(10, 20);
    println(@to_string(bigger));  // prints: 20
    bigger
}
```

Because `if` is an expression, you can use it anywhere a value is expected:

```rue check
const std = @import("std");

fn main() -> i32 {
    let x = 5;
    let description = if x > 0 { 1 } else { 0 };
    println(@to_string(description));  // prints: 1
    0
}
```

Both branches must have the same type.

## While Loops

```rue check
const std = @import("std");

fn main() -> i32 {
    let mut sum = 0;
    let mut i = 1;

    while i <= 10 {
        sum = sum + i;
        i = i + 1;
    }

    println(@to_string(sum));  // prints: 55 (1+2+...+10)
    sum
}
```

## Match Expressions

For multi-way branching, use `match`:

```rue check
const std = @import("std");

fn day_type(day: i32) -> i32 {
    // 0 = weekend, 1 = weekday
    match day {
        0 => 0,  // Sunday
        6 => 0,  // Saturday
        _ => 1,  // Everything else
    }
}

fn main() -> i32 {
    println(@to_string(day_type(0)));  // prints: 0 (weekend)
    println(@to_string(day_type(3)));  // prints: 1 (weekday)
    0
}
```

The `_` is a wildcard that matches anything. Match expressions must be exhaustive—every possible value must be handled.

## Example: FizzBuzz

Here's a classic example combining everything:

```rue check
const std = @import("std");

fn fizzbuzz(n: i32) -> i32 {
    let div_by_3 = n % 3 == 0;
    let div_by_5 = n % 5 == 0;

    if div_by_3 && div_by_5 {
        3  // FizzBuzz
    } else if div_by_3 {
        1  // Fizz
    } else if div_by_5 {
        2  // Buzz
    } else {
        n
    }
}

fn main() -> i32 {
    let mut i = 1;
    while i <= 15 {
        println(@to_string(fizzbuzz(i)));
        i = i + 1;
    }
    0
}
```
