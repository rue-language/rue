//! Crash-detecting execution harness for fuzz targets.
//!
//! # Why this exists (RUE-43)
//!
//! The naive way to run a fuzz target is `catch_unwind` in-process. That only
//! catches Rust **panics**. An *abort* — a stack overflow (SIGSEGV on the guard
//! page), an out-of-memory `abort()`, an explicit `SIGABRT`, or a `SIGSEGV`/
//! `SIGFPE` from unsafe code — tears the whole process down *without* unwinding,
//! so `catch_unwind` never returns and the crash is completely invisible to the
//! fuzzer. That is exactly how the RUE-42 stack overflow slipped through: the
//! process just died and the loop reported a clean run.
//!
//! To see aborts we run every input in a **forked child process** and inspect
//! the child's wait-status in the parent. A child killed by a signal shows up as
//! `WIFSIGNALED`, so `SIGSEGV`/`SIGABRT`/`SIGFPE`/... are reported as crashes.
//!
//! ## Panics under `panic = abort`
//!
//! This toolchain builds with `panic = abort` (see
//! `toolchains/rust/defs.bzl`), so a Rust panic does **not** unwind — it runs
//! the panic hook and then calls `abort()` (SIGABRT). `catch_unwind` therefore
//! catches nothing here. To keep panics distinguishable from raw aborts (and to
//! give each panic a distinct dedup signature) the child installs a panic hook
//! that streams the panic message — including its source location — over a pipe
//! to the parent *before* the process dies. The parent uses that message when
//! present, and falls back to the terminating signal otherwise. `catch_unwind`
//! is still wrapped around the target so the harness also behaves correctly if
//! ever built with `panic = unwind`.
//!
//! The parent process never executes target code, so a target that corrupts
//! memory or blows the stack cannot damage the fuzzer itself.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Exit code the child uses to signal "I caught a panic" (distinct from a
/// signal death and from a clean exit). 101 matches rustc's own convention.
const PANIC_EXIT_CODE: i32 = 101;

/// Default per-input wall-clock budget. Fuzz inputs are tiny; a healthy
/// compile finishes in milliseconds, so anything still running after this
/// long is a hang/superlinear bug, not a slow success.
pub const DEFAULT_PER_INPUT_TIMEOUT: Duration = Duration::from_secs(5);

/// Number of additional distinct reproducers saved for coarse crash buckets.
///
/// Every crash bucket is deliberately broader than the bug that filled it.
/// Signals and timeouts are broad because the forked harness cannot cheaply
/// recover a precise crash site from the dying child; panics are broad because
/// their signature redacts input-derived payload so one invariant stays in one
/// bucket (see [`panic_signature`]). Saving a small amount of duplicate
/// evidence keeps flood control while avoiding a single `signal:SIGSEGV`,
/// `timeout`, or ICE bucket hiding later, materially different inputs.
const MAX_DUPLICATE_REPRODUCERS_PER_SIGNATURE: u64 = 5;

/// Cap on the redacted message text a panic signature carries. ICE payloads can
/// be arbitrarily long `{:?}` dumps; the invariant's wording comes first.
const MAX_SIGNATURE_DETAIL_CHARS: usize = 200;

/// Cap on the crash description recorded in `.meta`, for the same reason.
const MAX_DESCRIPTION_CHARS: usize = 600;

/// The outcome of running one fuzz input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunOutcome {
    /// The target returned normally.
    Ok,
    /// The target panicked; carries the panic message (best-effort).
    Panic(String),
    /// The child was killed by a terminating signal (e.g. SIGSEGV=11,
    /// SIGABRT=6, SIGFPE=8). Fork isolation lets the harness observe this
    /// without dying with the target.
    Signal(i32),
    /// The child exceeded the per-input timeout and was SIGKILLed by the
    /// harness. Carries the budget in seconds. Hangs (infinite loops,
    /// superlinear parse blowups) are compiler bugs too — and without this,
    /// one hung input wedges the whole run in `waitpid`.
    Timeout(u64),
}

impl RunOutcome {
    /// Whether this outcome represents a crash (anything other than a clean run).
    pub fn is_crash(&self) -> bool {
        !matches!(self, RunOutcome::Ok)
    }

