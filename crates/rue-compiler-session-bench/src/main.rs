//! Reproducible in-process invalidation workload for `CanonicalFrontendSession`.

use std::{collections::HashMap, env, process, sync::Arc, time::Instant};

use rue_compiler::{
    CanonicalFrontendSession, CanonicalFrontendSessionWork, CompileOptions, ParsedModulesWork,
    SemanticDependencyIncompleteReason, SemanticDependencyInputManifest, SemanticDependencySurface,
    SemanticFullInvalidationReason, SemanticInvalidationPlan, SemanticInvalidationScope,
    SourceMetadata, SourceSnapshot,
};
use rue_span::FileId;
use serde_json::{Value, json};

const DEFAULT_MODULES: usize = 128;
const DEFAULT_WARMUP: usize = 3;
const DEFAULT_ITERATIONS: usize = 10;

#[derive(Clone, Copy)]
struct Config {
    modules: usize,
    warmup: usize,
    iterations: usize,
}

#[derive(Clone, Copy, Default)]
struct QueryCounts {
    merge_executions: usize,
    merge_reuses: usize,
    rir_executions: usize,
    rir_reuses: usize,
    semantic_executions: usize,
    semantic_reuses: usize,
    definition_executions: usize,
    definition_reuses: usize,
    downstream_invalidations: usize,
    semantic_entries_invalidated: usize,
    definition_entries_invalidated: usize,
    dependency_manifest_executions: usize,
    dependency_manifest_reuses: usize,
    invalidation_plan_executions: usize,
    invalidation_plan_reuses: usize,
    declaration_reuse_plans: usize,
    durable_records_compared: usize,
    durable_records_reused: usize,
    ordinary_declaration_resolutions_skipped: usize,
    durable_installs: usize,
    declaration_reuse_fallbacks: usize,
    durable_cache_population_bindings: usize,
}

impl QueryCounts {
    fn from(work: &CanonicalFrontendSessionWork) -> Self {
        Self {
            merge_executions: work.merge.executions,
            merge_reuses: work.merge.reuses,
            rir_executions: work.rir.executions,
            rir_reuses: work.rir.reuses,
            semantic_executions: work.semantic.executions,
            semantic_reuses: work.semantic.reuses,
            definition_executions: work.definitions.executions,
            definition_reuses: work.definitions.reuses,
            downstream_invalidations: work.downstream_invalidations,
            semantic_entries_invalidated: work.semantic_entries_invalidated,
            definition_entries_invalidated: work.definition_entries_invalidated,
            dependency_manifest_executions: work.dependency_manifests.executions,
            dependency_manifest_reuses: work.dependency_manifests.reuses,
            invalidation_plan_executions: work.invalidation_plans.executions,
            invalidation_plan_reuses: work.invalidation_plans.reuses,
            declaration_reuse_plans: work.declaration_reuse_plans,
            durable_records_compared: work.durable_records_compared,
            durable_records_reused: work.durable_records_reused,
            ordinary_declaration_resolutions_skipped: work.ordinary_declaration_resolutions_skipped,
            durable_installs: work.durable_installs,
            declaration_reuse_fallbacks: work.declaration_reuse_fallbacks,
            durable_cache_population_bindings: work.durable_cache_population_bindings,
        }
    }

    fn delta(self, before: Self) -> Self {
        Self {
            merge_executions: self.merge_executions - before.merge_executions,
            merge_reuses: self.merge_reuses - before.merge_reuses,
            rir_executions: self.rir_executions - before.rir_executions,
            rir_reuses: self.rir_reuses - before.rir_reuses,
            semantic_executions: self.semantic_executions - before.semantic_executions,
            semantic_reuses: self.semantic_reuses - before.semantic_reuses,
            definition_executions: self.definition_executions - before.definition_executions,
            definition_reuses: self.definition_reuses - before.definition_reuses,
            downstream_invalidations: self.downstream_invalidations
                - before.downstream_invalidations,
            semantic_entries_invalidated: self.semantic_entries_invalidated
                - before.semantic_entries_invalidated,
            definition_entries_invalidated: self.definition_entries_invalidated
                - before.definition_entries_invalidated,
            dependency_manifest_executions: self.dependency_manifest_executions
                - before.dependency_manifest_executions,
            dependency_manifest_reuses: self.dependency_manifest_reuses
                - before.dependency_manifest_reuses,
            invalidation_plan_executions: self.invalidation_plan_executions
                - before.invalidation_plan_executions,
            invalidation_plan_reuses: self.invalidation_plan_reuses
                - before.invalidation_plan_reuses,
            declaration_reuse_plans: self.declaration_reuse_plans - before.declaration_reuse_plans,
            durable_records_compared: self.durable_records_compared
                - before.durable_records_compared,
            durable_records_reused: self.durable_records_reused - before.durable_records_reused,
            ordinary_declaration_resolutions_skipped: self.ordinary_declaration_resolutions_skipped
                - before.ordinary_declaration_resolutions_skipped,
            durable_installs: self.durable_installs - before.durable_installs,
            declaration_reuse_fallbacks: self.declaration_reuse_fallbacks
                - before.declaration_reuse_fallbacks,
            durable_cache_population_bindings: self.durable_cache_population_bindings
                - before.durable_cache_population_bindings,
        }
    }

