//! CFG to Aarch64Mir lowering.
//!
//! This module converts CFG (explicit control flow graph) to Aarch64Mir
//! (AArch64 instructions with virtual registers).

use std::collections::HashMap;

use lasso::ThreadedRodeo;
use rue_air::FrozenTypeInternPool;
use rue_cfg::{BlockId, Cfg, CfgValue, Type, ValidatedCfg};
use rue_error::CompileResult;
use rue_target::Target;

use super::mir::{Aarch64Inst, Aarch64Mir, Cond, LabelId, Operand, Reg, VReg};
use crate::agg_slots::SlotBackend;
use crate::allocation;
use crate::cfg_lower::CfgLowerContext;

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

// Call sequences and the prologue name these registers physically, and liveness
// models neither those physical definitions nor their uses. So they must be off
// limits to the allocator, not merely unlikely to collide (RUE-1146).
const _: () = {
    let mut index = 0;
    while index < ARG_REGS.len() {
        assert!(
            super::mir::is_reserved(ARG_REGS[index]),
            "every ABI argument register must be reserved from allocation"
        );
        index += 1;
    }
    let mut index = 0;
    while index < RET_REGS.len() {
        assert!(
            super::mir::is_reserved(RET_REGS[index]),
            "every ABI result register must be reserved from allocation"
        );
        index += 1;
    }
};

/// Round `value` up to a multiple of `alignment` (a power of two ≥ 1).
const fn align_up_u32(value: u32, alignment: u32) -> u32 {
    value.div_ceil(alignment) * alignment
}

/// CFG to Aarch64Mir lowering.
pub struct CfgLower<'a> {
    /// Shared context with type helpers and chain tracing.
    ctx: CfgLowerContext<'a>,
    /// Interner for resolving Spur to string
    interner: &'a ThreadedRodeo,
    symbols: crate::MachineSymbolResolver<'a>,
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
    /// Maps by-reference parameter indices to their pointer vregs.
    /// For by-ref params, the slot contains a pointer to the caller's memory.
    /// This map stores the vreg holding that pointer so Store can use it.
    by_ref_param_ptrs: HashMap<u32, VReg>,
    /// Maps register-only by-value parameter indices (RUE-1170) to the vreg
    /// their entry copy defined. These vregs are read-only for the rest of
    /// the function — every consumer copies before mutating, the same
    /// invariant that lets CSE key repeated `Param` reads (RUE-914) — so a
    /// single vreg serves every read.
    param_reg_vregs: HashMap<u32, VReg>,
}

impl crate::call_plan::CallMaterializer for CfgLower<'_> {
    fn materialize_scalar(&mut self, value: CfgValue) -> VReg {
        self.get_vreg(value)
    }

    fn materialize_aggregate(&mut self, value: CfgValue) -> Vec<VReg> {
        self.require_aggregate_slots(value)
    }

    fn materialize_by_ref(&mut self, plan: &crate::value_plan::ByRefAddressPlan) -> VReg {
        crate::byref_args::lower_byref_arg_addr(self, plan)
    }

    fn materialize_sret_pointer(&mut self, storage_bytes: u32) -> VReg {
        self.mir.push(Aarch64Inst::SubImm {
            dst: Operand::Physical(Reg::Sp),
            src: Operand::Physical(Reg::Sp),
            imm: storage_bytes as i32,
        });
        let pointer = self.mir.alloc_vreg();
        self.mir.push(Aarch64Inst::MovRR {
            dst: Operand::Virtual(pointer),
            src: Operand::Physical(Reg::Sp),
        });
        pointer
    }

    fn materialize_indirect_value_arg(
        &mut self,
        value: CfgValue,
        image: &[crate::types::PhysicalEnumSlot],
        padding: &[rue_air::layout::PaddingRange],
        storage_bytes: u32,
    ) -> VReg {
        // Reserve a caller-owned buffer just below the sret storage (RUE-1005),
        // capture its address, and write the aggregate's compact image into it —
        // each slot truncated to its physical width at its compact byte offset,
        // exactly the image the callee prologue unmarshals. The image's padding is
        // zeroed first (ADR-0052 ruling 5) so the buffer is fully initialized.
        self.mir.push(Aarch64Inst::SubImm {
            dst: Operand::Physical(Reg::Sp),
            src: Operand::Physical(Reg::Sp),
            imm: storage_bytes as i32,
        });
        let pointer = self.mir.alloc_vreg();
        self.mir.push(Aarch64Inst::MovRR {
            dst: Operand::Virtual(pointer),
            src: Operand::Physical(Reg::Sp),
        });
        let slots = self.require_aggregate_slots(value);
        crate::agg_slots::store_enum_slots_through_ptr(self, &slots, pointer, image, padding);
        pointer
    }

    fn materialize_indirect_value_arg_dispatch(
        &mut self,
        value: CfgValue,
        image: &crate::types::DispatchImage,
        storage_bytes: u32,
    ) -> VReg {
        // As `materialize_indirect_value_arg`, but the caller-owned buffer is
        // written with a per-variant tag dispatch (RUE-1037).
        self.mir.push(Aarch64Inst::SubImm {
            dst: Operand::Physical(Reg::Sp),
            src: Operand::Physical(Reg::Sp),
            imm: storage_bytes as i32,
        });
        let pointer = self.mir.alloc_vreg();
        self.mir.push(Aarch64Inst::MovRR {
            dst: Operand::Virtual(pointer),
            src: Operand::Physical(Reg::Sp),
        });
        let slots = self.require_aggregate_slots(value);
        crate::agg_slots::store_dispatch_image(self, &slots, pointer, image);
        pointer
    }
}

impl<'a> CfgLower<'a> {
    /// Create a new CFG lowering pass.
    pub fn new(
        cfg: &'a ValidatedCfg,
        type_pool: &'a FrozenTypeInternPool,
        interner: &'a ThreadedRodeo,
        target: Target,
    ) -> Self {
        Self::new_inner(
            cfg,
            type_pool,
            interner,
            target,
            crate::MachineSymbolResolver::default(),
        )
    }

    pub fn new_with_symbols(
        cfg: &'a ValidatedCfg,
        type_pool: &'a FrozenTypeInternPool,
        interner: &'a ThreadedRodeo,
        target: Target,
        symbols: crate::MachineSymbolResolver<'a>,
    ) -> Self {
        Self::new_inner(cfg, type_pool, interner, target, symbols)
    }

    /// Install the pipeline's per-parameter storage decision (RUE-1170).
    pub(crate) fn with_param_storage(
        mut self,
        param_storage: &'a crate::param_storage::ParamStoragePlan,
    ) -> Self {
        self.ctx = self.ctx.with_param_storage(param_storage);
        self
    }

    /// Install the pipeline's local frame-slot decision (RUE-768).
    pub(crate) fn with_local_storage(
        mut self,
        local_storage: &'a crate::local_storage::LocalSlotPlan,
    ) -> Self {
        self.ctx = self.ctx.with_local_storage(local_storage);
        self
    }

    #[cfg(test)]
    pub(crate) fn new_unchecked(
        cfg: &'a Cfg,
        type_pool: &'a FrozenTypeInternPool,
        interner: &'a ThreadedRodeo,
        target: Target,
    ) -> Self {
        Self::new_inner(
            cfg,
            type_pool,
            interner,
            target,
            crate::MachineSymbolResolver::default(),
        )
    }

    fn new_inner(
        cfg: &'a Cfg,
        type_pool: &'a FrozenTypeInternPool,
        interner: &'a ThreadedRodeo,
        target: Target,
        symbols: crate::MachineSymbolResolver<'a>,
    ) -> Self {
        let num_params = cfg.num_params();

        // Pre-calculate capacity hints to reduce HashMap reallocations
        let num_values = cfg.value_count();
        let num_blocks = cfg.blocks().len();
        // Estimate ~4 block params per block on average
        let estimated_block_params = num_blocks.saturating_mul(4);
        // Estimate ~10% of values are struct inits
        let estimated_struct_inits = num_values / 10;
        // Estimate by-ref params are rare, start small.
        let estimated_by_ref_params = num_params.min(4) as usize;

        Self {
            ctx: CfgLowerContext::new(cfg, type_pool),
            interner,
            symbols,
            target,
            mir: Aarch64Mir::new(),
            value_map: HashMap::with_capacity(num_values),
            block_param_vregs: HashMap::with_capacity(estimated_block_params),
            fn_name: cfg.fn_name(),
            struct_slot_vregs: HashMap::with_capacity(estimated_struct_inits),
            by_ref_param_ptrs: HashMap::with_capacity(estimated_by_ref_params),
            param_reg_vregs: HashMap::new(),
        }
    }

    // ========================================================================
    // Helper methods
    // ========================================================================

    /// Intern a symbol name and return its ID.
    fn intern_symbol(&mut self, symbol: &str) -> u32 {
        self.mir.intern_symbol(symbol)
    }

    fn lower_runtime_call(
        &mut self,
        plan: crate::runtime_call_plan::RuntimeCallPlan,
    ) -> crate::value_plan::MaterializedValue {
        use crate::runtime_call_plan::{RuntimeCallArg, RuntimeCallResult};
        use rue_runtime_abi::CallingConvention;

        assert_eq!(plan.calling_convention(), CallingConvention::TargetC);
        assert!(
            plan.args().len() <= ARG_REGS.len(),
            "runtime manifest exceeds the AArch64 target-C register budget"
        );

        let out_shape = plan.out_shape();
        let out_bytes = out_shape
            .map(|shape| (shape.shape().slots.len() as i32 * 8 + 15) & !15)
            .unwrap_or(0);
        if out_bytes > 0 {
            self.mir.push(Aarch64Inst::SubImm {
                dst: Operand::Physical(Reg::Sp),
                src: Operand::Physical(Reg::Sp),
                imm: out_bytes,
            });
        }

        let materialized = plan
            .args()
            .iter()
            .map(|arg| match *arg {
                RuntimeCallArg::Slot { value, .. } => Some(value),
                RuntimeCallArg::Scaled { value, scale, .. } => {
                    Some(crate::allocation::lower_scale(self, value, scale))
                }
                RuntimeCallArg::Extended {
                    value, extension, ..
                } => {
                    if extension == crate::value_plan::IntegerExtension::None {
                        Some(value)
                    } else {
                        let extended = self.mir.alloc_vreg();
                        self.mir.push(Aarch64Inst::MovRR {
                            dst: Operand::Virtual(extended),
                            src: Operand::Virtual(value),
                        });
                        let dst = Operand::Virtual(extended);
                        let src = Operand::Virtual(extended);
                        self.mir.push(match extension {
                            crate::value_plan::IntegerExtension::Sign8 => {
                                Aarch64Inst::Sxtb { dst, src }
                            }
                            crate::value_plan::IntegerExtension::Zero8 => {
                                Aarch64Inst::Uxtb { dst, src }
                            }
                            crate::value_plan::IntegerExtension::Sign16 => {
                                Aarch64Inst::Sxth { dst, src }
                            }
                            crate::value_plan::IntegerExtension::Zero16 => {
                                Aarch64Inst::Uxth { dst, src }
                            }
                            crate::value_plan::IntegerExtension::Sign32 => {
                                Aarch64Inst::Sxtw { dst, src }
                            }
                            crate::value_plan::IntegerExtension::None => unreachable!(),
                        });
                        Some(extended)
                    }
                }
                RuntimeCallArg::Immediate { .. } | RuntimeCallArg::OutPointer { .. } => None,
            })
            .collect::<Vec<_>>();

        for (index, (arg, value)) in plan.args().iter().zip(&materialized).enumerate() {
            let dst = Operand::Physical(ARG_REGS[index]);
            match *arg {
                RuntimeCallArg::Slot { .. }
                | RuntimeCallArg::Scaled { .. }
                | RuntimeCallArg::Extended { .. } => {
                    self.mir.push(Aarch64Inst::MovRR {
                        dst,
                        src: Operand::Virtual(value.expect("materialized runtime argument")),
                    });
                }
                RuntimeCallArg::Immediate { value, .. } => {
                    self.mir.push(Aarch64Inst::MovImm {
                        dst,
                        imm: value as i64,
                    });
                }
                RuntimeCallArg::OutPointer { shape } => {
                    assert_eq!(Some(shape), out_shape);
                    self.mir.push(Aarch64Inst::MovRR {
                        dst,
                        src: Operand::Physical(Reg::Sp),
                    });
                }
            }
        }

        // The manifest's control contract travels with the call: a trap helper
        // aborts, so liveness must not propagate anything past it (RUE-1224).
        let symbol_id = self.intern_symbol(plan.symbol());
        self.mir.push(Aarch64Inst::Bl {
            symbol_id,
            returns: plan.return_behavior(),
        });

        match plan.result() {
            RuntimeCallResult::OutPointer(shape) => {
                let slots = (0..shape.shape().slots.len())
                    .map(|index| {
                        let slot = self.mir.alloc_vreg();
                        self.mir.push(Aarch64Inst::Ldr {
                            dst: Operand::Virtual(slot),
                            base: Reg::Sp,
                            offset: (index * 8) as i32,
                        });
                        slot
                    })
                    .collect::<Vec<_>>();
                self.mir.push(Aarch64Inst::AddImm {
                    dst: Operand::Physical(Reg::Sp),
                    src: Operand::Physical(Reg::Sp),
                    imm: out_bytes,
                });
                crate::value_plan::MaterializedValue {
                    primary: slots[0],
                    slots,
                }
            }
            RuntimeCallResult::Scalar(_) => {
                let primary = self.mir.alloc_vreg();
                self.mir.push(Aarch64Inst::MovRR {
                    dst: Operand::Virtual(primary),
                    src: Operand::Physical(Reg::X0),
                });
                crate::value_plan::MaterializedValue {
                    primary,
                    slots: Vec::new(),
                }
            }
            RuntimeCallResult::Void => {
                let primary = self.mir.alloc_vreg();
                crate::value_plan::MaterializedValue {
                    primary,
                    slots: Vec::new(),
                }
            }
        }
    }

    fn emit_store_ptr_base(&mut self, src: VReg, ptr: VReg) {
        self.mir.push(Aarch64Inst::StrIndexed {
            src: Operand::Virtual(src),
            base: ptr,
        });
    }

    /// Recursively collect all scalar vregs from a struct value.
    fn ensure_by_ref_param_ptr(&mut self, param_slot: u32) -> VReg {
        if let Some(ptr_vreg) = self.by_ref_param_ptrs.get(&param_slot).copied() {
            return ptr_vreg;
        }

        // Load the pointer from the param's frame home. A register-only
        // by-ref pointer (RUE-1170) never reaches this load: the entry
        // preamble copies it out of its argument register into the cache
        // before any block is lowered, so the memoized hit above serves it.
        // Stack-passed pointers stay homed by the prologue, so this load is
        // uniform regardless of param count.
        let ptr_vreg = self.mir.alloc_vreg();
        let slot = self.ctx.param_frame_slot(param_slot);
        let offset = self.ctx.local_offset(slot);
        self.mir.push(Aarch64Inst::Ldr {
            dst: Operand::Virtual(ptr_vreg),
            base: Reg::Fp,
            offset,
        });

        // Cache it for future use
        self.by_ref_param_ptrs.insert(param_slot, ptr_vreg);
        ptr_vreg
    }

    /// Copy every register-only parameter (RUE-1170) out of its incoming
    /// argument register into a virtual register, before CFG control flow
    /// begins: the argument registers are caller-saved, so the copies must
    /// precede every call and dominate every use (including loop back-edges
    /// into the entry block). A register-only by-ref pointer seeds the
    /// by-ref cache directly.
    fn materialize_register_params(&mut self) {
        for (param_slot, abi_index) in self.ctx.param_entry_copies() {
            let vreg = self.mir.alloc_vreg();
            self.mir.push(Aarch64Inst::MovRR {
                dst: Operand::Virtual(vreg),
                src: Operand::Physical(ARG_REGS[abi_index as usize]),
            });
            if self.ctx.cfg.is_param_by_ref(param_slot) {
                self.by_ref_param_ptrs.insert(param_slot, vreg);
            } else {
                self.param_reg_vregs.insert(param_slot, vreg);
            }
        }
    }

    /// Materialize every by-reference parameter pointer before CFG control
    /// flow begins, so the function-wide cache only contains definitions that
    /// dominate every block which may reuse them.
    fn preload_by_ref_param_ptrs(&mut self) {
        self.materialize_register_params();
        for param_slot in crate::value_plan::by_ref_param_slots(&self.ctx) {
            self.ensure_by_ref_param_ptr(param_slot);
        }
    }

    /// Emit a target-local cleanup call using the normalized slot vector.
    fn emit_slot_call(&mut self, arg_vregs: &[VReg], symbol: &str) {
        let plan = crate::call_plan::CallPlan::from_slot_values(
            crate::call_plan::CallTarget::rue(symbol),
            arg_vregs,
            ARG_REGS.len(),
        );
        let _ = self.lower_call_plan(plan);
    }

    /// Call `symbol` passing `arg_vregs` as flattened by-value slot arguments
    /// per the standard convention: the first slots in ARG_REGS, the rest
    /// stored to a 16-byte-aligned stack area released after the call — the
    /// same shape as generic `Call` lowering. Drop and drop-glue calls use this
    /// path so every slot beyond the eight register arguments is passed on the
    /// stack (RUE-193).
    fn block_label(&self, block_id: BlockId) -> LabelId {
        Aarch64Mir::block_label(block_id.as_u32())
    }

    /// Get or compute the slot vregs for a multi-slot aggregate value.
    /// Single shared implementation — see crate::agg_slots. (RUE-121)
    fn require_aggregate_slots(&mut self, value: CfgValue) -> Vec<VReg> {
        crate::agg_slots::require_aggregate_slots(self, value)
    }

    /// Lower CFG to Aarch64Mir.
    pub fn lower(mut self) -> CompileResult<Aarch64Mir> {
        crate::types::ensure_compact_layout_codegen_supported(
            self.ctx.cfg,
            self.ctx.type_pool,
            self.interner,
        )?;
        let ctx = self.ctx;
        crate::terminator_plan::lower_cfg(&ctx, &mut self, None, RET_REGS.len() as u32);
        Ok(self.mir)
    }

    /// Lower CFG to Aarch64Mir with debug information about instruction selection.
    ///
    /// This is like `lower()` but also captures detailed information about
    /// how each CFG instruction maps to MIR instructions.
    pub fn lower_with_debug(mut self) -> CompileResult<(Aarch64Mir, crate::LoweringDebugInfo)> {
        crate::types::ensure_compact_layout_codegen_supported(
            self.ctx.cfg,
            self.ctx.type_pool,
            self.interner,
        )?;
        let mut debug_info = crate::LoweringDebugInfo {
            fn_name: self.fn_name.to_string(),
            target_arch: "aarch64".to_string(),
            blocks: Vec::new(),
        };

        let ctx = self.ctx;
        crate::terminator_plan::lower_cfg(
            &ctx,
            &mut self,
            Some(&mut debug_info),
            RET_REGS.len() as u32,
        );
        Ok((self.mir, debug_info))
    }

