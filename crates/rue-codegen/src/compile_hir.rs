//! HIR-based compilation functions
//!
//! This module provides functions to compile from HIR to executable code.

use crate::CodegenError;
use crate::backend_driver::BackendDriver;
use crate::elf_writer::ElfWriter;
use crate::format_instructions_as_assembly;
use crate::hir_codegen::HirCodegen;
use crate::x86_emitter::X86Emitter;
use rue_ir::hir::HirProgram;

/// Compile HIR to assembly text
pub fn compile_hir_to_assembly(hir: &HirProgram) -> Result<String, CodegenError> {
    // Phase 0: Create backend driver with runtime code
    let driver = BackendDriver::new()?;

    // Phase 1: Generate high-level IR instructions from HIR
    let (instructions, function_labels) = {
        let mut cg = HirCodegen::new();
        cg.generate(hir)?;
        cg.get_output()
    };

    // Phase 2: Identify function boundaries
    let function_boundaries = driver.discover_function_boundaries(&instructions, &function_labels);

    // Phase 3: Assign label IDs and lower functions
    let (ir_to_machine_labels, label_id_counter) = driver.assign_label_ids(&instructions, 0);
    let all_machine_instructions = driver.lower_functions(
        &instructions,
        function_labels.clone(),
        function_boundaries,
        ir_to_machine_labels.clone(),
        label_id_counter,
    )?;

    // Phase 4: Combine runtime and user code
    let final_instructions = driver.combine_runtime_and_user_code(all_machine_instructions);

    // Phase 5: Generate assembly text
    let all_function_labels = driver.build_final_labels(&function_labels, &ir_to_machine_labels);
    let runtime_label_count = driver.runtime_label_count();

    // Convert machine instructions to assembly text
    let asm_text = format_instructions_as_assembly(
        &final_instructions,
        &all_function_labels,
        runtime_label_count,
    );
    Ok(asm_text)
}

/// Compile HIR to executable
pub fn compile_hir_to_executable(hir: &HirProgram) -> Result<Vec<u8>, CodegenError> {
    // Phase 0: Create backend driver with runtime code
    let driver = BackendDriver::new()?;

    // Phase 1: Generate high-level IR instructions from HIR
    let (instructions, function_labels) = {
        let mut cg = HirCodegen::new();
        cg.generate(hir)?;
        cg.get_output()
    };

    // Phase 2: Identify function boundaries
    let function_boundaries = driver.discover_function_boundaries(&instructions, &function_labels);

    // Phase 3: Assign label IDs and lower functions
    let (ir_to_machine_labels, label_id_counter) = driver.assign_label_ids(&instructions, 0);
    let all_machine_instructions = driver.lower_functions(
        &instructions,
        function_labels.clone(),
        function_boundaries,
        ir_to_machine_labels.clone(),
        label_id_counter,
    )?;

    // Phase 4: Combine runtime and user code
    let final_instructions = driver.combine_runtime_and_user_code(all_machine_instructions);

    // Phase 5: Emit machine code
    let mut x86_emitter = X86Emitter::new();

    // Set up all labels (runtime + user)
    let all_function_labels = driver.build_final_labels(&function_labels, &ir_to_machine_labels);
    let runtime_label_count = driver.runtime_label_count();
    x86_emitter.set_function_labels(all_function_labels, runtime_label_count);

    let code = x86_emitter
        .emit_all(&final_instructions)
        .map_err(|e| CodegenError { message: e })?;

    let (_, symbols) = x86_emitter.get_output();

    // Phase 6: Generate ELF executable
    let elf_writer = ElfWriter::new();
    let elf = elf_writer.generate_elf(&code, &symbols);
    Ok(elf)
}
