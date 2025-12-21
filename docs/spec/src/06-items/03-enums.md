# Enums

r[6.3.1#normative]
An enum is defined using the `enum` keyword.

r[6.3.2#normative]
```ebnf
enum_def = "enum" IDENT "{" [ enum_variants ] "}" ;
enum_variants = IDENT { "," IDENT } [ "," ] ;
```

## Enum Definition

r[6.3.3#normative]
Variant names must be unique within an enum.

r[6.3.12#normative]
An enum with zero variants is valid and represents an uninhabited type.
A zero-variant enum can never be constructed.

r[6.3.4#normative]
Enum variants are referenced using path syntax: `EnumName::VariantName`.
An error is raised if the enum type does not exist.

r[6.3.5#normative]
An error is raised if the variant does not exist within the enum.

r[6.3.6]
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

r[6.3.7#normative]
Enum values can be matched using pattern matching in `match` expressions.
Each arm pattern uses the same path syntax as enum variant expressions.

r[6.3.8#normative]
Match expressions on enums must be exhaustive: all variants must be covered,
either explicitly or via a wildcard pattern `_`.

r[6.3.9]
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

r[6.3.10#normative]
Enums can be used as function parameter types, return types, and struct field types.

r[6.3.11]
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
