+++
title = "Hello, World"
weight = 3
template = "tutorial/page.html"
+++

# Hello, World

Create a file called `hello.rue`:

```rue run
fn main() -> i32 {
    println("Hello, Rue!");
    0
}
```

Run it:

```bash
scripts/rue exec hello.rue
```

```text
Hello, Rue!
```

Three things are happening here.

**`main` is the entry point.** Every Rue program has a function called `main`
that takes no arguments and returns an `i32`.

**The return value is the exit status.** The last expression in a function
body, written without a semicolon, is the function's value. Here that is `0`,
which becomes the process's exit status. Zero means success. Let's check:

```bash
scripts/rue exec hello.rue
echo $?
```

```text
Hello, Rue!
0
```

**`println` prints a line.** It writes its argument followed by a newline to
standard output. `print` does the same without the newline:

```rue run
fn main() -> i32 {
    print("Hello, ");
    print("Rue!");
    println("");
    0
}
```

```text
Hello, Rue!
```

## Exit codes are values

Because `main` returns an ordinary integer, a program can report a result
through its exit status. This program returns 3:

```rue run exit=3
fn main() -> i32 {
    println("about to exit with 3");
    3
}
```

```text
about to exit with 3
```

```bash
scripts/rue exec three.rue; echo $?
```

You will see the message and then `3`. Several of the repository's examples
work this way. It is handy in scripts, but note that a shell treats any nonzero
status as failure, so the programs in this tutorial return `0` unless the exit
status is the point.

## Comments

Comments start with `//` and run to the end of the line:

```rue run
fn main() -> i32 {
    // This is a comment.
    println("comments are ignored"); // so is this
    0
}
```

```text
comments are ignored
```

## Compiling by hand

`scripts/rue exec` is convenient, but it throws the executable away. To keep
it, use the compiler directly (with `RUE` and `RUE_STD_PATH` set up as in the
previous chapter):

```bash
"$RUE" hello.rue -o hello
./hello
```

The compiler takes one source file and an output path. Larger programs are
made of several files, but you still name only one: the root. Chapter 12
explains how the rest are found.

## A debugging aid

`println` prints text. When you just want to see a number while you are
developing, the `@dbg` intrinsic prints any integer or boolean and a newline:

```rue run
fn main() -> i32 {
    @dbg(42);
    @dbg(true);
    0
}
```

```text
42
true
```

The `@` prefix marks a *compiler intrinsic*: an operation the compiler
provides directly rather than a function defined in some library. You will meet
a few more. Use `println` for anything a user should see, and `@dbg` when you
are poking at a value.

Next: values, types, and how to turn a number into something `println` can
print.
