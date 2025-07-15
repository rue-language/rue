# Code Generation Implementation Notes

## Architecture Overview

The code generation system follows a three-stage pipeline:

1. **Code Generation**: AST → x86-64 Instructions
2. **Assembly**: Instructions → Machine Code  
3. **ELF Generation**: Machine Code → Executable

## Code Generation Stage

### Instruction Set Design

```rust
#[derive(Debug, Clone)]
pub enum Instruction {
    // Stack operations
    Push(Operand),
    Pop(Operand),
    
    // Arithmetic
    Add(Operand, Operand),
    Sub(Operand, Operand), 
    Mul(Operand),
    Div(Operand),
    
    // Comparison and control flow
    Cmp(Operand, Operand),
    Jmp(String), Je(String), Jne(String), Jle(String),
    Call(String), Ret,
    
    // Data movement and system
    Mov(Operand, Operand),
    Syscall,
    
    // Labels (pseudo-instructions)
    Label(String),
}
```

### Expression Evaluation Strategy

Uses stack-based evaluation to handle arbitrary expression complexity:

1. **Binary Operations**: 
   - Evaluate left operand → push to stack
   - Evaluate right operand → leave in rax  
   - Pop left operand to rbx
   - Perform operation (rbx op rax → rax)

2. **Function Calls**:
   - Evaluate arguments right-to-left
   - Place first argument in rdi (System V ABI)
   - Generate call instruction with symbol

3. **Variable Access**:
   - Track variables in stack offset map
   - Generate memory operands with rbp-relative addressing

### Function Code Generation

```rust
// Function prologue
push rbp
mov rbp, rsp

// Parameter handling (first parameter goes to stack)
mov [rbp-8], rdi  

// Function body generation
// ... statements and expressions ...

// Function epilogue  
mov rsp, rbp
pop rbp
ret
```

## Assembly Stage

### Two-Pass Assembly Process

**Pass 1: Symbol Collection**
- Scan all instructions
- Record label positions
- Calculate instruction sizes
- Build symbol table

**Pass 2: Code Emission** 
- Emit machine code for each instruction
- Add relocations for unresolved symbols
- Resolve relocations using symbol table

### Critical Bug Fix: Instruction Sizing

The most critical bug was in `instruction_size()` calculation:

```rust
// BEFORE (incorrect - caused segfaults)
Instruction::Push(_) => 2,  // Wrong! push reg is 1 byte
Instruction::Pop(_) => 2,   // Wrong! pop reg is 1 byte

// AFTER (correct)  
Instruction::Push(_) => 1,  // push reg = 50+r
Instruction::Pop(_) => 1,   // pop reg = 58+r
```

### x86-64 Instruction Encoding

Key instruction encodings implemented:

```assembly
; Move immediate to register (10 bytes)
mov rax, 0x123456789abcdef0  ; 48 b8 f0 de bc 9a 78 56 34 12

; Move register to register (3 bytes)  
mov rax, rbx                 ; 48 89 d8

; Stack-relative memory access (4 bytes)
mov rax, [rbp-8]            ; 48 8b 45 f8
mov [rbp-8], rax            ; 48 89 45 f8

; Arithmetic operations (3 bytes)
add rax, rbx                ; 48 01 d8
sub rax, rbx                ; 48 29 d8

; Comparison (3-4 bytes)
cmp rax, rbx                ; 48 39 d8
cmp rax, 0                  ; 48 83 f8 00

; Control flow (5-6 bytes)
jmp label                   ; e9 [rel32]
je label                    ; 0f 84 [rel32]
call label                  ; e8 [rel32]
```

### Relocation Process

For relative jumps and calls:

1. **Relocation Recording**: Store offset where address should go
2. **Address Calculation**: `target_addr - (current_addr + 4)`  
3. **Patching**: Write 32-bit relative offset to instruction

## ELF Generation Stage

### ELF Header Structure

```rust
// ELF identification
&[0x7f, 0x45, 0x4c, 0x46]  // ELF magic
0x02                        // 64-bit
0x01                        // Little endian  
0x01                        // ELF version
0x00                        // System V ABI

// ELF header fields
e_type:      2              // ET_EXEC (executable)
e_machine:   0x3e           // EM_X86_64
e_version:   1              // Current version
e_entry:     0x400078       // Entry point (_start)
e_phoff:     64             // Program header offset
e_ehsize:    64             // ELF header size
e_phentsize: 56             // Program header entry size
e_phnum:     1              // Number of program headers
```

### Program Header (PT_LOAD)

```rust
p_type:   1                 // PT_LOAD
p_flags:  5                 // PF_R | PF_X (readable + executable)
p_offset: 0                 // Offset in file
p_vaddr:  0x400000          // Virtual address
p_paddr:  0x400000          // Physical address  
p_filesz: total_size        // Size in file
p_memsz:  total_size        // Size in memory
p_align:  0x1000            // Page alignment
```

### Memory Layout

```
Virtual Address    Content
0x400000          ELF Header (64 bytes)
0x400040          Program Header (56 bytes)  
0x400078          Machine Code (variable size)
```

## Key Implementation Details

### Stack Frame Management

Each function maintains its own stack frame:

```rust
// Variable storage
self.stack_offset -= 8;  // Allocate 8 bytes
self.variables.insert(name, self.stack_offset);
```

### System V ABI Compliance

- **Parameter Passing**: First parameter in rdi
- **Return Values**: Result in rax
- **Stack Alignment**: Maintained by prologue/epilogue
- **Callee-saved Registers**: rbp properly saved/restored

### Error Handling Strategy

```rust
pub struct CodegenError {
    pub message: String,
}
```

Comprehensive error reporting for:
- Undefined variables
- Undefined symbols  
- Unsupported operations
- Assembly encoding failures

## Testing Strategy

### Unit Tests
- Expression code generation
- Function code generation  
- Control flow structures
- Assembler instruction encoding

### Integration Tests
- End-to-end compilation
- ELF format validation
- Executable execution
- Factorial recursion test

### Debugging Tools Used
- **gdb**: Stepping through generated code
- **hexdump**: Examining binary output
- **objdump**: Disassembling machine code  
- **readelf**: Validating ELF structure

## Performance Characteristics

### Code Generation Speed
- Single-pass AST traversal
- O(n) instruction generation
- Minimal memory allocation

### Generated Code Quality
- Stack-based evaluation (not optimized)
- No register allocation optimization
- Direct instruction translation
- Room for significant optimization

## Future Optimization Opportunities

1. **Register Allocation**: Move from stack-based to register-based evaluation
2. **Peephole Optimization**: Remove redundant mov instructions  
3. **Dead Code Elimination**: Remove unused computations
4. **Constant Folding**: Compile-time arithmetic evaluation
5. **Tail Call Optimization**: Optimize recursive function calls