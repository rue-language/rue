//! Signal handler implementations

use crate::constants::*;
use crate::machine_runtime::RuntimeContext;
use rue_ir::target::{MachineInstr, Register};

/// Generate signal handlers
pub fn generate_signal_handlers(ctx: &mut RuntimeContext) {
    let sigfpe_handler_label = ctx.define_label("__rue_sigfpe_handler");
    let sigsegv_handler_label = ctx.define_label("__rue_sigsegv_handler");

    // SIGFPE handler
    ctx.instructions.push(MachineInstr::Label {
        id: sigfpe_handler_label,
    });
    ctx.instructions.push(MachineInstr::MovRI64 {
        dest: Register::Rdi,
        imm: EXIT_CODE_DIV_BY_ZERO,
    });
    ctx.instructions.push(MachineInstr::MovRI64 {
        dest: Register::Rax,
        imm: SYSCALL_EXIT,
    });
    ctx.instructions.push(MachineInstr::Syscall);

    // SIGSEGV handler
    ctx.instructions.push(MachineInstr::Label {
        id: sigsegv_handler_label,
    });
    ctx.instructions.push(MachineInstr::MovRI64 {
        dest: Register::Rdi,
        imm: EXIT_CODE_STACK_OVERFLOW,
    });
    ctx.instructions.push(MachineInstr::MovRI64 {
        dest: Register::Rax,
        imm: SYSCALL_EXIT,
    });
    ctx.instructions.push(MachineInstr::Syscall);
}

/// Generate setup_signal_handlers function
pub fn generate_setup_signal_handlers(ctx: &mut RuntimeContext) {
    let setup_label = ctx.define_label("__rue_setup_signal_handlers");

    ctx.instructions
        .push(MachineInstr::Label { id: setup_label });

    // Set up stack frame
    ctx.instructions.push(MachineInstr::EnterFrame);

    // Allocate space for sigaction structs
    ctx.instructions.push(MachineInstr::AllocStack {
        size: 2 * SIGACTION_STRUCT_SIZE,
    });

    // Setup SIGFPE handler
    // Clear the sigaction struct
    ctx.instructions.push(MachineInstr::MovRR {
        dest: Register::Rdi,
        src: Register::Rsp,
    });
    ctx.instructions.push(MachineInstr::MovRI64 {
        dest: Register::Rax,
        imm: 0,
    });
    ctx.instructions.push(MachineInstr::MovRI64 {
        dest: Register::Rcx,
        imm: SIGACTION_STRUCT_SIZE as i64,
    });
    ctx.instructions.push(MachineInstr::Cld);
    ctx.instructions.push(MachineInstr::RepStosb);

    // Set sa_handler
    ctx.instructions.push(MachineInstr::LeaLabel {
        dest: Register::Rax,
        label: "__rue_sigfpe_handler".to_string(),
    });
    ctx.instructions.push(MachineInstr::MovMR {
        base: Register::Rsp,
        offset: 0,
        src: Register::Rax,
    });

    // Set sa_flags = SA_RESTORER
    ctx.instructions.push(MachineInstr::MovRI64 {
        dest: Register::Rax,
        imm: SA_RESTORER,
    });
    ctx.instructions.push(MachineInstr::MovMR {
        base: Register::Rsp,
        offset: 136,
        src: Register::Rax,
    });

    // Call sigaction
    ctx.instructions.push(MachineInstr::MovRI64 {
        dest: Register::Rdi,
        imm: SIGFPE,
    });
    ctx.instructions.push(MachineInstr::MovRR {
        dest: Register::Rsi,
        src: Register::Rsp,
    });
    ctx.instructions.push(MachineInstr::MovRI64 {
        dest: Register::Rdx,
        imm: 0, // NULL for oldact
    });
    ctx.instructions.push(MachineInstr::MovRI64 {
        dest: Register::R10,
        imm: SIGSET_SIZE,
    });
    ctx.instructions.push(MachineInstr::MovRI64 {
        dest: Register::Rax,
        imm: SYSCALL_RT_SIGACTION,
    });
    ctx.instructions.push(MachineInstr::Syscall);

    // Setup SIGSEGV handler - reuse the same struct
    ctx.instructions.push(MachineInstr::LeaLabel {
        dest: Register::Rax,
        label: "__rue_sigsegv_handler".to_string(),
    });
    ctx.instructions.push(MachineInstr::MovMR {
        base: Register::Rsp,
        offset: 0,
        src: Register::Rax,
    });

    // Call sigaction
    ctx.instructions.push(MachineInstr::MovRI64 {
        dest: Register::Rdi,
        imm: SIGSEGV,
    });
    ctx.instructions.push(MachineInstr::MovRR {
        dest: Register::Rsi,
        src: Register::Rsp,
    });
    ctx.instructions.push(MachineInstr::MovRI64 {
        dest: Register::Rdx,
        imm: 0, // NULL for oldact
    });
    ctx.instructions.push(MachineInstr::MovRI64 {
        dest: Register::R10,
        imm: SIGSET_SIZE,
    });
    ctx.instructions.push(MachineInstr::MovRI64 {
        dest: Register::Rax,
        imm: SYSCALL_RT_SIGACTION,
    });
    ctx.instructions.push(MachineInstr::Syscall);

    // Clean up stack and leave frame
    ctx.instructions.push(MachineInstr::LeaveFrame);

    ctx.instructions.push(MachineInstr::Ret);
}
