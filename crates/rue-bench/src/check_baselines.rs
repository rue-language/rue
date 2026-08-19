//! Answer "does every declared baseline still name a record that exists?".
//!
//! `derive` resolves an epoch's baseline by address among that epoch's own
//! records (`derive.rs:1316`). A miss is not an error there: the epoch simply
//! publishes no index and no workload ratios while still plotting every
//! per-workload series, so the dashboard keeps moving and loses the one number
//! it exists to publish. `validate-performance-stall.py`'s `unindexed()` gate
//! reports exactly that — but only for the epoch holding each platform's newest
//! point.
//!
//! That gate cannot be extended to cover a retired epoch by editing it, because
//! the records are not there to inspect. `staleness-inputs` selects the live
//! epoch alone before `derive` runs (RUE-1542), so a retired epoch's records
//! are never materialized into the gate's data root. Selecting every epoch that
//! declares a baseline would restore the cost RUE-1542 removed: on
//! 2026-08-18 that is 1,437 of 1,440 records, against the 321 the gate reads
//! now.
//!
//! So the question is asked where it is cheap and total instead. Baseline
//! resolution needs no derived data and no run object — only the manifest and
//! `index.json`, which the gate has already checked out to decide what to read.
//! This covers every epoch, live or retired, at the cost of one pass over an
//! index the step holds anyway.
//!
//! The check is deliberately stricter than "the address exists somewhere". A
//! baseline must be a record of its own epoch and platform, because that is
//! what `derive` requires: an address that resolves under a different epoch
//! resolves to nothing where it is looked up, and would otherwise pass a
//! whole-store existence test while still publishing no index.

use std::path::{Path, PathBuf};

use rue_perf_schema::Manifest;

use crate::staleness_inputs::{Index, parse_index};

/// One epoch whose declared baseline does not resolve to a record of that epoch.
#[derive(Debug, PartialEq, Eq)]
pub struct UnresolvedBaseline {
    pub platform: String,
    pub epoch: u32,
    /// The address the manifest declares.
    pub run: String,
    /// Why it did not resolve, in the words the report prints.
    pub detail: String,
}

/// Compare every declared baseline against the index.
///
/// Pure, so the rule is testable without a checkout or a data branch. Epochs
/// with no baseline are skipped rather than reported: declaring an epoch before
/// its first complete run is the documented state, and `unpinned` is what holds
/// that state to a deadline.
pub fn unresolved(manifest: &Manifest, index: &Index) -> Vec<UnresolvedBaseline> {
    let mut missing = Vec::new();
    for epoch in manifest.epochs() {
        let Some(baseline) = epoch.baseline.as_ref() else {
            continue;
        };
        let entry = index.runs.iter().find(|entry| entry.run == baseline.run);
        let detail = match entry {
            None => Some("no record in index.json carries this address".to_string()),
            Some(entry) if entry.platform != epoch.platform || entry.epoch != epoch.id => {
                Some(format!(
                    "the record is {}'s epoch {}, not {}'s epoch {}",
                    entry.platform, entry.epoch, epoch.platform, epoch.id
                ))
            }
            Some(_) => None,
        };
        if let Some(detail) = detail {
            missing.push(UnresolvedBaseline {
                platform: epoch.platform.clone(),
                epoch: epoch.id,
                run: baseline.run.clone(),
                detail,
            });
        }
    }
    missing
}

pub fn run() -> Result<u8, String> {
    let mut manifest_path: Option<PathBuf> = None;
    let mut index_path: Option<PathBuf> = None;
    let mut args = std::env::args().skip(2);
    while let Some(flag) = args.next() {
        let mut value = || {
            args.next()
                .ok_or_else(|| format!("{flag} requires a value"))
        };
        match flag.as_str() {
            "--manifest" => manifest_path = Some(PathBuf::from(value()?)),
            "--index" => index_path = Some(PathBuf::from(value()?)),
            other => return Err(format!("unrecognized argument {other:?}")),
        }
    }
    let manifest_path = manifest_path.ok_or("check-baselines requires --manifest <path>")?;
    let index_path = index_path.ok_or("check-baselines requires --index <path>")?;
    report(&manifest_path, &index_path, &mut std::io::stdout())
}

