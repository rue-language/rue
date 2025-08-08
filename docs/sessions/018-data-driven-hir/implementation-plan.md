# Implementation Plan: Data-Driven HIR

## Phase 1: Create New HIR Structure

### Step 1.1: Define HIR2 Module
**File**: `crates/rue-ir/src/hir2.rs`

```rust
// Core structures needed:
pub struct Hir {
    instructions: InstructionList,
    extra_data: Vec<u32>,
    types: Vec<RueType>,
    spans: Vec<Span>,
    strings: StringPool,
    
    // Function table
    functions: Vec<FunctionDef>,
}

pub struct InstructionList {
    tags: Vec<InstTag>,
    data: Vec<InstData>,
}

pub struct InstData {
    lhs: u32,
    rhs: u32,
}

pub enum InstTag {
    // Control flow
    Function,
    Block,
    Return,
    If,
    While,
    
    // Variables
    Let,
    Load,
    Store,
    
    // Expressions
    Literal,
    Binary,
    Unary,
    Call,
    Cast,
    
    // Aggregates
    StructLit,
    FieldAccess,
}
```

### Step 1.2: Create HIR Builder
**File**: `crates/rue-ir/src/hir2_builder.rs`

```rust
pub struct HirBuilder {
    hir: Hir,
    current_function: Option<FunctionIndex>,
}

impl HirBuilder {
    pub fn emit_binary(&mut self, lhs: InstIndex, rhs: InstIndex, op: BinOp, ty: RueType) -> InstIndex {
        let index = self.hir.instructions.tags.len();
        self.hir.instructions.tags.push(InstTag::Binary);
        self.hir.instructions.data.push(InstData { lhs: lhs.0, rhs: rhs.0 });
        self.hir.extra_data.push(op as u32);
        self.hir.types.push(ty);
        InstIndex(index as u32)
    }
    // ... other emit methods
}
```

## Phase 2: Adapt TypeChecker

### Step 2.1: Modify TypeChecker Output
**File**: `crates/rue-semantic/src/type_checker2.rs`

Current TypeChecker returns tree-based HIR. Modify to use HirBuilder:

```rust
impl TypeChecker {
    fn check_expression(&mut self, expr: &CstExpr, builder: &mut HirBuilder) -> Result<InstIndex, Error> {
        match expr {
            CstExpr::Binary { lhs, rhs, op } => {
                let lhs_inst = self.check_expression(lhs, builder)?;
                let rhs_inst = self.check_expression(rhs, builder)?;
                let ty = self.infer_binary_type(lhs_ty, rhs_ty, op)?;
                Ok(builder.emit_binary(lhs_inst, rhs_inst, op, ty))
            }
            // ...
        }
    }
}
```

### Step 2.2: Create Migration Wrapper
Temporarily support both HIR formats:

```rust
pub fn analyze_cst_v2(cst: &CstRoot) -> Result<Hir2, Error> {
    let mut builder = HirBuilder::new();
    let mut type_checker = TypeChecker::new();
    type_checker.check_program(cst, &mut builder)?;
    Ok(builder.finish())
}
```

## Phase 3: Update MIR Lowering

### Step 3.1: Create HIR2 to MIR Lowering
**File**: `crates/rue-lowering/src/hir2_to_mir.rs`

```rust
pub fn lower_hir2_to_mir(hir: &Hir2) -> Mir {
    // Process instruction stream instead of tree
    for (i, tag) in hir.instructions.tags.iter().enumerate() {
        match tag {
            InstTag::Binary => {
                let data = hir.instructions.data[i];
                // Generate MIR instructions
            }
            // ...
        }
    }
}
```

## Phase 4: Testing Strategy

### Step 4.1: Parallel Testing
1. Keep both HIR formats temporarily
2. Run tests through both paths
3. Compare outputs

### Step 4.2: Performance Benchmarks
```rust
#[bench]
fn bench_hir_v1_vs_v2() {
    // Measure:
    // - Memory usage
    // - Cache misses
    // - Traversal speed
}
```

## Phase 5: Migration

### Step 5.1: Update All Tests
- Modify semantic tests to use new HIR
- Update HIR validation to work with instructions

### Step 5.2: Remove Old HIR
- Delete `hir.rs`
- Rename `hir2.rs` to `hir.rs`
- Update all imports

## Implementation Order

**Day 1**:
- [ ] Create `hir2.rs` with data structures
- [ ] Implement `HirBuilder` with basic instructions
- [ ] Write unit tests for builder

**Day 2**:
- [ ] Create `type_checker2.rs` 
- [ ] Modify to output instructions
- [ ] Create parallel test path

**Day 3**:
- [ ] Update MIR lowering
- [ ] Performance benchmarks
- [ ] Fix failing tests

**Day 4**:
- [ ] Complete migration
- [ ] Remove old HIR
- [ ] Documentation update

## Key Files to Modify

1. `crates/rue-ir/src/lib.rs` - Add hir2 module
2. `crates/rue-ir/src/hir2.rs` - New HIR structure
3. `crates/rue-semantic/src/type_checker.rs` - Adapt to output instructions
4. `crates/rue-lowering/src/lib.rs` - Add HIR2 to MIR lowering
5. `crates/rue-compiler/src/pipeline.rs` - Update compilation pipeline

## Testing Checklist

- [ ] All 67 semantic tests pass
- [ ] HIR validation works with instructions
- [ ] MIR output identical for test programs
- [ ] Performance improvement measured
- [ ] No memory leaks
- [ ] Incremental compilation ready

## Success Metrics

1. **Memory**: 4x reduction in HIR memory usage
2. **Speed**: 2x faster HIR traversal
3. **Cache**: 50% reduction in cache misses
4. **Code**: Simpler optimization passes