//! Startup and system initialization for the Rue runtime
//!
//! This module provides the _start function and system initialization code.

use crate::buffered_io::BufferedStdout;

/// Linux syscall numbers
const SYS_WRITE: i64 = 1;

/// Linux syscall numbers
const SYS_EXIT: i64 = 60;

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

/// Raw syscall wrapper for 3 arguments (for write syscall)
#[inline(always)]
unsafe fn syscall3(syscall_num: i64, arg1: i64, arg2: i64, arg3: i64) -> i64 {
    let ret: i64;
    unsafe {
        core::arch::asm!(
            "syscall",
            in("rax") syscall_num,
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

/// Debug trace helper - writes directly to stderr using syscalls
/// Only compiled in debug builds to avoid production crashes
#[cfg(debug_assertions)]
#[inline(always)]
unsafe fn debug_trace(msg: &[u8]) {
    unsafe {
        // Write to stderr (fd 2)
        syscall3(SYS_WRITE, 2, msg.as_ptr() as i64, msg.len() as i64);
    }
}

/// Force debug trace that always executes (for this debugging session)
#[inline(always)]
unsafe fn force_debug_trace(msg: &[u8]) {
    unsafe {
        // Write to stderr (fd 2)
        syscall3(SYS_WRITE, 2, msg.as_ptr() as i64, msg.len() as i64);
    }
}

/// No-op version for release builds
#[cfg(not(debug_assertions))]
#[inline(always)]
unsafe fn debug_trace(_msg: &[u8]) {
    // No-op in release builds
}

/// Exit the program with the given status code
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __rue_exit(status: i32) -> ! {
    // Flush stdout buffer before exiting
    let _ = BufferedStdout::flush();

    unsafe {
        syscall1(SYS_EXIT, status as i64);
    }

    // This should never be reached
    loop {}
}

/// Runtime wrapper that calls the user's main function
///
/// This function sets up the runtime environment and calls the user's main function
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __rue_main() -> i32 {
    #[cfg(debug_assertions)]
    unsafe {
        debug_trace(b"[DEBUG] __rue_main: entering\n")
    };

    // Initialize the heap allocator
    #[cfg(debug_assertions)]
    unsafe {
        debug_trace(b"[DEBUG] __rue_main: calling heap_init\n")
    };

    unsafe { crate::allocator::__rue_heap_init() };

    #[cfg(debug_assertions)]
    unsafe {
        debug_trace(b"[DEBUG] __rue_main: heap_init done\n")
    };

    // Call the user's main function
    // This should be linked from the compiled Rue program
    unsafe extern "C" {
        fn main() -> i32;
    }

    // Check if main function pointer looks valid (not null)
    let main_ptr = main as *const ();
    if main_ptr.is_null() {
        #[cfg(debug_assertions)]
        unsafe {
            debug_trace(b"[ERROR] main function pointer is null!\n")
        };
        unsafe { __rue_exit(-1) };
    }

    #[cfg(debug_assertions)]
    unsafe {
        debug_trace(b"[DEBUG] __rue_main: calling user main()\n")
    };

    unsafe { force_debug_trace(b"[FORCE] About to call user main()\n") };

    let result = unsafe { main() };

    #[cfg(debug_assertions)]
    unsafe {
        debug_trace(b"[DEBUG] __rue_main: user main() returned\n")
    };

    unsafe { force_debug_trace(b"[FORCE] User main() returned\n") };

    // Flush stdout before returning
    let _ = BufferedStdout::flush();

    result
}

/// Program entry point
///
/// This is the actual entry point that the linker looks for (_start).
/// It sets up the minimal runtime environment and calls __rue_main.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _start() -> ! {
    // Force debug output to show we reached _start
    unsafe { force_debug_trace(b"[FORCE] _start() entered\n") };

    // Call the runtime main function
    let exit_code = unsafe { __rue_main() };

    unsafe { force_debug_trace(b"[FORCE] _start() about to exit\n") };

    // Exit with the return code
    unsafe { __rue_exit(exit_code) };
}
