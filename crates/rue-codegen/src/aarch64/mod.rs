//! AArch64 backend for the Rue compiler.
//!
//! This module implements the full AArch64 code generation pipeline:
//!
//! ```text
//! AIR → CFG → Aarch64Mir (virtual registers) → Register Allocation → Machine Code
//! ```
//!
//! The pipeline is split into distinct phases:
//! - `cfg_lower`: Converts CFG to Aarch64Mir with virtual registers
//! - `regalloc`: Assigns physical registers to virtual registers
//! - `emit`: Encodes Aarch64Mir instructions to machine code bytes

mod cfg_lower;
mod emit;
mod liveness;
mod mir;
mod regalloc;

pub use cfg_lower::CfgLower;
pub use emit::Emitter;
pub use mir::{Aarch64Inst, Aarch64Mir, Cond, Operand, Reg, VReg};
pub use regalloc::RegAlloc;

use rue_air::StructDef;
use rue_cfg::CfgBuilder;

use crate::MachineCode;

/// Generate machine code from AIR.
///
/// This is the main entry point for AArch64 code generation.
pub fn generate(
    air: &rue_air::Air,
    struct_defs: &[StructDef],
    num_locals: u32,
    num_params: u32,
    fn_name: &str,
) -> MachineCode {
    // Phase 1: Build CFG from AIR
    let cfg = CfgBuilder::build(air, num_locals, num_params, fn_name);

    // Phase 2: Lower CFG to Aarch64Mir with virtual registers
    let mir = CfgLower::new(&cfg, struct_defs).lower();

    // Phase 3: Allocate physical registers
    let existing_slots = num_locals + num_params;
    let (mir, num_spills, used_callee_saved) =
        RegAlloc::new(mir, existing_slots).allocate_with_spills();

    // Phase 4: Emit machine code bytes
    let total_locals = num_locals + num_spills;
    let (code, relocations) =
        Emitter::new(&mir, total_locals, num_params, &used_callee_saved).emit();

    MachineCode { code, relocations }
}
