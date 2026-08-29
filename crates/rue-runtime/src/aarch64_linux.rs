//! AArch64 Linux syscall implementations.
//!
//! This module provides direct syscall wrappers for Linux on AArch64.
//! No libc is used - we invoke the kernel directly via the `svc` instruction.
//!
//! # Platform Requirements
//!
//! This module only compiles on aarch64 Linux. Attempting to compile on other
//! platforms will result in a compile error.
//!
//! # Syscall Conventions
//!
//! On aarch64 Linux:
//! - Syscall number goes in `x8`
//! - Arguments go in `x0`, `x1`, `x2`, `x3`, `x4`, `x5` (in order)
//! - Return value comes back in `x0`
//! - On error, `x0` contains a negative value representing `-errno`
//!
//! # Linux Syscall Numbers
//!
//! AArch64 Linux uses the "new" syscall interface. Syscall numbers are defined in
//! `/usr/include/asm-generic/unistd.h` and differ from x86_64 Linux.

// Compile-time check for platform requirements
#[cfg(not(all(target_arch = "aarch64", target_os = "linux")))]
compile_error!("aarch64_linux module only supports aarch64 Linux");

use core::arch::asm;
use rue_runtime_abi::RuntimeTarget;

/// Linux aarch64 syscall number for read (from asm-generic/unistd.h).
const SYS_READ: u64 = 63;

/// Linux aarch64 syscall number for write (from asm-generic/unistd.h).
const SYS_WRITE: u64 = RuntimeTarget::Aarch64Linux.write_syscall_number();

/// Linux aarch64 syscall number for exit (from asm-generic/unistd.h).
const SYS_EXIT: u64 = 93;

/// Linux aarch64 syscall number for mmap (from asm-generic/unistd.h).
const SYS_MMAP: u64 = 222;