    /// A stable signature used to collapse duplicate crashes.
    ///
    /// - Panics dedup on their crash site *and* a redacted form of the panic
    ///   message; see [`panic_signature`] for why the site alone is not enough.
    /// - Signals dedup on the signal type. Async-signal-safe backtracing is out
    ///   of scope, so all crashes of the same signal kind collapse to one entry
    ///   — which is precisely the flood-control we want (the RUE-42 overflow
    ///   made *every* deep input SIGSEGV).
    pub fn signature(&self) -> Option<String> {
        match self {
            RunOutcome::Ok => None,
            RunOutcome::Panic(msg) => Some(format!("panic:{}", panic_signature(msg))),
            RunOutcome::Signal(sig) => Some(format!("signal:{}", signal_name(*sig))),
            // All hangs collapse to one bucket — same flood-control rationale
            // as signals (one superlinear-parse bug makes *many* inputs hang).
            RunOutcome::Timeout(_) => Some("timeout".to_string()),
        }
    }

    /// Short human-readable description for logs and the `.meta` sibling.
    ///
    /// A panic keeps its whole message (flattened onto one line so `.meta` stays
    /// line-oriented, and length-bounded so a runaway `{:?}` payload cannot
    /// dominate the artifact). For a graceful ICE the message *is* the finding:
    /// dropping everything after the `panicked at <site>:` line left the
    /// downloaded artifact saying only that some ICE happened somewhere.
    pub fn describe(&self) -> String {
        match self {
            RunOutcome::Ok => "ok".to_string(),
            RunOutcome::Panic(msg) => {
                format!("panic: {}", truncate(&flatten(msg), MAX_DESCRIPTION_CHARS))
            }
            RunOutcome::Signal(sig) => format!("signal: {} ({})", signal_name(*sig), sig),
            RunOutcome::Timeout(secs) => format!("timeout: hung for more than {secs}s"),
        }
    }
}

/// Dedup key for a panic: the crash site plus a payload-redacted digest of the
/// message.
///
/// The site alone is not a usable key. `assert_no_ice` funnels *every* graceful
/// compiler ICE — whichever invariant broke, in whichever phase — through one
/// `panic!`, so a site-only signature collapses every ICE the fuzzer can ever
/// find into a single bucket — and, panic buckets having kept only their first
/// reproducer, every later ICE was discarded unseen. The nightly job reported
/// the same opaque `panicked at targets.rs:<line>:<col>` night after night while
/// the evidence for materially different bugs was thrown away. That is the
/// RUE-383 over-merge, one crash class over.
///
/// The message is redacted rather than used raw because ICE text embeds
/// input-derived payload — module paths, symbol names, indices. Keying on it
/// verbatim splits one invariant across an unbounded number of buckets, which
/// is the opposite failure and refloods the crashes directory. See
/// [`redact_payload`] for what is dropped; what survives is the invariant's own
/// wording, which is the discriminator.
fn panic_signature(msg: &str) -> String {
    let mut lines = msg.lines().map(str::trim).filter(|line| !line.is_empty());
    let site = lines.next().unwrap_or("");
    match lines.next() {
        Some(detail) => format!(
            "{site} {}",
            truncate(&redact_payload(detail), MAX_SIGNATURE_DETAIL_CHARS)
        ),
        None => site.to_string(),
    }
}

