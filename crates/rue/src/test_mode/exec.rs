//! One test, one process (ADR-0083 §3).
//!
//! The exec contract this implements is the image's published interface, fixed
//! by RUE-1917 and restated in `docs/process/test-events.md`: `argv` is
//! `["rue-test", "<ordinal as 16 lowercase hex digits>"]`, `envp` is exactly
//! `["RUE_TEST=1"]`, the working directory is a fresh private scratch
//! directory, stdin is an immediate EOF, and descriptor 3 is the write end of
//! the structured failure channel. Those are contract values, not conveniences:
//! ADR-0083 §3 pins them because the loader lays the real strings on the
//! initial process stack, so their sizes are stack consumption no later pointer
//! swap can undo — which is what will make a keyed configuration's stack
//! consumption deterministic when the deferred verdict cache (§6) needs it.
//!
//! The supervision mechanics — the process group, the SIGKILL on expiry, the
//! bounded concurrent drains, the post-exit group kill — are
//! `rue_test_runner::supervise`'s and are driven from there rather than
//! reimplemented, so the harnesses and the product runner cannot drift on the
//! deadlock class RUE-338 closed. This module supplies the policy that is this
//! runner's own: the failure channel as an extra capture, a flood as a verdict
//! rather than a runner error, and the live-group registry the signal handler
//! walks.

use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicI32, Ordering};
use std::time::Duration;

use rue_test_runner::supervise::{
    GroupObserver, Outcome, OverflowPolicy, SupervisionError, Supervisor,
};

use super::verdict::{
    CaptureStream, ChannelFrames, Classification, Observation, Overflow, Supervision, classify,
};

/// The failure channel's descriptor, pinned by the ADR-0083 §3 exec contract
/// and by `crates/rue-runtime/src/test_channel.rs`, which writes to it.
const CHANNEL_FD: i32 = 3;

/// `argv[0]` every test process observes. Constant by contract: the image path
/// varies per run and the test-visible inventory must not.
const LOGICAL_ARGV0: &str = "rue-test";

/// The one environment entry a test process inherits.
const TEST_ENV_VAR: &str = "RUE_TEST";
const TEST_ENV_VALUE: &str = "1";

/// Default retention budget for each of stdout and stderr.
pub(crate) const DEFAULT_STREAM_BUDGET: usize = 1024 * 1024;

/// The failure channel's own budget, separate from the streams' by design: a
/// test that floods stdout must not be able to truncate its own failure record
/// (ADR-0083 §2).
pub(crate) const CHANNEL_BUDGET: usize = 256 * 1024;

/// How often the supervision loop wakes to poll the drains and the child.
const POLL_INTERVAL: Duration = Duration::from_millis(5);

/// What one test process produced, as the event builder needs it.
pub(crate) struct Execution {
    pub(crate) classification: Classification,
    pub(crate) exit_code: Option<i32>,
    pub(crate) signal: Option<i32>,
    pub(crate) frames: ChannelFrames,
    pub(crate) stdout: Vec<u8>,
    pub(crate) stdout_total: u64,
    pub(crate) stderr: Vec<u8>,
    pub(crate) stderr_total: u64,
    pub(crate) duration: Duration,
    pub(crate) scratch_dir: PathBuf,
}

/// Everything one dispatch needs.
pub(crate) struct Dispatch<'a> {
    pub(crate) image: &'a Path,
    /// The run's private directory, which every scratch directory sits inside.
    pub(crate) run_root: &'a Path,
    pub(crate) ordinal: u32,
    pub(crate) seed: u64,
    pub(crate) timeout: Duration,
    pub(crate) stream_budget: usize,
    /// A watch cycle's run cancellation, so a child that starts in the instant
    /// the cycle is abandoned is killed by the observer that registers it
    /// rather than running to its own timeout (RUE-2023).
    pub(crate) cancellation: Option<&'a RunCancellation>,
}

/// Render a selector exactly as the dispatcher parses it: sixteen lowercase
/// hex digits, fixed width (ADR-0083 §3).
pub(crate) fn selector(ordinal: u32) -> String {
    format!("{:016x}", u64::from(ordinal))
}

/// The private directory one run owns, holding its image and every scratch
/// directory.
///
/// The process id is what makes it private. Two runs launched with the same
/// explicit `--seed` — a repro next to the run that produced it, or two CLI
/// cases in a parallel suite — would otherwise name the same scratch
/// directories, and one run's fresh-directory setup would delete the other
/// run's working directory out from under a live test.
/// `cycle` is `Some` under `rue test --watch`, where one process runs many
/// times: without it, cycle N+1 would delete the scratch directory a failing
/// test in cycle N deliberately retained (RUE-2023). A one-shot run passes
/// `None` and keeps the name it always had.
pub(crate) fn run_root(seed: u64, cycle: Option<u64>) -> PathBuf {
    let pid = std::process::id();
    let name = match cycle {
        Some(cycle) => format!("rue-test-{seed}-{pid}-c{cycle}"),
        None => format!("rue-test-{seed}-{pid}"),
    };
    std::env::temp_dir().join(name)
}

