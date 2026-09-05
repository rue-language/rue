//! Canonical native call-ABI classifier (ADR-0052 phase 5).
//!
//! One authority that answers a single question: *how does a value of a given
//! type cross a call boundary on this target* — returned in registers, returned
//! indirectly through caller storage (sret), passed by value across argument
//! slots, or passed as one by-reference pointer. ADR-0052's third
//! representation ("call ABI classification") is deliberately separate from the
//! physical slot *count*: today the two coincide (an aggregate return uses sret
//! exactly when its flattened slot count exceeds the return-register budget, and
//! an argument occupies one slot per leaf), and this authority reproduces that
//! decision byte-for-byte. Its value is that both code-generation backends, the
//! sret/return-budget decision sites, and the oracle's model of the call
//! contract consult *one* place instead of each rediscovering
//! `slot_count > budget`.
//!
//! Which convention governs a boundary is named by exactly one value type,
//! [`rue_target::CallingConvention`], whose rows are the native Rue convention
//! and the concrete platform psABIs. This module answers the *native* decision
//! tree ([`NativeAbiTypeFacts`]) and the per-convention scalar facts a C
//! crossing needs ([`TargetCCallAbi`], [`CAbiScalarKind`]). Where a value
//! crosses a C boundary is answered once by
//! [`lower_c_signature`](crate::lower_c_signature), which reads the convention
//! description in `rue-target` against the facts [`c_abi_type_facts`] projects.
//!
//! ## Two planes, one policy kernel
//!
//! Two walkers consume this module because their lifetimes differ: the live
//! classifiers here walk the request-scoped [`FrozenTypeInternPool`], while the
//! stable query plane (`compiler.call-abi` in `rue-compiler`) walks its own
//! revision-stable type keys and canonical layout values and must not hold a
//! live pool. Both project per-type facts and then consult the same pure
//! kernel — [`NativeAbiTypeFacts`] for the native decision tree,
//! [`CAbiTypeFacts`] plus [`CAbiScalarKind`] for the target-C placement and
//! extensions, [`native_return_register_budget`] and
//! [`rue_target::CallingConvention::c_for_target`] for the per-target numbers —
//! so the classification policy itself has exactly one production home.
//!
//! ## Memory-first transitional rule (ADR-0052 ratified ruling 9)
//!
//! The preserved slot call convention stays in force unchanged while memory
//! layout migrates. Under the compact layout preview an aggregate whose compact
//! representation is *not* slot-identical cannot be expressed by the preserved
//! register convention, so this classifier rules it **indirect** (by-reference /
//! sret). That composes with RUE-974's loud refusal: a compact aggregate
//! crossing a call is either expressible slot-identically (classified and
//! marshaled exactly as today), passed indirectly (this rule), or refused
//! loudly by code generation because the narrow indirect marshaling is not yet
//! implemented — never silently wrong. Slot-identical aggregates keep the
//! historical register classification byte-for-byte.

use crate::lowered_signature::CAbiTypeFacts;
use crate::{FrozenTypeInternPool, Type, TypeKind};
use rue_target::{
    Arch, CConventionSpec, CRegisterClass, CallingConvention, StackedArgumentPacking,
};

/// The native return-register budget for a target architecture: how many
/// flattened eight-byte slots an aggregate return may occupy before it must
/// cross through caller storage (sret). This is the single policy home of the
/// per-target number; each backend's physical return-register roster is pinned
/// against it by that backend's tests, and the stable query plane consults it
/// directly instead of restating the numbers.
pub const fn native_return_register_budget(arch: Arch) -> u32 {
    match arch {
        Arch::X86_64 => 6,
        Arch::Aarch64 => 8,
    }
}

/// Target-independent facts about one value type at a native call boundary:
/// the pure classification kernel both planes share.
///
/// The live classifier ([`NativeCallAbi`]) projects these facts from the
/// [`FrozenTypeInternPool`]; the stable query plane projects them from its
/// canonical layout value and stable type keys. The projections differ by
/// representation, but the decision tree — zero-sized omission, the `StrBuf`
/// sret rule, the return-register budget, and the compact memory-first
/// indirectness rule — lives only here, so the two planes cannot drift.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NativeAbiTypeFacts {
    /// Flattened eight-byte ABI slot count (0 for a zero-sized type).
    pub abi_slots: u32,
    /// Whether the plane's own aggregate predicate holds for the type. The
    /// planes project this differently on purpose: the live classifier keeps a
    /// discriminant-only enum scalar, while the stable projection reports it as
    /// its one register slot. Both crossings are physically identical; each
    /// plane's projection is preserved by feeding its own predicate in.
    pub aggregate: bool,
    /// Whether the type is the canonical trusted standard-library `StrBuf`,
    /// which always returns through sret.
    pub strbuf: bool,
    /// Whether the compact physical layout is byte-for-byte identical to the
    /// flattened slot representation (see [`is_slot_identical_layout`]).
    pub slot_identical: bool,
}

impl NativeAbiTypeFacts {
    /// Classify a by-value return: zero-sized returns nothing; `StrBuf`, an
    /// aggregate over the return-register budget, and a multi-slot aggregate
    /// the compact layout cannot express slot-identically use sret; any other
    /// aggregate returns in registers; everything else is a scalar.
    pub const fn classify_return(self, ret_reg_budget: u32) -> ReturnClass {
        if self.abi_slots == 0 {
            return ReturnClass::ZeroSized;
        }
        if self.strbuf
            || (self.aggregate && self.abi_slots > ret_reg_budget)
            || self.crosses_indirectly_under_compact()
        {
            return ReturnClass::Indirect {
                slot_count: self.abi_slots,
            };
        }
        if self.aggregate {
            ReturnClass::Registers {
                slot_count: self.abi_slots,
            }
        } else {
            ReturnClass::Scalar
        }
    }

    /// Classify one argument: a by-reference `inout` / `borrow` is one pointer
    /// slot; a by-value argument is omitted when zero-sized, forced indirect
    /// when the compact layout cannot express it slot-identically, and passed
    /// directly across its flattened slots otherwise.
    pub const fn classify_arg(self, convention: ArgConvention) -> ArgClass {
        match convention {
            ArgConvention::ByReference => ArgClass::Indirect,
            ArgConvention::ByValue => {
                if self.abi_slots == 0 {
                    ArgClass::Omitted
                } else if self.crosses_indirectly_under_compact() {
                    ArgClass::Indirect
                } else {
                    ArgClass::Direct {
                        slot_count: self.abi_slots,
                    }
                }
            }
        }
    }

