# Design Decisions: While Loop Implementation

## Key Architectural Choices

### 1. Syntax Design

**Decision**: Use Rust-style `while condition { body }` syntax  
**Rationale**: 
- Consistent with existing if/else and function syntax
- Familiar to developers coming from Rust/C-family languages
- Clear visual separation of condition and body

**Alternatives Considered**:
- C-style `while (condition) { body }` - rejected for consistency
- Python-style `while condition:` - rejected due to brace-based design

### 2. AST Node Structure

**Decision**: Create dedicated `WhileStatementNode` with explicit fields
```rust
pub struct WhileStatementNode {
    pub while_token: TokenNode,
    pub condition: ExpressionNode,
    pub body: BlockNode,
    pub trivia: Trivia,
}
```

**Rationale**:
- Explicit structure makes code generation straightforward
- Trivia handling preserves whitespace for IDE features
- Follows established pattern from `IfStatementNode`

**Alternatives Considered**:
- Generic loop node with type discriminator - rejected for simplicity
- Inline condition in statement enum - rejected for type safety

### 3. Condition Evaluation Semantics

**Decision**: Treat condition as boolean expression (0 = false, non-zero = true)  
**Rationale**:
- Consistent with if statement condition handling
- Matches C/Rust semantics for integer conditions
- Simplifies code generation (single comparison against 0)

**Example**:
```rue
while n > 0 { ... }    // Comparison evaluates to 0 or 1
while some_func() { ... }  // Function result treated as boolean
```

### 4. Code Generation Strategy

**Decision**: Label-based jumps with condition check at loop start
```assembly
loop_start_N:
    ; Evaluate condition
    cmp rax, 0
    je loop_end_N
    ; Execute body
    jmp loop_start_N
loop_end_N:
```

**Rationale**:
- Efficient for most cases (condition typically becomes false)
- Consistent with if/else jump pattern
- Simple label management using existing counter

**Alternatives Considered**:
- Jump to condition at end (do-while style) - rejected for semantic mismatch
- Structured control flow with stack - rejected for complexity

### 5. Operator Support Extension

**Decision**: Implement full comparison operator set (`<`, `<=`, `>`, `>=`, `==`, `!=`)  
**Rationale**:
- While loops commonly need greater-than comparisons
- Complete operator set prevents future limitations
- Symmetric implementation reduces cognitive load

**Impact**: Required adding `Jg` instruction and updating parser precedence

### 6. Variable Scoping

**Decision**: While loop body shares scope with containing function
**Rationale**:
- Consistent with if statement scoping rules
- Simpler implementation (no new scope frame)
- Variables declared in loop body available after loop

**Note**: This will need revision when block-scoped variables are added

### 7. Error Handling Strategy

**Decision**: Comprehensive error reporting with span information
**Examples**:
- Missing condition: "Expected expression after 'while'"
- Missing body: "Expected '{' after while condition"
- Invalid condition: Semantic analysis catches undefined variables

**Implementation**: Leverages existing error infrastructure from if/else

### 8. Testing Approach

**Decision**: Multi-layered testing strategy
1. **Unit Tests**: Each compiler phase tested independently
2. **Integration Tests**: End-to-end compilation and execution
3. **LSP Tests**: Parser integration for IDE features

**Rationale**:
- Catches regressions at multiple levels
- Validates both correctness and usability
- Ensures dual build system (Cargo/Buck2) compatibility

## Non-Decisions (Future Work)

### 1. Loop Control Statements
- **Deferred**: `break` and `continue` statements
- **Reason**: Adds complexity to control flow analysis
- **Future**: Will require additional jump targets and scope handling

### 2. Variable Reassignment
- **Deferred**: Modifying variables within loop body
- **Reason**: Requires mutable binding semantics
- **Current Limitation**: Infinite loops unless condition is initially false

### 3. Loop Optimizations
- **Deferred**: Loop unrolling, hoisting, vectorization
- **Reason**: Focus on correctness before performance
- **Future**: Could significantly improve generated code quality

### 4. Iterator-Style Loops
- **Deferred**: `for item in collection` syntax
- **Reason**: Requires collection types and iterators
- **Future**: More ergonomic than manual index management

## Implementation Lessons

### 1. Incremental Development
- Each compiler phase updated independently
- Tests written alongside implementation
- Frequent validation prevented compound errors

### 2. Existing Pattern Reuse
- If/else implementation provided blueprint
- Label generation, instruction emission patterns reused
- Error handling infrastructure already established

### 3. Operator Completeness
- Initial implementation only had `<=` operator
- While loops exposed gap in comparison operators
- Systematic addition of all comparison operators
- Future features less likely to hit operator limitations

### 4. Code Generation Complexity
- Assembly generation more complex than parsing
- Jump instruction opcodes require exact specification
- Label resolution happens in separate pass
- Debugging assembly requires understanding x86-64 encoding

## Quality Metrics

### Test Coverage
- **31 total tests** across all crates
- **100% pass rate** on both Cargo and Buck2
- **End-to-end validation** with executable programs

### Documentation
- Session notes capture decision rationale
- Implementation details for future maintainers
- Design patterns documented for consistency

### IDE Integration
- LSP server automatically supports new syntax
- VS Code extension updated with keyword highlighting
- Real-time error detection in editors

This implementation establishes while loops as a first-class language feature and provides the foundation for Rue's Turing completeness.