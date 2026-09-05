//! The native Rue convention's argument description (ADR-0084).
//!
//! The native convention places every argument exactly where the compilation
//! target's C row places it, so *where* a value goes is
//! [`rue_air::lower_native_signature`]'s answer against the
//! [`CAbiTypeFacts`] this module projects. What this module adds is the other
//! half of the crossing: *how* a value reaches that placement, which the C
//! boundary answers with its own [`crate::foreign_call::AggregateImage`] and
//! the native side answers here, because a native aggregate may be an enum and
//! therefore may need a tag-dispatched image.
//!
//! ## Leaves, eightbytes, and the two marshaling shapes
//!
//! A native value is held as one canonically 64-bit-extended vreg per scalar
//! leaf. A placement is expressed in *eightbytes*. The two coincide exactly
//! when every leaf starts its own eightbyte — `{i64, i64}`, `StrBuf`, a
//! one-field struct, a scalar — and then the leaf vregs are the eightbytes and
//! nothing is marshaled ([`NativeArgMarshal::Direct`]). Otherwise the value's
//! leaves are written into a stack image at their compact byte offsets and the
//! eightbytes are read back out of it ([`NativeArgMarshal::Image`]), which is
//! what packs `{u8, u8, u8, u8}` into one register.
//!
//! The callee undoes whichever shape the caller used, so the predicate that
//! chooses between them ([`NativeArg::marshal`]) is consulted from both ends
//! and has exactly one home.

use rue_air::{
    AggregateLeaves, ArgConvention, ArgLocation, CAbiScalarKind, CAbiTypeFacts,
    FrozenTypeInternPool, Type, aggregate_leaves,
};
use rue_target::CRegisterClass;

use crate::call_plan::AbiSlotClass;
use crate::frame_layout::checked_aligned_region_bytes;
use crate::types::{self, DispatchImage, PhysicalEnumSlot};
use crate::value_plan::FloatWidth;

/// The compact memory image of one native by-value aggregate: the bytes the
/// convention classifies and the marshaling both ends run against them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeImage {
    /// How the image's bytes are written and read.
    pub kind: NativeImageKind,
    /// The aggregate's `@size_of`.
    pub size: u32,
    /// The aggregate's `@align_of`.
    pub align: u32,
    /// The aggregate's internal value-decomposition slot count.
    pub slot_count: u32,
    /// Backing-buffer size: `size` rounded to the 16-byte call-stack granule,
    /// so whole-eightbyte loads and stores never run past the buffer.
    pub storage_bytes: u32,
    /// The scalar leaves the convention classifies the eightbytes by.
    pub leaves: AggregateLeaves,
}

/// How one aggregate's compact image is written and read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NativeImageKind {
    /// A single variant-independent internal-slot → physical-byte map.
    Map {
        /// One entry per internal slot, in decomposition order.
        map: Vec<PhysicalEnumSlot>,
        /// Padding byte ranges zeroed before the leaf stores, so the buffer is
        /// deterministically initialized (ADR-0052 ruling 5).
        padding: Vec<rue_air::layout::PaddingRange>,
    },
    /// A per-variant tag dispatch, for an aggregate whose variants place their
    /// payloads differently (RUE-1037).
    Dispatch(DispatchImage),
}

impl NativeImage {
    /// How many eightbytes the image spans.
    pub fn eightbytes(&self) -> u32 {
        self.size.div_ceil(8)
    }

    /// The classification facts this image presents.
    pub fn facts(&self) -> CAbiTypeFacts {
        CAbiTypeFacts::Aggregate {
            size: u64::from(self.size),
            align: u64::from(self.align),
            leaves: self.leaves,
        }
    }

    /// The bank and width each leaf's own vreg travels in when the leaves *are*
    /// the eightbytes, or `None` when the image must be marshaled.
    ///
    /// The leaves are the eightbytes exactly when leaf *i* starts at byte
    /// `8 * i`: there is then one leaf per eightbyte, in ascending memory
    /// order, and the leaf's canonically extended vreg already holds every byte
    /// the eightbyte's classification can observe. (Bytes above a narrow leaf
    /// are padding, which no reader of the image looks at.) A tag-dispatched
    /// image is never direct: its leaves move with the active variant.
    pub fn direct_leaves(&self) -> Option<Vec<AbiSlotClass>> {
        let NativeImageKind::Map { map, .. } = &self.kind else {
            return None;
        };
        let direct = map
            .iter()
            .enumerate()
            .all(|(index, leaf)| leaf.byte_offset == i32::try_from(index * 8).unwrap_or(i32::MAX));
        if !direct {
            return None;
        }
        Some(map.iter().map(leaf_slot_class).collect())
    }
}

