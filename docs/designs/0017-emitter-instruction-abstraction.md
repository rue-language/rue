---
id: 0017
title: Emitter Instruction Abstraction
status: proposal
tags: [codegen, refactoring]
created: 2025-12-27
accepted:
implemented:
spec-sections: []
---

# ADR-0017: Emitter Instruction Abstraction

## Status

Proposal

## Summary

Refactor the x86-64 and aarch64 emitters to use an explicit instruction representation that captures both machine code bytes and assembly text, enabling accurate `--emit asm` output that includes prologue/epilogue.

## Context

The current `--emit asm` output is misleading. It calls `format_assembly()` on the MIR, which only shows MIR instructions. But the actual emitter generates additional code:

1. **Prologue**: `push rbp`, `mov rbp, rsp`, callee-saved register saves, stack allocation, parameter spills
2. **Epilogue augmentation**: When the emitter sees `mov rsp, rbp`, it inserts callee-saved restoration
3. **Implicit instruction expansion**: Some MIR instructions expand to multiple machine instructions

**What `--emit asm` shows:**
```asm
main:
    mov rax, 42
    ret
```

**What actually runs:**
```asm
main:
    push rbp
    mov rbp, rsp
    sub rsp, 16
    mov rax, 42
    mov rsp, rbp
    pop rbp
    ret
```

This gap makes debugging stack/ABI issues difficult.

### Why This Is Hard to Fix

The difficulty reveals an architectural issue: **the emitter conflates instruction selection with encoding**. Each `emit_*` method directly pushes bytes to a buffer without leaving any record of what instruction was emitted.

There are ~72 emit methods in x86-64 and ~54 in aarch64. Adding assembly text recording to each would:
- Duplicate the instruction description (once in bytes, once in text)
- Risk drift between bytes and text
- Be error-prone and hard to maintain

## Decision

Introduce an explicit `EmittedInst` type that represents a single emitted instruction with both its bytes and text representation. The emitter will produce a sequence of these, which can then be serialized to either bytes or assembly text.

### Core Types

```rust
/// A single emitted machine instruction.
pub struct EmittedInst {
    /// The machine code bytes for this instruction.
    pub bytes: Vec<u8>,
    /// Human-readable assembly text (e.g., "mov rax, rbx").
    pub asm: String,
    /// Byte offset from start of function (filled in during finalization).
    pub offset: usize,
}

/// Result of emitting a function.
pub struct EmittedFunction {
    /// All emitted instructions in order.
    pub instructions: Vec<EmittedInst>,
    /// Relocations that need to be applied.
    pub relocations: Vec<EmittedRelocation>,
    /// Label name to instruction index mapping (for fixups).
    pub labels: HashMap<LabelId, usize>,
}

impl EmittedFunction {
    /// Get the raw machine code bytes.
    pub fn to_bytes(&self) -> Vec<u8> {
        self.instructions.iter()
            .flat_map(|inst| inst.bytes.iter().copied())
            .collect()
    }

    /// Get the assembly text representation.
    pub fn to_asm(&self) -> String {
        let mut result = String::new();
        for inst in &self.instructions {
            if inst.bytes.is_empty() {
                // Label or comment
                result.push_str(&inst.asm);
            } else {
                result.push_str(&format!("{:4x}:   {}", inst.offset, inst.asm));
            }
            result.push('\n');
        }
        result
    }
}
```

### Emit Method Pattern

Each `emit_*` method will return or record an `EmittedInst`:

```rust
// Before
fn emit_mov_rr(&mut self, dst: Reg, src: Reg) {
    let rex = 0x48 | ...;
    self.code.push(rex);
    self.code.push(0x89);
    self.code.push(modrm);
}

// After
fn emit_mov_rr(&mut self, dst: Reg, src: Reg) {
    let rex = 0x48 | ...;
    let modrm = ...;
    self.emit(EmittedInst {
        bytes: vec![rex, 0x89, modrm],
        asm: format!("mov {}, {}", dst, src),
        offset: 0, // filled in during finalization
    });
}
```

### Helper Method

Add a helper to reduce boilerplate:

```rust
impl Emitter {
    fn emit(&mut self, inst: EmittedInst) {
        self.instructions.push(inst);
    }

    fn emit_inst(&mut self, bytes: impl Into<Vec<u8>>, asm: impl Into<String>) {
        self.instructions.push(EmittedInst {
            bytes: bytes.into(),
            asm: asm.into(),
            offset: 0,
        });
    }

    fn emit_label(&mut self, name: impl Into<String>) {
        self.instructions.push(EmittedInst {
            bytes: vec![],
            asm: format!("{}:", name.into()),
            offset: 0,
        });
    }

    fn emit_comment(&mut self, text: impl Into<String>) {
        self.instructions.push(EmittedInst {
            bytes: vec![],
            asm: format!("; {}", text.into()),
            offset: 0,
        });
    }
}
```

