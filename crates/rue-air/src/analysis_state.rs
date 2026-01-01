//! Per-function mutable state for semantic analysis.
//!
//! This module contains state that is mutated during function analysis.
//! Each function can have its own `FunctionAnalysisState`, which is then
//! merged after parallel analysis completes.
//!
//! # Array Type Handling (Phase 2 - ADR-0024)
//!
//! Array types are now handled by the shared `ArrayTypeRegistry` in `SemaContext`,
//! which is thread-safe and handles deduplication automatically. Per-function
//! array tracking has been removed - array types created during function analysis
//! go directly to the shared registry.

use std::collections::HashMap;

use rue_error::CompileWarning;

/// Per-function mutable state during semantic analysis.
///
/// This struct contains all mutable state that is modified during function
/// body analysis. For parallel analysis, each function gets its own instance,
/// and results are merged afterward.
///
/// # Contents
///
/// - String literals encountered
/// - Warnings generated
///
/// # Note on Array Types
///
/// Array types are handled by the shared `ArrayTypeRegistry` in `SemaContext`.
/// They are no longer tracked per-function as of ADR-0024 Phase 2.
///
/// # Merging
///
/// After parallel analysis, use `merge_into` to combine results:
/// - Strings are deduplicated
/// - Warnings are concatenated
#[derive(Debug, Default)]
pub struct FunctionAnalysisState {
    /// String table for deduplication.
    pub string_table: HashMap<String, u32>,
    /// String literals in order of creation.
    pub strings: Vec<String>,
    /// Warnings collected during analysis.
    pub warnings: Vec<CompileWarning>,
}

impl FunctionAnalysisState {
    /// Create a new empty analysis state.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a string to the string table, returning its index.
    /// Deduplicates identical strings.
    pub fn add_string(&mut self, content: String) -> u32 {
        use std::collections::hash_map::Entry;
        match self.string_table.entry(content) {
            Entry::Occupied(e) => *e.get(),
            Entry::Vacant(e) => {
                let id = self.strings.len() as u32;
                self.strings.push(e.key().clone());
                e.insert(id);
                id
            }
        }
    }

    /// Add a warning.
    pub fn add_warning(&mut self, warning: CompileWarning) {
        self.warnings.push(warning);
    }
}

/// Merged state from multiple function analyses.
///
/// This is the result of merging all `FunctionAnalysisState` instances
/// after parallel analysis completes.
///
/// # Note on Array Types
///
/// Array types are handled by the shared `ArrayTypeRegistry` in `SemaContext`.
/// They are no longer merged here as of ADR-0024 Phase 2.
#[derive(Debug, Default)]
pub struct MergedAnalysisState {
    /// All string literals (deduplicated).
    pub strings: Vec<String>,
    /// Mapping from string content to final index.
    pub string_map: HashMap<String, u32>,
    /// All warnings from all functions.
    pub warnings: Vec<CompileWarning>,
}

impl MergedAnalysisState {
    /// Create a new empty merged state.
    pub fn new() -> Self {
        Self::default()
    }

    /// Merge a function's analysis state into this merged state.
    ///
    /// Returns a remapping for string indices so the function's AIR
    /// can be updated with the final IDs.
    ///
    /// # Note
    ///
    /// Array type merging is no longer needed (ADR-0024 Phase 2).
    /// Array types go directly to the shared `ArrayTypeRegistry`.
    pub fn merge_function_state(&mut self, state: FunctionAnalysisState) -> AnalysisStateRemapping {
        let mut string_remap = HashMap::new();

        // Merge strings (deduplicate by content)
        for (content, old_id) in state.string_table {
            let new_id = if let Some(&id) = self.string_map.get(&content) {
                id
            } else {
                let id = self.strings.len() as u32;
                self.strings.push(content.clone());
                self.string_map.insert(content, id);
                id
            };
            if old_id != new_id {
                string_remap.insert(old_id, new_id);
            }
        }

        // Merge warnings (no deduplication needed)
        self.warnings.extend(state.warnings);

        AnalysisStateRemapping { string_remap }
    }
}

/// Remapping information for updating AIR after merging.
///
/// When function analysis states are merged, IDs may change due to
/// deduplication. This struct provides the mapping from old to new IDs.
///
/// # Note
///
/// Array type remapping is no longer needed (ADR-0024 Phase 2).
/// Array types use the shared `ArrayTypeRegistry` which handles deduplication.
#[derive(Debug, Default)]
pub struct AnalysisStateRemapping {
    /// Mapping from old string index to new string index.
    /// Only contains entries where the index changed.
    pub string_remap: HashMap<u32, u32>,
}

impl AnalysisStateRemapping {
    /// Check if any remapping is needed.
    pub fn is_empty(&self) -> bool {
        self.string_remap.is_empty()
    }

    /// Remap a string index if needed.
    pub fn remap_string(&self, id: u32) -> u32 {
        self.string_remap.get(&id).copied().unwrap_or(id)
    }
}
