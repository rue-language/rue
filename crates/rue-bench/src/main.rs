//! The ADR-0067 compiler-performance runner.
//!
//! One binary reads the manifest, runs the corpus under the epoch's sampling
//! policy, validates what it measured, and emits one immutable run object. It
//! runs identically locally and in CI, so any recorded run is rerunnable under
//! the protocol it recorded. Rerunnable, not reproducible: hosted-hardware
//! timings do not repeat byte for byte.
//!
//! The runner never decides whether a measurement is good. It records what
//! happened — including crashes, unresolvable pins, and phase-sum violations —
//! and hands the result to `rue_perf_schema::validate_run`. Keeping judgement
//! out of the producer is what lets validation catch a producer that is itself
//! wrong.
//!
//! The `runtime` subcommand answers ADR-0072's question instead: not how fast
//! Rue compiles a program, but how fast the compiled program runs. It follows
//! the same separation — record what happened, let `rue_perf_schema` decide
//! appendability — over its own manifest and record kind.

mod calibrate;
mod check_baselines;
mod check_pins;
mod derive;
mod digest;
mod environment;
mod fixture;
mod incremental;
mod measure;
mod pins;
mod reencode;
mod runtime;
mod scaling;
mod staleness_inputs;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

use rue_perf_schema::{
    Completeness, FULL_EVIDENCE_SCHEMA_VERSION, FailureRecord, Invocation, Manifest,
    ProcessElapsedRegression, ResolvedPins, RunIdentity, RunObject, Sample, ValidationOutcome,
    WorkloadObservation, encode_stored_v3, process_elapsed_regressions, validate_run,
};

use crate::measure::{SampleOutcome, SampleRequest, measure_sample};

/// Exit codes, so a workflow can tell "collection broke" from "this run may not
/// be appended".
mod exit {
    /// The run object was written and is appendable.
    pub const OK: u8 = 0;
    /// The runner could not be configured or could not write its output.
    pub const USAGE: u8 = 2;
    /// A run object was written, but validation refuses it for its series.
    ///
    /// Distinct from `USAGE` because the evidence still exists and is worth
    /// collecting; only publication is blocked.
    pub const NOT_APPENDABLE: u8 = 3;
    /// The run is valid and publishable, but crossed a reviewed performance
    /// ratchet. The raw evidence still belongs in its series.
    pub const REGRESSION: u8 = 4;
    /// The run is appendable, but a workload did not complete validly.
    ///
    /// Distinct from every code above: nothing is wrong with the run object,
    /// and the evidence belongs in the store. What is wrong is the series,
    /// which now has a hole where a point should be, and in the all-workloads
    /// case has no new point at all (RUE-1514).
    ///
    /// It exists because ADR-0067 §7's dashboard warning was not enough on its
    /// own: an incomplete collection exited 0, the workflow went green, and the
    /// repository-wide stall gate spoke up days later at commits that did not
    /// cause it. `rue-bench runtime` already answers this question this way;
    /// the two runners in one binary should not disagree about it.
    pub const INCOMPLETE: u8 = 5;
}

struct Options {
    manifest: PathBuf,
    platform: String,
    epoch: Option<u32>,
    compiler: PathBuf,
    commit: String,
    repo_root: PathBuf,
    std_root: Option<PathBuf>,
    output: PathBuf,
    workdir: Option<PathBuf>,
}

const USAGE: &str = "\
rue-bench — ADR-0067 compiler-performance runner