    fn json(self) -> Value {
        json!({
            "merge_executions": self.merge_executions,
            "merge_reuses": self.merge_reuses,
            "rir_executions": self.rir_executions,
            "rir_reuses": self.rir_reuses,
            "semantic_executions": self.semantic_executions,
            "semantic_reuses": self.semantic_reuses,
            "definition_executions": self.definition_executions,
            "definition_reuses": self.definition_reuses,
            "downstream_invalidations": self.downstream_invalidations,
            "semantic_entries_invalidated": self.semantic_entries_invalidated,
            "definition_entries_invalidated": self.definition_entries_invalidated,
            "dependency_manifest_executions": self.dependency_manifest_executions,
            "dependency_manifest_reuses": self.dependency_manifest_reuses,
            "invalidation_plan_executions": self.invalidation_plan_executions,
            "invalidation_plan_reuses": self.invalidation_plan_reuses,
            "declaration_reuse_plans": self.declaration_reuse_plans,
            "durable_records_compared": self.durable_records_compared,
            "durable_records_reused": self.durable_records_reused,
            "ordinary_declaration_resolutions_skipped": self.ordinary_declaration_resolutions_skipped,
            "durable_installs": self.durable_installs,
            "declaration_reuse_fallbacks": self.declaration_reuse_fallbacks,
            "durable_cache_population_bindings": self.durable_cache_population_bindings,
        })
    }
}

struct Fixture {
    modules: usize,
    base: SourceSnapshot,
    reachable_root_body_edit: SourceSnapshot,
    identity_edit: SourceSnapshot,
    syntax_error: SourceSnapshot,
}

impl Fixture {
    fn new(modules: usize) -> Self {
        assert!(modules >= 2, "workload requires at least two modules");
        Self {
            modules,
            base: snapshot(modules, Variant::Base),
            reachable_root_body_edit: snapshot(modules, Variant::ReachableRootBodyEdit),
            identity_edit: snapshot(modules, Variant::IdentityEdit),
            syntax_error: snapshot(modules, Variant::SyntaxError),
        }
    }
}

#[derive(Clone, Copy)]
enum Variant {
    Base,
    ReachableRootBodyEdit,
    IdentityEdit,
    SyntaxError,
}

fn snapshot(modules: usize, variant: Variant) -> SourceSnapshot {
    let changed = modules - 1;
    let physical = (0..modules)
        .map(|index| (FileId::new(index as u32), format!("/bench/m{index:03}.rue")))
        .collect::<HashMap<_, _>>();
    let logical = (0..modules)
        .map(|index| {
            let name = if matches!(variant, Variant::IdentityEdit) && index == changed {
                format!("renamed{index:03}.rue")
            } else {
                format!("m{index:03}.rue")
            };
            (FileId::new(index as u32), name)
        })
        .collect::<HashMap<_, _>>();
    let metadata = SourceMetadata::new(FileId::new(0), physical, logical).unwrap();
    let sources = (0..modules)
        .map(|index| {
            let source = if index == 0 {
                format!(
                    "fn main() -> i32 {{ {} }}",
                    usize::from(!matches!(variant, Variant::Base))
                )
            } else if index == changed && matches!(variant, Variant::SyntaxError) {
                format!("fn leaf{index}( {{")
            } else {
                format!("fn leaf{index}() -> i32 {{ 0 }}")
            };
            (FileId::new(index as u32), Arc::new(source))
        })
        .collect();
    SourceSnapshot::new(metadata, sources).unwrap()
}

fn parse_json(work: ParsedModulesWork) -> Value {
    json!({
        "lexer_invocations": work.syntax.lexer_invocations,
        "parser_invocations": work.syntax.parser_invocations,
        "lexed_bytes": work.syntax.lexed_bytes,
        "tokens": work.syntax.tokens,
        "modules_considered": work.modules_considered,
        "modules_reused": work.modules_reused,
        "modules_rebound": work.modules_rebound,
        "modules_reparsed": work.modules_reparsed,
        "source_text_clones": work.source_text_clones,
        "source_bytes_rehashed": work.source_bytes_rehashed,
    })
}

