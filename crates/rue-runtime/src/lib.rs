//! Rue Runtime Library
//!
//! This crate provides minimal runtime support for Rue programs.
//! It's designed to be compiled as a staticlib and linked into
//! Rue executables.
//!
//! # Overview
//!
//! The Rue compiler generates machine code that calls into this runtime
//! for certain operations that can't be efficiently or safely inlined:
//!
//! - **Process exit**: When `main()` returns, generated code calls [`__rue_exit`](entry::__rue_exit)
//!   with the return value as the exit code.
//! - **Runtime errors**: Division by zero and integer overflow trigger calls to
//!   error handlers in the [`error`] module.
//! - **Debug output**: The `@dbg` builtin calls functions in the [`debug`] module.
//! - **String operations**: String equality, allocation, and methods are in the [`string`] module.
//! - **I/O operations**: Input functions like `readLine()` are in the [`io`] module.
//!
//! # Platform Requirements
//!
//! This runtime supports the following platforms:
//!
//! - **x86-64 Linux**
//! - **aarch64 Linux**
//! - **aarch64 macOS**
//!
//! It uses direct syscalls and contains platform-specific assembly.
//! Attempting to compile on other platforms will result in a compile error.
//!
//! # Design Philosophy
//!
//! The runtime is deliberately minimal:
//!
//! - **`#![no_std]`**: No dependency on the Rust standard library or libc.
//!   All OS interaction happens via direct syscalls.
//! - **Zero allocations**: The runtime never allocates memory (except when explicitly
//!   requested via `__rue_alloc`).
//! - **Small code size**: Compiled with `-Copt-level=z` and LTO for minimal footprint.
//!
//! # Calling Conventions
//!
//! All public functions use the C ABI (`extern "C"`) and are `#[no_mangle]` so
//! they can be called from Rue-generated machine code. The compiler generates
//! `call` instructions to these symbol names.
//!
//! # Exit Codes
//!
//! Rue programs use the following exit codes by convention:
//!
//! | Code | Meaning |
//! |------|---------|
//! | 0 | Success (or whatever `main()` returned) |
//! | 101 | Any panic or runtime trap — overflow, division by zero, bounds check, cast overflow, invalid UTF-8, `@panic`/`@assert`, allocation failure, and the Rust `panic_handler` (spec Appendix B) |
//!
//! # Integration with the Compiler
//!
//! The `rue-linker` crate links this runtime library into every Rue executable.
//! The runtime is compiled as a static library (`.a` file) and its symbols are
//! referenced by generated code in `rue-codegen`.
//!
//! Specifically:
//! - `rue-codegen/src/x86_64/emit.rs` generates `call __rue_*` instructions
//! - `rue-linker` links the runtime archive into the final ELF executable

#![no_std]
// Doc comments before macro invocations are intentional - they document the functions
// that the macro generates. Rust can't attach them automatically, but they serve as
// documentation for readers of this source file.
#![allow(unused_doc_comments)]

// Platform-specific implementations
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
mod x86_64_linux;

#[cfg(all(target_arch = "aarch64", target_os = "macos"))]
mod aarch64_macos;

#[cfg(all(target_arch = "aarch64", target_os = "linux"))]
mod aarch64_linux;

// Heap allocation (available on all supported platforms)
#[cfg(any(
    all(target_arch = "x86_64", target_os = "linux"),
    all(target_arch = "aarch64", target_os = "macos"),
    all(target_arch = "aarch64", target_os = "linux")
))]
mod heap;

#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
use x86_64_linux as platform;

#[cfg(all(target_arch = "aarch64", target_os = "macos"))]
use aarch64_macos as platform;

#[cfg(all(target_arch = "aarch64", target_os = "linux"))]
use aarch64_linux as platform;

