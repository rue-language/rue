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
    TargetCCallAbi, Type,
};
use rue_cfg::{Cfg, CfgCallArg, CfgValue};
use rue_target::{CallingConvention, StackedArgumentPacking};

use crate::frame_layout::{
    FrameBudgetExceeded, checked_aligned_region_bytes, checked_call_area_from_stack_bytes,
};
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
    ///
    /// `natural_bytes` is the scalar's declared C width (1, 2, 4, or 8). It is
    /// the footprint a stacked copy occupies under a psABI that packs its
    /// outgoing argument area at natural size (Apple's arm64 amendment); under
    /// the 8-byte-slot rule the whole slot is written and the width is unused.
    Scalar { value: CfgValue, natural_bytes: u32 },
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
/// the convention whose classifier produced them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForeignCallInputs {
    /// The resolved (unmangled) C symbol the undefined reference targets.
    pub symbol: String,
    /// The convention the compilation target's `"C"` alias resolves to. Always
    /// a C row; the native Rue convention never reaches this path.
    pub convention: CallingConvention,
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
    Register {
        arg_index: usize,
    },
    Stack {
        arg_index: usize,
        /// Byte offset of this argument from the base of the outgoing argument
        /// area.
        offset: u64,
    },
}

/// One store into the outgoing argument area, in ascending-offset order.
///
/// `bytes` is the number of low bytes of `value` the store must commit: 1, 2, 4,
/// or 8. Under [`StackedArgumentPacking::EightByteSlots`] it is always 8. Under
/// [`StackedArgumentPacking::NaturalSize`] a stacked *scalar* names its own C
/// width, so an `i8` writes one byte at the next free byte and an `i16` starts
/// at the next even offset.
///
/// `offset` is always a multiple of `bytes`, which is what makes every store
/// encodable in AArch64's scaled `imm12` addressing mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ForeignStackStore {
    pub(crate) value: VReg,
    pub(crate) offset: u32,
    pub(crate) bytes: u32,
}

/// The complete outgoing argument area: what to store where, and how many bytes
/// the caller reserves (already rounded to the psABI's call alignment).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ForeignStackArea {
    pub(crate) stores: Vec<ForeignStackStore>,
    pub(crate) bytes: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ForeignArgShape {
    /// Number of integer eightbytes (or one pointer) consumed by the argument.
    pieces: u64,
    /// Storage retained for a by-reference argument copy.
    byref_bytes: u64,
    /// Whether this argument may use integer argument registers.
    can_register: bool,
    /// Whether the argument is a scalar or pointer rather than a composite.
    /// Only the natural-size packing rule reads it.
    scalar: bool,
    /// A scalar's declared C width in bytes (1, 2, 4, or 8). Only the
    /// natural-size packing rule reads it, and only for a scalar.
    natural_bytes: u64,
}

impl ForeignArgShape {
    /// The footprint and alignment this argument claims in the outgoing
    /// argument area under `packing`.
    ///
    /// Apple's amendment is applied to stacked *scalars*, which is where Rue's
    /// supported C surface differs from the generic 8-byte-slot rule: a stacked
    /// `i8` occupies one byte, an `i16` starts at the next even offset. A
    /// composite keeps whole eightbytes at 8-byte alignment under every row,
    /// because it crosses through its eightbyte image and a byte-exact copy
    /// would need marshaling this path does not have — the one Apple stack
    /// amendment still open, recorded in
    /// `docs/notes/ffi-abi-conformance-audit.md`. Keeping composites on whole
    /// eightbytes is also what keeps every store's offset a multiple of its
    /// width, so each one encodes in AArch64's scaled addressing mode.
    fn stack_extent(self, packing: StackedArgumentPacking) -> (u64, u64) {
        match packing {
            StackedArgumentPacking::NaturalSize if self.scalar => {
                let bytes = self.natural_bytes.clamp(1, 8);
                (bytes, bytes)
            }
            StackedArgumentPacking::EightByteSlots | StackedArgumentPacking::NaturalSize => {
                (self.pieces.saturating_mul(8), 8)
            }
        }
    }

