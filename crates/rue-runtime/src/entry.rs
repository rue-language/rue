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

/// SIGSEGV handler installed by `_start` to turn a stack overflow into a clean
/// abort (RUE-645).
///
/// # Why this exists
///
/// Deep/unbounded recursion exhausts the main stack; the next push faults on the
/// kernel guard page and raises `SIGSEGV`. With no handler the default
/// disposition kills the process, and the shell reports the raw crash (exit code
/// 139 = 128 + SIGSEGV). Rust and Go instead catch this and print a readable
/// "stack overflow" message before exiting non-zero. This handler does the same:
/// it writes `"stack overflow\n"` to stderr and exits with code 101 — the same
/// abort code the other runtime traps use (division by zero, overflow, bounds).
///
/// # Running on an exhausted stack
///
/// The handler cannot run on the main stack, which is what overflowed. `_start`
/// registers a small alternate signal stack via `sigaltstack` and installs this
/// handler with `SA_ONSTACK`, so the kernel switches to that alt stack to deliver
/// the signal. See each platform's `install_stack_overflow_handler`.
///
/// # Treating any SIGSEGV as stack overflow
///
/// We do not inspect `siginfo.si_addr` to confirm the fault is near the stack
/// pointer. In safe Rue there is no other way to reach `SIGSEGV`: array accesses
/// are bounds-checked, there are no null/raw-pointer dereferences, and arithmetic
/// traps rather than corrupting memory. So any `SIGSEGV` a compiled Rue program
/// can produce is a stack overflow, and reporting it as such is correct. This
/// keeps the handler freestanding (no `SA_SIGINFO`, no `siginfo_t` layout).
///
/// The signal-number argument is unused for the same reason: this handler is
/// only ever registered for `SIGSEGV`.
///
/// # Never returns
///
/// The handler calls `platform::exit` and does not return. Because it never
/// returns, the kernel's signal-return trampoline is never executed, so no
/// `sa_restorer` needs to be supplied on Linux.
#[cfg(all(
    not(test),
    any(
        all(target_arch = "x86_64", target_os = "linux"),
        all(target_arch = "aarch64", target_os = "macos"),
        all(target_arch = "aarch64", target_os = "linux")
    )
))]
pub(crate) extern "C" fn __rue_stack_overflow_handler(_sig: i32) -> ! {
    // "stack overflow\n" built byte-by-byte to avoid the macOS byte-string
    // linker bug (mirrors the message handlers in `error.rs`).
    let mut msg = [0u8; 15];
    msg[0] = b's';
    msg[1] = b't';
    msg[2] = b'a';
    msg[3] = b'c';
    msg[4] = b'k';
    msg[5] = b' ';
    msg[6] = b'o';
    msg[7] = b'v';
    msg[8] = b'e';
    msg[9] = b'r';
    msg[10] = b'f';
    msg[11] = b'l';
    msg[12] = b'o';
    msg[13] = b'w';
    msg[14] = b'\n';
    platform::write_stderr(&msg);
    platform::exit(101)
}

#[cfg(all(
    not(test),
    any(
        all(target_arch = "x86_64", target_os = "linux"),
        all(target_arch = "aarch64", target_os = "macos"),
        all(target_arch = "aarch64", target_os = "linux")
    )
))]
const _: extern "C" fn(i32) -> ! = __rue_stack_overflow_handler;

