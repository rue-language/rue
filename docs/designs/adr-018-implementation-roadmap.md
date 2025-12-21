# ADR-018: Type System Implementation Roadmap

## Status

Proposed

## Overview

This document outlines the implementation sequence for evolving Rue's type system as described in ADRs 013-017. The ordering reflects dependencies between features.

## Implementation Phases

```
┌─────────────────────────────────────────────────────────────────┐
│                         PHASE 1                                  │
│                    Ownership Modes                               │
│         value | move | linear | rc type declarations            │
│                                                                  │
│  Enables: Memory safety, use-after-move detection,              │
│           linear must-use checking, reference counting          │
└─────────────────────────────────────────────────────────────────┘
                              │
                              ▼
┌─────────────────────────────────────────────────────────────────┐
│                         PHASE 2                                  │
│               Mutable Value Semantics                            │
│              inout parameters + exclusivity                      │
│                                                                  │
│  Requires: Ownership modes (to know how types behave)           │
│  Enables: Safe mutation without borrow checker, no lifetimes    │
└─────────────────────────────────────────────────────────────────┘
                              │
                              ▼
┌─────────────────────────────────────────────────────────────────┐
│                         PHASE 3                                  │
│                  Comptime Evaluation                             │
│         comptime keyword + types as values + interpreter         │
│                                                                  │
│  Requires: Stable value semantics to evaluate                   │
│  Enables: Generics, type-level computation, specialization      │
└─────────────────────────────────────────────────────────────────┘
                              │
                              ▼
┌─────────────────────────────────────────────────────────────────┐
│                         PHASE 4                                  │
│                 Generators & Iteration                           │
│              fn* syntax + yield + state machines                 │
│                                                                  │
│  Requires: Comptime (for generic iterators)                     │
│  Enables: Lazy iteration, foundation for async                  │
└─────────────────────────────────────────────────────────────────┘
```

## Phase 1: Ownership Modes

**Goal:** Implement `value`, `move`, `linear`, `rc` type modifiers.

### Step 1.1: Parser + AST (1-2 weeks)
- Add `OwnershipMode` enum to parser
- Update struct declarations to include mode
- Parse `value struct`, `move struct`, `linear struct`, `rc struct`
- Update AST pretty-printing

### Step 1.2: RIR + Type Representation
- Add mode to `StructDecl` in RIR
- Store mode in `StructDef` in AIR
- Add `ownership_mode()` method to `Type`

### Step 1.3: Move Semantics
- Track `VarState` (Valid/Moved) in sema
- Detect use-after-move for `move` types
- Good error messages with span info

### Step 1.4: Linear Checking
- Check all linear values consumed at scope exit
- Track consumption via:
  - Explicit drop calls
  - Passing to consuming functions
  - Returning
  - Exhaustive pattern matching
- Error on implicit drop of linear values

### Step 1.5: Reference Counting
- Add rc runtime primitives
- Generate rc_clone/rc_drop calls
- Optimize away redundant refcount ops

### Step 1.6: Specification + Tests
- Add spec chapter for ownership modes
- Comprehensive spec tests
- Error message UI tests

**Deliverable:** Types can be declared with ownership modes; compiler enforces move/linear/rc semantics.

---

## Phase 2: Mutable Value Semantics

**Goal:** Implement `inout` parameters with exclusivity checking.

### Step 2.1: Parser + AST
- Add `ParamMode` (Default/Inout/Move) to parameters
- Add `ArgMode` to call arguments
- Parse `fn foo(inout x: T)` and `foo(inout x)`

### Step 2.2: RIR + AIR
- Represent parameter modes in IR
- Track which parameters are inout

### Step 2.3: Exclusivity Analysis
- Implement path overlap detection
- Track active borrows during analysis
- Error on conflicting accesses

### Step 2.4: Projection Tracking
- Handle `inout x.field` correctly
- Allow disjoint field access
- Track array element access (conservative: whole array)

### Step 2.5: Codegen
- Pass inout parameters by pointer
- Generate correct dereferences in callee
- Handle struct inout (flatten or pointer)

### Step 2.6: Method Syntax
- Implement `inout self` for methods
- Decide on implicit inout for mutating calls

### Step 2.7: Specification + Tests
- Add spec chapter for inout/MVS
- Tests for exclusivity violations
- Tests for correct mutation semantics

**Deliverable:** Functions can take `inout` parameters; exclusivity is enforced at compile time.

---

## Phase 3: Comptime

**Goal:** Implement compile-time evaluation with types as values.

### Step 3.1: Comptime Interpreter Core
- Implement `ComptimeValue` enum (Int, Bool, Type, Array, Struct)
- Basic expression evaluation
- Control flow (if, while)
- No side effects, no heap

### Step 3.2: Type as Value
- `type` type for comptime
- Type intrinsics: `size_of`, `align_of`, `type_name`
- Type construction: array types, struct types

### Step 3.3: Comptime Parameters
- Parse `comptime` keyword on parameters
- Track comptime vs runtime values
- Error on runtime value in comptime position

### Step 3.4: Comptime Blocks
- Parse `comptime { }` blocks
- Evaluate at compile time
- Inline results into generated code

### Step 3.5: Generic Instantiation
- Cache instantiated functions by comptime args
- Monomorphize on demand
- Handle recursive generics

