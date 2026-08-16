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
//! [`CallAbi`] is the extensibility seam ADR-0052 requires. The preserved native
//! Rue convention is the first and only implementation; the guaranteed target C
//! ABI (RUE-742 / RUE-745, SysV / AAPCS) will add further variants to this
//! authority rather than embedding a peer FFI calculator inside a backend.
//!
//! ## Two planes, one policy kernel
//!
//! Two walkers consume this module because their lifetimes differ: the live
//! classifiers here walk the request-scoped [`FrozenTypeInternPool`], while the
//! stable query plane (`compiler.call-abi` in `rue-compiler`) walks its own
//! revision-stable type keys and canonical layout values and must not hold a
//! live pool. Both project per-type facts and then consult the same pure
//! kernel — [`NativeAbiTypeFacts`] for the native decision tree,
//! [`TargetCCallAbi`] plus [`CAbiScalarKind`] for the target-C thresholds and
//! extensions, [`native_return_register_budget`] and
//! [`TargetCAbiFlavor::for_arch`] for the per-target numbers — so the
//! classification policy itself has exactly one production home.
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

use crate::{FrozenTypeInternPool, Type, TypeKind};
use rue_target::Arch;

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

/// Which calling convention governs a boundary.
///
/// The seam ADR-0052 requires so the guaranteed target-C classifier extends this
/// authority instead of adding a peer path. ADR-0064 P2 (RUE-1056) fills the
/// second variant with [`TargetCCallAbi`] for scalars and pointers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallAbi {
    /// The preserved native Rue convention (RUE-106): a by-value aggregate is
    /// returned one flattened ABI slot per return register, or via sret when it
    /// does not fit; canonical `StrBuf` always returns via sret; a by-reference
    /// `inout` / `borrow` argument is one pointer slot; a by-value argument
    /// occupies one slot per leaf.
    Native,
    /// The guaranteed target-C convention (ADR-0064): SysV AMD64 or AAPCS64 per
    /// the target flavor. In P2 it governs scalar and pointer crossings at an
    /// `extern "C"` boundary — one integer register per scalar, with the psABI's
    /// narrow-integer extension and the 1-byte `_Bool` 0/1 contract. See
    /// [`TargetCCallAbi`].
    TargetC(TargetCAbiFlavor),
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

/// The native call-ABI classifier: the [`CallAbi::Native`] implementation.
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
    pub const fn abi(&self) -> CallAbi {
        CallAbi::Native
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
pub fn is_slot_identical_layout(type_pool: &FrozenTypeInternPool, ty: Type) -> bool {
    match ty.kind() {
        // Eight-byte leaves and the recovery scalar: identical in both models.
        TypeKind::I64
        | TypeKind::U64
        | TypeKind::PtrConst(_)
        | TypeKind::PtrMut(_)
        | TypeKind::Error => true,
        // Zero-sized and compile-time-only types have identical (zero) extent.
        TypeKind::Unit | TypeKind::Never | TypeKind::ComptimeType | TypeKind::Module(_) => true,
        // Narrow scalars: one/two/four bytes under the compact layout.
        TypeKind::I8
        | TypeKind::U8
        | TypeKind::Bool
        | TypeKind::I16
        | TypeKind::U16
        | TypeKind::I32
        | TypeKind::U32 => false,
        TypeKind::Struct(id) => type_pool
            .struct_def(id)
            .fields
            .iter()
            .all(|field| is_slot_identical_layout(type_pool, field.ty)),
        TypeKind::Array(id) => {
            let (element, _length) = type_pool.array_def(id);
            is_slot_identical_layout(type_pool, element)
        }
        // Enums narrow their tag (u8/u16/u32 vs an eight-byte slot).
        TypeKind::Enum(_) => false,
    }
}

// ============================================================================
// The guaranteed target-C classifier (ADR-0064 P2, RUE-1056)
// ============================================================================

/// Which platform psABI a [`CallAbi::TargetC`] boundary follows. The flavor is
/// selected from the compilation target's architecture: x86-64 uses SysV AMD64,
/// AArch64 uses AAPCS64. The two agree on the scalar operations P2 needs (one
/// integer register per scalar, narrow values extended to their canonical form),
/// and differ only in the details documented on the flavor's methods — details
/// P3/P4 will exercise (byval stack area, sret register + echo).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetCAbiFlavor {
    /// System V AMD64 psABI (x86-64 Linux/BSD/macOS).
    SysVAmd64,
    /// The ARM 64-bit Procedure Call Standard (AArch64 Linux/macOS).
    Aapcs64,
}

