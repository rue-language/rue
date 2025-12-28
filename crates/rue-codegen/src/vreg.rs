//! Shared virtual register types for code generation backends.
//!
//! Virtual registers are target-independent - they represent values before
//! physical register allocation. Both x86_64 and aarch64 backends use the
//! same VReg type, with target-specific physical registers assigned later.

use std::fmt;

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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
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
