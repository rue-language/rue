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
constants, and module expressions (`@import(...)` and module member access
— chapter 10). An initializer that is not compile-time evaluable produces a
compile-time error (E0434).

{{ rule(id="6.5:3", cat="example") }}

```rue
const NEG: i32 = -5;             // negated literal
const AREA = 6 * 7;              // constant arithmetic
const DOUBLE = AREA + AREA;      // references another constant
const FLAG = !false;             // boolean operators
```

{{ rule(id="6.5:10", cat="informative") }}

In the current implementation, a module member access (`m.CONST`, 10.4:12)
is compile-time evaluable only as a whole initializer, not as the operand of
an operator: write `const BASE = m.LIMIT; const N = BASE + 1;` rather than
`const N = m.LIMIT + 1;`. This restriction is an implementation artifact,
not a language guarantee.

## Types of Constants

{{ rule(id="6.5:4", cat="normative") }}

An integer constant without a type annotation has the smallest of the types
`i32`, `i64`, `u64` that can represent its value. A boolean constant has
type `bool`; a unit constant has type `()`.

{{ rule(id="6.5:5", cat="legality-rule") }}

An integer constant with a type annotation has the annotated type; its value
**MUST** be representable in that type, and a value out of range is a
compile-time error reported at the declaration. A non-integer annotation
**MUST** match the type of the initializer's value exactly.

{{ rule(id="6.5:6", cat="example") }}

```rue
const BIG = 5000000000;          // i64: does not fit i32
const HUGE = 18446744073709551615; // u64: does not fit i64
const SMALL: u8 = 200;           // u8: annotation adopted, value fits
// const BAD: u8 = 300;          // error: out of range for u8 (E0800)
```

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
const A = B + 1;                 // forward reference: fine
const B = 2;
// const X = Y; const Y = X;     // error: cycle X -> Y -> X (E0461)
```
