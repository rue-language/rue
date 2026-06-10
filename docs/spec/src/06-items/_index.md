+++
title = "Items"
weight = 6
sort_by = "weight"
template = "spec/section.html"
page_template = "spec/page.html"
+++

# Items

This chapter describes items in Rue.

{{ rule(id="6.0:1") }}

Items are top-level definitions in a program. Unlike statements, items are visible throughout the module.

## Type Name Uniqueness

{{ rule(id="6.0:2", cat="legality-rule") }}

User-defined type names (structs and enums) **MUST** be unique within a program. Defining multiple types with the same name produces a compile-time error.

{{ rule(id="6.0:3", cat="legality-rule") }}

User-defined types **MUST NOT** use names reserved for built-in types. Currently, the only reserved type name is `String`.

{{ rule(id="6.0:4", cat="example") }}

```rue
// Error: cannot define type with reserved name
struct String { data: i32 }  // compile error
```

{{ rule(id="6.0:5", cat="legality-rule") }}

User-defined functions **MUST NOT** use names reserved for runtime and code-generation helpers. Reserved function names are: any name beginning with `__rue_`, the program entry point `_start`, and any name of the form `T__m` where `T` is a built-in type name (for example `String__len`). Defining a function with a reserved name produces a compile-time error.

{{ rule(id="6.0:6", cat="example") }}

```rue
// Error: cannot define function with reserved name
fn String__len() -> i32 { 0 }  // compile error
```
