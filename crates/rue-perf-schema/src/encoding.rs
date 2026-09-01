//! The stored record encoding: one boundary witness plus per-process digests.
//!
//! ADR-0071 Amendment 1 (accepted 2026-08-23) replaces per-process copies of
//! byte-identical boundary evidence with one complete witness per workload
//! observation and a pair of SHA-256 digests per process. ADR-0067 Amendment 1
//! makes `schema_version` the axis that owns this change: encoding shape
//! dispatches on the record's `schema_version`, while what must be *proven*
//! keeps dispatching on the suite's `protocol_version`.
//!
//! The hoist is a **lossless partition**: every field of a process's
//! [`RunnerBoundaryEvidence`] and [`CompilerBoundaryEvidence`] lands in exactly
//! one of the run-level block (invariant across the whole run) or the
//! workload-level block (invariant across that workload's processes), and a
//! reader reassembles the complete pair from the two blocks
//! ([`reassemble_witness`]). The digests commit to the evidence *as measured*:
//! each process's digest is computed from that process's own original value,
//! never from the witness, so a process that disagreed with its siblings still
//! disagrees after encoding — by digest instead of by bytes.
//!
//! The digest is `SHA-256(tag || canonical_json(value))` with a fixed ASCII
//! domain tag ending in a newline. `schema_version` is deliberately outside
//! every preimage: the process evidence is a property of the process, not of
//! the record encoding that carries it. If a preimage ever changes, the tag's
//! trailing `.1` increments and the two schemes are distinguishable by
//! construction.
//!
//! `critical_path` is the one evidence member measured to vary across
//! processes. One is retained per workload observation — the first process of
//! the first evidence-bearing sample — and the record states that provenance
//! in [`WorkloadBoundary::critical_path_source`] rather than relying on the
//! convention. The same rule and the same `_source` shape carry the
//! representative `compiler_work` a parallel boundary epoch retains: at one
//! worker it is a witness every process's digest must match; at any other
//! worker setting the values are schedule-dependent by design, no per-process
//! work digest is stored (a digest of unstored bytes that are expected to
//! differ certifies nothing), and the retained value is a sample, not a
//! witness.

use serde::{Deserialize, Serialize};

use crate::boundary::{
    ArtifactHitEvidence, BuildBoundary, BuildBoundaryEvidence, CompilerBoundaryEvidence,
    CompilerConfigurationEvidence, CompilerCriticalPathEvidence, CompilerInputEvidence,
    CompilerPipeline, CompilerStage, EmbeddedAssetEvidence, RunnerBoundaryEvidence,
    RunnerClockBoundary, WorkerSetting,
};
use crate::canonical::CanonicalError;
use crate::run::RunObject;
use crate::scaling::{CompilerWork, LegacySemanticBodyStructureWork};

/// Domain tag for the per-process identity digest over `{runner, compiler}`.
pub const IDENTITY_DIGEST_TAG: &str = "rue.boundary.identity.1\n";

/// Domain tag for the v3 per-process work digest over `compiler_work`.
/// Candidate-plan fields changed the canonical work preimage, so v3 is
/// deliberately domain-separated from the historical v2 digest.
pub const WORK_DIGEST_TAG: &str = "rue.boundary.work.2\n";
/// Historical v2 domain tag, retained solely for validating v2 records.
pub const LEGACY_WORK_DIGEST_TAG: &str = "rue.boundary.work.1\n";

/// The schema version of the current full-evidence encoding.
///
/// This is also the encoding the producer builds in memory, validates in
/// full, and retains as the collection workflow's artifact; only what reaches
/// the store is written at [`crate::RUN_SCHEMA_VERSION`].
pub const FULL_EVIDENCE_SCHEMA_VERSION: u32 = 4;
/// Historical full-evidence encoding retained for validation only.
pub const LEGACY_FULL_EVIDENCE_SCHEMA_VERSION: u32 = 1;

/// Where a retained evidence member came from.
///
/// `sample_index` and `process_index` name the entry of the original
/// full-evidence record that supplied the value. Without this, a retained
/// member is a number with no stated provenance — which is how a
/// representative sample gets misread as a witness.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvidenceSource {
    pub sample_index: u32,
    pub process_index: u32,
}

/// The run-invariant half of every process's `runner` evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunnerRunInvariant {
    pub compiler_binary_sha256: String,
    pub fresh_state_directory: bool,
    pub fresh_output_directory: bool,
    pub daemon_endpoint_supplied: bool,
    pub retained_session_handle_supplied: bool,
    pub clock_boundary: RunnerClockBoundary,
    pub successful_exit: bool,
    pub native_output_verified: bool,
}

