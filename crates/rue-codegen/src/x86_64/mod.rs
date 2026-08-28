//! x86-64 backend for the Rue compiler.
//!
//! This module implements the full x86-64 code generation pipeline:
//!
//! ```text
//! CFG → X86Mir (virtual registers) → Register Allocation → Verify → Machine Code
//! ```
//!
//! The pipeline is split into distinct phases:
//! - `cfg_lower`: Converts CFG to X86Mir with virtual registers
//! - `regalloc`: Assigns physical registers to virtual registers
//! - `verify`: Verifies stack alignment invariants (debug mode)
//! - `emit`: Encodes X86Mir instructions to machine code bytes

mod cfg_lower;
mod emit;
pub mod liveness;
mod mir;
mod peephole;
mod regalloc;
mod schedule;
mod verify;

pub use cfg_lower::CfgLower;
pub use emit::Emitter;
pub use mir::{LabelId, Operand, Reg, VReg, X86Inst, X86Mir};
pub use regalloc::RegAlloc;

use lasso::ThreadedRodeo;
use rue_air::FrozenTypeInternPool;
use rue_cfg::ValidatedCfg;
use rue_error::CompileResult;
use tracing::info_span;

// Re-export from parent
pub use super::{EmittedCode, EmittedRelocation, MachineCode};

pub(crate) fn prepare_backend(
    cfg: &ValidatedCfg,
    type_pool: &FrozenTypeInternPool,
    interner: &ThreadedRodeo,
    symbols: crate::MachineSymbolResolver<'_>,
) -> CompileResult<crate::codegen_pipeline::PreparedMir<X86Mir, Reg>> {
    prepare_backend_with_artifacts(
        cfg,
        type_pool,
        interner,
        symbols,
        crate::BackendArtifactRequest::default(),
    )
    .map(|(prepared, _artifacts)| prepared)
}

fn prepare_backend_with_artifacts(
    cfg: &ValidatedCfg,
    type_pool: &FrozenTypeInternPool,
    interner: &ThreadedRodeo,
    symbols: crate::MachineSymbolResolver<'_>,
    request: crate::BackendArtifactRequest,
) -> CompileResult<(
    crate::codegen_pipeline::PreparedMir<X86Mir, Reg>,
    crate::BackendArtifacts,
)> {
    crate::codegen_pipeline::prepare_mir_with_artifacts(
        cfg,
        type_pool,
        interner,
        cfg_lower::ARG_REGS.len() as u32,
        cfg_lower::RET_REGS.len() as u32,
        crate::frame_layout::SavedRegScheme::X86_64,
        rue_air::TargetCAbiFlavor::SysVAmd64,
        &|name| symbols.is_foreign(&symbols.resolve(interner.resolve(&name))),
        |param_storage, local_storage| {
            let (mir, lowering) = if request.lowering {
                let (mir, debug) = CfgLower::new_with_symbols(cfg, type_pool, interner, symbols)
                    .with_param_storage(param_storage)
                    .with_local_storage(local_storage)
                    .lower_with_debug()?;
                (mir, Some(debug))
            } else {
                (
                    CfgLower::new_with_symbols(cfg, type_pool, interner, symbols)
                        .with_param_storage(param_storage)
                        .with_local_storage(local_storage)
                        .lower()?,
                    None,
                )
            };
            let mir_text = request.mir.then(|| mir.to_string());
            Ok((
                mir,
                crate::BackendArtifacts {
                    lowering,
                    mir: mir_text,
                    ..Default::default()
                },
            ))
        },
        |mir, existing_slots, artifacts| {
            let (mir, spills, used_callee_saved, liveness, regalloc) =
                RegAlloc::new_with_artifacts(mir, existing_slots, request.liveness)
                    .allocate_with_artifacts(request.regalloc)?;
            artifacts.liveness = liveness.map(|debug| debug.to_string());
            artifacts.regalloc = regalloc.map(|debug| debug.to_string());
            Ok((mir, spills, used_callee_saved))
        },
        |mir| {
            peephole::optimize(mir.instructions_vec_mut());
        },
        schedule::schedule,
        verify::verify_stack_alignment,
        X86Mir::is_leaf,
    )
}

