# Parser Design Decisions - Session 2

## Core Architecture Choice: CST → AST Two-Pass Approach

### Decision
Implement a Concrete Syntax Tree (CST) first, then lower to a flat Abstract Syntax Tree (AST) in a separate step.

### Alternatives Considered
1. **Parse directly to Flat AST** - Single pass, lower memory
2. **CST first, then lower to Flat AST** - Two passes, IDE-friendly
3. **Hybrid single structure** - One representation serving both purposes

### Why We Chose CST First
- **IDE-first design:** Aligns with core project philosophy
- **Error recovery:** Natural to insert "missing" or "error" nodes
- **Incremental parsing:** Easier to reparse subtrees when source changes
- **Clear separation:** Parsing concerns vs compilation concerns
- **Proven approach:** Used by Roslyn, rust-analyzer, and other modern compilers

### Trade-offs Accepted
- **Higher memory usage:** Temporary CST before lowering (acceptable for project goals)
- **Two-pass compilation:** Adds complexity but enables better incremental compilation
- **Sync complexity:** Need to keep representations aligned (mitigated by clear ownership)

## Node Structure: Traditional Trees with Clean Abstractions

### Decision
Use traditional parent/child pointer trees with clean navigation APIs.

### Alternatives Considered
- **Red/Green trees** (Roslyn-style) - More complex but memory efficient
- **Traditional trees** - Simpler to implement and understand
- **Flat indexed trees** - Most memory efficient but complex navigation

### Why Traditional Trees
- **Simplicity:** Easier to implement correctly on first attempt
- **Migration path:** Can switch to red/green later if needed
- **Clean abstractions:** API can hide implementation details
- **Developer experience:** Easier to debug and extend

### Future Migration Strategy
If we need red/green trees later:
- Keep current navigation API
- Switch underlying implementation
- Most code won't need to change

## Token Representation: Reuse Lexer Types

### Decision
Use `rue-lexer::Token` directly as `TokenNode` rather than defining separate types.

### Why This Approach
- **DRY principle:** Don't duplicate token definitions
- **Consistency:** Single source of truth for token information
- **Simplicity:** Fewer types to maintain
- **Integration:** Natural fit with lexer output

### Implementation
```rust
pub type TokenNode = Token;  // Simple type alias
```

## Error Handling: ParseError with Spans

### Decision
Use `Result<T, ParseError>` throughout with span information for errors.

### Error Structure
```rust
pub struct ParseError {
    pub message: String,
    pub span: Span,
}
```

### Benefits
- **Good error messages:** Include source location
- **Composable:** Works well with ? operator
- **Extensible:** Can add more error context later
- **IDE-friendly:** Spans enable good error highlighting

## Expression Precedence: Explicit Precedence Climbing

### Decision
Implement operator precedence using separate methods for each precedence level.

### Structure
```
parse_expression
  └─ parse_comparison (<=)
      └─ parse_addition (+, -)
          └─ parse_multiplication (*, /, %)
              └─ parse_call (function calls)
                  └─ parse_primary (literals, identifiers, parens)
```

### Why This Approach
- **Clear precedence:** Each level is explicit
- **Easy to extend:** Adding new operators is straightforward
- **Readable:** Easy to understand operator precedence
- **Standard pattern:** Used in many parser implementations

## Trivia Handling: Attached to Nodes

### Decision
Attach whitespace and comments as "trivia" to syntax nodes rather than making them first-class nodes.

### Structure
```rust
pub struct Trivia {
    pub leading: Vec<TokenNode>,
    pub trailing: Vec<TokenNode>,
}
```

### Benefits
- **Clean tree:** Core structure not cluttered with whitespace
- **Preserved information:** All source text can be reconstructed
- **IDE features:** Supports formatting, syntax highlighting
- **Flexible:** Can ignore trivia for compilation, use for IDE features

### Current Status
- Structure in place but lexer doesn't emit trivia yet
- Ready for when we add comment support to lexer

## Recursive Structures: Box for Owned Children

### Decision
Use `Box<T>` for recursive structures to avoid infinite size.

### Examples
```rust
pub struct BinaryExprNode {
    pub left: Box<ExpressionNode>,   // Boxed to avoid infinite size
    pub right: Box<ExpressionNode>,
}
```

### Why Box Over Rc/Arc
- **Ownership clarity:** Each node has a single parent
- **Simplicity:** No reference counting overhead
- **Sufficient:** CST doesn't need sharing (unlike red/green trees)

## Testing Strategy: Comprehensive Coverage

### Decision
Test each language feature individually plus complex integration test.

### Test Categories
1. **Simple literals** - Basic token handling
2. **Binary expressions** - Operator precedence
3. **Function calls** - Argument parsing
4. **Statements** - Let statements, if/else
5. **Functions** - Complete function definitions
6. **Integration** - Full factorial example from spec

### Why This Approach
- **Incremental debugging:** Failures are easy to isolate
- **Regression prevention:** Changes won't break existing features
- **Documentation:** Tests serve as usage examples
- **Confidence:** Comprehensive coverage of language features

## Build System Integration: Dual Support

### Decision
Support both Cargo and Buck2 with manual Buck file maintenance.

### Approach
- Use Cargo for development and testing
- Manually maintain Buck BUCK files when dependencies change
- Verify both build systems work for each major change

### Why Manual Buck Maintenance
- **Reindeer limitations:** Doesn't always pick up workspace dependencies correctly
- **Control:** Can ensure Buck files are exactly what we need
- **Reliability:** Manual verification catches integration issues early

## Future-Proofing Decisions

### Salsa Integration Points
- CST parsing will become a Salsa query
- File input → CST output as incremental computation
- CST → AST lowering as separate incremental query

### IDE Feature Readiness
- All span information preserved
- Trivia structure ready for comments/whitespace
- Node navigation API ready for go-to-definition, hover, etc.

### Performance Considerations
- Traditional trees are fine for initial implementation
- Can migrate to red/green if memory becomes an issue
- Flat AST will handle compilation performance needs

These design decisions establish a solid foundation for rue's parsing infrastructure while maintaining flexibility for future enhancements.