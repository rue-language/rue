//! Type conversion functions (itoa, atoi, to_i32, to_i64)

use crate::constants::*;
use crate::runtime::context::RuntimeContext;
use rue_target::{ConditionCode, LabelRef, X86Register, X8664Instr};

impl RuntimeContext {
    /// Generate itoa helper function
    /// Input: RDI = number to convert
    ///        RSI = buffer pointer (must have at least 20 bytes)
    /// Output: RAX = number of digits written
    ///         Buffer contains ASCII digits (no null terminator)
    pub fn generate_itoa_function(&mut self) {
        let itoa_label = self.define_label("__rue_itoa");
        let negative_label = self.new_label();
        let positive_label = self.new_label();
        let digit_loop = self.new_label();
        let reverse_start = self.new_label();
        let reverse_loop = self.new_label();
        let reverse_done = self.new_label();
        let itoa_done = self.new_label();

        self.instructions.push(X8664Instr::Label { id: itoa_label });

        // Save registers
        self.instructions.push(X8664Instr::Push {
            reg: X86Register::Rbx,
        });
        self.instructions.push(X8664Instr::Push {
            reg: X86Register::Rcx,
        });
        self.instructions.push(X8664Instr::Push {
            reg: X86Register::Rdx,
        });
        self.instructions.push(X8664Instr::Push {
            reg: X86Register::R8,
        });
        self.instructions.push(X8664Instr::Push {
            reg: X86Register::R9,
        });
        self.instructions.push(X8664Instr::Push {
            reg: X86Register::R10,
        });
        self.instructions.push(X8664Instr::Push {
            reg: X86Register::R11,
        });

        // R8 = output buffer
        self.instructions.push(X8664Instr::MovRR {
            dest: X86Register::R8,
            src: X86Register::Rsi,
        });

        // RBX = number to convert
        self.instructions.push(X8664Instr::MovRR {
            dest: X86Register::Rbx,
            src: X86Register::Rdi,
        });

        // R9 = digit count (starts at 0)
        self.instructions.push(X8664Instr::MovRI64 {
            dest: X86Register::R9,
            imm: 0,
        });

        // Check if negative
        self.instructions.push(X8664Instr::CmpRI {
            reg: X86Register::Rbx,
            imm: 0,
        });
        self.instructions.push(X8664Instr::JmpCC {
            cc: ConditionCode::Less,
            target: LabelRef::Local(negative_label),
        });
        self.instructions.push(X8664Instr::Jmp {
            target: LabelRef::Local(positive_label),
        });

        // Handle negative numbers
        self.instructions
            .push(X8664Instr::Label { id: negative_label });
        // Write '-' to buffer
        self.instructions.push(X8664Instr::MovRI64 {
            dest: X86Register::Rax,
            imm: CHAR_MINUS as i64,
        });
        self.instructions.push(X8664Instr::MovMR8 {
            base: X86Register::R8,
            offset: 0,
            src: X86Register::Rax,
        });
        // Increment buffer pointer
        self.instructions.push(X8664Instr::AddRI {
            dest: X86Register::R8,
            imm: 1,
        });
        // Increment digit count
        self.instructions.push(X8664Instr::AddRI {
            dest: X86Register::R9,
            imm: 1,
        });

        // Handle special case of i64::MIN (-9223372036854775808)
        self.instructions.push(X8664Instr::MovRI64 {
            dest: X86Register::Rax,
            imm: i64::MIN,
        });
        self.instructions.push(X8664Instr::CmpRR {
            left: X86Register::Rbx,
            right: X86Register::Rax,
        });

        let not_min_label = self.new_label();
        self.instructions.push(X8664Instr::JmpCC {
            cc: ConditionCode::NotEqual,
            target: LabelRef::Local(not_min_label),
        });

        // Special handling for i64::MIN - write the literal string
        let min_digits = "9223372036854775808";
        for (i, ch) in min_digits.chars().enumerate() {
            self.instructions.push(X8664Instr::MovRI64 {
                dest: X86Register::Rax,
                imm: ch as i64,
            });
            self.instructions.push(X8664Instr::MovMR8 {
                base: X86Register::R8,
                offset: i as i32,
                src: X86Register::Rax,
            });
        }
        self.instructions.push(X8664Instr::AddRI {
            dest: X86Register::R9,
            imm: min_digits.len() as i32,
        });
        self.instructions.push(X8664Instr::Jmp {
            target: LabelRef::Local(itoa_done),
        });

        self.instructions
            .push(X8664Instr::Label { id: not_min_label });
        // Negate the number (two's complement)
        self.instructions.push(X8664Instr::MovRI64 {
            dest: X86Register::Rax,
            imm: 0,
        });
        self.instructions.push(X8664Instr::SubRR {
            dest: X86Register::Rax,
            src: X86Register::Rbx,
        });
        self.instructions.push(X8664Instr::MovRR {
            dest: X86Register::Rbx,
            src: X86Register::Rax,
        });

        // Process positive number
        self.instructions
            .push(X8664Instr::Label { id: positive_label });

        // R10 = temporary buffer pointer for digit extraction
        self.instructions.push(X8664Instr::MovRR {
            dest: X86Register::R10,
            src: X86Register::R8,
        });

        // Extract digits in reverse order
        self.instructions.push(X8664Instr::Label { id: digit_loop });

        // RAX = RBX / 10, RDX = RBX % 10
        self.instructions.push(X8664Instr::MovRR {
            dest: X86Register::Rax,
            src: X86Register::Rbx,
        });
        self.instructions.push(X8664Instr::MovRI64 {
            dest: X86Register::Rdx,
            imm: 0,
        });
        self.instructions.push(X8664Instr::MovRI64 {
            dest: X86Register::Rcx,
            imm: 10,
        });
        self.instructions.push(X8664Instr::Idiv {
            divisor: X86Register::Rcx,
        });

        // Convert digit to ASCII (add '0')
        self.instructions.push(X8664Instr::AddRI {
            dest: X86Register::Rdx,
            imm: CHAR_ZERO as i32,
        });

        // Store digit
        self.instructions.push(X8664Instr::MovMR8 {
            base: X86Register::R10,
            offset: 0,
            src: X86Register::Rdx,
        });
        self.instructions.push(X8664Instr::AddRI {
            dest: X86Register::R10,
            imm: 1,
        });
        self.instructions.push(X8664Instr::AddRI {
            dest: X86Register::R9,
            imm: 1,
        });

        // Update RBX = quotient for next iteration
        self.instructions.push(X8664Instr::MovRR {
            dest: X86Register::Rbx,
            src: X86Register::Rax,
        });

        // Continue if quotient != 0
        self.instructions.push(X8664Instr::CmpRI {
            reg: X86Register::Rbx,
            imm: 0,
        });
        self.instructions.push(X8664Instr::JmpCC {
            cc: ConditionCode::NotEqual,
            target: LabelRef::Local(digit_loop),
        });

        // Now reverse the digits in place
        self.instructions
            .push(X8664Instr::Label { id: reverse_start });

        // R10 = end pointer (one past last digit)
        // R11 = start pointer
        self.instructions.push(X8664Instr::MovRR {
            dest: X86Register::R11,
            src: X86Register::R8,
        });
        self.instructions.push(X8664Instr::SubRI {
            dest: X86Register::R10,
            imm: 1,
        });

        self.instructions
            .push(X8664Instr::Label { id: reverse_loop });
        self.instructions.push(X8664Instr::CmpRR {
            left: X86Register::R11,
            right: X86Register::R10,
        });
        self.instructions.push(X8664Instr::JmpCC {
            cc: ConditionCode::GreaterEqual,
            target: LabelRef::Local(reverse_done),
        });

        // Swap characters at R11 and R10
        self.instructions.push(X8664Instr::MovRM8 {
            dest: X86Register::Rax,
            base: X86Register::R11,
            offset: 0,
        });
        self.instructions.push(X8664Instr::MovRM8 {
            dest: X86Register::Rbx,
            base: X86Register::R10,
            offset: 0,
        });
        self.instructions.push(X8664Instr::MovMR8 {
            base: X86Register::R11,
            offset: 0,
            src: X86Register::Rbx,
        });
        self.instructions.push(X8664Instr::MovMR8 {
            base: X86Register::R10,
            offset: 0,
            src: X86Register::Rax,
        });

        // Move pointers inward
        self.instructions.push(X8664Instr::AddRI {
            dest: X86Register::R11,
            imm: 1,
        });
        self.instructions.push(X8664Instr::SubRI {
            dest: X86Register::R10,
            imm: 1,
        });
        self.instructions.push(X8664Instr::Jmp {
            target: LabelRef::Local(reverse_loop),
        });

        self.instructions
            .push(X8664Instr::Label { id: reverse_done });

        self.instructions.push(X8664Instr::Label { id: itoa_done });
        // Return digit count in RAX
        self.instructions.push(X8664Instr::MovRR {
            dest: X86Register::Rax,
            src: X86Register::R9,
        });

        // Restore registers
        self.instructions.push(X8664Instr::Pop {
            reg: X86Register::R11,
        });
        self.instructions.push(X8664Instr::Pop {
            reg: X86Register::R10,
        });
        self.instructions.push(X8664Instr::Pop {
            reg: X86Register::R9,
        });
        self.instructions.push(X8664Instr::Pop {
            reg: X86Register::R8,
        });
        self.instructions.push(X8664Instr::Pop {
            reg: X86Register::Rdx,
        });
        self.instructions.push(X8664Instr::Pop {
            reg: X86Register::Rcx,
        });
        self.instructions.push(X8664Instr::Pop {
            reg: X86Register::Rbx,
        });

        self.instructions.push(X8664Instr::Ret);
    }

