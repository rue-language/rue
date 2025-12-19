//! CFG to Aarch64Mir lowering.
//!
//! This module converts CFG (explicit control flow graph) to Aarch64Mir
//! (AArch64 instructions with virtual registers).

use std::collections::HashMap;

use rue_cfg::{
    BasicBlock, BlockId, Cfg, CfgInstData, CfgValue, StructDef, StructId, Terminator, Type,
};

use super::mir::{Aarch64Inst, Aarch64Mir, Cond, Operand, Reg, VReg};

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
    mir: Aarch64Mir,
    /// Maps CFG values to vregs
    value_map: HashMap<CfgValue, VReg>,
    /// Maps block parameters to vregs (block_id, param_index) -> vreg
    block_param_vregs: HashMap<(BlockId, u32), VReg>,
    /// Label counter for generating unique labels
    label_counter: u32,
    /// Number of local variable slots
    num_locals: u32,
    /// Number of parameter slots
    num_params: u32,
    /// Function name
    fn_name: String,
    /// Maps StructInit CFG values to their field vregs
    struct_field_vregs: HashMap<CfgValue, Vec<VReg>>,
}

impl<'a> CfgLower<'a> {
    /// Create a new CFG lowering pass.
    pub fn new(cfg: &'a Cfg, struct_defs: &'a [StructDef]) -> Self {
        let num_locals = cfg.num_locals();
        let num_params = cfg.num_params();
        Self {
            cfg,
            struct_defs,
            mir: Aarch64Mir::new(),
            value_map: HashMap::new(),
            block_param_vregs: HashMap::new(),
            label_counter: 0,
            num_locals,
            num_params,
            fn_name: cfg.fn_name().to_string(),
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

    /// Calculate the stack offset for a local variable slot.
    /// On AArch64, we use negative offsets from FP (x29).
    fn local_offset(&self, slot: u32) -> i32 {
        -((slot as i32 + 1) * 8)
    }

    /// Generate a unique label name.
    fn new_label(&mut self, prefix: &str) -> String {
        // macOS requires symbols to start with underscore
        let label = format!("_L{}_{}_{}", self.fn_name, prefix, self.label_counter);
        self.label_counter += 1;
        label
    }

    /// Get the label for a block.
    fn block_label(&self, block_id: BlockId) -> String {
        format!("_L{}_{}", self.fn_name, block_id.as_u32())
    }

    /// Lower CFG to Aarch64Mir.
    pub fn lower(mut self) -> Aarch64Mir {
        // Pre-allocate vregs for block parameters
        for block in self.cfg.blocks() {
            for (param_idx, (param_val, _ty)) in block.params.iter().enumerate() {
                let vreg = self.mir.alloc_vreg();
                self.block_param_vregs
                    .insert((block.id, param_idx as u32), vreg);
                self.value_map.insert(*param_val, vreg);
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
                name: self.block_label(block.id),
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

                // Use ADDS to set overflow flag
                self.mir.push(Aarch64Inst::AddsRR {
                    dst: Operand::Virtual(vreg),
                    src1: Operand::Virtual(lhs_vreg),
                    src2: Operand::Virtual(rhs_vreg),
                });

                // Overflow check - branch on overflow set
                let ok_label = self.new_label("add_ok");
                self.mir.push(Aarch64Inst::Bvc {
                    label: ok_label.clone(),
                });
                self.mir.push(Aarch64Inst::Bl {
                    symbol: "__rue_overflow".to_string(),
                });
                self.mir.push(Aarch64Inst::Label { name: ok_label });
            }

            CfgInstData::Sub(lhs, rhs) => {
                let vreg = self.mir.alloc_vreg();
                self.value_map.insert(value, vreg);

                let lhs_vreg = self.get_vreg(*lhs);
                let rhs_vreg = self.get_vreg(*rhs);

                self.mir.push(Aarch64Inst::SubsRR {
                    dst: Operand::Virtual(vreg),
                    src1: Operand::Virtual(lhs_vreg),
                    src2: Operand::Virtual(rhs_vreg),
                });

                let ok_label = self.new_label("sub_ok");
                self.mir.push(Aarch64Inst::Bvc {
                    label: ok_label.clone(),
                });
                self.mir.push(Aarch64Inst::Bl {
                    symbol: "__rue_overflow".to_string(),
                });
                self.mir.push(Aarch64Inst::Label { name: ok_label });
            }

            CfgInstData::Mul(lhs, rhs) => {
                let vreg = self.mir.alloc_vreg();
                self.value_map.insert(value, vreg);

                let lhs_vreg = self.get_vreg(*lhs);
                let rhs_vreg = self.get_vreg(*rhs);

                // For signed overflow detection on 32-bit multiply:
                // 1. SMULL gives 64-bit result
                // 2. Sign-extend the low 32 bits with SXTW
                // 3. Compare - if they differ, we have overflow
                let smull_vreg = self.mir.alloc_vreg();
                self.mir.push(Aarch64Inst::SmullRR {
                    dst: Operand::Virtual(smull_vreg),
                    src1: Operand::Virtual(lhs_vreg),
                    src2: Operand::Virtual(rhs_vreg),
                });

                // Copy low 32 bits to result (MUL is effectively low 32 bits of SMULL)
                self.mir.push(Aarch64Inst::MovRR {
                    dst: Operand::Virtual(vreg),
                    src: Operand::Virtual(smull_vreg),
                });

                // Sign-extend the 32-bit result to compare with full 64-bit result
                let sext_vreg = self.mir.alloc_vreg();
                self.mir.push(Aarch64Inst::Sxtw {
                    dst: Operand::Virtual(sext_vreg),
                    src: Operand::Virtual(smull_vreg),
                });

                // Compare 64-bit SMULL result with sign-extended 32-bit value
                // If they differ, the result didn't fit in 32 bits
                let ok_label = self.new_label("mul_ok");
                self.mir.push(Aarch64Inst::Cmp64RR {
                    src1: Operand::Virtual(smull_vreg),
                    src2: Operand::Virtual(sext_vreg),
                });
                self.mir.push(Aarch64Inst::BCond {
                    cond: Cond::Eq,
                    label: ok_label.clone(),
                });
                self.mir.push(Aarch64Inst::Bl {
                    symbol: "__rue_overflow".to_string(),
                });
                self.mir.push(Aarch64Inst::Label { name: ok_label });
            }

            CfgInstData::Div(lhs, rhs) => {
                let vreg = self.mir.alloc_vreg();
                self.value_map.insert(value, vreg);

                let lhs_vreg = self.get_vreg(*lhs);
                let rhs_vreg = self.get_vreg(*rhs);

                // Division by zero check
                let ok_label = self.new_label("div_ok");
                self.mir.push(Aarch64Inst::Cbnz {
                    src: Operand::Virtual(rhs_vreg),
                    label: ok_label.clone(),
                });
                self.mir.push(Aarch64Inst::Bl {
                    symbol: "__rue_div_by_zero".to_string(),
                });
                self.mir.push(Aarch64Inst::Label { name: ok_label });

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
                let ok_label = self.new_label("mod_ok");
                self.mir.push(Aarch64Inst::Cbnz {
                    src: Operand::Virtual(rhs_vreg),
                    label: ok_label.clone(),
                });
                self.mir.push(Aarch64Inst::Bl {
                    symbol: "__rue_div_by_zero".to_string(),
                });
                self.mir.push(Aarch64Inst::Label { name: ok_label });

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

                // Use NEGS to set overflow flag (overflow only happens for MIN_VALUE)
                self.mir.push(Aarch64Inst::Negs {
                    dst: Operand::Virtual(vreg),
                    src: Operand::Virtual(operand_vreg),
                });

                // Check overflow flag (V=1 means overflow)
                let ok_label = self.new_label("neg_ok");
                self.mir.push(Aarch64Inst::Bvc {
                    label: ok_label.clone(),
                });
                self.mir.push(Aarch64Inst::Bl {
                    symbol: "__rue_overflow".to_string(),
                });
                self.mir.push(Aarch64Inst::Label { name: ok_label });
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
                self.emit_comparison(value, *lhs, *rhs, Cond::Lt);
            }

            CfgInstData::Gt(lhs, rhs) => {
                self.emit_comparison(value, *lhs, *rhs, Cond::Gt);
            }

            CfgInstData::Le(lhs, rhs) => {
                self.emit_comparison(value, *lhs, *rhs, Cond::Le);
            }

            CfgInstData::Ge(lhs, rhs) => {
                self.emit_comparison(value, *lhs, *rhs, Cond::Ge);
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
                if matches!(init_type, Type::Struct(_)) {
                    // Struct: store all fields to consecutive slots
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
                        let base_vreg = self.get_vreg(*base);
                        self.mir.push(Aarch64Inst::MovRR {
                            dst: Operand::Virtual(vreg),
                            src: Operand::Virtual(base_vreg),
                        });
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
        }
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
                    let arg_vreg = self.get_vreg(arg);
                    let param_vreg = self.block_param_vregs[&(*target, i as u32)];
                    self.mir.push(Aarch64Inst::MovRR {
                        dst: Operand::Virtual(param_vreg),
                        src: Operand::Virtual(arg_vreg),
                    });
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
                let else_setup_label = self.new_label("else_setup");

                // If zero, jump to else setup (where we copy else_args)
                self.mir.push(Aarch64Inst::Cbz {
                    src: Operand::Virtual(cond_vreg),
                    label: else_setup_label.clone(),
                });

                // Copy then_args to then_block's params
                for (i, &arg) in then_args.iter().enumerate() {
                    let arg_vreg = self.get_vreg(arg);
                    let param_vreg = self.block_param_vregs[&(*then_block, i as u32)];
                    self.mir.push(Aarch64Inst::MovRR {
                        dst: Operand::Virtual(param_vreg),
                        src: Operand::Virtual(arg_vreg),
                    });
                }

                // Jump to then block
                self.mir.push(Aarch64Inst::B {
                    label: self.block_label(*then_block),
                });

                // Else setup: copy else_args to else_block's params
                self.mir.push(Aarch64Inst::Label {
                    name: else_setup_label,
                });
                for (i, &arg) in else_args.iter().enumerate() {
                    let arg_vreg = self.get_vreg(arg);
                    let param_vreg = self.block_param_vregs[&(*else_block, i as u32)];
                    self.mir.push(Aarch64Inst::MovRR {
                        dst: Operand::Virtual(param_vreg),
                        src: Operand::Virtual(arg_vreg),
                    });
                }

                // Jump to else block (or fall through if next)
                let next_block_id = BlockId::from_raw(block.id.as_u32() + 1);
                if *else_block != next_block_id {
                    self.mir.push(Aarch64Inst::B {
                        label: self.block_label(*else_block),
                    });
                }
            }

            Terminator::Return { value } => {
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
                        CfgInstData::StructInit { .. } | CfgInstData::Call { .. } => {
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

        let sema = Sema::new(&rir, &interner);
        let output = sema.analyze_all().unwrap();

        let func = &output.functions[0];
        let struct_defs = &output.struct_defs;
        let cfg = CfgBuilder::build(&func.air, func.num_locals, func.num_param_slots, &func.name);

        CfgLower::new(&cfg, struct_defs).lower()
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
