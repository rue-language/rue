---
id: 0024-revised
title: Type Intern Pool - Revised Approach
status: implemented
tags: [type-system, performance, parallelization]
feature-flag: null
created: 2026-01-02
supersedes: 0024-type-intern-pool.md
---

# ADR-0024 Revised: Type Intern Pool - Simplified Migration

## Status

**Implemented** (2026-01-02) - Phases 1-4 complete. Type is now a u32 newtype with O(1) equality.

## Executive Summary

After multiple failed migration attempts, we discovered that **the pool is already the primary lookup mechanism** for struct/enum definitions. The `struct_defs` and `enum_defs` Vecs are legacy artifacts carried around for "backwards compatibility" but aren't used in the main codepath.

This revised approach:
1. **Removes the Vec duplication** (Phase 2A) - simple cleanup
2. **Keeps the `Type` enum unchanged** - no pattern match migration needed
3. **Migrates arrays to the pool** (Phase 2B) - the real value
4. **Defers `Type` → `TypeId` rename** until generics needs it (Phase 4, optional)

## Context: Why Previous Attempts Failed

### Original ADR-0024 Phase 4
The original plan required:
- Renaming `InternedType` → `Type` globally (~675 usages)
- Updating all pattern matches simultaneously
- Massive compiler errors eating all context (600+ errors)

### Incremental Migration Approach
A previous incremental approach added a `Type::Interned(TypeId)` variant, but:
- Created dual representations that both needed handling
- Every pattern match needed `Type::Interned(_) => panic!(...)` or `.normalize()`
- The migration stalled with 9+ locations needing manual updates

## Key Discovery: Pool Already Primary

Analysis revealed that `SemaContext.get_struct_def()` already uses the pool:

```rust
// sema_context.rs:370-372
pub fn get_struct_def(&self, id: StructId) -> StructDef {
    self.type_pool.struct_def(id)  // Uses pool, NOT Vec!
}
```

The only code using the Vec directly:
- `TypeContext.get_struct_def()` - legacy, limited use
- Test assertions checking `output.struct_defs.len()`
- Logging for struct_count

This means **90%+ of struct/enum lookups already use the pool**.

## Revised Approach

### Guiding Principles

1. **Keep `Type` enum unchanged** - pattern matching works, don't break it
2. **Remove duplication first** - the Vecs are pure overhead
3. **Pool is canonical** - all lookups go through pool
4. **Defer rename to Phase 4** - only if generics specialization needs it

### What Changes

| Component | Current | After Phase 2A | After Phase 2B |
|-----------|---------|----------------|----------------|
| `Sema.struct_defs` | `Vec<StructDef>` | **Removed** | Removed |
| `Sema.enum_defs` | `Vec<EnumDef>` | **Removed** | Removed |
| `TypeContext.struct_defs` | `Vec<StructDef>` | **Removed** | Removed |
| `SemaContext.struct_defs` | `Vec<StructDef>` (unused) | **Removed** | Removed |
| `SemaOutput.struct_defs` | `Vec<StructDef>` | **Pool ref** | Pool ref |
| `ArrayTypeRegistry` | Separate | Separate | **Pool** |
| `Type` enum | 15 variants | **Unchanged** | Unchanged |
| Pattern matches | ~215 locations | **Unchanged** | Unchanged |

### What Stays the Same

- `Type::I32`, `Type::Struct(StructId)` - unchanged
- All pattern matches on `Type` - unchanged
- `StructId`, `EnumId` newtypes - unchanged (they wrap pool indices)
- `ArrayTypeId` - unchanged until Phase 2B

## Implementation Phases

### Phase 1: Infrastructure ✅ (Already Complete)

The pool infrastructure exists and is populated:
- `TypeInternPool` in `intern_pool.rs`
- `type_pool.struct_def(id)` works
- `SemaContext` uses pool for lookups

### Phase 2A: Remove Vec Duplication (NEW - Easy)

**Goal**: Single source of truth for struct/enum definitions.

**Changes**:
1. Remove `struct_defs: Vec<StructDef>` from `Sema`, `TypeContext`, `SemaContext`
2. Remove `enum_defs: Vec<EnumDef>` from same
3. Update `SemaOutput` to provide pool access instead of Vecs
4. Update tests to use `type_pool.struct_count()` instead of `output.struct_defs.len()`
5. Update logging to use pool stats

