//! Print functions implementation

#[allow(unused_imports)]
use crate::constants::*;
use crate::helpers::*;
use crate::machine_runtime::RuntimeContext;
use rue_ir::target::{MachineInstr, Register};

/// Generate println_i64 function
pub fn generate_println_i64_function(ctx: &mut RuntimeContext) {
    let println_i64_label = ctx.define_label("__rue_println_i64");

    ctx.instructions.push(MachineInstr::Label {
        id: println_i64_label,
    });

    // Set up stack frame
    ctx.instructions.push(MachineInstr::EnterFrame);

    // Allocate space for number buffer
    ctx.instructions.push(MachineInstr::AllocStack {
        size: ITOA_BUFFER_SIZE,
    });

    // Call itoa to convert number to string
    // RSI = buffer pointer (use stack)
    ctx.instructions.push(MachineInstr::MovRR {
        dest: Register::Rsi,
        src: Register::Rsp,
    });
    ctx.instructions.push(MachineInstr::Call {
        target: "__rue_itoa".to_string(),
    });

    // RAX now contains number of digits
    // Write the digits with error checking
    ctx.instructions.push(MachineInstr::MovRI64 {
        dest: Register::Rdi,
        imm: FD_STDOUT,
    });
    ctx.instructions.push(MachineInstr::MovRR {
        dest: Register::Rsi,
        src: Register::Rsp,
    });
    ctx.instructions.push(MachineInstr::MovRR {
        dest: Register::Rdx,
        src: Register::Rax,
    });
    emit_syscall_with_error_check(ctx, SYSCALL_WRITE, EXIT_CODE_SYSCALL_FAILED);

    // Write newline - use static newline from data section
    ctx.instructions.push(MachineInstr::MovRI64 {
        dest: Register::Rdi,
        imm: FD_STDOUT,
    });
    ctx.instructions.push(MachineInstr::LeaLabel {
        dest: Register::Rsi,
        label: "__rue_newline".to_string(),
    });
    ctx.instructions.push(MachineInstr::MovRI64 {
        dest: Register::Rdx,
        imm: 1, // length
    });
    emit_syscall_with_error_check(ctx, SYSCALL_WRITE, EXIT_CODE_SYSCALL_FAILED);

    // Clean up stack and leave frame
    ctx.instructions.push(MachineInstr::LeaveFrame);

    ctx.instructions.push(MachineInstr::Ret);
}

/// Generate println_i32 function
pub fn generate_println_i32_function(ctx: &mut RuntimeContext) {
    let println_i32_label = ctx.define_label("__rue_println_i32");

    ctx.instructions.push(MachineInstr::Label {
        id: println_i32_label,
    });

    // Sign-extend i32 to i64
    // Input is in EDI (lower 32 bits of RDI)
    // We need to sign-extend properly
    ctx.instructions.push(MachineInstr::MovRR {
        dest: Register::Rax,
        src: Register::Rdi,
    });

    // Use shift left then arithmetic shift right to sign extend
    ctx.instructions.push(MachineInstr::MovRI64 {
        dest: Register::Rcx,
        imm: 32,
    });
    ctx.instructions.push(MachineInstr::Shl {
        dest: Register::Rax,
        count: Register::Rcx,
    });
    ctx.instructions.push(MachineInstr::Sar {
        dest: Register::Rax,
        count: Register::Rcx,
    });
    ctx.instructions.push(MachineInstr::MovRR {
        dest: Register::Rdi,
        src: Register::Rax,
    });

    // Call println_i64
    ctx.instructions.push(MachineInstr::Call {
        target: "__rue_println_i64".to_string(),
    });
    ctx.instructions.push(MachineInstr::Ret);
}