Required:
  --manifest <path>    performance manifest declaring suites and epochs
  --platform <name>    platform whose epoch to run, e.g. x86_64-linux
  --epoch <n>          epoch identifier within that platform
                       (default: the manifest's collection epoch)
  --compiler <path>    compiler binary under measurement
  --commit <sha>       40-character compiler revision being measured
  --out <path>         where to write the run object

Subcommands:
  derive --manifest <path> [--runtime-manifest <path>] --data-root <dir>
         --out <path>
                       rebuild the dashboard's view from raw records. Reads the
                       performance-data-v1 checkout; writes derived JSON.
                       --runtime-manifest additionally derives the ADR-0072
                       runtime record kind from the same store.

  staleness-inputs --index <path>
                       print the data-branch paths the staleness gate reads:
                       every record of the epoch holding each platform's newest
                       point. Lets that gate check out and parse the live epoch
                       instead of the whole append-only history.

  check-baselines --manifest <path> --index <path>
                       answer whether every declared `[epoch.baseline] run`
                       still names a record of its own epoch, reading only the
                       manifest and index.json. Covers retired epochs, which
                       `staleness-inputs` deliberately keeps out of the derived
                       data. Exits 3 when one does not resolve.

  reencode --data-root <dir> --manifest <path> [--apply]
                       rewrite every full-evidence record in
                       <dir>/runs into the v2 witness-plus-digests encoding
                       under new content addresses, moving index.json and the
                       manifest pins with them (ADR-0067 Amendment 1). Dry run
                       by default; --apply writes, then requires every declared
                       baseline to resolve, exiting 3 when one does not.

  check-pins --manifest <path> --compiler <path> [--repo-root <path>]
                       answer whether this tree still matches every collection
                       epoch. Exits 3 on drift, which is the change stalling
                       the series; nothing is measured.

  calibrate --runs <dir> [--report <path>]
                       analyse repeated runs and recommend sampling counts,
                       batching factors, and the flagging rule. Produces
                       artifacts only; nothing here enters a series.

  calibrate --runtime-reports <dir> [--report <path>]
                       the same analysis for the ADR-0072 runtime suite, over
                       stored runtime reports: per workload and per platform
                       epoch, the dispersion of an unchanged program over an
                       unchanged corpus, and a flagging recommendation. Reads
                       a workflow's freshly written reports or a checkout of
                       performance-data-v1/runtime. Artifacts only.

  scaling --manifest <path> --compiler <path> --commit <sha> --out <path>
                       measure maintained programs with independent fresh
                       compiler processes; also writes a Markdown report.

  runtime --platform <name> --compiler <path> --commit <sha> --out <path>
                       measure how fast compiled Rue programs run (ADR-0072).
                       Builds each workload at release quality, prepares its
                       fixture outside the timed window, measures N fresh
                       processes spawn-to-exit, and judges the output against a
                       committed golden. Defaults to performance/runtime.toml;
                       override with --manifest.

  incremental --commit <sha> --out <path>
                       measure retained-session edits of Mosaic and Lattice,
                       including the bounded 1,000-revision retention witness;
                       writes raw JSON and same-stem derived Markdown.
                       Defaults to performance/incremental.toml and
                       performance/incremental-fixtures.toml.

Optional:
  --repo-root <path>   root for resolving workload sources (default: .)
  --std-root <path>    standard-library root (default: <repo-root>/std)
  --workdir <path>     directory for compiled outputs (default: a temp dir)
";

fn main() -> ExitCode {
    // `calibrate` reads runs that already exist; every other invocation
    // produces one. Keeping them one binary means the calibration analysis and
    // the collector can never disagree about what a run object means.
    if std::env::args().nth(1).as_deref() == Some("derive") {
        return match run_derive() {
            Ok(()) => ExitCode::from(exit::OK),
            Err(message) => {
                eprintln!("rue-bench: {message}");
                ExitCode::from(exit::USAGE)
            }
        };
    }
    if std::env::args().nth(1).as_deref() == Some("calibrate") {
        return match run_calibration() {
            Ok(()) => ExitCode::from(exit::OK),
            Err(message) => {
                eprintln!("rue-bench: {message}");
                ExitCode::from(exit::USAGE)
            }
        };
    }
    if std::env::args().nth(1).as_deref() == Some("scaling") {
        return match scaling::run() {
            Ok(()) => ExitCode::from(exit::OK),
            Err(message) => {
                eprintln!("rue-bench: {message}");
                ExitCode::from(exit::USAGE)
            }
        };
    }
    if std::env::args().nth(1).as_deref() == Some("runtime") {
        return match runtime::run() {
            Ok(status) => ExitCode::from(status),
            Err(message) => {
                eprintln!("rue-bench runtime: {message}");
                ExitCode::from(runtime::exit::USAGE)
            }
        };
    }
    if std::env::args().nth(1).as_deref() == Some("staleness-inputs") {
        return match staleness_inputs::run() {
            Ok(()) => ExitCode::from(exit::OK),
            Err(message) => {
                eprintln!("rue-bench staleness-inputs: {message}");
                ExitCode::from(exit::USAGE)
            }
        };
    }
    if std::env::args().nth(1).as_deref() == Some("check-baselines") {
        return match check_baselines::run() {
            Ok(status) => ExitCode::from(status),
            Err(message) => {
                eprintln!("rue-bench check-baselines: {message}");
                ExitCode::from(exit::USAGE)
            }
        };
    }
    if std::env::args().nth(1).as_deref() == Some("reencode") {
        return match reencode::run() {
            Ok(status) => ExitCode::from(status),
            Err(message) => {
                eprintln!("rue-bench reencode: {message}");
                ExitCode::from(exit::USAGE)
            }
        };
    }
    if std::env::args().nth(1).as_deref() == Some("check-pins") {
        return match check_pins::run() {
            Ok(()) => ExitCode::from(exit::OK),
            Err(message) => {
                eprintln!("rue-bench check-pins: {message}");
                ExitCode::from(exit::NOT_APPENDABLE)
            }
        };
    }
    if std::env::args().nth(1).as_deref() == Some("incremental") {
        return match incremental::run() {
            Ok(status) => ExitCode::from(status.exit_code()),
            Err(message) => {
                eprintln!("rue-bench: {message}");
                ExitCode::from(exit::USAGE)
            }
        };
    }
    let options = match parse_args() {
        Ok(options) => options,
        Err(message) => {
            eprintln!("rue-bench: {message}\n\n{USAGE}");
            return ExitCode::from(exit::USAGE);
        }
    };
    match run(&options) {
        Ok(code) => ExitCode::from(code),
        Err(message) => {
            eprintln!("rue-bench: {message}");
            ExitCode::from(exit::USAGE)
        }
    }
}

/// Analyse a directory of calibration runs.
///
/// Nothing this produces may enter a series. The report is a workflow artifact
/// and a recommendation for a maintainer, not a measurement.
fn run_calibration() -> Result<(), String> {
    let mut runs_dir = None;
    let mut runtime_dir = None;
    let mut report_path = None;
    let mut args = std::env::args().skip(2);
    while let Some(flag) = args.next() {
        let mut value = || {
            args.next()
                .ok_or_else(|| format!("{flag} requires a value"))
        };
        match flag.as_str() {
            "--runs" => runs_dir = Some(PathBuf::from(value()?)),
            "--runtime-reports" => runtime_dir = Some(PathBuf::from(value()?)),
            "--report" => report_path = Some(PathBuf::from(value()?)),
            other => return Err(format!("unrecognized argument {other:?}")),
        }
    }

    // One record kind per invocation. The two analyses answer the same
    // questions about different measurements, and a single report holding both
    // would invite reading one suite's dispersion as the other's — which is
    // exactly the transfer ADR-0072 Decision 5 forbids.
    let (rendered, encoded) = match (runs_dir, runtime_dir) {
        (Some(_), Some(_)) => {
            return Err(
                "calibrate takes --runs or --runtime-reports, not both: they are different \
                 record kinds and their dispersions do not transfer"
                    .to_string(),
            );
        }
        (Some(runs_dir), None) => {
            let runs = calibrate::load_runs(&runs_dir).map_err(|error| error.to_string())?;
            let report = calibrate::calibrate(&runs).map_err(|error| error.to_string())?;
            (
                calibrate::render(&report),
                serde_json::to_string_pretty(&report),
            )
        }
        (None, Some(runtime_dir)) => {
            let reports =
                calibrate::load_runtime_reports(&runtime_dir).map_err(|error| error.to_string())?;
            let analyses =
                calibrate::calibrate_runtime(&reports).map_err(|error| error.to_string())?;
            (
                calibrate::render_runtime(&analyses),
                serde_json::to_string_pretty(&analyses),
            )
        }
        (None, None) => {
            return Err(
                "calibrate requires --runs <directory> or --runtime-reports <directory>"
                    .to_string(),
            );
        }
    };

    if let Some(path) = report_path {
        std::fs::write(&path, &rendered)
            .map_err(|error| format!("could not write {}: {error}", path.display()))?;
        let json = path.with_extension("json");
        let encoded =
            encoded.map_err(|error| format!("could not serialize the report: {error}"))?;
        std::fs::write(&json, encoded)
            .map_err(|error| format!("could not write {}: {error}", json.display()))?;
        eprintln!("rue-bench: wrote {} and {}", path.display(), json.display());
    }
    println!("{rendered}");
    Ok(())
}

/// Rebuild everything the dashboard renders from the raw records.
///
/// Deliberately a subcommand of the runner rather than a step in the site
/// build: it goes through the same `rue_perf_schema::stats` the calibrator
/// uses, so the page and the calibration can never disagree about what moved.
fn run_derive() -> Result<(), String> {
    let mut manifest_path = None;
    let mut runtime_manifest_path = None;
    let mut data_root = None;
    let mut out = None;
    let mut args = std::env::args().skip(2);
    while let Some(flag) = args.next() {
        let mut value = || {
            args.next()
                .ok_or_else(|| format!("{flag} requires a value"))
        };
        match flag.as_str() {
            "--manifest" => manifest_path = Some(PathBuf::from(value()?)),
            "--runtime-manifest" => runtime_manifest_path = Some(PathBuf::from(value()?)),
            "--data-root" => data_root = Some(PathBuf::from(value()?)),
            "--out" => out = Some(PathBuf::from(value()?)),
            other => return Err(format!("unrecognized argument {other:?}")),
        }
    }
    let manifest_path = manifest_path.ok_or("derive requires --manifest <path>")?;
    let data_root = data_root.ok_or("derive requires --data-root <directory>")?;
    let out = out.ok_or("derive requires --out <path>")?;

    let text = std::fs::read_to_string(&manifest_path)
        .map_err(|error| format!("could not read {}: {error}", manifest_path.display()))?;
    let manifest = Manifest::parse(&text).map_err(|error| error.to_string())?;

    let runs = derive::load_data_branch(&data_root)?;
    let mut data = derive::derive(&manifest, &runs);

    // The ADR-0072 runtime record kind, derived from the same store by the same
    // step. Opt-in rather than implicit: a caller that does not name a runtime
    // manifest gets no runtime section at all, so a page can tell "not asked
    // for" from "asked for and nothing collected yet".
    if let Some(path) = runtime_manifest_path {
        let text = std::fs::read_to_string(&path)
            .map_err(|error| format!("could not read {}: {error}", path.display()))?;
        let runtime_manifest = rue_perf_schema::RuntimeManifest::parse(&text)?;
        let (reports, unreadable) = derive::load_runtime_records(&data_root)?;
        let runtime = derive::derive_runtime(&runtime_manifest, &reports, &unreadable);
        let points: usize = runtime
            .platforms
            .iter()
            .flat_map(|platform| platform.epochs.iter())
            .map(|epoch| epoch.points.len())
            .sum();
        eprintln!(
            "rue-bench: derived {points} runtime point(s) across {} platform(s); \
             {} report(s) rejected",
            runtime.platforms.len(),
            runtime.rejected.len()
        );
        data.runtime = Some(runtime);
    }

    let encoded = serde_json::to_string_pretty(&data)
        .map_err(|error| format!("could not serialize the site data: {error}"))?;
    if let Some(parent) = out.parent().filter(|path| !path.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("could not create {}: {error}", parent.display()))?;
    }
    std::fs::write(&out, encoded)
        .map_err(|error| format!("could not write {}: {error}", out.display()))?;

    let points: usize = data
        .platforms
        .iter()
        .flat_map(|platform| platform.epochs.iter())
        .map(|epoch| epoch.points.len())
        .sum();
    eprintln!(
        "rue-bench: derived {} point(s) across {} platform(s) into {}; {} run(s) rejected",
        points,
        data.platforms.len(),
        out.display(),
        data.rejected.len()
    );
    Ok(())
}

