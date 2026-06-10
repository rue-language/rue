# Runtime ABI Specification

This document describes the Application Binary Interface (ABI) between the Rue compiler and the `rue-runtime` library. It defines the contract that allows compiler-generated machine code to call runtime functions for operations that cannot be efficiently or safely inlined.

## Overview

The Rue compiler generates machine code that directly calls functions in the `rue-runtime` static library (`.a` file). The runtime provides:

- **Process management**: Program entry/exit
- **Memory allocation**: Heap allocation for Strings and other dynamic types
- **Runtime checks**: Division by zero, integer overflow detection
- **Built-in operations**: String operations, I/O, debugging output
- **Type lifecycle**: Constructor, destructor, and method implementations for built-in types

The runtime is compiled as `#![no_std]` with no libc dependency, using direct OS syscalls.

## Calling Conventions

All runtime functions use the standard C calling convention (`extern "C"`) for the target platform.

### x86-64 Linux (System V AMD64 ABI)

| Purpose | Register |
|---------|----------|
| 1st argument | `rdi` |
| 2nd argument | `rsi` |
| 3rd argument | `rdx` |
| 4th argument | `rcx` |
| 5th argument | `r8` |
| 6th argument | `r9` |
| 7th+ arguments | Stack (right-to-left) |
| Return value | `rax` |
| Return value (128-bit) | `rax:rdx` |

**Register preservation:**
- **Caller-saved**: `rax`, `rcx`, `rdx`, `rsi`, `rdi`, `r8`, `r9`, `r10`, `r11`
  - The caller must save these if their values are needed after the call
- **Callee-saved**: `rbx`, `rbp`, `r12`, `r13`, `r14`, `r15`
  - The callee (runtime function) must preserve these
- **Special**: `rsp` (stack pointer) must be preserved, 16-byte aligned before `call`

**Syscall clobbers**: Linux syscalls clobber `rcx` and `r11` in addition to the return register.

### aarch64 macOS and Linux (ARM64 Procedure Call Standard)

| Purpose | Register |
|---------|----------|
| 1st argument | `x0` (or `w0` for 32-bit) |
| 2nd argument | `x1` (or `w1` for 32-bit) |
| 3rd argument | `x2` (or `w2` for 32-bit) |
| 4th argument | `x3` (or `w3` for 32-bit) |
| 5th argument | `x4` |
| 6th argument | `x5` |
| 7th argument | `x6` |
| 8th argument | `x7` |
| 9th+ arguments | Stack |
| Return value | `x0` (or `w0` for 32-bit) |

**Register preservation:**
- **Caller-saved**: `x0-x18`, `x29`, `x30`
  - The caller must save these if needed after the call
- **Callee-saved**: `x19-x28`
  - The callee must preserve these
- **Special**:
  - `x29` (frame pointer)
  - `x30` (link register)
  - `sp` (stack pointer) must be 16-byte aligned

**Platform differences:**
- macOS syscalls use different numbers than Linux (e.g., `write` is `0x2000004` on macOS, `64` on Linux aarch64)
- macOS requires different `mmap` flags for executable memory

## Struct Return Convention (sret)

For functions that return large structs (like `String`, which is 24 bytes), the **struct return (sret)** convention is used:

1. The **caller** allocates space on the stack for the return value
2. The caller passes a **pointer to this space** as a **hidden first argument**
3. The callee writes the return value to this memory
4. No value is returned in registers

**Example:**
```rust
// Rue source:
let s = String.new();

// Becomes (conceptually):
let mut s_storage: StringResult = uninitialized;
String__new(&mut s_storage);  // First arg is pointer to s_storage
// s_storage now contains the String value
```

On x86-64, the sret pointer goes in `rdi` (shifting all other arguments right). On aarch64, it goes in `x8`.

## Memory Layout

### Scalar Types

| Type | Size | Alignment | Notes |
|------|------|-----------|-------|
| `i8` | 1 byte | 1 | Signed 8-bit integer |
| `i16` | 2 bytes | 2 | Signed 16-bit integer |
| `i32` | 4 bytes | 4 | Signed 32-bit integer |
| `i64` | 8 bytes | 8 | Signed 64-bit integer |
| `u8` | 1 byte | 1 | Unsigned 8-bit integer |
| `u16` | 2 bytes | 2 | Unsigned 16-bit integer |
| `u32` | 4 bytes | 4 | Unsigned 32-bit integer |
| `u64` | 8 bytes | 8 | Unsigned 64-bit integer |
| `bool` | 1 byte | 1 | 0 = false, 1 = true |
| `()` (unit) | 0 bytes | 1 | Zero-sized type |

