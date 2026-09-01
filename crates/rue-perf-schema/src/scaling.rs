//! Lower-frequency compiler scaling reports for maintained Rue programs.
//!
//! These records deliberately do not enter the ADR-0067 headline series. The
//! startup suite answers a high-frequency regression question; this report
//! answers how one fresh compiler process scales as maintained programs grow.
//! Both use the same raw [`crate::Sample`] and additive phase accounting.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::{
    BuildBoundary, DisplayIdentityWork, EnvironmentFingerprint, Sample, ValidationWork,
    WorkerSetting,
};

/// Version of the scaling-report wire format.
pub const SCALING_REPORT_SCHEMA_VERSION: u32 = 27;

/// The lower-frequency scaling suite declaration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScalingManifest {
    /// Version of this manifest syntax.
    pub schema_version: u32,
    /// Fixture revision. Increment when membership or intent changes.
    pub revision: u32,
    /// Independent fresh-process samples per workload.
    pub samples: u32,
    /// Behaviour-affecting arguments passed to every compiler process.
    #[serde(default)]
    pub args: Vec<String>,
    /// Required Rust build profile of the compiler binary under measurement.
    pub compiler_build_profile: String,
    /// Complete product boundary every row must prove.
    pub boundary: BuildBoundary,
    /// Independently declared compiler target.
    pub target: String,
    /// Ordered worker matrix measured for every workload.
    pub workers: Vec<WorkerSetting>,
    /// Maintained programs, in report order.
    pub workloads: Vec<ScalingWorkload>,
}

impl ScalingManifest {
    /// Parse and validate one scaling manifest.
    pub fn parse(text: &str) -> Result<Self, String> {
        let manifest: ScalingManifest =
            toml::from_str(text).map_err(|error| format!("invalid scaling manifest: {error}"))?;
        if manifest.schema_version != 3 {
            return Err(format!(
                "unsupported scaling manifest schema version {}",
                manifest.schema_version
            ));
        }
        if manifest.samples < 2 {
            return Err("scaling measurements require at least two samples".to_string());
        }
        if manifest.compiler_build_profile.is_empty() {
            return Err("the scaling manifest declares no compiler build profile".to_string());
        }
        if manifest.boundary != BuildBoundary::FreshSourceToNativeV1 {
            return Err("the scaling manifest declares an unsupported build boundary".to_string());
        }
        if manifest.target.is_empty() {
            return Err("the scaling manifest declares no compiler target".to_string());
        }
        if manifest.workers != WorkerSetting::REFERENCE_MATRIX {
            return Err(format!(
                "the scaling manifest must declare the ADR-0071 worker matrix {:?}",
                WorkerSetting::REFERENCE_MATRIX
            ));
        }
        if !manifest.args.is_empty() {
            return Err(
                "fresh_source_to_native_v1 scaling admits no extra compiler arguments".to_string(),
            );
        }
        if manifest.workloads.is_empty() {
            return Err("the scaling manifest declares no workloads".to_string());
        }
        let mut ids = BTreeSet::new();
        for workload in &manifest.workloads {
            if workload.id.is_empty() {
                return Err("a scaling workload has an empty id".to_string());
            }
            if !ids.insert(&workload.id) {
                return Err(format!("duplicate scaling workload id {:?}", workload.id));
            }
            if workload.source.is_empty() || workload.source.starts_with('/') {
                return Err(format!(
                    "scaling workload {:?} must use a repository-relative source",
                    workload.id
                ));
            }
        }
        Ok(manifest)
    }
}

/// One maintained program in the scaling suite.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScalingWorkload {
    /// Stable short name.
    pub id: String,
    /// Root source, relative to the repository.
    pub source: String,
    /// The scaling question this program represents.
    pub question: String,
}

/// One immutable, raw scaling report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScalingReport {
    /// Version of this report syntax.
    pub schema_version: u32,
    /// Identity and environment of the measurement.
    pub identity: ScalingIdentity,
    /// Structural measurement regime.
    pub regime: ScalingRegime,
    /// Raw measurements, in manifest order.
    pub workloads: Vec<ScalingObservation>,
}

/// Identity of a scaling report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScalingIdentity {
    /// Scaling-manifest fixture revision.
    pub manifest_revision: u32,
    /// Compiler source revision under measurement.
    pub commit: String,
    /// UTC start timestamp.
    pub started_at: String,
    /// UTC completion timestamp.
    pub finished_at: String,
    /// Target reported by the compiler.
    pub target: String,
    /// Machine and runner fingerprint.
    pub environment: EnvironmentFingerprint,
}

/// What every sample in a scaling report means.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScalingRegime {
    /// Always `fresh_process_compile`: every sample launches a compiler.
    pub compiler_state: String,
    /// Always `uncontrolled`: page-cache state is observed, not reset.
    pub os_page_cache: String,
    /// Always false. Heavyweight example runtime is a separate measurement.
    pub program_runtime_executed: bool,
    /// Sequential independent samples per workload.
    pub samples_per_workload: u32,
    /// Behaviour-affecting arguments passed to every compiler process.
    pub compiler_args: Vec<String>,
    /// Rust build profile reported by the compiler binary.
    pub compiler_build_profile: String,
    /// Complete boundary jointly proved by manifest, runner, and compiler.
    pub boundary: BuildBoundary,
    /// Ordered worker matrix measured for every workload.
    pub workers: Vec<WorkerSetting>,
}

