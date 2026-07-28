//! Input/Output functions.
//!
//! This module provides I/O operations for Rue programs:
//! - `__rue_read_line` - Read a line from standard input
//! - `__rue_print` - Write a String's raw bytes to stdout (no newline)
//! - `__rue_println` - Write a String's raw bytes to stdout, then a newline

use core::cell::UnsafeCell;
#[cfg(test)]
use core::sync::atomic::AtomicUsize;
use core::sync::atomic::{AtomicBool, Ordering};

use crate::heap;
use crate::platform;
use crate::string::STRING_MIN_CAPACITY;
pub use rue_runtime_abi::OptionStrBufResult;

// =============================================================================
// String output (RUE-1: print / println)
// =============================================================================
//
// `print(s)` and `println(s)` write the raw bytes of a `String` to stdout via
// the same `write(1, ptr, len)` syscall path `@dbg` uses, but they emit the
// string's actual bytes rather than a debug format. Unlike `@dbg`, `print`
// adds nothing and `println` adds exactly one `\n`.
//
// The String argument is passed by borrow, flattened into three ABI slots
// (ptr, len, cap) exactly like the borrowed operands of `s1 + s2` and the
// arguments to `s.contains(needle)`. `cap` is part of the ABI but unused here:
// output only needs the pointer and length, and the borrow means neither
// function takes ownership, so the caller's String stays valid and is dropped
// by its owner as usual.

crate::define_runtime_implementation! {
    /// Write a String's raw bytes to stdout with no trailing newline.
    ///
    /// Called by the `print(s: String)` builtin free function (RUE-1). Writes
    /// exactly `len` bytes starting at `ptr` to file descriptor 1.
    ///
    /// # ABI
    ///
    /// ```text
    /// extern "C" fn __rue_print(ptr: *const u8, len: u64, cap: u64)
    /// ```
    ///
    /// - `ptr` / `len` / `cap` are the flattened fields of a borrowed String
    ///   (first three argument registers). `cap` is unused.
    ///
    /// # Safety
    ///
    /// When `len > 0`, `ptr` must be non-null and point to `len` valid,
    /// initialized bytes that remain valid for the duration of the call. It
    /// may be null when `len == 0`. The borrow ABI guarantees the live-buffer
    /// requirement: the String is not consumed and outlives the call.
    #[allow(non_snake_case)]
    pub unsafe extern "C" fn __rue_print(ptr: *const u8, len: u64, _cap: u64) {
        // SAFETY: `ptr` points to `len` valid bytes owned by the caller's
        // String (borrowed, not consumed), and we only read from the slice.
        if len > 0 {
            // SAFETY: the caller guarantees a non-null pointer valid for `len`
            // initialized bytes when the length is positive.
            let bytes = unsafe { core::slice::from_raw_parts(ptr, len as usize) };
            platform::write_stdout(bytes);
        }
    }
}

crate::define_runtime_implementation! {
    /// Write a shared two-word `str` view to stdout.
    pub unsafe extern "C" fn __rue_str_print(ptr: *const u8, len: u64) {
        if len > 0 {
            let bytes = unsafe { core::slice::from_raw_parts(ptr, len as usize) };
            platform::write_stdout(bytes);
        }
    }
}

crate::define_runtime_implementation! {
    /// Write a String's raw bytes to stdout followed by a single newline.
    ///
    /// Called by the `println(s: String)` builtin free function (RUE-1). Writes
    /// exactly `len` bytes starting at `ptr` to file descriptor 1, then a lone
    /// `\n`.
    ///
    /// # ABI
    ///
    /// ```text
    /// extern "C" fn __rue_println(ptr: *const u8, len: u64, cap: u64)
    /// ```
    ///
    /// - `ptr` / `len` / `cap` are the flattened fields of a borrowed String
    ///   (first three argument registers). `cap` is unused.
    ///
    /// # Safety
    ///
    /// See [`__rue_print`].
    #[allow(non_snake_case)]
    pub unsafe extern "C" fn __rue_println(ptr: *const u8, len: u64, _cap: u64) {
        // SAFETY: see `__rue_print` — `ptr` is valid for `len` borrowed bytes.
        if len > 0 {
            // SAFETY: see `__rue_print`.
            let bytes = unsafe { core::slice::from_raw_parts(ptr, len as usize) };
            platform::write_stdout(bytes);
        }
        // Byte-array literal (not `b"\n"`) to avoid a macOS linker quirk seen
        // with `b"..."` in `__rue_dbg_str`.
        let newline = [b'\n'];
        platform::write_stdout(&newline);
    }
}

