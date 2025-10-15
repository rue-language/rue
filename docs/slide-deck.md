---
marp: true
theme: default
paginate: true
headingDivider: 2
title: Rue and the Beauty of Compilers
description: A gentle backend/codegen tour using Rue
---

# Rue and the Beauty of Compilers

### Steve Klabnik — Rue Language Project

## What is Rue?

- Rue is a new language inspired by Rust, Zig, Hylo.

## Why Rue Exists

- Experimental Testbed — Explores cutting-edge compiler techniques in a minimal,
  understandable codebase
- IDE-First Design — Built with incremental compilation (Salsa) and concrete
  syntax trees for excellent IDE support
- Self-Contained Toolchain — Generates native ELF executables without external linkers or runtime dependencies
- Educational Value — Simple Rust-like syntax makes it ideal for learning
  compiler implementation
- Modern Architecture — Demonstrates SSA-form IR, optimization passes, and
  register allocation in a clean implementation
- Built with AI assistance — Testing Claude's ability to implement complex systems
- Buck2 Build System — Modern, parallel, incremental builds with excellent caching

## Specification-Driven Development

### The Spec Comes First

**Traditional Approach:**
1. Write code
2. Document what you built
3. Spec is often outdated

**Rue's Approach:**
1. Update formal specification (`docs/spec.md`)
2. Specification defines semantics
3. Implementation follows spec exactly
4. Tests validate spec compliance

## Specification-Driven Development

### Why This Matters

- **Single source of truth** — No ambiguity about language behavior
- **Alternative implementations** — Anyone can build a Rue compiler from the spec
- **Prevents feature creep** — Must justify changes at spec level
- **Educational clarity** — Learn the language from formal definition

> "The specification IS the language, not the implementation"


## A Tiny Example (Rue → MIR)

```rust
fn main() -> i32 {
    42
}
````

**MIR**

```text
// Function signatures:
// main() -> i32

fn main() -> i32:
  B0:
    t0 = 42_i32
    return t0
```

## A Tiny Example (MIR → ASM)

**Assembly**

```asm
main:
    pushq %rbp
    movq %rsp, %rbp
    subq $16, %rsp          # Allocate stack space for spills
.L86:
    movq $42, %r10          # Load constant to scratch register
    movq %r10, -16(%rbp)    # Spill to stack (allocator's "home slot")
    movq -16(%rbp), %r10    # Reload from stack
    movq %r10, %rax         # Move to return register
    movq %rbp, %rsp
    popq %rbp
    ret
```

* Shows the "spill everything" register allocator approach
* Every value gets a stack slot for safety and simplicity

## Compilers Are Just Translators... Right?

- You write code → hit build → binary.
- But compilers are far more: analysis, optimization, interpretation.
- They encode **meaning** into machine form.

> Think of a compiler as both engineer and poet.

## From Code to CPU

```
Source → Parser → HIR → MIR → Codegen → Binary

Source → Parser → CST → Semantic Analysis → HIR → MIR → PIR → Codegen → Binary
````

## From Code to CPU

### Front End

* Source - Raw Rue source code (.rue files) written by the programmer
* Parser - Converts text into a CST (Concrete Syntax Tree), performs syntax
validation, and provides error recovery for invalid code

## From Code to CPU

### "Middle end"

* HIR (High-level IR) - Type-checked representation with semantic information,
resolves names/scopes, validates function calls and type correctness
* MIR (Mid-level IR) - SSA-form representation with explicit control flow graphs,
enables optimization passes like dead code elimination and constant folding

## From Code to CPU

### Back end

* Codegen - Transforms MIR into x86-64 assembly instructions, performs register
allocation, and generates runtime integration code
* Binary - Final ELF executable for Linux x86-64, includes runtime support for
memory management and program startup

## Register Allocation: The Core Challenge

### The Problem
- CPUs have ~16 registers, programs have unlimited variables
- Which values go in registers? Which get "spilled" to memory?
- Wrong choices = 10x slower code

### Rue's Approach
- "Spill everything" - simple, correct, slow
- Every value gets a memory slot
- Load when needed, store after use
- Future: graph coloring or linear scan

## Instruction Selection: Many Roads to Rome

```rue
x + y * 2
```

### Option 1: Naive
```asm
movq y, %rax
imulq $2, %rax
addq x, %rax
```

### Option 2: LEA Magic
```asm
leaq (%rdi,%rsi,2), %rax  # One instruction!
```

* Modern CPUs have hundreds of instructions
* Choosing the right one is an art

## Calling Conventions: How Functions Talk

### The x86-64 System V ABI Rules
- First 6 arguments: RDI, RSI, RDX, RCX, R8, R9
- Return value: RAX
- Stack must be 16-byte aligned before CALL
- Some registers are "callee-saved" (must preserve)

### Why This Matters
- Get it wrong = mysterious crashes
- Every language on Linux follows these rules
- Enables C interop (if you had a linker!)

## Why Backend is Hard

