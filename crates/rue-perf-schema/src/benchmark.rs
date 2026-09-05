//! The `--benchmark-json` envelope: what the compiler publishes about one
//! compilation, and what the runner reads back.
//!
//! This is the outermost object of the measurement contract. The sections
//! inside it are owned elsewhere in this crate — [`PhaseAccounting`] partitions
//! compiler-root wall time, [`CompilerWork`] counts host-independent work,
//! [`CompilerBoundaryEvidence`] and [`CompilerCriticalPathEvidence`] carry the
//! protocol-v2 evidence — and the envelope's own job is to say which of them
//! are present and under which schema version.
//!
//! One declaration, two directions. The compiler builds a [`BenchmarkReport`]
//! and serializes it; the runner deserializes the same type. Renaming a field
//! therefore renames it for both, which is the property the separate producer
//! and consumer declarations this replaced could not offer.
//!
//! The wire form is key-sorted JSON ([`sorted_json`]), so the bytes depend on
//! the value rather than on a producer's field declaration order and two
//! reports can be diffed line for line.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::boundary::{CompilerBoundaryEvidence, CompilerCriticalPathEvidence};
use crate::canonical::{CanonicalError, sorted_json};
use crate::run::PhaseAccounting;
use crate::scaling::{CompilerWork, WorkloadShape};

/// Version of the machine-readable timing contract the compiler publishes.
///
/// Producer and consumer read this one constant, so a schema change is a
/// single edit and a runner cannot silently accept a report it was not built
/// for. Bumping it is a consumer-visible break: every reader of
/// `--benchmark-json` refuses the previous shape from that moment on.
pub const BENCHMARK_JSON_SCHEMA_VERSION: u32 = 19;

/// The pass table's timing model: inclusive spans that nest and overlap.
pub const BENCHMARK_TIMING_MODEL: &str = "inclusive_spans";

/// The interval whole-compiler allocation counting covers.
pub const COMPILER_ALLOCATION_BOUNDARY: &str =
    "canonical compile root including discovery and backend";

/// One compilation, as the compiler reports it on stdout.
///
/// Every section a driver path may not have is an `Option` that is omitted
/// from the wire rather than written as null, so a report from a path that
/// measured less is still a valid report rather than a parse failure.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchmarkReport {
    /// Always [`BENCHMARK_JSON_SCHEMA_VERSION`]; a reader refuses anything else.
    #[serde(deserialize_with = "deserialize_schema_version")]
    pub schema_version: u32,
    /// Always [`BENCHMARK_TIMING_MODEL`]: pass durations are inclusive and may
    /// overlap their parents.
    pub timing_model: String,
    /// The additive wall-clock partition of compiler-root time.
    ///
    /// This is the only model that may be stacked. Its integer nanoseconds
    /// satisfy `sum(phase_ns) + mixed_parallel_ns + unattributed_ns ==
    /// compiler_root_ns` exactly. The `passes` table is the *other* model —
    /// inclusive spans that nest and overlap — and the two must never be mixed
    /// in one visualization.
    pub phase_accounting: PhaseAccounting,
    /// Who produced this report, where, and when.
    pub metadata: BenchmarkMetadata,
    /// Individual pass timings in milliseconds.
    pub passes: Vec<BenchmarkPass>,
    /// Driver-side phases measured outside the compiler's timing root.
    ///
    /// These break down `process - total_ms`, never `total_ms` itself, so they
    /// must not be added to the pass table. The field is omitted entirely when
    /// the run measured no driver phase.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub driver_phases: Vec<BenchmarkDriverPhase>,
    /// Total compilation time in milliseconds.
    pub total_ms: f64,
    /// Source and program-shape metrics for throughput calculations.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_metrics: Option<WorkloadShape>,
    /// Deterministic compiler work independent of host timing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compiler_work: Option<CompilerWork>,
    /// Peak memory usage in bytes, where the platform reports it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peak_memory_bytes: Option<u64>,
    /// The artifact this compilation published, identified by digest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub emitted_output: Option<EmittedOutput>,
    /// Protocol-v2 evidence that this was a fresh source-to-native build.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compiler_boundary: Option<CompilerBoundaryEvidence>,
    /// Protocol-v2 evidence about what the schedule was waiting on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub critical_path: Option<CompilerCriticalPathEvidence>,
    /// Whole-compiler allocation accounting, when the binary was built with it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compiler_allocations: Option<CompilerAllocations>,
}

