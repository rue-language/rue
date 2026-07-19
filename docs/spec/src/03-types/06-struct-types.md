+++
title = "Struct Types"
weight = 6
template = "spec/page.html"
+++

# Struct Types

{{ rule(id="3.6:1", cat="normative") }}

A struct type is a composite type consisting of named fields.

{{ rule(id="3.6:2", cat="normative") }}

A struct is defined using the `struct` keyword:

```ebnf
struct_def = "struct" IDENT "{" [ struct_fields ] "}" ;
struct_fields = struct_field { "," struct_field } [ "," ] ;
struct_field = IDENT ":" type ;
```

{{ rule(id="3.6:3") }}

```rue
struct Point {
    x: i32,
    y: i32,
}

fn main() -> i32 {
    let p = Point { x: 10, y: 20 };
    p.x + p.y  // 30
}
```

{{ rule(id="3.6:4", cat="normative") }}

Struct fields are accessed using dot notation: `value.field_name`.

{{ rule(id="3.6:5", cat="normative") }}

All fields **MUST** be initialized when creating a struct instance.

{{ rule(id="3.6:6", cat="normative") }}

Field names **MUST** be unique within a struct.

## Memory Layout

{{ rule(id="3.6:7", cat="informative") }}

The in-memory layout of a struct — the byte offset of each field, any padding, the overall size, and the alignment — is **implementation-defined** (see 1.3:6). The implementation is free to choose any layout consistent with the guarantees of 3.6:8, and **MAY** place fields in any order, pack small scalars together, insert or omit padding, drop storage for zero-sized fields, or exploit value-representation ("niche") optimizations; the chosen layout may change between versions. A program can *observe* the chosen layout — through `@size_of`, `@align_of`, or `unchecked` raw-pointer access — but those observations report the current choice, they do **not** guarantee a particular value. To obtain a field's offset or a pointer to a field without hard-coding a layout, use the compiler-mediated intrinsics `@offset_of(T, field)` and `@field_ptr(place)` (see the [Intrinsics](@/04-expressions/13-intrinsics.md) chapter): they report the offset under whatever layout the implementation chose, so `unchecked` code that reaches fields through them stays correct under any layout. A portable program must not hand-compute a field offset from a literal.

{{ rule(id="3.6:8", cat="normative") }}

Whatever layout the implementation chooses, every value of a struct type satisfies these guarantees:

- Each field is accessible by name (3.6:4) and reads back the value most recently stored to it.
- Distinct fields occupy non-overlapping storage.
- `@size_of(T)` and `@align_of(T)` are well-defined for every sized type `T` and report the size and alignment the implementation has chosen for `T`.
- The size of a type is a multiple of its alignment.

These are the only layout properties a portable program may rely on; the specific offsets, sizes, alignments, and paddings the current implementation happens to use (3.6:9, 3.6:10, 3.6:12) are documented observations, not part of this set.

{{ rule(id="3.6:9", cat="informative") }}

Under the current implementation, each scalar type uses its natural byte width and alignment (`i8`, `u8`, and `bool` are one byte; `i16`/`u16` two; `i32`/`u32` four; `i64`/`u64` and raw pointers eight), and a struct's fields are placed in declaration order, each at the lowest offset satisfying its alignment, with interior padding inserted where a field's alignment requires it. This is a documented property of the current implementation (1.3:6), not a language guarantee.

{{ rule(id="3.6:10", cat="informative") }}

It follows that, under the current implementation, a struct's size packs its fields at their natural alignment and is rounded up to the struct's own alignment, so it includes any interior and tail padding (a zero-sized field contributes nothing). A future version that reordered fields or chose a different packing would change this observation.

{{ rule(id="3.6:10a", cat="informative") }}

This is the *compact* native layout ratified by ADR-0052, the compiler's default. Under it a type's size includes tail padding and equals its array element stride (`stride == size`), enums use the smallest sufficient unsigned tag placed before a payload at the maximum variant alignment, and a zero-sized type has size `0`, alignment `1`, and stride `0`. This too is a documented observation, not a guarantee: only the properties of 3.6:8 are portable, and `@size_of`, `@align_of`, `@offset_of`, and `@field_ptr` continue to report the chosen layout so compiler-mediated code stays correct even if it changes again.

{{ rule(id="3.6:11") }}

```rue
// Under the current implementation, two i32 fields pack to 8 bytes.
struct Point { x: i32, y: i32 }

// A nested struct is the sum of its nested fields: two Points = 16 bytes.
struct Line { start: Point, end: Point }
```

{{ rule(id="3.6:12", cat="informative") }}

Under the current implementation, a struct's alignment is that of its most-aligned field (a struct of `i32` fields is four-aligned), and an empty struct (a zero-sized type) has 1-byte alignment; `@align_of` observes these values. Like the sizes above, these are documented observations of the current layout, not language guarantees.

{{ rule(id="3.6:13", cat="normative") }}

A struct with no fields is a zero-sized type. See [Zero-Sized Types](../#zero-sized-types) for the general definition.

{{ rule(id="3.6:14") }}

```rue
// An empty struct is a zero-sized type
struct Empty {}

fn main() -> i32 {
    let e = Empty {};
    @size_of(Empty)  // 0
}
```

## Struct Literals

{{ rule(id="3.6:15", cat="normative") }}

In a struct literal, field initializers may appear in any order. Each initializer is matched to a declared field by name; the order in which fields are written need not match the declaration order. Providing the same field more than once, omitting a field, or naming a field the struct does not declare are all errors (see 3.6:5, 3.6:6).

{{ rule(id="3.6:16", cat="dynamic-semantics") }}

Regardless of the order in which fields are written, each field of the resulting value is set from the initializer that names it (field matching is by name, not by position). Field initializer expressions are still evaluated in source order (left-to-right as written; see 4.0:9), so a field written earlier is evaluated earlier even when the implementation stores it in a later location.

{{ rule(id="3.6:17") }}

```rue
struct Point { x: i32, y: i32 }

fn main() -> i32 {
    // Fields given out of declaration order; matched by name.
    let p = Point { y: 20, x: 10 };
    p.x - p.y  // -10, i.e. 10 - 20
}
```

### Field-Init Shorthand

{{ rule(id="3.6:18", cat="normative") }}

A field initializer written as a bare identifier `x` — with no `: expression` — is **field-init shorthand** and is exactly equivalent to `x: x`: the field named `x` is initialized from the value bound to `x` in the enclosing scope, resolved to the innermost binding of that name (a `let` that shadows an outer `x` supplies the shorthand's value). If no `x` is in scope the shorthand is an undefined-name error, exactly as the explicit `x: x` would be. Shorthand and explicit initializers may be freely mixed within one literal (`P { x, y: 2 }`), and the shorthand is available for every struct-literal head, including `Self { .. }`.

{{ rule(id="3.6:19") }}

```rue
struct Point { x: i32, y: i32 }

fn main() -> i32 {
    let x = 10;
    // `x` is shorthand for `x: x`; `y` is written explicitly. Mixed forms are allowed.
    let p = Point { x, y: 20 };
    p.x + p.y  // 30
}
```
