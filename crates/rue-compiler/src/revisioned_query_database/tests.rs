use super::body::RevisionSymbolSpace;
use super::test_support::*;
use super::*;
use crate::api_inventory::{module_declarations, test_function_declarations};
use crate::{
    CompilerSession, DiscoverySourceAssembler, FileMetadataFingerprint, ImportDiscoveryContext,
    ImportObservation, PhysicalFileIdentity, SourceMetadata,
};
use rue_span::FileId;
use std::collections::{BTreeMap, BTreeSet};

mod backend;
mod body_provider;
mod parse_import;
mod retention_cancellation;
mod semantic_declaration;

const EXPECTED_REVISIONED_QUERY_TESTS: &[&str] = &[
    "absent_declaration_shell_is_a_typed_position_free_failure_terminal",
    "absent_import_bindings_are_first_class_records_with_stamp_discipline",
    "alias_observed_after_publication_keeps_snapshot_manifest_and_view_in_agreement",
    "anonymous_dependency_frontier_canonicalizes_aliases_before_deduplication",
    "anonymous_member_diagnostics_relocate_and_internal_trivia_invalidates",
    "anonymous_member_kind_mismatch_is_deterministic_not_cancellation",
    "anonymous_member_materialization_cancellation_publishes_nothing_and_retries",
    "anonymous_member_reuses_its_candidate_artifact_and_current_source_basis",
    "anonymous_nominal_traversal_visits_each_shared_identity_exactly_once",
    "anonymous_producer_closures_are_derived_once_per_instance",
    "anonymous_producer_preserves_a_deterministic_body_diagnostic_as_a_typed_failure",
    "anonymous_producer_preserves_its_candidate_artifact_failure_before_member_publication",
    "artifact_failure_is_candidate_available_and_reaches_exact_body_failure",
    "authoritative_signature_cancellation_publishes_nothing_and_retries",
    "backend_root_publication_gate_serializes_distinct_epochs",
    "backend_root_publication_handoff_restores_last_good_root_on_rollback",
    "body_closure_call_scc_is_a_finite_graph_not_a_query_cycle",
    "body_closure_cold_warm_deletion_latency_benchmark",
    "body_closure_digest_forcing_rejects_mutation_after_publication",
    "body_closure_digest_registrar_is_permutation_independent_across_collisions",
    "body_closure_edge_addition_and_deletion_publish_exact_reached_sets",
    "body_closure_one_and_many_workers_publish_identical_reached_work_and_diagnostics",
    "body_closure_parked_outcome_precedes_an_already_observed_collision",
    "body_closure_rejects_forced_declaration_body_digest_collision",
    "body_closure_rejects_forced_declaration_declaration_digest_collision",
    "body_closure_rejects_forced_enum_enum_digest_collision",
    "body_closure_rejects_forced_struct_enum_digest_collision",
    "body_closure_rejects_forced_struct_struct_digest_collision",
    "body_closure_root_pins_reached_programs_past_the_history_floor_and_releases_deletions",
    "body_diagnostic_projection_preserves_nonlocal_and_unknown_spans",
    "body_input_cancellation_aborts_without_publishing_a_terminal",
    "body_input_registered_evaluator_classifies_unsupported_and_missing_inputs",
    "body_plan_failure_preserves_compile_errors_and_referenced_body_reachability",
    "body_publication_three_callback_transaction_rolls_back_and_retries",
    "body_reachability_scans_each_prefetched_frontier_once",
    "body_transaction_owns_candidate_artifact_through_canonical_projections",
    "call_abi_batches_layouts_across_mixed_modes_and_duplicate_parameter_types",
    "call_abi_classifies_native_target_c_named_destructor_and_drop_glue_on_both_targets",
    "call_abi_derives_anonymous_destructor_signature_from_its_exact_producer",
    "call_abi_native_classification_matches_the_live_classifier_on_both_targets",
    "call_abi_resolves_value_specialized_array_layout_on_both_targets",
    "call_abi_strbuf_return_uses_sret_on_both_planes",
    "call_abi_target_c_classification_matches_the_live_classifier_on_both_targets",
    "canceled_and_evicted_declaration_import_requests_recover",
    "canceled_declaration_shell_request_publishes_no_terminal_and_recovers",
    "canceled_production_body_attempt_commits_no_lookup_handoff",
    "cancellation_mid_body_materialization_publishes_no_terminal_and_retry_succeeds",
    "candidate_artifact_retention_bounds_history_and_rederives_evicted_values",
    "canonical_layout_matches_frozen_pool_for_padding_nested_arrays_and_enums",
    "canonical_rir_presentation_preserves_resource_limit_and_capacity_codes",
    "cold_foreign_comptime_probe_admits_owned_program_without_value_evaluation",
    "cold_signature_only_demand_does_no_candidate_astgen_work",
    "compiler_anonymous_digest_canonicalizes_empty_specialization_producers",
    "compiler_body_anonymous_registry_is_canonical_and_indexed",
    "compiler_body_anonymous_registry_unifies_producers_and_keeps_rich_facts",
    "compiler_provider_fatal_status_dominates_incomplete_status",
    "comptime_anchor_identity_comes_from_the_candidate_artifact",
    "concurrent_body_deferral_classification_is_atomic_and_not_cancellation",
    "const_produced_anonymous_member_uses_the_const_candidate_artifact",
    "declaration_import_recovers_when_resolution_observations_arrive",
    "declaration_import_relocation_reuses_and_stale_absolute_site_fails_typed",
    "declaration_imports_are_exact_lazy_and_distinguish_duplicate_specifiers",
    "declaration_imports_preserve_canonical_resolution_and_category_boundaries",
    "declaration_root_handoff_prevents_const_and_comptime_artifact_rederivation",
    "declaration_shell_batches_over_64_entries_reuse_without_thrashing",
    "declaration_shell_queries_are_keyed_exact_and_payload_stable",
    "declaration_signature_is_exact_lazy_and_red_green",
    "deferred_value_call_diagnostics_are_stable_and_keep_query_channels",
    "deterministic_failure_references_keep_selected_and_exclude_rejected_candidate",
    "direct_const_family_evaluates_the_annotated_initializer",
    "direct_const_keys_preserve_structured_evaluator_failures",
    "direct_const_named_array_length_live_local_kinds_do_not_fall_through",
    "direct_const_named_array_length_uses_the_live_evaluator_policy",
    "direct_declaration_import_family_matches_independent_import_graph_oracle",
    "direct_family_failures_are_deterministic_without_root_prevalidation",
    "direct_identity_and_signature_families_are_complete_per_declaration",
    "direct_ownership_terminals_accept_droppable_and_reject_linear_payloads",
    "direct_semantic_keys_own_declaration_validity",
    "drop_glue_plan_is_cold_reusable_and_changes_with_order_not_only_nested_set",
    "drop_glue_provider_preserves_exceptional_query_semantics",
    "drop_glue_reads_the_shape_carried_by_type_facts_instead_of_requesting_it",
    "duplicate_occurrences_share_one_host_operation_and_fan_out_typed_results",
    "durable_callable_admission_pipeline_preserves_policy_table",
    "durable_copy_and_drop_facts_match_the_air_composite_policy",
    "durable_function_materialization_shares_the_canonical_parameter_payload",
    "durable_module_member_projection_preserves_order_types_and_dependencies",
    "durable_named_member_rejects_every_multi_candidate_shape",
    "durable_named_member_resolves_each_unique_candidate_with_one_probe",
    "durable_named_value_projection_covers_each_lookup_kind_and_dependency",
    "durable_named_value_projection_preserves_real_module_binding_identity",
    "durable_nominal_materialization_shares_the_canonical_signature_payload",
    "each_body_toolchain_demand_is_queried_once_per_reachability_request",
    "editing_module_revalidates_only_its_own_retained_lookups",
    "editing_one_demanded_module_reuses_other_module_terminals",
    "equal_lookup_output_preserves_stamp_across_unrelated_module_edit",
    "explicit_occurrence_roots_select_one_of_twenty_seven_without_speculative_io",
    "facade_declaration_import_observes_its_provenance_leaf_only",
    "failed_runtime_publication_releases_pending_input_stamp_leases",
    "file_id_reassignment_refreshes_current_basis_without_dirtying_body_input",
    "file_id_renumbering_reuses_terminals_and_rebinds_current_projections",
    "foreign_signature_agreement_uses_resolved_identity_mode_and_comptime_not_names",
    "green_body_refreshes_equal_lookup_to_fresh_incarnation",
    "import_binding_classifier_covers_absent_rejected_and_repeated_sites",
    "import_frontier_rejects_roots_outside_the_pinned_plan",
    "import_publication_rejects_duplicate_and_unmatched_physical_provenance",
    "incremental_pending_requests_stop_at_conclusive_vendored_std_failure",
    "injected_body_transaction_failure_runs_in_the_structured_frontier",
    "input_stamp_tables_follow_exact_retained_full_and_overlay_views",
    "internal_body_trivia_recomputes_failure_at_the_current_span",
    "invalid_undemanded_module_is_neither_parsed_nor_lowered",
    "last_good_module_stamp_survives_beyond_the_revision_window_and_recovers_green",
    "layout_observes_only_structural_by_value_dependencies",
    "live_evaluator_named_global_cancellation_preserves_abort_channel",
    "live_root_authority_resolves_keyed_substitutions_and_restores_provider_state",
    "live_type_provider_array_length_adapter_preserves_integer_boundaries_without_rir",
    "live_type_provider_named_array_length_cases_preserve_substitution_and_lookup_channels",
    "lookup_incarnation_history_keeps_name_and_import_families_distinct",
    "lookup_incarnation_history_mutations_roll_back_exactly",
    "lookup_incarnation_history_refreshes_recency_without_duplicate_order_entries",
    "lookup_name_retains_position_free_facts_across_trivia_shifts",
    "lookup_records_distinguish_every_canonical_outcome",
    "lookup_root_handoff_journal_rolls_back_and_retries",
    "malformed_exact_option_fails_body_request_without_compute_or_publication",
    "missing_trusted_option_is_typed_incomplete_without_body_publication",
    "missing_trusted_strbuf_is_typed_incomplete_without_body_publication",
    "module_index_carries_candidate_columns_and_stays_in_module",
    "module_index_exact_import_partition_ignores_irrelevant_directives",
    "module_index_exact_name_partition_ignores_irrelevant_definitions",
    "module_index_projection_requests_and_reuses_lookup_name_terminals",
    "negative_to_positive_flips_lookup_while_unrelated_name_keeps_stamp",
    "nested_anonymous_members_share_the_ultimate_candidate_artifact",
    "new_request_generation_has_no_carried_ledger_authority",
    "nominal_well_formedness_is_a_keyed_query_and_preserves_indirection",
    "non_selector_benchmark_has_zero_staged_work",
    "noncomputing_foreign_probe_adapter_admits_a_cold_miss_once",
    "noncomputing_foreign_probe_adapter_does_not_admit_not_ready",
    "ordinary_and_rooted_publication_share_one_compatibility_namespace",
    "ownership_property_memo_preserves_decisions_across_repeats_and_recursion",
    "parked_toolchain_rounds_retain_and_reuse_the_exact_reachability_cone",
    "parking_unions_pending_demands_without_re_querying_them",
    "parse_module_frontier_one_and_many_workers_preserve_error_order",
    "parse_module_frontier_parallelizes_and_reports_exact_work",
    "parse_module_frontier_reuses_unedited_children_on_narrow_edit",
    "parsed_accessor_signature_uses_exact_owner_facts",
    "parsed_signature_projection_covers_every_category_and_exact_duplicate",
    "parsed_signature_projection_excludes_body_peer_and_absolute_trivia",
    "parsed_signature_projection_preserves_every_annotation_type_shape",
    "physical_path_change_invalidates_named_body_input",
    "platform_native_direct_target_selected_comptime_evaluates_under_the_host_arch",
    "production_provider_boundary_uses_owned_handles_and_shared_rir_view",
    "production_root_authority_keyed_admission_preserves_identity_and_dependency",
    "provider_aggregate_facts_is_accessible_follows_the_directory_domain",
    "provider_aggregate_facts_resolve_nominals_and_builtins",
    "provider_aggregate_facts_selection_order_follows_the_candidate_ranking",
    "provider_call_facts_associated_function_is_assembled_from_durable_truth",
    "provider_call_facts_function_contains_selects_from_the_candidate_set",
    "provider_call_facts_function_info_is_assembled_from_durable_truth",
    "provider_call_facts_method_info_is_assembled_from_durable_truth",
    "provider_const_info_assembly_composes_durable_truth_with_exact_spans",
    "provider_declaration_facts_match_production_epoch",
    "provider_differential_over_representative_bodies",
    "provider_endpoint_facts_anonymous_arm_mints_after_registration",
    "provider_endpoint_facts_anonymous_enum_mints_from_durable_identity",
    "provider_endpoint_facts_deferred_arms_are_pinned_gaps",
    "provider_endpoint_facts_resolve_instance_type_mints_the_declared_surface",
    "provider_endpoint_facts_rir_ops_and_nominal_presence",
    "provider_endpoint_facts_slice_arm_resolves_after_registration",
    "provider_import_absence_matches_epoch_and_records_lookup_edge",
    "provider_member_candidates_span_methods_and_assoc_fns_with_signature_handles",
    "provider_name_lookup_matches_epoch_and_records_lookup_name_edge",
    "provider_named_destructor_metadata_is_retained_on_the_minted_nominal",
    "provider_produced_anonymous_projection_rejects_conflicting_duplicate_identity",
    "provider_produced_anonymous_projection_rejects_relocated_thin_rich_duplicate",
    "provider_producer_facts_preserve_specialization_instance_terminal",
    "provider_repeated_name_lookup_reuses_the_request_local_terminal",
    "provider_repeated_nucleus_fact_reuses_the_request_local_terminal",
    "provider_type_facts_absent_and_kind_mismatch_do_not_resolve",
    "provider_type_facts_builtin_str_and_slice_names_match_epoch",
    "provider_type_facts_comptime_calls_match_epoch",
    "provider_type_facts_deferred_shapes_are_documented_gaps",
    "provider_type_facts_named_array_length_matches_epoch",
    "provider_type_facts_resolve_nominals_and_alias_match_epoch",
    "provider_type_facts_resolve_primitive_and_structural_shapes",
    "provider_well_known_option_install_mints_the_demanded_payloads",
    "published_lookup_root_edit_error_fix_loop_keeps_failure_set_warm",
    "published_lookup_root_empty_successor_replaces_prior_lease",
    "published_lookup_root_handoff_has_no_birth_eviction_window",
    "published_lookup_root_never_promotes_canceled_or_speculative",
    "published_lookup_root_pressure_exceeds_floor_supersedes_and_meters_thrash",
    "ready_anonymous_producers_share_one_structured_frontier",
    "ready_foreign_comptime_probe_reuses_full_projection_without_body_materialization",
    "require_droppable_propagates_signature_cycles_and_accepts_deferred_pointer_graphs",
    "resolve_import_recomputes_when_only_discovery_context_changes",
    "resolve_import_retains_more_than_module_cap_without_recomputation",
    "resolved_declaration_import_observes_only_winning_physical_provenance",
    "restored_state_kernel_restores_exact_state_when_operation_panics",
    "retained_provider_specialization_materializes_with_live_air_parity",
    "reused_parse_failures_are_rebound_to_the_current_file_id",
    "revisioned_query_test_family_sources_stay_partitioned",
    "revisioned_query_test_inventory_is_exact",
    "rooted_runtime_and_comptime_use_candidate_artifacts",
    "rooted_specializations_observe_one_candidate_artifact_incarnation_and_astgen",
    "rue_1667_frontier_latency_witness",
    "semantic_comptime_call_depth_guard_restores_after_every_exit",
    "semantic_comptime_call_depth_guard_restores_while_unwinding",
    "semantic_import_is_typed_missing_input_and_recovers_on_successor_revision",
    "semantic_nucleus_demand_does_not_touch_unrelated_declarations",
    "semantic_nucleus_evaluates_only_selected_const_dependencies_and_reports_cycles",
    "semantic_nucleus_lifecycle_distinguishes_terminals_from_control_flow",
    "semantic_nucleus_resolves_exact_signatures_without_whole_module_semantics",
    "semantic_nucleus_selects_declaration_time_target_branches_from_exact_configuration",
    "sibling_only_edit_keeps_artifact_transaction_and_downstream_green",
    "signature_engine_cycles_publish_family_owned_domain_failures",
    "signature_facts_constructor_head_carries_named_typed_parameters",
    "single_worker_toolchain_park_aggregates_the_complete_ready_frontier",
    "speculative_frontiers_are_effect_free_and_cannot_publish_host_results",
    "stable_declaration_classification_is_narrow_green_and_multiplicity_sensitive",
    "stable_definition_kinds_have_fixed_syntax_candidate_sets",
    "staged_comptime_facts_are_repeated_and_parallel_deterministic",
    "staged_frontier_constraint_cancellation_publishes_nothing_and_retry_is_identical",
    "staged_local_and_selector_work_scales_linearly",
    "staged_nested_selector_prefix_work_scales_linearly",
    "staged_non_generic_calls_do_not_materialize_scope",
    "staged_runtime_parameter_scope_is_inserted_once",
    "successor_revisions_carry_observations_but_new_epochs_reread",
    "test_probe_specializations_share_one_candidate_plan_arc",
    "the_revision_symbol_space_is_one_generation_per_revision",
    "toolchain_demand_maps_all_five_artifact_kinds_across_nested_method",
    "toolchain_demand_uses_typed_artifact_intrinsics_not_source_mentions",
    "transient_body_resolver_uses_canonical_plan_and_current_basis_without_reparse",
    "trusted_std_option_comptime_call_resolves_for_i64",
    "type_facts_leaf_drop_matrix_covers_every_nonaggregate_variant",
    "type_syntax_adapters_preserve_comptime_and_signature_diagnostics",
    "warning_body_projection_is_candidate_exact_and_fail_closed",
    "warning_reference_frontier_inflight_cancellation_publishes_no_aggregate",
    "warning_reference_frontier_parallelizes_and_reports_exact_work",
    "warning_reference_frontier_retains_large_cross_revision_narrow_reuse",
    "well_known_dependency_abort_classification_is_exhaustive",
    "wide_root_imports_form_one_exact_compiler_frontier",
    "wrong_exact_option_projection_fails_body_request_atomically",
];