    /// Generate rationale for instruction lowering decisions.
    fn get_lowering_rationale(
        &self,
        kind: crate::value_plan::ValueKind,
        ty: Type,
    ) -> Option<String> {
        match kind {
            crate::value_plan::ValueKind::BinaryArithmetic => {
                Some("Operation with overflow check".to_string())
            }
            crate::value_plan::ValueKind::Call => {
                Some("AAPCS64 call uses the shared logical slot plan".to_string())
            }
            crate::value_plan::ValueKind::Parameter => {
                Some("Parameter uses the shared ABI slot plan".to_string())
            }
            crate::value_plan::ValueKind::PlaceRead | crate::value_plan::ValueKind::PlaceWrite => {
                Some("Place operation with bounds checks".to_string())
            }
            crate::value_plan::ValueKind::Shift => {
                if matches!(ty, Type::I8 | Type::I16 | Type::I32 | Type::I64) {
                    Some("Signed shift right (ASR) preserves sign bit".to_string())
                } else {
                    Some("Unsigned shift right (LSR) zero-extends".to_string())
                }
            }
            _ => None,
        }
    }
    fn lower_checked_arithmetic(
        &mut self,
        plan: crate::value_plan::ArithmeticPlan,
    ) -> crate::value_plan::MaterializedValue {
        use crate::value_plan::ArithmeticOperation;
        let overflow_call = plan.overflow_call;
        let div_by_zero_call = plan.div_by_zero_call;
        let wrap = plan.wrap;
        let vreg = match plan.operation {
            ArithmeticOperation::Add { lhs, rhs, width } => {
                let vreg = self.mir.alloc_vreg();
                self.mir.push(if width.bits == 64 {
                    Aarch64Inst::AddsRR64 {
                        dst: Operand::Virtual(vreg),
                        src1: Operand::Virtual(lhs),
                        src2: Operand::Virtual(rhs),
                    }
                } else {
                    Aarch64Inst::AddsRR {
                        dst: Operand::Virtual(vreg),
                        src1: Operand::Virtual(lhs),
                        src2: Operand::Virtual(rhs),
                    }
                });
                if wrap {
                    self.emit_wrap_narrow_subword(width, vreg);
                } else {
                    self.emit_overflow_check_add(width, vreg, overflow_call.clone());
                }
                vreg
            }
            ArithmeticOperation::Sub { lhs, rhs, width } => {
                let vreg = self.mir.alloc_vreg();
                self.mir.push(if width.bits == 64 {
                    Aarch64Inst::SubsRR64 {
                        dst: Operand::Virtual(vreg),
                        src1: Operand::Virtual(lhs),
                        src2: Operand::Virtual(rhs),
                    }
                } else {
                    Aarch64Inst::SubsRR {
                        dst: Operand::Virtual(vreg),
                        src1: Operand::Virtual(lhs),
                        src2: Operand::Virtual(rhs),
                    }
                });
                if wrap {
                    self.emit_wrap_narrow_subword(width, vreg);
                } else {
                    self.emit_overflow_check_sub(width, vreg, overflow_call.clone());
                }
                vreg
            }
            ArithmeticOperation::Mul {
                lhs,
                rhs,
                width,
                shift,
            } => {
                let vreg = self.mir.alloc_vreg();
                if wrap {
                    // Wrapping multiply: one plain 64-bit MUL. Low bits agree for
                    // signed and unsigned; the result is then truncated to the
                    // declared width so the two's-complement wrap is observable
                    // (RUE-647). No widening overflow probe is emitted.
                    self.mir.push(Aarch64Inst::MulRR {
                        dst: Operand::Virtual(vreg),
                        src1: Operand::Virtual(lhs),
                        src2: Operand::Virtual(rhs),
                    });
                    self.emit_wrap_narrow_mul(width, vreg);
                } else if let Some((src, amount)) = shift.filter(|_| width.bits >= 32) {
                    self.mir.push(if width.bits == 64 {
                        Aarch64Inst::LslImm {
                            dst: Operand::Virtual(vreg),
                            src: Operand::Virtual(src),
                            imm: amount,
                        }
                    } else {
                        Aarch64Inst::Lsl32Imm {
                            dst: Operand::Virtual(vreg),
                            src: Operand::Virtual(src),
                            imm: amount,
                        }
                    });
                    let check = self.mir.alloc_vreg();
                    self.mir.push(if width.signed {
                        if width.bits == 64 {
                            Aarch64Inst::Asr64Imm {
                                dst: Operand::Virtual(check),
                                src: Operand::Virtual(vreg),
                                imm: amount,
                            }
                        } else {
                            Aarch64Inst::Asr32Imm {
                                dst: Operand::Virtual(check),
                                src: Operand::Virtual(vreg),
                                imm: amount,
                            }
                        }
                    } else if width.bits == 64 {
                        Aarch64Inst::Lsr64Imm {
                            dst: Operand::Virtual(check),
                            src: Operand::Virtual(vreg),
                            imm: amount,
                        }
                    } else {
                        Aarch64Inst::Lsr32Imm {
                            dst: Operand::Virtual(check),
                            src: Operand::Virtual(vreg),
                            imm: amount,
                        }
                    });
                    self.mir.push(if width.bits == 64 {
                        Aarch64Inst::Cmp64RR {
                            src1: Operand::Virtual(check),
                            src2: Operand::Virtual(src),
                        }
                    } else {
                        Aarch64Inst::CmpRR {
                            src1: Operand::Virtual(check),
                            src2: Operand::Virtual(src),
                        }
                    });
                    let ok = self.mir.alloc_label();
                    self.mir.push(Aarch64Inst::BCond {
                        cond: Cond::Eq,
                        label: ok,
                    });
                    let _ = self.lower_runtime_call(overflow_call.clone());
                    self.mir.push(Aarch64Inst::Label { id: ok });
                } else {
                    self.emit_overflow_check_mul(width, vreg, lhs, rhs, overflow_call.clone());
                }
                vreg
            }
            ArithmeticOperation::Div { lhs, rhs, width } => {
                let vreg = self.mir.alloc_vreg();
                let ok = self.mir.alloc_label();
                self.mir.push(Aarch64Inst::Cbnz {
                    src: Operand::Virtual(rhs),
                    label: ok,
                });
                let _ = self.lower_runtime_call(div_by_zero_call.clone());
                self.mir.push(Aarch64Inst::Label { id: ok });
                if width.signed {
                    self.emit_signed_div_overflow_check(width, lhs, rhs, overflow_call.clone());
                }
                self.mir.push(if width.signed {
                    if width.bits == 64 {
                        Aarch64Inst::Sdiv64RR {
                            dst: Operand::Virtual(vreg),
                            src1: Operand::Virtual(lhs),
                            src2: Operand::Virtual(rhs),
                        }
                    } else {
                        Aarch64Inst::SdivRR {
                            dst: Operand::Virtual(vreg),
                            src1: Operand::Virtual(lhs),
                            src2: Operand::Virtual(rhs),
                        }
                    }
                } else if width.bits == 64 {
                    Aarch64Inst::Udiv64RR {
                        dst: Operand::Virtual(vreg),
                        src1: Operand::Virtual(lhs),
                        src2: Operand::Virtual(rhs),
                    }
                } else {
                    Aarch64Inst::UdivRR {
                        dst: Operand::Virtual(vreg),
                        src1: Operand::Virtual(lhs),
                        src2: Operand::Virtual(rhs),
                    }
                });
                vreg
            }
            ArithmeticOperation::Mod { lhs, rhs, width } => {
                let vreg = self.mir.alloc_vreg();
                let ok = self.mir.alloc_label();
                self.mir.push(Aarch64Inst::Cbnz {
                    src: Operand::Virtual(rhs),
                    label: ok,
                });
                let _ = self.lower_runtime_call(div_by_zero_call.clone());
                self.mir.push(Aarch64Inst::Label { id: ok });
                if width.signed {
                    self.emit_signed_div_overflow_check(width, lhs, rhs, overflow_call.clone());
                }
                let quotient = self.mir.alloc_vreg();
                self.mir.push(if width.signed {
                    if width.bits == 64 {
                        Aarch64Inst::Sdiv64RR {
                            dst: Operand::Virtual(quotient),
                            src1: Operand::Virtual(lhs),
                            src2: Operand::Virtual(rhs),
                        }
                    } else {
                        Aarch64Inst::SdivRR {
                            dst: Operand::Virtual(quotient),
                            src1: Operand::Virtual(lhs),
                            src2: Operand::Virtual(rhs),
                        }
                    }
                } else if width.bits == 64 {
                    Aarch64Inst::Udiv64RR {
                        dst: Operand::Virtual(quotient),
                        src1: Operand::Virtual(lhs),
                        src2: Operand::Virtual(rhs),
                    }
                } else {
                    Aarch64Inst::UdivRR {
                        dst: Operand::Virtual(quotient),
                        src1: Operand::Virtual(lhs),
                        src2: Operand::Virtual(rhs),
                    }
                });
                self.mir.push(if width.bits == 64 {
                    Aarch64Inst::Msub64 {
                        dst: Operand::Virtual(vreg),
                        src1: Operand::Virtual(quotient),
                        src2: Operand::Virtual(rhs),
                        src3: Operand::Virtual(lhs),
                    }
                } else {
                    Aarch64Inst::Msub {
                        dst: Operand::Virtual(vreg),
                        src1: Operand::Virtual(quotient),
                        src2: Operand::Virtual(rhs),
                        src3: Operand::Virtual(lhs),
                    }
                });
                vreg
            }
            ArithmeticOperation::Neg { value, width } => {
                let vreg = self.mir.alloc_vreg();
                self.mir.push(if width.bits == 64 {
                    Aarch64Inst::Negs {
                        dst: Operand::Virtual(vreg),
                        src: Operand::Virtual(value),
                    }
                } else {
                    Aarch64Inst::Negs32 {
                        dst: Operand::Virtual(vreg),
                        src: Operand::Virtual(value),
                    }
                });
                self.emit_overflow_check_neg(width, vreg, overflow_call.clone());
                vreg
            }
        };
        crate::value_plan::MaterializedValue {
            primary: vreg,
            slots: Vec::new(),
        }
    }

    fn lower_drop_plan(
        &mut self,
        actions: Vec<crate::value_plan::DropAction>,
    ) -> crate::value_plan::ValueResult {
        for action in actions {
            self.emit_slot_call(&action.slots, &action.symbol);
        }

        let primary = self.mir.alloc_vreg();
        self.mir.push(Aarch64Inst::MovImm {
            dst: Operand::Virtual(primary),
            imm: 0,
        });
        crate::value_plan::ValueResult::SideEffect
    }

    fn lower_call_plan(
        &mut self,
        plan: crate::call_plan::CallPlan,
    ) -> crate::value_plan::MaterializedValue {
        use crate::call_plan::ReturnPlan;
        let primary = plan.result.unwrap_or_else(|| self.mir.alloc_vreg());
        let num_reg_args = plan.abi_slots.len().min(ARG_REGS.len());
        if plan.stack_bytes > 0 {
            self.mir.push(Aarch64Inst::SubImm {
                dst: Operand::Physical(Reg::Sp),
                src: Operand::Physical(Reg::Sp),
                imm: plan.stack_bytes as i32,
            });
        }
        for (index, arg) in plan.abi_slots.iter().skip(ARG_REGS.len()).enumerate() {
            self.mir.push(Aarch64Inst::Str {
                src: Operand::Virtual(*arg),
                base: Reg::Sp,
                offset: (index * 8) as i32,
            });
        }
        for (index, arg) in plan.abi_slots.iter().take(num_reg_args).enumerate() {
            self.mir.push(Aarch64Inst::MovRR {
                dst: Operand::Physical(ARG_REGS[index]),
                src: Operand::Virtual(*arg),
            });
        }
        let symbol_id = self.intern_symbol(plan.target.symbol());
        self.mir.push(Aarch64Inst::call(symbol_id));
        if plan.stack_bytes > 0 {
            self.mir.push(Aarch64Inst::AddImm {
                dst: Operand::Physical(Reg::Sp),
                src: Operand::Physical(Reg::Sp),
                imm: plan.stack_bytes as i32,
            });
        }
        // Free the by-value indirect argument buffers (RUE-1005), restoring sp
        // to the sret storage (or the pre-call baseline) before the sret
        // read-back reads its buffer at a fixed offset.
        if plan.caller_indirect_bytes > 0 {
            self.mir.push(Aarch64Inst::AddImm {
                dst: Operand::Physical(Reg::Sp),
                src: Operand::Physical(Reg::Sp),
                imm: plan.caller_indirect_bytes as i32,
            });
        }
        let slots = match plan.return_plan {
            ReturnPlan::Sret {
                slot_count,
                storage_bytes,
            } => {
                let slots = if let Some(map) = &plan.compact_return_image {
                    // Compact aggregate return (RUE-1004): read the callee-written
                    // compact image back from the sret buffer, extending each slot
                    // from its physical width at its compact byte offset.
                    let base = self.mir.alloc_vreg();
                    self.mir.push(Aarch64Inst::MovRR {
                        dst: Operand::Virtual(base),
                        src: Operand::Physical(Reg::Sp),
                    });
                    crate::agg_slots::load_enum_slots_through_ptr(self, base, map)
                } else if let Some(image) = &plan.compact_return_dispatch {
                    // Heterogeneous compact aggregate return (RUE-1037): dispatch on
                    // the tag and read the active variant's image from the sret buffer.
                    let base = self.mir.alloc_vreg();
                    self.mir.push(Aarch64Inst::MovRR {
                        dst: Operand::Virtual(base),
                        src: Operand::Physical(Reg::Sp),
                    });
                    crate::agg_slots::load_dispatch_image(self, base, image)
                } else {
                    (0..slot_count)
                        .map(|index| {
                            let slot = self.mir.alloc_vreg();
                            self.mir.push(Aarch64Inst::Ldr {
                                dst: Operand::Virtual(slot),
                                base: Reg::Sp,
                                offset: (index * 8) as i32,
                            });
                            slot
                        })
                        .collect()
                };
                self.mir.push(Aarch64Inst::AddImm {
                    dst: Operand::Physical(Reg::Sp),
                    src: Operand::Physical(Reg::Sp),
                    imm: storage_bytes as i32,
                });
                slots
            }
            ReturnPlan::Registers { slot_count } => (0..slot_count)
                .map(|index| {
                    let slot = self.mir.alloc_vreg();
                    self.mir.push(Aarch64Inst::MovRR {
                        dst: Operand::Virtual(slot),
                        src: Operand::Physical(RET_REGS[index as usize]),
                    });
                    slot
                })
                .collect(),
            ReturnPlan::Scalar | ReturnPlan::ZeroSized => Vec::new(),
        };
        if let Some(&slot) = slots.first() {
            self.mir.push(Aarch64Inst::MovRR {
                dst: Operand::Virtual(primary),
                src: Operand::Virtual(slot),
            });
        } else if matches!(plan.return_plan, ReturnPlan::Scalar) {
            self.mir.push(Aarch64Inst::MovRR {
                dst: Operand::Virtual(primary),
                src: Operand::Physical(Reg::X0),
            });
        } else if matches!(plan.return_plan, ReturnPlan::ZeroSized) {
            self.mir.push(Aarch64Inst::MovImm {
                dst: Operand::Virtual(primary),
                imm: 0,
            });
        }
        // Target-C boundary (ADR-0064 P2): re-extend a foreign scalar return to
        // Rue's canonical 64-bit form. A C callee leaves the bits above the
        // return's declared width unspecified, so the caller extends per the
        // shared classifier's rule.
        if let Some(ext) = plan.foreign_return_extension {
            self.emit_c_return_extension(primary, ext);
        }
        crate::value_plan::MaterializedValue { primary, slots }
    }

    /// Extend a foreign scalar return (in `vreg`) to its canonical 64-bit form
    /// per the target-C classifier (ADR-0064 P2). The narrow value occupies the
    /// low bits of `x0` with unspecified high bits; this restores the sign/zero
    /// extension Rue's scalar invariant relies on.
    fn emit_c_return_extension(&mut self, vreg: VReg, ext: rue_air::ScalarAbiExtension) {
        use rue_air::ScalarAbiExtension;
        let dst = Operand::Virtual(vreg);
        let src = Operand::Virtual(vreg);
        match ext {
            ScalarAbiExtension::None => {}
            ScalarAbiExtension::Signed { from_bits: 8 } => {
                self.mir.push(Aarch64Inst::Sxtb { dst, src })
            }
            ScalarAbiExtension::Signed { from_bits: 16 } => {
                self.mir.push(Aarch64Inst::Sxth { dst, src })
            }
            ScalarAbiExtension::Signed { from_bits: 32 } => {
                self.mir.push(Aarch64Inst::Sxtw { dst, src })
            }
            ScalarAbiExtension::Unsigned { from_bits: 8 } => {
                self.mir.push(Aarch64Inst::Uxtb { dst, src })
            }
            ScalarAbiExtension::Unsigned { from_bits: 16 } => {
                self.mir.push(Aarch64Inst::Uxth { dst, src })
            }
            ScalarAbiExtension::Unsigned { from_bits: 32 } => {
                // AAPCS64 has no single `uxtw` instruction; a 64-bit left/right
                // logical-shift pair zero-extends the low 32 bits.
                self.mir.push(Aarch64Inst::LslImm { dst, src, imm: 32 });
                self.mir.push(Aarch64Inst::Lsr64Imm {
                    dst,
                    src: dst,
                    imm: 32,
                });
            }
            ScalarAbiExtension::Signed { from_bits }
            | ScalarAbiExtension::Unsigned { from_bits } => {
                panic!("unexpected target-C scalar extension width {from_bits}")
            }
        }
    }

