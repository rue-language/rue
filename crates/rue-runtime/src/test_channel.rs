//! The `rue test` structured failure channel (ADR-0083 §3 and §5.1).
//!
//! A dispatched test process inherits one dedicated descriptor, [`CHANNEL_FD`],
//! whose write end the runner drains with its own budget. The channel carries
//! newline-delimited JSON records: the dispatcher's terminal `complete` frame
//! and the `failure` frames assertion sugar emits before aborting. It is not a
//! security boundary — it exists so an accidental collision with a test's own
//! stdout or stderr cannot forge or truncate a verdict.
//!
//! Two properties are load-bearing:
//!
//! - **Writes are best-effort.** A test run by hand has no descriptor 3, so
//!   `EBADF` is expected rather than exceptional; a runner that closed its read
//!   end yields `EPIPE`. Both are discarded. (`SIGPIPE` keeps its default
//!   disposition here exactly as it does for stdout, spec §8.5, so a closed
//!   reader terminates the process before `write` returns at all. The runner
//!   holds its read end open until the child exits precisely for this reason.)
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
//! within its own retention budget — a message large enough to exhaust that
//! budget too still overflows there.
//!
//! Records reserved by §5.1 and §5.2 — `promotion` payloads and per-case
//! `sub_result` identities — are named by the schema and produced by nothing in
//! this version.

use core::sync::atomic::{AtomicPtr, AtomicU64, Ordering};

use crate::platform;

/// Inherited failure-channel descriptor, pinned by the §3 exec contract.
pub const CHANNEL_FD: u64 = 3;

/// The failing source location staged by the most recent
/// [`__rue_test_failure_site`] call.
///
/// A failure record carries three byte views plus a file, a line, and a column
/// — ten arguments, where every runtime helper is register-only and x86-64
/// affords six. The record is therefore assembled by two calls, and this holds
/// the first one's result until the second consumes it. Generated code emits
/// the pair adjacently with nothing in between, and the process aborts inside
/// the second, so the window is a straight line with no other Rue code in it.
///
/// The pointer is borrowed rather than copied: the runtime allocates nothing,
/// and the caller's obligation to keep those bytes readable across the pair is
/// exactly what the manifest's `READABLE_BYTES` contract records.
static SITE_FILE: AtomicPtr<u8> = AtomicPtr::new(core::ptr::null_mut());
static SITE_FILE_LEN: AtomicU64 = AtomicU64::new(0);
/// Line in the high half, column in the low half. One word rather than two
/// keeps the staged site to three statics, and neither field is meaningful
/// without the other.
static SITE_POSITION: AtomicU64 = AtomicU64::new(0);

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
/// The message is chosen by the kind rather than passed, which is what keeps
/// the comparison call to the six registers every runtime helper is limited to
/// while still carrying both rendered operands. A kind this does not recognize
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

