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

These are the only layout properties a portable program may rely on; the specific offsets, slot sizes, and paddings the current implementation happens to use (3.6:9, 3.6:10, 3.6:12) are documented observations, not part of this set.

{{ rule(id="3.6:9", cat="informative") }}

Under the current implementation, every non-zero-sized value occupies one or more 8-byte slots: each scalar type (`i8`, `i16`, `i32`, `i64`, `u8`, `u16`, `u32`, `u64`, `bool`) occupies a single slot, and a struct's fields are placed in declaration order, each in the slots determined by its type, with no padding between or after them. This is a documented property of the current implementation (1.3:6), not a language guarantee.

{{ rule(id="3.6:10", cat="informative") }}

It follows that, under the current implementation, the size of a struct is the sum of the sizes of all its fields, with no padding between fields or at the end (a zero-sized field contributes nothing). A future version that packed scalars or reordered fields would change this observation.

{{ rule(id="3.6:11") }}

```rue
// Under the current implementation, two i32 fields occupy 2 slots (16 bytes).
struct Point { x: i32, y: i32 }

// A nested struct occupies the sum of all nested field slots: 4 slots (32 bytes).
struct Line { start: Point, end: Point }
```

{{ rule(id="3.6:12", cat="informative") }}

Under the current implementation, a struct with one or more fields has 8-byte alignment, and an empty struct (a zero-sized type) has 1-byte alignment; `@align_of` observes these values. Like the sizes above, these are documented observations of the current layout, not language guarantees.

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