/// Replace input-derived payload with fixed placeholders so that two reports of
/// the same broken invariant share a signature.
///
/// Balanced `{}`/`[]`/`()` groups collapse whole, quoted spans become `""`, and
/// digit runs become `#`; what survives is the invariant's own wording. Group
/// collapsing is what keeps the wording *within* [`MAX_SIGNATURE_DETAIL_CHARS`]:
/// an ICE that debug-prints a nested query key spends hundreds of characters on
/// payload before reaching the phrase that names the broken invariant, so
/// redacting character-by-character would leave the cap to truncate the only
/// discriminating part away.
fn redact_payload(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '"' => {
                out.push_str("\"\"");
                i += 1;
                while i < chars.len() && chars[i] != '"' {
                    i += 1;
                }
                i += 1; // step past the closing quote (or off the end)
            }
            open @ ('{' | '[' | '(') => {
                let close = matching_close(open);
                out.push(open);
                out.push(close);
                i += 1;
                // Track depth so a nested group does not close the outer one.
                // An unbalanced tail (a truncated debug dump) just consumes the
                // rest, which is exactly the payload we mean to drop.
                let mut depth = 1usize;
                while i < chars.len() && depth > 0 {
                    match chars[i] {
                        c if c == open => depth += 1,
                        c if c == close => depth -= 1,
                        _ => {}
                    }
                    i += 1;
                }
            }
            c if c.is_ascii_digit() => {
                out.push('#');
                while i < chars.len() && chars[i].is_ascii_digit() {
                    i += 1;
                }
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    out
}

fn matching_close(open: char) -> char {
    match open {
        '{' => '}',
        '[' => ']',
        _ => ')',
    }
}

/// Collapse a multi-line message onto one line. The `.meta` format is
/// `key: value` per line, so an embedded newline would corrupt it.
fn flatten(s: &str) -> String {
    s.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Truncate on a char boundary, marking that the text was cut.
fn truncate(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    let head: String = s.chars().take(max_chars).collect();
    format!("{head}...")
}

/// Human-readable name for the common fatal signals.
pub fn signal_name(sig: i32) -> String {
    let name = match sig {
        libc::SIGSEGV => "SIGSEGV",
        libc::SIGABRT => "SIGABRT",
        libc::SIGFPE => "SIGFPE",
        libc::SIGILL => "SIGILL",
        libc::SIGBUS => "SIGBUS",
        libc::SIGKILL => "SIGKILL",
        libc::SIGTRAP => "SIGTRAP",
        _ => return format!("SIG{sig}"),
    };
    name.to_string()
}

/// Run `f` in a forked child with the [default][DEFAULT_PER_INPUT_TIMEOUT]
/// per-input timeout. See [`run_forked_with_timeout`].
pub fn run_forked<F: FnOnce()>(f: F) -> RunOutcome {
    run_forked_with_timeout(f, DEFAULT_PER_INPUT_TIMEOUT)
}

/// Run `f` in a forked child process and report how it terminated.
///
/// The child runs the closure under `catch_unwind`; the parent `waitpid`s and
/// classifies the result. Fatal signals become [`RunOutcome::Signal`], panics
/// become [`RunOutcome::Panic`], a clean run is [`RunOutcome::Ok`]. A child
/// still running after `timeout` is SIGKILLed and reported as
/// [`RunOutcome::Timeout`] — without this, a hung compile blocks the parent
/// in `waitpid` forever and the entire hang/DoS bug class is undetectable.
///
/// If `fork`/`pipe` themselves fail (extremely unlikely), we degrade gracefully
/// to an in-process `catch_unwind` run — that still catches panics, we just lose
/// signal detection (and hang detection) for that one input.
pub fn run_forked_with_timeout<F: FnOnce()>(f: F, timeout: Duration) -> RunOutcome {
    // Pipe for streaming the panic message from child to parent.
    let mut fds = [0 as libc::c_int; 2];
    if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
        return run_in_process(f);
    }
    let (read_fd, write_fd) = (fds[0], fds[1]);

    let pid = unsafe { libc::fork() };
    if pid < 0 {
        unsafe {
            libc::close(read_fd);
            libc::close(write_fd);
        }
        return run_in_process(f);
    }

    if pid == 0 {
        // ---- Child: runs the (potentially crashing) target and never returns.
        unsafe { libc::close(read_fd) };

        // Stream the panic message to the parent from the panic hook, which runs
        // *before* the process dies. Under `panic = abort` this is the only way
        // to recover the message (the runtime aborts right after the hook);
        // under `panic = unwind` `catch_unwind` below also handles it.
        let hook_fd = write_fd;
        std::panic::set_hook(Box::new(move |info| {
            // `info` Displays as "panicked at <file>:<line>:<col>:\n<message>",
            // so the location is included — that keeps distinct panic sites in
            // distinct dedup buckets.
            let text = info.to_string();
            let bytes = text.as_bytes();
            unsafe {
                libc::write(hook_fd, bytes.as_ptr() as *const libc::c_void, bytes.len());
            }
        }));

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
        let code = if result.is_ok() { 0 } else { PANIC_EXIT_CODE };
        unsafe {
            libc::close(write_fd);
            // _exit, not exit: skip atexit handlers and stdio flushing so we
            // don't double-flush buffers the parent also owns. `_exit` returns
            // `!`, so the child branch diverges and never falls through to the
            // parent code below (which would otherwise wait on itself).
            libc::_exit(code);
        }
    }

    // ---- Parent: collect any panic message, then reap the child — both
    // bounded by the per-input deadline. The pipe read must be bounded too: a
    // hung child holds its write end open and never writes, so an unbounded
    // read() would block before we ever reached waitpid.
    unsafe { libc::close(write_fd) };
    let deadline = Instant::now() + timeout;
    let mut msg = Vec::new();
    let mut buf = [0u8; 4096];
    let mut timed_out = false;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            timed_out = true;
            break;
        }
        let mut pfd = libc::pollfd {
            fd: read_fd,
            events: libc::POLLIN,
            revents: 0,
        };
        let wait_ms = remaining.as_millis().min(i32::MAX as u128) as libc::c_int;
        let r = unsafe { libc::poll(&mut pfd, 1, wait_ms.max(1)) };
        if r < 0 {
            if std::io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            break; // poll error: give up on the message, fall through to reap
        }
        if r == 0 {
            // Deadline expired with the child still holding the pipe open.
            timed_out = true;
            break;
        }
        let n = unsafe { libc::read(read_fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
        if n < 0 {
            if std::io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            break;
        }
        if n == 0 {
            break; // EOF: the child closed its end (normal exit or death)
        }
        msg.extend_from_slice(&buf[..n as usize]);
    }
    unsafe { libc::close(read_fd) };

    if timed_out {
        unsafe {
            libc::kill(pid, libc::SIGKILL);
        }
    }

    // Reap, still bounded: even after pipe EOF a pathological child could
    // linger (e.g. the closure closed our write end itself and then hung), so
    // poll with WNOHANG and escalate to SIGKILL at the deadline.
    let mut status: libc::c_int = 0;
    let mut killed = timed_out;
    loop {
        let r = unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) };
        if r == pid {
            break;
        }
        if r == -1 {
            if std::io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            // ECHILD or another wait error: nothing reapable; classify by what
            // we know.
            return if timed_out {
                RunOutcome::Timeout(timeout.as_secs())
            } else {
                outcome_from_status_exited_unknown(msg)
            };
        }
        // r == 0: child still running.
        if !killed && Instant::now() >= deadline {
            timed_out = true;
            killed = true;
            unsafe {
                libc::kill(pid, libc::SIGKILL);
            }
        }
        std::thread::sleep(Duration::from_millis(2));
    }

    if timed_out {
        // We killed it (or it died at the boundary): either way the input
        // exceeded its budget.
        return RunOutcome::Timeout(timeout.as_secs());
    }
    outcome_from_status(status, msg)
}