impl TargetCAbiFlavor {
    /// The psABI flavor a target architecture's C boundary follows. The single
    /// home of the architecture-to-flavor mapping, consulted by the export
    /// thunk, the import call lowering, and the stable query plane.
    pub const fn for_arch(arch: Arch) -> Self {
        match arch {
            Arch::X86_64 => Self::SysVAmd64,
            Arch::Aarch64 => Self::Aapcs64,
        }
    }
}

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
}

/// How a C-classifiable aggregate crosses a target-C boundary as an *argument*
/// (ADR-0064 P3, RUE-1057).
///
/// The classification is computed from the aggregate's byte size (and alignment
/// for the memory paths) by [`TargetCCallAbi::classify_aggregate_arg`]. In the
/// integer-only core every eightbyte classifies INTEGER (a field type that would
/// classify SSE cannot exist until RUE-714), so the two ≤16-byte psABIs coincide:
/// the struct packs into one or two general-purpose integer registers **in C
/// field order**. That C order is the register-packing audit's key finding — the
/// native slot model decomposes one slot per leaf and *reverses* multi-slot
/// values, which disagrees with C packing even for a two-field struct — so a
/// target-C aggregate is marshaled through its physical memory image (ascending
/// C order) rather than reusing the native decomposition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AggregateArgClass {
    /// Packed into `eightbytes` (1 or 2) consecutive integer argument registers,
    /// low eightbyte first, i.e. C field order. Covers SysV AMD64 INTEGER-class
    /// ≤16-byte structs and AAPCS64 ≤16-byte composites.
    IntegerRegisters {
        /// Number of eightbyte integer registers (1 or 2).
        eightbytes: u32,
    },
    /// SysV AMD64 MEMORY class (>16 bytes): the whole struct image is passed by
    /// value in the outgoing stack argument area — `size` bytes at `align`
    /// alignment — consuming no integer registers.
    ByValueStack {
        /// The struct's byte size.
        size: u32,
        /// The struct's alignment.
        align: u32,
    },
    /// AAPCS64 composite >16 bytes (AAPCS64 §6.8.2 B.4/C.12): passed **by
    /// reference to a caller-owned copy** — the caller copies the struct and
    /// passes the copy's address in one integer register. This is *not* the SysV
    /// byval-on-stack rule.
    ByReferenceCopy {
        /// The struct's byte size (the caller copy's size).
        size: u32,
        /// The struct's alignment (the caller copy's alignment).
        align: u32,
    },
}

/// How a C-classifiable aggregate crosses a target-C boundary as a *return value*
/// (ADR-0064 P3, RUE-1057). Computed by
/// [`TargetCCallAbi::classify_aggregate_return`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AggregateReturnClass {
    /// Returned in `eightbytes` (1 or 2) result registers — `rax:rdx` on SysV,
    /// `x0:x1` on AAPCS64 — low eightbyte first (C field order), ≤16 bytes.
    IntegerRegisters {
        /// Number of eightbyte result registers (1 or 2).
        eightbytes: u32,
    },
    /// Returned indirectly through caller-provided storage (sret), >16 bytes.
    /// SysV passes the hidden pointer in `rdi` and the callee **echoes it in
    /// `rax`**; AAPCS64 passes it in the dedicated `x8` and does not echo (see
    /// [`TargetCCallAbi::sret_pointer_echoed_in_result_register`] and
    /// [`TargetCCallAbi::sret_pointer_in_dedicated_register`]).
    Indirect {
        /// The struct's byte size (caller storage size).
        size: u32,
        /// The struct's alignment.
        align: u32,
    },
}

