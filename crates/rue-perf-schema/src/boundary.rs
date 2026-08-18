//! Machine-checkable source-to-native build-boundary evidence (ADR-0071).
//!
//! The manifest, external runner, and compiler each describe the same
//! observation independently. A reference sample is admissible only when the
//! three descriptions agree exactly; new product modes require new exhaustive
//! enum variants rather than quietly widening this boundary.

use serde::{Deserialize, Serialize};

use crate::CompilerWork;
use crate::sanity::is_sha256_digest;

/// The complete product boundary measured by one compiler process.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BuildBoundary {
    /// A clean CLI process reads source and emits one native executable.
    FreshSourceToNativeV1,
}

/// The compiler pipeline admitted by the reference boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompilerPipeline {
    /// `CompilerSession`'s canonical rooted query graph and one-shot adapter.
    CanonicalRootedQueryGraphV1,
}

/// Rust implementation profile of the compiler binary under measurement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompilerBuildProfile {
    /// Rue's release platform: optimized Rust with ThinLTO.
    ReleaseThinLto,
    /// A development binary. Never admitted by the ADR-0071 reference regime.
    Debug,
}

/// Rue optimization contract for the produced program.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OptimizationLevel {
    O0,
    O1,
    O2,
    O3,
}

/// Link implementation used to produce the executable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LinkPolicy {
    /// Rue's built-in linker.
    Internal,
    /// An external command selected by the caller.
    System,
}

/// Product emitted at the end of the measured invocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputKind {
    NativeExecutable,
}

/// Worker rows declared by ADR-0071's scaling matrix.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkerSetting {
    One,
    Two,
    Four,
    Eight,
    Automatic,
}

impl WorkerSetting {
    /// Ordered worker matrix required by ADR-0071's reference scaling report.
    pub const REFERENCE_MATRIX: [Self; 5] = [
        Self::One,
        Self::Two,
        Self::Four,
        Self::Eight,
        Self::Automatic,
    ];

    /// CLI `--jobs` value represented by this row (`0` means automatic).
    pub const fn jobs(self) -> usize {
        match self {
            Self::One => 1,
            Self::Two => 2,
            Self::Four => 4,
            Self::Eight => 8,
            Self::Automatic => 0,
        }
    }

    /// Classify the explicit CLI value used by the compiler.
    pub const fn from_jobs(jobs: usize) -> Option<Self> {
        match jobs {
            0 => Some(Self::Automatic),
            1 => Some(Self::One),
            2 => Some(Self::Two),
            4 => Some(Self::Four),
            8 => Some(Self::Eight),
            _ => None,
        }
    }
}

/// Filesystem input classes admitted by the reference boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompilerInputClass {
    /// Source in the workload's frozen root closure.
    WorkloadSource,
    /// Source selected from the configured trusted standard library.
    TrustedStandardLibrarySource,
}

/// Compiler-owned data linked without a filesystem read by this invocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EmbeddedAssetClass {
    /// The target-specific `rue-runtime` archive embedded in the compiler.
    BundledRuntimeArchive,
}

/// Successful milestones which must all occur before a v1 observation exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompilerStage {
    SourceDiscoveryAndParsing,
    ProgramConstruction,
    SemanticAnalysis,
    CfgAndOptimization,
    Backend,
    ObjectGeneration,
    Linking,
    OutputPublication,
}

impl CompilerStage {
    /// Exact ordered completion sequence for `fresh_source_to_native_v1`.
    pub const FRESH_SOURCE_TO_NATIVE_V1: [Self; 8] = [
        Self::SourceDiscoveryAndParsing,
        Self::ProgramConstruction,
        Self::SemanticAnalysis,
        Self::CfgAndOptimization,
        Self::Backend,
        Self::ObjectGeneration,
        Self::Linking,
        Self::OutputPublication,
    ];
}

/// Manifest authority for one measured invocation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BuildBoundaryPolicy {
    pub boundary: BuildBoundary,
    pub pipeline: CompilerPipeline,
    pub compiler_build_profile: CompilerBuildProfile,
    pub optimization: OptimizationLevel,
    pub linker: LinkPolicy,
    pub output_kind: OutputKind,
    pub worker_setting: WorkerSetting,
    pub allowed_input_classes: Vec<CompilerInputClass>,
    pub allowed_embedded_asset_classes: Vec<EmbeddedAssetClass>,
    pub required_stages: Vec<CompilerStage>,
}

impl BuildBoundaryPolicy {
    /// The sole policy currently admitted to the ADR-0071 reference series.
    pub fn fresh_source_to_native_v1(worker_setting: WorkerSetting) -> Self {
        Self {
            boundary: BuildBoundary::FreshSourceToNativeV1,
            pipeline: CompilerPipeline::CanonicalRootedQueryGraphV1,
            compiler_build_profile: CompilerBuildProfile::ReleaseThinLto,
            optimization: OptimizationLevel::O3,
            linker: LinkPolicy::Internal,
            output_kind: OutputKind::NativeExecutable,
            worker_setting,
            allowed_input_classes: vec![
                CompilerInputClass::WorkloadSource,
                CompilerInputClass::TrustedStandardLibrarySource,
            ],
            allowed_embedded_asset_classes: vec![EmbeddedAssetClass::BundledRuntimeArchive],
            required_stages: CompilerStage::FRESH_SOURCE_TO_NATIVE_V1.to_vec(),
        }
    }

    /// Reject a policy which gives the known variant broader or different
    /// meaning than its exhaustive schema definition.
    pub fn validate(&self) -> Result<(), String> {
        let expected = Self::fresh_source_to_native_v1(self.worker_setting);
        if self == &expected {
            Ok(())
        } else {
            Err(format!(
                "fresh_source_to_native_v1 policy differs from its exhaustive contract: expected {expected:?}, found {self:?}"
            ))
        }
    }

    /// Canonical CLI arguments whose independent parse must produce this
    /// policy. Target and output paths are supplied by dedicated runner fields.
    pub fn canonical_compiler_args(&self) -> Vec<String> {
        vec![
            match self.optimization {
                OptimizationLevel::O0 => "-O0",
                OptimizationLevel::O1 => "-O1",
                OptimizationLevel::O2 => "-O2",
                OptimizationLevel::O3 => "-O3",
            }
            .to_string(),
            format!("-j{}", self.worker_setting.jobs()),
        ]
    }
}

/// One exact accepted source input reported from the compiler snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompilerInputEvidence {
    pub class: CompilerInputClass,
    /// Canonical compiler module identity, independent of checkout location.
    pub logical_identity: String,
    /// SHA-256 of the immutable source bytes consumed by the compiler.
    pub sha256: String,
}

/// One compiler-embedded input selected by the build.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EmbeddedAssetEvidence {
    pub class: EmbeddedAssetClass,
    /// Stable logical identity. The compiler binary digest owns the bytes.
    pub logical_identity: String,
    pub target: String,
}

