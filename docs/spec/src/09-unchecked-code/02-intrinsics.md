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

The `@syscall` intrinsic returns an `i64` value representing the result of the
syscall. On every supported platform, operating-system syscall errors are
returned as negative error numbers. Successful return values and the meaning of
each error number depend on the syscall and platform.

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
collections (for example a source-level `ArrayBuf`) are built: the unsafety is
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
result type is the same pointer type as `p`. On allocation failure it returns
null and leaves the original block allocated, with its contents unchanged; the
caller remains responsible for freeing that original block.

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

## Raw Byte Intrinsics

{{ preview_feature(feature="raw_bytes", adr="RUE-879") }}

{{ rule(id="9.2:14a", cat="normative") }}

The `@alloc_bytes`, `@realloc_bytes`, `@free_bytes`, `@byte_read`,
`@byte_write`, `@byte_copy`, `@byte_set`, `@ptr_read_unaligned`, and
`@ptr_write_unaligned` intrinsics provide raw access to packed physical bytes
and to potentially unaligned typed scalars. They are enabled by the `raw_bytes`
preview feature and may only appear inside a `checked` block. They do not change
the element-scaled semantics of `@alloc`, `@realloc`, `@free`, `@ptr_offset`,
`@ptr_read`, or `@ptr_write`.

{{ rule(id="9.2:14b", cat="dynamic-semantics") }}

`@alloc_bytes(size, align)` allocates `size` physical bytes of uninitialized
storage aligned to `align` bytes and returns `ptr mut u8`. `@free_bytes(p, size,
align)` releases a block returned by `@alloc_bytes` or `@realloc_bytes`; the
`size` and `align` passed to `@free_bytes` must equal those used to allocate the
block, and freeing a null pointer is permitted. All `size` and `align` arguments
have type `u64`, and `align` must be a power of two (9.2:14j). Allocation failure
returns null and does not trap. `@alloc_bytes(0, align)` returns null.
`@free_bytes(null, 0, align)` is permitted and has no effect.

{{ rule(id="9.2:14c", cat="dynamic-semantics") }}

`@realloc_bytes(p, old_size, align, new_size)` resizes a raw-byte block. The
first `min(old_size, new_size)` physical bytes are preserved. The `align` must
equal the alignment used to allocate the block, and behaves as an allocation
with that alignment; a null `p` behaves like `@alloc_bytes(new_size, align)`. On
failure it returns null and leaves the original block allocated and unchanged.

If `new_size` is zero, `@realloc_bytes(p, old_size, align, 0)` frees a non-null
`p` and returns null. If `p` is null, reallocation behaves like
`@alloc_bytes(new_size, align)`, including returning null when `new_size` is
zero.

{{ rule(id="9.2:14d", cat="dynamic-semantics") }}

`@byte_read(p, offset)` reads and returns exactly one physical byte at address
`address_of(p) + offset`; `p` may be `ptr const u8` or `ptr mut u8` and `offset`
has type `u64`. `@byte_write(p, offset, value)` writes exactly the low eight
bits represented by its `u8` value at that address; `p` must be `ptr mut u8`.
Offsets are not multiplied by Rue's typed-pointer slot size.

{{ rule(id="9.2:14e", cat="legality-rule") }}

Every raw-byte intrinsic requires both `--preview raw_bytes` and an enclosing
`checked` block. Allocation and write pointers must be `ptr mut u8`; a byte
read also accepts `ptr const u8`. Size, offset, and alignment operands are
exactly `u64`, and a byte-write value is exactly `u8`. `@alloc_bytes` takes
`(size, align)`, `@realloc_bytes` takes `(p, old_size, align, new_size)`, and
`@free_bytes` takes `(p, size, align)`.

{{ rule(id="9.2:14f", cat="example") }}

```rue
fn main() -> i32 {
    checked {
        let p = @alloc_bytes(2, 1);
        @byte_write(p, 0, 65);
        @byte_write(p, 1, 66);
        let result: i32 = @intCast(@byte_read(p, 1));
        @free_bytes(p, 2, 1);
        result // 66
    }
}
```

{{ rule(id="9.2:14g", cat="dynamic-semantics") }}

`@byte_copy(dst, src, size)` copies exactly `size` physical bytes from the
region beginning at `src` to the region beginning at `dst`, in the manner of a
memcpy. The two regions must not overlap; a call in which `[dst, dst + size)`
and `[src, src + size)` overlap is undefined behavior (§9.1, ADR-0028). `dst`
must be `ptr mut u8`; `src` may be `ptr const u8` or `ptr mut u8`. `@byte_set(dst,
byte, size)` writes the `u8` value `byte` to each of the `size` physical bytes
beginning at `dst`, in the manner of a memset; `dst` must be `ptr mut u8`. In
both intrinsics `size` has type `u64`, byte counts are not multiplied by Rue's
typed-pointer slot size, and a `size` of zero performs no access and reads and
writes no memory.

