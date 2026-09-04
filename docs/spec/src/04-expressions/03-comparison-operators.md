+++
title = "Comparison Operators"
weight = 3
template = "spec/page.html"
+++

# Comparison Operators

{{ rule(id="4.3:1", cat="normative") }}

A comparison operator takes two operands of the same type and produces a `bool`
(core calculus `docs/formal/01-core-calculus.md` §5.8, rule `(Eq)` for `==`/`!=`;
§6.4, rules `(D-Eq)` and the ordering compares): the value produced is `true`
exactly when the two operand values stand in the operator's relation, and
`false` otherwise.

## Equality Operators

{{ rule(id="4.3:2", cat="normative") }}

Equality operators work on integers, booleans, strings, the unit type, and the
aggregate types: structs, arrays, and enums.

| Operator | Name | Description |
|----------|------|-------------|
| `==` | Equal | True if operands are equal |
| `!=` | Not equal | True if operands are not equal |

{{ rule(id="4.3:3", cat="normative") }}

Two strings are equal if they have the same length and identical byte content.
This holds wherever a string is reached, not only as a whole operand: a string
that is a struct field, an array element, or an enum payload field, at any
depth, is compared by this rule and not by the representation of its header
(a string is a leaf of the structural recursion in rule 4.3:3b, so rule 4.3:3e
never applies to the pointer inside one).

{{ rule(id="4.3:3a", cat="normative") }}

Two unit values are always equal.

{{ rule(id="4.3:3b", cat="normative") }}

Equality on the aggregate types is **structural**: two aggregate values are
equal if and only if they have the same type and their components — determined
recursively by this rule down to scalar leaves — are equal (core calculus
`docs/formal/01-core-calculus.md` §6.4, the structural-equality relation `≈` of
rule `(D-Eq)`). Specifically, two struct values are equal if and only if they
have the same struct type and all corresponding fields are equal.

{{ rule(id="4.3:3c", cat="normative") }}

Two array values are equal if and only if they have the same element type and
length and their elements are equal index-by-index. (Two array types of
different lengths are distinct types and cannot be compared; see rule 4.3:10.)

{{ rule(id="4.3:3d", cat="normative") }}

Two enum values are equal if and only if they have the same enum type, are the
same variant, and — for a variant carrying a payload — their payload fields are
equal field-by-field. Two values of different variants are never equal.

{{ rule(id="4.3:3e", cat="normative") }}

A raw-pointer leaf (a `*const T` or `*mut T` field or element reached while
comparing an aggregate) compares by **address**: two raw pointers are equal if
and only if they hold the same address. The pointees are not examined.

{{ rule(id="4.3:3f", cat="dynamic-semantics") }}

Equality **borrows** its operands: evaluating `a == b` or `a != b` reads both
operands without consuming them. When an operand is a named place it is read
through a comparison-scoped shared loan rather than moved (core calculus
`docs/formal/01-core-calculus.md` §4.1, §5.4, and the operand side condition of
rule `(Eq)` in §5.8; dynamically §6.3). An affine or linear value may therefore
be compared without discharging its move obligation, and both operands remain
usable afterward.

{{ rule(id="4.3:3g", cat="informative") }}

Equality is structural **by default**. Trait-based refinement of equality —
opting a type out of comparison, or giving it a user-defined equality (a
`PartialEq`-style mechanism) — is deferred until traits exist (RUE-246). A
related future consideration, once floating-point types exist, is the
partial-equality of `NaN` (where `NaN != NaN`), which motivates the eventual
`PartialEq`/`Eq` split; today no leaf type has such a value, so structural
equality is a total equivalence.

{{ rule(id="4.3:4") }}

```rue
fn main() -> i32 {
    let a = 1 == 1;    // true
    let b = 1 != 2;    // true
    let c = true == false;  // false (bool equality)
    let d = "hello" == "hello";  // true (string equality)
    let e = () == ();  // true (unit equality)
    if a && b && !c && d && e { 1 } else { 0 }
}
```

{{ rule(id="4.3:4a", cat="example") }}

```rue
struct Point { x: i32, y: i32 }

fn main() -> i32 {
    let p1 = Point { x: 1, y: 2 };
    let p2 = Point { x: 1, y: 2 };
    let p3 = Point { x: 1, y: 3 };
    if p1 == p2 && p1 != p3 { 1 } else { 0 }
}
```

{{ rule(id="4.3:4b", cat="example") }}

Arrays compare element-by-element; nested aggregates recurse. Comparing does
not consume the operands, so `a` and `b` remain usable afterward.

```rue
fn main() -> i32 {
    let a = [1, 2, 3];
    let b = [1, 2, 3];
    let equal = a == b;      // borrows a and b
    if equal && a[0] == b[0] { 1 } else { 0 }
}
```

## Ordering Operators

{{ rule(id="4.3:5", cat="normative") }}

Ordering operators work only on integers. They compare the two operands by their
integer values, respecting the signedness of the shared operand type — a signed
type orders negatives below non-negatives, an unsigned type orders by magnitude
(core calculus `docs/formal/01-core-calculus.md` §6.4: scalars compare by their
integer value, respecting signedness).

| Operator | Name | Description |
|----------|------|-------------|
| `<` | Less than | True if left < right |
| `>` | Greater than | True if left > right |
| `<=` | Less or equal | True if left <= right |
| `>=` | Greater or equal | True if left >= right |

{{ rule(id="4.3:6", cat="legality-rule") }}

Ordering operators on boolean, string, unit, or aggregate (struct, array, or
enum) values are a compile-time error. Implementations **MUST** reject such
programs.

{{ rule(id="4.3:7") }}

```rue
fn main() -> i32 {
    let a = 1 < 2;     // true
    let b = 5 >= 5;    // true
    if a && b { 1 } else { 0 }
}
```

## Precedence

{{ rule(id="4.3:8", cat="normative") }}

Comparison operators have lower precedence than arithmetic, shift, and bitwise operators, and higher precedence than the logical operators `&&` and `||`. (The complete precedence ladder, which matches Rust's, is given in rule 4.3a:13.)

{{ rule(id="4.3:9") }}

```rue
fn main() -> i32 {
    if 1 + 2 == 3 { 1 } else { 0 }  // 1 (comparison after arithmetic)
}
```

## Type Checking

{{ rule(id="4.3:10", cat="legality-rule") }}

Both operands of a comparison **MUST** have the same type.

{{ rule(id="4.3:11", cat="normative") }}

When one operand has a known type, the other is inferred to have the same type.

## Associativity

{{ rule(id="4.3:12", cat="legality-rule") }}

Comparison operators cannot be chained. Expressions like `a < b < c` or `a == b == c` are compile-time errors. The restriction is syntactic: it applies when a comparison expression is directly an operand of another comparison. Explicit parentheses break a chain — `(a < b) == c` is an ordinary equality whose left operand is a parenthesized boolean expression, and is legal whenever its operand types are (a parenthesized boolean operand of an ordered comparison such as `(a < b) < c` is instead rejected by the ordinary operand typing rules).

{{ rule(id="4.3:13", cat="example") }}

To compare multiple values, use logical operators:

```rue
fn main() -> i32 {
    let a = 1;
    let b = 2;
    let c = 3;
    if a < b && b < c { 1 } else { 0 }  // correct way to chain comparisons
}
```
