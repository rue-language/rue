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
//! This runtime only supports **x86-64 Linux**. It uses direct syscalls and
//! contains platform-specific assembly. Attempting to compile on other platforms
//! will result in a compile error.
//!
//! # Design Philosophy
//!
//! The runtime is deliberately minimal:
//!
//! - **`#![no_std]`**: No dependency on the Rust standard library or libc.
//!   All OS interaction happens via direct syscalls.
//! - **Heap allocation**: The runtime provides mmap-based heap allocation for
//!   mutable strings and other dynamic data.
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

#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
use x86_64_linux as platform;

#[cfg(all(target_arch = "aarch64", target_os = "macos"))]
use aarch64_macos as platform;

// Compile error for unsupported platforms
#[cfg(not(any(
    all(target_arch = "x86_64", target_os = "linux"),
    all(target_arch = "aarch64", target_os = "macos")
)))]
compile_error!(
    "rue-runtime only supports x86-64 Linux and aarch64 macOS. \
     Other platforms are not currently supported."
);

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
        all(target_arch = "aarch64", target_os = "macos")
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

/// Debug intrinsic: print a string.
///
/// Called by `@dbg(expr)` when the expression is a String type.
/// Writes the string content followed by a newline to stdout.
///
/// # ABI
///
/// ```text
/// extern "C" fn __rue_dbg_str(ptr: *const u8, len: u64, cap: i64)
/// ```
///
/// - `ptr` is passed in the first argument register (rdi on x86_64, x0 on aarch64)
/// - `len` is passed in the second argument register (rsi on x86_64, x1 on aarch64)
/// - `cap` is passed in the third argument register (rdx on x86_64, x2 on aarch64)
/// - String is passed as a 3-tuple (ptr, len, cap) expanded into three arguments
///
/// # Safety
///
/// The caller must ensure:
/// - `ptr` points to a valid UTF-8 string of `len` bytes
/// - The memory region remains valid for the duration of the call
/// - `cap` is either -1 (rodata) or >= 0 (heap-allocated)
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
#[unsafe(no_mangle)]
pub extern "C" fn __rue_dbg_str(ptr: *const u8, len: u64, _cap: i64) {
    // SAFETY: The caller guarantees ptr and len are valid
    // Note: capacity is not used for printing, but accepted for consistency
    // with the 3-tuple string representation
    let bytes = unsafe { core::slice::from_raw_parts(ptr, len as usize) };
    platform::write_stdout(bytes);
    platform::write_stdout(b"\n");
}

#[cfg(all(target_arch = "aarch64", target_os = "macos"))]
#[unsafe(no_mangle)]
pub extern "C" fn __rue_dbg_str(ptr: *const u8, len: u64, _cap: i64) {
    // SAFETY: The caller guarantees ptr and len are valid
    // Note: capacity is not used for printing, but accepted for consistency
    // with the 3-tuple string representation
    let bytes = unsafe { core::slice::from_raw_parts(ptr, len as usize) };
    platform::write_stdout(bytes);
    platform::write_stdout(b"\n");
}

