//! Helper functions to reduce code duplication in runtime functions
//!
//! This module provides common patterns used across runtime functions
//! to improve maintainability and reduce code duplication.

use crate::constants::*;
use crate::machine_runtime::RuntimeContext;
use rue_ir::target::{MachineInstr, Register};

/// Emit instructions to write a newline character to stdout
pub fn emit_write_newline(ctx: &mut RuntimeContext) {
    // Push newline character on stack
    ctx.instructions.push(MachineInstr::MovRI64 {
        dest: Register::Rax,
        imm: CHAR_NEWLINE as i64,
    });
    ctx.instructions
        .push(MachineInstr::Push { reg: Register::Rax });

    // Setup write syscall with error checking
    ctx.instructions.push(MachineInstr::MovRI64 {
        dest: Register::Rdi,
        imm: FD_STDOUT,
    });
    ctx.instructions.push(MachineInstr::MovRR {
        dest: Register::Rsi,
        src: Register::Rsp,
    });
    ctx.instructions.push(MachineInstr::MovRI64 {
        dest: Register::Rdx,
        imm: 1, // Write 1 byte
    });
    emit_syscall_with_error_check(ctx, SYSCALL_WRITE, EXIT_CODE_SYSCALL_FAILED);

    // Clean up stack
    ctx.instructions.push(MachineInstr::AddRI {
        dest: Register::Rsp,
        imm: 8,
    });
}

/// Emit a syscall with error checking
///
/// After the syscall, checks if the return value is negative (error).
/// If so, exits with the provided error code.
///
/// NOTE: This assumes all syscall parameters (RDI, RSI, RDX, etc.)
/// are already set up. It only sets RAX to the syscall number.
pub fn emit_syscall_with_error_check(
    ctx: &mut RuntimeContext,
    syscall_num: i64,
    error_exit_code: i64,
) {
    // Set syscall number
    ctx.instructions.push(MachineInstr::MovRI64 {
        dest: Register::Rax,
        imm: syscall_num,
    });

    // Make the syscall
    ctx.instructions.push(MachineInstr::Syscall);

    // Check for error (negative return value)
    ctx.instructions.push(MachineInstr::CmpRI {
        reg: Register::Rax,
        imm: 0,
    });

    let success_label = ctx.define_label(&format!("syscall_success_{}", ctx.instructions.len()));
    ctx.instructions.push(MachineInstr::JmpCC {
        cc: rue_ir::target::ConditionCode::GreaterEqual,
        target: rue_ir::target::LabelRef::Local(success_label),
    });

    // Error path: exit with error code
    ctx.instructions.push(MachineInstr::MovRI64 {
        dest: Register::Rdi,
        imm: error_exit_code,
    });
    ctx.instructions.push(MachineInstr::MovRI64 {
        dest: Register::Rax,
        imm: SYSCALL_EXIT,
    });
    ctx.instructions.push(MachineInstr::Syscall);

    // Success path continues here
    ctx.instructions
        .push(MachineInstr::Label { id: success_label });
}

/// Emit instructions to write a string to stdout
///
/// The string should already be on the stack or in memory.
/// This function sets up the write syscall with the given length.
pub fn emit_write_string(ctx: &mut RuntimeContext, length: i64) {
    ctx.instructions.push(MachineInstr::MovRI64 {
        dest: Register::Rax,
        imm: SYSCALL_WRITE,
    });
    ctx.instructions.push(MachineInstr::MovRI64 {
        dest: Register::Rdi,
        imm: FD_STDOUT,
    });
    ctx.instructions.push(MachineInstr::MovRR {
        dest: Register::Rsi,
        src: Register::Rsp,
    });
    ctx.instructions.push(MachineInstr::MovRI64 {
        dest: Register::Rdx,
        imm: length,
    });
    ctx.instructions.push(MachineInstr::Syscall);
}

/// Emit instructions to setup a write syscall with error checking
///
/// Combines emit_write_string with error checking.
pub fn emit_write_string_with_error_check(ctx: &mut RuntimeContext, length: i64) {
    ctx.instructions.push(MachineInstr::MovRI64 {
        dest: Register::Rdi,
        imm: FD_STDOUT,
    });
    ctx.instructions.push(MachineInstr::MovRR {
        dest: Register::Rsi,
        src: Register::Rsp,
    });
    ctx.instructions.push(MachineInstr::MovRI64 {
        dest: Register::Rdx,
        imm: length,
    });

    emit_syscall_with_error_check(ctx, SYSCALL_WRITE, EXIT_CODE_SYSCALL_FAILED);
}

/// Emit instructions to clear a buffer
///
/// Uses rep stosb to efficiently zero out memory
pub fn emit_clear_buffer(ctx: &mut RuntimeContext, size: i64) {
    // RDI should already point to the buffer
    ctx.instructions.push(MachineInstr::MovRI64 {
        dest: Register::Rax,
        imm: 0,
    });
    ctx.instructions.push(MachineInstr::MovRI64 {
        dest: Register::Rcx,
        imm: size,
    });
    ctx.instructions.push(MachineInstr::Cld);
    ctx.instructions.push(MachineInstr::RepStosb);
}
