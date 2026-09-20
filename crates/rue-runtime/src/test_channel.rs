//! The `rue test` structured failure channel (ADR-0083 §3 and §5.1).
//!
//! A dispatched test process inherits one dedicated descriptor, [`CHANNEL_FD`],
//! whose write end the runner drains with its own budget. The channel carries
//! newline-delimited JSON records: the dispatcher's terminal `complete` frame
//! and the `failure` frames assertion sugar emits before aborting. It is not a
//! security boundary — it exists so an accidental collision with a test's own
//! stdout or stderr cannot forge or truncate a verdict.
//!
//! Three properties are load-bearing:
//!
//! - **Only a test image writes.** The assertion family lowers identically
//!   everywhere (§5.1), so the runtime, not the compiler, decides whether a
//!   frame is written: the dispatcher's prologue arms the channel, and nothing
//!   else does. An ordinary executable's descriptor 3 belongs to whoever opened
//!   it — `prog 3>file`, or a program with a third file of its own — and it
//!   receives nothing (RUE-2066).
//! - **Writes are best-effort.** An armed image run by hand has no descriptor
//!   3, so `EBADF` is expected rather than exceptional; a runner that closed
//!   its read end yields `EPIPE`. Both are discarded. (`SIGPIPE` keeps its
//!   default disposition here exactly as it does for stdout, spec §8.5, so a
//!   closed reader terminates the process before `write` returns at all. The
//!   runner holds its read end open until the child exits precisely for this
//!   reason.)
//! - **No allocation, and no staging buffer.** A record is emitted as runs
//!   borrowed straight from the caller's own bytes, so no part of it is ever
//!   assembled in memory first. The only fixed-size buffer is the six bytes one
//!   escape occupies.
//!
//! A record's `message` is bounded to [`MESSAGE_BOUND`] because the channel has
//! a retention budget of its own and a record that exhausts it costs the test
//! its verdict: the runner kills the process group and publishes
//! `output_overflow` in place of the class and the exit status the record was
//! carrying. Stderr, which has no such role, still prints the message whole
//! within its own retention budget — and a message large enough to exhaust
//! that budget too still keeps its verdict, because the bounded record outranks
//! a stream's flood (RUE-2083).
//!
//! Records reserved by §5.1 and §5.2 — `promotion` payloads and per-case
//! `sub_result` identities — are named by the schema and produced by nothing in
//! this version.

#[cfg(all(test, rue_hosted_threads))]
extern crate std;

#[cfg(rue_hosted_threads)]
use core::cell::UnsafeCell;
#[cfg(rue_hosted_threads)]
use core::pin::Pin;
use core::sync::atomic::{AtomicBool, Ordering};

use rue_runtime_abi::{FailureReport, FailureSite};

use crate::platform;

/// Inherited failure-channel descriptor, pinned by the §3 exec contract.
pub const CHANNEL_FD: u64 = 3;

/// Whether this process is a test image, and so owns descriptor 3.
///
/// Only a test image's dispatcher prologue calls
/// [`crate::process::__rue_test_normalize_process`], which is the one place
/// that sets this. An ordinary executable never does, so it never writes a
/// frame — descriptor 3 there belongs to whoever opened it (`prog 3>file`, or
/// a program with a third file of its own), and the assertion family must not
/// scribble JSON into someone else's descriptor (RUE-2066).
///
/// `Relaxed` is sufficient: the prologue runs before any user code on the same
/// thread, so no other ordering could observe the store out of turn.
static CHANNEL_ARMED: AtomicBool = AtomicBool::new(false);

/// Arm the failure channel: this process is a test image dispatched by the
/// runner, so descriptor 3 is the channel's write end.
///
/// Called from the dispatcher prologue's process normalization and nowhere
/// else, which is exactly what makes "armed" mean "is a test image".
pub(crate) fn arm_channel() {
    CHANNEL_ARMED.store(true, Ordering::Relaxed);
}

#[cfg(rue_hosted_threads)]
struct ReportMutexStorage(UnsafeCell<crate::parking::ParkMutex>);
#[cfg(rue_hosted_threads)]
unsafe impl Sync for ReportMutexStorage {}
#[cfg(rue_hosted_threads)]
static REPORT_MUTEX: ReportMutexStorage =
    ReportMutexStorage(UnsafeCell::new(crate::parking::ParkMutex::new()));
#[cfg(rue_hosted_threads)]
static REPORT_MUTEX_READY: AtomicBool = AtomicBool::new(false);
#[cfg(all(rue_hosted_threads, test))]
static REPORT_TEST_INIT_LOCK: AtomicBool = AtomicBool::new(false);

#[cfg(all(test, rue_hosted_threads))]
const TEST_PARTIAL_FRAME_PATH_ENV: &str = "RUE_REPORT_TEST_FRAME_PATH";
#[cfg(all(test, rue_hosted_threads))]
const TEST_PARTIAL_STARTED_PATH_ENV: &str = "RUE_REPORT_TEST_STARTED_PATH";
#[cfg(all(test, rue_hosted_threads))]
const TEST_PARTIAL_CONTENTION_PATH_ENV: &str = "RUE_REPORT_TEST_CONTENTION_PATH";
#[cfg(all(test, rue_hosted_threads))]
const TEST_PARTIAL_RELEASE_PATH_ENV: &str = "RUE_REPORT_TEST_RELEASE_PATH";
#[cfg(all(test, rue_hosted_threads))]
static TEST_PARTIAL_BYTES: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(all(test, rue_hosted_threads))]
static TEST_PARTIAL_PAUSED: AtomicBool = AtomicBool::new(false);
#[cfg(all(test, rue_hosted_threads))]
static TEST_PARTIAL_CONTENTION_REPORTED: AtomicBool = AtomicBool::new(false);

#[cfg(rue_hosted_threads)]
pub(crate) unsafe fn initialize_hosted_reporting() -> Result<(), i32> {
    if REPORT_MUTEX_READY.load(Ordering::Acquire) {
        return Ok(());
    }
    // SAFETY: the startup hook calls this exactly once before user code or
    // worker threads can observe the runtime. The static address never moves.
    let storage = unsafe { &mut *REPORT_MUTEX.0.get() };
    let result = unsafe { Pin::new_unchecked(storage) }.init();
    if result.is_ok() {
        REPORT_MUTEX_READY.store(true, Ordering::Release);
    }
    result
}

/// Whether [`arm_channel`] has run. Exists so `process.rs` can assert that
/// normalizing the process is what arms the channel, under the serialization
/// its own process-global test already keeps.
#[cfg(test)]
pub(crate) fn channel_is_armed() -> bool {
    CHANNEL_ARMED.load(Ordering::Relaxed)
}

/// Ownership of the terminal report channel. A terminal reporter keeps this
/// gate until process termination; competing ordinary reporters park in the
/// compare/exchange loop and therefore cannot emit a second complete frame or
/// exit early. Signal and explicit-exit paths never enter this helper.
#[cfg(not(rue_hosted_threads))]
static REPORT_GATE: AtomicBool = AtomicBool::new(false);

/// The literal frame text, in field order. Every record carries the schema
/// version inline, so a reader never has to infer it.
///
/// These are value constants rather than `&'static [u8]` statics: the runtime's
/// message paths deliberately avoid static byte-string relocations, which have
/// misbuilt on the macOS linker before (see `error.rs`).
const COMPLETE_FRAME: [u8; 37] = *b"{\"record\":\"complete\",\"schema\":\"1.0\"}\n";
const FAILURE_HEAD: [u8; 43] = *b"{\"record\":\"failure\",\"schema\":\"1.0\",\"kind\":\"";
const MESSAGE_FIELD: [u8; 13] = *b"\",\"message\":\"";
const LOCATION_FIELD: [u8; 22] = *b"\",\"location\":{\"file\":\"";
const LINE_FIELD: [u8; 9] = *b"\",\"line\":";
const COLUMN_FIELD: [u8; 10] = *b",\"column\":";
const PAYLOAD_FIELD: [u8; 13] = *b"},\"payload\":\"";
const LEFT_FIELD: [u8; 10] = *b"},\"left\":\"";
const RIGHT_FIELD: [u8; 11] = *b"\",\"right\":\"";
const FRAME_TAIL: [u8; 3] = *b"\"}\n";
/// Closes the location object and the record with no field after it: the shape
/// a bare assertion writes, which has neither an open `payload` nor a pair of
/// operands to report.
const LOCATION_TAIL: [u8; 3] = *b"}}\n";

/// The kind `@assert` reports under, both spellings alike, so a consumer never
/// has to know which one failed.
const ASSERT_KIND: [u8; 6] = *b"assert";

