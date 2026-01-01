---
id: 0024
title: Type Intern Pool
status: proposal
tags: [type-system, performance, parallelization]
feature-flag: null
created: 2026-01-01
accepted:
implemented:
spec-sections: []
superseded-by:
---

# ADR-0024: Type Intern Pool

## Status

Proposal

## Summary

Replace Rue's current type representation (a `Type` enum with separate ID registries for structs, enums, and arrays) with a unified **Type Intern Pool** inspired by Zig's `InternPool`. All types become 32-bit indices into a canonical, thread-safe pool, enabling O(1) type equality, efficient memory usage, and clean parallel compilation.

## Context

### Current Architecture

Rue currently has three separate ID systems for composite types:

| Type | ID Type | Storage | Creation Point |
|------|---------|---------|----------------|
| Structs | `StructId(u32)` | `Vec<StructDef>` in `TypeContext` | Declaration collection (pre-analysis) |
| Enums | `EnumId(u32)` | `Vec<EnumDef>` in `TypeContext` | Declaration collection (pre-analysis) |
| Arrays | `ArrayTypeId(u32)` | `Vec<ArrayTypeDef>` in `FunctionAnalysisState` | During function body analysis |

The `Type` enum is:
```rust
pub enum Type {
    I8, I16, I32, I64, U8, U16, U32, U64,
    Bool, Unit, Error, Never,
    Struct(StructId),
    Enum(EnumId),
    Array(ArrayTypeId),
}
```

### Problems

1. **Dynamic array type creation**: Arrays are created during type inference, requiring mutable state during what should be parallel function analysis. This led to `FunctionAnalysisState` with per-function array registries that must be merged afterward (see rue-wcg7).

2. **Inconsistent creation patterns**: Structs/enums use one path, arrays use another. This asymmetry complicates the codebase.

3. **Type comparison overhead**: Comparing types requires matching on enum variants. For composite types, you then need to compare IDs, which requires knowing they came from the same registry.

4. **Future generics**: When we add `Vec<T>` or `Option<T>`, we'll need to intern instantiated generic types like `Vec<i32>` vs `Vec<String>`. The current architecture has no path to this.

### What Zig Does

Zig uses an `InternPool` - a canonical, thread-safe, sharded hash table that deduplicates and interns all types and values:

- Types are represented as 32-bit indices
- The `Key` union defines all possible type variants (including `ArrayType { len, child, sentinel }`)
- Content-addressed deduplication: identical types share the same index
- Type equality is just `u32 == u32`
- Thread-safe via sharded hash maps with atomic operations

## Decision

Implement a unified `TypeInternPool` for all types in Rue.

### Core Design

```rust
/// Interned type index - 32 bits, Copy, cheap comparison.
///
/// Reserved indices 0-15 are primitives (no lookup needed).
/// Index 16+ are composite types stored in the pool.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Type(u32);

impl Type {
    // Reserved indices for primitives
    pub const I8: Type = Type(0);
    pub const I16: Type = Type(1);
    pub const I32: Type = Type(2);
    pub const I64: Type = Type(3);
    pub const U8: Type = Type(4);
    pub const U16: Type = Type(5);
    pub const U32: Type = Type(6);
    pub const U64: Type = Type(7);
    pub const BOOL: Type = Type(8);
    pub const UNIT: Type = Type(9);
    pub const NEVER: Type = Type(10);
    pub const ERROR: Type = Type(11);
    // 12-15 reserved for future primitives

    const PRIMITIVE_COUNT: u32 = 16;

    /// Check if this is a primitive type (no pool lookup needed).
    #[inline]
    pub fn is_primitive(self) -> bool {
        self.0 < Self::PRIMITIVE_COUNT
    }
}

/// Type data stored in the intern pool.
///
/// This is NOT Copy - it lives in the pool. You work with `Type` indices.
pub enum TypeData {
    /// User-defined struct (nominal type)
    Struct(StructDef),
    /// User-defined enum (nominal type)
    Enum(EnumDef),
    /// Fixed-size array (structural type)
    Array { element: Type, len: u64 },
    // Future: Pointer, Function, Generic instantiations, etc.
}

/// Thread-safe intern pool for all composite types.
pub struct TypeInternPool {
    inner: RwLock<TypeInternPoolInner>,
}

struct TypeInternPoolInner {
    /// All composite type data, indexed by (Type.0 - PRIMITIVE_COUNT)
    types: Vec<TypeData>,

    /// Structural type deduplication: (element, len) -> Type for arrays
    array_map: HashMap<(Type, u64), Type>,

    /// Nominal type lookup: name -> Type for structs/enums
    struct_by_name: HashMap<Spur, Type>,
    enum_by_name: HashMap<Spur, Type>,
}
```

