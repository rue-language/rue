# Runtime and I/O Design Decisions

## Overview

This document outlines the design for adding runtime capabilities and I/O
operations to the Rue programming language. The goal is to provide basic
input/output functionality while maintaining Rue's minimalist philosophy and
direct compilation to native code.

## Design Goals

1. **Minimalism**: Keep the runtime as small as possible
2. **Direct syscalls**: Avoid libc dependency, use Linux syscalls directly
3. **Type safety**: Maintain Rue's type system integrity
4. **Error handling**: Provide basic runtime error detection
5. **Performance**: Minimal overhead for I/O operations

## Architecture Overview

### Current State
- Rue compiles directly to ELF executables
- Programs return their result as exit code (limited to 0-255)
- No runtime library or dependencies
- Direct x86-64 machine code generation

### Proposed Runtime Model
- Minimal runtime embedded in each executable
- Direct Linux syscall interface (no libc)
- Static linking of runtime functions
- Runtime errors return specific exit codes

## Core Components

### 1. Syscall Interface

#### Design Decision: Direct Syscalls
- Use `syscall` instruction directly
- Avoid libc dependency to maintain minimal footprint
- Target Linux x86-64 ABI only (initially)

#### Implementation Approach
```
; Example syscall wrapper for write(2)
; rdi = fd, rsi = buf, rdx = count
sys_write:
    mov rax, 1      ; write syscall number
    syscall
    ret
```

### 2. Print Functions

#### Signatures
```rue
fn println_i32(value: i32) -> ()
fn println_i64(value: i64) -> ()
fn println_bool(value: bool) -> ()
fn println_unit(value: ()) -> ()
```

#### Design Decisions
- Separate function names for each type (no overloading)
- Functions named with type suffix for clarity
- Prints to stdout (fd 1)
- Integer formatting in decimal
- Boolean prints "true" or "false"
- Unit type prints "()"
- Automatic newline after each print
- No format strings initially (keep it simple)
- No output buffering (direct syscalls)

#### Implementation Strategy
1. Convert integers to ASCII digits (stack-based algorithm)
2. Use write(2) syscall to output directly
3. Append newline character
4. Each type has its own implementation function
5. i32 can reuse i64 conversion with sign extension

### 3. Input Function

#### Signature
```rue
fn input() -> i64
```

#### Design Decisions
- Reads from stdin (fd 0)
- Parses decimal integers only
- Skip whitespace before number
- Stop at first non-digit
- Return 0 on parse error (initially)

#### Implementation Strategy
1. Read bytes using read(2) syscall
2. Parse ASCII digits to integer
3. Handle negative numbers
4. Basic error handling (invalid input returns 0)

### 4. Exit Function

#### Signature
```rue
fn exit(code: i64) -> ()
```

#### Design Decisions
- Returns unit type (avoiding need for never type)
- Function never actually returns at runtime
- Exit code truncated to 8 bits (0-255)
- Immediate process termination
- No cleanup handlers (initially)

### 5. Runtime Error Handling

#### Error Types
1. **Division by zero**: Detected by CPU trap
2. **Stack overflow**: Detected by guard page
3. **Integer overflow**: Wraparound behavior (Rust-like)

#### Error Reporting
- Specific exit codes for runtime errors:
  - 250: Division by zero
  - 251: Stack overflow
  - 252: Assertion failure (future)
  - 253: Panic (future)
  - 254: Internal runtime error
  - 255: Reserved

## Memory Layout

### Runtime Code Section
```
.text:
    _start:             ; Entry point
    main:               ; User's main function
    __rue_println_i32:  ; Print i32 implementation
    __rue_println_i64:  ; Print i64 implementation
    __rue_println_bool: ; Print bool implementation
    __rue_println_unit: ; Print unit implementation
    __rue_input:        ; Input implementation
    __rue_exit:         ; Exit implementation
    __rue_itoa:         ; Integer to ASCII helper
    __rue_atoi:         ; ASCII to integer helper
```

