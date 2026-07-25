#![recursion_limit = "256"]

//! Reproducible in-process invalidation workload for `CompilerSession`.

use std::{collections::HashMap, env, process, sync::Arc, time::Instant};

#[cfg(test)]
use rue_air::{FrozenTypeInternPool, TypeKind};
use rue_compiler::unstable::{
    DependencyBaseline, DependencyIncompleteReason, DependencySurface, DiscoverySourceAssembler,
    FullInvalidationReason, InvalidationMetrics, InvalidationScope, MetricsSnapshot, ParseMetrics,
    prepare_stable_definitions, query_merge, semantic_parity_snapshot,
};
use rue_compiler::{
    AcceptedReadManifest, CompileOptions, CompilerSession, FileMetadataFingerprint,
    FrontendDiagnosticSnapshot, ImportDiscoveryContext, ImportObservationLedger, OptLevel,
    PhysicalFileIdentity, SemanticView, SourceMetadata, SourceSnapshot,
};
use rue_span::FileId;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

mod representative;

const DEFAULT_MODULES: usize = 128;
const DEFAULT_WARMUP: usize = 3;
const DEFAULT_ITERATIONS: usize = 10;

const AVAILABLE_CONTEXT_COUNTERS: &[(&str, &[&str])] = &[
    (
        "cold_body_preparations",
        &[
            "semantic_work",
            "per_body_declaration_context",
            "cold_body_preparations",
        ],
    ),
    (
        "declarations_inspected",
        &[
            "semantic_work",
            "per_body_declaration_context",
            "declarations_inspected",
        ],
    ),
    (
        "modules_registered",
        &[
            "semantic_work",
            "per_body_declaration_context",
            "modules_registered",
        ],
    ),
    (
        "rir_indexes_constructed",
        &[
            "semantic_work",
            "per_body_declaration_context",
            "rir_indexes_constructed",
        ],
    ),
    (
        "rir_instructions_visited",
        &[
            "semantic_work",
            "per_body_declaration_context",
            "rir_instructions_visited",
        ],
    ),
    (
        "durable_source_records_inspected",
        &[
            "semantic_work",
            "per_body_declaration_context",
            "durable_source_records_inspected",
        ],
    ),
    (
        "durable_source_records_copied",
        &[
            "semantic_work",
            "per_body_declaration_context",
            "durable_source_records_copied",
        ],
    ),
    (
        "shells_prepared",
        &[
            "semantic_work",
            "per_body_declaration_context",
            "shells_prepared",
        ],
    ),
    (
        "projections_performed",
        &[
            "semantic_work",
            "per_body_declaration_context",
            "projections_performed",
        ],
    ),
    (
        "semantics_installed",
        &[
            "semantic_work",
            "per_body_declaration_context",
            "semantics_installed",
        ],
    ),
    (
        "endpoints_installed",
        &[
            "semantic_work",
            "per_body_declaration_context",
            "endpoints_installed",
        ],
    ),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Config {
    modules: usize,
    warmup: usize,
    iterations: usize,
    representative: bool,
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
}

impl QueryCounts {
    fn from(work: &MetricsSnapshot) -> Self {
        Self {
            merge_executions: work.merge().executions,
            merge_reuses: work.merge().reuses,
            rir_executions: work.rir().executions,
            rir_reuses: work.rir().reuses,
            semantic_executions: work.semantic().executions,
            semantic_reuses: work.semantic().reuses,
            definition_executions: work.definitions().executions,
            definition_reuses: work.definitions().reuses,
            downstream_invalidations: work.downstream_invalidations(),
            semantic_entries_invalidated: work.semantic_entries_invalidated(),
            definition_entries_invalidated: work.definition_entries_invalidated(),
            dependency_manifest_executions: work.dependency_manifests().executions,
            dependency_manifest_reuses: work.dependency_manifests().reuses,
            invalidation_plan_executions: work.invalidation_plans().executions,
            invalidation_plan_reuses: work.invalidation_plans().reuses,
            declaration_reuse_plans: work.declaration_reuse_plans(),
            durable_records_compared: work.durable_records_compared(),
            durable_records_reused: work.durable_records_reused(),
            ordinary_declaration_resolutions_skipped: work
                .ordinary_declaration_resolutions_skipped(),
            durable_installs: work.durable_installs(),
            declaration_reuse_fallbacks: work.declaration_reuse_fallbacks(),
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
        })
    }
}

struct Fixture {
    modules: usize,
    base: SourceSnapshot,
    reachable_root_body_edit: SourceSnapshot,
    identity_edit: SourceSnapshot,
    syntax_error: SourceSnapshot,
    semantic_error: SourceSnapshot,
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
            semantic_error: snapshot(modules, Variant::SemanticError),
        }
    }
}

#[derive(Clone, Copy)]
enum Variant {
    Base,
    ReachableRootBodyEdit,
    IdentityEdit,
    SyntaxError,
    SemanticError,
}

struct CompletionFixture {
    direct: SourceSnapshot,
    direct_unrelated_edit: SourceSnapshot,
    direct_reachable_edit: SourceSnapshot,
    chain: SourceSnapshot,
    chain_reachable_edit: SourceSnapshot,
    semantic_error: SourceSnapshot,
    specialization: SourceSnapshot,
    specialization_unrelated_edit: SourceSnapshot,
}

#[derive(Clone, Copy)]
enum CompletionGraph {
    Direct,
    Chain,
}

impl CompletionFixture {
    fn new(functions: usize) -> Self {
        assert!(
            functions >= 4,
            "completion workload requires at least four functions"
        );
        Self {
            direct: completion_snapshot(functions, CompletionGraph::Direct, 0, false),
            direct_unrelated_edit: completion_snapshot(
                functions,
                CompletionGraph::Direct,
                1,
                false,
            ),
            direct_reachable_edit: completion_snapshot(
                functions,
                CompletionGraph::Direct,
                0,
                true,
            ),
            chain: completion_snapshot(functions, CompletionGraph::Chain, 0, false),
            chain_reachable_edit: completion_snapshot(
                functions,
                CompletionGraph::Chain,
                0,
                true,
            ),
            semantic_error: single_root_snapshot(completion_source(
                functions,
                CompletionGraph::Direct,
                0,
                false,
                true,
            )),
            specialization: single_root_snapshot(
                "fn choose(comptime n: i32) -> i32 { n }\nfn stable() -> i32 { 2 }\nfn dead() -> i32 { 0 }\nfn main() -> i32 { choose(40) + stable() }".to_owned(),
            ),
            specialization_unrelated_edit: single_root_snapshot(
                "fn choose(comptime n: i32) -> i32 { n }\nfn stable() -> i32 { 2 }\nfn dead() -> i32 { 1 }\nfn main() -> i32 { choose(40) + stable() }".to_owned(),
            ),
        }
    }
}

fn completion_snapshot(
    functions: usize,
    graph: CompletionGraph,
    dead_value: i32,
    reachable_edit: bool,
) -> SourceSnapshot {
    single_root_snapshot(completion_source(
        functions,
        graph,
        dead_value,
        reachable_edit,
        false,
    ))
}

fn completion_source(
    functions: usize,
    graph: CompletionGraph,
    dead_value: i32,
    reachable_edit: bool,
    semantic_error: bool,
) -> String {
    let leaves = functions - 1;
    let chain_len = leaves / 2 + leaves % 2;
    let mut source = String::new();
    for index in 0..leaves {
        let body = if index == 0 {
            usize::from(reachable_edit).to_string()
        } else if matches!(graph, CompletionGraph::Chain) && index < chain_len {
            format!("f{:03}() + 1", index - 1)
        } else {
            index.to_string()
        };
        source.push_str(&format!("fn f{index:03}() -> i32 {{ {body} }}\n"));
    }
    source.push_str(&format!("fn dead() -> i32 {{ {dead_value} }}\n"));
    if semantic_error {
        source.push_str("fn main() -> i32 { missing_name }\n");
    } else {
        let calls = match graph {
            CompletionGraph::Direct => (0..leaves).collect::<Vec<_>>(),
            CompletionGraph::Chain => {
                let mut calls = vec![chain_len - 1];
                calls.extend(chain_len..leaves);
                calls
            }
        };
        source.push_str("fn main() -> i32 { ");
        source.push_str(
            &calls
                .into_iter()
                .map(|index| format!("f{index:03}()"))
                .collect::<Vec<_>>()
                .join(" + "),
        );
        source.push_str(" }\n");
    }
    source
}