    /// Physical parameter-slot width of one argument (ADR-0052
    /// representation 2): one pointer slot by reference, the flattened slot
    /// count by value. Deliberately independent of the compact transitional
    /// classification — see [`NativeCallAbi::arg_slot_width`].
    pub const fn arg_slot_width(self, convention: ArgConvention) -> u32 {
        match convention {
            ArgConvention::ByReference => 1,
            ArgConvention::ByValue => self.abi_slots,
        }
    }

    /// The memory-first rule (ADR-0052 ruling 9): a multi-slot aggregate whose
    /// compact representation is not slot-identical cannot cross the preserved
    /// register convention and must go indirect — *unless it occupies exactly
    /// one ABI slot* (RUE-1035), where one register transports the compact
    /// image losslessly and the one-slot path stays byte-for-byte correct.
    const fn crosses_indirectly_under_compact(self) -> bool {
        self.abi_slots > 1 && self.aggregate && !self.slot_identical
    }
}

/// How a by-value return value crosses the call boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReturnClass {
    /// Zero-sized: no return register and no caller storage.
    ZeroSized,
    /// A single scalar returned in the target's primary return register.
    Scalar,
    /// A complete aggregate returned one flattened slot per return register.
    Registers {
        /// Number of flattened return slots.
        slot_count: u32,
    },
    /// A complete aggregate written to caller-provided storage, whose address
    /// is passed as a hidden first argument (sret).
    Indirect {
        /// Number of flattened return slots the callee writes.
        slot_count: u32,
    },
}

impl ReturnClass {
    /// Number of flattened return slots represented by this classification.
    pub const fn slot_count(self) -> u32 {
        match self {
            Self::ZeroSized => 0,
            Self::Scalar => 1,
            Self::Registers { slot_count } | Self::Indirect { slot_count } => slot_count,
        }
    }

    /// Whether the return crosses indirectly through caller storage (sret).
    pub const fn uses_sret(self) -> bool {
        matches!(self, Self::Indirect { .. })
    }
}

/// How an argument is presented at the source level, before ABI classification.
///
/// The classifier only needs to distinguish a by-value argument from a
/// by-reference one; callers map their own argument-mode enum (`CfgArgMode`)
/// onto this so the classifier does not depend on the CFG crate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArgConvention {
    /// A normal by-value argument; its physical layout crosses directly.
    ByValue,
    /// An `inout` or `borrow` argument, represented by one caller pointer.
    ByReference,
}

/// How one argument crosses the call boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArgClass {
    /// Occupies no ABI slot (a zero-sized by-value argument).
    Omitted,
    /// Passed by value across `slot_count` flattened ABI slots: the first
    /// `arg_reg_budget` in argument registers, the remainder on the stack.
    Direct {
        /// Number of flattened ABI slots the value occupies.
        slot_count: u32,
    },
    /// Passed as one caller-provided pointer slot: a by-reference `inout` /
    /// `borrow`, or — transitionally under the compact layout — a by-value
    /// aggregate the preserved slot convention cannot express directly (see the
    /// module memory-first rule).
    Indirect,
}

impl ArgClass {
    /// Number of ABI *crossing* slots this classification presents: a direct
    /// value's slot count, one pointer for indirect, none when omitted.
    ///
    /// This is the representation-3 crossing width. The physical
    /// value-decomposition width the CFG and the oracle track is
    /// [`NativeCallAbi::arg_slot_width`], which is unaffected by the compact
    /// transitional rule.
    pub const fn crossing_slots(self) -> u32 {
        match self {
            Self::Omitted => 0,
            Self::Direct { slot_count } => slot_count,
            Self::Indirect => 1,
        }
    }
}

/// The native call-ABI classifier: the [`CallingConvention::Rue`] implementation.
///
/// Consumes the canonical slot decomposition and the target's return-register
/// budget. Argument register partitioning is a downstream concern of the
/// per-backend lowerer (it counts materialized ABI slots against
/// `arg_reg_budget`), so the budget is not stored here.
#[derive(Debug, Clone, Copy)]
pub struct NativeCallAbi<'a> {
    type_pool: &'a FrozenTypeInternPool,
    ret_reg_budget: u32,
}

impl<'a> NativeCallAbi<'a> {
    /// Build the native classifier for a target whose return-register budget is
    /// `ret_reg_budget` (6 on x86-64, 8 on AArch64).
    pub fn new(type_pool: &'a FrozenTypeInternPool, ret_reg_budget: u32) -> Self {
        Self {
            type_pool,
            ret_reg_budget,
        }
    }

    /// Build the native classifier for argument-side queries only
    /// ([`classify_arg`](Self::classify_arg) and
    /// [`arg_slot_width`](Self::arg_slot_width)), which do not depend on the
    /// return-register budget.
    ///
    /// The target-independent oracle uses this to model the native call
    /// contract without knowing a target's return registers; calling a
    /// return-classification method on the result is not meaningful.
    pub fn for_arguments(type_pool: &'a FrozenTypeInternPool) -> Self {
        // The return-register budget is never read by the argument-side
        // queries, so a sentinel is sound here.
        Self {
            type_pool,
            ret_reg_budget: 0,
        }
    }

    /// The convention this classifier implements.
    pub const fn abi(&self) -> CallingConvention {
        CallingConvention::Rue
    }

    /// Project the kernel facts for `ty` from the live type pool. This is the
    /// live plane's representation-specific walk; the classification decision
    /// itself lives on [`NativeAbiTypeFacts`].
    fn facts(&self, ty: Type) -> NativeAbiTypeFacts {
        let abi_slots = self.type_pool.abi_slot_count(ty);
        NativeAbiTypeFacts {
            abi_slots,
            aggregate: self.is_multislot_aggregate(ty, abi_slots),
            strbuf: matches!(
                ty.kind(),
                TypeKind::Struct(struct_id) if self.type_pool.is_strbuf(struct_id)
            ),
            slot_identical: is_slot_identical_layout(self.type_pool, ty),
        }
    }

    /// Classify how a by-value return of `ty` crosses the boundary.
    ///
    /// Reproduces the historical decision exactly: zero-sized returns nothing;
    /// canonical `StrBuf` and any aggregate whose flattened slot count exceeds
    /// the return-register budget use sret; a smaller aggregate returns in
    /// registers; everything else is a scalar. Under the compact layout a
    /// non-slot-identical aggregate is forced indirect (memory-first rule).
    pub fn classify_return(&self, ty: Type) -> ReturnClass {
        self.facts(ty).classify_return(self.ret_reg_budget)
    }

