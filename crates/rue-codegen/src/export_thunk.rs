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
//! The native side is the *other* convention, and it is unchanged by this
//! module: a by-value aggregate the native classifier rules indirect crosses as
//! one pointer to its compact memory image; a direct multi-slot aggregate
//! crosses one slot per flattened leaf with the slots **reversed**; a hidden
//! sret pointer is native ABI slot 0; every scalar slot carries Rue's canonical
//! 64-bit extension. [`ExportSignature`] is the pairing of the two views, built
//! once from the type pool by [`ExportSignature::for_types`].
//!
//! ## Why the compact image is the C image
//!
//! Under the compact-layout default a `@repr(c)` aggregate's physical memory
//! image *is* its C object layout, and the native convention's indirect
//! transports (an indirect by-value argument, an sret return) already pass that
//! exact image through memory. So the thunk never repacks those: it hands the C
//! caller's own bytes to the native body, and hands the native body's sret
//! storage — the C caller's storage, when the C return is also indirect — back.
//! Only a *direct* native crossing needs marshaling, and then only the leaf
//! loads and stores the compact image map already describes.
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
    LoweredSignature, NativeAbiTypeFacts, NativeCallAbi, PaddingRange, PointerLocation, Type,
    lower_c_signature, native_return_register_budget,
};
use rue_target::{Arch, CRegisterClass, SretRegisterKind, Target};

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

/// How the native body receives one parameter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NativeParameter {
    /// One native ABI slot per flattened leaf, in ascending image order. A
    /// multi-slot aggregate's slots are **reversed** before placement, which is
    /// the native convention's rule.
    Direct {
        /// The value's flattened leaves, in ascending image order.
        leaves: Vec<ImageLeaf>,
        /// Whether the native convention reverses this value's slots.
        reversed: bool,
    },
    /// One pointer to the value's compact memory image, which the callee
    /// unmarshals at entry.
    Indirect,
}

/// How the native body returns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NativeReturn {
    /// Nothing.
    Void,
    /// One scalar in the primary result register, already in Rue's canonical
    /// form — which is exactly what a C caller accepts, so it passes through.
    Scalar,
    /// One slot per flattened leaf in the return registers, in ascending slot
    /// order; each leaf names where that slot belongs in the C image.
    Registers {
        /// The value's flattened leaves, in ascending image order.
        leaves: Vec<ImageLeaf>,
    },
    /// The body writes the value's compact image into caller storage whose
    /// address is the hidden first native ABI slot.
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
    parameters: Vec<ExportParameter>,
    result: CAbiTypeFacts,
    /// The return's native classification kernel input. The native decision
    /// also needs the target's return-register budget, which is an
    /// architecture fact the thunk generator supplies, so the facts travel and
    /// the classification happens at generation.
    return_facts: NativeAbiTypeFacts,
    return_leaves: Vec<ImageLeaf>,
    return_padding: Vec<PaddingRange>,
}

impl ExportSignature {
    /// Project an export's signature from the live type pool.
    ///
    /// `param_types` are the export's declared parameter types in source order
    /// and `return_type` its result. Semantic analysis has already gated the
    /// signature through `c_passable_by_value`, so every type here is a
    /// C-passable scalar, pointer, or `@repr(c)` aggregate, and every parameter
    /// is by value.
    pub fn for_types(
        type_pool: &FrozenTypeInternPool,
        param_types: &[Type],
        return_type: Type,
    ) -> Self {
        let native = NativeCallAbi::for_arguments(type_pool);
        let parameters = param_types
            .iter()
            .map(|&ty| ExportParameter {
                c: rue_air::c_abi_type_facts(type_pool, ty),
                native: match native.classify_arg(ty, ArgConvention::ByValue) {
                    rue_air::ArgClass::Indirect => NativeParameter::Indirect,
                    // A zero-sized by-value parameter still occupies one
                    // incoming argument slot in the callee's parameter layout
                    // (`SourceParamAbi` clamps its width to one), so the thunk
                    // supplies one, holding no meaningful value.
                    rue_air::ArgClass::Omitted => NativeParameter::Direct {
                        leaves: Vec::new(),
                        reversed: false,
                    },
                    rue_air::ArgClass::Direct { .. } => NativeParameter::Direct {
                        leaves: image_leaves(type_pool, ty),
                        reversed: crate::types::is_multislot_aggregate(type_pool, ty),
                    },
                },
            })
            .collect();
        Self {
            parameters,
            result: rue_air::c_abi_type_facts(type_pool, return_type),
            return_facts: native_return_facts(type_pool, return_type),
            return_leaves: image_leaves(type_pool, return_type),
            return_padding: type_pool.compact_image_padding_ranges(return_type),
        }
    }

