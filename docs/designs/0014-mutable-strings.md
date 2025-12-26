---
id: 0014
title: Mutable Strings
status: proposal
tags: [types, memory, strings]
feature-flag: mutable-strings
created: 2025-12-25
accepted:
implemented:
spec-sections: ["3.10"]
superseded-by:
---

# ADR-0014: Mutable Strings

## Status

Proposal

## Summary

Add a mutable `String` type that owns a heap-allocated byte buffer. String literals create immutable views into read-only data; mutation methods promote data to the heap. The type is affine (move semantics), uses a destructor for cleanup, and enables common string operations like appending and building strings dynamically.

## Context

### Current String Situation

Rue currently has string literals but they're severely limited:

```rue
let s = "hello";      // Type: String (but immutable, points to rodata)
@dbg_str(s);          // Can print
if s == "hello" { }   // Can compare
// That's it - no appending, no building, no mutation
```

The current `String` type is a fat pointer (ptr + len) pointing into read-only memory. There's no way to build strings dynamically or modify them.

### What We Need

For a practical programming language, we need:

1. **String building**: Construct strings from parts
2. **Appending**: `s.push_str("more")`
3. **Dynamic content**: Build from user input or computation

### Design Philosophy: Mutable Value Semantics

Following ADR-0008 (Affine Types and MVS), strings should be values:

- **No aliasing**: When you have a String, you own it exclusively
- **Affine**: Used at most once, then moved or dropped
- **Mutation via ownership**: You can mutate what you own
- **Explicit duplication**: Use `.clone()` if you need a copy

This differs from languages with garbage collection where strings are often immutable and cheap to share (Java, Python) or reference-counted (Swift). In Rue, a String is like Rust's `String`: an owned, mutable buffer.

### Byte String Semantics (like bstr/Go)

Rue strings are **conventionally UTF-8** rather than strictly validated:

- String literals are valid UTF-8 (validated at compile time)
- At runtime, strings are byte sequences
- Methods like `push_str` accept any bytes
- Display/debug may show invalid UTF-8 as replacement characters or escapes
- No runtime UTF-8 validation overhead

This matches Go's `string` and Rust's `bstr` crate: UTF-8 is the convention, but the type doesn't enforce it at runtime.

### Dependencies

This feature builds on infrastructure that is already implemented:

- **ADR-0008 (Affine Types)**: Strings are affine, move on use ✓
- **ADR-0009 (Struct Methods)**: Methods on String ✓
- **ADR-0010 (Destructors)**: String destructor frees the buffer ✓
- **ADR-0011 (Runtime Heap)**: `__rue_alloc`/`__rue_free` for buffer management ✓
- **ADR-0013 (Borrowing Modes)**: `borrow self` for query methods ✓

## Decision

### String Representation

```rue
// Conceptually, though users don't see the internals:
struct String {
    ptr: *mut u8,    // Pointer to data (heap or rodata)
    len: u64,        // Current length in bytes
    cap: u64,        // Allocated capacity (0 for rodata strings)
}
```

**Key insight**: `cap == 0` indicates a string literal (points to rodata). Any mutation requires heap promotion first.

Size: 24 bytes (3 × 8-byte fields).

### Literal vs Heap Strings

There is only one `String` type. Literals simply have `cap=0`:

```rue
let s = "hello";           // cap=0, ptr points to rodata, len=5
var t = "world";           // cap=0, ptr points to rodata, len=5
t.push_str("!");           // push_str promotes to heap, then appends
// t is now "world!" with cap>0
```

String literals are cheap (no allocation). Mutation methods check `cap` and promote to heap automatically:
1. Allocate a heap buffer with room to grow
2. Copy the existing content (from rodata or old heap)
3. Update ptr/cap
4. Perform the mutation

This is transparent to the user - there's no "owned" vs "borrowed" distinction.

### Type System

`String` is:
- **Not `@copy`**: Copying a String would require allocating and copying the buffer
- **Affine**: Consumed on use, cannot be used twice without explicit clone
- **Has destructor**: Frees heap buffer when dropped (if cap > 0)

```rue
fn takes_string(s: String) { ... }

fn main() -> i32 {
    var s = "hello";
    takes_string(s);    // s is moved
    // takes_string(s); // ERROR: use of moved value
    0
}
```

