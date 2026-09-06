//! Thread-safe module registry.
//!
//! This module contains [`ModuleRegistry`], the shared registry of imported
//! modules used during semantic analysis. A semantic epoch prepopulates the
//! registry in canonical durable-ID order before analysis begins.
//!
use std::sync::{PoisonError, RwLock};

use crate::types::{ModuleDef, ModuleId};

/// How a module is named in a diagnostic that talks about it.
///
/// One rendering for every emitter: the import path the source wrote, so
/// `module 'sub/lib.rue' has no member 'x'` says which file was consulted.
/// Rendering only the file stem loses exactly the part that distinguishes
/// same-named files in different directories, which is the case the reader
/// most needs to see.
///
/// The argument is the import path rather than a [`ModuleDef`] because the
/// three emitters hold a module in three shapes — a registry definition, an
/// aggregate module fact, and a durable module identity — and the import path
/// is what they share. Pass [`ModuleDef::import_path`]; do not re-render it.
pub fn module_display_name(import_path: &str) -> &str {
    import_path
}

/// Thread-safe registry for modules.
///
/// The registry allows concurrent lookups after canonical construction.
#[derive(Debug)]
pub struct ModuleRegistry {
    /// Module definitions indexed by compact, epoch-local ModuleId. Canonical
    /// semantic construction prepopulates this vector in durable-ID order.
    defs: RwLock<Vec<ModuleDef>>,
}

impl ModuleRegistry {
    /// Create a new empty registry.
    pub fn new() -> Self {
        Self {
            defs: RwLock::new(Vec::new()),
        }
    }

    /// Append a module while constructing a fresh canonical semantic epoch.
    pub(crate) fn push_canonical(&self, def: ModuleDef) -> ModuleId {
        let mut defs = self.defs.write().unwrap_or_else(PoisonError::into_inner);
        let id = ModuleId::new(defs.len() as u32);
        defs.push(def);
        id
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
}

impl Default for ModuleRegistry {
    fn default() -> Self {
        Self::new()
    }
}