/// The run-invariant half of every process's `compiler` evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompilerRunInvariant {
    pub boundary: BuildBoundary,
    pub pipeline: CompilerPipeline,
    pub session_count: u32,
    pub root_request_count: u32,
    pub configuration: CompilerConfigurationEvidence,
    pub completed_stages: Vec<CompilerStage>,
}

/// Parts of the boundary evidence invariant across the whole run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunBoundary {
    pub runner: RunnerRunInvariant,
    pub compiler: CompilerRunInvariant,
}

/// The workload-invariant half of a process's `runner` evidence.
///
/// Output identity is four fields, not two; these are the runner's half, and
/// [`CompilerWorkloadInvariant`] carries the compiler's.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunnerWorkloadInvariant {
    pub output_sha256: String,
    pub output_size_bytes: u64,
}

/// The workload-invariant half of a process's `compiler` evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompilerWorkloadInvariant {
    pub accepted_inputs: Vec<CompilerInputEvidence>,
    pub embedded_assets: Vec<EmbeddedAssetEvidence>,
    pub artifact_hits: ArtifactHitEvidence,
    pub emitted_output_sha256: String,
    pub emitted_output_size_bytes: u64,
}

/// Parts of the boundary evidence invariant across one workload's processes,
/// plus the retained per-workload members.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkloadBoundary {
    pub runner: RunnerWorkloadInvariant,
    pub compiler: CompilerWorkloadInvariant,
    /// The retained deterministic-work value.
    ///
    /// At one worker this is a witness: every process's
    /// `boundary_work_processes` digest must equal [`work_digest`] of this
    /// value. At any other worker setting it is a representative sample
    /// carried under the same provenance convention, and no per-process work
    /// digests are stored.
    pub compiler_work: CompilerWork,
    pub compiler_work_source: EvidenceSource,
    /// The one retained per-commit critical path for this workload.
    pub critical_path: CompilerCriticalPathEvidence,
    pub critical_path_source: EvidenceSource,
}

/// The digest preimage value: the reassembled per-process pair.
///
/// Canonical JSON sorts keys, so this serializes as the two-key object
/// `{"compiler": …, "runner": …}` regardless of field order here.
#[derive(Serialize)]
struct IdentityPreimage<'a> {
    runner: &'a RunnerBoundaryEvidence,
    compiler: &'a CompilerBoundaryEvidence,
}