/// The scratch directory one test runs in.
///
/// Named from the seed and the ordinal so a retained directory can be tied back
/// to the run and the test that produced it from the event stream alone. It is
/// removed first when a stale one is present: "fresh" is the contract, and a
/// leftover from an interrupted earlier run would otherwise be a test's
/// starting state.
pub(crate) fn scratch_path(run_root: &Path, seed: u64, ordinal: u32) -> PathBuf {
    run_root.join(format!("rue-test-{seed}-{ordinal}"))
}

/// Holds descriptor 3 open for the life of the process so nothing else can be
/// allocated there.
///
/// `None` means this static holds nothing: either the descriptor was already
/// occupied when we looked — which establishes the same invariant, by someone
/// else's ownership — or claiming it failed, which is best-effort and leaves
/// the runner exactly as correct as it was before the reservation existed.
static CHANNEL_RESERVATION: OnceLock<Option<OwnedFd>> = OnceLock::new();

/// Claim descriptor 3 before any test is spawned.
///
/// The child's `pre_exec` puts the channel's write end on descriptor 3, but a
/// `dup2` onto 3 silently replaces whatever the *parent* had there — and the
/// parent does not control descriptor 3 on its own. `Command::spawn` opens its
/// own close-on-exec pipe at spawn time, on the lowest free descriptors, to
/// report a failed `exec` back to the parent. This runner spawns from several
/// threads at once, so if that pipe's write end lands on 3 for one spawn, that
/// child's `dup2` destroys it: an `exec` failure would then be written into our
/// failure channel as a malformed frame while the parent read EOF from the real
/// pipe and concluded the spawn had succeeded.
///
/// Pinning a placeholder on 3 for the whole process closes that window at its
/// source. No later `open` or `pipe` can be given descriptor 3 while it is
/// occupied, so the child's `dup2` always replaces this placeholder and never a
/// live descriptor of the runner's own.
///
/// Idempotent, and safe to call from anywhere: the first caller wins and the
/// descriptor is never released.
pub(crate) fn reserve_channel_descriptor() {
    CHANNEL_RESERVATION.get_or_init(|| {
        // SAFETY: a bare query of a descriptor's flags.
        if unsafe { libc::fcntl(CHANNEL_FD, libc::F_GETFD) } >= 0 {
            // Already open — our own parent handed us something on 3. Leave it
            // alone: it is not ours to close, and an occupied descriptor is
            // exactly the invariant this function exists to establish.
            return None;
        }
        let placeholder = std::fs::File::open("/dev/null").ok()?;
        if placeholder.as_raw_fd() == CHANNEL_FD {
            // `open` was handed 3 directly, which is the common case. Rust
            // opens with `O_CLOEXEC`, so it is already close-on-exec.
            return Some(OwnedFd::from(placeholder));
        }
        // SAFETY: both descriptors are open; `dup2` closes nothing we own,
        // because the branch above proved 3 was free.
        if unsafe { libc::dup2(placeholder.as_raw_fd(), CHANNEL_FD) } < 0 {
            return None;
        }
        // `dup2` clears close-on-exec on the new descriptor. Restore it so the
        // placeholder never leaks into an unrelated child — the test image's
        // own descriptor 3 is installed by `pre_exec`, not inherited from here.
        // SAFETY: descriptor 3 is now open and owned by this process.
        unsafe {
            libc::fcntl(CHANNEL_FD, libc::F_SETFD, libc::FD_CLOEXEC);
        }
        // Dropping `placeholder` closes its original descriptor; 3 survives as
        // the duplicate, owned from here on by this static.
        // SAFETY: `dup2` succeeded, so descriptor 3 is open and unowned.
        Some(unsafe { OwnedFd::from_raw_fd(CHANNEL_FD) })
    });
}

/// Slots in the live-group registry.
///
/// One slot per concurrently running test, with room to spare: `--jobs` is
/// capped at `MAX_EXPLICIT_JOBS` (256) in `main.rs`, so a run cannot have more
/// children alive than that. The registry is a fixed array because the signal
/// handler walks it, and a handler may neither allocate nor take a lock.
const MAX_LIVE_GROUPS: usize = 1024;

/// The process-group ids of the tests running right now; 0 marks a free slot.
///
/// Async-signal-safe storage on purpose. Every child leads its own process
/// group, so the terminal's SIGINT reaches the runner and nothing else; without
/// this registry a Ctrl-C would leave the images running with nobody left to
/// enforce their timeout.
static LIVE_GROUPS: [AtomicI32; MAX_LIVE_GROUPS] = [const { AtomicI32::new(0) }; MAX_LIVE_GROUPS];

/// Publish a live process group to the signal handler.
///
/// `None` means the registry was full, which is not a failure: the test still
/// runs, and only the handler's best-effort teardown is missed. The slot index
/// comes back so the entry can be withdrawn by exactly its owner.
fn register_group(pgid: i32) -> Option<usize> {
    register_in(&LIVE_GROUPS, pgid)
}

