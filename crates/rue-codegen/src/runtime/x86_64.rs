//! X86-64 machine instruction generation for runtime functions

use crate::constants::*;
use rue_target::{ConditionCode, LabelRef, X86Register, X8664Instr};
use std::collections::HashMap;

/// Context for generating runtime functions
pub struct RuntimeContext {
    pub instructions: Vec<X8664Instr>,
    pub labels: HashMap<String, u32>,
    pub label_counter: u32,
}

impl Default for RuntimeContext {
    fn default() -> Self {
        Self::new()
    }
}

impl RuntimeContext {
    pub fn new() -> Self {
        Self {
            instructions: Vec::new(),
            labels: HashMap::new(),
            label_counter: 0,
        }
    }

    pub fn new_label(&mut self) -> u32 {
        let id = self.label_counter;
        self.label_counter += 1;
        id
    }

    pub fn define_label(&mut self, name: &str) -> u32 {
        let label = self.new_label();
        self.labels.insert(name.to_string(), label);
        label
    }
    /// Generate startup function that serves as ELF entry point
    fn generate_startup_function(&mut self) {
        // Create _start label
        let start_label = self.define_label("_start");

        // _start entry point
        self.instructions
            .push(X8664Instr::Label { id: start_label });

        // xor %rbp, %rbp - clear frame pointer
        self.instructions.push(X8664Instr::XorRR {
            dest: X86Register::Rbp,
            src: X86Register::Rbp,
        });

        // call __rue_main - runtime wrapper
        self.instructions.push(X8664Instr::Call {
            target: "__rue_main".to_string(),
        });

        // mov %rax, %rdi - exit status
        self.instructions.push(X8664Instr::MovRR {
            dest: X86Register::Rdi,
            src: X86Register::Rax,
        });

        // mov $60, %eax - SYS_exit
        self.instructions.push(X8664Instr::MovRI64 {
            dest: X86Register::Rax,
            imm: 60,
        });

        // syscall - ...and we're gone
        self.instructions.push(X8664Instr::Syscall);

        // ud2 - unreachable, but nice for debuggers
        self.instructions.push(X8664Instr::Ud2);

        // Create __rue_main runtime wrapper function
        self.generate_rue_main_function();
    }

    /// Generate __rue_main runtime wrapper function
    fn generate_rue_main_function(&mut self) {
        let rue_main_label = self.define_label("__rue_main");

        self.instructions
            .push(X8664Instr::Label { id: rue_main_label });

        // Standard function prologue
        self.instructions.push(X8664Instr::EnterFrame);

        // Set up signal handlers
        self.instructions.push(X8664Instr::Call {
            target: "__rue_setup_signal_handlers".to_string(),
        });

        // Call user's main function
        self.instructions.push(X8664Instr::Call {
            target: "main".to_string(),
        });

        // Standard function epilogue (return value is already in RAX)
        self.instructions.push(X8664Instr::LeaveFrame);
        self.instructions.push(X8664Instr::Ret);
    }

    /// Generate exit function: exit(code: i64) -> ()
    fn generate_exit_function(&mut self) {
        let exit_label = self.define_label("__rue_exit");

        self.instructions.push(X8664Instr::Label { id: exit_label });
        // rdi = exit code
        self.instructions.push(X8664Instr::MovRI64 {
            dest: X86Register::Rax,
            imm: SYSCALL_EXIT,
        });
        self.instructions.push(X8664Instr::Syscall);
    }

    /// Generate data section with static strings
    fn generate_data_section(&mut self) {
        // Jump over data section
        let data_end_label = self.new_label();
        self.instructions.push(X8664Instr::Jmp {
            target: LabelRef::Local(data_end_label),
        });

        // Newline character
        let newline_label = self.define_label("__rue_newline");
        self.instructions
            .push(X8664Instr::Label { id: newline_label });
        // Store raw byte for newline (0x0A)
        self.instructions
            .push(X8664Instr::DataBytes { bytes: vec![0x0A] });

        // "true" string
        let true_str_label = self.define_label("__rue_true_str");
        self.instructions
            .push(X8664Instr::Label { id: true_str_label });
        // Store "true" as 4 bytes: 't' 'r' 'u' 'e'
        self.instructions.push(X8664Instr::DataBytes {
            bytes: b"true".to_vec(),
        });

        // "false" string
        let false_str_label = self.define_label("__rue_false_str");
        self.instructions.push(X8664Instr::Label {
            id: false_str_label,
        });
        // Store "false" as 5 bytes: 'f' 'a' 'l' 's' 'e'
        self.instructions.push(X8664Instr::DataBytes {
            bytes: b"false".to_vec(),
        });

        // "()" string
        let unit_str_label = self.define_label("__rue_unit_str");
        self.instructions
            .push(X8664Instr::Label { id: unit_str_label });
        self.instructions.push(X8664Instr::DataBytes {
            bytes: b"()".to_vec(),
        });

        // Buffer space (1024 bytes for input)
        let input_buffer_label = self.define_label("__rue_input_buffer");
        self.instructions.push(X8664Instr::Label {
            id: input_buffer_label,
        });
        // Reserve 1024 bytes for input buffer
        self.instructions
            .push(X8664Instr::ReserveBytes { count: 1024 });

        // End of data section
        self.instructions
            .push(X8664Instr::Label { id: data_end_label });
    }
}

