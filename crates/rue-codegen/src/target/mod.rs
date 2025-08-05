/// Target-specific code generation
///
/// This module provides target architecture abstractions for code generation.
/// Each target provides implementations for instruction emission, ELF generation,
/// and assembly formatting.
pub mod x86_64;

use rue_ir::pir::Label;
use std::collections::HashMap;

/// Trait for target-specific instruction emitters
///
/// This trait is generic over the instruction type to support multiple architectures.
pub trait InstructionEmitter<Instr> {
    type Error;

    /// Emit machine instructions to bytes
    fn emit_all(&mut self, instructions: &[Instr]) -> Result<Vec<u8>, Self::Error>;

    /// Set function labels for jump resolution
    fn set_function_labels(&mut self, labels: HashMap<String, Label>, runtime_label_count: u32);

    /// Get the emitted code and symbol table
    fn get_output(&self) -> (&[u8], HashMap<String, usize>);

    /// Get data section content and BSS size for ELF generation
    fn get_data_and_bss(&self) -> (&[u8], usize);
}

/// Trait for target-specific executable format writers
pub trait ExecutableWriter {
    /// Generate an executable from machine code and symbols
    fn generate_executable(&self, machine_code: &[u8], symbols: &HashMap<String, usize>)
    -> Vec<u8>;

    /// Generate an executable with data and BSS sections
    fn generate_executable_with_sections(
        &self,
        machine_code: &[u8],
        symbols: &HashMap<String, usize>,
        data_section: &[u8],
        bss_size: usize,
    ) -> Vec<u8>;
}

/// Trait for target-specific assembly formatters  
///
/// This trait is generic over the instruction type to support multiple architectures.
pub trait AssemblyFormatter<Instr> {
    /// Format machine instructions as human-readable assembly
    fn format_assembly(
        &self,
        instructions: &[Instr],
        function_labels: &HashMap<String, Label>,
        runtime_label_count: u32,
    ) -> String;
}

/// Target architecture enumeration
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetArch {
    X86_64,
}

/// Target registry for managing different architectures
pub struct TargetRegistry;

impl TargetRegistry {
    /// Get the default target architecture (currently x86_64)
    pub fn default_target() -> TargetArch {
        TargetArch::X86_64
    }

    /// Create an instruction emitter for the specified target
    pub fn create_emitter(
        target: TargetArch,
    ) -> Box<dyn InstructionEmitter<rue_target::X8664Instr, Error = String>> {
        match target {
            TargetArch::X86_64 => Box::new(x86_64::X86Emitter::new()),
        }
    }

    /// Create an executable writer for the specified target
    pub fn create_executable_writer(target: TargetArch) -> Box<dyn ExecutableWriter> {
        match target {
            TargetArch::X86_64 => Box::new(x86_64::ElfWriter::new()),
        }
    }

    /// Create an assembly formatter for the specified target
    pub fn create_assembly_formatter(
        target: TargetArch,
    ) -> Box<dyn AssemblyFormatter<rue_target::X8664Instr>> {
        match target {
            TargetArch::X86_64 => Box::new(x86_64::X86AssemblyFormatter),
        }
    }
}
