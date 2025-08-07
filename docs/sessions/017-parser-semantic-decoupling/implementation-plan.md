# Implementation Plan: Parser-Semantic Decoupling [IN PROGRESS]

**Status: 🚧 Partially Implemented**

The AST infrastructure and CST→AST lowering are fully implemented and tested. Integration into the main pipeline (Phases 4-7) remains to be completed.

## Implementation Checklist

### Phase 1: Foundation (Day 1) ✅
- [x] **1.1** Create `docs/architecture/ir-pipeline.md`
- [x] **1.2** Create session directory with design documentation
- [x] **1.3** Document implementation plan with checklist

### Phase 2: Zig-Inspired AST Implementation (Days 2-3) ✅
- [x] **2.1** Create `rue-ir/src/ast.rs` with Zig-inspired structure
  - [x] **2.1a** Define `NodeList` struct-of-arrays container
  - [x] **2.1b** Define `NodeIndex` and `StringIndex` types (u32-based)
  - [x] **2.1c** Implement `StringPool` for string deduplication
  - [x] **2.1d** Define parallel `spans` array strategy

- [x] **2.2** Implement compact node representation
  - [x] **2.2a** `NodeTag` enum (1 byte) for all node types
  - [x] **2.2b** `NodeData` struct (8 bytes) for dual-purpose fields
  - [x] **2.2c** `extra_data` array for variable-length data
  - [x] **2.2d** 16-byte total node size (with alignment)

- [x] **2.3** Implement AST builder with new design
  - [x] **2.3a** Efficient node allocation (no individual allocations)
  - [x] **2.3b** Extra data management for variable-length content
  - [x] **2.3c** String interning integration
  - [x] **2.3d** Builder methods for common node types

- [x] **2.4** Write AST unit tests
  - [x] **2.4a** Node size verification (16 bytes)
  - [x] **2.4b** String interning tests
  - [x] **2.4c** Basic AST construction tests
  - [x] **2.4d** Comprehensive traversal tests (18 tests passing)

### Phase 3: CST to AST Lowering (Days 3-4) ✅
- [x] **3.1** Create `rue-parser/src/ast_builder.rs`
  - [x] **3.1a** Define `CstToAst` struct with builder
  - [x] **3.1b** Implement `lower_cst_to_ast(cst: &CstRoot) -> Ast`
  - [x] **3.1c** Handle trivia removal (preserve spans only)
  - [x] **3.1d** Desugar syntactic constructs

- [x] **3.2** Implement CST traversal
  - [x] **3.2a** `convert_function(node: FunctionNode) -> NodeIndex`
  - [x] **3.2b** `convert_block(node: BlockNode) -> NodeIndex`
  - [x] **3.2c** `convert_statement(node: StatementNode) -> NodeIndex`
  - [x] **3.2d** `convert_expression(node: ExpressionNode) -> NodeIndex`

- [x] **3.3** Handle special cases
  - [x] **3.3a** All expression types (binary, unary, calls, if, while)
  - [x] **3.3b** Statement vs expression blocks
  - [x] **3.3c** Implicit returns (final_expr handling)
  - [x] **3.3d** Struct/tuple/array literals and field/array access

- [x] **3.4** Write lowering tests
  - [x] **3.4a** Test each node type lowering (18 comprehensive tests)
  - [x] **3.4b** Test span preservation
  - [x] **3.4c** Test string interning
  - [x] **3.4d** Complex program tests

### Phase 4: AST-Based Semantic Analysis (Days 4-5)
- [ ] **4.1** Create `rue-semantic/src/ast_analyzer.rs`
  - [ ] **4.1a** Define `AstAnalyzer` struct
  - [ ] **4.1b** Implement `analyze_ast(ast: &Ast) -> Result<AnalysisResult>`
  - [ ] **4.1c** Adapt existing type checker to work with AST
  - [ ] **4.1d** Preserve all existing semantic checks

- [ ] **4.2** Implement AST traversal for analysis
  - [ ] **4.2a** Type checking with AST nodes
  - [ ] **4.2b** Scope management with AST structure
  - [ ] **4.2c** HIR construction from AST + types
  - [ ] **4.2d** Error reporting with AST spans

- [ ] **4.3** Maintain compatibility
  - [ ] **4.3a** Keep existing `analyze_cst` working
  - [ ] **4.3b** Same `AnalysisResult` output format
  - [ ] **4.3c** Same error messages and spans
  - [ ] **4.3d** Same HIR structure produced

### Phase 5: Pipeline Integration (Days 5-6)
- [ ] **5.1** Update `rue-compiler/src/pipeline.rs`
  - [ ] **5.1a** Add AST stage to pipeline
  - [ ] **5.1b** Support both CST→HIR and CST→AST→HIR paths
  - [ ] **5.1c** Add feature flag or config for path selection
  - [ ] **5.1d** Update error handling for new path

- [ ] **5.2** Create validation framework
  - [ ] **5.2a** HIR comparison function (structural equality)
  - [ ] **5.2b** Parallel execution of both paths
  - [ ] **5.2c** Detailed diff reporting for mismatches
  - [ ] **5.2d** Performance benchmarking harness

