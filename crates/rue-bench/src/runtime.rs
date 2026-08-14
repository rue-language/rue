//! The ADR-0072 runtime-measurement mode: how fast does compiled Rue *run*?
//!
//! The compile-time runner in `main.rs` measures the compiler. This one
//! measures the program the compiler produced. It is a mode of the same binary
//! rather than a separate tool, so the two can never disagree about what a
//! sample, an epoch, or a canonical record means.
//!
//! One run does four things per workload, in this order:
//!
//! 1. **Build** the program at release quality (`-O3`), and record the
//!    executable's size and digest. This compilation is *not* a compile-time
//!    observation and never enters a compile-time series: a one-sample compile
//!    from here matches no declared epoch in `manifest.toml` or `scaling.toml`,
//!    and ADR-0067's validation would rightly refuse it (ADR-0072 Decision 7).
//! 2. **Prepare** the fixture from the manifest's pinned seed and generator
//!    revision, and record the digest of the bytes actually produced. Outside
//!    the timed window, always.
//! 3. **Measure** N independent fresh processes, spawn to exit, recording wall
//!    time, peak RSS, exit code, and a digest of stdout for each.
//! 4. **Judge** the output against the committed golden, and check that every
//!    sample produced the same bytes. Also outside the timed window.
//!
//! Judgement stops there. This module records what happened — including a wrong
//! answer, a crash, and a fixture whose digest moved — and hands the record to
//! `rue_perf_schema::validate_runtime_report`, which decides appendability.
//! Keeping the verdict out of the producer is what lets validation catch a
//! producer that is itself wrong.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Instant;

use rue_perf_schema::{
    CorpusTreeProvenance, FIXTURE_ARGUMENT, FIXTURE_INPUT_NAME, FixtureDeclaration,
    GeneratedProvenance, OUTPUT_ARGUMENT, OracleKind, OracleOutcome, OracleVerdict,
    PeerObservation, PeerPolicy, PeerRole, PeerThreadPolicy, ProgramIdentity, RUNTIME_RECORD_KIND,
    RUNTIME_REPORT_SCHEMA_VERSION, RecordedInput, RuntimeCompleteness, RuntimeEpoch,
    RuntimeFailure, RuntimeIdentity, RuntimeManifest, RuntimeObservation, RuntimeRegime,
    RuntimeReport, RuntimeSample, RuntimeSuiteRevision, RuntimeWorkload, validate_runtime_report,
};

use crate::digest::sha256_bytes as sha256;
use crate::fixture;
use crate::measure::spawn_and_reap;
use crate::{environment, pins};

/// Exit statuses this mode reports.
pub mod exit {
    /// The report was written and is appendable, and the suite completed.
    pub const OK: u8 = 0;
    /// The runner could not be configured or could not write its output.
    pub const USAGE: u8 = 2;
    /// A report was written, but validation refuses it for its series.
    pub const NOT_APPENDABLE: u8 = 3;
    /// The report is appendable, but a workload did not complete.
    ///
    /// Distinct from [`NOT_APPENDABLE`]: the evidence belongs in the store and
    /// the series simply has a hole here. Collection health should be visible
    /// on the dashboard, not absent from it.
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
    /// Directory holding the previous peer run's recorded state, one file per
    /// workload, for the event question of ADR-0072 Decision 9. An absent file
    /// means "no peer observation is recorded" and the full leg runs — the
    /// conservative answer, since a comparison that has never been taken cannot
    /// be joined to. A missing directory therefore costs a peer run rather than
    /// a wrong join.
    peer_state: Option<PathBuf>,
    /// `platform/epoch`, so a new epoch is itself a peer-leg event.
    epoch_label: String,
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
    let mut peer_state = None;

    let mut args = std::env::args().skip(2);
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
            "--peer-state-dir" => peer_state = Some(PathBuf::from(value()?)),
            other => return Err(format!("unrecognized argument {other:?}")),
        }
    }

    let repo_root = repo_root.unwrap_or_else(|| PathBuf::from("."));
    let std_root = std_root.or_else(|| {
        let candidate = repo_root.join("std");
        candidate.is_dir().then_some(candidate)
    });

    let platform = platform.ok_or("runtime requires --platform")?;
    let epoch_label = format!(
        "{platform}/{}",
        epoch
            .map(|id| id.to_string())
            .unwrap_or_else(|| "collection".to_string())
    );
    Ok(Options {
        manifest: manifest.unwrap_or_else(|| repo_root.join("performance/runtime.toml")),
        platform,
        epoch,
        compiler: compiler.ok_or("runtime requires --compiler")?,
        commit: commit.ok_or("runtime requires --commit")?,
        output: output.ok_or("runtime requires --out")?,
        repo_root,
        std_root,
        workdir,
        peer_state,
        epoch_label,
    })
}

