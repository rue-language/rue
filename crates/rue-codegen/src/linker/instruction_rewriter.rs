// Instruction rewriting patterns for static linking
//
// This module provides patterns and utilities for rewriting instructions
// during the linking process, particularly for converting GOT-relative
// instructions to direct calls in static linking.

use crate::CodegenError;

/// Represents different x86-64 instruction patterns that may need rewriting
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstructionPattern {
    /// Indirect call through GOT: ff 15 xx xx xx xx (call qword ptr [rip+offset])
    IndirectCallGOT,
    /// LEA instruction loading GOT address: 48 8d 3d xx xx xx xx (lea rdi, [rip+offset])
    LeaGOT,
    /// MOV instruction loading from GOT: 48 8b 05 xx xx xx xx (mov rax, [rip+offset])
    MovFromGOT,
}

/// Detects instruction patterns at the given offset
pub fn detect_instruction_pattern(data: &[u8], offset: usize) -> Option<InstructionPattern> {
    // We need to look backwards from the relocation offset to find the instruction
    // GOTPCREL relocations point to the displacement, not the instruction start

    // Try different offsets to handle variations in instruction encoding
    // GOTPCREL relocations typically point to the displacement field
    // which is 2 bytes after the start of the instruction for most cases
    for backward_offset in [2, 3, 1, 0] {
        if offset < backward_offset {
            continue;
        }

        let inst_start = offset - backward_offset;

        // Check for indirect call: ff 15
        if inst_start + 1 < data.len() && data[inst_start] == 0xff && data[inst_start + 1] == 0x15 {
            return Some(InstructionPattern::IndirectCallGOT);
        }

        // Check for LEA with REX prefix: 48 8d 3d or 48 8d 35
        if inst_start + 2 < data.len()
            && data[inst_start] == 0x48
            && data[inst_start + 1] == 0x8d
            && (data[inst_start + 2] == 0x3d || data[inst_start + 2] == 0x35)
        {
            return Some(InstructionPattern::LeaGOT);
        }

        // Check for LEA without REX: 8d 3d or 8d 35
        if inst_start + 1 < data.len()
            && data[inst_start] == 0x8d
            && (data[inst_start + 1] == 0x3d || data[inst_start + 1] == 0x35)
        {
            return Some(InstructionPattern::LeaGOT);
        }

        // Check for MOV from GOT: 48 8b 05
        if inst_start + 2 < data.len()
            && data[inst_start] == 0x48
            && data[inst_start + 1] == 0x8b
            && data[inst_start + 2] == 0x05
        {
            return Some(InstructionPattern::MovFromGOT);
        }
    }

    None
}

/// Rewrite result containing the modified instruction and new offset
pub struct RewriteResult {
    /// The position where the rewritten instruction should be placed
    pub start_offset: usize,
    /// The new instruction bytes
    pub new_bytes: Vec<u8>,
    /// The offset where the relocation value should be written
    pub relocation_offset: usize,
    /// Whether the original instruction was shortened (affects subsequent offsets)
    pub size_delta: isize,
}

/// Rewrite a GOTPCREL instruction to a direct addressing instruction
pub fn rewrite_gotpcrel_instruction(
    data: &[u8],
    relocation_offset: usize,
    pattern: InstructionPattern,
) -> Result<RewriteResult, CodegenError> {
    match pattern {
        InstructionPattern::IndirectCallGOT => {
            // Convert indirect call (ff 15) to direct call (e8)
            // Original: ff 15 xx xx xx xx (6 bytes)
            // New:      e8 xx xx xx xx    (5 bytes)

            // Find the instruction start
            let inst_start = relocation_offset.checked_sub(2).ok_or_else(|| {
                CodegenError::InvalidOperation(
                    "Indirect call instruction start calculation underflow".to_string(),
                )
            })?;

            // Verify the instruction pattern
            if inst_start + 1 >= data.len()
                || data[inst_start] != 0xff
                || data[inst_start + 1] != 0x15
            {
                return Err(CodegenError::InvalidOperation(
                    "Expected indirect call pattern not found at calculated offset".to_string(),
                ));
            }

            Ok(RewriteResult {
                start_offset: inst_start,
                new_bytes: vec![0xe8], // Direct call opcode only - displacement filled by relocation processing
                relocation_offset: inst_start + 1, // Displacement follows immediately
                size_delta: -1,        // Instruction is 1 byte shorter (6 -> 5)
            })
        }
        InstructionPattern::LeaGOT => {
            // LEA instructions loading GOT addresses can often be converted to MOV immediate
            // This is more complex and depends on the register being loaded
            // For now, we don't rewrite LEA instructions
            Err(CodegenError::InvalidOperation(
                "LEA instruction rewriting not yet implemented".to_string(),
            ))
        }
        InstructionPattern::MovFromGOT => {
            // MOV from GOT could potentially be converted to MOV immediate
            // This requires careful analysis of register usage
            Err(CodegenError::InvalidOperation(
                "MOV instruction rewriting not yet implemented".to_string(),
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_indirect_call() {
        let data = vec![0x00, 0xff, 0x15, 0x00, 0x00, 0x00, 0x00];
        let pattern = detect_instruction_pattern(&data, 3);
        assert_eq!(pattern, Some(InstructionPattern::IndirectCallGOT));
    }

    #[test]
    fn test_detect_lea_with_rex() {
        let data = vec![0x48, 0x8d, 0x3d, 0x00, 0x00, 0x00, 0x00];
        let pattern = detect_instruction_pattern(&data, 3);
        assert_eq!(pattern, Some(InstructionPattern::LeaGOT));
    }

    #[test]
    fn test_rewrite_indirect_call() {
        let data = vec![0xff, 0x15, 0x00, 0x00, 0x00, 0x00];
        let result = rewrite_gotpcrel_instruction(
            &data,
            2, // Relocation offset points to displacement
            InstructionPattern::IndirectCallGOT,
        )
        .unwrap();

        assert_eq!(result.start_offset, 0);
        assert_eq!(result.new_bytes, vec![0xe8]);
        assert_eq!(result.relocation_offset, 1);
        assert_eq!(result.size_delta, -1);
    }
}
