---
id: 0020
title: Built-in Types as Synthetic Structs
status: proposal
tags: [architecture, types, refactoring, strings]
feature-flag:
created: 2025-12-31
accepted:
implemented:
spec-sections: []
superseded-by:
---

# ADR-0020: Built-in Types as Synthetic Structs

## Status

Proposal

## Summary

Refactor built-in types like `String` from hardcoded `Type` enum variants into synthetic structs that are injected by the compiler before user code is analyzed. This removes ~50 scattered `Type::String` special cases across the compiler, centralizes built-in type metadata, and establishes an architecture that scales to future built-in types (`Vec<T>`, `HashMap<K,V>`, etc.).

## Context

### The Problem: Scattered Special-Casing

Today, `String` is a primitive variant in the `Type` enum:

```rust
// rue-air/src/types.rs
pub enum Type {
    I8, I16, I32, I64,
    U8, U16, U32, U64,
    Bool, Unit,
    Struct(StructId),
    Enum(EnumId),
    Array(ArrayTypeId),
    String,        // <-- Hardcoded magic
    Error, Never,
}
```

Because `String` is "magic" (a built-in with heap semantics, 3-slot ABI, runtime methods), every compiler phase needs explicit `Type::String` checks. A grep shows ~50 locations:

| Phase | Location | What it does |
|-------|----------|--------------|
| Sema | `sema.rs:4428` | Dispatches `String::new()`, methods |
| Sema | `sema.rs:4864` | Disallows `<`, `>` on strings |
| Sema | `sema.rs:4748` | Returns slot count (3) |
| Type inference | `generate.rs:264` | Infers `StringConst → String` |
| CFG builder | `build.rs:1682` | Marks String as needing drop |
| Codegen (x86) | `cfg_lower.rs:1229` | Emits `__rue_str_eq` call |
| Codegen (x86) | `cfg_lower.rs:1371` | Handles 3-slot alloc |
| Codegen (arm) | `cfg_lower.rs:946` | Same, duplicated |
| Drop glue | `drop_glue.rs:47` | String needs drop |
| Drop glue | `drop_glue.rs:85` | String has 3 slots |

### Why This Doesn't Scale

If we wanted to add `Vec<T>`, `HashMap<K,V>`, or even `&str`, we'd need to:

1. Add new `Type` variants
2. Hunt down every `match` on `Type` and add new cases
3. Duplicate logic across both codegen backends (x86_64, aarch64)
4. Handle special ABIs (multi-slot representations)
5. Wire up runtime functions manually

### How Zig and Rust Handle This

**Zig**: No built-in `String` type at all. Strings are `[]u8` (a compiler primitive slice). Growable strings are `std.ArrayList(u8)` — pure library code using comptime generics. The compiler only knows primitives, pointers, slices, and user-defined types.

**Rust**: Uses "lang items" — markers like `#[lang = "owned_box"]` that tell the compiler "this library type implements this language concept." The compiler knows about traits (`Drop`, `Eq`, `Deref`) but `String` and `Vec<T>` are plain library structs that *use* those traits. The compiler doesn't special-case them directly.

**Rue Today**: Hard-codes `String` as a compiler primitive, requiring scattered special-case code everywhere.

### The Insight

`String` isn't fundamentally different from a user-defined struct — it's just a struct whose methods are implemented in the runtime rather than generated from Rue source. If the compiler sees it as "just a struct," we can unify the handling.

## Decision

### Core Idea: Synthetic Structs

Introduce the concept of **synthetic structs**: struct types that are injected by the compiler before user code is parsed, with methods that map to runtime functions rather than generated code.

From the type system's perspective, `String` becomes:

```rust
// Conceptually what the compiler "sees"
struct String {
    ptr: u64,   // Actually *mut u8, but we don't have pointers yet
    len: u64,
    cap: u64,
}
```

The `Type` enum loses its `String` variant:

```rust
pub enum Type {
    I8, I16, I32, I64,
    U8, U16, U32, U64,
    Bool, Unit,
    Struct(StructId),   // String is StructId(0) or similar
    Enum(EnumId),
    Array(ArrayTypeId),
    // No Type::String!
    Error, Never,
}
```

### Builtin Type Registry

Create a central registry that describes built-in types:

```rust
// New module: rue-builtins or within rue-air

/// Descriptor for a built-in type's properties
pub struct BuiltinTypeDef {
    /// Type name as it appears in source code
    pub name: &'static str,
    /// Field definitions for the synthetic struct
    pub fields: &'static [BuiltinField],
    /// Whether this type is Copy (can be implicitly duplicated)
    pub is_copy: bool,
    /// Runtime function to call for drop, if any
    pub drop_fn: Option<&'static str>,
    /// Supported operators and their runtime implementations
    pub operators: &'static [BuiltinOperator],
    /// Associated functions (e.g., String::new)
    pub associated_fns: &'static [BuiltinAssociatedFn],
    /// Instance methods (e.g., s.len())
    pub methods: &'static [BuiltinMethod],
}

pub struct BuiltinField {
    pub name: &'static str,
    pub ty: BuiltinFieldType,
}

pub enum BuiltinFieldType {
    U64,
    // Add more as needed
}

pub struct BuiltinOperator {
    pub op: BinOp,
    pub runtime_fn: &'static str,
}

pub struct BuiltinAssociatedFn {
    pub name: &'static str,
    pub params: &'static [(&'static str, BuiltinFieldType)],
    pub return_slots: u32,  // Number of return slots (3 for String)
    pub runtime_fn: &'static str,
}

pub struct BuiltinMethod {
    pub name: &'static str,
    pub receiver_mode: ReceiverMode,  // ByValue, ByRef, ByMutRef
    pub params: &'static [(&'static str, BuiltinFieldType)],
    pub return_ty: Option<BuiltinFieldType>,
    pub runtime_fn: &'static str,
}
```

The `String` type is defined as:

```rust
pub static STRING_TYPE: BuiltinTypeDef = BuiltinTypeDef {
    name: "String",
    fields: &[
        BuiltinField { name: "ptr", ty: BuiltinFieldType::U64 },
        BuiltinField { name: "len", ty: BuiltinFieldType::U64 },
        BuiltinField { name: "cap", ty: BuiltinFieldType::U64 },
    ],
    is_copy: false,
    drop_fn: Some("__rue_drop_String"),
    operators: &[
        BuiltinOperator { op: BinOp::Eq, runtime_fn: "__rue_str_eq" },
        BuiltinOperator { op: BinOp::Ne, runtime_fn: "__rue_str_eq" }, // Inverted
    ],
    associated_fns: &[
        BuiltinAssociatedFn {
            name: "new",
            params: &[],
            return_slots: 3,
            runtime_fn: "String__new",
        },
        BuiltinAssociatedFn {
            name: "with_capacity",
            params: &[("cap", BuiltinFieldType::U64)],
            return_slots: 3,
            runtime_fn: "String__with_capacity",
        },
    ],
    methods: &[
        BuiltinMethod {
            name: "len",
            receiver_mode: ReceiverMode::ByRef,
            params: &[],
            return_ty: Some(BuiltinFieldType::U64),
            runtime_fn: "String__len",
        },
        BuiltinMethod {
            name: "capacity",
            receiver_mode: ReceiverMode::ByRef,
            params: &[],
            return_ty: Some(BuiltinFieldType::U64),
            runtime_fn: "String__capacity",
        },
        BuiltinMethod {
            name: "is_empty",
            receiver_mode: ReceiverMode::ByRef,
            params: &[],
            return_ty: Some(BuiltinFieldType::U64), // Returns 0 or 1
            runtime_fn: "String__is_empty",
        },
        BuiltinMethod {
            name: "clone",
            receiver_mode: ReceiverMode::ByRef,
            params: &[],
            return_ty: None, // Returns String (3 slots)
            runtime_fn: "String__clone",
        },
        BuiltinMethod {
            name: "push_str",
            receiver_mode: ReceiverMode::ByMutRef,
            params: &[("other", BuiltinFieldType::U64)], // Actually String
            return_ty: None,
            runtime_fn: "String__push_str",
        },
        // ... more methods
    ],
};

pub static BUILTIN_TYPES: &[&BuiltinTypeDef] = &[
    &STRING_TYPE,
    // Future: &VEC_TYPE, &HASHMAP_TYPE, etc.
];
```

### StructDef Changes

Add a flag to identify synthetic structs:

```rust
pub struct StructDef {
    pub name: String,
    pub fields: Vec<StructField>,
    pub is_copy: bool,
    pub destructor: Option<String>,
    pub is_builtin: bool,  // NEW: true for synthetic structs
}
```

### Injection Point

During `Sema::gather_declarations()`, before processing user code:

```rust
impl<'a> Sema<'a> {
    pub fn gather_declarations(&mut self) -> Result<...> {
        // NEW: Inject built-in types first
        self.inject_builtin_types();

        // Then process user declarations as before
        self.gather_user_declarations()?;
        // ...
    }

    fn inject_builtin_types(&mut self) {
        for builtin in BUILTIN_TYPES {
            let struct_id = StructId(self.struct_defs.len() as u32);

            // Create StructDef from builtin descriptor
            let struct_def = StructDef {
                name: builtin.name.to_string(),
                fields: builtin.fields.iter().map(|f| StructField {
                    name: f.name.to_string(),
                    ty: f.ty.to_type(),
                }).collect(),
                is_copy: builtin.is_copy,
                destructor: builtin.drop_fn.map(|s| s.to_string()),
                is_builtin: true,
            };

            self.struct_defs.push(struct_def);

            // Register in struct lookup
            let name_spur = self.interner.get_or_intern(builtin.name);
            self.structs.insert(name_spur, struct_id);

            // Register associated functions
            for assoc_fn in builtin.associated_fns {
                self.register_builtin_associated_fn(struct_id, assoc_fn);
            }

            // Register methods
            for method in builtin.methods {
                self.register_builtin_method(struct_id, method);
            }
        }
    }
}
```

### Sema Changes

Replace `Type::String` checks with builtin struct queries:

```rust
// Before:
if ty == Type::String {
    // Special string handling
}

// After:
if self.is_builtin_type(ty, "String") {
    // Uses centralized builtin registry
}

// Or for slot counts:
fn type_slot_count(&self, ty: Type) -> u32 {
    match ty {
        Type::Struct(id) => {
            let def = &self.struct_defs[id.0 as usize];
            def.fields.iter().map(|f| self.type_slot_count(f.ty)).sum()
        }
        // No Type::String case needed!
        _ => 1,
    }
}
```

Method dispatch becomes uniform:

```rust
// Before (sema.rs:4428):
if receiver_type == Type::String {
    return self.analyze_string_method_call(receiver, method_name, args, span);
}

// After:
if let Type::Struct(struct_id) = receiver_type {
    let struct_def = &self.struct_defs[struct_id.0 as usize];
    if struct_def.is_builtin {
        return self.analyze_builtin_method_call(struct_id, receiver, method_name, args, span);
    }
}
```

### Codegen Changes

The codegen doesn't need to know about `Type::String` at all. It sees a struct with 3 fields and generates code accordingly. The only special handling is for runtime function calls:

```rust
// Before (cfg_lower.rs):
if lhs_ty == Type::String {
    let vreg = self.emit_string_eq_call(*lhs, *rhs);
    // ...
}

// After:
if let Some(runtime_fn) = self.get_builtin_operator(lhs_ty, BinOp::Eq) {
    let vreg = self.emit_runtime_call(runtime_fn, &[lhs, rhs]);
    // ...
}
```

### Drop Glue Changes

The existing drop glue system already handles structs with destructors. With `is_builtin: true` and `destructor: Some("__rue_drop_String")`, the drop glue synthesizer will correctly generate calls to the runtime drop function.

```rust
// drop_glue.rs - no changes needed for String specifically!
// The existing code handles structs with destructors:
if let Some(dtor) = &struct_def.destructor {
    // Emit call to dtor
}
```

### StringConst Handling

String literals still need special handling because they create values from data in `.rodata`. The `StringConst` AIR instruction remains, but its type becomes the synthetic String struct:

```rust
// In sema.rs, when analyzing a string literal:
let string_idx = self.add_string(content);
let air_ref = self.air.add_inst(AirInst {
    data: AirInstData::StringConst(string_idx),
    ty: self.builtin_string_type(),  // Returns Type::Struct(string_struct_id)
    span,
});
```

## Implementation Phases

**Epic**: rue-c8lp

### Phase 1: Builtin Registry Infrastructure

**Issues**: rue-fgx3 (crate), rue-cbsc (injection)

**Goal**: Create the builtin type registry without changing existing behavior.

**Tasks**:
- Create `rue-builtins` crate
- Define `BuiltinTypeDef` and related types
- Define `STRING_TYPE` with all current String operations
- Add `is_builtin` field to `StructDef`
- Add builtin injection to `Sema::gather_declarations()`
- Store the synthetic String's `StructId` for later reference
- Error if user defines type with reserved name

**Verification**: All existing tests pass. String is now also a synthetic struct (but `Type::String` still exists in parallel).

### Phase 2: Migrate Sema

**Issue**: rue-hp13

**Goal**: Replace `Type::String` checks in semantic analysis with struct-based queries.

**Tasks**:
- Add helper methods: `is_builtin_type()`, `get_builtin_operator()`, `get_builtin_method()`
- Migrate `analyze_type_name()` to recognize String as a struct
- Migrate associated function dispatch (`String::new`, `String::with_capacity`)
- Migrate method dispatch (`.len()`, `.push_str()`, etc.)
- Migrate operator restriction (no `<`, `>` on strings)
- Migrate slot counting to use struct fields

**Verification**: All spec tests and unit tests pass.

### Phase 3: Migrate Codegen

**Issues**: rue-s6mk (x86_64), rue-tco7 (aarch64), rue-5cfw (other)

