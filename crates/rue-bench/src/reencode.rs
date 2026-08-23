//! Re-encode a store tree from the full-evidence encoding to the stored one.
//!
//! The one-time compaction ADR-0067 Amendment 1 Question 2 accepts: every
//! schema-v1 record in `runs/` is rewritten in the v2 witness-plus-digests
//! encoding under its own new content address, `index.json` and the manifest's
//! pinned addresses move with them, and the originals leave the tree. Nothing
//! here touches git: the operator lands the result as a single ordinary append
//! commit, with the pre-compaction tip tagged so every original record keeps a
//! quotable name in history.
//!
//! `schema_version` is part of the canonical form, so **every** re-encoded
//! record moves — evidence-free records included, whose only change is the
//! version field. The manifest re-pin is therefore not optional, and the run
//! ends by asking the `check-baselines` question of the rewritten pair: a
//! wrong baseline address on a retired epoch is exactly the silent failure
//! this tool must not be able to produce.
//!
//! Address rewriting in `index.json` and the manifest is textual: a content
//! address is a 64-hex string unique to its record, so replacing occurrences
//! preserves every comment and formatting choice in files this tool does not
//! own.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use rue_perf_schema::{FULL_EVIDENCE_SCHEMA_VERSION, Manifest, RunObject, Stored, encode_v2};

use crate::check_baselines::unresolved;
use crate::staleness_inputs::parse_index;

pub fn run() -> Result<u8, String> {
    let mut data_root: Option<PathBuf> = None;
    let mut manifest_path: Option<PathBuf> = None;
    let mut apply = false;
    let mut args = std::env::args().skip(2);
    while let Some(flag) = args.next() {
        let mut value = || {
            args.next()
                .ok_or_else(|| format!("{flag} requires a value"))
        };
        match flag.as_str() {
            "--data-root" => data_root = Some(PathBuf::from(value()?)),
            "--manifest" => manifest_path = Some(PathBuf::from(value()?)),
            "--apply" => apply = true,
            other => return Err(format!("unrecognized argument {other:?}")),
        }
    }
    let data_root = data_root.ok_or("reencode requires --data-root <path>")?;
    let manifest_path = manifest_path.ok_or("reencode requires --manifest <path>")?;
    report(&data_root, &manifest_path, apply, &mut std::io::stdout())
}

