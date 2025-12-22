# ADR-019: Mutable Strings

## Status

Accepted

## Context

Rue currently has immutable string literals stored in `.rodata`. Strings are represented as fat pointers `(ptr, len)` pointing to read-only memory. This is sufficient for printing and comparison but doesn't support:

- Building strings dynamically (concatenation, formatting)
- Reading strings from I/O (stdin, files)
- Modifying string contents

To enable I/O and dynamic string manipulation, we need mutable, heap-allocated strings.

## Decision

Extend `String` to support mutation with these properties:

1. **Copy semantics**: Assigning a string duplicates it (deep copy)
2. **Scope-based cleanup**: Strings are freed when they go out of scope
3. **Literal promotion**: String literals start in `.rodata` and promote to heap on first mutation

This approach is simple to implement, safe by default, and provides a clean path to optimization via linear types later.

### Representation

Change `String` from a 2-tuple to a 3-tuple:

```
String = (ptr: *u8, len: u64, capacity: i64)
```

The `capacity` field serves double duty:

| Capacity Value | Meaning |
|----------------|---------|
| `-1` | Pointer is to `.rodata` (immutable literal) |
| `>= 0` | Pointer is to heap, can hold up to `capacity` bytes |

This uses a single field to distinguish storage location and track available space.

### String Literals

String literals compile to:
- Pointer to `.rodata` section (as today)
- Length of the string
- Capacity of `-1`

```rue
let s = "hello";  // ptr=<rodata>, len=5, cap=-1
```

No heap allocation occurs for literals that are never mutated.

### Copy Semantics

When a string is copied (assignment, passed by value), a deep copy is made:

```rue
let a = "hello";   // a: rodata string
let b = a;         // b: new heap allocation with copy of "hello"
a = a + "!";       // a: promotes to heap, appends "!"
// a = "hello!", b = "hello" (independent)
```

For literals specifically:
- Copying a literal creates a heap-allocated copy
- The original variable still points to `.rodata`

This ensures true value semantics: modifications to one variable never affect another.

### Mutation and Promotion

When a mutating operation is called on a `.rodata` string:

1. Allocate heap buffer (with growth room)
2. Copy bytes from `.rodata` to heap
3. Update `(ptr, len, capacity)` to reflect heap storage
4. Perform the mutation

```rue
let mut s = "hello";     // rodata
s = s + " world";        // promotes to heap, appends
// s is now (heap_ptr, 11, 16) or similar
```

### Scope-Based Cleanup

Strings are freed when they go out of scope:

```rue
fn example() {
    let s = "hello" + " world";  // heap allocated
    // use s...
}  // s is freed here
```

For `.rodata` strings (capacity = -1), no deallocation occurs.

### Operations

Initial mutating operations:

| Operation | Signature | Behavior |
|-----------|-----------|----------|
| Concatenation | `String + String -> String` | Returns new string with combined contents |
| Push | `push(inout s: String, c: u8)` | Appends byte to string |
| Append | `append(inout s: String, other: String)` | Appends other's contents |
| Clear | `clear(inout s: String)` | Sets length to 0, keeps capacity |

Future operations (not in initial implementation):
- `pop() -> Option<u8>` - Remove and return last byte
- `insert(index, byte)` - Insert at position
- `remove(index) -> u8` - Remove at position
- Slicing and substrings

### Memory Management

The runtime needs a heap allocator. Minimal implementation:

```rust
// In rue-runtime
fn __rue_alloc(size: u64) -> *mut u8 {
    // Use mmap for simplicity
    // Returns null on failure (or panic)
}

fn __rue_realloc(ptr: *mut u8, old_size: u64, new_size: u64) -> *mut u8 {
    // Grow or shrink allocation
}

fn __rue_dealloc(ptr: *mut u8, size: u64) {
    // munmap the region
}
```

String-specific functions:

```rust
fn __rue_string_promote(rodata_ptr: *const u8, len: u64) -> (*mut u8, u64, i64) {
    // Allocate heap buffer, copy from rodata, return new tuple
}

fn __rue_string_grow(ptr: *mut u8, len: u64, cap: i64, additional: u64) -> (*mut u8, i64) {
    // Grow capacity if needed, return new ptr and cap
}

fn __rue_string_drop(ptr: *mut u8, len: u64, cap: i64) {
    if cap >= 0 {
        __rue_dealloc(ptr, cap as u64);
    }
    // rodata strings (cap == -1) are not freed
}

fn __rue_string_clone(ptr: *const u8, len: u64, cap: i64) -> (*mut u8, u64, i64) {
    // Always creates a heap copy
}
```

### Codegen Changes

#### MIR Instructions

Replace current string instructions:

```rust
// Old (x86_64/mir.rs, aarch64/mir.rs)
StringConstPtr { dst, string_id },
StringConstLen { dst, string_id },

// New
StringConst { dst_ptr, dst_len, dst_cap, string_id },  // Load all 3 components
```

Or keep separate loads for register allocation flexibility:

```rust
StringConstPtr { dst, string_id },
StringConstLen { dst, string_id },
StringConstCap { dst, string_id },  // Always loads -1 for literals
```

#### Drop Insertion

The compiler must insert drops at scope exits:

```rust
// In CFG building or a new pass
fn insert_drops(cfg: &mut Cfg, scope: &Scope) {
    for var in scope.string_variables() {
        cfg.insert_before_exit(CfgInst::DropString { var });
    }
}
```

The `DropString` instruction lowers to a call to `__rue_string_drop`.

#### Copy Insertion

On assignment, insert clone:

```rue
let a = "hello";
let b = a;  // Insert: b = __rue_string_clone(a.ptr, a.len, a.cap)
```

### Type System Considerations

