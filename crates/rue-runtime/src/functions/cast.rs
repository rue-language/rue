//! Cast function implementations

use crate::machine_runtime::RuntimeContext;
use rue_ir::target::{MachineInstr, Register};

/// Generate to_i32 function: to_i32(value: i64) -> i32
/// Truncates a 64-bit integer to 32 bits
pub fn generate_to_i32_function(ctx: &mut RuntimeContext) {
    let to_i32_label = ctx.define_label("__rue_to_i32");

    ctx.instructions
        .push(MachineInstr::Label { id: to_i32_label });

    // Input is in RDI (i64)
    // Output will be in RAX (i32)
    // MOV EAX, EDI performs the truncation automatically
    ctx.instructions.push(MachineInstr::MovRR {
        dest: Register::Rax,
        src: Register::Rdi,
    });

    ctx.instructions.push(MachineInstr::Ret);
}

/// Generate to_i64 function: to_i64(value: i32) -> i64
/// Sign-extends a 32-bit integer to 64 bits
pub fn generate_to_i64_function(ctx: &mut RuntimeContext) {
    let to_i64_label = ctx.define_label("__rue_to_i64");

    ctx.instructions
        .push(MachineInstr::Label { id: to_i64_label });

    // Input is in EDI (i32)
    // Output will be in RAX (i64)
    // MOVSXD RAX, EDI performs sign extension from 32 to 64 bits
    ctx.instructions.push(MachineInstr::Movsxd {
        dest: Register::Rax,
        src: Register::Rdi,
    });

    ctx.instructions.push(MachineInstr::Ret);
}
