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
    checked {
        // Linux x86-64: write(fd=1, buf, len)
        let result = @syscall(1_u64, 1_u64, msg_ptr, msg_len);

        // Linux x86-64: exit_group(code)
        @syscall(231_u64, 0_u64);
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
