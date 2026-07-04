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

{{ rule(id="6.2:11", cat="informative") }}

A struct definition **MAY** be prefixed with the `linear` keyword, making it a
*linear* (must-consume) type: a value of a `linear struct` type must be
explicitly consumed and cannot be implicitly dropped. The `linear` modifier and
its full semantics — including infectious linearity through fields and arrays —
are specified with move semantics (3.8), not repeated here.

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

A field of a mutable struct binding is a *place* and may be the target of an
assignment (5.2:1, 5.2:2); assigning to it drops the field's prior value, if
live, before storing the new one (overwrite-drop, 5.2:1). Assigning to a field
of an immutable binding is a compile-time error (5.2:8).

{{ rule(id="6.2:10") }}

```rue
struct Counter { value: i32 }

fn main() -> i32 {
    let mut c = Counter { value: 0 };
    c.value = c.value + 1;
    c.value
}
```
