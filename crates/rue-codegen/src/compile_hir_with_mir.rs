//! HIR compilation with MIR intermediate step
//!
//! This module demonstrates how to integrate MIR into the compilation pipeline.
//! It's an alternative to compile_hir that uses MIR for potential optimizations.

use crate::backend::Backend;
use crate::elf_writer::ElfWriter;
use crate::mir_to_instructions::MirToInstructions;
use crate::x86_emitter::X86Emitter;
use crate::{CodegenError, format_instructions_as_assembly};
use rue_ir::hir::HirProgram;
use rue_ir::mir::MirProgram;
use rue_lowering::MirBuilder;
#[cfg(debug_assertions)]
use rue_lowering::MirVerifier;
use rue_optimize::{CommonSubexpressionElimination, ConstProp, DeadCodeElimination};

/// Helper function to optimize and verify MIR
fn optimize_and_verify_mir(
    mir: &mut MirProgram,
    enable_optimizations: bool,
) -> Result<(), CodegenError> {
    const MAX_ITERATIONS: usize = 3;

    // Only run optimizations if enabled
    if enable_optimizations {
        // Run optimization passes in a fixed-point loop
        // We run multiple iterations because optimizations can enable further optimizations
        // For example: const prop might enable dead code elimination, which might enable more const prop
        for iteration in 0..MAX_ITERATIONS {
            if std::env::var("RUE_DUMP_MIR").is_ok() && iteration > 0 {
                eprintln!("=== MIR optimization iteration {} ===", iteration + 1);
            }

            // Run constant propagation first
            let mut const_prop = ConstProp::new();
            const_prop.run(mir);

            // Run common subexpression elimination
            let mut cse = CommonSubexpressionElimination::new();
            cse.run(mir);

            // Run dead code elimination last (after other passes may have made code dead)
            let mut dce = DeadCodeElimination::new();
            dce.run(mir);

            // TODO: Once passes return whether they made changes, we can break early
        }
    }

    if std::env::var("RUE_DUMP_MIR").is_ok() {
        eprintln!("=== MIR (after optimization) ===");
        eprintln!("{mir}");
        eprintln!("=== END MIR ===");
    }

    // Verify MIR if in debug mode
    // Note: This is debug-only for performance reasons. The verifier will panic (via error return)
    // if invalid MIR is detected, preventing undefined behavior. In release builds, we trust
    // that the MIR generation and optimization passes are correct.
    #[cfg(debug_assertions)]
    {
        let mut verifier = MirVerifier::new();
        if let Err(errors) = verifier.verify_program(mir) {
            eprintln!("MIR verification failed:");
            for error in errors {
                eprintln!("  - [{}] {}", error.function, error.message);
            }
            return Err(CodegenError::MirVerificationFailed);
        }
    }

    Ok(())
}

