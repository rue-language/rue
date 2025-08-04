//! MIR to PIR lowering
//!
//! This module converts MIR (in SSA form with block parameters) to the
//! platform-independent representation (PIR) with virtual registers.

use rue_ir::mir::{
    BasicBlock, BlockId, MirBinOp, MirConst, MirFunction, MirProgram, MirStatement, MirTerminator,
    MirUnaryOp, MirValue, Temp,
};
use rue_ir::pir::{BinOp, Label, PIR, PhysicalRegId, VReg, Value};
use rue_ir::types::FieldId;
use std::collections::HashMap;
use tracing::{debug, trace};

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
                    MirConst::Aggregate { .. } => {
                        // Aggregate constants not yet supported in lowering
                        panic!("Aggregate constants not yet supported in MIR to PIR lowering");
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
                self.emit(PIR::Call {
                    dest: Some(dest),
                    function: func.clone(),
                    args: arg_vregs,
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
                    self.emit(PIR::Return { value: Some(vreg) });
                } else {
                    self.emit(PIR::Return { value: None });
                }
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

    /// Lower ConstructAggregate to PIR
    fn lower_construct_aggregate(
        &mut self,
        dest: VReg,
        ty: &rue_ir::types::RueType,
        fields: &[Temp],
    ) {
        let size = Self::compute_type_size(ty);

        // Allocate memory for the aggregate
        self.emit(PIR::AllocateAggregate {
            dest,
            size,
            alignment: 8, // Use 8-byte alignment for all aggregates
        });

        // Store each field value into the allocated memory
        let mut current_offset = 0;
        match ty {
            rue_ir::types::RueType::Tuple(field_types) => {
                for (field_temp, field_ty) in fields.iter().zip(field_types.iter()) {
                    let field_vreg = self.get_vreg(*field_temp);
                    self.emit(PIR::StoreField {
                        base: dest,
                        offset: current_offset,
                        src: Value::VReg(field_vreg),
                        field_type: field_ty.clone(),
                    });
                    current_offset += Self::compute_type_size(field_ty);
                }
            }
            rue_ir::types::RueType::Array(elem_ty, _len) => {
                let elem_size = Self::compute_type_size(elem_ty);
                for field_temp in fields.iter() {
                    let field_vreg = self.get_vreg(*field_temp);
                    self.emit(PIR::StoreField {
                        base: dest,
                        offset: current_offset,
                        src: Value::VReg(field_vreg),
                        field_type: (**elem_ty).clone(),
                    });
                    current_offset += elem_size;
                }
            }
            rue_ir::types::RueType::Struct(_) => {
                // Simple sequential layout for structs
                for field_temp in fields.iter() {
                    let field_vreg = self.get_vreg(*field_temp);
                    self.emit(PIR::StoreField {
                        base: dest,
                        offset: current_offset,
                        src: Value::VReg(field_vreg),
                        field_type: rue_ir::types::RueType::I64, // Conservative assumption
                    });
                    current_offset += 8; // Conservative field size
                }
            }
            _ => {
                panic!("Cannot construct aggregate of type {ty:?}");
            }
        }
    }

    /// Lower GetField to PIR
    fn lower_get_field(&mut self, dest: VReg, base: Temp, field: &FieldId) {
        let base_vreg = self.get_vreg(base);

        // We need the base type to compute the field offset
        // For now, we'll make conservative assumptions
        // In the future, this would use type information from the MIR

        match field {
            FieldId::Index(idx) => {
                // Assume base is a pointer to an aggregate with 8-byte fields
                let offset = (*idx as i64) * 8;
                self.emit(PIR::LoadField {
                    dest,
                    base: base_vreg,
                    offset,
                    field_type: rue_ir::types::RueType::I64, // Conservative assumption
                });
            }
            FieldId::Named(_name) => {
                // For named fields, assume offset 0 for now
                self.emit(PIR::LoadField {
                    dest,
                    base: base_vreg,
                    offset: 0,
                    field_type: rue_ir::types::RueType::I64, // Conservative assumption
                });
            }
        }
    }

    /// Lower SetField to PIR
    fn lower_set_field(&mut self, dest: VReg, base: Temp, field: &FieldId, value: Temp) {
        let base_vreg = self.get_vreg(base);
        let value_vreg = self.get_vreg(value);

        // Allocate a new aggregate (functional update)
        // For now, assume base type is a 64-byte struct
        self.emit(PIR::AllocateAggregate {
            dest,
            size: 64,
            alignment: 8,
        });

        // Copy the base aggregate to the new location
        self.emit(PIR::CopyAggregate {
            dest,
            src: base_vreg,
            size: 64,
        });

        // Update the specific field
        match field {
            FieldId::Index(idx) => {
                let offset = (*idx as i64) * 8;
                self.emit(PIR::StoreField {
                    base: dest,
                    offset,
                    src: Value::VReg(value_vreg),
                    field_type: rue_ir::types::RueType::I64,
                });
            }
            FieldId::Named(_name) => {
                self.emit(PIR::StoreField {
                    base: dest,
                    offset: 0,
                    src: Value::VReg(value_vreg),
                    field_type: rue_ir::types::RueType::I64,
                });
            }
        }
    }

    /// Lower StructUpdate to PIR
    fn lower_struct_update(
        &mut self,
        dest: VReg,
        base: Temp,
        updates: &[(FieldId, Temp)],
        _struct_type: rue_ir::types::StructId,
    ) {
        let base_vreg = self.get_vreg(base);

        // Allocate a new struct
        self.emit(PIR::AllocateAggregate {
            dest,
            size: 64, // Conservative struct size
            alignment: 8,
        });

        // Copy the base struct
        self.emit(PIR::CopyAggregate {
            dest,
            src: base_vreg,
            size: 64,
        });

        // Apply all updates
        for (field, value_temp) in updates {
            let value_vreg = self.get_vreg(*value_temp);
            match field {
                FieldId::Index(idx) => {
                    let offset = (*idx as i64) * 8;
                    self.emit(PIR::StoreField {
                        base: dest,
                        offset,
                        src: Value::VReg(value_vreg),
                        field_type: rue_ir::types::RueType::I64,
                    });
                }
                FieldId::Named(_name) => {
                    self.emit(PIR::StoreField {
                        base: dest,
                        offset: 0,
                        src: Value::VReg(value_vreg),
                        field_type: rue_ir::types::RueType::I64,
                    });
                }
            }
        }
    }

    /// Helper function to compute size of a type in bytes
    fn compute_type_size(ty: &rue_ir::types::RueType) -> i64 {
        match ty {
            rue_ir::types::RueType::I32 => 4,
            rue_ir::types::RueType::I64 | rue_ir::types::RueType::Bool => 8, // Bool stored as i64
            rue_ir::types::RueType::Unit | rue_ir::types::RueType::Unknown => 0,
            rue_ir::types::RueType::Tuple(types) => {
                // Sum of field sizes - simple layout for now
                types.iter().map(Self::compute_type_size).sum()
            }
            rue_ir::types::RueType::Array(elem_ty, len) => {
                Self::compute_type_size(elem_ty) * (*len as i64)
            }
            rue_ir::types::RueType::Struct(_) => {
                // Conservative size estimate for structs
                // In the future, this would query a type registry
                64
            }
        }
    }

    /// Helper function to compute field offset within an aggregate
    #[expect(dead_code)] // Will be used when proper type information is available
    fn compute_field_offset(
        &self,
        base_ty: &rue_ir::types::RueType,
        field: &FieldId,
    ) -> (i64, rue_ir::types::RueType) {
        match (base_ty, field) {
            (rue_ir::types::RueType::Tuple(types), FieldId::Index(idx)) => {
                if *idx >= types.len() {
                    panic!(
                        "Field index {} out of bounds for tuple with {} fields",
                        idx,
                        types.len()
                    );
                }
                let offset = types.iter().take(*idx).map(Self::compute_type_size).sum();
                (offset, types[*idx].clone())
            }
            (rue_ir::types::RueType::Array(elem_ty, len), FieldId::Index(idx)) => {
                if *idx >= *len {
                    panic!("Array index {idx} out of bounds for array of length {len}");
                }
                let elem_size = Self::compute_type_size(elem_ty);
                let offset = elem_size * (*idx as i64);
                (offset, (**elem_ty).clone())
            }
            (rue_ir::types::RueType::Struct(_struct_id), FieldId::Named(_name)) => {
                // For now, return conservative defaults
                // In the future, this would query the type registry
                (0, rue_ir::types::RueType::I64)
            }
            (rue_ir::types::RueType::Struct(_struct_id), FieldId::Index(idx)) => {
                // Treat as tuple-like access for now
                ((*idx as i64) * 8, rue_ir::types::RueType::I64)
            }
            _ => {
                panic!("Invalid field access: {field:?} on type {base_ty:?}");
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
        // Test tuple size calculation
        let tuple_ty = RueType::Tuple(vec![RueType::I64, RueType::I32]);
        let tuple_size = MirToPir::compute_type_size(&tuple_ty);
        assert_eq!(
            tuple_size, 12,
            "Tuple (i64, i32) should be 8 + 4 = 12 bytes"
        );

        // Test array size calculation
        let array_ty = RueType::Array(Box::new(RueType::I32), 5);
        let array_size = MirToPir::compute_type_size(&array_ty);
        assert_eq!(array_size, 20, "Array [i32; 5] should be 4 * 5 = 20 bytes");

        // Test struct size (conservative estimate)
        let struct_ty = RueType::Struct(rue_ir::types::StructId::new(1));
        let struct_size = MirToPir::compute_type_size(&struct_ty);
        assert_eq!(
            struct_size, 64,
            "Struct should use conservative size of 64 bytes"
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
            matches!(inst, PIR::AllocateAggregate { dest: d, size: 12, alignment: 8 } if *d == dest)
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
