//! Measuring one sample: spawn the compiler, time it from outside, and read
//! back the partition it reports from inside.
//!
//! Two clocks meet here. `process_elapsed_ns` is measured by this process
//! around the child, so it includes process startup, output publication, and
//! every other driver cost the user actually waits for. `compiler_root_ns`
//! comes from the child's own accounting and covers only compiler work. Their
//! difference is real and is reported, but it never enters the phase stack.
//!
//! Peak memory is likewise external: `wait4` reports the child's resident high
//! water mark, which includes allocator overhead the compiler's own probe
//! cannot see.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use crate::digest::{sha256_bytes, sha256_file};
use rue_perf_schema::{
    BuildBoundaryEvidence, BuildBoundaryPolicy, CompilerBoundaryEvidence,
    CompilerCriticalPathEvidence, CompilerInputClass, CompilerWork, FailureRecord, PhaseAccounting,
    RunnerBoundaryEvidence, RunnerClockBoundary, Sample, WorkloadShape,
};

static PROCESS_DIRECTORY_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Everything needed to measure one sample.
pub struct SampleRequest<'a> {
    /// The compiler binary under measurement.
    pub compiler: &'a Path,
    /// The workload's root source.
    pub source: &'a Path,
    /// Behaviour-affecting compiler arguments, from the epoch.
    pub args: &'a [String],
    /// Exact target declared independently by the epoch.
    pub target: Option<&'a str>,
    /// Where the compiled binary goes.
    pub output: PathBuf,
    /// Standard-library root, when the workload needs one.
    pub std_root: Option<&'a Path>,
    /// How many compilations make up this sample.
    ///
    /// Short workloads batch so that timer resolution and per-process jitter do
    /// not dominate. The sample records the batch's totals plus this factor,
    /// leaving per-compile figures to be derived — storage keeps what was
    /// measured, not what a reader might want.
    pub batch_size: u32,
    /// The workload this sample belongs to, for failure records.
    pub workload: &'a str,
    /// Which sample this is, for failure records.
    pub sample_index: u32,
    /// Exhaustive source-to-native contract for protocol-v2 observations.
    pub boundary_policy: Option<&'a BuildBoundaryPolicy>,
}

/// What measuring one sample produced.
pub enum SampleOutcome {
    /// A sample that ran to completion. It may still be invalid; validation
    /// decides that, not this module.
    Measured(Box<Sample>),
    /// The sample could not be produced, with structured evidence of why.
    Failed(Box<FailureRecord>),
}

/// The compiler's `--benchmark-json` output.
#[derive(serde::Deserialize)]
struct BenchmarkJson {
    #[serde(
        rename = "schema_version",
        deserialize_with = "deserialize_benchmark_schema_version"
    )]
    // The custom deserializer validates this wire field; the benchmark
    // consumer otherwise has no need to retain the already-checked value.
    _schema_version: u32,
    phase_accounting: PhaseAccounting,
    metadata: BenchmarkMetadata,
    source_metrics: SourceMetrics,
    compiler_work: CompilerWork,
    emitted_output: EmittedOutput,
    #[serde(default)]
    compiler_boundary: Option<CompilerBoundaryEvidence>,
    #[serde(default)]
    critical_path: Option<CompilerCriticalPathEvidence>,
}

const BENCHMARK_JSON_SCHEMA_VERSION: u32 = 17;

fn deserialize_benchmark_schema_version<'de, D>(deserializer: D) -> Result<u32, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::Error;

    let version = <u32 as serde::Deserialize>::deserialize(deserializer)?;
    if version != BENCHMARK_JSON_SCHEMA_VERSION {
        return Err(D::Error::custom(format!(
            "unsupported benchmark JSON schema version {version}; expected {BENCHMARK_JSON_SCHEMA_VERSION}"
        )));
    }
    Ok(version)
}

#[derive(serde::Deserialize)]
struct BenchmarkMetadata {
    target: String,
    compiler_build_profile: String,
}

#[derive(serde::Deserialize)]
struct SourceMetrics {
    files: u64,
    modules: u64,
    bytes: u64,
    lines: u64,
    tokens: u64,
    functions: u64,
}

#[derive(serde::Deserialize)]
struct EmittedOutput {
    size_bytes: u64,
    sha256: String,
}