/// Reached only if `waitpid` errored with no status available: fall back to
/// classifying on the panic message alone.
fn outcome_from_status_exited_unknown(panic_msg: Vec<u8>) -> RunOutcome {
    let msg = String::from_utf8_lossy(&panic_msg);
    if !msg.trim().is_empty() {
        return RunOutcome::Panic(msg.into_owned());
    }
    RunOutcome::Ok
}

fn outcome_from_status(status: libc::c_int, panic_msg: Vec<u8>) -> RunOutcome {
    // A captured panic message (from the child's panic hook) takes priority: it
    // identifies the exact crash site, which is far more useful for dedup than
    // the bare SIGABRT the abort runtime raises afterwards.
    let msg = String::from_utf8_lossy(&panic_msg);
    if !msg.trim().is_empty() {
        return RunOutcome::Panic(msg.into_owned());
    }

    if libc::WIFSIGNALED(status) {
        // Signal death with no panic message is a genuine abort — stack
        // overflow, segfault, FPE, or a direct abort() — observable because
        // the target runs in a child process.
        return RunOutcome::Signal(libc::WTERMSIG(status));
    }
    if libc::WIFEXITED(status) {
        let code = libc::WEXITSTATUS(status);
        if code == 0 {
            return RunOutcome::Ok;
        }
        // A non-zero exit with no panic message (e.g. the target called
        // process::exit): still abnormal, treat it as a crash.
        return RunOutcome::Panic(format!("child exited with status {code}"));
    }
    RunOutcome::Ok
}

/// Fallback path when fork/pipe are unavailable. Note: under `panic = abort`
/// this cannot catch panics either (the process aborts), but it preserves
/// correctness under `panic = unwind`.
fn run_in_process<F: FnOnce()>(f: F) -> RunOutcome {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)) {
        Ok(()) => RunOutcome::Ok,
        Err(_) => RunOutcome::Panic("panic (message unavailable)".to_string()),
    }
}

