//! Rue-to-C export thunks (ADR-0064 P4).
//!
//! The mirror of the foreign-call path. A foreign *call* adapts the native
//! convention to the target-C convention on the way *out* to a C callee; an
//! *export* thunk adapts the target-C convention to the native convention on the
//! way *in* from a C caller. A `pub extern "C" fn` is compiled like any other
//! Rue function (a native-conventioned body under a mangled symbol); this module
//! emits an additional, globally-visible, **unmangled** entry symbol — the C
//! symbol — whose body receives arguments per the psABI and forwards to the
//! native body.
//!
//! ## One lowered signature, read in the callee direction
//!
//! The thunk reads the same [`LoweredSignature`] the import path writes: the
//! C caller has already put every argument where [`lower_c_signature`] says it
//! goes, so the thunk finds each one there. That is what makes an import of a
//! signature and an export of the same signature agree by construction, which
//! is ADR-0064's ratified acceptance criterion.
//!
//! The native side is placed by the *same* function, against the same facts:
//! the native convention is the compilation target's C convention with a wider
//! return bank (ADR-0084), so [`rue_air::lower_native_signature`] answers where
//! the native body expects each argument, exactly as the callee's own parameter
//! plan (`crate::param_storage`) asks it, and [`rue_air::lower_native_return`]
//! answers where its result comes back. The two conventions therefore agree
//! about every argument whenever the export names the target's own C row, and —
//! because C's result registers are a prefix of the native bank — about every
//! result within C's own bank. What remains of the thunk is the re-extension a
//! narrow scalar needs and the adaptation of a result the two banks place
//! differently. [`ExportSignature`] is the pairing of the two views, built once
//! from the type pool by [`ExportSignature::for_types`].
//!
//! ## Why the compact image is the C image
//!
//! Under the compact-layout default a `@repr(c)` aggregate's physical memory
//! image *is* its C object layout, and the native convention's indirect
//! transports (an indirect by-value argument, an sret return) already pass that
//! exact image through memory. So the thunk never repacks those: it hands the C
//! caller's own bytes to the native body, and hands the native body's sret
//! storage — the C caller's storage, when the C return is also indirect — back.
//! Only a native crossing that reads the value apart needs marshaling, and then
//! only the leaf loads and stores the compact image map already describes.
//!
//! ## Abort at the boundary (ratified, ADR-0064 ruling 3)
//!
//! A trap that occurs while a C caller is on the stack must abort the process,
//! never unwind a C frame. Rue has **no unwinding machinery at all**: every trap
//! (overflow, bounds, `@panic`, failed checked assertion) lowers to a runtime
//! call that writes a diagnostic and performs a direct `exit(2)` syscall, and
//! `unreachable` lowers to an illegal instruction. Executing a native body
//! through this thunk therefore inherits abort-at-boundary for free — there is
//! no code path by which a trap could return into the thunk and propagate an
//! unwind into C. This module adds no guard because none is needed; the property
//! is structural, and the CLI suite proves it by observing a trapping export
//! terminate a C caller with the runtime's deterministic exit status.

use rue_air::{
    ArgConvention, ArgLocation, CAbiTypeFacts, FrozenTypeInternPool, LoweredReturn,
    LoweredSignature, PaddingRange, PointerLocation, Type, lower_c_signature, lower_native_return,
    lower_native_signature,
};
use rue_target::{
    Arch, CRegisterClass, CallingConvention, ConventionSpec, SretRegisterKind, Target,
};

#[cfg(test)]
use rue_air::ScalarAbiExtension;

use crate::{EmittedRelocation, MachineCode};

/// One flattened leaf of a value's compact memory image: where one native ABI
/// slot lives in the C image, and how wide it is there.
///
/// Loading a leaf into a native slot extends it to Rue's canonical 64-bit form;
/// storing a native slot back into the image truncates it to `width`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImageLeaf {
    /// Byte offset of the leaf within the compact image.
    pub byte_offset: u32,
    /// Physical width in bytes: 1, 2, 4, or 8.
    pub width: u32,
    /// Whether a load sign-extends (`true`) or zero-extends (`false`).
    pub signed: bool,
}

/// How the native body reads one parameter apart.
///
/// *Where* the parameter travels is the native lowering's answer, the same one
/// the callee's own parameter plan reads; this is the other half — whether the
/// value's leaves are its eightbytes, and where each leaf sits in the compact
/// image either way.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeParameter {
    /// The value's flattened leaves, in ascending image order.
    pub leaves: Vec<ImageLeaf>,
    /// Whether each leaf starts its own eightbyte, so the leaves themselves
    /// cross in Rue's canonical 64-bit form rather than the image's eightbytes
    /// crossing whole (`crate::native_abi::NativeImage::direct_leaves`).
    pub leaves_are_eightbytes: bool,
}

/// How the native body returns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NativeReturn {
    /// Nothing.
    Void,
    /// One scalar in the primary result register, already in Rue's canonical
    /// form — which is exactly what a C caller accepts, so it passes through.
    Scalar,
    /// One result register per flattened leaf, in ascending slot order,
    /// because every leaf starts its own eightbyte; each leaf names where that
    /// slot belongs in the C image.
    Registers {
        /// The value's flattened leaves, in ascending image order.
        leaves: Vec<ImageLeaf>,
    },
    /// One result register per eightbyte of the value's compact image, because
    /// its leaves pack together. The compact image *is* the C image, so those
    /// eightbytes are already the ones a C caller expects.
    Eightbytes {
        /// How many eightbytes the image spans.
        count: u32,
    },
    /// The body writes the value's compact image into caller storage whose
    /// address travels in the target row's own indirect-result register.
    Sret,
}

/// One exported parameter, seen from both conventions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportParameter {
    /// What the C boundary sees.
    pub c: CAbiTypeFacts,
    /// What the native body expects.
    pub native: NativeParameter,
}

/// A `pub extern "C" fn` export's complete ABI description: what a C caller
/// presents and what the native body expects, with every type fact already
/// resolved so the description outlives the type pool it was projected from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportSignature {
    /// The convention the export's C entry follows, resolved once from the
    /// declaration's ABI string against the compilation target (spec 9.3:1b).
    /// The thunk places the C side from this row rather than re-deriving the
    /// target's own C row, so an explicitly named convention and the `"C"` alias
    /// reach the emitter by one route.
    convention: CallingConvention,
    parameters: Vec<ExportParameter>,
    result: CAbiTypeFacts,
    /// Whether the result is an aggregate rather than a scalar, which decides
    /// whether a register return names leaves or a single canonical value.
    result_is_aggregate: bool,
    /// Whether the native convention hands the result's leaves over as
    /// themselves rather than packing them into the image's eightbytes.
    return_leaves_are_eightbytes: bool,
    return_leaves: Vec<ImageLeaf>,
    return_padding: Vec<PaddingRange>,
    /// The result's byte size, which is how much of the C caller's storage an
    /// indirect return fills.
    return_bytes: u32,
}

impl ExportSignature {
    /// Project an export's signature from the live type pool.
    ///
    /// `param_types` are the export's declared parameter types in source order
    /// and `return_type` its result. Semantic analysis has already gated the
    /// signature through `c_passable_by_value`, so every type here is a
    /// C-passable scalar, pointer, or `@repr(c)` aggregate, and every parameter
    /// is by value; `convention` is the row it already resolved the export's ABI
    /// string to.
    pub fn for_types(
        type_pool: &FrozenTypeInternPool,
        convention: CallingConvention,
        param_types: &[Type],
        return_type: Type,
    ) -> Self {
        let parameters = param_types
            .iter()
            .map(|&ty| ExportParameter {
                c: rue_air::c_abi_type_facts(type_pool, ty),
                native: NativeParameter {
                    leaves: image_leaves(type_pool, ty),
                    leaves_are_eightbytes: native_leaves_are_eightbytes(type_pool, ty),
                },
            })
            .collect();
        Self {
            convention,
            parameters,
            result: rue_air::c_abi_type_facts(type_pool, return_type),
            result_is_aggregate: crate::types::is_multislot_aggregate(type_pool, return_type),
            return_leaves_are_eightbytes: native_leaves_are_eightbytes(type_pool, return_type),
            return_leaves: image_leaves(type_pool, return_type),
            return_padding: type_pool.compact_image_padding_ranges(return_type),
            return_bytes: u32::try_from(type_pool.layout(return_type).size)
                .expect("an export result size fits u32"),
        }
    }

    /// The lowered C signature a caller of this export writes and this thunk
    /// reads, under the convention the declaration named.
    pub fn lowered(&self) -> LoweredSignature {
        let parameters = self
            .parameters
            .iter()
            .map(|parameter| (parameter.c, ArgConvention::ByValue))
            .collect::<Vec<_>>();
        lower_c_signature(self.convention, &parameters, self.result)
    }

    /// How the native body returns on `target`, whose C row plus the wider
    /// native return bank fixes the placement (ADR-0084).
    ///
    /// The facts are the C ones: an export's result is a C-passable type, whose
    /// compact image is its C object layout, so the two conventions classify
    /// the same bytes and differ only in how many registers they will spend on
    /// them.
    fn native_return(&self, target: Target) -> NativeReturn {
        match lower_native_return(ConventionSpec::native(target), self.result) {
            LoweredReturn::Void => NativeReturn::Void,
            LoweredReturn::Registers { pieces, .. } => {
                if !self.result_is_aggregate {
                    NativeReturn::Scalar
                } else if self.return_leaves_are_eightbytes {
                    NativeReturn::Registers {
                        leaves: self.return_leaves.clone(),
                    }
                } else {
                    NativeReturn::Eightbytes {
                        count: pieces.len(),
                    }
                }
            }
            LoweredReturn::Sret { .. } => NativeReturn::Sret,
        }
    }
}

