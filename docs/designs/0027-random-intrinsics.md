---
id: 0027
title: Random Number Intrinsics
status: accepted
tags: [intrinsics, runtime, semantics]
feature-flag: random
created: 2026-01-02
accepted: 2026-01-02
implemented:
spec-sections: []
superseded-by:
---

# ADR-0027: Random Number Intrinsics

## Status

Accepted

## Summary

Add `@random_u32()` and `@random_u64()` intrinsics that generate cryptographically-secure random numbers using platform syscalls, enabling interactive examples like guessing games without requiring a complex PRNG implementation or global state management.

## Context

Interactive programs like guessing games require randomness. The Rust Book's guessing game tutorial is a canonical example for learning:

```rust
use rand::Rng;

fn main() {
    let secret = rand::thread_rng().gen_range(1..=100);
    // ... rest of game
}
```

Rue currently has no randomness capability. Adding it requires choosing between several approaches:

1. **Intrinsic functions** - Simple, stateless, syscall-based
2. **Built-in PRNG type** - Stateful, requires passing state or global variables
3. **External library** - Requires module system and package management (not yet available)

For Rue's current stage of development, intrinsics are the best fit:
- No generics yet (needed for `Random<T>`)
- No module system (needed for external libraries)
- Educational use cases don't require determinism or PRNG state
- Fits existing intrinsic pattern (`@dbg`, `@read_line`, etc.)

## Decision

Add two new intrinsics:

### `@random_u32()`

**Signature:**
```rue
@random_u32() -> u32
```

**Behavior:**
- Returns a random unsigned 32-bit integer
- Takes no arguments
- Non-deterministic - each call produces a different value
- Uses platform entropy source (cryptographically secure)

**Example:**
```rue
fn main() -> i32 {
    let secret: u32 = (@random_u32() % 100) + 1;  // 1-100
    @dbg("Guess the number between 1 and 100!");

    let mut guesses = 0;
    loop {
        let input = @read_line();
        let guess = @parse_u32(input);
        guesses = guesses + 1;

        if guess < secret {
            @dbg("Too low!");
        } else if guess > secret {
            @dbg("Too high!");
        } else {
            @dbg("You got it!");
            break;
        }
    }

    @intCast(guesses)
}
```

### `@random_u64()`

**Signature:**
```rue
@random_u64() -> u64
```

**Behavior:**
- Identical to `@random_u32()` but returns 64-bit values
- Provided for consistency with other intrinsics that have 32/64-bit variants

### Runtime Implementation

**Platform-specific entropy sources:**

| Platform | Method | Syscall/API |
|----------|--------|-------------|
| Linux x86-64 | `getrandom()` | Syscall #318 |
| Linux aarch64 | `getrandom()` | Syscall #278 |
| macOS aarch64 | `getentropy()` | libSystem function |

**Error handling:**
- If entropy source is unavailable: runtime panic with "randomness unavailable"
- If syscall fails: runtime panic with "random number generation failed"

**Implementation:**
```rust
// In rue-runtime
#[unsafe(no_mangle)]
pub extern "C" fn __rue_random_u32(out: *mut u32) {
    unsafe {
        let mut bytes = [0u8; 4];
        platform::get_random_bytes(&mut bytes);
        *out = u32::from_ne_bytes(bytes);
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn __rue_random_u64(out: *mut u64) {
    unsafe {
        let mut bytes = [0u8; 8];
        platform::get_random_bytes(&mut bytes);
        *out = u64::from_ne_bytes(bytes);
    }
}
```

### Compiler Pipeline Changes

1. **Lexer**: No change (already handles `@` prefix)
2. **Parser**: Add `RandomU32` and `RandomU64` to `IntrinsicKind` enum
3. **Sema**:
   - Validate zero arguments
   - Type as `u32` or `u64` return type
   - Gate behind `PreviewFeature::Random`
4. **CFG**: Lower to `Intrinsic::RandomU32` or `Intrinsic::RandomU64`
5. **Codegen**: Generate `call __rue_random_u32` or `call __rue_random_u64`
6. **Linker**: Link runtime functions (already automatic)

### Specification

Add to `docs/spec/src/04-expressions/13-intrinsics.md`:

```markdown
## `@random_u32`

{{ rule(id="4.13:XX", cat="normative") }}

The `@random_u32` intrinsic generates a random unsigned 32-bit integer.

{{ rule(id="4.13:XX", cat="normative") }}

`@random_u32` accepts no arguments.

{{ rule(id="4.13:XX", cat="normative") }}

The return type of `@random_u32` is `u32`.

{{ rule(id="4.13:XX", cat="dynamic-semantics") }}

Each call to `@random_u32` returns a non-deterministic value using a
platform-provided cryptographically-secure entropy source.

{{ rule(id="4.13:XX", cat="dynamic-semantics") }}

If the platform entropy source is unavailable or fails, a runtime panic occurs.

## `@random_u64`

{{ rule(id="4.13:XX", cat="normative") }}

The `@random_u64` intrinsic behaves identically to `@random_u32` but returns
a random unsigned 64-bit integer.

{{ rule(id="4.13:XX", cat="normative") }}

The return type of `@random_u64` is `u64`.
```

## Implementation Phases

- [x] **Phase 1: Spec and tests** - rue-ddko
  - Add spec documentation for `@random_u32` and `@random_u64`
  - Add preview-gated spec tests (non-deterministic, so test compilation only)
  - Add `Random` to `PreviewFeature` enum

- [x] **Phase 2: Parser and Sema** - rue-ub3z
  - Add `RandomU32` and `RandomU64` to intrinsic parsing
  - Add type checking (returns u32/u64, takes no args)
  - Add preview feature gate check
  - Add CFG lowering for random intrinsics

- [x] **Phase 3: Runtime implementation** - rue-5852
  - Implement `__rue_random_u32` for x86-64 Linux
  - Implement `__rue_random_u64` for x86-64 Linux
  - Implement for aarch64 macOS (using getentropy from libSystem)
  - Implement for aarch64 Linux
  - Handle error cases (no entropy source)

- [ ] **Phase 4: Codegen** - rue-2h42
  - Add call generation for x86-64 backend
  - Add call generation for aarch64 backend
  - Integration testing with example programs

## Consequences

### Positive

- Enables interactive educational examples (guessing games, dice rolling, simulations)
- Simple mental model - just call the function, no state management
- Platform-native cryptographic quality randomness (suitable for non-crypto uses)
- No runtime state or initialization required
- Fits existing intrinsic pattern
- Works with Rue's `no_std` philosophy

### Negative

- Non-deterministic behavior makes testing harder (can't assert specific values)
- Syscall overhead on every call (vs batched PRNG with local state)
- No way to seed for reproducibility (testing, debugging, simulations)
- Limited to unsigned integers (no floating point, no other distributions)
- Requires different syscalls per platform (maintenance burden)

### Neutral

- Users must implement their own range helpers (`random_range(min, max)`)
- Statistical quality depends on platform entropy source
- Not suitable for cryptographic purposes (but neither advertised as such)

## Open Questions

None - design is approved.

## Future Work

Explicitly out of scope for this ADR:

1. **Seeded PRNG state**: Would require either:
   - Global mutable state (not idiomatic)
   - Passing state through function calls (needs linear types)
   - A built-in PRNG type (needs generics for `Rng<T>`)

2. **Range helpers**: `fn random_range(min, max)` best left to user code or future standard library

3. **Other distributions**: Uniform floats, gaussian, etc. require floating point support

4. **Statistical quality tests**: Validate platform entropy sources meet quality standards

5. **Deterministic mode**: `--deterministic` flag that seeds with fixed value for testing

## References

- Rust's `rand` crate: https://docs.rs/rand/
- Rust Book's guessing game: https://doc.rust-lang.org/book/ch02-00-guessing-game-tutorial.html
- Linux `getrandom()` syscall: https://man7.org/linux/man-pages/man2/getrandom.2.html
- macOS `getentropy()`: https://developer.apple.com/library/archive/documentation/System/Conceptual/ManPages_iPhoneOS/man2/getentropy.2.html
- ADR-0016: Preview Feature Infrastructure
- ADR-0021: Stdin Input (precedent for intrinsics with I/O)