`String` remains a single type - no distinction between literal and heap strings at the type level. The representation difference is an implementation detail.

For function parameters:
- `fn f(s: String)` - Receives a copy (caller's string is cloned)
- `fn f(inout s: String)` - Exclusive mutable access, no copy
- `fn f(s: move String)` - Takes ownership (future, with linear types)

### Interaction with Ownership Modes

Currently, `String` has implicit copy semantics. When ownership modes are implemented:

| Mode | Assignment | Pass by value | Pass `inout` |
|------|------------|---------------|--------------|
| `value String` (current) | Deep copy | Deep copy | Exclusive access |
| `move String` (future) | Transfer | Transfer | Exclusive access |
| `linear String` (future) | Transfer | Transfer | Exclusive access |

Linear types will allow eliding copies when the compiler can prove the source is not used after the copy.

## Implementation Phases

### Phase 1: Heap Allocator

Add to `rue-runtime`:
- `__rue_alloc`, `__rue_realloc`, `__rue_dealloc` using `mmap`/`munmap`
- Simple bump allocator or free-list for efficiency

### Phase 2: Representation Change

Update all stages for 3-tuple representation:

1. **AIR** (`rue-air/src/types.rs`): Update `Type::String` documentation
2. **CFG** (`rue-cfg`): String values carry 3 components
3. **Codegen** (`rue-codegen`):
   - Add `StringConstCap` MIR instruction (or combined `StringConst`)
   - Update lowering to load capacity = -1 for literals
   - Both x86_64 and aarch64 backends
4. **Runtime**: Add `__rue_string_drop`, `__rue_string_clone`

### Phase 3: Drop Insertion

Add compiler pass to insert `DropString` at scope exits:
- Track which variables are strings
- Insert drops in reverse declaration order
- Handle early returns, breaks, continues

### Phase 4: Copy Insertion

Insert clones on string assignment:
- `let b = a` where `a: String` becomes clone
- Passing string by value becomes clone
- Returning string may need special handling

### Phase 5: Mutation Operations

Add string manipulation:
- `+` operator for concatenation
- `push`, `append`, `clear` functions
- Promotion logic in runtime functions

### Phase 6: I/O Integration

With mutable strings, implement:
- `read_line() -> String` - Read from stdin
- File reading (future)

## File Changes Summary

| Crate | Changes |
|-------|---------|
| `rue-runtime` | Add allocator, string functions |
| `rue-air` | Update String type docs |
| `rue-cfg` | Track 3-component strings |
| `rue-codegen/x86_64` | StringConstCap, DropString, CloneString |
| `rue-codegen/aarch64` | Same as x86_64 |
| `rue-linker` | No changes (rodata handling unchanged) |

## Consequences

### Positive

- **Enables I/O**: Can read strings from external sources
- **Value semantics**: Simple mental model - copies are independent
- **No refcounting overhead**: Direct ownership, no runtime tracking
- **Literal optimization**: No heap allocation for immutable literals
- **Path to optimization**: Linear types can elide copies later

### Negative

- **Copy overhead**: Every assignment/pass duplicates the string
- **Memory usage**: Multiple copies of same data
- **No sharing**: Can't efficiently share large strings

### Mitigations

The copy overhead is acceptable because:
1. Most strings are small
2. Linear types will optimize this later
3. `inout` parameters avoid copies for mutations
4. Explicit `move` (future) for intentional transfer

## Alternatives Considered

### Reference Counting

```rue
String = (ptr: *StringHeader, ...)
StringHeader = { refcount: u64, len: u64, cap: u64, data: [u8] }
```

Rejected because:
- Runtime overhead on every copy/drop
- Complicates mutation (need COW or exclusive access check)
- Adds complexity before we know we need it

### Separate `StringBuf` Type

Keep `String` as immutable reference, add `StringBuf` for mutable:

```rue
let s: String = "hello";     // Immutable, rodata
let b: StringBuf = s.to_owned();  // Mutable, heap
```

Rejected because:
- Two types adds cognitive overhead
- Conversion between them is annoying
- Doesn't match mutable value semantics philosophy

### Garbage Collection

Let a GC handle string memory.

Rejected because:
- Rue aims to be a systems language
- GC adds unpredictable latency
- Conflicts with "no runtime" goal

## Test Plan

Tests should cover:

1. **Literal strings** (no heap allocation)
2. **Copy semantics** (modifications are independent)
3. **Promotion** (mutation causes heap allocation)
4. **Scope cleanup** (no leaks)
5. **Concatenation**
6. **Empty strings**
7. **Large strings** (capacity growth)
8. **Nested scopes** (correct drop ordering)
9. **Early returns** (drops still happen)
10. **Function parameters** (copy vs inout)

## Design Decisions

1. **Growth factor**: Use 1.5x (matches Rust's `Vec` strategy). Balances memory efficiency with amortized O(1) append.

2. **Small string optimization**: Not implementing SSO initially. Keep the design simple; optimize later if profiling shows it's needed.

3. **UTF-8 validation**: Defer to future work. Strings are byte sequences for now. A future ADR will address UTF-8 guarantees and `char` operations.

4. **String interning**: Not implementing. Literals are already deduplicated at compile time. Heap string interning is a future optimization if memory pressure becomes an issue.

## Related ADRs

- ADR-014: Ownership Modes
- ADR-015: Mutable Value Semantics
- Future: Linear Types ADR (will enable copy elision)

## References

- [Swift String Implementation](https://github.com/apple/swift/blob/main/stdlib/public/core/String.swift)
- [Rust String](https://doc.rust-lang.org/std/string/struct.String.html)
- [Val/Hylo Strings](https://github.com/hylo-lang/hylo)
