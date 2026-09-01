//! Semantic work accounting for canonical pipeline requests.

use rue_air::{
    BodyAnalysisWork, DeclarationBindingWork, RirDeclarationIndexWork, SemanticBindingManifestWork,
};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
/// Structural work from one canonical semantic request.
pub struct CanonicalSemanticWork {
    /// Candidate declaration body-plan construction performed by the
    /// registered `declaration-body-plan-artifacts` query.
    pub candidate_body_plan_construction: CandidateBodyPlanWork,
    /// Candidate plan materialization performed by body transactions. This is
    /// the remap/validation/index and AIR-facing instruction/payload boundary.
    pub candidate_body_plan_materialization: CandidateBodyPlanWork,
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

/// Deterministic lifecycle and output counts for one query-native structural
/// work category. Only successfully published terminals contribute output
/// counts; reused counts describe successful retained terminals returned by a
/// request and never include cancellation or deterministic failures.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CandidateBodyPlanWork {
    pub computed: usize,
    pub reused: usize,
    pub instructions_produced: usize,
    pub payload_words_produced: usize,
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
    /// Additive bounded-work totals from successful local optimization and
    /// changed-caller reoptimization runs.
    pub optimization_passes: CfgOptimizationWork,
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

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CfgOptimizationWork {
    pub constopt_fold_attempts: usize,
    pub constopt_folded: usize,
    pub constopt_loads_rewritten: usize,
    pub peephole_divmods_reduced: usize,
    pub peephole_identities_rewired: usize,
    pub simplify_blocks_scanned: usize,
    pub simplify_branches_folded: usize,
    pub simplify_switches_folded: usize,
    pub simplify_edges_threaded: usize,
    pub simplify_forwarders_resolved: usize,
    pub simplify_blocks_merged: usize,
    pub forward_insts_scanned: usize,
    pub forward_loads_single_write: usize,
    pub forward_loads_block_local: usize,
    pub forward_rule1_dominance_pairs_checked: usize,
    pub forward_dominator_computations: usize,
    pub cse_insts_scanned: usize,
    pub cse_duplicates_replaced: usize,
    /// Sum of the per-run value-number table high-water marks.
    pub cse_max_table_entries_sum: usize,
    pub cse_dominator_computations: usize,
    pub preheader_normalization_forest_computations: usize,
    pub preheader_normalization_loops_examined: usize,
    pub preheader_normalization_preheaders_materialized: usize,
    pub preheader_normalization_verifier_dominator_computations: usize,
    pub licm_forest_computations: usize,
    pub licm_def_block_scans: usize,
    pub licm_loops_analyzed: usize,
    pub licm_instructions_examined: usize,
    pub licm_slot_fact_instructions_scanned: usize,
    pub licm_slot_fact_entries_initialized: usize,
    pub licm_slot_fact_workspace_growths: usize,
    pub licm_candidate_dependencies: usize,
    pub licm_worklist_pops: usize,
    pub licm_invariants_hoisted: usize,
    pub licm_hoist_workspace_growths: usize,
    pub unroll_forest_computations: usize,
    pub unroll_loops_analyzed: usize,
    pub unroll_loops_unrolled: usize,
    pub unroll_budget_refusals: usize,
    pub unroll_shape_refusals: usize,
    pub unroll_blocks_cloned: usize,
    pub unroll_values_cloned: usize,
    pub unroll_instructions_cloned: usize,
    pub publication_verifier_dominator_computations: usize,
    pub accessor_splice_imported_callee_verifier_dominator_computations: usize,
    pub accessor_splice_preoptimization_verifier_dominator_computations: usize,
    pub general_inline_splice_imported_callee_verifier_dominator_computations: usize,
    pub inline_splice_pre_reoptimization_verifier_dominator_computations: usize,
}