/// Run the runtime suite for one platform epoch.
pub fn run() -> Result<u8, String> {
    let mut options = parse_args()?;
    let text = std::fs::read_to_string(&options.manifest)
        .map_err(|error| format!("could not read {}: {error}", options.manifest.display()))?;
    let manifest = RuntimeManifest::parse(&text)?;

    // Without `--epoch`, the manifest's collection epoch for this platform is
    // measured, for the same reason the compile-time runner does it: a workflow
    // naming an epoch number would have to be edited alongside the manifest,
    // and the two would eventually disagree about what is being collected.
    let epoch = match options.epoch {
        Some(id) => manifest.epoch(&options.platform, id).ok_or_else(|| {
            format!(
                "the runtime manifest declares no epoch {id} for platform {}",
                options.platform
            )
        })?,
        None => manifest
            .collection_epoch(&options.platform)
            .ok_or_else(|| {
                format!(
                    "the runtime manifest marks no collection epoch for platform {}; \
                     pass --epoch to measure a specific one",
                    options.platform
                )
            })?,
    };
    // The epoch a run actually measured, not the one the command line named:
    // collection passes no `--epoch`, and a new epoch is itself an event that
    // makes the full peer leg due (ADR-0072 Decision 9).
    options.epoch_label = format!("{}/{}", options.platform, epoch.id);
    let suite = manifest.suite(epoch.suite_revision).ok_or_else(|| {
        format!(
            "runtime suite revision {} is not declared",
            epoch.suite_revision
        )
    })?;

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

    let started_at = crate::utc_timestamp();
    let compiler_version = compiler_version(&options.compiler)?;
    let toolchain_hash =
        pins::toolchain_hash(&options.repo_root).map_err(|error| error.to_string())?;
    let stdlib_hash = match options.std_root.as_deref() {
        Some(root) => pins::stdlib_hash(root).map_err(|error| error.to_string())?,
        None => String::new(),
    };

    let mut failures: Vec<RuntimeFailure> = Vec::new();
    let mut workload_source_hashes = BTreeMap::new();
    let mut observations: Vec<RuntimeObservation> = Vec::new();

    for workload in &suite.workloads {
        match pins::workload_source_hash(
            &options.compiler,
            &options.repo_root.join(&workload.source),
            options.std_root.as_deref(),
        ) {
            Ok(hash) => {
                workload_source_hashes.insert(workload.id.clone(), hash);
            }
            Err(error) => {
                // Recorded, not fatal. The identity of what was measured is
                // part of the observation, so its absence must be visible in
                // the record rather than aborting collection.
                failures.push(RuntimeFailure::ValidationRejected {
                    workload: workload.id.clone(),
                    detail: error.to_string(),
                });
            }
        }

        match measure_workload(&options, epoch, workload, workdir, &mut failures) {
            Ok(observation) => observations.push(observation),
            Err(failure) => failures.push(failure),
        }
    }

    // Sorted order is part of the canonical form, so an unsorted report would
    // hash differently from the identical measurement written correctly.
    observations.sort_by(|left, right| left.workload.cmp(&right.workload));

    let report = RuntimeReport {
        record_kind: RUNTIME_RECORD_KIND.to_string(),
        schema_version: RUNTIME_REPORT_SCHEMA_VERSION,
        identity: RuntimeIdentity {
            suite_revision: epoch.suite_revision,
            epoch: epoch.id,
            platform: options.platform.clone(),
            commit: options.commit.clone(),
            compiler_version,
            started_at,
            finished_at: crate::utc_timestamp(),
            toolchain_hash,
            stdlib_hash,
            workload_source_hashes,
            environment: environment::fingerprint(),
        },
        regime: regime(epoch, suite),
        workloads: observations,
        failures,
    };

    let outcome = validate_runtime_report(&manifest, &report);

    // Written whatever the verdict. Evidence of a broken collection is worth
    // more than a missing file.
    let serialized = rue_perf_schema::canonical_json(&report)
        .map_err(|error| format!("could not serialize the runtime report: {error}"))?;
    if let Some(parent) = options
        .output
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("could not create {}: {error}", parent.display()))?;
    }
    std::fs::write(&options.output, &serialized)
        .map_err(|error| format!("could not write {}: {error}", options.output.display()))?;

    let address = report
        .content_address()
        .unwrap_or_else(|_| "<unaddressable>".to_string());
    eprintln!(
        "rue-bench runtime: wrote {} ({address})",
        options.output.display()
    );
    for error in &outcome.errors {
        eprintln!("rue-bench runtime: not appendable: {error}");
    }
    for failure in &report.failures {
        eprintln!(
            "rue-bench runtime: {} failure: {failure:?}",
            failure.workload()
        );
    }

    Ok(match &outcome.completeness {
        _ if !outcome.is_appendable() => exit::NOT_APPENDABLE,
        RuntimeCompleteness::Complete => {
            eprintln!("rue-bench runtime: complete run; every workload may publish");
            exit::OK
        }
        RuntimeCompleteness::Partial { missing } => {
            eprintln!(
                "rue-bench runtime: partial run. Incomplete workloads: {}",
                missing.join(", ")
            );
            exit::INCOMPLETE
        }
    })
}

/// The regime every sample in this report was taken under.
///
/// Assembled from the epoch and suite rather than restated by hand, so the
/// record and the declaration cannot drift; validation then compares the two
/// independently produced answers.
fn regime(epoch: &RuntimeEpoch, suite: &RuntimeSuiteRevision) -> RuntimeRegime {
    RuntimeRegime {
        measured_boundary: suite.measured_boundary,
        program_state: "fresh_process".to_string(),
        os_page_cache: "uncontrolled".to_string(),
        // Both are facts about this runner's structure, asserted here and
        // checked by validation. Fixture preparation happens before the clock
        // starts; the oracle runs after it stops.
        fixture_preparation_measured: false,
        oracle_comparison_measured: false,
        optimization: epoch.optimization,
        compiler_args: epoch.compiler_args.clone(),
        target: epoch.target.clone(),
        thread_policy: epoch.thread_policy,
        hardware_counters: epoch.hardware_counters,
    }
}

/// Build, prepare, measure, and judge one workload.
fn measure_workload(
    options: &Options,
    epoch: &RuntimeEpoch,
    workload: &RuntimeWorkload,
    workdir: &Path,
    failures: &mut Vec<RuntimeFailure>,
) -> Result<RuntimeObservation, RuntimeFailure> {
    let source = options.repo_root.join(&workload.source);
    let binary = workdir.join(format!("{}-program", workload.id));
    let program = build_program(options, epoch, &source, &binary).map_err(|detail| {
        RuntimeFailure::CompileFailed {
            workload: workload.id.clone(),
            detail,
        }
    })?;

    // Outside the timed window, deliberately and structurally: nothing below
    // starts a clock until the fixture already exists on disk.
    let peer_policy = epoch.peers.get(&workload.id);
    let prepared = prepare_fixture(options, workload, workdir, peer_policy).map_err(|detail| {
        RuntimeFailure::FixturePreparationFailed {
            workload: workload.id.clone(),
            detail,
        }
    })?;

    let samples_wanted = epoch
        .sampling
        .get(&workload.id)
        .map(|policy| policy.samples)
        .unwrap_or(0);
    let mut samples: Vec<RuntimeSample> = Vec::with_capacity(samples_wanted as usize);
    let mut outputs: Vec<Vec<u8>> = Vec::with_capacity(samples_wanted as usize);
    let mut trees: Vec<PathBuf> = Vec::new();
    for index in 0..samples_wanted {
        eprintln!(
            "rue-bench runtime: {} sample {}/{samples_wanted}",
            workload.id,
            index + 1
        );
        // Every sample writes to its own destination. Sharing one would let a
        // later sample find a warm tree and skip work an earlier one did, which
        // is the incremental-rebuild scenario rather than this one, and would
        // also make the determinism check compare a tree with itself.
        let destination = workdir.join(format!("{}-out-{index}", workload.id));
        let arguments = resolve_arguments(workload, &prepared.path, &destination);
        match run_sample(
            &binary,
            &arguments,
            workdir,
            workload.oracle.kind,
            &destination,
        ) {
            Ok((sample, stdout)) => {
                if sample.exit_code != 0 {
                    failures.push(RuntimeFailure::ProgramCrashed {
                        workload: workload.id.clone(),
                        sample_index: index,
                        detail: format!("the program exited with code {}", sample.exit_code),
                    });
                    samples.push(sample);
                    outputs.push(stdout);
                    // Keep the evidence and stop: further samples of a program
                    // that is failing measure nothing.
                    break;
                }
                samples.push(sample);
                outputs.push(stdout);
                trees.push(destination);
            }
            Err(detail) => {
                failures.push(RuntimeFailure::ProgramCrashed {
                    workload: workload.id.clone(),
                    sample_index: index,
                    detail,
                });
                break;
            }
        }
    }

    // The clock has stopped for every sample; judging output costs whatever it
    // costs.
    // The peer legs, measured by this runner and this clock so no part of the
    // published ratio comes from a different measurement apparatus. They run
    // BEFORE judgement, because the trees they emit are half of what Decision 4
    // judges: the file sets, page counts, and per-page semantics of all three
    // tools are compared against each other, so a collection run cannot publish
    // a ratio whose denominator built a smaller site.
    let (peers, peer_trees) = match (peer_policy, &prepared.description) {
        (Some(policy), Some(description)) => measure_peers(
            options,
            workload,
            policy,
            description,
            samples_wanted,
            workdir,
            failures,
        ),
        _ => (Vec::new(), BTreeMap::new()),
    };

    let oracle = match workload.oracle.kind {
        OracleKind::GoldenStdout => {
            let golden_path = options.repo_root.join(&workload.oracle.path);
            judge(workload, &golden_path, &outputs)
        }
        OracleKind::SemanticSitePages => {
            judge_site(options, workload, &prepared, &samples, &trees, &peer_trees)
        }
    };
    if oracle.verdict != OracleVerdict::Match {
        failures.push(RuntimeFailure::WrongOutput {
            workload: workload.id.clone(),
            detail: oracle.detail.clone(),
        });
    }

    Ok(RuntimeObservation {
        workload: workload.id.clone(),
        source: workload.source.clone(),
        question: workload.question.clone(),
        // The declared arguments, not the resolved ones: the fixture's path
        // names a temporary directory, and storing it would make two identical
        // measurements produce two different records.
        program_args: workload.program_args.clone(),
        recorded_inputs: vec![prepared.recorded.clone()],
        program,
        oracle,
        samples,
        peers,
    })
}

