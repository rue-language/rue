---
id: 0021
title: Standard Input
status: proposal
tags: [io, intrinsics, runtime]
feature-flag: stdin-input
created: 2025-12-31
accepted:
implemented:
spec-sections: ["4.13"]
superseded-by:
---

# ADR-0021: Standard Input

## Status

Proposal

## Summary

Add a `@read_line()` intrinsic that reads a line of text from standard input and returns it as a `String`. On EOF or I/O error, the intrinsic panics. This provides the simplest possible input mechanism for Rue programs.

## Context

### Current I/O Situation

Rue currently has output via `@dbg`, but no input mechanism:

```rue
fn main() -> i32 {
    @dbg("Hello, world!");  // Output works
    // But how do we read user input?
    0
}
```

Without input, Rue programs cannot:
- Interact with users
- Process data from pipes
- Read configuration at runtime

### Design Philosophy: Start Simple

Rue is building up capabilities incrementally. For input, the simplest useful primitive is reading a line of text. More sophisticated I/O (files, binary data, non-blocking I/O) can come later.

Key constraints:
- **No generics yet**: Can't have `Result<String, Error>`
- **No error enums**: Can't return structured errors
- **Mutable strings exist**: Can return an owned `String`

Given these constraints, the simplest approach is: read a line, return a String, panic on failure.

### Why Panic on Error?

For a first implementation, panicking on error is acceptable because:

1. **EOF is rare in interactive use**: Users typically provide input
2. **Pipe failures are exceptional**: Usually indicate broken pipelines
3. **Matches existing patterns**: `@intCast` panics on overflow, bounds checks panic
4. **No error handling overhead**: Simple control flow

Later, we can add `@try_read_line()` that returns a sentinel (empty string on EOF) or eventually a proper `Result` type when generics exist.

### Dependencies

This feature requires:
- **ADR-0014 (Mutable Strings)**: To return a `String` ✓ (implemented)
- **Runtime syscall layer**: Already exists for `write`, needs `read`

## Decision

### The `@read_line` Intrinsic

```rue
@read_line() -> String
```

Reads characters from standard input until a newline (`\n`) is encountered. Returns the line as a `String`, **excluding** the trailing newline.

#### Behavior

1. Read bytes from stdin (file descriptor 0) into an internal buffer
2. Accumulate bytes until `\n` is found or EOF is reached
3. If `\n` found: return the bytes before it as a String (newline is consumed but not included)
4. If EOF with no bytes read: panic with "unexpected end of input"
5. If EOF with some bytes: return those bytes as a String (partial line)
6. If read error: panic with "input error"

#### Examples

```rue
fn main() -> i32 {
    @dbg("What is your name?");
    let name = @read_line();
    @dbg("Hello, ");
    @dbg(name);
    0
}
```

Running interactively:
```
$ ./program
What is your name?
Alice
Hello,
Alice
```

#### Edge Cases

| Input | Result |
|-------|--------|
| `"hello\n"` | `"hello"` |
| `"hello\nworld\n"` | First call: `"hello"`, second call: `"world"` |
| `"hello"` (no newline, EOF) | `"hello"` |
| Empty (immediate EOF) | Panic: "unexpected end of input" |
| `"\n"` (just newline) | `""` (empty string) |

### Runtime Implementation

Add to the platform-specific runtime (`x86_64_linux.rs`, `aarch64_linux.rs`, `aarch64_macos.rs`):

```rust
pub fn read(fd: u64, buf: *mut u8, len: usize) -> i64 {
    // syscall read(fd, buf, len)
    // Returns bytes read, 0 on EOF, negative on error
}
```

Add to the shared runtime (`lib.rs`):