/// Withdraw a group once its child is reaped.
///
/// Prompt withdrawal is what keeps the handler honest: a pid is reusable the
/// moment its group is empty, and a stale entry would aim a SIGKILL at whatever
/// unrelated process inherited the number.
fn unregister_group(slot: usize, pgid: i32) {
    unregister_in(&LIVE_GROUPS, slot, pgid);
}

/// SIGKILL every registered process group.
///
/// This is what the handler runs, and the tests exercise the same body over a
/// registry of their own — the only way to observe it without signalling the
/// test binary itself.
pub(crate) fn kill_registered_groups() {
    kill_groups_in(&LIVE_GROUPS);
}

/// The authority that ends a watch cycle's execution phase (RUE-2023).
///
/// A `rue test --watch` cycle is abandonable at every point, including while
/// tests are running: an edit kills the live process groups and the cycle
/// publishes `run_canceled` rather than verdicts about source the user has
/// already replaced. The flag and the kill are one operation because a worker
/// between two tests must be stopped by the flag while the test already
/// running must be stopped by the signal.
///
/// A one-shot run holds none of these: nothing outside the run can end it, and
/// a terminal's Ctrl-C is [`install_signal_forwarding`]'s.
#[derive(Clone, Default)]
pub(crate) struct RunCancellation {
    canceled: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl RunCancellation {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Stop the run: publish the flag, then kill every live group.
    ///
    /// That order is what makes the two halves cover each other. A worker
    /// about to spawn observes the flag and never spawns; a worker that
    /// spawned before the flag was published has already registered its group
    /// and the sweep reaches it; and a worker that registered *between* the
    /// flag and the sweep re-reads the flag from `LiveGroup::spawned` and kills
    /// its own child. `SeqCst` on both sides is what leaves no fourth case.
    pub(crate) fn cancel(&self) {
        self.canceled.store(true, Ordering::SeqCst);
        kill_registered_groups();
    }

    pub(crate) fn is_canceled(&self) -> bool {
        self.canceled.load(Ordering::SeqCst)
    }
}

/// The registry operations, over the array rather than the static, so a test
/// can drive them without publishing pids into the process-wide registry the
/// signal handler reads.
fn register_in(groups: &[AtomicI32], pgid: i32) -> Option<usize> {
    groups.iter().position(|slot| {
        slot.compare_exchange(0, pgid, Ordering::AcqRel, Ordering::Relaxed)
            .is_ok()
    })
}

fn unregister_in(groups: &[AtomicI32], slot: usize, pgid: i32) {
    if let Some(entry) = groups.get(slot) {
        let _ = entry.compare_exchange(pgid, 0, Ordering::AcqRel, Ordering::Relaxed);
    }
}

fn kill_groups_in(groups: &[AtomicI32]) {
    for slot in groups {
        let pgid = slot.load(Ordering::Acquire);
        if pgid > 0 {
            // SAFETY: async-signal-safe. A negative pid names the group led by
            // `pgid`; an already-empty group fails harmlessly.
            unsafe {
                libc::kill(-pgid, libc::SIGKILL);
            }
        }
    }
}

/// The handler installed for SIGINT, SIGTERM, and SIGHUP.
///
/// Kill the tests, then die of the same signal with the default disposition, so
/// the runner's wait status is the conventional one and an interactive shell
/// sees an interrupt rather than an ordinary exit.
extern "C" fn forward_termination(signal: i32) {
    kill_registered_groups();
    // SAFETY: every call here is async-signal-safe and nothing allocates.
    // `sigaction` with SIG_DFL cannot fail for a catchable signal, and the
    // `raise` that follows does not return.
    unsafe {
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = libc::SIG_DFL;
        libc::sigemptyset(&mut action.sa_mask);
        libc::sigaction(signal, &action, std::ptr::null_mut());
        libc::raise(signal);
    }
}

static SIGNAL_FORWARDING: OnceLock<()> = OnceLock::new();

/// Take responsibility for the tests when the runner is asked to stop.
///
/// Each test leads its own process group so a timeout can kill its whole tree,
/// which also means the terminal's Ctrl-C is delivered to the runner alone.
/// Without a handler the runner would die and leave every live test running
/// with no supervisor and no timeout.
///
/// Idempotent, and called once per invocation next to
/// [`reserve_channel_descriptor`]. A signal already ignored when we started —
/// `nohup`, or a shell that detached the job — stays ignored: overriding that
/// would make the runner catchable where its parent deliberately made it not.
pub(crate) fn install_signal_forwarding() {
    SIGNAL_FORWARDING.get_or_init(|| {
        for signal in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
            // SAFETY: `sigaction` on a catchable signal with a valid handler.
            unsafe {
                let mut previous: libc::sigaction = std::mem::zeroed();
                if libc::sigaction(signal, std::ptr::null(), &mut previous) == 0
                    && previous.sa_sigaction == libc::SIG_IGN
                {
                    continue;
                }
                let mut action: libc::sigaction = std::mem::zeroed();
                action.sa_sigaction = forward_termination as libc::sighandler_t;
                libc::sigemptyset(&mut action.sa_mask);
                action.sa_flags = libc::SA_RESTART;
                libc::sigaction(signal, &action, std::ptr::null_mut());
            }
        }
    });
}

