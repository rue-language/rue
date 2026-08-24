+++
title = "Type Inference"
weight = 11
template = "spec/page.html"
+++

# Type Inference

Rue determines the type of every expression through *local type inference*: a
constraint-based algorithm derived from Hindley–Milner inference (Algorithm W;
see ADR-0007) that runs over one function body at a time. This section specifies
that algorithm — the constraint model, how an expected type flows inward, which
positions require an explicit annotation, and how inference interacts with
comptime — so that an independent implementation infers the same types and
accepts the same programs.

{{ rule(id="3.11:1") }}

The integer-literal rules of this section (defaulting to `i32`, unify-then-
default) are stated normatively in [Integer Literal Type Inference](@/03-types/01-integer-types.md#integer-literal-type-inference)
(3.1:14, 3.1:15); this section describes the surrounding algorithm they are part
of.

## Locality

{{ rule(id="3.11:2", cat="normative") }}

Type inference operates on one function body in isolation. This one-body scope
is the permanent maximum inference scope, not merely an implementation or
query-locality choice. Type information does not flow across a function
boundary: a callee's parameter and return types are not inferred from its call
sites, and a caller does not observe inference variables of a callee. To make
each boundary self-describing, every function parameter, every function return
type, and every value constant (6.5) **MUST** carry an explicit type
annotation; these annotations are semantic firebreaks. A `let` binding (5.1)
**MAY** omit its annotation, in which case its type is determined by inference
from its initializer and later uses. Future closures, trait systems, overloads,
and generics do not widen this boundary without a new language-design decision.

{{ rule(id="3.11:3") }}

```rue
fn main() -> i32 {
    let a = 1;        // inferred, no annotation needed
    let b = a + 2;    // inferred from a
    b
}
```

## Constraint Model

{{ rule(id="3.11:4", cat="normative") }}

The compiler assigns a fresh type variable to each expression whose type is not
syntactically determined, and generates equality constraints from the structure
of the program: the two operands of a binary operator share a type, a call
argument has its parameter's type, a `let` initializer has its annotation's
type, an array element has the array's element type, and a returned expression
has the function's return type. All constraints collected from a function body
are solved together, and the resulting types **MUST NOT** depend on the textual
order in which the constraints were generated. In particular, an unannotated
integer literal (3.1:14) unifies with every use of its value across the body,
and two uses that demand different integer types are a compile-time error
(E0206) whichever use is written first.

{{ rule(id="3.11:5") }}

```rue
fn takes_i64(v: i64) -> i64 { v }

fn main() -> i32 {
    let c = 7;              // no type yet
    let _d = takes_i64(c);  // this use fixes c (and 7) to i64, order-independently
    0
}
```

## Expected Types (Bidirectional Propagation)

{{ rule(id="3.11:6", cat="normative") }}

When the surrounding context fixes the type an expression must produce, that
type is supplied to the expression as its *expected type* and flows inward to
the expression's sub-expressions. The contexts that impose an expected type are:
a `let` initializer under a type annotation, an argument at a call (the
parameter type), an element of an array literal under an array-typed annotation,
the operand of a `return` and a block's tail expression (the return type), and a
`match` scrutinee under typed arm patterns. An unannotated integer literal in an
expected-type position takes the expected integer type rather than defaulting
(3.1:15).

{{ rule(id="3.11:7") }}

```rue
fn main() -> i32 {
    // The annotation makes [i64; 3] the expected type, so each element literal
    // — including 3000000000, which does not fit i32 — is inferred as i64.
    let a: [i64; 3] = [1, 2, 3000000000];
    if a[2] == 3000000000 { 0 } else { 1 }
}
```

## Compatibility

{{ rule(id="3.11:8", cat="legality-rule") }}

When an expected type is imposed on an expression, the expression's inferred
type **MUST** be compatible with it (1.1): the two types are equal, or the
inferred type coerces to the expected type. There are no implicit conversions
between distinct concrete integer types; an incompatible type is a compile-time
error (E0206).

{{ rule(id="3.11:9", cat="normative") }}

The only coercion applied during inference is the never type coercing to any
type (3.4): a diverging expression (type `!`) is accepted in any expected-type
position. The resolution of an unannotated integer literal to a concrete integer
type (3.1:14) is inference, not a coercion — the literal has no fixed type until
it is solved.

{{ rule(id="3.11:10") }}

```rue
fn diverge() -> ! { loop {} }

fn main() -> i32 {
    // The else branch has type `!`, which coerces to the i32 expected here.
    let x: i32 = if true { 5 } else { diverge() };
    x
}
```

## Interaction with Comptime

{{ rule(id="3.11:11", cat="normative") }}

A `comptime T: type` parameter (4.14) is substituted with its concrete type
argument before the corresponding runtime argument is inferred, and the
substituted type becomes that argument's expected type (3.11:6). A literal
passed at a `type`-parameterized position is therefore inferred at the
substituted type. A `comptime { … }` block contributes a value whose type is the
concrete, already-known type of the block's tail expression; it is not an
inference variable.

{{ rule(id="3.11:12") }}

```rue
fn identity(comptime T: type, x: T) -> T { x }

fn main() -> i32 {
    // T is substituted with i64, so 3000000000 is inferred as i64, not i32.
    let r: i64 = identity(i64, 3000000000);
    if r == 3000000000 { 0 } else { 1 }
}
```