/// Raw measurements for one maintained program.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScalingObservation {
    /// Stable workload id.
    pub workload: String,
    /// Root source recorded for human inspection.
    pub source: String,
    /// Why this fixture participates in the scale curve.
    pub question: String,
    /// Manifest row represented by this observation.
    pub worker_setting: WorkerSetting,
    /// Actual worker count selected by the compiler (`automatic` is resolved
    /// from the host rather than assumed by the runner).
    pub resolved_workers: u32,
    /// Compiler-produced fixture shape. A change makes cross-report comparison
    /// advisory even when the manifest revision was not yet advanced.
    pub shape: WorkloadShape,
    /// Deterministic compiler work, required to agree across the fixed
    /// single-worker structural probes.
    pub work: CompilerWork,
    /// Independent raw fresh-process measurements.
    pub samples: Vec<Sample>,
}

/// Deterministic work performed by one compiler process.
///
/// This is deliberately separate from elapsed time and fixture shape. It grows
/// as the compiler performs internal work, so a report can identify
/// amplification even when host timing is noisy.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompilerWork {
    /// Query-native source/AST-to-candidate declaration body-plan work.
    #[serde(default)]
    pub candidate_body_plan_construction: CandidateBodyPlanWork,
    /// Query-native candidate plan materialization/remapping/validation work.
    #[serde(default)]
    pub candidate_body_plan_materialization: CandidateBodyPlanWork,
    /// Query-native whole-program RIR presentation work. This is populated
    /// only for an explicit presentation request and never for rooted builds.
    #[serde(default)]
    pub canonical_rir_presentation: CanonicalRirPresentationWork,
    /// Exact semantic facts observed by provider-native body analysis.
    pub semantic_provider: SemanticProviderWork,
    /// Work performed while discovering and scheduling reachable bodies.
    pub semantic_reachability: SemanticReachabilityWork,
    /// Unrelated semantic-analysis evidence. Candidate-plan fields above are
    /// the sole structural lowering authority in v3.
    #[serde(default)]
    pub semantic_analysis_structure: SemanticAnalysisStructureWork,
    /// Historical v2 payload retained only in memory while validating a v2
    /// record. It is deliberately omitted from every v3 serialization.
    #[serde(rename = "semantic_body_structure", default, skip_serializing)]
    #[doc(hidden)]
    #[doc(hidden)]
    pub legacy_v2_semantic_body_structure: Option<LegacySemanticBodyStructureWork>,
    /// Request-local lookup preparation for exact CFG materialization facts.
    pub cfg_materialization: CfgMaterializationWork,
    /// Stable-type scans and registered prerequisite requests for CFG bodies.
    #[serde(default)]
    pub cfg_prerequisites: CfgPrerequisiteWork,
    /// Logical retained-charge bookkeeping for CFG artifacts.
    #[serde(default)]
    pub cfg_retained_charge: CfgRetainedChargeWork,
    /// Aggregate size of the rooted body-local semantic domains retained by CFG.
    #[serde(default)]
    pub cfg_local_epoch: CfgLocalEpochWork,
    /// Bounded structural work from target-independent CFG optimization.
    #[serde(default, skip_serializing_if = "is_default")]
    pub cfg_optimization: CfgOptimizationWork,
    /// Work performed by the revisioned query runtime.
    pub query_runtime: QueryRuntimeWork,
    /// Publication-seam health for the proof-lease handoffs (RUE-1576).
    #[serde(default)]
    pub publication: PublicationWork,
}

fn is_zero(value: &u64) -> bool {
    *value == 0
}

fn is_default<T: Default + PartialEq>(value: &T) -> bool {
    value == &T::default()
}

/// Exact lifecycle and output quantities for one candidate body-plan query.
/// Computed and reused are request counts; output quantities count successfully
/// published plans only and are independent of timing.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CandidateBodyPlanWork {
    pub computed: u64,
    pub reused: u64,
    pub instructions_produced: u64,
    pub payload_words_produced: u64,
}

/// Exact quantities for explicit whole-program canonical RIR presentation.
/// Requests and projected modules are intentionally separate units.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CanonicalRirPresentationWork {
    pub requests_computed: u64,
    pub modules_projected: u64,
    pub instructions_produced: u64,
    pub payload_words_produced: u64,
}

/// Deterministic publication-seam health for one compiler process.
///
/// The collection-scope handoffs replace per-node lease-reacquisition demand
/// cascades with borrowed cone authority; this group records the times a
/// handoff could not be produced and validation silently fell back. Expected
/// zero; the compiler's pipeline gate pins it there on maintained workloads.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublicationWork {
    /// Declaration publications that could not retain their projection cone.
    pub cone_retention_failures: u64,
}

