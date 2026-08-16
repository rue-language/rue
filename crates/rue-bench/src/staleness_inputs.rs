//! Name the records the staleness gate reads, so it can stop reading the rest.
//!
//! `performance-data-v1` is append-only and unbounded — 1.5 GB across 1,188
//! records on 2026-08-16, growing by roughly 300 MB every day since protocol v2
//! began retaining per-sample boundary evidence. The staleness gate
//! materialized and parsed all of it on every pull request, which is work
//! proportional to the whole history of the project for a question about its
//! present (RUE-1542).
//!
//! The gate reads far less than that. Every rule it applies —
//! `newest_plotted`, `unindexed`, `unpinned` — is a property of the epoch
//! holding each platform's newest point, and nothing else. Older epochs are
//! retired: they keep whatever they published, cannot receive another point,
//! and cannot become stale.
//!
//! So the selection is exactly that: per platform, the epoch its newest point
//! belongs to, and every record in it. Two properties matter and are tested.
//!
//! It is the epoch of the platform's newest point rather than the manifest's
//! collecting epoch. Those are usually the same, and when they are not, the
//! difference is a platform whose collection has stopped — the one case the
//! gate exists to catch. Selecting by manifest would drop such a platform out
//! of the derived data entirely, and a platform with no points at all is not
//! stalled by `newest_plotted`'s reckoning: it is absent, which reads as fine.
//!
//! And the baseline comes along for free. A ratio is measured against the
//! epoch's own baseline run, which is a record of that epoch, so selecting the
//! epoch selects whatever the manifest pins — no separate lookup, and no way
//! for the gate to lose an index it would otherwise have published.
//!
//! The rule reads `index.json`, which already carries each record's platform,
//! epoch, commit, and finish time. That is what makes this cheap: the
//! selection never opens a run object to decide whether to read it.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

/// One `index.json` entry, as `scripts/publish-performance-runs.py` writes it.
///
/// Deliberately a subset. This names the fields the selection needs and lets
/// serde ignore the rest, so a later index field cannot fail the gate.
#[derive(Debug, Deserialize)]
struct IndexEntry {
    platform: String,
    epoch: u32,
    finished_at: String,
    run: String,
}

#[derive(Debug, Deserialize)]
struct Index {
    /// Compiler runs (ADR-0067). The `runtime` list is deliberately not read:
    /// ADR-0072 Decision 9 keeps the runtime series out of this gate.
    #[serde(default)]
    runs: Vec<IndexEntry>,
}

/// Return the data-branch paths the staleness gate needs, in a stable order.
///
/// An empty index yields no paths rather than an error: a data branch with
/// nothing on it is the honest first state of a suite that has not collected,
/// and the gate reports that as "nothing to stall" rather than failing.
pub fn select(index_json: &str) -> Result<Vec<String>, String> {
    let index: Index = serde_json::from_str(index_json)
        .map_err(|error| format!("could not parse index.json: {error}"))?;

    // The newest entry per platform, and therefore the epoch to keep. Ties on
    // `finished_at` break on the content address so the choice is a function of
    // the data rather than of directory order — two runs finishing inside the
    // same second is ordinary on a fast platform.
    let mut newest: BTreeMap<&str, &IndexEntry> = BTreeMap::new();
    for entry in &index.runs {
        newest
            .entry(entry.platform.as_str())
            .and_modify(|current| {
                if (&entry.finished_at, &entry.run) > (&current.finished_at, &current.run) {
                    *current = entry;
                }
            })
            .or_insert(entry);
    }

    let mut paths: Vec<String> = index
        .runs
        .iter()
        .filter(|entry| {
            newest
                .get(entry.platform.as_str())
                .is_some_and(|live| live.epoch == entry.epoch)
        })
        .map(|entry| format!("runs/{}.json", entry.run))
        .collect();
    paths.sort();
    paths.dedup();
    Ok(paths)
}

pub fn run() -> Result<(), String> {
    let mut index_path: Option<PathBuf> = None;
    let mut args = std::env::args().skip(2);
    while let Some(flag) = args.next() {
        let mut value = || {
            args.next()
                .ok_or_else(|| format!("{flag} requires a value"))
        };
        match flag.as_str() {
            "--index" => index_path = Some(PathBuf::from(value()?)),
            other => return Err(format!("unrecognized argument {other:?}")),
        }
    }
    let index_path = index_path.ok_or("staleness-inputs requires --index <path>")?;
    print(&index_path, &mut std::io::stdout())
}

