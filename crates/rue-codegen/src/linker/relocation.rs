// Relocation handling for object file linking
//
// This module defines relocation types and operations for applying
// relocations during the linking process.

/// Types of relocations supported by the linker
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelocationKind {
    /// R_X86_64_64: 64-bit absolute address
    Absolute64,
    /// R_X86_64_PC32: 32-bit PC-relative address
    PC32,
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

    /// Check if this relocation is absolute (not position-dependent)
    pub fn is_absolute(&self) -> bool {
        matches!(self.kind, RelocationKind::Absolute64)
    }

    /// Check if this relocation is PC-relative
    pub fn is_pc_relative(&self) -> bool {
        matches!(self.kind, RelocationKind::PC32)
    }

    /// Get the size in bytes of the relocation target
    pub fn size_bytes(&self) -> usize {
        match self.kind {
            RelocationKind::Absolute64 => 8,
            RelocationKind::PC32 => 4,
        }
    }
}

impl std::fmt::Display for RelocationKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RelocationKind::Absolute64 => write!(f, "R_X86_64_64"),
            RelocationKind::PC32 => write!(f, "R_X86_64_PC32"),
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
                RelocationKind::PC32,
                "symbol".to_string(),
                0
            )
            .is_pc_relative()
        );
    }

    #[test]
    fn test_relocation_size() {
        let abs_rel = RelocationEntry::new(
            ".text".to_string(),
            0x100,
            RelocationKind::Absolute64,
            "symbol".to_string(),
            0,
        );
        assert_eq!(abs_rel.size_bytes(), 8);

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
}
