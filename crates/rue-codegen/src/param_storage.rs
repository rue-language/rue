//! Per-parameter storage planning (RUE-1170).
//!
//! Historically both backends stored every incoming ABI argument into a frame
//! slot at function entry, and the body re-loaded parameters from those homes.
//! Unused and read-only register arguments paid entry stores, frame growth,
//! and per-read reloads for nothing.
//!
//! This module decides, once per function and target, *where each parameter
//! arrives* — [`rue_air::lower_native_signature`]'s answer against the facts the
//! parameter's own type projects, which is the same answer every caller of that
//! signature computes (ADR-0084) — and where each of its ABI slots then lives:
//!
//! - **Frame**: the prologue homes the slot into the (compacted) frame
//!   parameter area, exactly as before. Everything that can address memory —
//!   aggregates, address-taken or writable by-value parameters, by-reference
//!   call arguments naming the parameter, stack-passed arguments, and raw CFG
//!   slot references into the parameter range — keeps a home.
//! - **Register-only**: the value (for a by-value scalar) or the pointer (for
//!   a by-reference parameter) arrives in one incoming argument register and
//!   is never stored. Lowering copies it into a virtual register in the entry
//!   preamble; the register allocator gives it a callee-saved register or an
//!   ordinary spill slot. An unused register argument produces no code and no
//!   frame slot at all.
//!
//! The plan is the single authority shared by frame accounting
//! (`codegen_pipeline`), both MIR lowerers, both emitters' prologues, and the
//! `--emit stackframe` reporter, so the frame layout and the code that
//! addresses it cannot drift apart.
//!
//! An aggregate whose leaves are not its eightbytes needs one more step: the
//! prologue lays the argument's eightbytes down as a contiguous frame image and
//! the body's entry unmarshal reads the leaves back out of it
//! ([`ParamStoragePlan::unmarshals`]), which is the exact inverse of the
//! marshaling the caller did.
//!
//! A CFG without grouped source-parameter descriptors (a directly constructed
//! CFG in synthetic tests) falls back to homing one register-width value per
//! slot.

use rue_air::{
    ArgConvention, ArgLocation, CAbiTypeFacts, FrozenTypeInternPool, LoweredReturn,
    PointerLocation, SLOT_BYTES, SourceParamAbi, lower_native_signature,
};
use rue_cfg::{Cfg, CfgArgMode, CfgInstData, PlaceBase};
use rue_target::{ConventionSpec, SretRegisterKind};

use crate::call_plan::{AbiRegisterBanks, AbiSlotClass, AbiSlotLocation};
use crate::codegen_pipeline::ParamHoming;
use crate::native_abi::{NativeArg, NativeArgMarshal, NativeImage, native_by_value_arg};

/// Where one parameter ABI slot lives inside the function.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ParamSlotStorage {
    /// Homed: the prologue stores this slot into the frame parameter area at
    /// `area_slot` (0-based within the area, after register-only slots are
    /// compacted away).
    Frame { area_slot: u32 },
    /// Register-only: the slot arrives in one incoming ABI argument register
    /// and has no frame home.
    Register {
        class: AbiSlotClass,
        location: AbiSlotLocation,
    },
}

/// Where the entry unmarshal of one parameter reads its compact image from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ImageSource {
    /// The parameter's own frame slots, into which the prologue copied the
    /// eightbytes the convention placed it in.
    FrameImage,
    /// A caller-owned copy, whose pointer the prologue homed into the
    /// parameter's first frame slot (AAPCS64 section 6.8.2 C.12).
    Pointer,
}

/// One by-value aggregate parameter whose leaves are not its eightbytes, and so
/// must be read back out of its compact image at function entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ParamUnmarshal {
    /// The parameter's first ABI slot, which names its frame home.
    pub(crate) param_slot: u32,
    /// How many frame slots past the parameter's first the image begins at.
    ///
    /// A frame-resident aggregate is laid out ascending in address with its
    /// logical slots (ADR-0040) while frame slot numbers descend in address, so
    /// its low end — byte 0 of the image — is its last slot. Zero for an image
    /// reached through a pointer, which addresses the copy directly.
    pub(crate) image_slot_offset: u32,
    /// Where the image lives.
    pub(crate) source: ImageSource,
    /// The image the leaves are read out of.
    pub(crate) image: NativeImage,
}

/// The complete per-function parameter storage decision.
#[derive(Debug, Clone)]
pub(crate) struct ParamStoragePlan {
    /// Per parameter ABI slot.
    slots: Vec<ParamSlotStorage>,
    /// Per parameter ABI slot: whether any `Param` instruction (or raw slot
    /// reference) names it. A register-only slot that is unused needs no
    /// entry copy.
    used: Vec<bool>,
    /// Frame slots the compacted parameter area occupies.
    homed_area_slots: u32,
    /// Prologue homing entries for the homed source parameters.
    homing: Vec<ParamHoming>,
    /// Entry unmarshals for the parameters whose leaves pack together.
    unmarshals: Vec<ParamUnmarshal>,
}