fn semantic_work_json(work: &CanonicalFrontendSessionWork, from: usize) -> Value {
    let records = &work.semantic_records[from..];
    json!({
        "bind_invocations": records.iter().map(|record| record.work.binding.bind_invocations).sum::<usize>(),
        "body_free_function_lookups": records.iter().map(|record| record.work.body_analysis.free_function_record_lookups).sum::<usize>(),
        "bodies_attempted": records.iter().map(|record| record.work.body_analysis.bodies_attempted).sum::<usize>(),
        "bodies_succeeded": records.iter().map(|record| record.work.body_analysis.bodies_succeeded).sum::<usize>(),
        "bodies_failed": records.iter().map(|record| record.work.body_analysis.bodies_failed).sum::<usize>(),
        "air_instructions_produced": records.iter().map(|record| record.work.body_analysis.air_instructions_produced).sum::<usize>(),
        "local_strings_produced": records.iter().map(|record| record.work.body_analysis.local_strings_produced).sum::<usize>(),
        "string_ids_remapped": records.iter().map(|record| record.work.body_analysis.string_ids_remapped).sum::<usize>(),
        "specialization_air_instructions_scanned": records.iter().map(|record| record.work.body_analysis.specialization_air_instructions_scanned).sum::<usize>(),
        "generic_calls_observed": records.iter().map(|record| record.work.body_analysis.generic_calls_observed).sum::<usize>(),
        "specialization_requests_unique": records.iter().map(|record| record.work.body_analysis.specialization_requests_unique).sum::<usize>(),
        "specialization_requests_duplicate": records.iter().map(|record| record.work.body_analysis.specialization_requests_duplicate).sum::<usize>(),
        "specialization_rewrites": records.iter().map(|record| record.work.body_analysis.specialization_rewrites).sum::<usize>(),
        "specialization_rounds": records.iter().map(|record| record.work.body_analysis.specialization_rounds).sum::<usize>(),
        "specialized_bodies_attempted": records.iter().map(|record| record.work.body_analysis.specialized_bodies_attempted).sum::<usize>(),
        "specialized_bodies_succeeded": records.iter().map(|record| record.work.body_analysis.specialized_bodies_succeeded).sum::<usize>(),
        "specialized_bodies_failed": records.iter().map(|record| record.work.body_analysis.specialized_bodies_failed).sum::<usize>(),
        "cfg": {
            "drop_glue_functions_synthesized": records.iter().map(|record| record.work.cfg.drop_glue_functions_synthesized).sum::<usize>(),
            "functions_considered": records.iter().map(|record| record.work.cfg.functions_considered).sum::<usize>(),
            "comptime_functions_filtered": records.iter().map(|record| record.work.cfg.comptime_functions_filtered).sum::<usize>(),
            "builds_attempted": records.iter().map(|record| record.work.cfg.cfg_builds_attempted).sum::<usize>(),
            "builds_succeeded": records.iter().map(|record| record.work.cfg.cfg_builds_succeeded).sum::<usize>(),
            "builds_failed": records.iter().map(|record| record.work.cfg.cfg_builds_failed).sum::<usize>(),
            "air_instructions_consumed": records.iter().map(|record| record.work.cfg.air_instructions_consumed).sum::<usize>(),
            "optimization_attempts": records.iter().map(|record| record.work.cfg.optimization_attempts).sum::<usize>(),
            "optimization_completions": records.iter().map(|record| record.work.cfg.optimization_completions).sum::<usize>(),
            "optimized_level_attempts": records.iter().map(|record| record.work.cfg.optimized_level_attempts).sum::<usize>(),
            "warnings_emitted": records.iter().map(|record| record.work.cfg.cfg_warnings_emitted).sum::<usize>(),
            "implicit_destructor_targets_emitted": records.iter().map(|record| record.work.cfg.implicit_destructor_targets_emitted).sum::<usize>(),
        },
        "ordinary_free_function_dependency_events": records.iter().map(|record| record.work.body_analysis.ordinary_free_function_dependency_events).sum::<usize>(),
        "specialized_origin_records": records.iter().map(|record| record.work.body_analysis.specialized_origin_records).sum::<usize>(),
        "specialized_free_function_dependency_events": records.iter().map(|record| record.work.body_analysis.specialized_free_function_dependency_events).sum::<usize>(),
        "named_method_dependency_events": records.iter().map(|record| record.work.body_analysis.named_method_dependency_events).sum::<usize>(),
        "named_destructor_dependency_events": records.iter().map(|record| record.work.body_analysis.named_destructor_dependency_events).sum::<usize>(),
        "declaration_type_dependency_events": records.iter().map(|record| record.work.body_analysis.declaration_type_dependency_events).sum::<usize>(),
        "declaration_type_call_head_dependency_events": records.iter().map(|record| record.work.body_analysis.declaration_type_call_head_dependency_events).sum::<usize>(),
        "named_const_dependency_events": records.iter().map(|record| record.work.body_analysis.named_const_dependency_events).sum::<usize>(),
        "manifest_build_invocations": records.iter().map(|record| record.work.manifest.build_invocations).sum::<usize>(),
        "declaration_reuse": {
            "plan_executions": records.iter().map(|record| record.work.declaration_reuse.plan_executions).sum::<usize>(),
            "durable_records_compared": records.iter().map(|record| record.work.declaration_reuse.durable_records_compared).sum::<usize>(),
            "durable_records_reused": records.iter().map(|record| record.work.declaration_reuse.durable_records_reused).sum::<usize>(),
            "ordinary_declaration_resolutions_skipped": records.iter().map(|record| record.work.declaration_reuse.ordinary_declaration_resolutions_skipped).sum::<usize>(),
            "install_invocations": records.iter().map(|record| record.work.declaration_reuse.install_invocations).sum::<usize>(),
            "fallbacks": records.iter().map(|record| record.work.declaration_reuse.fallbacks).sum::<usize>(),
            "semantic_epochs_started": records.iter().map(|record| record.work.declaration_reuse.semantic_epochs_started).sum::<usize>(),
            "declaration_indexes_built": records.iter().map(|record| record.work.declaration_reuse.declaration_indexes_built).sum::<usize>(),
            "shell_predeclaration_epochs": records.iter().map(|record| record.work.declaration_reuse.shell_predeclaration_epochs).sum::<usize>(),
            "durable_cache_population_exports": records.iter().map(|record| record.work.declaration_reuse.durable_cache_population_exports).sum::<usize>(),
            "fallback_epochs_started": records.iter().map(|record| record.work.declaration_reuse.fallback_epochs_started).sum::<usize>(),
        },
        "semantic_epochs_started": records.iter().map(|record| record.work.declaration_reuse.semantic_epochs_started).sum::<usize>(),
        "declaration_indexes_built": records.iter().map(|record| record.work.declaration_reuse.declaration_indexes_built).sum::<usize>(),
        "shell_predeclaration_epochs": records.iter().map(|record| record.work.declaration_reuse.shell_predeclaration_epochs).sum::<usize>(),
        "durable_cache_population_exports": records.iter().map(|record| record.work.declaration_reuse.durable_cache_population_exports).sum::<usize>(),
        "fallback_epochs_started": records.iter().map(|record| record.work.declaration_reuse.fallback_epochs_started).sum::<usize>(),
    })
}

