+++
title = "Modules and the Standard Library"
weight = 12
template = "tutorial/page.html"
+++

# Modules and the Standard Library

A Rue program is one root file plus everything it reaches through `@import`.
Each file is a module. A module's public declarations are reached through the
name you bind the import to, and the standard library is just a module you
import the same way.

Create a directory `geometry` and put this in `geometry/shapes.rue`:

```rue file=geometry/shapes.rue
const std = @import("std");

pub struct Rect {
    width: i32,
    height: i32,

    fn area(borrow self) -> i32 {
        self.width * self.height
    }
}

pub fn square(side: i32) -> Rect {
    Rect { width: side, height: side }
}

pub fn describe(borrow r: Rect) -> std.strbuf.StrBuf {
    @to_string(r.width) + "x" + @to_string(r.height) + " (area " + @to_string(r.area()) + ")"
}

fn helper() -> i32 {
    99
}
```

Then, next to the `geometry` directory, `main.rue`:

```rue run
const std = @import("std");
const shapes = @import("geometry/shapes.rue");

fn main() -> i32 {
    let r = shapes.Rect { width: 3, height: 4 };
    println(shapes.describe(borrow r));
    let s = shapes.square(5);
    println(shapes.describe(borrow s));
    0
}
```

```bash
scripts/rue exec main.rue
```

```text
3x4 (area 12)
5x5 (area 25)
```

You name only `main.rue`. The compiler follows its imports to find
`geometry/shapes.rue`, and would follow that file's imports in turn.

## How imports work

`@import("path.rue")` loads the file at that path, relative to the directory
of the importing file, and evaluates to a *module object*. Binding it with
`const` gives it a local name, and members are reached with `.`: `shapes.Rect`,
`shapes.describe`. The path must be a string literal; imports are resolved at
compile time, and there is no runtime loading.

Because the module name is explicit at every use, a reader of `main.rue` can
tell where `describe` comes from without searching. Rue has no `use`-style
glob imports that pull names into scope invisibly. If a path is long, bind
what you need to a shorter `const` at the top of the file:

```rue skip
const Rect = @import("geometry/shapes.rue").Rect;
```

## Public and private

Only declarations marked `pub` are visible to code in other directories. The
`geometry` directory is one such boundary, so `main.rue` cannot reach the
private helper:

```rue compile-fail E0706
const shapes = @import("geometry/shapes.rue");

fn main() -> i32 {
    shapes.helper()
}
```

```text
error: [E0706]: function `helper` is private
```

The boundary is the directory, not the file. Files in the same directory can
use each other's private declarations, which lets a directory be split into
several files that share internals while showing one `pub` surface to the rest
of the program.

`pub` applies to functions, structs, enums, and `const` bindings. A `pub const`
that holds a module object or a type re-exports it, which is how a directory
can present one module made of several files.

## The standard library

The standard library is a module too. `@import("std")` is a special path that
the compiler resolves to the toolchain's `std` directory (that is what
`RUE_STD_PATH` points at), and everything in it is reached through the
binding:

```rue run
const std = @import("std");

fn main() -> i32 {
    println("gcd: " + @to_string(std.math.gcd(i32, 84, 36)));
    println("max: " + @to_string(std.math.max(3, 9)));
    println("hex: " + std.fmt.to_hex(255));
    0
}
```

```text
gcd: 12
max: 9
hex: ff
```

There is no prelude. `Option`, `StrBuf`, and `ArrayBuf` are not in scope until
you import `std` and name them, which is why every program that uses them
starts with the import and a few `const` aliases. The aliases are ordinary
constants: `std.option.Option(i64)` is a call that returns a type, and `const`
just gives the result a name.

The current modules are:

| Module | What it holds |
| --- | --- |
| `std.option`, `std.result` | `Option(T)` and `Result(T, E)` |
| `std.strbuf`, `std.strings`, `std.ascii`, `std.fmt` | growable strings, text helpers, formatting |
| `std.arraybuf`, `std.stack`, `std.queue`, `std.deque`, `std.binary_heap` | growable collections |
| `std.intmap`, `std.strmap`, `std.bitset`, `std.grid` | maps keyed by integers or strings, bit sets, 2-D grids |
| `std.math`, `std.cmp`, `std.hash`, `std.rand` | integer and float math, ordering, hashing, random numbers |
| `std.fs`, `std.env`, `std.net`, `std.binary` | files and directories, environment, TCP, byte encoding |
| `std.json`, `std.sort` | JSON parsing, sorting |
| `std.mem`, `std.tuple`, `std.c` | `swap`/`replace`, pairs and triples, C scalar aliases |

The library is written in Rue, in the repository's `std/` directory, and
`std/_std.rue` is its table of contents. It is small and it changes, so the
source is the reference for now.

## Generic types are functions

You have been calling `std.option.Option(i64)` and `std.arraybuf.ArrayBuf(i32)`
since chapter 7 without a word about generics. That is because Rue does not
have a separate generics feature. A function whose parameter is
`comptime T: type` runs at compile time and can return a type:

```rue run
const std = @import("std");

fn Pair(comptime T: type) -> type {
    struct {
        first: T,
        second: T,

        fn swapped(borrow self) -> Self {
            Self { first: self.second, second: self.first }
        }
    }
}

const IntPair = Pair(i32);

fn main() -> i32 {
    let p = IntPair { first: 1, second: 2 };
    let q = p.swapped();
    println(@to_string(q.first) + " " + @to_string(q.second));
    0
}
```

```text
2 1
```

`Pair(i32)` and `Pair(bool)` are two distinct struct types, each with its own
`swapped`. The whole standard library's collection story is built this way. It
means "generic code" is written with the same `fn`, `struct`, and calls you
already know; the only new word is `comptime`.

Next: the last piece of language before the project, tests.