/// Unrelated deterministic semantic-analysis counters retained in v3.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SemanticAnalysisStructureWork {
    pub precompute_bodies: u64,
    pub precompute_alias_nodes_visited: u64,
    pub precompute_alias_block_statements: u64,
    pub precompute_alias_allocations_examined: u64,
    pub precompute_alias_filter_accepts: u64,
    pub precompute_alias_filter_skips: u64,
    pub precompute_alias_eval_attempts: u64,
    pub precompute_alias_type_successes: u64,
    pub precompute_inline_scan_pops: u64,
    pub precompute_inline_scan_child_edges: u64,
    #[serde(default)]
    pub precompute_inline_scan_bodies: u64,
    pub precompute_inline_raw_candidates: u64,
    pub precompute_inline_final_candidates: u64,
    pub precompute_inline_eval_attempts: u64,
    pub precompute_inline_type_successes: u64,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub staged_resolved_instructions: u64,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub staged_constraints_generated: u64,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub staged_fact_nodes: u64,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub staged_canonical_evaluations: u64,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub staged_binding_scope_nodes: u64,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub staged_binding_materializations: u64,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub staged_binding_trie_updates: u64,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub staged_binding_trie_lookups: u64,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub staged_probe_nodes: u64,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub staged_precompute_nodes: u64,
}

/// Private v2-only body lowering evidence. It is accepted solely while
/// decoding and validating historical records; no v3 producer or renderer
/// exposes this retired subgroup.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[doc(hidden)]
pub struct LegacySemanticBodyStructureWork {
    pub body_lowerings: u64,
    pub source_bytes: u64,
    pub declaration_fragments: u64,
    pub rir_instructions: u64,
    pub rir_payload_words: u64,
    pub index_builds: u64,
    pub index_rir_instructions_visited: u64,
    pub index_method_references_visited: u64,
    pub index_shell_declarations_visited: u64,
    pub index_named_methods_indexed: u64,
    pub index_const_declarations_indexed: u64,
    pub precompute_bodies: u64,
    pub precompute_alias_nodes_visited: u64,
    pub precompute_alias_block_statements: u64,
    pub precompute_alias_allocations_examined: u64,
    pub precompute_alias_filter_accepts: u64,
    pub precompute_alias_filter_skips: u64,
    pub precompute_alias_eval_attempts: u64,
    pub precompute_alias_type_successes: u64,
    pub precompute_inline_scan_pops: u64,
    pub precompute_inline_scan_child_edges: u64,
    #[serde(default)]
    pub precompute_inline_scan_bodies: u64,
    pub precompute_inline_raw_candidates: u64,
    pub precompute_inline_final_candidates: u64,
    pub precompute_inline_eval_attempts: u64,
    pub precompute_inline_type_successes: u64,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub staged_resolved_instructions: u64,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub staged_constraints_generated: u64,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub staged_fact_nodes: u64,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub staged_canonical_evaluations: u64,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub staged_binding_scope_nodes: u64,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub staged_binding_materializations: u64,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub staged_binding_trie_updates: u64,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub staged_binding_trie_lookups: u64,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub staged_probe_nodes: u64,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub staged_precompute_nodes: u64,
}

impl CompilerWork {
    /// Reconstruct the exact historical semantic subgroup for a v2 digest.
    /// A decoded v2 record carries its original payload; a newly constructed
    /// value projects the unrelated v3 evidence and leaves retired counters
    /// at their historical zero.
    pub(crate) fn legacy_v2_semantic_body_structure(&self) -> LegacySemanticBodyStructureWork {
        self.legacy_v2_semantic_body_structure
            .unwrap_or_else(|| LegacySemanticBodyStructureWork {
                precompute_bodies: self.semantic_analysis_structure.precompute_bodies,
                precompute_alias_nodes_visited: self
                    .semantic_analysis_structure
                    .precompute_alias_nodes_visited,
                precompute_alias_block_statements: self
                    .semantic_analysis_structure
                    .precompute_alias_block_statements,
                precompute_alias_allocations_examined: self
                    .semantic_analysis_structure
                    .precompute_alias_allocations_examined,
                precompute_alias_filter_accepts: self
                    .semantic_analysis_structure
                    .precompute_alias_filter_accepts,
                precompute_alias_filter_skips: self
                    .semantic_analysis_structure
                    .precompute_alias_filter_skips,
                precompute_alias_eval_attempts: self
                    .semantic_analysis_structure
                    .precompute_alias_eval_attempts,
                precompute_alias_type_successes: self
                    .semantic_analysis_structure
                    .precompute_alias_type_successes,
                precompute_inline_scan_pops: self
                    .semantic_analysis_structure
                    .precompute_inline_scan_pops,
                precompute_inline_scan_child_edges: self
                    .semantic_analysis_structure
                    .precompute_inline_scan_child_edges,
                precompute_inline_scan_bodies: self
                    .semantic_analysis_structure
                    .precompute_inline_scan_bodies,
                precompute_inline_raw_candidates: self
                    .semantic_analysis_structure
                    .precompute_inline_raw_candidates,
                precompute_inline_final_candidates: self
                    .semantic_analysis_structure
                    .precompute_inline_final_candidates,
                precompute_inline_eval_attempts: self
                    .semantic_analysis_structure
                    .precompute_inline_eval_attempts,
                precompute_inline_type_successes: self
                    .semantic_analysis_structure
                    .precompute_inline_type_successes,
                staged_resolved_instructions: self
                    .semantic_analysis_structure
                    .staged_resolved_instructions,
                staged_constraints_generated: self
                    .semantic_analysis_structure
                    .staged_constraints_generated,
                staged_fact_nodes: self.semantic_analysis_structure.staged_fact_nodes,
                staged_canonical_evaluations: self
                    .semantic_analysis_structure
                    .staged_canonical_evaluations,
                staged_binding_scope_nodes: self
                    .semantic_analysis_structure
                    .staged_binding_scope_nodes,
                staged_binding_materializations: self
                    .semantic_analysis_structure
                    .staged_binding_materializations,
                staged_binding_trie_updates: self
                    .semantic_analysis_structure
                    .staged_binding_trie_updates,
                staged_binding_trie_lookups: self
                    .semantic_analysis_structure
                    .staged_binding_trie_lookups,
                staged_probe_nodes: self.semantic_analysis_structure.staged_probe_nodes,
                staged_precompute_nodes: self.semantic_analysis_structure.staged_precompute_nodes,
                ..LegacySemanticBodyStructureWork::default()
            })
    }
}

