//! HIR-based compilation functions
//!
//! This module provides functions to compile from HIR to executable code.

use crate::CodegenError;
use crate::elf_writer::ElfWriter;
use crate::hir_codegen::HirCodegen;
use crate::x86_emitter::X86Emitter;
use crate::{Instruction, Lowering, RegisterAllocator, format_instructions_as_assembly};
use rue_ir::hir::HirProgram;
use rue_ir::target::{LabelRef, MachineInstr};
use std::collections::HashMap;

/// Compile HIR to assembly text
pub fn compile_hir_to_assembly(hir: &HirProgram) -> Result<String, CodegenError> {
    // Phase 0: Generate runtime code from rue-runtime crate
    let (runtime_instructions, runtime_labels) =
        rue_runtime::generate_runtime().map_err(|e| CodegenError { message: e })?;

    // Phase 1: Generate high-level IR instructions from HIR
    let (instructions, function_labels) = {
        let mut cg = HirCodegen::new();
        cg.generate(hir)?;
        cg.get_output()
    };

    // Phase 2: Identify function boundaries and allocate registers per function
    let mut function_boundaries = Vec::new();
    let mut current_start = 0;

    for (i, instr) in instructions.iter().enumerate() {
        if let Instruction::Label(label_id) = instr {
            if function_labels.values().any(|&id| id == *label_id) {
                if current_start < i {
                    function_boundaries.push((current_start, i));
                }
                current_start = i;
            }
        }
    }
    if current_start < instructions.len() {
        function_boundaries.push((current_start, instructions.len()));
    }

    // Phase 3: Lower each function separately with its own register allocator
    let mut all_machine_instructions = Vec::new();
    let mut label_id_counter = 0u32;
    let mut ir_to_machine_labels = HashMap::new();

    // First pass: scan for labels and assign machine instruction label IDs
    for instr in &instructions {
        if let Instruction::Label(label_id) = instr {
            ir_to_machine_labels.entry(*label_id).or_insert_with(|| {
                let id = label_id_counter;
                label_id_counter += 1;
                id
            });
        }
    }

    // Second pass: lower each function with its own allocator
    for (start, end) in function_boundaries {
        let function_instrs = &instructions[start..end];

        // Create a fresh register allocator for this function
        let mut function_allocator = RegisterAllocator::new();
        let mut function_machine_instrs = Vec::new();
        let next_label_id;

        {
            let mut lowering = Lowering::new(&mut function_allocator, label_id_counter);
            lowering.set_function_labels(function_labels.clone());
            lowering.set_label_map(ir_to_machine_labels.clone());

            // Process all non-label instructions for this function
            let mut function_instrs_without_labels = Vec::new();

            for instr in function_instrs {
                match instr {
                    Instruction::Label(label_id) => {
                        // First, lower any accumulated instructions
                        if !function_instrs_without_labels.is_empty() {
                            let machine_instrs = lowering
                                .lower(&function_instrs_without_labels)
                                .map_err(|e| CodegenError { message: e })?;
                            function_machine_instrs.extend(machine_instrs);
                            function_instrs_without_labels.clear();
                        }

                        // Then emit the label
                        let machine_label_id = ir_to_machine_labels[label_id];
                        function_machine_instrs.push(MachineInstr::Label {
                            id: machine_label_id,
                        });
                    }
                    _ => {
                        function_instrs_without_labels.push(instr.clone());
                    }
                }
            }

            // Lower any remaining instructions
            if !function_instrs_without_labels.is_empty() {
                let machine_instrs = lowering
                    .lower(&function_instrs_without_labels)
                    .map_err(|e| CodegenError { message: e })?;
                function_machine_instrs.extend(machine_instrs);
            }

            next_label_id = lowering.next_label_id();
        }

        // Patch stack allocation with actual required space
        Lowering::patch_stack_allocation(&mut function_machine_instrs, &function_allocator);

        // Add this function's instructions to the overall list
        all_machine_instructions.extend(function_machine_instrs);

        // Update the global label counter for the next function
        label_id_counter = next_label_id;
    }

    // Phase 4: Combine runtime and user code
    let mut final_instructions = Vec::new();

    // Add runtime instructions first
    final_instructions.extend(runtime_instructions);

    // Adjust user code labels to account for runtime labels
    let runtime_label_count = runtime_labels.values().max().copied().unwrap_or(0) + 1;

    // Adjust all label IDs in user code
    for instr in all_machine_instructions {
        match instr {
            MachineInstr::Label { id } => {
                final_instructions.push(MachineInstr::Label {
                    id: id + runtime_label_count,
                });
            }
            MachineInstr::Jmp { target } => {
                let adjusted_target = match target {
                    LabelRef::Local(id) => LabelRef::Local(id + runtime_label_count),
                    global => global,
                };
                final_instructions.push(MachineInstr::Jmp {
                    target: adjusted_target,
                });
            }
            MachineInstr::JmpCC { cc, target } => {
                let adjusted_target = match target {
                    LabelRef::Local(id) => LabelRef::Local(id + runtime_label_count),
                    global => global,
                };
                final_instructions.push(MachineInstr::JmpCC {
                    cc,
                    target: adjusted_target,
                });
            }
            other => final_instructions.push(other),
        }
    }

    // Phase 5: Generate assembly text
    let mut all_function_labels = HashMap::new();

    // Add runtime labels
    for (name, &id) in &runtime_labels {
        all_function_labels.insert(name.clone(), crate::LabelId(id));
    }

    // Add user function labels (adjusted)
    for (name, ir_label_id) in &function_labels {
        if let Some(&machine_label_id) = ir_to_machine_labels.get(ir_label_id) {
            all_function_labels.insert(
                name.clone(),
                crate::LabelId(machine_label_id + runtime_label_count),
            );
        }
    }

    // Convert machine instructions to assembly text
    let asm_text = format_instructions_as_assembly(&final_instructions, &all_function_labels);
    Ok(asm_text)
}

