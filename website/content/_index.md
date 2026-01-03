+++
title = "Rue"
template = "index.html"

[extra]
tagline = "Exploring memory safety that's easier to use"

[[extra.features]]
title = "Early Stage"
description = "Rue is a research project, not ready for real use. We're still building the basics. Expect bugs, missing features, and breaking changes."

[[extra.features]]
title = "Familiar Syntax"
description = "If you know Rust, Go, or C, you'll feel at home. Rue aims for a gentle learning curve without sacrificing clarity."

[[extra.features]]
title = "Native Compilation"
description = "Compiles to x86-64 and ARM64 machine code. No VM, no interpreter, no garbage collector."
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
    // Print the first 10 Fibonacci numbers
    let mut i = 0;
    while i < 10 {
        @dbg(fib(i));
        i = i + 1;
    }
    0
}
```
