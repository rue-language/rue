//! Input function implementation

use crate::constants::*;
use crate::machine_runtime::RuntimeContext;
use rue_ir::target::{MachineInstr, Register};

/// Generate input function
pub fn generate_input_function(ctx: &mut RuntimeContext) {
    let input_label = ctx.define_label("__rue_input");

    ctx.instructions
        .push(MachineInstr::Label { id: input_label });

    // Set up stack frame
    ctx.instructions.push(MachineInstr::EnterFrame);

    // Allocate buffer on stack
    ctx.instructions.push(MachineInstr::AllocStack {
        size: INPUT_BUFFER_SIZE,
    });

    // Read from stdin into buffer
    ctx.instructions.push(MachineInstr::MovRI64 {
        dest: Register::Rdi,
        imm: FD_STDIN,
    });
    ctx.instructions.push(MachineInstr::MovRR {
        dest: Register::Rsi,
        src: Register::Rsp,
    });
    ctx.instructions.push(MachineInstr::MovRI64 {
        dest: Register::Rdx,
        imm: INPUT_BUFFER_SIZE as i64,
    });
    // Set up read syscall parameters
    ctx.instructions.push(MachineInstr::MovRI64 {
        dest: Register::Rax,
        imm: SYSCALL_READ,
    });
    ctx.instructions.push(MachineInstr::Syscall);

    // Check for error (negative return value)
    ctx.instructions.push(MachineInstr::CmpRI {
        reg: Register::Rax,
        imm: 0,
    });

    let read_success = ctx.new_label();
    ctx.instructions.push(MachineInstr::JmpCC {
        cc: rue_ir::target::ConditionCode::GreaterEqual,
        target: rue_ir::target::LabelRef::Local(read_success),
    });

    // Error path: exit with error code
    ctx.instructions.push(MachineInstr::MovRI64 {
        dest: Register::Rdi,
        imm: EXIT_CODE_SYSCALL_FAILED,
    });
    ctx.instructions.push(MachineInstr::MovRI64 {
        dest: Register::Rax,
        imm: SYSCALL_EXIT,
    });
    ctx.instructions.push(MachineInstr::Syscall);

    // Success path
    ctx.instructions
        .push(MachineInstr::Label { id: read_success });

    // RAX now contains number of bytes read
    // Call atoi to parse the input
    ctx.instructions.push(MachineInstr::MovRR {
        dest: Register::Rsi,
        src: Register::Rsp,
    });
    ctx.instructions.push(MachineInstr::MovRR {
        dest: Register::Rdx,
        src: Register::Rax,
    });
    ctx.instructions.push(MachineInstr::Call {
        target: "__rue_atoi".to_string(),
    });

    // Clean up stack and leave frame
    ctx.instructions.push(MachineInstr::LeaveFrame);

    // RAX contains the parsed number
    ctx.instructions.push(MachineInstr::Ret);
}
