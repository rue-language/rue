# Migration Checklist: Data-Driven HIR

## Pre-Implementation Setup
- [ ] Read all session documentation
- [ ] Understand current HIR structure in `crates/rue-ir/src/hir.rs`
- [ ] Understand AST structure in `crates/rue-ir/src/ast.rs`
- [ ] Review TypeChecker in `crates/rue-semantic/src/type_checker.rs`

## Phase 1: Create HIR2 Structure (Day 1)

### Morning: Core Data Structures
- [ ] Create `crates/rue-ir/src/hir2.rs`
  - [ ] Define `Hir` struct with instruction list
  - [ ] Define `InstTag` enum with all instruction types
  - [ ] Define `InstData` struct (two u32 fields)
  - [ ] Define index types (InstIndex, BlockIndex, etc.)
  - [ ] Add string pool (reuse from AST if possible)

### Afternoon: Builder Infrastructure
- [ ] Create `crates/rue-ir/src/hir2_builder.rs`
  - [ ] Implement `HirBuilder` struct
  - [ ] Add `emit_literal` method
  - [ ] Add `emit_binary` method
  - [ ] Add `emit_let` method
  - [ ] Add `emit_return` method
  - [ ] Add block management methods

### Evening: Basic Testing
- [ ] Create `crates/rue-ir/src/hir2_tests.rs`
  - [ ] Test instruction emission
  - [ ] Test block creation
  - [ ] Test type tracking
  - [ ] Verify memory layout

## Phase 2: TypeChecker Integration (Day 2)

### Morning: Copy and Modify TypeChecker
- [ ] Copy `type_checker.rs` to `type_checker2.rs`
- [ ] Add `HirBuilder` parameter to all methods
- [ ] Convert `check_literal` to emit instructions
- [ ] Convert `check_binary` to emit instructions
- [ ] Convert `check_identifier` to emit instructions

### Afternoon: Control Flow
- [ ] Convert `check_if` to emit instructions
- [ ] Convert `check_while` to emit instructions
- [ ] Convert `check_block` to emit instructions
- [ ] Convert `check_return` to emit instructions

### Evening: Function Handling
- [ ] Convert `check_function` to emit instructions
- [ ] Convert `check_call` to emit instructions
- [ ] Handle function parameters as instructions
- [ ] Test with simple programs

## Phase 3: Pipeline Integration (Day 3)

### Morning: Create Parallel Path
- [ ] Add `analyze_cst_v2` to `crates/rue-semantic/src/lib.rs`
- [ ] Create feature flag for testing both paths
- [ ] Update `AnalysisResult` to support HIR2
- [ ] Add conversion from HIR2 to old HIR (temporary)

### Afternoon: MIR Lowering
- [ ] Create `crates/rue-lowering/src/hir2_to_mir.rs`
- [ ] Implement instruction-based lowering
- [ ] Compare output with old path
- [ ] Fix any discrepancies

### Evening: Testing
- [ ] Run semantic tests through both paths
- [ ] Compare outputs
- [ ] Fix failing tests
- [ ] Add performance benchmarks

## Phase 4: Complete Migration (Day 4)

### Morning: Update All Tests
- [ ] Update aggregate_type_tests.rs
- [ ] Update type_preservation_test.rs
- [ ] Update hir_control_flow_test.rs
- [ ] Update hir_validation_integration_test.rs
- [ ] Update constraint_inference_test.rs
- [ ] Update hir_roundtrip_test.rs
- [ ] Update hir_builder/tests.rs

### Afternoon: Remove Old Code
- [ ] Delete old HIR structure
- [ ] Delete old TypeChecker methods
- [ ] Remove conversion code
- [ ] Update all imports

### Evening: Documentation
- [ ] Update architecture documentation
- [ ] Add HIR2 format specification
- [ ] Update README
- [ ] Create migration guide

## Validation Checklist

### Correctness
- [ ] All 67 semantic tests pass
- [ ] HIR validation works
- [ ] MIR output identical
- [ ] Executables run correctly

### Performance
- [ ] Memory usage reduced by 4x
- [ ] Cache misses reduced by 50%
- [ ] Traversal speed improved by 2x
- [ ] Compilation time improved

### Code Quality
- [ ] No compiler warnings
- [ ] No clippy warnings
- [ ] Documentation complete
- [ ] Examples updated

## Rollback Plan

If issues arise:
1. Keep old HIR path via feature flag
2. Run both paths in production
3. Compare outputs for discrepancies
4. Fix issues incrementally
5. Only remove old path when stable

## Key Commands

```bash
# Build and test HIR2
cargo test -p rue-ir hir2

# Run semantic tests with new path
cargo test -p rue-semantic --features hir2

# Benchmark old vs new
cargo bench -p rue-ir hir_comparison

# Check for memory leaks
valgrind --leak-check=full target/debug/rue

# Profile cache usage
perf stat -e cache-misses,cache-references target/release/rue
```

## Common Issues and Solutions

### Issue: Instruction ordering wrong
**Solution**: Instructions must be emitted in evaluation order

### Issue: Types not propagating
**Solution**: Ensure parallel type array is updated for each instruction

### Issue: Spans lost
**Solution**: Builder must track current span and add to parallel array

### Issue: Extra data indexing wrong
**Solution**: Carefully track extra_data indices, consider helper methods

### Issue: Block scoping broken
**Solution**: Use block start/end instructions to maintain scope

## Success Criteria

- [ ] Zero heap allocations in HIR (except strings)
- [ ] All tests passing
- [ ] Performance improvements measured
- [ ] Code is simpler and more maintainable
- [ ] Ready for future CST elimination