//! String conversion functions (itoa, atoi)

use crate::constants::*;
use crate::machine_runtime::RuntimeContext;
use rue_ir::target::{ConditionCode, LabelRef, MachineInstr, Register};

/// Generate itoa helper function
/// Input: RDI = number to convert
///        RSI = buffer pointer (must have at least 20 bytes)
/// Output: RAX = number of digits written
///         Buffer contains ASCII digits (no null terminator)
pub fn generate_itoa_function(ctx: &mut RuntimeContext) {
    let itoa_label = ctx.define_label("__rue_itoa");
    let negative_label = ctx.new_label();
    let positive_label = ctx.new_label();
    let digit_loop = ctx.new_label();
    let reverse_start = ctx.new_label();
    let reverse_loop = ctx.new_label();
    let reverse_done = ctx.new_label();
    let itoa_done = ctx.new_label();

    ctx.instructions
        .push(MachineInstr::Label { id: itoa_label });

    // Save registers
    ctx.instructions
        .push(MachineInstr::Push { reg: Register::Rbx });
    ctx.instructions
        .push(MachineInstr::Push { reg: Register::Rcx });
    ctx.instructions
        .push(MachineInstr::Push { reg: Register::Rdx });
    ctx.instructions
        .push(MachineInstr::Push { reg: Register::R8 });
    ctx.instructions
        .push(MachineInstr::Push { reg: Register::R9 });
    ctx.instructions
        .push(MachineInstr::Push { reg: Register::R10 });
    ctx.instructions
        .push(MachineInstr::Push { reg: Register::R11 });

    // R8 = output buffer
    ctx.instructions.push(MachineInstr::MovRR {
        dest: Register::R8,
        src: Register::Rsi,
    });

    // RBX = number to convert
    ctx.instructions.push(MachineInstr::MovRR {
        dest: Register::Rbx,
        src: Register::Rdi,
    });

    // R9 = digit count (starts at 0)
    ctx.instructions.push(MachineInstr::MovRI64 {
        dest: Register::R9,
        imm: 0,
    });

    // Check if negative
    ctx.instructions.push(MachineInstr::CmpRI {
        reg: Register::Rbx,
        imm: 0,
    });
    ctx.instructions.push(MachineInstr::JmpCC {
        cc: ConditionCode::Less,
        target: LabelRef::Local(negative_label),
    });
    ctx.instructions.push(MachineInstr::Jmp {
        target: LabelRef::Local(positive_label),
    });

    // Handle negative numbers
    ctx.instructions
        .push(MachineInstr::Label { id: negative_label });
    // Write '-' to buffer
    ctx.instructions.push(MachineInstr::MovRI64 {
        dest: Register::Rax,
        imm: CHAR_MINUS as i64,
    });
    ctx.instructions.push(MachineInstr::MovMR8 {
        base: Register::R8,
        offset: 0,
        src: Register::Rax,
    });
    // Increment buffer pointer
    ctx.instructions.push(MachineInstr::AddRI {
        dest: Register::R8,
        imm: 1,
    });
    // Increment digit count
    ctx.instructions.push(MachineInstr::AddRI {
        dest: Register::R9,
        imm: 1,
    });

    // Handle special case of i64::MIN (-9223372036854775808)
    ctx.instructions.push(MachineInstr::MovRI64 {
        dest: Register::Rax,
        imm: i64::MIN,
    });
    ctx.instructions.push(MachineInstr::CmpRR {
        left: Register::Rbx,
        right: Register::Rax,
    });

    let not_min_label = ctx.new_label();
    ctx.instructions.push(MachineInstr::JmpCC {
        cc: ConditionCode::NotEqual,
        target: LabelRef::Local(not_min_label),
    });

    // Special handling for i64::MIN - write the literal string
    let min_digits = "9223372036854775808";
    for (i, ch) in min_digits.chars().enumerate() {
        ctx.instructions.push(MachineInstr::MovRI64 {
            dest: Register::Rax,
            imm: ch as i64,
        });
        ctx.instructions.push(MachineInstr::MovMR8 {
            base: Register::R8,
            offset: i as i32,
            src: Register::Rax,
        });
    }
    ctx.instructions.push(MachineInstr::AddRI {
        dest: Register::R9,
        imm: min_digits.len() as i32,
    });
    ctx.instructions.push(MachineInstr::Jmp {
        target: LabelRef::Local(itoa_done),
    });

    ctx.instructions
        .push(MachineInstr::Label { id: not_min_label });
    // Negate the number (two's complement)
    ctx.instructions.push(MachineInstr::MovRI64 {
        dest: Register::Rax,
        imm: 0,
    });
    ctx.instructions.push(MachineInstr::SubRR {
        dest: Register::Rax,
        src: Register::Rbx,
    });
    ctx.instructions.push(MachineInstr::MovRR {
        dest: Register::Rbx,
        src: Register::Rax,
    });

    // Process positive number
    ctx.instructions
        .push(MachineInstr::Label { id: positive_label });

    // R10 = temporary buffer pointer for digit extraction
    ctx.instructions.push(MachineInstr::MovRR {
        dest: Register::R10,
        src: Register::R8,
    });

    // Extract digits in reverse order
    // This approach is simpler and more efficient than calculating the number
    // of digits first. The reversal step is a simple O(n/2) operation.
    ctx.instructions
        .push(MachineInstr::Label { id: digit_loop });

    // RAX = RBX / 10, RDX = RBX % 10
    ctx.instructions.push(MachineInstr::MovRR {
        dest: Register::Rax,
        src: Register::Rbx,
    });
    ctx.instructions.push(MachineInstr::MovRI64 {
        dest: Register::Rdx,
        imm: 0,
    });
    ctx.instructions.push(MachineInstr::MovRI64 {
        dest: Register::Rcx,
        imm: 10,
    });
    ctx.instructions.push(MachineInstr::Idiv {
        divisor: Register::Rcx,
    });

    // Convert digit to ASCII (add '0')
    ctx.instructions.push(MachineInstr::AddRI {
        dest: Register::Rdx,
        imm: CHAR_ZERO as i32,
    });

    // Store digit
    ctx.instructions.push(MachineInstr::MovMR8 {
        base: Register::R10,
        offset: 0,
        src: Register::Rdx,
    });
    ctx.instructions.push(MachineInstr::AddRI {
        dest: Register::R10,
        imm: 1,
    });
    ctx.instructions.push(MachineInstr::AddRI {
        dest: Register::R9,
        imm: 1,
    });

    // Update RBX = quotient for next iteration
    ctx.instructions.push(MachineInstr::MovRR {
        dest: Register::Rbx,
        src: Register::Rax,
    });

    // Continue if quotient != 0
    ctx.instructions.push(MachineInstr::CmpRI {
        reg: Register::Rbx,
        imm: 0,
    });
    ctx.instructions.push(MachineInstr::JmpCC {
        cc: ConditionCode::NotEqual,
        target: LabelRef::Local(digit_loop),
    });

    // Now reverse the digits in place
    ctx.instructions
        .push(MachineInstr::Label { id: reverse_start });

    // R10 = end pointer (one past last digit)
    // R11 = start pointer
    ctx.instructions.push(MachineInstr::MovRR {
        dest: Register::R11,
        src: Register::R8,
    });
    ctx.instructions.push(MachineInstr::SubRI {
        dest: Register::R10,
        imm: 1,
    });

    ctx.instructions
        .push(MachineInstr::Label { id: reverse_loop });
    ctx.instructions.push(MachineInstr::CmpRR {
        left: Register::R11,
        right: Register::R10,
    });
    ctx.instructions.push(MachineInstr::JmpCC {
        cc: ConditionCode::GreaterEqual,
        target: LabelRef::Local(reverse_done),
    });

    // Swap characters at R11 and R10
    ctx.instructions.push(MachineInstr::MovRM8 {
        dest: Register::Rax,
        base: Register::R11,
        offset: 0,
    });
    ctx.instructions.push(MachineInstr::MovRM8 {
        dest: Register::Rbx,
        base: Register::R10,
        offset: 0,
    });
    ctx.instructions.push(MachineInstr::MovMR8 {
        base: Register::R11,
        offset: 0,
        src: Register::Rbx,
    });
    ctx.instructions.push(MachineInstr::MovMR8 {
        base: Register::R10,
        offset: 0,
        src: Register::Rax,
    });

    // Move pointers inward
    ctx.instructions.push(MachineInstr::AddRI {
        dest: Register::R11,
        imm: 1,
    });
    ctx.instructions.push(MachineInstr::SubRI {
        dest: Register::R10,
        imm: 1,
    });
    ctx.instructions.push(MachineInstr::Jmp {
        target: LabelRef::Local(reverse_loop),
    });

    ctx.instructions
        .push(MachineInstr::Label { id: reverse_done });

    ctx.instructions.push(MachineInstr::Label { id: itoa_done });
    // Return digit count in RAX
    ctx.instructions.push(MachineInstr::MovRR {
        dest: Register::Rax,
        src: Register::R9,
    });

    // Restore registers
    ctx.instructions
        .push(MachineInstr::Pop { reg: Register::R11 });
    ctx.instructions
        .push(MachineInstr::Pop { reg: Register::R10 });
    ctx.instructions
        .push(MachineInstr::Pop { reg: Register::R9 });
    ctx.instructions
        .push(MachineInstr::Pop { reg: Register::R8 });
    ctx.instructions
        .push(MachineInstr::Pop { reg: Register::Rdx });
    ctx.instructions
        .push(MachineInstr::Pop { reg: Register::Rcx });
    ctx.instructions
        .push(MachineInstr::Pop { reg: Register::Rbx });

    ctx.instructions.push(MachineInstr::Ret);
}