// Compile error for unsupported platforms
#[cfg(not(any(
    all(target_arch = "x86_64", target_os = "linux"),
    all(target_arch = "aarch64", target_os = "macos"),
    all(target_arch = "aarch64", target_os = "linux")
)))]
compile_error!(
    "rue-runtime only supports x86-64 Linux, aarch64 Linux, and aarch64 macOS. \
     Other platforms are not currently supported."
);

// ============================================================================
// Platform-agnostic runtime implementations
// ============================================================================

/// Define the Rust implementation behind a manifest-declared runtime wrapper.
///
/// # Usage
///
/// ```ignore
/// define_runtime_implementation! {
///     /// Documentation for the function
///     pub extern "C" fn function_name(arg: Type) -> ReturnType {
///         // implementation
///     }
/// }
/// ```
#[macro_export]
macro_rules! define_runtime_implementation {
    (
        $(#[$meta:meta])*
        pub unsafe extern "C" fn $name:ident($($arg:ident : $arg_ty:ty),* $(,)?) $(-> $ret:ty)? $body:block
    ) => {
        $(#[$meta])*
        pub unsafe fn $name($($arg : $arg_ty),*) $(-> $ret)? $body
    };
    (
        $(#[$meta:meta])*
        pub extern "C" fn $name:ident($($arg:ident : $arg_ty:ty),* $(,)?) $(-> $ret:ty)? $body:block
    ) => {
        $(#[$meta])*
        pub fn $name($($arg : $arg_ty),*) $(-> $ret)? $body
    };
}

// ============================================================================
// Runtime modules
// ============================================================================

pub mod debug;
pub mod entry;
pub mod error;
pub mod io;
pub mod memory;
pub mod parse;
pub mod process;
pub mod random;
pub mod string;
pub mod test_channel;

macro_rules! call_runtime_helper_implementation {
    (__rue_exit($($argument:expr),*)) => { crate::entry::__rue_exit($($argument),*) };
    (__rue_alloc($($argument:expr),*)) => { crate::string::__rue_alloc($($argument),*) };
    (__rue_alloc_zeroed($($argument:expr),*)) => { crate::string::__rue_alloc_zeroed($($argument),*) };
    (__rue_free($($argument:expr),*)) => { crate::string::__rue_free($($argument),*) };
    (__rue_realloc($($argument:expr),*)) => { crate::string::__rue_realloc($($argument),*) };
    (__rue_resize($($argument:expr),*)) => { crate::string::__rue_resize($($argument),*) };
    (__rue_div_by_zero($($argument:expr),*)) => { crate::error::__rue_div_by_zero($($argument),*) };
    (__rue_overflow($($argument:expr),*)) => { crate::error::__rue_overflow($($argument),*) };
    (__rue_intcast_overflow($($argument:expr),*)) => { crate::error::__rue_intcast_overflow($($argument),*) };
    (__rue_bounds_check($($argument:expr),*)) => { crate::error::__rue_bounds_check($($argument),*) };
    (__rue_panic($($argument:expr),*)) => { crate::error::__rue_panic($($argument),*) };
    (__rue_panic_no_msg($($argument:expr),*)) => { crate::error::__rue_panic_no_msg($($argument),*) };
    (__rue_assert_failed($($argument:expr),*)) => { crate::error::__rue_assert_failed($($argument),*) };
    (__rue_dbg_i64($($argument:expr),*)) => { crate::debug::__rue_dbg_i64($($argument),*) };
    (__rue_dbg_u64($($argument:expr),*)) => { crate::debug::__rue_dbg_u64($($argument),*) };
    (__rue_dbg_bool($($argument:expr),*)) => { crate::debug::__rue_dbg_bool($($argument),*) };
    (__rue_dbg_str($($argument:expr),*)) => { crate::debug::__rue_dbg_str($($argument),*) };
    (__rue_str_eq($($argument:expr),*)) => { crate::string::__rue_str_eq($($argument),*) };
    (__rue_str_byte_at($($argument:expr),*)) => { crate::string::__rue_str_byte_at($($argument),*) };
    (__rue_str_char_scalar($($argument:expr),*)) => { crate::string::__rue_str_char_scalar($($argument),*) };
    (__rue_str_char_next($($argument:expr),*)) => { crate::string::__rue_str_char_next($($argument),*) };
    (__rue_str_char_scalar_lossy($($argument:expr),*)) => { crate::string::__rue_str_char_scalar_lossy($($argument),*) };
    (__rue_str_char_next_lossy($($argument:expr),*)) => { crate::string::__rue_str_char_next_lossy($($argument),*) };
    (__rue_to_string($($argument:expr),*)) => { crate::string::__rue_to_string($($argument),*) };
    (__rue_to_string_unsigned($($argument:expr),*)) => { crate::string::__rue_to_string_unsigned($($argument),*) };
    (__rue_to_string_float($($argument:expr),*)) => { crate::string::__rue_to_string_float($($argument),*) };
    (__rue_print($($argument:expr),*)) => { crate::io::__rue_print($($argument),*) };
    (__rue_println($($argument:expr),*)) => { crate::io::__rue_println($($argument),*) };
    (__rue_str_print($($argument:expr),*)) => { crate::io::__rue_str_print($($argument),*) };
    (__rue_str_println($($argument:expr),*)) => { crate::io::__rue_str_println($($argument),*) };
    (__rue_read_line($($argument:expr),*)) => { crate::io::__rue_read_line($($argument),*) };
    (__rue_parse_i32($($argument:expr),*)) => { crate::parse::__rue_parse_i32($($argument),*) };
    (__rue_parse_i64($($argument:expr),*)) => { crate::parse::__rue_parse_i64($($argument),*) };
    (__rue_parse_u32($($argument:expr),*)) => { crate::parse::__rue_parse_u32($($argument),*) };
    (__rue_parse_u64($($argument:expr),*)) => { crate::parse::__rue_parse_u64($($argument),*) };
    (__rue_random_u32($($argument:expr),*)) => { crate::random::__rue_random_u32($($argument),*) };
    (__rue_random_u64($($argument:expr),*)) => { crate::random::__rue_random_u64($($argument),*) };
    (__rue_invalid_utf8($($argument:expr),*)) => { crate::error::__rue_invalid_utf8($($argument),*) };
    (__rue_arg_count($($argument:expr),*)) => { crate::process::__rue_arg_count($($argument),*) };
    (__rue_arg_ptr($($argument:expr),*)) => { crate::process::__rue_arg_ptr($($argument),*) };
    (__rue_arg_len($($argument:expr),*)) => { crate::process::__rue_arg_len($($argument),*) };
    (__rue_env_count($($argument:expr),*)) => { crate::process::__rue_env_count($($argument),*) };
    (__rue_env_ptr($($argument:expr),*)) => { crate::process::__rue_env_ptr($($argument),*) };
    (__rue_env_len($($argument:expr),*)) => { crate::process::__rue_env_len($($argument),*) };
    (__rue_byte_copy($($argument:expr),*)) => { crate::memory::__rue_byte_copy($($argument),*) };
    (__rue_byte_move($($argument:expr),*)) => { crate::memory::__rue_byte_move($($argument),*) };
    (__rue_byte_set($($argument:expr),*)) => { crate::memory::__rue_byte_set($($argument),*) };
    (__rue_test_normalize_process($($argument:expr),*)) => {
        crate::process::__rue_test_normalize_process($($argument),*)
    };
    (__rue_test_complete($($argument:expr),*)) => {
        crate::test_channel::__rue_test_complete($($argument),*)
    };
    (__rue_test_failure_site($($argument:expr),*)) => {
        crate::test_channel::__rue_test_failure_site($($argument),*)
    };
    (__rue_test_fail($($argument:expr),*)) => {
        crate::test_channel::__rue_test_fail($($argument),*)
    };
    (__rue_test_fail_comparison($($argument:expr),*)) => {
        crate::test_channel::__rue_test_fail_comparison($($argument),*)
    };
    (__rue_test_fail_assert($($argument:expr),*)) => {
        crate::test_channel::__rue_test_fail_assert($($argument),*)
    };
    (__rue_test_usage_error($($argument:expr),*)) => {
        crate::test_channel::__rue_test_usage_error($($argument),*)
    };
}

macro_rules! declare_runtime_helper {
    (
        safe $function:ident($($argument:ident : $rust_type:ty),* $(,)?) $(-> $result:ty)?
    ) => {
        #[unsafe(no_mangle)]
        pub extern "C" fn $function($($argument: $rust_type),*) $(-> $result)? {
            call_runtime_helper_implementation!($function($($argument),*))
        }
    };
    (
        unsafe $function:ident($($argument:ident : $rust_type:ty),* $(,)?) $(-> $result:ty)?
    ) => {
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn $function($($argument: $rust_type),*) $(-> $result)? {
            // SAFETY: This wrapper exposes exactly the caller obligations
            // recorded by the canonical runtime ABI manifest.
            unsafe { call_runtime_helper_implementation!($function($($argument),*)) }
        }
    };
}

macro_rules! declare_runtime_helpers {
    (
        $(
            $variant:ident => $safety:ident $function:ident(
                $($argument:ident : $rust_type:ty),* $(,)?
            ) $(-> $result:ty)? {
                symbol: $symbol:literal,
                parameters: $parameters:expr,
                result: $abi_result:expr,
                safety: $contract:expr,
                returns: $returns:expr
            }
        ),+ $(,)?
    ) => {
        $(
            declare_runtime_helper! {
                $safety $function($($argument: $rust_type),*) $(-> $result)?
            }
        )+
    };
}

rue_runtime_abi::for_each_runtime_helper!(declare_runtime_helpers);

#[allow(unused_macros)]
macro_rules! call_reserved_export_implementation {
    (memcpy($($argument:expr),*)) => { crate::memory::memcpy($($argument),*) };
    (memmove($($argument:expr),*)) => { crate::memory::memmove($($argument),*) };
    (memset($($argument:expr),*)) => { crate::memory::memset($($argument),*) };
    (memcmp($($argument:expr),*)) => { crate::memory::memcmp($($argument),*) };
    (bcmp($($argument:expr),*)) => { crate::memory::bcmp($($argument),*) };
    (_main($($argument:expr),*)) => { crate::entry::_main($($argument),*) };
    (__rue_x86_64_linux_start($($argument:expr),*)) => {
        crate::entry::__rue_x86_64_linux_start($($argument),*)
    };
    (__rue_aarch64_linux_start($($argument:expr),*)) => {
        crate::entry::__rue_aarch64_linux_start($($argument),*)
    };
}

macro_rules! declare_reserved_function {
    (
        all unsafe $function:ident($($argument:ident : $rust_type:ty),* $(,)?)
        $(-> $result:ty)?
    ) => {
        // These libc-shaped names must not be present in a libtest binary:
        // the host test harness and its standard library may call them while
        // starting up. Production staticlibs still expose the ABI boundary.
        #[cfg(not(test))]
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn $function($($argument: $rust_type),*) $(-> $result)? {
            // SAFETY: The reserved-export manifest records the complete caller
            // contract for this compiler-built boundary.
            unsafe { call_reserved_export_implementation!($function($($argument),*)) }
        }
    };
    (
        aarch64_macos unsafe $function:ident($($argument:ident : $rust_type:ty),* $(,)?)
        $(-> $result:ty)?
    ) => {
        #[cfg(all(not(test), target_arch = "aarch64", target_os = "macos"))]
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn $function($($argument: $rust_type),*) $(-> $result)? {
            // SAFETY: This is entered only through the platform entry ABI.
            unsafe { call_reserved_export_implementation!($function($($argument),*)) }
        }
    };
    (
        x86_64_linux safe $function:ident($($argument:ident : $rust_type:ty),* $(,)?)
        $(-> $result:ty)?
    ) => {
        #[cfg(all(not(test), target_arch = "x86_64", target_os = "linux"))]
        #[unsafe(no_mangle)]
        pub extern "C" fn $function($($argument: $rust_type),*) $(-> $result)? {
            call_reserved_export_implementation!($function($($argument),*))
        }
    };
    (
        aarch64_linux safe $function:ident($($argument:ident : $rust_type:ty),* $(,)?)
        $(-> $result:ty)?
    ) => {
        #[cfg(all(not(test), target_arch = "aarch64", target_os = "linux"))]
        #[unsafe(no_mangle)]
        pub extern "C" fn $function($($argument: $rust_type),*) $(-> $result)? {
            call_reserved_export_implementation!($function($($argument),*))
        }
    };
}

macro_rules! declare_linux_entry {
    (linux unsafe $function:ident() -> $result:ty) => {
        // x86-64 Linux: a prologue-free shim that captures the untouched entry
        // `%rsp` (which points at argc) into the first argument register before
        // aligning the stack, then tail-calls the Rust start helper. Capturing
        // before `and rsp, -16` is required — the alignment can move `%rsp`, and
        // the helper's own prologue would otherwise clobber the entry pointer.
        #[cfg(all(not(test), target_arch = "x86_64", target_os = "linux"))]
        core::arch::global_asm!(
            concat!(".pushsection .text.", stringify!($function), ",\"ax\",@progbits"),
            concat!(".global ", stringify!($function)),
            concat!(".type ", stringify!($function), ",@function"),
            concat!(stringify!($function), ":"),
            "mov rdi, rsp",
            "and rsp, -16",
            "call {start}",
            "ud2",
            concat!(".size ", stringify!($function), ", .-", stringify!($function)),
            ".popsection",
            start = sym __rue_x86_64_linux_start,
        );

        // AArch64 Linux: the same idea. The kernel enters `_start` with `sp`
        // pointing at argc; a plain Rust entry function's prologue would adjust
        // `sp` before we could read it, so the shim captures `sp` into `x0`
        // first and calls the Rust start helper.
        #[cfg(all(not(test), target_arch = "aarch64", target_os = "linux"))]
        core::arch::global_asm!(
            concat!(".pushsection .text.", stringify!($function), ",\"ax\",@progbits"),
            concat!(".global ", stringify!($function)),
            concat!(".type ", stringify!($function), ",@function"),
            concat!(stringify!($function), ":"),
            "mov x0, sp",
            // AArch64 logical-immediate ops cannot target SP directly; align
            // through a scratch register (x9 is caller-saved and dead at entry).
            "and x9, x0, #-16",
            "mov sp, x9",
            "bl {start}",
            "brk #0x1",
            concat!(".size ", stringify!($function), ", .-", stringify!($function)),
            ".popsection",
            start = sym __rue_aarch64_linux_start,
        );

        #[cfg(all(
            not(test),
            any(
                all(target_arch = "x86_64", target_os = "linux"),
                all(target_arch = "aarch64", target_os = "linux")
            )
        ))]
        unsafe extern "C" {
            fn $function() -> $result;
        }

        #[cfg(all(
            not(test),
            any(
                all(target_arch = "x86_64", target_os = "linux"),
                all(target_arch = "aarch64", target_os = "linux")
            )
        ))]
        const _: unsafe extern "C" fn() -> $result = $function;
    };
}

