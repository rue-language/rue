//! HIR compilation with MIR intermediate step
//!
//! This module demonstrates how to integrate MIR into the compilation pipeline.
//! It's an alternative to compile_hir that uses MIR for potential optimizations.

use crate::backend_driver::BackendDriver;
use crate::elf_writer::ElfWriter;
use crate::mir_to_instructions::MirToInstructions;
use crate::x86_emitter::X86Emitter;
use crate::{CodegenError, format_instructions_as_assembly};
use rue_ir::hir::HirProgram;
use rue_ir::mir_lowering::MirBuilder;
use rue_ir::mir_passes::{CommonSubexpressionElimination, ConstProp, DeadCodeElimination};
#[cfg(debug_assertions)]
use rue_ir::mir_verifier::MirVerifier;
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

    // Step 5: Create backend driver with runtime code
    let driver = BackendDriver::new()?;

    // Step 6: Identify function boundaries
    let function_boundaries = driver.discover_function_boundaries(&instructions, &function_labels);

    // Step 7: Assign label IDs and lower functions
    let (ir_to_machine_labels, label_id_counter) = driver.assign_label_ids(&instructions, 0);
    let all_machine_instructions = driver.lower_functions(
        &instructions,
        function_labels.clone(),
        function_boundaries,
        ir_to_machine_labels.clone(),
        label_id_counter,
    )?;

    // Step 8: Combine runtime and user code
    let final_instructions = driver.combine_runtime_and_user_code(all_machine_instructions);

    // Step 9: Build final labels and generate assembly
    let all_function_labels = driver.build_final_labels(&function_labels, &ir_to_machine_labels);
    let runtime_label_count = driver.runtime_label_count();

    // Generate assembly
    Ok(format_instructions_as_assembly(
        &final_instructions,
        &all_function_labels,
        runtime_label_count,
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

    // Step 5: Create backend driver with runtime code
    let driver = BackendDriver::new()?;

    // Step 6: Identify function boundaries
    let function_boundaries = driver.discover_function_boundaries(&instructions, &function_labels);

    // Step 7: Assign label IDs and lower functions
    let (ir_to_machine_labels, label_id_counter) = driver.assign_label_ids(&instructions, 0);
    let all_machine_instructions = driver.lower_functions(
        &instructions,
        function_labels.clone(),
        function_boundaries,
        ir_to_machine_labels.clone(),
        label_id_counter,
    )?;

    // Step 8: Combine runtime and user code
    let final_instructions = driver.combine_runtime_and_user_code(all_machine_instructions);

    // Step 9: Build final labels
    let all_function_labels = driver.build_final_labels(&function_labels, &ir_to_machine_labels);
    let runtime_label_count = driver.runtime_label_count();

    // Step 10: Generate x86 code
    let mut emitter = X86Emitter::new();
    emitter.set_function_labels(all_function_labels, runtime_label_count);
    let code = emitter
        .emit_all(&final_instructions)
        .map_err(|e| CodegenError { message: e })?;

    // Extract symbol positions from emitter
    let (_, symbols) = emitter.get_output();

    // Step 11: Generate ELF executable
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
