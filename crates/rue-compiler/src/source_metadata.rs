//! Validated source identities at compiler API boundaries.
//!
//! Physical paths are used for diagnostics and module resolution. Logical
//! paths are semantic identities used for ordering and generated symbols;
//! callers that need relocation stability provide logical paths independent
//! of the checkout location. Keeping both maps together with an explicit root
//! makes it impossible for those coupled inputs to drift after construction.

use std::collections::HashMap;
#[cfg(test)]
use std::collections::HashSet;

use rue_air::normalize_module_path;
use rue_error::{CompileError, CompileResult, ErrorKind};
use rue_span::FileId;

use crate::ModuleId;
#[cfg(test)]
use crate::SourceView;

/// Immutable, validated identities for every source in a compilation.
///
/// The descriptor owns its path maps and keeps a separate ascending `FileId`
/// index. Callers therefore retain constant-time lookup while deterministic
/// iterators never depend on `HashMap` iteration order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceMetadata {
    root_file_id: FileId,
    sorted_ids: Vec<FileId>,
    physical_paths: HashMap<FileId, String>,
    logical_paths: HashMap<FileId, String>,
}

impl SourceMetadata {
    /// Validate complete physical and logical path maps for an explicit root.
    ///
    /// Both maps must have exactly the same nonempty set of `FileId` keys.
    /// Logical values are lexically normalized before storage. Callers must
    /// materialize both maps so relocation-sensitive defaults remain explicit.
    pub fn new(
        root_file_id: FileId,
        physical_paths: HashMap<FileId, String>,
        logical_paths: HashMap<FileId, String>,
    ) -> CompileResult<Self> {
        if physical_paths.is_empty() {
            return Err(invalid_input("source metadata contains no files"));
        }

        let mut file_ids: Vec<_> = physical_paths.keys().copied().collect();
        file_ids.sort_by_key(|file_id| file_id.index());

        if !physical_paths.contains_key(&root_file_id) {
            return Err(invalid_input(format!(
                "root file ID {} is absent from source metadata",
                display_file_id(root_file_id)
            )));
        }

        let missing_logical_ids: Vec<_> = file_ids
            .iter()
            .copied()
            .filter(|file_id| !logical_paths.contains_key(file_id))
            .collect();
        if !missing_logical_ids.is_empty() {
            return Err(invalid_input(format!(
                "logical path map is missing file IDs: {}",
                display_file_ids(&missing_logical_ids)
            )));
        }

        let mut unknown_logical_ids: Vec<_> = logical_paths
            .keys()
            .copied()
            .filter(|file_id| !physical_paths.contains_key(file_id))
            .collect();
        unknown_logical_ids.sort_by_key(|file_id| file_id.index());
        if !unknown_logical_ids.is_empty() {
            return Err(invalid_input(format!(
                "logical path map contains unknown file IDs: {}",
                display_file_ids(&unknown_logical_ids)
            )));
        }

        let logical_paths: HashMap<_, _> = logical_paths
            .into_iter()
            .map(|(file_id, path)| (file_id, normalize_module_path(&path)))
            .collect();

        validate_nonempty_paths("physical", &file_ids, &physical_paths)?;
        validate_nonempty_paths("logical", &file_ids, &logical_paths)?;
        validate_normalized_collisions("physical", &file_ids, &physical_paths)?;
        validate_normalized_collisions("logical", &file_ids, &logical_paths)?;

        Ok(Self {
            root_file_id,
            sorted_ids: file_ids,
            physical_paths,
            logical_paths,
        })
    }

