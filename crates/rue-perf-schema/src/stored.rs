//! Stored records: the name a record already carries.
//!
//! A record's name is the content address of the bytes it was published as,
//! which is what makes a stored record verifiable against its own name without
//! trusting whoever wrote it. Recomputing that name by re-serializing a parsed
//! record is a different function: it names the record as *this* build of
//! the schema would have written it, not as it was written.
//!
//! The two agree only while the schema struct is byte-identical to the one the
//! record was written under, and record fields are deliberately additive —
//! boundary evidence gains phase counters as the compiler gains phases, each
//! `#[serde(default)]` so older records stay readable. Every such field renames
//! every record written before it: the parse fills the field with a zero the
//! stored bytes never contained, and the re-serialized form carries it.
//!
//! That silently unnames pinned records. A manifest baseline names a run by the
//! address it was published under, so a renamed baseline stops resolving, its
//! epoch loses every workload ratio, and the headline index disappears from an
//! epoch that is otherwise collecting normally — with no rejected run, no failed
//! workflow, and nothing to see but an absence.
//!
//! So a reader takes the address from the record and never from the struct.

use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::CanonicalError;
use crate::run::RunObject;
use crate::runtime::RuntimeReport;

/// Why a stored record could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoredRecordError {
    /// The stored bytes are not JSON, or not a record of the expected kind.
    Malformed {
        /// The underlying parse error, rendered.
        detail: String,
    },
    /// The stored value has no canonical form, so it has no name.
    ///
    /// In practice a floating-point number, which raw records never contain:
    /// integer nanoseconds and integer bytes are what keep a record's name
    /// independent of how a writer formats a decimal.
    Unaddressable(CanonicalError),
}

impl std::fmt::Display for StoredRecordError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StoredRecordError::Malformed { detail } => {
                write!(f, "not a well-formed record: {detail}")
            }
            StoredRecordError::Unaddressable(error) => {
                write!(f, "stored record cannot be addressed: {error}")
            }
        }
    }
}

impl std::error::Error for StoredRecordError {}

/// A record together with the immutable name it carries.
///
/// Consumers that resolve a record by name — the dashboard's derivation
/// matching a manifest baseline, anything linking a plotted point back to its
/// raw record — must use [`Stored::address`] rather than re-addressing
/// [`Stored::record`], which names a value about to be written.
///
/// Generic over the record kind because the rule is a property of the *store*,
/// not of any one schema. ADR-0067's run objects and ADR-0072's runtime reports
/// are both append-only, both content-addressed, and both grow fields over
/// time; two copies of this would be two chances to get the naming rule wrong,
/// and the wrong one would be the one silently unnaming records.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stored<T> {
    address: String,
    record: T,
}

impl<T: DeserializeOwned> Stored<T> {
    /// Read a published record, taking its name from the record itself.
    ///
    /// The name is the address of the stored value's canonical form, which is
    /// the digest the publisher stored it under: it never depends on the fields
    /// this build of the schema happens to know about.
    pub fn read(text: &str) -> Result<Stored<T>, StoredRecordError> {
        let value: serde_json::Value =
            serde_json::from_str(text).map_err(|error| StoredRecordError::Malformed {
                detail: error.to_string(),
            })?;
        let address = crate::content_address(&value).map_err(StoredRecordError::Unaddressable)?;
        // Deserialized from the value that was addressed, so the typed view and
        // the name describe the same bytes.
        let record =
            serde_json::from_value(value).map_err(|error| StoredRecordError::Malformed {
                detail: error.to_string(),
            })?;
        Ok(Stored { address, record })
    }
}

impl<T: Serialize> Stored<T> {
    /// Name a record this process just built.
    ///
    /// Correct only because nothing has been stored yet: the canonical form
    /// being addressed is the one that will be written. A record read back from
    /// storage takes its name from [`Stored::read`] instead.
    pub fn minted(record: T) -> Result<Stored<T>, CanonicalError> {
        let address = crate::content_address(&record)?;
        Ok(Stored { address, record })
    }
}

impl<T> Stored<T> {
    /// The record's immutable name.
    pub fn address(&self) -> &str {
        &self.address
    }

    /// The record.
    pub fn record(&self) -> &T {
        &self.record
    }
}

/// One stored ADR-0067 compiler-performance run object.
pub type StoredRun = Stored<RunObject>;

