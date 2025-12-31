//! String literal handling for semantic analysis.
//!
//! This module contains string interning and deduplication logic.

use std::collections::HashMap;

use super::Sema;

impl Sema<'_> {
    /// Add a string literal to the string table, deduplicating if already present.
    ///
    /// Returns the string ID (index into the strings vector).
    pub(crate) fn add_string(&mut self, content: String) -> u32 {
        if let Some(&idx) = self.string_table.get(&content) {
            return idx;
        }

        let idx = self.strings.len() as u32;
        self.strings.push(content.clone());
        self.string_table.insert(content, idx);
        idx
    }
}