/// The bank one image leaf's own vreg belongs to: an enum's union payload is a
/// general-purpose bit carrier whatever its variants hold, a float leaf rides
/// in the floating-point file at its own width, everything else is
/// general-purpose.
fn leaf_slot_class(leaf: &PhysicalEnumSlot) -> AbiSlotClass {
    match leaf.float_width {
        Some(width) if !leaf.bit_carrier => AbiSlotClass::Fp(width),
        _ => AbiSlotClass::Gp,
    }
}

/// What one native argument is, and how it reaches the placement the lowered
/// signature gives it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NativeArg {
    /// A zero-sized by-value argument: no register, no stack byte, no pointer.
    Omitted,
    /// A scalar or a by-reference pointer: one vreg, one register.
    Scalar {
        /// Its width-and-signedness class, which fixes the footprint a stacked
        /// copy takes under Apple's natural-size packing.
        kind: CAbiScalarKind,
        /// The bank and move width its vreg uses.
        class: AbiSlotClass,
    },
    /// An aggregate, classified and marshaled through its compact image.
    Aggregate {
        /// The compact image.
        image: NativeImage,
    },
    /// An already-flattened value crossing one register-width leaf at a time —
    /// the cleanup convention destructors and drop glue are invoked under.
    ///
    /// A destructor and a drop glue body receive an aggregate's leaves
    /// directly, in ascending order, because the caller has already
    /// materialized them and the callee walks them as leaves rather than
    /// reconstructing a value. Placement is still the convention's: each leaf
    /// is one register-width value, so the leaves take consecutive argument
    /// registers and then the argument area's own packing, exactly as that many
    /// separate `i64` arguments would. Tracked for retirement with RUE-2039,
    /// which collapses the remaining convention branches.
    PerLeaf {
        /// How many leaves cross.
        count: u32,
    },
}

impl NativeArg {
    /// The classification facts this argument presents, one entry per value
    /// the convention places.
    ///
    /// Every shape but the cleanup convention's presents exactly one: a value
    /// is placed as a whole. [`PerLeaf`](Self::PerLeaf) presents one
    /// register-width entry per leaf, which is what makes its leaves take the
    /// registers and stack slots that many separate scalars would.
    pub fn facts(&self) -> Vec<CAbiTypeFacts> {
        match self {
            Self::Omitted => vec![CAbiTypeFacts::ZeroSized],
            Self::Scalar { kind, .. } => vec![CAbiTypeFacts::Scalar {
                kind: *kind,
                class: kind.register_class(),
            }],
            Self::Aggregate { image } => vec![image.facts()],
            Self::PerLeaf { count } => {
                vec![
                    CAbiTypeFacts::Scalar {
                        kind: CAbiScalarKind::RegisterWidth,
                        class: CRegisterClass::Gp,
                    };
                    *count as usize
                ]
            }
        }
    }

    /// The canonical 64-bit extension this value carries: a narrow scalar's
    /// sign or zero extension, and none for an aggregate or a zero-sized value.
    pub fn extension(&self) -> rue_air::ScalarAbiExtension {
        match self {
            Self::Scalar { kind, .. } => kind.extension(),
            Self::Omitted | Self::Aggregate { .. } | Self::PerLeaf { .. } => {
                rue_air::ScalarAbiExtension::None
            }
        }
    }

    /// Whether a stacked copy is packed at the scalar's own width (Apple's
    /// natural-size amendment) rather than as whole eightbytes.
    pub fn packs_as_scalar(&self) -> bool {
        matches!(self, Self::Scalar { .. })
    }

    /// The bank and move width of each eightbyte the value presents to
    /// `location`, and whether they are the value's own leaf vregs.
    ///
    /// A value's leaves are its eightbytes only if they also travel in the
    /// banks the placement named: an aggregate whose leaves start their own
    /// eightbytes still marshals through its image when the convention puts an
    /// eightbyte in a bank its leaf does not live in — AAPCS64 passes a
    /// composite in integer registers whatever its members are (section 6.8.2
    /// rules C.13 and C.14), and an enum's union payload is a general-purpose
    /// bit carrier even where every variant puts a float there.
    pub fn marshal(&self, location: ArgLocation) -> NativeArgMarshal {
        let marshal = self.eightbyte_classes();
        let (NativeArgMarshal::Direct { classes }, ArgLocation::Registers { pieces }) =
            (&marshal, location)
        else {
            return marshal;
        };
        let banks_agree = classes.len() == pieces.len() as usize
            && classes
                .iter()
                .zip(pieces.as_slice())
                .all(|(class, piece)| class.bank() == piece.class);
        if banks_agree {
            return marshal;
        }
        match self {
            Self::Aggregate { image } => NativeArgMarshal::Image {
                eightbytes: image.eightbytes(),
            },
            _ => marshal,
        }
    }

