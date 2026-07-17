//! Captured process arguments and environment (RUE-935).
//!
//! `argc`/`argv`/`envp` exist only in the register/stack state the platform
//! loader hands the program at entry, so the per-target entry code captures
//! them here *before* `main` runs. The `__rue_arg_*` / `__rue_env_*` accessors
//! expose the capture across the versioned runtime ABI; `std.env` assembles
//! owned `StrBuf` copies on top of them.
//!
//! The vectors the loader supplies live above the initial stack and remain
//! valid for the whole process, so storing the raw pointers is sound. Reads
//! happen only after `capture` runs and Rue has no threads yet, so `Relaxed`
//! ordering is sufficient; the atomics exist purely to avoid `static mut`.

use core::sync::atomic::{AtomicPtr, AtomicU64, Ordering};

static ARG_COUNT: AtomicU64 = AtomicU64::new(0);
static ARG_VECTOR: AtomicPtr<*const u8> = AtomicPtr::new(core::ptr::null_mut());
static ENV_COUNT: AtomicU64 = AtomicU64::new(0);
static ENV_VECTOR: AtomicPtr<*const u8> = AtomicPtr::new(core::ptr::null_mut());

/// Record the argument and environment vectors captured at process entry.
///
/// `argv` points at `argc` NUL-terminated C strings; `envp` points at a
/// NULL-terminated array of `KEY=VALUE` C strings whose length is derived here.
///
/// # Safety
///
/// `argv` must address `argc` readable string pointers and `envp` a
/// NULL-terminated array of readable string pointers, exactly as supplied by
/// the platform loader.
pub unsafe fn capture(argc: u64, argv: *const *const u8, envp: *const *const u8) {
    ARG_COUNT.store(argc, Ordering::Relaxed);
    ARG_VECTOR.store(argv as *mut *const u8, Ordering::Relaxed);

    // The environment array length is not passed in; it is NULL-terminated.
    let mut env_count = 0u64;
    if !envp.is_null() {
        // SAFETY: the loader guarantees a NULL-terminated array of readable
        // pointers.
        while !unsafe { *envp.add(env_count as usize) }.is_null() {
            env_count += 1;
        }
    }
    ENV_COUNT.store(env_count, Ordering::Relaxed);
    ENV_VECTOR.store(envp as *mut *const u8, Ordering::Relaxed);
}

/// Capture from the raw initial stack pointer on the two Linux targets.
///
/// The System V process-startup contract lays the initial stack out as:
///
/// ```text
///   [sp + 0]                 argc                (one word)
///   [sp + 1 ..= sp + argc]   argv[0 .. argc]
///   [sp + 1 + argc]          NULL                (argv terminator)
///   [sp + 2 + argc ..]       envp[0 ..], NULL
/// ```
///
/// which holds for both x86-64 and AArch64 Linux (argc at `[sp]`, argv
/// following, envp after the argv NULL). The entry shim passes the untouched
/// entry `sp`/`rsp` so this offset arithmetic is exact.
///
/// # Safety
///
/// `stack` must be the unmodified initial stack pointer the kernel set at
/// process entry.
#[cfg(all(
    not(test),
    any(
        all(target_arch = "x86_64", target_os = "linux"),
        all(target_arch = "aarch64", target_os = "linux")
    )
))]
pub unsafe fn capture_from_stack(stack: *const usize) {
    // SAFETY: `stack` addresses the initial process stack described above.
    let argc = unsafe { *stack } as u64;
    let argv = unsafe { stack.add(1) } as *const *const u8;
    // envp begins one word past the NULL that terminates argv.
    let envp = unsafe { stack.add(2 + argc as usize) } as *const *const u8;
    // SAFETY: the layout above yields readable argv/envp vectors.
    unsafe { capture(argc, argv, envp) };
}

/// NUL-terminated byte length of `ptr` (`strlen`).
///
/// # Safety
///
/// `ptr` must be non-null and point to a NUL-terminated byte string.
unsafe fn c_str_len(ptr: *const u8) -> u64 {
    let mut len = 0u64;
    // Volatile reads deliberately defeat LLVM's loop-idiom recognition, which
    // would otherwise rewrite this scan into a `strlen` libcall — a symbol the
    // no_std runtime (and the Rue linker) does not provide.
    // SAFETY: caller guarantees a NUL terminator within the allocation.
    while unsafe { core::ptr::read_volatile(ptr.add(len as usize)) } != 0 {
        len += 1;
    }
    len
}

fn vector_entry(vector: &AtomicPtr<*const u8>, count: &AtomicU64, index: u64) -> *mut u8 {
    if index >= count.load(Ordering::Relaxed) {
        return core::ptr::null_mut();
    }
    let base = vector.load(Ordering::Relaxed) as *const *const u8;
    if base.is_null() {
        return core::ptr::null_mut();
    }
    // SAFETY: `index` is in range and `base` addresses at least that many
    // captured entries.
    unsafe { *base.add(index as usize) as *mut u8 }
}