fn definition_work_json(work: &CanonicalFrontendSessionWork, from: usize) -> Value {
    let records = &work.definition_records[from..];
    json!({
        "bind_invocations": records.iter().map(|record| record.binding.bind_invocations).sum::<usize>(),
        "manifest_build_invocations": records.iter().map(|record| record.manifest.build_invocations).sum::<usize>(),
        "manifest_bindings_visited": records.iter().map(|record| record.issuance.manifest_bindings_visited).sum::<usize>(),
        "ids_issued": records.iter().map(|record| record.issuance.ids_issued).sum::<usize>(),
    })
}

fn measure<F>(session: &mut CanonicalFrontendSession, operation: F) -> Value
where
    F: FnOnce(&mut CanonicalFrontendSession) -> ParsedModulesWork,
{
    let before = QueryCounts::from(session.work());
    let semantic_records = session.work().semantic_records.len();
    let definition_records = session.work().definition_records.len();
    let started = Instant::now();
    let parse = operation(session);
    let elapsed = started.elapsed();
    let work = session.work();
    let query_delta = QueryCounts::from(work).delta(before);
    // Successful updates replace the current-revision record vectors. Treat
    // that reset as a new zero-based measurement window.
    let semantic_records =
        if query_delta.semantic_executions > 0 && work.semantic_records.len() <= semantic_records {
            0
        } else {
            semantic_records
        };
    let definition_records = if query_delta.definition_executions > 0
        && work.definition_records.len() <= definition_records
    {
        0
    } else {
        definition_records
    };
    json!({
        "wall_time_ns": elapsed.as_nanos(),
        "parse": parse_json(parse),
        "queries": query_delta.json(),
        "merge_work": {
            "definition_shards_indexed": work.last_merge.definition_shards_indexed,
            "definition_shards_reused": work.last_merge.definition_shards_reused,
            "definition_shards_rebuilt": work.last_merge.definition_shards_rebuilt,
        },
        "semantic_work": semantic_work_json(work, semantic_records),
        "definition_work": definition_work_json(work, definition_records),
    })
}

fn invalidation_plan_json(plan: &SemanticInvalidationPlan) -> Value {
    let mut dependency_blockers = Vec::new();
    let (scope, reasons) = match plan.scope() {
        SemanticInvalidationScope::Full { reasons } => (
            "full",
            reasons
                .iter()
                .map(|reason| match reason {
                    SemanticFullInvalidationReason::RootChanged => "root_changed",
                    SemanticFullInvalidationReason::ModuleImportsChanged => {
                        "module_imports_changed"
                    }
                    SemanticFullInvalidationReason::TargetChanged => "target_changed",
                    SemanticFullInvalidationReason::PreviewFeaturesChanged => {
                        "preview_features_changed"
                    }
                    SemanticFullInvalidationReason::IncompleteDefinitionUniverse => {
                        "incomplete_definition_universe"
                    }
                    SemanticFullInvalidationReason::IncompleteDependencyGraph(blockers) => {
                        dependency_blockers.extend(blockers.iter().map(|blocker| {
                            json!({
                                "owner": blocker.owner().map(|owner| owner.name()),
                                "surface": dependency_surface_name(blocker.surface()),
                                "reason": dependency_reason_name(blocker.reason()),
                            })
                        }));
                        "incomplete_dependency_graph"
                    }
                })
                .collect::<Vec<_>>(),
        ),
        SemanticInvalidationScope::Incremental => ("incremental", Vec::new()),
    };
    let work = plan.work();
    json!({
        "scope": scope,
        "full_reasons": reasons,
        "dependency_blockers": dependency_blockers,
        "added": plan.added().len(),
        "removed": plan.removed().len(),
        "changed": plan.changed().len(),
        "invalidated": plan.invalidated().len(),
        "reusable": plan.reusable().len(),
        "work": {
            "definition_fingerprints_compared": work.definition_fingerprints_compared,
            "dependency_edges_visited": work.dependency_edges_visited,
            "reverse_closure_nodes_visited": work.reverse_closure_nodes_visited,
            "extra_rir_instructions_visited": work.extra_rir_instructions_visited,
        },
    })
}

fn dependency_surface_name(surface: SemanticDependencySurface) -> &'static str {
    match surface {
        SemanticDependencySurface::FreeFunctionCall => "free_function_call",
        SemanticDependencySurface::NonGenericNamedMethodCall => "non_generic_named_method_call",
        SemanticDependencySurface::GenericNamedMethodCall => "generic_named_method_call",
        SemanticDependencySurface::NamedDestructorCall => "named_destructor_call",
        SemanticDependencySurface::ImplicitNamedDestructor => "implicit_named_destructor",
        SemanticDependencySurface::DeclarationType => "declaration_type",
        SemanticDependencySurface::DeclarationTypeCallHead => "declaration_type_call_head",
        SemanticDependencySurface::SupportedTypeCallHead => "supported_type_call_head",
        SemanticDependencySurface::NamedValueConst => "named_value_const",
    }
}