**Files affected**: ~8-10 files, mostly deletions

**Ship criterion**: All tests pass, no `struct_defs` or `enum_defs` Vecs anywhere.

### Phase 2B: Migrate Arrays to Pool

**Goal**: Array types interned in pool, enabling parallel creation without merging.

**Changes**:
1. Move `ArrayTypeRegistry` functionality into `TypeInternPool`
2. Use `type_pool.intern_array(element, len)` instead of registry
3. Remove `ArrayTypeRegistry` from `SemaContext`
4. Arrays deduplicate automatically (same element+len = same type)

**Files affected**: ~5-10 files

**Ship criterion**: Arrays work, no separate array registry, parallel function analysis cleaner.

### Phase 3: Struct/Enum Unified Indexing (Optional)

**Goal**: `StructId` and `EnumId` are just `TypeId` under the hood.

Currently `StructId(0)` and `EnumId(0)` could both exist (different types). After this phase, all composite types share one index space.

**Changes**:
1. Make `StructId` and `EnumId` aliases for a range of `TypeId`
2. Update pattern matching on `Type::Struct(id)` to extract from TypeId

**Complexity**: Medium. May not be needed if current design works.

### Phase 4: Type Enum → TypeId (Deferred)

**Goal**: Replace `Type` enum with `TypeId(u32)` for O(1) comparison in generics.

**Only do this when**:
- Generics specialization needs canonical type comparison
- `SpecializationKey { type_args: Vec<Type> }` hash collisions become an issue
- We're adding `Vec<T>` and need to intern generic instantiations

**Changes**:
1. Rename `Type` → `TypeKind` (the pattern-matchable form)
2. Make `TypeId` the primary type representation
3. Add `TypeId::kind(&self, pool) -> TypeKind` for pattern matching
4. Migrate storage: `ty: Type` → `ty: TypeId`
5. Migrate patterns: `match ty { Type::I32 => }` → `match ty.kind(pool) { TypeKind::I32 => }`

**Complexity**: High. 200+ pattern matches need updating. Only do if benefits justify cost.

## Benefits of This Approach

### Immediate (Phase 2A)
- **Simpler codebase**: Remove redundant Vec storage
- **Single source of truth**: Pool is canonical
- **No risk**: Just deletions, easy to verify

### Medium-term (Phase 2B)
- **Parallel array creation**: No per-function merging
- **Array deduplication**: `[i32; 5]` same type everywhere
- **Cleaner architecture**: One registry for all composite types

### Long-term (Phase 4, if needed)
- **O(1) type equality**: Critical for generic specialization caching
- **Foundation for generics**: `Vec<i32>` as interned type
- **Future type features**: Pointers, function types, etc.

## Comparison to Original Plan

| Aspect | Original | Revised |
|--------|----------|---------|
| Pattern matches changed | 215+ | **0** (until Phase 4) |
| Files changed (Phase 2) | ~25 | **~10** |
| Risk of breaking changes | High | **Low** |
| Immediate benefit | Low (just infrastructure) | **High** (remove duplication) |
| Type representation | Changes immediately | **Unchanged until needed** |
| Generics support | Required before generics | **Only if needed** |

## Migration Order

```
Phase 1 ✅ (done)
    │
    ▼
Phase 2A: Remove Vec duplication (~1-2 hours)
    │
    ▼
Phase 2B: Migrate arrays to pool (~2-4 hours)
    │
    ▼
[STOP HERE unless generics needs it]
    │
    ▼
Phase 3: Unified indexing (optional, ~2-4 hours)
    │
    ▼
Phase 4: Type→TypeId rename (only if needed, ~8-16 hours)
```

## Files to Change

### Phase 2A (Remove Vecs)

**Delete fields**:
- `crates/rue-air/src/sema/mod.rs`: `struct_defs`, `enum_defs` fields
- `crates/rue-air/src/sema_context.rs`: `struct_defs`, `enum_defs` fields
- `crates/rue-air/src/type_context.rs`: `struct_defs`, `enum_defs` fields

**Update**:
- `crates/rue-air/src/sema/declarations.rs`: Remove `.push()` calls
- `crates/rue-air/src/sema/builtins.rs`: Remove `.push()` call
- `crates/rue-air/src/sema/analysis.rs`: Remove `std::mem::take(&mut sema.struct_defs)`
- `crates/rue-air/src/sema/mod.rs`: Remove Vec cloning in `build_type_context()`
- `crates/rue-air/src/sema/tests.rs`: Use `type_pool.struct_count()` instead
- `crates/rue-compiler/src/lib.rs`: Use `type_pool.struct_count()` for logging

