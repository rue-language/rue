//! Memory management helper functions for the Rue runtime
//!
//! This module provides efficient, self-contained memory operations:
//! - memcpy: Fast memory copying with size-optimized strategies
//! - memmove: Safe memory copying that handles overlapping regions
//! - memzero: Efficient memory zeroing using rep stosb
//!
//! All functions are generated as inline assembly without libc dependencies.

use crate::constants::{MEMCPY_QWORD_THRESHOLD, MEMCPY_REP_THRESHOLD};
use crate::runtime::context::RuntimeContext;
use rue_target::{ConditionCode, LabelRef, X86Register, X8664Instr};

impl RuntimeContext {
    /// Generate memory helper functions
    pub fn generate_memory_functions(&mut self) {
        self.generate_memcpy_function();
        self.generate_memmove_function();
        self.generate_memzero_function();
    }

    /// Generate __rue_memcpy function with size-optimized strategy
    ///
    /// Strategy:
    /// - Small copies (< 8 bytes): byte-by-byte loop
    /// - Medium copies (8-256 bytes): 8-byte chunks with remainder handling
    /// - Large copies (>= 256 bytes): rep movsb (optimized on modern CPUs)
    ///
    /// Parameters: RDI = dest, RSI = src, RDX = len
    /// Returns: void (no return value)
    pub fn generate_memcpy_function(&mut self) {
        let memcpy_label = self.define_label("__rue_memcpy");
        let byte_copy_loop = self.new_label();
        let qword_copy_loop = self.new_label();
        let use_rep_movsb = self.new_label();
        let done = self.new_label();

        self.instructions
            .push(X8664Instr::Label { id: memcpy_label });

        // Check if len == 0
        self.instructions.push(X8664Instr::CmpRI {
            reg: X86Register::Rdx,
            imm: 0,
        });
        self.instructions.push(X8664Instr::JmpCC {
            cc: ConditionCode::Equal,
            target: LabelRef::Local(done),
        });

        // Check if len >= MEMCPY_REP_THRESHOLD (use rep movsb for large copies)
        self.instructions.push(X8664Instr::CmpRI {
            reg: X86Register::Rdx,
            imm: MEMCPY_REP_THRESHOLD as i32,
        });
        self.instructions.push(X8664Instr::JmpCC {
            cc: ConditionCode::GreaterEqual,
            target: LabelRef::Local(use_rep_movsb),
        });

        // Check if len >= MEMCPY_QWORD_THRESHOLD (use qword copies)
        self.instructions.push(X8664Instr::CmpRI {
            reg: X86Register::Rdx,
            imm: MEMCPY_QWORD_THRESHOLD as i32,
        });
        self.instructions.push(X8664Instr::JmpCC {
            cc: ConditionCode::Less,
            target: LabelRef::Local(byte_copy_loop),
        });

        // Qword copy loop
        self.instructions.push(X8664Instr::Label {
            id: qword_copy_loop,
        });

        // Copy 8 bytes at a time
        self.instructions.push(X8664Instr::MovRM {
            dest: X86Register::Rax,
            base: X86Register::Rsi,
            offset: 0,
        });
        self.instructions.push(X8664Instr::MovMR {
            base: X86Register::Rdi,
            offset: 0,
            src: X86Register::Rax,
        });

        // Advance pointers
        self.instructions.push(X8664Instr::AddRI {
            dest: X86Register::Rsi,
            imm: 8,
        });
        self.instructions.push(X8664Instr::AddRI {
            dest: X86Register::Rdi,
            imm: 8,
        });
        self.instructions.push(X8664Instr::SubRI {
            dest: X86Register::Rdx,
            imm: 8,
        });

        // Continue if len >= 8
        self.instructions.push(X8664Instr::CmpRI {
            reg: X86Register::Rdx,
            imm: MEMCPY_QWORD_THRESHOLD as i32,
        });
        self.instructions.push(X8664Instr::JmpCC {
            cc: ConditionCode::GreaterEqual,
            target: LabelRef::Local(qword_copy_loop),
        });

        // Handle remaining bytes
        self.instructions.push(X8664Instr::CmpRI {
            reg: X86Register::Rdx,
            imm: 0,
        });
        self.instructions.push(X8664Instr::JmpCC {
            cc: ConditionCode::Equal,
            target: LabelRef::Local(done),
        });

        // Byte copy loop for remainder
        self.instructions
            .push(X8664Instr::Label { id: byte_copy_loop });
        self.instructions.push(X8664Instr::MovRM8 {
            dest: X86Register::Rax,
            base: X86Register::Rsi,
            offset: 0,
        });
        self.instructions.push(X8664Instr::MovMR8 {
            base: X86Register::Rdi,
            offset: 0,
            src: X86Register::Rax,
        });

        self.instructions.push(X8664Instr::AddRI {
            dest: X86Register::Rsi,
            imm: 1,
        });
        self.instructions.push(X8664Instr::AddRI {
            dest: X86Register::Rdi,
            imm: 1,
        });
        self.instructions.push(X8664Instr::SubRI {
            dest: X86Register::Rdx,
            imm: 1,
        });

        self.instructions.push(X8664Instr::JmpCC {
            cc: ConditionCode::NotEqual,
            target: LabelRef::Local(byte_copy_loop),
        });
        self.instructions.push(X8664Instr::Jmp {
            target: LabelRef::Local(done),
        });

        // Large copy using rep movsb
        self.instructions
            .push(X8664Instr::Label { id: use_rep_movsb });
        self.instructions.push(X8664Instr::MovRR {
            dest: X86Register::Rcx,
            src: X86Register::Rdx,
        });
        self.instructions.push(X8664Instr::Cld); // Clear direction flag
        self.instructions.push(X8664Instr::RepMovsb);

        // Done
        self.instructions.push(X8664Instr::Label { id: done });
        self.instructions.push(X8664Instr::Ret);
    }