- [x] **5.3** Update build system (Partial)
  - [x] **5.3a** Update Cargo.toml dependencies (rue-ir added)
  - [x] **5.3b** Update Buck2 build files (rue-ir dependency added)
  - [ ] **5.3c** Add feature flags for development
  - [ ] **5.3d** Update CI configuration

### Phase 6: Testing and Validation (Days 6-7)
- [ ] **6.1** Comprehensive testing
  - [ ] **6.1a** Run all existing tests through both paths
  - [ ] **6.1b** Compare HIR outputs for equivalence
  - [ ] **6.1c** Verify error messages unchanged
  - [ ] **6.1d** Check performance characteristics

- [ ] **6.2** Corpus testing
  - [ ] **6.2a** Test all example programs
  - [ ] **6.2b** Test all test suite programs
  - [ ] **6.2c** Test error cases
  - [ ] **6.2d** Test edge cases (empty programs, etc.)

- [ ] **6.3** Performance validation
  - [ ] **6.3a** Benchmark CST→HIR vs CST→AST→HIR
  - [ ] **6.3b** Memory usage comparison
  - [ ] **6.3c** Cache performance analysis
  - [ ] **6.3d** Parallel processing potential

- [ ] **6.4** Documentation updates
  - [ ] **6.4a** Update compiler documentation
  - [ ] **6.4b** Update README if needed
  - [ ] **6.4c** Add examples of AST usage
  - [ ] **6.4d** Document migration guide

### Phase 7: Migration Completion (Day 8)
- [ ] **7.1** Switch default path
  - [ ] **7.1a** Make CST→AST→HIR the default
  - [ ] **7.1b** Keep old path available via flag
  - [ ] **7.1c** Update all tools to use new path
  - [ ] **7.1d** Monitor for any issues

- [ ] **7.2** Cleanup (after stabilization period)
  - [ ] **7.2a** Remove old CST→HIR path
  - [ ] **7.2b** Remove compatibility shims
  - [ ] **7.2c** Simplify configuration
  - [ ] **7.2d** Archive migration code

- [ ] **7.3** Future work planning
  - [ ] **7.3a** Plan HIR data-oriented refactoring
  - [ ] **7.3b** Plan MIR improvements
  - [ ] **7.3c** Consider CST move to rue-ir
  - [ ] **7.3d** Document lessons learned

## Testing Strategy

### Unit Tests
Each component tested in isolation:
- Arena allocation and deallocation
- String interning correctness
- AST node construction
- Lowering individual CST nodes

### Integration Tests
Complete pipeline testing:
- Parse → CST → AST → Semantic → HIR
- Error propagation through pipeline
- Span preservation through transformations

### Comparison Tests
Validate equivalence:
```rust
#[test]
fn test_both_paths_equivalent() {
    let source = "fn main() -> i32 { 42 }";
    let cst = parse(source).unwrap();
    
    // Old path
    let hir_direct = analyze_cst(&cst).unwrap();
    
    // New path
    let ast = lower_cst_to_ast(&cst);
    let hir_via_ast = analyze_ast(&ast).unwrap();
    
    assert_eq!(hir_direct, hir_via_ast);
}
```

### Performance Tests
Benchmark critical operations:
- AST construction time
- Memory usage (peak and total)
- Cache misses during traversal
- Parallel processing scalability

## Risk Mitigation

### Risk: HIR Mismatch
**Mitigation**: Extensive comparison testing, gradual rollout

### Risk: Performance Regression
**Mitigation**: Benchmark early and often, optimize hot paths

### Risk: Breaking Changes
**Mitigation**: Parallel paths, feature flags, incremental migration

### Risk: Incomplete Migration
**Mitigation**: Clear checklist, phase gates, documentation

## Success Criteria

- [x] AST infrastructure tests pass (18 tests)
- [x] CST→AST lowering fully implemented
- [ ] All tests pass with new AST path
- [ ] No performance regression (goal: 10% improvement)
- [ ] HIR output identical between paths
- [ ] Documentation complete and clear
- [ ] Team confident in new architecture

## Notes

- Commit after each numbered step for checkpoints
- Update this checklist as work progresses
- Mark items complete with [x] when done
- Add sub-items if steps need breakdown
- Document any deviations or discoveries

## Current Status Summary

**Completed:**
- ✅ Phase 1: Foundation (documentation)
- ✅ Phase 2: Zig-inspired AST implementation in rue-ir
- ✅ Phase 3: CST to AST lowering in rue-parser
- ✅ Build system updates (dependencies)

**Remaining Work:**
- ⏳ Phase 4: AST-based semantic analysis (not started)
- ⏳ Phase 5: Pipeline integration (partial - build files updated)
- ⏳ Phase 6: Testing and validation (not started)
- ⏳ Phase 7: Migration completion (not started)

**Known Issues:**
- Struct type definitions not yet supported in AST (only literals work)
- Tuple and array type annotations need implementation
- AST path not integrated into main compilation pipeline