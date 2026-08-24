+++
title = "Enums"
weight = 3
template = "spec/page.html"
+++

# Enums

{{ rule(id="6.3:1", cat="normative") }}

An enum is defined using the `enum` keyword.

{{ rule(id="6.3:2", cat="normative") }}

```ebnf
enum_def = directives [ "pub" ] "enum" IDENT "{" [ enum_variants ] "}" ;
enum_variants = enum_variant { "," enum_variant } [ "," ] ;
enum_variant = IDENT [ "(" type { "," type } [ "," ] ")" ] ;
```

## Enum Definition

{{ rule(id="6.3:3", cat="normative") }}

Variant names **MUST** be unique within an enum.

{{ rule(id="6.3:12", cat="normative") }}

An enum with zero variants is valid and represents an uninhabited type.
A zero-variant enum can never be constructed.

{{ rule(id="6.3:4", cat="normative") }}

Enum variants are referenced using dot syntax: `EnumName.VariantName`. Because
`.` is the sole member-access spelling, `Name.VariantName` where `Name` does not
name a type is an ordinary undefined-name reference and is a compile-time error.

{{ rule(id="6.3:5", cat="normative") }}

It is a compile-time error (E0420) if the named enum type exists but the named
variant does not exist within it.

{{ rule(id="6.3:6") }}

```rue
enum Color {
    Red,
    Green,
    Blue,
}

fn main() -> i32 {
    let c = Color.Red;
    0
}
```

## Match on Enums

{{ rule(id="6.3:21", cat="normative") }}

`@non_exhaustive` is an opt-in preview-gated declaration directive. It **MUST**
appear only on a `pub enum`; applying it to a private enum is a compile-time
error. The directive promises source/API compatibility: a match written in a
module other than the enum's defining module **MUST** contain a wildcard arm,
even when it names every variant currently known to the compiler. The defining
module keeps ordinary exhaustive checking and ordinary unreachable-pattern
diagnostics. A wildcard in an importing module remains reachable under this
rule because a future enum variant may select it.

{{ rule(id="6.3:22", cat="normative") }}

Adding a variant to a `@non_exhaustive` enum does not promise stable layout,
ABI, or runtime behavior. Such an addition may change enum representation,
size, alignment, generated code, or other target-level behavior; the promise
is limited to source compatibility of importing matches that include a
wildcard.

{{ rule(id="6.3:7", cat="normative") }}

Enum values can be matched using pattern matching in `match` expressions.
Each arm pattern uses the same path syntax as enum variant expressions.

{{ rule(id="6.3:8", cat="normative") }}

Match expressions on enums **MUST** be exhaustive: all variants **MUST** be covered,
either explicitly or via a wildcard pattern `_`.

{{ rule(id="6.3:9") }}

```rue
enum Color { Red, Green, Blue }

fn main() -> i32 {
    let c = Color.Green;
    match c {
        Color.Red => 1,
        Color.Green => 2,
        Color.Blue => 3,
    }
}
```

## Enum Types

{{ rule(id="6.3:10", cat="normative") }}

Enums can be used as function parameter types, return types, and struct field types.

{{ rule(id="6.3:11") }}

```rue
enum Color { Red, Green, Blue }

fn get_value(c: Color) -> i32 {
    match c {
        Color.Red => 1,
        Color.Green => 2,
        Color.Blue => 3,
    }
}
```

## Variant Payloads

{{ rule(id="6.3:13", cat="normative") }}

A variant **MAY** carry data as a **tuple variant** — a parenthesized,
comma-separated list of one or more payload field types written after the
variant name (`Circle(i32)`, `Rect(i32, i32)`). A variant without a payload
list is a discriminant-only variant, as before. An enum in which at least one
variant carries a payload is a *sum type*.

{{ rule(id="6.3:14", cat="normative") }}

A variant **MAY** carry a payload: a parenthesized, comma-separated list of one
or more types written after the variant name (ADR-0038). A variant with no such
list is discriminant-only, and an enum may freely mix payload-carrying and
discriminant-only variants.

{{ rule(id="6.3:15", cat="normative") }}

The representation of an enum is a **tagged union**: a discriminant that
selects the active variant, together with payload storage sized to accommodate
the largest variant's payload. The exact layout is **implementation-defined**
(see spec 1.3), so niche optimizations are permitted without a change to this
specification.

{{ rule(id="6.3:16", cat="normative") }}

A tuple variant is constructed by applying the variant path to payload
arguments: `EnumName.Variant(arg, ...)`. The number of arguments **MUST**
equal the variant's payload arity, and each argument's type **MUST** match the
corresponding declared payload type. Payload arguments are unmarked; tuple
variants do not declare `borrow` or `inout` parameters. Using a
payload-carrying variant as a bare path (`EnumName.Variant`, with no
arguments) is an error.

{{ rule(id="6.3:17", cat="normative") }}

Binding a variant's payload in a `match` arm (see spec 4.7) moves the payload
out of the enum value in move mode; a moved-out payload that has a destructor
runs that destructor exactly once, when its binding leaves scope.

{{ rule(id="6.3:19", cat="legality-rule") }}

An enum's multiplicity is the **join** of its variants' payload multiplicities
over the lattice Copy ⊑ Affine ⊑ Linear. An enum whose every payload is Copy
(including a discriminant-only enum) is itself Copy; an enum a variant of which
carries an Affine (drop-carrying) payload is Affine; an enum a variant of which
carries a **linear** payload is itself linear and **MUST** be consumed —
letting such a value be implicitly dropped is a must-consume error, exactly as
for a linear struct. Consuming the enum (for example, by a `match` that binds
and consumes the payload) discharges the obligation.

{{ rule(id="6.3:20", cat="dynamic-semantics") }}

Dropping an enum value that is not consumed drops the payload of its **active**
variant — the one selected by the discriminant at run time — running that
payload's drop glue exactly once, and nothing for a discriminant-only active
variant. A payload moved out of the enum beforehand (for example, through a
`match` binding) is not dropped again at scope exit.

{{ rule(id="6.3:18") }}

```rue
enum Shape { Circle(i32), Rect(i32, i32), Empty }

fn area(s: Shape) -> i32 {
    match s {
        Shape.Circle(r) => r,
        Shape.Rect(w, h) => w + h,
        Shape.Empty => 0,
    }
}

fn main() -> i32 {
    area(Shape.Rect(3, 4))
}
```
