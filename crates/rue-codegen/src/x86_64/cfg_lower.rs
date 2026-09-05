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

use ahash::AHashMap;
use lasso::ThreadedRodeo;
use rue_air::{FrozenTypeInternPool, TypeKind};
use rue_cfg::{BlockId, Cfg, CfgValue, Type, ValidatedCfg};
use rue_error::CompileResult;

use super::mir::{FloatWidth, LabelId, Operand, Reg, VReg, X86Inst, X86Mir};
use crate::agg_slots::SlotBackend;
use crate::allocation;
use crate::cfg_lower::CfgLowerContext;
use crate::frame_layout::{
    checked_aligned_cell_region_bytes, checked_cell_byte_offset, checked_cell_region_bytes,
    checked_cell_region_padding_bytes, checked_displacement_bytes,
};
use crate::vreg::BLOCK_LABEL_BASE;

/// Argument passing registers per System V AMD64 ABI. ABI arg slots beyond
/// these are passed on the caller's stack (slot `k >= 6` at `[rbp+16+(k-6)*8]`
/// in the callee); the callee prologue copies them into its frame param area
/// so the body addresses every param slot uniformly (see `emit_prologue`).
/// The one x86-64 target Rue supports. This backend serves exactly that target,
/// so it names it rather than threading it through lowering; every convention
/// question still resolves through the single `"C"` alias table keyed by it,
/// never by architecture.
pub(super) const X86_64_TARGET: rue_target::Target = rue_target::Target::X86_64Linux;

/// The convention an `extern "C"` boundary follows on this backend's target.
pub(super) const fn x86_64_target_c_convention() -> rue_target::CallingConvention {
    X86_64_TARGET.c_calling_convention()
}

pub(super) const ARG_REGS: [Reg; 6] = [Reg::Rdi, Reg::Rsi, Reg::Rdx, Reg::Rcx, Reg::R8, Reg::R9];

pub(super) const FP_ARG_REGS: [Reg; 8] = [
    Reg::Xmm0,
    Reg::Xmm1,
    Reg::Xmm2,
    Reg::Xmm3,
    Reg::Xmm4,
    Reg::Xmm5,
    Reg::Xmm6,
    Reg::Xmm7,
];

/// The physical register a foreign call's classified register piece names.
///
/// A foreign argument is marshaled through its compact memory image, whose
/// pieces are whole eightbytes in general-purpose vregs, so an SSE-classed
/// piece has no value to move: the C boundary rejects `f32`/`f64`
/// (`c_passable_by_value`), which is what keeps every piece integer-classed.
fn foreign_argument_register(class: rue_target::CRegisterClass, index: u32) -> Reg {
    assert_eq!(
        class,
        rue_target::CRegisterClass::Gp,
        "a foreign argument crosses through its eightbyte image in the \
         general-purpose bank"
    );
    ARG_REGS[index as usize]
}

/// Return value registers for the internal Rue convention (SysV only defines
/// rax/rdx; the rest are caller-saved scratch regs we extend the convention
/// with for multi-slot aggregate returns). Aggregates with more slots than
/// this — and builtin String always — return via sret instead; see
/// `crate::cfg_lower::type_uses_sret_return`. (RUE-106)
pub(super) const RET_REGS: [Reg; 6] = [Reg::Rax, Reg::Rdx, Reg::Rcx, Reg::R8, Reg::R9, Reg::R10];

/// Floating-point return registers. A float-classed return slot travels in the
/// same XMM bank a float argument does — `xmm0` is already the scalar float
/// return register — so the two directions name one register file.
pub(super) const FP_RET_REGS: [Reg; 8] = FP_ARG_REGS;

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
    let mut index = 0;
    while index < FP_RET_REGS.len() {
        assert!(
            super::mir::is_reserved(FP_RET_REGS[index]),
            "every ABI result register must be reserved from allocation"
        );
        index += 1;
    }
};

/// The target's return-register file, one bank per register class: a
/// general-purpose slot lands in [`RET_REGS`], a float-classed one in
/// [`FP_RET_REGS`].
pub(super) const fn return_register_banks() -> crate::call_plan::AbiRegisterBanks {
    crate::call_plan::AbiRegisterBanks {
        gp: RET_REGS.len(),
        fp: FP_RET_REGS.len(),
    }
}

/// CFG to X86Mir lowering.
pub struct CfgLower<'a> {
    /// Shared context with type helpers and chain tracing.
    ctx: CfgLowerContext<'a>,
    /// Cooperative cancellation authority for this lowering (RUE-1827).
    cancellation: crate::GenerationCancellation<'a>,
    /// Interner for resolving Spur to string
    interner: &'a ThreadedRodeo,
    symbols: crate::MachineSymbolResolver<'a>,
    mir: X86Mir,
    /// Maps CFG values to vregs
    value_map: AHashMap<CfgValue, VReg>,
    /// Maps block parameters to vregs (block_id, param_index) -> vreg
    block_param_vregs: AHashMap<(BlockId, u32), VReg>,
    /// Next inline label ID for generating unique labels.
    ///
    /// Inline labels (for overflow checks, bounds checks, etc.) use IDs from
    /// the lower half of the `u32` space. See module docs for namespace details.
    next_label: u32,
    /// Function name (needed to detect main function)
    fn_name: &'a str,
    /// Maps StructInit CFG values to their field vregs
    struct_slot_vregs: AHashMap<CfgValue, Vec<VReg>>,
    /// Maps by-reference parameter indices to their pointer vregs.
    /// For by-ref params, the slot contains a pointer to the caller's memory.
    /// This map stores the vreg holding that pointer so Store can use it.
    by_ref_param_ptrs: AHashMap<u32, VReg>,
    /// Maps register-only by-value parameter indices (RUE-1170) to the vreg
    /// their entry copy defined. These vregs are read-only for the rest of
    /// the function — every consumer copies before mutating, the same
    /// invariant that lets CSE key repeated `Param` reads (RUE-914) — so a
    /// single vreg serves every read.
    param_reg_vregs: AHashMap<u32, VReg>,
}