### Key Properties

1. **O(1) type equality**: `type_a == type_b` is just `u32 == u32`

2. **Primitives are free**: No lookup for `i32`, `bool`, etc. - they're encoded in the index itself

3. **Structural deduplication for arrays**: `[i32; 5]` interns to the same `Type` regardless of where it's created

4. **Nominal identity for structs/enums**: Two structs with the same fields but different names are different types (as they should be)

5. **Thread-safe**: RwLock (or sharded locks) for concurrent access during parallel compilation

6. **Self-contained**: Arrays can reference other types via their `Type` index, enabling nested types like `[[i32; 3]; 4]`

### API

```rust
impl TypeInternPool {
    /// Create a new pool with primitives pre-registered.
    pub fn new() -> Self;

    /// Intern an array type (structural - deduplicates).
    pub fn intern_array(&self, element: Type, len: u64) -> Type;

    /// Register a new struct (nominal - no deduplication).
    /// Returns the Type and whether it was newly inserted.
    pub fn register_struct(&self, name: Spur, def: StructDef) -> (Type, bool);

    /// Register a new enum (nominal - no deduplication).
    pub fn register_enum(&self, name: Spur, def: EnumDef) -> (Type, bool);

    /// Look up a struct by name.
    pub fn get_struct_by_name(&self, name: Spur) -> Option<Type>;

    /// Look up an enum by name.
    pub fn get_enum_by_name(&self, name: Spur) -> Option<Type>;

    /// Get type data (panics for primitives - use Type::is_primitive first).
    pub fn get(&self, ty: Type) -> &TypeData;

    /// Get type data if composite, None for primitives.
    pub fn try_get(&self, ty: Type) -> Option<&TypeData>;

    // Convenience methods
    pub fn is_struct(&self, ty: Type) -> bool;
    pub fn is_enum(&self, ty: Type) -> bool;
    pub fn is_array(&self, ty: Type) -> bool;
    pub fn get_struct_def(&self, ty: Type) -> Option<&StructDef>;
    pub fn get_enum_def(&self, ty: Type) -> Option<&EnumDef>;
    pub fn get_array_info(&self, ty: Type) -> Option<(Type, u64)>; // (element, len)
}
```

### Migration Strategy

The key insight is that `Type` remains a small, Copy value - we're just changing its internal representation from an enum to an index. Most code that uses `Type` doesn't need to change semantically; it just needs mechanical updates.

## Implementation Phases

**Epic**: rue-igt6

### Phase 1: Introduce TypeInternPool alongside existing system (rue-3mjg)

Create the new `TypeInternPool` infrastructure without removing the old system. Both coexist temporarily.

- [ ] Create `rue-air/src/intern_pool.rs` with `TypeInternPool`, `TypeData`
- [ ] Add `TypeInternPool` to `Sema` and `TypeContext`
- [ ] Populate pool during declaration collection (structs, enums)
- [ ] Verify pool contents match existing registries (test coverage)

**Ship criterion**: All existing tests pass, pool is populated but not yet used.

### Phase 2: Migrate array types to the pool (rue-9e5t)

Replace `ArrayTypeId` and the per-function array type handling with pool interning.

