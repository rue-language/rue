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
//! - **Process exit**: When `main()` returns, generated code calls [`__rue_exit`]
//!   with the return value as the exit code.
//! - **Runtime errors**: Division by zero and integer overflow trigger calls to
//!   [`__rue_div_by_zero`] and [`__rue_overflow`] respectively.
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
//! - **Zero allocations**: The runtime never allocates memory.
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
//! | 1 | Panic (from Rust runtime, shouldn't happen in normal operation) |
//! | 101 | Runtime error (division by zero, overflow) |
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
// Memory intrinsics
// ============================================================================
//
// These functions are required by LLVM/rustc when using ptr::copy_nonoverlapping,
// ptr::write_bytes, etc. in no_std environments. They provide the same functionality
// as the libc functions but are implemented in pure Rust.

/// Copy `n` bytes from `src` to `dst`. The memory regions must not overlap.
///
/// # Safety
///
/// - `dst` must be valid for writes of `n` bytes
/// - `src` must be valid for reads of `n` bytes
/// - The memory regions must not overlap
#[unsafe(no_mangle)]
pub unsafe extern "C" fn memcpy(dst: *mut u8, src: *const u8, n: usize) -> *mut u8 {
    let mut i = 0;
    while i < n {
        *dst.add(i) = *src.add(i);
        i += 1;
    }
    dst
}

/// Copy `n` bytes from `src` to `dst`. The memory regions may overlap.
///
/// # Safety
///
/// - `dst` must be valid for writes of `n` bytes
/// - `src` must be valid for reads of `n` bytes
#[unsafe(no_mangle)]
pub unsafe extern "C" fn memmove(dst: *mut u8, src: *const u8, n: usize) -> *mut u8 {
    if (dst as usize) < (src as usize) {
        // Copy forwards
        let mut i = 0;
        while i < n {
            *dst.add(i) = *src.add(i);
            i += 1;
        }
    } else {
        // Copy backwards to handle overlap
        let mut i = n;
        while i > 0 {
            i -= 1;
            *dst.add(i) = *src.add(i);
        }
    }
    dst
}

/// Fill `n` bytes of memory at `dst` with the byte `c`.
///
/// # Safety
///
/// - `dst` must be valid for writes of `n` bytes
#[unsafe(no_mangle)]
pub unsafe extern "C" fn memset(dst: *mut u8, c: i32, n: usize) -> *mut u8 {
    let byte = c as u8;
    let mut i = 0;
    while i < n {
        *dst.add(i) = byte;
        i += 1;
    }
    dst
}