fn single_root_snapshot(source: String) -> SourceSnapshot {
    completion_discovery(Arc::new(source)).0
}

fn completion_discovery(
    source: Arc<String>,
) -> (SourceSnapshot, ImportDiscoveryContext, AcceptedReadManifest) {
    let context = ImportDiscoveryContext::new(1, "/bench", None, "rue-888-benchmark").unwrap();
    let source_len = source.len() as u64;
    let mut assembler = DiscoverySourceAssembler::new(
        context.clone(),
        "/bench/main.rue",
        "/bench/main.rue",
        PhysicalFileIdentity::new(1, 1),
        FileMetadataFingerprint::new(source_len, 0, 0),
        source,
    )
    .unwrap();
    (
        assembler.snapshot().unwrap(),
        context,
        assembler.accepted_read_manifest(),
    )
}

fn close_completion_discovery(session: &mut CompilerSession, source: &SourceSnapshot) {
    let expected_revision = source.source_revision().clone();
    let source_text = Arc::new(source.files().next().unwrap().source.to_owned());
    let (discovered, context, accepted_reads) = completion_discovery(source_text);
    assert_eq!(discovered.source_revision(), &expected_revision);
    session
        .stage_import_discovery(
            &discovered,
            context,
            accepted_reads.shared_slice(),
            ImportObservationLedger::default(),
        )
        .unwrap();
    session
        .close_import_discovery(ImportObservationLedger::default())
        .unwrap();
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
                if matches!(variant, Variant::SemanticError) {
                    "fn main() -> i32 { missing_name }".to_string()
                } else {
                    format!(
                        "fn main() -> i32 {{ {} }}",
                        usize::from(!matches!(variant, Variant::Base))
                    )
                }
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

fn parse_json(work: ParseMetrics) -> Value {
    json!({
        "lexer_invocations": work.lexer_invocations,
        "parser_invocations": work.parser_invocations,
        "lexed_bytes": work.lexed_bytes,
        "tokens": work.tokens,
        "modules_considered": work.modules_considered,
        "modules_reused": work.modules_reused,
        "modules_rebound": work.modules_rebound,
        "modules_reparsed": work.modules_reparsed,
        "source_text_clones": work.source_text_clones,
        "source_bytes_rehashed": work.source_bytes_rehashed,
    })
}

fn semantic_work_json(work: &MetricsSnapshot, from: usize) -> Value {
    work.semantic_work_json(from)
}

fn definition_work_json(work: &MetricsSnapshot, from: usize) -> Value {
    work.definition_work_json(from)
}

fn retention_json(work: &MetricsSnapshot) -> Value {
    json!({
        "diagnostic_entries": work.retention().diagnostic_entries,
        "diagnostic_source_attempts": work.retention().diagnostic_source_attempts,
        "diagnostic_source_bytes": work.retention().diagnostic_source_bytes,
        "dependency_manifests": work.retention().dependency_manifests,
        "invalidation_plans": work.retention().invalidation_plans,
    })
}

fn measure<F>(session: &mut CompilerSession, operation: F) -> Value
where
    F: FnOnce(&mut CompilerSession) -> ParseMetrics,
{
    let before_metrics = session.unstable_metrics();
    let before = QueryCounts::from(&before_metrics);
    let semantic_records = before_metrics.semantic_record_count();
    let definition_records = before_metrics.definition_record_count();
    let started = Instant::now();
    let parse = operation(session);
    let elapsed = started.elapsed();
    let work = session.unstable_metrics();
    let query_delta = QueryCounts::from(&work).delta(before);
    // Successful updates replace the current-revision record vectors. Treat
    // that reset as a new zero-based measurement window.
    let semantic_records = if work.semantic_record_count() < semantic_records
        || (query_delta.semantic_executions > 0 && work.semantic_record_count() <= semantic_records)
    {
        0
    } else {
        semantic_records
    };
    let definition_records = if work.definition_record_count() < definition_records
        || (query_delta.definition_executions > 0
            && work.definition_record_count() <= definition_records)
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
            "definition_shards_indexed": work.merge_metrics().definition_shards_indexed,
            "definition_shards_reused": work.merge_metrics().definition_shards_reused,
            "definition_shards_rebuilt": work.merge_metrics().definition_shards_rebuilt,
        },
        "semantic_work": semantic_work_json(&work, semantic_records),
        "definition_work": definition_work_json(&work, definition_records),
        "retention": retention_json(&work),
    })
}

fn invalidation_plan_json(plan: &InvalidationMetrics) -> Value {
    let mut dependency_blockers = Vec::new();
    let (scope, reasons) = match plan.scope() {
        InvalidationScope::Full { reasons } => (
            "full",
            reasons
                .iter()
                .map(|reason| match reason {
                    FullInvalidationReason::RootChanged => "root_changed",
                    FullInvalidationReason::ModuleImportsChanged => "module_imports_changed",
                    FullInvalidationReason::TargetChanged => "target_changed",
                    FullInvalidationReason::PreviewFeaturesChanged => "preview_features_changed",
                    FullInvalidationReason::IncompleteDefinitionUniverse => {
                        "incomplete_definition_universe"
                    }
                    FullInvalidationReason::IncompleteDependencyGraph(blockers) => {
                        dependency_blockers.extend(blockers.iter().map(|blocker| {
                            json!({
                                "owner": blocker.owner(),
                                "surface": dependency_surface_name(blocker.surface()),
                                "reason": dependency_reason_name(blocker.reason()),
                            })
                        }));
                        "incomplete_dependency_graph"
                    }
                })
                .collect::<Vec<_>>(),
        ),
        InvalidationScope::Incremental => ("incremental", Vec::new()),
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

fn dependency_surface_name(surface: DependencySurface) -> &'static str {
    match surface {
        DependencySurface::BodyOwner => "body_owner",
        DependencySurface::FreeFunctionCall => "free_function_call",
        DependencySurface::NonGenericNamedMethodCall => "non_generic_named_method_call",
        DependencySurface::GenericNamedMethodCall => "generic_named_method_call",
        DependencySurface::NamedDestructorCall => "named_destructor_call",
        DependencySurface::ImplicitNamedDestructor => "implicit_named_destructor",
        DependencySurface::DeclarationType => "declaration_type",
        DependencySurface::DeclarationTypeCallHead => "declaration_type_call_head",
        DependencySurface::SupportedTypeCallHead => "supported_type_call_head",
        DependencySurface::NamedValueConst => "named_value_const",
    }
}

fn dependency_reason_name(reason: DependencyIncompleteReason) -> &'static str {
    match reason {
        DependencyIncompleteReason::AnonymousBodyOwnerUnavailable => {
            "anonymous_body_owner_unavailable"
        }
        DependencyIncompleteReason::CallerEndpointUnavailable => "caller_endpoint_unavailable",
        DependencyIncompleteReason::GenericSubstitutionIdentityUnavailable => {
            "generic_substitution_identity_unavailable"
        }
        DependencyIncompleteReason::DestructorEndpointUnavailable => {
            "destructor_endpoint_unavailable"
        }
        DependencyIncompleteReason::AnonymousDropOwnerUnavailable => {
            "anonymous_drop_owner_unavailable"
        }
        DependencyIncompleteReason::ResolvedTypeIdentityUnavailable => {
            "resolved_type_identity_unavailable"
        }
        DependencyIncompleteReason::TypeCallHeadIdentityUnavailable => {
            "type_call_head_identity_unavailable"
        }
        DependencyIncompleteReason::UnsupportedDynamicTypeCallHead => {
            "unsupported_dynamic_type_call_head"
        }
        DependencyIncompleteReason::ConstEndpointUnavailable => "const_endpoint_unavailable",
    }
}

