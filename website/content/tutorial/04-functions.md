+++
title = "Functions"
weight = 4
template = "tutorial/page.html"
+++

# Functions

Functions are declared with `fn`, followed by parameters and a return type.

## Basic Functions

```rue check
const std = @import("std");

fn add(a: i32, b: i32) -> i32 {
    a + b
}

fn is_positive(n: i32) -> bool {
    n > 0
}

fn main() -> i32 {
    let sum = add(3, 4);
    println("sum = " + @to_string(sum));

    if is_positive(sum) {
        println("sum is positive");
    }
    if is_positive(-5) {
        println("-5 is positive");
    } else {
        println("-5 is not positive");
    }

    sum
}
```

This prints:

```
sum = 7
sum is positive
-5 is not positive
```

## Function Body Values

Rue is expression-oriented: a function body evaluates to the value of its final
expression. That value is the function call's result:

```rue skip
fn double(x: i32) -> i32 {
    x * 2  // this is the body expression's value
}
```

Note the lack of a semicolon. Adding one would make it a statement instead of an expression.

## Explicit Returns

You can use `return` for early exits:

```rue check
const std = @import("std");

fn absolute(n: i32) -> i32 {
    if n < 0 {
        return -n;
    }
    n
}

fn main() -> i32 {
    println("abs(-42) = " + @to_string(absolute(-42)));
    println("abs(17) = " + @to_string(absolute(17)));
    0
}
```

## Functions Without Return Values

Functions that don't return a meaningful value return the unit type `()`:

```rue check
fn greet() {
    println("hello from greet");
}

fn main() -> i32 {
    greet();
    0
}
```

When there's no `-> Type`, the return type is implicitly `()`.