fn report(
    data_root: &Path,
    manifest_path: &Path,
    apply: bool,
    out: &mut impl std::io::Write,
) -> Result<u8, String> {
    let runs_dir = data_root.join("runs");
    let mut entries: Vec<PathBuf> = std::fs::read_dir(&runs_dir)
        .map_err(|error| format!("could not read {}: {error}", runs_dir.display()))?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<Result<_, _>>()
        .map_err(|error| format!("could not list {}: {error}", runs_dir.display()))?;
    entries.retain(|path| {
        path.extension()
            .is_some_and(|extension| extension == "json")
    });
    entries.sort();

    let mut moves: BTreeMap<String, String> = BTreeMap::new();
    let mut planned: Vec<(PathBuf, String, String)> = Vec::new();
    let mut already_encoded = 0usize;
    let mut bytes_before = 0u64;
    let mut bytes_after = 0u64;

    for path in &entries {
        let text = std::fs::read_to_string(path)
            .map_err(|error| format!("could not read {}: {error}", path.display()))?;
        let stored = Stored::<RunObject>::read(&text)
            .map_err(|error| format!("{}: {error}", path.display()))?;
        bytes_before += text.len() as u64;
        if stored.record().schema_version != FULL_EVIDENCE_SCHEMA_VERSION {
            already_encoded += 1;
            bytes_after += text.len() as u64;
            continue;
        }
        let encoded =
            encode_v2(stored.record()).map_err(|error| format!("{}: {error}", path.display()))?;
        let minted =
            Stored::minted(encoded).map_err(|error| format!("{}: {error}", path.display()))?;
        let serialized = rue_perf_schema::canonical_json(minted.record())
            .map_err(|error| format!("{}: {error}", path.display()))?;
        bytes_after += serialized.len() as u64;
        moves.insert(stored.address().to_string(), minted.address().to_string());
        planned.push((path.clone(), minted.address().to_string(), serialized));
    }

    if apply {
        for (old_path, new_address, serialized) in &planned {
            let new_path = runs_dir.join(format!("{new_address}.json"));
            // The publisher's accident-refusal rule, kept here: differing
            // bytes under an existing name are an error, identical bytes an
            // idempotent no-op.
            match std::fs::read_to_string(&new_path) {
                Ok(existing) if existing == *serialized => {}
                Ok(_) => {
                    return Err(format!(
                        "{} exists with different bytes",
                        new_path.display()
                    ));
                }
                Err(_) => {
                    std::fs::write(&new_path, serialized).map_err(|error| {
                        format!("could not write {}: {error}", new_path.display())
                    })?;
                }
            }
            std::fs::remove_file(old_path)
                .map_err(|error| format!("could not remove {}: {error}", old_path.display()))?;
        }
        let index_path = data_root.join("index.json");
        let index_rewrites = rewrite_addresses(&index_path, &moves)?;
        let manifest_rewrites = rewrite_addresses(manifest_path, &moves)?;
        writeln!(
            out,
            "rewrote {index_rewrites} address(es) in {} and {manifest_rewrites} in {}",
            index_path.display(),
            manifest_path.display()
        )
        .map_err(|error| format!("could not write the report: {error}"))?;

        // The acceptance condition, asked of the result: every declared
        // baseline must resolve against the rewritten index, retired epochs
        // included.
        let manifest_text = std::fs::read_to_string(manifest_path)
            .map_err(|error| format!("could not read {}: {error}", manifest_path.display()))?;
        let manifest = Manifest::parse(&manifest_text).map_err(|error| error.to_string())?;
        let index_text = std::fs::read_to_string(&index_path)
            .map_err(|error| format!("could not read {}: {error}", index_path.display()))?;
        let index = parse_index(&index_text)?;
        let missing = unresolved(&manifest, &index);
        if !missing.is_empty() {
            for item in &missing {
                writeln!(
                    out,
                    "{} epoch {}: baseline {} does not resolve — {}",
                    item.platform, item.epoch, item.run, item.detail
                )
                .map_err(|error| format!("could not write the report: {error}"))?;
            }
            return Ok(crate::exit::NOT_APPENDABLE);
        }
    }

    writeln!(
        out,
        "{} record(s) re-encoded{}, {} already encoded; {} -> {} bytes",
        planned.len(),
        if apply {
            ""
        } else {
            " (dry run; pass --apply to write)"
        },
        already_encoded,
        bytes_before,
        bytes_after
    )
    .map_err(|error| format!("could not write the report: {error}"))?;
    for (old, new) in &moves {
        writeln!(out, "{old} {new}")
            .map_err(|error| format!("could not write the report: {error}"))?;
    }
    Ok(crate::exit::OK)
}