/// The pinned message each comparison kind reports.
///
/// The message is chosen by the kind rather than passed while still carrying
/// both rendered operands. A kind this does not recognize
/// gets the bare `assertion failed`, so an unknown kind is still a report.
const ASSERT_EQ_KIND: [u8; 9] = *b"assert_eq";
const ASSERT_NE_KIND: [u8; 9] = *b"assert_ne";
const ASSERT_EQ_MESSAGE: [u8; 31] = *b"assertion failed: left == right";
const ASSERT_NE_MESSAGE: [u8; 31] = *b"assertion failed: left != right";
const ASSERT_MESSAGE: [u8; 16] = *b"assertion failed";

/// The kinds the runtime traps report under (RUE-2019).
///
/// These are the `trap:<class>` spellings the runner already publishes for a
/// trap it classified from stderr, so a trap that reports its own site changes
/// the record's `location` and nothing else: the same kind, and the same
/// message, reach the event stream either way.
const TRAP_PANIC_KIND: [u8; 10] = *b"trap:panic";
const TRAP_BOUNDS_CHECK_KIND: [u8; 17] = *b"trap:bounds_check";

/// The pinned stderr text each trap frame reports as its message, without the
/// trailing newline stderr carries.
///
/// A trap's record message is the line the runner would otherwise have read off
/// stderr, so the two agree byte for byte and the record adds only the site.
const PANIC_PREFIX: [u8; 7] = *b"panic: ";
const PANIC_MESSAGE: [u8; 5] = *b"panic";
const BOUNDS_CHECK_MESSAGE: [u8; 26] = *b"error: index out of bounds";

/// The pinned malformed-selector diagnostic (ADR-0083 §3).
const USAGE_MESSAGE: [u8; 50] = *b"rue-test: expected one 16-hex-digit test selector\n";

/// How many bytes of a record's `message` reach the channel.
///
/// This is [`rue_runtime_abi::RENDERING_BOUND`] — the same 4 KiB rendering
/// bound the `?` payload and the comparison operands are held to (spec
/// 6.7:15), read from the one crate the compiler's printer and this writer
/// both depend on rather than spelled again here — applied for a different
/// reason: those are bounded so a report stays readable, and this is bounded
/// so a report stays a report. The channel's retention budget is a quarter of
/// a stream's and JSON escaping can expand a control byte six-fold, so a
/// message a few tens of kilobytes long would exhaust the budget, and the
/// runner answers an exhausted channel by killing the process group and
/// publishing `output_overflow` — the trap's class and its exit status lost to
/// the length of its own text.
const MESSAGE_BOUND: usize = rue_runtime_abi::RENDERING_BOUND as usize;

/// Appended to a `message` the bound cut short, in the spelling spec 6.7:15
/// already fixes for a truncated rendering and
/// [`rue_runtime_abi::RENDERING_TRUNCATION_MARKER`] carries for both writers.
const TRUNCATION_MARKER: &[u8] = rue_runtime_abi::RENDERING_TRUNCATION_MARKER.as_bytes();

/// Emitter for one channel frame.
///
/// `emit` receives already-escaped bytes in order, and the frame is assembled
/// as a series of runs borrowed straight from the caller's own bytes — there is
/// no staging buffer. That is a size decision as much as an allocation one: a
/// buffer large enough to hold a frame would be zero-initialized through a
/// `memset`-family libcall the freestanding runtime does not export (on Darwin,
/// `bzero`), and the link would fail. Runs cost one `write(2)` each, which for
/// a record written once per failing test is not worth a buffer.
///
/// Production passes the descriptor writer; tests pass a collector, which is
/// what lets the exact frame bytes be asserted without a real pipe.
struct FrameWriter<'a> {
    emit: &'a mut dyn FnMut(&[u8]),
}

impl<'a> FrameWriter<'a> {
    fn new(emit: &'a mut dyn FnMut(&[u8])) -> Self {
        Self { emit }
    }

    fn raw(&mut self, bytes: &[u8]) {
        if !bytes.is_empty() {
            (self.emit)(bytes);
        }
    }

    /// Emit `bytes` as the body of a JSON string.
    ///
    /// A record has to be JSON *text*, and a Rue string is an arbitrary byte
    /// sequence: `@panic` and `@assert(c, msg)` both take one, and so does
    /// anything the standard library panics with. A byte that is not part of a
    /// well-formed UTF-8 sequence therefore cannot travel raw — the whole line
    /// would decode as malformed and the runner would drop a real `trap:panic`
    /// to a bare `exit` with a note about an unreadable channel.
    ///
    /// So this escapes what JSON requires — the quote, the backslash, and every
    /// control byte below `0x20` — and, of the bytes at or above `0x80`, only
    /// the ones that do not form a valid sequence. Each of those becomes
    /// `\u00xx`, which names the byte's own value and keeps the line JSON text.
    /// The field is not byte-reversible, though: an escaped byte and a genuine
    /// character of the same value decode to the same scalar, so a consumer
    /// that needs the exact bytes reads the stderr capture instead. Validating
    /// rather than escaping every high byte is what keeps an ordinary non-ASCII
    /// message byte-identical on the wire and legible in a report.
    ///
    /// Unescaped stretches are emitted as one run, so a message that needs no
    /// escaping costs exactly one write.
    fn escaped(&mut self, bytes: &[u8]) {
        const HEX: [u8; 16] = *b"0123456789abcdef";
        let mut run_start = 0;
        let mut index = 0;
        while index < bytes.len() {
            let byte = bytes[index];
            if byte >= 0x80 {
                // A valid sequence stays in the run whole; an invalid lead or
                // continuation byte is escaped one byte at a time, so the next
                // iteration re-examines the rest as a fresh sequence.
                if let Some(width) = utf8_sequence_width(&bytes[index..]) {
                    index += width;
                } else {
                    self.raw(&bytes[run_start..index]);
                    let escape = [
                        b'\\',
                        b'u',
                        b'0',
                        b'0',
                        HEX[usize::from(byte >> 4)],
                        HEX[usize::from(byte & 0x0f)],
                    ];
                    self.raw(&escape);
                    index += 1;
                    run_start = index;
                }
                continue;
            }
            let mut escape = [0u8; 6];
            let escape_len = match byte {
                b'"' => {
                    escape[0] = b'\\';
                    escape[1] = b'"';
                    2
                }
                b'\\' => {
                    escape[0] = b'\\';
                    escape[1] = b'\\';
                    2
                }
                value if value < 0x20 => {
                    escape[0] = b'\\';
                    escape[1] = b'u';
                    escape[2] = b'0';
                    escape[3] = b'0';
                    escape[4] = HEX[usize::from(value >> 4)];
                    escape[5] = HEX[usize::from(value & 0x0f)];
                    6
                }
                _ => 0,
            };
            if escape_len > 0 {
                self.raw(&bytes[run_start..index]);
                self.raw(&escape[..escape_len]);
                run_start = index + 1;
            }
            index += 1;
        }
        self.raw(&bytes[run_start..]);
    }

    /// Emit `value` as a JSON number.
    fn number(&mut self, value: u32) {
        // Ten digits hold every `u32`; the array is small enough to initialize
        // inline rather than through a `memset` libcall.
        let mut digits = [0u8; 10];
        let mut written = 0;
        let mut remaining = value;
        loop {
            digits[written] = b'0' + (remaining % 10) as u8;
            written += 1;
            remaining /= 10;
            if remaining == 0 {
                break;
            }
        }
        // The loop produced least-significant digit first.
        let mut reversed = [0u8; 10];
        for (index, digit) in digits[..written].iter().rev().enumerate() {
            reversed[index] = *digit;
        }
        self.raw(&reversed[..written]);
    }
}

/// The width of the well-formed multi-byte UTF-8 sequence `bytes` starts with,
/// or `None` when it does not start with one.
///
/// Well-formedness is [`crate::utf8`]'s table — Unicode's own Table 3-7, which
/// rejects an overlong encoding, a surrogate, and a scalar above `U+10FFFF` as
/// well as a truncated sequence. `serde_json` and every other reader applies
/// that same table, so anything looser here would let a line through that a
/// reader still rejects.
///
/// ASCII is not a case: `escaped` handles a byte below `0x80` on its own
/// path and only consults this one for `byte >= 0x80`, so a lead the table has
/// no row for — including an ASCII byte — is reported as `None` rather than as
/// a one-byte sequence.
fn utf8_sequence_width(bytes: &[u8]) -> Option<usize> {
    let lead = crate::utf8::lead(*bytes.first()?)?;
    // `bytes[1]` is in bounds because the width check short-circuits first and
    // every row is at least two bytes wide (`crate::utf8::Utf8Lead::width`).
    if bytes.len() < lead.width || !lead.accepts_first_continuation(bytes[1]) {
        return None;
    }
    // The lead byte's row already constrained the first continuation; the rest
    // are unconstrained trail bytes.
    let mut index = 2;
    while index < lead.width {
        if !crate::utf8::is_continuation(bytes[index]) {
            return None;
        }
        index += 1;
    }
    Some(lead.width)
}