### Core Operations

All operations are methods via `impl String`:

#### Construction

```rue
impl String {
    // Empty string (no allocation until first push)
    fn new() -> String { ... }

    // Pre-allocate capacity
    fn with_capacity(cap: u64) -> String { ... }
}

// Usage:
let s = String::new();
let s = String::with_capacity(1024);
```

#### Query Methods

```rue
impl String {
    // Length in bytes
    fn len(borrow self) -> u64 { ... }

    // Allocated capacity
    fn capacity(borrow self) -> u64 { ... }

    // Is empty?
    fn is_empty(borrow self) -> bool { ... }
}
```

These borrow `self` (read-only access) so the string remains valid after the call.

#### Mutation Methods

```rue
impl String {
    // Append bytes from another string
    fn push_str(inout self, other: String) { ... }

    // Append a single byte
    fn push(inout self, byte: u8) { ... }

    // Clear contents (keep capacity)
    fn clear(inout self) { ... }

    // Ensure at least `additional` more bytes can be appended
    fn reserve(inout self, additional: u64) { ... }
}

// Usage:
var s = String::new();
s.push_str("hello");
s.push_str(" world");
s.push(33);  // '!'
// s is now "hello world!"
```

#### Clone

```rue
impl String {
    // Deep copy
    fn clone(borrow self) -> String { ... }
}

// Usage:
let a = "hello";
let b = a.clone();  // Deep copy (allocates new heap buffer)
// Both a and b are valid, independent strings
```

Clone borrows `self` so the original remains valid. It's explicit (not implicit like `@copy`) because it allocates.

### Destructor

When a String goes out of scope:

```rue
fn example() {
    var s = "hello";
    s.push_str("!");  // Promotes to heap
}  // __rue_drop_String called: frees heap buffer
```

Destructor logic:
```rust
fn __rue_drop_String(ptr: *mut u8, len: u64, cap: u64) {
    if cap > 0 {
        __rue_free(ptr, cap, 1);  // Only free heap strings
    }
    // rodata strings (cap == 0) are not freed
}
```

### Growth Strategy

When appending exceeds capacity:
1. Allocate new buffer (2x current capacity, minimum 16 bytes)
2. Copy existing content
3. Free old buffer (if heap)
4. Update ptr/cap

This amortizes allocation cost over many appends.

### Comparison Semantics

String comparison (`==`, `!=`) uses an optimized algorithm:

1. **Pointer equality fast path**: If both strings have the same `ptr` and `len`, they're equal (same memory)
2. **Length check**: If lengths differ, strings are not equal
3. **Byte-by-byte comparison**: Otherwise, compare contents

This optimization is significant for literal comparisons:

```rue
let s = "hello";
if s == "hello" {  // Same rodata pointer - fast path!
    // ...
}
```

The runtime function `__rue_str_eq` implements this logic.

### Rodata to Heap Promotion

When a mutation method is called on a rodata string (cap == 0, but ptr is non-null):

```rue
let s = "hello";     // rodata: cap=0, len=5
var t = s;           // Still rodata (no copy needed for move)
t.push_str("!");     // Promotes to heap: allocate, copy, then append
```

The promotion happens transparently inside mutation methods.

## Implementation Phases

Epic: rue-0hef

All functionality is gated behind `--preview mutable_strings`. Spec and tests come first.

### Phase 1: Specification and Feature Gate - rue-0hef.1 (COMPLETE)

Write the specification first:

- [x] Add `MutableStrings` to `PreviewFeature` enum in `rue-error`
- [x] Add spec section 3.10 for mutable strings
- [x] Define String representation (ptr, len, cap)
- [x] Define all method signatures and semantics
- [x] Add spec tests with `preview = "mutable_strings"` (these will fail initially)

**Testable**: Spec tests exist and are ignored (preview feature not yet implemented).

### Phase 2: Three-Field String Representation - rue-0hef.2 (COMPLETE)

Extend String from 2 fields (ptr, len) to 3 fields (ptr, len, cap):

- [x] Update AIR String type to carry capacity
- [x] Update codegen for 24-byte String type
- [x] Literals have cap=0
- [x] Gate new behavior behind preview feature

**Testable**: Existing string tests still pass with new representation.

