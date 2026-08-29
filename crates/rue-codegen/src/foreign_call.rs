//! Target-independent planning for `extern "C"` foreign calls (ADR-0064 P3).
//!
//! P2 crossed only scalars and pointers, which occupy one integer register each
//! and reuse the native call plan with a boundary return extension. P3 adds
//! **C-classifiable aggregates**, whose target-C crossing disagrees with the
//! native slot model — the native planner decomposes an aggregate one slot per
//! leaf and *reverses* the slots (the register-packing audit's key finding),
//! whereas C packs fields into eightbytes in ascending memory order. So a foreign
//! call that touches an aggregate is planned here, entirely separate from the
//! native `CallPlan`, and lowered through each aggregate's **physical memory
//! image** (which, for a `@repr(c)` struct of scalar/pointer fields, is exactly
//! the C object layout under the compact-layout default).
//!
//! This module classifies each argument and return with the target-C authority,
//! carries the aggregate's compact image map, and drives the shared
//! register/stack/sret event sequence. The two backends implement only the
//! physical register, stack, instruction, and image-operation leaves, so
//! neither can grow a second ABI sequence.

use rue_air::layout::PaddingRange;
use rue_air::{
    AggregateArgClass, AggregateReturnClass, FrozenTypeInternPool, ScalarAbiExtension,
    TargetCAbiFlavor, TargetCCallAbi, Type,
};
use rue_cfg::{Cfg, CfgCallArg, CfgValue};

use crate::frame_layout::{checked_aligned_region_bytes, checked_call_area_bytes};
use crate::types::{self, PhysicalEnumSlot};
use crate::vreg::VReg;

/// The compact physical memory image of an aggregate crossing the C boundary —
/// its C object layout under the compact-layout default. Every by-value
/// aggregate crossing (register-packed, byval-stack, by-reference, and every
/// aggregate return) is marshaled through this image, so C field order is
/// honored by construction and the native reversed-slot packing is never used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AggregateImage {
    /// Internal-slot → physical-byte map (the compact/C image). Writing an
    /// aggregate's native slots through this map lays out the C image; reading it
    /// back reconstructs the native slots.
    pub map: Vec<PhysicalEnumSlot>,
    /// Padding byte ranges zeroed before the field stores so the image buffer is
    /// deterministically initialized (ADR-0052 ruling 5).
    pub padding: Vec<PaddingRange>,
    /// Number of internal value-decomposition slots (native slot count).
    pub slot_count: u32,
    /// The aggregate's byte size (`@size_of`).
    pub size: u32,
    /// The aggregate's alignment (`@align_of`).
    pub align: u32,
    /// Backing-buffer size: `size` rounded up to the 16-byte call-stack
    /// alignment, so whole-eightbyte loads/stores never run past the buffer.
    pub storage_bytes: u32,
}

impl AggregateImage {
    fn for_type(type_pool: &FrozenTypeInternPool, ty: Type) -> Self {
        let map = types::aggregate_physical_slot_map(type_pool, ty).expect(
            "an FFI-eligible @repr(c) aggregate (no enums, variant-independent) must have a \
             single compact memory image; c_passable_by_value gated this before lowering",
        );
        let layout = type_pool.layout(ty);
        AggregateImage {
            map,
            padding: type_pool.compact_image_padding_ranges(ty),
            slot_count: types::type_slot_count(type_pool, ty),
            size: u32::try_from(layout.size).expect("foreign aggregate size must fit u32"),
            align: u32::try_from(layout.alignment)
                .expect("foreign aggregate alignment must fit u32"),
            storage_bytes: checked_aligned_region_bytes(layout.size)
                .expect("foreign aggregate storage must pass frame-budget preflight"),
        }
    }

    /// Number of eightbytes (8-byte integer registers / stack slots) the image
    /// spans: `ceil(size / 8)`.
    pub fn eightbytes(&self) -> u32 {
        self.size.div_ceil(8)
    }
}

/// How one foreign-call argument crosses the target-C boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ForeignArg {
    /// A scalar or pointer, already canonically extended in its vreg by Rue's
    /// internal invariant — occupies one integer register (or spills to the
    /// stack once the integer registers are exhausted).
    Scalar { value: CfgValue },
    /// A ≤16-byte aggregate packed into one or two integer registers in C field
    /// order ([`AggregateArgClass::IntegerRegisters`]).
    AggregateRegisters {
        value: CfgValue,
        image: AggregateImage,
    },
    /// A >16-byte aggregate passed by value in the outgoing stack argument area
    /// (SysV MEMORY class, [`AggregateArgClass::ByValueStack`]).
    AggregateByvalStack {
        value: CfgValue,
        image: AggregateImage,
    },
    /// A >16-byte aggregate passed as a pointer to a caller-owned copy (AAPCS64,
    /// [`AggregateArgClass::ByReferenceCopy`]).
    AggregateByRefCopy {
        value: CfgValue,
        image: AggregateImage,
    },
}