    /// Whether a by-value return of `ty` uses the sret convention. Thin
    /// predicate over [`classify_return`] for the decision sites that only need
    /// the boolean.
    pub fn return_is_sret(&self, ty: Type) -> bool {
        self.classify_return(ty).uses_sret()
    }

    /// Classify how one argument crosses the boundary.
    ///
    /// A by-reference `inout` / `borrow` is one pointer slot. A by-value
    /// argument is omitted when zero-sized, otherwise passed directly across its
    /// flattened slots — except that under the compact layout a non-slot-
    /// identical aggregate is forced indirect (memory-first rule).
    pub fn classify_arg(&self, ty: Type, convention: ArgConvention) -> ArgClass {
        match convention {
            // The kernel's by-reference rule is convention-only, so the facts
            // projection (a pool walk) is skipped for it.
            ArgConvention::ByReference => ArgClass::Indirect,
            ArgConvention::ByValue => self.facts(ty).classify_arg(ArgConvention::ByValue),
        }
    }

    /// Physical parameter-slot width of one argument: the value-decomposition
    /// count the CFG parameter layout and the oracle's call contract track.
    ///
    /// A by-reference argument is one pointer slot; a by-value argument is its
    /// flattened slot count. This is representation 2 in ADR-0052 terms and is
    /// deliberately independent of the compact transitional classification: the
    /// callee's parameter slots stay slot-shaped even for a compact aggregate,
    /// which is precisely why code generation refuses the not-yet-implemented
    /// indirect marshaling rather than silently disagreeing about slot counts.
    pub fn arg_slot_width(&self, ty: Type, convention: ArgConvention) -> u32 {
        match convention {
            ArgConvention::ByReference => 1,
            ArgConvention::ByValue => self.type_pool.abi_slot_count(ty),
        }
    }

    /// The live plane's aggregate predicate: whether `ty` needs a complete
    /// aggregate slot representation rather than a single primary vreg.
    /// Discriminant-only enums stay scalar (oversized enums route through the
    /// same slot-count policy per RUE-946). This is a facts *projection*; the
    /// decisions consuming it live on [`NativeAbiTypeFacts`].
    fn is_multislot_aggregate(&self, ty: Type, slot_count: u32) -> bool {
        matches!(ty.kind(), TypeKind::Struct(_) | TypeKind::Array(_))
            || (ty.is_enum() && slot_count > 1)
    }
}

/// Whether the compact physical layout of `ty` is byte-for-byte identical to
/// the flattened eight-byte slot layout, so slot-shaped marshaling is exactly
/// correct for it (ADR-0052).
///
/// True for eight-byte leaves (`i64`/`u64`/pointers, the recovery scalar) and
/// zero-sized / compile-time-only types, and for aggregates built entirely from
/// slot-identical leaves. Narrow scalars (one/two/four bytes) and enums (narrow
/// tag) are not slot-identical. This is the single authority both the compact
/// call-ABI transitional rule above and code generation's narrow-access refusal
/// (RUE-974) consult, so they cannot disagree about which types the compact
/// layout leaves unchanged.
pub fn is_slot_identical_layout<P: crate::FfiTypePool + ?Sized>(type_pool: &P, ty: Type) -> bool {
    match ty.kind() {
        // Eight-byte leaves and the recovery scalar: identical in both models.
        TypeKind::I64
        | TypeKind::U64
        | TypeKind::PtrConst(_)
        | TypeKind::PtrMut(_)
        | TypeKind::Error => true,
        // Zero-sized and compile-time-only types have identical (zero) extent.
        TypeKind::Unit
        | TypeKind::Never
        | TypeKind::ComptimeType
        | TypeKind::ComptimeFloat
        | TypeKind::Module(_) => true,
        // Phase 4 deliberately has no float ABI lowering yet.
        TypeKind::F32 | TypeKind::F64 => false,
        // Narrow scalars: one/two/four bytes under the compact layout.
        TypeKind::I8
        | TypeKind::U8
        | TypeKind::Bool
        | TypeKind::I16
        | TypeKind::U16
        | TypeKind::I32
        | TypeKind::U32 => false,
        TypeKind::Struct(id) => type_pool
            .ffi_struct_field_types(id)
            .into_iter()
            .all(|field_ty| is_slot_identical_layout(type_pool, field_ty)),
        TypeKind::Array(id) => {
            let element = type_pool.ffi_array_element(id);
            is_slot_identical_layout(type_pool, element)
        }
        // Enums narrow their tag (u8/u16/u32 vs an eight-byte slot).
        TypeKind::Enum(_) => false,
    }
}

// ============================================================================
// The guaranteed target-C classifier (ADR-0064 P2, RUE-1056)
// ============================================================================

/// Width-and-signedness class of a target-C-passable scalar: the one fact the
/// extension policy needs. Each plane projects its own type representation
/// onto this class, so the sign/zero/`_Bool` extension table itself
/// ([`Self::extension`]) has exactly one home.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CAbiScalarKind {
    /// 8-bit signed integer.
    I8,
    /// 16-bit signed integer.
    I16,
    /// 32-bit signed integer.
    I32,
    /// 8-bit unsigned integer.
    U8,
    /// 16-bit unsigned integer.
    U16,
    /// 32-bit unsigned integer.
    U32,
    /// The 1-byte C `_Bool` whose byte is 0/1 by contract.
    Bool,
    /// A value that already fills its 64-bit register: `i64`/`u64`, pointers,
    /// and the recovery scalar.
    RegisterWidth,
}

impl CAbiScalarKind {
    /// The live plane's projection of a type onto its width-and-signedness
    /// class, or `None` when the type is not a target-C-passable scalar (an
    /// aggregate, or a type `c_passable_by_value` rejects). The stable query
    /// plane makes the same projection from its own type keys.
    pub fn for_live_type(ty: Type) -> Option<Self> {
        Some(match ty.kind() {
            TypeKind::I8 => Self::I8,
            TypeKind::I16 => Self::I16,
            TypeKind::I32 => Self::I32,
            TypeKind::U8 => Self::U8,
            TypeKind::U16 => Self::U16,
            TypeKind::U32 => Self::U32,
            TypeKind::Bool => Self::Bool,
            TypeKind::I64
            | TypeKind::U64
            | TypeKind::PtrConst(_)
            | TypeKind::PtrMut(_)
            | TypeKind::Error => Self::RegisterWidth,
            _ => return None,
        })
    }

    /// The register bank this scalar travels in. Every scalar the C boundary
    /// currently admits is an integer or a pointer, so every one is
    /// general-purpose; floats join this projection when they cross.
    pub const fn register_class(self) -> CRegisterClass {
        CRegisterClass::Gp
    }

