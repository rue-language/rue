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
//! implemented — never silently wrong. With the gate off `compact_layout()` is
//! false, the rule never fires, and every classification is byte-for-byte the
//! historical slot decision.

use crate::{FrozenTypeInternPool, Type, TypeKind};

/// Which calling convention governs a boundary.
///
/// The seam ADR-0052 requires so the future guaranteed target C classifier
/// extends this authority instead of adding a peer path. Only the native
/// convention is implemented today.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallAbi {
    /// The preserved native Rue convention (RUE-106): a by-value aggregate is
    /// returned one flattened ABI slot per return register, or via sret when it
    /// does not fit; canonical `StrBuf` always returns via sret; a by-reference
    /// `inout` / `borrow` argument is one pointer slot; a by-value argument
    /// occupies one slot per leaf.
    Native,
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

    /// Classify how a by-value return of `ty` crosses the boundary.
    ///
    /// Reproduces the historical decision exactly: zero-sized returns nothing;
    /// canonical `StrBuf` and any aggregate whose flattened slot count exceeds
    /// the return-register budget use sret; a smaller aggregate returns in
    /// registers; everything else is a scalar. Under the compact layout a
    /// non-slot-identical aggregate is forced indirect (memory-first rule).
    pub fn classify_return(&self, ty: Type) -> ReturnClass {
        let slot_count = self.type_pool.abi_slot_count(ty);
        if slot_count == 0 {
            return ReturnClass::ZeroSized;
        }
        if self.return_uses_sret(ty, slot_count)
            || self.crosses_indirectly_under_compact(ty, slot_count)
        {
            return ReturnClass::Indirect { slot_count };
        }
        if self.is_multislot_aggregate(ty, slot_count) {
            ReturnClass::Registers { slot_count }
        } else {
            ReturnClass::Scalar
        }
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
            ArgConvention::ByReference => ArgClass::Indirect,
            ArgConvention::ByValue => {
                let slot_count = self.type_pool.abi_slot_count(ty);
                if slot_count == 0 {
                    ArgClass::Omitted
                } else if self.crosses_indirectly_under_compact(ty, slot_count) {
                    // Memory-first transitional rule: the preserved slot
                    // convention cannot pass this compact aggregate by value.
                    // Code generation refuses it loudly until the indirect
                    // marshaling lands; the authority still records the intent.
                    ArgClass::Indirect
                } else {
                    ArgClass::Direct { slot_count }
                }
            }
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

    /// The historical `type_uses_sret_return` predicate: strbuf, or a
    /// struct/array/payload-enum whose slot count exceeds the budget
    /// (oversized enums route through the same slot-count policy per RUE-946;
    /// discriminant-only enums are single-slot and stay scalar).
    fn return_uses_sret(&self, ty: Type, slot_count: u32) -> bool {
        match ty.kind() {
            TypeKind::Struct(struct_id) => {
                self.type_pool.is_strbuf(struct_id) || slot_count > self.ret_reg_budget
            }
            TypeKind::Array(_) | TypeKind::Enum(_) => slot_count > self.ret_reg_budget,
            _ => false,
        }
    }

    /// Whether `ty` needs a complete aggregate slot representation rather than a
    /// single primary vreg. Discriminant-only enums stay scalar.
    fn is_multislot_aggregate(&self, ty: Type, slot_count: u32) -> bool {
        matches!(ty.kind(), TypeKind::Struct(_) | TypeKind::Array(_))
            || (ty.is_enum() && slot_count > 1)
    }

    /// The memory-first transitional rule (ADR-0052 ruling 9): under the compact
    /// layout, an aggregate whose compact representation is not slot-identical
    /// cannot cross the preserved register convention and must go indirect —
    /// *unless it occupies exactly one ABI slot* (RUE-1035).
    ///
    /// A single-slot aggregate carries its whole value in one eight-byte slot: a
    /// narrow leaf lives in that slot's low bytes under both the compact image
    /// and the slot-based frame, so one register transports it losslessly and the
    /// callee's one-slot decomposition view is identical either way. Forcing such
    /// a value indirect gained nothing and drove the not-yet-correct single-slot
    /// indirect marshalling (garbage by-value args, RUE-1035 M1); classifying it
    /// Direct instead routes it through the proven one-slot path — byte-for-byte
    /// the same classification the gate-off slot model already produces for it, on
    /// both backends and in the oracle. Returns stay consistent: a single-slot
    /// aggregate return is `Registers { slot_count: 1 }` (never sret), so no
    /// caller-side image read-back is implied.
    ///
    /// Always false with the gate off (`compact_layout()` is false), so every
    /// classification is byte-for-byte the historical slot decision.
    fn crosses_indirectly_under_compact(&self, ty: Type, slot_count: u32) -> bool {
        self.type_pool.compact_layout()
            && slot_count > 1
            && self.is_multislot_aggregate(ty, slot_count)
            && !is_slot_identical_layout(self.type_pool, ty)
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

#[cfg(test)]
mod tests {
    use super::*;

    // Behavioral coverage that exercises the classifier against a real type
    // pool lives with the backend ABI/oracle suites (which own program
    // fixtures); these unit checks pin the pure classification algebra.

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
    fn arg_class_crossing_width_matches_the_slot_contract() {
        assert_eq!(ArgClass::Omitted.crossing_slots(), 0);
        assert_eq!(ArgClass::Direct { slot_count: 4 }.crossing_slots(), 4);
        // A by-reference argument and a transitional indirect aggregate both
        // cross as one pointer.
        assert_eq!(ArgClass::Indirect.crossing_slots(), 1);
    }
}
