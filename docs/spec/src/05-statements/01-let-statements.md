# Let Statements

r[5.1.1#normative]
A let statement introduces a new variable binding.

r[5.1.2#normative]
```ebnf
let_stmt = "let" [ "mut" ] IDENT [ ":" type ] "=" expression ";" ;
```

## Immutable Bindings

r[5.1.3#normative]
By default, variables are immutable. An immutable variable cannot be reassigned.

r[5.1.4#normative]
```rue
fn main() -> i32 {
    let x = 42;
    x
}
```

## Mutable Bindings

r[5.1.5#normative]
The `mut` keyword creates a mutable binding that can be reassigned.

r[5.1.6]
```rue
fn main() -> i32 {
    let mut x = 10;
    x = 20;
    x
}
```

## Type Annotations

r[5.1.7#normative]
Type annotations are optional when the type can be inferred from the initializer.

r[5.1.8#normative]
When a type annotation is present, the initializer must be compatible with that type.

r[5.1.9]
```rue
fn main() -> i32 {
    let x: i32 = 42;      // explicit type
    let y = 10;           // type inferred as i32
    let z: i64 = 100;     // 100 inferred as i64
    x + y
}
```

## Shadowing

r[5.1.10#normative]
A variable can shadow a previous variable of the same name in the same scope.

r[5.1.11#normative]
When shadowing, the new variable can have a different type.

r[5.1.12]
```rue
fn main() -> i32 {
    let x = 10;
    let x = x + 5;  // shadows previous x
    x  // 15
}
```