/// Compare `n` bytes of memory at `s1` and `s2`.
///
/// Returns 0 if equal, negative if s1 < s2, positive if s1 > s2.
///
/// # Safety
///
/// - `s1` must be valid for reads of `n` bytes
/// - `s2` must be valid for reads of `n` bytes
#[unsafe(no_mangle)]
pub unsafe extern "C" fn memcmp(s1: *const u8, s2: *const u8, n: usize) -> i32 {
    let mut i = 0;
    while i < n {
        let a = *s1.add(i);
        let b = *s2.add(i);
        if a != b {
            return (a as i32) - (b as i32);
        }
        i += 1;
    }
    0
}

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
/// Program entry point.
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
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _start() -> ! {
    use core::arch::asm;

    // main is defined by the user's code
    unsafe extern "C" {
        fn main() -> i32;
    }

    let exit_code: i32;
    // SAFETY: We're setting up the stack frame and calling main, which is
    // the expected behavior for a program entry point.
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
    // SAFETY: We're setting up the stack frame and calling main.
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
    // SAFETY: We're setting up the stack frame and calling main.
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

#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
#[unsafe(no_mangle)]
pub extern "C" fn __rue_exit(status: i32) -> ! {
    platform::exit(status)
}

#[cfg(all(target_arch = "aarch64", target_os = "macos"))]
#[unsafe(no_mangle)]
pub extern "C" fn __rue_exit(status: i32) -> ! {
    platform::exit(status)
}

#[cfg(all(target_arch = "aarch64", target_os = "linux"))]
#[unsafe(no_mangle)]
pub extern "C" fn __rue_exit(status: i32) -> ! {
    platform::exit(status)
}

/// Runtime error: division by zero.
///
/// Called when a division or modulo operation has a zero divisor. This is
/// typically triggered by a conditional jump inserted by the compiler before
/// division operations.
///
/// # Behavior
///
/// 1. Writes `"error: division by zero\n"` to stderr (best-effort)
/// 2. Exits with code 101
///
/// # ABI
///
/// ```text
/// extern "C" fn __rue_div_by_zero() -> !
/// ```
///
/// No arguments. Never returns.
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
#[unsafe(no_mangle)]
pub extern "C" fn __rue_div_by_zero() -> ! {
    platform::write_stderr(b"error: division by zero\n");
    platform::exit(101)
}

#[cfg(all(target_arch = "aarch64", target_os = "macos"))]
#[unsafe(no_mangle)]
pub extern "C" fn __rue_div_by_zero() -> ! {
    platform::write_stderr(b"error: division by zero\n");
    platform::exit(101)
}

#[cfg(all(target_arch = "aarch64", target_os = "linux"))]
#[unsafe(no_mangle)]
pub extern "C" fn __rue_div_by_zero() -> ! {
    platform::write_stderr(b"error: division by zero\n");
    platform::exit(101)
}

/// Runtime error: integer overflow.
///
/// Called when an arithmetic operation overflows. This is typically triggered
/// by a conditional jump inserted by the compiler after arithmetic operations
/// that check the overflow flag.
///
/// # Behavior
///
/// 1. Writes `"error: integer overflow\n"` to stderr (best-effort)
/// 2. Exits with code 101
///
/// # ABI
///
/// ```text
/// extern "C" fn __rue_overflow() -> !
/// ```
///
/// No arguments. Never returns.
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
#[unsafe(no_mangle)]
pub extern "C" fn __rue_overflow() -> ! {
    platform::write_stderr(b"error: integer overflow\n");
    platform::exit(101)
}

#[cfg(all(target_arch = "aarch64", target_os = "macos"))]
#[unsafe(no_mangle)]
pub extern "C" fn __rue_overflow() -> ! {
    platform::write_stderr(b"error: integer overflow\n");
    platform::exit(101)
}

#[cfg(all(target_arch = "aarch64", target_os = "linux"))]
#[unsafe(no_mangle)]
pub extern "C" fn __rue_overflow() -> ! {
    platform::write_stderr(b"error: integer overflow\n");
    platform::exit(101)
}

/// Runtime error: index out of bounds.
///
/// Called when an array index operation accesses an element outside the
/// valid range [0, length). The compiler inserts a bounds check before
/// each array access that compares the index against the array length.
///
/// # Behavior
///
/// 1. Writes `"error: index out of bounds\n"` to stderr (best-effort)
/// 2. Exits with code 101
///
/// # ABI
///
/// ```text
/// extern "C" fn __rue_bounds_check() -> !
/// ```
///
/// No arguments. Never returns.
///
/// # Design Notes
///
/// Unlike some languages that include the index and length in the error
/// message, we keep this simple for minimal runtime size. The compiler
/// already performs compile-time checks for constant indices, so this
/// handler is only reached for dynamic indices that fail at runtime.
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
#[unsafe(no_mangle)]
pub extern "C" fn __rue_bounds_check() -> ! {
    platform::write_stderr(b"error: index out of bounds\n");
    platform::exit(101)
}

#[cfg(all(target_arch = "aarch64", target_os = "macos"))]
#[unsafe(no_mangle)]
pub extern "C" fn __rue_bounds_check() -> ! {
    platform::write_stderr(b"error: index out of bounds\n");
    platform::exit(101)
}

#[cfg(all(target_arch = "aarch64", target_os = "linux"))]
#[unsafe(no_mangle)]
pub extern "C" fn __rue_bounds_check() -> ! {
    platform::write_stderr(b"error: index out of bounds\n");
    platform::exit(101)
}

/// Debug intrinsic: print a signed 64-bit integer.
///
/// Called by `@dbg(expr)` when the expression is a signed integer type.
/// Prints the value followed by a newline to stdout.
///
/// # ABI
///
/// ```text
/// extern "C" fn __rue_dbg_i64(value: i64)
/// ```
///
/// - `value` is passed in the `rdi` register (System V AMD64 ABI)
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
#[unsafe(no_mangle)]
pub extern "C" fn __rue_dbg_i64(value: i64) {
    platform::print_i64(value);
}

#[cfg(all(target_arch = "aarch64", target_os = "macos"))]
#[unsafe(no_mangle)]
pub extern "C" fn __rue_dbg_i64(value: i64) {
    platform::print_i64(value);
}

#[cfg(all(target_arch = "aarch64", target_os = "linux"))]
#[unsafe(no_mangle)]
pub extern "C" fn __rue_dbg_i64(value: i64) {
    platform::print_i64(value);
}

/// Debug intrinsic: print an unsigned 64-bit integer.
///
/// Called by `@dbg(expr)` when the expression is an unsigned integer type.
/// Prints the value followed by a newline to stdout.
///
/// # ABI
///
/// ```text
/// extern "C" fn __rue_dbg_u64(value: u64)
/// ```
///
/// - `value` is passed in the `rdi` register (System V AMD64 ABI)
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
#[unsafe(no_mangle)]
pub extern "C" fn __rue_dbg_u64(value: u64) {
    platform::print_u64(value);
}

#[cfg(all(target_arch = "aarch64", target_os = "macos"))]
#[unsafe(no_mangle)]
pub extern "C" fn __rue_dbg_u64(value: u64) {
    platform::print_u64(value);
}

#[cfg(all(target_arch = "aarch64", target_os = "linux"))]
#[unsafe(no_mangle)]
pub extern "C" fn __rue_dbg_u64(value: u64) {
    platform::print_u64(value);
}

/// Debug intrinsic: print a boolean.
///
/// Called by `@dbg(expr)` when the expression is a boolean.
/// Prints "true" or "false" followed by a newline to stdout.
///
/// # ABI
///
/// ```text
/// extern "C" fn __rue_dbg_bool(value: i64)
/// ```
///
/// - `value` is passed in the `rdi` register (System V AMD64 ABI)
/// - Non-zero values are treated as true, zero as false
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
#[unsafe(no_mangle)]
pub extern "C" fn __rue_dbg_bool(value: i64) {
    platform::print_bool(value != 0);
}

#[cfg(all(target_arch = "aarch64", target_os = "macos"))]
#[unsafe(no_mangle)]
pub extern "C" fn __rue_dbg_bool(value: i64) {
    platform::print_bool(value != 0);
}

#[cfg(all(target_arch = "aarch64", target_os = "linux"))]
#[unsafe(no_mangle)]
pub extern "C" fn __rue_dbg_bool(value: i64) {
    platform::print_bool(value != 0);
}

/// Debug intrinsic: print a string.
///
/// Called by `@dbg(expr)` when the expression is a String type.
/// Writes the string content followed by a newline to stdout.
///
/// # ABI
///
/// ```text
/// extern "C" fn __rue_dbg_str(ptr: *const u8, len: u64)
/// ```
///
/// - `ptr` is passed in the first argument register (rdi on x86_64, x0 on aarch64)
/// - `len` is passed in the second argument register (rsi on x86_64, x1 on aarch64)
/// - String is passed as a fat pointer (ptr, len) expanded into two arguments
///
/// # Safety
///
/// The caller must ensure:
/// - `ptr` points to a valid UTF-8 string of `len` bytes
/// - The memory region remains valid for the duration of the call
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
#[unsafe(no_mangle)]
pub extern "C" fn __rue_dbg_str(ptr: *const u8, len: u64) {
    // SAFETY: The caller guarantees ptr and len are valid
    let bytes = unsafe { core::slice::from_raw_parts(ptr, len as usize) };
    platform::write_stdout(bytes);
    platform::write_stdout(b"\n");
}

#[cfg(all(target_arch = "aarch64", target_os = "macos"))]
#[unsafe(no_mangle)]
pub extern "C" fn __rue_dbg_str(ptr: *const u8, len: u64) {
    // SAFETY: The caller guarantees ptr and len are valid
    let bytes = unsafe { core::slice::from_raw_parts(ptr, len as usize) };
    platform::write_stdout(bytes);
    platform::write_stdout(b"\n");
}

#[cfg(all(target_arch = "aarch64", target_os = "linux"))]
#[unsafe(no_mangle)]
pub extern "C" fn __rue_dbg_str(ptr: *const u8, len: u64) {
    // SAFETY: The caller guarantees ptr and len are valid
    let bytes = unsafe { core::slice::from_raw_parts(ptr, len as usize) };
    platform::write_stdout(bytes);
    platform::write_stdout(b"\n");
}

/// String equality comparison.
///
/// Called by the `==` operator on String types. Compares two strings
/// represented as fat pointers (pointer + length pairs).
///
/// # ABI
///
/// ```text
/// extern "C" fn __rue_str_eq(ptr1: *const u8, len1: u64, ptr2: *const u8, len2: u64) -> u8
/// ```
///
/// - `ptr1` is passed in the first argument register (rdi on x86_64, x0 on aarch64)
/// - `len1` is passed in the second argument register (rsi on x86_64, x1 on aarch64)
/// - `ptr2` is passed in the third argument register (rdx on x86_64, x2 on aarch64)
/// - `len2` is passed in the fourth argument register (rcx on x86_64, x3 on aarch64)
/// - Returns 1 if strings are equal, 0 otherwise (in `al`/`w0` register)
///
/// # Implementation
///
/// Fast path: If lengths differ, strings cannot be equal (returns 0).
/// Slow path: Compare bytes one by one until a difference is found or
/// all bytes match.
///
/// # Safety
///
/// The caller must ensure that:
/// - `ptr1` points to a valid buffer of at least `len1` bytes
/// - `ptr2` points to a valid buffer of at least `len2` bytes
/// - Both pointers remain valid for the duration of the call
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
#[unsafe(no_mangle)]
pub extern "C" fn __rue_str_eq(ptr1: *const u8, len1: u64, ptr2: *const u8, len2: u64) -> u8 {
    // Fast path: different lengths means not equal
    if len1 != len2 {
        return 0;
    }

    // Slow path: compare bytes one by one
    // We avoid slice comparison (==) because it generates a call to bcmp,
    // which is a libc function not available in our no_std runtime.
    // SAFETY: Caller guarantees pointers are valid for their respective lengths
    for i in 0..len1 as usize {
        let b1 = unsafe { *ptr1.add(i) };
        let b2 = unsafe { *ptr2.add(i) };
        if b1 != b2 {
            return 0;
        }
    }
    1
}

#[cfg(all(target_arch = "aarch64", target_os = "macos"))]
#[unsafe(no_mangle)]
pub extern "C" fn __rue_str_eq(ptr1: *const u8, len1: u64, ptr2: *const u8, len2: u64) -> u8 {
    // Fast path: different lengths means not equal
    if len1 != len2 {
        return 0;
    }

    // Slow path: compare bytes one by one
    // We avoid slice comparison (==) because it generates a call to bcmp,
    // which may not be available in all runtime environments.
    // SAFETY: Caller guarantees pointers are valid for their respective lengths
    for i in 0..len1 as usize {
        let b1 = unsafe { *ptr1.add(i) };
        let b2 = unsafe { *ptr2.add(i) };
        if b1 != b2 {
            return 0;
        }
    }
    1
}

#[cfg(all(target_arch = "aarch64", target_os = "linux"))]
#[unsafe(no_mangle)]
pub extern "C" fn __rue_str_eq(ptr1: *const u8, len1: u64, ptr2: *const u8, len2: u64) -> u8 {
    // Fast path: different lengths means not equal
    if len1 != len2 {
        return 0;
    }

    // Slow path: compare bytes one by one
    // We avoid slice comparison (==) because it generates a call to bcmp,
    // which is a libc function not available in our no_std runtime.
    // SAFETY: Caller guarantees pointers are valid for their respective lengths
    for i in 0..len1 as usize {
        let b1 = unsafe { *ptr1.add(i) };
        let b2 = unsafe { *ptr2.add(i) };
        if b1 != b2 {
            return 0;
        }
    }
    1
}

// =============================================================================
// Heap Allocation
// =============================================================================

/// Allocate memory from the heap.
///
/// This is the main allocation function for Rue programs. Memory is allocated
/// from a bump allocator backed by `mmap`.
///
/// # Arguments
///
/// * `size` - Number of bytes to allocate
/// * `align` - Required alignment (must be a power of 2)
///
/// # Returns
///
/// A pointer to the allocated memory, or null on failure.
/// The memory is zero-initialized.
///
/// # ABI
///
/// ```text
/// extern "C" fn __rue_alloc(size: u64, align: u64) -> *mut u8
/// ```
///
/// - `size` is passed in the first argument register (rdi on x86_64, x0 on aarch64)
/// - `align` is passed in the second argument register (rsi on x86_64, x1 on aarch64)
/// - Returns pointer in rax (x86_64) or x0 (aarch64)
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
#[unsafe(no_mangle)]
pub extern "C" fn __rue_alloc(size: u64, align: u64) -> *mut u8 {
    heap::alloc(size, align)
}

#[cfg(all(target_arch = "aarch64", target_os = "macos"))]
#[unsafe(no_mangle)]
pub extern "C" fn __rue_alloc(size: u64, align: u64) -> *mut u8 {
    heap::alloc(size, align)
}

#[cfg(all(target_arch = "aarch64", target_os = "linux"))]
#[unsafe(no_mangle)]
pub extern "C" fn __rue_alloc(size: u64, align: u64) -> *mut u8 {
    heap::alloc(size, align)
}

/// Free memory previously allocated by `__rue_alloc`.
///
/// # Arguments
///
/// * `ptr` - Pointer to the memory to free
/// * `size` - Size of the allocation (for future compatibility)
/// * `align` - Alignment of the allocation (for future compatibility)
///
/// # Current Implementation
///
/// This is a **no-op** in the current bump allocator. Memory is only
/// reclaimed when the program exits. The size and align parameters are
/// accepted for API compatibility with future allocators.
///
/// # ABI
///
/// ```text
/// extern "C" fn __rue_free(ptr: *mut u8, size: u64, align: u64)
/// ```
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
#[unsafe(no_mangle)]
pub extern "C" fn __rue_free(ptr: *mut u8, size: u64, align: u64) {
    heap::free(ptr, size, align)
}

#[cfg(all(target_arch = "aarch64", target_os = "macos"))]
#[unsafe(no_mangle)]
pub extern "C" fn __rue_free(ptr: *mut u8, size: u64, align: u64) {
    heap::free(ptr, size, align)
}

#[cfg(all(target_arch = "aarch64", target_os = "linux"))]
#[unsafe(no_mangle)]
pub extern "C" fn __rue_free(ptr: *mut u8, size: u64, align: u64) {
    heap::free(ptr, size, align)
}

/// Reallocate memory to a new size.
///
/// # Arguments
///
/// * `ptr` - Pointer to the existing allocation (or null for new allocation)
/// * `old_size` - Size of the existing allocation (ignored if ptr is null)
/// * `new_size` - Desired new size
/// * `align` - Required alignment (must be a power of 2)
///
/// # Returns
///
/// A pointer to the reallocated memory, or null on failure.
///
/// # Behavior
///
/// - If `ptr` is null: behaves like `__rue_alloc(new_size, align)`
/// - If `new_size` is 0: frees the memory and returns null
/// - If `new_size <= old_size`: returns `ptr` unchanged
/// - If `new_size > old_size`: allocates new block, copies data, returns new pointer
///
/// # ABI
///
/// ```text
/// extern "C" fn __rue_realloc(ptr: *mut u8, old_size: u64, new_size: u64, align: u64) -> *mut u8
/// ```
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
#[unsafe(no_mangle)]
pub extern "C" fn __rue_realloc(ptr: *mut u8, old_size: u64, new_size: u64, align: u64) -> *mut u8 {
    heap::realloc(ptr, old_size, new_size, align)
}

#[cfg(all(target_arch = "aarch64", target_os = "macos"))]
#[unsafe(no_mangle)]
pub extern "C" fn __rue_realloc(ptr: *mut u8, old_size: u64, new_size: u64, align: u64) -> *mut u8 {
    heap::realloc(ptr, old_size, new_size, align)
}

#[cfg(all(target_arch = "aarch64", target_os = "linux"))]
#[unsafe(no_mangle)]
pub extern "C" fn __rue_realloc(ptr: *mut u8, old_size: u64, new_size: u64, align: u64) -> *mut u8 {
    heap::realloc(ptr, old_size, new_size, align)
}

// =============================================================================
// String Runtime Functions
// =============================================================================

/// Minimum capacity for string buffers.
/// This provides room for small appends without immediate reallocation.
const STRING_MIN_CAPACITY: u64 = 16;

/// Allocate a new string buffer with the given capacity.
///
/// # Arguments
///
/// * `cap` - Desired capacity in bytes (will be at least STRING_MIN_CAPACITY)
///
/// # Returns
///
/// A pointer to the allocated buffer, or null on failure.
/// The memory is zero-initialized.
///
/// # ABI
///
/// ```text
/// extern "C" fn __rue_string_alloc(cap: u64) -> *mut u8
/// ```
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
#[unsafe(no_mangle)]
pub extern "C" fn __rue_string_alloc(cap: u64) -> *mut u8 {
    let actual_cap = if cap < STRING_MIN_CAPACITY {
        STRING_MIN_CAPACITY
    } else {
        cap
    };
    heap::alloc(actual_cap, 1) // Strings are byte-aligned
}

#[cfg(all(target_arch = "aarch64", target_os = "macos"))]
#[unsafe(no_mangle)]
pub extern "C" fn __rue_string_alloc(cap: u64) -> *mut u8 {
    let actual_cap = if cap < STRING_MIN_CAPACITY {
        STRING_MIN_CAPACITY
    } else {
        cap
    };
    heap::alloc(actual_cap, 1)
}

#[cfg(all(target_arch = "aarch64", target_os = "linux"))]
#[unsafe(no_mangle)]
pub extern "C" fn __rue_string_alloc(cap: u64) -> *mut u8 {
    let actual_cap = if cap < STRING_MIN_CAPACITY {
        STRING_MIN_CAPACITY
    } else {
        cap
    };
    heap::alloc(actual_cap, 1)
}

/// Reallocate a string buffer to a new capacity.
///
/// Implements the growth strategy: 2x current capacity, minimum STRING_MIN_CAPACITY.
///
/// # Arguments
///
/// * `ptr` - Pointer to the existing buffer (or null for new allocation)
/// * `old_cap` - Current capacity (used for copying data)
/// * `new_cap` - Desired new capacity (will grow by at least 2x if larger)
///
/// # Returns
///
/// A pointer to the new buffer with old data copied, or null on failure.
///
/// # Growth Strategy
///
/// If `new_cap > old_cap`, the actual capacity will be:
/// - `max(new_cap, old_cap * 2, STRING_MIN_CAPACITY)`
///
/// This amortizes allocation cost over many appends.
///
/// # ABI
///
/// ```text
/// extern "C" fn __rue_string_realloc(ptr: *mut u8, old_cap: u64, new_cap: u64) -> *mut u8
/// ```
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
#[unsafe(no_mangle)]
pub extern "C" fn __rue_string_realloc(ptr: *mut u8, old_cap: u64, new_cap: u64) -> *mut u8 {
    string_realloc_impl(ptr, old_cap, new_cap)
}

#[cfg(all(target_arch = "aarch64", target_os = "macos"))]
#[unsafe(no_mangle)]
pub extern "C" fn __rue_string_realloc(ptr: *mut u8, old_cap: u64, new_cap: u64) -> *mut u8 {
    string_realloc_impl(ptr, old_cap, new_cap)
}

#[cfg(all(target_arch = "aarch64", target_os = "linux"))]
#[unsafe(no_mangle)]
pub extern "C" fn __rue_string_realloc(ptr: *mut u8, old_cap: u64, new_cap: u64) -> *mut u8 {
    string_realloc_impl(ptr, old_cap, new_cap)
}

/// Implementation of string realloc, shared across platforms.
#[cfg(any(
    all(target_arch = "x86_64", target_os = "linux"),
    all(target_arch = "aarch64", target_os = "macos"),
    all(target_arch = "aarch64", target_os = "linux")
))]
fn string_realloc_impl(ptr: *mut u8, old_cap: u64, new_cap: u64) -> *mut u8 {
    // Calculate actual new capacity with growth strategy
    let grown_cap = old_cap.saturating_mul(2);
    let actual_cap = new_cap.max(grown_cap).max(STRING_MIN_CAPACITY);

    // Use the general realloc, which handles null ptr and copying
    heap::realloc(ptr, old_cap, actual_cap, 1)
}