**Goal**: Replace `Type::String` checks in both backends and remaining crates.

**Tasks**:
- Add `get_builtin_operator()` lookup to codegen context
- Migrate x86_64 `cfg_lower.rs`:
  - `Eq`/`Ne` operators → runtime call lookup
  - `Alloc` for strings → struct field handling
  - `Load`/`Store` for strings → struct handling
  - `Call` with string args/returns → struct ABI
  - `Drop` → existing struct drop path
- Mirror all changes in aarch64 `cfg_lower.rs`
- Migrate rue-cfg, rue-compiler/drop_glue, rue-codegen/types

**Verification**: All tests pass on both architectures.

### Phase 4: Remove Type::String

**Issue**: rue-bmje

**Goal**: Delete the `Type::String` variant entirely.

**Tasks**:
- Remove `Type::String` from the enum
- Remove `Type::is_string()` method
- Fix any remaining compile errors (there should be none if phases 2-3 were thorough)
- Update type name formatting to use struct name

**Verification**: Compiler builds, all tests pass, `Type::String` no longer exists.

### Phase 5: Documentation and Cleanup

**Issue**: rue-n20l

**Goal**: Document the new architecture for future contributors.

**Tasks**:
- Add documentation to `rue-builtins` explaining how to add new built-in types
- Update CLAUDE.md with builtin type information
- Remove any dead code from the migration

## Consequences

### Positive

- **Scalability**: Adding `Vec<T>` becomes "add an entry to `BUILTIN_TYPES`" instead of editing 50 files
- **Consistency**: Built-in types follow the same code paths as user-defined types
- **Maintainability**: Builtin type behavior is centralized in one registry
- **Backend uniformity**: Both x86_64 and aarch64 share the same builtin definitions
- **Foundation for generics**: When generics land, `Vec<T>` follows the same pattern
- **Foundation for lang items**: The registry is a stepping stone toward Rust-style lang items

### Negative

- **Initial complexity**: Adding the registry infrastructure before removing `Type::String`
- **Migration risk**: Phased migration requires careful testing at each step
- **Indirection**: Looking up builtin properties is slightly more indirect than `match Type::String`

### Neutral

- **No user-visible change**: The language behaves identically
- **Different from Zig**: Zig has no built-in String; we still do, but it's internally a struct
- **Similar to Rust's outcome**: Rust's `String` is also "just a struct" in the type system

## Design Decisions

1. **Where does the registry live?** New `rue-builtins` crate. This provides the cleanest separation of concerns and makes the builtin type definitions easy to find and modify.

2. **How do we handle String literals in inference?** The `StringConst` instruction needs to know the String struct's ID. The registry will return the `StructId` after injection, and we'll store it in a well-known field (e.g., `Sema::builtin_string_id`) for fast access.

3. **Should builtin structs be visible to users?** Yes, for now. Users can technically construct `String { ptr: 0, len: 0, cap: 0 }`. This is intentionally deferred — when the standard library and privacy/visibility rules land, those mechanisms will cleanly hide the internal fields. No need for a special `is_opaque` flag.

4. **How do we prevent users from defining their own `String` type?** During declaration gathering, after injecting builtins, we check user-defined type names against the builtin registry. If a collision is found, emit an error: "cannot define type `String`: name is reserved for built-in type".

5. **String literal optimization**: String literals from `.rodata` use `cap: 0` as a sentinel — the drop function checks this and skips freeing. This is the current behavior and remains correct.

6. **Builtin method error messages**: Treat uniformly. When a user calls a non-existent method on String, the error message should be the same as for any other struct (e.g., "no method `foo` on type `String`"). No special "this is a built-in type" messaging.

## Open Questions

None at this time.

## Future Work

- **Phase 2+ built-in types**: `Vec<T>`, `HashMap<K,V>`, `Box<T>` following the same pattern
- **Lang items**: Evolve the registry toward trait-based lang items when traits land
- **Opaque types**: Prevent users from constructing built-in types directly
- **Generic builtins**: When generics land, extend the registry to support type parameters

## References

- [Zig Documentation - No built-in string type](https://ziglang.org/documentation/master/)
- [Zig ArrayList implementation](https://github.com/ziglang/zig/blob/master/lib/std/array_list.zig)
- [Rust Lang Items](https://doc.rust-lang.org/unstable-book/language-features/lang-items.html)
- [Rust Tidbits: What Is a Lang Item?](https://manishearth.github.io/blog/2017/01/11/rust-tidbits-what-is-a-lang-item/)
- [ADR-0010: Destructors](0010-destructors.md) — Drop glue infrastructure
- [ADR-0014: Mutable Strings](0014-mutable-strings.md) — Current String implementation