### String Type

The `String` type is represented as a fat pointer with three 64-bit fields:

```c
struct String {
    uint8_t* ptr;   // Pointer to heap-allocated UTF-8 data (8 bytes)
    uint64_t len;   // Length in bytes (8 bytes)
    uint64_t cap;   // Capacity in bytes (8 bytes)
};
// Total: 24 bytes, 8-byte aligned
```

**Field semantics:**
- `ptr`: Points to heap memory containing UTF-8 bytes (may be null if capacity is 0)
- `len`: Number of valid UTF-8 bytes (always `<= cap`)
- `cap`: Total allocated capacity in bytes

**Heap allocation:**
- The `ptr` field points to memory allocated by `__rue_alloc`
- When a String is dropped, `__rue_drop_String` frees this memory
- Empty strings may have `ptr = null`, `len = 0`, `cap = 0`

### Arrays

Fixed-size arrays are stored inline (not heap-allocated):

```rust
// Rue source:
let arr: [i32; 4] = [1, 2, 3, 4];

// Memory layout:
// [1, 2, 3, 4]  // 16 consecutive bytes, 4-byte aligned
```

- **Size**: `element_size * length`
- **Alignment**: Same as element type
- **Storage**: Inline (stack or struct field)

### Structs

Structs are laid out with fields in declaration order, with padding for alignment:

```rust
// Rue source:
struct Point {
    x: i32,    // offset 0, size 4
    y: i64,    // offset 8, size 8 (4 bytes padding before this)
}

// Memory layout: 16 bytes total, 8-byte aligned
// [xxxx][----][yyyyyyyy]
//  x    pad    y
```

**Rules:**
- Fields appear in declaration order
- Each field is aligned to its type's alignment
- Padding is inserted to satisfy alignment requirements
- The struct's size is rounded up to its alignment

## Heap Allocation

The runtime provides a simple bump allocator backed by `mmap`.

### Allocation Interface

```c
// Allocate memory
uint8_t* __rue_alloc(uint64_t size, uint64_t align);

// Free memory (currently a no-op)
void __rue_free(uint8_t* ptr, uint64_t size, uint64_t align);

// Reallocate memory
uint8_t* __rue_realloc(uint8_t* ptr, uint64_t old_size, uint64_t new_size, uint64_t align);
```

**Arguments:**
- `size`: Number of bytes to allocate (must be > 0)
- `align`: Required alignment in bytes (must be a power of 2)
- `ptr`: Pointer to existing allocation (or null)
- `old_size`: Size of existing allocation
- `new_size`: Desired new size

**Return values:**
- `__rue_alloc`: Pointer to allocated memory (8-byte aligned, zero-initialized), or null on failure
- `__rue_realloc`: Pointer to reallocated memory, or null on failure

**Failure conditions:**
- `size == 0` (for alloc)
- `align == 0` or not a power of 2
- Out of memory (mmap fails)

**Implementation details:**
- Memory is allocated in 64 KiB arenas from `mmap`
- Allocations bump a pointer forward within the arena
- `free` is a no-op (memory is reclaimed when the program exits)
- `realloc` may return a new pointer and copy data

## Runtime Function Reference

### Program Entry and Exit

#### `__rue_exit`
```c
void __rue_exit(int32_t code);
```

**Purpose**: Exit the program with the given exit code.

**Arguments:**
- `code` (in `edi`/`w0`): Exit code (typically the return value of `main`)

**Behavior**: Invokes the `exit` syscall. Never returns.

**Generated by**: Compiler-generated `_start` function after `main` returns.

### Debug Output

#### `__rue_print_i32`
```c
void __rue_print_i32(int32_t value);
```

**Purpose**: Print a signed 32-bit integer to stdout, followed by a newline.

**Arguments:**
- `value` (in `edi`/`w0`): The i32 value to print

**Clobbers**: `rax`, `rcx`, `rdx`, `rsi`, `r8-r11` (x86-64)

**Generated by**: The `@dbg` builtin for i32 values.

#### `__rue_print_bool`
```c
void __rue_print_bool(uint8_t value);
```

**Purpose**: Print a boolean value (`true` or `false`) to stdout, followed by a newline.

**Arguments:**
- `value` (in `dil`/`w0`): 0 for false, 1 for true

**Clobbers**: `rax`, `rcx`, `rdx`, `rsi`, `r8-r11` (x86-64)

**Generated by**: The `@dbg` builtin for bool values.

#### `__rue_print_str`
```c
void __rue_print_str(const uint8_t* ptr, uint64_t len);
```

**Purpose**: Print a string (pointer + length) to stdout, followed by a newline.