/// Write the terminal completion frame.
///
/// The dispatcher's epilogue is the only producer: exit 0 with end of stream
/// and no completion frame is how the runner detects a test body that called
/// `std.exit(0)` before its assertions ran (§3, failure kind `incomplete`).
fn complete_frame(emit: &mut dyn FnMut(&[u8])) {
    let mut writer = FrameWriter::new(emit);
    writer.raw(&COMPLETE_FRAME);
}

/// Write every field the failure shapes share, through the open location
/// object: they differ only in what follows the column.
///
/// The message arrives as a list of runs rather than one view so a producer
/// whose text is spelled in pieces — a trap's pinned stderr line is its
/// `panic: ` prefix and then the programmer's own bytes — can report the whole
/// of it without joining the pieces in a buffer first. They are one JSON string
/// on the wire and one message to every consumer.
///
/// The runs are bounded together to [`MESSAGE_BOUND`]: the bound belongs to the
/// `message` field a reader sees, not to whichever producer's piece happened to
/// be long. A run the bound falls inside is cut at that byte, so a UTF-8
/// sequence the cut lands in the middle of reaches the record as the escapes
/// its leftover bytes get — still JSON text, which is the property that has to
/// hold.
fn failure_head(
    writer: &mut FrameWriter<'_>,
    kind: &[u8],
    message: &[&[u8]],
    file: &[u8],
    line: u32,
    column: u32,
) {
    writer.raw(&FAILURE_HEAD);
    writer.escaped(kind);
    writer.raw(&MESSAGE_FIELD);
    let mut remaining = MESSAGE_BOUND;
    for run in message {
        if run.len() > remaining {
            writer.escaped(&run[..remaining]);
            writer.raw(TRUNCATION_MARKER);
            break;
        }
        writer.escaped(run);
        remaining -= run.len();
    }
    writer.raw(&LOCATION_FIELD);
    writer.escaped(file);
    writer.raw(&LINE_FIELD);
    writer.number(line);
    writer.raw(&COLUMN_FIELD);
    writer.number(column);
}

/// Write one failure frame.
///
/// `kind`, `message`, `file`, and `payload` are borrowed byte views; `payload`
/// is the open, versioned extension point §5.1 reserves for assertion
/// libraries, and is empty when a producer has nothing structured to say.
fn failure_frame(
    emit: &mut dyn FnMut(&[u8]),
    kind: &[u8],
    message: &[u8],
    file: &[u8],
    line: u32,
    column: u32,
    payload: &[u8],
) {
    let mut writer = FrameWriter::new(emit);
    failure_head(&mut writer, kind, &[message], file, line, column);
    writer.raw(&PAYLOAD_FIELD);
    writer.escaped(payload);
    writer.raw(&FRAME_TAIL);
}

/// Write one comparison failure frame (ADR-0083 Phase 2.5).
///
/// It carries `left` and `right` where [`failure_frame`] carries the open
/// `payload`, and no `payload` at all: two rendered operands are not one string
/// a consumer has to split, and the runner computes the diff between them.
/// `left` and `right` are the operands in the order the source wrote them and
/// carry no role, so `@assert_eq(got, want)` — the conventional spelling, with
/// the observed value first — reads the way it is spelled.
///
/// The message is not a parameter: it is pinned by the kind while still
/// carrying both operands.
fn comparison_frame(
    emit: &mut dyn FnMut(&[u8]),
    kind: &[u8],
    file: &[u8],
    line: u32,
    column: u32,
    left: &[u8],
    right: &[u8],
) {
    let mut writer = FrameWriter::new(emit);
    failure_head(
        &mut writer,
        kind,
        &[comparison_message(kind)],
        file,
        line,
        column,
    );
    writer.raw(&LEFT_FIELD);
    writer.escaped(left);
    writer.raw(&RIGHT_FIELD);
    writer.escaped(right);
    writer.raw(&FRAME_TAIL);
}

/// The pinned message one comparison kind reports.
///
/// `assert_eq` and `assert_ne` are the only kinds the compiler emits. Another
/// producer — §5.1 makes the channel an open protocol — gets the bare
/// `assertion failed`, because a report with an unfamiliar kind is still a
/// report.
fn comparison_message(kind: &[u8]) -> &'static [u8] {
    if kind == ASSERT_EQ_KIND {
        &ASSERT_EQ_MESSAGE
    } else if kind == ASSERT_NE_KIND {
        &ASSERT_NE_MESSAGE
    } else {
        &ASSERT_MESSAGE
    }
}

/// Write one `@assert` failure frame (spec 4.13:5d).
///
/// The kind is pinned rather than passed — `@assert` is the only producer — and
/// the record ends at the location object: a bare assertion has no operands to
/// report and nothing structured to put in the open `payload`, and an empty
/// `payload` would claim otherwise.
fn assert_frame(emit: &mut dyn FnMut(&[u8]), message: &[u8], file: &[u8], line: u32, column: u32) {
    let mut writer = FrameWriter::new(emit);
    failure_head(&mut writer, &ASSERT_KIND, &[message], file, line, column);
    writer.raw(&LOCATION_TAIL);
}

/// Write one runtime-trap failure frame (RUE-2019).
///
/// The shape is the bare assertion's: a trap has no operands to report and
/// nothing structured for the open `payload`, so the record ends at the
/// location object. Its message is the pinned stderr line, spelled as the two
/// runs [`failure_head`] joins into one string: the `panic: ` prefix and the
/// programmer's own bytes.
fn trap_frame(
    emit: &mut dyn FnMut(&[u8]),
    kind: &[u8],
    message_prefix: &[u8],
    message: &[u8],
    file: &[u8],
    line: u32,
    column: u32,
) {
    let mut writer = FrameWriter::new(emit);
    failure_head(
        &mut writer,
        kind,
        &[message_prefix, message],
        file,
        line,
        column,
    );
    writer.raw(&LOCATION_TAIL);
}

/// Decode a caller-owned site. A null site is the ABI's absent-location form,
/// which
/// reads back as the empty file at 0:0 — the shape every reporting helper
/// already writes when its caller could not name a location, and the one the
/// runner answers by falling back to the test declaration's header.
unsafe fn site_view(site: *const FailureSite) -> (&'static [u8], u32, u32) {
    if site.is_null() {
        return (&[], 0, 0);
    }
    // SAFETY: the caller owns a readable FailureSite for the duration of the
    // terminal helper, as required by the runtime manifest.
    let site = unsafe { &*site };
    // SAFETY: the site descriptor's pointer/length pair is caller-owned and
    // valid for this call.
    let file = unsafe { view(site.file_ptr, site.file_len) };
    (file, (site.position >> 32) as u32, site.position as u32)
}

/// Report one runtime trap on the §5.1 channel, ahead of its pinned stderr
/// line and its abort (RUE-2019).
///
/// The kind is the `trap:<class>` spelling the runner already publishes for a
/// trap it classified from stderr, and the message is that same pinned stderr
/// line without its newline, so a trap that reports here changes the record's
/// `location` and nothing else.
///
/// The frame is written whether or not a site is available, exactly as the
/// assertion helpers write theirs: a trap reached without one — an allocation
/// failure, or a check the compiler emits below AIR — still reports, with the
/// empty location the runner answers from the test's header.
unsafe fn report_trap(
    site: *const FailureSite,
    kind: &[u8],
    message_prefix: &[u8],
    message: &[u8],
) {
    let (file, line, column) = unsafe { site_view(site) };
    acquire_terminal_report_gate();
    trap_frame(
        &mut emit_to_channel,
        kind,
        message_prefix,
        message,
        file,
        line,
        column,
    );
}

/// Report a `@panic(msg)` as a `trap:panic` failure (RUE-2019).
pub(crate) unsafe fn report_panic(site: *const FailureSite, message: &[u8]) {
    // SAFETY: inherited from the runtime ABI helper's caller contract.
    unsafe { report_trap(site, &TRAP_PANIC_KIND, &PANIC_PREFIX, message) };
}

/// Report a `@panic()` as a `trap:panic` failure, under the message-less
/// form's own pinned stderr line.
pub(crate) unsafe fn report_panic_no_message(site: *const FailureSite) {
    // SAFETY: inherited from the runtime ABI helper's caller contract.
    unsafe { report_trap(site, &TRAP_PANIC_KIND, &PANIC_MESSAGE, &[]) };
}

/// Report a failed bounds check as a `trap:bounds_check` failure.
pub(crate) fn report_bounds_check() {
    // SAFETY: a null site is the documented absent-location representation.
    unsafe {
        report_trap(
            core::ptr::null(),
            &TRAP_BOUNDS_CHECK_KIND,
            &BOUNDS_CHECK_MESSAGE,
            &[],
        )
    };
}

