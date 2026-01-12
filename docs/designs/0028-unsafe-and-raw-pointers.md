---
id: 0028
title: Unchecked Code and Raw Pointers
status: implemented
tags: [types, semantics, stdlib, ffi]
feature-flag: unchecked
created: 2026-01-03
accepted: 2026-01-03
implemented: 2026-01-04
spec-sections: ["9.1", "9.2"]
superseded-by:
---

# ADR-0028: Unchecked Code and Raw Pointers

## Status

Implemented and stabilized (2026-01-11)

## Summary

Add `checked` blocks, `unchecked` functions, and raw pointer types (`ptr const T`, `ptr mut T`) to Rue, enabling low-level operations necessary for implementing the standard library in Rue itself.

## Context

### The Standard Library Problem

Rue aims to provide a useful standard library with:
- Collections (Vec, HashMap)
- I/O (File, stdin, stdout)
- Process control (exit, args)
- String operations (already partially implemented)

Currently, all low-level operations are implemented in `rue-runtime` (Rust code) and exposed via intrinsics like `@dbg`, `@read_line`, `@parse_i32`, etc. This works but has limitations:

1. **Every OS capability needs a new intrinsic**: Adding file I/O means adding `@open`, `@read`, `@write`, `@close`, etc.
2. **Cannot self-host the stdlib**: The standard library will always depend on Rust code
3. **Limits user extensibility**: Users can't write their own low-level code
4. **Collection implementations**: Vec needs heap allocation and pointer arithmetic

### What We Need

To implement Tier 1 stdlib functionality (Vec, File I/O, process control), we need:
1. **Raw pointers**: Read/write arbitrary memory locations
2. **Syscalls**: Interface with the operating system
3. **Containment**: Keep unchecked operations visibly separate from checked code

### Design Goals

1. **Minimal surface area**: Only add what's necessary, not the kitchen sink
2. **Explicit and visible**: Unchecked code should be obvious at both definition and use sites
3. **No implicit coercions**: Converting between pointers and owned/borrowed values should be explicit
4. **Composable**: Should work with existing type system features (comptime generics, borrow modes)

### What Other Languages Do

**Rust**: `unsafe` blocks unlock raw pointers (`*const T`, `*mut T`), dereferencing, FFI calls, mutable statics.

**Zig**: No `unsafe` keyword. Pointers exist alongside slices. Uses `@intToPtr` and `@ptrToInt` for conversions. Trusts the programmer.

**C**: Everything is unchecked by default. Pointers are first-class.

**Swift**: `UnsafePointer<T>`, `UnsafeMutablePointer<T>`. Separate types, no keyword gating.

### Why Not Just Add More Intrinsics?

We could continue adding intrinsics (`@heap_alloc`, `@heap_free`, `@ptr_read`, etc.). Arguments against:

1. **Combinatorial explosion**: Every operation times every type equals many intrinsics
2. **Doesn't compose**: Can't write generic pointer code
3. **Still not self-hosting**: Runtime remains in Rust
4. **Users can't extend**: Only compiler team can add capabilities

With raw pointers, users can implement their own allocators, data structures, and FFI bindings.

## Decision

### Checked Blocks

The `checked` keyword introduces a block where unchecked operations are permitted:

```rue
fn example() {
    // Normal code here

    checked {
        // Can use raw pointers and call unchecked functions
    }

    // Normal code resumes
}
```

The name `checked` (rather than `unsafe`) reflects that the programmer is taking responsibility for checking invariants that the compiler cannot verify.

### Unchecked Functions

Functions that perform low-level operations can be marked `unchecked`:

```rue
unchecked fn dangerous_operation(p: ptr mut i32) {
    // Body is NOT implicitly a checked block
    // Must still use checked { } for unchecked operations
    checked {
        @ptr_write(p, 42);
    }
}

fn caller() {
    checked {
        dangerous_operation(some_ptr);  // Must be in checked block
    }
}
```

### Raw Pointer Types

Introduce two raw pointer types following Rue's keyword-based syntax:

```rue
ptr const T   // Pointer to immutable T
ptr mut T     // Pointer to mutable T
```

Why `ptr const`/`ptr mut` instead of `*const`/`*mut`:
- Consistent with Rue's keyword-based approach (cf. `borrow`, `inout`)
- Avoids overloading `*` which is already the dereference operator
- More readable, especially in complex types: `ptr mut ptr const i32` vs `*mut *const i32`

### Pointer Operations

All pointer operations require a `checked` block:

```rue
checked {
    // Create pointer from integer address
    let p: ptr mut i32 = @int_to_ptr(0x1000);

    // Read through pointer
    let value: i32 = @ptr_read(p);

    // Write through pointer
    @ptr_write(p, 42);

    // Pointer arithmetic
    let next: ptr mut i32 = @ptr_offset(p, 1);  // Advances by sizeof(i32)

    // Convert pointer to integer
    let addr: u64 = @ptr_to_int(p);

    // Null pointer
    let null: ptr const i32 = @null_ptr();

    // Check for null
    let is_null: bool = @is_null(p);
}
```

### Pointer Intrinsics

| Intrinsic | Signature | Description |
|-----------|-----------|-------------|
| `@ptr_read(p)` | `(ptr const T) -> T` | Read value at pointer |
| `@ptr_write(p, v)` | `(ptr mut T, T) -> ()` | Write value at pointer |
| `@ptr_offset(p, n)` | `(ptr T, i64) -> ptr T` | Offset by n elements (not bytes) |
| `@ptr_to_int(p)` | `(ptr T) -> u64` | Convert pointer to integer |
| `@int_to_ptr(n)` | `(u64) -> ptr mut T` | Convert integer to pointer |
| `@null_ptr()` | `() -> ptr const T` | Create null pointer |
| `@is_null(p)` | `(ptr T) -> bool` | Check if pointer is null |
| `@ptr_copy(dst, src, n)` | `(ptr mut T, ptr const T, u64) -> ()` | Copy n elements |

### Raw Pointer Intrinsics

To get pointers from values, use `@raw` and `@raw_mut`:

```rue
// From a borrow - get const pointer
fn print_impl(borrow s: String) {
    checked {
        let p: ptr const String = @raw(s);
        // Now can pass to syscalls, etc.
    }
}

// From an inout - get mutable pointer
fn mutate_impl(inout s: String) {
    checked {
        let p: ptr mut String = @raw_mut(s);
    }
}

// From an owned value - can get either
fn consume_impl(s: String) {
    checked {
        let p: ptr const String = @raw(s);      // const pointer to owned
        let p: ptr mut String = @raw_mut(s);    // mutable pointer to owned
    }
}
```

| Intrinsic | Input | Output | Description |
|-----------|-------|--------|-------------|
| `@raw(x)` | `borrow T` | `ptr const T` | Const pointer from immutable borrow |
| `@raw(x)` | `T` (owned) | `ptr const T` | Const pointer from owned value |
| `@raw_mut(x)` | `inout T` | `ptr mut T` | Mutable pointer from mutable borrow |
| `@raw_mut(x)` | `T` (owned) | `ptr mut T` | Mutable pointer from owned value |

These intrinsics:
- Require a `checked` block
- Work on borrows, inouts, and owned values
- The pointer is only valid while the source value is valid
- Enable writing stdlib functions that don't consume their arguments

### Comparison to Rust

In Rust, creating a raw pointer is safe - only dereferencing is unsafe:

```rust
let p: *const i32 = &x as *const i32;  // Safe in Rust
unsafe { *p }                           // Unsafe in Rust
```

In Rue, we require `checked` for both creating and using pointers. This is more conservative but makes all pointer-related code visibly marked. The tradeoff:

| Operation | Rust | Rue |
|-----------|------|-----|
| Create pointer from value | Safe | Requires `checked` |
| Pointer arithmetic | Safe | Requires `checked` |
| Dereference pointer | Unsafe | Requires `checked` |

Rue's approach means any code touching pointers is auditable by searching for `checked` blocks.

### Syscall Intrinsic

For OS interaction, add a syscall intrinsic:

```rue
checked {
    // Linux write(fd, buf, len)
    let result = @syscall(1, fd, buf_ptr, len);

    // Linux exit_group(code)
    @syscall(231, code);
}
```

The `@syscall` intrinsic:
- Takes a syscall number and up to 6 arguments
- Returns `i64` (syscall return value)
- Arguments are passed as `u64` (pointers converted via `@ptr_to_int`)
- Platform-specific (Linux x86-64 syscall numbers differ from macOS)

A future ADR will address platform abstraction (`std.os.linux`, `std.os.macos`).

### Integration with Borrow Modes

Raw pointers integrate with Rue's borrow system via `@raw` and `@raw_mut`:

```rue
// Safe API - takes borrow, doesn't consume
fn print(borrow s: String) {
    checked {
        let p = @raw(s);
        let len = s.len();
        @syscall(1, 1, @ptr_to_int(p), len);  // write to stdout
    }
}

fn main() {
    let s = "hello";
    print(borrow s);  // s is borrowed, not consumed
    print(borrow s);  // can use s again!
}
```

Without `@raw`, pointers could only be obtained from owned values, forcing APIs to consume their arguments.

### Integration with Comptime Generics

Raw pointers work with comptime generics:

```rue
fn Vec(comptime T: type) -> type {
    struct {
        ptr: ptr mut T,
        len: u64,
        cap: u64,
    }
}

unchecked fn vec_push(comptime T: type, v: inout Vec(T), item: T) {
    checked {
        // Implementation using pointer operations
    }
}
```

### What This ADR Does NOT Include

Explicitly out of scope:

1. **Inline assembly**: Too complex, defer to later ADR
2. **Transmute/reinterpret_cast**: Use pointer casts instead
3. **Unchecked arithmetic**: Use explicit wrapping intrinsics if needed
4. **Union types**: Separate feature if needed
5. **Static mut**: Mutable globals are a separate concern
6. **Platform abstraction**: A follow-up ADR will add `std.os` wrappers

### Memory Safety Guarantees

Inside `checked` blocks, the programmer is responsible for:
- Not dereferencing null or dangling pointers
- Not creating aliasing violations (multiple `ptr mut` to same location)
- Not violating alignment requirements
- Ensuring pointed-to memory is valid for the type
- Ensuring pointers from `@raw`/`@raw_mut` don't outlive the borrow

The compiler does NOT verify these - that's what makes it unchecked.

Outside `checked` blocks, all of Rue's safety guarantees still hold.

## Implementation Phases

- [x] **Phase 1: Parser support** (rue-7qxm) - Add `checked` block syntax, `unchecked` function modifier, `ptr const`/`ptr mut` types
- [x] **Phase 2: Type system** (rue-pb4z) - Pointer types in sema, checked block tracking
- [x] **Phase 3: Pointer intrinsics** (rue-u9a4) - `@ptr_read`, `@ptr_write`, `@ptr_offset`, `@addr_of`, `@addr_of_mut`, `@ptr_to_int`, `@int_to_ptr`
- [x] **Phase 4: Syscall intrinsic** (rue-pwyw) - `@syscall` for direct OS calls
- [x] **Phase 5: Codegen** (rue-bk7s) - Generate correct code for pointer operations in both x86_64 and aarch64 backends
- [ ] **Phase 6: Stdlib foundation** (rue-i3ti) - Implement basic Vec and I/O using unchecked code (follow-on work)

## Consequences

### Positive

- **Self-hosting path**: Stdlib can be written in Rue
- **User extensibility**: Advanced users can write low-level code
- **Minimal intrinsics**: Don't need a new intrinsic for every OS operation
- **FFI foundation**: Groundwork for calling C libraries
- **Composable**: Works with generics for generic data structures
- **Ergonomic stdlib APIs**: `@raw`/`@raw_mut` enable non-consuming APIs like `print(borrow s)`

### Negative

- **Safety escape hatch**: Users can write memory-unsafe code
- **Complexity**: Another concept to learn (though can be ignored by most users)
- **Platform specifics**: Syscall numbers vary by OS
- **Review burden**: Unchecked code requires more careful review

### Neutral

- **Different from Rust**: `checked`/`unchecked` naming, no implicit checked body
- **Different from Zig**: Zig doesn't have an unchecked keyword, trusts programmer everywhere

## Resolved Questions

1. **Should pointers be nullable by default?** Yes, with `@is_null` check.

2. **Pointer-to-borrow conversion?** No. Rue doesn't have the lifetime tracking needed to make this safe.

3. **Platform abstraction for syscalls?** Will be addressed in a follow-up ADR with `std.os` wrappers.

4. **Should `unchecked fn` require `checked { }` at call site?** Yes.

5. **Sized types only?** Yes. Rue doesn't have unsized types yet.

## Future Work

- **FFI**: Calling C functions, `extern` declarations
- **Inline assembly**: For performance-critical code or hardware access
- **Platform abstraction layer**: `std.os.linux`, `std.os.macos` with typed syscall wrappers
- **Non-null pointers**: `ptr! mut T` that's guaranteed non-null
- **Volatile operations**: For memory-mapped I/O

## References

- [Rust Unsafe](https://doc.rust-lang.org/book/ch19-01-unsafe-rust.html) - Rust's approach
- [Rust RFC: unsafe_op_in_unsafe_fn](https://rust-lang.github.io/rfcs/2585-unsafe-block-in-unsafe-fn.html) - Fixing the implicit unsafe body mistake
- [Zig Pointers](https://ziglang.org/documentation/master/#Pointers) - Zig's pointer types
- [Swift UnsafePointer](https://developer.apple.com/documentation/swift/unsafepointer) - Swift's unsafe types
- [ADR-0008: Affine Types and MVS](0008-affine-types-mvs.md) - Rue's ownership model
- [ADR-0013: Borrowing Modes](0013-borrowing-modes.md) - Rue's borrow system
- [ADR-0025: Comptime](0025-comptime.md) - Generic type parameters