/// Product configuration the compiler actually executed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompilerConfigurationEvidence {
    pub target: String,
    pub compiler_build_profile: CompilerBuildProfile,
    pub optimization: OptimizationLevel,
    pub linker: LinkPolicy,
    pub output_kind: OutputKind,
    pub requested_workers: WorkerSetting,
    pub resolved_workers: u32,
    /// Empty in the reference regime; explicit so a preview cannot enter
    /// without changing the evidence.
    pub preview_features: Vec<String>,
    /// Zero in the reference regime; external archives are undeclared inputs.
    pub external_link_archives: u32,
}

/// Artifact sources excluded from the fresh v1 boundary.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactHitEvidence {
    pub retained_session_inputs: u64,
    pub daemon_handoffs: u64,
    pub persistent_cache_hits: u64,
    pub precompiled_program_hits: u64,
    pub precompiled_package_hits: u64,
    pub precompiled_standard_artifact_hits: u64,
}

impl ArtifactHitEvidence {
    pub const fn is_zero(self) -> bool {
        self.retained_session_inputs == 0
            && self.daemon_handoffs == 0
            && self.persistent_cache_hits == 0
            && self.precompiled_program_hits == 0
            && self.precompiled_package_hits == 0
            && self.precompiled_standard_artifact_hits == 0
    }
}

/// Compiler authority emitted only after successful output publication.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompilerBoundaryEvidence {
    pub boundary: BuildBoundary,
    pub pipeline: CompilerPipeline,
    pub session_count: u32,
    pub root_request_count: u32,
    pub configuration: CompilerConfigurationEvidence,
    pub completed_stages: Vec<CompilerStage>,
    pub accepted_inputs: Vec<CompilerInputEvidence>,
    pub embedded_assets: Vec<EmbeddedAssetEvidence>,
    pub artifact_hits: ArtifactHitEvidence,
    pub emitted_output_sha256: String,
    pub emitted_output_size_bytes: u64,
}

/// External clock semantics used by the runner.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunnerClockBoundary {
    /// Starts immediately before spawn; ends after exit and output verification.
    MonotonicPreSpawnThroughExitAndOutputVerification,
}

/// Independent process facts established by the external runner.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunnerBoundaryEvidence {
    pub compiler_binary_sha256: String,
    pub fresh_state_directory: bool,
    pub fresh_output_directory: bool,
    pub daemon_endpoint_supplied: bool,
    pub retained_session_handle_supplied: bool,
    pub clock_boundary: RunnerClockBoundary,
    pub successful_exit: bool,
    pub native_output_verified: bool,
    pub output_sha256: String,
    pub output_size_bytes: u64,
}

/// A bounded log2 histogram of durations in nanoseconds.
///
/// Producers update thread-local buckets and merge them only at bounded worker
/// completion. The 64 buckets cover every `u64` duration without retaining one
/// record per body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DurationDistribution {
    pub count: u64,
    pub total_ns: u64,
    pub max_ns: u64,
    pub log2_buckets: Vec<u64>,
}

impl Default for DurationDistribution {
    fn default() -> Self {
        Self {
            count: 0,
            total_ns: 0,
            max_ns: 0,
            log2_buckets: vec![0; 64],
        }
    }
}

impl DurationDistribution {
    pub fn validate(&self) -> bool {
        self.log2_buckets.len() == 64
            && self
                .log2_buckets
                .iter()
                .copied()
                .fold(0_u64, u64::saturating_add)
                == self.count
            && (self.count != 0 || (self.total_ns == 0 && self.max_ns == 0))
            && self.max_ns <= self.total_ns
    }
}

/// Bounded compiler-side evidence explaining a worker-scaling observation.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompilerCriticalPathEvidence {
    /// Active execution-permit time, summed across independent query workers.
    pub query_worker_active_ns: u64,
    /// Dependency-ready registered batch items observed by workers.
    pub ready_items: u64,
    pub ready_wait_ns: u64,
    pub max_ready_wait_ns: u64,
    /// Longest nested query/batch ancestry in number of producer nodes.
    pub longest_query_dependency_chain: u64,
    pub peak_query_workers: u64,
    /// Inclusive duration of reached-toolchain acquisition inside compiler root.
    pub toolchain_acquisition_ns: u64,
    pub semantic_bodies: DurationDistribution,
    /// Registered-query prerequisites acquired before each body analysis.
    #[serde(default)]
    pub semantic_prerequisite_bodies: DurationDistribution,
    /// Inclusive request-wide declaration graph collection.
    #[serde(default)]
    pub semantic_declaration_graph: DurationDistribution,
    /// Module-local declaration occurrence index requests inside that graph.
    #[serde(default)]
    pub semantic_declaration_occurrence_indexes: DurationDistribution,
    /// Exact declaration-nucleus requests inside that graph.
    #[serde(default)]
    pub semantic_declaration_nuclei: DurationDistribution,
    /// Exact signature-fragment parsing across semantic query consumers.
    ///
    /// Permanently absent from a current producer. RUE-1510 replaced signature
    /// source reconstruction and parser re-entry with a projection from the
    /// canonical parsed declaration, so no span opens and this stays at its
    /// default. Unlike the three deleted body-input intervals below, which are
    /// still entered once per materialization and report zero nanoseconds, the
    /// count itself is zero. The field is retained because immutable historical
    /// records carry real values here and `deny_unknown_fields` would refuse
    /// them without it, and
    /// [`BuildBoundaryEvidence::validate_current_producer_against`] now proves
    /// the deletion instead of the work.
    #[serde(default)]
    pub semantic_declaration_signature_parsing: DurationDistribution,
    /// Exact signature projection from the canonical parsed declaration.
    ///
    /// The positive half of the pair above. RUE-1514 proved the deletion of
    /// signature parsing, which constrains what must *not* happen; a producer
    /// that removed or short-circuited projection entirely satisfied that and
    /// published nothing here. RUE-1515 requires this non-zero so the boundary
    /// covers the path that replaced parsing, not only the path it deleted.
    #[serde(default)]
    pub semantic_declaration_signature_projection: DurationDistribution,
    /// Inclusive rooted body-closure query and immediate work reduction.
    #[serde(default)]
    pub semantic_body_closure: DurationDistribution,
    /// Projection of the closure and declaration graph into the rooted body graph.
    #[serde(default)]
    pub semantic_body_graph_projection: DurationDistribution,
    /// Every body-fragment lowering attempt, including attempts that return an
    /// error for ordinary semantic recovery.
    #[serde(default)]
    pub semantic_body_input_lowering: DurationDistribution,
    /// Successful named-body plan materializations only, partitioned by the
    /// five same-clock adapter intervals below. The first three are retained
    /// as zero-valued schema evidence after deletion of body-local assembly,
    /// parsing, and RIR lowering. The historical remap/validation interval now
    /// covers current-span projection, temporary base-symbol reconstruction,
    /// and validation until rue-air consumes the immutable plan directly.
    #[serde(default)]
    pub semantic_body_input_attributed_total: DurationDistribution,
    #[serde(default)]
    pub semantic_body_input_assembly_snapshot: DurationDistribution,
    #[serde(default)]
    pub semantic_body_input_lex_parse: DurationDistribution,
    #[serde(default)]
    pub semantic_body_input_rir_lower: DurationDistribution,
    #[serde(default)]
    pub semantic_body_input_span_remap_validation: DurationDistribution,
    #[serde(default)]
    pub semantic_body_input_rir_index: DurationDistribution,
    /// Every provider-backed semantic-analysis attempt after body-fragment
    /// lowering, including attempts that return an ordinary semantic error.
    #[serde(default)]
    pub semantic_provider_analysis: DurationDistribution,
    /// Provider-host construction and pre-engine body setup for successful
    /// provider analyses. The five provider-detail fields through result
    /// projection share this success denominator.
    #[serde(default)]
    pub semantic_provider_host_setup: DurationDistribution,
    /// Canonical expression inference and body analysis engine.
    #[serde(default)]
    pub semantic_provider_expression_engine: DurationDistribution,
    /// Parameter/layout setup before expression inference.
    #[serde(default)]
    pub semantic_expression_setup: DurationDistribution,
    /// Compile-time alias and inline-constructor preparation before constraints.
    #[serde(default)]
    pub semantic_inference_precompute: DurationDistribution,
    #[serde(default)]
    pub semantic_inference_precompute_structural: DurationDistribution,
    #[serde(default)]
    pub semantic_inference_precompute_eval_provider: DurationDistribution,
    /// Constraint generation over the body RIR.
    #[serde(default)]
    pub semantic_constraint_generation: DurationDistribution,
    /// Constraint solving and concrete resolved-type projection.
    #[serde(default)]
    pub semantic_unification_resolution: DurationDistribution,
    /// AIR construction plus post-analysis semantic validation.
    #[serde(default)]
    pub semantic_air_emission_validation: DurationDistribution,
    /// Post-analysis specialization selection and identity installation.
    #[serde(default)]
    pub semantic_provider_specialization_selection: DurationDistribution,
    /// Projection from analyzed local AIR into the durable semantic body.
    #[serde(default)]
    pub semantic_provider_body_export: DurationDistribution,
    /// Final durable references, tokens, and produced-nominal projection.
    #[serde(default)]
    pub semantic_provider_result_projection: DurationDistribution,
    /// Stable-input preparation before body-local semantic materialization.
    #[serde(default)]
    pub cfg_input_preparation_bodies: DurationDistribution,
    /// Fresh body-local semantic epoch construction and body import.
    #[serde(default)]
    pub semantic_materialization_bodies: DurationDistribution,
    /// Stable-domain projection and layout/drop prerequisite queries.
    #[serde(default)]
    pub cfg_domain_prerequisite_bodies: DurationDistribution,
    /// Projection from the live body epoch into stable CFG identities.
    #[serde(default)]
    pub cfg_domain_projection_bodies: DurationDistribution,
    /// Unique layout/drop prerequisite discovery and request-key preparation.
    #[serde(default)]
    pub cfg_prerequisite_collection_bodies: DurationDistribution,
    /// Registered layout, type-fact, and drop-glue prerequisite queries.
    #[serde(default)]
    pub cfg_prerequisite_query_bodies: DurationDistribution,
    /// AIR-to-CFG construction excluding publication and ABI projection.
    #[serde(default)]
    pub cfg_builder_bodies: DurationDistribution,
    /// Runtime-call ABI projection, retained-charge accounting, and publication.
    #[serde(default)]
    pub cfg_publication_bodies: DurationDistribution,
    pub cfg_construction_bodies: DurationDistribution,
    pub cfg_optimization_bodies: DurationDistribution,
    /// Contention evidence the runtime can support exactly.
    pub joins: u64,
    pub declined_joins: u64,
    pub donated_permits: u64,
}

