+++
title = "Visibility"
weight = 3
template = "spec/page.html"
+++

# Visibility

This section specifies the `pub` visibility modifier and the
intra-directory visibility rule: visibility boundaries in Rue are
*directories*, not files.

{{ rule(id="10.3:1", cat="normative") }}

A top-level function, struct, enum, or constant **MAY** be marked with the
`pub` modifier. An item without `pub` is *private*.

{{ rule(id="10.3:2", cat="normative") }}

A private item is visible throughout the directory that contains its
defining file: code in any source file in the *same directory* may access
it, including through a module import.

{{ rule(id="10.3:3", cat="legality-rule") }}

It is a compile-time error to access a private item through a module from a
source file in a *different directory* than the item's defining file
(error E0706).

{{ rule(id="10.3:4", cat="normative") }}

A `pub` item is accessible through a module import from any directory.

{{ rule(id="10.3:5", cat="normative") }}

A directory module's facade file lives inside the directory it fronts
(10.1:3); the facade therefore has intra-directory access to the private
items of the other files in that directory, while external importers of the
directory module see only the facade's `pub` members.

{{ rule(id="10.3:6", cat="example") }}

```rue
// utils/_utils.rue — the facade
const strings = @import("strings");
pub fn format() -> i32 { strings.internal_format() }  // OK: same directory

// utils/strings.rue
fn internal_format() -> i32 { 42 }  // private

// main.rue — a different directory
fn main() -> i32 {
    let utils = @import("utils");
    utils.format()              // OK: `format` is pub
    // utils.internal_format()  // would be an error even if imported:
    //                          // private to the utils/ directory
}
```

{{ rule(id="10.3:7", cat="legality-rule") }}

Visibility is uniform across every multi-file compilation: an item is
visible outside its defining directory if and only if it is `pub`,
whether its defining file was loaded via `@import` or only listed
explicitly in the compilation. It is a compile-time error to call a
private function by its unqualified name from a source file in a
different directory than the function's defining file (error E0460).

{{ rule(id="10.3:8", cat="normative") }}

The transitional flat namespace (10.5:2) affects only *name resolution*:
an unqualified reference may resolve to an item in any file of the
compilation without an import, but the visibility rules of this section
apply regardless of how the item's file was loaded. A `pub` function in
another directory is therefore callable unqualified without any import;
a private one is not.

{{ rule(id="10.3:9", cat="example") }}

```rue
// sub/lib.rue
fn secret() -> i32 { 99 }   // private to sub/
pub fn open() -> i32 { 7 }

// main.rue — a different directory; no import needed (10.5:2)
fn main() -> i32 {
    // secret()              // error E0460: private to sub/lib.rue
    open()                   // OK: `open` is pub
}
```