### Phase 3: Runtime String Functions - rue-0hef.3 (COMPLETE)

Add string-specific functions to runtime:

- [x] `__rue_string_alloc(cap: u64) -> *mut u8` - allocate buffer (min 16 bytes)
- [x] `__rue_string_realloc(ptr: *mut u8, old_cap: u64, new_cap: u64) -> *mut u8` - grow buffer
- [x] `__rue_string_clone(ptr: *const u8, len: u64) -> *mut u8` - deep copy
- [x] `__rue_drop_String(ptr: *mut u8, len: u64, cap: u64)` - free if heap (cap > 0)
- [x] Growth strategy implementation (2x, min 16)

**Testable**: Unit tests in Rust for allocation/reallocation.

### Phase 4: String Destructor Integration

Wire String type to use destructors:

- Mark String as `needs_drop` in type system
- Drop elaboration inserts Drop instructions for String
- Codegen calls `__rue_drop_String`

**Testable**: Valgrind-clean string allocation and deallocation.

### Phase 5: Construction Methods

Add `impl String` with construction (gated):

- `String::new() -> String` - empty string
- `String::with_capacity(cap: u64) -> String` - pre-allocated

**Testable**: Create strings, verify cap is set correctly.

### Phase 6: Query Methods

Add query methods (gated):

- `fn len(borrow self) -> u64`
- `fn capacity(borrow self) -> u64`
- `fn is_empty(borrow self) -> bool`

**Testable**: Query methods return correct values.

### Phase 7: Mutation Methods

Add mutation methods with `inout self` (gated):

- `fn push_str(inout self, other: String)`
- `fn push(inout self, byte: u8)`
- `fn clear(inout self)`
- `fn reserve(inout self, additional: u64)`
- Automatic rodata-to-heap promotion
- Automatic grow-on-append

**Testable**: Build strings through multiple appends.

### Phase 8: Clone Method

Add explicit cloning (gated):

- `fn clone(borrow self) -> String`
- Deep copy via `__rue_string_clone`

**Testable**: Clone a string, verify both are independent.

### Phase 9: Equality Optimization

Optimize string comparison:

- If both strings have same ptr and len, return true immediately (pointer equality)
- Otherwise fall back to byte-by-byte comparison
- Update `__rue_str_eq` runtime function

**Testable**: Comparing a literal to itself uses fast path.

## Consequences

### Positive

- **Dynamic strings**: Can finally build and manipulate strings at runtime
- **Memory safe**: Destructor ensures no leaks
- **Predictable**: Affine semantics mean clear ownership
- **Efficient literals**: String literals remain cheap (no allocation)
- **No validation overhead**: Byte string semantics avoid runtime UTF-8 checks
- **Go/bstr compatible**: Familiar model for Go developers

### Negative

- **Allocation cost**: Mutations may allocate
- **No implicit sharing**: Can't cheaply pass string to multiple functions
- **Clone required**: Must explicitly copy if keeping original
- **Larger type**: 24 bytes vs 16 bytes for the fat pointer

### Neutral

- **Conventionally UTF-8**: Not enforced, but expected
- **No indexing**: Can't access individual bytes/chars (for now)

## Open Questions

None remaining.

## Future Work

- **String slices**: Non-owning views into strings (requires lifetime-lite or separate type)
- **Formatting**: `format!()` macro or similar
- **String interpolation**: `"hello \(name)"` syntax
- **Byte access**: Indexing into string bytes
- **UTF-8 iteration**: Iterator over codepoints when needed
- **Pattern matching**: `match s { "foo" => ..., _ => ... }`

## References

- [ADR-0008: Affine Types and MVS](0008-affine-types-mvs.md) - Ownership model
- [ADR-0009: Struct Methods](0009-struct-methods.md) - Method syntax
- [ADR-0010: Destructors](0010-destructors.md) - Cleanup mechanism
- [ADR-0011: Runtime Heap](0011-runtime-heap.md) - Allocation support
- [ADR-0013: Borrowing Modes](0013-borrowing-modes.md) - `borrow self` for query methods
- [Rust String](https://doc.rust-lang.org/std/string/struct.String.html) - Similar design
- [bstr crate](https://docs.rs/bstr) - Byte string semantics
- [Go strings](https://go.dev/blog/strings) - Conventionally UTF-8 approach