/// The flattened compact-image leaves of `ty`, one per native ABI slot.
fn image_leaves(type_pool: &FrozenTypeInternPool, ty: Type) -> Vec<ImageLeaf> {
    crate::types::aggregate_physical_slot_map(type_pool, ty)
        .expect(
            "a C-passable type has a variant-independent compact memory image; \
             c_passable_by_value gated the export signature before lowering",
        )
        .into_iter()
        .map(|slot| {
            assert!(
                slot.float_width.is_none(),
                "the C boundary still rejects floats, so no export leaf is float-classed"
            );
            ImageLeaf {
                byte_offset: u32::try_from(slot.byte_offset)
                    .expect("a compact image offset is non-negative and fits u32"),
                width: slot.access.map_or(8, |access| u32::from(access.width)),
                signed: slot.access.is_some_and(|access| access.signed),
            }
        })
        .collect()
}

/// Whether the native convention hands `ty`'s leaves over as themselves rather
/// than packing them into the image's eightbytes.
///
/// This is the one predicate both ends of a native crossing consult
/// (`crate::native_abi::NativeImage::direct_leaves`); a scalar is trivially its
/// own eightbyte. Every export type is C-passable, so every leaf is
/// general-purpose and the bank agreement the predicate also checks is
/// automatic.
fn native_leaves_are_eightbytes(type_pool: &FrozenTypeInternPool, ty: Type) -> bool {
    match crate::native_abi::native_by_value_arg(type_pool, ty) {
        crate::native_abi::NativeArg::Aggregate { image } => image.direct_leaves().is_some(),
        _ => true,
    }
}

/// Build the machine code for a Rue-to-C export thunk.
///
/// `native_symbol` is the mangled symbol of the natively-conventioned body the
/// thunk forwards to. The returned [`MachineCode`] carries one call/branch
/// relocation targeting `native_symbol` and no string data.
pub fn generate_export_thunk(
    target: Target,
    native_symbol: &str,
    signature: &ExportSignature,
) -> MachineCode {
    let plan = ThunkPlan::new(target, signature);
    match target.arch() {
        Arch::X86_64 => {
            let mut emitter = X86Emitter::default();
            plan.emit(&mut emitter, native_symbol);
            emitter.finish()
        }
        Arch::Aarch64 => {
            let mut emitter = Aarch64Emitter::default();
            plan.emit(&mut emitter, native_symbol);
            emitter.finish()
        }
    }
}

// ============================================================================
// The target-independent thunk plan
// ============================================================================

/// Where one value the native convention places comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NativeSlotSource {
    /// The address of the hidden return storage the native body writes.
    ReturnStorage,
    /// The address of parameter `parameter`'s compact image.
    ImageAddress { parameter: usize },
    /// Leaf `leaf` of parameter `parameter`'s compact image, extended to Rue's
    /// canonical 64-bit form.
    Leaf { parameter: usize, leaf: usize },
    /// Eightbyte `index` of parameter `parameter`'s compact image, whole.
    Eightbyte { parameter: usize, index: usize },
}

/// Where the native convention puts one of those values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NativeSlotDestination {
    /// Native argument register `index` of the general-purpose roster. Every
    /// export type is C-passable, so no value reaches the floating-point one.
    Register { index: u32 },
    /// The target row's dedicated indirect-result register, outside the
    /// argument roster (AAPCS64 `x8`, section 6.9).
    SretRegister,
    /// Byte `offset` of the outgoing native argument area.
    Stack { offset: u32 },
}

/// How to reach one parameter's compact image.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ImageBase {
    /// `frame + offset`: the saved incoming argument registers, which hold a
    /// register-passed value's eightbytes contiguously.
    Frame { offset: u32 },
    /// `incoming + offset`: the C caller's outgoing argument area, which holds
    /// a byval-stacked value's image directly.
    Incoming { offset: u32 },
    /// The pointer stored at `frame + offset`, which addresses a caller-owned
    /// copy.
    SavedPointer { offset: u32 },
}

/// Round `value` up to a multiple of the power-of-two `align`.
fn align_up(value: u32, align: u32) -> u32 {
    value
        .checked_add(align - 1)
        .expect("an export thunk frame fits u32")
        & !(align - 1)
}

/// The complete description of one thunk body: every frame position and every
/// value movement, decided once and encoded twice.
struct ThunkPlan {
    c: LoweredSignature,
    native_return: NativeReturn,
    /// The image base of each parameter, in source order.
    bases: Vec<ImageBase>,
    /// The leaves of each parameter, in source order.
    leaves: Vec<Vec<ImageLeaf>>,
    /// Every value the native convention places, in native order, and where it
    /// places it.
    slots: Vec<(NativeSlotSource, NativeSlotDestination)>,
    /// The return value's leaves, when the native body returns in registers.
    return_leaves: Vec<ImageLeaf>,
    return_padding: Vec<PaddingRange>,
    /// Frame offset each native value is assembled at: a staging cell for a
    /// register-passed one, its own position in the outgoing native argument
    /// area for a stacked one.
    slot_offsets: Vec<u32>,
    /// `(native argument register, staging cell)` for every register-passed
    /// value, in placement order.
    register_loads: Vec<(Option<u32>, u32)>,
    /// Frame offset of the incoming C argument register save block.
    save_base: u32,
    /// Frame offset holding the C caller's indirect-result pointer, when the C
    /// return uses one.
    c_sret_offset: Option<u32>,
    /// Frame offset of the buffer the C image is assembled in, when the C
    /// return is an aggregate. Equal to the C caller's own storage when the C
    /// return is indirect (through [`Self::c_sret_offset`]).
    return_image: Option<ReturnImage>,
    frame_bytes: u32,
}

/// Where the C image of an aggregate return is assembled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReturnImage {
    /// The C caller's own indirect-result storage, whose pointer is saved at
    /// this frame offset. The native body writes it directly when the native
    /// return is also indirect.
    CallerStorage { pointer_offset: u32 },
    /// A scratch buffer in the thunk's frame, because the C return travels in
    /// result registers.
    Scratch { offset: u32 },
    /// The native body returned the value's whole eightbytes but the C return
    /// is indirect: the eightbytes are staged in a scratch buffer, which can
    /// take whole-eightbyte stores, and the result's own bytes are then copied
    /// into the C caller's storage, which is exactly `bytes` long and no better
    /// aligned than `align`.
    StagedCallerStorage {
        scratch: u32,
        pointer_offset: u32,
        bytes: u32,
        align: u32,
    },
}

