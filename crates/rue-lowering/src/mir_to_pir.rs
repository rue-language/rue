//! MIR to PIR lowering
//!
//! This module converts MIR (in SSA form with block parameters) to the
//! platform-independent representation (PIR) with virtual registers.

use rue_ir::mir::{
    BasicBlock, BlockId, MirBinOp, MirConst, MirFunction, MirProgram, MirStatement, MirTerminator,
    MirUnaryOp, MirValue, Temp,
};
use rue_ir::pir::{BinOp, Label, PIR, PhysicalRegId, VReg, Value};
use rue_ir::types::{FieldId, RueType, TypeContext, TypeError};
use std::collections::HashMap;
use tracing::{debug, error, trace, warn};

/// Memory allocation strategy for aggregates
#[derive(Debug, Clone, Copy, PartialEq)]
enum AllocationStrategy {
    /// Zero-sized aggregates need no allocation
    ZeroSized,
    /// Small aggregates allocated on stack
    Stack,
    /// Large aggregates allocated on heap
    Heap,
}

/// Lowers MIR to PIR
pub struct MirToPir {
    /// Generated PIR instructions
    instructions: Vec<PIR>,
    /// Counter for virtual registers
    vreg_counter: u32,
    /// Counter for labels
    label_counter: u32,
    /// Mapping from MIR temps to virtual registers
    temp_to_vreg: HashMap<Temp, VReg>,
    /// Mapping from block IDs to labels
    block_to_label: HashMap<BlockId, Label>,
    /// Function labels for calls
    function_labels: HashMap<String, Label>,
    /// Current function blocks (needed for block parameter lookup)
    current_blocks: Vec<BasicBlock>,
    /// Mapping from (block_id, param_index) to VReg for block parameters
    block_param_vregs: HashMap<(BlockId, usize), VReg>,
    /// Mapping from (block_id, param_index) to dedicated stack offset for block parameters
    /// These are allocated at function entry to ensure consistent locations
    block_param_slots: HashMap<(BlockId, usize), i64>,
    /// Current stack offset for allocating block parameter slots
    block_param_stack_offset: i64,
    /// Stack offset per function (function name -> lowest stack offset used)
    function_stack_offsets: HashMap<String, i64>,
    /// Type information for the current function's temporaries
    current_temp_types: HashMap<Temp, rue_ir::types::RueType>,
    /// Type context for looking up struct definitions
    type_context: TypeContext,
    /// Function signatures for return type lookup during calls
    function_signatures: HashMap<String, rue_ir::mir::FunctionSignature>,
}

impl Default for MirToPir {
    fn default() -> Self {
        Self::new()
    }
}

impl MirToPir {
    pub fn new() -> Self {
        Self {
            instructions: Vec::new(),
            vreg_counter: 0,
            label_counter: 0,
            temp_to_vreg: HashMap::new(),
            block_to_label: HashMap::new(),
            function_labels: HashMap::new(),
            current_blocks: Vec::new(),
            block_param_vregs: HashMap::new(),
            block_param_slots: HashMap::new(),
            block_param_stack_offset: -8, // Start after saved RBP
            function_stack_offsets: HashMap::new(),
            current_temp_types: HashMap::new(),
            type_context: TypeContext::new(),
            function_signatures: HashMap::new(),
        }
    }

    /// Create a new MirToPir with a given type context
    pub fn with_type_context(type_context: TypeContext) -> Self {
        Self {
            type_context,
            ..Self::new()
        }
    }

    /// Create a new MirToPir with type context and function signatures
    pub fn with_type_context_and_signatures(
        type_context: TypeContext,
        function_signatures: HashMap<String, rue_ir::mir::FunctionSignature>,
    ) -> Self {
        Self {
            type_context,
            function_signatures,
            ..Self::new()
        }
    }

    /// Generate a fresh virtual register
    fn fresh_vreg(&mut self) -> VReg {
        let vreg = VReg(self.vreg_counter);
        self.vreg_counter += 1;
        vreg
    }

    /// Generate a fresh label
    fn fresh_label(&mut self) -> Label {
        let label = Label::user(self.label_counter);
        self.label_counter += 1;
        label
    }

    /// Get or create a virtual register for a temp
    fn get_vreg(&mut self, temp: Temp) -> VReg {
        if let Some(&vreg) = self.temp_to_vreg.get(&temp) {
            trace!(target: "rue::codegen::regalloc", ?temp, ?vreg, "get_vreg: temp -> existing vreg");
            vreg
        } else {
            let vreg = self.fresh_vreg();
            self.temp_to_vreg.insert(temp, vreg);
            trace!(target: "rue::codegen::regalloc", ?temp, ?vreg, "get_vreg: temp -> new vreg");
            vreg
        }
    }

    /// Get or create a label for a block
    fn get_label(&mut self, block: BlockId) -> Label {
        if let Some(&label) = self.block_to_label.get(&block) {
            label
        } else {
            let label = self.fresh_label();
            self.block_to_label.insert(block, label);
            label
        }
    }

    /// Emit a PIR instruction
    fn emit(&mut self, instr: PIR) {
        debug!(target: "rue::codegen::instructions", ?instr, "Emitting PIR instruction");
        self.instructions.push(instr);
    }

    /// Lower a MIR program to PIR
    pub fn lower_program(&mut self, program: &MirProgram) -> Vec<PIR> {
        // Store function signatures for call handling
        self.function_signatures = program.function_signatures.clone();
        self.type_context = program.type_context.clone();

        // First pass: collect all function labels
        for func in &program.functions {
            let label = self.fresh_label();
            self.function_labels.insert(func.name.clone(), label);
        }

        // Generate code for all functions
        for func in &program.functions {
            self.lower_function(func);
        }

        self.instructions.clone()
    }

    /// Get the function labels mapping
    pub fn get_function_labels(&self) -> HashMap<String, Label> {
        self.function_labels.clone()
    }