/// Clone a string by allocating a new buffer and copying the content.
///
/// # Arguments
///
/// * `ptr` - Pointer to the source string data
/// * `len` - Length of the string in bytes
///
/// # Returns
///
/// A pointer to a new buffer containing a copy of the string data,
/// or null on allocation failure.
///
/// The new buffer has capacity equal to len (minimum STRING_MIN_CAPACITY).
///
/// # Safety
///
/// The caller must ensure `ptr` points to valid memory of at least `len` bytes.
///
/// # ABI
///
/// ```text
/// extern "C" fn __rue_string_clone(ptr: *const u8, len: u64) -> *mut u8
/// ```
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
#[unsafe(no_mangle)]
pub extern "C" fn __rue_string_clone(ptr: *const u8, len: u64) -> *mut u8 {
    string_clone_impl(ptr, len)
}

#[cfg(all(target_arch = "aarch64", target_os = "macos"))]
#[unsafe(no_mangle)]
pub extern "C" fn __rue_string_clone(ptr: *const u8, len: u64) -> *mut u8 {
    string_clone_impl(ptr, len)
}

#[cfg(all(target_arch = "aarch64", target_os = "linux"))]
#[unsafe(no_mangle)]
pub extern "C" fn __rue_string_clone(ptr: *const u8, len: u64) -> *mut u8 {
    string_clone_impl(ptr, len)
}

