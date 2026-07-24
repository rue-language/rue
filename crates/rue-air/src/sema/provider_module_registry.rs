//! Body-local module registry assembled from durable module facts.

use std::collections::HashMap;
use std::hash::Hash;

use rue_span::FileId;

use crate::{ModuleDef, ModuleId};

/// Provider-era counterpart of the semantic epoch's `ModuleRegistry`.
///
/// The durable module handle is the deduplication key. Presentation paths and
/// the current request's file handle are registered alongside it because they
/// are request-local facts, not properties reconstructible from a compact id.
pub(in crate::sema) struct ProviderModuleRegistry<M> {
    by_durable: HashMap<M, ModuleId>,
    by_file: HashMap<FileId, ModuleId>,
    defs: Vec<ModuleDef>,
}

impl<M> Default for ProviderModuleRegistry<M> {
    fn default() -> Self {
        Self {
            by_durable: HashMap::new(),
            by_file: HashMap::new(),
            defs: Vec::new(),
        }
    }
}

impl<M: Eq + Hash> ProviderModuleRegistry<M> {
    pub(in crate::sema) fn register(
        &mut self,
        durable: M,
        file: FileId,
        file_path: &str,
        import_path: &str,
        durable_id: &str,
    ) -> Option<ModuleId> {
        if let Some(&id) = self.by_durable.get(&durable) {
            let def = self
                .defs
                .get(id.index() as usize)
                .expect("provider module durable key must index an existing definition");
            return (def.file_id == file
                && def.file_path == file_path
                && def.import_path == import_path
                && def.durable_id == durable_id)
                .then_some(id);
        }
        if self.by_file.contains_key(&file) {
            return None;
        }

        let id = ModuleId::new(self.defs.len() as u32);
        self.defs.push(ModuleDef::new(
            import_path.to_owned(),
            file_path.to_owned(),
            durable_id.to_owned(),
            file,
        ));
        self.by_durable.insert(durable, id);
        self.by_file.insert(file, id);
        Some(id)
    }

    pub(in crate::sema) fn get(&self, id: ModuleId) -> Option<ModuleDef> {
        self.defs.get(id.index() as usize).cloned()
    }

    pub(in crate::sema) fn id_for_file(&self, file: FileId) -> Option<ModuleId> {
        self.by_file.get(&file).copied()
    }

    pub(in crate::sema) fn id_for_durable(&self, durable: &M) -> Option<ModuleId> {
        self.by_durable.get(durable).copied()
    }
}
