//! The one signature classifier and the lowered signature it produces
//! (ADR-0064 P2/P3/P4, ADR-0084).
//!
//! [`lower_c_signature`] is the single function that answers *where every value
//! of a `"C"` signature lives*: which register bank and roster index, which
//! byte of the outgoing argument area, or which pointer. Every C crossing site
//! consumes its answer — the `extern "C"` import call planner, the
//! `pub extern "C" fn` export thunk, and, when it exists, the callback
//! trampoline — which is ADR-0064's ratified acceptance criterion that the
//! by-value classifier agree across all four sites. There is no second placement
//! algorithm in the tree.
//!
//! ## Two entry points, one placement walk
//!
//! The native Rue convention is the compilation target's C convention plus a
//! wider return bank (ADR-0084), so it places arguments by exactly these rules
//! and differs only in its result. That is the shape of the two entry points:
//! [`lower_c_signature`] takes a C row and classifies both halves of the
//! signature, while [`lower_native_signature`] takes a
//! [`ConventionSpec`] and an already-decided [`LoweredReturn`], because the
//! native return bank is wider than any [`CConventionSpec`] describes. Both run
//! the same walk over the same [`Placer`], so a native argument and a C
//! argument of the same shape land in the same place by construction.
//!
//! ## Why this crate is the home
//!
//! The psABI *rules* need no type facts, so they are data in `rue-target`
//! ([`CConventionSpec`]). The classifier does need type facts — a size, an
//! alignment, a scalar's width and signedness — and `rue-air` is where type
//! facts first exist. So the description lives one crate below and this module
//! reads it.
//!
//! ## Two planes, one kernel
//!
//! The input is [`CAbiTypeFacts`], never a [`Type`](crate::Type): the live
//! classifier projects the facts from the request-scoped `FrozenTypeInternPool`
//! and the stable query plane projects them from its revision-stable type keys
//! and canonical layout values, exactly as they already share
//! [`NativeAbiTypeFacts`](crate::NativeAbiTypeFacts) for the native convention.
//! Both then call this one kernel, so the two planes cannot disagree about a
//! placement.
//!
//! ## Floats
//!
//! The C boundary still rejects `f32`/`f64` (`c_passable_by_value`), so
//! [`CRegisterClass::Fp`] and [`EightbyteClass::Sse`] are unreachable today.
//! They exist because the float slice is then an extension of this model —
//! project an `Fp` scalar, classify an eightbyte `Sse` — rather than a rewrite
//! of it.

use rue_target::{
    AggregateClassificationRule, CConventionSpec, CRegisterClass, CallingConvention,
    ConventionSpec, SretRegisterKind, StackedArgumentPacking,
};

use crate::call_abi::{ArgConvention, CAbiScalarKind, ScalarAbiExtension};

/// Target-independent facts about one value type at a C boundary: the
/// projection each plane makes before this module classifies it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CAbiTypeFacts {
    /// A zero-sized value. It occupies no register, no stack byte, and no
    /// pointer, in either direction.
    ZeroSized,
    /// A scalar or pointer: one register of `class`, with `kind` naming the
    /// width and signedness the extension table is keyed by.
    Scalar {
        /// The scalar's width-and-signedness class.
        kind: CAbiScalarKind,
        /// The register bank the scalar travels in. `Gp` for every scalar the
        /// C boundary currently admits.
        class: CRegisterClass,
    },
    /// A C-classifiable aggregate (`@repr(c)` struct or array) of `size` bytes
    /// at `align` alignment.
    Aggregate {
        /// The aggregate's `@size_of`.
        size: u64,
        /// The aggregate's `@align_of`.
        align: u64,
    },
}

impl CAbiTypeFacts {
    /// The facts for one by-reference (`inout` / `borrow`) argument: a single
    /// register-width pointer, whatever it points at.
    pub const fn by_reference_pointer() -> Self {
        Self::Scalar {
            kind: CAbiScalarKind::RegisterWidth,
            class: CRegisterClass::Gp,
        }
    }

    /// The facts an argument presents once its source-level mode is applied: a
    /// by-reference argument is one pointer regardless of its pointee.
    pub const fn as_argument(self, convention: ArgConvention) -> Self {
        match convention {
            ArgConvention::ByReference => Self::by_reference_pointer(),
            ArgConvention::ByValue => self,
        }
    }

    /// The extension this value carries at the boundary: a scalar's canonical
    /// 64-bit extension, and none for an aggregate or a zero-sized value.
    pub const fn extension(self) -> ScalarAbiExtension {
        match self {
            Self::Scalar { kind, .. } => kind.extension(),
            Self::ZeroSized | Self::Aggregate { .. } => ScalarAbiExtension::None,
        }
    }
}

/// The register class of one eightbyte of an aggregate.
///
/// SysV AMD64 section 3.2.3 classifies each eightbyte of an aggregate and
/// assigns it a register of the matching bank; AAPCS64's composite rule reaches
/// the same answer for the integer-only surface. Every eightbyte of every
/// currently admissible type is [`Integer`](Self::Integer): a field that would
/// classify [`Sse`](Self::Sse) is a float, which the boundary rejects.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EightbyteClass {
    /// The eightbyte travels in a general-purpose integer register.
    Integer,
    /// The eightbyte travels in a floating-point/SIMD register.
    Sse,
}