/// Implementation of string clone, shared across platforms.
#[cfg(any(
    all(target_arch = "x86_64", target_os = "linux"),
    all(target_arch = "aarch64", target_os = "macos"),
    all(target_arch = "aarch64", target_os = "linux")
))]
fn string_clone_impl(ptr: *const u8, len: u64) -> *mut u8 {
    // Allocate new buffer with capacity >= len
    let cap = len.max(STRING_MIN_CAPACITY);
    let new_ptr = heap::alloc(cap, 1);
    if new_ptr.is_null() {
        return new_ptr;
    }

    // Copy the string content
    if len > 0 && !ptr.is_null() {
        // SAFETY: Caller guarantees ptr is valid for len bytes
        // and new_ptr is freshly allocated with at least len bytes
        unsafe {
            core::ptr::copy_nonoverlapping(ptr, new_ptr, len as usize);
        }
    }

    new_ptr
}

/// Drop a String, freeing its heap buffer if it was heap-allocated.
///
/// # Arguments
///
/// * `ptr` - Pointer to the string data
/// * `len` - Length of the string (unused, but part of the String struct)
/// * `cap` - Capacity of the buffer
///
/// # Behavior
///
/// - If `cap == 0`: The string is a literal pointing to rodata; do nothing.
/// - If `cap > 0`: The string is heap-allocated; free the buffer.
///
/// # ABI
///
/// ```text
/// extern "C" fn __rue_drop_String(ptr: *mut u8, len: u64, cap: u64)
/// ```
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
#[unsafe(no_mangle)]
pub extern "C" fn __rue_drop_String(ptr: *mut u8, _len: u64, cap: u64) {
    // Only free heap-allocated strings (cap > 0)
    // Rodata strings have cap == 0 and must not be freed
    if cap > 0 {
        heap::free(ptr, cap, 1);
    }
}

