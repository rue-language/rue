//! The pre-merge answer to "would this change stall the performance series?".
//!
//! Every pin that can still refuse a run is a content hash of a file in the
//! tree, so the question is decidable from a checkout alone — no measurement,
//! no data branch, no runner. That is what lets it run on a pull request and
//! fail the author who introduced the drift, rather than being discovered days
//! later by whoever notices the chart stopped moving.
//!
//! Resolution goes through `pins`, the same module the collector uses. A second
//! implementation would eventually disagree with the first, and the gate would
//! then be certifying something other than what collection enforces.

use std::collections::BTreeMap;
use std::path::PathBuf;

use rue_perf_schema::{Manifest, PlatformEpoch};

use crate::pins;

/// One pinned component whose declared value no longer matches the tree.
#[derive(Debug, PartialEq, Eq)]
pub struct PinDrift {
    pub platform: String,
    pub epoch: u32,
    /// The pin's name, matching `ValidationError::PinMismatch`'s spelling.
    pub field: String,
    pub declared: String,
    pub resolved: String,
    /// What a maintainer would look at to understand the change.
    pub source: String,
}

/// Compare one epoch's declared pins against values resolved from the tree.
///
/// Pure, so the comparison is testable without a compiler or a checkout. Only
/// the resolvable pins appear here: `target` and `args` are declarations with
/// nothing in the tree to disagree with, so they cannot drift.
pub fn compare(
    epoch: &PlatformEpoch,
    resolved_toolchain: &str,
    resolved_workloads: &BTreeMap<String, String>,
) -> Vec<PinDrift> {
    let mut drifts = Vec::new();
    if epoch.toolchain_hash != resolved_toolchain {
        drifts.push(PinDrift {
            platform: epoch.platform.clone(),
            epoch: epoch.id,
            field: "toolchain_hash".to_string(),
            declared: epoch.toolchain_hash.clone(),
            resolved: resolved_toolchain.to_string(),
            source: "rust-toolchain.toml".to_string(),
        });
    }
    for (workload, declared) in &epoch.workload_source_hashes {
        // A workload the resolver could not read is reported as drift rather
        // than skipped: "we could not tell" must never pass a gate whose whole
        // job is to answer whether the series will keep running.
        let resolved = resolved_workloads
            .get(workload)
            .map(String::as_str)
            .unwrap_or("<unresolved>");
        if resolved != declared {
            drifts.push(PinDrift {
                platform: epoch.platform.clone(),
                epoch: epoch.id,
                field: format!("workload_source_hashes/{workload}"),
                declared: declared.clone(),
                resolved: resolved.to_string(),
                source: format!("the {workload} workload's own sources"),
            });
        }
    }
    drifts
}