/// Substitute the declared placeholders with this sample's real paths.
fn resolve_arguments(
    workload: &RuntimeWorkload,
    fixture: &Path,
    destination: &Path,
) -> Vec<String> {
    workload
        .program_args
        .iter()
        .map(|argument| {
            if argument == FIXTURE_ARGUMENT {
                fixture.display().to_string()
            } else if argument == OUTPUT_ARGUMENT {
                destination.display().to_string()
            } else {
                argument.clone()
            }
        })
        .collect()
}

/// Measure the peer tools building the identical corpus (ADR-0072 Decision 5).
///
/// Two legs, and the difference between them is the whole of Decision 9's
/// cadence. The CANARY — one single-threaded build of the 1x corpus by the
/// declared canary tool — runs beside every observation, so the ratio published
/// for this commit has a same-run denominator. The FULL leg — the second peer,
/// the scale variants, and the default-parallel secondary row — runs only when
/// the preparer reports an event: the recorded fixture identity moved, a peer
/// version moved, or the epoch changed. Between events, the derive step joins
/// this observation to the latest full peer observation with a matching fixture
/// identity.
fn measure_peers(
    options: &Options,
    workload: &RuntimeWorkload,
    policy: &PeerPolicy,
    description: &PrepareDescription,
    samples_wanted: u32,
    workdir: &Path,
    failures: &mut Vec<RuntimeFailure>,
) -> (Vec<PeerObservation>, BTreeMap<String, PathBuf>) {
    let due = description
        .peer_event
        .as_ref()
        .map(|event| event.due)
        .unwrap_or(true);
    if let Some(event) = &description.peer_event {
        eprintln!(
            "rue-bench runtime: full peer leg {}: {}",
            if event.due { "due" } else { "not due" },
            event.reason
        );
    }

    let mut wanted: Vec<(String, PeerThreadPolicy, u32, PeerRole)> = vec![(
        policy.canary_tool.clone(),
        policy.primary_thread_policy,
        policy.canary_scale,
        PeerRole::Canary,
    )];
    if due {
        // The peers build this workload's own fixture, at the scale it was
        // prepared at, so the matrix is tools by thread policies. The canary
        // has already covered one cell of it.
        let scale = description.identity.scale;
        for tool in &policy.tools {
            for thread_policy in [
                Some(policy.primary_thread_policy),
                policy.secondary_thread_policy,
            ]
            .into_iter()
            .flatten()
            {
                let already_measured = *tool == policy.canary_tool
                    && thread_policy == policy.primary_thread_policy
                    && scale == policy.canary_scale;
                if already_measured {
                    // Measuring the identical configuration twice would publish
                    // two denominators for one run.
                    continue;
                }
                wanted.push((tool.clone(), thread_policy, scale, PeerRole::Full));
            }
        }
    }

    let wanted_count = wanted.len();
    let mut observations = Vec::new();
    // One emitted tree per tool, from its single-threaded leg: what the
    // cross-tool checks read. The parallel leg builds the same site, so judging
    // both would compare a tool with itself.
    let mut trees: BTreeMap<String, PathBuf> = BTreeMap::new();
    for (tool, thread_policy, scale, role) in wanted {
        match measure_one_peer(
            options,
            workload,
            description,
            &tool,
            thread_policy,
            scale,
            role,
            samples_wanted,
            workdir,
        ) {
            Ok((observation, tree)) => {
                if thread_policy == policy.primary_thread_policy {
                    trees.insert(tool.clone(), tree);
                }
                observations.push(observation);
            }
            Err(detail) => {
                // A peer that could not be measured is recorded as a failure
                // and omitted, never synthesized from an earlier run. If it was
                // the canary, validation marks the workload incomplete: the
                // evidence is kept and no ratio publishes.
                failures.push(RuntimeFailure::ValidationRejected {
                    workload: workload.id.clone(),
                    detail: format!("peer {tool} at {scale}x ({thread_policy:?}): {detail}"),
                });
            }
        }
    }

    // Record the state the NEXT run asks its event question against, and only
    // when a full leg actually completed. Written after the fact rather than
    // before, so a leg that failed halfway leaves the previous state in place
    // and the next run tries again — the conservative direction, since the cost
    // of a redundant peer run is a minute and the cost of a wrongly skipped one
    // is a segment of ratios joined to a peer sample that never existed.
    let complete_full_leg = due
        && observations
            .iter()
            .filter(|observation| observation.role == PeerRole::Full)
            .count()
            + 1
            == wanted_count;
    if complete_full_leg && let Some(directory) = options.peer_state.as_deref() {
        let state = serde_json::json!({
            "fixture_digest": description.identity.fixture_digest,
            "peer_versions": description.peer_versions,
            "epoch": options.epoch_label,
        });
        let path = directory.join(format!("{}.json", workload.id));
        if let Err(error) = std::fs::create_dir_all(directory)
            .and_then(|()| std::fs::write(&path, format!("{state:#}\n")))
        {
            eprintln!(
                "rue-bench runtime: could not record the peer state at {}: {error}",
                path.display()
            );
        }
    }
    (observations, trees)
}