/// Semantic critical-path counters a current producer must actually report,
/// each with the schema field name the rejection names.
///
/// A table rather than a chain of `||` so the check can say which entry failed.
/// Every row is evidence that a phase ran at all; a row is removed only when
/// the work it timed is deleted from the compiler, and then the deletion itself
/// becomes the assertion rather than the row simply disappearing.
type SemanticEvidenceRow = (
    &'static str,
    fn(&CompilerCriticalPathEvidence) -> &DurationDistribution,
);

const REQUIRED_SEMANTIC_EVIDENCE: [SemanticEvidenceRow; 27] = [
    ("semantic_prerequisite_bodies", |critical| {
        &critical.semantic_prerequisite_bodies
    }),
    ("semantic_declaration_graph", |critical| {
        &critical.semantic_declaration_graph
    }),
    ("semantic_declaration_occurrence_indexes", |critical| {
        &critical.semantic_declaration_occurrence_indexes
    }),
    ("semantic_declaration_nuclei", |critical| {
        &critical.semantic_declaration_nuclei
    }),
    ("semantic_body_closure", |critical| {
        &critical.semantic_body_closure
    }),
    ("semantic_body_graph_projection", |critical| {
        &critical.semantic_body_graph_projection
    }),
    ("semantic_body_input_lowering", |critical| {
        &critical.semantic_body_input_lowering
    }),
    ("semantic_body_input_attributed_total", |critical| {
        &critical.semantic_body_input_attributed_total
    }),
    ("semantic_body_input_assembly_snapshot", |critical| {
        &critical.semantic_body_input_assembly_snapshot
    }),
    ("semantic_body_input_lex_parse", |critical| {
        &critical.semantic_body_input_lex_parse
    }),
    ("semantic_body_input_rir_lower", |critical| {
        &critical.semantic_body_input_rir_lower
    }),
    ("semantic_body_input_span_remap_validation", |critical| {
        &critical.semantic_body_input_span_remap_validation
    }),
    ("semantic_body_input_rir_index", |critical| {
        &critical.semantic_body_input_rir_index
    }),
    ("semantic_declaration_signature_projection", |critical| {
        &critical.semantic_declaration_signature_projection
    }),
    ("semantic_provider_analysis", |critical| {
        &critical.semantic_provider_analysis
    }),
    ("semantic_provider_host_setup", |critical| {
        &critical.semantic_provider_host_setup
    }),
    ("semantic_provider_expression_engine", |critical| {
        &critical.semantic_provider_expression_engine
    }),
    ("semantic_expression_setup", |critical| {
        &critical.semantic_expression_setup
    }),
    ("semantic_inference_precompute", |critical| {
        &critical.semantic_inference_precompute
    }),
    ("semantic_inference_precompute_structural", |critical| {
        &critical.semantic_inference_precompute_structural
    }),
    ("semantic_inference_precompute_eval_provider", |critical| {
        &critical.semantic_inference_precompute_eval_provider
    }),
    ("semantic_constraint_generation", |critical| {
        &critical.semantic_constraint_generation
    }),
    ("semantic_unification_resolution", |critical| {
        &critical.semantic_unification_resolution
    }),
    ("semantic_air_emission_validation", |critical| {
        &critical.semantic_air_emission_validation
    }),
    ("semantic_provider_specialization_selection", |critical| {
        &critical.semantic_provider_specialization_selection
    }),
    ("semantic_provider_body_export", |critical| {
        &critical.semantic_provider_body_export
    }),
    ("semantic_provider_result_projection", |critical| {
        &critical.semantic_provider_result_projection
    }),
];

/// Complete evidence attached to one fresh compiler process.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BuildBoundaryEvidence {
    pub runner: RunnerBoundaryEvidence,
    pub compiler: CompilerBoundaryEvidence,
    pub critical_path: CompilerCriticalPathEvidence,
    /// Deterministic work for this exact compiler process.
    pub compiler_work: CompilerWork,
}

