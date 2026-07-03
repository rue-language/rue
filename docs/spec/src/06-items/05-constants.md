+++
title = "Constants"
weight = 5
template = "spec/page.html"
+++

# Constants

{{ rule(id="6.5:1", cat="normative") }}

A constant item binds a name to a compile-time value at the top level of a
file. The grammar is given in Appendix A (`const_decl`): an optional `pub`,
the `const` keyword, a name, an optional type annotation, and an initializer
expression.

{{ rule(id="6.5:2", cat="legality-rule") }}

The initializer of a constant **MUST** be evaluable at compile time.
Compile-time evaluable expressions include literals, the arithmetic,
comparison, logical, bitwise, and shift operators applied to compile-time
evaluable operands, `comptime` block expressions, references to other
constants, module expressions (`@import(...)` and module member access
— chapter 10), and type expressions (type names and calls to comptime
functions returning `type`, 6.5:12). An initializer that is not compile-time
evaluable produces a compile-time error (E0434).

{{ rule(id="6.5:3", cat="example") }}

```rue
const NEG: i32 = -5;             // negated literal
const AREA: i32 = 6 * 7;         // constant arithmetic
const DOUBLE: i32 = AREA + AREA; // references another constant
const FLAG: bool = !false;       // boolean operators
```

{{ rule(id="6.5:10", cat="informative") }}

In the current implementation, a module member access (`m.CONST`, 10.4:12)
is compile-time evaluable only as a whole initializer, not as the operand of
an operator: write `const BASE: i32 = m.LIMIT; const N: i32 = BASE + 1;`
rather than `const N: i32 = m.LIMIT + 1;`. This restriction is an
implementation artifact, not a language guarantee.

## Types of Constants

{{ rule(id="6.5:4", cat="legality-rule") }}

A *value constant* — a constant whose initializer evaluates to a value
rather than a module — **MUST** have a type annotation. A value constant
declared without one is a compile-time error (E0475). Module bindings
(`const m = @import(...)`, aliases of module bindings, and re-exports —
chapter 10) are not value constants: they take no annotation (no type
annotation can name a module type) and require none.

{{ rule(id="6.5:11", cat="informative") }}

Earlier drafts inferred an unannotated integer constant's type from its
value (the smallest of `i32`, `i64`, `u64` that could represent it). That
inference was removed in favor of explicit annotations; some form of
inference for constants may be revisited in a future revision. The E0475
diagnostic suggests the annotation the removed inference would have chosen.

{{ rule(id="6.5:5", cat="legality-rule") }}

An integer constant's value **MUST** be representable in its annotated
type; a value out of range is a compile-time error reported at the
declaration. A non-integer annotation **MUST** match the type of the
initializer's value exactly.

{{ rule(id="6.5:6", cat="example") }}

```rue
const BIG: i64 = 5000000000;     // i64: annotation required and adopted
const HUGE: u64 = 18446744073709551615; // u64: largest u64 value
const SMALL: u8 = 200;           // u8: annotation adopted, value fits
// const BAD: u8 = 300;          // error: out of range for u8 (E0800)
// const NONE = 5;               // error: missing type annotation (E0475)
```

## Type Constants

{{ rule(id="6.5:12", cat="normative") }}

A *type constant* is a constant whose initializer evaluates to a type: a type
name (`const Int = i32;`), a call to a comptime function returning `type`
(`const R = Result(i32, i32);`, 4.14:7), or an alias of another type constant.
Like a module binding, a type constant is not a value constant: it takes an
optional `type` annotation (`const R: type = ...;`) and, unlike a value
constant, requires none. A non-`type` annotation on a type-valued initializer
is a type error.

{{ rule(id="6.5:13", cat="normative") }}

A type constant may be used wherever a type name may appear — as a type
annotation, and as a function parameter or return type — resolving to the
concrete (monomorphized) type its initializer names. This lets a comptime
type function's result cross a function boundary in the signature, so a
function can take or return a generic `Option`/`Result` named through a
constant. A `let`-bound type alias of the same name inside a function body
shadows the constant.

{{ rule(id="6.5:14", cat="example") }}

```rue
fn Option(comptime T: type) -> type { enum { Some(T), None } }
const OptI: type = Option(i32);

fn unwrap_or(o: OptI, d: i32) -> i32 {
    match o { OptI::Some(v) => v, OptI::None => d }
}

fn main() -> i32 {
    unwrap_or(OptI::Some(42), 0)  // 42
}
```

{{ rule(id="6.5:15", cat="informative") }}

In the current implementation, a type constant used in a function *signature*
(a parameter or return type) must be declared before that function, since
signature types are resolved as each function is collected. This ordering
restriction is an implementation artifact, not a language guarantee: unlike
6.5:7 (which governs a constant's own value), it applies only to signature-
position uses of a type constant.

## Evaluation Order

{{ rule(id="6.5:7", cat="normative") }}

A constant initializer may reference constants declared later in the same
file or in another file of the program; declaration and file order do not
affect the result. Initializers are evaluated in dependency order.

{{ rule(id="6.5:8", cat="legality-rule") }}

Constant initializers **MUST NOT** form a reference cycle. A cycle —
including a constant that references itself — is a compile-time error
(E0461), not an evaluation loop.

{{ rule(id="6.5:9", cat="example") }}

```rue
const A: i32 = B + 1;            // forward reference: fine
const B: i32 = 2;
// const X: i32 = Y; const Y: i32 = X; // error: cycle X -> Y -> X (E0461)
```
