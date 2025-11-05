//! ABI definitions and constants for Rue runtime
//!
//! This module defines the stable ABI between the Rue compiler and runtime,
//! including calling conventions, type definitions, and constants.
//!
//! # Calling Convention
//!
//! All runtime functions use the System V AMD64 ABI (standard for Linux x86-64):
//! - First 6 integer/pointer args: RDI, RSI, RDX, RCX, R8, R9
//! - Return value: RAX
//! - Caller-saved: RAX, RCX, RDX, RSI, RDI, R8-R11
//! - Callee-saved: RBX, RBP, R12-R15
//!
//! # Safety
//!
//! Functions exported via C ABI are marked `unsafe extern "C"` and require:
//! - Pointers must be valid and properly aligned
//! - Sizes must not cause buffer overflows
//! - Caller ensures data races don't occur

/// Buffer size for stdout buffering (4KB)
pub const STDOUT_BUFFER_SIZE: usize = 4096;

/// Input buffer size for reading from stdin (1KB)
pub const INPUT_BUFFER_SIZE: usize = 1024;

/// Buffer size for integer-to-string conversion (20 bytes is enough for i64)
pub const ITOA_BUFFER_SIZE: usize = 20;

/// Memory alignment for allocations (16 bytes for x86-64)
pub const MEMORY_ALIGNMENT: usize = 16;

/// Linux syscall numbers
pub const SYS_READ: i64 = 0;
pub const SYS_WRITE: i64 = 1;
pub const SYS_EXIT: i64 = 60;

/// File descriptors
pub const STDIN_FD: i32 = 0;
pub const STDOUT_FD: i32 = 1;
pub const STDERR_FD: i32 = 2;

/// Exit codes
pub const EXIT_SUCCESS: i32 = 0;
pub const EXIT_FAILURE: i32 = 1;
pub const EXIT_PANIC: i32 = 255;
pub const EXIT_SYSCALL_FAILED: i32 = 253;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_buffer_sizes() {
        assert!(
            STDOUT_BUFFER_SIZE >= 1024,
            "stdout buffer should be at least 1KB"
        );
        assert!(
            INPUT_BUFFER_SIZE >= 256,
            "input buffer should be at least 256 bytes"
        );
        assert!(ITOA_BUFFER_SIZE >= 20, "itoa buffer must fit i64::MIN");
    }

    #[test]
    fn test_alignment() {
        assert!(
            MEMORY_ALIGNMENT.is_power_of_two(),
            "alignment must be power of 2"
        );
        assert!(MEMORY_ALIGNMENT >= 8, "alignment must be at least 8 bytes");
    }
}