#[test]
fn revisioned_query_test_inventory_is_exact() {
    let sources = [
        include_str!("tests.rs"),
        include_str!("tests/backend.rs"),
        include_str!("tests/body_provider/body.rs"),
        include_str!("tests/body_provider/provider.rs"),
        include_str!("tests/parse_import.rs"),
        include_str!("tests/retention_cancellation.rs"),
        include_str!("tests/semantic_declaration.rs"),
    ];
    let mut actual = sources
        .into_iter()
        .flat_map(test_function_declarations)
        .collect::<Vec<_>>();
    actual.sort();
    let unique = actual.iter().collect::<BTreeSet<_>>();
    assert_eq!(
        unique.len(),
        actual.len(),
        "revisioned-query test names must be unique"
    );
    assert_eq!(
        actual, EXPECTED_REVISIONED_QUERY_TESTS,
        "the exact revisioned-query regression inventory changed; classify intentional additions or removals in the production-aligned family list",
    );
}

#[test]
fn revisioned_query_test_family_sources_stay_partitioned() {
    let root = include_str!("tests.rs");
    assert!(
        root.lines().count() <= 400,
        "tests.rs must remain a thin family hub"
    );
    assert_eq!(
        module_declarations(root),
        [
            "backend",
            "body_provider",
            "parse_import",
            "retention_cancellation",
            "semantic_declaration",
        ],
        "tests.rs must declare exactly the production-aligned family owners",
    );
    assert!(
        include_str!("test_support.rs").lines().count() <= 1_000,
        "shared fixtures must remain a small authority",
    );
    let body_provider = include_str!("tests/body_provider.rs");
    assert!(
        body_provider.lines().count() <= 100,
        "the body/provider family hub must remain structural",
    );
    assert_eq!(
        module_declarations(body_provider),
        ["body", "provider"],
        "the body/provider hub must own exactly its two production seams",
    );
    for (family, source) in [
        ("backend", include_str!("tests/backend.rs")),
        ("body", include_str!("tests/body_provider/body.rs")),
        ("provider", include_str!("tests/body_provider/provider.rs")),
        ("parse/import", include_str!("tests/parse_import.rs")),
        (
            "retention/cancellation",
            include_str!("tests/retention_cancellation.rs"),
        ),
        (
            "semantic/declaration",
            include_str!("tests/semantic_declaration.rs"),
        ),
    ] {
        assert!(
            source.lines().count() <= 6_000,
            "{family} revisioned-query tests outgrew their production-aligned owner",
        );
        assert!(
            module_declarations(source).is_empty(),
            "{family} must not add an unscanned nested test module",
        );
    }

    let adversarial_tests = r#"
        #[test]
        /* comments and attributes may separate the marker from the item */
        #[allow(dead_code)]
        pub(in crate::revisioned_query_database)
        fn restricted_multiline_test() {}

        # [
            test
        ]
        // A comment cannot consume the pending test marker.
        pub(crate) fn commented_test() {}
    "#;
    assert_eq!(
        test_function_declarations(adversarial_tests),
        ["restricted_multiline_test", "commented_test"],
        "valid attributes, comments, visibility, and layout must not hide tests",
    );
    for (source, expected) in [
        ("pub(in crate::tests) mod\n hidden;", "hidden"),
        (
            "pub(crate) /* split */ mod inline { fn helper() {} }",
            "inline",
        ),
    ] {
        assert_eq!(
            module_declarations(source),
            [expected],
            "valid visibility, comments, and layout must not hide modules",
        );
    }
}
