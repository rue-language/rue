+++
title = "Modules"
weight = 11
template = "tutorial/page.html"
+++

# Modules

Rue programs can be split across files. A file imports another file with
`@import("path.rue")`, binds the returned module object to a name, and then uses
that name to access public declarations.

## A Two-File Program

Create a directory named `lib`, then create `lib/math.rue`:

```rue
pub fn double(x: i32) -> i32 {
    x * 2
}

fn secret() -> i32 {
    99
}
```

Then create `main.rue` next to the `lib` directory:

```rue
const math = @import("lib/math.rue");

fn main() -> i32 {
    math.double(21)
}
```

Compile and run it:

```bash
RUE="$(scripts/rue-bin)"
export RUE_STD_PATH="$PWD/std"
"$RUE" main.rue -o main
./main
echo $?  # prints: 42
```

`@import("lib/math.rue")` loads the other file and returns a module object. The
`const math = ...` binding gives that module object a local name.

## Public and Private Declarations

Across directory boundaries, only declarations marked `pub` are available
through a module object. In the example above, `math.double(21)` works because
`double` is public.

This does not compile:

```rue
const math = @import("lib/math.rue");

fn main() -> i32 {
    math.secret()
}
```

For the cross-directory import used above, `secret` is private to
`lib/math.rue`, so the compiler reports that the function is private. Use `pub`
only for declarations that are part of the imported file's interface.

## Import Paths

Relative import paths are resolved from the importing file's directory:

```rue
const math = @import("lib/math.rue");
```

The path string must be a string literal known at compile time. Rue does not
dynamically load modules at runtime; imports are part of compilation.

## Standard Library Status

Rue's accepted standard-library direction is an explicit standard module:

```rue
const std = @import("std");
```

The intent is that standard-library types and functions are accessed through
that namespace, for example `std.option.Option` or
`std.arraybuf.ArrayBuf`.

The repository wrapper commands know where the checked-in `std/` directory
lives. If you invoke the compiler binary directly from somewhere else and see
`standard library not found`, set `RUE_STD_PATH` to the repository's `std`
directory.

The important habit is still the same: use explicit module imports and
namespace-qualified access rather than hidden global names.

## Next Steps

You've learned the current core of Rue. For the complete language reference,
read the [Language Specification](/spec/).

Rue is still in early development. If you find bugs or have ideas, please
[file an issue](https://github.com/rue-language/rue/issues).
