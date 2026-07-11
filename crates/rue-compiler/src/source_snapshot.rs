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

use crate::{SourceFile, SourceMetadata};

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
    contents: Vec<(FileId, Arc<String>)>,
    index: HashMap<FileId, usize>,
}

impl SourceSnapshot {
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

        let index = contents
            .iter()
            .enumerate()
            .map(|(index, (file_id, _))| (*file_id, index))
            .collect();

        Ok(Self {
            data: Arc::new(SourceSnapshotData {
                metadata,
                contents,
                index,
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
        self.content(file_id).cloned()
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
        let index = *self.data.index.get(&file_id)?;
        let (stored_file_id, source) = &self.data.contents[index];
        debug_assert_eq!(*stored_file_id, file_id);
        Some(source)
    }

    fn file_at(&self, index: usize) -> SourceFile<'_> {
        let (file_id, source) = &self.data.contents[index];
        let path = self
            .data
            .metadata
            .physical_path(*file_id)
            .expect("snapshot contents validated against metadata");
        SourceFile::new(path, source.as_str(), *file_id)
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
