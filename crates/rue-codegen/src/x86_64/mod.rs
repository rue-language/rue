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

use crate::regalloc::RegAllocDebugInfo;

use lasso::ThreadedRodeo;
use rue_air::TypeInternPool;
use rue_cfg::Cfg;
use rue_error::CompileResult;

// Re-export from parent
pub use super::{EmittedCode, EmittedRelocation, MachineCode};

/// Generate machine code from CFG.
///
/// This is the main entry point for x86-64 code generation.
/// The pipeline is: CFG → X86Mir → Machine Code
pub fn generate(
    cfg: &Cfg,
    type_pool: &TypeInternPool,
    strings: &[String],
    interner: &ThreadedRodeo,
) -> CompileResult<MachineCode> {
    let num_locals = cfg.num_locals();
    let num_params = cfg.num_params();
    // sret returns reserve one extra frame slot (one past the param area)
    // for the incoming buffer pointer; see crate::cfg_lower::type_uses_sret_return.
    let has_sret =
        crate::cfg_lower::fn_uses_sret_return(cfg, type_pool, cfg_lower::RET_REGS.len() as u32);

    // Lower CFG to X86Mir with virtual registers
    let mir = CfgLower::new(cfg, type_pool, interner).lower();

    // Allocate physical registers (may add spill slots)
    // Spill slots go after locals, parameters AND the sret pointer slot to avoid conflicts
    let existing_slots = num_locals + num_params + has_sret as u32;
    let (mut mir, num_spills, used_callee_saved) =
        RegAlloc::new(mir, existing_slots).allocate_with_spills()?;

    // Apply peephole optimizations after register allocation
    peephole::optimize(mir.instructions_vec_mut());

    // Schedule instructions for better performance
    schedule::schedule(&mut mir);

    // Verify stack alignment. Runs in every build (not `#[cfg(debug_assertions)]`):
    // a stack-misalignment miscompile is an ABI-level correctness bug that must
    // be caught in release too, not silently emitted (RUE-45; cf. RUE-24).
    verify::verify_stack_alignment(&mir)?;

    // Emit machine code bytes (with prologue for stack frame setup)
    // Total local slots = local variables + spill slots (params handled separately)
    let total_locals = num_locals + num_spills;
    // Pass both total_locals (for stack frame size) and num_locals (for param offset calculation)
    // CfgLower generates param offsets based on num_locals, so the prologue must match
    let (code, relocations) = Emitter::new(
        &mir,
        total_locals,
        num_locals,
        num_params,
        &used_callee_saved,
        strings,
    )
    .with_sret(has_sret)
    .emit()?;

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
    let has_sret =
        crate::cfg_lower::fn_uses_sret_return(cfg, type_pool, cfg_lower::RET_REGS.len() as u32);

    // Lower CFG to X86Mir with virtual registers
    let mir = CfgLower::new(cfg, type_pool, interner).lower();

    // Allocate physical registers (may add spill slots)
    let existing_slots = num_locals + num_params + has_sret as u32;
    let (mut mir, num_spills, used_callee_saved) =
        RegAlloc::new(mir, existing_slots).allocate_with_spills()?;

    // Apply peephole optimizations after register allocation
    peephole::optimize(mir.instructions_vec_mut());

    // Schedule instructions for better performance
    schedule::schedule(&mut mir);

    // Verify stack alignment. Runs in every build (not `#[cfg(debug_assertions)]`):
    // a stack-misalignment miscompile is an ABI-level correctness bug that must
    // be caught in release too, not silently emitted (RUE-45; cf. RUE-24).
    verify::verify_stack_alignment(&mir)?;

    // Emit machine code bytes with assembly text
    let total_locals = num_locals + num_spills;
    let emitted = Emitter::new(
        &mir,
        total_locals,
        num_locals,
        num_params,
        &used_callee_saved,
        strings,
    )
    .with_sret(has_sret)
    .emit_all()?;

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
    interner: &ThreadedRodeo,
) -> CompileResult<RegAllocDebugInfo<Reg>> {
    let num_locals = cfg.num_locals();
    let num_params = cfg.num_params();
    let has_sret =
        crate::cfg_lower::fn_uses_sret_return(cfg, type_pool, cfg_lower::RET_REGS.len() as u32);

    // Lower CFG to X86Mir with virtual registers
    let mir = CfgLower::new(cfg, type_pool, interner).lower();

    // Allocate physical registers with debug info
    let existing_slots = num_locals + num_params + has_sret as u32;
    let (_mir, _num_spills, _used_callee_saved, debug_info) =
        RegAlloc::new(mir, existing_slots).allocate_with_debug()?;

    Ok(debug_info)
}
