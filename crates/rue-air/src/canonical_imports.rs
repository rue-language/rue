//! Accepted, already-resolved import identities for one semantic epoch.

use std::collections::{BTreeMap, HashMap};

use rue_error::{CompileError, CompileResult, ErrorKind};
use rue_span::{FileId, Span};

use crate::{ModuleDef, ModuleId, ModuleRegistry, normalize_module_path};

/// Read-only bridge from the compiler's canonical source/import artifacts.
///
/// Implementations borrow their owning artifacts and expose fields only while
/// AIR builds its compact semantic lookup. AIR does not own a peer import graph.
pub trait CanonicalImportView {
    fn visit_modules(
        &self,
        visitor: &mut dyn FnMut(&str, FileId, &str) -> CompileResult<()>,
    ) -> CompileResult<()>;

    fn visit_resolved_sites(
        &self,
        visitor: &mut dyn FnMut(&str, u32, &str, &str) -> CompileResult<()>,
    ) -> CompileResult<()>;
}

#[derive(Debug)]
pub(crate) struct CanonicalImportContext {
    by_file: HashMap<FileId, String>,
    sites: BTreeMap<(String, u32, String), ModuleId>,
}

impl CanonicalImportContext {
    pub(crate) fn build(view: &dyn CanonicalImportView) -> CompileResult<(Self, ModuleRegistry)> {
        let mut display_names = BTreeMap::new();
        let mut unresolved_sites = BTreeMap::new();
        view.visit_resolved_sites(&mut |importer, source_offset, specifier, target| {
            display_names
                .entry(target.to_owned())
                .or_insert_with(|| specifier.to_owned());
            let key = (
                importer.to_owned(),
                source_offset,
                normalize_module_path(specifier),
            );
            if unresolved_sites.insert(key, target.to_owned()).is_some() {
                return Err(invalid("canonical import site is duplicated"));
            }
            Ok(())
        })?;

        let mut modules = BTreeMap::new();
        let mut files = HashMap::new();
        view.visit_modules(&mut |durable_id, file_id, file_path| {
            if modules
                .insert(durable_id.to_owned(), (file_id, file_path.to_owned()))
                .is_some()
                || files.insert(file_id, durable_id.to_owned()).is_some()
            {
                return Err(invalid(
                    "canonical semantic module identities are not one-to-one",
                ));
            }
            Ok(())
        })?;

        let registry = ModuleRegistry::new();
        let mut by_file = HashMap::new();
        let mut compact = BTreeMap::new();
        for (durable_id, (file_id, file_path)) in modules {
            let display_name = display_names
                .remove(&durable_id)
                .unwrap_or_else(|| durable_id.clone());
            let id = registry.push_canonical(ModuleDef::new(
                display_name,
                file_path,
                durable_id.clone(),
                file_id,
            ));
            by_file.insert(file_id, durable_id.clone());
            compact.insert(durable_id, id);
        }
        let mut sites = BTreeMap::new();
        for ((importer, source_offset, specifier), target) in unresolved_sites {
            if !compact.contains_key(&importer) {
                return Err(invalid("canonical import has a foreign importer"));
            }
            let Some(&target) = compact.get(&target) else {
                return Err(invalid("canonical import has a foreign target"));
            };
            sites.insert((importer, source_offset, specifier), target);
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

    struct FixtureModule(&'static str, u32, &'static str);
    struct FixtureSite(&'static str, u32, &'static str, &'static str);
    struct FixtureView {
        modules: Vec<FixtureModule>,
        sites: Vec<FixtureSite>,
    }

    impl CanonicalImportView for FixtureView {
        fn visit_modules(
            &self,
            visitor: &mut dyn FnMut(&str, FileId, &str) -> CompileResult<()>,
        ) -> CompileResult<()> {
            for FixtureModule(id, file, path) in &self.modules {
                visitor(id, FileId::new(*file), path)?;
            }
            Ok(())
        }

        fn visit_resolved_sites(
            &self,
            visitor: &mut dyn FnMut(&str, u32, &str, &str) -> CompileResult<()>,
        ) -> CompileResult<()> {
            for FixtureSite(importer, offset, specifier, target) in &self.sites {
                visitor(importer, *offset, specifier, target)?;
            }
            Ok(())
        }
    }

    #[test]
    fn compact_ids_are_durable_ordered_and_file_ids_are_arbitrary() {
        let view = FixtureView {
            modules: vec![
                FixtureModule("z.rue", 41, "/moved/z.rue"),
                FixtureModule("a.rue", 900, "/moved/a.rue"),
            ],
            sites: vec![FixtureSite("z.rue", 17, "./a.rue", "a.rue")],
        };
        let (context, registry) = CanonicalImportContext::build(&view).unwrap();
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
        let empty = FixtureView {
            modules: vec![FixtureModule("main.rue", 7, "/main.rue")],
            sites: vec![],
        };
        let (context, _) = CanonicalImportContext::build(&empty).unwrap();
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
            CanonicalImportContext::build(&FixtureView {
                modules: vec![FixtureModule("main.rue", 7, "/main.rue")],
                sites: vec![FixtureSite("foreign.rue", 0, "main", "main.rue")],
            })
            .is_err()
        );
    }
}