fn dependency_reason_name(reason: SemanticDependencyIncompleteReason) -> &'static str {
    match reason {
        SemanticDependencyIncompleteReason::CallerEndpointUnavailable => {
            "caller_endpoint_unavailable"
        }
        SemanticDependencyIncompleteReason::GenericSubstitutionIdentityUnavailable => {
            "generic_substitution_identity_unavailable"
        }
        SemanticDependencyIncompleteReason::DestructorEndpointUnavailable => {
            "destructor_endpoint_unavailable"
        }
        SemanticDependencyIncompleteReason::AnonymousDropOwnerUnavailable => {
            "anonymous_drop_owner_unavailable"
        }
        SemanticDependencyIncompleteReason::ResolvedTypeIdentityUnavailable => {
            "resolved_type_identity_unavailable"
        }
        SemanticDependencyIncompleteReason::TypeCallHeadIdentityUnavailable => {
            "type_call_head_identity_unavailable"
        }
        SemanticDependencyIncompleteReason::UnsupportedDynamicTypeCallHead => {
            "unsupported_dynamic_type_call_head"
        }
        SemanticDependencyIncompleteReason::ConstEndpointUnavailable => {
            "const_endpoint_unavailable"
        }
    }
}

fn measure_manifest_plan(
    session: &mut CanonicalFrontendSession,
    source: &SourceSnapshot,
    previous: Option<&Arc<SemanticDependencyInputManifest>>,
    options: &CompileOptions,
) -> (Value, Arc<SemanticDependencyInputManifest>) {
    let before = QueryCounts::from(session.work());
    let update = session.update(source);
    let parse = update.work();
    update.into_result().unwrap();
    let manifest_started = Instant::now();
    let current = session.semantic_dependency_inputs(options, None).unwrap();
    let manifest_elapsed = manifest_started.elapsed();
    let after_manifest = QueryCounts::from(session.work());
    let plan_started = Instant::now();
    let plan = session.semantic_invalidation_plan(previous.unwrap_or(&current), &current);
    let plan_elapsed = plan_started.elapsed();
    let after_plan = QueryCounts::from(session.work());
    (
        json!({
            "manifest_wall_time_ns": manifest_elapsed.as_nanos(),
            "plan_wall_time_ns": plan_elapsed.as_nanos(),
            "parse": parse_json(parse),
            "manifest_queries": after_manifest.delta(before).json(),
            "planner_queries": after_plan.delta(after_manifest).json(),
            "plan": invalidation_plan_json(&plan),
        }),
        current,
    )
}

fn run_iteration(fixture: &Fixture) -> Vec<Value> {
    let options = CompileOptions::default();
    let mut session = CanonicalFrontendSession::new();
    let mut scenarios = Vec::new();

    scenarios.push(named(
        "cold",
        measure(&mut session, |session| {
            let parse = session.update(&fixture.base).work();
            session.merge().unwrap();
            session.rir().unwrap();
            session.semantic(&options).unwrap();
            parse
        }),
    ));
    scenarios.push(named(
        "exact_noop",
        measure(&mut session, |session| {
            let parse = session.update(&fixture.base).work();
            session.merge().unwrap();
            session.rir().unwrap();
            session.semantic(&options).unwrap();
            parse
        }),
    ));
    scenarios.push(named(
        "reachable_root_body_edit",
        measure(&mut session, |session| {
            let update = session.update(&fixture.reachable_root_body_edit);
            assert!(update.downstream_invalidated());
            let parse = update.work();
            update.into_result().unwrap();
            session.merge().unwrap();
            session.rir().unwrap();
            session.semantic(&options).unwrap();
            parse
        }),
    ));
    scenarios.push(named(
        "module_identity_change",
        measure(&mut session, |session| {
            let update = session.update(&fixture.identity_edit);
            assert!(update.downstream_invalidated());
            let parse = update.work();
            update.into_result().unwrap();
            session.merge().unwrap();
            session.rir().unwrap();
            session.semantic(&options).unwrap();
            parse
        }),
    ));
    scenarios.push(named(
        "failed_syntax_edit",
        measure(&mut session, |session| {
            let update = session.update(&fixture.syntax_error);
            assert!(update.result().is_err());
            let parse = update.work();
            session.merge().unwrap();
            session.rir().unwrap();
            session.semantic(&options).unwrap();
            parse
        }),
    ));
    scenarios.push(named(
        "syntax_recovery",
        measure(&mut session, |session| {
            let parse = session.update(&fixture.identity_edit).work();
            session.merge().unwrap();
            session.rir().unwrap();
            session.semantic(&options).unwrap();
            parse
        }),
    ));

    let mut stable = CanonicalFrontendSession::new();
    scenarios.push(named(
        "stable_definitions_cold",
        measure(&mut stable, |session| {
            let parse = session.update(&fixture.base).work();
            session.stable_definitions(&options).unwrap();
            parse
        }),
    ));
    scenarios.push(named(
        "stable_definitions_reuse",
        measure(&mut stable, |session| {
            session.stable_definitions(&options).unwrap();
            ParsedModulesWork::default()
        }),
    ));
    let mut planner = CanonicalFrontendSession::new();
    let (cold_plan, base_manifest) =
        measure_manifest_plan(&mut planner, &fixture.base, None, &options);
    scenarios.push(named("invalidation_plan_cold", cold_plan));
    let (noop_plan, noop_manifest) =
        measure_manifest_plan(&mut planner, &fixture.base, Some(&base_manifest), &options);
    scenarios.push(named("invalidation_plan_exact_noop", noop_plan));
    let (root_plan, root_manifest) = measure_manifest_plan(
        &mut planner,
        &fixture.reachable_root_body_edit,
        Some(&noop_manifest),
        &options,
    );
    scenarios.push(named(
        "invalidation_plan_reachable_root_body_edit",
        root_plan,
    ));
    let (identity_plan, _) = measure_manifest_plan(
        &mut planner,
        &fixture.identity_edit,
        Some(&root_manifest),
        &options,
    );
    scenarios.push(named(
        "invalidation_plan_module_identity_change",
        identity_plan,
    ));
    assert_structure(&scenarios, fixture.modules);
    scenarios
}

