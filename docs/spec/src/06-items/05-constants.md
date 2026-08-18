+++
title = "Constants"
weight = 5
template = "spec/page.html"
+++

# Constants

{{ rule(id="6.5:1", cat="normative") }}

A constant item binds a name, at the top level of a file, to the value
obtained by evaluating its initializer expression at compile time; that
initializer is a comptime-evaluable expression (4.14:26–29, 6.5:2), and the
name denotes that single compile-time value at every use. The grammar is
given in Appendix A (`const_decl`): an optional `pub`, the `const` keyword, a
name, an optional type annotation, and an initializer expression.

{{ rule(id="6.5:2", cat="legality-rule") }}

The initializer of a constant **MUST** be *comptime-evaluable* — a member of
the comptime-evaluable set defined in 4.14:26–29 (the same set governing a
`comptime` argument position). As they arise in a constant initializer, those
forms are: literals (4.14:26); the arithmetic, comparison, logical, bitwise,
and shift operators applied to comptime-evaluable operands (4.14:27);
`comptime` block expressions (4.14:2, 4.14:27); references to other constants
(4.14:26); and module expressions (`@import(...)` and module member access —
chapter 10, 6.5:10). Additionally, a string literal is a valid constant
initializer (6.5:16) even though it is not in the general comptime-evaluable
set: string values exist in constant-initializer position only, not in
`comptime` blocks or `comptime` argument positions. A reference to a function
item is comptime-evaluable only for the purpose of forming a callable alias
(6.5:15). An initializer outside this set is a compile-time error (E0434) —
the same diagnostic 4.14:29 names for a `const` initializer that is not
comptime-evaluable.

{{ rule(id="6.5:3", cat="example") }}

```rue
const NEG: i32 = -5;             // negated literal
const AREA: i32 = 6 * 7;         // constant arithmetic
const DOUBLE: i32 = AREA + AREA; // references another constant
const FLAG: bool = !false;       // boolean operators
```

{{ rule(id="6.5:10", cat="informative") }}

A module member access (`m.CONST`, 10.4:12) is itself compile-time evaluable,
so it composes in any operand position of a constant initializer just like a
reference to a local constant: `const N: i32 = m.LIMIT + 1;` is accepted, as
is the whole-initializer form `const N: i32 = m.LIMIT;`.

{{ rule(id="6.5:15", cat="normative") }}

A constant initializer may name a function item, either directly (`const f =
some_fn;`) or as a module member (`const f = @import("math.rue").abs;`). Such a
constant is a **callable alias**: it may appear as the callee of a call
expression (`f(1, 2)`) and has the same call behavior as the aliased function.
It is not a runtime value and may not be used as an ordinary expression,
stored in a local, passed as an argument, or placed in an aggregate value.

## Types of Constants

{{ rule(id="6.5:4", cat="legality-rule") }}

A *value constant* — a constant whose initializer evaluates to a value
rather than a module — **MUST** have a type annotation. A value constant
declared without one is a compile-time error (E0475). Module bindings
(`const m = @import(...)`, aliases of module bindings, and re-exports —
chapter 10) are not value constants: they take no annotation (no type
annotation can name a module type) and require none. Callable function aliases
(6.5:15) likewise take no annotation because no source-level type names a
function reference.

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

{{ rule(id="6.5:12", cat="legality-rule") }}

If evaluating a constant initializer would trap — an arithmetic operation that
overflows its result type, or a division or remainder by zero — the constant is
rejected at compile time (E1200). The trap is reported as a compile-time error
rather than deferring to a runtime panic, because the initializer is evaluated
during compilation (6.5:2). This applies whether the trapping operation is the
whole initializer or a sub-expression of it.

{{ rule(id="6.5:13", cat="example") }}

```rue
// const OVF: i32 = 2147483647 + 1;  // error: integer overflow (E1200)
// const DIV: i32 = 5 / 0;           // error: division by zero (E1200)
// const REM: i32 = 5 % 0;           // error: remainder by zero (E1200)
const OK: i32 = 2147483647;          // in range: fine
```

{{ rule(id="6.5:14", cat="informative") }}

In the current revision the compile-time-evaluable expression forms (6.5:2)
produce scalar values — integers and `bool` — plus string values from string
literals (6.5:16), so a value constant's type is in practice a scalar type or
`str`. There is no const-evaluable form that yields a user struct or an
array: an aggregate initializer such as a struct literal or an array literal
is not compile-time evaluable and is rejected (E0434). Constants of aggregate
type may be revisited in a future revision.

## String Constants

{{ rule(id="6.5:16", cat="normative") }}

A string-literal initializer declares a *string constant*. A string constant
is always of type `str` — the static, copyable string view (3.7) — and its
annotation **MUST** be `str` (the owning `StrBuf` and fixed `Str(N)`
representations are runtime-only and do not match, 6.5:5). Like every value
constant, a string constant requires a type annotation (6.5:4). A use of a
string constant materializes the same value the string literal itself would
denote at that site: string constants participate in `str` operations,
`println`, and reads exactly like inline literals. A constant initialized
from another string constant (`const B: str = A;`) denotes the same value.
String values remain outside `comptime` blocks and `comptime` argument
positions (6.5:2).

{{ rule(id="6.5:17", cat="example") }}

```rue
const GREETING: str = "hello";
const ALIAS: str = GREETING;

fn main() -> i32 {
    println(GREETING);
    @intCast(GREETING.len() + ALIAS.len())   // 10
}
```

## Evaluation Order

{{ rule(id="6.5:7", cat="normative") }}

A constant initializer may reference constants declared later in the same
file, and constants of other modules through a module binding (`m.LIMIT`);
declaration and file order do not affect the result. Initializers are
evaluated in dependency order. An unqualified reference to a constant
defined only in another file does not resolve (10.3:8).

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
