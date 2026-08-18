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
const strings = @import("strings.rue");
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

Visibility is uniform across every multi-file compilation and every item
kind: an item is usable outside its defining directory if and only if it
is `pub`. Access to a private item through a module binding from another
directory is error E0706 (10.4:18) — the diagnostic for privacy
violations, since cross-module references are spelled through module
bindings, and an unqualified reference to another file's item does not
resolve at all (10.3:8). One form reports a distinct code: applying a
comptime type constructor reached through a module binding in a type
position (10.4:16) checks the constructor's visibility at the
application site, and a private constructor applied from another
directory is error E0460, naming the constructor and its defining file.

{{ rule(id="10.3:8", cat="normative") }}

Unqualified references resolve module-locally: a top-level name refers to
an item of the referencing file (or a compiler builtin). A name defined
only in other loaded files — `pub` or not, in any directory — does not
resolve unqualified; the reference is a name-resolution error (E0201 for
variables/constants and enum type names in expressions, E0202 for
functions, E0204 for types), never a silent resolution into another
file. Cross-module access is spelled through a module binding (10.4:1).

{{ rule(id="10.3:9", cat="example") }}

```rue
// sub/lib.rue
fn secret() -> i32 { 99 }       // private to sub/
pub fn open() -> i32 { 7 }
struct Hidden { n: i32, }       // private to sub/
pub struct Shared { n: i32, }
pub const MAX: i32 = 16;

// main.rue — a different directory
const lib = @import("sub/lib.rue");

fn main() -> i32 {
    // secret()                    // error E0202: does not resolve here (10.3:8)
    // Shared { n: 1 };            // error E0204: pub, but still module-scoped
    // lib.secret()                // error E0706: private to sub/ (10.4:18)
    // lib.Hidden { n: 1 };        // error E0706: private to sub/
    let s = lib.Shared { n: lib.MAX };  // OK: pub members through the binding
    lib.open() + s.n                    // 7 + 16 = 23
}
```