fn named(name: &str, mut value: Value) -> Value {
    value["name"] = json!(name);
    value
}

fn count(value: &Value, path: &[&str]) -> u64 {
    path.iter()
        .fold(value, |value, key| &value[*key])
        .as_u64()
        .unwrap()
}

fn assert_structure(scenarios: &[Value], modules: usize) {
    let get = |name: &str| {
        scenarios
            .iter()
            .find(|value| value["name"] == name)
            .unwrap()
    };
    let cold = get("cold");
    assert_eq!(count(cold, &["parse", "modules_reparsed"]), modules as u64);
    assert_eq!(count(cold, &["parse", "lexer_invocations"]), modules as u64);
    assert_eq!(
        count(cold, &["parse", "parser_invocations"]),
        modules as u64
    );
    assert_query_executions(cold, 1, 1, 1, 0);
    assert_semantic_work(cold, 1, 1, 0);
    assert_declaration_epoch_work(cold, 1, 1, 1, 1, 0);
    assert_eq!(
        cold["semantic_work"]["declaration_reuse"]["plan_executions"],
        0
    );
    assert_eq!(
        count(cold, &["queries", "durable_cache_population_bindings"]),
        0
    );

    let noop = get("exact_noop");
    assert_eq!(count(noop, &["parse", "modules_reused"]), modules as u64);
    assert_reuse_parse_is_all_zero(noop);
    assert_query_executions(noop, 0, 0, 0, 0);
    assert_eq!(count(noop, &["queries", "merge_reuses"]), 1);
    // The explicit RIR query reuses once, then semantic validation asks for it again.
    assert_eq!(count(noop, &["queries", "rir_reuses"]), 2);
    assert_eq!(count(noop, &["queries", "semantic_reuses"]), 1);

    let root_edit = get("reachable_root_body_edit");
    assert_eq!(
        count(root_edit, &["parse", "modules_reused"]),
        (modules - 1) as u64
    );
    assert_eq!(count(root_edit, &["parse", "modules_reparsed"]), 1);
    assert_eq!(count(root_edit, &["parse", "lexer_invocations"]), 1);
    assert_eq!(count(root_edit, &["parse", "parser_invocations"]), 1);
    assert_query_executions(root_edit, 1, 1, 1, 0);
    assert_eq!(
        count(root_edit, &["merge_work", "definition_shards_reused"]),
        modules as u64
    );
    assert_eq!(
        count(root_edit, &["merge_work", "definition_shards_rebuilt"]),
        0
    );
    assert_eq!(
        count(root_edit, &["queries", "downstream_invalidations"]),
        1
    );
    assert_eq!(count(root_edit, &["queries", "declaration_reuse_plans"]), 1);
    assert_eq!(
        count(root_edit, &["queries", "durable_records_compared"]),
        modules as u64
    );
    assert_eq!(
        count(root_edit, &["queries", "durable_records_reused"]),
        modules as u64
    );
    assert_eq!(
        count(
            root_edit,
            &["queries", "ordinary_declaration_resolutions_skipped"]
        ),
        1
    );
    assert_eq!(count(root_edit, &["queries", "durable_installs"]), 1);
    assert_eq!(
        count(root_edit, &["queries", "declaration_reuse_fallbacks"]),
        0
    );
    assert_semantic_work(root_edit, 1, 1, 0);
    assert_declaration_epoch_work(root_edit, 1, 1, 1, 0, 0);
    let root_reuse = &root_edit["semantic_work"]["declaration_reuse"];
    assert_eq!(root_reuse["plan_executions"], 1);
    assert_eq!(root_reuse["durable_records_compared"], modules as u64);
    assert_eq!(root_reuse["durable_records_reused"], modules as u64);
    assert_eq!(root_reuse["ordinary_declaration_resolutions_skipped"], 1);
    assert_eq!(root_reuse["install_invocations"], 1);
    assert_eq!(root_reuse["fallbacks"], 0);
    assert_body_cfg_work_equal(cold, root_edit);
    assert_eq!(
        count(root_edit, &["semantic_work", "semantic_epochs_started"]),
        1
    );
    assert_eq!(
        count(root_edit, &["semantic_work", "declaration_indexes_built"]),
        1
    );
    assert_eq!(
        count(root_edit, &["semantic_work", "shell_predeclaration_epochs"]),
        1
    );
    assert_eq!(
        count(root_edit, &["semantic_work", "fallback_epochs_started"]),
        0
    );
    assert_eq!(
        count(root_edit, &["queries", "durable_cache_population_bindings"]),
        0
    );

    let identity = get("module_identity_change");
    assert_eq!(count(identity, &["parse", "modules_rebound"]), 1);
    assert_reuse_parse_is_all_zero(identity);
    assert_query_executions(identity, 1, 1, 1, 0);
    assert_eq!(
        count(identity, &["merge_work", "definition_shards_reused"]),
        (modules - 1) as u64
    );
    assert_eq!(
        count(identity, &["merge_work", "definition_shards_rebuilt"]),
        1
    );
    assert_eq!(count(identity, &["queries", "downstream_invalidations"]), 1);

    let failed = get("failed_syntax_edit");
    assert_eq!(count(failed, &["parse", "modules_reparsed"]), 1);
    assert_eq!(count(failed, &["parse", "lexer_invocations"]), 1);
    assert_eq!(count(failed, &["parse", "parser_invocations"]), 1);
    assert_query_executions(failed, 0, 0, 0, 0);
    assert_eq!(count(failed, &["queries", "merge_reuses"]), 1);
    assert_eq!(count(failed, &["queries", "rir_reuses"]), 2);
    assert_eq!(count(failed, &["queries", "downstream_invalidations"]), 0);
    assert_eq!(count(failed, &["queries", "semantic_reuses"]), 1);

    let recovery = get("syntax_recovery");
    assert_eq!(
        count(recovery, &["parse", "modules_reused"]),
        modules as u64
    );
    assert_reuse_parse_is_all_zero(recovery);
    assert_query_executions(recovery, 0, 0, 0, 0);
    assert_eq!(count(recovery, &["queries", "semantic_reuses"]), 1);

    let stable_cold = get("stable_definitions_cold");
    assert_query_executions(stable_cold, 1, 1, 1, 1);
    assert_semantic_work(stable_cold, 1, 1, 0);
    assert_eq!(
        count(stable_cold, &["definition_work", "bind_invocations"]),
        1
    );
    assert_eq!(
        count(
            stable_cold,
            &["definition_work", "manifest_build_invocations"]
        ),
        1
    );
    let stable_reuse = get("stable_definitions_reuse");
    assert_query_executions(stable_reuse, 0, 0, 0, 0);
    assert_semantic_work(stable_reuse, 0, 0, 0);
    assert_eq!(
        count(stable_reuse, &["definition_work", "bind_invocations"]),
        0
    );
    assert_eq!(
        count(
            stable_reuse,
            &["definition_work", "manifest_build_invocations"]
        ),
        0
    );
    assert_eq!(count(stable_reuse, &["queries", "definition_reuses"]), 1);

    let plan_cold = get("invalidation_plan_cold");
    assert_eq!(
        count(
            plan_cold,
            &["manifest_queries", "dependency_manifest_executions"]
        ),
        1
    );
    assert_production_incremental_plan(plan_cold, 1, 0, 0, modules, modules, 0);
    let plan_noop = get("invalidation_plan_exact_noop");
    assert_reuse_parse_is_all_zero(plan_noop);
    assert_eq!(
        count(
            plan_noop,
            &["manifest_queries", "dependency_manifest_reuses"]
        ),
        1
    );
    assert_eq!(
        count(plan_noop, &["planner_queries", "invalidation_plan_reuses"]),
        1
    );
    assert_production_incremental_plan(plan_noop, 0, 0, 0, modules, modules, 0);
    let plan_root_edit = get("invalidation_plan_reachable_root_body_edit");
    assert_eq!(count(plan_root_edit, &["parse", "modules_reparsed"]), 1);
    assert_production_incremental_plan(plan_root_edit, 1, 1, 1, modules - 1, modules, 1);
    let plan_identity = get("invalidation_plan_module_identity_change");
    assert_eq!(count(plan_identity, &["parse", "modules_rebound"]), 1);
    assert_production_incremental_plan(plan_identity, 1, 0, 2, modules - 1, modules - 1, 2);
}