fn measure_manifest_plan(
    session: &mut CompilerSession,
    source: &SourceSnapshot,
    previous: Option<&Arc<DependencyBaseline>>,
    options: &CompileOptions,
) -> (Value, Arc<DependencyBaseline>) {
    let before = QueryCounts::from(&session.unstable_metrics());
    let update = session.update(source);
    let parse = update.unstable_metrics();
    update.into_result().unwrap();
    let manifest_started = Instant::now();
    let current = session.unstable_dependency_baseline(options, None).unwrap();
    let manifest_elapsed = manifest_started.elapsed();
    let manifest_work = current.unstable_metrics();
    let after_manifest = QueryCounts::from(&session.unstable_metrics());
    let plan_started = Instant::now();
    let plan = session
        .unstable_invalidation_metrics(previous.unwrap_or(&current), &current)
        .unwrap();
    let plan_elapsed = plan_started.elapsed();
    let after_plan = QueryCounts::from(&session.unstable_metrics());
    (
        json!({
            "manifest_wall_time_ns": manifest_elapsed.as_nanos(),
            "plan_wall_time_ns": plan_elapsed.as_nanos(),
            "parse": parse_json(parse),
            "manifest_queries": after_manifest.delta(before).json(),
            "manifest_work": {
                "body_owner_events_translated": manifest_work.body_owner_events_translated,
                "body_named_events_translated": manifest_work.body_named_events_translated,
                "body_dependency_records_built": manifest_work.body_dependency_records_built,
                "extra_rir_instructions_visited": manifest_work.extra_rir_instructions_visited,
                "durable_bodies": manifest_work.durable_bodies,
            },
            "planner_queries": after_plan.delta(after_manifest).json(),
            "plan": invalidation_plan_json(&plan),
            "retention": retention_json(&session.unstable_metrics()),
        }),
        current,
    )
}

fn measured_semantic(
    session: &mut CompilerSession,
    source: &SourceSnapshot,
    options: &CompileOptions,
) -> (Value, Arc<SemanticView>) {
    let mut output = None;
    let value = measure(session, |session| {
        let update = session.update(source);
        let parse = update.unstable_metrics();
        update.into_result().unwrap();
        output = Some(session.semantic(options).unwrap());
        parse
    });
    (value, output.unwrap())
}

#[cfg(test)]
fn type_pool_snapshot(pool: &FrozenTypeInternPool) -> Vec<String> {
    pool.all_types()
        .map(|ty| match ty.kind() {
            TypeKind::Struct(id) => format!("struct:{:?}", pool.struct_def(id)),
            TypeKind::Enum(id) => format!("enum:{:?}", pool.enum_def(id)),
            TypeKind::Array(id) => {
                let (element, len) = pool.array_def(id);
                format!("array:{element:?}:{len}")
            }
            TypeKind::PtrConst(id) => format!("ptr_const:{:?}", pool.ptr_const_def(id)),
            TypeKind::PtrMut(id) => format!("ptr_mut:{:?}", pool.ptr_mut_def(id)),
            _ => unreachable!("type pool stores only composite types"),
        })
        .collect()
}

fn assert_diagnostic_parity(
    reused: Option<&Arc<FrontendDiagnosticSnapshot>>,
    cold: Option<&Arc<FrontendDiagnosticSnapshot>>,
) {
    assert_eq!(reused.is_some(), cold.is_some());
    let (Some(reused), Some(cold)) = (reused, cold) else {
        return;
    };
    assert_eq!(reused.source_revision(), cold.source_revision());
    assert_eq!(
        format!("{:?}", reused.stage()),
        format!("{:?}", cold.stage())
    );
    assert_eq!(
        format!("{:?}", reused.errors()),
        format!("{:?}", cold.errors())
    );
    assert_eq!(
        format!("{:?}", reused.warnings()),
        format!("{:?}", cold.warnings())
    );
    assert_eq!(reused.is_success(), cold.is_success());
}

fn assert_cold_reused_parity(
    reused_session: &mut CompilerSession,
    reused: &SemanticView,
    source: &SourceSnapshot,
    options: &CompileOptions,
) -> Value {
    assert_cold_reused_parity_with_discovery(
        reused_session,
        reused,
        source,
        options,
        None,
        |_, _| {},
        close_completion_discovery,
    )
}