**Arguments:**
- `ptr` (in `rdi`/`x0`): Pointer to UTF-8 bytes
- `len` (in `rsi`/`x1`): Number of bytes

**Clobbers**: `rax`, `rcx`, `rdx`, `rsi`, `rdi`, `r8-r11` (x86-64)

**Generated by**: The `@dbg` builtin for string literals and String values.

### Runtime Errors

#### `__rue_error_div_by_zero`
```c
void __rue_error_div_by_zero(void);
```

**Purpose**: Report a division by zero error and exit with code 101.

**Generated by**: Integer division and remainder operations when the divisor is not a compile-time constant.

**Behavior**: Prints `"runtime error: division by zero"` to stderr and exits.

#### `__rue_error_overflow`
```c
void __rue_error_overflow(void);
```

**Purpose**: Report an integer overflow error and exit with code 101.

**Generated by**: Checked arithmetic operations (add, sub, mul) that overflow.

**Behavior**: Prints `"runtime error: integer overflow"` to stderr and exits.

### String Operations

#### `__rue_str_eq`
```c
uint8_t __rue_str_eq(const uint8_t* ptr1, uint64_t len1, const uint8_t* ptr2, uint64_t len2);
```

**Purpose**: Compare two strings for equality.

**Arguments:**
- `ptr1` (in `rdi`/`x0`): Pointer to first string's bytes
- `len1` (in `rsi`/`x1`): Length of first string
- `ptr2` (in `rdx`/`x2`): Pointer to second string's bytes
- `len2` (in `rcx`/`x3`): Length of second string

**Return value**: 1 if equal, 0 if not equal (in `al`/`w0`)

**Generated by**: String equality operator (`str1 == str2`)

**Algorithm:**
1. Fast path: If lengths differ, return 0
2. Fast path: If pointers are equal, return 1
3. Slow path: Compare bytes one by one

#### `String__new`
```c
void String__new(StringResult* out);
```

**Purpose**: Create an empty String.

**Arguments:**
- `out` (in `rdi`/`x8`): Pointer to caller-allocated StringResult struct (sret)

**Writes**: `out->ptr = null`, `out->len = 0`, `out->cap = 0`

**Generated by**: `String.new()` constructor calls

#### `String__with_capacity`
```c
void String__with_capacity(StringResult* out, uint64_t capacity);
```

**Purpose**: Create a String with the specified capacity.

**Arguments:**
- `out` (in `rdi`/`x8`): Pointer to caller-allocated StringResult struct (sret)
- `capacity` (in `rsi`/`x0`): Desired capacity in bytes

**Writes**: `out->ptr` (allocated buffer), `out->len = 0`, `out->cap = capacity`

**Behavior**: Allocates `capacity` bytes (minimum 16) using `__rue_alloc`.

**Generated by**: `String.with_capacity(n)` constructor calls

#### `String__from_str`
```c
void String__from_str(StringResult* out, const uint8_t* ptr, uint64_t len);
```

**Purpose**: Create a String by copying from a string slice.

**Arguments:**
- `out` (in `rdi`/`x8`): Pointer to caller-allocated StringResult struct (sret)
- `ptr` (in `rsi`/`x0`): Pointer to source bytes
- `len` (in `rdx`/`x1`): Number of bytes to copy

**Writes**: `out->ptr` (heap allocation with copied data), `out->len = len`, `out->cap >= len`

**Generated by**: String literals converted to owned Strings

#### `String__len`
```c
uint64_t String__len(const uint8_t* ptr, uint64_t len, uint64_t cap);
```

**Purpose**: Get the length of a String in bytes.

**Arguments:**
- `ptr` (in `rdi`/`x0`): String pointer (unused)
- `len` (in `rsi`/`x1`): String length
- `cap` (in `rdx`/`x2`): String capacity (unused)

**Return value**: `len` (in `rax`/`x0`)

**Generated by**: `string.len()` method calls

**Note**: This function is trivial (just returns the `len` field) but exists for consistency with the method call ABI.

#### `String__capacity`
```c
uint64_t String__capacity(const uint8_t* ptr, uint64_t len, uint64_t cap);
```

**Purpose**: Get the capacity of a String in bytes.

**Arguments:**
- `ptr` (in `rdi`/`x0`): String pointer (unused)
- `len` (in `rsi`/`x1`): String length (unused)
- `cap` (in `rdx`/`x2`): String capacity

**Return value**: `cap` (in `rax`/`x0`)

**Generated by**: `string.capacity()` method calls

#### `String__is_empty`
```c
uint8_t String__is_empty(const uint8_t* ptr, uint64_t len, uint64_t cap);
```

