// Symbol table management for object file linking
//
// This module defines symbol types and provides a symbol table for
// resolving symbols during the linking process.

use std::collections::HashMap;

/// Policy trait for symbol resolution behavior
///
/// This trait allows customization of how symbols are resolved based on
/// their source and context. This is particularly important for no_std
/// environments where runtime library symbols need special handling.
pub trait SymbolResolutionPolicy {
    /// Determine if a local symbol should be available for resolution
    fn should_resolve_local(&self, symbol: &Symbol) -> bool;

    /// Determine resolution precedence between two symbols with the same name
    fn resolution_precedence(&self, existing: &Symbol, new: &Symbol) -> ResolutionChoice;
}

/// Choice for symbol resolution when conflicts occur
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolutionChoice {
    KeepExisting,
    UseNew,
}

/// Default policy for static linking in no_std environment
pub struct NoStdStaticLinkPolicy;

impl SymbolResolutionPolicy for NoStdStaticLinkPolicy {
    fn should_resolve_local(&self, symbol: &Symbol) -> bool {
        // In static linking, local symbols from runtime libraries should be resolvable
        matches!(symbol.source, SymbolSource::RuntimeLibrary)
    }

    fn resolution_precedence(&self, existing: &Symbol, new: &Symbol) -> ResolutionChoice {
        // Global symbols always take precedence over weak
        if existing.kind == SymbolKind::Global && new.kind == SymbolKind::Weak {
            return ResolutionChoice::KeepExisting;
        }
        if existing.kind == SymbolKind::Weak && new.kind == SymbolKind::Global {
            return ResolutionChoice::UseNew;
        }

        // User code takes precedence over runtime library
        if existing.source == SymbolSource::UserCode && new.source != SymbolSource::UserCode {
            return ResolutionChoice::KeepExisting;
        }
        if existing.source != SymbolSource::UserCode && new.source == SymbolSource::UserCode {
            return ResolutionChoice::UseNew;
        }

        // Otherwise keep existing
        ResolutionChoice::KeepExisting
    }
}

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

