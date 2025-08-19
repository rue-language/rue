//! Optimized memory operations for x86-64
//!
//! This module provides highly optimized memory primitives with both
//! baseline and ERMS variants. The implementations use size-based
//! dispatch to choose the most efficient strategy for each operation.

use crate::runtime::context::RuntimeContext;
use rue_target::{ConditionCode, LabelRef, X86Register, X8664Instr};

impl RuntimeContext {
    /// Generate optimized memory functions
    pub fn generate_optimized_memory_functions(&mut self) {
        // For now, update the existing ERMS stubs to be more optimized
        // Once we have object file support, we'll use real assembly

        // The existing functions in memory.rs are already reasonably optimized
        // We'll enhance the ERMS versions here

        self.generate_memcpy_erms_enhanced();
        self.generate_memmove_erms_enhanced();
        self.generate_memset_erms_enhanced();
    }

    /// Enhanced ERMS memcpy that handles small sizes better
    fn generate_memcpy_erms_enhanced(&mut self) {
        // Replace the existing stub with a better version
        let memcpy_label = self.define_label("__rue_memcpy_erms_v2");
        let small_copy = self.new_label();
        let done = self.new_label();

        self.instructions
            .push(X8664Instr::Label { id: memcpy_label });

        // Check if len == 0
        self.instructions.push(X8664Instr::TestRR {
            left: X86Register::Rdx,
            right: X86Register::Rdx,
        });
        self.instructions.push(X8664Instr::JmpCC {
            cc: ConditionCode::Equal,
            target: LabelRef::Local(done),
        });

        // For very small copies (< 32), use simple moves
        self.instructions.push(X8664Instr::CmpRI {
            reg: X86Register::Rdx,
            imm: 32,
        });
        self.instructions.push(X8664Instr::JmpCC {
            cc: ConditionCode::Below,
            target: LabelRef::Local(small_copy),
        });

        // Large copy: use rep movsb (ERMS makes this fast)
        self.instructions.push(X8664Instr::MovRR {
            dest: X86Register::Rcx,
            src: X86Register::Rdx,
        });
        self.instructions.push(X8664Instr::Cld);
        self.instructions.push(X8664Instr::RepMovsb);
        self.instructions.push(X8664Instr::Jmp {
            target: LabelRef::Local(done),
        });

        // Small copy: use overlapping qword copies
        self.instructions.push(X8664Instr::Label { id: small_copy });

        // Copy first 8 bytes
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

        // If size > 8, copy last 8 bytes (may overlap)
        self.instructions.push(X8664Instr::CmpRI {
            reg: X86Register::Rdx,
            imm: 8,
        });
        self.instructions.push(X8664Instr::JmpCC {
            cc: ConditionCode::LessEqual,
            target: LabelRef::Local(done),
        });

        // Copy last 8 bytes at offset (size - 8)
        self.instructions.push(X8664Instr::SubRI {
            dest: X86Register::Rdx,
            imm: 8,
        });
        self.instructions.push(X8664Instr::AddRR {
            dest: X86Register::Rsi,
            src: X86Register::Rdx,
        });
        self.instructions.push(X8664Instr::AddRR {
            dest: X86Register::Rdi,
            src: X86Register::Rdx,
        });
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

        self.instructions.push(X8664Instr::Label { id: done });
        self.instructions.push(X8664Instr::Ret);
    }

    /// Enhanced ERMS memmove with proper overlap handling
    fn generate_memmove_erms_enhanced(&mut self) {
        let memmove_label = self.define_label("__rue_memmove_erms_v2");
        let backward_copy = self.new_label();
        let forward_copy = self.new_label();
        let done = self.new_label();

        self.instructions
            .push(X8664Instr::Label { id: memmove_label });

        // Check if len == 0
        self.instructions.push(X8664Instr::TestRR {
            left: X86Register::Rdx,
            right: X86Register::Rdx,
        });
        self.instructions.push(X8664Instr::JmpCC {
            cc: ConditionCode::Equal,
            target: LabelRef::Local(done),
        });

        // Check for overlap: if dest > src && dest < src + len
        self.instructions.push(X8664Instr::CmpRR {
            left: X86Register::Rdi,
            right: X86Register::Rsi,
        });
        self.instructions.push(X8664Instr::JmpCC {
            cc: ConditionCode::LessEqual,
            target: LabelRef::Local(forward_copy),
        });

        // Check if dest >= src + len (no overlap)
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

        // Backward copy needed
        self.instructions
            .push(X8664Instr::Label { id: backward_copy });

        // Set direction flag for backward copy
        self.instructions.push(X8664Instr::Std);

        // Adjust pointers to end
        self.instructions.push(X8664Instr::AddRR {
            dest: X86Register::Rsi,
            src: X86Register::Rdx,
        });
        self.instructions.push(X8664Instr::DecR {
            reg: X86Register::Rsi,
        });
        self.instructions.push(X8664Instr::AddRR {
            dest: X86Register::Rdi,
            src: X86Register::Rdx,
        });
        self.instructions.push(X8664Instr::DecR {
            reg: X86Register::Rdi,
        });

        // Copy backwards
        self.instructions.push(X8664Instr::MovRR {
            dest: X86Register::Rcx,
            src: X86Register::Rdx,
        });
        self.instructions.push(X8664Instr::RepMovsb);

        // Clear direction flag
        self.instructions.push(X8664Instr::Cld);
        self.instructions.push(X8664Instr::Jmp {
            target: LabelRef::Local(done),
        });

        // Forward copy
        self.instructions
            .push(X8664Instr::Label { id: forward_copy });
        self.instructions.push(X8664Instr::MovRR {
            dest: X86Register::Rcx,
            src: X86Register::Rdx,
        });
        self.instructions.push(X8664Instr::Cld);
        self.instructions.push(X8664Instr::RepMovsb);

        self.instructions.push(X8664Instr::Label { id: done });
        self.instructions.push(X8664Instr::Ret);
    }

    /// Enhanced ERMS memset
    fn generate_memset_erms_enhanced(&mut self) {
        let memset_label = self.define_label("__rue_memset_erms_v2");
        let done = self.new_label();

        self.instructions
            .push(X8664Instr::Label { id: memset_label });

        // Parameters: RDI = dest, RSI = byte value, RDX = len

        // Check if len == 0
        self.instructions.push(X8664Instr::TestRR {
            left: X86Register::Rdx,
            right: X86Register::Rdx,
        });
        self.instructions.push(X8664Instr::JmpCC {
            cc: ConditionCode::Equal,
            target: LabelRef::Local(done),
        });

        // Move byte value to AL
        self.instructions.push(X8664Instr::MovRR {
            dest: X86Register::Rax,
            src: X86Register::Rsi,
        });

        // ERMS makes rep stosb fast for all sizes
        self.instructions.push(X8664Instr::MovRR {
            dest: X86Register::Rcx,
            src: X86Register::Rdx,
        });
        self.instructions.push(X8664Instr::Cld);
        self.instructions.push(X8664Instr::RepStosb);

        self.instructions.push(X8664Instr::Label { id: done });
        self.instructions.push(X8664Instr::Ret);
    }
}