impl BuildBoundaryEvidence {
    /// Cross-check runner and compiler facts against the independently declared
    /// manifest policy. Returns the first disagreement so a producer can reject
    /// the process before its sample reaches storage.
    pub fn validate_against(
        &self,
        policy: &BuildBoundaryPolicy,
        target: &str,
    ) -> Result<(), String> {
        policy.validate()?;
        let runner = &self.runner;
        if !is_sha256_digest(&runner.compiler_binary_sha256) {
            return Err("runner reported an invalid compiler binary SHA-256".to_string());
        }
        if !runner.fresh_state_directory || !runner.fresh_output_directory {
            return Err("runner did not create fresh state and output directories".to_string());
        }
        if runner.daemon_endpoint_supplied || runner.retained_session_handle_supplied {
            return Err("runner supplied a daemon or retained-session handle".to_string());
        }
        if runner.clock_boundary
            != RunnerClockBoundary::MonotonicPreSpawnThroughExitAndOutputVerification
        {
            return Err("runner used an unsupported clock boundary".to_string());
        }
        if !runner.successful_exit || !runner.native_output_verified {
            return Err(
                "compiler did not exit successfully with verified native output".to_string(),
            );
        }
        if !is_sha256_digest(&runner.output_sha256) {
            return Err("runner reported an invalid output SHA-256".to_string());
        }
        if runner.output_size_bytes == 0 {
            return Err("runner reported an empty output".to_string());
        }

        let compiler = &self.compiler;
        if compiler.boundary != policy.boundary || compiler.pipeline != policy.pipeline {
            return Err("compiler boundary or pipeline identity disagrees with policy".to_string());
        }
        if compiler.session_count != 1 || compiler.root_request_count != 1 {
            return Err(format!(
                "compiler reported {} sessions and {} root requests",
                compiler.session_count, compiler.root_request_count
            ));
        }
        let configuration = &compiler.configuration;
        if configuration.target != target
            || configuration.compiler_build_profile != policy.compiler_build_profile
            || configuration.optimization != policy.optimization
            || configuration.linker != policy.linker
            || configuration.output_kind != policy.output_kind
            || configuration.requested_workers != policy.worker_setting
        {
            return Err(format!(
                "compiler configuration {configuration:?} disagrees with target {target:?} and policy {policy:?}"
            ));
        }
        let resolved_expected = match policy.worker_setting {
            WorkerSetting::One => Some(1),
            WorkerSetting::Two => Some(2),
            WorkerSetting::Four => Some(4),
            WorkerSetting::Eight => Some(8),
            WorkerSetting::Automatic => None,
        };
        if configuration.resolved_workers == 0
            || resolved_expected.is_some_and(|expected| configuration.resolved_workers != expected)
        {
            return Err(format!(
                "worker setting {:?} resolved to {}",
                policy.worker_setting, configuration.resolved_workers
            ));
        }
        if !configuration.preview_features.is_empty() || configuration.external_link_archives != 0 {
            return Err(
                "preview features or external link archives entered the reference build"
                    .to_string(),
            );
        }
        if compiler.completed_stages != policy.required_stages {
            return Err("compiler completion stages disagree with policy".to_string());
        }
        if !compiler.artifact_hits.is_zero() {
            return Err(format!(
                "excluded retained/daemon/cache/precompiled artifacts were used: {:?}",
                compiler.artifact_hits
            ));
        }
        if !is_sha256_digest(&compiler.emitted_output_sha256)
            || compiler.emitted_output_sha256 != runner.output_sha256
            || compiler.emitted_output_size_bytes != runner.output_size_bytes
        {
            return Err("compiler and runner output evidence disagrees".to_string());
        }

        let mut previous_input: Option<(&CompilerInputClass, &str)> = None;
        let mut saw_workload = false;
        for input in &compiler.accepted_inputs {
            if !policy.allowed_input_classes.contains(&input.class) {
                return Err(format!(
                    "compiler reported disallowed input class {:?}",
                    input.class
                ));
            }
            if input.logical_identity.is_empty() || !is_sha256_digest(&input.sha256) {
                return Err("compiler reported malformed input provenance".to_string());
            }
            let identity = (&input.class, input.logical_identity.as_str());
            if previous_input.is_some_and(|previous| previous >= identity) {
                return Err(
                    "compiler input provenance is duplicated or not canonically sorted".to_string(),
                );
            }
            previous_input = Some(identity);
            saw_workload |= input.class == CompilerInputClass::WorkloadSource;
        }
        if !saw_workload {
            return Err("compiler reported no workload source input".to_string());
        }

        if compiler.embedded_assets.len() != policy.allowed_embedded_asset_classes.len() {
            return Err("compiler embedded-asset count disagrees with policy".to_string());
        }
        for (asset, expected_class) in compiler
            .embedded_assets
            .iter()
            .zip(&policy.allowed_embedded_asset_classes)
        {
            if &asset.class != expected_class
                || asset.logical_identity.is_empty()
                || asset.target != target
            {
                return Err(format!(
                    "compiler reported invalid embedded asset {asset:?}"
                ));
            }
        }

        let critical = &self.critical_path;
        if !critical.semantic_bodies.validate()
            || !critical.semantic_prerequisite_bodies.validate()
            || !critical.semantic_declaration_graph.validate()
            || !critical.semantic_declaration_occurrence_indexes.validate()
            || !critical.semantic_declaration_nuclei.validate()
            || !critical.semantic_declaration_signature_parsing.validate()
            || !critical.semantic_body_closure.validate()
            || !critical.semantic_body_graph_projection.validate()
            || !critical.semantic_body_input_lowering.validate()
            || !critical.semantic_body_input_attributed_total.validate()
            || !critical.semantic_body_input_assembly_snapshot.validate()
            || !critical.semantic_body_input_lex_parse.validate()
            || !critical.semantic_body_input_rir_lower.validate()
            || !critical
                .semantic_body_input_span_remap_validation
                .validate()
            || !critical.semantic_body_input_rir_index.validate()
            || !critical.semantic_provider_analysis.validate()
            || !critical.semantic_provider_host_setup.validate()
            || !critical.semantic_provider_expression_engine.validate()
            || !critical.semantic_expression_setup.validate()
            || !critical.semantic_inference_precompute.validate()
            || !critical.semantic_inference_precompute_structural.validate()
            || !critical
                .semantic_inference_precompute_eval_provider
                .validate()
            || !critical.semantic_constraint_generation.validate()
            || !critical.semantic_unification_resolution.validate()
            || !critical.semantic_air_emission_validation.validate()
            || !critical
                .semantic_provider_specialization_selection
                .validate()
            || !critical.semantic_provider_body_export.validate()
            || !critical.semantic_provider_result_projection.validate()
            || !critical.cfg_input_preparation_bodies.validate()
            || !critical.semantic_materialization_bodies.validate()
            || !critical.cfg_domain_prerequisite_bodies.validate()
            || !critical.cfg_domain_projection_bodies.validate()
            || !critical.cfg_prerequisite_collection_bodies.validate()
            || !critical.cfg_prerequisite_query_bodies.validate()
            || !critical.cfg_builder_bodies.validate()
            || !critical.cfg_publication_bodies.validate()
            || !critical.cfg_construction_bodies.validate()
            || !critical.cfg_optimization_bodies.validate()
        {
            return Err("compiler reported an invalid bounded duration distribution".to_string());
        }
        if critical.query_worker_active_ns == 0
            || critical.longest_query_dependency_chain == 0
            || critical.semantic_bodies.count == 0
            || critical.cfg_construction_bodies.count == 0
            || critical.cfg_optimization_bodies.count == 0
        {
            return Err("compiler omitted required critical-path evidence".to_string());
        }
        if critical.ready_items == 0 {
            if critical.ready_wait_ns != 0 || critical.max_ready_wait_ns != 0 {
                return Err("ready-wait duration exists without ready items".to_string());
            }
        } else if critical.max_ready_wait_ns > critical.ready_wait_ns {
            return Err("maximum ready wait exceeds total ready wait".to_string());
        }
        if critical.peak_query_workers == 0
            || critical.peak_query_workers > u64::from(configuration.resolved_workers)
        {
            return Err("peak query workers is outside the resolved worker budget".to_string());
        }
        Ok(())
    }

