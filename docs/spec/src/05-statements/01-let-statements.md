+++
title = "Let Statements"
weight = 1
template = "spec/page.html"
+++

# Let Statements

{{ rule(id="5.1:1", cat="normative") }}

A let statement introduces a new variable binding.

{{ rule(id="5.1:2", cat="normative") }}

<!-- grammar-sync(id="5.1:2", production="let_pattern", role="source") -->
<!-- grammar-sync(id="5.1:2", production="struct_pattern", role="source") -->
<!-- grammar-sync(id="5.1:2", production="field_patterns", role="source") -->
<!-- grammar-sync(id="5.1:2", production="field_pattern", role="source") -->

```ebnf
let_stmt = "let" [ "mut" ] let_pattern [ ":" type ] "=" expression ";" ;
let_pattern    = IDENT | "_" | struct_pattern ;
struct_pattern = type "{" [ field_patterns ] "}" ;
field_patterns = field_pattern { "," field_pattern } [ "," ] ;
field_pattern  = [ "mut" ] IDENT
               | IDENT ":" ( [ "mut" ] IDENT | "_" ) ;
```

A `struct_pattern` (5.1:18) is a preview feature; its head is written with
the type grammar (3.1) and so takes the same forms a `let` annotation does.

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

## Struct Patterns

{{ preview_feature(feature="struct_patterns", adr="ADR-0091", doc="0091-struct-patterns.md") }}

{{ rule(id="5.1:18", cat="normative") }}

A let statement **MAY** bind the fields of a struct value with a *struct
pattern*: `let T { f: b, ... } = e;`. The head `T` is written with the type
grammar exactly as a `let` annotation names a type — a struct name, a
module-qualified name, or a type-constructor call (4.14:23) — and names the
struct type of `e`. Each field pattern names one declared field of that struct
and either binds it to a fresh name (`f: b`, or the shorthand `f`, which binds
the field to a name of its own spelling exactly as field-init shorthand names a
struct literal's field), binds it mutably (`f: mut b`, or the shorthand
`mut f`), or discards it (`f: _`). Struct patterns are a preview feature: a let
statement with a struct pattern **MUST** be compiled with
`--preview struct_patterns` (8.4:1).

{{ rule(id="5.1:19", cat="legality-rule") }}

The head of a struct pattern **MUST** name a struct type (E0213), and the
initializer **MUST** have exactly that type (E0206); an explicit annotation on
the statement is checked against the initializer as usual (5.1:8) and so must
name the same type. Mutability belongs to a binding and is written inside the
pattern: `mut` before a struct pattern is a syntax error.

{{ rule(id="5.1:20", cat="legality-rule") }}

A struct pattern has no rest form. It **MUST** name every field the struct
declares, each exactly once: a field the pattern omits is a compile-time error
naming the missing fields (E0400), a name that is not a field of the struct is
E0401, and a field named twice is E0402. Adding a field to a struct is
therefore a compile-time error at every struct pattern over that struct until
the pattern names the field.

{{ rule(id="5.1:21", cat="normative") }}

A struct pattern is the sequence of let statements it stands for. The
initializer is evaluated once and bound to an unnameable temporary as
`let t: T = e;` would bind it (5.1:8, 5.1:12). Then, for each field pattern in
source order, a named binding `f: b` is `let b = t.f;` (`let mut b = t.f;` for
`mut b`) and a discard `f: _` is `let _ = t.f;` (5.1:16). Each binding is
consequently introduced after the whole initializer and after the bindings of
earlier fields (5.1:12), shadows as any let binding does (5.1:10), and takes
its field's value by the rules of field access in value context (4.12): a Copy
field is copied, a move field is moved out of the temporary, a field whose type
carries a linear value cannot be discarded (E0478, 3.8), and a move field of a
struct that has a destructor cannot be moved out (E0456, 3.9). Whatever the
temporary still owns when the enclosing block ends is dropped then (3.9:4).

{{ rule(id="5.1:22") }}

```rue
struct Point { x: i32, y: i32 }

fn manhattan(p: Point) -> i32 {
    let Point { x, y: py } = p;   // binds x and py
    x + py
}

fn main() -> i32 {
    let Point { mut x, y: _ } = Point { x: 40, y: 7 };  // y is discarded
    x = x + 2;
    x   // 42
}
```
