//! Linux syscall wrappers
//!
//! This module provides safe wrappers around Linux system calls using inline assembly.
//! All syscalls follow the System V AMD64 ABI.

// This module will be implemented in Phase 2
// For now, just re-export the syscall helpers from buffered_io

/// Result type for syscall operations
pub type SyscallResult<T> = Result<T, i32>;

// Syscall implementation will be added in Phase 2