#[cfg(all(target_arch = "aarch64", target_os = "macos"))]
#[unsafe(no_mangle)]
pub extern "C" fn __rue_drop_String(ptr: *mut u8, _len: u64, cap: u64) {
    if cap > 0 {
        heap::free(ptr, cap, 1);
    }
}

#[cfg(all(target_arch = "aarch64", target_os = "linux"))]
#[unsafe(no_mangle)]
pub extern "C" fn __rue_drop_String(ptr: *mut u8, _len: u64, cap: u64) {
    if cap > 0 {
        heap::free(ptr, cap, 1);
    }
}

// Re-export platform functions for tests
#[cfg(all(test, target_arch = "x86_64", target_os = "linux"))]
pub use x86_64_linux::{exit, write, write_all, write_stderr};

#[cfg(all(test, target_arch = "aarch64", target_os = "macos"))]
pub use aarch64_macos::{exit, write, write_all, write_stderr};

#[cfg(all(test, target_arch = "aarch64", target_os = "linux"))]
pub use aarch64_linux::{exit, write, write_all, write_stderr};

#[cfg(test)]
mod tests {
    // Most tests are in x86_64_linux.rs since they test syscall behavior.
    // This module contains tests for the public API and integration.

    #[test]
    fn test_error_messages_are_newline_terminated() {
        // Ensure our error messages end with newlines for clean terminal output
        let div_msg = b"error: division by zero\n";
        let overflow_msg = b"error: integer overflow\n";

        assert!(div_msg.ends_with(b"\n"));
        assert!(overflow_msg.ends_with(b"\n"));
    }

