+++
title = "Rue"
template = "index.html"

[extra]
tagline = "A systems language, grown carefully."
+++

```rue
fn fib(n: i32) -> i32 {
    if n <= 1 {
        n
    } else {
        fib(n - 1) + fib(n - 2)
    }
}

fn main() -> i32 {
    // the first ten Fibonacci numbers
    let mut i = 0;
    while i < 10 {
        @dbg(fib(i));
        i = i + 1;
    }
    0
}
```
