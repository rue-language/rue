// ============================================================================
// Backend (code generation and linking)
// ============================================================================

/// Compile analyzed functions to a binary.
///
/// This backend handles both architectures. It:
/// 1. Generates machine code for each function in parallel
/// 2. Creates object files with relocations
/// 3. Links them into an executable
///
/// This function is used by the sole one-shot compilation adapter.
pub(crate) fn compile_backend(
    functions: &[FunctionWithCfg],
    type_pool: &TypeInternPool,
    strings: &[String],
    interner: &ThreadedRodeo,
    options: &CompileOptions,
    warnings: &[CompileWarning],
) -> MultiErrorResult<CompileOutput> {
    // Check for main function
    let _main_fn = functions
        .iter()
        .find(|f| f.analyzed.name == "main")
        .ok_or_else(|| {
            CompileErrors::from(CompileError::without_span(ErrorKind::NoMainFunction))
        })?;

    // Generate object files based on target architecture
    let object_files = match options.target.arch() {
        Arch::X86_64 => generate_x86_64_objects(functions, type_pool, strings, interner, options)?,
        Arch::Aarch64 => {
            generate_aarch64_objects(functions, type_pool, strings, interner, options)?
        }
    };

    // Link to executable
    match &options.linker {
        LinkerMode::Internal => link_internal_with_warnings(options, &object_files, warnings),
        LinkerMode::System(linker_cmd) => {
            link_system_with_warnings(options, &object_files, linker_cmd, warnings)
        }
    }
}

/// Generate x86-64 object files for all functions.
fn generate_x86_64_objects(
    functions: &[FunctionWithCfg],
    type_pool: &TypeInternPool,
    strings: &[String],
    interner: &ThreadedRodeo,
    options: &CompileOptions,
) -> MultiErrorResult<Vec<Vec<u8>>> {
    let _span = info_span!("codegen", arch = "x86_64").entered();

    let results: Vec<CompileResult<Vec<u8>>> = functions
        .par_iter()
        .map(|func| {
            let machine_code =
                rue_codegen::x86_64::generate(&func.cfg, type_pool, strings, interner)?;

            let mut obj_builder = ObjectBuilder::new(options.target, &func.analyzed.name)
                .code(machine_code.code)
                .strings(machine_code.strings);

            for reloc in machine_code.relocations {
                let rel_type = match reloc.kind {
                    RelocationKind::X86Pc32 => RelocationType::Pc32,
                    RelocationKind::X86Plt32 => RelocationType::Plt32,
                    RelocationKind::Aarch64AdrpPage21
                    | RelocationKind::Aarch64AddLo12
                    | RelocationKind::Aarch64Call26 => {
                        unreachable!("x86-64 codegen emitted AArch64 relocation {:?}", reloc.kind)
                    }
                };

                obj_builder = obj_builder.relocation(CodeRelocation {
                    offset: reloc.offset,
                    symbol: reloc.symbol,
                    rel_type,
                    addend: reloc.addend,
                });
            }

            Ok(obj_builder.build())
        })
        .collect();

    collect_codegen_results(results, functions.len())
}

/// Generate AArch64 object files for all functions.
fn generate_aarch64_objects(
    functions: &[FunctionWithCfg],
    type_pool: &TypeInternPool,
    strings: &[String],
    interner: &ThreadedRodeo,
    options: &CompileOptions,
) -> MultiErrorResult<Vec<Vec<u8>>> {
    let _span = info_span!("codegen", arch = "aarch64").entered();

    let results: Vec<CompileResult<Vec<u8>>> = functions
        .par_iter()
        .map(|func| {
            let machine_code = rue_codegen::aarch64::generate(
                &func.cfg,
                type_pool,
                strings,
                interner,
                options.target,
            )?;

            let mut obj_builder = ObjectBuilder::new(options.target, &func.analyzed.name)
                .code(machine_code.code)
                .strings(machine_code.strings);

            for reloc in machine_code.relocations {
                let rel_type = match reloc.kind {
                    RelocationKind::Aarch64AdrpPage21 => RelocationType::AdrpPage21,
                    RelocationKind::Aarch64AddLo12 => RelocationType::AddLo12,
                    RelocationKind::Aarch64Call26 => RelocationType::Call26,
                    RelocationKind::X86Pc32 | RelocationKind::X86Plt32 => {
                        unreachable!("AArch64 codegen emitted x86-64 relocation {:?}", reloc.kind)
                    }
                };

                obj_builder = obj_builder.relocation(CodeRelocation {
                    offset: reloc.offset,
                    symbol: reloc.symbol,
                    rel_type,
                    addend: reloc.addend,
                });
            }

            Ok(obj_builder.build())
        })
        .collect();

    collect_codegen_results(results, functions.len())
}

