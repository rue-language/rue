//! Print functions implementation

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

    // Write newline - we need to put newline character on stack
    ctx.instructions.push(MachineInstr::MovRI64 {
        dest: Register::Rax,
        imm: CHAR_NEWLINE as i64,
    });
    ctx.instructions
        .push(MachineInstr::Push { reg: Register::Rax });

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
        imm: 1, // length
    });
    emit_syscall_with_error_check(ctx, SYSCALL_WRITE, EXIT_CODE_SYSCALL_FAILED);

    // Clean up stack (buffer + 8 bytes for newline)
    ctx.instructions.push(MachineInstr::AddRI {
        dest: Register::Rsp,
        imm: (ITOA_BUFFER_SIZE + 8) as i32,
    });

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
    let print_false = ctx.new_label();
    let print_done = ctx.new_label();

    ctx.instructions.push(MachineInstr::Label {
        id: println_bool_label,
    });

    // Check if RDI is 0 (false) or non-zero (true)
    ctx.instructions.push(MachineInstr::CmpRI {
        reg: Register::Rdi,
        imm: 0,
    });
    ctx.instructions.push(MachineInstr::JmpCC {
        cc: rue_ir::target::ConditionCode::Equal,
        target: rue_ir::target::LabelRef::Local(print_false),
    });

    // Print "true" - put the string on the stack
    ctx.instructions.push(MachineInstr::MovRI64 {
        dest: Register::Rax,
        imm: TRUE_STRING_HEX,
    });
    ctx.instructions
        .push(MachineInstr::Push { reg: Register::Rax });

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
        imm: 4, // length of "true"
    });
    emit_syscall_with_error_check(ctx, SYSCALL_WRITE, EXIT_CODE_SYSCALL_FAILED);

    // Clean up "true" from stack
    ctx.instructions.push(MachineInstr::AddRI {
        dest: Register::Rsp,
        imm: 8,
    });

    ctx.instructions.push(MachineInstr::Jmp {
        target: rue_ir::target::LabelRef::Local(print_done),
    });

    // Print "false"
    ctx.instructions
        .push(MachineInstr::Label { id: print_false });

    // Put "false" on stack
    ctx.instructions.push(MachineInstr::MovRI64 {
        dest: Register::Rax,
        imm: FALSE_STRING_1,
    });
    ctx.instructions
        .push(MachineInstr::Push { reg: Register::Rax });

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
        imm: 5, // length of "false"
    });
    emit_syscall_with_error_check(ctx, SYSCALL_WRITE, EXIT_CODE_SYSCALL_FAILED);

    // Clean up "false" from stack
    ctx.instructions.push(MachineInstr::AddRI {
        dest: Register::Rsp,
        imm: 8,
    });

    ctx.instructions
        .push(MachineInstr::Label { id: print_done });

    // Print newline
    ctx.instructions.push(MachineInstr::MovRI64 {
        dest: Register::Rax,
        imm: CHAR_NEWLINE as i64,
    });
    ctx.instructions
        .push(MachineInstr::Push { reg: Register::Rax });

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
        imm: 1, // length
    });
    emit_syscall_with_error_check(ctx, SYSCALL_WRITE, EXIT_CODE_SYSCALL_FAILED);

    // Clean up newline
    ctx.instructions.push(MachineInstr::AddRI {
        dest: Register::Rsp,
        imm: 8,
    });

    ctx.instructions.push(MachineInstr::Ret);
}

/// Generate println_unit function
pub fn generate_println_unit_function(ctx: &mut RuntimeContext) {
    let println_unit_label = ctx.define_label("__rue_println_unit");

    ctx.instructions.push(MachineInstr::Label {
        id: println_unit_label,
    });

    // Print "()" - put it on stack
    ctx.instructions.push(MachineInstr::MovRI64 {
        dest: Register::Rax,
        imm: UNIT_STRING_HEX,
    });
    ctx.instructions
        .push(MachineInstr::Push { reg: Register::Rax });

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
        imm: 2, // length of "()"
    });
    emit_syscall_with_error_check(ctx, SYSCALL_WRITE, EXIT_CODE_SYSCALL_FAILED);

    // Clean up "()" from stack
    ctx.instructions.push(MachineInstr::AddRI {
        dest: Register::Rsp,
        imm: 8,
    });

    // Print newline
    ctx.instructions.push(MachineInstr::MovRI64 {
        dest: Register::Rax,
        imm: CHAR_NEWLINE as i64,
    });
    ctx.instructions
        .push(MachineInstr::Push { reg: Register::Rax });

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
        imm: 1, // length
    });
    emit_syscall_with_error_check(ctx, SYSCALL_WRITE, EXIT_CODE_SYSCALL_FAILED);

    // Clean up newline
    ctx.instructions.push(MachineInstr::AddRI {
        dest: Register::Rsp,
        imm: 8,
    });

    ctx.instructions.push(MachineInstr::Ret);
}
