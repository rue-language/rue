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
    FIXTURE_ARGUMENT, FIXTURE_INPUT_NAME, GeneratedProvenance, OracleOutcome, OracleVerdict,
    ProgramIdentity, RUNTIME_RECORD_KIND, RUNTIME_REPORT_SCHEMA_VERSION, RecordedInput,
    RuntimeCompleteness, RuntimeEpoch, RuntimeFailure, RuntimeIdentity, RuntimeManifest,
    RuntimeObservation, RuntimeRegime, RuntimeReport, RuntimeSample, RuntimeSuiteRevision,
    RuntimeWorkload, validate_runtime_report,
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
            other => return Err(format!("unrecognized argument {other:?}")),
        }
    }

    let repo_root = repo_root.unwrap_or_else(|| PathBuf::from("."));
    let std_root = std_root.or_else(|| {
        let candidate = repo_root.join("std");
        candidate.is_dir().then_some(candidate)
    });

    Ok(Options {
        manifest: manifest.unwrap_or_else(|| repo_root.join("performance/runtime.toml")),
        platform: platform.ok_or("runtime requires --platform")?,
        epoch,
        compiler: compiler.ok_or("runtime requires --compiler")?,
        commit: commit.ok_or("runtime requires --commit")?,
        output: output.ok_or("runtime requires --out")?,
        repo_root,
        std_root,
        workdir,
    })
}

/// Run the runtime suite for one platform epoch.
pub fn run() -> Result<u8, String> {
    let options = parse_args()?;
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
    let fixture_path = workdir.join(&workload.fixture.file_name);
    let recorded_fixture = prepare_fixture(workload, &fixture_path).map_err(|detail| {
        RuntimeFailure::FixturePreparationFailed {
            workload: workload.id.clone(),
            detail,
        }
    })?;

    let arguments: Vec<String> = workload
        .program_args
        .iter()
        .map(|argument| {
            if argument == FIXTURE_ARGUMENT {
                fixture_path.display().to_string()
            } else {
                argument.clone()
            }
        })
        .collect();

    let samples_wanted = epoch
        .sampling
        .get(&workload.id)
        .map(|policy| policy.samples)
        .unwrap_or(0);
    let mut samples: Vec<RuntimeSample> = Vec::with_capacity(samples_wanted as usize);
    let mut outputs: Vec<Vec<u8>> = Vec::with_capacity(samples_wanted as usize);
    for index in 0..samples_wanted {
        eprintln!(
            "rue-bench runtime: {} sample {}/{samples_wanted}",
            workload.id,
            index + 1
        );
        match run_sample(&binary, &arguments, workdir) {
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
    let golden_path = options.repo_root.join(&workload.oracle.path);
    let oracle = judge(workload, &golden_path, &outputs);
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
        recorded_inputs: vec![recorded_fixture],
        program,
        oracle,
        samples,
    })
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

/// Generate a workload's fixture and record the identity of the bytes produced.
///
/// The generator's revision is checked against the declaration rather than
/// assumed: the committed golden output is a function of the produced bytes, so
/// a manifest asking for a revision this binary does not implement would
/// produce a confusing oracle mismatch instead of a clear refusal.
fn prepare_fixture(workload: &RuntimeWorkload, path: &Path) -> Result<RecordedInput, String> {
    let declaration = &workload.fixture;
    if declaration.generator != fixture::ZIPF_ASCII_TEXT {
        return Err(format!(
            "this runner implements the {:?} fixture generator, not {:?}",
            fixture::ZIPF_ASCII_TEXT,
            declaration.generator
        ));
    }
    if declaration.generator_revision != fixture::ZIPF_ASCII_TEXT_REVISION {
        return Err(format!(
            "the manifest pins {} revision {}, but this runner implements revision {}",
            declaration.generator,
            declaration.generator_revision,
            fixture::ZIPF_ASCII_TEXT_REVISION
        ));
    }
    let bytes = fixture::generate_zipf_ascii_text(
        declaration.seed,
        declaration.bytes,
        declaration.vocabulary_size,
    );
    std::fs::write(path, &bytes)
        .map_err(|error| format!("could not write {}: {error}", path.display()))?;
    Ok(RecordedInput {
        name: FIXTURE_INPUT_NAME.to_string(),
        category: declaration.category,
        description: declaration.description.clone(),
        // Over the bytes the program will actually read, not over the
        // declaration that asked for them. That is what makes a generator whose
        // output drifted from its declaration visible in the data.
        identity_sha256: sha256(&bytes),
        files: 1,
        bytes: bytes.len() as u64,
        provenance: Some(GeneratedProvenance {
            generator: declaration.generator.clone(),
            generator_revision: declaration.generator_revision,
            seed: declaration.seed,
            vocabulary_size: declaration.vocabulary_size,
        }),
    })
}

/// Measure one fresh process, spawn to exit.
fn run_sample(
    binary: &Path,
    arguments: &[String],
    workdir: &Path,
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

    let sample = RuntimeSample {
        process_elapsed_ns,
        peak_memory_bytes: reaped.peak_memory_bytes,
        exit_code,
        stdout_bytes: reaped.stdout.len() as u64,
        stdout_sha256: sha256(&reaped.stdout),
    };
    Ok((sample, reaped.stdout))
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
    let output = Command::new(compiler)
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
            fixture: FixtureDeclaration {
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
        let recorded = prepare_fixture(&workload(), &path).expect("prepared");
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
        workload.fixture.generator_revision = fixture::ZIPF_ASCII_TEXT_REVISION + 1;
        let error = prepare_fixture(&workload, &temporary.path().join("input.txt")).unwrap_err();
        assert!(error.contains("revision"), "{error}");
    }

    #[test]
    fn a_fixture_pinned_to_an_unknown_generator_is_refused() {
        let temporary = tempfile::tempdir().unwrap();
        let mut workload = workload();
        workload.fixture.generator = "some_future_generator".to_string();
        let error = prepare_fixture(&workload, &temporary.path().join("input.txt")).unwrap_err();
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
