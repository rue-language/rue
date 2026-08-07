//! Lower-frequency compiler scaling reports for maintained Rue programs.
//!
//! These records deliberately do not enter the ADR-0067 headline series. The
//! startup suite answers a high-frequency regression question; this report
//! answers how one fresh compiler process scales as maintained programs grow.
//! Both use the same raw [`crate::Sample`] and additive phase accounting.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::{EnvironmentFingerprint, Sample};

/// Version of the scaling-report wire format.
pub const SCALING_REPORT_SCHEMA_VERSION: u32 = 1;

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
    /// Maintained programs, in report order.
    pub workloads: Vec<ScalingWorkload>,
}

impl ScalingManifest {
    /// Parse and validate one scaling manifest.
    pub fn parse(text: &str) -> Result<Self, String> {
        let manifest: ScalingManifest =
            toml::from_str(text).map_err(|error| format!("invalid scaling manifest: {error}"))?;
        if manifest.schema_version != 1 {
            return Err(format!(
                "unsupported scaling manifest schema version {}",
                manifest.schema_version
            ));
        }
        if manifest.samples < 2 {
            return Err("scaling measurements require at least two samples".to_string());
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
    /// Compiler-produced fixture shape. A change makes cross-report comparison
    /// advisory even when the manifest revision was not yet advanced.
    pub shape: WorkloadShape,
    /// Independent raw fresh-process measurements.
    pub samples: Vec<Sample>,
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
schema_version = 1
revision = 2
samples = 3

[[workloads]]
id = "ruelex"
source = "examples/ruelex/main.rue"
question = "small maintained compiler frontend"
"#;

    #[test]
    fn parses_a_versioned_scaling_manifest() {
        let manifest = ScalingManifest::parse(VALID).unwrap();
        assert_eq!(manifest.revision, 2);
        assert_eq!(manifest.samples, 3);
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
}