crate::define_runtime_implementation! {
    /// Write a shared two-word `str` view followed by a newline.
    pub unsafe extern "C" fn __rue_str_println(ptr: *const u8, len: u64) {
        if len > 0 {
            let bytes = unsafe { core::slice::from_raw_parts(ptr, len as usize) };
            platform::write_stdout(bytes);
        }
        platform::write_stdout(b"\n");
    }
}

/// Initial buffer size for reading lines.
/// This is a reasonable size for most interactive input.
const READ_LINE_INITIAL_CAPACITY: u64 = 128;

/// Bytes requested from stdin by each buffered read.
///
/// One retained chunk is shared by consecutive `read_line` calls so bytes
/// following a newline do not require another syscall and are not discarded.
const STDIN_CHUNK_SIZE: usize = 4096;

#[derive(Debug)]
struct InputSegment<'a> {
    bytes: &'a [u8],
    ends_line: bool,
}

struct StdinBuffer {
    bytes: [u8; STDIN_CHUNK_SIZE],
    cursor: usize,
    filled: usize,
}

impl StdinBuffer {
    const fn new() -> Self {
        Self {
            bytes: [0; STDIN_CHUNK_SIZE],
            cursor: 0,
            filled: 0,
        }
    }

    /// Return the next in-memory segment, refilling the retained chunk only
    /// when every byte from the preceding read has been consumed.
    fn next_segment<'a>(
        &'a mut self,
        read: &mut impl FnMut(*mut u8, usize) -> i64,
    ) -> Result<Option<InputSegment<'a>>, i64> {
        if self.cursor == self.filled {
            let result = read(self.bytes.as_mut_ptr(), self.bytes.len());
            if result < 0 {
                return Err(result);
            }
            if result == 0 {
                return Ok(None);
            }

            let filled = result as usize;
            if filled > self.bytes.len() {
                // A platform read must never claim more bytes than requested.
                // Treat a violated platform contract like EIO instead of
                // exposing uninitialized memory.
                return Err(-5);
            }
            self.cursor = 0;
            self.filled = filled;
        }

        let start = self.cursor;
        let mut end = start;
        while end < self.filled && self.bytes[end] != b'\n' {
            end += 1;
        }
        let ends_line = end < self.filled;
        self.cursor = end + usize::from(ends_line);

        Ok(Some(InputSegment {
            bytes: &self.bytes[start..end],
            ends_line,
        }))
    }
}

/// Process-global stdin state.
///
/// Rue does not currently expose threads, but keeping the state synchronized
/// makes the runtime ABI sound for native embedders too. The lock spans the
/// blocking read so one logical line cannot interleave with another caller.
struct GlobalStdinBuffer {
    locked: AtomicBool,
    buffer: UnsafeCell<StdinBuffer>,
}

// SAFETY: `with` serializes every access to `buffer`.
unsafe impl Sync for GlobalStdinBuffer {}

impl GlobalStdinBuffer {
    const fn new() -> Self {
        Self {
            locked: AtomicBool::new(false),
            buffer: UnsafeCell::new(StdinBuffer::new()),
        }
    }

    fn with<R>(&self, operation: impl FnOnce(&mut StdinBuffer) -> R) -> R {
        while self
            .locked
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            core::hint::spin_loop();
        }