/// Deterministic provider operations performed by semantic body analysis.
///
/// These are observations already required by the production provider. The
/// benchmark merely snapshots them; collecting this report adds no provider
/// lookup or materialization work.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SemanticProviderWork {
    /// Unqualified, qualified, and language-item name lookups.
    pub name_lookups: u64,
    /// Import and module-binding lookups.
    pub import_lookups: u64,
    /// Named-method candidate requests.
    pub method_candidates: u64,
    /// Operator-method candidate requests.
    pub operator_candidates: u64,
    /// All declaration fact reads; exactly partitioned by the four fields below.
    pub declaration_facts: u64,
    /// Exact declaration-identity reads.
    pub identity_facts: u64,
    /// Exact callable and nominal-signature reads.
    pub signature_facts: u64,
    /// Exact nominal well-formedness reads.
    pub type_facts: u64,
    /// Exact constant and compile-time reduction reads.
    pub const_facts: u64,
    /// Durable facts materialized into body-local representations.
    pub materializations: u64,
    /// Requests routed through materializers which share canonical payloads.
    #[serde(default)]
    pub shared_payload_materializations: u64,
    /// Requests routed through materializers which rebuild owned payloads.
    #[serde(default)]
    pub owned_payload_materializations: u64,
    /// Constant facts materialized into body-local representations.
    #[serde(default)]
    pub const_materializations: u64,
    /// Named nominal facts materialized into body-local representations.
    #[serde(default)]
    pub nominal_materializations: u64,
    /// Free-function facts materialized into body-local representations.
    #[serde(default)]
    pub function_materializations: u64,
    /// Method facts materialized into body-local representations.
    #[serde(default)]
    pub method_materializations: u64,
    /// Named nominal requests satisfied by an exact body-transaction payload reuse.
    #[serde(default)]
    pub nominal_materialization_reuses: u64,
    /// Free-function requests satisfied by an exact body-transaction payload reuse.
    #[serde(default)]
    pub function_materialization_reuses: u64,
    /// Anonymous-nominal fact reads.
    pub anonymous_facts: u64,
    /// Anonymous-producer body fact reads.
    pub producer_facts: u64,
    /// Trusted-toolchain fact reads.
    pub toolchain_facts: u64,
    /// Top-level imported-type nominal registration requests.
    #[serde(default)]
    pub import_nominal_registration_requests: u64,
    /// Durable type nodes visited by imported-type nominal registration.
    #[serde(default)]
    pub import_nominal_type_visits: u64,
    /// Named nominal nodes probed against the body-local registration cache.
    #[serde(default)]
    pub import_named_nominal_probes: u64,
    /// Named nominal probes satisfied by complete body-local closures.
    #[serde(default)]
    pub import_named_nominal_complete_hits: u64,
    /// Recursive named nominal probes stopped by an in-progress cycle marker.
    #[serde(default)]
    pub import_named_nominal_cycle_hits: u64,
    /// Named nominal closures installed completely in body-local identity domains.
    #[serde(default)]
    pub import_named_nominals_registered: u64,
    /// Container-element and nominal-field edges traversed while installing closures.
    #[serde(default)]
    pub import_nominal_type_edges_traversed: u64,
    /// Anonymous nominal identities installed through imported durable types.
    #[serde(default)]
    pub import_anonymous_nominals_registered: u64,
}

