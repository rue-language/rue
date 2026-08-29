//! x86-64 Linux syscall implementations.
//!
//! This module provides direct syscall wrappers for Linux on x86-64.
//! No libc is used - we invoke the kernel directly via the `syscall` instruction.
//!
//! # Platform Requirements
//!
//! This module only compiles on x86-64 Linux. Attempting to compile on other
//! platforms will result in a compile error.
//!
//! # Syscall Conventions
//!
//! On x86-64 Linux:
//! - Syscall number goes in `rax`
//! - Arguments go in `rdi`, `rsi`, `rdx`, `r10`, `r8`, `r9` (in order)
//! - Return value comes back in `rax`
//! - `rcx` and `r11` are clobbered by the syscall instruction
//!
//! # Error Handling
//!
//! Linux syscalls return negative values on error (specifically, `-errno`).
//! This module preserves those error codes in return values where applicable.

// Compile-time check for platform requirements
#[cfg(not(all(target_arch = "x86_64", target_os = "linux")))]
compile_error!("rue-runtime only supports x86-64 Linux");

use core::arch::asm;
use rue_runtime_abi::RuntimeTarget;

/// Linux syscall number for read (see `man 2 read`).
const SYS_READ: i64 = 0;

/// Linux syscall number for write (see `man 2 write`).
const SYS_WRITE: i64 = RuntimeTarget::X86_64Linux.write_syscall_number() as i64;

/// Linux syscall number for mmap (see `man 2 mmap`).
const SYS_MMAP: i64 = 9;

/// Linux syscall number for munmap (see `man 2 munmap`).
const SYS_MUNMAP: i64 = 11;

/// Linux syscall number for exit (see `man 2 exit`).
const SYS_EXIT: i64 = 60;

/// Standard input file descriptor.
pub const STDIN: u64 = 0;

/// Standard output file descriptor.
pub const STDOUT: u64 = 1;

/// Standard error file descriptor.
pub const STDERR: u64 = 2;

/// Write bytes to a file descriptor.
///
/// This is a thin wrapper around the Linux `write(2)` syscall.
///
/// # Arguments
///
/// * `fd` - File descriptor to write to
/// * `buf` - Pointer to the buffer containing data to write
/// * `len` - Number of bytes to write
///
/// # Returns
///
/// On success, returns the number of bytes written (which may be less than `len`
/// if the write was interrupted or the pipe/socket buffer is full).
///
/// On error, returns a negative value representing `-errno`.
///
/// # Safety
///
/// The caller must ensure:
/// - `buf` points to a valid memory region of at least `len` bytes
/// - The memory region remains valid for the duration of the syscall
/// - `buf` is properly aligned (though byte alignment is always satisfied)
///
/// # Note on Partial Writes
///
/// This function may write fewer bytes than requested. Callers should check
/// the return value and use [`write_all`] if complete writes are required.
pub fn write(fd: u64, buf: *const u8, len: usize) -> i64 {
    let result: i64;
    // SAFETY: Making the write(2) syscall is safe because:
    // - The syscall interface is stable and well-defined
    // - We pass arguments in the correct registers per x86-64 Linux ABI
    // - The kernel validates fd, buf, and len; invalid values return errors
    // - We correctly mark rcx and r11 as clobbered (per syscall ABI)
    // - The caller is responsible for ensuring buf points to valid memory
    unsafe {
        asm!(
            "syscall",
            in("rax") SYS_WRITE,
            in("rdi") fd,
            in("rsi") buf,
            in("rdx") len,
            lateout("rax") result,
            // syscall clobbers rcx and r11 per x86-64 ABI
            out("rcx") _,
            out("r11") _,
        );
    }
    result
}

