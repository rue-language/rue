---
id: 0022
title: Integer Parsing
status: proposal
tags: [intrinsics, runtime, strings]
feature-flag: integer-parsing
created: 2025-12-31
accepted:
implemented:
spec-sections: ["4.13"]
superseded-by:
---

# ADR-0022: Integer Parsing

## Status

Proposal

## Summary

Add intrinsics to parse strings into integer values: `@parse_i32`, `@parse_i64`, `@parse_u32`, `@parse_u64`. These intrinsics accept a `String` and return the corresponding integer type, panicking on invalid input or overflow. This enables programs to process numeric input from users or files.

## Context

### The Input→Process→Output Pattern

With `@read_line()` (ADR-0021) providing input and `@dbg` providing output, we need a way to process numeric input:

```rue
fn main() -> i32 {
    @dbg("Enter a number:");
    let input = @read_line();

    // How do we convert input to an integer?
    let n = ???;  // Need this!

    @dbg(n * 2);
    0
}
```

### Why Intrinsics Instead of Methods?

Two design options exist:

1. **Intrinsics**: `@parse_i32(s)`
2. **Methods**: `s.parse_i32()` or `i32::parse(s)`

We choose intrinsics because:
- Consistent with other conversion operations (`@intCast`)
- No need for associated functions on primitive types yet
- Simpler implementation (no method dispatch)
- Can migrate to methods later if desired

### Error Handling: Panic

Like `@intCast`, parsing panics on failure:
- Invalid characters (e.g., "12abc")
- Overflow (e.g., "999999999999" for i32)
- Empty string

This matches Rue's current pattern: runtime errors are panics. Once we have `Result<T, E>` (requires generics), we can add `@try_parse_i32` that returns a Result.

### What About Other Bases?

For simplicity, we only support base-10 (decimal) parsing. Hexadecimal (`0x`), binary (`0b`), and octal (`0o`) can be added later if needed.

## Decision

### Parsing Intrinsics

```rue
@parse_i32(s: String) -> i32
@parse_i64(s: String) -> i64
@parse_u32(s: String) -> u32
@parse_u64(s: String) -> u64
```

Each intrinsic:
1. Borrows the string (does not consume it)
2. Parses ASCII decimal digits
3. Returns the parsed integer
4. Panics on invalid input or overflow

### Syntax Accepted

The parsed string must match this grammar:

```ebnf
integer_string = [ "-" ] digit { digit } ;
digit = "0" | "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" ;
```

Rules:
- Optional leading `-` for signed types only
- One or more ASCII digits
- No leading/trailing whitespace allowed
- No underscores, no `+` sign
- No `0x`, `0b`, `0o` prefixes

### Valid Examples

| Input | `@parse_i32` | `@parse_u32` |
|-------|--------------|--------------|
| `"42"` | `42` | `42` |
| `"0"` | `0` | `0` |
| `"-17"` | `-17` | panic (negative) |
| `"2147483647"` | `2147483647` | `2147483647` |
| `"-2147483648"` | `-2147483648` | panic (negative) |

### Invalid Examples (All Panic)

| Input | Reason |
|-------|--------|
| `""` | Empty string |
| `"  42"` | Leading whitespace |
| `"42  "` | Trailing whitespace |
| `"4 2"` | Internal whitespace |
| `"12abc"` | Invalid characters |
| `"abc"` | No digits |
| `"+42"` | Plus sign not allowed |
| `"1_000"` | Underscores not allowed |
| `"0x10"` | Hex prefix not allowed |
| `"99999999999"` | Overflow (for i32) |
| `"-1"` | Negative (for unsigned) |

### Panic Messages

Clear, actionable error messages:

| Condition | Message |
|-----------|---------|
| Empty string | `"parse error: empty string"` |
| Invalid character | `"parse error: invalid character"` |
| Overflow | `"parse error: integer overflow"` |
| Negative for unsigned | `"parse error: negative value for unsigned type"` |

### String Consumption

The intrinsics **borrow** the string rather than consuming it:

```rue
fn main() -> i32 {
    let s = "42";
    let n = @parse_i32(s);  // Borrows s
    @dbg(s);                // s is still valid
    @dbg(n);
    0
}
```

This is analogous to how `s.len()` borrows rather than consumes.

### Runtime Implementation

Add to the shared runtime (`lib.rs`):

