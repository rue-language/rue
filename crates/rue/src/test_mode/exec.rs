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
//! bounded concurrent drains, the post-exit group kill — are `rue-test-runner`'s
//! and are used from there rather than reimplemented, so the harness and the
//! product runner cannot drift on the deadlock class RUE-338 closed.

use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicI32, Ordering};
use std::time::{Duration, Instant};

use rue_test_runner::pipe_drain::{PIPE_DRAIN_FINISH_TIMEOUT, PipeDrain, spawn_pipe_drain};
use rue_test_runner::{configure_process_group, kill_process_group};

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
pub(crate) fn run_root(seed: u64) -> PathBuf {
    std::env::temp_dir().join(format!("rue-test-{seed}-{}", std::process::id()))
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
fn kill_registered_groups() {
    kill_groups_in(&LIVE_GROUPS);
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
    configure_process_group(&mut command);

    let start = Instant::now();
    let mut child = command.spawn()?;
    // The parent's copy of the write end must go now: while it is open, the
    // reader below can never see end of stream.
    drop(channel_write);
    let pid = child.id() as i32;
    // The child leads its own group, so its pid is its pgid. Registered before
    // anything can block, so a signal arriving during the drain below still
    // finds this test.
    let group_slot = register_group(pid);

    let mut stdout_drain = spawn_pipe_drain(child.stdout.take(), Some(dispatch.stream_budget));
    let mut stderr_drain = spawn_pipe_drain(child.stderr.take(), Some(dispatch.stream_budget));
    // Ownership of the read end moves to the drain thread, which closes it at
    // end of stream. The runner holds it open until then on purpose: a test
    // writing to a channel whose reader had closed would die of SIGPIPE.
    let mut channel_drain = spawn_pipe_drain(
        Some(std::fs::File::from(channel_read)),
        Some(CHANNEL_BUDGET),
    );

    let mut supervision = Supervision::Exited;
    let status = loop {
        stdout_drain.poll();
        stderr_drain.poll();
        channel_drain.poll();

        if let Some(overflow) = overflowed(
            &stdout_drain,
            &stderr_drain,
            &channel_drain,
            dispatch.stream_budget,
        ) {
            supervision = Supervision::OutputOverflow(overflow);
            kill_process_group(&mut child);
            break None;
        }

        match child.try_wait()? {
            Some(status) => break Some(status),
            None => {
                if start.elapsed() > dispatch.timeout {
                    supervision = Supervision::TimedOut;
                    kill_process_group(&mut child);
                    break None;
                }
                std::thread::sleep(POLL_INTERVAL);
            }
        }
    };
    let duration = start.elapsed();

    // Post-exit group SIGKILL for stragglers: hygiene, not containment
    // (ADR-0083 §3). A descendant that outlived its parent would otherwise keep
    // a pipe open and stall the bounded finish below for no useful reason.
    reap_process_group(pid);
    if let Some(slot) = group_slot {
        unregister_group(slot, pid);
    }

    finish(&mut stdout_drain);
    finish(&mut stderr_drain);
    finish(&mut channel_drain);

    // The budget is enforced by the drain threads, which hand their verdict
    // over a channel — so a test that floods its stdout and then exits
    // immediately can be reaped before the overflow message arrives. Reading
    // the flag again after the bounded finish is what keeps such a test from
    // reporting as a pass whose digest silently covers a truncated prefix.
    if supervision == Supervision::Exited
        && let Some(overflow) = overflowed(
            &stdout_drain,
            &stderr_drain,
            &channel_drain,
            dispatch.stream_budget,
        )
    {
        supervision = Supervision::OutputOverflow(overflow);
    }

    let (exit_code, signal) = match &status {
        Some(status) => {
            use std::os::unix::process::ExitStatusExt;
            (status.code(), status.signal())
        }
        None => (None, None),
    };
    let frames = super::verdict::parse_channel(channel_drain.bytes());
    let status_for_classification = match (exit_code, signal) {
        (Some(code), _) => Ok(code),
        (None, Some(signal)) => Err(signal),
        // The runner killed the group, so there is no self-reported status.
        // Supervision decides these, ahead of the status, in `classify`.
        (None, None) => Err(libc::SIGKILL),
    };
    let classification = classify(Observation {
        supervision,
        status: status_for_classification,
        stderr: stderr_drain.bytes(),
        frames: &frames,
    });

    Ok(Execution {
        classification,
        exit_code,
        signal,
        frames,
        stdout_total: stdout_drain.bytes_total(),
        stderr_total: stderr_drain.bytes_total(),
        stdout: stdout_drain.into_bytes(),
        stderr: stderr_drain.into_bytes(),
        duration,
        scratch_dir: scratch,
    })
}

fn finish(drain: &mut PipeDrain) {
    drain.finish(PIPE_DRAIN_FINISH_TIMEOUT);
}

/// The first capture to outgrow its budget, if any.
///
/// The channel is checked with the streams rather than left to the frame parser
/// (RUE-2025): a channel truncated mid-line surfaces as a malformed frame, which
/// reports the test as a bare `exit` with a runner note about an unreadable
/// channel — a description of the symptom, not of the test writing a quarter of
/// a megabyte of failure records.
fn overflowed(
    stdout: &PipeDrain,
    stderr: &PipeDrain,
    channel: &PipeDrain,
    stream_budget: usize,
) -> Option<Overflow> {
    let candidates = [
        (stdout, CaptureStream::Stdout, stream_budget),
        (stderr, CaptureStream::Stderr, stream_budget),
        (channel, CaptureStream::Channel, CHANNEL_BUDGET),
    ];
    candidates
        .into_iter()
        .find(|(drain, _, _)| drain.overflowed())
        .map(|(_, stream, budget)| Overflow { stream, budget })
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

/// Best-effort teardown of anything the test left running in its group.
fn reap_process_group(pid: i32) {
    // SAFETY: a negative pid names the process group led by `pid`. Failure
    // (an already-empty group) is the ordinary case and carries no obligation.
    unsafe {
        libc::kill(-pid, libc::SIGKILL);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let root = run_root(417);
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
        let root = run_root(417);
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

    /// The channel's budget is separate from the streams' so a test that floods
    /// stdout cannot truncate its own failure record (ADR-0083 §2).
    #[test]
    fn the_channel_budget_is_independent_of_the_stream_budget() {
        assert_eq!(DEFAULT_STREAM_BUDGET, 1024 * 1024);
        assert_eq!(CHANNEL_BUDGET, 256 * 1024);
        assert_ne!(DEFAULT_STREAM_BUDGET, CHANNEL_BUDGET);
    }

    fn drained(bytes: usize, budget: usize) -> PipeDrain {
        let mut drain =
            spawn_pipe_drain(Some(std::io::Cursor::new(vec![b'x'; bytes])), Some(budget));
        drain.finish(Duration::from_secs(5));
        drain
    }

    /// A channel past its budget is the same supervision outcome as a flooded
    /// stream, and names its own budget: before RUE-2025 it was not checked at
    /// all, so it surfaced as a truncated frame and a bare `exit`.
    #[test]
    fn a_channel_past_its_budget_overflows_naming_the_channel() {
        let quiet = drained(16, DEFAULT_STREAM_BUDGET);
        let flooded = drained(CHANNEL_BUDGET + 1, CHANNEL_BUDGET);
        let overflow = overflowed(&quiet, &quiet, &flooded, DEFAULT_STREAM_BUDGET)
            .expect("a channel past its budget is an overflow");
        assert_eq!(overflow.stream, CaptureStream::Channel);
        assert_eq!(overflow.budget, CHANNEL_BUDGET);
    }

    /// Each capture reports its own budget, which is the whole reason the
    /// overflow carries one: the channel's is a quarter of a stream's.
    #[test]
    fn an_overflowing_stream_reports_the_budget_it_exceeded() {
        let quiet = drained(16, 64);
        let flooded = drained(128, 64);
        let stdout = overflowed(&flooded, &quiet, &quiet, 64).expect("stdout overflowed");
        assert_eq!(stdout.stream, CaptureStream::Stdout);
        assert_eq!(stdout.budget, 64);
        let stderr = overflowed(&quiet, &flooded, &quiet, 64).expect("stderr overflowed");
        assert_eq!(stderr.stream, CaptureStream::Stderr);
        assert!(overflowed(&quiet, &quiet, &quiet, 64).is_none());
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