    /// Generate __rue_memmove function with overlap detection
    ///
    /// This function safely handles overlapping memory regions by:
    /// - Checking for overlap: if dest > src && dest < src + len
    /// - Forward copy if no overlap or dest <= src
    /// - Backward copy if overlap and dest > src
    ///
    /// Parameters: RDI = dest, RSI = src, RDX = len
    /// Returns: void (no return value)
    pub fn generate_memmove_function(&mut self) {
        let memmove_label = self.define_label("__rue_memmove");
        let forward_copy = self.new_label();
        let backward_copy = self.new_label();
        let backward_loop = self.new_label();
        let done = self.new_label();

        self.instructions
            .push(X8664Instr::Label { id: memmove_label });

        // Check if len == 0
        self.instructions.push(X8664Instr::CmpRI {
            reg: X86Register::Rdx,
            imm: 0,
        });
        self.instructions.push(X8664Instr::JmpCC {
            cc: ConditionCode::Equal,
            target: LabelRef::Local(done),
        });

        // Check if dest <= src (no overlap or forward is safe)
        self.instructions.push(X8664Instr::CmpRR {
            left: X86Register::Rdi,
            right: X86Register::Rsi,
        });
        self.instructions.push(X8664Instr::JmpCC {
            cc: ConditionCode::LessEqual,
            target: LabelRef::Local(forward_copy),
        });

        // Check if dest >= src + len (no overlap)
        // R8 = src + len
        self.instructions.push(X8664Instr::MovRR {
            dest: X86Register::R8,
            src: X86Register::Rsi,
        });
        self.instructions.push(X8664Instr::AddRR {
            dest: X86Register::R8,
            src: X86Register::Rdx,
        });

        self.instructions.push(X8664Instr::CmpRR {
            left: X86Register::Rdi,
            right: X86Register::R8,
        });
        self.instructions.push(X8664Instr::JmpCC {
            cc: ConditionCode::GreaterEqual,
            target: LabelRef::Local(forward_copy),
        });

        // Overlapping regions, dest > src, need backward copy
        self.instructions.push(X8664Instr::Jmp {
            target: LabelRef::Local(backward_copy),
        });

        // Forward copy - same as memcpy
        self.instructions
            .push(X8664Instr::Label { id: forward_copy });
        self.instructions.push(X8664Instr::Call {
            target: "__rue_memcpy".to_string(),
        });
        self.instructions.push(X8664Instr::Jmp {
            target: LabelRef::Local(done),
        });

        // Backward copy - start from end
        self.instructions
            .push(X8664Instr::Label { id: backward_copy });

        // Adjust pointers to end of regions
        // RDI = dest + len - 1
        // RSI = src + len - 1
        self.instructions.push(X8664Instr::AddRR {
            dest: X86Register::Rdi,
            src: X86Register::Rdx,
        });
        self.instructions.push(X8664Instr::SubRI {
            dest: X86Register::Rdi,
            imm: 1,
        });

        self.instructions.push(X8664Instr::AddRR {
            dest: X86Register::Rsi,
            src: X86Register::Rdx,
        });
        self.instructions.push(X8664Instr::SubRI {
            dest: X86Register::Rsi,
            imm: 1,
        });

        // Backward byte copy loop
        self.instructions
            .push(X8664Instr::Label { id: backward_loop });

        // Copy byte
        self.instructions.push(X8664Instr::MovRM8 {
            dest: X86Register::Rax,
            base: X86Register::Rsi,
            offset: 0,
        });
        self.instructions.push(X8664Instr::MovMR8 {
            base: X86Register::Rdi,
            offset: 0,
            src: X86Register::Rax,
        });

        // Move pointers backward
        self.instructions.push(X8664Instr::SubRI {
            dest: X86Register::Rsi,
            imm: 1,
        });
        self.instructions.push(X8664Instr::SubRI {
            dest: X86Register::Rdi,
            imm: 1,
        });

        // Decrement count and loop
        self.instructions.push(X8664Instr::SubRI {
            dest: X86Register::Rdx,
            imm: 1,
        });
        self.instructions.push(X8664Instr::JmpCC {
            cc: ConditionCode::NotEqual,
            target: LabelRef::Local(backward_loop),
        });

        // Done
        self.instructions.push(X8664Instr::Label { id: done });
        self.instructions.push(X8664Instr::Ret);
    }

