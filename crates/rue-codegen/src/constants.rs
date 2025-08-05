//! Constants used throughout the Rue runtime
//!
//! This module centralizes all magic numbers and constants used in the runtime
//! to improve maintainability and readability.

// Exit codes
/// Exit code for division by zero error
pub const EXIT_CODE_DIV_BY_ZERO: i64 = 250;

/// Exit code for stack overflow or segmentation fault
pub const EXIT_CODE_STACK_OVERFLOW: i64 = 251;

/// Exit code for array bounds violation
pub const EXIT_CODE_BOUNDS_CHECK: i64 = 252;

/// Exit code for failed syscall
pub const EXIT_CODE_SYSCALL_FAILED: i64 = 253;

// Linux x86-64 syscall numbers
/// Read from file descriptor
pub const SYSCALL_READ: i64 = 0;

/// Write to file descriptor
pub const SYSCALL_WRITE: i64 = 1;

/// Exit the process
pub const SYSCALL_EXIT: i64 = 60;

// File descriptors
/// Standard input
pub const FD_STDIN: i64 = 0;

/// Standard output
pub const FD_STDOUT: i64 = 1;

// Buffer sizes
/// Maximum size for input buffer
pub const INPUT_BUFFER_SIZE: u32 = 1024;

/// Buffer size for integer to string conversion
pub const ITOA_BUFFER_SIZE: u32 = 32;

// Character constants

/// Minus sign character
pub const CHAR_MINUS: u8 = 45;

/// Zero character
pub const CHAR_ZERO: u8 = 48;

// Memory management constants
/// Memory management system call: brk (change data segment size)
#[expect(unused)]
pub const SYSCALL_BRK: i64 = 12;

/// Memory management system call: mmap (map files or devices into memory)
#[expect(unused)]
pub const SYSCALL_MMAP: i64 = 9;

/// Memory management system call: munmap (unmap files or devices from memory)
#[expect(unused)]
pub const SYSCALL_MUNMAP: i64 = 11;

/// Default heap size for static allocator (64KB)
pub const DEFAULT_HEAP_SIZE: u64 = 65536;

/// Memory page size on x86-64 (4KB)
#[expect(unused)]
pub const PAGE_SIZE: u64 = 4096;

/// Heap alignment requirement (8 bytes for x86-64)
#[expect(unused)]
pub const HEAP_ALIGNMENT: u64 = 8;

/// Threshold for using rep movsb in memcpy (256 bytes)
pub const MEMCPY_REP_THRESHOLD: u64 = 256;

/// Threshold for qword copy vs byte copy (8 bytes)
pub const MEMCPY_QWORD_THRESHOLD: u64 = 8;