    #[test]
    fn test_error_messages_are_valid_utf8() {
        // Error messages should be valid UTF-8 for proper display
        let div_msg = b"error: division by zero\n";
        let overflow_msg = b"error: integer overflow\n";

        assert!(core::str::from_utf8(div_msg).is_ok());
        assert!(core::str::from_utf8(overflow_msg).is_ok());
    }

    // =========================================================================
    // String Runtime Function Tests
    // =========================================================================

    use super::STRING_MIN_CAPACITY;
    use core::ptr;

    #[test]
    fn test_string_alloc_basic() {
        let ptr = super::__rue_string_alloc(32);
        assert!(!ptr.is_null());

        // Should be usable
        unsafe {
            *ptr = b'H';
            *ptr.add(1) = b'i';
            assert_eq!(*ptr, b'H');
            assert_eq!(*ptr.add(1), b'i');
        }
    }

    #[test]
    fn test_string_alloc_enforces_minimum() {
        // Even with cap=0, should allocate at least STRING_MIN_CAPACITY
        let ptr = super::__rue_string_alloc(0);
        assert!(!ptr.is_null());

        // Should be able to write STRING_MIN_CAPACITY bytes
        unsafe {
            for i in 0..STRING_MIN_CAPACITY as usize {
                *ptr.add(i) = i as u8;
            }
            for i in 0..STRING_MIN_CAPACITY as usize {
                assert_eq!(*ptr.add(i), i as u8);
            }
        }
    }

