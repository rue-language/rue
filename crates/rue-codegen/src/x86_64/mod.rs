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

use crate::backend::Backend;
use lasso::ThreadedRodeo;
use rue_air::FrozenTypeInternPool;
use rue_cfg::ValidatedCfg;
use rue_error::CompileResult;
use rue_target::Target;

// Re-export from parent
pub use super::{EmittedCode, EmittedRelocation, MachineCode};

struct X86CodegenBackend;

pub(crate) fn prepare_backend(
    cfg: &ValidatedCfg,
    type_pool: &FrozenTypeInternPool,
    interner: &ThreadedRodeo,
    target: Target,
    symbols: crate::MachineSymbolResolver<'_>,
) -> CompileResult<crate::codegen_pipeline::PreparedMir<X86Mir, Reg>> {
    crate::codegen_pipeline::prepare_mir_with_backend::<X86CodegenBackend>(
        cfg,
        type_pool,
        interner,
        target,
        symbols,
        crate::BackendArtifactRequest::default(),
        crate::GenerationCancellation::NONE,
    )
    .map(|(prepared, _)| prepared)
}

impl Backend for X86CodegenBackend {
    type Mir = X86Mir;
    type Reg = Reg;

    const ARCH: rue_target::Arch = rue_target::Arch::X86_64;
    const ARG_REG_COUNT: u32 = cfg_lower::ARG_REGS.len() as u32;
    const FP_ARG_REG_COUNT: u32 = cfg_lower::FP_ARG_REGS.len() as u32;
    const RETURN_REG_COUNT: u32 = cfg_lower::RET_REGS.len() as u32;
    const SAVED_REG_SCHEME: crate::frame_layout::SavedRegScheme =
        crate::frame_layout::SavedRegScheme::X86_64;
    const TARGET_C_FLAVOR: rue_air::TargetCAbiFlavor = rue_air::TargetCAbiFlavor::SysVAmd64;

