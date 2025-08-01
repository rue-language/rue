/// x86_64 target implementation
///
/// This module provides x86_64-specific implementations for:
/// - Instruction emission (machine code generation)
/// - ELF executable generation  
/// - Assembly formatting
mod assembler;
mod elf;
mod emitter;

// Re-export the main types with appropriate names
pub use assembler::format_instructions_as_assembly;
pub use elf::ElfWriter;
pub use emitter::X86Emitter;

use crate::target::{AssemblyFormatter, ExecutableWriter, InstructionEmitter};
use rue_ir::pir::Label;
use rue_target::X8664Instr;
use std::collections::HashMap;

// Implement the target traits for x86_64

impl InstructionEmitter<X8664Instr> for X86Emitter {
    type Error = String;

    fn emit_all(&mut self, instructions: &[X8664Instr]) -> Result<Vec<u8>, Self::Error> {
        self.emit_all(instructions)
    }

    fn set_function_labels(&mut self, labels: HashMap<String, Label>, runtime_label_count: u32) {
        self.set_function_labels(labels, runtime_label_count);
    }

    fn get_output(&self) -> (&[u8], HashMap<String, usize>) {
        self.get_output()
    }
}

impl ExecutableWriter for ElfWriter {
    fn generate_executable(
        &self,
        machine_code: &[u8],
        symbols: &HashMap<String, usize>,
    ) -> Vec<u8> {
        self.generate_elf(machine_code, symbols)
    }
}

/// x86_64 assembly formatter
pub struct X86AssemblyFormatter;

impl AssemblyFormatter<X8664Instr> for X86AssemblyFormatter {
    fn format_assembly(
        &self,
        instructions: &[X8664Instr],
        function_labels: &HashMap<String, Label>,
        runtime_label_count: u32,
    ) -> String {
        format_instructions_as_assembly(instructions, function_labels, runtime_label_count)
    }
}
