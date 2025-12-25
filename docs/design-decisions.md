# Design Decisions

This document records significant architectural decisions made in the Rue compiler, along with context and rationale.

## ADR-001: Architecture-Specific Machine IR (X86Mir)

**Status:** Accepted

**Context:**
When lowering from a typed IR to machine code, compilers typically have a "machine IR" layer. There are two main approaches:
1. **Shared Machine IR**: A generic MIR that abstracts over all target architectures (like LLVM's MachineInstr or Rust's MIR)
2. **Architecture-Specific MIR**: Separate MIRs for each target (like Zig's approach with x86 MIR, ARM MIR, etc.)

**Decision:**
We chose architecture-specific MIRs (Option 2), following Zig's approach.

**Rationale:**
- Allows architecture-specific optimizations without abstraction overhead
- Instructions map directly to target ISA, making emit phase straightforward
- No need to design abstractions that work across vastly different architectures
- Simpler to implement and debug for a single target
- When adding new targets, each gets its own clean MIR without compromises

**Consequences:**
- Each new target requires implementing its own MIR types
- Some code duplication between targets (register allocator structure, etc.)
- Clear ownership: x86-64 backend owns all x86-64 decisions

---

## ADR-002: Dense Instruction Storage with Index References

**Status:** Accepted

**Context:**
IRs need to reference other instructions. Common approaches:
1. **Pointer-based**: Instructions reference each other via pointers/Rc/Arc
2. **Index-based**: Instructions stored in a Vec, referenced by index (u32)

**Decision:**
We use index-based references throughout (InstRef, AirRef, VReg).

**Rationale:**
- Cache-friendly: sequential memory access patterns
- Smaller references: u32 vs 8-byte pointer
- Trivial serialization (indices are stable)
- Matches Zig's ZIR approach, which is proven at scale
- No lifetime complexity from self-referential structures

**Consequences:**
- Must be careful about instruction ordering during construction
- Cannot easily delete instructions (would invalidate indices)
- Index validity is not statically checked (runtime panic on invalid access)

---

## ADR-003: Separate RIR and AIR Phases

**Status:** Accepted

**Context:**
Some compilers have a single IR that gets progressively annotated with type information. Others have distinct typed and untyped IRs.

**Decision:**
We have two distinct IRs:
- **RIR** (Rue IR): Untyped, produced by AstGen
- **AIR** (Analyzed IR): Typed, produced by Sema

**Rationale:**
- Clear separation of concerns: parsing vs type checking
- RIR can be inspected before type errors occur
- Different instruction sets appropriate to each phase
- Easier to reason about what information is available at each stage

**Consequences:**
- Two IR definitions to maintain
- Transformation pass (Sema) between them
- Some duplication of similar concepts (InstRef vs AirRef)

---

## ADR-004: No Incremental Compilation (Initially)

**Status:** Accepted

**Context:**
Incremental compilation can dramatically improve rebuild times but adds significant complexity (dependency tracking, caching, invalidation).

**Decision:**
Focus on fast from-scratch compilation first. No incremental compilation initially.

**Rationale:**
- Simpler architecture to start
- Forces us to keep the full pipeline fast
- Zig demonstrates that fast from-scratch compilation can be competitive
- Can add incrementality later without major restructuring

**Consequences:**
- Every change requires full recompilation
- Must keep compilation fast through other means (parallelism, efficient algorithms)

---

## ADR-005: Direct Machine Code Emission (No LLVM)

**Status:** Accepted

**Context:**
Many compilers use LLVM for code generation, benefiting from its optimizations and target support. Others emit machine code directly.

**Decision:**
Rue emits machine code directly, without LLVM.

**Rationale:**
- Full control over compilation speed
- No external dependencies
- Smaller binary size for the compiler itself
- Educational value: understand the full stack
- Can add LLVM backend later as an optional optimization tier

**Consequences:**
- Must implement our own register allocator
- Must implement our own instruction encoding
- Initially suboptimal code quality compared to LLVM
- Each new target requires significant work

---

## ADR-006: Virtual Registers with Linear Scan Allocation

**Status:** Accepted

**Context:**
Register allocation maps an infinite set of virtual registers to a finite set of physical registers. Approaches range from simple (linear scan) to complex (graph coloring, SSA-based).

**Decision:**
Use virtual registers in X86Mir with a simple linear scan allocator.

**Rationale:**
- Virtual registers simplify instruction selection (don't worry about register constraints)
- Linear scan is simple to implement and fast
- Good enough for initial implementation
- Can upgrade to more sophisticated allocation later

**Consequences:**
- May spill more than necessary (can improve later)
- Simple implementation is easy to debug
- Clear separation between instruction selection and register allocation

---

## ADR-007: Minimal ELF Output

**Status:** Accepted

**Context:**
Executables need to be wrapped in a format the OS can load. Options include raw binary, ELF, Mach-O, PE, etc.

**Decision:**
Generate minimal static ELF executables for Linux x86-64.

**Rationale:**
- ELF is the standard on Linux
- Static linking avoids runtime dependencies
- Minimal headers = smaller output, faster generation
- Direct syscalls avoid libc dependency

**Consequences:**
- Linux-only initially
- No dynamic linking support yet
- No debug info in output (yet)

---

## ADR-008: TOML-Based Test Specifications

**Status:** Accepted

**Context:**
Compiler tests need to verify behavior across many inputs. Approaches include:
- Test functions in code
- External test files with conventions
- Structured test specifications

**Decision:**
Use TOML files with structured test cases in `rue-spec/cases/`.

**Rationale:**
- Tests are data, not code
- Easy to add new tests without touching Rust
- Clear structure: source, expected output, expected errors
- Supports golden tests naturally
- Human-readable and version-control friendly

**Consequences:**
- Need a test runner (rue-spec) to execute them
- Tests are outside the normal `cargo test` flow
- Must keep test format and runner in sync

---

## ADR-009: Language Design Philosophy

**Status:** Accepted

**Context:**
Programming languages occupy different points in the abstraction/control tradeoff:
- **Low-level** (C, Zig): Manual memory management, maximum control
- **Systems** (Rust): Memory safety with ownership, zero-cost abstractions
- **Managed** (Go, Java): Garbage collection, simpler mental model

**Decision:**
Rue aims to be higher-level than Rust/Zig but lower-level than Go. Memory safety by default, but no garbage collector.

**Influences:**
- **Hylo** (formerly Val): Mutable value semantics, memory safety without GC
- **Swift**: Ergonomic syntax, value types, reference counting where needed
- **Rust**: Ownership concepts, zero-cost abstractions, expression-based syntax

**Rationale:**
- GC-free enables predictable performance and embedded use
- Memory safety by default reduces bugs without runtime overhead
- Higher-level than Rust means less annotation burden on the programmer
- Mutable value semantics (à la Hylo) may provide safety with less complexity than Rust's borrow checker

**Consequences:**
- Must design a memory safety model (ownership, borrowing, or value semantics)
- Syntax will feel familiar to Rust/Swift programmers
- May need to make different tradeoffs than Rust for ergonomics

---

## ADR-010: Rust-Like Syntax (Initially)

**Status:** Accepted

**Context:**
Language syntax affects learnability, tooling, and migration paths.

**Decision:**
Start with a subset of Rust syntax. May evolve independently later.

**Rationale:**
- Familiar to target audience (systems programmers)
- Existing Rust tooling (syntax highlighting, etc.) works initially
- Allows focusing on semantics before syntax bikeshedding
- Clear migration path for Rust users exploring Rue

**Current syntax:**
```rue
fn main() -> i32 {
    42
}
```

**Consequences:**
- Parser follows Rust conventions
- May diverge from Rust syntax as language evolves
- Users may expect Rust semantics where Rue differs

---

## ADR-011: Uniform 8-Byte Stack Slots for All Values

**Status:** Accepted (temporary simplification)

**Context:**
When storing values on the stack (local variables, struct fields, function parameters), we need to decide on memory layout. Options include:

1. **Type-aware layout**: Each type uses its natural size (i8 = 1 byte, i16 = 2 bytes, i32 = 4 bytes, i64 = 8 bytes) with appropriate alignment padding
2. **Uniform slots**: Every value uses a fixed-size slot (e.g., 8 bytes)
3. **C-compatible layout**: Follow the System V AMD64 ABI for struct layout (required for FFI)

**Decision:**
Use uniform 8-byte slots for all scalar values and struct fields.

**Rationale:**
- Dramatically simplifies implementation: no alignment calculations, no padding logic
- Correct for x86-64: smaller values are sign/zero-extended to 64-bit registers naturally
- Stack slot arithmetic is trivial: `offset = -(slot + 1) * 8`
- Struct field access uses simple indexing: `field_offset = base_slot + field_index`
- Allows us to focus on language semantics before optimizing memory layout

**Example:**
```rue
struct Point { x: i8, y: i8 }
```
- Current layout: 16 bytes (2 slots × 8 bytes)
- Optimal layout: 2 bytes (no padding) or 2 bytes with 6 bytes padding for alignment
- C-compatible layout: 2 bytes

**Consequences:**
- Wastes memory for small types (i8 uses 8 bytes instead of 1)
- Struct sizes are larger than necessary
- **Not ABI-compatible with C** (cannot do FFI with C structs)
- Will need to revisit for FFI support or memory-constrained use cases

**Future Work:**
When implementing FFI or optimizing memory usage, we should:
1. Add `size_of()` and `align_of()` methods to `Type`
2. Calculate actual byte offsets in struct layout (with padding)
3. Add type-appropriate load/store instructions (movb, movw, movl, movq)
4. Consider `#[repr(C)]` attribute for C-compatible layout

---

## ADR-012: Internal Compiler Error Handling

**Status:** Accepted

**Context:**
Compilers encounter two kinds of errors:
1. **User errors**: Invalid syntax, type mismatches, undefined variables - errors in the user's code
2. **Internal errors**: Invalid invariants, out-of-bounds indices, unreachable code paths - bugs in the compiler itself

How should we handle internal errors? Options include:
1. **Return `Result<_, InternalError>`**: Propagate through the error system like user errors
2. **Panic with `expect()`/`unwrap()`**: Fail fast with a stack trace
3. **Hybrid**: Use Result where convenient, panic where not

**Decision:**
Use `panic!` (via `expect()` or `unreachable!()`) for internal compiler errors, not `Result`.

**Rationale:**
- **Semantic clarity**: A panic says "this is a compiler bug" while `Result` implies recoverable/expected errors
- **Fail fast**: Don't continue executing with corrupted state; a stack trace helps debug the compiler
- **No viral `?`**: Avoid polluting every function signature to propagate errors that should never occur
- **Simplicity**: Methods like `format_type_name()` or `is_type_copy()` shouldn't need to return `Result`

Internal invariants (like valid `StructId` indices) are enforced by construction:
- `StructId` values are only created when registering a struct in `struct_defs`
- `Type::Struct(id)` values are only created through the type resolution path
- If an ID is invalid, it means the compiler has a bug, not the user's code

**Pattern:**
```rust
// Good: Clear panic message for compiler bugs
fn get_struct_def(&self, id: StructId) -> &StructDef {
    &self.struct_defs[id.0 as usize]  // Panics with index out of bounds if bug
}

// Also acceptable: Custom panic message
fn get_struct_def(&self, id: StructId) -> &StructDef {
    self.struct_defs.get(id.0 as usize)
        .expect("ICE: invalid StructId")
}

// Avoid: Treating internal errors as recoverable
fn get_struct_def(&self, id: StructId, span: Span) -> Result<&StructDef, CompileError> {
    self.struct_defs.get(id.0 as usize)
        .ok_or_else(|| CompileError::new(ErrorKind::InternalError(...), span))
}
```

**When to use `ErrorKind::InternalError`:**
Reserve `InternalError` for cases where:
- You're already in a `Result`-returning context
- You have a meaningful source span to report
- The error message benefits from the standard error formatting

Example: "FnDecl should not appear in expression context" - this is an internal error that happens during analysis where we have a span and are already returning `Result`.

**Consequences:**
- Compiler panics on internal bugs (desirable - fail fast)
- Stack traces help developers debug the compiler
- User-facing code paths remain clean and focused on user errors
- `ErrorKind::InternalError` is rarely used, only in specific contexts

---

## Future Considerations

Decisions we've deferred or are still considering:

- **Parallelism**: How to parallelize lexing/parsing (Zig lexes in parallel)
- **Error Recovery**: How much to recover after errors to report multiple issues
- **Optimization Passes**: When to add, what granularity (per-IR vs peephole)
- **Debug Info**: DWARF generation for debugger support
- **Multiple Targets**: ARM64, RISC-V, WebAssembly
- **Proper Struct Layout**: C-compatible struct layout for FFI (see ADR-011)
