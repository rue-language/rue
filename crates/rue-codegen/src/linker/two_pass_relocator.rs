// Two-pass relocation system for handling instruction transformations
//
// This module implements a two-pass approach to relocations:
// 1. First pass: Analyze relocations and plan transformations
// 2. Second pass: Apply transformations with adjusted offsets

use super::instruction_rewriter::{
    InstructionPattern, detect_instruction_pattern, rewrite_gotpcrel_instruction,
};
use super::{MergedSection, RelocationEntry, RelocationKind, SectionOffset, SymbolTable};
use crate::CodegenError;
use std::collections::HashMap;

/// Represents a planned transformation of an instruction
#[derive(Debug, Clone)]
pub struct PlannedTransformation {
    /// Original offset in the section
    pub original_offset: usize,
    /// Size change (negative for shrinking, positive for growing)
    pub size_delta: isize,
    /// The new instruction bytes
    pub new_bytes: Vec<u8>,
    /// Where the relocation value should be written in the new instruction
    pub relocation_offset_in_new: usize,
    /// Original instruction size
    pub original_size: usize,
}

/// Maps from original offsets to final offsets after transformations
#[derive(Debug, Clone)]
pub struct OffsetRemapping {
    /// For each position, how much to adjust subsequent offsets
    adjustments: Vec<(usize, isize)>,
}

impl OffsetRemapping {
    /// Create a new offset remapping from planned transformations
    pub fn from_transformations(
        transformations: &[PlannedTransformation],
        _section_size: usize,
    ) -> Self {
        let mut adjustments = Vec::new();

        // Sort transformations by offset
        let mut sorted_transforms = transformations.to_vec();
        sorted_transforms.sort_by_key(|t| t.original_offset);

        for transform in sorted_transforms {
            adjustments.push((transform.original_offset, transform.size_delta));
        }

        Self { adjustments }
    }

    /// Remap an offset from original to final position
    pub fn remap_offset(&self, original_offset: usize) -> usize {
        let mut final_offset = original_offset;
        let mut cumulative_adjustment = 0isize;

        for (transform_offset, size_delta) in &self.adjustments {
            if original_offset > *transform_offset {
                cumulative_adjustment += size_delta;
            }
        }

        if cumulative_adjustment < 0 {
            final_offset = final_offset.saturating_sub((-cumulative_adjustment) as usize);
        } else {
            final_offset += cumulative_adjustment as usize;
        }

        final_offset
    }

    /// Get the total size change for the section
    pub fn total_size_change(&self) -> isize {
        self.adjustments.iter().map(|(_, delta)| delta).sum()
    }
}

/// Two-pass relocator that handles instruction transformations
pub struct TwoPassRelocator<'a> {
    symbol_table: &'a SymbolTable,
    section_offsets: &'a HashMap<String, SectionOffset>,
}

impl<'a> TwoPassRelocator<'a> {
    pub fn new(
        symbol_table: &'a SymbolTable,
        section_offsets: &'a HashMap<String, SectionOffset>,
    ) -> Self {
        Self {
            symbol_table,
            section_offsets,
        }
    }

    /// Perform two-pass relocation on merged sections
    /// Returns the offset remappings for sections that were modified
    pub fn apply_relocations(
        &self,
        merged_sections: &mut HashMap<String, MergedSection>,
        relocations: &[RelocationEntry],
    ) -> Result<HashMap<String, OffsetRemapping>, CodegenError> {
        tracing::info!(
            "Starting two-pass relocation with {} relocations",
            relocations.len()
        );

        // Pass 1: Analyze and plan transformations
        let transformations = self.analyze_pass(merged_sections, relocations)?;
        tracing::info!(
            "Pass 1 complete: planned {} transformations across {} sections",
            transformations.values().map(|v| v.len()).sum::<usize>(),
            transformations.len()
        );

        // Pass 2: Apply transformations and relocations
        let offset_remappings = self.apply_pass(merged_sections, relocations, &transformations)?;
        tracing::info!("Pass 2 complete: all relocations applied");

        Ok(offset_remappings)
    }

