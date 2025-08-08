# Implementation Details: Data-Driven HIR

## Detailed Instruction Specifications

### Instruction Index Encoding
```rust
// All indices are u32, with 0 reserved for "none"
pub struct InstIndex(u32);
pub struct BlockIndex(u32);
pub struct TypeIndex(u32);
pub struct StringIndex(u32);

impl InstIndex {
    pub const NONE: u32 = 0;
    
    pub fn new(index: u32) -> Option<Self> {
        if index == 0 { None } else { Some(InstIndex(index)) }
    }
}
```

### Complete Instruction Set

#### Declaration Instructions
```rust
Function    // data.lhs = name_string, data.rhs = body_block, extra = [param_count, param_types...]
Param       // data.lhs = name_string, data.rhs = type_index
```

#### Block Instructions
```rust
Block       // data.lhs = first_inst, data.rhs = inst_count
BlockParam  // data.lhs = param_index, data.rhs = type_index
```

#### Statement Instructions
```rust
Let         // data.lhs = name_string, data.rhs = init_expr
Assign      // data.lhs = target_expr, data.rhs = value_expr
Return      // data.lhs = value_expr (or NONE)
While       // data.lhs = cond_expr, data.rhs = body_block
Break       // data.lhs = NONE, data.rhs = NONE
Continue    // data.lhs = NONE, data.rhs = NONE
```

#### Expression Instructions
```rust
Literal     // data.lhs = literal_index, data.rhs = type_index
Load        // data.lhs = name_string, data.rhs = type_index
Binary      // data.lhs = left_expr, data.rhs = right_expr, extra = [op]
Unary       // data.lhs = operand_expr, data.rhs = NONE, extra = [op]
Call        // data.lhs = func_string, data.rhs = extra_index, extra = [arg_count, args...]
Cast        // data.lhs = expr, data.rhs = target_type
If          // data.lhs = extra_index, extra = [cond, then_block, else_block]
```

#### Aggregate Instructions
```rust
StructLit   // data.lhs = type_index, data.rhs = extra_index, extra = [field_count, (field_name, field_expr)...]
TupleLit    // data.lhs = type_index, data.rhs = extra_index, extra = [elem_count, elems...]
ArrayLit    // data.lhs = type_index, data.rhs = extra_index, extra = [elem_count, elems...]
FieldAccess // data.lhs = base_expr, data.rhs = field_string
IndexAccess // data.lhs = base_expr, data.rhs = index_expr
```

### Type System Integration

```rust
// Types stored in parallel array
pub struct TypeArray {
    types: Vec<Option<RueType>>,  // None for statements
}

impl Hir {
    pub fn get_type(&self, inst: InstIndex) -> Option<&RueType> {
        self.types.get(inst.0 as usize).and_then(|t| t.as_ref())
    }
}
```

### Builder Pattern Implementation

```rust
pub struct HirBuilder {
    hir: Hir,
    current_block: Option<BlockIndex>,
    scope_stack: Vec<Scope>,
}

impl HirBuilder {
    // Example: Building a binary expression
    pub fn build_binary(&mut self, lhs: InstIndex, rhs: InstIndex, op: BinOp) -> InstIndex {
        let lhs_type = self.hir.get_type(lhs).unwrap();
        let rhs_type = self.hir.get_type(rhs).unwrap();
        let result_type = self.infer_binary_type(lhs_type, rhs_type, op);
        
        let index = self.next_inst_index();
        self.hir.instructions.tags.push(InstTag::Binary);
        self.hir.instructions.data.push(InstData {
            lhs: lhs.0,
            rhs: rhs.0,
        });
        self.hir.extra_data.push(op as u32);
        self.hir.types.push(Some(result_type));
        self.hir.spans.push(self.current_span());
        
        InstIndex(index)
    }
    
    // Building a block
    pub fn start_block(&mut self) -> BlockIndex {
        let block_index = self.hir.blocks.len() as u32;
        self.hir.blocks.push(BlockData {
            first_inst: self.next_inst_index(),
            inst_count: 0,
        });
        self.current_block = Some(BlockIndex(block_index));
        BlockIndex(block_index)
    }
    
    pub fn end_block(&mut self) {
        if let Some(block_idx) = self.current_block {
            let block = &mut self.hir.blocks[block_idx.0 as usize];
            block.inst_count = self.next_inst_index() - block.first_inst;
        }
        self.current_block = None;
    }
}
```

## TypeChecker Adaptation

### Current TypeChecker (CST-based)
```rust
impl TypeChecker {
    fn check_expression(&mut self, expr: &CstExpr) -> Result<HirExpr, Error> {
        match expr {
            CstExpr::Binary { lhs, rhs, op } => {
                let lhs_expr = Box::new(self.check_expression(lhs)?);
                let rhs_expr = Box::new(self.check_expression(rhs)?);
                Ok(HirExpr::Binary { lhs: lhs_expr, rhs: rhs_expr, op, ty, span })
            }
        }
    }
}
```

### New TypeChecker (Instruction-based)
```rust
impl TypeChecker {
    fn check_expression(&mut self, expr: &CstExpr, builder: &mut HirBuilder) -> Result<InstIndex, Error> {
        match expr {
            CstExpr::Binary { lhs, rhs, op } => {
                let lhs_inst = self.check_expression(lhs, builder)?;
                let rhs_inst = self.check_expression(rhs, builder)?;
                Ok(builder.build_binary(lhs_inst, rhs_inst, op))
            }
        }
    }
}
```

## MIR Lowering Adaptation