/// Replace every mapped address in a text file, preserving everything else.
fn rewrite_addresses(path: &Path, moves: &BTreeMap<String, String>) -> Result<usize, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|error| format!("could not read {}: {error}", path.display()))?;
    let mut rewritten = text.clone();
    let mut count = 0usize;
    for (old, new) in moves {
        let occurrences = rewritten.matches(old.as_str()).count();
        if occurrences > 0 {
            rewritten = rewritten.replace(old.as_str(), new.as_str());
            count += occurrences;
        }
    }
    if rewritten != text {
        std::fs::write(path, &rewritten)
            .map_err(|error| format!("could not write {}: {error}", path.display()))?;
    }
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rue_perf_schema::RUN_SCHEMA_VERSION;

    // The derive fixtures build evidence-free protocol-1 records; the
    // evidence-bearing encode path is covered exhaustively in
    // rue_perf_schema::encoding. This module tests the orchestration: files
    // move, the index and manifest follow, the gate runs.
    fn store_with(records: &[&RunObject]) -> tempfile::TempDir {
        let directory = tempfile::tempdir().expect("temp dir");
        std::fs::create_dir(directory.path().join("runs")).expect("runs dir");
        let mut index_rows = Vec::new();
        for record in records {
            let stored = Stored::minted((*record).clone()).expect("addressable");
            let serialized =
                rue_perf_schema::canonical_json(stored.record()).expect("serializable");
            std::fs::write(
                directory
                    .path()
                    .join("runs")
                    .join(format!("{}.json", stored.address())),
                serialized,
            )
            .expect("write record");
            index_rows.push(format!(
                r#"{{"platform":"{}","epoch":{},"finished_at":"{}","run":"{}"}}"#,
                record.identity.platform,
                record.identity.epoch,
                record.identity.finished_at,
                stored.address()
            ));
        }
        std::fs::write(
            directory.path().join("index.json"),
            format!(r#"{{"runs":[{}]}}"#, index_rows.join(",")),
        )
        .expect("write index");
        directory
    }

    fn fixture_record() -> RunObject {
        use rue_perf_schema::{
            EnvironmentFingerprint, Invocation, Phase, PhaseAccounting, ResolvedPins, RunIdentity,
            Sample, WorkloadObservation,
        };
        use std::collections::BTreeMap;
        let mut phase_ns: BTreeMap<Phase, u64> =
            Phase::ALL.into_iter().map(|phase| (phase, 0)).collect();
        phase_ns.insert(Phase::SemanticAnalysis, 1_000_000);
        RunObject {
            schema_version: FULL_EVIDENCE_SCHEMA_VERSION,
            identity: RunIdentity {
                suite_revision: 1,
                epoch: 1,
                platform: "x86_64-linux".to_string(),
                commit: "a".repeat(40),
                started_at: "2026-08-20T00:00:00Z".to_string(),
                finished_at: "2026-08-20T00:01:00Z".to_string(),
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
                    runner_image_version: "20260820.1".to_string(),
                    cpu_model: "AMD EPYC 7763".to_string(),
                    core_count: 4,
                    memory_bytes: 16 * 1024 * 1024 * 1024,
                    kernel_version: "6.8.0".to_string(),
                    os_version: "Ubuntu 24.04".to_string(),
                    architecture: "x86_64".to_string(),
                },
            },
            boundary: None,
            full_evidence: None,
            workloads: vec![WorkloadObservation {
                workload: "startup".to_string(),
                boundary: None,
                samples: vec![Sample {
                    batch_size: 1,
                    process_elapsed_ns: 2_000_000,
                    peak_memory_bytes: 32 * 1024 * 1024,
                    output_binary_bytes: 12_288,
                    phases: PhaseAccounting {
                        phase_ns,
                        mixed_parallel_ns: 0,
                        unattributed_ns: 0,
                        compiler_root_ns: 1_000_000,
                    },
                    boundary_evidence: Vec::new(),
                    boundary_processes: Vec::new(),
                    boundary_work_processes: Vec::new(),
                }],
            }],
            failures: Vec::new(),
        }
    }

    fn manifest_pinned_to(address: &str) -> String {
        format!(
            "{}\n[epoch.baseline]\ncommit = \"{}\"\nrun = \"{address}\"\n",
            crate::check_baselines::tests::BASE,
            "a".repeat(40)
        )
    }

    #[test]
    fn a_dry_run_moves_nothing_and_prints_the_map() {
        let record = fixture_record();
        let store = store_with(&[&record]);
        let old_address = Stored::minted(record.clone())
            .unwrap()
            .address()
            .to_string();
        let manifest_file = store.path().join("manifest.toml");
        std::fs::write(&manifest_file, manifest_pinned_to(&old_address)).unwrap();

        let mut output = Vec::new();
        let code = report(store.path(), &manifest_file, false, &mut output).unwrap();
        assert_eq!(code, crate::exit::OK);
        let output = String::from_utf8(output).unwrap();
        assert!(
            output.contains("1 record(s) re-encoded (dry run"),
            "{output}"
        );
        assert!(output.contains(&old_address), "{output}");
        // Nothing moved: the original file is still the only record.
        assert!(
            store
                .path()
                .join("runs")
                .join(format!("{old_address}.json"))
                .exists()
        );
    }

    #[test]
    fn applying_moves_records_and_repins_the_manifest_and_index() {
        let record = fixture_record();
        let store = store_with(&[&record]);
        let old_address = Stored::minted(record.clone())
            .unwrap()
            .address()
            .to_string();
        let manifest_file = store.path().join("manifest.toml");
        std::fs::write(&manifest_file, manifest_pinned_to(&old_address)).unwrap();

        let encoded = encode_v2(&record).unwrap();
        assert_eq!(encoded.schema_version, RUN_SCHEMA_VERSION);
        assert_eq!(
            encoded.full_evidence.as_deref(),
            Some(old_address.as_str()),
            "a re-encoded record names its pre-compaction original"
        );
        let new_address = Stored::minted(encoded).unwrap().address().to_string();
        assert_ne!(old_address, new_address, "the version field is addressed");

        let mut output = Vec::new();
        let code = report(store.path(), &manifest_file, true, &mut output).unwrap();
        assert_eq!(
            code,
            crate::exit::OK,
            "{}",
            String::from_utf8(output).unwrap()
        );

        let runs = store.path().join("runs");
        assert!(!runs.join(format!("{old_address}.json")).exists());
        assert!(runs.join(format!("{new_address}.json")).exists());
        let index = std::fs::read_to_string(store.path().join("index.json")).unwrap();
        assert!(index.contains(&new_address));
        assert!(!index.contains(&old_address));
        let manifest = std::fs::read_to_string(&manifest_file).unwrap();
        assert!(manifest.contains(&new_address));
        assert!(!manifest.contains(&old_address));
    }

    #[test]
    fn a_second_application_is_an_idempotent_no_op() {
        let record = fixture_record();
        let store = store_with(&[&record]);
        let manifest_file = store.path().join("manifest.toml");
        let old_address = Stored::minted(record.clone())
            .unwrap()
            .address()
            .to_string();
        std::fs::write(&manifest_file, manifest_pinned_to(&old_address)).unwrap();

        let mut output = Vec::new();
        report(store.path(), &manifest_file, true, &mut output).unwrap();
        let manifest_after_first = std::fs::read_to_string(&manifest_file).unwrap();

        let mut output = Vec::new();
        let code = report(store.path(), &manifest_file, true, &mut output).unwrap();
        assert_eq!(code, crate::exit::OK);
        let output = String::from_utf8(output).unwrap();
        assert!(
            output.contains("0 record(s) re-encoded, 1 already encoded"),
            "{output}"
        );
        assert_eq!(
            std::fs::read_to_string(&manifest_file).unwrap(),
            manifest_after_first
        );
    }

    #[test]
    fn a_baseline_the_rewrite_cannot_reach_fails_the_gate() {
        // A pin naming an address outside the store: the map cannot move it,
        // and the closing check-baselines question must fail loudly rather
        // than let the operator commit a silently broken manifest.
        let record = fixture_record();
        let store = store_with(&[&record]);
        let manifest_file = store.path().join("manifest.toml");
        std::fs::write(&manifest_file, manifest_pinned_to(&"f".repeat(64))).unwrap();

        let mut output = Vec::new();
        let code = report(store.path(), &manifest_file, true, &mut output).unwrap();
        assert_eq!(code, crate::exit::NOT_APPENDABLE);
        let output = String::from_utf8(output).unwrap();
        assert!(output.contains("does not resolve"), "{output}");
    }
}
