+++
title = "Floating-Point Types"
weight = 12
template = "spec/page.html"
+++

# Floating-Point Types

## The Floating-Point Types

{{ rule(id="3.12:1", cat="normative") }}

A floating-point type is one of `f32` or `f64`. The type `f32` represents the
IEEE 754-2019 `binary32` format, and the type `f64` represents the `binary64`
format. Each denotes exactly the values of its format: the finite numbers, the
two infinities, and the NaNs, with `-0.0` and `+0.0` distinct values.

{{ rule(id="3.12:2", cat="normative") }}

`f32` and `f64` are ordinary identifiers naming builtin types, not keywords.
They may be written wherever a type is named — `let` annotations, function
parameters and results, `const` annotations, struct fields, array element
types — and, like the integer type names, may be used as `type` values
(`@size_of(f64)`).

{{ rule(id="3.12:3", cat="normative") }}

`comptime_float` is the compile-time-only type of a float literal (ADR-0025),
the floating-point counterpart of `comptime_int`. Like every comptime-only
type it has no runtime representation: a `comptime_float` reaches run time
only after it has been given a concrete floating-point type by 3.12:7 or
3.12:8. It is a type the implementation infers, never one a program writes:
`comptime_float` is not among the type names of 3.12:2, and — exactly as for
`comptime_int` — a program that writes it in a type position, including the
type of a `comptime` parameter, is rejected as an unknown type (`E0204`). A
value that must be both compile-time known and floating-point is declared
`f32` or `f64`, which fixes its width while keeping its exact value.

{{ rule(id="3.12:4", cat="example") }}

```rue
struct Vec2 { x: f32, y: f32 }

fn scale(v: f32, k: f32) -> f32 { v * k }

fn main() -> i32 {
    let width: f64 = 2.5;
    let point = Vec2 { x: 1.0, y: 2.0 };
    let row: [f64; 2] = [width, 0.5];
    @dbg(scale(point.x, 3.0));
    @dbg(row[1]);
    0
}
```

## Float Literals

{{ rule(id="3.12:5", cat="syntax") }}

A float literal is a decimal digit run followed by a fraction (a `.` and at
least one further digit), an exponent, or both. The exponent marker is `e` or
`E`, takes an optional `+` or `-` sign, and is followed by at least one digit.
Underscores may separate digits anywhere inside any of the three digit runs and
do not affect the value. There are no floating-point literal suffixes.

```ebnf
float_literal  = dec_literal ( float_fraction [ float_exponent ]
                             | float_exponent ) ;
float_fraction = "." dec_literal ;
float_exponent = ( "e" | "E" ) [ "+" | "-" ] dec_literal ;
```

`dec_literal` is the decimal digit run of 2.1:3.

{{ rule(id="3.12:6", cat="legality-rule") }}

A float literal **MUST NOT** begin with `.` or end with `.`, and an exponent
marker **MUST** be followed by at least one digit. `.5`, `5.`, and `1e` are
compile-time errors; write `0.5`, `5.0`, and `1e0`.

{{ rule(id="3.12:7", cat="normative") }}

A float literal has type `comptime_float`, and takes a concrete floating-point
type from its context: a type annotation, the other operand of a
floating-point binary operator, the parameter type of a call it is an argument
to, or the declared result type of the function it is returned from.

{{ rule(id="3.12:8", cat="normative") }}

A float literal whose context supplies no floating-point type has type `f64`.

{{ rule(id="3.12:9", cat="normative") }}

Giving a float literal a concrete type rounds its exact decimal value to the
nearest value of that type, with ties resolved to the even significand (the
IEEE 754 default rounding attribute). The literal's written text is converted
exactly: no intermediate width is interposed, so a literal that is
representable in the target type denotes exactly that value.

{{ rule(id="3.12:10", cat="legality-rule") }}

A float literal whose value rounds to an infinity in its target type **MUST**
be rejected at compile time (`E0206`). Rounding *down* to zero is not an
error: a literal smaller in magnitude than the target's smallest subnormal
denotes a zero.

{{ rule(id="3.12:11", cat="normative") }}

An integer literal is accepted wherever a floating-point type is expected, and
denotes the value of 3.12:9 applied to its exact integer value, so
`let x: f64 = 3;` binds `3.0`. The converse does not hold: a float literal is
never accepted where an integer type is expected, however integral its value.

{{ rule(id="3.12:12", cat="example") }}

```rue
fn takes_f32(v: f32) -> f32 { v }

fn main() -> i32 {
    let a = 1.0;             // no context: f64 (3.12:8)
    let b: f32 = 0.1;        // rounded to the nearest f32 (3.12:9)
    let c: f64 = 1_000.5;    // separators are ignored
    let d: f64 = 1.5e-3;     // exponent form
    let e: f64 = 3;          // integer literal in float position (3.12:11)
    @dbg(a);
    @dbg(takes_f32(b));
    @dbg(c + d + e);
    0
}
```