/// The width of the well-formed UTF-8 sequence `bytes` starts with, or `None`
/// when it does not start with one.
///
/// The ranges are Unicode's own well-formed byte sequences (Table 3-7), which
/// reject an overlong encoding, a surrogate, and a scalar above `U+10FFFF` as
/// well as a truncated sequence — `serde_json` and every other reader applies
/// the same table, so anything looser here would let a line through that a
/// reader still rejects. Written out rather than delegated to
/// `core::str::from_utf8` so the freestanding build's emitted code stays a
/// handful of comparisons with no libcall in it.
fn utf8_sequence_width(bytes: &[u8]) -> Option<usize> {
    let lead = *bytes.first()?;
    let (width, first_continuation) = match lead {
        0x00..=0x7f => return Some(1),
        0xc2..=0xdf => (2, 0x80..=0xbf),
        0xe0 => (3, 0xa0..=0xbf),
        0xe1..=0xec | 0xee..=0xef => (3, 0x80..=0xbf),
        0xed => (3, 0x80..=0x9f),
        0xf0 => (4, 0x90..=0xbf),
        0xf1..=0xf3 => (4, 0x80..=0xbf),
        0xf4 => (4, 0x80..=0x8f),
        _ => return None,
    };
    if bytes.len() < width || !first_continuation.contains(&bytes[1]) {
        return None;
    }
    // The lead byte's range already constrained the first continuation; the
    // rest are unconstrained trail bytes.
    let mut index = 2;
    while index < width {
        if !(0x80..=0xbf).contains(&bytes[index]) {
            return None;
        }
        index += 1;
    }
    Some(width)
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
/// The message is not a parameter: it is pinned by the kind, which is what
/// keeps this call to the six registers a runtime helper is limited to while
/// still carrying both operands.
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

/// The site [`__rue_test_failure_site`] most recently staged.
///
/// An unstaged site is the null-with-zero-length form the ABI permits, which
/// reads back as the empty file at 0:0 — the shape every reporting helper
/// already writes when its caller could not name a location, and the one the
/// runner answers by falling back to the test declaration's header.
fn staged_site() -> (&'static [u8], u32, u32) {
    let position = SITE_POSITION.load(Ordering::Relaxed);
    // SAFETY: a staged site's bytes stay readable across the pair, and an
    // unstaged one is the null-with-zero-length form `view` accepts.
    let file = unsafe {
        view(
            SITE_FILE.load(Ordering::Relaxed) as *const u8,
            SITE_FILE_LEN.load(Ordering::Relaxed),
        )
    };
    (file, (position >> 32) as u32, position as u32)
}

/// Report one runtime trap on the §5.1 channel, ahead of its pinned stderr
/// line and its abort (RUE-2019).
///
/// The kind is the `trap:<class>` spelling the runner already publishes for a
/// trap it classified from stderr, and the message is that same pinned stderr
/// line without its newline, so a trap that reports here changes the record's
/// `location` and nothing else.
///
/// The frame is written whether or not a site was staged, exactly as the
/// assertion helpers write theirs: a trap reached without one — an allocation
/// failure, or a check the compiler emits below AIR — still reports, with the
/// empty location the runner answers from the test's header.
fn report_trap(kind: &[u8], message_prefix: &[u8], message: &[u8]) {
    let (file, line, column) = staged_site();
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
pub(crate) fn report_panic(message: &[u8]) {
    report_trap(&TRAP_PANIC_KIND, &PANIC_PREFIX, message);
}

/// Report a `@panic()` as a `trap:panic` failure, under the message-less
/// form's own pinned stderr line.
pub(crate) fn report_panic_no_message() {
    report_trap(&TRAP_PANIC_KIND, &PANIC_MESSAGE, &[]);
}

/// Report a failed bounds check as a `trap:bounds_check` failure.
pub(crate) fn report_bounds_check() {
    report_trap(&TRAP_BOUNDS_CHECK_KIND, &BOUNDS_CHECK_MESSAGE, &[]);
}

/// Best-effort write of already-framed bytes to the inherited channel.
fn emit_to_channel(bytes: &[u8]) {
    // Discarded deliberately: `EBADF` when the program was run by hand without
    // a channel, `EPIPE` when the runner is gone. Neither is recoverable and
    // neither should disturb the test's own result.
    let _ = platform::write_all(CHANNEL_FD, bytes);
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
        complete_frame(&mut emit_to_channel);
    }
}

crate::define_runtime_implementation! {
    /// Stage the source location the next failure record will carry.
    ///
    /// Paired with [`__rue_test_fail`], which consumes it. Nothing clears the
    /// staging afterwards, because nothing runs afterwards: the consumer aborts
    /// the process. A site staged without a following failure would be adopted
    /// by the next one, which generated code never allows — it emits the two
    /// calls together.
    ///
    /// # ABI
    ///
    /// ```text
    /// extern "C" fn __rue_test_failure_site(
    ///     file_ptr: *const u8, file_len: u64, line: u32, column: u32,
    /// )
    /// ```
    ///
    /// # Safety
    ///
    /// `file_ptr`/`file_len` must describe initialized bytes that stay valid
    /// until the paired `__rue_test_fail` has written its record, or be null
    /// with a zero length.
    pub unsafe extern "C" fn __rue_test_failure_site(
        file_ptr: *const u8,
        file_len: u64,
        line: u32,
        column: u32,
    ) {
        SITE_FILE.store(file_ptr as *mut u8, Ordering::Relaxed);
        SITE_FILE_LEN.store(file_len, Ordering::Relaxed);
        SITE_POSITION.store(
            (u64::from(line) << 32) | u64::from(column),
            Ordering::Relaxed,
        );
    }
}

crate::define_runtime_implementation! {
    /// Report a structured test failure, then abort like any other trap.
    ///
    /// Writes one `failure` frame to the channel — carrying whatever location
    /// [`__rue_test_failure_site`] staged, or none — and then takes the
    /// ordinary panic path: `panic: {message}\n` on stderr and exit 101. The
    /// frame goes first so a failure is recorded even if the stderr write is
    /// lost.
    ///
    /// # ABI
    ///
    /// ```text
    /// extern "C" fn __rue_test_fail(
    ///     kind_ptr: *const u8, kind_len: u64,
    ///     message_ptr: *const u8, message_len: u64,
    ///     payload_ptr: *const u8, payload_len: u64,
    /// ) -> !
    /// ```
    ///
    /// # Safety
    ///
    /// Each pointer/length pair must describe initialized bytes valid for the
    /// call, or be null with a zero length.
    pub unsafe extern "C" fn __rue_test_fail(
        kind_ptr: *const u8,
        kind_len: u64,
        message_ptr: *const u8,
        message_len: u64,
        payload_ptr: *const u8,
        payload_len: u64,
    ) -> ! {
        // SAFETY: the caller guarantees every pair describes readable bytes.
        let message = unsafe { view(message_ptr, message_len) };
        // SAFETY: as above.
        let (kind, payload) = unsafe {
            (
                view(kind_ptr, kind_len),
                view(payload_ptr, payload_len),
            )
        };
        let (file, line, column) = staged_site();
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
    /// what the runner reads; in an ordinary executable there is no descriptor
    /// 3, the frame write fails with `EBADF` as designed, and the pinned stderr
    /// message is the whole report. `@assert_eq` therefore lowers the same way
    /// wherever it is written.
    ///
    /// # ABI
    ///
    /// ```text
    /// extern "C" fn __rue_test_fail_comparison(
    ///     kind_ptr: *const u8, kind_len: u64,
    ///     left_ptr: *const u8, left_len: u64,
    ///     right_ptr: *const u8, right_len: u64,
    /// ) -> !
    /// ```
    ///
    /// # Safety
    ///
    /// Each pointer/length pair must describe initialized bytes valid for the
    /// call, or be null with a zero length.
    pub unsafe extern "C" fn __rue_test_fail_comparison(
        kind_ptr: *const u8,
        kind_len: u64,
        left_ptr: *const u8,
        left_len: u64,
        right_ptr: *const u8,
        right_len: u64,
    ) -> ! {
        // SAFETY: the caller guarantees every pair describes readable bytes.
        let (kind, left, right) = unsafe {
            (
                view(kind_ptr, kind_len),
                view(left_ptr, left_len),
                view(right_ptr, right_len),
            )
        };
        let (file, line, column) = staged_site();
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
    /// [`__rue_test_failure_site`] staged, and then writes the pinned stderr
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
    /// what the runner reads; in an ordinary executable there is no descriptor
    /// 3, the frame write fails with `EBADF` as designed, and the pinned stderr
    /// message is the whole report. `@assert` therefore lowers the same way
    /// wherever it is written.
    ///
    /// # ABI
    ///
    /// ```text
    /// extern "C" fn __rue_test_fail_assert(
    ///     message_ptr: *const u8, message_len: u64, with_message: u32,
    /// ) -> !
    /// ```
    ///
    /// # Safety
    ///
    /// `message_ptr`/`message_len` must describe initialized bytes valid for
    /// the call, or be null with a zero length.
    pub unsafe extern "C" fn __rue_test_fail_assert(
        message_ptr: *const u8,
        message_len: u64,
        with_message: u32,
    ) -> ! {
        // SAFETY: the caller guarantees the pair describes readable bytes.
        let message = unsafe { view(message_ptr, message_len) };
        let (file, line, column) = staged_site();
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
            crate::error::__rue_assert_failed()
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
    /// comparison call inside the six-register helper budget.
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
    fn a_staged_site_round_trips_through_the_packed_position() {
        let file = b"app/parser_tests.rue";
        // SAFETY: `file` outlives the read below.
        unsafe { __rue_test_failure_site(file.as_ptr(), file.len() as u64, 7, 3) };
        let position = SITE_POSITION.load(Ordering::Relaxed);
        assert_eq!(((position >> 32) as u32, position as u32), (7, 3));
        assert_eq!(SITE_FILE_LEN.load(Ordering::Relaxed), file.len() as u64);
        // SAFETY: the staged pointer is `file`, still live here.
        let staged = unsafe {
            view(
                SITE_FILE.load(Ordering::Relaxed) as *const u8,
                SITE_FILE_LEN.load(Ordering::Relaxed),
            )
        };
        assert_eq!(staged, file);
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

    /// A trap reached with no staged site — an allocation failure, or a check
    /// the compiler emits below AIR — still reports. The empty location is what
    /// the runner answers from the test declaration's header, so the record is
    /// never worse than the stderr classification it replaces.
    #[test]
    fn an_unstaged_trap_frame_names_no_file() {
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

    #[test]
    fn writing_to_a_closed_descriptor_is_tolerated() {
        // The channel is absent whenever a test image is run by hand, so an
        // `EBADF` write must return rather than abort. A descriptor far above
        // anything the harness opens stands in for that.
        let _ = platform::write_all(1_000_000, b"{}\n");
    }
}
