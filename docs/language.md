# Rue language overview

Rue is an experimental systems programming language built around mutable value
semantics, memory safety without garbage collection, and direct native-code
generation. Its syntax is currently Rust-like, while its ownership model draws
substantial influence from Hylo and Swift.

This page is an orientation, not a language reference. The authoritative
definition is the [Rue specification](spec/), and executable examples are in
the [specification cases](../crates/rue-spec/cases/).

## Small example

```rue
fn gcd(a: i32, b: i32) -> i32 {
    let mut x = a;
    let mut y = b;
    while y != 0 {
        let next = x % y;
        x = y;
        y = next;
    }
    x
}

fn main() -> i32 {
    @dbg(gcd(84, 30));
    0
}
```

Rue programs are expression-oriented. Functions, blocks, conditionals, loops,
and pattern matching participate in ordinary type checking and control-flow
analysis.

## Implemented language areas

The stable or substantially implemented surface includes:

- signed and unsigned integer types, `f32`/`f64` floating point with IEEE-754
  semantics, booleans, unit, and the never type;
- inference, constants, functions, recursion, methods, and comptime execution;
- blocks, `if`, `match`, `while`, `for`, `break`, `continue`, `return`, and `?`;
- structs, anonymous structs, payload enums, fixed arrays, and strings;
- mutable bindings, assignments (plain and compound, `x += 1`), field/index
  places, and evaluation-order rules;
- affine move semantics, `borrow`/`inout` parameter modes, exclusivity checks,
  destructors, and explicit `@drop`;
- modules, `@import`, visibility, and the `std` module;
- checked arithmetic and bounds behavior;
- `unchecked` code, raw pointers, heap operations, I/O, parsing, and random data.

Some accepted work remains experimental. The CLI reports currently available
feature gates through `--help`; preview syntax must be enabled explicitly with
`--preview <feature>` and may change or be removed.

## Ownership model

Values are affine: an owned value can be consumed at most once, and the
compiler inserts destruction for values that remain live at scope exit.
Ordinary parameters receive values; `borrow` permits a temporary read-only
view, while `inout` permits temporary mutation of the caller's place. The
exclusivity checker prevents incompatible overlapping accesses.

Rue is still refining this model. The specification—not analogy with Rust—is
the authority for moves, access paths, partial moves, destruction, and escape
rules.

## Modules and programs

A program may span multiple files. A normal compile names one root source file;
that root's `@import` graph is discovered transitively. `@import("path.rue")`
introduces a module object, and public declarations can be selected from it or
re-exported through public constants. The standard library is imported as
`@import("std")`.

```rue
const math = @import("math.rue");

fn main() -> i32 {
    math.answer()
}
```

The transitional flat multi-file namespace is being removed; new code should
compile the root source and use module imports rather than relying on
unqualified names from separately listed files.

## Runtime and targets

Rue produces native executables for x86-64 Linux, AArch64 Linux, and AArch64
macOS. It does not depend on a garbage collector or LLVM. Runtime facilities
include allocation, strings, memory operations, standard I/O, integer parsing,
and randomness.

The project is not production-ready. The standard library is small, language
semantics continue to evolve, and implementation limits are documented in the
specification. See the [tutorial](../website/content/tutorial/) for guided
examples and [architecture.md](architecture.md) for compiler internals.