    /// The canonical 64-bit extension for this scalar at a target-C boundary.
    /// Both psABIs agree on the operation; signed narrows sign-extend from
    /// their declared width, unsigned narrows zero-extend, `_Bool`
    /// zero-extends from its low byte, and register-width values need nothing.
    pub const fn extension(self) -> ScalarAbiExtension {
        match self {
            Self::I8 => ScalarAbiExtension::Signed { from_bits: 8 },
            Self::I16 => ScalarAbiExtension::Signed { from_bits: 16 },
            Self::I32 => ScalarAbiExtension::Signed { from_bits: 32 },
            Self::U8 | Self::Bool => ScalarAbiExtension::Unsigned { from_bits: 8 },
            Self::U16 => ScalarAbiExtension::Unsigned { from_bits: 16 },
            Self::U32 => ScalarAbiExtension::Unsigned { from_bits: 32 },
            Self::RegisterWidth => ScalarAbiExtension::None,
        }
    }
}

/// How a narrow scalar is extended to fill its 64-bit integer register at a
/// target-C boundary (ADR-0064 P2). "Narrow" means any value smaller than the
/// register: the sub-64-bit integers and `bool`. The extension is the same
/// operation whether it is applied by the caller before an argument crosses or
/// by the caller after a return crosses — see [`TargetCCallAbi`] for *who*
/// applies it under each psABI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScalarAbiExtension {
    /// The value already fills its 64-bit register (`i64`/`u64`/pointer): no
    /// extension instruction is emitted.
    None,
    /// Sign-extend the low `from_bits` to 64 (a signed narrow integer:
    /// `i8`/`i16`/`i32`).
    Signed {
        /// The declared width of the value, in bits (8, 16, or 32).
        from_bits: u32,
    },
    /// Zero-extend the low `from_bits` to 64 (an unsigned narrow integer
    /// `u8`/`u16`/`u32`, or `bool` as the 1-byte `_Bool` whose byte is 0/1 by
    /// contract, `from_bits == 8`).
    Unsigned {
        /// The declared width of the value, in bits (8, 16, or 32).
        from_bits: u32,
    },
}

impl ScalarAbiExtension {
    /// Whether this extension emits no instruction (the value is already
    /// register-width canonical).
    pub const fn is_noop(self) -> bool {
        matches!(self, Self::None)
    }

    /// The scalar's natural C width in bytes: the declared width a narrow value
    /// is extended *from*, or the full 8-byte register for a register-width
    /// value. This is the size a stacked argument occupies under a psABI that
    /// packs the outgoing argument area at natural size
    /// ([`StackedArgumentPacking::NaturalSize`]).
    pub const fn natural_bytes(self) -> u32 {
        match self {
            Self::None => 8,
            Self::Signed { from_bits } | Self::Unsigned { from_bits } => from_bits / 8,
        }
    }
}

/// The guaranteed target-C call-ABI classifier: the classifier for one C row of
/// [`CallingConvention`] (ADR-0064).
///
/// This answers the per-convention *facts* a scalar crossing needs — the
/// narrow-integer extension, the argument-register budget, the sret register and
/// echo, the call-boundary alignment — each read from the convention's
/// [`CConventionSpec`]. It is constructed from a convention, and a target
/// reaches it through the one `"C"` alias table ([`Self::for_target`]), so the
/// two AArch64 targets, which share an architecture, get their own rows.
///
/// *Where* a value crosses is not this type's answer: that is
/// [`lower_c_signature`](crate::lower_c_signature), the one placement function
/// every C crossing site consumes.
///
/// ## What P2 fixes, and why scalars need only the return re-extension
///
/// Every supported scalar (`c_passable_by_value`: the full integer set, `bool`,
/// pointers) occupies exactly one general-purpose integer register — the first
/// six on SysV (`rdi, rsi, rdx, rcx, r8, r9`), the first eight on AAPCS64
/// (`x0..x7`) — and spills to the outgoing argument area only once that budget
/// is exhausted. Rue's
/// internal scalar invariant already keeps a narrow value canonically extended
/// in its 64-bit vreg (signed values sign-extended, unsigned/`bool`
/// zero-extended), which is a *stronger* guarantee than either psABI asks of an
/// argument, so **argument passing needs no boundary instruction**. The one
/// direction that does is the **return**: a C callee leaves the bits above the
/// result's declared width unspecified (SysV) — or extended only to 32 bits
/// (AAPCS64) — so the caller must re-extend the returned scalar to Rue's
/// canonical 64-bit form. [`Self::scalar_return_extension`] names that
/// operation; applying it at the boundary is what preserves the program-wide
/// scalar invariant across a foreign call.
///
/// ## Narrow-integer extension: who extends
///
/// - **SysV AMD64.** Arguments narrower than the register have unspecified high
///   bits; a callee that needs a wider value re-extends. Rue over-satisfies the
///   argument rule by always passing a canonically extended value. For a return,
///   the bits above the type width are callee-visible garbage, so the **caller
///   re-extends** (this classifier's return extension).
/// - **AAPCS64.** A callee is required to extend a narrow return value to 32
///   bits, but bits 32..63 stay unspecified, so re-extending from the value's
///   *own* declared width is still correct (bits already agree up to 32). The
///   caller therefore applies the same [`ScalarAbiExtension`].
/// - **Apple arm64.** The *caller* must extend an argument narrower than 32
///   bits. Rue's canonical 64-bit form already satisfies that, so the import
///   side needs no extra instruction, and the export thunk re-extends on every
///   row because the native body needs the stronger 64-bit form regardless.
///
/// ## `_Bool`
///
/// C `_Bool` is one byte whose only valid values are 0 and 1. Passing Rue's
/// `bool` (a 0/1 word) satisfies that directly; a `_Bool` return is
/// zero-extended from its low byte, materializing exactly 0/1
/// ([`ScalarAbiExtension::Unsigned`] `{ from_bits: 8 }`).
#[derive(Debug, Clone, Copy)]
pub struct TargetCCallAbi {
    convention: CallingConvention,
}

impl TargetCCallAbi {
    /// Build the classifier for one platform C convention.
    ///
    /// `convention` must be a C row; [`CallingConvention::Rue`] is the native
    /// convention and is classified by [`NativeCallAbi`] instead.
    pub const fn new(convention: CallingConvention) -> Self {
        assert!(
            convention.is_c(),
            "TargetCCallAbi classifies a C boundary; the native Rue convention \
             is NativeCallAbi's authority"
        );
        Self { convention }
    }

