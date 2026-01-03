+++
title = "Hello, World"
weight = 2
template = "tutorial/page.html"
+++

# Hello, World

Let's start with the simplest possible program. Create a file called `hello.rue`:

```rue
fn main() -> i32 {
    0
}
```

Every Rue program needs a `main` function that returns an `i32`. This return value becomes the program's exit code—`0` means success.

## Compiling and Running

Compile and run it:

```bash
./buck2 run //crates/rue:rue -- hello.rue hello
./hello
echo $?  # prints: 0
```

The compiler takes the source file (`hello.rue`) and produces an executable (`hello`).

## Printing Output

To see output, use the `@dbg` intrinsic:

```rue
fn main() -> i32 {
    @dbg(42);
    0
}
```

This prints `42` to the console. The `@` prefix indicates a compiler intrinsic—a built-in operation provided by the compiler.

Run this program and you'll see:

```
42
```

The `@dbg` intrinsic works with any type: integers, booleans, and more. It's your primary debugging tool while developing.