    /// Generate atoi helper function
    /// Input: RSI = buffer pointer, RDX = buffer length
    /// Output: RAX = parsed integer (0 on error)
    pub fn generate_atoi_function(&mut self) {
        let atoi_label = self.define_label("__rue_atoi");
        let skip_whitespace_loop = self.new_label();
        let skip_whitespace_done = self.new_label();
        let skip_whitespace_next = self.new_label();
        let check_sign = self.new_label();
        let negative_number = self.new_label();
        let parse_digits = self.new_label();
        let digit_loop = self.new_label();
        let invalid_char = self.new_label();
        let apply_sign = self.new_label();
        let atoi_done = self.new_label();

        self.instructions.push(X8664Instr::Label { id: atoi_label });

        // Save registers
        self.instructions.push(X8664Instr::Push {
            reg: X86Register::Rbx,
        });
        self.instructions.push(X8664Instr::Push {
            reg: X86Register::Rcx,
        });
        self.instructions.push(X8664Instr::Push {
            reg: X86Register::R8,
        });
        self.instructions.push(X8664Instr::Push {
            reg: X86Register::R9,
        });
        self.instructions.push(X8664Instr::Push {
            reg: X86Register::R10,
        });
        self.instructions.push(X8664Instr::Push {
            reg: X86Register::R11,
        });

        // R8 = buffer pointer
        self.instructions.push(X8664Instr::MovRR {
            dest: X86Register::R8,
            src: X86Register::Rsi,
        });

        // R9 = remaining length
        self.instructions.push(X8664Instr::MovRR {
            dest: X86Register::R9,
            src: X86Register::Rdx,
        });

        // Skip leading whitespace
        self.instructions.push(X8664Instr::Label {
            id: skip_whitespace_loop,
        });
        self.instructions.push(X8664Instr::CmpRI {
            reg: X86Register::R9,
            imm: 0,
        });
        self.instructions.push(X8664Instr::JmpCC {
            cc: ConditionCode::Equal,
            target: LabelRef::Local(skip_whitespace_done),
        });

        // Load character
        self.instructions.push(X8664Instr::MovRM8 {
            dest: X86Register::Rax,
            base: X86Register::R8,
            offset: 0,
        });

        // Check for whitespace (space, tab, newline, etc.)
        self.instructions.push(X8664Instr::CmpRI {
            reg: X86Register::Rax,
            imm: 32, // space
        });
        self.instructions.push(X8664Instr::JmpCC {
            cc: ConditionCode::Equal,
            target: LabelRef::Local(skip_whitespace_next),
        });
        self.instructions.push(X8664Instr::CmpRI {
            reg: X86Register::Rax,
            imm: 9, // tab
        });
        self.instructions.push(X8664Instr::JmpCC {
            cc: ConditionCode::Equal,
            target: LabelRef::Local(skip_whitespace_next),
        });
        self.instructions.push(X8664Instr::CmpRI {
            reg: X86Register::Rax,
            imm: 10, // newline
        });
        self.instructions.push(X8664Instr::JmpCC {
            cc: ConditionCode::Equal,
            target: LabelRef::Local(skip_whitespace_next),
        });
        self.instructions.push(X8664Instr::CmpRI {
            reg: X86Register::Rax,
            imm: 13, // carriage return
        });
        self.instructions.push(X8664Instr::JmpCC {
            cc: ConditionCode::Equal,
            target: LabelRef::Local(skip_whitespace_next),
        });

        // Not whitespace, continue
        self.instructions.push(X8664Instr::Jmp {
            target: LabelRef::Local(skip_whitespace_done),
        });

        // Actually skip the whitespace character
        self.instructions.push(X8664Instr::Label {
            id: skip_whitespace_next,
        });
        self.instructions.push(X8664Instr::AddRI {
            dest: X86Register::R8,
            imm: 1,
        });
        self.instructions.push(X8664Instr::SubRI {
            dest: X86Register::R9,
            imm: 1,
        });
        self.instructions.push(X8664Instr::Jmp {
            target: LabelRef::Local(skip_whitespace_loop),
        });

        self.instructions.push(X8664Instr::Label {
            id: skip_whitespace_done,
        });

        // Check for empty string after whitespace
        self.instructions.push(X8664Instr::CmpRI {
            reg: X86Register::R9,
            imm: 0,
        });
        self.instructions.push(X8664Instr::JmpCC {
            cc: ConditionCode::Equal,
            target: LabelRef::Local(invalid_char),
        });

        // R10 = sign (1 for positive, -1 for negative)
        self.instructions.push(X8664Instr::MovRI64 {
            dest: X86Register::R10,
            imm: 1,
        });

        // Check for sign
        self.instructions.push(X8664Instr::Label { id: check_sign });
        self.instructions.push(X8664Instr::MovRM8 {
            dest: X86Register::Rax,
            base: X86Register::R8,
            offset: 0,
        });

        self.instructions.push(X8664Instr::CmpRI {
            reg: X86Register::Rax,
            imm: CHAR_MINUS as i32,
        });
        self.instructions.push(X8664Instr::JmpCC {
            cc: ConditionCode::Equal,
            target: LabelRef::Local(negative_number),
        });

        self.instructions.push(X8664Instr::CmpRI {
            reg: X86Register::Rax,
            imm: 43, // '+'
        });
        self.instructions.push(X8664Instr::JmpCC {
            cc: ConditionCode::NotEqual,
            target: LabelRef::Local(parse_digits),
        });

        // Skip '+' sign
        self.instructions.push(X8664Instr::AddRI {
            dest: X86Register::R8,
            imm: 1,
        });
        self.instructions.push(X8664Instr::SubRI {
            dest: X86Register::R9,
            imm: 1,
        });
        self.instructions.push(X8664Instr::Jmp {
            target: LabelRef::Local(parse_digits),
        });

        self.instructions.push(X8664Instr::Label {
            id: negative_number,
        });
        self.instructions.push(X8664Instr::MovRI64 {
            dest: X86Register::R10,
            imm: -1i64,
        });
        self.instructions.push(X8664Instr::AddRI {
            dest: X86Register::R8,
            imm: 1,
        });
        self.instructions.push(X8664Instr::SubRI {
            dest: X86Register::R9,
            imm: 1,
        });

        // Parse digits
        self.instructions
            .push(X8664Instr::Label { id: parse_digits });
        self.instructions.push(X8664Instr::MovRI64 {
            dest: X86Register::Rbx,
            imm: 0, // accumulator
        });

        self.instructions.push(X8664Instr::Label { id: digit_loop });
        self.instructions.push(X8664Instr::CmpRI {
            reg: X86Register::R9,
            imm: 0,
        });
        self.instructions.push(X8664Instr::JmpCC {
            cc: ConditionCode::Equal,
            target: LabelRef::Local(apply_sign),
        });

        // Load character
        self.instructions.push(X8664Instr::MovRM8 {
            dest: X86Register::Rax,
            base: X86Register::R8,
            offset: 0,
        });

        // Check if it's a digit (0-9)
        self.instructions.push(X8664Instr::CmpRI {
            reg: X86Register::Rax,
            imm: CHAR_ZERO as i32, // '0'
        });
        self.instructions.push(X8664Instr::JmpCC {
            cc: ConditionCode::Less,
            target: LabelRef::Local(apply_sign),
        });
        self.instructions.push(X8664Instr::CmpRI {
            reg: X86Register::Rax,
            imm: 57, // '9'
        });
        self.instructions.push(X8664Instr::JmpCC {
            cc: ConditionCode::Greater,
            target: LabelRef::Local(apply_sign),
        });

        // Convert ASCII to digit
        self.instructions.push(X8664Instr::SubRI {
            dest: X86Register::Rax,
            imm: CHAR_ZERO as i32,
        });

        // Simple overflow check - if accumulator is already very large, return 0
        // Check if RBX > 922337203685477580 (i64::MAX / 10)
        self.instructions.push(X8664Instr::MovRI64 {
            dest: X86Register::Rcx,
            imm: 922337203685477580,
        });
        self.instructions.push(X8664Instr::CmpRR {
            left: X86Register::Rbx,
            right: X86Register::Rcx,
        });

        let no_overflow = self.new_label();
        self.instructions.push(X8664Instr::JmpCC {
            cc: ConditionCode::LessEqual,
            target: LabelRef::Local(no_overflow),
        });

        // Overflow - return 0
        self.instructions.push(X8664Instr::MovRI64 {
            dest: X86Register::Rax,
            imm: 0,
        });
        self.instructions.push(X8664Instr::Jmp {
            target: LabelRef::Local(atoi_done),
        });

        self.instructions
            .push(X8664Instr::Label { id: no_overflow });

        // Multiply accumulator by 10
        self.instructions.push(X8664Instr::ImulRI {
            dest: X86Register::Rbx,
            imm: 10,
        });

        // Add new digit
        self.instructions.push(X8664Instr::AddRR {
            dest: X86Register::Rbx,
            src: X86Register::Rax,
        });

        // Next character
        self.instructions.push(X8664Instr::AddRI {
            dest: X86Register::R8,
            imm: 1,
        });
        self.instructions.push(X8664Instr::SubRI {
            dest: X86Register::R9,
            imm: 1,
        });
        self.instructions.push(X8664Instr::Jmp {
            target: LabelRef::Local(digit_loop),
        });

        // Invalid character - return 0
        self.instructions
            .push(X8664Instr::Label { id: invalid_char });
        self.instructions.push(X8664Instr::MovRI64 {
            dest: X86Register::Rax,
            imm: 0,
        });
        self.instructions.push(X8664Instr::Jmp {
            target: LabelRef::Local(atoi_done),
        });

        // Apply sign
        self.instructions.push(X8664Instr::Label { id: apply_sign });
        self.instructions.push(X8664Instr::MovRR {
            dest: X86Register::Rax,
            src: X86Register::Rbx,
        });
        self.instructions.push(X8664Instr::ImulRR {
            dest: X86Register::Rax,
            src: X86Register::R10,
        });

        self.instructions.push(X8664Instr::Label { id: atoi_done });
        // Restore registers (in reverse order)
        self.instructions.push(X8664Instr::Pop {
            reg: X86Register::R11,
        });
        self.instructions.push(X8664Instr::Pop {
            reg: X86Register::R10,
        });
        self.instructions.push(X8664Instr::Pop {
            reg: X86Register::R9,
        });
        self.instructions.push(X8664Instr::Pop {
            reg: X86Register::R8,
        });
        self.instructions.push(X8664Instr::Pop {
            reg: X86Register::Rcx,
        });
        self.instructions.push(X8664Instr::Pop {
            reg: X86Register::Rbx,
        });

        self.instructions.push(X8664Instr::Ret);
    }

