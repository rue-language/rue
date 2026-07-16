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
use rue_air::{FrozenTypeInternPool, TypeKind};
use rue_cfg::{BlockId, Cfg, CfgValue, Type};
use rue_error::CompileResult;

use super::mir::{LabelId, Operand, Reg, VReg, X86Inst, X86Mir};
use crate::agg_slots::SlotBackend;
use crate::allocation;
use crate::cfg_lower::CfgLowerContext;
use crate::vreg::BLOCK_LABEL_BASE;

/// Argument passing registers per System V AMD64 ABI. ABI arg slots beyond
/// these are passed on the caller's stack (slot `k >= 6` at `[rbp+16+(k-6)*8]`
/// in the callee); the callee prologue copies them into its frame param area
/// so the body addresses every param slot uniformly (see `emit_prologue`).
pub(super) const ARG_REGS: [Reg; 6] = [Reg::Rdi, Reg::Rsi, Reg::Rdx, Reg::Rcx, Reg::R8, Reg::R9];

/// Return value registers for the internal Rue convention (SysV only defines
/// rax/rdx; the rest are caller-saved scratch regs we extend the convention
/// with for multi-slot aggregate returns). Aggregates with more slots than
/// this — and builtin String always — return via sret instead; see
/// `crate::cfg_lower::type_uses_sret_return`. (RUE-106)
pub(super) const RET_REGS: [Reg; 6] = [Reg::Rax, Reg::Rdx, Reg::Rcx, Reg::R8, Reg::R9, Reg::R10];

/// CFG to X86Mir lowering.
pub struct CfgLower<'a> {
    /// Shared context with type helpers and chain tracing.
    ctx: CfgLowerContext<'a>,
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
    /// Function name (needed to detect main function)
    fn_name: &'a str,
    /// Maps StructInit CFG values to their field vregs
    struct_slot_vregs: HashMap<CfgValue, Vec<VReg>>,
    /// Maps by-reference parameter indices to their pointer vregs.
    /// For by-ref params, the slot contains a pointer to the caller's memory.
    /// This map stores the vreg holding that pointer so Store can use it.
    by_ref_param_ptrs: HashMap<u32, VReg>,
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
        self.mir.push(X86Inst::AddRI {
            dst: Operand::Physical(Reg::Rsp),
            imm: -(storage_bytes as i32),
        });
        let pointer = self.mir.alloc_vreg();
        self.mir.push(X86Inst::MovRR {
            dst: Operand::Virtual(pointer),
            src: Operand::Physical(Reg::Rsp),
        });
        pointer
    }
}

impl<'a> CfgLower<'a> {
    /// Create a new CFG lowering pass.
    pub fn new(
        cfg: &'a Cfg,
        type_pool: &'a FrozenTypeInternPool,
        interner: &'a ThreadedRodeo,
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
            mir: X86Mir::new(),
            value_map: HashMap::with_capacity(num_values),
            block_param_vregs: HashMap::with_capacity(estimated_block_params),
            next_label: 0,
            fn_name: cfg.fn_name(),
            struct_slot_vregs: HashMap::with_capacity(estimated_struct_inits),
            by_ref_param_ptrs: HashMap::with_capacity(estimated_by_ref_params),
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
            "runtime manifest exceeds the x86-64 target-C register budget"
        );

        let out_shape = plan.out_shape();
        let out_bytes = out_shape
            .map(|shape| (shape.shape().slots.len() as i32 * 8 + 15) & !15)
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

        let symbol_id = self.intern_symbol(plan.symbol());
        self.mir.push(X86Inst::CallRel { symbol_id });