    /// Get the stack offset for a specific function
    pub fn get_function_stack_offset(&self, function_name: &str) -> i64 {
        *self
            .function_stack_offsets
            .get(function_name)
            .unwrap_or(&-8)
    }

    /// Get all block parameter offsets for proper tracking in lowering
    pub fn get_block_param_offsets(&self) -> std::collections::HashSet<i64> {
        self.block_param_slots.values().copied().collect()
    }

    /// Lower a MIR function to PIR
    fn lower_function(&mut self, func: &MirFunction) {
        // Clear temp mappings for new function
        self.temp_to_vreg.clear();
        self.block_to_label.clear();
        self.block_param_vregs.clear();
        self.block_param_slots.clear();
        self.block_param_stack_offset = -8; // Reset for new function
        // Note: We don't clear return_param_vregs because it needs to accumulate
        // across all functions in the program
        self.current_blocks = func.blocks.clone();
        self.current_temp_types = func.temp_types.clone();

        // CRITICAL: Pre-allocate stack slots for ALL block parameters
        // This ensures consistent memory locations across all predecessor blocks
        for block in &func.blocks {
            // Skip entry block - its parameters come from calling convention
            if block.id == func.entry_block {
                continue;
            }

            // Allocate a stack slot for each block parameter
            for (i, (_param_temp, _param_ty)) in block.params.iter().enumerate() {
                self.block_param_stack_offset -= 8; // Each parameter is 8 bytes
                self.block_param_slots
                    .insert((block.id, i), self.block_param_stack_offset);

                debug!(
                    target: "rue::codegen",
                    block = ?block.id,
                    param_index = i,
                    offset = self.block_param_stack_offset,
                    "Allocated block param slot"
                );
            }
        }

        // Function label
        let func_label = self.function_labels[&func.name];
        self.emit(PIR::Label(func_label));

        // Function prologue
        self.emit(PIR::EnterFrame);

        // Handle function parameters from calling convention registers
        // Map to abstract register IDs following x86-64 calling convention
        let param_registers = [
            PhysicalRegId(0), // Rdi - First parameter register
            PhysicalRegId(1), // Rsi - Second parameter register
            PhysicalRegId(2), // Rdx - Third parameter register
            PhysicalRegId(3), // Rcx - Fourth parameter register
            PhysicalRegId(4), // R8 - Fifth parameter register
            PhysicalRegId(5), // R9 - Sixth parameter register
        ];

        // Map entry block parameters to function parameters
        if let Some(entry_block) = func.blocks.iter().find(|b| b.id == func.entry_block) {
            for (i, (temp, _ty)) in entry_block.params.iter().enumerate() {
                if i < param_registers.len() {
                    let vreg = self.get_vreg(*temp);
                    self.emit(PIR::Copy {
                        dest: vreg,
                        src: Value::PhysicalReg(param_registers[i]),
                    });
                }
            }
        }

        // Generate labels for all blocks
        for block in &func.blocks {
            self.get_label(block.id);
        }

        // Lower each block
        for block in &func.blocks {
            self.lower_block(block, func);
        }

        // Store the lowest stack offset used by this function
        self.function_stack_offsets
            .insert(func.name.clone(), self.block_param_stack_offset);
    }

    /// Lower a basic block
    fn lower_block(&mut self, block: &BasicBlock, func: &MirFunction) {
        // Emit block label
        let label = self.get_label(block.id);
        self.emit(PIR::Label(label));

        // CRITICAL: Load block parameters from their dedicated stack slots
        // This is the "always spill" approach - parameters are always in memory
        if block.id != func.entry_block && !block.params.is_empty() {
            for (i, (param_temp, _ty)) in block.params.iter().enumerate() {
                // Get or create VReg for this parameter
                let param_vreg = self.get_vreg(*param_temp);

                // Get the pre-allocated stack slot for this block parameter
                if let Some(&slot_offset) = self.block_param_slots.get(&(block.id, i)) {
                    // Load from the dedicated stack slot
                    self.emit(PIR::Load {
                        dest: param_vreg,
                        offset: slot_offset,
                    });

                    debug!(
                        target: "rue::codegen",
                        block = ?block.id,
                        param_index = i,
                        ?param_temp,
                        offset = slot_offset,
                        ?param_vreg,
                        "Loading block param from stack"
                    );
                }
            }
        }

        // Lower statements
        for stmt in &block.statements {
            self.lower_statement(stmt);
        }

        // Lower terminator
        self.lower_terminator(&block.terminator, func);
    }

    /// Lower a MIR statement
    fn lower_statement(&mut self, stmt: &MirStatement) {
        match stmt {
            MirStatement::Assign { dest, value, .. } => {
                let dest_vreg = self.get_vreg(*dest);
                self.lower_value(dest_vreg, value);
            }
            MirStatement::Return { value, .. } => {
                let return_vreg = value.as_ref().map(|temp| self.get_vreg(*temp));
                self.emit(PIR::Return { value: return_vreg });
            }
        }
    }

