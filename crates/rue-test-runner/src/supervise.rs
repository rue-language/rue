//! One supervision loop for every child process the tree starts.
//!
//! Spawning a test process, draining its pipes so it cannot wedge on a full
//! one, enforcing a wall-clock budget, and tearing down its process group is a
//! single mechanism with a single failure class (RUE-338). It lives here so the
//! specification and CLI harnesses, the `rue test` runner in the compiler
//! driver (ADR-0083 §3), and the differential fuzzer share it rather than
//! keeping loops that drift apart under load.
//!
//! What varies between those consumers is policy, not mechanism, so the
//! variation is parameters:
//!
//! - [`OverflowPolicy`] decides what a stream outgrowing its retention budget
//!   means: a harness failure, a reported verdict, or a truncated capture.
//! - [`Supervisor::extra_capture`] adds pipes beyond stdout and stderr, which
//!   is how the `rue test` runner drains its structured failure channel.
//! - [`Supervisor::reap_group`] adds a post-exit SIGKILL to the child's group,
//!   for a consumer that wants stragglers gone rather than merely bounded.
//! - [`GroupObserver`] hands the consumer the two instants only the supervisor
//!   knows: the child is running under a known process-group id, and that id
//!   has been reaped and may be reused.
//!
//! The caller configures stdout and stderr on its own [`Command`]; a stream it
//! leaves un-piped simply produces an empty capture, which is how the
//! differential harness discards compiler stdout. Stdin belongs to the
//! supervisor, because feeding it is what its writer thread does.

use std::io::{self, Read, Write};
use std::process::{Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use crate::pipe_drain::{PIPE_DRAIN_FINISH_TIMEOUT, PipeDrain, spawn_pipe_drain};
use crate::{configure_process_group, kill_process_group};

/// How often the loop wakes to collect drained output and re-check the child.
const DEFAULT_POLL_INTERVAL: Duration = Duration::from_millis(10);

/// What a capture outgrowing its retention budget means to the consumer.
///
/// The two stopping policies both kill the process group the moment a budget is
/// exceeded; they differ in what the supervisor owes afterwards.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverflowPolicy {
    /// Stop the process group and return at once. The run is a harness failure,
    /// so the retained prefix has no reader and the bounded finish would only
    /// delay the report.
    Fail,
    /// Stop the process group, then complete the bounded finish: the retained
    /// prefixes and byte totals are published with the verdict that names the
    /// overflow.
    Verdict,
    /// Let the process run to its own end. Each capture records that bytes were
    /// discarded, so a consumer can refuse to treat a prefix as a complete
    /// result.
    Truncate,
}

impl OverflowPolicy {
    /// Whether an overflow ends the run rather than being recorded on the
    /// capture.
    fn stops_the_child(self) -> bool {
        matches!(self, Self::Fail | Self::Verdict)
    }
}

/// Which capture a report is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureStream {
    Stdout,
    Stderr,
    /// A pipe added with [`Supervisor::extra_capture`], in the order added.
    Extra(usize),
}

/// The capture that outgrew its budget, and the budget it exceeded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Overflow {
    pub stream: CaptureStream,
    pub budget: usize,
}

/// One drained stream.
#[derive(Debug, Default)]
pub struct Capture {
    /// The retained prefix: at most the configured budget.
    pub bytes: Vec<u8>,
    /// Every byte the process wrote to this stream, budget or no budget.
    pub total: u64,
    /// Whether bytes were read and discarded past the budget, making `bytes` a
    /// prefix rather than the whole stream.
    pub truncated: bool,
}

/// How supervision ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// The child reported its own status.
    Exited,
    /// The wall-clock budget expired and the process group was killed.
    TimedOut,
    /// A capture exceeded its budget under a stopping [`OverflowPolicy`].
    Overflowed(Overflow),
}