/// Generate println_bool function
pub fn generate_println_bool_function(ctx: &mut RuntimeContext) {
    let println_bool_label = ctx.define_label("__rue_println_bool");
    let print_false_label = ctx.new_label();
    let print_done_label = ctx.new_label();

    ctx.instructions.push(MachineInstr::Label {
        id: println_bool_label,
    });

    // Set up stack frame
    ctx.instructions.push(MachineInstr::EnterFrame);

    // Check if RDI is 0 (false) or non-zero (true)
    ctx.instructions.push(MachineInstr::CmpRI {
        reg: Register::Rdi,
        imm: 0,
    });
    ctx.instructions.push(MachineInstr::JmpCC {
        cc: rue_ir::target::ConditionCode::Equal,
        target: rue_ir::target::LabelRef::Local(print_false_label),
    });

    // Print "true" - use static string from data section
    ctx.instructions.push(MachineInstr::MovRI64 {
        dest: Register::Rdi,
        imm: FD_STDOUT,
    });
    ctx.instructions.push(MachineInstr::LeaLabel {
        dest: Register::Rsi,
        label: "__rue_true_str".to_string(),
    });
    ctx.instructions.push(MachineInstr::MovRI64 {
        dest: Register::Rdx,
        imm: 4, // length of "true"
    });
    emit_syscall_with_error_check(ctx, SYSCALL_WRITE, EXIT_CODE_SYSCALL_FAILED);

    ctx.instructions.push(MachineInstr::Jmp {
        target: rue_ir::target::LabelRef::Local(print_done_label),
    });

    // Print "false"
    ctx.instructions.push(MachineInstr::Label {
        id: print_false_label,
    });

    // Print "false" - use static string from data section
    ctx.instructions.push(MachineInstr::MovRI64 {
        dest: Register::Rdi,
        imm: FD_STDOUT,
    });
    ctx.instructions.push(MachineInstr::LeaLabel {
        dest: Register::Rsi,
        label: "__rue_false_str".to_string(),
    });
    ctx.instructions.push(MachineInstr::MovRI64 {
        dest: Register::Rdx,
        imm: 5, // length of "false"
    });
    emit_syscall_with_error_check(ctx, SYSCALL_WRITE, EXIT_CODE_SYSCALL_FAILED);

    ctx.instructions.push(MachineInstr::Label {
        id: print_done_label,
    });

    // Print newline - use static newline from data section
    ctx.instructions.push(MachineInstr::MovRI64 {
        dest: Register::Rdi,
        imm: FD_STDOUT,
    });
    ctx.instructions.push(MachineInstr::LeaLabel {
        dest: Register::Rsi,
        label: "__rue_newline".to_string(),
    });
    ctx.instructions.push(MachineInstr::MovRI64 {
        dest: Register::Rdx,
        imm: 1, // length
    });
    emit_syscall_with_error_check(ctx, SYSCALL_WRITE, EXIT_CODE_SYSCALL_FAILED);

    // Clean up stack and leave frame
    ctx.instructions.push(MachineInstr::LeaveFrame);

    ctx.instructions.push(MachineInstr::Ret);
}

/// Generate println_unit function
pub fn generate_println_unit_function(ctx: &mut RuntimeContext) {
    let println_unit_label = ctx.define_label("__rue_println_unit");

    ctx.instructions.push(MachineInstr::Label {
        id: println_unit_label,
    });

    // Set up stack frame
    ctx.instructions.push(MachineInstr::EnterFrame);

    // Print "()" - use static string from data section
    ctx.instructions.push(MachineInstr::MovRI64 {
        dest: Register::Rdi,
        imm: FD_STDOUT,
    });
    ctx.instructions.push(MachineInstr::LeaLabel {
        dest: Register::Rsi,
        label: "__rue_unit_str".to_string(),
    });
    ctx.instructions.push(MachineInstr::MovRI64 {
        dest: Register::Rdx,
        imm: 2, // length of "()"
    });
    emit_syscall_with_error_check(ctx, SYSCALL_WRITE, EXIT_CODE_SYSCALL_FAILED);

    // Print newline - use static newline from data section
    ctx.instructions.push(MachineInstr::MovRI64 {
        dest: Register::Rdi,
        imm: FD_STDOUT,
    });
    ctx.instructions.push(MachineInstr::LeaLabel {
        dest: Register::Rsi,
        label: "__rue_newline".to_string(),
    });
    ctx.instructions.push(MachineInstr::MovRI64 {
        dest: Register::Rdx,
        imm: 1, // length
    });
    emit_syscall_with_error_check(ctx, SYSCALL_WRITE, EXIT_CODE_SYSCALL_FAILED);

    // Clean up stack and leave frame
    ctx.instructions.push(MachineInstr::LeaveFrame);

    ctx.instructions.push(MachineInstr::Ret);
}