/// The guaranteed target-C call-ABI classifier: the [`CallAbi::TargetC`]
/// implementation for scalars and pointers (ADR-0064 P2, RUE-1056).
///
/// This is the single authority both backends' `extern "C"` call lowering and
/// the oracle consult, so no backend makes a local target-C ABI decision. In P2
/// its scope is register-only scalar crossings; P3 extends the same authority
/// with eightbyte aggregate classification and byval stack arguments, and P5
/// with the SSE/FP register class once RUE-714 lands floats.
///
/// ## What P2 fixes, and why scalars need only the return re-extension
///
/// Every supported scalar (`c_passable_by_value`: the full integer set, `bool`,
/// pointers) occupies exactly one general-purpose integer register — the first
/// six on SysV (`rdi, rsi, rdx, rcx, r8, r9`), the first eight on AAPCS64
/// (`x0..x7`) — and P2 stays within that budget (no stack arguments). Rue's
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
///
/// ## `_Bool`
///
/// C `_Bool` is one byte whose only valid values are 0 and 1. Passing Rue's
/// `bool` (a 0/1 word) satisfies that directly; a `_Bool` return is
/// zero-extended from its low byte, materializing exactly 0/1
/// ([`ScalarAbiExtension::Unsigned`] `{ from_bits: 8 }`).
#[derive(Debug, Clone, Copy)]
pub struct TargetCCallAbi {
    flavor: TargetCAbiFlavor,
}

impl TargetCCallAbi {
    /// Build the classifier for a given psABI flavor.
    pub const fn new(flavor: TargetCAbiFlavor) -> Self {
        Self { flavor }
    }

    /// The SysV AMD64 classifier (x86-64).
    pub const fn sysv_amd64() -> Self {
        Self::new(TargetCAbiFlavor::SysVAmd64)
    }

    /// The AAPCS64 classifier (AArch64).
    pub const fn aapcs64() -> Self {
        Self::new(TargetCAbiFlavor::Aapcs64)
    }

    /// The convention this classifier implements.
    pub const fn abi(&self) -> CallAbi {
        CallAbi::TargetC(self.flavor)
    }

    /// The psABI flavor this classifier follows.
    pub const fn flavor(&self) -> TargetCAbiFlavor {
        self.flavor
    }

    /// The number of general-purpose integer registers used for arguments
    /// before the byval stack area (P3) begins: 6 on SysV (`rdi..r9`), 8 on
    /// AAPCS64 (`x0..x7`). P2 stays within this budget.
    pub const fn int_arg_register_budget(&self) -> u32 {
        match self.flavor {
            TargetCAbiFlavor::SysVAmd64 => 6,
            TargetCAbiFlavor::Aapcs64 => 8,
        }
    }

    /// Whether the callee echoes the hidden sret pointer back in the primary
    /// return register: SysV requires `rax` to hold the sret pointer on return;
    /// AAPCS64 uses the dedicated indirect-result register `x8`, which is **not**
    /// echoed. Reachable from P3: an aggregate return >16 bytes takes the sret
    /// path; scalars in P2 never do.
    pub const fn sret_pointer_echoed_in_result_register(&self) -> bool {
        match self.flavor {
            TargetCAbiFlavor::SysVAmd64 => true,
            TargetCAbiFlavor::Aapcs64 => false,
        }
    }

    /// Whether the hidden sret pointer is passed in a **dedicated** indirect-
    /// result register rather than the first ordinary integer argument register.
    /// AAPCS64 uses the dedicated `x8` (§6.9), so the sret pointer does not
    /// consume `x0` and the ordinary arguments still start at `x0`. SysV AMD64
    /// passes the sret pointer as the hidden first argument in `rdi`, consuming
    /// the first integer argument register. P3 aggregate returns exercise both.
    pub const fn sret_pointer_in_dedicated_register(&self) -> bool {
        match self.flavor {
            TargetCAbiFlavor::SysVAmd64 => false,
            TargetCAbiFlavor::Aapcs64 => true,
        }
    }

    /// The maximum aggregate size, in bytes, that a target-C boundary passes or
    /// returns in integer registers rather than in memory. Both psABIs use two
    /// eightbytes (16 bytes): a larger INTEGER-class aggregate goes to memory
    /// (SysV MEMORY class / AAPCS64 by-reference).
    pub const fn max_aggregate_register_bytes(&self) -> u64 {
        16
    }