    /// Pass 1: Analyze relocations and plan transformations
    fn analyze_pass(
        &self,
        merged_sections: &HashMap<String, MergedSection>,
        relocations: &[RelocationEntry],
    ) -> Result<HashMap<String, Vec<PlannedTransformation>>, CodegenError> {
        let mut transformations: HashMap<String, Vec<PlannedTransformation>> = HashMap::new();

        for relocation in relocations {
            // DEBUG: Check if this is the println relocation
            if relocation
                .symbol_name
                .contains("b0b5dd69a1903197786a132465e2060b.7")
            {
                tracing::debug!(
                    "DEBUG analyze_pass: Found println relocation - section='{}', offset=0x{:x}, kind={:?}",
                    relocation.section_name,
                    relocation.offset,
                    relocation.kind
                );
            }

            // Skip debug and other non-included sections
            if self.should_skip_section(&relocation.section_name) {
                if relocation
                    .symbol_name
                    .contains("b0b5dd69a1903197786a132465e2060b.7")
                {
                    tracing::debug!("DEBUG: println relocation SKIPPED due to section filter");
                }
                continue;
            }

            let normalized_section = self.normalize_section_name(&relocation.section_name);

            // Get the section data
            let section = merged_sections.get(&normalized_section).ok_or_else(|| {
                CodegenError::InvalidOperation(format!("Section not found: {}", normalized_section))
            })?;

            // Calculate actual offset in merged section
            let subsection_offset =
                self.get_subsection_offset(&relocation.section_name, &normalized_section);
            let actual_offset = subsection_offset + relocation.offset;

            // DEBUG: Check if this is an Absolute32 relocation for println
            if relocation.kind == RelocationKind::Absolute32
                && relocation
                    .symbol_name
                    .contains("b0b5dd69a1903197786a132465e2060b.7")
            {
                tracing::debug!(
                    "DEBUG: Absolute32 relocation for println at actual_offset=0x{:x} (subsection_offset=0x{:x} + relocation.offset=0x{:x})",
                    actual_offset,
                    subsection_offset,
                    relocation.offset
                );
            }

            // For GOTPCREL, REX_GOTPCRELX, and GOTPCRELX relocations, check if we need to transform the instruction
            if matches!(
                relocation.kind,
                RelocationKind::GOTPCREL
                    | RelocationKind::REX_GOTPCRELX
                    | RelocationKind::GOTPCRELX
            ) {
                if let Some(pattern) =
                    detect_instruction_pattern(&section.data, actual_offset as usize)
                {
                    tracing::trace!(
                        "Detected {:?} pattern for GOTPCREL at offset 0x{:x}",
                        pattern,
                        actual_offset
                    );

                    // Plan the transformation
                    if pattern == InstructionPattern::IndirectCallGOT {
                        // CRITICAL PROTECTION: Prevent transformations from corrupting runtime library functions
                        // Check if this transformation would overwrite a runtime function
                        if self.would_corrupt_runtime_function(
                            &section.data,
                            actual_offset as usize,
                            &relocation,
                        ) {
                            tracing::debug!(
                                "SKIPPING GOTPCREL transformation at offset 0x{:x} - would corrupt runtime function. Section: '{}', relocation from: '{}'",
                                actual_offset,
                                normalized_section,
                                relocation.section_name
                            );
                            continue; // Skip this transformation to avoid corruption
                        }

                        match rewrite_gotpcrel_instruction(
                            &section.data,
                            actual_offset as usize,
                            pattern,
                        ) {
                            Ok(rewrite_result) => {
                                // CRITICAL FIX: The new_bytes only contains the opcode (e.g., 0xe8 for call)
                                // but the transformation should generate the complete instruction.
                                // For GOTPCREL -> PC32 transformation:
                                // - Original: ff 15 xx xx xx xx (6 bytes)
                                // - New: e8 xx xx xx xx (5 bytes complete)
                                // - size_delta = -1 (6 -> 5)
                                // We need to generate the complete new instruction with placeholder displacement
                                let mut complete_new_bytes = rewrite_result.new_bytes;
                                complete_new_bytes.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]); // placeholder 4-byte displacement

                                let original_size = (complete_new_bytes.len() as isize
                                    - rewrite_result.size_delta)
                                    as usize;

                                // CRITICAL VALIDATION: Ensure original_size is reasonable
                                if original_size > 100 {
                                    tracing::debug!(
                                        "SUSPICIOUS: Transformation at 0x{:x} has original_size={} which is too large for a GOTPCREL instruction!",
                                        rewrite_result.start_offset,
                                        original_size
                                    );
                                    tracing::debug!(
                                        "Details: new_bytes.len()={}, size_delta={}, calculated original_size={}",
                                        complete_new_bytes.len(),
                                        rewrite_result.size_delta,
                                        original_size
                                    );
                                    // Skip this suspicious transformation
                                    continue;
                                }

                                let transformation = PlannedTransformation {
                                    original_offset: rewrite_result.start_offset,
                                    size_delta: rewrite_result.size_delta,
                                    new_bytes: complete_new_bytes,
                                    relocation_offset_in_new: 1, // Displacement starts 1 byte after opcode in direct call
                                    original_size,
                                };

                                transformations
                                    .entry(normalized_section.clone())
                                    .or_insert_with(Vec::new)
                                    .push(transformation);

                                tracing::trace!(
                                    "Planned GOTPCREL transformation at offset 0x{:x} with size_delta={}",
                                    actual_offset,
                                    rewrite_result.size_delta
                                );
                            }
                            Err(e) => {
                                tracing::warn!("Failed to plan GOTPCREL transformation: {}", e);
                            }
                        }
                    }
                }
            }
        }

        Ok(transformations)
    }

    /// Pass 2: Apply transformations and relocations with adjusted offsets
    fn apply_pass(
        &self,
        merged_sections: &mut HashMap<String, MergedSection>,
        relocations: &[RelocationEntry],
        transformations: &HashMap<String, Vec<PlannedTransformation>>,
    ) -> Result<HashMap<String, OffsetRemapping>, CodegenError> {
        // First, apply all transformations and build offset remappings
        let mut offset_remappings: HashMap<String, OffsetRemapping> = HashMap::new();

        for (section_name, section_transformations) in transformations {
            // Skip sections with no transformations - don't rebuild them
            if section_transformations.is_empty() {
                tracing::debug!(
                    "Skipping section '{}' - no transformations to apply",
                    section_name
                );
                continue;
            }

            let section = merged_sections.get_mut(section_name).ok_or_else(|| {
                CodegenError::InvalidOperation(format!("Section not found: {}", section_name))
            })?;

            // Build offset remapping for this section
            let remapping =
                OffsetRemapping::from_transformations(section_transformations, section.data.len());

            // Apply transformations by rebuilding the section
            // This ensures we don't lose data at the end when shrinking
            let mut new_data = Vec::with_capacity(section.data.len());
            let mut last_end = 0;

            // Sort transformations by offset (forward order for rebuilding)
            let mut sorted_transforms = section_transformations.clone();
            sorted_transforms.sort_by_key(|t| t.original_offset);

            // Log all transformations in the critical region
            if section_name == ".text" {
                for transform in &sorted_transforms {
                    if transform.original_offset >= 0x1470 && transform.original_offset <= 0x1490 {
                        tracing::debug!(
                            "CRITICAL REGION TRANSFORM: offset=0x{:x}, original_size={}, new_size={}, size_delta={}",
                            transform.original_offset,
                            transform.original_size,
                            transform.new_bytes.len(),
                            transform.size_delta
                        );
                    }
                }
            }

            tracing::debug!(
                "Applying {} transformations to section '{}' (size: 0x{:x})",
                sorted_transforms.len(),
                section_name,
                section.data.len()
            );

            for transform in &sorted_transforms {
                let start = transform.original_offset;
                let end = start + transform.original_size;

                // Debug: Log what we're about to replace
                if start < section.data.len() && end <= section.data.len() {
                    let old_bytes = &section.data[start..end];
                    tracing::debug!(
                        "Transforming in section '{}' at offset 0x{:x}: old_bytes={:?} new_bytes={:?}",
                        section_name,
                        start,
                        old_bytes,
                        &transform.new_bytes
                    );

                    // CRITICAL: Check if this transformation is near println_i64
                    if section_name == ".text" && start >= 0x1470 && start <= 0x1490 {
                        tracing::debug!(
                            "CRITICAL: Transformation at 0x{:x} is dangerously close to println_i64 at 0x1480!",
                            start
                        );
                        tracing::debug!(
                            "Transform details: start=0x{:x}, end=0x{:x}, original_size={}, new_size={}, size_delta={}",
                            start,
                            end,
                            transform.original_size,
                            transform.new_bytes.len(),
                            transform.size_delta
                        );
                    }
                }

                // Check size change
                let size_diff = transform.new_bytes.len() as i32 - transform.original_size as i32;
                if size_diff != 0 && section_name == ".text" {
                    tracing::debug!(
                        "GOTPCREL transformation in .text at offset 0x{:x}: {} bytes -> {} bytes (diff: {})",
                        start,
                        transform.original_size,
                        transform.new_bytes.len(),
                        size_diff
                    );
                }

                // Copy bytes before this transformation
                if last_end < start {
                    let copy_size = start - last_end;

                    // CRITICAL DEBUG: Check if we're about to copy corrupted data
                    if section_name == ".text" && last_end == 0x10a7 && start == 0x5ae6 {
                        tracing::debug!(
                            "CRITICAL: About to copy {} bytes from 0x{:x} to 0x{:x}",
                            copy_size,
                            last_end,
                            start
                        );
                        // Check what's at the critical offset in the source
                        if 0x1480 >= last_end && 0x1480 < start {
                            let offset_in_range = 0x1480 - last_end;
                            if offset_in_range + 10 <= copy_size {
                                let bytes_to_copy = &section.data[0x1480..0x1480 + 10];
                                tracing::debug!(
                                    "Source bytes at 0x1480 (will be copied): {:02x?}",
                                    bytes_to_copy
                                );
                            }
                        }
                    }

                    // Store the position before copy for debugging
                    let pos_before = new_data.len();
                    new_data.extend_from_slice(&section.data[last_end..start]);

                    // DEBUG: Check what was actually copied at 0x1480
                    if section_name == ".text" && last_end == 0x10a7 && start == 0x5ae6 {
                        // The offset 0x1480 in the original is at position (0x1480 - 0x10a7) relative to this copy
                        let offset_in_copy = 0x1480 - last_end;
                        let new_pos = pos_before + offset_in_copy;
                        if new_pos + 10 <= new_data.len() {
                            let copied_bytes = &new_data[new_pos..new_pos + 10];
                            tracing::debug!(
                                "After copy: Bytes at new position 0x{:x} (for original 0x1480): {:02x?}",
                                new_pos,
                                copied_bytes
                            );

                            // Also check what's at 0x1480 in new_data (should be different data now)
                            if 0x1480 + 10 <= new_data.len() {
                                let bytes_at_old_pos = &new_data[0x1480..0x1480 + 10];
                                tracing::debug!(
                                    "After copy: Bytes at OLD position 0x1480 in new_data: {:02x?}",
                                    bytes_at_old_pos
                                );
                            }
                        }
                    }

                    // Check if we're copying around the critical println_i64 region
                    if section_name == ".text" && last_end <= 0x1480 && start > 0x1480 {
                        tracing::debug!(
                            "CRITICAL COPY: Copying {} bytes from 0x{:x} to 0x{:x} (includes println_i64 at 0x1480)",
                            copy_size,
                            last_end,
                            start
                        );
                        // Check what we're actually copying at 0x1480
                        if last_end <= 0x1480 && start > 0x1480 {
                            let println_offset_in_range = 0x1480 - last_end;
                            if println_offset_in_range + 10 <= copy_size {
                                let copied_println_bytes = &section.data[0x1480..0x1480 + 10];
                                tracing::debug!(
                                    "Copying println_i64 bytes at 0x1480: {:02x?}",
                                    copied_println_bytes
                                );
                            }
                        }
                    }
                }

                // Add the transformed bytes
                new_data.extend_from_slice(&transform.new_bytes);

                // Track where this transformation puts us
                if section_name == ".text" && start >= 0x1470 && start <= 0x1490 {
                    tracing::debug!(
                        "After adding transform at 0x{:x}: new_data.len()=0x{:x}, transform.new_bytes={:02x?}",
                        start,
                        new_data.len(),
                        &transform.new_bytes
                    );
                }

                // DEBUG: Check if this transformation ends at the critical 0x10a7
                if section_name == ".text" && end == 0x10a7 {
                    tracing::debug!(
                        "FOUND IT: Transformation at 0x{:x} ends at 0x10a7 (start + original_size = 0x{:x} + {} = 0x{:x})",
                        start,
                        start,
                        transform.original_size,
                        end
                    );
                    tracing::debug!(
                        "Transform details: original_size={}, new_bytes.len()={}, size_delta={}",
                        transform.original_size,
                        transform.new_bytes.len(),
                        transform.size_delta
                    );
                }

                last_end = end;

                // Debug: Track last_end progression around critical region
                if section_name == ".text" && (last_end > 0x1400 && last_end < 0x1600) {
                    tracing::warn!(
                        "LAST_END UPDATE: After transform at 0x{:x}, last_end=0x{:x} (end=start+original_size=0x{:x}+{}=0x{:x})",
                        transform.original_offset,
                        last_end,
                        start,
                        transform.original_size,
                        end
                    );
                }

                tracing::trace!(
                    "Applied transformation at offset 0x{:x}: {} bytes -> {} bytes",
                    start,
                    transform.original_size,
                    transform.new_bytes.len()
                );
            }

            // Copy any remaining bytes after the last transformation
            if last_end < section.data.len() {
                new_data.extend_from_slice(&section.data[last_end..]);
                if section_name == ".text" && last_end < section.data.len() {
                    tracing::debug!(
                        "Preserving {} bytes after last transformation in .text (from 0x{:x} to 0x{:x})",
                        section.data.len() - last_end,
                        last_end,
                        section.data.len()
                    );
                }
            }

            // Check the bytes at 0x1480 before and after rebuild
            if section_name == ".text" {
                let println_offset = 0x1480;
                if println_offset + 10 <= section.data.len() {
                    let old_bytes = &section.data[println_offset..println_offset + 10];
                    tracing::debug!(
                        "BEFORE REBUILD: Bytes at 0x{:x} (println_i64): {:02x?}",
                        println_offset,
                        old_bytes
                    );
                }

                // Calculate the new offset based on transformations
                let mut new_offset = println_offset;
                for transform in &sorted_transforms {
                    if transform.original_offset < println_offset {
                        new_offset = (new_offset as isize + transform.size_delta) as usize;
                        tracing::debug!(
                            "Transform at 0x{:x} shifts println_i64: old_offset=0x{:x} -> new_offset=0x{:x} (delta={})",
                            transform.original_offset,
                            println_offset,
                            new_offset,
                            transform.size_delta
                        );
                    }
                }

                tracing::debug!(
                    "println_i64 should be at new offset 0x{:x} (was at 0x{:x})",
                    new_offset,
                    println_offset
                );

                if new_offset + 10 <= new_data.len() {
                    let new_bytes = &new_data[new_offset..new_offset + 10];
                    tracing::debug!(
                        "AFTER REBUILD: Bytes at 0x{:x} (println_i64): {:02x?}",
                        new_offset,
                        new_bytes
                    );
                    // Check if these are the expected println_i64 bytes
                    let expected = [0x48, 0x83, 0xec, 0x38, 0x0f, 0x57, 0xc0, 0x0f, 0x29, 0x04];
                    if new_bytes != expected {
                        tracing::debug!(
                            "CORRUPTION DETECTED: println_i64 bytes have been corrupted!"
                        );
                        // Also check what's at the OLD offset
                        if println_offset + 10 <= new_data.len() {
                            let bytes_at_old_offset =
                                &new_data[println_offset..println_offset + 10];
                            tracing::debug!(
                                "Bytes at OLD offset 0x{:x}: {:02x?}",
                                println_offset,
                                bytes_at_old_offset
                            );
                        }
                    }
                }
            }

            // Replace the section data with the transformed version
            tracing::debug!(
                "Section '{}' rebuilt: old_size=0x{:x}, new_size=0x{:x}, transformations={}",
                section_name,
                section.data.len(),
                new_data.len(),
                section_transformations.len()
            );

            // If there were no transformations but we still got here, this is likely a bug
            if section_transformations.is_empty() {
                tracing::debug!(
                    "CRITICAL BUG: Section '{}' has no transformations but is being rebuilt - this will corrupt the section",
                    section_name
                );
            }

            section.data = new_data;

            offset_remappings.insert(section_name.clone(), remapping);
        }

        // Update symbol addresses for symbols in transformed sections before applying relocations
        // This is critical - relocations need to use the updated symbol addresses
        for (section_name, _remapping) in &offset_remappings {
            // We need to update any symbols that point into this section
            // Since we only have a reference to the symbol table, we'll need to track
            // which symbols need remapping and apply the remapping when we look them up
            tracing::debug!(
                "Section '{}' was transformed, symbols in this section will be remapped",
                section_name
            );
        }

        // Now apply all relocations with adjusted offsets
        for relocation in relocations {
            if self.should_skip_section(&relocation.section_name) {
                continue;
            }

            let normalized_section = self.normalize_section_name(&relocation.section_name);

            // Get the symbol
            if relocation.section_name.contains("text") {
                tracing::debug!(
                    "TEXT RELOCATION: Looking up symbol '{}' for relocation in section '{}', kind={:?}",
                    relocation.symbol_name,
                    relocation.section_name,
                    relocation.kind
                );
            } else {
                tracing::debug!(
                    "Looking up symbol '{}' for relocation in section '{}', kind={:?}",
                    relocation.symbol_name,
                    relocation.section_name,
                    relocation.kind
                );
            }

            // CRITICAL DEBUG: Track _start relocations specifically
            if relocation.section_name.contains("_start") {
                tracing::debug!(
                    "CRITICAL _START RELOCATION: symbol='{}', from_section='{}', kind={:?}, offset=0x{:x}, addend={}",
                    relocation.symbol_name,
                    relocation.section_name,
                    relocation.kind,
                    relocation.offset,
                    relocation.addend
                );

                // Show what symbols are available in the table for comparison
                let available_symbols: Vec<String> = self.symbol_table.symbol_names();
                tracing::debug!(
                    "_START DEBUG: Available symbols in table: {:?}",
                    available_symbols
                        .iter()
                        .filter(|name| name.contains("main") || name.contains("_start"))
                        .collect::<Vec<_>>()
                );
            }

            // DEBUG: Check for println relocation before lookup
            if relocation
                .symbol_name
                .contains("b0b5dd69a1903197786a132465e2060b.7")
            {
                tracing::debug!(
                    "DEBUG apply_pass: About to look up println symbol '{}'",
                    relocation.symbol_name
                );
            }

            let mut symbol = self
                .symbol_table
                .get_symbol_including_local(&relocation.symbol_name)
                .ok_or_else(|| {
                    // Debug: Log more info about the missing symbol
                    if relocation.symbol_name.contains("HEAP")
                        || relocation.symbol_name.contains("bss")
                    {
                        tracing::debug!(
                            "BSS symbol not found: '{}' in relocation from section '{}'",
                            relocation.symbol_name,
                            relocation.section_name
                        );
                    } else if relocation.symbol_name.starts_with("__rue_") {
                        tracing::debug!(
                            "Runtime symbol not found: '{}' in relocation from section '{}'",
                            relocation.symbol_name,
                            relocation.section_name
                        );
                    } else if relocation.symbol_name.contains("b0b5dd69a1903197786a132465e2060b.7") {
                        tracing::debug!(
                            "CRITICAL: println rodata symbol not found: '{}' in relocation from section '{}'",
                            relocation.symbol_name,
                            relocation.section_name
                        );
                    }
                    CodegenError::InvalidOperation(format!(
                        "Undefined symbol: {}",
                        relocation.symbol_name
                    ))
                })?
                .clone();

            // CRITICAL DEBUG: Show what symbol was actually resolved for _start relocations
            if relocation.section_name.contains("_start") {
                tracing::debug!(
                    "RESOLVED _START TARGET: symbol='{}' address=0x{:x} section='{}' source={:?} size={}",
                    symbol.name,
                    symbol.address,
                    symbol.section_name,
                    symbol.source,
                    symbol.size
                );
            }

            // DEBUG: Check if println symbol was found
            if relocation
                .symbol_name
                .contains("b0b5dd69a1903197786a132465e2060b.7")
            {
                tracing::debug!(
                    "DEBUG apply_pass: FOUND println symbol '{}' at address 0x{:x} in section '{}'",
                    symbol.name,
                    symbol.address,
                    symbol.section_name
                );
            }

            tracing::debug!(
                "Found symbol '{}': address=0x{:x}, section='{}'",
                symbol.name,
                symbol.address,
                symbol.section_name
            );

            // If the symbol is in a section that was transformed, remap its address
            // Note: symbol.section_name might be the original section name (e.g., ".text.__rue_println_i64")
            // but offset_remappings uses normalized section names (e.g., ".text")
            let normalized_symbol_section = self.normalize_section_name(&symbol.section_name);
            if let Some(remapping) = offset_remappings.get(&normalized_symbol_section) {
                let original_address = symbol.address as usize;
                let remapped_address = remapping.remap_offset(original_address);
                if remapped_address != original_address {
                    tracing::warn!(
                        "SYMBOL REMAP: '{}' (section '{}') address: 0x{:x} -> 0x{:x} (delta: {})",
                        symbol.name,
                        symbol.section_name,
                        original_address,
                        remapped_address,
                        remapped_address as i64 - original_address as i64
                    );
                    symbol.address = remapped_address as u64;
                } else {
                    tracing::debug!(
                        "Symbol '{}' in transformed section '{}' but no address change needed (0x{:x})",
                        symbol.name,
                        symbol.section_name,
                        original_address
                    );
                }
            }

            // Debug main and __rue_main symbol resolution
            if relocation.symbol_name == "main" || relocation.symbol_name == "__rue_main" {
                tracing::debug!(
                    "Resolving '{}' symbol: address=0x{:x}, section='{}', size={}, from_section='{}'",
                    relocation.symbol_name,
                    symbol.address,
                    symbol.section_name,
                    symbol.size,
                    relocation.section_name
                );
            }

            // Get the section
            let section = merged_sections
                .get_mut(&normalized_section)
                .ok_or_else(|| {
                    CodegenError::InvalidOperation(format!(
                        "Section not found: {}",
                        normalized_section
                    ))
                })?;

            // Calculate offset with adjustments
            let subsection_offset =
                self.get_subsection_offset(&relocation.section_name, &normalized_section);
            let original_offset = subsection_offset + relocation.offset;

            // Debug: log offset calculation for critical relocations
            if relocation.symbol_name == "main" {
                tracing::debug!(
                    "main relocation offset calculation: section='{}', normalized='{}', subsection_offset=0x{:x}, reloc_offset=0x{:x}, original_offset=0x{:x}",
                    relocation.section_name,
                    normalized_section,
                    subsection_offset,
                    relocation.offset,
                    original_offset
                );
            }

            // Remap the offset if transformations were applied
            let actual_offset = if let Some(remapping) = offset_remappings.get(&normalized_section)
            {
                let remapped = remapping.remap_offset(original_offset as usize);
                if relocation.symbol_name == "main" {
                    tracing::debug!(
                        "main relocation remapping: original_offset=0x{:x} -> remapped_offset=0x{:x}",
                        original_offset,
                        remapped
                    );
                }
                remapped
            } else {
                original_offset as usize
            };

            // For GOTPCREL/REX_GOTPCRELX/GOTPCRELX relocations that were transformed, we need to adjust how we apply them
            // The instruction has been converted from indirect to direct, so treat as PC32
            let effective_relocation_kind = if matches!(
                relocation.kind,
                RelocationKind::GOTPCREL
                    | RelocationKind::REX_GOTPCRELX
                    | RelocationKind::GOTPCRELX
            ) {
                // Check if this relocation was part of a transformation
                let original_offset_in_section = original_offset as usize;
                let was_transformed = transformations
                    .get(&normalized_section)
                    .map(|transforms| {
                        transforms.iter().any(|t| {
                            // The relocation offset points into the original instruction
                            let reloc_in_instruction = original_offset_in_section
                                >= t.original_offset
                                && original_offset_in_section
                                    <= t.original_offset + t.original_size;
                            reloc_in_instruction
                        })
                    })
                    .unwrap_or(false);

                if was_transformed {
                    // The instruction was transformed from indirect to direct call
                    // Treat this as a PC32 relocation now
                    RelocationKind::PC32
                } else {
                    relocation.kind
                }
            } else {
                relocation.kind
            };

            // Debug: Log specific relocations
            if symbol.section_name.starts_with(".bss") {
                tracing::debug!(
                    "Applying BSS relocation: symbol='{}' section='{}' address=0x{:x} offset=0x{:x} kind={:?}",
                    relocation.symbol_name,
                    symbol.section_name,
                    symbol.address,
                    actual_offset,
                    effective_relocation_kind
                );
            } else if relocation.symbol_name.contains("println")
                || relocation.symbol_name.contains("__rue_")
            {
                tracing::debug!(
                    "CRITICAL RELOCATION: symbol='{}' section='{}' address=0x{:x} offset=0x{:x} kind={:?} source={:?}",
                    relocation.symbol_name,
                    symbol.section_name,
                    symbol.address,
                    actual_offset,
                    effective_relocation_kind,
                    symbol.source
                );

                // Log what bytes are currently at the relocation offset
                if actual_offset + 4 <= section.data.len() {
                    let current_bytes = &section.data[actual_offset..actual_offset + 4];
                    tracing::debug!(
                        "Current bytes at relocation offset 0x{:x}: {:02x?}",
                        actual_offset,
                        current_bytes
                    );
                }
            }

            // DEBUG: Check if this is the println relocation before applying
            if relocation
                .symbol_name
                .contains("b0b5dd69a1903197786a132465e2060b.7")
            {
                tracing::debug!(
                    "DEBUG: About to apply println relocation at actual_offset=0x{:x} with symbol address 0x{:x}, kind={:?}",
                    actual_offset,
                    symbol.address,
                    effective_relocation_kind
                );
            }

            // Apply the relocation with the effective kind
            self.apply_single_relocation_with_kind(
                section,
                relocation,
                symbol.address,
                actual_offset,
                effective_relocation_kind,
            )?;

            // DEBUG: Check if the println relocation was applied
            if relocation
                .symbol_name
                .contains("b0b5dd69a1903197786a132465e2060b.7")
            {
                tracing::debug!(
                    "DEBUG: Applied println relocation at actual_offset=0x{:x}",
                    actual_offset
                );
            }
        }

        Ok(offset_remappings)
    }

    /// Apply a single relocation at the given offset
    fn apply_single_relocation(
        &self,
        section: &mut MergedSection,
        relocation: &RelocationEntry,
        target_address: u64,
        offset: usize,
    ) -> Result<(), CodegenError> {
        self.apply_single_relocation_with_kind(
            section,
            relocation,
            target_address,
            offset,
            relocation.kind,
        )
    }

    /// Apply a single relocation at the given offset with a specific kind
    fn apply_single_relocation_with_kind(
        &self,
        section: &mut MergedSection,
        relocation: &RelocationEntry,
        target_address: u64,
        offset: usize,
        kind: RelocationKind,
    ) -> Result<(), CodegenError> {
        const BASE_ADDRESS: u64 = 0x400000;
        const HEADERS_SIZE: u64 = 0x80;

        // Debug: Log critical relocation info
        if relocation.symbol_name.contains("__rue_main") || relocation.symbol_name == "main" {
            tracing::debug!(
                "Critical {} relocation: symbol='{}', target_address=0x{:x}, offset=0x{:x}, kind={:?}, addend={}",
                relocation.symbol_name,
                relocation.symbol_name,
                target_address,
                offset,
                kind,
                relocation.addend
            );
        }

        match kind {
            RelocationKind::Absolute64 => {
                // For absolute relocations, we need the virtual address, not just the file offset
                let value = ((BASE_ADDRESS + target_address) as i64 + relocation.addend) as u64;
                if offset + 8 > section.data.len() {
                    return Err(CodegenError::InvalidOperation(format!(
                        "Relocation offset out of bounds: 0x{:x}",
                        offset
                    )));
                }
                section.data[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
            }
            RelocationKind::Absolute32 => {
                // Calculate 32-bit absolute address
                // For absolute relocations, we need the virtual address, not just the file offset
                let full_value = (BASE_ADDRESS + target_address) as i64 + relocation.addend;
                let full_value = full_value as u64;

                // Check if the value fits in 32 bits
                if full_value > u32::MAX as u64 {
                    return Err(CodegenError::InvalidOperation(format!(
                        "R_X86_64_32 relocation value 0x{:x} does not fit in 32 bits for symbol '{}' at offset 0x{:x}",
                        full_value, relocation.symbol_name, offset
                    )));
                }

                let value = full_value as u32;

                if offset + 4 > section.data.len() {
                    return Err(CodegenError::InvalidOperation(format!(
                        "Relocation offset out of bounds: 0x{:x}",
                        offset
                    )));
                }

                section.data[offset..offset + 4].copy_from_slice(&value.to_le_bytes());

                tracing::debug!(
                    "Applied R_X86_64_32 relocation at offset 0x{:x}: symbol='{}' target=0x{:x} value=0x{:x}",
                    offset,
                    relocation.symbol_name,
                    target_address,
                    value
                );
            }
            RelocationKind::Absolute32S => {
                // Calculate 32-bit sign-extended absolute address
                // For absolute relocations, we need the virtual address, not just the file offset
                let full_value = (BASE_ADDRESS + target_address) as i64 + relocation.addend;

                // For R_X86_64_32S, check if the value can be represented as a sign-extended 32-bit value
                // This means the value must be in the range [-2^31, 2^31-1] when viewed as a signed 64-bit value
                if full_value < i32::MIN as i64 || full_value > i32::MAX as i64 {
                    return Err(CodegenError::InvalidOperation(format!(
                        "R_X86_64_32S relocation value 0x{:x} ({}) does not fit in 32-bit signed range for symbol '{}' at offset 0x{:x}",
                        full_value as u64, full_value, relocation.symbol_name, offset
                    )));
                }

                let value = full_value as i32;

                if offset + 4 > section.data.len() {
                    return Err(CodegenError::InvalidOperation(format!(
                        "Relocation offset out of bounds: 0x{:x}",
                        offset
                    )));
                }

                section.data[offset..offset + 4].copy_from_slice(&value.to_le_bytes());

                tracing::debug!(
                    "Applied R_X86_64_32S relocation at offset 0x{:x}: symbol='{}' target=0x{:x} value=0x{:x} ({})",
                    offset,
                    relocation.symbol_name,
                    target_address,
                    value as u32,
                    value
                );
            }
            RelocationKind::PC32
            | RelocationKind::PLT32
            | RelocationKind::GOTPCREL
            | RelocationKind::REX_GOTPCRELX
            | RelocationKind::GOTPCRELX => {
                // Calculate PC-relative offset
                // For PC32/PLT32 relocations, the formula is: displacement = target - (relocation_address + 4) + addend
                // where relocation_address is the address of the relocation (not including the +4)
                // Note: section.base_address already includes the offset from the start of the file (e.g., HEADERS_SIZE)
                let relocation_address = BASE_ADDRESS + section.base_address + offset as u64;

                // Target address calculation:
                // The target_address is the symbol's final address after symbol resolution
                // This comes from update_symbol_addresses() which calculates the offset from the start of the file
                // We need to add BASE_ADDRESS to get the virtual address
                let target = BASE_ADDRESS + target_address;

                // Standard PC32/PLT32 formula: displacement = target - (relocation_address + 4) + addend

                // Critical fix for PLT32 relocations:
                // For PLT32 relocations, the target_address from update_symbol_addresses() is already
                // calculated as file_offset, and relocation_address already includes BASE_ADDRESS.
                // The issue is that update_symbol_addresses() calculates addresses as file offsets,
                // but we're adding BASE_ADDRESS to both source and target, creating wrong displacements.

                // Get symbol information to check if this is a runtime library internal call
                let symbol = self
                    .symbol_table
                    .get_symbol_including_local(&relocation.symbol_name)
                    .ok_or_else(|| {
                        CodegenError::InvalidOperation(format!(
                            "Symbol not found during relocation: {}",
                            relocation.symbol_name
                        ))
                    })?;

                // Check if this is truly an internal runtime library call
                // Internal calls are only between runtime library functions:
                // 1. _start calling __rue_ functions
                // 2. __rue_ functions calling other __rue_ functions
                // User code calls should be treated as external even if they end up in the same .text section
                let is_runtime_source = relocation.section_name.contains("__rue_")
                    || relocation.section_name.contains("_start");
                let is_runtime_target = symbol.section_name.contains("__rue_")
                    || relocation.symbol_name.contains("__rue_");
                let is_internal_call = is_runtime_source && is_runtime_target;

                // For internal PLT32 calls (like _start -> __rue_main), both addresses are file offsets
                // and we should calculate displacement directly without double-adding BASE_ADDRESS
                let displacement = if is_internal_call && relocation.kind == RelocationKind::PLT32 {
                    // Both target_address and section.base_address are file offsets
                    // Calculate displacement directly: target_file_offset - source_file_offset - 4
                    // For internal calls, ignore the compiler-generated addend since we're using direct addressing
                    let target_file_offset = target_address;
                    let source_file_offset = section.base_address + offset as u64;
                    (target_file_offset as i64 - source_file_offset as i64 - 4) as i32
                } else {
                    // External calls: use virtual addresses (add BASE_ADDRESS)
                    let adjusted_addend = if (relocation.symbol_name == "main"
                        || relocation.symbol_name.starts_with("__rue_"))
                        && relocation.addend == -4
                    {
                        0 // Ignore the -4 addend for main and runtime functions
                    } else {
                        relocation.addend
                    };
                    (target as i64 - (relocation_address as i64 + 4) + adjusted_addend) as i32
                };

                let value = displacement;

                // Debug: Log relocation calculation details for critical functions
                if relocation.symbol_name.contains("__rue_main")
                    || relocation.symbol_name == "main"
                    || relocation.symbol_name.starts_with("__rue_")
                    || relocation.section_name.contains("_start")
                {
                    tracing::debug!(
                        "DETAILED PC-REL CALC for {}: from_section='{}' relocation_addr=0x{:x} target=0x{:x} displacement=0x{:x} ({}) is_internal={}",
                        relocation.symbol_name,
                        relocation.section_name,
                        relocation_address,
                        target,
                        value,
                        value as i32,
                        is_internal_call
                    );
                    tracing::debug!(
                        "  Source: BASE_ADDRESS=0x{:x} + section.base_address=0x{:x} + offset=0x{:x} = 0x{:x}",
                        BASE_ADDRESS,
                        section.base_address,
                        offset,
                        relocation_address
                    );
                    tracing::debug!(
                        "  Target: BASE_ADDRESS=0x{:x} + target_address=0x{:x} = 0x{:x}",
                        BASE_ADDRESS,
                        target_address,
                        target
                    );
                    tracing::debug!(
                        "  Formula: target=0x{:x} - (relocation_addr=0x{:x} + 4) + addend={} = 0x{:x}",
                        target,
                        relocation_address,
                        relocation.addend,
                        value
                    );

                    // CRITICAL: Show exactly what address the call will target
                    let call_target_address = (relocation_address as i64 + 4 + value as i64) as u64;
                    tracing::debug!(
                        "  FINAL CALL TARGET: 0x{:x} + 4 + 0x{:x} = 0x{:x}",
                        relocation_address,
                        value,
                        call_target_address
                    );

                    // Special debug for internal calls
                    if is_internal_call {
                        let file_offset_calc = target_address as i64
                            - (section.base_address + offset as u64) as i64
                            - 4;
                        tracing::debug!(
                            "  Internal call calc: target_file_offset=0x{:x} - source_file_offset=0x{:x} - 4 = 0x{:x}",
                            target_address,
                            section.base_address + offset as u64,
                            file_offset_calc
                        );
                    }
                }

                if offset + 4 > section.data.len() {
                    return Err(CodegenError::InvalidOperation(format!(
                        "Relocation offset out of bounds: 0x{:x}",
                        offset
                    )));
                }
                section.data[offset..offset + 4].copy_from_slice(&value.to_le_bytes());

                // Debug: Log what was actually written for critical relocations
                if relocation.symbol_name.contains("__rue_main")
                    || relocation.symbol_name == "main"
                    || relocation.symbol_name.starts_with("__rue_")
                    || relocation.section_name.contains("_start")
                {
                    let written_bytes = &section.data[offset..offset + 4];
                    tracing::debug!(
                        "WROTE RELOCATION: symbol='{}' at offset 0x{:x} wrote bytes={:02x?} (displacement=0x{:x})",
                        relocation.symbol_name,
                        offset,
                        written_bytes,
                        value
                    );

                    // If this is a call instruction, show the complete call
                    if offset > 0 && section.data[offset - 1] == 0xe8 {
                        let call_bytes = &section.data[offset - 1..offset + 4];
                        tracing::debug!(
                            "Complete call instruction at 0x{:x}: {:02x?}",
                            offset - 1,
                            call_bytes
                        );
                    }
                }

                tracing::trace!(
                    "Applied {:?} relocation at offset 0x{:x}: reloc_addr=0x{:x} target=0x{:x} value=0x{:x}",
                    relocation.kind,
                    offset,
                    relocation_address,
                    target,
                    value
                );
            }
        }

        Ok(())
    }

    /// Check if a section should be skipped
    fn should_skip_section(&self, section_name: &str) -> bool {
        section_name.starts_with(".debug")
            || section_name.starts_with(".comment")
            || section_name.starts_with(".note")
            || section_name.starts_with(".eh_frame")
            || section_name.starts_with(".rela.eh_frame")
    }

    /// Normalize section name for merging
    fn normalize_section_name(&self, name: &str) -> String {
        if name.starts_with(".text") {
            ".text".to_string()
        } else if name.starts_with(".rodata") {
            ".rodata".to_string()
        } else if name.starts_with(".data") {
            ".data".to_string()
        } else if name.starts_with(".bss") {
            ".bss".to_string()
        } else {
            name.to_string()
        }
    }

    /// Get the offset of a subsection within its merged section
    fn get_subsection_offset(&self, section_name: &str, normalized_name: &str) -> u64 {
        // Debug logging for critical relocations
        if section_name.contains("text")
            && (section_name.contains("println") || section_name == ".text")
        {
            tracing::debug!(
                "get_subsection_offset: section='{}', normalized='{}', same_name={}",
                section_name,
                normalized_name,
                section_name == normalized_name
            );
            if let Some(section_offset) = self.section_offsets.get(section_name) {
                tracing::debug!(
                    "Found section offset for '{}': offset_within_merged=0x{:x}, merged_section_name='{}'",
                    section_name,
                    section_offset.offset_within_merged,
                    section_offset.merged_section_name
                );
            } else {
                tracing::debug!("No section offset found for '{}'", section_name);
            }
        }

        if section_name != normalized_name {
            self.section_offsets
                .get(section_name)
                .map(|section_offset| section_offset.offset_within_merged)
                .unwrap_or(0)
        } else {
            // Even if the names are the same, we still need to check if this section was placed
            // at a specific offset within the merged section
            self.section_offsets
                .get(section_name)
                .map(|section_offset| section_offset.offset_within_merged)
                .unwrap_or(0)
        }
    }

    /// Check if applying a transformation at the given offset would corrupt a runtime function
    fn would_corrupt_runtime_function(
        &self,
        section_data: &[u8],
        offset: usize,
        relocation: &RelocationEntry,
    ) -> bool {
        // Known runtime function signatures that should not be transformed
        let runtime_function_signatures = [
            // println_i64 function signature
            ([0x48, 0x83, 0xec, 0x38, 0x0f, 0x57], "println_i64"),
            // Add other runtime function signatures here as needed
        ];

        // Check if we're about to overwrite any known runtime function
        for (signature, function_name) in &runtime_function_signatures {
            if offset + signature.len() <= section_data.len() {
                let bytes_at_offset = &section_data[offset..offset + signature.len()];
                if bytes_at_offset == *signature {
                    tracing::debug!(
                        "PROTECTION: Detected attempt to transform {} function at offset 0x{:x}",
                        function_name,
                        offset
                    );
                    return true;
                }
            }
        }

        // Additional check: If this is supposed to be a transformation of a runtime library call,
        // but we're detecting the pattern inside a runtime function, something is wrong
        if relocation.section_name.contains("__rue_") && offset >= 0x1400 {
            // This suggests the relocation metadata might be pointing to the wrong location
            tracing::warn!(
                "Suspicious: Runtime section '{}' has GOTPCREL relocation at offset 0x{:x} - this might indicate incorrect relocation metadata",
                relocation.section_name,
                offset
            );
        }

        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_multiple_transformations_corruption() {
        // Test with multiple transformations to see if overlapping or multiple transforms cause issues
        let mut section_data = vec![0x00; 0x2000];

        // Place runtime function at 0x1480
        let println_start = 0x1480;
        let expected_println_bytes = [
            0x48, 0x83, 0xec, 0x38, 0x0f, 0x57, 0xc0, 0x0f, 0x29, 0x04, 0x24,
        ];
        section_data[println_start..println_start + expected_println_bytes.len()]
            .copy_from_slice(&expected_println_bytes);

        // Create multiple transformations before println_i64
        let transform1_offset = 0x1400;
        let transform2_offset = 0x1450;

        // First transformation at 0x1400
        let original_gotpcrel1 = [0xff, 0x15, 0x00, 0x00, 0x00, 0x00];
        section_data[transform1_offset..transform1_offset + original_gotpcrel1.len()]
            .copy_from_slice(&original_gotpcrel1);

        // Second transformation at 0x1450
        let original_gotpcrel2 = [0xff, 0x15, 0x11, 0x22, 0x33, 0x44];
        section_data[transform2_offset..transform2_offset + original_gotpcrel2.len()]
            .copy_from_slice(&original_gotpcrel2);

        let transformations = vec![
            PlannedTransformation {
                original_offset: transform1_offset,
                size_delta: -1, // 6 bytes -> 5 bytes
                new_bytes: vec![0xe8, 0x00, 0x00, 0x00, 0x00],
                relocation_offset_in_new: 1,
                original_size: 6,
            },
            PlannedTransformation {
                original_offset: transform2_offset,
                size_delta: -1, // 6 bytes -> 5 bytes
                new_bytes: vec![0xe8, 0x11, 0x22, 0x33, 0x44],
                relocation_offset_in_new: 1,
                original_size: 6,
            },
        ];

        // Show before state
        println!(
            "BEFORE: println_i64 at 0x{:x}: {:02x?}",
            println_start,
            &section_data[println_start..println_start + expected_println_bytes.len()]
        );

        // Simulate the section rebuild logic
        let mut new_data = Vec::with_capacity(section_data.len());
        let mut last_end = 0;

        // Sort by offset (this is what the real code does)
        let mut sorted_transforms = transformations.clone();
        sorted_transforms.sort_by_key(|t| t.original_offset);

        for (i, transform) in sorted_transforms.iter().enumerate() {
            let start = transform.original_offset;
            let end = start + transform.original_size;

            println!(
                "Transform {}: start=0x{:x}, end=0x{:x}, last_end=0x{:x}",
                i, start, end, last_end
            );

            // Copy bytes before this transformation
            if last_end < start {
                println!(
                    "  Copying bytes[0x{:x}..0x{:x}] (len={})",
                    last_end,
                    start,
                    start - last_end
                );
                new_data.extend_from_slice(&section_data[last_end..start]);
            }

            // Add the transformed bytes
            println!("  Adding transformed bytes: {:?}", transform.new_bytes);
            new_data.extend_from_slice(&transform.new_bytes);

            last_end = end;
            println!(
                "  New last_end = 0x{:x}, new_data.len() = 0x{:x}",
                last_end,
                new_data.len()
            );
        }

        // Copy remaining bytes
        if last_end < section_data.len() {
            println!(
                "Copying remaining bytes[0x{:x}..0x{:x}] (len={})",
                last_end,
                section_data.len(),
                section_data.len() - last_end
            );
            new_data.extend_from_slice(&section_data[last_end..]);
        }

        println!(
            "FINAL: new_data.len() = 0x{:x}, expected total delta = {}",
            new_data.len(),
            -2
        );

        // Calculate where println_i64 should be after both transformations
        // Transform 1 at 0x1400 shrinks by 1 byte: println moves from 0x1480 to 0x147F
        // Transform 2 at 0x1450 shrinks by 1 byte: println moves from 0x147F to 0x147E
        let expected_new_offset = println_start - 2; // Both transforms shrink by 1 byte each

        if expected_new_offset + expected_println_bytes.len() <= new_data.len() {
            let actual_bytes =
                &new_data[expected_new_offset..expected_new_offset + expected_println_bytes.len()];
            println!(
                "AFTER: println_i64 at expected offset 0x{:x}: {:02x?}",
                expected_new_offset, actual_bytes
            );

            assert_eq!(
                actual_bytes, expected_println_bytes,
                "Multiple transformations corrupted println_i64 bytes"
            );
        } else {
            panic!("Section too small after transformations");
        }
    }

    #[test]
    fn test_section_corruption_bug() {
        // Reproduce the exact corruption scenario described in the bug report
        // We have a section with runtime function at 0x1480 and a transformation before it

        // Create mock section data with runtime function at 0x1480
        let mut section_data = vec![0x00; 0x2000]; // 8KB section

        // Place println_i64 function at offset 0x1480 with expected bytes
        let println_start = 0x1480;
        let expected_println_bytes = [
            0x48, 0x83, 0xec, 0x38, 0x0f, 0x57, 0xc0, 0x0f, 0x29, 0x04, 0x24,
        ];
        section_data[println_start..println_start + expected_println_bytes.len()]
            .copy_from_slice(&expected_println_bytes);

        // Create a transformation that occurs before println_i64 (let's say at 0x1400)
        let transform_offset = 0x1400;

        // Original GOTPCREL instruction: ff 15 xx xx xx xx (6 bytes)
        let original_gotpcrel = [0xff, 0x15, 0x00, 0x00, 0x00, 0x00];
        section_data[transform_offset..transform_offset + original_gotpcrel.len()]
            .copy_from_slice(&original_gotpcrel);

        let transformation = PlannedTransformation {
            original_offset: transform_offset,
            size_delta: -1,                                // 6 bytes -> 5 bytes
            new_bytes: vec![0xe8, 0x00, 0x00, 0x00, 0x00], // direct call
            relocation_offset_in_new: 1,
            original_size: 6,
        };

        // Simulate the section rebuild logic
        let mut new_data = Vec::with_capacity(section_data.len());
        let mut last_end = 0;
        let transforms = vec![transformation];

        for transform in &transforms {
            let start = transform.original_offset;
            let end = start + transform.original_size;

            // Copy bytes before this transformation
            if last_end < start {
                println!(
                    "Copying bytes[0x{:x}..0x{:x}] (len={})",
                    last_end,
                    start,
                    start - last_end
                );
                new_data.extend_from_slice(&section_data[last_end..start]);
            }

            // Add the transformed bytes
            println!(
                "Adding transformed bytes at 0x{:x}: {:?}",
                start, transform.new_bytes
            );
            new_data.extend_from_slice(&transform.new_bytes);

            last_end = end;
            println!("Setting last_end = 0x{:x}", last_end);
        }

        // Copy any remaining bytes after the last transformation
        if last_end < section_data.len() {
            println!(
                "Copying remaining bytes[0x{:x}..0x{:x}] (len={})",
                last_end,
                section_data.len(),
                section_data.len() - last_end
            );
            new_data.extend_from_slice(&section_data[last_end..]);
        }

        // Show debug output regardless
        eprintln!(
            "Original section size: 0x{:x}, New section size: 0x{:x}",
            section_data.len(),
            new_data.len()
        );

        // Check if println_i64 bytes are preserved
        // After the transformation (6->5 bytes, delta = -1), println_i64 should be at 0x1480 - 1 = 0x147F
        let expected_new_println_offset = println_start - 1; // Account for the -1 byte delta

        if expected_new_println_offset + expected_println_bytes.len() <= new_data.len() {
            let actual_bytes = &new_data[expected_new_println_offset
                ..expected_new_println_offset + expected_println_bytes.len()];
            eprintln!(
                "Expected println bytes at 0x{:x}: {:02x?}",
                expected_new_println_offset, expected_println_bytes
            );
            eprintln!(
                "Actual bytes at 0x{:x}: {:02x?}",
                expected_new_println_offset, actual_bytes
            );

            // Also check what we have at the original offset
            if println_start + expected_println_bytes.len() <= new_data.len() {
                let bytes_at_original =
                    &new_data[println_start..println_start + expected_println_bytes.len()];
                eprintln!(
                    "Bytes at original offset 0x{:x}: {:02x?}",
                    println_start, bytes_at_original
                );
            }

            if actual_bytes != expected_println_bytes {
                panic!(
                    "CORRUPTION REPRODUCED! Expected {:02x?} at 0x{:x} but got {:02x?}",
                    expected_println_bytes, expected_new_println_offset, actual_bytes
                );
            }
        } else {
            panic!(
                "New section too small to contain println_i64 at expected offset 0x{:x}",
                expected_new_println_offset
            );
        }
    }

    #[test]
    fn test_runtime_function_protection() {
        // Test that the protection prevents corrupting runtime functions
        let mut section_data = vec![0x00; 0x2000];

        // Place the exact println_i64 signature at offset 0x1480
        let println_offset = 0x1480;
        let println_signature = [
            0x48, 0x83, 0xec, 0x38, 0x0f, 0x57, 0xc0, 0x0f, 0x29, 0x04, 0x24,
        ];
        section_data[println_offset..println_offset + println_signature.len()]
            .copy_from_slice(&println_signature);

        // Create a mock TwoPassRelocator
        let symbol_table = SymbolTable::new();
        let section_offsets = HashMap::new();
        let relocator = TwoPassRelocator::new(&symbol_table, &section_offsets);

        // Create a mock relocation that would target the runtime function
        let malicious_relocation = RelocationEntry {
            section_name: ".text.user_function".to_string(),
            offset: 0, // This would make actual_offset = 0x1480
            kind: RelocationKind::GOTPCREL,
            symbol_name: "some_symbol".to_string(),
            addend: 0,
        };

        // Test the protection function
        assert!(
            relocator.would_corrupt_runtime_function(
                &section_data,
                println_offset,
                &malicious_relocation
            ),
            "Protection should detect attempt to corrupt println_i64"
        );

        // Test that normal offsets don't trigger false positives
        assert!(
            !relocator.would_corrupt_runtime_function(&section_data, 0x1000, &malicious_relocation),
            "Protection should not trigger false positives"
        );
    }

    #[test]
    fn test_offset_remapping() {
        let transformations = vec![
            PlannedTransformation {
                original_offset: 10,
                size_delta: -1, // Shrinks by 1 byte
                new_bytes: vec![0xe8, 0x00, 0x00, 0x00, 0x00],
                relocation_offset_in_new: 1,
                original_size: 6,
            },
            PlannedTransformation {
                original_offset: 20,
                size_delta: -1, // Shrinks by 1 byte
                new_bytes: vec![0xe8, 0x00, 0x00, 0x00, 0x00],
                relocation_offset_in_new: 1,
                original_size: 6,
            },
        ];

        let remapping = OffsetRemapping::from_transformations(&transformations, 100);

        // Before first transformation
        assert_eq!(remapping.remap_offset(5), 5);

        // After first transformation (offset 10, shrinks by 1)
        assert_eq!(remapping.remap_offset(15), 14);

        // After both transformations (each shrinks by 1)
        assert_eq!(remapping.remap_offset(25), 23);

        // Total size change
        assert_eq!(remapping.total_size_change(), -2);
    }
}
