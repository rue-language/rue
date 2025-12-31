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
macro_rules! define_for_all_platforms {
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
        // SAFETY: Caller guarantees dst and src are valid for n bytes and don't overlap
        unsafe { *dst.add(i) = *src.add(i) };
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
            // SAFETY: Caller guarantees dst and src are valid for n bytes
            unsafe { *dst.add(i) = *src.add(i) };
            i += 1;
        }
    } else {
        // Copy backwards to handle overlap
        let mut i = n;
        while i > 0 {
            i -= 1;
            // SAFETY: Caller guarantees dst and src are valid for n bytes
            unsafe { *dst.add(i) = *src.add(i) };
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
        // SAFETY: Caller guarantees dst is valid for n bytes
        unsafe { *dst.add(i) = byte };
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
        // SAFETY: Caller guarantees s1 and s2 are valid for n bytes
        let a = unsafe { *s1.add(i) };
        let b = unsafe { *s2.add(i) };
        if a != b {
            return (a as i32) - (b as i32);
        }
        i += 1;
    }
    0
}

/// Compare `n` bytes of memory at `s1` and `s2` for equality.
///
/// Returns 0 if equal, non-zero if different.
///
/// This is a simplified version of `memcmp` that only tests for equality,
/// not ordering. Some compilers (including rustc/LLVM) may generate calls
/// to `bcmp` for slice equality comparisons in no_std environments.
///
/// # Safety
///
/// - `s1` must be valid for reads of `n` bytes
/// - `s2` must be valid for reads of `n` bytes
#[unsafe(no_mangle)]
pub unsafe extern "C" fn bcmp(s1: *const u8, s2: *const u8, n: usize) -> i32 {
    let mut i = 0;
    while i < n {
        // SAFETY: Caller guarantees s1 and s2 are valid for n bytes
        let a = unsafe { *s1.add(i) };
        let b = unsafe { *s2.add(i) };
        if a != b {
            return 1;
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

define_for_all_platforms! {
    pub extern "C" fn __rue_exit(status: i32) -> ! {
        platform::exit(status)
    }
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
define_for_all_platforms! {
    pub extern "C" fn __rue_div_by_zero() -> ! {
        platform::write_stderr(b"error: division by zero\n");
        platform::exit(101)
    }
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
define_for_all_platforms! {
    pub extern "C" fn __rue_overflow() -> ! {
        platform::write_stderr(b"error: integer overflow\n");
        platform::exit(101)
    }
}

/// Runtime error: integer cast overflow.
///
/// Called when `@intCast` would produce a value that cannot be represented
/// in the target type. For example, casting `-1i32` to `u8` or `256u32` to `u8`.
///
/// # Behavior
///
/// 1. Writes `"error: integer cast overflow\n"` to stderr (best-effort)
/// 2. Exits with code 101
///
/// # ABI
///
/// ```text
/// extern "C" fn __rue_intcast_overflow() -> !
/// ```
///
/// No arguments. Never returns.
define_for_all_platforms! {
    pub extern "C" fn __rue_intcast_overflow() -> ! {
        platform::write_stderr(b"error: integer cast overflow\n");
        platform::exit(101)
    }
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
define_for_all_platforms! {
    pub extern "C" fn __rue_bounds_check() -> ! {
        platform::write_stderr(b"error: index out of bounds\n");
        platform::exit(101)
    }
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
define_for_all_platforms! {
    pub extern "C" fn __rue_dbg_i64(value: i64) {
        platform::print_i64(value);
    }
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
define_for_all_platforms! {
    pub extern "C" fn __rue_dbg_u64(value: u64) {
        platform::print_u64(value);
    }
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
define_for_all_platforms! {
    pub extern "C" fn __rue_dbg_bool(value: i64) {
        platform::print_bool(value != 0);
    }
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
define_for_all_platforms! {
    pub extern "C" fn __rue_dbg_str(ptr: *const u8, len: u64) {
        // SAFETY: The caller guarantees ptr and len are valid
        let bytes = unsafe { core::slice::from_raw_parts(ptr, len as usize) };
        platform::write_stdout(bytes);
        platform::write_stdout(b"\n");
    }
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
define_for_all_platforms! {
    pub extern "C" fn __rue_str_eq(ptr1: *const u8, len1: u64, ptr2: *const u8, len2: u64) -> u8 {
        // Fast path 1: different lengths means not equal
        if len1 != len2 {
            return 0;
        }

        // Fast path 2: pointer equality - if both point to same memory with same length,
        // they're equal. This is especially useful for comparing string literals to themselves
        // since they point to the same rodata location.
        if ptr1 == ptr2 {
            return 1;
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
define_for_all_platforms! {
    pub extern "C" fn __rue_alloc(size: u64, align: u64) -> *mut u8 {
        heap::alloc(size, align)
    }
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
define_for_all_platforms! {
    pub extern "C" fn __rue_free(ptr: *mut u8, size: u64, align: u64) {
        heap::free(ptr, size, align)
    }
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
define_for_all_platforms! {
    pub extern "C" fn __rue_realloc(ptr: *mut u8, old_size: u64, new_size: u64, align: u64) -> *mut u8 {
        heap::realloc(ptr, old_size, new_size, align)
    }
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
define_for_all_platforms! {
    pub extern "C" fn __rue_string_alloc(cap: u64) -> *mut u8 {
        let actual_cap = if cap < STRING_MIN_CAPACITY {
            STRING_MIN_CAPACITY
        } else {
            cap
        };
        heap::alloc(actual_cap, 1) // Strings are byte-aligned
    }
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
define_for_all_platforms! {
    pub extern "C" fn __rue_string_realloc(ptr: *mut u8, old_cap: u64, new_cap: u64) -> *mut u8 {
        // Calculate actual new capacity with growth strategy
        let grown_cap = old_cap.saturating_mul(2);
        let actual_cap = new_cap.max(grown_cap).max(STRING_MIN_CAPACITY);

        // Use the general realloc, which handles null ptr and copying
        heap::realloc(ptr, old_cap, actual_cap, 1)
    }
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
define_for_all_platforms! {
    pub extern "C" fn __rue_string_clone(ptr: *const u8, len: u64) -> *mut u8 {
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
define_for_all_platforms! {
    pub extern "C" fn __rue_drop_String(ptr: *mut u8, _len: u64, cap: u64) {
        // Only free heap-allocated strings (cap > 0)
        // Rodata strings have cap == 0 and must not be freed
        if cap > 0 {
            heap::free(ptr, cap, 1);
        }
    }
}

// =========================================================================
// String Construction Functions
// =========================================================================

/// Create an empty String with no allocation.
///
/// Returns an empty String (ptr=null, len=0, cap=0). This represents an empty
/// string that points to no data. Any mutation will trigger heap allocation.
///
/// Since we want to return 3 values in registers (to match Rue's multi-value
/// return convention), we return 3 separate u64 values:
/// - ptr (first return value)
/// - len (second return value)
/// - cap (third return value)
///
/// This approach avoids struct ABI differences where large structs (>16 bytes
/// on ARM64) would be returned via pointer.
///
/// # ABI (sret convention)
///
/// ```text
/// extern "C" fn String__new(out: *mut StringResult)
/// ```
///
/// Caller allocates space for the return value and passes pointer.
/// Callee writes (ptr=0, len=0, cap=0) to that pointer.
///
/// This avoids multi-register return complexity across platforms.

/// The StringResult struct used for sret (struct return) convention.
/// Caller allocates this on stack, passes pointer to callee.
#[repr(C)]
pub struct StringResult {
    pub ptr: *mut u8,
    pub len: u64,
    pub cap: u64,
}

#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
#[unsafe(no_mangle)]
#[allow(non_snake_case)]
pub extern "C" fn String__new(out: *mut StringResult) {
    unsafe {
        (*out).ptr = core::ptr::null_mut();
        (*out).len = 0;
        (*out).cap = 0;
    }
}

#[cfg(all(target_arch = "aarch64", target_os = "macos"))]
#[unsafe(no_mangle)]
#[allow(non_snake_case)]
pub extern "C" fn String__new(out: *mut StringResult) {
    unsafe {
        (*out).ptr = core::ptr::null_mut();
        (*out).len = 0;
        (*out).cap = 0;
    }
}

#[cfg(all(target_arch = "aarch64", target_os = "linux"))]
#[unsafe(no_mangle)]
#[allow(non_snake_case)]
pub extern "C" fn String__new(out: *mut StringResult) {
    unsafe {
        (*out).ptr = core::ptr::null_mut();
        (*out).len = 0;
        (*out).cap = 0;
    }
}

/// Create an empty String with pre-allocated capacity.
///
/// Allocates a heap buffer with the given capacity (at least STRING_MIN_CAPACITY).
/// Returns a String with len=0 but capacity available for appending.
///
/// # Arguments
///
/// * `out` - Pointer to StringResult where result will be written
/// * `requested_cap` - Desired capacity in bytes (will be at least STRING_MIN_CAPACITY)
///
/// # ABI (sret convention)
///
/// ```text
/// extern "C" fn String__with_capacity(out: *mut StringResult, cap: u64)
/// ```
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
#[unsafe(no_mangle)]
#[allow(non_snake_case)]
pub extern "C" fn String__with_capacity(out: *mut StringResult, requested_cap: u64) {
    let actual_cap = if requested_cap < STRING_MIN_CAPACITY {
        STRING_MIN_CAPACITY
    } else {
        requested_cap
    };
    let ptr = heap::alloc(actual_cap, 1);
    unsafe {
        (*out).ptr = ptr;
        (*out).len = 0;
        (*out).cap = actual_cap;
    }
}

#[cfg(all(target_arch = "aarch64", target_os = "macos"))]
#[unsafe(no_mangle)]
#[allow(non_snake_case)]
pub extern "C" fn String__with_capacity(out: *mut StringResult, requested_cap: u64) {
    let actual_cap = if requested_cap < STRING_MIN_CAPACITY {
        STRING_MIN_CAPACITY
    } else {
        requested_cap
    };
    let ptr = heap::alloc(actual_cap, 1);
    unsafe {
        (*out).ptr = ptr;
        (*out).len = 0;
        (*out).cap = actual_cap;
    }
}

#[cfg(all(target_arch = "aarch64", target_os = "linux"))]
#[unsafe(no_mangle)]
#[allow(non_snake_case)]
pub extern "C" fn String__with_capacity(out: *mut StringResult, requested_cap: u64) {
    let actual_cap = if requested_cap < STRING_MIN_CAPACITY {
        STRING_MIN_CAPACITY
    } else {
        requested_cap
    };
    let ptr = heap::alloc(actual_cap, 1);
    unsafe {
        (*out).ptr = ptr;
        (*out).len = 0;
        (*out).cap = actual_cap;
    }
}

// =============================================================================
// String Query Methods (Phase 6: len, capacity, is_empty)
// =============================================================================
//
// These methods take a String (ptr, len, cap) and return a single value.
// They use `borrow self` semantics - the String is not consumed.
//
// ABI: String is passed as 3 separate arguments (ptr, len, cap)
// - x86-64: ptr in rdi, len in rsi, cap in rdx
// - aarch64: ptr in x0, len in x1, cap in x2
//
// Return value is in rax (x86-64) or x0 (aarch64)

/// Get the length of a String in bytes.
///
/// # Arguments
/// * `_ptr` - Pointer to string data (unused, but part of ABI)
/// * `len` - Length in bytes
/// * `_cap` - Capacity (unused, but part of ABI)
///
/// # Returns
/// The length in bytes (u64)
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
#[unsafe(no_mangle)]
#[allow(non_snake_case)]
pub extern "C" fn String__len(_ptr: *const u8, len: u64, _cap: u64) -> u64 {
    len
}

#[cfg(all(target_arch = "aarch64", target_os = "macos"))]
#[unsafe(no_mangle)]
#[allow(non_snake_case)]
pub extern "C" fn String__len(_ptr: *const u8, len: u64, _cap: u64) -> u64 {
    len
}

#[cfg(all(target_arch = "aarch64", target_os = "linux"))]
#[unsafe(no_mangle)]
#[allow(non_snake_case)]
pub extern "C" fn String__len(_ptr: *const u8, len: u64, _cap: u64) -> u64 {
    len
}

/// Get the capacity of a String in bytes.
///
/// Returns 0 for string literals (pointing to rodata).
/// Returns the allocated heap capacity for mutable strings.
///
/// # Arguments
/// * `_ptr` - Pointer to string data (unused, but part of ABI)
/// * `_len` - Length (unused, but part of ABI)
/// * `cap` - Capacity in bytes
///
/// # Returns
/// The capacity in bytes (u64)
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
#[unsafe(no_mangle)]
#[allow(non_snake_case)]
pub extern "C" fn String__capacity(_ptr: *const u8, _len: u64, cap: u64) -> u64 {
    cap
}

#[cfg(all(target_arch = "aarch64", target_os = "macos"))]
#[unsafe(no_mangle)]
#[allow(non_snake_case)]
pub extern "C" fn String__capacity(_ptr: *const u8, _len: u64, cap: u64) -> u64 {
    cap
}

#[cfg(all(target_arch = "aarch64", target_os = "linux"))]
#[unsafe(no_mangle)]
#[allow(non_snake_case)]
pub extern "C" fn String__capacity(_ptr: *const u8, _len: u64, cap: u64) -> u64 {
    cap
}

/// Check if a String is empty.
///
/// # Arguments
/// * `_ptr` - Pointer to string data (unused, but part of ABI)
/// * `len` - Length in bytes
/// * `_cap` - Capacity (unused, but part of ABI)
///
/// # Returns
/// 1 (true) if len == 0, 0 (false) otherwise
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
#[unsafe(no_mangle)]
#[allow(non_snake_case)]
pub extern "C" fn String__is_empty(_ptr: *const u8, len: u64, _cap: u64) -> u8 {
    if len == 0 { 1 } else { 0 }
}

#[cfg(all(target_arch = "aarch64", target_os = "macos"))]
#[unsafe(no_mangle)]
#[allow(non_snake_case)]
pub extern "C" fn String__is_empty(_ptr: *const u8, len: u64, _cap: u64) -> u8 {
    if len == 0 { 1 } else { 0 }
}

#[cfg(all(target_arch = "aarch64", target_os = "linux"))]
#[unsafe(no_mangle)]
#[allow(non_snake_case)]
pub extern "C" fn String__is_empty(_ptr: *const u8, len: u64, _cap: u64) -> u8 {
    if len == 0 { 1 } else { 0 }
}

// =============================================================================
// String Clone Method (Phase 8)
// =============================================================================
//
// Clone creates a deep copy of a String. It uses `borrow self` semantics -
// the original String is not consumed.
//
// ABI (sret convention): out pointer first, then String fields (ptr, len, cap)

/// Clone a String, creating a deep copy.
///
/// # Arguments
///
/// * `out` - Pointer to StringResult where result will be written
/// * `ptr` - Pointer to the source string data
/// * `len` - Length of the string in bytes
/// * `_cap` - Capacity (unused for cloning, but part of ABI)
///
/// # Behavior
///
/// Always allocates a new heap buffer, even for literals (cap == 0).
/// The clone is always heap-allocated so it can be mutated.
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
#[unsafe(no_mangle)]
#[allow(non_snake_case)]
pub extern "C" fn String__clone(out: *mut StringResult, ptr: *const u8, len: u64, _cap: u64) {
    let new_cap = len.max(STRING_MIN_CAPACITY);
    let new_ptr = heap::alloc(new_cap, 1);

    // Check for allocation failure before copy to avoid UB
    if new_ptr.is_null() {
        unsafe {
            (*out).ptr = core::ptr::null_mut();
            (*out).len = 0;
            (*out).cap = 0;
        }
        return;
    }

    if len > 0 && !ptr.is_null() {
        unsafe {
            core::ptr::copy_nonoverlapping(ptr, new_ptr, len as usize);
        }
    }

    unsafe {
        (*out).ptr = new_ptr;
        (*out).len = len;
        (*out).cap = new_cap;
    }
}

#[cfg(all(target_arch = "aarch64", target_os = "macos"))]
#[unsafe(no_mangle)]
#[allow(non_snake_case)]
pub extern "C" fn String__clone(out: *mut StringResult, ptr: *const u8, len: u64, _cap: u64) {
    let new_cap = len.max(STRING_MIN_CAPACITY);
    let new_ptr = heap::alloc(new_cap, 1);

    // Check for allocation failure before copy to avoid UB
    if new_ptr.is_null() {
        unsafe {
            (*out).ptr = core::ptr::null_mut();
            (*out).len = 0;
            (*out).cap = 0;
        }
        return;
    }

    if len > 0 && !ptr.is_null() {
        unsafe {
            core::ptr::copy_nonoverlapping(ptr, new_ptr, len as usize);
        }
    }

    unsafe {
        (*out).ptr = new_ptr;
        (*out).len = len;
        (*out).cap = new_cap;
    }
}

#[cfg(all(target_arch = "aarch64", target_os = "linux"))]
#[unsafe(no_mangle)]
#[allow(non_snake_case)]
pub extern "C" fn String__clone(out: *mut StringResult, ptr: *const u8, len: u64, _cap: u64) {
    let new_cap = len.max(STRING_MIN_CAPACITY);
    let new_ptr = heap::alloc(new_cap, 1);

    // Check for allocation failure before copy to avoid UB
    if new_ptr.is_null() {
        unsafe {
            (*out).ptr = core::ptr::null_mut();
            (*out).len = 0;
            (*out).cap = 0;
        }
        return;
    }

    if len > 0 && !ptr.is_null() {
        unsafe {
            core::ptr::copy_nonoverlapping(ptr, new_ptr, len as usize);
        }
    }

    unsafe {
        (*out).ptr = new_ptr;
        (*out).len = len;
        (*out).cap = new_cap;
    }
}

// =============================================================================
// String Mutation Methods (Phase 7: push_str, push, clear, reserve)
// =============================================================================
//
// These methods take a String (ptr, len, cap) and additional arguments,
// then return an updated String (ptr, len, cap) via sret convention.
// They use `inout self` semantics - the String is modified in place.
//
// ABI (sret convention): out pointer first, then String fields and other args
//
// Heap promotion: If cap == 0, the string is a literal pointing to rodata.
// Any mutation first promotes to heap by allocating a new buffer and copying.

/// Append another string's content to this string.
///
/// # Arguments
///
/// * `out` - Pointer to StringResult where result will be written
/// * `ptr` - Pointer to the string data
/// * `len` - Current length in bytes
/// * `cap` - Current capacity (0 for literals)
/// * `other_ptr` - Pointer to the other string's data
/// * `other_len` - Length of the other string
/// * `_other_cap` - Capacity of the other string (unused, but part of ABI)
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
#[unsafe(no_mangle)]
#[allow(non_snake_case)]
pub extern "C" fn String__push_str(
    out: *mut StringResult,
    ptr: *mut u8,
    len: u64,
    cap: u64,
    other_ptr: *const u8,
    other_len: u64,
    _other_cap: u64,
) {
    let (new_ptr, new_cap) = string_ensure_capacity(ptr, len, cap, other_len);

    if other_len > 0 && !other_ptr.is_null() {
        unsafe {
            core::ptr::copy_nonoverlapping(
                other_ptr,
                new_ptr.add(len as usize),
                other_len as usize,
            );
        }
    }

    let new_len = len + other_len;

    unsafe {
        (*out).ptr = new_ptr;
        (*out).len = new_len;
        (*out).cap = new_cap;
    }
}

#[cfg(all(target_arch = "aarch64", target_os = "macos"))]
#[unsafe(no_mangle)]
#[allow(non_snake_case)]
pub extern "C" fn String__push_str(
    out: *mut StringResult,
    ptr: *mut u8,
    len: u64,
    cap: u64,
    other_ptr: *const u8,
    other_len: u64,
    _other_cap: u64,
) {
    let (new_ptr, new_cap) = string_ensure_capacity(ptr, len, cap, other_len);

    if other_len > 0 && !other_ptr.is_null() {
        unsafe {
            core::ptr::copy_nonoverlapping(
                other_ptr,
                new_ptr.add(len as usize),
                other_len as usize,
            );
        }
    }

    let new_len = len + other_len;

    unsafe {
        (*out).ptr = new_ptr;
        (*out).len = new_len;
        (*out).cap = new_cap;
    }
}

#[cfg(all(target_arch = "aarch64", target_os = "linux"))]
#[unsafe(no_mangle)]
#[allow(non_snake_case)]
pub extern "C" fn String__push_str(
    out: *mut StringResult,
    ptr: *mut u8,
    len: u64,
    cap: u64,
    other_ptr: *const u8,
    other_len: u64,
    _other_cap: u64,
) {
    let (new_ptr, new_cap) = string_ensure_capacity(ptr, len, cap, other_len);

    if other_len > 0 && !other_ptr.is_null() {
        unsafe {
            core::ptr::copy_nonoverlapping(
                other_ptr,
                new_ptr.add(len as usize),
                other_len as usize,
            );
        }
    }

    let new_len = len + other_len;

    unsafe {
        (*out).ptr = new_ptr;
        (*out).len = new_len;
        (*out).cap = new_cap;
    }
}

/// Append a single byte to this string.
///
/// # Arguments
///
/// * `out` - Pointer to StringResult where result will be written
/// * `ptr` - Pointer to the string data
/// * `len` - Current length in bytes
/// * `cap` - Current capacity (0 for literals)
/// * `byte` - The byte to append
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
#[unsafe(no_mangle)]
#[allow(non_snake_case)]
pub extern "C" fn String__push(out: *mut StringResult, ptr: *mut u8, len: u64, cap: u64, byte: u8) {
    let (new_ptr, new_cap) = string_ensure_capacity(ptr, len, cap, 1);

    unsafe {
        *new_ptr.add(len as usize) = byte;
    }

    let new_len = len + 1;

    unsafe {
        (*out).ptr = new_ptr;
        (*out).len = new_len;
        (*out).cap = new_cap;
    }
}

#[cfg(all(target_arch = "aarch64", target_os = "macos"))]
#[unsafe(no_mangle)]
#[allow(non_snake_case)]
pub extern "C" fn String__push(out: *mut StringResult, ptr: *mut u8, len: u64, cap: u64, byte: u8) {
    let (new_ptr, new_cap) = string_ensure_capacity(ptr, len, cap, 1);

    unsafe {
        *new_ptr.add(len as usize) = byte;
    }

    let new_len = len + 1;

    unsafe {
        (*out).ptr = new_ptr;
        (*out).len = new_len;
        (*out).cap = new_cap;
    }
}

#[cfg(all(target_arch = "aarch64", target_os = "linux"))]
#[unsafe(no_mangle)]
#[allow(non_snake_case)]
pub extern "C" fn String__push(out: *mut StringResult, ptr: *mut u8, len: u64, cap: u64, byte: u8) {
    let (new_ptr, new_cap) = string_ensure_capacity(ptr, len, cap, 1);

    unsafe {
        *new_ptr.add(len as usize) = byte;
    }

    let new_len = len + 1;

    unsafe {
        (*out).ptr = new_ptr;
        (*out).len = new_len;
        (*out).cap = new_cap;
    }
}

/// Clear the string content, keeping capacity.
///
/// # Arguments
///
/// * `out` - Pointer to StringResult where result will be written
/// * `ptr` - Pointer to the string data
/// * `_len` - Current length in bytes (unused)
/// * `cap` - Current capacity
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
#[unsafe(no_mangle)]
#[allow(non_snake_case)]
pub extern "C" fn String__clear(out: *mut StringResult, ptr: *mut u8, _len: u64, cap: u64) {
    unsafe {
        (*out).ptr = ptr;
        (*out).len = 0;
        (*out).cap = cap;
    }
}

#[cfg(all(target_arch = "aarch64", target_os = "macos"))]
#[unsafe(no_mangle)]
#[allow(non_snake_case)]
pub extern "C" fn String__clear(out: *mut StringResult, ptr: *mut u8, _len: u64, cap: u64) {
    unsafe {
        (*out).ptr = ptr;
        (*out).len = 0;
        (*out).cap = cap;
    }
}

#[cfg(all(target_arch = "aarch64", target_os = "linux"))]
#[unsafe(no_mangle)]
#[allow(non_snake_case)]
pub extern "C" fn String__clear(out: *mut StringResult, ptr: *mut u8, _len: u64, cap: u64) {
    unsafe {
        (*out).ptr = ptr;
        (*out).len = 0;
        (*out).cap = cap;
    }
}

/// Reserve additional capacity in the string.
///
/// # Arguments
///
/// * `out` - Pointer to StringResult where result will be written
/// * `ptr` - Pointer to the string data
/// * `len` - Current length in bytes
/// * `cap` - Current capacity (0 for literals)
/// * `additional` - Number of additional bytes to reserve
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
#[unsafe(no_mangle)]
#[allow(non_snake_case)]
pub extern "C" fn String__reserve(
    out: *mut StringResult,
    ptr: *mut u8,
    len: u64,
    cap: u64,
    additional: u64,
) {
    let (new_ptr, new_cap) = string_ensure_capacity(ptr, len, cap, additional);

    unsafe {
        (*out).ptr = new_ptr;
        (*out).len = len; // len stays the same for reserve
        (*out).cap = new_cap;
    }
}

#[cfg(all(target_arch = "aarch64", target_os = "macos"))]
#[unsafe(no_mangle)]
#[allow(non_snake_case)]
pub extern "C" fn String__reserve(
    out: *mut StringResult,
    ptr: *mut u8,
    len: u64,
    cap: u64,
    additional: u64,
) {
    let (new_ptr, new_cap) = string_ensure_capacity(ptr, len, cap, additional);

    unsafe {
        (*out).ptr = new_ptr;
        (*out).len = len;
        (*out).cap = new_cap;
    }
}

#[cfg(all(target_arch = "aarch64", target_os = "linux"))]
#[unsafe(no_mangle)]
#[allow(non_snake_case)]
pub extern "C" fn String__reserve(
    out: *mut StringResult,
    ptr: *mut u8,
    len: u64,
    cap: u64,
    additional: u64,
) {
    let (new_ptr, new_cap) = string_ensure_capacity(ptr, len, cap, additional);

    unsafe {
        (*out).ptr = new_ptr;
        (*out).len = len;
        (*out).cap = new_cap;
    }
}

/// Helper function to ensure a string has enough capacity for additional bytes.
///
/// Handles heap promotion (cap == 0) and growth.
///
/// # Arguments
///
/// * `ptr` - Current pointer
/// * `len` - Current length
/// * `cap` - Current capacity (0 for literals)
/// * `additional` - Number of additional bytes needed
///
/// # Returns
///
/// (new_ptr, new_cap) with capacity >= len + additional.
/// Returns (null, 0) if allocation fails.
#[inline]
fn string_ensure_capacity(ptr: *mut u8, len: u64, cap: u64, additional: u64) -> (*mut u8, u64) {
    let required = len.saturating_add(additional);

    if cap == 0 {
        // Heap promotion: allocate new buffer and copy existing content
        let new_cap = required.max(STRING_MIN_CAPACITY);
        let new_ptr = heap::alloc(new_cap, 1);
        // Check for allocation failure before copy to avoid UB
        if new_ptr.is_null() {
            return (core::ptr::null_mut(), 0);
        }
        if len > 0 && !ptr.is_null() {
            unsafe {
                core::ptr::copy_nonoverlapping(ptr as *const u8, new_ptr, len as usize);
            }
        }
        (new_ptr, new_cap)
    } else if required > cap {
        // Need to grow: use the realloc function which implements growth strategy
        let new_ptr = heap::realloc(ptr, cap, required, 1);
        // Check for allocation failure
        if new_ptr.is_null() {
            return (core::ptr::null_mut(), 0);
        }
        // Calculate actual new capacity (realloc uses 2x growth strategy)
        let grown_cap = cap.saturating_mul(2);
        let new_cap = required.max(grown_cap).max(STRING_MIN_CAPACITY);
        (new_ptr, new_cap)
    } else {
        // Capacity is sufficient
        (ptr, cap)
    }
}

// =============================================================================
// Input Functions
// =============================================================================

/// Initial buffer size for reading lines.
/// This is a reasonable size for most interactive input.
const READ_LINE_INITIAL_CAPACITY: u64 = 128;

/// Read a line from standard input.
///
/// Reads bytes from stdin (file descriptor 0) until a newline character (`\n`)
/// is encountered or EOF is reached. Returns the line as a String (excluding
/// the trailing newline).
///
/// # Returns
///
/// Returns the string data via sret convention. Writes to `out`:
/// - ptr: Pointer to the string data (heap-allocated)
/// - len: Length of the string in bytes (excluding newline)
/// - cap: Capacity of the allocated buffer
///
/// # Panics
///
/// - If EOF is reached with no data read: panics with "unexpected end of input"
/// - If a read error occurs: panics with "input error"
///
/// # ABI (sret convention)
///
/// ```text
/// extern "C" fn __rue_read_line(out: *mut StringResult)
/// ```
///
/// Caller allocates space for the return value and passes pointer.
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
#[unsafe(no_mangle)]
#[allow(non_snake_case)]
pub extern "C" fn __rue_read_line(out: *mut StringResult) {
    read_line_impl(out);
}

#[cfg(all(target_arch = "aarch64", target_os = "macos"))]
#[unsafe(no_mangle)]
#[allow(non_snake_case)]
pub extern "C" fn __rue_read_line(out: *mut StringResult) {
    read_line_impl(out);
}

#[cfg(all(target_arch = "aarch64", target_os = "linux"))]
#[unsafe(no_mangle)]
#[allow(non_snake_case)]
pub extern "C" fn __rue_read_line(out: *mut StringResult) {
    read_line_impl(out);
}

/// Implementation of read_line shared across platforms.
///
/// This function reads from stdin byte-by-byte until:
/// - A newline character is found (returns line without the newline)
/// - EOF is reached with some data (returns partial line)
/// - EOF is reached with no data (panics)
/// - A read error occurs (panics)
#[inline]
fn read_line_impl(out: *mut StringResult) {
    // Allocate initial buffer
    let mut cap = READ_LINE_INITIAL_CAPACITY;
    let mut ptr = heap::alloc(cap, 1);
    if ptr.is_null() {
        // Allocation failed - panic
        platform::write_stderr(b"error: out of memory\n");
        platform::exit(101);
    }

    let mut len: u64 = 0;
    let mut byte_buf = [0u8; 1];

    loop {
        // Read one byte at a time
        let result = platform::read(platform::STDIN, byte_buf.as_mut_ptr(), 1);

        if result < 0 {
            // Read error - free buffer and panic
            heap::free(ptr, cap, 1);
            platform::write_stderr(b"error: input error\n");
            platform::exit(101);
        }

        if result == 0 {
            // EOF reached
            if len == 0 {
                // EOF with no data - free buffer and panic
                heap::free(ptr, cap, 1);
                platform::write_stderr(b"error: unexpected end of input\n");
                platform::exit(101);
            }
            // EOF with data - return partial line
            break;
        }

        // Got a byte
        let byte = byte_buf[0];

        // Check for newline - line is complete (don't include the newline)
        if byte == b'\n' {
            break;
        }

        // Need to store this byte - ensure we have capacity
        if len >= cap {
            // Grow the buffer (2x strategy)
            let new_cap = cap.saturating_mul(2).max(STRING_MIN_CAPACITY);
            let new_ptr = heap::realloc(ptr, cap, new_cap, 1);
            if new_ptr.is_null() {
                // Realloc failed - free old buffer and panic
                heap::free(ptr, cap, 1);
                platform::write_stderr(b"error: out of memory\n");
                platform::exit(101);
            }
            ptr = new_ptr;
            cap = new_cap;
        }

        // Store the byte
        unsafe {
            *ptr.add(len as usize) = byte;
        }
        len += 1;
    }

    // Return the string
    unsafe {
        (*out).ptr = ptr;
        (*out).len = len;
        (*out).cap = cap;
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
    // Memory Intrinsic Tests
    // =========================================================================

    #[test]
    fn test_bcmp_equal() {
        let a = b"hello world";
        let b = b"hello world";
        let result = unsafe { super::bcmp(a.as_ptr(), b.as_ptr(), a.len()) };
        assert_eq!(result, 0);
    }

    #[test]
    fn test_bcmp_not_equal() {
        let a = b"hello world";
        let b = b"hello xorld";
        let result = unsafe { super::bcmp(a.as_ptr(), b.as_ptr(), a.len()) };
        assert_ne!(result, 0);
    }

    #[test]
    fn test_bcmp_empty() {
        let a = b"";
        let b = b"";
        let result = unsafe { super::bcmp(a.as_ptr(), b.as_ptr(), 0) };
        assert_eq!(result, 0);
    }

    #[test]
    fn test_bcmp_first_byte_differs() {
        let a = b"abc";
        let b = b"xbc";
        let result = unsafe { super::bcmp(a.as_ptr(), b.as_ptr(), a.len()) };
        assert_ne!(result, 0);
    }

    #[test]
    fn test_bcmp_last_byte_differs() {
        let a = b"abc";
        let b = b"abx";
        let result = unsafe { super::bcmp(a.as_ptr(), b.as_ptr(), a.len()) };
        assert_ne!(result, 0);
    }

    #[test]
    fn test_bcmp_partial_comparison() {
        // Compare only first 3 bytes - they're the same
        let a = b"abcdef";
        let b = b"abcxyz";
        let result = unsafe { super::bcmp(a.as_ptr(), b.as_ptr(), 3) };
        assert_eq!(result, 0);
    }

    #[test]
    fn test_bcmp_single_byte_equal() {
        let a = [42u8];
        let b = [42u8];
        let result = unsafe { super::bcmp(a.as_ptr(), b.as_ptr(), 1) };
        assert_eq!(result, 0);
    }

    #[test]
    fn test_bcmp_single_byte_differs() {
        let a = [42u8];
        let b = [43u8];
        let result = unsafe { super::bcmp(a.as_ptr(), b.as_ptr(), 1) };
        assert_ne!(result, 0);
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

    // =========================================================================
    // String Equality Tests
    // =========================================================================

    #[test]
    fn test_str_eq_same_content() {
        // Use arrays on the stack to guarantee different pointers
        // (byte literals may be deduplicated by the compiler)
        let s1: [u8; 5] = *b"hello";
        let s2: [u8; 5] = *b"hello";
        // Different pointers, same content
        assert_ne!(s1.as_ptr(), s2.as_ptr());
        let result = super::__rue_str_eq(s1.as_ptr(), 5, s2.as_ptr(), 5);
        assert_eq!(result, 1);
    }

    #[test]
    fn test_str_eq_different_content() {
        let s1 = b"hello";
        let s2 = b"world";
        let result = super::__rue_str_eq(s1.as_ptr(), 5, s2.as_ptr(), 5);
        assert_eq!(result, 0);
    }

    #[test]
    fn test_str_eq_different_lengths() {
        let s1 = b"hello";
        let s2 = b"hi";
        let result = super::__rue_str_eq(s1.as_ptr(), 5, s2.as_ptr(), 2);
        assert_eq!(result, 0);
    }

    #[test]
    fn test_str_eq_pointer_equality_fast_path() {
        // Same pointer and length should use fast path and return true
        let s = b"hello";
        let result = super::__rue_str_eq(s.as_ptr(), 5, s.as_ptr(), 5);
        assert_eq!(result, 1);
    }

    #[test]
    fn test_str_eq_empty_strings() {
        // Two empty strings with different (null) pointers
        let result = super::__rue_str_eq(ptr::null(), 0, ptr::null(), 0);
        assert_eq!(result, 1);
    }

    #[test]
    fn test_str_eq_empty_vs_non_empty() {
        let s = b"hello";
        let result = super::__rue_str_eq(ptr::null(), 0, s.as_ptr(), 5);
        assert_eq!(result, 0);
    }

    #[test]
    fn test_str_eq_prefix() {
        // "hello" vs "hell" - should be not equal
        let s1 = b"hello";
        let s2 = b"hell";
        let result = super::__rue_str_eq(s1.as_ptr(), 5, s2.as_ptr(), 4);
        assert_eq!(result, 0);
    }
}