#[cfg(rue_hosted_threads)]
fn acquire_report_gate() -> crate::parking::ParkMutexGuard<'static> {
    #[cfg(test)]
    report_test_contention_attempt();

    #[cfg(test)]
    if !REPORT_MUTEX_READY.load(Ordering::Acquire) {
        // Unit tests call runtime helpers directly rather than through the
        // process entry hook; serialize first-use initialization so the
        // UnsafeCell-backed mutex is never initialized concurrently.
        while REPORT_TEST_INIT_LOCK
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            core::hint::spin_loop();
        }
        if !REPORT_MUTEX_READY.load(Ordering::Acquire) {
            let _ = unsafe { initialize_hosted_reporting() };
        }
        REPORT_TEST_INIT_LOCK.store(false, Ordering::Release);
    }
    if !REPORT_MUTEX_READY.load(Ordering::Acquire) {
        platform::exit(101);
    }
    // SAFETY: startup initialized the static before publishing READY, and it
    // remains pinned until process termination.
    let mutex = unsafe { &*REPORT_MUTEX.0.get() };
    match unsafe { Pin::new_unchecked(mutex) }.lock() {
        Ok(guard) => guard,
        Err(_) => platform::exit(101),
    }
}

#[cfg(all(rue_hosted_threads, test))]
fn report_test_contention_attempt() {
    if !TEST_PARTIAL_PAUSED.load(Ordering::Acquire)
        || TEST_PARTIAL_CONTENTION_REPORTED.load(Ordering::Acquire)
    {
        return;
    }
    let Some(path) = std::env::var_os(TEST_PARTIAL_CONTENTION_PATH_ENV) else {
        return;
    };
    if TEST_PARTIAL_CONTENTION_REPORTED
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_ok()
    {
        std::fs::write(path, b"contending").expect("publish gate contention marker");
    }
}

#[cfg(not(rue_hosted_threads))]
fn acquire_report_gate() {
    while REPORT_GATE
        .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
        .is_err()
    {
        core::hint::spin_loop();
    }
}

pub(crate) fn acquire_terminal_report_gate() {
    #[cfg(rue_hosted_threads)]
    {
        core::mem::forget(acquire_report_gate());
    }
    #[cfg(not(rue_hosted_threads))]
    acquire_report_gate();
}

/// Acquire the terminal gate for an ordinary runtime diagnostic and then write
/// its already-rendered stderr bytes before exiting. Framed reporters already
/// own this gate and use their private stderr leaves instead; this entry point
/// is for traps whose only observable output is stderr.
pub(crate) fn terminal_stderr(bytes: &[u8]) -> ! {
    acquire_terminal_report_gate();
    terminal_stderr_after_gate(bytes)
}

/// Write an ordinary diagnostic after its caller has already acquired the
/// terminal gate. Framed reporters and reports that also have a structured
/// record use this leaf to avoid recursive gate acquisition.
pub(crate) fn terminal_stderr_after_gate(bytes: &[u8]) -> ! {
    platform::write_stderr(bytes);
    platform::exit(101)
}

#[cfg(not(rue_hosted_threads))]
fn release_report_gate() {
    REPORT_GATE.store(false, Ordering::Release);
}

/// Best-effort write of already-framed bytes to the inherited channel, in a
/// test image only.
fn emit_to_channel(bytes: &[u8]) {
    write_when_armed(CHANNEL_ARMED.load(Ordering::Relaxed), bytes, &mut |frame| {
        #[cfg(all(test, rue_hosted_threads))]
        if emit_to_test_partial_sink(frame) {
            return;
        }
        // Discarded deliberately: `EBADF` when a test image is run by hand
        // without a channel, `EPIPE` when the runner is gone. Neither is
        // recoverable and neither should disturb the test's own result.
        let _ = platform::write_all(CHANNEL_FD, frame);
    });
}

#[cfg(all(test, rue_hosted_threads))]
fn emit_to_test_partial_sink(bytes: &[u8]) -> bool {
    use std::fs::{self, OpenOptions};
    use std::io::Write;
    use std::path::Path;
    use std::thread;
    use std::time::Duration;

    let Some(frame_path) = std::env::var_os(TEST_PARTIAL_FRAME_PATH_ENV) else {
        return false;
    };
    let Some(started_path) = std::env::var_os(TEST_PARTIAL_STARTED_PATH_ENV) else {
        return false;
    };
    let Some(release_path) = std::env::var_os(TEST_PARTIAL_RELEASE_PATH_ENV) else {
        return false;
    };

    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(frame_path)
        .expect("open partial report frame sink");
    let already_written = TEST_PARTIAL_BYTES.load(Ordering::Relaxed);
    const PAUSE_AFTER: usize = 24;
    if !TEST_PARTIAL_PAUSED.load(Ordering::Acquire)
        && already_written < PAUSE_AFTER
        && already_written.saturating_add(bytes.len()) >= PAUSE_AFTER
        && TEST_PARTIAL_PAUSED
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    {
        let split = PAUSE_AFTER - already_written;
        file.write_all(&bytes[..split])
            .expect("write partial report prefix");
        TEST_PARTIAL_BYTES.fetch_add(split, Ordering::Release);
        fs::write(started_path, b"paused").expect("publish partial report marker");
        while !Path::new(&release_path).exists() {
            thread::sleep(Duration::from_millis(1));
        }
        file.write_all(&bytes[split..])
            .expect("write remainder of partial report");
        TEST_PARTIAL_BYTES.fetch_add(bytes.len() - split, Ordering::Release);
    } else {
        file.write_all(bytes).expect("write report frame");
        TEST_PARTIAL_BYTES.fetch_add(bytes.len(), Ordering::Release);
    }
    true
}

/// The armed gate: a frame reaches the descriptor only in a test image.
///
/// This is what keeps the channel the runner's rather than the world's. The
/// assertion family lowers identically everywhere (§5.1), so without the gate
/// an ordinary executable run as `prog 3>file` — or one that simply opened a
/// third file of its own — would receive a JSON failure frame on a descriptor
/// it was never promised (RUE-2066). The passing path pays nothing and the
/// failing path pays one relaxed load.
///
/// The flag arrives as a parameter, and the descriptor as a sink, so the rule
/// is checked in-process without writing to whatever the *host's* descriptor 3
/// happens to be.
fn write_when_armed(armed: bool, bytes: &[u8], sink: &mut dyn FnMut(&[u8])) {
    if !armed {
        return;
    }
    sink(bytes);
}

/// Borrow `len` bytes at `ptr`, tolerating the null-with-zero-length form the
/// ABI permits for an absent view.
///
/// # Safety
///
/// When `len > 0`, `ptr` must address `len` initialized bytes that stay valid
/// for the call.
unsafe fn view<'a>(ptr: *const u8, len: u64) -> &'a [u8] {
    if len == 0 {
        return &[];
    }
    // SAFETY: the caller guarantees `len` readable bytes at a non-null `ptr`.
    unsafe { core::slice::from_raw_parts(ptr, len as usize) }
}

crate::define_runtime_implementation! {
    /// Write the terminal completion frame to the failure channel.
    ///
    /// # ABI
    ///
    /// ```text
    /// extern "C" fn __rue_test_complete()
    /// ```
    ///
    /// Called only from the synthesized dispatcher's epilogue, after the
    /// selected test body returns normally.
    pub extern "C" fn __rue_test_complete() {
        #[cfg(rue_hosted_threads)]
        {
            let _guard = acquire_report_gate();
            complete_frame(&mut emit_to_channel);
        }
        #[cfg(not(rue_hosted_threads))]
        {
            acquire_report_gate();
            complete_frame(&mut emit_to_channel);
            release_report_gate();
        }
    }
}

crate::define_runtime_implementation! {
    /// Report a structured test failure, then abort like any other trap.
    ///
    /// Writes one `failure` frame to the channel, carrying the location in the
    /// caller-owned report, and then takes the
    /// ordinary panic path: `panic: {message}\n` on stderr and exit 101. The
    /// frame goes first so a failure is recorded even if the stderr write is
    /// lost.
    ///
    /// # ABI
    ///
    /// ```text
    /// extern "C" fn __rue_test_fail(
    ///     report: *const FailureReport,
    /// ) -> !
    /// ```
    ///
    /// # Safety
    ///
    /// Each pointer/length pair must describe initialized bytes valid for the
    /// call, or be null with a zero length.
    pub unsafe extern "C" fn __rue_test_fail(report: *const FailureReport) -> ! {
        // SAFETY: the caller guarantees the report and all of its views remain
        // readable for the terminal helper.
        let report = unsafe { &*report };
        let (kind, message, payload) = unsafe {
            (
                view(report.kind_ptr, report.kind_len),
                view(report.first_ptr, report.first_len),
                view(report.second_ptr, report.second_len),
            )
        };
        let (file, line, column) = unsafe { site_view(&report.site) };
        acquire_terminal_report_gate();
        failure_frame(
            &mut emit_to_channel,
            kind,
            message,
            file,
            line,
            column,
            payload,
        );
        crate::error::panic_stderr(message)
    }
}