fn assert_cold_reused_parity_with_discovery<F, G>(
    reused_session: &mut CompilerSession,
    reused: &SemanticView,
    source: &SourceSnapshot,
    options: &CompileOptions,
    std_dir: Option<&str>,
    prepare_semantic: F,
    prepare_executable: G,
) -> Value
where
    F: Fn(&mut CompilerSession, &SourceSnapshot),
    G: Fn(&mut CompilerSession, &SourceSnapshot),
{
    prepare_semantic(reused_session, source);
    let mut cold_session = CompilerSession::new();
    cold_session.update(source).into_result().unwrap();
    prepare_semantic(&mut cold_session, source);
    let cold = cold_session.semantic(options).unwrap();
    let cold_semantic_work = semantic_work_json(&cold_session.unstable_metrics(), 0);
    let cold_count = |path: &[&str]| count(&cold_semantic_work, path);
    assert!(cold_count(&["bodies_attempted"]) > 0);
    assert_eq!(
        cold_count(&["bodies_attempted"]),
        cold_count(&["bodies_succeeded"])
    );
    assert_eq!(cold_count(&["durable_bodies", "candidate_comparisons"]), 0);
    assert_eq!(cold_count(&["durable_bodies", "reused_bodies"]), 0);
    assert_eq!(cold_count(&["cfg", "reuse_candidates"]), 0);
    assert_eq!(cold_count(&["cfg", "reuses"]), 0);
    assert_eq!(
        cold_count(&["cfg", "builds_attempted"]),
        cold_count(&["cfg", "builds_succeeded"])
    );
    assert_eq!(
        semantic_parity_snapshot(reused),
        semantic_parity_snapshot(&cold)
    );

    let reused_manifest = reused_session
        .unstable_dependency_baseline(options, std_dir)
        .unwrap();
    let cold_manifest = cold_session
        .unstable_dependency_baseline(options, std_dir)
        .unwrap();
    // Dependency queries may publish import-stage diagnostics on only the
    // cold side when the reused side already retained the manifest. Re-query
    // the canonical semantic artifact on both sides before comparing the
    // user-visible semantic diagnostic snapshot.
    reused_session.semantic(options).unwrap();
    cold_session.semantic(options).unwrap();

    assert_diagnostic_parity(
        reused_session.latest_diagnostics(),
        cold_session.latest_diagnostics(),
    );
    assert_diagnostic_parity(
        reused_session.latest_successful_diagnostics(),
        cold_session.latest_successful_diagnostics(),
    );
    assert_diagnostic_parity(
        reused_session.last_good_semantic_diagnostics(),
        cold_session.last_good_semantic_diagnostics(),
    );

    assert_eq!(reused_manifest.input(), cold_manifest.input());
    assert_eq!(reused_manifest.imports(), cold_manifest.imports());
    macro_rules! assert_manifest_field {
        ($field:ident) => {
            assert_eq!(
                format!("{:?}", reused_manifest.$field()),
                format!("{:?}", cold_manifest.$field()),
                "stable manifest field {} differs",
                stringify!($field)
            );
        };
    }
    assert_manifest_field!(definitions);
    assert_manifest_field!(definition_fingerprints);
    assert_manifest_field!(module_imports);
    assert_manifest_field!(free_function_dependencies);
    assert_manifest_field!(named_method_dependencies);
    assert_manifest_field!(named_destructor_dependencies);
    assert_manifest_field!(implicit_named_destructor_dependencies);
    assert_manifest_field!(declaration_type_dependencies);
    assert_manifest_field!(declaration_type_call_head_dependencies);
    assert_manifest_field!(builtin_type_call_head_inputs);
    assert_manifest_field!(named_const_dependencies);
    assert_manifest_field!(body_dependencies);
    assert_manifest_field!(body_dependency_blockers);
    assert_manifest_field!(dependency_blockers);
    assert_eq!(
        reused_manifest.unstable_durable_artifact_status(),
        cold_manifest.unstable_durable_artifact_status()
    );
    assert_eq!(
        reused_manifest.free_function_caller_dependencies_complete(),
        cold_manifest.free_function_caller_dependencies_complete()
    );
    assert_eq!(
        reused_manifest.non_generic_named_method_dependencies_complete(),
        cold_manifest.non_generic_named_method_dependencies_complete()
    );
    assert_eq!(
        reused_manifest.generic_named_method_dependencies_complete(),
        cold_manifest.generic_named_method_dependencies_complete()
    );
    assert_eq!(
        reused_manifest.named_destructor_dependencies_complete(),
        cold_manifest.named_destructor_dependencies_complete()
    );
    assert_eq!(
        reused_manifest.implicit_named_destructor_dependencies_complete(),
        cold_manifest.implicit_named_destructor_dependencies_complete()
    );
    assert_eq!(
        reused_manifest.declaration_type_dependencies_complete(),
        cold_manifest.declaration_type_dependencies_complete()
    );
    assert_eq!(
        reused_manifest.declaration_type_call_head_dependencies_complete(),
        cold_manifest.declaration_type_call_head_dependencies_complete()
    );
    assert_eq!(
        reused_manifest.supported_type_call_heads_complete(),
        cold_manifest.supported_type_call_heads_complete()
    );
    assert_eq!(
        reused_manifest.named_value_const_dependencies_complete(),
        cold_manifest.named_value_const_dependencies_complete()
    );
    assert_eq!(
        reused_manifest.semantic_dependency_graph_complete(),
        cold_manifest.semantic_dependency_graph_complete()
    );
    assert_eq!(
        reused_manifest.definition_universe_complete(),
        cold_manifest.definition_universe_complete()
    );

    prepare_executable(reused_session, source);
    prepare_executable(&mut cold_session, source);
    let reused_executable = reused_session.executable(options).unwrap();
    let cold_executable = cold_session.executable(options).unwrap();
    assert_eq!(reused_executable.elf, cold_executable.elf);
    assert_eq!(
        format!("{:?}", reused_executable.warnings),
        format!("{:?}", cold_executable.warnings)
    );

    let executable_sha256 = format!("{:x}", Sha256::digest(&reused_executable.elf));
    json!({
        "schema_version": 1,
        "status": "pass",
        "comparison": "cold_vs_reused_in_process",
        "comparison_completed": true,
        "public_semantic_cfg_artifacts": "exact",
        "public_type_universe": "exact_ordered_entries",
        "durable_specialized_body_payloads": "exact",
        "durable_ordinary_body_manifest_artifacts": "exact",
        "session_private_durable_cfg_cache": "not_exposed_compared_via_public_cfgs_and_emitted_output",
        "diagnostics": "exact",
        "warnings": "exact",
        "stable_identities": "exact",
        "dependency_records": "exact",
        "emitted_output": "byte_exact",
        "emitted_output_sha256": executable_sha256,
        "emitted_output_size_bytes": reused_executable.elf.len(),
        "work_counters": "reused_and_cold_serialized_with_structural_assertions",
        "cold_semantic_work": cold_semantic_work,
    })
}

fn with_parity(mut scenario: Value, evidence: Value) -> Value {
    scenario["evidence_schema"] = json!({
        "version": 2,
        "differential_parity_version": 1,
        "locality_work_version": 2,
    });
    scenario["differential_parity"] = evidence;
    scenario["required_vs_reused_work"] = work_projection(&scenario);
    scenario
}

/// Stable production-counter projection consumed by the value-audit runner.
/// The detailed counter ledger remains available beside this projection. Each
/// numeric field is copied from a production counter; an unavailable dimension
/// is an explicit reason object rather than a duplicated or inferred value.
/// `required` is the audit's uniform demand shape, not an additional production
/// counter. Identity sets are currently not exposed by this benchmark, so that
/// absence is explicit rather than inferred.
fn work_projection(scenario: &Value) -> Value {
    macro_rules! required_count {
        ($path:expr) => {
            match try_count(scenario, $path) {
                Some(value) => value,
                None => {
                    return json!({
                        "schema_version": 2,
                        "status": "unsupported",
                        "reason": "production counter path was absent",
                    });
                }
            }
        };
    }
    macro_rules! unavailable {
        ($reason:expr) => {
            json!({
                "available": false,
                "reason": $reason,
            })
        };
    }
    let mut projection = json!({
        "schema_version": 2,
        "counter_source": "production_metrics",
        "exact_identity_sets": {
            "available": false,
            "reason": "CompilerSession does not expose stable recompute/reuse identity sets in this protocol"
        },
        "modules": {
            "computed": required_count!(&["parse", "modules_reparsed"]),
            "required": required_count!(&["parse", "modules_reparsed"]),
            "reused": required_count!(&["parse", "modules_reused"]),
            "invalidated": unavailable!("CompilerSession exposes no module invalidation counter"),
        },
        "rir": {
            "computed": required_count!(&["queries", "rir_executions"]),
            "required": required_count!(&["queries", "rir_executions"]),
            "reused": required_count!(&["queries", "rir_reuses"]),
            "invalidated": unavailable!("CompilerSession exposes no RIR invalidation counter"),
        },
        "declarations": {
            "computed": required_count!(&["semantic_work", "declaration_resolution_invocations"]),
            "required": required_count!(&["semantic_work", "declaration_resolution_invocations"]),
            "invalidated": required_count!(&["queries", "definition_entries_invalidated"]),
            "reused": required_count!(&["semantic_work", "declaration_reuse", "durable_records_reused"]),
        },
        "durable_source": {
            "computed": required_count!(&["semantic_work", "per_body_declaration_context", "durable_source_records_copied"]),
            "required": required_count!(&["semantic_work", "per_body_declaration_context", "durable_source_records_copied"]),
            "invalidated": unavailable!("CompilerSession exposes no durable-source invalidation counter"),
            "reused": unavailable!("durable-body reuse is not a durable-source reuse counter"),
        },
        "semantic_bodies": {
            "computed": required_count!(&["semantic_work", "body_analyses_computed"]),
            "invalidated": required_count!(&["semantic_work", "body_analyses_invalidated"]),
            "required": required_count!(&["semantic_work", "bodies_attempted"]),
            "reused": required_count!(&["semantic_work", "body_analyses_reused"]),
        },
        "cfgs": {
            "computed": required_count!(&["semantic_work", "cfg", "builds_attempted"]),
            "required": required_count!(&["semantic_work", "cfg", "builds_attempted"]),
            "reused": required_count!(&["semantic_work", "cfg", "reuses"]),
            "invalidated": unavailable!("CompilerSession exposes no CFG invalidation counter"),
        },
        "semantic_queries": {
            "computed": required_count!(&["queries", "semantic_executions"]),
            "invalidated": required_count!(&["queries", "semantic_entries_invalidated"]),
            "required": required_count!(&["queries", "semantic_executions"]),
            "reused": required_count!(&["queries", "semantic_reuses"]),
        },
    });
    for (name, path) in AVAILABLE_CONTEXT_COUNTERS {
        let computed = match try_count(scenario, path) {
            Some(value) => value,
            None => {
                return json!({
                    "schema_version": 2,
                    "status": "unsupported",
                    "reason": format!("production counter path was absent: {name}"),
                });
            }
        };
        projection[*name] = json!({
            "computed": computed,
            "required": computed,
            "invalidated": unavailable!(format!("{name} has no invalidation counter")),
            "reused": unavailable!(format!("{name} has no reuse counter")),
        });
    }
    projection
}

