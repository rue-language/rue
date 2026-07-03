//! CFG to Aarch64Mir lowering.
//!
//! This module converts CFG (explicit control flow graph) to Aarch64Mir
//! (AArch64 instructions with virtual registers).

use std::collections::HashMap;

use lasso::ThreadedRodeo;
use rue_air::{StructId, TypeInternPool, TypeKind};
use rue_cfg::{
    BasicBlock, BlockId, Cfg, CfgInstData, CfgValue, Place, PlaceBase, Projection, Terminator, Type,
};
use rue_target::Target;

use super::mir::{Aarch64Inst, Aarch64Mir, Cond, LabelId, Operand, Reg, VReg};
use crate::cfg_lower::{CfgLowerContext, IndexLevel};
use crate::types;

/// Argument passing registers per AAPCS64. ABI arg slots beyond these are
/// passed on the caller's stack (slot `k >= 8` at `[fp+16+(k-8)*8]` in the
/// callee); the callee prologue copies them into its frame param area so the
/// body addresses every param slot uniformly (see `emit_prologue`).
pub(super) const ARG_REGS: [Reg; 8] = [
    Reg::X0,
    Reg::X1,
    Reg::X2,
    Reg::X3,
    Reg::X4,
    Reg::X5,
    Reg::X6,
    Reg::X7,
];

