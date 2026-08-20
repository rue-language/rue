+++
title = "Assignment Statements"
weight = 2
template = "spec/page.html"
+++

# Assignment Statements

{{ rule(id="5.2:1", cat="normative") }}

An assignment statement evaluates its right-hand side to a value and stores that value into the *place* named by its target (the target is a place, not merely a variable; see 5.2:2). If the target place currently holds a live (owned) value, that value is dropped before the new value is stored (overwrite-drop); storing into a moved-out place reinitializes it. The assignment itself evaluates to unit. These ownership effects are those of the core calculus's `assign p = e` form (`docs/formal/01-core-calculus.md` §5.2, §6.8) and of 3.8:55–56.

{{ rule(id="5.2:2", cat="normative") }}

```ebnf
assign_stmt   = assign_target "=" expression ";" ;
assign_target = place_expr ;
place_expr   = ( IDENT | "self" ) { place_postfix } ;
place_postfix = "." IDENT | "[" expression "]"
              | "." IDENT "(" [ call_args ] ")" ;
```

The assignment target is a *place*: a variable (or, inside a method, `self`)
followed by any number of field (`.f`) and index (`[e]`) projections, freely
mixed. An exclusive place-returning accessor call may serve as a place root,
so `v.get_mut(i)`, `v.get_mut(i).field`, and `v.get_mut(i)[j]` are also valid
targets; nested method-call links are allowed when each resolves to an
exclusive accessor. Ordinary value-returning or shared method calls are not
assignment places and remain a semantic error. All of `x`, `p.f`, `arr[i]`,
`arr[i].f`, `p.arr[i]`, `a.b.c`, and `o.items[i].arr[j]` are valid targets.
(Appendix A's `place_expr` is the normative statement of this production; see
that appendix.)

## Evaluation Order

{{ rule(id="5.2:14", cat="dynamic-semantics") }}

In an assignment `target = expression`, the right-hand side `expression` is
evaluated first to produce the value to be stored. The assignment target names
a *place*; any index subexpressions appearing in the target (the `[e]` in an
`arr[e]` target) are evaluated after the right-hand side, in source order
(left-to-right). Once the place has been resolved, the produced value is
written into it. This matches the core calculus's evaluation order: the
assignment context `assign p = E` reduces the right-hand side `E` first, and
the target place's index subexpressions are reduced as part of resolving the
place for the store (`docs/formal/01-core-calculus.md` §6.2, §6.8).

{{ rule(id="5.2:15") }}

```rue
fn tap(n: i32) -> i32 { @dbg(n); n }

fn main() -> i32 {
    let mut arr: [i32; 4] = [0, 0, 0, 0];
    // Prints 2 (the right-hand side) before 1 (the index of the place).
    arr[tap(1)] = tap(2);
    arr[1]  // 2
}
```

## Variable Assignment

{{ rule(id="5.2:3", cat="legality-rule") }}

The variable **MUST** have been declared with `let mut`.

{{ rule(id="5.2:4", cat="legality-rule") }}

The expression type **MUST** be compatible with the variable's type.

{{ rule(id="5.2:5") }}

```rue
fn main() -> i32 {
    let mut x = 0;
    x = 42;
    x
}
```

## Array Element Assignment

{{ rule(id="5.2:6", cat="normative") }}

Array element assignment requires a mutable array.

{{ rule(id="5.2:7") }}

```rue
fn main() -> i32 {
    let mut arr: [i32; 2] = [0, 0];
    arr[0] = 20;
    arr[1] = 22;
    arr[0] + arr[1]
}
```

## Struct Field Assignment

{{ rule(id="5.2:8", cat="normative") }}

Struct field assignment requires a mutable struct value.

{{ rule(id="5.2:9") }}

```rue
struct Point { x: i32, y: i32 }

fn main() -> i32 {
    let mut p = Point { x: 0, y: 0 };
    p.x = 42;
    p.x
}
```

### Nested Field Assignment

{{ rule(id="5.2:11", cat="normative") }}

Fields of nested structs can be assigned through chained field access.

{{ rule(id="5.2:12", cat="normative") }}

All struct values in the chain **MUST** be part of a mutable binding.

{{ rule(id="5.2:13") }}

```rue
struct Inner { value: i32 }
struct Outer { inner: Inner }

fn main() -> i32 {
    let mut o = Outer { inner: Inner { value: 0 } };
    o.inner.value = 42;
    o.inner.value
}
```

## Compound Assignment

{{ rule(id="5.2:16", cat="syntax") }}

```ebnf
compound_stmt = assign_target compound_op expression ";" ;
compound_op   = "+=" | "-=" | "*=" | "/=" | "%="
              | "&=" | "|=" | "^=" | "<<=" | ">>=" ;
```

A compound assignment applies a binary operator to the value already held in
the target place and stores the result back into that place. The ten operators
above are exactly the binary operators whose result has the type of their left
operand; the comparison and short-circuiting logical operators have no compound
form.

{{ rule(id="5.2:17", cat="normative") }}

`place op= value` means `place = place op value`: the operator's operands are
the value read from the place and the value of `value`, the operator is the one
named by `op` (4.2, 4.3a), and the result is stored back into the place.
Legality, typing, and every runtime effect are those of that expanded form —
including the requirement that the place be assignable (5.2:3), that the
operator apply to the operand types (5.2:4), and the operator's own overflow
(8.1), bounds-check (8.2), and division-by-zero (8.3) behavior. Compound
assignment introduces no new core form: it
denotes the same `assign p = e` reduction as 5.2:1 (`docs/formal/01-core-calculus.md`
§5.2, §6.8), with `e` the applied operator.

{{ rule(id="5.2:18", cat="dynamic-semantics") }}

The target place is evaluated **exactly once**. Any index subexpression in the
target (the `[e]` in an `arr[e]` target) is evaluated once, before the
right-hand side and in source order (left to right); the place is then read,
the operator applied, and the result written back through that same place. This
is the one respect in which `place op= value` is not interchangeable with
`place = place op value`, which evaluates the target's index subexpressions a
second time (5.2:14).

{{ rule(id="5.2:19") }}

```rue
fn tap(n: u64) -> u64 { @dbg(n); n }

fn main() -> i32 {
    let mut arr: [i32; 4] = [0, 0, 0, 40];
    // `tap(3)` runs once, not once per mention of the place: prints 3 only.
    arr[tap(3)] += 2;
    arr[3]  // 42
}
```

{{ rule(id="5.2:20") }}

```rue
struct Counter { hits: i32 }

fn main() -> i32 {
    let mut c = Counter { hits: 16 };
    let mut arr: [i32; 2] = [1, 2];
    c.hits += 1;
    c.hits *= 2;
    arr[0] <<= 3;
    arr[1] -= 2;
    c.hits + arr[0] + arr[1]  // 34 + 8 + 0
}
```

## Assignment is Not an Expression

{{ rule(id="5.2:10", cat="legality-rule") }}

Assignment is a statement, not an expression. It **MUST NOT** be used in
expression position. This holds for the compound forms as well: `place op=
value` is a statement and produces no value.