    /// Lower a MIR value, storing result in dest_vreg
    fn lower_value(&mut self, dest: VReg, value: &MirValue) {
        match value {
            MirValue::Use(temp) => {
                let src_vreg = self.get_vreg(*temp);
                self.emit(PIR::Copy {
                    dest,
                    src: Value::VReg(src_vreg),
                });
            }
            MirValue::Const(c) => {
                let imm = match c {
                    MirConst::Int32(n) => *n as i64,
                    MirConst::Int64(n) => *n,
                    MirConst::Bool(b) => {
                        if *b {
                            1
                        } else {
                            0
                        }
                    }
                    MirConst::Unit => 0,
                    MirConst::Aggregate { ty, fields } => {
                        // Lower aggregate constant using enhanced method
                        self.lower_aggregate_constant(dest, ty, fields);
                        return; // Already handled, don't emit additional Copy instruction
                    }
                };
                self.emit(PIR::Copy {
                    dest,
                    src: Value::SignedImm(imm),
                });
            }
            MirValue::BinaryOp { op, lhs, rhs } => {
                let lhs_vreg = self.get_vreg(*lhs);
                let rhs_vreg = self.get_vreg(*rhs);

                let instr_op = match op {
                    MirBinOp::Add => BinOp::Add,
                    MirBinOp::Sub => BinOp::Sub,
                    MirBinOp::Mul => BinOp::Mul,
                    MirBinOp::Div => BinOp::Div,
                    MirBinOp::Mod => BinOp::Mod,
                    MirBinOp::Lt => BinOp::Lt,
                    MirBinOp::Le => BinOp::Le,
                    MirBinOp::Gt => BinOp::Gt,
                    MirBinOp::Ge => BinOp::Ge,
                    MirBinOp::Eq => BinOp::Eq,
                    MirBinOp::Ne => BinOp::Ne,
                };

                // Check if we have type information for the LHS operand
                // For arithmetic operations, the result type should match the operand type
                if let Some(lhs_ty) = self.current_temp_types.get(lhs) {
                    // Use typed binary operation when type information is available
                    self.emit(PIR::TypedBinaryOp {
                        dest,
                        lhs: Value::VReg(lhs_vreg),
                        rhs: Value::VReg(rhs_vreg),
                        op: instr_op,
                        ty: lhs_ty.clone(),
                    });
                } else {
                    // Fall back to untyped operation
                    self.emit(PIR::BinaryOp {
                        dest,
                        lhs: Value::VReg(lhs_vreg),
                        rhs: Value::VReg(rhs_vreg),
                        op: instr_op,
                    });
                }
            }
            MirValue::UnaryOp { op, operand } => {
                let operand_vreg = self.get_vreg(*operand);

                match op {
                    MirUnaryOp::Neg => {
                        // Implement negation as 0 - operand
                        let zero = self.fresh_vreg();
                        self.emit(PIR::Copy {
                            dest: zero,
                            src: Value::SignedImm(0),
                        });
                        self.emit(PIR::BinaryOp {
                            dest,
                            lhs: Value::VReg(zero),
                            rhs: Value::VReg(operand_vreg),
                            op: BinOp::Sub,
                        });
                    }
                }
            }
            MirValue::Call { func, args, .. } => {
                let arg_vregs: Vec<VReg> = args.iter().map(|&arg| self.get_vreg(arg)).collect();
                // Look up return type from function signatures
                let return_type = self
                    .function_signatures
                    .get(func)
                    .map(|sig| sig.return_type.clone());

                // Compute return size for aggregates
                let return_size = if let Some(ref ret_type) = return_type {
                    if self.is_aggregate_type(ret_type) {
                        Some(self.compute_type_size(ret_type))
                    } else {
                        None
                    }
                } else {
                    None
                };

                self.emit(PIR::Call {
                    dest: Some(dest),
                    function: func.clone(),
                    args: arg_vregs,
                    return_type,
                    return_size,
                });
            }
            // Aggregate operations
            MirValue::ConstructAggregate { ty, fields } => {
                self.lower_construct_aggregate(dest, ty, fields);
            }
            MirValue::GetField { base, field } => {
                self.lower_get_field(dest, *base, field);
            }
            MirValue::SetField { base, field, value } => {
                self.lower_set_field(dest, *base, field, *value);
            }
            MirValue::StructUpdate {
                base,
                updates,
                struct_type,
            } => {
                self.lower_struct_update(dest, *base, updates, *struct_type);
            }
            MirValue::DynamicArrayAccess { base, index } => {
                self.lower_dynamic_array_access(dest, *base, *index);
            }
        }
    }

