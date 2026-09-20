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

/// SIGSEGV handler installed at process entry, which turns a fault into a clean
/// abort (RUE-645) and says which kind of fault it was (RUE-2163).
///
/// # Why this exists
///
/// Deep/unbounded recursion exhausts the main stack; the next push faults on the
/// kernel guard page and raises `SIGSEGV`. With no handler the default
/// disposition kills the process, and the shell reports the raw crash (exit code
/// 139 = 128 + SIGSEGV). Rust and Go instead catch this and print a readable
/// "stack overflow" message before exiting non-zero. This handler does the same,
/// and exits with code 101 — the same abort code the other runtime traps use
/// (division by zero, overflow, bounds).
///
/// # Running on an exhausted stack
///
/// The handler cannot run on the main stack, which is what overflowed. The entry
/// code registers a small alternate signal stack via `sigaltstack` and installs
/// this handler with `SA_ONSTACK`, so the kernel switches to that alt stack to
/// deliver the signal. See each platform's `install_segv_handler`.
///
/// # Classifying the fault
///
/// A blown stack is no longer the only `SIGSEGV` a Rue program can raise: a
/// `checked` block can write through a null or wild raw pointer (spec chapter
/// 9), and a C FFI callee can fault anywhere. The handler is therefore installed
/// with `SA_SIGINFO` and reads the faulting address out of the `siginfo_t` the
/// kernel supplies. An address inside the window below the captured stack base
/// is a stack overflow and keeps the pinned `stack overflow` message; anything
/// else reports `segmentation fault at 0x<address>`. [`crate::fault`] owns the
/// window rule and the two messages.
///
/// The signal-number argument is unused: this handler is registered for
/// `SIGSEGV` and, on hosted Darwin, the equivalent guard-page `SIGBUS`. The
/// interrupted context is unused too — the handler exits rather than resuming.
///
/// # Never returns
///
/// The handler calls `platform::exit` and does not return. Because it never
/// returns, the kernel's signal-return trampoline is never executed, so no
/// `sa_restorer` needs to be supplied on Linux.
#[cfg(any(
    all(target_arch = "x86_64", target_os = "linux"),
    all(target_arch = "aarch64", target_os = "macos"),
    all(target_arch = "aarch64", target_os = "linux")
))]
pub(crate) extern "C" fn __rue_segv_handler(_sig: i32, info: *const u8, _context: *const u8) -> ! {
    // SAFETY: the kernel delivers this handler's `siginfo_t` in `info`, and the
    // platform decoder tolerates a null pointer.
    let address = unsafe { platform::fault_address(info) };

    if crate::fault::is_stack_overflow(address) {
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
    } else {
        let mut buffer = [0u8; crate::fault::SEGFAULT_MESSAGE_MAX];
        platform::write_stderr(crate::fault::segfault_message(address, &mut buffer));
    }
    platform::exit(101)
}

#[cfg(any(
    all(target_arch = "x86_64", target_os = "linux"),
    all(target_arch = "aarch64", target_os = "macos"),
    all(target_arch = "aarch64", target_os = "linux")
))]
const _: crate::fault::SegvHandler = __rue_segv_handler;

/// Initialize process-wide runtime state before user code. Hosted entries
/// reach this only after libc startup; their parked I/O, reporting, and fault
/// state must all be ready before any worker can run.
#[cfg(all(
    not(test),
    any(
        all(target_arch = "x86_64", target_os = "linux"),
        all(target_arch = "aarch64", target_os = "macos"),
        all(target_arch = "aarch64", target_os = "linux")
    )
))]
fn initialize_runtime(stack_top: usize) -> Result<(), i32> {
    #[cfg(rue_hosted_threads)]
    {
        crate::io::initialize_hosted()?;
        // SAFETY: startup calls this once before any user worker can access
        // the reporting mutex, whose static address remains stable.
        unsafe { crate::test_channel::initialize_hosted_reporting() }?;
        crate::fault::initialize_main(stack_top, platform::stack_limit())
    }
    #[cfg(not(rue_hosted_threads))]
    {
        crate::fault::record_stack_window(stack_top, platform::stack_limit());
        platform::install_segv_handler(__rue_segv_handler);
        Ok(())
    }
}

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

    // The initial `%rsp` is the base of the main stack, which is what the
    // SIGSEGV handler classifies a faulting address against (RUE-2163).
    if initialize_runtime(stack as usize).is_err() {
        platform::exit(101);
    }

    // SAFETY: `main` is the linked Rue entry function and uses the C ABI.
    let exit_code = unsafe { main() };
    platform::exit(exit_code)
}

/// The raw Linux entry saves the untouched stack before entering libc. Keep it
/// in stable private startup state until this callback receives libc's fully
/// initialized process, then perform the shared capture/fault setup immediately
/// before generated `main`.
#[cfg(all(
    not(test),
    rue_hosted_threads,
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
static HOSTED_START_STACK: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);

#[cfg(all(
    not(test),
    rue_hosted_threads,
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
extern "C" fn __rue_hosted_main(
    argc: i32,
    argv: *mut *mut libc::c_char,
    envp: *mut *mut libc::c_char,
) -> i32 {
    unsafe extern "C" {
        fn main() -> i32;
    }

    // SAFETY: libc supplies the loader-owned vectors unchanged.
    unsafe {
        crate::process::capture(
            argc as u64,
            argv as *const *const u8,
            envp as *const *const u8,
        )
    };
    let stack = HOSTED_START_STACK.load(core::sync::atomic::Ordering::Acquire);
    if initialize_runtime(stack).is_err() {
        platform::exit(101);
    }

    // SAFETY: libc invokes this callback with the generated Rue `main`, which
    // uses the runtime's native zero-argument C ABI.
    let exit_code = unsafe { main() };
    platform::exit(exit_code)
}

