//! Immutable, owned source text at compiler API boundaries.
//!
//! [`SourceMetadata`] owns source identities and paths, while this module owns
//! the corresponding text. Keeping those responsibilities separate lets a
//! snapshot cheaply share source buffers without duplicating or allowing path
//! information to drift.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use rue_error::{CompileError, CompileResult, ErrorKind};
use rue_span::FileId;

use crate::{
    ModuleId, ModuleRevision, SourceFile, SourceId, SourceMetadata, SourceRevision, SourceStore,
};

/// Maximum source byte length representable by Rue's `u32` span offsets.
pub const MAX_SOURCE_BYTES: usize = u32::MAX as usize;

/// An immutable, validated set of source texts and their metadata.
///
/// Cloning a snapshot is constant time: clones share both the descriptor and
/// source table through a single [`Arc`]. Source text can also be retained
/// independently through [`Self::shared_source_text`]. A [`FileId`] denotes
/// diagnostic membership within the snapshot; it does not identify content or
/// form a standalone cache key.
#[derive(Debug, Clone)]
pub struct SourceSnapshot {
    data: Arc<SourceSnapshotData>,
}

#[derive(Debug)]
struct SourceSnapshotData {
    metadata: SourceMetadata,
    contents: Vec<SourceRecord>,
    index: HashMap<FileId, usize>,
    revision: SourceRevision,
    source_store: SourceStore,
}

#[derive(Debug)]
struct SourceRecord {
    file_id: FileId,
    module_id: ModuleId,
    source_id: SourceId,
    text: Arc<String>,
}

impl SourceSnapshot {
    /// Reassemble diagnostic source records from already-validated parsed
    /// artifacts without hashing source bytes again.
    pub(crate) fn from_parsed_modules(
        program: &crate::parsed_modules::ParsedProgram,
    ) -> CompileResult<Self> {
        let root_file_id = program
            .modules()
            .iter()
            .find(|module| module.module_id() == program.root())
            .expect("parsed program validates its root")
            .file_id();
        let physical_paths = program
            .modules()
            .iter()
            .map(|module| (module.file_id(), module.physical_path().to_owned()))
            .collect();
        let logical_paths = program
            .modules()
            .iter()
            .map(|module| (module.file_id(), module.module_id().as_str().to_owned()))
            .collect();
        let metadata = SourceMetadata::new(root_file_id, physical_paths, logical_paths)?;
        validate_source_lengths(
            program
                .modules()
                .iter()
                .map(|module| (module.file_id(), module.source_text().len())),
            &metadata,
        )?;

        let source_store = SourceStore::from_ids(
            program
                .modules()
                .iter()
                .map(|module| module.source_id().clone()),
        );
        let contents: Vec<_> = program
            .modules()
            .iter()
            .map(|module| {
                let module_text = module.shared_source_text();
                if !Arc::ptr_eq(&module_text, &module.source_id().shared_text()) {
                    return Err(invalid_input(format!(
                        "parsed module {} source text has foreign identity provenance",
                        module.module_id()
                    )));
                }
                let source_id = source_store
                    .get(module.source_id())
                    .expect("store was built from every parsed module source")
                    .clone();
                let text = source_store
                    .shared_text(&source_id)
                    .expect("canonical store identity retains exact source text");
                Ok(SourceRecord {
                    file_id: module.file_id(),
                    module_id: module.module_id().clone(),
                    source_id,
                    text,
                })
            })
            .collect::<CompileResult<_>>()?;
        let index = contents
            .iter()
            .enumerate()
            .map(|(index, record)| (record.file_id, index))
            .collect();
        let revision = program.source_revision().clone();

        Ok(Self {
            data: Arc::new(SourceSnapshotData {
                metadata,
                contents,
                index,
                revision,
                source_store,
            }),
        })
    }