/// Read bytes from a file descriptor.
///
/// This is a thin wrapper around the Linux `read(2)` syscall.
///
/// # Arguments
///
/// * `fd` - File descriptor to read from
/// * `buf` - Pointer to the buffer to read data into
/// * `len` - Maximum number of bytes to read
///
/// # Returns
///
/// On success, returns the number of bytes read (0 indicates end-of-file).
///
/// On error, returns a negative value representing `-errno`.
///
/// # Safety
///
/// The caller must ensure:
/// - `buf` points to a valid, writable memory region of at least `len` bytes
/// - The memory region remains valid for the duration of the syscall
pub fn read(fd: u64, buf: *mut u8, len: usize) -> i64 {
    let result: i64;
    // SAFETY: Making the read(2) syscall is safe because:
    // - The syscall interface is stable and well-defined
    // - We pass arguments in the correct registers per x86-64 Linux ABI
    // - The kernel validates fd, buf, and len; invalid values return errors
    // - We correctly mark rcx and r11 as clobbered (per syscall ABI)
    // - The caller is responsible for ensuring buf points to writable memory
    unsafe {
        asm!(
            "syscall",
            in("rax") SYS_READ,
            in("rdi") fd,
            in("rsi") buf,
            in("rdx") len,
            lateout("rax") result,
            // syscall clobbers rcx and r11 per x86-64 ABI
            out("rcx") _,
            out("r11") _,
        );
    }
    result
}

/// Write all bytes to a file descriptor, handling partial writes.
///
/// This function loops until all bytes are written or an unrecoverable error occurs.
/// It handles partial writes by advancing the buffer pointer and retrying.
///
/// # Arguments
///
/// * `fd` - File descriptor to write to
/// * `buf` - Slice of bytes to write
///
/// # Returns
///
/// * `Ok(())` - All bytes were successfully written
/// * `Err(errno)` - A syscall error occurred (errno is positive)
///
/// # Behavior on Error
///
/// If a write fails (returns negative), this function returns immediately with
/// the error code. No retry is attempted on error.
///
/// # Safety
///
/// This function is safe because it only accepts a slice, which guarantees
/// valid memory.
pub fn write_all(fd: u64, mut buf: &[u8]) -> Result<(), i64> {
    while !buf.is_empty() {
        let result = write(fd, buf.as_ptr(), buf.len());
        if result < 0 {
            // Syscall error - return the errno (as positive)
            return Err(-result);
        }
        if result == 0 {
            // This shouldn't happen for stderr, but handle it to avoid infinite loop.
            // A zero return typically means the fd is in a state where no more data
            // can be written (e.g., closed pipe). We treat this as an error.
            return Err(5); // EIO - I/O error
        }
        // Advance past the bytes we successfully wrote
        buf = &buf[result as usize..];
    }
    Ok(())
}

/// Write a message to stderr.
///
/// This is a best-effort write operation: any error returned by [`write_all`]
/// is dropped because there is no meaningful recovery action and the runtime is
/// typically about to exit anyway.
///
/// # Broken pipes vs. other write errors (RUE-369)
///
/// The dropped-`Err` path is **not** what handles a broken output pipe. We do
/// not install a `SIGPIPE` handler, so the signal keeps its default
/// disposition: when the reader on the other end of a pipe or socket is gone,
/// the `write` syscall raises `SIGPIPE` and the kernel terminates the process
/// *before* the syscall returns — the process dies with exit code 141 (128 +
/// SIGPIPE) and `write_all` never observes the `EPIPE` error. This is Rue's
/// intended Unix default (spec §8.5).
///
/// Swallowing the `Err` still matters for failures that return an errno
/// *without* raising a signal — e.g. a closed or otherwise-invalid file
/// descriptor yields `EBADF`, and a stuck fd yields the `EIO` we synthesize
/// below. In those cases no signal fires, the process keeps running, and we
/// intentionally discard the error.
///
/// This function handles partial writes by looping until all bytes are written
/// or an error occurs.
///
/// # Arguments
///
/// * `msg` - The bytes to write to stderr
pub fn write_stderr(msg: &[u8]) {
    // Best-effort: drop non-fatal errno failures (e.g. EBADF on a closed fd).
    // A broken pipe never reaches here — SIGPIPE kills the process first.
    let _ = write_all(STDERR, msg);
}

/// Write a message to stdout.
///
/// Best-effort, exactly like [`write_stderr`]: a broken output pipe kills the
/// process with `SIGPIPE` (exit 141) before `write_all` returns, while a
/// non-signalling errno such as `EBADF` from a closed fd is swallowed here.
///
/// # Arguments
///
/// * `msg` - The bytes to write to stdout
pub fn write_stdout(msg: &[u8]) {
    let _ = write_all(STDOUT, msg);
}

