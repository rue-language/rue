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
//!   borrowed straight from the caller's own bytes, so an arbitrarily long
//!   message needs no heap and no bound on its own length.
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

/// The pinned malformed-selector diagnostic (ADR-0083 §3).
const USAGE_MESSAGE: [u8; 50] = *b"rue-test: expected one 16-hex-digit test selector\n";

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
    /// Rue strings are arbitrary byte sequences, not guaranteed UTF-8, so this
    /// escapes exactly what JSON requires and nothing else: the quote, the
    /// backslash, and every control byte below `0x20` as a `\u00xx` escape.
    /// Bytes at or above `0x80` are emitted raw, which keeps a valid UTF-8
    /// message byte-identical on the wire and leaves an invalid one to the
    /// runner's encoding tag (§2) rather than corrupting it here.
    ///
    /// Unescaped stretches are emitted as one run, so a message that needs no
    /// escaping costs exactly one write.
    fn escaped(&mut self, bytes: &[u8]) {
        const HEX: [u8; 16] = *b"0123456789abcdef";
        let mut run_start = 0;
        for (index, byte) in bytes.iter().enumerate() {
            let mut escape = [0u8; 6];
            let escape_len = match *byte {
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

/// Write the terminal completion frame.
///
/// The dispatcher's epilogue is the only producer: exit 0 with end of stream
/// and no completion frame is how the runner detects a test body that called
/// `std.exit(0)` before its assertions ran (§3, failure kind `incomplete`).
fn complete_frame(emit: &mut dyn FnMut(&[u8])) {
    let mut writer = FrameWriter::new(emit);
    writer.raw(&COMPLETE_FRAME);
}

/// Write every field both failure shapes share, through the open location
/// object: the two differ only in what follows the column.
fn failure_head(
    writer: &mut FrameWriter<'_>,
    kind: &[u8],
    message: &[u8],
    file: &[u8],
    line: u32,
    column: u32,
) {
    writer.raw(&FAILURE_HEAD);
    writer.escaped(kind);
    writer.raw(&MESSAGE_FIELD);
    writer.escaped(message);
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
    failure_head(&mut writer, kind, message, file, line, column);
    writer.raw(&PAYLOAD_FIELD);
    writer.escaped(payload);
    writer.raw(&FRAME_TAIL);
}

/// Write one comparison failure frame (ADR-0083 Phase 2.5).
///
/// It carries `left` and `right` where [`failure_frame`] carries the open
/// `payload`, and no `payload` at all: two rendered operands are not one string
/// a consumer has to split, and the runner computes the diff between them.
/// `left` and `right` are the operands in the order the source wrote them, so
/// `@assert_eq(want, got)` reads the way it is spelled.
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
        comparison_message(kind),
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
    failure_head(&mut writer, &ASSERT_KIND, message, file, line, column);
    writer.raw(&LOCATION_TAIL);
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
        let position = SITE_POSITION.load(Ordering::Relaxed);
        // SAFETY: a staged site's bytes stay readable across the pair, and an
        // unstaged one is the null-with-zero-length form `view` accepts.
        let file = unsafe {
            view(
                SITE_FILE.load(Ordering::Relaxed) as *const u8,
                SITE_FILE_LEN.load(Ordering::Relaxed),
            )
        };
        failure_frame(
            &mut emit_to_channel,
            kind,
            message,
            file,
            (position >> 32) as u32,
            position as u32,
            payload,
        );
        // SAFETY: `message` is a live borrow of the caller's bytes.
        unsafe { crate::error::__rue_panic(message.as_ptr(), message.len() as u64) }
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
        let position = SITE_POSITION.load(Ordering::Relaxed);
        // SAFETY: a staged site's bytes stay readable across the pair, and an
        // unstaged one is the null-with-zero-length form `view` accepts.
        let file = unsafe {
            view(
                SITE_FILE.load(Ordering::Relaxed) as *const u8,
                SITE_FILE_LEN.load(Ordering::Relaxed),
            )
        };
        comparison_frame(
            &mut emit_to_channel,
            kind,
            file,
            (position >> 32) as u32,
            position as u32,
            left,
            right,
        );
        let message = comparison_message(kind);
        // SAFETY: `message` is a `'static` run of this module's own bytes.
        unsafe { crate::error::__rue_panic(message.as_ptr(), message.len() as u64) }
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
        let position = SITE_POSITION.load(Ordering::Relaxed);
        // SAFETY: a staged site's bytes stay readable across the pair, and an
        // unstaged one is the null-with-zero-length form `view` accepts.
        let file = unsafe {
            view(
                SITE_FILE.load(Ordering::Relaxed) as *const u8,
                SITE_FILE_LEN.load(Ordering::Relaxed),
            )
        };
        let framed = if with_message == 0 {
            &ASSERT_MESSAGE[..]
        } else {
            message
        };
        assert_frame(
            &mut emit_to_channel,
            framed,
            file,
            (position >> 32) as u32,
            position as u32,
        );
        if with_message == 0 {
            crate::error::__rue_assert_failed()
        } else {
            // SAFETY: `message` is a live borrow of the caller's bytes.
            unsafe { crate::error::__rue_panic(message.as_ptr(), message.len() as u64) }
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

    #[test]
    fn bytes_at_or_above_0x80_are_written_raw() {
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
        // The message field body appears verbatim, invalid UTF-8 included.
        let needle: &[u8] = &[
            b'"', b'm', b'e', b's', b's', b'a', b'g', b'e', b'"', b':', b'"',
        ];
        let start = bytes
            .windows(needle.len())
            .position(|window| window == needle)
            .unwrap()
            + needle.len();
        assert_eq!(&bytes[start..start + 4], &[0xe2, 0x9c, 0x93, 0xff]);
    }

    #[test]
    fn a_long_message_is_emitted_in_order_across_runs() {
        let long = std::vec![b'x'; 4_096];
        let bytes = frame_bytes(|emit| failure_frame(emit, b"assert", &long, b"a.rue", 1, 1, b""));
        let rendered = std::str::from_utf8(&bytes).unwrap();
        assert!(rendered.starts_with("{\"record\":\"failure\","));
        assert!(rendered.ends_with("\"payload\":\"\"}\n"));
        assert_eq!(rendered.matches('x').count(), long.len());
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

    #[test]
    fn writing_to_a_closed_descriptor_is_tolerated() {
        // The channel is absent whenever a test image is run by hand, so an
        // `EBADF` write must return rather than abort. A descriptor far above
        // anything the harness opens stands in for that.
        let _ = platform::write_all(1_000_000, b"{}\n");
    }
}