    /// Lower a MIR terminator
    fn lower_terminator(&mut self, term: &MirTerminator, func: &MirFunction) {
        match term {
            MirTerminator::Goto { target, args, .. } => {
                // Handle block arguments by copying to block parameters
                self.lower_block_arguments(*target, args, func);

                let label = self.get_label(*target);
                self.emit(PIR::Jump(label));
            }
            MirTerminator::Branch {
                condition,
                then_block,
                then_args,
                else_block,
                else_args,
                ..
            } => {
                // Note: We can't reliably force spill here because the register allocator
                // may optimize it away. The fix is in lower_block_arguments.

                let cond_vreg = self.get_vreg(*condition);

                // We need to handle block arguments differently for each branch
                // Generate intermediate blocks to handle the arguments

                let then_label = self.fresh_label();
                let else_label = self.fresh_label();
                let then_target = self.get_label(*then_block);
                let else_target = self.get_label(*else_block);

                // Emit the branch
                self.emit(PIR::Branch {
                    condition: cond_vreg,
                    true_label: then_label,
                    false_label: else_label,
                });

                // Then branch: copy arguments and jump
                self.emit(PIR::Label(then_label));
                self.lower_block_arguments(*then_block, then_args, func);
                self.emit(PIR::Jump(then_target));

                // Else branch: copy arguments and jump
                self.emit(PIR::Label(else_label));
                self.lower_block_arguments(*else_block, else_args, func);
                self.emit(PIR::Jump(else_target));
            }
            MirTerminator::Switch {
                discriminant,
                targets,
                default,
                default_args,
                ..
            } => {
                // For now, implement switch as a chain of conditional branches
                // This can be optimized later to use jump tables for dense switches
                let discriminant_vreg = self.get_vreg(*discriminant);
                let default_label = self.get_label(*default);

                // Create intermediate labels for each case
                let mut case_labels = Vec::new();
                for _ in targets {
                    case_labels.push(self.fresh_label());
                }

                // Generate comparisons and branches for each case
                for (i, (value, target_block, target_args)) in targets.iter().enumerate() {
                    let case_label = case_labels[i];
                    let target_label = self.get_label(*target_block);

                    // Compare discriminant with case value
                    let temp_vreg = self.fresh_vreg();
                    self.emit(PIR::Copy {
                        dest: temp_vreg,
                        src: Value::SignedImm(*value),
                    });

                    let cmp_result = self.fresh_vreg();
                    self.emit(PIR::BinaryOp {
                        dest: cmp_result,
                        lhs: Value::VReg(discriminant_vreg),
                        rhs: Value::VReg(temp_vreg),
                        op: BinOp::Eq,
                    });

                    // Branch to case or continue to next check
                    let next_check_label = if i + 1 < targets.len() {
                        self.fresh_label()
                    } else {
                        default_label
                    };

                    self.emit(PIR::Branch {
                        condition: cmp_result,
                        true_label: case_label,
                        false_label: next_check_label,
                    });

                    // Case block: handle arguments and jump to target
                    self.emit(PIR::Label(case_label));
                    self.lower_block_arguments(*target_block, target_args, func);
                    self.emit(PIR::Jump(target_label));

                    // Continue to next check if not last case
                    if i + 1 < targets.len() {
                        self.emit(PIR::Label(next_check_label));
                    }
                }

                // Default case
                self.emit(PIR::Label(default_label));
                self.lower_block_arguments(*default, default_args, func);
                let default_target = self.get_label(*default);
                self.emit(PIR::Jump(default_target));
            }
            MirTerminator::Unreachable { .. } => {
                // Emit a trap instruction or halt for unreachable code
                // For now, we'll emit a comment and a jump to itself (infinite loop)
                // This should never be reached during normal execution
                let unreachable_label = self.fresh_label();
                self.emit(PIR::Label(unreachable_label));
                self.emit(PIR::Jump(unreachable_label)); // Infinite loop
            }
            MirTerminator::Return { value, .. } => {
                if let Some(val) = value {
                    let vreg = self.get_vreg(*val);
                    debug!(target: "rue::codegen", ?val, ?vreg, "Return terminator: temp -> vreg");

                    // Check if this is an aggregate return
                    if let Some(return_type) = self.current_temp_types.get(val).cloned() {
                        if self.is_aggregate_type(&return_type) {
                            let size = self.compute_type_size(&return_type);
                            debug!(
                                target: "rue::codegen",
                                ?return_type,
                                size,
                                "Returning aggregate type"
                            );

                            if size <= 16 {
                                // Small aggregate: return in registers (RAX, RDX)
                                self.emit(PIR::ReturnSmallAggregate { value: vreg, size });
                            } else {
                                // Large aggregate: should use hidden return pointer
                                // For now, fall back to regular return and handle in x86 backend
                                self.emit(PIR::ReturnLargeAggregate { value: vreg, size });
                            }
                        } else {
                            // Scalar return value
                            self.emit(PIR::Return { value: Some(vreg) });
                        }
                    } else {
                        // No type information available, assume scalar
                        warn!(
                            target: "rue::codegen",
                            ?val,
                            "No type information for return value, assuming scalar"
                        );
                        self.emit(PIR::Return { value: Some(vreg) });
                    }
                } else {
                    self.emit(PIR::Return { value: None });
                }
            }
            MirTerminator::Uninit => {
                panic!(
                    "Encountered Uninit terminator during MIR to PIR lowering - this indicates a lowering bug"
                );
            }
        }
    }

    /// Handle block arguments when jumping to a block
    fn lower_block_arguments(&mut self, target_block: BlockId, args: &[Temp], _func: &MirFunction) {
        // Skip if this is the entry block (no block parameters to handle)
        if target_block == BlockId(0) {
            return;
        }

        // CRITICAL: "Always spill" approach for block parameters
        // Store each argument value to its dedicated stack slot
        // The target block will load these values after its label

        for (i, &arg_temp) in args.iter().enumerate() {
            // Get the pre-allocated stack slot for this block parameter
            if let Some(&slot_offset) = self.block_param_slots.get(&(target_block, i)) {
                // Get VReg for the argument value
                let arg_vreg = self.get_vreg(arg_temp);

                trace!(
                    target: "rue::mir::blocks",
                    ?target_block,
                    arg_index = i,
                    ?arg_temp,
                    ?arg_vreg,
                    offset = slot_offset,
                    "About to store block arg to stack"
                );

                // Store the argument value to the dedicated slot
                self.emit(PIR::Store {
                    src: arg_vreg,
                    offset: slot_offset,
                });

                debug!(
                    target: "rue::mir::blocks",
                    ?target_block,
                    arg_index = i,
                    ?arg_temp,
                    ?arg_vreg,
                    offset = slot_offset,
                    "Storing block arg to stack"
                );
                // Also log what instruction we just emitted
                if let Some(last_inst) = self.instructions.last() {
                    trace!(target: "rue::mir::blocks", ?last_inst, "Last emitted instruction");
                }
            }
        }
    }

    /// Helper function to compute size of a type in bytes using proper layout
    fn compute_type_size(&mut self, ty: &RueType) -> i64 {
        match self.type_context.compute_layout(ty) {
            Ok(layout) => layout.size as i64,
            Err(e) => {
                warn!("Failed to compute type size for {:?}: {}", ty, e);
                // Conservative fallback based on type
                match ty {
                    RueType::I32 => 4,
                    RueType::I64 => 8,
                    RueType::Bool => 1,
                    RueType::Unit => 0,
                    _ => 8, // Conservative default
                }
            }
        }
    }

    /// Helper function to check if a type is an aggregate (struct, tuple, or array)
    fn is_aggregate_type(&self, ty: &RueType) -> bool {
        matches!(
            ty,
            RueType::Struct(_) | RueType::Tuple(_) | RueType::Array(_, _)
        )
    }