    #[cfg(test)]
    pub(crate) fn from_sources(
        sources: &[SourceView<'_>],
        root_file_id: FileId,
        logical_path_overrides: HashMap<FileId, String>,
    ) -> CompileResult<Self> {
        if sources.is_empty() {
            return Err(invalid_input("source metadata contains no files"));
        }

        let mut counts = HashMap::<FileId, usize>::new();
        for source in sources {
            *counts.entry(source.file_id).or_default() += 1;
        }
        let mut duplicate_ids: Vec<_> = counts
            .into_iter()
            .filter_map(|(file_id, count)| (count > 1).then_some(file_id))
            .collect();
        duplicate_ids.sort_by_key(|file_id| file_id.index());
        if !duplicate_ids.is_empty() {
            return Err(invalid_input(format!(
                "source files contain duplicate file IDs: {}",
                display_file_ids(&duplicate_ids)
            )));
        }

        let physical_paths: HashMap<_, _> = sources
            .iter()
            .map(|source| (source.file_id, source.path.to_owned()))
            .collect();

        let mut unknown_override_ids: Vec<_> = logical_path_overrides
            .keys()
            .copied()
            .filter(|file_id| !physical_paths.contains_key(file_id))
            .collect();
        unknown_override_ids.sort_by_key(|file_id| file_id.index());
        if !unknown_override_ids.is_empty() {
            return Err(invalid_input(format!(
                "logical path overrides contain unknown file IDs: {}",
                display_file_ids(&unknown_override_ids)
            )));
        }

        let logical_paths = physical_paths
            .iter()
            .map(|(&file_id, physical_path)| {
                let logical_path = logical_path_overrides
                    .get(&file_id)
                    .cloned()
                    .unwrap_or_else(|| physical_path.clone());
                (file_id, logical_path)
            })
            .collect();

        Self::new(root_file_id, physical_paths, logical_paths)
    }

    /// The explicitly designated semantic root.
    #[inline]
    pub fn root_file_id(&self) -> FileId {
        self.root_file_id
    }

    /// Number of files described by this metadata.
    #[inline]
    pub fn len(&self) -> usize {
        self.sorted_ids.len()
    }

    /// Whether this descriptor contains no files.
    ///
    /// Validated descriptors are never empty; this conventional companion to
    /// [`Self::len`] therefore always returns `false`.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.sorted_ids.is_empty()
    }

    /// Whether this descriptor contains `file_id`.
    #[inline]
    pub fn contains_file(&self, file_id: FileId) -> bool {
        self.physical_paths.contains_key(&file_id)
    }

    /// File IDs in ascending numeric order.
    pub fn file_ids(&self) -> impl DoubleEndedIterator<Item = FileId> + ExactSizeIterator + '_ {
        self.sorted_ids.iter().copied()
    }

    /// The physical diagnostic/module path for `file_id`.
    pub fn physical_path(&self, file_id: FileId) -> Option<&str> {
        self.physical_paths.get(&file_id).map(String::as_str)
    }

    /// The canonical logical semantic identity for `file_id`.
    ///
    /// Logical paths are lexically normalized once during construction. They
    /// identify modules within this descriptor, but do not identify source
    /// contents, snapshots, or interner epochs and are not standalone cache
    /// keys.
    pub fn logical_path(&self, file_id: FileId) -> Option<&str> {
        self.logical_paths.get(&file_id).map(String::as_str)
    }

    /// Canonical logical module identity for a request-local file ID.
    pub fn module_id(&self, file_id: FileId) -> Option<ModuleId> {
        self.logical_path(file_id)
            .map(ModuleId::from_validated_canonical)
    }

    /// Canonical logical identity of the explicitly designated root module.
    pub fn root_module_id(&self) -> ModuleId {
        self.module_id(self.root_file_id)
            .expect("validated root file has a logical path")
    }

    /// Physical paths in ascending `FileId` order.
    pub fn physical_paths(
        &self,
    ) -> impl DoubleEndedIterator<Item = (FileId, &str)> + ExactSizeIterator + '_ {
        self.sorted_ids.iter().map(|&file_id| {
            let path = self
                .physical_paths
                .get(&file_id)
                .expect("validated physical path key");
            (file_id, path.as_str())
        })
    }

    /// Logical paths in ascending `FileId` order.
    pub fn logical_paths(
        &self,
    ) -> impl DoubleEndedIterator<Item = (FileId, &str)> + ExactSizeIterator + '_ {
        self.sorted_ids.iter().map(|&file_id| {
            let path = self
                .logical_paths
                .get(&file_id)
                .expect("validated logical path key");
            (file_id, path.as_str())
        })
    }

    /// The complete physical path map.
    ///
    /// The map is immutable but its iteration order is unspecified. Use
    /// [`Self::physical_paths`] when deterministic iteration matters.
    pub fn physical_path_map(&self) -> &HashMap<FileId, String> {
        &self.physical_paths
    }

    /// The complete logical path map.
    ///
    /// The map is immutable but its iteration order is unspecified. Use
    /// [`Self::logical_paths`] when deterministic iteration matters.
    pub fn logical_path_map(&self) -> &HashMap<FileId, String> {
        &self.logical_paths
    }

    #[cfg(test)]
    pub(crate) fn validate_sources(&self, sources: &[SourceView<'_>]) -> CompileResult<()> {
        let mut counts = HashMap::<FileId, usize>::new();
        for source in sources {
            *counts.entry(source.file_id).or_default() += 1;
        }

        let mut duplicate_ids: Vec<_> = counts
            .iter()
            .filter_map(|(&file_id, &count)| (count > 1).then_some(file_id))
            .collect();
        duplicate_ids.sort_by_key(|file_id| file_id.index());
        if !duplicate_ids.is_empty() {
            return Err(invalid_input(format!(
                "source files contain duplicate file IDs: {}",
                display_file_ids(&duplicate_ids)
            )));
        }

        let seen: HashSet<_> = counts.keys().copied().collect();
        let mut unknown_ids: Vec<_> = seen
            .iter()
            .copied()
            .filter(|&file_id| !self.contains_file(file_id))
            .collect();
        unknown_ids.sort_by_key(|file_id| file_id.index());
        if !unknown_ids.is_empty() {
            return Err(invalid_input(format!(
                "source files contain unknown file IDs: {}",
                display_file_ids(&unknown_ids)
            )));
        }

        let mut mismatched_paths: Vec<_> = sources
            .iter()
            .filter_map(|source| {
                let expected_path = self
                    .physical_path(source.file_id)
                    .expect("unknown source IDs rejected above");
                (source.path != expected_path).then_some((
                    source.file_id,
                    expected_path,
                    source.path,
                ))
            })
            .collect();
        mismatched_paths.sort_by_key(|(file_id, _, _)| file_id.index());
        if let Some((file_id, expected_path, actual_path)) = mismatched_paths.first() {
            return Err(invalid_input(format!(
                "physical path for {} is {:?}, but source file uses {:?}",
                display_file_id(*file_id),
                expected_path,
                actual_path
            )));
        }

        let missing_ids: Vec<_> = self
            .file_ids()
            .filter(|file_id| !seen.contains(file_id))
            .collect();
        if !missing_ids.is_empty() {
            return Err(invalid_input(format!(
                "source files are missing metadata file IDs: {}",
                display_file_ids(&missing_ids)
            )));
        }

        Ok(())
    }
}