/// Deterministic preparation and selection work for body-local CFG facts.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CfgMaterializationWork {
    /// Immutable request-local indexes constructed.
    pub index_builds: u64,
    /// Durable declarations visited while constructing those indexes.
    pub declarations_scanned: u64,
    /// Durable anonymous nominals visited while constructing those indexes.
    pub anonymous_nominals_scanned: u64,
    /// Durable type nodes visited while discovering named slice sources.
    pub type_nodes_scanned: u64,
    /// Exact body or drop-glue fact closures selected from the shared index.
    pub fact_selections: u64,
    /// Distinct closures allocated, and selections served from a closure an
    /// earlier body already built. These sum to `fact_selections`.
    #[serde(default)]
    pub fact_closures_allocated: u64,
    #[serde(default)]
    pub fact_closures_reused: u64,
    /// Durable declaration facts copied into selected body-local closures.
    #[serde(default)]
    pub declarations_selected: u64,
    /// Durable anonymous nominal facts copied into selected closures.
    #[serde(default)]
    pub anonymous_nominals_selected: u64,
    /// Callable facts copied into selected closures.
    #[serde(default)]
    pub callables_selected: u64,
    /// Nominal metadata facts copied into selected closures.
    #[serde(default)]
    pub nominal_metadata_selected: u64,
    /// Module identities copied into selected closures.
    #[serde(default)]
    pub modules_selected: u64,
    /// Synthetic builtin nominal requests copied into selected closures.
    #[serde(default)]
    pub builtin_nominals_selected: u64,
    /// Additional stable type roots copied into selected closures.
    #[serde(default)]
    pub required_types_selected: u64,
}

/// Deterministic prerequisite work performed before AIR-to-CFG construction.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CfgPrerequisiteWork {
    /// Stable types scanned across every body-local domain.
    pub stable_types_scanned: u64,
    /// Unique layout terminals requested across all CFG bodies.
    pub layout_requests: u64,
    /// Retired: always zero since drop-glue terminals took ownership of the
    /// exact type-fact dependency and the duplicate top-level request was
    /// removed. The field survives so stored reports keep deserializing under
    /// `deny_unknown_fields`; nothing records it.
    pub type_fact_requests: u64,
    /// Unique drop-glue terminals requested for drop-relevant types.
    pub drop_glue_requests: u64,
}

/// Deterministic logical retained-charge work for CFG artifacts.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CfgRetainedChargeWork {
    /// Body-local symbol tables walked to compute their logical retained charge.
    pub interner_scans: u64,
    /// Symbol-table entries visited by those retained-charge walks.
    pub interner_entries_scanned: u64,
    /// UTF-8 symbol bytes visited by those retained-charge walks.
    pub interner_utf8_bytes_scanned: u64,
}

/// Aggregate size of rooted body-local semantic domains retained by CFG.
///
/// These values come from constant-time container and payload-store accounting;
/// collecting them does not add a second traversal of the local epoch.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CfgLocalEpochWork {
    /// Rooted CFG records carrying local semantic domains.
    pub epochs: u64,
    /// AIR instructions consumed across local epochs.
    pub air_instructions: u64,
    /// Logical bytes in AIR's variable-width payload stores.
    pub air_payload_bytes: u64,
    /// Entries in body-local type pools.
    pub type_entries: u64,
    /// Stable aliases for body-local aggregate types.
    pub aggregate_type_aliases: u64,
    /// Explicit caller-owned type handles imported for mandatory splices.
    pub materialized_type_handles: u64,
    /// Entries in body-local symbol interners.
    pub interner_entries: u64,
    /// UTF-8 bytes in body-local symbol interners.
    pub interner_utf8_bytes: u64,
    /// String-literal slots retained by local epochs.
    pub strings: u64,
    /// Stable relocation atoms retained by local epochs.
    pub local_atoms: u64,
}

/// Additive per-pass work from successful local optimization and changed-caller
/// reoptimization. Analysis construction is charged to the pass that owns it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CfgOptimizationWork {
    pub constopt_fold_attempts: u64,
    pub constopt_folded: u64,
    pub constopt_loads_rewritten: u64,
    pub peephole_divmods_reduced: u64,
    pub peephole_identities_rewired: u64,
    pub simplify_blocks_scanned: u64,
    pub simplify_branches_folded: u64,
    pub simplify_switches_folded: u64,
    pub simplify_edges_threaded: u64,
    pub simplify_forwarders_resolved: u64,
    pub simplify_blocks_merged: u64,
    pub forward_insts_scanned: u64,
    pub forward_loads_single_write: u64,
    pub forward_loads_block_local: u64,
    pub forward_rule1_dominance_pairs_checked: u64,
    pub forward_dominator_computations: u64,
    pub cse_insts_scanned: u64,
    pub cse_duplicates_replaced: u64,
    /// Sum of the per-run value-number table high-water marks.
    pub cse_max_table_entries_sum: u64,
    pub cse_dominator_computations: u64,
    pub preheader_normalization_forest_computations: u64,
    pub preheader_normalization_loops_examined: u64,
    pub preheader_normalization_preheaders_materialized: u64,
    pub preheader_normalization_verifier_dominator_computations: u64,
    pub licm_forest_computations: u64,
    pub licm_def_block_scans: u64,
    pub licm_loops_analyzed: u64,
    pub licm_instructions_examined: u64,
    pub licm_slot_fact_instructions_scanned: u64,
    pub licm_slot_fact_entries_initialized: u64,
    pub licm_slot_fact_workspace_growths: u64,
    /// Candidate instructions physically visited by sparse CSR refills.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub licm_use_index_users_visited: u64,
    /// Candidate operand edges physically visited by sparse CSR refills.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub licm_use_index_edges_visited: u64,
    /// Dense value-domain entries initialized on amortized index growth.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub licm_use_index_domain_entries_initialized: u64,
    pub licm_candidate_dependencies: u64,
    pub licm_worklist_pops: u64,
    pub licm_invariants_hoisted: u64,
    pub licm_hoist_workspace_growths: u64,
    pub unroll_forest_computations: u64,
    pub unroll_loops_analyzed: u64,
    pub unroll_loops_unrolled: u64,
    pub unroll_budget_refusals: u64,
    pub unroll_shape_refusals: u64,
    pub unroll_blocks_cloned: u64,
    pub unroll_values_cloned: u64,
    pub unroll_instructions_cloned: u64,
    pub publication_verifier_dominator_computations: u64,
    pub accessor_splice_imported_callee_verifier_dominator_computations: u64,
    pub accessor_splice_preoptimization_verifier_dominator_computations: u64,
    pub general_inline_splice_imported_callee_verifier_dominator_computations: u64,
    pub inline_splice_pre_reoptimization_verifier_dominator_computations: u64,
}