**Purpose**: Check if a String is empty.

**Arguments:**
- `ptr` (in `rdi`/`x0`): String pointer (unused)
- `len` (in `rsi`/`x1`): String length
- `cap` (in `rdx`/`x2`): String capacity (unused)

**Return value**: 1 if `len == 0`, 0 otherwise (in `al`/`w0`)

**Generated by**: `string.is_empty()` method calls

#### `String__push_str`
```c
void String__push_str(StringResult* out, const uint8_t* self_ptr, uint64_t self_len, uint64_t self_cap,
                       const uint8_t* str_ptr, uint64_t str_len);
```

**Purpose**: Append a string slice to a String, reallocating if necessary.

**Arguments:**
- `out` (in `rdi`/`x8`): Pointer to caller-allocated StringResult struct (sret)
- `self_ptr` (in `rsi`/`x0`): String's current pointer
- `self_len` (in `rdx`/`x1`): String's current length
- `self_cap` (in `rcx`/`x2`): String's current capacity
- `str_ptr` (in `r8`/`x3`): Pointer to bytes to append
- `str_len` (in `r9`/`x4`): Number of bytes to append

**Writes**: Updated String in `out` (may have new `ptr` if reallocation occurred)

**Generated by**: `string.push_str(other)` method calls

**Behavior**: If `self_len + str_len > self_cap`, reallocates with capacity `max(self_cap * 2, self_len + str_len)`.

#### `String__clone`
```c
void String__clone(StringResult* out, const uint8_t* ptr, uint64_t len, uint64_t cap);
```

**Purpose**: Create a deep copy of a String.

**Arguments:**
- `out` (in `rdi`/`x8`): Pointer to caller-allocated StringResult struct (sret)
- `ptr` (in `rsi`/`x0`): Source String pointer
- `len` (in `rdx`/`x1`): Source String length
- `cap` (in `rcx`/`x2`): Source String capacity

**Writes**: New String in `out` with copied data

**Generated by**: Explicit `string.clone()` calls or copy semantics

**Behavior**: Allocates new heap memory and copies the string data.

#### `__rue_drop_String`
```c
void __rue_drop_String(uint8_t* ptr, uint64_t len, uint64_t cap);
```

**Purpose**: Free a String's heap allocation.

**Arguments:**
- `ptr` (in `rdi`/`x0`): String pointer
- `len` (in `rsi`/`x1`): String length (unused)
- `cap` (in `rdx`/`x2`): String capacity

**Generated by**: Compiler-inserted drop calls at end of scope

**Behavior**: Calls `__rue_free(ptr, cap, 8)` to release the heap memory. Safe to call multiple times (no-op if `ptr` is null).

**Safety**: Must not be called twice on the same String value (double-free).

### I/O Operations

#### `__rue_readLine`
```c
void __rue_readLine(StringResult* out);
```

**Purpose**: Read a line from stdin into a String.

**Arguments:**
- `out` (in `rdi`/`x8`): Pointer to caller-allocated StringResult struct (sret)

**Writes**: String containing the line (without newline) in `out`

**Generated by**: `readLine()` function calls

**Behavior**: Reads bytes from stdin until newline (`\n`) or EOF. The newline is not included in the result. Returns empty string on EOF.

### Intrinsics

#### `__rue_intrinsic_likely`
```c
uint8_t __rue_intrinsic_likely(uint8_t cond);
```

**Purpose**: Hint that a condition is likely to be true (branch prediction hint).

**Arguments:**
- `cond` (in `dil`/`w0`): Boolean condition (0 or 1)

**Return value**: `cond` (unchanged, in `al`/`w0`)

**Generated by**: `@likely(condition)` builtin

**Behavior**: Identity function; the compiler uses this as a marker for branch prediction hints.

#### `__rue_intrinsic_unlikely`
```c
uint8_t __rue_intrinsic_unlikely(uint8_t cond);
```

**Purpose**: Hint that a condition is unlikely to be true (branch prediction hint).

**Arguments:**
- `cond` (in `dil`/`w0`): Boolean condition (0 or 1)

**Return value**: `cond` (unchanged, in `al`/`w0`)

**Generated by**: `@unlikely(condition)` builtin

**Behavior**: Identity function; the compiler uses this as a marker for branch prediction hints.

#### `__rue_random_u64`
```c
uint64_t __rue_random_u64(void);
```

**Purpose**: Generate a random 64-bit unsigned integer.

**Return value**: Random u64 value (in `rax`/`x0`)

**Generated by**: `@random()` builtin

