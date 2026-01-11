//! Program entry points and exit handling.
//!
//! This module provides:
//! - Platform-specific `_start` / `_main` entry points
//! - `__rue_exit` function called when main() returns
//! - Panic handler for no_std environments

use crate::platform;

/// Panic handler for `#![no_std]` environments.
///
/// This handler is only active when the crate is compiled as a library (not
/// during tests, which use the standard library's panic handler). When a panic
/// occurs, we exit with code 101.
///
/// # Why `#[cfg(not(test))]`?
///
/// During testing, Rust's test harness provides its own panic handler that
/// catches panics and reports them as test failures. If we provided a panic
/// handler, it would conflict with the test harness and prevent proper test
/// execution.
#[cfg(all(
    not(test),
    any(
        all(target_arch = "x86_64", target_os = "linux"),
        all(target_arch = "aarch64", target_os = "macos"),
        all(target_arch = "aarch64", target_os = "linux")
    )
))]
#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    platform::exit(101)
}

/// Program entry point for Linux x86-64.
///
/// The Linux kernel starts execution here with RSP 16-byte aligned.
/// The System V AMD64 ABI expects RSP to be 8-byte aligned at function entry
/// (i.e., 16-byte aligned before `call` pushes the return address).
///
/// `_start` bridges this gap by:
/// 1. Aligning the stack for function calls (sub $8, %rsp)
/// 2. Calling `main` (the user's entry point)
/// 3. Passing the return value to `__rue_exit`
///
/// # ABI
///
/// ```text
/// _start:
///     sub $8, %rsp      ; align stack (kernel gives 16-byte, we need 8-byte before call)
///     call main         ; call user's main function
///     mov %eax, %edi    ; pass return value as exit code
///     call __rue_exit   ; exit (never returns)
/// ```
#[cfg(all(not(test), target_arch = "x86_64", target_os = "linux"))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _start() -> ! {
    use core::arch::asm;

    // main is defined by the user's code
    unsafe extern "C" {
        fn main() -> i32;
    }

    let exit_code: i32;
    // SAFETY: This is the program entry point called by the kernel.
    // - The kernel starts execution with RSP 16-byte aligned
    // - We adjust the stack to maintain proper alignment for the call
    // - `main` is an extern "C" function defined by user code and linked in
    // - The assembly uses the System V AMD64 calling convention
    // - After `main` returns, we pass its return value to exit()
    // - This function never returns (we call exit() which is noreturn)
    unsafe {
        asm!(
            // Stack alignment: kernel starts us with 16-byte aligned RSP.
            // The `call main` will push 8 bytes (return address), making RSP
            // 8-byte aligned when main starts - exactly what the ABI expects.
            // But first we need to align to 16 bytes before call, so subtract 8.
            "sub rsp, 8",
            "call {main}",
            // Return value is in eax
            "mov edi, eax",
            main = sym main,
            out("edi") exit_code,
            clobber_abi("C"),
        );
    }
    platform::exit(exit_code)
}

/// Program entry point for macOS aarch64.
///
/// On macOS, the entry point is `_main` (or `start` for dyld). The kernel
/// starts execution with SP 16-byte aligned. The AAPCS64 ABI expects SP
/// to be 16-byte aligned at function entry.
#[cfg(all(not(test), target_arch = "aarch64", target_os = "macos"))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _main() -> ! {
    use core::arch::asm;

    // main is defined by the user's code
    unsafe extern "C" {
        fn main() -> i32;
    }

    let exit_code: i32;
    // SAFETY: This is the program entry point called by the kernel.
    // - The kernel starts execution with SP 16-byte aligned
    // - `main` is an extern "C" function defined by user code and linked in
    // - The assembly uses the AAPCS64 calling convention
    // - After `main` returns, we pass its return value (in w0) to exit()
    // - This function never returns (we call exit() which is noreturn)
    unsafe {
        asm!(
            // Call user's main function
            "bl {main}",
            // Return value is in w0
            main = sym main,
            lateout("w0") exit_code,
            clobber_abi("C"),
        );
    }
    platform::exit(exit_code)
}

/// Program entry point for Linux aarch64.
///
/// On Linux, the entry point is `_start`. The kernel starts execution
/// with SP 16-byte aligned. The AAPCS64 ABI expects SP to be 16-byte
/// aligned at function entry.
#[cfg(all(not(test), target_arch = "aarch64", target_os = "linux"))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _start() -> ! {
    use core::arch::asm;

    // main is defined by the user's code
    unsafe extern "C" {
        fn main() -> i32;
    }

    let exit_code: i32;
    // SAFETY: This is the program entry point called by the kernel.
    // - The kernel starts execution with SP 16-byte aligned
    // - `main` is an extern "C" function defined by user code and linked in
    // - The assembly uses the AAPCS64 calling convention
    // - After `main` returns, we pass its return value (in w0) to exit()
    // - This function never returns (we call exit() which is noreturn)
    unsafe {
        asm!(
            // Call user's main function
            "bl {main}",
            // Return value is in w0
            main = sym main,
            lateout("w0") exit_code,
            clobber_abi("C"),
        );
    }
    platform::exit(exit_code)
}

crate::define_for_all_platforms! {
    /// Exit the process with the given status code.
    ///
    /// This is the main entry point called by Rue-generated code when `main()`
    /// returns. The return value of `main()` becomes the exit code.
    ///
    /// # ABI
    ///
    /// ```text
    /// extern "C" fn __rue_exit(status: i32) -> !
    /// ```
    ///
    /// - `status` is passed in the `edi` register (System V AMD64 ABI)
    /// - This function never returns
    ///
    /// # Example
    ///
    /// Generated code for `fn main() -> i32 { 42 }`:
    /// ```asm
    /// main:
    ///     mov eax, 42
    ///     ret
    /// _start:
    ///     call main
    ///     mov edi, eax
    ///     call __rue_exit
    /// ```
    pub extern "C" fn __rue_exit(status: i32) -> ! {
        platform::exit(status)
    }
}

#[cfg(test)]
mod tests {
    // Entry point tests would require process spawning, so we keep them minimal here.
    // The main integration is tested via spec tests.
}
