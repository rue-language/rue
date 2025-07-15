# TargetIR Design Decisions

## Context
Rue currently generates x86-64 assembly directly from the AST. This approach limits our ability to target multiple architectures and makes future optimizations difficult. We decided to introduce a platform-independent intermediate representation (IR) for code generation.

## Architecture Vision
The long-term architecture will be:
**AST** → **MidIR** (SSA, optimizations) → **TargetIR** (simple, codegen) → **Assembly**

We're implementing TargetIR first as the foundation for multi-target support.

## Design Decisions

### 1. IR Naming: "TargetIR"
**Decision**: Call this intermediate representation "TargetIR"
**Alternatives Considered**:
- RueCodegenIR
- RueLIR (Low-level IR)
- RueTargetIR

**Rationale**: 
- Clear and concise
- Indicates its purpose (targeting specific architectures)
- Leaves room for future IRs (MidIR, etc.)
- Follows established naming patterns

### 2. Design Constraints: Simplicity First
**Decision**: Prioritize simplicity over sophistication
**Priority Order**:
1. Simplicity - Keep it as simple as possible
2. Performance - Don't slow down compilation
3. Multi-target ready - Design for future expansion but don't implement multiple targets yet

**Rationale**:
- TargetIR is a foundation; we can add complexity later
- Small project can afford aggressive changes
- Performance matters for developer experience
- Multi-target is future goal, not immediate need

### 3. Virtual Registers: Simple Numbering
**Decision**: Use `VReg(u32)` for virtual registers
**Alternatives Considered**:
- Named virtuals: `VReg { name: String, version: u32 }`
- Typed virtuals: `VReg { id: u32, ty: Type }`

**Rationale**:
- Simplicity is top priority
- Debugging info will be handled by higher-level IRs (MidIR)
- DWARF generation will use source-level information
- Types can be added later via separate tables if needed
- Easy to extend: `VReg(u32)` → `VReg { id: u32, ty: Type }`

### 4. Control Flow: Labels and Jumps
**Decision**: Use linear instruction sequence with labels and jumps
**Alternatives Considered**:
- Basic block structure
- Hybrid approach with control flow metadata

**Rationale**:
- Optimizations will happen in MidIR, not TargetIR
- TargetIR is purely for codegen, no analysis passes needed
- Labels/jumps are close to assembly representation
- Already proven to work in current codebase
- Simple to generate and consume

### 5. Instruction Design: Unified Operations
**Decision**: Use simplified instruction set with unified operands
**Design**:
```rust
Copy { dest: VReg, src: Value },
BinaryOp { dest: VReg, lhs: Value, rhs: Value, op: BinOp },
```
where `Value = VReg(u32) | Immediate(i64)`

**Alternatives Considered**:
- Separate instructions per operation (Add, Sub, Mul, etc.)
- Separate LoadImm and Move instructions

**Rationale**:
- Fewer instruction types = simpler code
- Unified Value type eliminates duplicate logic
- Still maps cleanly to assembly
- Easier to add new operations (just extend BinOp enum)

### 6. Data Structure: Flat Instruction Enum
**Decision**: Use flat enum structure for instructions
**Alternatives Considered**:
- Grouped enums (DataOp, ControlOp, CallOp)
- Separate types for different instruction categories

**Rationale**:
- Simplicity priority
- Easier pattern matching in backends
- Less indirection
- Consistent with current codebase style

### 7. Implementation Strategy: Big Bang Replacement
**Decision**: Replace entire current Instruction enum at once
**Alternatives Considered**:
- Side-by-side implementation (new crate)
- Gradual migration (one instruction type at a time)

**Rationale**:
- Small project allows aggressive changes
- Cleaner than managing two systems
- Avoids partial migration complexity
- Fast feedback on design decisions

## Missing Operations Strategy
**Decision**: Don't implement missing comparison operators (==, !=, <, >=) in TargetIR initially
**Rationale**: Implement them when we add the language features, not as part of TargetIR work

## Type System Strategy
**Decision**: No types in initial TargetIR implementation
**Future Extension Plan**: 
- Option 1: Extend VReg: `VReg { id: u32, ty: Type }`
- Option 2: Separate type table: `HashMap<VReg, Type>`
- Option 3: Typed instructions: Add type fields to operations

**Rationale**: Easy to add later, unnecessary complexity now

## Success Criteria
1. All existing tests pass with new TargetIR
2. No performance regression in compilation time
3. Generated executables behave identically
4. Code is simpler and more maintainable
5. Foundation is ready for future multi-target support