# Session 011: Register Spilling - Design Decisions

## Problem Statement

The Rue compiler currently fails compilation when programs require more than 11 virtual registers. While we extended from 8 to 11 registers by including R11-R15, complex programs still exceed this limit, particularly recursive functions and expressions with nested function calls.

## Core Issue Analysis

- **Available registers**: 11 physical x86-64 registers (Rbx, Rcx, Rdx, Rsi, Rdi, R8-R15, excluding RAX)
- **Register pressure patterns**:
  - Recursive functions (factorial): 10+ registers
  - Control flow (if/while): 4-9 registers  
  - Function calls with locals: 8+ registers
  - Multiple assignments: 8+ registers

## Solution Decision: Stack Spilling

### Why Stack Spilling Over Alternatives

**Considered alternatives:**
1. **SSA conversion first** - Would optimize usage but doesn't solve fundamental capacity limit
2. **Graph coloring allocation** - Complex, still bounded by physical registers
3. **Increase register count** - Limited by x86-64 architecture

**Stack spilling chosen because:**
- **Unlimited capacity** - Can handle arbitrarily complex programs
- **Unblocks all examples immediately** - Direct path to working compiler
- **Foundation for future optimizations** - SSA/graph coloring can layer on top
- **Minimal complexity** - Straightforward implementation vs alternatives

### Architecture Design

**Register allocation becomes a two-tier system:**
- **Tier 1**: Physical registers (fast access) - 11 registers
- **Tier 2**: Stack via push/pop (slower but unlimited)

**Key design decisions:**

1. **Push/Pop spilling**: Use native stack operations instead of memory moves
2. **Spill policy**: Least Recently Used (LRU) for simplicity
3. **Spill tracking**: HashMap<VReg, i32> tracks spilled registers
4. **Immediate restore**: Pop instructions generated when accessing spilled values

### Implementation Strategy

**Phase approach:**
1. **Extend allocator** - Add Location enum and stack slot management
2. **Update code generation** - Handle both register and stack locations
3. **Add spill/load logic** - Insert memory operations as needed
4. **Test and validate** - Ensure all disabled examples work

**Performance considerations:**
- Register pressure is typically localized (short-lived variables)
- Modern CPUs have excellent cache performance for stack access
- Can optimize hot paths later with better allocation algorithms

### Interface Design

The implementation maintains a simpler interface than originally planned:

```rust
struct RegisterAllocator {
    allocation: HashMap<VReg, Register>,
    spill_slots: HashMap<VReg, i32>,  // Track spilled VRegs
    available_registers: Vec<Register>,
    usage_order: Vec<VReg>,  // LRU tracking
}
```

Key methods:
- `allocate(vreg) -> Result<Register, CodegenError>` - Main allocation interface
- `spill_register() -> Register` - Evict LRU register, generate Push
- `is_spilled(vreg) -> bool` - Check if register was spilled

### Trade-offs Accepted

**Pros:**
- Solves register exhaustion completely
- Relatively simple to implement
- Provides foundation for calling conventions
- Performance adequate for current needs

**Cons:**
- Introduces memory traffic for spilled values
- Stack management complexity
- Not optimal allocation (but can improve later)

**Decision rationale:** Getting a working compiler is higher priority than optimal performance. This approach unblocks development while providing a solid foundation for future optimizations.