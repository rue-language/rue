// Relocation handling for object file linking
//
// This module defines relocation types and operations for applying
// relocations during the linking process.

use std::hash::Hash;

/// Types of relocations supported by the linker
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RelocationKind {
    /// R_X86_64_64: 64-bit absolute address
    Absolute64,
    /// R_X86_64_32: 32-bit absolute address
    Absolute32,
    /// R_X86_64_32S: 32-bit sign-extended absolute address
    Absolute32S,
    /// R_X86_64_PC32: 32-bit PC-relative address
    PC32,
    /// R_X86_64_PLT32: 32-bit PLT-relative address (should use same formula as PC32)
    PLT32,
    /// R_X86_64_GOTPCREL: 32-bit PC-relative offset to GOT entry (we convert to PC32)
    GOTPCREL,
    /// R_X86_64_REX_GOTPCRELX: 32-bit PC-relative offset to GOT entry (optimized REX encoding)
    REX_GOTPCRELX,
    /// R_X86_64_GOTPCRELX: 32-bit PC-relative offset to GOT entry (optimized encoding)
    GOTPCRELX,
}

impl RelocationKind {
    /// Convert from x86-64 relocation type number
    pub fn from_x86_64_type(reloc_type: u32) -> Option<Self> {
        match reloc_type {
            1 => Some(Self::Absolute64),
            2 => Some(Self::PC32),
            4 => Some(Self::PLT32),
            9 => Some(Self::GOTPCREL),
            10 => Some(Self::Absolute32),
            11 => Some(Self::Absolute32S),
            41 => Some(Self::REX_GOTPCRELX),
            42 => Some(Self::GOTPCRELX),
            _ => None,
        }
    }
}

/// Represents a relocation entry that needs to be applied during linking
#[derive(Debug, Clone)]
pub struct RelocationEntry {
    /// Name of the section where the relocation is applied
    pub section_name: String,
    /// Offset within the section where the relocation is applied
    pub offset: u64,
    /// Type of relocation
    pub kind: RelocationKind,
    /// Name of the symbol this relocation refers to
    pub symbol_name: String,
    /// Addend value to add to the symbol address
    pub addend: i64,
}

/// A key for deduplicating relocations
/// This ensures we don't apply the same relocation multiple times
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RelocationKey {
    pub section_name: String,
    pub offset: u64,
    pub kind: RelocationKind,
    pub symbol_name: String,
    pub addend: i64,
}

impl RelocationEntry {
    /// Create a new relocation entry
    pub fn new(
        section_name: String,
        offset: u64,
        kind: RelocationKind,
        symbol_name: String,
        addend: i64,
    ) -> Self {
        Self {
            section_name,
            offset,
            kind,
            symbol_name,
            addend,
        }
    }

    /// Get a deduplication key for this relocation
    /// This creates a key that uniquely identifies this relocation
    /// to prevent duplicate application
    ///
    /// CRITICAL: We must preserve internal relocations within runtime library archives.
    /// These are relocations within functions like __rue_println_i64 that reference
    /// internal data like string constants in .rodata sections.
    pub fn dedup_key(&self) -> RelocationKey {
        RelocationKey {
            section_name: self.section_name.clone(),
            offset: self.offset,
            kind: self.kind,
            symbol_name: self.symbol_name.clone(),
            addend: self.addend,
        }
    }

    /// Check if this relocation is absolute (not position-dependent)
    pub fn is_absolute(&self) -> bool {
        matches!(
            self.kind,
            RelocationKind::Absolute64 | RelocationKind::Absolute32 | RelocationKind::Absolute32S
        )
    }

    /// Check if this relocation is PC-relative
    pub fn is_pc_relative(&self) -> bool {
        matches!(
            self.kind,
            RelocationKind::PC32
                | RelocationKind::PLT32
                | RelocationKind::GOTPCREL
                | RelocationKind::REX_GOTPCRELX
                | RelocationKind::GOTPCRELX
        )
    }

    /// Get the size in bytes of the relocation target
    pub fn size_bytes(&self) -> usize {
        match self.kind {
            RelocationKind::Absolute64 => 8,
            RelocationKind::Absolute32 => 4,
            RelocationKind::Absolute32S => 4,
            RelocationKind::PC32 => 4,
            RelocationKind::PLT32 => 4,
            RelocationKind::GOTPCREL => 4,
            RelocationKind::REX_GOTPCRELX => 4,
            RelocationKind::GOTPCRELX => 4,
        }
    }
}

