+++
title = "Module Forms"
weight = 1
template = "spec/page.html"
+++

# Module Forms

This section describes what a module is and the two on-disk forms a module
can take: a file module and a directory module.

{{ rule(id="10.1:1", cat="normative") }}

A module is a Rue source file. The top-level items of the file — functions,
structs, enums, and constants — are the members of the module.

{{ rule(id="10.1:2", cat="normative") }}

A *file module* is a source file `{name}.rue`. An import of `{name}` that
resolves to this file yields the module defined by it.

{{ rule(id="10.1:3", cat="normative") }}

A *directory module* is a directory `{name}/` that contains a facade file
`_{name}.rue` directly inside it. The facade file is the directory module's
root: an import of `{name}` that resolves to the directory module yields the
module defined by the facade file. Other files inside the directory are not
automatically part of the imported module; the facade reaches them via its
own imports.

{{ rule(id="10.1:4", cat="normative") }}

A file `_{name}.rue` is a facade only when its enclosing directory is named
`{name}`. Anywhere else — in particular, as a *sibling* of a directory or a
file named `{name}` — it is an ordinary source file with no special meaning,
and it does not make `{name}` importable.

{{ rule(id="10.1:5") }}

Both module forms may coexist for the same name: an extensionless import
path names the directory module `{path}/_{basename}.rue` alone, and the
file module `{path}.rue` is reached only by its extensioned spelling
(rule 10.2:1). Neither form can capture an import addressed to the other,
so no ambiguity arises. (The former ambiguity rejection, E0708, is
retired.)

{{ rule(id="10.1:6", cat="example") }}

A project mixing both forms:

```text
main.rue            # root file
math.rue            # file module: @import("math.rue")
utils/              # directory module: @import("utils")
├── _utils.rue      #   facade — the module root for "utils"
└── strings.rue     #   helper file, reached via the facade's imports
```
