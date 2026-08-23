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

An import path `P` names exactly one candidate file:

1. If `P` is exactly `"std"`, the import resolves to the standard library
   (see 10.2:6).
2. If `P` ends with the `.rue` extension, the import resolves to the file at
   exactly that relative path.
3. Otherwise, the import resolves to the directory-module facade
   `{P}/_{basename}.rue` (where `basename` is the final path component of
   `P`), and to nothing else. A file module is imported only by its
   extensioned spelling; a sibling `{P}.rue` is never a candidate for the
   extensionless path, so both forms may coexist without ambiguity.

{{ rule(id="10.2:2", cat="normative") }}

A relative candidate path is resolved against exactly one base directory:
the directory containing the *importing* file. There is no fallback base;
in particular, resolution never retries relative to the root file. Adding a
file therefore either satisfies a previously failing import at its one
candidate path or is irrelevant to it — it can never retarget an import
that already resolves.

{{ rule(id="10.2:8", cat="normative") }}

An import path is a *relative* path. An empty path names no candidate, and
an absolute path — one beginning with the root separator `/` — is not
resolved against the importing file's directory at all, so 10.2:1 and 10.2:2
give it no meaning. An absolute path would also bind the program to one
machine's directory layout, which the project-root-relative identity of
10.2:4 exists to prevent: the same source tree, checked out elsewhere, would
stop compiling. Either shape is a compile-time error (rule
[4.13:133](@/04-expressions/13-intrinsics.md), error E0714), decided from the
path text alone, and no filesystem probe is attempted for it. The reserved
specifier `"std"` is not a path and is not subject to this rule (10.2:6).

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
import path to the file's canonical location: its normalized path relative
to the *project root*, the directory containing the root file. Rule 10.2:7
makes this identity total. Two import spellings denote the same module
exactly when they normalize to the same project-root-relative path; an
import that resolves to an already-loaded file refers to that same module.
Import cycles are therefore legal: two modules may import each other, and a
chain of imports may return to its starting file.

{{ rule(id="10.2:5", cat="example") }}

Mutually importing modules:

```rue
// a.rue
const b = @import("b.rue");
pub fn from_a() -> i32 { 1 }
pub fn a_uses_b() -> i32 { b.from_b() }

// b.rue
const a = @import("a.rue");
pub fn from_b() -> i32 { 42 }

// main.rue
fn main() -> i32 {
    let a = @import("a.rue");
    a.a_uses_b()  // 42
}
```

## Standard Library Resolution

{{ rule(id="10.2:6", cat="normative") }}

`std` is a reserved specifier, not a relative path: it is never searched
relative to the importing file. `@import("std")` resolves to the standard
library facade `_std.rue` through a fixed precedence chain, taking the
first that exists:

1. the program's vendored copy, `std/_std.rue` under the project root; then
2. the toolchain installation default, `$RUE_STD_PATH/_std.rue`, when the
   `RUE_STD_PATH` environment variable is set.

A standard library the program ships therefore cannot be replaced by
ambient environment. If neither location holds a facade, the import is a
compile-time error (E0705).

## Project-Root Identity

{{ rule(id="10.2:7", cat="normative") }}

Every relative candidate path lies within the project root. An import whose
normalized candidate path would fall outside the root file's directory is
rejected at compile time (rule
[4.13:89](@/04-expressions/13-intrinsics.md), error E0713); no filesystem
probe is attempted for it. Standard-library modules resolve within their
own root under 10.2:6 and are not subject to this rule.