    /// The classifier for the convention `target`'s `"C"` boundary follows.
    pub const fn for_target(target: rue_target::Target) -> Self {
        Self::new(CallingConvention::c_for_target(target))
    }

    /// The SysV AMD64 classifier (x86-64 Linux).
    pub const fn sysv_amd64() -> Self {
        Self::new(CallingConvention::X86_64SysV)
    }

    /// The AAPCS64 classifier (AArch64 Linux).
    pub const fn aapcs64() -> Self {
        Self::new(CallingConvention::Aarch64Aapcs)
    }

    /// The Apple arm64 classifier (AArch64 macOS): AAPCS64 with Apple's
    /// amendments.
    pub const fn aapcs64_darwin() -> Self {
        Self::new(CallingConvention::Aarch64AapcsDarwin)
    }

    /// The convention this classifier implements.
    pub const fn convention(&self) -> CallingConvention {
        self.convention
    }

    /// How this psABI lays a stacked (byval / register-overflow) argument out in
    /// the outgoing argument area. Every C row answers; the packing rule is the
    /// convention's, so no backend needs an operating-system test.
    pub const fn stacked_argument_packing(&self) -> StackedArgumentPacking {
        self.convention.stacked_argument_packing()
    }

    /// This convention's complete psABI description, the one table every
    /// predicate below reads.
    pub const fn spec(&self) -> CConventionSpec {
        self.convention.c_spec()
    }

    /// The number of general-purpose integer registers used for arguments
    /// before the outgoing stack area begins: 6 on SysV (`rdi..r9`), 8 on
    /// AAPCS64 (`x0..x7`).
    pub const fn int_arg_register_budget(&self) -> u32 {
        self.spec().gp_argument_registers
    }

    /// Whether the callee echoes the hidden sret pointer back in the primary
    /// return register: SysV requires `rax` to hold the sret pointer on return;
    /// AAPCS64 uses the dedicated indirect-result register `x8`, which is **not**
    /// echoed. Reachable from P3: an aggregate return >16 bytes takes the sret
    /// path; scalars in P2 never do.
    pub const fn sret_pointer_echoed_in_result_register(&self) -> bool {
        self.spec().sret_pointer_echoed_in_result_register
    }

    /// Whether the hidden sret pointer is passed in a **dedicated** indirect-
    /// result register rather than the first ordinary integer argument register.
    /// AAPCS64 uses the dedicated `x8` (§6.9), so the sret pointer does not
    /// consume `x0` and the ordinary arguments still start at `x0`. SysV AMD64
    /// passes the sret pointer as the hidden first argument in `rdi`, consuming
    /// the first integer argument register. P3 aggregate returns exercise both.
    pub const fn sret_pointer_in_dedicated_register(&self) -> bool {
        !self.spec().sret_pointer_in_argument_register()
    }

    /// The stack alignment required at a `call` instruction on this psABI: 16
    /// bytes on both SysV AMD64 and AAPCS64.
    pub const fn call_stack_alignment(&self) -> u32 {
        self.spec().call_stack_alignment
    }

    /// The extension a scalar *return* value needs to become Rue's canonical
    /// 64-bit form after crossing back from C. `None` for register-width scalars
    /// (`i64`/`u64`/pointer); a sign/zero extension for a narrow integer; a
    /// zero-extend-from-8 for `bool`/`_Bool`.
    ///
    /// The argument-side extension is the same operation
    /// ([`Self::scalar_arg_extension`]); the two are separated only so the
    /// "who extends" documentation can differ per direction.
    pub fn scalar_return_extension(&self, ty: Type) -> ScalarAbiExtension {
        Self::canonical_extension(ty)
    }

    /// The extension a scalar *argument* value must already carry when it
    /// crosses into C. Identical to [`Self::scalar_return_extension`]; Rue's
    /// internal scalar invariant already satisfies it, so the caller emits no
    /// extra instruction for arguments in P2.
    pub fn scalar_arg_extension(&self, ty: Type) -> ScalarAbiExtension {
        Self::canonical_extension(ty)
    }

    /// The live plane's projection of a supported target-C scalar onto its
    /// width-and-signedness class; the extension operation itself lives on
    /// [`CAbiScalarKind::extension`], shared with the stable query plane.
    fn canonical_extension(ty: Type) -> ScalarAbiExtension {
        CAbiScalarKind::for_live_type(ty)
            .unwrap_or_else(|| {
                panic!(
                    "TargetCCallAbi scalar classification called on non-scalar type {:?}; \
                     aggregates and unsupported types are gated by c_passable_by_value \
                     before lowering",
                    ty.kind()
                )
            })
            .extension()
    }
}

/// The live plane's projection of `ty` onto the target-C classification facts
/// [`lower_c_signature`](crate::lower_c_signature) consumes.
///
/// This is the C-boundary twin of [`NativeCallAbi::facts`]: it walks the
/// request-scoped [`FrozenTypeInternPool`], while the stable query plane makes
/// the same projection from its revision-stable type keys and canonical layout
/// values. Both then classify through the one kernel, so a call, a return, and
/// an export cannot disagree about a placement.
///
/// `ty` must already have passed
/// [`c_passable_by_value`](crate::c_passable_by_value); an unsupported type
/// panics rather than being guessed at.
pub fn c_abi_type_facts(type_pool: &FrozenTypeInternPool, ty: Type) -> CAbiTypeFacts {
    if matches!(ty.kind(), TypeKind::Struct(_) | TypeKind::Array(_)) {
        let layout = type_pool.layout(ty);
        return CAbiTypeFacts::Aggregate {
            size: layout.size,
            align: layout.alignment,
            leaves: aggregate_leaves(type_pool, ty, layout.size),
        };
    }
    if type_pool.abi_slot_count(ty) == 0 {
        return CAbiTypeFacts::ZeroSized;
    }
    let kind = CAbiScalarKind::for_live_type(ty).unwrap_or_else(|| {
        panic!(
            "target-C classification called on unsupported type {:?}; \
             c_passable_by_value gates the boundary before lowering",
            ty.kind()
        )
    });
    CAbiTypeFacts::Scalar {
        kind,
        class: kind.register_class(),
    }
}