impl crate::call_plan::CallMaterializer for CfgLower<'_> {
    fn target_c_convention(&self) -> rue_target::CallingConvention {
        x86_64_target_c_convention()
    }

    fn native_convention(&self) -> rue_target::ConventionSpec {
        rue_target::ConventionSpec::native(X86_64_TARGET)
    }

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
        self.mir.push(X86Inst::AddRI {
            dst: Operand::Physical(Reg::Rsp),
            imm: -checked_displacement_bytes(u64::from(storage_bytes))
                .expect("sret storage must fit displacement"),
        });
        let pointer = self.mir.alloc_vreg();
        self.mir.push(X86Inst::MovRR {
            dst: Operand::Virtual(pointer),
            src: Operand::Physical(Reg::Rsp),
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
        self.mir.push(X86Inst::AddRI {
            dst: Operand::Physical(Reg::Rsp),
            imm: -checked_displacement_bytes(u64::from(storage_bytes))
                .expect("indirect argument storage must fit displacement"),
        });
        let pointer = self.mir.alloc_vreg();
        self.mir.push(X86Inst::MovRR {
            dst: Operand::Virtual(pointer),
            src: Operand::Physical(Reg::Rsp),
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
        self.mir.push(X86Inst::AddRI {
            dst: Operand::Physical(Reg::Rsp),
            imm: -checked_displacement_bytes(u64::from(storage_bytes))
                .expect("indirect argument storage must fit displacement"),
        });
        let pointer = self.mir.alloc_vreg();
        self.mir.push(X86Inst::MovRR {
            dst: Operand::Virtual(pointer),
            src: Operand::Physical(Reg::Rsp),
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
    ) -> Self {
        Self::new_inner(
            cfg,
            type_pool,
            interner,
            crate::MachineSymbolResolver::default(),
        )
    }

    pub fn new_with_symbols(
        cfg: &'a ValidatedCfg,
        type_pool: &'a FrozenTypeInternPool,
        interner: &'a ThreadedRodeo,
        symbols: crate::MachineSymbolResolver<'a>,
    ) -> Self {
        Self::new_inner(cfg, type_pool, interner, symbols)
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
    /// Install the caller's cooperative cancellation authority (RUE-1827).
    pub(crate) fn with_cancellation(
        mut self,
        cancellation: crate::GenerationCancellation<'a>,
    ) -> Self {
        self.cancellation = cancellation;
        self
    }

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
    ) -> Self {
        Self::new_inner(
            cfg,
            type_pool,
            interner,
            crate::MachineSymbolResolver::default(),
        )
    }

    fn new_inner(
        cfg: &'a Cfg,
        type_pool: &'a FrozenTypeInternPool,
        interner: &'a ThreadedRodeo,
        symbols: crate::MachineSymbolResolver<'a>,
    ) -> Self {
        let num_params = cfg.num_params();

        // Pre-calculate capacity hints to reduce AHashMap reallocations
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
            cancellation: crate::GenerationCancellation::NONE,
            interner,
            symbols,
            mir: X86Mir::new(),
            value_map: AHashMap::with_capacity(num_values),
            block_param_vregs: AHashMap::with_capacity(estimated_block_params),
            next_label: 0,
            fn_name: cfg.fn_name(),
            struct_slot_vregs: AHashMap::with_capacity(estimated_struct_inits),
            by_ref_param_ptrs: AHashMap::with_capacity(estimated_by_ref_params),
            param_reg_vregs: AHashMap::new(),
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
        use crate::runtime_call_plan::{RuntimeCallArg, RuntimeCallPlan, RuntimeCallResult};

        assert_eq!(
            RuntimeCallPlan::calling_convention(X86_64_TARGET),
            rue_target::CallingConvention::X86_64SysV,
            "this lowering implements SysV AMD64, the convention the runtime-helper \
             boundary resolves to on x86-64"
        );
        assert!(
            plan.args().len() <= ARG_REGS.len(),
            "runtime manifest exceeds the x86-64 target-C register budget"
        );

        let out_shape = plan.out_shape();
        let out_bytes = out_shape
            .map(|shape| {
                i32::try_from(
                    checked_aligned_cell_region_bytes(
                        u64::try_from(shape.shape().slots.len())
                            .expect("runtime out slot count must fit u64"),
                    )
                    .expect("runtime out area must pass frame-budget preflight"),
                )
                .expect("runtime out area must fit displacement")
            })
            .unwrap_or(0);
        if out_bytes > 0 {
            self.mir.push(X86Inst::AddRI {
                dst: Operand::Physical(Reg::Rsp),
                imm: -out_bytes,
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
                        self.mir.push(X86Inst::MovRR {
                            dst: Operand::Virtual(extended),
                            src: Operand::Virtual(value),
                        });
                        let dst = Operand::Virtual(extended);
                        let src = Operand::Virtual(extended);
                        self.mir.push(match extension {
                            crate::value_plan::IntegerExtension::Sign8 => {
                                X86Inst::Movsx8To64 { dst, src }
                            }
                            crate::value_plan::IntegerExtension::Zero8 => {
                                X86Inst::Movzx8To64 { dst, src }
                            }
                            crate::value_plan::IntegerExtension::Sign16 => {
                                X86Inst::Movsx16To64 { dst, src }
                            }
                            crate::value_plan::IntegerExtension::Zero16 => {
                                X86Inst::Movzx16To64 { dst, src }
                            }
                            crate::value_plan::IntegerExtension::Sign32 => {
                                X86Inst::Movsx32To64 { dst, src }
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
                    self.mir.push(X86Inst::MovRR {
                        dst,
                        src: Operand::Virtual(value.expect("materialized runtime argument")),
                    });
                }
                RuntimeCallArg::Immediate { value, parameter } => {
                    if matches!(
                        parameter.ty,
                        rue_runtime_abi::AbiType::I32 | rue_runtime_abi::AbiType::U32
                    ) {
                        self.mir.push(X86Inst::MovRI32 {
                            dst,
                            imm: value as i32,
                        });
                    } else {
                        self.mir.push(X86Inst::MovRI64 {
                            dst,
                            imm: value as i64,
                        });
                    }
                }
                RuntimeCallArg::OutPointer { shape } => {
                    assert_eq!(Some(shape), out_shape);
                    self.mir.push(X86Inst::MovRR {
                        dst,
                        src: Operand::Physical(Reg::Rsp),
                    });
                }
            }
        }

        // The manifest's control contract travels with the call: a trap helper
        // aborts, so liveness must not propagate anything past it (RUE-1224).
        let symbol_id = self.intern_symbol(plan.symbol());
        self.mir.push(X86Inst::CallRel {
            symbol_id,
            returns: plan.return_behavior(),
        });

        match plan.result() {
            RuntimeCallResult::OutPointer(shape) => {
                let slots = (0..shape.shape().slots.len())
                    .map(|index| {
                        let slot = self.mir.alloc_vreg();
                        self.mir.push(X86Inst::MovRM {
                            dst: Operand::Virtual(slot),
                            base: Reg::Rsp,
                            offset: checked_cell_byte_offset(index as u64)
                                .expect("runtime out slot offset must fit displacement"),
                        });
                        slot
                    })
                    .collect::<Vec<_>>();
                self.mir.push(X86Inst::AddRI {
                    dst: Operand::Physical(Reg::Rsp),
                    imm: out_bytes,
                });
                crate::value_plan::MaterializedValue {
                    primary: slots[0],
                    slots,
                }
            }
            RuntimeCallResult::Scalar(_) => {
                let primary = self.mir.alloc_vreg();
                self.mir.push(X86Inst::MovRR {
                    dst: Operand::Virtual(primary),
                    src: Operand::Physical(Reg::Rax),
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
        self.mir.push(X86Inst::MovMRIndexed {
            base: ptr,
            offset: 0,
            src: Operand::Virtual(src),
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
        self.mir.push(X86Inst::MovRM {
            dst: Operand::Virtual(ptr_vreg),
            base: Reg::Rbp,
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
        for (param_slot, class, location) in self.ctx.param_entry_copies() {
            let (vreg, copy) = match (class, location) {
                (
                    crate::call_plan::AbiSlotClass::Gp,
                    crate::call_plan::AbiSlotLocation::GpReg(class_index),
                ) => {
                    let vreg = self.mir.alloc_vreg();
                    (
                        vreg,
                        X86Inst::MovRR {
                            dst: Operand::Virtual(vreg),
                            src: Operand::Physical(ARG_REGS[class_index]),
                        },
                    )
                }
                (
                    crate::call_plan::AbiSlotClass::Fp(width),
                    crate::call_plan::AbiSlotLocation::FpReg(class_index),
                ) => {
                    let vreg = self.mir.alloc_vreg_in(crate::reg_class::RegClass::Fp);
                    (
                        vreg,
                        X86Inst::FloatMov {
                            dst: Operand::Virtual(vreg),
                            src: Operand::Physical(FP_ARG_REGS[class_index]),
                            width,
                        },
                    )
                }
                _ => unreachable!("register-only parameter class and ABI bank must agree"),
            };
            self.mir.push(copy);
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
            rue_target::ConventionSpec::native(X86_64_TARGET),
            crate::call_plan::AbiRegisterBanks {
                gp: ARG_REGS.len(),
                fp: FP_ARG_REGS.len(),
            },
            x86_64_target_c_convention(),
        );
        let _ = self.lower_call_plan(plan);
    }

    /// Call `symbol` passing `arg_vregs` as flattened by-value slot arguments
    /// per the standard convention: the first slots in ARG_REGS, the rest
    /// pushed on the stack (16-byte aligned, cleaned up after the call) —
    /// the same shape as generic `Call` lowering. Drop and drop-glue calls use
    /// this path so every slot beyond the six register arguments is passed on
    /// the stack (RUE-193).
    fn emit_div_core(&mut self, is_64: bool, is_signed: bool, rhs_vreg: VReg) {
        if is_signed {
            // Sign-extend (R/E)AX into (R/E)DX.
            self.mir
                .push(if is_64 { X86Inst::Cqo } else { X86Inst::Cdq });
            self.mir.push(if is_64 {
                X86Inst::Idiv64R {
                    src: Operand::Virtual(rhs_vreg),
                }
            } else {
                X86Inst::IdivR {
                    src: Operand::Virtual(rhs_vreg),
                }
            });
        } else {
            // Zero RDX so the dividend is RDX:RAX with a zero high half.
            // (`xor edx, edx` clears all 64 bits of RDX.)
            self.mir.push(X86Inst::XorRR {
                dst: Operand::Physical(Reg::Rdx),
                src: Operand::Physical(Reg::Rdx),
            });
            self.mir.push(if is_64 {
                X86Inst::Div64R {
                    src: Operand::Virtual(rhs_vreg),
                }
            } else {
                X86Inst::DivR {
                    src: Operand::Virtual(rhs_vreg),
                }
            });
        }
    }

    /// Emit the signed-division overflow guard: `MIN / -1` (and `MIN % -1`)
    /// overflows because the quotient `-MIN` is unrepresentable. The hardware
    /// gives no usable trap: 32/64-bit IDIV raises #DE (SIGFPE, exit 136) and
    /// sub-word divisions — performed as 32-bit IDIV on sign-extended
    /// operands — silently produce the out-of-range quotient +2^(w-1). Check
    /// `dividend == MIN && divisor == -1` explicitly and call the overflow
    /// panic handler (RUE-30, spec 8.1:3).
    ///
    /// For 32-bit-and-narrower types the compares run at 32-bit width:
    /// sub-word values are kept sign-extended in the low 32 bits of their
    /// registers, so comparing against the type's MIN there is exact.
    fn emit_signed_div_overflow_check(
        &mut self,
        width: crate::value_plan::IntegerWidth,
        lhs_vreg: VReg,
        rhs_vreg: VReg,
        trap_call: crate::runtime_call_plan::RuntimeCallPlan,
    ) {
        let ok_label = self.new_label();
        if width.bits == 64 {
            // divisor == -1?
            self.mir.push(X86Inst::Cmp64RI {
                src: Operand::Virtual(rhs_vreg),
                imm: -1,
            });
            self.mir.push(X86Inst::Jnz { label: ok_label });
            // dividend == i64::MIN? (doesn't fit imm32; materialize it)
            let min_vreg = self.mir.alloc_vreg();
            self.mir.push(X86Inst::MovRI64 {
                dst: Operand::Virtual(min_vreg),
                imm: i64::MIN,
            });
            self.mir.push(X86Inst::Cmp64RR {
                src1: Operand::Virtual(lhs_vreg),
                src2: Operand::Virtual(min_vreg),
            });
        } else {
            let (min_val, _) = crate::value_plan::integer_range(width);
            // divisor == -1?
            self.mir.push(X86Inst::CmpRI {
                src: Operand::Virtual(rhs_vreg),
                imm: -1,
            });
            self.mir.push(X86Inst::Jnz { label: ok_label });
            // dividend == MIN?
            self.mir.push(X86Inst::CmpRI {
                src: Operand::Virtual(lhs_vreg),
                imm: min_val as i32,
            });
        }
        self.mir.push(X86Inst::Jnz { label: ok_label });

        // Overflow - call panic handler
        let _ = self.lower_runtime_call(trap_call.clone());
        self.mir.push(X86Inst::Label { id: ok_label });
    }

    /// Materialize the shift count masked to the operand's bit width into a
    /// fresh vreg, so a sub-word variable count >= the width wraps per spec
    /// (the x86 CL shift only masks by 31/63). For 32/64-bit operands the
    /// hardware mask already matches, so the count is returned unmasked.
    fn emit_subword_narrow(&mut self, vreg: VReg, ty: Type) {
        let dst = Operand::Virtual(vreg);
        let src = Operand::Virtual(vreg);
        match crate::value_plan::type_bits(ty) {
            8 if crate::value_plan::type_is_signed(ty) => {
                self.mir.push(X86Inst::Movsx8To64 { dst, src })
            }
            8 => self.mir.push(X86Inst::Movzx8To64 { dst, src }),
            16 if crate::value_plan::type_is_signed(ty) => {
                self.mir.push(X86Inst::Movsx16To64 { dst, src })
            }
            16 => self.mir.push(X86Inst::Movzx16To64 { dst, src }),
            _ => {}
        }
    }

    /// Truncate a wrapping-arithmetic result to its declared width so the
    /// two's-complement wrap is observable (RUE-647). A sub-word wrapping
    /// `Add`/`Sub`/`Mul` is emitted as a 32-bit ALU op whose out-of-range bits
    /// (bits N..31) do not match the canonical sign/zero extension of the low
    /// N bits; re-extending fixes them. The 32-bit ops already zero bits 32..63,
    /// so 32- and 64-bit results need no narrowing.
    fn emit_wrap_narrow(&mut self, width: crate::value_plan::IntegerWidth, vreg: VReg) {
        let dst = Operand::Virtual(vreg);
        let src = Operand::Virtual(vreg);
        match (width.bits, width.signed) {
            (8, true) => self.mir.push(X86Inst::Movsx8To64 { dst, src }),
            (8, false) => self.mir.push(X86Inst::Movzx8To64 { dst, src }),
            (16, true) => self.mir.push(X86Inst::Movsx16To64 { dst, src }),
            (16, false) => self.mir.push(X86Inst::Movzx16To64 { dst, src }),
            _ => {}
        }
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

    /// Get or compute the slot vregs for a multi-slot aggregate value.
    /// Single shared implementation — see crate::agg_slots. (RUE-121)
    fn require_aggregate_slots(&mut self, value: CfgValue) -> Vec<VReg> {
        crate::agg_slots::require_aggregate_slots(self, value)
    }

    /// Lower CFG to X86Mir.
    pub fn lower(mut self) -> CompileResult<X86Mir> {
        crate::types::ensure_compact_layout_codegen_supported(
            self.ctx.cfg,
            self.ctx.type_pool,
            self.interner,
        )?;
        let ctx = self.ctx;
        let cancellation = self.cancellation;
        crate::terminator_plan::lower_cfg(
            &ctx,
            &mut self,
            None,
            return_register_banks(),
            cancellation,
        )?;
        Ok(self.mir)
    }

    /// Lower CFG to X86Mir with debug information about instruction selection.
    ///
    /// This is like `lower()` but also captures detailed information about
    /// how each CFG instruction maps to MIR instructions.
    pub fn lower_with_debug(mut self) -> CompileResult<(X86Mir, crate::LoweringDebugInfo)> {
        crate::types::ensure_compact_layout_codegen_supported(
            self.ctx.cfg,
            self.ctx.type_pool,
            self.interner,
        )?;
        let mut debug_info = crate::LoweringDebugInfo {
            fn_name: self.fn_name.to_string(),
            target_arch: "x86_64".to_string(),
            blocks: Vec::new(),
        };

        let ctx = self.ctx;
        let cancellation = self.cancellation;
        crate::terminator_plan::lower_cfg(
            &ctx,
            &mut self,
            Some(&mut debug_info),
            return_register_banks(),
            cancellation,
        )?;
        Ok((self.mir, debug_info))
    }

    /// Generate rationale for instruction lowering decisions.
    fn get_lowering_rationale(
        &self,
        kind: crate::value_plan::ValueKind,
        ty: Type,
    ) -> Option<String> {
        match kind {
            crate::value_plan::ValueKind::Constant => {
                if crate::value_plan::integer_width(ty).is_some_and(|width| width.bits <= 32) {
                    Some("32-bit immediate (zero-extends to 64-bit)".to_string())
                } else {
                    Some("64-bit immediate required".to_string())
                }
            }
            crate::value_plan::ValueKind::BinaryArithmetic => {
                if crate::value_plan::is_64_bit(ty) {
                    Some("64-bit operation with 64-bit overflow check".to_string())
                } else {
                    Some("operation with overflow check".to_string())
                }
            }
            crate::value_plan::ValueKind::Call => {
                Some("SysV ABI call uses the shared logical slot plan".to_string())
            }
            crate::value_plan::ValueKind::Parameter => {
                Some("Parameter uses the shared ABI slot plan".to_string())
            }
            crate::value_plan::ValueKind::PlaceRead | crate::value_plan::ValueKind::PlaceWrite => {
                Some("Place operation with bounds checks".to_string())
            }
            crate::value_plan::ValueKind::Shift => {
                if matches!(
                    ty.kind(),
                    TypeKind::I8 | TypeKind::I16 | TypeKind::I32 | TypeKind::I64
                ) {
                    Some("Signed shift right (SAR) preserves sign bit".to_string())
                } else {
                    Some("Unsigned shift right (SHR) zero-extends".to_string())
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
            ArithmeticOperation::FloatAdd { lhs, rhs, width }
            | ArithmeticOperation::FloatSub { lhs, rhs, width }
            | ArithmeticOperation::FloatMul { lhs, rhs, width }
            | ArithmeticOperation::FloatDiv { lhs, rhs, width } => {
                let op = match plan.operation {
                    ArithmeticOperation::FloatAdd { .. } => super::mir::FloatBinOp::Add,
                    ArithmeticOperation::FloatSub { .. } => super::mir::FloatBinOp::Sub,
                    ArithmeticOperation::FloatMul { .. } => super::mir::FloatBinOp::Mul,
                    ArithmeticOperation::FloatDiv { .. } => super::mir::FloatBinOp::Div,
                    _ => unreachable!(),
                };
                let dst = self.mir.alloc_vreg_in(crate::reg_class::RegClass::Fp);
                self.mir.push(X86Inst::FloatMov {
                    dst: Operand::Virtual(dst),
                    src: Operand::Virtual(lhs),
                    width,
                });
                self.mir.push(X86Inst::FloatBin {
                    op,
                    dst: Operand::Virtual(dst),
                    src: Operand::Virtual(rhs),
                    width,
                });
                dst
            }
            ArithmeticOperation::FloatNeg { value, width } => {
                let dst = self.mir.alloc_vreg_in(crate::reg_class::RegClass::Fp);
                self.mir.push(X86Inst::FloatNeg {
                    dst: Operand::Virtual(dst),
                    src: Operand::Virtual(value),
                    width,
                });
                dst
            }
            ArithmeticOperation::Add { lhs, rhs, width } => {
                let vreg = self.mir.alloc_vreg();
                self.mir.push(X86Inst::MovRR {
                    dst: Operand::Virtual(vreg),
                    src: Operand::Virtual(lhs),
                });
                self.mir.push(if width.bits == 64 {
                    X86Inst::AddRR64 {
                        dst: Operand::Virtual(vreg),
                        src: Operand::Virtual(rhs),
                    }
                } else {
                    X86Inst::AddRR {
                        dst: Operand::Virtual(vreg),
                        src: Operand::Virtual(rhs),
                    }
                });
                if wrap {
                    self.emit_wrap_narrow(width, vreg);
                } else {
                    self.emit_overflow_check(width, vreg, overflow_call.clone());
                }
                vreg
            }
            ArithmeticOperation::Sub { lhs, rhs, width } => {
                let vreg = self.mir.alloc_vreg();
                self.mir.push(X86Inst::MovRR {
                    dst: Operand::Virtual(vreg),
                    src: Operand::Virtual(lhs),
                });
                self.mir.push(if width.bits == 64 {
                    X86Inst::SubRR64 {
                        dst: Operand::Virtual(vreg),
                        src: Operand::Virtual(rhs),
                    }
                } else {
                    X86Inst::SubRR {
                        dst: Operand::Virtual(vreg),
                        src: Operand::Virtual(rhs),
                    }
                });
                if wrap {
                    self.emit_wrap_narrow(width, vreg);
                } else {
                    self.emit_overflow_check(width, vreg, overflow_call.clone());
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
                    // Wrapping multiply: one plain two-operand IMUL per width.
                    // Signed and unsigned agree on the low bits, and a 32-bit
                    // IMUL already zeroes the upper half; sub-32-bit widths are
                    // re-narrowed so the two's-complement wrap is observable
                    // (RUE-647). No overflow probe is emitted.
                    self.mir.push(X86Inst::MovRR {
                        dst: Operand::Virtual(vreg),
                        src: Operand::Virtual(lhs),
                    });
                    self.mir.push(if width.bits == 64 {
                        X86Inst::ImulRR64 {
                            dst: Operand::Virtual(vreg),
                            src: Operand::Virtual(rhs),
                        }
                    } else {
                        X86Inst::ImulRR {
                            dst: Operand::Virtual(vreg),
                            src: Operand::Virtual(rhs),
                        }
                    });
                    self.emit_wrap_narrow(width, vreg);
                } else if let Some((src, amount)) = shift.filter(|_| width.bits >= 32) {
                    self.mir.push(X86Inst::MovRR {
                        dst: Operand::Virtual(vreg),
                        src: Operand::Virtual(src),
                    });
                    self.mir.push(if width.bits == 64 {
                        X86Inst::ShlRI {
                            dst: Operand::Virtual(vreg),
                            imm: amount,
                        }
                    } else {
                        X86Inst::Shl32RI {
                            dst: Operand::Virtual(vreg),
                            imm: amount,
                        }
                    });
                    let check = self.mir.alloc_vreg();
                    self.mir.push(X86Inst::MovRR {
                        dst: Operand::Virtual(check),
                        src: Operand::Virtual(vreg),
                    });
                    self.mir.push(if width.signed {
                        if width.bits == 64 {
                            X86Inst::SarRI {
                                dst: Operand::Virtual(check),
                                imm: amount,
                            }
                        } else {
                            X86Inst::Sar32RI {
                                dst: Operand::Virtual(check),
                                imm: amount,
                            }
                        }
                    } else if width.bits == 64 {
                        X86Inst::ShrRI {
                            dst: Operand::Virtual(check),
                            imm: amount,
                        }
                    } else {
                        X86Inst::Shr32RI {
                            dst: Operand::Virtual(check),
                            imm: amount,
                        }
                    });
                    self.mir.push(if width.bits == 64 {
                        X86Inst::Cmp64RR {
                            src1: Operand::Virtual(check),
                            src2: Operand::Virtual(src),
                        }
                    } else {
                        X86Inst::CmpRR {
                            src1: Operand::Virtual(check),
                            src2: Operand::Virtual(src),
                        }
                    });
                    let ok = self.new_label();
                    self.mir.push(X86Inst::Jz { label: ok });
                    let _ = self.lower_runtime_call(overflow_call.clone());
                    self.mir.push(X86Inst::Label { id: ok });
                } else if !width.signed && width.bits >= 32 {
                    self.mir.push(X86Inst::MovRR {
                        dst: Operand::Physical(Reg::Rax),
                        src: Operand::Virtual(lhs),
                    });
                    self.mir.push(if width.bits == 64 {
                        X86Inst::Mul64R {
                            src: Operand::Virtual(rhs),
                        }
                    } else {
                        X86Inst::MulR {
                            src: Operand::Virtual(rhs),
                        }
                    });
                    self.mir.push(X86Inst::MovRR {
                        dst: Operand::Virtual(vreg),
                        src: Operand::Physical(Reg::Rax),
                    });
                    self.emit_overflow_check(width, vreg, overflow_call.clone());
                } else {
                    self.mir.push(X86Inst::MovRR {
                        dst: Operand::Virtual(vreg),
                        src: Operand::Virtual(lhs),
                    });
                    self.mir.push(if width.bits == 64 {
                        X86Inst::ImulRR64 {
                            dst: Operand::Virtual(vreg),
                            src: Operand::Virtual(rhs),
                        }
                    } else {
                        X86Inst::ImulRR {
                            dst: Operand::Virtual(vreg),
                            src: Operand::Virtual(rhs),
                        }
                    });
                    self.emit_overflow_check(width, vreg, overflow_call.clone());
                }
                vreg
            }
            ArithmeticOperation::Div { lhs, rhs, width } => {
                let vreg = self.mir.alloc_vreg();
                let ok = self.new_label();
                self.mir.push(if width.bits == 64 {
                    X86Inst::Test64RR {
                        src1: Operand::Virtual(rhs),
                        src2: Operand::Virtual(rhs),
                    }
                } else {
                    X86Inst::TestRR {
                        src1: Operand::Virtual(rhs),
                        src2: Operand::Virtual(rhs),
                    }
                });
                self.mir.push(X86Inst::Jnz { label: ok });
                let _ = self.lower_runtime_call(div_by_zero_call.clone());
                self.mir.push(X86Inst::Label { id: ok });
                if width.signed {
                    self.emit_signed_div_overflow_check(width, lhs, rhs, overflow_call.clone());
                }
                self.mir.push(X86Inst::MovRR {
                    dst: Operand::Physical(Reg::Rax),
                    src: Operand::Virtual(lhs),
                });
                self.emit_div_core(width.bits == 64, width.signed, rhs);
                self.mir.push(X86Inst::MovRR {
                    dst: Operand::Virtual(vreg),
                    src: Operand::Physical(Reg::Rax),
                });
                vreg
            }
            ArithmeticOperation::Mod { lhs, rhs, width } => {
                let vreg = self.mir.alloc_vreg();
                let ok = self.new_label();
                self.mir.push(if width.bits == 64 {
                    X86Inst::Test64RR {
                        src1: Operand::Virtual(rhs),
                        src2: Operand::Virtual(rhs),
                    }
                } else {
                    X86Inst::TestRR {
                        src1: Operand::Virtual(rhs),
                        src2: Operand::Virtual(rhs),
                    }
                });
                self.mir.push(X86Inst::Jnz { label: ok });
                let _ = self.lower_runtime_call(div_by_zero_call.clone());
                self.mir.push(X86Inst::Label { id: ok });
                if width.signed {
                    self.emit_signed_div_overflow_check(width, lhs, rhs, overflow_call.clone());
                }
                self.mir.push(X86Inst::MovRR {
                    dst: Operand::Physical(Reg::Rax),
                    src: Operand::Virtual(lhs),
                });
                self.emit_div_core(width.bits == 64, width.signed, rhs);
                self.mir.push(X86Inst::MovRR {
                    dst: Operand::Virtual(vreg),
                    src: Operand::Physical(Reg::Rdx),
                });
                vreg
            }
            ArithmeticOperation::Neg { value, width } => {
                let vreg = self.mir.alloc_vreg();
                self.mir.push(X86Inst::MovRR {
                    dst: Operand::Virtual(vreg),
                    src: Operand::Virtual(value),
                });
                self.mir.push(if width.bits == 64 {
                    X86Inst::Neg64 {
                        dst: Operand::Virtual(vreg),
                    }
                } else {
                    X86Inst::Neg {
                        dst: Operand::Virtual(vreg),
                    }
                });
                self.emit_overflow_check(width, vreg, overflow_call.clone());
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
        self.mir.push(X86Inst::MovRI32 {
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
        let num_stack_args = plan.stack_slot_count;
        let has_fp_stack_arg =
            plan.abi_classes
                .iter()
                .zip(&plan.abi_locations)
                .any(|(class, location)| {
                    matches!(class, crate::call_plan::AbiSlotClass::Fp(_))
                        && matches!(location, crate::call_plan::AbiSlotLocation::Stack { .. })
                });
        let stack_cell_bytes = checked_cell_region_bytes(
            u64::try_from(num_stack_args).expect("call stack slot count must fit u64"),
        )
        .expect("call stack area must fit displacement");
        let needs_alignment = plan.stack_bytes > stack_cell_bytes;
        if has_fp_stack_arg {
            self.mir.push(X86Inst::AddRI {
                dst: Operand::Physical(Reg::Rsp),
                imm: -checked_displacement_bytes(u64::from(plan.stack_bytes))
                    .expect("call stack area must fit displacement"),
            });
            for ((arg, class), location) in plan
                .abi_slots
                .iter()
                .zip(&plan.abi_classes)
                .zip(&plan.abi_locations)
            {
                let crate::call_plan::AbiSlotLocation::Stack { offset, .. } = location else {
                    continue;
                };
                let offset = checked_displacement_bytes(u64::from(*offset))
                    .expect("call stack slot offset must fit displacement");
                if let crate::call_plan::AbiSlotClass::Fp(width) = class {
                    self.mir.push(X86Inst::FloatStore {
                        base: Reg::Rsp,
                        offset,
                        src: Operand::Virtual(*arg),
                        width: *width,
                    });
                } else {
                    self.mir.push(X86Inst::MovMR {
                        base: Reg::Rsp,
                        offset,
                        src: Operand::Virtual(*arg),
                    });
                }
            }
        } else if needs_alignment {
            self.mir.push(X86Inst::AddRI {
                dst: Operand::Physical(Reg::Rsp),
                imm: -i32::try_from(
                    checked_cell_region_padding_bytes(
                        u64::try_from(num_stack_args).expect("call stack slot count must fit u64"),
                    )
                    .expect("call stack alignment padding must fit displacement"),
                )
                .expect("call stack alignment padding must fit i32"),
            });
        }
        if !has_fp_stack_arg {
            for (arg, location) in plan.abi_slots.iter().zip(&plan.abi_locations).rev() {
                let crate::call_plan::AbiSlotLocation::Stack { offset, size, .. } = location else {
                    continue;
                };
                // Pushing the stacked slots in reverse leaves slot 0 nearest
                // `rsp`, which is the offset the assignment gave it: the push
                // sequence is only correct while the area is whole ascending
                // eightbytes, which is what a native slot claims.
                assert_eq!(
                    (*size, offset % 8),
                    (8, 0),
                    "a pushed native stack slot occupies one whole eightbyte"
                );
                self.mir.push(X86Inst::MovRR {
                    dst: Operand::Physical(Reg::Rax),
                    src: Operand::Virtual(*arg),
                });
                self.mir.push(X86Inst::Push {
                    src: Operand::Physical(Reg::Rax),
                });
            }
        }
        let gp_args: Vec<_> = plan
            .abi_slots
            .iter()
            .zip(&plan.abi_locations)
            .filter_map(|(arg, location)| match location {
                crate::call_plan::AbiSlotLocation::GpReg(index) => Some((*index, *arg)),
                _ => None,
            })
            .collect();
        let fp_args: Vec<_> = plan
            .abi_slots
            .iter()
            .zip(&plan.abi_classes)
            .zip(&plan.abi_locations)
            .filter_map(|((arg, class), location)| match (class, location) {
                (
                    crate::call_plan::AbiSlotClass::Fp(width),
                    crate::call_plan::AbiSlotLocation::FpReg(index),
                ) => Some((*index, *arg, *width)),
                _ => None,
            })
            .collect();
        for (_, arg) in gp_args.iter().rev() {
            self.mir.push(X86Inst::MovRR {
                dst: Operand::Physical(Reg::Rax),
                src: Operand::Virtual(*arg),
            });
            self.mir.push(X86Inst::Push {
                src: Operand::Physical(Reg::Rax),
            });
        }
        for (index, _) in &gp_args {
            self.mir.push(X86Inst::Pop {
                dst: Operand::Physical(ARG_REGS[*index]),
            });
        }
        // Regalloc reloads a spilled FloatMov source through xmm0. Populate
        // the FP argument bank from high to low so such a reload happens
        // before xmm0 receives its final ABI value. The allocator reserves
        // xmm0-xmm7, so no other source can alias a destination in this bank.
        for (index, arg, width) in fp_args.into_iter().rev() {
            self.mir.push(X86Inst::FloatMov {
                dst: Operand::Physical(FP_ARG_REGS[index]),
                src: Operand::Virtual(arg),
                width,
            });
        }
        let symbol_id = self.intern_symbol(plan.target.symbol());
        self.mir.push(X86Inst::call(symbol_id));
        if num_stack_args > 0 || needs_alignment {
            self.mir.push(X86Inst::AddRI {
                dst: Operand::Physical(Reg::Rsp),
                imm: checked_displacement_bytes(u64::from(plan.stack_bytes))
                    .expect("call stack area must fit displacement"),
            });
        }
        // Free the by-value indirect argument buffers (RUE-1005), restoring rsp
        // to the sret storage (or the pre-call baseline) before the sret
        // read-back reads its buffer at a fixed offset.
        if plan.caller_indirect_bytes > 0 {
            self.mir.push(X86Inst::AddRI {
                dst: Operand::Physical(Reg::Rsp),
                imm: checked_displacement_bytes(u64::from(plan.caller_indirect_bytes))
                    .expect("indirect argument area must fit displacement"),
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
                    self.mir.push(X86Inst::MovRR {
                        dst: Operand::Virtual(base),
                        src: Operand::Physical(Reg::Rsp),
                    });
                    crate::agg_slots::load_enum_slots_through_ptr(self, base, map)
                } else if let Some(image) = &plan.compact_return_dispatch {
                    // Heterogeneous compact aggregate return (RUE-1037): dispatch on
                    // the tag and read the active variant's image from the sret buffer.
                    let base = self.mir.alloc_vreg();
                    self.mir.push(X86Inst::MovRR {
                        dst: Operand::Virtual(base),
                        src: Operand::Physical(Reg::Rsp),
                    });
                    crate::agg_slots::load_dispatch_image(self, base, image)
                } else {
                    (0..slot_count)
                        .map(|index| {
                            let slot = self.mir.alloc_vreg();
                            self.mir.push(X86Inst::MovRM {
                                dst: Operand::Virtual(slot),
                                base: Reg::Rsp,
                                offset: checked_cell_byte_offset(index as u64)
                                    .expect("sret slot offset must fit displacement"),
                            });
                            slot
                        })
                        .collect()
                };
                self.mir.push(X86Inst::AddRI {
                    dst: Operand::Physical(Reg::Rsp),
                    imm: checked_displacement_bytes(u64::from(storage_bytes))
                        .expect("sret storage must fit displacement"),
                });
                slots
            }
            ReturnPlan::Registers { slot_count } => {
                assert_eq!(
                    plan.return_slot_regs.len(),
                    slot_count as usize,
                    "a register return must name a return register for every slot"
                );
                plan.return_slot_regs
                    .iter()
                    .map(|reg| match *reg {
                        crate::call_plan::ReturnSlotReg::Gp(index) => {
                            let slot = self.mir.alloc_vreg();
                            self.mir.push(X86Inst::MovRR {
                                dst: Operand::Virtual(slot),
                                src: Operand::Physical(RET_REGS[index]),
                            });
                            slot
                        }
                        crate::call_plan::ReturnSlotReg::Fp { index, width } => {
                            let slot = self.mir.alloc_vreg_in(crate::reg_class::RegClass::Fp);
                            self.mir.push(X86Inst::FloatMov {
                                dst: Operand::Virtual(slot),
                                src: Operand::Physical(FP_RET_REGS[index]),
                                width,
                            });
                            slot
                        }
                    })
                    .collect()
            }
            ReturnPlan::Scalar => Vec::new(),
            ReturnPlan::ZeroSized => Vec::new(),
        };
        if let Some(&slot) = slots.first() {
            // The primary vreg mirrors logical slot 0, so the move that syncs it
            // has to be the one for that slot's class.
            self.mir.push(
                if self.mir.vreg_class(slot) == crate::reg_class::RegClass::Fp {
                    X86Inst::FloatMov {
                        dst: Operand::Virtual(primary),
                        src: Operand::Virtual(slot),
                        width: plan
                            .result_float_width
                            .expect("FP aggregate return slot width"),
                    }
                } else {
                    X86Inst::MovRR {
                        dst: Operand::Virtual(primary),
                        src: Operand::Virtual(slot),
                    }
                },
            );
        } else if matches!(plan.return_plan, ReturnPlan::Scalar) {
            self.mir.push(
                if self.mir.vreg_class(primary) == crate::reg_class::RegClass::Fp {
                    X86Inst::FloatMov {
                        dst: Operand::Virtual(primary),
                        src: Operand::Physical(Reg::Xmm0),
                        width: plan.result_float_width.expect("FP call result width"),
                    }
                } else {
                    X86Inst::MovRR {
                        dst: Operand::Virtual(primary),
                        src: Operand::Physical(Reg::Rax),
                    }
                },
            );
        } else if matches!(plan.return_plan, ReturnPlan::ZeroSized) {
            self.mir.push(X86Inst::MovRI32 {
                dst: Operand::Virtual(primary),
                imm: 0,
            });
        }
        crate::value_plan::MaterializedValue { primary, slots }
    }

    /// Extend a foreign scalar return (in `vreg`) to its canonical 64-bit form
    /// per the target-C classifier (ADR-0064 P2). The narrow value occupies the
    /// low bits of the return register with unspecified high bits; this restores
    /// the sign/zero extension Rue's scalar invariant relies on.
    fn emit_c_return_extension(&mut self, vreg: VReg, ext: rue_air::ScalarAbiExtension) {
        use rue_air::ScalarAbiExtension;
        let dst = Operand::Virtual(vreg);
        let src = Operand::Virtual(vreg);
        match ext {
            ScalarAbiExtension::None => {}
            ScalarAbiExtension::Signed { from_bits: 8 } => {
                self.mir.push(X86Inst::Movsx8To64 { dst, src })
            }
            ScalarAbiExtension::Signed { from_bits: 16 } => {
                self.mir.push(X86Inst::Movsx16To64 { dst, src })
            }
            ScalarAbiExtension::Signed { from_bits: 32 } => {
                self.mir.push(X86Inst::Movsx32To64 { dst, src })
            }
            ScalarAbiExtension::Unsigned { from_bits: 8 } => {
                self.mir.push(X86Inst::Movzx8To64 { dst, src })
            }
            ScalarAbiExtension::Unsigned { from_bits: 16 } => {
                self.mir.push(X86Inst::Movzx16To64 { dst, src })
            }
            ScalarAbiExtension::Unsigned { from_bits: 32 } => {
                self.mir.push(X86Inst::Movzx32To64 { dst, src });
            }
            ScalarAbiExtension::Signed { from_bits }
            | ScalarAbiExtension::Unsigned { from_bits } => {
                panic!("unexpected target-C scalar extension width {from_bits}")
            }
        }
    }

    /// Materialize an aggregate argument's eightbytes (ADR-0064 P3): write its
    /// native slots into a scratch buffer as the compact C image, then load the
    /// whole eightbytes back so they pack in ascending C field order. The scratch
    /// buffer is freed immediately — a register/byval aggregate argument's bytes
    /// live only in the returned eightbyte vregs, so this is rsp-neutral.
    fn image_arg_eightbytes(
        &mut self,
        value: CfgValue,
        image: &crate::foreign_call::AggregateImage,
    ) -> Vec<VReg> {
        self.mir.push(X86Inst::AddRI {
            dst: Operand::Physical(Reg::Rsp),
            imm: -checked_displacement_bytes(u64::from(image.storage_bytes))
                .expect("foreign argument storage must fit displacement"),
        });
        let buf = self.mir.alloc_vreg();
        self.mir.push(X86Inst::MovRR {
            dst: Operand::Virtual(buf),
            src: Operand::Physical(Reg::Rsp),
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
        self.mir.push(X86Inst::AddRI {
            dst: Operand::Physical(Reg::Rsp),
            imm: checked_displacement_bytes(u64::from(image.storage_bytes))
                .expect("foreign argument storage must fit displacement"),
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
                if let Some(float_width) = crate::value_plan::float_width(ty) {
                    let dst = self.mir.alloc_vreg_in(crate::reg_class::RegClass::Fp);
                    self.mir.push(X86Inst::FloatConst {
                        dst: Operand::Virtual(dst),
                        bits: value,
                        width: float_width,
                    });
                    return ValueResult::Materialized(crate::value_plan::MaterializedValue {
                        primary: dst,
                        slots: Vec::new(),
                    });
                }
                let dst = self.mir.alloc_vreg();
                if value <= u32::MAX as u64 {
                    self.mir.push(X86Inst::MovRI32 {
                        dst: Operand::Virtual(dst),
                        imm: value as i32,
                    });
                } else {
                    self.mir.push(X86Inst::MovRI64 {
                        dst: Operand::Virtual(dst),
                        imm: value as i64,
                    });
                }
                dst
            }
            ResidualValuePlan::BoolConst { value } => {
                let dst = self.mir.alloc_vreg();
                self.mir.push(X86Inst::MovRI32 {
                    dst: Operand::Virtual(dst),
                    imm: value as i32,
                });
                dst
            }
            ResidualValuePlan::StringConst { string_id } => {
                let ptr = self.mir.alloc_vreg();
                let len = self.mir.alloc_vreg();
                self.mir.push(X86Inst::StringConstPtr {
                    dst: Operand::Virtual(ptr),
                    string_id,
                });
                self.mir.push(X86Inst::StringConstLen {
                    dst: Operand::Virtual(len),
                    string_id,
                });
                // A `str` view is `{ptr, len}`; an owned `StrBuf` header is
                // `{buf, cap, len}` — the `RawBuf(u8)` core's `{buf, cap}` then
                // the length (RUE-1066), so `cap` precedes `len`.
                if plan.policy.shape.slot_count() >= 3 {
                    let cap = self.mir.alloc_vreg();
                    self.mir.push(X86Inst::StringConstCap {
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
            ResidualValuePlan::BlockParam { index: _ } => {
                panic!("block parameter must be preallocated")
            }
            ResidualValuePlan::Not { value } => {
                let dst = self.mir.alloc_vreg();
                self.mir.push(X86Inst::MovRR {
                    dst: Operand::Virtual(dst),
                    src: Operand::Virtual(value),
                });
                self.mir.push(X86Inst::XorRI {
                    dst: Operand::Virtual(dst),
                    imm: 1,
                });
                dst
            }
            ResidualValuePlan::BitNot { value } => {
                let dst = self.mir.alloc_vreg();
                self.mir.push(X86Inst::MovRR {
                    dst: Operand::Virtual(dst),
                    src: Operand::Virtual(value),
                });
                self.mir.push(if width.is_some_and(|w| w.bits == 64) {
                    X86Inst::Not64R {
                        dst: Operand::Virtual(dst),
                    }
                } else {
                    X86Inst::NotR {
                        dst: Operand::Virtual(dst),
                    }
                });
                self.emit_subword_narrow(dst, ty);
                dst
            }
            ResidualValuePlan::Bitwise { op, lhs, rhs } => {
                let dst = self.mir.alloc_vreg();
                self.mir.push(X86Inst::MovRR {
                    dst: Operand::Virtual(dst),
                    src: Operand::Virtual(lhs),
                });
                self.mir
                    .push(match (op, width.is_some_and(|w| w.bits == 64)) {
                        (BitwiseOp::And, true) => X86Inst::And64RR {
                            dst: Operand::Virtual(dst),
                            src: Operand::Virtual(rhs),
                        },
                        (BitwiseOp::And, false) => X86Inst::AndRR {
                            dst: Operand::Virtual(dst),
                            src: Operand::Virtual(rhs),
                        },
                        (BitwiseOp::Or, true) => X86Inst::Or64RR {
                            dst: Operand::Virtual(dst),
                            src: Operand::Virtual(rhs),
                        },
                        (BitwiseOp::Or, false) => X86Inst::OrRR {
                            dst: Operand::Virtual(dst),
                            src: Operand::Virtual(rhs),
                        },
                        (BitwiseOp::Xor, true) => X86Inst::Xor64RR {
                            dst: Operand::Virtual(dst),
                            src: Operand::Virtual(rhs),
                        },
                        (BitwiseOp::Xor, false) => X86Inst::XorRR {
                            dst: Operand::Virtual(dst),
                            src: Operand::Virtual(rhs),
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
                self.mir.push(X86Inst::MovRR {
                    dst: Operand::Virtual(dst),
                    src: Operand::Virtual(lhs),
                });
                let mask = plan.policy.shift_count_mask.expect("shift count mask");
                if let Some(value) = constant {
                    let imm = (value & mask) as u8;
                    self.mir.push(
                        match (
                            op,
                            width.is_some_and(|w| w.bits == 64),
                            width.is_some_and(|w| w.signed),
                        ) {
                            (ShiftOp::Left, true, _) => X86Inst::ShlRI {
                                dst: Operand::Virtual(dst),
                                imm,
                            },
                            (ShiftOp::Left, false, _) => X86Inst::Shl32RI {
                                dst: Operand::Virtual(dst),
                                imm,
                            },
                            (ShiftOp::Right, true, true) => X86Inst::SarRI {
                                dst: Operand::Virtual(dst),
                                imm,
                            },
                            (ShiftOp::Right, false, true) => X86Inst::Sar32RI {
                                dst: Operand::Virtual(dst),
                                imm,
                            },
                            (ShiftOp::Right, true, false) => X86Inst::ShrRI {
                                dst: Operand::Virtual(dst),
                                imm,
                            },
                            (ShiftOp::Right, false, false) => X86Inst::Shr32RI {
                                dst: Operand::Virtual(dst),
                                imm,
                            },
                        },
                    );
                } else {
                    let count = self.emit_masked_shift_count_vreg(rhs, mask);
                    self.mir.push(X86Inst::MovRR {
                        dst: Operand::Physical(Reg::Rcx),
                        src: Operand::Virtual(count),
                    });
                    self.mir.push(
                        match (
                            op,
                            width.is_some_and(|w| w.bits == 64),
                            width.is_some_and(|w| w.signed),
                        ) {
                            (ShiftOp::Left, true, _) => X86Inst::ShlRCl {
                                dst: Operand::Virtual(dst),
                            },
                            (ShiftOp::Left, false, _) => X86Inst::Shl32RCl {
                                dst: Operand::Virtual(dst),
                            },
                            (ShiftOp::Right, true, true) => X86Inst::SarRCl {
                                dst: Operand::Virtual(dst),
                            },
                            (ShiftOp::Right, false, true) => X86Inst::Sar32RCl {
                                dst: Operand::Virtual(dst),
                            },
                            (ShiftOp::Right, true, false) => X86Inst::ShrRCl {
                                dst: Operand::Virtual(dst),
                            },
                            (ShiftOp::Right, false, false) => X86Inst::Shr32RCl {
                                dst: Operand::Virtual(dst),
                            },
                        },
                    );
                }
                self.emit_subword_narrow(dst, ty);
                dst
            }
            ResidualValuePlan::Comparison {
                op,
                lhs,
                rhs,
                equality_leaves,
                runtime_call,
            } => self.lower_comparison(op, lhs, rhs, plan.policy, &equality_leaves, runtime_call),
            ResidualValuePlan::Alloc {
                slot,
                init,
                init_shape,
                float_width,
                leaf_types,
            } => {
                if init_shape.slot_count() == 0 {
                    return ValueResult::SideEffect;
                } else if init.slots.is_empty() {
                    if let Some(width) = float_width {
                        self.mir.push(X86Inst::FloatStore {
                            base: Reg::Rbp,
                            offset: self.ctx.local_offset(slot),
                            src: Operand::Virtual(init.primary),
                            width,
                        });
                    } else {
                        self.emit_store_slot(init.primary, slot);
                    }
                } else {
                    crate::agg_slots::store_slots_typed(self, &init.slots, slot, &leaf_types);
                }
                return ValueResult::SideEffect;
            }
            ResidualValuePlan::Load {
                slot,
                float_width,
                leaf_types,
            } => {
                let count = plan.policy.shape.slot_count();
                if count == 0 {
                    self.mir.alloc_vreg()
                } else if count > 1 {
                    slots = crate::agg_slots::load_slots_at_low_typed(
                        self,
                        slot + count - 1,
                        &leaf_types,
                    );
                    let dst = slots[0];
                    dst
                } else {
                    let dst = self.mir.alloc_vreg_in(if float_width.is_some() {
                        crate::reg_class::RegClass::Fp
                    } else {
                        crate::reg_class::RegClass::Gp
                    });
                    if let Some(width) = float_width {
                        self.mir.push(X86Inst::FloatLoad {
                            dst: Operand::Virtual(dst),
                            base: Reg::Rbp,
                            offset: self.ctx.local_offset(slot),
                            width,
                        });
                    } else {
                        self.emit_load_slot(dst, slot);
                    }
                    dst
                }
            }
            ResidualValuePlan::Store {
                destination,
                value,
                value_shape,
                float_width,
                leaf_types,
            } => {
                if value_shape.slot_count() == 0 {
                    return ValueResult::SideEffect;
                }
                if value.slots.is_empty() {
                    match destination {
                        crate::value_plan::StoreDestination::FrameSlot(slot) => {
                            if let Some(width) = float_width {
                                self.mir.push(X86Inst::FloatStore {
                                    base: Reg::Rbp,
                                    offset: self.ctx.local_offset(slot),
                                    src: Operand::Virtual(value.primary),
                                    width,
                                });
                            } else {
                                self.emit_store_slot(value.primary, slot);
                            }
                        }
                        crate::value_plan::StoreDestination::ByRefParam(param_slot) => {
                            let ptr = self.ensure_by_ref_param_ptr(param_slot);
                            if let Some(width) = float_width {
                                <Self as crate::agg_slots::SlotBackend>::emit_float_store_through_ptr(
                                    self, value.primary, ptr, 0, width,
                                );
                            } else {
                                self.emit_store_ptr_base(value.primary, ptr);
                            }
                        }
                    }
                } else {
                    match destination {
                        crate::value_plan::StoreDestination::FrameSlot(slot) => {
                            crate::agg_slots::store_slots_typed(
                                self,
                                &value.slots,
                                slot,
                                &leaf_types,
                            )
                        }
                        crate::value_plan::StoreDestination::ByRefParam(param_slot) => {
                            let ptr = self.ensure_by_ref_param_ptr(param_slot);
                            crate::agg_slots::store_slots_through_ptr_typed(
                                self,
                                &value.slots,
                                ptr,
                                0,
                                &leaf_types,
                            );
                        }
                    }
                }
                return ValueResult::SideEffect;
            }
            ResidualValuePlan::ParamStore {
                param_slot,
                value,
                value_shape,
                float_width,
                leaf_types,
            } => {
                if value_shape.slot_count() == 0 {
                    return ValueResult::SideEffect;
                }
                if self.ctx.cfg.is_param_by_ref(param_slot) {
                    let ptr = self.ensure_by_ref_param_ptr(param_slot);
                    if value.slots.is_empty() {
                        if let Some(width) = float_width {
                            <Self as crate::agg_slots::SlotBackend>::emit_float_store_through_ptr(
                                self,
                                value.primary,
                                ptr,
                                0,
                                width,
                            );
                        } else {
                            self.emit_store_ptr_base(value.primary, ptr);
                        }
                    } else {
                        crate::agg_slots::store_slots_through_ptr_typed(
                            self,
                            &value.slots,
                            ptr,
                            0,
                            &leaf_types,
                        );
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
                    if vals.len() == 1
                        && let Some(width) = float_width
                    {
                        <Self as crate::place_lower::PlaceLowerBackend>::emit_float_store_slot(
                            self,
                            vals[0],
                            self.ctx.param_frame_slot(param_slot),
                            width,
                        );
                    } else {
                        crate::agg_slots::store_slots_typed(
                            self,
                            &vals,
                            self.ctx.param_frame_slot(param_slot),
                            &leaf_types,
                        );
                    }
                }
                return ValueResult::SideEffect;
            }
            ResidualValuePlan::PlaceRead { place, leaf_types } => {
                let count = plan.policy.shape.slot_count();
                if count > 1 {
                    let addr = self.mir.alloc_vreg();
                    crate::place_lower::lower_checked_place_addr_plan(self, addr, &place);
                    slots = crate::agg_slots::load_slots_through_ptr_typed(self, addr, &leaf_types);
                    let dst = slots[0];
                    dst
                } else {
                    let dst =
                        self.mir
                            .alloc_vreg_in(if plan.policy.primary_float_width.is_some() {
                                crate::reg_class::RegClass::Fp
                            } else {
                                crate::reg_class::RegClass::Gp
                            });
                    crate::place_lower::lower_place_read_plan(self, dst, &place, ty);
                    dst
                }
            }
            ResidualValuePlan::PlaceWrite {
                place,
                value,
                value_shape,
                float_width,
                leaf_types,
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
                crate::place_lower::lower_place_write_plan(
                    self,
                    &place,
                    &vals,
                    float_width,
                    &leaf_types,
                );
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
                // The primary vreg mirrors logical slot 0, so it takes that
                // slot's register class and is written with that class's move
                //. The zero placeholders are general-purpose: nothing
                // reads them as a value.
                match (
                    plan.policy.aggregate_primary,
                    slots.first().copied(),
                    plan.policy.primary_float_width,
                ) {
                    (crate::value_plan::AggregatePrimary::FirstSlot, Some(first), Some(width)) => {
                        let dst = self.mir.alloc_vreg_in(crate::reg_class::RegClass::Fp);
                        self.mir.push(X86Inst::FloatMov {
                            dst: Operand::Virtual(dst),
                            src: Operand::Virtual(first),
                            width,
                        });
                        dst
                    }
                    (crate::value_plan::AggregatePrimary::FirstSlot, Some(first), None) => {
                        let dst = self.mir.alloc_vreg();
                        self.mir.push(X86Inst::MovRR {
                            dst: Operand::Virtual(dst),
                            src: Operand::Virtual(first),
                        });
                        dst
                    }
                    _ => {
                        let dst = self.mir.alloc_vreg();
                        self.mir.push(X86Inst::MovRI32 {
                            dst: Operand::Virtual(dst),
                            imm: 0,
                        });
                        dst
                    }
                }
            }
            ResidualValuePlan::EnumVariant {
                variant_index,
                payload,
                total_slots,
                leaf_types,
                zero_unused_payload,
                ..
            } => {
                assert_eq!(leaf_types.len(), total_slots as usize);
                slots = leaf_types
                    .iter()
                    .map(|ty| {
                        self.mir
                            .alloc_vreg_in(if crate::value_plan::float_width(*ty).is_some() {
                                crate::reg_class::RegClass::Fp
                            } else {
                                crate::reg_class::RegClass::Gp
                            })
                    })
                    .collect();
                self.mir.push(X86Inst::MovRI32 {
                    dst: Operand::Virtual(slots[0]),
                    imm: variant_index as i32,
                });
                let mut offset = 1;
                for (value, shape, value_leaf_types) in payload {
                    if shape.slot_count() == 0 {
                        continue;
                    }
                    let values = if value.slots.is_empty() {
                        vec![value.primary]
                    } else {
                        value.slots
                    };
                    assert_eq!(values.len(), value_leaf_types.len());
                    for (value, value_ty) in values.into_iter().zip(value_leaf_types) {
                        if offset < slots.len() {
                            if let Some(width) = crate::value_plan::float_width(value_ty) {
                                assert_eq!(
                                    self.mir.vreg_class(value),
                                    crate::reg_class::RegClass::Fp,
                                    "float enum payload source must use the FP class"
                                );
                                self.mir.push(X86Inst::FloatToBits {
                                    dst: Operand::Virtual(slots[offset]),
                                    src: Operand::Virtual(value),
                                    width,
                                });
                            } else {
                                assert_eq!(
                                    self.mir.vreg_class(value),
                                    crate::reg_class::RegClass::Gp,
                                    "enum payload slot class must match its canonical leaf type"
                                );
                                self.mir.push(X86Inst::MovRR {
                                    dst: Operand::Virtual(slots[offset]),
                                    src: Operand::Virtual(value),
                                });
                            }
                            offset += 1;
                        }
                    }
                }
                // Zero the payload slots this (shorter) variant does not write, so a
                // compact memory image marshalled from the value carries no residue
                // from a wider variant (ADR-0052 ruling 5). Only under the gate.
                if zero_unused_payload {
                    for (index, slot) in slots.iter().enumerate().skip(offset) {
                        if let Some(width) = crate::value_plan::float_width(leaf_types[index]) {
                            self.mir.push(X86Inst::FloatConst {
                                dst: Operand::Virtual(*slot),
                                bits: 0,
                                width,
                            });
                        } else {
                            self.mir.push(X86Inst::MovRI32 {
                                dst: Operand::Virtual(*slot),
                                imm: 0,
                            });
                        }
                    }
                }
                let dst = slots[0];
                dst
            }
            ResidualValuePlan::EnumPayloadGet {
                base_slots,
                field_offset,
                field_slots,
                field_types,
            } => {
                assert_eq!(field_types.len(), field_slots as usize);
                slots.clear();
                for index in 0..field_slots {
                    let ty = field_types[index as usize];
                    let width = crate::value_plan::float_width(ty);
                    let dst = self.mir.alloc_vreg_in(if width.is_some() {
                        crate::reg_class::RegClass::Fp
                    } else {
                        crate::reg_class::RegClass::Gp
                    });
                    let src = base_slots[(field_offset + index) as usize];
                    if let Some(width) = width {
                        self.mir.push(X86Inst::BitsToFloat {
                            dst: Operand::Virtual(dst),
                            src: Operand::Virtual(src),
                            width,
                        });
                    } else {
                        self.mir.push(X86Inst::MovRR {
                            dst: Operand::Virtual(dst),
                            src: Operand::Virtual(src),
                        });
                    }
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
                self.mir.push(X86Inst::MovRR {
                    dst: Operand::Virtual(dst),
                    src: Operand::Virtual(value),
                });
                let to_width = width.unwrap();
                self.emit_int_cast_check(value, from_width, to_width, trap_call);
                if from_width.signed && to_width.bits > from_width.bits {
                    let src = Operand::Virtual(value);
                    let dst = Operand::Virtual(dst);
                    self.mir.push(match from_width.bits {
                        8 => X86Inst::Movsx8To64 { dst, src },
                        16 => X86Inst::Movsx16To64 { dst, src },
                        _ => X86Inst::Movsx32To64 { dst, src },
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
        ty: Type,
        policy: crate::value_plan::ValuePlan,
    ) -> (VReg, Vec<VReg>) {
        let float_width = crate::value_plan::float_width(ty);
        let dst = self.mir.alloc_vreg_in(if float_width.is_some() {
            crate::reg_class::RegClass::Fp
        } else {
            crate::reg_class::RegClass::Gp
        });
        let count = policy.shape.slot_count();
        if count == 0 {
            self.mir.push(X86Inst::MovRI32 {
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
                        self.mir.push(X86Inst::MovRMIndexed {
                            dst: Operand::Virtual(v),
                            base: ptr,
                            offset: (slot * 8) as i32,
                        });
                        v
                    })
                    .collect();
                return (slots[0], slots);
            }
            if let Some(width) = float_width {
                <Self as crate::place_lower::PlaceLowerBackend>::emit_load_ptr_base(
                    self,
                    dst,
                    ptr,
                    Some(width),
                );
            } else {
                self.mir.push(X86Inst::MovRMIndexed {
                    dst: Operand::Virtual(dst),
                    base: ptr,
                    offset: 0,
                });
            }
        } else if count > 1 {
            let leaf_types = crate::types::aggregate_leaf_types(self.ctx.type_pool, ty);
            let slots: Vec<_> = (0..count)
                .map(|slot| {
                    let width = crate::value_plan::float_width(leaf_types[slot as usize]);
                    let v = self.mir.alloc_vreg_in(if width.is_some() {
                        crate::reg_class::RegClass::Fp
                    } else {
                        crate::reg_class::RegClass::Gp
                    });
                    let frame_slot = self.ctx.param_frame_slot(index) + count - 1 - slot;
                    if let Some(width) = width {
                        <Self as crate::place_lower::PlaceLowerBackend>::emit_float_load_slot(
                            self, v, frame_slot, width,
                        );
                    } else {
                        self.mir.push(X86Inst::MovRM {
                            dst: Operand::Virtual(v),
                            base: Reg::Rbp,
                            offset: self.ctx.local_offset(frame_slot),
                        });
                    }
                    v
                })
                .collect();
            return (slots[0], slots);
        } else if let Some(&vreg) = self.param_reg_vregs.get(&index) {
            // Register-only scalar (RUE-1170): the entry preamble copied the
            // argument register into one read-only vreg shared by every read.
            let _ = ty;
            return (vreg, Vec::new());
        } else if let Some(width) = float_width {
            <Self as crate::place_lower::PlaceLowerBackend>::emit_float_load_slot(
                self,
                dst,
                self.ctx.param_frame_slot(index),
                width,
            );
        } else {
            self.mir.push(X86Inst::MovRM {
                dst: Operand::Virtual(dst),
                base: Reg::Rbp,
                offset: self.ctx.local_offset(self.ctx.param_frame_slot(index)),
            });
        }
        (dst, Vec::new())
    }

    fn emit_masked_shift_count_vreg(&mut self, rhs: VReg, mask: u64) -> VReg {
        if mask >= 31 {
            return rhs;
        }
        let mask_vreg = self.mir.alloc_vreg();
        self.mir.push(X86Inst::MovRI32 {
            dst: Operand::Virtual(mask_vreg),
            imm: mask as i32,
        });
        let dst = self.mir.alloc_vreg();
        self.mir.push(X86Inst::MovRR {
            dst: Operand::Virtual(dst),
            src: Operand::Virtual(rhs),
        });
        self.mir.push(X86Inst::AndRR {
            dst: Operand::Virtual(dst),
            src: Operand::Virtual(mask_vreg),
        });
        dst
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
            X86Inst::Cmp64RR {
                src1: Operand::Virtual(lhs),
                src2: Operand::Virtual(rhs),
            }
        } else {
            X86Inst::CmpRR {
                src1: Operand::Virtual(lhs),
                src2: Operand::Virtual(rhs),
            }
        });
        self.mir.push(match (op, width.signed) {
            (crate::value_plan::ComparisonOp::Eq, _) => X86Inst::Sete {
                dst: Operand::Virtual(dst),
            },
            (crate::value_plan::ComparisonOp::Ne, _) => X86Inst::Setne {
                dst: Operand::Virtual(dst),
            },
            (crate::value_plan::ComparisonOp::Lt, true) => X86Inst::Setl {
                dst: Operand::Virtual(dst),
            },
            (crate::value_plan::ComparisonOp::Lt, false) => X86Inst::Setb {
                dst: Operand::Virtual(dst),
            },
            (crate::value_plan::ComparisonOp::Gt, true) => X86Inst::Setg {
                dst: Operand::Virtual(dst),
            },
            (crate::value_plan::ComparisonOp::Gt, false) => X86Inst::Seta {
                dst: Operand::Virtual(dst),
            },
            (crate::value_plan::ComparisonOp::Le, true) => X86Inst::Setle {
                dst: Operand::Virtual(dst),
            },
            (crate::value_plan::ComparisonOp::Le, false) => X86Inst::Setbe {
                dst: Operand::Virtual(dst),
            },
            (crate::value_plan::ComparisonOp::Ge, true) => X86Inst::Setge {
                dst: Operand::Virtual(dst),
            },
            (crate::value_plan::ComparisonOp::Ge, false) => X86Inst::Setae {
                dst: Operand::Virtual(dst),
            },
        });
        // `setcc` writes only the destination byte.  The shared boolean
        // value is subsequently consumed as a full-width integer by branch
        // and arithmetic lowering, so clear the upper bits before exposing
        // the result to the rest of the MIR.  This is an x86-64 fact; AArch64
        // `cset` already defines the complete 64-bit register.
        self.mir.push(X86Inst::Movzx {
            dst: Operand::Virtual(dst),
            src: Operand::Virtual(dst),
        });
        dst
    }

    fn lower_comparison(
        &mut self,
        op: crate::value_plan::ComparisonOp,
        lhs: crate::value_plan::MaterializedValue,
        rhs: crate::value_plan::MaterializedValue,
        policy: crate::value_plan::ValuePlan,
        equality_leaves: &[crate::aggregate_eq::EqualityLeaf],
        runtime_call: Option<crate::runtime_call_plan::RuntimeCallPlan>,
    ) -> VReg {
        match policy.comparison.expect("comparison plan") {
            crate::value_plan::ComparisonPreparation::Scalar { .. } => {
                self.lower_scalar_comparison(op, lhs.primary, rhs.primary, policy)
            }
            crate::value_plan::ComparisonPreparation::Float { width } => {
                self.lower_float_comparison(op, lhs.primary, rhs.primary, width)
            }
            crate::value_plan::ComparisonPreparation::Unit => {
                let dst = self.mir.alloc_vreg();
                self.mir.push(X86Inst::MovRI32 {
                    dst: Operand::Virtual(dst),
                    imm: matches!(op, crate::value_plan::ComparisonOp::Eq) as i32,
                });
                dst
            }
            crate::value_plan::ComparisonPreparation::StringContent { .. } => {
                let dst = self
                    .lower_runtime_call(runtime_call.expect("string equality runtime call plan"))
                    .primary;
                if matches!(op, crate::value_plan::ComparisonOp::Ne) {
                    self.mir.push(X86Inst::XorRI {
                        dst: Operand::Virtual(dst),
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
                    equality_leaves,
                    matches!(op, crate::value_plan::ComparisonOp::Ne),
                )
            }
        }
    }

    /// `u64 -> f32/f64`, correctly rounded. SSE2 converts only signed
    /// integers, so a value with the top bit set is halved with its dropped
    /// bit folded into the new low bit (a sticky bit that keeps the single
    /// rounding correct), converted, and doubled exactly.
    fn lower_u64_to_float(&mut self, dst: VReg, src: VReg, width: FloatWidth) {
        let non_negative = self.new_label();
        let done = self.new_label();
        self.mir.push(X86Inst::Cmp64RI {
            src: Operand::Virtual(src),
            imm: 0,
        });
        self.mir.push(X86Inst::Jge {
            label: non_negative,
        });
        let half = self.mir.alloc_vreg();
        self.mir.push(X86Inst::MovRR {
            dst: Operand::Virtual(half),
            src: Operand::Virtual(src),
        });
        self.mir.push(X86Inst::ShrRI {
            dst: Operand::Virtual(half),
            imm: 1,
        });
        let sticky = self.mir.alloc_vreg();
        self.mir.push(X86Inst::MovRR {
            dst: Operand::Virtual(sticky),
            src: Operand::Virtual(src),
        });
        let one = self.mir.alloc_vreg();
        self.mir.push(X86Inst::MovRI32 {
            dst: Operand::Virtual(one),
            imm: 1,
        });
        self.mir.push(X86Inst::And64RR {
            dst: Operand::Virtual(sticky),
            src: Operand::Virtual(one),
        });
        self.mir.push(X86Inst::Or64RR {
            dst: Operand::Virtual(half),
            src: Operand::Virtual(sticky),
        });
        self.mir.push(X86Inst::IntToFloat {
            dst: Operand::Virtual(dst),
            src: Operand::Virtual(half),
            int_bits: 64,
            width,
        });
        self.mir.push(X86Inst::FloatBin {
            op: super::mir::FloatBinOp::Add,
            dst: Operand::Virtual(dst),
            src: Operand::Virtual(dst),
            width,
        });
        self.mir.push(X86Inst::Jmp { label: done });
        self.mir.push(X86Inst::Label { id: non_negative });
        self.mir.push(X86Inst::IntToFloat {
            dst: Operand::Virtual(dst),
            src: Operand::Virtual(src),
            int_bits: 64,
            width,
        });
        self.mir.push(X86Inst::Label { id: done });
    }

    /// `f32/f64 -> u64` for an operand the guards have already proved to lie
    /// in `(-1, 2^64)`. `cvtts*2si` handles `x < 2^63` directly; a larger
    /// value is converted with `2^63` subtracted (exact, since both operands
    /// are at least `2^63`) and the bit put back on the integer side.
    fn lower_float_to_u64(&mut self, dst: VReg, src: VReg, width: FloatWidth) {
        let small = self.new_label();
        let done = self.new_label();
        let two63 = self.mir.alloc_vreg_in(crate::reg_class::RegClass::Fp);
        self.mir.push(X86Inst::FloatConst {
            dst: Operand::Virtual(two63),
            bits: crate::value_plan::float_const_bits(width, 9_223_372_036_854_775_808.0),
            width,
        });
        self.mir.push(X86Inst::FloatCmp {
            lhs: Operand::Virtual(src),
            rhs: Operand::Virtual(two63),
            width,
        });
        self.mir.push(X86Inst::Jb { label: small });
        let shifted = self.mir.alloc_vreg_in(crate::reg_class::RegClass::Fp);
        self.mir.push(X86Inst::FloatMov {
            dst: Operand::Virtual(shifted),
            src: Operand::Virtual(src),
            width,
        });
        self.mir.push(X86Inst::FloatBin {
            op: super::mir::FloatBinOp::Sub,
            dst: Operand::Virtual(shifted),
            src: Operand::Virtual(two63),
            width,
        });
        self.mir.push(X86Inst::FloatToInt {
            dst: Operand::Virtual(dst),
            src: Operand::Virtual(shifted),
            int_bits: 64,
            width,
        });
        let sign = self.mir.alloc_vreg();
        self.mir.push(X86Inst::MovRI64 {
            dst: Operand::Virtual(sign),
            imm: i64::MIN,
        });
        self.mir.push(X86Inst::Xor64RR {
            dst: Operand::Virtual(dst),
            src: Operand::Virtual(sign),
        });
        self.mir.push(X86Inst::Jmp { label: done });
        self.mir.push(X86Inst::Label { id: small });
        self.mir.push(X86Inst::FloatToInt {
            dst: Operand::Virtual(dst),
            src: Operand::Virtual(src),
            int_bits: 64,
            width,
        });
        self.mir.push(X86Inst::Label { id: done });
    }

    /// The IEEE `totalOrder` key of a float as a signed integer: the bit
    /// pattern with every bit but the sign flipped for negative values, so
    /// that a signed integer compare orders `-NaN < -inf < ... < -0 < +0 < ...
    /// < +inf < +NaN` (ADR-0065 §8).
    fn lower_float_total_order_key(&mut self, value: VReg, width: FloatWidth) -> VReg {
        let bits = self.mir.alloc_vreg();
        self.mir.push(X86Inst::FloatToBits {
            dst: Operand::Virtual(bits),
            src: Operand::Virtual(value),
            width,
        });
        let key = self.mir.alloc_vreg();
        self.mir.push(X86Inst::MovRR {
            dst: Operand::Virtual(key),
            src: Operand::Virtual(bits),
        });
        if width == FloatWidth::F64 {
            self.mir.push(X86Inst::SarRI {
                dst: Operand::Virtual(key),
                imm: 63,
            });
            self.mir.push(X86Inst::ShrRI {
                dst: Operand::Virtual(key),
                imm: 1,
            });
            self.mir.push(X86Inst::Xor64RR {
                dst: Operand::Virtual(key),
                src: Operand::Virtual(bits),
            });
        } else {
            self.mir.push(X86Inst::Sar32RI {
                dst: Operand::Virtual(key),
                imm: 31,
            });
            self.mir.push(X86Inst::Shr32RI {
                dst: Operand::Virtual(key),
                imm: 1,
            });
            self.mir.push(X86Inst::XorRR {
                dst: Operand::Virtual(key),
                src: Operand::Virtual(bits),
            });
        }
        key
    }

    /// Integral rounding. Floor, ceil, and trunc are one `rounds*`. SSE has
    /// no ties-away-from-zero mode, so `@round` truncates and then moves the
    /// result one unit away from zero when the discarded fraction has
    /// magnitude at least one half; NaN and infinities fall through every
    /// comparison and return the truncated (identical) value.
    fn lower_float_rounding(
        &mut self,
        src: VReg,
        width: FloatWidth,
        mode: crate::value_plan::FloatRounding,
    ) -> VReg {
        let dst = self.mir.alloc_vreg_in(crate::reg_class::RegClass::Fp);
        if mode != crate::value_plan::FloatRounding::NearestTiesAway {
            self.mir.push(X86Inst::FloatRound {
                dst: Operand::Virtual(dst),
                src: Operand::Virtual(src),
                width,
                mode,
            });
            return dst;
        }
        self.mir.push(X86Inst::FloatRound {
            dst: Operand::Virtual(dst),
            src: Operand::Virtual(src),
            width,
            mode: crate::value_plan::FloatRounding::Trunc,
        });
        let fraction = self.mir.alloc_vreg_in(crate::reg_class::RegClass::Fp);
        self.mir.push(X86Inst::FloatMov {
            dst: Operand::Virtual(fraction),
            src: Operand::Virtual(src),
            width,
        });
        self.mir.push(X86Inst::FloatBin {
            op: super::mir::FloatBinOp::Sub,
            dst: Operand::Virtual(fraction),
            src: Operand::Virtual(dst),
            width,
        });
        let one = self.mir.alloc_vreg_in(crate::reg_class::RegClass::Fp);
        self.mir.push(X86Inst::FloatConst {
            dst: Operand::Virtual(one),
            bits: crate::value_plan::float_const_bits(width, 1.0),
            width,
        });
        let half = self.mir.alloc_vreg_in(crate::reg_class::RegClass::Fp);
        self.mir.push(X86Inst::FloatConst {
            dst: Operand::Virtual(half),
            bits: crate::value_plan::float_const_bits(width, 0.5),
            width,
        });
        let not_up = self.new_label();
        let done = self.new_label();
        // fraction >= 0.5 (ordered): round up.
        self.mir.push(X86Inst::FloatCmp {
            lhs: Operand::Virtual(fraction),
            rhs: Operand::Virtual(half),
            width,
        });
        self.mir.push(X86Inst::Jb { label: not_up });
        self.mir.push(X86Inst::FloatBin {
            op: super::mir::FloatBinOp::Add,
            dst: Operand::Virtual(dst),
            src: Operand::Virtual(one),
            width,
        });
        self.mir.push(X86Inst::Jmp { label: done });
        self.mir.push(X86Inst::Label { id: not_up });
        // fraction <= -0.5 (ordered): round down. Spelled as `-0.5 < fraction`
        // falling through to done so that an unordered compare also skips.
        let neg_half = self.mir.alloc_vreg_in(crate::reg_class::RegClass::Fp);
        self.mir.push(X86Inst::FloatConst {
            dst: Operand::Virtual(neg_half),
            bits: crate::value_plan::float_const_bits(width, -0.5),
            width,
        });
        self.mir.push(X86Inst::FloatCmp {
            lhs: Operand::Virtual(neg_half),
            rhs: Operand::Virtual(fraction),
            width,
        });
        self.mir.push(X86Inst::Jb { label: done });
        self.mir.push(X86Inst::FloatBin {
            op: super::mir::FloatBinOp::Sub,
            dst: Operand::Virtual(dst),
            src: Operand::Virtual(one),
            width,
        });
        self.mir.push(X86Inst::Label { id: done });
        dst
    }

    fn lower_float_comparison(
        &mut self,
        op: crate::value_plan::ComparisonOp,
        lhs: VReg,
        rhs: VReg,
        width: crate::value_plan::FloatWidth,
    ) -> VReg {
        self.mir.push(X86Inst::FloatCmp {
            lhs: Operand::Virtual(lhs),
            rhs: Operand::Virtual(rhs),
            width,
        });
        let dst = self.mir.alloc_vreg();
        self.mir.push(match op {
            crate::value_plan::ComparisonOp::Eq => X86Inst::Sete {
                dst: Operand::Virtual(dst),
            },
            crate::value_plan::ComparisonOp::Ne => X86Inst::Setne {
                dst: Operand::Virtual(dst),
            },
            crate::value_plan::ComparisonOp::Lt => X86Inst::Setb {
                dst: Operand::Virtual(dst),
            },
            crate::value_plan::ComparisonOp::Gt => X86Inst::Seta {
                dst: Operand::Virtual(dst),
            },
            crate::value_plan::ComparisonOp::Le => X86Inst::Setbe {
                dst: Operand::Virtual(dst),
            },
            crate::value_plan::ComparisonOp::Ge => X86Inst::Setae {
                dst: Operand::Virtual(dst),
            },
        });
        if matches!(
            op,
            crate::value_plan::ComparisonOp::Eq
                | crate::value_plan::ComparisonOp::Lt
                | crate::value_plan::ComparisonOp::Le
        ) {
            let ordered = self.mir.alloc_vreg();
            self.mir.push(X86Inst::Setnp {
                dst: Operand::Virtual(ordered),
            });
            self.mir.push(X86Inst::AndRR {
                dst: Operand::Virtual(dst),
                src: Operand::Virtual(ordered),
            });
        } else if matches!(op, crate::value_plan::ComparisonOp::Ne) {
            let unordered = self.mir.alloc_vreg();
            self.mir.push(X86Inst::Setp {
                dst: Operand::Virtual(unordered),
            });
            self.mir.push(X86Inst::OrRR {
                dst: Operand::Virtual(dst),
                src: Operand::Virtual(unordered),
            });
        }
        self.mir.push(X86Inst::Movzx {
            dst: Operand::Virtual(dst),
            src: Operand::Virtual(dst),
        });
        dst
    }

    fn lower_trap(
        &mut self,
        plan: crate::value_plan::TrapPlan,
    ) -> crate::value_plan::MaterializedValue {
        match plan {
            crate::value_plan::TrapPlan::Panic { call } => self.lower_runtime_call(call),
            crate::value_plan::TrapPlan::Assert { condition, call } => {
                let pass = self.new_label();
                self.mir.push(X86Inst::CmpRI {
                    src: Operand::Virtual(condition),
                    imm: 0,
                });
                self.mir.push(X86Inst::Jnz { label: pass });
                let result = self.lower_runtime_call(call);
                self.mir.push(X86Inst::Label { id: pass });
                result
            }
        }
    }

    fn lower_intrinsic_plan(
        &mut self,
        plan: crate::value_plan::IntrinsicPlan,
    ) -> crate::value_plan::MaterializedValue {
        let operation = plan.operation;
        let mut slots = Vec::new();
        let primary = match operation {
            rue_air::IntrinsicOperation::ReadLine
            | rue_air::IntrinsicOperation::ParseI32
            | rue_air::IntrinsicOperation::ParseI64
            | rue_air::IntrinsicOperation::ParseU32
            | rue_air::IntrinsicOperation::ParseU64 => {
                let result = self.lower_runtime_call(
                    plan.runtime_call
                        .clone()
                        .expect("option intrinsic runtime call plan"),
                );
                slots = result.slots;
                result.primary
            }
            rue_air::IntrinsicOperation::RandomU32 | rue_air::IntrinsicOperation::RandomU64 => {
                self.lower_runtime_call(
                    plan.runtime_call
                        .expect("random intrinsic runtime call plan"),
                )
                .primary
            }
            rue_air::IntrinsicOperation::ArgCount
            | rue_air::IntrinsicOperation::ArgPtr
            | rue_air::IntrinsicOperation::ArgLen
            | rue_air::IntrinsicOperation::EnvCount
            | rue_air::IntrinsicOperation::EnvPtr
            | rue_air::IntrinsicOperation::EnvLen => {
                self.lower_runtime_call(
                    plan.runtime_call
                        .expect("process arg/env intrinsic runtime call plan"),
                )
                .primary
            }
            rue_air::IntrinsicOperation::PtrToInt | rue_air::IntrinsicOperation::IntToPtr => {
                let dst = self.mir.alloc_vreg();
                self.mir.push(X86Inst::MovRR {
                    dst: Operand::Virtual(dst),
                    src: Operand::Virtual(plan.args[0].primary),
                });
                dst
            }
            // `@bitCast` (RUE-952) moves no bits: it copies the operand and
            // rebuilds only the bits above the shared width, which belong to the
            // source type's register image. The shared planner picked the form
            // from the target type; this leaf just spells it.
            rue_air::IntrinsicOperation::BitCast => {
                use crate::value_plan::BitCastForm;
                let form = plan
                    .bit_cast_form
                    .expect("bitCast intrinsic must carry its contextual form");
                // A float operand or result crosses the register-file boundary
                // (ADR-0065 §6): `FloatToBits` lands the pattern zero-extended
                // in a general-purpose register, and `BitsToFloat` is the
                // whole cast the other way.
                if let Some(width) = crate::value_plan::float_width(plan.result_ty) {
                    let dst = self.mir.alloc_vreg_in(crate::reg_class::RegClass::Fp);
                    self.mir.push(X86Inst::BitsToFloat {
                        dst: Operand::Virtual(dst),
                        src: Operand::Virtual(plan.args[0].primary),
                        width,
                    });
                    return crate::value_plan::MaterializedValue {
                        primary: dst,
                        slots: Vec::new(),
                    };
                }
                let dst = self.mir.alloc_vreg();
                if let Some(width) = crate::value_plan::float_width(plan.args[0].ty) {
                    self.mir.push(X86Inst::FloatToBits {
                        dst: Operand::Virtual(dst),
                        src: Operand::Virtual(plan.args[0].primary),
                        width,
                    });
                } else {
                    self.mir.push(X86Inst::MovRR {
                        dst: Operand::Virtual(dst),
                        src: Operand::Virtual(plan.args[0].primary),
                    });
                }
                let operand = Operand::Virtual(dst);
                match form {
                    BitCastForm::Move => {}
                    BitCastForm::Sign8 => self.mir.push(X86Inst::Movsx8To64 {
                        dst: operand,
                        src: operand,
                    }),
                    BitCastForm::Zero8 => self.mir.push(X86Inst::Movzx8To64 {
                        dst: operand,
                        src: operand,
                    }),
                    BitCastForm::Sign16 => self.mir.push(X86Inst::Movsx16To64 {
                        dst: operand,
                        src: operand,
                    }),
                    BitCastForm::Zero16 => self.mir.push(X86Inst::Movzx16To64 {
                        dst: operand,
                        src: operand,
                    }),
                    BitCastForm::Zero32 => {
                        self.mir.push(X86Inst::Movzx32To64 {
                            dst: operand,
                            src: operand,
                        });
                    }
                }
                dst
            }
            rue_air::IntrinsicOperation::PtrRead
            | rue_air::IntrinsicOperation::PtrReadUnaligned => {
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
                    // The pointee's slots are read at their leaf widths and
                    // classes, matching what every aggregate writer through a
                    // pointer stores.
                    let leaf_types =
                        crate::types::aggregate_leaf_types(self.ctx.type_pool, plan.result_ty);
                    slots = crate::agg_slots::load_slots_through_ptr_typed(self, ptr, &leaf_types);
                    slots[0]
                } else {
                    let float_width = crate::value_plan::float_width(plan.result_ty);
                    let dst = self.mir.alloc_vreg_in(if float_width.is_some() {
                        crate::reg_class::RegClass::Fp
                    } else {
                        crate::reg_class::RegClass::Gp
                    });
                    if count != 0 {
                        // A narrow scalar pointee reads 1/2/4 physical bytes and
                        // extends into the slot-shaped vreg (RUE-989); a full-slot
                        // pointee keeps the eight-byte load.
                        if let Some(width) = float_width {
                            self.mir.push(X86Inst::MovRR {
                                dst: Operand::Physical(Reg::Rax),
                                src: Operand::Virtual(ptr),
                            });
                            self.mir.push(X86Inst::FloatLoad {
                                dst: Operand::Virtual(dst),
                                base: Reg::Rax,
                                offset: 0,
                                width,
                            });
                        } else if let Some(narrow) = plan.narrow_access {
                            self.mir.push(X86Inst::NarrowLoadIndexed {
                                dst: Operand::Virtual(dst),
                                base: ptr,
                                offset: 0,
                                width: narrow.width,
                                signed: narrow.signed,
                            });
                        } else {
                            self.mir.push(X86Inst::MovRMIndexed {
                                dst: Operand::Virtual(dst),
                                base: ptr,
                                offset: 0,
                            });
                        }
                    }
                    if count != 0 {
                        slots.push(dst);
                    }
                    dst
                }
            }
            rue_air::IntrinsicOperation::PtrWrite
            | rue_air::IntrinsicOperation::PtrWriteUnaligned => {
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
                    let leaf_types =
                        crate::types::aggregate_leaf_types(self.ctx.type_pool, plan.args[1].ty);
                    crate::agg_slots::store_slots_through_ptr_typed(
                        self,
                        &value.slots,
                        ptr,
                        0,
                        &leaf_types,
                    );
                } else if let Some(width) = crate::value_plan::float_width(plan.args[1].ty) {
                    self.mir.push(X86Inst::MovRR {
                        dst: Operand::Physical(Reg::Rax),
                        src: Operand::Virtual(ptr),
                    });
                    self.mir.push(X86Inst::FloatStore {
                        base: Reg::Rax,
                        offset: 0,
                        src: Operand::Virtual(value.primary),
                        width,
                    });
                } else if let Some(narrow) = plan.narrow_access {
                    // A narrow scalar truncates to 1/2/4 physical bytes (RUE-989).
                    self.mir.push(X86Inst::NarrowStoreIndexed {
                        base: ptr,
                        src: Operand::Virtual(value.primary),
                        offset: 0,
                        width: narrow.width,
                    });
                } else {
                    self.mir.push(X86Inst::MovMRIndexed {
                        base: ptr,
                        offset: 0,
                        src: Operand::Virtual(value.primary),
                    });
                }
                let dst = self.mir.alloc_vreg();
                self.mir.push(X86Inst::MovRI32 {
                    dst: Operand::Virtual(dst),
                    imm: 0,
                });
                dst
            }
            rue_air::IntrinsicOperation::PtrOffset => {
                let offset = plan.args[1].primary;
                let offset = match plan.args[1].integer_extension {
                    crate::value_plan::IntegerExtension::None => offset,
                    extension => {
                        let extended = self.mir.alloc_vreg();
                        self.mir.push(X86Inst::MovRR {
                            dst: Operand::Virtual(extended),
                            src: Operand::Virtual(offset),
                        });
                        let dst = Operand::Virtual(extended);
                        let src = Operand::Virtual(extended);
                        match extension {
                            crate::value_plan::IntegerExtension::Sign8 => {
                                self.mir.push(X86Inst::Movsx8To64 { dst, src })
                            }
                            crate::value_plan::IntegerExtension::Zero8 => {
                                self.mir.push(X86Inst::Movzx8To64 { dst, src })
                            }
                            crate::value_plan::IntegerExtension::Sign16 => {
                                self.mir.push(X86Inst::Movsx16To64 { dst, src })
                            }
                            crate::value_plan::IntegerExtension::Zero16 => {
                                self.mir.push(X86Inst::Movzx16To64 { dst, src })
                            }
                            crate::value_plan::IntegerExtension::Sign32 => {
                                self.mir.push(X86Inst::Movsx32To64 { dst, src })
                            }
                            crate::value_plan::IntegerExtension::None => unreachable!(),
                        }
                        extended
                    }
                };
                let scaled =
                    allocation::lower_scale(self, offset, plan.scale.expect("ptr_offset scale"));
                let dst = self.mir.alloc_vreg();
                self.mir.push(X86Inst::MovRR {
                    dst: Operand::Virtual(dst),
                    src: Operand::Virtual(plan.args[0].primary),
                });
                self.mir.push(X86Inst::AddRR64 {
                    dst: Operand::Virtual(dst),
                    src: Operand::Virtual(scaled),
                });
                dst
            }
            // The unified allocation family and the bulk byte primitives are
            // pure runtime calls: the shared plan already carries their
            // operands in helper order (ADR-0059 Phase 3, RUE-961).
            rue_air::IntrinsicOperation::Alloc => {
                self.lower_runtime_call(plan.runtime_call.expect("alloc runtime call plan"))
                    .primary
            }
            rue_air::IntrinsicOperation::AllocZeroed => {
                self.lower_runtime_call(plan.runtime_call.expect("alloc-zeroed runtime call plan"))
                    .primary
            }
            rue_air::IntrinsicOperation::Free => {
                self.lower_runtime_call(plan.runtime_call.expect("free runtime call plan"))
                    .primary
            }
            rue_air::IntrinsicOperation::Realloc => {
                self.lower_runtime_call(plan.runtime_call.expect("realloc runtime call plan"))
                    .primary
            }
            rue_air::IntrinsicOperation::Resize => {
                self.lower_runtime_call(plan.runtime_call.expect("resize runtime call plan"))
                    .primary
            }
            rue_air::IntrinsicOperation::ByteCopy => {
                self.lower_runtime_call(plan.runtime_call.expect("byte-copy runtime call plan"))
                    .primary
            }
            rue_air::IntrinsicOperation::ByteMove => {
                self.lower_runtime_call(plan.runtime_call.expect("byte-move runtime call plan"))
                    .primary
            }
            rue_air::IntrinsicOperation::ByteSet => {
                self.lower_runtime_call(plan.runtime_call.expect("byte-set runtime call plan"))
                    .primary
            }
            rue_air::IntrinsicOperation::Raw
            | rue_air::IntrinsicOperation::RawMut
            | rue_air::IntrinsicOperation::FieldPtr => {
                let place = plan.args[0]
                    .place
                    .as_ref()
                    .expect("raw intrinsic place plan");
                let dst = self.mir.alloc_vreg();
                crate::place_lower::lower_place_addr_plan(self, dst, place);
                dst
            }
            rue_air::IntrinsicOperation::DebugI64
            | rue_air::IntrinsicOperation::DebugU64
            | rue_air::IntrinsicOperation::DebugFloat
            | rue_air::IntrinsicOperation::DebugBool
            | rue_air::IntrinsicOperation::DebugStr => {
                self.lower_runtime_call(plan.runtime_call.expect("debug runtime call plan"))
                    .primary
            }
            rue_air::IntrinsicOperation::Syscall => {
                let syscall_regs = [Reg::Rdi, Reg::Rsi, Reg::Rdx, Reg::R10, Reg::R8, Reg::R9];
                for arg in plan.args.iter().rev() {
                    self.mir.push(X86Inst::MovRR {
                        dst: Operand::Physical(Reg::Rax),
                        src: Operand::Virtual(arg.primary),
                    });
                    self.mir.push(X86Inst::Push {
                        src: Operand::Physical(Reg::Rax),
                    });
                }
                self.mir.push(X86Inst::Pop {
                    dst: Operand::Physical(Reg::Rax),
                });
                for (index, reg) in syscall_regs.iter().enumerate() {
                    if index + 1 < plan.args.len() {
                        self.mir.push(X86Inst::Pop {
                            dst: Operand::Physical(*reg),
                        });
                    }
                }
                self.mir.push(X86Inst::Syscall);
                let dst = self.mir.alloc_vreg();
                self.mir.push(X86Inst::MovRR {
                    dst: Operand::Virtual(dst),
                    src: Operand::Physical(Reg::Rax),
                });
                dst
            }
            rue_air::IntrinsicOperation::PanicNoMessage
            | rue_air::IntrinsicOperation::Panic
            | rue_air::IntrinsicOperation::AssertFailed
            | rue_air::IntrinsicOperation::BoundsCheck => {
                unreachable!("trap handled above")
            }
            rue_air::IntrinsicOperation::IntToFloat => {
                let width = crate::value_plan::float_width(plan.result_ty).expect("float result");
                let int =
                    crate::value_plan::integer_width(plan.args[0].ty).expect("integer source");
                let src = plan.args[0].primary;
                let dst = self.mir.alloc_vreg_in(crate::reg_class::RegClass::Fp);
                if int.signed {
                    self.mir.push(X86Inst::IntToFloat {
                        dst: Operand::Virtual(dst),
                        src: Operand::Virtual(src),
                        int_bits: int.bits.max(32),
                        width,
                    });
                } else if int.bits < 64 {
                    // A narrow unsigned value is exact in the 64-bit signed
                    // form once its upper bits are cleared, and `cvtsi2s*`
                    // rounds that form once, correctly.
                    let wide = self.mir.alloc_vreg();
                    self.mir.push(match plan.args[0].integer_extension {
                        crate::value_plan::IntegerExtension::Zero8 => X86Inst::Movzx8To64 {
                            dst: Operand::Virtual(wide),
                            src: Operand::Virtual(src),
                        },
                        crate::value_plan::IntegerExtension::Zero16 => X86Inst::Movzx16To64 {
                            dst: Operand::Virtual(wide),
                            src: Operand::Virtual(src),
                        },
                        _ => X86Inst::Movzx32To64 {
                            dst: Operand::Virtual(wide),
                            src: Operand::Virtual(src),
                        },
                    });
                    self.mir.push(X86Inst::IntToFloat {
                        dst: Operand::Virtual(dst),
                        src: Operand::Virtual(wide),
                        int_bits: 64,
                        width,
                    });
                } else {
                    self.lower_u64_to_float(dst, src, width);
                }
                dst
            }
            rue_air::IntrinsicOperation::FloatToInt => {
                let width = crate::value_plan::float_width(plan.args[0].ty).expect("float source");
                let int = crate::value_plan::integer_width(plan.result_ty).expect("integer result");
                let check = crate::value_plan::checked_float_to_int_plan(width, int);
                for guard in check.guards {
                    let bound = self.mir.alloc_vreg_in(crate::reg_class::RegClass::Fp);
                    self.mir.push(X86Inst::FloatConst {
                        dst: Operand::Virtual(bound),
                        bits: guard.bound_bits,
                        width,
                    });
                    self.mir.push(X86Inst::FloatCmp {
                        lhs: Operand::Virtual(plan.args[0].primary),
                        rhs: Operand::Virtual(bound),
                        width,
                    });
                    let ok = self.new_label();
                    self.mir.push(match guard.success {
                        crate::value_plan::FloatToIntGuardRelation::OrderedGreaterEqual => {
                            X86Inst::Jae { label: ok }
                        }
                        crate::value_plan::FloatToIntGuardRelation::OrderedGreater => {
                            X86Inst::Ja { label: ok }
                        }
                        crate::value_plan::FloatToIntGuardRelation::OrderedLess => {
                            X86Inst::Jb { label: ok }
                        }
                    });
                    let _ = self.lower_runtime_call(check.trap_call.clone());
                    self.mir.push(X86Inst::Label { id: ok });
                }
                let dst = self.mir.alloc_vreg();
                if int.signed {
                    self.mir.push(X86Inst::FloatToInt {
                        dst: Operand::Virtual(dst),
                        src: Operand::Virtual(plan.args[0].primary),
                        int_bits: int.bits.max(32),
                        width,
                    });
                } else if int.bits < 64 {
                    // The guards proved `0 <= trunc(x) < 2^bits <= 2^32`, so the
                    // 64-bit signed conversion is exact and its low bits are
                    // the unsigned result.
                    self.mir.push(X86Inst::FloatToInt {
                        dst: Operand::Virtual(dst),
                        src: Operand::Virtual(plan.args[0].primary),
                        int_bits: 64,
                        width,
                    });
                } else {
                    self.lower_float_to_u64(dst, plan.args[0].primary, width);
                }
                dst
            }
            rue_air::IntrinsicOperation::FloatCast => {
                let from = crate::value_plan::float_width(plan.args[0].ty).expect("float source");
                let to = crate::value_plan::float_width(plan.result_ty).expect("float result");
                let dst = self.mir.alloc_vreg_in(crate::reg_class::RegClass::Fp);
                self.mir.push(X86Inst::FloatCast {
                    dst: Operand::Virtual(dst),
                    src: Operand::Virtual(plan.args[0].primary),
                    from,
                    to,
                });
                dst
            }
            rue_air::IntrinsicOperation::TotalCmp => {
                let width = crate::value_plan::float_width(plan.args[0].ty).expect("float operand");
                let lhs = self.lower_float_total_order_key(plan.args[0].primary, width);
                let rhs = self.lower_float_total_order_key(plan.args[1].primary, width);
                self.mir.push(if width == FloatWidth::F64 {
                    X86Inst::Cmp64RR {
                        src1: Operand::Virtual(lhs),
                        src2: Operand::Virtual(rhs),
                    }
                } else {
                    X86Inst::CmpRR {
                        src1: Operand::Virtual(lhs),
                        src2: Operand::Virtual(rhs),
                    }
                });
                let greater = self.mir.alloc_vreg();
                let less = self.mir.alloc_vreg();
                self.mir.push(X86Inst::Setg {
                    dst: Operand::Virtual(greater),
                });
                self.mir.push(X86Inst::Setl {
                    dst: Operand::Virtual(less),
                });
                self.mir.push(X86Inst::Movzx {
                    dst: Operand::Virtual(greater),
                    src: Operand::Virtual(greater),
                });
                self.mir.push(X86Inst::Movzx {
                    dst: Operand::Virtual(less),
                    src: Operand::Virtual(less),
                });
                let dst = self.mir.alloc_vreg();
                self.mir.push(X86Inst::MovRR {
                    dst: Operand::Virtual(dst),
                    src: Operand::Virtual(greater),
                });
                self.mir.push(X86Inst::SubRR {
                    dst: Operand::Virtual(dst),
                    src: Operand::Virtual(less),
                });
                dst
            }
            rue_air::IntrinsicOperation::FloatSqrt => {
                let width = crate::value_plan::float_width(plan.result_ty).expect("float result");
                let dst = self.mir.alloc_vreg_in(crate::reg_class::RegClass::Fp);
                self.mir.push(X86Inst::FloatSqrt {
                    dst: Operand::Virtual(dst),
                    src: Operand::Virtual(plan.args[0].primary),
                    width,
                });
                dst
            }
            rue_air::IntrinsicOperation::FloatFloor
            | rue_air::IntrinsicOperation::FloatCeil
            | rue_air::IntrinsicOperation::FloatTrunc
            | rue_air::IntrinsicOperation::FloatRound => {
                let width = crate::value_plan::float_width(plan.result_ty).expect("float result");
                let mode = crate::value_plan::float_rounding(plan.operation).expect("rounding");
                self.lower_float_rounding(plan.args[0].primary, width, mode)
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
    fn emit_overflow_check(
        &mut self,
        width: crate::value_plan::IntegerWidth,
        result_vreg: VReg,
        trap_call: crate::runtime_call_plan::RuntimeCallPlan,
    ) {
        let ok_label = self.new_label();

        match (width.bits, width.signed) {
            // 32-bit and 64-bit unsigned: check carry flag
            (32 | 64, false) => {
                self.mir.push(X86Inst::Jae { label: ok_label });
            }
            // 32-bit and 64-bit signed: check overflow flag
            (32 | 64, true) => {
                self.mir.push(X86Inst::Jno { label: ok_label });
            }
            // Sub-word unsigned types: check if result fits in range [0, max]
            (8, false) => {
                // Result must be <= 255
                self.mir.push(X86Inst::CmpRI {
                    src: Operand::Virtual(result_vreg),
                    imm: 255,
                });
                // Jump if below or equal (unsigned)
                self.mir.push(X86Inst::Jbe { label: ok_label });
            }
            (16, false) => {
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
            (8, true) => {
                // For i8: result must be in [-128, 127]
                // Sign-extend to 64-bit and compare with original
                // If they differ, overflow occurred
                let sext_vreg = self.mir.alloc_vreg();
                self.mir.push(X86Inst::Movsx8To64 {
                    dst: Operand::Virtual(sext_vreg),
                    src: Operand::Virtual(result_vreg),
                });
                // Compare at 32-bit width. The sub-word arithmetic was emitted
                // as a 32-bit op that zero-extends bits 32-63, so result_vreg's
                // valid data is only in its low 32 bits. A 64-bit compare against
                // the sign-extended byte/word (1s in bits 32-63 for a negative
                // value) would mismatch a legitimately-negative in-range result
                // and falsely trap (RUE-28 sub, RUE-60 neg). The low 32 bits —
                // sign-extended byte/word vs the 32-bit result — must match.
                self.mir.push(X86Inst::CmpRR {
                    src1: Operand::Virtual(result_vreg),
                    src2: Operand::Virtual(sext_vreg),
                });
                self.mir.push(X86Inst::Jz { label: ok_label });
            }
            (16, true) => {
                // For i16: result must be in [-32768, 32767]
                // Sign-extend to 64-bit and compare with original
                let sext_vreg = self.mir.alloc_vreg();
                self.mir.push(X86Inst::Movsx16To64 {
                    dst: Operand::Virtual(sext_vreg),
                    src: Operand::Virtual(result_vreg),
                });
                // Compare at 32-bit width. The sub-word arithmetic was emitted
                // as a 32-bit op that zero-extends bits 32-63, so result_vreg's
                // valid data is only in its low 32 bits. A 64-bit compare against
                // the sign-extended byte/word (1s in bits 32-63 for a negative
                // value) would mismatch a legitimately-negative in-range result
                // and falsely trap (RUE-28 sub, RUE-60 neg). The low 32 bits —
                // sign-extended byte/word vs the 32-bit result — must match.
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
        let _ = self.lower_runtime_call(trap_call.clone());
        self.mir.push(X86Inst::Label { id: ok_label });
    }

    /// Emit an aborting call to `__rue_panic(ptr, len)` for `@panic("msg")` and
    /// `@assert(cond, "msg")` (RUE-319). `msg_val` is the CFG value of the
    /// message `String`; its fat pointer supplies the `ptr`/`len` arguments.
    /// Never returns at runtime.
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

        let ok_label = self.new_label();

        // Calculate the min and max values for the target type
        let (min_val, max_val) = crate::value_plan::integer_range(to_width);

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
                    let _ = self.lower_runtime_call(trap_call.clone());
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
                    let _ = self.lower_runtime_call(trap_call.clone());
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
                let _ = self.lower_runtime_call(trap_call.clone());
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
                    let _ = self.lower_runtime_call(trap_call.clone());
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
                let _ = self.lower_runtime_call(trap_call.clone());
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
                    let _ = self.lower_runtime_call(trap_call.clone());
                    self.mir.push(X86Inst::Label { id: ok_label });
                }
            }
        }
    }

    /// Emit a comparison instruction.
    fn emit_terminator_plan(&mut self, plan: crate::terminator_plan::TerminatorPlan) {
        use crate::terminator_plan::{ReturnMode, ReturnValuePlan, TerminatorPlan};

        match plan {
            TerminatorPlan::Goto { edge } => {
                self.emit_edge_moves(&edge);
                if !edge.fallthrough {
                    self.mir.push(X86Inst::Jmp {
                        label: self.block_label(edge.target),
                    });
                }
            }
            TerminatorPlan::Branch {
                condition,
                then_edge,
                else_edge,
            } => {
                self.mir.push(X86Inst::CmpRI {
                    src: Operand::Virtual(condition),
                    imm: 0,
                });
                if then_edge.fallthrough {
                    let then_setup_label = self.new_label();
                    self.mir.push(X86Inst::Jnz {
                        label: then_setup_label,
                    });
                    self.emit_edge_moves(&else_edge);
                    if !else_edge.fallthrough {
                        self.mir.push(X86Inst::Jmp {
                            label: self.block_label(else_edge.target),
                        });
                    }
                    self.mir.push(X86Inst::Label {
                        id: then_setup_label,
                    });
                    self.emit_edge_moves(&then_edge);
                } else {
                    let else_setup_label = self.new_label();
                    self.mir.push(X86Inst::Jz {
                        label: else_setup_label,
                    });
                    self.emit_edge_moves(&then_edge);
                    if !then_edge.fallthrough {
                        self.mir.push(X86Inst::Jmp {
                            label: self.block_label(then_edge.target),
                        });
                    }
                    self.mir.push(X86Inst::Label {
                        id: else_setup_label,
                    });
                    self.emit_edge_moves(&else_edge);
                    if !else_edge.fallthrough {
                        self.mir.push(X86Inst::Jmp {
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
                    self.mir.push(X86Inst::MovRI64 {
                        dst: Operand::Virtual(case_vreg),
                        imm: case.value,
                    });
                    if width.bits == 64 {
                        self.mir.push(X86Inst::Cmp64RR {
                            src1: Operand::Virtual(scrutinee),
                            src2: Operand::Virtual(case_vreg),
                        });
                    } else {
                        self.mir.push(X86Inst::CmpRR {
                            src1: Operand::Virtual(scrutinee),
                            src2: Operand::Virtual(case_vreg),
                        });
                    }
                    self.mir.push(X86Inst::Jz {
                        label: self.block_label(case.target),
                    });
                }
                self.mir.push(X86Inst::Jmp {
                    label: self.block_label(default),
                });
            }
            TerminatorPlan::Return { mode } => match mode {
                ReturnMode::Exit { call } => {
                    let _ = self.lower_runtime_call(call);
                }
                ReturnMode::Function { value } => match value {
                    ReturnValuePlan::ZeroSized => self.mir.push(X86Inst::Ret),
                    ReturnValuePlan::Scalar { value, float_width } => {
                        self.mir.push(
                            if self.mir.vreg_class(value) == crate::reg_class::RegClass::Fp {
                                X86Inst::FloatMov {
                                    dst: Operand::Physical(Reg::Xmm0),
                                    src: Operand::Virtual(value),
                                    width: float_width.expect("FP return width"),
                                }
                            } else {
                                X86Inst::MovRR {
                                    dst: Operand::Physical(Reg::Rax),
                                    src: Operand::Virtual(value),
                                }
                            },
                        );
                        self.mir.push(X86Inst::Ret);
                    }
                    ReturnValuePlan::Aggregate {
                        slots,
                        return_plan,
                        slot_regs,
                    } => {
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
                            assert_eq!(
                                slot_regs.len(),
                                slots.len(),
                                "a register return must name a return register for every slot"
                            );
                            for (reg, slot) in slot_regs.iter().zip(&slots).rev() {
                                self.mir.push(match *reg {
                                    crate::call_plan::ReturnSlotReg::Gp(index) => X86Inst::MovRR {
                                        dst: Operand::Physical(RET_REGS[index]),
                                        src: Operand::Virtual(*slot),
                                    },
                                    crate::call_plan::ReturnSlotReg::Fp { index, width } => {
                                        X86Inst::FloatMov {
                                            dst: Operand::Physical(FP_RET_REGS[index]),
                                            src: Operand::Virtual(*slot),
                                            width,
                                        }
                                    }
                                });
                            }
                        }
                        self.mir.push(X86Inst::Ret);
                    }
                },
            },
            TerminatorPlan::Unreachable => self.mir.push(X86Inst::Ud2),
        }
    }

    fn emit_edge_moves(&mut self, edge: &crate::terminator_plan::EdgePlan) {
        for movement in &edge.moves {
            self.mir.push(if let Some(width) = movement.float_width {
                X86Inst::FloatMov {
                    dst: Operand::Virtual(movement.destination),
                    src: Operand::Virtual(movement.source),
                    width,
                }
            } else {
                X86Inst::MovRR {
                    dst: Operand::Virtual(movement.destination),
                    src: Operand::Virtual(movement.source),
                }
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
        self.mir.push(X86Inst::Label {
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
        let primary_ty = crate::types::aggregate_leaf_types(self.ctx.type_pool, ty)
            .first()
            .copied()
            .unwrap_or(ty);
        let vreg =
            self.mir
                .alloc_vreg_in(if crate::value_plan::float_width(primary_ty).is_some() {
                    crate::reg_class::RegClass::Fp
                } else {
                    crate::reg_class::RegClass::Gp
                });
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
            TerminatorPlan::Branch { .. } => {
                Some("Compare with zero, conditional jump".to_string())
            }
            TerminatorPlan::Return {
                mode: ReturnMode::Exit { .. },
            } => Some("Main function: return value becomes exit code".to_string()),
            TerminatorPlan::Return {
                mode: ReturnMode::Function { value },
            } if !matches!(value, crate::terminator_plan::ReturnValuePlan::ZeroSized) => {
                Some("Return value in RAX (SysV ABI)".to_string())
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
    fn reserve_typed_value_result(
        &mut self,
        float_width: Option<crate::value_plan::FloatWidth>,
    ) -> VReg {
        self.mir.alloc_vreg_in(if float_width.is_some() {
            crate::reg_class::RegClass::Fp
        } else {
            crate::reg_class::RegClass::Gp
        })
    }
    fn resolve_symbol(&self, symbol: lasso::Spur) -> String {
        self.symbols.resolve(self.interner.resolve(&symbol))
    }
    fn resolve_named_symbol(&self, symbol: &str) -> String {
        self.symbols.resolve(symbol)
    }
    fn foreign_symbol_convention(
        &self,
        machine_symbol: &str,
    ) -> Option<rue_target::CallingConvention> {
        self.symbols.foreign_convention(machine_symbol)
    }
    fn call_arg_register_banks(&self) -> crate::call_plan::AbiRegisterBanks {
        crate::call_plan::AbiRegisterBanks {
            gp: ARG_REGS.len(),
            fp: FP_ARG_REGS.len(),
        }
    }
    fn return_register_banks(&self) -> crate::call_plan::AbiRegisterBanks {
        return_register_banks()
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
        crate::value_plan::ValueResult::Materialized(crate::foreign_call::lower_foreign_call(
            self, inputs, result,
        ))
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

/// Return the immediate form usable for a full-width enum-tag comparison.
/// x86-64 sign-extends the compare immediate, so only non-negative i32 values
/// preserve the zero-extended u32 tag representation.
fn marshal_tag_cmp_imm(discriminant: u64) -> Option<i32> {
    i32::try_from(discriminant).ok()
}

fn compact_tag_discriminant(discriminant: u64) -> u32 {
    u32::try_from(discriminant)
        .expect("enum discriminant exceeds the compact u32 tag representation")
}

/// Emit the compare and skip branch used by every heterogeneous enum image
/// dispatch. Keeping this sequence in one helper lets the MIR boundary tests
/// exercise the same path as [`SlotBackend::emit_marshal_branch_if_tag_ne`].
fn emit_marshal_tag_ne(mir: &mut X86Mir, tag: VReg, discriminant: u32, label: LabelId) {
    // Compact enum tags are zero-extended u8/u16/u32 values in their
    // slot-shaped vregs. Keep the compare 64-bit so a tag above i32::MAX
    // cannot be changed by an unchecked u64-to-i32 cast. The immediate
    // form is sign-extended by x86, so it is valid only for values that fit
    // i32; larger (still representable u32) tags use the existing full-
    // width constant materialization and register compare.
    if let Some(imm) = marshal_tag_cmp_imm(u64::from(discriminant)) {
        mir.push(X86Inst::Cmp64RI {
            src: Operand::Virtual(tag),
            imm,
        });
    } else {
        let constant = mir.alloc_vreg();
        mir.push(X86Inst::MovRI64 {
            dst: Operand::Virtual(constant),
            imm: i64::from(discriminant),
        });
        mir.push(X86Inst::Cmp64RR {
            src1: Operand::Virtual(tag),
            src2: Operand::Virtual(constant),
        });
    }
    mir.push(X86Inst::Jnz { label });
}

impl crate::foreign_call::ForeignCallLoweringBackend for CfgLower<'_> {
    fn target_c_convention(&self) -> rue_target::CallingConvention {
        x86_64_target_c_convention()
    }

    fn foreign_int_arg_register_count(&self) -> usize {
        ARG_REGS.len()
    }

    fn foreign_reserve_sret(&mut self, image: &crate::foreign_call::AggregateImage) -> VReg {
        self.mir.push(X86Inst::AddRI {
            dst: Operand::Physical(Reg::Rsp),
            imm: -checked_displacement_bytes(u64::from(image.storage_bytes))
                .expect("foreign sret storage must fit displacement"),
        });
        let ptr = self.mir.alloc_vreg();
        self.mir.push(X86Inst::MovRR {
            dst: Operand::Virtual(ptr),
            src: Operand::Physical(Reg::Rsp),
        });
        ptr
    }

    fn foreign_get_vreg(&mut self, value: CfgValue) -> VReg {
        self.get_vreg(value)
    }

    fn foreign_image_arg_eightbytes(
        &mut self,
        value: CfgValue,
        image: &crate::foreign_call::AggregateImage,
    ) -> Vec<VReg> {
        self.image_arg_eightbytes(value, image)
    }

    fn foreign_byref_copy(
        &mut self,
        _value: CfgValue,
        _image: &crate::foreign_call::AggregateImage,
    ) -> VReg {
        panic!("SysV AMD64 does not pass foreign aggregates by reference")
    }

    fn foreign_emit_stack_args(&mut self, stack: &crate::foreign_call::ForeignStackArea) {
        // SysV packs its outgoing argument area in whole 8-byte slots, so the
        // planner's stores are always one full slot at an ascending cell offset
        // and this adapter keeps its push-based sequence: pushing in reverse
        // order leaves store 0 nearest `rsp`, which is the offset the planner
        // assigned it.
        let count =
            u64::try_from(stack.stores.len()).expect("foreign stack slot count must fit u64");
        for (index, store) in stack.stores.iter().enumerate() {
            let cell_offset = checked_cell_byte_offset(index as u64)
                .ok()
                .and_then(|offset| u32::try_from(offset).ok())
                .expect("foreign stack slot offset must fit an unsigned displacement");
            assert_eq!(
                (store.bytes, store.offset),
                (8, cell_offset),
                "SysV AMD64 stacks every argument in one whole 8-byte slot"
            );
        }
        let cell_bytes =
            checked_cell_region_bytes(count).expect("foreign stack area must fit displacement");
        if stack.bytes > cell_bytes {
            self.mir.push(X86Inst::AddRI {
                dst: Operand::Physical(Reg::Rsp),
                imm: -i32::try_from(stack.bytes - cell_bytes)
                    .expect("foreign stack alignment padding must fit i32"),
            });
        }
        for store in stack.stores.iter().rev() {
            self.mir.push(X86Inst::MovRR {
                dst: Operand::Physical(Reg::Rax),
                src: Operand::Virtual(store.value),
            });
            self.mir.push(X86Inst::Push {
                src: Operand::Physical(Reg::Rax),
            });
        }
    }

    fn foreign_emit_register_args(
        &mut self,
        register_args: &[crate::foreign_call::ForeignRegisterArg],
    ) {
        // Staging every value on the stack first and popping it into its
        // register afterwards keeps one argument's move from clobbering
        // another's source, whatever order the classification named.
        for arg in register_args.iter().rev() {
            self.mir.push(X86Inst::MovRR {
                dst: Operand::Physical(Reg::Rax),
                src: Operand::Virtual(arg.value),
            });
            self.mir.push(X86Inst::Push {
                src: Operand::Physical(Reg::Rax),
            });
        }
        for arg in register_args {
            self.mir.push(X86Inst::Pop {
                dst: Operand::Physical(foreign_argument_register(arg.class, arg.index)),
            });
        }
    }

    fn foreign_assign_sret(&mut self, _sret_ptr: VReg) {
        panic!("SysV AMD64 assigns sret through the first integer argument")
    }

    fn foreign_issue_call(&mut self, symbol: &str) {
        let symbol_id = self.intern_symbol(symbol);
        self.mir.push(X86Inst::call(symbol_id));
    }

    fn foreign_cleanup_stack(&mut self, stack: &crate::foreign_call::ForeignStackArea) {
        if stack.bytes > 0 {
            self.mir.push(X86Inst::AddRI {
                dst: Operand::Physical(Reg::Rsp),
                imm: checked_displacement_bytes(u64::from(stack.bytes))
                    .expect("foreign stack area must fit displacement"),
            });
        }
    }

    fn foreign_cleanup_byref(&mut self, _byref_bytes: u32) {}

    fn foreign_zero_result(&mut self, primary: VReg) {
        self.mir.push(X86Inst::MovRI32 {
            dst: Operand::Virtual(primary),
            imm: 0,
        });
    }

    fn foreign_scalar_result(&mut self, primary: VReg, ext: rue_air::ScalarAbiExtension) {
        self.mir.push(X86Inst::MovRR {
            dst: Operand::Virtual(primary),
            src: Operand::Physical(Reg::Rax),
        });
        self.emit_c_return_extension(primary, ext);
    }

    fn foreign_register_result(
        &mut self,
        _primary: VReg,
        image: &crate::foreign_call::AggregateImage,
    ) -> Vec<VReg> {
        self.mir.push(X86Inst::AddRI {
            dst: Operand::Physical(Reg::Rsp),
            imm: -checked_displacement_bytes(u64::from(image.storage_bytes))
                .expect("foreign result storage must fit displacement"),
        });
        let buf = self.mir.alloc_vreg();
        self.mir.push(X86Inst::MovRR {
            dst: Operand::Virtual(buf),
            src: Operand::Physical(Reg::Rsp),
        });
        let mut eb_vals = Vec::with_capacity(image.eightbytes() as usize);
        for index in 0..image.eightbytes() as usize {
            let v = self.mir.alloc_vreg();
            self.mir.push(X86Inst::MovRR {
                dst: Operand::Virtual(v),
                src: Operand::Physical(RET_REGS[index]),
            });
            eb_vals.push(v);
        }
        crate::agg_slots::store_slots_through_ptr(self, &eb_vals, buf, 0);
        let native = crate::agg_slots::load_enum_slots_through_ptr(self, buf, &image.map);
        self.mir.push(X86Inst::AddRI {
            dst: Operand::Physical(Reg::Rsp),
            imm: checked_displacement_bytes(u64::from(image.storage_bytes))
                .expect("foreign result storage must fit displacement"),
        });
        native
    }

    fn foreign_sret_result(
        &mut self,
        _primary: VReg,
        image: &crate::foreign_call::AggregateImage,
        sret_ptr: VReg,
    ) -> Vec<VReg> {
        let native = crate::agg_slots::load_enum_slots_through_ptr(self, sret_ptr, &image.map);
        self.mir.push(X86Inst::AddRI {
            dst: Operand::Physical(Reg::Rsp),
            imm: checked_displacement_bytes(u64::from(image.storage_bytes))
                .expect("foreign sret storage must fit displacement"),
        });
        native
    }

    fn foreign_move_primary(&mut self, primary: VReg, slot: VReg) {
        self.mir.push(X86Inst::MovRR {
            dst: Operand::Virtual(primary),
            src: Operand::Virtual(slot),
        });
    }
}

impl crate::agg_slots::SlotBackend for CfgLower<'_> {
    fn ctx(&self) -> &crate::cfg_lower::CfgLowerContext<'_> {
        &self.ctx
    }
    fn slot_cache(&mut self) -> &mut AHashMap<CfgValue, Vec<VReg>> {
        &mut self.struct_slot_vregs
    }
    fn alloc_vreg(&mut self) -> VReg {
        self.mir.alloc_vreg()
    }
    fn alloc_float_vreg(&mut self) -> VReg {
        self.mir.alloc_vreg_in(crate::reg_class::RegClass::Fp)
    }
    fn get_vreg(&mut self, value: CfgValue) -> VReg {
        CfgLower::get_vreg(self, value)
    }
    fn emit_load_slot(&mut self, dst: VReg, slot: u32) {
        let offset = self.ctx.local_offset(slot);
        if self.mir.vreg_class(dst) == crate::reg_class::RegClass::Fp {
            self.mir.push(X86Inst::FloatLoad {
                dst: Operand::Virtual(dst),
                base: Reg::Rbp,
                offset,
                width: FloatWidth::F64,
            });
        } else {
            self.mir.push(X86Inst::MovRM {
                dst: Operand::Virtual(dst),
                base: Reg::Rbp,
                offset,
            });
        }
    }
    fn emit_typed_float_load_slot(&mut self, dst: VReg, slot: u32, width: FloatWidth) {
        self.mir.push(X86Inst::FloatLoad {
            dst: Operand::Virtual(dst),
            base: Reg::Rbp,
            offset: self.ctx.local_offset(slot),
            width,
        });
    }
    fn emit_reg_move(&mut self, dst: VReg, src: VReg) {
        self.mir.push(X86Inst::MovRR {
            dst: Operand::Virtual(dst),
            src: Operand::Virtual(src),
        });
    }
    fn emit_store_slot(&mut self, src: VReg, slot: u32) {
        let offset = self.ctx.local_offset(slot);
        if self.mir.vreg_class(src) == crate::reg_class::RegClass::Fp {
            self.mir.push(X86Inst::FloatStore {
                base: Reg::Rbp,
                offset,
                src: Operand::Virtual(src),
                width: FloatWidth::F64,
            });
        } else {
            self.mir.push(X86Inst::MovMR {
                base: Reg::Rbp,
                offset,
                src: Operand::Virtual(src),
            });
        }
    }
    fn emit_typed_float_store_slot(&mut self, src: VReg, slot: u32, width: FloatWidth) {
        self.mir.push(X86Inst::FloatStore {
            base: Reg::Rbp,
            offset: self.ctx.local_offset(slot),
            src: Operand::Virtual(src),
            width,
        });
    }
    fn emit_store_through_ptr(&mut self, src: VReg, ptr: VReg, byte_offset: i32) {
        self.mir.push(X86Inst::MovMRIndexed {
            base: ptr,
            offset: byte_offset,
            src: Operand::Virtual(src),
        });
    }
    fn emit_load_through_ptr(&mut self, dst: VReg, ptr: VReg, byte_offset: i32) {
        self.mir.push(X86Inst::MovRMIndexed {
            dst: Operand::Virtual(dst),
            base: ptr,
            offset: byte_offset,
        });
    }
    fn emit_float_store_through_ptr(
        &mut self,
        src: VReg,
        ptr: VReg,
        byte_offset: i32,
        width: FloatWidth,
    ) {
        self.mir.push(X86Inst::MovRR {
            dst: Operand::Physical(Reg::Rax),
            src: Operand::Virtual(ptr),
        });
        self.mir.push(X86Inst::FloatStore {
            base: Reg::Rax,
            offset: byte_offset,
            src: Operand::Virtual(src),
            width,
        });
    }
    fn emit_float_load_through_ptr(
        &mut self,
        dst: VReg,
        ptr: VReg,
        byte_offset: i32,
        width: FloatWidth,
    ) {
        self.mir.push(X86Inst::MovRR {
            dst: Operand::Physical(Reg::Rax),
            src: Operand::Virtual(ptr),
        });
        self.mir.push(X86Inst::FloatLoad {
            dst: Operand::Virtual(dst),
            base: Reg::Rax,
            offset: byte_offset,
            width,
        });
    }
    fn emit_float_to_bits(&mut self, dst: VReg, src: VReg, width: FloatWidth) {
        self.mir.push(X86Inst::FloatToBits {
            dst: Operand::Virtual(dst),
            src: Operand::Virtual(src),
            width,
        });
    }
    fn emit_bits_to_float(&mut self, dst: VReg, src: VReg, width: FloatWidth) {
        self.mir.push(X86Inst::BitsToFloat {
            dst: Operand::Virtual(dst),
            src: Operand::Virtual(src),
            width,
        });
    }
    fn emit_narrow_store_through_ptr(
        &mut self,
        src: VReg,
        ptr: VReg,
        byte_offset: i32,
        access: crate::types::NarrowScalar,
    ) {
        // x86 `[base+disp]` encodes any i32 displacement (disp8/disp32), so fold
        // the byte offset straight into the narrow store's addressing mode
        // (RUE-1079) rather than materializing `base+offset` into a register.
        self.mir.push(X86Inst::NarrowStoreIndexed {
            base: ptr,
            src: Operand::Virtual(src),
            offset: byte_offset,
            width: access.width,
        });
    }
    fn emit_narrow_load_through_ptr(
        &mut self,
        dst: VReg,
        ptr: VReg,
        byte_offset: i32,
        access: crate::types::NarrowScalar,
    ) {
        // x86 folds any i32 displacement into the load's ModRM byte (RUE-1079).
        self.mir.push(X86Inst::NarrowLoadIndexed {
            dst: Operand::Virtual(dst),
            base: ptr,
            offset: byte_offset,
            width: access.width,
            signed: access.signed,
        });
    }
    fn emit_zero_vreg(&mut self) -> VReg {
        let dst = self.mir.alloc_vreg();
        self.mir.push(X86Inst::MovRI64 {
            dst: Operand::Virtual(dst),
            imm: 0,
        });
        dst
    }
    fn emit_set_zero(&mut self, dst: VReg) {
        self.mir.push(X86Inst::MovRI32 {
            dst: Operand::Virtual(dst),
            imm: 0,
        });
    }
    fn emit_set_float_zero(&mut self, dst: VReg, width: FloatWidth) {
        self.mir.push(X86Inst::FloatConst {
            dst: Operand::Virtual(dst),
            bits: 0,
            width,
        });
    }
    fn alloc_marshal_label(&mut self) -> LabelId {
        self.new_label()
    }
    fn emit_marshal_branch_if_tag_ne(&mut self, tag: VReg, discriminant: u64, label: LabelId) {
        let discriminant = compact_tag_discriminant(discriminant);
        emit_marshal_tag_ne(&mut self.mir, tag, discriminant, label);
    }
    fn emit_marshal_jump(&mut self, label: LabelId) {
        self.mir.push(X86Inst::Jmp { label });
    }
    fn emit_marshal_label(&mut self, label: LabelId) {
        self.mir.push(X86Inst::Label { id: label });
    }
}

impl crate::place_lower::PlaceLowerBackend for CfgLower<'_> {
    fn ensure_by_ref_param_ptr(&mut self, param_slot: u32) -> VReg {
        CfgLower::ensure_by_ref_param_ptr(self, param_slot)
    }

    fn emit_frame_addr(&mut self, dst: VReg, slot: u32) {
        let offset = self.ctx.local_offset(slot);
        self.mir.push(X86Inst::Lea {
            dst: Operand::Virtual(dst),
            base: Reg::Rbp,
            disp: offset,
        });
    }

    fn emit_addr_add(&mut self, dst: VReg, rhs: VReg) {
        self.mir.push(X86Inst::AddRR64 {
            dst: Operand::Virtual(dst),
            src: Operand::Virtual(rhs),
        });
    }

    fn emit_addr_add_imm(&mut self, dst: VReg, byte_offset: i32) {
        let offset = self.mir.alloc_vreg();
        self.mir.push(X86Inst::MovRI64 {
            dst: Operand::Virtual(offset),
            imm: byte_offset as i64,
        });
        self.mir.push(X86Inst::AddRR64 {
            dst: Operand::Virtual(dst),
            src: Operand::Virtual(offset),
        });
    }

    fn emit_scale_index_bytes(&mut self, scaled: VReg, plan: crate::allocation::ScalePlan) {
        <Self as crate::allocation::ScaleBackend>::emit_scale(self, scaled, scaled, plan);
    }

    fn emit_zero_sized_place(&mut self, dst: VReg) {
        self.mir.push(X86Inst::MovRI64 {
            dst: Operand::Virtual(dst),
            imm: 0,
        });
    }

    fn emit_zero_sized_place_addr(&mut self, dst: VReg) {
        self.mir.push(X86Inst::MovRI64 {
            dst: Operand::Virtual(dst),
            imm: crate::place_lower::ZERO_SIZED_PLACE_ADDR,
        });
    }

    fn emit_load_ptr_base(&mut self, dst: VReg, ptr: VReg, float_width: Option<FloatWidth>) {
        if let Some(width) = float_width {
            self.mir.push(X86Inst::MovRR {
                dst: Operand::Physical(Reg::Rax),
                src: Operand::Virtual(ptr),
            });
            self.mir.push(X86Inst::FloatLoad {
                dst: Operand::Virtual(dst),
                base: Reg::Rax,
                offset: 0,
                width,
            });
        } else {
            self.mir.push(X86Inst::MovRMIndexed {
                dst: Operand::Virtual(dst),
                base: ptr,
                offset: 0,
            });
        }
    }

    fn emit_float_load_slot(&mut self, dst: VReg, slot: u32, width: FloatWidth) {
        self.mir.push(X86Inst::FloatLoad {
            dst: Operand::Virtual(dst),
            base: Reg::Rbp,
            offset: self.ctx.local_offset(slot),
            width,
        });
    }

    fn emit_float_store_slot(&mut self, src: VReg, slot: u32, width: FloatWidth) {
        self.mir.push(X86Inst::FloatStore {
            base: Reg::Rbp,
            offset: self.ctx.local_offset(slot),
            src: Operand::Virtual(src),
            width,
        });
    }
}

impl crate::allocation::BoundsCheckBackend for CfgLower<'_> {
    fn alloc_bounds_length(&mut self, length: u64) -> VReg {
        let vreg = self.mir.alloc_vreg();
        self.mir.push(X86Inst::MovRI64 {
            dst: Operand::Virtual(vreg),
            imm: length as i64,
        });
        vreg
    }

    fn emit_bounds_compare(&mut self, index: VReg, length: VReg) {
        self.mir.push(X86Inst::Cmp64RR {
            src1: Operand::Virtual(index),
            src2: Operand::Virtual(length),
        });
    }

    fn alloc_bounds_label(&mut self) -> LabelId {
        self.new_label()
    }

    fn emit_bounds_branch(
        &mut self,
        condition: crate::allocation::BoundsCondition,
        label: LabelId,
    ) {
        match condition {
            crate::allocation::BoundsCondition::UnsignedIndexLessThanLength => {
                self.mir.push(X86Inst::Jb { label });
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
        self.mir.push(X86Inst::Label { id: label });
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
                ScaleKind::Zero => self.mir.push(X86Inst::MovRI32 {
                    dst: Operand::Virtual(dst),
                    imm: 0,
                }),
                ScaleKind::Identity => self.mir.push(X86Inst::MovRR {
                    dst: Operand::Virtual(dst),
                    src: Operand::Virtual(src),
                }),
                ScaleKind::Constant(8) => {
                    let shift_count = self.mir.alloc_vreg();
                    self.mir.push(X86Inst::MovRI32 {
                        dst: Operand::Virtual(shift_count),
                        imm: 3,
                    });
                    if dst != src {
                        self.mir.push(X86Inst::MovRR {
                            dst: Operand::Virtual(dst),
                            src: Operand::Virtual(src),
                        });
                    }
                    self.mir.push(X86Inst::Shl {
                        dst: Operand::Virtual(dst),
                        count: Operand::Virtual(shift_count),
                    });
                }
                ScaleKind::Constant(bytes) => {
                    let stride = self.mir.alloc_vreg();
                    self.mir.push(X86Inst::MovRI64 {
                        dst: Operand::Virtual(stride),
                        imm: bytes as i64,
                    });
                    if dst != src {
                        self.mir.push(X86Inst::MovRR {
                            dst: Operand::Virtual(dst),
                            src: Operand::Virtual(src),
                        });
                    }
                    self.mir.push(X86Inst::ImulRR64 {
                        dst: Operand::Virtual(dst),
                        src: Operand::Virtual(stride),
                    });
                }
            },
            (ScalePurpose::PointerOffset, OverflowBehavior::Wrap) => match plan.kind {
                ScaleKind::Zero => self.mir.push(X86Inst::MovRI32 {
                    dst: Operand::Virtual(dst),
                    imm: 0,
                }),
                ScaleKind::Identity => self.mir.push(X86Inst::MovRR {
                    dst: Operand::Virtual(dst),
                    src: Operand::Virtual(src),
                }),
                ScaleKind::Constant(bytes) => {
                    let stride = self.mir.alloc_vreg();
                    self.mir.push(X86Inst::MovRI64 {
                        dst: Operand::Virtual(stride),
                        imm: bytes as i64,
                    });
                    self.mir.push(X86Inst::MovRR {
                        dst: Operand::Physical(Reg::Rax),
                        src: Operand::Virtual(src),
                    });
                    self.mir.push(X86Inst::Mul64R {
                        src: Operand::Virtual(stride),
                    });
                    self.mir.push(X86Inst::MovRR {
                        dst: Operand::Virtual(dst),
                        src: Operand::Physical(Reg::Rax),
                    });
                }
            },
            (ScalePurpose::AllocationSize, OverflowBehavior::Trap) => match plan.kind {
                ScaleKind::Zero => self.mir.push(X86Inst::MovRI32 {
                    dst: Operand::Virtual(dst),
                    imm: 0,
                }),
                ScaleKind::Identity => self.mir.push(X86Inst::MovRR {
                    dst: Operand::Virtual(dst),
                    src: Operand::Virtual(src),
                }),
                ScaleKind::Constant(bytes) => {
                    let stride = self.mir.alloc_vreg();
                    self.mir.push(X86Inst::MovRI64 {
                        dst: Operand::Virtual(stride),
                        imm: bytes as i64,
                    });
                    self.mir.push(X86Inst::MovRR {
                        dst: Operand::Physical(Reg::Rax),
                        src: Operand::Virtual(src),
                    });
                    self.mir.push(X86Inst::Mul64R {
                        src: Operand::Virtual(stride),
                    });
                    self.mir.push(X86Inst::MovRR {
                        dst: Operand::Virtual(dst),
                        src: Operand::Physical(Reg::Rax),
                    });
                    let ok_label = self.new_label();
                    self.mir.push(X86Inst::Jae { label: ok_label });
                    let _ = self.lower_runtime_call(
                        crate::runtime_call_plan::RuntimeCallPlan::no_args(
                            rue_runtime_abi::RuntimeHelperId::Overflow,
                        ),
                    );
                    self.mir.push(X86Inst::Label { id: ok_label });
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
        self.mir.push(X86Inst::MovRI32 {
            dst: Operand::Virtual(dst),
            imm: value as i32,
        });
    }
    fn emit_slot_eq(
        &mut self,
        dst: VReg,
        mut lhs: VReg,
        mut rhs: VReg,
        leaf_ty: Type,
        bit_carrier: bool,
    ) {
        if let Some(width) = crate::value_plan::float_width(leaf_ty) {
            if bit_carrier {
                let lhs_fp = self.mir.alloc_vreg_in(crate::reg_class::RegClass::Fp);
                let rhs_fp = self.mir.alloc_vreg_in(crate::reg_class::RegClass::Fp);
                self.mir.push(X86Inst::BitsToFloat {
                    dst: Operand::Virtual(lhs_fp),
                    src: Operand::Virtual(lhs),
                    width,
                });
                self.mir.push(X86Inst::BitsToFloat {
                    dst: Operand::Virtual(rhs_fp),
                    src: Operand::Virtual(rhs),
                    width,
                });
                lhs = lhs_fp;
                rhs = rhs_fp;
            }
            self.mir.push(X86Inst::FloatCmp {
                lhs: Operand::Virtual(lhs),
                rhs: Operand::Virtual(rhs),
                width,
            });
            self.mir.push(X86Inst::Sete {
                dst: Operand::Virtual(dst),
            });
            let ordered = self.mir.alloc_vreg();
            self.mir.push(X86Inst::Setnp {
                dst: Operand::Virtual(ordered),
            });
            self.mir.push(X86Inst::AndRR {
                dst: Operand::Virtual(dst),
                src: Operand::Virtual(ordered),
            });
            self.mir.push(X86Inst::Movzx {
                dst: Operand::Virtual(dst),
                src: Operand::Virtual(dst),
            });
            return;
        }
        let wide = crate::types::slot_needs_wide_compare(leaf_ty);
        self.mir.push(if wide {
            X86Inst::Cmp64RR {
                src1: Operand::Virtual(lhs),
                src2: Operand::Virtual(rhs),
            }
        } else {
            X86Inst::CmpRR {
                src1: Operand::Virtual(lhs),
                src2: Operand::Virtual(rhs),
            }
        });
        self.mir.push(X86Inst::Sete {
            dst: Operand::Virtual(dst),
        });
        self.mir.push(X86Inst::Movzx {
            dst: Operand::Virtual(dst),
            src: Operand::Virtual(dst),
        });
    }
    fn emit_tag_eq(&mut self, dst: VReg, tag: VReg, discriminant: u32) {
        self.mir.push(X86Inst::CmpRI {
            src: Operand::Virtual(tag),
            imm: discriminant as i32,
        });
        self.mir.push(X86Inst::Sete {
            dst: Operand::Virtual(dst),
        });
        self.mir.push(X86Inst::Movzx {
            dst: Operand::Virtual(dst),
            src: Operand::Virtual(dst),
        });
    }
    fn emit_bool_and(&mut self, acc: VReg, rhs: VReg) {
        self.mir.push(X86Inst::AndRR {
            dst: Operand::Virtual(acc),
            src: Operand::Virtual(rhs),
        });
    }
    fn emit_bool_not(&mut self, value: VReg) {
        self.mir.push(X86Inst::XorRI {
            dst: Operand::Virtual(value),
            imm: 1,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rue_air::{
        EnumDef, EnumId, ParamSlotModes, SourceParamAbi, StructDef, StructField, StructId,
        TypeInternPool,
    };
    use rue_cfg::{CfgArgMode, CfgCallArg, CfgInst, CfgInstData, PlaceBase, Projection};
    use rue_span::{FileId, Span};

    /// The convention a test's parameter-storage plan places by: this
    /// backend's own target, whose native pairing decides how the incoming
    /// argument area is packed.
    const PLAN_CONVENTION: rue_target::ConventionSpec =
        rue_target::ConventionSpec::native(X86_64_TARGET);

    #[test]
    fn zero32_consumers_use_canonical_move_not_shift_pair() {
        let source = include_str!("cfg_lower.rs");
        let foreign = source
            .split("ScalarAbiExtension::Unsigned { from_bits: 32 } => {")
            .nth(1)
            .expect("foreign u32 return extension must remain present")
            .split("ScalarAbiExtension::Signed { from_bits }")
            .next()
            .expect("foreign extension match arm must have a following arm");
        assert!(foreign.contains("X86Inst::Movzx32To64"));
        assert!(!foreign.contains("X86Inst::ShlRI"));
        assert!(!foreign.contains("X86Inst::ShrRI"));
        let bitcast = source
            .split("BitCastForm::Zero32 => {")
            .nth(1)
            .expect("BitCastForm::Zero32 consumer must remain present")
            .split("\n                    }\n                }")
            .next()
            .expect("BitCastForm::Zero32 arm must be bounded");
        assert!(bitcast.contains("X86Inst::Movzx32To64"));
        assert!(!bitcast.contains("X86Inst::ShlRI"));
        assert!(!bitcast.contains("X86Inst::ShrRI"));
    }

    #[test]
    fn marshal_tag_cmp_imm_covers_i32_boundary() {
        assert_eq!(marshal_tag_cmp_imm(4095), Some(4095));
        assert_eq!(marshal_tag_cmp_imm(4096), Some(4096));
        assert_eq!(marshal_tag_cmp_imm(i32::MAX as u64), Some(i32::MAX));
        assert_eq!(marshal_tag_cmp_imm(i32::MAX as u64 + 1), None);
        assert_eq!(marshal_tag_cmp_imm(u32::MAX as u64), None);
    }

    #[test]
    fn marshal_tag_emits_immediate_and_full_width_fallback_mir_sequences() {
        for (discriminant, expected) in [(4095, 4095), (4096, 4096), (i32::MAX as u32, i32::MAX)] {
            let mut mir = X86Mir::new();
            let tag = mir.alloc_vreg();
            let label = LabelId::new(19);
            emit_marshal_tag_ne(&mut mir, tag, discriminant, label);
            assert_eq!(mir.vreg_count(), 1, "immediate compare must not allocate");
            assert!(matches!(
                mir.instructions(),
                [
                    X86Inst::Cmp64RI {
                        src: Operand::Virtual(src),
                        imm,
                    },
                    X86Inst::Jnz { label: branch_label },
                ] if *src == tag && *imm == expected && *branch_label == label
            ));
        }

        for discriminant in [i32::MAX as u64 + 1, u32::MAX as u64] {
            let mut mir = X86Mir::new();
            let tag = mir.alloc_vreg();
            let label = LabelId::new(19);
            emit_marshal_tag_ne(
                &mut mir,
                tag,
                u32::try_from(discriminant).expect("test discriminant must fit the compact tag"),
                label,
            );
            assert_eq!(
                mir.vreg_count(),
                2,
                "fallback compare must allocate its constant"
            );
            let constant = match mir.instructions() {
                [
                    X86Inst::MovRI64 {
                        dst: Operand::Virtual(constant),
                        imm,
                    },
                    ..,
                ] => {
                    assert_eq!(*imm, i64::try_from(discriminant).unwrap());
                    *constant
                }
                _ => panic!("fallback compare must materialize its constant first"),
            };
            assert!(matches!(
                mir.instructions(),
                [
                    X86Inst::MovRI64 { .. },
                    X86Inst::Cmp64RR {
                        src1: Operand::Virtual(src1),
                        src2: Operand::Virtual(src2),
                    },
                    X86Inst::Jnz { label: branch_label },
                ] if *src1 == tag && *src2 == constant && *branch_label == label
            ));
        }
    }

    #[test]
    #[should_panic(expected = "compact u32 tag representation")]
    fn marshal_tag_rejects_unrepresentable_discriminant() {
        let _ = compact_tag_discriminant(u32::MAX as u64 + 1);
    }

    #[test]
    fn physical_return_register_roster_matches_the_abi_kernel_budget() {
        assert_eq!(
            RET_REGS.len() as u32,
            rue_air::native_return_register_budget(rue_target::Arch::X86_64),
            "the backend's return-register roster and the classification \
             kernel's budget must agree"
        );
    }

    fn span() -> Span {
        Span::new(0, 0)
    }

    fn register_struct(
        pool: &TypeInternPool,
        interner: &ThreadedRodeo,
        name: &str,
        fields: &[(&str, Type)],
    ) -> StructId {
        let (id, _) = pool.register_struct(
            interner.get_or_intern(name),
            StructDef {
                name: name.into(),
                fields: fields
                    .iter()
                    .map(|(field, ty)| StructField {
                        name: (*field).to_string(),
                        ty: *ty,
                    })
                    .collect(),
                is_copy: false,
                is_linear: false,
                declared_linear: false,
                destructor: None,
                is_builtin: false,
                is_pub: false,
                file_id: FileId::DEFAULT,
            },
        );
        id
    }

    fn register_enum(
        pool: &TypeInternPool,
        interner: &ThreadedRodeo,
        name: &str,
        variants: &[(&str, Vec<Type>)],
    ) -> EnumId {
        let (id, _) = pool.register_enum(
            interner.get_or_intern(name),
            EnumDef {
                name: name.into(),
                variants: variants
                    .iter()
                    .map(|(variant, _)| (*variant).into())
                    .collect(),
                variant_payloads: variants
                    .iter()
                    .map(|(_, payload)| payload.clone())
                    .collect(),
                is_pub: false,
                is_non_exhaustive: false,
                file_id: FileId::DEFAULT,
            },
        );
        id
    }

    /// One direct scalar ABI descriptor per parameter slot.
    fn scalar_param_abi(count: u32) -> Vec<SourceParamAbi> {
        (0..count)
            .map(|slot| SourceParamAbi {
                start_slot: slot,
                slot_count: 1,
                crossing_regs: 1,
                crossing_classes: vec![rue_air::NativeArgClass::Gp],
                ty: None,
            })
            .collect()
    }

    /// A CFG fixture under construction. Instructions append to `current`,
    /// which multi-block fixtures retarget as they go.
    struct FixtureCfg<'a> {
        cfg: Cfg,
        current: BlockId,
        pool: &'a FrozenTypeInternPool,
        interner: &'a ThreadedRodeo,
    }

    impl<'a> FixtureCfg<'a> {
        fn new(
            return_type: Type,
            num_locals: u32,
            name: &str,
            param_modes: ParamSlotModes,
            source_param_abi: Vec<SourceParamAbi>,
            pool: &'a FrozenTypeInternPool,
            interner: &'a ThreadedRodeo,
        ) -> Self {
            let num_params = param_modes.by_ref().len() as u32;
            let mut cfg = Cfg::new(
                return_type,
                num_locals,
                num_params,
                name.to_string(),
                param_modes,
            );
            cfg.set_source_param_abi(source_param_abi);
            let entry = cfg.new_block();
            cfg.entry = entry;
            Self {
                cfg,
                current: entry,
                pool,
                interner,
            }
        }

        fn value(&mut self, data: CfgInstData, ty: Type) -> CfgValue {
            self.cfg.append_inst(
                self.current,
                CfgInst {
                    data,
                    ty,
                    span: span(),
                },
            )
        }

        fn konst(&mut self, literal: u64, ty: Type) -> CfgValue {
            self.value(CfgInstData::Const(literal), ty)
        }

        fn unit(&mut self) -> CfgValue {
            self.konst(0, Type::UNIT)
        }

        fn cast(&mut self, from_value: CfgValue, from_ty: Type, ty: Type) -> CfgValue {
            self.value(
                CfgInstData::IntCast {
                    value: from_value,
                    from_ty,
                },
                ty,
            )
        }

        fn live(&mut self, slot: u32, local_ty: Type) {
            self.value(CfgInstData::StorageLive { slot, local_ty }, Type::UNIT);
        }

        fn dead(&mut self, slot: u32, local_ty: Type) {
            self.value(CfgInstData::StorageDead { slot, local_ty }, Type::UNIT);
        }

        fn alloc(&mut self, slot: u32, init: CfgValue) {
            self.value(CfgInstData::Alloc { slot, init }, Type::UNIT);
        }

        fn load(&mut self, slot: u32, ty: Type) -> CfgValue {
            self.value(CfgInstData::Load { slot }, ty)
        }

        fn param(&mut self, index: u32, ty: Type) -> CfgValue {
            self.value(CfgInstData::Param { index }, ty)
        }

        fn intrinsic(
            &mut self,
            operation: rue_air::IntrinsicOperation,
            name: &str,
            args: &[CfgValue],
            ty: Type,
        ) -> CfgValue {
            let name = self.interner.get_or_intern(name);
            self.cfg
                .append_intrinsic_operation(
                    self.current,
                    operation,
                    name,
                    args.iter().copied(),
                    ty,
                    span(),
                )
                .unwrap()
        }

        fn call(&mut self, name: &str, args: Vec<CfgCallArg>, ty: Type) -> CfgValue {
            let name = self.interner.get_or_intern(name);
            self.cfg
                .append_call(self.current, None, name, args, ty, span())
                .unwrap()
        }

        fn struct_init(&mut self, struct_id: StructId, fields: &[CfgValue], ty: Type) -> CfgValue {
            self.cfg
                .append_struct_init(self.current, struct_id, fields.iter().copied(), ty, span())
                .unwrap()
        }

        fn array_init(&mut self, elements: &[CfgValue], ty: Type) -> CfgValue {
            self.cfg
                .append_array_init(self.current, elements.iter().copied(), ty, span())
                .unwrap()
        }

        fn enum_variant(
            &mut self,
            enum_id: EnumId,
            variant_index: u32,
            payload: &[CfgValue],
            ty: Type,
        ) -> CfgValue {
            self.cfg
                .append_enum_variant(
                    self.pool,
                    self.current,
                    enum_id,
                    variant_index,
                    payload.iter().copied(),
                    ty,
                    span(),
                )
                .unwrap()
        }

        fn place_read(
            &mut self,
            base: PlaceBase,
            base_type: Type,
            projections: impl IntoIterator<Item = Projection>,
            ty: Type,
        ) -> CfgValue {
            self.cfg
                .append_place_read(self.current, base, base_type, projections, ty, span())
                .unwrap()
        }

        fn ret(&mut self, result: Option<CfgValue>) {
            self.cfg.set_return(self.current, result);
        }

        fn lower(self) -> rue_error::CompileResult<X86Mir> {
            let cfg = self.cfg.finish(self.pool).expect("test CFG must verify");
            CfgLower::new(&cfg, self.pool, self.interner).lower()
        }

        /// Lower with the pipeline's real parameter storage plan applied
        /// (RUE-1170), as production lowering does.
        fn lower_with_plan(self) -> X86Mir {
            let cfg = self.cfg.finish(self.pool).expect("test CFG must verify");
            let plan = crate::param_storage::ParamStoragePlan::plan(
                &cfg,
                self.pool,
                false,
                PLAN_CONVENTION,
                ARG_REGS.len() as u32,
            );
            CfgLower::new(&cfg, self.pool, self.interner)
                .with_param_storage(&plan)
                .lower()
                .unwrap()
        }
    }

    /// The `let p: ptr mut T = @int_to_ptr(@ptr_to_int(@alloc(size, align)))`
    /// opening every checked pointer-marshalling fixture uses: allocate,
    /// launder the provenance through an integer, and store the typed pointer
    /// in `slot`. `raw_ptr_ty` is `@alloc`'s own `ptr mut u8` result type.
    fn checked_alloc(
        fixture: &mut FixtureCfg<'_>,
        slot: u32,
        size: u64,
        align: u64,
        raw_ptr_ty: Type,
        ptr_ty: Type,
    ) {
        fixture.live(slot, ptr_ty);
        let size_i32 = fixture.konst(size, Type::I32);
        let size = fixture.cast(size_i32, Type::I32, Type::U64);
        let align_i32 = fixture.konst(align, Type::I32);
        let align = fixture.cast(align_i32, Type::I32, Type::U64);
        let raw = fixture.intrinsic(
            rue_air::IntrinsicOperation::Alloc,
            "alloc",
            &[size, align],
            raw_ptr_ty,
        );
        let address = fixture.intrinsic(
            rue_air::IntrinsicOperation::PtrToInt,
            "ptr_to_int",
            &[raw],
            Type::U64,
        );
        let pointer = fixture.intrinsic(
            rue_air::IntrinsicOperation::IntToPtr,
            "int_to_ptr",
            &[address],
            ptr_ty,
        );
        fixture.alloc(slot, pointer);
    }

    /// The matching `@free(@int_to_ptr(@ptr_to_int(p)), size, align)` closing.
    fn checked_free(
        fixture: &mut FixtureCfg<'_>,
        slot: u32,
        size: u64,
        align: u64,
        raw_ptr_ty: Type,
        ptr_ty: Type,
    ) {
        let pointer = fixture.load(slot, ptr_ty);
        let address = fixture.intrinsic(
            rue_air::IntrinsicOperation::PtrToInt,
            "ptr_to_int",
            &[pointer],
            Type::U64,
        );
        let laundered = fixture.intrinsic(
            rue_air::IntrinsicOperation::IntToPtr,
            "int_to_ptr",
            &[address],
            raw_ptr_ty,
        );
        let size_i32 = fixture.konst(size, Type::I32);
        let size = fixture.cast(size_i32, Type::I32, Type::U64);
        let align_i32 = fixture.konst(align, Type::I32);
        let align = fixture.cast(align_i32, Type::I32, Type::U64);
        fixture.intrinsic(
            rue_air::IntrinsicOperation::Free,
            "free",
            &[laundered, size, align],
            Type::UNIT,
        );
        fixture.unit();
    }

    /// RUE-1014: under `aggregate_layout`, a non-slot-identical **array** frame
    /// value now lowers: array `[]` indexing strides by the *slot* stride
    /// (`abi_slot_count(element) * SLOT_BYTES`) because the frame stores every
    /// element slot-shaped (RUE-975), so element addressing is physically correct
    /// even when the compact element size diverges from the slot stride.
    #[test]
    fn aggregate_layout_allows_frame_array_slot_stride_indexing() {
        // `let a: [i32; 3] = [1, 2, 3]; let i: u64 = 1; a[i]`
        let interner = ThreadedRodeo::new();
        let pool = TypeInternPool::new();
        let array_ty = Type::new_array(pool.intern_array_from_type(Type::I32, 3));
        let pool = pool.freeze();
        let mut fixture = FixtureCfg::new(
            Type::I32,
            4,
            "main",
            ParamSlotModes::new(vec![], vec![]),
            vec![],
            &pool,
            &interner,
        );
        fixture.live(0, array_ty);
        let elements: Vec<CfgValue> = [1u64, 2, 3]
            .into_iter()
            .map(|literal| fixture.konst(literal, Type::I32))
            .collect();
        let array = fixture.array_init(&elements, array_ty);
        fixture.alloc(0, array);
        fixture.live(3, Type::U64);
        let one = fixture.konst(1, Type::U64);
        fixture.alloc(3, one);
        let index = fixture.load(3, Type::U64);
        let element = fixture.place_read(
            PlaceBase::Local(0),
            array_ty,
            [Projection::Index {
                array_type: array_ty,
                index,
            }],
            Type::I32,
        );
        fixture.dead(3, Type::U64);
        fixture.dead(0, array_ty);
        fixture.ret(Some(element));
        fixture
            .lower()
            .expect("a frame array indexed at the slot stride must lower under compact layout");
    }

    /// RUE-989: narrow scalar access through a typed pointer lowers, emitting the
    /// narrow (1/2/4-byte) load/store pseudos instead of a full eight-byte slot
    /// access.
    #[test]
    fn aggregate_layout_allows_narrow_scalar_physical_access() {
        // `let p: ptr mut i32 = @int_to_ptr(@ptr_to_int(@alloc(4, 4)));
        //  @ptr_write(p, 5); @dbg(@ptr_read(p)); @free(..., 4, 4); 0`
        let interner = ThreadedRodeo::new();
        let pool = TypeInternPool::new();
        let raw_ptr_ty = Type::new_ptr_mut(pool.intern_ptr_mut_from_type(Type::U8));
        let ptr_ty = Type::new_ptr_mut(pool.intern_ptr_mut_from_type(Type::I32));
        let pool = pool.freeze();
        let mut fixture = FixtureCfg::new(
            Type::I32,
            1,
            "main",
            ParamSlotModes::new(vec![], vec![]),
            vec![],
            &pool,
            &interner,
        );
        checked_alloc(&mut fixture, 0, 4, 4, raw_ptr_ty, ptr_ty);
        let pointer = fixture.load(0, ptr_ty);
        let five = fixture.konst(5, Type::I32);
        fixture.intrinsic(
            rue_air::IntrinsicOperation::PtrWrite,
            "ptr_write",
            &[pointer, five],
            Type::UNIT,
        );
        fixture.unit();
        let pointer = fixture.load(0, ptr_ty);
        let read = fixture.intrinsic(
            rue_air::IntrinsicOperation::PtrRead,
            "ptr_read",
            &[pointer],
            Type::I32,
        );
        fixture.intrinsic(
            rue_air::IntrinsicOperation::DebugI64,
            "dbg",
            &[read],
            Type::UNIT,
        );
        fixture.unit();
        checked_free(&mut fixture, 0, 4, 4, raw_ptr_ty, ptr_ty);
        fixture.unit();
        fixture.dead(0, ptr_ty);
        let result = fixture.konst(0, Type::I32);
        fixture.ret(Some(result));
        let mir = fixture
            .lower()
            .expect("a narrow scalar through a typed pointer must lower under compact layout");
        assert!(
            mir.instructions()
                .iter()
                .any(|inst| matches!(inst, X86Inst::NarrowStoreIndexed { width: 4, .. })),
            "a narrow i32 @ptr_write must emit a 4-byte narrow store"
        );
        assert!(
            mir.instructions().iter().any(|inst| matches!(
                inst,
                X86Inst::NarrowLoadIndexed {
                    width: 4,
                    signed: true,
                    ..
                }
            )),
            "a narrow i32 @ptr_read must emit a sign-extending 4-byte narrow load"
        );
    }

    /// RUE-1000: a compact enum with a variant-independent memory image
    /// round-trips through a typed pointer, marshalling the narrow tag at offset 0
    /// and the payload at its compact offset.
    #[test]
    fn aggregate_layout_allows_compact_enum_memory() {
        // `enum Opt { Some(i32), None }` written through `ptr mut Opt`, read
        // back, and matched: `@ptr_write(p, Opt.Some(42)); let e = @ptr_read(p);
        // match e { Opt.Some(x) => @dbg(x), Opt.None => @dbg(0) }`.
        let interner = ThreadedRodeo::new();
        let pool = TypeInternPool::new();
        let opt_id = register_enum(
            &pool,
            &interner,
            "Opt",
            &[("Some", vec![Type::I32]), ("None", vec![])],
        );
        let opt_ty = Type::new_enum(opt_id);
        let raw_ptr_ty = Type::new_ptr_mut(pool.intern_ptr_mut_from_type(Type::U8));
        let ptr_ty = Type::new_ptr_mut(pool.intern_ptr_mut_from_type(opt_ty));
        let pool = pool.freeze();
        let mut fixture = FixtureCfg::new(
            Type::I32,
            4,
            "main",
            ParamSlotModes::new(vec![], vec![]),
            vec![],
            &pool,
            &interner,
        );
        checked_alloc(&mut fixture, 0, 8, 4, raw_ptr_ty, ptr_ty);
        let pointer = fixture.load(0, ptr_ty);
        let forty_two = fixture.konst(42, Type::I32);
        let variant = fixture.enum_variant(opt_id, 0, &[forty_two], opt_ty);
        fixture.intrinsic(
            rue_air::IntrinsicOperation::PtrWrite,
            "ptr_write",
            &[pointer, variant],
            Type::UNIT,
        );
        fixture.unit();
        fixture.live(1, opt_ty);
        let pointer = fixture.load(0, ptr_ty);
        let read = fixture.intrinsic(
            rue_air::IntrinsicOperation::PtrRead,
            "ptr_read",
            &[pointer],
            opt_ty,
        );
        fixture.alloc(1, read);
        let scrutinee = fixture.load(1, opt_ty);
        let some_arm = fixture.cfg.new_block();
        let none_arm = fixture.cfg.new_block();
        let join = fixture.cfg.new_block();
        fixture
            .cfg
            .set_switch(fixture.current, scrutinee, [(0, some_arm)], none_arm);

        fixture.current = some_arm;
        fixture.live(3, Type::I32);
        let payload = fixture.value(
            CfgInstData::EnumPayloadGet {
                base: scrutinee,
                enum_id: opt_id,
                variant_index: 0,
                field_index: 0,
            },
            Type::I32,
        );
        fixture.alloc(3, payload);
        let x = fixture.load(3, Type::I32);
        fixture.intrinsic(
            rue_air::IntrinsicOperation::DebugI64,
            "dbg",
            &[x],
            Type::UNIT,
        );
        fixture.unit();
        fixture.dead(3, Type::I32);
        fixture.cfg.set_goto(some_arm, join, []);

        fixture.current = none_arm;
        let zero = fixture.konst(0, Type::I32);
        fixture.intrinsic(
            rue_air::IntrinsicOperation::DebugI64,
            "dbg",
            &[zero],
            Type::UNIT,
        );
        fixture.unit();
        fixture.cfg.set_goto(none_arm, join, []);

        fixture.current = join;
        checked_free(&mut fixture, 0, 8, 4, raw_ptr_ty, ptr_ty);
        fixture.unit();
        fixture.dead(1, opt_ty);
        fixture.dead(0, ptr_ty);
        let result = fixture.konst(0, Type::I32);
        fixture.ret(Some(result));
        let mir = fixture
            .lower()
            .expect("a compact enum with a variant-independent image must lower");
        assert!(
            mir.instructions()
                .iter()
                .any(|inst| matches!(inst, X86Inst::NarrowStoreIndexed { width: 1, .. })),
            "the enum write must store the u8 tag narrow at offset 0"
        );
        assert!(
            mir.instructions().iter().any(|inst| matches!(
                inst,
                X86Inst::NarrowLoadIndexed {
                    width: 1,
                    signed: false,
                    ..
                }
            )),
            "the enum read must load the u8 tag zero-extended"
        );
    }

    /// RUE-987: a whole compact struct round-trips through a typed pointer on
    /// x86-64, reusing the enum-image slot machinery — `@ptr_write` stores each
    /// field narrow at its compact offset and `@ptr_read` reloads it.
    #[test]
    fn aggregate_layout_allows_compact_struct_memory() {
        // `struct Padded { a: u8, b: i32, c: u8 }` written whole through
        // `ptr mut Padded`, read back, and one field debugged:
        // `@ptr_write(p, Padded { a: 7, b: 1000, c: 9 }); let s = @ptr_read(p); @dbg(s.b)`.
        let interner = ThreadedRodeo::new();
        let pool = TypeInternPool::new();
        let padded_id = register_struct(
            &pool,
            &interner,
            "Padded",
            &[("a", Type::U8), ("b", Type::I32), ("c", Type::U8)],
        );
        let padded_ty = Type::new_struct(padded_id);
        let raw_ptr_ty = Type::new_ptr_mut(pool.intern_ptr_mut_from_type(Type::U8));
        let ptr_ty = Type::new_ptr_mut(pool.intern_ptr_mut_from_type(padded_ty));
        let pool = pool.freeze();
        let mut fixture = FixtureCfg::new(
            Type::I32,
            4,
            "main",
            ParamSlotModes::new(vec![], vec![]),
            vec![],
            &pool,
            &interner,
        );
        checked_alloc(&mut fixture, 0, 12, 4, raw_ptr_ty, ptr_ty);
        let pointer = fixture.load(0, ptr_ty);
        let a = fixture.konst(7, Type::U8);
        let b = fixture.konst(1000, Type::I32);
        let c = fixture.konst(9, Type::U8);
        let padded = fixture.struct_init(padded_id, &[a, b, c], padded_ty);
        fixture.intrinsic(
            rue_air::IntrinsicOperation::PtrWrite,
            "ptr_write",
            &[pointer, padded],
            Type::UNIT,
        );
        fixture.unit();
        fixture.live(1, padded_ty);
        let pointer = fixture.load(0, ptr_ty);
        let read = fixture.intrinsic(
            rue_air::IntrinsicOperation::PtrRead,
            "ptr_read",
            &[pointer],
            padded_ty,
        );
        fixture.alloc(1, read);
        let b = fixture.place_read(
            PlaceBase::Local(1),
            padded_ty,
            [Projection::Field {
                struct_id: padded_id,
                field_index: 1,
            }],
            Type::I32,
        );
        fixture.intrinsic(
            rue_air::IntrinsicOperation::DebugI64,
            "dbg",
            &[b],
            Type::UNIT,
        );
        fixture.unit();
        checked_free(&mut fixture, 0, 12, 4, raw_ptr_ty, ptr_ty);
        fixture.unit();
        fixture.dead(1, padded_ty);
        fixture.dead(0, ptr_ty);
        let result = fixture.konst(0, Type::I32);
        fixture.ret(Some(result));
        let mir = fixture.lower().expect(
            "a whole compact struct through a typed pointer must lower under compact layout",
        );
        assert!(
            mir.instructions()
                .iter()
                .any(|inst| matches!(inst, X86Inst::NarrowStoreIndexed { width: 1, .. })),
            "the struct write must store the u8 fields narrow at their compact offsets"
        );
        assert!(
            mir.instructions()
                .iter()
                .any(|inst| matches!(inst, X86Inst::NarrowLoadIndexed { width: 4, .. })),
            "the struct read must reload the i32 field narrow from its compact offset"
        );
    }

    /// RUE-1014: a struct containing a fixed array now HAS a variant-independent
    /// compact image — the array flattens to its elements at the compact stride —
    /// so it marshals whole through a pointer on x86-64. The narrow element
    /// stores/loads land at the compact byte offsets.
    #[test]
    fn aggregate_layout_allows_array_bearing_struct_memory() {
        // `struct HasArr { tag: u8, xs: [i32; 2] }` written whole through
        // `ptr mut HasArr`, read back, and both elements summed:
        // `@ptr_write(p, HasArr { tag: 5, xs: [10, 20] }); let v = @ptr_read(p);
        //  ... v.xs[0] + v.xs[1]`.
        let interner = ThreadedRodeo::new();
        let pool = TypeInternPool::new();
        let xs_ty = Type::new_array(pool.intern_array_from_type(Type::I32, 2));
        let has_arr_id = register_struct(
            &pool,
            &interner,
            "HasArr",
            &[("tag", Type::U8), ("xs", xs_ty)],
        );
        let has_arr_ty = Type::new_struct(has_arr_id);
        let raw_ptr_ty = Type::new_ptr_mut(pool.intern_ptr_mut_from_type(Type::U8));
        let ptr_ty = Type::new_ptr_mut(pool.intern_ptr_mut_from_type(has_arr_ty));
        let pool = pool.freeze();
        let mut fixture = FixtureCfg::new(
            Type::I32,
            5,
            "main",
            ParamSlotModes::new(vec![], vec![]),
            vec![],
            &pool,
            &interner,
        );
        fixture.live(4, Type::I32);
        checked_alloc(&mut fixture, 0, 12, 4, raw_ptr_ty, ptr_ty);
        let pointer = fixture.load(0, ptr_ty);
        let tag = fixture.konst(5, Type::U8);
        let ten = fixture.konst(10, Type::I32);
        let twenty = fixture.konst(20, Type::I32);
        let xs = fixture.array_init(&[ten, twenty], xs_ty);
        let has_arr = fixture.struct_init(has_arr_id, &[tag, xs], has_arr_ty);
        fixture.intrinsic(
            rue_air::IntrinsicOperation::PtrWrite,
            "ptr_write",
            &[pointer, has_arr],
            Type::UNIT,
        );
        fixture.unit();
        fixture.live(1, has_arr_ty);
        let pointer = fixture.load(0, ptr_ty);
        let read = fixture.intrinsic(
            rue_air::IntrinsicOperation::PtrRead,
            "ptr_read",
            &[pointer],
            has_arr_ty,
        );
        fixture.alloc(1, read);
        checked_free(&mut fixture, 0, 12, 4, raw_ptr_ty, ptr_ty);
        let mut elements = Vec::new();
        for literal in [0u64, 1] {
            let index = fixture.konst(literal, Type::I32);
            elements.push(fixture.place_read(
                PlaceBase::Local(1),
                has_arr_ty,
                [
                    Projection::Field {
                        struct_id: has_arr_id,
                        field_index: 1,
                    },
                    Projection::Index {
                        array_type: xs_ty,
                        index,
                    },
                ],
                Type::I32,
            ));
        }
        let sum = fixture.value(CfgInstData::Add(elements[0], elements[1]), Type::I32);
        fixture.dead(1, has_arr_ty);
        fixture.dead(0, ptr_ty);
        fixture.alloc(4, sum);
        let r = fixture.load(4, Type::I32);
        fixture.intrinsic(
            rue_air::IntrinsicOperation::DebugI64,
            "dbg",
            &[r],
            Type::UNIT,
        );
        fixture.unit();
        let result = fixture.konst(0, Type::I32);
        fixture.dead(4, Type::I32);
        fixture.ret(Some(result));
        let mir = fixture
            .lower()
            .expect("a struct with a compact array image must lower under compact layout");
        assert!(
            mir.instructions()
                .iter()
                .any(|inst| matches!(inst, X86Inst::NarrowStoreIndexed { width: 4, .. })),
            "the array elements must store narrow (4-byte i32) at their compact stride"
        );
    }

    /// RUE-1014: a variant-dependent enum whose per-variant aggregate payloads
    /// overlay one variant-independent scalar image (`Option(Point)`) marshals
    /// whole through a pointer on x86-64, extending each payload leaf narrow.
    #[test]
    fn aggregate_layout_allows_variant_dependent_enum_image() {
        // `enum Opt { Some(Point), None }` over `struct Point { x: i32, y: i32 }`
        // written whole through `ptr mut Opt`, read back, and matched:
        // `match v { Opt.Some(pt) => pt.x + pt.y, Opt.None => 0 - 1 }`.
        let interner = ThreadedRodeo::new();
        let pool = TypeInternPool::new();
        let point_id = register_struct(
            &pool,
            &interner,
            "Point",
            &[("x", Type::I32), ("y", Type::I32)],
        );
        let point_ty = Type::new_struct(point_id);
        let opt_id = register_enum(
            &pool,
            &interner,
            "Opt",
            &[("Some", vec![point_ty]), ("None", vec![])],
        );
        let opt_ty = Type::new_enum(opt_id);
        let raw_ptr_ty = Type::new_ptr_mut(pool.intern_ptr_mut_from_type(Type::U8));
        let ptr_ty = Type::new_ptr_mut(pool.intern_ptr_mut_from_type(opt_ty));
        let pool = pool.freeze();
        let mut fixture = FixtureCfg::new(
            Type::I32,
            7,
            "main",
            ParamSlotModes::new(vec![], vec![]),
            vec![],
            &pool,
            &interner,
        );
        fixture.live(6, Type::I32);
        checked_alloc(&mut fixture, 0, 12, 4, raw_ptr_ty, ptr_ty);
        let pointer = fixture.load(0, ptr_ty);
        let x = fixture.konst(40, Type::I32);
        let y = fixture.konst(2, Type::I32);
        let point = fixture.struct_init(point_id, &[x, y], point_ty);
        let variant = fixture.enum_variant(opt_id, 0, &[point], opt_ty);
        fixture.intrinsic(
            rue_air::IntrinsicOperation::PtrWrite,
            "ptr_write",
            &[pointer, variant],
            Type::UNIT,
        );
        fixture.unit();
        fixture.live(1, opt_ty);
        let pointer = fixture.load(0, ptr_ty);
        let read = fixture.intrinsic(
            rue_air::IntrinsicOperation::PtrRead,
            "ptr_read",
            &[pointer],
            opt_ty,
        );
        fixture.alloc(1, read);
        checked_free(&mut fixture, 0, 12, 4, raw_ptr_ty, ptr_ty);
        let scrutinee = fixture.load(1, opt_ty);
        let some_arm = fixture.cfg.new_block();
        let none_arm = fixture.cfg.new_block();
        let join = fixture.cfg.new_block();
        let join_value = fixture.cfg.add_block_param(join, Type::I32);
        fixture
            .cfg
            .set_switch(fixture.current, scrutinee, [(0, some_arm)], none_arm);

        fixture.current = some_arm;
        fixture.live(4, point_ty);
        let payload = fixture.value(
            CfgInstData::EnumPayloadGet {
                base: scrutinee,
                enum_id: opt_id,
                variant_index: 0,
                field_index: 0,
            },
            point_ty,
        );
        fixture.alloc(4, payload);
        let x = fixture.place_read(
            PlaceBase::Local(4),
            point_ty,
            [Projection::Field {
                struct_id: point_id,
                field_index: 0,
            }],
            Type::I32,
        );
        let y = fixture.place_read(
            PlaceBase::Local(4),
            point_ty,
            [Projection::Field {
                struct_id: point_id,
                field_index: 1,
            }],
            Type::I32,
        );
        let sum = fixture.value(CfgInstData::Add(x, y), Type::I32);
        fixture.dead(4, point_ty);
        fixture.cfg.set_goto(some_arm, join, [sum]);

        fixture.current = none_arm;
        let zero = fixture.konst(0, Type::I32);
        let one = fixture.konst(1, Type::I32);
        let negative = fixture.value(CfgInstData::Sub(zero, one), Type::I32);
        fixture.cfg.set_goto(none_arm, join, [negative]);

        fixture.current = join;
        fixture.dead(1, opt_ty);
        fixture.dead(0, ptr_ty);
        fixture.alloc(6, join_value);
        let r = fixture.load(6, Type::I32);
        fixture.intrinsic(
            rue_air::IntrinsicOperation::DebugI64,
            "dbg",
            &[r],
            Type::UNIT,
        );
        fixture.unit();
        let result = fixture.konst(0, Type::I32);
        fixture.dead(6, Type::I32);
        fixture.ret(Some(result));
        let mir = fixture
            .lower()
            .expect("an enum with a variant-independent struct-payload image must lower");
        assert!(
            mir.instructions()
                .iter()
                .any(|inst| matches!(inst, X86Inst::NarrowStoreIndexed { width: 4, .. })),
            "the struct-payload leaves must store narrow (4-byte i32) at their compact offsets"
        );
    }

    /// RUE-1037: a struct embedding a HETEROGENEOUS enum (`A(i64)` versus
    /// `B(i32, i32)`, whose payload layouts disagree) marshals through a pointer
    /// on x86-64 via a nested tag dispatch — the store compares the embedded tag
    /// and stores the active variant's leaves. (Previously refused as imageless.)
    #[test]
    fn aggregate_layout_marshals_struct_embedding_heterogeneous_enum() {
        // `enum Bad { A(i64), B(i32, i32) } struct HasBad { b: Bad }`:
        // `@ptr_write(p, HasBad { b: Bad.A(5) })` through `ptr mut HasBad`.
        let interner = ThreadedRodeo::new();
        let pool = TypeInternPool::new();
        let bad_id = register_enum(
            &pool,
            &interner,
            "Bad",
            &[("A", vec![Type::I64]), ("B", vec![Type::I32, Type::I32])],
        );
        let bad_ty = Type::new_enum(bad_id);
        let has_bad_id = register_struct(&pool, &interner, "HasBad", &[("b", bad_ty)]);
        let has_bad_ty = Type::new_struct(has_bad_id);
        let raw_ptr_ty = Type::new_ptr_mut(pool.intern_ptr_mut_from_type(Type::U8));
        let ptr_ty = Type::new_ptr_mut(pool.intern_ptr_mut_from_type(has_bad_ty));
        let pool = pool.freeze();
        let mut fixture = FixtureCfg::new(
            Type::I32,
            1,
            "main",
            ParamSlotModes::new(vec![], vec![]),
            vec![],
            &pool,
            &interner,
        );
        checked_alloc(&mut fixture, 0, 16, 8, raw_ptr_ty, ptr_ty);
        let pointer = fixture.load(0, ptr_ty);
        let five = fixture.konst(5, Type::I64);
        let variant = fixture.enum_variant(bad_id, 0, &[five], bad_ty);
        let has_bad = fixture.struct_init(has_bad_id, &[variant], has_bad_ty);
        fixture.intrinsic(
            rue_air::IntrinsicOperation::PtrWrite,
            "ptr_write",
            &[pointer, has_bad],
            Type::UNIT,
        );
        fixture.unit();
        checked_free(&mut fixture, 0, 16, 8, raw_ptr_ty, ptr_ty);
        fixture.unit();
        fixture.dead(0, ptr_ty);
        let result = fixture.konst(0, Type::I32);
        fixture.ret(Some(result));
        let mir = fixture
            .lower()
            .expect("a struct embedding a heterogeneous enum must lower via nested tag dispatch");
        assert!(
            mir.instructions()
                .iter()
                .any(|inst| matches!(inst, X86Inst::Cmp64RI { .. })),
            "the nested heterogeneous enum store must dispatch on the embedded tag"
        );
    }

    /// RUE-1004: a non-slot-identical struct returned by value is forced indirect
    /// (sret); the caller reads the callee-written compact image back from the
    /// sret buffer, extending each narrow field from its compact byte offset
    /// (`main` here is the caller).
    #[test]
    fn aggregate_layout_allows_compact_struct_sret_return() {
        // The caller of `fn make() -> Padded`: `let p = make(); @dbg(p.b); 0`.
        let interner = ThreadedRodeo::new();
        let pool = TypeInternPool::new();
        let padded_id = register_struct(
            &pool,
            &interner,
            "Padded",
            &[("a", Type::U8), ("b", Type::I32), ("c", Type::U8)],
        );
        let padded_ty = Type::new_struct(padded_id);
        let pool = pool.freeze();
        let mut fixture = FixtureCfg::new(
            Type::I32,
            3,
            "main",
            ParamSlotModes::new(vec![], vec![]),
            vec![],
            &pool,
            &interner,
        );
        fixture.live(0, padded_ty);
        let returned = fixture.call("make", vec![], padded_ty);
        fixture.alloc(0, returned);
        let b = fixture.place_read(
            PlaceBase::Local(0),
            padded_ty,
            [Projection::Field {
                struct_id: padded_id,
                field_index: 1,
            }],
            Type::I32,
        );
        fixture.intrinsic(
            rue_air::IntrinsicOperation::DebugI64,
            "dbg",
            &[b],
            Type::UNIT,
        );
        fixture.unit();
        let result = fixture.konst(0, Type::I32);
        fixture.dead(0, padded_ty);
        fixture.ret(Some(result));
        let mir = fixture
            .lower()
            .expect("a compact struct sret return must lower");
        assert!(
            mir.instructions()
                .iter()
                .any(|inst| matches!(inst, X86Inst::NarrowLoadIndexed { width: 1, .. })),
            "the caller must read the u8 fields narrow from the compact sret image"
        );
    }

    /// RUE-1037: a HETEROGENEOUS enum (variants whose payload layouts disagree) is
    /// marshalled through a pointer by dispatching on the runtime tag — the store
    /// emits a tag compare/branch per variant plus narrow field stores.
    #[test]
    fn aggregate_layout_heterogeneous_enum_ptr_write_tag_dispatches() {
        // `enum R { Ok(Point), Err(i64) }` over a three-field `Point`:
        // `fn store_it(p: ptr mut R) { @ptr_write(p, R.Ok(Point { x: 1, y: 2, z: 3 })); }`.
        let interner = ThreadedRodeo::new();
        let pool = TypeInternPool::new();
        let point_id = register_struct(
            &pool,
            &interner,
            "Point",
            &[("x", Type::I32), ("y", Type::I32), ("z", Type::I32)],
        );
        let point_ty = Type::new_struct(point_id);
        let r_id = register_enum(
            &pool,
            &interner,
            "R",
            &[("Ok", vec![point_ty]), ("Err", vec![Type::I64])],
        );
        let r_ty = Type::new_enum(r_id);
        let ptr_ty = Type::new_ptr_mut(pool.intern_ptr_mut_from_type(r_ty));
        let pool = pool.freeze();
        let mut fixture = FixtureCfg::new(
            Type::UNIT,
            0,
            "store_it",
            ParamSlotModes::new(vec![false], vec![false]),
            scalar_param_abi(1),
            &pool,
            &interner,
        );
        let pointer = fixture.param(0, ptr_ty);
        let x = fixture.konst(1, Type::I32);
        let y = fixture.konst(2, Type::I32);
        let z = fixture.konst(3, Type::I32);
        let point = fixture.struct_init(point_id, &[x, y, z], point_ty);
        let variant = fixture.enum_variant(r_id, 0, &[point], r_ty);
        fixture.intrinsic(
            rue_air::IntrinsicOperation::PtrWrite,
            "ptr_write",
            &[pointer, variant],
            Type::UNIT,
        );
        fixture.unit();
        fixture.unit();
        fixture.unit();
        fixture.ret(None);
        let mir = fixture
            .lower()
            .expect("a heterogeneous enum @ptr_write must lower via tag dispatch");
        assert!(
            mir.instructions()
                .iter()
                .any(|inst| matches!(inst, X86Inst::Cmp64RI { .. })),
            "the heterogeneous store must compare the tag to dispatch on the variant"
        );
        assert!(
            mir.instructions()
                .iter()
                .any(|inst| matches!(inst, X86Inst::NarrowStoreIndexed { .. })),
            "the active variant's narrow leaves must be stored narrow"
        );
    }

    /// RUE-1004: an `inout` non-slot-identical struct argument is accepted — the
    /// callee reads/writes the caller's slot-based frame storage through the
    /// by-reference pointer (slot-shaped transport), so the call site lowers
    /// rather than being refused.
    #[test]
    fn aggregate_layout_allows_inout_compact_struct_param() {
        // The caller of `fn bump(inout p: Padded)`:
        // `let mut s = Padded { a: 1, b: 2, c: 3 }; bump(inout s); s.b`.
        let interner = ThreadedRodeo::new();
        let pool = TypeInternPool::new();
        let padded_id = register_struct(
            &pool,
            &interner,
            "Padded",
            &[("a", Type::U8), ("b", Type::I32), ("c", Type::U8)],
        );
        let padded_ty = Type::new_struct(padded_id);
        let pool = pool.freeze();
        let mut fixture = FixtureCfg::new(
            Type::I32,
            3,
            "main",
            ParamSlotModes::new(vec![], vec![]),
            vec![],
            &pool,
            &interner,
        );
        fixture.live(0, padded_ty);
        let a = fixture.konst(1, Type::U8);
        let b = fixture.konst(2, Type::I32);
        let c = fixture.konst(3, Type::U8);
        let padded = fixture.struct_init(padded_id, &[a, b, c], padded_ty);
        fixture.alloc(0, padded);
        let argument = fixture.load(0, padded_ty);
        fixture.call(
            "bump",
            vec![CfgCallArg {
                value: argument,
                mode: CfgArgMode::Inout,
            }],
            Type::UNIT,
        );
        let b = fixture.place_read(
            PlaceBase::Local(0),
            padded_ty,
            [Projection::Field {
                struct_id: padded_id,
                field_index: 1,
            }],
            Type::I32,
        );
        fixture.dead(0, padded_ty);
        fixture.ret(Some(b));
        fixture
            .lower()
            .expect("an inout compact struct argument must lower");
    }

    /// RUE-1005: a non-slot-identical struct passed BY VALUE across a call lowers.
    /// The caller (`main`, first here) writes the aggregate's compact image into a
    /// caller-owned buffer with narrow stores and passes one pointer.
    #[test]
    fn aggregate_layout_allows_by_value_compact_struct_arg_caller() {
        // The caller side: `sum(Padded { a: 1, b: 5, c: 3 })`.
        let interner = ThreadedRodeo::new();
        let pool = TypeInternPool::new();
        let padded_id = register_struct(
            &pool,
            &interner,
            "Padded",
            &[("a", Type::U8), ("b", Type::I32), ("c", Type::U8)],
        );
        let padded_ty = Type::new_struct(padded_id);
        let pool = pool.freeze();
        let mut fixture = FixtureCfg::new(
            Type::I32,
            0,
            "main",
            ParamSlotModes::new(vec![], vec![]),
            vec![],
            &pool,
            &interner,
        );
        let a = fixture.konst(1, Type::U8);
        let b = fixture.konst(5, Type::I32);
        let c = fixture.konst(3, Type::U8);
        let padded = fixture.struct_init(padded_id, &[a, b, c], padded_ty);
        let result = fixture.call(
            "sum",
            vec![CfgCallArg {
                value: padded,
                mode: CfgArgMode::Normal,
            }],
            Type::I32,
        );
        fixture.ret(Some(result));
        let mir = fixture
            .lower()
            .expect("a by-value compact struct argument must lower");
        assert!(
            mir.instructions()
                .iter()
                .any(|inst| matches!(inst, X86Inst::NarrowStoreIndexed { width: 1, .. })),
            "the caller must write the u8 fields narrow into the compact argument buffer"
        );
    }

    /// RUE-1005 callee side: the function receiving a by-value compact aggregate
    /// unmarshals its compact image (narrow loads) from the homed pointer into
    /// its frame slots at entry.
    #[test]
    fn aggregate_layout_allows_by_value_compact_struct_arg_callee() {
        // The callee side: `fn sum(p: Padded) -> i32 { p.b }`, whose by-value
        // indirect parameter reserves three frame slots behind one pointer.
        let interner = ThreadedRodeo::new();
        let pool = TypeInternPool::new();
        let padded_id = register_struct(
            &pool,
            &interner,
            "Padded",
            &[("a", Type::U8), ("b", Type::I32), ("c", Type::U8)],
        );
        let padded_ty = Type::new_struct(padded_id);
        let pool = pool.freeze();
        let mut fixture = FixtureCfg::new(
            Type::I32,
            0,
            "sum",
            ParamSlotModes::new(vec![false; 3], vec![false; 3]),
            vec![SourceParamAbi {
                start_slot: 0,
                slot_count: 3,
                crossing_regs: 1,
                crossing_classes: vec![rue_air::NativeArgClass::Gp],
                ty: Some(padded_ty),
            }],
            &pool,
            &interner,
        );
        let b = fixture.place_read(
            PlaceBase::Param(0),
            padded_ty,
            [Projection::Field {
                struct_id: padded_id,
                field_index: 1,
            }],
            Type::I32,
        );
        fixture.ret(Some(b));
        let mir = fixture
            .lower()
            .expect("the callee of a by-value compact struct argument must lower");
        assert!(
            mir.instructions()
                .iter()
                .any(|inst| matches!(inst, X86Inst::NarrowLoadIndexed { width: 1, .. })),
            "the callee must unmarshal the u8 fields narrow from the homed pointer"
        );
    }

    /// RUE-1170: read-only scalar register arguments are entry-copied out of
    /// their incoming registers instead of being reloaded from frame homes.
    #[test]
    fn register_only_params_entry_copy_and_never_load_from_the_frame() {
        // `fn both(a: i64, b: i64) -> i64 { a + b }`, lowered with the real
        // parameter storage plan applied.
        let interner = ThreadedRodeo::new();
        let pool = FrozenTypeInternPool::new();
        let mut fixture = FixtureCfg::new(
            Type::I64,
            0,
            "both",
            ParamSlotModes::new(vec![false; 2], vec![false; 2]),
            scalar_param_abi(2),
            &pool,
            &interner,
        );
        let a = fixture.param(0, Type::I64);
        let b = fixture.param(1, Type::I64);
        let sum = fixture.value(CfgInstData::Add(a, b), Type::I64);
        fixture.ret(Some(sum));
        let mir = fixture.lower_with_plan();
        let insts = mir.instructions();
        assert!(
            matches!(
                insts[0],
                X86Inst::MovRR {
                    src: Operand::Physical(Reg::Rdi),
                    dst: Operand::Virtual(_),
                }
            ),
            "first instruction must copy rdi into a vreg, got {:?}",
            insts[0]
        );
        assert!(
            matches!(
                insts[1],
                X86Inst::MovRR {
                    src: Operand::Physical(Reg::Rsi),
                    dst: Operand::Virtual(_),
                }
            ),
            "second instruction must copy rsi into a vreg, got {:?}",
            insts[1]
        );
        assert!(
            !insts
                .iter()
                .any(|inst| matches!(inst, X86Inst::MovRM { base: Reg::Rbp, .. })),
            "register-only parameters must not be reloaded from the frame:\n{mir}"
        );
    }

    /// RUE-1170: an unused register argument produces no entry copy at all.
    #[test]
    fn unused_register_params_produce_no_code() {
        // `fn pick(a: i64, b: i64) -> i64 { 42 }`, lowered with the real
        // parameter storage plan applied.
        let interner = ThreadedRodeo::new();
        let pool = FrozenTypeInternPool::new();
        let mut fixture = FixtureCfg::new(
            Type::I64,
            0,
            "pick",
            ParamSlotModes::new(vec![false; 2], vec![false; 2]),
            scalar_param_abi(2),
            &pool,
            &interner,
        );
        let result = fixture.konst(42, Type::I64);
        fixture.ret(Some(result));
        let mir = fixture.lower_with_plan();
        assert!(
            !mir.instructions().iter().any(|inst| matches!(
                inst,
                X86Inst::MovRR {
                    src: Operand::Physical(Reg::Rdi | Reg::Rsi),
                    ..
                }
            )),
            "unused register arguments must not be copied:\n{mir}"
        );
    }

    /// RUE-1005 descriptor plumbing: a compact struct parameter crosses as one
    /// indirect pointer, so its plan entry consumes a single register while still
    /// reserving all three frame slots.
    #[test]
    fn param_homing_plan_collapses_indirect_compact_aggregate() {
        // `fn take(p: Padded, q: i32) -> i32 { p.b + q }` over the compact
        // `Padded { a: u8, b: i32, c: u8 }`.
        let interner = ThreadedRodeo::new();
        let pool = TypeInternPool::new();
        let padded_id = register_struct(
            &pool,
            &interner,
            "Padded",
            &[("a", Type::U8), ("b", Type::I32), ("c", Type::U8)],
        );
        let padded_ty = Type::new_struct(padded_id);
        let pool = pool.freeze();
        let mut fixture = FixtureCfg::new(
            Type::I32,
            0,
            "take",
            ParamSlotModes::new(vec![false; 4], vec![false; 4]),
            vec![
                SourceParamAbi {
                    start_slot: 0,
                    slot_count: 3,
                    crossing_regs: 1,
                    crossing_classes: vec![rue_air::NativeArgClass::Gp],
                    ty: Some(padded_ty),
                },
                SourceParamAbi {
                    start_slot: 3,
                    slot_count: 1,
                    crossing_regs: 1,
                    crossing_classes: vec![rue_air::NativeArgClass::Gp],
                    ty: None,
                },
            ],
            &pool,
            &interner,
        );
        let b = fixture.place_read(
            PlaceBase::Param(0),
            padded_ty,
            [Projection::Field {
                struct_id: padded_id,
                field_index: 1,
            }],
            Type::I32,
        );
        let q = fixture.param(3, Type::I32);
        let sum = fixture.value(CfgInstData::Add(b, q), Type::I32);
        fixture.ret(Some(sum));
        let cfg = fixture.cfg.finish(&pool).expect("test CFG must verify");
        let plan =
            crate::param_storage::ParamStoragePlan::plan(&cfg, &pool, false, PLAN_CONVENTION, 6);

        // The compact struct parameter collapses to one incoming pointer
        // register while its three frame slots stay reserved. The scalar `q`
        // is a read-only register argument, so it keeps no frame home at all
        // (RUE-1170) and appears in no homing entry — but its incoming
        // register is still the second one (`abi_index` 1), after the
        // struct's pointer.
        assert_eq!(cfg.num_params(), 4, "Padded (3 slots) + i32 (1 slot)");
        assert_eq!(plan.homed_area_slots(), 3, "only the aggregate is homed");
        assert_eq!(
            plan.homing(),
            [crate::codegen_pipeline::ParamHoming {
                start_slot: 0,
                class: crate::call_plan::AbiSlotClass::Gp,
                location: crate::call_plan::AbiSlotLocation::GpReg(0),
            }]
        );
        assert_eq!(
            plan.slot(3),
            crate::param_storage::ParamSlotStorage::Register {
                class: crate::call_plan::AbiSlotClass::Gp,
                location: crate::call_plan::AbiSlotLocation::GpReg(1),
            }
        );
    }

    /// RUE-1037: a compact enum whose union payload slots overlap (`A(i64)`
    /// versus `B(i32, i32)`, no variant-independent image) now marshals through a
    /// pointer via per-variant tag dispatch rather than being refused — the store
    /// dispatches on the tag and writes the active variant's leaves.
    #[test]
    fn aggregate_layout_marshals_variant_dependent_enum_memory() {
        // `enum Bad { A(i64), B(i32, i32) }`: `@ptr_write(p, Bad.A(5))`
        // through `ptr mut Bad`.
        let interner = ThreadedRodeo::new();
        let pool = TypeInternPool::new();
        let bad_id = register_enum(
            &pool,
            &interner,
            "Bad",
            &[("A", vec![Type::I64]), ("B", vec![Type::I32, Type::I32])],
        );
        let bad_ty = Type::new_enum(bad_id);
        let raw_ptr_ty = Type::new_ptr_mut(pool.intern_ptr_mut_from_type(Type::U8));
        let ptr_ty = Type::new_ptr_mut(pool.intern_ptr_mut_from_type(bad_ty));
        let pool = pool.freeze();
        let mut fixture = FixtureCfg::new(
            Type::I32,
            1,
            "main",
            ParamSlotModes::new(vec![], vec![]),
            vec![],
            &pool,
            &interner,
        );
        checked_alloc(&mut fixture, 0, 16, 8, raw_ptr_ty, ptr_ty);
        let pointer = fixture.load(0, ptr_ty);
        let five = fixture.konst(5, Type::I64);
        let variant = fixture.enum_variant(bad_id, 0, &[five], bad_ty);
        fixture.intrinsic(
            rue_air::IntrinsicOperation::PtrWrite,
            "ptr_write",
            &[pointer, variant],
            Type::UNIT,
        );
        fixture.unit();
        checked_free(&mut fixture, 0, 16, 8, raw_ptr_ty, ptr_ty);
        fixture.unit();
        fixture.dead(0, ptr_ty);
        let result = fixture.konst(0, Type::I32);
        fixture.ret(Some(result));
        let mir = fixture
            .lower()
            .expect("a variant-dependent enum memory image must lower via tag dispatch");
        assert!(
            mir.instructions()
                .iter()
                .any(|inst| matches!(inst, X86Inst::Cmp64RI { .. })),
            "the heterogeneous enum store must dispatch on the tag"
        );
    }

    /// A slot-identical aggregate (all eight-byte leaves) has an identical
    /// compact and slot layout, so its memory access marshals correctly and
    /// compact codegen is accepted.
    #[test]
    fn aggregate_layout_allows_slot_identical_physical_layout_codegen() {
        // `struct Cell { a: i64, b: i64 }`:
        // `let c = Cell { a: 7, b: 9 }; @intCast(c.a)`.
        let interner = ThreadedRodeo::new();
        let pool = TypeInternPool::new();
        let cell_id = register_struct(
            &pool,
            &interner,
            "Cell",
            &[("a", Type::I64), ("b", Type::I64)],
        );
        let cell_ty = Type::new_struct(cell_id);
        let pool = pool.freeze();
        let mut fixture = FixtureCfg::new(
            Type::I32,
            2,
            "main",
            ParamSlotModes::new(vec![], vec![]),
            vec![],
            &pool,
            &interner,
        );
        fixture.live(0, cell_ty);
        let a = fixture.konst(7, Type::I64);
        let b = fixture.konst(9, Type::I64);
        let cell = fixture.struct_init(cell_id, &[a, b], cell_ty);
        fixture.alloc(0, cell);
        let a = fixture.place_read(
            PlaceBase::Local(0),
            cell_ty,
            [Projection::Field {
                struct_id: cell_id,
                field_index: 0,
            }],
            Type::I64,
        );
        let result = fixture.cast(a, Type::I64, Type::I32);
        fixture.dead(0, cell_ty);
        fixture.ret(Some(result));
        fixture
            .lower()
            .expect("a slot-identical aggregate must lower under compact layout");
    }

    fn immediate_for_operand(mir: &X86Mir, operand: Operand) -> Option<i64> {
        let Operand::Virtual(vreg) = operand else {
            return None;
        };
        mir.instructions().iter().rev().find_map(|inst| match inst {
            X86Inst::MovRI64 {
                dst: Operand::Virtual(dst),
                imm,
            } if *dst == vreg => Some(*imm),
            X86Inst::MovRI32 {
                dst: Operand::Virtual(dst),
                imm,
            } if *dst == vreg => Some(i64::from(*imm)),
            _ => None,
        })
    }

    fn immediate_call_arg(mir: &X86Mir, call_index: usize, reg: Reg) -> Option<i64> {
        mir.instructions()[..call_index]
            .iter()
            .rev()
            .take_while(|inst| !matches!(inst, X86Inst::CallRel { .. }))
            .find_map(|inst| match inst {
                X86Inst::MovRI64 {
                    dst: Operand::Physical(dst),
                    imm,
                } if *dst == reg => Some(*imm),
                X86Inst::MovRI32 {
                    dst: Operand::Physical(dst),
                    imm,
                } if *dst == reg => Some(i64::from(*imm)),
                X86Inst::MovRR {
                    dst: Operand::Physical(dst),
                    src,
                } if *dst == reg => immediate_for_operand(mir, *src),
                _ => None,
            })
    }

    fn runtime_call_index(mir: &X86Mir, helper: rue_runtime_abi::RuntimeHelperId) -> usize {
        mir.instructions()
            .iter()
            .position(|inst| {
                matches!(inst, X86Inst::CallRel { symbol_id, .. } if mir.get_symbol(*symbol_id) == helper.symbol())
            })
            .unwrap_or_else(|| panic!("missing call to {helper:?}"))
    }

    #[test]
    fn runtime_manifest_fits_register_only_target_c_subset() {
        assert_eq!(
            crate::runtime_call_plan::RuntimeCallPlan::calling_convention(X86_64_TARGET),
            rue_target::CallingConvention::X86_64SysV,
            "the x86-64 runtime-helper boundary is SysV AMD64"
        );
        for id in rue_runtime_abi::RuntimeHelperId::ALL {
            let helper = id.helper();
            assert!(
                helper.parameters.len() <= ARG_REGS.len(),
                "{} has {} parameters, exceeding the x86-64 register-only runtime-call budget of {}",
                helper.symbol,
                helper.parameters.len(),
                ARG_REGS.len()
            );
        }
    }

    #[test]
    fn ordinary_call_uses_checked_aligned_stack_area() {
        // Seven scalar arguments leave one stack cell. The x86 lowering must
        // reserve 8 bytes of checked alignment padding, then restore the full
        // 16-byte aligned outgoing area after the call.
        let interner = ThreadedRodeo::new();
        let pool = FrozenTypeInternPool::new();
        let mut fixture = FixtureCfg::new(
            Type::I64,
            0,
            "caller",
            ParamSlotModes::new(vec![], vec![]),
            vec![],
            &pool,
            &interner,
        );
        let args = (0..7)
            .map(|value| CfgCallArg {
                value: fixture.konst(value, Type::I64),
                mode: CfgArgMode::Normal,
            })
            .collect();
        let result = fixture.call("callee", args, Type::I64);
        fixture.ret(Some(result));
        let mir = fixture.lower().expect("ordinary call must lower");
        let expected_padding = -i32::try_from(
            crate::frame_layout::checked_cell_region_padding_bytes(1)
                .expect("test call padding must fit the frame budget"),
        )
        .expect("test call padding must fit i32");
        let call_index = mir
            .instructions()
            .iter()
            .position(|inst| matches!(inst, X86Inst::CallRel { .. }))
            .expect("ordinary call must emit a call");
        assert!(mir.instructions()[..call_index].iter().any(|inst| {
            matches!(
                inst,
                X86Inst::AddRI {
                    dst: Operand::Physical(Reg::Rsp),
                    imm,
                }
                if *imm == expected_padding
            )
        }));
        assert!(mir.instructions()[call_index + 1..].iter().any(|inst| {
            matches!(
                inst,
                X86Inst::AddRI {
                    dst: Operand::Physical(Reg::Rsp),
                    imm: 16,
                }
            )
        }));
    }

    #[test]
    fn spilled_fp_call_argument_is_reloaded_before_xmm0_is_populated() {
        // Nine simultaneously-live f64 arguments exceed the eight allocatable
        // FP registers. A spill reload uses xmm0 as its reserved scratch, so
        // the ABI moves must run from xmm7 down through xmm0.
        let interner = ThreadedRodeo::new();
        let pool = FrozenTypeInternPool::new();
        let mut fixture = FixtureCfg::new(
            Type::F64,
            0,
            "caller",
            ParamSlotModes::new(vec![], vec![]),
            vec![],
            &pool,
            &interner,
        );
        let args = (1..=9)
            .map(|value| CfgCallArg {
                value: fixture.konst((value as f64).to_bits(), Type::F64),
                mode: CfgArgMode::Normal,
            })
            .collect();
        let result = fixture.call("sum9", args, Type::F64);
        fixture.ret(Some(result));

        let allocated = crate::x86_64::regalloc::RegAlloc::new(
            fixture.lower().expect("float call must lower"),
            0,
        )
        .allocate()
        .expect("float call must allocate");
        let call_index = allocated
            .instructions()
            .iter()
            .position(|inst| matches!(inst, X86Inst::CallRel { .. }))
            .expect("float call must emit a call");
        let call_setup = &allocated.instructions()[..call_index];
        let fp_destinations: Vec<_> = call_setup
            .iter()
            .filter_map(|inst| match inst {
                X86Inst::FloatMov {
                    dst: Operand::Physical(dst),
                    ..
                } if FP_ARG_REGS.contains(dst) => Some(*dst),
                _ => None,
            })
            .collect();
        assert_eq!(
            fp_destinations,
            vec![
                Reg::Xmm7,
                Reg::Xmm6,
                Reg::Xmm5,
                Reg::Xmm4,
                Reg::Xmm3,
                Reg::Xmm2,
                Reg::Xmm1,
                Reg::Xmm0,
            ]
        );
        let spill_reload = call_setup
            .iter()
            .position(|inst| {
                matches!(
                    inst,
                    X86Inst::FloatLoad {
                        dst: Operand::Physical(Reg::Xmm0),
                        ..
                    }
                )
            })
            .expect("register pressure must reload one FP argument through xmm0");
        let final_xmm0 = call_setup
            .iter()
            .rposition(|inst| {
                matches!(
                    inst,
                    X86Inst::FloatMov {
                        dst: Operand::Physical(Reg::Xmm0),
                        ..
                    }
                )
            })
            .expect("the first FP argument must populate xmm0");
        assert!(spill_reload < final_xmm0);
    }

    #[test]
    fn test_simple_return() {
        // `fn main() -> i32 { 42 }`
        let interner = ThreadedRodeo::new();
        let pool = FrozenTypeInternPool::new();
        let mut fixture = FixtureCfg::new(
            Type::I32,
            0,
            "main",
            ParamSlotModes::new(vec![], vec![]),
            vec![],
            &pool,
            &interner,
        );
        let result = fixture.konst(42, Type::I32);
        fixture.ret(Some(result));
        let mir = fixture.lower().expect("test lowering should succeed");
        assert!(!mir.instructions().is_empty());
    }

    #[test]
    fn unit_main_exits_with_zero_status() {
        // `fn main() {}`
        let interner = ThreadedRodeo::new();
        let pool = FrozenTypeInternPool::new();
        let mut fixture = FixtureCfg::new(
            Type::UNIT,
            0,
            "main",
            ParamSlotModes::new(vec![], vec![]),
            vec![],
            &pool,
            &interner,
        );
        fixture.unit();
        fixture.ret(None);
        let mir = fixture.lower().expect("test lowering should succeed");
        assert!(mir.instructions().windows(2).any(|pair| {
            matches!(
                pair,
                [
                    X86Inst::MovRI32 {
                        dst: Operand::Physical(Reg::Rdi),
                        imm: 0,
                    },
                    X86Inst::CallRel { .. },
                ]
            )
        }));
    }

    #[test]
    fn test_arithmetic() {
        // `fn main() -> i32 { 1 + 2 }`
        let interner = ThreadedRodeo::new();
        let pool = FrozenTypeInternPool::new();
        let mut fixture = FixtureCfg::new(
            Type::I32,
            0,
            "main",
            ParamSlotModes::new(vec![], vec![]),
            vec![],
            &pool,
            &interner,
        );
        let one = fixture.konst(1, Type::I32);
        let two = fixture.konst(2, Type::I32);
        let sum = fixture.value(CfgInstData::Add(one, two), Type::I32);
        fixture.ret(Some(sum));
        let mir = fixture.lower().expect("test lowering should succeed");
        assert!(!mir.instructions().is_empty());
    }

    #[test]
    fn test_if_else() {
        // `fn main() -> i32 { if true { 1 } else { 2 } }`
        let interner = ThreadedRodeo::new();
        let pool = FrozenTypeInternPool::new();
        let mut fixture = FixtureCfg::new(
            Type::I32,
            0,
            "main",
            ParamSlotModes::new(vec![], vec![]),
            vec![],
            &pool,
            &interner,
        );
        let condition = fixture.value(CfgInstData::BoolConst(true), Type::BOOL);
        let then_arm = fixture.cfg.new_block();
        let else_arm = fixture.cfg.new_block();
        let join = fixture.cfg.new_block();
        let join_value = fixture.cfg.add_block_param(join, Type::I32);
        fixture
            .cfg
            .set_branch(fixture.current, condition, then_arm, [], else_arm, []);
        fixture.current = then_arm;
        let one = fixture.konst(1, Type::I32);
        fixture.cfg.set_goto(then_arm, join, [one]);
        fixture.current = else_arm;
        let two = fixture.konst(2, Type::I32);
        fixture.cfg.set_goto(else_arm, join, [two]);
        fixture.current = join;
        fixture.ret(Some(join_value));
        let mir = fixture.lower().expect("test lowering should succeed");
        assert!(!mir.instructions().is_empty());
    }

    #[test]
    fn default_string_literal_lowers_only_ptr_and_len() {
        // `let s = "hello"; @intCast(s.len())` — the literal is the two-slot
        // builtin `str { ptr, len }`, and `.len()` reads its second slot.
        let interner = ThreadedRodeo::new();
        let pool = TypeInternPool::new();
        let (str_id, _) = pool.register_struct(
            interner.get_or_intern("str"),
            StructDef {
                name: "str".into(),
                fields: vec![
                    StructField {
                        name: "ptr".to_string(),
                        ty: Type::U64,
                    },
                    StructField {
                        name: "len".to_string(),
                        ty: Type::U64,
                    },
                ],
                is_copy: true,
                is_linear: false,
                declared_linear: false,
                destructor: None,
                is_builtin: true,
                is_pub: false,
                file_id: FileId::DEFAULT,
            },
        );
        let str_ty = Type::new_struct(str_id);
        let pool = pool.freeze();
        let mut fixture = FixtureCfg::new(
            Type::I32,
            4,
            "main",
            ParamSlotModes::new(vec![], vec![]),
            vec![],
            &pool,
            &interner,
        );
        fixture.live(0, str_ty);
        let literal = fixture.value(CfgInstData::StringConst(0), str_ty);
        fixture.alloc(0, literal);
        fixture.live(2, str_ty);
        let s = fixture.load(0, str_ty);
        fixture.alloc(2, s);
        let len = fixture.place_read(
            PlaceBase::Local(2),
            str_ty,
            [Projection::Field {
                struct_id: str_id,
                field_index: 1,
            }],
            Type::U64,
        );
        fixture.dead(2, str_ty);
        let result = fixture.cast(len, Type::U64, Type::I32);
        fixture.dead(0, str_ty);
        fixture.ret(Some(result));
        let mir = fixture.lower().expect("test lowering should succeed");
        assert!(
            mir.instructions()
                .iter()
                .any(|inst| matches!(inst, X86Inst::StringConstPtr { .. }))
        );
        assert!(
            mir.instructions()
                .iter()
                .any(|inst| matches!(inst, X86Inst::StringConstLen { .. }))
        );
        assert!(
            !mir.instructions()
                .iter()
                .any(|inst| matches!(inst, X86Inst::StringConstCap { .. }))
        );
    }

    #[test]
    fn strbuf_style_boolean_branch_zero_extends_x86_setcc_result() {
        // `struct StrBuf { cap: u64 }`:
        // `let s = StrBuf { cap: 1 }; if s.cap > 0 { 0 } else { 101 }`.
        let interner = ThreadedRodeo::new();
        let pool = TypeInternPool::new();
        let strbuf_id = register_struct(&pool, &interner, "StrBuf", &[("cap", Type::U64)]);
        let strbuf_ty = Type::new_struct(strbuf_id);
        let pool = pool.freeze();
        let mut fixture = FixtureCfg::new(
            Type::I32,
            1,
            "main",
            ParamSlotModes::new(vec![], vec![]),
            vec![],
            &pool,
            &interner,
        );
        fixture.live(0, strbuf_ty);
        let cap = fixture.konst(1, Type::U64);
        let strbuf = fixture.struct_init(strbuf_id, &[cap], strbuf_ty);
        fixture.alloc(0, strbuf);
        let cap = fixture.place_read(
            PlaceBase::Local(0),
            strbuf_ty,
            [Projection::Field {
                struct_id: strbuf_id,
                field_index: 0,
            }],
            Type::U64,
        );
        let zero = fixture.konst(0, Type::U64);
        let condition = fixture.value(CfgInstData::Gt(cap, zero), Type::BOOL);
        let then_arm = fixture.cfg.new_block();
        let else_arm = fixture.cfg.new_block();
        let join = fixture.cfg.new_block();
        let join_value = fixture.cfg.add_block_param(join, Type::I32);
        fixture
            .cfg
            .set_branch(fixture.current, condition, then_arm, [], else_arm, []);
        fixture.current = then_arm;
        let ok = fixture.konst(0, Type::I32);
        fixture.cfg.set_goto(then_arm, join, [ok]);
        fixture.current = else_arm;
        let fail = fixture.konst(101, Type::I32);
        fixture.cfg.set_goto(else_arm, join, [fail]);
        fixture.current = join;
        fixture.dead(0, strbuf_ty);
        fixture.ret(Some(join_value));
        let mir = fixture.lower().expect("test lowering should succeed");

        let mut saw_setcc = false;
        for pair in mir.instructions().windows(2) {
            let Some(dst) = (match &pair[0] {
                X86Inst::Sete { dst }
                | X86Inst::Setne { dst }
                | X86Inst::Setl { dst }
                | X86Inst::Setg { dst }
                | X86Inst::Setle { dst }
                | X86Inst::Setge { dst }
                | X86Inst::Setb { dst }
                | X86Inst::Seta { dst }
                | X86Inst::Setbe { dst }
                | X86Inst::Setae { dst } => Some(*dst),
                _ => None,
            }) else {
                continue;
            };
            saw_setcc = true;
            assert!(matches!(
                pair[1],
                X86Inst::Movzx {
                    dst: next_dst,
                    src: next_src,
                } if next_dst == dst && next_src == dst
            ));
        }
        assert!(saw_setcc, "regression test must lower at least one setcc");
    }

    /// The raw-byte fixture CFG:
    ///
    /// ```text
    /// let p = @alloc(3, 1);                    // slot 0
    /// @ptr_write(@ptr_offset(p, 1), 255);
    /// let q = @realloc(p, 3, 1, 5);            // slot 1
    /// let b = @ptr_read(@ptr_offset(q, 1));    // slot 2
    /// @free(q, 5, 1);
    /// @intCast(b)
    /// ```
    fn raw_bytes_fixture<'a>(
        pool: &'a FrozenTypeInternPool,
        interner: &'a ThreadedRodeo,
        ptr_ty: Type,
    ) -> FixtureCfg<'a> {
        let mut fixture = FixtureCfg::new(
            Type::I32,
            3,
            "main",
            ParamSlotModes::new(vec![], vec![]),
            vec![],
            pool,
            interner,
        );
        fixture.live(0, ptr_ty);
        let size = fixture.konst(3, Type::U64);
        let align = fixture.konst(1, Type::U64);
        let p = fixture.intrinsic(
            rue_air::IntrinsicOperation::Alloc,
            "alloc",
            &[size, align],
            ptr_ty,
        );
        fixture.alloc(0, p);
        let p = fixture.load(0, ptr_ty);
        let one = fixture.konst(1, Type::I32);
        let offset = fixture.intrinsic(
            rue_air::IntrinsicOperation::PtrOffset,
            "ptr_offset",
            &[p, one],
            ptr_ty,
        );
        let byte = fixture.konst(255, Type::U8);
        fixture.intrinsic(
            rue_air::IntrinsicOperation::PtrWrite,
            "ptr_write",
            &[offset, byte],
            Type::UNIT,
        );
        fixture.unit();
        fixture.live(1, ptr_ty);
        let p = fixture.load(0, ptr_ty);
        let old_size = fixture.konst(3, Type::U64);
        let old_align = fixture.konst(1, Type::U64);
        let new_size = fixture.konst(5, Type::U64);
        let q = fixture.intrinsic(
            rue_air::IntrinsicOperation::Realloc,
            "realloc",
            &[p, old_size, old_align, new_size],
            ptr_ty,
        );
        fixture.alloc(1, q);
        fixture.live(2, Type::U8);
        let q = fixture.load(1, ptr_ty);
        let one = fixture.konst(1, Type::I32);
        let offset = fixture.intrinsic(
            rue_air::IntrinsicOperation::PtrOffset,
            "ptr_offset",
            &[q, one],
            ptr_ty,
        );
        let byte = fixture.intrinsic(
            rue_air::IntrinsicOperation::PtrRead,
            "ptr_read",
            &[offset],
            Type::U8,
        );
        fixture.alloc(2, byte);
        let q = fixture.load(1, ptr_ty);
        let size = fixture.konst(5, Type::U64);
        let align = fixture.konst(1, Type::U64);
        fixture.intrinsic(
            rue_air::IntrinsicOperation::Free,
            "free",
            &[q, size, align],
            Type::UNIT,
        );
        fixture.unit();
        let byte = fixture.load(2, Type::U8);
        let result = fixture.cast(byte, Type::U8, Type::I32);
        fixture.dead(2, Type::U8);
        fixture.dead(1, ptr_ty);
        fixture.dead(0, ptr_ty);
        fixture.ret(Some(result));
        fixture
    }

    #[test]
    fn raw_bytes_runtime_helper_identity_and_slots_match_shared_plan() {
        let interner = ThreadedRodeo::new();
        let pool = TypeInternPool::new();
        let ptr_ty = Type::new_ptr_mut(pool.intern_ptr_mut_from_type(Type::U8));
        let pool = pool.freeze();
        let mir = raw_bytes_fixture(&pool, &interner, ptr_ty)
            .lower()
            .expect("test lowering should succeed");
        assert!(
            mir.instructions()
                .iter()
                .any(|inst| matches!(inst, X86Inst::NarrowStoreIndexed { width: 1, .. }))
        );
        assert!(
            mir.instructions()
                .iter()
                .any(|inst| matches!(inst, X86Inst::NarrowLoadIndexed { width: 1, .. }))
        );

        let alloc = runtime_call_index(&mir, rue_runtime_abi::RuntimeHelperId::Alloc);
        assert_eq!(immediate_call_arg(&mir, alloc, Reg::Rdi), Some(3));
        assert_eq!(immediate_call_arg(&mir, alloc, Reg::Rsi), Some(1));

        let realloc = runtime_call_index(&mir, rue_runtime_abi::RuntimeHelperId::Realloc);
        assert_eq!(immediate_call_arg(&mir, realloc, Reg::Rsi), Some(3));
        assert_eq!(immediate_call_arg(&mir, realloc, Reg::Rdx), Some(5));
        assert_eq!(immediate_call_arg(&mir, realloc, Reg::Rcx), Some(1));

        let free = runtime_call_index(&mir, rue_runtime_abi::RuntimeHelperId::Free);
        assert_eq!(immediate_call_arg(&mir, free, Reg::Rsi), Some(5));
        assert_eq!(immediate_call_arg(&mir, free, Reg::Rdx), Some(1));
    }

    /// RUE-978: the raw-byte access family folds into the ordinary typed
    /// `ptr u8` narrow path — a `u8` `@ptr_read`/`@ptr_write` emits a one-byte
    /// `NarrowLoadIndexed`/`NarrowStoreIndexed` a typed `@ptr_read`/`@ptr_write`
    /// of a `u8` pointee does.
    #[test]
    fn aggregate_layout_folds_byte_access_into_narrow_typed_path() {
        // `let p = @alloc(2, 1); @ptr_write(@ptr_offset(p, 0), 65);
        //  let b = @ptr_read(@ptr_offset(p, 1)); @free(p, 2, 1); @intCast(b)`
        let interner = ThreadedRodeo::new();
        let pool = TypeInternPool::new();
        let ptr_ty = Type::new_ptr_mut(pool.intern_ptr_mut_from_type(Type::U8));
        let pool = pool.freeze();
        let mut fixture = FixtureCfg::new(
            Type::I32,
            2,
            "main",
            ParamSlotModes::new(vec![], vec![]),
            vec![],
            &pool,
            &interner,
        );
        fixture.live(0, ptr_ty);
        let size = fixture.konst(2, Type::U64);
        let align = fixture.konst(1, Type::U64);
        let p = fixture.intrinsic(
            rue_air::IntrinsicOperation::Alloc,
            "alloc",
            &[size, align],
            ptr_ty,
        );
        fixture.alloc(0, p);
        let p = fixture.load(0, ptr_ty);
        let zero = fixture.konst(0, Type::I32);
        let offset = fixture.intrinsic(
            rue_air::IntrinsicOperation::PtrOffset,
            "ptr_offset",
            &[p, zero],
            ptr_ty,
        );
        let byte = fixture.konst(65, Type::U8);
        fixture.intrinsic(
            rue_air::IntrinsicOperation::PtrWrite,
            "ptr_write",
            &[offset, byte],
            Type::UNIT,
        );
        fixture.unit();
        fixture.live(1, Type::U8);
        let p = fixture.load(0, ptr_ty);
        let one = fixture.konst(1, Type::I32);
        let offset = fixture.intrinsic(
            rue_air::IntrinsicOperation::PtrOffset,
            "ptr_offset",
            &[p, one],
            ptr_ty,
        );
        let byte = fixture.intrinsic(
            rue_air::IntrinsicOperation::PtrRead,
            "ptr_read",
            &[offset],
            Type::U8,
        );
        fixture.alloc(1, byte);
        let p = fixture.load(0, ptr_ty);
        let size = fixture.konst(2, Type::U64);
        let align = fixture.konst(1, Type::U64);
        fixture.intrinsic(
            rue_air::IntrinsicOperation::Free,
            "free",
            &[p, size, align],
            Type::UNIT,
        );
        fixture.unit();
        let byte = fixture.load(1, Type::U8);
        let result = fixture.cast(byte, Type::U8, Type::I32);
        fixture.dead(1, Type::U8);
        fixture.dead(0, ptr_ty);
        fixture.ret(Some(result));
        let mir = fixture.lower().expect("test lowering should succeed");
        assert!(
            mir.instructions()
                .iter()
                .any(|inst| matches!(inst, X86Inst::NarrowStoreIndexed { width: 1, .. })),
            "a `u8` @ptr_write must use the one-byte narrow store"
        );
        assert!(
            mir.instructions().iter().any(|inst| matches!(
                inst,
                X86Inst::NarrowLoadIndexed {
                    width: 1,
                    signed: false,
                    ..
                }
            )),
            "a `u8` @ptr_read must use the one-byte zero-extended narrow load"
        );
    }

    #[test]
    fn sysv_stacks_a_narrow_tail_in_whole_slots_unaffected_by_the_apple_amendment() {
        use crate::foreign_call::{
            AggregateImage, ForeignArg, ForeignCallInputs, ForeignCallLoweringBackend,
            ForeignCallPlan, ForeignReturn,
        };
        // The same narrow `i8`/`i16`/`i32` tail Apple packs on AArch64: SysV has
        // no natural-size amendment, so each stacked argument keeps a whole
        // 8-byte slot whatever its C width, and the push-based sequence stands.
        use rue_air::{CAbiScalarKind, CAbiTypeFacts};
        let mut args = vec![ForeignArg::Aggregate {
            value: CfgValue::from_raw(0),
            image: AggregateImage {
                map: Vec::new(),
                padding: Vec::new(),
                slot_count: 0,
                size: 8,
                align: 8,
                storage_bytes: 16,
                leaves: rue_air::AggregateLeaves::all_integer(8),
            },
        }];
        args.extend((1..6).map(|index| ForeignArg::Scalar {
            value: CfgValue::from_raw(index),
            kind: CAbiScalarKind::RegisterWidth,
        }));
        for (index, kind) in [
            (6, CAbiScalarKind::I8),
            (7, CAbiScalarKind::I16),
            (8, CAbiScalarKind::I32),
        ] {
            args.push(ForeignArg::Scalar {
                value: CfgValue::from_raw(index),
                kind,
            });
        }
        let plan = ForeignCallPlan::new(ForeignCallInputs::new(
            "narrow_tail".into(),
            x86_64_target_c_convention(),
            args,
            ForeignReturn::ZeroSized,
            CAbiTypeFacts::ZeroSized,
        ));
        let area = plan.stack_area_for_test(&[VReg::new(80), VReg::new(81), VReg::new(82)]);
        assert_eq!(area.bytes, 32);
        assert_eq!(
            area.stores
                .iter()
                .map(|store| (store.offset, store.bytes))
                .collect::<Vec<_>>(),
            vec![(0, 8), (8, 8), (16, 8)]
        );

        let interner = ThreadedRodeo::new();
        let pool = FrozenTypeInternPool::new();
        let mut fixture = FixtureCfg::new(
            Type::UNIT,
            0,
            "f",
            ParamSlotModes::new(vec![], vec![]),
            vec![],
            &pool,
            &interner,
        );
        fixture.ret(None);
        let cfg = fixture.cfg.finish(&pool).expect("test CFG must verify");
        let mut lower = CfgLower::new(&cfg, &pool, &interner);
        lower.foreign_emit_stack_args(&area);
        // Three whole slots plus one cell of call-alignment padding, pushed in
        // reverse so the first store lands nearest `rsp`.
        assert_eq!(
            lower
                .mir
                .instructions()
                .iter()
                .filter(|inst| matches!(inst, X86Inst::Push { .. }))
                .count(),
            3
        );
        assert!(lower.mir.instructions().iter().any(|inst| matches!(
            inst,
            X86Inst::AddRI {
                dst: Operand::Physical(Reg::Rsp),
                imm: -8
            }
        )));
    }
}