/// Collect codegen results, propagating errors and logging stats.
fn collect_codegen_results(
    results: Vec<CompileResult<Vec<u8>>>,
    function_count: usize,
) -> MultiErrorResult<Vec<Vec<u8>>> {
    let mut object_files = Vec::with_capacity(results.len());
    let mut total_code_bytes = 0usize;

    for result in results {
        let obj = result.map_err(CompileErrors::from)?;
        total_code_bytes += obj.len();
        object_files.push(obj);
    }

    info!(
        function_count,
        code_bytes = total_code_bytes,
        "codegen complete"
    );
    Ok(object_files)
}

/// Machine IR that can hold either x86-64 or AArch64 MIR.
///
/// This enum allows the `--emit mir` and `--emit asm` commands to work
/// with any target architecture.
pub enum Mir {
    /// x86-64 machine IR.
    X86_64(X86Mir),
    /// AArch64 machine IR.
    Aarch64(Aarch64Mir),
}

impl std::fmt::Display for Mir {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Mir::X86_64(mir) => write!(f, "{}", mir),
            Mir::Aarch64(mir) => write!(f, "{}", mir),
        }
    }
}

impl Mir {
    /// Format MIR as assembly text.
    ///
    /// This prints the MIR instructions in assembly-like format.
    /// When called with allocated MIR (post-regalloc), physical registers
    /// are shown (rax, rbx, r12 for x86-64; x0, x1, x19 for aarch64).
    pub fn format_assembly(&self) -> String {
        let mut output = String::new();
        match self {
            Mir::X86_64(mir) => {
                use rue_codegen::x86_64::X86Inst;
                for inst in mir.instructions() {
                    match inst {
                        X86Inst::Label { id } => {
                            output.push_str(&format!("{}:\n", id));
                        }
                        X86Inst::CallRel { symbol_id } => {
                            output.push_str(&format!("    call {}\n", mir.get_symbol(*symbol_id)));
                        }
                        _ => {
                            output.push_str(&format!("    {}\n", inst));
                        }
                    }
                }
            }
            Mir::Aarch64(mir) => {
                use rue_codegen::aarch64::Aarch64Inst;
                for inst in mir.instructions() {
                    match inst {
                        Aarch64Inst::Label { id } => {
                            output.push_str(&format!("{}:\n", id));
                        }
                        Aarch64Inst::Bl { symbol_id } => {
                            output.push_str(&format!("    bl {}\n", mir.get_symbol(*symbol_id)));
                        }
                        _ => {
                            output.push_str(&format!("    {}\n", inst));
                        }
                    }
                }
            }
        }
        output
    }
}

/// Generate MIR from CFG for the given target (for debugging/inspection).
///
/// This returns the MIR before register allocation, with virtual registers.
pub fn generate_mir(
    cfg: &Cfg,
    type_pool: &TypeInternPool,
    interner: &ThreadedRodeo,
    target: Target,
) -> CompileResult<Mir> {
    match target.arch() {
        Arch::X86_64 => {
            let mir = rue_codegen::x86_64::CfgLower::new(cfg, type_pool, interner).lower()?;
            Ok(Mir::X86_64(mir))
        }
        Arch::Aarch64 => {
            let mir =
                rue_codegen::aarch64::CfgLower::new(cfg, type_pool, interner, target).lower()?;
            Ok(Mir::Aarch64(mir))
        }
    }
}

/// Generate MIR after register allocation for the given target (for debugging/inspection).
///
/// This returns the MIR after register allocation, with physical registers.
/// This is closer to the final assembly that will be emitted.
pub fn generate_allocated_mir(
    cfg: &Cfg,
    type_pool: &TypeInternPool,
    interner: &ThreadedRodeo,
    target: Target,
) -> CompileResult<Mir> {
    let num_locals = cfg.num_locals();
    let num_params = cfg.num_params();
    let existing_slots = num_locals + num_params;

    match target.arch() {
        Arch::X86_64 => {
            // Lower CFG to X86Mir with virtual registers
            let mir = rue_codegen::x86_64::CfgLower::new(cfg, type_pool, interner).lower()?;

            // Allocate physical registers
            let (mir, _num_spills, _used_callee_saved) =
                rue_codegen::x86_64::RegAlloc::new(mir, existing_slots).allocate_with_spills()?;

            Ok(Mir::X86_64(mir))
        }
        Arch::Aarch64 => {
            // Lower CFG to Aarch64Mir with virtual registers
            let mir =
                rue_codegen::aarch64::CfgLower::new(cfg, type_pool, interner, target).lower()?;

            // Allocate physical registers
            let (mir, _num_spills, _used_callee_saved) =
                rue_codegen::aarch64::RegAlloc::new(mir, existing_slots).allocate_with_spills()?;

            Ok(Mir::Aarch64(mir))
        }
    }
}

