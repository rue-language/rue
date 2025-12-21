+++
title = "Rue"
template = "index.html"

[extra]
tagline = "Higher level than Rust, lower level than Go"

[[extra.features]]
title = "Memory Safe"
description = "No garbage collector, no manual memory management. A work in progress, though."

[[extra.features]]
title = "Simple Syntax"
description = "Familiar syntax inspired by various programming languages. If you know one, you'll feel at home with Rue."

[[extra.features]]
title = "Fast Compilation"
description = "Direct compilation to native code."
+++

```rust
// It's a classic for a reason
fn fib(n: i32) -> i32 {
    if n <= 1 {
        n
    } else {
        fib(n - 1) + fib(n - 2)
    }
}

fn main() -> i32 {
    // Print the first 20 Fibonacci numbers
    let mut i = 0;
    while i < 20 {
        @dbg(fib(i));
        i = i + 1;
    }

    // Return fib(10) = 55
    fib(10)
}
```