    /// Build a snapshot from validated metadata and owned source buffers.
    ///
    /// `contents` may be in any order, and that caller-supplied order is
    /// retained by [`Self::files`]. Its `FileId` set must exactly match the
    /// metadata descriptor. Validation errors are selected in ascending
    /// `FileId` order and are reported before any frontend work begins.
    pub fn new(
        metadata: SourceMetadata,
        contents: Vec<(FileId, Arc<String>)>,
    ) -> CompileResult<Self> {
        let mut counts = HashMap::<FileId, usize>::new();
        for (file_id, _) in &contents {
            *counts.entry(*file_id).or_default() += 1;
        }

        let mut duplicate_ids: Vec<_> = counts
            .iter()
            .filter_map(|(&file_id, &count)| (count > 1).then_some(file_id))
            .collect();
        duplicate_ids.sort_by_key(|file_id| file_id.index());
        if !duplicate_ids.is_empty() {
            return Err(invalid_input(format!(
                "source contents contain duplicate file IDs: {}",
                display_file_ids(&duplicate_ids)
            )));
        }

        let seen: HashSet<_> = counts.keys().copied().collect();
        let mut unknown_ids: Vec<_> = seen
            .iter()
            .copied()
            .filter(|&file_id| !metadata.contains_file(file_id))
            .collect();
        unknown_ids.sort_by_key(|file_id| file_id.index());
        if !unknown_ids.is_empty() {
            return Err(invalid_input(format!(
                "source contents contain unknown file IDs: {}",
                display_file_ids(&unknown_ids)
            )));
        }

        let missing_ids: Vec<_> = metadata
            .file_ids()
            .filter(|file_id| !seen.contains(file_id))
            .collect();
        if !missing_ids.is_empty() {
            return Err(invalid_input(format!(
                "source contents are missing metadata file IDs: {}",
                display_file_ids(&missing_ids)
            )));
        }

        validate_source_lengths(
            contents
                .iter()
                .map(|(file_id, source)| (*file_id, source.len())),
            &metadata,
        )?;

        let candidates: Vec<_> = contents
            .into_iter()
            .map(|(file_id, text)| {
                let module_id = ModuleId::from_validated_canonical(
                    metadata
                        .logical_path(file_id)
                        .expect("validated membership"),
                );
                let source_id = SourceId::from_shared_text(text);
                (file_id, module_id, source_id)
            })
            .collect();
        let mut store_candidates: Vec<_> = candidates
            .iter()
            .map(|(_, module_id, source_id)| (module_id, source_id))
            .collect();
        store_candidates.sort_by(|(left, _), (right, _)| left.cmp(right));
        let source_store = SourceStore::from_ids(
            store_candidates
                .into_iter()
                .map(|(_, source)| source.clone()),
        );
        let contents: Vec<_> = candidates
            .into_iter()
            .map(|(file_id, module_id, requested_id)| {
                let source_id = source_store
                    .get(&requested_id)
                    .expect("store was built from every snapshot source")
                    .clone();
                let text = source_store
                    .shared_text(&source_id)
                    .expect("canonical store identity retains exact source text");
                SourceRecord {
                    file_id,
                    module_id,
                    source_id,
                    text,
                }
            })
            .collect();
        let index = contents
            .iter()
            .enumerate()
            .map(|(index, record)| (record.file_id, index))
            .collect();
        let root_module = ModuleId::from_validated_canonical(
            metadata
                .logical_path(metadata.root_file_id())
                .expect("validated root"),
        );
        let revision = SourceRevision::new(
            root_module,
            contents
                .iter()
                .map(|record| ModuleRevision {
                    module: record.module_id.clone(),
                    source: record.source_id.clone(),
                })
                .collect(),
        )?;

        Ok(Self {
            data: Arc::new(SourceSnapshotData {
                metadata,
                contents,
                index,
                revision,
                source_store,
            }),
        })
    }

