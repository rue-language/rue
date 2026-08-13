//! Machine-checkable source-to-native build-boundary evidence (ADR-0071).
//!
//! The manifest, external runner, and compiler each describe the same
//! observation independently. A reference sample is admissible only when the
//! three descriptions agree exactly; new product modes require new exhaustive
//! enum variants rather than quietly widening this boundary.

use serde::{Deserialize, Serialize};

use crate::CompilerWork;

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
        if !is_sha256(&runner.compiler_binary_sha256) {
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
        if !is_sha256(&runner.output_sha256) {
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
        if !is_sha256(&compiler.emitted_output_sha256)
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
            if input.logical_identity.is_empty() || !is_sha256(&input.sha256) {
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
        if critical.semantic_prerequisite_bodies.count == 0 {
            return Err("compiler omitted semantic-prerequisite evidence".to_string());
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

fn is_sha256(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
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
            compiler_work: CompilerWork::default(),
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
    fn unknown_boundary_variants_are_not_deserialized() {
        assert!(serde_json::from_str::<BuildBoundary>("\"future_cached_boundary\"").is_err());
    }
}