macro_rules! declare_reserved_assembly {
    (x86_64_linux unsafe $function:ident()) => {
        #[cfg(all(target_arch = "x86_64", target_os = "linux"))]
        core::arch::global_asm!(
            concat!(
                ".pushsection .text.",
                stringify!($function),
                ",\"ax\",@progbits"
            ),
            concat!(".globl ", stringify!($function)),
            concat!(".hidden ", stringify!($function)),
            concat!(".type ", stringify!($function), ",@function"),
            concat!(stringify!($function), ":"),
            "mov rax, 15",
            "syscall",
            concat!(
                ".size ",
                stringify!($function),
                ", .-",
                stringify!($function)
            ),
            ".popsection",
        );

        #[cfg(all(target_arch = "x86_64", target_os = "linux"))]
        unsafe extern "C" {
            pub(crate) fn $function();
        }

        #[cfg(all(target_arch = "x86_64", target_os = "linux"))]
        const _: unsafe extern "C" fn() = $function;
    };
}

macro_rules! declare_reserved_exports {
    (
        $(
            ($variant:ident => $($row:tt)*)
        ),+ $(,)?
    ) => {
        $(
            declare_reserved_exports!(@row $variant => $($row)*);
        )+
    };
    (
        @row $variant:ident => function $target:ident $safety:ident $function:ident(
            $($argument:ident : $rust_type:ty),* $(,)?
        ) $(-> $result:ty)? {
            class: $class:path,
            signature: $signature:ident
        }
    ) => {
        declare_reserved_function! {
            $target $safety $function($($argument: $rust_type),*) $(-> $result)?
        }
    };
    (
        @row $variant:ident => linux_entry $target:ident $safety:ident $function:ident()
            -> $result:ty {
                class: $class:path,
                signature: $signature:ident
            }
    ) => {
        declare_linux_entry! {
            $target $safety $function() -> $result
        }
    };
    (
        @row $variant:ident => assembly $target:ident $safety:ident $function:ident()
            $(-> $result:ty)? {
                class: $class:path,
                signature: $signature:ident
            }
    ) => {
        declare_reserved_assembly! {
            $target $safety $function()
        }
    };
    (
        @row RuntimeAbiVersion => marker all {
            class: $class:path,
            size: 1
        }
    ) => {};
}

