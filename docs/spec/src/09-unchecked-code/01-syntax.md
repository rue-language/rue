+++
title = "Unchecked Code Syntax"
weight = 1
template = "spec/page.html"
+++

# Unchecked Code Syntax

This section describes the syntax for unchecked code constructs.

## Unchecked Functions

{{ rule(id="9.1:1", cat="normative") }}

A function **MAY** be marked with the `unchecked` modifier to indicate that calling it requires a `checked` block.

{{ rule(id="9.1:2", cat="syntax") }}

```ebnf
function = [ "pub" ] [ "unchecked" ] "fn" IDENT "(" [ params ] ")" [ "->" type ] "{" block "}" ;
```

{{ rule(id="9.1:3", cat="example") }}

```rue
unchecked fn dangerous_operation() -> i32 {
    42
}

pub unchecked fn public_dangerous() -> i32 {
    0
}
```

## Checked Blocks

{{ rule(id="9.1:4", cat="normative") }}

A `checked` block is an expression that enables unchecked operations within its body.

{{ rule(id="9.1:5", cat="syntax") }}

```ebnf
checked_expr = "checked" "{" block "}" ;
```

{{ rule(id="9.1:6", cat="dynamic-semantics") }}

A `checked` block evaluates its body block using the ordinary block-expression
rules (4.5). Its value is the body block's value: the tail expression's value
when present, or `()` when the body has no tail expression. The type of a
`checked` block is the type of that body block.

{{ rule(id="9.1:7", cat="example") }}

```rue
fn main() -> i32 {
    let x = checked {
        let a = 10;
        let b = 32;
        a + b
    };
    x
}
```

## Raw Pointer Types

{{ rule(id="9.1:8", cat="normative") }}

Rue provides two raw pointer types for low-level memory access:
- `ptr const T` - a pointer to immutable data of type `T`
- `ptr mut T` - a pointer to mutable data of type `T`

{{ rule(id="9.1:9", cat="syntax") }}

```ebnf
ptr_type = "ptr" ( "const" | "mut" ) type ;
```

{{ rule(id="9.1:10", cat="normative") }}

Raw pointer types are fully type-checked: they may appear as the type of a
local, a parameter, a struct field, or a function return type. A `ptr const T`
and a `ptr mut T` are distinct types, and two pointer types are equal only when
their pointee types `T` are equal — a `ptr const i32` is neither a `ptr mut
i32` nor a `ptr const i64`.

{{ rule(id="9.1:11", cat="example") }}

```rue
fn takes_ptr(p: ptr const i32) -> i32 { 0 }
fn takes_mut_ptr(p: ptr mut i32) -> i32 { 0 }
fn identity_ptr(p: ptr const i32) -> ptr const i32 { p }
struct Node { next: ptr const Node, value: i32 }
```

{{ rule(id="9.1:12", cat="legality-rule") }}

A raw-pointer intrinsic — `@raw`, `@raw_mut`, `@field_ptr`, `@ptr_read`,
`@ptr_write`, `@ptr_read_unaligned`, `@ptr_write_unaligned`, `@ptr_offset`,
`@ptr_to_int`, or `@int_to_ptr` — is an unchecked operation and **MUST** appear
within a `checked` block. Using one outside a `checked` block is a compile
error. (Defining a `ptr const T` / `ptr mut T` value's *type* is always legal;
only the pointer *operations* require a `checked` block.) The same requirement
applies to the allocation family (9.2:13) and the raw-byte intrinsics
(9.2:14e, 9.2:14h, 9.2:14l).

{{ rule(id="9.1:13", cat="example") }}

```rue
fn main() -> i32 {
    let x: i32 = 42;
    // The pointer operations are wrapped in `checked`; the pointer types
    // themselves need no `checked`.
    let p: ptr const i32 = checked { @raw(x) };
    checked { @ptr_read(p) }
}
```