/// How a foreign call's return value crosses the target-C boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ForeignReturn {
    /// Unit / never / empty: no materialized value.
    ZeroSized,
    /// A scalar returned in the primary result register; `ext` re-extends it to
    /// Rue's canonical 64-bit form (a C callee leaves the high bits unspecified).
    Scalar { ext: ScalarAbiExtension },
    /// A ≤16-byte aggregate returned in one or two result registers (`rax:rdx` /
    /// `x0:x1`) in C field order; the backend bridges the registers through the
    /// image buffer to reconstruct the native slots.
    AggregateRegisters { image: AggregateImage },
    /// A >16-byte aggregate returned indirectly through caller storage (sret);
    /// the callee writes the C image, the backend reads the native slots back.
    AggregateSret { image: AggregateImage },
}

/// A classified foreign call: the C symbol, every argument and the return, plus
/// the psABI flavor whose classifier produced them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForeignCallInputs {
    /// The resolved (unmangled) C symbol the undefined reference targets.
    pub symbol: String,
    pub flavor: TargetCAbiFlavor,
    pub args: Vec<ForeignArg>,
    pub ret: ForeignReturn,
}

impl ForeignCallInputs {
    /// The C symbol this call targets.
    pub fn symbol_ref(&self) -> &str {
        &self.symbol
    }
}

