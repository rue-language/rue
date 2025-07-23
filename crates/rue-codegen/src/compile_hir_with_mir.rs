//! HIR compilation with MIR intermediate step
//!
//! This module demonstrates how to integrate MIR into the compilation pipeline.
//! It's an alternative to compile_hir that uses MIR for potential optimizations.

use crate::elf_writer::ElfWriter;
use crate::label_utils::adjust_label_ids;
use crate::mir_to_instructions::MirToInstructions;
use crate::x86_emitter::X86Emitter;
use crate::{
    CodegenError, Instruction, Lowering, RegisterAllocator, format_instructions_as_assembly,
};
use rue_ir::hir::HirProgram;
use rue_ir::mir_lowering::MirBuilder;
use rue_ir::mir_passes::{CommonSubexpressionElimination, ConstProp, DeadCodeElimination};
#[cfg(debug_assertions)]
use rue_ir::mir_verifier::MirVerifier;
use rue_ir::target::MachineInstr;
use std::collections::HashMap;
use std::path::Path;

/// Compile HIR to assembly via MIR
///
/// This function demonstrates the MIR pipeline:
/// HIR → MIR → (optimizations) → Instructions → Assembly
pub fn compile_hir_via_mir_to_assembly(hir: &HirProgram) -> Result<String, CodegenError> {
    // Step 1: Lower HIR to MIR
    let mut mir = MirBuilder::lower_program(hir);

    // Step 2: Print MIR for debugging (optional)
    if std::env::var("RUE_DUMP_MIR").is_ok() {
        eprintln!("=== MIR (before optimization) ===");
        eprintln!("{mir}");
        eprintln!("=== END MIR ===");
    }

    // Step 3: Apply MIR optimizations
    // Run constant propagation first
    let mut const_prop = ConstProp::new();
    const_prop.run(&mut mir);

    // Run common subexpression elimination
    let mut cse = CommonSubexpressionElimination::new();
    cse.run(&mut mir);

    // Run dead code elimination last (after other passes may have made code dead)
    let mut dce = DeadCodeElimination::new();
    dce.run(&mut mir);

    if std::env::var("RUE_DUMP_MIR").is_ok() {
        eprintln!("=== MIR (after optimization) ===");
        eprintln!("{mir}");
        eprintln!("=== END MIR ===");
    }

    // Step 3.5: Verify MIR if in debug mode
    #[cfg(debug_assertions)]
    {
        let mut verifier = MirVerifier::new();
        if let Err(errors) = verifier.verify_program(&mir) {
            eprintln!("MIR verification failed:");
            for error in errors {
                eprintln!("  - [{}] {}", error.function, error.message);
            }
            return Err(CodegenError {
                message: "MIR verification failed".to_string(),
            });
        }
    }

    // Step 4: Lower MIR to Instructions
    let mut mir_lowerer = MirToInstructions::new();
    let instructions = mir_lowerer.lower_program(&mir);
    let function_labels = mir_lowerer.get_function_labels();

    // Step 5: Generate runtime code
    let (runtime_instructions, _runtime_labels) =
        rue_runtime::generate_runtime().map_err(|e| CodegenError { message: e })?;

    // Step 6: Process functions with register allocation
    // Identify function boundaries
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

    // Lower each function separately with its own register allocator
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

    // Combine runtime and user code
    let mut final_instructions = Vec::new();
    final_instructions.extend(runtime_instructions);

    // Adjust user code labels to account for runtime labels
    let runtime_label_count = _runtime_labels.values().max().copied().unwrap_or(0) + 1;

    // Adjust all label IDs in user code
    let adjusted_user_instructions =
        adjust_label_ids(all_machine_instructions, runtime_label_count);
    final_instructions.extend(adjusted_user_instructions);

    // Build final function labels map
    let mut all_function_labels = HashMap::new();

    // Add runtime labels
    for (name, label_id) in _runtime_labels {
        all_function_labels.insert(name, crate::LabelId(label_id));
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

    // Generate assembly
    Ok(format_instructions_as_assembly(
        &final_instructions,
        &all_function_labels,
    ))
}

/// Compile HIR to executable via MIR
pub fn compile_hir_via_mir_to_executable(
    hir: &HirProgram,
    _output_path: &Path,
) -> Result<Vec<u8>, CodegenError> {
    // Step 1: Lower HIR to MIR
    let mut mir = MirBuilder::lower_program(hir);

    // Step 2: Print MIR for debugging (optional)
    if std::env::var("RUE_DUMP_MIR").is_ok() {
        eprintln!("=== MIR (before optimization) ===");
        eprintln!("{mir}");
        eprintln!("=== END MIR ===");
    }

    // Step 3: Apply MIR optimizations
    // Run constant propagation first
    let mut const_prop = ConstProp::new();
    const_prop.run(&mut mir);

    // Run common subexpression elimination
    let mut cse = CommonSubexpressionElimination::new();
    cse.run(&mut mir);

    // Run dead code elimination last (after other passes may have made code dead)
    let mut dce = DeadCodeElimination::new();
    dce.run(&mut mir);

    if std::env::var("RUE_DUMP_MIR").is_ok() {
        eprintln!("=== MIR (after optimization) ===");
        eprintln!("{mir}");
        eprintln!("=== END MIR ===");
    }

    // Step 3.5: Verify MIR if in debug mode
    #[cfg(debug_assertions)]
    {
        let mut verifier = MirVerifier::new();
        if let Err(errors) = verifier.verify_program(&mir) {
            eprintln!("MIR verification failed:");
            for error in errors {
                eprintln!("  - [{}] {}", error.function, error.message);
            }
            return Err(CodegenError {
                message: "MIR verification failed".to_string(),
            });
        }
    }

    // Step 4: Lower MIR to Instructions
    let mut mir_lowerer = MirToInstructions::new();
    let instructions = mir_lowerer.lower_program(&mir);
    let function_labels = mir_lowerer.get_function_labels();

    // Step 5: Generate runtime code
    let (_runtime_instructions, _runtime_labels) =
        rue_runtime::generate_runtime().map_err(|e| CodegenError { message: e })?;

    // Step 6-12: Same as compile_hir_via_mir_to_assembly
    // Get assembly first
    let _asm = compile_hir_via_mir_to_assembly(hir)?;

    // Convert assembly to machine code and generate ELF
    // This is a simplified approach - in production we'd want to share code better
    let mut all_machine_instructions = Vec::new();

    // Re-generate the machine instructions (this is inefficient but simple)
    // In a real implementation, we'd refactor to share the lowering logic

    // For now, let's use the compile_hir_to_executable approach
    // Generate runtime code again
    let (runtime_instructions, runtime_labels) =
        rue_runtime::generate_runtime().map_err(|e| CodegenError { message: e })?;

    // Process functions with register allocation (duplicated from above)
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

    let mut label_id_counter = 0u32;
    let mut ir_to_machine_labels = HashMap::new();

    for instr in &instructions {
        if let Instruction::Label(label_id) = instr {
            ir_to_machine_labels.entry(*label_id).or_insert_with(|| {
                let id = label_id_counter;
                label_id_counter += 1;
                id
            });
        }
    }

    for (start, end) in function_boundaries {
        let function_instrs = &instructions[start..end];

        let mut function_allocator = RegisterAllocator::new();
        let mut function_machine_instrs = Vec::new();
        let next_label_id;

        {
            let mut lowering = Lowering::new(&mut function_allocator, label_id_counter);
            lowering.set_function_labels(function_labels.clone());
            lowering.set_label_map(ir_to_machine_labels.clone());

            let mut function_instrs_without_labels = Vec::new();

            for instr in function_instrs {
                match instr {
                    Instruction::Label(label_id) => {
                        if !function_instrs_without_labels.is_empty() {
                            let machine_instrs = lowering
                                .lower(&function_instrs_without_labels)
                                .map_err(|e| CodegenError { message: e })?;
                            function_machine_instrs.extend(machine_instrs);
                            function_instrs_without_labels.clear();
                        }

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

            if !function_instrs_without_labels.is_empty() {
                let machine_instrs = lowering
                    .lower(&function_instrs_without_labels)
                    .map_err(|e| CodegenError { message: e })?;
                function_machine_instrs.extend(machine_instrs);
            }

            next_label_id = lowering.next_label_id();
        }

        Lowering::patch_stack_allocation(&mut function_machine_instrs, &function_allocator);
        all_machine_instructions.extend(function_machine_instrs);
        label_id_counter = next_label_id;
    }

    let mut final_instructions = Vec::new();
    final_instructions.extend(runtime_instructions);

    let runtime_label_count = runtime_labels.values().max().copied().unwrap_or(0) + 1;

    let adjusted_user_instructions =
        adjust_label_ids(all_machine_instructions, runtime_label_count);
    final_instructions.extend(adjusted_user_instructions);

    let mut all_function_labels = HashMap::new();

    for (name, label_id) in runtime_labels {
        all_function_labels.insert(name.clone(), crate::LabelId(label_id));
    }

    for (name, ir_label_id) in &function_labels {
        if let Some(&machine_label_id) = ir_to_machine_labels.get(ir_label_id) {
            all_function_labels.insert(
                name.clone(),
                crate::LabelId(machine_label_id + runtime_label_count),
            );
        }
    }

    // Generate x86 code
    let mut emitter = X86Emitter::new();
    emitter.set_function_labels(all_function_labels.clone());
    let code = emitter
        .emit_all(&final_instructions)
        .map_err(|e| CodegenError { message: e })?;

    // Extract symbol positions from emitter
    let (_, symbols) = emitter.get_output();

    // Generate ELF executable
    let elf_writer = ElfWriter::new();
    Ok(elf_writer.generate_elf(&code, &symbols))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rue_ir::hir::{BinOp, HirBlock, HirExpr, HirFunction, HirLiteral, HirProgram};
    use rue_ir::types::RueType;
    use rue_lexer::Span;

    #[test]
    fn test_compile_via_mir() {
        // Create a simple HIR program
        let hir = HirProgram {
            functions: vec![HirFunction {
                name: "main".to_string(),
                params: vec![],
                return_type: RueType::I32,
                body: HirBlock {
                    statements: vec![],
                    expr: Some(Box::new(HirExpr::Binary {
                        op: BinOp::Add,
                        lhs: Box::new(HirExpr::Literal {
                            lit: HirLiteral::Int32(20),
                            span: Span::dummy(),
                        }),
                        rhs: Box::new(HirExpr::Literal {
                            lit: HirLiteral::Int32(22),
                            span: Span::dummy(),
                        }),
                        ty: RueType::I32,
                        span: Span::dummy(),
                    })),
                },
                span: Span::dummy(),
            }],
        };

        // Set environment variable to see MIR output
        unsafe {
            std::env::set_var("RUE_DUMP_MIR", "1");
        }

        // Compile via MIR
        let asm = compile_hir_via_mir_to_assembly(&hir).unwrap();

        // Verify we got assembly
        assert!(asm.contains("main:"));
        assert!(asm.contains("movl"));

        // Clean up
        unsafe {
            std::env::remove_var("RUE_DUMP_MIR");
        }
    }

    #[test]
    fn test_fibonacci_via_mir() {
        // Create fibonacci HIR program
        let hir = HirProgram {
            functions: vec![
                HirFunction {
                    name: "fibonacci".to_string(),
                    params: vec![("n".to_string(), RueType::I32)],
                    return_type: RueType::I32,
                    body: HirBlock {
                        statements: vec![],
                        expr: Some(Box::new(HirExpr::If {
                            cond: Box::new(HirExpr::Binary {
                                op: BinOp::Le,
                                lhs: Box::new(HirExpr::Var {
                                    name: "n".to_string(),
                                    ty: RueType::I32,
                                    span: Span::dummy(),
                                }),
                                rhs: Box::new(HirExpr::Literal {
                                    lit: HirLiteral::Int32(1),
                                    span: Span::dummy(),
                                }),
                                ty: RueType::Bool,
                                span: Span::dummy(),
                            }),
                            then_block: HirBlock {
                                statements: vec![],
                                expr: Some(Box::new(HirExpr::Var {
                                    name: "n".to_string(),
                                    ty: RueType::I32,
                                    span: Span::dummy(),
                                })),
                            },
                            else_block: Some(HirBlock {
                                statements: vec![],
                                expr: Some(Box::new(HirExpr::Binary {
                                    op: BinOp::Add,
                                    lhs: Box::new(HirExpr::Call {
                                        func: "fibonacci".to_string(),
                                        args: vec![HirExpr::Binary {
                                            op: BinOp::Sub,
                                            lhs: Box::new(HirExpr::Var {
                                                name: "n".to_string(),
                                                ty: RueType::I32,
                                                span: Span::dummy(),
                                            }),
                                            rhs: Box::new(HirExpr::Literal {
                                                lit: HirLiteral::Int32(1),
                                                span: Span::dummy(),
                                            }),
                                            ty: RueType::I32,
                                            span: Span::dummy(),
                                        }],
                                        ty: RueType::I32,
                                        span: Span::dummy(),
                                    }),
                                    rhs: Box::new(HirExpr::Call {
                                        func: "fibonacci".to_string(),
                                        args: vec![HirExpr::Binary {
                                            op: BinOp::Sub,
                                            lhs: Box::new(HirExpr::Var {
                                                name: "n".to_string(),
                                                ty: RueType::I32,
                                                span: Span::dummy(),
                                            }),
                                            rhs: Box::new(HirExpr::Literal {
                                                lit: HirLiteral::Int32(2),
                                                span: Span::dummy(),
                                            }),
                                            ty: RueType::I32,
                                            span: Span::dummy(),
                                        }],
                                        ty: RueType::I32,
                                        span: Span::dummy(),
                                    }),
                                    ty: RueType::I32,
                                    span: Span::dummy(),
                                })),
                            }),
                            ty: RueType::I32,
                            span: Span::dummy(),
                        })),
                    },
                    span: Span::dummy(),
                },
                HirFunction {
                    name: "main".to_string(),
                    params: vec![],
                    return_type: RueType::I32,
                    body: HirBlock {
                        statements: vec![],
                        expr: Some(Box::new(HirExpr::Call {
                            func: "fibonacci".to_string(),
                            args: vec![HirExpr::Literal {
                                lit: HirLiteral::Int32(10),
                                span: Span::dummy(),
                            }],
                            ty: RueType::I32,
                            span: Span::dummy(),
                        })),
                    },
                    span: Span::dummy(),
                },
            ],
        };

        // Compile via MIR
        let result = compile_hir_via_mir_to_assembly(&hir);
        assert!(
            result.is_ok(),
            "Fibonacci should compile successfully via MIR: {:?}",
            result.err()
        );

        let asm = result.unwrap();

        // Verify we got assembly for both functions
        assert!(
            asm.contains("fibonacci:"),
            "Assembly should contain fibonacci function"
        );
        assert!(
            asm.contains("main:"),
            "Assembly should contain main function"
        );

        // The assembly should have proper control flow for fibonacci
        assert!(
            asm.contains("jle") || asm.contains("jg"),
            "Assembly should contain conditional jump for if statement"
        );
        assert!(
            asm.contains("call fibonacci"),
            "Assembly should contain recursive calls to fibonacci"
        );
    }
}