    /// The banks the value's own leaf vregs live in, before the placement is
    /// consulted.
    fn eightbyte_classes(&self) -> NativeArgMarshal {
        match self {
            Self::Omitted => NativeArgMarshal::Direct {
                classes: Vec::new(),
            },
            Self::Scalar { class, .. } => NativeArgMarshal::Direct {
                classes: vec![*class],
            },
            Self::PerLeaf { count } => NativeArgMarshal::Direct {
                classes: vec![AbiSlotClass::Gp; *count as usize],
            },
            Self::Aggregate { image } => match image.direct_leaves() {
                Some(classes) => NativeArgMarshal::Direct { classes },
                // A marshaled eightbyte is a 64-bit lane of the image, so an
                // SSE-classified one moves as a whole double even when the
                // leaves inside it are `f32`s.
                None => NativeArgMarshal::Image {
                    eightbytes: image.eightbytes(),
                },
            },
        }
    }
}

/// Whether an argument's eightbytes are its own leaf vregs or must be marshaled
/// through its compact image.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NativeArgMarshal {
    /// The leaf vregs are the eightbytes, with these banks and move widths.
    Direct {
        /// One class per leaf, in ascending memory order.
        classes: Vec<AbiSlotClass>,
    },
    /// The leaves are written to a stack image and this many eightbytes are
    /// read back from it.
    Image {
        /// How many eightbytes the image spans.
        eightbytes: u32,
    },
}

impl NativeArgMarshal {
    /// The bank and move width of eightbyte `index`.
    pub fn class(&self, index: usize, eightbyte_class: CRegisterClass) -> AbiSlotClass {
        match self {
            Self::Direct { classes } => classes[index],
            Self::Image { .. } => match eightbyte_class {
                CRegisterClass::Gp => AbiSlotClass::Gp,
                CRegisterClass::Fp => AbiSlotClass::Fp(FloatWidth::F64),
            },
        }
    }

    /// The bank a stacked eightbyte's store uses: the value's own leaf bank
    /// when the leaves are the eightbytes, and the general-purpose file for a
    /// marshaled image, whose eightbytes are already integer-shaped lanes read
    /// out of memory.
    pub fn stacked_bank(&self, index: usize) -> CRegisterClass {
        match self {
            Self::Direct { classes } => classes[index].bank(),
            Self::Image { .. } => CRegisterClass::Gp,
        }
    }

    /// How many eightbytes the value presents.
    pub fn eightbyte_count(&self) -> usize {
        match self {
            Self::Direct { classes } => classes.len(),
            Self::Image { eightbytes } => *eightbytes as usize,
        }
    }
}

/// Project the native description of a by-value argument or parameter of type
/// `ty`.
///
/// The aggregate predicate is [`crate::types::is_multislot_aggregate`], the same
/// one the value plane materializes by, so an argument's description and its
/// vregs cannot disagree about whether it has leaves.
pub fn native_by_value_arg(type_pool: &FrozenTypeInternPool, ty: Type) -> NativeArg {
    if types::is_multislot_aggregate(type_pool, ty) {
        return NativeArg::Aggregate {
            image: native_image(type_pool, ty),
        };
    }
    if type_pool.abi_slot_count(ty) == 0 {
        return NativeArg::Omitted;
    }
    let kind = native_scalar_kind(type_pool, ty);
    NativeArg::Scalar {
        kind,
        class: match kind {
            CAbiScalarKind::F32 => AbiSlotClass::Fp(FloatWidth::F32),
            CAbiScalarKind::F64 => AbiSlotClass::Fp(FloatWidth::F64),
            _ => AbiSlotClass::Gp,
        },
    }
}

/// The native description of one argument under its source-level mode: a
/// by-reference `inout` / `borrow` is one register-width pointer whatever it
/// points at.
pub fn native_arg(
    type_pool: &FrozenTypeInternPool,
    ty: Type,
    convention: ArgConvention,
) -> NativeArg {
    match convention {
        ArgConvention::ByReference => NativeArg::Scalar {
            kind: CAbiScalarKind::RegisterWidth,
            class: AbiSlotClass::Gp,
        },
        ArgConvention::ByValue => native_by_value_arg(type_pool, ty),
    }
}