impl ThunkPlan {
    fn new(target: Target, signature: &ExportSignature) -> Self {
        assert!(
            signature.convention.is_implemented_by(target),
            "an export thunk for {target:?} cannot be emitted under {}: semantic \
             analysis rejects a declaration naming a convention this target does \
             not implement (spec 9.3:1c)",
            signature.convention
        );
        let c = signature.lowered();
        let spec = c.spec();
        let native_return = signature.native_return(target);

        // The native side is placed by the one lowering every crossing
        // consumes, against the very facts the C side was placed by: the two
        // conventions classify the same compact image, and the native row is
        // this target's own C row with a wider return bank (ADR-0084).
        let native_pairing = rue_target::ConventionSpec::native(target);
        let native_parameters = signature
            .parameters
            .iter()
            .map(|parameter| (parameter.c, ArgConvention::ByValue))
            .collect::<Vec<_>>();
        let native_spec = native_pairing.spec();
        let native = lower_native_signature(
            native_pairing,
            &native_parameters,
            if matches!(native_return, NativeReturn::Sret) {
                LoweredReturn::Sret {
                    register: native_spec.sret_register,
                    echoed: native_spec.sret_pointer_echoed_in_result_register,
                    size: 8,
                    align: 8,
                }
            } else {
                LoweredReturn::Void
            },
        );

        // Every incoming general-purpose argument register is saved to a
        // contiguous cell block, so a register-passed value's eightbytes are
        // addressable as one image with no repacking.
        let save_cells = spec.gp_argument_registers + 1;

        let mut slots: Vec<(NativeSlotSource, NativeSlotDestination)> = Vec::new();
        if matches!(native_return, NativeReturn::Sret) {
            // The native convention takes its target row's own indirect-result
            // rule (ADR-0084): SysV AMD64's hidden first ordinary argument, or
            // AAPCS64's dedicated `x8` outside the roster.
            slots.push((
                NativeSlotSource::ReturnStorage,
                if native.sret_in_argument_register() {
                    NativeSlotDestination::Register { index: 0 }
                } else {
                    NativeSlotDestination::SretRegister
                },
            ));
        }
        let mut leaves = Vec::with_capacity(signature.parameters.len());
        for (parameter, (description, placement)) in signature
            .parameters
            .iter()
            .zip(native.arguments())
            .enumerate()
        {
            let value_leaves = &description.native.leaves;
            let source = |index: usize| {
                if description.native.leaves_are_eightbytes {
                    NativeSlotSource::Leaf {
                        parameter,
                        leaf: index,
                    }
                } else {
                    NativeSlotSource::Eightbyte { parameter, index }
                }
            };
            match placement.location {
                ArgLocation::Omitted => {}
                ArgLocation::Registers { pieces } => {
                    assert_eq!(
                        pieces.uniform_class(),
                        Some(CRegisterClass::Gp),
                        "an export argument still crosses only in general-purpose registers"
                    );
                    for (index, piece) in pieces.as_slice().iter().enumerate() {
                        slots.push((
                            source(index),
                            NativeSlotDestination::Register { index: piece.index },
                        ));
                    }
                }
                ArgLocation::Stack { offset, size, .. } => {
                    let count = if value_leaves.len() == 1 && size < 8 {
                        1
                    } else {
                        (size as usize).div_ceil(8)
                    };
                    for index in 0..count {
                        slots.push((
                            source(index),
                            NativeSlotDestination::Stack {
                                offset: offset + (index as u32) * 8,
                            },
                        ));
                    }
                }
                ArgLocation::Indirect { pointer, .. } => slots.push((
                    NativeSlotSource::ImageAddress { parameter },
                    match pointer {
                        PointerLocation::Register { index } => {
                            NativeSlotDestination::Register { index }
                        }
                        PointerLocation::Stack { offset } => {
                            NativeSlotDestination::Stack { offset }
                        }
                    },
                )),
            }
            leaves.push(value_leaves.clone());
        }

        let register_slots = slots
            .iter()
            .filter(|(_, destination)| {
                matches!(destination, NativeSlotDestination::Register { .. })
            })
            .count() as u32;
        // The outgoing native argument area sits at the base of the frame, so
        // it is the block the native callee addresses from its own entry stack
        // pointer. Its size is the native lowering's own answer, already rounded
        // to the call-boundary alignment, which is what keeps the whole frame —
        // and therefore the stack at the call — 16-byte aligned.
        let native_stack_bytes = native.stack_bytes();

        let stage_base = native_stack_bytes;
        let save_base = stage_base + register_slots * 8;
        let mut next = save_base + save_cells * 8;

        let c_sret_offset = match c.ret() {
            LoweredReturn::Sret { register, .. } => Some(match register {
                // SysV's hidden pointer *is* general-purpose argument register
                // zero, so it is already in the save block.
                SretRegisterKind::ArgumentRegister => save_base,
                SretRegisterKind::DedicatedRegister => save_base + spec.gp_argument_registers * 8,
            }),
            _ => None,
        };

        let return_image = match (c.ret(), &native_return) {
            // The native body returned whole eightbytes and the C caller wants
            // them in its own storage: stage them where a whole-eightbyte store
            // is in bounds, then copy the result's own bytes across.
            (LoweredReturn::Sret { .. }, NativeReturn::Eightbytes { count }) => {
                let scratch = align_up(next, 16);
                next = scratch + align_up(count * 8, 16);
                Some(ReturnImage::StagedCallerStorage {
                    scratch,
                    pointer_offset: c_sret_offset.expect("an indirect C return saves its pointer"),
                    bytes: signature.return_bytes,
                    align: match signature.result {
                        CAbiTypeFacts::Aggregate { align, .. } => {
                            u32::try_from(align).expect("an aggregate alignment fits u32")
                        }
                        _ => 1,
                    },
                })
            }
            (LoweredReturn::Sret { .. }, _) => Some(ReturnImage::CallerStorage {
                pointer_offset: c_sret_offset.expect("an indirect C return saves its pointer"),
            }),
            (LoweredReturn::Registers { pieces, .. }, NativeReturn::Registers { .. })
            | (LoweredReturn::Registers { pieces, .. }, NativeReturn::Sret)
                if !matches!(signature.result, CAbiTypeFacts::Scalar { .. }) =>
            {
                let offset = align_up(next, 16);
                next = offset + align_up(pieces.len() * 8, 16);
                Some(ReturnImage::Scratch { offset })
            }
            // The native body returned the compact image's eightbytes and the C
            // return is those same eightbytes in the same registers: C's result
            // registers are a prefix of the native bank, so nothing crosses.
            _ => None,
        };

        let frame_bytes = align_up(next, 16);

        let bases = signature
            .parameters
            .iter()
            .zip(c.arguments())
            .map(|(_, argument)| match argument.location {
                ArgLocation::Registers { pieces } => {
                    // The register save block holds the general-purpose roster,
                    // and a register-passed value's eightbytes are contiguous in
                    // it, so its image needs no repacking. Nothing reaches the
                    // floating-point roster while the C boundary rejects floats.
                    assert_eq!(
                        pieces.uniform_class(),
                        Some(CRegisterClass::Gp),
                        "an export argument still crosses only in general-purpose registers"
                    );
                    ImageBase::Frame {
                        offset: save_base + pieces.first_index().unwrap_or(0) * 8,
                    }
                }
                ArgLocation::Stack { offset, .. } => ImageBase::Incoming { offset },
                ArgLocation::Indirect { pointer, .. } => match pointer {
                    PointerLocation::Register { index } => ImageBase::SavedPointer {
                        offset: save_base + index * 8,
                    },
                    // A spilled pointer is itself in the incoming argument
                    // area; loading it needs the pointer's own cell as a base,
                    // which the incoming area addresses directly.
                    PointerLocation::Stack { offset } => ImageBase::Incoming { offset },
                },
                // A zero-sized argument has no image; its base is never read.
                ArgLocation::Omitted => ImageBase::Frame { offset: save_base },
            })
            .collect::<Vec<_>>();

        // A register-passed value is assembled in its own staging cell and
        // loaded into its register once every image read is done; a stacked one
        // is written straight into the outgoing argument area.
        let mut staged = 0u32;
        let mut slot_offsets = Vec::with_capacity(slots.len());
        let mut register_loads = Vec::new();
        for (_, destination) in &slots {
            match *destination {
                NativeSlotDestination::Register { index } => {
                    let offset = stage_base + staged * 8;
                    staged += 1;
                    register_loads.push((Some(index), offset));
                    slot_offsets.push(offset);
                }
                NativeSlotDestination::SretRegister => {
                    let offset = stage_base + staged * 8;
                    staged += 1;
                    register_loads.push((None, offset));
                    slot_offsets.push(offset);
                }
                NativeSlotDestination::Stack { offset } => slot_offsets.push(offset),
            }
        }

        Self {
            c,
            native_return,
            bases,
            leaves,
            slots,
            return_leaves: signature.return_leaves.clone(),
            return_padding: signature.return_padding.clone(),
            slot_offsets,
            register_loads,
            save_base,
            c_sret_offset,
            return_image,
            frame_bytes,
        }
    }

    /// Whether a parameter's image is reached through a pointer that itself
    /// lives in the C caller's outgoing argument area.
    fn pointer_spilled(&self, parameter: usize) -> bool {
        matches!(
            self.c.arguments()[parameter].location,
            ArgLocation::Indirect {
                pointer: PointerLocation::Stack { .. },
                ..
            }
        )
    }

    fn emit<E: ThunkEmitter>(&self, emitter: &mut E, native_symbol: &str) {
        let spec = self.c.spec();
        emitter.prologue(self.frame_bytes);

        // Everything the C caller left in a register is spilled first, so the
        // rest of the body reads memory and every register is free.
        let save_base = self.save_base;
        for index in 0..spec.gp_argument_registers {
            emitter.save_argument_register(index, save_base + index * 8);
        }
        if matches!(
            self.c.ret(),
            LoweredReturn::Sret {
                register: SretRegisterKind::DedicatedRegister,
                ..
            }
        ) {
            emitter.save_sret_register(save_base + spec.gp_argument_registers * 8);
        }

        for (index, (source, _)) in self.slots.iter().enumerate() {
            let destination = self.slot_offsets[index];
            match *source {
                NativeSlotSource::ReturnStorage => {
                    match self.return_image.expect("an sret return names its storage") {
                        ReturnImage::CallerStorage { pointer_offset } => {
                            emitter.base_from_saved_pointer(pointer_offset)
                        }
                        ReturnImage::Scratch { offset } => emitter.base_from_frame(offset),
                        ReturnImage::StagedCallerStorage { .. } => unreachable!(
                            "a staged return is a register return; a native sret \
                             return writes its storage directly"
                        ),
                    }
                    emitter.store_base(destination);
                }
                NativeSlotSource::ImageAddress { parameter } => {
                    self.set_base(emitter, parameter);
                    emitter.store_base(destination);
                }
                NativeSlotSource::Leaf { parameter, leaf } => {
                    self.set_base(emitter, parameter);
                    let leaf = self.leaves[parameter][leaf];
                    emitter.load_leaf(leaf, destination);
                }
                NativeSlotSource::Eightbyte { parameter, index } => {
                    // The leaves pack together, so the image's eightbyte crosses
                    // whole and the callee reads the leaves back out of it.
                    self.set_base(emitter, parameter);
                    emitter.load_leaf(
                        ImageLeaf {
                            byte_offset: (index as u32) * 8,
                            width: 8,
                            signed: false,
                        },
                        destination,
                    );
                }
            }
        }

        for (register, offset) in &self.register_loads {
            match register {
                Some(index) => emitter.load_argument_register(*index, *offset),
                None => emitter.load_sret_register(*offset),
            }
        }
        emitter.call(native_symbol);

        self.emit_result(emitter);
        emitter.epilogue(self.frame_bytes);
    }

