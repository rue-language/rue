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
