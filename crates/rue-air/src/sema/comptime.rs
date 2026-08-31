//! Host-generic canonical compile-time evaluator.
//!
//! The module tree separates the evaluator's semantic model, program registry
//! and completed-call memo, intrinsic decoding, host capabilities, structured
//! type continuation protocol, and recursive execution core. `execution` is
//! the only recursive RIR dispatcher; hosts provide facts and effects without
//! walking child instructions or owning another evaluator.

use ahash::{AHashMap, AHashSet};
use rue_rir::{
    InstData, InstRef, RepeatCount, Rir, RirIntrinsicArgsRange, SymbolHandle, ValidatedRir,
};
use rue_span::Span;
use std::hash::Hash;
use std::sync::Arc;

use crate::integer_semantics::{CheckedIntegerResult, IntegerType};

mod execution;
mod frames;
mod host;
mod intrinsics;
mod model;
mod registry;
mod sites;
mod structured_type;

pub use execution::*;
pub use frames::*;
pub use host::*;
pub use intrinsics::*;
pub use model::*;
pub use registry::*;
pub use sites::*;
pub use structured_type::*;

#[cfg(test)]
mod value_domain_tests;

/// Production plus the value-domain tests, whose fake host several guards
/// count overrides in.
#[cfg(test)]
pub(crate) const COMPTIME_SOURCE: &str = concat!(
    include_str!("comptime/execution.rs"),
    include_str!("comptime/frames.rs"),
    include_str!("comptime/host.rs"),
    include_str!("comptime/intrinsics.rs"),
    include_str!("comptime/model.rs"),
    include_str!("comptime/registry.rs"),
    include_str!("comptime/sites.rs"),
    include_str!("comptime/structured_type.rs"),
    include_str!("comptime.rs"),
    "\n#[cfg(test)]\nmod value_domain_tests {\n",
    include_str!("comptime/value_domain_tests.rs"),
    "\n}\n",
);

/// Maximum propagated comptime call depth. Depth zero is the root call, and
/// expression recursion does not spend this budget.
pub const MAX_COMPTIME_CALL_DEPTH: usize = 64;

/// Convert the number of active ancestor calls into the propagated depth of a
/// child call. Root evaluation is depth zero, so its first child is depth one.
#[inline]
pub const fn next_comptime_depth(active_ancestors: usize) -> usize {
    active_ancestors.saturating_add(1)
}

/// Return whether a propagated comptime call depth is beyond the language
/// limit. Depth zero denotes the root call, so the normative boundary itself
/// remains admissible and only the next frame is rejected.
#[inline]
pub const fn comptime_depth_over_limit(depth: usize) -> bool {
    depth > MAX_COMPTIME_CALL_DEPTH
}