fn tagged_digest<T: Serialize>(tag: &str, value: &T) -> Result<String, CanonicalError> {
    use sha2::{Digest, Sha256};
    let canonical = crate::canonical::canonical_json(value)?;
    let mut hasher = Sha256::new();
    hasher.update(tag.as_bytes());
    hasher.update(canonical.as_bytes());
    Ok(hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

/// The per-process identity digest over a `{runner, compiler}` pair.
pub fn identity_digest(
    runner: &RunnerBoundaryEvidence,
    compiler: &CompilerBoundaryEvidence,
) -> Result<String, CanonicalError> {
    tagged_digest(IDENTITY_DIGEST_TAG, &IdentityPreimage { runner, compiler })
}

/// The per-process work digest over a `compiler_work` value.
pub fn work_digest(work: &CompilerWork) -> Result<String, CanonicalError> {
    tagged_digest(WORK_DIGEST_TAG, work)
}

/// Reconstruct the exact v2 digest preimage. v2 records did not contain the
/// candidate-plan or canonical-RIR groups; serializing today's `CompilerWork`
/// after defaulting those fields would validate a different value.
#[derive(Serialize)]
struct HistoricalCompilerWorkV2<'a> {
    semantic_provider: &'a crate::scaling::SemanticProviderWork,
    semantic_reachability: &'a crate::scaling::SemanticReachabilityWork,
    semantic_body_structure: &'a LegacySemanticBodyStructureWork,
    cfg_materialization: &'a crate::scaling::CfgMaterializationWork,
    cfg_prerequisites: &'a crate::scaling::CfgPrerequisiteWork,
    cfg_retained_charge: &'a crate::scaling::CfgRetainedChargeWork,
    cfg_local_epoch: &'a crate::scaling::CfgLocalEpochWork,
    query_runtime: &'a crate::scaling::QueryRuntimeWork,
    publication: &'a crate::scaling::PublicationWork,
}

pub fn work_digest_v2(work: &CompilerWork) -> Result<String, CanonicalError> {
    let semantic_body_structure = work.legacy_v2_semantic_body_structure();
    tagged_digest(
        LEGACY_WORK_DIGEST_TAG,
        &HistoricalCompilerWorkV2 {
            semantic_provider: &work.semantic_provider,
            semantic_reachability: &work.semantic_reachability,
            semantic_body_structure: &semantic_body_structure,
            cfg_materialization: &work.cfg_materialization,
            cfg_retained_charge: &work.cfg_retained_charge,
            cfg_prerequisites: &work.cfg_prerequisites,
            cfg_local_epoch: &work.cfg_local_epoch,
            query_runtime: &work.query_runtime,
            publication: &work.publication,
        },
    )
}

/// Reassemble the complete `{runner, compiler}` pair from the two blocks.
///
/// This is the digest preimage a reader recomputes. The partition being
/// complete and disjoint is what makes the digest independent of *where* a
/// field was hoisted to; [`encode_stored_v3`] asserts the round-trip against the
/// original witness so a field landing in neither block, or in both, cannot
/// pass silently.
pub fn reassemble_witness(
    run: &RunBoundary,
    workload: &WorkloadBoundary,
) -> (RunnerBoundaryEvidence, CompilerBoundaryEvidence) {
    let runner = RunnerBoundaryEvidence {
        compiler_binary_sha256: run.runner.compiler_binary_sha256.clone(),
        fresh_state_directory: run.runner.fresh_state_directory,
        fresh_output_directory: run.runner.fresh_output_directory,
        daemon_endpoint_supplied: run.runner.daemon_endpoint_supplied,
        retained_session_handle_supplied: run.runner.retained_session_handle_supplied,
        clock_boundary: run.runner.clock_boundary,
        successful_exit: run.runner.successful_exit,
        native_output_verified: run.runner.native_output_verified,
        output_sha256: workload.runner.output_sha256.clone(),
        output_size_bytes: workload.runner.output_size_bytes,
    };
    let compiler = CompilerBoundaryEvidence {
        boundary: run.compiler.boundary,
        pipeline: run.compiler.pipeline,
        session_count: run.compiler.session_count,
        root_request_count: run.compiler.root_request_count,
        configuration: run.compiler.configuration.clone(),
        completed_stages: run.compiler.completed_stages.clone(),
        accepted_inputs: workload.compiler.accepted_inputs.clone(),
        embedded_assets: workload.compiler.embedded_assets.clone(),
        artifact_hits: workload.compiler.artifact_hits,
        emitted_output_sha256: workload.compiler.emitted_output_sha256.clone(),
        emitted_output_size_bytes: workload.compiler.emitted_output_size_bytes,
    };
    (runner, compiler)
}

fn split_run_invariant(evidence: &BuildBoundaryEvidence) -> RunBoundary {
    RunBoundary {
        runner: RunnerRunInvariant {
            compiler_binary_sha256: evidence.runner.compiler_binary_sha256.clone(),
            fresh_state_directory: evidence.runner.fresh_state_directory,
            fresh_output_directory: evidence.runner.fresh_output_directory,
            daemon_endpoint_supplied: evidence.runner.daemon_endpoint_supplied,
            retained_session_handle_supplied: evidence.runner.retained_session_handle_supplied,
            clock_boundary: evidence.runner.clock_boundary,
            successful_exit: evidence.runner.successful_exit,
            native_output_verified: evidence.runner.native_output_verified,
        },
        compiler: CompilerRunInvariant {
            boundary: evidence.compiler.boundary,
            pipeline: evidence.compiler.pipeline,
            session_count: evidence.compiler.session_count,
            root_request_count: evidence.compiler.root_request_count,
            configuration: evidence.compiler.configuration.clone(),
            completed_stages: evidence.compiler.completed_stages.clone(),
        },
    }
}

fn split_workload_invariant(
    evidence: &BuildBoundaryEvidence,
    source: EvidenceSource,
) -> WorkloadBoundary {
    WorkloadBoundary {
        runner: RunnerWorkloadInvariant {
            output_sha256: evidence.runner.output_sha256.clone(),
            output_size_bytes: evidence.runner.output_size_bytes,
        },
        compiler: CompilerWorkloadInvariant {
            accepted_inputs: evidence.compiler.accepted_inputs.clone(),
            embedded_assets: evidence.compiler.embedded_assets.clone(),
            artifact_hits: evidence.compiler.artifact_hits,
            emitted_output_sha256: evidence.compiler.emitted_output_sha256.clone(),
            emitted_output_size_bytes: evidence.compiler.emitted_output_size_bytes,
        },
        compiler_work: evidence.compiler_work,
        compiler_work_source: source,
        critical_path: evidence.critical_path.clone(),
        critical_path_source: source,
    }
}

/// Why a full-evidence record could not be re-encoded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EncodeError {
    /// The input does not declare the full-evidence schema version.
    NotFullEvidence { found: u32 },
    /// Historical full evidence contains a retired taxonomy that the stored
    /// schema cannot represent without dropping fields.
    HistoricalFullEvidenceUnsupported,
    /// The reassembled witness did not equal the original pair.
    ///
    /// This is the round-trip guarantee failing: a field landed in neither
    /// block or in both, and the digests would certify something other than
    /// the evidence as measured.
    RoundTrip { workload: String },
    /// A value could not be canonicalized for digesting.
    Canonical(CanonicalError),
}

