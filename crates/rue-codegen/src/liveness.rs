//! Shared liveness analysis types for register allocation.
//!
//! This module re-exports liveness types from [`crate::regalloc`] for convenience.
//! The types are defined in the regalloc module because liveness analysis is
//! fundamentally part of the register allocation pipeline.
//!
//! ## Provided Types
//!
//! - [`LiveRange`]: Represents the instruction range where a vreg's value is needed
//! - [`LivenessInfo`]: Holds all liveness information (ranges, live_at, clobbers)
//!
//! Each backend implements its own `analyze()` function that populates these types
//! based on its specific instruction set and control flow.

// Re-export types from regalloc module
pub use crate::regalloc::{LiveRange, LivenessInfo};