### Finalization

After all instructions are emitted, compute offsets and apply fixups:

```rust
impl EmittedFunction {
    fn finalize(&mut self) -> CompileResult<()> {
        // Compute byte offsets
        let mut offset = 0;
        for inst in &mut self.instructions {
            inst.offset = offset;
            offset += inst.bytes.len();
        }

        // Apply jump fixups using instruction indices -> byte offsets
        self.apply_fixups()?;

        Ok(())
    }
}
```

### Prologue/Epilogue

The prologue and epilogue become explicit sequences of `EmittedInst`:

```rust
fn emit_prologue(&mut self) {
    self.emit_comment("prologue");
    self.emit_inst([0x55], "push rbp");
    self.emit_inst([0x48, 0x89, 0xE5], "mov rbp, rsp");

    for &reg in &self.callee_saved {
        let bytes = self.encode_push(reg);
        self.emit_inst(bytes, format!("push {}", reg));
    }

    if stack_size > 0 {
        let bytes = self.encode_sub_rsp_imm(stack_size);
        self.emit_inst(bytes, format!("sub rsp, {}", stack_size));
    }
    // ... parameter saves
}
```

## Implementation Phases

- [x] **Phase 1: Core types and x86-64 refactor** - rue-hf6s (completed)
  - Add `EmittedInst` and `EmittedCode` types
  - Refactor x86-64 emitter to use new pattern
  - Update `--emit asm` to use `to_asm()`
  - Verify byte output is identical (regression test)

- [ ] **Phase 2: aarch64 refactor** - rue-4rzx (depends on Phase 1)
  - Apply same pattern to aarch64 emitter
  - Verify byte output is identical

- [ ] **Phase 3: Cleanup and optimization** - rue-h8r0 (depends on Phase 2)
  - Extract common helpers (REX building, ModR/M encoding)
  - Add offset adjustment helper for FP-relative accesses
  - Consider shared trait for cross-platform patterns

## Consequences

### Positive

- **Accurate `--emit asm`**: Output matches what actually executes, including prologue/epilogue
- **Single source of truth**: Each instruction is described once, with both bytes and text derived from the same emit call
- **Easier debugging**: Can correlate assembly lines with byte offsets
- **Foundation for future tools**: Disassembly, instruction-level profiling, binary diffing
- **Cleaner code**: Explicit instruction list vs implicit byte buffer
- **Testability**: Can assert on instruction sequences, not just final bytes

### Negative

- **Larger refactor**: ~70 methods in x86-64, ~54 in aarch64 need updating
- **Memory overhead**: Vec<EmittedInst> vs Vec<u8> uses more memory during compilation
- **String allocations**: Assembly text requires string formatting per instruction
- **Churn**: Significant changes to stable code

### Mitigations

- **Incremental migration**: Can be done method-by-method
- **Regression tests**: Existing tests verify byte output doesn't change
- **Memory**: Only during emit phase, which is fast; can optimize later if needed
- **Strings**: Only computed; could be lazy if profiling shows issues

## Open Questions

1. **Should asm text be optional?** Could use `Option<String>` and only populate when `--emit asm` is requested. Trades memory for slight complexity.

2. **Should we share types between backends?** `EmittedInst` could be in a shared crate. The backends would still have their own emit methods.

3. **Should labels be separate?** Currently mixing labels (0-byte "instructions") with real instructions. Could have `enum EmittedItem { Inst(EmittedInst), Label(String) }`.

4. **Fixup representation**: Currently fixups reference byte offsets. With instruction indices, we could have a cleaner model. Worth changing?

## Future Work

- **Instruction-level optimizations**: With explicit instruction list, could implement peephole optimizations (e.g., remove redundant moves)
- **Binary diffing**: Compare two compilations at instruction level
- **Size analysis**: Which functions generate the most code? Which patterns are expensive?
- **Alternative text formats**: Intel vs AT&T syntax, or custom annotations

## References

- Issue: rue-3dxp (`--emit asm should show actual emitted code including prologue/epilogue`)
- Current emit.rs files: `crates/rue-codegen/src/{x86_64,aarch64}/emit.rs`