    #[test]
    fn test_string_alloc_small_request() {
        // Request less than minimum - should still get at least minimum
        let ptr = super::__rue_string_alloc(1);
        assert!(!ptr.is_null());

        // Should be able to write STRING_MIN_CAPACITY bytes
        unsafe {
            for i in 0..STRING_MIN_CAPACITY as usize {
                *ptr.add(i) = (i + 100) as u8;
            }
        }
    }

    #[test]
    fn test_string_realloc_from_null() {
        // Realloc with null pointer should allocate
        let ptr = super::__rue_string_realloc(ptr::null_mut(), 0, 64);
        assert!(!ptr.is_null());

        unsafe {
            *ptr = 42;
            assert_eq!(*ptr, 42);
        }
    }

    #[test]
    fn test_string_realloc_growth_strategy() {
        // Start with a small allocation
        let ptr1 = super::__rue_string_alloc(16);
        assert!(!ptr1.is_null());

        // Write some data
        unsafe {
            for i in 0..16 {
                *ptr1.add(i) = i as u8;
            }
        }

        // Realloc to grow - should use 2x strategy
        // Requesting 24 bytes with old_cap=16 should give us max(24, 32, 16) = 32
        let ptr2 = super::__rue_string_realloc(ptr1, 16, 24);
        assert!(!ptr2.is_null());

        // Data should be preserved
        unsafe {
            for i in 0..16 {
                assert_eq!(*ptr2.add(i), i as u8, "byte {} not preserved", i);
            }
        }
    }

