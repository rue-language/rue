//! Thread-safe module registry.
//!
//! This module contains [`ModuleRegistry`], the shared registry of imported
//! modules used during semantic analysis. A semantic epoch prepopulates the
//! registry in canonical durable-ID order before analysis begins.
//!
use std::sync::{PoisonError, RwLock};

use crate::types::{ModuleDef, ModuleId};

/// Thread-safe registry for modules.
///
/// The registry allows concurrent lookups after canonical construction.
#[derive(Debug)]
pub struct ModuleRegistry {
    /// Module definitions indexed by compact, epoch-local ModuleId. Canonical
    /// semantic construction prepopulates this vector in durable-ID order.
    defs: RwLock<Vec<ModuleDef>>,
}

/// Deep-copies every registered module under a read lock, for the RUE-1133
/// clone-from-template probe (RUE-1091 ordered probe #1). Copying a registry is
/// a measurement instrument for what a per-body epoch costs structurally, never
/// a production sharing boundary.
impl Clone for ModuleRegistry {
    fn clone(&self) -> Self {
        Self {
            defs: RwLock::new(
                self.defs
                    .read()
                    .unwrap_or_else(PoisonError::into_inner)
                    .clone(),
            ),
        }
    }
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