/// Records crashes to disk, collapsing duplicates by [`RunOutcome::signature`].
///
/// A reproducer is always written for the *first* occurrence of each distinct
/// crash signature, and the first few duplicate inputs in a bucket are saved as
/// bounded evidence; exact same-input reruns are still deduped so retry loops do
/// not rewrite the same artifact.
pub struct CrashReporter {
    crash_dir: Option<PathBuf>,
    signatures: HashMap<String, SignatureState>,
    pub unique_crashes: u64,
    pub duplicate_crashes: u64,
}

#[derive(Default)]
struct SignatureState {
    total_seen: u64,
    saved_input_hashes: HashSet<u64>,
    duplicate_reproducers_saved: u64,
}

impl CrashReporter {
    pub fn new(crash_dir: Option<PathBuf>) -> Self {
        Self {
            crash_dir,
            signatures: HashMap::new(),
            unique_crashes: 0,
            duplicate_crashes: 0,
        }
    }

    /// Report a crash. Returns the reproducer path if one was written, or
    /// `None` if the crash was fully deduped (or `outcome` was not a crash).
    pub fn report(&mut self, target: &str, input: &[u8], outcome: &RunOutcome) -> Option<PathBuf> {
        let signature = outcome.signature()?;
        let input_hash = fnv1a(input);

        let state = self.signatures.entry(signature.clone()).or_default();
        let first_for_signature = state.total_seen == 0;
        state.total_seen += 1;

        let should_save = if first_for_signature {
            self.unique_crashes += 1;
            state.saved_input_hashes.insert(input_hash);
            true
        } else {
            self.duplicate_crashes += 1;

            if !state.saved_input_hashes.insert(input_hash) {
                return None;
            }

            if saves_duplicate_evidence(outcome)
                && state.duplicate_reproducers_saved < MAX_DUPLICATE_REPRODUCERS_PER_SIGNATURE
            {
                state.duplicate_reproducers_saved += 1;
                true
            } else {
                false
            }
        };

        if !should_save {
            return None;
        }

        eprintln!(
            "[{target}] new crash ({}) [signature: {signature}]",
            outcome.describe()
        );

        let crash_dir = self.crash_dir.as_ref()?;
        match write_reproducer(crash_dir, target, input, &signature, &outcome.describe()) {
            Ok(path) => {
                eprintln!("[{target}] saved reproducer: {}", path.display());
                Some(path)
            }
            Err(e) => {
                eprintln!("[{target}] warning: failed to save reproducer: {e}");
                None
            }
        }
    }

    /// Number of distinct crash signatures seen so far.
    pub fn distinct_signatures(&self) -> usize {
        self.signatures.len()
    }
}

/// Whether a bucket keeps bounded duplicate reproducers beyond its first.
///
/// Every crash class qualifies: none of their signatures is precise enough to
/// promise that two inputs in one bucket are the same bug. Panics were the
/// exception until a single site-only ICE bucket was observed swallowing every
/// graceful ICE after the first (see [`panic_signature`]).
fn saves_duplicate_evidence(outcome: &RunOutcome) -> bool {
    matches!(
        outcome,
        RunOutcome::Panic(_) | RunOutcome::Signal(_) | RunOutcome::Timeout(_)
    )
}

/// Write the crashing input to `crash_dir` under a name that includes both a
/// signature hash and an input hash, so identical inputs overwrite (not
/// accumulate) and distinct crashes never collide.
///
/// A `.meta` sibling records the target, signature, and human-readable crash
/// description, so a downloaded CI artifact is self-describing — without it
/// the crash class survives only as a non-invertible hash in the filename.
fn write_reproducer(
    crash_dir: &Path,
    target: &str,
    input: &[u8],
    signature: &str,
    description: &str,
) -> std::io::Result<PathBuf> {
    use std::io::Write;

    std::fs::create_dir_all(crash_dir)?;

    let input_hash = fnv1a(input);
    let sig_hash = fnv1a(signature.as_bytes()) as u32;

    let filename = format!("crash-{target}-{sig_hash:08x}-{input_hash:016x}.txt");
    let path = crash_dir.join(filename);

    let mut file = std::fs::File::create(&path)?;
    file.write_all(input)?;

    let meta = format!("target: {target}\nsignature: {signature}\noutcome: {description}\n");
    std::fs::write(path.with_extension("txt.meta"), meta)?;
    Ok(path)
}