/// The already-decided return a callee's parameters are placed against: phase 1
/// of ADR-0084 switches arguments only, so all argument placement needs to know
/// is whether a hidden indirect-result pointer takes an ordinary argument
/// register ahead of every user parameter.
fn incoming_return(pairing: ConventionSpec, has_sret: bool) -> LoweredReturn {
    if !has_sret {
        return LoweredReturn::Void;
    }
    let spec = pairing.spec();
    LoweredReturn::Sret {
        register: spec.sret_register,
        echoed: spec.sret_pointer_echoed_in_result_register,
        size: SLOT_BYTES as u32,
        align: SLOT_BYTES as u32,
    }
}

/// The native description of one source parameter.
///
/// A by-reference parameter is one pointer whatever it points at. A by-value
/// parameter with no recovered type is one register-width slot, which is what
/// the direct-slot cleanup convention (destructors and drop glue) presents for
/// every one of an aggregate's already-flattened leaves, and what a synthetic
/// CFG without descriptors presents for each of its slots.
fn parameter_native(
    cfg: &Cfg,
    type_pool: &FrozenTypeInternPool,
    descriptor: &SourceParamAbi,
) -> (NativeArg, ArgConvention) {
    if cfg.is_param_by_ref(descriptor.start_slot) {
        return (
            NativeArg::Scalar {
                kind: rue_air::CAbiScalarKind::RegisterWidth,
                class: AbiSlotClass::Gp,
            },
            ArgConvention::ByReference,
        );
    }
    match descriptor.ty {
        Some(ty) => (native_by_value_arg(type_pool, ty), ArgConvention::ByValue),
        // A typeless by-value descriptor spans already-flattened leaves: the
        // cleanup convention's parameters, whose caller
        // (`CallPlan::from_slot_values`) hands each leaf its own register-width
        // placement.
        None => (
            NativeArg::PerLeaf {
                count: descriptor.slot_count.max(1),
            },
            ArgConvention::ByValue,
        ),
    }
}

/// The prologue copies one parameter's incoming eightbytes make, and the entry
/// unmarshal it needs, given the placement the convention gave it.
///
/// `area_slot` is the parameter's first slot in the compacted frame parameter
/// area. Every shape reduces to the same two steps: lay the argument's
/// eightbytes down as a contiguous frame image, then — when the leaves are not
/// those eightbytes — read the leaves back out of it. A by-reference copy is
/// the one shape whose image is elsewhere: the prologue homes its pointer and
/// the unmarshal reads through it.
fn parameter_homing(
    native: &NativeArg,
    placements: &[ArgLocation],
    param_slot: u32,
    area_slot: u32,
    slot_count: u32,
) -> (Vec<ParamHoming>, Option<ParamUnmarshal>) {
    if let NativeArg::PerLeaf { .. } = native {
        // Each already-flattened leaf arrives in its own register-width
        // placement and homes into its own frame slot.
        return (
            placements
                .iter()
                .enumerate()
                .map(|(index, placement)| ParamHoming {
                    start_slot: area_slot + index as u32,
                    class: AbiSlotClass::Gp,
                    location: match *placement {
                        ArgLocation::Registers { pieces } => {
                            AbiSlotLocation::GpReg(pieces.as_slice()[0].index as usize)
                        }
                        ArgLocation::Stack {
                            offset,
                            size,
                            align,
                        } => AbiSlotLocation::Stack {
                            offset,
                            size,
                            align,
                        },
                        ArgLocation::Omitted | ArgLocation::Indirect { .. } => {
                            panic!("a register-width leaf is never omitted or indirect")
                        }
                    },
                    narrow_stack_load: None,
                })
                .collect(),
            None,
        );
    }
    let location = placements[0];
    let marshal = native.marshal(location);
    let image = match native {
        NativeArg::Aggregate { image } => Some(image.clone()),
        _ => None,
    };
    match location {
        ArgLocation::Omitted => (Vec::new(), None),
        ArgLocation::Indirect { pointer, .. } => (
            vec![ParamHoming {
                start_slot: area_slot,
                class: AbiSlotClass::Gp,
                location: match pointer {
                    PointerLocation::Register { index } => AbiSlotLocation::GpReg(index as usize),
                    PointerLocation::Stack { offset } => AbiSlotLocation::Stack {
                        offset,
                        size: SLOT_BYTES as u32,
                        align: SLOT_BYTES as u32,
                    },
                },
                narrow_stack_load: None,
            }],
            image.map(|image| ParamUnmarshal {
                param_slot,
                image_slot_offset: 0,
                source: ImageSource::Pointer,
                image,
            }),
        ),
        ArgLocation::Registers { pieces } => {
            let homing = pieces
                .as_slice()
                .iter()
                .enumerate()
                .map(|(index, piece)| ParamHoming {
                    start_slot: eightbyte_home_slot(area_slot, slot_count, index),
                    class: marshal.class(index, piece.class),
                    location: match piece.class {
                        rue_target::CRegisterClass::Gp => {
                            AbiSlotLocation::GpReg(piece.index as usize)
                        }
                        rue_target::CRegisterClass::Fp => {
                            AbiSlotLocation::FpReg(piece.index as usize)
                        }
                    },
                    narrow_stack_load: None,
                })
                .collect();
            (
                homing,
                packed_unmarshal(&marshal, image, param_slot, slot_count),
            )
        }
        ArgLocation::Stack {
            offset,
            size,
            align,
        } => {
            if native.packs_as_scalar() {
                // Apple's amendment stacks a scalar at its own width, so the
                // copy into the frame slot re-extends it to Rue's canonical
                // 64-bit form.
                let narrow = (size < SLOT_BYTES as u32).then(|| crate::types::NarrowScalar {
                    width: size as u8,
                    signed: matches!(
                        native.extension(),
                        rue_air::ScalarAbiExtension::Signed { .. }
                    ),
                });
                return (
                    vec![ParamHoming {
                        start_slot: area_slot,
                        class: marshal.class(0, marshal.stacked_bank(0)),
                        location: AbiSlotLocation::Stack {
                            offset,
                            size,
                            align,
                        },
                        narrow_stack_load: narrow,
                    }],
                    None,
                );
            }
            let homing = (0..marshal.eightbyte_count())
                .map(|index| ParamHoming {
                    start_slot: eightbyte_home_slot(area_slot, slot_count, index),
                    class: marshal.class(index, marshal.stacked_bank(index)),
                    location: AbiSlotLocation::Stack {
                        offset: offset + (index as u32) * SLOT_BYTES as u32,
                        size: SLOT_BYTES as u32,
                        align: SLOT_BYTES as u32,
                    },
                    narrow_stack_load: None,
                })
                .collect();
            (
                homing,
                packed_unmarshal(&marshal, image, param_slot, slot_count),
            )
        }
    }
}

