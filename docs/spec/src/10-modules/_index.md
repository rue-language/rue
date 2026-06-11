+++
title = "Modules"
weight = 10
sort_by = "weight"
template = "spec/section.html"
page_template = "spec/page.html"
+++

# Modules

This chapter describes Rue's module system: how source files form modules,
how import paths are resolved, item visibility, module bindings and
re-exports, and how loaded files compose into a program.

{{ rule(id="10.0:1") }}

A Rue program is composed of modules. Each module corresponds to a source
file. Modules are brought into scope with the `@import` intrinsic; the
intrinsic itself (argument form, result type, and the associated
compile-time errors) is specified in the
[intrinsics section](@/04-expressions/13-intrinsics.md) as rules
4.13:79 through 4.13:89. This chapter specifies the module system around
that intrinsic.