fn vector_len(vector: &AtomicPtr<*const u8>, count: &AtomicU64, index: u64) -> u64 {
    let ptr = vector_entry(vector, count, index);
    if ptr.is_null() {
        return 0;
    }
    // SAFETY: a captured entry is a NUL-terminated C string.
    unsafe { c_str_len(ptr) }
}

/// Number of command-line arguments captured at entry (includes `argv[0]`).
pub fn __rue_arg_count() -> u64 {
    ARG_COUNT.load(Ordering::Relaxed)
}

/// Pointer to argument `index`'s bytes, or null when `index >= arg_count`.
pub fn __rue_arg_ptr(index: u64) -> *mut u8 {
    vector_entry(&ARG_VECTOR, &ARG_COUNT, index)
}

/// Byte length of argument `index`, or 0 when out of range.
pub fn __rue_arg_len(index: u64) -> u64 {
    vector_len(&ARG_VECTOR, &ARG_COUNT, index)
}

/// Number of environment entries captured at entry.
pub fn __rue_env_count() -> u64 {
    ENV_COUNT.load(Ordering::Relaxed)
}

/// Pointer to environment entry `index`'s `KEY=VALUE` bytes, or null when out
/// of range.
pub fn __rue_env_ptr(index: u64) -> *mut u8 {
    vector_entry(&ENV_VECTOR, &ENV_COUNT, index)
}

/// Byte length of environment entry `index`, or 0 when out of range.
pub fn __rue_env_len(index: u64) -> u64 {
    vector_len(&ENV_VECTOR, &ENV_COUNT, index)
}

#[cfg(test)]
mod tests {
    use super::*;

    // A single test drives every accessor: the capture state is process-global,
    // so splitting it across parallel tests would race on the statics.
    #[test]
    fn capture_exposes_args_and_derives_env_count() {
        // argv: ["prog", "hello"] as NUL-terminated C strings + NULL terminator.
        let a0 = b"prog\0";
        let a1 = b"hello\0";
        let argv: [*const u8; 3] = [a0.as_ptr(), a1.as_ptr(), core::ptr::null()];
        // envp is NULL-terminated; capture derives its length.
        let e0 = b"RUE_ENV_TEST=marker\0";
        let e1 = b"PATH=/bin\0";
        let envp: [*const u8; 3] = [e0.as_ptr(), e1.as_ptr(), core::ptr::null()];

        // SAFETY: argv/envp are valid NUL-/NULL-terminated vectors for this call.
        unsafe { capture(2, argv.as_ptr(), envp.as_ptr()) };

        assert_eq!(__rue_arg_count(), 2);
        assert_eq!(__rue_arg_len(0), 4);
        assert_eq!(__rue_arg_len(1), 5);
        // SAFETY: index 1 is in range, so the pointer addresses 5 readable bytes.
        unsafe {
            let ptr = __rue_arg_ptr(1);
            assert!(!ptr.is_null());
            assert_eq!(core::slice::from_raw_parts(ptr, 5), b"hello");
        }
        // Out-of-range indices are defensively bounds-checked in the runtime.
        assert!(__rue_arg_ptr(2).is_null());
        assert_eq!(__rue_arg_len(2), 0);
        assert!(__rue_arg_ptr(9999).is_null());

        // env length is derived from the NULL terminator, not passed in.
        assert_eq!(__rue_env_count(), 2);
        assert_eq!(__rue_env_len(0), 19);
        // SAFETY: index 0 is in range.
        unsafe {
            let ptr = __rue_env_ptr(0);
            assert!(!ptr.is_null());
            assert_eq!(core::slice::from_raw_parts(ptr, 19), b"RUE_ENV_TEST=marker");
        }
        assert!(__rue_env_ptr(2).is_null());
        assert_eq!(__rue_env_len(2), 0);

        // Re-capture with an empty environment (envp[0] == NULL): the derived
        // count is zero without UB. Kept in this single test because the capture
        // state is a process-global that parallel tests would race on.
        let only = b"only\0";
        let argv2: [*const u8; 2] = [only.as_ptr(), core::ptr::null()];
        let envp2: [*const u8; 1] = [core::ptr::null()];
        // SAFETY: argv2/envp2 are valid terminated vectors.
        unsafe { capture(1, argv2.as_ptr(), envp2.as_ptr()) };
        assert_eq!(__rue_arg_count(), 1);
        assert_eq!(__rue_env_count(), 0);
        assert!(__rue_env_ptr(0).is_null());
        assert_eq!(__rue_env_len(0), 0);
    }
}