## No Implicit Conversions

{{ rule(id="3.12:13", cat="legality-rule") }}

Both operands of a floating-point arithmetic or comparison operator **MUST**
have the same floating-point type. There is no implicit widening: mixing an
`f32` operand with an `f64` operand is a compile-time error (`E0206`).

{{ rule(id="3.12:14", cat="legality-rule") }}

There is no implicit conversion between an integer type and a floating-point
type in either direction. An expression that mixes an integer operand with a
floating-point operand in an arithmetic or comparison operator is a
compile-time error (`E0206`).

{{ rule(id="3.12:15", cat="informative") }}

Every width or domain change is therefore written explicitly, with one of the
three conversion intrinsics of the next section. This is the same discipline
Rue applies to integers, where `@intCast` is required between integer widths,
and it leaves the never-type coercion of 3.4:3 the language's only coercion:
giving a literal a type (3.12:7, 3.12:11) is literal typing performed during
inference, not a conversion of a value that already has one.

## Conversion Intrinsics

{{ rule(id="3.12:16", cat="normative") }}

`@int_to_float(x)` converts an operand of any integer type to a floating-point
type taken from context. Its result is the operand's exact integer value
rounded to nearest, ties to even, at the result type. It never traps.

{{ rule(id="3.12:17", cat="dynamic-semantics") }}

`@float_to_int(x)` converts a floating-point operand to an integer type — signed
or unsigned — taken from context. It discards the operand's fractional part,
rounding toward zero, and produces that integral value.

{{ rule(id="3.12:18", cat="dynamic-semantics") }}

`@float_to_int(x)` traps exactly when the operand is a NaN or the value
truncated toward zero is not representable in the result type — equivalently,
it succeeds exactly when `MIN - 1 < x < MAX + 1` for the result type's bounds,
which admits both infinities as failures. The trap is the integer-overflow
runtime panic of §8.1: the program terminates with exit code 101 after
reporting `integer overflow`.

{{ rule(id="3.12:19", cat="normative") }}

`@float_cast(x)` converts between `f32` and `f64`, in either direction, with
the result type taken from context. Widening `f32` to `f64` is exact.
Narrowing `f64` to `f32` rounds to nearest, ties to even, and yields an
infinity when the operand's magnitude is too large for `f32`. `@float_cast`
never traps.

{{ rule(id="3.12:20", cat="example") }}

```rue
fn main() -> i32 {
    let n: i32 = 7;
    let f: f64 = @int_to_float(n);       // 7.0
    let g: f32 = @float_cast(f);         // 7.0 as an f32, exact here
    let back: i32 = @float_to_int(2.9);  // 2, truncated toward zero
    let neg: i32 = @float_to_int(-2.9);  // -2, truncated toward zero
    @dbg(f);
    @dbg(g);
    @dbg(back + neg);
    0
}
```

## Arithmetic

{{ rule(id="3.12:21", cat="dynamic-semantics") }}

The binary operators `+`, `-`, `*`, and `/` on two operands of the same
floating-point type produce the IEEE 754 operation on those operands, correctly
rounded to nearest with ties to even at that type. No floating-point arithmetic
operation traps: every combination of operands, including the ones that trap
for integers, has a defined result.

{{ rule(id="3.12:22", cat="dynamic-semantics") }}

Dividing a nonzero finite value by a zero yields an infinity whose sign is the
exclusive-or of the operands' signs, and dividing a zero by a zero yields a
NaN. This is the deliberate divergence from integer division, which traps on a
zero divisor (§8.3); the integer rule does not apply to floating-point
operands.

{{ rule(id="3.12:23", cat="dynamic-semantics") }}

An arithmetic result whose magnitude exceeds the operand type's range becomes
an infinity of that sign, and one too small in magnitude to round to any
subnormal becomes a zero of that sign. Neither is a trap, and neither is the
integer overflow panic of §8.1.

{{ rule(id="3.12:24", cat="dynamic-semantics") }}

Unary `-` applied to a floating-point operand flips its sign bit and changes
nothing else. It is defined for every operand: `-0.0` is negative zero, and
negating a NaN yields a NaN (of the opposite sign). Unlike integer negation
(4.2:16) it never traps, and unlike integer negation it applies to every
floating-point type rather than only the signed ones.

{{ rule(id="3.12:25", cat="legality-rule") }}