    /// Lower an `extern "C"` foreign call that crosses one or more aggregates by
    /// value under AAPCS64 (ADR-0064 P3). The classification comes from the shared
    /// [`ForeignCallInputs`](crate::foreign_call::ForeignCallInputs) authority;
    /// every aggregate is marshaled through its compact physical image (C field
    /// order). Differs from SysV in two documented ways: a >16-byte aggregate is
    /// passed **by reference to a caller-owned copy** (not byval-on-stack), and
    /// the sret pointer uses the dedicated `x8` (not the first argument register)
    /// and is not echoed.
    fn lower_foreign_call(
        &mut self,
        inputs: crate::foreign_call::ForeignCallInputs,
        primary: VReg,
    ) -> crate::value_plan::MaterializedValue {
        use crate::foreign_call::{ForeignArg, ForeignReturn};
        let abi = rue_air::TargetCCallAbi::new(inputs.flavor);
        let budget = ARG_REGS.len();

        // sret storage first (survives the call); its pointer goes in x8.
        let mut sret_ptr: Option<VReg> = None;
        let mut sret_storage: u32 = 0;
        if let ForeignReturn::AggregateSret { image } = &inputs.ret {
            sret_storage = image.storage_bytes;
            self.mir.push(Aarch64Inst::SubImm {
                dst: Operand::Physical(Reg::Sp),
                src: Operand::Physical(Reg::Sp),
                imm: sret_storage as i32,
            });
            let p = self.mir.alloc_vreg();
            self.mir.push(Aarch64Inst::MovRR {
                dst: Operand::Virtual(p),
                src: Operand::Physical(Reg::Sp),
            });
            sret_ptr = Some(p);
        }

        let mut int_ops: Vec<VReg> = Vec::new();
        let mut stack_ops: Vec<VReg> = Vec::new();
        // Caller-owned by-reference copies (AAPCS64 >16-byte composites) are
        // allocated below the sret storage and must survive the call; freed
        // together after the outgoing stack area.
        let mut byref_bytes: u32 = 0;

        for arg in &inputs.args {
            match arg {
                ForeignArg::Scalar { value } => {
                    let v = self.get_vreg(*value);
                    if int_ops.len() < budget {
                        int_ops.push(v);
                    } else {
                        stack_ops.push(v);
                    }
                }
                ForeignArg::AggregateRegisters { value, image } => {
                    let ebs = self.image_arg_eightbytes(*value, image);
                    if int_ops.len() + ebs.len() <= budget {
                        int_ops.extend(ebs);
                    } else {
                        stack_ops.extend(ebs);
                    }
                }
                ForeignArg::AggregateByRefCopy { value, image } => {
                    // Copy the struct into a caller-owned buffer and pass its
                    // address in one integer register (AAPCS64 §6.8.2 C.12).
                    self.mir.push(Aarch64Inst::SubImm {
                        dst: Operand::Physical(Reg::Sp),
                        src: Operand::Physical(Reg::Sp),
                        imm: image.storage_bytes as i32,
                    });
                    byref_bytes += image.storage_bytes;
                    let ptr = self.mir.alloc_vreg();
                    self.mir.push(Aarch64Inst::MovRR {
                        dst: Operand::Virtual(ptr),
                        src: Operand::Physical(Reg::Sp),
                    });
                    let slots = self.require_aggregate_slots(*value);
                    crate::agg_slots::store_enum_slots_through_ptr(
                        self,
                        &slots,
                        ptr,
                        &image.map,
                        &image.padding,
                    );
                    if int_ops.len() < budget {
                        int_ops.push(ptr);
                    } else {
                        stack_ops.push(ptr);
                    }
                }
                ForeignArg::AggregateByvalStack { .. } => {
                    panic!(
                        "AAPCS64 passes a >16-byte aggregate by reference to a caller copy, not \
                         byval-on-stack; ByValueStack is a SysV-only class"
                    )
                }
            }
        }

        let num_stack = stack_ops.len();
        let stack_bytes = align_up_u32((num_stack * 8) as u32, 16);
        if stack_bytes > 0 {
            self.mir.push(Aarch64Inst::SubImm {
                dst: Operand::Physical(Reg::Sp),
                src: Operand::Physical(Reg::Sp),
                imm: stack_bytes as i32,
            });
        }
        for (index, v) in stack_ops.iter().enumerate() {
            self.mir.push(Aarch64Inst::Str {
                src: Operand::Virtual(*v),
                base: Reg::Sp,
                offset: (index * 8) as i32,
            });
        }
        for (index, v) in int_ops.iter().enumerate() {
            self.mir.push(Aarch64Inst::MovRR {
                dst: Operand::Physical(ARG_REGS[index]),
                src: Operand::Virtual(*v),
            });
        }
        // The dedicated indirect-result register x8 holds the sret pointer.
        if let Some(p) = sret_ptr {
            assert!(
                abi.sret_pointer_in_dedicated_register(),
                "AAPCS64 must pass the sret pointer in dedicated register x8"
            );
            self.mir.push(Aarch64Inst::MovRR {
                dst: Operand::Physical(Reg::X8),
                src: Operand::Virtual(p),
            });
        }
        let symbol_id = self.intern_symbol(inputs.symbol_ref());
        self.mir.push(Aarch64Inst::call(symbol_id));
        if stack_bytes > 0 {
            self.mir.push(Aarch64Inst::AddImm {
                dst: Operand::Physical(Reg::Sp),
                src: Operand::Physical(Reg::Sp),
                imm: stack_bytes as i32,
            });
        }
        if byref_bytes > 0 {
            self.mir.push(Aarch64Inst::AddImm {
                dst: Operand::Physical(Reg::Sp),
                src: Operand::Physical(Reg::Sp),
                imm: byref_bytes as i32,
            });
        }

        let slots = match &inputs.ret {
            ForeignReturn::ZeroSized => {
                self.mir.push(Aarch64Inst::MovImm {
                    dst: Operand::Virtual(primary),
                    imm: 0,
                });
                Vec::new()
            }
            ForeignReturn::Scalar { ext } => {
                self.mir.push(Aarch64Inst::MovRR {
                    dst: Operand::Virtual(primary),
                    src: Operand::Physical(Reg::X0),
                });
                self.emit_c_return_extension(primary, *ext);
                Vec::new()
            }
            ForeignReturn::AggregateRegisters { image } => {
                // x0:x1 hold the return eightbytes (C field order). Bridge them
                // through a scratch buffer to reconstruct the native slots.
                let eb = image.eightbytes();
                self.mir.push(Aarch64Inst::SubImm {
                    dst: Operand::Physical(Reg::Sp),
                    src: Operand::Physical(Reg::Sp),
                    imm: image.storage_bytes as i32,
                });
                let buf = self.mir.alloc_vreg();
                self.mir.push(Aarch64Inst::MovRR {
                    dst: Operand::Virtual(buf),
                    src: Operand::Physical(Reg::Sp),
                });
                let mut eb_vals = Vec::with_capacity(eb as usize);
                for index in 0..eb as usize {
                    let v = self.mir.alloc_vreg();
                    self.mir.push(Aarch64Inst::MovRR {
                        dst: Operand::Virtual(v),
                        src: Operand::Physical(RET_REGS[index]),
                    });
                    eb_vals.push(v);
                }
                crate::agg_slots::store_slots_through_ptr(self, &eb_vals, buf, 0);
                let native = crate::agg_slots::load_enum_slots_through_ptr(self, buf, &image.map);
                self.mir.push(Aarch64Inst::AddImm {
                    dst: Operand::Physical(Reg::Sp),
                    src: Operand::Physical(Reg::Sp),
                    imm: image.storage_bytes as i32,
                });
                native
            }
            ForeignReturn::AggregateSret { image } => {
                let p = sret_ptr.expect("an sret return reserved its storage pointer");
                let native = crate::agg_slots::load_enum_slots_through_ptr(self, p, &image.map);
                self.mir.push(Aarch64Inst::AddImm {
                    dst: Operand::Physical(Reg::Sp),
                    src: Operand::Physical(Reg::Sp),
                    imm: sret_storage as i32,
                });
                native
            }
        };
        if let Some(&slot) = slots.first() {
            self.mir.push(Aarch64Inst::MovRR {
                dst: Operand::Virtual(primary),
                src: Operand::Virtual(slot),
            });
        }
        crate::value_plan::MaterializedValue { primary, slots }
    }

    /// Materialize an aggregate argument's eightbytes (ADR-0064 P3): write its
    /// native slots into a scratch buffer as the compact C image, then load the
    /// whole eightbytes back so they pack in ascending C field order. rsp-neutral
    /// — a register aggregate argument's bytes live only in the returned vregs.
    fn image_arg_eightbytes(
        &mut self,
        value: CfgValue,
        image: &crate::foreign_call::AggregateImage,
    ) -> Vec<VReg> {
        self.mir.push(Aarch64Inst::SubImm {
            dst: Operand::Physical(Reg::Sp),
            src: Operand::Physical(Reg::Sp),
            imm: image.storage_bytes as i32,
        });
        let buf = self.mir.alloc_vreg();
        self.mir.push(Aarch64Inst::MovRR {
            dst: Operand::Virtual(buf),
            src: Operand::Physical(Reg::Sp),
        });
        let slots = self.require_aggregate_slots(value);
        crate::agg_slots::store_enum_slots_through_ptr(
            self,
            &slots,
            buf,
            &image.map,
            &image.padding,
        );
        let ebs = crate::agg_slots::load_slots_through_ptr(self, buf, image.eightbytes());
        self.mir.push(Aarch64Inst::AddImm {
            dst: Operand::Physical(Reg::Sp),
            src: Operand::Physical(Reg::Sp),
            imm: image.storage_bytes as i32,
        });
        ebs
    }

    fn lower_residual_value(
        &mut self,
        plan: crate::value_plan::ValueEmissionPlan,
    ) -> crate::value_plan::ValueResult {
        use crate::value_plan::{BitwiseOp, ResidualValuePlan, ShiftOp, ValueResult};
        let ty = plan.ty;
        let width = plan.policy.integer_width;
        let mut slots = Vec::new();
        let primary = match plan.value {
            ResidualValuePlan::Const { value } => {
                let dst = self.mir.alloc_vreg();
                self.mir.push(Aarch64Inst::MovImm {
                    dst: Operand::Virtual(dst),
                    imm: value as i64,
                });
                dst
            }
            ResidualValuePlan::BoolConst { value } => {
                let dst = self.mir.alloc_vreg();
                self.mir.push(Aarch64Inst::MovImm {
                    dst: Operand::Virtual(dst),
                    imm: value as i64,
                });
                dst
            }
            ResidualValuePlan::StringConst { string_id } => {
                let ptr = self.mir.alloc_vreg();
                let len = self.mir.alloc_vreg();
                self.mir.push(Aarch64Inst::StringConstPtr {
                    dst: Operand::Virtual(ptr),
                    string_id,
                });
                self.mir.push(Aarch64Inst::StringConstLen {
                    dst: Operand::Virtual(len),
                    string_id,
                });
                // A `str` view is `{ptr, len}`; an owned `StrBuf` header is
                // `{buf, cap, len}` — the `RawBuf(u8)` core's `{buf, cap}` then
                // the length (RUE-1066), so `cap` precedes `len`.
                if plan.policy.shape.slot_count() >= 3 {
                    let cap = self.mir.alloc_vreg();
                    self.mir.push(Aarch64Inst::StringConstCap {
                        dst: Operand::Virtual(cap),
                        string_id,
                    });
                    slots = vec![ptr, cap, len];
                } else {
                    slots = vec![ptr, len];
                }
                ptr
            }
            ResidualValuePlan::Param { index } => {
                let (primary, result_slots) = self.lower_param_value(index, ty, plan.policy);
                slots = result_slots;
                primary
            }
            ResidualValuePlan::BlockParam { .. } => panic!("block parameter must be preallocated"),
            ResidualValuePlan::Not { value } => {
                let dst = self.mir.alloc_vreg();
                self.mir.push(Aarch64Inst::EorImm {
                    dst: Operand::Virtual(dst),
                    src: Operand::Virtual(value),
                    imm: 1,
                });
                dst
            }
            ResidualValuePlan::BitNot { value } => {
                let dst = self.mir.alloc_vreg();
                self.mir.push(if width.is_some_and(|w| w.bits == 64) {
                    Aarch64Inst::MvnRR {
                        dst: Operand::Virtual(dst),
                        src: Operand::Virtual(value),
                    }
                } else {
                    Aarch64Inst::Mvn32RR {
                        dst: Operand::Virtual(dst),
                        src: Operand::Virtual(value),
                    }
                });
                self.emit_subword_narrow(dst, ty);
                dst
            }
            ResidualValuePlan::Bitwise { op, lhs, rhs } => {
                let dst = self.mir.alloc_vreg();
                self.mir.push(match op {
                    BitwiseOp::And => Aarch64Inst::AndRR {
                        dst: Operand::Virtual(dst),
                        src1: Operand::Virtual(lhs),
                        src2: Operand::Virtual(rhs),
                    },
                    BitwiseOp::Or => Aarch64Inst::OrrRR {
                        dst: Operand::Virtual(dst),
                        src1: Operand::Virtual(lhs),
                        src2: Operand::Virtual(rhs),
                    },
                    BitwiseOp::Xor => Aarch64Inst::EorRR {
                        dst: Operand::Virtual(dst),
                        src1: Operand::Virtual(lhs),
                        src2: Operand::Virtual(rhs),
                    },
                });
                dst
            }
            ResidualValuePlan::Shift {
                op,
                lhs,
                rhs,
                constant,
            } => {
                let dst = self.mir.alloc_vreg();
                let is64 = width.is_some_and(|w| w.bits == 64);
                let signed = width.is_some_and(|w| w.signed);
                if let Some(value) = constant {
                    let imm =
                        (value & plan.policy.shift_count_mask.expect("shift count mask")) as u8;
                    self.mir.push(match (op, is64, signed) {
                        (ShiftOp::Left, true, _) => Aarch64Inst::LslImm {
                            dst: Operand::Virtual(dst),
                            src: Operand::Virtual(lhs),
                            imm,
                        },
                        (ShiftOp::Left, false, _) => Aarch64Inst::Lsl32Imm {
                            dst: Operand::Virtual(dst),
                            src: Operand::Virtual(lhs),
                            imm,
                        },
                        (ShiftOp::Right, true, true) => Aarch64Inst::Asr64Imm {
                            dst: Operand::Virtual(dst),
                            src: Operand::Virtual(lhs),
                            imm,
                        },
                        (ShiftOp::Right, false, true) => Aarch64Inst::Asr32Imm {
                            dst: Operand::Virtual(dst),
                            src: Operand::Virtual(lhs),
                            imm,
                        },
                        (ShiftOp::Right, true, false) => Aarch64Inst::Lsr64Imm {
                            dst: Operand::Virtual(dst),
                            src: Operand::Virtual(lhs),
                            imm,
                        },
                        (ShiftOp::Right, false, false) => Aarch64Inst::Lsr32Imm {
                            dst: Operand::Virtual(dst),
                            src: Operand::Virtual(lhs),
                            imm,
                        },
                    });
                } else {
                    let rhs = self.emit_masked_shift_count_vreg(
                        rhs,
                        plan.policy.shift_count_mask.expect("shift count mask"),
                    );
                    self.mir.push(match (op, is64, signed) {
                        (ShiftOp::Left, true, _) => Aarch64Inst::LslRR {
                            dst: Operand::Virtual(dst),
                            src1: Operand::Virtual(lhs),
                            src2: Operand::Virtual(rhs),
                        },
                        (ShiftOp::Left, false, _) => Aarch64Inst::Lsl32RR {
                            dst: Operand::Virtual(dst),
                            src1: Operand::Virtual(lhs),
                            src2: Operand::Virtual(rhs),
                        },
                        (ShiftOp::Right, true, true) => Aarch64Inst::AsrRR {
                            dst: Operand::Virtual(dst),
                            src1: Operand::Virtual(lhs),
                            src2: Operand::Virtual(rhs),
                        },
                        (ShiftOp::Right, false, true) => Aarch64Inst::Asr32RR {
                            dst: Operand::Virtual(dst),
                            src1: Operand::Virtual(lhs),
                            src2: Operand::Virtual(rhs),
                        },
                        (ShiftOp::Right, true, false) => Aarch64Inst::LsrRR {
                            dst: Operand::Virtual(dst),
                            src1: Operand::Virtual(lhs),
                            src2: Operand::Virtual(rhs),
                        },
                        (ShiftOp::Right, false, false) => Aarch64Inst::Lsr32RR {
                            dst: Operand::Virtual(dst),
                            src1: Operand::Virtual(lhs),
                            src2: Operand::Virtual(rhs),
                        },
                    });
                }
                self.emit_subword_narrow(dst, ty);
                dst
            }
            ResidualValuePlan::Comparison {
                op,
                lhs,
                rhs,
                leaf_types,
                runtime_call,
            } => self.lower_comparison(op, lhs, rhs, plan.policy, &leaf_types, runtime_call),
            ResidualValuePlan::Alloc {
                slot,
                init,
                init_shape,
            } => {
                if init_shape.slot_count() == 0 {
                    return ValueResult::SideEffect;
                } else if init.slots.is_empty() {
                    self.emit_store_slot(init.primary, slot);
                } else {
                    crate::agg_slots::store_slots(self, &init.slots, slot);
                }
                return ValueResult::SideEffect;
            }
            ResidualValuePlan::Load { slot } => {
                let count = plan.policy.shape.slot_count();
                if count == 0 {
                    self.mir.alloc_vreg()
                } else if count > 1 {
                    slots = crate::agg_slots::load_slots_at_low(self, slot + count - 1, count);
                    let dst = slots[0];
                    dst
                } else {
                    let dst = self.mir.alloc_vreg();
                    self.emit_load_slot(dst, slot);
                    dst
                }
            }
            ResidualValuePlan::Store {
                destination,
                value,
                value_shape,
            } => {
                if value_shape.slot_count() == 0 {
                    return ValueResult::SideEffect;
                }
                if value.slots.is_empty() {
                    match destination {
                        crate::value_plan::StoreDestination::FrameSlot(slot) => {
                            self.emit_store_slot(value.primary, slot)
                        }
                        crate::value_plan::StoreDestination::ByRefParam(param_slot) => {
                            let ptr = self.ensure_by_ref_param_ptr(param_slot);
                            self.emit_store_ptr_base(value.primary, ptr);
                        }
                    }
                } else {
                    match destination {
                        crate::value_plan::StoreDestination::FrameSlot(slot) => {
                            crate::agg_slots::store_slots(self, &value.slots, slot)
                        }
                        crate::value_plan::StoreDestination::ByRefParam(param_slot) => {
                            let ptr = self.ensure_by_ref_param_ptr(param_slot);
                            crate::agg_slots::store_slots_through_ptr(self, &value.slots, ptr, 0);
                        }
                    }
                }
                return ValueResult::SideEffect;
            }
            ResidualValuePlan::ParamStore {
                param_slot,
                value,
                value_shape,
            } => {
                if value_shape.slot_count() == 0 {
                    return ValueResult::SideEffect;
                }
                if self.ctx.cfg.is_param_by_ref(param_slot) {
                    let ptr = self.ensure_by_ref_param_ptr(param_slot);
                    if value.slots.is_empty() {
                        self.emit_store_ptr_base(value.primary, ptr);
                    } else {
                        crate::agg_slots::store_slots_through_ptr(self, &value.slots, ptr, 0);
                    }
                } else {
                    // By-value slot (a `mut self` receiver): the param area
                    // itself holds the value, so write the slots directly —
                    // there is no pointer to chase and no caller write-back.
                    // Mirrors the projection-free by-value Param arm of
                    // `place_lower::lower_place_write_plan`.
                    let vals = if value.slots.is_empty() {
                        vec![value.primary]
                    } else {
                        value.slots
                    };
                    crate::agg_slots::store_slots(
                        self,
                        &vals,
                        self.ctx.param_frame_slot(param_slot),
                    );
                }
                return ValueResult::SideEffect;
            }
            ResidualValuePlan::PlaceRead { place } => {
                let count = plan.policy.shape.slot_count();
                if count > 1 {
                    let addr = self.mir.alloc_vreg();
                    crate::place_lower::lower_checked_place_addr_plan(self, addr, &place);
                    slots = crate::agg_slots::load_slots_through_ptr(self, addr, count);
                    let dst = slots[0];
                    dst
                } else {
                    let dst = self.mir.alloc_vreg();
                    crate::place_lower::lower_place_read_plan(self, dst, &place, ty);
                    dst
                }
            }
            ResidualValuePlan::PlaceWrite {
                place,
                value,
                value_shape,
            } => {
                let vals = if matches!(
                    value_shape,
                    crate::value_plan::ValueShape::ZeroSized
                        | crate::value_plan::ValueShape::CompleteAggregate { slot_count: 0 }
                ) {
                    Vec::new()
                } else if value.slots.is_empty() {
                    vec![value.primary]
                } else {
                    value.slots
                };
                crate::place_lower::lower_place_write_plan(self, &place, &vals);
                return ValueResult::SideEffect;
            }
            ResidualValuePlan::StructInit { fields, .. }
            | ResidualValuePlan::ArrayInit { elements: fields } => {
                slots.clear();
                for (field, shape) in fields {
                    if matches!(
                        shape,
                        crate::value_plan::ValueShape::ZeroSized
                            | crate::value_plan::ValueShape::CompleteAggregate { slot_count: 0 }
                    ) {
                        continue;
                    }
                    let source = if field.slots.is_empty() {
                        vec![field.primary]
                    } else {
                        field.slots
                    };
                    slots.extend(source);
                }
                let dst = self.mir.alloc_vreg();
                if matches!(
                    plan.policy.aggregate_primary,
                    crate::value_plan::AggregatePrimary::Zero
                ) {
                    self.mir.push(Aarch64Inst::MovImm {
                        dst: Operand::Virtual(dst),
                        imm: 0,
                    });
                } else if let Some(&first) = slots.first() {
                    self.mir.push(Aarch64Inst::MovRR {
                        dst: Operand::Virtual(dst),
                        src: Operand::Virtual(first),
                    });
                } else {
                    self.mir.push(Aarch64Inst::MovImm {
                        dst: Operand::Virtual(dst),
                        imm: 0,
                    });
                }
                dst
            }
            ResidualValuePlan::EnumVariant {
                variant_index,
                payload,
                total_slots,
                zero_unused_payload,
                ..
            } => {
                slots = (0..total_slots).map(|_| self.mir.alloc_vreg()).collect();
                self.mir.push(Aarch64Inst::MovImm {
                    dst: Operand::Virtual(slots[0]),
                    imm: variant_index as i64,
                });
                let mut offset = 1;
                for (value, shape) in payload {
                    if shape.slot_count() == 0 {
                        continue;
                    }
                    let values = if value.slots.is_empty() {
                        vec![value.primary]
                    } else {
                        value.slots
                    };
                    for value in values {
                        if offset < slots.len() {
                            self.mir.push(Aarch64Inst::MovRR {
                                dst: Operand::Virtual(slots[offset]),
                                src: Operand::Virtual(value),
                            });
                            offset += 1;
                        }
                    }
                }
                // Zero the payload slots this (shorter) variant does not write, so a
                // compact memory image marshalled from the value carries no residue
                // from a wider variant (ADR-0052 ruling 5). Only under the gate.
                if zero_unused_payload {
                    for slot in slots.iter().skip(offset) {
                        self.mir.push(Aarch64Inst::MovImm {
                            dst: Operand::Virtual(*slot),
                            imm: 0,
                        });
                    }
                }
                let dst = slots[0];
                dst
            }
            ResidualValuePlan::EnumPayloadGet {
                base_slots,
                field_offset,
                field_slots,
            } => {
                slots.clear();
                for index in 0..field_slots {
                    let dst = self.mir.alloc_vreg();
                    self.mir.push(Aarch64Inst::MovRR {
                        dst: Operand::Virtual(dst),
                        src: Operand::Virtual(base_slots[(field_offset + index) as usize]),
                    });
                    slots.push(dst);
                }
                let dst = slots
                    .first()
                    .copied()
                    .unwrap_or_else(|| self.mir.alloc_vreg());
                dst
            }
            ResidualValuePlan::IntCast {
                value,
                from_width,
                trap_call,
            } => {
                let dst = self.mir.alloc_vreg();
                self.mir.push(Aarch64Inst::MovRR {
                    dst: Operand::Virtual(dst),
                    src: Operand::Virtual(value),
                });
                let to_width = width.unwrap();
                self.emit_int_cast_check(value, from_width, to_width, trap_call);
                if from_width.signed && to_width.bits > from_width.bits {
                    let src = Operand::Virtual(value);
                    let dst = Operand::Virtual(dst);
                    self.mir.push(match from_width.bits {
                        8 => Aarch64Inst::Sxtb { dst, src },
                        16 => Aarch64Inst::Sxth { dst, src },
                        _ => Aarch64Inst::Sxtw { dst, src },
                    });
                }
                dst
            }
            ResidualValuePlan::Drop { actions } => {
                self.lower_drop_plan(actions);
                return ValueResult::SideEffect;
            }
            ResidualValuePlan::StorageLive { .. } | ResidualValuePlan::StorageDead { .. } => {
                return ValueResult::SideEffect;
            }
        };
        ValueResult::Materialized(crate::value_plan::MaterializedValue { primary, slots })
    }