/// What one supervised process produced.
#[derive(Debug)]
pub struct SupervisedRun {
    pub outcome: Outcome,
    /// The status the child reported for itself, present whenever it reported
    /// one. A run the supervisor stopped has none: the group was killed, so the
    /// outcome decides the result ahead of any status.
    ///
    /// An overflow noticed only after the child had already exited keeps its
    /// status, because the process still spoke for itself.
    pub status: Option<ExitStatus>,
    pub stdout: Capture,
    pub stderr: Capture,
    /// The extra captures, in the order they were added.
    pub extra: Vec<Capture>,
    /// Wall-clock time from just before the spawn to the end of the loop, so a
    /// consumer reports the process's own duration rather than the teardown's.
    pub duration: Duration,
}

/// Why supervision could not produce an outcome.
///
/// Both variants carry the operating system's error so a consumer working in
/// [`io::Result`] can hand it on unchanged.
#[derive(Debug)]
pub enum SupervisionError {
    /// The child could not be started.
    Spawn(io::Error),
    /// Waiting on a running child failed.
    Wait(io::Error),
}

impl SupervisionError {
    /// The underlying operating-system error.
    pub fn into_io(self) -> io::Error {
        match self {
            Self::Spawn(error) | Self::Wait(error) => error,
        }
    }
}

impl std::fmt::Display for SupervisionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Spawn(error) => write!(formatter, "Failed to spawn process: {error}"),
            Self::Wait(error) => write!(formatter, "Failed to wait for process: {error}"),
        }
    }
}

/// The two instants a consumer may need that only the supervisor observes.
///
/// The default implementations do nothing, so a consumer overrides only the
/// half it cares about.
pub trait GroupObserver {
    /// The child is running and leads the process group `pgid`. Called before
    /// anything can block, so a consumer that must be able to tear the group
    /// down from a signal handler is already able to.
    fn spawned(&mut self, pgid: i32) {
        let _ = pgid;
    }

    /// The child has been reaped and `pgid` may be reused by the operating
    /// system from here on, so a consumer holding it must let it go now.
    fn reaped(&mut self, pgid: i32) {
        let _ = pgid;
    }
}

struct ExtraCapture {
    reader: Box<dyn Read + Send>,
    budget: usize,
}

/// A pipe being drained, with the identity and budget its overflow is reported
/// against.
struct Tracked {
    drain: PipeDrain,
    stream: CaptureStream,
    budget: Option<usize>,
}

impl Tracked {
    fn capture(self) -> Capture {
        Capture {
            total: self.drain.bytes_total(),
            truncated: self.drain.overflowed(),
            bytes: self.drain.into_bytes(),
        }
    }
}

/// One child process, supervised to an [`Outcome`].
///
/// Built from a configured [`Command`] and a wall-clock budget; every other
/// knob has a default that suits a harness running one bounded test process.
pub struct Supervisor<'a> {
    cmd: Command,
    timeout: Duration,
    poll_interval: Duration,
    stdin_input: Option<&'a str>,
    stdout_budget: Option<usize>,
    stderr_budget: Option<usize>,
    extra: Vec<ExtraCapture>,
    overflow: OverflowPolicy,
    reap_group: bool,
    observer: Option<&'a mut dyn GroupObserver>,
}

impl<'a> Supervisor<'a> {
    /// Supervise `cmd`, killing its process group if it runs past `timeout`.
    pub fn new(cmd: Command, timeout: Duration) -> Self {
        Self {
            cmd,
            timeout,
            poll_interval: DEFAULT_POLL_INTERVAL,
            stdin_input: None,
            stdout_budget: None,
            stderr_budget: None,
            extra: Vec::new(),
            overflow: OverflowPolicy::Fail,
            reap_group: false,
            observer: None,
        }
    }

    /// Feed `input` to the child's stdin from a writer thread.
    ///
    /// Without this the child's stdin is an immediate end of file.
    pub fn stdin_input(mut self, input: &'a str) -> Self {
        self.stdin_input = Some(input);
        self
    }

    /// Retain at most `budget` bytes of stdout and of stderr.
    pub fn stream_budget(mut self, budget: usize) -> Self {
        self.stdout_budget = Some(budget);
        self.stderr_budget = Some(budget);
        self
    }