#[allow(clippy::too_many_arguments)]
fn measure_one_peer(
    options: &Options,
    workload: &RuntimeWorkload,
    description: &PrepareDescription,
    tool: &str,
    thread_policy: PeerThreadPolicy,
    scale: u32,
    role: PeerRole,
    samples_wanted: u32,
    workdir: &Path,
) -> Result<(PeerObservation, PathBuf), String> {
    if scale != description.identity.scale {
        // The preparer laid out one scale; another would need its own
        // preparation, which is the event-driven leg's business rather than
        // something to improvise from the tree at hand.
        return Err(format!(
            "the prepared fixture is {}x, so a {scale}x peer build has no fixture to build",
            description.identity.scale
        ));
    }
    let policy_name = match thread_policy {
        PeerThreadPolicy::PinnedSingleThread => "pinned_single_thread",
        PeerThreadPolicy::ToolDefaultParallel => "tool_default_parallel",
    };
    let key = format!("{tool}/{policy_name}");
    let command = description
        .commands
        .get(&key)
        .ok_or_else(|| format!("the preparer laid out no {key} invocation"))?;
    let version = description
        .peer_versions
        .get(tool)
        .cloned()
        .ok_or_else(|| format!("the preparer recorded no version for {tool}"))?;

    let mut samples = Vec::new();
    let mut digest = String::new();
    let mut emitted_files = 0;
    let mut first_tree = PathBuf::new();
    for index in 0..samples_wanted {
        // The thread policy is part of the name, not decoration: the canary and
        // the secondary row are the same tool at the same scale, and Zola
        // refuses to write into a directory that already exists — so without it
        // the second leg of every full run failed on a collision.
        let destination = workdir.join(format!(
            "{}-{tool}-{policy_name}-{scale}x-{index}",
            workload.id
        ));
        let arguments: Vec<String> = command
            .argv
            .iter()
            .skip(1)
            .map(|argument| argument.replace("{out}", &destination.display().to_string()))
            .collect();
        let program = PathBuf::from(&command.argv[0]);
        let mut invocation = Command::new(&program);
        invocation
            .args(&arguments)
            .current_dir(workdir)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (name, value) in &command.env {
            invocation.env(name, value);
        }
        let started = Instant::now();
        let reaped = spawn_and_reap(&mut invocation).map_err(|error| error.to_string())?;
        let elapsed = u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX);
        if !libc_exited_cleanly(reaped.status) {
            return Err(format!(
                "the build did not exit successfully; stderr tail:\n{}",
                String::from_utf8_lossy(&reaped.stderr)
                    .lines()
                    .rev()
                    .take(5)
                    .collect::<Vec<_>>()
                    .join("\n")
            ));
        }
        let tree = crate::digest::sha256_tree(&destination)?;
        if index == 0 {
            digest = tree.clone();
            emitted_files = crate::digest::count_tree(&destination)?;
            first_tree = destination.clone();
        }
        samples.push(RuntimeSample {
            process_elapsed_ns: elapsed,
            peak_memory_bytes: reaped.peak_memory_bytes,
            exit_code: 0,
            stdout_bytes: reaped.stdout.len() as u64,
            stdout_sha256: sha256(&reaped.stdout),
            artifact_sha256: Some(tree),
        });
    }
    let _ = options;
    Ok((
        PeerObservation {
            tool: tool.to_string(),
            version,
            role,
            thread_policy,
            scale,
            output_sha256: digest,
            emitted_files,
            samples,
        },
        first_tree,
    ))
}

fn libc_exited_cleanly(status: i32) -> bool {
    libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0
}

/// Compile one workload at release quality and describe the executable.
///
/// The compile is a cost this harness pays because it must run the program; its
/// timing is deliberately discarded rather than recorded, per ADR-0072
/// Decision 7.
fn build_program(
    options: &Options,
    epoch: &RuntimeEpoch,
    source: &Path,
    binary: &Path,
) -> Result<ProgramIdentity, String> {
    let mut command = Command::new(&options.compiler);
    crate::environment::sanitize_compiler_environment(&mut command);
    command
        .arg(source)
        .arg("-o")
        .arg(binary)
        .args(&epoch.compiler_args)
        .arg("--target")
        .arg(&epoch.target)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(std_root) = options.std_root.as_deref() {
        command.env("RUE_STD_PATH", std_root);
    }
    let output = command
        .output()
        .map_err(|error| format!("could not run the compiler: {error}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let tail: String = stderr.lines().rev().take(10).collect::<Vec<_>>().join("\n");
        return Err(format!(
            "the compiler exited unsuccessfully ({}); stderr tail:\n{tail}",
            output.status
        ));
    }
    let bytes = std::fs::read(binary)
        .map_err(|error| format!("could not read the compiled program: {error}"))?;
    if bytes.is_empty() {
        return Err("the compiler produced an empty executable".to_string());
    }
    Ok(ProgramIdentity {
        binary_bytes: bytes.len() as u64,
        sha256: sha256(&bytes),
    })
}

/// What preparing one workload's fixture produced.
#[derive(Debug)]
pub struct PreparedFixture {
    /// The path the program receives in place of [`FIXTURE_ARGUMENT`] — a file
    /// for a seeded fixture, a directory for a corpus tree.
    pub path: PathBuf,
    /// The identity recorded with the observation.
    pub recorded: RecordedInput,
    /// What the corpus preparer reported, when there was one.
    pub description: Option<PrepareDescription>,
}

/// The corpus preparer's report: what it laid out, and what it found.
///
/// ADR-0072 names `scripts/gazette-corpus-diff.py` the reference
/// implementation of corpus assembly, identity, and validation, and says the
/// harness mode should adopt it rather than reinvent it. This runner does
/// exactly that: it asks the preparer to lay out every tool's fixture and to
/// answer the peer-event question, and keeps for itself the one thing a second
/// implementation would genuinely damage — the measurement, so every tool's
/// time comes from one clock and every tool's memory from one reaping.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct PrepareDescription {
    /// The recorded fixture identity, as the preparer computed it.
    pub identity: FixtureIdentity,
    /// Each pinned peer's own version string.
    #[serde(default)]
    pub peer_versions: BTreeMap<String, String>,
    /// The measured invocation for each tool and thread policy.
    #[serde(default)]
    pub commands: BTreeMap<String, PreparedCommand>,
    /// Whether the full peer leg is due, and why.
    #[serde(default)]
    pub peer_event: Option<PeerEvent>,
    /// How many content pages the assembled corpus holds.
    #[serde(default)]
    pub pages: u64,
    /// The fixture-assembly revision the preparer implements.
    #[serde(default)]
    pub preparer_revision: u32,
}

