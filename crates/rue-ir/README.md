# Rue IR (Intermediate Representation)

The intermediate representation library for the Rue compiler, providing a low-level abstraction over x86-64 machine instructions.

## Overview

rue-ir defines a machine-level intermediate representation that serves as the target for code generation. It provides a thin abstraction over x86-64 assembly while maintaining close correspondence to actual machine instructions.

## Key Components

### Machine Instructions (`MachineInstr`)

The core enum representing x86-64 instructions:

- **Data Movement**: `MovRR`, `MovRI32`, `MovRI64`, `MovRM`, `MovMR`, `MovRM8`, `MovMR8`
- **Arithmetic**: `AddRR`, `AddRI`, `SubRR`, `SubRI`, `ImulRR`, `ImulRI`, `Idiv`
- **Bitwise**: `AndRR`, `Shl`, `Sar`
- **Comparison**: `CmpRR`, `CmpRI`, `SetCC`
- **Control Flow**: `Jmp`, `JmpCC`, `Call`, `Ret`
- **Stack Operations**: `Push`, `Pop`, `EnterFrame`, `LeaveFrame`, `AllocStack`
- **System**: `Syscall`, `Cqo`, `Cld`, `RepStosb`
- **Other**: `Label`, `Movzx`, `LeaLabel`

### Registers

Supports all general-purpose x86-64 registers:
- `Rax`, `Rcx`, `Rdx`, `Rbx`, `Rsp`, `Rbp`, `Rsi`, `Rdi`
- `R8` through `R15`

### Condition Codes

Supports common x86-64 condition codes:
- `Equal`, `NotEqual`
- `Less`, `LessEqual`
- `Greater`, `GreaterEqual`

### Label References

Two types of label references for control flow:
- `Local(u32)` - Local labels within a function
- `Global(String)` - Global function names

## Design Philosophy

rue-ir is designed to be:
- **Minimal**: Only includes instructions actually used by the compiler
- **Direct**: Each IR instruction maps to one or few machine instructions
- **Type-safe**: Rust's type system prevents invalid instruction combinations
- **Extensible**: New instructions can be added as needed

## Usage

This crate is used internally by:
- `rue-codegen` - Generates IR from the AST
- `rue-runtime` - Uses IR for runtime code generation
- The x86 emitter - Converts IR to actual machine code

## Example

```rust
use rue_ir::target::{MachineInstr, Register};

// Generate code to add two registers
let instr = MachineInstr::AddRR {
    dest: Register::Rax,
    src: Register::Rbx,
};
```

## Platform Support

Currently x86-64 specific. The design allows for potential future support of other architectures by defining alternative instruction sets.