fn print(index_path: &Path, out: &mut impl std::io::Write) -> Result<(), String> {
    // A missing index is the empty branch, not a failure: `git checkout` of a
    // path that does not exist would have failed before reaching here, so this
    // is the first-collection case rather than a lost file.
    let text = match std::fs::read_to_string(index_path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(format!("could not read {}: {error}", index_path.display()));
        }
    };
    for path in select(&text)? {
        writeln!(out, "{path}")
            .map_err(|error| format!("could not write the selection: {error}"))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn index(entries: &[(&str, u32, &str, &str)]) -> String {
        let runs: Vec<String> = entries
            .iter()
            .map(|(platform, epoch, finished_at, run)| {
                format!(
                    "{{\"commit\":\"{}\",\"epoch\":{epoch},\"finished_at\":\"{finished_at}\",\
                     \"platform\":\"{platform}\",\"run\":\"{run}\",\"suite_revision\":4}}",
                    "a".repeat(40)
                )
            })
            .collect();
        format!(
            "{{\"runs\":[{}],\"runtime\":[],\"schema_version\":2}}",
            runs.join(",")
        )
    }

    #[test]
    fn only_the_epoch_holding_the_newest_point_is_selected() {
        let text = index(&[
            ("x86_64-linux", 5, "2026-08-14T00:00:00Z", "old"),
            ("x86_64-linux", 6, "2026-08-15T00:00:00Z", "new"),
            ("x86_64-linux", 6, "2026-08-15T01:00:00Z", "newer"),
        ]);
        assert_eq!(
            select(&text).unwrap(),
            vec!["runs/new.json".to_string(), "runs/newer.json".to_string()]
        );
    }

    #[test]
    fn every_platform_keeps_its_own_live_epoch() {
        // Platforms advance independently: one may already be collecting a new
        // epoch while another is still on the previous one.
        let text = index(&[
            ("x86_64-linux", 6, "2026-08-15T00:00:00Z", "x6"),
            ("aarch64-macos", 5, "2026-08-15T00:00:00Z", "m5"),
            ("aarch64-macos", 4, "2026-08-01T00:00:00Z", "m4"),
        ]);
        assert_eq!(
            select(&text).unwrap(),
            vec!["runs/m5.json".to_string(), "runs/x6.json".to_string()]
        );
    }

    #[test]
    fn a_platform_whose_collection_stopped_keeps_the_epoch_it_stopped_in() {
        // The case that makes this "newest point" rather than "collecting
        // epoch": x86 has moved on, macOS stopped in epoch 5. Selecting by the
        // manifest's collecting epoch would drop macOS entirely, and a platform
        // with no points is not stalled — it is invisible.
        let text = index(&[
            ("x86_64-linux", 6, "2026-08-15T00:00:00Z", "x6"),
            ("aarch64-macos", 5, "2026-07-01T00:00:00Z", "m5"),
        ]);
        assert_eq!(
            select(&text).unwrap(),
            vec!["runs/m5.json".to_string(), "runs/x6.json".to_string()]
        );
    }

    #[test]
    fn ties_on_the_finish_time_are_broken_by_the_address() {
        // Two runs finishing in the same second must not make the selection
        // depend on index order; both are in the same epoch here, so the
        // selection is the same either way, and that is the point.
        let text = index(&[
            ("x86_64-linux", 6, "2026-08-15T00:00:00Z", "b"),
            ("x86_64-linux", 6, "2026-08-15T00:00:00Z", "a"),
            ("x86_64-linux", 5, "2026-08-14T00:00:00Z", "old"),
        ]);
        assert_eq!(
            select(&text).unwrap(),
            vec!["runs/a.json".to_string(), "runs/b.json".to_string()]
        );
    }

    #[test]
    fn an_empty_branch_selects_nothing() {
        assert!(
            select("{\"runs\":[],\"runtime\":[],\"schema_version\":2}")
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn an_index_field_this_does_not_know_is_not_an_error() {
        // The index is written by a script that may gain fields. A gate that
        // failed on one would turn an additive change into a repository-wide
        // outage.
        let text = "{\"runs\":[{\"commit\":\"c\",\"epoch\":6,\"finished_at\":\"2026-08-15T00:00:00Z\",\
                    \"platform\":\"x86_64-linux\",\"run\":\"r\",\"suite_revision\":4,\"future\":1}],\
                    \"schema_version\":99}";
        assert_eq!(select(text).unwrap(), vec!["runs/r.json".to_string()]);
    }

    #[test]
    fn a_missing_index_is_the_empty_branch() {
        let mut out = Vec::new();
        print(Path::new("/nonexistent/index.json"), &mut out).expect("not an error");
        assert!(out.is_empty());
    }
}
