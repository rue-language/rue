---
id: 0011
title: Runtime Heap
status: implemented
tags: [runtime, memory, allocator]
feature-flag: runtime-heap
created: 2025-12-25
accepted: 2025-12-25
implemented: 2026-01-04
spec-sections: []
superseded-by:
---

# ADR-0011: Runtime Heap

## Status

Implemented

## Summary

Add heap allocation support to the Rue runtime via a simple bump allocator backed by `mmap`/`munmap` syscalls. This provides the foundation for heap-allocated types like `String`, `Vec`, and `Box` without depending on libc. The implementation exposes `__rue_alloc`, `__rue_realloc`, and `__rue_free` functions that can be called from generated code.

## Context

### Why a Custom Allocator?

Rue compiles to standalone executables with no libc dependency. The runtime uses direct syscalls for all system interactions (exit, write). For heap allocation, we need to continue this pattern:

1. **No libc dependency**: Keep executables minimal and self-contained
2. **Control**: Understand exactly what's happening with memory
3. **Simplicity**: A bump allocator is trivial to implement and debug
4. **Foundation**: Enables String, Vec, Box, and other heap types

### What's Needed

The destructor ADR (0010) identifies the missing piece:

> "Open question: How do we hook into malloc/free? System allocator? Custom?"

This ADR answers: custom allocator using mmap/munmap directly.

### Design Constraints

1. **Platform support**: Must work on x86-64 Linux and AArch64 macOS
2. **No global state initialization**: The allocator must work without explicit init
3. **Thread safety**: Not required initially (Rue is single-threaded)
4. **Simplicity over performance**: This is V1; optimize later if needed

## Decision

### Allocator Design: Bump Allocator with Arenas

We use a **bump allocator** - the simplest possible design:

1. Request large chunks ("arenas") from the OS via `mmap`
2. Allocations bump a pointer forward within the current arena
3. When an arena fills, request a new one
4. Individual `free()` is a no-op; memory is only returned when all arenas are freed

This trades memory efficiency for simplicity. It's appropriate because:
- Rue programs are typically short-lived (compile, run, exit)
- Memory is reclaimed by the OS on exit anyway
- Future optimization can add a more sophisticated allocator

### API

Three functions exported from the runtime:

```rust
/// Allocate `size` bytes aligned to `align`.
/// Returns null on failure (OOM or invalid arguments).
#[no_mangle]
pub extern "C" fn __rue_alloc(size: u64, align: u64) -> *mut u8

/// Reallocate `ptr` from `old_size` to `new_size` bytes.
/// Returns null on failure. Old data is copied to new location.
/// If ptr is null, behaves like alloc. If new_size is 0, behaves like free.
#[no_mangle]
pub extern "C" fn __rue_realloc(ptr: *mut u8, old_size: u64, new_size: u64, align: u64) -> *mut u8

/// Free memory at `ptr`.
/// For the bump allocator, this is a no-op (memory reclaimed on exit).
#[no_mangle]
pub extern "C" fn __rue_free(ptr: *mut u8, size: u64, align: u64)
```

The API includes `size` and `align` parameters even for `free` to enable future allocator upgrades without API changes.

### Arena Management

```
+------------------+------------------+------------------+
|     Arena 1      |     Arena 2      |     Arena 3      |
|  [alloc][alloc]  | [alloc][...free] |    [unused]      |
+------------------+------------------+------------------+
                          ^
                          bump pointer
```

- Default arena size: 64 KiB (one large page, adjustable)
- Arenas are linked via a header at the start of each arena
- Large allocations (> arena size / 2) get their own dedicated arena

### Implementation Details

#### Global State

```rust
struct ArenaHeader {
    next: *mut ArenaHeader,  // linked list of arenas
    size: usize,             // arena size (excluding header)
    used: usize,             // bytes allocated in this arena
}

static mut ARENA_HEAD: *mut ArenaHeader = null_mut();
static mut CURRENT_ARENA: *mut ArenaHeader = null_mut();
```

Global state is initialized lazily on first allocation.