impl BenchmarkReport {
    /// Render the report as the bytes `--benchmark-json` writes to stdout.
    ///
    /// Keys are sorted at every depth, so the shape is a function of the value
    /// and not of the order this struct happens to declare its fields in.
    pub fn to_wire_json(&self) -> Result<String, CanonicalError> {
        sorted_json(self)
    }
}

/// Who produced a report, where, and when.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchmarkMetadata {
    /// UTC timestamp of when the benchmark was run, from
    /// [`utc_timestamp`](crate::utc_timestamp).
    pub timestamp: String,
    /// Compiler version.
    pub version: String,
    /// Target platform (for example `x86_64-linux`, `aarch64-macos`).
    pub target: String,
    /// Build profile of the Rust compiler binary being measured.
    pub compiler_build_profile: String,
}

/// Timing for a single compiler pass, from the inclusive-span model.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchmarkPass {
    /// Name of the pass (for example `lexer`, `parser`).
    pub name: String,
    /// Time spent in this pass in milliseconds.
    pub duration_ms: f64,
    /// Percentage of total compilation time.
    pub percent: f64,
    /// Number of spans aggregated into this row.
    pub invocations: u64,
    /// Number of invocations without a parent span.
    pub root_invocations: u64,
    /// Number of invocations without a child span.
    pub leaf_invocations: u64,
}

/// Timing for one driver-side phase outside the compiler's timing root.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchmarkDriverPhase {
    /// Name of the driver phase (for example `output_write`).
    pub name: String,
    /// Time spent in this phase in milliseconds.
    pub duration_ms: f64,
    /// Number of spans aggregated into this row.
    pub invocations: u64,
}

/// The artifact a compilation published, identified by digest.
///
/// The runner hashes the file it finds on disk and compares; a mismatch means
/// the compiler and the runner are not talking about the same bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmittedOutput {
    /// Lowercase hexadecimal SHA-256 of the published bytes.
    pub sha256: String,
    /// Size of the published bytes.
    pub size_bytes: u64,
}

impl EmittedOutput {
    /// Identify published bytes, digesting them once for every consumer that
    /// needs the digest.
    pub fn of(bytes: &[u8]) -> Self {
        Self {
            sha256: format!("{:x}", Sha256::digest(bytes)),
            size_bytes: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
        }
    }
}

/// Whole-compiler allocation accounting over the compile root.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompilerAllocations {
    /// Allocations performed inside the boundary.
    pub count: u64,
    /// Bytes requested by those allocations.
    pub requested_bytes: u64,
    /// Prose statement of what the counters cover.
    pub boundary: String,
}

impl CompilerAllocations {
    /// Record counts taken over [`COMPILER_ALLOCATION_BOUNDARY`].
    pub fn over_compile_root(count: u64, requested_bytes: u64) -> Self {
        Self {
            count,
            requested_bytes,
            boundary: COMPILER_ALLOCATION_BOUNDARY.to_string(),
        }
    }
}

