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

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Instant;

use rue_perf_schema::{FailureRecord, PhaseAccounting, Sample};

/// Everything needed to measure one sample.
pub struct SampleRequest<'a> {
    /// The compiler binary under measurement.
    pub compiler: &'a Path,
    /// The workload's root source.
    pub source: &'a Path,
    /// Behaviour-affecting compiler arguments, from the epoch.
    pub args: &'a [String],
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
}

/// What measuring one sample produced.
pub enum SampleOutcome {
    /// A sample that ran to completion. It may still be invalid; validation
    /// decides that, not this module.
    Measured(Box<Sample>),
    /// The sample could not be produced, with structured evidence of why.
    Failed(Box<FailureRecord>),
}

/// The compiler's `--benchmark-json` output, of which only the additive
/// partition matters here.
#[derive(serde::Deserialize)]
struct BenchmarkJson {
    phase_accounting: PhaseAccounting,
}

/// Measure one sample.
///
/// Runs `batch_size` compilations and reports their totals as one unit.
/// Summing phase accountings is safe: each addend partitions its own timeline
/// exactly, so the sums partition the concatenation exactly too.
pub fn measure_sample(request: &SampleRequest<'_>) -> SampleOutcome {
    let mut total: Option<PhaseAccounting> = None;
    let mut peak_memory_bytes = 0u64;

    let started = Instant::now();
    for _ in 0..request.batch_size {
        match run_once(request) {
            Ok((accounting, peak)) => {
                peak_memory_bytes = peak_memory_bytes.max(peak);
                total = Some(match total {
                    None => accounting,
                    Some(running) => add_accounting(running, &accounting),
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
    let process_elapsed_ns = u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX);

    let Some(phases) = total else {
        // A zero batch size cannot measure anything. The manifest rejects it,
        // so reaching here means the policy was bypassed.
        return SampleOutcome::Failed(Box::new(FailureRecord::ValidationRejected {
            workload: request.workload.to_string(),
            detail: "batch size of zero measures no compilation".to_string(),
        }));
    };

    let output_binary_bytes = std::fs::metadata(&request.output)
        .map(|metadata| metadata.len())
        .unwrap_or(0);

    SampleOutcome::Measured(Box::new(Sample {
        batch_size: request.batch_size,
        process_elapsed_ns,
        peak_memory_bytes,
        output_binary_bytes,
        phases,
    }))
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
fn run_once(request: &SampleRequest<'_>) -> Result<(PhaseAccounting, u64), String> {
    let mut command = Command::new(request.compiler);
    command
        .arg(request.source)
        .arg("-o")
        .arg(&request.output)
        .args(request.args)
        .arg("--benchmark-json")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(std_root) = request.std_root {
        command.env("RUE_STD_PATH", std_root);
    }

    let mut child = command
        .spawn()
        .map_err(|error| format!("could not spawn the compiler: {error}"))?;

    // Drain both pipes on their own threads. Reading one to completion before
    // the other deadlocks as soon as the unread pipe's buffer fills, and the
    // benchmark JSON is large enough for that to be a live risk.
    let mut stdout_pipe = child.stdout.take().expect("stdout was piped");
    let mut stderr_pipe = child.stderr.take().expect("stderr was piped");
    let (stdout, stderr) = std::thread::scope(|scope| {
        let stderr_reader = scope.spawn(move || {
            let mut buffer = String::new();
            let _ = stderr_pipe.read_to_string(&mut buffer);
            buffer
        });
        let mut stdout = String::new();
        let _ = stdout_pipe.read_to_string(&mut stdout);
        (stdout, stderr_reader.join().unwrap_or_default())
    });

    let peak = wait_for_peak_memory(&child)?;
    // `wait4` above already reaped the child, so `child` must not be waited on
    // again. `Child::drop` does not reap, so letting it fall out of scope is
    // correct; calling `wait()` here would fail with ECHILD.
    drop(child);

    // A workload's exit status belongs to its own program, so a nonzero status
    // is only a failure when nothing parseable came back.
    let parsed: BenchmarkJson = serde_json::from_str(stdout.trim()).map_err(|error| {
        let tail: String = stderr.lines().rev().take(5).collect::<Vec<_>>().join("\n");
        format!("no benchmark JSON on stdout ({error}); stderr tail:\n{tail}")
    })?;

    Ok((parsed.phase_accounting, peak))
}

/// Reap one child and report *its* peak resident memory, in bytes.
///
/// Deliberately `wait4` rather than `getrusage(RUSAGE_CHILDREN)`. The latter
/// reports a maximum across every child this process has ever reaped, so once
/// the largest workload had run, every later workload would inherit its peak
/// and the memory metric would be silently wrong for all but the biggest.
fn wait_for_peak_memory(child: &std::process::Child) -> Result<u64, String> {
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
            "could not reap the compiler: {}",
            std::io::Error::last_os_error()
        ));
    }
    // SAFETY: `wait4` succeeded, so the usage value is initialized.
    let usage = unsafe { usage.assume_init() };
    Ok(max_rss_bytes(usage.ru_maxrss))
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
