//! Simple bump allocator for the Rue runtime
//!
//! This module provides a basic bump allocator using static memory:
//! - Fixed 64KB heap allocated in .bss section
//! - Allocation only (no free) - suitable for aggregates and temporary data
//! - 16-byte alignment for all allocations (SSE/AVX compatible)
//! - Can be upgraded to sys_brk or mmap later
//!
//! The allocator is designed for the initial aggregate type support and can
//! be extended with more sophisticated memory management in the future.

use crate::runtime::context::RuntimeContext;
use rue_target::{X86Register, X8664Instr};

impl RuntimeContext {
    /// Generate allocator functions
    pub fn generate_alloc_functions(&mut self) {
        self.generate_heap_init_function();
        self.generate_alloc_function();
        self.generate_free_function();
        self.generate_heap_reset_function();
    }

    /// Generate heap initialization function
    ///
    /// Initializes the heap by setting the current heap pointer to the start of the heap space.
    /// This must be called before any allocations are made.
    pub fn generate_heap_init_function(&mut self) {
        let heap_init_label = self.define_label("__rue_heap_init");

        self.instructions.push(X8664Instr::Label {
            id: heap_init_label,
        });

        // Load heap start address into RAX using LEA
        self.instructions.push(X8664Instr::LeaLabel {
            dest: X86Register::Rax,
            label: "__rue_heap_start".to_string(),
        });

        // Store heap start address into heap pointer location
        // Use LEA to get address of __rue_heap_ptr, then store RAX there
        self.instructions.push(X8664Instr::LeaLabel {
            dest: X86Register::Rdx,
            label: "__rue_heap_ptr".to_string(),
        });
        self.instructions.push(X8664Instr::MovMR {
            base: X86Register::Rdx,
            offset: 0,
            src: X86Register::Rax,
        });

        self.instructions.push(X8664Instr::Ret);
    }

    /// Generate __rue_alloc function
    ///
    /// Implements a simple bump allocator that allocates memory from a fixed heap.
    /// All allocations are 16-byte aligned. Returns NULL if out of memory.
    ///
    /// Parameters: RDI = size (bytes to allocate)
    /// Returns: RAX = pointer to allocated memory, or NULL if out of memory
    pub fn generate_alloc_function(&mut self) {
        let alloc_label = self.define_label("__rue_alloc");

        self.instructions
            .push(X8664Instr::Label { id: alloc_label });

        // Load current heap pointer into RAX
        self.instructions.push(X8664Instr::LeaLabel {
            dest: X86Register::Rdx,
            label: "__rue_heap_ptr".to_string(),
        });
        self.instructions.push(X8664Instr::MovRM {
            dest: X86Register::Rax,
            base: X86Register::Rdx,
            offset: 0,
        });

        // Save the current pointer as our return value (allocation address)
        self.instructions.push(X8664Instr::MovRR {
            dest: X86Register::Rcx,
            src: X86Register::Rax,
        });

        // Align the requested size to 16-byte boundary
        // size = (size + 15) & ~15
        self.instructions.push(X8664Instr::AddRI {
            dest: X86Register::Rdi,
            imm: 15,
        });
        self.instructions.push(X8664Instr::AndRI {
            dest: X86Register::Rdi,
            imm: -16, // 0xFFFFFFFFFFFFFFF0 for proper 16-byte alignment mask
        });

        // Calculate new heap pointer: current + aligned_size
        self.instructions.push(X8664Instr::AddRR {
            dest: X86Register::Rax,
            src: X86Register::Rdi,
        });

        // Check if new pointer exceeds heap end
        self.instructions.push(X8664Instr::LeaLabel {
            dest: X86Register::Rsi,
            label: "__rue_heap_end".to_string(),
        });
        self.instructions.push(X8664Instr::CmpRR {
            left: X86Register::Rax,
            right: X86Register::Rsi,
        });

        // Jump to out_of_memory if new_ptr >= heap_end
        let out_of_memory_label = self.new_label();
        self.instructions.push(X8664Instr::JmpCC {
            cc: rue_target::ConditionCode::GreaterEqual,
            target: rue_target::LabelRef::Local(out_of_memory_label),
        });

        // Update heap pointer with new value
        self.instructions.push(X8664Instr::MovMR {
            base: X86Register::Rdx,
            offset: 0,
            src: X86Register::Rax,
        });

        // Return the old pointer (stored in RCX)
        self.instructions.push(X8664Instr::MovRR {
            dest: X86Register::Rax,
            src: X86Register::Rcx,
        });
        self.instructions.push(X8664Instr::Ret);

        // Out of memory: return NULL
        self.instructions.push(X8664Instr::Label {
            id: out_of_memory_label,
        });
        self.instructions.push(X8664Instr::XorRR {
            dest: X86Register::Rax,
            src: X86Register::Rax,
        });
        self.instructions.push(X8664Instr::Ret);
    }

    /// Generate __rue_free function (no-op for bump allocator)
    ///
    /// This is a no-op function for compatibility. The bump allocator
    /// doesn't support individual frees - memory is reclaimed when
    /// the entire heap is reset.
    ///
    /// Parameters: RDI = pointer (ignored)
    /// Returns: void
    pub fn generate_free_function(&mut self) {
        let free_label = self.define_label("__rue_free");

        self.instructions.push(X8664Instr::Label { id: free_label });
        // No-op: just return
        self.instructions.push(X8664Instr::Ret);
    }

    /// Generate __rue_heap_reset function
    ///
    /// Resets the heap by setting the current heap pointer back to the start of the heap space.
    /// This effectively deallocates all allocated memory in the bump allocator.
    ///
    /// Parameters: none
    /// Returns: void
    pub fn generate_heap_reset_function(&mut self) {
        let reset_label = self.define_label("__rue_heap_reset");

        self.instructions
            .push(X8664Instr::Label { id: reset_label });

        // Load heap start address into RAX using LEA
        self.instructions.push(X8664Instr::LeaLabel {
            dest: X86Register::Rax,
            label: "__rue_heap_start".to_string(),
        });

        // Store heap start address into heap pointer location
        // Use LEA to get address of __rue_heap_ptr, then store RAX there
        self.instructions.push(X8664Instr::LeaLabel {
            dest: X86Register::Rdx,
            label: "__rue_heap_ptr".to_string(),
        });
        self.instructions.push(X8664Instr::MovMR {
            base: X86Register::Rdx,
            offset: 0,
            src: X86Register::Rax,
        });

        self.instructions.push(X8664Instr::Ret);
    }
}