/// Generate liveness debug information for a CFG.
///
/// This performs liveness analysis on the MIR (before register allocation)
/// and returns detailed per-instruction liveness information.
///
/// Used by `--emit liveness` to visualize which values are live at each program point.
pub fn generate_liveness_info(
    cfg: &Cfg,
    type_pool: &TypeInternPool,
    interner: &ThreadedRodeo,
    target: Target,
) -> CompileResult<rue_codegen::LivenessDebugInfo> {
    match target.arch() {
        Arch::X86_64 => {
            let mir = rue_codegen::x86_64::CfgLower::new(cfg, type_pool, interner).lower()?;
            Ok(rue_codegen::x86_64::liveness::analyze_debug(&mir))
        }
        Arch::Aarch64 => {
            let mir =
                rue_codegen::aarch64::CfgLower::new(cfg, type_pool, interner, target).lower()?;
            Ok(rue_codegen::aarch64::liveness::analyze_debug(&mir))
        }
    }
}

/// Generate lowering debug information for a CFG.
///
/// This performs CFG-to-MIR lowering (instruction selection) and returns
/// detailed information about how each CFG instruction maps to MIR instructions.
///
/// Used by `--emit lowering` to visualize the instruction selection process.
pub fn generate_lowering_info(
    cfg: &Cfg,
    type_pool: &TypeInternPool,
    interner: &ThreadedRodeo,
    target: Target,
) -> CompileResult<rue_codegen::LoweringDebugInfo> {
    match target.arch() {
        Arch::X86_64 => {
            let (_mir, debug_info) =
                rue_codegen::x86_64::CfgLower::new(cfg, type_pool, interner).lower_with_debug()?;
            Ok(debug_info)
        }
        Arch::Aarch64 => {
            let (_mir, debug_info) =
                rue_codegen::aarch64::CfgLower::new(cfg, type_pool, interner, target)
                    .lower_with_debug()?;
            Ok(debug_info)
        }
    }
}

/// Generate the actual emitted assembly text for a CFG.
///
/// Unlike `format_assembly()` on Mir which shows MIR instructions,
/// this returns the actual assembly that will be emitted, including
/// prologue/epilogue code that the emitter adds.
///
/// This is useful for debugging and for --emit asm output that accurately
/// reflects what's in the binary.
pub fn generate_emitted_asm(
    cfg: &Cfg,
    type_pool: &TypeInternPool,
    strings: &[String],
    interner: &ThreadedRodeo,
    target: Target,
) -> CompileResult<String> {
    match target.arch() {
        Arch::X86_64 => {
            let (_machine_code, asm) =
                rue_codegen::x86_64::generate_with_asm(cfg, type_pool, strings, interner)?;
            Ok(asm)
        }
        Arch::Aarch64 => {
            let (_machine_code, asm) =
                rue_codegen::aarch64::generate_with_asm(cfg, type_pool, strings, interner, target)?;
            Ok(asm)
        }
    }
}

/// Generate register allocation debug information for a CFG.
///
/// This returns information about the register allocation process,
/// including live ranges, interference edges, and allocation decisions.
/// The output is formatted as a human-readable string.
pub fn generate_regalloc_info(
    cfg: &Cfg,
    type_pool: &TypeInternPool,
    interner: &ThreadedRodeo,
    target: Target,
) -> CompileResult<String> {
    match target.arch() {
        Arch::X86_64 => {
            let debug_info = rue_codegen::x86_64::generate_regalloc_info(cfg, type_pool, interner)?;
            Ok(debug_info.to_string())
        }
        Arch::Aarch64 => {
            let debug_info =
                rue_codegen::aarch64::generate_regalloc_info(cfg, type_pool, interner, target)?;
            Ok(debug_info.to_string())
        }
    }
}

// ============================================================================
use rayon::prelude::*;
use tracing::{info, info_span};

use crate::linking::{link_internal_with_warnings, link_system_with_warnings};
use crate::*;