impl EightbyteClass {
    /// The register bank this eightbyte class travels in.
    pub const fn register_class(self) -> CRegisterClass {
        match self {
            Self::Integer => CRegisterClass::Gp,
            Self::Sse => CRegisterClass::Fp,
        }
    }
}

/// The classes of a register-passed aggregate's eightbytes, in ascending memory
/// order. A register-passed aggregate spans at most
/// [`CConventionSpec::max_aggregate_register_bytes`] (16) bytes, so at most two.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EightbyteClasses {
    classes: [EightbyteClass; 2],
    len: u32,
}

impl EightbyteClasses {
    /// The classification of an aggregate whose every eightbyte is INTEGER —
    /// the only case the integer-only C surface reaches.
    pub const fn all_integer(count: u32) -> Self {
        assert!(
            count >= 1 && count <= 2,
            "a register-passed aggregate spans one or two eightbytes"
        );
        Self {
            classes: [EightbyteClass::Integer; 2],
            len: count,
        }
    }

    /// How many eightbytes the aggregate spans.
    pub const fn len(&self) -> u32 {
        self.len
    }

    /// Whether the aggregate spans no eightbyte. Never true for a classified
    /// aggregate; present so `len` reads as a length.
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The classes in ascending memory order.
    pub fn as_slice(&self) -> &[EightbyteClass] {
        &self.classes[..self.len as usize]
    }

    /// The single register bank every eightbyte travels in, or `None` when the
    /// aggregate is split across banks (unreachable until floats cross).
    pub fn uniform_register_class(&self) -> Option<CRegisterClass> {
        let first = self.as_slice().first()?.register_class();
        self.as_slice()
            .iter()
            .all(|class| class.register_class() == first)
            .then_some(first)
    }
}

/// Where the pointer of an indirectly-passed argument lives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PointerLocation {
    /// In general-purpose argument register `index`.
    Register {
        /// Roster index within the general-purpose argument registers.
        index: u32,
    },
    /// At `offset` bytes into the outgoing argument area, because the integer
    /// argument registers were exhausted.
    Stack {
        /// Byte offset from the base of the outgoing argument area.
        offset: u32,
    },
}

/// Where one argument of a lowered C signature lives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArgLocation {
    /// A zero-sized value: no register, no stack byte, no pointer.
    Omitted,
    /// `count` consecutive registers of `class`, starting at roster index
    /// `first_index`. A scalar is one register; an aggregate is one register per
    /// eightbyte in ascending memory order, i.e. C field order.
    Registers {
        /// The register bank.
        class: CRegisterClass,
        /// Roster index of the first register.
        first_index: u32,
        /// How many consecutive registers the value occupies.
        count: u32,
    },
    /// By value in the outgoing argument area: `size` bytes at `offset`, whose
    /// alignment the placement already satisfied.
    Stack {
        /// Byte offset from the base of the outgoing argument area.
        offset: u32,
        /// Footprint in bytes, per the convention's stacked-argument packing.
        size: u32,
        /// Alignment the offset satisfies.
        align: u32,
    },
    /// By pointer to a caller-owned copy of `size` bytes at `align` alignment
    /// (AAPCS64 section 6.8.2 B.4 / C.12).
    Indirect {
        /// Where the pointer itself travels.
        pointer: PointerLocation,
        /// The copy's byte size.
        size: u32,
        /// The copy's alignment.
        align: u32,
    },
}

/// One lowered argument: where it lives and what extension its value carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LoweredArgument {
    /// Where the argument lives at the boundary.
    pub location: ArgLocation,
    /// The narrow-integer extension the value carries. Rue's canonical 64-bit
    /// form already satisfies it on the way out; an export thunk applies it to
    /// an incoming register, whose high bits no C caller is required to define.
    pub extension: ScalarAbiExtension,
}

/// How a lowered C signature's result crosses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoweredReturn {
    /// No value crosses back.
    Void,
    /// `count` result registers of `class`, low eightbyte first (C field order
    /// for an aggregate). `extension` is the operation that restores Rue's
    /// canonical 64-bit form to a narrow scalar whose high bits the callee left
    /// unspecified; it is [`ScalarAbiExtension::None`] for an aggregate.
    Registers {
        /// The register bank.
        class: CRegisterClass,
        /// How many consecutive result registers the value occupies.
        count: u32,
        /// The extension a returning scalar needs.
        extension: ScalarAbiExtension,
    },
    /// Through caller-provided storage whose address crosses as a hidden
    /// argument (sret).
    Sret {
        /// Where the hidden pointer travels.
        register: SretRegisterKind,
        /// Whether the callee echoes the pointer in the primary result
        /// register.
        echoed: bool,
        /// Byte size of the caller storage.
        size: u32,
        /// Alignment of the caller storage.
        align: u32,
    },
}

impl LoweredReturn {
    /// Whether the result crosses through caller storage.
    pub const fn uses_sret(self) -> bool {
        matches!(self, Self::Sret { .. })
    }
}

