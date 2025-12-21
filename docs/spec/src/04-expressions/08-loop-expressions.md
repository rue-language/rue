# Loop Expressions

## While Loops

r[4.8.1#normative]
A while loop repeatedly executes its body while a condition is true.

r[4.8.2#normative]
```ebnf
while_expr = "while" expression "{" block "}" ;
```

r[4.8.3#normative]
The condition expression must have type `bool`.

r[4.8.4#normative]
A while expression has type `()`.

r[4.8.5#normative]
The condition is evaluated before each iteration. If it is `true`, the body is executed and the condition is re-evaluated. If it is `false`, the loop terminates.

r[4.8.6]
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

r[4.8.15#normative]
An infinite loop repeatedly executes its body unconditionally.

r[4.8.16#normative]
```ebnf
loop_expr = "loop" "{" block "}" ;
```

r[4.8.17#normative]
A loop expression has type `!` (never), because it never terminates normally.

r[4.8.18#normative]
The only way to exit a `loop` is via `break` or `return`.

r[4.8.19]
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

r[4.8.20]
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

r[4.8.7#normative]
The `break` expression exits the innermost enclosing loop.

r[4.8.8#normative]
The `continue` expression skips to the next iteration of the innermost enclosing loop.

r[4.8.9#normative]
Both `break` and `continue` must appear within a loop. Using them outside a loop is a compile-time error.

r[4.8.10#normative]
Both `break` and `continue` have the never type `!`.

r[4.8.11]
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

r[4.8.12]
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

r[4.8.13#normative]
In nested loops, `break` and `continue` affect only the innermost enclosing loop.

r[4.8.14]
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