/// One stored ADR-0072 runtime report.
pub type StoredRuntimeReport = Stored<RuntimeReport>;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::RUN_SCHEMA_VERSION;
    use crate::run::{
        Band, EnvironmentFingerprint, Invocation, Phase, PhaseAccounting, ResolvedPins,
        RunIdentity, Sample, WorkloadObservation,
    };
    use std::collections::BTreeMap;

    fn sample_run() -> RunObject {
        let mut phase_ns: BTreeMap<Phase, u64> =
            Phase::ALL.into_iter().map(|phase| (phase, 0)).collect();
        phase_ns.insert(Phase::SourceDiscoveryAndParsing, 900_000);
        RunObject {
            schema_version: RUN_SCHEMA_VERSION,
            identity: RunIdentity {
                suite_revision: 1,
                epoch: 1,
                platform: "x86_64-linux".to_string(),
                commit: "a".repeat(40),
                started_at: "2026-07-28T00:00:00Z".to_string(),
                finished_at: "2026-07-28T00:00:30Z".to_string(),
                pins: ResolvedPins {
                    toolchain_hash: "toolchain".to_string(),
                    stdlib_hash: "stdlib".to_string(),
                    workload_source_hashes: BTreeMap::from([(
                        "startup".to_string(),
                        "startup-hash".to_string(),
                    )]),
                    invocation: Invocation {
                        target: "x86_64-unknown-linux-gnu".to_string(),
                        args: Vec::new(),
                    },
                },
                environment: EnvironmentFingerprint {
                    runner_label: "github-hosted".to_string(),
                    runner_image: "ubuntu-24.04".to_string(),
                    runner_image_version: "20260720.1".to_string(),
                    cpu_model: "AMD EPYC 7763".to_string(),
                    core_count: 4,
                    memory_bytes: 16 * 1024 * 1024 * 1024,
                    kernel_version: "6.8.0".to_string(),
                    os_version: "Ubuntu 24.04".to_string(),
                    architecture: "x86_64".to_string(),
                },
            },
            workloads: vec![WorkloadObservation {
                workload: "startup".to_string(),
                samples: vec![Sample {
                    batch_size: 1,
                    process_elapsed_ns: 2_000_000,
                    peak_memory_bytes: 32 * 1024 * 1024,
                    output_binary_bytes: 12_288,
                    phases: PhaseAccounting {
                        phase_ns,
                        mixed_parallel_ns: 0,
                        unattributed_ns: 100_000,
                        compiler_root_ns: 1_000_000,
                    },
                    boundary_evidence: Vec::new(),
                }],
            }],
            failures: Vec::new(),
        }
    }

    #[test]
    fn a_stored_record_is_named_by_the_bytes_it_was_published_as() {
        let run = sample_run();
        let text = crate::canonical_json(&run).expect("addressable");
        let stored = StoredRun::read(&text).expect("readable");
        assert_eq!(stored.address(), run.content_address().unwrap());
        assert_eq!(stored.record(), &run);
    }

    #[test]
    fn a_schema_change_does_not_rename_an_existing_record() {
        // The defect this type exists to prevent. A record's fields are
        // additive: every `#[serde(default)]` field added to boundary evidence
        // parses into records written before it existed, so re-serializing one
        // yields bytes — and therefore a name — the record never had.
        //
        // Written here as the same drift in its cheapest form: stored bytes
        // carrying a field the current writer omits. Either direction renames
        // the record, and a renamed record is one a manifest baseline can no
        // longer find.
        let run = sample_run();
        let published = crate::canonical_json(&run).expect("addressable");
        let mut value: serde_json::Value = serde_json::from_str(&published).unwrap();
        value["workloads"][0]["samples"][0]
            .as_object_mut()
            .unwrap()
            .insert("boundary_evidence".to_string(), serde_json::json!([]));
        let as_written = crate::canonical_json(&value).expect("addressable");
        assert_ne!(as_written, published, "the fixture must actually differ");

        let stored = StoredRun::read(&as_written).expect("readable");
        assert_eq!(
            stored.address(),
            crate::content_address(&value).unwrap(),
            "the name must be the record's own, not this schema's rendering of it"
        );
        assert_ne!(
            stored.address(),
            stored.record().content_address().unwrap(),
            "re-serializing the parsed record is what renames it"
        );
    }

    #[test]
    fn insignificant_whitespace_does_not_change_a_record_name() {
        // The publisher refuses non-canonical bytes, so this cannot arrive from
        // the data branch; a record handed over some other way still resolves
        // by the name its value has.
        let run = sample_run();
        let canonical = crate::canonical_json(&run).expect("addressable");
        let pretty = serde_json::to_string_pretty(&run).unwrap();
        assert_eq!(
            StoredRun::read(&canonical).unwrap().address(),
            StoredRun::read(&pretty).unwrap().address()
        );
    }

    #[test]
    fn changing_a_measurement_changes_the_stored_name() {
        let run = sample_run();
        let mut altered = run.clone();
        altered.workloads[0].samples[0].phases.compiler_root_ns += 1;
        altered.workloads[0].samples[0]
            .phases
            .phase_ns
            .insert(Phase::SourceDiscoveryAndParsing, 900_001);
        assert_eq!(
            altered.workloads[0].samples[0].phases.attributed_ns(),
            altered.workloads[0].samples[0].phases.compiler_root_ns
        );
        let one = StoredRun::read(&crate::canonical_json(&run).unwrap()).unwrap();
        let two = StoredRun::read(&crate::canonical_json(&altered).unwrap()).unwrap();
        assert_ne!(one.address(), two.address());
        // The band the sample moved is the one the reader sees move.
        assert_eq!(
            two.record().workloads[0].samples[0]
                .phases
                .band_ns(Band::Phase(Phase::SourceDiscoveryAndParsing)),
            900_001
        );
    }

    #[test]
    fn a_minted_record_carries_the_name_it_will_be_written_under() {
        let run = sample_run();
        let minted = StoredRun::minted(run.clone()).expect("addressable");
        let published = crate::canonical_json(&run).expect("addressable");
        assert_eq!(
            minted.address(),
            StoredRun::read(&published).unwrap().address()
        );
    }

    #[test]
    fn a_malformed_record_is_reported_rather_than_guessed_at() {
        assert!(matches!(
            StoredRun::read("{"),
            Err(StoredRecordError::Malformed { .. })
        ));
        assert!(matches!(
            StoredRun::read(r#"{"schema_version":1}"#),
            Err(StoredRecordError::Malformed { .. })
        ));
    }
}