fn assert_body_cfg_work_equal(left: &Value, right: &Value) {
    for field in [
        "bodies_attempted",
        "bodies_succeeded",
        "bodies_failed",
        "air_instructions_produced",
        "local_strings_produced",
        "string_ids_remapped",
        "specialization_air_instructions_scanned",
        "generic_calls_observed",
        "specialization_requests_unique",
        "specialization_requests_duplicate",
        "specialization_rewrites",
        "specialization_rounds",
        "specialized_bodies_attempted",
        "specialized_bodies_succeeded",
        "specialized_bodies_failed",
    ] {
        assert_eq!(
            left["semantic_work"][field], right["semantic_work"][field],
            "body work field {field} differs before body reuse exists"
        );
    }
    assert_eq!(
        left["semantic_work"]["cfg"], right["semantic_work"]["cfg"],
        "CFG work differs before CFG reuse exists"
    );
}

fn assert_production_incremental_plan(
    scenario: &Value,
    executions: u64,
    changed: u64,
    invalidated: usize,
    reusable: usize,
    compared: usize,
    closure: usize,
) {
    assert_eq!(scenario["plan"]["scope"], "incremental");
    assert_eq!(scenario["plan"]["full_reasons"], json!([]));
    assert_eq!(scenario["plan"]["dependency_blockers"], json!([]));
    assert_eq!(count(scenario, &["plan", "reusable"]), reusable as u64);
    assert_eq!(
        count(scenario, &["plan", "invalidated"]),
        invalidated as u64
    );
    assert_eq!(count(scenario, &["plan", "changed"]), changed);
    assert_eq!(
        count(
            scenario,
            &["plan", "work", "definition_fingerprints_compared"]
        ),
        compared as u64
    );
    assert_eq!(
        count(
            scenario,
            &["plan", "work", "extra_rir_instructions_visited"]
        ),
        0
    );
    assert_eq!(
        count(scenario, &["plan", "work", "dependency_edges_visited"]),
        0
    );
    assert_eq!(
        count(scenario, &["plan", "work", "reverse_closure_nodes_visited"]),
        closure as u64
    );
    assert_eq!(count(scenario, &["planner_queries", "rir_executions"]), 0);
    assert_eq!(
        count(
            scenario,
            &["planner_queries", "invalidation_plan_executions"]
        ),
        executions
    );
}