/// Convert a signed 64-bit integer to a decimal string and write it to stdout.
///
/// Handles negative numbers by printing a leading '-'.
pub fn print_i64(value: i64) {
    // Buffer for decimal digits (max 20 digits for i64 + sign + newline)
    let mut buf = [0u8; 22];
    let mut pos = buf.len() - 1;

    // Always end with newline
    buf[pos] = b'\n';
    pos -= 1;

    let is_negative = value < 0;
    // Handle the absolute value (special case for i64::MIN)
    let mut abs_value = if value == i64::MIN {
        // i64::MIN cannot be negated, handle specially
        // We'll print the digits manually
        9223372036854775808u64
    } else if is_negative {
        (-value) as u64
    } else {
        value as u64
    };

    // Generate digits in reverse order
    if abs_value == 0 {
        buf[pos] = b'0';
        pos -= 1;
    } else {
        while abs_value > 0 {
            buf[pos] = b'0' + (abs_value % 10) as u8;
            abs_value /= 10;
            pos -= 1;
        }
    }

    // Add sign if negative
    if is_negative {
        buf[pos] = b'-';
        pos -= 1;
    }

    // Write from pos+1 to end
    write_stdout(&buf[pos + 1..]);
}

/// Convert an unsigned 64-bit integer to a decimal string and write it to stdout.
pub fn print_u64(value: u64) {
    // Buffer for decimal digits (max 20 digits for u64 + newline)
    let mut buf = [0u8; 22];
    let mut pos = buf.len() - 1;

    // Always end with newline
    buf[pos] = b'\n';
    pos -= 1;

    let mut val = value;

    // Generate digits in reverse order
    if val == 0 {
        buf[pos] = b'0';
        pos -= 1;
    } else {
        while val > 0 {
            buf[pos] = b'0' + (val % 10) as u8;
            val /= 10;
            pos -= 1;
        }
    }

    // Write from pos+1 to end
    write_stdout(&buf[pos + 1..]);
}

/// Print a boolean value to stdout ("true\n" or "false\n").
pub fn print_bool(value: bool) {
    if value {
        write_stdout(b"true\n");
    } else {
        write_stdout(b"false\n");
    }
}

/// Map anonymous memory pages.
///
/// This is a wrapper around the Linux `mmap(2)` syscall configured for
/// anonymous private memory allocation (no file backing).
///
/// # Arguments
///
/// * `size` - Number of bytes to allocate. Will be rounded up to page size by the kernel.
///
/// # Returns
///
/// On success, returns a pointer to the mapped memory region.
/// On error, returns a null pointer.
///
/// # Memory Protection
///
/// The mapped region is readable and writable (PROT_READ | PROT_WRITE).
///
/// # Safety
///
/// The returned pointer (if non-null) points to valid, zero-initialized memory.
/// The caller is responsible for calling `munmap` when done.
pub fn mmap(size: usize) -> *mut u8 {
    // mmap flags
    const PROT_READ: i64 = 0x1;
    const PROT_WRITE: i64 = 0x2;
    const MAP_PRIVATE: i64 = 0x02;
    const MAP_ANONYMOUS: i64 = 0x20;

    let result: i64;
    // SAFETY: Making the mmap(2) syscall with anonymous mapping is safe because:
    // - MAP_ANONYMOUS + MAP_PRIVATE creates a private zero-initialized memory region
    // - We request PROT_READ | PROT_WRITE which is safe for heap memory
    // - addr=NULL lets the kernel choose a safe address
    // - fd=-1 is correct for anonymous mappings (no file backing)
    // - The kernel validates all parameters and returns an error on failure
    // - We correctly mark rcx and r11 as clobbered (per syscall ABI)
    unsafe {
        asm!(
            "syscall",
            in("rax") SYS_MMAP,
            in("rdi") 0i64,           // addr: NULL (let kernel choose)
            in("rsi") size,           // length
            in("rdx") PROT_READ | PROT_WRITE,  // prot
            in("r10") MAP_PRIVATE | MAP_ANONYMOUS,  // flags
            in("r8") -1i64,           // fd: -1 for anonymous
            in("r9") 0i64,            // offset: 0
            lateout("rax") result,
            out("rcx") _,
            out("r11") _,
        );
    }

    // mmap returns MAP_FAILED (-1 as usize) on error
    if result < 0 {
        core::ptr::null_mut()
    } else {
        result as *mut u8
    }
}