/// Compile HIR to executable
pub fn compile_hir_to_executable(hir: &HirProgram) -> Result<Vec<u8>, CodegenError> {
    // Phase 0: Generate runtime code from rue-runtime crate
    let (runtime_instructions, runtime_labels) =
        rue_runtime::generate_runtime().map_err(|e| CodegenError { message: e })?;

    // Phase 1: Generate high-level IR instructions from HIR
    let (instructions, function_labels) = {
        let mut cg = HirCodegen::new();
        cg.generate(hir)?;
        cg.get_output()
    };

    // Phase 2: Identify function boundaries and allocate registers per function
    let mut function_boundaries = Vec::new();
    let mut current_start = 0;

    for (i, instr) in instructions.iter().enumerate() {
        if let Instruction::Label(label_id) = instr {
            if function_labels.values().any(|&id| id == *label_id) {
                if current_start < i {
                    function_boundaries.push((current_start, i));
                }
                current_start = i;
            }
        }
    }
    if current_start < instructions.len() {
        function_boundaries.push((current_start, instructions.len()));
    }

    // Phase 3: Lower each function separately with its own register allocator
    let mut all_machine_instructions = Vec::new();
    let mut label_id_counter = 0u32;
    let mut ir_to_machine_labels = HashMap::new();

    // First pass: scan for labels and assign machine instruction label IDs
    for instr in &instructions {
        if let Instruction::Label(label_id) = instr {
            ir_to_machine_labels.entry(*label_id).or_insert_with(|| {
                let id = label_id_counter;
                label_id_counter += 1;
                id
            });
        }
    }

    // Second pass: lower each function with its own allocator
    for (start, end) in function_boundaries {
        let function_instrs = &instructions[start..end];

        // Create a fresh register allocator for this function
        let mut function_allocator = RegisterAllocator::new();
        let mut function_machine_instrs = Vec::new();
        let next_label_id;

        {
            let mut lowering = Lowering::new(&mut function_allocator, label_id_counter);
            lowering.set_function_labels(function_labels.clone());
            lowering.set_label_map(ir_to_machine_labels.clone());

            // Process all non-label instructions for this function
            let mut function_instrs_without_labels = Vec::new();

            for instr in function_instrs {
                match instr {
                    Instruction::Label(label_id) => {
                        // First, lower any accumulated instructions
                        if !function_instrs_without_labels.is_empty() {
                            let machine_instrs = lowering
                                .lower(&function_instrs_without_labels)
                                .map_err(|e| CodegenError { message: e })?;
                            function_machine_instrs.extend(machine_instrs);
                            function_instrs_without_labels.clear();
                        }

                        // Then emit the label
                        let machine_label_id = ir_to_machine_labels[label_id];
                        function_machine_instrs.push(MachineInstr::Label {
                            id: machine_label_id,
                        });
                    }
                    _ => {
                        function_instrs_without_labels.push(instr.clone());
                    }
                }
            }

            // Lower any remaining instructions
            if !function_instrs_without_labels.is_empty() {
                let machine_instrs = lowering
                    .lower(&function_instrs_without_labels)
                    .map_err(|e| CodegenError { message: e })?;
                function_machine_instrs.extend(machine_instrs);
            }

            next_label_id = lowering.next_label_id();
        }

        // Patch stack allocation with actual required space
        Lowering::patch_stack_allocation(&mut function_machine_instrs, &function_allocator);

        // Add this function's instructions to the overall list
        all_machine_instructions.extend(function_machine_instrs);

        // Update the global label counter for the next function
        label_id_counter = next_label_id;
    }

    // Phase 4: Combine runtime and user code
    let mut final_instructions = Vec::new();

    // Add runtime instructions first
    final_instructions.extend(runtime_instructions);

    // Adjust user code labels to account for runtime labels
    let runtime_label_count = runtime_labels.values().max().copied().unwrap_or(0) + 1;

    // Adjust all label IDs in user code
    for instr in all_machine_instructions {
        match instr {
            MachineInstr::Label { id } => {
                final_instructions.push(MachineInstr::Label {
                    id: id + runtime_label_count,
                });
            }
            MachineInstr::Jmp { target } => {
                let adjusted_target = match target {
                    LabelRef::Local(id) => LabelRef::Local(id + runtime_label_count),
                    global => global,
                };
                final_instructions.push(MachineInstr::Jmp {
                    target: adjusted_target,
                });
            }
            MachineInstr::JmpCC { cc, target } => {
                let adjusted_target = match target {
                    LabelRef::Local(id) => LabelRef::Local(id + runtime_label_count),
                    global => global,
                };
                final_instructions.push(MachineInstr::JmpCC {
                    cc,
                    target: adjusted_target,
                });
            }
            other => final_instructions.push(other),
        }
    }

    // Phase 5: Emit machine code
    let mut x86_emitter = X86Emitter::new();

    // Set up all labels (runtime + user)
    let mut all_function_labels = HashMap::new();

    // Add runtime labels
    for (name, &id) in &runtime_labels {
        all_function_labels.insert(name.clone(), crate::LabelId(id));
    }

    // Add user function labels (adjusted)
    for (name, ir_label_id) in &function_labels {
        if let Some(&machine_label_id) = ir_to_machine_labels.get(ir_label_id) {
            all_function_labels.insert(
                name.clone(),
                crate::LabelId(machine_label_id + runtime_label_count),
            );
        }
    }

    x86_emitter.set_function_labels(all_function_labels);

    let code = x86_emitter
        .emit_all(&final_instructions)
        .map_err(|e| CodegenError { message: e })?;

    let (_, symbols) = x86_emitter.get_output();

    // Phase 6: Generate ELF executable
    let elf_writer = ElfWriter::new();
    let elf = elf_writer.generate_elf(&code, &symbols);
    Ok(elf)
}
