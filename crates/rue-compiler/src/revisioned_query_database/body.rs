//! Per-body query ownership boundary.
//!
//! This facade preserves the established `revisioned_query_database` names
//! while keeping each body responsibility in one source owner. The children
//! share the database and its registered `rue-query` runtime; none is a peer
//! evaluator or compatibility path.

mod closure_nucleus;
mod durable_comptime_adapters;
mod provider_body;
mod revision_symbol_space;
mod transactions;

pub(crate) use closure_nucleus::*;
pub(crate) use durable_comptime_adapters::*;
pub(crate) use provider_body::*;
pub(in crate::revisioned_query_database) use revision_symbol_space::*;
pub(crate) use transactions::*;