- [ ] Replace `FunctionAnalysisState.array_types` with `TypeInternPool.intern_array()`
- [ ] Update `MergedAnalysisState` - array merging becomes a no-op (pool handles dedup)
- [ ] Update AIR instructions that reference `ArrayTypeId` to use `Type`
- [ ] Remove `ArrayTypeId`, `ArrayTypeDef` as separate concepts

**Ship criterion**: Arrays work, parallel function analysis uses shared pool, no per-function array registries.

### Phase 3: Migrate struct/enum IDs to pool indices (rue-ej3x)

Replace `StructId` and `EnumId` with `Type` indices directly.

- [ ] Change `Type::Struct(StructId)` → composite `Type` index
- [ ] Change `Type::Enum(EnumId)` → composite `Type` index
- [ ] Update all `StructId`/`EnumId` usages in AIR, CFG, codegen
- [ ] Remove `StructId`, `EnumId` as separate types

**Ship criterion**: `Type` is now a u32 index. `StructId`/`EnumId` no longer exist.

### Phase 4: Unify Type representation (rue-wsny)

Replace the `Type` enum entirely with the `Type(u32)` newtype.

- [ ] Remove old `Type` enum variants
- [ ] Update all pattern matches on `Type` to use pool queries
- [ ] Add helper methods to `Type` for common checks (`is_integer()`, `is_signed()`, etc.)
- [ ] Optimize: inline primitive checks (no pool lookup for `Type::I32.is_integer()`)

**Ship criterion**: Single unified type representation. Clean API.

### Phase 5: Performance optimization (rue-ynk5, optional)

If profiling shows lock contention:

- [ ] Implement sharded locks (Zig uses 16 shards)
- [ ] Consider lock-free reads for the common case (append-only types array)
- [ ] Benchmark and tune

**Ship criterion**: No regression from current performance. Improvements for parallel compilation.

## Consequences

### Positive

1. **Correctness**: Single source of truth for types eliminates registry synchronization bugs

2. **Performance**:
   - O(1) type comparison (u32 equality vs enum matching)
   - Better cache locality (types are indices, pool is contiguous)
   - Clean parallel compilation (no per-function merging for arrays)

3. **Simplicity**:
   - One way to create and compare types
   - Removes `StructId`, `EnumId`, `ArrayTypeId` - just `Type`
   - `FunctionAnalysisState` becomes much simpler (no array handling)

4. **Future-ready**:
   - Clear path to generic type instantiation (`Vec<i32>`)
   - Pointer types, function types, etc. fit naturally
   - Foundation for incremental compilation (stable type hashes)

### Negative

1. **Large refactor**: ~4000-5000 lines across ~15-20 files

2. **Indirection for type queries**: `pool.get(ty)` instead of direct enum match
   - Mitigated by helper methods and inlined primitive checks

3. **Lock overhead for type creation**: RwLock for concurrent access
   - Mitigated by read-heavy workload (types created once, queried many times)
   - Can shard if profiling shows contention

4. **Learning curve**: Contributors need to understand the interning pattern

## Open Questions

1. **Sharding from the start?** Zig uses 16 shards. We could start with a single RwLock and shard later if needed. The API doesn't change.

2. **Primitive representation**: Reserved indices 0-15 vs. separate enum? Reserved indices are simpler and avoid branching.

3. **Error handling for invalid Type indices**: Panic (current approach) or Result? Panic is fine for compiler internals - an invalid Type index is always a bug.

## Future Work

- **Generic types**: `Vec<T>` instantiation will intern `Vec<i32>` etc. The pool design supports this naturally.
- **Pointer types**: `&T`, `&mut T` would be interned composite types
- **Function types**: For function pointers or closures
- **Incremental compilation**: Pool contents can be serialized/hashed for incremental cache keys

## References

- [Zig InternPool PR #15569](https://github.com/ziglang/zig/pull/15569)
- [Zig InternPool.zig source](https://github.com/ziglang/zig/blob/master/src/InternPool.zig)
- [rue-50gf: Original issue on array type design](rue-50gf)
- [rue-wcg7: Parallel function analysis](rue-wcg7)
