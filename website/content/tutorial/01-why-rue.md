+++
title = "Why Rue"
weight = 1
template = "tutorial/page.html"
+++

# Why Rue

Rue is a systems programming language. It compiles straight to native code, has
no garbage collector, and aims for memory safety without the lifetime
annotations that make Rust hard to learn. Its syntax will look familiar if you
have seen Rust; its ownership model owes more to Swift and Hylo.

This chapter is about the ideas behind the language. You do not need to
understand all of it before writing code, and nothing here is required to
follow the later chapters. But most of Rue's design follows from a few
principles, and knowing them makes the rest of the language feel less like a
list of rules and more like a set of consequences.

## Locality of reasoning

The organizing idea is that **everything you need to understand a line of code
should be visible on that line, or in the signature of what it calls**. When you
read a function, you should not have to trace through the rest of the program
to know what it does to your data.

Here is what that looks like in practice. Read this call without looking at the
function it calls:

```rue skip
sort(inout scores);
total(borrow scores);
```

You already know that `sort` changes `scores` and `total` only reads it. The
call site says so, and the compiler makes sure it is telling the truth. A
function that wants to mutate its argument must declare the parameter `inout`,
and every caller must write `inout` too. Compare a language where mutation is
invisible at the call:

```python
values.sort()      # does this change values? you have to know
```

The same principle shows up all over the language:

- **Failure is visible.** A function that can fail returns an `Option` or a
  `Result`. The `?` operator marks every place a function might return early.
  There are no exceptions, so a call can never jump out of your function
  without a mark on the line.
- **Nothing converts silently.** An `i32` never becomes an `i64` on its own; you
  write `@intCast`. An integer never becomes a string; you write `@to_string`.
  Every conversion is a named operation you can see.
- **No overloading, no macros.** One name means one function. Code that looks
  like a call is a call. Nothing runs on your source text before the compiler
  reads it, so the names in a file are exactly the names you wrote.
- **No hidden names.** There is no prelude. The standard library is a module you
  import explicitly, and everything you use from it is spelled with its module
  path. If a name is in scope, you can find the line that put it there.
- **Order does not matter.** A program means the same thing regardless of the
  order of its declarations or its imports. You can use a function before the
  line that defines it.

Rue's diagnostics follow the same rule. When the compiler rejects a program, it
tries to name the fix at the site of the problem, because the fix usually is at
that site:

```text
error: [E0205]: use of moved value 'p'
  = help: to use `p` after the move, pass it by borrow instead: `borrow p`
```

This matters for people, and it matters for tools that write code. Rue is
developed largely with AI coding agents, and a language where every fact is
local is one where a reader who sees a single function at a time, whether a
person or a model, can still get it right.

## Ownership without lifetimes

Rue values are **owned**. A struct lives in exactly one place, and passing it to
a function or assigning it to a new name *moves* it: the old name is no longer
usable. Types that are just data can opt in to being copied instead with the
`@copy` attribute. Types that own something, like a heap buffer or a file, get a
destructor that runs exactly once, when the owner goes out of scope.

To use a value without giving it away, you lend it. A `borrow` lends read
access for the duration of a call; `inout` lends write access. The rule that
keeps this safe is simple: **at any moment, a value has either any number of
readers or exactly one writer**. The compiler checks it entirely at compile
time, with no runtime cost.

What makes this simpler than Rust is that loans are *second-class*: a borrow
lives only for the call it is passed to. You cannot store one in a struct,
return one from a function, or keep one in a variable. Because a loan can never
outlive the call that created it, the compiler always sees both ends of every
borrow, and there is nothing for a lifetime annotation to say. The same idea
gives Rue **slices** that need no lifetimes: a `[T]` parameter is a view of
some array for the duration of one call, and the type system does not let it
escape.

You give up some things for this. You cannot build a linked list of borrowed
nodes, or return an iterator that borrows a collection. Rue's bet is that
most programs do not need to, and that the ones that do are better served by
indices, owned values, and explicit copies than by a lifetime system.

## Errors are values, bugs are traps

Rue draws a hard line between two kinds of failure.

A failure the program can reasonably handle, like a line that is not a number
or a file that does not exist, is a **value**. The function returns `Option` or
`Result`, the caller matches on it or passes it up with `?`, and the type
system makes sure nobody forgets.

A failure that means the program is wrong, like an integer overflow, an
out-of-bounds index, or a division by zero, is a **trap**. The program stops
immediately with an error message and a nonzero exit status. It does not wrap
around, it does not read garbage, and it does not unwind into a handler that
might be in an inconsistent state. There is no `catch`. If you want a
recoverable failure, return a value.

This is stricter than C or Rust in release mode, where arithmetic overflow
silently wraps. Rue's position is that a wrong answer is worse than a stopped
program, and that the check is cheap enough to always pay.

## Explicit over clever

Rue prefers a small number of visible mechanisms to a large number of implicit
ones.

- **Generics are functions that return types.** `ArrayBuf(i32)` is an ordinary
  call, evaluated at compile time, that returns a struct type. There is no
  separate generics syntax to learn, and no trait system yet.
- **Formatting is concatenation.** There are no format strings. You build a
  message with `+` and `@to_string`, which is more typing and much less
  machinery.
- **Tests are part of the language.** A `test "name" { ... }` block sits next
  to the code it tests, is checked by the same compiler, and runs with
  `rue test`. There is no test framework to choose.
- **Programs are deterministic.** Two runs of the same program on the same input
  produce the same output. The standard library never exposes an iteration
  order that depends on hashing or memory addresses.

## Where things stand

Rue is a young language, and the honest status is that these principles are
ahead of the implementation in places. Some of the points above are stated
guarantees today; others are the direction the design is being checked against,
recorded in the project's
[design records](https://github.com/rue-language/rue/tree/trunk/docs/designs)
and issue tracker. The tutorial says which is which whenever it matters, and the
[last chapter](@/tutorial/16-whats-next.md) lists what is missing.

The compiler itself is a complete pipeline written in Rust, from lexer to
machine-code emitter, with no dependency on LLVM. It targets x86-64 Linux,
AArch64 Linux, and AArch64 macOS.

Now let's build it.
