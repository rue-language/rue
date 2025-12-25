+++
title = "Structs"
weight = 2
template = "spec/page.html"
+++

# Structs

{{ rule(id="6.2:1", cat="normative") }}

A struct is defined using the `struct` keyword.

{{ rule(id="6.2:2", cat="normative") }}

```ebnf
struct_def = "struct" IDENT "{" [ struct_fields ] "}" ;
struct_fields = struct_field { "," struct_field } [ "," ] ;
struct_field = IDENT ":" type ;
```

## Struct Definition

{{ rule(id="6.2:3", cat="legality-rule") }}

Field names **MUST** be unique within a struct.

{{ rule(id="6.2:4") }}

```rue
struct Point {
    x: i32,
    y: i32,
}
```

## Struct Instantiation

{{ rule(id="6.2:5", cat="legality-rule") }}

All fields **MUST** be initialized when creating a struct instance.

{{ rule(id="6.2:6", cat="normative") }}

Field initializers **MAY** be provided in any order.

{{ rule(id="6.2:7") }}

```rue
struct Point { x: i32, y: i32 }

fn main() -> i32 {
    // Fields can be initialized in any order
    let p = Point { y: 20, x: 10 };
    p.x + p.y
}
```

## Struct Usage

{{ rule(id="6.2:8", cat="normative") }}

Struct fields are accessed using dot notation.

{{ rule(id="6.2:9", cat="normative") }}

Mutable struct values allow field reassignment.

{{ rule(id="6.2:10") }}

```rue
struct Counter { value: i32 }

fn main() -> i32 {
    let mut c = Counter { value: 0 };
    c.value = c.value + 1;
    c.value
}
```
