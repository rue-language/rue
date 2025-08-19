// Symbol table management for object file linking
//
// This module defines symbol types and provides a symbol table for
// resolving symbols during the linking process.

use std::collections::HashMap;

/// Types of symbols in object files
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SymbolKind {
    /// Global symbol (visible to other object files)
    Global,
    /// Local symbol (only visible within this object file)
    Local,
    /// Weak symbol (can be overridden by global symbols)
    Weak,
}

/// Represents a symbol in an object file
#[derive(Debug, Clone)]
pub struct Symbol {
    /// Symbol name
    pub name: String,
    /// Symbol visibility/binding
    pub kind: SymbolKind,
    /// Address/value of the symbol
    pub address: u64,
    /// Size of the symbol in bytes
    pub size: u64,
    /// Name of the section containing this symbol (empty if undefined)
    pub section_name: String,
}

/// Symbol table for resolving symbols during linking
#[derive(Debug, Clone)]
pub struct SymbolTable {
    symbols: HashMap<String, Symbol>,
    global_symbols: HashMap<String, Symbol>,
    local_symbols: HashMap<String, Symbol>,
    weak_symbols: HashMap<String, Symbol>,
}

impl Symbol {
    /// Create a new symbol
    pub fn new(
        name: String,
        kind: SymbolKind,
        address: u64,
        size: u64,
        section_name: String,
    ) -> Self {
        Self {
            name,
            kind,
            address,
            size,
            section_name,
        }
    }

    /// Check if this symbol is defined (has a section)
    pub fn is_defined(&self) -> bool {
        !self.section_name.is_empty()
    }

    /// Check if this symbol is undefined (external reference)
    pub fn is_undefined(&self) -> bool {
        self.section_name.is_empty()
    }

    /// Check if this symbol is global
    pub fn is_global(&self) -> bool {
        matches!(self.kind, SymbolKind::Global)
    }

    /// Check if this symbol is local
    pub fn is_local(&self) -> bool {
        matches!(self.kind, SymbolKind::Local)
    }

    /// Check if this symbol is weak
    pub fn is_weak(&self) -> bool {
        matches!(self.kind, SymbolKind::Weak)
    }
}

impl Default for SymbolTable {
    fn default() -> Self {
        Self::new()
    }
}

impl SymbolTable {
    /// Create a new empty symbol table
    pub fn new() -> Self {
        Self {
            symbols: HashMap::new(),
            global_symbols: HashMap::new(),
            local_symbols: HashMap::new(),
            weak_symbols: HashMap::new(),
        }
    }

    /// Add a symbol to the table
    pub fn add_symbol(&mut self, symbol: Symbol) {
        let name = symbol.name.clone();

        // Add to the appropriate specialized table
        match symbol.kind {
            SymbolKind::Global => {
                self.global_symbols.insert(name.clone(), symbol.clone());
            }
            SymbolKind::Local => {
                self.local_symbols.insert(name.clone(), symbol.clone());
            }
            SymbolKind::Weak => {
                self.weak_symbols.insert(name.clone(), symbol.clone());
            }
        }

        // Add to the main symbol table, with precedence rules:
        // Global symbols override weak symbols
        // Local symbols are kept separate and don't override
        match symbol.kind {
            SymbolKind::Global => {
                self.symbols.insert(name, symbol);
            }
            SymbolKind::Weak => {
                // Only add if no global symbol exists
                if !self.global_symbols.contains_key(&name) {
                    self.symbols.insert(name, symbol);
                }
            }
            SymbolKind::Local => {
                // Local symbols don't go in the main table for resolution
                // They're kept separate for completeness
            }
        }
    }

    /// Get a symbol by name (follows precedence rules)
    pub fn get_symbol(&self, name: &str) -> Option<&Symbol> {
        // First try global symbols
        if let Some(symbol) = self.global_symbols.get(name) {
            return Some(symbol);
        }

        // Then try weak symbols
        if let Some(symbol) = self.weak_symbols.get(name) {
            return Some(symbol);
        }

        // Local symbols are not returned for external resolution
        None
    }

    /// Get a local symbol by name
    pub fn get_local_symbol(&self, name: &str) -> Option<&Symbol> {
        self.local_symbols.get(name)
    }