fn report(
    manifest_path: &Path,
    index_path: &Path,
    out: &mut impl std::io::Write,
) -> Result<u8, String> {
    let manifest_text = std::fs::read_to_string(manifest_path)
        .map_err(|error| format!("could not read {}: {error}", manifest_path.display()))?;
    let manifest = Manifest::parse(&manifest_text).map_err(|error| error.to_string())?;

    // A missing index is a corrupt data branch here, not a first collection:
    // the caller checked the branch out to reach this point. Tolerating it
    // would pass the gate in exactly the state it exists to report.
    let index_text = std::fs::read_to_string(index_path)
        .map_err(|error| format!("could not read {}: {error}", index_path.display()))?;
    let index = parse_index(&index_text)?;

    let missing = unresolved(&manifest, &index);
    let declared = manifest
        .epochs()
        .filter(|epoch| epoch.baseline.is_some())
        .count();
    if missing.is_empty() {
        writeln!(out, "{declared} declared baseline(s) resolve")
            .map_err(|error| format!("could not write the report: {error}"))?;
        return Ok(crate::exit::OK);
    }
    for item in &missing {
        writeln!(
            out,
            "{} epoch {}: baseline {} does not resolve — {}",
            item.platform, item.epoch, item.run, item.detail
        )
        .map_err(|error| format!("could not write the report: {error}"))?;
    }
    writeln!(
        out,
        "{} of {declared} declared baseline(s) do not resolve; \
         the epoch publishes no index and no workload ratios",
        missing.len()
    )
    .map_err(|error| format!("could not write the report: {error}"))?;
    Ok(crate::exit::NOT_APPENDABLE)
}

#[cfg(test)]
mod tests {
    use super::*;

    // The shape `derive`'s own tests use, so the fixture cannot drift from
    // what the manifest parser actually requires.
    const BASE: &str = r#"
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
collection = true
platform = "x86_64-linux"
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

    fn manifest_with(baseline: Option<(&str, &str)>) -> Manifest {
        let text = match baseline {
            None => BASE.to_string(),
            Some((commit, run)) => {
                format!("{BASE}\n[epoch.baseline]\ncommit = \"{commit}\"\nrun = \"{run}\"\n")
            }
        };
        Manifest::parse(&text).expect("manifest parses")
    }

    fn index_with(entries: &[(&str, u32, &str)]) -> Index {
        let runs: Vec<String> = entries
            .iter()
            .map(|(platform, epoch, run)| {
                format!(
                    r#"{{"platform":"{platform}","epoch":{epoch},"finished_at":"2026-08-18T00:00:00Z","run":"{run}"}}"#
                )
            })
            .collect();
        parse_index(&format!(r#"{{"runs":[{}]}}"#, runs.join(","))).expect("index parses")
    }

    #[test]
    fn a_baseline_present_in_its_own_epoch_resolves() {
        let manifest = manifest_with(Some(("c0", "abc")));
        let index = index_with(&[("x86_64-linux", 1, "abc")]);
        assert_eq!(unresolved(&manifest, &index), Vec::new());
    }

    #[test]
    fn an_epoch_without_a_baseline_is_not_reported() {
        // Declaring an epoch before its first complete run is the documented
        // state; `unpinned` holds it to a deadline, not this rule.
        let manifest = manifest_with(None);
        let index = index_with(&[]);
        assert_eq!(unresolved(&manifest, &index), Vec::new());
    }

    #[test]
    fn a_baseline_naming_no_record_is_reported() {
        // The compaction's characteristic mistake: the manifest keeps a
        // pre-compaction address that no longer names anything.
        let manifest = manifest_with(Some(("c0", "gone")));
        let index = index_with(&[("x86_64-linux", 1, "abc")]);
        let missing = unresolved(&manifest, &index);
        assert_eq!(missing.len(), 1);
        assert_eq!(missing[0].run, "gone");
        assert!(
            missing[0].detail.contains("no record in index.json"),
            "unexpected detail: {}",
            missing[0].detail
        );
    }

    #[test]
    fn a_baseline_resolving_under_another_epoch_is_reported() {
        // Existence somewhere in the store is not resolution: `derive` looks
        // the address up among its own epoch's records and finds nothing.
        let manifest = manifest_with(Some(("c0", "abc")));
        let index = index_with(&[("x86_64-linux", 2, "abc")]);
        let missing = unresolved(&manifest, &index);
        assert_eq!(missing.len(), 1);
        assert!(
            missing[0].detail.contains("epoch 2"),
            "unexpected detail: {}",
            missing[0].detail
        );
    }

    #[test]
    fn a_baseline_resolving_under_another_platform_is_reported() {
        let manifest = manifest_with(Some(("c0", "abc")));
        let index = index_with(&[("aarch64-macos", 1, "abc")]);
        let missing = unresolved(&manifest, &index);
        assert_eq!(missing.len(), 1);
        assert!(
            missing[0].detail.contains("aarch64-macos"),
            "unexpected detail: {}",
            missing[0].detail
        );
    }

    #[test]
    fn a_retired_epoch_is_checked_the_same_as_a_live_one() {
        // The whole point: `collection = false` puts an epoch outside
        // `unindexed()`'s reach, and inside this one's.
        let retired = BASE.replace("collection = true", "collection = false");
        let text = format!("{retired}\n[epoch.baseline]\ncommit = \"c0\"\nrun = \"gone\"\n");
        let manifest = Manifest::parse(&text).expect("manifest parses");
        let index = index_with(&[("x86_64-linux", 1, "abc")]);
        assert_eq!(unresolved(&manifest, &index).len(), 1);
    }
}
