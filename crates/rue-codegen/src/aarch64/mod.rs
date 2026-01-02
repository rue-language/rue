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

use crate::MachineCode;
use crate::regalloc::RegAllocDebugInfo;

// Re-export from parent
pub use super::{EmittedCode, EmittedRelocation};

/// Generate machine code from CFG.
///
/// This is the main entry point for AArch64 code generation.
pub fn generate(
    cfg: &Cfg,
    type_pool: &TypeInternPool,
    strings: &[String],
    interner: &ThreadedRodeo,
) -> CompileResult<MachineCode> {
    let num_locals = cfg.num_locals();
    let num_params = cfg.num_params();

    // Lower CFG to Aarch64Mir with virtual registers
    let mir = CfgLower::new(cfg, type_pool, strings, interner).lower();

    // Allocate physical registers
    let existing_slots = num_locals + num_params;
    let (mut mir, num_spills, used_callee_saved) =
        RegAlloc::new(mir, existing_slots).allocate_with_spills()?;

    // Apply peephole optimizations after register allocation
    peephole::optimize(mir.instructions_vec_mut());

    // Schedule instructions for better performance
    schedule::schedule(&mut mir);

    // Verify stack alignment in debug builds
    #[cfg(debug_assertions)]
    verify::verify_stack_alignment(&mir)?;

    // Emit machine code bytes
    let total_locals = num_locals + num_spills;
    let (code, relocations) =
        Emitter::new(&mir, total_locals, num_params, &used_callee_saved, strings).emit()?;

    Ok(MachineCode {
        code,
        relocations,
        strings: strings.to_vec(),
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
) -> CompileResult<(MachineCode, String)> {
    let num_locals = cfg.num_locals();
    let num_params = cfg.num_params();

    // Lower CFG to Aarch64Mir with virtual registers
    let mir = CfgLower::new(cfg, type_pool, strings, interner).lower();

    // Allocate physical registers
    let existing_slots = num_locals + num_params;
    let (mut mir, num_spills, used_callee_saved) =
        RegAlloc::new(mir, existing_slots).allocate_with_spills()?;

    // Apply peephole optimizations after register allocation
    peephole::optimize(mir.instructions_vec_mut());

    // Schedule instructions for better performance
    schedule::schedule(&mut mir);

    // Verify stack alignment in debug builds
    #[cfg(debug_assertions)]
    verify::verify_stack_alignment(&mir)?;

    // Emit machine code bytes with assembly text
    let total_locals = num_locals + num_spills;
    let emitted =
        Emitter::new(&mir, total_locals, num_params, &used_callee_saved, strings).emit_all()?;

    let asm = emitted.to_asm();
    let machine_code = MachineCode {
        code: emitted.to_bytes(),
        relocations: emitted.relocations,
        strings: strings.to_vec(),
    };

    Ok((machine_code, asm))
}

/// Generate register allocation debug info from CFG.
///
/// This returns information about the register allocation process,
/// including live ranges, interference, and allocation decisions.
pub fn generate_regalloc_info(
    cfg: &Cfg,
    type_pool: &TypeInternPool,
    strings: &[String],
    interner: &ThreadedRodeo,
) -> CompileResult<RegAllocDebugInfo<Reg>> {
    let num_locals = cfg.num_locals();
    let num_params = cfg.num_params();

    // Lower CFG to Aarch64Mir with virtual registers
    let mir = CfgLower::new(cfg, type_pool, strings, interner).lower();

    // Allocate physical registers with debug info
    let existing_slots = num_locals + num_params;
    let (_mir, _num_spills, _used_callee_saved, debug_info) =
        RegAlloc::new(mir, existing_slots).allocate_with_debug()?;

    Ok(debug_info)
}