/// Generate atoi helper function
/// Input: RSI = buffer pointer, RDX = buffer length
/// Output: RAX = parsed integer (0 on error)
pub fn generate_atoi_function(ctx: &mut RuntimeContext) {
    let atoi_label = ctx.define_label("__rue_atoi");
    let skip_whitespace_loop = ctx.new_label();
    let skip_whitespace_done = ctx.new_label();
    let skip_whitespace_next = ctx.new_label();
    let check_sign = ctx.new_label();
    let negative_number = ctx.new_label();
    let parse_digits = ctx.new_label();
    let digit_loop = ctx.new_label();
    let invalid_char = ctx.new_label();
    let apply_sign = ctx.new_label();
    let atoi_done = ctx.new_label();

    ctx.instructions
        .push(MachineInstr::Label { id: atoi_label });

    // Save registers
    ctx.instructions
        .push(MachineInstr::Push { reg: Register::Rbx });
    ctx.instructions
        .push(MachineInstr::Push { reg: Register::Rcx });
    ctx.instructions
        .push(MachineInstr::Push { reg: Register::R8 });
    ctx.instructions
        .push(MachineInstr::Push { reg: Register::R9 });
    ctx.instructions
        .push(MachineInstr::Push { reg: Register::R10 });
    ctx.instructions
        .push(MachineInstr::Push { reg: Register::R11 });

    // R8 = buffer pointer
    ctx.instructions.push(MachineInstr::MovRR {
        dest: Register::R8,
        src: Register::Rsi,
    });

    // R9 = remaining length
    ctx.instructions.push(MachineInstr::MovRR {
        dest: Register::R9,
        src: Register::Rdx,
    });

    // Skip leading whitespace
    ctx.instructions.push(MachineInstr::Label {
        id: skip_whitespace_loop,
    });
    ctx.instructions.push(MachineInstr::CmpRI {
        reg: Register::R9,
        imm: 0,
    });
    ctx.instructions.push(MachineInstr::JmpCC {
        cc: ConditionCode::Equal,
        target: LabelRef::Local(skip_whitespace_done),
    });

    // Load character
    ctx.instructions.push(MachineInstr::MovRM8 {
        dest: Register::Rax,
        base: Register::R8,
        offset: 0,
    });

    // Check for whitespace (space, tab, newline, etc.)
    ctx.instructions.push(MachineInstr::CmpRI {
        reg: Register::Rax,
        imm: 32, // space
    });
    ctx.instructions.push(MachineInstr::JmpCC {
        cc: ConditionCode::Equal,
        target: LabelRef::Local(skip_whitespace_next),
    });
    ctx.instructions.push(MachineInstr::CmpRI {
        reg: Register::Rax,
        imm: 9, // tab
    });
    ctx.instructions.push(MachineInstr::JmpCC {
        cc: ConditionCode::Equal,
        target: LabelRef::Local(skip_whitespace_next),
    });
    ctx.instructions.push(MachineInstr::CmpRI {
        reg: Register::Rax,
        imm: 10, // newline
    });
    ctx.instructions.push(MachineInstr::JmpCC {
        cc: ConditionCode::Equal,
        target: LabelRef::Local(skip_whitespace_next),
    });
    ctx.instructions.push(MachineInstr::CmpRI {
        reg: Register::Rax,
        imm: 13, // carriage return
    });
    ctx.instructions.push(MachineInstr::JmpCC {
        cc: ConditionCode::Equal,
        target: LabelRef::Local(skip_whitespace_next),
    });

    // Not whitespace, continue
    ctx.instructions.push(MachineInstr::Jmp {
        target: LabelRef::Local(skip_whitespace_done),
    });

    // Actually skip the whitespace character
    ctx.instructions.push(MachineInstr::Label {
        id: skip_whitespace_next,
    });
    ctx.instructions.push(MachineInstr::AddRI {
        dest: Register::R8,
        imm: 1,
    });
    ctx.instructions.push(MachineInstr::SubRI {
        dest: Register::R9,
        imm: 1,
    });
    ctx.instructions.push(MachineInstr::Jmp {
        target: LabelRef::Local(skip_whitespace_loop),
    });

    ctx.instructions.push(MachineInstr::Label {
        id: skip_whitespace_done,
    });

    // Check for empty string after whitespace
    ctx.instructions.push(MachineInstr::CmpRI {
        reg: Register::R9,
        imm: 0,
    });
    ctx.instructions.push(MachineInstr::JmpCC {
        cc: ConditionCode::Equal,
        target: LabelRef::Local(invalid_char),
    });

    // R10 = sign (1 for positive, -1 for negative)
    ctx.instructions.push(MachineInstr::MovRI64 {
        dest: Register::R10,
        imm: 1,
    });

    // Check for sign
    ctx.instructions
        .push(MachineInstr::Label { id: check_sign });
    ctx.instructions.push(MachineInstr::MovRM8 {
        dest: Register::Rax,
        base: Register::R8,
        offset: 0,
    });

    ctx.instructions.push(MachineInstr::CmpRI {
        reg: Register::Rax,
        imm: CHAR_MINUS as i32,
    });
    ctx.instructions.push(MachineInstr::JmpCC {
        cc: ConditionCode::Equal,
        target: LabelRef::Local(negative_number),
    });

    ctx.instructions.push(MachineInstr::CmpRI {
        reg: Register::Rax,
        imm: 43, // '+'
    });
    ctx.instructions.push(MachineInstr::JmpCC {
        cc: ConditionCode::NotEqual,
        target: LabelRef::Local(parse_digits),
    });

    // Skip '+' sign
    ctx.instructions.push(MachineInstr::AddRI {
        dest: Register::R8,
        imm: 1,
    });
    ctx.instructions.push(MachineInstr::SubRI {
        dest: Register::R9,
        imm: 1,
    });
    ctx.instructions.push(MachineInstr::Jmp {
        target: LabelRef::Local(parse_digits),
    });

    ctx.instructions.push(MachineInstr::Label {
        id: negative_number,
    });
    ctx.instructions.push(MachineInstr::MovRI64 {
        dest: Register::R10,
        imm: -1i64,
    });
    ctx.instructions.push(MachineInstr::AddRI {
        dest: Register::R8,
        imm: 1,
    });
    ctx.instructions.push(MachineInstr::SubRI {
        dest: Register::R9,
        imm: 1,
    });

    // Parse digits
    ctx.instructions
        .push(MachineInstr::Label { id: parse_digits });
    ctx.instructions.push(MachineInstr::MovRI64 {
        dest: Register::Rbx,
        imm: 0, // accumulator
    });

    ctx.instructions
        .push(MachineInstr::Label { id: digit_loop });
    ctx.instructions.push(MachineInstr::CmpRI {
        reg: Register::R9,
        imm: 0,
    });
    ctx.instructions.push(MachineInstr::JmpCC {
        cc: ConditionCode::Equal,
        target: LabelRef::Local(apply_sign),
    });

    // Load character
    ctx.instructions.push(MachineInstr::MovRM8 {
        dest: Register::Rax,
        base: Register::R8,
        offset: 0,
    });

    // Check if it's a digit (0-9)
    ctx.instructions.push(MachineInstr::CmpRI {
        reg: Register::Rax,
        imm: CHAR_ZERO as i32, // '0'
    });
    ctx.instructions.push(MachineInstr::JmpCC {
        cc: ConditionCode::Less,
        target: LabelRef::Local(apply_sign),
    });
    ctx.instructions.push(MachineInstr::CmpRI {
        reg: Register::Rax,
        imm: 57, // '9'
    });
    ctx.instructions.push(MachineInstr::JmpCC {
        cc: ConditionCode::Greater,
        target: LabelRef::Local(apply_sign),
    });

    // Convert ASCII to digit
    ctx.instructions.push(MachineInstr::SubRI {
        dest: Register::Rax,
        imm: CHAR_ZERO as i32,
    });

    // Simple overflow check - if accumulator is already very large, return 0
    // Check if RBX > 922337203685477580 (i64::MAX / 10)
    ctx.instructions.push(MachineInstr::MovRI64 {
        dest: Register::Rcx,
        imm: 922337203685477580,
    });
    ctx.instructions.push(MachineInstr::CmpRR {
        left: Register::Rbx,
        right: Register::Rcx,
    });

    let no_overflow = ctx.new_label();
    ctx.instructions.push(MachineInstr::JmpCC {
        cc: ConditionCode::LessEqual,
        target: LabelRef::Local(no_overflow),
    });

    // Overflow - return 0
    ctx.instructions.push(MachineInstr::MovRI64 {
        dest: Register::Rax,
        imm: 0,
    });
    ctx.instructions.push(MachineInstr::Jmp {
        target: LabelRef::Local(atoi_done),
    });

    ctx.instructions
        .push(MachineInstr::Label { id: no_overflow });

    // Multiply accumulator by 10
    ctx.instructions.push(MachineInstr::ImulRI {
        dest: Register::Rbx,
        imm: 10,
    });

    // Add new digit
    ctx.instructions.push(MachineInstr::AddRR {
        dest: Register::Rbx,
        src: Register::Rax,
    });

    // Next character
    ctx.instructions.push(MachineInstr::AddRI {
        dest: Register::R8,
        imm: 1,
    });
    ctx.instructions.push(MachineInstr::SubRI {
        dest: Register::R9,
        imm: 1,
    });
    ctx.instructions.push(MachineInstr::Jmp {
        target: LabelRef::Local(digit_loop),
    });

    // Invalid character - return 0
    ctx.instructions
        .push(MachineInstr::Label { id: invalid_char });
    ctx.instructions.push(MachineInstr::MovRI64 {
        dest: Register::Rax,
        imm: 0,
    });
    ctx.instructions.push(MachineInstr::Jmp {
        target: LabelRef::Local(atoi_done),
    });

    // Apply sign
    ctx.instructions
        .push(MachineInstr::Label { id: apply_sign });
    ctx.instructions.push(MachineInstr::MovRR {
        dest: Register::Rax,
        src: Register::Rbx,
    });
    ctx.instructions.push(MachineInstr::ImulRR {
        dest: Register::Rax,
        src: Register::R10,
    });

    ctx.instructions.push(MachineInstr::Label { id: atoi_done });
    // Restore registers (in reverse order)
    ctx.instructions
        .push(MachineInstr::Pop { reg: Register::R11 });
    ctx.instructions
        .push(MachineInstr::Pop { reg: Register::R10 });
    ctx.instructions
        .push(MachineInstr::Pop { reg: Register::R9 });
    ctx.instructions
        .push(MachineInstr::Pop { reg: Register::R8 });
    ctx.instructions
        .push(MachineInstr::Pop { reg: Register::Rcx });
    ctx.instructions
        .push(MachineInstr::Pop { reg: Register::Rbx });

    ctx.instructions.push(MachineInstr::Ret);
}
