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
use rue_air::TypeInternPool;
use rue_cfg::Cfg;
use rue_error::CompileResult;
use rue_target::Target;

use crate::MachineCode;
use crate::regalloc::RegAllocDebugInfo;

// Re-export from parent
pub use super::{EmittedCode, EmittedRelocation};

fn prepare_backend(
    cfg: &Cfg,
    type_pool: &TypeInternPool,
    interner: &ThreadedRodeo,
    target: Target,
) -> CompileResult<crate::codegen_pipeline::PreparedMir<Aarch64Mir, Reg>> {
    crate::codegen_pipeline::prepare_mir(
        cfg,
        type_pool,
        cfg_lower::RET_REGS.len() as u32,
        || CfgLower::new(cfg, type_pool, interner, target).lower(),
        |mir, existing_slots| RegAlloc::new(mir, existing_slots).allocate_with_spills(),
        |mir| {
            peephole::optimize(mir.instructions_vec_mut());
        },
        schedule::schedule,
        verify::verify_stack_alignment,
    )
}

fn generate_inner<T, Emit>(
    cfg: &Cfg,
    type_pool: &TypeInternPool,
    strings: &[String],
    interner: &ThreadedRodeo,
    target: Target,
    emit: Emit,
) -> CompileResult<T>
where
    Emit: for<'a> FnOnce(Emitter<'a>) -> CompileResult<T>,
{
    let prepared = prepare_backend(cfg, type_pool, interner, target)?;
    let emitter = Emitter::new(
        &prepared.mir,
        prepared.total_locals,
        prepared.num_locals_original,
        prepared.num_params,
        &prepared.used_callee_saved,
        strings,
    )
    .with_sret(prepared.has_sret);
    emit(emitter)
}

/// Generate machine code from CFG.
///
/// This is the main entry point for AArch64 code generation.
pub fn generate(
    cfg: &Cfg,
    type_pool: &TypeInternPool,
    strings: &[String],
    interner: &ThreadedRodeo,
    target: Target,
) -> CompileResult<MachineCode> {
    generate_inner(cfg, type_pool, strings, interner, target, |emitter| {
        // Keep the normal path allocation-free with respect to assembly text.
        let (code, relocations) = emitter.emit()?;
        Ok(MachineCode {
            code,
            relocations,
            strings: strings.to_vec(),
        })
    })
}

/// Generate machine code with assembly text from CFG.
///
/// This returns both machine code bytes and human-readable assembly text
/// showing the actual emitted instructions (including prologue/epilogue).
pub fn generate_with_asm(
    cfg: &Cfg,
    type_pool: &TypeInternPool,
    strings: &[String],
    interner: &ThreadedRodeo,
    target: Target,
) -> CompileResult<(MachineCode, String)> {
    generate_inner(cfg, type_pool, strings, interner, target, |emitter| {
        let emitted = emitter.emit_all()?;
        let asm = emitted.to_asm();
        let machine_code = MachineCode {
            code: emitted.to_bytes(),
            relocations: emitted.relocations,
            strings: strings.to_vec(),
        };
        Ok((machine_code, asm))
    })
}

/// Generate register allocation debug info from CFG.
///
/// This returns information about the register allocation process,
/// including live ranges, interference, and allocation decisions.
pub fn generate_regalloc_info(
    cfg: &Cfg,
    type_pool: &TypeInternPool,
    interner: &ThreadedRodeo,
    target: Target,
) -> CompileResult<RegAllocDebugInfo<Reg>> {
    let num_locals = cfg.num_locals();
    let num_params = cfg.num_params();
    let has_sret =
        crate::cfg_lower::fn_uses_sret_return(cfg, type_pool, cfg_lower::RET_REGS.len() as u32);

    // Lower CFG to Aarch64Mir with virtual registers
    let mir = CfgLower::new(cfg, type_pool, interner, target).lower()?;

    // Allocate physical registers with debug info
    let existing_slots = num_locals + num_params + has_sret as u32;
    let (_mir, _num_spills, _used_callee_saved, debug_info) =
        RegAlloc::new(mir, existing_slots).allocate_with_debug()?;

    Ok(debug_info)
}
