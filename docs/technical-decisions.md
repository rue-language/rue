# Technical Decisions

## Architecture Choices

### Multi-Crate Design
**Decision:** 6-crate workspace structure  
**Rationale:** 
- Clear separation of concerns
- Faster incremental builds
- Easier testing and maintenance
- Follows Rust ecosystem patterns

### Flat AST with Integer Indices
**Decision:** ECS-inspired design with separate arrays and integer indices  
**Rationale:**
- Memory efficiency (no pointer overhead)
- Cache-friendly data layout
- Enables memory-mapped persistence
- Better for incremental compilation
- Inspired by data-oriented design principles

### Salsa for Incremental Compilation
**Decision:** Use Salsa 0.22 for query-based incremental compilation  
**Rationale:**
- Expression-level granularity by default
- Proven architecture (used by rust-analyzer)
- Automatic dependency tracking
- Efficient caching and invalidation

### Dual Build Systems
**Decision:** Support both Cargo and Buck2  
**Rationale:**
- Cargo for standard Rust ecosystem compatibility
- Buck2 for advanced incremental compilation features
- Learning opportunity for cutting-edge build systems
- Future cross-compilation capabilities

### Hand-Written Parser
**Decision:** Recursive descent parser instead of parser combinators or generators  
**Rationale:**
- Full control over error recovery
- IDE-friendly concrete syntax tree
- Easier to maintain and debug
- Better integration with incremental compilation

## Parser Architecture (Session 002)

### CST → AST Two-Pass Approach
**Decision:** Implement a Concrete Syntax Tree (CST) first, then lower to AST  
**Rationale:**
- IDE-first design aligns with core project philosophy
- Natural error recovery with "missing" or "error" nodes
- Easier incremental parsing when source changes
- Clear separation of parsing vs compilation concerns
- Proven approach used by Roslyn, rust-analyzer

### Node Structure
**Decision:** Traditional parent/child pointer trees with clean navigation APIs  
**Rationale:**
- Simplicity: easier to implement correctly on first attempt
- Migration path: can switch to red/green trees later if needed
- Clean abstractions hide implementation details
- Easier to debug and extend

### Expression Precedence
**Decision:** Explicit precedence climbing with separate methods per level  
**Rationale:**
- Clear precedence: each level is explicit
- Easy to extend: adding new operators is straightforward
- Readable: easy to understand operator precedence
- Standard pattern used in many parser implementations

### Trivia Handling
**Decision:** Attach whitespace and comments as "trivia" to syntax nodes  
**Rationale:**
- Clean tree: core structure not cluttered with whitespace
- Preserved information: all source text can be reconstructed
- IDE features: supports formatting, syntax highlighting
- Flexible: can ignore trivia for compilation, use for IDE features

## Code Generation Architecture (Session 003)

### Three-Stage Pipeline
**Decision:** Separate code generation into CodeGen → Assembly → ELF stages  
**Rationale:**
- Clear separation of concerns
- Easier testing and debugging
- Allows different backends (could add LLVM later)
- Follows traditional compiler architecture

### Stack-Based Expression Evaluation
**Decision:** Use stack-based evaluation for all expressions  
**Rationale:**
- Handles arbitrary expression complexity
- Simple to implement and understand
- No register allocation complexity initially
- Matches current language simplicity

### Manual Instruction Encoding
**Decision:** Hand-code x86-64 instruction encoding  
**Rationale:**
- Complete control over generated code
- No external dependencies
- Educational value in understanding machine code
- Enables future optimizations

### Minimal ELF Implementation
**Decision:** Generate minimal but complete ELF executables directly  
**Rationale:**
- Produces real executables that run on Linux
- Educational: understanding executable format
- No external linker dependency
- Complete control over output

## Intermediate Representation (Session 009)

### IR Architecture Design
**Decision:** Introduce platform-independent IR between AST and assembly  
**Rationale:**
- Foundation for multi-target support
- Separates platform logic from code generation
- Enables future optimizations
- Simplifies backend implementations

**Implementation Note:** While design docs refer to "TargetIR", the actual implementation uses:
- `Instruction` enum (in `rue-codegen`) as the platform-independent IR
- `MachineInstr` (in `rue-ir::target`) as the target-specific IR for x86-64

### Simple Virtual Registers
**Decision:** Use `VReg(u32)` for virtual registers  
**Rationale:**
- Simplicity is top priority
- Easy to extend later with types or metadata
- Debugging info handled by higher-level IRs
- Types can be added via separate tables if needed

### Linear Control Flow
**Decision:** Use linear instruction sequence with labels and jumps  
**Rationale:**
- Optimizations will happen in future MidIR, not in the target-specific IR
- The IR is purely for codegen, no analysis needed
- Labels/jumps are close to assembly representation
- Simple to generate and consume

## Type System Design (Session 010)

