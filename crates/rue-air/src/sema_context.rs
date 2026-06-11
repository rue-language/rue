//! Thread-safe module registry.
//!
//! This module contains [`ModuleRegistry`], the shared registry of imported
//! modules used during semantic analysis. It is `Send + Sync` so module
//! lookups and insertions can happen from any analysis context; it uses
//! `RwLock` with double-checked locking (read lock for lookups, write lock
//! only for new insertions).
//!
//! The `SemaContext` type that gave this file its name was part of the dead
//! parallel-analysis pipeline and was removed per ADR-0033 phase 1b.

use std::collections::HashMap;
use std::sync::{PoisonError, RwLock};

use crate::types::{ModuleDef, ModuleId};

/// Thread-safe registry for modules.
///
/// This registry allows concurrent lookups and insertions of imported modules during
/// parallel function analysis. It uses double-checked locking to minimize contention.
#[derive(Debug)]
pub struct ModuleRegistry {
    /// Maps import path (e.g., "math.rue") to ModuleId.
    paths: RwLock<HashMap<String, ModuleId>>,
    /// Module definitions indexed by ModuleId.
    defs: RwLock<Vec<ModuleDef>>,
}

impl ModuleRegistry {
    /// Create a new empty registry.
    pub fn new() -> Self {
        Self {
            paths: RwLock::new(HashMap::new()),
            defs: RwLock::new(Vec::new()),
        }
    }

    /// Look up a module by import path.
    pub fn get(&self, import_path: &str) -> Option<ModuleId> {
        self.paths
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .get(import_path)
            .copied()
    }

    /// Get or create a module for the given import path and resolved file path.
    ///
    /// Returns the ModuleId and whether it was newly created.
    pub fn get_or_create(&self, import_path: String, file_path: String) -> (ModuleId, bool) {
        // Fast path: check if already exists
        {
            let paths = self.paths.read().unwrap_or_else(PoisonError::into_inner);
            if let Some(id) = paths.get(&import_path) {
                return (*id, false);
            }
        }

        // Slow path: acquire write lock and insert
        let mut paths = self.paths.write().unwrap_or_else(PoisonError::into_inner);
        // Double-check after acquiring write lock
        if let Some(id) = paths.get(&import_path) {
            return (*id, false);
        }

        let mut defs = self.defs.write().unwrap_or_else(PoisonError::into_inner);
        let id = ModuleId::new(defs.len() as u32);
        defs.push(ModuleDef::new(import_path.clone(), file_path));
        paths.insert(import_path, id);
        (id, true)
    }

    /// Get a module definition by ID.
    pub fn get_def(&self, id: ModuleId) -> ModuleDef {
        self.defs
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .get(id.index() as usize)
            .cloned()
            .expect("Invalid ModuleId")
    }

    /// Update a module definition.
    pub fn update_def(&self, id: ModuleId, def: ModuleDef) {
        let mut defs = self.defs.write().unwrap_or_else(PoisonError::into_inner);
        defs[id.index() as usize] = def;
    }

    /// Get the number of modules in the registry.
    pub fn len(&self) -> usize {
        self.defs
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
    }

    /// Check if the registry is empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Extract the module definitions (consumes the registry).
    pub fn into_defs(self) -> Vec<ModuleDef> {
        self.defs
            .into_inner()
            .unwrap_or_else(PoisonError::into_inner)
    }
}

impl Default for ModuleRegistry {
    fn default() -> Self {
        Self::new()
    }
}
