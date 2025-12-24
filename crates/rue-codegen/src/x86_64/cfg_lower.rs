//! CFG to X86Mir lowering.
//!
//! This module converts CFG (explicit control flow graph) to X86Mir
//! (x86-64 instructions with virtual registers).

use std::collections::HashMap;

use rue_air::{ArrayTypeDef, ArrayTypeId};
use rue_cfg::{
    BasicBlock, BlockId, Cfg, CfgInstData, CfgValue, StructDef, StructId, Terminator, Type,
};

use super::mir::{LabelId, Operand, Reg, VReg, X86Inst, X86Mir};

/// Argument passing registers per System V AMD64 ABI.
const ARG_REGS: [Reg; 6] = [Reg::Rdi, Reg::Rsi, Reg::Rdx, Reg::Rcx, Reg::R8, Reg::R9];

/// Return value registers per System V AMD64 ABI.
const RET_REGS: [Reg; 6] = [Reg::Rax, Reg::Rdx, Reg::Rcx, Reg::R8, Reg::R9, Reg::R10];

/// Result of tracing back through a field access chain.
/// Indicates whether the chain originates from a Load or Param.
enum FieldChainBase {
    /// Chain originates from a Load instruction with the given slot.
    Load { slot: u32 },
    /// Chain originates from a Param instruction with the given index.
    Param { index: u32 },
}

/// CFG to X86Mir lowering.
pub struct CfgLower<'a> {
    cfg: &'a Cfg,
    struct_defs: &'a [StructDef],
    /// Array type definitions for bounds checking.
    array_types: &'a [ArrayTypeDef],
    /// String table from semantic analysis (indexed by StringId).
    strings: &'a [String],
    mir: X86Mir,
    /// Maps CFG values to vregs
    value_map: HashMap<CfgValue, VReg>,
    /// Maps block parameters to vregs (block_id, param_index) -> vreg
    block_param_vregs: HashMap<(BlockId, u32), VReg>,
    /// Next label ID for generating unique labels
    next_label: u32,
    /// Whether this function has a stack frame
    has_frame: bool,
    /// Number of local variable slots
    num_locals: u32,
    /// Number of parameter slots
    num_params: u32,
    /// Function name (needed to detect main function)
    fn_name: &'a str,
    /// Maps StructInit CFG values to their field vregs
    struct_field_vregs: HashMap<CfgValue, Vec<VReg>>,
}

impl<'a> CfgLower<'a> {
    /// Create a new CFG lowering pass.
    pub fn new(
        cfg: &'a Cfg,
        struct_defs: &'a [StructDef],
        array_types: &'a [ArrayTypeDef],
        strings: &'a [String],
    ) -> Self {
        let num_locals = cfg.num_locals();
        let num_params = cfg.num_params();
        Self {
            cfg,
            struct_defs,
            array_types,
            strings,
            mir: X86Mir::new(),
            value_map: HashMap::new(),
            block_param_vregs: HashMap::new(),
            next_label: 0,
            has_frame: num_locals > 0 || num_params > 0,
            num_locals,
            num_params,
            fn_name: cfg.fn_name(),
            struct_field_vregs: HashMap::new(),
        }
    }

    /// Get the number of fields for a struct type.
    fn struct_field_count(&self, struct_id: StructId) -> u32 {
        self.struct_defs
            .get(struct_id.0 as usize)
            .map(|def| def.field_count() as u32)
            .unwrap_or(1)
    }

    /// Get the length of an array type.
    fn array_length(&self, array_type_id: ArrayTypeId) -> u64 {
        debug_assert!(
            (array_type_id.0 as usize) < self.array_types.len(),
            "invalid array type ID: {:?}",
            array_type_id
        );
        self.array_types
            .get(array_type_id.0 as usize)
            .map(|def| def.length)
            .unwrap_or(0)
    }

    /// Get the array type definition.
    fn array_type_def(&self, array_type_id: ArrayTypeId) -> Option<&ArrayTypeDef> {
        self.array_types.get(array_type_id.0 as usize)
    }

    /// Calculate the total number of slots needed to store a type.
    /// For scalars, this is 1. For arrays, it's length * slot_count(element_type).
    /// For nested arrays, this recursively multiplies.
    fn type_slot_count(&self, ty: Type) -> u32 {
        match ty {
            Type::Array(array_type_id) => {
                if let Some(def) = self.array_type_def(array_type_id) {
                    let elem_slots = self.type_slot_count(def.element_type);
                    (def.length as u32) * elem_slots
                } else {
                    1
                }
            }
            Type::Struct(struct_id) => self.struct_field_count(struct_id),
            _ => 1,
        }
    }

    /// Calculate the slot count for a single element of an array type.
    fn array_element_slot_count(&self, array_type_id: ArrayTypeId) -> u32 {
        if let Some(def) = self.array_type_def(array_type_id) {
            self.type_slot_count(def.element_type)
        } else {
            1
        }
    }

    /// Calculate the slot offset for a field within a struct.
    /// This accounts for the sizes of all preceding fields.
    fn struct_field_slot_offset(&self, struct_id: StructId, field_index: u32) -> u32 {
        if let Some(struct_def) = self.struct_defs.get(struct_id.0 as usize) {
            let mut offset = 0u32;
            for i in 0..(field_index as usize) {
                if let Some(field) = struct_def.fields.get(i) {
                    offset += self.type_slot_count(field.ty);
                }
            }
            offset
        } else {
            field_index // Fallback to field index if struct not found
        }
    }