/// The entry unmarshal a register- or stack-placed aggregate needs: none when
/// its leaves are its eightbytes, and a read of the frame image otherwise.
fn packed_unmarshal(
    marshal: &NativeArgMarshal,
    image: Option<NativeImage>,
    param_slot: u32,
    slot_count: u32,
) -> Option<ParamUnmarshal> {
    match marshal {
        NativeArgMarshal::Direct { .. } => None,
        NativeArgMarshal::Image { .. } => Some(ParamUnmarshal {
            param_slot,
            image_slot_offset: slot_count.saturating_sub(1),
            source: ImageSource::FrameImage,
            image: image.expect("only an aggregate marshals through an image"),
        }),
    }
}

/// The frame slot eightbyte `index` of an incoming value lands in.
///
/// A frame-resident aggregate is laid out ascending in address with its logical
/// slots (ADR-0040) while frame slot numbers descend in address, so the
/// parameter's low end is its last slot and eightbyte `index` — which is
/// `index * 8` bytes into the value — lands `index` slots back from there. That
/// holds whether the eightbytes are the value's own leaves or the image its
/// leaves are read out of, so the prologue lays down one ascending copy either
/// way.
const fn eightbyte_home_slot(area_slot: u32, slot_count: u32, index: usize) -> u32 {
    area_slot + slot_count.saturating_sub(1) - index as u32
}

impl ParamStoragePlan {
    /// The plan for a CFG with no grouped source-parameter descriptors: every
    /// parameter ABI slot is one register-width value homed at its own index.
    /// This is the fallback for synthetic CFGs, and the baseline the planned
    /// path shrinks from.
    pub(crate) fn all_homed(
        num_params: u32,
        has_sret: bool,
        native_convention: ConventionSpec,
        register_banks: AbiRegisterBanks,
    ) -> Self {
        let native = NativeArg::Scalar {
            kind: rue_air::CAbiScalarKind::RegisterWidth,
            class: AbiSlotClass::Gp,
        };
        let parameters = vec![(native.facts()[0], ArgConvention::ByValue); num_params as usize];
        let signature = lower_native_signature(
            native_convention,
            &parameters,
            incoming_return(native_convention, has_sret),
        );
        let _ = register_banks;
        let mut homing = Vec::new();
        for (slot, argument) in signature.arguments().iter().enumerate() {
            let (entries, _) =
                parameter_homing(&native, &[argument.location], slot as u32, slot as u32, 1);
            homing.extend(entries);
        }
        Self {
            slots: (0..num_params)
                .map(|slot| ParamSlotStorage::Frame { area_slot: slot })
                .collect(),
            used: vec![true; num_params as usize],
            homed_area_slots: num_params,
            homing,
            unmarshals: Vec::new(),
        }
    }