crate::define_runtime_implementation! {
    /// Report a structured comparison failure, then abort like any other trap.
    ///
    /// The comparison form of [`__rue_test_fail`] (ADR-0083 Phase 2.5). It
    /// writes a `failure` frame carrying the two rendered operands as
    /// `left` and `right` — and no open `payload` — then takes the
    /// ordinary panic path with the message its `kind` pins: `panic: assertion
    /// failed: left == right` on stderr and exit 101.
    ///
    /// Both halves matter in different builds. Inside a test image the frame is
    /// what the runner reads; in an ordinary executable the channel is unarmed,
    /// no frame is written at all, and the pinned stderr message is the whole
    /// report. `@assert_eq` therefore lowers the same way wherever it is
    /// written, and descriptor 3 stays the property of whoever opened it.
    ///
    /// # ABI
    ///
    /// ```text
    /// extern "C" fn __rue_test_fail_comparison(
    ///     report: *const FailureReport,
    /// ) -> !
    /// ```
    ///
    /// # Safety
    ///
    /// Each pointer/length pair must describe initialized bytes valid for the
    /// call, or be null with a zero length.
    pub unsafe extern "C" fn __rue_test_fail_comparison(report: *const FailureReport) -> ! {
        // SAFETY: the caller guarantees the report and all of its views remain
        // readable for the terminal helper.
        let report = unsafe { &*report };
        let (kind, left, right) = unsafe {
            (
                view(report.kind_ptr, report.kind_len),
                view(report.first_ptr, report.first_len),
                view(report.second_ptr, report.second_len),
            )
        };
        let (file, line, column) = unsafe { site_view(&report.site) };
        acquire_terminal_report_gate();
        comparison_frame(
            &mut emit_to_channel,
            kind,
            file,
            line,
            column,
            left,
            right,
        );
        crate::error::panic_stderr(comparison_message(kind))
    }
}

crate::define_runtime_implementation! {
    /// Report a failed `@assert`, then abort like any other trap.
    ///
    /// The `@assert` form of [`__rue_test_fail`] (spec 4.13:5d). It writes a
    /// `failure` frame of kind `assert` carrying whatever location
    /// carried by the caller-owned report, and then writes the pinned stderr
    /// line the assertion has always written and exits 101.
    ///
    /// `@assert` has two pinned stderr forms rather than one, which is why the
    /// form is a parameter instead of a second symbol. With `with_message`
    /// zero, the message is not read at all: the frame carries the pinned
    /// `assertion failed`, and so does stderr, through the same
    /// [`crate::error::__rue_assert_failed`] the assertion used before it
    /// reported anything. Otherwise the caller's text is both the frame's
    /// message and `@panic`'s: `panic: {message}`. An empty message is
    /// therefore still the message form — `@assert(c, "")` keeps printing
    /// `panic: ` — because the form is stated, not inferred from the length.
    ///
    /// Both halves matter in different builds. Inside a test image the frame is
    /// what the runner reads; in an ordinary executable the channel is unarmed,
    /// no frame is written at all, and the pinned stderr message is the whole
    /// report. `@assert` therefore lowers the same way wherever it is written,
    /// and descriptor 3 stays the property of whoever opened it.
    ///
    /// # ABI
    ///
    /// ```text
    /// extern "C" fn __rue_test_fail_assert(
    ///     report: *const FailureReport, with_message: u32,
    /// ) -> !
    /// ```
    ///
    /// # Safety
    ///
    /// `report` must point to a caller-owned descriptor whose views remain
    /// readable for the call.
    pub unsafe extern "C" fn __rue_test_fail_assert(
        report: *const FailureReport,
        with_message: u32,
    ) -> ! {
        // SAFETY: the caller guarantees the report and all of its views remain
        // readable for the terminal helper.
        let report = unsafe { &*report };
        let message = unsafe { view(report.first_ptr, report.first_len) };
        let (file, line, column) = unsafe { site_view(&report.site) };
        acquire_terminal_report_gate();
        let framed = if with_message == 0 {
            &ASSERT_MESSAGE[..]
        } else {
            message
        };
        assert_frame(
            &mut emit_to_channel,
            framed,
            file,
            line,
            column,
        );
        if with_message == 0 {
            crate::error::assert_failed_stderr()
        } else {
            crate::error::panic_stderr(message)
        }
    }
}