/// The identity fields this runner records from the preparer's report.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct FixtureIdentity {
    /// One digest over every input class the tools consume.
    pub fixture_digest: String,
    /// Files and bytes of the corpus as identified — before duplication, so a
    /// scale variant of an unchanged corpus does not look like a content
    /// change.
    pub content_files: u64,
    /// Total bytes of that content.
    pub content_bytes: u64,
    /// Files and bytes of the corpus AS BUILT, after the scale duplication.
    /// What the record states, because it is what the tools read.
    pub scaled_content_files: u64,
    /// Total bytes of the corpus as built.
    pub scaled_content_bytes: u64,
    /// Static passthrough files, which are measured work for every tool.
    pub static_files: u64,
    /// Their total bytes.
    pub static_bytes: u64,
    /// Template-port and parity-config files.
    pub port_files: u64,
    /// Their total bytes.
    pub port_bytes: u64,
    /// The port revision a maintainer bumps deliberately.
    pub port_revision: u32,
    /// The scale variant assembled.
    pub scale: u32,
    /// The content paths excluded from the corpus.
    pub excluded: Vec<String>,
}

/// One tool's measured invocation, as the preparer laid it out.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct PreparedCommand {
    /// The argument vector, with `{out}` standing for the destination.
    pub argv: Vec<String>,
    /// Environment the thread policy requires — `RAYON_NUM_THREADS` for Zola,
    /// `GOMAXPROCS` for Hugo.
    pub env: BTreeMap<String, String>,
}

/// Whether the full peer leg is due this run (ADR-0072 Decision 9).
#[derive(Debug, Clone, serde::Deserialize)]
pub struct PeerEvent {
    /// Whether an event fired.
    pub due: bool,
    /// Which one, for the log and for the annotation.
    pub reason: String,
}

/// Prepare a workload's fixture and record the identity of what it consumed.
///
/// Outside the timed window, structurally: nothing here starts a clock, and
/// every caller has the fixture on disk before it does.
fn prepare_fixture(
    options: &Options,
    workload: &RuntimeWorkload,
    workdir: &Path,
    peers: Option<&PeerPolicy>,
) -> Result<PreparedFixture, String> {
    match &workload.fixture {
        FixtureDeclaration::SeededGenerator { .. } => {
            prepare_seeded_fixture(workload, workdir.join(workload.fixture.work_name()))
        }
        FixtureDeclaration::CorpusTree { .. } => {
            prepare_corpus_tree(options, workload, workdir, peers)
        }
    }
}

/// Generate a seeded fixture and record the identity of the bytes produced.
///
/// The generator's revision is checked against the declaration rather than
/// assumed: the committed golden output is a function of the produced bytes, so
/// a manifest asking for a revision this binary does not implement would
/// produce a confusing oracle mismatch instead of a clear refusal.
fn prepare_seeded_fixture(
    workload: &RuntimeWorkload,
    path: PathBuf,
) -> Result<PreparedFixture, String> {
    let FixtureDeclaration::SeededGenerator {
        category,
        generator,
        generator_revision,
        seed,
        bytes: declared_bytes,
        vocabulary_size,
        description,
        ..
    } = &workload.fixture
    else {
        return Err("a seeded fixture was expected".to_string());
    };
    if generator != fixture::ZIPF_ASCII_TEXT {
        return Err(format!(
            "this runner implements the {:?} fixture generator, not {generator:?}",
            fixture::ZIPF_ASCII_TEXT,
        ));
    }
    if *generator_revision != fixture::ZIPF_ASCII_TEXT_REVISION {
        return Err(format!(
            "the manifest pins {generator} revision {generator_revision}, but this runner \
             implements revision {}",
            fixture::ZIPF_ASCII_TEXT_REVISION
        ));
    }
    let bytes = fixture::generate_zipf_ascii_text(*seed, *declared_bytes, *vocabulary_size);
    std::fs::write(&path, &bytes)
        .map_err(|error| format!("could not write {}: {error}", path.display()))?;
    Ok(PreparedFixture {
        recorded: RecordedInput {
            name: FIXTURE_INPUT_NAME.to_string(),
            category: *category,
            description: description.clone(),
            // Over the bytes the program will actually read, not over the
            // declaration that asked for them. That is what makes a generator
            // whose output drifted from its declaration visible in the data.
            identity_sha256: sha256(&bytes),
            files: 1,
            bytes: bytes.len() as u64,
            provenance: Some(GeneratedProvenance {
                generator: generator.clone(),
                generator_revision: *generator_revision,
                seed: *seed,
                vocabulary_size: *vocabulary_size,
            }),
            tree: None,
        },
        path,
        description: None,
    })
}