/// A complete C signature, lowered to placements.
///
/// Produced by [`lower_c_signature`] and consumed identically by the import
/// call planner (caller direction: write these places, then call) and the export
/// thunk (callee direction: read these places, then adapt to the native body).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoweredSignature {
    convention: ConventionSpec,
    arguments: Vec<LoweredArgument>,
    ret: LoweredReturn,
    stack_bytes: u32,
}

impl LoweredSignature {
    /// The convention that produced this lowering.
    pub fn convention(&self) -> CallingConvention {
        self.convention.convention()
    }

    /// The description this lowering placed by. For a C row it is that row's
    /// own psABI entry; for the native convention it is the target's C entry
    /// with the native amendments (`ConventionSpec::native`).
    pub fn spec(&self) -> CConventionSpec {
        self.convention.spec()
    }

    /// The convention and description this lowering placed by, as one value.
    pub fn convention_spec(&self) -> ConventionSpec {
        self.convention
    }

    /// Every argument's placement, in source order.
    pub fn arguments(&self) -> &[LoweredArgument] {
        &self.arguments
    }

    /// The result's placement.
    pub fn ret(&self) -> LoweredReturn {
        self.ret
    }

    /// Bytes the caller reserves for the outgoing argument area, already
    /// rounded to the convention's call-boundary alignment. Includes the
    /// convention's shadow space.
    pub fn stack_bytes(&self) -> u32 {
        self.stack_bytes
    }

    /// Whether the hidden indirect-result pointer consumes the first ordinary
    /// integer argument register (SysV) rather than a dedicated one (AAPCS64).
    /// False when the result does not use sret at all.
    pub fn sret_in_argument_register(&self) -> bool {
        matches!(
            self.ret,
            LoweredReturn::Sret {
                register: SretRegisterKind::ArgumentRegister,
                ..
            }
        )
    }

    /// Total bytes of caller-owned copies the convention's by-reference
    /// aggregate rule requires, each already rounded to the call-boundary
    /// allocation granule by the caller that reserves it.
    pub fn indirect_copy_sizes(&self) -> impl Iterator<Item = (u32, u32)> + '_ {
        self.arguments
            .iter()
            .filter_map(|argument| match argument.location {
                ArgLocation::Indirect { size, align, .. } => Some((size, align)),
                _ => None,
            })
    }
}

/// Round `value` up to a multiple of the power-of-two `align`, saturating
/// rather than wrapping.
///
/// The mask form is exact only for a power of two, and every alignment reaching
/// here is one: [`Placer::stack_extent`] produces 1, 2, 4, or 8, and the final
/// rounding uses the convention's `call_stack_alignment`, which is 16 on every
/// row.
fn align_up(value: u64, align: u64) -> u64 {
    value.saturating_add(align - 1) & !(align - 1)
}

/// Saturating byte-count projection for the `u32` fields of the placements.
/// Semantic analysis rejects layouts anywhere near this bound before they reach
/// a boundary; saturating keeps the classifier total instead of panicking on a
/// value no program can construct.
fn saturate_u32(value: u64) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

/// Number of eightbytes a `size`-byte value occupies.
fn eightbytes(size: u64) -> u32 {
    saturate_u32(size.div_ceil(8))
}

/// Running state of the placement walk: the register rosters consumed so far
/// and the outgoing argument area's cursor.
struct Placer {
    spec: CConventionSpec,
    gp: u32,
    fp: u32,
    stack: u64,
}

impl Placer {
    fn new(spec: CConventionSpec) -> Self {
        Self {
            spec,
            gp: 0,
            fp: 0,
            // The callee's shadow space, when a row has one, sits at the base of
            // the outgoing area ahead of the first stacked argument.
            stack: u64::from(spec.shadow_space_bytes),
        }
    }

    fn used(&self, class: CRegisterClass) -> u32 {
        match class {
            CRegisterClass::Gp => self.gp,
            CRegisterClass::Fp => self.fp,
        }
    }

    fn consume(&mut self, class: CRegisterClass, count: u32) {
        match class {
            CRegisterClass::Gp => self.gp = self.gp.saturating_add(count),
            CRegisterClass::Fp => self.fp = self.fp.saturating_add(count),
        }
    }

    /// Mark every remaining register of `class` used, so no later argument
    /// takes one (AAPCS64 rule C.11 after a composite fails to fit).
    fn exhaust(&mut self, class: CRegisterClass) {
        let all = self.spec.argument_registers(class);
        match class {
            CRegisterClass::Gp => self.gp = all,
            CRegisterClass::Fp => self.fp = all,
        }
    }

    /// Claim `count` consecutive registers of `class`, or `None` when the
    /// roster cannot hold the whole value. Register assignment is
    /// all-or-nothing: a two-eightbyte aggregate with one register left goes to
    /// memory entire, it does not split.
    fn claim_registers(&mut self, class: CRegisterClass, count: u32) -> Option<u32> {
        let first = self.used(class);
        if first.saturating_add(count) > self.spec.argument_registers(class) {
            return None;
        }
        self.consume(class, count);
        Some(first)
    }

