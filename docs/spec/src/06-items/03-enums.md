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
enum_variants = IDENT { "," IDENT } [ "," ] ;
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
