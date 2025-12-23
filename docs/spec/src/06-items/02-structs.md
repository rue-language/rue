# Structs

r[6.2:1#normative]
A struct is defined using the `struct` keyword.

r[6.2:2#normative]
```ebnf
struct_def = "struct" IDENT "{" [ struct_fields ] "}" ;
struct_fields = struct_field { "," struct_field } [ "," ] ;
struct_field = IDENT ":" type ;
```

## Struct Definition

r[6.2:3#normative]
Field names must be unique within a struct.

r[6.2:4]
```rue
struct Point {
    x: i32,
    y: i32,
}
```

## Struct Instantiation

r[6.2:5#normative]
All fields must be initialized when creating a struct instance.

r[6.2:6#normative]
Field initializers may be provided in any order.

r[6.2:7]
```rue
struct Point { x: i32, y: i32 }

fn main() -> i32 {
    // Fields can be initialized in any order
    let p = Point { y: 20, x: 10 };
    p.x + p.y
}
```

## Struct Usage

r[6.2:8#normative]
Struct fields are accessed using dot notation.

r[6.2:9#normative]
Mutable struct values allow field reassignment.

r[6.2:10]
```rue
struct Counter { value: i32 }

fn main() -> i32 {
    let mut c = Counter { value: 0 };
    c.value = c.value + 1;
    c.value
}
```