    /// Copy borrowed compatibility inputs into one immutable snapshot.
    ///
    /// Physical paths are validated against `metadata` before source text is
    /// copied. Each source is then copied directly into its final owned
    /// [`String`] buffer.
    pub fn from_sources(
        sources: &[SourceFile<'_>],
        metadata: SourceMetadata,
    ) -> CompileResult<Self> {
        metadata.validate_sources(sources)?;
        validate_source_lengths(
            sources
                .iter()
                .map(|source| (source.file_id, source.source.len())),
            &metadata,
        )?;
        let contents = sources
            .iter()
            .map(|source| (source.file_id, Arc::new(source.source.to_owned())))
            .collect();
        Self::new(metadata, contents)
    }

    /// The validated identities and paths for this snapshot.
    #[inline]
    pub fn metadata(&self) -> &SourceMetadata {
        &self.data.metadata
    }

    /// Number of source files in this snapshot.
    #[inline]
    pub fn len(&self) -> usize {
        self.data.contents.len()
    }

    /// Whether this snapshot contains no source files.
    ///
    /// Validated metadata is never empty, so a constructed snapshot always
    /// returns `false`.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.data.contents.is_empty()
    }

    /// Borrow the source text for `file_id`.
    pub fn source_text(&self, file_id: FileId) -> Option<&str> {
        self.content(file_id).map(|source| source.as_str())
    }

    /// Share ownership of the source text for `file_id`.
    pub fn shared_source_text(&self, file_id: FileId) -> Option<Arc<String>> {
        self.record(file_id).map(|record| record.text.clone())
    }

    /// Stable exact content identity for a request-local file.
    pub fn source_id(&self, file_id: FileId) -> Option<&SourceId> {
        self.record(file_id).map(|record| &record.source_id)
    }

    /// Canonical logical module identity for a request-local file.
    pub fn module_id(&self, file_id: FileId) -> Option<&ModuleId> {
        self.record(file_id).map(|record| &record.module_id)
    }

    /// Load-order-independent root and module/content mapping.
    pub fn source_revision(&self) -> &SourceRevision {
        &self.data.revision
    }

    /// Snapshot-owned exact source storage shared by all equal source texts.
    pub fn source_store(&self) -> &SourceStore {
        &self.data.source_store
    }

    /// Borrow one source as a compatibility [`SourceFile`] view.
    pub fn source_file(&self, file_id: FileId) -> Option<SourceFile<'_>> {
        let index = *self.data.index.get(&file_id)?;
        Some(self.file_at(index))
    }

    /// Borrow all sources in caller-supplied order.
    pub fn files(
        &self,
    ) -> impl DoubleEndedIterator<Item = SourceFile<'_>> + ExactSizeIterator + '_ {
        (0..self.data.contents.len()).map(|index| self.file_at(index))
    }

    fn content(&self, file_id: FileId) -> Option<&Arc<String>> {
        self.record(file_id).map(|record| &record.text)
    }

    fn record(&self, file_id: FileId) -> Option<&SourceRecord> {
        let record = &self.data.contents[*self.data.index.get(&file_id)?];
        debug_assert_eq!(record.file_id, file_id);
        Some(record)
    }

    fn file_at(&self, index: usize) -> SourceFile<'_> {
        let record = &self.data.contents[index];
        let file_id = record.file_id;
        let path = self
            .data
            .metadata
            .physical_path(file_id)
            .expect("snapshot contents validated against metadata");
        SourceFile::new(path, record.text.as_str(), file_id)
    }
}

fn validate_source_len(file_id: FileId, path: &str, len: usize) -> CompileResult<()> {
    if len > MAX_SOURCE_BYTES {
        return Err(invalid_input(format!(
            "source text for file ID {} ({path:?}) is {len} bytes, exceeding the maximum supported length of {} bytes",
            file_id.index(),
            MAX_SOURCE_BYTES
        )));
    }
    Ok(())
}

fn validate_source_lengths(
    lengths: impl IntoIterator<Item = (FileId, usize)>,
    metadata: &SourceMetadata,
) -> CompileResult<()> {
    let mut lengths: Vec<_> = lengths.into_iter().collect();
    lengths.sort_by_key(|(file_id, _)| file_id.index());
    for (file_id, len) in lengths {
        let path = metadata
            .physical_path(file_id)
            .expect("source length IDs validated against metadata");
        validate_source_len(file_id, path, len)?;
    }
    Ok(())
}

