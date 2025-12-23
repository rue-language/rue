# Field Access Expressions

r[4.12:1#normative]
A field access expression accesses a field of a struct.

r[4.12:2#normative]
```ebnf
field_access = expression "." IDENT ;
```

r[4.12:3#normative]
The expression before the dot must have a struct type.

r[4.12:4#normative]
The identifier must be a valid field name for that struct type.

r[4.12:5#normative]
The type of a field access expression is the type of the accessed field.

r[4.12:6]
```rue
struct Point {
    x: i32,
    y: i32,
}

fn main() -> i32 {
    let p = Point { x: 10, y: 32 };
    p.x + p.y  // 42
}
```

## Field Assignment

r[4.12:7#normative]
For mutable struct values, fields can be assigned.

r[4.12:8]
```rue
struct Point {
    x: i32,
    y: i32,
}

fn main() -> i32 {
    let mut p = Point { x: 0, y: 0 };
    p.x = 20;
    p.y = 22;
    p.x + p.y  // 42
}
```
