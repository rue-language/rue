+++
title = "What's Next"
weight = 16
template = "tutorial/page.html"
+++

# What's Next

You have seen the core of Rue as it exists today. This page is about the edges:
what the language does not have yet, where the current implementation is rough,
and where to go from here.

## What Rue does not have yet

Rue is a young language, and several things you may expect are missing on
purpose, not yet designed, or in progress. Knowing the list saves you from
looking for them.

- **Traits or interfaces.** There is no way to say "any type with a `less`
  method". Generic code is written with `comptime` type parameters, and each
  instantiation is checked separately. Interfaces are the next major language
  feature under design.
- **Closures and function values.** You cannot pass a function as an
  argument or store one in a struct. The design direction is second-class
  function parameters, in the same spirit as slices.
- **Format strings.** Output is `+` and `@to_string`. A compile-time
  formatting facility is possible under Rue's rules (it would run on a string
  literal, not on syntax) but does not exist.
- **A prelude.** `Option`, `StrBuf`, and `ArrayBuf` must be imported and
  named. This is a deliberate stance, though the ceremony may shrink.
- **Iteration over `ArrayBuf`.** `for` walks fixed arrays and strings. For a
  buffer, index over `len()`.
- **Slices of narrow elements.** `[T]` currently works only when `T` is 64
  bits wide.
- **Package management.** A program is one root file and its imports. There
  is no package registry or dependency resolution.
- **Concurrency.** None. Rue programs are single-threaded, which is also why
  its exclusivity rules need no runtime checks.
- **Exceptions or `catch`.** Never, by design. Failures are values or traps.
- **Implicit conversions, overloading, macros.** Never, by design.

There is also `unchecked` code with raw pointers, which the standard library
uses to implement `StrBuf` and `ArrayBuf`. This tutorial does not cover it; the
specification's chapter on unchecked code does.

## Rough edges

Expect to hit compiler bugs and unhelpful diagnostics in places. When you do,
the [issue tracker](https://github.com/rue-language/rue/issues) is the right
place for them, and a small program that reproduces the problem is the most
useful thing you can include. The programs in this tutorial are all
compiled, and most are run, by the project's test suite, so if one of them
fails for you, that is a bug worth reporting on its own.

## Where to go

- The [language specification](/spec/) is the authoritative definition of
  everything this tutorial taught operationally. When the tutorial and the
  specification disagree, the specification wins, and the disagreement is a
  bug in the tutorial.
- The repository's `examples/` directory holds larger programs: a Pratt-parsing
  calculator across modules, a word-frequency counter, a JSON formatter, a
  tiny database, a static site generator, and more. They are the best picture
  of what idiomatic Rue looks like at a few hundred lines.
- The standard library in `std/` is written in Rue. Reading `std/arraybuf.rue`
  shows how a generic collection is built from `comptime` and `unchecked`
  primitives.
- The design records in `docs/designs/` explain why the language is shaped
  the way it is, decision by decision.

Rue is developed in public, largely by AI agents working under a human
maintainer, and its design is being tuned for readers who see one function at
a time. If that experiment interests you, the repository's blog and
`CONTRIBUTING.md` are the places to start.

Thanks for reading.