/// The live plane's projection of an aggregate's scalar leaves.
///
/// Classification asks which bank each eightbyte belongs to and whether the
/// whole aggregate is a homogeneous floating-point aggregate, and both answers
/// come from the leaves: a scalar field's byte offset, its width, and whether
/// it is an integer, an `f32` or an `f64`. The walk reads the same canonical
/// layout every other physical consumer reads — struct field offsets, array
/// element stride, the enum tag and per-variant payload offsets — so the leaves
/// describe the type's actual memory image.
///
/// An aggregate larger than [`crate::MAX_LEAF_CLASSIFIED_BYTES`] is reported all
/// integer without a walk: no supported row's answer for one that large depends
/// on its leaves, so this bounds the cost of a large array.
pub fn aggregate_leaves(
    type_pool: &FrozenTypeInternPool,
    ty: Type,
    size: u64,
) -> crate::AggregateLeaves {
    if size > crate::MAX_LEAF_CLASSIFIED_BYTES {
        return crate::AggregateLeaves::all_integer(size);
    }
    let mut leaves = Vec::new();
    push_leaves(type_pool, ty, 0, &mut leaves);
    crate::AggregateLeaves::from_leaves(size, leaves)
}

/// The classification kind of one scalar leaf: the floats are told apart from
/// each other because AAPCS64's homogeneous rule is keyed by member width, and
/// everything else — integers, `bool`, pointers — is one kind.
fn leaf_kind(ty: Type) -> crate::CAbiLeafKind {
    match ty.kind() {
        TypeKind::F32 => crate::CAbiLeafKind::F32,
        TypeKind::F64 => crate::CAbiLeafKind::F64,
        _ => crate::CAbiLeafKind::Integer,
    }
}

