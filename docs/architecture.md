# Rue Compiler Architecture

This document describes the internal architecture of the Rue compiler.

## Overview

Rue compiles source code through a series of intermediate representations (IRs), each serving a specific purpose in the compilation pipeline:

```
Source Code
    │
    ▼
┌─────────┐
│  Lexer  │  rue-lexer: Tokenizes source into tokens
└────┬────┘
     │
     ▼
┌─────────┐
│ Parser  │  rue-parser: Builds AST from tokens
└────┬────┘
     │
     ▼
┌─────────┐
│ AstGen  │  rue-rir: Lowers AST to RIR (untyped IR)
└────┬────┘
     │
     ▼
┌─────────┐
│  Sema   │  rue-air: Semantic analysis, produces AIR (typed IR)
└────┬────┘
     │
     ▼
┌─────────┐
│  Lower  │  rue-codegen: Lowers AIR to X86Mir (machine IR)
└────┬────┘
     │
     ▼
┌─────────┐
│RegAlloc │  rue-codegen: Maps virtual registers to physical
└────┬────┘
     │
     ▼
┌─────────┐
│  Emit   │  rue-codegen: Emits machine code bytes
└────┬────┘
     │
     ▼
┌─────────┐
│ Object  │  rue-linker: Creates relocatable object file
└────┬────┘
     │
     ▼
┌─────────┐
│  Link   │  rue-linker: Links objects into ELF executable
└─────────┘
```

## Crate Responsibilities

### `rue-lexer`
Converts source text into a stream of tokens. Each token carries:
- Kind (keyword, identifier, literal, punctuation)
- Span (start/end byte offsets for error reporting)

### `rue-parser`
Builds an Abstract Syntax Tree (AST) from tokens. The AST represents the syntactic structure but doesn't resolve names or types.

### `rue-rir` (Rue Intermediate Representation)
**Purpose:** First IR after parsing, still untyped.

The RIR is inspired by Zig's ZIR. It linearizes the AST into a dense array of instructions referenced by index. This representation is:
- Untyped (type resolution happens later)
- Per-file (no cross-file analysis yet)
- Dense (instructions stored in `Vec`, referenced by `InstRef`)

Key types:
- `Rir` - Container for all instructions
- `InstRef` - Index into the instruction array
- `InstData` - The actual instruction payload

### `rue-air` (Analyzed Intermediate Representation)
**Purpose:** Typed IR after semantic analysis.

AIR is the result of semantic analysis (sema). It:
- Resolves all types
- Validates type correctness
- Prepares for code generation

Key types:
- `Air` - Container for typed instructions
- `AirRef` - Index into instruction array
- `AirInstData` - Typed instruction variants (Const, Ret, etc.)
- `Type` - Type information (I32, etc.)

### `rue-codegen`
**Purpose:** Generate machine code from AIR.

The codegen uses a multi-phase approach:

1. **Lower** (`x86_64/lower.rs`): Converts AIR to X86Mir with virtual registers
2. **RegAlloc** (`x86_64/regalloc.rs`): Maps virtual registers to physical x86-64 registers
3. **Emit** (`x86_64/emit.rs`): Produces actual machine code bytes

#### X86Mir (Machine IR)
X86Mir is architecture-specific, closely matching x86-64 instructions but using virtual registers. This design (inspired by Zig) allows:
- Architecture-specific optimizations
- Clean separation between instruction selection and register allocation
- Future support for other architectures (ARM, RISC-V) with their own MIRs

Key types:
- `VReg` - Virtual register (infinite supply)
- `Reg` - Physical x86-64 register (rax, rdi, r10, etc.)
- `Operand` - Virtual register, physical register, or immediate
- `X86Inst` - Individual machine instruction

### `rue-linker`
A minimal linker for the Rue compiler. Handles:
- **Object file creation** (`ObjectBuilder`): Creates ELF64 relocatable object files (.o) from machine code
- **Object file parsing** (`ObjectFile`): Reads ELF64 relocatable objects
- **Linking** (`Linker`): Combines object files, resolves symbols, applies relocations, and produces a final ELF64 executable

The linker supports standard x86-64 relocation types (PC32, PLT32, Abs64, Abs32, Abs32S) and handles symbol resolution including weak symbols. This architecture enables future multi-file compilation and linking with external libraries.

### `rue-compiler`
Orchestrates the full pipeline. Provides:
- `compile(source) -> ELF bytes` - Full compilation
- `compile_frontend(source) -> CompileState` - Frontend compilation (through CFG) for debugging

### `rue-error`
Shared error types and reporting infrastructure.

### `rue-span`
Source location tracking (`Span` type) used throughout for error messages.

### `rue-intern`
String interning for identifiers. Reduces memory usage and enables fast equality comparisons.

### `rue`
The CLI binary. Supports:
- Normal compilation: `rue source.rue [output]`
- IR dumps: `--dump-rir`, `--dump-air`, `--dump-mir`

### `rue-spec`
Test harness for specification tests. Runs `.toml` test cases that verify:
- Exit codes
- Compilation failures
- Golden tests for IR output and error messages

## Data Flow Example

For `fn main() -> i32 { 42 }`:

**Tokens:**
```
fn, main, (, ), ->, i32, {, 42, }
```

**AST:**
```
Function {
  name: "main",
  return_type: "i32",
  body: Expr::Integer(42)
}
```

**RIR:**
```
%0 = const 42
%1 = fn main() -> i32 { %0 }
```

**AIR:**
```
%0 : i32 = const 42
%1 : i32 = ret %0
```

**X86Mir (before regalloc):**
```
mov v0, 42
mov rdi, v0
mov rax, 60
syscall
```

**X86Mir (after regalloc):**
```
mov r10, 42
mov rdi, r10
mov rax, 60
syscall
```

**Machine Code:**
```
41 BA 2A 00 00 00    mov r10d, 42
4C 89 D7             mov rdi, r10
B8 3C 00 00 00       mov eax, 60
0F 05                syscall
```

## Design Principles

1. **Dense Instruction Storage**: Instructions are stored in vectors and referenced by index (u32). This is cache-friendly and matches Zig's approach.

2. **Architecture-Specific MIR**: Rather than a generic "machine IR", each target architecture has its own MIR (X86Mir, future ArmMir, etc.). This allows architecture-specific optimizations without abstraction overhead.

3. **Explicit Phases**: Each transformation is explicit and inspectable. The `--dump-*` flags let you see the IR at each stage.

4. **Minimal Runtime**: Rue currently produces standalone executables with no runtime dependencies. Exit is via direct syscall.