struct CompletedCompile {
    accounting: PhaseAccounting,
    peak_memory_bytes: u64,
    output_binary_bytes: u64,
    shape: WorkloadShape,
    target: String,
    compiler_build_profile: String,
    compiler_work: CompilerWork,
    process_elapsed_ns: u64,
    output_sha256: String,
    boundary_evidence: Option<BuildBoundaryEvidence>,
}

/// The raw result of one fresh compiler process for the scaling report.
pub struct FreshCompile {
    pub sample: Sample,
    pub shape: WorkloadShape,
    pub target: String,
    pub compiler_build_profile: String,
    pub compiler_work: CompilerWork,
}

/// Measure one sample.
///
/// Runs `batch_size` compilations and reports their totals as one unit.
/// Summing phase accountings is safe: each addend partitions its own timeline
/// exactly, so the sums partition the concatenation exactly too.
pub fn measure_sample(request: &SampleRequest<'_>) -> SampleOutcome {
    let mut total: Option<PhaseAccounting> = None;
    let mut peak_memory_bytes = 0u64;
    let mut process_elapsed_ns = 0u64;
    let mut output_binary_bytes = None;
    let mut output_sha256 = None;
    let mut boundary_evidence = Vec::with_capacity(request.batch_size as usize);

    for _ in 0..request.batch_size {
        match run_once(request) {
            Ok(completed) => {
                peak_memory_bytes = peak_memory_bytes.max(completed.peak_memory_bytes);
                process_elapsed_ns =
                    process_elapsed_ns.saturating_add(completed.process_elapsed_ns);
                match (&output_sha256, output_binary_bytes) {
                    (None, None) => {
                        output_sha256 = Some(completed.output_sha256.clone());
                        output_binary_bytes = Some(completed.output_binary_bytes);
                    }
                    (Some(expected_hash), Some(expected_size))
                        if expected_hash == &completed.output_sha256
                            && expected_size == completed.output_binary_bytes => {}
                    _ => {
                        return SampleOutcome::Failed(Box::new(
                            FailureRecord::ValidationRejected {
                                workload: request.workload.to_string(),
                                detail: "fresh compiler processes produced different native output"
                                    .to_string(),
                            },
                        ));
                    }
                }
                if let Some(evidence) = completed.boundary_evidence {
                    boundary_evidence.push(evidence);
                }
                total = Some(match total {
                    None => completed.accounting,
                    Some(running) => add_accounting(running, &completed.accounting),
                });
            }
            Err(detail) => {
                return SampleOutcome::Failed(Box::new(FailureRecord::WorkloadCrashed {
                    workload: request.workload.to_string(),
                    sample_index: request.sample_index,
                    detail,
                }));
            }
        }
    }
    let Some(phases) = total else {
        // A zero batch size cannot measure anything. The manifest rejects it,
        // so reaching here means the policy was bypassed.
        return SampleOutcome::Failed(Box::new(FailureRecord::ValidationRejected {
            workload: request.workload.to_string(),
            detail: "batch size of zero measures no compilation".to_string(),
        }));
    };

    SampleOutcome::Measured(Box::new(Sample {
        batch_size: request.batch_size,
        process_elapsed_ns,
        peak_memory_bytes,
        output_binary_bytes: output_binary_bytes.unwrap_or(0),
        phases,
        boundary_evidence,
        boundary_processes: Vec::new(),
        boundary_work_processes: Vec::new(),
    }))
}

/// Measure exactly one fresh compiler process and retain its fixture shape.
///
/// Unlike [`measure_sample`], this has no batching path: heavyweight programs
/// need independent samples and uncertainty, not the startup probe's timer
/// amplification policy.
pub fn measure_fresh_compile(request: &SampleRequest<'_>) -> Result<FreshCompile, String> {
    if request.batch_size != 1 {
        return Err("a scaling sample must launch exactly one compiler process".to_string());
    }
    let completed = run_once(request)?;
    Ok(FreshCompile {
        sample: Sample {
            batch_size: 1,
            process_elapsed_ns: completed.process_elapsed_ns,
            peak_memory_bytes: completed.peak_memory_bytes,
            output_binary_bytes: completed.output_binary_bytes,
            phases: completed.accounting,
            boundary_evidence: completed.boundary_evidence.into_iter().collect(),
            boundary_processes: Vec::new(),
            boundary_work_processes: Vec::new(),
        },
        shape: completed.shape,
        target: completed.target,
        compiler_build_profile: completed.compiler_build_profile,
        compiler_work: completed.compiler_work,
    })
}