    /// Validate evidence emitted by the compiler version linked into the
    /// current measurement runner.
    ///
    /// Stored run objects use additive defaults so newer readers can continue
    /// to validate immutable historical evidence. The current producer has no
    /// such exception: every field it knows about must be populated.
    pub fn validate_current_producer_against(
        &self,
        policy: &BuildBoundaryPolicy,
        target: &str,
    ) -> Result<(), String> {
        self.validate_against(policy, target)?;
        let critical = &self.critical_path;
        // Named, not a boolean chain. One message for every counter says only
        // that some evidence is absent, which is what a maintainer gets from CI
        // when this fires; diagnosing RUE-1514 from it took reading this
        // function and bisecting the published data branch to find which of the
        // conditions had gone false.
        let omitted = REQUIRED_SEMANTIC_EVIDENCE
            .iter()
            .filter(|(_, select)| select(critical).count == 0)
            .map(|(name, _)| *name)
            .collect::<Vec<_>>();
        if !omitted.is_empty() {
            return Err(format!(
                "compiler omitted semantic critical-path evidence: {}",
                omitted.join(", ")
            ));
        }
        // The one counter deleted rather than omitted, held to the opposite
        // rule. RUE-1510 replaced signature source reconstruction and parser
        // re-entry with a projection from the canonical parsed declaration, so
        // a current producer opens no such span. Requiring it to be non-zero
        // rejected every process from that commit onward; simply dropping the
        // entry would leave the boundary with nothing to say about a frontend
        // path this variant no longer describes. Proving the deletion keeps
        // the evidence fail-closed in the direction that is still falsifiable.
        if critical.semantic_declaration_signature_parsing.count != 0 {
            return Err(format!(
                "semantic_declaration_signature_parsing reported {} invocations; RUE-1510 deleted \
                 signature parsing, so a fresh_source_to_native_v1 process performing one is \
                 running a frontend path this boundary variant does not describe",
                critical.semantic_declaration_signature_parsing.count
            ));
        }
        let lowering_counts = [
            critical.semantic_body_input_attributed_total.count,
            critical.semantic_body_input_assembly_snapshot.count,
            critical.semantic_body_input_lex_parse.count,
            critical.semantic_body_input_rir_lower.count,
            critical.semantic_body_input_span_remap_validation.count,
            critical.semantic_body_input_rir_index.count,
        ];
        let successful_lowerings = critical.semantic_body_input_attributed_total.count;
        if lowering_counts
            .iter()
            .any(|count| *count != successful_lowerings)
            || successful_lowerings > critical.semantic_body_input_lowering.count
            || critical.semantic_body_input_attributed_total.total_ns
                != critical
                    .semantic_body_input_assembly_snapshot
                    .total_ns
                    .saturating_add(critical.semantic_body_input_lex_parse.total_ns)
                    .saturating_add(critical.semantic_body_input_rir_lower.total_ns)
                    .saturating_add(critical.semantic_body_input_span_remap_validation.total_ns)
                    .saturating_add(critical.semantic_body_input_rir_index.total_ns)
        {
            return Err("compiler body-lowering attribution is incomplete".to_string());
        }
        let successful_provider_analyses = critical.semantic_provider_expression_engine.count;
        let provider_counts = [
            critical.semantic_provider_host_setup.count,
            critical.semantic_provider_expression_engine.count,
            critical.semantic_provider_specialization_selection.count,
            critical.semantic_provider_body_export.count,
            critical.semantic_provider_result_projection.count,
        ];
        if provider_counts
            .iter()
            .any(|count| *count != successful_provider_analyses)
            || successful_provider_analyses > critical.semantic_provider_analysis.count
        {
            return Err("compiler provider-analysis attribution is incomplete".to_string());
        }
        if critical.semantic_inference_precompute_structural.count
            != critical.semantic_inference_precompute.count
            || critical.semantic_inference_precompute_eval_provider.count
                != critical.semantic_inference_precompute.count
            || critical.semantic_inference_precompute.total_ns
                != critical
                    .semantic_inference_precompute_structural
                    .total_ns
                    .saturating_add(
                        critical
                            .semantic_inference_precompute_eval_provider
                            .total_ns,
                    )
        {
            return Err("compiler precompute attribution is incomplete".to_string());
        }
        let structure = self.compiler_work.semantic_body_structure;
        if structure.body_lowerings != successful_lowerings
            || structure.index_builds != structure.body_lowerings
            || structure.rir_instructions != structure.index_rir_instructions_visited
            || structure.precompute_bodies != critical.semantic_inference_precompute.count
            || structure.precompute_alias_eval_attempts != structure.precompute_alias_filter_accepts
            || structure
                .precompute_alias_filter_accepts
                .saturating_add(structure.precompute_alias_filter_skips)
                > structure.precompute_alias_allocations_examined
            || structure.precompute_alias_type_successes > structure.precompute_alias_eval_attempts
            || structure.precompute_inline_scan_pops
                != structure
                    .precompute_inline_scan_child_edges
                    .saturating_add(structure.precompute_bodies)
            || structure.precompute_inline_final_candidates
                > structure.precompute_inline_raw_candidates
            || structure.precompute_inline_eval_attempts
                != structure.precompute_inline_final_candidates
            || structure.precompute_inline_type_successes
                > structure.precompute_inline_eval_attempts
        {
            return Err("compiler body structural-work attribution is inconsistent".to_string());
        }
        let expression_counts = [
            critical.semantic_expression_setup.count,
            critical.semantic_inference_precompute.count,
            critical.semantic_constraint_generation.count,
            critical.semantic_unification_resolution.count,
            critical.semantic_air_emission_validation.count,
        ];
        if expression_counts
            .iter()
            .any(|count| *count != critical.semantic_provider_expression_engine.count)
        {
            return Err(
                "compiler omitted or split semantic-expression breakdown evidence".to_string(),
            );
        }
        let breakdown_counts = [
            critical.cfg_input_preparation_bodies.count,
            critical.semantic_materialization_bodies.count,
            critical.cfg_domain_prerequisite_bodies.count,
            critical.cfg_domain_projection_bodies.count,
            critical.cfg_prerequisite_collection_bodies.count,
            critical.cfg_prerequisite_query_bodies.count,
            critical.cfg_builder_bodies.count,
            critical.cfg_publication_bodies.count,
        ];
        if critical.cfg_construction_bodies.count == 0
            || breakdown_counts
                .iter()
                .any(|count| *count != critical.cfg_construction_bodies.count)
        {
            return Err(
                "compiler omitted or split CFG-construction breakdown evidence".to_string(),
            );
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn distribution() -> DurationDistribution {
        let mut log2_buckets = vec![0; 64];
        log2_buckets[6] = 1;
        DurationDistribution {
            count: 1,
            total_ns: 100,
            max_ns: 100,
            log2_buckets,
        }
    }

    fn distribution_total(total_ns: u64) -> DurationDistribution {
        DurationDistribution {
            total_ns,
            max_ns: total_ns,
            ..distribution()
        }
    }

    fn add_success(distribution: &mut DurationDistribution) {
        distribution.count += 1;
        distribution.log2_buckets[6] += 1;
    }

    fn evidence() -> BuildBoundaryEvidence {
        let output_sha256 = "a".repeat(64);
        BuildBoundaryEvidence {
            runner: RunnerBoundaryEvidence {
                compiler_binary_sha256: "b".repeat(64),
                fresh_state_directory: true,
                fresh_output_directory: true,
                daemon_endpoint_supplied: false,
                retained_session_handle_supplied: false,
                clock_boundary:
                    RunnerClockBoundary::MonotonicPreSpawnThroughExitAndOutputVerification,
                successful_exit: true,
                native_output_verified: true,
                output_sha256: output_sha256.clone(),
                output_size_bytes: 4096,
            },
            compiler: CompilerBoundaryEvidence {
                boundary: BuildBoundary::FreshSourceToNativeV1,
                pipeline: CompilerPipeline::CanonicalRootedQueryGraphV1,
                session_count: 1,
                root_request_count: 1,
                configuration: CompilerConfigurationEvidence {
                    target: "x86-64-linux".to_string(),
                    compiler_build_profile: CompilerBuildProfile::ReleaseThinLto,
                    optimization: OptimizationLevel::O3,
                    linker: LinkPolicy::Internal,
                    output_kind: OutputKind::NativeExecutable,
                    requested_workers: WorkerSetting::One,
                    resolved_workers: 1,
                    preview_features: Vec::new(),
                    external_link_archives: 0,
                },
                completed_stages: CompilerStage::FRESH_SOURCE_TO_NATIVE_V1.to_vec(),
                accepted_inputs: vec![CompilerInputEvidence {
                    class: CompilerInputClass::WorkloadSource,
                    logical_identity: "main.rue".to_string(),
                    sha256: "c".repeat(64),
                }],
                embedded_assets: vec![EmbeddedAssetEvidence {
                    class: EmbeddedAssetClass::BundledRuntimeArchive,
                    logical_identity: "rue-runtime".to_string(),
                    target: "x86-64-linux".to_string(),
                }],
                artifact_hits: ArtifactHitEvidence::default(),
                emitted_output_sha256: output_sha256,
                emitted_output_size_bytes: 4096,
            },
            critical_path: CompilerCriticalPathEvidence {
                query_worker_active_ns: 1,
                ready_items: 1,
                ready_wait_ns: 1,
                max_ready_wait_ns: 1,
                longest_query_dependency_chain: 1,
                peak_query_workers: 1,
                toolchain_acquisition_ns: 1,
                semantic_bodies: distribution(),
                semantic_prerequisite_bodies: distribution(),
                semantic_declaration_graph: distribution(),
                semantic_declaration_occurrence_indexes: distribution(),
                semantic_declaration_nuclei: distribution(),
                // Zero for a current producer: RUE-1510 deleted the span.
                semantic_declaration_signature_parsing: DurationDistribution::default(),
                semantic_declaration_signature_projection: distribution(),
                semantic_body_closure: distribution(),
                semantic_body_graph_projection: distribution(),
                semantic_body_input_lowering: distribution(),
                semantic_body_input_attributed_total: distribution_total(500),
                semantic_body_input_assembly_snapshot: distribution(),
                semantic_body_input_lex_parse: distribution(),
                semantic_body_input_rir_lower: distribution(),
                semantic_body_input_span_remap_validation: distribution(),
                semantic_body_input_rir_index: distribution(),
                semantic_provider_analysis: distribution(),
                semantic_provider_host_setup: distribution(),
                semantic_provider_expression_engine: distribution(),
                semantic_expression_setup: distribution(),
                semantic_inference_precompute: distribution_total(200),
                semantic_inference_precompute_structural: distribution(),
                semantic_inference_precompute_eval_provider: distribution(),
                semantic_constraint_generation: distribution(),
                semantic_unification_resolution: distribution(),
                semantic_air_emission_validation: distribution(),
                semantic_provider_specialization_selection: distribution(),
                semantic_provider_body_export: distribution(),
                semantic_provider_result_projection: distribution(),
                cfg_input_preparation_bodies: distribution(),
                semantic_materialization_bodies: distribution(),
                cfg_domain_prerequisite_bodies: distribution(),
                cfg_domain_projection_bodies: distribution(),
                cfg_prerequisite_collection_bodies: distribution(),
                cfg_prerequisite_query_bodies: distribution(),
                cfg_builder_bodies: distribution(),
                cfg_publication_bodies: distribution(),
                cfg_construction_bodies: distribution(),
                cfg_optimization_bodies: distribution(),
                joins: 0,
                declined_joins: 0,
                donated_permits: 0,
            },
            compiler_work: CompilerWork {
                semantic_body_structure: crate::SemanticBodyStructureWork {
                    body_lowerings: 1,
                    index_builds: 1,
                    precompute_bodies: 1,
                    precompute_inline_scan_pops: 1,
                    ..crate::SemanticBodyStructureWork::default()
                },
                ..CompilerWork::default()
            },
        }
    }

    #[test]
    fn reference_policy_has_one_exhaustive_canonical_spelling() {
        let policy = BuildBoundaryPolicy::fresh_source_to_native_v1(WorkerSetting::One);
        assert_eq!(
            policy.canonical_compiler_args(),
            ["-O3".to_string(), "-j1".to_string()]
        );
        policy.validate().unwrap();

        let mut widened = policy;
        widened
            .allowed_input_classes
            .push(CompilerInputClass::WorkloadSource);
        assert!(widened.validate().is_err());
        assert_eq!(WorkerSetting::REFERENCE_MATRIX.len(), 5);
    }

    #[test]
    fn historical_critical_path_evidence_defaults_cfg_breakdown() {
        let mut encoded = serde_json::to_value(evidence()).unwrap();
        let critical = encoded["critical_path"].as_object_mut().unwrap();
        for field in [
            "semantic_prerequisite_bodies",
            "semantic_declaration_graph",
            "semantic_declaration_occurrence_indexes",
            "semantic_declaration_nuclei",
            "semantic_declaration_signature_parsing",
            "semantic_declaration_signature_projection",
            "semantic_body_closure",
            "semantic_body_graph_projection",
            "semantic_body_input_lowering",
            "semantic_body_input_attributed_total",
            "semantic_body_input_assembly_snapshot",
            "semantic_body_input_lex_parse",
            "semantic_body_input_rir_lower",
            "semantic_body_input_span_remap_validation",
            "semantic_body_input_rir_index",
            "semantic_provider_analysis",
            "semantic_provider_host_setup",
            "semantic_provider_expression_engine",
            "semantic_expression_setup",
            "semantic_inference_precompute",
            "semantic_inference_precompute_structural",
            "semantic_inference_precompute_eval_provider",
            "semantic_constraint_generation",
            "semantic_unification_resolution",
            "semantic_air_emission_validation",
            "semantic_provider_specialization_selection",
            "semantic_provider_body_export",
            "semantic_provider_result_projection",
            "cfg_input_preparation_bodies",
            "semantic_materialization_bodies",
            "cfg_domain_prerequisite_bodies",
            "cfg_domain_projection_bodies",
            "cfg_prerequisite_collection_bodies",
            "cfg_prerequisite_query_bodies",
            "cfg_builder_bodies",
            "cfg_publication_bodies",
        ] {
            critical.remove(field);
        }

        let decoded: BuildBoundaryEvidence = serde_json::from_value(encoded).unwrap();
        assert_eq!(
            decoded.critical_path.semantic_prerequisite_bodies,
            DurationDistribution::default()
        );
        assert_eq!(
            decoded.critical_path.semantic_declaration_graph,
            DurationDistribution::default()
        );
        assert_eq!(
            decoded
                .critical_path
                .semantic_declaration_occurrence_indexes,
            DurationDistribution::default()
        );
        assert_eq!(
            decoded.critical_path.semantic_declaration_nuclei,
            DurationDistribution::default()
        );
        assert_eq!(
            decoded.critical_path.semantic_declaration_signature_parsing,
            DurationDistribution::default()
        );
        assert_eq!(
            decoded.critical_path.semantic_body_closure,
            DurationDistribution::default()
        );
        assert_eq!(
            decoded.critical_path.semantic_body_graph_projection,
            DurationDistribution::default()
        );
        assert_eq!(
            decoded.critical_path.semantic_body_input_lowering,
            DurationDistribution::default()
        );
        assert_eq!(
            decoded.critical_path.semantic_provider_analysis,
            DurationDistribution::default()
        );
        assert_eq!(
            decoded.critical_path.semantic_provider_host_setup,
            DurationDistribution::default()
        );
        assert_eq!(
            decoded.critical_path.semantic_provider_expression_engine,
            DurationDistribution::default()
        );
        assert_eq!(
            decoded.critical_path.semantic_expression_setup,
            DurationDistribution::default()
        );
        assert_eq!(
            decoded.critical_path.semantic_inference_precompute,
            DurationDistribution::default()
        );
        assert_eq!(
            decoded.critical_path.semantic_constraint_generation,
            DurationDistribution::default()
        );
        assert_eq!(
            decoded.critical_path.semantic_unification_resolution,
            DurationDistribution::default()
        );
        assert_eq!(
            decoded.critical_path.semantic_air_emission_validation,
            DurationDistribution::default()
        );
        assert_eq!(
            decoded
                .critical_path
                .semantic_provider_specialization_selection,
            DurationDistribution::default()
        );
        assert_eq!(
            decoded.critical_path.semantic_provider_body_export,
            DurationDistribution::default()
        );
        assert_eq!(
            decoded.critical_path.semantic_provider_result_projection,
            DurationDistribution::default()
        );
        assert_eq!(
            decoded.critical_path.cfg_input_preparation_bodies,
            DurationDistribution::default()
        );
        assert_eq!(
            decoded.critical_path.semantic_materialization_bodies,
            DurationDistribution::default()
        );
        assert_eq!(
            decoded.critical_path.cfg_domain_prerequisite_bodies,
            DurationDistribution::default()
        );
        assert_eq!(
            decoded.critical_path.cfg_domain_projection_bodies,
            DurationDistribution::default()
        );
        assert_eq!(
            decoded.critical_path.cfg_prerequisite_collection_bodies,
            DurationDistribution::default()
        );
        assert_eq!(
            decoded.critical_path.cfg_prerequisite_query_bodies,
            DurationDistribution::default()
        );
        assert_eq!(
            decoded.critical_path.cfg_builder_bodies,
            DurationDistribution::default()
        );
        assert_eq!(
            decoded.critical_path.cfg_publication_bodies,
            DurationDistribution::default()
        );
        decoded
            .validate_against(
                &BuildBoundaryPolicy::fresh_source_to_native_v1(WorkerSetting::One),
                "x86-64-linux",
            )
            .unwrap();
        assert!(
            decoded
                .validate_current_producer_against(
                    &BuildBoundaryPolicy::fresh_source_to_native_v1(WorkerSetting::One),
                    "x86-64-linux",
                )
                .is_err()
        );
    }

    #[test]
    fn complete_runner_compiler_and_manifest_evidence_agrees() {
        evidence()
            .validate_against(
                &BuildBoundaryPolicy::fresh_source_to_native_v1(WorkerSetting::One),
                "x86-64-linux",
            )
            .unwrap();
    }

    #[test]
    fn current_producer_separates_attempts_from_successful_attribution() {
        let policy = BuildBoundaryPolicy::fresh_source_to_native_v1(WorkerSetting::One);
        let mut recovered = evidence();
        add_success(&mut recovered.critical_path.semantic_body_input_lowering);
        add_success(&mut recovered.critical_path.semantic_provider_analysis);
        recovered
            .validate_current_producer_against(&policy, "x86-64-linux")
            .unwrap();

        let mut too_many_body_successes = evidence();
        for distribution in [
            &mut too_many_body_successes
                .critical_path
                .semantic_body_input_attributed_total,
            &mut too_many_body_successes
                .critical_path
                .semantic_body_input_assembly_snapshot,
            &mut too_many_body_successes
                .critical_path
                .semantic_body_input_lex_parse,
            &mut too_many_body_successes
                .critical_path
                .semantic_body_input_rir_lower,
            &mut too_many_body_successes
                .critical_path
                .semantic_body_input_span_remap_validation,
            &mut too_many_body_successes
                .critical_path
                .semantic_body_input_rir_index,
        ] {
            add_success(distribution);
        }
        too_many_body_successes
            .compiler_work
            .semantic_body_structure
            .body_lowerings = 2;
        too_many_body_successes
            .compiler_work
            .semantic_body_structure
            .index_builds = 2;
        assert_eq!(
            too_many_body_successes
                .validate_current_producer_against(&policy, "x86-64-linux")
                .unwrap_err(),
            "compiler body-lowering attribution is incomplete"
        );

        let mut too_many_provider_successes = evidence();
        for distribution in [
            &mut too_many_provider_successes
                .critical_path
                .semantic_provider_host_setup,
            &mut too_many_provider_successes
                .critical_path
                .semantic_provider_expression_engine,
            &mut too_many_provider_successes
                .critical_path
                .semantic_provider_specialization_selection,
            &mut too_many_provider_successes
                .critical_path
                .semantic_provider_body_export,
            &mut too_many_provider_successes
                .critical_path
                .semantic_provider_result_projection,
        ] {
            add_success(distribution);
        }
        assert_eq!(
            too_many_provider_successes
                .validate_current_producer_against(&policy, "x86-64-linux")
                .unwrap_err(),
            "compiler provider-analysis attribution is incomplete"
        );
    }

    #[test]
    fn omitted_semantic_evidence_names_the_missing_counters() {
        // The rejection a maintainer reads out of CI has to point at the field.
        // RUE-1514 was one message covering 27 conditions: every published run
        // for a day said "compiler omitted semantic critical-path evidence" and
        // nothing more, and finding which counter had gone to zero meant
        // reading this predicate and bisecting the data branch.
        let policy = BuildBoundaryPolicy::fresh_source_to_native_v1(WorkerSetting::One);

        let mut one_missing = evidence();
        one_missing.critical_path.semantic_declaration_nuclei = DurationDistribution::default();
        assert_eq!(
            one_missing
                .validate_current_producer_against(&policy, "x86-64-linux")
                .unwrap_err(),
            "compiler omitted semantic critical-path evidence: semantic_declaration_nuclei"
        );

        // Every absent counter is named, not just the first: a producer that
        // stopped reporting a whole phase should say so in one collection.
        let mut several_missing = evidence();
        several_missing.critical_path.semantic_declaration_graph = DurationDistribution::default();
        several_missing
            .critical_path
            .semantic_unification_resolution = DurationDistribution::default();
        assert_eq!(
            several_missing
                .validate_current_producer_against(&policy, "x86-64-linux")
                .unwrap_err(),
            "compiler omitted semantic critical-path evidence: semantic_declaration_graph, \
             semantic_unification_resolution"
        );

        assert_eq!(REQUIRED_SEMANTIC_EVIDENCE.len(), 27);
    }

    #[test]
    fn a_vanished_signature_projection_fails_closed() {
        // The positive half of the pair below. RUE-1514 proved only that
        // signature *parsing* is gone; a producer that removed or
        // short-circuited signature *projection* satisfied that and published
        // nothing at all for the path that replaced parsing. Under the old
        // predicate the analogous regression was caught, which is how
        // RUE-1510's change surfaced in the first place.
        let policy = BuildBoundaryPolicy::fresh_source_to_native_v1(WorkerSetting::One);
        let mut vanished = evidence();
        vanished
            .critical_path
            .semantic_declaration_signature_projection = DurationDistribution::default();
        assert_eq!(
            vanished
                .validate_current_producer_against(&policy, "x86-64-linux")
                .unwrap_err(),
            "compiler omitted semantic critical-path evidence: \
             semantic_declaration_signature_projection"
        );
    }

    #[test]
    fn a_reappearing_signature_parse_fails_closed() {
        // The inverse rule, and the reason the field is not simply dropped from
        // the required set. RUE-1510 deleted signature source reconstruction
        // and parser re-entry, so the current producer reports zero here — the
        // fixture above. A process that reports one is doing frontend work this
        // boundary variant does not describe, which is a reviewed schema event
        // rather than a sample to admit quietly.
        let policy = BuildBoundaryPolicy::fresh_source_to_native_v1(WorkerSetting::One);
        evidence()
            .validate_current_producer_against(&policy, "x86-64-linux")
            .unwrap();

        let mut reparsed = evidence();
        reparsed
            .critical_path
            .semantic_declaration_signature_parsing = distribution();
        let error = reparsed
            .validate_current_producer_against(&policy, "x86-64-linux")
            .unwrap_err();
        assert!(
            error.starts_with("semantic_declaration_signature_parsing reported 1 invocations"),
            "{error}"
        );

        // Immutable historical records still carry real values here and must
        // keep validating; only the current producer is held to the deletion.
        reparsed.validate_against(&policy, "x86-64-linux").unwrap();
    }

    #[test]
    fn mismatched_output_artifacts_and_hidden_cache_hits_fail_closed() {
        let policy = BuildBoundaryPolicy::fresh_source_to_native_v1(WorkerSetting::One);
        let mut wrong_size = evidence();
        wrong_size.runner.output_size_bytes += 1;
        assert!(
            wrong_size
                .validate_against(&policy, "x86-64-linux")
                .is_err()
        );

        let mut cache_hit = evidence();
        cache_hit.compiler.artifact_hits.persistent_cache_hits = 1;
        assert!(cache_hit.validate_against(&policy, "x86-64-linux").is_err());

        let mut missing_histogram = evidence();
        missing_histogram.critical_path.semantic_bodies = DurationDistribution::default();
        assert!(
            missing_histogram
                .validate_against(&policy, "x86-64-linux")
                .is_err()
        );
    }

    #[test]
    fn every_boundary_digest_answers_to_the_one_digest_spelling() {
        // Uppercase hexadecimal is a well-formed SHA-256; it is a *second*
        // spelling of one, and two spellings give one measured artifact two
        // content addresses. Boundary evidence is what a sample's admissibility
        // is decided on, so the boundary must not be the one place a second
        // spelling can enter a store that cannot delete it.
        let policy = BuildBoundaryPolicy::fresh_source_to_native_v1(WorkerSetting::One);

        let mut binary = evidence();
        binary.runner.compiler_binary_sha256 =
            binary.runner.compiler_binary_sha256.to_ascii_uppercase();
        assert_eq!(
            binary
                .validate_against(&policy, "x86-64-linux")
                .unwrap_err(),
            "runner reported an invalid compiler binary SHA-256"
        );

        // Runner and compiler still name the same output as each other here, so
        // the spelling is the only thing left to reject.
        let mut output = evidence();
        output.runner.output_sha256 = output.runner.output_sha256.to_ascii_uppercase();
        output.compiler.emitted_output_sha256 =
            output.compiler.emitted_output_sha256.to_ascii_uppercase();
        assert_eq!(
            output
                .validate_against(&policy, "x86-64-linux")
                .unwrap_err(),
            "runner reported an invalid output SHA-256"
        );

        // This case pins the mismatch branch rather than the spelling one. The
        // two share an error, and the runner's own digest is untouched and
        // still lowercase, so a respelled compiler digest is caught as a
        // disagreement whichever predicate is in force. It is here because the
        // field is one of the four, not because it can tell the rules apart.
        let mut emitted = evidence();
        emitted.compiler.emitted_output_sha256 =
            emitted.compiler.emitted_output_sha256.to_ascii_uppercase();
        assert_eq!(
            emitted
                .validate_against(&policy, "x86-64-linux")
                .unwrap_err(),
            "compiler and runner output evidence disagrees"
        );

        let mut input = evidence();
        input.compiler.accepted_inputs[0].sha256 = input.compiler.accepted_inputs[0]
            .sha256
            .to_ascii_uppercase();
        assert_eq!(
            input.validate_against(&policy, "x86-64-linux").unwrap_err(),
            "compiler reported malformed input provenance"
        );

        // The lowercase spelling the compiler and runner actually emit is the
        // one that passes; the fixture is otherwise unchanged.
        evidence()
            .validate_against(&policy, "x86-64-linux")
            .unwrap();
    }

    #[test]
    fn unknown_boundary_variants_are_not_deserialized() {
        assert!(serde_json::from_str::<BuildBoundary>("\"future_cached_boundary\"").is_err());
    }
}