/// Append every scalar leaf of `ty`, placed at `base` bytes, to `out`.
///
/// An enum contributes its tag as an integer leaf and then the *union* of every
/// variant's payload leaves, each at that variant's own offsets: a union's
/// eightbyte is classified by every leaf that can occupy it, which is the same
/// merge SysV AMD64 section 3.2.3 applies to a C union.
fn push_leaves(
    type_pool: &FrozenTypeInternPool,
    ty: Type,
    base: u64,
    out: &mut Vec<crate::CAbiLeaf>,
) {
    match ty.kind() {
        TypeKind::Unit | TypeKind::Never => {}
        TypeKind::Struct(struct_id) => {
            let layout = type_pool.layout(ty);
            let crate::LayoutKind::Struct { field_offsets, .. } = &layout.kind else {
                return;
            };
            let struct_def = type_pool.struct_def(struct_id);
            for (field, offset) in struct_def.fields.iter().zip(field_offsets) {
                push_leaves(type_pool, field.ty, base.saturating_add(*offset), out);
            }
        }
        TypeKind::Array(array_id) => {
            let (element, count) = type_pool.array_def(array_id);
            let stride = type_pool.layout(element).stride;
            for index in 0..count {
                push_leaves(
                    type_pool,
                    element,
                    base.saturating_add(index.saturating_mul(stride)),
                    out,
                );
            }
        }
        TypeKind::Enum(enum_id) => {
            let layout = type_pool.layout(ty);
            let crate::LayoutKind::Enum { tag, variants, .. } = &layout.kind else {
                return;
            };
            out.push(crate::CAbiLeaf::integer(base, tag.size));
            let enum_def = type_pool.enum_def(enum_id);
            for (variant, offsets) in variants.iter().enumerate() {
                for (payload, offset) in enum_def.variant_payload(variant).iter().zip(offsets) {
                    push_leaves(type_pool, *payload, base.saturating_add(*offset), out);
                }
            }
        }
        _ => out.push(crate::CAbiLeaf {
            offset: base,
            width: type_pool.layout(ty).size,
            kind: leaf_kind(ty),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Behavioral coverage that exercises the classifier against a real type
    // pool lives with the backend ABI/oracle suites (which own program
    // fixtures); these unit checks pin the pure classification algebra, and the
    // leaf projection against a small pool of its own.

    fn pool_with_struct(fields: &[(&str, Type)]) -> (crate::FrozenTypeInternPool, Type) {
        let interner = lasso::ThreadedRodeo::new();
        let pool = crate::TypeInternPool::new();
        let (id, _) = pool.register_struct(
            interner.get_or_intern("Probe"),
            crate::StructDef {
                name: "Probe".into(),
                fields: fields
                    .iter()
                    .map(|(name, ty)| crate::StructField {
                        name: (*name).to_string(),
                        ty: *ty,
                    })
                    .collect(),
                is_copy: true,
                is_linear: false,
                declared_linear: false,
                destructor: None,
                is_builtin: false,
                is_pub: false,
                file_id: rue_span::FileId::DEFAULT,
            },
        );
        let ty = Type::new_struct(id);
        let pool = pool.freeze();
        (pool, ty)
    }

    #[test]
    fn the_live_plane_projects_each_leaf_at_its_own_offset_and_kind() {
        let (pool, mixed) = pool_with_struct(&[("a", Type::F64), ("b", Type::I64)]);
        let facts = c_abi_type_facts(&pool, mixed);
        let CAbiTypeFacts::Aggregate { size, leaves, .. } = facts else {
            panic!("a struct projects aggregate facts, got {facts:?}");
        };
        assert_eq!(
            leaves.eightbyte_class(0),
            crate::EightbyteClass::Sse,
            "the `f64` field's eightbyte carries no integer leaf"
        );
        assert_eq!(
            leaves.eightbyte_class(size.div_ceil(8) as u32 - 1),
            crate::EightbyteClass::Integer
        );
        assert_eq!(leaves.homogeneous_floats(), None);
        assert!(!leaves.has_unaligned_leaf());

        // Two floats of one width are a homogeneous floating-point aggregate,
        // whatever the layout in force spaces them at.
        let (pool, floats) = pool_with_struct(&[("a", Type::F64), ("b", Type::F64)]);
        let facts = c_abi_type_facts(&pool, floats);
        let CAbiTypeFacts::Aggregate { leaves, .. } = facts else {
            panic!("a struct projects aggregate facts, got {facts:?}");
        };
        assert_eq!(leaves.homogeneous_floats(), Some((8, 2)));
        assert_eq!(leaves.eightbyte_class(0), crate::EightbyteClass::Sse);
        assert_eq!(leaves.eightbyte_class(1), crate::EightbyteClass::Sse);
    }

    #[test]
    fn an_aggregate_past_the_leaf_bound_is_projected_all_integer() {
        // Nothing any row does with an aggregate this large depends on its
        // leaves, so the projection stops rather than walking it.
        let pool = crate::TypeInternPool::new();
        let array = pool.intern_array_from_type(Type::F64, 64);
        let pool = pool.freeze();
        let ty = Type::new_array(array);
        assert!(pool.layout(ty).size > crate::MAX_LEAF_CLASSIFIED_BYTES);
        let facts = c_abi_type_facts(&pool, ty);
        let CAbiTypeFacts::Aggregate { size, leaves, .. } = facts else {
            panic!("an array projects aggregate facts, got {facts:?}");
        };
        assert_eq!(leaves, crate::AggregateLeaves::all_integer(size));
    }

    #[test]
    fn target_c_scalar_return_extension_table() {
        let sysv = TargetCCallAbi::sysv_amd64();
        assert_eq!(
            sysv.scalar_return_extension(Type::I8),
            ScalarAbiExtension::Signed { from_bits: 8 }
        );
        assert_eq!(
            sysv.scalar_return_extension(Type::U8),
            ScalarAbiExtension::Unsigned { from_bits: 8 }
        );
        assert_eq!(
            sysv.scalar_return_extension(Type::I16),
            ScalarAbiExtension::Signed { from_bits: 16 }
        );
        assert_eq!(
            sysv.scalar_return_extension(Type::U16),
            ScalarAbiExtension::Unsigned { from_bits: 16 }
        );
        assert_eq!(
            sysv.scalar_return_extension(Type::I32),
            ScalarAbiExtension::Signed { from_bits: 32 }
        );
        assert_eq!(
            sysv.scalar_return_extension(Type::U32),
            ScalarAbiExtension::Unsigned { from_bits: 32 }
        );
        // The 1-byte `_Bool` 0/1 contract: zero-extend from its byte.
        assert_eq!(
            sysv.scalar_return_extension(Type::BOOL),
            ScalarAbiExtension::Unsigned { from_bits: 8 }
        );
        // Register-width scalars need no extension.
        assert!(sysv.scalar_return_extension(Type::I64).is_noop());
        assert!(sysv.scalar_return_extension(Type::U64).is_noop());
    }

    #[test]
    fn both_flavors_agree_on_the_scalar_extension_operation() {
        // The narrow-integer extension is the same operation on both psABIs; the
        // flavors differ only in documented "who extends" / sret-echo details.
        let sysv = TargetCCallAbi::sysv_amd64();
        let aapcs = TargetCCallAbi::aapcs64();
        for ty in [
            Type::I8,
            Type::U8,
            Type::I16,
            Type::U16,
            Type::I32,
            Type::U32,
            Type::I64,
            Type::U64,
            Type::BOOL,
        ] {
            assert_eq!(
                sysv.scalar_return_extension(ty),
                aapcs.scalar_return_extension(ty),
                "flavors must agree on the extension for {ty:?}"
            );
            assert_eq!(
                sysv.scalar_arg_extension(ty),
                sysv.scalar_return_extension(ty),
                "arg and return extension are the same operation for {ty:?}"
            );
        }
    }

    #[test]
    fn flavor_specific_register_and_sret_echo_facts() {
        let sysv = TargetCCallAbi::sysv_amd64();
        let aapcs = TargetCCallAbi::aapcs64();
        assert_eq!(sysv.int_arg_register_budget(), 6);
        assert_eq!(aapcs.int_arg_register_budget(), 8);
        // SysV echoes the sret pointer in rax; AAPCS64's x8 is not echoed.
        assert!(sysv.sret_pointer_echoed_in_result_register());
        assert!(!aapcs.sret_pointer_echoed_in_result_register());
        // 16-byte call alignment on both.
        assert_eq!(sysv.call_stack_alignment(), 16);
        assert_eq!(aapcs.call_stack_alignment(), 16);
        assert_eq!(sysv.convention(), CallingConvention::X86_64SysV);
        assert_eq!(aapcs.convention(), CallingConvention::Aarch64Aapcs);
    }

    #[test]
    fn return_class_slot_count_and_sret_predicate_agree() {
        assert_eq!(ReturnClass::ZeroSized.slot_count(), 0);
        assert_eq!(ReturnClass::Scalar.slot_count(), 1);
        assert_eq!(ReturnClass::Registers { slot_count: 3 }.slot_count(), 3);
        assert_eq!(ReturnClass::Indirect { slot_count: 9 }.slot_count(), 9);
        assert!(!ReturnClass::Registers { slot_count: 3 }.uses_sret());
        assert!(ReturnClass::Indirect { slot_count: 9 }.uses_sret());
    }

    #[test]
    fn sret_pointer_register_and_echo_diverge_by_psabi() {
        let sysv = TargetCCallAbi::sysv_amd64();
        let aapcs = TargetCCallAbi::aapcs64();
        // SysV: sret pointer in rdi (first arg reg), echoed in rax.
        assert!(!sysv.sret_pointer_in_dedicated_register());
        assert!(sysv.sret_pointer_echoed_in_result_register());
        // AAPCS64: dedicated x8, not echoed.
        assert!(aapcs.sret_pointer_in_dedicated_register());
        assert!(!aapcs.sret_pointer_echoed_in_result_register());
    }

    #[test]
    fn per_arch_native_budget_has_one_home() {
        assert_eq!(native_return_register_budget(Arch::X86_64), 6);
        assert_eq!(native_return_register_budget(Arch::Aarch64), 8);
    }

    #[test]
    fn the_classifier_follows_the_target_c_alias_not_the_architecture() {
        use rue_target::Target;
        assert_eq!(
            TargetCCallAbi::for_target(Target::X86_64Linux).convention(),
            CallingConvention::X86_64SysV
        );
        assert_eq!(
            TargetCCallAbi::for_target(Target::Aarch64Linux).convention(),
            CallingConvention::Aarch64Aapcs
        );
        assert_eq!(
            TargetCCallAbi::for_target(Target::Aarch64Macos).convention(),
            CallingConvention::Aarch64AapcsDarwin
        );
    }

    #[test]
    fn the_two_aapcs_rows_agree_except_on_stacked_argument_packing() {
        let aapcs = TargetCCallAbi::aapcs64();
        let darwin = TargetCCallAbi::aapcs64_darwin();
        assert_eq!(
            aapcs.int_arg_register_budget(),
            darwin.int_arg_register_budget()
        );
        assert_eq!(
            aapcs.sret_pointer_in_dedicated_register(),
            darwin.sret_pointer_in_dedicated_register()
        );
        assert_eq!(
            aapcs.sret_pointer_echoed_in_result_register(),
            darwin.sret_pointer_echoed_in_result_register()
        );
        assert_eq!(aapcs.call_stack_alignment(), darwin.call_stack_alignment());
        // Apple's amendment: a stacked argument occupies its natural size at
        // its natural alignment rather than a whole 8-byte slot.
        assert_eq!(
            aapcs.stacked_argument_packing(),
            StackedArgumentPacking::EightByteSlots
        );
        assert_eq!(
            darwin.stacked_argument_packing(),
            StackedArgumentPacking::NaturalSize
        );
        assert_eq!(
            TargetCCallAbi::sysv_amd64().stacked_argument_packing(),
            StackedArgumentPacking::EightByteSlots,
            "x86-64 is unaffected by the Apple amendment"
        );
    }

    #[test]
    fn every_c_row_agrees_on_the_caller_side_narrow_extension() {
        // Apple requires the caller to extend an argument narrower than 32 bits;
        // Rue's canonical 64-bit-extension invariant already produces that, and
        // every C row asks for the same operation, so the import side needs no
        // Darwin-specific work.
        for convention in [
            CallingConvention::X86_64SysV,
            CallingConvention::Aarch64Aapcs,
            CallingConvention::Aarch64AapcsDarwin,
        ] {
            let abi = TargetCCallAbi::new(convention);
            assert_eq!(
                abi.scalar_arg_extension(Type::I8),
                ScalarAbiExtension::Signed { from_bits: 8 },
                "{convention} must sign-extend a narrow signed argument"
            );
            assert_eq!(
                abi.scalar_arg_extension(Type::U16),
                ScalarAbiExtension::Unsigned { from_bits: 16 },
                "{convention} must zero-extend a narrow unsigned argument"
            );
            assert_eq!(
                abi.scalar_arg_extension(Type::BOOL),
                ScalarAbiExtension::Unsigned { from_bits: 8 },
                "{convention} must materialize the 1-byte `_Bool` 0/1 contract"
            );
            assert!(abi.scalar_arg_extension(Type::I64).is_noop());
        }
    }

    #[test]
    fn native_facts_kernel_decision_table() {
        let scalar = NativeAbiTypeFacts {
            abi_slots: 1,
            aggregate: false,
            strbuf: false,
            slot_identical: true,
        };
        let aggregate = |abi_slots: u32, slot_identical: bool| NativeAbiTypeFacts {
            abi_slots,
            aggregate: true,
            strbuf: false,
            slot_identical,
        };
        for budget in [6u32, 8] {
            // Zero-sized values vanish in both positions.
            let zero = NativeAbiTypeFacts {
                abi_slots: 0,
                aggregate: true,
                strbuf: false,
                slot_identical: true,
            };
            assert_eq!(zero.classify_return(budget), ReturnClass::ZeroSized);
            assert_eq!(zero.classify_arg(ArgConvention::ByValue), ArgClass::Omitted);

            assert_eq!(scalar.classify_return(budget), ReturnClass::Scalar);
            assert_eq!(
                scalar.classify_arg(ArgConvention::ByValue),
                ArgClass::Direct { slot_count: 1 }
            );
            // By-reference is one pointer slot regardless of the facts.
            assert_eq!(
                aggregate(budget + 1, true).classify_arg(ArgConvention::ByReference),
                ArgClass::Indirect
            );
            assert_eq!(
                aggregate(budget + 1, true).arg_slot_width(ArgConvention::ByReference),
                1
            );

            // The return-register budget boundary: budget - 1 and budget fit,
            // budget + 1 goes through sret. Arguments ignore the budget.
            assert_eq!(
                aggregate(budget - 1, true).classify_return(budget),
                ReturnClass::Registers {
                    slot_count: budget - 1
                }
            );
            assert_eq!(
                aggregate(budget, true).classify_return(budget),
                ReturnClass::Registers { slot_count: budget }
            );
            assert_eq!(
                aggregate(budget + 1, true).classify_return(budget),
                ReturnClass::Indirect {
                    slot_count: budget + 1
                }
            );
            assert_eq!(
                aggregate(budget + 1, true).classify_arg(ArgConvention::ByValue),
                ArgClass::Direct {
                    slot_count: budget + 1
                }
            );

            // The compact memory-first rule: a multi-slot non-slot-identical
            // aggregate goes indirect in both positions; a single-slot one
            // stays direct (RUE-1035).
            assert_eq!(
                aggregate(2, false).classify_return(budget),
                ReturnClass::Indirect { slot_count: 2 }
            );
            assert_eq!(
                aggregate(2, false).classify_arg(ArgConvention::ByValue),
                ArgClass::Indirect
            );
            assert_eq!(
                aggregate(1, false).classify_return(budget),
                ReturnClass::Registers { slot_count: 1 }
            );
            assert_eq!(
                aggregate(1, false).classify_arg(ArgConvention::ByValue),
                ArgClass::Direct { slot_count: 1 }
            );

            // Canonical StrBuf always returns through sret, even under budget.
            let strbuf = NativeAbiTypeFacts {
                abi_slots: 3,
                aggregate: true,
                strbuf: true,
                slot_identical: true,
            };
            assert_eq!(
                strbuf.classify_return(budget),
                ReturnClass::Indirect { slot_count: 3 }
            );
            assert_eq!(
                strbuf.classify_arg(ArgConvention::ByValue),
                ArgClass::Direct { slot_count: 3 }
            );
        }
    }

    #[test]
    fn c_scalar_kind_extension_table_is_the_shared_authority() {
        use CAbiScalarKind as K;
        assert_eq!(
            K::I8.extension(),
            ScalarAbiExtension::Signed { from_bits: 8 }
        );
        assert_eq!(
            K::I16.extension(),
            ScalarAbiExtension::Signed { from_bits: 16 }
        );
        assert_eq!(
            K::I32.extension(),
            ScalarAbiExtension::Signed { from_bits: 32 }
        );
        assert_eq!(
            K::U8.extension(),
            ScalarAbiExtension::Unsigned { from_bits: 8 }
        );
        assert_eq!(
            K::U16.extension(),
            ScalarAbiExtension::Unsigned { from_bits: 16 }
        );
        assert_eq!(
            K::U32.extension(),
            ScalarAbiExtension::Unsigned { from_bits: 32 }
        );
        // The 1-byte `_Bool` 0/1 contract zero-extends from its byte.
        assert_eq!(
            K::Bool.extension(),
            ScalarAbiExtension::Unsigned { from_bits: 8 }
        );
        assert!(K::RegisterWidth.extension().is_noop());
    }

    #[test]
    fn arg_class_crossing_width_matches_the_slot_contract() {
        assert_eq!(ArgClass::Omitted.crossing_slots(), 0);
        assert_eq!(ArgClass::Direct { slot_count: 4 }.crossing_slots(), 4);
        // A by-reference argument and a transitional indirect aggregate both
        // cross as one pointer.
        assert_eq!(ArgClass::Indirect.crossing_slots(), 1);
    }
}