/// Generate runtime machine code and symbols
pub fn generate_runtime() -> Result<(Vec<X8664Instr>, HashMap<String, u32>), String> {
    let mut ctx = RuntimeContext::new();

    // Generate startup function first
    ctx.generate_startup_function();

    // Generate data section
    ctx.generate_data_section();

    // Generate all runtime functions
    ctx.generate_exit_function();
    ctx.generate_itoa_function();
    ctx.generate_println_i64_function();
    ctx.generate_println_i32_function();
    ctx.generate_println_bool_function();
    ctx.generate_println_unit_function();
    ctx.generate_atoi_function();
    ctx.generate_input_function();
    ctx.generate_to_i32_function();
    ctx.generate_to_i64_function();
    ctx.generate_signal_handlers();
    ctx.generate_setup_signal_handlers();

    Ok((ctx.instructions, ctx.labels))
}

impl RuntimeContext {
    /// Emit a syscall with error checking
    ///
    /// After the syscall, checks if the return value is negative (error).
    /// If so, exits with the provided error code.
    ///
    /// NOTE: This assumes all syscall parameters (RDI, RSI, RDX, etc.)
    /// are already set up. It only sets RAX to the syscall number.
    fn emit_syscall_with_error_check(&mut self, syscall_num: i64, error_exit_code: i64) {
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

    /// Generate itoa helper function
    /// Input: RDI = number to convert
    ///        RSI = buffer pointer (must have at least 20 bytes)
    /// Output: RAX = number of digits written
    ///         Buffer contains ASCII digits (no null terminator)
    fn generate_itoa_function(&mut self) {
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

    /// Generate println_i64 function
    fn generate_println_i64_function(&mut self) {
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
        // Write the digits with error checking
        self.instructions.push(X8664Instr::MovRI64 {
            dest: X86Register::Rdi,
            imm: FD_STDOUT,
        });
        self.instructions.push(X8664Instr::MovRR {
            dest: X86Register::Rsi,
            src: X86Register::Rsp,
        });
        self.instructions.push(X8664Instr::MovRR {
            dest: X86Register::Rdx,
            src: X86Register::Rax,
        });
        self.emit_syscall_with_error_check(SYSCALL_WRITE, EXIT_CODE_SYSCALL_FAILED);

        // Write newline - use static newline from data section
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

    /// Generate println_i32 function
    fn generate_println_i32_function(&mut self) {
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
    fn generate_println_bool_function(&mut self) {
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
    fn generate_println_unit_function(&mut self) {
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

    /// Generate atoi helper function
    /// Input: RSI = buffer pointer, RDX = buffer length
    /// Output: RAX = parsed integer (0 on error)
    fn generate_atoi_function(&mut self) {
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

    /// Generate input function
    fn generate_input_function(&mut self) {
        let input_label = self.define_label("__rue_input");

        self.instructions
            .push(X8664Instr::Label { id: input_label });

        // Set up stack frame
        self.instructions.push(X8664Instr::EnterFrame);

        // Allocate buffer on stack
        self.instructions.push(X8664Instr::AllocStack {
            size: INPUT_BUFFER_SIZE,
        });

        // Read from stdin into buffer
        self.instructions.push(X8664Instr::MovRI64 {
            dest: X86Register::Rdi,
            imm: FD_STDIN,
        });
        self.instructions.push(X8664Instr::MovRR {
            dest: X86Register::Rsi,
            src: X86Register::Rsp,
        });
        self.instructions.push(X8664Instr::MovRI64 {
            dest: X86Register::Rdx,
            imm: INPUT_BUFFER_SIZE as i64,
        });
        // Set up read syscall parameters
        self.instructions.push(X8664Instr::MovRI64 {
            dest: X86Register::Rax,
            imm: SYSCALL_READ,
        });
        self.instructions.push(X8664Instr::Syscall);

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

    /// Generate to_i32 function: to_i32(value: i64) -> i32
    /// Truncates a 64-bit integer to 32 bits
    fn generate_to_i32_function(&mut self) {
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
    fn generate_to_i64_function(&mut self) {
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

    /// Generate signal handlers
    fn generate_signal_handlers(&mut self) {
        let sigfpe_handler_label = self.define_label("__rue_sigfpe_handler");
        let sigsegv_handler_label = self.define_label("__rue_sigsegv_handler");

        // SIGFPE handler
        self.instructions.push(X8664Instr::Label {
            id: sigfpe_handler_label,
        });
        self.instructions.push(X8664Instr::MovRI64 {
            dest: X86Register::Rdi,
            imm: EXIT_CODE_DIV_BY_ZERO,
        });
        self.instructions.push(X8664Instr::MovRI64 {
            dest: X86Register::Rax,
            imm: SYSCALL_EXIT,
        });
        self.instructions.push(X8664Instr::Syscall);

        // SIGSEGV handler
        self.instructions.push(X8664Instr::Label {
            id: sigsegv_handler_label,
        });
        self.instructions.push(X8664Instr::MovRI64 {
            dest: X86Register::Rdi,
            imm: EXIT_CODE_STACK_OVERFLOW,
        });
        self.instructions.push(X8664Instr::MovRI64 {
            dest: X86Register::Rax,
            imm: SYSCALL_EXIT,
        });
        self.instructions.push(X8664Instr::Syscall);
    }

    /// Generate setup_signal_handlers function
    fn generate_setup_signal_handlers(&mut self) {
        let setup_label = self.define_label("__rue_setup_signal_handlers");

        self.instructions
            .push(X8664Instr::Label { id: setup_label });

        // Set up stack frame
        self.instructions.push(X8664Instr::EnterFrame);

        // For now, just a minimal setup that returns successfully
        // TODO: Full signal handler setup implementation needed

        self.instructions.push(X8664Instr::LeaveFrame);
        self.instructions.push(X8664Instr::Ret);
    }
}
