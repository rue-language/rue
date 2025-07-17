//! Constants used throughout the Rue runtime
//!
//! This module centralizes all magic numbers and constants used in the runtime
//! to improve maintainability and readability.

// Exit codes
/// Exit code for division by zero error
pub const EXIT_CODE_DIV_BY_ZERO: i64 = 250;

/// Exit code for stack overflow or segmentation fault
pub const EXIT_CODE_STACK_OVERFLOW: i64 = 251;

/// Exit code for failed syscall
pub const EXIT_CODE_SYSCALL_FAILED: i64 = 252;

// Linux x86-64 syscall numbers
/// Read from file descriptor
pub const SYSCALL_READ: i64 = 0;

/// Write to file descriptor
pub const SYSCALL_WRITE: i64 = 1;

/// Exit the process
pub const SYSCALL_EXIT: i64 = 60;

/// Set signal handler
pub const SYSCALL_RT_SIGACTION: i64 = 13;

// Signal numbers
/// Floating point exception signal (division by zero)
pub const SIGFPE: i64 = 8;

/// Segmentation fault signal
pub const SIGSEGV: i64 = 11;

// File descriptors
/// Standard input
pub const FD_STDIN: i64 = 0;

/// Standard output
pub const FD_STDOUT: i64 = 1;

/// Standard error
pub const FD_STDERR: i64 = 2;

// Buffer sizes
/// Maximum size for input buffer
pub const INPUT_BUFFER_SIZE: u32 = 1024;

/// Buffer size for integer to string conversion
pub const ITOA_BUFFER_SIZE: u32 = 32;

// Signal handling
/// Size of sigaction struct on Linux x86-64
pub const SIGACTION_STRUCT_SIZE: u32 = 152;

/// SA_RESTORER flag for signal handlers
pub const SA_RESTORER: i64 = 0x04000000;

/// Size of sigset_t
pub const SIGSET_SIZE: i64 = 8;

// String constants (encoded as hex for assembly)
/// "true" encoded as little-endian u64
pub const TRUE_STRING_HEX: i64 = 0x65757274; // "true" in hex

/// "false" encoded as little-endian u64
pub const FALSE_STRING_1: i64 = 0x65736c6166; // "false" part 1

/// "()" encoded as little-endian u64
pub const UNIT_STRING_HEX: i64 = 0x2928; // "()" in hex

// Character constants
/// Newline character
pub const CHAR_NEWLINE: u8 = 10;

/// Minus sign character
pub const CHAR_MINUS: u8 = 45;

/// Zero character
pub const CHAR_ZERO: u8 = 48;
