+++
title = "Program Composition"
weight = 5
template = "spec/page.html"
+++

# Program Composition

This section specifies how the root source file, imported modules, and
explicitly listed source files compose into one program.

The driver contract is root-module based: a normal compiler invocation
names one root source file, and that root's transitive `@import` graph
determines the semantic module graph. Source files may also be declared to
a build system as action inputs (a source manifest, ADR-0047), but
declaration as an available input is not a way to put names in scope: a
source file affects name resolution only when the program reaches it
through an explicit import.

Extra source files listed positionally on the command line are a legacy
input form (ADR-0046 defines its removal; RUE-434 tracks it). Listed files
are loaded, but their names are **not** placed in any shared scope: an
unqualified reference to another file's item is a name-resolution error
exactly as it is between imported modules (rule 10.5:2).

## Top-Level Names Are Module-Scoped

{{ rule(id="10.5:1", cat="legality-rule") }}

It is a compile-time error for one source file to define two top-level
items with the same name, whether of the same kind (duplicate definition,
E0436) or of different kinds (a `const` and a `fn` sharing a name, also
E0436). This check is per-file: it never considers items in other loaded
files.

{{ rule(id="10.5:2", cat="normative") }}

Top-level names are scoped to their defining source file (their module).
Two loaded files **MAY** define top-level items with the same name — of
the same kind or of different kinds — regardless of visibility and
regardless of directory. Each file's items are reachable from other files
only through a module binding (`m.item`, rule 10.4:1); an unqualified
reference to a name defined only in another loaded file is a
name-resolution error (E0201/E0202/E0204, rule 10.3:8), never a silent
resolution into the other file.

{{ rule(id="10.5:3", cat="example") }}

```rue
// a.rue
pub fn shared() -> i32 { 1 }

// b.rue
pub fn shared() -> i32 { 2 }   // legal: names are module-scoped

// main.rue
fn main() -> i32 {
    let a = @import("a");
    let b = @import("b");
    a.shared() + b.shared()    // 3: each call resolves in its own module
}
```

## Root Module and Extent of Analysis

{{ rule(id="10.5:4", cat="normative") }}

The semantic compilation unit is the root module and its transitive import
graph. An implementation **MAY** analyze imported modules on demand
(ADR-0045): items of an imported module that the program never references
are not guaranteed to be semantically analyzed, so a program **MUST NOT**
rely on a compile-time error inside an unreferenced imported item being
reported. Errors in code the program reaches — including declaration-level
errors of any loaded file (parse errors, duplicate definitions, invalid
signatures) — are always reported.