    /// The lowered C signature a caller of this export writes and this thunk
    /// reads, under `target`'s `"C"` alias.
    pub fn lowered(&self, target: Target) -> LoweredSignature {
        let parameters = self
            .parameters
            .iter()
            .map(|parameter| (parameter.c, ArgConvention::ByValue))
            .collect::<Vec<_>>();
        lower_c_signature(target.c_calling_convention(), &parameters, self.result)
    }

    /// How the native body returns on `target`, whose architecture fixes the
    /// return-register budget.
    fn native_return(&self, target: Target) -> NativeReturn {
        match self
            .return_facts
            .classify_return(native_return_register_budget(target.arch()))
        {
            rue_air::ReturnClass::ZeroSized => NativeReturn::Void,
            rue_air::ReturnClass::Scalar => NativeReturn::Scalar,
            rue_air::ReturnClass::Registers { .. } => NativeReturn::Registers {
                leaves: self.return_leaves.clone(),
            },
            rue_air::ReturnClass::Indirect { .. } => NativeReturn::Sret,
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

/// The native classification kernel input for an export's return type.
fn native_return_facts(type_pool: &FrozenTypeInternPool, ty: Type) -> NativeAbiTypeFacts {
    let abi_slots = type_pool.abi_slot_count(ty);
    NativeAbiTypeFacts {
        abi_slots,
        aggregate: crate::types::is_multislot_aggregate(type_pool, ty),
        // An export's return is a C-passable `@repr(c)` type; the canonical
        // `StrBuf` is not one, so it never reaches this projection.
        strbuf: false,
        slot_identical: rue_air::is_slot_identical_layout(type_pool, ty),
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

/// Where one native ABI slot's value comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NativeSlotSource {
    /// The address of the hidden return storage the native body writes.
    ReturnStorage,
    /// The address of parameter `parameter`'s compact image.
    ImageAddress { parameter: usize },
    /// Leaf `leaf` of parameter `parameter`'s compact image.
    Leaf { parameter: usize, leaf: usize },
    /// A slot the callee's parameter layout reserves for a zero-sized by-value
    /// parameter, which holds no value.
    Empty,
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
    /// Every native ABI slot, in native order.
    slots: Vec<NativeSlotSource>,
    /// The return value's leaves, when the native body returns in registers.
    return_leaves: Vec<ImageLeaf>,
    return_padding: Vec<PaddingRange>,
    /// Frame offset of the staging cell for native slot `k` (`k` < the native
    /// argument roster), or the outgoing native stack offset otherwise.
    slot_offsets: Vec<u32>,
    /// How many native slots travel in argument registers.
    register_slots: u32,
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
}

impl ThunkPlan {
    fn new(target: Target, signature: &ExportSignature) -> Self {
        let c = signature.lowered(target);
        let spec = c.spec();
        let native_return = signature.native_return(target);
        let native_arg_registers = native_argument_register_count(target);

        // Every incoming general-purpose argument register is saved to a
        // contiguous cell block, so a register-passed value's eightbytes are
        // addressable as one image with no repacking.
        let save_cells = spec.gp_argument_registers + 1;

        let mut slots = Vec::new();
        if matches!(native_return, NativeReturn::Sret) {
            slots.push(NativeSlotSource::ReturnStorage);
        }
        let mut leaves = Vec::with_capacity(signature.parameters.len());
        for (parameter, description) in signature.parameters.iter().enumerate() {
            match &description.native {
                NativeParameter::Indirect => {
                    slots.push(NativeSlotSource::ImageAddress { parameter });
                    leaves.push(Vec::new());
                }
                NativeParameter::Direct {
                    leaves: value_leaves,
                    reversed,
                } => {
                    if value_leaves.is_empty() {
                        slots.push(NativeSlotSource::Empty);
                    } else {
                        let indices = 0..value_leaves.len();
                        let ordered: Vec<usize> = if *reversed {
                            indices.rev().collect()
                        } else {
                            indices.collect()
                        };
                        slots.extend(
                            ordered
                                .into_iter()
                                .map(|leaf| NativeSlotSource::Leaf { parameter, leaf }),
                        );
                    }
                    leaves.push(value_leaves.clone());
                }
            }
        }

        let slot_count = u32::try_from(slots.len()).expect("an export has fewer than 2^32 slots");
        let register_slots = slot_count.min(native_arg_registers);
        let stack_slots = slot_count - register_slots;
        // The outgoing native argument area sits at the base of the frame, so
        // it is the block the native callee addresses from its own entry stack
        // pointer. Rounding it to the call-boundary alignment is what keeps the
        // whole frame, and therefore the stack at the call, 16-byte aligned.
        let native_stack_bytes = align_up(
            stack_slots
                .checked_mul(8)
                .expect("an export thunk frame fits u32"),
            16,
        );

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
            (LoweredReturn::Sret { .. }, _) => Some(ReturnImage::CallerStorage {
                pointer_offset: c_sret_offset.expect("an indirect C return saves its pointer"),
            }),
            (LoweredReturn::Registers { count, .. }, NativeReturn::Registers { .. })
            | (LoweredReturn::Registers { count, .. }, NativeReturn::Sret)
                if !matches!(signature.result, CAbiTypeFacts::Scalar { .. }) =>
            {
                let offset = align_up(next, 16);
                next = offset + align_up(count * 8, 16);
                Some(ReturnImage::Scratch { offset })
            }
            _ => None,
        };

        let frame_bytes = align_up(next, 16);

        let bases = signature
            .parameters
            .iter()
            .zip(c.arguments())
            .map(|(_, argument)| match argument.location {
                ArgLocation::Registers {
                    class, first_index, ..
                } => {
                    // The register save block holds the general-purpose roster,
                    // and a register-passed value's eightbytes are contiguous in
                    // it, so its image needs no repacking. Nothing reaches the
                    // floating-point roster while the C boundary rejects floats.
                    assert_eq!(
                        class,
                        CRegisterClass::Gp,
                        "an export argument still crosses only in general-purpose registers"
                    );
                    ImageBase::Frame {
                        offset: save_base + first_index * 8,
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

        let slot_offsets = (0..slot_count)
            .map(|index| {
                if index < register_slots {
                    stage_base + index * 8
                } else {
                    (index - register_slots) * 8
                }
            })
            .collect();

        Self {
            c,
            native_return,
            bases,
            leaves,
            slots,
            return_leaves: signature.return_leaves.clone(),
            return_padding: signature.return_padding.clone(),
            slot_offsets,
            register_slots,
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

        for (index, source) in self.slots.iter().enumerate() {
            let destination = self.slot_offsets[index];
            match *source {
                NativeSlotSource::Empty => emitter.zero_slot(destination),
                NativeSlotSource::ReturnStorage => {
                    match self.return_image.expect("an sret return names its storage") {
                        ReturnImage::CallerStorage { pointer_offset } => {
                            emitter.base_from_saved_pointer(pointer_offset)
                        }
                        ReturnImage::Scratch { offset } => emitter.base_from_frame(offset),
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
            }
        }

        for index in 0..self.register_slots {
            emitter.load_argument_register(index, self.slot_offsets[index as usize]);
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
            ReturnImage::Scratch { offset } => emitter.base_from_frame(offset),
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
            LoweredReturn::Registers { class, count, .. } => {
                assert_eq!(
                    class,
                    CRegisterClass::Gp,
                    "the C boundary still returns only general-purpose values"
                );
                for index in 0..count {
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

/// How many general-purpose argument registers the native convention uses on
/// `target`. The native roster is the target's ordinary integer argument
/// roster, which is the same size as the platform C one.
fn native_argument_register_count(target: Target) -> u32 {
    match target.arch() {
        Arch::X86_64 => 6,
        Arch::Aarch64 => 8,
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
    /// frame + `destination` := 0.
    fn zero_slot(&mut self, destination: u32);
    /// Native argument register `index` := frame + `offset`.
    fn load_argument_register(&mut self, index: u32, offset: u32);
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
}

/// Zeroing a padding run, largest naturally-aligned store first.
fn zero_runs(offset: u32, len: u32) -> Vec<(u32, u32)> {
    let mut runs = Vec::new();
    let mut position = offset;
    let end = offset + len;
    while position < end {
        let mut width = 8;
        while width > 1 && (position % width != 0 || position + width > end) {
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

    fn zero_slot(&mut self, destination: u32) {
        // mov qword [rsp + destination], 0
        self.mem(
            &[0xC7],
            0,
            X86_RSP,
            Self::frame_disp(destination),
            true,
            false,
        );
        self.code.extend_from_slice(&0u32.to_le_bytes());
    }

    fn load_argument_register(&mut self, index: u32, offset: u32) {
        self.load_frame(X86_ARG_REGS[index as usize], offset);
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

    fn zero_slot(&mut self, destination: u32) {
        self.store64(A64_SP, A64_SP, destination); // `xzr` shares the encoding
    }

    fn load_argument_register(&mut self, index: u32, offset: u32) {
        self.load64(index, A64_SP, offset);
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
            native: NativeParameter::Direct {
                leaves: vec![scalar_leaf(width, signed)],
                reversed: false,
            },
        }
    }

    fn word_parameter() -> ExportParameter {
        scalar_parameter(rue_air::CAbiScalarKind::RegisterWidth, 8, false)
    }

    fn void_signature(parameters: Vec<ExportParameter>) -> ExportSignature {
        ExportSignature {
            parameters,
            result: CAbiTypeFacts::ZeroSized,
            return_facts: NativeAbiTypeFacts {
                abi_slots: 0,
                aggregate: false,
                strbuf: false,
                slot_identical: true,
            },
            return_leaves: Vec::new(),
            return_padding: Vec::new(),
        }
    }

    /// A `{i64, i64, i64}`-shaped export: 24 bytes, three slot-identical
    /// leaves, so the native side is direct-and-reversed and the C side is
    /// byval-on-stack (SysV) or by-reference (AAPCS64), returning through sret.
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
            parameters: vec![ExportParameter {
                c: CAbiTypeFacts::Aggregate { size: 24, align: 8 },
                native: NativeParameter::Direct {
                    leaves: leaves.clone(),
                    reversed: true,
                },
            }],
            result: CAbiTypeFacts::Aggregate { size: 24, align: 8 },
            return_facts: NativeAbiTypeFacts {
                abi_slots: 3,
                aggregate: true,
                strbuf: false,
                slot_identical: true,
            },
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
            parameters: vec![ExportParameter {
                c: CAbiTypeFacts::Aggregate { size: 8, align: 4 },
                native: NativeParameter::Indirect,
            }],
            result: CAbiTypeFacts::Aggregate { size: 8, align: 4 },
            return_facts: NativeAbiTypeFacts {
                abi_slots: 2,
                aggregate: true,
                strbuf: false,
                slot_identical: false,
            },
            return_leaves: leaves,
            return_padding: Vec::new(),
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
                &void_signature(vec![word_parameter(), word_parameter()]),
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
            generate_export_thunk(Target::Aarch64Linux, "native", &registers).code,
            generate_export_thunk(Target::Aarch64Macos, "native", &registers).code
        );

        let narrow = void_signature(
            std::iter::repeat_with(word_parameter)
                .take(8)
                .chain([scalar_parameter(rue_air::CAbiScalarKind::I8, 1, true)])
                .collect(),
        );
        let linux = generate_export_thunk(Target::Aarch64Linux, "native", &narrow);
        let darwin = generate_export_thunk(Target::Aarch64Macos, "native", &narrow);
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
            let plan = ThunkPlan::new(target, &signature);
            assert_eq!(plan.slots.len(), 9);
            let registers = native_argument_register_count(target);
            assert_eq!(plan.register_slots, registers);
            // The stacked native slots start at the base of the outgoing area.
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
    fn a_direct_multislot_parameter_reaches_the_body_in_reversed_slot_order() {
        for target in [Target::X86_64Linux, Target::Aarch64Linux] {
            let plan = ThunkPlan::new(target, &triple_signature());
            // The native return is three slot-identical slots, under both
            // budgets, so it comes back in registers and there is no hidden
            // pointer ahead of the user argument.
            assert_eq!(
                plan.native_return,
                NativeReturn::Registers {
                    leaves: plan.return_leaves.clone()
                }
            );
            assert_eq!(
                plan.slots,
                vec![
                    NativeSlotSource::Leaf {
                        parameter: 0,
                        leaf: 2
                    },
                    NativeSlotSource::Leaf {
                        parameter: 0,
                        leaf: 1
                    },
                    NativeSlotSource::Leaf {
                        parameter: 0,
                        leaf: 0
                    },
                ],
                "the native convention reverses a multi-slot value's slots"
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
        let sysv = ThunkPlan::new(Target::X86_64Linux, &triple_signature());
        assert!(matches!(sysv.bases[0], ImageBase::Incoming { offset: 0 }));
        let aapcs = ThunkPlan::new(Target::Aarch64Linux, &triple_signature());
        assert!(matches!(aapcs.bases[0], ImageBase::SavedPointer { .. }));
    }

    #[test]
    fn an_indirect_native_pair_forwards_the_caller_bytes_and_its_own_storage() {
        for target in [Target::X86_64Linux, Target::Aarch64Linux] {
            let plan = ThunkPlan::new(target, &pair_signature());
            // Eight bytes of narrow fields: the native convention cannot express
            // it in registers, so both directions cross through memory.
            assert_eq!(plan.native_return, NativeReturn::Sret);
            assert_eq!(
                plan.slots,
                vec![
                    NativeSlotSource::ReturnStorage,
                    NativeSlotSource::ImageAddress { parameter: 0 },
                ],
                "the hidden return pointer is native ABI slot zero"
            );
            // The C side returns the eight bytes in one register, so the image
            // is assembled in the thunk's own frame.
            assert!(matches!(
                plan.return_image,
                Some(ReturnImage::Scratch { .. })
            ));
            // The argument arrived in one register, so its image is the saved
            // register cell.
            assert!(matches!(plan.bases[0], ImageBase::Frame { .. }));
        }
    }

    #[test]
    fn only_sysv_echoes_the_indirect_result_pointer() {
        let sysv = ThunkPlan::new(Target::X86_64Linux, &triple_signature());
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

        let aapcs = ThunkPlan::new(Target::Aarch64Linux, &triple_signature());
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
                let plan = ThunkPlan::new(target, &signature);
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
            &void_signature(vec![word_parameter()]),
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
            &void_signature(vec![word_parameter()]),
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
                parameters: vec![ExportParameter {
                    c: CAbiTypeFacts::Aggregate {
                        size: width.into(),
                        align: width.into(),
                    },
                    native: NativeParameter::Direct {
                        leaves: leaves.clone(),
                        reversed: true,
                    },
                }],
                result: CAbiTypeFacts::Aggregate {
                    size: width.into(),
                    align: width.into(),
                },
                return_facts: NativeAbiTypeFacts {
                    abi_slots: 1,
                    aggregate: true,
                    strbuf: false,
                    slot_identical: width == 8,
                },
                return_leaves: leaves,
                return_padding: vec![PaddingRange {
                    start: u64::from(width),
                    end: u64::from(width) + 1,
                }],
            };
            for target in [Target::X86_64Linux, Target::Aarch64Linux] {
                let code = generate_export_thunk(target, "native", &signature);
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
