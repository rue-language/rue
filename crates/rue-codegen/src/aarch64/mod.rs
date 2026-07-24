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

use crate::MachineCode;
use crate::regalloc::RegAllocDebugInfo;

// Re-export from parent
pub use super::{EmittedCode, EmittedRelocation};

pub(crate) fn prepare_backend(
    cfg: &ValidatedCfg,
    type_pool: &FrozenTypeInternPool,
    interner: &ThreadedRodeo,
    target: Target,
    symbols: crate::MachineSymbolResolver<'_>,
) -> CompileResult<crate::codegen_pipeline::PreparedMir<Aarch64Mir, Reg>> {
    crate::codegen_pipeline::prepare_mir(
        cfg,
        type_pool,
        cfg_lower::ARG_REGS.len() as u32,
        cfg_lower::RET_REGS.len() as u32,
        crate::frame_layout::SavedRegScheme::Aarch64,
        || CfgLower::new_with_symbols(cfg, type_pool, interner, target, symbols).lower(),
        |mir, existing_slots| RegAlloc::new(mir, existing_slots).allocate_with_spills(),
        |mir| {
            peephole::optimize(mir.instructions_vec_mut());
        },
        schedule::schedule,
        verify::verify_stack_alignment,
    )
}

fn generate_inner<T, Emit>(
    cfg: &ValidatedCfg,
    type_pool: &FrozenTypeInternPool,
    strings: &[String],
    interner: &ThreadedRodeo,
    target: Target,
    symbols: crate::MachineSymbolResolver<'_>,
    atoms: &[crate::LocalAtomProjection<'_>],
    require_complete_atoms: bool,
    emit: Emit,
) -> CompileResult<(T, Vec<String>)>
where
    Emit: for<'a> FnOnce(Emitter<'a>) -> CompileResult<T>,
{
    let mut prepared = prepare_backend(cfg, type_pool, interner, target, symbols)?;
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
    let (local_strings, string_id_remap) =
        crate::compact_string_table(strings, atoms, referenced_strings, require_complete_atoms)?;
    for inst in prepared.mir.instructions_vec_mut() {
        let string_id = match inst {
            Aarch64Inst::StringConstPtr { string_id, .. }
            | Aarch64Inst::StringConstLen { string_id, .. }
            | Aarch64Inst::StringConstCap { string_id, .. } => string_id,
            _ => continue,
        };
        *string_id = string_id_remap[string_id];
    }
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
    let emitted = emit(emitter)?;
    Ok((emitted, local_strings))
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
    let ((code, relocations), strings) = generate_inner(
        cfg,
        type_pool,
        strings,
        interner,
        target,
        crate::MachineSymbolResolver::default(),
        &[],
        false,
        |emitter| {
            // Keep the normal path allocation-free with respect to assembly text.
            emitter.emit()
        },
    )?;
    Ok(MachineCode {
        code,
        relocations,
        strings,
    })
}

pub fn generate_with_symbols(
    cfg: &ValidatedCfg,
    type_pool: &FrozenTypeInternPool,
    strings: &[String],
    interner: &ThreadedRodeo,
    target: Target,
    symbols: crate::MachineSymbolResolver<'_>,
) -> CompileResult<MachineCode> {
    let ((code, relocations), strings) = generate_inner(
        cfg,
        type_pool,
        strings,
        interner,
        target,
        symbols,
        &[],
        false,
        |emitter| emitter.emit(),
    )?;
    Ok(MachineCode {
        code,
        relocations,
        strings,
    })
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
    let ((code, relocations), strings) = generate_inner(
        cfg,
        type_pool,
        strings,
        interner,
        target,
        symbols,
        atoms,
        true,
        |emitter| emitter.emit(),
    )?;
    Ok(MachineCode {
        code,
        relocations,
        strings,
    })
}

/// Generate machine code with assembly text from CFG.
///
/// This returns both machine code bytes and human-readable assembly text
/// showing the actual emitted instructions (including prologue/epilogue).
pub fn generate_with_asm(
    cfg: &ValidatedCfg,
    type_pool: &FrozenTypeInternPool,
    strings: &[String],
    interner: &ThreadedRodeo,
    target: Target,
) -> CompileResult<(MachineCode, String)> {
    let (emitted, strings) = generate_inner(
        cfg,
        type_pool,
        strings,
        interner,
        target,
        crate::MachineSymbolResolver::default(),
        &[],
        false,
        |emitter| emitter.emit_all(),
    )?;
    let asm = emitted.to_asm();
    let machine_code = MachineCode {
        code: emitted.to_bytes(),
        relocations: emitted.relocations,
        strings,
    };
    Ok((machine_code, asm))
}

/// Generate register allocation debug info from CFG.
///
/// This returns information about the register allocation process,
/// including live ranges, interference, and allocation decisions.
pub fn generate_regalloc_info(
    cfg: &ValidatedCfg,
    type_pool: &FrozenTypeInternPool,
    interner: &ThreadedRodeo,
    target: Target,
) -> CompileResult<RegAllocDebugInfo<Reg>> {
    let has_sret = crate::codegen_pipeline::validate_pre_lowering_budget(
        cfg,
        type_pool,
        cfg_lower::ARG_REGS.len() as u32,
        cfg_lower::RET_REGS.len() as u32,
        crate::frame_layout::SavedRegScheme::Aarch64,
    )?;

    // Lower CFG to Aarch64Mir with virtual registers
    let mir = CfgLower::new(cfg, type_pool, interner, target).lower()?;

    // Allocate physical registers with debug info
    let existing_slots = crate::codegen_pipeline::checked_slot_sum([
        cfg.num_locals(),
        cfg.num_params(),
        u32::from(has_sret),
    ])
    .ok_or_else(|| crate::codegen_pipeline::frame_budget_error(cfg, None))?;
    let (_mir, num_spills, used_callee_saved, debug_info) =
        RegAlloc::new(mir, existing_slots).allocate_with_debug()?;
    let total_slots = existing_slots
        .checked_add(num_spills)
        .ok_or_else(|| crate::codegen_pipeline::frame_budget_error(cfg, None))?;
    crate::frame_layout::FrameLayout::try_new(
        crate::frame_layout::SavedRegScheme::Aarch64,
        used_callee_saved.len(),
        total_slots,
    )
    .map_err(|_| crate::codegen_pipeline::frame_budget_error(cfg, None))?;

    Ok(debug_info)
}
