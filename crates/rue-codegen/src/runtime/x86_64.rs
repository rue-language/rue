//! X86-64 machine instruction generation for runtime functions

use crate::runtime::context::RuntimeContext;
use rue_target::X8664Instr;
use std::collections::HashMap;

/// Generate runtime machine code and symbols
///
/// If `use_rust_runtime` is true, only generates startup infrastructure (_start, __rue_main)
/// and signal handlers. Runtime I/O and conversion functions are expected to come from
/// librue_runtime.a instead.
pub fn generate_runtime(
    use_rust_runtime: bool,
) -> Result<(Vec<X8664Instr>, HashMap<String, u32>), String> {
    let mut ctx = RuntimeContext::new();

    // Always generate startup function (entry point)
    ctx.generate_startup_function();

    if !use_rust_runtime {
        // Generate data section (only needed for generated runtime)
        ctx.generate_data_section();

        // Generate CPU feature detection and dispatch
        ctx.generate_cpu_feature_detection();
        ctx.generate_erms_stubs();

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
    }

    // Always generate signal handlers (small, not worth moving to Rust runtime)
    ctx.generate_signal_handlers();
    ctx.generate_setup_signal_handlers();

    Ok((ctx.instructions, ctx.labels))
}