/// Unmap memory pages previously mapped with `mmap`.
///
/// This is a wrapper around the Linux `munmap(2)` syscall.
///
/// # Arguments
///
/// * `addr` - Pointer to the start of the mapped region (must be page-aligned)
/// * `size` - Size of the region to unmap (will be rounded up to page size)
///
/// # Returns
///
/// Returns 0 on success, or a negative errno on failure.
///
/// # Safety
///
/// The caller must ensure:
/// - `addr` was returned by a previous `mmap` call
/// - `size` matches the size used in the `mmap` call
/// - The memory is not accessed after this call
pub fn munmap(addr: *mut u8, size: usize) -> i64 {
    let result: i64;
    // SAFETY: Making the munmap(2) syscall is safe because:
    // - The kernel validates addr and size; invalid values return errors
    // - The caller guarantees addr was returned by a previous mmap call
    // - The caller guarantees size matches the mmap call
    // - The caller guarantees the memory won't be accessed after this call
    // - We correctly mark rcx and r11 as clobbered (per syscall ABI)
    unsafe {
        asm!(
            "syscall",
            in("rax") SYS_MUNMAP,
            in("rdi") addr,
            in("rsi") size,
            lateout("rax") result,
            out("rcx") _,
            out("r11") _,
        );
    }
    result
}

// ============================================================================
// Stack-overflow trapping (RUE-645)
// ============================================================================
//
// A stack overflow faults on the guard page and raises SIGSEGV. With no handler
// the kernel kills the process (exit 139). To abort cleanly we register a small
// alternate signal stack and a SIGSEGV handler that runs on it (SA_ONSTACK),
// prints "stack overflow", and exits 101. See `entry::__rue_stack_overflow_handler`
// for why any SIGSEGV from safe Rue is treated as a stack overflow.

/// Linux x86-64 syscall number for rt_sigaction (see `man 2 rt_sigaction`).
const SYS_RT_SIGACTION: i64 = 13;

/// Linux x86-64 syscall number for sigaltstack (see `man 2 sigaltstack`).
const SYS_SIGALTSTACK: i64 = 131;

/// `SIGSEGV` signal number on Linux.
const SIGSEGV: i32 = 11;

/// `SA_ONSTACK`: run the handler on the alternate signal stack.
const SA_ONSTACK: u64 = 0x0800_0000;

/// `SA_RESTORER`: the `sa_restorer` field is valid and should be used as the
/// signal-return trampoline.
const SA_RESTORER: u64 = 0x0400_0000;

/// Kernel `struct sigaction` layout on x86-64 Linux (the ABI `rt_sigaction`
/// expects). Field order differs from the userspace/POSIX struct: on x86-64 the
/// mask comes last. `sa_restorer` must point at a valid signal-return
/// trampoline (see `__rue_rt_sigreturn`) or the kernel refuses to deliver.
#[repr(C)]
struct KernelSigaction {
    sa_handler: usize,
    sa_flags: u64,
    sa_restorer: usize,
    sa_mask: u64,
}

/// `stack_t` layout for `sigaltstack` on x86-64 Linux.
#[repr(C)]
struct StackT {
    ss_sp: *mut u8,
    ss_flags: i32,
    ss_size: usize,
}

/// Register the alternate signal stack with `sigaltstack(2)`.
///
/// Returns 0 on success or a negative errno on failure.
///
/// # Safety
///
/// `ss` must point to a valid `StackT` whose `ss_sp`/`ss_size` describe a live,
/// writable memory region for the lifetime of the alt stack.
unsafe fn sigaltstack(ss: *const StackT) -> i64 {
    let result: i64;
    // SAFETY: sigaltstack takes a pointer to a stack_t and an optional old
    // stack_t (NULL here); the kernel validates the pointer. rcx/r11 are
    // clobbered per the syscall ABI.
    unsafe {
        asm!(
            "syscall",
            in("rax") SYS_SIGALTSTACK,
            in("rdi") ss,
            in("rsi") 0i64, // oldss = NULL
            lateout("rax") result,
            out("rcx") _,
            out("r11") _,
        );
    }
    result
}