fn invalid_input(message: impl Into<String>) -> CompileError {
    CompileError::without_span(ErrorKind::InvalidCompilerInput(message.into()))
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

    fn metadata(entries: &[(u32, &str)]) -> SourceMetadata {
        let paths: HashMap<_, _> = entries
            .iter()
            .map(|&(file_id, path)| (FileId::new(file_id), path.to_owned()))
            .collect();
        SourceMetadata::new(FileId::new(entries[0].0), paths.clone(), paths).unwrap()
    }

    fn contents(entries: &[(u32, &str)]) -> Vec<(FileId, Arc<String>)> {
        entries
            .iter()
            .map(|&(file_id, source)| (FileId::new(file_id), Arc::new(source.to_owned())))
            .collect()
    }

    fn error_message(result: CompileResult<SourceSnapshot>) -> String {
        result.unwrap_err().to_string()
    }

    #[test]
    fn requires_exact_metadata_membership() {
        let descriptor = metadata(&[(2, "two.rue"), (4, "four.rue"), (8, "eight.rue")]);
        assert_eq!(
            error_message(SourceSnapshot::new(
                descriptor.clone(),
                contents(&[(8, "eight"), (2, "two"), (8, "duplicate"), (2, "again")])
            )),
            "invalid compiler input: source contents contain duplicate file IDs: 2, 8"
        );
        assert_eq!(
            error_message(SourceSnapshot::new(
                descriptor.clone(),
                contents(&[(9, "nine"), (7, "seven"), (2, "two")])
            )),
            "invalid compiler input: source contents contain unknown file IDs: 7, 9"
        );
        assert_eq!(
            error_message(SourceSnapshot::new(descriptor, contents(&[(8, "eight")]))),
            "invalid compiler input: source contents are missing metadata file IDs: 2, 4"
        );
    }

    #[test]
    fn validation_diagnostics_do_not_depend_on_caller_order() {
        let first = error_message(SourceSnapshot::new(
            metadata(&[(1, "one.rue")]),
            contents(&[(9, "nine"), (4, "four")]),
        ));
        let second = error_message(SourceSnapshot::new(
            metadata(&[(1, "one.rue")]),
            contents(&[(4, "four"), (9, "nine")]),
        ));

        assert_eq!(first, second);
        assert_eq!(
            first,
            "invalid compiler input: source contents contain unknown file IDs: 4, 9"
        );
    }

    #[test]
    fn preserves_caller_order_for_borrowed_file_views() {
        let snapshot = SourceSnapshot::new(
            metadata(&[(10, "ten.rue"), (20, "twenty.rue"), (30, "thirty.rue")]),
            contents(&[(30, "thirty"), (10, "ten"), (20, "twenty")]),
        )
        .unwrap();

        assert_eq!(snapshot.len(), 3);
        assert!(!snapshot.is_empty());
        assert_eq!(
            snapshot
                .files()
                .map(|file| (file.file_id, file.path, file.source))
                .collect::<Vec<_>>(),
            [
                (FileId::new(30), "thirty.rue", "thirty"),
                (FileId::new(10), "ten.rue", "ten"),
                (FileId::new(20), "twenty.rue", "twenty"),
            ]
        );
        let first_from_back = snapshot.files().next_back().unwrap();
        assert_eq!(first_from_back.file_id, FileId::new(20));
    }

    #[test]
    fn indexed_access_uses_metadata_paths_and_shares_source_arcs() {
        let source = Arc::new("fn main() {}".to_owned());
        let snapshot = SourceSnapshot::new(
            metadata(&[(7, "src/main.rue")]),
            vec![(FileId::new(7), Arc::clone(&source))],
        )
        .unwrap();

        assert_eq!(snapshot.source_text(FileId::new(7)), Some("fn main() {}"));
        assert_eq!(snapshot.source_text(FileId::new(8)), None);
        let shared = snapshot.shared_source_text(FileId::new(7)).unwrap();
        assert!(Arc::ptr_eq(&source, &shared));
        assert!(snapshot.shared_source_text(FileId::new(8)).is_none());
        let file = snapshot.source_file(FileId::new(7)).unwrap();
        assert_eq!(file.path, "src/main.rue");
        assert_eq!(file.source, "fn main() {}");
        assert!(snapshot.source_file(FileId::new(8)).is_none());
    }

    #[test]
    fn clones_share_snapshot_data_and_external_arc_mutation_is_isolated() {
        let source = Arc::new("original".to_owned());
        let snapshot = SourceSnapshot::new(
            metadata(&[(1, "main.rue")]),
            vec![(FileId::new(1), Arc::clone(&source))],
        )
        .unwrap();
        let clone = snapshot.clone();

        assert!(Arc::ptr_eq(&snapshot.data, &clone.data));

        let mut external = source;
        Arc::make_mut(&mut external).push_str(" changed");
        assert_eq!(external.as_str(), "original changed");
        assert_eq!(snapshot.source_text(FileId::new(1)), Some("original"));
        assert_eq!(clone.source_text(FileId::new(1)), Some("original"));
    }

    #[test]
    fn edited_snapshots_reuse_unchanged_text_without_mutating_old_views() {
        let root = FileId::new(1);
        let helper = FileId::new(2);
        let old_root = Arc::new("fn main() -> i32 { helper() }".to_owned());
        let shared_helper = Arc::new("fn helper() -> i32 { 1 }".to_owned());
        let old = SourceSnapshot::new(
            metadata(&[(1, "/old/main.rue"), (2, "/old/helper.rue")]),
            vec![
                (root, Arc::clone(&old_root)),
                (helper, Arc::clone(&shared_helper)),
            ],
        )
        .unwrap();

        let edited_root = Arc::new("fn main() -> i32 { helper() + 1 }".to_owned());
        let edited = SourceSnapshot::new(
            metadata(&[(1, "/new/main.rue"), (2, "/new/helper.rue")]),
            vec![
                (root, Arc::clone(&edited_root)),
                (helper, old.shared_source_text(helper).unwrap()),
            ],
        )
        .unwrap();

        assert!(Arc::ptr_eq(
            &old.shared_source_text(helper).unwrap(),
            &edited.shared_source_text(helper).unwrap()
        ));
        assert!(!Arc::ptr_eq(
            &old.shared_source_text(root).unwrap(),
            &edited.shared_source_text(root).unwrap()
        ));
        assert_eq!(old.source_text(root), Some("fn main() -> i32 { helper() }"));
        assert_eq!(old.source_file(root).unwrap().path, "/old/main.rue");
        assert_eq!(
            edited.source_text(root),
            Some("fn main() -> i32 { helper() + 1 }")
        );
        assert_eq!(edited.source_file(root).unwrap().path, "/new/main.rue");
    }

    #[test]
    fn source_snapshots_are_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<SourceSnapshot>();
    }

    #[test]
    fn equal_module_sources_share_one_snapshot_store_payload() {
        let left = Arc::new(String::from("fn shared() {}"));
        let right = Arc::new(String::from("fn shared() {}"));
        assert!(!Arc::ptr_eq(&left, &right));
        let snapshot = SourceSnapshot::new(
            metadata(&[(20, "left.rue"), (1, "right.rue")]),
            vec![(FileId::new(1), right), (FileId::new(20), left.clone())],
        )
        .unwrap();

        let left_stored = snapshot.shared_source_text(FileId::new(20)).unwrap();
        let right_stored = snapshot.shared_source_text(FileId::new(1)).unwrap();
        assert_eq!(snapshot.source_store().len(), 1);
        assert!(Arc::ptr_eq(&left_stored, &right_stored));
        assert!(Arc::ptr_eq(&left_stored, &left));
        assert_eq!(
            snapshot.source_id(FileId::new(20)),
            snapshot.source_id(FileId::new(1))
        );
    }

    #[test]
    fn revisions_ignore_file_ids_physical_roots_and_input_order() {
        let make = |root_id, helper_id, physical_root: &str, reversed: bool| {
            let physical = HashMap::from([
                (FileId::new(root_id), physical_root.to_owned()),
                (
                    FileId::new(helper_id),
                    format!("{physical_root}/../helper.rue"),
                ),
            ]);
            let logical = HashMap::from([
                (FileId::new(root_id), "app/main.rue".to_owned()),
                (FileId::new(helper_id), "app/helper.rue".to_owned()),
            ]);
            let metadata = SourceMetadata::new(FileId::new(root_id), physical, logical).unwrap();
            let mut contents = vec![
                (FileId::new(root_id), Arc::new("fn main() {}".to_owned())),
                (
                    FileId::new(helper_id),
                    Arc::new("fn helper() {}".to_owned()),
                ),
            ];
            if reversed {
                contents.reverse();
            }
            SourceSnapshot::new(metadata, contents).unwrap()
        };
        let first = make(9, 2, "/one/main.rue", false);
        let second = make(100, 7, "/relocated/main.rue", true);
        assert_eq!(first.source_revision(), second.source_revision());
        assert_eq!(
            first
                .source_revision()
                .modules()
                .iter()
                .map(|m| m.module.as_str())
                .collect::<Vec<_>>(),
            ["app/helper.rue", "app/main.rue"]
        );
    }

    #[test]
    fn edits_and_module_renames_change_only_their_respective_identities() {
        let original = SourceSnapshot::new(
            metadata(&[(1, "main.rue")]),
            contents(&[(1, "fn main() {}")]),
        )
        .unwrap();
        let edited = SourceSnapshot::new(
            metadata(&[(1, "main.rue")]),
            contents(&[(1, "fn main() { let x = 1; }")]),
        )
        .unwrap();
        let renamed = SourceSnapshot::new(
            metadata(&[(1, "renamed.rue")]),
            contents(&[(1, "fn main() {}")]),
        )
        .unwrap();
        let id = FileId::new(1);
        assert_eq!(original.module_id(id), edited.module_id(id));
        assert_ne!(original.source_id(id), edited.source_id(id));
        assert_ne!(original.module_id(id), renamed.module_id(id));
        assert_eq!(original.source_id(id), renamed.source_id(id));
        assert!(Arc::ptr_eq(
            &original.shared_source_text(id).unwrap(),
            &original.source_id(id).unwrap().shared_text(),
        ));
    }

    #[test]
    fn compatibility_adapter_copies_borrowed_text_and_validates_paths() {
        let mut borrowed = "borrowed".to_owned();
        let descriptor = metadata(&[(3, "main.rue")]);
        let snapshot = SourceSnapshot::from_sources(
            &[SourceFile::new("main.rue", &borrowed, FileId::new(3))],
            descriptor.clone(),
        )
        .unwrap();

        borrowed.push_str(" changed");
        assert_eq!(snapshot.source_text(FileId::new(3)), Some("borrowed"));

        let wrong_path = [SourceFile::new("other.rue", "source", FileId::new(3))];
        assert_eq!(
            error_message(SourceSnapshot::from_sources(&wrong_path, descriptor)),
            "invalid compiler input: physical path for 3 is \"main.rue\", but source file uses \"other.rue\""
        );
    }

    #[test]
    fn moving_a_string_into_a_snapshot_preserves_its_buffer() {
        let source = String::from("a source buffer long enough to make allocation observable");
        let buffer = source.as_ptr();
        let snapshot = SourceSnapshot::new(
            metadata(&[(5, "main.rue")]),
            vec![(FileId::new(5), Arc::new(source))],
        )
        .unwrap();

        assert_eq!(
            snapshot.source_text(FileId::new(5)).unwrap().as_ptr(),
            buffer
        );
    }

    #[cfg(target_pointer_width = "64")]
    #[test]
    fn rejects_source_lengths_that_spans_cannot_represent() {
        assert_eq!(
            validate_source_len(FileId::new(12), "large.rue", u32::MAX as usize + 1)
                .unwrap_err()
                .to_string(),
            "invalid compiler input: source text for file ID 12 (\"large.rue\") is 4294967296 bytes, exceeding the maximum supported length of 4294967295 bytes"
        );
    }
}
