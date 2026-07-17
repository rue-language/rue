//! Canonical identity metadata required when constructing semantic analysis.

use std::collections::{HashMap, HashSet};

use rue_error::{CompileError, CompileResult, ErrorKind};
use rue_rir::Rir;
use rue_span::FileId;

use crate::path_norm::normalize_module_path;

/// Complete source identity metadata for one semantic request.
#[derive(Debug, Clone)]
pub struct SemaMetadata {
    pub(super) root_file_id: FileId,
    pub(super) physical_paths: HashMap<FileId, String>,
    pub(super) logical_paths: HashMap<FileId, String>,
}

impl SemaMetadata {
    /// Validate canonical root, physical-path, and logical-path metadata.
    pub fn new(
        root_file_id: FileId,
        physical_paths: HashMap<FileId, String>,
        logical_paths: HashMap<FileId, String>,
    ) -> CompileResult<Self> {
        validate(root_file_id, &physical_paths, &logical_paths)?;
        Ok(Self {
            root_file_id,
            physical_paths,
            logical_paths,
        })
    }

    /// Explicit single-file fixture for phase-local and synthetic tests.
    ///
    /// Such tests do not participate in durable compiler identity joins.
    pub fn synthetic_single_file(file_id: FileId, path: impl Into<String>) -> Self {
        let path = path.into();
        Self::new(
            file_id,
            HashMap::from([(file_id, path.clone())]),
            HashMap::from([(file_id, path)]),
        )
        .expect("synthetic sema fixture path must be non-empty")
    }

    pub(super) fn synthetic_for_rir(rir: &Rir) -> Self {
        let mut ids: Vec<_> = rir.iter().map(|(_, inst)| inst.span.file_id).collect();
        ids.sort_by_key(|id| id.index());
        ids.dedup();
        if ids.is_empty() {
            ids.push(FileId::DEFAULT);
        }
        let physical_paths: HashMap<_, _> = ids
            .iter()
            .map(|&id| (id, format!("synthetic/{}.rue", id.index())))
            .collect();
        Self::new(ids[0], physical_paths.clone(), physical_paths)
            .expect("synthetic RIR identities are complete")
    }
}

fn validate(
    root_file_id: FileId,
    physical_paths: &HashMap<FileId, String>,
    logical_paths: &HashMap<FileId, String>,
) -> CompileResult<()> {
    if physical_paths.is_empty() || !physical_paths.contains_key(&root_file_id) {
        return Err(invalid("semantic metadata requires a present root file"));
    }
    let physical_ids: HashSet<_> = physical_paths.keys().copied().collect();
    let logical_ids: HashSet<_> = logical_paths.keys().copied().collect();
    if physical_ids != logical_ids {
        return Err(invalid(
            "semantic physical and logical path maps must contain identical file IDs",
        ));
    }
    for (kind, paths) in [("physical", physical_paths), ("logical", logical_paths)] {
        let mut seen = HashSet::new();
        for (&file_id, path) in paths {
            let normalized = normalize_module_path(path);
            if normalized.is_empty() {
                return Err(invalid(format!(
                    "semantic {kind} path for {} has an empty identity",
                    file_id.index()
                )));
            }
            if !seen.insert(normalized.clone()) {
                return Err(invalid(format!(
                    "semantic {kind} paths contain duplicate identity {normalized:?}"
                )));
            }
        }
    }
    Ok(())
}

fn invalid(message: impl Into<String>) -> CompileError {
    CompileError::without_span(ErrorKind::InvalidCompilerInput(message.into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_metadata_requires_complete_logical_paths() {
        let root = FileId::new(7);
        let other = FileId::new(2);
        let error = SemaMetadata::new(
            root,
            HashMap::from([
                (root, "src/main.rue".to_owned()),
                (other, "src/helper.rue".to_owned()),
            ]),
            HashMap::from([(root, "pkg/main.rue".to_owned())]),
        )
        .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("must contain identical file IDs")
        );
    }

    #[test]
    fn canonical_metadata_rejects_colliding_logical_identities() {
        let root = FileId::new(7);
        let other = FileId::new(2);
        let error = SemaMetadata::new(
            root,
            HashMap::from([
                (root, "src/main.rue".to_owned()),
                (other, "src/helper.rue".to_owned()),
            ]),
            HashMap::from([
                (root, "pkg/./main.rue".to_owned()),
                (other, "pkg/main.rue".to_owned()),
            ]),
        )
        .unwrap_err();

        assert!(error.to_string().contains("duplicate identity"));
    }
}
