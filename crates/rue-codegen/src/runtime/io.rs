//! I/O functions (println variants, input)

use crate::constants::*;
use crate::runtime::context::RuntimeContext;
use rue_target::{ConditionCode, LabelRef, X86Register, X8664Instr};

impl RuntimeContext {
    /// Generate println_i64 function
    pub fn generate_println_i64_function(&mut self) {
        let println_i64_label = self.define_label("__rue_println_i64");

        self.instructions.push(X8664Instr::Label {
            id: println_i64_label,
        });

        // Set up stack frame
        self.instructions.push(X8664Instr::EnterFrame);

        // Allocate space for number buffer
        self.instructions.push(X8664Instr::AllocStack {
            size: ITOA_BUFFER_SIZE,
        });

        // Call itoa to convert number to string
        // RSI = buffer pointer (use stack)
        self.instructions.push(X8664Instr::MovRR {
            dest: X86Register::Rsi,
            src: X86Register::Rsp,
        });
        self.instructions.push(X8664Instr::Call {
            target: "__rue_itoa".to_string(),
        });

        // RAX now contains number of digits
        // Write the digits with error checking using syscall API
        self.sys_write(FD_STDOUT, X86Register::Rsp, X86Register::Rax);

        // Write newline - use static newline from data section
        self.instructions.push(X8664Instr::LeaLabel {
            dest: X86Register::Rsi,
            label: "__rue_newline".to_string(),
        });
        self.instructions.push(X8664Instr::MovRI64 {
            dest: X86Register::Rdx,
            imm: 1, // length
        });
        self.sys_write(FD_STDOUT, X86Register::Rsi, X86Register::Rdx);

        // Clean up stack and leave frame
        self.instructions.push(X8664Instr::LeaveFrame);

        self.instructions.push(X8664Instr::Ret);
    }

    /// Generate println_i32 function
    pub fn generate_println_i32_function(&mut self) {
        let println_i32_label = self.define_label("__rue_println_i32");

        self.instructions.push(X8664Instr::Label {
            id: println_i32_label,
        });

        // Sign-extend i32 to i64
        // Input is in EDI (lower 32 bits of RDI)
        // We need to sign-extend properly
        self.instructions.push(X8664Instr::MovRR {
            dest: X86Register::Rax,
            src: X86Register::Rdi,
        });

        // Use shift left then arithmetic shift right to sign extend
        self.instructions.push(X8664Instr::MovRI64 {
            dest: X86Register::Rcx,
            imm: 32,
        });
        self.instructions.push(X8664Instr::Shl {
            dest: X86Register::Rax,
            count: X86Register::Rcx,
        });
        self.instructions.push(X8664Instr::Sar {
            dest: X86Register::Rax,
            count: X86Register::Rcx,
        });
        self.instructions.push(X8664Instr::MovRR {
            dest: X86Register::Rdi,
            src: X86Register::Rax,
        });

        // Call println_i64
        self.instructions.push(X8664Instr::Call {
            target: "__rue_println_i64".to_string(),
        });
        self.instructions.push(X8664Instr::Ret);
    }

    /// Generate println_bool function
    pub fn generate_println_bool_function(&mut self) {
        let println_bool_label = self.define_label("__rue_println_bool");
        let print_false_label = self.new_label();
        let print_done_label = self.new_label();

        self.instructions.push(X8664Instr::Label {
            id: println_bool_label,
        });

        // Set up stack frame
        self.instructions.push(X8664Instr::EnterFrame);

        // Check if RDI is 0 (false) or non-zero (true)
        self.instructions.push(X8664Instr::CmpRI {
            reg: X86Register::Rdi,
            imm: 0,
        });
        self.instructions.push(X8664Instr::JmpCC {
            cc: ConditionCode::Equal,
            target: LabelRef::Local(print_false_label),
        });

        // Print "true" - use static string from data section
        self.instructions.push(X8664Instr::MovRI64 {
            dest: X86Register::Rdi,
            imm: FD_STDOUT,
        });
        self.instructions.push(X8664Instr::LeaLabel {
            dest: X86Register::Rsi,
            label: "__rue_true_str".to_string(),
        });
        self.instructions.push(X8664Instr::MovRI64 {
            dest: X86Register::Rdx,
            imm: 4, // length of "true"
        });
        self.emit_syscall_with_error_check(SYSCALL_WRITE, EXIT_CODE_SYSCALL_FAILED);

        self.instructions.push(X8664Instr::Jmp {
            target: LabelRef::Local(print_done_label),
        });

        // Print "false"
        self.instructions.push(X8664Instr::Label {
            id: print_false_label,
        });

        // Print "false" - use static string from data section
        self.instructions.push(X8664Instr::MovRI64 {
            dest: X86Register::Rdi,
            imm: FD_STDOUT,
        });
        self.instructions.push(X8664Instr::LeaLabel {
            dest: X86Register::Rsi,
            label: "__rue_false_str".to_string(),
        });
        self.instructions.push(X8664Instr::MovRI64 {
            dest: X86Register::Rdx,
            imm: 5, // length of "false"
        });
        self.emit_syscall_with_error_check(SYSCALL_WRITE, EXIT_CODE_SYSCALL_FAILED);

        self.instructions.push(X8664Instr::Label {
            id: print_done_label,
        });

        // Print newline - use static newline from data section
        self.instructions.push(X8664Instr::MovRI64 {
            dest: X86Register::Rdi,
            imm: FD_STDOUT,
        });
        self.instructions.push(X8664Instr::LeaLabel {
            dest: X86Register::Rsi,
            label: "__rue_newline".to_string(),
        });
        self.instructions.push(X8664Instr::MovRI64 {
            dest: X86Register::Rdx,
            imm: 1, // length
        });
        self.emit_syscall_with_error_check(SYSCALL_WRITE, EXIT_CODE_SYSCALL_FAILED);

        // Clean up stack and leave frame
        self.instructions.push(X8664Instr::LeaveFrame);

        self.instructions.push(X8664Instr::Ret);
    }