    /// Generate __rue_memzero function using rep stosb
    ///
    /// This function efficiently zeros memory using the rep stosb instruction,
    /// which is highly optimized on modern CPUs.
    ///
    /// Parameters: RDI = dest, RSI = len
    /// Returns: void (no return value)
    pub fn generate_memzero_function(&mut self) {
        let memzero_label = self.define_label("__rue_memzero");
        let done = self.new_label();

        self.instructions
            .push(X8664Instr::Label { id: memzero_label });

        // Check if len == 0
        self.instructions.push(X8664Instr::CmpRI {
            reg: X86Register::Rsi,
            imm: 0,
        });
        self.instructions.push(X8664Instr::JmpCC {
            cc: ConditionCode::Equal,
            target: LabelRef::Local(done),
        });

        // Set up for rep stosb
        // AL = 0 (zero byte value)
        self.instructions.push(X8664Instr::XorRR {
            dest: X86Register::Rax,
            src: X86Register::Rax,
        });

        // RCX = len (count for rep)
        self.instructions.push(X8664Instr::MovRR {
            dest: X86Register::Rcx,
            src: X86Register::Rsi,
        });

        // Clear direction flag and execute rep stosb
        self.instructions.push(X8664Instr::Cld);
        self.instructions.push(X8664Instr::RepStosb);

        // Done
        self.instructions.push(X8664Instr::Label { id: done });
        self.instructions.push(X8664Instr::Ret);
    }
}
