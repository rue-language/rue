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
#[cfg(test)]
pub(crate) use mir::MAX_ADD_SUB_IMMEDIATE;
pub use mir::{Aarch64Inst, Aarch64Mir, Cond, Operand, Reg, VReg};
pub use regalloc::RegAlloc;

use crate::backend::Backend;
use lasso::ThreadedRodeo;
use rue_air::FrozenTypeInternPool;
use rue_cfg::ValidatedCfg;
use rue_error::CompileResult;
use rue_target::Target;

use crate::MachineCode;
// Re-export from parent
pub use super::{EmittedCode, EmittedRelocation};

struct Aarch64CodegenBackend;

pub(crate) fn prepare_backend(
    cfg: &ValidatedCfg,
    type_pool: &FrozenTypeInternPool,
    interner: &ThreadedRodeo,
    target: Target,
    symbols: crate::MachineSymbolResolver<'_>,
) -> CompileResult<crate::codegen_pipeline::PreparedMir<Aarch64Mir, Reg>> {
    crate::codegen_pipeline::prepare_mir_with_backend::<Aarch64CodegenBackend>(
        cfg,
        type_pool,
        interner,
        target,
        symbols,
        crate::BackendArtifactRequest::default(),
    )
    .map(|(prepared, _)| prepared)
}

impl Backend for Aarch64CodegenBackend {
    type Mir = Aarch64Mir;
    type Reg = Reg;

    const ARCH: rue_target::Arch = rue_target::Arch::Aarch64;
    const ARG_REG_COUNT: u32 = cfg_lower::ARG_REGS.len() as u32;
    const RETURN_REG_COUNT: u32 = cfg_lower::RET_REGS.len() as u32;
    const SAVED_REG_SCHEME: crate::frame_layout::SavedRegScheme =
        crate::frame_layout::SavedRegScheme::Aarch64;
    const TARGET_C_FLAVOR: rue_air::TargetCAbiFlavor = rue_air::TargetCAbiFlavor::Aapcs64;

    fn lower(
        cfg: &ValidatedCfg,
        type_pool: &FrozenTypeInternPool,
        interner: &ThreadedRodeo,
        target: Target,
        symbols: crate::MachineSymbolResolver<'_>,
        request: crate::BackendArtifactRequest,
        param_storage: &crate::param_storage::ParamStoragePlan,
        local_storage: &crate::local_storage::LocalSlotPlan,
    ) -> CompileResult<(Self::Mir, crate::BackendArtifacts)> {
        let (mir, lowering) = if request.lowering {
            let (mir, debug) =
                CfgLower::new_with_symbols(cfg, type_pool, interner, target, symbols)
                    .with_param_storage(param_storage)
                    .with_local_storage(local_storage)
                    .lower_with_debug()?;
            (mir, Some(debug))
        } else {
            (
                CfgLower::new_with_symbols(cfg, type_pool, interner, target, symbols)
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
    }

    fn allocate(
        mir: Self::Mir,
        existing_slots: u32,
        artifacts: &mut crate::BackendArtifacts,
        request: crate::BackendArtifactRequest,
    ) -> CompileResult<(Self::Mir, u32, Vec<Self::Reg>)> {
        let (mir, spills, used, liveness, regalloc) =
            RegAlloc::new_with_artifacts(mir, existing_slots, request.liveness)
                .allocate_with_artifacts(request.regalloc)?;
        artifacts.liveness = liveness.map(|debug| debug.to_string());
        artifacts.regalloc = regalloc.map(|debug| debug.to_string());
        Ok((mir, spills, used))
    }

    fn peephole(mir: &mut Self::Mir) {
        peephole::optimize(mir.instructions_vec_mut());
    }
    fn schedule(mir: &mut Self::Mir) {
        schedule::schedule(mir);
    }
    fn verify(mir: &Self::Mir) -> CompileResult<()> {
        verify::verify_stack_alignment(mir)
    }
    fn is_leaf(mir: &Self::Mir) -> bool {
        Aarch64Mir::is_leaf(mir)
    }

    fn referenced_string_ids(mir: &Self::Mir) -> Vec<u32> {
        mir.instructions()
            .iter()
            .filter_map(|inst| match inst {
                Aarch64Inst::StringConstPtr { string_id, .. }
                | Aarch64Inst::StringConstLen { string_id, .. }
                | Aarch64Inst::StringConstCap { string_id, .. } => Some(*string_id),
                _ => None,
            })
            .collect()
    }

    fn remap_string_ids(mir: &mut Self::Mir, remap: &std::collections::BTreeMap<u32, u32>) {
        for inst in mir.instructions_vec_mut() {
            let string_id = match inst {
                Aarch64Inst::StringConstPtr { string_id, .. }
                | Aarch64Inst::StringConstLen { string_id, .. }
                | Aarch64Inst::StringConstCap { string_id, .. } => string_id,
                _ => continue,
            };
            *string_id = remap[string_id];
        }
    }

    fn emit(
        prepared: &crate::codegen_pipeline::PreparedMir<Self::Mir, Self::Reg>,
        local_strings: &[String],
        request: crate::BackendArtifactRequest,
        artifacts: &mut crate::BackendArtifacts,
    ) -> CompileResult<MachineCode> {
        let emitter = Emitter::new(
            &prepared.mir,
            prepared.total_locals,
            prepared.frame_local_slots,
            prepared.param_storage.homed_area_slots(),
            &prepared.used_callee_saved,
            local_strings,
        )
        .with_sret(prepared.has_sret)
        .with_frame_layout(prepared.frame_layout)
        .with_param_homing(prepared.param_homing.clone());
        if request.asm {
            let emitted = emitter.emit_all()?;
            artifacts.asm = Some(emitted.to_asm());
            Ok(MachineCode {
                code: emitted.to_bytes(),
                relocations: emitted.relocations,
                strings: local_strings.to_vec(),
            })
        } else {
            let (code, relocations) = emitter.emit()?;
            Ok(MachineCode {
                code,
                relocations,
                strings: local_strings.to_vec(),
            })
        }
    }
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
    Ok(
        crate::codegen_pipeline::generate_with_backend::<Aarch64CodegenBackend>(
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
        .machine_code,
    )
}

pub fn generate_with_symbols(
    cfg: &ValidatedCfg,
    type_pool: &FrozenTypeInternPool,
    strings: &[String],
    interner: &ThreadedRodeo,
    target: Target,
    symbols: crate::MachineSymbolResolver<'_>,
) -> CompileResult<MachineCode> {
    Ok(
        crate::codegen_pipeline::generate_with_backend::<Aarch64CodegenBackend>(
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
        .machine_code,
    )
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
    Ok(
        crate::codegen_pipeline::generate_with_backend::<Aarch64CodegenBackend>(
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
        .machine_code,
    )
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
    crate::codegen_pipeline::generate_with_backend::<Aarch64CodegenBackend>(
        cfg, type_pool, strings, interner, target, symbols, atoms, true, request,
    )
}