fn parse_args() -> Result<Options, String> {
    let mut manifest = None;
    let mut platform = None;
    let mut epoch = None;
    let mut compiler = None;
    let mut commit = None;
    let mut output = None;
    let mut repo_root = None;
    let mut std_root = None;
    let mut workdir = None;

    let mut args = std::env::args().skip(1);
    while let Some(flag) = args.next() {
        let mut value = || {
            args.next()
                .ok_or_else(|| format!("{flag} requires a value"))
        };
        match flag.as_str() {
            "--manifest" => manifest = Some(PathBuf::from(value()?)),
            "--platform" => platform = Some(value()?),
            "--epoch" => {
                let raw = value()?;
                epoch = Some(
                    raw.parse::<u32>()
                        .map_err(|_| format!("--epoch expects a number, got {raw:?}"))?,
                );
            }
            "--compiler" => compiler = Some(PathBuf::from(value()?)),
            "--commit" => commit = Some(value()?),
            "--out" => output = Some(PathBuf::from(value()?)),
            "--repo-root" => repo_root = Some(PathBuf::from(value()?)),
            "--std-root" => std_root = Some(PathBuf::from(value()?)),
            "--workdir" => workdir = Some(PathBuf::from(value()?)),
            "-h" | "--help" => {
                println!("{USAGE}");
                std::process::exit(0);
            }
            other => return Err(format!("unrecognized argument {other:?}")),
        }
    }

    let repo_root = repo_root.unwrap_or_else(|| PathBuf::from("."));
    let std_root = std_root.or_else(|| {
        let candidate = repo_root.join("std");
        candidate.is_dir().then_some(candidate)
    });

    Ok(Options {
        manifest: manifest.ok_or("--manifest is required")?,
        platform: platform.ok_or("--platform is required")?,
        epoch,
        compiler: compiler.ok_or("--compiler is required")?,
        commit: commit.ok_or("--commit is required")?,
        output: output.ok_or("--out is required")?,
        repo_root,
        std_root,
        workdir,
    })
}

