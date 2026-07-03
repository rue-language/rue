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
enum_def = "enum" IDENT "{" [ enum_variants ] "}" ;
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

Enum variants are referenced using path syntax: `EnumName::VariantName`.
An error is raised if the enum type does not exist.

{{ rule(id="6.3:5", cat="normative") }}

An error is raised if the variant does not exist within the enum.

{{ rule(id="6.3:6") }}

```rue
enum Color {
    Red,
    Green,
    Blue,
}

fn main() -> i32 {
    let c = Color::Red;
    0
}
```

## Match on Enums

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
    let c = Color::Green;
    match c {
        Color::Red => 1,
        Color::Green => 2,
        Color::Blue => 3,
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
        Color::Red => 1,
        Color::Green => 2,
        Color::Blue => 3,
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

Tuple-variant payloads are a preview feature: an enum declaration containing a
payload-carrying variant requires the `enum_payloads` preview feature to be
enabled (ADR-0038). Discriminant-only enums remain available without any
preview feature.

{{ rule(id="6.3:15", cat="normative") }}

The representation of an enum is a **tagged union**: a discriminant that
selects the active variant, together with payload storage sized to accommodate
the largest variant's payload. The exact layout is **implementation-defined**
(see spec 1.3), so niche optimizations are permitted without a change to this
specification.

{{ rule(id="6.3:16", cat="normative") }}

A tuple variant is constructed by applying the variant path to payload
arguments: `EnumName::Variant(arg, ...)`. The number of arguments **MUST**
equal the variant's payload arity, and each argument's type **MUST** match the
corresponding declared payload type. Using a payload-carrying variant as a
bare path (`EnumName::Variant`, with no arguments) is an error.

{{ rule(id="6.3:17", cat="normative") }}

Binding a variant's payload in a `match` arm (see spec 4.7) moves the payload
out of the enum value in move mode; a moved-out payload that has a destructor
runs that destructor exactly once, when its binding leaves scope.

{{ rule(id="6.3:19") }}

The intended full model is that an enum's multiplicity is the join of its
variants' payload multiplicities — an enum a variant of which carries a linear
payload is itself linear and must be consumed — and that dropping an enum value
without consuming it drops the payload of its **active** variant (the one
selected by the discriminant). Multiplicity infectiousness and implicit
active-variant drop glue are a follow-up phase of the `enum_payloads` preview
feature (RUE-221); until then, implicitly dropping an unconsumed
payload-carrying enum leaks (but never double-drops) its payload.

{{ rule(id="6.3:18") }}

```rue
enum Shape { Circle(i32), Rect(i32, i32), Empty }

fn area(s: Shape) -> i32 {
    match s {
        Shape::Circle(r) => r,
        Shape::Rect(w, h) => w + h,
        Shape::Empty => 0,
    }
}

fn main() -> i32 {
    area(Shape::Rect(3, 4))
}
```
