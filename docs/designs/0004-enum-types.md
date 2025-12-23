---
id: 0004
title: Enum Types
status: implemented
tags: [types, syntax]
feature-flag: enum-types
created: 2025-01-01
accepted: 2025-01-01
implemented: 2025-01-01
spec-sections: []
superseded-by:
---

<!-- Note: This ADR predates the preview feature system (ADR-0005). The feature-flag
     is a placeholder to satisfy the schema; this feature was not actually gated. -->

# ADR-0004: Enum Types (Discriminated Unions Without Data)

## Status

Implemented

## Summary

Add simple enum types (discriminated unions without associated data) to Rue, enabling type-safe variant types and exhaustive match expressions.

## Context

Rue currently supports structs as the only user-defined type. Users cannot define a type with a fixed set of variants. For example, there's no way to express:

```rue
// Not currently possible
enum Direction {
    North,
    East,
    South,
    West,
}
```

This forces users to represent states as integers with magic values, losing type safety and readability. Enums are fundamental to expressing domain concepts and integrating with match expressions.

## Decision

Add simple enum types to Rue. This first iteration focuses on **discriminated unions without associated data** (C-style enums), which are a stepping stone toward full algebraic data types.

### Syntax

```rue
enum Color {
    Red,
    Green,
    Blue,
}

fn main() -> i32 {
    let c = Color::Green;
    match c {
        Color::Red => 1,
        Color::Green => 2,
        Color::Blue => 3,
    }
}
```

### Grammar Changes

Add to the grammar:

```ebnf
item           = function | struct_def | enum_def ;
enum_def       = "enum" IDENT "{" enum_variants "}" ;
enum_variants  = enum_variant { "," enum_variant } [ "," ] ;
enum_variant   = IDENT ;

// Extend paths for variant access
path           = IDENT [ "::" IDENT ] ;
primary        = ... | path | ... ;
pattern        = "_" | INTEGER | BOOL | path ;
```

### Semantics

#### Enum Definition

An enum defines:
- A new type with the enum's name
- A set of named variants, each associated with a discriminant value

**Type namespace**: Enum names occupy the type namespace alongside structs and primitive types.

#### Zero-Variant Enums

An enum with no variants is valid and represents an uninhabited type (like the never type `!`):

```rue
enum Void {}
```

A zero-variant enum:
- Has `Type::Never` as its discriminant type (since no discriminant value is needed)
- Can never be constructed (no variants exist)
- Match expressions on it require no arms (vacuously exhaustive)
- Values of this type can coerce to any type (like `!`)

#### Discriminant Values

Discriminants are automatically assigned starting from 0:

```rue
enum Status {
    Pending,   // discriminant 0
    Active,    // discriminant 1
    Complete,  // discriminant 2
}
```

#### Discriminant Type

The discriminant type is chosen to be the smallest unsigned integer that can represent all variants:
- 0 variants: `Never`
- 1-256 variants: `u8`
- 257-65536 variants: `u16`
- etc.

#### Variant Construction

Variants are constructed using path syntax `EnumName::VariantName`:

```rue
let color = Color::Red;
```

#### Pattern Matching

Enum variants are matched using the same path syntax:

```rue
match color {
    Color::Red => 0,
    Color::Green => 1,
    Color::Blue => 2,
}
```

#### Exhaustiveness Checking

Match expressions on enum types must cover all variants:

```rue
match direction {
    Direction::North => 0,
    Direction::South => 1,
    // ERROR: match is not exhaustive, missing variants: East, West
}
```

A wildcard pattern can be used to match remaining variants:

```rue
match direction {
    Direction::North => 0,
    _ => 1,  // matches East, South, West
}
```

## Implementation Phases

- [x] **Phase 1: Core enum types** - Lexer, parser, AST, RIR, AIR, type system, semantic analysis, exhaustiveness checking, code generation

## Consequences

### Positive

- **Type safety**: Named variants instead of magic integers
- **Exhaustiveness**: Compiler ensures all cases are handled
- **Foundation**: Stepping stone to full algebraic data types
- **Match integration**: Works naturally with existing match expressions
- **Familiar syntax**: Follows Rust syntax conventions

### Negative

- **Complexity**: New item type to track through the compiler
- **Path resolution**: Need to handle qualified paths (`Enum::Variant`)
- **No data yet**: Users may want associated data (future work)

## Open Questions

None remaining.

## Future Work

- Enum variants with associated data (`Some(i32)`)
- Explicit discriminant values (`North = 1`)
- `#[repr(...)]` for controlling memory layout
- Enum methods
- `use Enum::*` imports
- Enum-to-integer conversion (`as u8`)

## References

- Rust enum documentation
