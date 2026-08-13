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
pub const SCALING_REPORT_SCHEMA_VERSION: u32 = 17;

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
    /// Exact semantic facts observed by provider-native body analysis.
    pub semantic_provider: SemanticProviderWork,
    /// Work performed while discovering and scheduling reachable bodies.
    pub semantic_reachability: SemanticReachabilityWork,
    /// Request-local lookup preparation for exact CFG materialization facts.
    pub cfg_materialization: CfgMaterializationWork,
    /// Stable-type scans and registered prerequisite requests for CFG bodies.
    #[serde(default)]
    pub cfg_prerequisites: CfgPrerequisiteWork,
    /// Logical retained-charge bookkeeping for CFG artifacts.
    #[serde(default)]
    pub cfg_retained_charge: CfgRetainedChargeWork,
    /// Work performed by the revisioned query runtime.
    pub query_runtime: QueryRuntimeWork,
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
    /// Anonymous-nominal fact reads.
    pub anonymous_facts: u64,
    /// Anonymous-producer body fact reads.
    pub producer_facts: u64,
    /// Trusted-toolchain fact reads.
    pub toolchain_facts: u64,
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
    /// Unique type-fact terminals requested for drop-relevant types.
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
    fn older_compiler_work_defaults_additive_cfg_categories() {
        let mut encoded = serde_json::to_value(CompilerWork::default()).unwrap();
        let object = encoded.as_object_mut().unwrap();
        object.remove("cfg_prerequisites");
        object.remove("cfg_retained_charge");
        let decoded: CompilerWork = serde_json::from_value(encoded).unwrap();
        assert_eq!(decoded.cfg_prerequisites, CfgPrerequisiteWork::default());
        assert_eq!(
            decoded.cfg_retained_charge,
            CfgRetainedChargeWork::default()
        );
    }

    #[test]
    fn older_semantic_provider_work_defaults_the_materialization_partition() {
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
}
