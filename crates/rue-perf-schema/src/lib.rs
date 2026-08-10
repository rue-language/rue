//! The compiler-performance measurement contract (ADR-0067).
//!
//! This crate owns the vocabulary that every other part of the performance
//! system speaks: what a workload is, what pins a comparison, what a raw
//! observation looks like on disk, and which observations may enter a series.
//!
//! Four ideas carry the design.
//!
//! **Wall-clock phase accounting is additive by construction.** A sample's
//! [`PhaseAccounting`] partitions compiler-root wall time into mutually
//! exclusive bands whose integer nanoseconds sum *exactly* to
//! [`PhaseAccounting::compiler_root_ns`]. The equality is checked, not assumed
//! ([`PhaseAccounting::attributed_ns`]). Inclusive tracing spans are a separate
//! machinery and never appear here.
//!
//! **Versioning has two layers.** A [`SuiteRevision`] pins the
//! platform-independent logical contract — which workloads exist, the published
//! phase taxonomy, the runner protocol. A [`PlatformEpoch`] pins everything that
//! can vary per platform — invocation, resolved content hashes, sampling policy,
//! environment policy, baseline. Together they make a configuration-invalid
//! observation *unappendable* rather than merely detectable after the fact.
//!
//! **Storage is raw only.** A [`RunObject`] holds complete raw samples in
//! integer nanoseconds plus structured evidence of whatever failed. Medians,
//! dispersion, indexes, and chart data are derived by consumers and never
//! stored. [`content_address`] gives a run object its immutable name.
//!
//! **Failure is evidence.** A run is stored whatever happened. Appendability
//! errors ([`ValidationError`]) reject a run outright; per-sample invalidity
//! ([`InvalidSample`]) excludes a sample from publication while keeping it on
//! disk. [`ValidationOutcome`] reports both, plus the run's
//! [`Completeness`].

mod canonical;
mod incremental;
mod manifest;
mod run;
mod scaling;
mod series;
mod stats;
mod validate;

pub use canonical::{CanonicalError, canonical_json, content_address};
pub use incremental::{
    EDIT_REPORT_SCHEMA_VERSION, EditEndpoints, EditManifest, EditManifestError, EditOutcome,
    EditReport, EditReportIdentity, EditReportRegime, EditRow, EditRowSummary, EditSample,
    EditScenario, EditScenarioDeclaration, EditSummary, EditValidation, EditWorkload,
    EndpointSummary, ExpectedEditOutcome, FailureStage, HostClass, LinkBandSummary,
    OptimizationSetting, OracleComparison, OutcomeIdentity, OutcomeKind, PhaseWork, ReferenceHost,
    RetainedGauges, RetentionSequence, RetentionStep, RetentionStepOutcome, RotationRule,
    SourceShape, StructuralWork, StructuralWorkSummary, TransformationIdentity, ValidationFinding,
    ValidationWork, WorkerDeclaration, WorkerMode, derive_edit_report, render_edit_report_markdown,
    validate_edit_report,
};
pub use manifest::{
    Baseline, EnvironmentPolicy, FlaggingPolicy, Manifest, ManifestError, PlatformEpoch,
    SamplingPolicy, SuiteRevision, Workload,
};
pub use run::{
    Band, EnvironmentFingerprint, FailureRecord, Invocation, Phase, PhaseAccounting, ResolvedPins,
    RunIdentity, RunObject, Sample, WorkloadObservation,
};
pub use scaling::{
    CompilerWork, QueryRuntimeWork, SCALING_REPORT_SCHEMA_VERSION, ScalingIdentity,
    ScalingManifest, ScalingObservation, ScalingRegime, ScalingReport, ScalingWorkload,
    WorkloadShape,
};
pub use series::{Metric, SeriesId};
pub use stats::{
    Summary, flags_movement, geometric_mean, median, median_absolute_deviation, pooled_uncertainty,
    ratio, sample_value,
};
pub use validate::{
    Completeness, InvalidSample, InvalidSampleReason, ValidationError, ValidationOutcome,
    validate_run,
};

/// The schema version of [`RunObject`] as defined by this crate.
///
/// A run object records the version it was written under. Readers refuse
/// versions they do not implement rather than guessing: there is no
/// compatibility path, by design.
pub const RUN_SCHEMA_VERSION: u32 = 1;
