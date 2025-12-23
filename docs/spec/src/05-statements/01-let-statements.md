+++
title = "Let Statements"
weight = 1
template = "spec/page.html"
+++

# Let Statements

{{ rule(id="5.1:1", cat="normative") }}

A let statement introduces a new variable binding.

{{ rule(id="5.1:2", cat="normative") }}

```ebnf
let_stmt = "let" [ "mut" ] IDENT [ ":" type ] "=" expression ";" ;
```

## Immutable Bindings

{{ rule(id="5.1:3", cat="normative") }}

By default, variables are immutable. An immutable variable cannot be reassigned.

{{ rule(id="5.1:4", cat="normative") }}

```rue
fn main() -> i32 {
    let x = 42;
    x
}
```

## Mutable Bindings

{{ rule(id="5.1:5", cat="normative") }}

The `mut` keyword creates a mutable binding that can be reassigned.

{{ rule(id="5.1:6") }}

```rue
fn main() -> i32 {
    let mut x = 10;
    x = 20;
    x
}
```

## Type Annotations

{{ rule(id="5.1:7", cat="normative") }}

Type annotations are optional when the type can be inferred from the initializer.

{{ rule(id="5.1:8", cat="normative") }}

When a type annotation is present, the initializer must be compatible with that type.

{{ rule(id="5.1:9") }}

```rue
fn main() -> i32 {
    let x: i32 = 42;      // explicit type
    let y = 10;           // type inferred as i32
    let z: i64 = 100;     // 100 inferred as i64
    x + y
}
```

## Shadowing

{{ rule(id="5.1:10", cat="normative") }}

A variable can shadow a previous variable of the same name in the same scope.

{{ rule(id="5.1:11", cat="normative") }}

When shadowing, the new variable can have a different type.

{{ rule(id="5.1:12", cat="normative") }}

The scope of a binding introduced by a let statement begins after the complete let statement, including its initializer. The initializer expression is evaluated before the new binding is introduced, so references to a shadowed name within the initializer resolve to the previous binding.

{{ rule(id="5.1:13") }}

```rue
fn main() -> i32 {
    let x = 10;
    let x = x + 5;  // shadows previous x, initializer uses old x
    x  // 15
}
```