```rust
/// Parse string as i32. Panics on invalid input or overflow.
#[unsafe(no_mangle)]
pub extern "C" fn __rue_parse_i32(ptr: *const u8, len: u64) -> i32 {
    // 1. Check for empty string
    // 2. Check for leading '-'
    // 3. Parse digits, checking for overflow
    // 4. Panic on any error
}

/// Parse string as i64. Panics on invalid input or overflow.
#[unsafe(no_mangle)]
pub extern "C" fn __rue_parse_i64(ptr: *const u8, len: u64) -> i64 { ... }

/// Parse string as u32. Panics on invalid input, overflow, or negative.
#[unsafe(no_mangle)]
pub extern "C" fn __rue_parse_u32(ptr: *const u8, len: u64) -> u32 { ... }

/// Parse string as u64. Panics on invalid input, overflow, or negative.
#[unsafe(no_mangle)]
pub extern "C" fn __rue_parse_u64(ptr: *const u8, len: u64) -> u64 { ... }
```

Each function receives the string's pointer and length (extracted from the String fat pointer by codegen).

### Codegen

For `@parse_i32(s)`:

1. Extract `ptr` and `len` from String `s`
2. Call `__rue_parse_i32(ptr, len)`
3. Return result in register

The String is borrowed, so no drop is inserted for it (the caller retains ownership).

## Implementation Phases

### Phase 1: Runtime Parsing Functions

- [ ] Add `__rue_parse_i64` to runtime (signed, 64-bit as base case)
- [ ] Add `__rue_parse_u64` to runtime (unsigned, 64-bit)
- [ ] Add `__rue_parse_i32` to runtime (delegates to i64, checks range)
- [ ] Add `__rue_parse_u32` to runtime (delegates to u64, checks range)
- [ ] Unit tests in Rust for all parsing functions
- [ ] Test edge cases: empty, whitespace, overflow, negative

### Phase 2: Intrinsics in Compiler

- [ ] Add `@parse_i32`, `@parse_i64`, `@parse_u32`, `@parse_u64` to known intrinsics
- [ ] Type check: one String argument, returns appropriate integer type
- [ ] Mark String argument as borrowed (not consumed)
- [ ] Lower to appropriate `__rue_parse_*` call in codegen
- [ ] Extract ptr/len from String for call

### Phase 3: Spec and Tests

- [ ] Add parsing intrinsics to spec section 4.13 (Intrinsics)
- [ ] Add spec tests for valid parsing
- [ ] Add spec tests for panic cases (compile_fail with error_contains)
- [ ] Document in language guide

## Consequences

### Positive

- **Enables numeric input**: Complete the input→process→output cycle
- **Clear semantics**: Panic on invalid input, no ambiguity
- **Type-specific**: Each type has its own intrinsic, clear about range
- **Non-consuming**: String can be reused after parsing

### Negative

- **Panics on invalid input**: Can't recover from user typos
- **No whitespace tolerance**: Must trim before parsing
- **Decimal only**: No hex, binary, octal support
- **Four intrinsics**: Verbose compared to a generic `parse<T>`

### Neutral

- **Borrow semantics**: String is borrowed, matches query methods

## Open Questions

1. **Should we allow leading/trailing whitespace?**
   - Current decision: No, be strict
   - Alternative: Trim automatically (more forgiving)
   - Recommendation: Stay strict, add `s.trim()` method later

2. **Should we have `@parse_i8`, `@parse_u8`, etc.?**
   - Current decision: Only 32-bit and 64-bit
   - Can use `@intCast(@parse_i32(s))` for smaller types
   - Add if there's demand

3. **Naming: `@parse_i32` vs `@parseInt32` vs `@str_to_i32`?**
   - `@parse_i32` follows Rust naming (`parse::<i32>()`)
   - Consistent lowercase with underscores

## Future Work

- **`@try_parse_i32(s) -> ???`**: Non-panicking version (needs Result type)
- **Radix support**: `@parse_i32_radix(s, 16)` for hex
- **String methods**: `s.parse_i32()` as method
- **Format intrinsic**: `@format("{}", n)` to convert integer to String
- **Trimming**: `s.trim()` method to remove whitespace before parsing

## References

- [ADR-0021: Standard Input](0021-stdin-input.md) - Provides the input strings
- [ADR-0014: Mutable Strings](0014-mutable-strings.md) - String type
- [Rust str::parse](https://doc.rust-lang.org/std/primitive.str.html#method.parse) - Similar API
- [Go strconv.Atoi](https://pkg.go.dev/strconv#Atoi) - Similar simple parsing