**Behavior**: Uses platform-specific randomness source (e.g., `/dev/urandom` or `getrandom` syscall).

#### `__rue_parse_i32`
```c
int32_t __rue_parse_i32(const uint8_t* ptr, uint64_t len, int32_t* success_out);
```

**Purpose**: Parse a string as an i32.

**Arguments:**
- `ptr` (in `rdi`/`x0`): Pointer to string bytes
- `len` (in `rsi`/`x1`): String length
- `success_out` (in `rdx`/`x2`): Pointer to output flag (writes 1 on success, 0 on failure)

**Return value**: Parsed i32 value (in `eax`/`w0`), or 0 if parsing failed

**Generated by**: `@parseInt(string)` builtin

**Behavior**: Parses decimal integers with optional leading `+` or `-`. Sets `*success_out = 0` on invalid input or overflow.

## Error Handling and Panics

### Runtime Errors

The runtime reports errors by printing a message to stderr and exiting with code 101:

```
runtime error: division by zero
runtime error: integer overflow
```

### Panic Mechanism

Currently, there is no stack unwinding. When a runtime error occurs:

1. Error message is written to stderr (fd 2)
2. Program calls `exit(101)` syscall
3. Process terminates immediately

### Exit Codes

| Code | Meaning |
|------|---------|
| 0-100 | User-specified exit code from `main` |
| 101 | Runtime error (overflow, division by zero, etc.) |
| 102-255 | Reserved for future use |

## Platform-Specific Details

### x86-64 Linux

**Syscall numbers:**
```c
#define SYS_READ    0
#define SYS_WRITE   1
#define SYS_MMAP    9
#define SYS_MUNMAP  11
#define SYS_EXIT    60
```

**Syscall convention:**
- Number in `rax`
- Args in `rdi`, `rsi`, `rdx`, `r10`, `r8`, `r9` (note: `r10` instead of `rcx`)
- Return in `rax` (negative on error)
- Clobbers `rcx`, `r11`

**Stack alignment**: Must be 16-byte aligned before `call` instruction (enforced by System V ABI).

### aarch64 macOS

**Syscall numbers:**
```c
#define SYS_READ    0x2000003
#define SYS_WRITE   0x2000004
#define SYS_MMAP    0x20000C5
#define SYS_MUNMAP  0x2000049
#define SYS_EXIT    0x2000001
```

**Syscall convention:**
- Number in `x16`
- Args in `x0-x5`
- Return in `x0` (negative on error)
- Invoke with `svc #0x80`

**Stack alignment**: Must be 16-byte aligned.

### aarch64 Linux

**Syscall numbers:**
```c
#define SYS_READ    63
#define SYS_WRITE   64
#define SYS_MMAP    222
#define SYS_MUNMAP  215
#define SYS_EXIT    93
```

**Syscall convention:**
- Number in `x8`
- Args in `x0-x5`
- Return in `x0` (negative on error)
- Invoke with `svc #0`

**Stack alignment**: Must be 16-byte aligned.

## Symbol Name Mangling

All runtime functions use C linkage (`#[no_mangle]`) and are exported with their plain names:

- `__rue_exit`
- `__rue_alloc`
- `__rue_str_eq`
- `String__new`
- `String__len`
- etc.

**Naming convention:**
- `__rue_*`: Core runtime functions (allocation, errors, I/O)
- `TypeName__method`: Method implementations for built-in types (e.g., `String__new`)
- `__rue_intrinsic_*`: Intrinsic functions (builtins)
- `__rue_drop_*`: Destructor functions (e.g., `__rue_drop_String`)

## Linking

The runtime is compiled as a static library (`librue_runtime.a`) and linked into every Rue executable:

1. **Build**: `rue-runtime` is compiled with `-Copt-level=z` and LTO
2. **Archive**: Object files are packaged into `librue_runtime.a` using `ar`
3. **Link**: `rue-linker` extracts needed objects from the archive and links them into the final ELF/Mach-O executable

**Symbol resolution:**
- The linker scans the archive for symbols referenced by the generated code
- Only objects containing referenced symbols are included (dead code elimination)
- The linker resolves relocations and produces a statically-linked executable

## Future Extensions

### Planned Features

- **Stack unwinding**: For better error recovery and resource cleanup
- **Real garbage collection**: Replace bump allocator with a proper GC
- **Thread support**: Multi-threaded runtime with thread-local allocators
- **FFI**: Calling C libraries from Rue

### Stability Guarantees

This ABI is **unstable** and may change between compiler versions. Runtime and compiler must be built from the same version of the Rue codebase.

When the language reaches v1.0, the runtime ABI will be stabilized and versioned.
