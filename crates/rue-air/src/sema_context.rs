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

use crate::path_norm::normalize_module_path;
use crate::types::{ModuleDef, ModuleId};

/// Lexically normalize a resolved file path for use as a registry key so all
/// spellings of the same physical file key to one module (spec 10.2:4):
/// `./foo.rue` == `foo.rue`, and `a/../std/opt.rue` == `std/opt.rue`
/// (RUE-317). Purely lexical — never touches the filesystem.
fn normalize_key(path: &str) -> String {
    normalize_module_path(path)
}

/// Thread-safe registry for modules.
///
/// This registry allows concurrent lookups and insertions of imported modules during
/// parallel function analysis. It uses double-checked locking to minimize contention.
#[derive(Debug)]
pub struct ModuleRegistry {
    /// Maps a module's *resolved* file path (normalized) to its ModuleId.
    ///
    /// Keying by the resolved file rather than the `@import` string is what
    /// makes modules importer-relative (RUE-266): two `@import("foo")` sites in
    /// different directories resolve to different files and get distinct
    /// modules, while any two imports that resolve to the *same* file share one
    /// module (spec 10.2:4), whatever string or spelling reached it.
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

    /// Get or create a module for the given import path and resolved file path.
    ///
    /// The module is keyed by its *resolved* `file_path` (normalized), so two
    /// importers that reach the same file share one module and two that reach
    /// different files get distinct modules even under the same `@import`
    /// string (RUE-266). The `import_path` is retained only for diagnostics.
    ///
    /// Returns the ModuleId and whether it was newly created.
    pub fn get_or_create(&self, import_path: String, file_path: String) -> (ModuleId, bool) {
        let key = normalize_key(&file_path);

        // Fast path: check if already exists
        {
            let paths = self.paths.read().unwrap_or_else(PoisonError::into_inner);
            if let Some(id) = paths.get(&key) {
                return (*id, false);
            }
        }

        // Slow path: acquire write lock and insert
        let mut paths = self.paths.write().unwrap_or_else(PoisonError::into_inner);
        // Double-check after acquiring write lock
        if let Some(id) = paths.get(&key) {
            return (*id, false);
        }

        let mut defs = self.defs.write().unwrap_or_else(PoisonError::into_inner);
        let id = ModuleId::new(defs.len() as u32);
        defs.push(ModuleDef::new(import_path, file_path));
        paths.insert(key, id);
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