    fn lower_param_value(
        &mut self,
        index: u32,
        _ty: Type,
        policy: crate::value_plan::ValuePlan,
    ) -> (VReg, Vec<VReg>) {
        let dst = self.mir.alloc_vreg();
        let count = policy.shape.slot_count();
        if count == 0 {
            self.mir.push(Aarch64Inst::MovImm {
                dst: Operand::Virtual(dst),
                imm: 0,
            });
            return (dst, Vec::new());
        }
        if let crate::value_plan::StoragePolicy::ParameterSlot { by_ref: true, .. } = policy.storage
        {
            let ptr = self.ensure_by_ref_param_ptr(index);
            if count > 1 {
                let slots: Vec<_> = (0..count)
                    .map(|slot| {
                        let v = self.mir.alloc_vreg();
                        self.mir.push(Aarch64Inst::LdrIndexedOffset {
                            dst: Operand::Virtual(v),
                            base: ptr,
                            offset: (slot * 8) as i32,
                        });
                        v
                    })
                    .collect();
                return (slots[0], slots);
            }
            self.mir.push(Aarch64Inst::LdrIndexed {
                dst: Operand::Virtual(dst),
                base: ptr,
            });
        } else if count > 1 {
            let slots: Vec<_> = (0..count)
                .map(|slot| {
                    let v = self.mir.alloc_vreg();
                    let frame_slot = self.ctx.param_frame_slot(index) + count - 1 - slot;
                    self.mir.push(Aarch64Inst::Ldr {
                        dst: Operand::Virtual(v),
                        base: Reg::Fp,
                        offset: self.ctx.local_offset(frame_slot),
                    });
                    v
                })
                .collect();
            return (slots[0], slots);
        } else if let Some(&vreg) = self.param_reg_vregs.get(&index) {
            // Register-only scalar (RUE-1170): the entry preamble copied the
            // argument register into one read-only vreg shared by every read.
            return (vreg, Vec::new());
        } else {
            self.mir.push(Aarch64Inst::Ldr {
                dst: Operand::Virtual(dst),
                base: Reg::Fp,
                offset: self.ctx.local_offset(self.ctx.param_frame_slot(index)),
            });
        }
        (dst, Vec::new())
    }

    fn lower_scalar_comparison(
        &mut self,
        op: crate::value_plan::ComparisonOp,
        lhs: VReg,
        rhs: VReg,
        policy: crate::value_plan::ValuePlan,
    ) -> VReg {
        let dst = self.mir.alloc_vreg();
        let width = policy.comparison_width();
        self.mir.push(if width.bits == 64 {
            Aarch64Inst::Cmp64RR {
                src1: Operand::Virtual(lhs),
                src2: Operand::Virtual(rhs),
            }
        } else {
            Aarch64Inst::CmpRR {
                src1: Operand::Virtual(lhs),
                src2: Operand::Virtual(rhs),
            }
        });
        let cond = match (op, width.signed) {
            (crate::value_plan::ComparisonOp::Eq, _) => Cond::Eq,
            (crate::value_plan::ComparisonOp::Ne, _) => Cond::Ne,
            (crate::value_plan::ComparisonOp::Lt, true) => Cond::Lt,
            (crate::value_plan::ComparisonOp::Lt, false) => Cond::Lo,
            (crate::value_plan::ComparisonOp::Gt, true) => Cond::Gt,
            (crate::value_plan::ComparisonOp::Gt, false) => Cond::Hi,
            (crate::value_plan::ComparisonOp::Le, true) => Cond::Le,
            (crate::value_plan::ComparisonOp::Le, false) => Cond::Ls,
            (crate::value_plan::ComparisonOp::Ge, true) => Cond::Ge,
            (crate::value_plan::ComparisonOp::Ge, false) => Cond::Hs,
        };
        self.mir.push(Aarch64Inst::Cset {
            dst: Operand::Virtual(dst),
            cond,
        });
        dst
    }

    fn emit_masked_shift_count_vreg(&mut self, rhs: VReg, mask: u64) -> VReg {
        if mask >= 31 {
            return rhs;
        }
        let mask_vreg = self.mir.alloc_vreg();
        self.mir.push(Aarch64Inst::MovImm {
            dst: Operand::Virtual(mask_vreg),
            imm: mask as i64,
        });
        let dst = self.mir.alloc_vreg();
        self.mir.push(Aarch64Inst::AndRR {
            dst: Operand::Virtual(dst),
            src1: Operand::Virtual(rhs),
            src2: Operand::Virtual(mask_vreg),
        });
        dst
    }

    fn lower_comparison(
        &mut self,
        op: crate::value_plan::ComparisonOp,
        lhs: crate::value_plan::MaterializedValue,
        rhs: crate::value_plan::MaterializedValue,
        policy: crate::value_plan::ValuePlan,
        leaf_types: &[Type],
        runtime_call: Option<crate::runtime_call_plan::RuntimeCallPlan>,
    ) -> VReg {
        match policy.comparison.expect("comparison plan") {
            crate::value_plan::ComparisonPreparation::Scalar { .. } => {
                self.lower_scalar_comparison(op, lhs.primary, rhs.primary, policy)
            }
            crate::value_plan::ComparisonPreparation::Unit => {
                let dst = self.mir.alloc_vreg();
                self.mir.push(Aarch64Inst::MovImm {
                    dst: Operand::Virtual(dst),
                    imm: matches!(op, crate::value_plan::ComparisonOp::Eq) as i64,
                });
                dst
            }
            crate::value_plan::ComparisonPreparation::StringContent { .. } => {
                let dst = self
                    .lower_runtime_call(runtime_call.expect("string equality runtime call plan"))
                    .primary;
                if matches!(op, crate::value_plan::ComparisonOp::Ne) {
                    self.mir.push(Aarch64Inst::EorImm {
                        dst: Operand::Virtual(dst),
                        src: Operand::Virtual(dst),
                        imm: 1,
                    });
                }
                dst
            }
            crate::value_plan::ComparisonPreparation::Aggregate { .. } => {
                crate::aggregate_eq::emit_aggregate_equality_plan(
                    self,
                    &lhs.slots,
                    &rhs.slots,
                    leaf_types,
                    matches!(op, crate::value_plan::ComparisonOp::Ne),
                )
            }
        }
    }

    fn lower_trap(
        &mut self,
        plan: crate::value_plan::TrapPlan,
    ) -> crate::value_plan::MaterializedValue {
        match plan {
            crate::value_plan::TrapPlan::Panic { call } => self.lower_runtime_call(call),
            crate::value_plan::TrapPlan::Assert { condition, call } => {
                let pass = self.mir.alloc_label();
                self.mir.push(Aarch64Inst::Cbnz {
                    src: Operand::Virtual(condition),
                    label: pass,
                });
                let result = self.lower_runtime_call(call);
                self.mir.push(Aarch64Inst::Label { id: pass });
                result
            }
        }
    }

    fn lower_option_intrinsic(
        &mut self,
        plan: &crate::value_plan::IntrinsicPlan,
        _intrinsic: crate::value_plan::OptionIntrinsic,
    ) -> crate::value_plan::MaterializedValue {
        self.lower_runtime_call(
            plan.runtime_call
                .clone()
                .expect("option intrinsic runtime call plan"),
        )
    }