/// Add two phase partitions band by band.
///
/// Saturating throughout: a corrupt addend must not panic the runner. Any
/// resulting inconsistency is caught by validation as an invariant violation,
/// which is evidence rather than a crash.
fn add_accounting(mut running: PhaseAccounting, next: &PhaseAccounting) -> PhaseAccounting {
    for (phase, ns) in &next.phase_ns {
        let entry = running.phase_ns.entry(*phase).or_insert(0);
        *entry = entry.saturating_add(*ns);
    }
    running.mixed_parallel_ns = running
        .mixed_parallel_ns
        .saturating_add(next.mixed_parallel_ns);
    running.unattributed_ns = running.unattributed_ns.saturating_add(next.unattributed_ns);
    running.compiler_root_ns = running
        .compiler_root_ns
        .saturating_add(next.compiler_root_ns);
    running
}

/// Run the compiler once, returning its partition and externally measured peak
/// resident memory.
fn run_once(request: &SampleRequest<'_>) -> Result<CompletedCompile, String> {
    let sequence = PROCESS_DIRECTORY_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let base = request
        .output
        .parent()
        .ok_or_else(|| "benchmark output has no parent directory".to_string())?;
    let process_root = base.join(format!(".fresh-compiler-{}-{sequence}", std::process::id()));
    let state_directory = process_root.join("state");
    let output_directory = process_root.join("output");
    fs::create_dir(&process_root)
        .map_err(|error| format!("could not create fresh process directory: {error}"))?;
    fs::create_dir(&state_directory)
        .map_err(|error| format!("could not create fresh state directory: {error}"))?;
    fs::create_dir(&output_directory)
        .map_err(|error| format!("could not create fresh output directory: {error}"))?;
    let output_path = output_directory.join(
        request
            .output
            .file_name()
            .unwrap_or_else(|| std::ffi::OsStr::new("artifact")),
    );
    let compiler_binary_sha256 = sha256_file(request.compiler)?;

    let mut command = Command::new(request.compiler);
    crate::environment::sanitize_compiler_environment(&mut command);
    command
        .arg(request.source)
        .arg("-o")
        .arg(&output_path)
        .args(request.args)
        .arg("--benchmark-json")
        .env("RUE_BENCH_STATE_DIR", &state_directory)
        .env_remove("RUE_DAEMON_ENDPOINT")
        .env_remove("RUE_RETAINED_SESSION")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(target) = request.target {
        command.arg("--target").arg(target);
    }
    if let Some(std_root) = request.std_root {
        command.env("RUE_STD_PATH", std_root);
    }

    // The external clock starts only after all setup and immediately before
    // process creation. It stops after successful exit and independent native
    // output verification below.
    let started = Instant::now();
    let reaped = spawn_and_reap(&mut command).map_err(|error| format!("the compiler {error}"))?;
    let stdout = String::from_utf8_lossy(&reaped.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&reaped.stderr).into_owned();
    let peak = reaped.peak_memory_bytes;

    let status = reaped.status;
    if !libc::WIFEXITED(status) || libc::WEXITSTATUS(status) != 0 {
        let tail: String = stderr.lines().rev().take(5).collect::<Vec<_>>().join("\n");
        return Err(format!(
            "compiler exited unsuccessfully (status {status}); stderr tail:\n{tail}"
        ));
    }

    let parsed: BenchmarkJson = serde_json::from_str(stdout.trim()).map_err(|error| {
        let tail: String = stderr.lines().rev().take(5).collect::<Vec<_>>().join("\n");
        format!("no benchmark JSON on stdout ({error}); stderr tail:\n{tail}")
    })?;
    let output_bytes = fs::read(&output_path)
        .map_err(|error| format!("could not read compiler output for verification: {error}"))?;
    if output_bytes.is_empty()
        || !native_output_matches_target(&output_bytes, &parsed.metadata.target)
    {
        return Err(format!(
            "compiler output is not a nonempty native executable for {}",
            parsed.metadata.target
        ));
    }
    let output_sha256 = sha256_bytes(&output_bytes);
    let output_binary_bytes = u64::try_from(output_bytes.len()).unwrap_or(u64::MAX);
    if parsed.emitted_output.sha256 != output_sha256
        || parsed.emitted_output.size_bytes != output_binary_bytes
    {
        return Err("compiler and runner disagree about emitted output bytes".to_string());
    }
    if !directory_contains_only(&output_directory, &output_path)? {
        return Err(
            "compiler produced an undeclared artifact in the fresh output directory".to_string(),
        );
    }
    let process_elapsed_ns = u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX);

    let boundary_evidence = match request.boundary_policy {
        None => None,
        Some(policy) => {
            let compiler = parsed.compiler_boundary.clone().ok_or_else(|| {
                "compiler omitted protocol-v2 build-boundary evidence".to_string()
            })?;
            let critical_path = parsed
                .critical_path
                .clone()
                .ok_or_else(|| "compiler omitted protocol-v2 critical-path evidence".to_string())?;
            verify_input_provenance(request, &compiler)?;
            let evidence = BuildBoundaryEvidence {
                runner: RunnerBoundaryEvidence {
                    compiler_binary_sha256,
                    // Both directories were created successfully immediately
                    // before the clock and process spawn. The output directory
                    // is also checked above to contain only the verified
                    // requested artifact after exit.
                    fresh_state_directory: true,
                    fresh_output_directory: true,
                    daemon_endpoint_supplied: false,
                    retained_session_handle_supplied: false,
                    clock_boundary:
                        RunnerClockBoundary::MonotonicPreSpawnThroughExitAndOutputVerification,
                    successful_exit: true,
                    native_output_verified: true,
                    output_sha256: output_sha256.clone(),
                    output_size_bytes: output_binary_bytes,
                },
                compiler,
                critical_path,
                compiler_work: parsed.compiler_work.clone(),
            };
            let expected_target = request.target.ok_or_else(|| {
                "boundary policy has no independently declared target".to_string()
            })?;
            evidence
                .validate_current_producer_against(policy, expected_target)
                .map_err(|detail| format!("build-boundary evidence rejected: {detail}"))?;
            Some(evidence)
        }
    };

    Ok(CompletedCompile {
        accounting: parsed.phase_accounting,
        peak_memory_bytes: peak,
        output_binary_bytes,
        shape: WorkloadShape {
            files: parsed.source_metrics.files,
            modules: parsed.source_metrics.modules,
            bytes: parsed.source_metrics.bytes,
            lines: parsed.source_metrics.lines,
            tokens: parsed.source_metrics.tokens,
            functions: parsed.source_metrics.functions,
        },
        target: parsed.metadata.target,
        compiler_build_profile: parsed.metadata.compiler_build_profile,
        compiler_work: parsed.compiler_work,
        process_elapsed_ns,
        output_sha256,
        boundary_evidence,
    })
}