    /// Retain at most `stdout` bytes of stdout and `stderr` bytes of stderr,
    /// for a consumer whose two streams are worth different amounts.
    pub fn stream_budgets(mut self, stdout: usize, stderr: usize) -> Self {
        self.stdout_budget = Some(stdout);
        self.stderr_budget = Some(stderr);
        self
    }

    /// Drain one more pipe alongside stdout and stderr, retaining at most
    /// `budget` bytes of it.
    ///
    /// The reader is drained on its own thread from the moment the child is
    /// running, exactly like the standard streams, and its overflow is reported
    /// as [`CaptureStream::Extra`] with the index it was added at.
    pub fn extra_capture(mut self, reader: impl Read + Send + 'static, budget: usize) -> Self {
        self.extra.push(ExtraCapture {
            reader: Box::new(reader),
            budget,
        });
        self
    }

    /// What an exceeded retention budget means to this consumer.
    pub fn overflow_policy(mut self, policy: OverflowPolicy) -> Self {
        self.overflow = policy;
        self
    }

    /// How often to collect drained output and re-check the child.
    pub fn poll_interval(mut self, interval: Duration) -> Self {
        self.poll_interval = interval;
        self
    }

    /// SIGKILL the child's process group once the child is gone.
    ///
    /// Hygiene rather than containment: the bounded finish already keeps an
    /// escaped descendant from stalling the caller, and this additionally stops
    /// it from outliving the run.
    pub fn reap_group(mut self, reap: bool) -> Self {
        self.reap_group = reap;
        self
    }

    /// Observe the child's process-group id while it is live.
    pub fn observer(mut self, observer: &'a mut dyn GroupObserver) -> Self {
        self.observer = Some(observer);
        self
    }

    /// Run the child to an outcome.
    ///
    /// The stdout and stderr drains start on their own threads immediately
    /// after the spawn, and stdin is written from a third. That concurrency is
    /// the point (RUE-338): a child writing more than a pipe's capacity while
    /// the parent waited for its exit would block in `write` forever, never
    /// report a status, and turn into a manufactured timeout.
    pub fn run(self) -> Result<SupervisedRun, SupervisionError> {
        let Self {
            mut cmd,
            timeout,
            poll_interval,
            stdin_input,
            stdout_budget,
            stderr_budget,
            extra,
            overflow,
            reap_group,
            mut observer,
        } = self;

        configure_process_group(&mut cmd);
        cmd.stdin(if stdin_input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        });

        let start = Instant::now();
        let mut child = cmd.spawn().map_err(SupervisionError::Spawn)?;
        // The child leads its own group, so its pid is its process-group id.
        let pgid = child.id() as i32;
        if let Some(observer) = observer.as_deref_mut() {
            observer.spawned(pgid);
        }

        let mut captures = Vec::with_capacity(2 + extra.len());
        captures.push(Tracked {
            drain: spawn_pipe_drain(child.stdout.take(), stdout_budget),
            stream: CaptureStream::Stdout,
            budget: stdout_budget,
        });
        captures.push(Tracked {
            drain: spawn_pipe_drain(child.stderr.take(), stderr_budget),
            stream: CaptureStream::Stderr,
            budget: stderr_budget,
        });
        for (index, capture) in extra.into_iter().enumerate() {
            captures.push(Tracked {
                drain: spawn_pipe_drain(Some(capture.reader), Some(capture.budget)),
                stream: CaptureStream::Extra(index),
                budget: Some(capture.budget),
            });
        }

        // A program may exit without reading all of its input, so a broken pipe
        // here is not a failure. Dropping the pipe at the end of the closure
        // closes it, which is the child's end of file.
        let stdin_writer = child.stdin.take().map(|mut stdin| {
            let input = stdin_input.unwrap_or_default().to_string();
            std::thread::spawn(move || {
                let _ = stdin.write_all(input.as_bytes());
            })
        });

