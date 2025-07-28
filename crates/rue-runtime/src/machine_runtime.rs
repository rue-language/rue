//! Machine instruction generation for runtime functions

use rue_ir::target::{MachineInstr, Register};
use std::collections::HashMap;

use crate::functions::{
    cast::{generate_to_i32_function, generate_to_i64_function},
    data_section::generate_data_section,
    exit::generate_exit_function,
    input::generate_input_function,
    print::{
        generate_println_bool_function, generate_println_i32_function,
        generate_println_i64_function, generate_println_unit_function,
    },
    signal_handlers::{generate_setup_signal_handlers, generate_signal_handlers},
    string_conversion::{generate_atoi_function, generate_itoa_function},
};

/// Context for generating runtime functions
pub struct RuntimeContext {
    pub instructions: Vec<MachineInstr>,
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
}

/// Generate runtime machine code and symbols
pub fn generate_runtime() -> Result<(Vec<MachineInstr>, HashMap<String, u32>), String> {
    let mut ctx = RuntimeContext::new();

    // Generate startup function first
    generate_startup_function(&mut ctx);

    // Generate data section
    generate_data_section(&mut ctx);

    // Generate all runtime functions
    generate_exit_function(&mut ctx);
    generate_itoa_function(&mut ctx);
    generate_println_i64_function(&mut ctx);
    generate_println_i32_function(&mut ctx);
    generate_println_bool_function(&mut ctx);
    generate_println_unit_function(&mut ctx);
    generate_atoi_function(&mut ctx);
    generate_input_function(&mut ctx);
    generate_to_i32_function(&mut ctx);
    generate_to_i64_function(&mut ctx);
    generate_signal_handlers(&mut ctx);
    generate_setup_signal_handlers(&mut ctx);

    Ok((ctx.instructions, ctx.labels))
}

/// Generate startup function that serves as ELF entry point
fn generate_startup_function(ctx: &mut RuntimeContext) {
    // Create _start label
    let start_label = ctx.define_label("_start");

    // _start entry point
    ctx.instructions
        .push(MachineInstr::Label { id: start_label });

    // xor %rbp, %rbp - clear frame pointer
    ctx.instructions.push(MachineInstr::XorRR {
        dest: Register::Rbp,
        src: Register::Rbp,
    });

    // call __rue_main - runtime wrapper
    ctx.instructions.push(MachineInstr::Call {
        target: "__rue_main".to_string(),
    });

    // mov %rax, %rdi - exit status
    ctx.instructions.push(MachineInstr::MovRR {
        dest: Register::Rdi,
        src: Register::Rax,
    });

    // mov $60, %eax - SYS_exit
    ctx.instructions.push(MachineInstr::MovRI64 {
        dest: Register::Rax,
        imm: 60,
    });

    // syscall - ...and we're gone
    ctx.instructions.push(MachineInstr::Syscall);

    // ud2 - unreachable, but nice for debuggers
    ctx.instructions.push(MachineInstr::Ud2);

    // Create __rue_main runtime wrapper function
    generate_rue_main_function(ctx);
}

/// Generate __rue_main runtime wrapper function
fn generate_rue_main_function(ctx: &mut RuntimeContext) {
    let rue_main_label = ctx.define_label("__rue_main");

    ctx.instructions
        .push(MachineInstr::Label { id: rue_main_label });

    // Standard function prologue
    ctx.instructions.push(MachineInstr::EnterFrame);

    // Set up signal handlers
    ctx.instructions.push(MachineInstr::Call {
        target: "__rue_setup_signal_handlers".to_string(),
    });

    // Call user's main function
    ctx.instructions.push(MachineInstr::Call {
        target: "main".to_string(),
    });

    // Standard function epilogue (return value is already in RAX)
    ctx.instructions.push(MachineInstr::LeaveFrame);
    ctx.instructions.push(MachineInstr::Ret);
}
