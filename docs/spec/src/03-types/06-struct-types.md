+++
title = "Struct Types"
weight = 6
template = "spec/page.html"
+++

# Struct Types

{{ rule(id="3.6:1", cat="normative") }}

A struct type is a composite type consisting of named fields.

{{ rule(id="3.6:2", cat="normative") }}

A struct is defined using the `struct` keyword:

```ebnf
struct_def = "struct" IDENT "{" [ struct_fields ] "}" ;
struct_fields = struct_field { "," struct_field } [ "," ] ;
struct_field = IDENT ":" type ;
```

{{ rule(id="3.6:3") }}

```rue
struct Point {
    x: i32,
    y: i32,
}

fn main() -> i32 {
    let p = Point { x: 10, y: 20 };
    p.x + p.y  // 30
}
```

{{ rule(id="3.6:4", cat="normative") }}

Struct fields are accessed using dot notation: `value.field_name`.

{{ rule(id="3.6:5", cat="normative") }}

All fields must be initialized when creating a struct instance.

{{ rule(id="3.6:6", cat="normative") }}

Field names must be unique within a struct.
