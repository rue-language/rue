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
const FRAME_TAIL: [u8; 3] = *b"\"}\n";

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
    writer.raw(&PAYLOAD_FIELD);
    writer.escaped(payload);
    writer.raw(&FRAME_TAIL);
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
