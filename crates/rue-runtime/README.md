# Rue Runtime

The runtime library for the Rue programming language, providing essential I/O and system functionality.

## Overview

rue-runtime implements the built-in functions available to Rue programs using direct Linux syscalls, avoiding any dependency on libc. This approach results in minimal binary size overhead (< 2KB) while providing essential functionality.

## Built-in Functions

The runtime provides the following built-in functions:

- **`exit(code: i64)`** - Exit the program with the specified exit code (0-255)
- **`println_i64(value: i64)`** - Print a 64-bit integer to stdout
- **`println_i32(value: i32)`** - Print a 32-bit integer to stdout  
- **`println_bool(value: bool)`** - Print a boolean value ("true" or "false")
- **`println_unit(value: ())`** - Print unit type (prints "()")
- **`input() -> i64`** - Read a 64-bit integer from stdin

## Implementation Details

### Direct Syscalls
All I/O operations use direct Linux syscalls:
- `write` (syscall 1) for output
- `read` (syscall 0) for input
- `exit` (syscall 60) for program termination

### Integer Conversion
The runtime includes optimized integer-to-ASCII and ASCII-to-integer conversion routines:
- Handles edge cases including `i64::MIN` (-9223372036854775808)
- Proper whitespace handling for input parsing
- Error handling returns 0 for invalid input

### Error Handling
Runtime errors are handled via signal handlers:
- Division by zero triggers SIGFPE, caught and exits with code 250
- Invalid memory access triggers SIGSEGV, caught and exits with code 251

### Code Generation
The runtime is embedded directly into generated executables:
- Assembly code is generated inline during compilation
- No external libraries or linking required
- Platform-specific optimizations for x86-64

## Usage

This crate is used internally by the Rue compiler and is not intended for direct use. The compiler automatically includes the necessary runtime functions when generating executables.

## Architecture

The runtime is organized into several modules:
- `syscalls.rs` - Direct Linux syscall wrappers
- `codegen.rs` - Runtime code generation for the compiler
- `machine_runtime.rs` - Low-level machine code generation
- `lib.rs` - Public API for the compiler

## Platform Support

Currently supports Linux x86-64 only. The direct syscall approach means the runtime is tightly coupled to the Linux ABI.