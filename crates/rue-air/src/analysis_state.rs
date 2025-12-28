//! Per-function mutable state for semantic analysis.
//!
//! This module contains state that is mutated during function analysis.
//! Each function can have its own `FunctionAnalysisState`, which is then
//! merged after parallel analysis completes.

use std::collections::HashMap;

use rue_error::CompileWarning;

use crate::types::{ArrayTypeDef, ArrayTypeId, Type};

/// Per-function mutable state during semantic analysis.
///
/// This struct contains all mutable state that is modified during function
/// body analysis. For parallel analysis, each function gets its own instance,
/// and results are merged afterward.
///
/// # Contents
///
/// - Array types created during analysis
/// - String literals encountered
/// - Warnings generated
///
/// # Merging
///
/// After parallel analysis, use `merge_into` to combine results:
/// - Array types are deduplicated
/// - Strings are deduplicated
/// - Warnings are concatenated
#[derive(Debug, Default)]
pub struct FunctionAnalysisState {
    /// Array types created during this function's analysis.
    /// Key is (element_type, length), value is the ArrayTypeId assigned.
    pub array_types: HashMap<(Type, u64), ArrayTypeId>,
    /// Array type definitions in order of creation.
    pub array_type_defs: Vec<ArrayTypeDef>,
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

    /// Get or create an array type, returning its ID.
    pub fn get_or_create_array_type(&mut self, element_type: Type, length: u64) -> ArrayTypeId {
        let key = (element_type, length);
        if let Some(&id) = self.array_types.get(&key) {
            return id;
        }

        let id = ArrayTypeId(self.array_type_defs.len() as u32);
        self.array_type_defs.push(ArrayTypeDef {
            element_type,
            length,
        });
        self.array_types.insert(key, id);
        id
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
#[derive(Debug, Default)]
pub struct MergedAnalysisState {
    /// All array type definitions (deduplicated).
    pub array_type_defs: Vec<ArrayTypeDef>,
    /// Mapping from original (element_type, length) to final ArrayTypeId.
    pub array_type_map: HashMap<(Type, u64), ArrayTypeId>,
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
    /// Returns a remapping for array type IDs so the function's AIR
    /// can be updated with the final IDs.
    pub fn merge_function_state(&mut self, state: FunctionAnalysisState) -> AnalysisStateRemapping {
        let mut array_remap = HashMap::new();
        let mut string_remap = HashMap::new();

        // Merge array types (deduplicate by (element_type, length))
        for (key, old_id) in state.array_types {
            let new_id = if let Some(&id) = self.array_type_map.get(&key) {
                id
            } else {
                let id = ArrayTypeId(self.array_type_defs.len() as u32);
                self.array_type_defs.push(ArrayTypeDef {
                    element_type: key.0,
                    length: key.1,
                });
                self.array_type_map.insert(key, id);
                id
            };
            if old_id != new_id {
                array_remap.insert(old_id, new_id);
            }
        }

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

        AnalysisStateRemapping {
            array_remap,
            string_remap,
        }
    }
}

/// Remapping information for updating AIR after merging.
///
/// When function analysis states are merged, IDs may change due to
/// deduplication. This struct provides the mapping from old to new IDs.
#[derive(Debug, Default)]
pub struct AnalysisStateRemapping {
    /// Mapping from old ArrayTypeId to new ArrayTypeId.
    /// Only contains entries where the ID changed.
    pub array_remap: HashMap<ArrayTypeId, ArrayTypeId>,
    /// Mapping from old string index to new string index.
    /// Only contains entries where the index changed.
    pub string_remap: HashMap<u32, u32>,
}

impl AnalysisStateRemapping {
    /// Check if any remapping is needed.
    pub fn is_empty(&self) -> bool {
        self.array_remap.is_empty() && self.string_remap.is_empty()
    }

    /// Remap an array type ID if needed.
    pub fn remap_array_type(&self, id: ArrayTypeId) -> ArrayTypeId {
        self.array_remap.get(&id).copied().unwrap_or(id)
    }

    /// Remap a string index if needed.
    pub fn remap_string(&self, id: u32) -> u32 {
        self.string_remap.get(&id).copied().unwrap_or(id)
    }
}