/// The status a watch-mode interrupt exits with (ADR-0083 §2, RUE-2023).
///
/// `rue test --watch` produces an exit code only when it is asked to stop, and
/// that code is the last completed cycle's: `0` all passed, `1` otherwise, and
/// `2` while no cycle has completed at all. Held here, next to the handler that
/// reads it, because a signal handler may not take a lock.
static WATCH_EXIT_STATUS: AtomicI32 = AtomicI32::new(super::TestExitCode::RunnerError as i32);

/// Publish the status a later interrupt should exit with.
pub(crate) fn set_watch_exit_status(exit: super::TestExitCode) {
    WATCH_EXIT_STATUS.store(exit.code(), Ordering::SeqCst);
}

/// The handler `rue test --watch` installs for SIGINT and SIGTERM.
///
/// A watch process has no natural end, so being asked to stop IS its result:
/// it kills the tests and reports the last completed cycle's status rather
/// than dying of the signal the way a one-shot run does. `_exit` is used
/// because it is async-signal-safe and because every event is already
/// flushed as it is written.
extern "C" fn exit_with_last_cycle_status(_signal: i32) {
    kill_registered_groups();
    // SAFETY: `_exit` is async-signal-safe and does not return.
    unsafe {
        libc::_exit(WATCH_EXIT_STATUS.load(Ordering::SeqCst));
    }
}

static WATCH_SIGNAL_EXIT: OnceLock<()> = OnceLock::new();

/// Take responsibility for the tests, and for the process's exit status, in
/// watch mode.
///
/// The counterpart of [`install_signal_forwarding`], installed instead of it:
/// the two disagree about what a stopped runner should look like, and only one
/// disposition can be in force. A signal already ignored when we started stays
/// ignored, for the same reason it does there.
pub(crate) fn install_watch_signal_exit() {
    WATCH_SIGNAL_EXIT.get_or_init(|| {
        for signal in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
            // SAFETY: `sigaction` on a catchable signal with a valid handler.
            unsafe {
                let mut previous: libc::sigaction = std::mem::zeroed();
                if libc::sigaction(signal, std::ptr::null(), &mut previous) == 0
                    && previous.sa_sigaction == libc::SIG_IGN
                {
                    continue;
                }
                let mut action: libc::sigaction = std::mem::zeroed();
                action.sa_sigaction = exit_with_last_cycle_status as libc::sighandler_t;
                libc::sigemptyset(&mut action.sa_mask);
                action.sa_flags = libc::SA_RESTART;
                libc::sigaction(signal, &action, std::ptr::null_mut());
            }
        }
    });
}

/// Run one test to a verdict.
///
/// Errors are runner errors — the image could not be executed at all — and are
/// distinct from a test that ran and failed.
pub(crate) fn run_one(dispatch: Dispatch<'_>) -> io::Result<Execution> {
    let scratch = scratch_path(dispatch.run_root, dispatch.seed, dispatch.ordinal);
    if scratch.exists() {
        let _ = std::fs::remove_dir_all(&scratch);
    }
    std::fs::create_dir_all(&scratch)?;

    let (channel_read, channel_write) = channel_pipe()?;
    let channel_write_fd = channel_write.as_raw_fd();
    let channel_read_fd = channel_read.as_raw_fd();

    let mut command = Command::new(dispatch.image);
    {
        use std::os::unix::process::CommandExt;
        command
            .arg0(LOGICAL_ARGV0)
            .arg(selector(dispatch.ordinal))
            .env_clear()
            .env(TEST_ENV_VAR, TEST_ENV_VALUE)
            .current_dir(&scratch)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        // SAFETY: the closure runs between fork and exec in the child. It calls
        // only async-signal-safe syscalls (`dup2`, `fcntl`, `close`) and
        // allocates nothing, which is the whole obligation `pre_exec` imposes.
        unsafe {
            command.pre_exec(move || install_channel(channel_write_fd, channel_read_fd));
        }
    }

    let mut live = LiveGroup {
        channel_write: Some(channel_write),
        slot: None,
        cancellation: dispatch.cancellation,
    };
    let run = Supervisor::new(command, dispatch.timeout)
        .stream_budget(dispatch.stream_budget)
        // The failure channel is drained on the same terms as the streams and
        // against its own budget, so a test that floods stdout cannot truncate
        // its own failure record (ADR-0083 §2). Budgeting it here rather than
        // leaving it to the frame parser (RUE-2025) is what makes a flooded
        // channel report as a flood instead of as an unreadable frame.
        //
        // Ownership of the read end moves to the drain thread, which closes it
        // at end of stream. It stays open until then on purpose: a test writing
        // to a channel whose reader had closed would die of SIGPIPE.
        .extra_capture(std::fs::File::from(channel_read), CHANNEL_BUDGET)
        // A capture past its budget is this test's verdict, not a runner
        // failure: the run continues and the flood is reported.
        .overflow_policy(OverflowPolicy::Verdict)
        .poll_interval(POLL_INTERVAL)
        // Post-exit group SIGKILL for stragglers: hygiene, not containment
        // (ADR-0083 §3).
        .reap_group(true)
        .observer(&mut live)
        .run()
        .map_err(SupervisionError::into_io)?;

    let supervision = match run.outcome {
        Outcome::Exited => Supervision::Exited,
        Outcome::TimedOut => Supervision::TimedOut,
        Outcome::Overflowed(record) => Supervision::OutputOverflow(overflow(record)),
    };
    let (exit_code, signal) = match &run.status {
        Some(status) => {
            use std::os::unix::process::ExitStatusExt;
            (status.code(), status.signal())
        }
        // The runner killed the group, so there is no self-reported status.
        None => (None, None),
    };
    let channel = run
        .extra
        .into_iter()
        .next()
        .expect("the failure channel is always captured");
    let frames = super::verdict::parse_channel(&channel.bytes);
    let status_for_classification = match (exit_code, signal) {
        (Some(code), _) => Ok(code),
        (None, Some(signal)) => Err(signal),
        // Supervision decides these, ahead of the status, in `classify`.
        (None, None) => Err(libc::SIGKILL),
    };
    let classification = classify(Observation {
        supervision,
        status: status_for_classification,
        stderr: &run.stderr.bytes,
        frames: &frames,
    });

    Ok(Execution {
        classification,
        exit_code,
        signal,
        frames,
        stdout_total: run.stdout.total,
        stderr_total: run.stderr.total,
        stdout: run.stdout.bytes,
        stderr: run.stderr.bytes,
        duration: run.duration,
        scratch_dir: scratch,
    })
}

