//! Memory operation functions
//!
//! Provides optimized memory operations (copy, move, set, zero) with
//! CPU feature detection for ERMS support.
//!
//! # Performance Strategy
//!
//! This module uses a two-tier approach:
//! 1. **Baseline**: Rust's ptr methods (LLVM optimizes these well)
//! 2. **ERMS**: Enhanced REP MOVSB/STOSB for large operations (>4KB)
//!
//! At startup, we detect CPU features and set up function pointers to
//! dispatch to the optimal implementation.

use core::sync::atomic::{AtomicPtr, Ordering};

/// Threshold for using ERMS variants (4KB)
const ERMS_THRESHOLD: usize = 4096;

/// CPU feature flags
static mut CPU_FEATURES: u32 = 0;

/// Feature bit for ERMS support
const FEATURE_ERMS: u32 = 1 << 0;

/// Function pointer type for memcpy
type MemcpyFn = unsafe extern "C" fn(*mut u8, *const u8, usize);

/// Function pointer type for memmove
type MemmoveFn = unsafe extern "C" fn(*mut u8, *const u8, usize);

/// Function pointer type for memset
type MemsetFn = unsafe extern "C" fn(*mut u8, u8, usize);

/// Function pointer type for memzero
type MemzeroFn = unsafe extern "C" fn(*mut u8, usize);

/// Global function pointers (initialized at startup)
static MEMCPY_PTR: AtomicPtr<()> = AtomicPtr::new(memcpy_baseline as *mut ());
static MEMMOVE_PTR: AtomicPtr<()> = AtomicPtr::new(memmove_baseline as *mut ());
static MEMSET_PTR: AtomicPtr<()> = AtomicPtr::new(memset_baseline as *mut ());
static MEMZERO_PTR: AtomicPtr<()> = AtomicPtr::new(memzero_baseline as *mut ());

/// Detect CPU features using CPUID
///
/// This should be called once at program startup.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __rue_detect_cpu_features() {
    // CPUID with EAX=7, ECX=0 returns extended features in EBX
    // Bit 9 of EBX indicates ERMS (Enhanced REP MOVSB/STOSB) support
    let ebx: u32;
    unsafe {
        core::arch::asm!(
            "push rbx",       // Save RBX (callee-saved)
            "mov eax, 7",     // CPUID function 7
            "mov ecx, 0",     // Sub-function 0
            "cpuid",
            "mov {0:e}, ebx", // Move EBX result to output
            "pop rbx",        // Restore RBX
            out(reg) ebx,
            out("rax") _,
            out("rcx") _,
            out("rdx") _,
        );
    }

    // Check bit 9 for ERMS
    if (ebx & (1 << 9)) != 0 {
        unsafe {
            CPU_FEATURES |= FEATURE_ERMS;

            // Update function pointers to use ERMS variants
            MEMCPY_PTR.store(memcpy_erms as *mut (), Ordering::Relaxed);
            MEMMOVE_PTR.store(memmove_erms as *mut (), Ordering::Relaxed);
            MEMSET_PTR.store(memset_erms as *mut (), Ordering::Relaxed);
            MEMZERO_PTR.store(memzero_erms as *mut (), Ordering::Relaxed);
        }
    }
}

// ============================================================================
// Baseline Implementations (using Rust's ptr methods)
// ============================================================================

/// Baseline memcpy using ptr::copy_nonoverlapping
unsafe extern "C" fn memcpy_baseline(dst: *mut u8, src: *const u8, len: usize) {
    if dst.is_null() || src.is_null() || len == 0 {
        return;
    }
    unsafe {
        core::ptr::copy_nonoverlapping(src, dst, len);
    }
}

/// Baseline memmove using ptr::copy (handles overlap)
unsafe extern "C" fn memmove_baseline(dst: *mut u8, src: *const u8, len: usize) {
    if dst.is_null() || src.is_null() || len == 0 {
        return;
    }
    unsafe {
        core::ptr::copy(src, dst, len);
    }
}

/// Baseline memset using ptr::write_bytes
unsafe extern "C" fn memset_baseline(dst: *mut u8, value: u8, len: usize) {
    if dst.is_null() || len == 0 {
        return;
    }
    unsafe {
        core::ptr::write_bytes(dst, value, len);
    }
}