/// Deterministic work performed by database-owned semantic reachability.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SemanticReachabilityWork {
    /// Pending-set scans that discover dependency-ready logical frontiers.
    pub frontier_scans: u64,
    /// Body keys examined across logical-frontier scans.
    pub frontier_scan_keys: u64,
    /// Non-empty dependency-ready logical frontiers discovered.
    pub frontier_batches: u64,
    /// Body keys selected across those logical frontiers.
    pub frontier_keys: u64,
    pub frontier_width_one: u64,
    pub frontier_width_two_to_three: u64,
    pub frontier_width_four_to_seven: u64,
    pub frontier_width_eight_or_more: u64,
    /// Transactions consumed from bounded windows of a ready frontier.
    pub transactions_prefetched: u64,
    /// Transactions demanded outside the ready-frontier scheduler.
    pub transactions_serial: u64,
}

/// Deterministic query-runtime work performed by one compiler process.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QueryRuntimeWork {
    /// New query computations claimed by this process.
    pub claims: u64,
    /// Compatible retained or task-local terminals reused.
    pub reuses: u64,
    /// Compatible in-flight query computations joined.
    pub joins: u64,
    /// Joined computations declined to avoid a wait-graph cycle.
    pub declined_joins: u64,
    /// Query bodies which completed before publication checks.
    pub body_completions: u64,
    /// Publications whose observable stamp stayed unchanged.
    pub red_publications: u64,
    /// New or observably changed publications.
    pub green_publications: u64,
    /// Query computations or waiters canceled.
    pub cancellations: u64,
    /// True query dependency cycles reported.
    pub cycles: u64,
    /// Retained-terminal validation and proof work.
    pub validation: ValidationWork,
    /// Presentation-only query identity materialization.
    pub display_identities: DisplayIdentityWork,
    /// Family-local retention passes run.
    pub retention_enforcements: u64,
    /// Retention-queue entries examined by those passes.
    pub retention_scan_entries: u64,
}

/// Compiler-produced dimensions of one fixture.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkloadShape {
    /// Physical source files in the resolved program.
    pub files: u64,
    /// Modules consumed by parsing.
    pub modules: u64,
    /// Source bytes.
    pub bytes: u64,
    /// Source lines.
    pub lines: u64,
    /// Lexer tokens consumed by parsing.
    pub tokens: u64,
    /// Source and synthesized functions considered for CFG construction.
    pub functions: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALID: &str = r#"
schema_version = 3
revision = 3
samples = 3
compiler_build_profile = "release_thin_lto"
boundary = "fresh_source_to_native_v1"
target = "x86-64-linux"
workers = ["one", "two", "four", "eight", "automatic"]

