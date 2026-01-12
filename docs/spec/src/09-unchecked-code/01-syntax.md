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

A `checked` block evaluates its body and returns the value of the final expression. The type of a `checked` block is the type of its body expression.

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

{{ rule(id="9.1:10", cat="informative") }}

Raw pointer types are parsed in Phase 1 but semantic analysis (type checking, pointer operations) is implemented in later phases. Until Phase 2 is implemented, using pointer types results in an "unknown type" error.

{{ rule(id="9.1:11", cat="example") }}

```rue
// These parse correctly but fail type checking until Phase 2
fn takes_ptr(p: ptr const i32) -> i32 { 0 }
fn takes_mut_ptr(p: ptr mut i32) -> i32 { 0 }
unchecked fn get_ptr() -> ptr const i32 { @panic() }
```