### Current (Tree-based HIR)
```rust
fn lower_expr(expr: &HirExpr) -> Vec<MirInst> {
    match expr {
        HirExpr::Binary { lhs, rhs, op, .. } => {
            let mut insts = vec![];
            insts.extend(lower_expr(lhs));
            insts.extend(lower_expr(rhs));
            insts.push(MirInst::Binary(*op));
            insts
        }
    }
}
```

### New (Instruction-based HIR)
```rust
fn lower_instructions(hir: &Hir) -> Vec<MirInst> {
    let mut mir_insts = vec![];
    
    for (i, tag) in hir.instructions.tags.iter().enumerate() {
        match tag {
            InstTag::Binary => {
                let data = hir.instructions.data[i];
                let op = BinOp::from_u32(hir.extra_data[...]);
                // Instructions already in correct order!
                mir_insts.push(MirInst::Binary(op));
            }
        }
    }
    
    mir_insts
}
```

## Memory Layout Optimization

### Alignment and Padding
```rust
// Ensure optimal alignment for SIMD
#[repr(C, align(16))]
pub struct InstructionList {
    tags: Vec<InstTag>,      // 1 byte each, but vec is 16-byte aligned
    data: Vec<InstData>,     // 8 bytes each
}

// Pack instruction data tightly
#[repr(C)]
pub struct InstData {
    lhs: u32,  // 4 bytes
    rhs: u32,  // 4 bytes
    // Total: 8 bytes, no padding
}
```

### Cache Line Optimization
```
Cache line (64 bytes) can hold:
- 64 instruction tags, or
- 8 InstData entries, or  
- 16 u32 extra_data entries

With parallel arrays, processing 8 instructions touches:
- 1/8 cache line for tags
- 1 cache line for data
- Variable for extra_data (sparse access)
```

## Error Handling

### Instruction Validation
```rust
impl Hir {
    pub fn validate(&self) -> Result<(), ValidationError> {
        for (i, tag) in self.instructions.tags.iter().enumerate() {
            self.validate_instruction(i, *tag)?;
        }
        Ok(())
    }
    
    fn validate_instruction(&self, index: usize, tag: InstTag) -> Result<(), ValidationError> {
        let data = self.instructions.data.get(index)
            .ok_or(ValidationError::MissingData(index))?;
            
        match tag {
            InstTag::Binary => {
                self.validate_inst_ref(InstIndex(data.lhs))?;
                self.validate_inst_ref(InstIndex(data.rhs))?;
            }
            // ... other validations
        }
        Ok(())
    }
}
```

## Debug Support

### Pretty Printing
```rust
impl fmt::Display for Hir {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        for (i, tag) in self.instructions.tags.iter().enumerate() {
            write!(f, "{:04}: ", i)?;
            self.format_instruction(f, i, *tag)?;
            writeln!(f)?;
        }
        Ok(())
    }
}

// Example output:
// 0000: Function "main" -> Block(1)
// 0001: Block [2..5]
// 0002: Let "x" = Inst(3)
// 0003: Literal 42 : I32
// 0004: Return Inst(3)
```

### Source Mapping
```rust
impl Hir {
    pub fn get_span(&self, inst: InstIndex) -> Span {
        self.spans[inst.0 as usize]
    }
    
    pub fn get_source_location(&self, inst: InstIndex) -> (usize, usize) {
        let span = self.get_span(inst);
        // Convert to line:column
        self.source_map.span_to_location(span)
    }
}
```

## Testing Strategy

### Equivalence Testing
```rust
#[test]
fn test_hir_equivalence() {
    let source = "fn main() { let x = 1 + 2; x }";
    
    // Old path
    let old_hir = analyze_cst_old(parse(source));
    let old_mir = lower_hir_old(&old_hir);
    
    // New path
    let new_hir = analyze_cst_new(parse(source));
    let new_mir = lower_hir_new(&new_hir);
    
    assert_eq!(old_mir, new_mir);
}
```

### Performance Testing
```rust
#[bench]
fn bench_hir_traversal(b: &mut Bencher) {
    let hir = create_large_hir();
    
    b.iter(|| {
        let mut sum = 0;
        for (i, tag) in hir.instructions.tags.iter().enumerate() {
            if *tag == InstTag::Literal {
                sum += hir.instructions.data[i].lhs;
            }
        }
        sum
    });
}
```

## Common Patterns

### Iterating Instructions
```rust
for (i, tag) in hir.instructions.tags.iter().enumerate() {
    let data = &hir.instructions.data[i];
    let ty = hir.types.get(i).and_then(|t| t.as_ref());
    // Process instruction
}
```

### Finding Functions
```rust
impl Hir {
    pub fn find_function(&self, name: &str) -> Option<InstIndex> {
        let name_idx = self.strings.lookup(name)?;
        
        for (i, tag) in self.instructions.tags.iter().enumerate() {
            if *tag == InstTag::Function {
                let data = &self.instructions.data[i];
                if data.lhs == name_idx.0 {
                    return Some(InstIndex(i as u32));
                }
            }
        }
        None
    }
}
```

### Walking Blocks
```rust
impl Hir {
    pub fn walk_block<F>(&self, block: BlockIndex, mut f: F) 
    where F: FnMut(InstIndex, InstTag, &InstData)
    {
        let block_data = &self.blocks[block.0 as usize];
        let start = block_data.first_inst as usize;
        let end = start + block_data.inst_count as usize;
        
        for i in start..end {
            let tag = self.instructions.tags[i];
            let data = &self.instructions.data[i];
            f(InstIndex(i as u32), tag, data);
        }
    }
}
```