    /// Claim `size` bytes at `align` alignment in the outgoing argument area.
    fn claim_stack(&mut self, size: u64, align: u64) -> (u32, u32, u32) {
        let offset = align_up(self.stack, align);
        self.stack = offset.saturating_add(size);
        (
            saturate_u32(offset),
            saturate_u32(size),
            saturate_u32(align),
        )
    }

    /// The footprint a value of `natural_size` bytes claims in the outgoing
    /// argument area.
    ///
    /// Under [`StackedArgumentPacking::EightByteSlots`] every stacked argument
    /// starts at the next 8-byte boundary and occupies whole eightbytes. Under
    /// Apple's [`NaturalSize`](StackedArgumentPacking::NaturalSize) amendment a
    /// stacked *scalar* occupies exactly its C width at its own alignment, so a
    /// stacked `i8` takes one byte and an `i16` starts at the next even offset.
    /// A composite keeps whole eightbytes at 8-byte alignment under every row:
    /// it crosses through its eightbyte image, and a byte-exact copy needs
    /// marshaling this path does not have. That one open Apple amendment is
    /// recorded in `docs/notes/ffi-abi-conformance-audit.md`.
    fn stack_extent(&self, natural_size: u64, scalar: bool) -> (u64, u64) {
        match self.spec.stacked_argument_packing {
            StackedArgumentPacking::NaturalSize if scalar => {
                let bytes = natural_size.clamp(1, 8);
                (bytes, bytes)
            }
            StackedArgumentPacking::EightByteSlots | StackedArgumentPacking::NaturalSize => {
                (u64::from(eightbytes(natural_size)).saturating_mul(8), 8)
            }
        }
    }
}

/// Classify one whole `"C"` signature into placements.
///
/// `parameters` are the source parameters in declaration order, each already
/// reduced to the facts its plane projected and the mode it is passed under;
/// `result` is the return type's facts. `convention` must be a C row: the
/// pairing [`ConventionSpec::c`] is what asserts it, because a C boundary is
/// exactly a convention that describes itself.
pub fn lower_c_signature(
    convention: CallingConvention,
    parameters: &[(CAbiTypeFacts, ArgConvention)],
    result: CAbiTypeFacts,
) -> LoweredSignature {
    let pairing = ConventionSpec::c(convention);
    let ret = lower_return(&pairing.spec(), result);
    place_signature(pairing, parameters, ret)
}

/// Classify one whole *native* signature's arguments into placements, against
/// an already-decided return.
///
/// Arguments are placed by exactly the rules `pairing` describes, which for
/// [`ConventionSpec::native`] are the compilation target's own C rules
/// (ADR-0084). The return is an input rather than a decision because the native
/// return bank is wider than any C row's and no [`CConventionSpec`] describes
/// it: the caller classifies the return and hands it in, and this function
/// honors the one consequence a return has for argument placement — a native
/// sret pointer is the hidden first ordinary argument, so it shifts every user
/// argument one general-purpose register right.
pub fn lower_native_signature(
    pairing: ConventionSpec,
    parameters: &[(CAbiTypeFacts, ArgConvention)],
    result: LoweredReturn,
) -> LoweredSignature {
    place_signature(pairing, parameters, result)
}

/// Place every argument of a signature whose result crosses as `ret`, under
/// `pairing`.
///
/// The one placement walk both entry points run: the sret shift, the argument
/// classification in declaration order, and the outgoing argument area's final
/// size. What differs between a C boundary and a native one is only where `ret`
/// came from.
fn place_signature(
    pairing: ConventionSpec,
    parameters: &[(CAbiTypeFacts, ArgConvention)],
    ret: LoweredReturn,
) -> LoweredSignature {
    let spec = pairing.spec();
    let mut placer = Placer::new(spec);
    if ret.uses_sret() && spec.sret_pointer_in_argument_register() {
        // The hidden pointer is the hidden *first* argument, so it shifts every
        // user argument one general-purpose register right.
        placer.consume(CRegisterClass::Gp, 1);
    }

    let arguments = parameters
        .iter()
        .map(|(facts, mode)| {
            let facts = facts.as_argument(*mode);
            LoweredArgument {
                location: lower_argument(&mut placer, facts),
                extension: facts.extension(),
            }
        })
        .collect();

    LoweredSignature {
        convention: pairing,
        arguments,
        ret,
        stack_bytes: saturate_u32(align_up(placer.stack, u64::from(spec.call_stack_alignment))),
    }
}

fn lower_argument(placer: &mut Placer, facts: CAbiTypeFacts) -> ArgLocation {
    match facts {
        CAbiTypeFacts::ZeroSized => ArgLocation::Omitted,
        CAbiTypeFacts::Scalar { kind, class } => {
            if let Some(first_index) = placer.claim_registers(class, 1) {
                return ArgLocation::Registers {
                    class,
                    first_index,
                    count: 1,
                };
            }
            let natural = u64::from(kind.extension().natural_bytes());
            let (size, align) = placer.stack_extent(natural, true);
            let (offset, size, align) = placer.claim_stack(size, align);
            ArgLocation::Stack {
                offset,
                size,
                align,
            }
        }
        CAbiTypeFacts::Aggregate { size, align } => lower_aggregate_argument(placer, size, align),
    }
}