/// FNV-1a hash — small, dependency-free, good enough for filenames.
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_run_is_ok() {
        let outcome = run_forked(|| {
            let _ = 1 + 1;
        });
        assert_eq!(outcome, RunOutcome::Ok);
        assert!(!outcome.is_crash());
        assert!(outcome.signature().is_none());
    }

    #[test]
    fn panic_is_detected_with_message() {
        let outcome = run_forked(|| {
            panic!("boom-42");
        });
        match &outcome {
            RunOutcome::Panic(msg) => assert!(msg.contains("boom-42"), "got: {msg}"),
            other => panic!("expected panic, got {other:?}"),
        }
        assert!(outcome.is_crash());
    }

    // The core RUE-43 regression: an *abort* (signal death, which the old
    // in-process catch_unwind harness could NOT see) must be reported as a
    // crash carrying the terminating signal.
    #[test]
    fn abort_is_detected_as_signal() {
        let outcome = run_forked(|| {
            std::process::abort();
        });
        match outcome {
            RunOutcome::Signal(sig) => assert_eq!(sig, libc::SIGABRT),
            other => panic!("expected SIGABRT signal, got {other:?}"),
        }
    }

    #[test]
    fn segfault_is_detected_as_signal() {
        let outcome = run_forked(|| {
            // Deliberate null dereference in the child only.
            unsafe {
                let p = std::ptr::null_mut::<u8>();
                std::ptr::write_volatile(p, 1);
            }
        });
        match outcome {
            RunOutcome::Signal(sig) => {
                assert!(
                    sig == libc::SIGSEGV || sig == libc::SIGBUS,
                    "got signal {sig}"
                )
            }
            other => panic!("expected a segfault signal, got {other:?}"),
        }
    }

    // The hang-detection analog of the RUE-43 abort test: a child that never
    // terminates must be killed at the deadline and reported as a Timeout,
    // not wedge the harness in waitpid forever.
    #[test]
    fn hang_is_detected_as_timeout() {
        let started = std::time::Instant::now();
        let outcome = run_forked_with_timeout(
            || loop {
                std::thread::sleep(Duration::from_millis(50));
            },
            Duration::from_millis(300),
        );
        assert_eq!(outcome, RunOutcome::Timeout(0)); // 300ms rounds to 0s
        assert!(outcome.is_crash());
        assert_eq!(outcome.signature().as_deref(), Some("timeout"));
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "harness must not block anywhere near beyond the deadline"
        );
    }

    // A child that hangs while holding the panic-message pipe open must not
    // block the parent's pipe read either (the read is deadline-bounded).
    #[test]
    fn hang_with_partial_pipe_write_is_still_a_timeout() {
        let outcome = run_forked_with_timeout(
            || {
                // Simulates a target that got partway through work then hung.
                loop {
                    std::hint::spin_loop();
                }
            },
            Duration::from_millis(300),
        );
        assert!(matches!(outcome, RunOutcome::Timeout(_)));
    }

    #[test]
    fn fast_run_is_not_a_timeout() {
        let outcome = run_forked_with_timeout(|| (), Duration::from_secs(30));
        assert_eq!(outcome, RunOutcome::Ok);
    }

    /// Two graceful ICEs share one `panic!` site inside `assert_no_ice`, so a
    /// site-only signature merged every ICE the fuzzer could find into one
    /// bucket. The broken invariant must discriminate them.
    #[test]
    fn distinct_ices_from_one_panic_site_get_distinct_signatures() {
        let site = "panicked at crates/rue-fuzz/src/targets.rs:38:17:";
        let terminal = RunOutcome::Panic(format!(
            "{site}\ngraceful ICE: error: [E9000]: internal compiler error: \
             reached body query Specialization did not publish a terminal"
        ));
        let producer = RunOutcome::Panic(format!(
            "{site}\ngraceful ICE: error: [E9000]: internal compiler error: \
             anonymous nominal producer did not publish the requested identity"
        ));
        assert_ne!(terminal.signature(), producer.signature());
    }

    /// The other half of the contract: one invariant reported over inputs that
    /// differ only in the payload it echoes back (module paths, symbol names,
    /// indices) must stay in a single bucket, or the crashes directory refloods.
    #[test]
    fn one_ice_over_varying_payload_keeps_one_signature() {
        let ice = |module: &str, index: u32| {
            RunOutcome::Panic(format!(
                "panicked at crates/rue-fuzz/src/targets.rs:38:17:\n\
                 graceful ICE: reached body query Specialization {{ module: \
                 ModuleId {{ logical_path: \"{module}\" }}, slot: {index} }} \
                 did not publish a terminal"
            ))
        };
        assert_eq!(
            ice("a.rue", 3).signature(),
            ice("other.rue", 1291).signature()
        );
    }

    /// A graceful ICE's own text is the finding. Keeping only the `panicked at`
    /// line left the downloaded `.meta` unable to say which bug it recorded.
    #[test]
    fn panic_description_carries_the_ice_text() {
        let outcome = RunOutcome::Panic(
            "panicked at crates/rue-fuzz/src/targets.rs:38:17:\n\
             graceful ICE: reached body query did not publish a terminal"
                .to_string(),
        );
        let described = outcome.describe();
        assert!(
            described.contains("did not publish a terminal"),
            "got: {described}"
        );
        assert!(
            !described.contains('\n'),
            "`.meta` is line-oriented: {described}"
        );
    }

    /// Verbatim message from the nightly `sema`/`payload_schemas` crash of
    /// 2026-07-28 (GitHub #1930). Its debug payload runs for ~200 characters
    /// before the phrase that names the broken invariant, so the signature is
    /// only discriminating if the payload collapses rather than being redacted
    /// character-by-character and then truncated.
    #[test]
    fn real_ice_signature_keeps_the_invariant_wording() {
        let outcome = RunOutcome::Panic(
            "panicked at crates/rue-fuzz/src/targets.rs:38:17:\n\
             graceful ICE: internal compiler error: reached body query \
             Specialization { base: Definition(StableDefinitionKey { module: \
             ModuleId { logical_path: \"<fuzz>\", origin: Caller }, namespace: \
             Value, kind: Function, name: \"Box\", owner: None }), arguments: \
             CanonicalArguments { types: [U64], values: [] } } did not publish \
             a terminal"
                .to_string(),
        );
        let signature = outcome.signature().unwrap();
        assert!(
            signature.contains("did not publish a terminal"),
            "the broken invariant must survive redaction and the cap: {signature}"
        );
        assert!(
            !signature.contains("Box") && !signature.contains("logical_path"),
            "input-derived payload must not reach the key: {signature}"
        );
    }

    #[test]
    fn oversized_panic_payload_is_bounded() {
        let outcome = RunOutcome::Panic(format!(
            "panicked at src/x.rs:1:1:\ngraceful ICE: {}",
            "payload ".repeat(500)
        ));
        assert!(outcome.signature().unwrap().chars().count() < 300);
        assert!(outcome.describe().chars().count() < 700);
    }

    /// Distinct ICEs also have to survive to disk, not merely be counted: a
    /// panic bucket that keeps only its first reproducer leaves later, different
    /// crashing inputs with no evidence at all (RUE-383, one class over).
    #[test]
    fn reporter_saves_bounded_panic_duplicate_evidence() {
        let dir = std::env::temp_dir().join(format!(
            "rue-fuzz-panic-duplicates-test-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let mut reporter = CrashReporter::new(Some(dir.clone()));
        let outcome = RunOutcome::Panic(
            "panicked at crates/rue-fuzz/src/targets.rs:38:17:\n\
             graceful ICE: reached body query did not publish a terminal"
                .to_string(),
        );

        assert!(
            reporter
                .report("sema", b"first-ice-input", &outcome)
                .is_some()
        );
        let second = reporter.report("sema", b"second-ice-input", &outcome);
        let second = second.expect("a distinct input in one ICE bucket must be saved");
        assert_eq!(std::fs::read(&second).unwrap(), b"second-ice-input");
        assert_eq!(reporter.unique_crashes, 1);
        assert_eq!(reporter.distinct_signatures(), 1);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn signal_and_panic_have_distinct_signatures() {
        let sig = RunOutcome::Signal(libc::SIGSEGV).signature();
        let pnc = RunOutcome::Panic("index out of bounds".to_string()).signature();
        assert!(sig.is_some() && pnc.is_some());
        assert_ne!(sig, pnc);
    }

    #[test]
    fn reporter_dedups_identical_crashes() {
        let dir = std::env::temp_dir().join(format!("rue-fuzz-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut reporter = CrashReporter::new(Some(dir.clone()));

        let outcome = RunOutcome::Signal(libc::SIGSEGV);

        // First occurrence: written.
        let first = reporter.report("parser", b"deep((((", &outcome);
        assert!(first.is_some(), "first crash should be saved");
        assert_eq!(reporter.unique_crashes, 1);
        assert_eq!(reporter.duplicate_crashes, 0);

        // Same coarse signature with different input bytes: saved as bounded
        // duplicate evidence, but still counted as a duplicate signature.
        let second = reporter.report("parser", b"deep[[[[", &outcome);
        let second = second.expect("distinct signal input should be saved");
        assert_eq!(std::fs::read(&second).unwrap(), b"deep[[[[");
        assert_eq!(reporter.unique_crashes, 1);
        assert_eq!(reporter.duplicate_crashes, 1);
        assert_eq!(reporter.distinct_signatures(), 1);

        // Exact same input reruns still dedup.
        let repeated = reporter.report("parser", b"deep[[[[", &outcome);
        assert!(
            repeated.is_none(),
            "same signal and same input should be deduped"
        );
        assert_eq!(reporter.unique_crashes, 1);
        assert_eq!(reporter.duplicate_crashes, 2);

        // A genuinely different crash: written.
        let other = reporter.report("parser", b"x", &RunOutcome::Panic("nope".into()));
        assert!(other.is_some(), "distinct crash should be saved");
        assert_eq!(reporter.unique_crashes, 2);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reporter_caps_duplicate_signal_reproducers() {
        let dir =
            std::env::temp_dir().join(format!("rue-fuzz-signal-cap-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut reporter = CrashReporter::new(Some(dir.clone()));
        let outcome = RunOutcome::Signal(libc::SIGSEGV);

        assert!(reporter.report("parser", b"first", &outcome).is_some());

        let mut duplicate_reproducers = 0;
        for i in 0..MAX_DUPLICATE_REPRODUCERS_PER_SIGNATURE {
            let input = format!("duplicate-{i}");
            if reporter
                .report("parser", input.as_bytes(), &outcome)
                .is_some()
            {
                duplicate_reproducers += 1;
            }
        }

        assert_eq!(
            duplicate_reproducers,
            MAX_DUPLICATE_REPRODUCERS_PER_SIGNATURE
        );
        assert!(
            reporter
                .report("parser", b"duplicate-overflow", &outcome)
                .is_none(),
            "duplicate signal reproducers should be capped"
        );
        assert_eq!(reporter.unique_crashes, 1);
        assert_eq!(
            reporter.duplicate_crashes,
            MAX_DUPLICATE_REPRODUCERS_PER_SIGNATURE + 1
        );
        assert_eq!(reporter.distinct_signatures(), 1);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reporter_saves_bounded_timeout_duplicate_evidence() {
        let dir = std::env::temp_dir().join(format!(
            "rue-fuzz-timeout-duplicates-test-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let mut reporter = CrashReporter::new(Some(dir.clone()));
        let outcome = RunOutcome::Timeout(5);

        assert!(reporter.report("parser", b"slow-one", &outcome).is_some());
        let duplicate = reporter.report("parser", b"slow-two", &outcome);
        let duplicate = duplicate.expect("distinct timeout input should be saved");
        let meta = std::fs::read_to_string(duplicate.with_extension("txt.meta")).unwrap();
        assert!(meta.contains("signature: timeout"), "meta: {meta}");
        assert_eq!(reporter.unique_crashes, 1);
        assert_eq!(reporter.duplicate_crashes, 1);
        assert_eq!(reporter.distinct_signatures(), 1);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn end_to_end_abort_is_saved_and_deduped() {
        // Prove the full path: an aborting closure -> detected as a crash ->
        // reproducer written -> re-running the same input dedups.
        let dir = std::env::temp_dir().join(format!("rue-fuzz-e2e-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut reporter = CrashReporter::new(Some(dir.clone()));
        let input = b"aborting-input";

        let outcome1 = run_forked(|| std::process::abort());
        assert!(outcome1.is_crash());
        let path = reporter.report("synthetic", input, &outcome1);
        let path = path.expect("abort should be saved as a reproducer");
        assert!(path.exists(), "reproducer file must exist on disk");
        assert_eq!(std::fs::read(&path).unwrap(), input);

        // The .meta sibling makes the artifact self-describing for triage.
        let meta = std::fs::read_to_string(path.with_extension("txt.meta")).unwrap();
        assert!(meta.contains("target: synthetic"), "meta: {meta}");
        assert!(meta.contains("signature: signal:SIGABRT"), "meta: {meta}");

        let outcome2 = run_forked(|| std::process::abort());
        assert!(reporter.report("synthetic", input, &outcome2).is_none());
        assert_eq!(reporter.unique_crashes, 1);
        assert_eq!(reporter.duplicate_crashes, 1);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
