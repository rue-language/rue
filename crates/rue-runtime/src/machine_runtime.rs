//! Machine instruction generation for runtime functions

use rue_ir::target::MachineInstr;
use std::collections::HashMap;

use crate::functions::{
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

    // Generate data section first
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
    generate_signal_handlers(&mut ctx);
    generate_setup_signal_handlers(&mut ctx);

    Ok((ctx.instructions, ctx.labels))
}