fn lower_aggregate_argument(placer: &mut Placer, size: u64, align: u64) -> ArgLocation {
    if size == 0 {
        return ArgLocation::Omitted;
    }
    if size <= placer.spec.max_aggregate_register_bytes {
        let classes = EightbyteClasses::all_integer(eightbytes(size));
        let class = classes
            .uniform_register_class()
            .expect("an integer-only eightbyte classification names one bank");
        if let Some(first_index) = placer.claim_registers(class, classes.len()) {
            return ArgLocation::Registers {
                class,
                first_index,
                count: classes.len(),
            };
        }
        // Registers exhausted: the whole aggregate goes to memory, keeping its
        // eightbyte image contiguous. AAPCS64 rule C.11 additionally sets the
        // next general-purpose register number to 8, so every later argument
        // is stacked too even though a register may remain; SysV AMD64 has no
        // such rule and a later scalar still takes the register the aggregate
        // could not fit in.
        if placer.spec.aggregate_rule == AggregateClassificationRule::Aapcs64Composite {
            placer.exhaust(class);
        }
        let (stack_size, stack_align) = placer.stack_extent(size, false);
        let (offset, size, align) = placer.claim_stack(stack_size, stack_align);
        return ArgLocation::Stack {
            offset,
            size,
            align,
        };
    }

    match placer.spec.aggregate_rule {
        AggregateClassificationRule::SysVEightbyte => {
            // MEMORY class: by value in the outgoing argument area, consuming
            // no register.
            let (stack_size, stack_align) = placer.stack_extent(size, false);
            let (offset, stack_size, stack_align) = placer.claim_stack(stack_size, stack_align);
            let _ = align;
            ArgLocation::Stack {
                offset,
                size: stack_size,
                align: stack_align,
            }
        }
        AggregateClassificationRule::Aapcs64Composite => {
            // One pointer to a caller-owned copy; the pointer itself is an
            // ordinary register-width argument and spills like one.
            let pointer = match placer.claim_registers(CRegisterClass::Gp, 1) {
                Some(index) => PointerLocation::Register { index },
                None => {
                    let (slot_size, slot_align) = placer.stack_extent(8, true);
                    let (offset, _, _) = placer.claim_stack(slot_size, slot_align);
                    PointerLocation::Stack { offset }
                }
            };
            ArgLocation::Indirect {
                pointer,
                size: saturate_u32(size),
                align: saturate_u32(align),
            }
        }
    }
}

