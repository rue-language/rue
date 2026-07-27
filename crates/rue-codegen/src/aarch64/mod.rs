//! AArch64 backend for the Rue compiler.
//!
//! This module implements the full AArch64 code generation pipeline:
//!
//! ```text
//! CFG → Aarch64Mir (virtual registers) → Register Allocation → Verify → Machine Code
//! ```
//!
//! The pipeline is split into distinct phases:
//! - `cfg_lower`: Converts CFG to Aarch64Mir with virtual registers
//! - `regalloc`: Assigns physical registers to virtual registers
//! - `verify`: Verifies stack alignment invariants (debug mode)
//! - `emit`: Encodes Aarch64Mir instructions to machine code bytes

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
pub use mir::{Aarch64Inst, Aarch64Mir, Cond, Operand, Reg, VReg};
pub use regalloc::RegAlloc;

use lasso::ThreadedRodeo;
use rue_air::FrozenTypeInternPool;
use rue_cfg::ValidatedCfg;
use rue_error::CompileResult;
use rue_target::Target;
use tracing::info_span;

use crate::MachineCode;
// Re-export from parent
pub use super::{EmittedCode, EmittedRelocation};

pub(crate) fn prepare_backend(
    cfg: &ValidatedCfg,
    type_pool: &FrozenTypeInternPool,
    interner: &ThreadedRodeo,
    target: Target,
    symbols: crate::MachineSymbolResolver<'_>,
) -> CompileResult<crate::codegen_pipeline::PreparedMir<Aarch64Mir, Reg>> {
    prepare_backend_with_artifacts(
        cfg,
        type_pool,
        interner,
        target,
        symbols,
        crate::BackendArtifactRequest::default(),
    )
    .map(|(prepared, _artifacts)| prepared)
}

fn prepare_backend_with_artifacts(
    cfg: &ValidatedCfg,
    type_pool: &FrozenTypeInternPool,
    interner: &ThreadedRodeo,
    target: Target,
    symbols: crate::MachineSymbolResolver<'_>,
    request: crate::BackendArtifactRequest,
) -> CompileResult<(
    crate::codegen_pipeline::PreparedMir<Aarch64Mir, Reg>,
    crate::BackendArtifacts,
)> {
    crate::codegen_pipeline::prepare_mir_with_artifacts(
        cfg,
        type_pool,
        cfg_lower::ARG_REGS.len() as u32,
        cfg_lower::RET_REGS.len() as u32,
        crate::frame_layout::SavedRegScheme::Aarch64,
        || {
            let (mir, lowering) = if request.lowering {
                let (mir, debug) =
                    CfgLower::new_with_symbols(cfg, type_pool, interner, target, symbols)
                        .lower_with_debug()?;
                (mir, Some(debug))
            } else {
                (
                    CfgLower::new_with_symbols(cfg, type_pool, interner, target, symbols)
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
    )
}

fn generate_inner(
    cfg: &ValidatedCfg,
    type_pool: &FrozenTypeInternPool,
    strings: &[String],
    interner: &ThreadedRodeo,
    target: Target,
    symbols: crate::MachineSymbolResolver<'_>,
    atoms: &[crate::LocalAtomProjection<'_>],
    require_complete_atoms: bool,
    request: crate::BackendArtifactRequest,
) -> CompileResult<crate::BackendProduct> {
    let (mut prepared, mut artifacts) =
        prepare_backend_with_artifacts(cfg, type_pool, interner, target, symbols, request)?;
    let referenced_strings = prepared
        .mir
        .instructions()
        .iter()
        .filter_map(|inst| match inst {
            Aarch64Inst::StringConstPtr { string_id, .. }
            | Aarch64Inst::StringConstLen { string_id, .. }
            | Aarch64Inst::StringConstCap { string_id, .. } => Some(*string_id),
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
                Aarch64Inst::StringConstPtr { string_id, .. }
                | Aarch64Inst::StringConstLen { string_id, .. }
                | Aarch64Inst::StringConstCap { string_id, .. } => string_id,
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
        prepared.num_locals_original,
        prepared.num_params,
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
/// This is the main entry point for AArch64 code generation.
pub fn generate(
    cfg: &ValidatedCfg,
    type_pool: &FrozenTypeInternPool,
    strings: &[String],
    interner: &ThreadedRodeo,
    target: Target,
) -> CompileResult<MachineCode> {
    Ok(generate_inner(
        cfg,
        type_pool,
        strings,
        interner,
        target,
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
    target: Target,
    symbols: crate::MachineSymbolResolver<'_>,
) -> CompileResult<MachineCode> {
    Ok(generate_inner(
        cfg,
        type_pool,
        strings,
        interner,
        target,
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
    target: Target,
    symbols: crate::MachineSymbolResolver<'_>,
    atoms: &[crate::LocalAtomProjection<'_>],
) -> CompileResult<MachineCode> {
    Ok(generate_inner(
        cfg,
        type_pool,
        strings,
        interner,
        target,
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
    target: Target,
    symbols: crate::MachineSymbolResolver<'_>,
    atoms: &[crate::LocalAtomProjection<'_>],
    request: crate::BackendArtifactRequest,
) -> CompileResult<crate::BackendProduct> {
    generate_inner(
        cfg, type_pool, strings, interner, target, symbols, atoms, true, request,
    )
}