rue_runtime_abi::for_each_reserved_runtime_export!(declare_reserved_exports);

macro_rules! declare_runtime_abi_marker {
    ($version:literal, $marker:ident) => {
        #[doc(hidden)]
        #[used]
        #[unsafe(no_mangle)]
        pub static $marker: u8 = 0;
    };
}

rue_runtime_abi::runtime_abi_version!(declare_runtime_abi_marker);

// Re-export platform functions for tests
#[cfg(all(test, target_arch = "x86_64", target_os = "linux"))]
pub use x86_64_linux::{exit, write, write_all, write_stderr};

#[cfg(all(test, target_arch = "aarch64", target_os = "macos"))]
pub use aarch64_macos::{exit, write, write_all, write_stderr};

#[cfg(all(test, target_arch = "aarch64", target_os = "linux"))]
pub use aarch64_linux::{exit, write, write_all, write_stderr};

#[cfg(test)]
mod boundary_tests {
    #[test]
    fn null_pointer_is_valid_at_zero_length_boundaries() {
        let null = core::ptr::null();

        // SAFETY: each boundary explicitly permits null for a zero-length
        // input, and every out-pointer below names valid exclusive storage.
        unsafe {
            crate::debug::__rue_dbg_str(null, 0);
            crate::io::__rue_print(null, 0, 0);
            crate::io::__rue_println(null, 0, 0);
            assert!(crate::memory::memcpy(core::ptr::null_mut(), null, 0).is_null());
            assert!(crate::memory::memmove(core::ptr::null_mut(), null, 0).is_null());
            assert!(crate::memory::memset(core::ptr::null_mut(), 0, 0).is_null());
            assert_eq!(crate::memory::memcmp(null, null, 0), 0);
            assert_eq!(crate::memory::bcmp(null, null, 0), 0);
            assert_eq!(crate::string::__rue_str_eq(null, 0, null, 0), 1);
            crate::string::__rue_free(core::ptr::null_mut(), 0, 1);

            let mut parsed = crate::parse::OptionIntResult { disc: 0, value: 1 };
            crate::parse::__rue_parse_i32(&mut parsed, null, 0, 7, 9);
            assert_eq!(parsed.disc, 9);
            assert_eq!(parsed.value, 0);
        }
    }
}
