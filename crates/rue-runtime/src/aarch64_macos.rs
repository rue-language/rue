//! AArch64 macOS syscall implementations.
//!
//! This module provides direct syscall wrappers for macOS on Apple Silicon.
//! No libc is used - we invoke the kernel directly via the `svc` instruction.
//!
//! # Platform Requirements
//!
//! This module only compiles on aarch64 macOS. Attempting to compile on other
//! platforms will result in a compile error.
//!
//! # Syscall Conventions
//!
//! On aarch64 macOS (Darwin):
//! - Syscall number goes in `x16`
//! - Arguments go in `x0`, `x1`, `x2`, `x3`, `x4`, `x5` (in order)
//! - Return value comes back in `x0`
//! - On error, the carry flag is set and `x0` contains the errno
//! - `x16` and `x17` may be clobbered
//!
//! # Darwin Syscall Numbers
//!
//! macOS uses the BSD syscall interface. Syscall numbers are defined in
//! `<sys/syscall.h>` and are different from Linux.

// Compile-time check for platform requirements
#[cfg(not(all(target_arch = "aarch64", target_os = "macos")))]
compile_error!("aarch64_macos module only supports aarch64 macOS");

use core::arch::asm;

/// macOS syscall number for exit (SYS_exit).
const SYS_EXIT: u64 = 1;

/// macOS syscall number for read (SYS_read).
const SYS_READ: u64 = 3;

/// macOS syscall number for write (SYS_write).
const SYS_WRITE: u64 = 4;

/// macOS syscall number for mmap (SYS_mmap).
const SYS_MMAP: u64 = 197;

/// macOS syscall number for munmap (SYS_munmap).
const SYS_MUNMAP: u64 = 73;

/// Standard input file descriptor.
pub const STDIN: u64 = 0;

/// Standard output file descriptor.
pub const STDOUT: u64 = 1;

/// Standard error file descriptor.
pub const STDERR: u64 = 2;

/// Write bytes to a file descriptor.
///
/// This is a thin wrapper around the macOS `write(2)` syscall.
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
    let err_flag: u64;

    // SAFETY: Making the write(2) syscall is safe because:
    // - The Darwin syscall interface is stable and well-defined
    // - We pass arguments in the correct registers per AAPCS64/Darwin ABI
    // - The kernel validates fd, buf, and len; invalid values return errors
    // - We check the carry flag to detect errors (Darwin convention)
    // - We correctly mark x17 as clobbered (per Darwin syscall ABI)
    // - The caller is responsible for ensuring buf points to valid memory
    unsafe {
        asm!(
            "svc #0x80",
            // Check carry flag for error
            "cset {err}, cs",
            inlateout("x16") SYS_WRITE => _,
            in("x0") fd,
            in("x1") buf,
            in("x2") len,
            lateout("x0") result,
            err = out(reg) err_flag,
            // x17 may be clobbered by the syscall
            out("x17") _,
        );
    }

    // If carry flag was set, result is errno (positive), negate it
    if err_flag != 0 { -result } else { result }
}

/// Read bytes from a file descriptor.
///
/// This is a thin wrapper around the macOS `read(2)` syscall.
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
    let err_flag: u64;

    // SAFETY: Making the read(2) syscall is safe because:
    // - The Darwin syscall interface is stable and well-defined
    // - We pass arguments in the correct registers per AAPCS64/Darwin ABI
    // - The kernel validates fd, buf, and len; invalid values return errors
    // - We check the carry flag to detect errors (Darwin convention)
    // - We correctly mark x17 as clobbered (per Darwin syscall ABI)
    // - The caller is responsible for ensuring buf points to writable memory
    unsafe {
        asm!(
            "svc #0x80",
            // Check carry flag for error
            "cset {err}, cs",
            inlateout("x16") SYS_READ => _,
            in("x0") fd,
            in("x1") buf,
            in("x2") len,
            lateout("x0") result,
            err = out(reg) err_flag,
            // x17 may be clobbered by the syscall
            out("x17") _,
        );
    }

    // If carry flag was set, result is errno (positive), negate it
    if err_flag != 0 { -result } else { result }
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
/// This is a wrapper around the macOS `mmap(2)` syscall configured for
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
    // mmap flags (same values as Linux/BSD)
    const PROT_READ: u64 = 0x1;
    const PROT_WRITE: u64 = 0x2;
    const MAP_PRIVATE: u64 = 0x02;
    const MAP_ANONYMOUS: u64 = 0x1000; // Note: macOS uses 0x1000, not 0x20 like Linux

    let result: i64;
    let err_flag: u64;

    // SAFETY: Making the mmap(2) syscall with anonymous mapping is safe because:
    // - MAP_ANONYMOUS + MAP_PRIVATE creates a private zero-initialized memory region
    // - We request PROT_READ | PROT_WRITE which is safe for heap memory
    // - addr=0 lets the kernel choose a safe address
    // - fd=-1 is correct for anonymous mappings (no file backing)
    // - The kernel validates all parameters and returns an error on failure
    // - We check the carry flag to detect errors (Darwin convention)
    // - We correctly mark x17 as clobbered (per Darwin syscall ABI)
    unsafe {
        asm!(
            "svc #0x80",
            // Check carry flag for error
            "cset {err}, cs",
            inlateout("x16") SYS_MMAP => _,
            in("x0") 0u64,                    // addr: NULL (let kernel choose)
            in("x1") size,                    // length
            in("x2") PROT_READ | PROT_WRITE,  // prot
            in("x3") MAP_PRIVATE | MAP_ANONYMOUS,  // flags
            in("x4") -1i64 as u64,            // fd: -1 for anonymous
            in("x5") 0u64,                    // offset: 0
            lateout("x0") result,
            err = out(reg) err_flag,
            out("x17") _,
        );
    }

    // If carry flag was set, syscall failed
    if err_flag != 0 {
        core::ptr::null_mut()
    } else {
        result as *mut u8
    }
}