impl std::fmt::Display for EncodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EncodeError::NotFullEvidence { found } => write!(
                f,
                "expected current full-evidence schema v{FULL_EVIDENCE_SCHEMA_VERSION}, found v{found}"
            ),
            EncodeError::HistoricalFullEvidenceUnsupported => write!(
                f,
                "historical full-evidence schema v{LEGACY_FULL_EVIDENCE_SCHEMA_VERSION} cannot be losslessly re-encoded: its retired semantic_body_structure taxonomy is not representable in stored schema v{}",
                crate::RUN_SCHEMA_VERSION
            ),
            EncodeError::RoundTrip { workload } => write!(
                f,
                "workload {workload}: reassembled witness differs from the original evidence"
            ),
            EncodeError::Canonical(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for EncodeError {}

impl From<CanonicalError> for EncodeError {
    fn from(error: CanonicalError) -> Self {
        EncodeError::Canonical(error)
    }
}

/// Encode current full-evidence schema v4 into the stored (v3) form.
///
/// A record with no boundary evidence anywhere — a protocol-1 suite — changes
/// only its declared `schema_version`. Everything the encoding drops is
/// dropped knowingly: per-process `critical_path` histograms for processes
/// beyond the retained one, replaced by nothing, and per-process
/// `{runner, compiler}` / `compiler_work` values, replaced by digests a reader
/// checks against the retained witness.
///
/// Digests are computed from each process's own evidence. A record whose
/// processes disagreed — which validation refuses to append, but the store
/// keeps as evidence — therefore still disagrees after encoding: the
/// mismatching process's digest fails the witness comparison exactly where
/// the original bytes failed the equality check.
pub fn encode_stored_v3(full: &RunObject) -> Result<RunObject, EncodeError> {
    if full.schema_version == LEGACY_FULL_EVIDENCE_SCHEMA_VERSION {
        return Err(EncodeError::HistoricalFullEvidenceUnsupported);
    }
    if full.schema_version != FULL_EVIDENCE_SCHEMA_VERSION {
        return Err(EncodeError::NotFullEvidence {
            found: full.schema_version,
        });
    }

    let mut encoded = full.clone();
    encoded.schema_version = crate::RUN_SCHEMA_VERSION;
    // The commitment to what this encoding drops: the full form's own content
    // address, which is both the retained artifact's name and — for a
    // re-encoded record — the pre-compaction record's address in history.
    encoded.full_evidence = Some(crate::canonical::content_address(full)?);

    // The run-invariant block comes from the first evidence-bearing process in
    // the record. A record with none stays evidence-free.
    let first_evidence = full.workloads.iter().find_map(|observation| {
        observation
            .samples
            .iter()
            .find_map(|sample| sample.boundary_evidence.first())
    });
    let Some(first_evidence) = first_evidence else {
        return Ok(encoded);
    };
    let run_boundary = split_run_invariant(first_evidence);

    for observation in &mut encoded.workloads {
        // The witness rule: the first process of the first evidence-bearing
        // sample, with the actual indices recorded as provenance. For a
        // well-formed record that is samples[0], boundary_evidence[0].
        let witness = observation
            .samples
            .iter()
            .enumerate()
            .find_map(|(sample_index, sample)| {
                sample.boundary_evidence.first().map(|evidence| {
                    (
                        EvidenceSource {
                            sample_index: sample_index as u32,
                            process_index: 0,
                        },
                        evidence.clone(),
                    )
                })
            });
        let Some((source, witness)) = witness else {
            // No evidence in this workload: nothing to hoist, nothing to
            // digest. The reader's protocol checks decide whether that state
            // was legal, exactly as they did for the original record.
            continue;
        };

        let workload_boundary = split_workload_invariant(&witness, source);

        // The round-trip guarantee: the partition is complete and disjoint,
        // proven by reassembling the witness and requiring exact equality.
        let (runner, compiler) = reassemble_witness(&run_boundary, &workload_boundary);
        if runner != witness.runner || compiler != witness.compiler {
            return Err(EncodeError::RoundTrip {
                workload: observation.workload.clone(),
            });
        }

        // Work digests are stored only where the epoch's whole record is
        // deterministic per process: one worker. The setting is read from the
        // evidence itself — validation already requires it to equal the
        // epoch's declared policy.
        let one_worker =
            run_boundary.compiler.configuration.requested_workers == WorkerSetting::One;

        for sample in &mut observation.samples {
            let mut identity_digests = Vec::with_capacity(sample.boundary_evidence.len());
            let mut work_digests = Vec::with_capacity(sample.boundary_evidence.len());
            for evidence in &sample.boundary_evidence {
                identity_digests.push(identity_digest(&evidence.runner, &evidence.compiler)?);
                if one_worker {
                    work_digests.push(work_digest(&evidence.compiler_work)?);
                }
            }
            sample.boundary_processes = identity_digests;
            sample.boundary_work_processes = work_digests;
            sample.boundary_evidence = Vec::new();
        }

        observation.boundary = Some(workload_boundary);
    }

    encoded.boundary = Some(run_boundary);
    Ok(encoded)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A protocol-2-shaped full-evidence run: the validate fixture with a
    /// batch of evidence attached to every sample.
    fn full_run() -> RunObject {
        let mut run = crate::validate::tests::sample_run();
        for observation in &mut run.workloads {
            for sample in &mut observation.samples {
                let mut evidence = crate::boundary::tests::evidence();
                evidence.runner.output_size_bytes = sample.output_binary_bytes;
                evidence.compiler.emitted_output_size_bytes = sample.output_binary_bytes;
                sample.boundary_evidence = vec![evidence; sample.batch_size as usize];
            }
        }
        run
    }

    // The worked vector from the supporting note: the mechanism — tag,
    // canonicalization, hex — pinned to exact bytes checkable with sha256sum.
    fn vector_runner() -> RunnerBoundaryEvidence {
        RunnerBoundaryEvidence {
            compiler_binary_sha256: "a".repeat(64),
            fresh_state_directory: true,
            fresh_output_directory: true,
            daemon_endpoint_supplied: false,
            retained_session_handle_supplied: false,
            clock_boundary: RunnerClockBoundary::MonotonicPreSpawnThroughExitAndOutputVerification,
            successful_exit: true,
            native_output_verified: true,
            output_sha256: "b".repeat(64),
            output_size_bytes: 16384,
        }
    }

    #[test]
    fn the_worked_vector_reproduces_byte_for_byte() {
        let runner = vector_runner();
        let canonical = crate::canonical::canonical_json(&runner).unwrap();
        assert_eq!(
            canonical.len(),
            464,
            "the note pins the preimage at 464 bytes"
        );
        assert_eq!(
            tagged_digest(IDENTITY_DIGEST_TAG, &runner).unwrap(),
            "2a095434f674a8c7d4096f6c69d45273c1f811a8ac05bd590f82649170a8501e"
        );
        assert_eq!(
            tagged_digest("", &runner).unwrap(),
            "354de1ad26a990020fb8548f8e29a8b3e5618562fa91e9b256224beb68433675"
        );
        assert_eq!(
            tagged_digest(WORK_DIGEST_TAG, &runner).unwrap(),
            "24a54e6f381f2b1741ca57e33069fba148d5f23053fcdb2ba32a256192858e9c"
        );
    }

    #[test]
    fn v2_work_digest_uses_the_historical_preimage() {
        // This fixture intentionally has the current taxonomy absent (the
        // v2 wire shape) while retaining the historical semantic structure.
        // The fixed digest guards against accidentally defaulting and
        // reserializing the v3 `CompilerWork` shape during validation.
        let work = CompilerWork::default();
        assert_eq!(
            work_digest_v2(&work).unwrap(),
            "31e7133458a98d37218c0b5b8c18c1ae0f54095ef3df8134e6e14a2a5297ba57"
        );
    }

    #[test]
    fn nonzero_v2_preimage_has_a_pinned_work1_digest_and_differs_from_work2() {
        let mut work = CompilerWork::default();
        work.semantic_provider.name_lookups = 2;
        work.semantic_reachability.frontier_scans = 3;
        work.cfg_materialization.index_builds = 1;
        work.legacy_v2_semantic_body_structure = Some(LegacySemanticBodyStructureWork {
            body_lowerings: 2,
            source_bytes: 37,
            declaration_fragments: 2,
            rir_instructions: 11,
            rir_payload_words: 7,
            index_builds: 2,
            index_rir_instructions_visited: 11,
            index_method_references_visited: 5,
            index_shell_declarations_visited: 3,
            index_named_methods_indexed: 2,
            index_const_declarations_indexed: 1,
            precompute_bodies: 0,
            ..LegacySemanticBodyStructureWork::default()
        });
        assert_eq!(
            work_digest_v2(&work).unwrap(),
            "782c32cdf7db40c23df3bbd6ceb81bedffc8ecbfd68878fea8c43b1dec217db4"
        );
        assert_ne!(work_digest_v2(&work).unwrap(), work_digest(&work).unwrap());
    }

    #[test]
    fn the_two_digest_tags_domain_separate_one_value() {
        // Same construction, different tags, different names — the property
        // the tag buys. Checked on the vector above for exact bytes; checked
        // here for the general shape.
        let work = CompilerWork::default();
        let under_work_tag = work_digest(&work).unwrap();
        let under_historical_work_tag = work_digest_v2(&work).unwrap();
        let under_identity_tag = tagged_digest(IDENTITY_DIGEST_TAG, &work).unwrap();
        assert_ne!(under_work_tag, under_identity_tag);
        assert_ne!(under_work_tag, under_historical_work_tag);
    }

    #[test]
    fn the_named_evidence_types_serialize_every_field() {
        // Amendment rule 3: no omission or default rules on
        // RunnerBoundaryEvidence and CompilerBoundaryEvidence, by name. Every
        // field is always present in the canonical form, so the preimage can
        // never depend on what a writer chose to emit.
        let runner = vector_runner();
        let canonical = crate::canonical::canonical_json(&runner).unwrap();
        for field in [
            "compiler_binary_sha256",
            "fresh_state_directory",
            "fresh_output_directory",
            "daemon_endpoint_supplied",
            "retained_session_handle_supplied",
            "clock_boundary",
            "successful_exit",
            "native_output_verified",
            "output_sha256",
            "output_size_bytes",
        ] {
            assert!(
                canonical.contains(&format!("\"{field}\":")),
                "{field} omitted"
            );
        }
        let compiler = crate::boundary::tests::evidence().compiler;
        let canonical = crate::canonical::canonical_json(&compiler).unwrap();
        for field in [
            "boundary",
            "pipeline",
            "session_count",
            "root_request_count",
            "configuration",
            "completed_stages",
            "accepted_inputs",
            "embedded_assets",
            "artifact_hits",
            "emitted_output_sha256",
            "emitted_output_size_bytes",
        ] {
            assert!(
                canonical.contains(&format!("\"{field}\":")),
                "{field} omitted"
            );
        }
    }

    #[test]
    fn schema_version_is_outside_every_preimage() {
        // Re-encoding changes the record's version and nothing about what was
        // measured, so the digests of a v1 record's evidence and the digests
        // stored in its v2 form are the same strings.
        let full = full_run();
        let encoded = encode_stored_v3(&full).unwrap();
        let original = full.workloads[0].samples[0].boundary_evidence[0].clone();
        assert_eq!(
            encoded.workloads[0].samples[0].boundary_processes[0],
            identity_digest(&original.runner, &original.compiler).unwrap()
        );
    }

    #[test]
    fn encoding_round_trips_the_witness_exactly() {
        let full = full_run();
        let encoded = encode_stored_v3(&full).unwrap();
        let run = encoded.boundary.as_ref().unwrap();
        let workload = encoded.workloads[0].boundary.as_ref().unwrap();
        let (runner, compiler) = reassemble_witness(run, workload);
        let witness = &full.workloads[0].samples[0].boundary_evidence[0];
        assert_eq!(runner, witness.runner);
        assert_eq!(compiler, witness.compiler);
        assert_eq!(workload.critical_path, witness.critical_path);
        assert_eq!(workload.compiler_work, witness.compiler_work);
        assert_eq!(workload.critical_path_source.sample_index, 0);
        assert_eq!(workload.critical_path_source.process_index, 0);
    }

    #[test]
    fn one_worker_records_carry_both_digests_per_process() {
        let full = full_run();
        let encoded = encode_stored_v3(&full).unwrap();
        for observation in &encoded.workloads {
            for sample in &observation.samples {
                assert!(sample.boundary_evidence.is_empty());
                assert_eq!(sample.boundary_processes.len(), sample.batch_size as usize);
                assert_eq!(
                    sample.boundary_work_processes.len(),
                    sample.batch_size as usize
                );
            }
        }
    }

    #[test]
    fn a_parallel_record_stores_no_work_digests() {
        let mut full = full_run();
        for observation in &mut full.workloads {
            for sample in &mut observation.samples {
                for evidence in &mut sample.boundary_evidence {
                    evidence.compiler.configuration.requested_workers = WorkerSetting::Automatic;
                    evidence.compiler.configuration.resolved_workers = 4;
                }
            }
        }
        let encoded = encode_stored_v3(&full).unwrap();
        for observation in &encoded.workloads {
            assert!(observation.boundary.is_some());
            for sample in &observation.samples {
                assert_eq!(sample.boundary_processes.len(), sample.batch_size as usize);
                assert!(sample.boundary_work_processes.is_empty());
            }
        }
    }

    #[test]
    fn an_evidence_free_record_changes_only_its_version() {
        let mut full = full_run();
        for observation in &mut full.workloads {
            for sample in &mut observation.samples {
                sample.boundary_evidence = Vec::new();
            }
        }
        let encoded = encode_stored_v3(&full).unwrap();
        assert_eq!(encoded.schema_version, crate::RUN_SCHEMA_VERSION);
        assert!(encoded.boundary.is_none());
        for observation in &encoded.workloads {
            assert!(observation.boundary.is_none());
            for sample in &observation.samples {
                assert!(sample.boundary_processes.is_empty());
                assert!(sample.boundary_work_processes.is_empty());
            }
        }
        let mut expected = full.clone();
        expected.schema_version = crate::RUN_SCHEMA_VERSION;
        expected.full_evidence = Some(crate::canonical::content_address(&full).unwrap());
        assert_eq!(encoded, expected);
    }

    #[test]
    fn a_disagreeing_process_still_disagrees_after_encoding() {
        let mut full = full_run();
        // The startup workload batches 40 processes per sample.
        let samples = &mut full.workloads[1].samples;
        let evidence = &mut samples[0].boundary_evidence;
        assert!(
            evidence.len() >= 2,
            "fixture must batch at least two processes"
        );
        evidence[1].runner.output_sha256 = "c".repeat(64);
        let encoded = encode_stored_v3(&full).unwrap();
        let digests = &encoded.workloads[1].samples[0].boundary_processes;
        assert_ne!(digests[0], digests[1]);
        assert_eq!(digests[0], digests[2]);
    }

    #[test]
    fn a_record_already_encoded_is_refused() {
        let full = full_run();
        let encoded = encode_stored_v3(&full).unwrap();
        assert_eq!(
            encode_stored_v3(&encoded),
            Err(EncodeError::NotFullEvidence {
                found: crate::RUN_SCHEMA_VERSION
            })
        );
    }

    #[test]
    fn historical_full_evidence_v1_refuses_lossy_reencoding() {
        let mut historical = full_run();
        historical.schema_version = LEGACY_FULL_EVIDENCE_SCHEMA_VERSION;
        let before = historical.clone();
        let error =
            encode_stored_v3(&historical).expect_err("historical evidence must not lose fields");
        assert_eq!(error, EncodeError::HistoricalFullEvidenceUnsupported);
        assert!(
            error
                .to_string()
                .contains("cannot be losslessly re-encoded")
        );
        assert_eq!(historical, before, "refusal must not mutate the input");
    }

    #[test]
    fn a_named_evidence_type_with_a_missing_field_refuses_to_parse() {
        // The other half of rule 3: no `serde(default)` may be added either.
        // A field absent from stored bytes must fail the parse, not fill in.
        for field in ["output_sha256", "successful_exit", "clock_boundary"] {
            let mut value = serde_json::to_value(vector_runner()).unwrap();
            value.as_object_mut().unwrap().remove(field);
            let parsed: Result<RunnerBoundaryEvidence, _> = serde_json::from_value(value);
            assert!(parsed.is_err(), "{field} parsed as a default");
        }
        for field in ["accepted_inputs", "emitted_output_sha256", "artifact_hits"] {
            let mut value =
                serde_json::to_value(crate::boundary::tests::evidence().compiler).unwrap();
            value.as_object_mut().unwrap().remove(field);
            let parsed: Result<CompilerBoundaryEvidence, _> = serde_json::from_value(value);
            assert!(parsed.is_err(), "{field} parsed as a default");
        }
    }

    #[test]
    fn empty_collections_still_serialize_their_keys() {
        // A `skip_serializing_if` on an empty collection would pass a
        // non-empty fixture's field-presence test while changing the preimage
        // of every record whose value is empty. Pin the empty case directly.
        let mut compiler = crate::boundary::tests::evidence().compiler;
        compiler.accepted_inputs = Vec::new();
        compiler.embedded_assets = Vec::new();
        compiler.configuration.preview_features = Vec::new();
        let canonical = crate::canonical::canonical_json(&compiler).unwrap();
        for field in ["accepted_inputs", "embedded_assets", "preview_features"] {
            assert!(
                canonical.contains(&format!("\"{field}\":[]")),
                "{field} omitted when empty: {canonical}"
            );
        }
    }

    #[test]
    fn the_work_digest_preimage_serializes_every_group() {
        // `CompilerWork` is the v3 work-digest preimage. Retired v2 lowering
        // evidence is intentionally absent from this public wire shape.
        let mut work = CompilerWork::default();
        work.cfg_optimization.constopt_fold_attempts = 1;
        let canonical = crate::canonical::canonical_json(&work).unwrap();
        for field in [
            "candidate_body_plan_construction",
            "candidate_body_plan_materialization",
            "canonical_rir_presentation",
            "semantic_provider",
            "semantic_reachability",
            "semantic_analysis_structure",
            "cfg_materialization",
            "cfg_prerequisites",
            "cfg_retained_charge",
            "cfg_local_epoch",
            "cfg_optimization",
            "query_runtime",
            "publication",
        ] {
            assert!(
                canonical.contains(&format!("\"{field}\":")),
                "{field} omitted"
            );
        }
        assert_ne!(
            work_digest(&work).unwrap(),
            work_digest(&CompilerWork::default()).unwrap(),
            "nonzero optimizer work must participate in work.2"
        );
    }

    #[test]
    fn zero_cfg_optimization_preserves_the_legacy_work2_preimage() {
        let work = CompilerWork::default();
        let canonical = crate::canonical::canonical_json(&work).unwrap();
        assert!(!canonical.contains("\"cfg_optimization\":"), "{canonical}");
        assert_eq!(
            work_digest(&work).unwrap(),
            "a0f93c49a0b63bf6009aa3b3e6e1b78b599eb2187b20fcae6988e2cfaa7e8bd9"
        );
    }

    #[test]
    fn the_identity_preimage_is_the_two_key_object() {
        let evidence = crate::boundary::tests::evidence();
        let preimage = IdentityPreimage {
            runner: &evidence.runner,
            compiler: &evidence.compiler,
        };
        let canonical = crate::canonical::canonical_json(&preimage).unwrap();
        let expected = format!(
            "{{\"compiler\":{},\"runner\":{}}}",
            crate::canonical::canonical_json(&evidence.compiler).unwrap(),
            crate::canonical::canonical_json(&evidence.runner).unwrap()
        );
        assert_eq!(canonical, expected);
    }

    #[test]
    fn an_encoded_record_round_trips_through_stored_bytes() {
        let encoded = encode_stored_v3(&full_run()).unwrap();
        let serialized = crate::canonical::canonical_json(&encoded).unwrap();
        let parsed: RunObject = serde_json::from_str(&serialized).unwrap();
        assert_eq!(parsed, encoded);
        assert_eq!(
            crate::canonical::canonical_json(&parsed).unwrap(),
            serialized
        );
    }

    #[test]
    fn v1_bytes_without_the_new_fields_reserialize_byte_identically() {
        // A stored v1 record never contained the v2 keys; parsing it must not
        // materialize them, or every pre-change address would move.
        let full = full_run();
        let serialized = crate::canonical::canonical_json(&full).unwrap();
        let value: serde_json::Value = serde_json::from_str(&serialized).unwrap();
        let top = value.as_object().unwrap();
        assert!(!top.contains_key("boundary"));
        assert!(!top.contains_key("full_evidence"));
        let sample = &value["workloads"][0]["samples"][0];
        let sample = sample.as_object().unwrap();
        assert!(!sample.contains_key("boundary_processes"));
        assert!(!sample.contains_key("boundary_work_processes"));
        let parsed: RunObject = serde_json::from_str(&serialized).unwrap();
        assert_eq!(
            crate::canonical::canonical_json(&parsed).unwrap(),
            serialized
        );
        assert_eq!(
            crate::content_address(&parsed).unwrap(),
            crate::content_address(&full).unwrap()
        );
    }
}