/// String equality comparison.
///
/// Called by the `==` operator on String types. Compares two strings
/// represented as 3-tuples (pointer, length, capacity).
///
/// # ABI
///
/// ```text
/// extern "C" fn __rue_str_eq(ptr1: *const u8, len1: u64, cap1: i64, ptr2: *const u8, len2: u64, cap2: i64) -> u8
/// ```
///
/// - `ptr1` is passed in the first argument register (rdi on x86_64, x0 on aarch64)
/// - `len1` is passed in the second argument register (rsi on x86_64, x1 on aarch64)
/// - `cap1` is passed in the third argument register (rdx on x86_64, x2 on aarch64)
/// - `ptr2` is passed in the fourth argument register (rcx on x86_64, x3 on aarch64)
/// - `len2` is passed in the fifth argument register (r8 on x86_64, x4 on aarch64)
/// - `cap2` is passed in the sixth argument register (r9 on x86_64, x5 on aarch64)
/// - Returns 1 if strings are equal, 0 otherwise (in `al`/`w0` register)
///
/// # Implementation
///
/// The capacity fields are not used for comparison - equality only depends on
/// the actual content (ptr + len). They are included for consistency with other
/// string operations that receive the full 3-tuple representation.
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
pub extern "C" fn __rue_str_eq(
    ptr1: *const u8,
    len1: u64,
    _cap1: i64,
    ptr2: *const u8,
    len2: u64,
    _cap2: i64,
) -> u8 {
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
pub extern "C" fn __rue_str_eq(
    ptr1: *const u8,
    len1: u64,
    _cap1: i64,
    ptr2: *const u8,
    len2: u64,
    _cap2: i64,
) -> u8 {
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

// ============================================================================
// String Memory Management
// ============================================================================
//
// Strings in Rue use a 3-tuple representation: (ptr, len, capacity)
//
// - capacity >= 0: heap-allocated string, can be mutated and freed
// - capacity == -1: rodata string literal, immutable, must not be freed
//
// When a rodata string is mutated, it is "promoted" to the heap by copying
// its contents to a new heap allocation.

/// Sentinel value for rodata string capacity.
/// Rodata strings have capacity = -1 to indicate they must not be freed.
#[allow(dead_code)]
const RODATA_CAPACITY: i64 = -1;

/// Copy bytes from src to dst without relying on compiler builtins.
///
/// This function performs a byte-by-byte copy to avoid LLVM lowering the copy
/// to a `memcpy` call, which would be undefined in our `#![no_std]` environment.
///
/// # Safety
///
/// The caller must ensure:
/// - `src` and `dst` do not overlap
/// - Both pointers are valid for `len` bytes
/// - Both pointers are properly aligned (byte alignment is always satisfied)
#[inline(always)]
unsafe fn copy_bytes(src: *const u8, dst: *mut u8, len: usize) {
    for i in 0..len {
        *dst.add(i) = *src.add(i);
    }
}

/// Drop (deallocate) a string.
///
/// Called when a string goes out of scope. If the string is heap-allocated
/// (capacity >= 0), frees the memory. Rodata strings (capacity == -1) are
/// not freed.
///
/// # ABI
///
/// ```text
/// extern "C" fn __rue_string_drop(ptr: *mut u8, len: u64, cap: i64)
/// ```
///
/// # Safety
///
/// The caller must ensure:
/// - If cap >= 0, ptr was allocated by the runtime allocator
/// - The string is not used after this call
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
#[unsafe(no_mangle)]
pub extern "C" fn __rue_string_drop(ptr: *mut u8, _len: u64, cap: i64) {
    if cap >= 0 {
        platform::dealloc(ptr, cap as u64);
    }
    // Rodata strings (cap == -1) are not freed
}

#[cfg(all(target_arch = "aarch64", target_os = "macos"))]
#[unsafe(no_mangle)]
pub extern "C" fn __rue_string_drop(ptr: *mut u8, _len: u64, cap: i64) {
    if cap >= 0 {
        platform::dealloc(ptr, cap as u64);
    }
}

/// Clone a string, creating a new heap-allocated copy.
///
/// Always creates a heap copy, regardless of whether the source is rodata
/// or heap-allocated. This implements copy semantics for strings.
///
/// # ABI
///
/// ```text
/// extern "C" fn __rue_string_clone(
///     src_ptr: *const u8,
///     src_len: u64,
///     src_cap: i64,
///     out_ptr: *mut *mut u8,
///     out_len: *mut u64,
///     out_cap: *mut i64
/// )
/// ```
///
/// The output is written to the three out parameters.
///
/// # Safety
///
/// The caller must ensure:
/// - src_ptr points to valid memory of at least src_len bytes
/// - out_ptr, out_len, out_cap are valid pointers
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
#[unsafe(no_mangle)]
pub extern "C" fn __rue_string_clone(
    src_ptr: *const u8,
    src_len: u64,
    _src_cap: i64,
    out_ptr: *mut *mut u8,
    out_len: *mut u64,
    out_cap: *mut i64,
) {
    if src_len == 0 {
        // Empty string: return null ptr with zero len/cap
        unsafe {
            *out_ptr = core::ptr::null_mut();
            *out_len = 0;
            *out_cap = 0;
        }
        return;
    }

    // Allocate new buffer with exact size (no extra capacity for clones)
    let new_ptr = platform::alloc(src_len);
    if new_ptr.is_null() {
        // Allocation failed - panic
        platform::write_stderr(b"error: out of memory\n");
        platform::exit(101);
    }

    // Copy the data
    // SAFETY: src_ptr is valid for src_len bytes, new_ptr is a fresh allocation
    // of src_len bytes, so they don't overlap.
    unsafe {
        copy_bytes(src_ptr, new_ptr, src_len as usize);
        *out_ptr = new_ptr;
        *out_len = src_len;
        *out_cap = src_len as i64;
    }
}

#[cfg(all(target_arch = "aarch64", target_os = "macos"))]
#[unsafe(no_mangle)]
pub extern "C" fn __rue_string_clone(
    src_ptr: *const u8,
    src_len: u64,
    _src_cap: i64,
    out_ptr: *mut *mut u8,
    out_len: *mut u64,
    out_cap: *mut i64,
) {
    if src_len == 0 {
        unsafe {
            *out_ptr = core::ptr::null_mut();
            *out_len = 0;
            *out_cap = 0;
        }
        return;
    }

    let new_ptr = platform::alloc(src_len);
    if new_ptr.is_null() {
        platform::write_stderr(b"error: out of memory\n");
        platform::exit(101);
    }

    // SAFETY: src_ptr is valid for src_len bytes, new_ptr is a fresh allocation
    // of src_len bytes, so they don't overlap.
    unsafe {
        copy_bytes(src_ptr, new_ptr, src_len as usize);
        *out_ptr = new_ptr;
        *out_len = src_len;
        *out_cap = src_len as i64;
    }
}

/// Concatenate two strings, returning a new heap-allocated string.
///
/// # ABI
///
/// ```text
/// extern "C" fn __rue_string_concat(
///     ptr1: *const u8, len1: u64, cap1: i64,
///     ptr2: *const u8, len2: u64, cap2: i64,
///     out_ptr: *mut *mut u8, out_len: *mut u64, out_cap: *mut i64
/// )
/// ```
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
#[unsafe(no_mangle)]
pub extern "C" fn __rue_string_concat(
    ptr1: *const u8,
    len1: u64,
    _cap1: i64,
    ptr2: *const u8,
    len2: u64,
    _cap2: i64,
    out_ptr: *mut *mut u8,
    out_len: *mut u64,
    out_cap: *mut i64,
) {
    // Use checked addition to detect overflow
    let total_len = match len1.checked_add(len2) {
        Some(len) => len,
        None => {
            platform::write_stderr(b"error: string too large\n");
            platform::exit(101);
        }
    };

    if total_len == 0 {
        unsafe {
            *out_ptr = core::ptr::null_mut();
            *out_len = 0;
            *out_cap = 0;
        }
        return;
    }

    // Allocate with some extra capacity for future growth (1.5x, min 16 bytes)
    // Use saturating arithmetic for capacity calculation to avoid overflow
    let new_cap = if total_len < 16 {
        16
    } else {
        total_len.saturating_add(total_len / 2)
    };
    let new_ptr = platform::alloc(new_cap);
    if new_ptr.is_null() {
        platform::write_stderr(b"error: out of memory\n");
        platform::exit(101);
    }

    // Copy both strings
    // SAFETY: ptr1/ptr2 are valid for their respective lengths, new_ptr is a
    // fresh allocation, so there's no overlap.
    unsafe {
        if len1 > 0 {
            copy_bytes(ptr1, new_ptr, len1 as usize);
        }
        if len2 > 0 {
            copy_bytes(ptr2, new_ptr.add(len1 as usize), len2 as usize);
        }
        *out_ptr = new_ptr;
        *out_len = total_len;
        *out_cap = new_cap as i64;
    }
}

#[cfg(all(target_arch = "aarch64", target_os = "macos"))]
#[unsafe(no_mangle)]
pub extern "C" fn __rue_string_concat(
    ptr1: *const u8,
    len1: u64,
    _cap1: i64,
    ptr2: *const u8,
    len2: u64,
    _cap2: i64,
    out_ptr: *mut *mut u8,
    out_len: *mut u64,
    out_cap: *mut i64,
) {
    // Use checked addition to detect overflow
    let total_len = match len1.checked_add(len2) {
        Some(len) => len,
        None => {
            platform::write_stderr(b"error: string too large\n");
            platform::exit(101);
        }
    };

    if total_len == 0 {
        unsafe {
            *out_ptr = core::ptr::null_mut();
            *out_len = 0;
            *out_cap = 0;
        }
        return;
    }

    // Allocate with some extra capacity for future growth (1.5x, min 16 bytes)
    // Use saturating arithmetic for capacity calculation to avoid overflow
    let new_cap = if total_len < 16 {
        16
    } else {
        total_len.saturating_add(total_len / 2)
    };
    let new_ptr = platform::alloc(new_cap);
    if new_ptr.is_null() {
        platform::write_stderr(b"error: out of memory\n");
        platform::exit(101);
    }

    // SAFETY: ptr1/ptr2 are valid for their respective lengths, new_ptr is a
    // fresh allocation, so there's no overlap.
    unsafe {
        if len1 > 0 {
            copy_bytes(ptr1, new_ptr, len1 as usize);
        }
        if len2 > 0 {
            copy_bytes(ptr2, new_ptr.add(len1 as usize), len2 as usize);
        }
        *out_ptr = new_ptr;
        *out_len = total_len;
        *out_cap = new_cap as i64;
    }
}

// Re-export platform functions for tests
#[cfg(all(test, target_arch = "x86_64", target_os = "linux"))]
pub use x86_64_linux::{exit, write, write_all, write_stderr};

#[cfg(all(test, target_arch = "aarch64", target_os = "macos"))]
pub use aarch64_macos::{exit, write, write_all, write_stderr};

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
}