    /// Generate println_unit function
    pub fn generate_println_unit_function(&mut self) {
        let println_unit_label = self.define_label("__rue_println_unit");

        self.instructions.push(X8664Instr::Label {
            id: println_unit_label,
        });

        // Set up stack frame
        self.instructions.push(X8664Instr::EnterFrame);

        // Print "()" - use static string from data section
        self.instructions.push(X8664Instr::MovRI64 {
            dest: X86Register::Rdi,
            imm: FD_STDOUT,
        });
        self.instructions.push(X8664Instr::LeaLabel {
            dest: X86Register::Rsi,
            label: "__rue_unit_str".to_string(),
        });
        self.instructions.push(X8664Instr::MovRI64 {
            dest: X86Register::Rdx,
            imm: 2, // length of "()"
        });
        self.emit_syscall_with_error_check(SYSCALL_WRITE, EXIT_CODE_SYSCALL_FAILED);

        // Print newline - use static newline from data section
        self.instructions.push(X8664Instr::MovRI64 {
            dest: X86Register::Rdi,
            imm: FD_STDOUT,
        });
        self.instructions.push(X8664Instr::LeaLabel {
            dest: X86Register::Rsi,
            label: "__rue_newline".to_string(),
        });
        self.instructions.push(X8664Instr::MovRI64 {
            dest: X86Register::Rdx,
            imm: 1, // length
        });
        self.emit_syscall_with_error_check(SYSCALL_WRITE, EXIT_CODE_SYSCALL_FAILED);

        // Clean up stack and leave frame
        self.instructions.push(X8664Instr::LeaveFrame);

        self.instructions.push(X8664Instr::Ret);
    }

    /// Generate input function
    pub fn generate_input_function(&mut self) {
        let input_label = self.define_label("__rue_input");

        self.instructions
            .push(X8664Instr::Label { id: input_label });

        // Set up stack frame
        self.instructions.push(X8664Instr::EnterFrame);

        // Allocate buffer on stack
        self.instructions.push(X8664Instr::AllocStack {
            size: INPUT_BUFFER_SIZE,
        });

        // Read from stdin into buffer using syscall API
        self.sys_read(FD_STDIN, X86Register::Rsp, INPUT_BUFFER_SIZE as i64);

        // Check for error (negative return value)
        self.instructions.push(X8664Instr::CmpRI {
            reg: X86Register::Rax,
            imm: 0,
        });

        let read_success = self.new_label();
        self.instructions.push(X8664Instr::JmpCC {
            cc: ConditionCode::GreaterEqual,
            target: LabelRef::Local(read_success),
        });

        // Error path: exit with error code
        self.instructions.push(X8664Instr::MovRI64 {
            dest: X86Register::Rdi,
            imm: EXIT_CODE_SYSCALL_FAILED,
        });
        self.instructions.push(X8664Instr::MovRI64 {
            dest: X86Register::Rax,
            imm: SYSCALL_EXIT,
        });
        self.instructions.push(X8664Instr::Syscall);

        // Success path
        self.instructions
            .push(X8664Instr::Label { id: read_success });

        // RAX now contains number of bytes read
        // Call atoi to parse the input
        self.instructions.push(X8664Instr::MovRR {
            dest: X86Register::Rsi,
            src: X86Register::Rsp,
        });
        self.instructions.push(X8664Instr::MovRR {
            dest: X86Register::Rdx,
            src: X86Register::Rax,
        });
        self.instructions.push(X8664Instr::Call {
            target: "__rue_atoi".to_string(),
        });

        // Clean up stack and leave frame
        self.instructions.push(X8664Instr::LeaveFrame);

        // RAX contains the parsed number
        self.instructions.push(X8664Instr::Ret);
    }
}
