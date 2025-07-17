# Design Decisions - Session 013: LSP Improvements

## Overview

This document captures the key design decisions made during the LSP improvement session and the rationale behind them.

## 1. Position Calculation Strategy

### Decision: Separate Position Calculator Module
We created a dedicated `position.rs` module rather than inline position calculations.

**Rationale:**
- Performance: Pre-compute line starts for O(log n) position lookups via binary search
- Reusability: Can be used for other tools that need position information
- Testability: Isolated module is easier to test thoroughly
- Unicode support: Centralized handling of UTF-8 character boundaries

**Alternatives Considered:**
- Inline calculation: Simpler but O(n) performance on every error
- Caching positions in errors: Would complicate the error types across crates

## 2. Semantic Analysis Integration

### Decision: Run Semantic Analysis After Successful Parse
We only run semantic analysis if parsing succeeds, creating a two-stage diagnostic pipeline.

**Rationale:**
- User experience: Syntax errors are more fundamental and should be fixed first
- Performance: No need to run semantic analysis on syntactically invalid code
- Error clarity: Avoids cascading errors from invalid syntax

**Trade-offs:**
- Users don't see type errors until syntax is fixed
- Slightly more complex diagnostic logic

## 3. Error Reporting Architecture

### Decision: Convert All Errors to LSP Diagnostics at the Boundary
Each error type (ParseError, SemanticError) gets its own conversion function.

**Rationale:**
- Separation of concerns: Core compiler doesn't depend on LSP types
- Flexibility: Easy to customize how different errors appear in the editor
- Maintainability: Changes to error types don't cascade through the LSP

## 4. Testing Strategy

### Decision: Test Internal Methods Directly
Rather than spinning up a full LSP server for tests, we test the diagnostic methods directly.

**Rationale:**
- Speed: Tests run much faster without async server setup
- Simplicity: No need for complex test harnesses
- Coverage: Can test edge cases more easily

**Alternatives Considered:**
- Full integration tests with LSP protocol: More realistic but much slower
- Mocking the LSP client: Added complexity without much benefit

## 5. Comment Syntax Highlighting

### Decision: Use Recursive Pattern for Nested Comments
The TextMate grammar includes itself recursively for nested comment support.

```json
{
  "name": "comment.block.rue",
  "begin": "/\\*",
  "end": "\\*/",
  "patterns": [
    {
      "include": "#comments"
    }
  ]
}
```

**Rationale:**
- Correctness: Properly highlights nested comments like `/* outer /* inner */ outer */`
- Standard approach: This is how other languages handle nested comments
- VS Code support: TextMate grammars handle recursion well

## 6. Build System Integration

### Decision: Maintain Separate Test Target in Buck2
We added a `rust_test` target rather than trying to test through the library target.

**Rationale:**
- Consistency: Matches how other Rue crates are tested
- Clarity: Explicit test target is easier to discover and run
- Flexibility: Can have different dependencies for tests if needed

## 7. Dependency Management

### Decision: Remove Unused tokio-test Dependency
We removed tokio-test even though it was in Cargo.toml because we weren't using it.

**Rationale:**
- Simplicity: Fewer dependencies to manage
- Build times: One less crate to compile
- Buck2 compatibility: Avoided needing to add it to the root BUCK file

## Future Considerations

### Incremental Parsing
Currently, we reparse the entire file on every change. For larger files, we should consider:
- Incremental lexing based on change regions
- Tree-sitter or similar incremental parsing approach
- Caching ASTs between changes

### Parallel Diagnostics
We could run syntax and semantic analysis in parallel for better performance:
- Parse in one thread while running semantic analysis on the previous AST
- Would require careful synchronization of results

### Rich Error Information
Future enhancements could include:
- Quick fixes for common errors
- Related information linking to documentation
- Code actions for automatic fixes