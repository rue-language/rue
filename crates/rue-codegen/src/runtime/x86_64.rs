//! X86-64 machine instruction generation for runtime functions

use crate::runtime::context::RuntimeContext;
use rue_target::X8664Instr;
use std::collections::HashMap;

/// Generate runtime machine code and symbols
pub fn generate_runtime() -> Result<(Vec<X8664Instr>, HashMap<String, u32>), String> {
    let mut ctx = RuntimeContext::new();

    // Generate startup function first
    ctx.generate_startup_function();

    // Generate data section
    ctx.generate_data_section();

    // Generate memory management functions
    ctx.generate_memory_functions();

    // Generate allocation functions (with stub implementations)
    ctx.generate_alloc_functions();

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
