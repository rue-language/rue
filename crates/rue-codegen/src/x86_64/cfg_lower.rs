//! CFG to X86Mir lowering.
//!
//! This module converts CFG (explicit control flow graph) to X86Mir
//! (x86-64 instructions with virtual registers).
//!
//! # Label Namespace Separation
//!
//! During lowering, we need to generate labels for two distinct purposes:
//!
//! 1. **Block labels** - Each CFG basic block gets a label for control flow
//!    (jumps, branches, etc.). These are derived deterministically from block IDs.
//!
//! 2. **Inline labels** - Generated during instruction lowering for things like
//!    overflow checks, bounds checks, division-by-zero checks, and conditional
//!    branches within a single CFG instruction.
//!
//! To prevent collisions, we partition the `u32` label ID space:
//!
//! - **Inline labels**: IDs `0` to `BLOCK_LABEL_BASE - 1` (allocated via [`CfgLower::new_label`])
//! - **Block labels**: IDs `BLOCK_LABEL_BASE` to `u32::MAX` (computed via [`CfgLower::block_label`])
//!
//! See [`crate::vreg::BLOCK_LABEL_BASE`] for the constant definition.
//!
//! This gives each namespace ~2 billion IDs, which is more than sufficient for
//! any realistic function. The separation is handled automatically by the
//! respective methods.

use std::collections::HashMap;

use lasso::ThreadedRodeo;
use rue_air::{ArrayTypeDef, ArrayTypeId};
use rue_cfg::{
    BasicBlock, BlockId, Cfg, CfgInstData, CfgValue, StructDef, StructId, Terminator, Type,
};

use super::mir::{LabelId, Operand, Reg, VReg, X86Inst, X86Mir};
use crate::cfg_lower::{FieldChainBase, IndexChainBase, IndexLevel};
use crate::types;
use crate::vreg::BLOCK_LABEL_BASE;

/// Argument passing registers per System V AMD64 ABI.
const ARG_REGS: [Reg; 6] = [Reg::Rdi, Reg::Rsi, Reg::Rdx, Reg::Rcx, Reg::R8, Reg::R9];

/// Return value registers per System V AMD64 ABI.
const RET_REGS: [Reg; 6] = [Reg::Rax, Reg::Rdx, Reg::Rcx, Reg::R8, Reg::R9, Reg::R10];

/// CFG to X86Mir lowering.
pub struct CfgLower<'a> {
    cfg: &'a Cfg,
    struct_defs: &'a [StructDef],
    /// Array type definitions for bounds checking.
    array_types: &'a [ArrayTypeDef],
    /// String table from semantic analysis (indexed by StringId).
    strings: &'a [String],
    /// Interner for resolving Spur to string
    interner: &'a ThreadedRodeo,
    mir: X86Mir,
    /// Maps CFG values to vregs
    value_map: HashMap<CfgValue, VReg>,
    /// Maps block parameters to vregs (block_id, param_index) -> vreg
    block_param_vregs: HashMap<(BlockId, u32), VReg>,
    /// Next inline label ID for generating unique labels.
    ///
    /// Inline labels (for overflow checks, bounds checks, etc.) use IDs from
    /// the lower half of the `u32` space. See module docs for namespace details.
    next_label: u32,
    /// Number of local variable slots
    num_locals: u32,
    /// Number of parameter slots
    num_params: u32,
    /// Function name (needed to detect main function)
    fn_name: &'a str,
    /// Maps StructInit CFG values to their field vregs
    struct_slot_vregs: HashMap<CfgValue, Vec<VReg>>,
    /// Maps inout parameter indices to their pointer vregs.
    /// For inout params, the slot contains a pointer to the caller's memory.
    /// This map stores the vreg holding that pointer so Store can use it.
    inout_param_ptrs: HashMap<u32, VReg>,
}

impl<'a> CfgLower<'a> {
    /// Create a new CFG lowering pass.
    pub fn new(
        cfg: &'a Cfg,
        struct_defs: &'a [StructDef],
        array_types: &'a [ArrayTypeDef],
        strings: &'a [String],
        interner: &'a ThreadedRodeo,
    ) -> Self {
        let num_locals = cfg.num_locals();
        let num_params = cfg.num_params();
        Self {
            cfg,
            struct_defs,
            array_types,
            strings,
            interner,
            mir: X86Mir::new(),
            value_map: HashMap::new(),
            block_param_vregs: HashMap::new(),
            next_label: 0,
            num_locals,
            num_params,
            fn_name: cfg.fn_name(),
            struct_slot_vregs: HashMap::new(),
            inout_param_ptrs: HashMap::new(),
        }
    }

    /// Intern a symbol name and return its ID.
    ///
    /// Convenience method that delegates to the MIR's symbol table.
    fn intern_symbol(&mut self, symbol: &str) -> u32 {
        self.mir.intern_symbol(symbol)
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
        types::array_type_def(self.array_types, array_type_id)
    }

    /// Calculate the total number of slots needed to store a type.
    fn type_slot_count(&self, ty: Type) -> u32 {
        types::type_slot_count(self.struct_defs, self.array_types, ty)
    }

    /// Calculate the slot count for a single element of an array type.
    fn array_element_slot_count(&self, array_type_id: ArrayTypeId) -> u32 {
        types::array_element_slot_count(self.struct_defs, self.array_types, array_type_id)
    }

    /// Calculate the slot offset for a field within a struct.
    fn struct_field_slot_offset(&self, struct_id: StructId, field_index: u32) -> u32 {
        types::struct_field_slot_offset(self.struct_defs, self.array_types, struct_id, field_index)
    }

    /// Trace back through a chain of FieldGet instructions to find the original
    /// Load or Param source. Returns the base kind and accumulated slot offset.
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

