---
id: 0030
title: Place Expressions for Memory Locations
status: proposal
tags: [ir, codegen, performance]
created: 2026-01-04
spec-sections: []
---

# ADR-0030: Place Expressions for Memory Locations

## Status

Proposal

## Summary

Introduce a `Place` abstraction in Rue's IR to represent memory locations (lvalues) as first-class values. This eliminates redundant Load instructions for array indexing and field access, fixing an asymmetry where read operations take loaded values while write operations take slots directly.

## Context

### The Problem

Currently, Rue's IR conflates memory locations ("places") with values, leading to:

1. **Asymmetric instruction signatures**:
   - `IndexGet { base: AirRef, ... }` - takes a *loaded* array value
   - `IndexSet { slot: u32, ... }` - takes a *slot* directly
   - Same asymmetry exists for `FieldGet` vs `FieldSet`

2. **Redundant Load instructions**: For `arr[0] + arr[1] + arr[2]`, the compiler generates:
   ```
   %1 = load $0        ; Load entire array
   %2 = index_get %1[0]
   %3 = load $0        ; Load entire array AGAIN
   %4 = index_get %3[1]
   %5 = load $0        ; Load entire array AGAIN
   %6 = index_get %5[2]
   ```
   Each `arr[i]` access requires loading all array elements first.

3. **Workarounds for nested access**: `ParamFieldSet` has an `inner_offset` field to handle `p.inner.x` because there's no way to compose places.

4. **Performance impact**: The `array_heavy` benchmark generates 2x more code than comparable benchmarks due to redundant array loads.

### Inspiration from Rust MIR

Rust's MIR uses a [Place struct](https://doc.rust-lang.org/beta/nightly-rustc/rustc_middle/mir/struct.Place.html) that cleanly separates memory locations from values:

```rust
pub struct Place<'tcx> {
    pub local: Local,                        // Base variable
    pub projection: &'tcx List<PlaceElem>,   // Path: [Field(0), Index(i), Field(1)]
}

pub enum ProjectionElem<V, T> {
    Deref,
    Field(FieldIdx, T),
    Index(V),
    ConstantIndex { offset, min_length, from_end },
    Subslice { from, to, from_end },
    Downcast(Symbol, VariantIdx),
}
```

A place like `arr[i].field` is represented as:
```
Place { local: arr, projection: [Index(i), Field(0)] }
```

Operations on places are explicit:
- `PlaceRead(place)` - Load value from place
- `PlaceWrite(place, value)` - Store value to place
- `AddressOf(place)` - Get pointer to place (for `&`)

## Decision

### Phase 1: Introduce Place Type in CFG

Add a `Place` type to `rue-cfg` that represents memory locations:

```rust
/// A memory location that can be read from or written to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Place {
    /// The base of the place - either a local slot or parameter slot
    pub base: PlaceBase,
    /// Projections applied to reach the final location
    pub projections: Vec<Projection>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaceBase {
    /// Local variable slot
    Local(u32),
    /// Parameter slot (for inout parameters)
    Param(u32),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Projection {
    /// Field access: `.field_name`
    Field {
        struct_id: StructId,
        field_index: u32,
    },
    /// Array index: `[index]`
    Index {
        array_type: Type,
        index: CfgValue,
    },
    // Future: Deref for pointer dereference
}
```

### Phase 2: Unify Read/Write Operations

Replace the asymmetric instruction pairs with unified place-based operations:

**Before (6 instruction variants):**
```rust
IndexGet { base: CfgValue, array_type, index }
IndexSet { slot: u32, array_type, index, value }
ParamIndexSet { param_slot: u32, array_type, index, value }
FieldGet { base: CfgValue, struct_id, field_index }
FieldSet { slot: u32, struct_id, field_index, value }
ParamFieldSet { param_slot: u32, inner_offset: u32, struct_id, field_index, value }
```

**After (2 instruction variants):**
```rust
/// Read a value from a memory location
PlaceRead {
    place: Place,
}

/// Write a value to a memory location
PlaceWrite {
    place: Place,
    value: CfgValue,
}
```

### Phase 3: Update Codegen

The codegen `trace_index_chain` and `trace_field_chain` functions become trivial - they just walk the `Place::projections` list to compute the final memory offset.

**Example: `arr[i][j]` codegen**

```rust
// Place representation:
Place {
    base: Local(0),
    projections: [
        Index { array_type: [[i32; 3]; 3], index: i },
        Index { array_type: [i32; 3], index: j },
    ]
}

// Codegen walks projections to compute:
// offset = slot_offset(0) + i * 3 * 8 + j * 8
```

### IR Examples

**Simple array read: `arr[i]`**

Before:
```
%1 = load $0                    ; Load all elements of arr
%2 = index_get %1[i]            ; Then index
```

After:
```
%1 = place_read Place { Local(0), [Index(arr_type, i)] }
```

**Nested field access: `point.inner.x`**

Before:
```
%1 = load $0                    ; Load point
%2 = field_get %1.inner         ; Get inner struct (intermediate value)
%3 = field_get %2.x             ; Get x field
```

After:
```
%1 = place_read Place { Local(0), [Field(Point, 0), Field(Inner, 0)] }
```

**Array element field: `arr[i].x`**

Before:
```
%1 = load $0                    ; Load all array elements
%2 = index_get %1[i]            ; Get element (intermediate value)
%3 = field_get %2.x             ; Get x field
```

After:
```
%1 = place_read Place { Local(0), [Index(arr_type, i), Field(Point, 0)] }
```

### AIR Changes

AIR will also adopt the Place abstraction, but can use a simpler representation since types are already resolved:

```rust
/// Reference to a place in AIR - stored as index into places array
#[derive(Debug, Clone, Copy)]
pub struct PlaceRef(u32);

/// A memory location
pub struct AirPlace {
    pub base: AirPlaceBase,
    pub projections_start: u32,
    pub projections_len: u32,
}

pub enum AirPlaceBase {
    Local(u32),
    Param(u32),
}

pub enum AirProjection {
    Field { struct_id: StructId, field_index: u32 },
    Index { array_type: Type, index: AirRef },
}
```

## Implementation Phases

- [ ] **Phase 1: Add Place type to CFG** - Define `Place`, `PlaceBase`, `Projection` in `rue-cfg`
- [ ] **Phase 2: Add PlaceRead/PlaceWrite instructions** - Add new CFG instruction variants alongside existing ones
- [ ] **Phase 3: Update CFG builder** - Generate Place-based instructions for simple cases
- [ ] **Phase 4: Update x86_64 codegen** - Emit efficient code for Place-based instructions
- [ ] **Phase 5: Update aarch64 codegen** - Same for ARM64 backend
- [ ] **Phase 6: Migrate remaining cases** - Handle all array/field operations via places
- [ ] **Phase 7: Remove old instructions** - Delete IndexGet/FieldGet with base values
- [ ] **Phase 8: Apply same changes to AIR** - Propagate place abstraction to AIR level

## Consequences

### Positive

- **Eliminates redundant loads**: Array access `arr[0] + arr[1] + arr[2]` generates 3 loads instead of 9
- **Unified instruction set**: 2 instructions instead of 6 for place operations
- **Simpler codegen**: No need for `trace_index_chain`/`trace_field_chain` - just walk projections
- **Smaller IR**: Fewer intermediate Load instructions
- **Better optimization opportunities**: Places can be analyzed for aliasing, CSE, etc.
- **Future-proof**: Easy to add `Deref` projection for pointers

### Negative

- **Larger change**: Touches AIR, CFG, and both codegen backends
- **Migration complexity**: Need to handle both old and new instructions during transition
- **Place storage**: Projections stored in vectors/arenas (memory overhead, but small)

## Open Questions

1. **Should RIR also use places?** Currently RIR is untyped, so places would need type information added. We should check what other compilers do here - likely construct places during AIR lowering since that's when we have full type information.

2. **How to handle computed array bases?** For `get_array()[i]`, the array isn't in a local.

   **Decision**: Spill to temporary local, then use place. This is simple, correct, and matches what optimizing compilers do anyway.

## Performance Considerations

### Rust's Approach

According to the [rustc documentation](https://doc.rust-lang.org/beta/nightly-rustc/rustc_middle/mir/struct.Place.html), Rust's `Place` is 16 bytes:
- `local: Local` (4 bytes, a `u32` newtype)
- `projection: &'tcx List<PlaceElem>` (8 bytes, interned reference)
- 4 bytes padding

Projections are **interned** in rustc, meaning:
- Common projection sequences are deduplicated
- Equality checks are pointer comparisons (O(1))
- Memory is arena-allocated (fast allocation, batch deallocation)

See [rustc memory management](https://rustc-dev-guide.rust-lang.org/memory.html) for details.

### Rue's Approach

Rue already uses an `extra: Vec<CfgValue>` pattern for variable-length data (struct fields, array elements). We'll extend this for projections:

```rust
/// A place is 12 bytes (fits in 2 cache lines with instruction data)
pub struct Place {
    pub base: PlaceBase,        // 8 bytes (enum with u32 payload)
    pub proj_start: u32,        // 4 bytes - index into projections array
    pub proj_len: u32,          // 4 bytes - number of projections
}

// Stored in Cfg::projections: Vec<Projection>
pub enum Projection {
    Field { struct_id: StructId, field_index: u32 },  // 8 bytes
    Index { array_type: Type, index: CfgValue },      // 16 bytes
}
```

### Why Not Intern?

For Rue's current scale, interning adds complexity without clear benefit:
- **Most projection chains are short** (1-3 elements for `arr[i].field`)
- **Duplication is limited** - each function has its own CFG
- **Equality checks are rare** - we mostly iterate projections, not compare them

If profiling shows projection storage is a bottleneck, we can add interning later.

### Comparison: Before vs After

**Before (current):**
```
IndexGet { base: CfgValue, array_type: Type, index: CfgValue }
// 24 bytes per instruction, plus separate Load instruction (another 16+ bytes)
// For arr[i][j]: 2 Load + 2 IndexGet = ~80 bytes
```

**After (proposed):**
```
PlaceRead { place: Place }
// 12 bytes for Place + projection storage
// For arr[i][j]: 1 PlaceRead + 2 projections = ~44 bytes
```

**Net effect**: Smaller IR, fewer instructions, faster compilation.

## Future Work

- **Deref projection**: When pointers/references are added, `Place` can represent `*ptr.field`
- **Slice projections**: Subslice operations like `arr[1..3]`
- **Alias analysis**: Place representation enables tracking which places may alias
- **Place-based optimizations**: Dead store elimination, load forwarding

## References

- [Rust MIR Place struct](https://doc.rust-lang.org/beta/nightly-rustc/rustc_middle/mir/struct.Place.html)
- [Rust MIR ProjectionElem](https://doc.rust-lang.org/nightly/nightly-rustc/rustc_middle/mir/enum.ProjectionElem.html)
- [Rust Compiler Dev Guide - MIR](https://rustc-dev-guide.rust-lang.org/mir/index.html)
- Benchmark showing array_heavy generating 2x more code than other benchmarks