fn validate_nonempty_paths(
    kind: &str,
    file_ids: &[FileId],
    paths: &HashMap<FileId, String>,
) -> CompileResult<()> {
    for &file_id in file_ids {
        let path = paths.get(&file_id).expect("validated path key");
        if path.is_empty() || normalize_module_path(path).is_empty() {
            return Err(invalid_input(format!(
                "{kind} path for {} has an empty identity",
                display_file_id(file_id)
            )));
        }
    }
    Ok(())
}

fn validate_normalized_collisions(
    kind: &str,
    file_ids: &[FileId],
    paths: &HashMap<FileId, String>,
) -> CompileResult<()> {
    let mut seen = HashMap::<String, FileId>::new();
    let mut collisions = Vec::new();
    for &file_id in file_ids {
        let normalized = normalize_module_path(paths.get(&file_id).expect("validated path key"));
        if let Some(&first_file_id) = seen.get(&normalized) {
            collisions.push((first_file_id, file_id, normalized));
        } else {
            seen.insert(normalized, file_id);
        }
    }

    collisions.sort_by_key(|(first, second, _)| (first.index(), second.index()));
    if let Some((first, second, normalized)) = collisions.into_iter().next() {
        return Err(invalid_input(format!(
            "{kind} paths for {} and {} normalize to the same identity {:?}",
            display_file_id(first),
            display_file_id(second),
            normalized
        )));
    }

    Ok(())
}

fn invalid_input(message: impl Into<String>) -> CompileError {
    CompileError::without_span(ErrorKind::InvalidCompilerInput(message.into()))
}

fn display_file_id(file_id: FileId) -> u32 {
    file_id.index()
}