fn run(options: &Options) -> Result<u8, String> {
    let text = std::fs::read_to_string(&options.manifest)
        .map_err(|error| format!("could not read {}: {error}", options.manifest.display()))?;
    let manifest = Manifest::parse(&text).map_err(|error| error.to_string())?;

    // Without `--epoch`, the manifest's collection epoch for this platform is
    // measured. Scheduled collection relies on that: a workflow naming an epoch
    // number would have to be edited alongside the manifest whenever the next
    // epoch is declared, and the two would eventually disagree about which
    // epoch is being collected.
    let epoch = match options.epoch {
        Some(id) => manifest.epoch(&options.platform, id).ok_or_else(|| {
            format!(
                "the manifest declares no epoch {id} for platform {}",
                options.platform
            )
        })?,
        None => manifest
            .collection_epoch(&options.platform)
            .ok_or_else(|| {
                format!(
                    "the manifest marks no collection epoch for platform {}; \
                     pass --epoch to measure a specific one",
                    options.platform
                )
            })?,
    };
    let suite = manifest
        .suite(epoch.suite_revision)
        .ok_or_else(|| format!("suite revision {} is not declared", epoch.suite_revision))?;

    let holder;
    let workdir = match &options.workdir {
        Some(directory) => {
            std::fs::create_dir_all(directory)
                .map_err(|error| format!("could not create {}: {error}", directory.display()))?;
            directory.as_path()
        }
        None => {
            holder = tempfile::tempdir()
                .map_err(|error| format!("could not create a work directory: {error}"))?;
            holder.path()
        }
    };

    let started_at = utc_timestamp();
    let mut failures: Vec<FailureRecord> = Vec::new();

    // Pins are resolved from the tree, never read back from the epoch, so that
    // validation compares two independently produced answers.
    let mut workload_source_hashes = BTreeMap::new();
    for workload in &suite.workloads {
        let source = options.repo_root.join(&workload.source);
        match pins::workload_source_hash(&options.compiler, &source, options.std_root.as_deref()) {
            Ok(hash) => {
                workload_source_hashes.insert(workload.id.clone(), hash);
            }
            Err(error) => {
                // Recorded, not fatal. The run proceeds and validation reports
                // the pin mismatch, which is more informative than aborting.
                failures.push(FailureRecord::ValidationRejected {
                    workload: workload.id.clone(),
                    detail: error.to_string(),
                });
            }
        }
    }

    let toolchain_hash =
        pins::toolchain_hash(&options.repo_root).map_err(|error| error.to_string())?;
    let stdlib_hash = match options.std_root.as_deref() {
        Some(root) => pins::stdlib_hash(root).map_err(|error| error.to_string())?,
        None => String::new(),
    };

    let mut observations: Vec<WorkloadObservation> = Vec::new();
    for workload in &suite.workloads {
        let Some(policy) = epoch.sampling.get(&workload.id) else {
            // Manifest parsing rejects this, so reaching it means the manifest
            // was constructed some other way.
            failures.push(FailureRecord::ValidationRejected {
                workload: workload.id.clone(),
                detail: "the epoch declares no sampling policy".to_string(),
            });
            continue;
        };

        let source = options.repo_root.join(&workload.source);
        let output = workdir.join(format!("{}-out", workload.id));
        let mut samples: Vec<Sample> = Vec::new();

        for index in 0..policy.samples {
            let request = SampleRequest {
                compiler: &options.compiler,
                source: &source,
                args: &epoch.args,
                target: Some(&epoch.target),
                output: output.clone(),
                std_root: options.std_root.as_deref(),
                batch_size: policy.batch_size,
                workload: &workload.id,
                sample_index: index,
                boundary_policy: epoch.boundary.as_ref(),
            };
            match measure_sample(&request) {
                SampleOutcome::Measured(sample) => {
                    // Record the producer's own view of an invariant violation.
                    // Validation recomputes it independently, so a runner that
                    // failed to notice is itself caught.
                    if !sample.phases.holds() {
                        failures.push(FailureRecord::PhaseInvariant {
                            workload: workload.id.clone(),
                            sample_index: index,
                            compiler_root_ns: sample.phases.compiler_root_ns,
                            attributed_ns: sample.phases.attributed_ns(),
                        });
                    }
                    samples.push(*sample);
                }
                SampleOutcome::Failed(failure) => {
                    failures.push(*failure);
                    // Stop this workload but keep the run going: a partial run
                    // still publishes every workload that completed validly.
                    break;
                }
            }
        }

        if !samples.is_empty() {
            observations.push(WorkloadObservation {
                workload: workload.id.clone(),
                boundary: None,
                samples,
            });
        }
    }

    // Sorted order is part of the canonical form, so an unsorted run object
    // would hash differently from the identical measurement written correctly.
    observations.sort_by(|left, right| left.workload.cmp(&right.workload));

    // Assembled in the full-evidence encoding: every per-process proof is
    // present for validation's deep checks and for the retained artifact.
    // Only what reaches the store is written in the digest encoding below.
    let run = RunObject {
        schema_version: FULL_EVIDENCE_SCHEMA_VERSION,
        identity: RunIdentity {
            suite_revision: epoch.suite_revision,
            epoch: epoch.id,
            platform: options.platform.clone(),
            commit: options.commit.clone(),
            started_at,
            finished_at: utc_timestamp(),
            pins: ResolvedPins {
                toolchain_hash,
                stdlib_hash,
                workload_source_hashes,
                invocation: Invocation {
                    target: epoch.target.clone(),
                    args: epoch.args.clone(),
                },
            },
            environment: environment::fingerprint(),
        },
        boundary: None,
        full_evidence: None,
        workloads: observations,
        failures,
    };

    let outcome = validate_run(&manifest, &run);
    let regressions = process_elapsed_regressions(&manifest, &run, &outcome);

    // Evidence is written before anything can abort. A run that validation
    // refuses is exactly the run whose bytes matter most (RUE-1258), so the
    // full-evidence form — the collection workflow's retained artifact,
    // complete per-process depth at zero cost to the store — reaches disk
    // before the encoding step gets a chance to fail.
    let full_serialized = rue_perf_schema::canonical_json(&run)
        .map_err(|error| format!("could not serialize the full-evidence form: {error}"))?;
    if let Some(parent) = options
        .output
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("could not create {}: {error}", parent.display()))?;
    }
    // Deliberately NOT a `.json` sibling: the publish job sweeps every
    // `*.json` artifact onto the data branch, and under the dual-version
    // reader the full-evidence form is itself appendable — the one file that
    // must never reach the store is the one a `.json` spelling would publish.
    let full_evidence_path = options.output.with_extension("full-evidence");
    std::fs::write(&full_evidence_path, &full_serialized)
        .map_err(|error| format!("could not write {}: {error}", full_evidence_path.display()))?;

    // The stored form replaces per-process evidence with one witness per
    // workload plus per-process digests (ADR-0071 Amendment 1).
    let encoded = encode_stored_v3(&run)
        .map_err(|error| format!("could not encode the run object for storage: {error}"))?;
    let serialized = rue_perf_schema::canonical_json(&encoded)
        .map_err(|error| format!("could not serialize the run object: {error}"))?;
    std::fs::write(&options.output, &serialized)
        .map_err(|error| format!("could not write {}: {error}", options.output.display()))?;

    // The encoded form is validated again so an encoding defect fails the
    // collection loudly rather than reaching the store unnoticed. The gate is
    // appendability agreement, not error-list equality: the two encodings
    // word the same refusal differently (per-process details against
    // witness-level ones), and a run that validation rejects under both is
    // ordinary rejected evidence, published and exit-coded exactly as
    // before. Both files are already on disk either way.
    let encoded_outcome = validate_run(&manifest, &encoded);
    if encoded_outcome.is_appendable() != outcome.is_appendable() {
        return Err(format!(
            "the encoded run object's appendability disagrees with the full form's: full \
             {:?}, encoded {:?}",
            outcome.errors, encoded_outcome.errors
        ));
    }

    report(&encoded, &outcome, &regressions, &options.output);

    Ok(collection_exit(&outcome, &regressions))
}