    /// Get all global symbols
    pub fn global_symbols(&self) -> &HashMap<String, Symbol> {
        &self.global_symbols
    }

    /// Get all local symbols
    pub fn local_symbols(&self) -> &HashMap<String, Symbol> {
        &self.local_symbols
    }

    /// Get all weak symbols
    pub fn weak_symbols(&self) -> &HashMap<String, Symbol> {
        &self.weak_symbols
    }

    /// Check if a symbol exists in the table
    pub fn contains_symbol(&self, name: &str) -> bool {
        self.get_symbol(name).is_some()
    }

    /// Get the number of symbols in the table
    pub fn len(&self) -> usize {
        self.global_symbols.len() + self.local_symbols.len() + self.weak_symbols.len()
    }

    /// Check if the symbol table is empty
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Get all symbol names
    pub fn symbol_names(&self) -> Vec<String> {
        let mut names = Vec::new();
        names.extend(self.global_symbols.keys().cloned());
        names.extend(self.local_symbols.keys().cloned());
        names.extend(self.weak_symbols.keys().cloned());
        names.sort();
        names
    }
}

impl std::fmt::Display for SymbolKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SymbolKind::Global => write!(f, "GLOBAL"),
            SymbolKind::Local => write!(f, "LOCAL"),
            SymbolKind::Weak => write!(f, "WEAK"),
        }
    }
}

impl std::fmt::Display for Symbol {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let section = if self.is_defined() {
            &self.section_name
        } else {
            "UNDEF"
        };
        write!(
            f,
            "{:016x} {} {} {} ({})",
            self.address, self.kind, section, self.name, self.size
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_symbol_properties() {
        let symbol = Symbol::new(
            "test_symbol".to_string(),
            SymbolKind::Global,
            0x1000,
            100,
            ".text".to_string(),
        );

        assert!(symbol.is_defined());
        assert!(!symbol.is_undefined());
        assert!(symbol.is_global());
        assert!(!symbol.is_local());
        assert!(!symbol.is_weak());
    }

    #[test]
    fn test_undefined_symbol() {
        let symbol = Symbol::new(
            "extern_symbol".to_string(),
            SymbolKind::Global,
            0,
            0,
            "".to_string(),
        );

        assert!(!symbol.is_defined());
        assert!(symbol.is_undefined());
    }

    #[test]
    fn test_symbol_table_precedence() {
        let mut table = SymbolTable::new();

        // Add a weak symbol first
        let weak_symbol = Symbol::new(
            "test".to_string(),
            SymbolKind::Weak,
            0x1000,
            100,
            ".text".to_string(),
        );
        table.add_symbol(weak_symbol);

        // Add a global symbol with the same name
        let global_symbol = Symbol::new(
            "test".to_string(),
            SymbolKind::Global,
            0x2000,
            200,
            ".text".to_string(),
        );
        table.add_symbol(global_symbol);

        // The global symbol should take precedence
        let resolved = table.get_symbol("test").unwrap();
        assert_eq!(resolved.address, 0x2000);
        assert!(resolved.is_global());
    }

    #[test]
    fn test_local_symbols_not_resolved() {
        let mut table = SymbolTable::new();

        let local_symbol = Symbol::new(
            "local_test".to_string(),
            SymbolKind::Local,
            0x1000,
            100,
            ".text".to_string(),
        );
        table.add_symbol(local_symbol);

        // Local symbols shouldn't be found by normal resolution
        assert!(table.get_symbol("local_test").is_none());

        // But should be found by local symbol lookup
        assert!(table.get_local_symbol("local_test").is_some());
    }

    #[test]
    fn test_symbol_table_operations() {
        let mut table = SymbolTable::new();
        assert!(table.is_empty());

        let symbol = Symbol::new(
            "test".to_string(),
            SymbolKind::Global,
            0x1000,
            100,
            ".text".to_string(),
        );
        table.add_symbol(symbol);

        assert!(!table.is_empty());
        assert_eq!(table.len(), 1);
        assert!(table.contains_symbol("test"));
        assert!(!table.contains_symbol("nonexistent"));

        let names = table.symbol_names();
        assert_eq!(names, vec!["test".to_string()]);
    }
}
