//! Register classes: the general-purpose / floating-point dimension.
//!
//! A physical register file is not one flat pool. An integer value can live in
//! `rax` or `x19` but never in `xmm0` or `d8`, and the reverse holds for a
//! floating-point value; the two halves have separate move instructions,
//! separate call-clobber rules, and separate spill/reload forms. Every pass
//! that reasons about "which register can hold this value" therefore needs to
//! know which half of the file a value belongs to.
//!
//! Both partitions are populated: `f32`/`f64` values live in [`RegClass::Fp`]
//! (x86-64 XMM, AArch64 V) and everything else in [`RegClass::Gp`]. The
//! dimension was introduced on its own first, so that adding floats became a
//! matter of *populating* the `Fp` partition rather than of widening
//! single-class data structures under a working allocator (RUE-1067, first step
//! of RUE-1067..1076).
//!
//! A value's class follows its own type, and an aggregate SLOT's class follows
//! that slot's LEAF type — not the aggregate's. `struct P { field: f64 }` is one
//! ABI slot in the `Fp` partition even though `P` is not a float type; asking
//! the wrapper instead of the leaf is what put an FP-classed slot in a
//! general-purpose register. See
//! [`crate::value_plan::primary_slot_float_width`].
//!
//! ## What the class dimension is for
//!
//! * [`crate::liveness`] records a class for every virtual register and carries
//!   it into [`crate::regalloc::LivenessInfo`], which makes it the one
//!   authoritative class table allocation reads.
//! * [`crate::regalloc::coalesce`] refuses to merge two virtual registers of
//!   different classes: a `mov` between an integer and a floating-point
//!   register is not a plain register-to-register move, so such a pair is not a
//!   coalescing candidate no matter how its live ranges sit.
//! * [`crate::regalloc`] allocation keeps its scan state per class and offers
//!   an interval only the registers of its own class, while the spill-slot
//!   allocator and the callee-saved save set stay shared — there is one stack
//!   frame and one prologue no matter how many register classes a target has.
//! * [`crate::schedule_core`] partitions its physical-register dependency
//!   bookkeeping by class, so a backend whose floating-point registers are
//!   numbered independently of its integer ones cannot alias the two.

use std::fmt;

use crate::vreg::VReg;

/// The class of a register: which half of the physical register file a value
/// can live in.
///
/// The variants are ordered and their discriminants are the canonical
/// per-class array index ([`RegClass::index`]); [`RegClass::ALL`] lists them in
/// that order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum RegClass {
    /// General-purpose (integer, pointer, and boolean) registers.
    Gp = 0,
    /// Floating-point / SIMD registers: x86-64 `xmm0..15`, AArch64 `v0..31`.
    /// Selected by every `f32`/`f64` value and by every aggregate slot whose
    /// leaf is one.
    Fp = 1,
}

impl RegClass {
    /// Number of register classes.
    pub const COUNT: usize = 2;

    /// Every register class, in [`RegClass::index`] order.
    pub const ALL: [RegClass; Self::COUNT] = [RegClass::Gp, RegClass::Fp];

    /// Index of this class in a per-class array.
    #[inline]
    pub const fn index(self) -> usize {
        self as usize
    }

    /// Short lowercase name, used in diagnostics and panic messages.
    #[inline]
    pub const fn name(self) -> &'static str {
        match self {
            RegClass::Gp => "gp",
            RegClass::Fp => "fp",
        }
    }
}

impl fmt::Display for RegClass {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// The register class of every virtual register in one function's MIR.
///
/// Dense, indexed by [`VReg::index`], and grown one entry per minted virtual
/// register: each backend's `alloc_vreg` pushes the class of the register it
/// hands out, so `len()` equals the MIR's `vreg_count()` by construction. That
/// invariant is what lets allocation index this table with any vreg the
/// liveness ranges mention.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VRegClasses {
    classes: Vec<RegClass>,
}

impl VRegClasses {
    /// An empty table, for a MIR with no virtual registers yet.
    pub const fn new() -> Self {
        Self {
            classes: Vec::new(),
        }
    }

    /// A table in which every one of `count` virtual registers is
    /// general-purpose.
    ///
    /// This is what the target-independent `linear_scan*` entry points and
    /// hand-constructed test liveness use: they name a register set directly
    /// rather than a target's register file, and a bare register set has no
    /// class structure to read.
    pub fn all_gp(count: u32) -> Self {
        Self {
            classes: vec![RegClass::Gp; count as usize],
        }
    }

    /// Record the class of the next virtual register.
    ///
    /// Callers must push exactly once per minted register, in mint order, or
    /// the table stops agreeing with `vreg_count()`.
    #[inline]
    pub fn push(&mut self, class: RegClass) {
        self.classes.push(class);
    }

    /// Number of virtual registers with a recorded class.
    #[inline]
    pub fn len(&self) -> u32 {
        self.classes.len() as u32
    }

    /// Whether no virtual register has a recorded class.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.classes.is_empty()
    }

    /// The class of `vreg`.
    ///
    /// A virtual register past the end of the table answers [`RegClass::Gp`].
    /// That case does not arise in a MIR built through `alloc_vreg`, and
    /// [`crate::regalloc::RegAllocDriver`] asserts the table covers the MIR
    /// before allocation reads it; the saturating answer only keeps a
    /// hand-built MIR behaving exactly as it did before classes existed
    /// instead of panicking mid-codegen.
    #[inline]
    pub fn class_of(&self, vreg: VReg) -> RegClass {
        self.classes
            .get(vreg.index() as usize)
            .copied()
            .unwrap_or(RegClass::Gp)
    }

    /// How many virtual registers are in `class`.
    pub fn count_in(&self, class: RegClass) -> u32 {
        self.classes.iter().filter(|&&c| c == class).count() as u32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn class_indices_are_the_canonical_array_order() {
        for (index, class) in RegClass::ALL.iter().enumerate() {
            assert_eq!(class.index(), index);
        }
        assert_eq!(RegClass::ALL.len(), RegClass::COUNT);
    }

    #[test]
    fn an_all_gp_table_covers_every_vreg_and_holds_no_fp() {
        let classes = VRegClasses::all_gp(4);
        assert_eq!(classes.len(), 4);
        assert_eq!(classes.count_in(RegClass::Gp), 4);
        assert_eq!(classes.count_in(RegClass::Fp), 0);
        for idx in 0..4 {
            assert_eq!(classes.class_of(VReg::new(idx)), RegClass::Gp);
        }
    }

    #[test]
    fn pushing_records_classes_in_mint_order() {
        let mut classes = VRegClasses::new();
        assert!(classes.is_empty());
        classes.push(RegClass::Gp);
        classes.push(RegClass::Fp);
        classes.push(RegClass::Gp);

        assert_eq!(classes.len(), 3);
        assert_eq!(classes.class_of(VReg::new(0)), RegClass::Gp);
        assert_eq!(classes.class_of(VReg::new(1)), RegClass::Fp);
        assert_eq!(classes.class_of(VReg::new(2)), RegClass::Gp);
        assert_eq!(classes.count_in(RegClass::Gp), 2);
        assert_eq!(classes.count_in(RegClass::Fp), 1);
    }

    #[test]
    fn a_vreg_past_the_table_reads_as_general_purpose() {
        let classes = VRegClasses::all_gp(1);
        assert_eq!(classes.class_of(VReg::new(7)), RegClass::Gp);
    }
}