[[workloads]]
id = "ruelex"
source = "examples/ruelex/main.rue"
question = "small maintained compiler frontend"
"#;

    #[test]
    fn parses_a_versioned_scaling_manifest() {
        let manifest = ScalingManifest::parse(VALID).unwrap();
        assert_eq!(manifest.revision, 3);
        assert_eq!(manifest.samples, 3);
        assert_eq!(manifest.compiler_build_profile, "release_thin_lto");
        assert_eq!(manifest.workers, WorkerSetting::REFERENCE_MATRIX);
        assert_eq!(manifest.workloads[0].id, "ruelex");
    }

    #[test]
    fn rejects_single_sample_reports_because_they_have_no_uncertainty() {
        let error =
            ScalingManifest::parse(&VALID.replace("samples = 3", "samples = 1")).unwrap_err();
        assert!(error.contains("at least two samples"), "{error}");
    }

    #[test]
    fn rejects_duplicate_fixture_ids() {
        let duplicate = format!(
            "{VALID}\n{}",
            &VALID[VALID.find("[[workloads]]").unwrap()..]
        );
        let error = ScalingManifest::parse(&duplicate).unwrap_err();
        assert!(error.contains("duplicate scaling workload"), "{error}");
    }

    #[test]
    fn rejects_unknown_fields_instead_of_guessing() {
        let error = ScalingManifest::parse(&format!("{VALID}\nsurprise = true\n")).unwrap_err();
        assert!(error.contains("unknown field"), "{error}");
    }

    #[test]
    fn older_compiler_work_defaults_publication_seam_health() {
        let mut encoded = serde_json::to_value(CompilerWork::default()).unwrap();
        let object = encoded.as_object_mut().unwrap();
        object.remove("publication");
        let decoded: CompilerWork = serde_json::from_value(encoded).unwrap();
        assert_eq!(decoded.publication, PublicationWork::default());
    }

    #[test]
    fn older_compiler_work_defaults_cfg_optimization_work() {
        let mut encoded = serde_json::to_value(CompilerWork::default()).unwrap();
        encoded.as_object_mut().unwrap().remove("cfg_optimization");
        let decoded: CompilerWork = serde_json::from_value(encoded).unwrap();
        assert_eq!(decoded.cfg_optimization, CfgOptimizationWork::default());
    }

    #[test]
    fn revision_26_cfg_optimization_defaults_new_licm_use_index_work() {
        let mut work = CompilerWork::default();
        work.cfg_optimization.licm_forest_computations = 1;
        let encoded = serde_json::to_value(work).unwrap();
        let cfg_optimization = encoded["cfg_optimization"].as_object().unwrap();
        assert!(!cfg_optimization.contains_key("licm_use_index_users_visited"));
        assert!(!cfg_optimization.contains_key("licm_use_index_edges_visited"));
        assert!(!cfg_optimization.contains_key("licm_use_index_domain_entries_initialized"));

        let decoded: CompilerWork = serde_json::from_value(encoded).unwrap();
        assert_eq!(decoded.cfg_optimization.licm_forest_computations, 1);
        assert_eq!(decoded.cfg_optimization.licm_use_index_users_visited, 0);
        assert_eq!(decoded.cfg_optimization.licm_use_index_edges_visited, 0);
        assert_eq!(
            decoded
                .cfg_optimization
                .licm_use_index_domain_entries_initialized,
            0
        );
    }

    #[test]
    fn legacy_compiler_work_preserves_semantic_structure() {
        let mut encoded = serde_json::to_value(CompilerWork::default()).unwrap();
        let mut expected =
            serde_json::to_value(LegacySemanticBodyStructureWork::default()).unwrap();
        expected["body_lowerings"] = 7.into();
        expected["source_bytes"] = 11.into();
        expected["rir_instructions"] = 13.into();
        expected["index_builds"] = 7.into();
        encoded["semantic_body_structure"] = serde_json::to_value(expected).unwrap();
        let decoded: CompilerWork = serde_json::from_value(encoded).unwrap();
        assert_eq!(
            decoded
                .legacy_v2_semantic_body_structure
                .unwrap()
                .body_lowerings,
            7
        );
    }

    #[test]
    fn legacy_compiler_work_defaults_new_taxonomy() {
        let mut encoded = serde_json::to_value(CompilerWork::default()).unwrap();
        let mut structure =
            serde_json::to_value(LegacySemanticBodyStructureWork::default()).unwrap();
        structure["body_lowerings"] = 7.into();
        structure["rir_instructions"] = 11.into();
        structure["source_bytes"] = 13.into();
        structure["index_builds"] = 7.into();
        encoded["semantic_body_structure"] = structure;
        encoded
            .as_object_mut()
            .unwrap()
            .remove("candidate_body_plan_construction");
        encoded
            .as_object_mut()
            .unwrap()
            .remove("candidate_body_plan_materialization");
        encoded
            .as_object_mut()
            .unwrap()
            .remove("canonical_rir_presentation");
        let decoded: CompilerWork = serde_json::from_value(encoded).unwrap();
        assert_eq!(
            decoded.candidate_body_plan_construction,
            CandidateBodyPlanWork::default()
        );
        assert_eq!(
            decoded.candidate_body_plan_materialization,
            CandidateBodyPlanWork::default()
        );
        assert_eq!(
            decoded.canonical_rir_presentation,
            CanonicalRirPresentationWork::default()
        );
    }

    #[test]
    fn older_compiler_work_defaults_additive_cfg_categories() {
        let mut encoded = serde_json::to_value(CompilerWork::default()).unwrap();
        let object = encoded.as_object_mut().unwrap();
        object.remove("cfg_prerequisites");
        object.remove("cfg_retained_charge");
        object.remove("semantic_body_structure");
        let decoded: CompilerWork = serde_json::from_value(encoded).unwrap();
        assert_eq!(decoded.cfg_prerequisites, CfgPrerequisiteWork::default());
        assert_eq!(
            decoded.cfg_retained_charge,
            CfgRetainedChargeWork::default()
        );
        assert_eq!(
            decoded.semantic_analysis_structure,
            SemanticAnalysisStructureWork::default()
        );
    }

    #[test]
    fn staged_body_work_is_additive_and_round_trips_in_json() {
        let structure = SemanticAnalysisStructureWork {
            staged_resolved_instructions: 11,
            staged_constraints_generated: 13,
            staged_fact_nodes: 17,
            staged_canonical_evaluations: 19,
            staged_binding_scope_nodes: 41,
            staged_binding_materializations: 43,
            staged_binding_trie_updates: 31,
            staged_binding_trie_lookups: 37,
            staged_probe_nodes: 23,
            staged_precompute_nodes: 29,
            ..SemanticAnalysisStructureWork::default()
        };
        let encoded = serde_json::to_value(structure).unwrap();
        let decoded: SemanticAnalysisStructureWork = serde_json::from_value(encoded).unwrap();
        assert_eq!(decoded, structure);
        let mut legacy = serde_json::to_value(structure).unwrap();
        for field in [
            "staged_resolved_instructions",
            "staged_constraints_generated",
            "staged_fact_nodes",
            "staged_canonical_evaluations",
            "staged_binding_scope_nodes",
            "staged_binding_materializations",
            "staged_binding_trie_updates",
            "staged_binding_trie_lookups",
            "staged_probe_nodes",
            "staged_precompute_nodes",
        ] {
            legacy.as_object_mut().unwrap().remove(field);
        }
        let decoded: SemanticAnalysisStructureWork = serde_json::from_value(legacy).unwrap();
        assert_eq!(
            decoded,
            SemanticAnalysisStructureWork {
                ..SemanticAnalysisStructureWork::default()
            }
        );
    }

    #[test]
    fn older_semantic_body_structure_defaults_inline_scan_bodies() {
        let mut structure = serde_json::to_value(SemanticAnalysisStructureWork {
            precompute_inline_scan_bodies: 7,
            ..SemanticAnalysisStructureWork::default()
        })
        .unwrap();
        structure
            .as_object_mut()
            .unwrap()
            .remove("precompute_inline_scan_bodies");
        let decoded: SemanticAnalysisStructureWork = serde_json::from_value(structure).unwrap();
        assert_eq!(decoded.precompute_inline_scan_bodies, 0);
    }

    #[test]
    fn older_semantic_provider_work_defaults_additive_partitions() {
        let mut encoded = serde_json::to_value(SemanticProviderWork {
            materializations: 23,
            ..SemanticProviderWork::default()
        })
        .unwrap();
        let object = encoded.as_object_mut().unwrap();
        for field in [
            "shared_payload_materializations",
            "owned_payload_materializations",
            "const_materializations",
            "nominal_materializations",
            "function_materializations",
            "method_materializations",
            "nominal_materialization_reuses",
            "function_materialization_reuses",
            "import_nominal_registration_requests",
            "import_nominal_type_visits",
            "import_named_nominal_probes",
            "import_named_nominal_complete_hits",
            "import_named_nominal_cycle_hits",
            "import_named_nominals_registered",
            "import_nominal_type_edges_traversed",
            "import_anonymous_nominals_registered",
        ] {
            object.remove(field);
        }
        let decoded: SemanticProviderWork = serde_json::from_value(encoded).unwrap();
        assert_eq!(decoded.materializations, 23);
        assert_eq!(decoded.shared_payload_materializations, 0);
        assert_eq!(decoded.owned_payload_materializations, 0);
        assert_eq!(decoded.const_materializations, 0);
        assert_eq!(decoded.nominal_materializations, 0);
        assert_eq!(decoded.function_materializations, 0);
        assert_eq!(decoded.method_materializations, 0);
        assert_eq!(decoded.nominal_materialization_reuses, 0);
        assert_eq!(decoded.function_materialization_reuses, 0);
        assert_eq!(decoded.import_nominal_registration_requests, 0);
        assert_eq!(decoded.import_nominal_type_visits, 0);
        assert_eq!(decoded.import_named_nominal_probes, 0);
        assert_eq!(decoded.import_named_nominal_complete_hits, 0);
        assert_eq!(decoded.import_named_nominal_cycle_hits, 0);
        assert_eq!(decoded.import_named_nominals_registered, 0);
        assert_eq!(decoded.import_nominal_type_edges_traversed, 0);
        assert_eq!(decoded.import_anonymous_nominals_registered, 0);
    }

    #[test]
    fn older_cfg_materialization_work_defaults_selected_epoch_inputs() {
        let mut encoded = serde_json::to_value(CfgMaterializationWork {
            fact_selections: 11,
            ..CfgMaterializationWork::default()
        })
        .unwrap();
        let object = encoded.as_object_mut().unwrap();
        for field in [
            "declarations_selected",
            "anonymous_nominals_selected",
            "callables_selected",
            "nominal_metadata_selected",
            "modules_selected",
            "builtin_nominals_selected",
            "required_types_selected",
        ] {
            object.remove(field);
        }
        let decoded: CfgMaterializationWork = serde_json::from_value(encoded).unwrap();
        assert_eq!(decoded.fact_selections, 11);
        assert_eq!(decoded.declarations_selected, 0);
        assert_eq!(decoded.anonymous_nominals_selected, 0);
        assert_eq!(decoded.callables_selected, 0);
        assert_eq!(decoded.nominal_metadata_selected, 0);
        assert_eq!(decoded.modules_selected, 0);
        assert_eq!(decoded.builtin_nominals_selected, 0);
        assert_eq!(decoded.required_types_selected, 0);
    }

    #[test]
    fn older_compiler_work_defaults_local_epoch_accounting() {
        let mut encoded = serde_json::to_value(CompilerWork::default()).unwrap();
        encoded.as_object_mut().unwrap().remove("cfg_local_epoch");
        let decoded: CompilerWork = serde_json::from_value(encoded).unwrap();
        assert_eq!(decoded.cfg_local_epoch, CfgLocalEpochWork::default());
    }
}