fn lower_return(spec: &CConventionSpec, result: CAbiTypeFacts) -> LoweredReturn {
    match result {
        CAbiTypeFacts::ZeroSized => LoweredReturn::Void,
        CAbiTypeFacts::Scalar { kind, class } => LoweredReturn::Registers {
            class,
            count: 1,
            extension: kind.extension(),
        },
        CAbiTypeFacts::Aggregate { size, align } => {
            if size == 0 {
                return LoweredReturn::Void;
            }
            if size <= spec.max_aggregate_register_bytes {
                let classes = EightbyteClasses::all_integer(eightbytes(size));
                return LoweredReturn::Registers {
                    class: classes
                        .uniform_register_class()
                        .expect("an integer-only eightbyte classification names one bank"),
                    count: classes.len(),
                    extension: ScalarAbiExtension::None,
                };
            }
            LoweredReturn::Sret {
                register: spec.sret_register,
                echoed: spec.sret_pointer_echoed_in_result_register,
                size: saturate_u32(size),
                align: saturate_u32(align),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rue_target::Target;

    const SYSV: CallingConvention = CallingConvention::X86_64SysV;
    const AAPCS: CallingConvention = CallingConvention::Aarch64Aapcs;
    const DARWIN: CallingConvention = CallingConvention::Aarch64AapcsDarwin;

    fn scalar(kind: CAbiScalarKind) -> (CAbiTypeFacts, ArgConvention) {
        (
            CAbiTypeFacts::Scalar {
                kind,
                class: CRegisterClass::Gp,
            },
            ArgConvention::ByValue,
        )
    }

    fn word() -> (CAbiTypeFacts, ArgConvention) {
        scalar(CAbiScalarKind::RegisterWidth)
    }

    fn aggregate(size: u64, align: u64) -> (CAbiTypeFacts, ArgConvention) {
        (
            CAbiTypeFacts::Aggregate { size, align },
            ArgConvention::ByValue,
        )
    }

    fn registers(first_index: u32, count: u32) -> ArgLocation {
        ArgLocation::Registers {
            class: CRegisterClass::Gp,
            first_index,
            count,
        }
    }

    #[test]
    fn scalars_fill_the_roster_then_spill_to_the_argument_area() {
        let params = vec![word(); 9];
        let sysv = lower_c_signature(SYSV, &params, CAbiTypeFacts::ZeroSized);
        for (index, argument) in sysv.arguments().iter().take(6).enumerate() {
            assert_eq!(argument.location, registers(index as u32, 1));
        }
        assert_eq!(
            sysv.arguments()[6].location,
            ArgLocation::Stack {
                offset: 0,
                size: 8,
                align: 8
            }
        );
        assert_eq!(
            sysv.arguments()[8].location,
            ArgLocation::Stack {
                offset: 16,
                size: 8,
                align: 8
            }
        );
        assert_eq!(sysv.stack_bytes(), 32);

        let aapcs = lower_c_signature(AAPCS, &params, CAbiTypeFacts::ZeroSized);
        assert_eq!(aapcs.arguments()[7].location, registers(7, 1));
        assert_eq!(
            aapcs.arguments()[8].location,
            ArgLocation::Stack {
                offset: 0,
                size: 8,
                align: 8
            }
        );
        assert_eq!(aapcs.stack_bytes(), 16);
    }

    #[test]
    fn the_apple_row_packs_stacked_scalars_at_natural_size() {
        let mut params = vec![word(); 8];
        params.extend([
            scalar(CAbiScalarKind::I8),
            scalar(CAbiScalarKind::I16),
            scalar(CAbiScalarKind::I32),
            word(),
        ]);
        let linux = lower_c_signature(AAPCS, &params, CAbiTypeFacts::ZeroSized);
        let darwin = lower_c_signature(DARWIN, &params, CAbiTypeFacts::ZeroSized);
        let offsets = |signature: &LoweredSignature| {
            signature.arguments()[8..]
                .iter()
                .map(|argument| match argument.location {
                    ArgLocation::Stack {
                        offset,
                        size,
                        align,
                    } => (offset, size, align),
                    other => panic!("expected a stacked argument, got {other:?}"),
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(
            offsets(&linux),
            vec![(0, 8, 8), (8, 8, 8), (16, 8, 8), (24, 8, 8)]
        );
        assert_eq!(
            offsets(&darwin),
            vec![(0, 1, 1), (2, 2, 2), (4, 4, 4), (8, 8, 8)]
        );
        assert_eq!(linux.stack_bytes(), 32);
        assert_eq!(darwin.stack_bytes(), 16);
    }

    #[test]
    fn aggregates_pack_into_registers_up_to_two_eightbytes() {
        for convention in [SYSV, AAPCS, DARWIN] {
            let signature = lower_c_signature(
                convention,
                &[aggregate(8, 4), aggregate(16, 8), aggregate(12, 4)],
                CAbiTypeFacts::ZeroSized,
            );
            assert_eq!(signature.arguments()[0].location, registers(0, 1));
            assert_eq!(signature.arguments()[1].location, registers(1, 2));
            // A non-multiple-of-8 size rounds up to whole eightbytes.
            assert_eq!(signature.arguments()[2].location, registers(3, 2));
            assert_eq!(signature.stack_bytes(), 0);
        }
    }

    #[test]
    fn an_oversized_aggregate_follows_its_conventions_rule() {
        let sysv = lower_c_signature(SYSV, &[aggregate(24, 8)], CAbiTypeFacts::ZeroSized);
        assert_eq!(
            sysv.arguments()[0].location,
            ArgLocation::Stack {
                offset: 0,
                size: 24,
                align: 8
            }
        );
        assert_eq!(sysv.stack_bytes(), 32);
        for convention in [AAPCS, DARWIN] {
            let aapcs =
                lower_c_signature(convention, &[aggregate(24, 8)], CAbiTypeFacts::ZeroSized);
            assert_eq!(
                aapcs.arguments()[0].location,
                ArgLocation::Indirect {
                    pointer: PointerLocation::Register { index: 0 },
                    size: 24,
                    align: 8
                }
            );
            assert_eq!(aapcs.stack_bytes(), 0);
        }
    }

    #[test]
    fn a_sysv_memory_argument_consumes_no_register() {
        // SysV MEMORY class is byval-on-stack and leaves the integer roster
        // untouched, so the scalar after it still lands in `rsi`.
        let signature = lower_c_signature(
            SYSV,
            &[word(), aggregate(24, 8), word()],
            CAbiTypeFacts::ZeroSized,
        );
        assert_eq!(signature.arguments()[0].location, registers(0, 1));
        assert_eq!(signature.arguments()[2].location, registers(1, 1));
    }

    #[test]
    fn register_assignment_of_an_aggregate_is_all_or_nothing() {
        // Five words leave one integer register; a two-eightbyte aggregate does
        // not split across the boundary, it goes to memory entire, and the word
        // after it still takes the last register.
        let signature = lower_c_signature(
            SYSV,
            &[
                word(),
                word(),
                word(),
                word(),
                word(),
                aggregate(16, 8),
                word(),
            ],
            CAbiTypeFacts::ZeroSized,
        );
        assert_eq!(
            signature.arguments()[5].location,
            ArgLocation::Stack {
                offset: 0,
                size: 16,
                align: 8
            }
        );
        assert_eq!(signature.arguments()[6].location, registers(5, 1));
    }

    #[test]
    fn an_aapcs64_composite_that_does_not_fit_exhausts_the_register_roster() {
        // Seven words leave `x7`; a two-eightbyte composite cannot fit, so it
        // goes to the stack entire and, per AAPCS64 rule C.11, the next
        // general-purpose register number becomes 8: the word after it is
        // stacked behind the composite rather than taking the free `x7`.
        for convention in [AAPCS, DARWIN] {
            let signature = lower_c_signature(
                convention,
                &[
                    word(),
                    word(),
                    word(),
                    word(),
                    word(),
                    word(),
                    word(),
                    aggregate(16, 8),
                    word(),
                ],
                CAbiTypeFacts::ZeroSized,
            );
            assert_eq!(
                signature.arguments()[7].location,
                ArgLocation::Stack {
                    offset: 0,
                    size: 16,
                    align: 8
                }
            );
            assert_eq!(
                signature.arguments()[8].location,
                ArgLocation::Stack {
                    offset: 16,
                    size: 8,
                    align: 8
                },
                "{convention}: the register the composite could not use stays unused"
            );
            assert_eq!(signature.stack_bytes(), 32);
        }
    }

    #[test]
    fn an_aapcs64_by_reference_pointer_spills_like_any_pointer() {
        let mut params = vec![word(); 8];
        params.push(aggregate(24, 8));
        let signature = lower_c_signature(AAPCS, &params, CAbiTypeFacts::ZeroSized);
        assert_eq!(
            signature.arguments()[8].location,
            ArgLocation::Indirect {
                pointer: PointerLocation::Stack { offset: 0 },
                size: 24,
                align: 8
            }
        );
        assert_eq!(signature.stack_bytes(), 16);
    }

    #[test]
    fn a_sysv_sret_pointer_shifts_user_arguments_and_aapcs64_does_not() {
        let big = CAbiTypeFacts::Aggregate { size: 24, align: 8 };
        let sysv = lower_c_signature(SYSV, &[word(), word()], big);
        assert_eq!(
            sysv.ret(),
            LoweredReturn::Sret {
                register: SretRegisterKind::ArgumentRegister,
                echoed: true,
                size: 24,
                align: 8
            }
        );
        assert!(sysv.sret_in_argument_register());
        assert_eq!(sysv.arguments()[0].location, registers(1, 1));
        assert_eq!(sysv.arguments()[1].location, registers(2, 1));

        let aapcs = lower_c_signature(AAPCS, &[word(), word()], big);
        assert_eq!(
            aapcs.ret(),
            LoweredReturn::Sret {
                register: SretRegisterKind::DedicatedRegister,
                echoed: false,
                size: 24,
                align: 8
            }
        );
        assert!(!aapcs.sret_in_argument_register());
        assert_eq!(aapcs.arguments()[0].location, registers(0, 1));
    }

    #[test]
    fn returns_follow_the_scalar_extension_and_eightbyte_tables() {
        let signature = lower_c_signature(
            SYSV,
            &[],
            CAbiTypeFacts::Scalar {
                kind: CAbiScalarKind::I16,
                class: CRegisterClass::Gp,
            },
        );
        assert_eq!(
            signature.ret(),
            LoweredReturn::Registers {
                class: CRegisterClass::Gp,
                count: 1,
                extension: ScalarAbiExtension::Signed { from_bits: 16 }
            }
        );
        for (size, count) in [(1_u64, 1_u32), (8, 1), (9, 2), (16, 2)] {
            let signature =
                lower_c_signature(SYSV, &[], CAbiTypeFacts::Aggregate { size, align: 8 });
            assert_eq!(
                signature.ret(),
                LoweredReturn::Registers {
                    class: CRegisterClass::Gp,
                    count,
                    extension: ScalarAbiExtension::None
                }
            );
        }
        assert_eq!(
            lower_c_signature(SYSV, &[], CAbiTypeFacts::ZeroSized).ret(),
            LoweredReturn::Void
        );
    }

    #[test]
    fn a_by_reference_argument_is_one_pointer_whatever_it_points_at() {
        let signature = lower_c_signature(
            SYSV,
            &[(
                CAbiTypeFacts::Aggregate {
                    size: 4096,
                    align: 8,
                },
                ArgConvention::ByReference,
            )],
            CAbiTypeFacts::ZeroSized,
        );
        assert_eq!(signature.arguments()[0].location, registers(0, 1));
        assert_eq!(signature.stack_bytes(), 0);
    }

    #[test]
    fn every_eightbyte_of_the_integer_only_surface_is_the_integer_class() {
        let classes = EightbyteClasses::all_integer(2);
        assert_eq!(classes.len(), 2);
        assert!(!classes.is_empty());
        assert_eq!(
            classes.as_slice(),
            &[EightbyteClass::Integer, EightbyteClass::Integer]
        );
        assert_eq!(
            classes.uniform_register_class(),
            Some(CRegisterClass::Gp),
            "an integer-only classification names the general-purpose bank"
        );
        assert_eq!(EightbyteClass::Sse.register_class(), CRegisterClass::Fp);
    }

    #[test]
    fn oversized_byte_counts_saturate_rather_than_wrapping() {
        // Semantic analysis rejects layouts anywhere near this bound before they
        // reach a boundary; the classifier stays total rather than panicking on
        // a value no program can construct.
        let huge = u64::from(u32::MAX) + 9;
        let signature = lower_c_signature(
            SYSV,
            &[aggregate(huge, 8)],
            CAbiTypeFacts::Aggregate {
                size: huge,
                align: 8,
            },
        );
        assert_eq!(
            signature.arguments()[0].location,
            ArgLocation::Stack {
                offset: 0,
                size: u32::MAX,
                align: 8
            }
        );
        assert_eq!(
            signature.ret(),
            LoweredReturn::Sret {
                register: SretRegisterKind::ArgumentRegister,
                echoed: true,
                size: u32::MAX,
                align: 8
            }
        );
        assert_eq!(signature.stack_bytes(), u32::MAX);
    }

    #[test]
    #[should_panic(expected = "a C pairing names a psABI row")]
    fn the_native_convention_is_not_a_c_boundary() {
        let _ = lower_c_signature(CallingConvention::Rue, &[], CAbiTypeFacts::ZeroSized);
    }

    #[test]
    fn a_void_native_signature_places_arguments_exactly_as_its_targets_c_row() {
        // ADR-0084's argument rule, stated as an equality: with nothing forced
        // by the return, every native placement is the target C placement.
        let params = vec![
            word(),
            aggregate(16, 8),
            aggregate(24, 8),
            word(),
            word(),
            word(),
            word(),
            word(),
            word(),
            scalar(CAbiScalarKind::I8),
        ];
        for target in Target::all() {
            let c = lower_c_signature(
                target.c_calling_convention(),
                &params,
                CAbiTypeFacts::ZeroSized,
            );
            let native = lower_native_signature(
                ConventionSpec::native(*target),
                &params,
                LoweredReturn::Void,
            );
            assert_eq!(
                native.arguments(),
                c.arguments(),
                "{target:?}: the native convention places arguments by its C row"
            );
            assert_eq!(native.stack_bytes(), c.stack_bytes());
            assert_eq!(native.convention(), CallingConvention::Rue);
            assert_eq!(native.ret(), LoweredReturn::Void);
            assert_eq!(
                native.spec().gp_argument_registers,
                c.spec().gp_argument_registers
            );
        }
    }

    #[test]
    fn a_native_sret_shifts_user_arguments_on_every_target() {
        // The native hidden indirect-result pointer is an ordinary first
        // argument on both architectures, so it costs the first
        // general-purpose argument register everywhere — unlike AAPCS64's
        // dedicated `x8`, which leaves user arguments at roster index 0.
        let sret = LoweredReturn::Sret {
            register: SretRegisterKind::ArgumentRegister,
            echoed: false,
            size: 64,
            align: 8,
        };
        for target in Target::all() {
            let native =
                lower_native_signature(ConventionSpec::native(*target), &[word(), word()], sret);
            assert!(native.sret_in_argument_register());
            assert_eq!(native.arguments()[0].location, registers(1, 1));
            assert_eq!(native.arguments()[1].location, registers(2, 1));
        }
        // The same shape at the AAPCS64 C boundary keeps `x0` for the user's
        // first argument, which is the difference the pairing carries.
        let c = lower_c_signature(
            CallingConvention::Aarch64Aapcs,
            &[word(), word()],
            CAbiTypeFacts::Aggregate { size: 64, align: 8 },
        );
        assert!(!c.sret_in_argument_register());
        assert_eq!(c.arguments()[0].location, registers(0, 1));
    }

    #[test]
    fn a_native_signature_fills_its_targets_roster_before_the_argument_area() {
        // Nine words: SysV stacks the last three, AAPCS64 the last one, and the
        // Apple row packs a narrow stacked scalar at its natural size just as
        // it does at its C boundary.
        let params = vec![word(); 9];
        let native = |target: Target| {
            lower_native_signature(ConventionSpec::native(target), &params, LoweredReturn::Void)
        };
        let sysv = native(Target::X86_64Linux);
        assert_eq!(sysv.arguments()[5].location, registers(5, 1));
        assert_eq!(
            sysv.arguments()[6].location,
            ArgLocation::Stack {
                offset: 0,
                size: 8,
                align: 8
            }
        );
        assert_eq!(sysv.stack_bytes(), 32);
        let aapcs = native(Target::Aarch64Linux);
        assert_eq!(aapcs.arguments()[7].location, registers(7, 1));
        assert_eq!(
            aapcs.arguments()[8].location,
            ArgLocation::Stack {
                offset: 0,
                size: 8,
                align: 8
            }
        );

        let mut narrow = vec![word(); 8];
        narrow.push(scalar(CAbiScalarKind::I8));
        let darwin = lower_native_signature(
            ConventionSpec::native(Target::Aarch64Macos),
            &narrow,
            LoweredReturn::Void,
        );
        assert_eq!(
            darwin.arguments()[8].location,
            ArgLocation::Stack {
                offset: 0,
                size: 1,
                align: 1
            }
        );
    }

    #[test]
    fn a_native_register_return_costs_no_argument_register() {
        // Only an sret return shifts arguments; a return that fits the native
        // bank leaves the whole roster to the user's arguments.
        for target in Target::all() {
            let native = lower_native_signature(
                ConventionSpec::native(*target),
                &[word()],
                LoweredReturn::Registers {
                    class: CRegisterClass::Gp,
                    count: 6,
                    extension: ScalarAbiExtension::None,
                },
            );
            assert_eq!(native.arguments()[0].location, registers(0, 1));
        }
    }
}