/// The status a written run object reports to its workflow.
///
/// Separate from the run's assembly because it is the whole interface between
/// a collection and the human who finds out about it: every other signal this
/// runner produces is a line in a log nobody reads unless something already
/// told them to.
///
/// A regression outranks incompleteness: it is a judgement about the
/// measurement that did happen, while incompleteness is about the one that did
/// not, and the ratchet is the louder of the two.
fn collection_exit(outcome: &ValidationOutcome, regressions: &[ProcessElapsedRegression]) -> u8 {
    if !outcome.is_appendable() {
        exit::NOT_APPENDABLE
    } else if !regressions.is_empty() {
        exit::REGRESSION
    } else if !outcome.completeness.publishes_headline() {
        exit::INCOMPLETE
    } else {
        exit::OK
    }
}

fn report(
    run: &RunObject,
    outcome: &ValidationOutcome,
    regressions: &[ProcessElapsedRegression],
    output: &Path,
) {
    let address = run
        .content_address()
        .unwrap_or_else(|_| "<unaddressable>".to_string());
    eprintln!("rue-bench: wrote {} ({address})", output.display());

    for error in &outcome.errors {
        eprintln!("rue-bench: not appendable: {error}");
    }
    // Every recorded failure, in the job log rather than only inside the
    // uploaded run object. A workload that produced no sample records exactly
    // one of these and nothing else, so this line is the whole explanation of
    // an empty collection — and reading it should not require downloading an
    // artifact (RUE-1514).
    for failure in &run.failures {
        eprintln!(
            "rue-bench: {} failure: {failure:?}",
            failure.workload().unwrap_or("run-level")
        );
    }
    for sample in &outcome.invalid_samples {
        eprintln!(
            "rue-bench: invalid sample {}[{}]: {}",
            sample.workload, sample.sample_index, sample.reason
        );
    }
    for regression in regressions {
        eprintln!(
            "rue-bench: performance regression: workload {:?} fresh-process median {} ns \
             exceeds the fixed {} ns ratchet",
            regression.workload, regression.current_median_ns, regression.limit_ns
        );
    }
    match &outcome.completeness {
        Completeness::Complete => {
            eprintln!("rue-bench: complete run; a headline point may be published");
        }
        Completeness::Partial { missing } => {
            eprintln!(
                "rue-bench: partial run; no headline point. Incomplete workloads: {}",
                missing.join(", ")
            );
            if run.workloads.is_empty() {
                eprintln!(
                    "rue-bench: no workload produced a sample, so this commit adds no point to \
                     any series"
                );
            }
        }
    }
}