        let mut outcome = Outcome::Exited;
        let mut status = None;
        loop {
            for tracked in &mut captures {
                tracked.drain.poll();
            }
            if overflow.stops_the_child()
                && let Some(record) = first_overflow(&captures)
            {
                kill_process_group(&mut child);
                outcome = Outcome::Overflowed(record);
                break;
            }

            match child.try_wait() {
                Ok(Some(exited)) => {
                    status = Some(exited);
                    break;
                }
                Ok(None) => {
                    if start.elapsed() >= timeout {
                        kill_process_group(&mut child);
                        outcome = Outcome::TimedOut;
                        break;
                    }
                    std::thread::sleep(poll_interval);
                }
                Err(error) => {
                    // A child we can no longer wait on is a child we can no
                    // longer supervise; kill the group rather than leave it
                    // running behind a returning caller.
                    kill_process_group(&mut child);
                    return Err(SupervisionError::Wait(error));
                }
            }
        }
        let duration = start.elapsed();

        if reap_group {
            reap_process_group(pgid);
        }
        if let Some(observer) = observer.as_deref_mut() {
            observer.reaped(pgid);
        }

        // Under `Fail` the retained prefix has no reader, so the run reports as
        // soon as the group is dead. Every other ending collects what the drain
        // threads still hold, bounded so a descendant that inherited a pipe and
        // never closes it cannot stall the answer.
        if !(overflow == OverflowPolicy::Fail && matches!(outcome, Outcome::Overflowed(_))) {
            for tracked in &mut captures {
                tracked.drain.finish(PIPE_DRAIN_FINISH_TIMEOUT);
            }
        }
        drop(stdin_writer);

        // The budget is enforced by the drain threads, which hand their verdict
        // over a channel, so a process that floods a stream and exits at once
        // can be reaped before the overflow message arrives. Re-reading the
        // flags after the bounded finish is what keeps such a run from
        // reporting as a clean exit whose capture silently covers a prefix.
        if overflow.stops_the_child()
            && outcome == Outcome::Exited
            && let Some(record) = first_overflow(&captures)
        {
            outcome = Outcome::Overflowed(record);
        }

        let mut captures = captures.into_iter();
        let stdout = captures.next().expect("stdout is always tracked").capture();
        let stderr = captures.next().expect("stderr is always tracked").capture();
        Ok(SupervisedRun {
            outcome,
            status,
            stdout,
            stderr,
            extra: captures.map(Tracked::capture).collect(),
            duration,
        })
    }
}

/// The first capture to outgrow its budget, in the order stdout, stderr, then
/// the extra pipes.
fn first_overflow(captures: &[Tracked]) -> Option<Overflow> {
    captures
        .iter()
        .find(|tracked| tracked.drain.overflowed())
        .map(|tracked| Overflow {
            stream: tracked.stream,
            budget: tracked
                .budget
                .expect("a stream can only overflow a budget it was given"),
        })
}

/// Best-effort teardown of anything the child left running in its group.
#[cfg(unix)]
fn reap_process_group(pgid: i32) {
    // SAFETY: a negative pid names the process group led by `pgid`. Failure
    // (an already-empty group) is the ordinary case and carries no obligation.
    unsafe {
        libc::kill(-pgid, libc::SIGKILL);
    }
}

#[cfg(not(unix))]
fn reap_process_group(_pgid: i32) {}

#[cfg(test)]
mod tests {
    use super::*;

    const SECONDS_5: Duration = Duration::from_secs(5);

    fn shell(script: &str) -> Command {
        let mut cmd = Command::new("sh");
        cmd.arg("-c")
            .arg(script)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        cmd
    }

    /// The ordinary ending: the child speaks for itself and both streams are
    /// captured whole.
    #[test]
    fn an_exiting_child_reports_its_own_status_and_output() {
        let run = Supervisor::new(shell("printf out; printf err >&2; exit 7"), SECONDS_5)
            .run()
            .expect("spawn");
        assert_eq!(run.outcome, Outcome::Exited);
        assert_eq!(run.status.and_then(|status| status.code()), Some(7));
        assert_eq!(run.stdout.bytes, b"out");
        assert_eq!(run.stderr.bytes, b"err");
        assert!(!run.stdout.truncated);
        assert_eq!(run.stdout.total, 3);
    }

