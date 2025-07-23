//! Utilities for label manipulation in machine instructions

use rue_ir::target::{LabelRef, MachineInstr};

/// Adjusts all label IDs in a list of machine instructions by adding an offset.
///
/// This function is used when combining runtime code with user code, where user
/// code labels need to be shifted to avoid conflicts with runtime labels.
///
/// # Arguments
/// * `instructions` - The machine instructions to adjust
/// * `offset` - The amount to add to each local label ID
///
/// # Returns
/// A new vector of machine instructions with adjusted label IDs
pub fn adjust_label_ids(instructions: Vec<MachineInstr>, offset: u32) -> Vec<MachineInstr> {
    instructions
        .into_iter()
        .map(|instr| match instr {
            MachineInstr::Label { id } => MachineInstr::Label { id: id + offset },
            MachineInstr::Jmp { target } => {
                let adjusted_target = match target {
                    LabelRef::Local(id) => LabelRef::Local(id + offset),
                    global => global,
                };
                MachineInstr::Jmp {
                    target: adjusted_target,
                }
            }
            MachineInstr::JmpCC { cc, target } => {
                let adjusted_target = match target {
                    LabelRef::Local(id) => LabelRef::Local(id + offset),
                    global => global,
                };
                MachineInstr::JmpCC {
                    cc,
                    target: adjusted_target,
                }
            }
            other => other,
        })
        .collect()
}