/// Assemble a corpus tree through the checked-in preparer the suite pins.
///
/// The scale variants, the exclusions, the peers' sites, and the identity all
/// come from that one implementation. What this function adds is the check the
/// manifest can make and the script cannot: that the tree the preparer built is
/// the tree the suite declared, at the declared scale with the declared pages
/// excluded. A record whose scale or exclusion set drifted from the manifest is
/// measuring a different workload, and it fails here rather than publishing a
/// point into a series it does not belong to.
fn prepare_corpus_tree(
    options: &Options,
    workload: &RuntimeWorkload,
    workdir: &Path,
    peers: Option<&PeerPolicy>,
) -> Result<PreparedFixture, String> {
    let FixtureDeclaration::CorpusTree {
        category,
        preparer,
        preparer_revision,
        scale,
        excluded,
        description,
        ..
    } = &workload.fixture
    else {
        return Err("a corpus-tree fixture was expected".to_string());
    };
    let root = workdir.join(workload.fixture.work_name());
    let report = workdir.join(format!("{}-fixture.json", workload.id));
    let preparer_path = options.repo_root.join(preparer);

    let mut command = Command::new("python3");
    command
        .arg(&preparer_path)
        .arg("prepare")
        .arg("--root")
        .arg(&root)
        .arg("--scale")
        .arg(scale.to_string())
        .arg("--out")
        .arg(&report)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // The peers' sites are laid out only where the epoch declares peers. A run
    // that will not measure them should not pay to assemble them.
    if peers.is_some() {
        command.arg("--peers");
        command.arg("--epoch").arg(&options.epoch_label);
        if let Some(state) = options.peer_state.as_deref() {
            command
                .arg("--peer-state")
                .arg(state.join(format!("{}.json", workload.id)));
        }
    }
    let output = command
        .output()
        .map_err(|error| format!("could not run {}: {error}", preparer_path.display()))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "the corpus preparer exited unsuccessfully ({}); stderr tail:\n{}",
            output.status,
            stderr.lines().rev().take(10).collect::<Vec<_>>().join("\n")
        ));
    }
    let text = std::fs::read_to_string(&report)
        .map_err(|error| format!("could not read {}: {error}", report.display()))?;
    let description_report: PrepareDescription = serde_json::from_str(&text)
        .map_err(|error| format!("could not read the preparer's report: {error}"))?;
    let identity = &description_report.identity;

    // Checked exactly as the seeded generator's revision is: the assembly is
    // what every tool consumes, so a manifest pinning one this script no longer
    // implements would measure a different job and say nothing about it.
    if description_report.preparer_revision != *preparer_revision {
        return Err(format!(
            "the manifest pins {preparer} revision {preparer_revision}, but the checked-in \
             preparer implements revision {}",
            description_report.preparer_revision
        ));
    }
    if identity.scale != *scale {
        return Err(format!(
            "the preparer assembled the {}x corpus, but the suite declares {scale}x",
            identity.scale
        ));
    }
    let mut prepared_exclusions = identity.excluded.clone();
    let mut declared_exclusions = excluded.clone();
    prepared_exclusions.sort();
    declared_exclusions.sort();
    if prepared_exclusions != declared_exclusions {
        return Err(format!(
            "the preparer excluded {prepared_exclusions:?}, but the suite declares \
             {declared_exclusions:?}; a page joining or leaving that set changes the \
             measured job and is a suite-revision event"
        ));
    }
    if !root.is_dir() {
        return Err(format!(
            "the preparer reported success but {} is not a directory",
            root.display()
        ));
    }
    // Logged because a collection run's log is where a maintainer looks first
    // when a series moves: the page count and the port revision are the two
    // numbers that say whether the JOB changed, and the digest is what a reader
    // matches an observation to its segment by.
    eprintln!(
        "rue-bench runtime: prepared the {}x corpus for {}: {} page(s) from {} content file(s) \
         and {} bytes of Markdown, template-port revision {}, fixture digest {}",
        identity.scale,
        workload.id,
        description_report.pages,
        identity.content_files,
        identity.content_bytes,
        identity.port_revision,
        identity.fixture_digest
    );

    // Every input class the tools consume, counted together: the Markdown
    // content, the static passthrough (more bytes than the Markdown itself),
    // and the versioned template ports and parity configurations. The digest is
    // the preparer's own fixture digest, which covers all three plus the
    // exclusion set, so no input class can change the measured job without
    // changing the value that segments the series.
    Ok(PreparedFixture {
        recorded: RecordedInput {
            name: FIXTURE_INPUT_NAME.to_string(),
            category: *category,
            description: description.clone(),
            identity_sha256: identity.fixture_digest.clone(),
            files: identity.scaled_content_files + identity.static_files + identity.port_files,
            bytes: identity.scaled_content_bytes + identity.static_bytes + identity.port_bytes,
            provenance: None,
            tree: Some(CorpusTreeProvenance {
                preparer: preparer.clone(),
                preparer_revision: *preparer_revision,
                scale: *scale,
                excluded: prepared_exclusions,
            }),
        },
        path: root,
        description: Some(description_report),
    })
}

/// Measure one fresh process, spawn to exit.
fn run_sample(
    binary: &Path,
    arguments: &[String],
    workdir: &Path,
    oracle: OracleKind,
    destination: &Path,
) -> Result<(RuntimeSample, Vec<u8>), String> {
    let mut command = Command::new(binary);
    command
        .args(arguments)
        .current_dir(workdir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    // The clock starts after every piece of setup and immediately before
    // process creation, and stops once the process has exited and its output
    // has been drained. Nothing else is inside it.
    let started = Instant::now();
    let reaped = spawn_and_reap(&mut command).map_err(|error| format!("the program {error}"))?;
    let process_elapsed_ns = u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX);

    let exit_code = if libc::WIFEXITED(reaped.status) {
        libc::WEXITSTATUS(reaped.status)
    } else if libc::WIFSIGNALED(reaped.status) {
        // Negative so a signal can never be read as a successful exit.
        -libc::WTERMSIG(reaped.status)
    } else {
        -1
    };

    // The digest of what this sample EMITTED, for a workload whose answer is a
    // tree rather than a stream. Computed after the clock has stopped, and
    // stored per sample rather than once, because it is what makes both the
    // determinism check and the oracle's verdict provable from the record for a
    // program whose stdout is empty by design.
    let artifact_sha256 = match oracle {
        OracleKind::GoldenStdout => None,
        OracleKind::SemanticSitePages if exit_code == 0 => {
            Some(crate::digest::sha256_tree(destination)?)
        }
        OracleKind::SemanticSitePages => None,
    };

    let sample = RuntimeSample {
        process_elapsed_ns,
        peak_memory_bytes: reaped.peak_memory_bytes,
        exit_code,
        stdout_bytes: reaped.stdout.len() as u64,
        stdout_sha256: sha256(&reaped.stdout),
        artifact_sha256,
    };
    Ok((sample, reaped.stdout))
}