{{ rule(id="9.2:14h", cat="legality-rule") }}

`@byte_copy` and `@byte_set` each require both `--preview raw_bytes` and an
enclosing `checked` block. Their destination operand is exactly `ptr mut u8`;
the `@byte_copy` source operand is `ptr const u8` or `ptr mut u8`. The
`@byte_set` fill operand is exactly `u8`, and every `size` operand is exactly
`u64`. Both intrinsics evaluate to `()`.

{{ rule(id="9.2:14i", cat="example") }}

```rue
fn main() -> i32 {
    checked {
        let src = @alloc_bytes(3, 1);
        @byte_set(src, 7, 3);
        let dst = @alloc_bytes(3, 1);
        @byte_copy(dst, src, 3);
        let result: i32 = @intCast(@byte_read(dst, 0))
            + @intCast(@byte_read(dst, 1))
            + @intCast(@byte_read(dst, 2));
        @free_bytes(src, 3, 1);
        @free_bytes(dst, 3, 1);
        result // 21
    }
}
```

{{ rule(id="9.2:14j", cat="legality-rule") }}

The `align` argument to `@alloc_bytes`, `@realloc_bytes`, and `@free_bytes` is a
byte count that must be a power of two. When `align` is a compile-time constant
that is zero or not a power of two, the program is rejected at compile time.
A non-constant `align` is not checked at compile time; supplying a value that is
zero or not a power of two is then undefined behavior (§9.1, ADR-0028), like the
other unchecked contracts of this family.

{{ rule(id="9.2:14k", cat="dynamic-semantics") }}

`@ptr_read_unaligned(p)` and `@ptr_write_unaligned(p, value)` are the unaligned
counterparts of `@ptr_read`/`@ptr_write` (ADR-0059). Their pointee type,
operands, result, and value semantics are exactly those of the aligned pair —
`p` is `ptr const T` or `ptr mut T` for the read and `ptr mut T` for the write,
the read returns `T` and the write returns `()` — except that the caller does
not promise the address satisfies `@align_of(T)`. Reading or writing an
underaligned address is well defined for these two intrinsics and undefined
behavior (§9.1, ADR-0028) for the aligned pair. They exist to access packed and
parsed data (integers embedded in a byte buffer) without a per-byte assembly.

{{ rule(id="9.2:14l", cat="legality-rule") }}

`@ptr_read_unaligned` and `@ptr_write_unaligned` each require both
`--preview raw_bytes` and an enclosing `checked` block. Aside from the alignment
obligation they carry the same legality rules as `@ptr_read`/`@ptr_write`: the
write pointer is `ptr mut T`, the read pointer is `ptr const T` or `ptr mut T`,
and a written value has type `T`.

{{ rule(id="9.2:14m", cat="example") }}

```rue
fn main() -> i32 {
    checked {
        let p: ptr mut i32 = @alloc(1);
        @ptr_write_unaligned(p, 1234);
        let v: i32 = @ptr_read_unaligned(p);
        @free(p, 1);
        v // 1234
    }
}
```

## Field Pointer Intrinsic

{{ rule(id="9.2:15", cat="normative") }}

The `@field_ptr` intrinsic forms a raw pointer to a struct field place without
taking a reference, the analog of Rust's `&raw mut (*p).field`. Like the other
raw-pointer intrinsics it may only appear inside a `checked` block (§9.1). It is
compiler-mediated field access: the pointer addresses the field at the offset
the compiler assigns, so unchecked code can walk a struct without hardcoding
slot offsets, remaining correct even if the struct layout is
implementation-defined.

{{ rule(id="9.2:16", cat="syntax") }}

```ebnf
field_ptr_intrinsic = "@field_ptr" "(" field_access ")" ;
field_access = expression "." IDENT ;
```

{{ rule(id="9.2:17", cat="legality-rule") }}

The `@field_ptr` intrinsic takes exactly one argument, which **MUST** be a
field-access expression `s.field`. Applying it to any other expression (a bare
variable, an array element, a call result, or a literal) is a compile-time
error; use `@raw`/`@raw_mut` to address those places.

{{ rule(id="9.2:18", cat="dynamic-semantics") }}

`@field_ptr(s.field)` returns a `ptr mut F`, where `F` is the type of `field`,
addressing the storage of that field within `s`. The result addresses the same
location as `@raw_mut` applied to the field, and reading it with `@ptr_read`
observes the same value as the ordinary field access `s.field`. Because the
returned pointer is mutable, `@ptr_write` through it updates the field in place.

{{ rule(id="9.2:19", cat="example") }}

```rue
struct Mixed { a: i32, b: i64, c: bool }

fn main() -> i32 {
    let m = Mixed { a: 11, b: 22, c: true };
    let got: i64 = checked {
        let p: ptr mut i64 = @field_ptr(m.b);
        @ptr_read(p)                              // 22, same as m.b
    };
    @intCast(got)
}
```
