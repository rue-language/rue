//! Startup and signal handling functions

use crate::constants::*;
use crate::runtime::context::RuntimeContext;
use rue_target::{X86Register, X8664Instr};

impl RuntimeContext {
    /// Generate startup function that serves as ELF entry point
    pub fn generate_startup_function(&mut self) {
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

        // Initialize heap (stub implementation - safe to call)
        self.instructions.push(X8664Instr::Call {
            target: "__rue_heap_init".to_string(),
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
    pub fn generate_exit_function(&mut self) {
        let exit_label = self.define_label("__rue_exit");

        self.instructions.push(X8664Instr::Label { id: exit_label });
        // Use syscall API - exit code is already in RDI
        self.sys_exit(X86Register::Rdi);
    }

    /// Generate signal handlers
    pub fn generate_signal_handlers(&mut self) {
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
        self.sys_exit(X86Register::Rdi);

        // SIGSEGV handler
        self.instructions.push(X8664Instr::Label {
            id: sigsegv_handler_label,
        });
        self.instructions.push(X8664Instr::MovRI64 {
            dest: X86Register::Rdi,
            imm: EXIT_CODE_STACK_OVERFLOW,
        });
        self.sys_exit(X86Register::Rdi);
    }

    /// Generate setup_signal_handlers function
    pub fn generate_setup_signal_handlers(&mut self) {
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
