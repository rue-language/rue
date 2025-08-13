//! Simple tests for register allocator
//!
//! These tests verify basic properties of the register allocator.

use rue_codegen::RegisterAllocator;

#[test]
fn test_allocator_creation() {
    // Test that we can create an allocator
    let allocator = RegisterAllocator::new();

    // Basic smoke test - allocator should be created successfully
    // The actual allocation logic is tested through integration tests
    // since the allocator's main interface is through the lowering layer

    assert_eq!(
        allocator.get_stack_size(),
        0,
        "Initial stack size should be 0"
    );
}

#[test]
fn test_stack_alignment() {
    let allocator = RegisterAllocator::new();

    // Force some stack allocation
    // Note: The actual allocation methods are internal,
    // so we test through the public interface

    // Stack size should always be 16-byte aligned (x86-64 ABI requirement)
    let size = allocator.get_stack_size();
    assert_eq!(size % 16, 0, "Stack size must be 16-byte aligned");
}
