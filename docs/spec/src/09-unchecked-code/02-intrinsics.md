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
    let write_num: u64 = 1;          // Linux x86-64: write
    let fd: u64 = 1;                 // stdout
    let exit_num: u64 = 231;         // Linux x86-64: exit_group
    let code: u64 = 0;
    checked {
        // A syscall buffer must be a contiguous byte image; allocate it on the
        // heap (@alloc storage is the compact image, 9.2:10) rather than an @raw
        // of a frame-resident byte array, whose storage is slot-shaped under the
        // current implementation (ADR-0052).
        let msg: ptr mut u8 = @alloc(3, 1); // "HI\n"
        @ptr_write(msg, 72);
        @ptr_write(@ptr_offset(msg, 1), 73);
        @ptr_write(@ptr_offset(msg, 2), 10);
        // write(fd=1, buf, len)
        let msg_ptr: u64 = @ptr_to_int(msg);
        let msg_len: u64 = 3;
        let result = @syscall(write_num, fd, msg_ptr, msg_len);
        @free(msg, 3, 1);

        // exit_group(code)
        @syscall(exit_num, code);
    };
    0
}
```

## Pointer Access Intrinsics

{{ rule(id="9.2:6a", cat="normative") }}

The `@raw`, `@raw_mut`, `@ptr_read`, `@ptr_write`, `@ptr_to_int`, and
`@int_to_ptr` intrinsics are the pointee-typed access surface over raw
pointers. Like the other intrinsics of this section they may only appear
inside a `checked` block (§9.1, 9.1:12). `@raw(place)` evaluates to a
`ptr const T` and `@raw_mut(place)` to a `ptr mut T` addressing that place's
storage, where `T` is the type of the place.

{{ rule(id="9.2:6b", cat="dynamic-semantics") }}

`@ptr_read(p)` reads the value stored at `address_of(p)` and
`@ptr_write(p, value)` stores one there, where the value's type `T` is the
pointee type of `p`. Each transfers exactly `@size_of(T)` **physical bytes** —
the width the implementation has chosen for `T` (§3.6, 3.6:8), never a
fixed-width machine slot — so at `T = u8` the access is exactly one byte wide
and at `T = i32` exactly four. A write therefore leaves every byte outside
`[address_of(p), address_of(p) + @size_of(T))` unchanged. The
address **MUST** satisfy `@align_of(T)`; using this pair on an underaligned
address is undefined behavior (§9.1, ADR-0028), for which `@ptr_read_unaligned`
and `@ptr_write_unaligned` (9.2:14k) are the well-defined form.

{{ rule(id="9.2:6c", cat="dynamic-semantics") }}

`@ptr_to_int(p)` evaluates to the address `p` holds as a `u64`, and
`@int_to_ptr(a)` evaluates to the pointer whose address is the `u64` `a`, its
pointee type taken from the context the result is used in. The two round-trip:
`@int_to_ptr(@ptr_to_int(p))` addresses the same storage as `p`, which is also
how a `ptr mut u8` block from the allocation family (9.2:10) is viewed as a
`ptr mut T`. They are the only conversions between an address and a pointer,
and are kept apart from access and arithmetic so the common path never
silently launders an address into a pointer (ADR-0059). A zero address is the
null pointer: `@int_to_ptr` applied to a `u64` operand of value zero produces
null, and `@ptr_to_int(p) == 0` is the null test.

{{ rule(id="9.2:6d", cat="legality-rule") }}

`@ptr_read` accepts a `ptr const T` or a `ptr mut T` and evaluates to `T`;
`@ptr_write` requires a `ptr mut T` and a value of exactly the pointee type
`T`, and evaluates to `()`. `@ptr_to_int` accepts a pointer of any pointee
type and mutability and evaluates to `u64`. `@int_to_ptr` requires an operand
of exactly `u64` — an untyped integer literal is not accepted, so the null
idiom is written over a `u64` binding — and evaluates to a `ptr mut T` whose
pointee type is inferred from context.

{{ rule(id="9.2:6e", cat="example") }}

```rue
fn main() -> i32 {
    let mut x: i32 = 10;
    let zero: u64 = 0;
    checked {
        let p: ptr mut i32 = @raw_mut(x);
        @ptr_write(p, 99);                        // writes @size_of(i32) == 4 bytes
        let back: ptr mut i32 = @int_to_ptr(@ptr_to_int(p));
        let null: ptr mut u8 = @int_to_ptr(zero); // the null-pointer idiom
        if @ptr_to_int(null) == 0 { @ptr_read(back) } else { 0 }
    }
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
element `i+1`, so `@ptr_offset` and array indexing agree. The scale is the
pointee's physical size, so at `T = u8` — where `@size_of(u8)` is `1` — the
stride is exactly one byte and `@ptr_offset` is itself the byte-granular walk;
Rue therefore has no separate byte-granular offset intrinsic (ADR-0059).
Offsetting outside the bounds of the pointed-to allocation is undefined
behavior (see ADR-0028).

{{ rule(id="9.2:8", cat="example") }}

```rue
fn main() -> i32 {
    // A slot-identical element type (i64: one eight-byte slot per element) so a
    // raw pointer into the frame-resident array is supported; a non-slot-identical
    // frame array (e.g. [i32; 3], which packs to four-byte strides while the frame
    // stores eight-byte slots) is refused by the current implementation (ADR-0052).
    let arr: [i64; 3] = [10, 20, 30];
    let v: i64 = checked {
        let base: ptr const i64 = @raw(arr[0]);
        // base points at element 0; +1 advances to element 1 (value 20).
        @ptr_read(@ptr_offset(base, 1))
    };
    @intCast(v)
}
```

## Heap Allocation Intrinsics

{{ rule(id="9.2:9", cat="normative") }}

The `@alloc`, `@alloc_zeroed`, `@free`, `@realloc`, and `@resize` intrinsics
provide raw, unchecked access to the heap. Like the raw-pointer intrinsics they
may only appear inside a `checked` block (§9.1). They form **one byte-oriented
allocation family**: every size is a count of physical bytes, every allocation
states an explicit alignment, and every pointer is `ptr mut u8`. Allocating
storage for values of a type `T` is ordinary source arithmetic over the
reflection intrinsics — `@alloc(count * @intCast(@size_of(T)), @intCast(@align_of(T)))`,
the `@intCast` being what carries `@size_of`/`@align_of`'s `i32` result
(4.13:14, 4.13:20) to the `u64` operands this family requires — so the
language has no separate element-counted allocator. They are the primitives on
which safe, owned collections (for example a source-level `ArrayBuf`) are built:
the unsafety is confined to the collection's internals behind a checked API.

{{ rule(id="9.2:10", cat="dynamic-semantics") }}

`@alloc(size, align)` allocates `size` physical bytes of **uninitialized**
storage aligned to `align` bytes and returns a `ptr mut u8` addressing the
block. `@alloc_zeroed(size, align)` is identical except that every byte of the
returned block reads as zero. On allocation failure both return null; the caller
is responsible for checking. `@alloc(0, align)` and `@alloc_zeroed(0, align)`
return null. The returned pointer is suitable for the byte intrinsics and, once
converted to a `ptr mut T` (9.2:6c) addressing an offset that satisfies
`@align_of(T)`, for `@ptr_offset`/`@ptr_read`/`@ptr_write` over the `T` values
the block can hold; `@ptr_read_unaligned`/`@ptr_write_unaligned` (9.2:14k) reach
a `T` at an offset that does not satisfy `@align_of(T)`.

{{ rule(id="9.2:11", cat="dynamic-semantics") }}

`@free(p, size, align)` releases a block previously returned by
`@alloc`/`@alloc_zeroed`/`@realloc`. The `size` and `align` must equal those the
block currently carries: the allocator keeps no per-block header, so the caller
returns the layout. Using `p` after it is freed is undefined behavior. Freeing a
null pointer is permitted and has no effect.

{{ rule(id="9.2:12", cat="dynamic-semantics") }}

`@realloc(p, old_size, align, new_size)` resizes the block addressed by `p` from
`old_size` to `new_size` physical bytes and returns a `ptr mut u8` to the
resized block, which may differ from `p`. The first
`min(old_size, new_size)` bytes are preserved and `align` must equal the
alignment the block was allocated with. If `p` is null it behaves like
`@alloc(new_size, align)`, including returning null when `new_size` is zero. If
`new_size` is zero and `p` is non-null, the block is freed and null is returned.
On allocation failure it returns null and leaves the original block allocated,
with its contents unchanged; the caller remains responsible for freeing that
original block.

{{ rule(id="9.2:12a", cat="dynamic-semantics") }}

`@resize(p, old_size, align, new_size)` is the in-place-only counterpart of
`@realloc`: the block **never moves**. It evaluates to `true` when the block at
`p` now describes `new_size` bytes at the same address — the caller must then
pass `new_size` to a later `@free`/`@realloc`/`@resize` — and to `false` when
the request was refused, in which case nothing changed and `old_size` still
describes the block. Bytes below `min(old_size, new_size)` are preserved on
success and no byte is written on refusal. Whether any particular request is
satisfied in place is unspecified; a conforming implementation may always
answer `false`, so a program's observable behavior must not depend on `true`.
`@resize` never copies, never frees, and never allocates, so it cannot fail with
a null result.

{{ rule(id="9.2:13", cat="legality-rule") }}

`@alloc`, `@alloc_zeroed`, `@free`, `@realloc`, and `@resize` may only be used
inside a `checked` block. Every size and alignment operand (`size`, `align`,
`old_size`, `new_size`) must be `u64`, and the pointer operand of
`@free`/`@realloc`/`@resize` must be `ptr mut u8`. `@alloc` and `@alloc_zeroed`
evaluate to `ptr mut u8`, `@realloc` to `ptr mut u8`, `@resize` to `bool`, and
`@free` to `()`.

{{ rule(id="9.2:13a", cat="legality-rule") }}

The `align` argument of every intrinsic in this family is a byte count that must
be a power of two. When `align` is a compile-time constant that is zero or not a
power of two, the program is rejected at compile time. A non-constant `align` is
not checked at compile time; supplying a value that is zero or not a power of
two is then undefined behavior (§9.1, ADR-0028), like the other unchecked
contracts of this family.

{{ rule(id="9.2:14", cat="example") }}

```rue
fn main() -> i32 {
    checked {
        // Room for 4 i32s, computed from the type's own layout.
        let unit: u64 = @intCast(@size_of(i32));
        let align: u64 = @intCast(@align_of(i32));
        let raw: ptr mut u8 = @alloc(4 * unit, align);
        let p: ptr mut i32 = @int_to_ptr(@ptr_to_int(raw));
        @ptr_write(p, 10);
        @ptr_write(@ptr_offset(p, 1), 20);
        @ptr_write(@ptr_offset(p, 2), 30);
        let grown_raw: ptr mut u8 = @realloc(raw, 4 * unit, align, 8 * unit);
        let grown: ptr mut i32 = @int_to_ptr(@ptr_to_int(grown_raw));
        let v: i32 = @ptr_read(@ptr_offset(grown, 1)); // 20
        @free(grown_raw, 8 * unit, align);
        v
    }
}
```

## Raw Byte Intrinsics

{{ rule(id="9.2:14a", cat="normative") }}

The `@byte_read`, `@byte_write`, `@byte_copy`, `@byte_move`, `@byte_set`,
`@ptr_read_unaligned`, and `@ptr_write_unaligned` intrinsics provide raw access
to packed physical bytes and to potentially unaligned typed scalars. They may
only appear inside a `checked` block. In the five byte-granular intrinsics every
pointer is a `u8` pointer and every count and offset is a number of physical
bytes, scaled by nothing: no operand of this family is multiplied by a pointee
width. The two `_unaligned` intrinsics are instead pointee-typed, like the
aligned pair of 9.2:6b — they take no count or offset and move exactly
`@size_of(T)` bytes — and differ from it only in dropping the alignment
obligation (9.2:14k).

{{ rule(id="9.2:14d", cat="dynamic-semantics") }}

`@byte_read(p, offset)` reads and returns exactly one physical byte at address
`address_of(p) + offset`; `p` may be `ptr const u8` or `ptr mut u8` and `offset`
has type `u64`. `@byte_write(p, offset, value)` writes exactly the low eight
bits represented by its `u8` value at that address; `p` must be `ptr mut u8`.
The `offset` is a plain byte displacement, added to the address unscaled. Since
`@size_of(u8)` is `1`, that is the same address `@ptr_offset(p, offset)` names
(9.2:7), so `@byte_read(p, offset)` and `@ptr_read(@ptr_offset(p, offset))`
access the same single byte, as do `@byte_write` and the corresponding
`@ptr_write`.

{{ rule(id="9.2:14e", cat="legality-rule") }}

`@byte_read` and `@byte_write` each require an enclosing `checked` block.
`@byte_write` requires a `ptr mut u8`; `@byte_read` accepts `ptr const u8` or
`ptr mut u8`. Their offset operand is exactly `u64` and a byte-write value is
exactly `u8`. `@byte_read` evaluates to `u8` and `@byte_write` to `()`. The
legality of the bulk intrinsics is 9.2:14h and that of the `_unaligned` pair is
9.2:14l.

{{ rule(id="9.2:14f", cat="example") }}

```rue
fn main() -> i32 {
    checked {
        let p = @alloc(2, 1);
        @byte_write(p, 0, 65);
        @byte_write(p, 1, 66);
        let result: i32 = @intCast(@byte_read(p, 1));
        @free(p, 2, 1);
        result // 66
    }
}
```

{{ rule(id="9.2:14g", cat="dynamic-semantics") }}

`@byte_copy(dst, src, size)` copies exactly `size` physical bytes from the
region beginning at `src` to the region beginning at `dst`, in the manner of a
memcpy. The two regions must not overlap; a call in which `[dst, dst + size)`
and `[src, src + size)` overlap is undefined behavior (§9.1, ADR-0028).
`@byte_move(dst, src, size)` copies the same bytes in the manner of a memmove:
the two regions **may** overlap, and the result is as if the `size` source bytes
were first read into a temporary region and then written to `dst`, so every
source byte is observed before any destination byte overwrites it. In both, `dst`
must be `ptr mut u8` and `src` may be `ptr const u8` or `ptr mut u8`.
`@byte_set(dst, byte, size)` writes the `u8` value `byte` to each of the `size`
physical bytes beginning at `dst`, in the manner of a memset; `dst` must be
`ptr mut u8`. In all three intrinsics `size` has type `u64`, byte counts are not
multiplied by a pointee width, and a `size` of zero performs no access and reads
and writes no memory.

{{ rule(id="9.2:14h", cat="legality-rule") }}

`@byte_copy`, `@byte_move`, and `@byte_set` each require an enclosing `checked`
block. Their destination operand is exactly `ptr mut u8`; the `@byte_copy` and
`@byte_move` source operand is `ptr const u8` or `ptr mut u8`. The `@byte_set`
fill operand is exactly `u8`, and every `size` operand is exactly `u64`. All
three intrinsics evaluate to `()`.

{{ rule(id="9.2:14i", cat="example") }}

```rue
fn main() -> i32 {
    checked {
        let src = @alloc(3, 1);
        @byte_set(src, 7, 3);
        let dst = @alloc(3, 1);
        @byte_copy(dst, src, 3);
        let result: i32 = @intCast(@byte_read(dst, 0))
            + @intCast(@byte_read(dst, 1))
            + @intCast(@byte_read(dst, 2));
        @free(src, 3, 1);
        @free(dst, 3, 1);
        result // 21
    }
}
```

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

`@ptr_read_unaligned` and `@ptr_write_unaligned` each require an
enclosing `checked` block. Aside from the alignment
obligation they carry the same legality rules as `@ptr_read`/`@ptr_write`: the
write pointer is `ptr mut T`, the read pointer is `ptr const T` or `ptr mut T`,
and a written value has type `T`.

{{ rule(id="9.2:14m", cat="example") }}

```rue
fn main() -> i32 {
    checked {
        let bytes: u64 = @intCast(@size_of(i32));
        let align: u64 = @intCast(@align_of(i32));
        let raw: ptr mut u8 = @alloc(bytes, align);
        let p: ptr mut i32 = @int_to_ptr(@ptr_to_int(raw));
        @ptr_write_unaligned(p, 1234);
        let v: i32 = @ptr_read_unaligned(p);
        @free(raw, bytes, align);
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