        struct Unlock<'a>(&'a AtomicBool);
        impl Drop for Unlock<'_> {
            fn drop(&mut self) {
                self.0.store(false, Ordering::Release);
            }
        }

        let _unlock = Unlock(&self.locked);
        // SAFETY: the acquired lock gives this invocation exclusive access to
        // the cell until `_unlock` releases it.
        operation(unsafe { &mut *self.buffer.get() })
    }
}

static STDIN_BUFFER: GlobalStdinBuffer = GlobalStdinBuffer::new();

#[cfg(test)]
static READ_LINE_SYSCALLS: AtomicUsize = AtomicUsize::new(0);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ReadLineFailure {
    Allocation,
    Input,
}

impl ReadLineFailure {
    fn trap(self) -> ! {
        match self {
            Self::Allocation => crate::error::allocation_failure(),
            Self::Input => {
                let mut msg = [0u8; 19];
                msg[0] = b'e';
                msg[1] = b'r';
                msg[2] = b'r';
                msg[3] = b'o';
                msg[4] = b'r';
                msg[5] = b':';
                msg[6] = b' ';
                msg[7] = b'i';
                msg[8] = b'n';
                msg[9] = b'p';
                msg[10] = b'u';
                msg[11] = b't';
                msg[12] = b' ';
                msg[13] = b'e';
                msg[14] = b'r';
                msg[15] = b'r';
                msg[16] = b'o';
                msg[17] = b'r';
                msg[18] = b'\n';
                platform::write_stderr(&msg);
                platform::exit(101);
            }
        }
    }
}

/// Read a line from standard input, returning `Option(StrBuf)`.
///
/// Reads bytes from stdin (file descriptor 0) until a newline character (`\n`)
/// is encountered or EOF is reached. Yields the line (excluding the trailing
/// newline) as `Some(StrBuf)`, or `None` at end-of-input (RUE-6, ADR-0038).
///
/// # Returns
///
/// Writes the whole `Option(StrBuf)` to `out` via sret:
/// - On success (line read, or partial line at EOF): `disc = some_disc` and the
///   StrBuf fields `ptr`/`len`/`cap` are populated.
/// - At EOF with no data read: `disc = none_disc` and the payload slots are
///   zeroed (a `None` never drops its payload, so the zeros are inert).
///
/// `some_disc`/`none_disc` are the caller's `Some`/`None` variant indices,
/// passed in so the runtime need not know the compiler's discriminant
/// assignment.
///
/// # Panics
///
/// - If a read error occurs: panics with "input error"
/// - If memory allocation fails: panics with "out of memory"
///
/// EOF is **not** a panic — it is reported as `None`.
///
/// # ABI (sret convention)
///
/// ```text
/// extern "C" fn __rue_read_line(out: *mut OptionStrBufResult, some_disc: u64, none_disc: u64)
/// ```
///
/// Caller allocates space for the return value and passes pointer.
///
/// # Safety
///
/// `out` must be valid, aligned, writable storage for one
/// [`OptionStrBufResult`] and must remain exclusively accessible for the call.
#[allow(non_snake_case)]
pub unsafe fn __rue_read_line(out: *mut OptionStrBufResult, some_disc: u64, none_disc: u64) {
    read_line_impl(out, some_disc, none_disc);
}

/// Implementation of read_line shared across platforms.
///
/// This function scans retained stdin chunks until:
/// - A newline character is found (returns `Some` line without the newline)
/// - EOF is reached with some data (returns `Some` partial line)
/// - EOF is reached with no data (returns `None`)
/// - A read error occurs (panics)
#[inline]
fn read_line_impl(out: *mut OptionStrBufResult, some_disc: u64, none_disc: u64) {
    let result = STDIN_BUFFER
        .with(|input| read_line_from_fd(input, out, some_disc, none_disc, platform::STDIN));
    if let Err(failure) = result {
        // Trap only after `with` releases the process-global input lock. On
        // Linux, the raw exit syscall terminates only the calling thread, so a
        // native embedder may legitimately invoke the runtime again.
        failure.trap();
    }
}