### It's Not Just "Print Instructions"

**Correctness Challenges:**
- Undefined behavior lurks everywhere
- Stack alignment off by 8 bytes? Crash.
- Forget to save a register? Corruption.

**Performance Challenges:**
- Register pressure vs spilling
- Instruction scheduling for CPU pipelines
- Cache-friendly data layout

## Why Backend is Hard

### It's Not Just "Print Instructions"

**Debugging Challenges:**
- Errors manifest far from their cause
- Need to think in assembly AND high-level code
- Tool support is primitive (compared to frontend)

## Design Decisions: Simplicity First

### What Rue Does
✅ Direct instruction emission
✅ Simple "spill everything" allocator  
✅ Single-pass compilation
✅ Readable assembly output

### What Rue Doesn't (Yet)
❌ Register allocation optimization
❌ Instruction scheduling
❌ Peephole optimization
❌ SIMD vectorization

**Philosophy:** Get it working, make it clear, then make it fast

## War Story: The Missing Alignment

```asm
subq $24, %rsp  # Allocate 24 bytes
call some_func  # CRASH! Stack not 16-byte aligned
```

### The Bug
- Stack was 8-byte aligned, not 16
- Only crashed with certain functions
- Worked in debug, failed in release

### The Lesson
- Backend bugs are subtle
- Hardware has hidden requirements
- This is why we test everything!

## What This Means for You

### When you see...
- "Segmentation fault" → Often stack/memory issues
- Slow debug builds → No optimization passes
- `-O2` makes code faster → Register allocation + inlining
- Debugger shows weird values → Optimizations moved things

### Now you know why!

## What Happens Before `main`

### The Embedded Runtime

_start → __rue_main → main → exit syscall

• **Self-contained runtime** — Generated as raw x86-64 assembly, no external
dependencies
• **`_start`** — ELF entry point, sets up stack and calls runtime wrapper
• **`__rue_main`** — Detects CPU features, sets up signal handlers, initializes 
heap
• **Direct syscalls** — No libc, runtime makes Linux syscalls directly

## Linking: The Final Puzzle

### Direct Executable Generation

• Single-pass compilation — Source → HIR → MIR → Machine Code → ELF
• Embedded runtime — Runtime functions generated as raw assembly, combined with
user code
• Simple ELF writer — Creates minimal static executables (no relocations needed)
• Self-contained binary — One PT_LOAD segment at 0x400000, RWX permissions
• No external dependencies — No linker, no libc, just direct syscalls

## Making the Invisible Visible

### Rue's Glass-Box Compiler

Available outputs:

• Parse trees — CST preserving all syntax details (whitespace, comments)
• MIR dumps — --emit-mir shows SSA form with optimization passes
• Assembly — -S or --emit-asm outputs x86-64 assembly
• Diagnostics — Rich error messages with source locations and suggestions
• Debug logging — -v, -vv, -vvv or RUST_LOG for detailed traces

## Making the Invisible Visible (part 2)

IDE Integration:

• LSP server — Real-time diagnostics, semantic highlighting, completions
• VS Code extension — Full IDE experience with error squiggles

Debugging features:

• --log-format=tree — Hierarchical view of compilation phases
• --log-filter — Target specific compiler components
• Incremental compilation via Salsa — Cache inspection possible

This truly is a glass-box compiler designed for learning and debugging, with every
stage observable and IDE support for interactive exploration.

## Example: Rich Diagnostics

```rust
fn main() -> i32 {
    let x = 42  // Missing semicolon
    x
}
```

```
error: expected `;` after statement
  --> test.rue:2:15
   |
2  |     let x = 42
   |               ^ expected `;` here
```

## Lessons Learned

* Codegen is not 'print instructions', it's preserving meaning through successive translations
* HIR, MIR, ELF are different views of truth
* Backend work = systems engineering + language semantics

## Why Compilers Are Beautiful

* They bridge abstract logic and raw metal.
* Translators, optimizers, *and* philosophers.
* Understanding one changes how you write all software.

> Compilers are poets of computation.

## Performance Tracking

* GitHub Actions CI runs benchmarks on every commit
* Tracks compilation speed, binary size, and runtime performance
* Historical data stored for regression detection
* Performance tests prevent slow compiler regressions
* Buck2 caching metrics ensure build efficiency stays high

## Rue and the Future

* Explicit semantics → predictable codegen.
* Transparent runtime + linker for learning & reliability.
* A language **and** a laboratory for compiler ideas.

## Demo / Q&A

```bash
# Build and run
$ ./buck2 run //crates/rue:rue examples/basic/simple.rue
$ ./examples/basic/simple ; echo $?
42

# View compilation stages
$ ./buck2 run //crates/rue:rue -- --emit-mir examples/basic/simple.rue
$ ./buck2 run //crates/rue:rue -- -S examples/basic/simple.rue

# Debug compilation
$ ./buck2 run //crates/rue:rue -- -vv examples/basic/simple.rue
```