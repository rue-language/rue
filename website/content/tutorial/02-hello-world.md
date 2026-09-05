+++
title = "Hello, World"
weight = 2
template = "tutorial/page.html"
+++

# Hello, World

Let's start with the simplest possible program. Create a file called `hello.rue`:

```rue check
fn main() -> i32 {
    0
}
```

Every Rue program needs a `main` function that returns an `i32`. This return value becomes the program's exit code—`0` means success.

## Compiling and Running

Compile and run it:

```bash
RUE="$(scripts/rue-bin)"
export RUE_STD_PATH="$PWD/std"
"$RUE" hello.rue -o hello
./hello
echo $?  # prints: 0
```

The compiler takes the source file (`hello.rue`) and produces an executable
(`hello`). `RUE_STD_PATH` tells the compiler where the standard library lives;
without it, any program that imports `std` (which starts in the next chapter)
fails with `standard library not found`.

For quick experiments, you can also let the repository wrapper build the
compiler, compile your file to a temporary executable, and run it:

```bash
scripts/rue exec hello.rue
echo $?  # prints: 0
```

## Printing Output

To print user-facing text, use `println`:

```rue check
fn main() -> i32 {
    println("Hello, Rue!");
    0
}
```

Run this program and you'll see:

```
Hello, Rue!
```

`println` writes a string followed by a newline. Use `print` when you do not
want the trailing newline.

For quick debugging, Rue also provides the `@dbg` intrinsic:

```rue check
fn main() -> i32 {
    @dbg(42);
    0
}
```

This prints the value followed by a newline:

```
42
```

The `@` prefix indicates a compiler intrinsic—a built-in operation provided by
the compiler. Use `println` for normal program output and `@dbg` when you want
to inspect a value while developing.
