//! Data section generation for static strings

use crate::machine_runtime::RuntimeContext;
use rue_ir::target::{LabelRef, MachineInstr, Register};

/// Generate data section with static strings
pub fn generate_data_section(ctx: &mut RuntimeContext) {
    // Jump over data section
    let data_end_label = ctx.new_label();
    ctx.instructions.push(MachineInstr::Jmp {
        target: LabelRef::Local(data_end_label),
    });

    // Newline character
    let newline_label = ctx.define_label("__rue_newline");
    ctx.instructions
        .push(MachineInstr::Label { id: newline_label });
    // Store raw byte for newline (0x0A)
    ctx.instructions.push(MachineInstr::MovRI64 {
        dest: Register::Rax,
        imm: 0x0A,
    });

    // "true" string
    let true_str_label = ctx.define_label("__rue_true_str");
    ctx.instructions
        .push(MachineInstr::Label { id: true_str_label });
    // Store "true" as 4 bytes: 't' 'r' 'u' 'e'
    ctx.instructions.push(MachineInstr::MovRI64 {
        dest: Register::Rax,
        imm: 0x65757274, // "true" in little-endian
    });

    // "false" string
    let false_str_label = ctx.define_label("__rue_false_str");
    ctx.instructions.push(MachineInstr::Label {
        id: false_str_label,
    });
    // Store "false" as 5 bytes: 'f' 'a' 'l' 's' 'e'
    ctx.instructions.push(MachineInstr::MovRI64 {
        dest: Register::Rax,
        imm: 0x65736c6166, // "false" in little-endian
    });

    // "()" string
    let unit_str_label = ctx.define_label("__rue_unit_str");
    ctx.instructions
        .push(MachineInstr::Label { id: unit_str_label });
    ctx.instructions.push(MachineInstr::MovRI64 {
        dest: Register::Rax,
        imm: 0x2928, // "()" in little-endian
    });

    // Buffer space (1024 bytes for input)
    let input_buffer_label = ctx.define_label("__rue_input_buffer");
    ctx.instructions.push(MachineInstr::Label {
        id: input_buffer_label,
    });
    // We'll let the assembler/linker allocate space here

    // End of data section
    ctx.instructions
        .push(MachineInstr::Label { id: data_end_label });
}