    /// Decide storage for every parameter of `cfg`.
    ///
    /// Where each parameter's incoming value arrives is
    /// [`lower_native_signature`]'s answer against the facts the parameter's
    /// own type projects — the same answer every caller of this function
    /// computes for the same signature (ADR-0084).
    pub(crate) fn plan(
        cfg: &Cfg,
        type_pool: &FrozenTypeInternPool,
        has_sret: bool,
        native_convention: ConventionSpec,
        register_banks: impl Into<AbiRegisterBanks>,
    ) -> Self {
        let register_banks = register_banks.into();
        let num_params = cfg.num_params();
        let descriptors = cfg.source_param_abi();
        if descriptors.is_empty() {
            return Self::all_homed(num_params, has_sret, native_convention, register_banks);
        }
        // Defensive: the descriptors must tile the parameter area exactly;
        // anything else falls back to the historical all-homed plan rather
        // than planning against inconsistent metadata.
        let covered: u32 = descriptors.iter().map(|d| d.slot_count).sum();
        if covered != num_params
            || descriptors
                .iter()
                .zip(descriptors.iter().skip(1))
                .any(|(a, b)| a.start_slot + a.slot_count != b.start_slot)
            || descriptors.first().is_some_and(|d| d.start_slot != 0)
        {
            return Self::all_homed(num_params, has_sret, native_convention, register_banks);
        }

        let natives: Vec<(NativeArg, ArgConvention)> = descriptors
            .iter()
            .map(|descriptor| parameter_native(cfg, type_pool, descriptor))
            .collect();
        // Every parameter contributes one placement, except the cleanup
        // convention's already-flattened leaves, which contribute one each; the
        // spans map a parameter back to the run of placements it owns.
        let mut parameters: Vec<(CAbiTypeFacts, ArgConvention)> = Vec::new();
        let mut spans = Vec::with_capacity(natives.len());
        for (native, mode) in &natives {
            let start = parameters.len();
            parameters.extend(native.facts().into_iter().map(|facts| (facts, *mode)));
            spans.push(start..parameters.len());
        }
        let signature = lower_native_signature(
            native_convention,
            &parameters,
            incoming_return(native_convention, has_sret),
        );
        if has_sret {
            assert_eq!(
                signature.spec().sret_register,
                SretRegisterKind::ArgumentRegister,
                "the native convention takes its indirect-result pointer in the \
                 first ordinary argument register"
            );
        }

        let scan = scan_param_references(cfg, type_pool);
        let mut slots = Vec::with_capacity(num_params as usize);
        let mut homing = Vec::new();
        let mut unmarshals = Vec::new();
        let mut area = 0u32;
        for ((descriptor, (native, _)), span) in descriptors.iter().zip(&natives).zip(spans) {
            let slot = descriptor.start_slot;
            let placements = signature.arguments()[span]
                .iter()
                .map(|argument| argument.location)
                .collect::<Vec<_>>();
            let (entries, unmarshal) =
                parameter_homing(native, &placements, slot, area, descriptor.slot_count);
            // A register-only parameter must be a single slot arriving in a
            // single register that actually is a register (not stack-passed)
            // and needing no image. A by-reference parameter's slot holds only
            // the incoming pointer, which nothing may address (every consumer
            // goes through `ensure_by_ref_param_ptr`); a by-value slot must
            // additionally be immutable and never addressed.
            let eligible = descriptor.slot_count == 1
                && unmarshal.is_none()
                && entries.len() == 1
                && !matches!(entries[0].location, AbiSlotLocation::Stack { .. })
                && !scan.needs_home[slot as usize]
                && (cfg.is_param_by_ref(slot)
                    || (!cfg.is_param_writable(slot) && !cfg.is_param_address_taken(slot)));
            if eligible {
                slots.push(ParamSlotStorage::Register {
                    class: entries[0].class,
                    location: entries[0].location,
                });
                continue;
            }
            for j in 0..descriptor.slot_count {
                slots.push(ParamSlotStorage::Frame {
                    area_slot: area + j,
                });
            }
            homing.extend(entries);
            unmarshals.extend(unmarshal);
            area += descriptor.slot_count;
        }
        Self {
            slots,
            used: scan.used,
            homed_area_slots: area,
            homing,
            unmarshals,
        }
    }

    /// Storage decision for parameter ABI slot `index`.
    #[cfg(test)]
    pub(crate) fn slot(&self, index: u32) -> ParamSlotStorage {
        self.slots[index as usize]
    }

    /// The compacted parameter-area position of slot `index`, if homed.
    pub(crate) fn area_slot(&self, index: u32) -> Option<u32> {
        match self.slots[index as usize] {
            ParamSlotStorage::Frame { area_slot } => Some(area_slot),
            ParamSlotStorage::Register { .. } => None,
        }
    }

    /// Whether any CFG instruction references parameter ABI slot `index`.
    #[cfg(test)]
    pub(crate) fn is_used(&self, index: u32) -> bool {
        self.used[index as usize]
    }

    /// Frame slots the compacted parameter area occupies.
    pub(crate) fn homed_area_slots(&self) -> u32 {
        self.homed_area_slots
    }

    /// Prologue homing entries for the homed source parameters.
    pub(crate) fn homing(&self) -> &[ParamHoming] {
        &self.homing
    }

    /// Entry unmarshals for the parameters whose leaves are not their
    /// eightbytes.
    pub(crate) fn unmarshals(&self) -> &[ParamUnmarshal] {
        &self.unmarshals
    }

    /// Register-only parameter slots needing an entry copy, as
    /// `(param ABI slot, class, location)`.
    ///
    /// A by-reference pointer is copied whether or not it is used, mirroring
    /// the unconditional by-ref preload of the homed path; a by-value slot is
    /// copied only when a `Param` instruction reads it.
    pub(crate) fn entry_copies<'a>(
        &'a self,
        cfg: &'a Cfg,
    ) -> impl Iterator<Item = (u32, AbiSlotClass, AbiSlotLocation)> + 'a {
        self.slots
            .iter()
            .enumerate()
            .filter_map(move |(slot, storage)| {
                let slot = slot as u32;
                match storage {
                    ParamSlotStorage::Register { class, location }
                        if cfg.is_param_by_ref(slot) || self.used[slot as usize] =>
                    {
                        Some((slot, *class, *location))
                    }
                    _ => None,
                }
            })
    }
}

