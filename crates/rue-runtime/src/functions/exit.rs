//! Exit function implementation

use crate::constants::*;
use crate::machine_runtime::RuntimeContext;
use rue_ir::target::{MachineInstr, Register};

/// Generate exit function: exit(code: i64) -> ()
pub fn generate_exit_function(ctx: &mut RuntimeContext) {
    let exit_label = ctx.define_label("__rue_exit");

    ctx.instructions
        .push(MachineInstr::Label { id: exit_label });
    // rdi = exit code
    ctx.instructions.push(MachineInstr::MovRI64 {
        dest: Register::Rax,
        imm: SYSCALL_EXIT,
    });
    ctx.instructions.push(MachineInstr::Syscall);
}