    fn set_base<E: ThunkEmitter>(&self, emitter: &mut E, parameter: usize) {
        match self.bases[parameter] {
            ImageBase::Frame { offset } => emitter.base_from_frame(offset),
            ImageBase::Incoming { offset } if self.pointer_spilled(parameter) => {
                emitter.base_from_incoming_pointer(offset)
            }
            ImageBase::Incoming { offset } => emitter.base_from_incoming(offset),
            ImageBase::SavedPointer { offset } => emitter.base_from_saved_pointer(offset),
        }
    }

    fn emit_result<E: ThunkEmitter>(&self, emitter: &mut E) {
        let Some(image) = self.return_image else {
            // A void or scalar return needs no fix-up: the native body already
            // leaves the canonical value in the register a C caller reads.
            return;
        };

        // Address the C image once; every store and load below is relative to
        // it, and the base register is outside the native return roster.
        match image {
            ReturnImage::CallerStorage { pointer_offset } => {
                emitter.base_from_saved_pointer(pointer_offset)
            }
            ReturnImage::Scratch { offset }
            | ReturnImage::StagedCallerStorage {
                scratch: offset, ..
            } => emitter.base_from_frame(offset),
        }

        if let (
            NativeReturn::Eightbytes { count },
            ReturnImage::StagedCallerStorage {
                scratch,
                pointer_offset,
                bytes,
                align,
            },
        ) = (&self.native_return, image)
        {
            // Stage the whole eightbytes, then hand the C caller exactly the
            // bytes its storage holds.
            for index in 0..*count {
                emitter.store_return_register(
                    index,
                    ImageLeaf {
                        byte_offset: index * 8,
                        width: 8,
                        signed: false,
                    },
                );
            }
            emitter.base_from_saved_pointer(pointer_offset);
            emitter.copy_image_bytes(scratch, bytes, align);
        }

        if let NativeReturn::Registers { .. } = self.native_return {
            // The native body returned slots, not an image: zero the padding
            // for a deterministic image (ADR-0052 ruling 5), then write each
            // slot at its own byte position and width.
            for range in &self.return_padding {
                let start = u32::try_from(range.start).expect("a padding offset fits u32");
                let end = u32::try_from(range.end).expect("a padding offset fits u32");
                emitter.zero_image_bytes(start, end - start);
            }
            for (index, leaf) in self.return_leaves.iter().enumerate() {
                emitter.store_return_register(
                    u32::try_from(index).expect("a return slot index fits u32"),
                    *leaf,
                );
            }
        }

        match self.c.ret() {
            LoweredReturn::Registers { pieces, .. } => {
                assert_eq!(
                    pieces.uniform_class(),
                    Some(CRegisterClass::Gp),
                    "the C boundary still returns only general-purpose values"
                );
                for index in 0..pieces.len() {
                    emitter.load_result_register(index, index * 8);
                }
            }
            LoweredReturn::Sret { echoed, .. } => {
                if echoed {
                    emitter.echo_sret_pointer(
                        self.c_sret_offset
                            .expect("an indirect C return saves its pointer"),
                    );
                }
            }
            LoweredReturn::Void => {}
        }
    }
}

// ============================================================================
// The per-target instruction leaves
// ============================================================================

/// The physical leaves an export thunk body is assembled from.
///
/// Every offset is a byte offset into the thunk's own frame, measured from the
/// stack pointer after the prologue, except `base_from_incoming`'s, which is
/// measured from the base of the C caller's outgoing argument area. The plan
/// above owns every placement decision; an implementation chooses only
/// encodings.
///
/// An implementation keeps one dedicated *base* register, set by the
/// `base_from_*` leaves and read by every leaf that names an image position. It
/// is never one of the native return registers, so the result leaves can run
/// with a live return value.
trait ThunkEmitter {
    /// Establish the frame and reserve `frame_bytes`, which is already a
    /// multiple of the call-boundary alignment.
    fn prologue(&mut self, frame_bytes: u32);
    /// Release the frame and return to the C caller.
    fn epilogue(&mut self, frame_bytes: u32);
    /// Spill general-purpose argument register `index` to `offset`.
    fn save_argument_register(&mut self, index: u32, offset: u32);
    /// Spill the dedicated indirect-result register to `offset`.
    fn save_sret_register(&mut self, offset: u32);
    /// base := frame + `offset`.
    fn base_from_frame(&mut self, offset: u32);
    /// base := incoming argument area + `offset`.
    fn base_from_incoming(&mut self, offset: u32);
    /// base := the pointer stored at frame + `offset`.
    fn base_from_saved_pointer(&mut self, offset: u32);
    /// base := the pointer stored in the incoming argument area at `offset`.
    fn base_from_incoming_pointer(&mut self, offset: u32);
    /// frame + `destination` := `leaf` loaded from base, extended to 64 bits.
    fn load_leaf(&mut self, leaf: ImageLeaf, destination: u32);
    /// frame + `destination` := base.
    fn store_base(&mut self, destination: u32);
    /// Native argument register `index` := frame + `offset`.
    fn load_argument_register(&mut self, index: u32, offset: u32);
    /// The native convention's dedicated indirect-result register := frame +
    /// `offset`.
    fn load_sret_register(&mut self, offset: u32);
    /// Call the native body.
    fn call(&mut self, symbol: &str);
    /// base + `leaf.byte_offset` := the low `leaf.width` bytes of native return
    /// register `index`.
    fn store_return_register(&mut self, index: u32, leaf: ImageLeaf);
    /// Zero `len` bytes at base + `offset`.
    fn zero_image_bytes(&mut self, offset: u32, len: u32);
    /// C result register `index` := the eight bytes at base + `offset`.
    fn load_result_register(&mut self, index: u32, offset: u32);
    /// The primary C result register := the pointer stored at frame + `offset`.
    fn echo_sret_pointer(&mut self, offset: u32);
    /// base + 0 .. base + `byte_count` := frame + `source_offset` .. , copied in
    /// 8/4/2/1-byte steps of at most `max_width` bytes, each naturally aligned
    /// within the image, so no store runs past the C caller's storage or past
    /// the alignment that storage is guaranteed.
    fn copy_image_bytes(&mut self, source_offset: u32, byte_count: u32, max_width: u32);
}

/// Zeroing a padding run, largest naturally-aligned store first.
fn zero_runs(offset: u32, len: u32) -> Vec<(u32, u32)> {
    image_runs(offset, len, 8)
}

/// The 8/4/2/1-byte steps that cover `len` bytes from `offset`, each naturally
/// aligned within the image and none wider than `max_width`.
fn image_runs(offset: u32, len: u32, max_width: u32) -> Vec<(u32, u32)> {
    let mut runs = Vec::new();
    let mut position = offset;
    let end = offset + len;
    let cap = max_width.clamp(1, 8);
    while position < end {
        let mut width = 8;
        while width > 1 && (width > cap || position % width != 0 || position + width > end) {
            width /= 2;
        }
        runs.push((position, width));
        position += width;
    }
    runs
}

// ============================================================================
// x86-64 / SysV AMD64
// ============================================================================

/// SysV integer argument registers, in order: `rdi, rsi, rdx, rcx, r8, r9`.
const X86_ARG_REGS: [u8; 6] = [7, 6, 2, 1, 8, 9];
/// Native return registers, in order: `rax, rdx, rcx, r8, r9, r10`.
const X86_RET_REGS: [u8; 6] = [0, 2, 1, 8, 9, 10];
/// SysV result registers: `rax, rdx`.
const X86_RESULT_REGS: [u8; 2] = [0, 2];
const X86_RAX: u8 = 0;
const X86_RSP: u8 = 4;
const X86_RBP: u8 = 5;
/// The dedicated base register: caller-saved and outside every roster above.
const X86_BASE: u8 = 11;

#[derive(Default)]
struct X86Emitter {
    code: Vec<u8>,
    relocations: Vec<EmittedRelocation>,
}

impl X86Emitter {
    fn finish(self) -> MachineCode {
        MachineCode {
            code: self.code,
            relocations: self.relocations,
            strings: Vec::new(),
        }
    }

    /// Emit `opcode` with a `reg`-to-`[base + disp]` ModRM operand. `rex_w`
    /// selects the 64-bit operand size; `force_rex` emits the prefix even when
    /// no bit is set, which the byte-register encodings need.
    fn mem(&mut self, opcode: &[u8], reg: u8, base: u8, disp: i32, rex_w: bool, force_rex: bool) {
        let mut rex = 0x40;
        if rex_w {
            rex |= 0x08;
        }
        if reg >= 8 {
            rex |= 0x04;
        }
        if base >= 8 {
            rex |= 0x01;
        }
        if rex != 0x40 || force_rex {
            self.code.push(rex);
        }
        self.code.extend_from_slice(opcode);
        // Always the 32-bit displacement form, so no offset is out of range.
        self.code.push(0x80 | ((reg & 7) << 3) | (base & 7));
        if base & 7 == X86_RSP {
            self.code.push(0x24); // SIB: no index, base = rsp/r12
        }
        self.code.extend_from_slice(&disp.to_le_bytes());
    }

    fn frame_disp(offset: u32) -> i32 {
        i32::try_from(offset).expect("an export thunk frame offset fits i32")
    }

    /// `mov [rsp + offset], reg`
    fn store_frame(&mut self, reg: u8, offset: u32) {
        self.mem(&[0x89], reg, X86_RSP, Self::frame_disp(offset), true, false);
    }

    /// `mov reg, [rsp + offset]`
    fn load_frame(&mut self, reg: u8, offset: u32) {
        self.mem(&[0x8B], reg, X86_RSP, Self::frame_disp(offset), true, false);
    }
}