    fn lower(
        cfg: &ValidatedCfg,
        type_pool: &FrozenTypeInternPool,
        interner: &ThreadedRodeo,
        _target: rue_target::Target,
        symbols: crate::MachineSymbolResolver<'_>,
        request: crate::BackendArtifactRequest,
        param_storage: &crate::param_storage::ParamStoragePlan,
        local_storage: &crate::local_storage::LocalSlotPlan,
        cancellation: crate::GenerationCancellation<'_>,
    ) -> CompileResult<(Self::Mir, crate::BackendArtifacts)> {
        let (mir, lowering) = if request.lowering {
            let (mir, debug) = CfgLower::new_with_symbols(cfg, type_pool, interner, symbols)
                .with_param_storage(param_storage)
                .with_local_storage(local_storage)
                .with_cancellation(cancellation)
                .lower_with_debug()?;
            (mir, Some(debug))
        } else {
            (
                CfgLower::new_with_symbols(cfg, type_pool, interner, symbols)
                    .with_param_storage(param_storage)
                    .with_local_storage(local_storage)
                    .with_cancellation(cancellation)
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
        cancellation: crate::GenerationCancellation<'_>,
    ) -> CompileResult<(Self::Mir, u32, Vec<Self::Reg>)> {
        let (mir, spills, used, liveness, regalloc) =
            RegAlloc::new_with_artifacts(mir, existing_slots, request.liveness)
                .allocate_with_artifacts(request.regalloc, cancellation)?;
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
        X86Mir::is_leaf(mir)
    }

    fn referenced_string_ids(mir: &Self::Mir) -> Vec<u32> {
        mir.instructions()
            .iter()
            .filter_map(|inst| match inst {
                X86Inst::StringConstPtr { string_id, .. }
                | X86Inst::StringConstLen { string_id, .. }
                | X86Inst::StringConstCap { string_id, .. } => Some(*string_id),
                _ => None,
            })
            .collect()
    }

    fn remap_string_ids(mir: &mut Self::Mir, remap: &std::collections::BTreeMap<u32, u32>) {
        for inst in mir.instructions_vec_mut() {
            let string_id = match inst {
                X86Inst::StringConstPtr { string_id, .. }
                | X86Inst::StringConstLen { string_id, .. }
                | X86Inst::StringConstCap { string_id, .. } => string_id,
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
        cancellation: crate::GenerationCancellation<'_>,
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
        .with_param_homing(prepared.param_homing.clone())
        .with_cancellation(cancellation);
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
/// This is the main entry point for x86-64 code generation.
/// The pipeline is: CFG → X86Mir → Machine Code
pub fn generate(
    cfg: &ValidatedCfg,
    type_pool: &FrozenTypeInternPool,
    strings: &[String],
    interner: &ThreadedRodeo,
    target: rue_target::Target,
) -> CompileResult<MachineCode> {
    Ok(
        crate::codegen_pipeline::generate_with_backend::<X86CodegenBackend>(
            cfg,
            type_pool,
            strings,
            interner,
            target,
            crate::MachineSymbolResolver::default(),
            &[],
            false,
            crate::BackendArtifactRequest::default(),
            crate::GenerationCancellation::NONE,
        )?
        .machine_code,
    )
}

pub fn generate_with_symbols(
    cfg: &ValidatedCfg,
    type_pool: &FrozenTypeInternPool,
    strings: &[String],
    interner: &ThreadedRodeo,
    target: rue_target::Target,
    symbols: crate::MachineSymbolResolver<'_>,
) -> CompileResult<MachineCode> {
    Ok(
        crate::codegen_pipeline::generate_with_backend::<X86CodegenBackend>(
            cfg,
            type_pool,
            strings,
            interner,
            target,
            symbols,
            &[],
            false,
            crate::BackendArtifactRequest::default(),
            crate::GenerationCancellation::NONE,
        )?
        .machine_code,
    )
}

pub fn generate_with_symbols_and_atoms(
    cfg: &ValidatedCfg,
    type_pool: &FrozenTypeInternPool,
    strings: &[String],
    interner: &ThreadedRodeo,
    target: rue_target::Target,
    symbols: crate::MachineSymbolResolver<'_>,
    atoms: &[crate::LocalAtomProjection<'_>],
) -> CompileResult<MachineCode> {
    Ok(
        crate::codegen_pipeline::generate_with_backend::<X86CodegenBackend>(
            cfg,
            type_pool,
            strings,
            interner,
            target,
            symbols,
            atoms,
            true,
            crate::BackendArtifactRequest::default(),
            crate::GenerationCancellation::NONE,
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
    target: rue_target::Target,
    symbols: crate::MachineSymbolResolver<'_>,
    atoms: &[crate::LocalAtomProjection<'_>],
    request: crate::BackendArtifactRequest,
) -> CompileResult<crate::BackendProduct> {
    generate_product_with_symbols_atoms_and_cancellation(
        cfg,
        type_pool,
        strings,
        interner,
        target,
        symbols,
        atoms,
        request,
        crate::GenerationCancellation::NONE,
    )
}

/// [`generate_product_with_symbols_and_atoms`] under a caller-owned
/// cooperative cancellation authority (RUE-1827).
pub fn generate_product_with_symbols_atoms_and_cancellation(
    cfg: &ValidatedCfg,
    type_pool: &FrozenTypeInternPool,
    strings: &[String],
    interner: &ThreadedRodeo,
    target: rue_target::Target,
    symbols: crate::MachineSymbolResolver<'_>,
    atoms: &[crate::LocalAtomProjection<'_>],
    request: crate::BackendArtifactRequest,
    cancellation: crate::GenerationCancellation<'_>,
) -> CompileResult<crate::BackendProduct> {
    crate::codegen_pipeline::generate_with_backend::<X86CodegenBackend>(
        cfg,
        type_pool,
        strings,
        interner,
        target,
        symbols,
        atoms,
        true,
        request,
        cancellation,
    )
}