fn completion_workloads(functions: usize) -> Vec<Value> {
    let fixture = CompletionFixture::new(functions);
    let o0 = CompileOptions {
        opt_level: OptLevel::O0,
        ..CompileOptions::default()
    };
    let o1 = CompileOptions {
        opt_level: OptLevel::O1,
        ..CompileOptions::default()
    };
    let mut scenarios = Vec::new();

    let mut noop = CompilerSession::new();
    let (cold_value, _) = measured_semantic(&mut noop, &fixture.direct, &o1);
    scenarios.push(named("completion_cold_n", cold_value));
    let (noop_value, noop_output) = measured_semantic(&mut noop, &fixture.direct, &o1);
    let parity = assert_cold_reused_parity(&mut noop, &noop_output, &fixture.direct, &o1);
    scenarios.push(with_parity(
        named("completion_exact_noop", noop_value),
        parity,
    ));

    let mut unrelated = CompilerSession::new();
    measured_semantic(&mut unrelated, &fixture.direct, &o1);
    let (unrelated_value, unrelated_output) =
        measured_semantic(&mut unrelated, &fixture.direct_unrelated_edit, &o1);
    let parity = assert_cold_reused_parity(
        &mut unrelated,
        &unrelated_output,
        &fixture.direct_unrelated_edit,
        &o1,
    );
    scenarios.push(with_parity(
        named("completion_unrelated_edit", unrelated_value),
        parity,
    ));

    let mut changed = CompilerSession::new();
    measured_semantic(&mut changed, &fixture.direct, &o1);
    let (changed_value, changed_output) =
        measured_semantic(&mut changed, &fixture.direct_reachable_edit, &o1);
    let parity = assert_cold_reused_parity(
        &mut changed,
        &changed_output,
        &fixture.direct_reachable_edit,
        &o1,
    );
    scenarios.push(with_parity(
        named("completion_changed_reachable_body", changed_value),
        parity,
    ));

    let mut closure = CompilerSession::new();
    measured_semantic(&mut closure, &fixture.chain, &o1);
    let (closure_value, closure_output) =
        measured_semantic(&mut closure, &fixture.chain_reachable_edit, &o1);
    let parity = assert_cold_reused_parity(
        &mut closure,
        &closure_output,
        &fixture.chain_reachable_edit,
        &o1,
    );
    scenarios.push(with_parity(
        named("completion_reverse_closure", closure_value),
        parity,
    ));

    for (name, options) in [("completion_cfg_o0", o0), ("completion_cfg_o1", o1.clone())] {
        let mut session = CompilerSession::new();
        measured_semantic(&mut session, &fixture.direct, &options);
        let (value, output) =
            measured_semantic(&mut session, &fixture.direct_unrelated_edit, &options);
        let parity = assert_cold_reused_parity(
            &mut session,
            &output,
            &fixture.direct_unrelated_edit,
            &options,
        );
        scenarios.push(with_parity(named(name, value), parity));
    }

    let mut specialization = CompilerSession::new();
    measured_semantic(&mut specialization, &fixture.specialization, &o1);
    let (specialization_value, specialization_output) = measured_semantic(
        &mut specialization,
        &fixture.specialization_unrelated_edit,
        &o1,
    );
    let parity = assert_cold_reused_parity(
        &mut specialization,
        &specialization_output,
        &fixture.specialization_unrelated_edit,
        &o1,
    );
    scenarios.push(with_parity(
        named("completion_specialization_reuse", specialization_value),
        parity,
    ));

    let mut recovery = CompilerSession::new();
    measured_semantic(&mut recovery, &fixture.direct, &o1);
    let failed = measure(&mut recovery, |session| {
        let update = session.update(&fixture.semantic_error);
        let parse = update.unstable_metrics();
        update.into_result().unwrap();
        session.semantic(&o1).unwrap_err();
        parse
    });
    scenarios.push(named("completion_failed_semantic", failed));
    let (recovery_value, recovery_output) =
        measured_semantic(&mut recovery, &fixture.direct_unrelated_edit, &o1);
    let parity = assert_cold_reused_parity(
        &mut recovery,
        &recovery_output,
        &fixture.direct_unrelated_edit,
        &o1,
    );
    scenarios.push(with_parity(
        named("completion_failure_recovery", recovery_value),
        parity,
    ));

    assert_completion_structure(&scenarios, functions);
    scenarios
}

