//! Compiler-host allocation policy.
//!
//! Target Rue programs retain their own runtime allocation policy. This module
//! only selects the allocator for the compiler process.

#[cfg(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
#[global_allocator]
static ALLOCATOR: mimalloc::MiMalloc = mimalloc::MiMalloc;

#[cfg(not(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64")
)))]
#[global_allocator]
static ALLOCATOR: std::alloc::System = std::alloc::System;
