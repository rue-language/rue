//! Syscall wrappers and helpers

use crate::constants::*;
use crate::runtime::context::RuntimeContext;
use rue_target::{ConditionCode, LabelRef, X86Register, X8664Instr};

impl RuntimeContext {
    /// Emit a syscall with error checking
    ///
    /// After the syscall, checks if the return value is negative (error).
    /// If so, exits with the provided error code.
    ///
    /// NOTE: This assumes all syscall parameters (RDI, RSI, RDX, etc.)
    /// are already set up. It only sets RAX to the syscall number.
    pub fn emit_syscall_with_error_check(&mut self, syscall_num: i64, error_exit_code: i64) {
        // Set syscall number
        self.instructions.push(X8664Instr::MovRI64 {
            dest: X86Register::Rax,
            imm: syscall_num,
        });

        // Make the syscall
        self.instructions.push(X8664Instr::Syscall);

        // Check for error (negative return value)
        self.instructions.push(X8664Instr::CmpRI {
            reg: X86Register::Rax,
            imm: 0,
        });

        let success_label =
            self.define_label(&format!("syscall_success_{}", self.instructions.len()));
        self.instructions.push(X8664Instr::JmpCC {
            cc: ConditionCode::GreaterEqual,
            target: LabelRef::Local(success_label),
        });

        // Error path: exit with error code
        self.instructions.push(X8664Instr::MovRI64 {
            dest: X86Register::Rdi,
            imm: error_exit_code,
        });
        self.instructions.push(X8664Instr::MovRI64 {
            dest: X86Register::Rax,
            imm: SYSCALL_EXIT,
        });
        self.instructions.push(X8664Instr::Syscall);

        // Success path continues here
        self.instructions
            .push(X8664Instr::Label { id: success_label });
    }

    /// Direct syscall wrapper for write
    /// Parameters: fd in RDI, buf_reg contains buffer address, len_reg contains length
    pub fn sys_write(&mut self, fd: i64, buf_reg: X86Register, len_reg: X86Register) {
        // Set up parameters
        self.instructions.push(X8664Instr::MovRI64 {
            dest: X86Register::Rdi,
            imm: fd,
        });
        self.instructions.push(X8664Instr::MovRR {
            dest: X86Register::Rsi,
            src: buf_reg,
        });
        self.instructions.push(X8664Instr::MovRR {
            dest: X86Register::Rdx,
            src: len_reg,
        });

        self.emit_syscall_with_error_check(SYSCALL_WRITE, EXIT_CODE_SYSCALL_FAILED);
    }

    /// Direct syscall wrapper for read
    /// Parameters: fd in RDI, buf_reg contains buffer address, len is immediate
    pub fn sys_read(&mut self, fd: i64, buf_reg: X86Register, len: i64) {
        // Set up parameters
        self.instructions.push(X8664Instr::MovRI64 {
            dest: X86Register::Rdi,
            imm: fd,
        });
        self.instructions.push(X8664Instr::MovRR {
            dest: X86Register::Rsi,
            src: buf_reg,
        });
        self.instructions.push(X8664Instr::MovRI64 {
            dest: X86Register::Rdx,
            imm: len,
        });

        // Set up read syscall parameters (no error check for read - caller handles)
        self.instructions.push(X8664Instr::MovRI64 {
            dest: X86Register::Rax,
            imm: SYSCALL_READ,
        });
        self.instructions.push(X8664Instr::Syscall);
    }

    /// Direct syscall wrapper for exit
    /// Parameters: code_reg contains exit code
    pub fn sys_exit(&mut self, code_reg: X86Register) {
        self.instructions.push(X8664Instr::MovRR {
            dest: X86Register::Rdi,
            src: code_reg,
        });
        self.instructions.push(X8664Instr::MovRI64 {
            dest: X86Register::Rax,
            imm: SYSCALL_EXIT,
        });
        self.instructions.push(X8664Instr::Syscall);
    }
}