/// Unmap memory pages previously mapped with `mmap`.
///
/// This is a wrapper around the macOS `munmap(2)` syscall.
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
    let err_flag: u64;

    // SAFETY: Making the munmap(2) syscall is safe because:
    // - The kernel validates addr and size; invalid values return errors
    // - The caller guarantees addr was returned by a previous mmap call
    // - The caller guarantees size matches the mmap call
    // - The caller guarantees the memory won't be accessed after this call
    // - We check the carry flag to detect errors (Darwin convention)
    // - We correctly mark x17 as clobbered (per Darwin syscall ABI)
    unsafe {
        asm!(
            "svc #0x80",
            "cset {err}, cs",
            inlateout("x16") SYS_MUNMAP => _,
            in("x0") addr,
            in("x1") size,
            lateout("x0") result,
            err = out(reg) err_flag,
            out("x17") _,
        );
    }

    // If carry flag was set, result is errno (positive), negate it
    if err_flag != 0 { -result } else { result }
}

/// macOS syscall number for sigaction (SYS_sigaction).
const SYS_SIGACTION: u64 = 46;
/// macOS syscall number for sigaltstack (SYS_sigaltstack).
const SYS_SIGALTSTACK: u64 = 53;
/// Segmentation-fault signal number (same value as Linux).
const SIGSEGV: u64 = 11;
/// Run the handler on the alternate signal stack. Darwin's value (0x0001)
/// differs from Linux's (0x0800_0000).
const SA_ONSTACK: i32 = 0x0001;

// Darwin's `sigaction(2)` (SYS_sigaction = 46) takes a `struct __sigaction`
// that, unlike Linux, carries a CALLER-SUPPLIED signal trampoline
// (`sa_tramp`): on delivery the kernel jumps to `sa_tramp`, not the handler.
// macOS provides no kernel/vDSO trampoline (libc supplies its own
// `_sigtramp`), so this freestanding runtime defines one. The kernel's
// `sendsig` enters it with
//   x0 = catcher (the registered handler), x1 = infostyle, x2 = sig,
//   x3 = siginfo*, x4 = ucontext*.
// Our catcher is `extern "C" fn(i32) -> !` and exits the process, so the
// trampoline only needs to invoke it with the signal number in `x0`; it never
// returns, so no `sigreturn` epilogue is required (the same reasoning as the
// x86-64 Linux restorer, which also never runs because the handler exits).
core::arch::global_asm!(
    ".private_extern _rue_darwin_sigtramp",
    ".globl _rue_darwin_sigtramp",
    ".p2align 2",
    "_rue_darwin_sigtramp:",
    "mov x9, x0", // x9 = catcher (the real handler)
    "mov x0, x2", // x0 = sig (the handler's only argument)
    "blr x9",     // catcher(sig) — never returns (it exits)
    "brk #0x1",   // trap if control ever returns here
);

unsafe extern "C" {
    /// Signal-entry trampoline defined in `global_asm!` above. Address only —
    /// never called directly from Rust. Referencing it from Rust emits a
    /// Mach-O GOT_LOAD relocation pair, which the internal linker statically
    /// relaxes to direct addressing (RUE-707).
    fn rue_darwin_sigtramp();
}

/// Darwin `stack_t` layout: `{ void *ss_sp; size_t ss_size; int ss_flags; }`.
/// Note the field order differs from Linux (size before flags).
#[repr(C)]
struct DarwinStackT {
    ss_sp: *mut u8,
    ss_size: usize,
    ss_flags: i32,
}

/// Darwin `struct __sigaction` — the shape the raw `sigaction` syscall
/// expects: the handler, then the caller-supplied trampoline, then the mask
/// and flags (XNU `bsd/sys/signal.h`).
#[repr(C)]
struct DarwinSigaction {
    sa_handler: usize,
    sa_tramp: usize,
    sa_mask: u32,
    sa_flags: i32,
}