### Phase 2B (Arrays to Pool)

- `crates/rue-air/src/intern_pool.rs`: Already has `intern_array()`
- `crates/rue-air/src/sema_context.rs`: Replace `ArrayTypeRegistry` with pool
- `crates/rue-air/src/sema/analysis.rs`: Use `type_pool.intern_array()`
- `crates/rue-codegen/src/types.rs`: Update array lookups

## Success Criteria

### Phase 2A Complete ✅ (2026-01-02)
- [x] No `struct_defs: Vec<StructDef>` anywhere in codebase
- [x] No `enum_defs: Vec<EnumDef>` anywhere in codebase
- [x] All struct/enum lookups go through `type_pool`
- [x] All tests pass
- [x] `./test.sh` green

### Phase 2B Complete ✅ (2026-01-02)
- [x] No `ArrayTypeRegistry`
- [x] Arrays interned via `type_pool.intern_array()`
- [x] Array deduplication works (same element+len = same ArrayTypeId)
- [x] All tests pass

## Phase 3 & 4: Type Enum → Type(u32) Migration

**Status**: Implemented (2026-01-02)

After completing Phase 2B, we proceeded with Phases 3 and 4 to achieve the full benefits described in the original ADR-0024:
- O(1) type equality via u32 comparison
- Foundation for generic type instantiation
- Unified type representation

### Migration Strategy: "Shadow Type" Approach

The key challenge is migrating ~61 pattern match sites without creating 600+ simultaneous compilation errors. Our approach uses **incremental migration with TypeKind**:

#### Phase 3.1: Introduce TypeKind enum

Create a new `TypeKind` enum that mirrors the current `Type` enum structure:

```rust
// crates/rue-air/src/types.rs
pub enum TypeKind {
    I8, I16, I32, I64, U8, U16, U32, U64,
    Bool, Unit,
    Struct(StructId),
    Enum(EnumId),
    Array(ArrayTypeId),
    Module(ModuleId),
    Error,
    Never,
    ComptimeType,
}
```

**Why**: TypeKind is the pattern-matchable representation of a Type. Separating these concerns allows incremental migration.

#### Phase 3.2: Add Type::kind() method

Add a method to convert Type to TypeKind:

```rust
impl Type {
    pub fn kind(&self) -> TypeKind {
        match self {
            Type::I8 => TypeKind::I8,
            Type::I16 => TypeKind::I16,
            // ... etc for all variants
        }
    }
}
```

**Why**: This allows pattern matches to gradually migrate from `match ty { Type::I32 => }` to `match ty.kind() { TypeKind::I32 => }` while keeping everything compiling.

#### Phase 3.3: Migrate pattern matches incrementally

Migrate one file at a time:

```rust
// Before:
match ty {
    Type::I32 | Type::I64 => emit_integer_op(),
    Type::Struct(id) => {
        let def = pool.struct_def(id);
        emit_struct_op(&def);
    }
    _ => panic!("unexpected type"),
}

// After:
match ty.kind() {
    TypeKind::I32 | TypeKind::I64 => emit_integer_op(),
    TypeKind::Struct(id) => {
        let def = pool.struct_def(id);
        emit_struct_op(&def);
    }
    _ => panic!("unexpected type"),
}
```

**Benefits**:
- Each file compiles and tests pass ✅
- Can ship intermediate states ✅
- Easy to back out if issues arise ✅
- Clear progress tracking (~61 match sites)

#### Phase 4.1: Replace Type enum with Type(InternedType)

Once all pattern matches use `.kind()`, replace the Type enum:

```rust
// Remove the old enum:
// pub enum Type { I8, I16, ... }

// Replace with newtype:
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Type(InternedType);

impl Type {
    // Primitive constants
    pub const I8: Type = Type(InternedType::I8);
    pub const I16: Type = Type(InternedType::I16);
    // ... etc

    // Now kind() does a pool lookup:
    pub fn kind(&self, pool: &TypeInternPool) -> TypeKind {
        if self.0.is_primitive() {
            // Fast path: decode primitive from index
            match self.0.index() {
                0 => TypeKind::I8,
                1 => TypeKind::I16,
                // ... etc
            }
        } else {
            // Composite types: pool lookup
            pool.get_kind(self.0)
        }
    }
}
```