/// The runner's stake in one live test process.
///
/// Both halves exist only while the child does: the parent's copy of the
/// channel's write end, which must be released the moment the child owns its
/// own, and the registry slot the signal handler walks.
struct LiveGroup<'a> {
    channel_write: Option<OwnedFd>,
    slot: Option<usize>,
    cancellation: Option<&'a RunCancellation>,
}

impl GroupObserver for LiveGroup<'_> {
    fn spawned(&mut self, pgid: i32) {
        // While the parent's write end is open, the channel's reader can never
        // see end of stream.
        self.channel_write = None;
        // Published before anything can block, so a signal arriving during the
        // drain still finds this test.
        self.slot = register_group(pgid);
        // A watch cycle canceled while this child was being spawned would
        // otherwise have swept the registry before this entry joined it, and
        // the child would run to its own timeout with nobody waiting for its
        // verdict (RUE-2023).
        if self.cancellation.is_some_and(RunCancellation::is_canceled) {
            // SAFETY: a negative pid names the group led by `pgid`; an
            // already-empty group fails harmlessly.
            unsafe {
                libc::kill(-pgid, libc::SIGKILL);
            }
        }
    }

    fn reaped(&mut self, pgid: i32) {
        // A pid is reusable the moment its group is empty, so a stale entry
        // would aim a SIGKILL at whatever unrelated process inherited the
        // number. Withdraw it as soon as the group is gone.
        if let Some(slot) = self.slot.take() {
            unregister_group(slot, pgid);
        }
    }
}

/// Name the capture that flooded in the runner's own vocabulary.
///
/// The supervisor knows the failure channel as the first extra capture; the
/// event stream knows it as the channel, because that is what a reader of a
/// verdict needs told.
fn overflow(record: rue_test_runner::supervise::Overflow) -> Overflow {
    use rue_test_runner::supervise::CaptureStream as Captured;
    let stream = match record.stream {
        Captured::Stdout => CaptureStream::Stdout,
        Captured::Stderr => CaptureStream::Stderr,
        Captured::Extra(0) => CaptureStream::Channel,
        Captured::Extra(index) => {
            unreachable!("a test process has one extra capture, not {}", index + 1)
        }
    };
    Overflow {
        stream,
        budget: record.budget,
    }
}

/// A fresh pipe for one test's failure channel.
///
/// Both ends are close-on-exec: this runner spawns from several threads at
/// once, and a descriptor without the flag would be inherited by whichever
/// unrelated test happened to fork next, keeping that test's channel from ever
/// reaching end of stream and making it pay the bounded finish timeout. The
/// child's own descriptor 3 has the flag cleared inside `pre_exec`, where it is
/// the one descriptor meant to survive.
///
/// On Linux the flag is set by `pipe2(O_CLOEXEC)`, atomically with the pipe's
/// creation. A `pipe` followed by two `fcntl` calls leaves exactly the window
/// this flag exists to close: a fork on another worker thread in between
/// inherits an unflagged end (RUE-2025). Other targets keep that spelling
/// because they have no `pipe2` — macOS in particular — so the window is
/// narrowed there rather than closed.
fn channel_pipe() -> io::Result<(OwnedFd, OwnedFd)> {
    let mut fds = [0 as libc::c_int; 2];
    #[cfg(target_os = "linux")]
    // SAFETY: `fds` is a valid two-element array for `pipe2` to fill.
    let created = unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) };
    #[cfg(not(target_os = "linux"))]
    // SAFETY: `fds` is a valid two-element array for `pipe` to fill.
    let created = unsafe { libc::pipe(fds.as_mut_ptr()) };
    if created != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the pipe was created, so both descriptors are open and unowned.
    let ends = unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) };
    #[cfg(not(target_os = "linux"))]
    for end in [&ends.0, &ends.1] {
        // SAFETY: the descriptor is owned and open.
        if unsafe { libc::fcntl(end.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) } < 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(ends)
}

