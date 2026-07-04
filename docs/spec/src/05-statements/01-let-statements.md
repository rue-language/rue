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
let_stmt = "let" [ "mut" ] let_pattern [ ":" type ] "=" expression ";" ;
let_pattern = IDENT | "_" ;
```

## Immutable Bindings

{{ rule(id="5.1:3", cat="legality-rule") }}

By default, variables are immutable. An immutable variable **MUST NOT** be reassigned.

{{ rule(id="5.1:4", cat="normative") }}

```rue
fn main() -> i32 {
    let x = 42;
    x
}
```

## Mutable Bindings

{{ rule(id="5.1:5", cat="normative") }}

The `mut` keyword creates a mutable binding that **MAY** be reassigned.

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

{{ rule(id="5.1:8", cat="legality-rule") }}

When a type annotation is present, the initializer **MUST** be compatible with that type.

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

A variable **MAY** shadow a previous variable of the same name in the same scope.

{{ rule(id="5.1:11", cat="normative") }}

When shadowing, the new variable **MAY** have a different type.

{{ rule(id="5.1:12", cat="normative") }}

The scope of a binding introduced by a let statement begins after the complete let statement, including its initializer. The initializer expression is evaluated before the new binding is introduced, so references to a shadowed name within the initializer resolve to the previous binding. This is exactly the core calculus's `let x = e1 ; e2` form (`docs/formal/01-core-calculus.md` §6.7, rule `(D-Let)`): the initializer `e1` is reduced to a value before the cell for `x` is bound in the environment, so `x` is not in scope while `e1` is evaluated.

{{ rule(id="5.1:13") }}

```rue
fn main() -> i32 {
    let x = 10;
    let x = x + 5;  // shadows previous x, initializer uses old x
    x  // 15
}
```

{{ rule(id="5.1:14", cat="normative") }}

A let binding **MAY** shadow a function parameter of the same name, following
the same rules as shadowing a previous let binding (5.1:10–5.1:12): the
initializer is evaluated before the new binding is introduced, so a reference to
the name in the initializer resolves to the parameter, and the new binding **MAY**
have a different type.

{{ rule(id="5.1:15") }}

```rue
fn f(x: i32) -> i32 {
    let x = x + 100;  // shadows the parameter; initializer reads the parameter
    x
}

fn main() -> i32 { f(5) }  // 105
```

## Wildcard Bindings

{{ rule(id="5.1:16", cat="normative") }}

The wildcard `_` **MAY** appear in place of the binding name. `let _ = e;`
evaluates `e` and discards its value exactly as an expression statement would
(5.3): it introduces no binding, and `_` **MUST NOT** be referred to as a value.
Because `_` discards rather than consumes, a discarded value of a type that
carries a linear value (3.8) is not thereby consumed; discarding a linear value
this way is a compile-time error (E0478). A discarded value of a Copy or affine
type is dropped in place, and an affine value is moved out of any place named in
`e` just as by-value use elsewhere.

{{ rule(id="5.1:17") }}

```rue
fn main() -> i32 {
    let _ = 5 + 5;   // evaluated, then discarded; no binding introduced
    let _ = 99;
    3
}
```
