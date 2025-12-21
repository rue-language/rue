//! CFG to Aarch64Mir lowering.
//!
//! This module converts CFG (explicit control flow graph) to Aarch64Mir
//! (AArch64 instructions with virtual registers).

use std::collections::HashMap;

use rue_air::{ArrayTypeDef, ArrayTypeId};
use rue_cfg::{
    BasicBlock, BlockId, Cfg, CfgInstData, CfgValue, StructDef, StructId, Terminator, Type,
};

use super::mir::{Aarch64Inst, Aarch64Mir, Cond, LabelId, Operand, Reg, VReg};

/// Argument passing registers per AAPCS64.
const ARG_REGS: [Reg; 8] = [
    Reg::X0,
    Reg::X1,
    Reg::X2,
    Reg::X3,
    Reg::X4,
    Reg::X5,
    Reg::X6,
    Reg::X7,
];

/// Return value registers per AAPCS64.
const RET_REGS: [Reg; 8] = [
    Reg::X0,
    Reg::X1,
    Reg::X2,
    Reg::X3,
    Reg::X4,
    Reg::X5,
    Reg::X6,
    Reg::X7,
];

/// CFG to Aarch64Mir lowering.
pub struct CfgLower<'a> {
    cfg: &'a Cfg,
    struct_defs: &'a [StructDef],
    /// Array type definitions for bounds checking.
    array_types: &'a [ArrayTypeDef],
    mir: Aarch64Mir,
    /// Maps CFG values to vregs
    value_map: HashMap<CfgValue, VReg>,
    /// Maps block parameters to vregs (block_id, param_index) -> vreg
    block_param_vregs: HashMap<(BlockId, u32), VReg>,
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
    ) -> Self {
        let num_locals = cfg.num_locals();
        let num_params = cfg.num_params();
        Self {
            cfg,
            struct_defs,
            array_types,
            mir: Aarch64Mir::new(),
            value_map: HashMap::new(),
            block_param_vregs: HashMap::new(),
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
        self.array_types
            .get(array_type_id.0 as usize)
            .map(|def| def.length)
            .unwrap_or(0)
    }

    /// Calculate the stack offset for a local variable slot.
    /// On AArch64, we use negative offsets from FP (x29).
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
        self.mir.push(Aarch64Inst::MovImm {
            dst: Operand::Virtual(length_vreg),
            imm: length as i64,
        });

        // Compare index (unsigned) against length
        self.mir.push(Aarch64Inst::CmpRR {
            src1: Operand::Virtual(index_vreg),
            src2: Operand::Virtual(length_vreg),
        });

        // If index < length (unsigned), branch to ok label; otherwise call bounds check
        let ok_label = self.mir.alloc_label();
        self.mir.push(Aarch64Inst::BCond {
            cond: Cond::Lo, // Lower (unsigned <)
            label: ok_label,
        });

        // Call the bounds check error handler (never returns)
        self.mir.push(Aarch64Inst::Bl {
            symbol: "__rue_bounds_check".to_string(),
        });

        // Continue with valid access
        self.mir.push(Aarch64Inst::Label { id: ok_label });
    }

    /// Get the label for a block.
    fn block_label(&self, block_id: BlockId) -> LabelId {
        Aarch64Mir::block_label(block_id.as_u32())
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
                    self.mir.push(Aarch64Inst::Ldr {
                        dst: Operand::Virtual(vreg),
                        base: Reg::Fp,
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
                    self.mir.push(Aarch64Inst::Ldr {
                        dst: Operand::Virtual(vreg),
                        base: Reg::Fp,
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
                self.mir.push(Aarch64Inst::MovRR {
                    dst: Operand::Virtual(*dst_vreg),
                    src: Operand::Virtual(*src_vreg),
                });
            }
        }
    }

    /// Lower CFG to Aarch64Mir.
    pub fn lower(mut self) -> Aarch64Mir {
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
            self.mir.push(Aarch64Inst::Label {
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

                self.mir.push(Aarch64Inst::MovImm {
                    dst: Operand::Virtual(vreg),
                    imm: *v,
                });
            }

            CfgInstData::BoolConst(v) => {
                let vreg = self.mir.alloc_vreg();
                self.value_map.insert(value, vreg);

                self.mir.push(Aarch64Inst::MovImm {
                    dst: Operand::Virtual(vreg),
                    imm: if *v { 1 } else { 0 },
                });
            }

            CfgInstData::Param { index } => {
                let vreg = self.mir.alloc_vreg();
                self.value_map.insert(value, vreg);

                if (*index as usize) < ARG_REGS.len() {
                    let slot = self.num_locals + *index;
                    let offset = self.local_offset(slot);
                    self.mir.push(Aarch64Inst::Ldr {
                        dst: Operand::Virtual(vreg),
                        base: Reg::Fp,
                        offset,
                    });
                } else {
                    // Stack arguments are above the frame pointer
                    let stack_offset = 16 + ((*index as i32) - 8) * 8;
                    self.mir.push(Aarch64Inst::Ldr {
                        dst: Operand::Virtual(vreg),
                        base: Reg::Fp,
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

                // Use ADDS to set overflow and carry flags
                // Use 64-bit version for 64-bit types to get correct overflow detection
                if matches!(ty, Type::I64 | Type::U64) {
                    self.mir.push(Aarch64Inst::AddsRR64 {
                        dst: Operand::Virtual(vreg),
                        src1: Operand::Virtual(lhs_vreg),
                        src2: Operand::Virtual(rhs_vreg),
                    });
                } else {
                    self.mir.push(Aarch64Inst::AddsRR {
                        dst: Operand::Virtual(vreg),
                        src1: Operand::Virtual(lhs_vreg),
                        src2: Operand::Virtual(rhs_vreg),
                    });
                }

                // Overflow check - use appropriate flag based on signedness
                self.emit_overflow_check_add(ty, vreg);
            }

            CfgInstData::Sub(lhs, rhs) => {
                let vreg = self.mir.alloc_vreg();
                self.value_map.insert(value, vreg);

                let lhs_vreg = self.get_vreg(*lhs);
                let rhs_vreg = self.get_vreg(*rhs);

                // Use 64-bit version for 64-bit types to get correct overflow detection
                if matches!(ty, Type::I64 | Type::U64) {
                    self.mir.push(Aarch64Inst::SubsRR64 {
                        dst: Operand::Virtual(vreg),
                        src1: Operand::Virtual(lhs_vreg),
                        src2: Operand::Virtual(rhs_vreg),
                    });
                } else {
                    self.mir.push(Aarch64Inst::SubsRR {
                        dst: Operand::Virtual(vreg),
                        src1: Operand::Virtual(lhs_vreg),
                        src2: Operand::Virtual(rhs_vreg),
                    });
                }

                // Overflow check - use appropriate flag based on signedness
                self.emit_overflow_check_sub(ty, vreg);
            }

            CfgInstData::Mul(lhs, rhs) => {
                let vreg = self.mir.alloc_vreg();
                self.value_map.insert(value, vreg);

                let lhs_vreg = self.get_vreg(*lhs);
                let rhs_vreg = self.get_vreg(*rhs);

                // Overflow check for multiplication
                self.emit_overflow_check_mul(ty, vreg, lhs_vreg, rhs_vreg);
            }

            CfgInstData::Div(lhs, rhs) => {
                let vreg = self.mir.alloc_vreg();
                self.value_map.insert(value, vreg);

                let lhs_vreg = self.get_vreg(*lhs);
                let rhs_vreg = self.get_vreg(*rhs);

                // Division by zero check
                let ok_label = self.mir.alloc_label();
                self.mir.push(Aarch64Inst::Cbnz {
                    src: Operand::Virtual(rhs_vreg),
                    label: ok_label,
                });
                self.mir.push(Aarch64Inst::Bl {
                    symbol: "__rue_div_by_zero".to_string(),
                });
                self.mir.push(Aarch64Inst::Label { id: ok_label });

                self.mir.push(Aarch64Inst::SdivRR {
                    dst: Operand::Virtual(vreg),
                    src1: Operand::Virtual(lhs_vreg),
                    src2: Operand::Virtual(rhs_vreg),
                });
            }

            CfgInstData::Mod(lhs, rhs) => {
                let vreg = self.mir.alloc_vreg();
                self.value_map.insert(value, vreg);

                let lhs_vreg = self.get_vreg(*lhs);
                let rhs_vreg = self.get_vreg(*rhs);

                // Division by zero check
                let ok_label = self.mir.alloc_label();
                self.mir.push(Aarch64Inst::Cbnz {
                    src: Operand::Virtual(rhs_vreg),
                    label: ok_label,
                });
                self.mir.push(Aarch64Inst::Bl {
                    symbol: "__rue_div_by_zero".to_string(),
                });
                self.mir.push(Aarch64Inst::Label { id: ok_label });

                // Compute quotient first
                let quot_vreg = self.mir.alloc_vreg();
                self.mir.push(Aarch64Inst::SdivRR {
                    dst: Operand::Virtual(quot_vreg),
                    src1: Operand::Virtual(lhs_vreg),
                    src2: Operand::Virtual(rhs_vreg),
                });

                // rem = dividend - (quotient * divisor)
                self.mir.push(Aarch64Inst::Msub {
                    dst: Operand::Virtual(vreg),
                    src1: Operand::Virtual(quot_vreg),
                    src2: Operand::Virtual(rhs_vreg),
                    src3: Operand::Virtual(lhs_vreg),
                });
            }

            CfgInstData::Neg(operand) => {
                let vreg = self.mir.alloc_vreg();
                self.value_map.insert(value, vreg);

                let operand_vreg = self.get_vreg(*operand);

                // Use NEGS to set overflow and carry flags
                // Use 32-bit variant for 32-bit and sub-word types, 64-bit for I64/U64
                let dst = Operand::Virtual(vreg);
                let src = Operand::Virtual(operand_vreg);
                if matches!(ty, Type::I64 | Type::U64) {
                    self.mir.push(Aarch64Inst::Negs { dst, src });
                } else {
                    self.mir.push(Aarch64Inst::Negs32 { dst, src });
                }

                // Overflow check - use appropriate flag based on signedness
                // For signed: V flag indicates overflow (when negating MIN_VALUE)
                // For unsigned: C flag indicates non-zero operand (0 - x wraps for x != 0)
                self.emit_overflow_check_neg(ty, vreg);
            }

            CfgInstData::Not(operand) => {
                let vreg = self.mir.alloc_vreg();
                self.value_map.insert(value, vreg);

                let operand_vreg = self.get_vreg(*operand);

                // XOR with 1 to flip the boolean
                self.mir.push(Aarch64Inst::EorImm {
                    dst: Operand::Virtual(vreg),
                    src: Operand::Virtual(operand_vreg),
                    imm: 1,
                });
            }

            CfgInstData::Eq(lhs, rhs) => {
                self.emit_comparison(value, *lhs, *rhs, Cond::Eq);
            }

            CfgInstData::Ne(lhs, rhs) => {
                self.emit_comparison(value, *lhs, *rhs, Cond::Ne);
            }

            CfgInstData::Lt(lhs, rhs) => {
                let cond = if self.is_unsigned_comparison(*lhs) {
                    Cond::Lo // unsigned lower
                } else {
                    Cond::Lt // signed less than
                };
                self.emit_comparison(value, *lhs, *rhs, cond);
            }

            CfgInstData::Gt(lhs, rhs) => {
                let cond = if self.is_unsigned_comparison(*lhs) {
                    Cond::Hi // unsigned higher
                } else {
                    Cond::Gt // signed greater than
                };
                self.emit_comparison(value, *lhs, *rhs, cond);
            }

            CfgInstData::Le(lhs, rhs) => {
                let cond = if self.is_unsigned_comparison(*lhs) {
                    Cond::Ls // unsigned lower or same
                } else {
                    Cond::Le // signed less than or equal
                };
                self.emit_comparison(value, *lhs, *rhs, cond);
            }

            CfgInstData::Ge(lhs, rhs) => {
                let cond = if self.is_unsigned_comparison(*lhs) {
                    Cond::Hs // unsigned higher or same
                } else {
                    Cond::Ge // signed greater than or equal
                };
                self.emit_comparison(value, *lhs, *rhs, cond);
            }

            CfgInstData::And(lhs, rhs) => {
                let vreg = self.mir.alloc_vreg();
                self.value_map.insert(value, vreg);

                let lhs_vreg = self.get_vreg(*lhs);
                let rhs_vreg = self.get_vreg(*rhs);

                self.mir.push(Aarch64Inst::AndRR {
                    dst: Operand::Virtual(vreg),
                    src1: Operand::Virtual(lhs_vreg),
                    src2: Operand::Virtual(rhs_vreg),
                });
            }

            CfgInstData::Or(lhs, rhs) => {
                let vreg = self.mir.alloc_vreg();
                self.value_map.insert(value, vreg);

                let lhs_vreg = self.get_vreg(*lhs);
                let rhs_vreg = self.get_vreg(*rhs);

                self.mir.push(Aarch64Inst::OrrRR {
                    dst: Operand::Virtual(vreg),
                    src1: Operand::Virtual(lhs_vreg),
                    src2: Operand::Virtual(rhs_vreg),
                });
            }

            CfgInstData::Alloc { slot, init } => {
                let init_type = self.cfg.get_inst(*init).ty;
                if matches!(init_type, Type::Struct(_)) || matches!(init_type, Type::Array(_)) {
                    // Struct/Array: store all fields/elements to consecutive slots
                    if let Some(field_vregs) = self.struct_field_vregs.get(init).cloned() {
                        for (i, field_vreg) in field_vregs.iter().enumerate() {
                            let field_slot = slot + i as u32;
                            let offset = self.local_offset(field_slot);
                            self.mir.push(Aarch64Inst::Str {
                                src: Operand::Virtual(*field_vreg),
                                base: Reg::Fp,
                                offset,
                            });
                        }
                    } else {
                        let init_vreg = self.get_vreg(*init);
                        let offset = self.local_offset(*slot);
                        self.mir.push(Aarch64Inst::Str {
                            src: Operand::Virtual(init_vreg),
                            base: Reg::Fp,
                            offset,
                        });
                    }
                } else {
                    let init_vreg = self.get_vreg(*init);
                    let offset = self.local_offset(*slot);
                    self.mir.push(Aarch64Inst::Str {
                        src: Operand::Virtual(init_vreg),
                        base: Reg::Fp,
                        offset,
                    });
                }
            }

            CfgInstData::Load { slot } => {
                let vreg = self.mir.alloc_vreg();
                self.value_map.insert(value, vreg);

                let offset = self.local_offset(*slot);
                self.mir.push(Aarch64Inst::Ldr {
                    dst: Operand::Virtual(vreg),
                    base: Reg::Fp,
                    offset,
                });
            }

            CfgInstData::Store { slot, value: val } => {
                let val_vreg = self.get_vreg(*val);
                let offset = self.local_offset(*slot);
                self.mir.push(Aarch64Inst::Str {
                    src: Operand::Virtual(val_vreg),
                    base: Reg::Fp,
                    offset,
                });
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
                                        self.mir.push(Aarch64Inst::Ldr {
                                            dst: Operand::Virtual(field_vreg),
                                            base: Reg::Fp,
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
                                        self.mir.push(Aarch64Inst::Ldr {
                                            dst: Operand::Virtual(field_vreg),
                                            base: Reg::Fp,
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
                        _ => {
                            flattened_vregs.push(self.get_vreg(*arg));
                        }
                    }
                }

                // Move arguments to registers (AAPCS64 uses X0-X7)
                let num_reg_args = flattened_vregs.len().min(ARG_REGS.len());
                let num_stack_args = flattened_vregs.len().saturating_sub(ARG_REGS.len());

                // Allocate stack space for stack arguments (must be 16-byte aligned)
                let stack_space = if num_stack_args > 0 {
                    ((num_stack_args * 8 + 15) / 16) * 16
                } else {
                    0
                };

                if stack_space > 0 {
                    self.mir.push(Aarch64Inst::SubImm {
                        dst: Operand::Physical(Reg::Sp),
                        src: Operand::Physical(Reg::Sp),
                        imm: stack_space as i32,
                    });
                }

                // Store stack arguments to allocated space
                for (i, arg_vreg) in flattened_vregs.iter().skip(ARG_REGS.len()).enumerate() {
                    let offset = (i * 8) as i32;
                    self.mir.push(Aarch64Inst::Str {
                        src: Operand::Virtual(*arg_vreg),
                        base: Reg::Sp,
                        offset,
                    });
                }

                // Move register arguments
                for (i, arg_vreg) in flattened_vregs.iter().take(num_reg_args).enumerate() {
                    self.mir.push(Aarch64Inst::MovRR {
                        dst: Operand::Physical(ARG_REGS[i]),
                        src: Operand::Virtual(*arg_vreg),
                    });
                }

                // Call the function - the linker will add the underscore prefix for macOS
                self.mir.push(Aarch64Inst::Bl {
                    symbol: name.clone(),
                });

                // Clean up stack space after call
                if stack_space > 0 {
                    self.mir.push(Aarch64Inst::AddImm {
                        dst: Operand::Physical(Reg::Sp),
                        src: Operand::Physical(Reg::Sp),
                        imm: stack_space as i32,
                    });
                }

                // Handle struct return
                if let Type::Struct(struct_id) = ty {
                    let field_count = self.struct_field_count(struct_id);
                    let mut field_vregs = Vec::new();
                    for field_idx in 0..field_count {
                        let field_vreg = self.mir.alloc_vreg();
                        if (field_idx as usize) < RET_REGS.len() {
                            self.mir.push(Aarch64Inst::MovRR {
                                dst: Operand::Virtual(field_vreg),
                                src: Operand::Physical(RET_REGS[field_idx as usize]),
                            });
                        }
                        field_vregs.push(field_vreg);
                    }
                    self.struct_field_vregs.insert(value, field_vregs.clone());
                    if let Some(&first_vreg) = field_vregs.first() {
                        self.mir.push(Aarch64Inst::MovRR {
                            dst: Operand::Virtual(result_vreg),
                            src: Operand::Virtual(first_vreg),
                        });
                    }
                } else {
                    // Move result from X0
                    self.mir.push(Aarch64Inst::MovRR {
                        dst: Operand::Virtual(result_vreg),
                        src: Operand::Physical(Reg::X0),
                    });
                }
            }

            CfgInstData::Intrinsic { name, args } => {
                if name == "dbg" {
                    let arg_val = args[0];
                    let arg_vreg = self.get_vreg(arg_val);
                    let arg_type = self.cfg.get_inst(arg_val).ty;

                    let runtime_fn = match arg_type {
                        Type::Bool => "__rue_dbg_bool",
                        Type::I8 | Type::I16 | Type::I32 | Type::I64 => "__rue_dbg_i64",
                        Type::U8 | Type::U16 | Type::U32 | Type::U64 => "__rue_dbg_u64",
                        _ => unreachable!("@dbg only supports scalars"),
                    };

                    // Handle type extensions
                    match arg_type {
                        Type::I8 => {
                            self.mir.push(Aarch64Inst::MovRR {
                                dst: Operand::Physical(Reg::X0),
                                src: Operand::Virtual(arg_vreg),
                            });
                            self.mir.push(Aarch64Inst::Sxtb {
                                dst: Operand::Physical(Reg::X0),
                                src: Operand::Physical(Reg::X0),
                            });
                        }
                        Type::I16 => {
                            self.mir.push(Aarch64Inst::MovRR {
                                dst: Operand::Physical(Reg::X0),
                                src: Operand::Virtual(arg_vreg),
                            });
                            self.mir.push(Aarch64Inst::Sxth {
                                dst: Operand::Physical(Reg::X0),
                                src: Operand::Physical(Reg::X0),
                            });
                        }
                        Type::I32 => {
                            self.mir.push(Aarch64Inst::MovRR {
                                dst: Operand::Physical(Reg::X0),
                                src: Operand::Virtual(arg_vreg),
                            });
                            self.mir.push(Aarch64Inst::Sxtw {
                                dst: Operand::Physical(Reg::X0),
                                src: Operand::Physical(Reg::X0),
                            });
                        }
                        Type::U8 => {
                            self.mir.push(Aarch64Inst::MovRR {
                                dst: Operand::Physical(Reg::X0),
                                src: Operand::Virtual(arg_vreg),
                            });
                            self.mir.push(Aarch64Inst::Uxtb {
                                dst: Operand::Physical(Reg::X0),
                                src: Operand::Physical(Reg::X0),
                            });
                        }
                        Type::U16 => {
                            self.mir.push(Aarch64Inst::MovRR {
                                dst: Operand::Physical(Reg::X0),
                                src: Operand::Virtual(arg_vreg),
                            });
                            self.mir.push(Aarch64Inst::Uxth {
                                dst: Operand::Physical(Reg::X0),
                                src: Operand::Physical(Reg::X0),
                            });
                        }
                        Type::U32 | Type::I64 | Type::U64 | Type::Bool => {
                            self.mir.push(Aarch64Inst::MovRR {
                                dst: Operand::Physical(Reg::X0),
                                src: Operand::Virtual(arg_vreg),
                            });
                        }
                        _ => unreachable!(),
                    }

                    self.mir.push(Aarch64Inst::Bl {
                        symbol: runtime_fn.to_string(),
                    });

                    let result_vreg = self.mir.alloc_vreg();
                    self.value_map.insert(value, result_vreg);
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
                    self.mir.push(Aarch64Inst::MovRR {
                        dst: Operand::Virtual(vreg),
                        src: Operand::Virtual(first_vreg),
                    });
                } else {
                    self.mir.push(Aarch64Inst::MovImm {
                        dst: Operand::Virtual(vreg),
                        imm: 0,
                    });
                }

                self.struct_field_vregs.insert(value, field_vregs);
            }

            CfgInstData::FieldGet {
                base,
                struct_id: _,
                field_index,
            } => {
                let vreg = self.mir.alloc_vreg();
                self.value_map.insert(value, vreg);

                let base_data = &self.cfg.get_inst(*base).data;
                match base_data {
                    CfgInstData::Load { slot } => {
                        let actual_slot = slot + field_index;
                        let offset = self.local_offset(actual_slot);
                        self.mir.push(Aarch64Inst::Ldr {
                            dst: Operand::Virtual(vreg),
                            base: Reg::Fp,
                            offset,
                        });
                    }
                    CfgInstData::Param { index } => {
                        let param_slot = self.num_locals + index + field_index;
                        let offset = self.local_offset(param_slot);
                        self.mir.push(Aarch64Inst::Ldr {
                            dst: Operand::Virtual(vreg),
                            base: Reg::Fp,
                            offset,
                        });
                    }
                    _ => {
                        // For other sources (BlockParam, StructInit, Call), use field vregs
                        if let Some(field_vregs) = self.struct_field_vregs.get(base).cloned() {
                            if let Some(&field_vreg) = field_vregs.get(*field_index as usize) {
                                self.mir.push(Aarch64Inst::MovRR {
                                    dst: Operand::Virtual(vreg),
                                    src: Operand::Virtual(field_vreg),
                                });
                            } else {
                                // Fallback if field_index out of range
                                let base_vreg = self.get_vreg(*base);
                                self.mir.push(Aarch64Inst::MovRR {
                                    dst: Operand::Virtual(vreg),
                                    src: Operand::Virtual(base_vreg),
                                });
                            }
                        } else {
                            // Fallback for cases without field vregs (e.g., single-field struct)
                            let base_vreg = self.get_vreg(*base);
                            self.mir.push(Aarch64Inst::MovRR {
                                dst: Operand::Virtual(vreg),
                                src: Operand::Virtual(base_vreg),
                            });
                        }
                    }
                }
            }

            CfgInstData::FieldSet {
                slot,
                struct_id: _,
                field_index,
                value: val,
            } => {
                let val_vreg = self.get_vreg(*val);
                let actual_slot = slot + field_index;
                let offset = self.local_offset(actual_slot);
                self.mir.push(Aarch64Inst::Str {
                    src: Operand::Virtual(val_vreg),
                    base: Reg::Fp,
                    offset,
                });
            }

            CfgInstData::ArrayInit {
                array_type_id: _,
                elements,
            } => {
                // Array is stored in local slots; we just create vregs for elements.
                let vreg = self.mir.alloc_vreg();
                self.value_map.insert(value, vreg);

                // Store element vregs for later IndexGet access
                let element_vregs: Vec<VReg> = elements.iter().map(|e| self.get_vreg(*e)).collect();
                self.struct_field_vregs.insert(value, element_vregs);

                // Move 0 into vreg as placeholder
                self.mir.push(Aarch64Inst::MovImm {
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

                let base_data = &self.cfg.get_inst(*base).data;
                match base_data {
                    CfgInstData::Load { slot } => {
                        // Base is a load from a local variable - use dynamic indexing
                        let index_vreg = self.get_vreg(*index);

                        // Emit runtime bounds check
                        let array_length = self.array_length(*array_type_id);
                        self.emit_bounds_check(index_vreg, array_length);

                        let base_offset = self.local_offset(*slot);

                        // Calculate effective address: base_ptr - index * 8
                        // (stack grows down, array laid out sequentially)

                        // Shift left by 3 (multiply by 8)
                        let scaled_index = self.mir.alloc_vreg();
                        self.mir.push(Aarch64Inst::LslImm {
                            dst: Operand::Virtual(scaled_index),
                            src: Operand::Virtual(index_vreg),
                            imm: 3,
                        });

                        // Compute base address (base_offset is negative, e.g., -8)
                        // We need addr = FP + base_offset = FP - abs(base_offset)
                        let addr_vreg = self.mir.alloc_vreg();
                        self.mir.push(Aarch64Inst::SubImm {
                            dst: Operand::Virtual(addr_vreg),
                            src: Operand::Physical(Reg::Fp),
                            imm: -base_offset,
                        });

                        // Subtract scaled index
                        self.mir.push(Aarch64Inst::SubRR {
                            dst: Operand::Virtual(addr_vreg),
                            src1: Operand::Virtual(addr_vreg),
                            src2: Operand::Virtual(scaled_index),
                        });

                        // Load from computed address
                        self.mir.push(Aarch64Inst::LdrIndexed {
                            dst: Operand::Virtual(vreg),
                            base: addr_vreg,
                        });
                    }
                    _ => {
                        // For other sources (ArrayInit), use element vregs if index is constant
                        let index_inst = &self.cfg.get_inst(*index).data;
                        if let CfgInstData::Const(idx) = index_inst {
                            if let Some(element_vregs) = self.struct_field_vregs.get(base).cloned()
                            {
                                if let Some(&elem_vreg) = element_vregs.get(*idx as usize) {
                                    self.mir.push(Aarch64Inst::MovRR {
                                        dst: Operand::Virtual(vreg),
                                        src: Operand::Virtual(elem_vreg),
                                    });
                                    return;
                                }
                            }
                        }
                        // Fallback
                        self.mir.push(Aarch64Inst::MovImm {
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

                // Shift left by 3 (multiply by 8)
                let scaled_index = self.mir.alloc_vreg();
                self.mir.push(Aarch64Inst::LslImm {
                    dst: Operand::Virtual(scaled_index),
                    src: Operand::Virtual(index_vreg),
                    imm: 3,
                });

                // Compute base address (base_offset is negative, e.g., -8)
                // We need addr = FP + base_offset = FP - abs(base_offset)
                let base_offset = self.local_offset(*slot);
                let addr_vreg = self.mir.alloc_vreg();
                self.mir.push(Aarch64Inst::SubImm {
                    dst: Operand::Virtual(addr_vreg),
                    src: Operand::Physical(Reg::Fp),
                    imm: -base_offset,
                });

                // Subtract scaled index
                self.mir.push(Aarch64Inst::SubRR {
                    dst: Operand::Virtual(addr_vreg),
                    src1: Operand::Virtual(addr_vreg),
                    src2: Operand::Virtual(scaled_index),
                });

                // Store to computed address
                self.mir.push(Aarch64Inst::StrIndexed {
                    src: Operand::Virtual(val_vreg),
                    base: addr_vreg,
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

    /// Emit overflow check for ADD based on the type.
    ///
    /// For 32/64-bit types, we use CPU flags directly:
    /// - Signed (i32, i64): V (overflow) flag via BVC
    /// - Unsigned (u32, u64): C (carry) flag - C=1 means overflow, so branch on Lo (C=0)
    ///
    /// For sub-word types, check if result fits in the type's range.
    fn emit_overflow_check_add(&mut self, ty: Type, result_vreg: VReg) {
        let ok_label = self.mir.alloc_label();

        match ty {
            // 32-bit and 64-bit unsigned: C=1 means overflow (carry out)
            // Branch to ok if C=0 (no overflow)
            Type::U32 | Type::U64 => {
                self.mir.push(Aarch64Inst::BCond {
                    cond: Cond::Lo, // Lo = C=0 (no carry)
                    label: ok_label,
                });
            }
            // 32-bit and 64-bit signed: V flag indicates overflow
            Type::I32 | Type::I64 => {
                self.mir.push(Aarch64Inst::Bvc { label: ok_label });
            }
            // Sub-word unsigned types: check if result fits in range [0, max]
            Type::U8 => {
                // Result must be <= 255
                self.mir.push(Aarch64Inst::CmpImm {
                    src: Operand::Virtual(result_vreg),
                    imm: 255,
                });
                // Branch if below or same (unsigned <=)
                self.mir.push(Aarch64Inst::BCond {
                    cond: Cond::Ls,
                    label: ok_label,
                });
            }
            Type::U16 => {
                // Result must be <= 65535
                let max_vreg = self.mir.alloc_vreg();
                self.mir.push(Aarch64Inst::MovImm {
                    dst: Operand::Virtual(max_vreg),
                    imm: 65535,
                });
                self.mir.push(Aarch64Inst::CmpRR {
                    src1: Operand::Virtual(result_vreg),
                    src2: Operand::Virtual(max_vreg),
                });
                self.mir.push(Aarch64Inst::BCond {
                    cond: Cond::Ls,
                    label: ok_label,
                });
            }
            // Sub-word signed types: check if result fits in range [min, max]
            Type::I8 => {
                // Sign-extend to 64-bit and compare with original
                let sext_vreg = self.mir.alloc_vreg();
                self.mir.push(Aarch64Inst::Sxtb {
                    dst: Operand::Virtual(sext_vreg),
                    src: Operand::Virtual(result_vreg),
                });
                self.mir.push(Aarch64Inst::CmpRR {
                    src1: Operand::Virtual(result_vreg),
                    src2: Operand::Virtual(sext_vreg),
                });
                self.mir.push(Aarch64Inst::BCond {
                    cond: Cond::Eq,
                    label: ok_label,
                });
            }
            Type::I16 => {
                // Sign-extend to 64-bit and compare with original
                let sext_vreg = self.mir.alloc_vreg();
                self.mir.push(Aarch64Inst::Sxth {
                    dst: Operand::Virtual(sext_vreg),
                    src: Operand::Virtual(result_vreg),
                });
                self.mir.push(Aarch64Inst::CmpRR {
                    src1: Operand::Virtual(result_vreg),
                    src2: Operand::Virtual(sext_vreg),
                });
                self.mir.push(Aarch64Inst::BCond {
                    cond: Cond::Eq,
                    label: ok_label,
                });
            }
            // Other types don't have arithmetic
            _ => return,
        }

        // Overflow occurred - call panic handler
        self.mir.push(Aarch64Inst::Bl {
            symbol: "__rue_overflow".to_string(),
        });
        self.mir.push(Aarch64Inst::Label { id: ok_label });
    }

    /// Emit overflow check for SUB based on the type.
    ///
    /// For ARM64 SUBS:
    /// - Signed: V flag indicates overflow
    /// - Unsigned: C=0 means borrow (underflow), C=1 means no borrow
    fn emit_overflow_check_sub(&mut self, ty: Type, result_vreg: VReg) {
        let ok_label = self.mir.alloc_label();

        match ty {
            // 32-bit and 64-bit unsigned: C=0 means borrow (underflow)
            // Branch to ok if C=1 (no underflow)
            Type::U32 | Type::U64 => {
                self.mir.push(Aarch64Inst::BCond {
                    cond: Cond::Hs, // Hs = C=1 (no borrow)
                    label: ok_label,
                });
            }
            // 32-bit and 64-bit signed: V flag indicates overflow
            Type::I32 | Type::I64 => {
                self.mir.push(Aarch64Inst::Bvc { label: ok_label });
            }
            // Sub-word types: same as ADD - check range
            Type::U8 | Type::U16 | Type::I8 | Type::I16 => {
                // For subtraction, we need to check both overflow (signed) and underflow
                // Use the same logic as ADD - check if result fits in type's range
                match ty {
                    Type::U8 => {
                        self.mir.push(Aarch64Inst::CmpImm {
                            src: Operand::Virtual(result_vreg),
                            imm: 255,
                        });
                        self.mir.push(Aarch64Inst::BCond {
                            cond: Cond::Ls,
                            label: ok_label,
                        });
                    }
                    Type::U16 => {
                        let max_vreg = self.mir.alloc_vreg();
                        self.mir.push(Aarch64Inst::MovImm {
                            dst: Operand::Virtual(max_vreg),
                            imm: 65535,
                        });
                        self.mir.push(Aarch64Inst::CmpRR {
                            src1: Operand::Virtual(result_vreg),
                            src2: Operand::Virtual(max_vreg),
                        });
                        self.mir.push(Aarch64Inst::BCond {
                            cond: Cond::Ls,
                            label: ok_label,
                        });
                    }
                    Type::I8 => {
                        let sext_vreg = self.mir.alloc_vreg();
                        self.mir.push(Aarch64Inst::Sxtb {
                            dst: Operand::Virtual(sext_vreg),
                            src: Operand::Virtual(result_vreg),
                        });
                        self.mir.push(Aarch64Inst::CmpRR {
                            src1: Operand::Virtual(result_vreg),
                            src2: Operand::Virtual(sext_vreg),
                        });
                        self.mir.push(Aarch64Inst::BCond {
                            cond: Cond::Eq,
                            label: ok_label,
                        });
                    }
                    Type::I16 => {
                        let sext_vreg = self.mir.alloc_vreg();
                        self.mir.push(Aarch64Inst::Sxth {
                            dst: Operand::Virtual(sext_vreg),
                            src: Operand::Virtual(result_vreg),
                        });
                        self.mir.push(Aarch64Inst::CmpRR {
                            src1: Operand::Virtual(result_vreg),
                            src2: Operand::Virtual(sext_vreg),
                        });
                        self.mir.push(Aarch64Inst::BCond {
                            cond: Cond::Eq,
                            label: ok_label,
                        });
                    }
                    _ => unreachable!(),
                }
            }
            // Other types don't have arithmetic
            _ => return,
        }

        self.mir.push(Aarch64Inst::Bl {
            symbol: "__rue_overflow".to_string(),
        });
        self.mir.push(Aarch64Inst::Label { id: ok_label });
    }

    /// Emit overflow check for MUL based on the type.
    ///
    /// For multiplication, we need different approaches for signed vs unsigned:
    /// - Signed: Use SMULL (64-bit result), compare with sign-extended 32-bit
    /// - Unsigned: Use UMULL (64-bit result), check if high bits are non-zero
    fn emit_overflow_check_mul(
        &mut self,
        ty: Type,
        result_vreg: VReg,
        lhs_vreg: VReg,
        rhs_vreg: VReg,
    ) {
        let ok_label = self.mir.alloc_label();

        match ty {
            // 32-bit signed: SMULL gives 64-bit result
            Type::I32 => {
                let smull_vreg = self.mir.alloc_vreg();
                self.mir.push(Aarch64Inst::SmullRR {
                    dst: Operand::Virtual(smull_vreg),
                    src1: Operand::Virtual(lhs_vreg),
                    src2: Operand::Virtual(rhs_vreg),
                });
                // Copy low 32 bits to result
                self.mir.push(Aarch64Inst::MovRR {
                    dst: Operand::Virtual(result_vreg),
                    src: Operand::Virtual(smull_vreg),
                });
                // Sign-extend the 32-bit result
                let sext_vreg = self.mir.alloc_vreg();
                self.mir.push(Aarch64Inst::Sxtw {
                    dst: Operand::Virtual(sext_vreg),
                    src: Operand::Virtual(smull_vreg),
                });
                // Compare 64-bit result with sign-extended 32-bit
                self.mir.push(Aarch64Inst::Cmp64RR {
                    src1: Operand::Virtual(smull_vreg),
                    src2: Operand::Virtual(sext_vreg),
                });
                self.mir.push(Aarch64Inst::BCond {
                    cond: Cond::Eq,
                    label: ok_label,
                });
            }
            // 32-bit unsigned: UMULL gives 64-bit result
            Type::U32 => {
                let umull_vreg = self.mir.alloc_vreg();
                self.mir.push(Aarch64Inst::UmullRR {
                    dst: Operand::Virtual(umull_vreg),
                    src1: Operand::Virtual(lhs_vreg),
                    src2: Operand::Virtual(rhs_vreg),
                });
                // Copy low 32 bits to result
                self.mir.push(Aarch64Inst::MovRR {
                    dst: Operand::Virtual(result_vreg),
                    src: Operand::Virtual(umull_vreg),
                });
                // Check if high 32 bits are zero (shift right by 32, compare with 0)
                let high_vreg = self.mir.alloc_vreg();
                self.mir.push(Aarch64Inst::Lsr64Imm {
                    dst: Operand::Virtual(high_vreg),
                    src: Operand::Virtual(umull_vreg),
                    imm: 32,
                });
                self.mir.push(Aarch64Inst::Cbz {
                    src: Operand::Virtual(high_vreg),
                    label: ok_label,
                });
            }
            // 64-bit signed: Use SMULH for high bits
            Type::I64 => {
                // Do the multiply first
                self.mir.push(Aarch64Inst::MulRR {
                    dst: Operand::Virtual(result_vreg),
                    src1: Operand::Virtual(lhs_vreg),
                    src2: Operand::Virtual(rhs_vreg),
                });
                // Get high bits with SMULH
                let high_vreg = self.mir.alloc_vreg();
                self.mir.push(Aarch64Inst::SmulhRR {
                    dst: Operand::Virtual(high_vreg),
                    src1: Operand::Virtual(lhs_vreg),
                    src2: Operand::Virtual(rhs_vreg),
                });
                // Sign-extend the low result's sign bit to compare
                let sign_vreg = self.mir.alloc_vreg();
                self.mir.push(Aarch64Inst::Asr64Imm {
                    dst: Operand::Virtual(sign_vreg),
                    src: Operand::Virtual(result_vreg),
                    imm: 63,
                });
                // If high bits == sign extension, no overflow
                self.mir.push(Aarch64Inst::Cmp64RR {
                    src1: Operand::Virtual(high_vreg),
                    src2: Operand::Virtual(sign_vreg),
                });
                self.mir.push(Aarch64Inst::BCond {
                    cond: Cond::Eq,
                    label: ok_label,
                });
            }
            // 64-bit unsigned: Use UMULH for high bits
            Type::U64 => {
                // Do the multiply first
                self.mir.push(Aarch64Inst::MulRR {
                    dst: Operand::Virtual(result_vreg),
                    src1: Operand::Virtual(lhs_vreg),
                    src2: Operand::Virtual(rhs_vreg),
                });
                // Get high bits with UMULH
                let high_vreg = self.mir.alloc_vreg();
                self.mir.push(Aarch64Inst::UmulhRR {
                    dst: Operand::Virtual(high_vreg),
                    src1: Operand::Virtual(lhs_vreg),
                    src2: Operand::Virtual(rhs_vreg),
                });
                // If high bits are zero, no overflow
                self.mir.push(Aarch64Inst::Cbz {
                    src: Operand::Virtual(high_vreg),
                    label: ok_label,
                });
            }
            // Sub-word types: do the multiply, then check range
            Type::I8 | Type::I16 | Type::U8 | Type::U16 => {
                // For sub-word, just do the multiply and check range
                self.mir.push(Aarch64Inst::MulRR {
                    dst: Operand::Virtual(result_vreg),
                    src1: Operand::Virtual(lhs_vreg),
                    src2: Operand::Virtual(rhs_vreg),
                });
                // Check range using same logic as ADD
                match ty {
                    Type::U8 => {
                        self.mir.push(Aarch64Inst::CmpImm {
                            src: Operand::Virtual(result_vreg),
                            imm: 255,
                        });
                        self.mir.push(Aarch64Inst::BCond {
                            cond: Cond::Ls,
                            label: ok_label,
                        });
                    }
                    Type::U16 => {
                        let max_vreg = self.mir.alloc_vreg();
                        self.mir.push(Aarch64Inst::MovImm {
                            dst: Operand::Virtual(max_vreg),
                            imm: 65535,
                        });
                        self.mir.push(Aarch64Inst::CmpRR {
                            src1: Operand::Virtual(result_vreg),
                            src2: Operand::Virtual(max_vreg),
                        });
                        self.mir.push(Aarch64Inst::BCond {
                            cond: Cond::Ls,
                            label: ok_label,
                        });
                    }
                    Type::I8 => {
                        let sext_vreg = self.mir.alloc_vreg();
                        self.mir.push(Aarch64Inst::Sxtb {
                            dst: Operand::Virtual(sext_vreg),
                            src: Operand::Virtual(result_vreg),
                        });
                        self.mir.push(Aarch64Inst::CmpRR {
                            src1: Operand::Virtual(result_vreg),
                            src2: Operand::Virtual(sext_vreg),
                        });
                        self.mir.push(Aarch64Inst::BCond {
                            cond: Cond::Eq,
                            label: ok_label,
                        });
                    }
                    Type::I16 => {
                        let sext_vreg = self.mir.alloc_vreg();
                        self.mir.push(Aarch64Inst::Sxth {
                            dst: Operand::Virtual(sext_vreg),
                            src: Operand::Virtual(result_vreg),
                        });
                        self.mir.push(Aarch64Inst::CmpRR {
                            src1: Operand::Virtual(result_vreg),
                            src2: Operand::Virtual(sext_vreg),
                        });
                        self.mir.push(Aarch64Inst::BCond {
                            cond: Cond::Eq,
                            label: ok_label,
                        });
                    }
                    _ => unreachable!(),
                }
            }
            _ => return,
        }

        self.mir.push(Aarch64Inst::Bl {
            symbol: "__rue_overflow".to_string(),
        });
        self.mir.push(Aarch64Inst::Label { id: ok_label });
    }

    /// Emit overflow check for NEG based on the type.
    ///
    /// For NEGS (0 - x):
    /// - Signed: V flag indicates overflow (when negating MIN_VALUE)
    /// - Unsigned: Any non-zero value causes overflow (since 0 - x wraps)
    fn emit_overflow_check_neg(&mut self, ty: Type, result_vreg: VReg) {
        let ok_label = self.mir.alloc_label();

        match ty {
            // Unsigned: NEGS sets C=0 for non-zero operands (which is overflow)
            // Branch to ok if C=1 (meaning operand was 0, no overflow)
            Type::U32 | Type::U64 => {
                self.mir.push(Aarch64Inst::BCond {
                    cond: Cond::Hs, // Hs = C=1
                    label: ok_label,
                });
            }
            // Signed: V flag indicates overflow
            Type::I32 | Type::I64 => {
                self.mir.push(Aarch64Inst::Bvc { label: ok_label });
            }
            // Sub-word types: check range
            Type::U8 | Type::U16 => {
                // For unsigned negation, only 0 is valid (negating to 0)
                // Result must be 0 for no overflow
                self.mir.push(Aarch64Inst::Cbz {
                    src: Operand::Virtual(result_vreg),
                    label: ok_label,
                });
            }
            Type::I8 => {
                let sext_vreg = self.mir.alloc_vreg();
                self.mir.push(Aarch64Inst::Sxtb {
                    dst: Operand::Virtual(sext_vreg),
                    src: Operand::Virtual(result_vreg),
                });
                self.mir.push(Aarch64Inst::CmpRR {
                    src1: Operand::Virtual(result_vreg),
                    src2: Operand::Virtual(sext_vreg),
                });
                self.mir.push(Aarch64Inst::BCond {
                    cond: Cond::Eq,
                    label: ok_label,
                });
            }
            Type::I16 => {
                let sext_vreg = self.mir.alloc_vreg();
                self.mir.push(Aarch64Inst::Sxth {
                    dst: Operand::Virtual(sext_vreg),
                    src: Operand::Virtual(result_vreg),
                });
                self.mir.push(Aarch64Inst::CmpRR {
                    src1: Operand::Virtual(result_vreg),
                    src2: Operand::Virtual(sext_vreg),
                });
                self.mir.push(Aarch64Inst::BCond {
                    cond: Cond::Eq,
                    label: ok_label,
                });
            }
            _ => return,
        }

        self.mir.push(Aarch64Inst::Bl {
            symbol: "__rue_overflow".to_string(),
        });
        self.mir.push(Aarch64Inst::Label { id: ok_label });
    }

    /// Emit a comparison instruction.
    fn emit_comparison(&mut self, value: CfgValue, lhs: CfgValue, rhs: CfgValue, cond: Cond) {
        let vreg = self.mir.alloc_vreg();
        self.value_map.insert(value, vreg);

        let lhs_vreg = self.get_vreg(lhs);
        let rhs_vreg = self.get_vreg(rhs);

        self.mir.push(Aarch64Inst::CmpRR {
            src1: Operand::Virtual(lhs_vreg),
            src2: Operand::Virtual(rhs_vreg),
        });
        self.mir.push(Aarch64Inst::Cset {
            dst: Operand::Virtual(vreg),
            cond,
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
                        self.mir.push(Aarch64Inst::MovRR {
                            dst: Operand::Virtual(param_vreg),
                            src: Operand::Virtual(arg_vreg),
                        });
                    }
                }

                // Jump to target (unless it's the next block)
                let next_block_id = BlockId::from_raw(block.id.as_u32() + 1);
                if *target != next_block_id {
                    self.mir.push(Aarch64Inst::B {
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
                let else_setup_label = self.mir.alloc_label();

                // If zero, jump to else setup (where we copy else_args)
                self.mir.push(Aarch64Inst::Cbz {
                    src: Operand::Virtual(cond_vreg),
                    label: else_setup_label,
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
                        self.mir.push(Aarch64Inst::MovRR {
                            dst: Operand::Virtual(param_vreg),
                            src: Operand::Virtual(arg_vreg),
                        });
                    }
                }

                // Jump to then block
                self.mir.push(Aarch64Inst::B {
                    label: self.block_label(*then_block),
                });

                // Else setup: copy else_args to else_block's params
                self.mir.push(Aarch64Inst::Label {
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
                        self.mir.push(Aarch64Inst::MovRR {
                            dst: Operand::Virtual(param_vreg),
                            src: Operand::Virtual(arg_vreg),
                        });
                    }
                }

                // Jump to else block (or fall through if next)
                let next_block_id = BlockId::from_raw(block.id.as_u32() + 1);
                if *else_block != next_block_id {
                    self.mir.push(Aarch64Inst::B {
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
                    // Compare scrutinee with case value
                    let imm_vreg = self.mir.alloc_vreg();
                    self.mir.push(Aarch64Inst::MovImm {
                        dst: Operand::Virtual(imm_vreg),
                        imm: *value,
                    });
                    self.mir.push(Aarch64Inst::CmpRR {
                        src1: Operand::Virtual(scrutinee_vreg),
                        src2: Operand::Virtual(imm_vreg),
                    });
                    self.mir.push(Aarch64Inst::BCond {
                        cond: Cond::Eq,
                        label: self.block_label(*target),
                    });
                }

                // Fall through to default
                self.mir.push(Aarch64Inst::B {
                    label: self.block_label(*default),
                });
            }

            Terminator::Return { value } => {
                // Handle `return;` without expression (unit-returning functions)
                let Some(value) = value else {
                    self.mir.push(Aarch64Inst::Ret);
                    return;
                };

                let return_type = self.cfg.return_type();

                if self.fn_name == "main" {
                    let val_vreg = self.get_vreg(*value);
                    self.mir.push(Aarch64Inst::MovRR {
                        dst: Operand::Physical(Reg::X0),
                        src: Operand::Virtual(val_vreg),
                    });
                    self.mir.push(Aarch64Inst::Bl {
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
                                for (i, field_vreg) in field_vregs.iter().enumerate() {
                                    if i < RET_REGS.len() {
                                        self.mir.push(Aarch64Inst::MovRR {
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
                                self.mir.push(Aarch64Inst::Ldr {
                                    dst: Operand::Physical(RET_REGS[field_idx as usize]),
                                    base: Reg::Fp,
                                    offset,
                                });
                            }
                        }
                        CfgInstData::Load { slot } => {
                            for field_idx in 0..field_count {
                                let actual_slot = slot + field_idx;
                                let offset = self.local_offset(actual_slot);
                                self.mir.push(Aarch64Inst::Ldr {
                                    dst: Operand::Physical(RET_REGS[field_idx as usize]),
                                    base: Reg::Fp,
                                    offset,
                                });
                            }
                        }
                        _ => {
                            let val_vreg = self.get_vreg(*value);
                            self.mir.push(Aarch64Inst::MovRR {
                                dst: Operand::Physical(Reg::X0),
                                src: Operand::Virtual(val_vreg),
                            });
                        }
                    }

                    self.mir.push(Aarch64Inst::Ret);
                } else {
                    let val_vreg = self.get_vreg(*value);
                    self.mir.push(Aarch64Inst::MovRR {
                        dst: Operand::Physical(Reg::X0),
                        src: Operand::Virtual(val_vreg),
                    });
                    self.mir.push(Aarch64Inst::Ret);
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

    fn lower_to_mir(source: &str) -> Aarch64Mir {
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
        let cfg_output =
            CfgBuilder::build(&func.air, func.num_locals, func.num_param_slots, &func.name);

        CfgLower::new(&cfg_output.cfg, struct_defs, array_types).lower()
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