/// Source of a symbol (where it came from)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SymbolSource {
    /// Symbol from user code
    UserCode,
    /// Symbol from runtime library (libcore, compiler-rt, etc.)
    RuntimeLibrary,
    /// Symbol from other external library
    ExternalLibrary,
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
    /// Source of this symbol (for debugging and policy decisions)
    pub source: SymbolSource,
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
        // Infer source from symbol name patterns
        let source = Self::infer_source(&name);
        Self {
            name,
            kind,
            address,
            size,
            section_name,
            source,
        }
    }

    /// Create a new symbol with explicit source
    pub fn new_with_source(
        name: String,
        kind: SymbolKind,
        address: u64,
        size: u64,
        section_name: String,
        source: SymbolSource,
    ) -> Self {
        Self {
            name,
            kind,
            address,
            size,
            section_name,
            source,
        }
    }

    /// Infer the source of a symbol based on its name patterns
    fn infer_source(name: &str) -> SymbolSource {
        // CRITICAL: Check Rue runtime functions FIRST before other patterns
        // This must come before the "main" check since "__rue_main" contains "main"
        if name.starts_with("__rue_") {
            // Rue runtime functions (MUST be first)
            SymbolSource::RuntimeLibrary
        }
        // Rust runtime patterns
        else if name.starts_with("_ZN4core") ||      // Rust core library
                name.starts_with("_ZN5alloc") ||     // Rust alloc library  
                name.starts_with("rust_") ||         // Rust runtime functions
                name.starts_with("__rust") ||        // Rust compiler intrinsics
                name.starts_with(".Lanon") ||        // Anonymous constants from rustc
                name.starts_with(".LBB") ||          // LLVM basic block labels
                name.contains("compiler_builtins") || // Compiler builtins
                name.contains("compiler_rt")
        // Compiler runtime
        {
            SymbolSource::RuntimeLibrary
        } else if name.starts_with("_start") || // Entry point we generate
                  name.starts_with("main") ||   // User main function (now safe after __rue_ check)
                  name.starts_with("rue_")
        // Rue-specific user symbols
        {
            SymbolSource::UserCode
        } else {
            // Default to external for other symbols
            SymbolSource::ExternalLibrary
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
                // Check if we already have a defined global symbol with this name
                if let Some(existing) = self.global_symbols.get(&name) {
                    // Don't overwrite a defined symbol with an undefined one
                    if existing.is_defined() && symbol.is_undefined() {
                        return;
                    }
                }
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
                // Check if we already have a defined symbol with this name
                if let Some(existing) = self.symbols.get(&name) {
                    // Don't overwrite a defined symbol with an undefined one
                    if existing.is_defined() && symbol.is_undefined() {
                        return;
                    }
                }
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
            // Debug critical symbol resolution
            if name == "__rue_main" || name == "main" || name == "_start" {
                tracing::debug!(
                    "Symbol resolution: '{}' resolved to global symbol at 0x{:x} from section '{}' (source: {:?})",
                    name,
                    symbol.address,
                    symbol.section_name,
                    symbol.source
                );
            }
            return Some(symbol);
        }

        // Then try weak symbols
        if let Some(symbol) = self.weak_symbols.get(name) {
            // Debug critical symbol resolution
            if name == "__rue_main" || name == "main" || name == "_start" {
                tracing::debug!(
                    "Symbol resolution: '{}' resolved to weak symbol at 0x{:x} from section '{}' (source: {:?})",
                    name,
                    symbol.address,
                    symbol.section_name,
                    symbol.source
                );
            }
            return Some(symbol);
        }

        // Debug failed resolution for critical symbols
        if name == "__rue_main" || name == "main" || name == "_start" {
            tracing::warn!(
                "Symbol resolution: '{}' NOT FOUND in global or weak symbols",
                name
            );
        }

        // Local symbols are not returned for external resolution
        None
    }

    /// Get a symbol by name, including local symbols (for static library linking)
    /// This variant should be used when linking with static libraries where
    /// local symbols should be available for resolution
    pub fn get_symbol_including_local(&self, name: &str) -> Option<&Symbol> {
        // First try global symbols
        if let Some(symbol) = self.global_symbols.get(name) {
            return Some(symbol);
        }

        // Then try weak symbols
        if let Some(symbol) = self.weak_symbols.get(name) {
            return Some(symbol);
        }

        // Finally try local symbols (for static library linking)
        if let Some(symbol) = self.local_symbols.get(name) {
            return Some(symbol);
        }

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

    /// Check if a symbol exists in the table (including local symbols)
    pub fn contains_symbol_including_local(&self, name: &str) -> bool {
        self.get_symbol_including_local(name).is_some()
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

    /// Get all symbols in the table
    pub fn symbols(&self) -> Vec<&Symbol> {
        let mut symbols = Vec::new();
        symbols.extend(self.global_symbols.values());
        symbols.extend(self.local_symbols.values());
        symbols.extend(self.weak_symbols.values());
        symbols
    }

    /// Update the address of a symbol by name
    pub fn update_symbol_address(&mut self, name: &str, new_address: u64) {
        // Update in global symbols table
        if let Some(symbol) = self.global_symbols.get_mut(name) {
            symbol.address = new_address;
            // Also update in main symbols table
            if let Some(main_symbol) = self.symbols.get_mut(name) {
                main_symbol.address = new_address;
            }
        }

        // Update in local symbols table
        if let Some(symbol) = self.local_symbols.get_mut(name) {
            symbol.address = new_address;
        }

        // Update in weak symbols table
        if let Some(symbol) = self.weak_symbols.get_mut(name) {
            symbol.address = new_address;
            // Also update in main symbols table if this weak symbol is the active one
            if let Some(main_symbol) = self.symbols.get_mut(name) {
                main_symbol.address = new_address;
            }
        }
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

impl std::fmt::Display for SymbolSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SymbolSource::UserCode => write!(f, "USER"),
            SymbolSource::RuntimeLibrary => write!(f, "RUNTIME"),
            SymbolSource::ExternalLibrary => write!(f, "EXTERNAL"),
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
            "{:016x} {} {} {} {} ({})",
            self.address, self.kind, self.source, section, self.name, self.size
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
