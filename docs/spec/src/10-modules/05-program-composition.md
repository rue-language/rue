+++
title = "Program Composition"
weight = 5
template = "spec/page.html"
+++

# Program Composition

This section specifies how the root source file, imported modules, and
legacy explicitly listed source files compose into one program.

The intended driver contract is root-module based: a normal compiler
invocation names one root source file, and that root's transitive
`@import` graph determines the semantic module graph. Source files may
also be declared to a build system as action inputs, but declaration as an
available input is not a way to put names in scope. A source file affects
name resolution only when the program reaches it through an explicit import
or through the legacy flat multi-file mode described below.

The current implementation still contains that legacy flat mode for
additional source files listed explicitly on the command line. ADR-0046
and ADR-0047 define its removal; RUE-434 tracks the implementation work.
Until that removal lands, this section distinguishes the intended root
module/import-graph contract from the transitional loaded-file namespace.

## Legacy Name Collisions

{{ rule(id="10.5:1", cat="legality-rule") }}

In the legacy flat loaded-file namespace, it is a compile-time error for
two loaded source files to define a top-level item with the same name
(duplicate definition, E0436). This holds regardless of the items'
visibility and regardless of whether the files are in the same directory.
Module-binding constants (`const m = @import(...)`) are the exception:
they are scoped per file (rule 10.4:8) and do not collide across files.

{{ rule(id="10.5:2") }}

Rule 10.5:1 is *transitional*: loaded files currently share one flat global
namespace, so symbols collide program-wide even when each is only ever
accessed through its own module. This is not the long-term module model.
The root-module contract scopes program structure through explicit imports,
and programs should not be designed to rely on cross-file collisions being
errors outside a single module.

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

## Root Module and Extent of Analysis

{{ rule(id="10.5:4") }}

In the current implementation, every loaded source file is fully analyzed:
a compile-time error anywhere in an imported module is reported even if the
offending item is never referenced by the importing program.

This eager behavior is an implementation property, not the semantic
definition of a Rue program. The semantic compilation unit is the root
module and its transitive import graph; ADR-0045 allows future lazy,
on-demand analysis of imported modules, and ADR-0047 allows future
build-system source manifests that constrain which files may be imported
without turning every declared input into a semantic root.