/// Judge an emitted site through the checked-in comparison the suite names.
///
/// The judgement is the script's, for the reason the fixture's preparation is:
/// ADR-0072 names it the reference implementation, and a second implementation
/// of routing, section membership, ordering, and the semantic oracle would be a
/// second implementation to disagree. What this function contributes is the
/// part a record needs and a script cannot supply — the digests, so a reader of
/// the store can check the verdict against the samples rather than believe it.
fn judge_site(
    options: &Options,
    workload: &RuntimeWorkload,
    prepared: &PreparedFixture,
    samples: &[RuntimeSample],
    trees: &[PathBuf],
    peer_trees: &BTreeMap<String, PathBuf>,
) -> OracleOutcome {
    let kind = workload.oracle.kind;
    let reference = workload.oracle.path.clone();
    let judge_path = options.repo_root.join(&workload.oracle.path);
    // The oracle's own identity, recorded with the observation: the judgement
    // is only as stable as the judge, so a change to it must be visible in the
    // data rather than only in a commit log.
    let reference_sha256 = crate::digest::sha256_file(&judge_path).unwrap_or_default();
    let observed_sha256 = samples
        .first()
        .and_then(|sample| sample.artifact_sha256.clone())
        .unwrap_or_default();
    let deterministic = samples
        .iter()
        .all(|sample| sample.artifact_sha256 == samples[0].artifact_sha256);

    let Some(tree) = trees.first() else {
        return OracleOutcome {
            kind,
            reference,
            reference_sha256,
            observed_sha256,
            verdict: OracleVerdict::Indeterminate,
            deterministic_across_samples: deterministic,
            detail: "no sample emitted a site to judge".to_string(),
        };
    };

    let report = tree.with_extension("judgement.json");
    let mut command = Command::new("python3");
    command
        .arg(&judge_path)
        .arg("judge")
        .arg("--root")
        .arg(&prepared.path)
        .arg("--gazette-out")
        .arg(tree)
        .arg("--scale")
        .arg(
            prepared
                .description
                .as_ref()
                .map(|description| description.identity.scale)
                .unwrap_or(1)
                .to_string(),
        )
        .arg("--out")
        .arg(&report)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (tool, tree) in peer_trees {
        command.arg(format!("--{tool}-out")).arg(tree);
    }
    let output = match command.output() {
        Ok(output) => output,
        Err(error) => {
            return OracleOutcome {
                kind,
                reference,
                reference_sha256,
                observed_sha256,
                verdict: OracleVerdict::Indeterminate,
                deterministic_across_samples: deterministic,
                detail: format!("could not run the oracle: {error}"),
            };
        }
    };
    let detail = String::from_utf8_lossy(&output.stdout)
        .lines()
        .chain(String::from_utf8_lossy(&output.stderr).lines())
        .filter(|line| line.contains("FAIL") || line.contains("mismatch"))
        .take(5)
        .collect::<Vec<_>>()
        .join("; ");
    let verdict = if output.status.success() {
        OracleVerdict::Match
    } else {
        OracleVerdict::Mismatch
    };
    OracleOutcome {
        kind,
        reference,
        reference_sha256,
        observed_sha256,
        verdict,
        deterministic_across_samples: deterministic,
        detail: if verdict == OracleVerdict::Match {
            String::new()
        } else if detail.is_empty() {
            format!("the oracle refused the emitted site ({})", output.status)
        } else {
            detail
        },
    }
}

/// Compare what the program printed with what it was supposed to print.
///
/// Two independent questions, both answered here: did every sample agree with
/// the others, and did they agree with the committed golden. A program that
/// agrees with itself and not the golden is consistently wrong; one that
/// disagrees with itself is wrong in a more alarming way, and the record
/// distinguishes them.
fn judge(workload: &RuntimeWorkload, golden_path: &Path, outputs: &[Vec<u8>]) -> OracleOutcome {
    let kind = workload.oracle.kind;
    let reference = workload.oracle.path.clone();
    let deterministic = outputs.windows(2).all(|pair| pair[0] == pair[1]);
    let observed_sha256 = outputs
        .first()
        .map(|bytes| sha256(bytes))
        .unwrap_or_default();

    let golden = match std::fs::read(golden_path) {
        Ok(bytes) => bytes,
        Err(error) => {
            return OracleOutcome {
                kind,
                reference,
                reference_sha256: String::new(),
                observed_sha256,
                verdict: OracleVerdict::Indeterminate,
                deterministic_across_samples: deterministic,
                detail: format!("could not read {}: {error}", golden_path.display()),
            };
        }
    };
    let reference_sha256 = sha256(&golden);

    let Some(first) = outputs.first() else {
        return OracleOutcome {
            kind,
            reference,
            reference_sha256,
            observed_sha256,
            verdict: OracleVerdict::Indeterminate,
            deterministic_across_samples: deterministic,
            detail: "no sample produced output to judge".to_string(),
        };
    };

    // Every sample is compared, not just the first. Checking one and asserting
    // determinism separately would report a match for a run in which the
    // majority of samples were wrong.
    let mismatched = outputs.iter().position(|output| output != &golden);
    let (verdict, detail) = match mismatched {
        None => (OracleVerdict::Match, String::new()),
        Some(index) => (
            OracleVerdict::Mismatch,
            format!("sample {index}: {}", describe_difference(&golden, first)),
        ),
    };
    OracleOutcome {
        kind,
        reference,
        reference_sha256,
        observed_sha256,
        verdict,
        deterministic_across_samples: deterministic,
        detail,
    }
}

/// A short, bounded description of where two outputs first differ.
///
/// Bounded deliberately: the record is stored forever and a program printing
/// megabytes of wrong output must not write megabytes of diff into the durable
/// store.
fn describe_difference(expected: &[u8], observed: &[u8]) -> String {
    let expected_lines: Vec<&[u8]> = expected.split(|byte| *byte == b'\n').collect();
    let observed_lines: Vec<&[u8]> = observed.split(|byte| *byte == b'\n').collect();
    for (index, (left, right)) in expected_lines.iter().zip(observed_lines.iter()).enumerate() {
        if left != right {
            return format!(
                "line {} expected {:?}, observed {:?}",
                index + 1,
                truncate(left),
                truncate(right)
            );
        }
    }
    format!(
        "expected {} line(s) and {} byte(s), observed {} line(s) and {} byte(s)",
        expected_lines.len(),
        expected.len(),
        observed_lines.len(),
        observed.len()
    )
}

fn truncate(bytes: &[u8]) -> String {
    const LIMIT: usize = 120;
    let text = String::from_utf8_lossy(&bytes[..bytes.len().min(LIMIT)]).into_owned();
    if bytes.len() > LIMIT {
        format!("{text}…")
    } else {
        text
    }
}

