//! Semantic work accounting for canonical pipeline requests.

use rue_air::{
    BodyAnalysisWork, DeclarationBindingWork, RirDeclarationIndexWork, SemanticBindingManifestWork,
};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
/// Structural work from one canonical semantic request.
pub struct CanonicalSemanticWork {
    /// One request-local RIR declaration-index construction.
    pub declaration_index: RirDeclarationIndexWork,
    /// Completed declaration binding, independent of optional manifest work.
    pub binding: DeclarationBindingWork,
    /// Authoritative binding-manifest traversal used to validate body tokens.
    pub manifest: SemanticBindingManifestWork,
    /// Demand-driven function-body analysis work.
    pub body_analysis: BodyAnalysisWork,
    /// Drop-glue, CFG construction, and optimization work.
    pub cfg: CfgConstructionWork,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CfgConstructionWork {
    pub drop_glue_functions_synthesized: usize,
    pub functions_considered: usize,
    pub comptime_functions_filtered: usize,
    pub materialization_index_builds: usize,
    pub materialization_declarations_scanned: usize,
    pub materialization_anonymous_nominals_scanned: usize,
    pub materialization_type_nodes_scanned: usize,
    pub materialization_fact_selections: usize,
    /// Distinct fact closures allocated, and selections served from one an
    /// earlier body already built. These sum to `materialization_fact_selections`.
    pub materialization_fact_closures_allocated: usize,
    pub materialization_fact_closures_reused: usize,
    pub materialization_declarations_selected: usize,
    pub materialization_anonymous_nominals_selected: usize,
    pub materialization_callables_selected: usize,
    pub materialization_nominal_metadata_selected: usize,
    pub materialization_modules_selected: usize,
    pub materialization_builtin_nominals_selected: usize,
    pub materialization_required_types_selected: usize,
    pub prerequisite_stable_types_scanned: usize,
    pub prerequisite_layout_requests: usize,
    pub prerequisite_drop_glue_requests: usize,
    pub retained_interner_charge_scans: usize,
    pub retained_interner_entries_scanned: usize,
    pub retained_interner_utf8_bytes_scanned: usize,
    pub local_epochs: usize,
    pub local_air_instructions: usize,
    pub local_air_payload_bytes: usize,
    pub local_type_entries: usize,
    pub local_aggregate_type_aliases: usize,
    pub local_materialized_type_handles: usize,
    pub local_interner_entries: usize,
    pub local_interner_utf8_bytes: usize,
    pub local_strings: usize,
    pub local_atoms: usize,
    pub cfg_builds_attempted: usize,
    pub cfg_builds_succeeded: usize,
    pub cfg_builds_failed: usize,
    pub air_instructions_consumed: usize,
    pub optimization_attempts: usize,
    pub optimization_completions: usize,
    pub optimized_level_attempts: usize,
    pub optimization_loops_analyzed: usize,
    pub optimization_loops_unrolled: usize,
    pub optimization_budget_refusals: usize,
    pub optimization_inline_budget_refusals: usize,
    pub optimization_inline_importability_refusals: usize,
    pub optimization_inline_importability_checks: usize,
    pub optimization_inline_import_attempts: usize,
    pub optimization_inline_interner_stages: usize,
    pub optimization_inline_growth_preflights: usize,
    /// Total values charged by the shared O3 growth budget, including both
    /// local unrolling and accepted general inlining.
    pub optimization_code_growth_used: usize,
    /// Total basic blocks charged by the shared O3 growth budget.
    pub optimization_code_growth_blocks_used: usize,
    pub optimization_inline_code_growth_used: usize,
    pub optimization_inline_code_growth_blocks_used: usize,
    pub optimization_reoptimization_attempts: usize,
    pub optimization_reoptimization_completions: usize,
    pub optimization_reoptimization_code_growth_used: usize,
    pub optimization_reoptimization_code_growth_blocks_used: usize,
    pub cfg_warnings_emitted: usize,
    pub cfg_reuse_candidates: usize,
    pub cfg_reuses: usize,
}
