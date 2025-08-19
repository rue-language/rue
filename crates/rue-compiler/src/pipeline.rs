//! Compilation pipeline orchestration
//!
//! This module contains the core compilation pipeline that orchestrates the entire
//! compilation process from HIR to executable binaries.

use rue_codegen::backend::RuntimeProvider;
use rue_codegen::target::TargetRegistry;
use rue_codegen::{CodegenError, Linker, LoweringError, RegisterAllocator, X8664Codegen};
use rue_ir::hir::Hir;
use rue_ir::mir::MirProgram;
use rue_ir::pir::{Label, PIR};
use rue_ir::types::TypeContext;
#[cfg(debug_assertions)]
use rue_lowering::MirVerifier;
use rue_lowering::{MirToPir, lower_hir_to_mir};
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
    pub instructions: Vec<X8664Instr>,
    pub function_labels: HashMap<String, Label>,
}

/// Discover function boundaries by scanning for labels that correspond to function entry points
fn discover_function_boundaries(
    instructions: &[PIR],
    function_labels: &HashMap<String, Label>,
) -> Vec<(usize, usize)> {
    let mut function_boundaries = Vec::new();
    let mut current_start = 0;

    for (i, instr) in instructions.iter().enumerate() {
        if let PIR::Label(label) = instr
            && function_labels
                .values()
                .any(|&func_label| func_label == *label)
        {
            if current_start < i {
                function_boundaries.push((current_start, i));
            }
            current_start = i;
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
    hir: &Hir,
    type_context: TypeContext,
    enable_optimizations: bool,
) -> Result<CompilationIntermediateResult, CompileError> {
    // Step 1: Lower HIR to MIR with TypeContext
    info!(target: "rue::mir", "Lowering HIR to MIR");
    let mut mir = lower_hir_to_mir(hir, type_context);

    // Step 2: Print MIR for debugging
    debug!(target: "rue::mir", mir = %mir, "MIR from HIR before optimization");

    // Step 3: Apply MIR optimizations and verify
    if enable_optimizations {
        info!(target: "rue::optimize", "Running optimization passes");
        optimize_and_verify_mir(&mut mir, enable_optimizations)?;
    }

    // Step 4: Lower MIR to Instructions
    info!(target: "rue::codegen", "Lowering MIR to instructions");
    let mut mir_lowerer = MirToPir::with_type_context(mir.type_context.clone());
    let instructions = mir_lowerer.lower_program(&mir);
    let function_labels = mir_lowerer.get_function_labels();

    // Step 5: Identify function boundaries
    let function_boundaries = discover_function_boundaries(&instructions, &function_labels);

    // Step 6: Assign label IDs (starting from 0 for user code)
    let (ir_to_machine_labels, label_id_counter) = assign_label_ids(&instructions, 0);
    let all_machine_instructions = lower_functions(
        &instructions,
        &function_labels,
        function_boundaries,
        &ir_to_machine_labels,
        label_id_counter,
        &mir_lowerer,
    )?;

    Ok(CompilationIntermediateResult {
        instructions: all_machine_instructions,
        function_labels: function_labels.clone(),
    })
}

/// Compile HIR to assembly via MIR
///
/// This function uses the unified compilation pipeline and formats the result as assembly.
/// HIR → MIR → (optimizations) → Instructions → Assembly
#[instrument(skip_all, fields(optimize = enable_optimizations))]
pub fn compile_hir_via_mir_to_assembly(
    hir: &Hir,
    type_context: TypeContext,
    enable_optimizations: bool,
) -> Result<String, CompileError> {
    // Use the unified compilation pipeline
    let intermediate =
        compile_hir_via_mir_to_intermediate(hir, type_context, enable_optimizations)?;

    // Generate assembly from intermediate results
    info!(target: "rue::codegen", "Generating assembly");
    let formatter = TargetRegistry::create_assembly_formatter(TargetRegistry::default_target());
    Ok(formatter.format_assembly(
        &intermediate.instructions,
        &intermediate.function_labels,
        0, // No runtime label count needed
    ))
}

/// Compile HIR to executable via MIR
///
/// This function uses the unified compilation pipeline and generates an ELF executable.
#[instrument(skip_all, fields(optimize = enable_optimizations))]
pub fn compile_hir_via_mir_to_executable(
    hir: &Hir,
    type_context: TypeContext,
    enable_optimizations: bool,
) -> Result<Vec<u8>, CompileError> {
    // Use the unified compilation pipeline
    let intermediate =
        compile_hir_via_mir_to_intermediate(hir, type_context, enable_optimizations)?;

    // Generate user code machine code from intermediate results
    info!(target: "rue::codegen", "Generating user code");
    let mut emitter = TargetRegistry::create_emitter(TargetRegistry::default_target());
    emitter.set_function_labels(intermediate.function_labels.clone(), 0);
    let user_code = emitter
        .emit_all(&intermediate.instructions)
        .map_err(rue_codegen::CodegenError::InvalidOperation)?;

    // Extract user symbol positions from emitter
    let (_, user_symbols) = emitter.get_output();

    // Extract external symbol references from emitter
    // We need to cast to the concrete type to access the external references method
    let external_refs = if let Some(x86_emitter) = emitter
        .as_any()
        .downcast_ref::<rue_codegen::target::x86_64::X86Emitter>(
    ) {
        let refs = x86_emitter.get_external_references();
        info!(target: "rue::elf", "X86 emitter found, extracted {} external references", refs.len());
        for (name, offset) in &refs {
            info!(target: "rue::elf", "External reference: {} at offset 0x{:x}", name, offset);
        }
        refs
    } else {
        info!(target: "rue::elf", "Non-X86 emitter found, no external references extracted");
        Vec::new() // No external references for other emitters
    };

    // Now link with the runtime library
    info!(target: "rue::elf", "Linking with runtime library");
    let mut linker = Linker::new();

    // Add the runtime library
    let lib_path = std::env::var("RUE_RUNTIME_LIB").map_err(|_| {
        CompileError::Codegen(CodegenError::InvalidOperation(
            "RUE_RUNTIME_LIB environment variable not set - cannot find runtime library"
                .to_string(),
        ))
    })?;

    linker.add_object_file_from_path(&lib_path)?;

    // Add user code as an object file with external references
    info!(target: "rue::elf", "Adding user code as object file (code size: {}, symbols: {}, external refs: {})", 
          user_code.len(), user_symbols.len(), external_refs.len());
    linker.add_user_code_object_with_externals(&user_code, &user_symbols, &external_refs)?;

    // Link everything together and generate the executable
    info!(target: "rue::elf", "Linking and generating executable");
    let executable = linker.link_executable()?;

    info!(target: "rue::elf", "Generated executable size: {} bytes", executable.len());

    Ok(executable)
}