    /// The width of each store this argument makes into the outgoing area: its
    /// packed footprint for a scalar, one whole eightbyte per piece otherwise.
    fn store_widths(self, packing: StackedArgumentPacking) -> impl Iterator<Item = u64> {
        let (size, _) = self.stack_extent(packing);
        let width = if self.scalar { size } else { 8 };
        (0..self.pieces).map(move |_| width)
    }
}

/// Round `value` up to a multiple of `align`, refusing rather than wrapping.
///
/// `align` is always one of the psABI's argument alignments (1, 2, 4, or 8), so
/// the power-of-two mask below is exact; [`ForeignArgShape::stack_extent`] is
/// the only producer.
fn align_up(value: u64, align: u64) -> Result<u64, FrameBudgetExceeded> {
    value
        .checked_add(align - 1)
        .map(|sum| sum & !(align - 1))
        .ok_or(FrameBudgetExceeded)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ForeignPlacementPlan {
    placements: Vec<ForeignArgPlacement>,
    byref_bytes: u64,
    /// Bytes the outgoing argument area occupies before call alignment.
    stack_bytes: u64,
    sret_in_argument_register: bool,
}

impl ForeignPlacementPlan {
    /// Reserved size of the outgoing argument area: the packed footprint
    /// rounded to the call-boundary alignment.
    fn stack_area_bytes(&self) -> Result<u32, FrameBudgetExceeded> {
        checked_aligned_region_bytes(self.stack_bytes)
    }
}

fn place_foreign_args(
    shapes: &[ForeignArgShape],
    int_register_budget: u64,
    sret_in_argument_register: bool,
    packing: StackedArgumentPacking,
) -> Result<ForeignPlacementPlan, FrameBudgetExceeded> {
    let mut used_registers = u64::from(sret_in_argument_register);
    let mut placements = Vec::with_capacity(shapes.len());
    let mut byref_bytes = 0_u64;
    let mut stack_bytes = 0_u64;

    let stack = |shape: &ForeignArgShape,
                 arg_index: usize,
                 stack_bytes: &mut u64|
     -> Result<ForeignArgPlacement, FrameBudgetExceeded> {
        let (size, align) = shape.stack_extent(packing);
        let offset = align_up(*stack_bytes, align)?;
        *stack_bytes = offset.checked_add(size).ok_or(FrameBudgetExceeded)?;
        Ok(ForeignArgPlacement::Stack { arg_index, offset })
    };

    for (arg_index, shape) in shapes.iter().enumerate() {
        byref_bytes = byref_bytes
            .checked_add(shape.byref_bytes)
            .ok_or(FrameBudgetExceeded)?;
        let placement = if shape.can_register {
            let register_end = used_registers
                .checked_add(shape.pieces)
                .ok_or(FrameBudgetExceeded)?;
            if register_end <= int_register_budget {
                used_registers = register_end;
                ForeignArgPlacement::Register { arg_index }
            } else {
                stack(shape, arg_index, &mut stack_bytes)?
            }
        } else {
            stack(shape, arg_index, &mut stack_bytes)?
        };
        placements.push(placement);
    }

    Ok(ForeignPlacementPlan {
        placements,
        byref_bytes,
        stack_bytes,
        sret_in_argument_register,
    })
}

fn shape_from_foreign_arg(arg: &ForeignArg) -> ForeignArgShape {
    match arg {
        ForeignArg::Scalar { natural_bytes, .. } => ForeignArgShape {
            pieces: 1,
            byref_bytes: 0,
            can_register: true,
            scalar: true,
            natural_bytes: u64::from(*natural_bytes),
        },
        ForeignArg::AggregateRegisters { image, .. } => ForeignArgShape {
            pieces: u64::from(image.eightbytes()),
            byref_bytes: 0,
            can_register: true,
            scalar: false,
            natural_bytes: u64::from(image.size),
        },
        ForeignArg::AggregateByvalStack { image, .. } => ForeignArgShape {
            pieces: u64::from(image.eightbytes()),
            byref_bytes: 0,
            can_register: false,
            scalar: false,
            natural_bytes: u64::from(image.size),
        },
        // A by-reference copy crosses as one pointer, which is a
        // register-width scalar wherever it lands.
        ForeignArg::AggregateByRefCopy { image, .. } => ForeignArgShape {
            pieces: 1,
            byref_bytes: u64::from(image.storage_bytes),
            can_register: true,
            scalar: true,
            natural_bytes: 8,
        },
    }
}

fn shape_from_cfg_arg(
    cfg: &Cfg,
    type_pool: &FrozenTypeInternPool,
    abi: TargetCCallAbi,
    arg: &CfgCallArg,
) -> Result<ForeignArgShape, FrameBudgetExceeded> {
    let ty = cfg.get_inst(arg.value).ty;
    if !is_aggregate(ty) {
        return Ok(ForeignArgShape {
            pieces: 1,
            byref_bytes: 0,
            can_register: true,
            scalar: true,
            natural_bytes: u64::from(abi.scalar_arg_extension(ty).natural_bytes()),
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
                scalar: false,
                natural_bytes: layout.size,
            },
            AggregateArgClass::ByValueStack { .. } => ForeignArgShape {
                pieces: layout.size.div_ceil(8),
                byref_bytes: 0,
                can_register: false,
                scalar: false,
                natural_bytes: layout.size,
            },
            AggregateArgClass::ByReferenceCopy { .. } => ForeignArgShape {
                pieces: 1,
                byref_bytes: copy_bytes()?,
                can_register: true,
                scalar: true,
                natural_bytes: 8,
            },
        },
    )
}

impl ForeignArgPlacement {
    #[inline]
    pub const fn arg_index(self) -> usize {
        match self {
            Self::Register { arg_index } | Self::Stack { arg_index, .. } => arg_index,
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
    /// Per-argument shapes, retained so the driver can ask each stacked
    /// argument how wide each of its stores is.
    shapes: Vec<ForeignArgShape>,
    /// How the convention lays out the outgoing argument area.
    packing: StackedArgumentPacking,
    /// Reserved size of the outgoing argument area, call-aligned.
    stack_area_bytes: u32,
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
        let abi = TargetCCallAbi::new(inputs.convention);
        let packing = abi.stacked_argument_packing();
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
            packing,
        )
        .expect("foreign argument placement must pass frame-budget preflight");
        let stack_area_bytes = placement
            .stack_area_bytes()
            .expect("foreign stack area must pass frame-budget preflight");

        Self {
            inputs,
            placements: placement.placements,
            shapes,
            packing,
            stack_area_bytes,
            byref_bytes: u32::try_from(placement.byref_bytes)
                .expect("foreign argument storage must fit u32"),
            sret_in_argument_register,
        }
    }

    /// The stores one stacked argument makes: its eightbyte (or narrower)
    /// pieces at ascending offsets from the argument's own packed offset.
    fn stack_stores(
        &self,
        arg_index: usize,
        offset: u64,
        values: &[VReg],
    ) -> Vec<ForeignStackStore> {
        let widths = self.shapes[arg_index].store_widths(self.packing);
        values
            .iter()
            .zip(widths)
            .enumerate()
            .map(|(piece, (value, bytes))| ForeignStackStore {
                value: *value,
                offset: u32::try_from(offset + piece as u64 * 8)
                    .expect("foreign stack offset must fit u32"),
                bytes: u32::try_from(bytes).expect("foreign stack store width must fit u32"),
            })
            .collect()
    }
}

#[cfg(test)]
impl ForeignCallPlan {
    /// The outgoing argument area this plan produces, taking one vreg per
    /// stacked piece in placement order. Backend encoding tests drive
    /// [`ForeignCallLoweringBackend::foreign_emit_stack_args`] with it instead
    /// of standing up a whole lowering fixture.
    pub(crate) fn stack_area_for_test(&self, values: &[VReg]) -> ForeignStackArea {
        let mut values = values.iter().copied();
        let mut stores = Vec::new();
        for placement in &self.placements {
            let ForeignArgPlacement::Stack { arg_index, offset } = placement else {
                continue;
            };
            let pieces = self.shapes[*arg_index].pieces as usize;
            let taken = (&mut values).take(pieces).collect::<Vec<_>>();
            assert_eq!(taken.len(), pieces, "one vreg per stacked piece");
            stores.extend(self.stack_stores(*arg_index, *offset, &taken));
        }
        assert!(
            values.next().is_none(),
            "every vreg must name a stacked piece"
        );
        ForeignStackArea {
            stores,
            bytes: self.stack_area_bytes,
        }
    }
}

/// Target-specific leaves for the shared foreign-call lowering driver.
///
/// The driver below owns the call's event order and all target-independent
/// decisions. Implementations only select concrete registers/instructions and
/// perform the target's stack and image operations.
pub(crate) trait ForeignCallLoweringBackend {
    /// The convention this backend's compilation target resolves `"C"` to.
    fn target_c_convention(&self) -> CallingConvention;
    fn foreign_int_arg_register_count(&self) -> usize;
    fn foreign_reserve_sret(&mut self, image: &AggregateImage) -> VReg;
    fn foreign_get_vreg(&mut self, value: CfgValue) -> VReg;
    fn foreign_image_arg_eightbytes(
        &mut self,
        value: CfgValue,
        image: &AggregateImage,
    ) -> Vec<VReg>;
    fn foreign_byref_copy(&mut self, value: CfgValue, image: &AggregateImage) -> VReg;
    /// Reserve the outgoing argument area and commit every store in it. The
    /// area's size and each store's offset and width are the convention's
    /// decision, made once in [`ForeignCallPlan`]; the backend chooses only the
    /// instructions.
    fn foreign_emit_stack_args(&mut self, stack: &ForeignStackArea);
    fn foreign_emit_register_args(&mut self, int_ops: &[VReg]);
    fn foreign_assign_sret(&mut self, sret_ptr: VReg);
    fn foreign_issue_call(&mut self, symbol: &str);
    /// Release the outgoing argument area reserved by
    /// [`foreign_emit_stack_args`](Self::foreign_emit_stack_args).
    fn foreign_cleanup_stack(&mut self, stack: &ForeignStackArea);
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
        inputs.convention,
        backend.target_c_convention(),
        "foreign call convention disagrees with the selected backend"
    );
    let abi = TargetCCallAbi::new(backend.target_c_convention());
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
    let mut stack = ForeignStackArea {
        stores: Vec::new(),
        bytes: plan.stack_area_bytes,
    };
    if plan.sret_in_argument_register {
        int_ops.push(sret_ptr.expect("SysV sret placement requires storage"));
    }
    for placement in &plan.placements {
        let arg_index = placement.arg_index();
        let arg = &inputs.args[arg_index];
        let values = match arg {
            ForeignArg::Scalar { value, .. } => vec![backend.foreign_get_vreg(*value)],
            ForeignArg::AggregateRegisters { value, image }
            | ForeignArg::AggregateByvalStack { value, image } => {
                if matches!(arg, ForeignArg::AggregateByvalStack { .. })
                    && inputs.convention != CallingConvention::X86_64SysV
                {
                    panic!(
                        "AAPCS64 passes a >16-byte aggregate by reference to a caller copy, not \
                         byval-on-stack; ByValueStack is a SysV-only class"
                    );
                }
                backend.foreign_image_arg_eightbytes(*value, image)
            }
            ForeignArg::AggregateByRefCopy { value, image } => {
                assert!(
                    matches!(
                        inputs.convention,
                        CallingConvention::Aarch64Aapcs | CallingConvention::Aarch64AapcsDarwin
                    ),
                    "SysV AMD64 does not pass foreign aggregates by reference"
                );
                vec![backend.foreign_byref_copy(*value, image)]
            }
        };
        match placement {
            ForeignArgPlacement::Register { .. } => int_ops.extend(values),
            ForeignArgPlacement::Stack { offset, .. } => stack
                .stores
                .extend(plan.stack_stores(arg_index, *offset, &values)),
        }
    }

    // The adapters own concrete stack layout and register moves, but the
    // driver fixes the shared boundary order: outgoing stack, integer args,
    // hidden sret assignment, call, then stack/byref cleanup.
    backend.foreign_emit_stack_args(&stack);
    backend.foreign_emit_register_args(&int_ops);
    if let Some(sret_ptr) = sret_ptr {
        if !plan.sret_in_argument_register {
            backend.foreign_assign_sret(sret_ptr);
        }
    }
    backend.foreign_issue_call(inputs.symbol_ref());
    backend.foreign_cleanup_stack(&stack);
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
        convention: CallingConvention,
    ) -> Result<u32, FrameBudgetExceeded> {
        let abi = TargetCCallAbi::new(convention);
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
        let placement = place_foreign_args(
            &shapes,
            register_budget,
            sret_in_argument_register,
            abi.stacked_argument_packing(),
        )?;
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
                    checked_call_area_from_stack_bytes(
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
                    checked_call_area_from_stack_bytes(
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
                    checked_call_area_from_stack_bytes(0, 0, u64::from(scratch))?;
                }
                AggregateReturnClass::Indirect { .. } => {}
            }
        }
        checked_call_area_from_stack_bytes(
            u64::from(placement.stack_area_bytes()?),
            sret_storage_bytes,
            indirect_bytes,
        )
    }

    /// Classify a foreign call through the shared [`TargetCCallAbi`] authority.
    pub(crate) fn from_cfg(
        symbol: String,
        cfg: &Cfg,
        type_pool: &FrozenTypeInternPool,
        return_ty: Type,
        args: &[CfgCallArg],
        convention: CallingConvention,
    ) -> Self {
        let abi = TargetCCallAbi::new(convention);
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
                    return ForeignArg::Scalar {
                        value,
                        natural_bytes: abi.scalar_arg_extension(ty).natural_bytes(),
                    };
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
            convention,
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
        image_aligned(size, 8)
    }

    fn image_aligned(size: u32, align: u32) -> AggregateImage {
        AggregateImage {
            map: Vec::new(),
            padding: Vec::new(),
            slot_count: 0,
            size,
            align,
            storage_bytes: size.div_ceil(16) * 16,
        }
    }

    fn scalar(index: u32) -> ForeignArg {
        narrow_scalar(index, 8)
    }

    fn narrow_scalar(index: u32, natural_bytes: u32) -> ForeignArg {
        ForeignArg::Scalar {
            value: CfgValue::from_raw(index),
            natural_bytes,
        }
    }

    fn shape(pieces: u64, byref_bytes: u64, can_register: bool) -> ForeignArgShape {
        ForeignArgShape {
            pieces,
            byref_bytes,
            can_register,
            scalar: false,
            natural_bytes: pieces.saturating_mul(8),
        }
    }

    fn stack_offsets(plan: &ForeignCallPlan) -> Vec<u64> {
        plan.placements
            .iter()
            .filter_map(|placement| match placement {
                ForeignArgPlacement::Stack { offset, .. } => Some(*offset),
                ForeignArgPlacement::Register { .. } => None,
            })
            .collect()
    }

    #[test]
    fn sysv_hidden_sret_and_aggregate_placement_are_shared_decisions() {
        let inputs = ForeignCallInputs {
            symbol: "f".into(),
            convention: CallingConvention::X86_64SysV,
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
            ForeignArgPlacement::Stack { offset: 0, .. }
        ));
    }

    #[test]
    fn aapcs_byref_storage_does_not_consume_a_register_per_byte() {
        let inputs = ForeignCallInputs {
            symbol: "f".into(),
            convention: CallingConvention::Aarch64Aapcs,
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

    /// The byval tail of a call whose arguments overflow the eight AAPCS64
    /// integer registers. Every argument after the eighth is stacked, so the two
    /// AAPCS rows can be compared on identical inputs.
    fn narrow_tail_inputs(convention: CallingConvention) -> ForeignCallInputs {
        let mut args = vec![ForeignArg::AggregateRegisters {
            value: CfgValue::from_raw(0),
            image: image(8),
        }];
        // Seven register-width scalars fill x1..x7 behind the aggregate in x0.
        args.extend((1..8).map(scalar));
        // The stacked tail: i8, i16, i32, i64.
        args.push(narrow_scalar(8, 1));
        args.push(narrow_scalar(9, 2));
        args.push(narrow_scalar(10, 4));
        args.push(narrow_scalar(11, 8));
        ForeignCallInputs {
            symbol: "narrow_tail".into(),
            convention,
            args,
            ret: ForeignReturn::ZeroSized,
        }
    }

    #[test]
    fn a_narrow_scalar_tail_takes_whole_slots_under_aapcs_and_packs_under_darwin() {
        let aapcs = ForeignCallPlan::new(narrow_tail_inputs(CallingConvention::Aarch64Aapcs), 8);
        // AAPCS64 gives every stacked argument its own 8-byte slot.
        assert_eq!(stack_offsets(&aapcs), vec![0, 8, 16, 24]);
        assert_eq!(aapcs.stack_area_bytes, 32);

        let darwin =
            ForeignCallPlan::new(narrow_tail_inputs(CallingConvention::Aarch64AapcsDarwin), 8);
        // Apple's amendment packs each argument at its natural size and
        // alignment: i8 at 0, i16 at 2 (aligned), i32 at 4, i64 at 8.
        assert_eq!(stack_offsets(&darwin), vec![0, 2, 4, 8]);
        // 16 packed bytes, already call-aligned.
        assert_eq!(darwin.stack_area_bytes, 16);
    }

    #[test]
    fn only_the_darwin_row_narrows_a_stacked_scalar_store() {
        let plan = ForeignCallPlan::new(narrow_tail_inputs(CallingConvention::Aarch64Aapcs), 8);
        let stores = plan.stack_stores(8, 0, &[VReg::new(1)]);
        assert_eq!(stores[0].bytes, 8, "AAPCS64 writes the whole 8-byte slot");

        let plan =
            ForeignCallPlan::new(narrow_tail_inputs(CallingConvention::Aarch64AapcsDarwin), 8);
        assert_eq!(plan.stack_stores(8, 0, &[VReg::new(1)])[0].bytes, 1);
        assert_eq!(plan.stack_stores(9, 2, &[VReg::new(1)])[0].bytes, 2);
        assert_eq!(plan.stack_stores(10, 4, &[VReg::new(1)])[0].bytes, 4);
        assert_eq!(plan.stack_stores(11, 8, &[VReg::new(1)])[0].bytes, 8);
    }

    #[test]
    fn x86_64_is_unaffected_by_the_apple_amendment() {
        // The same narrow tail on SysV: six integer registers, then whole
        // 8-byte slots for every stacked argument regardless of its C width.
        let mut args = vec![ForeignArg::AggregateRegisters {
            value: CfgValue::from_raw(0),
            image: image(8),
        }];
        args.extend((1..6).map(scalar));
        args.push(narrow_scalar(6, 1));
        args.push(narrow_scalar(7, 2));
        args.push(narrow_scalar(8, 4));
        let plan = ForeignCallPlan::new(
            ForeignCallInputs {
                symbol: "narrow_tail".into(),
                convention: CallingConvention::X86_64SysV,
                args,
                ret: ForeignReturn::ZeroSized,
            },
            6,
        );
        assert_eq!(stack_offsets(&plan), vec![0, 8, 16]);
        assert_eq!(plan.stack_area_bytes, 32);
        for arg_index in 6..9 {
            assert_eq!(plan.stack_stores(arg_index, 0, &[VReg::new(1)])[0].bytes, 8);
        }
    }

    #[test]
    fn a_stacked_composite_keeps_whole_eightbytes_under_every_row() {
        // A stacked byte followed by a 12-byte, 4-aligned composite: the byte
        // packs to one byte under Apple's rule, and the composite still takes
        // whole eightbytes at 8-byte alignment, so it starts at 8 and the
        // following scalar at 24. Every store offset stays a multiple of its
        // width, which is what keeps the AArch64 encodings in range.
        let args = (0..8)
            .map(scalar)
            .chain([
                narrow_scalar(8, 1),
                ForeignArg::AggregateRegisters {
                    value: CfgValue::from_raw(9),
                    image: image_aligned(12, 4),
                },
                narrow_scalar(10, 4),
            ])
            .collect();
        let plan = ForeignCallPlan::new(
            ForeignCallInputs {
                symbol: "composite_tail".into(),
                convention: CallingConvention::Aarch64AapcsDarwin,
                args,
                ret: ForeignReturn::ZeroSized,
            },
            8,
        );
        assert_eq!(stack_offsets(&plan), vec![0, 8, 24]);
        assert_eq!(plan.stack_area_bytes, 32);
        let stores = plan.stack_stores(9, 8, &[VReg::new(1), VReg::new(2)]);
        assert_eq!(stores.len(), 2);
        assert_eq!((stores[0].offset, stores[0].bytes), (8, 8));
        assert_eq!((stores[1].offset, stores[1].bytes), (16, 8));
    }

    #[test]
    fn every_stacked_store_offset_is_a_multiple_of_its_width() {
        // AArch64 addresses a stacked store through the scaled `imm12` form, so
        // an unaligned offset would be unencodable. The packing rule keeps the
        // invariant for both AAPCS rows.
        for convention in [
            CallingConvention::Aarch64Aapcs,
            CallingConvention::Aarch64AapcsDarwin,
        ] {
            let plan = ForeignCallPlan::new(narrow_tail_inputs(convention), 8);
            for (arg_index, placement) in plan.placements.iter().enumerate() {
                let ForeignArgPlacement::Stack { offset, .. } = placement else {
                    continue;
                };
                for store in plan.stack_stores(arg_index, *offset, &[VReg::new(1)]) {
                    assert_eq!(
                        store.offset % store.bytes,
                        0,
                        "{convention}: store at {} is not {}-aligned",
                        store.offset,
                        store.bytes
                    );
                }
            }
        }
    }

    #[test]
    fn placement_arithmetic_reports_budget_overflow_without_panicking() {
        let packing = StackedArgumentPacking::EightByteSlots;
        let byref_overflow = place_foreign_args(
            &[shape(0, u64::MAX, false), shape(0, 1, false)],
            0,
            false,
            packing,
        );
        assert!(byref_overflow.is_err());

        let stack_overflow = place_foreign_args(
            &[shape(u64::MAX, 0, false), shape(1, 0, false)],
            0,
            false,
            packing,
        );
        assert!(stack_overflow.is_err());

        let register_overflow = place_foreign_args(
            &[shape(u64::MAX, 0, true), shape(1, 0, true)],
            u64::MAX,
            false,
            packing,
        );
        assert!(register_overflow.is_err());
    }

    #[derive(Debug)]
    struct TraceBackend {
        convention: CallingConvention,
        register_count: usize,
        next_vreg: u32,
        events: Vec<String>,
    }

    impl TraceBackend {
        fn new(convention: CallingConvention, register_count: usize) -> Self {
            Self {
                convention,
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
        fn target_c_convention(&self) -> CallingConvention {
            self.convention
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

        fn foreign_emit_stack_args(&mut self, stack: &ForeignStackArea) {
            let stores = stack
                .stores
                .iter()
                .map(|store| format!("{}@{}+{}", store.value, store.offset, store.bytes))
                .collect::<Vec<_>>();
            self.record(format!("stack[{}]:{}", stack.bytes, stores.join(",")));
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

        fn foreign_cleanup_stack(&mut self, stack: &ForeignStackArea) {
            self.record(format!("cleanup_stack:{}", stack.bytes));
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
            convention: CallingConvention::X86_64SysV,
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
        let mut sysv = TraceBackend::new(CallingConvention::X86_64SysV, 6);
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
                "stack[16]:v106@0+8",
                "registers:[VReg(100), VReg(101), VReg(102), VReg(103), VReg(104), VReg(105)]",
                "call:sysv_fn",
                "cleanup_stack:16",
                "cleanup_byref:0",
                "sret_result:3",
                "move_primary",
            ]
        );

        let aapcs_inputs = ForeignCallInputs {
            symbol: "aapcs_fn".into(),
            convention: CallingConvention::Aarch64Aapcs,
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
        let mut aapcs = TraceBackend::new(CallingConvention::Aarch64Aapcs, 8);
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
                "stack[16]:v109@0+8,v110@8+8",
                "registers:[VReg(101), VReg(102), VReg(103), VReg(104), VReg(105), VReg(106), VReg(107), VReg(108)]",
                "assign_sret",
                "call:aapcs_fn",
                "cleanup_stack:16",
                "cleanup_byref:32",
                "sret_result:3",
                "move_primary",
            ]
        );

        let register_return_inputs = ForeignCallInputs {
            symbol: "aapcs_register_fn".into(),
            convention: CallingConvention::Aarch64Aapcs,
            args: Vec::new(),
            ret: ForeignReturn::AggregateRegisters { image: image(16) },
        };
        let mut register_return = TraceBackend::new(CallingConvention::Aarch64Aapcs, 8);
        let register_result =
            lower_foreign_call(&mut register_return, register_return_inputs, VReg::new(0));
        assert_eq!(register_result.slots.len(), 1);
        assert_eq!(
            register_return.events,
            vec![
                "stack[0]:",
                "registers:[]",
                "call:aapcs_register_fn",
                "cleanup_stack:0",
                "cleanup_byref:0",
                "register_result:2",
                "move_primary",
            ]
        );
    }

    #[test]
    fn the_darwin_row_reaches_the_backend_as_packed_stores() {
        let mut darwin = TraceBackend::new(CallingConvention::Aarch64AapcsDarwin, 8);
        lower_foreign_call(
            &mut darwin,
            narrow_tail_inputs(CallingConvention::Aarch64AapcsDarwin),
            VReg::new(0),
        );
        assert!(
            darwin
                .events
                .contains(&"stack[16]:v108@0+1,v109@2+2,v110@4+4,v111@8+8".to_string()),
            "packed Darwin stores must reach the backend leaf: {:?}",
            darwin.events
        );

        let mut linux = TraceBackend::new(CallingConvention::Aarch64Aapcs, 8);
        lower_foreign_call(
            &mut linux,
            narrow_tail_inputs(CallingConvention::Aarch64Aapcs),
            VReg::new(0),
        );
        assert!(
            linux
                .events
                .contains(&"stack[32]:v108@0+8,v109@8+8,v110@16+8,v111@24+8".to_string()),
            "AAPCS64 keeps whole 8-byte slots: {:?}",
            linux.events
        );
    }
}
