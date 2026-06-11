+++
title = "Program Composition"
weight = 5
template = "spec/page.html"
+++

# Program Composition

This section specifies how the set of loaded source files composes into one
program: the (transitional) shared namespace and the extent of semantic
analysis.

## Name Collisions

{{ rule(id="10.5:1", cat="legality-rule") }}

It is a compile-time error for two loaded source files to define a top-level
item with the same name (duplicate definition, E0436). This holds regardless
of the items' visibility and regardless of whether the files are in the same
directory. Module-binding constants (`const m = @import(...)`) are the
exception: they are scoped per file (rule 10.4:8) and do not collide across
files.

{{ rule(id="10.5:2") }}

Rule 10.5:1 is *transitional*: loaded files currently share one flat global
namespace, so symbols collide program-wide even when each is only ever
accessed through its own module. A future revision of the module system is
expected to scope top-level names per module, relaxing this rule; programs
should not be designed to rely on cross-file collisions being errors.

{{ rule(id="10.5:3", cat="example") }}

```rue
// a.rue
pub fn shared() -> i32 { 1 }

// b.rue
pub fn shared() -> i32 { 2 }   // error: duplicate definition of `shared`

// main.rue
fn main() -> i32 {
    let a = @import("a");
    let b = @import("b");      // loading both files collides
    a.shared()
}
```

## Extent of Analysis

{{ rule(id="10.5:4") }}

Every loaded source file is fully analyzed: a compile-time error anywhere in
an imported module is reported even if the offending item is never
referenced by the importing program. (ADR-0026 envisions lazy, on-demand
analysis of imported modules; that is not implemented, and this paragraph is
informative so that the eager behavior is not a guarantee either.)