    /// Helper function to compute alignment of a type in bytes
    fn compute_type_alignment(&mut self, ty: &RueType) -> i64 {
        match self.type_context.compute_layout(ty) {
            Ok(layout) => layout.align as i64,
            Err(e) => {
                warn!("Failed to compute type alignment for {:?}: {}", ty, e);
                // Conservative fallback based on type
                match ty {
                    RueType::I32 => 4,
                    RueType::I64 => 8,
                    RueType::Bool => 1,
                    RueType::Unit => 1,
                    _ => 8, // Conservative default
                }
            }
        }
    }

    /// Memory allocation strategy for aggregates
    fn choose_allocation_strategy(size: usize) -> AllocationStrategy {
        const STACK_ALLOCATION_THRESHOLD: usize = 128;
        match size {
            0 => AllocationStrategy::ZeroSized,
            1..=STACK_ALLOCATION_THRESHOLD => AllocationStrategy::Stack,
            _ => AllocationStrategy::Heap,
        }
    }

    /// Optimization thresholds for inline aggregate operations
    const INLINE_COPY_THRESHOLD: i64 = 16; // Inline copy for ≤16 bytes
    const INLINE_ZERO_THRESHOLD: i64 = 8; // Inline zero for ≤8 bytes

    /// Helper function to compute field offset within an aggregate using proper type layout
    fn compute_field_offset(
        &mut self,
        base_ty: &RueType,
        field: &FieldId,
    ) -> Result<(i64, RueType), TypeError> {
        match (base_ty, field) {
            (RueType::Tuple(types), FieldId::Index(idx)) => {
                let offset = self.type_context.compute_tuple_field_offset(types, *idx)? as i64;
                let field_type = types
                    .get(*idx)
                    .ok_or(TypeError::OutOfBoundsAccess {
                        index: *idx,
                        len: types.len(),
                    })?
                    .clone();
                Ok((offset, field_type))
            }
            (RueType::Array(elem_ty, len), FieldId::Index(idx)) => {
                let offset = self
                    .type_context
                    .compute_array_element_offset(elem_ty, *len, *idx)?
                    as i64;
                Ok((offset, (**elem_ty).clone()))
            }
            (RueType::Struct(struct_id), field) => {
                // Use TypeContext to look up struct field information
                let offset = self.type_context.compute_field_offset(*struct_id, field)? as i64;
                let field_type = self.type_context.get_field_type(*struct_id, field)?.clone();
                Ok((offset, field_type))
            }
            _ => Err(TypeError::InvalidFieldAccess {
                ty: base_ty.clone(),
                field: field.clone(),
            }),
        }
    }

    /// Lower ConstructAggregate to PIR with optimized allocation strategy
    fn lower_construct_aggregate(&mut self, dest: VReg, ty: &RueType, fields: &[Temp]) {
        let size = self.compute_type_size(ty) as usize;
        let alignment = self.compute_type_alignment(ty);
        let strategy = Self::choose_allocation_strategy(size);

        // Handle allocation based on strategy
        match strategy {
            AllocationStrategy::ZeroSized => {
                // Zero-sized aggregates don't need actual allocation
                // Just assign a dummy pointer value
                self.emit(PIR::Copy {
                    dest,
                    src: Value::UnsignedImm(1), // Non-null dummy pointer
                });
                return; // No fields to store
            }
            AllocationStrategy::Stack => {
                // TODO: Implement stack allocation via stack frame management
                // For now, fall back to heap allocation
                self.emit(PIR::AllocateAggregate {
                    dest,
                    size: size as i64,
                    alignment,
                });
            }
            AllocationStrategy::Heap => {
                self.emit(PIR::AllocateAggregate {
                    dest,
                    size: size as i64,
                    alignment,
                });
            }
        }

        // Zero-initialize the allocated memory for safety
        if size > 0 {
            let size_i64 = size as i64;
            if size_i64 <= Self::INLINE_ZERO_THRESHOLD {
                // Use inline zero for small aggregates
                self.emit(PIR::InlineZeroAggregate {
                    dest,
                    size: size_i64,
                });
            } else {
                // Use runtime helper for larger aggregates
                self.emit(PIR::ZeroAggregate {
                    dest,
                    size: size_i64,
                });
            }
        }

        // Store each field value using proper layout computation
        self.store_aggregate_fields(dest, ty, fields);
    }

    /// Store fields into aggregate using proper layout
    fn store_aggregate_fields(&mut self, dest: VReg, ty: &RueType, fields: &[Temp]) {
        match ty {
            RueType::Tuple(field_types) => {
                for (idx, (field_temp, field_ty)) in
                    fields.iter().zip(field_types.iter()).enumerate()
                {
                    let field_vreg = self.get_vreg(*field_temp);
                    let offset = self
                        .type_context
                        .compute_tuple_field_offset(field_types, idx)
                        .unwrap_or_else(|e| {
                            warn!("Failed to compute tuple field offset: {}", e);
                            idx * 8 // Fallback to conservative offset
                        }) as i64;

                    self.emit(PIR::StoreField {
                        base: dest,
                        offset,
                        src: Value::VReg(field_vreg),
                        field_type: field_ty.clone(),
                    });
                }
            }
            RueType::Array(elem_ty, len) => {
                for (idx, field_temp) in fields.iter().enumerate() {
                    let field_vreg = self.get_vreg(*field_temp);
                    let offset = self
                        .type_context
                        .compute_array_element_offset(elem_ty, *len, idx)
                        .unwrap_or_else(|e| {
                            warn!("Failed to compute array element offset: {}", e);
                            idx * 8 // Fallback to conservative offset
                        }) as i64;

                    self.emit(PIR::StoreField {
                        base: dest,
                        offset,
                        src: Value::VReg(field_vreg),
                        field_type: (**elem_ty).clone(),
                    });
                }
            }
            RueType::Struct(struct_id) => {
                // Use TypeContext to compute proper field offsets and types
                for (idx, field_temp) in fields.iter().enumerate() {
                    let field_vreg = self.get_vreg(*field_temp);
                    let field_id = FieldId::Index(idx);

                    let (offset, field_type) = self
                        .type_context
                        .compute_field_offset(*struct_id, &field_id)
                        .and_then(|offset| {
                            self.type_context
                                .get_field_type(*struct_id, &field_id)
                                .map(|field_type| (offset as i64, field_type.clone()))
                        })
                        .unwrap_or_else(|e| {
                            warn!("Failed to compute struct field offset/type: {}", e);
                            (idx as i64 * 8, RueType::I64) // Fallback to conservative assumptions
                        });

                    self.emit(PIR::StoreField {
                        base: dest,
                        offset,
                        src: Value::VReg(field_vreg),
                        field_type,
                    });
                }
            }
            _ => {
                error!("Cannot construct aggregate of type {ty:?}");
                // Generate a dummy value to continue compilation
                self.emit(PIR::Copy {
                    dest,
                    src: Value::UnsignedImm(0),
                });
            }
        }
    }