/// One child process, run to completion with its output and peak RSS.
pub(crate) struct ReapedChild {
    /// Everything the child wrote to stdout.
    pub stdout: Vec<u8>,
    /// Everything it wrote to stderr.
    pub stderr: Vec<u8>,
    /// Its `wait4` status word.
    pub status: libc::c_int,
    /// Its peak resident set size, in bytes.
    pub peak_memory_bytes: u64,
}

/// Spawn a prepared command, drain its pipes, and reap it.
///
/// The caller owns the clock: the compiler path stops timing only after it has
/// verified the emitted binary, while a runtime sample stops at exit. Sharing
/// the spawn instead of the timing keeps both honest about their own boundary
/// while there is exactly one implementation of the pipe-draining and `wait4`
/// dance below.
///
/// Bytes, not `String`: a measured program's output is judged by digest and may
/// not be UTF-8 at all. The compiler path converts afterwards, which moves that
/// validation and one copy out of the reader threads and into the caller's
/// measured window — sub-millisecond against samples measured in hundreds of
/// milliseconds, and far below the regime's dispersion, but a real change
/// rather than a byte-for-byte preserving refactor.
///
/// `stdout` and `stderr` must already be piped.
pub(crate) fn spawn_and_reap(command: &mut Command) -> Result<ReapedChild, String> {
    let mut child = command
        .spawn()
        .map_err(|error| format!("could not be spawned: {error}"))?;

    // Drain both pipes on their own threads. Reading one to completion before
    // the other deadlocks as soon as the unread pipe's buffer fills, and both
    // benchmark JSON and program output are large enough for that to be a live
    // risk.
    let mut stdout_pipe = child.stdout.take().expect("stdout was piped");
    let mut stderr_pipe = child.stderr.take().expect("stderr was piped");
    let (stdout, stderr, reaped) = std::thread::scope(|scope| {
        let stdout_reader = scope.spawn(move || {
            let mut buffer = Vec::new();
            let _ = stdout_pipe.read_to_end(&mut buffer);
            buffer
        });
        let stderr_reader = scope.spawn(move || {
            let mut buffer = Vec::new();
            let _ = stderr_pipe.read_to_end(&mut buffer);
            buffer
        });
        let reaped = wait_for_peak_memory(&child);
        (
            stdout_reader.join().unwrap_or_default(),
            stderr_reader.join().unwrap_or_default(),
            reaped,
        )
    });
    // `wait4` above already reaped the child, so `child` must not be waited on
    // again. `Child::drop` does not reap, so letting it fall out of scope is
    // correct; calling `wait()` here would fail with ECHILD.
    drop(child);

    let (peak_memory_bytes, status) = reaped?;
    Ok(ReapedChild {
        stdout,
        stderr,
        status,
        peak_memory_bytes,
    })
}