/// Install a signal handler with `rt_sigaction(2)`.
///
/// Returns 0 on success or a negative errno on failure.
///
/// # Safety
///
/// `act` must point to a valid `KernelSigaction`; `sig` must be a valid signal.
unsafe fn rt_sigaction(sig: i32, act: *const KernelSigaction) -> i64 {
    let result: i64;
    // SAFETY: rt_sigaction(sig, act, oldact, sigsetsize). oldact is NULL and
    // sigsetsize is 8 (bytes in the kernel sigset_t on x86-64). rcx/r11 are
    // clobbered per the syscall ABI.
    unsafe {
        asm!(
            "syscall",
            in("rax") SYS_RT_SIGACTION,
            in("rdi") sig as i64,
            in("rsi") act,
            in("rdx") 0i64, // oldact = NULL
            in("r10") 8i64, // sigsetsize
            lateout("rax") result,
            out("rcx") _,
            out("r11") _,
        );
    }
    result
}

/// Install the stack-overflow SIGSEGV handler (see module comment above).
///
/// Best-effort: if the alt stack cannot be mapped or a syscall fails, we return
/// without installing anything and the process keeps the default SIGSEGV
/// disposition.
///
/// `handler` never returns, so no `sa_restorer` is supplied.
pub fn install_stack_overflow_handler(handler: extern "C" fn(i32) -> !) {
    /// Size of the alternate signal stack (16 KiB — ample for the tiny handler).
    const ALT_STACK_SIZE: usize = 16 * 1024;

    let stack = mmap(ALT_STACK_SIZE);
    if stack.is_null() {
        return;
    }

    let ss = StackT {
        ss_sp: stack,
        ss_flags: 0,
        ss_size: ALT_STACK_SIZE,
    };
    // SAFETY: `ss` describes the region just mmap'd, which stays mapped for the
    // remaining life of the process.
    if unsafe { sigaltstack(&ss) } < 0 {
        return;
    }

    let act = KernelSigaction {
        sa_handler: handler as usize,
        sa_flags: SA_ONSTACK | SA_RESTORER,
        sa_restorer: crate::__rue_rt_sigreturn as usize,
        sa_mask: 0,
    };
    // SAFETY: `act` is a valid KernelSigaction; SIGSEGV is a valid signal.
    let _ = unsafe { rt_sigaction(SIGSEGV, &act) };
}