/// Normal SysV function called by the prologue-free x86-64 Linux entry shim.
///
/// `stack` is the untouched initial `%rsp` the shim captured before aligning
/// the stack; the System V startup layout places argc there, so the process
/// module derives argc/argv/envp from it (RUE-935).
#[cfg(all(not(test), target_arch = "x86_64", target_os = "linux"))]
pub(crate) fn __rue_x86_64_linux_start(stack: *const usize) -> ! {
    unsafe extern "C" {
        fn main() -> i32;
    }

    // Capture argc/argv/envp from the entry stack before any user code runs so
    // `std.env` can read them later (RUE-935).
    // SAFETY: `stack` is the initial process stack pointer from `_start`.
    unsafe { crate::process::capture_from_stack(stack) };

    // Install the stack-overflow SIGSEGV handler before running user code so a
    // deep-recursion overflow aborts cleanly (RUE-645) instead of dying with a
    // raw SIGSEGV (exit 139). Best-effort: if setup fails the program simply
    // keeps the default SIGSEGV disposition.
    platform::install_stack_overflow_handler(__rue_stack_overflow_handler);

    // SAFETY: `main` is the linked Rue entry function and uses the C ABI.
    let exit_code = unsafe { main() };
    platform::exit(exit_code)
}

/// Program entry point for macOS aarch64.
///
/// The Rue Mach-O executable is a dynamic executable (`LC_MAIN`), so dyld's
/// bootstrap calls this entry point as the C `main(argc, argv, envp, apple)`:
/// argc in `w0`, argv in `x1`, envp in `x2` (apple, in `x3`, is ignored). This
/// differs from the Linux targets, where the kernel enters `_start` with the
/// raw stack and a shim recovers the pointers. Receiving them as `extern "C"`
/// parameters is what lets us capture them (RUE-935) — a register-only read
/// would be clobbered by this function's prologue.
///
/// # Safety
///
/// This must only be entered by dyld with the AAPCS64 register/stack state of
/// the process entry point.
#[cfg(all(not(test), target_arch = "aarch64", target_os = "macos"))]
pub(crate) unsafe fn _main(argc: i32, argv: *const *const u8, envp: *const *const u8) -> ! {
    use core::arch::asm;

    // main is defined by the user's code
    unsafe extern "C" {
        fn main() -> i32;
    }

    // Capture argc/argv/envp handed to us by dyld before running user code so
    // `std.env` can read them later (RUE-935).
    // SAFETY: `argv`/`envp` are the loader-supplied vectors for this process.
    unsafe { crate::process::capture(argc as u64, argv, envp) };

    // Install the stack-overflow SIGSEGV handler before running user code.
    // (A deliberate no-op on macOS; the Darwin trampoline is tracked by
    // RUE-707.)
    platform::install_stack_overflow_handler(__rue_stack_overflow_handler);

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

/// Normal AAPCS64 function called by the prologue-free AArch64 Linux entry
/// shim.
///
/// `stack` is the untouched initial `sp` the shim captured before aligning the
/// stack; the System V startup layout places argc there (argc at `[sp]`, argv
/// following, envp after the argv NULL), so the process module derives
/// argc/argv/envp from it (RUE-935).
///
/// # Safety
///
/// This must only be entered from the `_start` shim with the initial stack
/// pointer the kernel supplied at process entry.
#[cfg(all(not(test), target_arch = "aarch64", target_os = "linux"))]
pub(crate) fn __rue_aarch64_linux_start(stack: *const usize) -> ! {
    use core::arch::asm;

    // main is defined by the user's code
    unsafe extern "C" {
        fn main() -> i32;
    }

    // Capture argc/argv/envp from the entry stack before any user code runs so
    // `std.env` can read them later (RUE-935).
    // SAFETY: `stack` is the initial process stack pointer from `_start`.
    unsafe { crate::process::capture_from_stack(stack) };

    // Install the stack-overflow SIGSEGV handler before running user code so a
    // deep-recursion overflow aborts cleanly (RUE-645) instead of dying with a
    // raw SIGSEGV (exit 139). Best-effort: if setup fails the program simply
    // keeps the default SIGSEGV disposition.
    platform::install_stack_overflow_handler(__rue_stack_overflow_handler);

    let exit_code: i32;
    // SAFETY:
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

crate::define_runtime_implementation! {
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

// No unit tests for this module: the `_start`/`_main` entry points and
// `__rue_exit` cannot be exercised in-process (they require being the real
// program entry point / terminate the process). They are covered end-to-end
// by the spec and CLI integration suites, which run compiled Rue programs
// and assert on their exit codes.