struct ParamReferenceScan {
    /// Slot requires a frame home for a reason visible only in the
    /// instruction stream (writes, address exposure, raw slot references).
    needs_home: Vec<bool>,
    /// Slot is referenced at all.
    used: Vec<bool>,
}

/// Scan the CFG for the per-slot facts eligibility needs beyond the static
/// parameter metadata: actual writes, address-exposing uses, and raw frame
/// slot references into the parameter range.
///
/// A reference rooted at a parameter's first ABI slot can span the
/// parameter's whole type — a multi-slot `Param` read, a place access, or a
/// by-value `ParamStore` addresses `[index, index + slot_count)` as one
/// contiguous frame region. Under the direct-slot cleanup convention
/// (destructors and drop glue) those slots are described by *separate*
/// single-slot descriptors, so every mark covers the referenced type's full
/// slot span, not just the base slot.
fn scan_param_references(cfg: &Cfg, type_pool: &FrozenTypeInternPool) -> ParamReferenceScan {
    let num_params = cfg.num_params() as usize;
    let num_locals = cfg.num_locals();
    let mut needs_home = vec![false; num_params];
    let mut used = vec![false; num_params];

    let param_of_slot = |slot: u32| -> Option<u32> {
        (slot >= num_locals && slot < num_locals + num_params as u32).then(|| slot - num_locals)
    };
    let mark_span = |v: &mut Vec<bool>, index: u32, span: u32| {
        for offset in 0..span {
            if let Some(flag) = v.get_mut((index + offset) as usize) {
                *flag = true;
            }
        }
    };
    let span_of = |ty: rue_cfg::Type| crate::types::type_slot_span(type_pool, ty);

    for block in cfg.blocks() {
        for &value in &block.insts {
            let inst = cfg.get_inst(value);
            match &inst.data {
                CfgInstData::Param { index } => {
                    let span = if cfg.is_param_by_ref(*index) {
                        1
                    } else {
                        span_of(inst.ty)
                    };
                    mark_span(&mut used, *index, span);
                    // A multi-slot by-value read loads the value out of its
                    // contiguous frame region, so the whole span must be
                    // homed even when the cleanup convention split it into
                    // single-slot descriptors. A scalar read consumes the
                    // register copy and needs no home.
                    if !cfg.is_param_by_ref(*index) && span > 1 {
                        mark_span(&mut needs_home, *index, span);
                    }
                }
                CfgInstData::ParamStore { param_slot, value } => {
                    let span = if cfg.is_param_by_ref(*param_slot) {
                        1
                    } else {
                        span_of(cfg.get_inst(*value).ty)
                    };
                    mark_span(&mut used, *param_slot, span);
                    // A by-ref store goes through the incoming pointer; a
                    // by-value store (a `mut self` receiver) writes the home.
                    if !cfg.is_param_by_ref(*param_slot) {
                        mark_span(&mut needs_home, *param_slot, span);
                    }
                }
                CfgInstData::PlaceRead { place } | CfgInstData::PlaceWrite { place, .. } => {
                    match place.base {
                        PlaceBase::Param(slot) => {
                            // Place access to a by-value parameter addresses
                            // its home region (rooted at the base slot and
                            // spanning the base type); by-ref access goes
                            // through the pointer.
                            if cfg.is_param_by_ref(slot) {
                                mark_span(&mut used, slot, 1);
                            } else {
                                let span = span_of(place.base_type);
                                mark_span(&mut used, slot, span);
                                mark_span(&mut needs_home, slot, span);
                            }
                        }
                        PlaceBase::Local(base_slot) => {
                            if let Some(index) = param_of_slot(base_slot) {
                                let span = span_of(place.base_type);
                                mark_span(&mut used, index, span);
                                mark_span(&mut needs_home, index, span);
                            }
                        }
                        PlaceBase::Accessor(_) => {
                            panic!("mandatory-inline accessor place reached codegen")
                        }
                        // The pointer producer is an ordinary SSA value. Any
                        // parameter storage it depends on is recorded by that
                        // producer's own operands; the indirect base itself
                        // names no parameter slot or home region.
                        PlaceBase::Indirect(_) => {}
                    }
                }
                CfgInstData::Alloc { slot, .. }
                | CfgInstData::Load { slot }
                | CfgInstData::StorageLive { slot, .. }
                | CfgInstData::StorageDead { slot, .. } => {
                    if let Some(index) = param_of_slot(*slot) {
                        let span = span_of(inst.ty);
                        mark_span(&mut used, index, span);
                        mark_span(&mut needs_home, index, span);
                    }
                }
                CfgInstData::Store { slot, value } => {
                    if let Some(index) = param_of_slot(*slot) {
                        // A store to a by-ref parameter slot is redirected
                        // through the incoming pointer (`store_destination`);
                        // a by-value one writes the home region.
                        if cfg.is_param_by_ref(index) {
                            mark_span(&mut used, index, 1);
                        } else {
                            let span = span_of(cfg.get_inst(*value).ty);
                            mark_span(&mut used, index, span);
                            mark_span(&mut needs_home, index, span);
                        }
                    }
                }
                data @ CfgInstData::Call { .. } => {
                    // A by-reference call argument forwards the address of
                    // its operand. When that operand is a by-value parameter,
                    // `byref_args` takes the address of its frame home.
                    for arg in cfg.get_call_args(data) {
                        if arg.mode == CfgArgMode::Normal {
                            continue;
                        }
                        if let CfgInstData::Param { index } = &cfg.get_inst(arg.value).data {
                            let arg_inst = cfg.get_inst(arg.value);
                            if cfg.is_param_by_ref(*index) {
                                mark_span(&mut used, *index, 1);
                            } else {
                                let span = span_of(arg_inst.ty);
                                mark_span(&mut used, *index, span);
                                mark_span(&mut needs_home, *index, span);
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }

    ParamReferenceScan { needs_home, used }
}

#[cfg(test)]
mod tests {
    use lasso::Spur;
    use rue_air::{SourceParamAbi, Type, TypeInternPool};
    use rue_cfg::{Cfg, CfgArgMode, CfgCallArg, CfgInst, CfgInstData};
    use rue_span::Span;

    use super::{ParamSlotStorage, ParamStoragePlan};

    /// The two native pairings a plan is exercised under: the plan's stacked
    /// slots are packed by the convention, so a test names the row whose
    /// argument roster it is filling.
    const SYSV: rue_target::ConventionSpec =
        rue_target::ConventionSpec::native(rue_target::Target::X86_64Linux);
    const AAPCS: rue_target::ConventionSpec =
        rue_target::ConventionSpec::native(rue_target::Target::Aarch64Linux);

    fn pool() -> rue_air::FrozenTypeInternPool {
        TypeInternPool::new().freeze()
    }

    fn scalar_descriptors(count: u32) -> Vec<SourceParamAbi> {
        (0..count)
            .map(|slot| SourceParamAbi {
                start_slot: slot,
                slot_count: 1,
                ty: None,
            })
            .collect()
    }

    fn push_param(cfg: &mut Cfg, block: rue_cfg::BlockId, index: u32) -> rue_cfg::CfgValue {
        cfg.append_inst(
            block,
            CfgInst {
                data: CfgInstData::Param { index },
                ty: Type::I64,
                span: Span::default(),
            },
        )
    }

    #[test]
    fn synthetic_cfg_without_descriptors_homes_every_slot() {
        let cfg = Cfg::new(Type::UNIT, 1, 2, "synthetic".into(), vec![false, false]);
        let plan = ParamStoragePlan::plan(&cfg, &pool(), false, SYSV, 6);
        assert_eq!(plan.homed_area_slots(), 2);
        assert_eq!(plan.slot(0), ParamSlotStorage::Frame { area_slot: 0 });
        assert_eq!(plan.slot(1), ParamSlotStorage::Frame { area_slot: 1 });
        assert_eq!(plan.homing().len(), 2);
        assert_eq!(
            plan.homing()[1].location,
            crate::call_plan::AbiSlotLocation::GpReg(1)
        );
    }

    #[test]
    fn descriptorless_fallback_uses_target_gp_bank_width() {
        let cfg = Cfg::new(Type::UNIT, 1, 8, "synthetic-arm".into(), vec![false; 8]);
        let arm = ParamStoragePlan::plan(
            &cfg,
            &pool(),
            false,
            AAPCS,
            crate::call_plan::AbiRegisterBanks { gp: 8, fp: 8 },
        );
        assert_eq!(arm.homing().len(), 8);
        assert_eq!(
            arm.homing()[6].location,
            crate::call_plan::AbiSlotLocation::GpReg(6)
        );
        assert_eq!(
            arm.homing()[7].location,
            crate::call_plan::AbiSlotLocation::GpReg(7)
        );

        let x86 = ParamStoragePlan::plan(
            &cfg,
            &pool(),
            false,
            SYSV,
            crate::call_plan::AbiRegisterBanks { gp: 6, fp: 8 },
        );
        assert_eq!(
            x86.homing()[6].location,
            crate::call_plan::AbiSlotLocation::stack_slot(0)
        );
        assert_eq!(
            x86.homing()[7].location,
            crate::call_plan::AbiSlotLocation::stack_slot(1)
        );
    }

    #[test]
    fn unused_and_read_only_register_params_lose_their_homes() {
        let mut cfg = Cfg::new(Type::I64, 0, 2, "reads".into(), vec![false, false]);
        cfg.set_source_param_abi(scalar_descriptors(2));
        let entry = cfg.new_block();
        cfg.entry = entry;
        // Param 0 is read; param 1 is never referenced.
        push_param(&mut cfg, entry, 0);

        let plan = ParamStoragePlan::plan(&cfg, &pool(), false, SYSV, 6);
        assert!(matches!(plan.slot(0), ParamSlotStorage::Register { .. }));
        assert!(matches!(plan.slot(1), ParamSlotStorage::Register { .. }));
        assert!(plan.is_used(0));
        assert!(!plan.is_used(1));
        assert_eq!(plan.homed_area_slots(), 0);
        assert!(plan.homing().is_empty());
        assert_eq!(
            plan.entry_copies(&cfg).next().unwrap().2,
            crate::call_plan::AbiSlotLocation::GpReg(0)
        );
    }

    #[test]
    fn sret_shifts_register_indices() {
        let mut cfg = Cfg::new(Type::I64, 0, 1, "sret".into(), vec![false]);
        cfg.set_source_param_abi(scalar_descriptors(1));
        let entry = cfg.new_block();
        cfg.entry = entry;
        push_param(&mut cfg, entry, 0);

        let plan = ParamStoragePlan::plan(&cfg, &pool(), true, SYSV, 6);
        assert!(matches!(
            plan.slot(0),
            ParamSlotStorage::Register {
                location: crate::call_plan::AbiSlotLocation::GpReg(1),
                ..
            }
        ));
    }

    #[test]
    fn stack_passed_params_keep_their_homes() {
        let count = 8u32;
        let mut cfg = Cfg::new(
            Type::I64,
            0,
            count,
            "stack".into(),
            vec![false; count as usize],
        );
        cfg.set_source_param_abi(scalar_descriptors(count));
        let entry = cfg.new_block();
        cfg.entry = entry;
        for index in 0..count {
            push_param(&mut cfg, entry, index);
        }

        // Six argument registers: slots 6 and 7 are stack-passed.
        let plan = ParamStoragePlan::plan(&cfg, &pool(), false, SYSV, 6);
        for slot in 0..6 {
            assert!(matches!(plan.slot(slot), ParamSlotStorage::Register { .. }));
        }
        assert_eq!(plan.slot(6), ParamSlotStorage::Frame { area_slot: 0 });
        assert_eq!(plan.slot(7), ParamSlotStorage::Frame { area_slot: 1 });
        assert_eq!(plan.homed_area_slots(), 2);
        // The homed entries still name their original incoming ABI indices.
        assert_eq!(
            plan.homing()[0].location,
            crate::call_plan::AbiSlotLocation::stack_slot(0)
        );
        assert_eq!(
            plan.homing()[1].location,
            crate::call_plan::AbiSlotLocation::stack_slot(1)
        );

        // With eight argument registers (AArch64) everything is register-only.
        let plan = ParamStoragePlan::plan(&cfg, &pool(), false, AAPCS, 8);
        assert_eq!(plan.homed_area_slots(), 0);
    }

    #[test]
    fn writes_and_byref_call_arguments_force_homes() {
        let mut cfg = Cfg::new(Type::I64, 0, 3, "writes".into(), vec![false, false, false]);
        cfg.set_source_param_abi(scalar_descriptors(3));
        let entry = cfg.new_block();
        cfg.entry = entry;
        // Param 0: written via ParamStore (a by-value `mut self` shape).
        let value = cfg.append_inst(
            entry,
            CfgInst {
                data: CfgInstData::Const(1),
                ty: Type::I64,
                span: Span::default(),
            },
        );
        cfg.append_inst(
            entry,
            CfgInst {
                data: CfgInstData::ParamStore {
                    param_slot: 0,
                    value,
                },
                ty: Type::UNIT,
                span: Span::default(),
            },
        );
        // Param 1: forwarded as a by-reference call argument.
        let param1 = push_param(&mut cfg, entry, 1);
        cfg.append_call(
            entry,
            None,
            Spur::default(),
            vec![CfgCallArg {
                value: param1,
                mode: CfgArgMode::Inout,
            }],
            Type::UNIT,
            Span::default(),
        )
        .unwrap();
        // Param 2: plain read.
        push_param(&mut cfg, entry, 2);

        let plan = ParamStoragePlan::plan(&cfg, &pool(), false, SYSV, 6);
        assert_eq!(plan.slot(0), ParamSlotStorage::Frame { area_slot: 0 });
        assert_eq!(plan.slot(1), ParamSlotStorage::Frame { area_slot: 1 });
        assert!(matches!(
            plan.slot(2),
            ParamSlotStorage::Register {
                location: crate::call_plan::AbiSlotLocation::GpReg(2),
                ..
            }
        ));
        assert_eq!(plan.homed_area_slots(), 2);
    }

    #[test]
    fn address_taken_and_writable_by_value_params_keep_homes() {
        let mut cfg = rue_cfg::Cfg::new(
            Type::I64,
            0,
            2,
            "flags".into(),
            rue_air::ParamSlotModes::new(vec![false, false], vec![false, true]),
        );
        cfg.set_source_param_abi(scalar_descriptors(2));
        cfg.mark_param_address_taken(0);
        let entry = cfg.new_block();
        cfg.entry = entry;
        push_param(&mut cfg, entry, 0);
        push_param(&mut cfg, entry, 1);

        let plan = ParamStoragePlan::plan(&cfg, &pool(), false, SYSV, 6);
        assert_eq!(plan.slot(0), ParamSlotStorage::Frame { area_slot: 0 });
        assert_eq!(plan.slot(1), ParamSlotStorage::Frame { area_slot: 1 });
    }

    #[test]
    fn by_ref_pointer_params_are_register_only_even_when_writable() {
        // An inout parameter: by-ref and writable. The write goes through the
        // pointer, so the pointer itself can stay in a register.
        let mut cfg = rue_cfg::Cfg::new(
            Type::UNIT,
            0,
            1,
            "inout".into(),
            rue_air::ParamSlotModes::new(vec![true], vec![true]),
        );
        cfg.set_source_param_abi(scalar_descriptors(1));
        let entry = cfg.new_block();
        cfg.entry = entry;
        let value = cfg.append_inst(
            entry,
            CfgInst {
                data: CfgInstData::Const(7),
                ty: Type::I64,
                span: Span::default(),
            },
        );
        cfg.append_inst(
            entry,
            CfgInst {
                data: CfgInstData::ParamStore {
                    param_slot: 0,
                    value,
                },
                ty: Type::UNIT,
                span: Span::default(),
            },
        );

        let plan = ParamStoragePlan::plan(&cfg, &pool(), false, SYSV, 6);
        assert!(matches!(
            plan.slot(0),
            ParamSlotStorage::Register {
                location: crate::call_plan::AbiSlotLocation::GpReg(0),
                ..
            }
        ));
        // The pointer is copied at entry whether or not a Param inst exists.
        assert_eq!(
            plan.entry_copies(&cfg).next().unwrap().2,
            crate::call_plan::AbiSlotLocation::GpReg(0)
        );
    }

    /// The direct-slot cleanup convention (destructors, drop glue) describes
    /// one source aggregate as SEPARATE single-slot descriptors. A place or
    /// multi-slot `Param` reference rooted at the first slot addresses the
    /// whole contiguous region, so the scan must home the full type span —
    /// homing only the base slot leaves the tail slots reading garbage (the
    /// RUE-1170 ArrayBuf destructor regression).
    #[test]
    fn references_home_the_full_type_span_across_split_descriptors() {
        let type_pool = TypeInternPool::new();
        let interner = lasso::ThreadedRodeo::new();
        let (struct_id, _) = type_pool.register_struct(
            interner.get_or_intern("PairLike"),
            rue_air::StructDef {
                name: "PairLike".into(),
                fields: vec![
                    rue_air::StructField {
                        name: "a".to_string(),
                        ty: Type::I64,
                    },
                    rue_air::StructField {
                        name: "b".to_string(),
                        ty: Type::I64,
                    },
                ],
                is_copy: true,
                is_linear: false,
                declared_linear: false,
                destructor: None,
                is_builtin: false,
                is_pub: false,
                file_id: rue_span::FileId::DEFAULT,
            },
        );
        let pair_ty = Type::new_struct(struct_id);
        let pool = type_pool.freeze();

        let mut cfg = Cfg::new(Type::UNIT, 0, 2, "split".into(), vec![false, false]);
        // Two single-slot descriptors for one two-slot source value, the
        // direct-slot cleanup shape.
        cfg.set_source_param_abi(scalar_descriptors(2));
        let entry = cfg.new_block();
        cfg.entry = entry;
        // One whole-value read of the two-slot aggregate rooted at slot 0.
        cfg.append_inst(
            entry,
            CfgInst {
                data: CfgInstData::Param { index: 0 },
                ty: pair_ty,
                span: Span::default(),
            },
        );

        let plan = ParamStoragePlan::plan(&cfg, &pool, false, SYSV, 6);
        assert_eq!(plan.slot(0), ParamSlotStorage::Frame { area_slot: 0 });
        assert_eq!(
            plan.slot(1),
            ParamSlotStorage::Frame { area_slot: 1 },
            "the read's type span must home the tail slot too"
        );
        assert_eq!(plan.homed_area_slots(), 2);
    }

    #[test]
    fn aggregates_and_raw_slot_references_keep_homes() {
        // Param 0 is a two-slot aggregate; param slot 2 is referenced as a
        // raw frame slot (num_locals + 2).
        let mut cfg = Cfg::new(Type::I64, 1, 3, "agg".into(), vec![false, false, false]);
        cfg.set_source_param_abi(vec![
            SourceParamAbi {
                start_slot: 0,
                slot_count: 2,
                ty: None,
            },
            SourceParamAbi {
                start_slot: 2,
                slot_count: 1,
                ty: None,
            },
        ]);
        let entry = cfg.new_block();
        cfg.entry = entry;
        cfg.append_inst(
            entry,
            CfgInst {
                data: CfgInstData::Load { slot: 1 + 2 },
                ty: Type::I64,
                span: Span::default(),
            },
        );

        let plan = ParamStoragePlan::plan(&cfg, &pool(), false, SYSV, 6);
        assert_eq!(plan.slot(0), ParamSlotStorage::Frame { area_slot: 0 });
        assert_eq!(plan.slot(1), ParamSlotStorage::Frame { area_slot: 1 });
        assert_eq!(plan.slot(2), ParamSlotStorage::Frame { area_slot: 2 });
        assert_eq!(plan.homed_area_slots(), 3);
        assert_eq!(plan.homing().len(), 3);
    }
}