impl ThunkEmitter for X86Emitter {
    fn prologue(&mut self, frame_bytes: u32) {
        self.code.push(0x55); // push rbp
        self.code.extend_from_slice(&[0x48, 0x89, 0xE5]); // mov rbp, rsp
        self.code.extend_from_slice(&[0x48, 0x81, 0xEC]); // sub rsp, imm32
        self.code.extend_from_slice(&frame_bytes.to_le_bytes());
    }

    fn epilogue(&mut self, frame_bytes: u32) {
        self.code.extend_from_slice(&[0x48, 0x81, 0xC4]); // add rsp, imm32
        self.code.extend_from_slice(&frame_bytes.to_le_bytes());
        self.code.push(0x5D); // pop rbp
        self.code.push(0xC3); // ret
    }

    fn save_argument_register(&mut self, index: u32, offset: u32) {
        self.store_frame(X86_ARG_REGS[index as usize], offset);
    }

    fn save_sret_register(&mut self, _offset: u32) {
        unreachable!("SysV AMD64 passes the hidden result pointer in an argument register");
    }

    fn base_from_frame(&mut self, offset: u32) {
        // lea r11, [rsp + offset]
        self.mem(
            &[0x8D],
            X86_BASE,
            X86_RSP,
            Self::frame_disp(offset),
            true,
            false,
        );
    }

    fn base_from_incoming(&mut self, offset: u32) {
        // The C caller's outgoing area begins just past the saved frame pointer
        // and the return address the `call` pushed: `rbp + 16`.
        self.mem(
            &[0x8D],
            X86_BASE,
            X86_RBP,
            Self::frame_disp(offset) + 16,
            true,
            false,
        );
    }

    fn base_from_saved_pointer(&mut self, offset: u32) {
        self.mem(
            &[0x8B],
            X86_BASE,
            X86_RSP,
            Self::frame_disp(offset),
            true,
            false,
        );
    }

    fn base_from_incoming_pointer(&mut self, offset: u32) {
        self.mem(
            &[0x8B],
            X86_BASE,
            X86_RBP,
            Self::frame_disp(offset) + 16,
            true,
            false,
        );
    }

    fn load_leaf(&mut self, leaf: ImageLeaf, destination: u32) {
        let disp = Self::frame_disp(leaf.byte_offset);
        match (leaf.width, leaf.signed) {
            (8, _) => self.mem(&[0x8B], X86_RAX, X86_BASE, disp, true, false),
            (4, true) => self.mem(&[0x63], X86_RAX, X86_BASE, disp, true, false), // movsxd
            // A 32-bit `mov` zero-extends into the full register.
            (4, false) => self.mem(&[0x8B], X86_RAX, X86_BASE, disp, false, false),
            (2, true) => self.mem(&[0x0F, 0xBF], X86_RAX, X86_BASE, disp, true, false),
            (2, false) => self.mem(&[0x0F, 0xB7], X86_RAX, X86_BASE, disp, true, false),
            (1, true) => self.mem(&[0x0F, 0xBE], X86_RAX, X86_BASE, disp, true, false),
            (1, false) => self.mem(&[0x0F, 0xB6], X86_RAX, X86_BASE, disp, true, false),
            (width, _) => unreachable!("a compact image leaf is 1, 2, 4, or 8 bytes, not {width}"),
        }
        self.store_frame(X86_RAX, destination);
    }

    fn store_base(&mut self, destination: u32) {
        self.store_frame(X86_BASE, destination);
    }

    fn load_argument_register(&mut self, index: u32, offset: u32) {
        self.load_frame(X86_ARG_REGS[index as usize], offset);
    }

    fn load_sret_register(&mut self, _offset: u32) {
        unreachable!("SysV AMD64 has no dedicated indirect-result register");
    }

    fn call(&mut self, symbol: &str) {
        self.code.push(0xE8);
        let offset = self.code.len() as u64;
        self.code.extend_from_slice(&[0, 0, 0, 0]);
        self.relocations
            .push(EmittedRelocation::x86_call(offset, symbol));
    }

    fn store_return_register(&mut self, index: u32, leaf: ImageLeaf) {
        let reg = X86_RET_REGS[index as usize];
        let disp = Self::frame_disp(leaf.byte_offset);
        match leaf.width {
            8 => self.mem(&[0x89], reg, X86_BASE, disp, true, false),
            4 => self.mem(&[0x89], reg, X86_BASE, disp, false, false),
            2 => {
                self.code.push(0x66);
                self.mem(&[0x89], reg, X86_BASE, disp, false, false);
            }
            // The byte form needs a REX prefix to name `sil`/`dil`/`spl`/`bpl`;
            // emitting it unconditionally is correct for every register.
            1 => self.mem(&[0x88], reg, X86_BASE, disp, false, true),
            width => unreachable!("a compact image leaf is 1, 2, 4, or 8 bytes, not {width}"),
        }
    }

    fn zero_image_bytes(&mut self, offset: u32, len: u32) {
        for (position, width) in zero_runs(offset, len) {
            let disp = Self::frame_disp(position);
            match width {
                8 => {
                    self.mem(&[0xC7], 0, X86_BASE, disp, true, false);
                    self.code.extend_from_slice(&0u32.to_le_bytes());
                }
                4 => {
                    self.mem(&[0xC7], 0, X86_BASE, disp, false, false);
                    self.code.extend_from_slice(&0u32.to_le_bytes());
                }
                2 => {
                    self.code.push(0x66);
                    self.mem(&[0xC7], 0, X86_BASE, disp, false, false);
                    self.code.extend_from_slice(&0u16.to_le_bytes());
                }
                _ => {
                    self.mem(&[0xC6], 0, X86_BASE, disp, false, false);
                    self.code.push(0);
                }
            }
        }
    }

    fn load_result_register(&mut self, index: u32, offset: u32) {
        self.mem(
            &[0x8B],
            X86_RESULT_REGS[index as usize],
            X86_BASE,
            Self::frame_disp(offset),
            true,
            false,
        );
    }

    fn echo_sret_pointer(&mut self, offset: u32) {
        self.load_frame(X86_RESULT_REGS[0], offset);
    }

    fn copy_image_bytes(&mut self, source_offset: u32, byte_count: u32, max_width: u32) {
        // `rax` is free here: the native result registers were already staged
        // to the frame, and the C result registers are written after this.
        for (position, width) in image_runs(0, byte_count, max_width) {
            let source = Self::frame_disp(source_offset + position);
            let destination = Self::frame_disp(position);
            match width {
                8 => {
                    self.mem(&[0x8B], X86_RAX, X86_RSP, source, true, false);
                    self.mem(&[0x89], X86_RAX, X86_BASE, destination, true, false);
                }
                4 => {
                    self.mem(&[0x8B], X86_RAX, X86_RSP, source, false, false);
                    self.mem(&[0x89], X86_RAX, X86_BASE, destination, false, false);
                }
                2 => {
                    self.mem(&[0x0F, 0xB7], X86_RAX, X86_RSP, source, true, false);
                    self.code.push(0x66);
                    self.mem(&[0x89], X86_RAX, X86_BASE, destination, false, false);
                }
                _ => {
                    self.mem(&[0x0F, 0xB6], X86_RAX, X86_RSP, source, true, false);
                    self.mem(&[0x88], X86_RAX, X86_BASE, destination, false, true);
                }
            }
        }
    }
}

// ============================================================================
// AArch64 / AAPCS64
// ============================================================================

/// The dedicated base register, and the two scratch registers the addressing
/// fallbacks use. All three are caller-saved and outside every roster.
const A64_BASE: u32 = 9;
const A64_ADDR: u32 = 10;
const A64_VALUE: u32 = 11;
const A64_SP: u32 = 31;
const A64_FP: u32 = 29;
/// The dedicated indirect-result register (AAPCS64 section 6.9).
const A64_SRET: u32 = 8;
/// The largest 16-byte-aligned immediate one `sub sp, sp, #imm12` encodes.
const A64_MAX_SP_STEP: u32 = 4080;

#[derive(Default)]
struct Aarch64Emitter {
    words: Vec<u32>,
    relocations: Vec<EmittedRelocation>,
}

impl Aarch64Emitter {
    fn finish(self) -> MachineCode {
        let mut code = Vec::with_capacity(self.words.len() * 4);
        for word in self.words {
            code.extend_from_slice(&word.to_le_bytes());
        }
        MachineCode {
            code,
            relocations: self.relocations,
            strings: Vec::new(),
        }
    }

    /// `mov xd, #imm` through as many `movz`/`movk` as the value needs.
    fn move_immediate(&mut self, rd: u32, value: u32) {
        self.words.push(0xD280_0000 | ((value & 0xFFFF) << 5) | rd); // movz
        if value >> 16 != 0 {
            self.words
                .push(0xF2A0_0000 | (((value >> 16) & 0xFFFF) << 5) | rd); // movk lsl 16
        }
    }

    /// `add xd, xn, #imm`, materializing the immediate when it does not encode.
    /// `xn` may be the stack pointer.
    fn add_immediate(&mut self, rd: u32, rn: u32, imm: u32) {
        if imm <= 0xFFF {
            self.words.push(0x9100_0000 | (imm << 10) | (rn << 5) | rd);
            return;
        }
        self.move_immediate(A64_ADDR, imm);
        // ADD (extended register) so the stack pointer is addressable as `xn`.
        self.words
            .push(0x8B20_6000 | (A64_ADDR << 16) | (rn << 5) | rd);
    }

