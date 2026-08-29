//! Shared virtual register types for code generation backends.
//!
//! Virtual registers are target-independent - they represent values before
//! physical register allocation. Both x86_64 and aarch64 backends use the
//! same VReg type, with target-specific physical registers assigned later.

use std::fmt;

use ahash::AHashMap;

use crate::reg_class::{RegClass, VRegClasses};

/// Base ID for block labels in the partitioned label ID space.
///
/// During codegen, we need labels for two purposes:
/// - **Inline labels** (IDs `0` to `BLOCK_LABEL_BASE - 1`): Generated during
///   instruction lowering for overflow checks, bounds checks, etc.
/// - **Block labels** (IDs `BLOCK_LABEL_BASE` to `u32::MAX`): Each CFG basic
///   block gets a label computed as `BLOCK_LABEL_BASE + block_id`.
///
/// This partitioning gives each namespace ~2 billion IDs, which is more than
/// sufficient for any realistic function.
pub const BLOCK_LABEL_BASE: u32 = u32::MAX / 2;

use crate::index_map::Handle;

/// A virtual register.
///
/// Virtual registers are unlimited and allocated to physical registers
/// during register allocation. They are target-independent; the mapping
/// to physical registers happens in each backend's register allocator.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct VReg(u32);

impl VReg {
    /// Create a new virtual register with the given index.
    #[inline]
    pub const fn new(index: u32) -> Self {
        Self(index)
    }

    /// Get the index of this virtual register.
    #[inline]
    pub const fn index(self) -> u32 {
        self.0
    }
}

impl fmt::Display for VReg {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "v{}", self.0)
    }
}

impl Handle for VReg {
    fn index(self) -> u32 {
        self.0
    }

    fn from_index(index: u32) -> Self {
        Self(index)
    }
}

/// A label identifier.
///
/// Labels are local to a function and are represented as a lightweight u32 index
/// rather than as heap-allocated strings. This avoids allocations during codegen.
/// Labels are target-independent; each backend emits them in its own format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LabelId(u32);

impl LabelId {
    /// Create a new label with the given index.
    #[inline]
    pub const fn new(index: u32) -> Self {
        Self(index)
    }

    /// Get the index of this label.
    #[inline]
    pub const fn index(self) -> u32 {
        self.0
    }
}

impl fmt::Display for LabelId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, ".L{}", self.0)
    }
}

/// Target-independent mutable state shared by both MIR implementations.
///
/// Virtual-register numbering and callable-symbol interning are properties of
/// a MIR function, rather than of an instruction set. Keeping their authority
/// here makes the x86-64 and AArch64 adapters preserve the same ordering and
/// class-table invariants without maintaining parallel implementations.
#[derive(Debug, Default)]
pub struct MirState {
    next_vreg: u32,
    vreg_classes: VRegClasses,
    symbols: Vec<String>,
    symbol_index: AHashMap<String, u32>,
}

impl MirState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn intern_symbol(&mut self, symbol: &str) -> u32 {
        if let Some(&idx) = self.symbol_index.get(symbol) {
            return idx;
        }
        let idx = self.symbols.len() as u32;
        let owned = symbol.to_owned();
        self.symbol_index.insert(owned.clone(), idx);
        self.symbols.push(owned);
        idx
    }

    #[inline]
    pub fn get_symbol(&self, symbol_id: u32) -> &str {
        &self.symbols[symbol_id as usize]
    }

    #[inline]
    pub fn symbols(&self) -> &[String] {
        &self.symbols
    }

    pub fn take_symbols(&mut self) -> Vec<String> {
        self.symbol_index.clear();
        std::mem::take(&mut self.symbols)
    }

    pub fn set_symbols(&mut self, symbols: Vec<String>) {
        self.symbol_index.clear();
        for (idx, sym) in symbols.iter().enumerate() {
            self.symbol_index.insert(sym.clone(), idx as u32);
        }
        self.symbols = symbols;
    }

    #[inline]
    pub fn alloc_vreg(&mut self, class: RegClass) -> VReg {
        let vreg = VReg::new(self.next_vreg);
        self.next_vreg += 1;
        self.vreg_classes.push(class);
        vreg
    }

    #[inline]
    pub fn vreg_classes(&self) -> &VRegClasses {
        &self.vreg_classes
    }

    #[inline]
    pub fn vreg_class(&self, vreg: VReg) -> RegClass {
        self.vreg_classes.class_of(vreg)
    }

    #[inline]
    pub fn vreg_count(&self) -> u32 {
        self.next_vreg
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_state_preserves_symbol_ids_and_vreg_classes() {
        let mut state = MirState::new();
        assert_eq!(state.intern_symbol("alpha"), 0);
        assert_eq!(state.intern_symbol("beta"), 1);
        assert_eq!(state.intern_symbol("alpha"), 0);

        let gp = state.alloc_vreg(RegClass::Gp);
        let fp = state.alloc_vreg(RegClass::Fp);
        assert_eq!(gp.index(), 0);
        assert_eq!(fp.index(), 1);
        assert_eq!(state.vreg_count(), 2);
        assert_eq!(state.vreg_class(gp), RegClass::Gp);
        assert_eq!(state.vreg_class(fp), RegClass::Fp);
        assert_eq!(state.vreg_classes().len(), 2);
    }

    #[test]
    fn shared_state_symbol_transfer_rebuilds_lookup() {
        let mut state = MirState::new();
        assert_eq!(state.intern_symbol("before"), 0);
        let symbols = state.take_symbols();
        assert_eq!(symbols, vec!["before"]);
        state.set_symbols(vec!["restored".into(), "other".into()]);
        assert_eq!(state.intern_symbol("restored"), 0);
        assert_eq!(state.intern_symbol("new"), 2);
    }
}