fn deserialize_schema_version<'de, D>(deserializer: D) -> Result<u32, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::Error;

    let version = <u32 as Deserialize>::deserialize(deserializer)?;
    if version != BENCHMARK_JSON_SCHEMA_VERSION {
        return Err(D::Error::custom(format!(
            "unsupported benchmark JSON schema version {version}; expected {BENCHMARK_JSON_SCHEMA_VERSION}"
        )));
    }
    Ok(version)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    fn report() -> BenchmarkReport {
        BenchmarkReport {
            schema_version: BENCHMARK_JSON_SCHEMA_VERSION,
            timing_model: BENCHMARK_TIMING_MODEL.to_string(),
            phase_accounting: PhaseAccounting {
                phase_ns: BTreeMap::new(),
                mixed_parallel_ns: 0,
                unattributed_ns: 0,
                compiler_root_ns: 0,
            },
            metadata: BenchmarkMetadata {
                timestamp: "2026-01-02T03:04:05Z".to_string(),
                version: "0.1.0".to_string(),
                target: "x86_64-linux".to_string(),
                compiler_build_profile: "release_thin_lto".to_string(),
            },
            passes: vec![BenchmarkPass {
                name: "lexer".to_string(),
                duration_ms: 1.5,
                percent: 50.0,
                invocations: 1,
                root_invocations: 1,
                leaf_invocations: 1,
            }],
            driver_phases: Vec::new(),
            total_ms: 3.0,
            source_metrics: Some(WorkloadShape {
                files: 1,
                modules: 1,
                bytes: 24,
                lines: 1,
                tokens: 9,
                functions: 1,
            }),
            compiler_work: Some(CompilerWork::default()),
            peak_memory_bytes: Some(4096),
            emitted_output: Some(EmittedOutput::of(b"artifact")),
            compiler_boundary: None,
            critical_path: None,
            compiler_allocations: None,
        }
    }

    #[test]
    fn the_wire_form_round_trips_through_the_consumer() {
        // Producer and consumer are the same declaration, so this fails at
        // compile time on a rename and at test time on a serde attribute that
        // stops agreeing with itself.
        let published = report().to_wire_json().expect("a report renders");
        let parsed: BenchmarkReport =
            serde_json::from_str(&published).expect("the runner parses what the compiler writes");
        assert_eq!(parsed.schema_version, BENCHMARK_JSON_SCHEMA_VERSION);
        assert_eq!(parsed.timing_model, BENCHMARK_TIMING_MODEL);
        assert_eq!(parsed.emitted_output, report().emitted_output);
        assert_eq!(
            parsed
                .source_metrics
                .as_ref()
                .expect("source metrics survive")
                .bytes,
            24
        );
        assert_eq!(
            parsed.to_wire_json().expect("a parsed report renders"),
            published
        );
    }

    #[test]
    fn the_wire_form_sorts_keys_at_every_depth() {
        let published = report().to_wire_json().expect("a report renders");
        assert!(published.starts_with(r#"{"compiler_work":"#), "{published}");
        assert!(published.contains(r#""emitted_output":{"sha256":"#));
        assert!(published.contains(r#""metadata":{"compiler_build_profile":"#));
    }

    #[test]
    fn absent_sections_are_omitted_rather_than_written_as_null() {
        let published = report().to_wire_json().expect("a report renders");
        assert!(!published.contains("null"), "{published}");
        assert!(!published.contains("critical_path"), "{published}");
        assert!(!published.contains("driver_phases"), "{published}");
    }

    #[test]
    fn a_report_without_the_optional_sections_still_parses() {
        // The runner requires more than this, and says so itself. Parsing is
        // not where a path that measured less is refused.
        let mut lean = report();
        lean.source_metrics = None;
        lean.compiler_work = None;
        lean.peak_memory_bytes = None;
        lean.emitted_output = None;
        let published = lean.to_wire_json().expect("a lean report renders");
        let parsed: BenchmarkReport =
            serde_json::from_str(&published).expect("a lean report parses");
        assert!(parsed.emitted_output.is_none());
    }

    #[test]
    fn a_retired_schema_version_is_refused() {
        let error = serde_json::from_str::<BenchmarkReport>(r#"{"schema_version":16}"#)
            .expect_err("a retired schema version must be refused");
        assert!(error.to_string().contains("expected 19"), "{error}");
    }

    #[test]
    fn the_emitted_output_digest_is_the_sha256_of_the_bytes() {
        let emitted = EmittedOutput::of(b"");
        assert_eq!(
            emitted.sha256,
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(emitted.size_bytes, 0);
    }
}