    /// The base register and byte offset an access of `size` bytes at
    /// `base + offset` uses, materializing the address when the scaled
    /// immediate cannot reach it.
    fn addressable(&mut self, base: u32, offset: u32, size: u32) -> (u32, u32) {
        if offset % size == 0 && offset / size <= 0xFFF {
            return (base, offset);
        }
        self.add_immediate(A64_ADDR, base, offset);
        (A64_ADDR, 0)
    }

    fn access(&mut self, opcode: u32, size: u32, rt: u32, base: u32, offset: u32) {
        let (base, offset) = self.addressable(base, offset, size);
        self.words
            .push(opcode | (((offset / size) & 0xFFF) << 10) | (base << 5) | rt);
    }

    /// `str xt, [base, #offset]`
    fn store64(&mut self, rt: u32, base: u32, offset: u32) {
        self.access(0xF900_0000, 8, rt, base, offset);
    }

    /// `ldr xt, [base, #offset]`
    fn load64(&mut self, rt: u32, base: u32, offset: u32) {
        self.access(0xF940_0000, 8, rt, base, offset);
    }

    /// The incoming argument area begins just past the frame record the
    /// prologue pushed.
    fn incoming_base(&mut self, offset: u32) -> u32 {
        self.add_immediate(A64_BASE, A64_FP, offset + 16);
        A64_BASE
    }
}

impl ThunkEmitter for Aarch64Emitter {
    fn prologue(&mut self, frame_bytes: u32) {
        self.words.push(0xA9BF_7BFD); // stp x29, x30, [sp, #-16]!
        self.words.push(0x9100_03FD); // mov x29, sp
        let mut remaining = frame_bytes;
        while remaining > 0 {
            let step = remaining.min(A64_MAX_SP_STEP);
            self.words
                .push(0xD100_0000 | (step << 10) | (A64_SP << 5) | A64_SP);
            remaining -= step;
        }
    }

    fn epilogue(&mut self, frame_bytes: u32) {
        let mut remaining = frame_bytes;
        while remaining > 0 {
            let step = remaining.min(A64_MAX_SP_STEP);
            self.words
                .push(0x9100_0000 | (step << 10) | (A64_SP << 5) | A64_SP);
            remaining -= step;
        }
        self.words.push(0xA8C1_7BFD); // ldp x29, x30, [sp], #16
        self.words.push(0xD65F_03C0); // ret
    }

    fn save_argument_register(&mut self, index: u32, offset: u32) {
        self.store64(index, A64_SP, offset);
    }

    fn save_sret_register(&mut self, offset: u32) {
        self.store64(A64_SRET, A64_SP, offset);
    }

    fn base_from_frame(&mut self, offset: u32) {
        self.add_immediate(A64_BASE, A64_SP, offset);
    }

    fn base_from_incoming(&mut self, offset: u32) {
        self.incoming_base(offset);
    }

    fn base_from_saved_pointer(&mut self, offset: u32) {
        self.load64(A64_BASE, A64_SP, offset);
    }

    fn base_from_incoming_pointer(&mut self, offset: u32) {
        let base = self.incoming_base(offset);
        self.load64(A64_BASE, base, 0);
    }

    fn load_leaf(&mut self, leaf: ImageLeaf, destination: u32) {
        let opcode = match (leaf.width, leaf.signed) {
            (8, _) => 0xF940_0000,
            (4, true) => 0xB980_0000,  // ldrsw x
            (4, false) => 0xB940_0000, // ldr w (zero-extends)
            (2, true) => 0x7980_0000,  // ldrsh x
            (2, false) => 0x7940_0000, // ldrh w
            (1, true) => 0x3980_0000,  // ldrsb x
            (1, false) => 0x3940_0000, // ldrb w
            (width, _) => unreachable!("a compact image leaf is 1, 2, 4, or 8 bytes, not {width}"),
        };
        self.access(opcode, leaf.width, A64_VALUE, A64_BASE, leaf.byte_offset);
        self.store64(A64_VALUE, A64_SP, destination);
    }

    fn store_base(&mut self, destination: u32) {
        self.store64(A64_BASE, A64_SP, destination);
    }

    fn load_argument_register(&mut self, index: u32, offset: u32) {
        self.load64(index, A64_SP, offset);
    }

    fn load_sret_register(&mut self, offset: u32) {
        self.load64(A64_SRET, A64_SP, offset);
    }

    fn call(&mut self, symbol: &str) {
        let offset = (self.words.len() * 4) as u64;
        self.words.push(0x9400_0000); // bl <native>
        self.relocations
            .push(EmittedRelocation::aarch64_call(offset, symbol));
    }

    fn store_return_register(&mut self, index: u32, leaf: ImageLeaf) {
        let opcode = match leaf.width {
            8 => 0xF900_0000,
            4 => 0xB900_0000,
            2 => 0x7900_0000,
            1 => 0x3900_0000,
            width => unreachable!("a compact image leaf is 1, 2, 4, or 8 bytes, not {width}"),
        };
        self.access(opcode, leaf.width, index, A64_BASE, leaf.byte_offset);
    }

    fn zero_image_bytes(&mut self, offset: u32, len: u32) {
        for (position, width) in zero_runs(offset, len) {
            let opcode = match width {
                8 => 0xF900_0000,
                4 => 0xB900_0000,
                2 => 0x7900_0000,
                _ => 0x3900_0000,
            };
            self.access(opcode, width, A64_SP, A64_BASE, position);
        }
    }

    fn load_result_register(&mut self, index: u32, offset: u32) {
        self.load64(index, A64_BASE, offset);
    }

    fn echo_sret_pointer(&mut self, _offset: u32) {
        unreachable!("AAPCS64 does not echo the indirect-result pointer");
    }

    fn copy_image_bytes(&mut self, source_offset: u32, byte_count: u32, max_width: u32) {
        for (position, width) in image_runs(0, byte_count, max_width) {
            let (load, store) = match width {
                8 => (0xF940_0000, 0xF900_0000),
                4 => (0xB940_0000, 0xB900_0000),
                2 => (0x7940_0000, 0x7900_0000),
                _ => (0x3940_0000, 0x3900_0000),
            };
            self.access(load, width, A64_VALUE, A64_SP, source_offset + position);
            self.access(store, width, A64_VALUE, A64_BASE, position);
        }
    }
}

