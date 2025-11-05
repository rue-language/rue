//! Linux syscall wrappers
//!
//! This module provides safe wrappers around Linux system calls using inline assembly.
//! All syscalls follow the System V AMD64 ABI.
//!
//! # Syscall Mechanism
//!
//! Linux x86-64 syscalls use the following registers:
//! - RAX: syscall number
//! - RDI: arg1
//! - RSI: arg2
//! - RDX: arg3
//! - R10: arg4 (note: not RCX)
//! - R8: arg5
//! - R9: arg6
//! - Return value: RAX (negative on error)
//!
//! The `syscall` instruction clobbers RCX and R11.

use crate::abi::*;

/// Result type for syscall operations
///
/// On success, contains the return value (usually bytes transferred or 0).
/// On error, contains the negated errno value.
pub type SyscallResult<T> = Result<T, i32>;

/// Execute a syscall with 0 arguments
#[inline(always)]
pub unsafe fn syscall0(num: i64) -> i64 {
    let ret: i64;
    unsafe {
        core::arch::asm!(
            "syscall",
            in("rax") num,
            lateout("rax") ret,
            out("rcx") _,  // clobbered by syscall
            out("r11") _,  // clobbered by syscall
            options(nostack, preserves_flags)
        );
    }
    ret
}

/// Execute a syscall with 1 argument
#[inline(always)]
pub unsafe fn syscall1(num: i64, arg1: i64) -> i64 {
    let ret: i64;
    unsafe {
        core::arch::asm!(
            "syscall",
            in("rax") num,
            in("rdi") arg1,
            lateout("rax") ret,
            out("rcx") _,
            out("r11") _,
            options(nostack, preserves_flags)
        );
    }
    ret
}

/// Execute a syscall with 2 arguments
#[inline(always)]
pub unsafe fn syscall2(num: i64, arg1: i64, arg2: i64) -> i64 {
    let ret: i64;
    unsafe {
        core::arch::asm!(
            "syscall",
            in("rax") num,
            in("rdi") arg1,
            in("rsi") arg2,
            lateout("rax") ret,
            out("rcx") _,
            out("r11") _,
            options(nostack, preserves_flags)
        );
    }
    ret
}

/// Execute a syscall with 3 arguments
#[inline(always)]
pub unsafe fn syscall3(num: i64, arg1: i64, arg2: i64, arg3: i64) -> i64 {
    let ret: i64;
    unsafe {
        core::arch::asm!(
            "syscall",
            in("rax") num,
            in("rdi") arg1,
            in("rsi") arg2,
            in("rdx") arg3,
            lateout("rax") ret,
            out("rcx") _,
            out("r11") _,
            options(nostack, preserves_flags)
        );
    }
    ret
}

/// Write to a file descriptor
///
/// # Safety
/// - `buf` must be valid for `len` bytes
pub unsafe fn sys_write(fd: i32, buf: *const u8, len: usize) -> SyscallResult<usize> {
    let ret = unsafe { syscall3(SYS_WRITE, fd as i64, buf as i64, len as i64) };
    if ret < 0 {
        Err((-ret) as i32)
    } else {
        Ok(ret as usize)
    }
}

/// Read from a file descriptor
///
/// # Safety
/// - `buf` must be valid for `len` bytes
pub unsafe fn sys_read(fd: i32, buf: *mut u8, len: usize) -> SyscallResult<usize> {
    let ret = unsafe { syscall3(SYS_READ, fd as i64, buf as i64, len as i64) };
    if ret < 0 {
        Err((-ret) as i32)
    } else {
        Ok(ret as usize)
    }
}

/// Exit the process
///
/// # Safety
/// This function never returns
pub unsafe fn sys_exit(code: i32) -> ! {
    unsafe {
        syscall1(SYS_EXIT, code as i64);
    }
    // Should never reach here, but Rust requires unreachable code annotation
    core::hint::unreachable_unchecked()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sys_write_stdout() {
        let msg = b"test\n";
        let result = unsafe { sys_write(STDOUT_FD, msg.as_ptr(), msg.len()) };
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), msg.len());
    }

    #[test]
    fn test_sys_write_invalid_fd() {
        let msg = b"test\n";
        let result = unsafe { sys_write(-1, msg.as_ptr(), msg.len()) };
        assert!(result.is_err());
        // EBADF is error 9
        assert_eq!(result.unwrap_err(), 9);
    }

    #[test]
    fn test_sys_read_empty() {
        let mut buf = [0u8; 16];
        // Reading from a non-blocking stdin might return EAGAIN or 0
        // We just test that the syscall doesn't crash
        let result = unsafe { sys_read(STDIN_FD, buf.as_mut_ptr(), 0) };
        // Reading 0 bytes should succeed and return 0
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), 0);
    }
}
