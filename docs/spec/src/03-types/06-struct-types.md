# Struct Types

r[3.6:1#normative]
A struct type is a composite type consisting of named fields.

r[3.6:2#normative]
A struct is defined using the `struct` keyword:

```ebnf
struct_def = "struct" IDENT "{" [ struct_fields ] "}" ;
struct_fields = struct_field { "," struct_field } [ "," ] ;
struct_field = IDENT ":" type ;
```

r[3.6:3]
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

r[3.6:4#normative]
Struct fields are accessed using dot notation: `value.field_name`.

r[3.6:5#normative]
All fields must be initialized when creating a struct instance.

r[3.6:6#normative]
Field names must be unique within a struct.
