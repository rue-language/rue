//! x86-64 backend for the Rue compiler.
//!
//! This module implements the full x86-64 code generation pipeline:
//!
//! ```text
//! AIR → CFG → X86Mir (virtual registers) → Register Allocation → Machine Code
//! ```
//!
//! The pipeline is split into distinct phases:
//! - `cfg_lower`: Converts CFG to X86Mir with virtual registers
//! - `regalloc`: Assigns physical registers to virtual registers
//! - `emit`: Encodes X86Mir instructions to machine code bytes
//!
//! The old `lower` module (AIR → X86Mir) is kept for backward compatibility.

mod cfg_lower;
mod emit;
mod liveness;
mod lower;
mod mir;
mod regalloc;

pub use cfg_lower::CfgLower;
pub use emit::Emitter;
pub use lower::Lower;
pub use mir::{Operand, Reg, VReg, X86Mir, X86Inst};
pub use regalloc::RegAlloc;

use rue_air::{Air, StructDef};
use rue_cfg::CfgBuilder;

// Re-export from parent
pub use super::{EmittedRelocation, MachineCode};

/// Generate machine code from AIR.
///
/// This is the main entry point for x86-64 code generation.
/// The pipeline is: AIR → CFG → X86Mir → Machine Code
pub fn generate(
    air: &Air,
    struct_defs: &[StructDef],
    num_locals: u32,
    num_params: u32,
    fn_name: &str,
) -> MachineCode {
    // Phase 1: Build CFG from AIR
    let cfg = CfgBuilder::build(air, num_locals, num_params, fn_name);

    // Phase 2: Lower CFG to X86Mir with virtual registers
    let mir = CfgLower::new(&cfg, struct_defs).lower();

    // Phase 3: Allocate physical registers (may add spill slots)
    // Spill slots go after both locals AND parameters to avoid conflicts
    let existing_slots = num_locals + num_params;
    let (mir, num_spills, used_callee_saved) = RegAlloc::new(mir, existing_slots).allocate_with_spills();

    // Phase 4: Emit machine code bytes (with prologue for stack frame setup)
    // Total local slots = local variables + spill slots (params handled separately)
    let total_locals = num_locals + num_spills;
    let (code, relocations) = Emitter::new(&mir, total_locals, num_params, &used_callee_saved).emit();

    MachineCode { code, relocations }
}