/// Put the channel's write end on descriptor 3 in the forked child.
///
/// Runs between `fork` and `exec`, so it may call nothing but
/// async-signal-safe syscalls.
///
/// The `== CHANNEL_FD` branches below cannot be taken once
/// [`reserve_channel_descriptor`] has run: descriptor 3 is occupied by the
/// placeholder for the life of the process, so neither end of a pipe created
/// afterwards can be allocated there. They are kept as belt and braces — a
/// future caller that spawns without reserving first would otherwise get a
/// `dup2` onto itself, which is a no-op that leaves close-on-exec set and would
/// hand the image a channel that closes at `exec`.
fn install_channel(write_fd: i32, read_fd: i32) -> io::Result<()> {
    #[cfg(target_os = "linux")]
    {
        // A runner killed by SIGKILL runs no handler, so nothing forwards the
        // kill to the tests; this asks the kernel to do it instead. Best effort
        // — a kernel that refuses it costs nothing that was promised.
        //
        // PDEATHSIG fires when the forking *thread* dies, not the process. That
        // is the behaviour we want here: a worker thread outlives every child
        // it spawns, waiting for each before claiming the next and exiting only
        // when the scope ends, so the signal cannot arrive while the test is
        // still legitimately supervised.
        // SAFETY: async-signal-safe, and `prctl` here touches only this child.
        unsafe {
            libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL);
        }
    }
    if write_fd == CHANNEL_FD {
        // SAFETY: async-signal-safe, on a descriptor this child owns.
        if unsafe { libc::fcntl(CHANNEL_FD, libc::F_SETFD, 0) } < 0 {
            return Err(io::Error::last_os_error());
        }
    } else {
        // `dup2` clears close-on-exec on the new descriptor, which is exactly
        // what makes descriptor 3 survive into the image. What it replaces is
        // the reserved placeholder, never a live descriptor of the runner's.
        // SAFETY: async-signal-safe, on descriptors this child owns.
        if unsafe { libc::dup2(write_fd, CHANNEL_FD) } < 0 {
            return Err(io::Error::last_os_error());
        }
    }
    if read_fd != CHANNEL_FD {
        // SAFETY: async-signal-safe; the child has no use for the read end and
        // holding it open would keep the runner from seeing end of stream if
        // the image ever forked.
        unsafe {
            libc::close(read_fd);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    /// The dispatcher parses a fixed-width, lowercase, sixteen-digit selector
    /// and rejects everything else, so this rendering is contract.
    #[test]
    fn selectors_are_sixteen_lowercase_hex_digits() {
        assert_eq!(selector(0), "0000000000000000");
        assert_eq!(selector(1), "0000000000000001");
        assert_eq!(selector(255), "00000000000000ff");
        assert_eq!(selector(u32::MAX), "00000000ffffffff");
        for ordinal in [0, 1, 41, 1000, u32::MAX] {
            let rendered = selector(ordinal);
            assert_eq!(rendered.len(), 16);
            assert!(
                rendered
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
                "{rendered}"
            );
        }
    }

    /// The scratch name ties a retained directory back to the run and the test
    /// from the event stream alone.
    #[test]
    fn a_scratch_directory_is_named_from_the_seed_and_ordinal() {
        let root = run_root(417, None);
        let path = scratch_path(&root, 417, 3);
        assert_eq!(
            path.file_name().unwrap().to_str().unwrap(),
            "rue-test-417-3"
        );
        assert_eq!(path.parent().unwrap(), root);
    }

    /// Two runs sharing an explicit seed must not share scratch paths: one
    /// run's fresh-directory setup would delete the other's live working
    /// directory. The run root is what keeps them disjoint.
    #[test]
    fn a_run_root_is_private_to_its_process() {
        let root = run_root(417, None);
        assert_eq!(root.parent().unwrap(), std::env::temp_dir());
        assert!(
            root.file_name()
                .unwrap()
                .to_str()
                .unwrap()
                .ends_with(&format!("-{}", std::process::id())),
            "{root:?}"
        );
    }

    /// A watch process runs the same seed many times, so its run root also
    /// names the cycle: without that, cycle N+1 would delete the scratch
    /// directory a failing test in cycle N deliberately retained (RUE-2023).
    #[test]
    fn a_watch_cycle_gets_a_run_root_of_its_own() {
        let first = run_root(417, Some(1));
        let second = run_root(417, Some(2));
        assert_ne!(first, second);
        assert_ne!(first, run_root(417, None));
        assert!(
            first
                .file_name()
                .unwrap()
                .to_str()
                .unwrap()
                .ends_with("-c1"),
            "{first:?}"
        );
        // A retained scratch directory is still tied to the run by seed and
        // ordinal; only the root that holds it moved.
        assert_eq!(
            scratch_path(&first, 417, 3).file_name(),
            scratch_path(&second, 417, 3).file_name()
        );
    }

    /// Cancellation is published before the sweep, so a worker that reads the
    /// flag at any point after `cancel` returns never spawns.
    #[test]
    fn a_canceled_run_reports_itself_to_every_worker() {
        let cancellation = RunCancellation::new();
        assert!(!cancellation.is_canceled());
        let clone = cancellation.clone();
        cancellation.cancel();
        assert!(clone.is_canceled(), "cancellation is shared, not copied");
    }

    /// The channel's budget is separate from the streams' so a test that floods
    /// stdout cannot truncate its own failure record (ADR-0083 §2).
    #[test]
    fn the_channel_budget_is_independent_of_the_stream_budget() {
        assert_eq!(DEFAULT_STREAM_BUDGET, 1024 * 1024);
        assert_eq!(CHANNEL_BUDGET, 256 * 1024);
        assert_ne!(DEFAULT_STREAM_BUDGET, CHANNEL_BUDGET);
    }

    /// A channel past its budget is the same supervision outcome as a flooded
    /// stream, and names its own budget: before RUE-2025 it was not checked at
    /// all, so it surfaced as a truncated frame and a bare `exit`.
    #[test]
    fn a_channel_past_its_budget_overflows_naming_the_channel() {
        let reported = overflow(rue_test_runner::supervise::Overflow {
            stream: rue_test_runner::supervise::CaptureStream::Extra(0),
            budget: CHANNEL_BUDGET,
        });
        assert_eq!(reported.stream, CaptureStream::Channel);
        assert_eq!(reported.budget, CHANNEL_BUDGET);
    }

    /// Each capture reports its own budget, which is the whole reason the
    /// overflow carries one: the channel's is a quarter of a stream's.
    #[test]
    fn an_overflowing_stream_reports_the_budget_it_exceeded() {
        use rue_test_runner::supervise::CaptureStream as Captured;

        for (captured, expected) in [
            (Captured::Stdout, CaptureStream::Stdout),
            (Captured::Stderr, CaptureStream::Stderr),
        ] {
            let reported = overflow(rue_test_runner::supervise::Overflow {
                stream: captured,
                budget: DEFAULT_STREAM_BUDGET,
            });
            assert_eq!(reported.stream, expected);
            assert_eq!(reported.budget, DEFAULT_STREAM_BUDGET);
        }
    }

    /// A pipe both of whose ends leaked into an unrelated concurrent spawn
    /// would keep that test's channel from ever reaching end of stream.
    #[test]
    fn both_channel_ends_are_close_on_exec_in_the_parent() {
        reserve_channel_descriptor();
        let (read, write) = channel_pipe().expect("a pipe");
        for end in [&read, &write] {
            // SAFETY: the descriptor is owned and open.
            let flags = unsafe { libc::fcntl(end.as_raw_fd(), libc::F_GETFD) };
            assert!(flags >= 0);
            assert_eq!(flags & libc::FD_CLOEXEC, libc::FD_CLOEXEC);
        }
    }

    /// The reservation's whole purpose: once descriptor 3 is held, nothing the
    /// process opens afterwards can be allocated there. If a pipe end could
    /// still land on 3, a child's `dup2` onto the channel would destroy it —
    /// and for `Command::spawn`'s own exec-reporting pipe that means an exec
    /// failure written into the failure channel while the parent reads EOF from
    /// the real pipe and believes the spawn succeeded.
    #[test]
    fn nothing_is_allocated_at_the_channel_descriptor_after_reserving_it() {
        reserve_channel_descriptor();

        // SAFETY: a bare query of a descriptor's flags.
        assert!(
            unsafe { libc::fcntl(CHANNEL_FD, libc::F_GETFD) } >= 0,
            "descriptor {CHANNEL_FD} must be occupied after reserving it"
        );

        // Several pipes, because the first free descriptor moves as they stack
        // up: none of their ends may be the reserved one.
        let mut held = Vec::new();
        for _ in 0..8 {
            let (read, write) = channel_pipe().expect("a pipe");
            assert_ne!(read.as_raw_fd(), CHANNEL_FD);
            assert_ne!(write.as_raw_fd(), CHANNEL_FD);
            held.push((read, write));
        }

        // An ordinary file open is allocated from the same descriptor space.
        let file = std::fs::File::open("/dev/null").expect("/dev/null");
        assert_ne!(file.as_raw_fd(), CHANNEL_FD);
    }

    /// A registry of the tests' own. The process-wide one is what the signal
    /// handler kills, so a test must never publish a pid into it that it is not
    /// about to reap: an unrelated process could hold that number by then.
    fn registry(slots: usize) -> Vec<AtomicI32> {
        (0..slots).map(|_| AtomicI32::new(0)).collect()
    }

    /// A registration is visible until it is withdrawn, and withdrawal is by
    /// owner: a slot recycled between the two must not be cleared by the
    /// previous tenant.
    #[test]
    fn a_group_registration_is_withdrawn_by_its_owner() {
        let groups = registry(4);
        let slot = register_in(&groups, 4_242).expect("a free slot");
        assert_eq!(groups[slot].load(Ordering::Acquire), 4_242);
        unregister_in(&groups, slot, 9_999);
        assert_eq!(
            groups[slot].load(Ordering::Acquire),
            4_242,
            "a withdrawal naming another group leaves the slot alone"
        );
        unregister_in(&groups, slot, 4_242);
        assert_eq!(groups[slot].load(Ordering::Acquire), 0);
        // An out-of-range slot is a lost teardown, never a crashed runner.
        unregister_in(&groups, groups.len(), 1);
    }

    /// A full registry refuses rather than failing a run: registration is best
    /// effort, and all that is lost is the handler's teardown of that group.
    #[test]
    fn a_full_registry_refuses_rather_than_failing_a_run() {
        let groups = registry(3);
        let held: Vec<usize> = (0..3)
            .map(|index| register_in(&groups, 100 + index).expect("a free slot"))
            .collect();
        assert_eq!(held.len(), 3);
        assert!(register_in(&groups, 200).is_none());
        unregister_in(&groups, held[1], 101);
        assert_eq!(
            register_in(&groups, 200),
            Some(held[1]),
            "a freed slot is reused"
        );
    }

    /// What a Ctrl-C must do, exercised without signalling this process: the
    /// registered group dies even though the child leads a process group of its
    /// own, which is exactly the group the terminal's SIGINT never reaches.
    #[test]
    fn killing_the_registered_groups_kills_a_live_child() {
        use std::os::unix::process::{CommandExt as _, ExitStatusExt as _};

        let mut command = Command::new("sleep");
        command
            .arg("30")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0);
        let mut child = command.spawn().expect("sleep(1) is a POSIX utility");
        let pid = child.id() as i32;
        let groups = registry(2);
        register_in(&groups, pid).expect("a free slot");

        kill_groups_in(&groups);

        let deadline = Instant::now() + Duration::from_secs(5);
        let status = loop {
            if let Some(status) = child.try_wait().expect("waiting on our own child") {
                break status;
            }
            assert!(Instant::now() < deadline, "the child outlived the kill");
            std::thread::sleep(POLL_INTERVAL);
        };
        assert_eq!(status.signal(), Some(libc::SIGKILL));
    }

    /// A withdrawn group is not signalled again: the pid is reusable the moment
    /// its group empties, and a stale entry would aim a kill at a stranger.
    #[test]
    fn a_withdrawn_group_is_no_longer_reachable_from_the_handler() {
        let groups = registry(2);
        let slot = register_in(&groups, 1_234).expect("a free slot");
        unregister_in(&groups, slot, 1_234);
        assert!(
            groups.iter().all(|slot| slot.load(Ordering::Acquire) == 0),
            "a withdrawn group leaves nothing for the handler to kill"
        );
    }

    /// The registry is sized against the driver's own cap on `--jobs`, so the
    /// full case is unreachable in a real run rather than merely handled.
    #[test]
    fn the_registry_holds_more_groups_than_a_run_can_have_children() {
        assert!(MAX_LIVE_GROUPS > crate::MAX_EXPLICIT_JOBS);
        assert_eq!(LIVE_GROUPS.len(), MAX_LIVE_GROUPS);
    }

    /// Installing the handlers twice must be harmless: the runner calls it once
    /// per invocation, and these tests call it alongside.
    #[test]
    fn installing_signal_forwarding_is_idempotent() {
        install_signal_forwarding();
        install_signal_forwarding();
        for signal in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
            // SAFETY: a bare query of a signal's current disposition.
            let mut installed: libc::sigaction = unsafe { std::mem::zeroed() };
            // SAFETY: `installed` is a valid destination for the query.
            let queried = unsafe { libc::sigaction(signal, std::ptr::null(), &mut installed) };
            assert_eq!(queried, 0);
            assert_ne!(installed.sa_sigaction, libc::SIG_DFL, "signal {signal}");
        }
    }

    /// Reserving twice is not an error and does not change what is held: the
    /// runner calls it once per invocation and tests call it freely.
    #[test]
    fn reserving_the_channel_descriptor_is_idempotent() {
        reserve_channel_descriptor();
        // SAFETY: a bare query of a descriptor's flags.
        let first = unsafe { libc::fcntl(CHANNEL_FD, libc::F_GETFD) };
        reserve_channel_descriptor();
        // SAFETY: a bare query of a descriptor's flags.
        let second = unsafe { libc::fcntl(CHANNEL_FD, libc::F_GETFD) };
        assert!(first >= 0);
        assert_eq!(first, second);
    }
}