/// Return value registers for the internal Rue convention (AAPCS64 only
/// defines x0/x1; we extend with the remaining arg registers for multi-slot
/// aggregate returns). Aggregates with more slots than this — and builtin
/// String always — return via sret instead; see
/// `crate::cfg_lower::type_uses_sret_return`. (RUE-106)
pub(super) const RET_REGS: [Reg; 8] = [
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
    /// Shared context with type helpers and chain tracing.
    ctx: CfgLowerContext<'a>,
    /// Interner for resolving Spur to string
    interner: &'a ThreadedRodeo,
    /// Target platform (needed for syscall ABI differences between Linux/macOS).
    target: Target,
    mir: Aarch64Mir,
    /// Maps CFG values to vregs
    value_map: HashMap<CfgValue, VReg>,
    /// Maps block parameters to vregs (block_id, param_index) -> vreg
    block_param_vregs: HashMap<(BlockId, u32), VReg>,
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
        type_pool: &'a TypeInternPool,
        interner: &'a ThreadedRodeo,
        target: Target,
    ) -> Self {
        let num_params = cfg.num_params();

        // Pre-calculate capacity hints to reduce HashMap reallocations
        let num_values = cfg.value_count();
        let num_blocks = cfg.blocks().len();
        // Estimate ~4 block params per block on average
        let estimated_block_params = num_blocks.saturating_mul(4);
        // Estimate ~10% of values are struct inits
        let estimated_struct_inits = num_values / 10;
        // Estimate inout params are rare, start small
        let estimated_inout_params = num_params.min(4) as usize;

        Self {
            ctx: CfgLowerContext::new(cfg, type_pool),
            interner,
            target,
            mir: Aarch64Mir::new(),
            value_map: HashMap::with_capacity(num_values),
            block_param_vregs: HashMap::with_capacity(estimated_block_params),
            fn_name: cfg.fn_name(),
            struct_slot_vregs: HashMap::with_capacity(estimated_struct_inits),
            inout_param_ptrs: HashMap::with_capacity(estimated_inout_params),
        }
    }

    // ========================================================================
    // Helper methods
    // ========================================================================

    /// Intern a symbol name and return its ID.
    fn intern_symbol(&mut self, symbol: &str) -> u32 {
        self.mir.intern_symbol(symbol)
    }

    /// Recursively collect all scalar vregs from an array value.
    fn collect_array_scalar_vregs(&mut self, value: CfgValue) -> Vec<VReg> {
        let slot_vregs = self.struct_slot_vregs.clone();
        types::collect_array_scalar_vregs(self.ctx.cfg, &slot_vregs, value, &mut |v| {
            self.get_vreg(v)
        })
    }

    /// Recursively collect all scalar vregs from a struct value.
    fn collect_struct_scalar_vregs(&mut self, value: CfgValue) -> Vec<VReg> {
        let slot_vregs = self.struct_slot_vregs.clone();
        types::collect_struct_scalar_vregs(self.ctx.cfg, &slot_vregs, value, &mut |v| {
            self.get_vreg(v)
        })
    }

    /// Check if a slot corresponds to an inout parameter.
    fn slot_to_inout_param_index(&self, slot: u32) -> Option<u32> {
        if let Some(param_index) = self.ctx.slot_to_inout_param_index(slot) {
            if self.ctx.cfg.is_param_inout(param_index) {
                return Some(param_index);
            }
        }
        None
    }

    /// Ensure the inout parameter pointer vreg exists for the given param slot.
    fn ensure_inout_param_ptr(&mut self, param_slot: u32) -> VReg {
        if let Some(ptr_vreg) = self.inout_param_ptrs.get(&param_slot).copied() {
            return ptr_vreg;
        }

        // Load the pointer from the param slot. The prologue copies every ABI
        // arg slot — register- and stack-passed alike — into the contiguous
        // frame param area, so this is uniform regardless of param count.
        let ptr_vreg = self.mir.alloc_vreg();
        let slot = self.ctx.num_locals + param_slot;
        let offset = self.ctx.local_offset(slot);
        self.mir.push(Aarch64Inst::Ldr {
            dst: Operand::Virtual(ptr_vreg),
            base: Reg::Fp,
            offset,
        });

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
        self.mir.push(Aarch64Inst::MovImm {
            dst: Operand::Virtual(length_vreg),
            imm: length as i64,
        });

        // Compare index (unsigned) against length. The index is a usize
        // (64-bit) and the length is materialized as a 64-bit immediate, so the
        // comparison MUST be 64-bit — a 32-bit cmp would ignore the high half of
        // the index and let an out-of-range index whose low 32 bits happen to be
        // in range bypass the check, reading out of bounds (RUE-87).
        self.mir.push(Aarch64Inst::Cmp64RR {
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
        let symbol_id = self.intern_symbol("__rue_bounds_check");
        self.mir.push(Aarch64Inst::Bl { symbol_id });

        // Continue with valid access
        self.mir.push(Aarch64Inst::Label { id: ok_label });
    }

    /// Call `symbol` passing `arg_vregs` as flattened by-value slot arguments
    /// per the standard convention: the first slots in ARG_REGS, the rest
    /// stored to a 16-byte-aligned stack area released after the call — the
    /// same shape as the generic `Call` lowering. Used by the Drop paths,
    /// whose destructor/drop-glue calls previously moved slots into argument
    /// registers only and panicked past 8 slots (RUE-193).
    fn emit_call_with_slot_args(&mut self, arg_vregs: &[VReg], symbol: &str) {
        let num_reg_args = arg_vregs.len().min(ARG_REGS.len());
        let num_stack_args = arg_vregs.len().saturating_sub(ARG_REGS.len());

        // Allocate stack space for stack arguments (must be 16-byte aligned)
        let stack_space = if num_stack_args > 0 {
            (num_stack_args * 8).div_ceil(16) * 16
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

        // Store stack arguments to the allocated space
        for (i, arg_vreg) in arg_vregs.iter().skip(ARG_REGS.len()).enumerate() {
            let offset = (i * 8) as i32;
            self.mir.push(Aarch64Inst::Str {
                src: Operand::Virtual(*arg_vreg),
                base: Reg::Sp,
                offset,
            });
        }

        // Move register arguments
        for (i, arg_vreg) in arg_vregs.iter().take(num_reg_args).enumerate() {
            self.mir.push(Aarch64Inst::MovRR {
                dst: Operand::Physical(ARG_REGS[i]),
                src: Operand::Virtual(*arg_vreg),
            });
        }

        let symbol_id = self.intern_symbol(symbol);
        self.mir.push(Aarch64Inst::Bl { symbol_id });

        // Clean up stack space after the call
        if stack_space > 0 {
            self.mir.push(Aarch64Inst::AddImm {
                dst: Operand::Physical(Reg::Sp),
                src: Operand::Physical(Reg::Sp),
                imm: stack_space as i32,
            });
        }
    }

    /// Get the label for a CFG basic block.
    ///
    /// Delegates to [`Aarch64Mir::block_label`]. See the mir module docs for
    /// details on label namespace separation.
    fn block_label(&self, block_id: BlockId) -> LabelId {
        Aarch64Mir::block_label(block_id.as_u32())
    }

    /// Get or compute the slot vregs for a multi-slot aggregate value.
    /// Single shared implementation — see crate::agg_slots. (RUE-121)
    fn get_or_compute_field_vregs(&mut self, value: CfgValue) -> Option<Vec<VReg>> {
        crate::agg_slots::get_or_compute_field_vregs(self, value)
    }

    /// Copy an aggregate value's slot vregs to a block parameter's slot vregs.
    /// Covers structs, builtin String, and fixed-size arrays — every slot must
    /// cross the join edge, not just the primary vreg (RUE-167).
    fn copy_aggregate_to_block_param(
        &mut self,
        arg: CfgValue,
        target_block: BlockId,
        param_idx: u32,
    ) {
        let target_param = self.ctx.cfg.get_block(target_block).params[param_idx as usize].0;

        // The accessor covers StructInit/ArrayInit/Call/BlockParam (cache
        // hits) and Load/Param/static PlaceRead; fall back to the recursive
        // flatteners for anything it doesn't model (mirrors Alloc lowering).
        let src_slots = self.get_or_compute_field_vregs(arg).unwrap_or_else(|| {
            let arg_ty = self.ctx.cfg.get_inst(arg).ty;
            if arg_ty.is_array() {
                self.collect_array_scalar_vregs(arg)
            } else {
                self.collect_struct_scalar_vregs(arg)
            }
        });
        let dst_slots = self
            .struct_slot_vregs
            .get(&target_param)
            .cloned()
            .expect("aggregate block param should have slot vregs pre-allocated");

        debug_assert_eq!(
            src_slots.len(),
            dst_slots.len(),
            "source and destination aggregate slot counts must match"
        );
        for (dst_vreg, src_vreg) in dst_slots.iter().zip(src_slots.iter()) {
            self.mir.push(Aarch64Inst::MovRR {
                dst: Operand::Virtual(*dst_vreg),
                src: Operand::Virtual(*src_vreg),
            });
        }
    }

    /// Lower CFG to Aarch64Mir.
    pub fn lower(mut self) -> Aarch64Mir {
        // Pre-allocate vregs for block parameters
        for block in self.ctx.cfg.blocks() {
            for (param_idx, (param_val, ty)) in block.params.iter().enumerate() {
                let vreg = self.mir.alloc_vreg();
                self.block_param_vregs
                    .insert((block.id, param_idx as u32), vreg);
                self.value_map.insert(*param_val, vreg);

                // For aggregate types (structs, builtin String, fixed-size
                // arrays), also allocate vregs for each slot. Arrays included:
                // their primary vreg is just a placeholder, so without slot
                // vregs every element would be dropped at the join (RUE-167).
                // Exactly slot_count vregs: a zero-slot aggregate ([T; 0],
                // fieldless struct) gets an EMPTY list, matching the empty
                // slot lists its sources cache — a phantom slot here tripped
                // the join's count assert (RUE-194).
                if ty.is_struct() || ty.is_array() {
                    let slot_count = self.ctx.type_slot_count(*ty);
                    let mut slot_vregs = Vec::with_capacity(slot_count as usize);
                    if slot_count > 0 {
                        slot_vregs.push(vreg); // First slot uses main vreg
                    }
                    for _ in 1..slot_count {
                        slot_vregs.push(self.mir.alloc_vreg());
                    }
                    self.struct_slot_vregs.insert(*param_val, slot_vregs);
                }
            }
        }

        // Lower each block
        for block in self.ctx.cfg.blocks() {
            self.lower_block(block);
        }

        self.mir
    }

    /// Lower CFG to Aarch64Mir with debug information about instruction selection.
    ///
    /// This is like `lower()` but also captures detailed information about
    /// how each CFG instruction maps to MIR instructions.
    pub fn lower_with_debug(mut self) -> (Aarch64Mir, crate::LoweringDebugInfo) {
        use crate::cfg_lower::{format_cfg_inst_data, format_terminator};
        use crate::{
            BlockLoweringInfo, LoweringDebugInfo, LoweringDecision, TerminatorLoweringDecision,
        };

        let mut debug_info = LoweringDebugInfo {
            fn_name: self.fn_name.to_string(),
            target_arch: "aarch64".to_string(),
            blocks: Vec::new(),
        };

        // Pre-allocate vregs for block parameters (same as lower())
        for block in self.ctx.cfg.blocks() {
            for (param_idx, (param_val, ty)) in block.params.iter().enumerate() {
                let vreg = self.mir.alloc_vreg();
                self.block_param_vregs
                    .insert((block.id, param_idx as u32), vreg);
                self.value_map.insert(*param_val, vreg);

                if ty.is_struct() || ty.is_array() {
                    // Exactly slot_count vregs; zero-slot aggregates get an
                    // empty list (RUE-194) — same as lower().
                    let slot_count = self.ctx.type_slot_count(*ty);
                    let mut slot_vregs = Vec::with_capacity(slot_count as usize);
                    if slot_count > 0 {
                        slot_vregs.push(vreg);
                    }
                    for _ in 1..slot_count {
                        slot_vregs.push(self.mir.alloc_vreg());
                    }
                    self.struct_slot_vregs.insert(*param_val, slot_vregs);
                }
            }
        }

        // Lower each block with debug tracking
        for block in self.ctx.cfg.blocks() {
            let mut block_info = BlockLoweringInfo {
                block_id: block.id,
                instructions: Vec::new(),
                terminator: None,
            };

            // Emit block label (except for entry block)
            if block.id != self.ctx.cfg.entry {
                self.mir.push(Aarch64Inst::Label {
                    id: self.block_label(block.id),
                });
            }

            // Lower each instruction with tracking
            for &value in &block.insts {
                // Skip if already lowered
                if self.value_map.contains_key(&value) {
                    continue;
                }

                let inst = self.ctx.cfg.get_inst(value);
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
                terminator_desc: format_terminator(self.ctx.cfg, &block.terminator),
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
                Some("With overflow check".to_string())
            }
            CfgInstData::Div(_, _) | CfgInstData::Mod(_, _) => {
                if matches!(ty, Type::I8 | Type::I16 | Type::I32 | Type::I64) {
                    Some("Signed division".to_string())
                } else {
                    Some("Unsigned division".to_string())
                }
            }
            CfgInstData::Shr(_, _) => {
                if matches!(ty, Type::I8 | Type::I16 | Type::I32 | Type::I64) {
                    Some("Signed shift right (ASR) preserves sign bit".to_string())
                } else {
                    Some("Unsigned shift right (LSR) zero-extends".to_string())
                }
            }
            CfgInstData::Call {
                args_start,
                args_len,
                ..
            } => {
                let args = self.ctx.cfg.get_call_args(*args_start, *args_len);
                let inout_count = args.iter().filter(|a| a.is_inout()).count();
                let borrow_count = args.iter().filter(|a| a.is_borrow()).count();
                if inout_count > 0 || borrow_count > 0 {
                    Some(format!(
                        "AAPCS64 with {} inout, {} borrow params (passed as pointers)",
                        inout_count, borrow_count
                    ))
                } else if args.len() > 8 {
                    Some("AAPCS64 with stack-passed arguments".to_string())
                } else {
                    None
                }
            }
            CfgInstData::Param { index } => {
                if self.ctx.cfg.is_param_inout(*index) {
                    Some("Inout param: load pointer then dereference".to_string())
                } else if (*index as usize) < ARG_REGS.len() {
                    Some(format!(
                        "From register {} (AAPCS64)",
                        ARG_REGS[*index as usize]
                    ))
                } else {
                    Some("From stack (AAPCS64, args > 8)".to_string())
                }
            }
            CfgInstData::IndexSet { .. }
            | CfgInstData::PlaceRead { .. }
            | CfgInstData::PlaceWrite { .. } => Some("Includes bounds check".to_string()),
            _ => None,
        }
    }

    /// Generate rationale for terminator lowering decisions.
    fn get_terminator_rationale(&self, terminator: &Terminator) -> Option<String> {
        match terminator {
            Terminator::Branch { .. } => Some("Compare and branch".to_string()),
            Terminator::Return { value } => {
                if self.fn_name == "main" {
                    Some("Main function: return value becomes exit code".to_string())
                } else if value.is_some() {
                    Some("Return value in X0 (AAPCS64)".to_string())
                } else {
                    None
                }
            }
            Terminator::Switch { cases_len, .. } => {
                Some(format!("Linear scan through {} cases", cases_len))
            }
            _ => None,
        }
    }

    /// Lower a single basic block.
    fn lower_block(&mut self, block: &BasicBlock) {
        // Emit block label (except for entry block)
        if block.id != self.ctx.cfg.entry {
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

        let inst = self.ctx.cfg.get_inst(value);
        let ty = inst.ty;

        match &inst.data {
            CfgInstData::Const(v) => {
                let vreg = self.mir.alloc_vreg();
                self.value_map.insert(value, vreg);

                // Cast u64 to i64 to preserve bit pattern
                self.mir.push(Aarch64Inst::MovImm {
                    dst: Operand::Virtual(vreg),
                    imm: *v as i64,
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

            CfgInstData::StringConst(string_id) => {
                let ptr_vreg = self.mir.alloc_vreg();
                let len_vreg = self.mir.alloc_vreg();
                let cap_vreg = self.mir.alloc_vreg();

                self.mir.push(Aarch64Inst::StringConstPtr {
                    dst: Operand::Virtual(ptr_vreg),
                    string_id: *string_id,
                });

                self.mir.push(Aarch64Inst::StringConstLen {
                    dst: Operand::Virtual(len_vreg),
                    string_id: *string_id,
                });

                self.mir.push(Aarch64Inst::StringConstCap {
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
                let is_inout = self.ctx.cfg.is_param_inout(*index);

                // The prologue copies every ABI arg slot — register- and
                // stack-passed alike — into the contiguous frame param area
                // (slots num_locals..num_locals+num_params), so all param
                // reads are uniform frame-slot loads. Previously slots past
                // the 8 arg registers were read from [fp+16+...], which the
                // frame-slot-based aggregate and write paths didn't mirror,
                // dropping slots of >8-slot aggregate args. (RUE-13/79/91)
                if is_inout {
                    // For inout params, the slot contains a POINTER to the caller's memory.
                    // Load the pointer, then dereference to get the value.
                    let ptr_vreg = self.mir.alloc_vreg();
                    let val_vreg = self.mir.alloc_vreg();

                    // Load the pointer from the param slot
                    let slot = self.ctx.num_locals + *index;
                    let offset = self.ctx.local_offset(slot);
                    self.mir.push(Aarch64Inst::Ldr {
                        dst: Operand::Virtual(ptr_vreg),
                        base: Reg::Fp,
                        offset,
                    });

                    // Store the pointer vreg for later use by Store
                    self.inout_param_ptrs.insert(*index, ptr_vreg);

                    // Dereference the pointer to get the actual value
                    self.mir.push(Aarch64Inst::LdrIndexed {
                        dst: Operand::Virtual(val_vreg),
                        base: ptr_vreg,
                    });

                    self.value_map.insert(value, val_vreg);
                } else {
                    // Normal parameter: load the value directly from the slot
                    let vreg = self.mir.alloc_vreg();
                    self.value_map.insert(value, vreg);

                    let slot = self.ctx.num_locals + *index;
                    let offset = self.ctx.local_offset(slot);
                    self.mir.push(Aarch64Inst::Ldr {
                        dst: Operand::Virtual(vreg),
                        base: Reg::Fp,
                        offset,
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

                // Strength reduction: multiply by power of 2 -> shift left
                // This replaces expensive MUL with LSL (typically faster).
                // Only apply to 32/64-bit types - sub-word types (i8, i16, u8, u16)
                // have complex overflow checking that doesn't work well with shifts.
                // Check rhs first (more common: x * constant), then lhs (constant * x)
                //
                // Future optimization: x * 2 could use `add x, x` instead of `lsl x, #1`
                // (same latency but potentially better for some microarchitectures).
                let is_word_or_larger = matches!(ty, Type::I32 | Type::I64 | Type::U32 | Type::U64);
                let shift_amount = if is_word_or_larger {
                    self.try_power_of_two_shift(*rhs)
                        .or_else(|| self.try_power_of_two_shift(*lhs))
                } else {
                    None
                };

                if let Some(shift) = shift_amount {
                    // Use the non-constant operand as the value to shift
                    let src_vreg = if self.try_power_of_two_shift(*rhs).is_some() {
                        lhs_vreg
                    } else {
                        self.get_vreg(*rhs)
                    };

                    // Emit shift left
                    if ty.is_64_bit() {
                        self.mir.push(Aarch64Inst::LslImm {
                            dst: Operand::Virtual(vreg),
                            src: Operand::Virtual(src_vreg),
                            imm: shift,
                        });
                    } else {
                        self.mir.push(Aarch64Inst::Lsl32Imm {
                            dst: Operand::Virtual(vreg),
                            src: Operand::Virtual(src_vreg),
                            imm: shift,
                        });
                    }

                    // Overflow check: shift back and compare with original
                    // If they differ, bits were lost during the shift (overflow)
                    let check_vreg = self.mir.alloc_vreg();

                    // Use arithmetic shift (ASR) for signed, logical shift (LSR) for unsigned
                    if ty.is_signed() {
                        if ty.is_64_bit() {
                            self.mir.push(Aarch64Inst::Asr64Imm {
                                dst: Operand::Virtual(check_vreg),
                                src: Operand::Virtual(vreg),
                                imm: shift,
                            });
                        } else {
                            self.mir.push(Aarch64Inst::Asr32Imm {
                                dst: Operand::Virtual(check_vreg),
                                src: Operand::Virtual(vreg),
                                imm: shift,
                            });
                        }
                    } else if ty.is_64_bit() {
                        self.mir.push(Aarch64Inst::Lsr64Imm {
                            dst: Operand::Virtual(check_vreg),
                            src: Operand::Virtual(vreg),
                            imm: shift,
                        });
                    } else {
                        self.mir.push(Aarch64Inst::Lsr32Imm {
                            dst: Operand::Virtual(check_vreg),
                            src: Operand::Virtual(vreg),
                            imm: shift,
                        });
                    }

                    // Compare with original value
                    let ok_label = self.mir.alloc_label();
                    if ty.is_64_bit() {
                        self.mir.push(Aarch64Inst::Cmp64RR {
                            src1: Operand::Virtual(check_vreg),
                            src2: Operand::Virtual(src_vreg),
                        });
                    } else {
                        self.mir.push(Aarch64Inst::CmpRR {
                            src1: Operand::Virtual(check_vreg),
                            src2: Operand::Virtual(src_vreg),
                        });
                    }

                    // Branch if equal (no overflow)
                    self.mir.push(Aarch64Inst::BCond {
                        cond: Cond::Eq,
                        label: ok_label,
                    });

                    // Overflow - call panic handler
                    let symbol_id = self.intern_symbol("__rue_overflow");
                    self.mir.push(Aarch64Inst::Bl { symbol_id });
                    self.mir.push(Aarch64Inst::Label { id: ok_label });
                } else {
                    // Fall back to regular multiply for non-power-of-2 constants
                    let rhs_vreg = self.get_vreg(*rhs);

                    // Overflow check for multiplication
                    self.emit_overflow_check_mul(ty, vreg, lhs_vreg, rhs_vreg);
                }
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
                let symbol_id = self.intern_symbol("__rue_div_by_zero");
                self.mir.push(Aarch64Inst::Bl { symbol_id });
                self.mir.push(Aarch64Inst::Label { id: ok_label });

                // Signed MIN / -1 overflows; SDIV silently wraps per the ARM
                // architecture, so trap it explicitly (RUE-30).
                if ty.is_signed() {
                    self.emit_signed_div_overflow_check(ty, lhs_vreg, rhs_vreg);
                }

                // Use SDIV for signed types, UDIV for unsigned types, selecting
                // 64-bit (X-register) forms for 64-bit operands — the 32-bit
                // (W-register) forms only divide the low 32 bits (RUE-26).
                let is_64 = ty.is_64_bit();
                if ty.is_signed() {
                    self.mir.push(if is_64 {
                        Aarch64Inst::Sdiv64RR {
                            dst: Operand::Virtual(vreg),
                            src1: Operand::Virtual(lhs_vreg),
                            src2: Operand::Virtual(rhs_vreg),
                        }
                    } else {
                        Aarch64Inst::SdivRR {
                            dst: Operand::Virtual(vreg),
                            src1: Operand::Virtual(lhs_vreg),
                            src2: Operand::Virtual(rhs_vreg),
                        }
                    });
                } else {
                    self.mir.push(if is_64 {
                        Aarch64Inst::Udiv64RR {
                            dst: Operand::Virtual(vreg),
                            src1: Operand::Virtual(lhs_vreg),
                            src2: Operand::Virtual(rhs_vreg),
                        }
                    } else {
                        Aarch64Inst::UdivRR {
                            dst: Operand::Virtual(vreg),
                            src1: Operand::Virtual(lhs_vreg),
                            src2: Operand::Virtual(rhs_vreg),
                        }
                    });
                }
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
                let symbol_id = self.intern_symbol("__rue_div_by_zero");
                self.mir.push(Aarch64Inst::Bl { symbol_id });
                self.mir.push(Aarch64Inst::Label { id: ok_label });

                // Signed MIN % -1 overflows like MIN / -1 (the implied
                // quotient -MIN is unrepresentable); SDIV silently wraps per
                // the ARM architecture, so trap it explicitly (RUE-30).
                if ty.is_signed() {
                    self.emit_signed_div_overflow_check(ty, lhs_vreg, rhs_vreg);
                }

                // Compute quotient first using SDIV or UDIV based on signedness,
                // selecting 64-bit forms for 64-bit operands (RUE-26).
                let is_64 = ty.is_64_bit();
                let quot_vreg = self.mir.alloc_vreg();
                if ty.is_signed() {
                    self.mir.push(if is_64 {
                        Aarch64Inst::Sdiv64RR {
                            dst: Operand::Virtual(quot_vreg),
                            src1: Operand::Virtual(lhs_vreg),
                            src2: Operand::Virtual(rhs_vreg),
                        }
                    } else {
                        Aarch64Inst::SdivRR {
                            dst: Operand::Virtual(quot_vreg),
                            src1: Operand::Virtual(lhs_vreg),
                            src2: Operand::Virtual(rhs_vreg),
                        }
                    });
                } else {
                    self.mir.push(if is_64 {
                        Aarch64Inst::Udiv64RR {
                            dst: Operand::Virtual(quot_vreg),
                            src1: Operand::Virtual(lhs_vreg),
                            src2: Operand::Virtual(rhs_vreg),
                        }
                    } else {
                        Aarch64Inst::UdivRR {
                            dst: Operand::Virtual(quot_vreg),
                            src1: Operand::Virtual(lhs_vreg),
                            src2: Operand::Virtual(rhs_vreg),
                        }
                    });
                }

                // rem = dividend - (quotient * divisor)
                self.mir.push(if is_64 {
                    Aarch64Inst::Msub64 {
                        dst: Operand::Virtual(vreg),
                        src1: Operand::Virtual(quot_vreg),
                        src2: Operand::Virtual(rhs_vreg),
                        src3: Operand::Virtual(lhs_vreg),
                    }
                } else {
                    Aarch64Inst::Msub {
                        dst: Operand::Virtual(vreg),
                        src1: Operand::Virtual(quot_vreg),
                        src2: Operand::Virtual(rhs_vreg),
                        src3: Operand::Virtual(lhs_vreg),
                    }
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
                let lhs_ty = self.ctx.cfg.get_inst(*lhs).ty;

                if self.ctx.is_builtin_string(lhs_ty) {
                    // String equality: call __rue_str_eq(ptr1, len1, ptr2, len2)
                    let vreg = self.emit_string_eq_call(*lhs, *rhs);
                    self.value_map.insert(value, vreg);
                } else if lhs_ty == Type::UNIT {
                    // Unit equality: () == () is always true
                    let vreg = self.mir.alloc_vreg();
                    self.value_map.insert(value, vreg);
                    self.mir.push(Aarch64Inst::MovImm {
                        dst: Operand::Virtual(vreg),
                        imm: 1,
                    });
                } else if let Some(struct_id) = lhs_ty.as_struct() {
                    // Struct equality: compare all fields
                    self.emit_struct_equality(value, *lhs, *rhs, struct_id, false);
                } else {
                    self.emit_comparison(value, *lhs, *rhs, Cond::Eq);
                }
            }

            CfgInstData::Ne(lhs, rhs) => {
                let lhs_ty = self.ctx.cfg.get_inst(*lhs).ty;

                if self.ctx.is_builtin_string(lhs_ty) {
                    // String inequality: call __rue_str_eq and invert result
                    let vreg = self.emit_string_eq_call(*lhs, *rhs);
                    self.value_map.insert(value, vreg);
                    // Invert result: 0 -> 1, 1 -> 0
                    self.mir.push(Aarch64Inst::EorImm {
                        dst: Operand::Virtual(vreg),
                        src: Operand::Virtual(vreg),
                        imm: 1,
                    });
                } else if lhs_ty == Type::UNIT {
                    // Unit inequality: () != () is always false
                    let vreg = self.mir.alloc_vreg();
                    self.value_map.insert(value, vreg);
                    self.mir.push(Aarch64Inst::MovImm {
                        dst: Operand::Virtual(vreg),
                        imm: 0,
                    });
                } else if let Some(struct_id) = lhs_ty.as_struct() {
                    // Struct inequality: compare all fields, invert result
                    self.emit_struct_equality(value, *lhs, *rhs, struct_id, true);
                } else {
                    self.emit_comparison(value, *lhs, *rhs, Cond::Ne);
                }
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

            CfgInstData::BitNot(operand) => {
                let vreg = self.mir.alloc_vreg();
                self.value_map.insert(value, vreg);

                let operand_vreg = self.get_vreg(*operand);

                // Sub-64-bit operands need the 32-bit w-form so the result
                // is zero-extended above the operand width; the 64-bit mvn
                // would set the upper 32 bits (wrong for u32, whose consumers
                // assume zero-extended registers) (RUE-59).
                self.mir.push(if ty.is_64_bit() {
                    Aarch64Inst::MvnRR {
                        dst: Operand::Virtual(vreg),
                        src: Operand::Virtual(operand_vreg),
                    }
                } else {
                    Aarch64Inst::Mvn32RR {
                        dst: Operand::Virtual(vreg),
                        src: Operand::Virtual(operand_vreg),
                    }
                });
                // MVN flips all 32 w-register bits, setting bits above a
                // sub-word operand's width (e.g. ~0u8 leaves 0xFFFFFFFF, not
                // 0xFF); narrow back to the operand's type (RUE-162).
                self.emit_subword_narrow(vreg, ty);
            }

            CfgInstData::BitAnd(lhs, rhs) => {
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

            CfgInstData::BitOr(lhs, rhs) => {
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

            CfgInstData::BitXor(lhs, rhs) => {
                let vreg = self.mir.alloc_vreg();
                self.value_map.insert(value, vreg);

                let lhs_vreg = self.get_vreg(*lhs);
                let rhs_vreg = self.get_vreg(*rhs);

                self.mir.push(Aarch64Inst::EorRR {
                    dst: Operand::Virtual(vreg),
                    src1: Operand::Virtual(lhs_vreg),
                    src2: Operand::Virtual(rhs_vreg),
                });
            }

            CfgInstData::Shl(lhs, rhs) => {
                let vreg = self.mir.alloc_vreg();
                self.value_map.insert(value, vreg);

                let lhs_vreg = self.get_vreg(*lhs);

                // Shift count taken modulo the operand bit width (spec 4.3a:10);
                // sub-word counts need an explicit mask (RUE-29).
                let count_mask = Self::shift_count_mask(ty);
                let rhs_inst = &self.ctx.cfg.get_inst(*rhs).data;
                if let CfgInstData::Const(shift_amount) = rhs_inst {
                    let imm = (*shift_amount & count_mask) as u8;
                    if ty.is_64_bit() {
                        self.mir.push(Aarch64Inst::LslImm {
                            dst: Operand::Virtual(vreg),
                            src: Operand::Virtual(lhs_vreg),
                            imm,
                        });
                    } else {
                        self.mir.push(Aarch64Inst::Lsl32Imm {
                            dst: Operand::Virtual(vreg),
                            src: Operand::Virtual(lhs_vreg),
                            imm,
                        });
                    }
                } else {
                    // Variable shift amount - mask it (mod bit width).
                    let count_vreg = self.emit_masked_shift_count(*rhs, ty);
                    if ty.is_64_bit() {
                        self.mir.push(Aarch64Inst::LslRR {
                            dst: Operand::Virtual(vreg),
                            src1: Operand::Virtual(lhs_vreg),
                            src2: Operand::Virtual(count_vreg),
                        });
                    } else {
                        self.mir.push(Aarch64Inst::Lsl32RR {
                            dst: Operand::Virtual(vreg),
                            src1: Operand::Virtual(lhs_vreg),
                            src2: Operand::Virtual(count_vreg),
                        });
                    }
                }
                // Left shift can set bits above the operand width; narrow the
                // result back to the sub-word type (RUE-29).
                self.emit_subword_narrow(vreg, ty);
            }

            CfgInstData::Shr(lhs, rhs) => {
                let vreg = self.mir.alloc_vreg();
                self.value_map.insert(value, vreg);

                let lhs_vreg = self.get_vreg(*lhs);

                // Shift count taken modulo the operand bit width (spec 4.3a:10);
                // sub-word counts need an explicit mask (RUE-29).
                let count_mask = Self::shift_count_mask(ty);
                let rhs_inst = &self.ctx.cfg.get_inst(*rhs).data;
                if let CfgInstData::Const(shift_amount) = rhs_inst {
                    let imm = (*shift_amount & count_mask) as u8;
                    // Use arithmetic shift (ASR) for signed types, logical shift (LSR) for unsigned
                    if ty.is_64_bit() && ty.is_signed() {
                        self.mir.push(Aarch64Inst::Asr64Imm {
                            dst: Operand::Virtual(vreg),
                            src: Operand::Virtual(lhs_vreg),
                            imm,
                        });
                    } else if ty.is_64_bit() {
                        self.mir.push(Aarch64Inst::Lsr64Imm {
                            dst: Operand::Virtual(vreg),
                            src: Operand::Virtual(lhs_vreg),
                            imm,
                        });
                    } else if ty.is_signed() {
                        self.mir.push(Aarch64Inst::Asr32Imm {
                            dst: Operand::Virtual(vreg),
                            src: Operand::Virtual(lhs_vreg),
                            imm,
                        });
                    } else {
                        self.mir.push(Aarch64Inst::Lsr32Imm {
                            dst: Operand::Virtual(vreg),
                            src: Operand::Virtual(lhs_vreg),
                            imm,
                        });
                    }
                } else {
                    // Variable shift amount - mask it (mod bit width).
                    let count_vreg = self.emit_masked_shift_count(*rhs, ty);
                    if ty.is_64_bit() && ty.is_signed() {
                        self.mir.push(Aarch64Inst::AsrRR {
                            dst: Operand::Virtual(vreg),
                            src1: Operand::Virtual(lhs_vreg),
                            src2: Operand::Virtual(count_vreg),
                        });
                    } else if ty.is_64_bit() {
                        self.mir.push(Aarch64Inst::LsrRR {
                            dst: Operand::Virtual(vreg),
                            src1: Operand::Virtual(lhs_vreg),
                            src2: Operand::Virtual(count_vreg),
                        });
                    } else if ty.is_signed() {
                        self.mir.push(Aarch64Inst::Asr32RR {
                            dst: Operand::Virtual(vreg),
                            src1: Operand::Virtual(lhs_vreg),
                            src2: Operand::Virtual(count_vreg),
                        });
                    } else {
                        self.mir.push(Aarch64Inst::Lsr32RR {
                            dst: Operand::Virtual(vreg),
                            src1: Operand::Virtual(lhs_vreg),
                            src2: Operand::Virtual(count_vreg),
                        });
                    }
                }
            }

            CfgInstData::Alloc { slot, init } => {
                let init_type = self.ctx.cfg.get_inst(*init).ty;
                if init_type.is_array() {
                    // Array: store all element slots. Try the accessor first —
                    // it materializes lazily-sourced arrays (Load/Param/
                    // PlaceRead, including array-typed struct fields `s.arr`
                    // and indexed reads `m[i]`, RUE-188) and cache-hits eager
                    // ones. Fall back to the recursive flattener for ArrayInit.
                    let scalar_vregs = self
                        .get_or_compute_field_vregs(*init)
                        .unwrap_or_else(|| self.collect_array_scalar_vregs(*init));
                    crate::agg_slots::store_slots(self, &scalar_vregs, *slot);
                } else if self.ctx.is_multislot_aggregate(init_type)
                    && !self.ctx.is_builtin_string(init_type)
                {
                    // Struct or payload enum (RUE-221): store all slots via the
                    // single accessor. It materializes a struct read from a place
                    // (`let q = a.p`) by loading its slot_count consecutive slots,
                    // and cache-hits StructInit/EnumVariant/Load/Param/Call/
                    // BlockParam — all of which carry fully-flattened slot lists.
                    // Fall back to the flattener for any source the accessor
                    // doesn't model. (RUE-118 / RUE-22)
                    // (Builtin String is excluded above — it takes the fat-pointer branch.)
                    let scalar_vregs = self
                        .get_or_compute_field_vregs(*init)
                        .unwrap_or_else(|| self.collect_struct_scalar_vregs(*init));
                    crate::agg_slots::store_slots(self, &scalar_vregs, *slot);
                } else if self.ctx.is_builtin_string(init_type) {
                    // String: store ptr, len, and cap to consecutive slots
                    let field_vregs = self
                        .get_or_compute_field_vregs(*init)
                        .expect("string should have fat pointer fields in Alloc");
                    debug_assert_eq!(
                        field_vregs.len(),
                        3,
                        "string should have 3 fields (ptr, len, cap)"
                    );
                    crate::agg_slots::store_slots(self, &field_vregs, *slot);
                } else {
                    let init_vreg = self.get_vreg(*init);
                    let offset = self.ctx.local_offset(*slot);
                    self.mir.push(Aarch64Inst::Str {
                        src: Operand::Virtual(init_vreg),
                        base: Reg::Fp,
                        offset,
                    });
                }
            }

            CfgInstData::Load { slot } => {
                let load_type = self.ctx.cfg.get_inst(value).ty;

                if self.ctx.is_builtin_string(load_type) {
                    // String: load ptr, len, and cap from consecutive slots
                    let ptr_vreg = self.mir.alloc_vreg();
                    let len_vreg = self.mir.alloc_vreg();
                    let cap_vreg = self.mir.alloc_vreg();

                    // Load ptr from slot
                    let ptr_offset = self.ctx.local_offset(*slot);
                    self.mir.push(Aarch64Inst::Ldr {
                        dst: Operand::Virtual(ptr_vreg),
                        base: Reg::Fp,
                        offset: ptr_offset,
                    });

                    // Load len from slot + 1
                    let len_offset = self.ctx.local_offset(slot + 1);
                    self.mir.push(Aarch64Inst::Ldr {
                        dst: Operand::Virtual(len_vreg),
                        base: Reg::Fp,
                        offset: len_offset,
                    });

                    // Load cap from slot + 2
                    let cap_offset = self.ctx.local_offset(slot + 2);
                    self.mir.push(Aarch64Inst::Ldr {
                        dst: Operand::Virtual(cap_vreg),
                        base: Reg::Fp,
                        offset: cap_offset,
                    });

                    // Register String fields (ptr, len, cap)
                    self.struct_slot_vregs
                        .insert(value, vec![ptr_vreg, len_vreg, cap_vreg]);
                    self.value_map.insert(value, ptr_vreg);
                } else if load_type.is_array() {
                    // Array: load all element slots (recursively flattened)
                    let slot_count = self.ctx.type_slot_count(load_type);
                    let mut slot_vregs = Vec::with_capacity(slot_count as usize);

                    for i in 0..slot_count {
                        let elem_vreg = self.mir.alloc_vreg();
                        let elem_offset = self.ctx.local_offset(slot + i);
                        self.mir.push(Aarch64Inst::Ldr {
                            dst: Operand::Virtual(elem_vreg),
                            base: Reg::Fp,
                            offset: elem_offset,
                        });
                        slot_vregs.push(elem_vreg);
                    }

                    // Register array element vregs
                    self.struct_slot_vregs.insert(value, slot_vregs.clone());

                    // Use first element as the primary vreg
                    if let Some(&first_vreg) = slot_vregs.first() {
                        self.value_map.insert(value, first_vreg);
                    } else {
                        let vreg = self.mir.alloc_vreg();
                        self.value_map.insert(value, vreg);
                    }
                } else if self.ctx.is_multislot_aggregate(load_type) {
                    // Struct or payload enum (RUE-221): load all slots. For an
                    // enum, slot 0 is the discriminant (what a `match`
                    // switches on).
                    let slot_count = self.ctx.type_slot_count(load_type);
                    let mut slot_vregs = Vec::with_capacity(slot_count as usize);

                    for i in 0..slot_count {
                        let field_vreg = self.mir.alloc_vreg();
                        let field_offset = self.ctx.local_offset(slot + i);
                        self.mir.push(Aarch64Inst::Ldr {
                            dst: Operand::Virtual(field_vreg),
                            base: Reg::Fp,
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

                    let offset = self.ctx.local_offset(*slot);
                    self.mir.push(Aarch64Inst::Ldr {
                        dst: Operand::Virtual(vreg),
                        base: Reg::Fp,
                        offset,
                    });
                }
            }

            CfgInstData::Store { slot, value: val } => {
                let val_type = self.ctx.cfg.get_inst(*val).ty;
                // Multi-slot aggregate (struct, builtin String fat pointer, or array):
                // store ALL slots, not just the first. The accessor materializes
                // lazily-sourced values and cache-hits eager ones; if it can't model
                // the source (e.g. a dynamically-indexed place-read) fall back to the
                // old single-slot behavior rather than ICE. (RUE-118, RUE-80)
                let aggregate_vregs = if val_type.is_struct() || val_type.is_array() {
                    self.get_or_compute_field_vregs(*val)
                } else {
                    None
                };

                if let Some(slot_vregs) = aggregate_vregs {
                    // Check if this slot corresponds to an inout parameter
                    if let Some(param_index) = self.slot_to_inout_param_index(*slot) {
                        // Whole-aggregate store through the inout pointer. Caller
                        // slots descend from the pointer (stack grows down), so
                        // slot i lives at ptr - i*8 — matching the place-read path.
                        let ptr_vreg = self.ensure_inout_param_ptr(param_index);
                        crate::agg_slots::store_slots_through_ptr(self, &slot_vregs, ptr_vreg, 0);
                    } else {
                        crate::agg_slots::store_slots(self, &slot_vregs, *slot);
                    }
                } else {
                    let val_vreg = self.get_vreg(*val);

                    // Check if this slot corresponds to an inout parameter
                    if let Some(param_index) = self.slot_to_inout_param_index(*slot) {
                        // For inout params, store through the pointer
                        // Use ensure_inout_param_ptr in case the param was never accessed via Param instruction
                        let ptr_vreg = self.ensure_inout_param_ptr(param_index);
                        self.mir.push(Aarch64Inst::StrIndexed {
                            src: Operand::Virtual(val_vreg),
                            base: ptr_vreg,
                        });
                    } else {
                        // Normal local variable: store to stack slot
                        let offset = self.ctx.local_offset(*slot);
                        self.mir.push(Aarch64Inst::Str {
                            src: Operand::Virtual(val_vreg),
                            base: Reg::Fp,
                            offset,
                        });
                    }
                }
            }

            CfgInstData::ParamStore {
                param_slot,
                value: val,
            } => {
                // ParamStore is used for inout params - store through the pointer.
                //
                // For inout params, param_slot is the first ABI slot for that param.
                // For scalar params, param_slot = param_index.
                // For struct params, param_slot is the first slot (same as param_index for first param).
                if !self.ctx.cfg.is_param_inout(*param_slot) {
                    panic!("ParamStore used on non-inout param slot {}", param_slot);
                }

                // Whole-value reassignment of a multi-slot aggregate (struct,
                // builtin String, or array) through inout must write ALL slots
                // through the pointer, not just the first — same shape as the
                // aggregate branch of Store above. Caller slots descend from
                // the pointer (stack grows down), so slot i lives at ptr - i*8.
                // Unmodeled sources fall back to single-slot. (RUE-145)
                let val_type = self.ctx.cfg.get_inst(*val).ty;
                let aggregate_vregs = if val_type.is_struct() || val_type.is_array() {
                    self.get_or_compute_field_vregs(*val)
                } else {
                    None
                };

                // Use ensure_inout_param_ptr in case the param was never accessed via Param instruction
                let ptr_vreg = self.ensure_inout_param_ptr(*param_slot);
                if let Some(slot_vregs) = aggregate_vregs {
                    crate::agg_slots::store_slots_through_ptr(self, &slot_vregs, ptr_vreg, 0);
                } else {
                    let val_vreg = self.get_vreg(*val);
                    self.mir.push(Aarch64Inst::StrIndexed {
                        src: Operand::Virtual(val_vreg),
                        base: ptr_vreg,
                    });
                }
            }

            CfgInstData::Call {
                name,
                args_start,
                args_len,
            } => {
                let result_vreg = self.mir.alloc_vreg();
                self.value_map.insert(value, result_vreg);

                // Does this call return via the sret convention? Builtin
                // String always does (runtime fns take an out-pointer first
                // param); other aggregates do when their slots exceed the
                // return registers. See crate::cfg_lower::type_uses_sret_return.
                let is_sret_call = self.ctx.type_uses_sret(ty, RET_REGS.len() as u32);
                let ret_slot_count = if self.ctx.is_multislot_aggregate(ty) {
                    self.ctx.type_slot_count(ty)
                } else {
                    1
                };
                // Caller-allocated return buffer: one 8-byte slot per
                // aggregate slot, padded to keep sp 16-byte aligned.
                let sret_bytes = ((ret_slot_count as i32 * 8) + 15) / 16 * 16;

                // For sret calls, allocate the return buffer on the stack;
                // we'll pass a pointer to this space as the first argument.
                if is_sret_call {
                    self.mir.push(Aarch64Inst::SubImm {
                        dst: Operand::Physical(Reg::Sp),
                        src: Operand::Physical(Reg::Sp),
                        imm: sret_bytes,
                    });
                }

                // Flatten struct arguments and handle by-ref arguments (inout and borrow)
                let mut flattened_vregs: Vec<VReg> = Vec::new();

                // For sret calls, the first argument is the output pointer (current sp)
                if is_sret_call {
                    let sret_ptr_vreg = self.mir.alloc_vreg();
                    self.mir.push(Aarch64Inst::MovRR {
                        dst: Operand::Virtual(sret_ptr_vreg),
                        src: Operand::Physical(Reg::Sp),
                    });
                    flattened_vregs.push(sret_ptr_vreg);
                }
                let args = self.ctx.cfg.get_call_args(*args_start, *args_len).to_vec();
                for arg in &args {
                    let arg_value = arg.value;
                    let arg_type = self.ctx.cfg.get_inst(arg_value).ty;

                    // For by-ref args (inout or borrow), pass the address of
                    // the argument place — a variable, field, or array
                    // element (RUE-143). Single shared implementation — see
                    // crate::byref_args.
                    if arg.is_by_ref() {
                        let addr_vreg = crate::byref_args::lower_byref_arg_addr(self, arg_value);
                        flattened_vregs.push(addr_vreg);
                        continue;
                    }

                    if self.ctx.is_multislot_aggregate(arg_type) {
                        // Pass all slots of the (possibly multi-slot) aggregate arg —
                        // struct, builtin String, fixed-size array, or payload enum
                        // (RUE-221) — regardless of how it was produced. The accessor
                        // materializes lazily-sourced values (Load/Param/PlaceRead) and
                        // cache-hits eager ones (StructInit/ArrayInit/Call/BlockParam).
                        if let Some(field_vregs) = self.get_or_compute_field_vregs(arg_value) {
                            flattened_vregs.extend(field_vregs);
                        } else {
                            flattened_vregs.push(self.get_vreg(arg_value));
                        }
                    } else {
                        flattened_vregs.push(self.get_vreg(arg_value));
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
                let symbol_name = self.interner.resolve(name);
                let symbol_id = self.intern_symbol(symbol_name);
                self.mir.push(Aarch64Inst::Bl { symbol_id });

                // Clean up stack space after call
                if stack_space > 0 {
                    self.mir.push(Aarch64Inst::AddImm {
                        dst: Operand::Physical(Reg::Sp),
                        src: Operand::Physical(Reg::Sp),
                        imm: stack_space as i32,
                    });
                }

                // Handle struct and string returns (multi-slot types)
                // sret calls first: the callee wrote the slots to the buffer
                // at [sp]. (String always; big aggregates per RUE-106.)
                if is_sret_call {
                    // Load every slot from the return buffer
                    let mut slot_vregs = Vec::new();
                    for slot_idx in 0..ret_slot_count {
                        let slot_vreg = self.mir.alloc_vreg();
                        let offset = (slot_idx * 8) as i32;
                        self.mir.push(Aarch64Inst::Ldr {
                            dst: Operand::Virtual(slot_vreg),
                            base: Reg::Sp,
                            offset,
                        });
                        slot_vregs.push(slot_vreg);
                    }
                    // Pop the sret space (including alignment padding)
                    self.mir.push(Aarch64Inst::AddImm {
                        dst: Operand::Physical(Reg::Sp),
                        src: Operand::Physical(Reg::Sp),
                        imm: sret_bytes,
                    });
                    self.struct_slot_vregs.insert(value, slot_vregs.clone());
                    self.mir.push(Aarch64Inst::MovRR {
                        dst: Operand::Virtual(result_vreg),
                        src: Operand::Virtual(slot_vregs[0]),
                    });
                } else if self.ctx.is_multislot_aggregate(ty) {
                    // Aggregates that fit return in registers. Capture
                    // every slot from RET_REGS, not just x0 — arrays previously fell
                    // to the scalar path and lost all elements but the first (RUE-78),
                    // and payload enums fell there too, leaving the call result with
                    // no slot vregs and ICEing the enum-payload read (RUE-237).
                    let slot_count = ret_slot_count;
                    let mut slot_vregs = Vec::new();
                    for slot_idx in 0..slot_count {
                        let slot_vreg = self.mir.alloc_vreg();
                        if (slot_idx as usize) < RET_REGS.len() {
                            self.mir.push(Aarch64Inst::MovRR {
                                dst: Operand::Virtual(slot_vreg),
                                src: Operand::Physical(RET_REGS[slot_idx as usize]),
                            });
                        }
                        slot_vregs.push(slot_vreg);
                    }
                    self.struct_slot_vregs.insert(value, slot_vregs.clone());
                    if let Some(&first_vreg) = slot_vregs.first() {
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

            CfgInstData::Intrinsic {
                name,
                args_start,
                args_len,
            } => {
                let name_str = self.interner.resolve(name);
                if name_str == "read_line" {
                    // @read_line() intrinsic - reads a line from stdin and returns String.
                    // Uses sret convention: allocate space on stack for the result (ptr, len, cap).

                    // Allocate 32 bytes on stack for sret (24 bytes for String + 8 for alignment)
                    self.mir.push(Aarch64Inst::SubImm {
                        dst: Operand::Physical(Reg::Sp),
                        src: Operand::Physical(Reg::Sp),
                        imm: 32,
                    });

                    // Move SP (pointer to sret space) to X0 as first argument
                    self.mir.push(Aarch64Inst::MovRR {
                        dst: Operand::Physical(Reg::X0),
                        src: Operand::Physical(Reg::Sp),
                    });

                    // Call __rue_read_line
                    let symbol_id = self.intern_symbol("__rue_read_line");
                    self.mir.push(Aarch64Inst::Bl { symbol_id });

                    // Load ptr, len, cap from stack into vregs
                    let mut slot_vregs = Vec::new();
                    for slot_idx in 0..3 {
                        let slot_vreg = self.mir.alloc_vreg();
                        let offset = (slot_idx * 8) as i32;
                        self.mir.push(Aarch64Inst::Ldr {
                            dst: Operand::Virtual(slot_vreg),
                            base: Reg::Sp,
                            offset,
                        });
                        slot_vregs.push(slot_vreg);
                    }

                    // Pop the sret space
                    self.mir.push(Aarch64Inst::AddImm {
                        dst: Operand::Physical(Reg::Sp),
                        src: Operand::Physical(Reg::Sp),
                        imm: 32,
                    });

                    // Store the slot vregs for the String value
                    self.struct_slot_vregs.insert(value, slot_vregs.clone());

                    // Create a result vreg (for the primary value representation)
                    let result_vreg = self.mir.alloc_vreg();
                    self.mir.push(Aarch64Inst::MovRR {
                        dst: Operand::Virtual(result_vreg),
                        src: Operand::Virtual(slot_vregs[0]),
                    });
                    self.value_map.insert(value, result_vreg);
                } else if name_str == "dbg" {
                    let args = self.ctx.cfg.get_extra(*args_start, *args_len);
                    let arg_val = args[0];
                    let arg_type = self.ctx.cfg.get_inst(arg_val).ty;

                    // Handle String arguments separately
                    if self.ctx.is_builtin_string(arg_type) {
                        // String fat pointer (ptr, len, cap) — materialize a String read
                        // from a place (`@dbg(h.s)`) as well as cached sources. (RUE-118)
                        if let Some(field_vregs) = self.get_or_compute_field_vregs(arg_val) {
                            let ptr_vreg = field_vregs[0];
                            let len_vreg = field_vregs[1];

                            // Move pointer to X0 and length to X1
                            self.mir.push(Aarch64Inst::MovRR {
                                dst: Operand::Physical(Reg::X0),
                                src: Operand::Virtual(ptr_vreg),
                            });
                            self.mir.push(Aarch64Inst::MovRR {
                                dst: Operand::Physical(Reg::X1),
                                src: Operand::Virtual(len_vreg),
                            });

                            // Call __rue_dbg_str
                            let symbol_id = self.intern_symbol("__rue_dbg_str");
                            self.mir.push(Aarch64Inst::Bl { symbol_id });
                        } else {
                            unreachable!("String fat pointer not found in struct_slot_vregs");
                        }

                        let result_vreg = self.mir.alloc_vreg();
                        self.value_map.insert(value, result_vreg);
                    } else {
                        // Handle scalar types (integers and bool)
                        let arg_vreg = self.get_vreg(arg_val);

                        let runtime_fn = match arg_type.kind() {
                            TypeKind::Bool => "__rue_dbg_bool",
                            TypeKind::I8 | TypeKind::I16 | TypeKind::I32 | TypeKind::I64 => {
                                "__rue_dbg_i64"
                            }
                            TypeKind::U8 | TypeKind::U16 | TypeKind::U32 | TypeKind::U64 => {
                                "__rue_dbg_u64"
                            }
                            _ => unreachable!("@dbg only supports scalars and strings"),
                        };

                        // Handle type extensions
                        match arg_type.kind() {
                            TypeKind::I8 => {
                                self.mir.push(Aarch64Inst::MovRR {
                                    dst: Operand::Physical(Reg::X0),
                                    src: Operand::Virtual(arg_vreg),
                                });
                                self.mir.push(Aarch64Inst::Sxtb {
                                    dst: Operand::Physical(Reg::X0),
                                    src: Operand::Physical(Reg::X0),
                                });
                            }
                            TypeKind::I16 => {
                                self.mir.push(Aarch64Inst::MovRR {
                                    dst: Operand::Physical(Reg::X0),
                                    src: Operand::Virtual(arg_vreg),
                                });
                                self.mir.push(Aarch64Inst::Sxth {
                                    dst: Operand::Physical(Reg::X0),
                                    src: Operand::Physical(Reg::X0),
                                });
                            }
                            TypeKind::I32 => {
                                self.mir.push(Aarch64Inst::MovRR {
                                    dst: Operand::Physical(Reg::X0),
                                    src: Operand::Virtual(arg_vreg),
                                });
                                self.mir.push(Aarch64Inst::Sxtw {
                                    dst: Operand::Physical(Reg::X0),
                                    src: Operand::Physical(Reg::X0),
                                });
                            }
                            TypeKind::U8 => {
                                self.mir.push(Aarch64Inst::MovRR {
                                    dst: Operand::Physical(Reg::X0),
                                    src: Operand::Virtual(arg_vreg),
                                });
                                self.mir.push(Aarch64Inst::Uxtb {
                                    dst: Operand::Physical(Reg::X0),
                                    src: Operand::Physical(Reg::X0),
                                });
                            }
                            TypeKind::U16 => {
                                self.mir.push(Aarch64Inst::MovRR {
                                    dst: Operand::Physical(Reg::X0),
                                    src: Operand::Virtual(arg_vreg),
                                });
                                self.mir.push(Aarch64Inst::Uxth {
                                    dst: Operand::Physical(Reg::X0),
                                    src: Operand::Physical(Reg::X0),
                                });
                            }
                            TypeKind::U32 | TypeKind::I64 | TypeKind::U64 | TypeKind::Bool => {
                                self.mir.push(Aarch64Inst::MovRR {
                                    dst: Operand::Physical(Reg::X0),
                                    src: Operand::Virtual(arg_vreg),
                                });
                            }
                            _ => unreachable!(),
                        }

                        let symbol_id = self.intern_symbol(runtime_fn);
                        self.mir.push(Aarch64Inst::Bl { symbol_id });

                        let result_vreg = self.mir.alloc_vreg();
                        self.value_map.insert(value, result_vreg);
                    }
                } else if name_str == "parse_i32"
                    || name_str == "parse_i64"
                    || name_str == "parse_u32"
                    || name_str == "parse_u64"
                {
                    // @parse_* intrinsics: take a String, return an integer
                    let args = self.ctx.cfg.get_extra(*args_start, *args_len);
                    let arg_val = args[0];

                    // Get the String fat pointer (ptr, len, cap) — materialize a String
                    // read from a place (`@parse_i32(h.s)`) as well as cached sources. (RUE-118)
                    if let Some(field_vregs) = self.get_or_compute_field_vregs(arg_val) {
                        let ptr_vreg = field_vregs[0];
                        let len_vreg = field_vregs[1];

                        // Move ptr to X0, len to X1
                        self.mir.push(Aarch64Inst::MovRR {
                            dst: Operand::Physical(Reg::X0),
                            src: Operand::Virtual(ptr_vreg),
                        });
                        self.mir.push(Aarch64Inst::MovRR {
                            dst: Operand::Physical(Reg::X1),
                            src: Operand::Virtual(len_vreg),
                        });

                        // Determine the runtime function based on intrinsic name
                        let runtime_fn = match name_str {
                            "parse_i32" => "__rue_parse_i32",
                            "parse_i64" => "__rue_parse_i64",
                            "parse_u32" => "__rue_parse_u32",
                            "parse_u64" => "__rue_parse_u64",
                            _ => unreachable!(),
                        };

                        // Call the runtime function
                        let symbol_id = self.intern_symbol(runtime_fn);
                        self.mir.push(Aarch64Inst::Bl { symbol_id });

                        // Result is in X0, move to a vreg
                        let result_vreg = self.mir.alloc_vreg();
                        self.mir.push(Aarch64Inst::MovRR {
                            dst: Operand::Virtual(result_vreg),
                            src: Operand::Physical(Reg::X0),
                        });
                        self.value_map.insert(value, result_vreg);
                    } else {
                        unreachable!("String fat pointer not found in struct_slot_vregs");
                    }
                } else if name_str == "random_u32" || name_str == "random_u64" {
                    // @random_u32() and @random_u64() intrinsics - generate random numbers
                    // These intrinsics take no arguments and return u32/u64 respectively
                    // Call __rue_random_u32 or __rue_random_u64 from the runtime

                    let runtime_fn = match name_str {
                        "random_u32" => "__rue_random_u32",
                        "random_u64" => "__rue_random_u64",
                        _ => unreachable!(),
                    };

                    // Call the runtime function (no arguments)
                    let symbol_id = self.intern_symbol(runtime_fn);
                    self.mir.push(Aarch64Inst::Bl { symbol_id });

                    // Result is in X0, move to a vreg
                    let result_vreg = self.mir.alloc_vreg();
                    self.mir.push(Aarch64Inst::MovRR {
                        dst: Operand::Virtual(result_vreg),
                        src: Operand::Physical(Reg::X0),
                    });
                    self.value_map.insert(value, result_vreg);
                } else if name_str == "syscall" {
                    // @syscall intrinsic - perform a direct system call
                    //
                    // ABI differs between Linux and macOS:
                    //
                    // Linux aarch64:
                    //   - X8: syscall number
                    //   - X0-X5: arguments 1-6
                    //   - Returns result in X0
                    //   - Uses SVC #0
                    //
                    // macOS aarch64:
                    //   - X16: syscall number
                    //   - X0-X5: arguments 1-6
                    //   - Returns result in X0
                    //   - Uses SVC #0x80
                    //
                    // IMPORTANT: We allocate temporary stack space to stage arguments
                    // before loading them into physical registers. This prevents the register
                    // allocator from reusing these registers between setup and the SVC instruction,
                    // which would break the syscall (especially the syscall number register).

                    let args = self.ctx.cfg.get_extra(*args_start, *args_len);

                    // Allocate stack space for all arguments (8 bytes each, 16-byte aligned)
                    let num_args = args.len();
                    let stack_space = ((num_args * 8 + 15) & !15) as i32; // Round up to 16-byte alignment
                    if stack_space > 0 {
                        self.mir.push(Aarch64Inst::SubImm {
                            dst: Operand::Physical(Reg::Sp),
                            src: Operand::Physical(Reg::Sp),
                            imm: stack_space,
                        });
                    }

                    // Store all arguments to the stack
                    for (i, &arg) in args.iter().enumerate() {
                        let arg_vreg = self.get_vreg(arg);
                        let offset = (i * 8) as i32;
                        self.mir.push(Aarch64Inst::Str {
                            src: Operand::Virtual(arg_vreg),
                            base: Reg::Sp,
                            offset,
                        });
                    }

                    // Load syscall number from stack into syscall number register
                    // Linux uses X8, macOS uses X16
                    let syscall_num_reg = if self.target.is_macho() {
                        Reg::X16
                    } else {
                        Reg::X8
                    };
                    self.mir.push(Aarch64Inst::Ldr {
                        dst: Operand::Physical(syscall_num_reg),
                        base: Reg::Sp,
                        offset: 0,
                    });

                    // Load remaining arguments from stack into X0-X5
                    for i in 1..num_args {
                        if i > 6 {
                            break; // Syscall can have at most 6 arguments (plus syscall number)
                        }
                        let target_reg = match i - 1 {
                            0 => Reg::X0,
                            1 => Reg::X1,
                            2 => Reg::X2,
                            3 => Reg::X3,
                            4 => Reg::X4,
                            5 => Reg::X5,
                            _ => unreachable!(),
                        };
                        let offset = (i * 8) as i32;
                        self.mir.push(Aarch64Inst::Ldr {
                            dst: Operand::Physical(target_reg),
                            base: Reg::Sp,
                            offset,
                        });
                    }

                    // Execute the syscall
                    // Linux uses SVC #0, macOS uses SVC #0x80
                    let svc_imm = if self.target.is_macho() { 0x80 } else { 0 };
                    self.mir.push(Aarch64Inst::Svc { imm: svc_imm });

                    // Clean up stack space
                    if stack_space > 0 {
                        self.mir.push(Aarch64Inst::AddImm {
                            dst: Operand::Physical(Reg::Sp),
                            src: Operand::Physical(Reg::Sp),
                            imm: stack_space,
                        });
                    }

                    // Result is in X0, move to a vreg
                    let result_vreg = self.mir.alloc_vreg();
                    self.mir.push(Aarch64Inst::MovRR {
                        dst: Operand::Virtual(result_vreg),
                        src: Operand::Physical(Reg::X0),
                    });
                    self.value_map.insert(value, result_vreg);
                } else if name_str == "ptr_read" {
                    // @ptr_read(ptr) - Read the pointee value through the pointer.
                    let args = self.ctx.cfg.get_extra(*args_start, *args_len);
                    let ptr_val = args[0];
                    let ptr_vreg = self.get_vreg(ptr_val);

                    // The result type is the pointee type. An aggregate pointee
                    // (struct/array/payload enum) occupies several 8-byte slots;
                    // read EVERY slot at its descending byte offset (slot i at
                    // ptr - i*8, matching how @raw addresses the place and the
                    // by-ref aggregate load path). Reading only slot 0 dropped
                    // every field but the first (RUE-242).
                    let result_ty = self.ctx.cfg.get_inst(value).ty;
                    let slot_count = self.ctx.type_slot_count(result_ty);
                    if slot_count > 1 {
                        let slot_vregs =
                            crate::agg_slots::load_slots_through_ptr(self, ptr_vreg, slot_count);
                        let first = slot_vregs[0];
                        self.struct_slot_vregs.insert(value, slot_vregs);
                        self.value_map.insert(value, first);
                    } else {
                        let result_vreg = self.mir.alloc_vreg();
                        self.mir.push(Aarch64Inst::LdrIndexed {
                            dst: Operand::Virtual(result_vreg),
                            base: ptr_vreg,
                        });
                        self.value_map.insert(value, result_vreg);
                    }
                } else if name_str == "ptr_write" {
                    // @ptr_write(ptr, value) - Write value at pointer
                    // First argument is pointer, second is value to write.
                    let args = self.ctx.cfg.get_extra(*args_start, *args_len);
                    let ptr_val = args[0];
                    let value_val = args[1];
                    let ptr_vreg = self.get_vreg(ptr_val);

                    // An aggregate value spans several 8-byte slots; store EVERY
                    // slot at its descending byte offset (slot i at ptr - i*8),
                    // symmetric with @ptr_read. Storing only slot 0 dropped
                    // every field but the first (RUE-242).
                    let value_ty = self.ctx.cfg.get_inst(value_val).ty;
                    let slot_count = self.ctx.type_slot_count(value_ty);
                    if slot_count > 1 {
                        let slot_vregs = self
                            .get_or_compute_field_vregs(value_val)
                            .unwrap_or_else(|| vec![self.get_vreg(value_val)]);
                        crate::agg_slots::store_slots_through_ptr(self, &slot_vregs, ptr_vreg, 0);
                    } else {
                        let value_vreg = self.get_vreg(value_val);
                        self.mir.push(Aarch64Inst::StrIndexed {
                            src: Operand::Virtual(value_vreg),
                            base: ptr_vreg,
                        });
                    }

                    // Result is unit (no meaningful value)
                    let result_vreg = self.mir.alloc_vreg();
                    self.value_map.insert(value, result_vreg);
                } else if name_str == "ptr_offset" {
                    // @ptr_offset(ptr, offset) - Pointer arithmetic
                    // Advances pointer by offset * sizeof(pointee)
                    let args = self.ctx.cfg.get_extra(*args_start, *args_len);
                    let ptr_val = args[0];
                    let offset_val = args[1];
                    let ptr_vreg = self.get_vreg(ptr_val);
                    let raw_offset_vreg = self.get_vreg(offset_val);

                    // Get the pointer type to determine element size
                    let ptr_type = self.ctx.cfg.get_inst(ptr_val).ty;
                    let pointee_type = match ptr_type.kind() {
                        TypeKind::PtrConst(ptr_id) => self.ctx.type_pool.ptr_const_def(ptr_id),
                        TypeKind::PtrMut(ptr_id) => self.ctx.type_pool.ptr_mut_def(ptr_id),
                        _ => unreachable!("ptr_offset requires pointer type"),
                    };
                    let element_size = types::type_size_bytes(self.ctx.type_pool, pointee_type);

                    // Sign-/zero-extend the index to a full 64-bit value before
                    // the 64-bit scale + subtract below. A narrow signed index
                    // (e.g. an i32 `-1`) is zero-extended into the X register by
                    // a W-write, so without an explicit sign-extend the 64-bit
                    // multiply would treat it as ~4 billion and corrupt the
                    // address (RUE-213).
                    let offset_ty = self.ctx.cfg.get_inst(offset_val).ty;
                    let offset_vreg = self.mir.alloc_vreg();
                    self.mir.push(Aarch64Inst::MovRR {
                        dst: Operand::Virtual(offset_vreg),
                        src: Operand::Virtual(raw_offset_vreg),
                    });
                    let ext_dst = Operand::Virtual(offset_vreg);
                    let ext_src = Operand::Virtual(offset_vreg);
                    match (Self::type_bits(offset_ty), offset_ty.is_signed()) {
                        (8, true) => self.mir.push(Aarch64Inst::Sxtb {
                            dst: ext_dst,
                            src: ext_src,
                        }),
                        (8, false) => self.mir.push(Aarch64Inst::Uxtb {
                            dst: ext_dst,
                            src: ext_src,
                        }),
                        (16, true) => self.mir.push(Aarch64Inst::Sxth {
                            dst: ext_dst,
                            src: ext_src,
                        }),
                        (16, false) => self.mir.push(Aarch64Inst::Uxth {
                            dst: ext_dst,
                            src: ext_src,
                        }),
                        (32, true) => self.mir.push(Aarch64Inst::Sxtw {
                            dst: ext_dst,
                            src: ext_src,
                        }),
                        // 32-bit unsigned already zero-extends; 64-bit is full width.
                        _ => {}
                    }

                    // Calculate: ptr - (offset * element_size)
                    // Aggregates (arrays) are laid out with element 0 at the
                    // highest address and later elements at lower addresses
                    // (the stack grows down), so array indexing subtracts
                    // index * 8 from the base. @ptr_offset must subtract the
                    // scaled offset too, so that advancing a pointer by N lands
                    // on element N rather than walking off the array (RUE-213).
                    // First, multiply offset by element size.
                    let scaled_offset_vreg = self.mir.alloc_vreg();
                    if element_size == 1 {
                        // No multiplication needed
                        self.mir.push(Aarch64Inst::MovRR {
                            dst: Operand::Virtual(scaled_offset_vreg),
                            src: Operand::Virtual(offset_vreg),
                        });
                    } else if element_size == 0 {
                        // Zero-sized type - offset is always 0
                        self.mir.push(Aarch64Inst::MovImm {
                            dst: Operand::Virtual(scaled_offset_vreg),
                            imm: 0,
                        });
                    } else {
                        // Multiply offset by element size
                        let size_vreg = self.mir.alloc_vreg();
                        self.mir.push(Aarch64Inst::MovImm {
                            dst: Operand::Virtual(size_vreg),
                            imm: element_size as i64,
                        });
                        // MUL dst, src1, src2 (dst = src1 * src2)
                        self.mir.push(Aarch64Inst::MulRR {
                            dst: Operand::Virtual(scaled_offset_vreg),
                            src1: Operand::Virtual(offset_vreg),
                            src2: Operand::Virtual(size_vreg),
                        });
                    }

                    // Subtract from pointer (64-bit sub for addresses); see the
                    // descending-layout note above (RUE-213).
                    let result_vreg = self.mir.alloc_vreg();
                    self.mir.push(Aarch64Inst::SubRR {
                        dst: Operand::Virtual(result_vreg),
                        src1: Operand::Virtual(ptr_vreg),
                        src2: Operand::Virtual(scaled_offset_vreg),
                    });
                    self.value_map.insert(value, result_vreg);
                } else if name_str == "ptr_to_int" {
                    // @ptr_to_int(ptr) - Convert pointer to u64
                    // On aarch64, pointers are already 64-bit values, so this is a simple move.
                    let args = self.ctx.cfg.get_extra(*args_start, *args_len);
                    let ptr_val = args[0];
                    let ptr_vreg = self.get_vreg(ptr_val);

                    let result_vreg = self.mir.alloc_vreg();
                    self.mir.push(Aarch64Inst::MovRR {
                        dst: Operand::Virtual(result_vreg),
                        src: Operand::Virtual(ptr_vreg),
                    });
                    self.value_map.insert(value, result_vreg);
                } else if name_str == "int_to_ptr" {
                    // @int_to_ptr(addr) - Convert u64 to pointer
                    // On aarch64, this is also a simple move.
                    let args = self.ctx.cfg.get_extra(*args_start, *args_len);
                    let addr_val = args[0];
                    let addr_vreg = self.get_vreg(addr_val);

                    let result_vreg = self.mir.alloc_vreg();
                    self.mir.push(Aarch64Inst::MovRR {
                        dst: Operand::Virtual(result_vreg),
                        src: Operand::Virtual(addr_vreg),
                    });
                    self.value_map.insert(value, result_vreg);
                } else if name_str == "raw" || name_str == "raw_mut" {
                    // @raw(lvalue) / @raw_mut(lvalue) - Take address of a value
                    // The argument should be a local variable, and we compute its stack address.
                    let args = self.ctx.cfg.get_extra(*args_start, *args_len);
                    let lvalue_val = args[0];

                    // Get the local slot for this value
                    let lvalue_inst = self.ctx.cfg.get_inst(lvalue_val);
                    if let CfgInstData::Load { slot } = &lvalue_inst.data {
                        // Simple case: address of a local variable
                        let offset = self.ctx.local_offset(*slot);
                        let result_vreg = self.mir.alloc_vreg();
                        // ADD to compute address: result = fp + offset
                        // Since offset is negative (locals below FP), we use AddImm
                        self.mir.push(Aarch64Inst::AddImm {
                            dst: Operand::Virtual(result_vreg),
                            src: Operand::Physical(Reg::Fp),
                            imm: offset,
                        });
                        self.value_map.insert(value, result_vreg);
                    } else if let CfgInstData::PlaceRead { place } = &lvalue_inst.data {
                        // Address of a place expression (array element, struct field, etc.)
                        // We compute the address without loading the value.
                        let result_vreg = self.mir.alloc_vreg();
                        self.lower_place_addr(result_vreg, place);
                        self.value_map.insert(value, result_vreg);
                    } else {
                        // For other lvalue types (Param, etc.), fall back to vreg
                        // This is a limitation that can be addressed later.
                        let vreg = self.get_vreg(lvalue_val);
                        self.value_map.insert(value, vreg);
                    }
                }
            }

            CfgInstData::StructInit {
                struct_id: _,
                fields_start,
                fields_len,
            } => {
                crate::agg_slots::lower_struct_init(self, value, *fields_start, *fields_len);
            }

            CfgInstData::FieldSet {
                slot,
                struct_id,
                field_index,
                value: val,
            } => {
                let val_vreg = self.get_vreg(*val);
                let field_slot_offset = self.ctx.struct_field_slot_offset(*struct_id, *field_index);
                let actual_slot = slot + field_slot_offset;
                let offset = self.ctx.local_offset(actual_slot);
                self.mir.push(Aarch64Inst::Str {
                    src: Operand::Virtual(val_vreg),
                    base: Reg::Fp,
                    offset,
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
                let field_slot_offset = self.ctx.struct_field_slot_offset(*struct_id, *field_index);
                let total_offset = *inner_offset + field_slot_offset;

                // Check if this is an inout parameter
                if self.ctx.cfg.is_param_inout(*param_slot) {
                    // For inout params, store through the pointer
                    // Use ensure_inout_param_ptr in case the param was never accessed via Param instruction
                    let ptr_vreg = self.ensure_inout_param_ptr(*param_slot);
                    // Negative offset because stack grows down
                    self.mir.push(Aarch64Inst::StrIndexedOffset {
                        src: Operand::Virtual(val_vreg),
                        base: ptr_vreg,
                        offset: -((total_offset as i32) * 8),
                    });
                } else {
                    // Non-inout param: struct is on our stack
                    let param_stack_slot = self.ctx.num_locals + *param_slot + total_offset;
                    let offset = self.ctx.local_offset(param_stack_slot);
                    self.mir.push(Aarch64Inst::Str {
                        src: Operand::Virtual(val_vreg),
                        base: Reg::Fp,
                        offset,
                    });
                }
            }

            CfgInstData::ArrayInit {
                elements_start,
                elements_len,
            } => {
                crate::agg_slots::lower_array_init(self, value, *elements_start, *elements_len);
            }

            CfgInstData::IndexSet {
                slot,
                array_type,
                index,
                value: val,
            } => {
                let val_vreg = self.get_vreg(*val);
                let index_vreg = self.get_vreg(*index);

                // Emit runtime bounds check
                let array_length = self.ctx.array_length(*array_type);
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
                let base_offset = self.ctx.local_offset(*slot);
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

            CfgInstData::ParamIndexSet {
                param_slot,
                array_type,
                index,
                value: val,
            } => {
                let val_vreg = self.get_vreg(*val);
                let index_vreg = self.get_vreg(*index);

                // Emit runtime bounds check
                let array_length = self.ctx.array_length(*array_type);
                self.emit_bounds_check(index_vreg, array_length);

                // Shift left by 3 (multiply by 8)
                let scaled_index = self.mir.alloc_vreg();
                self.mir.push(Aarch64Inst::LslImm {
                    dst: Operand::Virtual(scaled_index),
                    src: Operand::Virtual(index_vreg),
                    imm: 3,
                });

                // For inout params, store through the pointer
                // Use ensure_inout_param_ptr in case the param was never accessed via Param instruction
                let ptr_vreg = self.ensure_inout_param_ptr(*param_slot);
                // Calculate address: ptr - (index * 8)
                // (Arrays are stored with element 0 at the highest address)
                let addr_vreg = self.mir.alloc_vreg();
                self.mir.push(Aarch64Inst::SubRR {
                    dst: Operand::Virtual(addr_vreg),
                    src1: Operand::Virtual(ptr_vreg),
                    src2: Operand::Virtual(scaled_index),
                });

                self.mir.push(Aarch64Inst::StrIndexed {
                    src: Operand::Virtual(val_vreg),
                    base: addr_vreg,
                });
            }

            CfgInstData::EnumVariant {
                variant_index,
                payload_start,
                payload_len,
                ..
            } => {
                let vty = self.ctx.cfg.get_inst(value).ty;
                if *payload_len == 0 && self.ctx.type_slot_count(vty) <= 1 {
                    // Discriminant-only (C-like) enum: single-slot scalar.
                    let vreg = self.mir.alloc_vreg();
                    self.value_map.insert(value, vreg);
                    self.mir.push(Aarch64Inst::MovImm {
                        dst: Operand::Virtual(vreg),
                        imm: *variant_index as i64,
                    });
                } else {
                    // Tagged-union value (RUE-221): discriminant + payload slots.
                    let (ps, pl) = (*payload_start, *payload_len);
                    crate::agg_slots::lower_enum_variant(self, value, ps, pl);
                }
            }

            CfgInstData::EnumPayloadGet {
                base,
                enum_id,
                variant_index,
                field_index,
            } => {
                // Read payload field from the scrutinee's tagged-union slots.
                let (enum_id, variant_index, field_index) =
                    (*enum_id, *variant_index, *field_index);
                let base = *base;
                let vty = self.ctx.cfg.get_inst(value).ty;
                let field_slots = self.ctx.type_slot_count(vty) as usize;
                let offset = types::enum_payload_slot_offset(
                    self.ctx.type_pool,
                    enum_id,
                    variant_index,
                    field_index,
                ) as usize;
                let base_slots = self
                    .get_or_compute_field_vregs(base)
                    .expect("enum payload base must have slot vregs");
                let vreg = self.mir.alloc_vreg();
                self.value_map.insert(value, vreg);
                if field_slots > 1 {
                    let slots: Vec<VReg> = base_slots[offset..offset + field_slots].to_vec();
                    self.struct_slot_vregs.insert(value, slots.clone());
                    self.mir.push(Aarch64Inst::MovRR {
                        dst: Operand::Virtual(vreg),
                        src: Operand::Virtual(slots[0]),
                    });
                } else {
                    self.mir.push(Aarch64Inst::MovRR {
                        dst: Operand::Virtual(vreg),
                        src: Operand::Virtual(base_slots[offset]),
                    });
                }
            }

            CfgInstData::IntCast {
                value: src_value,
                from_ty,
            } => {
                // Integer cast with runtime range check
                let vreg = self.mir.alloc_vreg();
                self.value_map.insert(value, vreg);

                let src_vreg = self.get_vreg(*src_value);
                let to_ty = self.ctx.cfg.get_inst(value).ty;

                // Emit range check and panic if out of bounds
                self.emit_int_cast_check(src_vreg, *from_ty, to_ty);

                // Move the value to the result vreg. A signed source widened to a
                // larger type must be SIGN-extended into the high bits — a plain
                // 64-bit copy would carry the source's zero-extended high bits,
                // turning e.g. i32 -5 into 4294967291 (RUE-88). Emit an explicit
                // sxtb/sxth/sxtw for that case.
                let from_bits = Self::type_bits(*from_ty);
                let to_bits = Self::type_bits(to_ty);
                if from_ty.is_signed() && to_bits > from_bits {
                    let dst = Operand::Virtual(vreg);
                    let src = Operand::Virtual(src_vreg);
                    match from_bits {
                        8 => self.mir.push(Aarch64Inst::Sxtb { dst, src }),
                        16 => self.mir.push(Aarch64Inst::Sxth { dst, src }),
                        _ => self.mir.push(Aarch64Inst::Sxtw { dst, src }),
                    }
                } else {
                    self.mir.push(Aarch64Inst::MovRR {
                        dst: Operand::Virtual(vreg),
                        src: Operand::Virtual(src_vreg),
                    });
                }
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
                let dropped_ty = self.ctx.cfg.get_inst(*dropped_value).ty;

                // Handle String specially - it's a fat pointer (ptr, len, cap)
                if self.ctx.is_builtin_string(dropped_ty) {
                    // String requires all 3 slots as arguments to __rue_drop_String.
                    // The accessor handles every source (cache for StructInit/Call/
                    // BlockParam; materialize for Load/Param/PlaceRead). (RUE-118)
                    let field_vregs = self
                        .get_or_compute_field_vregs(*dropped_value)
                        .expect("String value should have field vregs");

                    debug_assert_eq!(
                        field_vregs.len(),
                        3,
                        "String should have 3 slots (ptr, len, cap)"
                    );
                    // Pass all 3 components (ptr, len, cap) to __rue_drop_String
                    self.emit_call_with_slot_args(&field_vregs, "__rue_drop_String");
                    return;
                }

                // Handle struct drops - need to pass all flattened field values
                if let Some(struct_id) = dropped_ty.as_struct() {
                    let struct_def = self.ctx.type_pool.struct_def(struct_id);

                    // All flattened field slots. The accessor materializes
                    // lazily-sourced values (Load/Param/PlaceRead) so glue
                    // functions dropping a multi-slot Param element pass real
                    // slots, not just slot 0 + garbage (RUE-193); fall back to
                    // the recursive flattener for StructInit.
                    let field_vregs = self
                        .get_or_compute_field_vregs(*dropped_value)
                        .unwrap_or_else(|| self.collect_struct_scalar_vregs(*dropped_value));

                    // For user-defined destructor, we need to call it first with all fields
                    if let Some(ref destructor_name) = struct_def.destructor {
                        // Pass all fields to the user destructor
                        self.emit_call_with_slot_args(&field_vregs, destructor_name);
                    }

                    // Now call the drop glue function to drop fields, passing
                    // all fields again (the destructor call clobbered the
                    // argument registers)
                    let drop_fn_name = format!("__rue_drop_{}", struct_def.name);
                    self.emit_call_with_slot_args(&field_vregs, &drop_fn_name);
                    return;
                }

                // Handle array drops - need to pass all element values
                if let Some(array_id) = dropped_ty.as_array() {
                    // All flattened element slots (accessor first, as above;
                    // slot counts past the argument registers go on the stack
                    // via emit_call_with_slot_args — e.g. [String; 3] is 9
                    // slots, RUE-193)
                    let element_vregs = self
                        .get_or_compute_field_vregs(*dropped_value)
                        .unwrap_or_else(|| self.collect_array_scalar_vregs(*dropped_value));

                    let drop_fn_name = types::array_drop_glue_name(array_id, self.ctx.type_pool);
                    self.emit_call_with_slot_args(&element_vregs, &drop_fn_name);
                    return;
                }

                // Handle enum drops (RUE-221): pass the enum's flattened slots
                // (discriminant + payload union) to its synthesized drop glue,
                // which switches on the discriminant and drops the active
                // variant's payload.
                if let Some(enum_id) = dropped_ty.as_enum() {
                    let field_vregs = self
                        .get_or_compute_field_vregs(*dropped_value)
                        .expect("payload enum value should have field vregs");
                    let enum_def = self.ctx.type_pool.enum_def(enum_id);
                    let drop_fn_name = format!("__rue_drop_{}", enum_def.name);
                    self.emit_call_with_slot_args(&field_vregs, &drop_fn_name);
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

            // Place operations (ADR-0030)
            // These provide a unified abstraction for memory access with projections.
            CfgInstData::PlaceRead { place } => {
                let vreg = self.mir.alloc_vreg();
                self.value_map.insert(value, vreg);
                self.lower_place_read(vreg, place, ty);
            }

            CfgInstData::PlaceWrite { place, value: val } => {
                // A multi-slot aggregate value (struct, String, array) must write
                // ALL its slots to the place, not just the first. The accessor
                // materializes lazily-sourced values; if it can't model the source,
                // fall back to the old single-slot behavior. (RUE-118, RUE-23)
                let val_type = self.ctx.cfg.get_inst(*val).ty;
                let vals = if val_type.is_struct() || val_type.is_array() {
                    self.get_or_compute_field_vregs(*val)
                        .unwrap_or_else(|| vec![self.get_vreg(*val)])
                } else {
                    vec![self.get_vreg(*val)]
                };
                self.lower_place_write(place, &vals);
            }
        }
    }

    /// Check if a comparison should use unsigned comparison instructions.
    ///
    /// Sema guarantees both operands have the same signedness, so we only need to check one.
    fn is_unsigned_comparison(&self, lhs: CfgValue) -> bool {
        self.ctx.cfg.get_inst(lhs).ty.is_unsigned()
    }

    /// Try to extract a power-of-two shift amount from a constant value.
    ///
    /// Returns `Some(shift_amount)` if the value is a constant that is a power of 2
    /// greater than 1, otherwise returns `None`.
    ///
    /// Used for strength reduction: `x * 2^n` can be lowered to `x << n`.
    fn try_power_of_two_shift(&self, value: CfgValue) -> Option<u8> {
        let inst = self.ctx.cfg.get_inst(value);
        match &inst.data {
            CfgInstData::Const(n) => {
                let n = *n;
                // Check if n is a power of 2 and greater than 1
                // n > 1 because x * 1 should be handled by identity optimization (not here)
                // n must fit in u64 for is_power_of_two
                if n > 1 && n.is_power_of_two() {
                    Some(n.trailing_zeros() as u8)
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    /// Emit overflow check for ADD based on the type.
    ///
    /// For 32/64-bit types, we use CPU flags directly:
    /// - Signed (i32, i64): V (overflow) flag via BVC
    /// - Unsigned (u32, u64): C (carry) flag - C=1 means overflow, so branch on Lo (C=0)
    ///
    /// For sub-word types, check if result fits in the type's range.
    /// Emit a range check for sub-word types (U8, U16, I8, I16).
    ///
    /// This checks if the result value fits in the valid range for the type:
    /// - U8: result <= 255
    /// - U16: result <= 65535
    /// - I8: sign-extend and compare with original
    /// - I16: sign-extend and compare with original
    ///
    /// Branches to `ok_label` if the value is in range (no overflow).
    fn emit_subword_range_check(&mut self, ty: Type, result_vreg: VReg, ok_label: LabelId) {
        match ty.kind() {
            TypeKind::U8 => {
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
            TypeKind::U16 => {
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
            TypeKind::I8 => {
                // Sign-extend to 64-bit and compare with original
                let sext_vreg = self.mir.alloc_vreg();
                self.mir.push(Aarch64Inst::Sxtb {
                    dst: Operand::Virtual(sext_vreg),
                    src: Operand::Virtual(result_vreg),
                });
                // Compare at 32-bit width. The sub-word arithmetic was emitted
                // as a 32-bit (W-register) op that zeroes bits 32-63, so a 64-bit
                // compare against the sign-extended byte/word would mismatch a
                // legitimately-negative in-range result and falsely trap
                // (RUE-28 sub, RUE-60 neg). The low 32 bits must match.
                self.mir.push(Aarch64Inst::CmpRR {
                    src1: Operand::Virtual(result_vreg),
                    src2: Operand::Virtual(sext_vreg),
                });
                self.mir.push(Aarch64Inst::BCond {
                    cond: Cond::Eq,
                    label: ok_label,
                });
            }
            TypeKind::I16 => {
                // Sign-extend to 64-bit and compare with original
                let sext_vreg = self.mir.alloc_vreg();
                self.mir.push(Aarch64Inst::Sxth {
                    dst: Operand::Virtual(sext_vreg),
                    src: Operand::Virtual(result_vreg),
                });
                // Compare at 32-bit width. The sub-word arithmetic was emitted
                // as a 32-bit (W-register) op that zeroes bits 32-63, so a 64-bit
                // compare against the sign-extended byte/word would mismatch a
                // legitimately-negative in-range result and falsely trap
                // (RUE-28 sub, RUE-60 neg). The low 32 bits must match.
                self.mir.push(Aarch64Inst::CmpRR {
                    src1: Operand::Virtual(result_vreg),
                    src2: Operand::Virtual(sext_vreg),
                });
                self.mir.push(Aarch64Inst::BCond {
                    cond: Cond::Eq,
                    label: ok_label,
                });
            }
            _ => {
                // Not a sub-word type, do nothing
            }
        }
    }

    fn emit_overflow_check_add(&mut self, ty: Type, result_vreg: VReg) {
        let ok_label = self.mir.alloc_label();

        match ty.kind() {
            // 32-bit and 64-bit unsigned: C=1 means overflow (carry out)
            // Branch to ok if C=0 (no overflow)
            TypeKind::U32 | TypeKind::U64 => {
                self.mir.push(Aarch64Inst::BCond {
                    cond: Cond::Lo, // Lo = C=0 (no carry)
                    label: ok_label,
                });
            }
            // 32-bit and 64-bit signed: V flag indicates overflow
            TypeKind::I32 | TypeKind::I64 => {
                self.mir.push(Aarch64Inst::Bvc { label: ok_label });
            }
            // Sub-word types: check if result fits in type's range
            TypeKind::U8 | TypeKind::U16 | TypeKind::I8 | TypeKind::I16 => {
                self.emit_subword_range_check(ty, result_vreg, ok_label);
            }
            // Other types don't have arithmetic
            _ => return,
        }

        // Overflow occurred - call panic handler
        let symbol_id = self.intern_symbol("__rue_overflow");
        self.mir.push(Aarch64Inst::Bl { symbol_id });
        self.mir.push(Aarch64Inst::Label { id: ok_label });
    }

    /// Emit overflow check for SUB based on the type.
    ///
    /// For ARM64 SUBS:
    /// - Signed: V flag indicates overflow
    /// - Unsigned: C=0 means borrow (underflow), C=1 means no borrow
    fn emit_overflow_check_sub(&mut self, ty: Type, result_vreg: VReg) {
        let ok_label = self.mir.alloc_label();

        match ty.kind() {
            // 32-bit and 64-bit unsigned: C=0 means borrow (underflow)
            // Branch to ok if C=1 (no underflow)
            TypeKind::U32 | TypeKind::U64 => {
                self.mir.push(Aarch64Inst::BCond {
                    cond: Cond::Hs, // Hs = C=1 (no borrow)
                    label: ok_label,
                });
            }
            // 32-bit and 64-bit signed: V flag indicates overflow
            TypeKind::I32 | TypeKind::I64 => {
                self.mir.push(Aarch64Inst::Bvc { label: ok_label });
            }
            // Sub-word types: check if result fits in type's range
            TypeKind::U8 | TypeKind::U16 | TypeKind::I8 | TypeKind::I16 => {
                self.emit_subword_range_check(ty, result_vreg, ok_label);
            }
            // Other types don't have arithmetic
            _ => return,
        }

        let symbol_id = self.intern_symbol("__rue_overflow");
        self.mir.push(Aarch64Inst::Bl { symbol_id });
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

        match ty.kind() {
            // 32-bit signed: SMULL gives 64-bit result
            TypeKind::I32 => {
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
            TypeKind::U32 => {
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
            TypeKind::I64 => {
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
            TypeKind::U64 => {
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
            TypeKind::I8 | TypeKind::I16 | TypeKind::U8 | TypeKind::U16 => {
                // For sub-word, just do the multiply and check range
                self.mir.push(Aarch64Inst::MulRR {
                    dst: Operand::Virtual(result_vreg),
                    src1: Operand::Virtual(lhs_vreg),
                    src2: Operand::Virtual(rhs_vreg),
                });
                self.emit_subword_range_check(ty, result_vreg, ok_label);
            }
            _ => return,
        }

        let symbol_id = self.intern_symbol("__rue_overflow");
        self.mir.push(Aarch64Inst::Bl { symbol_id });
        self.mir.push(Aarch64Inst::Label { id: ok_label });
    }

    /// Emit the signed-division overflow guard: `MIN / -1` (and `MIN % -1`)
    /// overflows because the quotient `-MIN` is unrepresentable. The ARM
    /// architecture defines SDIV to silently wrap (MIN / -1 = MIN) with no
    /// flag or trap, so check `dividend == MIN && divisor == -1` explicitly
    /// and call the overflow panic handler (RUE-30, spec 8.1:3).
    ///
    /// For 32-bit-and-narrower types the compares run at W-register width:
    /// sub-word values are kept sign-extended in the low 32 bits of their
    /// registers, so comparing against the type's MIN there is exact.
    fn emit_signed_div_overflow_check(&mut self, ty: Type, lhs_vreg: VReg, rhs_vreg: VReg) {
        let ok_label = self.mir.alloc_label();
        let is_64 = ty.is_64_bit();

        // divisor == -1? (-1 doesn't fit CMP's imm12; materialize it)
        let neg1_vreg = self.mir.alloc_vreg();
        self.mir.push(Aarch64Inst::MovImm {
            dst: Operand::Virtual(neg1_vreg),
            imm: -1,
        });
        self.mir.push(if is_64 {
            Aarch64Inst::Cmp64RR {
                src1: Operand::Virtual(rhs_vreg),
                src2: Operand::Virtual(neg1_vreg),
            }
        } else {
            Aarch64Inst::CmpRR {
                src1: Operand::Virtual(rhs_vreg),
                src2: Operand::Virtual(neg1_vreg),
            }
        });
        self.mir.push(Aarch64Inst::BCond {
            cond: Cond::Ne,
            label: ok_label,
        });

        // dividend == MIN?
        let (min_val, _) = Self::type_range(ty);
        let min_vreg = self.mir.alloc_vreg();
        self.mir.push(Aarch64Inst::MovImm {
            dst: Operand::Virtual(min_vreg),
            imm: min_val,
        });
        self.mir.push(if is_64 {
            Aarch64Inst::Cmp64RR {
                src1: Operand::Virtual(lhs_vreg),
                src2: Operand::Virtual(min_vreg),
            }
        } else {
            Aarch64Inst::CmpRR {
                src1: Operand::Virtual(lhs_vreg),
                src2: Operand::Virtual(min_vreg),
            }
        });
        self.mir.push(Aarch64Inst::BCond {
            cond: Cond::Ne,
            label: ok_label,
        });

        // Overflow - call panic handler
        let symbol_id = self.intern_symbol("__rue_overflow");
        self.mir.push(Aarch64Inst::Bl { symbol_id });
        self.mir.push(Aarch64Inst::Label { id: ok_label });
    }

    /// Emit overflow check for NEG based on the type.
    ///
    /// For NEGS (0 - x):
    /// - Signed: V flag indicates overflow (when negating MIN_VALUE)
    /// - Unsigned: Any non-zero value causes overflow (since 0 - x wraps)
    fn emit_overflow_check_neg(&mut self, ty: Type, result_vreg: VReg) {
        let ok_label = self.mir.alloc_label();

        match ty.kind() {
            // Unsigned: NEGS sets C=0 for non-zero operands (which is overflow)
            // Branch to ok if C=1 (meaning operand was 0, no overflow)
            TypeKind::U32 | TypeKind::U64 => {
                self.mir.push(Aarch64Inst::BCond {
                    cond: Cond::Hs, // Hs = C=1
                    label: ok_label,
                });
            }
            // Signed: V flag indicates overflow
            TypeKind::I32 | TypeKind::I64 => {
                self.mir.push(Aarch64Inst::Bvc { label: ok_label });
            }
            // Sub-word unsigned types: only 0 is valid (negating to 0)
            TypeKind::U8 | TypeKind::U16 => {
                // Result must be 0 for no overflow
                self.mir.push(Aarch64Inst::Cbz {
                    src: Operand::Virtual(result_vreg),
                    label: ok_label,
                });
            }
            // Sub-word signed types: check if result fits in type's range
            TypeKind::I8 | TypeKind::I16 => {
                self.emit_subword_range_check(ty, result_vreg, ok_label);
            }
            _ => return,
        }

        let symbol_id = self.intern_symbol("__rue_overflow");
        self.mir.push(Aarch64Inst::Bl { symbol_id });
        self.mir.push(Aarch64Inst::Label { id: ok_label });
    }

    /// Push a register-register compare at the width of the compared value:
    /// `Cmp64RR` (x-registers) for 64-bit values, `CmpRR` (w-registers)
    /// otherwise. Range checks on 64-bit sources must not use the 32-bit
    /// form, which compares only the low half (RUE-31).
    fn push_cmp_rr(&mut self, src1: VReg, src2: VReg, bits: u32) {
        if bits > 32 {
            self.mir.push(Aarch64Inst::Cmp64RR {
                src1: Operand::Virtual(src1),
                src2: Operand::Virtual(src2),
            });
        } else {
            self.mir.push(Aarch64Inst::CmpRR {
                src1: Operand::Virtual(src1),
                src2: Operand::Virtual(src2),
            });
        }
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

        let ok_label = self.mir.alloc_label();

        // Calculate the min and max values for the target type
        let (min_val, max_val) = Self::type_range(to_ty);

        if from_signed {
            // Source is signed - need to check both min and max
            if to_signed {
                // Signed to signed: check MIN <= value <= MAX
                if to_bits < from_bits || (to_bits == from_bits && min_val != i64::MIN) {
                    // Check lower bound
                    let min_vreg = self.mir.alloc_vreg();
                    self.mir.push(Aarch64Inst::MovImm {
                        dst: Operand::Virtual(min_vreg),
                        imm: min_val,
                    });
                    // A 64-bit source needs a 64-bit compare: the 32-bit CmpRR
                    // only sees the low half, so e.g. 2^32 would pass an i32
                    // range check and silently truncate (RUE-31).
                    self.push_cmp_rr(src_vreg, min_vreg, from_bits);
                    // For signed comparison, branch if greater or equal
                    self.mir.push(Aarch64Inst::BCond {
                        cond: Cond::Ge,
                        label: ok_label,
                    });

                    // Below min - panic
                    let symbol_id = self.intern_symbol("__rue_intcast_overflow");
                    self.mir.push(Aarch64Inst::Bl { symbol_id });
                    self.mir.push(Aarch64Inst::Label { id: ok_label });

                    let ok_label2 = self.mir.alloc_label();
                    // Check upper bound
                    let max_vreg = self.mir.alloc_vreg();
                    self.mir.push(Aarch64Inst::MovImm {
                        dst: Operand::Virtual(max_vreg),
                        imm: max_val,
                    });
                    self.push_cmp_rr(src_vreg, max_vreg, from_bits);
                    // For signed comparison, branch if less or equal
                    self.mir.push(Aarch64Inst::BCond {
                        cond: Cond::Le,
                        label: ok_label2,
                    });

                    // Above max - panic
                    let symbol_id = self.intern_symbol("__rue_intcast_overflow");
                    self.mir.push(Aarch64Inst::Bl { symbol_id });
                    self.mir.push(Aarch64Inst::Label { id: ok_label2 });
                }
            } else {
                // Signed to unsigned: value must be >= 0 and <= max
                // Check for negative
                //
                // For sub-64-bit types, we need to sign-extend first because
                // ARM64 32-bit operations zero-extend to 64 bits, which would
                // make -1 appear as a large positive number in 64-bit compare.
                let compare_vreg = if from_bits < 64 {
                    let sext_vreg = self.mir.alloc_vreg();
                    match from_ty.kind() {
                        TypeKind::I8 => {
                            self.mir.push(Aarch64Inst::Sxtb {
                                dst: Operand::Virtual(sext_vreg),
                                src: Operand::Virtual(src_vreg),
                            });
                        }
                        TypeKind::I16 => {
                            self.mir.push(Aarch64Inst::Sxth {
                                dst: Operand::Virtual(sext_vreg),
                                src: Operand::Virtual(src_vreg),
                            });
                        }
                        TypeKind::I32 => {
                            self.mir.push(Aarch64Inst::Sxtw {
                                dst: Operand::Virtual(sext_vreg),
                                src: Operand::Virtual(src_vreg),
                            });
                        }
                        _ => unreachable!("non-integer type in intcast"),
                    }
                    sext_vreg
                } else {
                    src_vreg
                };

                self.mir.push(Aarch64Inst::CmpImm {
                    src: Operand::Virtual(compare_vreg),
                    imm: 0,
                });
                self.mir.push(Aarch64Inst::BCond {
                    cond: Cond::Ge,
                    label: ok_label,
                });

                // Negative - panic
                let symbol_id = self.intern_symbol("__rue_intcast_overflow");
                self.mir.push(Aarch64Inst::Bl { symbol_id });
                self.mir.push(Aarch64Inst::Label { id: ok_label });

                // Also check upper bound if narrowing
                if to_bits < from_bits {
                    let ok_label2 = self.mir.alloc_label();
                    let max_vreg = self.mir.alloc_vreg();
                    self.mir.push(Aarch64Inst::MovImm {
                        dst: Operand::Virtual(max_vreg),
                        imm: max_val,
                    });
                    self.push_cmp_rr(src_vreg, max_vreg, from_bits);
                    // Unsigned comparison for upper bound check
                    self.mir.push(Aarch64Inst::BCond {
                        cond: Cond::Ls, // Ls = unsigned less or same
                        label: ok_label2,
                    });

                    // Above max - panic
                    let symbol_id = self.intern_symbol("__rue_intcast_overflow");
                    self.mir.push(Aarch64Inst::Bl { symbol_id });
                    self.mir.push(Aarch64Inst::Label { id: ok_label2 });
                }
            }
        } else {
            // Source is unsigned
            if to_signed {
                // Unsigned to signed: value must fit in positive range of target
                // Check that value <= signed max
                let max_vreg = self.mir.alloc_vreg();
                self.mir.push(Aarch64Inst::MovImm {
                    dst: Operand::Virtual(max_vreg),
                    imm: max_val,
                });
                self.push_cmp_rr(src_vreg, max_vreg, from_bits);
                // Unsigned comparison (Ls = below or same)
                self.mir.push(Aarch64Inst::BCond {
                    cond: Cond::Ls,
                    label: ok_label,
                });

                // Above max - panic
                let symbol_id = self.intern_symbol("__rue_intcast_overflow");
                self.mir.push(Aarch64Inst::Bl { symbol_id });
                self.mir.push(Aarch64Inst::Label { id: ok_label });
            } else {
                // Unsigned to unsigned: narrowing check
                if to_bits < from_bits {
                    let max_vreg = self.mir.alloc_vreg();
                    self.mir.push(Aarch64Inst::MovImm {
                        dst: Operand::Virtual(max_vreg),
                        imm: max_val,
                    });
                    self.push_cmp_rr(src_vreg, max_vreg, from_bits);
                    self.mir.push(Aarch64Inst::BCond {
                        cond: Cond::Ls,
                        label: ok_label,
                    });

                    // Above max - panic
                    let symbol_id = self.intern_symbol("__rue_intcast_overflow");
                    self.mir.push(Aarch64Inst::Bl { symbol_id });
                    self.mir.push(Aarch64Inst::Label { id: ok_label });
                }
            }
        }
    }

    /// Get the bit width of an integer type.
    fn type_bits(ty: Type) -> u32 {
        match ty.kind() {
            TypeKind::I8 | TypeKind::U8 => 8,
            TypeKind::I16 | TypeKind::U16 => 16,
            TypeKind::I32 | TypeKind::U32 => 32,
            TypeKind::I64 | TypeKind::U64 => 64,
            _ => panic!("type_bits called on non-integer type: {:?}", ty),
        }
    }

    /// Get the min and max values for an integer type.
    /// The AND mask (bit_width - 1) applied to a shift count, since the count
    /// is taken modulo the operand's bit width (spec 4.3a:10).
    fn shift_count_mask(ty: Type) -> u64 {
        match Self::type_bits(ty) {
            8 => 0x07,
            16 => 0x0F,
            64 => 0x3F,
            _ => 0x1F, // 32-bit
        }
    }

    /// Materialize the shift count masked to the operand's bit width into a
    /// fresh vreg (so a sub-word variable count >= the width wraps per spec).
    /// For 32/64-bit operands the hardware mask already matches.
    fn emit_masked_shift_count(&mut self, rhs: CfgValue, ty: Type) -> VReg {
        let rhs_vreg = self.get_vreg(rhs);
        if Self::type_bits(ty) >= 32 {
            return rhs_vreg;
        }
        let mask_vreg = self.mir.alloc_vreg();
        self.mir.push(Aarch64Inst::MovImm {
            dst: Operand::Virtual(mask_vreg),
            imm: Self::shift_count_mask(ty) as i64,
        });
        let count_vreg = self.mir.alloc_vreg();
        self.mir.push(Aarch64Inst::AndRR {
            dst: Operand::Virtual(count_vreg),
            src1: Operand::Virtual(rhs_vreg),
            src2: Operand::Virtual(mask_vreg),
        });
        count_vreg
    }

    /// Narrow a value to a sub-word integer type by sign-/zero-extending its
    /// low byte/halfword, so it holds the correct value after an op that may
    /// have set bits above the operand width (e.g. a left shift). No-op for
    /// 32/64-bit types.
    fn emit_subword_narrow(&mut self, vreg: VReg, ty: Type) {
        let dst = Operand::Virtual(vreg);
        let src = Operand::Virtual(vreg);
        match Self::type_bits(ty) {
            8 if ty.is_signed() => self.mir.push(Aarch64Inst::Sxtb { dst, src }),
            8 => self.mir.push(Aarch64Inst::Uxtb { dst, src }),
            16 if ty.is_signed() => self.mir.push(Aarch64Inst::Sxth { dst, src }),
            16 => self.mir.push(Aarch64Inst::Uxth { dst, src }),
            _ => {}
        }
    }

    fn type_range(ty: Type) -> (i64, i64) {
        match ty.kind() {
            TypeKind::I8 => (i8::MIN as i64, i8::MAX as i64),
            TypeKind::I16 => (i16::MIN as i64, i16::MAX as i64),
            TypeKind::I32 => (i32::MIN as i64, i32::MAX as i64),
            TypeKind::I64 => (i64::MIN, i64::MAX),
            TypeKind::U8 => (0, u8::MAX as i64),
            TypeKind::U16 => (0, u16::MAX as i64),
            TypeKind::U32 => (0, u32::MAX as i64),
            TypeKind::U64 => (0, i64::MAX), // Can't represent u64::MAX in i64, but we use unsigned compare
            _ => panic!("type_range called on non-integer type: {:?}", ty),
        }
    }

    /// Emit a comparison instruction.
    fn emit_comparison(&mut self, value: CfgValue, lhs: CfgValue, rhs: CfgValue, cond: Cond) {
        let vreg = self.mir.alloc_vreg();
        self.value_map.insert(value, vreg);

        let lhs_ty = self.ctx.cfg.get_inst(lhs).ty;

        // Special handling for string comparisons
        if self.ctx.is_builtin_string(lhs_ty) {
            // String comparison requires calling __rue_str_eq runtime function
            // Strings are fat pointers: [ptr_vreg, len_vreg] in struct_slot_vregs

            // Get left string fat pointer
            let lhs_fields = self
                .get_or_compute_field_vregs(lhs)
                .expect("String should have fat pointer fields");
            let lhs_ptr = lhs_fields[0];
            let lhs_len = lhs_fields[1];

            // Get right string fat pointer
            let rhs_fields = self
                .get_or_compute_field_vregs(rhs)
                .expect("String should have fat pointer fields");
            let rhs_ptr = rhs_fields[0];
            let rhs_len = rhs_fields[1];

            // Set up arguments for __rue_str_eq(ptr1, len1, ptr2, len2)
            // ARM64 calling convention: X0, X1, X2, X3
            self.mir.push(Aarch64Inst::MovRR {
                dst: Operand::Physical(Reg::X0),
                src: Operand::Virtual(lhs_ptr),
            });
            self.mir.push(Aarch64Inst::MovRR {
                dst: Operand::Physical(Reg::X1),
                src: Operand::Virtual(lhs_len),
            });
            self.mir.push(Aarch64Inst::MovRR {
                dst: Operand::Physical(Reg::X2),
                src: Operand::Virtual(rhs_ptr),
            });
            self.mir.push(Aarch64Inst::MovRR {
                dst: Operand::Physical(Reg::X3),
                src: Operand::Virtual(rhs_len),
            });

            // Call __rue_str_eq
            let symbol_id = self.intern_symbol("__rue_str_eq");
            self.mir.push(Aarch64Inst::Bl { symbol_id });

            // Result is in X0 (0 or 1)
            self.mir.push(Aarch64Inst::MovRR {
                dst: Operand::Virtual(vreg),
                src: Operand::Physical(Reg::X0),
            });

            // For != comparison, invert the result
            if cond == Cond::Ne {
                self.mir.push(Aarch64Inst::EorImm {
                    dst: Operand::Virtual(vreg),
                    src: Operand::Virtual(vreg),
                    imm: 1,
                });
            }
        } else {
            // Normal scalar comparison
            let lhs_vreg = self.get_vreg(lhs);
            let rhs_vreg = self.get_vreg(rhs);

            // Use 64-bit compare for i64/u64 types
            if matches!(lhs_ty, Type::I64 | Type::U64) {
                self.mir.push(Aarch64Inst::Cmp64RR {
                    src1: Operand::Virtual(lhs_vreg),
                    src2: Operand::Virtual(rhs_vreg),
                });
            } else {
                self.mir.push(Aarch64Inst::CmpRR {
                    src1: Operand::Virtual(lhs_vreg),
                    src2: Operand::Virtual(rhs_vreg),
                });
            }
            self.mir.push(Aarch64Inst::Cset {
                dst: Operand::Virtual(vreg),
                cond,
            });
        }
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
            .get_or_compute_field_vregs(lhs)
            .expect("struct should have field vregs");
        let rhs_fields = self
            .get_or_compute_field_vregs(rhs)
            .expect("struct should have field vregs");

        let struct_def = self.ctx.type_pool.struct_def(struct_id);
        let field_count = struct_def.fields.len();

        if field_count == 0 {
            // Empty struct: always equal
            self.mir.push(Aarch64Inst::MovImm {
                dst: Operand::Virtual(result_vreg),
                imm: if invert { 0 } else { 1 },
            });
            return;
        }

        // Start with 1 (true), AND each field comparison result
        self.mir.push(Aarch64Inst::MovImm {
            dst: Operand::Virtual(result_vreg),
            imm: 1,
        });

        // Compare each field and AND with result
        let mut field_slot = 0usize;
        for field in &struct_def.fields {
            let field_slots = self.ctx.type_slot_count(field.ty) as usize;
            let lhs_field_vreg = lhs_fields[field_slot];
            let rhs_field_vreg = rhs_fields[field_slot];

            // Allocate a vreg for this field's comparison result
            let cmp_vreg = self.mir.alloc_vreg();

            // Use 64-bit compare for i64/u64 types
            if matches!(field.ty, Type::I64 | Type::U64) {
                self.mir.push(Aarch64Inst::Cmp64RR {
                    src1: Operand::Virtual(lhs_field_vreg),
                    src2: Operand::Virtual(rhs_field_vreg),
                });
            } else {
                self.mir.push(Aarch64Inst::CmpRR {
                    src1: Operand::Virtual(lhs_field_vreg),
                    src2: Operand::Virtual(rhs_field_vreg),
                });
            }
            self.mir.push(Aarch64Inst::Cset {
                dst: Operand::Virtual(cmp_vreg),
                cond: Cond::Eq,
            });

            // AND with accumulator
            self.mir.push(Aarch64Inst::AndRR {
                dst: Operand::Virtual(result_vreg),
                src1: Operand::Virtual(result_vreg),
                src2: Operand::Virtual(cmp_vreg),
            });

            field_slot += field_slots;
        }

        // Invert result if needed (for !=)
        if invert {
            self.mir.push(Aarch64Inst::EorImm {
                dst: Operand::Virtual(result_vreg),
                src: Operand::Virtual(result_vreg),
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
            .get_or_compute_field_vregs(lhs)
            .expect("string should have fat pointer fields");
        let rhs_fields = self
            .get_or_compute_field_vregs(rhs)
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

        // Move arguments to calling convention registers (AAPCS64)
        // X0 = ptr1, X1 = len1, X2 = ptr2, X3 = len2
        self.mir.push(Aarch64Inst::MovRR {
            dst: Operand::Physical(Reg::X0),
            src: Operand::Virtual(lhs_ptr),
        });
        self.mir.push(Aarch64Inst::MovRR {
            dst: Operand::Physical(Reg::X1),
            src: Operand::Virtual(lhs_len),
        });
        self.mir.push(Aarch64Inst::MovRR {
            dst: Operand::Physical(Reg::X2),
            src: Operand::Virtual(rhs_ptr),
        });
        self.mir.push(Aarch64Inst::MovRR {
            dst: Operand::Physical(Reg::X3),
            src: Operand::Virtual(rhs_len),
        });

        // Call __rue_str_eq
        let symbol_id = self.intern_symbol("__rue_str_eq");
        self.mir.push(Aarch64Inst::Bl { symbol_id });

        // Result is in X0 (0 or 1)
        self.mir.push(Aarch64Inst::MovRR {
            dst: Operand::Virtual(result_vreg),
            src: Operand::Physical(Reg::X0),
        });

        result_vreg
    }

    /// Lower a block terminator.
    fn lower_terminator(&mut self, block: &BasicBlock) {
        match &block.terminator {
            Terminator::Goto {
                target,
                args_start,
                args_len,
            } => {
                // Copy args to target's block params
                let args = self.ctx.cfg.get_extra(*args_start, *args_len);
                for (i, &arg) in args.iter().enumerate() {
                    let arg_type = self.ctx.cfg.get_inst(arg).ty;
                    if arg_type.is_struct() || arg_type.is_array() {
                        // For aggregate args (structs, String, arrays), copy all slot vregs
                        self.copy_aggregate_to_block_param(arg, *target, i as u32);
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
                then_args_start,
                then_args_len,
                else_block,
                else_args_start,
                else_args_len,
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
                let then_args = self.ctx.cfg.get_extra(*then_args_start, *then_args_len);
                for (i, &arg) in then_args.iter().enumerate() {
                    let arg_type = self.ctx.cfg.get_inst(arg).ty;
                    if arg_type.is_struct() || arg_type.is_array() {
                        // For aggregate args (structs, String, arrays), copy all slot vregs
                        self.copy_aggregate_to_block_param(arg, *then_block, i as u32);
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
                let else_args = self.ctx.cfg.get_extra(*else_args_start, *else_args_len);
                for (i, &arg) in else_args.iter().enumerate() {
                    let arg_type = self.ctx.cfg.get_inst(arg).ty;
                    if arg_type.is_struct() || arg_type.is_array() {
                        // For aggregate args (structs, String, arrays), copy all slot vregs
                        self.copy_aggregate_to_block_param(arg, *else_block, i as u32);
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
                cases_start,
                cases_len,
                default,
            } => {
                let scrutinee_vreg = self.get_vreg(*scrutinee);
                // The case value is materialized as a full 64-bit immediate, so a
                // 64-bit scrutinee must be compared at 64-bit width; a 32-bit cmp
                // would match on only the low 32 bits (RUE-27). Sub-64-bit
                // scrutinees keep the 32-bit compare (correct at their width).
                let scrutinee_is_64 = self.ctx.cfg.get_inst(*scrutinee).ty.is_64_bit();

                // Generate comparison and jump for each case
                let cases = self.ctx.cfg.get_switch_cases(*cases_start, *cases_len);
                for (value, target) in cases {
                    // Compare scrutinee with case value (supports signed values for negative patterns)
                    let imm_vreg = self.mir.alloc_vreg();
                    self.mir.push(Aarch64Inst::MovImm {
                        dst: Operand::Virtual(imm_vreg),
                        imm: *value,
                    });
                    if scrutinee_is_64 {
                        self.mir.push(Aarch64Inst::Cmp64RR {
                            src1: Operand::Virtual(scrutinee_vreg),
                            src2: Operand::Virtual(imm_vreg),
                        });
                    } else {
                        self.mir.push(Aarch64Inst::CmpRR {
                            src1: Operand::Virtual(scrutinee_vreg),
                            src2: Operand::Virtual(imm_vreg),
                        });
                    }
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

                let return_type = self.ctx.cfg.return_type();

                if self.fn_name == "main" {
                    let val_vreg = self.get_vreg(*value);
                    self.mir.push(Aarch64Inst::MovRR {
                        dst: Operand::Physical(Reg::X0),
                        src: Operand::Virtual(val_vreg),
                    });
                    let symbol_id = self.intern_symbol("__rue_exit");
                    self.mir.push(Aarch64Inst::Bl { symbol_id });
                } else if self.ctx.is_multislot_aggregate(return_type) {
                    // Return a multi-slot aggregate (struct, array, or a
                    // payload-carrying enum). Payload enums were previously
                    // omitted from this gate (it tested `struct || array`
                    // only), so a by-value enum return fell into the scalar
                    // branch, shipping only the discriminant in x0 while the
                    // payload slot base had no slot vregs — ICEing at the
                    // enum-payload place-read (RUE-237).
                    // Gather all slots through the single accessor, regardless of source
                    // (StructInit/ArrayInit/Call/BlockParam cache-hit; Load/Param/
                    // PlaceRead materialize). Previously the `_` arm returned only slot 0
                    // of a place-read aggregate, and arrays had NO return path at all —
                    // only the ArrayInit placeholder vreg reached x0. (RUE-118, RUE-78)
                    let slot_vregs = self
                        .get_or_compute_field_vregs(*value)
                        .unwrap_or_else(|| vec![self.get_vreg(*value)]);
                    if self.ctx.uses_sret_return(RET_REGS.len() as u32) {
                        // sret return (String always; aggregates that don't fit
                        // the return registers): the caller passed a buffer
                        // pointer as a hidden first argument, which the
                        // prologue saved at the dedicated frame slot one past
                        // the param area. Store every slot through it.
                        // Previously String returns took the register path
                        // here while the caller read the (never-written) sret
                        // buffer — len/cap arrived as garbage. (RUE-92)
                        crate::agg_slots::store_slots_to_sret(self, &slot_vregs);
                    } else {
                        for (i, slot_vreg) in slot_vregs.iter().enumerate() {
                            if i < RET_REGS.len() {
                                self.mir.push(Aarch64Inst::MovRR {
                                    dst: Operand::Physical(RET_REGS[i]),
                                    src: Operand::Virtual(*slot_vreg),
                                });
                            }
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
                // Defense-in-depth (RUE-208): emit a trap rather than nothing.
                // The compiler proved this block unreachable, but if a
                // control-flow bug ever lets execution reach it, `brk` faults
                // (SIGTRAP) immediately instead of silently falling through into
                // whatever code the block layout happens to place next.
                self.mir.push(Aarch64Inst::Brk);
            }

            Terminator::None => {
                panic!("block has no terminator");
            }
        }
    }

    // === Place operations (ADR-0030) ===

    /// Lower a PlaceRead instruction.
    ///
    /// This loads a value from the memory location described by the place,
    /// walking through any projections (field accesses, array indices).
    fn lower_place_read(&mut self, dst: VReg, place: &Place, _ty: Type) {
        let projections = self.ctx.cfg.get_place_projections(place);

        // Simple case: no projections, just load from the base slot
        if projections.is_empty() {
            match place.base {
                PlaceBase::Local(slot) => {
                    let offset = self.ctx.local_offset(slot);
                    self.mir.push(Aarch64Inst::Ldr {
                        dst: Operand::Virtual(dst),
                        base: Reg::Fp,
                        offset,
                    });
                }
                PlaceBase::Param(param_slot) => {
                    // Check if this is an inout parameter
                    if self.ctx.cfg.is_param_inout(param_slot) {
                        // Inout param - load through the pointer
                        let ptr_vreg = self.ensure_inout_param_ptr(param_slot);
                        self.mir.push(Aarch64Inst::LdrIndexed {
                            dst: Operand::Virtual(dst),
                            base: ptr_vreg,
                        });
                    } else {
                        // Normal param - load from local slot
                        let slot = self.ctx.num_locals + param_slot;
                        let offset = self.ctx.local_offset(slot);
                        self.mir.push(Aarch64Inst::Ldr {
                            dst: Operand::Virtual(dst),
                            base: Reg::Fp,
                            offset,
                        });
                    }
                }
            }
            return;
        }

        // Complex case: has projections - compute the address
        self.lower_place_read_with_projections(dst, place, projections);
    }

    /// Lower a PlaceRead with projections (field accesses and/or array indices).
    fn lower_place_read_with_projections(
        &mut self,
        dst: VReg,
        place: &Place,
        projections: &[Projection],
    ) {
        // Calculate the static field offset (sum of all Field projection offsets)
        let mut static_slot_offset: u32 = 0;

        // Collect index projections for dynamic offset calculation
        let mut index_levels: Vec<IndexLevel> = Vec::new();

        for proj in projections {
            match proj {
                Projection::Field {
                    struct_id,
                    field_index,
                } => {
                    let field_offset = self.ctx.struct_field_slot_offset(*struct_id, *field_index);
                    static_slot_offset += field_offset;
                }
                Projection::Index { array_type, index } => {
                    // Emit bounds check for this index
                    let index_vreg = self.get_vreg(*index);
                    let array_length = self.ctx.array_length(*array_type);
                    self.emit_bounds_check(index_vreg, array_length);

                    let elem_slot_count = self.ctx.array_element_slot_count(*array_type);
                    index_levels.push(IndexLevel {
                        index: *index,
                        elem_slot_count,
                        array_type: *array_type,
                    });
                }
            }
        }

        // Calculate dynamic offset from index projections
        let dynamic_offset_vreg = if !index_levels.is_empty() {
            Some(self.compute_index_offset(&index_levels))
        } else {
            None
        };

        // Compute final address based on base type
        match place.base {
            PlaceBase::Local(slot) => {
                let base_slot = slot + static_slot_offset;
                let base_offset = self.ctx.local_offset(base_slot);

                if let Some(dyn_offset) = dynamic_offset_vreg {
                    // Compute address: fp + base_offset - dynamic_offset
                    let addr_vreg = self.mir.alloc_vreg();
                    self.mir.push(Aarch64Inst::AddImm {
                        dst: Operand::Virtual(addr_vreg),
                        src: Operand::Physical(Reg::Fp),
                        imm: base_offset,
                    });
                    self.mir.push(Aarch64Inst::SubRR {
                        dst: Operand::Virtual(addr_vreg),
                        src1: Operand::Virtual(addr_vreg),
                        src2: Operand::Virtual(dyn_offset),
                    });
                    self.mir.push(Aarch64Inst::LdrIndexed {
                        dst: Operand::Virtual(dst),
                        base: addr_vreg,
                    });
                } else {
                    // Static offset only
                    self.mir.push(Aarch64Inst::Ldr {
                        dst: Operand::Virtual(dst),
                        base: Reg::Fp,
                        offset: base_offset,
                    });
                }
            }
            PlaceBase::Param(param_slot) => {
                if self.ctx.cfg.is_param_inout(param_slot) {
                    // Inout param - use pointer
                    let ptr_vreg = self.ensure_inout_param_ptr(param_slot);
                    let static_byte_offset = (static_slot_offset as i32) * 8;

                    if let Some(dyn_offset) = dynamic_offset_vreg {
                        // Compute address: ptr - static_offset - dynamic_offset
                        let addr_vreg = self.mir.alloc_vreg();
                        self.mir.push(Aarch64Inst::MovRR {
                            dst: Operand::Virtual(addr_vreg),
                            src: Operand::Virtual(ptr_vreg),
                        });
                        if static_byte_offset != 0 {
                            self.mir.push(Aarch64Inst::SubImm {
                                dst: Operand::Virtual(addr_vreg),
                                src: Operand::Virtual(addr_vreg),
                                imm: static_byte_offset,
                            });
                        }
                        self.mir.push(Aarch64Inst::SubRR {
                            dst: Operand::Virtual(addr_vreg),
                            src1: Operand::Virtual(addr_vreg),
                            src2: Operand::Virtual(dyn_offset),
                        });
                        self.mir.push(Aarch64Inst::LdrIndexed {
                            dst: Operand::Virtual(dst),
                            base: addr_vreg,
                        });
                    } else {
                        // Static offset only
                        self.mir.push(Aarch64Inst::LdrIndexedOffset {
                            dst: Operand::Virtual(dst),
                            base: ptr_vreg,
                            offset: -static_byte_offset,
                        });
                    }
                } else {
                    // Normal param - treat like local
                    let base_slot = self.ctx.num_locals + param_slot + static_slot_offset;
                    let base_offset = self.ctx.local_offset(base_slot);

                    if let Some(dyn_offset) = dynamic_offset_vreg {
                        let addr_vreg = self.mir.alloc_vreg();
                        self.mir.push(Aarch64Inst::AddImm {
                            dst: Operand::Virtual(addr_vreg),
                            src: Operand::Physical(Reg::Fp),
                            imm: base_offset,
                        });
                        self.mir.push(Aarch64Inst::SubRR {
                            dst: Operand::Virtual(addr_vreg),
                            src1: Operand::Virtual(addr_vreg),
                            src2: Operand::Virtual(dyn_offset),
                        });
                        self.mir.push(Aarch64Inst::LdrIndexed {
                            dst: Operand::Virtual(dst),
                            base: addr_vreg,
                        });
                    } else {
                        self.mir.push(Aarch64Inst::Ldr {
                            dst: Operand::Virtual(dst),
                            base: Reg::Fp,
                            offset: base_offset,
                        });
                    }
                }
            }
        }
    }

    /// Lower a PlaceWrite instruction.
    ///
    /// This stores a value to the memory location described by the place. `vals`
    /// holds one vreg per slot of the value (a single element for scalars); all
    /// slots are stored to consecutive locations. Slot i of an fp-relative place
    /// lives at `local_offset(slot + i)`; through a pointer it lives at
    /// `ptr_offset - i*8` (caller slots descend, stack grows down). (RUE-118)
    fn lower_place_write(&mut self, place: &Place, vals: &[VReg]) {
        let projections = self.ctx.cfg.get_place_projections(place);

        // Simple case: no projections, just store to the base slot
        if projections.is_empty() {
            match place.base {
                PlaceBase::Local(slot) => {
                    crate::agg_slots::store_slots(self, vals, slot);
                }
                PlaceBase::Param(param_slot) => {
                    if self.ctx.cfg.is_param_inout(param_slot) {
                        // Inout param - store through the pointer
                        let ptr_vreg = self.ensure_inout_param_ptr(param_slot);
                        crate::agg_slots::store_slots_through_ptr(self, vals, ptr_vreg, 0);
                    } else {
                        // Normal param - store to local slot
                        let slot = self.ctx.num_locals + param_slot;
                        crate::agg_slots::store_slots(self, vals, slot);
                    }
                }
            }
            return;
        }

        // Complex case: has projections - compute the address
        self.lower_place_write_with_projections(place, projections, vals);
    }

    /// Lower a PlaceWrite with projections.
    fn lower_place_write_with_projections(
        &mut self,
        place: &Place,
        projections: &[Projection],
        vals: &[VReg],
    ) {
        // Calculate the static field offset
        let mut static_slot_offset: u32 = 0;

        // Collect index projections for dynamic offset calculation
        let mut index_levels: Vec<IndexLevel> = Vec::new();

        for proj in projections {
            match proj {
                Projection::Field {
                    struct_id,
                    field_index,
                } => {
                    let field_offset = self.ctx.struct_field_slot_offset(*struct_id, *field_index);
                    static_slot_offset += field_offset;
                }
                Projection::Index { array_type, index } => {
                    // Emit bounds check for this index
                    let index_vreg = self.get_vreg(*index);
                    let array_length = self.ctx.array_length(*array_type);
                    self.emit_bounds_check(index_vreg, array_length);

                    let elem_slot_count = self.ctx.array_element_slot_count(*array_type);
                    index_levels.push(IndexLevel {
                        index: *index,
                        elem_slot_count,
                        array_type: *array_type,
                    });
                }
            }
        }

        // Calculate dynamic offset from index projections
        let dynamic_offset_vreg = if !index_levels.is_empty() {
            Some(self.compute_index_offset(&index_levels))
        } else {
            None
        };

        // Compute final address based on base type
        match place.base {
            PlaceBase::Local(slot) => {
                let base_slot = slot + static_slot_offset;
                let base_offset = self.ctx.local_offset(base_slot);

                if let Some(dyn_offset) = dynamic_offset_vreg {
                    let addr_vreg = self.mir.alloc_vreg();
                    self.mir.push(Aarch64Inst::AddImm {
                        dst: Operand::Virtual(addr_vreg),
                        src: Operand::Physical(Reg::Fp),
                        imm: base_offset,
                    });
                    self.mir.push(Aarch64Inst::SubRR {
                        dst: Operand::Virtual(addr_vreg),
                        src1: Operand::Virtual(addr_vreg),
                        src2: Operand::Virtual(dyn_offset),
                    });
                    crate::agg_slots::store_slots_through_ptr(self, vals, addr_vreg, 0);
                } else {
                    crate::agg_slots::store_slots(self, vals, base_slot);
                }
            }
            PlaceBase::Param(param_slot) => {
                if self.ctx.cfg.is_param_inout(param_slot) {
                    let ptr_vreg = self.ensure_inout_param_ptr(param_slot);
                    let static_byte_offset = (static_slot_offset as i32) * 8;

                    if let Some(dyn_offset) = dynamic_offset_vreg {
                        let addr_vreg = self.mir.alloc_vreg();
                        self.mir.push(Aarch64Inst::MovRR {
                            dst: Operand::Virtual(addr_vreg),
                            src: Operand::Virtual(ptr_vreg),
                        });
                        if static_byte_offset != 0 {
                            self.mir.push(Aarch64Inst::SubImm {
                                dst: Operand::Virtual(addr_vreg),
                                src: Operand::Virtual(addr_vreg),
                                imm: static_byte_offset,
                            });
                        }
                        self.mir.push(Aarch64Inst::SubRR {
                            dst: Operand::Virtual(addr_vreg),
                            src1: Operand::Virtual(addr_vreg),
                            src2: Operand::Virtual(dyn_offset),
                        });
                        crate::agg_slots::store_slots_through_ptr(self, vals, addr_vreg, 0);
                    } else {
                        crate::agg_slots::store_slots_through_ptr(
                            self,
                            vals,
                            ptr_vreg,
                            static_byte_offset,
                        );
                    }
                } else {
                    let base_slot = self.ctx.num_locals + param_slot + static_slot_offset;
                    let base_offset = self.ctx.local_offset(base_slot);

                    if let Some(dyn_offset) = dynamic_offset_vreg {
                        let addr_vreg = self.mir.alloc_vreg();
                        self.mir.push(Aarch64Inst::AddImm {
                            dst: Operand::Virtual(addr_vreg),
                            src: Operand::Physical(Reg::Fp),
                            imm: base_offset,
                        });
                        self.mir.push(Aarch64Inst::SubRR {
                            dst: Operand::Virtual(addr_vreg),
                            src1: Operand::Virtual(addr_vreg),
                            src2: Operand::Virtual(dyn_offset),
                        });
                        crate::agg_slots::store_slots_through_ptr(self, vals, addr_vreg, 0);
                    } else {
                        crate::agg_slots::store_slots(self, vals, base_slot);
                    }
                }
            }
        }
    }

    /// Compute the byte offset for a series of index projections.
    ///
    /// Returns a vreg containing the total byte offset (index * stride for each level).
    fn compute_index_offset(&mut self, levels: &[IndexLevel]) -> VReg {
        let mut total_offset_vreg: Option<VReg> = None;

        for level in levels {
            let level_index_vreg = self.get_vreg(level.index);
            let level_stride = level.elem_slot_count;

            // Scale this level's index by its stride
            let scaled = self.mir.alloc_vreg();
            self.mir.push(Aarch64Inst::MovRR {
                dst: Operand::Virtual(scaled),
                src: Operand::Virtual(level_index_vreg),
            });

            if level_stride == 1 {
                // Simple case: just shift by 3 (multiply by 8)
                self.mir.push(Aarch64Inst::LslImm {
                    dst: Operand::Virtual(scaled),
                    src: Operand::Virtual(scaled),
                    imm: 3,
                });
            } else {
                // Multiply by stride * 8
                let stride_vreg = self.mir.alloc_vreg();
                self.mir.push(Aarch64Inst::MovImm {
                    dst: Operand::Virtual(stride_vreg),
                    imm: (level_stride * 8) as i64,
                });
                self.mir.push(Aarch64Inst::MulRR {
                    dst: Operand::Virtual(scaled),
                    src1: Operand::Virtual(scaled),
                    src2: Operand::Virtual(stride_vreg),
                });
            }

            // Add to running total
            if let Some(prev_total) = total_offset_vreg {
                self.mir.push(Aarch64Inst::AddRR {
                    dst: Operand::Virtual(prev_total),
                    src1: Operand::Virtual(prev_total),
                    src2: Operand::Virtual(scaled),
                });
                // prev_total is modified in place
            } else {
                total_offset_vreg = Some(scaled);
            }
        }

        total_offset_vreg.expect("compute_index_offset called with empty levels")
    }

    /// Compute the address of a place (for the @raw intrinsic and for by-ref
    /// call arguments via `crate::byref_args`, RUE-143).
    ///
    /// This is similar to lower_place_read but returns the address instead of
    /// loading. Index projections are NOT bounds-checked here (@raw is
    /// deliberately unchecked); by-ref arguments bounds-check first in
    /// `byref_args::lower_byref_arg_addr`.
    fn lower_place_addr(&mut self, dst: VReg, place: &Place) {
        let projections = self.ctx.cfg.get_place_projections(place);

        // Calculate static slot offset from field projections
        let mut static_slot_offset: u32 = 0;
        let mut index_levels: Vec<IndexLevel> = Vec::new();

        for proj in projections {
            match proj {
                Projection::Field {
                    struct_id,
                    field_index,
                } => {
                    let field_offset = self.ctx.struct_field_slot_offset(*struct_id, *field_index);
                    static_slot_offset += field_offset;
                }
                Projection::Index { array_type, index } => {
                    let elem_slot_count = self.ctx.array_element_slot_count(*array_type);
                    index_levels.push(IndexLevel {
                        index: *index,
                        elem_slot_count,
                        array_type: *array_type,
                    });
                }
            }
        }

        // Calculate dynamic offset from index projections
        let dynamic_offset_vreg = if !index_levels.is_empty() {
            Some(self.compute_index_offset(&index_levels))
        } else {
            None
        };

        // Compute address based on base type
        match place.base {
            PlaceBase::Local(slot) => {
                let base_slot = slot + static_slot_offset;
                let base_offset = self.ctx.local_offset(base_slot);

                // Start with fp + base_offset
                self.mir.push(Aarch64Inst::AddImm {
                    dst: Operand::Virtual(dst),
                    src: Operand::Physical(Reg::Fp),
                    imm: base_offset,
                });

                // Subtract dynamic offset if any
                if let Some(dyn_offset) = dynamic_offset_vreg {
                    self.mir.push(Aarch64Inst::SubRR {
                        dst: Operand::Virtual(dst),
                        src1: Operand::Virtual(dst),
                        src2: Operand::Virtual(dyn_offset),
                    });
                }
            }
            PlaceBase::Param(param_slot) => {
                if self.ctx.cfg.is_param_inout(param_slot) {
                    let ptr_vreg = self.ensure_inout_param_ptr(param_slot);
                    let static_byte_offset = (static_slot_offset as i32) * 8;

                    // Start with the pointer value
                    self.mir.push(Aarch64Inst::MovRR {
                        dst: Operand::Virtual(dst),
                        src: Operand::Virtual(ptr_vreg),
                    });

                    // Subtract static offset
                    if static_byte_offset != 0 {
                        self.mir.push(Aarch64Inst::SubImm {
                            dst: Operand::Virtual(dst),
                            src: Operand::Virtual(dst),
                            imm: static_byte_offset,
                        });
                    }

                    // Subtract dynamic offset
                    if let Some(dyn_offset) = dynamic_offset_vreg {
                        self.mir.push(Aarch64Inst::SubRR {
                            dst: Operand::Virtual(dst),
                            src1: Operand::Virtual(dst),
                            src2: Operand::Virtual(dyn_offset),
                        });
                    }
                } else {
                    let base_slot = self.ctx.num_locals + param_slot + static_slot_offset;
                    let base_offset = self.ctx.local_offset(base_slot);

                    self.mir.push(Aarch64Inst::AddImm {
                        dst: Operand::Virtual(dst),
                        src: Operand::Physical(Reg::Fp),
                        imm: base_offset,
                    });

                    if let Some(dyn_offset) = dynamic_offset_vreg {
                        self.mir.push(Aarch64Inst::SubRR {
                            dst: Operand::Virtual(dst),
                            src1: Operand::Virtual(dst),
                            src2: Operand::Virtual(dyn_offset),
                        });
                    }
                }
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

impl crate::agg_slots::SlotBackend for CfgLower<'_> {
    fn ctx(&self) -> &crate::cfg_lower::CfgLowerContext<'_> {
        &self.ctx
    }
    fn slot_cache(&mut self) -> &mut std::collections::HashMap<CfgValue, Vec<VReg>> {
        &mut self.struct_slot_vregs
    }
    fn alloc_vreg(&mut self) -> VReg {
        self.mir.alloc_vreg()
    }
    fn get_vreg(&mut self, value: CfgValue) -> VReg {
        CfgLower::get_vreg(self, value)
    }
    fn map_value(&mut self, value: CfgValue, vreg: VReg) {
        self.value_map.insert(value, vreg);
    }
    fn emit_load_slot(&mut self, dst: VReg, slot: u32) {
        let offset = self.ctx.local_offset(slot);
        self.mir.push(Aarch64Inst::Ldr {
            dst: Operand::Virtual(dst),
            base: Reg::Fp,
            offset,
        });
    }
    fn emit_reg_move(&mut self, dst: VReg, src: VReg) {
        self.mir.push(Aarch64Inst::MovRR {
            dst: Operand::Virtual(dst),
            src: Operand::Virtual(src),
        });
    }
    fn emit_load_zero(&mut self, dst: VReg) {
        self.mir.push(Aarch64Inst::MovImm {
            dst: Operand::Virtual(dst),
            imm: 0,
        });
    }
    fn emit_load_imm(&mut self, dst: VReg, imm: i64) {
        self.mir.push(Aarch64Inst::MovImm {
            dst: Operand::Virtual(dst),
            imm,
        });
    }
    fn collect_array_scalars(&mut self, value: CfgValue) -> Vec<VReg> {
        self.collect_array_scalar_vregs(value)
    }
    fn emit_store_slot(&mut self, src: VReg, slot: u32) {
        let offset = self.ctx.local_offset(slot);
        self.mir.push(Aarch64Inst::Str {
            src: Operand::Virtual(src),
            base: Reg::Fp,
            offset,
        });
    }
    fn emit_store_through_ptr(&mut self, src: VReg, ptr: VReg, byte_offset: i32) {
        self.mir.push(Aarch64Inst::StrIndexedOffset {
            src: Operand::Virtual(src),
            base: ptr,
            offset: byte_offset,
        });
    }
    fn emit_load_through_ptr(&mut self, dst: VReg, ptr: VReg, byte_offset: i32) {
        self.mir.push(Aarch64Inst::LdrIndexedOffset {
            dst: Operand::Virtual(dst),
            base: ptr,
            offset: byte_offset,
        });
    }
    fn emit_bounds_check(&mut self, index_vreg: VReg, length: u64) {
        CfgLower::emit_bounds_check(self, index_vreg, length)
    }
    fn emit_place_addr(&mut self, dst: VReg, place: &Place) {
        CfgLower::lower_place_addr(self, dst, place)
    }
}

impl crate::byref_args::ByrefAddrBackend for CfgLower<'_> {
    fn ensure_inout_param_ptr(&mut self, param_slot: u32) -> VReg {
        CfgLower::ensure_inout_param_ptr(self, param_slot)
    }
    fn emit_frame_addr(&mut self, dst: VReg, slot: u32) {
        let offset = self.ctx.local_offset(slot);
        self.mir.push(Aarch64Inst::AddImm {
            dst: Operand::Virtual(dst),
            src: Operand::Physical(Reg::Fp),
            imm: offset,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rue_air::Sema;
    use rue_cfg::CfgBuilder;
    use rue_error::PreviewFeatures;
    use rue_lexer::Lexer;
    use rue_parser::Parser;
    use rue_rir::AstGen;

    fn lower_to_mir(source: &str) -> Aarch64Mir {
        let lexer = Lexer::new(source);
        let (tokens, interner) = lexer.tokenize().unwrap();
        let parser = Parser::new(tokens, interner);
        let (ast, mut interner) = parser.parse().unwrap();

        let astgen = AstGen::new(&ast, &mut interner);
        let rir = astgen.generate();

        let sema = Sema::new(&rir, &mut interner, PreviewFeatures::new());
        let output = sema.analyze_all().unwrap();

        let func = &output.functions[0];
        let type_pool = &output.type_pool;
        let cfg_output = CfgBuilder::build(
            &func.air,
            func.num_locals,
            func.num_param_slots,
            &func.name,
            type_pool,
            func.param_modes.clone(),
            &interner,
        );

        // Use host target for tests
        CfgLower::new(&cfg_output.cfg, type_pool, &interner, Target::host()).lower()
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