fn generate_inner(
    cfg: &ValidatedCfg,
    type_pool: &FrozenTypeInternPool,
    strings: &[String],
    interner: &ThreadedRodeo,
    symbols: crate::MachineSymbolResolver<'_>,
    atoms: &[crate::LocalAtomProjection<'_>],
    require_complete_atoms: bool,
    request: crate::BackendArtifactRequest,
) -> CompileResult<crate::BackendProduct> {
    let (mut prepared, mut artifacts) =
        prepare_backend_with_artifacts(cfg, type_pool, interner, symbols, request)?;
    let referenced_strings = prepared
        .mir
        .instructions()
        .iter()
        .filter_map(|inst| match inst {
            X86Inst::StringConstPtr { string_id, .. }
            | X86Inst::StringConstLen { string_id, .. }
            | X86Inst::StringConstCap { string_id, .. } => Some(*string_id),
            _ => None,
        });
    let local_strings = {
        let _span = info_span!("string_table_compaction").entered();
        let (local_strings, string_id_remap) = crate::compact_string_table(
            strings,
            atoms,
            referenced_strings,
            require_complete_atoms,
        )?;
        for inst in prepared.mir.instructions_vec_mut() {
            let string_id = match inst {
                X86Inst::StringConstPtr { string_id, .. }
                | X86Inst::StringConstLen { string_id, .. }
                | X86Inst::StringConstCap { string_id, .. } => string_id,
                _ => continue,
            };
            *string_id = string_id_remap[string_id];
        }
        local_strings
    };
    let _emission_span = info_span!("machine_emission").entered();
    let emitter = Emitter::new(
        &prepared.mir,
        prepared.total_locals,
        prepared.frame_local_slots,
        prepared.param_storage.homed_area_slots(),
        &prepared.used_callee_saved,
        &local_strings,
    )
    .with_sret(prepared.has_sret)
    .with_frame_layout(prepared.frame_layout)
    .with_param_homing(prepared.param_homing.clone());
    let machine_code = if request.asm {
        let emitted = emitter.emit_all()?;
        artifacts.asm = Some(emitted.to_asm());
        MachineCode {
            code: emitted.to_bytes(),
            relocations: emitted.relocations,
            strings: local_strings,
        }
    } else {
        let (code, relocations) = emitter.emit()?;
        MachineCode {
            code,
            relocations,
            strings: local_strings,
        }
    };
    Ok(crate::BackendProduct {
        machine_code,
        artifacts,
    })
}

/// Generate machine code from CFG.
///
/// This is the main entry point for x86-64 code generation.
/// The pipeline is: CFG → X86Mir → Machine Code
pub fn generate(
    cfg: &ValidatedCfg,
    type_pool: &FrozenTypeInternPool,
    strings: &[String],
    interner: &ThreadedRodeo,
) -> CompileResult<MachineCode> {
    Ok(generate_inner(
        cfg,
        type_pool,
        strings,
        interner,
        crate::MachineSymbolResolver::default(),
        &[],
        false,
        crate::BackendArtifactRequest::default(),
    )?
    .machine_code)
}

pub fn generate_with_symbols(
    cfg: &ValidatedCfg,
    type_pool: &FrozenTypeInternPool,
    strings: &[String],
    interner: &ThreadedRodeo,
    symbols: crate::MachineSymbolResolver<'_>,
) -> CompileResult<MachineCode> {
    Ok(generate_inner(
        cfg,
        type_pool,
        strings,
        interner,
        symbols,
        &[],
        false,
        crate::BackendArtifactRequest::default(),
    )?
    .machine_code)
}

pub fn generate_with_symbols_and_atoms(
    cfg: &ValidatedCfg,
    type_pool: &FrozenTypeInternPool,
    strings: &[String],
    interner: &ThreadedRodeo,
    symbols: crate::MachineSymbolResolver<'_>,
    atoms: &[crate::LocalAtomProjection<'_>],
) -> CompileResult<MachineCode> {
    Ok(generate_inner(
        cfg,
        type_pool,
        strings,
        interner,
        symbols,
        atoms,
        true,
        crate::BackendArtifactRequest::default(),
    )?
    .machine_code)
}

/// Run the production backend and retain requested diagnostic projections.
pub fn generate_product_with_symbols_and_atoms(
    cfg: &ValidatedCfg,
    type_pool: &FrozenTypeInternPool,
    strings: &[String],
    interner: &ThreadedRodeo,
    symbols: crate::MachineSymbolResolver<'_>,
    atoms: &[crate::LocalAtomProjection<'_>],
    request: crate::BackendArtifactRequest,
) -> CompileResult<crate::BackendProduct> {
    generate_inner(
        cfg, type_pool, strings, interner, symbols, atoms, true, request,
    )
}