/// Baseline memzero (delegates to memset with 0)
unsafe extern "C" fn memzero_baseline(dst: *mut u8, len: usize) {
    unsafe {
        memset_baseline(dst, 0, len);
    }
}

// ============================================================================
// ERMS Implementations (using rep movsb/stosb)
// ============================================================================

/// ERMS-optimized memcpy using rep movsb
unsafe extern "C" fn memcpy_erms(dst: *mut u8, src: *const u8, len: usize) {
    if dst.is_null() || src.is_null() || len == 0 {
        return;
    }

    // Use ERMS for large copies, baseline for small
    if len < ERMS_THRESHOLD {
        unsafe {
            memcpy_baseline(dst, src, len);
        }
        return;
    }

    // rep movsb: RDI=dest, RSI=src, RCX=count
    unsafe {
        core::arch::asm!(
            "rep movsb",
            inout("rdi") dst => _,
            inout("rsi") src => _,
            inout("rcx") len => _,
            options(nostack)
        );
    }
}

/// ERMS-optimized memmove using rep movsb
unsafe extern "C" fn memmove_erms(dst: *mut u8, src: *const u8, len: usize) {
    if dst.is_null() || src.is_null() || len == 0 {
        return;
    }

    // Use ERMS for large copies, baseline for small
    if len < ERMS_THRESHOLD {
        unsafe {
            memmove_baseline(dst, src, len);
        }
        return;
    }

    // Check for overlap
    let dst_addr = dst as usize;
    let src_addr = src as usize;

    if dst_addr == src_addr {
        return; // Same address, nothing to do
    }

    if dst_addr < src_addr || dst_addr >= src_addr + len {
        // No overlap or dst before src: forward copy is safe
        unsafe {
            core::arch::asm!(
                "rep movsb",
                inout("rdi") dst => _,
                inout("rsi") src => _,
                inout("rcx") len => _,
                options(nostack)
            );
        }
    } else {
        // Overlap with dst after src: need backward copy
        // Set direction flag for backward copy, then clear it
        let dst_end = dst.add(len - 1);
        let src_end = src.add(len - 1);
        unsafe {
            core::arch::asm!(
                "std",          // Set direction flag (backward)
                "rep movsb",
                "cld",          // Clear direction flag (restore forward)
                inout("rdi") dst_end => _,
                inout("rsi") src_end => _,
                inout("rcx") len => _,
                options(nostack)
            );
        }
    }
}

/// ERMS-optimized memset using rep stosb
unsafe extern "C" fn memset_erms(dst: *mut u8, value: u8, len: usize) {
    if dst.is_null() || len == 0 {
        return;
    }

    // Use ERMS for large sets, baseline for small
    if len < ERMS_THRESHOLD {
        unsafe {
            memset_baseline(dst, value, len);
        }
        return;
    }

    // rep stosb: RDI=dest, AL=value, RCX=count
    unsafe {
        core::arch::asm!(
            "rep stosb",
            inout("rdi") dst => _,
            in("al") value,
            inout("rcx") len => _,
            options(nostack)
        );
    }
}

/// ERMS-optimized memzero (delegates to memset_erms with 0)
unsafe extern "C" fn memzero_erms(dst: *mut u8, len: usize) {
    unsafe {
        memset_erms(dst, 0, len);
    }
}

// ============================================================================
// Public API (uses function pointers for dispatch)
// ============================================================================

/// Copy memory from src to dst (non-overlapping)
///
/// # Safety
/// - `dst` and `src` must not overlap
/// - Both pointers must be valid for `len` bytes
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __rue_memcpy(dst: *mut u8, src: *const u8, len: usize) {
    let func_ptr = MEMCPY_PTR.load(Ordering::Relaxed) as *const ();
    let func: MemcpyFn = unsafe { core::mem::transmute(func_ptr) };
    unsafe {
        func(dst, src, len);
    }
}