/// Exit the process with the given status code.
///
/// This performs a direct syscall to `exit(2)` and never returns.
/// The process terminates immediately without running any cleanup code
/// or destructors.
///
/// # Arguments
///
/// * `status` - The exit code to return to the parent process.
///   By convention: 0 for success, non-zero for failure.
///   Rue uses: 0 for success, 1 for panic, 101 for runtime errors.
///
/// # Exit Codes
///
/// This function never returns - it terminates the process.
pub fn exit(status: i32) -> ! {
    // SAFETY: The exit syscall is always safe to call and never returns.
    unsafe {
        asm!(
            "syscall",
            in("rax") SYS_EXIT,
            in("rdi") status as i64,
            options(noreturn)
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Note: We can't easily test exit() or the functions that call it
    // because they terminate the process. Those are tested via integration
    // tests that spawn child processes.

    #[test]
    fn test_write_to_stderr() {
        // Test that write() returns the correct byte count
        let msg = b"test message\n";
        let result = write(STDERR, msg.as_ptr(), msg.len());
        assert_eq!(result, msg.len() as i64);
    }

    #[test]
    fn test_write_empty() {
        // Writing zero bytes should succeed and return 0
        let result = write(STDERR, core::ptr::null(), 0);
        assert_eq!(result, 0);
    }

    #[test]
    fn test_write_invalid_fd() {
        // Writing to an invalid fd should return an error
        let msg = b"test";
        let result = write(999, msg.as_ptr(), msg.len());
        // Should return -EBADF (9) for bad file descriptor
        assert!(result < 0);
        assert_eq!(-result, 9); // EBADF
    }

    #[test]
    fn test_write_all_success() {
        let msg = b"write_all test\n";
        let result = write_all(STDERR, msg);
        assert!(result.is_ok());
    }

    #[test]
    fn test_write_all_empty() {
        // Writing empty slice should succeed immediately
        let result = write_all(STDERR, b"");
        assert!(result.is_ok());
    }

    #[test]
    fn test_write_all_invalid_fd() {
        let msg = b"test";
        let result = write_all(999, msg);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), 9); // EBADF
    }

    #[test]
    fn test_write_stderr_helper() {
        // This should not panic, even with various inputs
        write_stderr(b"stderr test 1\n");
        write_stderr(b"");
        write_stderr(b"stderr test 2\n");
    }

    #[test]
    fn test_write_large_message() {
        // Test writing a larger message to exercise any partial write handling
        // Use a stack-allocated array since we're no_std
        let msg = [b'x'; 4096];
        let result = write(STDERR, msg.as_ptr(), msg.len());
        // Should write all bytes (stderr is typically unbuffered)
        assert_eq!(result, 4096);
    }

    #[test]
    fn test_syscall_constants() {
        // Verify our syscall numbers match Linux x86-64 ABI
        assert_eq!(SYS_READ, 0);
        assert_eq!(SYS_WRITE, 1);
        assert_eq!(SYS_MMAP, 9);
        assert_eq!(SYS_MUNMAP, 11);
        assert_eq!(SYS_EXIT, 60);
        assert_eq!(STDIN, 0);
        assert_eq!(STDOUT, 1);
        assert_eq!(STDERR, 2);
    }

    #[test]
    fn test_read_invalid_fd() {
        // Reading from an invalid fd should return an error
        let mut buf = [0u8; 16];
        let result = read(999, buf.as_mut_ptr(), buf.len());
        // Should return -EBADF (9) for bad file descriptor
        assert!(result < 0);
        assert_eq!(-result, 9); // EBADF
    }

    #[test]
    fn test_read_zero_bytes() {
        // Reading zero bytes should succeed and return 0
        let mut buf = [0u8; 16];
        // Use stdin (fd 0) - reading 0 bytes should always succeed
        let result = read(STDIN, buf.as_mut_ptr(), 0);
        assert_eq!(result, 0);
    }

    #[test]
    fn test_mmap_basic() {
        // Allocate a page of memory
        let size = 4096;
        let ptr = mmap(size);
        assert!(!ptr.is_null());

        // Memory should be zero-initialized and writable
        unsafe {
            assert_eq!(*ptr, 0);
            *ptr = 42;
            assert_eq!(*ptr, 42);
        }

        // Clean up
        let result = munmap(ptr, size);
        assert_eq!(result, 0);
    }

    #[test]
    fn test_mmap_large() {
        // Allocate 1 MB
        let size = 1024 * 1024;
        let ptr = mmap(size);
        assert!(!ptr.is_null());

        // Write to first and last bytes
        unsafe {
            *ptr = 1;
            *ptr.add(size - 1) = 2;
            assert_eq!(*ptr, 1);
            assert_eq!(*ptr.add(size - 1), 2);
        }

        let result = munmap(ptr, size);
        assert_eq!(result, 0);
    }

    #[test]
    fn test_mmap_multiple() {
        // Allocate multiple regions
        let size = 4096;
        let ptr1 = mmap(size);
        let ptr2 = mmap(size);
        let ptr3 = mmap(size);

        assert!(!ptr1.is_null());
        assert!(!ptr2.is_null());
        assert!(!ptr3.is_null());

        // They should be different addresses
        assert_ne!(ptr1, ptr2);
        assert_ne!(ptr2, ptr3);
        assert_ne!(ptr1, ptr3);

        // Clean up all
        assert_eq!(munmap(ptr1, size), 0);
        assert_eq!(munmap(ptr2, size), 0);
        assert_eq!(munmap(ptr3, size), 0);
    }

    #[test]
    fn test_mmap_zero_size() {
        // Zero-size mmap should fail (returns EINVAL on Linux)
        let ptr = mmap(0);
        assert!(ptr.is_null());
    }

    #[test]
    fn stack_overflow_handler_is_installable() {
        // Reference the installer (and, transitively, its `sigaltstack` /
        // `rt_sigaction` wrappers, structs, and the `__rue_rt_sigreturn`
        // trampoline) so the RUE-645 stack-overflow trap machinery is
        // type-checked by the unit-test build. We only take its address:
        // calling it would install a process-wide SIGSEGV handler, which must
        // not happen inside the test harness.
        let installer: fn(extern "C" fn(i32) -> !) = install_stack_overflow_handler;
        assert!(installer as usize != 0);
    }
}
