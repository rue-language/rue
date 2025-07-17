//! Linux x86-64 syscall constants and helpers

/// Linux x86-64 syscall numbers
pub mod numbers {
    pub const SYS_READ: u64 = 0;
    pub const SYS_WRITE: u64 = 1;
    pub const SYS_EXIT: u64 = 60;
}

/// File descriptors
pub mod fd {
    pub const STDIN: u64 = 0;
    pub const STDOUT: u64 = 1;
    pub const STDERR: u64 = 2;
}
