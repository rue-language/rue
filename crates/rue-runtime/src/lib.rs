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
// Platform-agnostic macro for reducing code duplication
// ============================================================================
//
// Many runtime functions have identical implementations across platforms,
// differing only in their `#[cfg]` attributes. This macro generates all
// three platform-specific versions from a single definition.

/// Define a function for all supported platforms with identical implementation.
///
/// This macro generates three `#[cfg]`-gated versions of the same function,
/// one for each supported platform (x86_64 Linux, aarch64 macOS, aarch64 Linux).
///
/// # Usage
///
/// ```ignore
/// define_for_all_platforms! {
///     /// Documentation for the function
///     pub extern "C" fn function_name(arg: Type) -> ReturnType {
///         // implementation
///     }
/// }
/// ```
#[macro_export]
macro_rules! define_for_all_platforms {
    (
        $(#[$meta:meta])*
        pub unsafe extern "C" fn $name:ident($($arg:ident : $arg_ty:ty),* $(,)?) $(-> $ret:ty)? $body:block
    ) => {
        #[cfg(all(target_arch = "x86_64", target_os = "linux"))]
        $(#[$meta])*
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn $name($($arg : $arg_ty),*) $(-> $ret)? $body

        #[cfg(all(target_arch = "aarch64", target_os = "macos"))]
        $(#[$meta])*
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn $name($($arg : $arg_ty),*) $(-> $ret)? $body

        #[cfg(all(target_arch = "aarch64", target_os = "linux"))]
        $(#[$meta])*
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn $name($($arg : $arg_ty),*) $(-> $ret)? $body
    };
    (
        $(#[$meta:meta])*
        pub extern "C" fn $name:ident($($arg:ident : $arg_ty:ty),* $(,)?) $(-> $ret:ty)? $body:block
    ) => {
        #[cfg(all(target_arch = "x86_64", target_os = "linux"))]
        $(#[$meta])*
        #[unsafe(no_mangle)]
        pub extern "C" fn $name($($arg : $arg_ty),*) $(-> $ret)? $body

        #[cfg(all(target_arch = "aarch64", target_os = "macos"))]
        $(#[$meta])*
        #[unsafe(no_mangle)]
        pub extern "C" fn $name($($arg : $arg_ty),*) $(-> $ret)? $body

        #[cfg(all(target_arch = "aarch64", target_os = "linux"))]
        $(#[$meta])*
        #[unsafe(no_mangle)]
        pub extern "C" fn $name($($arg : $arg_ty),*) $(-> $ret)? $body
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
pub mod random;
pub mod string;

// Re-export StringResult for use by other modules
pub use string::StringResult;

// Re-export platform functions for tests
#[cfg(all(test, target_arch = "x86_64", target_os = "linux"))]
pub use x86_64_linux::{exit, write, write_all, write_stderr};

#[cfg(all(test, target_arch = "aarch64", target_os = "macos"))]
pub use aarch64_macos::{exit, write, write_all, write_stderr};

#[cfg(all(test, target_arch = "aarch64", target_os = "linux"))]
pub use aarch64_linux::{exit, write, write_all, write_stderr};

#[cfg(test)]
mod export_safety_tests {
    extern crate std;

    use self::std::{string::String, vec::Vec};
    const MODULE_SOURCES: &[(&str, &str)] = &[
        ("aarch64_linux", include_str!("aarch64_linux.rs")),
        ("aarch64_macos", include_str!("aarch64_macos.rs")),
        ("debug", include_str!("debug.rs")),
        ("entry", include_str!("entry.rs")),
        ("error", include_str!("error.rs")),
        ("heap", include_str!("heap.rs")),
        ("io", include_str!("io.rs")),
        ("memory", include_str!("memory.rs")),
        ("parse", include_str!("parse.rs")),
        ("random", include_str!("random.rs")),
        ("string", include_str!("string.rs")),
        ("x86_64_linux", include_str!("x86_64_linux.rs")),
    ];

    /// Every extern function declaration whose signature contains a raw
    /// pointer: symbol, whether it is unsafe, and declaration count. Counts are
    /// one for macro-defined exports and three for hand-written platform copies.
    /// This is intentionally checked against the
    /// source declaration rather than by assigning function pointers: Rust
    /// permits a safe function pointer to coerce to an unsafe one, which would
    /// let an accidentally-safe export pass a type-only test.
    const POINTER_EXPORTS: &[(&str, bool, usize)] = &[
        ("__rue_String_byte_at", true, 1),
        ("__rue_String_capacity", false, 3),
        ("__rue_String_char_next", true, 1),
        ("__rue_String_char_next_lossy", true, 1),
        ("__rue_String_char_scalar", true, 1),
        ("__rue_String_char_scalar_lossy", true, 1),
        ("__rue_String_clear", true, 3),
        ("__rue_String_clone", true, 3),
        ("__rue_String_concat", true, 1),
        ("__rue_String_contains", true, 1),
        ("__rue_String_is_empty", false, 3),
        ("__rue_String_len", false, 3),
        ("__rue_String_new", true, 3),
        ("__rue_String_push", true, 3),
        ("__rue_String_push_str", true, 3),
        ("__rue_String_reserve", true, 3),
        ("__rue_String_starts_with", true, 1),
        ("__rue_String_substring", true, 1),
        ("__rue_String_with_capacity", true, 3),
        ("__rue_alloc", false, 1),
        ("__rue_dbg_str", true, 1),
        ("__rue_drop_String", false, 1),
        ("__rue_free", false, 1),
        ("__rue_panic", true, 1),
        ("__rue_parse_i32", true, 1),
        ("__rue_parse_i64", true, 1),
        ("__rue_parse_u32", true, 1),
        ("__rue_parse_u64", true, 1),
        ("__rue_print", true, 1),
        ("__rue_println", true, 1),
        ("__rue_read_line", true, 3),
        ("__rue_realloc", true, 1),
        ("__rue_str_byte_at", true, 1),
        ("__rue_str_char_next", true, 1),
        ("__rue_str_char_next_lossy", true, 1),
        ("__rue_str_char_scalar", true, 1),
        ("__rue_str_char_scalar_lossy", true, 1),
        ("__rue_str_eq", true, 1),
        ("__rue_str_print", true, 1),
        ("__rue_str_println", true, 1),
        ("__rue_to_string", true, 1),
        ("__rue_to_string_unsigned", true, 1),
        ("bcmp", true, 1),
        ("memcmp", true, 1),
        ("memcpy", true, 1),
        ("memmove", true, 1),
        ("memset", true, 1),
    ];

    fn pointer_exports(source: &str) -> Vec<(&str, bool)> {
        let lines: Vec<_> = source.lines().collect();
        let mut exports = Vec::new();
        let mut line_index = 0;

        while line_index < lines.len() {
            let line = lines[line_index].trim_start();
            let has_c_abi = line.contains("extern \"C\" fn ");
            let has_export_attr = lines[line_index.saturating_sub(8)..line_index]
                .iter()
                .any(|line| line.contains("no_mangle"));
            let looks_like_function =
                line.contains(" fn ") || line.starts_with("fn ") || line.starts_with("unsafe fn ");
            if line.starts_with("///") || !looks_like_function || (!has_c_abi && !has_export_attr) {
                line_index += 1;
                continue;
            }
            let declaration_prefix = line
                .split_once(" fn ")
                .map(|(prefix, _)| prefix)
                .unwrap_or(line);
            let is_unsafe = declaration_prefix
                .split_whitespace()
                .any(|word| word == "unsafe");

            let mut signature = String::from(line);
            while !signature.contains('{') {
                line_index += 1;
                signature.push(' ');
                signature.push_str(lines[line_index].trim());
            }

            if signature.contains("*const ") || signature.contains("*mut ") {
                let after_fn = line
                    .split_once(" fn ")
                    .map(|(_, rest)| rest)
                    .or_else(|| line.strip_prefix("fn "))
                    .or_else(|| line.strip_prefix("unsafe fn "))
                    .expect("export declaration has fn keyword");
                let name = after_fn
                    .split_once('(')
                    .expect("export declaration has arguments")
                    .0;
                exports.push((name, is_unsafe));
            }
            line_index += 1;
        }

        exports
    }

    #[test]
    fn raw_pointer_export_safety_is_explicit_and_complete() {
        let mut actual = Vec::new();
        for &(_, source) in MODULE_SOURCES {
            actual.extend(pointer_exports(source));
        }
        actual.extend(pointer_exports(include_str!("lib.rs")));
        actual.sort_unstable();

        let mut counted = Vec::new();
        for &(name, is_unsafe) in &actual {
            match counted.last_mut() {
                Some((last_name, last_unsafe, count))
                    if *last_name == name && *last_unsafe == is_unsafe =>
                {
                    *count += 1;
                }
                _ => counted.push((name, is_unsafe, 1)),
            }
        }

        assert_eq!(counted, POINTER_EXPORTS);
    }

    #[test]
    fn every_declared_runtime_module_is_in_the_export_inventory() {
        let lib_source = include_str!("lib.rs");
        let mut declared = Vec::new();
        for line in lib_source.lines().map(str::trim) {
            let declaration = line
                .strip_prefix("pub mod ")
                .or_else(|| line.strip_prefix("mod "));
            if let Some(name) = declaration.and_then(|rest| rest.strip_suffix(';')) {
                declared.push(name);
            }
        }
        declared.sort_unstable();

        let mut inventoried: Vec<_> = MODULE_SOURCES.iter().map(|(name, _)| *name).collect();
        inventoried.sort_unstable();
        assert_eq!(inventoried, declared);
    }

    #[test]
    fn pointer_taking_safe_exports_have_no_pointer_precondition() {
        // These exports never inspect their pointer argument. The String query
        // functions return another ABI field, while free/drop are deliberate
        // bump-allocator no-ops. Calling any of them with an arbitrary pointer
        // therefore cannot violate a Rust memory-safety invariant.
        let arbitrary = core::ptr::dangling_mut::<u8>();
        assert_eq!(crate::string::__rue_String_len(arbitrary, 7, 11), 7);
        assert_eq!(crate::string::__rue_String_capacity(arbitrary, 7, 11), 11);
        assert_eq!(crate::string::__rue_String_is_empty(arbitrary, 7, 11), 0);
        crate::string::__rue_free(arbitrary, u64::MAX, 3);
        crate::string::__rue_drop_String(arbitrary, u64::MAX, u64::MAX);
    }

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

            let mut parsed = crate::parse::OptionIntResult { disc: 0, value: 1 };
            crate::parse::__rue_parse_i32(&mut parsed, null, 0, 7, 9);
            assert_eq!(parsed.disc, 9);
            assert_eq!(parsed.value, 0);

            let mut substring = crate::string::StringResult {
                ptr: core::ptr::dangling_mut(),
                len: u64::MAX,
                cap: u64::MAX,
            };
            crate::string::__rue_String_substring(&mut substring, null, 0, 0, 0, 0);
            assert!(substring.ptr.is_null());
            assert_eq!((substring.len, substring.cap), (0, 0));

            assert_eq!(
                crate::string::__rue_String_contains(null, 0, 0, null, 0, 0),
                1
            );
            assert_eq!(
                crate::string::__rue_String_starts_with(null, 0, 0, null, 0, 0),
                1
            );

            let mut concatenated = crate::string::StringResult {
                ptr: core::ptr::dangling_mut(),
                len: u64::MAX,
                cap: u64::MAX,
            };
            crate::string::__rue_String_concat(&mut concatenated, null, 0, 0, null, 0, 0);
            assert!(concatenated.ptr.is_null());
            assert_eq!((concatenated.len, concatenated.cap), (0, 0));
        }
    }
}