fn read_line_from_fd(
    input: &mut StdinBuffer,
    out: *mut OptionStrBufResult,
    some_disc: u64,
    none_disc: u64,
    fd: u64,
) -> Result<(), ReadLineFailure> {
    let mut read = |destination, capacity| {
        #[cfg(test)]
        READ_LINE_SYSCALLS.fetch_add(1, Ordering::Relaxed);
        platform::read(fd, destination, capacity)
    };
    read_line_from(input, out, some_disc, none_disc, &mut read)
}

fn read_line_from(
    input: &mut StdinBuffer,
    out: *mut OptionStrBufResult,
    some_disc: u64,
    none_disc: u64,
    read: &mut impl FnMut(*mut u8, usize) -> i64,
) -> Result<(), ReadLineFailure> {
    // Allocate initial buffer
    let mut cap = READ_LINE_INITIAL_CAPACITY;
    let mut ptr = heap::alloc(cap, 1);
    if ptr.is_null() {
        return Err(ReadLineFailure::Allocation);
    }

    let mut len: u64 = 0;

    loop {
        let segment = match input.next_segment(read) {
            Ok(segment) => segment,
            Err(_) => {
                // Read error - free the buffer before reporting the failure.
                // SAFETY: ptr is the live cap-byte buffer allocated above.
                unsafe { heap::free(ptr, cap, 1) };
                return Err(ReadLineFailure::Input);
            }
        };

        let Some(segment) = segment else {
            // EOF reached
            if len == 0 {
                // EOF with no data: this is not an error (RUE-6, ADR-0038).
                // Free the empty buffer and report `None` so a read-until-EOF
                // loop can terminate cleanly instead of trapping.
                // SAFETY: ptr is the live cap-byte buffer allocated above.
                unsafe { heap::free(ptr, cap, 1) };
                // SAFETY: `out` points to caller-allocated space for the
                // Option(StrBuf) result (4 slots). We write `None` and zero
                // the payload; a `None` never has its payload dropped, so the
                // zeroed String slots are inert.
                unsafe {
                    (*out).disc = none_disc;
                    (*out).ptr = core::ptr::null_mut();
                    (*out).len = 0;
                    (*out).cap = 0;
                }
                return Ok(());
            }
            // EOF with data - return partial line
            break;
        };

        let segment_len = match u64::try_from(segment.bytes.len()) {
            Ok(segment_len) => segment_len,
            Err(_) => {
                // SAFETY: ptr is the live cap-byte allocation.
                unsafe { heap::free(ptr, cap, 1) };
                return Err(ReadLineFailure::Allocation);
            }
        };
        let Some(required) = len.checked_add(segment_len) else {
            // SAFETY: ptr is the live cap-byte allocation.
            unsafe { heap::free(ptr, cap, 1) };
            return Err(ReadLineFailure::Allocation);
        };
        if required > cap {
            // Grow the buffer (2x strategy)
            let mut new_cap = cap;
            while new_cap < required {
                let Some(grown) = new_cap.checked_mul(2) else {
                    // SAFETY: ptr is the live cap-byte allocation.
                    unsafe { heap::free(ptr, cap, 1) };
                    return Err(ReadLineFailure::Allocation);
                };
                new_cap = grown.max(STRING_MIN_CAPACITY);
            }
            // SAFETY: ptr is live and readable for cap bytes.
            let new_ptr = unsafe { heap::realloc(ptr, cap, new_cap, 1) };
            if new_ptr.is_null() {
                // Failed realloc leaves the old allocation live.
                // SAFETY: ptr still identifies the live cap-byte allocation.
                unsafe { heap::free(ptr, cap, 1) };
                return Err(ReadLineFailure::Allocation);
            }
            ptr = new_ptr;
            cap = new_cap;
        }

        // SAFETY: `required <= cap`, and `segment` is an initialized,
        // non-overlapping range within the retained stdin chunk.
        unsafe {
            core::ptr::copy_nonoverlapping(
                segment.bytes.as_ptr(),
                ptr.add(len as usize),
                segment.bytes.len(),
            );
        }
        len = required;

        if segment.ends_line {
            break;
        }
    }

    // Return `Some(string)`.
    // SAFETY: `out` is caller-provided, aligned, exclusive result storage.
    unsafe {
        (*out).disc = some_disc;
        (*out).ptr = ptr;
        (*out).len = len;
        (*out).cap = cap;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    extern crate std;

    use self::std::io::Write;
    use self::std::os::fd::AsRawFd;
    use self::std::os::unix::net::UnixStream;
    use self::std::process::Command;
    use self::std::time::Instant;
    use self::std::vec;
    use self::std::vec::Vec;

    struct FakeStdin {
        bytes: Vec<u8>,
        cursor: usize,
        reads: usize,
        max_read: usize,
    }

    impl FakeStdin {
        fn new(bytes: Vec<u8>) -> Self {
            Self {
                bytes,
                cursor: 0,
                reads: 0,
                max_read: usize::MAX,
            }
        }

        fn read(&mut self, destination: *mut u8, capacity: usize) -> i64 {
            self.reads += 1;
            let available = self.bytes.len() - self.cursor;
            let count = available.min(capacity).min(self.max_read);
            if count == 0 {
                return 0;
            }
            // SAFETY: the buffer contract supplies `capacity` writable bytes,
            // and `count` is bounded by both source and destination.
            unsafe {
                core::ptr::copy_nonoverlapping(
                    self.bytes.as_ptr().add(self.cursor),
                    destination,
                    count,
                );
            }
            self.cursor += count;
            count as i64
        }
    }

    fn read_one(input: &mut super::StdinBuffer, source: &mut FakeStdin) -> Option<Vec<u8>> {
        let mut out = super::OptionStrBufResult {
            disc: u64::MAX,
            ptr: core::ptr::null_mut(),
            len: 0,
            cap: 0,
        };
        super::read_line_from(input, &mut out, 1, 0, &mut |destination, capacity| {
            source.read(destination, capacity)
        })
        .expect("fake stdin read succeeds");

        if out.disc == 0 {
            assert!(out.ptr.is_null());
            assert_eq!(out.len, 0);
            assert_eq!(out.cap, 0);
            return None;
        }
        assert_eq!(out.disc, 1);
        // SAFETY: `Some` transfers the live `cap`-byte allocation to this
        // test, with `len` initialized bytes.
        let bytes = unsafe { core::slice::from_raw_parts(out.ptr, out.len as usize) }.to_vec();
        // SAFETY: the test now owns the exact allocation returned above.
        unsafe { crate::heap::free(out.ptr, out.cap, 1) };
        Some(bytes)
    }

    #[test]
    fn buffered_lines_preserve_bytes_after_each_newline() {
        let mut input = super::StdinBuffer::new();
        let mut source = FakeStdin::new(b"first\n\nthird\xff\npartial".to_vec());

        assert_eq!(read_one(&mut input, &mut source), Some(b"first".to_vec()));
        assert_eq!(source.reads, 1);
        assert_eq!(read_one(&mut input, &mut source), Some(Vec::new()));
        assert_eq!(source.reads, 1);
        assert_eq!(
            read_one(&mut input, &mut source),
            Some(b"third\xff".to_vec())
        );
        assert_eq!(source.reads, 1);
        assert_eq!(read_one(&mut input, &mut source), Some(b"partial".to_vec()));
        assert_eq!(source.reads, 2);
        assert_eq!(read_one(&mut input, &mut source), None);
        assert_eq!(source.reads, 3);
    }

    #[test]
    fn newlines_at_both_sides_of_the_chunk_boundary_are_preserved() {
        let mut bytes = vec![b'a'; super::STDIN_CHUNK_SIZE - 1];
        bytes.extend_from_slice(b"\nnext\n");
        bytes.extend(vec![b'b'; super::STDIN_CHUNK_SIZE]);
        bytes.extend_from_slice(b"\nafter\n");

        let mut input = super::StdinBuffer::new();
        let mut source = FakeStdin::new(bytes);

        assert_eq!(
            read_one(&mut input, &mut source).unwrap(),
            vec![b'a'; super::STDIN_CHUNK_SIZE - 1]
        );
        assert_eq!(read_one(&mut input, &mut source), Some(b"next".to_vec()));
        assert_eq!(
            read_one(&mut input, &mut source).unwrap(),
            vec![b'b'; super::STDIN_CHUNK_SIZE]
        );
        assert_eq!(read_one(&mut input, &mut source), Some(b"after".to_vec()));
        assert_eq!(read_one(&mut input, &mut source), None);

        let mut bytes = vec![b'c'; super::STDIN_CHUNK_SIZE];
        bytes.extend_from_slice(b"\nlast\n");
        let mut input = super::StdinBuffer::new();
        let mut source = FakeStdin::new(bytes);

        assert_eq!(
            read_one(&mut input, &mut source).unwrap(),
            vec![b'c'; super::STDIN_CHUNK_SIZE]
        );
        assert_eq!(source.reads, 2, "newline begins the second chunk");
        assert_eq!(read_one(&mut input, &mut source), Some(b"last".to_vec()));
        assert_eq!(read_one(&mut input, &mut source), None);
    }

    #[test]
    fn partial_reads_are_assembled_without_extra_syscalls_per_byte() {
        let mut input = super::StdinBuffer::new();
        let mut source = FakeStdin::new(b"abcdefghij\n".to_vec());
        source.max_read = 3;

        assert_eq!(
            read_one(&mut input, &mut source),
            Some(b"abcdefghij".to_vec())
        );
        assert_eq!(source.reads, 4);
    }

    #[test]
    fn interrupted_read_is_still_an_input_error() {
        let mut input = super::StdinBuffer::new();
        let error = input
            .next_segment(&mut |_destination, _capacity| -4)
            .unwrap_err();
        assert_eq!(error, -4);
    }

    #[test]
    fn read_line_allocation_failure_uses_canonical_trap() {
        const CHILD_ENV: &str = "RUE_READ_LINE_OOM_CHILD";
        if self::std::env::var_os(CHILD_ENV).is_some() {
            crate::heap::fail_allocations_after_for_test(0);
            let mut out = super::OptionStrBufResult {
                disc: 0,
                ptr: core::ptr::null_mut(),
                len: 0,
                cap: 0,
            };
            super::read_line_impl(&mut out, 1, 0);
            unreachable!();
        }

        let output = Command::new(self::std::env::current_exe().expect("current test binary"))
            .args([
                "--exact",
                "io::tests::read_line_allocation_failure_uses_canonical_trap",
                "--nocapture",
            ])
            .env(CHILD_ENV, "1")
            .output()
            .expect("spawn allocation-failure child");

        assert_eq!(output.status.code(), Some(101));
        assert_eq!(output.stderr, b"panic: out of memory\n");
    }

    #[test]
    fn read_line_reallocation_failure_uses_canonical_trap() {
        const CHILD_ENV: &str = "RUE_READ_LINE_REALLOC_OOM_CHILD";
        if self::std::env::var_os(CHILD_ENV).is_some() {
            crate::heap::fail_allocations_after_for_test(1);
            let mut input = super::StdinBuffer::new();
            let mut source = FakeStdin::new(vec![b'x'; 256]);
            let mut out = super::OptionStrBufResult {
                disc: 0,
                ptr: core::ptr::null_mut(),
                len: 0,
                cap: 0,
            };
            let failure =
                super::read_line_from(&mut input, &mut out, 1, 0, &mut |destination, capacity| {
                    source.read(destination, capacity)
                })
                .unwrap_err();
            failure.trap();
        }

        let output = Command::new(self::std::env::current_exe().expect("current test binary"))
            .args([
                "--exact",
                "io::tests::read_line_reallocation_failure_uses_canonical_trap",
                "--nocapture",
            ])
            .env(CHILD_ENV, "1")
            .output()
            .expect("spawn reallocation-failure child");

        assert_eq!(output.status.code(), Some(101));
        assert_eq!(output.stderr, b"panic: out of memory\n");
    }

    #[test]
    fn read_line_error_uses_canonical_trap() {
        const CHILD_ENV: &str = "RUE_READ_LINE_ERROR_CHILD";
        if self::std::env::var_os(CHILD_ENV).is_some() {
            let mut input = super::StdinBuffer::new();
            let mut out = super::OptionStrBufResult {
                disc: 0,
                ptr: core::ptr::null_mut(),
                len: 0,
                cap: 0,
            };
            let failure =
                super::read_line_from(&mut input, &mut out, 1, 0, &mut |_, _| -4).unwrap_err();
            failure.trap();
        }

        let output = Command::new(self::std::env::current_exe().expect("current test binary"))
            .args([
                "--exact",
                "io::tests::read_line_error_uses_canonical_trap",
                "--nocapture",
            ])
            .env(CHILD_ENV, "1")
            .output()
            .expect("spawn read-error child");

        assert_eq!(output.status.code(), Some(101));
        assert_eq!(output.stderr, b"error: input error\n");
    }

    #[test]
    fn read_failure_releases_the_global_input_lock() {
        let input = super::GlobalStdinBuffer::new();
        let mut out = super::OptionStrBufResult {
            disc: 0,
            ptr: core::ptr::null_mut(),
            len: 0,
            cap: 0,
        };

        let failure = input
            .with(|buffer| super::read_line_from(buffer, &mut out, 1, 0, &mut |_, _| -4))
            .unwrap_err();
        assert_eq!(failure, super::ReadLineFailure::Input);

        // A Linux runtime trap exits only the invoking thread. Returning the
        // failure through `with` must therefore release this lock first.
        assert_eq!(input.with(|_| 42), 42);
    }

    /// Focused manual benchmark for RUE-1172. A Unix stream feeds the same
    /// platform-read helper used for production stdin, so the metric counts
    /// actual kernel reads and times the runtime allocation/copy path.
    #[test]
    #[ignore = "manual benchmark; run this exact test with --ignored --nocapture"]
    fn buffered_read_line_benchmark() {
        const LINE_BYTES: usize = 16 * 1024 * 1024;

        let (reader, mut writer) = UnixStream::pair().expect("create benchmark stream");
        let writer_thread = self::std::thread::spawn(move || {
            let mut bytes = vec![b'x'; LINE_BYTES];
            bytes.push(b'\n');
            writer.write_all(&bytes).expect("feed benchmark stream");
        });

        let mut input = super::StdinBuffer::new();
        let mut out = super::OptionStrBufResult {
            disc: 0,
            ptr: core::ptr::null_mut(),
            len: 0,
            cap: 0,
        };
        let fd = reader.as_raw_fd() as u64;
        super::READ_LINE_SYSCALLS.store(0, super::Ordering::Relaxed);

        let started = Instant::now();
        super::read_line_from_fd(&mut input, &mut out, 1, 0, fd).expect("benchmark line read");
        assert_eq!(out.disc, 1);
        assert_eq!(out.len, LINE_BYTES as u64);
        // SAFETY: `Some` transferred this exact live allocation.
        unsafe { crate::heap::free(out.ptr, out.cap, 1) };

        super::read_line_from_fd(&mut input, &mut out, 1, 0, fd).expect("benchmark EOF read");
        let elapsed = started.elapsed();
        assert_eq!(out.disc, 0);
        writer_thread.join().expect("benchmark writer thread");

        let read_syscalls = super::READ_LINE_SYSCALLS.load(super::Ordering::Relaxed);
        let total_bytes = LINE_BYTES + 1;
        let throughput_mib_s = total_bytes as f64 / (1024.0 * 1024.0) / elapsed.as_secs_f64();
        assert!(read_syscalls > 1);
        assert!(read_syscalls < total_bytes / 128);
        self::std::eprintln!(
            "read_line: bytes={total_bytes} lines=1 \
             read_syscalls={read_syscalls} elapsed_ms={:.3} \
             throughput_mib_s={throughput_mib_s:.1}",
            elapsed.as_secs_f64() * 1000.0,
        );
    }
}