    /// `Fail` stops the flood as it happens and names the stream that caused
    /// it, so a consumer can report a harness failure rather than a result.
    #[test]
    fn the_fail_policy_stops_an_overflowing_child_without_a_status() {
        let run = Supervisor::new(shell("head -c 200000 /dev/zero"), SECONDS_5)
            .stream_budget(16 * 1024)
            .overflow_policy(OverflowPolicy::Fail)
            .run()
            .expect("spawn");
        assert_eq!(
            run.outcome,
            Outcome::Overflowed(Overflow {
                stream: CaptureStream::Stdout,
                budget: 16 * 1024,
            })
        );
        assert!(run.status.is_none(), "the supervisor stopped the child");
    }

    /// `Verdict` stops the child too, but owes the consumer the prefix and the
    /// true byte count its verdict publishes.
    #[test]
    fn the_verdict_policy_stops_the_child_and_still_collects_the_prefix() {
        let run = Supervisor::new(shell("yes e >&2"), SECONDS_5)
            .stream_budget(8 * 1024)
            .overflow_policy(OverflowPolicy::Verdict)
            .run()
            .expect("spawn");
        assert_eq!(
            run.outcome,
            Outcome::Overflowed(Overflow {
                stream: CaptureStream::Stderr,
                budget: 8 * 1024,
            })
        );
        assert_eq!(run.stderr.bytes.len(), 8 * 1024);
        assert!(run.stderr.truncated);
        assert!(run.stderr.total >= 8 * 1024);
    }

    /// `Truncate` lets the child finish; the capture says it is a prefix, which
    /// is what keeps a consumer from comparing it as a complete result.
    #[test]
    fn the_truncate_policy_lets_the_child_finish_and_marks_the_prefix() {
        let run = Supervisor::new(shell("head -c 200000 /dev/zero; exit 3"), SECONDS_5)
            .stream_budget(16 * 1024)
            .overflow_policy(OverflowPolicy::Truncate)
            .run()
            .expect("spawn");
        assert_eq!(run.outcome, Outcome::Exited);
        assert_eq!(run.status.and_then(|status| status.code()), Some(3));
        assert_eq!(run.stdout.bytes.len(), 16 * 1024);
        assert!(run.stdout.truncated);
        assert_eq!(run.stdout.total, 200_000);
    }

    /// A stream that stays inside its budget is complete under every policy.
    #[test]
    fn a_stream_inside_its_budget_is_never_truncated() {
        let run = Supervisor::new(shell("printf hello"), SECONDS_5)
            .stream_budget(16 * 1024)
            .overflow_policy(OverflowPolicy::Truncate)
            .run()
            .expect("spawn");
        assert_eq!(run.stdout.bytes, b"hello");
        assert!(!run.stdout.truncated);
    }