#[cfg(all(
    not(test),
    rue_hosted_threads,
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
unsafe extern "C" {
    // glibc does not expose this private startup entry through libc's Rust
    // bindings. Its C ABI is stable for the dynamic Linux startup contract.
    fn __libc_start_main(
        main: extern "C" fn(i32, *mut *mut libc::c_char, *mut *mut libc::c_char) -> i32,
        argc: libc::c_int,
        argv: *mut *mut libc::c_char,
        init: Option<extern "C" fn()>,
        fini: Option<extern "C" fn()>,
        rtld_fini: Option<unsafe extern "C" fn()>,
        stack_end: *mut libc::c_void,
    ) -> libc::c_int;
}

#[cfg(all(
    not(test),
    rue_hosted_threads,
    target_arch = "x86_64",
    target_os = "linux"
))]
pub(crate) unsafe fn __rue_x86_64_linux_hosted_start(
    stack: *const usize,
    rtld_fini: *mut libc::c_void,
) -> ! {
    // SAFETY: the assembly shim passes the untouched startup stack and loader
    // finalizer from their process-entry locations.
    unsafe { __rue_linux_hosted_start(stack, rtld_fini) }
}

#[cfg(all(
    not(test),
    rue_hosted_threads,
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
unsafe fn __rue_linux_hosted_start(stack: *const usize, rtld_fini: *mut libc::c_void) -> ! {
    HOSTED_START_STACK.store(stack as usize, core::sync::atomic::Ordering::Release);

    // SAFETY: the dynamic loader supplies `rtld_fini` as a function pointer in
    // the target's process-entry register, and the remaining arguments follow
    // glibc's documented startup ABI.
    let rtld_fini = unsafe {
        core::mem::transmute::<*mut libc::c_void, Option<unsafe extern "C" fn()>>(rtld_fini)
    };
    let result = unsafe {
        __libc_start_main(
            __rue_hosted_main,
            *stack as libc::c_int,
            stack.add(1) as *mut *mut libc::c_char,
            None,
            None,
            rtld_fini,
            stack as *mut libc::c_void,
        )
    };
    platform::exit(result)
}

#[cfg(all(
    not(test),
    rue_hosted_threads,
    target_arch = "aarch64",
    target_os = "linux"
))]
pub(crate) unsafe fn __rue_aarch64_linux_hosted_start(
    stack: *const usize,
    rtld_fini: *mut libc::c_void,
) -> ! {
    // SAFETY: the assembly shim passes the untouched startup stack and loader
    // finalizer from their process-entry locations.
    unsafe { __rue_linux_hosted_start(stack, rtld_fini) }
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

    // dyld hands us argc/argv/envp rather than the raw entry stack, so the
    // stack base the SIGSEGV handler classifies against is read from `sp` here
    // (RUE-2163). This is a frame or two below the true base, which only widens
    // the overflow window downward and so cannot lose a real overflow.
    let stack_top: usize;
    // SAFETY: reading the stack pointer has no side effects.
    unsafe {
        asm!("mov {}, sp", out(reg) stack_top, options(nomem, nostack, preserves_flags));
    }
    if initialize_runtime(stack_top).is_err() {
        platform::exit(101);
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

    // The initial `sp` is the base of the main stack, which is what the SIGSEGV
    // handler classifies a faulting address against (RUE-2163).
    if initialize_runtime(stack as usize).is_err() {
        platform::exit(101);
    }

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

#[cfg(test)]
mod tests {
    extern crate std;

    use self::std::process::{Command, Stdio};
    use self::std::string::String;
    use self::std::thread;
    use self::std::time::{Duration, Instant};

    #[test]
    fn process_exit_from_worker_terminates_subprocess() {
        const CHILD_ENV: &str = "RUE_PROCESS_EXIT_WORKER_CHILD";
        const REQUESTED_EXIT_STATUS: i32 = 37;

        if self::std::env::var_os(CHILD_ENV).is_some() {
            thread::spawn(|| crate::platform::exit(REQUESTED_EXIT_STATUS))
                .join()
                .unwrap();
            unreachable!("the worker's process-wide exit returned");
        }

        let mut child = Command::new(self::std::env::current_exe().expect("current test binary"))
            .args([
                "--exact",
                "entry::tests::process_exit_from_worker_terminates_subprocess",
                "--nocapture",
            ])
            .env(CHILD_ENV, "1")
            // With panic=abort, libtest otherwise adds a subprocess that
            // translates the requested status into a failed-test status.
            .env(
                "__RUST_TEST_INVOKE",
                "entry::tests::process_exit_from_worker_terminates_subprocess",
            )
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn process-exit child");

        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(status) = child.try_wait().expect("wait for process-exit child") {
                let output = child
                    .wait_with_output()
                    .expect("collect process-exit child output");
                assert_eq!(
                    status.code(),
                    Some(REQUESTED_EXIT_STATUS),
                    "child stderr: {}",
                    String::from_utf8_lossy(&output.stderr)
                );
                return;
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!("process-exit child did not terminate within ten seconds");
            }
            thread::sleep(Duration::from_millis(10));
        }
    }
}