/// The width-and-signedness class of a native scalar.
///
/// A discriminant-only enum is a scalar whose compact image is its tag, so it
/// presents that tag's own unsigned width; every other scalar presents its own
/// type's class.
fn native_scalar_kind(type_pool: &FrozenTypeInternPool, ty: Type) -> CAbiScalarKind {
    if let Some(kind) = CAbiScalarKind::for_live_type(ty) {
        return kind;
    }
    if ty.is_enum() {
        return match type_pool.layout(ty).size {
            0 | 1 => CAbiScalarKind::U8,
            2 => CAbiScalarKind::U16,
            3..=4 => CAbiScalarKind::U32,
            _ => CAbiScalarKind::RegisterWidth,
        };
    }
    panic!(
        "the native convention classifies every value type; {:?} is neither a \
         scalar nor an aggregate",
        ty.kind()
    )
}

/// The compact image of a native by-value aggregate.
///
/// Every aggregate that reaches a call boundary has one: the compact-access
/// scan (`crate::types::compact_physical_access_unsupported`) refuses a
/// crossing aggregate that has neither a variant-independent map nor a
/// tag-dispatched image before code generation reaches here.
pub fn native_image(type_pool: &FrozenTypeInternPool, ty: Type) -> NativeImage {
    let layout = type_pool.layout(ty);
    let size = u32::try_from(layout.size).expect("native aggregate size must fit u32");
    let kind = match types::aggregate_physical_slot_map(type_pool, ty) {
        Some(map) => NativeImageKind::Map {
            map,
            padding: type_pool.compact_image_padding_ranges(ty),
        },
        None => NativeImageKind::Dispatch(types::aggregate_dispatch_image(type_pool, ty).expect(
            "a by-value aggregate crossing a call has a variant-independent or \
             tag-dispatched memory image (guaranteed by the compact-access scan)",
        )),
    };
    NativeImage {
        kind,
        size,
        align: u32::try_from(layout.alignment).expect("native aggregate alignment must fit u32"),
        slot_count: type_pool.abi_slot_count(ty),
        storage_bytes: checked_aligned_region_bytes(layout.size)
            .expect("native aggregate storage must pass frame-budget preflight"),
        leaves: aggregate_leaves(type_pool, ty, layout.size),
    }
}

/// The simultaneous transient stack a native call needs, from the same
/// placement its lowering will use.
///
/// Four regions can be live at once: the caller's indirect-result storage, the
/// caller-owned copies of by-reference aggregate arguments, the scratch image
/// one aggregate is marshaled through (released before the next one is taken,
/// so only the largest counts), and the outgoing argument area.
pub fn native_call_area_bytes(
    type_pool: &FrozenTypeInternPool,
    pairing: rue_target::ConventionSpec,
    sret_storage_bytes: u64,
    arguments: &[(Type, ArgConvention)],
) -> Result<u32, crate::frame_layout::FrameBudgetExceeded> {
    let natives: Vec<NativeArg> = arguments
        .iter()
        .map(|(ty, convention)| native_arg(type_pool, *ty, *convention))
        .collect();
    let parameters: Vec<(CAbiTypeFacts, ArgConvention)> = natives
        .iter()
        .zip(arguments)
        .flat_map(|(native, (_, convention))| {
            native
                .facts()
                .into_iter()
                .map(move |facts| (facts, *convention))
        })
        .collect();
    let ret = if sret_storage_bytes == 0 {
        rue_air::LoweredReturn::Void
    } else {
        let spec = pairing.spec();
        rue_air::LoweredReturn::Sret {
            register: spec.sret_register,
            echoed: spec.sret_pointer_echoed_in_result_register,
            size: 8,
            align: 8,
        }
    };
    assert_eq!(
        parameters.len(),
        natives.len(),
        "an ordinary call places one value per argument; only the cleanup \
         convention's already-flattened leaves place more, and it reaches no \
         aggregate"
    );
    let signature = rue_air::lower_native_signature(pairing, &parameters, ret);
    let mut indirect = 0u64;
    let mut scratch = 0u64;
    for (native, argument) in natives.iter().zip(signature.arguments()) {
        let NativeArg::Aggregate { image } = native else {
            continue;
        };
        match argument.location {
            ArgLocation::Indirect { .. } => {
                indirect = indirect.saturating_add(u64::from(image.storage_bytes));
            }
            location if matches!(native.marshal(location), NativeArgMarshal::Image { .. }) => {
                scratch = scratch.max(u64::from(image.storage_bytes));
            }
            _ => {}
        }
    }
    crate::frame_layout::checked_call_area_from_stack_bytes(
        u64::from(signature.stack_bytes()),
        sret_storage_bytes,
        indirect.saturating_add(scratch),
    )
}
