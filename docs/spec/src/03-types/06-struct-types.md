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

## Memory Layout

{{ rule(id="3.6:7", cat="informative") }}

The memory layout rules described in this section are provisional and may change in future versions of Rue. The current design prioritizes simplicity over space efficiency.

{{ rule(id="3.6:8", cat="normative") }}

All types in Rue occupy one or more 8-byte slots. Scalar types (`i8`, `i16`, `i32`, `i64`, `u8`, `u16`, `u32`, `u64`, `bool`) each occupy one slot. The `unit` type occupies one slot.

{{ rule(id="3.6:9", cat="normative") }}

Struct fields are laid out in memory in declaration order, with each field occupying a number of slots determined by its type.

{{ rule(id="3.6:10", cat="normative") }}

The size of a struct is the sum of the sizes of all its fields. There is no padding between fields or at the end of the struct.

{{ rule(id="3.6:11") }}

```rue
// A struct with two i32 fields occupies 2 slots (16 bytes)
struct Point { x: i32, y: i32 }

// A struct with a nested struct occupies the sum of all nested field slots
struct Line { start: Point, end: Point }  // 4 slots (32 bytes)
```

{{ rule(id="3.6:12", cat="normative") }}

All structs have 8-byte alignment.