/// Reap one child and report *its* peak resident memory, in bytes.
///
/// Deliberately `wait4` rather than `getrusage(RUSAGE_CHILDREN)`. The latter
/// reports a maximum across every child this process has ever reaped, so once
/// the largest workload had run, every later workload would inherit its peak
/// and the memory metric would be silently wrong for all but the biggest.
fn wait_for_peak_memory(child: &std::process::Child) -> Result<(u64, libc::c_int), String> {
    let mut status: libc::c_int = 0;
    let mut usage = std::mem::MaybeUninit::<libc::rusage>::zeroed();
    // SAFETY: `status` and `usage` are properly sized and aligned for `wait4`,
    // which only writes through the pointers. The pid is this process's own
    // un-reaped child, so it is valid to wait on exactly once.
    let reaped = unsafe {
        libc::wait4(
            child.id() as libc::pid_t,
            &mut status,
            0,
            usage.as_mut_ptr(),
        )
    };
    if reaped < 0 {
        return Err(format!(
            "could not be reaped: {}",
            std::io::Error::last_os_error()
        ));
    }
    // SAFETY: `wait4` succeeded, so the usage value is initialized.
    let usage = unsafe { usage.assume_init() };
    Ok((max_rss_bytes(usage.ru_maxrss), status))
}

fn directory_contains_only(directory: &Path, expected: &Path) -> Result<bool, String> {
    let entries = fs::read_dir(directory)
        .map_err(|error| format!("could not inspect {}: {error}", directory.display()))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("could not inspect {}: {error}", directory.display()))?;
    Ok(entries.len() == 1 && entries[0].path() == expected)
}

fn native_output_matches_target(bytes: &[u8], target: &str) -> bool {
    if target.contains("linux") {
        bytes.starts_with(b"\x7fELF")
    } else if target.contains("macos") || target.contains("darwin") {
        bytes.starts_with(&[0xcf, 0xfa, 0xed, 0xfe])
    } else {
        false
    }
}

fn verify_input_provenance(
    request: &SampleRequest<'_>,
    compiler: &CompilerBoundaryEvidence,
) -> Result<(), String> {
    let workload_root = request
        .source
        .parent()
        .ok_or_else(|| "workload source has no parent directory".to_string())?;
    for input in &compiler.accepted_inputs {
        let path = match input.class {
            CompilerInputClass::WorkloadSource => {
                let logical = Path::new(&input.logical_identity);
                if logical.is_absolute() {
                    return Err("compiler reported an absolute workload identity".to_string());
                }
                workload_root.join(logical)
            }
            CompilerInputClass::TrustedStandardLibrarySource => {
                let relative = input
                    .logical_identity
                    .strip_prefix('\0')
                    .and_then(|identity| identity.strip_prefix("rue-std/"))
                    .ok_or_else(|| {
                        "compiler reported a malformed trusted-standard-library identity"
                            .to_string()
                    })?;
                request
                    .std_root
                    .ok_or_else(|| {
                        "compiler used std but the runner supplied no std root".to_string()
                    })?
                    .join(relative)
            }
        };
        let observed = sha256_file(&path)?;
        if observed != input.sha256 {
            return Err(format!(
                "compiler input evidence for {:?} disagrees with {}",
                input.logical_identity,
                path.display()
            ));
        }
    }
    Ok(())
}

