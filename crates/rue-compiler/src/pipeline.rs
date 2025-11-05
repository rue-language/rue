//! Compilation pipeline orchestration
//!
//! This module contains the core compilation pipeline that orchestrates the entire
//! compilation process from HIR to executable binaries.

use rue_codegen::backend::RuntimeProvider;
use rue_codegen::target::TargetRegistry;
use rue_codegen::{CodegenError, LoweringError, RegisterAllocator, X8664Codegen};
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
#[instrument(skip_all, fields(optimize = enable_optimizations, use_rust_runtime))]
pub fn compile_hir_via_mir_to_intermediate(
    hir: &Hir,
    type_context: TypeContext,
    enable_optimizations: bool,
    use_rust_runtime: bool,
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

    // Step 5: Create runtime provider
    let runtime_provider = RuntimeProvider::new(use_rust_runtime)?;

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
#[instrument(skip_all, fields(optimize = enable_optimizations, use_rust_runtime))]
pub fn compile_hir_via_mir_to_assembly(
    hir: &Hir,
    type_context: TypeContext,
    enable_optimizations: bool,
    use_rust_runtime: bool,
) -> Result<String, CompileError> {
    // Use the unified compilation pipeline
    let intermediate = compile_hir_via_mir_to_intermediate(
        hir,
        type_context,
        enable_optimizations,
        use_rust_runtime,
    )?;

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
#[instrument(skip_all, fields(optimize = enable_optimizations, use_rust_runtime))]
pub fn compile_hir_via_mir_to_executable(
    hir: &Hir,
    type_context: TypeContext,
    enable_optimizations: bool,
    use_rust_runtime: bool,
) -> Result<Vec<u8>, CompileError> {
    // Use the unified compilation pipeline
    let intermediate = compile_hir_via_mir_to_intermediate(
        hir,
        type_context,
        enable_optimizations,
        use_rust_runtime,
    )?;

    if use_rust_runtime {
        // Generate relocatable object file and link with Rust runtime
        info!(target: "rue::elf", "Generating object file for Rust runtime linking");

        let mut emitter = TargetRegistry::create_emitter(TargetRegistry::default_target());
        emitter.set_function_labels(
            intermediate.all_function_labels,
            intermediate.runtime_label_count,
        );
        emitter.set_use_rust_runtime(true);

        // First emit the instructions to generate section data
        let _ = emitter
            .emit_all(&intermediate.final_instructions)
            .map_err(rue_codegen::CodegenError::InvalidOperation)?;

        // Then generate object file with external runtime symbols
        let object_bytes = emitter
            .emit_as_object_file()
            .map_err(rue_codegen::CodegenError::InvalidOperation)?;

        // Link with Rust runtime
        info!(target: "rue::elf", "Linking with Rust runtime");
        let mut linker = rue_codegen::Linker::new();

        // Add user object file
        linker
            .add_object_file("user_code.o".to_string(), &object_bytes)
            .map_err(CompileError::Codegen)?;

        // Add runtime library
        // Check RUE_RUNTIME_PATH environment variable, otherwise use default Buck2 path
        let default_runtime_path = "buck-out/v2/gen/root/2c621926a02f7469/crates/rue-runtime/__rue-runtime__/out/librue_runtime.a";
        let runtime_path =
            std::env::var("RUE_RUNTIME_PATH").unwrap_or_else(|_| default_runtime_path.to_string());

        // Verify the runtime library exists
        if !std::path::Path::new(&runtime_path).exists() {
            return Err(CompileError::Codegen(
                rue_codegen::CodegenError::InvalidOperation(format!(
                    "Runtime library not found at: {}\n\
                     Try rebuilding with: ./buck2 build //crates/rue-runtime:rue-runtime\n\
                     Or set RUE_RUNTIME_PATH environment variable to the library path",
                    runtime_path
                )),
            ));
        }

        debug!(target: "rue::elf", "Using runtime library: {}", runtime_path);
        linker
            .add_object_file_from_path(&runtime_path)
            .map_err(CompileError::Codegen)?;

        // Link all object files
        let linked = linker.link().map_err(CompileError::Codegen)?;

        // Convert symbol table to HashMap<String, usize> for ELF writer
        let symbols: std::collections::HashMap<String, usize> = linked
            .symbols
            .global_symbols()
            .iter()
            .map(|(name, sym)| (name.clone(), sym.address as usize))
            .chain(
                linked
                    .symbols
                    .local_symbols()
                    .iter()
                    .map(|(name, sym)| (name.clone(), sym.address as usize)),
            )
            .collect();

        // Validate that _start symbol exists
        if !symbols.contains_key("_start") {
            return Err(CompileError::Codegen(CodegenError::InvalidOperation(
                "_start symbol not found in symbol table - runtime initialization failed"
                    .to_string(),
            )));
        }

        // Generate ELF executable from linked result
        info!(target: "rue::elf", "Generating final ELF executable");
        let elf_writer = TargetRegistry::create_executable_writer(TargetRegistry::default_target());
        Ok(elf_writer.generate_executable_with_sections(
            &linked.text_section,
            &symbols,
            &linked.rodata_section,
            linked.bss_size as usize,
        ))
    } else {
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

        // Validate that _start symbol exists
        if !symbols.contains_key("_start") {
            return Err(CompileError::Codegen(CodegenError::InvalidOperation(
                "_start symbol not found in symbol table - runtime initialization failed"
                    .to_string(),
            )));
        }

        // Get data and BSS section information
        let (data_section, bss_size) = emitter.get_data_and_bss();

        // Generate ELF executable using trait abstraction with section support
        let elf_writer = TargetRegistry::create_executable_writer(TargetRegistry::default_target());
        Ok(elf_writer.generate_executable_with_sections(&code, &symbols, data_section, bss_size))
    }
}