    /// Lower GetField to PIR with proper type-aware offset computation
    fn lower_get_field(&mut self, dest: VReg, base: Temp, field: &FieldId) {
        let base_vreg = self.get_vreg(base);

        // Get the base type from our type information
        let base_type = self
            .current_temp_types
            .get(&base)
            .cloned()
            .unwrap_or(RueType::Unknown);

        let (offset, field_type) = if base_type != RueType::Unknown {
            // Use proper type-aware offset computation
            match self.compute_field_offset(&base_type, field) {
                Ok((offset, field_type)) => (offset, field_type),
                Err(e) => {
                    warn!("Failed to compute field offset: {}", e);
                    // Fall back to conservative assumptions
                    match field {
                        FieldId::Index(idx) => ((*idx as i64) * 8, RueType::I64),
                        FieldId::Named(_) => (0, RueType::I64),
                    }
                }
            }
        } else {
            warn!("Missing type information for temp {base:?}, using conservative assumptions");
            // Fall back to conservative assumptions
            match field {
                FieldId::Index(idx) => ((*idx as i64) * 8, RueType::I64),
                FieldId::Named(_) => (0, RueType::I64),
            }
        };

        self.emit(PIR::LoadField {
            dest,
            base: base_vreg,
            offset,
            field_type,
        });
    }

    /// Lower SetField to PIR with efficient functional update
    fn lower_set_field(&mut self, dest: VReg, base: Temp, field: &FieldId, value: Temp) {
        let base_vreg = self.get_vreg(base);
        let value_vreg = self.get_vreg(value);

        // Get the base type for proper size and layout computation
        let base_type = self
            .current_temp_types
            .get(&base)
            .cloned()
            .unwrap_or(RueType::Unknown);

        let (size, alignment) = if base_type != RueType::Unknown {
            (
                self.compute_type_size(&base_type),
                self.compute_type_alignment(&base_type),
            )
        } else {
            tracing::warn!(
                "Missing type information for temp {base:?}, using conservative assumptions"
            );
            (64, 8) // Conservative defaults
        };

        // Allocate a new aggregate for the functional update
        let strategy = Self::choose_allocation_strategy(size as usize);
        match strategy {
            AllocationStrategy::ZeroSized => {
                // Zero-sized aggregates don't need copying
                self.emit(PIR::Copy {
                    dest,
                    src: Value::UnsignedImm(1), // Non-null dummy pointer
                });
                return;
            }
            _ => {
                self.emit(PIR::AllocateAggregate {
                    dest,
                    size,
                    alignment,
                });
            }
        }

        // Copy the base aggregate to the new location
        if size <= Self::INLINE_COPY_THRESHOLD {
            // Use inline copy for small aggregates
            self.emit(PIR::InlineCopyAggregate {
                dest,
                src: base_vreg,
                size,
            });
        } else {
            // Use runtime helper for larger aggregates
            self.emit(PIR::CopyAggregate {
                dest,
                src: base_vreg,
                size,
            });
        }

        // Update the specific field with proper offset computation
        let (offset, field_type) = if base_type != RueType::Unknown {
            match self.compute_field_offset(&base_type, field) {
                Ok((offset, field_type)) => (offset, field_type),
                Err(e) => {
                    warn!("Failed to compute field offset in SetField: {}", e);
                    // Fall back to conservative assumptions
                    match field {
                        FieldId::Index(idx) => ((*idx as i64) * 8, RueType::I64),
                        FieldId::Named(_) => (0, RueType::I64),
                    }
                }
            }
        } else {
            // Fall back to conservative assumptions
            match field {
                FieldId::Index(idx) => ((*idx as i64) * 8, RueType::I64),
                FieldId::Named(_) => (0, RueType::I64),
            }
        };

        self.emit(PIR::StoreField {
            base: dest,
            offset,
            src: Value::VReg(value_vreg),
            field_type,
        });
    }