### Runtime Data Section
```
.rodata:
    __rue_true_str:  db "true", 10
    __rue_false_str: db "false", 10
    __rue_unit_str:  db "()", 10
```

## Code Generation Changes

### Function Call Convention
- Runtime functions use standard System V AMD64 ABI
- Parameters in rdi, rsi, rdx, rcx, r8, r9
- Return value in rax
- Preserve rbx, rsp, rbp, r12-r15

### Symbol Resolution
- Compiler recognizes built-in functions
- Generates calls to runtime symbols
- Runtime symbols prefixed with `__rue_`

### IR Architecture Clarification
During implementation, we discovered that the documentation refers to "TargetIR" but the actual implementation uses:
- **Instruction enum** (in `rue-codegen`): The high-level, platform-independent IR with virtual registers
- **MachineInstr** (in `rue-ir::target`): The low-level, x86-64 specific IR with physical registers

The `MachineInstr` type effectively serves as the "TargetIR" - it's the target-specific intermediate representation. The `Instruction` enum in codegen is the platform-independent IR that gets lowered to `MachineInstr`. This provides clean separation between platform-independent code generation and target-specific assembly emission.

## Type System Integration

### Built-in Functions
- Add to semantic analyzer's built-in symbol table
- Type checking for println/input functions
- Each println variant has specific type signature
- No overloading needed (distinct names)

### Exit Function Handling
- `exit` function typed as returning `()` for simplicity
- Function never actually returns at runtime
- Compiler can optionally warn about unreachable code after exit
- Future: Add proper never type (`!`) support

## Testing Strategy

### Unit Tests
1. Syscall wrappers
2. Number conversion routines
3. I/O buffering (if implemented)

### Integration Tests
1. Print various integers with println_i32 and println_i64
2. Print booleans with println_bool  
3. Print unit values with println_unit
4. Input parsing edge cases
5. Exit with different codes
6. Runtime error detection

### Example Programs
```rue
fn main() -> i64 {
    println_i64(42);
    println_i32(100);
    println_bool(true);
    println_unit(());
    
    let n: i64 = input();
    println_i64(n * 2);
    
    if n == 0 {
        exit(1);
    }
    
    n
}
```

## Future Enhancements

### Phase 1 (Current)
- Basic println functions for all types (i32, i64, bool, unit)
- Input function (returns i64)
- Exit function
- Direct syscalls
- No buffering

### Phase 2
- String literals and println_str
- Error handling with Result type
- Printf-style formatting

### Phase 3
- File I/O
- Command-line arguments
- Environment variables

### Phase 4
- Memory allocation (malloc/free)
- Dynamic data structures
- More sophisticated runtime
- Output buffering for performance

## Open Questions

1. **Buffering**: Should we buffer output for efficiency?
   - Pro: Better performance for multiple prints
   - Con: More complex, needs flush logic
   - Decision: Start unbuffered, add later if needed

2. **Error Handling**: How to handle input parse errors?
   - Option 1: Return 0 on error (simple)
   - Option 2: Panic/exit (harsh)
   - Option 3: Result type (requires more language features)
   - Decision: Return 0 initially, migrate to Result later

3. **Number Bases**: Support hex/octal/binary?
   - Decision: Decimal only initially

4. **Floating Point**: Add f64 support?
   - Decision: Not in this phase, integers only

## Implementation Order

1. Syscall wrappers (write, read, exit)
2. Exit function
3. println_i64 function
4. println_i32 function
5. println_bool function
6. println_unit function
7. Input function
8. Runtime error handlers
9. Integration with compiler
10. Testing suite

## Alternatives Considered

### Using libc
- Pros: Easier implementation, more features
- Cons: External dependency, larger binary
- Decision: Direct syscalls for minimalism

### Format Strings
- Pros: More flexible, familiar to C programmers
- Cons: Complex parsing, type safety challenges
- Decision: Type-specific functions initially

### Buffered I/O
- Pros: Better performance
- Cons: More complex, state management
- Decision: Start simple, optimize later