    #[test]
    fn test_string_realloc_large_request() {
        let ptr1 = super::__rue_string_alloc(16);
        assert!(!ptr1.is_null());

        unsafe {
            *ptr1 = 0xAB;
        }

        // Request much larger than 2x current
        let ptr2 = super::__rue_string_realloc(ptr1, 16, 1000);
        assert!(!ptr2.is_null());

        // Original data preserved
        unsafe {
            assert_eq!(*ptr2, 0xAB);
            // Can write to the new larger area
            *ptr2.add(999) = 0xCD;
            assert_eq!(*ptr2.add(999), 0xCD);
        }
    }

    #[test]
    fn test_string_clone_basic() {
        let source = b"Hello, World!";
        let ptr = super::__rue_string_clone(source.as_ptr(), source.len() as u64);
        assert!(!ptr.is_null());

        // Verify content was copied
        unsafe {
            for (i, &byte) in source.iter().enumerate() {
                assert_eq!(*ptr.add(i), byte, "byte {} differs", i);
            }
        }
    }

    #[test]
    fn test_string_clone_empty() {
        // Clone an empty string
        let ptr = super::__rue_string_clone(ptr::null(), 0);
        assert!(!ptr.is_null()); // Should still allocate minimum capacity
    }

    #[test]
    fn test_string_clone_small() {
        // Clone a small string - should still get minimum capacity
        let source = b"Hi";
        let ptr = super::__rue_string_clone(source.as_ptr(), source.len() as u64);
        assert!(!ptr.is_null());

        unsafe {
            assert_eq!(*ptr, b'H');
            assert_eq!(*ptr.add(1), b'i');
        }
    }

    #[test]
    fn test_drop_string_heap() {
        // Allocate a heap string and drop it
        let ptr = super::__rue_string_alloc(64);
        assert!(!ptr.is_null());

        // Write some data
        unsafe {
            *ptr = 0xFF;
        }

        // Drop should not crash (it's a no-op in bump allocator, but validates cap > 0 path)
        super::__rue_drop_String(ptr, 10, 64);
    }

    #[test]
    fn test_drop_string_rodata() {
        // Simulate a rodata string (cap == 0)
        // This should NOT try to free the pointer
        let rodata = b"hello";
        super::__rue_drop_String(rodata.as_ptr() as *mut u8, 5, 0);
        // If we get here without crashing, the test passes
    }

    #[test]
    fn test_drop_string_null() {
        // Drop with null pointer should be safe when cap == 0
        super::__rue_drop_String(ptr::null_mut(), 0, 0);
    }

    #[test]
    fn test_string_lifecycle() {
        // Test a typical string lifecycle: alloc, write, clone, drop

        // Allocate initial buffer
        let ptr = super::__rue_string_alloc(32);
        assert!(!ptr.is_null());

        // Write content
        let content = b"test string";
        unsafe {
            for (i, &byte) in content.iter().enumerate() {
                *ptr.add(i) = byte;
            }
        }

        // Clone it
        let clone_ptr = super::__rue_string_clone(ptr, content.len() as u64);
        assert!(!clone_ptr.is_null());
        assert_ne!(ptr, clone_ptr); // Should be different pointers

        // Verify clone content
        unsafe {
            for (i, &byte) in content.iter().enumerate() {
                assert_eq!(*clone_ptr.add(i), byte);
            }
        }

        // Grow original
        let new_ptr = super::__rue_string_realloc(ptr, 32, 100);
        assert!(!new_ptr.is_null());

        // Original content still there
        unsafe {
            for (i, &byte) in content.iter().enumerate() {
                assert_eq!(*new_ptr.add(i), byte);
            }
        }

        // Drop both
        super::__rue_drop_String(new_ptr, content.len() as u64, 100);
        super::__rue_drop_String(clone_ptr, content.len() as u64, 32);
    }
}