    /// Lower StructUpdate to PIR with efficient selective copying
    fn lower_struct_update(
        &mut self,
        dest: VReg,
        base: Temp,
        updates: &[(FieldId, Temp)],
        struct_type: rue_ir::types::StructId,
    ) {
        let base_vreg = self.get_vreg(base);
        let base_type = RueType::Struct(struct_type);
        let size = self.compute_type_size(&base_type);
        let alignment = self.compute_type_alignment(&base_type);

        // Allocate new struct
        let strategy = Self::choose_allocation_strategy(size as usize);
        match strategy {
            AllocationStrategy::ZeroSized => {
                self.emit(PIR::Copy {
                    dest,
                    src: Value::UnsignedImm(1),
                });
                return;
            }
            _ => {
                self.emit(PIR::AllocateAggregate {
                    dest,
                    size,
                    alignment,
                });
            }
        }

        // Copy the base struct to new location
        if size <= Self::INLINE_COPY_THRESHOLD {
            // Use inline copy for small aggregates
            self.emit(PIR::InlineCopyAggregate {
                dest,
                src: base_vreg,
                size,
            });
        } else {
            // Use runtime helper for larger aggregates
            self.emit(PIR::CopyAggregate {
                dest,
                src: base_vreg,
                size,
            });
        }

        // Apply all field updates
        for (field, value_temp) in updates {
            let value_vreg = self.get_vreg(*value_temp);
            let (offset, field_type) = match self.compute_field_offset(&base_type, field) {
                Ok((offset, field_type)) => (offset, field_type),
                Err(e) => {
                    warn!("Failed to compute field offset in StructUpdate: {}", e);
                    // Fall back to conservative assumptions
                    match field {
                        FieldId::Index(idx) => ((*idx as i64) * 8, RueType::I64),
                        FieldId::Named(_) => (0, RueType::I64),
                    }
                }
            };

            self.emit(PIR::StoreField {
                base: dest,
                offset,
                src: Value::VReg(value_vreg),
                field_type,
            });
        }
    }

    /// Lower dynamic array access with bounds checking
    fn lower_dynamic_array_access(&mut self, dest: VReg, base: Temp, index: Temp) {
        let base_vreg = self.get_vreg(base);
        let index_vreg = self.get_vreg(index);

        // Get the array type to determine element size and array length
        let base_type = self
            .current_temp_types
            .get(&base)
            .cloned()
            .unwrap_or(RueType::Unknown);

        let (element_type, array_len, element_size) = match &base_type {
            RueType::Array(elem_type, len) => {
                let elem_size = match self.type_context.compute_layout(elem_type) {
                    Ok(layout) => layout.size as i64,
                    Err(e) => {
                        warn!("Failed to compute element layout: {}", e);
                        8 // Conservative default
                    }
                };
                ((**elem_type).clone(), *len as u64, elem_size)
            }
            _ => {
                error!("Dynamic array access on non-array type: {base_type:?}");
                // Generate a dummy load to continue compilation
                self.emit(PIR::Copy {
                    dest,
                    src: Value::SignedImm(0),
                });
                return;
            }
        };

        // Create trap label for bounds violation
        let trap_label = self.fresh_label();

        trace!(
            target: "rue::mir::arrays",
            ?base_vreg,
            ?index_vreg,
            ?array_len,
            ?element_size,
            "Lowering dynamic array access with bounds checking"
        );

        // Create a label to continue after successful bounds check
        let continue_label = self.fresh_label();

        // Emit bounds check
        self.emit(PIR::ArrayBoundsCheck {
            array_base: base_vreg,
            index: index_vreg,
            array_len,
            trap_label,
        });

        // Emit dynamic load (bounds check ensures this is safe)
        self.emit(PIR::DynamicLoadField {
            dest,
            base: base_vreg,
            index: index_vreg,
            element_size,
            element_type,
        });

        // Jump over the trap code after successful load
        self.emit(PIR::Jump(continue_label));

        // Emit trap label and trap instruction
        self.emit(PIR::Label(trap_label));
        self.emit(PIR::Trap {
            message: format!("Array index out of bounds (array length: {array_len})"),
        });

        // Continue label for normal execution
        self.emit(PIR::Label(continue_label));
    }

    /// Lower aggregate constant construction
    fn lower_aggregate_constant(
        &mut self,
        dest: VReg,
        ty: &RueType,
        fields: &std::rc::Rc<[MirConst]>,
    ) {
        let size = self.compute_type_size(ty) as usize;
        let strategy = Self::choose_allocation_strategy(size);

        match strategy {
            AllocationStrategy::ZeroSized => {
                self.emit(PIR::Copy {
                    dest,
                    src: Value::UnsignedImm(1),
                });
                return;
            }
            _ => {
                let alignment = self.compute_type_alignment(ty);
                self.emit(PIR::AllocateAggregate {
                    dest,
                    size: size as i64,
                    alignment,
                });
            }
        }

        // Store constant field values
        match ty {
            RueType::Tuple(field_types) => {
                for (idx, (field_const, field_ty)) in
                    fields.iter().zip(field_types.iter()).enumerate()
                {
                    let offset = self
                        .type_context
                        .compute_tuple_field_offset(field_types, idx)
                        .unwrap_or_else(|e| {
                            warn!("Failed to compute tuple field offset in constant: {}", e);
                            idx * 8 // Fallback to conservative offset
                        }) as i64;

                    // Convert constant to immediate value
                    let immediate = self.constant_to_immediate(field_const);
                    self.emit(PIR::StoreField {
                        base: dest,
                        offset,
                        src: immediate,
                        field_type: field_ty.clone(),
                    });
                }
            }
            RueType::Array(elem_ty, len) => {
                for (idx, field_const) in fields.iter().enumerate() {
                    let offset = self
                        .type_context
                        .compute_array_element_offset(elem_ty, *len, idx)
                        .unwrap_or_else(|e| {
                            warn!("Failed to compute array element offset in constant: {}", e);
                            idx * 8 // Fallback to conservative offset  
                        }) as i64;

                    let immediate = self.constant_to_immediate(field_const);
                    self.emit(PIR::StoreField {
                        base: dest,
                        offset,
                        src: immediate,
                        field_type: (**elem_ty).clone(),
                    });
                }
            }
            _ => {
                error!("Unsupported aggregate constant type: {ty:?}");
                // Generate a dummy value to continue compilation
                self.emit(PIR::Copy {
                    dest,
                    src: Value::UnsignedImm(0),
                });
            }
        }
    }