**Why**: Now Type is just a u32 index, giving us O(1) equality. All existing pattern matches continue to work via `.kind()`.

#### Phase 4.2: Update method signatures

Once Type is Type(InternedType), update methods that pattern match:

```rust
// Before (Phase 3):
impl Type {
    pub fn is_integer(&self) -> bool {
        matches!(self.kind(), TypeKind::I8 | TypeKind::I16 | ...)
    }
}

// After (Phase 4, optimized):
impl Type {
    pub fn is_integer(&self) -> bool {
        // No pool lookup needed - just check the index
        matches!(self.0.index(), 0..=7) // i8..u64
    }
}
```

### Success Criteria

#### Phase 3 Complete ✅ (2026-01-02)
- [x] TypeKind enum exists in crates/rue-air/src/types.rs
- [x] Type::kind() method implemented
- [x] All ~61 pattern match sites migrated to use .kind()
- [x] All tests pass
- [x] No direct pattern matches on Type enum remain

#### Phase 4 Complete ✅ (2026-01-02)
- [x] Type enum removed, replaced with Type(u32) newtype
- [x] Type::kind() decodes u32 back to TypeKind for pattern matching
- [x] Type constants (Type::I32, etc.) defined as const Type(n)
- [x] Helper methods (is_integer, as_struct, etc.) optimized with u32 checks
- [x] All tests pass (1230 spec, 275 unit, 38 UI)
- [x] O(1) type equality via u32 comparison works

### Files Affected (Estimated)

**Phase 3.1-3.2** (~1-2 files):
- `crates/rue-air/src/types.rs` - Add TypeKind, Type::kind()

**Phase 3.3** (~20 files, 61 match sites):
- `crates/rue-air/src/sema/analysis.rs` (~19 matches)
- `crates/rue-air/src/sema/typeck.rs` (~9 matches)
- `crates/rue-codegen/src/x86_64/cfg_lower.rs` (~7 matches)
- `crates/rue-compiler/src/drop_glue.rs` (~8 matches)
- `crates/rue-air/src/intern_pool.rs` (~15 matches)
- ... (15 more files with 1-3 matches each)

**Phase 4.1-4.2** (~3-5 files):
- `crates/rue-air/src/types.rs` - Replace enum with newtype
- `crates/rue-air/src/intern_pool.rs` - Add get_kind() method
- `crates/rue-air/src/lib.rs` - Update exports

### Comparison to Big-Bang Approach

| Aspect | Big-Bang | Shadow Type (Our Approach) |
|--------|----------|----------------------------|
| Compilation errors | 600+ all at once | 0 (compiles at each step) |
| Testability | Only at the end | After each file migration |
| Risk | High | Low |
| Context window | Fills with errors | Clean, focused changes |
| Reversibility | Difficult | Easy (one file at a time) |
| Progress tracking | Binary (done/not done) | Linear (~61 match sites) |

### Why This Works

1. **TypeKind is the same structure as Type**: Just a renamed copy, so semantics don't change
2. **Type::kind() starts trivial**: Just returns the enum variant, no pool lookup
3. **Incremental migration**: Each file can be done independently
4. **Final flip is mechanical**: Once all matches use .kind(), replacing the enum is safe

### Implementation Order (Completed)

1. ✅ Add TypeKind enum to types.rs
2. ✅ Add Type::kind() → TypeKind conversion
3. ✅ Migrate pattern matches file by file, testing after each
4. ✅ Replace Type enum with Type(u32) newtype
5. ✅ Optimize Type::kind() and helper methods
6. Kept TypeKind for pattern matching (provides better ergonomics than direct u32 decoding)

## Appendix: Why We Proceeded with Phases 3 & 4

The revised ADR originally recommended stopping after Phase 2B and only proceeding if generics needed it. However, we implemented Phases 3 & 4 because:

1. **Clean foundation**: Better to complete the migration while the architecture is fresh
2. **Original design intent**: The full InternPool design provides clear benefits
3. **Incremental safety**: Our "Shadow Type" approach mitigates the risk that caused the original deferral
4. **Future-proofing**: O(1) type comparison and generic type instantiation will be needed eventually
