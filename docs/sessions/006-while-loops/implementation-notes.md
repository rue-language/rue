# Implementation Notes: While Loops

## Architecture Decisions

### 1. AST Design Choice
- **Decision**: Add `WhileStatementNode` as a separate statement type rather than a generic loop construct
- **Rationale**: Keeps AST nodes simple and specific, easier to extend with break/continue later
- **Alternative Considered**: Generic `LoopNode` with loop type enum

### 2. Code Generation Strategy
- **Decision**: Use label-based jumps rather than structured control flow
- **Rationale**: Direct assembly generation, matches conditional if/else pattern
- **Implementation**: Two labels per while loop (`loop_start_N`, `loop_end_N`)

### 3. Condition Evaluation
- **Decision**: Evaluate condition as expression, compare result against 0
- **Rationale**: Consistent with if statement condition handling
- **Behavior**: Any non-zero value is "true", zero is "false"

## Technical Challenges Solved

### 1. Operator Support Gap
- **Problem**: Parser only supported `<=` operator, but while loops often need `>`
- **Solution**: Extended comparison parsing to support all operators
- **Added**: `<`, `>`, `>=`, `==`, `!=` parsing and code generation

### 2. Jump Instruction Implementation
- **Problem**: Code generator missing `Jg` (jump if greater) instruction
- **Solution**: Added instruction to enum and implemented assembly generation
- **Opcode**: `0x0f 0x8f` for 32-bit relative jump if greater

### 3. Label Management
- **Problem**: Each while loop needs unique labels to avoid conflicts
- **Solution**: Used existing label counter system from if/else implementation
- **Pattern**: `loop_start_{counter}` and `loop_end_{counter}`

## Testing Strategy

### 1. Unit Test Coverage
- **Lexer**: Verify `while` keyword tokenization
- **Parser**: Test valid and invalid while loop syntax
- **Semantic**: Check variable scoping in while loop bodies
- **Codegen**: Validate assembly instruction generation

### 2. Integration Testing
- **End-to-end**: Compile and execute while loop programs
- **Edge Cases**: While loops that never execute (condition false from start)
- **Nested**: While loops inside if/else statements

### 3. LSP Validation
- **Syntax Highlighting**: Ensure `while` keyword is highlighted
- **Error Detection**: Parser integration correctly identifies syntax errors

## Code Generation Details

### Assembly Pattern
```assembly
; While loop: while condition { body }
loop_start_0:
    ; Evaluate condition expression
    ; Result in rax (0 = false, non-zero = true)
    cmp rax, 0
    je loop_end_0        ; Jump to end if condition is false
    
    ; Execute loop body statements
    ; (each statement may modify stack/registers)
    
    jmp loop_start_0     ; Jump back to condition check
loop_end_0:
    ; Continue with code after while loop
```

### Register Usage
- **rax**: Condition evaluation result
- **rbx**: Temporary for binary operations
- **rsp/rbp**: Stack frame management
- **rdi**: Function parameter passing

### Stack Management
- While loop doesn't introduce new stack frame
- Expression evaluation uses existing stack discipline
- No special stack cleanup needed (expressions are self-contained)

## Current Limitations

### 1. Variable Reassignment
- **Issue**: Variables cannot be modified within loop body
- **Impact**: Limits practical loop usage (infinite loops without external termination)
- **Workaround**: Use function calls to simulate state changes

### 2. Loop Control
- **Missing**: `break` and `continue` statements
- **Impact**: Cannot exit loops early or skip iterations
- **Future Work**: Would require additional jump targets and control flow

### 3. Performance
- **No Optimizations**: Condition evaluated every iteration
- **Opportunity**: Constant folding, loop unrolling for known bounds
- **Current Focus**: Correctness over optimization

## Design Patterns Used

### 1. Visitor Pattern
- Consistent with existing AST traversal in semantic analysis and codegen
- Each compiler phase handles `WhileStatementNode` appropriately

### 2. Template Method
- Code generation follows same pattern as if/else statements
- Label generation, instruction emission, cleanup

### 3. Error Handling
- Parse errors bubble up with proper span information
- Semantic errors provide meaningful error messages

## Future Extension Points

### 1. Loop Optimizations
- **Hoisting**: Move loop-invariant computations outside loop
- **Unrolling**: Replicate loop body for known iteration counts
- **Vectorization**: SIMD instructions for data-parallel loops

### 2. Advanced Control Flow
- **Break/Continue**: Additional jump targets within loops
- **Labeled Breaks**: Multi-level loop exit (like Java/Rust)
- **Loop-else**: Execute code only if loop completes normally

### 3. Static Analysis
- **Termination Analysis**: Detect infinite loops at compile time
- **Iteration Bounds**: Analyze loop iteration count
- **Data Flow**: Track variable usage across loop iterations