/// The extension the native body's canonical form applies to a scalar
/// parameter, which the leaf loads above reproduce: a signed narrow leaf
/// sign-extends, an unsigned one zero-extends, a register-width leaf needs
/// nothing. Kept as a named projection so the export side and the shared
/// [`ScalarAbiExtension`] table can be compared in one assertion.
#[cfg(test)]
fn leaf_extension(leaf: ImageLeaf) -> ScalarAbiExtension {
    match (leaf.width, leaf.signed) {
        (8, _) => ScalarAbiExtension::None,
        (width, true) => ScalarAbiExtension::Signed {
            from_bits: width * 8,
        },
        (width, false) => ScalarAbiExtension::Unsigned {
            from_bits: width * 8,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::RelocationKind;

    fn scalar_facts(kind: rue_air::CAbiScalarKind) -> CAbiTypeFacts {
        CAbiTypeFacts::Scalar {
            kind,
            class: CRegisterClass::Gp,
        }
    }

    fn scalar_leaf(width: u32, signed: bool) -> ImageLeaf {
        ImageLeaf {
            byte_offset: 0,
            width,
            signed,
        }
    }

    fn scalar_parameter(
        kind: rue_air::CAbiScalarKind,
        width: u32,
        signed: bool,
    ) -> ExportParameter {
        ExportParameter {
            c: scalar_facts(kind),
            native: NativeParameter {
                leaves: vec![scalar_leaf(width, signed)],
                leaves_are_eightbytes: true,
            },
        }
    }

    fn word_parameter() -> ExportParameter {
        scalar_parameter(rue_air::CAbiScalarKind::RegisterWidth, 8, false)
    }

    /// Re-key a shared test signature to `target`'s own C row.
    ///
    /// These signatures are type facts, and the same facts are exercised on
    /// every target; the convention a real export carries comes from its
    /// declaration, and semantic analysis has already rejected a declaration
    /// naming a row the target does not implement, so a signature reaching the
    /// thunk always names one of the target's own rows.
    fn on(target: Target, signature: &ExportSignature) -> ExportSignature {
        ExportSignature {
            convention: target.c_calling_convention(),
            ..signature.clone()
        }
    }

    fn void_signature(parameters: Vec<ExportParameter>) -> ExportSignature {
        ExportSignature {
            convention: CallingConvention::X86_64SysV,
            parameters,
            result: CAbiTypeFacts::ZeroSized,
            result_is_aggregate: false,
            return_leaves_are_eightbytes: true,
            return_bytes: 0,
            return_leaves: Vec::new(),
            return_padding: Vec::new(),
        }
    }

    /// A `{i64, i64, i64}`-shaped export: 24 bytes, three leaves that are its
    /// own eightbytes, so both sides place it byval-on-stack (SysV) or
    /// by-reference (AAPCS64), returning through sret.
    fn triple_signature() -> ExportSignature {
        let leaves = vec![
            ImageLeaf {
                byte_offset: 0,
                width: 8,
                signed: false,
            },
            ImageLeaf {
                byte_offset: 8,
                width: 8,
                signed: false,
            },
            ImageLeaf {
                byte_offset: 16,
                width: 8,
                signed: false,
            },
        ];
        ExportSignature {
            convention: CallingConvention::X86_64SysV,
            parameters: vec![ExportParameter {
                c: CAbiTypeFacts::integer_aggregate(24, 8),
                native: NativeParameter {
                    leaves: leaves.clone(),
                    leaves_are_eightbytes: true,
                },
            }],
            result: CAbiTypeFacts::integer_aggregate(24, 8),
            result_is_aggregate: true,
            return_leaves_are_eightbytes: true,
            return_bytes: 24,
            return_leaves: leaves,
            return_padding: Vec::new(),
        }
    }

    /// A `{i32, i32}`-shaped export: 8 bytes, two narrow leaves, so the native
    /// side crosses indirectly in both directions while the C side uses one
    /// register each way.
    fn pair_signature() -> ExportSignature {
        let leaves = vec![
            ImageLeaf {
                byte_offset: 0,
                width: 4,
                signed: true,
            },
            ImageLeaf {
                byte_offset: 4,
                width: 4,
                signed: true,
            },
        ];
        ExportSignature {
            convention: CallingConvention::X86_64SysV,
            parameters: vec![ExportParameter {
                c: CAbiTypeFacts::integer_aggregate(8, 4),
                native: NativeParameter {
                    leaves: leaves.clone(),
                    leaves_are_eightbytes: false,
                },
            }],
            result: CAbiTypeFacts::integer_aggregate(8, 4),
            result_is_aggregate: true,
            return_leaves_are_eightbytes: false,
            return_bytes: 8,
            return_leaves: leaves,
            return_padding: Vec::new(),
        }
    }

    #[test]
    fn a_scalars_only_signature_lowers_identically_as_an_import_and_an_export() {
        // Both directions of one `extern "C"` signature read the same
        // `LoweredSignature`, whatever the shapes involved: a scalars-only
        // signature has no aggregate to route it anywhere else. Nine arguments
        // reach the stacked tail on every row, and the narrow ones are where the
        // Apple row's natural-size packing would differ if the two directions
        // ever classified separately.
        use crate::foreign_call::{ForeignArg, ForeignCallInputs, ForeignReturn};
        use rue_air::CAbiScalarKind;
        use rue_cfg::CfgValue;

        let kinds = [
            CAbiScalarKind::RegisterWidth,
            CAbiScalarKind::I8,
            CAbiScalarKind::U16,
            CAbiScalarKind::I32,
            CAbiScalarKind::Bool,
            CAbiScalarKind::RegisterWidth,
            CAbiScalarKind::U32,
            CAbiScalarKind::I16,
            CAbiScalarKind::U8,
        ];
        let result = scalar_facts(CAbiScalarKind::I16);
        for target in Target::all() {
            let convention = target.c_calling_convention();
            let import = ForeignCallInputs::new(
                "f".into(),
                convention,
                kinds
                    .iter()
                    .enumerate()
                    .map(|(index, &kind)| ForeignArg::Scalar {
                        value: CfgValue::from_raw(index as u32),
                        kind,
                    })
                    .collect(),
                ForeignReturn::Scalar,
                result,
            );
            let export = ExportSignature {
                convention,
                parameters: kinds
                    .iter()
                    .map(|&kind| ExportParameter {
                        c: scalar_facts(kind),
                        native: NativeParameter {
                            leaves: vec![scalar_leaf(kind.natural_bytes(), false)],
                            leaves_are_eightbytes: true,
                        },
                    })
                    .collect(),
                result,
                result_is_aggregate: false,
                return_leaves_are_eightbytes: true,
                return_bytes: 8,
                return_leaves: vec![scalar_leaf(2, true)],
                return_padding: Vec::new(),
            };
            assert_eq!(
                *import.signature(),
                export.lowered(),
                "{target:?}: an import and an export of one signature place it identically"
            );
        }
    }

    #[test]
    fn a_scalar_passthrough_forwards_with_one_call_relocation() {
        for (target, kind) in [
            (Target::X86_64Linux, RelocationKind::X86Plt32),
            (Target::Aarch64Linux, RelocationKind::Aarch64Call26),
            (Target::Aarch64Macos, RelocationKind::Aarch64Call26),
        ] {
            let code = generate_export_thunk(
                target,
                "__rue_sem_native",
                &on(
                    target,
                    &void_signature(vec![word_parameter(), word_parameter()]),
                ),
            );
            assert_eq!(code.relocations.len(), 1, "{target:?}");
            assert_eq!(code.relocations[0].kind, kind);
            assert_eq!(code.relocations[0].symbol, "__rue_sem_native");
            assert!(code.strings.is_empty());
        }
    }

    #[test]
    fn every_target_row_reads_its_own_convention() {
        // The Apple row differs from generic AAPCS64 only in stacked-argument
        // packing, so a nine-scalar export — whose ninth argument is stacked —
        // is where the two rows diverge, and an all-register export is where
        // they agree.
        let registers = void_signature(vec![word_parameter(); 4]);
        assert_eq!(
            generate_export_thunk(
                Target::Aarch64Linux,
                "native",
                &on(Target::Aarch64Linux, &registers)
            )
            .code,
            generate_export_thunk(
                Target::Aarch64Macos,
                "native",
                &on(Target::Aarch64Macos, &registers)
            )
            .code
        );

        let narrow = void_signature(
            std::iter::repeat_with(word_parameter)
                .take(8)
                .chain([scalar_parameter(rue_air::CAbiScalarKind::I8, 1, true)])
                .collect(),
        );
        let linux = generate_export_thunk(
            Target::Aarch64Linux,
            "native",
            &on(Target::Aarch64Linux, &narrow),
        );
        let darwin = generate_export_thunk(
            Target::Aarch64Macos,
            "native",
            &on(Target::Aarch64Macos, &narrow),
        );
        // Both rows read a one-byte stacked argument at offset 0, so the codes
        // agree here too; what differs is where a *second* stacked argument
        // would land, which the lowered-signature tests pin directly.
        assert_eq!(linux.code, darwin.code);
    }

    #[test]
    fn a_narrow_argument_is_extended_into_its_native_slot() {
        // The extension a leaf load applies is the shared scalar table's,
        // element for element.
        for (kind, width, signed) in [
            (rue_air::CAbiScalarKind::I8, 1, true),
            (rue_air::CAbiScalarKind::U8, 1, false),
            (rue_air::CAbiScalarKind::I16, 2, true),
            (rue_air::CAbiScalarKind::U16, 2, false),
            (rue_air::CAbiScalarKind::I32, 4, true),
            (rue_air::CAbiScalarKind::U32, 4, false),
            (rue_air::CAbiScalarKind::RegisterWidth, 8, false),
        ] {
            assert_eq!(
                leaf_extension(scalar_leaf(width, signed)),
                kind.extension(),
                "{kind:?} must load through its canonical extension"
            );
        }
    }

    #[test]
    fn a_nine_scalar_export_spills_its_tail_on_both_rows() {
        let signature = void_signature(vec![word_parameter(); 9]);
        for target in [Target::X86_64Linux, Target::Aarch64Linux] {
            let plan = ThunkPlan::new(target, &on(target, &signature));
            assert_eq!(plan.slots.len(), 9);
            let registers = plan.register_loads.len() as u32;
            assert_eq!(
                registers,
                u32::from(target.c_calling_convention().c_spec().gp_argument_registers)
            );
            // The stacked native values start at the base of the outgoing area.
            assert_eq!(plan.slot_offsets[registers as usize], 0);
            // Every C argument beyond the roster is read from the caller's own
            // outgoing area.
            let stacked = signature.parameters.len() as u32 - registers;
            assert_eq!(
                plan.c
                    .arguments()
                    .iter()
                    .filter(|argument| matches!(argument.location, ArgLocation::Stack { .. }))
                    .count() as u32,
                stacked
            );
        }
    }

    #[test]
    fn a_direct_multislot_parameter_reaches_the_body_in_ascending_memory_order() {
        for target in [Target::X86_64Linux, Target::Aarch64Linux] {
            let plan = ThunkPlan::new(target, &on(target, &triple_signature()));
            // The native return is three slot-identical slots, under both
            // budgets, so it comes back in registers and there is no hidden
            // pointer ahead of the user argument.
            assert_eq!(
                plan.native_return,
                NativeReturn::Registers {
                    leaves: plan.return_leaves.clone()
                }
            );
            // 24 bytes: SysV stacks the value byval, so its three leaves cross
            // in ascending memory order; AAPCS64 passes one pointer to a
            // caller-owned copy, so only the image address crosses.
            let sources = plan
                .slots
                .iter()
                .map(|(source, _)| *source)
                .collect::<Vec<_>>();
            let expected = if target == Target::X86_64Linux {
                vec![
                    NativeSlotSource::Leaf {
                        parameter: 0,
                        leaf: 0,
                    },
                    NativeSlotSource::Leaf {
                        parameter: 0,
                        leaf: 1,
                    },
                    NativeSlotSource::Leaf {
                        parameter: 0,
                        leaf: 2,
                    },
                ]
            } else {
                vec![NativeSlotSource::ImageAddress { parameter: 0 }]
            };
            assert_eq!(
                sources, expected,
                "the native convention places a value's leaves in ascending memory order"
            );
            // The C return is 24 bytes, so it crosses through caller storage,
            // which is where the thunk assembles the image.
            assert!(matches!(
                plan.return_image,
                Some(ReturnImage::CallerStorage { .. })
            ));
        }
    }

    #[test]
    fn the_two_conventions_disagree_about_a_24_byte_argument() {
        // SysV passes it byval on the stack; AAPCS64 passes a pointer to a
        // caller-owned copy. Either way the thunk reads the C caller's bytes in
        // place and never repacks them.
        let sysv = ThunkPlan::new(
            Target::X86_64Linux,
            &on(Target::X86_64Linux, &triple_signature()),
        );
        assert!(matches!(sysv.bases[0], ImageBase::Incoming { offset: 0 }));
        let aapcs = ThunkPlan::new(
            Target::Aarch64Linux,
            &on(Target::Aarch64Linux, &triple_signature()),
        );
        assert!(matches!(aapcs.bases[0], ImageBase::SavedPointer { .. }));
    }

    #[test]
    fn a_packed_pair_crosses_as_one_eightbyte_of_its_image() {
        for target in [Target::X86_64Linux, Target::Aarch64Linux] {
            let plan = ThunkPlan::new(target, &on(target, &pair_signature()));
            // Eight bytes of narrow fields: both conventions return the one
            // eightbyte of the value's compact image in the first result
            // register, and the argument's two leaves pack into the one
            // eightbyte the convention gives them.
            assert_eq!(plan.native_return, NativeReturn::Eightbytes { count: 1 });
            assert_eq!(
                plan.slots
                    .iter()
                    .map(|(source, _)| *source)
                    .collect::<Vec<_>>(),
                vec![NativeSlotSource::Eightbyte {
                    parameter: 0,
                    index: 0
                }],
                "no hidden return pointer crosses, so the packed pair takes the \
                 first argument register"
            );
            // The eightbyte the native body returned is the eightbyte C wants.
            assert_eq!(plan.return_image, None);
            // The argument arrived in one register, so its image is the saved
            // register cell.
            assert!(matches!(plan.bases[0], ImageBase::Frame { .. }));
        }
    }

    /// A `{i32 x 5}`-shaped export: 20 bytes, five narrow leaves that pack into
    /// three eightbytes, so the native convention returns those eightbytes in
    /// three result registers while every C row returns the value indirectly.
    fn wide_packed_signature() -> ExportSignature {
        let leaves: Vec<ImageLeaf> = (0..5)
            .map(|index| ImageLeaf {
                byte_offset: index * 4,
                width: 4,
                signed: true,
            })
            .collect();
        ExportSignature {
            convention: CallingConvention::X86_64SysV,
            parameters: Vec::new(),
            result: CAbiTypeFacts::integer_aggregate(20, 4),
            result_is_aggregate: true,
            return_leaves_are_eightbytes: false,
            return_bytes: 20,
            return_leaves: leaves,
            return_padding: Vec::new(),
        }
    }

    #[test]
    fn a_result_the_native_bank_holds_and_c_returns_indirectly_is_staged_and_copied() {
        // The native body hands back three whole eightbytes; the C caller's
        // storage is exactly twenty bytes, so the eightbytes are staged where a
        // whole-eightbyte store is in bounds and only the result's own bytes
        // are copied across.
        for target in [
            Target::X86_64Linux,
            Target::Aarch64Linux,
            Target::Aarch64Macos,
        ] {
            let signature = on(target, &wide_packed_signature());
            let plan = ThunkPlan::new(target, &signature);
            assert_eq!(
                plan.native_return,
                NativeReturn::Eightbytes { count: 3 },
                "{target:?}: the native bank holds three eightbytes"
            );
            assert!(
                plan.c.ret().uses_sret(),
                "{target:?}: C returns 20 bytes indirectly"
            );
            let Some(ReturnImage::StagedCallerStorage { bytes, align, .. }) = plan.return_image
            else {
                panic!(
                    "{target:?}: expected a staged copy, got {:?}",
                    plan.return_image
                );
            };
            assert_eq!((bytes, align), (20, 4));
            let code = generate_export_thunk(target, "native", &signature);
            assert!(!code.code.is_empty(), "{target:?} must encode the thunk");
            if target.arch() == Arch::Aarch64 {
                assert_eq!(code.code.len() % 4, 0);
            }
        }
    }

    #[test]
    fn only_sysv_echoes_the_indirect_result_pointer() {
        let sysv = ThunkPlan::new(
            Target::X86_64Linux,
            &on(Target::X86_64Linux, &triple_signature()),
        );
        assert!(matches!(
            sysv.c.ret(),
            LoweredReturn::Sret {
                register: SretRegisterKind::ArgumentRegister,
                echoed: true,
                ..
            }
        ));
        // The pointer is argument register zero, so it is already saved there.
        assert_eq!(sysv.c_sret_offset, Some(sysv.save_base));

        let aapcs = ThunkPlan::new(
            Target::Aarch64Linux,
            &on(Target::Aarch64Linux, &triple_signature()),
        );
        assert!(matches!(
            aapcs.c.ret(),
            LoweredReturn::Sret {
                register: SretRegisterKind::DedicatedRegister,
                echoed: false,
                ..
            }
        ));
        // `x8` is outside the argument roster, so it gets its own cell.
        assert_eq!(aapcs.c_sret_offset, Some(aapcs.save_base + 8 * 8));
    }

    #[test]
    fn a_thunk_frame_is_call_aligned_on_every_row() {
        for signature in [
            void_signature(vec![word_parameter(); 9]),
            triple_signature(),
            pair_signature(),
            void_signature(Vec::new()),
        ] {
            for target in [
                Target::X86_64Linux,
                Target::Aarch64Linux,
                Target::Aarch64Macos,
            ] {
                let plan = ThunkPlan::new(target, &on(target, &signature));
                assert_eq!(
                    plan.frame_bytes % 16,
                    0,
                    "{target:?} must keep the stack 16-byte aligned at the call"
                );
            }
        }
    }

    #[test]
    fn x86_encodes_the_expected_frame_and_forwarding_shape() {
        let code = generate_export_thunk(
            Target::X86_64Linux,
            "native",
            &on(Target::X86_64Linux, &void_signature(vec![word_parameter()])),
        );
        // push rbp ; mov rbp, rsp ; sub rsp, imm32
        assert_eq!(&code.code[..4], &[0x55, 0x48, 0x89, 0xE5]);
        assert_eq!(code.code[4], 0x48);
        assert_eq!(&code.code[5..7], &[0x81, 0xEC]);
        // add rsp, imm32 ; pop rbp ; ret
        let tail = &code.code[code.code.len() - 9..];
        assert_eq!(&tail[..3], &[0x48, 0x81, 0xC4]);
        assert_eq!(&tail[7..], &[0x5D, 0xC3]);
    }

    #[test]
    fn aarch64_encodes_the_expected_frame_and_forwarding_shape() {
        let code = generate_export_thunk(
            Target::Aarch64Linux,
            "native",
            &on(
                Target::Aarch64Linux,
                &void_signature(vec![word_parameter()]),
            ),
        );
        assert_eq!(code.code.len() % 4, 0);
        let word = |index: usize| {
            u32::from_le_bytes(code.code[index * 4..index * 4 + 4].try_into().unwrap())
        };
        assert_eq!(word(0), 0xA9BF_7BFD, "stp x29, x30, [sp, #-16]!");
        assert_eq!(word(1), 0x9100_03FD, "mov x29, sp");
        let words = code.code.len() / 4;
        assert_eq!(word(words - 2), 0xA8C1_7BFD, "ldp x29, x30, [sp], #16");
        assert_eq!(word(words - 1), 0xD65F_03C0, "ret");
    }

    #[test]
    fn every_leaf_width_encodes_on_both_backends() {
        // One export per leaf width and signedness, so every load and store
        // form in both emitters is exercised at least once.
        for (width, signed) in [
            (1u32, true),
            (1, false),
            (2, true),
            (2, false),
            (4, true),
            (4, false),
            (8, false),
        ] {
            let leaves = vec![ImageLeaf {
                byte_offset: 0,
                width,
                signed,
            }];
            let signature = ExportSignature {
                convention: CallingConvention::X86_64SysV,
                parameters: vec![ExportParameter {
                    c: CAbiTypeFacts::integer_aggregate(width.into(), width.into()),
                    native: NativeParameter {
                        leaves: leaves.clone(),
                        leaves_are_eightbytes: true,
                    },
                }],
                result: CAbiTypeFacts::integer_aggregate(width.into(), width.into()),
                result_is_aggregate: true,
                return_leaves_are_eightbytes: true,
                return_bytes: width.into(),
                return_leaves: leaves,
                return_padding: vec![PaddingRange {
                    start: u64::from(width),
                    end: u64::from(width) + 1,
                }],
            };
            for target in [Target::X86_64Linux, Target::Aarch64Linux] {
                let code = generate_export_thunk(target, "native", &on(target, &signature));
                assert!(
                    !code.code.is_empty(),
                    "{target:?} must encode a {width}-byte leaf"
                );
                if target.arch() == Arch::Aarch64 {
                    assert_eq!(code.code.len() % 4, 0);
                }
            }
        }
    }

    #[test]
    fn a_padding_run_is_zeroed_with_naturally_aligned_stores() {
        assert_eq!(zero_runs(0, 8), vec![(0, 8)]);
        assert_eq!(zero_runs(4, 4), vec![(4, 4)]);
        assert_eq!(zero_runs(1, 3), vec![(1, 1), (2, 2)]);
        assert_eq!(zero_runs(6, 10), vec![(6, 2), (8, 8)]);
        for (offset, width) in zero_runs(3, 13) {
            assert_eq!(offset % width, 0, "a zeroing store must be aligned");
        }
    }
}
