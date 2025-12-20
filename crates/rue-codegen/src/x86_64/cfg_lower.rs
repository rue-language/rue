//! CFG to X86Mir lowering.
//!
//! This module converts CFG (explicit control flow graph) to X86Mir
//! (x86-64 instructions with virtual registers).

use std::collections::HashMap;

use rue_cfg::{
    BasicBlock, BlockId, Cfg, CfgInstData, CfgValue, StructDef, StructId, Terminator, Type,
};

use super::mir::{Operand, Reg, VReg, X86Inst, X86Mir};

/// Argument passing registers per System V AMD64 ABI.
const ARG_REGS: [Reg; 6] = [Reg::Rdi, Reg::Rsi, Reg::Rdx, Reg::Rcx, Reg::R8, Reg::R9];

/// Return value registers per System V AMD64 ABI.
const RET_REGS: [Reg; 6] = [Reg::Rax, Reg::Rdx, Reg::Rcx, Reg::R8, Reg::R9, Reg::R10];

/// CFG to X86Mir lowering.
pub struct CfgLower<'a> {
    cfg: &'a Cfg,
    struct_defs: &'a [StructDef],
    mir: X86Mir,
    /// Maps CFG values to vregs
    value_map: HashMap<CfgValue, VReg>,
    /// Maps block parameters to vregs (block_id, param_index) -> vreg
    block_param_vregs: HashMap<(BlockId, u32), VReg>,
    /// Label counter for generating unique labels
    label_counter: u32,
    /// Whether this function has a stack frame
    has_frame: bool,
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
            mir: X86Mir::new(),
            value_map: HashMap::new(),
            block_param_vregs: HashMap::new(),
            label_counter: 0,
            has_frame: num_locals > 0 || num_params > 0,
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
    fn local_offset(&self, slot: u32) -> i32 {
        -((slot as i32 + 1) * 8)
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

    /// Generate a unique label name.
    fn new_label(&mut self, prefix: &str) -> String {
        let label = format!(".L{}_{}_{}", self.fn_name, prefix, self.label_counter);
        self.label_counter += 1;
        label
    }

    /// Get the label for a block.
    fn block_label(&self, block_id: BlockId) -> String {
        format!(".L{}_{}", self.fn_name, block_id.as_u32())
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

                if *v >= i32::MIN as i64 && *v <= i32::MAX as i64 {
                    self.mir.push(X86Inst::MovRI32 {
                        dst: Operand::Virtual(vreg),
                        imm: *v as i32,
                    });
                } else {
                    self.mir.push(X86Inst::MovRI64 {
                        dst: Operand::Virtual(vreg),
                        imm: *v,
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
                self.mir.push(X86Inst::AddRR {
                    dst: Operand::Virtual(vreg),
                    src: Operand::Virtual(rhs_vreg),
                });

                // Overflow check
                let ok_label = self.new_label("add_ok");
                self.mir.push(X86Inst::Jno {
                    label: ok_label.clone(),
                });
                self.mir.push(X86Inst::CallRel {
                    symbol: "__rue_overflow".to_string(),
                });
                self.mir.push(X86Inst::Label { name: ok_label });
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
                self.mir.push(X86Inst::SubRR {
                    dst: Operand::Virtual(vreg),
                    src: Operand::Virtual(rhs_vreg),
                });

                let ok_label = self.new_label("sub_ok");
                self.mir.push(X86Inst::Jno {
                    label: ok_label.clone(),
                });
                self.mir.push(X86Inst::CallRel {
                    symbol: "__rue_overflow".to_string(),
                });
                self.mir.push(X86Inst::Label { name: ok_label });
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
                self.mir.push(X86Inst::ImulRR {
                    dst: Operand::Virtual(vreg),
                    src: Operand::Virtual(rhs_vreg),
                });

                let ok_label = self.new_label("mul_ok");
                self.mir.push(X86Inst::Jno {
                    label: ok_label.clone(),
                });
                self.mir.push(X86Inst::CallRel {
                    symbol: "__rue_overflow".to_string(),
                });
                self.mir.push(X86Inst::Label { name: ok_label });
            }

            CfgInstData::Div(lhs, rhs) => {
                let vreg = self.mir.alloc_vreg();
                self.value_map.insert(value, vreg);

                let lhs_vreg = self.get_vreg(*lhs);
                let rhs_vreg = self.get_vreg(*rhs);

                // Division by zero check
                let ok_label = self.new_label("div_ok");
                self.mir.push(X86Inst::TestRR {
                    src1: Operand::Virtual(rhs_vreg),
                    src2: Operand::Virtual(rhs_vreg),
                });
                self.mir.push(X86Inst::Jnz {
                    label: ok_label.clone(),
                });
                self.mir.push(X86Inst::CallRel {
                    symbol: "__rue_div_by_zero".to_string(),
                });
                self.mir.push(X86Inst::Label { name: ok_label });

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

                let ok_label = self.new_label("mod_ok");
                self.mir.push(X86Inst::TestRR {
                    src1: Operand::Virtual(rhs_vreg),
                    src2: Operand::Virtual(rhs_vreg),
                });
                self.mir.push(X86Inst::Jnz {
                    label: ok_label.clone(),
                });
                self.mir.push(X86Inst::CallRel {
                    symbol: "__rue_div_by_zero".to_string(),
                });
                self.mir.push(X86Inst::Label { name: ok_label });

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
                self.mir.push(X86Inst::Neg {
                    dst: Operand::Virtual(vreg),
                });

                let ok_label = self.new_label("neg_ok");
                self.mir.push(X86Inst::Jno {
                    label: ok_label.clone(),
                });
                self.mir.push(X86Inst::CallRel {
                    symbol: "__rue_overflow".to_string(),
                });
                self.mir.push(X86Inst::Label { name: ok_label });
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

            CfgInstData::Eq(lhs, rhs) => {
                self.emit_comparison(value, *lhs, *rhs, |mir, vreg| {
                    mir.push(X86Inst::Sete {
                        dst: Operand::Virtual(vreg),
                    });
                });
            }

            CfgInstData::Ne(lhs, rhs) => {
                self.emit_comparison(value, *lhs, *rhs, |mir, vreg| {
                    mir.push(X86Inst::Setne {
                        dst: Operand::Virtual(vreg),
                    });
                });
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
                if matches!(init_type, Type::Struct(_)) {
                    // Struct: store all fields to consecutive slots
                    if let Some(field_vregs) = self.struct_field_vregs.get(init).cloned() {
                        for (i, field_vreg) in field_vregs.iter().enumerate() {
                            let field_slot = slot + i as u32;
                            let offset = self.local_offset(field_slot);
                            self.mir.push(X86Inst::MovMR {
                                base: Reg::Rbp,
                                offset,
                                src: Operand::Virtual(*field_vreg),
                            });
                        }
                    } else {
                        // Fallback: just store single value
                        let init_vreg = self.get_vreg(*init);
                        let offset = self.local_offset(*slot);
                        self.mir.push(X86Inst::MovMR {
                            base: Reg::Rbp,
                            offset,
                            src: Operand::Virtual(init_vreg),
                        });
                    }
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
                let vreg = self.mir.alloc_vreg();
                self.value_map.insert(value, vreg);

                let offset = self.local_offset(*slot);
                self.mir.push(X86Inst::MovRM {
                    dst: Operand::Virtual(vreg),
                    base: Reg::Rbp,
                    offset,
                });
            }

            CfgInstData::Store { slot, value: val } => {
                let val_vreg = self.get_vreg(*val);
                let offset = self.local_offset(*slot);
                self.mir.push(X86Inst::MovMR {
                    base: Reg::Rbp,
                    offset,
                    src: Operand::Virtual(val_vreg),
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
                        self.mir.push(X86Inst::MovRM {
                            dst: Operand::Virtual(vreg),
                            base: Reg::Rbp,
                            offset,
                        });
                    }
                    CfgInstData::Param { index } => {
                        let param_slot = self.num_locals + index + field_index;
                        let offset = self.local_offset(param_slot);
                        self.mir.push(X86Inst::MovRM {
                            dst: Operand::Virtual(vreg),
                            base: Reg::Rbp,
                            offset,
                        });
                    }
                    _ => {
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
                self.mir.push(X86Inst::MovMR {
                    base: Reg::Rbp,
                    offset,
                    src: Operand::Virtual(val_vreg),
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

    /// Emit a comparison instruction.
    fn emit_comparison<F>(&mut self, value: CfgValue, lhs: CfgValue, rhs: CfgValue, emit_setcc: F)
    where
        F: FnOnce(&mut X86Mir, VReg),
    {
        let vreg = self.mir.alloc_vreg();
        self.value_map.insert(value, vreg);

        let lhs_vreg = self.get_vreg(lhs);
        let rhs_vreg = self.get_vreg(rhs);

        self.mir.push(X86Inst::CmpRR {
            src1: Operand::Virtual(lhs_vreg),
            src2: Operand::Virtual(rhs_vreg),
        });
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

                // Test condition
                self.mir.push(X86Inst::CmpRI {
                    src: Operand::Virtual(cond_vreg),
                    imm: 0,
                });

                // If false (zero), jump to else block
                self.mir.push(X86Inst::Jz {
                    label: self.block_label(*else_block),
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

                // Jump to then block (or fall through if next)
                let next_block_id = BlockId::from_raw(block.id.as_u32() + 1);
                if *then_block != next_block_id {
                    self.mir.push(X86Inst::Jmp {
                        label: self.block_label(*then_block),
                    });
                }

                // Note: else_args need to be copied before jumping to else_block.
                // This is done via an intermediate else_path label when args are non-empty.
                // For now, this simplified version works for empty args (most common case).
                // TODO: Handle non-empty else_args properly (like aarch64 does).
                let _ = else_args;
            }

            Terminator::Switch {
                scrutinee,
                cases,
                default,
            } => {
                let scrutinee_vreg = self.get_vreg(*scrutinee);

                // Generate comparison and jump for each case
                for (value, target) in cases {
                    // Load case value into a register to handle full i64 range
                    let case_vreg = self.mir.alloc_vreg();
                    self.mir.push(X86Inst::MovRI64 {
                        dst: Operand::Virtual(case_vreg),
                        imm: *value,
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
                    if self.has_frame {
                        self.emit_epilogue();
                    }
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

        let sema = Sema::new(&rir, &interner);
        let output = sema.analyze_all().unwrap();

        let func = &output.functions[0];
        let struct_defs = &output.struct_defs;
        let cfg_output =
            CfgBuilder::build(&func.air, func.num_locals, func.num_param_slots, &func.name);

        CfgLower::new(&cfg_output.cfg, struct_defs).lower()
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