/// Linux aarch64 syscall number for munmap (from asm-generic/unistd.h).
const SYS_MUNMAP: u64 = 215;

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
pub fn write(fd: u64, buf: *const u8, len: usize) -> i64 {
    let result: i64;

    // SAFETY: Making the write(2) syscall is safe because:
    // - The Linux syscall interface is stable and well-defined
    // - We pass arguments in the correct registers per AArch64 Linux ABI
    // - The kernel validates fd, buf, and len; invalid values return errors
    // - Syscall number goes in x8, args in x0-x2, result in x0
    // - The caller is responsible for ensuring buf points to valid memory
    unsafe {
        asm!(
            "svc #0",
            in("x8") SYS_WRITE,
            inlateout("x0") fd as i64 => result,
            in("x1") buf,
            in("x2") len,
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
    // - The Linux syscall interface is stable and well-defined
    // - We pass arguments in the correct registers per AArch64 Linux ABI
    // - The kernel validates fd, buf, and len; invalid values return errors
    // - Syscall number goes in x8, args in x0-x2, result in x0
    // - The caller is responsible for ensuring buf points to writable memory
    unsafe {
        asm!(
            "svc #0",
            in("x8") SYS_READ,
            inlateout("x0") fd as i64 => result,
            in("x1") buf,
            in("x2") len,
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
pub fn write_all(fd: u64, mut buf: &[u8]) -> Result<(), i64> {
    while !buf.is_empty() {
        let result = write(fd, buf.as_ptr(), buf.len());
        if result < 0 {
            // Syscall error - return the errno (as positive)
            return Err(-result);
        }
        if result == 0 {
            // This shouldn't happen for stderr, but handle it to avoid infinite loop.
            return Err(5); // EIO - I/O error
        }
        // Advance past the bytes we successfully wrote
        buf = &buf[result as usize..];
    }
    Ok(())
}

/// Write a message to stderr.
///
/// Best-effort: the `Err` from [`write_all`] is dropped because there is no
/// meaningful recovery and the runtime is typically about to exit.
///
/// The dropped-`Err` path does **not** cover a broken output pipe. We install
/// no `SIGPIPE` handler, so a `write` to a pipe whose reader is gone raises
/// `SIGPIPE` and the kernel terminates the process (exit 141 = 128 + SIGPIPE)
/// before the syscall returns — `write_all` never sees the `EPIPE`. This is
/// Rue's intended Unix default (spec §8.5, RUE-369). Swallowing the `Err` still
/// matters for non-signalling errno failures, e.g. `EBADF` on a closed fd,
/// where the process keeps running.
pub fn write_stderr(msg: &[u8]) {
    let _ = write_all(STDERR, msg);
}

/// Write a message to stdout.
///
/// Best-effort, exactly like [`write_stderr`]: a broken output pipe kills the
/// process via `SIGPIPE` (exit 141) before `write_all` returns, while a
/// non-signalling errno such as `EBADF` from a closed fd is swallowed here.
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

    write_stdout(&buf[pos + 1..]);
}

/// Convert an unsigned 64-bit integer to a decimal string and write it to stdout.
pub fn print_u64(value: u64) {
    let mut buf = [0u8; 22];
    let mut pos = buf.len() - 1;

    buf[pos] = b'\n';
    pos -= 1;

    let mut val = value;

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
    const PROT_READ: u64 = 0x1;
    const PROT_WRITE: u64 = 0x2;
    const MAP_PRIVATE: u64 = 0x02;
    const MAP_ANONYMOUS: u64 = 0x20;

    let result: i64;
    // SAFETY: Making the mmap(2) syscall with anonymous mapping is safe because:
    // - MAP_ANONYMOUS + MAP_PRIVATE creates a private zero-initialized memory region
    // - We request PROT_READ | PROT_WRITE which is safe for heap memory
    // - addr=0 lets the kernel choose a safe address
    // - fd=-1 is correct for anonymous mappings (no file backing)
    // - The kernel validates all parameters and returns an error on failure
    // - Syscall number goes in x8, args in x0-x5, result in x0
    unsafe {
        asm!(
            "svc #0",
            in("x8") SYS_MMAP,
            inlateout("x0") 0u64 => result,  // addr: NULL (let kernel choose)
            in("x1") size,                    // length
            in("x2") PROT_READ | PROT_WRITE,  // prot
            in("x3") MAP_PRIVATE | MAP_ANONYMOUS,  // flags
            in("x4") -1i64 as u64,            // fd: -1 for anonymous
            in("x5") 0u64,                    // offset: 0
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
    // - Syscall number goes in x8, args in x0-x1, result in x0
    unsafe {
        asm!(
            "svc #0",
            in("x8") SYS_MUNMAP,
            inlateout("x0") addr => result,
            in("x1") size,
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
//
// Unlike x86-64, aarch64 Linux needs no user-supplied signal-return trampoline:
// the kernel installs the sigreturn trampoline from the vDSO automatically when
// `SA_RESTORER` is absent, so we leave `sa_restorer` null. (Our handler exits
// and never returns, so it is never reached regardless.)

/// Linux aarch64 syscall number for rt_sigaction (from asm-generic/unistd.h).
const SYS_RT_SIGACTION: u64 = 134;

/// Linux aarch64 syscall number for sigaltstack (from asm-generic/unistd.h).
const SYS_SIGALTSTACK: u64 = 132;

/// `SIGSEGV` signal number on Linux.
const SIGSEGV: u64 = 11;

/// `SA_ONSTACK`: run the handler on the alternate signal stack.
const SA_ONSTACK: u64 = 0x0800_0000;

/// Kernel `struct sigaction` layout on aarch64 Linux (asm-generic ABI): handler,
/// flags, restorer, then mask last.
#[repr(C)]
struct KernelSigaction {
    sa_handler: usize,
    sa_flags: u64,
    sa_restorer: usize,
    sa_mask: u64,
}

/// `stack_t` layout for `sigaltstack` on aarch64 Linux.
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
/// `ss` must point to a valid `StackT` describing a live, writable region.
unsafe fn sigaltstack(ss: *const StackT) -> i64 {
    let result: i64;
    // SAFETY: sigaltstack(ss, oldss=NULL); the kernel validates the pointer.
    unsafe {
        asm!(
            "svc #0",
            in("x8") SYS_SIGALTSTACK,
            inlateout("x0") ss => result,
            in("x1") 0u64, // oldss = NULL
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
/// `act` must point to a valid `KernelSigaction`.
unsafe fn rt_sigaction(sig: u64, act: *const KernelSigaction) -> i64 {
    let result: i64;
    // SAFETY: rt_sigaction(sig, act, oldact=NULL, sigsetsize=8).
    unsafe {
        asm!(
            "svc #0",
            in("x8") SYS_RT_SIGACTION,
            inlateout("x0") sig => result,
            in("x1") act,
            in("x2") 0u64, // oldact = NULL
            in("x3") 8u64, // sigsetsize
        );
    }
    result
}

/// Install the stack-overflow SIGSEGV handler (see module comment above).
///
/// Best-effort: if the alt stack cannot be mapped or a syscall fails, we return
/// without installing anything and keep the default SIGSEGV disposition.
pub fn install_stack_overflow_handler(handler: extern "C" fn(i32) -> !) {
    /// Size of the alternate signal stack (16 KiB).
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
    // SAFETY: `ss` describes the region just mmap'd, mapped for the rest of the
    // process lifetime.
    if unsafe { sigaltstack(&ss) } < 0 {
        return;
    }

    let act = KernelSigaction {
        sa_handler: handler as usize,
        sa_flags: SA_ONSTACK,
        sa_restorer: 0,
        sa_mask: 0,
    };
    // SAFETY: `act` is a valid KernelSigaction; SIGSEGV is a valid signal.
    let _ = unsafe { rt_sigaction(SIGSEGV, &act) };
}

/// Exit the process with the given status code.
///
/// This performs a direct syscall to `exit(2)` and never returns.
pub fn exit(status: i32) -> ! {
    // SAFETY: The exit syscall is always safe to call and never returns.
    unsafe {
        asm!(
            "svc #0",
            in("x8") SYS_EXIT,
            in("x0") status as u64,
            options(noreturn)
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_write_to_stderr() {
        let msg = b"test message\n";
        let result = write(STDERR, msg.as_ptr(), msg.len());
        assert_eq!(result, msg.len() as i64);
    }

    #[test]
    fn test_write_empty() {
        let result = write(STDERR, core::ptr::null(), 0);
        assert_eq!(result, 0);
    }

    #[test]
    fn test_write_invalid_fd() {
        let msg = b"test";
        let result = write(999, msg.as_ptr(), msg.len());
        // Should return negative errno for bad file descriptor
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
    fn test_syscall_constants() {
        // Verify our syscall numbers match Linux aarch64
        assert_eq!(SYS_READ, 63);
        assert_eq!(SYS_WRITE, 64);
        assert_eq!(SYS_EXIT, 93);
        assert_eq!(SYS_MMAP, 222);
        assert_eq!(SYS_MUNMAP, 215);
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
        // `rt_sigaction` wrappers and structs) so the RUE-645 stack-overflow
        // trap machinery is type-checked by the unit-test build. We only take
        // its address: calling it would install a process-wide SIGSEGV handler,
        // which must not happen inside the test harness.
        let installer: fn(extern "C" fn(i32) -> !) = install_stack_overflow_handler;
        assert!(installer as usize != 0);
    }
}