    /// Trace back through a chain of IndexGet instructions to find the original
    /// source (Load, Param, or FieldGet). Returns the base kind and the list of
    /// index levels from outermost to innermost.
    fn trace_index_chain(&self, value: CfgValue) -> Option<(IndexChainBase, Vec<IndexLevel>)> {
        let inst = self.cfg.get_inst(value);
        match &inst.data {
            CfgInstData::Load { slot } => Some((IndexChainBase::Load { slot: *slot }, vec![])),
            CfgInstData::Param { index } => Some((IndexChainBase::Param { index: *index }, vec![])),
            CfgInstData::FieldGet {
                base,
                struct_id,
                field_index,
            } => {
                // Array within a struct - find the struct's Load/Param
                let struct_base_data = &self.cfg.get_inst(*base).data;
                match struct_base_data {
                    CfgInstData::Load { slot } => {
                        let field_slot_offset =
                            self.struct_field_slot_offset(*struct_id, *field_index);
                        Some((
                            IndexChainBase::FieldGet {
                                struct_base_slot: *slot,
                                field_slot_offset,
                            },
                            vec![],
                        ))
                    }
                    _ => None, // Other struct sources not supported
                }
            }
            CfgInstData::IndexGet {
                base,
                array_type_id,
                index,
            } => {
                // Recursively trace the base
                if let Some((base_kind, mut levels)) = self.trace_index_chain(*base) {
                    let elem_slot_count = self.array_element_slot_count(*array_type_id);
                    levels.push(IndexLevel {
                        index: *index,
                        elem_slot_count,
                        array_type_id: *array_type_id,
                    });
                    Some((base_kind, levels))
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    /// Recursively collect all scalar vregs from an array value.
    /// Delegates to the shared implementation in `types`.
    fn collect_array_scalar_vregs(&mut self, value: CfgValue) -> Vec<VReg> {
        // Clone struct_slot_vregs to avoid borrow conflict with get_vreg
        let slot_vregs = self.struct_slot_vregs.clone();
        types::collect_array_scalar_vregs(self.cfg, &slot_vregs, value, &mut |v| self.get_vreg(v))
    }

    /// Recursively collect all scalar vregs from a struct value.
    /// Delegates to the shared implementation in `types`.
    fn collect_struct_scalar_vregs(&mut self, value: CfgValue) -> Vec<VReg> {
        // Clone struct_slot_vregs to avoid borrow conflict with get_vreg
        let slot_vregs = self.struct_slot_vregs.clone();
        types::collect_struct_scalar_vregs(self.cfg, &slot_vregs, value, &mut |v| self.get_vreg(v))
    }

    /// Calculate the stack offset for a local variable slot.
    fn local_offset(&self, slot: u32) -> i32 {
        -((slot as i32 + 1) * 8)
    }

    /// Check if a slot corresponds to an inout parameter.
    /// Returns Some(param_index) if it's an inout param slot, None otherwise.
    fn slot_to_inout_param_index(&self, slot: u32) -> Option<u32> {
        if slot >= self.num_locals && slot < self.num_locals + self.num_params {
            let param_index = slot - self.num_locals;
            if self.cfg.is_param_inout(param_index) {
                return Some(param_index);
            }
        }
        None
    }

    /// Ensure the inout parameter pointer vreg exists for the given param slot.
    /// If the pointer has already been loaded (via a Param instruction), returns the cached vreg.
    /// Otherwise, loads the pointer from the parameter slot and caches it.
    ///
    /// This is needed because ParamIndexSet/ParamStore/etc. may reference an inout param
    /// that was never accessed via a Param instruction (e.g., write-only parameter).
    fn ensure_inout_param_ptr(&mut self, param_slot: u32) -> VReg {
        if let Some(ptr_vreg) = self.inout_param_ptrs.get(&param_slot).copied() {
            return ptr_vreg;
        }

        // Load the pointer from the param slot
        let ptr_vreg = self.mir.alloc_vreg();

        if (param_slot as usize) < ARG_REGS.len() {
            let slot = self.num_locals + param_slot;
            let offset = self.local_offset(slot);
            self.mir.push(X86Inst::MovRM {
                dst: Operand::Virtual(ptr_vreg),
                base: Reg::Rbp,
                offset,
            });
        } else {
            let stack_offset = 16 + ((param_slot as i32) - 6) * 8;
            self.mir.push(X86Inst::MovRM {
                dst: Operand::Virtual(ptr_vreg),
                base: Reg::Rbp,
                offset: stack_offset,
            });
        }

        // Cache it for future use
        self.inout_param_ptrs.insert(param_slot, ptr_vreg);
        ptr_vreg
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
        let symbol_id = self.intern_symbol("__rue_bounds_check");
        self.mir.push(X86Inst::CallRel { symbol_id });

        // Continue with valid access
        self.mir.push(X86Inst::Label { id: ok_label });
    }

    /// Allocate a new inline label ID.
    ///
    /// These labels are used for control flow within instruction lowering
    /// (overflow checks, bounds checks, etc.). IDs are allocated starting
    /// from 0 and incrementing, staying within the lower half of the ID space.
    ///
    /// See the module documentation for details on label namespace separation.
    fn new_label(&mut self) -> LabelId {
        let label = LabelId::new(self.next_label);
        self.next_label += 1;
        label
    }

    /// Get the label for a CFG basic block.
    ///
    /// Block labels use IDs in the upper half of the `u32` space (starting at
    /// [`BLOCK_LABEL_BASE`]) to avoid collisions with inline labels allocated by
    /// [`Self::new_label`]. The mapping is deterministic: `block_id` maps to
    /// `BLOCK_LABEL_BASE + block_id`.
    ///
    /// See the module documentation for details on label namespace separation.
    fn block_label(&self, block_id: BlockId) -> LabelId {
        LabelId::new(BLOCK_LABEL_BASE + block_id.as_u32())
    }

    /// Get or compute field vregs for a struct value.
    ///
    /// This handles different sources of struct values:
    /// - StructInit: use the field values directly
    /// - Load: load field values from stack slots
    /// - Param: use parameter registers/slots
    /// - BlockParam/Call: use cached struct_slot_vregs
    fn get_or_compute_field_vregs(&mut self, value: CfgValue) -> Option<Vec<VReg>> {
        // Check cache first
        if let Some(vregs) = self.struct_slot_vregs.get(&value).cloned() {
            return Some(vregs);
        }

        let inst = self.cfg.get_inst(value);
        let struct_id = match inst.ty {
            Type::Struct(id) => id,
            _ => return None,
        };

        match &inst.data.clone() {
            CfgInstData::StructInit {
                fields_start,
                fields_len,
                ..
            } => {
                let fields = self.cfg.get_extra(*fields_start, *fields_len);
                Some(fields.iter().map(|f| self.get_vreg(*f)).collect())
            }
            CfgInstData::Load { slot } => {
                // Load slot values from consecutive stack slots
                let slot_count = self.type_slot_count(Type::Struct(struct_id));
                let mut vregs = Vec::with_capacity(slot_count as usize);
                for i in 0..slot_count {
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
                // Get slot values from parameter area
                let slot_count = self.type_slot_count(Type::Struct(struct_id));
                let mut vregs = Vec::with_capacity(slot_count as usize);
                for i in 0..slot_count {
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
        let dst_fields = self.struct_slot_vregs.get(&target_param).cloned();

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

                // For struct types, also allocate vregs for each slot
                if let Type::Struct(struct_id) = ty {
                    let slot_count = self.type_slot_count(Type::Struct(*struct_id));
                    let mut slot_vregs = vec![vreg]; // First slot uses main vreg
                    for _ in 1..slot_count {
                        slot_vregs.push(self.mir.alloc_vreg());
                    }
                    self.struct_slot_vregs.insert(*param_val, slot_vregs);
                }
            }
        }

        // Lower each block
        for block in self.cfg.blocks() {
            self.lower_block(block);
        }

        self.mir
    }

    /// Lower CFG to X86Mir with debug information about instruction selection.
    ///
    /// This is like `lower()` but also captures detailed information about
    /// how each CFG instruction maps to MIR instructions.
    pub fn lower_with_debug(mut self) -> (X86Mir, crate::LoweringDebugInfo) {
        use crate::cfg_lower::{format_cfg_inst_data, format_terminator};
        use crate::{
            BlockLoweringInfo, LoweringDebugInfo, LoweringDecision, TerminatorLoweringDecision,
        };

        let mut debug_info = LoweringDebugInfo {
            fn_name: self.fn_name.to_string(),
            target_arch: "x86_64".to_string(),
            blocks: Vec::new(),
        };

        // Pre-allocate vregs for block parameters (same as lower())
        for block in self.cfg.blocks() {
            for (param_idx, (param_val, ty)) in block.params.iter().enumerate() {
                let vreg = self.mir.alloc_vreg();
                self.block_param_vregs
                    .insert((block.id, param_idx as u32), vreg);
                self.value_map.insert(*param_val, vreg);

                if let Type::Struct(struct_id) = ty {
                    let slot_count = self.type_slot_count(Type::Struct(*struct_id));
                    let mut slot_vregs = vec![vreg];
                    for _ in 1..slot_count {
                        slot_vregs.push(self.mir.alloc_vreg());
                    }
                    self.struct_slot_vregs.insert(*param_val, slot_vregs);
                }
            }
        }

        // Lower each block with debug tracking
        for block in self.cfg.blocks() {
            let mut block_info = BlockLoweringInfo {
                block_id: block.id,
                instructions: Vec::new(),
                terminator: None,
            };

            // Emit block label (except for entry block)
            if block.id != self.cfg.entry {
                self.mir.push(X86Inst::Label {
                    id: self.block_label(block.id),
                });
            }

            // Lower each instruction with tracking
            for &value in &block.insts {
                // Skip if already lowered
                if self.value_map.contains_key(&value) {
                    continue;
                }

                let inst = self.cfg.get_inst(value);
                let inst_before = self.mir.inst_count();

                // Lower the instruction
                self.lower_value(value);

                let inst_after = self.mir.inst_count();

                // Capture the generated instructions
                let mir_insts: Vec<String> = self.mir.instructions()[inst_before..inst_after]
                    .iter()
                    .map(|i| format!("{}", i))
                    .collect();

                // Generate rationale for interesting cases
                let rationale = self.get_lowering_rationale(&inst.data, inst.ty);

                if !mir_insts.is_empty() {
                    block_info.instructions.push(LoweringDecision {
                        cfg_value: value,
                        cfg_inst_desc: format_cfg_inst_data(&inst.data),
                        cfg_type: inst.ty.name().to_string(),
                        mir_insts,
                        rationale,
                    });
                }
            }

            // Lower terminator with tracking
            let term_before = self.mir.inst_count();
            self.lower_terminator(block);
            let term_after = self.mir.inst_count();

            let term_mir_insts: Vec<String> = self.mir.instructions()[term_before..term_after]
                .iter()
                .map(|i| format!("{}", i))
                .collect();

            let term_rationale = self.get_terminator_rationale(&block.terminator);

            block_info.terminator = Some(TerminatorLoweringDecision {
                terminator_desc: format_terminator(&block.terminator),
                mir_insts: term_mir_insts,
                rationale: term_rationale,
            });

            debug_info.blocks.push(block_info);
        }

        (self.mir, debug_info)
    }

    /// Generate rationale for instruction lowering decisions.
    fn get_lowering_rationale(&self, data: &CfgInstData, ty: Type) -> Option<String> {
        match data {
            CfgInstData::Add(_, _) | CfgInstData::Sub(_, _) | CfgInstData::Mul(_, _) => {
                if matches!(ty, Type::I64 | Type::U64) {
                    Some("64-bit operation with 64-bit overflow check".to_string())
                } else if matches!(
                    ty,
                    Type::I8 | Type::I16 | Type::I32 | Type::U8 | Type::U16 | Type::U32
                ) {
                    Some("32-bit operation with overflow check".to_string())
                } else {
                    None
                }
            }
            CfgInstData::Div(_, _) | CfgInstData::Mod(_, _) => {
                if matches!(ty, Type::I8 | Type::I16 | Type::I32 | Type::I64) {
                    Some("Signed division: uses CDQ/IDIV sequence".to_string())
                } else {
                    Some("Unsigned division: uses XOR/DIV sequence".to_string())
                }
            }
            CfgInstData::Shr(_, _) => {
                if matches!(ty, Type::I8 | Type::I16 | Type::I32 | Type::I64) {
                    Some("Signed shift right (SAR) preserves sign bit".to_string())
                } else {
                    Some("Unsigned shift right (SHR) zero-extends".to_string())
                }
            }
            CfgInstData::Call {
                args_start,
                args_len,
                ..
            } => {
                let args = self.cfg.get_call_args(*args_start, *args_len);
                let inout_count = args.iter().filter(|a| a.is_inout()).count();
                let borrow_count = args.iter().filter(|a| a.is_borrow()).count();
                if inout_count > 0 || borrow_count > 0 {
                    Some(format!(
                        "SysV ABI with {} inout, {} borrow params (passed as pointers)",
                        inout_count, borrow_count
                    ))
                } else if args.len() > 6 {
                    Some("SysV ABI with stack-passed arguments".to_string())
                } else {
                    None
                }
            }
            CfgInstData::Param { index } => {
                if self.cfg.is_param_inout(*index) {
                    Some("Inout param: load pointer then dereference".to_string())
                } else if (*index as usize) < ARG_REGS.len() {
                    Some(format!(
                        "From register {} (SysV ABI)",
                        ARG_REGS[*index as usize]
                    ))
                } else {
                    Some("From stack (SysV ABI, args > 6)".to_string())
                }
            }
            CfgInstData::Const(v) => {
                if *v <= u32::MAX as u64 {
                    Some("32-bit immediate (zero-extends to 64-bit)".to_string())
                } else {
                    Some("64-bit immediate required".to_string())
                }
            }
            CfgInstData::IndexGet { .. } | CfgInstData::IndexSet { .. } => {
                Some("Includes bounds check".to_string())
            }
            _ => None,
        }
    }

    /// Generate rationale for terminator lowering decisions.
    fn get_terminator_rationale(&self, terminator: &Terminator) -> Option<String> {
        match terminator {
            Terminator::Branch { .. } => Some("Compare with zero, conditional jump".to_string()),
            Terminator::Return { value } => {
                if self.fn_name == "main" {
                    Some("Main function: return value becomes exit code".to_string())
                } else if value.is_some() {
                    Some("Return value in RAX (SysV ABI)".to_string())
                } else {
                    None
                }
            }
            Terminator::Switch { cases, .. } => {
                Some(format!("Linear scan through {} cases", cases.len()))
            }
            _ => None,
        }
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
                let cap_vreg = self.mir.alloc_vreg();

                self.mir.push(X86Inst::StringConstPtr {
                    dst: Operand::Virtual(ptr_vreg),
                    string_id: *string_id,
                });

                self.mir.push(X86Inst::StringConstLen {
                    dst: Operand::Virtual(len_vreg),
                    string_id: *string_id,
                });

                self.mir.push(X86Inst::StringConstCap {
                    dst: Operand::Virtual(cap_vreg),
                    string_id: *string_id,
                });

                // Store all three in struct_slot_vregs for String (ptr, len, cap)
                self.struct_slot_vregs
                    .insert(value, vec![ptr_vreg, len_vreg, cap_vreg]);
                self.value_map.insert(value, ptr_vreg);
            }

            CfgInstData::Param { index } => {
                // Check if this is an inout parameter
                let is_inout = self.cfg.is_param_inout(*index);

                if is_inout {
                    // For inout params, the slot contains a POINTER to the caller's memory.
                    // Load the pointer, then dereference to get the value.
                    let ptr_vreg = self.mir.alloc_vreg();
                    let val_vreg = self.mir.alloc_vreg();

                    // Load the pointer from the param slot
                    if (*index as usize) < ARG_REGS.len() {
                        let slot = self.num_locals + *index;
                        let offset = self.local_offset(slot);
                        self.mir.push(X86Inst::MovRM {
                            dst: Operand::Virtual(ptr_vreg),
                            base: Reg::Rbp,
                            offset,
                        });
                    } else {
                        let stack_offset = 16 + ((*index as i32) - 6) * 8;
                        self.mir.push(X86Inst::MovRM {
                            dst: Operand::Virtual(ptr_vreg),
                            base: Reg::Rbp,
                            offset: stack_offset,
                        });
                    }

                    // Store the pointer vreg for later use by Store
                    self.inout_param_ptrs.insert(*index, ptr_vreg);

                    // Dereference the pointer to get the actual value
                    self.mir.push(X86Inst::MovRMIndexed {
                        dst: Operand::Virtual(val_vreg),
                        base: ptr_vreg,
                        offset: 0,
                    });

                    self.value_map.insert(value, val_vreg);
                } else {
                    // Normal parameter: load the value directly from the slot
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
                let symbol_id = self.intern_symbol("__rue_div_by_zero");
                self.mir.push(X86Inst::CallRel { symbol_id });
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
                let symbol_id = self.intern_symbol("__rue_div_by_zero");
                self.mir.push(X86Inst::CallRel { symbol_id });
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

                // Move LHS to result
                self.mir.push(X86Inst::MovRR {
                    dst: Operand::Virtual(vreg),
                    src: Operand::Virtual(lhs_vreg),
                });

                // Check if shift amount is a constant - use immediate form if so
                // Mask shift amount to match x86-64 hardware semantics:
                // 63 (0x3F) for 64-bit shifts, 31 (0x1F) for 32-bit shifts
                let rhs_inst = &self.cfg.get_inst(*rhs).data;
                if let CfgInstData::Const(shift_amount) = rhs_inst {
                    let mask = if ty.is_64_bit() { 0x3F } else { 0x1F };
                    let imm = (*shift_amount & mask) as u8;
                    // Use 64-bit shift for i64/u64, 32-bit shift for smaller types
                    if ty.is_64_bit() {
                        self.mir.push(X86Inst::ShlRI {
                            dst: Operand::Virtual(vreg),
                            imm,
                        });
                    } else {
                        self.mir.push(X86Inst::Shl32RI {
                            dst: Operand::Virtual(vreg),
                            imm,
                        });
                    }
                } else {
                    // Variable shift amount - use CL register
                    let rhs_vreg = self.get_vreg(*rhs);

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
            }

            CfgInstData::Shr(lhs, rhs) => {
                let vreg = self.mir.alloc_vreg();
                self.value_map.insert(value, vreg);

                let lhs_vreg = self.get_vreg(*lhs);

                // Move LHS to result
                self.mir.push(X86Inst::MovRR {
                    dst: Operand::Virtual(vreg),
                    src: Operand::Virtual(lhs_vreg),
                });

                // Check if shift amount is a constant - use immediate form if so.
                // Mask shift amount to match x86-64 hardware semantics:
                // 63 (0x3F) for 64-bit shifts, 31 (0x1F) for 32-bit shifts
                let rhs_inst = &self.cfg.get_inst(*rhs).data;
                if let CfgInstData::Const(shift_amount) = rhs_inst {
                    let mask = if ty.is_64_bit() { 0x3F } else { 0x1F };
                    let imm = (*shift_amount & mask) as u8;
                    // Use arithmetic shift (SAR) for signed types, logical shift (SHR) for unsigned
                    // Use 64-bit shift for i64/u64, 32-bit shift for smaller types
                    if ty.is_64_bit() && ty.is_signed() {
                        self.mir.push(X86Inst::SarRI {
                            dst: Operand::Virtual(vreg),
                            imm,
                        });
                    } else if ty.is_64_bit() {
                        self.mir.push(X86Inst::ShrRI {
                            dst: Operand::Virtual(vreg),
                            imm,
                        });
                    } else if ty.is_signed() {
                        self.mir.push(X86Inst::Sar32RI {
                            dst: Operand::Virtual(vreg),
                            imm,
                        });
                    } else {
                        self.mir.push(X86Inst::Shr32RI {
                            dst: Operand::Virtual(vreg),
                            imm,
                        });
                    }
                } else {
                    // Variable shift amount - use CL register
                    let rhs_vreg = self.get_vreg(*rhs);

                    // Move shift amount to RCX (CL is the low byte)
                    self.mir.push(X86Inst::MovRR {
                        dst: Operand::Physical(Reg::Rcx),
                        src: Operand::Virtual(rhs_vreg),
                    });

                    // Use arithmetic shift (SAR) for signed types, logical shift (SHR) for unsigned
                    // Use 64-bit shift for i64/u64, 32-bit shift for smaller types
                    if ty.is_64_bit() && ty.is_signed() {
                        self.mir.push(X86Inst::SarRCl {
                            dst: Operand::Virtual(vreg),
                        });
                    } else if ty.is_64_bit() {
                        self.mir.push(X86Inst::ShrRCl {
                            dst: Operand::Virtual(vreg),
                        });
                    } else if ty.is_signed() {
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
                    let vreg = self.emit_string_eq_call(*lhs, *rhs);
                    self.value_map.insert(value, vreg);
                } else if lhs_ty == Type::Unit {
                    // Unit equality: () == () is always true
                    let vreg = self.mir.alloc_vreg();
                    self.value_map.insert(value, vreg);
                    self.mir.push(X86Inst::MovRI32 {
                        dst: Operand::Virtual(vreg),
                        imm: 1,
                    });
                } else if let Type::Struct(struct_id) = lhs_ty {
                    // Struct equality: compare all fields
                    self.emit_struct_equality(value, *lhs, *rhs, struct_id, false);
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
                    let vreg = self.emit_string_eq_call(*lhs, *rhs);
                    self.value_map.insert(value, vreg);
                    // Invert result: 0 -> 1, 1 -> 0
                    self.mir.push(X86Inst::XorRI {
                        dst: Operand::Virtual(vreg),
                        imm: 1,
                    });
                } else if lhs_ty == Type::Unit {
                    // Unit inequality: () != () is always false
                    let vreg = self.mir.alloc_vreg();
                    self.value_map.insert(value, vreg);
                    self.mir.push(X86Inst::MovRI32 {
                        dst: Operand::Virtual(vreg),
                        imm: 0,
                    });
                } else if let Type::Struct(struct_id) = lhs_ty {
                    // Struct inequality: compare all fields, invert result
                    self.emit_struct_equality(value, *lhs, *rhs, struct_id, true);
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
                    // String: store ptr, len, and cap to consecutive slots
                    let field_vregs = self
                        .struct_slot_vregs
                        .get(init)
                        .cloned()
                        .expect("string should have fat pointer fields in Alloc");
                    debug_assert_eq!(
                        field_vregs.len(),
                        3,
                        "string should have 3 fields (ptr, len, cap)"
                    );

                    let ptr_vreg = field_vregs[0];
                    let len_vreg = field_vregs[1];
                    let cap_vreg = field_vregs[2];

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

                    // Store cap to slot + 2
                    let cap_offset = self.local_offset(slot + 2);
                    self.mir.push(X86Inst::MovMR {
                        base: Reg::Rbp,
                        offset: cap_offset,
                        src: Operand::Virtual(cap_vreg),
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
                    // String: load ptr, len, and cap from consecutive slots
                    let ptr_vreg = self.mir.alloc_vreg();
                    let len_vreg = self.mir.alloc_vreg();
                    let cap_vreg = self.mir.alloc_vreg();

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

                    // Load cap from slot + 2
                    let cap_offset = self.local_offset(slot + 2);
                    self.mir.push(X86Inst::MovRM {
                        dst: Operand::Virtual(cap_vreg),
                        base: Reg::Rbp,
                        offset: cap_offset,
                    });

                    // Register String fields (ptr, len, cap)
                    self.struct_slot_vregs
                        .insert(value, vec![ptr_vreg, len_vreg, cap_vreg]);
                    self.value_map.insert(value, ptr_vreg);
                } else if let Type::Struct(struct_id) = load_type {
                    // Struct: load all field slots (recursively flattened)
                    let slot_count = self.type_slot_count(Type::Struct(struct_id));
                    let mut slot_vregs = Vec::with_capacity(slot_count as usize);

                    for i in 0..slot_count {
                        let field_vreg = self.mir.alloc_vreg();
                        let field_offset = self.local_offset(slot + i);
                        self.mir.push(X86Inst::MovRM {
                            dst: Operand::Virtual(field_vreg),
                            base: Reg::Rbp,
                            offset: field_offset,
                        });
                        slot_vregs.push(field_vreg);
                    }

                    // Register struct field vregs
                    self.struct_slot_vregs.insert(value, slot_vregs.clone());

                    // Use first field as the primary vreg
                    if let Some(&first_vreg) = slot_vregs.first() {
                        self.value_map.insert(value, first_vreg);
                    } else {
                        let vreg = self.mir.alloc_vreg();
                        self.value_map.insert(value, vreg);
                    }
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
                    // String: store ptr, len, and cap to consecutive slots
                    let field_vregs = self
                        .struct_slot_vregs
                        .get(val)
                        .cloned()
                        .expect("string should have fat pointer fields in Store");
                    debug_assert_eq!(
                        field_vregs.len(),
                        3,
                        "string should have 3 fields (ptr, len, cap)"
                    );

                    let ptr_vreg = field_vregs[0];
                    let len_vreg = field_vregs[1];
                    let cap_vreg = field_vregs[2];

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

                    // Store cap to slot + 2
                    let cap_offset = self.local_offset(slot + 2);
                    self.mir.push(X86Inst::MovMR {
                        base: Reg::Rbp,
                        offset: cap_offset,
                        src: Operand::Virtual(cap_vreg),
                    });
                } else {
                    let val_vreg = self.get_vreg(*val);

                    // Check if this slot corresponds to an inout parameter
                    if let Some(param_index) = self.slot_to_inout_param_index(*slot) {
                        // For inout params, store through the pointer
                        // Use ensure_inout_param_ptr in case the param was never accessed via Param instruction
                        let ptr_vreg = self.ensure_inout_param_ptr(param_index);
                        self.mir.push(X86Inst::MovMRIndexed {
                            base: ptr_vreg,
                            offset: 0,
                            src: Operand::Virtual(val_vreg),
                        });
                    } else {
                        // Normal local variable: store to stack slot
                        let offset = self.local_offset(*slot);
                        self.mir.push(X86Inst::MovMR {
                            base: Reg::Rbp,
                            offset,
                            src: Operand::Virtual(val_vreg),
                        });
                    }
                }
            }

            CfgInstData::ParamStore {
                param_slot,
                value: val,
            } => {
                // ParamStore is used for inout params - store through the pointer
                let val_vreg = self.get_vreg(*val);

                // For inout params, param_slot is the first ABI slot for that param.
                // For scalar params, param_slot = param_index.
                // For struct params, param_slot is the first slot (same as param_index for first param).
                // We use is_param_inout(param_slot) to check if this slot is inout.
                if self.cfg.is_param_inout(*param_slot) {
                    // Use ensure_inout_param_ptr in case the param was never accessed via Param instruction
                    let ptr_vreg = self.ensure_inout_param_ptr(*param_slot);
                    self.mir.push(X86Inst::MovMRIndexed {
                        base: ptr_vreg,
                        offset: 0,
                        src: Operand::Virtual(val_vreg),
                    });
                } else {
                    panic!("ParamStore used on non-inout param slot {}", param_slot);
                }
            }

            CfgInstData::Call {
                name,
                args_start,
                args_len,
            } => {
                let result_vreg = self.mir.alloc_vreg();
                self.value_map.insert(value, result_vreg);

                // Check if this call returns a String (uses sret convention)
                let is_sret_call = ty == Type::String;

                // For sret calls, allocate 32 bytes on stack for the return value (24 bytes + padding for 16-byte alignment)
                // We'll pass a pointer to this space as the first argument
                // Use add with negative offset (no SubRI in MIR)
                if is_sret_call {
                    self.mir.push(X86Inst::AddRI {
                        dst: Operand::Physical(Reg::Rsp),
                        imm: -32,
                    });
                }

                // Flatten struct arguments and handle by-ref arguments (inout and borrow)
                let mut flattened_vregs: Vec<VReg> = Vec::new();

                // For sret calls, the first argument is the output pointer (current rsp)
                if is_sret_call {
                    let sret_ptr_vreg = self.mir.alloc_vreg();
                    self.mir.push(X86Inst::MovRR {
                        dst: Operand::Virtual(sret_ptr_vreg),
                        src: Operand::Physical(Reg::Rsp),
                    });
                    flattened_vregs.push(sret_ptr_vreg);
                }
                let args = self.cfg.get_call_args(*args_start, *args_len).to_vec();
                for arg in &args {
                    let arg_value = arg.value;
                    let arg_type = self.cfg.get_inst(arg_value).ty;

                    // For by-ref args (inout or borrow), pass address instead of value
                    if arg.is_by_ref() {
                        let arg_data = &self.cfg.get_inst(arg_value).data;
                        let addr_vreg = self.mir.alloc_vreg();

                        match arg_data {
                            CfgInstData::Load { slot } => {
                                // Emit lea to get the address of the local variable
                                let offset = self.local_offset(*slot);
                                self.mir.push(X86Inst::Lea {
                                    dst: Operand::Virtual(addr_vreg),
                                    base: Reg::Rbp,
                                    index: None,
                                    scale: 1,
                                    disp: offset,
                                });
                            }
                            CfgInstData::Param { index } => {
                                // Check if this param is itself a by-ref param (forwarding case)
                                if self.cfg.is_param_inout(*index) {
                                    // For by-ref param, just pass the pointer we received
                                    // Use ensure_inout_param_ptr in case the param was never accessed via Param instruction
                                    let ptr_vreg = self.ensure_inout_param_ptr(*index);
                                    self.mir.push(X86Inst::MovRR {
                                        dst: Operand::Virtual(addr_vreg),
                                        src: Operand::Virtual(ptr_vreg),
                                    });
                                } else {
                                    // Normal param: emit lea to get its address
                                    let param_slot = self.num_locals + *index;
                                    let offset = self.local_offset(param_slot);
                                    self.mir.push(X86Inst::Lea {
                                        dst: Operand::Virtual(addr_vreg),
                                        base: Reg::Rbp,
                                        index: None,
                                        scale: 1,
                                        disp: offset,
                                    });
                                }
                            }
                            _ => {
                                // For other sources (StructInit, Call result, etc.),
                                // we can't take an address - this should have been caught earlier
                                panic!("by-ref argument must be a variable, not {:?}", arg_data);
                            }
                        }
                        flattened_vregs.push(addr_vreg);
                        continue;
                    }

                    match arg_type {
                        Type::Struct(struct_id) => {
                            let arg_data = &self.cfg.get_inst(arg_value).data;
                            let slot_count = self.type_slot_count(Type::Struct(struct_id));
                            match arg_data {
                                CfgInstData::Load { slot } => {
                                    for slot_idx in 0..slot_count {
                                        let slot_vreg = self.mir.alloc_vreg();
                                        let actual_slot = slot + slot_idx;
                                        let offset = self.local_offset(actual_slot);
                                        self.mir.push(X86Inst::MovRM {
                                            dst: Operand::Virtual(slot_vreg),
                                            base: Reg::Rbp,
                                            offset,
                                        });
                                        flattened_vregs.push(slot_vreg);
                                    }
                                }
                                CfgInstData::Param { index } => {
                                    for slot_idx in 0..slot_count {
                                        let slot_vreg = self.mir.alloc_vreg();
                                        let param_slot = self.num_locals + index + slot_idx;
                                        let offset = self.local_offset(param_slot);
                                        self.mir.push(X86Inst::MovRM {
                                            dst: Operand::Virtual(slot_vreg),
                                            base: Reg::Rbp,
                                            offset,
                                        });
                                        flattened_vregs.push(slot_vreg);
                                    }
                                }
                                CfgInstData::StructInit { .. } | CfgInstData::Call { .. } => {
                                    if let Some(field_vregs) =
                                        self.struct_slot_vregs.get(&arg_value)
                                    {
                                        flattened_vregs.extend(field_vregs.iter().copied());
                                    } else {
                                        flattened_vregs.push(self.get_vreg(arg_value));
                                    }
                                }
                                _ => {
                                    flattened_vregs.push(self.get_vreg(arg_value));
                                }
                            }
                        }
                        Type::Array(array_type_id) => {
                            let arg_data = &self.cfg.get_inst(arg_value).data;
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
                                    if let Some(elem_vregs) = self.struct_slot_vregs.get(&arg_value)
                                    {
                                        flattened_vregs.extend(elem_vregs.iter().copied());
                                    } else {
                                        flattened_vregs.push(self.get_vreg(arg_value));
                                    }
                                }
                                _ => {
                                    flattened_vregs.push(self.get_vreg(arg_value));
                                }
                            }
                        }
                        Type::String => {
                            // String has 3 slots: ptr, len, cap
                            // Handle like a struct with 3 fields
                            let arg_data = &self.cfg.get_inst(arg_value).data;
                            match arg_data {
                                CfgInstData::Load { slot } => {
                                    // Load all 3 String fields from stack
                                    for field_idx in 0..3u32 {
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
                                    // Load all 3 String fields from param slots
                                    for field_idx in 0..3u32 {
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
                                CfgInstData::StringConst(_) | CfgInstData::Call { .. } => {
                                    // String from a call result or string const - use vregs
                                    if let Some(field_vregs) =
                                        self.struct_slot_vregs.get(&arg_value)
                                    {
                                        flattened_vregs.extend(field_vregs.iter().copied());
                                    } else {
                                        // Single vreg case (shouldn't happen for String)
                                        flattened_vregs.push(self.get_vreg(arg_value));
                                    }
                                }
                                _ => {
                                    // Fallback - should be rare for String
                                    flattened_vregs.push(self.get_vreg(arg_value));
                                }
                            }
                        }
                        _ => {
                            flattened_vregs.push(self.get_vreg(arg_value));
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

                let symbol_name = self.interner.resolve(name);
                let symbol_id = self.intern_symbol(symbol_name);
                self.mir.push(X86Inst::CallRel { symbol_id });

                // Clean up stack arguments
                if num_stack_args > 0 {
                    let stack_space = (num_stack_args * 8) as i32;
                    self.mir.push(X86Inst::AddRI {
                        dst: Operand::Physical(Reg::Rsp),
                        imm: stack_space,
                    });
                }

                // Handle struct and string returns (multi-slot types)
                if let Type::Struct(struct_id) = ty {
                    let slot_count = self.type_slot_count(Type::Struct(struct_id));
                    let mut slot_vregs = Vec::new();
                    for slot_idx in 0..slot_count {
                        let slot_vreg = self.mir.alloc_vreg();
                        if (slot_idx as usize) < RET_REGS.len() {
                            self.mir.push(X86Inst::MovRR {
                                dst: Operand::Virtual(slot_vreg),
                                src: Operand::Physical(RET_REGS[slot_idx as usize]),
                            });
                        }
                        slot_vregs.push(slot_vreg);
                    }
                    self.struct_slot_vregs.insert(value, slot_vregs.clone());
                    if let Some(&first_vreg) = slot_vregs.first() {
                        self.mir.push(X86Inst::MovRR {
                            dst: Operand::Virtual(result_vreg),
                            src: Operand::Virtual(first_vreg),
                        });
                    }
                } else if ty == Type::String {
                    // String uses sret convention: result was written to [rsp]
                    // Load ptr, len, cap from stack
                    let mut slot_vregs = Vec::new();
                    for slot_idx in 0..3 {
                        let slot_vreg = self.mir.alloc_vreg();
                        let offset = (slot_idx * 8) as i32;
                        self.mir.push(X86Inst::MovRM {
                            dst: Operand::Virtual(slot_vreg),
                            base: Reg::Rsp,
                            offset,
                        });
                        slot_vregs.push(slot_vreg);
                    }
                    // Pop the sret space (32 bytes including alignment padding)
                    self.mir.push(X86Inst::AddRI {
                        dst: Operand::Physical(Reg::Rsp),
                        imm: 32,
                    });
                    self.struct_slot_vregs.insert(value, slot_vregs.clone());
                    self.mir.push(X86Inst::MovRR {
                        dst: Operand::Virtual(result_vreg),
                        src: Operand::Virtual(slot_vregs[0]),
                    });
                } else {
                    self.mir.push(X86Inst::MovRR {
                        dst: Operand::Virtual(result_vreg),
                        src: Operand::Physical(Reg::Rax),
                    });
                }
            }

            CfgInstData::Intrinsic {
                name,
                args_start,
                args_len,
            } => {
                let name_str = self.interner.resolve(name);
                if name_str == "dbg" {
                    let args = self.cfg.get_extra(*args_start, *args_len);
                    let arg_val = args[0];
                    let arg_type = self.cfg.get_inst(arg_val).ty;

                    // Handle string type specially
                    if arg_type == Type::String {
                        // Get the fat pointer (ptr, len, cap) from struct_slot_vregs
                        if let Some(field_vregs) = self.struct_slot_vregs.get(&arg_val).cloned() {
                            debug_assert_eq!(
                                field_vregs.len(),
                                3,
                                "string should have exactly 3 vregs (ptr, len, cap)"
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
                            let symbol_id = self.intern_symbol("__rue_dbg_str");
                            self.mir.push(X86Inst::CallRel { symbol_id });
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

                        let symbol_id = self.intern_symbol(runtime_fn);
                        self.mir.push(X86Inst::CallRel { symbol_id });

                        let result_vreg = self.mir.alloc_vreg();
                        self.value_map.insert(value, result_vreg);
                    }
                }
            }

            CfgInstData::StructInit {
                struct_id: _,
                fields_start,
                fields_len,
            } => {
                let vreg = self.mir.alloc_vreg();
                self.value_map.insert(value, vreg);

                // Collect all slot vregs for the struct.
                // For scalar fields, this is a single vreg.
                // For nested struct fields, recursively collect all slot vregs.
                let mut slot_vregs = Vec::new();
                let fields = self.cfg.get_extra(*fields_start, *fields_len).to_vec();
                for field in &fields {
                    let field_inst = self.cfg.get_inst(*field);
                    if let Type::Struct(_) = field_inst.ty {
                        // Nested struct - get all its slot vregs
                        let nested_vregs = self
                            .struct_slot_vregs
                            .get(field)
                            .cloned()
                            .expect("nested struct field should have slot vregs in cache");
                        slot_vregs.extend(nested_vregs);
                    } else {
                        // Scalar field - single vreg
                        slot_vregs.push(self.get_vreg(*field));
                    }
                }

                if let Some(&first_vreg) = slot_vregs.first() {
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

                self.struct_slot_vregs.insert(value, slot_vregs);
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
                            // Check if this is an inout parameter
                            if self.cfg.is_param_inout(index) {
                                // For inout params, use the pointer we stored earlier
                                // Use ensure_inout_param_ptr in case the param was never accessed via Param instruction
                                let ptr_vreg = self.ensure_inout_param_ptr(index);
                                // Load from pointer - field offset (negative because stack grows down)
                                self.mir.push(X86Inst::MovRMIndexed {
                                    dst: Operand::Virtual(vreg),
                                    base: ptr_vreg,
                                    offset: -((total_offset as i32) * 8),
                                });
                            } else {
                                // Non-inout param: struct is copied to our stack
                                let param_slot = self.num_locals + index + total_offset;
                                let offset = self.local_offset(param_slot);
                                self.mir.push(X86Inst::MovRM {
                                    dst: Operand::Virtual(vreg),
                                    base: Reg::Rbp,
                                    offset,
                                });
                            }
                        }
                    }
                } else {
                    // For other sources (BlockParam, StructInit, Call), use slot vregs.
                    // IMPORTANT: struct_slot_vregs contains slot vregs (accounting for
                    // nested struct sizes), so we need to use the slot offset, not field index.
                    let slot_offset = self.struct_field_slot_offset(*struct_id, *field_index);
                    let slot_vregs = self
                        .struct_slot_vregs
                        .get(base)
                        .cloned()
                        .expect("struct base should have slot vregs in cache");
                    let slot_vreg = *slot_vregs
                        .get(slot_offset as usize)
                        .expect("slot_offset should be within slot_vregs bounds");
                    self.mir.push(X86Inst::MovRR {
                        dst: Operand::Virtual(vreg),
                        src: Operand::Virtual(slot_vreg),
                    });
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

            CfgInstData::ParamFieldSet {
                param_slot,
                inner_offset,
                struct_id,
                field_index,
                value: val,
            } => {
                let val_vreg = self.get_vreg(*val);
                let field_slot_offset = self.struct_field_slot_offset(*struct_id, *field_index);
                let total_offset = *inner_offset + field_slot_offset;

                // Check if this is an inout parameter
                if self.cfg.is_param_inout(*param_slot) {
                    // For inout params, store through the pointer
                    // Use ensure_inout_param_ptr in case the param was never accessed via Param instruction
                    let ptr_vreg = self.ensure_inout_param_ptr(*param_slot);
                    // Negative offset because stack grows down
                    self.mir.push(X86Inst::MovMRIndexed {
                        base: ptr_vreg,
                        offset: -((total_offset as i32) * 8),
                        src: Operand::Virtual(val_vreg),
                    });
                } else {
                    // Non-inout param: struct is on our stack
                    let param_stack_slot = self.num_locals + *param_slot + total_offset;
                    let offset = self.local_offset(param_stack_slot);
                    self.mir.push(X86Inst::MovMR {
                        base: Reg::Rbp,
                        offset,
                        src: Operand::Virtual(val_vreg),
                    });
                }
            }

            CfgInstData::ArrayInit {
                array_type_id: _,
                elements_start,
                elements_len,
            } => {
                // Array is stored in local slots; we just create vregs for elements.
                // The actual storage is handled by the Alloc that precedes this.
                // For now, just create a dummy vreg - arrays are passed by loading from slots.
                let vreg = self.mir.alloc_vreg();
                self.value_map.insert(value, vreg);

                // Store element vregs for later IndexGet access
                let elements = self.cfg.get_extra(*elements_start, *elements_len);
                let element_vregs: Vec<VReg> = elements.iter().map(|e| self.get_vreg(*e)).collect();
                self.struct_slot_vregs.insert(value, element_vregs);

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

                // First, check if base is an ArrayInit (constant index case)
                let base_data = &self.cfg.get_inst(*base).data.clone();
                if let CfgInstData::ArrayInit { .. } = base_data {
                    // For ArrayInit sources, use element vregs if index is constant
                    let index_inst = &self.cfg.get_inst(*index).data;
                    if let CfgInstData::Const(idx) = index_inst {
                        if let Some(element_vregs) = self.struct_slot_vregs.get(base).cloned() {
                            if let Some(&elem_vreg) = element_vregs.get(*idx as usize) {
                                self.mir.push(X86Inst::MovRR {
                                    dst: Operand::Virtual(vreg),
                                    src: Operand::Virtual(elem_vreg),
                                });
                                return;
                            }
                        }
                    }
                    // Fallback for non-constant index into ArrayInit
                    self.mir.push(X86Inst::MovRI32 {
                        dst: Operand::Virtual(vreg),
                        imm: 0,
                    });
                    return;
                }

                // Use trace_index_chain to handle arbitrary nesting depth
                if let Some((chain_base, mut levels)) = self.trace_index_chain(*base) {
                    // Add this level's index to the chain
                    levels.push(IndexLevel {
                        index: *index,
                        elem_slot_count,
                        array_type_id: *array_type_id,
                    });

                    // Emit bounds check for the innermost index
                    let innermost_index_vreg = self.get_vreg(*index);
                    let array_length = self.array_length(*array_type_id);
                    self.emit_bounds_check(innermost_index_vreg, array_length);

                    // Calculate total offset by summing index * stride for each level
                    let mut total_offset_vreg: Option<VReg> = None;

                    for level in &levels {
                        let level_index_vreg = self.get_vreg(level.index);
                        let level_stride = level.elem_slot_count;

                        // Scale this level's index by its stride
                        let scaled = self.mir.alloc_vreg();
                        self.mir.push(X86Inst::MovRR {
                            dst: Operand::Virtual(scaled),
                            src: Operand::Virtual(level_index_vreg),
                        });

                        if level_stride == 1 {
                            // Simple case: just shift by 3 (multiply by 8)
                            let shift_count = self.mir.alloc_vreg();
                            self.mir.push(X86Inst::MovRI32 {
                                dst: Operand::Virtual(shift_count),
                                imm: 3,
                            });
                            self.mir.push(X86Inst::Shl {
                                dst: Operand::Virtual(scaled),
                                count: Operand::Virtual(shift_count),
                            });
                        } else {
                            // Multiply by stride * 8
                            let stride_vreg = self.mir.alloc_vreg();
                            self.mir.push(X86Inst::MovRI64 {
                                dst: Operand::Virtual(stride_vreg),
                                imm: (level_stride * 8) as i64,
                            });
                            self.mir.push(X86Inst::ImulRR64 {
                                dst: Operand::Virtual(scaled),
                                src: Operand::Virtual(stride_vreg),
                            });
                        }

                        // Add to running total
                        if let Some(prev_total) = total_offset_vreg {
                            self.mir.push(X86Inst::AddRR64 {
                                dst: Operand::Virtual(prev_total),
                                src: Operand::Virtual(scaled),
                            });
                            // prev_total is modified in place, so keep using it
                        } else {
                            total_offset_vreg = Some(scaled);
                        }
                    }

                    // Compute base address - different for inout params vs locals/normal params
                    let addr_vreg = self.mir.alloc_vreg();

                    if let IndexChainBase::Param { index: param_index } = &chain_base {
                        // Check if this is an inout parameter
                        if self.cfg.is_param_inout(*param_index) {
                            // For inout params, use the stored pointer
                            // Use ensure_inout_param_ptr in case the param was never accessed via Param instruction
                            let ptr_vreg = self.ensure_inout_param_ptr(*param_index);
                            self.mir.push(X86Inst::MovRR {
                                dst: Operand::Virtual(addr_vreg),
                                src: Operand::Virtual(ptr_vreg),
                            });

                            // Subtract total offset
                            if let Some(total) = total_offset_vreg {
                                self.mir.push(X86Inst::SubRR64 {
                                    dst: Operand::Virtual(addr_vreg),
                                    src: Operand::Virtual(total),
                                });
                            }

                            // Load from computed address
                            self.mir.push(X86Inst::MovRMIndexed {
                                dst: Operand::Virtual(vreg),
                                base: addr_vreg,
                                offset: 0,
                            });
                            return;
                        }
                    }

                    // Normal case: compute from RBP offset
                    let base_offset = match &chain_base {
                        IndexChainBase::Load { slot } => self.local_offset(*slot),
                        IndexChainBase::Param { index: param_index } => {
                            let base_slot = self.num_locals + *param_index as u32;
                            self.local_offset(base_slot)
                        }
                        IndexChainBase::FieldGet {
                            struct_base_slot,
                            field_slot_offset,
                        } => {
                            let array_base_slot = struct_base_slot + field_slot_offset;
                            self.local_offset(array_base_slot)
                        }
                    };

                    self.mir.push(X86Inst::Lea {
                        dst: Operand::Virtual(addr_vreg),
                        base: Reg::Rbp,
                        index: None,
                        scale: 1,
                        disp: base_offset,
                    });

                    // Subtract total offset
                    if let Some(total) = total_offset_vreg {
                        self.mir.push(X86Inst::SubRR64 {
                            dst: Operand::Virtual(addr_vreg),
                            src: Operand::Virtual(total),
                        });
                    }

                    // Load from computed address
                    self.mir.push(X86Inst::MovRMIndexed {
                        dst: Operand::Virtual(vreg),
                        base: addr_vreg,
                        offset: 0,
                    });
                } else {
                    // Fallback for unsupported patterns
                    self.mir.push(X86Inst::MovRI32 {
                        dst: Operand::Virtual(vreg),
                        imm: 0,
                    });
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

            CfgInstData::ParamIndexSet {
                param_slot,
                array_type_id,
                index,
                value: val,
            } => {
                let val_vreg = self.get_vreg(*val);
                let index_vreg = self.get_vreg(*index);

                // Emit runtime bounds check
                let array_length = self.array_length(*array_type_id);
                self.emit_bounds_check(index_vreg, array_length);

                // Scale index by 8 (element size)
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

                // For inout params, store through the pointer
                // Use ensure_inout_param_ptr in case the param was never accessed via Param instruction
                let ptr_vreg = self.ensure_inout_param_ptr(*param_slot);
                // Calculate address: ptr - (index * 8)
                // (Arrays are stored with element 0 at the highest address)
                let addr_vreg = self.mir.alloc_vreg();
                self.mir.push(X86Inst::MovRR {
                    dst: Operand::Virtual(addr_vreg),
                    src: Operand::Virtual(ptr_vreg),
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

            CfgInstData::IntCast {
                value: src_value,
                from_ty,
            } => {
                // Integer cast with runtime range check
                let vreg = self.mir.alloc_vreg();
                self.value_map.insert(value, vreg);

                let src_vreg = self.get_vreg(*src_value);
                let to_ty = self.cfg.get_inst(value).ty;

                // Emit range check and panic if out of bounds
                self.emit_int_cast_check(src_vreg, *from_ty, to_ty);

                // Move the value to the result vreg (the bits are already correct
                // after sign/zero extension from the range check or simple copy)
                self.mir.push(X86Inst::MovRR {
                    dst: Operand::Virtual(vreg),
                    src: Operand::Virtual(src_vreg),
                });
            }

            CfgInstData::Drop {
                value: dropped_value,
            } => {
                // Drop instruction - runs destructor if the type needs one.
                // The CFG builder already elides Drop for trivially droppable types,
                // so reaching here means we need to emit actual cleanup code.
                //
                // Get the type of the value being dropped to determine which
                // destructor function to call.
                let dropped_ty = self.cfg.get_inst(*dropped_value).ty;

                // Handle String specially - it's a fat pointer (ptr, len, cap)
                if dropped_ty == Type::String {
                    // String requires all 3 slots as arguments to __rue_drop_String
                    // First, try to get the vregs from cache
                    let field_vregs =
                        if let Some(vregs) = self.struct_slot_vregs.get(dropped_value).cloned() {
                            vregs
                        } else {
                            // Not in cache - check if it's a Param instruction
                            let dropped_inst = &self.cfg.get_inst(*dropped_value).data;
                            if let CfgInstData::Param { index } = dropped_inst {
                                // Load all 3 String fields from param slots
                                let mut vregs = Vec::with_capacity(3);
                                for field_idx in 0..3u32 {
                                    let field_vreg = self.mir.alloc_vreg();
                                    let param_slot = self.num_locals + index + field_idx;
                                    let offset = self.local_offset(param_slot);
                                    self.mir.push(X86Inst::MovRM {
                                        dst: Operand::Virtual(field_vreg),
                                        base: Reg::Rbp,
                                        offset,
                                    });
                                    vregs.push(field_vreg);
                                }
                                vregs
                            } else if let CfgInstData::Load { slot } = dropped_inst {
                                // Load from local variable slots
                                let mut vregs = Vec::with_capacity(3);
                                for field_idx in 0..3u32 {
                                    let field_vreg = self.mir.alloc_vreg();
                                    let field_slot = slot + field_idx;
                                    let offset = self.local_offset(field_slot);
                                    self.mir.push(X86Inst::MovRM {
                                        dst: Operand::Virtual(field_vreg),
                                        base: Reg::Rbp,
                                        offset,
                                    });
                                    vregs.push(field_vreg);
                                }
                                vregs
                            } else {
                                unreachable!(
                                    "String value should have field vregs or be a Param/Load: {:?}",
                                    dropped_inst
                                );
                            }
                        };

                    debug_assert_eq!(
                        field_vregs.len(),
                        3,
                        "String should have 3 slots (ptr, len, cap)"
                    );
                    // Move all 3 components into argument registers (rdi, rsi, rdx)
                    self.mir.push(X86Inst::MovRR {
                        dst: Operand::Physical(ARG_REGS[0]),
                        src: Operand::Virtual(field_vregs[0]), // ptr
                    });
                    self.mir.push(X86Inst::MovRR {
                        dst: Operand::Physical(ARG_REGS[1]),
                        src: Operand::Virtual(field_vregs[1]), // len
                    });
                    self.mir.push(X86Inst::MovRR {
                        dst: Operand::Physical(ARG_REGS[2]),
                        src: Operand::Virtual(field_vregs[2]), // cap
                    });

                    let symbol_id = self.intern_symbol("__rue_drop_String");
                    self.mir.push(X86Inst::CallRel { symbol_id });
                    return;
                }

                // Handle struct drops - need to pass all flattened field values
                if let Type::Struct(struct_id) = dropped_ty {
                    let struct_def = &self.struct_defs[struct_id.0 as usize];

                    // Collect all scalar vregs for this struct (flattened)
                    let field_vregs = self.collect_struct_scalar_vregs(*dropped_value);

                    // For now, we only support structs that fit in registers
                    // This covers the common case. Stack spilling can be added later.
                    debug_assert!(
                        field_vregs.len() <= ARG_REGS.len(),
                        "struct drop with {} fields exceeds {} argument registers",
                        field_vregs.len(),
                        ARG_REGS.len()
                    );

                    // For user-defined destructor, we need to call it first with all fields
                    if let Some(ref destructor_name) = struct_def.destructor {
                        // Pass all fields to the user destructor
                        for (i, vreg) in field_vregs.iter().enumerate() {
                            self.mir.push(X86Inst::MovRR {
                                dst: Operand::Physical(ARG_REGS[i]),
                                src: Operand::Virtual(*vreg),
                            });
                        }
                        let symbol_id = self.intern_symbol(destructor_name);
                        self.mir.push(X86Inst::CallRel { symbol_id });
                    }

                    // Now call the drop glue function to drop fields
                    // Pass all fields again (call may have clobbered registers)
                    for (i, vreg) in field_vregs.iter().enumerate() {
                        self.mir.push(X86Inst::MovRR {
                            dst: Operand::Physical(ARG_REGS[i]),
                            src: Operand::Virtual(*vreg),
                        });
                    }

                    let drop_fn_name = format!("__rue_drop_{}", struct_def.name);
                    let symbol_id = self.intern_symbol(&drop_fn_name);
                    self.mir.push(X86Inst::CallRel { symbol_id });
                    return;
                }

                // Handle array drops - need to pass all element values
                if let Type::Array(array_id) = dropped_ty {
                    // Collect all scalar vregs for this array (flattened)
                    let element_vregs = self.collect_array_scalar_vregs(*dropped_value);

                    // For now, we only support arrays that fit in registers
                    debug_assert!(
                        element_vregs.len() <= ARG_REGS.len(),
                        "array drop with {} element slots exceeds {} argument registers",
                        element_vregs.len(),
                        ARG_REGS.len()
                    );

                    // Pass all element slots to the drop glue function
                    for (i, vreg) in element_vregs.iter().enumerate() {
                        self.mir.push(X86Inst::MovRR {
                            dst: Operand::Physical(ARG_REGS[i]),
                            src: Operand::Virtual(*vreg),
                        });
                    }

                    let drop_fn_name =
                        types::array_drop_glue_name(array_id, self.array_types, self.struct_defs);
                    let symbol_id = self.intern_symbol(&drop_fn_name);
                    self.mir.push(X86Inst::CallRel { symbol_id });
                    return;
                }

                // For other types that might need drop in the future
                unreachable!(
                    "Drop instruction reached codegen for unexpected type: {:?}",
                    dropped_ty
                );
            }

            CfgInstData::StorageLive { slot: _ } => {
                // StorageLive marks a slot as valid for use.
                // Currently a no-op in codegen. In the future, this could be used
                // for stack slot optimization (LLVM lifetime intrinsics).
            }

            CfgInstData::StorageDead { slot: _ } => {
                // StorageDead marks a slot as no longer in use.
                // Currently a no-op in codegen. In the future, this could be used
                // for stack slot optimization (LLVM lifetime intrinsics).
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
                // Sign-extend to 64-bit and compare with original
                // If they differ, overflow occurred
                let sext_vreg = self.mir.alloc_vreg();
                self.mir.push(X86Inst::Movsx8To64 {
                    dst: Operand::Virtual(sext_vreg),
                    src: Operand::Virtual(result_vreg),
                });
                // Use 64-bit compare since sext_vreg is a 64-bit value
                self.mir.push(X86Inst::Cmp64RR {
                    src1: Operand::Virtual(result_vreg),
                    src2: Operand::Virtual(sext_vreg),
                });
                self.mir.push(X86Inst::Jz { label: ok_label });
            }
            Type::I16 => {
                // For i16: result must be in [-32768, 32767]
                // Sign-extend to 64-bit and compare with original
                let sext_vreg = self.mir.alloc_vreg();
                self.mir.push(X86Inst::Movsx16To64 {
                    dst: Operand::Virtual(sext_vreg),
                    src: Operand::Virtual(result_vreg),
                });
                // Use 64-bit compare since sext_vreg is a 64-bit value
                self.mir.push(X86Inst::Cmp64RR {
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
        let symbol_id = self.intern_symbol("__rue_overflow");
        self.mir.push(X86Inst::CallRel { symbol_id });
        self.mir.push(X86Inst::Label { id: ok_label });
    }

    /// Emit integer cast range check.
    ///
    /// Checks if the source value can be represented in the target type.
    /// Panics via `__rue_intcast_overflow` if the value is out of range.
    fn emit_int_cast_check(&mut self, src_vreg: VReg, from_ty: Type, to_ty: Type) {
        // Get type properties
        let from_signed = from_ty.is_signed();
        let to_signed = to_ty.is_signed();
        let from_bits = Self::type_bits(from_ty);
        let to_bits = Self::type_bits(to_ty);

        // If casting to a larger or equal-sized type with compatible signedness,
        // and source is unsigned or both are signed, no check needed
        if to_bits >= from_bits {
            // Widening or same-size cast
            if from_signed == to_signed {
                // Same signedness, widening - always safe
                return;
            }
            if !from_signed && to_signed && to_bits > from_bits {
                // Unsigned to larger signed - always safe
                return;
            }
            // Signed to same-size unsigned needs check (negative values fail)
            // Unsigned to same-size signed needs check (large values fail)
        }

        let ok_label = self.new_label();

        // Calculate the min and max values for the target type
        let (min_val, max_val) = Self::type_range(to_ty);

        if from_signed {
            // Source is signed - need to check both min and max
            if to_signed {
                // Signed to signed: check MIN <= value <= MAX
                if to_bits < from_bits || (to_bits == from_bits && min_val != i64::MIN) {
                    // Check lower bound
                    let min_vreg = self.mir.alloc_vreg();
                    self.mir.push(X86Inst::MovRI64 {
                        dst: Operand::Virtual(min_vreg),
                        imm: min_val,
                    });
                    if from_bits > 32 {
                        self.mir.push(X86Inst::Cmp64RR {
                            src1: Operand::Virtual(src_vreg),
                            src2: Operand::Virtual(min_vreg),
                        });
                    } else {
                        self.mir.push(X86Inst::CmpRR {
                            src1: Operand::Virtual(src_vreg),
                            src2: Operand::Virtual(min_vreg),
                        });
                    }
                    // For signed comparison, use Jge (jump if greater or equal to min)
                    self.mir.push(X86Inst::Jge { label: ok_label });

                    // Below min - panic
                    let symbol_id = self.intern_symbol("__rue_intcast_overflow");
                    self.mir.push(X86Inst::CallRel { symbol_id });
                    self.mir.push(X86Inst::Label { id: ok_label });

                    let ok_label2 = self.new_label();
                    // Check upper bound
                    let max_vreg = self.mir.alloc_vreg();
                    self.mir.push(X86Inst::MovRI64 {
                        dst: Operand::Virtual(max_vreg),
                        imm: max_val,
                    });
                    if from_bits > 32 {
                        self.mir.push(X86Inst::Cmp64RR {
                            src1: Operand::Virtual(src_vreg),
                            src2: Operand::Virtual(max_vreg),
                        });
                    } else {
                        self.mir.push(X86Inst::CmpRR {
                            src1: Operand::Virtual(src_vreg),
                            src2: Operand::Virtual(max_vreg),
                        });
                    }
                    self.mir.push(X86Inst::Jle { label: ok_label2 });

                    // Above max - panic
                    let symbol_id = self.intern_symbol("__rue_intcast_overflow");
                    self.mir.push(X86Inst::CallRel { symbol_id });
                    self.mir.push(X86Inst::Label { id: ok_label2 });
                }
            } else {
                // Signed to unsigned: value must be >= 0 and <= max
                // Check for negative
                if from_bits > 32 {
                    self.mir.push(X86Inst::Cmp64RI {
                        src: Operand::Virtual(src_vreg),
                        imm: 0,
                    });
                } else {
                    self.mir.push(X86Inst::CmpRI {
                        src: Operand::Virtual(src_vreg),
                        imm: 0,
                    });
                }
                self.mir.push(X86Inst::Jge { label: ok_label });

                // Negative - panic
                let symbol_id = self.intern_symbol("__rue_intcast_overflow");
                self.mir.push(X86Inst::CallRel { symbol_id });
                self.mir.push(X86Inst::Label { id: ok_label });

                // Also check upper bound if narrowing
                if to_bits < from_bits {
                    let ok_label2 = self.new_label();
                    let max_vreg = self.mir.alloc_vreg();
                    self.mir.push(X86Inst::MovRI64 {
                        dst: Operand::Virtual(max_vreg),
                        imm: max_val,
                    });
                    if from_bits > 32 {
                        self.mir.push(X86Inst::Cmp64RR {
                            src1: Operand::Virtual(src_vreg),
                            src2: Operand::Virtual(max_vreg),
                        });
                        // Unsigned comparison for upper bound check
                        self.mir.push(X86Inst::Jbe { label: ok_label2 });
                    } else {
                        self.mir.push(X86Inst::CmpRR {
                            src1: Operand::Virtual(src_vreg),
                            src2: Operand::Virtual(max_vreg),
                        });
                        self.mir.push(X86Inst::Jbe { label: ok_label2 });
                    }

                    // Above max - panic
                    let symbol_id = self.intern_symbol("__rue_intcast_overflow");
                    self.mir.push(X86Inst::CallRel { symbol_id });
                    self.mir.push(X86Inst::Label { id: ok_label2 });
                }
            }
        } else {
            // Source is unsigned
            if to_signed {
                // Unsigned to signed: value must fit in positive range of target
                // Check that value <= signed max
                let max_vreg = self.mir.alloc_vreg();
                self.mir.push(X86Inst::MovRI64 {
                    dst: Operand::Virtual(max_vreg),
                    imm: max_val,
                });
                if from_bits > 32 {
                    self.mir.push(X86Inst::Cmp64RR {
                        src1: Operand::Virtual(src_vreg),
                        src2: Operand::Virtual(max_vreg),
                    });
                } else {
                    self.mir.push(X86Inst::CmpRR {
                        src1: Operand::Virtual(src_vreg),
                        src2: Operand::Virtual(max_vreg),
                    });
                }
                // Unsigned comparison
                self.mir.push(X86Inst::Jbe { label: ok_label });

                // Above max - panic
                let symbol_id = self.intern_symbol("__rue_intcast_overflow");
                self.mir.push(X86Inst::CallRel { symbol_id });
                self.mir.push(X86Inst::Label { id: ok_label });
            } else {
                // Unsigned to unsigned: narrowing check
                if to_bits < from_bits {
                    let max_vreg = self.mir.alloc_vreg();
                    self.mir.push(X86Inst::MovRI64 {
                        dst: Operand::Virtual(max_vreg),
                        imm: max_val,
                    });
                    if from_bits > 32 {
                        self.mir.push(X86Inst::Cmp64RR {
                            src1: Operand::Virtual(src_vreg),
                            src2: Operand::Virtual(max_vreg),
                        });
                    } else {
                        self.mir.push(X86Inst::CmpRR {
                            src1: Operand::Virtual(src_vreg),
                            src2: Operand::Virtual(max_vreg),
                        });
                    }
                    self.mir.push(X86Inst::Jbe { label: ok_label });

                    // Above max - panic
                    let symbol_id = self.intern_symbol("__rue_intcast_overflow");
                    self.mir.push(X86Inst::CallRel { symbol_id });
                    self.mir.push(X86Inst::Label { id: ok_label });
                }
            }
        }
    }

    /// Get the bit width of an integer type.
    fn type_bits(ty: Type) -> u32 {
        match ty {
            Type::I8 | Type::U8 => 8,
            Type::I16 | Type::U16 => 16,
            Type::I32 | Type::U32 => 32,
            Type::I64 | Type::U64 => 64,
            _ => panic!("type_bits called on non-integer type: {:?}", ty),
        }
    }

    /// Get the min and max values for an integer type.
    fn type_range(ty: Type) -> (i64, i64) {
        match ty {
            Type::I8 => (i8::MIN as i64, i8::MAX as i64),
            Type::I16 => (i16::MIN as i64, i16::MAX as i64),
            Type::I32 => (i32::MIN as i64, i32::MAX as i64),
            Type::I64 => (i64::MIN, i64::MAX),
            Type::U8 => (0, u8::MAX as i64),
            Type::U16 => (0, u16::MAX as i64),
            Type::U32 => (0, u32::MAX as i64),
            Type::U64 => (0, i64::MAX), // Can't represent u64::MAX in i64, but we use unsigned compare
            _ => panic!("type_range called on non-integer type: {:?}", ty),
        }
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

    /// Emit struct equality comparison.
    ///
    /// Compares all fields of two structs and returns true only if all fields are equal.
    /// If `invert` is true, returns true if any field is different (for !=).
    fn emit_struct_equality(
        &mut self,
        value: CfgValue,
        lhs: CfgValue,
        rhs: CfgValue,
        struct_id: StructId,
        invert: bool,
    ) {
        let result_vreg = self.mir.alloc_vreg();
        self.value_map.insert(value, result_vreg);

        // Get the struct field vregs
        let lhs_fields = self
            .struct_slot_vregs
            .get(&lhs)
            .cloned()
            .expect("struct should have field vregs");
        let rhs_fields = self
            .struct_slot_vregs
            .get(&rhs)
            .cloned()
            .expect("struct should have field vregs");

        let struct_def = &self.struct_defs[struct_id.0 as usize];
        let field_count = struct_def.fields.len();

        if field_count == 0 {
            // Empty struct: always equal
            self.mir.push(X86Inst::MovRI32 {
                dst: Operand::Virtual(result_vreg),
                imm: if invert { 0 } else { 1 },
            });
            return;
        }

        // Start with 1 (true), AND each field comparison result
        self.mir.push(X86Inst::MovRI32 {
            dst: Operand::Virtual(result_vreg),
            imm: 1,
        });

        // Compare each field and AND with result
        let mut field_slot = 0usize;
        for field in &struct_def.fields {
            let field_slots = self.type_slot_count(field.ty) as usize;
            let lhs_field_vreg = lhs_fields[field_slot];
            let rhs_field_vreg = rhs_fields[field_slot];

            // Allocate a vreg for this field's comparison result
            let cmp_vreg = self.mir.alloc_vreg();

            // Use 64-bit compare for i64/u64 types
            if matches!(field.ty, Type::I64 | Type::U64) {
                self.mir.push(X86Inst::Cmp64RR {
                    src1: Operand::Virtual(lhs_field_vreg),
                    src2: Operand::Virtual(rhs_field_vreg),
                });
            } else {
                self.mir.push(X86Inst::CmpRR {
                    src1: Operand::Virtual(lhs_field_vreg),
                    src2: Operand::Virtual(rhs_field_vreg),
                });
            }
            self.mir.push(X86Inst::Sete {
                dst: Operand::Virtual(cmp_vreg),
            });
            self.mir.push(X86Inst::Movzx {
                dst: Operand::Virtual(cmp_vreg),
                src: Operand::Virtual(cmp_vreg),
            });

            // AND with accumulator
            self.mir.push(X86Inst::AndRR {
                dst: Operand::Virtual(result_vreg),
                src: Operand::Virtual(cmp_vreg),
            });

            field_slot += field_slots;
        }

        // Invert result if needed (for !=)
        if invert {
            self.mir.push(X86Inst::XorRI {
                dst: Operand::Virtual(result_vreg),
                imm: 1,
            });
        }
    }

    /// Emit a call to __rue_str_eq for string comparison.
    ///
    /// Returns the vreg containing the result (0 or 1).
    fn emit_string_eq_call(&mut self, lhs: CfgValue, rhs: CfgValue) -> VReg {
        let result_vreg = self.mir.alloc_vreg();

        // Get string fields (ptr, len, cap) from struct_slot_vregs
        // For comparison, we only use ptr and len (cap is not compared)
        let lhs_fields = self
            .struct_slot_vregs
            .get(&lhs)
            .cloned()
            .expect("string should have fat pointer fields");
        let rhs_fields = self
            .struct_slot_vregs
            .get(&rhs)
            .cloned()
            .expect("string should have fat pointer fields");

        debug_assert_eq!(
            lhs_fields.len(),
            3,
            "string should have 3 fields (ptr, len, cap)"
        );
        debug_assert_eq!(
            rhs_fields.len(),
            3,
            "string should have 3 fields (ptr, len, cap)"
        );

        let lhs_ptr = lhs_fields[0];
        let lhs_len = lhs_fields[1];
        // lhs_fields[2] is cap, not used for comparison
        let rhs_ptr = rhs_fields[0];
        let rhs_len = rhs_fields[1];
        // rhs_fields[2] is cap, not used for comparison

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
        let symbol_id = self.intern_symbol("__rue_str_eq");
        self.mir.push(X86Inst::CallRel { symbol_id });

        // Result is in RAX (0 or 1)
        self.mir.push(X86Inst::MovRR {
            dst: Operand::Virtual(result_vreg),
            src: Operand::Physical(Reg::Rax),
        });

        result_vreg
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
                    // Load case value into a register (supports signed values for negative patterns)
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
                    let symbol_id = self.intern_symbol("__rue_exit");
                    self.mir.push(X86Inst::CallRel { symbol_id });
                } else if let Type::Struct(struct_id) = return_type {
                    // Return struct in registers
                    let slot_count = self.type_slot_count(Type::Struct(struct_id));
                    let value_data = &self.cfg.get_inst(*value).data;

                    match value_data {
                        CfgInstData::StructInit { .. }
                        | CfgInstData::Call { .. }
                        | CfgInstData::BlockParam { .. } => {
                            // Use slot vregs from cache (populated for BlockParam, StructInit, Call)
                            if let Some(slot_vregs) = self.struct_slot_vregs.get(value).cloned() {
                                // Move slot values to return registers in REVERSE order.
                                // This is important because register allocation uses Rax as
                                // scratch when loading spilled values. By moving to Rax last,
                                // we avoid clobbering it with scratch loads for later slots.
                                for (i, slot_vreg) in slot_vregs.iter().enumerate().rev() {
                                    if i < RET_REGS.len() {
                                        self.mir.push(X86Inst::MovRR {
                                            dst: Operand::Physical(RET_REGS[i]),
                                            src: Operand::Virtual(*slot_vreg),
                                        });
                                    }
                                }
                            }
                        }
                        CfgInstData::Param { index } => {
                            for slot_idx in 0..slot_count {
                                let param_slot = self.num_locals + index + slot_idx;
                                let offset = self.local_offset(param_slot);
                                self.mir.push(X86Inst::MovRM {
                                    dst: Operand::Physical(RET_REGS[slot_idx as usize]),
                                    base: Reg::Rbp,
                                    offset,
                                });
                            }
                        }
                        CfgInstData::Load { slot } => {
                            for slot_idx in 0..slot_count {
                                let actual_slot = slot + slot_idx;
                                let offset = self.local_offset(actual_slot);
                                self.mir.push(X86Inst::MovRM {
                                    dst: Operand::Physical(RET_REGS[slot_idx as usize]),
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

                    self.mir.push(X86Inst::Ret);
                } else {
                    let val_vreg = self.get_vreg(*value);
                    self.mir.push(X86Inst::MovRR {
                        dst: Operand::Physical(Reg::Rax),
                        src: Operand::Virtual(val_vreg),
                    });
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
    use lasso::ThreadedRodeo;
    use rue_air::Sema;
    use rue_cfg::CfgBuilder;
    use rue_error::PreviewFeatures;
    use rue_lexer::Lexer;
    use rue_parser::Parser;
    use rue_rir::AstGen;

    fn lower_to_mir(source: &str) -> X86Mir {
        let lexer = Lexer::new(source);
        let (tokens, interner) = lexer.tokenize().unwrap();
        let parser = Parser::new(tokens, interner);
        let (ast, mut interner) = parser.parse().unwrap();

        let astgen = AstGen::new(&ast, &mut interner);
        let rir = astgen.generate();

        let sema = Sema::new(&rir, &mut interner, PreviewFeatures::new());
        let output = sema.analyze_all().unwrap();

        let func = &output.functions[0];
        let struct_defs = &output.struct_defs;
        let array_types = &output.array_types;
        let strings = &output.strings;
        let cfg_output = CfgBuilder::build(
            &func.air,
            func.num_locals,
            func.num_param_slots,
            &func.name,
            struct_defs,
            array_types,
            func.param_modes.clone(),
            &interner,
        );

        CfgLower::new(
            &cfg_output.cfg,
            struct_defs,
            array_types,
            strings,
            &interner,
        )
        .lower()
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