fn run_iteration(fixture: &Fixture) -> Vec<Value> {
    let options = CompileOptions::default();
    let mut session = CompilerSession::new();
    let mut scenarios = Vec::new();

    scenarios.push(named(
        "cold",
        measure(&mut session, |session| {
            let parse = session.update(&fixture.base).unstable_metrics();
            query_merge(session).unwrap();
            session.rir().unwrap();
            session.semantic(&options).unwrap();
            parse
        }),
    ));
    scenarios.push(named(
        "exact_noop",
        measure(&mut session, |session| {
            let parse = session.update(&fixture.base).unstable_metrics();
            query_merge(session).unwrap();
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
            let parse = update.unstable_metrics();
            update.into_result().unwrap();
            query_merge(session).unwrap();
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
            let parse = update.unstable_metrics();
            update.into_result().unwrap();
            query_merge(session).unwrap();
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
            let parse = update.unstable_metrics();
            query_merge(session).unwrap();
            session.rir().unwrap();
            session.semantic(&options).unwrap();
            parse
        }),
    ));
    scenarios.push(named(
        "syntax_recovery",
        measure(&mut session, |session| {
            let parse = session.update(&fixture.identity_edit).unstable_metrics();
            query_merge(session).unwrap();
            session.rir().unwrap();
            session.semantic(&options).unwrap();
            parse
        }),
    ));
    scenarios.push(named(
        "failed_semantic_edit",
        measure(&mut session, |session| {
            let update = session.update(&fixture.semantic_error);
            assert!(update.downstream_invalidated());
            let parse = update.unstable_metrics();
            update.into_result().unwrap();
            session.semantic(&options).unwrap_err();
            parse
        }),
    ));
    scenarios.push(named(
        "semantic_recovery",
        measure(&mut session, |session| {
            let parse = session.update(&fixture.identity_edit).unstable_metrics();
            session.semantic(&options).unwrap();
            parse
        }),
    ));

    let mut stable = CompilerSession::new();
    scenarios.push(named(
        "stable_definitions_cold",
        measure(&mut stable, |session| {
            let parse = session.update(&fixture.base).unstable_metrics();
            prepare_stable_definitions(session, &options).unwrap();
            parse
        }),
    ));
    scenarios.push(named(
        "stable_definitions_reuse",
        measure(&mut stable, |session| {
            prepare_stable_definitions(session, &options).unwrap();
            ParseMetrics::default()
        }),
    ));
    let mut planner = CompilerSession::new();
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
    scenarios.extend(completion_workloads(fixture.modules));
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

fn try_count(value: &Value, path: &[&str]) -> Option<u64> {
    path.iter()
        .try_fold(value, |value, key| value.get(*key))
        .and_then(Value::as_u64)
}

fn assert_structure(scenarios: &[Value], modules: usize) {
    for scenario in scenarios {
        assert!(count(scenario, &["retention", "diagnostic_entries"]) < scenarios.len() as u64);
        assert!(count(scenario, &["retention", "invalidation_plans"]) < scenarios.len() as u64);
        assert!(
            count(scenario, &["retention", "dependency_manifests"])
                <= (scenarios.len() * 2 + 1) as u64
        );
    }
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
    assert_semantic_work(cold, 1, 0, 1);
    assert_declaration_epoch_work(cold, 1, 1, 1, 0, 0);
    assert_eq!(
        cold["semantic_work"]["declaration_reuse"]["plan_executions"],
        1
    );

    let noop = get("exact_noop");
    assert_reuse_parse_is_all_zero(noop);
    assert_query_executions(noop, 0, 0, 0, 0);
    assert_eq!(count(noop, &["queries", "merge_reuses"]), 1);
    // Semantic validates through its recorded RIR dependency, so the explicit
    // RIR request is the only query-store lookup on an exact no-op.
    assert_eq!(count(noop, &["queries", "rir_reuses"]), 1);
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
    assert_semantic_work(root_edit, 1, 0, 1);
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

    let identity = get("module_identity_change");
    // ParseModule is keyed by logical module identity. Renaming a module
    // therefore installs one new exact syntax leaf instead of rebinding the
    // former module's parser-owned artifact under a different identity.
    assert_eq!(count(identity, &["parse", "modules_rebound"]), 0);
    assert_eq!(count(identity, &["parse", "modules_reparsed"]), 1);
    assert_eq!(count(identity, &["parse", "lexer_invocations"]), 1);
    assert_eq!(count(identity, &["parse", "parser_invocations"]), 1);
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
    assert_eq!(count(failed, &["queries", "rir_reuses"]), 1);
    assert_eq!(count(failed, &["queries", "downstream_invalidations"]), 0);
    assert_eq!(count(failed, &["queries", "semantic_reuses"]), 1);

    let recovery = get("syntax_recovery");
    assert_reuse_parse_is_all_zero(recovery);
    assert_query_executions(recovery, 0, 0, 0, 0);
    assert_eq!(count(recovery, &["queries", "semantic_reuses"]), 1);

    let failed_semantic = get("failed_semantic_edit");
    assert_query_executions(failed_semantic, 1, 1, 1, 0);
    assert_eq!(
        count(failed_semantic, &["semantic_work", "failed_requests"]),
        1
    );
    assert_eq!(
        count(
            failed_semantic,
            &["semantic_work", "failure_phases", "body_analysis"]
        ),
        1
    );
    assert_eq!(
        count(failed_semantic, &["semantic_work", "bodies_attempted"]),
        1
    );
    assert_eq!(
        count(failed_semantic, &["semantic_work", "bodies_succeeded"]),
        0
    );
    assert_eq!(
        count(failed_semantic, &["semantic_work", "bodies_failed"]),
        1
    );

    let semantic_recovery = get("semantic_recovery");
    assert_query_executions(semantic_recovery, 0, 0, 0, 0);
    assert_eq!(count(semantic_recovery, &["queries", "semantic_reuses"]), 1);
    assert_eq!(
        count(
            semantic_recovery,
            &[
                "semantic_work",
                "declaration_reuse",
                "ordinary_declaration_resolutions_skipped"
            ]
        ),
        0,
        "recovery must restore the retained exact terminal without executing semantics"
    );

    let stable_cold = get("stable_definitions_cold");
    assert_query_executions(stable_cold, 1, 1, 1, 1);
    assert_semantic_work(stable_cold, 1, 0, 1);
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
    assert_eq!(
        count(
            plan_cold,
            &["manifest_work", "body_owner_events_translated"]
        ),
        0
    );
    assert_eq!(
        count(
            plan_cold,
            &["manifest_work", "body_dependency_records_built"]
        ),
        0
    );
    assert_eq!(
        count(
            plan_cold,
            &["manifest_work", "body_named_events_translated"]
        ),
        0
    );
    assert_eq!(
        count(
            plan_cold,
            &["manifest_work", "extra_rir_instructions_visited"]
        ),
        0
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
    assert_eq!(count(plan_identity, &["parse", "modules_rebound"]), 0);
    assert_eq!(count(plan_identity, &["parse", "modules_reparsed"]), 1);
    assert_eq!(count(plan_identity, &["parse", "lexer_invocations"]), 1);
    assert_eq!(count(plan_identity, &["parse", "parser_invocations"]), 1);
    assert_production_incremental_plan(plan_identity, 1, 0, 2, modules - 1, modules - 1, 2);
}

fn assert_completion_structure(scenarios: &[Value], functions: usize) {
    let get = |name: &str| {
        scenarios
            .iter()
            .find(|value| value["name"] == name)
            .unwrap()
    };
    let body = |scenario: &Value, field: &str| {
        count(scenario, &["semantic_work", "durable_bodies", field])
    };
    let cfg = |scenario: &Value, field: &str| count(scenario, &["semantic_work", "cfg", field]);

    let cold = get("completion_cold_n");
    assert_eq!(
        count(cold, &["semantic_work", "bodies_attempted"]),
        functions as u64
    );
    assert_eq!(body(cold, "export_attempts"), functions as u64);
    assert_eq!(body(cold, "export_successes"), functions as u64);
    assert_eq!(body(cold, "export_rejections"), 0);
    assert_eq!(body(cold, "conversion_attempts"), functions as u64);
    assert_eq!(body(cold, "conversion_completions"), functions as u64);
    assert_eq!(body(cold, "conversion_failures"), 0);
    assert_eq!(body(cold, "candidate_comparisons"), 0);
    assert_eq!(body(cold, "candidate_fallbacks"), 0);
    assert_eq!(body(cold, "projection_attempts"), 0);
    assert_eq!(body(cold, "import_attempts"), functions as u64);
    assert_eq!(body(cold, "import_successes"), functions as u64);
    assert_eq!(body(cold, "import_failures"), 0);
    assert_eq!(body(cold, "atomic_discards"), 0);
    assert_eq!(body(cold, "reused_bodies"), 0);
    assert_eq!(body(cold, "skipped_body_analyses"), 0);
    assert_eq!(count(cold, &["semantic_work", "bind_invocations"]), 1);
    assert_eq!(
        count(
            cold,
            &["semantic_work", "declaration_resolution_invocations"]
        ),
        0
    );
    assert_eq!(
        count(cold, &["semantic_work", "declaration_resolution_failures"]),
        0
    );
    assert_eq!(
        count(
            cold,
            &["semantic_work", "body_readiness_finalization_invocations"]
        ),
        1
    );
    assert_eq!(
        count(cold, &["semantic_work", "bodies_succeeded"]),
        functions as u64
    );
    assert_eq!(count(cold, &["semantic_work", "bodies_failed"]), 0);
    assert_eq!(cfg(cold, "builds_attempted"), functions as u64);
    assert_eq!(cfg(cold, "builds_succeeded"), functions as u64);
    assert_eq!(cfg(cold, "builds_failed"), 0);
    assert_eq!(cfg(cold, "optimization_attempts"), functions as u64);
    assert_eq!(cfg(cold, "optimization_completions"), functions as u64);
    assert_eq!(cfg(cold, "optimized_level_attempts"), functions as u64);
    assert_eq!(cfg(cold, "reuse_candidates"), 0);
    assert_eq!(cfg(cold, "fallbacks"), 0);
    assert_eq!(cfg(cold, "export_attempts"), functions as u64);
    assert_eq!(cfg(cold, "export_successes"), functions as u64);
    assert_eq!(cfg(cold, "export_rejections"), 0);
    assert_eq!(
        count(
            cold,
            &[
                "semantic_work",
                "declaration_reuse",
                "durable_cache_population_exports"
            ]
        ),
        0,
        "query-owned declarations require no post-analysis cache export"
    );
    assert_declaration_epoch_work(cold, 1, 1, 1, 0, 0);

    let noop = get("completion_exact_noop");
    for name in [
        "completion_exact_noop",
        "completion_unrelated_edit",
        "completion_changed_reachable_body",
        "completion_reverse_closure",
    ] {
        assert_eq!(get(name)["differential_parity"]["schema_version"], 1);
        assert_eq!(get(name)["differential_parity"]["status"], "pass");
        assert_eq!(
            get(name)["differential_parity"]["comparison_completed"],
            true
        );
        let projection = &get(name)["required_vs_reused_work"];
        assert_eq!(projection["schema_version"], 2);
        assert_eq!(projection["counter_source"], "production_metrics");
        assert!(projection["exact_identity_sets"]["available"].is_boolean());
        for artifact in [
            "modules",
            "rir",
            "declarations",
            "durable_source",
            "semantic_bodies",
            "cfgs",
            "semantic_queries",
            "cold_body_preparations",
            "declarations_inspected",
            "modules_registered",
            "rir_indexes_constructed",
            "rir_instructions_visited",
            "durable_source_records_inspected",
            "durable_source_records_copied",
            "shells_prepared",
            "projections_performed",
            "semantics_installed",
            "endpoints_installed",
        ] {
            assert!(
                projection[artifact]["required"].is_u64(),
                "{artifact} projection is missing required: {projection:?}"
            );
            assert!(
                projection[artifact]["computed"].is_u64(),
                "{artifact} projection is missing computed: {projection:?}"
            );
            for field in ["reused", "invalidated"] {
                let value = &projection[artifact][field];
                assert!(
                    value.is_u64() || (value["available"] == false && value["reason"].is_string()),
                    "{artifact}.{field} must be a real counter or an explicit unavailable reason"
                );
            }
        }
    }
    assert_eq!(count(noop, &["queries", "semantic_executions"]), 0);
    assert_eq!(count(noop, &["queries", "semantic_reuses"]), 1);

    let unrelated = get("completion_unrelated_edit");
    assert_eq!(body(unrelated, "candidate_comparisons"), 0);
    assert_eq!(body(unrelated, "reused_bodies"), 0);
    assert_eq!(body(unrelated, "skipped_body_analyses"), 0);
    assert_eq!(body(unrelated, "candidate_fallbacks"), 0);
    assert_eq!(body(unrelated, "import_attempts"), functions as u64);
    assert_eq!(body(unrelated, "import_successes"), functions as u64);
    assert_eq!(body(unrelated, "import_failures"), 0);
    assert_eq!(count(unrelated, &["semantic_work", "bodies_attempted"]), 0);
    assert_eq!(cfg(unrelated, "reuses"), functions as u64);
    assert_eq!(cfg(unrelated, "builds_attempted"), 0);
    assert_eq!(cfg(unrelated, "optimization_attempts"), 0);

    let changed = get("completion_changed_reachable_body");
    assert_eq!(body(changed, "candidate_comparisons"), 0);
    assert_eq!(body(changed, "reused_bodies"), 0);
    assert_eq!(body(changed, "candidate_fallbacks"), 0);
    assert_eq!(body(changed, "import_attempts"), functions as u64);
    assert_eq!(body(changed, "import_successes"), functions as u64);
    assert_eq!(body(changed, "import_failures"), 0);
    assert_eq!(count(changed, &["semantic_work", "bodies_attempted"]), 1);
    assert_eq!(cfg(changed, "reuses"), (functions - 1) as u64);
    assert_eq!(cfg(changed, "builds_attempted"), 1);
    assert_eq!(cfg(changed, "optimization_attempts"), 1);

    let closure = get("completion_reverse_closure");
    assert_eq!(body(closure, "candidate_comparisons"), 0);
    assert_eq!(body(closure, "reused_bodies"), 0);
    assert_eq!(body(closure, "candidate_fallbacks"), 0);
    assert_eq!(body(closure, "import_attempts"), functions as u64);
    assert_eq!(body(closure, "import_successes"), functions as u64);
    assert_eq!(body(closure, "import_failures"), 0);
    assert_eq!(count(closure, &["semantic_work", "bodies_attempted"]), 1);
    assert_eq!(cfg(closure, "reuses"), (functions - 1) as u64);
    assert_eq!(cfg(closure, "builds_attempted"), 1);

    for name in ["completion_cfg_o0", "completion_cfg_o1"] {
        let scenario = get(name);
        assert_eq!(body(scenario, "import_attempts"), functions as u64);
        assert_eq!(body(scenario, "import_successes"), functions as u64);
        assert_eq!(body(scenario, "import_failures"), 0);
        assert_eq!(cfg(scenario, "reuse_candidates"), functions as u64);
        assert_eq!(cfg(scenario, "import_attempts"), functions as u64);
        assert_eq!(cfg(scenario, "import_successes"), functions as u64);
        assert_eq!(cfg(scenario, "reuses"), functions as u64);
        assert_eq!(cfg(scenario, "fallbacks"), 0);
        assert_eq!(cfg(scenario, "builds_attempted"), 0);
        assert_eq!(cfg(scenario, "optimization_attempts"), 0);
    }

    let specialized = get("completion_specialization_reuse");
    assert_eq!(body(specialized, "candidate_comparisons"), 0);
    assert_eq!(body(specialized, "candidate_fallbacks"), 0);
    assert_eq!(body(specialized, "specialized_mapping_attempts"), 0);
    assert_eq!(body(specialized, "specialized_mapping_successes"), 0);
    assert_eq!(body(specialized, "specialized_mapping_failures"), 0);
    assert_eq!(body(specialized, "projection_attempts"), 0);
    assert_eq!(body(specialized, "projection_completions"), 0);
    assert_eq!(body(specialized, "projection_failures"), 0);
    assert_eq!(body(specialized, "import_attempts"), 3);
    assert_eq!(body(specialized, "import_successes"), 3);
    assert_eq!(body(specialized, "import_failures"), 0);
    assert_eq!(body(specialized, "atomic_discards"), 0);
    assert_eq!(body(specialized, "reused_bodies"), 0);
    assert_eq!(body(specialized, "skipped_body_analyses"), 0);
    assert_eq!(
        count(specialized, &["semantic_work", "bodies_attempted"]),
        0
    );
    assert_eq!(
        count(specialized, &["semantic_work", "bodies_succeeded"]),
        0
    );
    assert_eq!(count(specialized, &["semantic_work", "bodies_failed"]), 0);
    assert_eq!(
        count(
            specialized,
            &["semantic_work", "specialized_bodies_attempted"]
        ),
        0
    );
    assert_eq!(cfg(specialized, "reuse_candidates"), 3);
    assert_eq!(cfg(specialized, "import_attempts"), 3);
    assert_eq!(cfg(specialized, "import_successes"), 3);
    assert_eq!(cfg(specialized, "import_failures"), 0);
    assert_eq!(cfg(specialized, "reuses"), 3);
    assert_eq!(cfg(specialized, "fallbacks"), 0);
    assert_eq!(cfg(specialized, "builds_attempted"), 0);
    assert_eq!(cfg(specialized, "builds_succeeded"), 0);
    assert_eq!(cfg(specialized, "builds_failed"), 0);
    assert_eq!(cfg(specialized, "optimization_attempts"), 0);
    assert_eq!(cfg(specialized, "optimization_completions"), 0);
    assert_eq!(cfg(specialized, "optimized_level_attempts"), 0);

    let failed = get("completion_failed_semantic");
    assert_eq!(count(failed, &["semantic_work", "failed_requests"]), 1);
    assert_eq!(
        count(
            failed,
            &["semantic_work", "failure_phases", "body_analysis"]
        ),
        1
    );
    assert_eq!(
        count(failed, &["semantic_work", "failure_phases", "declaration"]),
        0
    );
    assert_eq!(
        count(
            failed,
            &["semantic_work", "failure_phases", "cfg_construction"]
        ),
        0
    );
    assert_eq!(body(failed, "candidate_comparisons"), 0);
    assert_eq!(body(failed, "candidate_fallbacks"), 0);
    assert_eq!(body(failed, "projection_attempts"), 0);
    assert_eq!(body(failed, "projection_completions"), 0);
    assert_eq!(body(failed, "projection_failures"), 0);
    assert_eq!(body(failed, "import_attempts"), 0);
    assert_eq!(body(failed, "reused_bodies"), 0);
    assert_eq!(body(failed, "skipped_body_analyses"), 0);
    assert_eq!(body(failed, "atomic_discards"), 0);
    assert_eq!(body(failed, "export_attempts"), 0);
    assert_eq!(count(failed, &["semantic_work", "bodies_attempted"]), 1);
    assert_eq!(count(failed, &["semantic_work", "bodies_succeeded"]), 0);
    assert_eq!(count(failed, &["semantic_work", "bodies_failed"]), 1);
    assert_eq!(cfg(failed, "reuse_candidates"), 0);
    assert_eq!(cfg(failed, "builds_attempted"), 0);
    assert_eq!(
        count(
            failed,
            &[
                "semantic_work",
                "declaration_reuse",
                "durable_cache_population_exports"
            ]
        ),
        0
    );
    assert_declaration_epoch_work(failed, 1, 1, 1, 0, 0);
    let recovery = get("completion_failure_recovery");
    assert_eq!(body(recovery, "reused_bodies"), 0);
    assert_eq!(body(recovery, "candidate_fallbacks"), 0);
    assert_eq!(body(recovery, "import_attempts"), functions as u64);
    assert_eq!(body(recovery, "import_successes"), functions as u64);
    assert_eq!(body(recovery, "import_failures"), 0);
    assert_eq!(count(recovery, &["semantic_work", "bodies_attempted"]), 1);
    assert_eq!(cfg(recovery, "reuses"), functions as u64);
    assert_eq!(cfg(recovery, "builds_attempted"), 0);
}

fn assert_body_cfg_work_equal(left: &Value, right: &Value) {
    for field in [
        "bodies_attempted",
        "bodies_succeeded",
        "bodies_failed",
        "air_instructions_produced",
        "body_dependency_air_instructions_observed",
        "local_strings_produced",
        "string_ids_remapped",
        "specialization_air_instructions_scanned",
        "generic_calls_observed",
        "specialization_requests_unique",
        "specialization_requests_duplicate",
        "specialization_rewrites",
        "specialization_rounds",
        "specialization_driver_failures",
        "specialized_bodies_attempted",
        "specialized_bodies_succeeded",
        "specialized_bodies_failed",
    ] {
        assert_eq!(
            left["semantic_work"][field], right["semantic_work"][field],
            "body work field {field} differs in scenarios expected to take the same body path"
        );
    }
    for field in [
        "drop_glue_functions_synthesized",
        "functions_considered",
        "comptime_functions_filtered",
        "builds_attempted",
        "builds_succeeded",
        "builds_failed",
        "air_instructions_consumed",
        "optimization_attempts",
        "optimization_completions",
        "optimized_level_attempts",
        "warnings_emitted",
        "implicit_destructor_targets_emitted",
        "export_attempts",
        "export_successes",
        "export_rejections",
    ] {
        assert_eq!(
            left["semantic_work"]["cfg"][field], right["semantic_work"]["cfg"][field],
            "CFG construction field {field} differs in scenarios expected to rebuild equally"
        );
    }
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
    parse_config_args(env::args().skip(1)).unwrap_or_else(|message| usage(&message))
}

fn parse_config_args(args: impl IntoIterator<Item = String>) -> Result<Config, String> {
    let mut config = Config {
        modules: DEFAULT_MODULES,
        warmup: DEFAULT_WARMUP,
        iterations: DEFAULT_ITERATIONS,
        representative: false,
    };
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        if arg == "--representative" {
            config.representative = true;
            continue;
        }
        let value = args
            .next()
            .ok_or_else(|| format!("missing value for {arg}"))?;
        let value = value
            .parse::<usize>()
            .map_err(|_| format!("invalid value for {arg}"))?;
        match arg.as_str() {
            "--modules" if value >= 4 => config.modules = value,
            "--warmup" => config.warmup = value,
            "--iterations" if value > 0 => config.iterations = value,
            _ => return Err(format!("unknown option or invalid value: {arg}")),
        }
    }
    Ok(config)
}

fn usage(message: &str) -> ! {
    eprintln!("error: {message}");
    eprintln!(
        "usage: rue-compiler-session-bench [--representative] [--modules N>=4] [--warmup N] [--iterations N>=1]"
    );
    process::exit(2)
}

fn main() {
    let config = parse_config();
    if config.representative {
        for _ in 0..config.warmup {
            representative::run_iteration();
        }
        let iterations = (0..config.iterations)
            .map(|_| representative::run_iteration())
            .collect::<Vec<_>>();
        println!(
            "{}",
            json!({
                "schema_version": 2,
                "workload": "representative_compiler_session_scenarios",
                "fixture": "benchmarks/scenarios/representative/main.rue",
                "measurement_scope": "compiler_build_and_query_performance_not_runtime",
                "configuration": {
                    "warmup_iterations": config.warmup,
                    "measured_iterations": config.iterations,
                    "canonical_query_graph": "CompilerSession",
                    "persistent_cache": false,
                    "timing_inferred_reuse": false,
                    "fresh_session_parity": true,
                },
                "iterations": iterations,
            })
        );
        return;
    }
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
            "schema_version": 12,
            "workload": "compiler_session_invalidation",
            "configuration": {
                "modules": config.modules,
                "warmup_iterations": config.warmup,
                "measured_iterations": config.iterations,
                "filesystem_discovery_during_updates": false,
                "backend_or_link_during_measured_scenarios": false,
                "differential_parity_backend_and_link": true,
            },
            "iterations": iterations,
        })
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use rue_air::{Type, TypeInternPool};

    #[test]
    fn type_pool_snapshot_preserves_global_allocation_order() {
        let pool = TypeInternPool::new();
        let pointer = pool.try_intern_ptr_const(Type::I32).unwrap();
        let array = pool.try_intern_array(Type::I32, 3).unwrap();
        let frozen = pool.freeze();

        assert_eq!(frozen.all_types().collect::<Vec<_>>(), [pointer, array]);
        assert_eq!(
            type_pool_snapshot(&frozen),
            ["ptr_const:Type::I32", "array:Type::I32:3"]
        );
    }

    #[test]
    fn small_workload_is_a_structural_smoke_test() {
        let scenarios = run_iteration(&Fixture::new(4));
        assert_eq!(scenarios.len(), 24);
        assert!(
            scenarios
                .iter()
                .all(|scenario| scenario["wall_time_ns"].is_u64()
                    || (scenario["manifest_wall_time_ns"].is_u64()
                        && scenario["plan_wall_time_ns"].is_u64()))
        );
    }

    #[test]
    fn completion_workload_rejects_fewer_than_four_modules_at_config_boundary() {
        for modules in ["2", "3"] {
            assert!(
                parse_config_args(["--modules".to_owned(), modules.to_owned()]).is_err(),
                "--modules {modules} must be rejected before fixture construction"
            );
        }
        assert_eq!(
            parse_config_args(["--modules".to_owned(), "4".to_owned()]).unwrap(),
            Config {
                modules: 4,
                warmup: DEFAULT_WARMUP,
                iterations: DEFAULT_ITERATIONS,
                representative: false,
            }
        );
    }

    #[test]
    fn representative_mode_is_explicit() {
        assert!(
            parse_config_args(["--representative".to_owned()])
                .unwrap()
                .representative
        );
    }
}