### Step 3.6: Surface Syntax Sugar
- Parse `fn foo<T>()` as sugar for comptime
- Parse `struct Foo<T>` as type-returning function

### Step 3.7: Reflection Intrinsics
- `has_field(T, name)`, `field_type(T, name)`
- `is_integer(T)`, `ownership_mode(T)`
- Enable conditional compilation based on type

### Step 3.8: Specification + Tests
- Comptime evaluation tests
- Generic function tests
- Type reflection tests

**Deliverable:** Full comptime evaluation; generics work; types are first-class at compile time.

---

## Phase 4: Generators

**Goal:** Implement generator functions with yield.

### Step 4.1: Parser + AST
- Add `fn*` or `gen fn` syntax
- Add `yield` expression
- Track yield type vs return type

### Step 4.2: Generator Analysis
- Collect local variables in generator
- Identify yield points
- Determine captured variables

### Step 4.3: State Machine Transformation
- Generate state struct for each generator
- Transform control flow to state transitions
- Handle loops and conditionals containing yields

### Step 4.4: Generator Type
- Define `Generator<T>` / `Generator<Y, I>` types
- Implement `next()` method protocol
- Handle bidirectional generators (send)

### Step 4.5: For Loop Integration
- Desugar for loops to iterator protocol
- Support any type with `next()` method

### Step 4.6: Ownership in Generators
- Handle moved values into generators
- Linear value capture = linear generator
- Proper cleanup on generator drop

### Step 4.7: Specification + Tests
- Generator semantics tests
- Iteration protocol tests
- Ownership interaction tests

**Deliverable:** Generator functions work; for loops iterate over generators.

---

## Dependencies Graph

```
                    ┌──────────────┐
                    │  Current     │
                    │  Type System │
                    └──────┬───────┘
                           │
                           ▼
               ┌───────────────────────┐
               │  Phase 1: Ownership   │
               │  - value/move/linear  │
               │  - rc reference count │
               └───────────┬───────────┘
                           │
            ┌──────────────┴──────────────┐
            │                             │
            ▼                             │
┌───────────────────────┐                 │
│  Phase 2: MVS         │                 │
│  - inout parameters   │                 │
│  - exclusivity        │                 │
└───────────┬───────────┘                 │
            │                             │
            └──────────────┬──────────────┘
                           │
                           ▼
               ┌───────────────────────┐
               │  Phase 3: Comptime    │
               │  - interpreter        │
               │  - types as values    │
               │  - generics           │
               └───────────┬───────────┘
                           │
                           ▼
               ┌───────────────────────┐
               │  Phase 4: Generators  │
               │  - fn* / yield        │
               │  - state machines     │
               │  - iteration          │
               └───────────────────────┘
```

## Risk Mitigation

### Risk: Comptime Complexity
**Mitigation:** Start with a minimal comptime subset (no loops initially, then add). The interpreter doesn't need to support everything immediately.

### Risk: Generator Transformation Bugs
**Mitigation:** Extensive testing of edge cases (nested loops, early returns, exception paths). Consider using a well-tested algorithm (see Rust's generator transform).

### Risk: Ownership Mode Proliferation
**Mitigation:** Resist adding more modes. Four is already a lot. If something doesn't fit, reconsider the model rather than adding a fifth mode.

### Risk: MVS Too Restrictive
**Mitigation:** Gather feedback on patterns that don't work. Consider escape hatches (unsafe) or extensions (interior mutability) if needed.

## Success Criteria

### Phase 1 Complete When:
- [ ] All four ownership modes parse and type-check
- [ ] Use-after-move detected for move types
- [ ] Linear values must be consumed (compiler error otherwise)
- [ ] Reference counting works for rc types
- [ ] Spec coverage for ownership

### Phase 2 Complete When:
- [ ] `inout` parameters work
- [ ] Exclusivity violations detected at compile time
- [ ] Field projections work correctly
- [ ] Spec coverage for MVS

### Phase 3 Complete When:
- [ ] `comptime` blocks evaluate at compile time
- [ ] Generic functions instantiate correctly
- [ ] Type reflection intrinsics work
- [ ] `<T>` syntax sugar works
- [ ] Spec coverage for comptime/generics

### Phase 4 Complete When:
- [ ] `fn*` generators work
- [ ] `yield` suspends and resumes correctly
- [ ] For loops work with generators
- [ ] Ownership interacts correctly with generators
- [ ] Spec coverage for generators

## Future Considerations (Post-Phase 4)

These are explicitly **not** in scope for the initial implementation but may come later:

1. **Effect System**
   - Tracking allocation, panic, IO effects
   - `!Alloc`, `!Panic` annotations
   - Effect polymorphism

2. **Async/Await**
   - Builds on generator state machine foundation
   - Requires runtime/executor design

3. **Interior Mutability**
   - `Cell<T>`, `RefCell<T>` equivalents
   - For patterns that don't fit MVS

4. **Interfaces/Traits**
   - Optional, for better error messages
   - Could be sugar over comptime checks

## Related ADRs

- ADR-013: Type System Evolution Overview
- ADR-014: Ownership Modes
- ADR-015: Mutable Value Semantics
- ADR-016: Comptime
- ADR-017: Generators