The remainder operator `%` is not defined on floating-point operands. An
application of `%` to a floating-point operand **MUST** be rejected at compile
time, whether it is evaluated at run time or at compile time. The standard
library's `std.math.rem` supplies the exact truncated remainder (C's `fmod`)
instead.

{{ rule(id="3.12:26", cat="example") }}

```rue
fn zero() -> f64 { 0.0 }

fn main() -> i32 {
    let z: f64 = zero();
    @dbg(1.0 / z);      // inf
    @dbg(-1.0 / z);     // -inf
    @dbg(z / z);        // NaN
    @dbg(-z);           // -0.0
    @dbg(0.1 + 0.2);    // 0.30000000000000004
    0
}
```

## Comparison

{{ rule(id="3.12:27", cat="dynamic-semantics") }}

The comparison operators `==`, `!=`, `<`, `<=`, `>`, and `>=` on two operands
of the same floating-point type are the IEEE 754 comparisons. They are a
*partial* order: when either operand is a NaN the two operands are unordered,
so every one of these operators yields `false` except `!=`, which yields
`true`. In particular a NaN is not equal to itself.

{{ rule(id="3.12:28", cat="dynamic-semantics") }}

Negative zero and positive zero compare equal, and neither is less than the
other, even though they are distinct values with distinct sign bits.

{{ rule(id="3.12:29", cat="dynamic-semantics") }}

Structural equality on an aggregate (4.3:3b) applies these rules at each
floating-point leaf. An array or struct holding a NaN is therefore not equal to
itself, and two aggregates whose corresponding leaves are `-0.0` and `+0.0` are
equal. A NaN is the value that makes Rue's structural `==` a partial
equivalence rather than a total one (4.3:3g).

{{ rule(id="3.12:30", cat="example") }}

```rue
fn zero() -> f64 { 0.0 }

fn main() -> i32 {
    let n: f64 = zero() / zero();
    @dbg(n == n);        // false
    @dbg(n != n);        // true
    @dbg(n < n);         // false
    @dbg(-0.0 == 0.0);   // true
    0
}
```

## Total Ordering

{{ rule(id="3.12:31", cat="normative") }}

`@total_cmp(a, b)` takes two operands of the same floating-point type and has
type `i32`.

{{ rule(id="3.12:32", cat="dynamic-semantics") }}

`@total_cmp(a, b)` returns a negative value when `a` precedes `b`, zero when
the two operands have the same bit pattern, and a positive value when `a`
follows `b`, under the IEEE 754 `totalOrder` predicate. That order is total and consistent with the
sign-magnitude bit pattern: every negative NaN, then `-inf`, the negative
finite values, `-0.0`, `+0.0`, the positive finite values, `+inf`, then every
positive NaN. It never traps, and it is the ordering to use for sorting,
hashing, and ordered containers, where the partial order of 3.12:27 is not
usable.

{{ rule(id="3.12:33", cat="example") }}

```rue
fn main() -> i32 {
    @dbg(@total_cmp(-0.0, 0.0));   // negative: -0.0 sorts first
    @dbg(@total_cmp(1.0, 1.0));    // 0
    @dbg(@total_cmp(1.0, 0.0));    // positive
    0
}
```

## Rounding and Square Root

{{ rule(id="3.12:34", cat="normative") }}

`@sqrt`, `@floor`, `@ceil`, `@trunc`, and `@round` each take exactly one
floating-point operand and produce a value of that same type.

{{ rule(id="3.12:35", cat="dynamic-semantics") }}

`@sqrt(x)` is the IEEE 754 square root, correctly rounded to nearest with ties
to even. `@sqrt` of a negative operand is a NaN, `@sqrt(-0.0)` is `-0.0`, and
`@sqrt(inf)` is `inf`.

{{ rule(id="3.12:36", cat="dynamic-semantics") }}

`@floor(x)`, `@ceil(x)`, `@trunc(x)`, and `@round(x)` produce the integral
value of the operand's type obtained by rounding `x` toward negative infinity,
toward positive infinity, toward zero, and to the nearest integral value with
ties rounded *away* from zero, respectively. Each result is exact — an integral
value near `x` is always representable — so no rounding error is introduced.

{{ rule(id="3.12:37", cat="dynamic-semantics") }}

None of the five intrinsics of 3.12:34 traps. A NaN operand yields a NaN, an
infinite operand yields that same infinity (except as 3.12:35 provides for
`@sqrt` of a negative operand), and a zero operand yields a zero of the same
sign.

{{ rule(id="3.12:38", cat="example") }}

```rue
fn main() -> i32 {
    @dbg(@sqrt(4.0));      // 2.0
    @dbg(@floor(-1.5));    // -2.0
    @dbg(@ceil(-1.5));     // -1.0
    @dbg(@trunc(-1.5));    // -1.0
    @dbg(@round(2.5));     // 3.0 (ties away from zero)
    @dbg(@round(-2.5));    // -3.0
    0
}
```