    /// Number of eightbytes a `size`-byte aggregate occupies (ceil(size / 8)),
    /// saturating at `u32::MAX` like every byte-count projection here; sema
    /// rejects layouts anywhere near that bound before they reach a boundary.
    const fn eightbytes(size: u64) -> u32 {
        let eightbytes = size.div_ceil(8);
        if eightbytes > u32::MAX as u64 {
            u32::MAX
        } else {
            eightbytes as u32
        }
    }

    /// Saturating byte-count projection for the u32 fields of the memory
    /// classes.
    const fn saturate_u32(value: u64) -> u32 {
        if value > u32::MAX as u64 {
            u32::MAX
        } else {
            value as u32
        }
    }

    /// Classify how a C-classifiable aggregate of `size` bytes at `align`
    /// alignment crosses as an *argument* under this psABI (ADR-0064 P3).
    ///
    /// Integer-only core: every eightbyte classifies INTEGER (SSE is unreachable
    /// until RUE-714), so a ≤16-byte aggregate packs into one or two integer
    /// registers in C field order on both psABIs. A larger aggregate diverges:
    /// SysV MEMORY class is byval-on-stack, AAPCS64 passes a pointer to a
    /// caller-owned copy. `size` is the aggregate's `@size_of`; `align` its
    /// `@align_of` — the caller has already gated the type through
    /// [`c_passable_by_value`](crate::c_passable_by_value), so `size >= 1`.
    pub fn classify_aggregate_arg(&self, size: u64, align: u64) -> AggregateArgClass {
        if size <= self.max_aggregate_register_bytes() {
            return AggregateArgClass::IntegerRegisters {
                eightbytes: Self::eightbytes(size),
            };
        }
        let size = Self::saturate_u32(size);
        let align = Self::saturate_u32(align);
        match self.flavor {
            TargetCAbiFlavor::SysVAmd64 => AggregateArgClass::ByValueStack { size, align },
            TargetCAbiFlavor::Aapcs64 => AggregateArgClass::ByReferenceCopy { size, align },
        }
    }

    /// Classify how a C-classifiable aggregate of `size` bytes at `align`
    /// alignment crosses as a *return value* under this psABI (ADR-0064 P3).
    ///
    /// ≤16 bytes returns in one or two result registers (`rax:rdx` / `x0:x1`) in
    /// C field order; a larger aggregate returns indirectly through caller
    /// storage (sret) on both psABIs, differing only in the sret-pointer register
    /// and echo ([`Self::sret_pointer_in_dedicated_register`],
    /// [`Self::sret_pointer_echoed_in_result_register`]).
    pub fn classify_aggregate_return(&self, size: u64, align: u64) -> AggregateReturnClass {
        if size <= self.max_aggregate_register_bytes() {
            AggregateReturnClass::IntegerRegisters {
                eightbytes: Self::eightbytes(size),
            }
        } else {
            AggregateReturnClass::Indirect {
                size: Self::saturate_u32(size),
                align: Self::saturate_u32(align),
            }
        }
    }

