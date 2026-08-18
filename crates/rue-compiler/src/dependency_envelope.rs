//! Deterministic presentation of one closed canonical import revision.

use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::{
    AcceptedReadManifestEntry, CanonicalImportGraphProblem, CanonicalImportResolution,
    ImportCandidateRole, ImportDiscoveryRevisionArtifact, ImportDiscoveryRevisionStatus,
    ImportObservationStatus, SourceRevision,
};

pub const DEPENDENCY_ENVELOPE_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DependencyEnvelopeStatus {
    Complete,
    Incomplete,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DependencyEnvelope {
    pub version: u32,
    pub status: DependencyEnvelopeStatus,
    pub revision: String,
    pub(crate) context: DependencyContext,
    pub topology: DependencyTopology,
    pub(crate) observations: Vec<DependencyObservation>,
    pub(crate) accepted_reads: Vec<DependencyAcceptedRead>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct DependencyContext {
    pub(crate) import_policy_version: u32,
    pub(crate) epoch: String,
    pub(crate) project_root: String,
    pub(crate) std_root: Option<String>,
    pub(crate) read_policy_revision: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DependencyTopology {
    pub root: String,
    pub records: Vec<DependencyTopologyRecord>,
    pub cycles: Vec<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DependencyTopologyRecord {
    pub importer: String,
    pub specifier: String,
    pub outcome: DependencyResolutionOutcome,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum DependencyResolutionOutcome {
    Resolved { module: String },
    Missing,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct DependencyObservation {
    pub(crate) request: DependencyRequest,
    pub(crate) outcome: DependencyObservationOutcome,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct DependencyRequest {
    pub(crate) policy_version: u32,
    pub(crate) epoch: String,
    pub(crate) read_policy_revision: String,
    pub(crate) importer: String,
    pub(crate) source_offset: u32,
    pub(crate) source_end: u32,
    pub(crate) exact_specifier: String,
    pub(crate) normalized_specifier: String,
    pub(crate) importer_anchor: String,
    pub(crate) root_anchor: String,
    pub(crate) group: u32,
    pub(crate) position: u32,
    pub(crate) requested_path: String,
    pub(crate) role: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub(crate) enum DependencyObservationOutcome {
    Absent,
    PresentReadable {
        canonical_path: String,
        physical_identity: String,
        metadata_fingerprint: String,
        content_fingerprint: String,
    },
    PresentUnreadable {
        message: String,
    },
    DeniedLexical,
    DeniedCanonical {
        canonical_path: String,
    },
    InvalidPhysicalType {
        canonical_path: String,
    },
    UnstableRead {
        message: String,
    },
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct DependencyAcceptedRead {
    pub(crate) module: String,
    pub(crate) requested_path: String,
    pub(crate) canonical_path: String,
    pub(crate) physical_identity: String,
    pub(crate) metadata_fingerprint: String,
    pub(crate) content_fingerprint: String,
}

impl DependencyEnvelope {
    /// Project the compiler-owned graph and ledgers without rediscovering an
    /// import edge or consulting semantic analysis.
    #[cfg(not(test))]
    pub fn from_closed_revision(revision: &crate::ImportDiscoveryView) -> Option<Self> {
        Self::from_artifact(&revision.inner)
    }

    #[cfg(test)]
    pub(crate) fn from_closed_revision(artifact: &ImportDiscoveryRevisionArtifact) -> Option<Self> {
        Self::from_artifact(artifact)
    }

    fn from_artifact(artifact: &ImportDiscoveryRevisionArtifact) -> Option<Self> {
        let graph = artifact.graph()?;
        let status = match artifact.status() {
            ImportDiscoveryRevisionStatus::ClosedValid if graph.validation().is_valid() => {
                DependencyEnvelopeStatus::Complete
            }
            ImportDiscoveryRevisionStatus::ClosedAttempted
                if !graph.validation().problems().is_empty()
                    && graph.validation().problems().iter().all(|problem| {
                        matches!(
                            problem,
                            CanonicalImportGraphProblem::MissingResolution { .. }
                        )
                    }) =>
            {
                DependencyEnvelopeStatus::Incomplete
            }
            ImportDiscoveryRevisionStatus::Open
            | ImportDiscoveryRevisionStatus::ClosedAttempted
            | ImportDiscoveryRevisionStatus::ClosedValid => return None,
        };
        let context = DependencyContext {
            import_policy_version: crate::IMPORT_DISCOVERY_POLICY_VERSION,
            epoch: artifact.context().epoch().to_string(),
            project_root: artifact.context().project_root().to_owned(),
            std_root: artifact.context().std_root().map(str::to_owned),
            read_policy_revision: artifact.context().read_policy_revision().to_owned(),
        };
        let topology = DependencyTopology {
            root: graph.graph().root().as_str().to_owned(),
            records: graph
                .graph()
                .records()
                .iter()
                .map(|record| DependencyTopologyRecord {
                    importer: record.importer().as_str().to_owned(),
                    specifier: record.normalized_specifier().to_owned(),
                    outcome: match record.resolution() {
                        CanonicalImportResolution::Resolved(module) => {
                            DependencyResolutionOutcome::Resolved {
                                module: module.as_str().to_owned(),
                            }
                        }
                        CanonicalImportResolution::Missing => DependencyResolutionOutcome::Missing,
                    },
                })
                .collect(),
            cycles: graph
                .validation()
                .cycles()
                .iter()
                .map(|cycle| {
                    cycle
                        .modules()
                        .iter()
                        .map(|module| module.as_str().to_owned())
                        .collect()
                })
                .collect(),
        };
        let observations: Vec<DependencyObservation> = artifact
            .ledger()
            .iter()
            .map(|observation| {
                let request = observation.request();
                let context = request.context();
                DependencyObservation {
                    request: DependencyRequest {
                        policy_version: crate::IMPORT_DISCOVERY_POLICY_VERSION,
                        epoch: context.epoch().to_string(),
                        read_policy_revision: context.read_policy_revision().to_owned(),
                        importer: request.occurrence().importer().as_str().to_owned(),
                        source_offset: request.occurrence().source_offset(),
                        source_end: request.occurrence().source_end(),
                        exact_specifier: request.exact_specifier().to_owned(),
                        normalized_specifier: request.normalized_specifier().to_owned(),
                        importer_anchor: request.importer_anchor().to_owned(),
                        root_anchor: request.root_anchor().to_owned(),
                        group: request.group() as u32,
                        position: request.position() as u32,
                        requested_path: request.requested_path().to_owned(),
                        role: candidate_role(request.role()),
                    },
                    outcome: observation_outcome(observation.status()),
                }
            })
            .collect();
        let accepted_reads: Vec<DependencyAcceptedRead> = artifact
            .accepted_read_manifest()
            .iter()
            .map(accepted_read)
            .collect();
        let revision = revision_label(
            artifact.source_revision(),
            status,
            &context,
            &topology,
            &observations,
            &accepted_reads,
        );
        Some(Self {
            version: DEPENDENCY_ENVELOPE_VERSION,
            status,
            revision,
            context,
            topology,
            observations,
            accepted_reads,
        })
    }
}

fn candidate_role(role: ImportCandidateRole) -> &'static str {
    match role {
        ImportCandidateRole::ExactFile => "exact_file",
        ImportCandidateRole::FileModule => "file_module",
        ImportCandidateRole::DirectoryFacade => "directory_facade",
        ImportCandidateRole::StandardLibraryFacade => "standard_library_facade",
    }
}

fn observation_outcome(status: &ImportObservationStatus) -> DependencyObservationOutcome {
    match status {
        ImportObservationStatus::Absent => DependencyObservationOutcome::Absent,
        ImportObservationStatus::PresentReadable {
            canonical_path,
            metadata_identity,
            metadata_fingerprint,
            content_fingerprint,
        } => DependencyObservationOutcome::PresentReadable {
            canonical_path: canonical_path.to_string(),
            physical_identity: physical_identity(*metadata_identity),
            metadata_fingerprint: metadata_fingerprint_string(*metadata_fingerprint),
            content_fingerprint: fingerprint(*content_fingerprint),
        },
        ImportObservationStatus::PresentUnreadable(message) => {
            DependencyObservationOutcome::PresentUnreadable {
                message: message.to_string(),
            }
        }
        ImportObservationStatus::DeniedLexical => DependencyObservationOutcome::DeniedLexical,
        ImportObservationStatus::DeniedCanonical { canonical_path } => {
            DependencyObservationOutcome::DeniedCanonical {
                canonical_path: canonical_path.to_string(),
            }
        }
        ImportObservationStatus::InvalidPhysicalType { canonical_path } => {
            DependencyObservationOutcome::InvalidPhysicalType {
                canonical_path: canonical_path.to_string(),
            }
        }
        ImportObservationStatus::UnstableRead(message) => {
            DependencyObservationOutcome::UnstableRead {
                message: message.to_string(),
            }
        }
        ImportObservationStatus::Cancelled => DependencyObservationOutcome::Cancelled,
    }
}

fn accepted_read(entry: &AcceptedReadManifestEntry) -> DependencyAcceptedRead {
    DependencyAcceptedRead {
        module: entry.module().as_str().to_owned(),
        requested_path: entry.requested_path().to_owned(),
        canonical_path: entry.canonical_path().to_owned(),
        physical_identity: physical_identity(entry.metadata_identity()),
        metadata_fingerprint: metadata_fingerprint_string(entry.metadata_fingerprint()),
        content_fingerprint: fingerprint(entry.content_fingerprint()),
    }
}

fn physical_identity(identity: crate::PhysicalFileIdentity) -> String {
    format!("{:016x}:{:016x}", identity.volume(), identity.file())
}

fn metadata_fingerprint_string(fingerprint: crate::FileMetadataFingerprint) -> String {
    format!(
        "{:016x}:{:016x}:{:016x}",
        fingerprint.length(),
        fingerprint.modified(),
        fingerprint.changed()
    )
}

fn fingerprint(value: u64) -> String {
    format!("{value:016x}")
}

fn revision_label(
    revision: &SourceRevision,
    status: DependencyEnvelopeStatus,
    context: &DependencyContext,
    topology: &DependencyTopology,
    observations: &[DependencyObservation],
    accepted_reads: &[DependencyAcceptedRead],
) -> String {
    let mut digest = Sha256::new();
    digest.update(b"rue-dependency-revision-v1\0");
    update_digest(&mut digest, revision.root().as_str().as_bytes());
    for module in revision.modules() {
        update_digest(&mut digest, module.module.as_str().as_bytes());
        update_digest(&mut digest, module.source.to_string().as_bytes());
    }
    let presentation =
        serde_json::to_vec(&(status, context, topology, observations, accepted_reads))
            .expect("dependency envelope values have infallible JSON serialization");
    update_digest(&mut digest, &presentation);
    format!("rue-dependency-revision-v1-sha256:{:x}", digest.finalize())
}

fn update_digest(digest: &mut Sha256, bytes: &[u8]) {
    digest.update((bytes.len() as u64).to_be_bytes());
    digest.update(bytes);
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::{
        AcceptedImportSource, CompilerSession, DiscoverySourceAssembler, FileMetadataFingerprint,
        ImportDiscoveryContext, ImportObservation, ImportObservationLedger, PhysicalFileIdentity,
    };

    fn context(epoch: u64) -> ImportDiscoveryContext {
        ImportDiscoveryContext::new(epoch, "/project", Some("/sdk"), "test-policy").unwrap()
    }

    fn metadata() -> FileMetadataFingerprint {
        FileMetadataFingerprint::new(10, 20, 30)
    }

    fn assembler(source: &str) -> DiscoverySourceAssembler {
        DiscoverySourceAssembler::new(
            context(1),
            "/project/main.rue",
            "/project/main.rue",
            PhysicalFileIdentity::new(1, 1),
            metadata(),
            Arc::new(source.into()),
        )
        .unwrap()
    }

    #[test]
    fn complete_envelope_uses_canonical_graph_and_separate_ledgers() {
        // Policy v2: the extensionless import resolves its facade candidate
        // (PresentReadable), and the std chain's vendored miss before the
        // toolchain hit supplies a benign Absent observation.
        let source = "const a = @import(\"a\"); const s = @import(\"std\"); fn main() -> i32 { 0 }";
        let imported = "pub fn answer() -> i32 { 42 }";
        let std_text = "pub fn parse_i64(s: str) -> i64 { 0 }";
        let mut assembler = assembler(source);
        assembler
            .add_explicit(
                "/project/a/_a.rue",
                "/project/a/_a.rue",
                PhysicalFileIdentity::new(1, 2),
                metadata(),
                Arc::new(imported.into()),
            )
            .unwrap();
        assembler
            .add_explicit(
                "/sdk/_std.rue",
                "/sdk/_std.rue",
                PhysicalFileIdentity::new(1, 3),
                metadata(),
                Arc::new(std_text.into()),
            )
            .unwrap();
        let snapshot = assembler.snapshot().unwrap();
        let mut session = CompilerSession::new();
        let plan = session
            .stage_import_discovery(
                &snapshot,
                context(1),
                assembler.accepted_read_manifest().shared_slice(),
                ImportObservationLedger::default(),
            )
            .unwrap();
        let mut ledger = ImportObservationLedger::default();
        loop {
            let pending = plan.pending_requests(&ledger);
            if pending.is_empty() {
                break;
            }
            for request in pending {
                let observation = if request.requested_path() == "/project/a/_a.rue" {
                    ImportObservation::accepted(
                        request.clone(),
                        AcceptedImportSource::new(
                            request.requested_path(),
                            "/project/a/_a.rue",
                            PhysicalFileIdentity::new(1, 2),
                            metadata(),
                            Arc::new(imported.into()),
                        )
                        .unwrap(),
                    )
                    .unwrap()
                } else if request.requested_path() == "/sdk/_std.rue" {
                    ImportObservation::accepted(
                        request.clone(),
                        AcceptedImportSource::new(
                            request.requested_path(),
                            "/sdk/_std.rue",
                            PhysicalFileIdentity::new(1, 3),
                            metadata(),
                            Arc::new(std_text.into()),
                        )
                        .unwrap(),
                    )
                    .unwrap()
                } else {
                    ImportObservation::absent(request)
                };
                ledger.record(observation).unwrap();
            }
        }
        let closed = session.close_import_discovery(ledger).unwrap();
        let envelope = DependencyEnvelope::from_closed_revision(&closed).unwrap();

        assert_eq!(envelope.status, DependencyEnvelopeStatus::Complete);
        assert_eq!(envelope.topology.root, "main.rue");
        assert!(matches!(
            envelope.topology.records[0].outcome,
            DependencyResolutionOutcome::Resolved { ref module } if module == "a/_a.rue"
        ));
        assert!(envelope.observations.iter().any(|observation| matches!(
            observation.outcome,
            DependencyObservationOutcome::Absent
        )));
        assert!(envelope.observations.iter().any(|observation| matches!(
            observation.outcome,
            DependencyObservationOutcome::PresentReadable { .. }
        )));
        assert_eq!(envelope.accepted_reads.len(), 3);
        assert!(
            envelope
                .revision
                .starts_with("rue-dependency-revision-v1-sha256:")
        );
    }

    #[test]
    fn missing_envelope_is_revision_labeled_and_incomplete() {
        let mut assembler = assembler("const x = @import(\"missing\"); fn main() -> i32 { 0 }");
        let snapshot = assembler.snapshot().unwrap();
        let mut session = CompilerSession::new();
        let plan = session
            .stage_import_discovery(
                &snapshot,
                context(1),
                assembler.accepted_read_manifest().shared_slice(),
                ImportObservationLedger::default(),
            )
            .unwrap();
        let mut ledger = ImportObservationLedger::default();
        loop {
            let pending = plan.pending_requests(&ledger);
            if pending.is_empty() {
                break;
            }
            for request in pending {
                ledger.record(ImportObservation::absent(request)).unwrap();
            }
        }
        session.close_import_discovery(ledger).unwrap_err();
        let envelope =
            DependencyEnvelope::from_closed_revision(session.discovery_attempt().unwrap()).unwrap();

        assert_eq!(envelope.status, DependencyEnvelopeStatus::Incomplete);
        assert!(matches!(
            envelope.topology.records[0].outcome,
            DependencyResolutionOutcome::Missing
        ));
        assert_eq!(envelope.accepted_reads.len(), 1);
        assert!(envelope.observations.iter().all(|observation| matches!(
            observation.outcome,
            DependencyObservationOutcome::Absent
        )));
    }

    #[test]
    fn policy_denial_is_not_serialized_as_a_probe_or_read() {
        assert!(matches!(
            observation_outcome(&ImportObservationStatus::DeniedLexical),
            DependencyObservationOutcome::DeniedLexical
        ));
        assert!(matches!(
            observation_outcome(&ImportObservationStatus::Absent),
            DependencyObservationOutcome::Absent
        ));
    }

    #[test]
    fn malformed_import_shape_is_not_an_incomplete_envelope() {
        let mut assembler = assembler("fn main() -> i32 { @import(); 0 }");
        let snapshot = assembler.snapshot().unwrap();
        let mut session = CompilerSession::new();
        let plan = session
            .stage_import_discovery(
                &snapshot,
                context(1),
                assembler.accepted_read_manifest().shared_slice(),
                ImportObservationLedger::default(),
            )
            .unwrap();
        assert!(
            plan.pending_requests(&ImportObservationLedger::default())
                .is_empty()
        );
        session
            .close_import_discovery(ImportObservationLedger::default())
            .unwrap_err();
        let attempted = session.discovery_attempt().unwrap();
        assert!(attempted.graph().is_some());
        assert!(attempted.graph().unwrap().validation().is_valid());
        assert!(DependencyEnvelope::from_closed_revision(attempted).is_none());
    }
}