    fn lower_intrinsic_plan(
        &mut self,
        plan: crate::value_plan::IntrinsicPlan,
    ) -> crate::value_plan::MaterializedValue {
        let operation = plan.operation;
        let mut slots = Vec::new();
        let primary = match operation {
            crate::value_plan::IntrinsicOperation::Option { intrinsic, .. } => {
                let result = self.lower_option_intrinsic(&plan, intrinsic);
                slots = result.slots;
                result.primary
            }
            crate::value_plan::IntrinsicOperation::RandomU32
            | crate::value_plan::IntrinsicOperation::RandomU64 => {
                self.lower_runtime_call(
                    plan.runtime_call
                        .expect("random intrinsic runtime call plan"),
                )
                .primary
            }
            crate::value_plan::IntrinsicOperation::ArgCount
            | crate::value_plan::IntrinsicOperation::ArgPtr
            | crate::value_plan::IntrinsicOperation::ArgLen
            | crate::value_plan::IntrinsicOperation::EnvCount
            | crate::value_plan::IntrinsicOperation::EnvPtr
            | crate::value_plan::IntrinsicOperation::EnvLen => {
                self.lower_runtime_call(
                    plan.runtime_call
                        .expect("process arg/env intrinsic runtime call plan"),
                )
                .primary
            }
            crate::value_plan::IntrinsicOperation::PtrToInt
            | crate::value_plan::IntrinsicOperation::IntToPtr => {
                let dst = self.mir.alloc_vreg();
                self.mir.push(Aarch64Inst::MovRR {
                    dst: Operand::Virtual(dst),
                    src: Operand::Virtual(plan.args[0].primary),
                });
                dst
            }
            // `@bitCast` (RUE-952) moves no bits: it copies the operand and
            // rebuilds only the bits above the shared width, which belong to the
            // source type's register image. The shared planner picked the form
            // from the target type; this leaf just spells it.
            crate::value_plan::IntrinsicOperation::BitCast(form) => {
                use crate::value_plan::BitCastForm;
                let dst = self.mir.alloc_vreg();
                self.mir.push(Aarch64Inst::MovRR {
                    dst: Operand::Virtual(dst),
                    src: Operand::Virtual(plan.args[0].primary),
                });
                let operand = Operand::Virtual(dst);
                match form {
                    BitCastForm::Move => {}
                    BitCastForm::Sign8 => self.mir.push(Aarch64Inst::Sxtb {
                        dst: operand,
                        src: operand,
                    }),
                    BitCastForm::Zero8 => self.mir.push(Aarch64Inst::Uxtb {
                        dst: operand,
                        src: operand,
                    }),
                    BitCastForm::Sign16 => self.mir.push(Aarch64Inst::Sxth {
                        dst: operand,
                        src: operand,
                    }),
                    BitCastForm::Zero16 => self.mir.push(Aarch64Inst::Uxth {
                        dst: operand,
                        src: operand,
                    }),
                    // The 64-bit shift pair clears bits 32..63 without needing a
                    // `uxtw`-shaped instruction in this instruction set.
                    BitCastForm::Zero32 => {
                        self.mir.push(Aarch64Inst::LslImm {
                            dst: operand,
                            src: operand,
                            imm: 32,
                        });
                        self.mir.push(Aarch64Inst::Lsr64Imm {
                            dst: operand,
                            src: operand,
                            imm: 32,
                        });
                    }
                }
                dst
            }
            crate::value_plan::IntrinsicOperation::PtrRead => {
                let ptr = plan.args[0].primary;
                let count = plan.result_slots;
                if let Some(map) = &plan.physical_slots {
                    // A compact enum pointee: load each internal slot from its
                    // physical byte position, extended into the slot-shaped vreg
                    // (RUE-1000).
                    slots = crate::agg_slots::load_enum_slots_through_ptr(self, ptr, map);
                    slots[0]
                } else if let Some(image) = &plan.dispatch_image {
                    // A heterogeneous compact aggregate pointee: dispatch on the
                    // runtime tag and load the active variant's image (RUE-1037).
                    slots = crate::agg_slots::load_dispatch_image(self, ptr, image);
                    slots[0]
                } else if count > 1 {
                    slots = crate::agg_slots::load_slots_through_ptr(self, ptr, count);
                    slots[0]
                } else {
                    let dst = self.mir.alloc_vreg();
                    if count != 0 {
                        // A narrow scalar pointee reads 1/2/4 physical bytes and
                        // extends into the slot-shaped vreg (RUE-989); a full-slot
                        // pointee keeps the eight-byte load.
                        if let Some(narrow) = plan.narrow_access {
                            self.mir.push(Aarch64Inst::NarrowLoadIndexed {
                                dst: Operand::Virtual(dst),
                                base: ptr,
                                offset: 0,
                                width: narrow.width,
                                signed: narrow.signed,
                            });
                        } else {
                            self.mir.push(Aarch64Inst::LdrIndexed {
                                dst: Operand::Virtual(dst),
                                base: ptr,
                            });
                        }
                    }
                    if count != 0 {
                        slots.push(dst);
                    }
                    dst
                }
            }
            crate::value_plan::IntrinsicOperation::PtrWrite => {
                let ptr = plan.args[0].primary;
                let value = &plan.args[1];
                if let Some(map) = &plan.physical_slots {
                    // A compact enum value: truncate each internal slot to its
                    // physical width at its compact byte offset (RUE-1000). The
                    // pointee's padding is zeroed first (ADR-0052 ruling 5).
                    let vals = if value.slots.is_empty() {
                        vec![value.primary]
                    } else {
                        value.slots.clone()
                    };
                    crate::agg_slots::store_enum_slots_through_ptr(
                        self,
                        &vals,
                        ptr,
                        map,
                        &plan.image_padding,
                    );
                } else if let Some(image) = &plan.dispatch_image {
                    // A heterogeneous compact aggregate value: zero the full image
                    // extent, then dispatch on the tag and store the active
                    // variant's leaves (RUE-1037).
                    let vals = if value.slots.is_empty() {
                        vec![value.primary]
                    } else {
                        value.slots.clone()
                    };
                    crate::agg_slots::store_dispatch_image(self, &vals, ptr, image);
                } else if value.slot_count == 0 {
                    // Zero-sized values have no bytes to write.
                } else if !value.slots.is_empty() {
                    crate::agg_slots::store_slots_through_ptr(self, &value.slots, ptr, 0);
                } else if let Some(narrow) = plan.narrow_access {
                    // A narrow scalar truncates to 1/2/4 physical bytes (RUE-989).
                    self.mir.push(Aarch64Inst::NarrowStoreIndexed {
                        src: Operand::Virtual(value.primary),
                        base: ptr,
                        offset: 0,
                        width: narrow.width,
                    });
                } else {
                    self.mir.push(Aarch64Inst::StrIndexed {
                        src: Operand::Virtual(value.primary),
                        base: ptr,
                    });
                }
                let dst = self.mir.alloc_vreg();
                self.mir.push(Aarch64Inst::MovImm {
                    dst: Operand::Virtual(dst),
                    imm: 0,
                });
                dst
            }
            crate::value_plan::IntrinsicOperation::PtrOffset => {
                let offset = plan.args[1].primary;
                let offset = match plan.args[1].integer_extension {
                    crate::value_plan::IntegerExtension::None => offset,
                    extension => {
                        let extended = self.mir.alloc_vreg();
                        self.mir.push(Aarch64Inst::MovRR {
                            dst: Operand::Virtual(extended),
                            src: Operand::Virtual(offset),
                        });
                        let dst = Operand::Virtual(extended);
                        let src = Operand::Virtual(extended);
                        match extension {
                            crate::value_plan::IntegerExtension::Sign8 => {
                                self.mir.push(Aarch64Inst::Sxtb { dst, src })
                            }
                            crate::value_plan::IntegerExtension::Zero8 => {
                                self.mir.push(Aarch64Inst::Uxtb { dst, src })
                            }
                            crate::value_plan::IntegerExtension::Sign16 => {
                                self.mir.push(Aarch64Inst::Sxth { dst, src })
                            }
                            crate::value_plan::IntegerExtension::Zero16 => {
                                self.mir.push(Aarch64Inst::Uxth { dst, src })
                            }
                            crate::value_plan::IntegerExtension::Sign32 => {
                                self.mir.push(Aarch64Inst::Sxtw { dst, src })
                            }
                            crate::value_plan::IntegerExtension::None => unreachable!(),
                        }
                        extended
                    }
                };
                let scaled =
                    allocation::lower_scale(self, offset, plan.scale.expect("ptr_offset scale"));
                let dst = self.mir.alloc_vreg();
                self.mir.push(Aarch64Inst::AddRR {
                    dst: Operand::Virtual(dst),
                    src1: Operand::Virtual(plan.args[0].primary),
                    src2: Operand::Virtual(scaled),
                });
                dst
            }
            // The unified allocation family and the bulk byte primitives are
            // pure runtime calls: the shared plan already carries their
            // operands in helper order (ADR-0059 Phase 3, RUE-961).
            crate::value_plan::IntrinsicOperation::Alloc => {
                self.lower_runtime_call(plan.runtime_call.expect("alloc runtime call plan"))
                    .primary
            }
            crate::value_plan::IntrinsicOperation::AllocZeroed => {
                self.lower_runtime_call(plan.runtime_call.expect("alloc-zeroed runtime call plan"))
                    .primary
            }
            crate::value_plan::IntrinsicOperation::Free => {
                self.lower_runtime_call(plan.runtime_call.expect("free runtime call plan"))
                    .primary
            }
            crate::value_plan::IntrinsicOperation::Realloc => {
                self.lower_runtime_call(plan.runtime_call.expect("realloc runtime call plan"))
                    .primary
            }
            crate::value_plan::IntrinsicOperation::Resize => {
                self.lower_runtime_call(plan.runtime_call.expect("resize runtime call plan"))
                    .primary
            }
            crate::value_plan::IntrinsicOperation::ByteCopy => {
                self.lower_runtime_call(plan.runtime_call.expect("byte-copy runtime call plan"))
                    .primary
            }
            crate::value_plan::IntrinsicOperation::ByteMove => {
                self.lower_runtime_call(plan.runtime_call.expect("byte-move runtime call plan"))
                    .primary
            }
            crate::value_plan::IntrinsicOperation::ByteSet => {
                self.lower_runtime_call(plan.runtime_call.expect("byte-set runtime call plan"))
                    .primary
            }
            crate::value_plan::IntrinsicOperation::PlaceAddress => {
                let place = plan.args[0]
                    .place
                    .as_ref()
                    .expect("raw intrinsic place plan");
                let dst = self.mir.alloc_vreg();
                crate::place_lower::lower_place_addr_plan(self, dst, place);
                dst
            }
            crate::value_plan::IntrinsicOperation::Debug => {
                self.lower_runtime_call(plan.runtime_call.expect("debug runtime call plan"))
                    .primary
            }
            crate::value_plan::IntrinsicOperation::Syscall => {
                let stack_space = ((plan.args.len() * 8 + 15) & !15) as i32;
                if stack_space > 0 {
                    self.mir.push(Aarch64Inst::SubImm {
                        dst: Operand::Physical(Reg::Sp),
                        src: Operand::Physical(Reg::Sp),
                        imm: stack_space,
                    });
                }
                for (index, arg) in plan.args.iter().enumerate() {
                    self.mir.push(Aarch64Inst::Str {
                        src: Operand::Virtual(arg.primary),
                        base: Reg::Sp,
                        offset: (index * 8) as i32,
                    });
                }
                let syscall_reg = if self.target.is_macho() {
                    Reg::X16
                } else {
                    Reg::X8
                };
                self.mir.push(Aarch64Inst::Ldr {
                    dst: Operand::Physical(syscall_reg),
                    base: Reg::Sp,
                    offset: 0,
                });
                for (index, reg) in [Reg::X0, Reg::X1, Reg::X2, Reg::X3, Reg::X4, Reg::X5]
                    .iter()
                    .enumerate()
                {
                    if index + 1 < plan.args.len() {
                        self.mir.push(Aarch64Inst::Ldr {
                            dst: Operand::Physical(*reg),
                            base: Reg::Sp,
                            offset: ((index + 1) * 8) as i32,
                        });
                    }
                }
                self.mir.push(if self.target.is_macho() {
                    Aarch64Inst::Svc { imm: 0x80 }
                } else {
                    Aarch64Inst::Svc { imm: 0 }
                });
                if self.target.is_macho() {
                    // Darwin reports syscall errors by setting carry and returning
                    // a positive errno in x0. Normalize that to the negative errno
                    // convention exposed by @syscall. Keep the flag consumer
                    // immediately after SVC: both instructions are scheduling
                    // barriers, so no flag-setting instruction can be moved between
                    // the kernel return and this test.
                    let success = self.mir.alloc_label();
                    self.mir.push(Aarch64Inst::BCond {
                        cond: Cond::Lo,
                        label: success,
                    });
                    self.mir.push(Aarch64Inst::Neg {
                        dst: Operand::Physical(Reg::X0),
                        src: Operand::Physical(Reg::X0),
                    });
                    self.mir.push(Aarch64Inst::Label { id: success });
                }
                if stack_space > 0 {
                    self.mir.push(Aarch64Inst::AddImm {
                        dst: Operand::Physical(Reg::Sp),
                        src: Operand::Physical(Reg::Sp),
                        imm: stack_space,
                    });
                }
                let dst = self.mir.alloc_vreg();
                self.mir.push(Aarch64Inst::MovRR {
                    dst: Operand::Virtual(dst),
                    src: Operand::Physical(Reg::X0),
                });
                dst
            }
        };
        crate::value_plan::MaterializedValue { primary, slots }
    }