/// Copy memory from src to dst (handles overlap)
///
/// # Safety
/// - Both pointers must be valid for `len` bytes
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __rue_memmove(dst: *mut u8, src: *const u8, len: usize) {
    let func_ptr = MEMMOVE_PTR.load(Ordering::Relaxed) as *const ();
    let func: MemmoveFn = unsafe { core::mem::transmute(func_ptr) };
    unsafe {
        func(dst, src, len);
    }
}

/// Set memory to a specific byte value
///
/// # Safety
/// - `dst` must be valid for `len` bytes
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __rue_memset(dst: *mut u8, value: u8, len: usize) {
    let func_ptr = MEMSET_PTR.load(Ordering::Relaxed) as *const ();
    let func: MemsetFn = unsafe { core::mem::transmute(func_ptr) };
    unsafe {
        func(dst, value, len);
    }
}

/// Zero memory
///
/// # Safety
/// - `dst` must be valid for `len` bytes
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __rue_memzero(dst: *mut u8, len: usize) {
    let func_ptr = MEMZERO_PTR.load(Ordering::Relaxed) as *const ();
    let func: MemzeroFn = unsafe { core::mem::transmute(func_ptr) };
    unsafe {
        func(dst, len);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_memcpy_basic() {
        let src = [1u8, 2, 3, 4, 5];
        let mut dst = [0u8; 5];

        unsafe {
            __rue_memcpy(dst.as_mut_ptr(), src.as_ptr(), 5);
        }

        assert_eq!(dst, src);
    }

    #[test]
    fn test_memcpy_empty() {
        let src = [1u8, 2, 3];
        let mut dst = [0u8; 3];

        unsafe {
            __rue_memcpy(dst.as_mut_ptr(), src.as_ptr(), 0);
        }

        assert_eq!(dst, [0, 0, 0]); // Should be unchanged
    }

    #[test]
    fn test_memmove_no_overlap() {
        let src = [1u8, 2, 3, 4, 5];
        let mut dst = [0u8; 5];

        unsafe {
            __rue_memmove(dst.as_mut_ptr(), src.as_ptr(), 5);
        }

        assert_eq!(dst, src);
    }

    #[test]
    fn test_memmove_overlap_forward() {
        let mut buf = [1u8, 2, 3, 4, 5, 0, 0, 0];

        // Copy from [0..5] to [3..8] (overlap)
        unsafe {
            __rue_memmove(buf.as_mut_ptr().add(3), buf.as_ptr(), 5);
        }

        assert_eq!(buf, [1, 2, 3, 1, 2, 3, 4, 5]);
    }

    #[test]
    fn test_memmove_overlap_backward() {
        let mut buf = [0u8, 0, 0, 1, 2, 3, 4, 5];

        // Copy from [3..8] to [0..5] (overlap)
        unsafe {
            __rue_memmove(buf.as_mut_ptr(), buf.as_ptr().add(3), 5);
        }

        assert_eq!(buf, [1, 2, 3, 4, 5, 3, 4, 5]);
    }

    #[test]
    fn test_memset_basic() {
        let mut buf = [0u8; 10];

        unsafe {
            __rue_memset(buf.as_mut_ptr(), 0xAA, 10);
        }

        assert_eq!(buf, [0xAA; 10]);
    }

    #[test]
    fn test_memzero_basic() {
        let mut buf = [0xFFu8; 10];

        unsafe {
            __rue_memzero(buf.as_mut_ptr(), 10);
        }

        assert_eq!(buf, [0; 10]);
    }

    #[test]
    fn test_cpu_detection() {
        // Just test that CPU detection doesn't crash
        unsafe {
            __rue_detect_cpu_features();
        }

        // After detection, function pointers should be set
        // (either to baseline or ERMS variants)
        let ptr = MEMCPY_PTR.load(Ordering::Relaxed);
        assert!(!ptr.is_null());
    }

    #[test]
    fn test_memcpy_large() {
        // Test with size > ERMS_THRESHOLD to exercise ERMS path
        let src = vec![42u8; 8192];
        let mut dst = vec![0u8; 8192];

        unsafe {
            // Ensure CPU detection has run
            __rue_detect_cpu_features();
            __rue_memcpy(dst.as_mut_ptr(), src.as_ptr(), 8192);
        }

        assert_eq!(dst, src);
    }
}