impl std::fmt::Display for RelocationKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RelocationKind::Absolute64 => write!(f, "R_X86_64_64"),
            RelocationKind::Absolute32 => write!(f, "R_X86_64_32"),
            RelocationKind::Absolute32S => write!(f, "R_X86_64_32S"),
            RelocationKind::PC32 => write!(f, "R_X86_64_PC32"),
            RelocationKind::PLT32 => write!(f, "R_X86_64_PLT32"),
            RelocationKind::GOTPCREL => write!(f, "R_X86_64_GOTPCREL"),
            RelocationKind::REX_GOTPCRELX => write!(f, "R_X86_64_REX_GOTPCRELX"),
            RelocationKind::GOTPCRELX => write!(f, "R_X86_64_GOTPCRELX"),
        }
    }
}

impl std::fmt::Display for RelocationEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}:{:08x} {} {} + {}",
            self.section_name, self.offset, self.kind, self.symbol_name, self.addend
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_relocation_kind_properties() {
        assert!(
            RelocationEntry::new(
                ".text".to_string(),
                0x100,
                RelocationKind::Absolute64,
                "symbol".to_string(),
                0
            )
            .is_absolute()
        );

        assert!(
            RelocationEntry::new(
                ".text".to_string(),
                0x100,
                RelocationKind::Absolute32,
                "symbol".to_string(),
                0
            )
            .is_absolute()
        );

        assert!(
            RelocationEntry::new(
                ".text".to_string(),
                0x100,
                RelocationKind::Absolute32S,
                "symbol".to_string(),
                0
            )
            .is_absolute()
        );

        assert!(
            RelocationEntry::new(
                ".text".to_string(),
                0x100,
                RelocationKind::PC32,
                "symbol".to_string(),
                0
            )
            .is_pc_relative()
        );
    }

    #[test]
    fn test_relocation_size() {
        let abs64_rel = RelocationEntry::new(
            ".text".to_string(),
            0x100,
            RelocationKind::Absolute64,
            "symbol".to_string(),
            0,
        );
        assert_eq!(abs64_rel.size_bytes(), 8);

        let abs32_rel = RelocationEntry::new(
            ".text".to_string(),
            0x100,
            RelocationKind::Absolute32,
            "symbol".to_string(),
            0,
        );
        assert_eq!(abs32_rel.size_bytes(), 4);

        let abs32s_rel = RelocationEntry::new(
            ".text".to_string(),
            0x100,
            RelocationKind::Absolute32S,
            "symbol".to_string(),
            0,
        );
        assert_eq!(abs32s_rel.size_bytes(), 4);

        let pc_rel = RelocationEntry::new(
            ".text".to_string(),
            0x100,
            RelocationKind::PC32,
            "symbol".to_string(),
            0,
        );
        assert_eq!(pc_rel.size_bytes(), 4);
    }

    #[test]
    fn test_relocation_display() {
        let rel = RelocationEntry::new(
            ".text".to_string(),
            0x1234,
            RelocationKind::Absolute64,
            "my_symbol".to_string(),
            -4,
        );
        let display = format!("{}", rel);
        assert!(display.contains(".text"));
        assert!(display.contains("00001234"));
        assert!(display.contains("R_X86_64_64"));
        assert!(display.contains("my_symbol"));
        assert!(display.contains("-4"));
    }

    #[test]
    fn test_relocation_kind_from_x86_64_type() {
        assert_eq!(
            RelocationKind::from_x86_64_type(1),
            Some(RelocationKind::Absolute64)
        );
        assert_eq!(
            RelocationKind::from_x86_64_type(2),
            Some(RelocationKind::PC32)
        );
        assert_eq!(
            RelocationKind::from_x86_64_type(4),
            Some(RelocationKind::PLT32)
        );
        assert_eq!(
            RelocationKind::from_x86_64_type(9),
            Some(RelocationKind::GOTPCREL)
        );
        assert_eq!(
            RelocationKind::from_x86_64_type(10),
            Some(RelocationKind::Absolute32)
        );
        assert_eq!(
            RelocationKind::from_x86_64_type(11),
            Some(RelocationKind::Absolute32S)
        );
        assert_eq!(
            RelocationKind::from_x86_64_type(41),
            Some(RelocationKind::REX_GOTPCRELX)
        );
        assert_eq!(
            RelocationKind::from_x86_64_type(42),
            Some(RelocationKind::GOTPCRELX)
        );
        assert_eq!(RelocationKind::from_x86_64_type(999), None); // Unknown type
    }

    #[test]
    fn test_absolute32_display() {
        assert_eq!(format!("{}", RelocationKind::Absolute32), "R_X86_64_32");
        assert_eq!(format!("{}", RelocationKind::Absolute32S), "R_X86_64_32S");
    }
}