    /// The stack alignment required at a `call` instruction on this psABI: 16
    /// bytes on both SysV AMD64 and AAPCS64.
    pub const fn call_stack_alignment(&self) -> u32 {
        16
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
        let kind = match ty.kind() {
            TypeKind::I8 => CAbiScalarKind::I8,
            TypeKind::I16 => CAbiScalarKind::I16,
            TypeKind::I32 => CAbiScalarKind::I32,
            TypeKind::U8 => CAbiScalarKind::U8,
            TypeKind::U16 => CAbiScalarKind::U16,
            TypeKind::U32 => CAbiScalarKind::U32,
            TypeKind::Bool => CAbiScalarKind::Bool,
            TypeKind::I64
            | TypeKind::U64
            | TypeKind::PtrConst(_)
            | TypeKind::PtrMut(_)
            | TypeKind::Error => CAbiScalarKind::RegisterWidth,
            other => panic!(
                "TargetCCallAbi scalar classification called on non-scalar type {other:?}; \
                 aggregates (P3) and unsupported types are gated by c_passable_by_value \
                 before lowering"
            ),
        };
        kind.extension()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Behavioral coverage that exercises the classifier against a real type
    // pool lives with the backend ABI/oracle suites (which own program
    // fixtures); these unit checks pin the pure classification algebra.

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
        assert_eq!(sysv.abi(), CallAbi::TargetC(TargetCAbiFlavor::SysVAmd64));
        assert_eq!(aapcs.abi(), CallAbi::TargetC(TargetCAbiFlavor::Aapcs64));
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
    fn aggregate_arg_classification_packs_by_size_in_c_order() {
        let sysv = TargetCCallAbi::sysv_amd64();
        let aapcs = TargetCCallAbi::aapcs64();
        // A two-`i32` struct is 8 bytes: ONE eightbyte register on both psABIs.
        // (Native would decompose it to two reversed slots — the packing the
        // audit warns disagrees; the target-C classifier packs one eightbyte.)
        assert_eq!(
            sysv.classify_aggregate_arg(8, 4),
            AggregateArgClass::IntegerRegisters { eightbytes: 1 }
        );
        assert_eq!(
            aapcs.classify_aggregate_arg(8, 4),
            AggregateArgClass::IntegerRegisters { eightbytes: 1 }
        );
        // A 16-byte struct packs into two eightbyte registers on both.
        assert_eq!(
            sysv.classify_aggregate_arg(16, 8),
            AggregateArgClass::IntegerRegisters { eightbytes: 2 }
        );
        assert_eq!(
            aapcs.classify_aggregate_arg(16, 8),
            AggregateArgClass::IntegerRegisters { eightbytes: 2 }
        );
        // >16 bytes diverges: SysV byval-on-stack, AAPCS64 by-reference copy.
        assert_eq!(
            sysv.classify_aggregate_arg(24, 8),
            AggregateArgClass::ByValueStack { size: 24, align: 8 }
        );
        assert_eq!(
            aapcs.classify_aggregate_arg(24, 8),
            AggregateArgClass::ByReferenceCopy { size: 24, align: 8 }
        );
        // A non-multiple-of-8 size rounds up to whole eightbytes.
        assert_eq!(
            sysv.classify_aggregate_arg(12, 4),
            AggregateArgClass::IntegerRegisters { eightbytes: 2 }
        );
    }

    #[test]
    fn aggregate_return_classification_registers_then_sret() {
        let sysv = TargetCCallAbi::sysv_amd64();
        let aapcs = TargetCCallAbi::aapcs64();
        assert_eq!(
            sysv.classify_aggregate_return(8, 4),
            AggregateReturnClass::IntegerRegisters { eightbytes: 1 }
        );
        assert_eq!(
            sysv.classify_aggregate_return(16, 8),
            AggregateReturnClass::IntegerRegisters { eightbytes: 2 }
        );
        // >16 bytes returns via sret on both psABIs.
        assert_eq!(
            sysv.classify_aggregate_return(24, 8),
            AggregateReturnClass::Indirect { size: 24, align: 8 }
        );
        assert_eq!(
            aapcs.classify_aggregate_return(24, 8),
            AggregateReturnClass::Indirect { size: 24, align: 8 }
        );
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
        assert_eq!(sysv.max_aggregate_register_bytes(), 16);
        assert_eq!(aapcs.max_aggregate_register_bytes(), 16);
    }

    #[test]
    fn per_arch_budget_and_flavor_have_one_home() {
        assert_eq!(native_return_register_budget(Arch::X86_64), 6);
        assert_eq!(native_return_register_budget(Arch::Aarch64), 8);
        assert_eq!(
            TargetCAbiFlavor::for_arch(Arch::X86_64),
            TargetCAbiFlavor::SysVAmd64
        );
        assert_eq!(
            TargetCAbiFlavor::for_arch(Arch::Aarch64),
            TargetCAbiFlavor::Aapcs64
        );
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
    fn oversized_aggregate_byte_counts_saturate() {
        let sysv = TargetCCallAbi::sysv_amd64();
        let huge = u64::from(u32::MAX) + 9;
        assert_eq!(
            sysv.classify_aggregate_arg(huge, 8),
            AggregateArgClass::ByValueStack {
                size: u32::MAX,
                align: 8
            }
        );
        assert_eq!(
            sysv.classify_aggregate_return(huge, 8),
            AggregateReturnClass::Indirect {
                size: u32::MAX,
                align: 8
            }
        );
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