/// Register `nss` as the alternate signal stack (`sigaltstack(2)`).
///
/// # Safety
/// `nss` must point at a valid `DarwinStackT` describing a live memory region.
unsafe fn sigaltstack(nss: *const DarwinStackT) -> i64 {
    let result: i64;
    let err_flag: u64;
    // SAFETY: caller guarantees `nss` is valid; the syscall itself follows the
    // same Darwin convention as every wrapper above (carry flag = error).
    unsafe {
        asm!(
            "svc #0x80",
            "cset {err}, cs",
            inlateout("x16") SYS_SIGALTSTACK => _,
            in("x0") nss,
            in("x1") 0u64, // oss: NULL
            lateout("x0") result,
            err = out(reg) err_flag,
            out("x17") _,
        );
    }
    if err_flag != 0 { -result } else { result }
}

/// Install `nsa` as the disposition for signal `sig` (`sigaction(2)`).
///
/// # Safety
/// `nsa` must point at a valid `DarwinSigaction` whose `sa_tramp` is a valid
/// signal-entry trampoline.
unsafe fn sigaction_syscall(sig: u64, nsa: *const DarwinSigaction) -> i64 {
    let result: i64;
    let err_flag: u64;
    // SAFETY: caller guarantees `nsa` is valid; same syscall convention as the
    // wrappers above.
    unsafe {
        asm!(
            "svc #0x80",
            "cset {err}, cs",
            inlateout("x16") SYS_SIGACTION => _,
            in("x0") sig,
            in("x1") nsa,
            in("x2") 0u64, // osa: NULL
            lateout("x0") result,
            err = out(reg) err_flag,
            out("x17") _,
        );
    }
    if err_flag != 0 { -result } else { result }
}

/// Install the stack-overflow SIGSEGV handler (RUE-645 / RUE-707).
///
/// Maps a 64 KiB alternate signal stack (comfortably above macOS
/// `MINSIGSTKSZ`), registers it with `sigaltstack`, and installs a `SIGSEGV`
/// handler via the raw `sigaction` syscall with `SA_ONSTACK` and our own
/// `sa_tramp`. A stack overflow then aborts cleanly ("stack overflow", exit
/// 101) instead of a raw SIGSEGV (exit 139), matching the Linux targets.
///
/// Every step is best-effort: any syscall failure leaves the default
/// disposition in place, and a wrong trampoline only affects the (rare)
/// stack-overflow delivery path — never a normal program — so the worst case
/// is the prior exit-139 behavior.
///
/// The first attempt at this (PR #1512) made every macOS link fail: the
/// `rue_darwin_sigtramp as usize` reference emits an
/// `ARM64_RELOC_GOT_LOAD_PAGE21/PAGEOFF12` pair, which the internal linker
/// did not support at the time. The linker now statically relaxes those
/// (LDR from the GOT slot -> ADD of the symbol address), so this reference
/// links; behavioral verification is CI's macOS leg running the
/// `stack_overflow.toml` CLI case.
pub fn install_stack_overflow_handler(handler: extern "C" fn(i32) -> !) {
    const ALT_STACK_SIZE: usize = 64 * 1024;

    let stack = mmap(ALT_STACK_SIZE);
    if stack.is_null() {
        return;
    }

    let ss = DarwinStackT {
        ss_sp: stack,
        ss_size: ALT_STACK_SIZE,
        ss_flags: 0,
    };
    // SAFETY: `ss` describes the region just mmap'd, live for the process.
    if unsafe { sigaltstack(&ss) } < 0 {
        return;
    }

    let act = DarwinSigaction {
        sa_handler: handler as usize,
        sa_tramp: rue_darwin_sigtramp as usize,
        sa_mask: 0,
        sa_flags: SA_ONSTACK,
    };
    // SAFETY: `act` is a valid `__sigaction` with a real trampoline; SIGSEGV
    // is a valid signal number.
    let _ = unsafe { sigaction_syscall(SIGSEGV, &act) };
}

/// Exit the process with the given status code.
///
/// This performs a direct syscall to `exit(2)` and never returns.
pub fn exit(status: i32) -> ! {
    // SAFETY: The exit syscall is always safe to call and never returns.
    unsafe {
        asm!(
            "svc #0x80",
            in("x16") SYS_EXIT,
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
        // Verify our syscall numbers match macOS
        assert_eq!(SYS_EXIT, 1);
        assert_eq!(SYS_READ, 3);
        assert_eq!(SYS_WRITE, 4);
        assert_eq!(SYS_MMAP, 197);
        assert_eq!(SYS_MUNMAP, 73);
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
        // Zero-size mmap should fail (returns EINVAL on macOS)
        let ptr = mmap(0);
        assert!(ptr.is_null());
    }

    #[test]
    fn stack_overflow_handler_is_installable() {
        // Reference the installer so the unit-test build type-checks it.
        // Taking its address keeps the signature in sync with the Linux
        // backends without executing anything (installing a real SIGSEGV
        // disposition inside the libtest harness would be hostile).
        let installer: fn(extern "C" fn(i32) -> !) = install_stack_overflow_handler;
        assert!(installer as usize != 0);
    }
}