        match plan.result() {
            RuntimeCallResult::OutPointer(shape) => {
                let slots = (0..shape.shape().slots.len())
                    .map(|index| {
                        let slot = self.mir.alloc_vreg();
                        self.mir.push(X86Inst::MovRM {
                            dst: Operand::Virtual(slot),
                            base: Reg::Rsp,
                            offset: (index * 8) as i32,
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

        // Load the pointer from the param slot. The prologue copies every ABI
        // arg slot — register- and stack-passed alike — into the contiguous
        // frame param area, so this is uniform regardless of param count.
        let ptr_vreg = self.mir.alloc_vreg();
        let slot = self.ctx.num_locals + param_slot;
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

    /// Materialize every by-reference parameter pointer before CFG control
    /// flow begins, so the function-wide cache only contains definitions that
    /// dominate every block which may reuse them.
    fn preload_by_ref_param_ptrs(&mut self) {
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
        let ctx = self.ctx;
        crate::terminator_plan::lower_cfg(&ctx, &mut self, None, RET_REGS.len() as u32);
        Ok(self.mir)
    }

    /// Lower CFG to X86Mir with debug information about instruction selection.
    ///
    /// This is like `lower()` but also captures detailed information about
    /// how each CFG instruction maps to MIR instructions.
    pub fn lower_with_debug(mut self) -> CompileResult<(X86Mir, crate::LoweringDebugInfo)> {
        let mut debug_info = crate::LoweringDebugInfo {
            fn_name: self.fn_name.to_string(),
            target_arch: "x86_64".to_string(),
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
        let vreg = match plan.operation {
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
                self.emit_overflow_check(width, vreg, overflow_call.clone());
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
                self.emit_overflow_check(width, vreg, overflow_call.clone());
                vreg
            }
            ArithmeticOperation::Mul {
                lhs,
                rhs,
                width,
                shift,
            } => {
                let vreg = self.mir.alloc_vreg();
                if let Some((src, amount)) = shift.filter(|_| width.bits >= 32) {
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
        let num_reg_args = plan.abi_slots.len().min(ARG_REGS.len());
        let num_stack_args = plan.stack_slot_count;
        let needs_alignment = plan.stack_bytes > (num_stack_args * 8) as u32;
        if needs_alignment {
            self.mir.push(X86Inst::AddRI {
                dst: Operand::Physical(Reg::Rsp),
                imm: -8,
            });
        }
        for arg in plan.abi_slots.iter().skip(ARG_REGS.len()).rev() {
            self.mir.push(X86Inst::MovRR {
                dst: Operand::Physical(Reg::Rax),
                src: Operand::Virtual(*arg),
            });
            self.mir.push(X86Inst::Push {
                src: Operand::Physical(Reg::Rax),
            });
        }
        for arg in plan.abi_slots.iter().take(num_reg_args).rev() {
            self.mir.push(X86Inst::MovRR {
                dst: Operand::Physical(Reg::Rax),
                src: Operand::Virtual(*arg),
            });
            self.mir.push(X86Inst::Push {
                src: Operand::Physical(Reg::Rax),
            });
        }
        for (index, _) in plan.abi_slots.iter().take(num_reg_args).enumerate() {
            self.mir.push(X86Inst::Pop {
                dst: Operand::Physical(ARG_REGS[index]),
            });
        }
        let symbol_id = self.intern_symbol(plan.target.symbol());
        self.mir.push(X86Inst::CallRel { symbol_id });
        if num_stack_args > 0 || needs_alignment {
            self.mir.push(X86Inst::AddRI {
                dst: Operand::Physical(Reg::Rsp),
                imm: plan.stack_bytes as i32,
            });
        }
        let slots = match plan.return_plan {
            ReturnPlan::Sret {
                slot_count,
                storage_bytes,
            } => {
                let slots: Vec<_> = (0..slot_count)
                    .map(|index| {
                        let slot = self.mir.alloc_vreg();
                        self.mir.push(X86Inst::MovRM {
                            dst: Operand::Virtual(slot),
                            base: Reg::Rsp,
                            offset: (index * 8) as i32,
                        });
                        slot
                    })
                    .collect();
                self.mir.push(X86Inst::AddRI {
                    dst: Operand::Physical(Reg::Rsp),
                    imm: storage_bytes as i32,
                });
                slots
            }
            ReturnPlan::Registers { slot_count } => (0..slot_count)
                .map(|index| {
                    let slot = self.mir.alloc_vreg();
                    self.mir.push(X86Inst::MovRR {
                        dst: Operand::Virtual(slot),
                        src: Operand::Physical(RET_REGS[index as usize]),
                    });
                    slot
                })
                .collect(),
            ReturnPlan::Scalar => Vec::new(),
            ReturnPlan::ZeroSized => Vec::new(),
        };
        if let Some(&slot) = slots.first() {
            self.mir.push(X86Inst::MovRR {
                dst: Operand::Virtual(primary),
                src: Operand::Virtual(slot),
            });
        } else if matches!(plan.return_plan, ReturnPlan::Scalar) {
            self.mir.push(X86Inst::MovRR {
                dst: Operand::Virtual(primary),
                src: Operand::Physical(Reg::Rax),
            });
        } else if matches!(plan.return_plan, ReturnPlan::ZeroSized) {
            self.mir.push(X86Inst::MovRI32 {
                dst: Operand::Virtual(primary),
                imm: 0,
            });
        }
        crate::value_plan::MaterializedValue { primary, slots }
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
                slots = vec![ptr, len];
                if plan.policy.shape.slot_count() >= 3 {
                    let cap = self.mir.alloc_vreg();
                    self.mir.push(X86Inst::StringConstCap {
                        dst: Operand::Virtual(cap),
                        string_id,
                    });
                    slots.push(cap);
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
                let ptr = self.ensure_by_ref_param_ptr(param_slot);
                if value.slots.is_empty() {
                    self.emit_store_ptr_base(value.primary, ptr);
                } else {
                    crate::agg_slots::store_slots_through_ptr(self, &value.slots, ptr, 0);
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
                    self.mir.push(X86Inst::MovRI32 {
                        dst: Operand::Virtual(dst),
                        imm: 0,
                    });
                } else if let Some(&first) = slots.first() {
                    self.mir.push(X86Inst::MovRR {
                        dst: Operand::Virtual(dst),
                        src: Operand::Virtual(first),
                    });
                } else {
                    self.mir.push(X86Inst::MovRI32 {
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
                ..
            } => {
                slots = (0..total_slots).map(|_| self.mir.alloc_vreg()).collect();
                self.mir.push(X86Inst::MovRI32 {
                    dst: Operand::Virtual(slots[0]),
                    imm: variant_index as i32,
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
                            self.mir.push(X86Inst::MovRR {
                                dst: Operand::Virtual(slots[offset]),
                                src: Operand::Virtual(value),
                            });
                            offset += 1;
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
            } => {
                slots.clear();
                for index in 0..field_slots {
                    let dst = self.mir.alloc_vreg();
                    self.mir.push(X86Inst::MovRR {
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
        let dst = self.mir.alloc_vreg();
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
            self.mir.push(X86Inst::MovRMIndexed {
                dst: Operand::Virtual(dst),
                base: ptr,
                offset: 0,
            });
        } else if count > 1 {
            let slots: Vec<_> = (0..count)
                .map(|slot| {
                    let v = self.mir.alloc_vreg();
                    let frame_slot = self.ctx.num_locals + index + count - 1 - slot;
                    self.mir.push(X86Inst::MovRM {
                        dst: Operand::Virtual(v),
                        base: Reg::Rbp,
                        offset: self.ctx.local_offset(frame_slot),
                    });
                    v
                })
                .collect();
            return (slots[0], slots);
        } else {
            self.mir.push(X86Inst::MovRM {
                dst: Operand::Virtual(dst),
                base: Reg::Rbp,
                offset: self.ctx.local_offset(self.ctx.num_locals + index),
            });
        }
        let _ = ty;
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
        leaf_types: &[Type],
        runtime_call: Option<crate::runtime_call_plan::RuntimeCallPlan>,
    ) -> VReg {
        match policy.comparison.expect("comparison plan") {
            crate::value_plan::ComparisonPreparation::Scalar { .. } => {
                self.lower_scalar_comparison(op, lhs.primary, rhs.primary, policy)
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
            crate::value_plan::IntrinsicOperation::PtrToInt
            | crate::value_plan::IntrinsicOperation::IntToPtr => {
                let dst = self.mir.alloc_vreg();
                self.mir.push(X86Inst::MovRR {
                    dst: Operand::Virtual(dst),
                    src: Operand::Virtual(plan.args[0].primary),
                });
                dst
            }
            crate::value_plan::IntrinsicOperation::PtrRead => {
                let ptr = plan.args[0].primary;
                let count = plan.result_slots;
                if count > 1 {
                    slots = crate::agg_slots::load_slots_through_ptr(self, ptr, count);
                    slots[0]
                } else {
                    let dst = self.mir.alloc_vreg();
                    if count != 0 {
                        self.mir.push(X86Inst::MovRMIndexed {
                            dst: Operand::Virtual(dst),
                            base: ptr,
                            offset: 0,
                        });
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
                if value.slot_count == 0 {
                    // Zero-sized values have no bytes to write.
                } else if !value.slots.is_empty() {
                    crate::agg_slots::store_slots_through_ptr(self, &value.slots, ptr, 0);
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
            crate::value_plan::IntrinsicOperation::PtrOffset => {
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
            crate::value_plan::IntrinsicOperation::Alloc { element_size } => {
                let _ = element_size;
                self.lower_runtime_call(plan.runtime_call.expect("alloc runtime call plan"))
                    .primary
            }
            crate::value_plan::IntrinsicOperation::Free { element_size } => {
                let _ = element_size;
                self.lower_runtime_call(plan.runtime_call.expect("free runtime call plan"))
                    .primary
            }
            crate::value_plan::IntrinsicOperation::Realloc { element_size } => {
                let _ = element_size;
                self.lower_runtime_call(plan.runtime_call.expect("realloc runtime call plan"))
                    .primary
            }
            crate::value_plan::IntrinsicOperation::AllocBytes => {
                self.lower_runtime_call(plan.runtime_call.expect("alloc-bytes runtime call plan"))
                    .primary
            }
            crate::value_plan::IntrinsicOperation::FreeBytes => {
                self.lower_runtime_call(plan.runtime_call.expect("free-bytes runtime call plan"))
                    .primary
            }
            crate::value_plan::IntrinsicOperation::ReallocBytes => {
                self.lower_runtime_call(plan.runtime_call.expect("realloc-bytes runtime call plan"))
                    .primary
            }
            crate::value_plan::IntrinsicOperation::ByteRead => {
                let addr = self.mir.alloc_vreg();
                self.mir.push(X86Inst::MovRR {
                    dst: Operand::Virtual(addr),
                    src: Operand::Virtual(plan.args[0].primary),
                });
                self.mir.push(X86Inst::AddRR64 {
                    dst: Operand::Virtual(addr),
                    src: Operand::Virtual(plan.args[1].primary),
                });
                let dst = self.mir.alloc_vreg();
                self.mir.push(X86Inst::Movzx8RMIndexed {
                    dst: Operand::Virtual(dst),
                    base: addr,
                    offset: 0,
                });
                dst
            }
            crate::value_plan::IntrinsicOperation::ByteWrite => {
                let addr = self.mir.alloc_vreg();
                self.mir.push(X86Inst::MovRR {
                    dst: Operand::Virtual(addr),
                    src: Operand::Virtual(plan.args[0].primary),
                });
                self.mir.push(X86Inst::AddRR64 {
                    dst: Operand::Virtual(addr),
                    src: Operand::Virtual(plan.args[1].primary),
                });
                self.mir.push(X86Inst::MovMR8Indexed {
                    base: addr,
                    offset: 0,
                    src: Operand::Virtual(plan.args[2].primary),
                });
                let dst = self.mir.alloc_vreg();
                self.mir.push(X86Inst::MovRI32 {
                    dst: Operand::Virtual(dst),
                    imm: 0,
                });
                dst
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
                    ReturnValuePlan::Scalar { value } => {
                        self.mir.push(X86Inst::MovRR {
                            dst: Operand::Physical(Reg::Rax),
                            src: Operand::Virtual(value),
                        });
                        self.mir.push(X86Inst::Ret);
                    }
                    ReturnValuePlan::Aggregate { slots, return_plan } => {
                        if return_plan.uses_sret() {
                            crate::agg_slots::store_slots_to_sret(self, &slots);
                        } else {
                            for (index, slot) in slots.iter().enumerate().rev() {
                                if index < RET_REGS.len() {
                                    self.mir.push(X86Inst::MovRR {
                                        dst: Operand::Physical(RET_REGS[index]),
                                        src: Operand::Virtual(*slot),
                                    });
                                }
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
            self.mir.push(X86Inst::MovRR {
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
    }

    fn prepare_block_param(&mut self, block: BlockId, index: u32, value: CfgValue, ty: Type) {
        let vreg = self.mir.alloc_vreg();
        self.block_param_vregs.insert((block, index), vreg);
        self.value_map.insert(value, vreg);
        crate::agg_slots::preallocate_block_param_slots(self, value, ty, vreg);
    }

    fn value_description(&self, value: CfgValue) -> String {
        let inst = self.ctx.cfg.get_inst(value);
        crate::format_cfg_inst_data_with_interner(self.ctx.cfg, &inst.data, self.interner)
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
    fn resolve_symbol(&self, symbol: lasso::Spur) -> String {
        self.interner.resolve(&symbol).to_owned()
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
        self.mir.push(X86Inst::MovRM {
            dst: Operand::Virtual(dst),
            base: Reg::Rbp,
            offset,
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
        self.mir.push(X86Inst::MovMR {
            base: Reg::Rbp,
            offset,
            src: Operand::Virtual(src),
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

    fn emit_load_ptr_base(&mut self, dst: VReg, ptr: VReg) {
        self.mir.push(X86Inst::MovRMIndexed {
            dst: Operand::Virtual(dst),
            base: ptr,
            offset: 0,
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
    fn emit_slot_eq(&mut self, dst: VReg, lhs: VReg, rhs: VReg, wide: bool) {
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
    use rue_air::Sema;
    use rue_cfg::CfgBuilder;
    use rue_error::{PreviewFeature, PreviewFeatures};
    use rue_lexer::Lexer;
    use rue_parser::Parser;
    use rue_rir::AstGen;

    fn lower_to_mir(source: &str) -> X86Mir {
        lower_to_mir_with_preview(source, PreviewFeatures::new())
    }

    fn lower_to_mir_with_preview(source: &str, preview: PreviewFeatures) -> X86Mir {
        let lexer = Lexer::new(source);
        let (tokens, interner) = lexer.tokenize().unwrap();
        let parser = Parser::new(tokens, interner);
        let (ast, mut interner) = parser.parse().unwrap();

        let mut astgen = AstGen::with_symbol_normalizer(&interner, |symbol| symbol);
        astgen.append_items(&ast.items);
        let rir = astgen.finish();

        let sema = Sema::new(&rir, &mut interner, preview);
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
            func.allow_unreachable_code,
        );

        CfgLower::new(&cfg_output.cfg, type_pool, &interner)
            .lower()
            .expect("test lowering should succeed")
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
                matches!(inst, X86Inst::CallRel { symbol_id } if mir.get_symbol(*symbol_id) == helper.symbol())
            })
            .unwrap_or_else(|| panic!("missing call to {helper:?}"))
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
        let mir = lower_to_mir_with_preview(
            "fn main() -> i32 { let s = \"hello\"; @intCast(s.len()) }",
            preview,
        );
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
        let mir = lower_to_mir(
            "struct StrBuf { cap: u64 }\n\
             fn main() -> i32 {\n\
                 let s = StrBuf { cap: 1 };\n\
                 if s.cap > 0 { 0 } else { 101 }\n\
             }",
        );

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

    #[test]
    fn raw_bytes_runtime_helper_identity_and_slots_match_shared_plan() {
        let mut preview = PreviewFeatures::new();
        preview.insert(PreviewFeature::RawBytes);
        let mir = lower_to_mir_with_preview(
            "fn main() -> i32 { checked { let p = @alloc_bytes(3); \
             @byte_write(p, 1, 255); let q = @realloc_bytes(p, 3, 5); \
             let b = @byte_read(q, 1); @free_bytes(q, 5); \
             @intCast(b) } }",
            preview,
        );
        assert!(
            mir.instructions()
                .iter()
                .any(|inst| matches!(inst, X86Inst::MovMR8Indexed { .. }))
        );
        assert!(
            mir.instructions()
                .iter()
                .any(|inst| matches!(inst, X86Inst::Movzx8RMIndexed { .. }))
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
}