fn display_file_ids(file_ids: &[FileId]) -> String {
    file_ids
        .iter()
        .map(|file_id| file_id.index().to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source<'a>(path: &'a str, file_id: u32) -> SourceView<'a> {
        SourceView::new(path, "", FileId::new(file_id))
    }

    fn physical(entries: &[(u32, &str)]) -> HashMap<FileId, String> {
        entries
            .iter()
            .map(|&(file_id, path)| (FileId::new(file_id), path.to_owned()))
            .collect()
    }

    fn error_message(result: CompileResult<SourceMetadata>) -> String {
        result.unwrap_err().to_string()
    }

    #[test]
    fn materializes_partial_logical_paths_and_preserves_explicit_root() {
        let sources = [source("src/root.rue", 20), source("src/util.rue", 10)];
        let metadata = SourceMetadata::from_sources(
            &sources,
            FileId::new(20),
            physical(&[(10, "stable/util.rue")]),
        )
        .unwrap();

        assert_eq!(metadata.root_file_id(), FileId::new(20));
        assert_eq!(
            metadata.file_ids().collect::<Vec<_>>(),
            [FileId::new(10), FileId::new(20)]
        );
        assert_eq!(
            metadata.physical_path(FileId::new(10)),
            Some("src/util.rue")
        );
        assert_eq!(
            metadata.logical_path(FileId::new(10)),
            Some("stable/util.rue")
        );
        assert_eq!(metadata.logical_path(FileId::new(20)), Some("src/root.rue"));
        assert_eq!(metadata.physical_path_map().len(), 2);
        assert_eq!(metadata.logical_path_map().len(), 2);
    }

    #[test]
    fn canonicalizes_logical_paths_but_preserves_physical_spellings() {
        let physical_paths = physical(&[(1, "pkg/a/../main.rue")]);
        let metadata = SourceMetadata::new(
            FileId::new(1),
            physical_paths.clone(),
            physical(&[(1, "pkg/a/../main.rue")]),
        )
        .unwrap();
        let canonical = SourceMetadata::new(
            FileId::new(1),
            physical_paths,
            physical(&[(1, "pkg/main.rue")]),
        )
        .unwrap();

        assert_eq!(
            metadata.physical_path(FileId::new(1)),
            Some("pkg/a/../main.rue")
        );
        assert_eq!(
            metadata.logical_path(FileId::new(1)),
            Some(normalize_module_path("pkg/main.rue").as_str())
        );
        assert_eq!(metadata, canonical);
    }

    #[test]
    fn preserves_leading_parent_and_std_sentinel_logical_identities() {
        let metadata = SourceMetadata::new(
            FileId::new(1),
            physical(&[(1, "dep.rue"), (2, "std.rue"), (3, "project.rue")]),
            physical(&[
                (1, "a/../../dep.rue"),
                (2, "\0rue-std/math/./float.rue"),
                (3, "@rue-std/math/float.rue"),
            ]),
        )
        .unwrap();

        assert_eq!(
            metadata.logical_path(FileId::new(1)),
            Some(normalize_module_path("../dep.rue").as_str())
        );
        assert_eq!(
            metadata.logical_path(FileId::new(2)),
            Some(normalize_module_path("\0rue-std/math/float.rue").as_str())
        );
        assert_ne!(
            metadata.logical_path(FileId::new(2)),
            metadata.logical_path(FileId::new(3))
        );
    }

    #[test]
    fn rejects_empty_input() {
        assert_eq!(
            error_message(SourceMetadata::new(
                FileId::new(1),
                HashMap::new(),
                HashMap::new()
            )),
            "invalid compiler input: source metadata contains no files"
        );
    }

    #[test]
    fn rejects_duplicate_source_ids_before_collection() {
        let sources = [
            source("z.rue", 9),
            source("b.rue", 2),
            source("a.rue", 2),
            source("y.rue", 9),
        ];
        assert_eq!(
            error_message(SourceMetadata::from_sources(
                &sources,
                FileId::new(2),
                HashMap::new()
            )),
            "invalid compiler input: source files contain duplicate file IDs: 2, 9"
        );
    }

    #[test]
    fn permits_default_id_for_compatibility() {
        let metadata = SourceMetadata::new(
            FileId::DEFAULT,
            physical(&[(0, "main.rue"), (1, "util.rue")]),
            physical(&[(0, "main.rue"), (1, "util.rue")]),
        )
        .unwrap();
        assert_eq!(metadata.root_file_id(), FileId::DEFAULT);
    }

    #[test]
    fn rejects_missing_root() {
        assert_eq!(
            error_message(SourceMetadata::new(
                FileId::new(7),
                physical(&[(2, "main.rue")]),
                physical(&[(2, "main.rue")]),
            )),
            "invalid compiler input: root file ID 7 is absent from source metadata"
        );
    }

    #[test]
    fn rejects_mismatched_raw_map_keys_deterministically() {
        assert_eq!(
            error_message(SourceMetadata::new(
                FileId::new(1),
                physical(&[(4, "four.rue"), (1, "one.rue"), (2, "two.rue")]),
                physical(&[(1, "one.rue"), (7, "seven.rue")]),
            )),
            "invalid compiler input: logical path map is missing file IDs: 2, 4"
        );

        assert_eq!(
            error_message(SourceMetadata::new(
                FileId::new(1),
                physical(&[(1, "one.rue")]),
                physical(&[(1, "one.rue"), (8, "eight.rue"), (3, "three.rue")]),
            )),
            "invalid compiler input: logical path map contains unknown file IDs: 3, 8"
        );
    }

    #[test]
    fn rejects_unknown_logical_override_ids() {
        assert_eq!(
            error_message(SourceMetadata::from_sources(
                &[source("main.rue", 1)],
                FileId::new(1),
                physical(&[(9, "nine.rue"), (4, "four.rue")]),
            )),
            "invalid compiler input: logical path overrides contain unknown file IDs: 4, 9"
        );
    }

    #[test]
    fn rejects_raw_and_normalized_empty_identities() {
        assert_eq!(
            error_message(SourceMetadata::new(
                FileId::new(1),
                physical(&[(1, "")]),
                physical(&[(1, "main.rue")]),
            )),
            "invalid compiler input: physical path for 1 has an empty identity"
        );
        assert_eq!(
            error_message(SourceMetadata::new(
                FileId::new(1),
                physical(&[(1, "main.rue")]),
                physical(&[(1, "./")]),
            )),
            "invalid compiler input: logical path for 1 has an empty identity"
        );
    }

    #[test]
    fn rejects_physical_and_logical_normalization_collisions_separately() {
        assert_eq!(
            error_message(SourceMetadata::new(
                FileId::new(5),
                physical(&[(5, "src/./main.rue"), (8, "src/main.rue")]),
                physical(&[(5, "main.rue"), (8, "util.rue")]),
            )),
            "invalid compiler input: physical paths for 5 and 8 normalize to the same identity \"src/main.rue\""
        );

        assert_eq!(
            error_message(SourceMetadata::new(
                FileId::new(5),
                physical(&[(5, "src/main.rue"), (8, "src/util.rue")]),
                physical(&[(5, "pkg/a/../main.rue"), (8, "pkg/main.rue")]),
            )),
            "invalid compiler input: logical paths for 5 and 8 normalize to the same identity \"pkg/main.rue\""
        );
    }

    #[test]
    fn normalized_collision_selection_is_file_id_ordered() {
        let result = SourceMetadata::new(
            FileId::new(1),
            physical(&[
                (9, "z/../same.rue"),
                (7, "same.rue"),
                (2, "a/../other.rue"),
                (1, "other.rue"),
            ]),
            physical(&[(1, "one"), (2, "two"), (7, "seven"), (9, "nine")]),
        );
        assert_eq!(
            error_message(result),
            "invalid compiler input: physical paths for 1 and 2 normalize to the same identity \"other.rue\""
        );
    }

    #[test]
    fn validates_source_ids_and_physical_paths() {
        let metadata = SourceMetadata::new(
            FileId::new(2),
            physical(&[(2, "root.rue"), (8, "util.rue")]),
            physical(&[(2, "root"), (8, "util")]),
        )
        .unwrap();

        metadata
            .validate_sources(&[source("util.rue", 8), source("root.rue", 2)])
            .unwrap();

        let duplicate = metadata
            .validate_sources(&[source("root.rue", 2), source("root.rue", 2)])
            .unwrap_err();
        assert_eq!(
            duplicate.to_string(),
            "invalid compiler input: source files contain duplicate file IDs: 2"
        );

        let unknown = metadata
            .validate_sources(&[source("root.rue", 2), source("other.rue", 3)])
            .unwrap_err();
        assert_eq!(
            unknown.to_string(),
            "invalid compiler input: source files contain unknown file IDs: 3"
        );

        let wrong_path = metadata
            .validate_sources(&[source("wrong.rue", 2), source("util.rue", 8)])
            .unwrap_err();
        assert_eq!(
            wrong_path.to_string(),
            "invalid compiler input: physical path for 2 is \"root.rue\", but source file uses \"wrong.rue\""
        );

        let missing = metadata
            .validate_sources(&[source("root.rue", 2)])
            .unwrap_err();
        assert_eq!(
            missing.to_string(),
            "invalid compiler input: source files are missing metadata file IDs: 8"
        );
    }

    #[test]
    fn source_validation_is_independent_of_vector_order() {
        let metadata = SourceMetadata::new(
            FileId::new(2),
            physical(&[(2, "root.rue"), (8, "util.rue")]),
            physical(&[(2, "root"), (8, "util")]),
        )
        .unwrap();

        let first = metadata
            .validate_sources(&[source("wrong-util.rue", 8), source("wrong-root.rue", 2)])
            .unwrap_err();
        let second = metadata
            .validate_sources(&[source("wrong-root.rue", 2), source("wrong-util.rue", 8)])
            .unwrap_err();

        assert_eq!(first.to_string(), second.to_string());
        assert_eq!(
            first.to_string(),
            "invalid compiler input: physical path for 2 is \"root.rue\", but source file uses \"wrong-root.rue\""
        );
    }
}