    /// Helper to convert MIR constant to PIR immediate value
    fn constant_to_immediate(&self, constant: &MirConst) -> Value {
        match constant {
            MirConst::Int32(n) => Value::SignedImm(*n as i64),
            MirConst::Int64(n) => Value::SignedImm(*n),
            MirConst::Bool(b) => Value::SignedImm(if *b { 1 } else { 0 }),
            MirConst::Unit => Value::SignedImm(0),
            MirConst::Aggregate { .. } => {
                error!("Nested aggregate constants not supported in immediate values");
                Value::SignedImm(0) // Default to 0 for error recovery
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rue_ir::mir::{
        BasicBlock, BlockId, MirBinOp, MirFunction, MirProgram, MirStatement, MirTerminator,
        MirValue, Temp,
    };
    use rue_ir::pir::PIR;
    use rue_ir::types::RueType;

    #[test]
    fn test_aggregate_size_calculation() {
        let mut lowerer = MirToPir::new();

        // Test tuple size calculation
        let tuple_ty = RueType::Tuple(vec![RueType::I64, RueType::I32]);
        let tuple_size = lowerer.compute_type_size(&tuple_ty);

        // Tuple (i64, i32) has proper alignment:
        // - i64 at offset 0 (8 bytes)
        // - i32 at offset 8 (4 bytes)
        // - Total size 16 bytes due to 8-byte alignment requirement
        assert_eq!(
            tuple_size, 16,
            "Tuple (i64, i32) should be 16 bytes with proper alignment"
        );

        // Test array size calculation
        let array_ty = RueType::Array(Box::new(RueType::I32), 5);
        let array_size = lowerer.compute_type_size(&array_ty);
        assert_eq!(array_size, 20, "Array [i32; 5] should be 4 * 5 = 20 bytes");

        // Test struct size (will use fallback without proper TypeContext)
        let struct_ty = RueType::Struct(rue_ir::types::StructId::new(1));
        let struct_size = lowerer.compute_type_size(&struct_ty);
        assert_eq!(
            struct_size, 8,
            "Struct should use fallback size of 8 bytes without TypeContext"
        );
    }

    #[test]
    fn test_lower_construct_aggregate() {
        let mut lowerer = MirToPir::new();

        // Test tuple construction
        let tuple_ty = RueType::Tuple(vec![RueType::I64, RueType::I32]);
        let field_temps = vec![Temp(0), Temp(1)];
        let dest = VReg(0);

        // Ensure temps have vregs
        lowerer.get_vreg(Temp(0));
        lowerer.get_vreg(Temp(1));

        lowerer.lower_construct_aggregate(dest, &tuple_ty, &field_temps);

        // Should have generated AllocateAggregate + 2 StoreField instructions
        let instructions = &lowerer.instructions;
        assert!(!instructions.is_empty());

        // Find AllocateAggregate instruction
        let alloc_found = instructions.iter().any(|inst| {
            matches!(inst, PIR::AllocateAggregate { dest: d, size: 16, alignment: 8 } if *d == dest)
        });
        assert!(alloc_found, "Should generate AllocateAggregate instruction");

        // Find StoreField instructions
        let store_count = instructions
            .iter()
            .filter(|inst| matches!(inst, PIR::StoreField { .. }))
            .count();
        assert_eq!(
            store_count, 2,
            "Should generate 2 StoreField instructions for tuple fields"
        );
    }

    #[test]
    fn test_lower_get_field() {
        let mut lowerer = MirToPir::new();

        let dest = VReg(0);
        let base_temp = Temp(1);
        let field = FieldId::Index(0);

        // Ensure base temp has vreg
        lowerer.get_vreg(base_temp);

        lowerer.lower_get_field(dest, base_temp, &field);

        // Should generate LoadField instruction
        let instructions = &lowerer.instructions;
        let load_found = instructions
            .iter()
            .any(|inst| matches!(inst, PIR::LoadField { dest: d, offset: 0, .. } if *d == dest));
        assert!(load_found, "Should generate LoadField instruction");
    }

    #[test]
    fn test_lower_simple_add() {
        // Create a simple MIR function: fn add(a, b) { return a + b }
        let mir_func = MirFunction {
            name: "add".to_string(),
            params: vec![
                ("a".to_string(), RueType::I32),
                ("b".to_string(), RueType::I32),
            ],
            return_type: RueType::I32,
            entry_block: BlockId(0),
            temp_types: std::collections::HashMap::new(),
            span: rue_lexer::Span::dummy(),
            blocks: vec![BasicBlock {
                id: BlockId(0),
                params: vec![(Temp(0), RueType::I32), (Temp(1), RueType::I32)],
                statements: vec![MirStatement::Assign {
                    dest: Temp(2),
                    value: MirValue::BinaryOp {
                        op: MirBinOp::Add,
                        lhs: Temp(0),
                        rhs: Temp(1),
                    },
                    span: None,
                }],
                terminator: MirTerminator::Return {
                    value: Some(Temp(2)),
                    span: None,
                },
            }],
        };

        let mir_program = MirProgram {
            functions: vec![mir_func],
            function_signatures: HashMap::new(),
            type_context: TypeContext::new(),
        };

        let mut lowerer = MirToPir::new();
        let pir_instructions = lowerer.lower_program(&mir_program);

        // Verify we have instructions
        assert!(!pir_instructions.is_empty());

        // Should have labels, copies for parameters, binary op, return
        let has_label = pir_instructions.iter().any(|i| matches!(i, PIR::Label(_)));
        let has_binary_op = pir_instructions
            .iter()
            .any(|i| matches!(i, PIR::BinaryOp { .. }));
        let has_return = pir_instructions
            .iter()
            .any(|i| matches!(i, PIR::Return { .. }));

        assert!(has_label);
        assert!(has_binary_op);
        assert!(has_return);
    }
}

#[cfg(test)]
#[path = "mir_to_pir_aggregate_test.rs"]
mod mir_to_pir_aggregate_test;