/// Render the drift report a failing gate prints.
///
/// The message is the remedy. With no bypass available, an author who cannot
/// tell what to do next is blocked, so this states which epoch to declare and
/// the values to declare it with rather than only that a hash moved.
pub fn render(drifts: &[PinDrift]) -> String {
    let mut out = String::new();
    let count = drifts.len();
    let plural = if count == 1 { "pin has" } else { "pins have" };
    out.push_str(&format!("{count} {plural} drifted from the tree:\n\n"));
    for drift in drifts {
        out.push_str(&format!(
            "  {} epoch {} — {}\n    declared  {}\n    resolved  {}\n    from      {}\n\n",
            drift.platform, drift.epoch, drift.field, drift.declared, drift.resolved, drift.source
        ));
    }
    for line in [
        "Collection would refuse every run measured after this change, and the published",
        "dashboard would stop moving without anything going red.",
        "",
        "Two ways forward:",
        "",
        "  1. Declare the next epoch for each platform above in performance/manifest.toml,",
        "     carrying the resolved values shown here. Mark the new epoch `collection = true`",
        "     and drop that marking from the old one. A new epoch needs no baseline to begin",
        "     accepting runs; its baseline is declared later, once one has been measured.",
        "",
        "  2. Or revert the change to the pinned input.",
    ] {
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// Resolve the tree's pins and compare them against every collection epoch.
pub fn run() -> Result<(), String> {
    let mut manifest_path = None;
    let mut repo_root = None;
    let mut compiler = None;
    let mut std_root = None;
    let mut args = std::env::args().skip(2);
    while let Some(flag) = args.next() {
        let mut value = || {
            args.next()
                .ok_or_else(|| format!("{flag} requires a value"))
        };
        match flag.as_str() {
            "--manifest" => manifest_path = Some(PathBuf::from(value()?)),
            "--repo-root" => repo_root = Some(PathBuf::from(value()?)),
            "--compiler" => compiler = Some(PathBuf::from(value()?)),
            "--std-root" => std_root = Some(PathBuf::from(value()?)),
            other => return Err(format!("unrecognized argument {other:?}")),
        }
    }
    let manifest_path = manifest_path.ok_or("check-pins requires --manifest <path>")?;
    let compiler = compiler.ok_or("check-pins requires --compiler <path>")?;
    let repo_root = repo_root.unwrap_or_else(|| PathBuf::from("."));
    let std_root = std_root.or_else(|| {
        let candidate = repo_root.join("std");
        candidate.is_dir().then_some(candidate)
    });

    let text = std::fs::read_to_string(&manifest_path)
        .map_err(|error| format!("could not read {}: {error}", manifest_path.display()))?;
    let manifest = Manifest::parse(&text).map_err(|error| error.to_string())?;

    let resolved_toolchain = pins::toolchain_hash(&repo_root).map_err(|error| error.to_string())?;

    let epochs: Vec<&PlatformEpoch> = manifest.collection_epochs().collect();
    if epochs.is_empty() {
        // Not a pass. A manifest with nothing marked for collection means the
        // gate is inspecting nothing, which looks identical to success.
        return Err(format!(
            "{} marks no epoch `collection = true`, so there is nothing to check",
            manifest_path.display()
        ));
    }

    let mut resolved_workloads: BTreeMap<String, String> = BTreeMap::new();
    let mut drifts = Vec::new();
    for epoch in &epochs {
        let suite = manifest
            .suite(epoch.suite_revision)
            .ok_or_else(|| format!("suite revision {} is not declared", epoch.suite_revision))?;
        for workload in &suite.workloads {
            if resolved_workloads.contains_key(&workload.id) {
                continue;
            }
            // Resolution does not depend on the epoch, so one pass over the
            // suite's workloads serves every platform.
            let source = repo_root.join(&workload.source);
            match pins::workload_source_hash(&compiler, &source, std_root.as_deref()) {
                Ok(hash) => {
                    resolved_workloads.insert(workload.id.clone(), hash);
                }
                Err(error) => {
                    return Err(format!(
                        "could not resolve the {} workload's sources: {error}",
                        workload.id
                    ));
                }
            }
        }
        drifts.extend(compare(epoch, &resolved_toolchain, &resolved_workloads));
    }

    if drifts.is_empty() {
        eprintln!(
            "rue-bench: {} collection epoch(s) still match the tree",
            epochs.len()
        );
        return Ok(());
    }
    Err(render(&drifts))
}

#[cfg(test)]
mod tests {
    use super::*;

    const MANIFEST: &str = r#"
[[suite]]
revision = 1
timing_schema_version = 1
protocol_version = 1

[[suite.workloads]]
id = "startup"
source = "performance/workloads/startup/main.rue"
question = "What does a minimal fresh compilation cost end to end?"

[[epoch]]
id = 1
platform = "probe"
suite_revision = 1
target = "x86_64-unknown-linux-gnu"
args = []
toolchain_hash = "toolchain"

[epoch.workload_source_hashes]
startup = "startup-hash"

[epoch.environment]
runner_label = "github-hosted"
runner_image = "ubuntu-24.04"

[epoch.sampling.startup]
samples = 3
batch_size = 1

[epoch.flagging]
k = 2.0
window = 3
"#;

    fn epoch(collection: bool) -> PlatformEpoch {
        let text = if collection {
            MANIFEST.replace("id = 1\nplatform", "id = 1\ncollection = true\nplatform")
        } else {
            MANIFEST.to_string()
        };
        Manifest::parse(&text)
            .expect("fixture manifest")
            .epochs()
            .next()
            .expect("one epoch")
            .clone()
    }

    fn resolved(hash: &str) -> BTreeMap<String, String> {
        BTreeMap::from([("startup".to_string(), hash.to_string())])
    }

    #[test]
    fn a_tree_matching_its_epoch_reports_no_drift() {
        let drifts = compare(&epoch(false), "toolchain", &resolved("startup-hash"));
        assert!(drifts.is_empty(), "{drifts:?}");
    }

    #[test]
    fn a_changed_rust_toolchain_is_drift() {
        let drifts = compare(&epoch(false), "upgraded", &resolved("startup-hash"));
        assert_eq!(drifts.len(), 1);
        assert_eq!(drifts[0].field, "toolchain_hash");
        assert_eq!(drifts[0].declared, "toolchain");
        assert_eq!(drifts[0].resolved, "upgraded");
    }

    #[test]
    fn an_edited_workload_source_is_drift() {
        let drifts = compare(&epoch(false), "toolchain", &resolved("edited"));
        assert_eq!(drifts.len(), 1);
        assert_eq!(drifts[0].field, "workload_source_hashes/startup");
    }

    #[test]
    fn a_workload_that_could_not_be_resolved_is_drift_rather_than_a_pass() {
        // "We could not tell" must not pass a gate that exists to answer
        // whether the series will keep running.
        let drifts = compare(&epoch(false), "toolchain", &BTreeMap::new());
        assert_eq!(drifts.len(), 1);
        assert_eq!(drifts[0].resolved, "<unresolved>");
    }

    #[test]
    fn every_drift_is_reported_rather_than_the_first() {
        let drifts = compare(&epoch(false), "upgraded", &resolved("edited"));
        assert_eq!(drifts.len(), 2, "{drifts:?}");
    }

    #[test]
    fn the_report_names_the_remedy_and_both_values() {
        // With no bypass available, an author who cannot tell what to do next
        // is blocked. The message has to carry the fix.
        let report = render(&compare(
            &epoch(false),
            "upgraded",
            &resolved("startup-hash"),
        ));
        assert!(report.contains("toolchain_hash"), "{report}");
        assert!(report.contains("upgraded"), "{report}");
        assert!(report.contains("performance/manifest.toml"), "{report}");
        assert!(report.contains("collection = true"), "{report}");
        assert!(report.contains("needs no baseline"), "{report}");
    }

    #[test]
    fn collection_marking_is_read_from_the_manifest() {
        assert!(!epoch(false).collection);
        assert!(epoch(true).collection);
    }
}