/// Compile HIR to assembly via MIR
///
/// This function demonstrates the MIR pipeline:
/// HIR → MIR → (optimizations) → Instructions → Assembly
pub fn compile_hir_via_mir_to_assembly(
    hir: &HirProgram,
    enable_optimizations: bool,
) -> Result<String, CodegenError> {
    // Step 1: Lower HIR to MIR
    let mut mir = MirBuilder::lower_program(hir);

    // Step 2: Print MIR for debugging (optional)
    if std::env::var("RUE_DUMP_MIR").is_ok() {
        eprintln!("=== MIR (before optimization) ===");
        eprintln!("{mir}");
        eprintln!("=== END MIR ===");
    }

    // Step 3: Apply MIR optimizations and verify
    optimize_and_verify_mir(&mut mir, enable_optimizations)?;

    // Step 4: Lower MIR to Instructions
    let mut mir_lowerer = MirToInstructions::new();
    let instructions = mir_lowerer.lower_program(&mir);
    let function_labels = mir_lowerer.get_function_labels();
    // Block parameters now handled via Load/Store with "always spill" approach

    // Step 5: Create backend with runtime code
    let driver = Backend::new()?;

    // Step 6: Identify function boundaries
    let function_boundaries = driver.discover_function_boundaries(&instructions, &function_labels);

    // Step 7: Get runtime label count and assign label IDs
    let runtime_label_count = driver.runtime_label_count();
    let (ir_to_machine_labels, label_id_counter) =
        driver.assign_label_ids(&instructions, runtime_label_count);
    let all_machine_instructions = driver.lower_functions(
        &instructions,
        &function_labels,
        function_boundaries,
        &ir_to_machine_labels,
        label_id_counter,
        &mir_lowerer,
    )?;

    // Step 8: Combine runtime and user code
    let final_instructions = driver.combine_runtime_and_user_code(all_machine_instructions);

    // Step 9: Build final labels and generate assembly
    let all_function_labels = driver.build_final_labels(&function_labels, &ir_to_machine_labels);

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
    enable_optimizations: bool,
) -> Result<Vec<u8>, CodegenError> {
    // Step 1: Lower HIR to MIR
    let mut mir = MirBuilder::lower_program(hir);

    // Step 2: Print MIR for debugging (optional)
    if std::env::var("RUE_DUMP_MIR").is_ok() {
        eprintln!("=== MIR (before optimization) ===");
        eprintln!("{mir}");
        eprintln!("=== END MIR ===");
    }

    // Step 3: Apply MIR optimizations and verify
    optimize_and_verify_mir(&mut mir, enable_optimizations)?;

    // Step 4: Lower MIR to Instructions
    let mut mir_lowerer = MirToInstructions::new();
    let instructions = mir_lowerer.lower_program(&mir);
    let function_labels = mir_lowerer.get_function_labels();
    // Block parameters now handled via Load/Store with "always spill" approach

    // Step 5: Create backend with runtime code
    let driver = Backend::new()?;

    // Step 6: Identify function boundaries
    let function_boundaries = driver.discover_function_boundaries(&instructions, &function_labels);

    // Step 7: Get runtime label count and assign label IDs
    let runtime_label_count = driver.runtime_label_count();
    let (ir_to_machine_labels, label_id_counter) =
        driver.assign_label_ids(&instructions, runtime_label_count);
    let all_machine_instructions = driver.lower_functions(
        &instructions,
        &function_labels,
        function_boundaries,
        &ir_to_machine_labels,
        label_id_counter,
        &mir_lowerer,
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
        .map_err(CodegenError::InvalidOperation)?;

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

        // Note: We would need to set RUE_DUMP_MIR=1 environment variable
        // to see MIR output, but we don't modify environment in tests
        // as it's not thread-safe. Run with RUE_DUMP_MIR=1 cargo test
        // to see MIR output during debugging.

        // Compile via MIR
        let asm = compile_hir_via_mir_to_assembly(&hir, false).unwrap();

        // Verify we got assembly using regex to ensure we match actual labels/instructions
        // and not comments
        let label_regex = regex::Regex::new(r"(?m)^main:").unwrap();
        assert!(
            label_regex.is_match(&asm),
            "Assembly should contain main label. Assembly:\n{asm}"
        );

        // Match mov instruction - be flexible about formatting
        let mov_regex = regex::Regex::new(r"(?i)\bmov\w*\b").unwrap();
        assert!(
            mov_regex.is_match(&asm),
            "Assembly should contain mov instruction. Assembly:\n{asm}"
        )
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
        let result = compile_hir_via_mir_to_assembly(&hir, false);
        assert!(
            result.is_ok(),
            "Fibonacci should compile successfully via MIR: {:?}",
            result.err()
        );

        let asm = result.unwrap();

        // Verify we got assembly for both functions using regex to match actual labels
        let fib_label_regex = regex::Regex::new(r"(?m)^fibonacci:").unwrap();
        assert!(
            fib_label_regex.is_match(&asm),
            "Assembly should contain fibonacci function label"
        );

        let main_label_regex = regex::Regex::new(r"(?m)^main:").unwrap();
        assert!(
            main_label_regex.is_match(&asm),
            "Assembly should contain main function label"
        );

        // The assembly should have proper control flow for fibonacci
        // Use word boundaries to ensure we're matching instructions, not comments
        let jump_regex = regex::Regex::new(r"\b(jle|jg)\b").unwrap();
        assert!(
            jump_regex.is_match(&asm),
            "Assembly should contain conditional jump instruction for if statement"
        );

        // Match call instruction followed by fibonacci (with optional whitespace)
        let call_regex = regex::Regex::new(r"\bcall\s+fibonacci\b").unwrap();
        assert!(
            call_regex.is_match(&asm),
            "Assembly should contain recursive calls to fibonacci"
        );
    }
}
