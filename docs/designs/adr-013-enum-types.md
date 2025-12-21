# ADR-013: Enum Types (Discriminated Unions Without Data)

## Status

Proposed

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

#### 1. Enum Definition

An enum defines:
- A new type with the enum's name
- A set of named variants, each associated with a discriminant value

**Type namespace**: Enum names occupy the type namespace alongside structs and primitive types. An enum cannot shadow another type:

```rue
struct Foo {}
enum Foo { A }  // ERROR: type `Foo` is already defined

enum i32 { A }  // ERROR: cannot shadow primitive type `i32`
```

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

This is useful for representing impossible states in type-level programming.

#### 2. Discriminant Values

Discriminants are automatically assigned starting from 0:

```rue
enum Status {
    Pending,   // discriminant 0
    Active,    // discriminant 1
    Complete,  // discriminant 2
}
```

**Future extension**: Explicit discriminant values (`Pending = 1`) are out of scope for this ADR.

#### 3. Discriminant Type

The discriminant type is chosen to be the smallest unsigned integer that can represent all variants:

```rust
fn discriminant_type(variant_count: usize) -> Type {
    if variant_count == 0 {
        Type::Never  // zero-variant enum is uninhabited
    } else if variant_count <= 256 {
        Type::U8
    } else if variant_count <= 65536 {
        Type::U16
    } else if variant_count <= 4_294_967_296 {
        Type::U32
    } else {
        Type::U64
    }
}
```

This is straightforward since the compiler already supports all integer types.

#### 4. Variant Construction

Variants are constructed using path syntax `EnumName::VariantName`:

```rue
let color = Color::Red;
```

This creates a value of type `Color` with the appropriate discriminant.

#### 5. Pattern Matching

Enum variants are matched using the same path syntax:

```rue
match color {
    Color::Red => 0,
    Color::Green => 1,
    Color::Blue => 2,
}
```

#### 6. Exhaustiveness Checking

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

### Type System Changes

#### 1. Extend the Type enum

In `rue-air/src/types.rs`:

```rust
pub enum Type {
    // ... existing variants ...

    /// User-defined enum type.
    /// The Symbol identifies the enum definition.
    Enum(Symbol),
}
```

#### 2. Add helper methods

```rust
impl Type {
    pub fn is_enum(&self) -> bool {
        matches!(self, Type::Enum(_))
    }
}
```

### AST Changes

Add new AST nodes:

```rust
// In items
pub struct EnumDef {
    pub name: Symbol,
    pub variants: Vec<EnumVariant>,
    pub span: Span,
}

pub struct EnumVariant {
    pub name: Symbol,
    pub span: Span,
}

// Extend Item enum
pub enum Item {
    Function(FunctionDef),
    Struct(StructDef),
    Enum(EnumDef),  // New
}
```

### RIR Changes

Add path expressions and patterns:

```rust
// In expressions
pub enum Expr {
    // ... existing ...
    Path(PathExpr),  // EnumName::VariantName
}

pub struct PathExpr {
    pub base: Symbol,      // The enum name
    pub variant: Symbol,   // The variant name
    pub span: Span,
}

// In patterns
pub enum Pattern {
    Wildcard(Span),
    IntLiteral(i64, Span),
    BoolLiteral(bool, Span),
    Path(PathPattern),  // New: EnumName::VariantName
}

pub struct PathPattern {
    pub base: Symbol,
    pub variant: Symbol,
    pub span: Span,
}
```

### AIR Changes

The typed IR needs to represent enum values:

```rust
pub enum AirInstData {
    // ... existing ...

    /// Load an enum variant value.
    /// Produces the discriminant value for the variant.
    EnumVariant {
        enum_type: Symbol,
        variant: Symbol,
    },
}
```

### Semantic Analysis Changes

#### 1. Track enum definitions

Add to the analysis context:

```rust
struct EnumInfo {
    name: Symbol,
    variants: Vec<Symbol>,
    variant_discriminants: HashMap<Symbol, i32>,
}

// In AnalysisContext
enum_defs: HashMap<Symbol, EnumInfo>,
```

#### 2. Resolve path expressions

When encountering `Foo::Bar`:
1. Look up `Foo` in enum definitions
2. Verify `Bar` is a variant of `Foo`
3. Produce `AirInstData::EnumVariant { enum_type: Foo, variant: Bar }`
4. The type is `Type::Enum(Foo)`

#### 3. Update exhaustiveness checking

For enum scrutinees, track which variants have been matched:

```rust
fn check_exhaustiveness(scrutinee_type: &Type, arms: &[MatchArm]) -> Result<(), Error> {
    match scrutinee_type {
        Type::Enum(enum_sym) => {
            let enum_info = ctx.enum_defs.get(enum_sym)?;
            let mut uncovered: HashSet<Symbol> = enum_info.variants.iter().copied().collect();

            for arm in arms {
                match &arm.pattern {
                    Pattern::Wildcard(_) => return Ok(()), // Covers all remaining
                    Pattern::Path(path) if path.base == *enum_sym => {
                        uncovered.remove(&path.variant);
                    }
                    _ => return Err(PatternTypeMismatch { ... }),
                }
            }

            if !uncovered.is_empty() {
                return Err(NonExhaustiveMatch { missing: uncovered });
            }
            Ok(())
        }
        // ... existing cases for bool, int ...
    }
}
```

### Code Generation Changes

#### 1. Lower EnumVariant to MIR

```rust
AirInstData::EnumVariant { enum_type, variant } => {
    let discriminant = ctx.get_discriminant(enum_type, variant);
    // Load immediate value
    mir.push(MirInst::LoadImm { dest: result_reg, value: discriminant });
}
```

#### 2. Match on enums

Enum matching compiles to the same compare-and-branch structure as integer matching, since enums are represented as their discriminant values.

### Lexer Changes

Add the `enum` keyword:

```rust
// In keyword list
"enum" => Token::Enum,
```

Also need to lex `::` as a single token or handle it specially in the parser.

### Parser Changes

#### 1. Parse enum definitions

```rust
fn parse_enum(&mut self) -> Result<EnumDef, ParseError> {
    self.expect(Token::Enum)?;
    let name = self.expect_ident()?;
    self.expect(Token::LBrace)?;
    let variants = self.parse_enum_variants()?;
    self.expect(Token::RBrace)?;
    Ok(EnumDef { name, variants, span })
}
```

#### 2. Parse path expressions and patterns

When we see an identifier followed by `::`:
- If parsing an expression: produce a `PathExpr`
- If parsing a pattern: produce a `PathPattern`

## Phases of Implementation

### Phase 1: Core Enum Types (This ADR)

1. Add `enum` keyword to lexer
2. Add `::` token to lexer (or handle in parser)
3. Add enum parsing to parser
4. Add enum AST nodes
5. Add RIR path expression/pattern nodes
6. Add AIR enum variant instruction
7. Add Type::Enum to type system
8. Implement semantic analysis for enums
9. Update exhaustiveness checking
10. Implement code generation

### Phase 2: Future Extensions (Separate ADRs)

- Enum variants with associated data (`Some(i32)`)
- Explicit discriminant values (`North = 1`)
- `#[repr(...)]` for controlling memory layout
- Enum methods

## File Changes Summary

| File | Changes |
|------|---------|
| `crates/rue-lexer/src/lib.rs` | Add `enum` keyword, handle `::` |
| `crates/rue-parser/src/lib.rs` | Parse enum definitions, path expressions/patterns |
| `crates/rue-parser/src/ast.rs` | Add `EnumDef`, `EnumVariant`, `PathExpr`, `PathPattern` |
| `crates/rue-rir/src/lib.rs` | Add path expression/pattern IR nodes |
| `crates/rue-air/src/types.rs` | Add `Type::Enum` variant |
| `crates/rue-air/src/sema.rs` | Analyze enums, update exhaustiveness |
| `crates/rue-codegen/src/x86_64/*.rs` | Lower enum variants and matching |
| `crates/rue-codegen/src/aarch64/*.rs` | Lower enum variants and matching |
| `docs/spec/src/03-types/07-enum-types.md` | New spec chapter |
| `crates/rue-spec/cases/enums/` | New test directory |

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

### Neutral

- **Memory layout**: Simple discriminant storage is efficient
- **No explicit values**: Users cannot specify discriminant values (yet)

## Test Plan

Test cases should cover:

1. **Basic enum definition and usage** (5+ tests)
2. **Match exhaustiveness** (5+ tests)
3. **Type checking** (5+ tests)
4. **Error cases** (5+ tests)
   - Duplicate variant names
   - Unknown enum in path
   - Unknown variant in path
   - Non-exhaustive match
   - Type mismatch in pattern

## Acceptance Criteria

- [ ] Enums can be defined with the `enum` keyword
- [ ] Variants are accessed with `EnumName::VariantName` syntax
- [ ] Enums work in variable bindings and function parameters
- [ ] Match expressions can match on enum values
- [ ] Exhaustiveness checking works for enums
- [ ] Clear error messages for:
  - [ ] Unknown enum type
  - [ ] Unknown variant
  - [ ] Non-exhaustive match
  - [ ] Duplicate variant names
- [ ] All existing tests continue to pass

## Design Decisions

1. **Enum names occupy the type namespace** - Cannot shadow structs, other enums, or primitive types.

2. **Zero-variant enums are allowed** - They represent uninhabited types (like `!`) and are vacuously exhaustive in match.

3. **Smallest discriminant type is used** - `u8` for ≤256 variants, `u16` for ≤65536, etc.

## Open Questions

1. **Should we support `use Enum::*` imports?** (Out of scope for this ADR)

2. **What about enum-to-integer conversion?** (Out of scope - add explicit `as u8` casting later)
