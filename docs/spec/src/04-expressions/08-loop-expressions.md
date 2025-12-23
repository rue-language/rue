+++
title = "Loop Expressions"
weight = 8
template = "spec/page.html"
+++

# Loop Expressions

## While Loops

{{ rule(id="4.8:1", cat="normative") }}

A while loop repeatedly executes its body while a condition is true.

{{ rule(id="4.8:2", cat="normative") }}

```ebnf
while_expr = "while" expression "{" block "}" ;
```

{{ rule(id="4.8:3", cat="normative") }}

The condition expression must have type `bool`.

{{ rule(id="4.8:4", cat="normative") }}

A while expression has type `()`.

{{ rule(id="4.8:5", cat="normative") }}

The condition is evaluated before each iteration. If it is `true`, the body is executed and the condition is re-evaluated. If it is `false`, the loop terminates.

{{ rule(id="4.8:6") }}

```rue
fn main() -> i32 {
    let mut sum = 0;
    let mut i = 1;
    while i <= 10 {
        sum = sum + i;
        i = i + 1;
    }
    sum  // 55
}
```

## Infinite Loops

{{ rule(id="4.8:15", cat="normative") }}

An infinite loop repeatedly executes its body unconditionally.

{{ rule(id="4.8:16", cat="normative") }}

```ebnf
loop_expr = "loop" "{" block "}" ;
```

{{ rule(id="4.8:17", cat="normative") }}

A loop expression has type `!` (never), because it never produces a value. Even when `break` is present, the loop expression itself does not yield a result.

{{ rule(id="4.8:18", cat="normative") }}

The only way to exit a `loop` is via `break` or `return`.

{{ rule(id="4.8:19") }}

```rue
fn main() -> i32 {
    let mut x = 0;
    loop {
        x = x + 1;
        if x == 5 {
            break;
        }
    }
    x  // 5
}
```

{{ rule(id="4.8:20") }}

The `loop` expression is preferred over `while true` for infinite loops:

```rue
// Preferred
loop {
    // ...
}

// Also valid, but less idiomatic
while true {
    // ...
}
```

## Break and Continue

{{ rule(id="4.8:7", cat="normative") }}

The `break` expression exits the innermost enclosing loop.

{{ rule(id="4.8:8", cat="normative") }}

The `continue` expression skips to the next iteration of the innermost enclosing loop.

{{ rule(id="4.8:9", cat="normative") }}

Both `break` and `continue` must appear within a loop. Using them outside a loop is a compile-time error.

{{ rule(id="4.8:10", cat="normative") }}

Both `break` and `continue` have the never type `!`.

{{ rule(id="4.8:21", cat="normative") }}

Currently, `break` does not carry a value. A `loop` expression has type `!` regardless of whether `break` is reachable, because the loop itself does not produce a value.

{{ rule(id="4.8:11") }}

```rue
fn main() -> i32 {
    let mut x = 0;
    while true {
        x = x + 1;
        if x == 5 {
            break;
        }
    }
    x  // 5
}
```

{{ rule(id="4.8:12") }}

```rue
fn main() -> i32 {
    let mut sum = 0;
    let mut i = 0;
    while i < 10 {
        i = i + 1;
        if i % 2 == 0 {
            continue;  // skip even numbers
        }
        sum = sum + i;
    }
    sum  // 25 (1+3+5+7+9)
}
```

## Nested Loops

{{ rule(id="4.8:13", cat="normative") }}

In nested loops, `break` and `continue` affect only the innermost enclosing loop.

{{ rule(id="4.8:14") }}

```rue
fn main() -> i32 {
    let mut total = 0;
    let mut outer = 0;
    while outer < 3 {
        let mut inner = 0;
        while true {
            inner = inner + 1;
            total = total + 1;
            if inner == 2 {
                break;  // exits inner loop only
            }
        }
        outer = outer + 1;
    }
    total  // 6
}
```