    /// Trace back through nested FieldGet operations to find the base
    /// and compute the total slot offset.
    ///
    /// Returns `Some((base, total_offset))` if the chain originates from a Load or Param,
    /// or `None` if it originates from something else (BlockParam, StructInit, Call, etc).
    fn trace_field_chain(&self, value: CfgValue) -> Option<(FieldChainBase, u32)> {
        let inst = self.cfg.get_inst(value);
        match &inst.data {
            CfgInstData::Load { slot } => Some((FieldChainBase::Load { slot: *slot }, 0)),
            CfgInstData::Param { index } => Some((FieldChainBase::Param { index: *index }, 0)),
            CfgInstData::FieldGet {
                base,
                struct_id,
                field_index,
            } => {
                // Recursively trace back through the base
                if let Some((base_kind, accumulated_offset)) = self.trace_field_chain(*base) {
                    let this_offset = self.struct_field_slot_offset(*struct_id, *field_index);
                    Some((base_kind, accumulated_offset + this_offset))
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    /// Recursively collect all scalar vregs from an array value.
    /// For nested arrays, this flattens them to a list of scalar vregs.
    fn collect_array_scalar_vregs(&mut self, value: CfgValue) -> Vec<VReg> {
        let inst = self.cfg.get_inst(value);
        match &inst.data.clone() {
            CfgInstData::ArrayInit { elements, .. } => {
                let mut result = Vec::new();
                for elem in elements {
                    let elem_inst = self.cfg.get_inst(*elem);
                    if matches!(elem_inst.ty, Type::Array(_)) {
                        // Recursively collect from nested array
                        result.extend(self.collect_array_scalar_vregs(*elem));
                    } else {
                        // Scalar element - get its vreg
                        result.push(self.get_vreg(*elem));
                    }
                }
                result
            }
            _ => {
                // For non-ArrayInit sources, try struct_field_vregs cache
                if let Some(vregs) = self.struct_field_vregs.get(&value).cloned() {
                    vregs
                } else {
                    vec![self.get_vreg(value)]
                }
            }
        }
    }

    /// Recursively collect all scalar vregs from a struct value.
    /// This flattens any array fields to their scalar elements.
    fn collect_struct_scalar_vregs(&mut self, value: CfgValue) -> Vec<VReg> {
        let inst = self.cfg.get_inst(value);
        match &inst.data.clone() {
            CfgInstData::StructInit { fields, .. } => {
                let mut result = Vec::new();
                for field in fields {
                    let field_inst = self.cfg.get_inst(*field);
                    if matches!(field_inst.ty, Type::Array(_)) {
                        // Recursively collect from array field
                        result.extend(self.collect_array_scalar_vregs(*field));
                    } else if matches!(field_inst.ty, Type::Struct(_)) {
                        // Recursively collect from nested struct field
                        result.extend(self.collect_struct_scalar_vregs(*field));
                    } else {
                        // Scalar field - get its vreg
                        result.push(self.get_vreg(*field));
                    }
                }
                result
            }
            _ => {
                // For non-StructInit sources, try struct_field_vregs cache
                if let Some(vregs) = self.struct_field_vregs.get(&value).cloned() {
                    vregs
                } else {
                    vec![self.get_vreg(value)]
                }
            }
        }
    }

    /// Calculate the stack offset for a local variable slot.
    fn local_offset(&self, slot: u32) -> i32 {
        -((slot as i32 + 1) * 8)
    }

    /// Emit a bounds check for array indexing.
    ///
    /// Generates code to check that `index_vreg < length` and calls `__rue_bounds_check`
    /// if the check fails. Uses unsigned comparison so negative indices also fail.
    fn emit_bounds_check(&mut self, index_vreg: VReg, length: u64) {
        // Load the array length into a temporary register
        let length_vreg = self.mir.alloc_vreg();
        self.mir.push(X86Inst::MovRI64 {
            dst: Operand::Virtual(length_vreg),
            imm: length as i64,
        });

        // Compare index (unsigned) against length
        self.mir.push(X86Inst::CmpRR {
            src1: Operand::Virtual(index_vreg),
            src2: Operand::Virtual(length_vreg),
        });

        // If index < length (unsigned), jump to ok label; otherwise call bounds check
        let ok_label = self.new_label();
        self.mir.push(X86Inst::Jb { label: ok_label });

        // Call the bounds check error handler (never returns)
        self.mir.push(X86Inst::CallRel {
            symbol: "__rue_bounds_check".to_string(),
        });

        // Continue with valid access
        self.mir.push(X86Inst::Label { id: ok_label });
    }

    /// Emit function epilogue.
    fn emit_epilogue(&mut self) {
        self.mir.push(X86Inst::MovRR {
            dst: Operand::Physical(Reg::Rsp),
            src: Operand::Physical(Reg::Rbp),
        });
        self.mir.push(X86Inst::Pop {
            dst: Operand::Physical(Reg::Rbp),
        });
    }

    /// Allocate a new label ID.
    fn new_label(&mut self) -> LabelId {
        let label = LabelId::new(self.next_label);
        self.next_label += 1;
        label
    }

    /// Get the label for a block.
    ///
    /// Block IDs are mapped to label IDs with a simple offset to avoid
    /// collisions with other allocated labels. Block labels use IDs starting
    /// at u32::MAX / 2 and counting down.
    fn block_label(&self, block_id: BlockId) -> LabelId {
        // Use high label IDs for blocks to avoid collision with new_label()
        LabelId::new(u32::MAX / 2 + block_id.as_u32())
    }

    /// Get or compute field vregs for a struct value.
    ///
    /// This handles different sources of struct values:
    /// - StructInit: use the field values directly
    /// - Load: load field values from stack slots
    /// - Param: use parameter registers/slots
    /// - BlockParam/Call: use cached struct_field_vregs
    fn get_or_compute_field_vregs(&mut self, value: CfgValue) -> Option<Vec<VReg>> {
        // Check cache first
        if let Some(vregs) = self.struct_field_vregs.get(&value).cloned() {
            return Some(vregs);
        }

        let inst = self.cfg.get_inst(value);
        let struct_id = match inst.ty {
            Type::Struct(id) => id,
            _ => return None,
        };

        match &inst.data.clone() {
            CfgInstData::StructInit { fields, .. } => {
                Some(fields.iter().map(|f| self.get_vreg(*f)).collect())
            }
            CfgInstData::Load { slot } => {
                // Load field values from consecutive stack slots
                let field_count = self.struct_field_count(struct_id);
                let mut vregs = Vec::new();
                for i in 0..field_count {
                    let vreg = self.mir.alloc_vreg();
                    let offset = self.local_offset(slot + i);
                    self.mir.push(X86Inst::MovRM {
                        dst: Operand::Virtual(vreg),
                        base: Reg::Rbp,
                        offset,
                    });
                    vregs.push(vreg);
                }
                Some(vregs)
            }
            CfgInstData::Param { index } => {
                // Get field values from parameter area
                let field_count = self.struct_field_count(struct_id);
                let mut vregs = Vec::new();
                for i in 0..field_count {
                    let vreg = self.mir.alloc_vreg();
                    let param_slot = self.num_locals + index + i;
                    let offset = self.local_offset(param_slot);
                    self.mir.push(X86Inst::MovRM {
                        dst: Operand::Virtual(vreg),
                        base: Reg::Rbp,
                        offset,
                    });
                    vregs.push(vreg);
                }
                Some(vregs)
            }
            // BlockParam and Call should already have field vregs in cache
            _ => None,
        }
    }

    /// Copy a struct value's field vregs to a block parameter's field vregs.
    fn copy_struct_to_block_param(&mut self, arg: CfgValue, target_block: BlockId, param_idx: u32) {
        let target_param = self.cfg.get_block(target_block).params[param_idx as usize].0;

        let src_fields = self.get_or_compute_field_vregs(arg);
        let dst_fields = self.struct_field_vregs.get(&target_param).cloned();

        debug_assert!(
            src_fields.is_some(),
            "struct arg should have field vregs available"
        );
        debug_assert!(
            dst_fields.is_some(),
            "struct block param should have field vregs pre-allocated"
        );

        if let (Some(src), Some(dst)) = (src_fields, dst_fields) {
            debug_assert_eq!(
                src.len(),
                dst.len(),
                "source and destination struct field counts must match"
            );
            for (dst_vreg, src_vreg) in dst.iter().zip(src.iter()) {
                self.mir.push(X86Inst::MovRR {
                    dst: Operand::Virtual(*dst_vreg),
                    src: Operand::Virtual(*src_vreg),
                });
            }
        }
    }

    /// Lower CFG to X86Mir.
    pub fn lower(mut self) -> X86Mir {
        // Pre-allocate vregs for block parameters
        for block in self.cfg.blocks() {
            for (param_idx, (param_val, ty)) in block.params.iter().enumerate() {
                let vreg = self.mir.alloc_vreg();
                self.block_param_vregs
                    .insert((block.id, param_idx as u32), vreg);
                self.value_map.insert(*param_val, vreg);

                // For struct types, also allocate vregs for each field
                if let Type::Struct(struct_id) = ty {
                    let field_count = self.struct_field_count(*struct_id);
                    let mut field_vregs = vec![vreg]; // First field uses main vreg
                    for _ in 1..field_count {
                        field_vregs.push(self.mir.alloc_vreg());
                    }
                    self.struct_field_vregs.insert(*param_val, field_vregs);
                }
            }
        }

        // Lower each block
        for block in self.cfg.blocks() {
            self.lower_block(block);
        }

        self.mir
    }

    /// Lower a single basic block.
    fn lower_block(&mut self, block: &BasicBlock) {
        // Emit block label (except for entry block)
        if block.id != self.cfg.entry {
            self.mir.push(X86Inst::Label {
                id: self.block_label(block.id),
            });
        }

        // Lower each instruction
        for &value in &block.insts {
            self.lower_value(value);
        }

        // Lower terminator
        self.lower_terminator(block);
    }

    /// Lower a CFG value (instruction).
    fn lower_value(&mut self, value: CfgValue) {
        // Skip if already lowered
        if self.value_map.contains_key(&value) {
            return;
        }

        let inst = self.cfg.get_inst(value);
        let ty = inst.ty;

        match &inst.data {
            CfgInstData::Const(v) => {
                let vreg = self.mir.alloc_vreg();
                self.value_map.insert(value, vreg);

                // Use 32-bit immediate if value fits in unsigned 32-bit range
                // (this is safe because mov r32, imm32 zero-extends to 64-bit)
                if *v <= u32::MAX as u64 {
                    self.mir.push(X86Inst::MovRI32 {
                        dst: Operand::Virtual(vreg),
                        imm: *v as i32,
                    });
                } else {
                    // For values > u32::MAX, use 64-bit move
                    // Cast to i64 to preserve the bit pattern
                    self.mir.push(X86Inst::MovRI64 {
                        dst: Operand::Virtual(vreg),
                        imm: *v as i64,
                    });
                }
            }

            CfgInstData::BoolConst(v) => {
                let vreg = self.mir.alloc_vreg();
                self.value_map.insert(value, vreg);

                self.mir.push(X86Inst::MovRI32 {
                    dst: Operand::Virtual(vreg),
                    imm: if *v { 1 } else { 0 },
                });
            }

            CfgInstData::StringConst(string_id) => {
                let ptr_vreg = self.mir.alloc_vreg();
                let len_vreg = self.mir.alloc_vreg();

                self.mir.push(X86Inst::StringConstPtr {
                    dst: Operand::Virtual(ptr_vreg),
                    string_id: *string_id,
                });

                self.mir.push(X86Inst::StringConstLen {
                    dst: Operand::Virtual(len_vreg),
                    string_id: *string_id,
                });

                // Store both in struct_field_vregs for fat pointer access
                self.struct_field_vregs
                    .insert(value, vec![ptr_vreg, len_vreg]);
                self.value_map.insert(value, ptr_vreg);
            }

            CfgInstData::Param { index } => {
                let vreg = self.mir.alloc_vreg();
                self.value_map.insert(value, vreg);

                if (*index as usize) < ARG_REGS.len() {
                    let slot = self.num_locals + *index;
                    let offset = self.local_offset(slot);
                    self.mir.push(X86Inst::MovRM {
                        dst: Operand::Virtual(vreg),
                        base: Reg::Rbp,
                        offset,
                    });
                } else {
                    let stack_offset = 16 + ((*index as i32) - 6) * 8;
                    self.mir.push(X86Inst::MovRM {
                        dst: Operand::Virtual(vreg),
                        base: Reg::Rbp,
                        offset: stack_offset,
                    });
                }
            }

            CfgInstData::BlockParam { .. } => {
                // Block parameters are pre-allocated, nothing to do here
            }

            CfgInstData::Add(lhs, rhs) => {
                let vreg = self.mir.alloc_vreg();
                self.value_map.insert(value, vreg);

                let lhs_vreg = self.get_vreg(*lhs);
                let rhs_vreg = self.get_vreg(*rhs);

                self.mir.push(X86Inst::MovRR {
                    dst: Operand::Virtual(vreg),
                    src: Operand::Virtual(lhs_vreg),
                });

                // Use 64-bit add for 64-bit types to get correct overflow detection
                if matches!(ty, Type::I64 | Type::U64) {
                    self.mir.push(X86Inst::AddRR64 {
                        dst: Operand::Virtual(vreg),
                        src: Operand::Virtual(rhs_vreg),
                    });
                } else {
                    self.mir.push(X86Inst::AddRR {
                        dst: Operand::Virtual(vreg),
                        src: Operand::Virtual(rhs_vreg),
                    });
                }

                // Overflow check - use appropriate flag based on signedness
                self.emit_overflow_check(ty, vreg);
            }

            CfgInstData::Sub(lhs, rhs) => {
                let vreg = self.mir.alloc_vreg();
                self.value_map.insert(value, vreg);

                let lhs_vreg = self.get_vreg(*lhs);
                let rhs_vreg = self.get_vreg(*rhs);

                self.mir.push(X86Inst::MovRR {
                    dst: Operand::Virtual(vreg),
                    src: Operand::Virtual(lhs_vreg),
                });

                // Use 64-bit sub for 64-bit types to get correct overflow detection
                if matches!(ty, Type::I64 | Type::U64) {
                    self.mir.push(X86Inst::SubRR64 {
                        dst: Operand::Virtual(vreg),
                        src: Operand::Virtual(rhs_vreg),
                    });
                } else {
                    self.mir.push(X86Inst::SubRR {
                        dst: Operand::Virtual(vreg),
                        src: Operand::Virtual(rhs_vreg),
                    });
                }

                // Overflow check - use appropriate flag based on signedness
                self.emit_overflow_check(ty, vreg);
            }

            CfgInstData::Mul(lhs, rhs) => {
                let vreg = self.mir.alloc_vreg();
                self.value_map.insert(value, vreg);

                let lhs_vreg = self.get_vreg(*lhs);
                let rhs_vreg = self.get_vreg(*rhs);

                self.mir.push(X86Inst::MovRR {
                    dst: Operand::Virtual(vreg),
                    src: Operand::Virtual(lhs_vreg),
                });

                // Use 64-bit mul for 64-bit types to get correct overflow detection
                if matches!(ty, Type::I64 | Type::U64) {
                    self.mir.push(X86Inst::ImulRR64 {
                        dst: Operand::Virtual(vreg),
                        src: Operand::Virtual(rhs_vreg),
                    });
                } else {
                    self.mir.push(X86Inst::ImulRR {
                        dst: Operand::Virtual(vreg),
                        src: Operand::Virtual(rhs_vreg),
                    });
                }

                // Overflow check - use appropriate flag based on signedness
                // Note: IMUL sets both OF and CF to the same value, so this works
                // for both signed and unsigned multiplication
                self.emit_overflow_check(ty, vreg);
            }

            CfgInstData::Div(lhs, rhs) => {
                let vreg = self.mir.alloc_vreg();
                self.value_map.insert(value, vreg);

                let lhs_vreg = self.get_vreg(*lhs);
                let rhs_vreg = self.get_vreg(*rhs);

                // Division by zero check
                let ok_label = self.new_label();
                self.mir.push(X86Inst::TestRR {
                    src1: Operand::Virtual(rhs_vreg),
                    src2: Operand::Virtual(rhs_vreg),
                });
                self.mir.push(X86Inst::Jnz { label: ok_label });
                self.mir.push(X86Inst::CallRel {
                    symbol: "__rue_div_by_zero".to_string(),
                });
                self.mir.push(X86Inst::Label { id: ok_label });

                self.mir.push(X86Inst::MovRR {
                    dst: Operand::Physical(Reg::Rax),
                    src: Operand::Virtual(lhs_vreg),
                });
                self.mir.push(X86Inst::Cdq);
                self.mir.push(X86Inst::IdivR {
                    src: Operand::Virtual(rhs_vreg),
                });
                self.mir.push(X86Inst::MovRR {
                    dst: Operand::Virtual(vreg),
                    src: Operand::Physical(Reg::Rax),
                });
            }

            CfgInstData::Mod(lhs, rhs) => {
                let vreg = self.mir.alloc_vreg();
                self.value_map.insert(value, vreg);

                let lhs_vreg = self.get_vreg(*lhs);
                let rhs_vreg = self.get_vreg(*rhs);

                let ok_label = self.new_label();
                self.mir.push(X86Inst::TestRR {
                    src1: Operand::Virtual(rhs_vreg),
                    src2: Operand::Virtual(rhs_vreg),
                });
                self.mir.push(X86Inst::Jnz { label: ok_label });
                self.mir.push(X86Inst::CallRel {
                    symbol: "__rue_div_by_zero".to_string(),
                });
                self.mir.push(X86Inst::Label { id: ok_label });

                self.mir.push(X86Inst::MovRR {
                    dst: Operand::Physical(Reg::Rax),
                    src: Operand::Virtual(lhs_vreg),
                });
                self.mir.push(X86Inst::Cdq);
                self.mir.push(X86Inst::IdivR {
                    src: Operand::Virtual(rhs_vreg),
                });
                self.mir.push(X86Inst::MovRR {
                    dst: Operand::Virtual(vreg),
                    src: Operand::Physical(Reg::Rdx),
                });
            }

            CfgInstData::Neg(operand) => {
                let vreg = self.mir.alloc_vreg();
                self.value_map.insert(value, vreg);

                let operand_vreg = self.get_vreg(*operand);

                self.mir.push(X86Inst::MovRR {
                    dst: Operand::Virtual(vreg),
                    src: Operand::Virtual(operand_vreg),
                });

                // Use 64-bit neg for 64-bit types to get correct overflow detection
                if matches!(ty, Type::I64 | Type::U64) {
                    self.mir.push(X86Inst::Neg64 {
                        dst: Operand::Virtual(vreg),
                    });
                } else {
                    self.mir.push(X86Inst::Neg {
                        dst: Operand::Virtual(vreg),
                    });
                }

                // Overflow check for negation
                // For signed types: NEG sets OF when negating MIN_VALUE
                // For unsigned types: NEG sets CF for all non-zero values,
                // but we only care about -0 = 0 (no overflow). Since negation
                // of any non-zero unsigned value would wrap (0 - x), we check CF.
                self.emit_overflow_check(ty, vreg);
            }

            CfgInstData::Not(operand) => {
                let vreg = self.mir.alloc_vreg();
                self.value_map.insert(value, vreg);

                let operand_vreg = self.get_vreg(*operand);

                self.mir.push(X86Inst::MovRR {
                    dst: Operand::Virtual(vreg),
                    src: Operand::Virtual(operand_vreg),
                });
                self.mir.push(X86Inst::XorRI {
                    dst: Operand::Virtual(vreg),
                    imm: 1,
                });
            }

            CfgInstData::BitNot(operand) => {
                let vreg = self.mir.alloc_vreg();
                self.value_map.insert(value, vreg);

                let operand_vreg = self.get_vreg(*operand);

                self.mir.push(X86Inst::MovRR {
                    dst: Operand::Virtual(vreg),
                    src: Operand::Virtual(operand_vreg),
                });
                self.mir.push(X86Inst::NotR {
                    dst: Operand::Virtual(vreg),
                });
            }

            CfgInstData::BitAnd(lhs, rhs) => {
                let vreg = self.mir.alloc_vreg();
                self.value_map.insert(value, vreg);

                let lhs_vreg = self.get_vreg(*lhs);
                let rhs_vreg = self.get_vreg(*rhs);

                self.mir.push(X86Inst::MovRR {
                    dst: Operand::Virtual(vreg),
                    src: Operand::Virtual(lhs_vreg),
                });
                self.mir.push(X86Inst::AndRR {
                    dst: Operand::Virtual(vreg),
                    src: Operand::Virtual(rhs_vreg),
                });
            }

            CfgInstData::BitOr(lhs, rhs) => {
                let vreg = self.mir.alloc_vreg();
                self.value_map.insert(value, vreg);

                let lhs_vreg = self.get_vreg(*lhs);
                let rhs_vreg = self.get_vreg(*rhs);

                self.mir.push(X86Inst::MovRR {
                    dst: Operand::Virtual(vreg),
                    src: Operand::Virtual(lhs_vreg),
                });
                self.mir.push(X86Inst::OrRR {
                    dst: Operand::Virtual(vreg),
                    src: Operand::Virtual(rhs_vreg),
                });
            }

            CfgInstData::BitXor(lhs, rhs) => {
                let vreg = self.mir.alloc_vreg();
                self.value_map.insert(value, vreg);

                let lhs_vreg = self.get_vreg(*lhs);
                let rhs_vreg = self.get_vreg(*rhs);

                self.mir.push(X86Inst::MovRR {
                    dst: Operand::Virtual(vreg),
                    src: Operand::Virtual(lhs_vreg),
                });
                self.mir.push(X86Inst::XorRR {
                    dst: Operand::Virtual(vreg),
                    src: Operand::Virtual(rhs_vreg),
                });
            }

            CfgInstData::Shl(lhs, rhs) => {
                let vreg = self.mir.alloc_vreg();
                self.value_map.insert(value, vreg);

                let lhs_vreg = self.get_vreg(*lhs);
                let rhs_vreg = self.get_vreg(*rhs);

                // Move LHS to result
                self.mir.push(X86Inst::MovRR {
                    dst: Operand::Virtual(vreg),
                    src: Operand::Virtual(lhs_vreg),
                });

                // Move shift amount to RCX (CL is the low byte)
                self.mir.push(X86Inst::MovRR {
                    dst: Operand::Physical(Reg::Rcx),
                    src: Operand::Virtual(rhs_vreg),
                });

                // Use 64-bit shift for i64/u64, 32-bit shift for smaller types
                // 32-bit shift masks by 31, 64-bit shift masks by 63
                if ty.is_64_bit() {
                    self.mir.push(X86Inst::ShlRCl {
                        dst: Operand::Virtual(vreg),
                    });
                } else {
                    self.mir.push(X86Inst::Shl32RCl {
                        dst: Operand::Virtual(vreg),
                    });
                }
            }

            CfgInstData::Shr(lhs, rhs) => {
                let vreg = self.mir.alloc_vreg();
                self.value_map.insert(value, vreg);

                let lhs_vreg = self.get_vreg(*lhs);
                let rhs_vreg = self.get_vreg(*rhs);

                // Move LHS to result
                self.mir.push(X86Inst::MovRR {
                    dst: Operand::Virtual(vreg),
                    src: Operand::Virtual(lhs_vreg),
                });

                // Move shift amount to RCX (CL is the low byte)
                self.mir.push(X86Inst::MovRR {
                    dst: Operand::Physical(Reg::Rcx),
                    src: Operand::Virtual(rhs_vreg),
                });

                // Use arithmetic shift (SAR) for signed types, logical shift (SHR) for unsigned
                // Use 64-bit shift for i64/u64, 32-bit shift for smaller types
                if ty.is_64_bit() {
                    if ty.is_signed() {
                        self.mir.push(X86Inst::SarRCl {
                            dst: Operand::Virtual(vreg),
                        });
                    } else {
                        self.mir.push(X86Inst::ShrRCl {
                            dst: Operand::Virtual(vreg),
                        });
                    }
                } else {
                    if ty.is_signed() {
                        self.mir.push(X86Inst::Sar32RCl {
                            dst: Operand::Virtual(vreg),
                        });
                    } else {
                        self.mir.push(X86Inst::Shr32RCl {
                            dst: Operand::Virtual(vreg),
                        });
                    }
                }
            }

            CfgInstData::Eq(lhs, rhs) => {
                let lhs_ty = self.cfg.get_inst(*lhs).ty;

                if lhs_ty == Type::String {
                    // String equality: call __rue_str_eq(ptr1, len1, ptr2, len2)
                    let vreg = self.mir.alloc_vreg();
                    self.value_map.insert(value, vreg);

                    // Get string fat pointers (ptr, len) from struct_field_vregs
                    let lhs_fields = self
                        .struct_field_vregs
                        .get(lhs)
                        .cloned()
                        .expect("string should have fat pointer fields");
                    let rhs_fields = self
                        .struct_field_vregs
                        .get(rhs)
                        .cloned()
                        .expect("string should have fat pointer fields");

                    debug_assert_eq!(
                        lhs_fields.len(),
                        2,
                        "string should have 2 fields (ptr, len)"
                    );
                    debug_assert_eq!(
                        rhs_fields.len(),
                        2,
                        "string should have 2 fields (ptr, len)"
                    );

                    let lhs_ptr = lhs_fields[0];
                    let lhs_len = lhs_fields[1];
                    let rhs_ptr = rhs_fields[0];
                    let rhs_len = rhs_fields[1];

                    // Move arguments to calling convention registers
                    // RDI = ptr1, RSI = len1, RDX = ptr2, RCX = len2
                    self.mir.push(X86Inst::MovRR {
                        dst: Operand::Physical(Reg::Rdi),
                        src: Operand::Virtual(lhs_ptr),
                    });
                    self.mir.push(X86Inst::MovRR {
                        dst: Operand::Physical(Reg::Rsi),
                        src: Operand::Virtual(lhs_len),
                    });
                    self.mir.push(X86Inst::MovRR {
                        dst: Operand::Physical(Reg::Rdx),
                        src: Operand::Virtual(rhs_ptr),
                    });
                    self.mir.push(X86Inst::MovRR {
                        dst: Operand::Physical(Reg::Rcx),
                        src: Operand::Virtual(rhs_len),
                    });

                    // Call __rue_str_eq
                    self.mir.push(X86Inst::CallRel {
                        symbol: "__rue_str_eq".to_string(),
                    });

                    // Result is in RAX (0 or 1)
                    self.mir.push(X86Inst::MovRR {
                        dst: Operand::Virtual(vreg),
                        src: Operand::Physical(Reg::Rax),
                    });
                } else {
                    self.emit_comparison(value, *lhs, *rhs, |mir, vreg| {
                        mir.push(X86Inst::Sete {
                            dst: Operand::Virtual(vreg),
                        });
                    });
                }
            }

            CfgInstData::Ne(lhs, rhs) => {
                let lhs_ty = self.cfg.get_inst(*lhs).ty;

                if lhs_ty == Type::String {
                    // String inequality: call __rue_str_eq and invert result
                    let vreg = self.mir.alloc_vreg();
                    self.value_map.insert(value, vreg);

                    // Get string fat pointers (ptr, len) from struct_field_vregs
                    let lhs_fields = self
                        .struct_field_vregs
                        .get(lhs)
                        .cloned()
                        .expect("string should have fat pointer fields");
                    let rhs_fields = self
                        .struct_field_vregs
                        .get(rhs)
                        .cloned()
                        .expect("string should have fat pointer fields");

                    debug_assert_eq!(
                        lhs_fields.len(),
                        2,
                        "string should have 2 fields (ptr, len)"
                    );
                    debug_assert_eq!(
                        rhs_fields.len(),
                        2,
                        "string should have 2 fields (ptr, len)"
                    );

                    let lhs_ptr = lhs_fields[0];
                    let lhs_len = lhs_fields[1];
                    let rhs_ptr = rhs_fields[0];
                    let rhs_len = rhs_fields[1];

                    // Move arguments to calling convention registers
                    // RDI = ptr1, RSI = len1, RDX = ptr2, RCX = len2
                    self.mir.push(X86Inst::MovRR {
                        dst: Operand::Physical(Reg::Rdi),
                        src: Operand::Virtual(lhs_ptr),
                    });
                    self.mir.push(X86Inst::MovRR {
                        dst: Operand::Physical(Reg::Rsi),
                        src: Operand::Virtual(lhs_len),
                    });
                    self.mir.push(X86Inst::MovRR {
                        dst: Operand::Physical(Reg::Rdx),
                        src: Operand::Virtual(rhs_ptr),
                    });
                    self.mir.push(X86Inst::MovRR {
                        dst: Operand::Physical(Reg::Rcx),
                        src: Operand::Virtual(rhs_len),
                    });

                    // Call __rue_str_eq
                    self.mir.push(X86Inst::CallRel {
                        symbol: "__rue_str_eq".to_string(),
                    });

                    // Result is in RAX (0 or 1), invert it with XOR 1
                    self.mir.push(X86Inst::MovRR {
                        dst: Operand::Virtual(vreg),
                        src: Operand::Physical(Reg::Rax),
                    });
                    self.mir.push(X86Inst::XorRI {
                        dst: Operand::Virtual(vreg),
                        imm: 1,
                    });
                } else {
                    self.emit_comparison(value, *lhs, *rhs, |mir, vreg| {
                        mir.push(X86Inst::Setne {
                            dst: Operand::Virtual(vreg),
                        });
                    });
                }
            }

            CfgInstData::Lt(lhs, rhs) => {
                let is_unsigned = self.is_unsigned_comparison(*lhs);
                self.emit_comparison(value, *lhs, *rhs, |mir, vreg| {
                    if is_unsigned {
                        mir.push(X86Inst::Setb {
                            dst: Operand::Virtual(vreg),
                        });
                    } else {
                        mir.push(X86Inst::Setl {
                            dst: Operand::Virtual(vreg),
                        });
                    }
                });
            }

            CfgInstData::Gt(lhs, rhs) => {
                let is_unsigned = self.is_unsigned_comparison(*lhs);
                self.emit_comparison(value, *lhs, *rhs, |mir, vreg| {
                    if is_unsigned {
                        mir.push(X86Inst::Seta {
                            dst: Operand::Virtual(vreg),
                        });
                    } else {
                        mir.push(X86Inst::Setg {
                            dst: Operand::Virtual(vreg),
                        });
                    }
                });
            }

            CfgInstData::Le(lhs, rhs) => {
                let is_unsigned = self.is_unsigned_comparison(*lhs);
                self.emit_comparison(value, *lhs, *rhs, |mir, vreg| {
                    if is_unsigned {
                        mir.push(X86Inst::Setbe {
                            dst: Operand::Virtual(vreg),
                        });
                    } else {
                        mir.push(X86Inst::Setle {
                            dst: Operand::Virtual(vreg),
                        });
                    }
                });
            }

            CfgInstData::Ge(lhs, rhs) => {
                let is_unsigned = self.is_unsigned_comparison(*lhs);
                self.emit_comparison(value, *lhs, *rhs, |mir, vreg| {
                    if is_unsigned {
                        mir.push(X86Inst::Setae {
                            dst: Operand::Virtual(vreg),
                        });
                    } else {
                        mir.push(X86Inst::Setge {
                            dst: Operand::Virtual(vreg),
                        });
                    }
                });
            }

            CfgInstData::And(lhs, rhs) => {
                let vreg = self.mir.alloc_vreg();
                self.value_map.insert(value, vreg);

                let lhs_vreg = self.get_vreg(*lhs);
                let rhs_vreg = self.get_vreg(*rhs);

                self.mir.push(X86Inst::MovRR {
                    dst: Operand::Virtual(vreg),
                    src: Operand::Virtual(lhs_vreg),
                });
                self.mir.push(X86Inst::AndRR {
                    dst: Operand::Virtual(vreg),
                    src: Operand::Virtual(rhs_vreg),
                });
            }

            CfgInstData::Or(lhs, rhs) => {
                let vreg = self.mir.alloc_vreg();
                self.value_map.insert(value, vreg);

                let lhs_vreg = self.get_vreg(*lhs);
                let rhs_vreg = self.get_vreg(*rhs);

                self.mir.push(X86Inst::MovRR {
                    dst: Operand::Virtual(vreg),
                    src: Operand::Virtual(lhs_vreg),
                });
                self.mir.push(X86Inst::OrRR {
                    dst: Operand::Virtual(vreg),
                    src: Operand::Virtual(rhs_vreg),
                });
            }

            CfgInstData::Alloc { slot, init } => {
                let init_type = self.cfg.get_inst(*init).ty;
                if matches!(init_type, Type::Array(_)) {
                    // Array: recursively flatten nested arrays and store scalar elements
                    let scalar_vregs = self.collect_array_scalar_vregs(*init);
                    for (i, scalar_vreg) in scalar_vregs.iter().enumerate() {
                        let elem_slot = slot + i as u32;
                        let offset = self.local_offset(elem_slot);
                        self.mir.push(X86Inst::MovMR {
                            base: Reg::Rbp,
                            offset,
                            src: Operand::Virtual(*scalar_vreg),
                        });
                    }
                } else if matches!(init_type, Type::Struct(_)) {
                    // Struct: recursively flatten struct fields (including array fields) to scalars
                    let scalar_vregs = self.collect_struct_scalar_vregs(*init);
                    for (i, scalar_vreg) in scalar_vregs.iter().enumerate() {
                        let field_slot = slot + i as u32;
                        let offset = self.local_offset(field_slot);
                        self.mir.push(X86Inst::MovMR {
                            base: Reg::Rbp,
                            offset,
                            src: Operand::Virtual(*scalar_vreg),
                        });
                    }
                } else if init_type == Type::String {
                    // String: store both ptr and len to consecutive slots
                    let field_vregs = self
                        .struct_field_vregs
                        .get(init)
                        .cloned()
                        .expect("string should have fat pointer fields in Alloc");
                    debug_assert_eq!(
                        field_vregs.len(),
                        2,
                        "string should have 2 fields (ptr, len)"
                    );

                    let ptr_vreg = field_vregs[0];
                    let len_vreg = field_vregs[1];

                    // Store ptr to slot
                    let ptr_offset = self.local_offset(*slot);
                    self.mir.push(X86Inst::MovMR {
                        base: Reg::Rbp,
                        offset: ptr_offset,
                        src: Operand::Virtual(ptr_vreg),
                    });

                    // Store len to slot + 1
                    let len_offset = self.local_offset(slot + 1);
                    self.mir.push(X86Inst::MovMR {
                        base: Reg::Rbp,
                        offset: len_offset,
                        src: Operand::Virtual(len_vreg),
                    });
                } else {
                    let init_vreg = self.get_vreg(*init);
                    let offset = self.local_offset(*slot);
                    self.mir.push(X86Inst::MovMR {
                        base: Reg::Rbp,
                        offset,
                        src: Operand::Virtual(init_vreg),
                    });
                }
            }

            CfgInstData::Load { slot } => {
                let load_type = self.cfg.get_inst(value).ty;

                if load_type == Type::String {
                    // String: load both ptr and len from consecutive slots
                    let ptr_vreg = self.mir.alloc_vreg();
                    let len_vreg = self.mir.alloc_vreg();

                    // Load ptr from slot
                    let ptr_offset = self.local_offset(*slot);
                    self.mir.push(X86Inst::MovRM {
                        dst: Operand::Virtual(ptr_vreg),
                        base: Reg::Rbp,
                        offset: ptr_offset,
                    });

                    // Load len from slot + 1
                    let len_offset = self.local_offset(slot + 1);
                    self.mir.push(X86Inst::MovRM {
                        dst: Operand::Virtual(len_vreg),
                        base: Reg::Rbp,
                        offset: len_offset,
                    });

                    // Register fat pointer metadata
                    self.struct_field_vregs
                        .insert(value, vec![ptr_vreg, len_vreg]);
                    self.value_map.insert(value, ptr_vreg);
                } else {
                    let vreg = self.mir.alloc_vreg();
                    self.value_map.insert(value, vreg);

                    let offset = self.local_offset(*slot);
                    self.mir.push(X86Inst::MovRM {
                        dst: Operand::Virtual(vreg),
                        base: Reg::Rbp,
                        offset,
                    });
                }
            }

            CfgInstData::Store { slot, value: val } => {
                let val_type = self.cfg.get_inst(*val).ty;
                if val_type == Type::String {
                    // String: store both ptr and len to consecutive slots
                    let field_vregs = self
                        .struct_field_vregs
                        .get(val)
                        .cloned()
                        .expect("string should have fat pointer fields in Store");
                    debug_assert_eq!(
                        field_vregs.len(),
                        2,
                        "string should have 2 fields (ptr, len)"
                    );

                    let ptr_vreg = field_vregs[0];
                    let len_vreg = field_vregs[1];

                    // Store ptr to slot
                    let ptr_offset = self.local_offset(*slot);
                    self.mir.push(X86Inst::MovMR {
                        base: Reg::Rbp,
                        offset: ptr_offset,
                        src: Operand::Virtual(ptr_vreg),
                    });

                    // Store len to slot + 1
                    let len_offset = self.local_offset(slot + 1);
                    self.mir.push(X86Inst::MovMR {
                        base: Reg::Rbp,
                        offset: len_offset,
                        src: Operand::Virtual(len_vreg),
                    });
                } else {
                    let val_vreg = self.get_vreg(*val);
                    let offset = self.local_offset(*slot);
                    self.mir.push(X86Inst::MovMR {
                        base: Reg::Rbp,
                        offset,
                        src: Operand::Virtual(val_vreg),
                    });
                }
            }

            CfgInstData::Call { name, args } => {
                let result_vreg = self.mir.alloc_vreg();
                self.value_map.insert(value, result_vreg);

                // Flatten struct arguments
                let mut flattened_vregs: Vec<VReg> = Vec::new();
                for arg in args {
                    let arg_type = self.cfg.get_inst(*arg).ty;
                    match arg_type {
                        Type::Struct(struct_id) => {
                            let arg_data = &self.cfg.get_inst(*arg).data;
                            match arg_data {
                                CfgInstData::Load { slot } => {
                                    let field_count = self.struct_field_count(struct_id);
                                    for field_idx in 0..field_count {
                                        let field_vreg = self.mir.alloc_vreg();
                                        let field_slot = slot + field_idx;
                                        let offset = self.local_offset(field_slot);
                                        self.mir.push(X86Inst::MovRM {
                                            dst: Operand::Virtual(field_vreg),
                                            base: Reg::Rbp,
                                            offset,
                                        });
                                        flattened_vregs.push(field_vreg);
                                    }
                                }
                                CfgInstData::Param { index } => {
                                    let field_count = self.struct_field_count(struct_id);
                                    for field_idx in 0..field_count {
                                        let field_vreg = self.mir.alloc_vreg();
                                        let param_slot = self.num_locals + index + field_idx;
                                        let offset = self.local_offset(param_slot);
                                        self.mir.push(X86Inst::MovRM {
                                            dst: Operand::Virtual(field_vreg),
                                            base: Reg::Rbp,
                                            offset,
                                        });
                                        flattened_vregs.push(field_vreg);
                                    }
                                }
                                CfgInstData::StructInit { .. } | CfgInstData::Call { .. } => {
                                    if let Some(field_vregs) = self.struct_field_vregs.get(arg) {
                                        flattened_vregs.extend(field_vregs.iter().copied());
                                    } else {
                                        flattened_vregs.push(self.get_vreg(*arg));
                                    }
                                }
                                _ => {
                                    flattened_vregs.push(self.get_vreg(*arg));
                                }
                            }
                        }
                        Type::Array(array_type_id) => {
                            let arg_data = &self.cfg.get_inst(*arg).data;
                            let array_len = self.array_length(array_type_id) as u32;
                            match arg_data {
                                CfgInstData::Load { slot } => {
                                    for elem_idx in 0..array_len {
                                        let elem_vreg = self.mir.alloc_vreg();
                                        let elem_slot = slot + elem_idx;
                                        let offset = self.local_offset(elem_slot);
                                        self.mir.push(X86Inst::MovRM {
                                            dst: Operand::Virtual(elem_vreg),
                                            base: Reg::Rbp,
                                            offset,
                                        });
                                        flattened_vregs.push(elem_vreg);
                                    }
                                }
                                CfgInstData::Param { index } => {
                                    for elem_idx in 0..array_len {
                                        let elem_vreg = self.mir.alloc_vreg();
                                        let param_slot = self.num_locals + index + elem_idx;
                                        let offset = self.local_offset(param_slot);
                                        self.mir.push(X86Inst::MovRM {
                                            dst: Operand::Virtual(elem_vreg),
                                            base: Reg::Rbp,
                                            offset,
                                        });
                                        flattened_vregs.push(elem_vreg);
                                    }
                                }
                                CfgInstData::ArrayInit { .. } | CfgInstData::Call { .. } => {
                                    if let Some(elem_vregs) = self.struct_field_vregs.get(arg) {
                                        flattened_vregs.extend(elem_vregs.iter().copied());
                                    } else {
                                        flattened_vregs.push(self.get_vreg(*arg));
                                    }
                                }
                                _ => {
                                    flattened_vregs.push(self.get_vreg(*arg));
                                }
                            }
                        }
                        _ => {
                            flattened_vregs.push(self.get_vreg(*arg));
                        }
                    }
                }

                let num_reg_args = flattened_vregs.len().min(ARG_REGS.len());
                let num_stack_args = flattened_vregs.len().saturating_sub(ARG_REGS.len());

                // Push stack arguments
                for arg_vreg in flattened_vregs.iter().skip(ARG_REGS.len()).rev() {
                    self.mir.push(X86Inst::MovRR {
                        dst: Operand::Physical(Reg::Rax),
                        src: Operand::Virtual(*arg_vreg),
                    });
                    self.mir.push(X86Inst::Push {
                        src: Operand::Physical(Reg::Rax),
                    });
                }

                // Push register arguments temporarily
                for arg_vreg in flattened_vregs.iter().take(num_reg_args).rev() {
                    self.mir.push(X86Inst::MovRR {
                        dst: Operand::Physical(Reg::Rax),
                        src: Operand::Virtual(*arg_vreg),
                    });
                    self.mir.push(X86Inst::Push {
                        src: Operand::Physical(Reg::Rax),
                    });
                }

                // Pop into argument registers
                for i in 0..num_reg_args {
                    self.mir.push(X86Inst::Pop {
                        dst: Operand::Physical(ARG_REGS[i]),
                    });
                }

                self.mir.push(X86Inst::CallRel {
                    symbol: name.clone(),
                });

                // Clean up stack arguments
                if num_stack_args > 0 {
                    let stack_space = (num_stack_args * 8) as i32;
                    self.mir.push(X86Inst::AddRI {
                        dst: Operand::Physical(Reg::Rsp),
                        imm: stack_space,
                    });
                }

                // Handle struct return
                if let Type::Struct(struct_id) = ty {
                    let field_count = self.struct_field_count(struct_id);
                    let mut field_vregs = Vec::new();
                    for field_idx in 0..field_count {
                        let field_vreg = self.mir.alloc_vreg();
                        if (field_idx as usize) < RET_REGS.len() {
                            self.mir.push(X86Inst::MovRR {
                                dst: Operand::Virtual(field_vreg),
                                src: Operand::Physical(RET_REGS[field_idx as usize]),
                            });
                        }
                        field_vregs.push(field_vreg);
                    }
                    self.struct_field_vregs.insert(value, field_vregs.clone());
                    if let Some(&first_vreg) = field_vregs.first() {
                        self.mir.push(X86Inst::MovRR {
                            dst: Operand::Virtual(result_vreg),
                            src: Operand::Virtual(first_vreg),
                        });
                    }
                } else {
                    self.mir.push(X86Inst::MovRR {
                        dst: Operand::Virtual(result_vreg),
                        src: Operand::Physical(Reg::Rax),
                    });
                }
            }

            CfgInstData::Intrinsic { name, args } => {
                if name == "dbg" {
                    let arg_val = args[0];
                    let arg_type = self.cfg.get_inst(arg_val).ty;

                    // Handle string type specially
                    if arg_type == Type::String {
                        // Get the fat pointer (ptr, len) from struct_field_vregs
                        if let Some(field_vregs) = self.struct_field_vregs.get(&arg_val).cloned() {
                            debug_assert_eq!(
                                field_vregs.len(),
                                2,
                                "string should have exactly 2 vregs (ptr, len)"
                            );
                            let ptr_vreg = field_vregs[0];
                            let len_vreg = field_vregs[1];

                            // Move ptr to RDI, len to RSI
                            self.mir.push(X86Inst::MovRR {
                                dst: Operand::Physical(Reg::Rdi),
                                src: Operand::Virtual(ptr_vreg),
                            });
                            self.mir.push(X86Inst::MovRR {
                                dst: Operand::Physical(Reg::Rsi),
                                src: Operand::Virtual(len_vreg),
                            });

                            // Call __rue_dbg_str
                            self.mir.push(X86Inst::CallRel {
                                symbol: "__rue_dbg_str".to_string(),
                            });
                        } else {
                            unreachable!("string value should have field vregs for fat pointer");
                        }

                        // Result is unit
                        let result_vreg = self.mir.alloc_vreg();
                        self.value_map.insert(value, result_vreg);
                    } else {
                        // Existing scalar handling
                        let arg_vreg = self.get_vreg(arg_val);

                        let runtime_fn = match arg_type {
                            Type::Bool => "__rue_dbg_bool",
                            Type::I8 | Type::I16 | Type::I32 | Type::I64 => "__rue_dbg_i64",
                            Type::U8 | Type::U16 | Type::U32 | Type::U64 => "__rue_dbg_u64",
                            _ => unreachable!("@dbg only supports scalars and strings"),
                        };

                        // Handle type extensions
                        match arg_type {
                            Type::I8 => {
                                self.mir.push(X86Inst::MovRR {
                                    dst: Operand::Physical(Reg::Rax),
                                    src: Operand::Virtual(arg_vreg),
                                });
                                self.mir.push(X86Inst::Movsx8To64 {
                                    dst: Operand::Physical(Reg::Rdi),
                                    src: Operand::Physical(Reg::Rax),
                                });
                            }
                            Type::I16 => {
                                self.mir.push(X86Inst::MovRR {
                                    dst: Operand::Physical(Reg::Rax),
                                    src: Operand::Virtual(arg_vreg),
                                });
                                self.mir.push(X86Inst::Movsx16To64 {
                                    dst: Operand::Physical(Reg::Rdi),
                                    src: Operand::Physical(Reg::Rax),
                                });
                            }
                            Type::I32 => {
                                self.mir.push(X86Inst::MovRR {
                                    dst: Operand::Physical(Reg::Rax),
                                    src: Operand::Virtual(arg_vreg),
                                });
                                self.mir.push(X86Inst::Movsx32To64 {
                                    dst: Operand::Physical(Reg::Rdi),
                                    src: Operand::Physical(Reg::Rax),
                                });
                            }
                            Type::U8 => {
                                self.mir.push(X86Inst::MovRR {
                                    dst: Operand::Physical(Reg::Rax),
                                    src: Operand::Virtual(arg_vreg),
                                });
                                self.mir.push(X86Inst::Movzx8To64 {
                                    dst: Operand::Physical(Reg::Rdi),
                                    src: Operand::Physical(Reg::Rax),
                                });
                            }
                            Type::U16 => {
                                self.mir.push(X86Inst::MovRR {
                                    dst: Operand::Physical(Reg::Rax),
                                    src: Operand::Virtual(arg_vreg),
                                });
                                self.mir.push(X86Inst::Movzx16To64 {
                                    dst: Operand::Physical(Reg::Rdi),
                                    src: Operand::Physical(Reg::Rax),
                                });
                            }
                            Type::U32 | Type::I64 | Type::U64 | Type::Bool => {
                                self.mir.push(X86Inst::MovRR {
                                    dst: Operand::Physical(Reg::Rdi),
                                    src: Operand::Virtual(arg_vreg),
                                });
                            }
                            _ => unreachable!(),
                        }

                        self.mir.push(X86Inst::CallRel {
                            symbol: runtime_fn.to_string(),
                        });

                        let result_vreg = self.mir.alloc_vreg();
                        self.value_map.insert(value, result_vreg);
                    }
                }
            }

            CfgInstData::StructInit {
                struct_id: _,
                fields,
            } => {
                let vreg = self.mir.alloc_vreg();
                self.value_map.insert(value, vreg);

                let mut field_vregs = Vec::new();
                for field in fields {
                    let field_vreg = self.get_vreg(*field);
                    field_vregs.push(field_vreg);
                }

                if let Some(&first_vreg) = field_vregs.first() {
                    self.mir.push(X86Inst::MovRR {
                        dst: Operand::Virtual(vreg),
                        src: Operand::Virtual(first_vreg),
                    });
                } else {
                    self.mir.push(X86Inst::MovRI32 {
                        dst: Operand::Virtual(vreg),
                        imm: 0,
                    });
                }

                self.struct_field_vregs.insert(value, field_vregs);
            }

            CfgInstData::FieldGet {
                base,
                struct_id,
                field_index,
            } => {
                let vreg = self.mir.alloc_vreg();
                self.value_map.insert(value, vreg);

                // Try to trace back through any chain of FieldGets to find
                // the original Load or Param. This handles nested struct field access.
                let this_offset = self.struct_field_slot_offset(*struct_id, *field_index);
                if let Some((base_kind, accumulated_offset)) = self.trace_field_chain(*base) {
                    let total_offset = accumulated_offset + this_offset;
                    match base_kind {
                        FieldChainBase::Load { slot } => {
                            // Chain originates from a Load - compute offset from local slot
                            let actual_slot = slot + total_offset;
                            let offset = self.local_offset(actual_slot);
                            self.mir.push(X86Inst::MovRM {
                                dst: Operand::Virtual(vreg),
                                base: Reg::Rbp,
                                offset,
                            });
                        }
                        FieldChainBase::Param { index } => {
                            // Chain originates from a Param - compute offset from param slot
                            let param_slot = self.num_locals + index + total_offset;
                            let offset = self.local_offset(param_slot);
                            self.mir.push(X86Inst::MovRM {
                                dst: Operand::Virtual(vreg),
                                base: Reg::Rbp,
                                offset,
                            });
                        }
                    }
                } else {
                    // For other sources (BlockParam, StructInit, Call), use field vregs
                    if let Some(field_vregs) = self.struct_field_vregs.get(base).cloned() {
                        if let Some(&field_vreg) = field_vregs.get(*field_index as usize) {
                            self.mir.push(X86Inst::MovRR {
                                dst: Operand::Virtual(vreg),
                                src: Operand::Virtual(field_vreg),
                            });
                        } else {
                            // Fallback if field_index out of range
                            let base_vreg = self.get_vreg(*base);
                            self.mir.push(X86Inst::MovRR {
                                dst: Operand::Virtual(vreg),
                                src: Operand::Virtual(base_vreg),
                            });
                        }
                    } else {
                        // Fallback for cases without field vregs (e.g., single-field struct)
                        let base_vreg = self.get_vreg(*base);
                        self.mir.push(X86Inst::MovRR {
                            dst: Operand::Virtual(vreg),
                            src: Operand::Virtual(base_vreg),
                        });
                    }
                }
            }

            CfgInstData::FieldSet {
                slot,
                struct_id,
                field_index,
                value: val,
            } => {
                let val_vreg = self.get_vreg(*val);
                let field_slot_offset = self.struct_field_slot_offset(*struct_id, *field_index);
                let actual_slot = slot + field_slot_offset;
                let offset = self.local_offset(actual_slot);
                self.mir.push(X86Inst::MovMR {
                    base: Reg::Rbp,
                    offset,
                    src: Operand::Virtual(val_vreg),
                });
            }

            CfgInstData::ArrayInit {
                array_type_id: _,
                elements,
            } => {
                // Array is stored in local slots; we just create vregs for elements.
                // The actual storage is handled by the Alloc that precedes this.
                // For now, just create a dummy vreg - arrays are passed by loading from slots.
                let vreg = self.mir.alloc_vreg();
                self.value_map.insert(value, vreg);

                // Store element vregs for later IndexGet access
                let element_vregs: Vec<VReg> = elements.iter().map(|e| self.get_vreg(*e)).collect();
                self.struct_field_vregs.insert(value, element_vregs);

                // Move 0 into vreg as placeholder (array base doesn't have a single value)
                self.mir.push(X86Inst::MovRI32 {
                    dst: Operand::Virtual(vreg),
                    imm: 0,
                });
            }

            CfgInstData::IndexGet {
                base,
                array_type_id,
                index,
            } => {
                let vreg = self.mir.alloc_vreg();
                self.value_map.insert(value, vreg);

                // Calculate the slot stride for this array's elements
                let elem_slot_count = self.array_element_slot_count(*array_type_id);

                let base_data = &self.cfg.get_inst(*base).data;
                match base_data {
                    CfgInstData::Load { slot } => {
                        // Base is a load from a local variable - use dynamic indexing
                        let index_vreg = self.get_vreg(*index);

                        // Emit runtime bounds check
                        let array_length = self.array_length(*array_type_id);
                        self.emit_bounds_check(index_vreg, array_length);

                        let base_vreg = self.mir.alloc_vreg();
                        let base_offset = self.local_offset(*slot);

                        // Multiply index by element slot count, then by 8
                        let scaled_index = self.mir.alloc_vreg();
                        self.mir.push(X86Inst::MovRR {
                            dst: Operand::Virtual(scaled_index),
                            src: Operand::Virtual(index_vreg),
                        });

                        if elem_slot_count == 1 {
                            // Simple case: just shift by 3 (multiply by 8)
                            let shift_count = self.mir.alloc_vreg();
                            self.mir.push(X86Inst::MovRI32 {
                                dst: Operand::Virtual(shift_count),
                                imm: 3,
                            });
                            self.mir.push(X86Inst::Shl {
                                dst: Operand::Virtual(scaled_index),
                                count: Operand::Virtual(shift_count),
                            });
                        } else {
                            // Multiply by elem_slot_count * 8
                            let stride_vreg = self.mir.alloc_vreg();
                            self.mir.push(X86Inst::MovRI64 {
                                dst: Operand::Virtual(stride_vreg),
                                imm: (elem_slot_count * 8) as i64,
                            });
                            self.mir.push(X86Inst::ImulRR64 {
                                dst: Operand::Virtual(scaled_index),
                                src: Operand::Virtual(stride_vreg),
                            });
                        }

                        // Compute base address
                        self.mir.push(X86Inst::Lea {
                            dst: Operand::Virtual(base_vreg),
                            base: Reg::Rbp,
                            index: None,
                            scale: 1,
                            disp: base_offset,
                        });

                        // Subtract scaled index (stack grows down, array laid out sequentially)
                        self.mir.push(X86Inst::SubRR64 {
                            dst: Operand::Virtual(base_vreg),
                            src: Operand::Virtual(scaled_index),
                        });

                        // Load from computed address
                        self.mir.push(X86Inst::MovRMIndexed {
                            dst: Operand::Virtual(vreg),
                            base: base_vreg,
                            offset: 0,
                        });
                    }
                    CfgInstData::Param { index: param_index } => {
                        // Base is a function parameter - array elements are in consecutive param slots
                        let index_vreg = self.get_vreg(*index);

                        // Emit runtime bounds check
                        let array_length = self.array_length(*array_type_id);
                        self.emit_bounds_check(index_vreg, array_length);

                        // Array parameter elements are stored at consecutive slots starting at
                        // num_locals + param_index
                        let base_slot = self.num_locals + *param_index as u32;
                        let base_offset = self.local_offset(base_slot);

                        // Multiply index by element slot count, then by 8
                        let scaled_index = self.mir.alloc_vreg();
                        self.mir.push(X86Inst::MovRR {
                            dst: Operand::Virtual(scaled_index),
                            src: Operand::Virtual(index_vreg),
                        });

                        if elem_slot_count == 1 {
                            let shift_count = self.mir.alloc_vreg();
                            self.mir.push(X86Inst::MovRI32 {
                                dst: Operand::Virtual(shift_count),
                                imm: 3,
                            });
                            self.mir.push(X86Inst::Shl {
                                dst: Operand::Virtual(scaled_index),
                                count: Operand::Virtual(shift_count),
                            });
                        } else {
                            let stride_vreg = self.mir.alloc_vreg();
                            self.mir.push(X86Inst::MovRI64 {
                                dst: Operand::Virtual(stride_vreg),
                                imm: (elem_slot_count * 8) as i64,
                            });
                            self.mir.push(X86Inst::ImulRR64 {
                                dst: Operand::Virtual(scaled_index),
                                src: Operand::Virtual(stride_vreg),
                            });
                        }

                        // Compute base address
                        let base_vreg = self.mir.alloc_vreg();
                        self.mir.push(X86Inst::Lea {
                            dst: Operand::Virtual(base_vreg),
                            base: Reg::Rbp,
                            index: None,
                            scale: 1,
                            disp: base_offset,
                        });

                        // Subtract scaled index (stack grows down, array laid out sequentially)
                        self.mir.push(X86Inst::SubRR64 {
                            dst: Operand::Virtual(base_vreg),
                            src: Operand::Virtual(scaled_index),
                        });

                        // Load from computed address
                        self.mir.push(X86Inst::MovRMIndexed {
                            dst: Operand::Virtual(vreg),
                            base: base_vreg,
                            offset: 0,
                        });
                    }
                    CfgInstData::IndexGet {
                        base: outer_base,
                        array_type_id: outer_array_type_id,
                        index: outer_index,
                    } => {
                        // Chained indexing: arr[i][j] where base is arr[i]
                        // We need to find the ultimate base (Load or Param) and compute the combined offset
                        let outer_base_data = &self.cfg.get_inst(*outer_base).data;

                        // Get the slot count for the outer array's elements
                        let outer_elem_slot_count =
                            self.array_element_slot_count(*outer_array_type_id);

                        let index_vreg = self.get_vreg(*index);
                        let outer_index_vreg = self.get_vreg(*outer_index);

                        // Emit runtime bounds check for inner index
                        let array_length = self.array_length(*array_type_id);
                        self.emit_bounds_check(index_vreg, array_length);

                        match outer_base_data {
                            CfgInstData::Load { slot } => {
                                let base_offset = self.local_offset(*slot);

                                // Combined offset = outer_index * outer_elem_slots * 8 + inner_index * elem_slots * 8
                                // For [[i32; 2]; 2], outer_elem_slots=2, elem_slots=1
                                // arr[1][1] = base - (1 * 2 * 8) - (1 * 1 * 8) = base - 24

                                // Calculate outer offset
                                let outer_scaled = self.mir.alloc_vreg();
                                self.mir.push(X86Inst::MovRR {
                                    dst: Operand::Virtual(outer_scaled),
                                    src: Operand::Virtual(outer_index_vreg),
                                });
                                if outer_elem_slot_count == 1 {
                                    let shift_count = self.mir.alloc_vreg();
                                    self.mir.push(X86Inst::MovRI32 {
                                        dst: Operand::Virtual(shift_count),
                                        imm: 3,
                                    });
                                    self.mir.push(X86Inst::Shl {
                                        dst: Operand::Virtual(outer_scaled),
                                        count: Operand::Virtual(shift_count),
                                    });
                                } else {
                                    let stride_vreg = self.mir.alloc_vreg();
                                    self.mir.push(X86Inst::MovRI64 {
                                        dst: Operand::Virtual(stride_vreg),
                                        imm: (outer_elem_slot_count * 8) as i64,
                                    });
                                    self.mir.push(X86Inst::ImulRR64 {
                                        dst: Operand::Virtual(outer_scaled),
                                        src: Operand::Virtual(stride_vreg),
                                    });
                                }

                                // Calculate inner offset
                                let inner_scaled = self.mir.alloc_vreg();
                                self.mir.push(X86Inst::MovRR {
                                    dst: Operand::Virtual(inner_scaled),
                                    src: Operand::Virtual(index_vreg),
                                });
                                if elem_slot_count == 1 {
                                    let shift_count = self.mir.alloc_vreg();
                                    self.mir.push(X86Inst::MovRI32 {
                                        dst: Operand::Virtual(shift_count),
                                        imm: 3,
                                    });
                                    self.mir.push(X86Inst::Shl {
                                        dst: Operand::Virtual(inner_scaled),
                                        count: Operand::Virtual(shift_count),
                                    });
                                } else {
                                    let stride_vreg = self.mir.alloc_vreg();
                                    self.mir.push(X86Inst::MovRI64 {
                                        dst: Operand::Virtual(stride_vreg),
                                        imm: (elem_slot_count * 8) as i64,
                                    });
                                    self.mir.push(X86Inst::ImulRR64 {
                                        dst: Operand::Virtual(inner_scaled),
                                        src: Operand::Virtual(stride_vreg),
                                    });
                                }

                                // Combine offsets
                                let total_offset = self.mir.alloc_vreg();
                                self.mir.push(X86Inst::MovRR {
                                    dst: Operand::Virtual(total_offset),
                                    src: Operand::Virtual(outer_scaled),
                                });
                                self.mir.push(X86Inst::AddRR64 {
                                    dst: Operand::Virtual(total_offset),
                                    src: Operand::Virtual(inner_scaled),
                                });

                                // Compute base address
                                let addr_vreg = self.mir.alloc_vreg();
                                self.mir.push(X86Inst::Lea {
                                    dst: Operand::Virtual(addr_vreg),
                                    base: Reg::Rbp,
                                    index: None,
                                    scale: 1,
                                    disp: base_offset,
                                });

                                // Subtract combined offset
                                self.mir.push(X86Inst::SubRR64 {
                                    dst: Operand::Virtual(addr_vreg),
                                    src: Operand::Virtual(total_offset),
                                });

                                // Load from computed address
                                self.mir.push(X86Inst::MovRMIndexed {
                                    dst: Operand::Virtual(vreg),
                                    base: addr_vreg,
                                    offset: 0,
                                });
                            }
                            _ => {
                                // Fallback for deeper nesting or other cases
                                self.mir.push(X86Inst::MovRI32 {
                                    dst: Operand::Virtual(vreg),
                                    imm: 0,
                                });
                            }
                        }
                    }
                    CfgInstData::FieldGet {
                        base: struct_base,
                        struct_id,
                        field_index,
                    } => {
                        // Indexing into an array that is a struct field
                        // The struct is loaded from a slot, and the array starts at field_slot_offset within
                        let struct_base_data = &self.cfg.get_inst(*struct_base).data;
                        let index_vreg = self.get_vreg(*index);

                        // Emit runtime bounds check
                        let array_length = self.array_length(*array_type_id);
                        self.emit_bounds_check(index_vreg, array_length);

                        // Calculate the actual slot offset for this field
                        let field_slot_offset =
                            self.struct_field_slot_offset(*struct_id, *field_index);

                        match struct_base_data {
                            CfgInstData::Load { slot } => {
                                // Array starts at slot + field_slot_offset
                                let array_base_slot = slot + field_slot_offset;
                                let base_offset = self.local_offset(array_base_slot);

                                // Multiply index by element slot count, then by 8
                                let scaled_index = self.mir.alloc_vreg();
                                self.mir.push(X86Inst::MovRR {
                                    dst: Operand::Virtual(scaled_index),
                                    src: Operand::Virtual(index_vreg),
                                });

                                if elem_slot_count == 1 {
                                    let shift_count = self.mir.alloc_vreg();
                                    self.mir.push(X86Inst::MovRI32 {
                                        dst: Operand::Virtual(shift_count),
                                        imm: 3,
                                    });
                                    self.mir.push(X86Inst::Shl {
                                        dst: Operand::Virtual(scaled_index),
                                        count: Operand::Virtual(shift_count),
                                    });
                                } else {
                                    let stride_vreg = self.mir.alloc_vreg();
                                    self.mir.push(X86Inst::MovRI64 {
                                        dst: Operand::Virtual(stride_vreg),
                                        imm: (elem_slot_count * 8) as i64,
                                    });
                                    self.mir.push(X86Inst::ImulRR64 {
                                        dst: Operand::Virtual(scaled_index),
                                        src: Operand::Virtual(stride_vreg),
                                    });
                                }

                                // Compute base address
                                let base_vreg = self.mir.alloc_vreg();
                                self.mir.push(X86Inst::Lea {
                                    dst: Operand::Virtual(base_vreg),
                                    base: Reg::Rbp,
                                    index: None,
                                    scale: 1,
                                    disp: base_offset,
                                });

                                // Subtract scaled index
                                self.mir.push(X86Inst::SubRR64 {
                                    dst: Operand::Virtual(base_vreg),
                                    src: Operand::Virtual(scaled_index),
                                });

                                // Load from computed address
                                self.mir.push(X86Inst::MovRMIndexed {
                                    dst: Operand::Virtual(vreg),
                                    base: base_vreg,
                                    offset: 0,
                                });
                            }
                            _ => {
                                // Fallback
                                self.mir.push(X86Inst::MovRI32 {
                                    dst: Operand::Virtual(vreg),
                                    imm: 0,
                                });
                            }
                        }
                    }
                    _ => {
                        // For other sources (ArrayInit), use element vregs if index is constant
                        let index_inst = &self.cfg.get_inst(*index).data;
                        if let CfgInstData::Const(idx) = index_inst {
                            if let Some(element_vregs) = self.struct_field_vregs.get(base).cloned()
                            {
                                if let Some(&elem_vreg) = element_vregs.get(*idx as usize) {
                                    self.mir.push(X86Inst::MovRR {
                                        dst: Operand::Virtual(vreg),
                                        src: Operand::Virtual(elem_vreg),
                                    });
                                    return;
                                }
                            }
                        }
                        // Fallback - shouldn't happen with proper constant folding
                        self.mir.push(X86Inst::MovRI32 {
                            dst: Operand::Virtual(vreg),
                            imm: 0,
                        });
                    }
                }
            }

            CfgInstData::IndexSet {
                slot,
                array_type_id,
                index,
                value: val,
            } => {
                let val_vreg = self.get_vreg(*val);
                let index_vreg = self.get_vreg(*index);

                // Emit runtime bounds check
                let array_length = self.array_length(*array_type_id);
                self.emit_bounds_check(index_vreg, array_length);

                // Similar to IndexGet but store instead of load
                let scaled_index = self.mir.alloc_vreg();
                self.mir.push(X86Inst::MovRR {
                    dst: Operand::Virtual(scaled_index),
                    src: Operand::Virtual(index_vreg),
                });
                let eight = self.mir.alloc_vreg();
                self.mir.push(X86Inst::MovRI32 {
                    dst: Operand::Virtual(eight),
                    imm: 3,
                });
                self.mir.push(X86Inst::Shl {
                    dst: Operand::Virtual(scaled_index),
                    count: Operand::Virtual(eight),
                });

                let base_offset = self.local_offset(*slot);
                let addr_vreg = self.mir.alloc_vreg();
                self.mir.push(X86Inst::Lea {
                    dst: Operand::Virtual(addr_vreg),
                    base: Reg::Rbp,
                    index: None,
                    scale: 1,
                    disp: base_offset,
                });
                self.mir.push(X86Inst::SubRR64 {
                    dst: Operand::Virtual(addr_vreg),
                    src: Operand::Virtual(scaled_index),
                });

                self.mir.push(X86Inst::MovMRIndexed {
                    base: addr_vreg,
                    offset: 0,
                    src: Operand::Virtual(val_vreg),
                });
            }

            CfgInstData::EnumVariant { variant_index, .. } => {
                // Enum variants are represented as their discriminant (variant index)
                let vreg = self.mir.alloc_vreg();
                self.value_map.insert(value, vreg);

                self.mir.push(X86Inst::MovRI32 {
                    dst: Operand::Virtual(vreg),
                    imm: *variant_index as i32,
                });
            }
        }
    }

    /// Check if a comparison should use unsigned comparison instructions.
    ///
    /// Sema guarantees both operands have the same signedness, so we only need to check one.
    fn is_unsigned_comparison(&self, lhs: CfgValue) -> bool {
        self.cfg.get_inst(lhs).ty.is_unsigned()
    }

    /// Emit overflow check based on the type.
    ///
    /// For 32/64-bit types, we can use the CPU flags directly:
    /// - Signed (i32, i64): Use OF (overflow flag) via JNO
    /// - Unsigned (u32, u64): Use CF (carry flag) via JAE (= JNC)
    ///
    /// For sub-word types (8/16-bit), the arithmetic is done in 32/64-bit registers,
    /// so we need to check if the result fits in the original type's range.
    fn emit_overflow_check(&mut self, ty: Type, result_vreg: VReg) {
        let ok_label = self.new_label();

        match ty {
            // 32-bit and 64-bit unsigned: check carry flag
            Type::U32 | Type::U64 => {
                self.mir.push(X86Inst::Jae { label: ok_label });
            }
            // 32-bit and 64-bit signed: check overflow flag
            Type::I32 | Type::I64 => {
                self.mir.push(X86Inst::Jno { label: ok_label });
            }
            // Sub-word unsigned types: check if result fits in range [0, max]
            Type::U8 => {
                // Result must be <= 255
                self.mir.push(X86Inst::CmpRI {
                    src: Operand::Virtual(result_vreg),
                    imm: 255,
                });
                // Jump if below or equal (unsigned)
                self.mir.push(X86Inst::Jbe { label: ok_label });
            }
            Type::U16 => {
                // Result must be <= 65535
                let max_vreg = self.mir.alloc_vreg();
                self.mir.push(X86Inst::MovRI32 {
                    dst: Operand::Virtual(max_vreg),
                    imm: 65535,
                });
                self.mir.push(X86Inst::CmpRR {
                    src1: Operand::Virtual(result_vreg),
                    src2: Operand::Virtual(max_vreg),
                });
                // Jump if below or equal (unsigned)
                self.mir.push(X86Inst::Jbe { label: ok_label });
            }
            // Sub-word signed types: check if result fits in range [min, max]
            Type::I8 => {
                // For i8: result must be in [-128, 127]
                // Sign-extend to 32-bit and compare with original
                // If they differ, overflow occurred
                let sext_vreg = self.mir.alloc_vreg();
                self.mir.push(X86Inst::MovRR {
                    dst: Operand::Virtual(sext_vreg),
                    src: Operand::Virtual(result_vreg),
                });
                self.mir.push(X86Inst::Movsx8To64 {
                    dst: Operand::Virtual(sext_vreg),
                    src: Operand::Virtual(sext_vreg),
                });
                self.mir.push(X86Inst::CmpRR {
                    src1: Operand::Virtual(result_vreg),
                    src2: Operand::Virtual(sext_vreg),
                });
                self.mir.push(X86Inst::Jz { label: ok_label });
            }
            Type::I16 => {
                // For i16: result must be in [-32768, 32767]
                // Sign-extend to 32-bit and compare with original
                let sext_vreg = self.mir.alloc_vreg();
                self.mir.push(X86Inst::MovRR {
                    dst: Operand::Virtual(sext_vreg),
                    src: Operand::Virtual(result_vreg),
                });
                self.mir.push(X86Inst::Movsx16To64 {
                    dst: Operand::Virtual(sext_vreg),
                    src: Operand::Virtual(sext_vreg),
                });
                self.mir.push(X86Inst::CmpRR {
                    src1: Operand::Virtual(result_vreg),
                    src2: Operand::Virtual(sext_vreg),
                });
                self.mir.push(X86Inst::Jz { label: ok_label });
            }
            // Other types (bool, unit, struct, etc.) don't have arithmetic
            _ => {
                // No overflow check needed
                return;
            }
        }

        // Overflow occurred - call panic handler
        self.mir.push(X86Inst::CallRel {
            symbol: "__rue_overflow".to_string(),
        });
        self.mir.push(X86Inst::Label { id: ok_label });
    }

    /// Emit a comparison instruction.
    fn emit_comparison<F>(&mut self, value: CfgValue, lhs: CfgValue, rhs: CfgValue, emit_setcc: F)
    where
        F: FnOnce(&mut X86Mir, VReg),
    {
        let vreg = self.mir.alloc_vreg();
        self.value_map.insert(value, vreg);

        let lhs_vreg = self.get_vreg(lhs);
        let rhs_vreg = self.get_vreg(rhs);

        // Use 64-bit compare for i64/u64 types
        let lhs_ty = self.cfg.get_inst(lhs).ty;
        if matches!(lhs_ty, Type::I64 | Type::U64) {
            self.mir.push(X86Inst::Cmp64RR {
                src1: Operand::Virtual(lhs_vreg),
                src2: Operand::Virtual(rhs_vreg),
            });
        } else {
            self.mir.push(X86Inst::CmpRR {
                src1: Operand::Virtual(lhs_vreg),
                src2: Operand::Virtual(rhs_vreg),
            });
        }
        emit_setcc(&mut self.mir, vreg);
        self.mir.push(X86Inst::Movzx {
            dst: Operand::Virtual(vreg),
            src: Operand::Virtual(vreg),
        });
    }

    /// Lower a block terminator.
    fn lower_terminator(&mut self, block: &BasicBlock) {
        match &block.terminator {
            Terminator::Goto { target, args } => {
                // Copy args to target's block params
                for (i, &arg) in args.iter().enumerate() {
                    let arg_type = self.cfg.get_inst(arg).ty;
                    if matches!(arg_type, Type::Struct(_)) {
                        // For struct args, copy all field vregs
                        self.copy_struct_to_block_param(arg, *target, i as u32);
                    } else {
                        // For scalar args, just copy the single vreg
                        let arg_vreg = self.get_vreg(arg);
                        let param_vreg = self.block_param_vregs[&(*target, i as u32)];
                        self.mir.push(X86Inst::MovRR {
                            dst: Operand::Virtual(param_vreg),
                            src: Operand::Virtual(arg_vreg),
                        });
                    }
                }

                // Jump to target (unless it's the next block)
                let next_block_id = BlockId::from_raw(block.id.as_u32() + 1);
                if *target != next_block_id {
                    self.mir.push(X86Inst::Jmp {
                        label: self.block_label(*target),
                    });
                }
            }

            Terminator::Branch {
                cond,
                then_block,
                then_args,
                else_block,
                else_args,
            } => {
                let cond_vreg = self.get_vreg(*cond);

                // Generate a unique label for the else path argument setup
                let else_setup_label = self.new_label();

                // Test condition
                self.mir.push(X86Inst::CmpRI {
                    src: Operand::Virtual(cond_vreg),
                    imm: 0,
                });

                // If false (zero), jump to else setup (where we copy else_args)
                self.mir.push(X86Inst::Jz {
                    label: else_setup_label.clone(),
                });

                // Copy then_args to then_block's params
                for (i, &arg) in then_args.iter().enumerate() {
                    let arg_type = self.cfg.get_inst(arg).ty;
                    if matches!(arg_type, Type::Struct(_)) {
                        // For struct args, copy all field vregs
                        self.copy_struct_to_block_param(arg, *then_block, i as u32);
                    } else {
                        // For scalar args, just copy the single vreg
                        let arg_vreg = self.get_vreg(arg);
                        let param_vreg = self.block_param_vregs[&(*then_block, i as u32)];
                        self.mir.push(X86Inst::MovRR {
                            dst: Operand::Virtual(param_vreg),
                            src: Operand::Virtual(arg_vreg),
                        });
                    }
                }

                // Jump to then block
                self.mir.push(X86Inst::Jmp {
                    label: self.block_label(*then_block),
                });

                // Else setup: copy else_args to else_block's params
                self.mir.push(X86Inst::Label {
                    id: else_setup_label,
                });
                for (i, &arg) in else_args.iter().enumerate() {
                    let arg_type = self.cfg.get_inst(arg).ty;
                    if matches!(arg_type, Type::Struct(_)) {
                        // For struct args, copy all field vregs
                        self.copy_struct_to_block_param(arg, *else_block, i as u32);
                    } else {
                        // For scalar args, just copy the single vreg
                        let arg_vreg = self.get_vreg(arg);
                        let param_vreg = self.block_param_vregs[&(*else_block, i as u32)];
                        self.mir.push(X86Inst::MovRR {
                            dst: Operand::Virtual(param_vreg),
                            src: Operand::Virtual(arg_vreg),
                        });
                    }
                }

                // Jump to else block (or fall through if next)
                let next_block_id = BlockId::from_raw(block.id.as_u32() + 1);
                if *else_block != next_block_id {
                    self.mir.push(X86Inst::Jmp {
                        label: self.block_label(*else_block),
                    });
                }
            }

            Terminator::Switch {
                scrutinee,
                cases,
                default,
            } => {
                let scrutinee_vreg = self.get_vreg(*scrutinee);

                // Generate comparison and jump for each case
                for (value, target) in cases {
                    // Load case value into a register to handle full u64 range
                    // Cast to i64 to preserve bit pattern
                    let case_vreg = self.mir.alloc_vreg();
                    self.mir.push(X86Inst::MovRI64 {
                        dst: Operand::Virtual(case_vreg),
                        imm: *value as i64,
                    });
                    self.mir.push(X86Inst::CmpRR {
                        src1: Operand::Virtual(scrutinee_vreg),
                        src2: Operand::Virtual(case_vreg),
                    });
                    self.mir.push(X86Inst::Jz {
                        label: self.block_label(*target),
                    });
                }

                // Fall through to default
                self.mir.push(X86Inst::Jmp {
                    label: self.block_label(*default),
                });
            }

            Terminator::Return { value } => {
                // Handle `return;` without expression (unit-returning functions)
                let Some(value) = value else {
                    if self.has_frame {
                        self.emit_epilogue();
                    }
                    self.mir.push(X86Inst::Ret);
                    return;
                };

                let return_type = self.cfg.return_type();

                if self.fn_name == "main" {
                    let val_vreg = self.get_vreg(*value);
                    self.mir.push(X86Inst::MovRR {
                        dst: Operand::Physical(Reg::Rdi),
                        src: Operand::Virtual(val_vreg),
                    });
                    // Don't emit epilogue before __rue_exit - it never returns, and
                    // restoring the frame would break stack alignment for the call
                    // (after pop rbp, rsp is 8 mod 16; call pushes 8 more, making
                    // it 0 mod 16 at callee entry, violating SysV ABI).
                    self.mir.push(X86Inst::CallRel {
                        symbol: "__rue_exit".to_string(),
                    });
                } else if let Type::Struct(struct_id) = return_type {
                    // Return struct in registers
                    let field_count = self.struct_field_count(struct_id);
                    let value_data = &self.cfg.get_inst(*value).data;

                    match value_data {
                        CfgInstData::StructInit { .. }
                        | CfgInstData::Call { .. }
                        | CfgInstData::BlockParam { .. } => {
                            // Use field vregs from cache (populated for BlockParam, StructInit, Call)
                            if let Some(field_vregs) = self.struct_field_vregs.get(value).cloned() {
                                // Move field values to return registers in REVERSE order.
                                // This is important because register allocation uses Rax as
                                // scratch when loading spilled values. By moving to Rax last,
                                // we avoid clobbering it with scratch loads for later fields.
                                for (i, field_vreg) in field_vregs.iter().enumerate().rev() {
                                    if i < RET_REGS.len() {
                                        self.mir.push(X86Inst::MovRR {
                                            dst: Operand::Physical(RET_REGS[i]),
                                            src: Operand::Virtual(*field_vreg),
                                        });
                                    }
                                }
                            }
                        }
                        CfgInstData::Param { index } => {
                            for field_idx in 0..field_count {
                                let param_slot = self.num_locals + index + field_idx;
                                let offset = self.local_offset(param_slot);
                                self.mir.push(X86Inst::MovRM {
                                    dst: Operand::Physical(RET_REGS[field_idx as usize]),
                                    base: Reg::Rbp,
                                    offset,
                                });
                            }
                        }
                        CfgInstData::Load { slot } => {
                            for field_idx in 0..field_count {
                                let actual_slot = slot + field_idx;
                                let offset = self.local_offset(actual_slot);
                                self.mir.push(X86Inst::MovRM {
                                    dst: Operand::Physical(RET_REGS[field_idx as usize]),
                                    base: Reg::Rbp,
                                    offset,
                                });
                            }
                        }
                        _ => {
                            let val_vreg = self.get_vreg(*value);
                            self.mir.push(X86Inst::MovRR {
                                dst: Operand::Physical(Reg::Rax),
                                src: Operand::Virtual(val_vreg),
                            });
                        }
                    }

                    if self.has_frame {
                        self.emit_epilogue();
                    }
                    self.mir.push(X86Inst::Ret);
                } else {
                    let val_vreg = self.get_vreg(*value);
                    self.mir.push(X86Inst::MovRR {
                        dst: Operand::Physical(Reg::Rax),
                        src: Operand::Virtual(val_vreg),
                    });
                    if self.has_frame {
                        self.emit_epilogue();
                    }
                    self.mir.push(X86Inst::Ret);
                }
            }

            Terminator::Unreachable => {
                // Nothing to emit - unreachable code
            }

            Terminator::None => {
                panic!("block has no terminator");
            }
        }
    }

    /// Get the vreg for a CFG value.
    fn get_vreg(&mut self, value: CfgValue) -> VReg {
        if let Some(&vreg) = self.value_map.get(&value) {
            return vreg;
        }

        // Not yet lowered - lower it now
        self.lower_value(value);

        self.value_map
            .get(&value)
            .copied()
            .expect("value should have been lowered")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rue_air::Sema;
    use rue_cfg::CfgBuilder;
    use rue_intern::Interner;
    use rue_lexer::Lexer;
    use rue_parser::Parser;
    use rue_rir::AstGen;

    fn lower_to_mir(source: &str) -> X86Mir {
        let mut lexer = Lexer::new(source);
        let tokens = lexer.tokenize().unwrap();
        let mut parser = Parser::new(tokens);
        let ast = parser.parse().unwrap();

        let mut interner = Interner::new();
        let astgen = AstGen::new(&ast, &mut interner);
        let rir = astgen.generate();

        let sema = Sema::new(&rir, &mut interner);
        let output = sema.analyze_all().unwrap();

        let func = &output.functions[0];
        let struct_defs = &output.struct_defs;
        let array_types = &output.array_types;
        let strings = &output.strings;
        let cfg_output =
            CfgBuilder::build(&func.air, func.num_locals, func.num_param_slots, &func.name);

        CfgLower::new(&cfg_output.cfg, struct_defs, array_types, strings).lower()
    }

    #[test]
    fn test_simple_return() {
        let mir = lower_to_mir("fn main() -> i32 { 42 }");
        assert!(!mir.instructions().is_empty());
    }

    #[test]
    fn test_arithmetic() {
        let mir = lower_to_mir("fn main() -> i32 { 1 + 2 }");
        assert!(!mir.instructions().is_empty());
    }

    #[test]
    fn test_if_else() {
        let mir = lower_to_mir("fn main() -> i32 { if true { 1 } else { 2 } }");
        assert!(!mir.instructions().is_empty());
    }
}
