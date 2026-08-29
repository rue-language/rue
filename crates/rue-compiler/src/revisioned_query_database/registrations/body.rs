include!("body/body_analysis_bundles.rs");
include!("body/body_closure_publications.rs");
include!("body/body_closures.rs");
include!("body/body_inputs.rs");
include!("body/body_produced_anonymous.rs");
include!("body/body_reachability.rs");
include!("body/body_source_bases.rs");
include!("body/body_toolchain_demands.rs");
include!("body/body_transactions.rs");
include!("body/warning_body_reference_batches.rs");
include!("body/warning_body_references.rs");
include!("body/warning_call_head_projections.rs");

pub(super) use register_body_body_analysis_bundles;
pub(super) use register_body_body_closure_publications;
pub(super) use register_body_body_closures;
#[cfg(test)]
pub(super) use register_body_body_inputs;
pub(super) use register_body_body_produced_anonymous;
pub(super) use register_body_body_reachability;
pub(super) use register_body_body_source_bases;
pub(super) use register_body_body_toolchain_demands;
pub(super) use register_body_body_transactions;
pub(super) use register_body_warning_body_reference_batches;
pub(super) use register_body_warning_body_references;
pub(super) use register_body_warning_call_head_projections;