## Formatting

{{ rule(id="3.12:39", cat="normative") }}

`@dbg` (4.13:6) and `@to_string` (§3.7) accept a floating-point argument. The
text they produce for it is defined by 3.12:40 through 3.12:42.

{{ rule(id="3.12:40", cat="normative") }}

A finite floating-point value is rendered as the **shortest decimal digit
string that round-trips** at the value's own type: the fewest significant
decimal digits which, read back at that type under 3.12:9, recover exactly the
value being printed. An `f32` therefore prints the digits that identify it as
an `f32`, not the digits of the `f64` with the same numeric value.

{{ rule(id="3.12:41", cat="normative") }}

Those digits are laid out in positional notation when the value's decimal
exponent lies in `-5..=15` for `f64`, or in `-6..=12` for `f32`, and in
scientific notation `d.ddde±XX` — a single leading digit, the remaining digits
after a `.` when there are any, then `e`, an explicit `+` or `-`, and the
decimal exponent — otherwise. The rendering always carries either a fractional
part or an exponent, so a value with no fractional digits prints as `1.0`
rather than `1`, and a value's text is never mistakable for an integer.

{{ rule(id="3.12:42", cat="normative") }}

A NaN renders as `NaN` whatever its sign or payload, a positive infinity as
`inf`, and a negative infinity as `-inf`. Negative zero renders as `-0.0`,
distinguishing it from `0.0`.

{{ rule(id="3.12:43", cat="example") }}

```rue
fn main() -> i32 {
    @dbg(1.0);        // 1.0
    @dbg(-0.0);       // -0.0
    @dbg(1.5);        // 1.5
    @dbg(1e15);       // 1000000000000000.0
    @dbg(1e16);       // 1e+16
    @dbg(1e-6);       // 1e-6
    @dbg(1.5e300);    // 1.5e+300
    0
}
```

## The Sign of a NaN

{{ rule(id="3.12:44", cat="normative") }}

The sign bit of a NaN produced by a floating-point operation is
**implementation-defined** (Appendix B.1): an implementation chooses it and
documents the choice. This implementation takes the sign its target's hardware
produces for a default NaN — negative on x86-64, positive on AArch64 — so one
program can get opposite `@total_cmp` results against a NaN on two targets. It is *not* undefined behavior: the value is a NaN on every target,
and every operation on it is fully defined.

{{ rule(id="3.12:45", cat="informative") }}

A NaN's sign is observable only through `@total_cmp` (3.12:32) or by inspecting
its bits; arithmetic, the comparison operators, and formatting (3.12:42) all
ignore it. A portable program must not depend on it. Compile-time evaluation
resolves this by canonicalizing instead: see 3.12:48.

## Compile-Time Evaluation

{{ rule(id="3.12:46", cat="normative") }}

`+`, `-`, `*`, `/`, unary `-`, and the comparison operators are
comptime-evaluable on floating-point operands (4.14:27), so they may appear in
`const` initializers and `comptime` blocks. Each operation is evaluated **at
the width of that operation** — the floating-point type its operands have,
which for an expression written only from literals is the `f64` of 3.12:8 and
otherwise is the type the context supplies.

{{ rule(id="3.12:47", cat="normative") }}

A compile-time floating-point operation produces exactly the value the same
operation produces at run time on the same operands, including its infinities
and its zero signs. Compile-time evaluation is not carried out at a wider
precision and then rounded once, so a `const` and the corresponding runtime
computation never disagree.

{{ rule(id="3.12:48", cat="normative") }}

A NaN produced by a compile-time operation is canonicalized to a positive quiet
NaN, whatever the sign the compiling host's hardware would have produced. The
implementation-defined sign of 3.12:44 is a property of the target, and a
compiler must produce the same program on every host.

{{ rule(id="3.12:49", cat="legality-rule") }}

A compile-time floating-point operation whose two operands have different
floating-point types **MUST** be rejected (`E1200`), with the same
no-implicit-conversion rule 3.12:13 states for run time.

{{ rule(id="3.12:50", cat="example") }}

```rue
const THIRD_64: f64 = 1.0 / 3.0;    // evaluated at f64
const THIRD_32: f32 = 1.0 / 3.0;    // evaluated at f32
const SUM: f64 = 0.1 + 0.2;
const ORDERED: bool = 1.0 < 2.0;

fn main() -> i32 {
    @dbg(THIRD_64);    // 0.3333333333333333
    @dbg(THIRD_32);    // 0.33333334
    @dbg(SUM);         // 0.30000000000000004
    @dbg(ORDERED);     // true
    0
}
```