    /// Lower a CFG value (instruction).
    /// Try to extract a power-of-two shift amount from a constant value.
    ///
    /// Returns `Some(shift_amount)` if the value is a constant that is a power of 2
    /// greater than 1, otherwise returns `None`.
    ///
    /// Used for strength reduction: `x * 2^n` can be lowered to `x << n`.
    fn emit_subword_range_check(
        &mut self,
        width: crate::value_plan::IntegerWidth,
        result_vreg: VReg,
        ok_label: LabelId,
    ) {
        match (width.bits, width.signed) {
            (8, false) => {
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
            (16, false) => {
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
            (8, true) => {
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
            (16, true) => {
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

    fn emit_overflow_check_add(
        &mut self,
        width: crate::value_plan::IntegerWidth,
        result_vreg: VReg,
        trap_call: crate::runtime_call_plan::RuntimeCallPlan,
    ) {
        let ok_label = self.mir.alloc_label();

        match (width.bits, width.signed) {
            // 32-bit and 64-bit unsigned: C=1 means overflow (carry out)
            // Branch to ok if C=0 (no overflow)
            (32 | 64, false) => {
                self.mir.push(Aarch64Inst::BCond {
                    cond: Cond::Lo, // Lo = C=0 (no carry)
                    label: ok_label,
                });
            }
            // 32-bit and 64-bit signed: V flag indicates overflow
            (32 | 64, true) => {
                self.mir.push(Aarch64Inst::Bvc { label: ok_label });
            }
            // Sub-word types: check if result fits in type's range
            (8 | 16, _) => {
                self.emit_subword_range_check(width, result_vreg, ok_label);
            }
            // Other types don't have arithmetic
            _ => return,
        }

        // Overflow occurred - call panic handler
        let _ = self.lower_runtime_call(trap_call.clone());
        self.mir.push(Aarch64Inst::Label { id: ok_label });
    }

    /// Emit overflow check for SUB based on the type.
    ///
    /// For ARM64 SUBS:
    /// - Signed: V flag indicates overflow
    /// - Unsigned: C=0 means borrow (underflow), C=1 means no borrow
    fn emit_overflow_check_sub(
        &mut self,
        width: crate::value_plan::IntegerWidth,
        result_vreg: VReg,
        trap_call: crate::runtime_call_plan::RuntimeCallPlan,
    ) {
        let ok_label = self.mir.alloc_label();

        match (width.bits, width.signed) {
            // 32-bit and 64-bit unsigned: C=0 means borrow (underflow)
            // Branch to ok if C=1 (no underflow)
            (32 | 64, false) => {
                self.mir.push(Aarch64Inst::BCond {
                    cond: Cond::Hs, // Hs = C=1 (no borrow)
                    label: ok_label,
                });
            }
            // 32-bit and 64-bit signed: V flag indicates overflow
            (32 | 64, true) => {
                self.mir.push(Aarch64Inst::Bvc { label: ok_label });
            }
            // Sub-word types: check if result fits in type's range
            (8 | 16, _) => {
                self.emit_subword_range_check(width, result_vreg, ok_label);
            }
            // Other types don't have arithmetic
            _ => return,
        }

        let _ = self.lower_runtime_call(trap_call.clone());
        self.mir.push(Aarch64Inst::Label { id: ok_label });
    }

    /// Emit overflow check for MUL based on the type.
    ///
    /// For multiplication, we need different approaches for signed vs unsigned:
    /// - Signed: Use SMULL (64-bit result), compare with sign-extended 32-bit
    /// - Unsigned: Use UMULL (64-bit result), check if high bits are non-zero
    fn emit_overflow_check_mul(
        &mut self,
        width: crate::value_plan::IntegerWidth,
        result_vreg: VReg,
        lhs_vreg: VReg,
        rhs_vreg: VReg,
        trap_call: crate::runtime_call_plan::RuntimeCallPlan,
    ) {
        let ok_label = self.mir.alloc_label();

        match (width.bits, width.signed) {
            // 32-bit signed: SMULL gives 64-bit result
            (32, true) => {
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
            (32, false) => {
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
            (64, true) => {
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
            (64, false) => {
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
            (8 | 16, _) => {
                // For sub-word, just do the multiply and check range
                self.mir.push(Aarch64Inst::MulRR {
                    dst: Operand::Virtual(result_vreg),
                    src1: Operand::Virtual(lhs_vreg),
                    src2: Operand::Virtual(rhs_vreg),
                });
                self.emit_subword_range_check(width, result_vreg, ok_label);
            }
            _ => return,
        }

        let _ = self.lower_runtime_call(trap_call.clone());
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
    fn emit_signed_div_overflow_check(
        &mut self,
        width: crate::value_plan::IntegerWidth,
        lhs_vreg: VReg,
        rhs_vreg: VReg,
        trap_call: crate::runtime_call_plan::RuntimeCallPlan,
    ) {
        let ok_label = self.mir.alloc_label();
        let is_64 = width.bits == 64;

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
        let (min_val, _) = crate::value_plan::integer_range(width);
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
        let _ = self.lower_runtime_call(trap_call.clone());
        self.mir.push(Aarch64Inst::Label { id: ok_label });
    }

    /// Emit overflow check for NEG based on the type.
    ///
    /// For NEGS (0 - x):
    /// - Signed: V flag indicates overflow (when negating MIN_VALUE)
    /// - Unsigned: Any non-zero value causes overflow (since 0 - x wraps)
    fn emit_overflow_check_neg(
        &mut self,
        width: crate::value_plan::IntegerWidth,
        result_vreg: VReg,
        trap_call: crate::runtime_call_plan::RuntimeCallPlan,
    ) {
        let ok_label = self.mir.alloc_label();

        match (width.bits, width.signed) {
            // Unsigned: NEGS sets C=0 for non-zero operands (which is overflow)
            // Branch to ok if C=1 (meaning operand was 0, no overflow)
            (32 | 64, false) => {
                self.mir.push(Aarch64Inst::BCond {
                    cond: Cond::Hs, // Hs = C=1
                    label: ok_label,
                });
            }
            // Signed: V flag indicates overflow
            (32 | 64, true) => {
                self.mir.push(Aarch64Inst::Bvc { label: ok_label });
            }
            // Sub-word unsigned types: only 0 is valid (negating to 0)
            (8 | 16, false) => {
                // Result must be 0 for no overflow
                self.mir.push(Aarch64Inst::Cbz {
                    src: Operand::Virtual(result_vreg),
                    label: ok_label,
                });
            }
            // Sub-word signed types: check if result fits in type's range
            (8 | 16, true) => {
                self.emit_subword_range_check(width, result_vreg, ok_label);
            }
            _ => return,
        }

        let _ = self.lower_runtime_call(trap_call.clone());
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

    /// Emit an aborting call to `__rue_panic(ptr, len)` for `@panic("msg")` and
    /// `@assert(cond, "msg")` (RUE-319). `msg_val` is the CFG value of the
    /// message `String`; its fat pointer supplies the `ptr`/`len` arguments
    /// (X0/X1). Never returns at runtime.
    fn emit_int_cast_check(
        &mut self,
        src_vreg: VReg,
        from_width: crate::value_plan::IntegerWidth,
        to_width: crate::value_plan::IntegerWidth,
        trap_call: crate::runtime_call_plan::RuntimeCallPlan,
    ) {
        let from_signed = from_width.signed;
        let to_signed = to_width.signed;
        let from_bits = from_width.bits;
        let to_bits = to_width.bits;

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
        let (min_val, max_val) = crate::value_plan::integer_range(to_width);

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
                    let _ = self.lower_runtime_call(trap_call.clone());
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
                    let _ = self.lower_runtime_call(trap_call.clone());
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
                    match from_bits {
                        8 => {
                            self.mir.push(Aarch64Inst::Sxtb {
                                dst: Operand::Virtual(sext_vreg),
                                src: Operand::Virtual(src_vreg),
                            });
                        }
                        16 => {
                            self.mir.push(Aarch64Inst::Sxth {
                                dst: Operand::Virtual(sext_vreg),
                                src: Operand::Virtual(src_vreg),
                            });
                        }
                        _ => {
                            self.mir.push(Aarch64Inst::Sxtw {
                                dst: Operand::Virtual(sext_vreg),
                                src: Operand::Virtual(src_vreg),
                            });
                        }
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
                let _ = self.lower_runtime_call(trap_call.clone());
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
                    let _ = self.lower_runtime_call(trap_call.clone());
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
                let _ = self.lower_runtime_call(trap_call.clone());
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
                    let _ = self.lower_runtime_call(trap_call.clone());
                    self.mir.push(Aarch64Inst::Label { id: ok_label });
                }
            }
        }
    }

    /// Get the min and max values for an integer type.
    /// Materialize the shift count masked to the operand's bit width into a
    /// fresh vreg (so a sub-word variable count >= the width wraps per spec).
    /// For 32/64-bit operands the hardware mask already matches.
    fn emit_subword_narrow(&mut self, vreg: VReg, ty: Type) {
        let dst = Operand::Virtual(vreg);
        let src = Operand::Virtual(vreg);
        match crate::value_plan::type_bits(ty) {
            8 if crate::value_plan::type_is_signed(ty) => {
                self.mir.push(Aarch64Inst::Sxtb { dst, src })
            }
            8 => self.mir.push(Aarch64Inst::Uxtb { dst, src }),
            16 if crate::value_plan::type_is_signed(ty) => {
                self.mir.push(Aarch64Inst::Sxth { dst, src })
            }
            16 => self.mir.push(Aarch64Inst::Uxth { dst, src }),
            _ => {}
        }
    }

    /// Re-narrow a sub-word wrapping `Add`/`Sub` result to its declared width
    /// (RUE-647). The flag-setting `Adds`/`Subs` are 32-bit ops that zero bits
    /// 32..63, so 32- and 64-bit results are already canonical; only 8/16-bit
    /// results need the low byte/halfword re-extended to match the canonical
    /// sign/zero extension of an in-range value.
    fn emit_wrap_narrow_subword(&mut self, width: crate::value_plan::IntegerWidth, vreg: VReg) {
        let dst = Operand::Virtual(vreg);
        let src = Operand::Virtual(vreg);
        match (width.bits, width.signed) {
            (8, true) => self.mir.push(Aarch64Inst::Sxtb { dst, src }),
            (8, false) => self.mir.push(Aarch64Inst::Uxtb { dst, src }),
            (16, true) => self.mir.push(Aarch64Inst::Sxth { dst, src }),
            (16, false) => self.mir.push(Aarch64Inst::Uxth { dst, src }),
            _ => {}
        }
    }

    /// Truncate a wrapping multiply result to its declared width (RUE-647).
    /// `MUL` is a 64-bit op whose upper bits carry the high half of the product,
    /// so every sub-64-bit width must be re-narrowed: 8/16-bit sign/zero-extend
    /// the low byte/halfword; a 32-bit signed result sign-extends the low word,
    /// and a 32-bit unsigned result zeroes the upper word (`lsl #32; lsr #32`)
    /// so a later `u32 -> u64` widening sees the correct zero-extended value.
    fn emit_wrap_narrow_mul(&mut self, width: crate::value_plan::IntegerWidth, vreg: VReg) {
        let dst = Operand::Virtual(vreg);
        let src = Operand::Virtual(vreg);
        match (width.bits, width.signed) {
            (8, true) => self.mir.push(Aarch64Inst::Sxtb { dst, src }),
            (8, false) => self.mir.push(Aarch64Inst::Uxtb { dst, src }),
            (16, true) => self.mir.push(Aarch64Inst::Sxth { dst, src }),
            (16, false) => self.mir.push(Aarch64Inst::Uxth { dst, src }),
            (32, true) => self.mir.push(Aarch64Inst::Sxtw { dst, src }),
            (32, false) => {
                self.mir.push(Aarch64Inst::LslImm { dst, src, imm: 32 });
                self.mir.push(Aarch64Inst::Lsr64Imm {
                    dst,
                    src: dst,
                    imm: 32,
                });
            }
            _ => {}
        }
    }

    /// Emit a comparison instruction.
    fn emit_terminator_plan(&mut self, plan: crate::terminator_plan::TerminatorPlan) {
        use crate::terminator_plan::{ReturnMode, ReturnValuePlan, TerminatorPlan};

        match plan {
            TerminatorPlan::Goto { edge } => {
                self.emit_edge_moves(&edge);
                if !edge.fallthrough {
                    self.mir.push(Aarch64Inst::B {
                        label: self.block_label(edge.target),
                    });
                }
            }
            TerminatorPlan::Branch {
                condition,
                then_edge,
                else_edge,
            } => {
                if then_edge.fallthrough {
                    let then_setup_label = self.mir.alloc_label();
                    self.mir.push(Aarch64Inst::Cbnz {
                        src: Operand::Virtual(condition),
                        label: then_setup_label,
                    });
                    self.emit_edge_moves(&else_edge);
                    if !else_edge.fallthrough {
                        self.mir.push(Aarch64Inst::B {
                            label: self.block_label(else_edge.target),
                        });
                    }
                    self.mir.push(Aarch64Inst::Label {
                        id: then_setup_label,
                    });
                    self.emit_edge_moves(&then_edge);
                } else {
                    let else_setup_label = self.mir.alloc_label();
                    self.mir.push(Aarch64Inst::Cbz {
                        src: Operand::Virtual(condition),
                        label: else_setup_label,
                    });
                    self.emit_edge_moves(&then_edge);
                    if !then_edge.fallthrough {
                        self.mir.push(Aarch64Inst::B {
                            label: self.block_label(then_edge.target),
                        });
                    }
                    self.mir.push(Aarch64Inst::Label {
                        id: else_setup_label,
                    });
                    self.emit_edge_moves(&else_edge);
                    if !else_edge.fallthrough {
                        self.mir.push(Aarch64Inst::B {
                            label: self.block_label(else_edge.target),
                        });
                    }
                }
            }
            TerminatorPlan::Switch {
                scrutinee,
                width,
                cases,
                default,
            } => {
                for case in cases {
                    let case_vreg = self.mir.alloc_vreg();
                    self.mir.push(Aarch64Inst::MovImm {
                        dst: Operand::Virtual(case_vreg),
                        imm: case.value,
                    });
                    if width.bits == 64 {
                        self.mir.push(Aarch64Inst::Cmp64RR {
                            src1: Operand::Virtual(scrutinee),
                            src2: Operand::Virtual(case_vreg),
                        });
                    } else {
                        self.mir.push(Aarch64Inst::CmpRR {
                            src1: Operand::Virtual(scrutinee),
                            src2: Operand::Virtual(case_vreg),
                        });
                    }
                    self.mir.push(Aarch64Inst::BCond {
                        cond: Cond::Eq,
                        label: self.block_label(case.target),
                    });
                }
                self.mir.push(Aarch64Inst::B {
                    label: self.block_label(default),
                });
            }
            TerminatorPlan::Return { mode } => match mode {
                ReturnMode::Exit { call } => {
                    let _ = self.lower_runtime_call(call);
                }
                ReturnMode::Function { value } => match value {
                    ReturnValuePlan::ZeroSized => self.mir.push(Aarch64Inst::Ret),
                    ReturnValuePlan::Scalar { value } => {
                        self.mir.push(Aarch64Inst::MovRR {
                            dst: Operand::Physical(Reg::X0),
                            src: Operand::Virtual(value),
                        });
                        self.mir.push(Aarch64Inst::Ret);
                    }
                    ReturnValuePlan::Aggregate { slots, return_plan } => {
                        if return_plan.uses_sret() {
                            let return_ty = self.ctx.cfg.return_type();
                            match crate::types::aggregate_physical_slot_map(
                                self.ctx.type_pool,
                                return_ty,
                            ) {
                                Some(map) => {
                                    // The sret image is written compact; its padding
                                    // is zeroed first (ADR-0052 ruling 5).
                                    let padding =
                                        self.ctx.type_pool.compact_image_padding_ranges(return_ty);
                                    crate::agg_slots::store_slots_to_sret_compact(
                                        self, &slots, &map, &padding,
                                    )
                                }
                                None => match crate::types::aggregate_dispatch_image(
                                    self.ctx.type_pool,
                                    return_ty,
                                ) {
                                    // Heterogeneous compact aggregate return (RUE-1037):
                                    // write the sret image with a per-variant tag dispatch.
                                    Some(image) => crate::agg_slots::store_dispatch_image_to_sret(
                                        self, &slots, &image,
                                    ),
                                    None => crate::agg_slots::store_slots_to_sret(self, &slots),
                                },
                            }
                        } else {
                            for (index, slot) in slots.iter().enumerate() {
                                if index < RET_REGS.len() {
                                    self.mir.push(Aarch64Inst::MovRR {
                                        dst: Operand::Physical(RET_REGS[index]),
                                        src: Operand::Virtual(*slot),
                                    });
                                }
                            }
                        }
                        self.mir.push(Aarch64Inst::Ret);
                    }
                },
            },
            TerminatorPlan::Unreachable => self.mir.push(Aarch64Inst::Brk),
        }
    }

    fn emit_edge_moves(&mut self, edge: &crate::terminator_plan::EdgePlan) {
        for movement in &edge.moves {
            self.mir.push(Aarch64Inst::MovRR {
                dst: Operand::Virtual(movement.destination),
                src: Operand::Virtual(movement.source),
            });
        }
    }

    /// Get the vreg for a CFG value.
    fn get_vreg(&mut self, value: CfgValue) -> VReg {
        if let Some(&vreg) = self.value_map.get(&value) {
            return vreg;
        }

        // Not yet lowered - lower it now
        let ctx = self.ctx;
        crate::value_plan::lower_value(&ctx, self, value);

        self.value_map
            .get(&value)
            .copied()
            .expect("value should have been lowered")
    }
}

impl crate::terminator_plan::TerminatorAdapter for CfgLower<'_> {
    fn materialize_value(
        &mut self,
        value: CfgValue,
        plan: crate::value_plan::ValuePlan,
    ) -> crate::value_plan::MaterializedValue {
        let primary = self.get_vreg(value);
        let slots = if plan.shape.requires_complete_slots() {
            self.require_aggregate_slots(value)
        } else {
            Vec::new()
        };
        crate::value_plan::MaterializedValue { primary, slots }
    }

    fn materialize_block_param(
        &mut self,
        target: BlockId,
        param_index: u32,
        value: CfgValue,
        plan: crate::value_plan::ValuePlan,
    ) -> crate::value_plan::MaterializedValue {
        let primary = self.block_param_vregs[&(target, param_index)];
        let slots = if plan.shape.requires_complete_slots() {
            let slots = self
                .struct_slot_vregs
                .get(&value)
                .cloned()
                .expect("aggregate block parameter slots should be preallocated");
            plan.assert_complete_slots(slots.len());
            slots
        } else {
            Vec::new()
        };
        crate::value_plan::MaterializedValue { primary, slots }
    }

    fn emit_block_label(&mut self, block: BlockId) {
        self.mir.push(Aarch64Inst::Label {
            id: self.block_label(block),
        });
    }

    fn emit_terminator(&mut self, plan: crate::terminator_plan::TerminatorPlan) {
        self.emit_terminator_plan(plan);
    }
}

impl crate::terminator_plan::CfgLowerAdapter for CfgLower<'_> {
    fn preload_by_ref_params(&mut self) {
        self.preload_by_ref_param_ptrs();
        // Unmarshal each by-value indirect compact aggregate parameter from the
        // homed pointer into its frame slots at entry (RUE-1005), so field
        // projection and whole-value reads see the correct decomposition.
        for (base_slot, map) in crate::value_plan::indirect_value_params(&self.ctx) {
            crate::agg_slots::unmarshal_indirect_value_param(self, base_slot, &map);
        }
        // Heterogeneous by-value indirect params unmarshal with a tag dispatch
        // (RUE-1037).
        for (base_slot, image) in crate::value_plan::indirect_value_params_dispatch(&self.ctx) {
            crate::agg_slots::unmarshal_indirect_value_param_dispatch(self, base_slot, &image);
        }
    }

    fn prepare_block_param(&mut self, block: BlockId, index: u32, value: CfgValue, ty: Type) {
        let vreg = self.mir.alloc_vreg();
        self.block_param_vregs.insert((block, index), vreg);
        self.value_map.insert(value, vreg);
        crate::agg_slots::preallocate_block_param_slots(self, value, ty, vreg);
    }

    fn value_description(&self, value: CfgValue) -> String {
        let inst = self.ctx.cfg.get_inst(value);
        crate::cfg_lower::format_cfg_inst_data_with_interner(
            self.ctx.cfg,
            &inst.data,
            self.interner,
        )
    }

    fn instruction_count(&self) -> usize {
        self.mir.inst_count()
    }

    fn instruction_strings(&self, range: std::ops::Range<usize>) -> Vec<String> {
        self.mir.instructions()[range]
            .iter()
            .map(ToString::to_string)
            .collect()
    }

    fn value_rationale(&self, kind: crate::value_plan::ValueKind, ty: Type) -> Option<String> {
        self.get_lowering_rationale(kind, ty)
    }

    fn terminator_rationale(
        &self,
        plan: &crate::terminator_plan::TerminatorPlan,
    ) -> Option<String> {
        use crate::terminator_plan::{ReturnMode, TerminatorPlan};
        match plan {
            TerminatorPlan::Branch { .. } => Some("Compare and branch".to_string()),
            TerminatorPlan::Return {
                mode: ReturnMode::Exit { .. },
            } => Some("Main function: return value becomes exit code".to_string()),
            TerminatorPlan::Return {
                mode: ReturnMode::Function { value },
            } if !matches!(value, crate::terminator_plan::ReturnValuePlan::ZeroSized) => {
                Some("Return value in X0 (AAPCS64)".to_string())
            }
            TerminatorPlan::Switch { cases, .. } => {
                Some(format!("Linear scan through {} cases", cases.len()))
            }
            _ => None,
        }
    }
}

impl crate::value_plan::ValueLowerAdapter for CfgLower<'_> {
    fn value_is_lowered(&self, value: CfgValue) -> bool {
        self.value_map.contains_key(&value)
    }
    fn reserve_value_result(&mut self) -> VReg {
        self.mir.alloc_vreg()
    }
    fn resolve_symbol(&self, symbol: lasso::Spur) -> String {
        self.symbols.resolve(self.interner.resolve(&symbol))
    }
    fn resolve_intrinsic_symbol(&self, symbol: lasso::Spur) -> String {
        self.interner.resolve(&symbol).to_owned()
    }
    fn resolve_named_symbol(&self, symbol: &str) -> String {
        self.symbols.resolve(symbol)
    }
    fn is_foreign_symbol(&self, machine_symbol: &str) -> bool {
        self.symbols.is_foreign(machine_symbol)
    }
    fn target_c_flavor(&self) -> rue_air::TargetCAbiFlavor {
        rue_air::TargetCAbiFlavor::Aapcs64
    }
    fn call_arg_register_budget(&self) -> usize {
        ARG_REGS.len()
    }
    fn return_register_budget(&self) -> u32 {
        RET_REGS.len() as u32
    }
    fn emit_value(
        &mut self,
        plan: crate::value_plan::ValueEmissionPlan,
    ) -> crate::value_plan::ValueResult {
        self.lower_residual_value(plan)
    }
    fn emit_call(&mut self, plan: crate::call_plan::CallPlan) -> crate::value_plan::ValueResult {
        crate::value_plan::ValueResult::Materialized(self.lower_call_plan(plan))
    }
    fn emit_foreign_call(
        &mut self,
        inputs: crate::foreign_call::ForeignCallInputs,
        result: VReg,
    ) -> crate::value_plan::ValueResult {
        crate::value_plan::ValueResult::Materialized(self.lower_foreign_call(inputs, result))
    }
    fn emit_runtime_call(
        &mut self,
        plan: crate::runtime_call_plan::RuntimeCallPlan,
    ) -> crate::value_plan::ValueResult {
        crate::value_plan::ValueResult::Materialized(self.lower_runtime_call(plan))
    }
    fn emit_intrinsic(
        &mut self,
        plan: crate::value_plan::IntrinsicPlan,
    ) -> crate::value_plan::ValueResult {
        crate::value_plan::ValueResult::Materialized(self.lower_intrinsic_plan(plan))
    }
    fn emit_checked_arithmetic(
        &mut self,
        plan: crate::value_plan::ArithmeticPlan,
    ) -> crate::value_plan::ValueResult {
        crate::value_plan::ValueResult::Materialized(self.lower_checked_arithmetic(plan))
    }
    fn emit_trap(&mut self, plan: crate::value_plan::TrapPlan) -> crate::value_plan::ValueResult {
        crate::value_plan::ValueResult::Materialized(self.lower_trap(plan))
    }
    fn cache_value(&mut self, value: CfgValue, result: crate::value_plan::MaterializedValue) {
        self.value_map.insert(value, result.primary);
        if !result.slots.is_empty() {
            self.struct_slot_vregs.insert(value, result.slots);
        }
    }
}

impl CfgLower<'_> {
    /// Materialize `ptr + byte_offset` for a narrow access that encodes no
    /// offset, returning `ptr` unchanged when the offset is zero (RUE-1000).
    fn narrow_ptr_base(&mut self, ptr: VReg, byte_offset: i32) -> VReg {
        if byte_offset == 0 {
            return ptr;
        }
        let base = self.mir.alloc_vreg();
        self.mir.push(Aarch64Inst::AddImm {
            dst: Operand::Virtual(base),
            src: Operand::Virtual(ptr),
            imm: byte_offset,
        });
        base
    }
}

/// Whether an AArch64 narrow load/store can fold `byte_offset` into its scaled
/// `imm12` addressing mode (RUE-1079). `ldrb`/`strb` scale the immediate by 1,
/// `ldrh`/`strh` by 2, and `ldr w`/`str w` by 4, so the byte offset is encodable
/// iff it is non-negative, an exact multiple of `width`, and its scaled index
/// fits the unsigned 12-bit immediate (`<= 0xFFF`). Anything else — a negative,
/// misaligned, or too-large offset — is NOT foldable and must fall back to
/// materializing `base + offset` in a register (correctness first).
fn aarch64_narrow_offset_encodable(byte_offset: i32, width: u8) -> bool {
    let width = width as i32;
    byte_offset >= 0 && byte_offset % width == 0 && byte_offset / width <= 0xFFF
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
    fn emit_narrow_store_through_ptr(
        &mut self,
        src: VReg,
        ptr: VReg,
        byte_offset: i32,
        access: crate::types::NarrowScalar,
    ) {
        // Fold the byte offset into the store's scaled imm12 when the scaled
        // form can encode it (RUE-1079); otherwise materialize `base + offset`
        // and address `[base]` with offset 0 — always correct (RUE-1000).
        if aarch64_narrow_offset_encodable(byte_offset, access.width) {
            self.mir.push(Aarch64Inst::NarrowStoreIndexed {
                src: Operand::Virtual(src),
                base: ptr,
                offset: byte_offset,
                width: access.width,
            });
        } else {
            let base = self.narrow_ptr_base(ptr, byte_offset);
            self.mir.push(Aarch64Inst::NarrowStoreIndexed {
                src: Operand::Virtual(src),
                base,
                offset: 0,
                width: access.width,
            });
        }
    }
    fn emit_narrow_load_through_ptr(
        &mut self,
        dst: VReg,
        ptr: VReg,
        byte_offset: i32,
        access: crate::types::NarrowScalar,
    ) {
        // Fold when the scaled imm12 form can encode the offset, else materialize
        // (RUE-1079); see `emit_narrow_store_through_ptr`.
        if aarch64_narrow_offset_encodable(byte_offset, access.width) {
            self.mir.push(Aarch64Inst::NarrowLoadIndexed {
                dst: Operand::Virtual(dst),
                base: ptr,
                offset: byte_offset,
                width: access.width,
                signed: access.signed,
            });
        } else {
            let base = self.narrow_ptr_base(ptr, byte_offset);
            self.mir.push(Aarch64Inst::NarrowLoadIndexed {
                dst: Operand::Virtual(dst),
                base,
                offset: 0,
                width: access.width,
                signed: access.signed,
            });
        }
    }
    fn emit_zero_vreg(&mut self) -> VReg {
        let dst = self.mir.alloc_vreg();
        self.mir.push(Aarch64Inst::MovImm {
            dst: Operand::Virtual(dst),
            imm: 0,
        });
        dst
    }
    fn emit_set_zero(&mut self, dst: VReg) {
        self.mir.push(Aarch64Inst::MovImm {
            dst: Operand::Virtual(dst),
            imm: 0,
        });
    }
    fn alloc_marshal_label(&mut self) -> LabelId {
        self.mir.alloc_label()
    }
    fn emit_marshal_branch_if_tag_ne(&mut self, tag: VReg, discriminant: u64, label: LabelId) {
        self.mir.push(Aarch64Inst::CmpImm {
            src: Operand::Virtual(tag),
            imm: discriminant as i32,
        });
        self.mir.push(Aarch64Inst::BCond {
            cond: Cond::Ne,
            label,
        });
    }
    fn emit_marshal_jump(&mut self, label: LabelId) {
        self.mir.push(Aarch64Inst::B { label });
    }
    fn emit_marshal_label(&mut self, label: LabelId) {
        self.mir.push(Aarch64Inst::Label { id: label });
    }
}

impl crate::place_lower::PlaceLowerBackend for CfgLower<'_> {
    fn ensure_by_ref_param_ptr(&mut self, param_slot: u32) -> VReg {
        CfgLower::ensure_by_ref_param_ptr(self, param_slot)
    }

    fn emit_frame_addr(&mut self, dst: VReg, slot: u32) {
        let offset = self.ctx.local_offset(slot);
        self.mir.push(Aarch64Inst::AddImm {
            dst: Operand::Virtual(dst),
            src: Operand::Physical(Reg::Fp),
            imm: offset,
        });
    }

    fn emit_addr_add(&mut self, dst: VReg, rhs: VReg) {
        self.mir.push(Aarch64Inst::AddRR {
            dst: Operand::Virtual(dst),
            src1: Operand::Virtual(dst),
            src2: Operand::Virtual(rhs),
        });
    }

    fn emit_addr_add_imm(&mut self, dst: VReg, byte_offset: i32) {
        self.mir.push(Aarch64Inst::AddImm {
            dst: Operand::Virtual(dst),
            src: Operand::Virtual(dst),
            imm: byte_offset,
        });
    }

    fn emit_scale_index_bytes(&mut self, scaled: VReg, plan: crate::allocation::ScalePlan) {
        <Self as crate::allocation::ScaleBackend>::emit_scale(self, scaled, scaled, plan);
    }

    fn emit_zero_sized_place(&mut self, dst: VReg) {
        self.mir.push(Aarch64Inst::MovImm {
            dst: Operand::Virtual(dst),
            imm: 0,
        });
    }

    fn emit_zero_sized_place_addr(&mut self, dst: VReg) {
        self.mir.push(Aarch64Inst::MovImm {
            dst: Operand::Virtual(dst),
            imm: crate::place_lower::ZERO_SIZED_PLACE_ADDR,
        });
    }

    fn emit_load_ptr_base(&mut self, dst: VReg, ptr: VReg) {
        self.mir.push(Aarch64Inst::LdrIndexed {
            dst: Operand::Virtual(dst),
            base: ptr,
        });
    }
}

impl crate::allocation::BoundsCheckBackend for CfgLower<'_> {
    fn alloc_bounds_length(&mut self, length: u64) -> VReg {
        let vreg = self.mir.alloc_vreg();
        self.mir.push(Aarch64Inst::MovImm {
            dst: Operand::Virtual(vreg),
            imm: length as i64,
        });
        vreg
    }

    fn emit_bounds_compare(&mut self, index: VReg, length: VReg) {
        self.mir.push(Aarch64Inst::Cmp64RR {
            src1: Operand::Virtual(index),
            src2: Operand::Virtual(length),
        });
    }

    fn alloc_bounds_label(&mut self) -> LabelId {
        self.mir.alloc_label()
    }

    fn emit_bounds_branch(
        &mut self,
        condition: crate::allocation::BoundsCondition,
        label: LabelId,
    ) {
        match condition {
            crate::allocation::BoundsCondition::UnsignedIndexLessThanLength => {
                self.mir.push(Aarch64Inst::BCond {
                    cond: Cond::Lo,
                    label,
                });
            }
        }
    }

    fn emit_bounds_trap(
        &mut self,
        trap: crate::allocation::BoundsTrap,
        call: crate::runtime_call_plan::RuntimeCallPlan,
    ) {
        match trap {
            crate::allocation::BoundsTrap::IndexOutOfBounds => {
                let _ = self.lower_runtime_call(call);
            }
        }
    }

    fn emit_bounds_label(&mut self, label: LabelId) {
        self.mir.push(Aarch64Inst::Label { id: label });
    }
}

impl crate::allocation::ScaleBackend for CfgLower<'_> {
    fn alloc_scale_result(&mut self) -> VReg {
        self.mir.alloc_vreg()
    }

    fn emit_scale(&mut self, dst: VReg, src: VReg, plan: crate::allocation::ScalePlan) {
        use crate::allocation::{OverflowBehavior, ScaleKind, ScalePurpose};

        match (plan.purpose, plan.overflow) {
            (ScalePurpose::IndexOffset, OverflowBehavior::Wrap) => match plan.kind {
                ScaleKind::Zero => self.mir.push(Aarch64Inst::MovImm {
                    dst: Operand::Virtual(dst),
                    imm: 0,
                }),
                ScaleKind::Identity => self.mir.push(Aarch64Inst::MovRR {
                    dst: Operand::Virtual(dst),
                    src: Operand::Virtual(src),
                }),
                ScaleKind::Constant(8) => self.mir.push(Aarch64Inst::LslImm {
                    dst: Operand::Virtual(dst),
                    src: Operand::Virtual(src),
                    imm: 3,
                }),
                ScaleKind::Constant(bytes) => {
                    let stride = self.mir.alloc_vreg();
                    self.mir.push(Aarch64Inst::MovImm {
                        dst: Operand::Virtual(stride),
                        imm: bytes as i64,
                    });
                    self.mir.push(Aarch64Inst::MulRR {
                        dst: Operand::Virtual(dst),
                        src1: Operand::Virtual(src),
                        src2: Operand::Virtual(stride),
                    });
                }
            },
            (ScalePurpose::PointerOffset, OverflowBehavior::Wrap)
            | (ScalePurpose::AllocationSize, OverflowBehavior::Trap) => match plan.kind {
                ScaleKind::Zero => self.mir.push(Aarch64Inst::MovImm {
                    dst: Operand::Virtual(dst),
                    imm: 0,
                }),
                ScaleKind::Identity => self.mir.push(Aarch64Inst::MovRR {
                    dst: Operand::Virtual(dst),
                    src: Operand::Virtual(src),
                }),
                ScaleKind::Constant(bytes) => {
                    let stride = self.mir.alloc_vreg();
                    self.mir.push(Aarch64Inst::MovImm {
                        dst: Operand::Virtual(stride),
                        imm: bytes as i64,
                    });
                    self.mir.push(Aarch64Inst::MulRR {
                        dst: Operand::Virtual(dst),
                        src1: Operand::Virtual(src),
                        src2: Operand::Virtual(stride),
                    });
                    if plan.purpose == ScalePurpose::AllocationSize {
                        let high_vreg = self.mir.alloc_vreg();
                        self.mir.push(Aarch64Inst::UmulhRR {
                            dst: Operand::Virtual(high_vreg),
                            src1: Operand::Virtual(src),
                            src2: Operand::Virtual(stride),
                        });
                        let ok_label = self.mir.alloc_label();
                        self.mir.push(Aarch64Inst::Cbz {
                            src: Operand::Virtual(high_vreg),
                            label: ok_label,
                        });
                        let _ = self.lower_runtime_call(
                            crate::runtime_call_plan::RuntimeCallPlan::no_args(
                                rue_runtime_abi::RuntimeHelperId::Overflow,
                            ),
                        );
                        self.mir.push(Aarch64Inst::Label { id: ok_label });
                    }
                }
            },
            (ScalePurpose::IndexOffset, OverflowBehavior::Trap)
            | (ScalePurpose::PointerOffset, OverflowBehavior::Trap)
            | (ScalePurpose::AllocationSize, OverflowBehavior::Wrap) => {
                panic!("invalid shared scaling plan: {plan:?}")
            }
        }
    }
}

impl crate::aggregate_eq::AggregateEqPlanBackend for CfgLower<'_> {
    fn alloc_vreg(&mut self) -> VReg {
        self.mir.alloc_vreg()
    }
    fn emit_bool_const(&mut self, dst: VReg, value: bool) {
        self.mir.push(Aarch64Inst::MovImm {
            dst: Operand::Virtual(dst),
            imm: value as i64,
        });
    }
    fn emit_slot_eq(&mut self, dst: VReg, lhs: VReg, rhs: VReg, wide: bool) {
        self.mir.push(if wide {
            Aarch64Inst::Cmp64RR {
                src1: Operand::Virtual(lhs),
                src2: Operand::Virtual(rhs),
            }
        } else {
            Aarch64Inst::CmpRR {
                src1: Operand::Virtual(lhs),
                src2: Operand::Virtual(rhs),
            }
        });
        self.mir.push(Aarch64Inst::Cset {
            dst: Operand::Virtual(dst),
            cond: Cond::Eq,
        });
    }
    fn emit_bool_and(&mut self, acc: VReg, rhs: VReg) {
        self.mir.push(Aarch64Inst::AndRR {
            dst: Operand::Virtual(acc),
            src1: Operand::Virtual(acc),
            src2: Operand::Virtual(rhs),
        });
    }
    fn emit_bool_not(&mut self, value: VReg) {
        self.mir.push(Aarch64Inst::EorImm {
            dst: Operand::Virtual(value),
            src: Operand::Virtual(value),
            imm: 1,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rue_air::{Sema, SemaMetadata};
    use rue_cfg::CfgBuilder;
    use rue_error::PreviewFeatures;
    use rue_lexer::Lexer;
    use rue_parser::Parser;
    use rue_rir::AstGen;

    #[test]
    fn physical_return_register_roster_matches_the_abi_kernel_budget() {
        assert_eq!(
            RET_REGS.len() as u32,
            rue_air::native_return_register_budget(rue_target::Arch::Aarch64),
            "the backend's return-register roster and the classification \
             kernel's budget must agree"
        );
    }

    fn lower_function_to_mir(source: &str, function_name: &str) -> Aarch64Mir {
        lower_function_to_mir_with_preview(source, function_name, PreviewFeatures::new())
    }

    fn lower_function_to_mir_with_preview(
        source: &str,
        function_name: &str,
        preview: PreviewFeatures,
    ) -> Aarch64Mir {
        let lexer = Lexer::new(source);
        let (tokens, interner) = lexer.tokenize().unwrap();
        let parser = Parser::new(tokens, interner);
        let (ast, mut interner) = parser.parse().unwrap();

        let mut astgen = AstGen::with_symbol_normalizer(&interner, |symbol| symbol);
        astgen.append_items(&ast.items);
        let rir = astgen.finish();

        let sema = Sema::new_synthetic(&rir, &mut interner, preview);
        let output = sema.analyze_all_for_test().unwrap();

        let symbol = SemaMetadata::synthetic_root_function_symbol(function_name);
        let func = output
            .functions
            .iter()
            .find(|func| func.name == symbol)
            .expect("requested test function should exist");
        let type_pool = &output.type_pool;
        let cfg_output = CfgBuilder::build(
            &func.air,
            func.num_locals,
            func.num_param_slots,
            &func.name,
            type_pool,
            func.param_modes.clone(),
            &interner,
            func.allow_unreachable_code,
            func.callable_kind,
        );

        // Use host target for tests
        CfgLower::new(
            cfg_output.cfg.as_ref().unwrap(),
            type_pool,
            &interner,
            Target::host().expect("test lowering requires a supported Rue host target"),
        )
        .lower()
        .expect("test lowering should succeed")
    }

    fn try_lower_first_fn(
        source: &str,
        preview: PreviewFeatures,
    ) -> rue_error::CompileResult<Aarch64Mir> {
        try_lower_named_fn(source, preview, None)
    }

    /// Lower `name` with the pipeline's real parameter storage plan applied
    /// (RUE-1170), as production lowering does.
    fn lower_named_fn_with_plan(source: &str, name: &str) -> Aarch64Mir {
        let lexer = Lexer::new(source);
        let (tokens, interner) = lexer.tokenize().unwrap();
        let parser = Parser::new(tokens, interner);
        let (ast, mut interner) = parser.parse().unwrap();
        let mut astgen = AstGen::with_symbol_normalizer(&interner, |symbol| symbol);
        astgen.append_items(&ast.items);
        let rir = astgen.finish();
        let output = Sema::new_synthetic(&rir, &mut interner, PreviewFeatures::new())
            .analyze_all_for_test()
            .unwrap();
        let symbol = SemaMetadata::synthetic_root_function_symbol(name);
        let func = output
            .functions
            .iter()
            .find(|f| f.name == symbol)
            .unwrap_or_else(|| panic!("no function named `{name}`"));
        let type_pool = &output.type_pool;
        let cfg_output = CfgBuilder::build(
            &func.air,
            func.num_locals,
            func.num_param_slots,
            &func.name,
            type_pool,
            func.param_modes.clone(),
            &interner,
            func.allow_unreachable_code,
            func.callable_kind,
        );
        let cfg = cfg_output.cfg.as_ref().unwrap();
        let plan = crate::param_storage::ParamStoragePlan::plan(
            cfg,
            type_pool,
            false,
            ARG_REGS.len() as u32,
        );
        CfgLower::new(cfg, type_pool, &interner, Target::Aarch64Linux)
            .with_param_storage(&plan)
            .lower()
            .unwrap()
    }

    /// RUE-1170: read-only scalar register arguments are entry-copied out of
    /// their incoming registers instead of being reloaded from frame homes.
    #[test]
    fn register_only_params_entry_copy_and_never_load_from_the_frame() {
        let mir = lower_named_fn_with_plan(
            "fn both(a: i64, b: i64) -> i64 { a + b } \
             fn main() -> i32 { let _ = both(1, 2); 0 }",
            "both",
        );
        let insts = mir.instructions();
        assert!(
            matches!(
                insts[0],
                Aarch64Inst::MovRR {
                    src: Operand::Physical(Reg::X0),
                    dst: Operand::Virtual(_),
                }
            ),
            "first instruction must copy x0 into a vreg, got {:?}",
            insts[0]
        );
        assert!(
            matches!(
                insts[1],
                Aarch64Inst::MovRR {
                    src: Operand::Physical(Reg::X1),
                    dst: Operand::Virtual(_),
                }
            ),
            "second instruction must copy x1 into a vreg, got {:?}",
            insts[1]
        );
        assert!(
            !insts
                .iter()
                .any(|inst| matches!(inst, Aarch64Inst::Ldr { base: Reg::Fp, .. })),
            "register-only parameters must not be reloaded from the frame:\n{mir}"
        );
    }

    /// RUE-1170: an unused register argument produces no entry copy at all.
    #[test]
    fn unused_register_params_produce_no_code() {
        let mir = lower_named_fn_with_plan(
            "fn pick(a: i64, b: i64) -> i64 { 42 } \
             fn main() -> i32 { let _ = pick(1, 2); 0 }",
            "pick",
        );
        assert!(
            !mir.instructions().iter().any(|inst| matches!(
                inst,
                Aarch64Inst::MovRR {
                    src: Operand::Physical(Reg::X0 | Reg::X1),
                    ..
                }
            )),
            "unused register arguments must not be copied:\n{mir}"
        );
    }

    /// Lower a specific function by name (or the first function when `None`),
    /// so tests can exercise a callee whose caller sorts ahead of it.
    fn try_lower_named_fn(
        source: &str,
        preview: PreviewFeatures,
        name: Option<&str>,
    ) -> rue_error::CompileResult<Aarch64Mir> {
        let lexer = Lexer::new(source);
        let (tokens, interner) = lexer.tokenize().unwrap();
        let parser = Parser::new(tokens, interner);
        let (ast, mut interner) = parser.parse().unwrap();
        let mut astgen = AstGen::with_symbol_normalizer(&interner, |symbol| symbol);
        astgen.append_items(&ast.items);
        let rir = astgen.finish();
        let output = Sema::new_synthetic(&rir, &mut interner, preview)
            .analyze_all_for_test()
            .unwrap();
        let func = match name {
            Some(name) => {
                let symbol = SemaMetadata::synthetic_root_function_symbol(name);
                output
                    .functions
                    .iter()
                    .find(|f| f.name == symbol)
                    .unwrap_or_else(|| panic!("no function named `{name}`"))
            }
            None => &output.functions[0],
        };
        let type_pool = &output.type_pool;
        let cfg_output = CfgBuilder::build(
            &func.air,
            func.num_locals,
            func.num_param_slots,
            &func.name,
            type_pool,
            func.param_modes.clone(),
            &interner,
            func.allow_unreachable_code,
            func.callable_kind,
        );
        CfgLower::new(
            cfg_output.cfg.as_ref().unwrap(),
            type_pool,
            &interner,
            Target::Aarch64Linux,
        )
        .lower()
    }

    /// RUE-1014: under the default compact layout, a non-slot-identical **array**
    /// frame value lowers on AArch64 in lockstep with x86-64: array `[]`
    /// indexing strides by the *slot* stride (`abi_slot_count(element) *
    /// SLOT_BYTES`) against the slot-shaped frame storage (RUE-975).
    #[test]
    fn aggregate_layout_allows_frame_array_slot_stride_indexing() {
        try_lower_first_fn(
            "fn main() -> i32 { let a: [i32; 3] = [1, 2, 3]; let i: u64 = 1; a[i] }",
            PreviewFeatures::new(),
        )
        .expect("a frame array indexed at the slot stride must lower under compact layout");
    }

    /// RUE-989: under the default compact layout, narrow scalar access through a
    /// typed pointer lowers on AArch64 in lockstep with x86-64, emitting the
    /// narrow pseudos (`ldrsw`/`str w`) instead of the eight-byte `Ldr`/`Str`.
    #[test]
    fn aggregate_layout_allows_narrow_scalar_physical_access() {
        let source = "fn main() -> i32 { checked { let p: ptr mut i32 = @int_to_ptr(@ptr_to_int(@alloc(@intCast(@size_of(i32)), @intCast(@align_of(i32))))); \
                      @ptr_write(p, 5); @dbg(@ptr_read(p)); @free(@int_to_ptr(@ptr_to_int(p)), @intCast(@size_of(i32)), @intCast(@align_of(i32))); }; 0 }";
        let mir = try_lower_first_fn(source, PreviewFeatures::new())
            .expect("a narrow scalar through a typed pointer must lower under compact layout");
        assert!(
            mir.instructions()
                .iter()
                .any(|inst| matches!(inst, Aarch64Inst::NarrowStoreIndexed { width: 4, .. })),
            "a narrow i32 @ptr_write must emit a 4-byte narrow store"
        );
        assert!(
            mir.instructions().iter().any(|inst| matches!(
                inst,
                Aarch64Inst::NarrowLoadIndexed {
                    width: 4,
                    signed: true,
                    ..
                }
            )),
            "a narrow i32 @ptr_read must emit a sign-extending 4-byte narrow load"
        );
    }

    /// RUE-1000: a compact enum with a variant-independent memory image
    /// round-trips through a typed pointer on AArch64, marshalling the narrow
    /// tag at offset 0 and the payload at its compact offset.
    #[test]
    fn aggregate_layout_allows_compact_enum_memory() {
        let source = "enum Opt { Some(i32), None } \
                      fn main() -> i32 { checked { let p: ptr mut Opt = @int_to_ptr(@ptr_to_int(@alloc(@intCast(@size_of(Opt)), @intCast(@align_of(Opt))))); \
                      @ptr_write(p, Opt.Some(42)); let e = @ptr_read(p); \
                      match e { Opt.Some(x) => @dbg(x), Opt.None => @dbg(0), }; \
                      @free(@int_to_ptr(@ptr_to_int(p)), @intCast(@size_of(Opt)), @intCast(@align_of(Opt))); }; 0 }";
        let mir = try_lower_first_fn(source, PreviewFeatures::new())
            .expect("a compact enum with a variant-independent image must lower");
        assert!(
            mir.instructions()
                .iter()
                .any(|inst| matches!(inst, Aarch64Inst::NarrowStoreIndexed { width: 1, .. })),
            "the enum write must store the u8 tag narrow at offset 0"
        );
        assert!(
            mir.instructions().iter().any(|inst| matches!(
                inst,
                Aarch64Inst::NarrowLoadIndexed {
                    width: 1,
                    signed: false,
                    ..
                }
            )),
            "the enum read must load the u8 tag zero-extended"
        );
    }

    /// RUE-1037: on AArch64, a HETEROGENEOUS enum (variants whose payload layouts
    /// disagree) is marshalled through a pointer by dispatching on the runtime tag
    /// — the store emits a tag compare per variant plus narrow field stores.
    #[test]
    fn aggregate_layout_heterogeneous_enum_ptr_write_tag_dispatches() {
        let source = "enum R { Ok(Point), Err(i64) } \
                      struct Point { x: i32, y: i32, z: i32 } \
                      fn store_it(p: ptr mut R) { checked { @ptr_write(p, R.Ok(Point { x: 1, y: 2, z: 3 })); }; } \
                      fn main() -> i32 { checked { let p: ptr mut R = @int_to_ptr(@ptr_to_int(@alloc(@intCast(@size_of(R)), @intCast(@align_of(R))))); store_it(p); @free(@int_to_ptr(@ptr_to_int(p)), @intCast(@size_of(R)), @intCast(@align_of(R))); }; 0 }";
        let mir = try_lower_named_fn(source, PreviewFeatures::new(), Some("store_it"))
            .expect("a heterogeneous enum @ptr_write must lower via tag dispatch");
        assert!(
            mir.instructions()
                .iter()
                .any(|inst| matches!(inst, Aarch64Inst::CmpImm { .. })),
            "the heterogeneous store must compare the tag to dispatch on the variant"
        );
        assert!(
            mir.instructions()
                .iter()
                .any(|inst| matches!(inst, Aarch64Inst::NarrowStoreIndexed { .. })),
            "the active variant's narrow leaves must be stored narrow"
        );
    }

    /// RUE-987: a whole compact struct round-trips through a typed pointer on
    /// AArch64, reusing the enum-image slot machinery — `@ptr_write` stores each
    /// field narrow at its compact offset and `@ptr_read` reloads it.
    #[test]
    fn aggregate_layout_allows_compact_struct_memory() {
        let source = "struct Padded { a: u8, b: i32, c: u8 } \
                      fn main() -> i32 { checked { let p: ptr mut Padded = @int_to_ptr(@ptr_to_int(@alloc(@intCast(@size_of(Padded)), @intCast(@align_of(Padded))))); \
                      @ptr_write(p, Padded { a: 7, b: 1000, c: 9 }); let s = @ptr_read(p); \
                      @dbg(s.b); @free(@int_to_ptr(@ptr_to_int(p)), @intCast(@size_of(Padded)), @intCast(@align_of(Padded))); }; 0 }";
        let mir = try_lower_first_fn(source, PreviewFeatures::new()).expect(
            "a whole compact struct through a typed pointer must lower under compact layout",
        );
        assert!(
            mir.instructions()
                .iter()
                .any(|inst| matches!(inst, Aarch64Inst::NarrowStoreIndexed { width: 1, .. })),
            "the struct write must store the u8 fields narrow at their compact offsets"
        );
        assert!(
            mir.instructions()
                .iter()
                .any(|inst| matches!(inst, Aarch64Inst::NarrowLoadIndexed { width: 4, .. })),
            "the struct read must reload the i32 field narrow from its compact offset"
        );
    }

    /// RUE-1014: a struct containing a fixed array now HAS a variant-independent
    /// compact image — the array flattens to its elements at the compact stride —
    /// so it marshals whole through a pointer on AArch64.
    #[test]
    fn aggregate_layout_allows_array_bearing_struct_memory() {
        let source = "struct HasArr { tag: u8, xs: [i32; 2] } \
                      fn main() -> i32 { let r = checked { let p: ptr mut HasArr = @int_to_ptr(@ptr_to_int(@alloc(@intCast(@size_of(HasArr)), @intCast(@align_of(HasArr))))); \
                      @ptr_write(p, HasArr { tag: 5, xs: [10, 20] }); let v = @ptr_read(p); \
                      @free(@int_to_ptr(@ptr_to_int(p)), @intCast(@size_of(HasArr)), @intCast(@align_of(HasArr))); v.xs[0] + v.xs[1] }; @dbg(r); 0 }";
        let mir = try_lower_first_fn(source, PreviewFeatures::new())
            .expect("a struct with a compact array image must lower under compact layout");
        assert!(
            mir.instructions()
                .iter()
                .any(|inst| matches!(inst, Aarch64Inst::NarrowStoreIndexed { width: 4, .. })),
            "the array elements must store narrow (4-byte i32) at their compact stride"
        );
    }

    /// RUE-1014: a variant-dependent enum whose per-variant aggregate payloads
    /// overlay one variant-independent scalar image (`Option(Point)`) marshals
    /// whole through a pointer on AArch64.
    #[test]
    fn aggregate_layout_allows_variant_dependent_enum_image() {
        let source = "struct Point { x: i32, y: i32 } enum Opt { Some(Point), None } \
                      fn main() -> i32 { let r = checked { let p: ptr mut Opt = @int_to_ptr(@ptr_to_int(@alloc(@intCast(@size_of(Opt)), @intCast(@align_of(Opt))))); \
                      @ptr_write(p, Opt.Some(Point { x: 40, y: 2 })); let v = @ptr_read(p); \
                      @free(@int_to_ptr(@ptr_to_int(p)), @intCast(@size_of(Opt)), @intCast(@align_of(Opt))); match v { Opt.Some(pt) => pt.x + pt.y, Opt.None => 0 - 1 } }; \
                      @dbg(r); 0 }";
        let mir = try_lower_first_fn(source, PreviewFeatures::new())
            .expect("an enum with a variant-independent struct-payload image must lower");
        assert!(
            mir.instructions()
                .iter()
                .any(|inst| matches!(inst, Aarch64Inst::NarrowStoreIndexed { width: 4, .. })),
            "the struct-payload leaves must store narrow (4-byte i32) at their compact offsets"
        );
    }

    /// RUE-1037: a struct embedding a HETEROGENEOUS enum (`A(i64)` versus
    /// `B(i32, i32)`, whose payload layouts disagree) marshals through a pointer
    /// on AArch64 via a nested tag dispatch — the store compares the embedded tag
    /// and stores the active variant's leaves. (Previously refused as imageless.)
    #[test]
    fn aggregate_layout_marshals_struct_embedding_heterogeneous_enum() {
        let source = "enum Bad { A(i64), B(i32, i32) } struct HasBad { b: Bad } \
                      fn main() -> i32 { checked { let p: ptr mut HasBad = @int_to_ptr(@ptr_to_int(@alloc(@intCast(@size_of(HasBad)), @intCast(@align_of(HasBad))))); \
                      @ptr_write(p, HasBad { b: Bad.A(5) }); @free(@int_to_ptr(@ptr_to_int(p)), @intCast(@size_of(HasBad)), @intCast(@align_of(HasBad))); }; 0 }";
        let mir = try_lower_first_fn(source, PreviewFeatures::new())
            .expect("a struct embedding a heterogeneous enum must lower via nested tag dispatch");
        assert!(
            mir.instructions()
                .iter()
                .any(|inst| matches!(inst, Aarch64Inst::CmpImm { .. })),
            "the nested heterogeneous enum store must dispatch on the embedded tag"
        );
    }

    /// RUE-1004: on AArch64, a non-slot-identical struct returned by value is
    /// forced indirect (sret); the caller reads the callee-written compact image
    /// back from the sret buffer with narrow loads at the compact offsets (`main`
    /// is the caller here).
    #[test]
    fn aggregate_layout_allows_compact_struct_sret_return() {
        let source = "struct Padded { a: u8, b: i32, c: u8 } \
                      fn make() -> Padded { Padded { a: 7, b: 1000, c: 9 } } \
                      fn main() -> i32 { let p = make(); @dbg(p.b); 0 }";
        let mir = try_lower_first_fn(source, PreviewFeatures::new())
            .expect("a compact struct sret return must lower");
        assert!(
            mir.instructions()
                .iter()
                .any(|inst| matches!(inst, Aarch64Inst::NarrowLoadIndexed { width: 1, .. })),
            "the caller must read the u8 fields narrow from the compact sret image"
        );
    }

    /// RUE-1004: on AArch64, an `inout` non-slot-identical struct argument is
    /// accepted (slot-shaped by-ref transport to the caller's frame storage).
    #[test]
    fn aggregate_layout_allows_inout_compact_struct_param() {
        try_lower_first_fn(
            "struct Padded { a: u8, b: i32, c: u8 } \
             fn bump(inout p: Padded) { p.b = p.b + 1; } \
             fn main() -> i32 { let mut s = Padded { a: 1, b: 2, c: 3 }; bump(inout s); s.b }",
            PreviewFeatures::new(),
        )
        .expect("an inout compact struct argument must lower");
    }

    /// RUE-1005: on AArch64, a non-slot-identical struct passed BY VALUE across a
    /// call now lowers. The caller (`main`, first here) writes the compact image
    /// into a caller-owned buffer with narrow stores and passes one pointer.
    #[test]
    fn aggregate_layout_allows_by_value_compact_struct_arg_caller() {
        let source = "struct Padded { a: u8, b: i32, c: u8 } \
                      fn main() -> i32 { sum(Padded { a: 1, b: 5, c: 3 }) } \
                      fn sum(p: Padded) -> i32 { p.b }";
        let mir = try_lower_first_fn(source, PreviewFeatures::new())
            .expect("a by-value compact struct argument must lower");
        assert!(
            mir.instructions()
                .iter()
                .any(|inst| matches!(inst, Aarch64Inst::NarrowStoreIndexed { width: 1, .. })),
            "the caller must write the u8 fields narrow into the compact argument buffer"
        );
    }

    /// RUE-1005 callee side on AArch64: the receiving function unmarshals the
    /// compact image (narrow loads) from the homed pointer into its frame slots
    /// at entry.
    #[test]
    fn aggregate_layout_allows_by_value_compact_struct_arg_callee() {
        let source = "struct Padded { a: u8, b: i32, c: u8 } \
                      fn sum(p: Padded) -> i32 { p.b } \
                      fn main() -> i32 { sum(Padded { a: 1, b: 5, c: 3 }) }";
        let mir = try_lower_named_fn(source, PreviewFeatures::new(), Some("sum"))
            .expect("the callee of a by-value compact struct argument must lower");
        assert!(
            mir.instructions()
                .iter()
                .any(|inst| matches!(inst, Aarch64Inst::NarrowLoadIndexed { width: 1, .. })),
            "the callee must unmarshal the u8 fields narrow from the homed pointer"
        );
    }

    /// RUE-1037: a compact enum whose union payload slots overlap (`A(i64)`
    /// versus `B(i32, i32)`, no variant-independent image) now marshals through a
    /// pointer on AArch64 via per-variant tag dispatch rather than being refused.
    #[test]
    fn aggregate_layout_marshals_variant_dependent_enum_memory() {
        let mir = try_lower_first_fn(
            "enum Bad { A(i64), B(i32, i32) } \
             fn main() -> i32 { checked { let p: ptr mut Bad = @int_to_ptr(@ptr_to_int(@alloc(@intCast(@size_of(Bad)), @intCast(@align_of(Bad))))); \
             @ptr_write(p, Bad.A(5)); @free(@int_to_ptr(@ptr_to_int(p)), @intCast(@size_of(Bad)), @intCast(@align_of(Bad))); }; 0 }",
            PreviewFeatures::new(),
        )
        .expect("a variant-dependent enum memory image must lower via tag dispatch");
        assert!(
            mir.instructions()
                .iter()
                .any(|inst| matches!(inst, Aarch64Inst::CmpImm { .. })),
            "the heterogeneous enum store must dispatch on the tag"
        );
    }

    /// A slot-identical aggregate (all eight-byte leaves) marshals correctly
    /// under compact layout on AArch64 and is accepted.
    #[test]
    fn aggregate_layout_allows_slot_identical_physical_layout_codegen() {
        try_lower_first_fn(
            "struct Cell { a: i64, b: i64 } fn main() -> i32 { let c = Cell { a: 7, b: 9 }; @intCast(c.a) }",
            PreviewFeatures::new(),
        )
        .expect("a slot-identical aggregate must lower under compact layout");
    }

    fn immediate_for_operand(mir: &Aarch64Mir, operand: Operand) -> Option<i64> {
        let Operand::Virtual(vreg) = operand else {
            return None;
        };
        mir.instructions().iter().rev().find_map(|inst| match inst {
            Aarch64Inst::MovImm {
                dst: Operand::Virtual(dst),
                imm,
            } if *dst == vreg => Some(*imm),
            _ => None,
        })
    }

    fn immediate_call_arg(mir: &Aarch64Mir, call_index: usize, reg: Reg) -> Option<i64> {
        mir.instructions()[..call_index]
            .iter()
            .rev()
            .take_while(|inst| !matches!(inst, Aarch64Inst::Bl { .. }))
            .find_map(|inst| match inst {
                Aarch64Inst::MovImm {
                    dst: Operand::Physical(dst),
                    imm,
                } if *dst == reg => Some(*imm),
                Aarch64Inst::MovRR {
                    dst: Operand::Physical(dst),
                    src,
                } if *dst == reg => immediate_for_operand(mir, *src),
                _ => None,
            })
    }

    fn runtime_call_index(mir: &Aarch64Mir, helper: rue_runtime_abi::RuntimeHelperId) -> usize {
        mir.instructions()
            .iter()
            .position(|inst| {
                matches!(inst, Aarch64Inst::Bl { symbol_id, .. } if mir.get_symbol(*symbol_id) == helper.symbol())
            })
            .unwrap_or_else(|| panic!("missing call to {helper:?}"))
    }

    #[test]
    fn runtime_manifest_fits_register_only_target_c_subset() {
        for id in rue_runtime_abi::RuntimeHelperId::ALL {
            let helper = id.helper();
            assert_eq!(
                helper.calling_convention,
                rue_runtime_abi::CallingConvention::TargetC,
                "{} must use the target C convention",
                helper.symbol
            );
            assert!(
                helper.parameters.len() <= ARG_REGS.len(),
                "{} has {} parameters, exceeding the AArch64 register-only runtime-call budget of {}",
                helper.symbol,
                helper.parameters.len(),
                ARG_REGS.len()
            );
        }
    }

    fn lower_to_mir(source: &str) -> Aarch64Mir {
        lower_function_to_mir(source, "main")
    }

    #[test]
    fn test_simple_return() {
        let mir = lower_to_mir("fn main() -> i32 { 42 }");
        assert!(!mir.instructions().is_empty());
    }

    #[test]
    fn unit_main_exits_with_zero_status() {
        let mir = lower_to_mir("fn main() {}");
        assert!(mir.instructions().windows(2).any(|pair| {
            matches!(
                pair,
                [
                    Aarch64Inst::MovImm {
                        dst: Operand::Physical(Reg::X0),
                        imm: 0,
                    },
                    Aarch64Inst::Bl { .. },
                ]
            )
        }));
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

    #[test]
    fn default_string_literal_lowers_only_ptr_and_len() {
        let preview = PreviewFeatures::new();
        let mir = lower_function_to_mir_with_preview(
            "fn main() -> i32 { let s = \"hello\"; @intCast(s.len()) }",
            "main",
            preview,
        );
        assert!(
            mir.instructions()
                .iter()
                .any(|inst| matches!(inst, Aarch64Inst::StringConstPtr { .. }))
        );
        assert!(
            mir.instructions()
                .iter()
                .any(|inst| matches!(inst, Aarch64Inst::StringConstLen { .. }))
        );
        assert!(
            !mir.instructions()
                .iter()
                .any(|inst| matches!(inst, Aarch64Inst::StringConstCap { .. }))
        );
    }

    #[test]
    fn scalar_param_store_keeps_base_only_indexed_mir() {
        let mir = lower_function_to_mir(
            "fn set(inout x: i32) { x = 42; } \
             fn main() -> i32 { let mut x = 0; set(inout x); x }",
            "set",
        );
        assert!(
            mir.instructions()
                .iter()
                .any(|inst| matches!(inst, Aarch64Inst::StrIndexed { .. }))
        );
        assert!(
            !mir.instructions()
                .iter()
                .any(|inst| matches!(inst, Aarch64Inst::StrIndexedOffset { offset: 0, .. })),
            "scalar ParamStore must not drift to the offset-form pseudo"
        );
    }

    #[test]
    fn raw_bytes_runtime_helper_identity_and_slots_match_shared_plan() {
        let preview = PreviewFeatures::new();
        let mir = lower_function_to_mir_with_preview(
            "fn main() -> i32 { checked { let p = @alloc(3, 1); \
             @ptr_write(@ptr_offset(p, 1), 255); let q = @realloc(p, 3, 1, 5); \
             let b = @ptr_read(@ptr_offset(q, 1)); @free(q, 5, 1); \
             @intCast(b) } }",
            "main",
            preview,
        );
        assert!(
            mir.instructions()
                .iter()
                .any(|inst| matches!(inst, Aarch64Inst::NarrowStoreIndexed { width: 1, .. }))
        );
        assert!(
            mir.instructions()
                .iter()
                .any(|inst| matches!(inst, Aarch64Inst::NarrowLoadIndexed { width: 1, .. }))
        );

        let alloc = runtime_call_index(&mir, rue_runtime_abi::RuntimeHelperId::Alloc);
        assert_eq!(immediate_call_arg(&mir, alloc, Reg::X0), Some(3));
        assert_eq!(immediate_call_arg(&mir, alloc, Reg::X1), Some(1));

        let realloc = runtime_call_index(&mir, rue_runtime_abi::RuntimeHelperId::Realloc);
        assert_eq!(immediate_call_arg(&mir, realloc, Reg::X1), Some(3));
        assert_eq!(immediate_call_arg(&mir, realloc, Reg::X2), Some(5));
        assert_eq!(immediate_call_arg(&mir, realloc, Reg::X3), Some(1));

        let free = runtime_call_index(&mir, rue_runtime_abi::RuntimeHelperId::Free);
        assert_eq!(immediate_call_arg(&mir, free, Reg::X1), Some(5));
        assert_eq!(immediate_call_arg(&mir, free, Reg::X2), Some(1));
    }

    /// RUE-978/RUE-962: byte-granular access is the ordinary typed `ptr u8`
    /// narrow path. `@ptr_read`/`@ptr_write` of a `u8` pointee, walked with
    /// `@ptr_offset`, emit a one-byte `NarrowLoadIndexed`/`NarrowStoreIndexed`
    /// rather than a full-slot access.
    #[test]
    fn aggregate_layout_folds_byte_access_into_narrow_typed_path() {
        let mir = lower_function_to_mir_with_preview(
            "fn main() -> i32 { checked { let p = @alloc(2, 1); \
             @ptr_write(@ptr_offset(p, 0), 65); let b = @ptr_read(@ptr_offset(p, 1)); \
             @free(p, 2, 1); @intCast(b) } }",
            "main",
            PreviewFeatures::new(),
        );
        assert!(
            mir.instructions()
                .iter()
                .any(|inst| matches!(inst, Aarch64Inst::NarrowStoreIndexed { width: 1, .. })),
            "a `u8` @ptr_write must use the one-byte narrow store"
        );
        assert!(
            mir.instructions().iter().any(|inst| matches!(
                inst,
                Aarch64Inst::NarrowLoadIndexed {
                    width: 1,
                    signed: false,
                    ..
                }
            )),
            "a `u8` @ptr_read must use the one-byte zero-extended narrow load"
        );
    }
}