```rust
/// Read a line from stdin, return as String.
/// Panics on EOF with no data or on I/O error.
#[unsafe(no_mangle)]
pub extern "C" fn __rue_read_line() -> (*mut u8, u64, u64) {
    // 1. Allocate initial buffer (e.g., 128 bytes)
    // 2. Read in a loop until \n or EOF
    // 3. Grow buffer as needed (using string allocation functions)
    // 4. On EOF with no data: panic
    // 5. On error: panic
    // 6. Return (ptr, len, cap) for String
}
```

The function returns the three components of a String (ptr, len, cap) which codegen assembles into the String value.

### Buffering Consideration

For simplicity, the initial implementation does **not** use stdio-style buffering. Each `@read_line()` call reads byte-by-byte until newline. This is inefficient for heavy I/O but fine for:
- Interactive input (human typing speed)
- Simple scripts reading a few lines

A future optimization could add an internal buffer to reduce syscalls.

### Codegen

The `@read_line` intrinsic is lowered to:

1. Call `__rue_read_line` (returns ptr, len, cap in registers/memory)
2. Construct String value from the three components

This follows the same pattern as `String::new()` and other String-returning functions.

## Implementation Phases

### Phase 1: Runtime Read Syscall

- [ ] Add `SYS_READ` constant to platform modules
- [ ] Add `read(fd, buf, len) -> i64` to platform modules
- [ ] Add `STDIN` constant (0) to platform modules
- [ ] Unit tests for read syscall

### Phase 2: Line Reading Runtime Function

- [ ] Add `__rue_read_line() -> (ptr, len, cap)` to runtime
- [ ] Implement byte-by-byte reading until newline
- [ ] Handle EOF (panic if no data, otherwise return partial)
- [ ] Handle errors (panic with message)
- [ ] Unit tests in Rust

### Phase 3: Intrinsic in Compiler

- [ ] Add `@read_line` to known intrinsics in sema
- [ ] Type check: no arguments, returns `String`
- [ ] Lower to `__rue_read_line` call in codegen
- [ ] Handle String return value assembly

### Phase 4: Spec and Tests

- [ ] Add `@read_line` to spec section 4.13 (Intrinsics)
- [ ] Add spec tests (may need special handling for stdin in test harness)
- [ ] Document in language guide

## Consequences

### Positive

- **Enables interactive programs**: Can now read user input
- **Simple API**: One function, easy to understand
- **Matches existing patterns**: Returns String like other constructors
- **No new concepts**: Uses existing String type

### Negative

- **Panics on EOF**: Programs can't gracefully handle end of input
- **Inefficient**: Byte-by-byte reading without buffering
- **No binary input**: Only line-oriented text

### Neutral

- **Newline handling**: Newline is consumed but not returned (common convention)
- **UTF-8**: Input is treated as bytes, conventionally UTF-8 (like all Rue strings)

## Open Questions

1. **Should empty input (just EOF) return empty string instead of panicking?**
   - Current decision: Panic, because it likely indicates unexpected EOF
   - Alternative: Return empty string, let caller check `.is_empty()`

2. **Should we strip `\r\n` on Windows (future)?**
   - Current: Not applicable (Linux/macOS only)
   - Future: Probably yes, normalize to `\n`

3. **Test harness support?**
   - Need to figure out how spec tests can provide stdin input
   - Options: heredoc in test file, separate input file, skip stdin tests

## Future Work

- **`@try_read_line() -> String`**: Return empty string on EOF instead of panicking
- **`@read_bytes(n: u64) -> [u8; N]`**: Read exact number of bytes
- **File I/O**: `@open`, `@read_file`, `@write_file`
- **Buffered I/O**: Internal buffering for efficiency
- **Result type**: Once generics exist, `@read_line() -> Result<String, IoError>`

## References

- [ADR-0014: Mutable Strings](0014-mutable-strings.md) - String type used for return value
- [POSIX read(2)](https://pubs.opengroup.org/onlinepubs/9699919799/functions/read.html) - Underlying syscall
- [Rust std::io::stdin](https://doc.rust-lang.org/std/io/fn.stdin.html) - Similar high-level API