/// The compiler's own version string, recorded with every observation.
///
/// Together with the commit this is what makes the series longitudinal: a
/// movement is attributable to a compiler change from the record alone.
fn compiler_version(compiler: &Path) -> Result<String, String> {
    let mut command = Command::new(compiler);
    crate::environment::sanitize_compiler_environment(&mut command);
    let output = command
        .arg("--version")
        .output()
        .map_err(|error| format!("could not run the compiler: {error}"))?;
    if !output.status.success() {
        return Err("the compiler could not report its version".to_string());
    }
    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if text.is_empty() {
        return Err("the compiler reported an empty version".to_string());
    }
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rue_perf_schema::{FixtureDeclaration, InputCategory, OracleDeclaration, OracleKind};

    fn workload() -> RuntimeWorkload {
        RuntimeWorkload {
            id: "wordfreq".to_string(),
            source: "examples/wordfreq/main.rue".to_string(),
            question: "How fast does compiled Rue count words?".to_string(),
            program_args: vec![FIXTURE_ARGUMENT.to_string()],
            fixture: FixtureDeclaration::SeededGenerator {
                category: InputCategory::Recorded,
                generator: fixture::ZIPF_ASCII_TEXT.to_string(),
                generator_revision: fixture::ZIPF_ASCII_TEXT_REVISION,
                seed: 20260813,
                bytes: 4096,
                vocabulary_size: 256,
                file_name: "input.txt".to_string(),
                description: "deterministic ASCII prose".to_string(),
            },
            oracle: OracleDeclaration {
                kind: OracleKind::GoldenStdout,
                path: "performance/fixtures/wordfreq/expected-stdout.txt".to_string(),
            },
        }
    }

    fn golden(directory: &Path, contents: &[u8]) -> PathBuf {
        let path = directory.join("expected-stdout.txt");
        std::fs::write(&path, contents).unwrap();
        path
    }

    #[test]
    fn identical_correct_output_matches() {
        let temporary = tempfile::tempdir().unwrap();
        let path = golden(temporary.path(), b"words=16\n");
        let outputs = vec![b"words=16\n".to_vec(); 5];
        let outcome = judge(&workload(), &path, &outputs);
        assert_eq!(outcome.verdict, OracleVerdict::Match);
        assert!(outcome.deterministic_across_samples);
        assert_eq!(outcome.detail, "");
        assert_eq!(outcome.observed_sha256, outcome.reference_sha256);
    }

    #[test]
    fn wrong_output_is_a_mismatch_naming_the_line() {
        let temporary = tempfile::tempdir().unwrap();
        let path = golden(temporary.path(), b"words=16\ndistinct=6\n");
        let outputs = vec![b"words=16\ndistinct=5\n".to_vec(); 3];
        let outcome = judge(&workload(), &path, &outputs);
        assert_eq!(outcome.verdict, OracleVerdict::Mismatch);
        assert!(outcome.detail.contains("line 2"), "{}", outcome.detail);
        assert!(outcome.detail.contains("distinct=5"), "{}", outcome.detail);
    }

    #[test]
    fn a_single_wrong_sample_fails_the_whole_observation() {
        // Judging only the first sample would report a match for a run in which
        // every later sample was wrong.
        let temporary = tempfile::tempdir().unwrap();
        let path = golden(temporary.path(), b"words=16\n");
        let outputs = vec![
            b"words=16\n".to_vec(),
            b"words=16\n".to_vec(),
            b"words=15\n".to_vec(),
        ];
        let outcome = judge(&workload(), &path, &outputs);
        assert_eq!(outcome.verdict, OracleVerdict::Mismatch);
        assert!(!outcome.deterministic_across_samples);
    }

    #[test]
    fn output_that_varies_between_samples_is_reported_separately_from_correctness() {
        // Both agree with nothing; the record must distinguish "consistently
        // wrong" from "not the same program twice".
        let temporary = tempfile::tempdir().unwrap();
        let path = golden(temporary.path(), b"words=16\n");
        let outputs = vec![b"words=17\n".to_vec(), b"words=18\n".to_vec()];
        let outcome = judge(&workload(), &path, &outputs);
        assert_eq!(outcome.verdict, OracleVerdict::Mismatch);
        assert!(!outcome.deterministic_across_samples);
    }

    #[test]
    fn a_missing_golden_is_indeterminate_rather_than_a_pass() {
        let temporary = tempfile::tempdir().unwrap();
        let outputs = vec![b"words=16\n".to_vec()];
        let outcome = judge(&workload(), &temporary.path().join("absent"), &outputs);
        assert_eq!(outcome.verdict, OracleVerdict::Indeterminate);
        assert!(outcome.reference_sha256.is_empty());
    }

    #[test]
    fn a_run_with_no_output_is_indeterminate_rather_than_a_pass() {
        let temporary = tempfile::tempdir().unwrap();
        let path = golden(temporary.path(), b"words=16\n");
        let outcome = judge(&workload(), &path, &[]);
        assert_eq!(outcome.verdict, OracleVerdict::Indeterminate);
        assert!(outcome.detail.contains("no sample"), "{}", outcome.detail);
    }

    #[test]
    fn a_trailing_newline_difference_is_a_mismatch() {
        // Byte-exact means byte-exact. Trimming here would let a real output
        // change pass unnoticed.
        let temporary = tempfile::tempdir().unwrap();
        let path = golden(temporary.path(), b"words=16\n");
        let outcome = judge(&workload(), &path, &[b"words=16".to_vec()]);
        assert_eq!(outcome.verdict, OracleVerdict::Mismatch);
    }

    #[test]
    fn preparing_a_fixture_records_the_digest_of_the_bytes_written() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("input.txt");
        let prepared = prepare_seeded_fixture(&workload(), path.clone()).expect("prepared");
        let recorded = prepared.recorded;
        let written = std::fs::read(&path).unwrap();
        assert_eq!(recorded.bytes, 4096);
        assert_eq!(written.len(), 4096);
        assert_eq!(recorded.identity_sha256, sha256(&written));
        assert_eq!(recorded.category, InputCategory::Recorded);
        let provenance = recorded.provenance.expect("generated");
        assert_eq!(provenance.seed, 20260813);
        assert_eq!(
            provenance.generator_revision,
            fixture::ZIPF_ASCII_TEXT_REVISION
        );
    }

    #[test]
    fn a_fixture_pinned_to_an_unimplemented_generator_revision_is_refused() {
        // The golden output is a function of the generated bytes, so silently
        // generating a different revision would surface as a baffling oracle
        // mismatch instead of a clear refusal.
        let temporary = tempfile::tempdir().unwrap();
        let mut workload = workload();
        let FixtureDeclaration::SeededGenerator {
            generator_revision, ..
        } = &mut workload.fixture
        else {
            unreachable!("the fixture above is a seeded one")
        };
        *generator_revision = fixture::ZIPF_ASCII_TEXT_REVISION + 1;
        let error =
            prepare_seeded_fixture(&workload, temporary.path().join("input.txt")).unwrap_err();
        assert!(error.contains("revision"), "{error}");
    }

    #[test]
    fn a_fixture_pinned_to_an_unknown_generator_is_refused() {
        let temporary = tempfile::tempdir().unwrap();
        let mut workload = workload();
        let FixtureDeclaration::SeededGenerator { generator, .. } = &mut workload.fixture else {
            unreachable!("the fixture above is a seeded one")
        };
        *generator = "some_future_generator".to_string();
        let error =
            prepare_seeded_fixture(&workload, temporary.path().join("input.txt")).unwrap_err();
        assert!(error.contains("some_future_generator"), "{error}");
    }

    #[test]
    fn a_difference_description_is_bounded() {
        let expected = vec![b'a'; 10_000];
        let observed = vec![b'b'; 10_000];
        let described = describe_difference(&expected, &observed);
        assert!(described.len() < 400, "{described}");
    }
}
