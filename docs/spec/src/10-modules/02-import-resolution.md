+++
title = "Import Resolution"
weight = 2
template = "spec/page.html"
+++

# Import Resolution

This section specifies how an import path given to `@import` is resolved to
a source file on disk, and how the set of loaded files is computed. The
`@import` intrinsic's argument form and its error conditions are rules
4.13:79–4.13:89 in the
[intrinsics section](@/04-expressions/13-intrinsics.md).

## Resolution Order

{{ rule(id="10.2:1", cat="normative") }}

An import path `P` is resolved by trying the following interpretations in
order; the first that matches is used:

1. If `P` is exactly `"std"`, the import resolves to the standard library
   (see 10.2:6).
2. If `P` ends with the `.rue` extension, the import resolves to the file at
   exactly that relative path.
3. Otherwise, the import resolves to the file module `{P}.rue`, or, when no
   such file exists, to the directory-module facade `{P}/_{basename}.rue`
   (where `basename` is the final path component of `P`). If *both* exist,
   the import is ambiguous and rejected (rule
   [4.13:89](@/04-expressions/13-intrinsics.md)).

{{ rule(id="10.2:2", cat="normative") }}

Relative candidate paths are searched against base directories in order:
first the directory containing the *importing* file, then the directory
containing the *root* file (the first source file given to the compiler). A
module found relative to the importing file takes precedence over a module
at the same relative path found relative to the root file.

## Transitive Loading

{{ rule(id="10.2:3", cat="normative") }}

Importing a module loads it *transitively*: imports appearing in an imported
file are themselves resolved and loaded, and so on, into the root module's
import graph. Loading a file through this graph makes it available as a
module; it does not inject the file's declarations into the importing
scope. The importer of a transitively loaded file — not the root file — is
the base for that file's own relative imports (per 10.2:2).

{{ rule(id="10.2:4", cat="normative") }}

Each source file is loaded at most once per compilation, after resolving the
import path to the file's canonical location. An import that resolves to an
already-loaded file refers to that same module, even if the import used a
different relative spelling. Import cycles are therefore legal: two modules
may import each other, and a chain of imports may return to its starting
file.

{{ rule(id="10.2:5", cat="example") }}

Mutually importing modules:

```rue
// a.rue
const b = @import("b");
pub fn from_a() -> i32 { 1 }
pub fn a_uses_b() -> i32 { b.from_b() }

// b.rue
const a = @import("a");
pub fn from_b() -> i32 { 42 }

// main.rue
fn main() -> i32 {
    let a = @import("a");
    a.a_uses_b()  // 42
}
```

## Standard Library Resolution

{{ rule(id="10.2:6", cat="normative") }}

`@import("std")` resolves to the standard library facade `_std.rue`. It is
searched for at `$RUE_STD_PATH/_std.rue` when the `RUE_STD_PATH` environment
variable is set, and otherwise as an ordinary directory module `std/_std.rue`
per 10.2:2. When both exist, `$RUE_STD_PATH` takes precedence. If no
standard library is found, the import is a compile-time error (E0705).