### Type Annotation Syntax
**Decision:** Adopt Rust-like type annotations with `:` separator  
**Rationale:**
- Consistency with Rue's existing Rust-inspired syntax
- Familiar to Rust developers
- Clear visual separation between name and type
- Well-established syntax for function return types

### Minimal Initial Types
**Decision:** Start with `i32`, `i64`, `bool`, and `()` types  
**Rationale:**
- Demonstrates multiple numeric types
- Maintains backward compatibility with i64
- Essential boolean type for conditionals
- Unit type for procedures

### Context-Sensitive Type Inference
**Decision:** Require explicit annotations, infer within expressions  
**Rationale:**
- No global analyses needed
- Specifying types and inferring bodies leads to good error messages
- Same approach as Rust
- Avoids complexity of bidirectional inference

### Strict Type Checking
**Decision:** No implicit conversions between types  
**Rationale:**
- Prevents subtle bugs
- Makes type errors explicit
- Simpler to implement
- Educational value in understanding types

## Register Allocation (Session 011)

### Stack Spilling Strategy
**Decision:** Use push/pop spilling when registers are exhausted  
**Rationale:**
- Unlimited capacity: can handle arbitrarily complex programs
- Unblocks all examples immediately
- Foundation for future optimizations
- Minimal complexity vs graph coloring

### LRU Spill Policy
**Decision:** Spill least recently used register  
**Rationale:**
- Simple to implement
- Good enough for current needs
- Can optimize later with better algorithms
- Predictable behavior

### Two-Tier Register System
**Decision:** Physical registers (fast) with stack spillover (unlimited)  
**Rationale:**
- Best of both worlds: performance when possible, capacity when needed
- Natural fit with x86-64 architecture
- Straightforward implementation
- Foundation for calling conventions

## Comment Design (Session 012)

### Comment Syntax
**Decision:** Use `//` for single-line, `/* */` for multi-line with nesting  
**Rationale:**
- Following Rust as primary language influence
- Nested comments allow commenting out code with comments
- Familiar to modern programmers
- Superior to C's non-nested approach

### Lexer-Only Implementation
**Decision:** Handle comments entirely in lexer, don't pass to parser  
**Rationale:**
- Comments are not part of the AST
- Simpler implementation
- Standard approach for most compilers
- Trade-off: cannot preserve for documentation generation

## LSP Architecture (Session 013)

### Semantic Token Categories
**Decision:** Support keyword, type, variable, function, comment, number tokens  
**Rationale:**
- Covers all current language constructs
- Maps well to standard VS Code themes
- Extensible for future language features
- Provides rich syntax highlighting

### Two-Stage Diagnostic Pipeline
**Decision:** Run semantic analysis only after successful parse  
**Rationale:**
- User experience: syntax errors are more fundamental
- Performance: no need to analyze invalid syntax
- Error clarity: avoids cascading errors
- Simpler diagnostic logic

### Position Calculation Module
**Decision:** Separate position calculator with pre-computed line starts  
**Rationale:**
- Performance: O(log n) position lookups via binary search
- Reusability: can be used for other tools
- Testability: isolated module easier to test
- Unicode support: centralized UTF-8 handling

### Error Recovery Strategy
**Decision:** Convert all error types to LSP diagnostics at the boundary  
**Rationale:**
- Separation of concerns: compiler doesn't depend on LSP
- Flexibility: easy to customize error appearance
- Maintainability: changes don't cascade through LSP
- Clean architecture boundaries

## Language Design Choices

### Expression-Oriented Design
**Decision:** Most constructs are expressions that return values  
**Rationale:**
- Consistent with Rust influence
- More composable language constructs
- Enables powerful expression nesting
- Natural for functional-style code

### Minimal Initial Subset
**Decision:** Start with functions, if/else, arithmetic, single parameters  
**Rationale:**
- Tests all major compiler phases
- Small enough to implement quickly
- Sufficient complexity for real testing
- Clear upgrade path to full language

### Exit Code as Return Value
**Decision:** main() return value becomes program exit code  
**Rationale:**
- No I/O needed initially
- Verifiable program execution
- Simple testing mechanism
- Unix-friendly design

## Development Workflow Choices

### Jujutsu (jj) over Git
**Decision:** Use jj for version control  
**Rationale:**
- Better support for modern development workflows
- Easier branch management
- More intuitive conflict resolution
- Educational value

### Comprehensive CI from Start
**Decision:** Set up extensive CI before first commit  
**Rationale:**
- Catch issues early
- Validate both build systems
- Cross-platform compatibility
- Quality enforcement from day one

### MIT/Apache 2.0 Licensing
**Decision:** Dual license like Rust ecosystem  
**Rationale:**
- Maximizes adoption potential
- Patent protection with Apache 2.0
- Simple with MIT
- Rust ecosystem standard