#### Alignment

Allocations are aligned by bumping the pointer to the next aligned address:

```rust
fn align_up(addr: usize, align: usize) -> usize {
    (addr + align - 1) & !(align - 1)
}
```

#### Platform Syscalls

**Linux (x86-64 and AArch64)**:
```rust
// mmap(NULL, size, PROT_READ|PROT_WRITE, MAP_PRIVATE|MAP_ANONYMOUS, -1, 0)
// syscall number: 9 (x86-64), 222 (aarch64)

// munmap(addr, size)
// syscall number: 11 (x86-64), 215 (aarch64)
```

**macOS (AArch64)**:
```rust
// mmap: syscall 197
// munmap: syscall 73
```

### Error Handling

- OOM: Return null pointer (caller must check)
- Invalid alignment (not power of 2): Return null pointer
- Zero-size allocation: Return null pointer

No panics in the allocator - it just returns null on failure. Higher-level code (String, Vec) can panic with a useful message.

### Testing Strategy

1. **Unit tests in runtime**: Test allocator directly with various sizes/alignments
2. **Integration tests**: Allocate from Rue code, verify memory is usable
3. **Valgrind/ASan**: Verify no memory corruption (on Linux)

## Implementation Phases

Epic: rue-n50n

### Phase 1: Syscall Wrappers (rue-n50n.1)

Add mmap/munmap wrappers to each platform module:
- `x86_64_linux.rs`: `mmap()`, `munmap()`
- `aarch64_linux.rs`: `mmap()`, `munmap()`
- `aarch64_macos.rs`: `mmap()`, `munmap()`

**Testable**: Call mmap, write to memory, munmap without crashing.

### Phase 2: Bump Allocator Core (rue-n50n.2)

Implement the allocator logic:
- Arena header structure
- `__rue_alloc` with alignment support
- `__rue_free` (no-op for now)
- Lazy initialization of first arena

**Testable**: Allocate various sizes, verify returned pointers are aligned.

### Phase 3: Realloc and Large Allocations (rue-n50n.3)

- `__rue_realloc` implementation
- Large allocation handling (dedicated arenas)
- Edge cases (null ptr, zero size)

**Testable**: Realloc growing and shrinking, large allocations.

### Phase 4: Integration with Compiler (rue-n50n.4)

- Add codegen support for calling `__rue_alloc`/`__rue_free`
- Wire up for future String/Vec types
- Document calling convention

**Testable**: Rue code can call allocation intrinsics.

## Consequences

### Positive

- **No libc dependency**: Maintains Rue's minimal runtime philosophy
- **Simplicity**: Bump allocator is ~100 lines of code
- **Cross-platform**: Same API on Linux and macOS
- **Foundation**: Enables all heap-allocated types

### Negative

- **Memory waste**: Bump allocator never frees until program exit
- **No thread safety**: Single-threaded only (acceptable for V1)
- **Large allocations**: Each gets a whole arena (wasteful)

### Neutral

- **API stability**: Include size/align in free for future compatibility
- **Performance**: Bump allocation is O(1), realloc is O(n) copy

## Open Questions

1. **Arena size**: 64 KiB reasonable? Should it grow dynamically?

2. **Thread safety**: When Rue adds threading, need mutex or thread-local arenas?

3. **Debug mode**: Should we poison freed memory in debug builds?

4. **Metrics**: Should we track total allocated bytes for debugging?

## Future Work

- **Freelist allocator**: Add actual freeing for long-running programs
- **Size classes**: Like jemalloc, for better memory reuse
- **Thread-local arenas**: For multithreaded programs
- **Custom allocators**: Let users provide their own allocator

## References

- [ADR-0010: Destructors](0010-destructors.md) - Consumer of heap allocation
- [ADR-0008: Affine Types](0008-affine-types-mvs.md) - Ownership model
- [mmap(2)](https://man7.org/linux/man-pages/man2/mmap.2.html) - Linux syscall
- [Bump Allocation](https://fitzgeraldnick.com/2019/11/01/always-bump-downwards.html) - Design reference