    /// Generate to_i32 function: to_i32(value: i64) -> i32
    /// Truncates a 64-bit integer to 32 bits
    pub fn generate_to_i32_function(&mut self) {
        let to_i32_label = self.define_label("__rue_to_i32");

        self.instructions
            .push(X8664Instr::Label { id: to_i32_label });

        // Input is in RDI (i64)
        // Output will be in RAX (i32)
        // MOV EAX, EDI performs the truncation automatically
        self.instructions.push(X8664Instr::MovRR {
            dest: X86Register::Rax,
            src: X86Register::Rdi,
        });

        self.instructions.push(X8664Instr::Ret);
    }

    /// Generate to_i64 function: to_i64(value: i32) -> i64
    /// Sign-extends a 32-bit integer to 64 bits
    pub fn generate_to_i64_function(&mut self) {
        let to_i64_label = self.define_label("__rue_to_i64");

        self.instructions
            .push(X8664Instr::Label { id: to_i64_label });

        // Input is in EDI (i32)
        // Output will be in RAX (i64)
        // MOVSXD RAX, EDI performs sign extension from 32 to 64 bits
        self.instructions.push(X8664Instr::Movsxd {
            dest: X86Register::Rax,
            src: X86Register::Rdi,
        });

        self.instructions.push(X8664Instr::Ret);
    }
}