/// The target-independent placement portion of foreign-call lowering.
///
/// Backends still own how a placed value is materialized and how the call
/// frame is adjusted (pushes on SysV versus stores on AAPCS64). They consume
/// this plan for the decisions which must be identical: aggregate pieces are
/// all-or-nothing with respect to the remaining integer registers, by-value
/// images consume their full eightbyte width, and by-reference copies consume
/// one pointer argument. Keeping those decisions here prevents the two
/// instruction-selection adapters from drifting while preserving their
/// target-specific MIR.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ForeignArgPlacement {
    Register { arg_index: usize },
    Stack { arg_index: usize },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ForeignArgShape {
    /// Number of integer eightbytes (or one pointer) consumed by the argument.
    pieces: u64,
    /// Storage retained for a by-reference argument copy.
    byref_bytes: u64,
    /// Whether this argument may use integer argument registers.
    can_register: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ForeignPlacementPlan {
    placements: Vec<ForeignArgPlacement>,
    byref_bytes: u64,
    stack_cells: u64,
    sret_in_argument_register: bool,
}

fn place_foreign_args(
    shapes: &[ForeignArgShape],
    int_register_budget: u64,
    sret_in_argument_register: bool,
) -> Result<ForeignPlacementPlan, crate::frame_layout::FrameBudgetExceeded> {
    let mut used_registers = u64::from(sret_in_argument_register);
    let mut placements = Vec::with_capacity(shapes.len());
    let mut byref_bytes = 0_u64;
    let mut stack_cells = 0_u64;

    for (arg_index, shape) in shapes.iter().enumerate() {
        byref_bytes = byref_bytes
            .checked_add(shape.byref_bytes)
            .ok_or(crate::frame_layout::FrameBudgetExceeded)?;
        let placement = if shape.can_register {
            let register_end = used_registers
                .checked_add(shape.pieces)
                .ok_or(crate::frame_layout::FrameBudgetExceeded)?;
            if register_end <= int_register_budget {
                used_registers = register_end;
                ForeignArgPlacement::Register { arg_index }
            } else {
                let stack_end = stack_cells
                    .checked_add(shape.pieces)
                    .ok_or(crate::frame_layout::FrameBudgetExceeded)?;
                stack_cells = stack_end;
                ForeignArgPlacement::Stack { arg_index }
            }
        } else {
            let stack_end = stack_cells
                .checked_add(shape.pieces)
                .ok_or(crate::frame_layout::FrameBudgetExceeded)?;
            stack_cells = stack_end;
            ForeignArgPlacement::Stack { arg_index }
        };
        placements.push(placement);
    }

    Ok(ForeignPlacementPlan {
        placements,
        byref_bytes,
        stack_cells,
        sret_in_argument_register,
    })
}

fn shape_from_foreign_arg(arg: &ForeignArg) -> ForeignArgShape {
    match arg {
        ForeignArg::Scalar { .. } => ForeignArgShape {
            pieces: 1,
            byref_bytes: 0,
            can_register: true,
        },
        ForeignArg::AggregateRegisters { image, .. } => ForeignArgShape {
            pieces: u64::from(image.eightbytes()),
            byref_bytes: 0,
            can_register: true,
        },
        ForeignArg::AggregateByvalStack { image, .. } => ForeignArgShape {
            pieces: u64::from(image.eightbytes()),
            byref_bytes: 0,
            can_register: false,
        },
        ForeignArg::AggregateByRefCopy { image, .. } => ForeignArgShape {
            pieces: 1,
            byref_bytes: u64::from(image.storage_bytes),
            can_register: true,
        },
    }
}

fn shape_from_cfg_arg(
    cfg: &Cfg,
    type_pool: &FrozenTypeInternPool,
    abi: TargetCCallAbi,
    arg: &CfgCallArg,
) -> Result<ForeignArgShape, crate::frame_layout::FrameBudgetExceeded> {
    let ty = cfg.get_inst(arg.value).ty;
    if !is_aggregate(ty) {
        return Ok(ForeignArgShape {
            pieces: 1,
            byref_bytes: 0,
            can_register: true,
        });
    }

    let layout = type_pool.layout(ty);
    let copy_bytes = || checked_aligned_region_bytes(layout.size).map(u64::from);
    Ok(
        match abi.classify_aggregate_arg(layout.size, layout.alignment) {
            AggregateArgClass::IntegerRegisters { eightbytes } => ForeignArgShape {
                pieces: u64::from(eightbytes),
                byref_bytes: 0,
                can_register: true,
            },
            AggregateArgClass::ByValueStack { .. } => ForeignArgShape {
                pieces: layout.size.div_ceil(8),
                byref_bytes: 0,
                can_register: false,
            },
            AggregateArgClass::ByReferenceCopy { .. } => ForeignArgShape {
                pieces: 1,
                byref_bytes: copy_bytes()?,
                can_register: true,
            },
        },
    )
}

impl ForeignArgPlacement {
    #[inline]
    pub const fn arg_index(self) -> usize {
        match self {
            Self::Register { arg_index } | Self::Stack { arg_index } => arg_index,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ForeignCallPlan {
    /// Classified call consumed by the backend adapters.
    inputs: ForeignCallInputs,
    /// Every user argument's placement, retaining source order for lowering
    /// side effects and deterministic virtual-register numbering.
    placements: Vec<ForeignArgPlacement>,
    /// Total storage reserved for AAPCS64 by-reference argument copies.
    byref_bytes: u32,
    /// SysV consumes the hidden sret pointer from the integer argument budget;
    /// AAPCS64 puts it in the dedicated x8 register instead.
    sret_in_argument_register: bool,
}

impl ForeignCallPlan {
    /// Plan argument placement after classification, preserving aggregate
    /// all-or-nothing register allocation and source ordering.
    pub fn new(inputs: ForeignCallInputs, int_register_budget: usize) -> Self {
        let abi = TargetCCallAbi::new(inputs.flavor);
        let sret_in_argument_register = matches!(&inputs.ret, ForeignReturn::AggregateSret { .. })
            && !abi.sret_pointer_in_dedicated_register();
        let shapes = inputs
            .args
            .iter()
            .map(shape_from_foreign_arg)
            .collect::<Vec<_>>();
        let placement = place_foreign_args(
            &shapes,
            u64::try_from(int_register_budget).expect("foreign register budget must fit u64"),
            sret_in_argument_register,
        )
        .expect("foreign argument placement must pass frame-budget preflight");

        Self {
            inputs,
            placements: placement.placements,
            byref_bytes: u32::try_from(placement.byref_bytes)
                .expect("foreign argument storage must fit u32"),
            sret_in_argument_register,
        }
    }
}

/// Target-specific leaves for the shared foreign-call lowering driver.
///
/// The driver below owns the call's event order and all target-independent
/// decisions. Implementations only select concrete registers/instructions and
/// perform the target's stack and image operations.
pub(crate) trait ForeignCallLoweringBackend {
    fn target_c_flavor(&self) -> TargetCAbiFlavor;
    fn foreign_int_arg_register_count(&self) -> usize;
    fn foreign_reserve_sret(&mut self, image: &AggregateImage) -> VReg;
    fn foreign_get_vreg(&mut self, value: CfgValue) -> VReg;
    fn foreign_image_arg_eightbytes(
        &mut self,
        value: CfgValue,
        image: &AggregateImage,
    ) -> Vec<VReg>;
    fn foreign_byref_copy(&mut self, value: CfgValue, image: &AggregateImage) -> VReg;
    fn foreign_emit_stack_args(&mut self, stack_ops: &[VReg]);
    fn foreign_emit_register_args(&mut self, int_ops: &[VReg]);
    fn foreign_assign_sret(&mut self, sret_ptr: VReg);
    fn foreign_issue_call(&mut self, symbol: &str);
    fn foreign_cleanup_stack(&mut self, stack_count: usize);
    fn foreign_cleanup_byref(&mut self, byref_bytes: u32);
    fn foreign_zero_result(&mut self, primary: VReg);
    fn foreign_scalar_result(&mut self, primary: VReg, ext: ScalarAbiExtension);
    fn foreign_register_result(&mut self, primary: VReg, image: &AggregateImage) -> Vec<VReg>;
    fn foreign_sret_result(
        &mut self,
        primary: VReg,
        image: &AggregateImage,
        sret_ptr: VReg,
    ) -> Vec<VReg>;
    fn foreign_move_primary(&mut self, primary: VReg, slot: VReg);
}

/// Lower one aggregate-crossing foreign call through the single shared event
/// sequence. Concrete backends provide only instruction-selection leaves via
/// [`ForeignCallLoweringBackend`].
pub(crate) fn lower_foreign_call<B: ForeignCallLoweringBackend>(
    backend: &mut B,
    inputs: ForeignCallInputs,
    primary: VReg,
) -> crate::value_plan::MaterializedValue {
    assert_eq!(
        inputs.flavor,
        backend.target_c_flavor(),
        "foreign call ABI flavor disagrees with the selected backend"
    );
    let abi = TargetCCallAbi::new(backend.target_c_flavor());
    let budget = usize::try_from(abi.int_arg_register_budget())
        .expect("target-C integer argument budget must fit usize");
    assert_eq!(
        budget,
        backend.foreign_int_arg_register_count(),
        "backend argument-register roster disagrees with target-C ABI"
    );
    let plan = ForeignCallPlan::new(inputs, budget);
    let inputs = &plan.inputs;

    // Establish indirect-result storage first; it remains live through the
    // call and is released only after its native slots have been reconstructed.
    let sret_ptr = match &inputs.ret {
        ForeignReturn::AggregateSret { image } => Some(backend.foreign_reserve_sret(image)),
        _ => None,
    };

    let mut int_ops = Vec::new();
    let mut stack_ops = Vec::new();
    if plan.sret_in_argument_register {
        int_ops.push(sret_ptr.expect("SysV sret placement requires storage"));
    }
    for placement in &plan.placements {
        let to_register = matches!(placement, ForeignArgPlacement::Register { .. });
        let arg = &inputs.args[placement.arg_index()];
        let mut placed = |values: Vec<VReg>| {
            if to_register {
                int_ops.extend(values);
            } else {
                stack_ops.extend(values);
            }
        };
        match arg {
            ForeignArg::Scalar { value } => placed(vec![backend.foreign_get_vreg(*value)]),
            ForeignArg::AggregateRegisters { value, image }
            | ForeignArg::AggregateByvalStack { value, image } => {
                if matches!(arg, ForeignArg::AggregateByvalStack { .. })
                    && inputs.flavor != TargetCAbiFlavor::SysVAmd64
                {
                    panic!(
                        "AAPCS64 passes a >16-byte aggregate by reference to a caller copy, not \
                         byval-on-stack; ByValueStack is a SysV-only class"
                    );
                }
                placed(backend.foreign_image_arg_eightbytes(*value, image));
            }
            ForeignArg::AggregateByRefCopy { value, image } => {
                if inputs.flavor != TargetCAbiFlavor::Aapcs64 {
                    panic!("SysV AMD64 does not pass foreign aggregates by reference");
                }
                placed(vec![backend.foreign_byref_copy(*value, image)]);
            }
        }
    }

    // The adapters own concrete stack layout and register moves, but the
    // driver fixes the shared boundary order: outgoing stack, integer args,
    // hidden sret assignment, call, then stack/byref cleanup.
    backend.foreign_emit_stack_args(&stack_ops);
    backend.foreign_emit_register_args(&int_ops);
    if let Some(sret_ptr) = sret_ptr {
        if !plan.sret_in_argument_register {
            backend.foreign_assign_sret(sret_ptr);
        }
    }
    backend.foreign_issue_call(inputs.symbol_ref());
    backend.foreign_cleanup_stack(stack_ops.len());
    backend.foreign_cleanup_byref(plan.byref_bytes);

    let slots = match &inputs.ret {
        ForeignReturn::ZeroSized => {
            backend.foreign_zero_result(primary);
            Vec::new()
        }
        ForeignReturn::Scalar { ext } => {
            backend.foreign_scalar_result(primary, *ext);
            Vec::new()
        }
        ForeignReturn::AggregateRegisters { image } => {
            backend.foreign_register_result(primary, image)
        }
        ForeignReturn::AggregateSret { image } => backend.foreign_sret_result(
            primary,
            image,
            sret_ptr.expect("sret return requires storage"),
        ),
    };
    if let Some(&slot) = slots.first() {
        backend.foreign_move_primary(primary, slot);
    }
    crate::value_plan::MaterializedValue { primary, slots }
}

impl ForeignCallInputs {
    /// Whether a foreign call to `return_ty` with `args` needs the aggregate
    /// path at all: true when the return or any argument is an aggregate
    /// (struct/array) type. A scalars-only foreign call keeps P2's native-plan
    /// route with a boundary return extension.
    pub(crate) fn call_touches_aggregate(cfg: &Cfg, return_ty: Type, args: &[CfgCallArg]) -> bool {
        is_aggregate(return_ty)
            || args
                .iter()
                .any(|arg| is_aggregate(cfg.get_inst(arg.value).ty))
    }

    /// Compute the simultaneous transient area of an aggregate foreign call
    /// using the target-C classifier that the lowerers consume. This accounts
    /// for hidden sret storage, SysV by-value stack cells, AAPCS64 caller-owned
    /// by-reference copies, and argument-register exhaustion that spills
    /// pointers/eightbytes to the outgoing stack area.
    pub(crate) fn checked_call_area_bytes(
        cfg: &Cfg,
        type_pool: &FrozenTypeInternPool,
        return_ty: Type,
        args: &[CfgCallArg],
        flavor: TargetCAbiFlavor,
    ) -> Result<u32, crate::frame_layout::FrameBudgetExceeded> {
        let abi = TargetCCallAbi::new(flavor);
        let register_budget = u64::from(abi.int_arg_register_budget());
        let sret_storage_bytes = if is_aggregate(return_ty) {
            let layout = type_pool.layout(return_ty);
            match abi.classify_aggregate_return(layout.size, layout.alignment) {
                AggregateReturnClass::Indirect { .. } => {
                    u64::from(checked_aligned_region_bytes(layout.size)?)
                }
                _ => 0,
            }
        } else {
            0
        };
        let sret_in_argument_register =
            sret_storage_bytes > 0 && !abi.sret_pointer_in_dedicated_register();
        let shapes = args
            .iter()
            .map(|arg| shape_from_cfg_arg(cfg, type_pool, abi, arg))
            .collect::<Result<Vec<_>, _>>()?;
        let placement = place_foreign_args(&shapes, register_budget, sret_in_argument_register)?;
        let mut indirect_bytes = 0_u64;

        for (arg, shape) in args.iter().zip(&shapes) {
            let ty = cfg.get_inst(arg.value).ty;
            if !is_aggregate(ty) {
                continue;
            }

            let layout = type_pool.layout(ty);
            match abi.classify_aggregate_arg(layout.size, layout.alignment) {
                AggregateArgClass::IntegerRegisters { .. } => {
                    // The backend marshals every register-packed aggregate
                    // through a temporary image buffer. It is short-lived, but
                    // can overlap the hidden sret area and any earlier AAPCS64
                    // caller-owned copies.
                    let scratch = checked_aligned_region_bytes(layout.size)?;
                    checked_call_area_bytes(
                        0,
                        sret_storage_bytes,
                        indirect_bytes
                            .checked_add(u64::from(scratch))
                            .ok_or(crate::frame_layout::FrameBudgetExceeded)?,
                    )?;
                }
                AggregateArgClass::ByValueStack { .. } => {
                    // SysV still needs a temporary C image before its
                    // eightbytes are copied into the outgoing stack area.
                    let scratch = checked_aligned_region_bytes(layout.size)?;
                    checked_call_area_bytes(
                        0,
                        sret_storage_bytes,
                        indirect_bytes
                            .checked_add(u64::from(scratch))
                            .ok_or(crate::frame_layout::FrameBudgetExceeded)?,
                    )?;
                }
                AggregateArgClass::ByReferenceCopy { .. } => {
                    indirect_bytes = indirect_bytes
                        .checked_add(shape.byref_bytes)
                        .ok_or(crate::frame_layout::FrameBudgetExceeded)?;
                }
            }
        }

        assert_eq!(
            indirect_bytes, placement.byref_bytes,
            "foreign-call preflight and placement disagree on by-reference storage"
        );

        if is_aggregate(return_ty) {
            let layout = type_pool.layout(return_ty);
            match abi.classify_aggregate_return(layout.size, layout.alignment) {
                AggregateReturnClass::IntegerRegisters { .. } => {
                    // Return-image scratch is allocated after outgoing stack
                    // and AAPCS64 by-reference areas are released, so it is a
                    // separate peak from the call-boundary reservation.
                    let scratch = checked_aligned_region_bytes(layout.size)?;
                    checked_call_area_bytes(0, 0, u64::from(scratch))?;
                }
                AggregateReturnClass::Indirect { .. } => {}
            }
        }
        checked_call_area_bytes(placement.stack_cells, sret_storage_bytes, indirect_bytes)
    }

    /// Classify a foreign call through the shared [`TargetCCallAbi`] authority.
    pub(crate) fn from_cfg(
        symbol: String,
        cfg: &Cfg,
        type_pool: &FrozenTypeInternPool,
        return_ty: Type,
        args: &[CfgCallArg],
        flavor: TargetCAbiFlavor,
    ) -> Self {
        let abi = TargetCCallAbi::new(flavor);
        let planned_args = args
            .iter()
            .map(|arg| {
                let value = arg.value;
                let ty = cfg.get_inst(value).ty;
                assert!(
                    matches!(arg.mode, rue_cfg::CfgArgMode::Normal),
                    "an `extern \"C\"` argument is always by value; inout/borrow do not cross a \
                     C boundary"
                );
                if !is_aggregate(ty) {
                    return ForeignArg::Scalar { value };
                }
                let image = AggregateImage::for_type(type_pool, ty);
                match abi.classify_aggregate_arg(image.size as u64, image.align as u64) {
                    AggregateArgClass::IntegerRegisters { .. } => {
                        ForeignArg::AggregateRegisters { value, image }
                    }
                    AggregateArgClass::ByValueStack { .. } => {
                        ForeignArg::AggregateByvalStack { value, image }
                    }
                    AggregateArgClass::ByReferenceCopy { .. } => {
                        ForeignArg::AggregateByRefCopy { value, image }
                    }
                }
            })
            .collect();

        let ret = if !is_aggregate(return_ty) {
            match type_pool.abi_slot_count(return_ty) {
                0 => ForeignReturn::ZeroSized,
                _ => ForeignReturn::Scalar {
                    ext: abi.scalar_return_extension(return_ty),
                },
            }
        } else {
            let image = AggregateImage::for_type(type_pool, return_ty);
            match abi.classify_aggregate_return(image.size as u64, image.align as u64) {
                AggregateReturnClass::IntegerRegisters { .. } => {
                    ForeignReturn::AggregateRegisters { image }
                }
                AggregateReturnClass::Indirect { .. } => ForeignReturn::AggregateSret { image },
            }
        };

        Self {
            symbol,
            flavor,
            args: planned_args,
            ret,
        }
    }
}

/// Whether `ty` is an aggregate (struct or fixed array) — the types whose
/// target-C crossing needs the image-based foreign path.
fn is_aggregate(ty: Type) -> bool {
    matches!(
        ty.kind(),
        rue_air::TypeKind::Struct(_) | rue_air::TypeKind::Array(_)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use rue_cfg::CfgValue;

    fn image(size: u32) -> AggregateImage {
        AggregateImage {
            map: Vec::new(),
            padding: Vec::new(),
            slot_count: 0,
            size,
            align: 8,
            storage_bytes: size.div_ceil(16) * 16,
        }
    }

    fn scalar(index: u32) -> ForeignArg {
        ForeignArg::Scalar {
            value: CfgValue::from_raw(index),
        }
    }

    #[test]
    fn sysv_hidden_sret_and_aggregate_placement_are_shared_decisions() {
        let inputs = ForeignCallInputs {
            symbol: "f".into(),
            flavor: TargetCAbiFlavor::SysVAmd64,
            args: vec![
                scalar(0),
                ForeignArg::AggregateRegisters {
                    value: CfgValue::from_raw(1),
                    image: image(16),
                },
                scalar(2),
                scalar(3),
                scalar(4),
            ],
            ret: ForeignReturn::AggregateSret { image: image(24) },
        };
        let plan = ForeignCallPlan::new(inputs, 6);

        assert!(plan.sret_in_argument_register);
        assert_eq!(plan.placements.len(), 5);
        assert!(matches!(
            plan.placements[0],
            ForeignArgPlacement::Register { .. }
        ));
        assert!(matches!(
            plan.placements[1],
            ForeignArgPlacement::Register { .. }
        ));
        assert!(matches!(
            plan.placements[2],
            ForeignArgPlacement::Register { .. }
        ));
        assert!(matches!(
            plan.placements[3],
            ForeignArgPlacement::Register { .. }
        ));
        assert!(matches!(
            plan.placements[4],
            ForeignArgPlacement::Stack { .. }
        ));
    }

    #[test]
    fn aapcs_byref_storage_does_not_consume_a_register_per_byte() {
        let inputs = ForeignCallInputs {
            symbol: "f".into(),
            flavor: TargetCAbiFlavor::Aapcs64,
            args: vec![ForeignArg::AggregateByRefCopy {
                value: CfgValue::from_raw(0),
                image: image(24),
            }],
            ret: ForeignReturn::ZeroSized,
        };
        let plan = ForeignCallPlan::new(inputs, 8);

        assert!(!plan.sret_in_argument_register);
        assert_eq!(plan.byref_bytes, 32);
        assert!(matches!(
            plan.placements[0],
            ForeignArgPlacement::Register { .. }
        ));
    }

    #[test]
    fn placement_arithmetic_reports_budget_overflow_without_panicking() {
        let byref_overflow = place_foreign_args(
            &[
                ForeignArgShape {
                    pieces: 0,
                    byref_bytes: u64::MAX,
                    can_register: false,
                },
                ForeignArgShape {
                    pieces: 0,
                    byref_bytes: 1,
                    can_register: false,
                },
            ],
            0,
            false,
        );
        assert!(byref_overflow.is_err());

        let stack_overflow = place_foreign_args(
            &[
                ForeignArgShape {
                    pieces: u64::MAX,
                    byref_bytes: 0,
                    can_register: false,
                },
                ForeignArgShape {
                    pieces: 1,
                    byref_bytes: 0,
                    can_register: false,
                },
            ],
            0,
            false,
        );
        assert!(stack_overflow.is_err());

        let register_overflow = place_foreign_args(
            &[
                ForeignArgShape {
                    pieces: u64::MAX,
                    byref_bytes: 0,
                    can_register: true,
                },
                ForeignArgShape {
                    pieces: 1,
                    byref_bytes: 0,
                    can_register: true,
                },
            ],
            u64::MAX,
            false,
        );
        assert!(register_overflow.is_err());
    }

    #[derive(Debug)]
    struct TraceBackend {
        flavor: TargetCAbiFlavor,
        register_count: usize,
        next_vreg: u32,
        events: Vec<String>,
    }

    impl TraceBackend {
        fn new(flavor: TargetCAbiFlavor, register_count: usize) -> Self {
            Self {
                flavor,
                register_count,
                next_vreg: 100,
                events: Vec::new(),
            }
        }

        fn vreg(&mut self) -> VReg {
            let vreg = VReg::new(self.next_vreg);
            self.next_vreg += 1;
            vreg
        }

        fn record(&mut self, event: impl Into<String>) {
            self.events.push(event.into());
        }
    }

    impl ForeignCallLoweringBackend for TraceBackend {
        fn target_c_flavor(&self) -> TargetCAbiFlavor {
            self.flavor
        }

        fn foreign_int_arg_register_count(&self) -> usize {
            self.register_count
        }

        fn foreign_reserve_sret(&mut self, _image: &AggregateImage) -> VReg {
            self.record("reserve_sret");
            self.vreg()
        }

        fn foreign_get_vreg(&mut self, value: CfgValue) -> VReg {
            self.record(format!("get:{value:?}"));
            self.vreg()
        }

        fn foreign_image_arg_eightbytes(
            &mut self,
            _value: CfgValue,
            image: &AggregateImage,
        ) -> Vec<VReg> {
            self.record(format!("image:{}", image.eightbytes()));
            (0..image.eightbytes()).map(|_| self.vreg()).collect()
        }

        fn foreign_byref_copy(&mut self, _value: CfgValue, image: &AggregateImage) -> VReg {
            self.record(format!("byref:{}", image.storage_bytes));
            self.vreg()
        }

        fn foreign_emit_stack_args(&mut self, stack_ops: &[VReg]) {
            self.record(format!("stack:{stack_ops:?}"));
        }

        fn foreign_emit_register_args(&mut self, int_ops: &[VReg]) {
            self.record(format!("registers:{int_ops:?}"));
        }

        fn foreign_assign_sret(&mut self, _sret_ptr: VReg) {
            self.record("assign_sret");
        }

        fn foreign_issue_call(&mut self, symbol: &str) {
            self.record(format!("call:{symbol}"));
        }

        fn foreign_cleanup_stack(&mut self, stack_count: usize) {
            self.record(format!("cleanup_stack:{stack_count}"));
        }

        fn foreign_cleanup_byref(&mut self, byref_bytes: u32) {
            self.record(format!("cleanup_byref:{byref_bytes}"));
        }

        fn foreign_zero_result(&mut self, _primary: VReg) {
            self.record("zero_result");
        }

        fn foreign_scalar_result(&mut self, _primary: VReg, _ext: ScalarAbiExtension) {
            self.record("scalar_result");
        }

        fn foreign_register_result(&mut self, _primary: VReg, image: &AggregateImage) -> Vec<VReg> {
            self.record(format!("register_result:{}", image.eightbytes()));
            vec![self.vreg()]
        }

        fn foreign_sret_result(
            &mut self,
            _primary: VReg,
            image: &AggregateImage,
            _sret_ptr: VReg,
        ) -> Vec<VReg> {
            self.record(format!("sret_result:{}", image.eightbytes()));
            vec![self.vreg()]
        }

        fn foreign_move_primary(&mut self, _primary: VReg, _slot: VReg) {
            self.record("move_primary");
        }
    }

    #[test]
    fn shared_driver_preserves_sysv_and_aapcs_event_order() {
        let sysv_inputs = ForeignCallInputs {
            symbol: "sysv_fn".into(),
            flavor: TargetCAbiFlavor::SysVAmd64,
            args: vec![
                scalar(0),
                ForeignArg::AggregateRegisters {
                    value: CfgValue::from_raw(1),
                    image: image(16),
                },
                scalar(2),
                scalar(3),
                scalar(4),
            ],
            ret: ForeignReturn::AggregateSret { image: image(24) },
        };
        let mut sysv = TraceBackend::new(TargetCAbiFlavor::SysVAmd64, 6);
        let sysv_result = lower_foreign_call(&mut sysv, sysv_inputs, VReg::new(0));
        assert_eq!(sysv_result.slots.len(), 1);
        assert_eq!(
            sysv.events,
            vec![
                "reserve_sret",
                "get:CfgValue(0)",
                "image:2",
                "get:CfgValue(2)",
                "get:CfgValue(3)",
                "get:CfgValue(4)",
                "stack:[VReg(106)]",
                "registers:[VReg(100), VReg(101), VReg(102), VReg(103), VReg(104), VReg(105)]",
                "call:sysv_fn",
                "cleanup_stack:1",
                "cleanup_byref:0",
                "sret_result:3",
                "move_primary",
            ]
        );

        let aapcs_inputs = ForeignCallInputs {
            symbol: "aapcs_fn".into(),
            flavor: TargetCAbiFlavor::Aapcs64,
            args: vec![
                scalar(0),
                ForeignArg::AggregateByRefCopy {
                    value: CfgValue::from_raw(1),
                    image: image(24),
                },
                scalar(2),
                scalar(3),
                scalar(4),
                scalar(5),
                scalar(6),
                scalar(7),
                scalar(8),
                scalar(9),
            ],
            ret: ForeignReturn::AggregateSret { image: image(24) },
        };
        let mut aapcs = TraceBackend::new(TargetCAbiFlavor::Aapcs64, 8);
        let aapcs_result = lower_foreign_call(&mut aapcs, aapcs_inputs, VReg::new(0));
        assert_eq!(aapcs_result.slots.len(), 1);
        assert_eq!(
            aapcs.events,
            vec![
                "reserve_sret",
                "get:CfgValue(0)",
                "byref:32",
                "get:CfgValue(2)",
                "get:CfgValue(3)",
                "get:CfgValue(4)",
                "get:CfgValue(5)",
                "get:CfgValue(6)",
                "get:CfgValue(7)",
                "get:CfgValue(8)",
                "get:CfgValue(9)",
                "stack:[VReg(109), VReg(110)]",
                "registers:[VReg(101), VReg(102), VReg(103), VReg(104), VReg(105), VReg(106), VReg(107), VReg(108)]",
                "assign_sret",
                "call:aapcs_fn",
                "cleanup_stack:2",
                "cleanup_byref:32",
                "sret_result:3",
                "move_primary",
            ]
        );

        let register_return_inputs = ForeignCallInputs {
            symbol: "aapcs_register_fn".into(),
            flavor: TargetCAbiFlavor::Aapcs64,
            args: Vec::new(),
            ret: ForeignReturn::AggregateRegisters { image: image(16) },
        };
        let mut register_return = TraceBackend::new(TargetCAbiFlavor::Aapcs64, 8);
        let register_result =
            lower_foreign_call(&mut register_return, register_return_inputs, VReg::new(0));
        assert_eq!(register_result.slots.len(), 1);
        assert_eq!(
            register_return.events,
            vec![
                "stack:[]",
                "registers:[]",
                "call:aapcs_register_fn",
                "cleanup_stack:0",
                "cleanup_byref:0",
                "register_result:2",
                "move_primary",
            ]
        );
    }
}
