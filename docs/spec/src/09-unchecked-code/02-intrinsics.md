+++
title = "Unchecked Intrinsics"
weight = 2
template = "spec/page.html"
+++

# Unchecked Intrinsics

This section describes intrinsics that require a checked block.

## Syscall Intrinsic

{{ rule(id="9.2:1", cat="normative") }}

The `@syscall` intrinsic performs a direct system call to the operating system.

{{ rule(id="9.2:2", cat="syntax") }}

```ebnf
syscall_intrinsic = "@syscall" "(" syscall_number { "," argument } ")" ;
syscall_number = expression ;
argument = expression ;
```

{{ rule(id="9.2:3", cat="legality-rule") }}

The `@syscall` intrinsic takes at least one argument (the syscall number) and at most seven arguments (syscall number plus six syscall arguments). All arguments must be of type `u64`.

{{ rule(id="9.2:4", cat="dynamic-semantics") }}

The `@syscall` intrinsic returns an `i64` value representing the result of the syscall. On Linux x86-64, negative values typically indicate errors. The exact behavior depends on the syscall being invoked and the platform.

{{ rule(id="9.2:5", cat="informative") }}

Syscall numbers and conventions differ between operating systems. Linux x86-64 syscall numbers are different from macOS aarch64 syscall numbers. Users should consult platform-specific documentation.

{{ rule(id="9.2:6", cat="example") }}

```rue
fn main() -> i32 {
    let msg: [u8; 3] = [72, 73, 10]; // "HI\n"
    let write_num: u64 = 1;          // Linux x86-64: write
    let fd: u64 = 1;                 // stdout
    let exit_num: u64 = 231;         // Linux x86-64: exit_group
    let code: u64 = 0;
    checked {
        // write(fd=1, buf, len)
        let msg_ptr: u64 = @ptr_to_int(@raw(msg[0]));
        let msg_len: u64 = 3;
        let result = @syscall(write_num, fd, msg_ptr, msg_len);

        // exit_group(code)
        @syscall(exit_num, code);
    };
    0
}
```

## Pointer Arithmetic Intrinsic

{{ rule(id="9.2:7", cat="dynamic-semantics") }}

The `@ptr_offset(p, n)` intrinsic performs **standard pointer arithmetic**: it
returns the pointer whose address is `address_of(p) + n * size_of(T)`, where `T`
is the pointee type of `p`. A positive `n` moves toward **higher** addresses and
a negative `n` toward lower addresses. This is a plain addition of the scaled
offset and is **uniform for every pointer origin** — a pointer into a local
array, a heap allocation, a memory-mapped region, or an address produced by
`@int_to_ptr` all advance identically. Because array elements are laid out
ascending (§3.5), advancing a pointer to element `i` by `1` yields a pointer to
element `i+1`, so `@ptr_offset` and array indexing agree. Offsetting outside the
bounds of the pointed-to allocation is undefined behavior (see ADR-0028).

{{ rule(id="9.2:8", cat="example") }}

```rue
fn main() -> i32 {
    let arr: [i32; 3] = [10, 20, 30];
    checked {
        let base: ptr const i32 = @raw(arr[0]);
        // base points at element 0; +1 advances to element 1 (value 20).
        @ptr_read(@ptr_offset(base, 1))
    }
}
```

## Heap Allocation Intrinsics

{{ rule(id="9.2:9", cat="normative") }}

The `@alloc`, `@free`, and `@realloc` intrinsics provide raw, unchecked access
to the heap. Like the raw-pointer intrinsics they may only appear inside a
`checked` block (§9.1). They are the primitives on which safe, owned
collections (for example a source-level `Vec`) are built: the unsafety is
confined to the collection's internals behind a checked API.

{{ rule(id="9.2:10", cat="dynamic-semantics") }}

`@alloc(count)` allocates a heap block large enough to hold `count` elements of
type `T` — that is, `count * size_of(T)` bytes of **uninitialized** storage —
and returns a `ptr mut T` addressing the block. The element type `T` (and hence
the result type `ptr mut T`) is inferred from the surrounding context, exactly
as for `@int_to_ptr`. `count` must have type `u64`. On allocation failure the
returned pointer is null; the caller is responsible for checking. The returned
pointer is suitable for `@ptr_offset`/`@ptr_read`/`@ptr_write` over the
`count` elements (element `i` at `@ptr_offset(p, i)`).

{{ rule(id="9.2:11", cat="dynamic-semantics") }}

`@free(p, count)` releases a block previously returned by `@alloc`/`@realloc`,
where `p` is a `ptr mut T` and `count` (a `u64`) is the element count that was
allocated. Using `p` after it is freed is undefined behavior. Freeing a null
pointer is permitted and has no effect.

{{ rule(id="9.2:12", cat="dynamic-semantics") }}

`@realloc(p, old_count, new_count)` resizes the block addressed by `p` (a
`ptr mut T`) from `old_count` to `new_count` elements and returns a `ptr mut T`
to the resized block, which may differ from `p`. The first
`min(old_count, new_count)` elements are preserved. If `p` is null it behaves
like `@alloc(new_count)`. `old_count` and `new_count` must have type `u64`. The
result type is the same pointer type as `p`.

{{ rule(id="9.2:13", cat="legality-rule") }}

`@alloc`, `@free`, and `@realloc` may only be used inside a `checked` block. The
element-count arguments (`count`, `old_count`, `new_count`) must be `u64`, and
the pointer arguments of `@free`/`@realloc` must be a mutable pointer
`ptr mut T`. The result type of `@alloc` must be resolvable to a `ptr mut T`
from context.

{{ rule(id="9.2:14", cat="example") }}

```rue
fn main() -> i32 {
    checked {
        // Allocate room for 4 i32s, write three, read one back, then free.
        let p: ptr mut i32 = @alloc(4);
        @ptr_write(p, 10);
        @ptr_write(@ptr_offset(p, 1), 20);
        @ptr_write(@ptr_offset(p, 2), 30);
        let grown: ptr mut i32 = @realloc(p, 4, 8); // contents preserved
        let v: i32 = @ptr_read(@ptr_offset(grown, 1)); // 20
        @free(grown, 8);
        v
    }
}
```
