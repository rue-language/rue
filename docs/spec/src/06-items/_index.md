+++
title = "Items"
weight = 6
sort_by = "weight"
template = "spec/section.html"
page_template = "spec/page.html"
+++

# Items

This chapter describes items in Rue.

> **Grammar note.** The EBNF fragments in this chapter are illustrative
> excerpts scoped to the construct under discussion, and are deliberately
> narrower than the full syntax (they may omit `pub`, directives, receiver
> modes, method bodies, or variant payloads). [Appendix A](../appendices/a-grammar/)
> is the normative grammar; where a fragment here differs from it, Appendix A
> governs.

{{ rule(id="6.0:1") }}

Items are top-level definitions in a program. Unlike statements, items are visible throughout the module.

{{ rule(id="6.0:1a", cat="informative") }}

The item kinds are:

- **Functions** (`fn`, 6.1), including the program entry point `main`.
- **Structs** (`struct`, 6.2), which may declare methods and associated
  functions in the struct body (6.4).
- **Enums** (`enum`, 6.3).
- **Constants** (`const`, 6.5).
- **Top-level destructors** (`drop fn`, 3.9), which are declared at the top
  level rather than inside any enclosing block.

Rue has no separate `impl` block: methods and associated functions are written
inside the struct body (6.4), and a user-defined destructor for a named struct
is a top-level `drop fn` item (3.9). There is no item form that groups
implementations under a type the way an `impl` block does in other languages.

## Type Name Uniqueness

{{ rule(id="6.0:2", cat="legality-rule") }}

User-defined type names (structs and enums) **MUST** be unique within a program. Defining multiple types with the same name produces a compile-time error.

{{ rule(id="6.0:3", cat="legality-rule") }}

User-defined types **MUST NOT** use names reserved for built-in types. Currently, the reserved growable-string type names are `StrBuf` and its deprecated alias `String`.

{{ rule(id="6.0:4", cat="example") }}

```rue
// Error: cannot define type with reserved name
struct StrBuf { data: i32 }  // compile error
```

{{ rule(id="6.0:5", cat="legality-rule") }}

User-defined functions **MUST NOT** use names reserved for runtime and code-generation helpers. The reserved function names are exactly: any name beginning with `__rue_`; the program entry points `_start` and `_main`; and the compiler-builtin memory routines `memcpy`, `memmove`, `memset`, `memcmp`, and `bcmp` (the runtime exports these under their fixed platform names, so a user definition would collide at link time). Every other compiler- and runtime-emitted symbol — including built-in type methods and associated functions (`__rue_String_len`, `__rue_String_new`) — lives under the `__rue_` prefix, so the reserved set does not grow as built-in types are added. Defining a function with a reserved name produces a compile-time error.

{{ rule(id="6.0:6", cat="example") }}

```rue
// Error: cannot define function with reserved name
fn __rue_alloc() -> i32 { 0 }  // compile error: `__rue_` prefix is reserved

// OK: `String__len` is an ordinary identifier, distinct from the built-in
// `String.len` method (whose runtime symbol is `__rue_String_len`).
fn String__len() -> i32 { 0 }  // allowed
```