fn assert_reuse_parse_is_all_zero(scenario: &Value) {
    assert_eq!(count(scenario, &["parse", "lexer_invocations"]), 0);
    assert_eq!(count(scenario, &["parse", "parser_invocations"]), 0);
    assert_eq!(count(scenario, &["parse", "source_text_clones"]), 0);
    assert_eq!(count(scenario, &["parse", "source_bytes_rehashed"]), 0);
}

fn assert_query_executions(
    scenario: &Value,
    merge: u64,
    rir: u64,
    semantic: u64,
    definitions: u64,
) {
    assert_eq!(count(scenario, &["queries", "merge_executions"]), merge);
    assert_eq!(count(scenario, &["queries", "rir_executions"]), rir);
    assert_eq!(
        count(scenario, &["queries", "semantic_executions"]),
        semantic
    );
    assert_eq!(
        count(scenario, &["queries", "definition_executions"]),
        definitions
    );
}

fn assert_semantic_work(scenario: &Value, binds: u64, bodies: u64, manifests: u64) {
    assert_eq!(
        count(scenario, &["semantic_work", "bind_invocations"]),
        binds,
        "{}",
        scenario["name"]
    );
    assert_eq!(
        count(scenario, &["semantic_work", "body_free_function_lookups"]),
        bodies
    );
    assert_eq!(
        count(scenario, &["semantic_work", "manifest_build_invocations"]),
        manifests
    );
}

fn assert_declaration_epoch_work(
    scenario: &Value,
    epochs: u64,
    indexes: u64,
    shell_predeclarations: u64,
    population_exports: u64,
    fallback_epochs: u64,
) {
    let work = &scenario["semantic_work"]["declaration_reuse"];
    assert_eq!(work["semantic_epochs_started"], epochs);
    assert_eq!(work["declaration_indexes_built"], indexes);
    assert_eq!(work["shell_predeclaration_epochs"], shell_predeclarations);
    assert_eq!(work["durable_cache_population_exports"], population_exports);
    assert_eq!(work["fallback_epochs_started"], fallback_epochs);
}

fn parse_config() -> Config {
    let mut config = Config {
        modules: DEFAULT_MODULES,
        warmup: DEFAULT_WARMUP,
        iterations: DEFAULT_ITERATIONS,
    };
    let mut args = env::args().skip(1);
    while let Some(arg) = args.next() {
        let value = args
            .next()
            .unwrap_or_else(|| usage(&format!("missing value for {arg}")));
        let value = value
            .parse::<usize>()
            .unwrap_or_else(|_| usage(&format!("invalid value for {arg}")));
        match arg.as_str() {
            "--modules" if value >= 2 => config.modules = value,
            "--warmup" => config.warmup = value,
            "--iterations" if value > 0 => config.iterations = value,
            _ => usage(&format!("unknown option or invalid value: {arg}")),
        }
    }
    config
}

fn usage(message: &str) -> ! {
    eprintln!("error: {message}");
    eprintln!("usage: rue-compiler-session-bench [--modules N] [--warmup N] [--iterations N]");
    process::exit(2)
}

fn main() {
    let config = parse_config();
    let fixture = Fixture::new(config.modules);
    for _ in 0..config.warmup {
        run_iteration(&fixture);
    }
    let iterations = (0..config.iterations)
        .map(|_| run_iteration(&fixture))
        .collect::<Vec<_>>();
    println!(
        "{}",
        json!({
            "schema_version": 4,
            "workload": "canonical_frontend_session_invalidation",
            "configuration": {
                "modules": config.modules,
                "warmup_iterations": config.warmup,
                "measured_iterations": config.iterations,
                "filesystem_discovery_during_updates": false,
                "backend_or_link": false,
            },
            "iterations": iterations,
        })
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_workload_is_a_structural_smoke_test() {
        let scenarios = run_iteration(&Fixture::new(4));
        assert_eq!(scenarios.len(), 12);
        assert!(
            scenarios
                .iter()
                .all(|scenario| scenario["wall_time_ns"].is_u64()
                    || (scenario["manifest_wall_time_ns"].is_u64()
                        && scenario["plan_wall_time_ns"].is_u64()))
        );
    }
}
