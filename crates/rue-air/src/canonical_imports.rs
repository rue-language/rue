//! Accepted, already-resolved import identities for one semantic epoch.

use std::collections::{BTreeMap, HashMap};

use rue_error::{CompileError, CompileResult, ErrorKind};
use rue_span::{FileId, Span};

use crate::{ModuleDef, ModuleId, ModuleRegistry, normalize_module_path};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SemanticModuleIdentity {
    pub durable_id: String,
    pub file_id: FileId,
    pub file_path: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SemanticResolvedImport {
    pub importer: String,
    pub source_offset: u32,
    pub specifier: String,
    pub target: String,
}

#[derive(Debug)]
pub(crate) struct CanonicalImportContext {
    by_file: HashMap<FileId, String>,
    sites: BTreeMap<(String, u32, String), ModuleId>,
}

impl CanonicalImportContext {
    pub(crate) fn build(
        mut modules: Vec<SemanticModuleIdentity>,
        mut imports: Vec<SemanticResolvedImport>,
    ) -> CompileResult<(Self, ModuleRegistry)> {
        modules.sort_by(|a, b| a.durable_id.cmp(&b.durable_id));
        imports.sort_by(|a, b| {
            (
                &a.importer,
                a.source_offset,
                normalize_module_path(&a.specifier),
                &a.target,
            )
                .cmp(&(
                    &b.importer,
                    b.source_offset,
                    normalize_module_path(&b.specifier),
                    &b.target,
                ))
        });
        let mut display_names = BTreeMap::new();
        for import in &imports {
            display_names
                .entry(import.target.clone())
                .or_insert_with(|| import.specifier.clone());
        }
        let registry = ModuleRegistry::new();
        let mut by_file = HashMap::new();
        let mut compact = BTreeMap::new();
        for module in modules {
            if compact.contains_key(&module.durable_id)
                || by_file
                    .insert(module.file_id, module.durable_id.clone())
                    .is_some()
            {
                return Err(invalid(
                    "canonical semantic module identities are not one-to-one",
                ));
            }
            let display_name = display_names
                .remove(&module.durable_id)
                .unwrap_or_else(|| module.durable_id.clone());
            let id = registry.push_canonical(ModuleDef::new(
                display_name,
                module.file_path,
                module.durable_id.clone(),
                module.file_id,
            ));
            compact.insert(module.durable_id, id);
        }
        let mut sites = BTreeMap::new();
        for import in imports {
            if !compact.contains_key(&import.importer) {
                return Err(invalid("canonical import has a foreign importer"));
            }
            let Some(&target) = compact.get(&import.target) else {
                return Err(invalid("canonical import has a foreign target"));
            };
            let key = (
                import.importer,
                import.source_offset,
                normalize_module_path(&import.specifier),
            );
            if sites.insert(key, target).is_some() {
                return Err(invalid("canonical import site is duplicated"));
            }
        }
        Ok((Self { by_file, sites }, registry))
    }

    pub(crate) fn resolve(&self, specifier: &str, span: Span) -> CompileResult<ModuleId> {
        let importer = self.by_file.get(&span.file_id).ok_or_else(|| {
            invalid("importing FileId is absent from the canonical semantic identity bridge")
        })?;
        self.sites
            .get(&(
                importer.clone(),
                span.start,
                normalize_module_path(specifier),
            ))
            .copied()
            .ok_or_else(|| invalid("import site is absent from accepted canonical import records"))
    }
}

fn invalid(message: &str) -> CompileError {
    CompileError::without_span(ErrorKind::InvalidCompilerInput(message.to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn module(id: &str, file: u32, path: &str) -> SemanticModuleIdentity {
        SemanticModuleIdentity {
            durable_id: id.to_owned(),
            file_id: FileId::new(file),
            file_path: path.to_owned(),
        }
    }

    #[test]
    fn compact_ids_are_durable_ordered_and_file_ids_are_arbitrary() {
        let (context, registry) = CanonicalImportContext::build(
            vec![
                module("z.rue", 41, "/moved/z.rue"),
                module("a.rue", 900, "/moved/a.rue"),
            ],
            vec![SemanticResolvedImport {
                importer: "z.rue".into(),
                source_offset: 17,
                specifier: "./a.rue".into(),
                target: "a.rue".into(),
            }],
        )
        .unwrap();
        assert_eq!(registry.get_def(ModuleId::new(0)).durable_id, "a.rue");
        assert_eq!(registry.get_def(ModuleId::new(0)).import_path, "./a.rue");
        assert_eq!(registry.get_def(ModuleId::new(1)).durable_id, "z.rue");
        assert_eq!(
            context
                .resolve("a.rue", Span::with_file(FileId::new(41), 17, 18))
                .unwrap(),
            ModuleId::new(0)
        );
    }

    #[test]
    fn sites_and_identity_bridge_fail_closed() {
        let (context, _) =
            CanonicalImportContext::build(vec![module("main.rue", 7, "/main.rue")], vec![])
                .unwrap();
        assert!(
            context
                .resolve("missing", Span::with_file(FileId::new(7), 1, 2))
                .is_err()
        );
        assert!(
            context
                .resolve("missing", Span::with_file(FileId::new(8), 1, 2))
                .is_err()
        );
        assert!(
            CanonicalImportContext::build(
                vec![module("main.rue", 7, "/main.rue")],
                vec![SemanticResolvedImport {
                    importer: "foreign.rue".into(),
                    source_offset: 0,
                    specifier: "main".into(),
                    target: "main.rue".into(),
                }],
            )
            .is_err()
        );
    }
}