    /// A process past its budget is killed with its whole group, and the loop
    /// returns rather than waiting for the child's own end.
    #[test]
    fn a_timeout_kills_the_group_and_reports_no_status() {
        let start = Instant::now();
        let run = Supervisor::new(shell("sleep 30"), Duration::from_millis(100))
            .run()
            .expect("spawn");
        assert_eq!(run.outcome, Outcome::TimedOut);
        assert!(run.status.is_none());
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "the timeout must not wait for the child: {:?}",
            start.elapsed()
        );
    }

    /// An extra pipe is drained on equal terms with the standard streams and
    /// reported in the order it was added.
    #[test]
    fn an_extra_capture_is_drained_alongside_the_standard_streams() {
        let run = Supervisor::new(shell("printf ordinary"), SECONDS_5)
            .extra_capture(std::io::Cursor::new(b"extra".to_vec()), 1024)
            .run()
            .expect("spawn");
        assert_eq!(run.stdout.bytes, b"ordinary");
        assert_eq!(run.extra.len(), 1);
        assert_eq!(run.extra[0].bytes, b"extra");
        assert!(!run.extra[0].truncated);
    }

    /// An extra pipe over its budget is reported by its index, so a consumer
    /// with several can say which one flooded — and it stops the child on the
    /// same terms as stdout or stderr would.
    #[test]
    fn an_extra_capture_overflows_under_its_own_index() {
        let run = Supervisor::new(shell("sleep 30"), SECONDS_5)
            .extra_capture(std::io::Cursor::new(vec![b'x'; 64 * 1024]), 1024)
            .overflow_policy(OverflowPolicy::Verdict)
            .run()
            .expect("spawn");
        assert_eq!(
            run.outcome,
            Outcome::Overflowed(Overflow {
                stream: CaptureStream::Extra(0),
                budget: 1024,
            })
        );
        assert!(run.status.is_none(), "the supervisor stopped the child");
        assert_eq!(run.extra[0].bytes.len(), 1024);
    }

    /// The observer sees the group while it is live and is told when the id is
    /// no longer safe to hold.
    #[test]
    fn an_observer_sees_the_group_appear_and_be_reaped() {
        #[derive(Default)]
        struct Recorder {
            spawned: Option<i32>,
            reaped: Option<i32>,
        }
        impl GroupObserver for Recorder {
            fn spawned(&mut self, pgid: i32) {
                self.spawned = Some(pgid);
            }
            fn reaped(&mut self, pgid: i32) {
                assert_eq!(self.spawned, Some(pgid), "reaped before spawned");
                self.reaped = Some(pgid);
            }
        }

        let mut recorder = Recorder::default();
        let run = Supervisor::new(shell("exit 0"), SECONDS_5)
            .observer(&mut recorder)
            .reap_group(true)
            .run()
            .expect("spawn");
        assert_eq!(run.outcome, Outcome::Exited);
        assert!(recorder.spawned.is_some_and(|pgid| pgid > 0));
        assert_eq!(recorder.reaped, recorder.spawned);
    }

    /// Stdin is fed from its own thread, so a large input and a large output
    /// proceed at the same time instead of deadlocking against each other.
    #[test]
    fn stdin_is_written_while_stdout_is_drained() {
        let input = "a".repeat(200_000);
        let mut cmd = Command::new("cat");
        cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
        let run = Supervisor::new(cmd, Duration::from_secs(10))
            .stdin_input(&input)
            .run()
            .expect("spawn");
        assert_eq!(run.outcome, Outcome::Exited);
        assert_eq!(run.stdout.bytes.len(), 200_000);
    }

    /// A descendant holding the write end open must not hold the caller: the
    /// finish is bounded, so the run returns with what it has.
    #[test]
    fn an_inherited_pipe_cannot_stall_the_run() {
        let start = Instant::now();
        let run = Supervisor::new(shell("sleep 5 & printf done"), Duration::from_secs(10))
            .run()
            .expect("spawn");
        assert_eq!(run.outcome, Outcome::Exited);
        assert_eq!(run.stdout.bytes, b"done");
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "the run waited for an inherited pipe: {:?}",
            start.elapsed()
        );
    }

    /// The post-exit reap is what stops a descendant from outliving the run.
    #[test]
    fn the_group_reap_removes_a_descendant_the_child_left_behind() {
        let marker =
            std::env::temp_dir().join(format!("rue-supervise-reap-{}", std::process::id()));
        let _ = std::fs::remove_file(&marker);
        let script = format!(
            "(sleep 1; printf survived > {}) & printf done",
            marker.display()
        );
        let run = Supervisor::new(shell(&script), Duration::from_secs(10))
            .reap_group(true)
            .run()
            .expect("spawn");
        assert_eq!(run.outcome, Outcome::Exited);
        std::thread::sleep(Duration::from_millis(1500));
        assert!(
            !marker.exists(),
            "the reap must kill a descendant the child left running"
        );
    }
}
