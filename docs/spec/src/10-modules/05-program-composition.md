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
files. The sole exception is the program entry point `main`, whose
uniqueness is enforced program-wide (rule 10.5:5).

{{ rule(id="10.5:2", cat="normative") }}

Top-level names are scoped to their defining source file (their module).
Two loaded files **MAY** define top-level items with the same name — of
the same kind or of different kinds — regardless of visibility and
regardless of directory, with the single exception of the entry point
`main` (rule 10.5:5). Each file's items are reachable from other files
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

## Program-Wide Uniqueness of `main`

{{ rule(id="10.5:5", cat="legality-rule") }}

The program entry point `main` is the one top-level name whose uniqueness
is enforced across the entire program rather than per-file. It is a
compile-time error (E0436) for two loaded files to each define a top-level
`main`, even when only one of them is the root file's `main` and the other
`main` is never called. This is an exception to rule 10.5:2: every other
top-level name — including `shared` in the 10.5:3 example — remains
module-scoped and may be duplicated freely across files. The check is
performed eagerly at declaration gathering, before body reachability is
known (rule 10.5:4), so a duplicate `main` in a loaded-but-unreferenced
module is still reported. The restriction exists because an executable
import graph has exactly one entry point, invoked through the executable
entry ABI (rules 6.1:7, 6.1:8); ADR-0047 records the rationale and tracks
the root-module end state that will eventually localize entry-point
selection.

{{ rule(id="10.5:6", cat="example") }}

```rue
// helper.rue
pub fn main() -> i32 { 0 }        // second entry point
pub fn answer() -> i32 { 42 }

// main.rue
const helper = @import("helper.rue");

fn main() -> i32 {                // error E0436: `main` is already defined
    helper.answer()               // helper.main is never called, but is rejected
}
```

## Root Module and Extent of Analysis

{{ rule(id="10.5:4", cat="normative") }}

The semantic compilation unit is the root module and its transitive import
graph. Ordinary function and method bodies are analyzed on demand from
`main`. A compile-time error inside an unreferenced ordinary body is not
reported, whether that body is in the root file or an imported module.

This body-level frontier does not yet make the entire front end lazy. Loaded
files are parsed and their declarations are gathered eagerly, so syntax,
duplicate-definition, and signature errors are reported before body
reachability is known. Named destructors are also currently implicit analysis
roots because drop glue is synthesized from the full type pool. ADR-0045
defines the broader on-demand model; ADR-0047 allows future build-system source
manifests that constrain which files may be imported without turning every
declared input into a semantic root.
