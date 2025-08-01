//! Compilation pipeline orchestration
//!
//! This module contains the core compilation pipeline that orchestrates the entire
//! compilation process from HIR to executable binaries.

use rue_codegen::backend::RuntimeProvider;
use rue_codegen::target::TargetRegistry;
use rue_codegen::{LoweringError, RegisterAllocator, X8664Codegen};
use rue_ir::hir::HirProgram;
use rue_ir::mir::MirProgram;
use rue_ir::pir::{Label, PIR};
#[cfg(debug_assertions)]
use rue_lowering::MirVerifier;
use rue_lowering::{MirBuilder, MirToPir};
use rue_optimize::{OptimizationLevel, OptimizationProfileFactory};
use rue_target::X8664Instr;
use std::collections::HashMap;
use tracing::{debug, info, instrument};

/// Compilation errors that can occur during the pipeline
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum CompileError {
    /// Error from codegen phase
    #[error("Codegen error: {0}")]
    Codegen(#[from] rue_codegen::CodegenError),

    /// Error from lowering phase
    #[error("Lowering error: {0}")]
    Lowering(#[from] LoweringError),

    /// MIR verification failed
    #[error("MIR verification failed")]
    MirVerificationFailed,
}

/// Intermediate compilation result containing all the data needed for final output generation
pub struct CompilationIntermediateResult {
    pub final_instructions: Vec<X8664Instr>,
    pub all_function_labels: HashMap<String, Label>,
    pub runtime_label_count: u32,
}

/// Discover function boundaries by scanning for labels that correspond to function entry points
fn discover_function_boundaries(
    instructions: &[PIR],
    function_labels: &HashMap<String, Label>,
) -> Vec<(usize, usize)> {
    let mut function_boundaries = Vec::new();
    let mut current_start = 0;

    for (i, instr) in instructions.iter().enumerate() {
        if let PIR::Label(label) = instr {
            if function_labels
                .values()
                .any(|&func_label| func_label == *label)
            {
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

    function_boundaries
}

/// Assign machine instruction label IDs to IR labels
fn assign_label_ids(instructions: &[PIR], starting_id: u32) -> (HashMap<Label, u32>, u32) {
    let mut ir_to_machine_labels = HashMap::new();
    let mut label_id_counter = starting_id;

    for instr in instructions {
        if let PIR::Label(label) = instr {
            ir_to_machine_labels.entry(*label).or_insert_with(|| {
                let id = label_id_counter;
                label_id_counter += 1;
                id
            });
        }
    }

    (ir_to_machine_labels, label_id_counter)
}

/// Lower functions to machine instructions with proper label handling and stack patching
fn lower_functions(
    instructions: &[PIR],
    function_labels: &HashMap<String, Label>,
    boundaries: Vec<(usize, usize)>,
    ir_to_machine_labels: &HashMap<Label, u32>,
    starting_label_id: u32,
    mir_lowerer: &MirToPir,
) -> Result<Vec<X8664Instr>, CompileError> {
    let mut all_machine_instructions = Vec::new();
    let mut label_id_counter = starting_label_id;

    for (start, end) in boundaries {
        let function_instrs = &instructions[start..end];

        // Find the function name by looking for the first label
        let mut function_name = None;
        for instr in function_instrs {
            if let PIR::Label(label) = instr {
                // Find which function this label belongs to
                for (name, &func_label) in function_labels {
                    if func_label == *label {
                        function_name = Some(name.clone());
                        break;
                    }
                }
                if function_name.is_some() {
                    break;
                }
            }
        }

        let mut function_allocator = RegisterAllocator::new();

        // Set the initial stack offset based on block parameters used in this function
        if let Some(ref name) = function_name {
            let stack_offset = mir_lowerer.get_function_stack_offset(name);
            debug!(
                target: "rue::codegen",
                function = %name,
                stack_offset,
                "Setting initial stack offset for function"
            );
            function_allocator.set_initial_stack_offset(stack_offset);
        }

        let mut function_machine_instrs = Vec::new();
        let next_label_id;

        {
            let mut lowering = X8664Codegen::new(&mut function_allocator, label_id_counter);
            lowering.set_label_map(ir_to_machine_labels);

            // Mark all block parameter offsets for proper handling in Store instructions
            let block_param_offsets = mir_lowerer.get_block_param_offsets();
            for offset in block_param_offsets {
                lowering.mark_block_param_offset(offset);
            }

            // Block parameters now handled via Load/Store with "always spill" approach
            // No need to mark VRegs specially

            // Process instructions in batches between labels
            let mut batch_start = 0;

            for (i, instr) in function_instrs.iter().enumerate() {
                if let PIR::Label(label) = instr {
                    // Lower any instructions before this label
                    if batch_start < i {
                        let batch = &function_instrs[batch_start..i];
                        let machine_instrs = lowering.lower(batch)?;
                        function_machine_instrs.extend(machine_instrs);
                    }

                    // Emit the label
                    let machine_label_id = ir_to_machine_labels[label];
                    function_machine_instrs.push(X8664Instr::Label {
                        id: machine_label_id,
                    });

                    // Next batch starts after this label
                    batch_start = i + 1;
                }
            }

            // Lower any remaining instructions after the last label
            if batch_start < function_instrs.len() {
                let batch = &function_instrs[batch_start..];
                let machine_instrs = lowering.lower(batch)?;
                function_machine_instrs.extend(machine_instrs);
            }

            next_label_id = lowering.next_label_id();
        }

        // Patch stack allocation with actual required space
        X8664Codegen::patch_stack_allocation(&mut function_machine_instrs, &function_allocator);

        // Add this function's instructions to the overall list
        all_machine_instructions.extend(function_machine_instrs);

        // Update the global label counter for the next function
        label_id_counter = next_label_id;
    }

    Ok(all_machine_instructions)
}

/// Build the final function labels map, combining runtime and user labels
fn build_final_labels(
    runtime_provider: &RuntimeProvider,
    function_labels: &HashMap<String, Label>,
    ir_to_machine_labels: &HashMap<Label, u32>,
) -> HashMap<String, Label> {
    let mut all_function_labels = HashMap::new();

    // Add runtime labels
    for (name, &id) in runtime_provider.runtime_labels() {
        all_function_labels.insert(name.clone(), Label::runtime(id));
    }

    // Add user function labels
    let runtime_label_count = runtime_provider.runtime_label_count();
    for (name, ir_label_id) in function_labels {
        if let Some(&machine_label_id) = ir_to_machine_labels.get(ir_label_id) {
            // The machine_label_id already includes the runtime offset from assign_label_ids
            // Use from_machine_id to create the correct label with proper space
            all_function_labels.insert(
                name.clone(),
                Label::from_machine_id(machine_label_id, runtime_label_count),
            );
        }
    }

    all_function_labels
}

/// Helper function to optimize and verify MIR
#[instrument(skip_all, fields(optimize = enable_optimizations))]
fn optimize_and_verify_mir(
    mir: &mut MirProgram,
    enable_optimizations: bool,
) -> Result<(), CompileError> {
    // Determine optimization level based on enable_optimizations flag
    let optimization_level = if enable_optimizations {
        OptimizationLevel::Full
    } else {
        OptimizationLevel::None
    };

    // Run optimizations using the new Pass Manager Framework
    OptimizationProfileFactory::optimize_program(mir, optimization_level);

    debug!(target: "rue::mir", mir = %mir, "MIR after optimization");

    // Verify MIR if in debug mode
    // Note: This is debug-only for performance reasons. The verifier will panic (via error return)
    // if invalid MIR is detected, preventing undefined behavior. In release builds, we trust
    // that the MIR generation and optimization passes are correct.
    #[cfg(debug_assertions)]
    {
        let mut verifier = MirVerifier::new();
        if let Err(errors) = verifier.verify_program(mir) {
            for error in &errors {
                tracing::error!(target: "rue::mir::verify", function = %error.function, "{}", error.message);
            }
            return Err(CompileError::MirVerificationFailed);
        }
    }

    Ok(())
}

/// Unified HIR compilation pipeline via MIR that produces intermediate results
///
/// This function contains the common compilation steps used by both assembly and executable generation.
/// It performs HIR → MIR → optimizations → instructions → machine code generation.
#[instrument(skip_all, fields(optimize = enable_optimizations))]
pub fn compile_hir_via_mir_to_intermediate(
    hir: &HirProgram,
    enable_optimizations: bool,
) -> Result<CompilationIntermediateResult, CompileError> {
    // Step 1: Lower HIR to MIR
    info!(target: "rue::mir", "Lowering HIR to MIR");
    let mut mir = MirBuilder::lower_program(hir);

    // Step 2: Print MIR for debugging
    debug!(target: "rue::mir", mir = %mir, "MIR before optimization");

    // Step 3: Apply MIR optimizations and verify
    if enable_optimizations {
        info!(target: "rue::optimize", "Running optimization passes");
    }
    optimize_and_verify_mir(&mut mir, enable_optimizations)?;

    // Step 4: Lower MIR to Instructions
    info!(target: "rue::codegen", "Lowering MIR to instructions");
    let mut mir_lowerer = MirToPir::new();
    let instructions = mir_lowerer.lower_program(&mir);
    let function_labels = mir_lowerer.get_function_labels();
    // Block parameters now handled via Load/Store with "always spill" approach

    // Step 5: Create runtime provider
    let runtime_provider = RuntimeProvider::new()?;

    // Step 6: Identify function boundaries
    let function_boundaries = discover_function_boundaries(&instructions, &function_labels);

    // Step 7: Get runtime label count and assign label IDs
    let runtime_label_count = runtime_provider.runtime_label_count();
    let (ir_to_machine_labels, label_id_counter) =
        assign_label_ids(&instructions, runtime_label_count);
    let all_machine_instructions = lower_functions(
        &instructions,
        &function_labels,
        function_boundaries,
        &ir_to_machine_labels,
        label_id_counter,
        &mir_lowerer,
    )?;

    // Step 8: Combine runtime and user code
    let final_instructions =
        runtime_provider.combine_runtime_and_user_code(all_machine_instructions);

    // Step 9: Build final labels
    let all_function_labels =
        build_final_labels(&runtime_provider, &function_labels, &ir_to_machine_labels);

    Ok(CompilationIntermediateResult {
        final_instructions,
        all_function_labels,
        runtime_label_count,
    })
}

/// Compile HIR to assembly via MIR
///
/// This function uses the unified compilation pipeline and formats the result as assembly.
/// HIR → MIR → (optimizations) → Instructions → Assembly
#[instrument(skip_all, fields(optimize = enable_optimizations))]
pub fn compile_hir_via_mir_to_assembly(
    hir: &HirProgram,
    enable_optimizations: bool,
) -> Result<String, CompileError> {
    // Use the unified compilation pipeline
    let intermediate = compile_hir_via_mir_to_intermediate(hir, enable_optimizations)?;

    // Generate assembly from intermediate results
    info!(target: "rue::codegen", "Generating assembly");
    let formatter = TargetRegistry::create_assembly_formatter(TargetRegistry::default_target());
    Ok(formatter.format_assembly(
        &intermediate.final_instructions,
        &intermediate.all_function_labels,
        intermediate.runtime_label_count,
    ))
}

/// Compile HIR to executable via MIR
///
/// This function uses the unified compilation pipeline and generates an ELF executable.
#[instrument(skip_all, fields(optimize = enable_optimizations))]
pub fn compile_hir_via_mir_to_executable(
    hir: &HirProgram,
    enable_optimizations: bool,
) -> Result<Vec<u8>, CompileError> {
    // Use the unified compilation pipeline
    let intermediate = compile_hir_via_mir_to_intermediate(hir, enable_optimizations)?;

    // Generate machine code from intermediate results using trait abstraction
    info!(target: "rue::elf", "Generating ELF executable");
    let mut emitter = TargetRegistry::create_emitter(TargetRegistry::default_target());
    emitter.set_function_labels(
        intermediate.all_function_labels,
        intermediate.runtime_label_count,
    );
    let code = emitter
        .emit_all(&intermediate.final_instructions)
        .map_err(rue_codegen::CodegenError::InvalidOperation)?;

    // Extract symbol positions from emitter
    let (_, symbols) = emitter.get_output();

    // Generate ELF executable using trait abstraction
    let elf_writer = TargetRegistry::create_executable_writer(TargetRegistry::default_target());
    Ok(elf_writer.generate_executable(&code, &symbols))
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
        // Look for various conditional jump instructions that might be generated
        let jump_regex = regex::Regex::new(r"\b(jle|jg|je|jne|jl|jge)\b").unwrap();
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
