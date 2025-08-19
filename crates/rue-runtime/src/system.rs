//! System-level functions for the Rue runtime
//!
//! This module provides system-level utilities and signal handling.

/// Linux syscall numbers
const SYS_RT_SIGACTION: i64 = 13;
const SYS_EXIT: i64 = 60;

/// Signal numbers
const SIGSEGV: i32 = 11;
const SIGFPE: i32 = 8;

/// Exit codes
const EXIT_CODE_SIGNAL: i32 = 128;

/// Raw syscall wrapper for 1 argument
#[inline(always)]
unsafe fn syscall1(syscall_num: i64, arg1: i64) -> i64 {
    let ret: i64;
    unsafe {
        core::arch::asm!(
            "syscall",
            in("rax") syscall_num,
            in("rdi") arg1,
            lateout("rax") ret,
            out("rcx") _,
            out("r11") _,
            options(nostack, preserves_flags)
        );
    }
    ret
}

/// Raw syscall wrapper for 4 arguments
#[inline(always)]
unsafe fn syscall4(syscall_num: i64, arg1: i64, arg2: i64, arg3: i64, arg4: i64) -> i64 {
    let ret: i64;
    unsafe {
        core::arch::asm!(
            "syscall",
            in("rax") syscall_num,
            in("rdi") arg1,
            in("rsi") arg2,
            in("rdx") arg3,
            in("r10") arg4,
            lateout("rax") ret,
            out("rcx") _,
            out("r11") _,
            options(nostack, preserves_flags)
        );
    }
    ret
}

/// Signal handler for SIGSEGV (segmentation fault)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __rue_sigsegv_handler(_signal: i32) {
    // Exit with signal-specific code
    unsafe {
        syscall1(SYS_EXIT, (EXIT_CODE_SIGNAL + SIGSEGV) as i64);
    }
}

/// Signal handler for SIGFPE (floating point exception, including division by zero)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __rue_sigfpe_handler(_signal: i32) {
    // Exit with signal-specific code
    unsafe {
        syscall1(SYS_EXIT, (EXIT_CODE_SIGNAL + SIGFPE) as i64);
    }
}

/// Set up signal handlers
///
/// This function installs signal handlers for common runtime errors
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __rue_setup_signal_handlers() {
    // For now, we'll implement a minimal signal handler setup
    // In a full implementation, we would set up proper sigaction structures
    // and call rt_sigaction, but for this consolidation we'll keep it simple

    // Note: Full signal handling would require setting up sigaction structures
    // with proper flags and masks, but since this is a consolidation exercise
    // focused on eliminating duplicate symbols, we'll provide stub implementations
}

/// Get the current CPU features
///
/// This function detects available CPU features for optimization
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __rue_get_cpu_features() -> u32 {
    // For this consolidation, we'll return a basic feature set
    // In a full implementation, this would use CPUID instructions
    0 // No special features detected
}