/// The current instant as `YYYY-MM-DDTHH:MM:SSZ`.
///
/// One spelling, because two identical measurements formatting the same instant
/// differently would produce two content addresses. Computed from the Unix
/// epoch rather than pulling in a date library for two fields.
pub(crate) fn utc_timestamp() -> String {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0);
    let (days, rest) = (seconds / 86_400, seconds % 86_400);
    let (hour, minute, second) = (rest / 3600, (rest % 3600) / 60, rest % 60);
    let (year, month, day) = civil_from_days(days as i64);
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

/// Howard Hinnant's `civil_from_days`, shifting the era to start in March so
/// the leap day lands at the end of a cycle.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * shifted_month + 2) / 5 + 1) as u32;
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    } as u32;
    (if month <= 2 { year + 1 } else { year }, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE_MANIFEST: &str = r#"
[[suite]]
revision = 1
timing_schema_version = 1
protocol_version = 1

[[suite.workloads]]
id = "startup"
source = "performance/workloads/startup/main.rue"
question = "What does a minimal compilation cost end to end?"

[[epoch]]
id = 1
platform = "x86_64-linux"
suite_revision = 1
target = "x86_64-unknown-linux-gnu"
args = ["-O2"]
toolchain_hash = "toolchain"

[epoch.workload_source_hashes]
startup = "startup-hash"

[epoch.environment]
runner_label = "local"
runner_image = "local"

[epoch.sampling.startup]
samples = 1
batch_size = 1

[epoch.flagging]
k = 3.0
window = 10
"#;

    fn fixture_run() -> RunObject {
        use rue_perf_schema::{EnvironmentFingerprint, Phase, PhaseAccounting};

        let phase_ns: BTreeMap<Phase, u64> =
            Phase::ALL.into_iter().map(|phase| (phase, 0)).collect();
        let mut phases = PhaseAccounting {
            phase_ns,
            mixed_parallel_ns: 0,
            unattributed_ns: 0,
            compiler_root_ns: 10,
        };
        phases.phase_ns.insert(Phase::SemanticAnalysis, 10);

        RunObject {
            schema_version: rue_perf_schema::RUN_SCHEMA_VERSION,
            identity: RunIdentity {
                suite_revision: 1,
                epoch: 1,
                platform: "x86_64-linux".to_string(),
                commit: "a".repeat(40),
                started_at: "2026-07-28T00:00:00Z".to_string(),
                finished_at: "2026-07-28T00:00:01Z".to_string(),
                pins: ResolvedPins {
                    toolchain_hash: "toolchain".to_string(),
                    stdlib_hash: "stdlib".to_string(),
                    workload_source_hashes: BTreeMap::from([(
                        "startup".to_string(),
                        "startup-hash".to_string(),
                    )]),
                    invocation: Invocation {
                        target: "x86_64-unknown-linux-gnu".to_string(),
                        args: vec!["-O2".to_string()],
                    },
                },
                environment: EnvironmentFingerprint {
                    runner_label: "local".to_string(),
                    runner_image: "local".to_string(),
                    runner_image_version: "unknown".to_string(),
                    cpu_model: "probe".to_string(),
                    core_count: 1,
                    memory_bytes: 1,
                    kernel_version: "probe".to_string(),
                    os_version: "probe".to_string(),
                    architecture: "x86_64".to_string(),
                },
            },
            boundary: None,
            // A placeholder commitment: validation checks the shape, and only
            // encode_stored_v3 computes the real address.
            full_evidence: Some("f".repeat(64)),
            workloads: vec![WorkloadObservation {
                workload: "startup".to_string(),
                boundary: None,
                samples: vec![Sample {
                    batch_size: 1,
                    process_elapsed_ns: 20,
                    peak_memory_bytes: 1,
                    output_binary_bytes: 1,
                    phases,
                    boundary_evidence: Vec::new(),
                    boundary_processes: Vec::new(),
                    boundary_work_processes: Vec::new(),
                }],
            }],
            failures: Vec::new(),
        }
    }

    #[test]
    fn timestamps_use_the_one_spelling_validation_accepts() {
        let stamp = utc_timestamp();
        assert_eq!(stamp.len(), 20, "{stamp}");
        assert!(stamp.ends_with('Z'), "{stamp}");
        assert_eq!(stamp.as_bytes()[10], b'T', "{stamp}");
        // Sub-second precision would make two runs of the same instant hash
        // differently, so it must not appear.
        assert!(!stamp.contains('.'), "{stamp}");
    }

    #[test]
    fn civil_dates_match_known_values() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(19_723), (2024, 1, 1));
        // A leap day, which the March-shifted era exists to get right.
        assert_eq!(civil_from_days(19_782), (2024, 2, 29));
        assert_eq!(civil_from_days(19_783), (2024, 3, 1));
    }

    #[test]
    fn a_generated_timestamp_is_accepted_by_schema_validation() {
        // The runner and the schema must agree on the format, and the schema is
        // the authority. Building a run around a generated stamp proves it.
        let manifest = Manifest::parse(FIXTURE_MANIFEST).expect("fixture manifest");
        let mut run = fixture_run();
        run.identity.started_at = utc_timestamp();
        run.identity.finished_at = utc_timestamp();
        let outcome = validate_run(&manifest, &run);
        assert!(
            !outcome.errors.iter().any(|error| matches!(
                error,
                rue_perf_schema::ValidationError::MalformedTimestamp { .. }
            )),
            "{:?}",
            outcome.errors
        );
    }

    #[test]
    fn a_runner_shaped_run_object_is_appendable() {
        // Guards the runner's own assembly against the schema: if the two ever
        // disagree about field meanings, this fails before CI collects anything.
        let manifest = Manifest::parse(FIXTURE_MANIFEST).expect("fixture manifest");
        let outcome = validate_run(&manifest, &fixture_run());
        assert_eq!(outcome.errors, Vec::new());
        assert!(outcome.publishes_headline());
    }

    #[test]
    fn an_unresolvable_pin_is_recorded_and_blocks_appending() {
        // The runner records a resolution failure rather than substituting the
        // epoch's declared value, so the mismatch surfaces instead of hiding.
        let manifest = Manifest::parse(FIXTURE_MANIFEST).expect("fixture manifest");
        let mut run = fixture_run();
        run.identity.pins.workload_source_hashes.clear();
        run.failures.push(FailureRecord::ValidationRejected {
            workload: "startup".to_string(),
            detail: "could not resolve the source closure".to_string(),
        });
        let outcome = validate_run(&manifest, &run);
        assert!(!outcome.is_appendable());
        assert!(outcome.errors.iter().any(|error| matches!(
            error,
            rue_perf_schema::ValidationError::PinMismatch { field, .. }
                if field == "workload_source_hashes/startup"
        )));
    }

    #[test]
    fn a_crashed_workload_leaves_a_partial_but_appendable_run() {
        // Collection health must be visible rather than appearing as a hole.
        let manifest = Manifest::parse(FIXTURE_MANIFEST).expect("fixture manifest");
        let mut run = fixture_run();
        run.workloads.clear();
        run.failures.push(FailureRecord::WorkloadCrashed {
            workload: "startup".to_string(),
            sample_index: 0,
            detail: "signal 11".to_string(),
        });
        let outcome = validate_run(&manifest, &run);
        assert!(outcome.is_appendable(), "{:?}", outcome.errors);
        assert!(!outcome.publishes_headline());
        assert_eq!(
            outcome.completeness,
            Completeness::Partial {
                missing: vec!["startup".to_string()],
            }
        );
    }

    #[test]
    fn an_incomplete_collection_reports_a_status_of_its_own() {
        // RUE-1514. The run above is appendable and stored, and used to exit 0
        // for that reason: a fail-closed sample check that was fail-open at the
        // level of the series it protects. Three platforms published
        // `workloads: []` for a day with every job green, and what eventually
        // spoke up was the stall gate — days late, and against every pull
        // request in the repository rather than this collection.
        let manifest = Manifest::parse(FIXTURE_MANIFEST).expect("fixture manifest");

        let complete = validate_run(&manifest, &fixture_run());
        assert_eq!(collection_exit(&complete, &[]), exit::OK);

        let mut empty = fixture_run();
        empty.workloads.clear();
        empty.failures.push(FailureRecord::WorkloadCrashed {
            workload: "startup".to_string(),
            sample_index: 0,
            detail: "build-boundary evidence rejected".to_string(),
        });
        let outcome = validate_run(&manifest, &empty);
        assert!(outcome.is_appendable(), "{:?}", outcome.errors);
        assert_eq!(collection_exit(&outcome, &[]), exit::INCOMPLETE);

        // A run that cannot be stored at all keeps saying so. Both codes fail
        // the collection job, but they are different facts with different
        // remedies, and the job's message says which one happened.
        let mut rejected = fixture_run();
        rejected.identity.pins.workload_source_hashes.clear();
        let outcome = validate_run(&manifest, &rejected);
        assert!(!outcome.is_appendable());
        assert_eq!(collection_exit(&outcome, &[]), exit::NOT_APPENDABLE);
    }
}
