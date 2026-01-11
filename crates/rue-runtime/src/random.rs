//! Random number generation intrinsics
//!
//! This module implements the `@random_u32` and `@random_u64` intrinsics for
//! generating cryptographically-secure random numbers using platform syscalls.
//!
//! See ADR-0027 for the design rationale.

use crate::platform;

define_for_all_platforms! {
    /// Generate a random u32 value.
    ///
    /// Uses platform-specific entropy source:
    /// - Linux: getrandom() syscall
    /// - macOS: getentropy() syscall
    ///
    /// # Calling Convention
    ///
    /// - No arguments
    /// - Returns: Random u32 value
    ///
    /// # Panics
    ///
    /// Panics if the entropy source is unavailable or fails.
    pub extern "C" fn __rue_random_u32() -> u32 {
        let mut bytes = [0u8; 4];
        get_random_bytes(&mut bytes);
        u32::from_ne_bytes(bytes)
    }
}

define_for_all_platforms! {
    /// Generate a random u64 value.
    ///
    /// Uses platform-specific entropy source:
    /// - Linux: getrandom() syscall
    /// - macOS: getentropy() syscall
    ///
    /// # Calling Convention
    ///
    /// - No arguments
    /// - Returns: Random u64 value
    ///
    /// # Panics
    ///
    /// Panics if the entropy source is unavailable or fails.
    pub extern "C" fn __rue_random_u64() -> u64 {
        let mut bytes = [0u8; 8];
        get_random_bytes(&mut bytes);
        u64::from_ne_bytes(bytes)
    }
}

/// Get random bytes from the platform entropy source.
///
/// # Platform-specific Implementation
///
/// ## Linux (x86-64 and aarch64)
///
/// Uses the `getrandom()` syscall:
/// - x86-64: syscall #318
/// - aarch64: syscall #278
///
/// ## macOS (aarch64)
///
/// Uses the `getentropy()` syscall (syscall #500).
///
/// # Panics
///
/// Panics if the syscall fails or returns insufficient bytes.
fn get_random_bytes(buf: &mut [u8]) {
    #[cfg(target_os = "linux")]
    {
        // Linux: use getrandom() syscall
        #[cfg(target_arch = "x86_64")]
        const SYS_GETRANDOM: i64 = 318;

        #[cfg(target_arch = "aarch64")]
        const SYS_GETRANDOM: u64 = 278;

        let result = unsafe {
            #[cfg(target_arch = "x86_64")]
            {
                let mut ret: i64;
                core::arch::asm!(
                    "syscall",
                    inlateout("rax") SYS_GETRANDOM => ret,
                    in("rdi") buf.as_mut_ptr(),
                    in("rsi") buf.len(),
                    in("rdx") 0u64, // flags (0 = default)
                    lateout("rcx") _,
                    lateout("r11") _,
                    options(nostack)
                );
                ret
            }

            #[cfg(target_arch = "aarch64")]
            {
                let mut ret: i64;
                core::arch::asm!(
                    "svc #0",
                    inlateout("x8") SYS_GETRANDOM => _,
                    in("x0") buf.as_mut_ptr(),
                    in("x1") buf.len(),
                    in("x2") 0u64, // flags (0 = default)
                    lateout("x0") ret,
                    options(nostack)
                );
                ret
            }
        };

        if result < 0 || result as usize != buf.len() {
            random_error(b"random number generation failed\n");
        }
    }

    #[cfg(all(target_arch = "aarch64", target_os = "macos"))]
    {
        // macOS: use getentropy() syscall (syscall #500)
        const SYS_GETENTROPY: u64 = 500;

        let result: i64;
        let err_flag: u64;

        unsafe {
            core::arch::asm!(
                "svc #0x80",
                // Check carry flag for error
                "cset {err}, cs",
                inlateout("x16") SYS_GETENTROPY => _,
                in("x0") buf.as_mut_ptr(),
                in("x1") buf.len(),
                lateout("x0") result,
                err = out(reg) err_flag,
                out("x17") _,
                options(nostack)
            );
        }

        if err_flag != 0 || result != 0 {
            random_error(b"random number generation failed\n");
        }
    }
}

/// Print a random error message and exit.
///
/// This is called when random number generation fails. It writes the error
/// message to stderr and exits with code 101 (the standard Rue runtime error
/// exit code).
fn random_error(msg: &[u8]) -> ! {
    platform::write_stderr(msg);
    platform::exit(101);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_random_u32_generates_values() {
        // We can't test for specific values (it's random), but we can test
        // that it doesn't panic and returns different values
        let a = __rue_random_u32();
        let b = __rue_random_u32();
        let c = __rue_random_u32();

        // Statistically, three random u32 values should not all be the same
        // (though this could theoretically fail, the probability is negligible)
        assert!(
            a != b || b != c,
            "random_u32 returned same value three times"
        );
    }

    #[test]
    fn test_random_u64_generates_values() {
        let a = __rue_random_u64();
        let b = __rue_random_u64();
        let c = __rue_random_u64();

        assert!(
            a != b || b != c,
            "random_u64 returned same value three times"
        );
    }

    #[test]
    fn test_get_random_bytes() {
        let mut buf = [0u8; 16];
        get_random_bytes(&mut buf);

        // Check that not all bytes are zero (extremely unlikely with real entropy)
        let all_zeros = buf.iter().all(|&b| b == 0);
        assert!(!all_zeros, "get_random_bytes returned all zeros");
    }
}