/// Normalize `ru_maxrss` to bytes.
///
/// Darwin reports bytes; Linux reports kilobytes. Getting this wrong is a
/// factor-of-1024 error that looks plausible on a chart, so it is separated out
/// and tested rather than inlined.
fn max_rss_bytes(ru_maxrss: libc::c_long) -> u64 {
    let value = ru_maxrss.max(0) as u64;
    if cfg!(target_os = "macos") {
        value
    } else {
        value.saturating_mul(1024)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use rue_perf_schema::Phase;

    use super::*;

    #[test]
    fn benchmark_json_schema_16_is_rejected() {
        let error = serde_json::from_str::<BenchmarkJson>(r#"{"schema_version":16}"#)
            .err()
            .expect("retired benchmark JSON schema must be rejected");
        assert!(error.to_string().contains("expected 17"));
    }

    #[test]
    fn benchmark_json_schema_17_is_accepted_by_the_parser() {
        let phase_accounting = PhaseAccounting {
            phase_ns: BTreeMap::new(),
            mixed_parallel_ns: 0,
            unattributed_ns: 0,
            compiler_root_ns: 0,
        };
        let value = serde_json::json!({
            "schema_version": 17,
            "phase_accounting": phase_accounting,
            "metadata": {"target": "x86_64-linux", "compiler_build_profile": "test"},
            "source_metrics": {"files": 0, "modules": 0, "bytes": 0, "lines": 0, "tokens": 0, "functions": 0},
            "compiler_work": CompilerWork::default(),
            "emitted_output": {"size_bytes": 0, "sha256": ""}
        });
        let parsed: BenchmarkJson = serde_json::from_value(value).expect("schema 17 parses");
        assert_eq!(parsed._schema_version, BENCHMARK_JSON_SCHEMA_VERSION);
    }

    fn accounting(root_ns: u64, semantic_ns: u64, unattributed_ns: u64) -> PhaseAccounting {
        let mut phase_ns: BTreeMap<Phase, u64> =
            Phase::ALL.into_iter().map(|phase| (phase, 0)).collect();
        phase_ns.insert(Phase::SemanticAnalysis, semantic_ns);
        PhaseAccounting {
            phase_ns,
            mixed_parallel_ns: 0,
            unattributed_ns,
            compiler_root_ns: root_ns,
        }
    }

    #[test]
    fn summing_partitions_preserves_the_invariant() {
        // This is why a batch may be reported as one sample: each compilation
        // partitions its own timeline exactly, so their sums partition the
        // concatenation exactly.
        let first = accounting(100, 60, 40);
        let second = accounting(250, 200, 50);
        assert!(first.holds() && second.holds());

        let total = add_accounting(first, &second);
        assert!(total.holds(), "{total:?}");
        assert_eq!(total.compiler_root_ns, 350);
        assert_eq!(total.phase_ns[&Phase::SemanticAnalysis], 260);
        assert_eq!(total.unattributed_ns, 90);
    }

    #[test]
    fn summing_keeps_every_published_phase_present() {
        let total = add_accounting(accounting(10, 10, 0), &accounting(10, 10, 0));
        assert!(
            total.missing_phases().is_empty(),
            "a summed partition must still describe the whole taxonomy"
        );
    }

    #[test]
    fn a_corrupt_addend_saturates_rather_than_panicking() {
        // Validation reports the resulting mismatch as an invariant violation.
        // Overflowing here would abort collection and destroy the evidence.
        let huge = accounting(u64::MAX, u64::MAX, 0);
        let total = add_accounting(accounting(10, 10, 0), &huge);
        assert_eq!(total.compiler_root_ns, u64::MAX);
    }

    #[test]
    fn max_rss_is_normalized_to_bytes_per_platform() {
        // A factor-of-1024 mistake here looks entirely plausible on a chart,
        // which is why the conversion is tested rather than trusted.
        let expected: u64 = if cfg!(target_os = "macos") {
            4096
        } else {
            4096 * 1024
        };
        assert_eq!(max_rss_bytes(4096), expected);
    }

    #[test]
    fn a_negative_max_rss_reads_as_zero_rather_than_wrapping() {
        assert_eq!(max_rss_bytes(-1), 0);
    }
}