crate::define_runtime_implementation! {
    /// Write the pinned malformed-selector diagnostic to stderr and return.
    ///
    /// The dispatcher, not the runtime, owns the exit status for this case, so
    /// unlike every other stderr-writing runtime path this one returns rather
    /// than terminating (ADR-0083 §3: a malformed selector is exit 2).
    ///
    /// # ABI
    ///
    /// ```text
    /// extern "C" fn __rue_test_usage_error()
    /// ```
    pub extern "C" fn __rue_test_usage_error() {
        platform::write_stderr(&USAGE_MESSAGE);
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;
    use std::vec::Vec;

    #[cfg(rue_hosted_threads)]
    mod gate {
        use super::*;

        use std::fs;
        use std::ops::{Deref, DerefMut};
        use std::path::{Path, PathBuf};
        use std::process::{Child, Command, Output, Stdio};
        use std::thread;
        use std::time::{Duration, Instant};

        const GATE_CHILD_ENV: &str = "RUE_REPORT_GATE_CHILD";
        const GATE_TEST_NAME: &str = "test_channel::tests::gate::terminal_gate_subprocesses";

        #[cfg(unix)]
        unsafe extern "C" {
            fn close(fd: i32) -> i32;
            fn raise(signal: i32) -> i32;
        }

        struct ChildGuard(Option<Child>);

        impl ChildGuard {
            fn wait_with_output(&mut self) -> std::io::Result<Output> {
                self.0
                    .take()
                    .expect("owned reporting child")
                    .wait_with_output()
            }
        }

        impl Deref for ChildGuard {
            type Target = Child;

            fn deref(&self) -> &Child {
                self.0.as_ref().expect("owned reporting child")
            }
        }

        impl DerefMut for ChildGuard {
            fn deref_mut(&mut self) -> &mut Child {
                self.0.as_mut().expect("owned reporting child")
            }
        }

        impl Drop for ChildGuard {
            fn drop(&mut self) {
                if let Some(child) = &mut self.0 {
                    let _ = child.kill();
                    let _ = child.wait();
                }
            }
        }

        fn fail_child(child: &mut Child, message: &str) -> ! {
            // Explicit cleanup also covers panic-abort test binaries, where Drop
            // cannot run after a failed assertion.
            let _ = child.kill();
            let _ = child.wait();
            panic!("{message}");
        }

        fn assert_stays_parked(child: &mut Child, frame: &Path) {
            // The child publishes its marker immediately before locking. Leave it
            // free to attempt that lock while the first writer remains paused;
            // sleeping in the child would let a broken gate pass this test.
            let deadline = Instant::now() + Duration::from_millis(100);
            loop {
                match child.try_wait() {
                    Ok(None) => {}
                    Ok(Some(status)) => fail_child(
                        child,
                        &std::format!("ordinary reporter exited before frame release: {status}"),
                    ),
                    Err(error) => fail_child(
                        child,
                        &std::format!("cannot poll parked reporting child: {error}"),
                    ),
                }
                match fs::read(frame) {
                    Ok(bytes) if bytes == COMPLETE_FRAME[..24] => {}
                    Ok(bytes) => fail_child(
                        child,
                        &std::format!("paused report changed before release: {bytes:?}"),
                    ),
                    Err(error) => fail_child(
                        child,
                        &std::format!("cannot read paused reporting frame: {error}"),
                    ),
                }
                if Instant::now() >= deadline {
                    return;
                }
                thread::sleep(Duration::from_millis(2));
            }
        }

        fn release_frame(child: &mut Child, path: &Path) {
            if let Err(error) = fs::write(path, b"release") {
                fail_child(
                    child,
                    &std::format!("cannot release reporting frame: {error}"),
                );
            }
        }

        fn wait_for_marker(child: &mut Child, path: &Path) {
            let deadline = Instant::now() + Duration::from_secs(10);
            while !path.exists() {
                if Instant::now() >= deadline {
                    fail_child(
                        child,
                        &std::format!("reporting child did not publish {}", path.display()),
                    );
                }
                thread::sleep(Duration::from_millis(2));
            }
        }

        fn wait_for_child(child: &mut Child) -> std::process::ExitStatus {
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                match child.try_wait() {
                    Ok(Some(status)) => return status,
                    Ok(None) => {}
                    Err(error) => {
                        fail_child(child, &std::format!("cannot poll reporting child: {error}"))
                    }
                }
                if Instant::now() >= deadline {
                    fail_child(
                        child,
                        "reporting child did not terminate within ten seconds",
                    );
                }
                thread::sleep(Duration::from_millis(2));
            }
        }

        fn gate_test_paths(mode: &str) -> (PathBuf, PathBuf, PathBuf, PathBuf) {
            let stem = std::format!("rue-report-gate-{}-{mode}", std::process::id());
            let root = std::env::temp_dir();
            (
                root.join(std::format!("{stem}.frame")),
                root.join(std::format!("{stem}.started")),
                root.join(std::format!("{stem}.ordinary")),
                root.join(std::format!("{stem}.release")),
            )
        }

        fn spawn_gate_child(
            mode: &str,
            paths: &(PathBuf, PathBuf, PathBuf, PathBuf),
        ) -> ChildGuard {
            ChildGuard(Some(
                Command::new(std::env::current_exe().expect("current test binary"))
                    .args(["--exact", GATE_TEST_NAME, "--nocapture"])
                    .env(GATE_CHILD_ENV, mode)
                    .env(TEST_PARTIAL_FRAME_PATH_ENV, &paths.0)
                    .env(TEST_PARTIAL_STARTED_PATH_ENV, &paths.1)
                    .env(TEST_PARTIAL_CONTENTION_PATH_ENV, &paths.2)
                    .env(TEST_PARTIAL_RELEASE_PATH_ENV, &paths.3)
                    .env("__RUST_TEST_INVOKE", GATE_TEST_NAME)
                    .stdout(Stdio::null())
                    .stderr(Stdio::piped())
                    .spawn()
                    .expect("spawn reporting-gate child"),
            ))
        }

        fn remove_gate_test_paths(paths: &(PathBuf, PathBuf, PathBuf, PathBuf)) {
            for path in [&paths.0, &paths.1, &paths.2, &paths.3] {
                let _ = fs::remove_file(path);
            }
        }

        /// Exercise the real hosted ParkMutex with a frame that is observably
        /// partial. The ordinary overflow reporter must park behind the first
        /// writer and only emit stderr after the complete frame is released.
        /// Separate subprocesses prove that explicit exit and an arriving signal
        /// terminate promptly while another thread still owns the gate.
        #[test]
        fn terminal_gate_subprocesses() {
            if let Some(mode) = std::env::var_os(GATE_CHILD_ENV) {
                let mode = mode.to_string_lossy();
                arm_channel();
                let started = PathBuf::from(
                    std::env::var_os(TEST_PARTIAL_STARTED_PATH_ENV)
                        .expect("child reporting marker path"),
                );
                thread::spawn(|| crate::test_channel::__rue_test_complete());
                while !started.exists() {
                    thread::sleep(Duration::from_millis(1));
                }
                match mode.as_ref() {
                    "ordinary" => {
                        thread::spawn(move || crate::error::__rue_overflow());
                        loop {
                            thread::sleep(Duration::from_secs(1));
                        }
                    }
                    "input" => {
                        crate::io::initialize_hosted().expect("initialize hosted stdin");
                        // Closing stdin makes the real read syscall return EBADF;
                        // ReadLineFailure::Input must use the same terminal gate
                        // as arithmetic traps while the complete frame is held.
                        #[cfg(unix)]
                        unsafe {
                            let _ = close(0);
                        }
                        let mut out = rue_runtime_abi::OptionStrBufResult {
                            disc: u64::MAX,
                            ptr: core::ptr::null_mut(),
                            len: 0,
                            cap: 0,
                        };
                        // SAFETY: `out` is aligned writable sret storage for the
                        // runtime's public read-line ABI.
                        unsafe { crate::io::__rue_read_line(&mut out, 1, 0) };
                        panic!("closed stdin unexpectedly returned");
                    }
                    "raw-exit" => crate::platform::exit(37),
                    "signal" => {
                        let stack_marker = 0usize;
                        crate::fault::initialize_main(
                            (&stack_marker as *const usize) as usize,
                            crate::platform::stack_limit(),
                        )
                        .expect("initialize hosted fault handler");
                        // SAFETY: raising SIGSEGV is intentional subprocess-test
                        // control flow; Rue's installed handler exits 101.
                        let result = unsafe { raise(11) };
                        panic!("SIGSEGV handler returned: {result}");
                    }
                    other => panic!("unknown reporting-gate child mode: {other}"),
                }
            }

            let (frame, started, contention, release) = gate_test_paths("ordinary");
            let paths = (
                frame.clone(),
                started.clone(),
                contention.clone(),
                release.clone(),
            );
            let mut child = spawn_gate_child("ordinary", &paths);
            wait_for_marker(&mut child, &started);
            wait_for_marker(&mut child, &contention);
            assert_stays_parked(&mut child, &frame);
            release_frame(&mut child, &release);
            let status = wait_for_child(&mut child);
            let output = child
                .wait_with_output()
                .expect("collect ordinary reporter output");
            assert_eq!(status.code(), Some(101), "ordinary child: {output:?}");
            assert_eq!(output.stderr, b"error: integer overflow\n");
            assert_eq!(
                fs::read(&frame).expect("read complete frame"),
                COMPLETE_FRAME
            );
            remove_gate_test_paths(&paths);

            let (frame, started, contention, release) = gate_test_paths("input");
            let paths = (
                frame.clone(),
                started.clone(),
                contention.clone(),
                release.clone(),
            );
            let mut child = spawn_gate_child("input", &paths);
            wait_for_marker(&mut child, &started);
            wait_for_marker(&mut child, &contention);
            assert_stays_parked(&mut child, &frame);
            release_frame(&mut child, &release);
            let status = wait_for_child(&mut child);
            let output = child
                .wait_with_output()
                .expect("collect input reporter output");
            assert_eq!(status.code(), Some(101), "input child: {output:?}");
            assert_eq!(output.stderr, b"error: input error\n");
            assert_eq!(
                fs::read(&frame).expect("read complete frame after input error"),
                COMPLETE_FRAME
            );
            remove_gate_test_paths(&paths);

            let (frame, started, ordinary, release) = gate_test_paths("raw-exit");
            let paths = (frame, started.clone(), ordinary, release);
            let mut child = spawn_gate_child("raw-exit", &paths);
            wait_for_marker(&mut child, &started);
            let status = wait_for_child(&mut child);
            assert_eq!(status.code(), Some(37));
            remove_gate_test_paths(&paths);

            let (frame, started, ordinary, release) = gate_test_paths("signal");
            let paths = (frame, started.clone(), ordinary, release);
            let mut child = spawn_gate_child("signal", &paths);
            wait_for_marker(&mut child, &started);
            let status = wait_for_child(&mut child);
            let output = child
                .wait_with_output()
                .expect("collect signal reporter output");
            assert_eq!(status.code(), Some(101), "signal child: {output:?}");
            assert!(
                output.stderr.starts_with(b"segmentation fault at 0x"),
                "signal stderr: {:?}",
                output.stderr
            );
            remove_gate_test_paths(&paths);
        }
    }

    fn frame_bytes(build: impl FnOnce(&mut dyn FnMut(&[u8]))) -> Vec<u8> {
        let mut collected = Vec::new();
        let mut emit = |bytes: &[u8]| collected.extend_from_slice(bytes);
        build(&mut emit);
        collected
    }

    #[test]
    fn completion_frame_is_the_pinned_bytes() {
        let bytes = frame_bytes(complete_frame);
        assert_eq!(
            std::str::from_utf8(&bytes).unwrap(),
            "{\"record\":\"complete\",\"schema\":\"1.0\"}\n"
        );
    }

    #[test]
    fn failure_frame_is_the_pinned_field_order() {
        let bytes = frame_bytes(|emit| {
            failure_frame(
                emit,
                b"assert",
                b"assertion failed",
                b"app/parser_tests.rue",
                7,
                3,
                b"",
            )
        });
        assert_eq!(
            std::str::from_utf8(&bytes).unwrap(),
            "{\"record\":\"failure\",\"schema\":\"1.0\",\"kind\":\"assert\",\
             \"message\":\"assertion failed\",\
             \"location\":{\"file\":\"app/parser_tests.rue\",\"line\":7,\"column\":3},\
             \"payload\":\"\"}\n"
        );
    }

    /// The comparison frame's field order, and the two fields that make it a
    /// different shape rather than a payload convention: `left` and
    /// `right` in place of `payload`, which is absent entirely.
    #[test]
    fn comparison_frame_carries_left_and_right_instead_of_a_payload() {
        let bytes = frame_bytes(|emit| {
            comparison_frame(
                emit,
                b"assert_eq",
                b"app/parser_tests.rue",
                7,
                5,
                b"41",
                b"42",
            )
        });
        assert_eq!(
            std::str::from_utf8(&bytes).unwrap(),
            "{\"record\":\"failure\",\"schema\":\"1.0\",\"kind\":\"assert_eq\",\
             \"message\":\"assertion failed: left == right\",\
             \"location\":{\"file\":\"app/parser_tests.rue\",\"line\":7,\"column\":5},\
             \"left\":\"41\",\"right\":\"42\"}\n"
        );
    }

    /// The message is pinned by the kind, not passed, which is what keeps the
    /// comparison report with both rendered operands in its caller-owned
    /// descriptor.
    #[test]
    fn each_comparison_kind_pins_its_own_message() {
        let ne =
            frame_bytes(|emit| comparison_frame(emit, b"assert_ne", b"a.rue", 1, 1, b"7", b"7"));
        assert!(
            std::str::from_utf8(&ne)
                .unwrap()
                .contains("\"message\":\"assertion failed: left != right\""),
            "{}",
            std::str::from_utf8(&ne).unwrap()
        );
        // The channel is an open protocol (§5.1): a kind from somewhere else is
        // still reported, with the bare assertion message.
        let other = frame_bytes(|emit| comparison_frame(emit, b"lib_eq", b"a.rue", 1, 1, b"", b""));
        assert!(
            std::str::from_utf8(&other)
                .unwrap()
                .contains("\"kind\":\"lib_eq\",\"message\":\"assertion failed\""),
            "{}",
            std::str::from_utf8(&other).unwrap()
        );
    }

    /// Both operands are escaped by the same rule the message is, so a rendered
    /// value containing a quote, a backslash, or a newline cannot break the
    /// frame it travels in.
    #[test]
    fn comparison_operands_are_escaped_like_every_other_string() {
        let bytes = frame_bytes(|emit| {
            comparison_frame(
                emit,
                b"assert_eq",
                b"a.rue",
                1,
                1,
                b"line one\nline two",
                b"say \"hi\"\\",
            )
        });
        let rendered = std::str::from_utf8(&bytes).unwrap();
        assert!(
            rendered.contains("\"left\":\"line one\\u000aline two\""),
            "{rendered}"
        );
        assert!(
            rendered.contains("\"right\":\"say \\\"hi\\\"\\\\\"}"),
            "{rendered}"
        );
    }

    /// An empty rendering is a value, not an absent field: `@assert_eq` on two
    /// empty strings must still publish both sides.
    #[test]
    fn empty_comparison_operands_stay_present_as_empty_strings() {
        let bytes = frame_bytes(|emit| comparison_frame(emit, b"assert_eq", b"", 0, 0, b"", b""));
        assert_eq!(
            std::str::from_utf8(&bytes).unwrap(),
            "{\"record\":\"failure\",\"schema\":\"1.0\",\"kind\":\"assert_eq\",\
             \"message\":\"assertion failed: left == right\",\
             \"location\":{\"file\":\"\",\"line\":0,\"column\":0},\
             \"left\":\"\",\"right\":\"\"}\n"
        );
    }

    /// The `@assert` frame's field order, and the field it does not have: the
    /// record ends at the location object, so a consumer reading `payload`
    /// sees an absent field rather than an empty one that would claim the
    /// assertion had something structured to say.
    #[test]
    fn assert_frame_ends_at_the_location_and_carries_no_payload() {
        let bytes =
            frame_bytes(|emit| assert_frame(emit, &ASSERT_MESSAGE, b"app/parser_tests.rue", 7, 3));
        assert_eq!(
            std::str::from_utf8(&bytes).unwrap(),
            "{\"record\":\"failure\",\"schema\":\"1.0\",\"kind\":\"assert\",\
             \"message\":\"assertion failed\",\
             \"location\":{\"file\":\"app/parser_tests.rue\",\"line\":7,\"column\":3}}\n"
        );
    }

    /// `@assert(cond, msg)` reports the user's text as the frame's message and
    /// keeps the same shape: one kind for both forms, so a consumer never has
    /// to know which spelling failed.
    #[test]
    fn an_assert_message_replaces_the_pinned_one_in_the_same_shape() {
        let bytes = frame_bytes(|emit| assert_frame(emit, b"port must be free", b"a.rue", 12, 5));
        assert_eq!(
            std::str::from_utf8(&bytes).unwrap(),
            "{\"record\":\"failure\",\"schema\":\"1.0\",\"kind\":\"assert\",\
             \"message\":\"port must be free\",\
             \"location\":{\"file\":\"a.rue\",\"line\":12,\"column\":5}}\n"
        );
    }

    /// An assertion message is escaped by the same rule every other string is,
    /// so user text containing a quote or a newline cannot break its frame.
    #[test]
    fn an_assert_message_is_escaped_like_every_other_string() {
        let bytes = frame_bytes(|emit| assert_frame(emit, b"say \"hi\"\n", b"a.rue", 1, 1));
        assert_eq!(
            std::str::from_utf8(&bytes).unwrap(),
            "{\"record\":\"failure\",\"schema\":\"1.0\",\"kind\":\"assert\",\
             \"message\":\"say \\\"hi\\\"\\u000a\",\
             \"location\":{\"file\":\"a.rue\",\"line\":1,\"column\":1}}\n"
        );
    }

    #[test]
    fn strings_escape_quotes_backslashes_and_control_bytes_only() {
        let bytes = frame_bytes(|emit| {
            failure_frame(
                emit,
                b"assert",
                b"say \"hi\"\\\n\t\x00",
                b"a.rue",
                1,
                1,
                b"expected \x1f",
            )
        });
        let rendered = std::str::from_utf8(&bytes).unwrap();
        assert!(
            rendered.contains("\"message\":\"say \\\"hi\\\"\\\\\\u000a\\u0009\\u0000\""),
            "{rendered}"
        );
        assert!(
            rendered.contains("\"payload\":\"expected \\u001f\""),
            "{rendered}"
        );
    }

    /// A well-formed sequence travels raw and an ill-formed byte is escaped, so
    /// a message that mixes the two stays legible where it can be and stays
    /// JSON text where it cannot.
    #[test]
    fn valid_utf8_travels_raw_and_an_invalid_byte_is_escaped() {
        let bytes = frame_bytes(|emit| {
            failure_frame(
                emit,
                b"assert",
                &[0xe2, 0x9c, 0x93, 0xff],
                b"a.rue",
                1,
                1,
                b"",
            )
        });
        let rendered = std::str::from_utf8(&bytes).unwrap();
        assert!(
            rendered.contains("\"message\":\"\u{2713}\\u00ff\""),
            "{rendered}"
        );
    }

    /// Every ill-formed shape Unicode's Table 3-7 rejects: a bare continuation
    /// byte, a lead byte with no continuation after it, an overlong encoding, a
    /// surrogate, and a scalar above `U+10FFFF`. Each contributes exactly its
    /// own bytes as escapes, so nothing is dropped and nothing is invented.
    #[test]
    fn every_ill_formed_sequence_is_escaped_byte_by_byte() {
        for (input, expected) in [
            (&[0x80u8][..], "\\u0080"),
            (&[0xc2][..], "\\u00c2"),
            (&[0xc2, 0x41][..], "\\u00c2A"),
            // Overlong: `/` encoded in two bytes.
            (&[0xc0, 0xaf][..], "\\u00c0\\u00af"),
            // Surrogate U+D800.
            (&[0xed, 0xa0, 0x80][..], "\\u00ed\\u00a0\\u0080"),
            // Above U+10FFFF.
            (
                &[0xf4, 0x90, 0x80, 0x80][..],
                "\\u00f4\\u0090\\u0080\\u0080",
            ),
            (&[0xff, 0xfe][..], "\\u00ff\\u00fe"),
            // Truncated four-byte sequence at the end of the message.
            (&[0xf0, 0x9f][..], "\\u00f0\\u009f"),
        ] {
            let bytes =
                frame_bytes(|emit| failure_frame(emit, b"assert", input, b"a.rue", 1, 1, b""));
            let rendered = std::str::from_utf8(&bytes).unwrap();
            assert!(
                rendered.contains(&std::format!("\"message\":\"{expected}\"")),
                "{input:?}: {rendered}"
            );
        }
    }

    /// Every well-formed shape reaches the record byte-identical: two, three,
    /// and four-byte sequences, and the boundary scalars of each range.
    #[test]
    fn every_well_formed_sequence_travels_raw() {
        for text in [
            "\u{80}",
            "\u{7ff}",
            "\u{800}",
            "\u{d7ff}",
            "\u{e000}",
            "\u{ffff}",
            "\u{10000}",
            "\u{10ffff}",
        ] {
            let bytes = frame_bytes(|emit| {
                failure_frame(emit, b"assert", text.as_bytes(), b"a.rue", 1, 1, b"")
            });
            let rendered = std::str::from_utf8(&bytes).unwrap();
            assert!(
                rendered.contains(&std::format!("\"message\":\"{text}\"")),
                "{text:?}: {rendered}"
            );
        }
    }

    /// A message exactly at the bound is emitted in order across its runs and
    /// carries no marker: the bound is the last length that fits, not the first
    /// that truncates.
    #[test]
    fn a_message_at_the_bound_is_emitted_whole_across_runs() {
        let long = std::vec![b'x'; MESSAGE_BOUND];
        let bytes = frame_bytes(|emit| failure_frame(emit, b"assert", &long, b"a.rue", 1, 1, b""));
        let rendered = std::str::from_utf8(&bytes).unwrap();
        assert!(rendered.starts_with("{\"record\":\"failure\","));
        assert!(rendered.ends_with("\"payload\":\"\"}\n"));
        assert_eq!(rendered.matches('x').count(), long.len());
        assert!(!rendered.contains("[truncated]"), "{}", &rendered[..80]);
    }

    /// One byte past the bound is cut to it and marked. Without this the record
    /// grows with the message, and a message a few tens of kilobytes long
    /// exhausts the channel's retention budget — which costs the test its whole
    /// verdict, not just the tail of its text (RUE-2064).
    #[test]
    fn a_message_past_the_bound_is_cut_to_it_and_marked() {
        let long = std::vec![b'x'; MESSAGE_BOUND + 1];
        let bytes = frame_bytes(|emit| failure_frame(emit, b"assert", &long, b"a.rue", 1, 1, b""));
        let rendered = std::str::from_utf8(&bytes).unwrap();
        assert_eq!(rendered.matches('x').count(), MESSAGE_BOUND);
        assert!(
            rendered.contains(&std::format!(
                "x \u{2026}[truncated]\",\"location\":{{\"file\":\"a.rue\""
            )),
            "{}",
            &rendered[rendered.len() - 80..]
        );
    }

    /// The bound belongs to the `message` field, so a trap's pinned prefix
    /// counts toward it: the field a reader sees is 4096 bytes plus the marker
    /// however many runs the producer spelled it in.
    #[test]
    fn a_traps_prefix_counts_against_the_same_bound() {
        let long = std::vec![b'x'; MESSAGE_BOUND];
        let bytes = frame_bytes(|emit| {
            trap_frame(emit, &TRAP_PANIC_KIND, &PANIC_PREFIX, &long, b"a.rue", 1, 1)
        });
        let rendered = std::str::from_utf8(&bytes).unwrap();
        assert_eq!(
            rendered.matches('x').count(),
            MESSAGE_BOUND - PANIC_PREFIX.len()
        );
        assert!(
            rendered.contains("\"message\":\"panic: x"),
            "{}",
            &rendered[..80]
        );
        assert!(rendered.contains(" \u{2026}[truncated]\","));
    }

    /// A cut that lands inside a UTF-8 sequence leaves that sequence's head
    /// bytes ill-formed, and the escaper answers them the way it answers any
    /// other ill-formed byte. The record stays JSON text, which is the property
    /// truncation must not be able to break.
    #[test]
    fn a_cut_inside_a_sequence_still_yields_json_text() {
        // 4095 filler bytes, then a three-byte sequence the bound splits after
        // its first byte.
        let mut long = std::vec![b'x'; MESSAGE_BOUND - 1];
        long.extend_from_slice("\u{2713}".as_bytes());
        let bytes = frame_bytes(|emit| failure_frame(emit, b"assert", &long, b"a.rue", 1, 1, b""));
        let rendered = std::str::from_utf8(&bytes).unwrap();
        assert!(
            rendered.contains("x\\u00e2 \u{2026}[truncated]\""),
            "{}",
            &rendered[rendered.len() - 80..]
        );
    }

    #[test]
    fn numbers_render_without_padding() {
        let bytes = frame_bytes(|emit| failure_frame(emit, b"k", b"m", b"f", 0, u32::MAX, b""));
        let rendered = std::str::from_utf8(&bytes).unwrap();
        assert!(
            rendered.contains("\"line\":0,\"column\":4294967295"),
            "{rendered}"
        );
    }

    #[test]
    fn a_caller_owned_site_round_trips_through_the_packed_position() {
        let file = b"app/parser_tests.rue";
        let site = FailureSite {
            file_ptr: file.as_ptr(),
            file_len: file.len() as u64,
            position: (7u64 << 32) | 3,
        };
        let (viewed_file, line, column) = unsafe { site_view(&site) };
        assert_eq!(viewed_file, file);
        assert_eq!((line, column), (7, 3));
    }

    /// A trap's frame is the bare assertion's shape — no `payload`, no operands
    /// — under the `trap:<class>` kind the runner already publishes, carrying
    /// the pinned stderr line as its message (RUE-2019).
    #[test]
    fn a_trap_frame_carries_its_class_and_pinned_message() {
        let bytes = frame_bytes(|emit| {
            trap_frame(
                emit,
                &TRAP_PANIC_KIND,
                &PANIC_PREFIX,
                b"boom",
                b"app/parser_tests.rue",
                21,
                5,
            )
        });
        assert_eq!(
            std::str::from_utf8(&bytes).unwrap(),
            "{\"record\":\"failure\",\"schema\":\"1.0\",\"kind\":\"trap:panic\",\
             \"message\":\"panic: boom\",\
             \"location\":{\"file\":\"app/parser_tests.rue\",\"line\":21,\"column\":5}}\n"
        );
    }

    /// The two-run message is one JSON string, and each run is escaped: a
    /// programmer's own text reaches the record intact however it is spelled.
    #[test]
    fn a_trap_message_escapes_both_of_its_runs() {
        let bytes = frame_bytes(|emit| {
            trap_frame(emit, &TRAP_PANIC_KIND, b"a\"b", b"c\nd", b"a.rue", 1, 1)
        });
        assert!(
            std::str::from_utf8(&bytes)
                .unwrap()
                .contains("\"message\":\"a\\\"bc\\u000ad\""),
            "{}",
            std::str::from_utf8(&bytes).unwrap()
        );
    }

    /// A trap reached with no caller-owned site — an allocation failure, or a check
    /// the compiler emits below AIR — still reports. The empty location is what
    /// the runner answers from the test declaration's header, so the record is
    /// never worse than the stderr classification it replaces.
    #[test]
    fn a_trap_without_site_names_no_file() {
        let bytes = frame_bytes(|emit| {
            trap_frame(
                emit,
                &TRAP_BOUNDS_CHECK_KIND,
                &BOUNDS_CHECK_MESSAGE,
                b"",
                b"",
                0,
                0,
            )
        });
        assert_eq!(
            std::str::from_utf8(&bytes).unwrap(),
            "{\"record\":\"failure\",\"schema\":\"1.0\",\"kind\":\"trap:bounds_check\",\
             \"message\":\"error: index out of bounds\",\
             \"location\":{\"file\":\"\",\"line\":0,\"column\":0}}\n"
        );
    }

    /// Each pinned trap message is the stderr line `error.rs` writes, without
    /// its newline: the record and stderr must not be able to drift apart.
    #[test]
    fn pinned_trap_messages_match_the_stderr_lines() {
        assert_eq!(&PANIC_PREFIX[..], b"panic: ");
        assert_eq!(&PANIC_MESSAGE[..], b"panic");
        assert_eq!(&BOUNDS_CHECK_MESSAGE[..], b"error: index out of bounds");
        assert_eq!(&TRAP_PANIC_KIND[..], b"trap:panic");
        assert_eq!(&TRAP_BOUNDS_CHECK_KIND[..], b"trap:bounds_check");
    }

    /// The RUE-2066 rule, in both directions. An unarmed process is an
    /// ordinary executable, whose descriptor 3 belongs to whoever opened it, so
    /// nothing at all is written; an armed one is a test image, and the frame
    /// reaches the sink byte for byte.
    #[test]
    fn a_frame_reaches_the_descriptor_only_while_armed() {
        let mut written = Vec::new();
        write_when_armed(false, b"{}\n", &mut |frame| {
            written.extend_from_slice(frame)
        });
        assert!(written.is_empty(), "{:?}", written);

        write_when_armed(true, b"{}\n", &mut |frame| written.extend_from_slice(frame));
        assert_eq!(&written[..], b"{}\n");
    }

    /// Arming is what [`emit_to_channel`] reads, so the two halves of the rule
    /// meet here: `arm_channel` flips exactly the flag the gate consults. The
    /// flag is never cleared and `process.rs` owns the assertion that the
    /// dispatcher prologue is what calls this, so nothing here depends on how
    /// the unit tests interleave.
    #[test]
    fn arming_the_channel_opens_the_gate() {
        arm_channel();
        assert!(channel_is_armed());
        write_when_armed(
            CHANNEL_ARMED.load(Ordering::Relaxed),
            b"{}\n",
            &mut |frame| assert_eq!(frame, b"{}\n"),
        );
    }

    #[test]
    fn writing_to_a_closed_descriptor_is_tolerated() {
        // The channel is absent whenever an armed test image is run by hand, so
        // an `EBADF` write must return rather than abort. A descriptor far above
        // anything the harness opens stands in for that.
        let _ = platform::write_all(1_000_000, b"{}\n");
    }
}
