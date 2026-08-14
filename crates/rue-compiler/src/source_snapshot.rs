//! Immutable, owned source text at compiler API boundaries.
//!
//! [`SourceMetadata`] owns source identities and paths, while this module owns
//! the corresponding text. Keeping those responsibilities separate lets a
//! snapshot cheaply share source buffers without duplicating or allowing path
//! information to drift.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use rue_error::{CompileError, CompileResult, ErrorKind};
pub use rue_lexer::MAX_SOURCE_BYTES;
use rue_span::FileId;

use crate::{
    ModuleId, ModuleRevision, SourceId, SourceMetadata, SourceRevision, SourceStore, SourceView,
};

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
    /// `Arc`-shared size-tiered record segments, oldest first. Untouched tiers
    /// stay shared; compacted tail tiers copy only record metadata and retain
    /// the same source-text and identity `Arc`s.
    segments: Vec<Arc<SnapshotSegment>>,
    /// Global start index of each segment, aligned with `segments`.
    segment_offsets: Vec<usize>,
    len: usize,
    revision: SourceRevision,
    source_store: SourceStore,
}

#[derive(Debug)]
struct SnapshotSegment {
    contents: Vec<SourceRecord>,
    /// Position within THIS segment for each of its file ids.
    index: HashMap<FileId, usize>,
    min_file_index: u32,
    max_file_index: u32,
}

impl SnapshotSegment {
    fn from_records(contents: Vec<SourceRecord>) -> Self {
        let min_file_index = contents
            .iter()
            .map(|record| record.file_id.index())
            .min()
            .expect("snapshot segments are nonempty");
        let max_file_index = contents
            .iter()
            .map(|record| record.file_id.index())
            .max()
            .expect("snapshot segments are nonempty");
        let index = contents
            .iter()
            .enumerate()
            .map(|(index, record)| (record.file_id, index))
            .collect();
        Self {
            contents,
            index,
            min_file_index,
            max_file_index,
        }
    }

    fn merge(left: &Self, right: &Self) -> Self {
        assert!(left.max_file_index < right.min_file_index);
        Self::from_records(
            left.contents
                .iter()
                .chain(right.contents.iter())
                .cloned()
                .collect(),
        )
    }
}

#[derive(Debug, Clone)]
struct SourceRecord {
    file_id: FileId,
    module_id: ModuleId,
    source_id: SourceId,
    text: Arc<String>,
}

impl SourceSnapshot {
    /// Build a validated one-module snapshot for in-memory tools and tests.
    pub fn single(path: impl Into<String>, source: impl Into<String>) -> CompileResult<Self> {
        let path = path.into();
        let root = FileId::DEFAULT;
        let metadata =
            SourceMetadata::new(root, [(root, path.clone())].into(), [(root, path)].into())?;
        Self::new(metadata, vec![(root, Arc::new(source.into()))])
    }

    /// Reassemble diagnostic source records from already-validated parsed
    /// artifacts without hashing source bytes again.
    #[cfg(test)]
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
        let trusted_standard_library_files = program
            .modules()
            .iter()
            .filter_map(|module| {
                module
                    .module_id()
                    .is_trusted_standard_library()
                    .then_some(module.file_id())
            })
            .collect();
        let metadata = SourceMetadata::new_with_trusted_standard_library(
            root_file_id,
            physical_paths,
            logical_paths,
            trusted_standard_library_files,
        )?;
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
        let revision = program.source_revision().clone();

        Ok(Self {
            data: Arc::new(SourceSnapshotData {
                metadata,
                len: contents.len(),
                segments: vec![Arc::new(SnapshotSegment::from_records(contents))],
                segment_offsets: vec![0],
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
        validate_source_file_count(contents.len())?;
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
                let module_id = metadata.module_id(file_id).expect("validated membership");
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
        let root_module = metadata.root_module_id();
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
                len: contents.len(),
                segments: vec![Arc::new(SnapshotSegment::from_records(contents))],
                segment_offsets: vec![0],
                revision,
                source_store,
            }),
        })
    }

    #[cfg(test)]
    pub(crate) fn from_sources(
        sources: &[SourceView<'_>],
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
        self.data.len
    }

    /// Whether this snapshot contains no source files.
    ///
    /// Validated metadata is never empty, so a constructed snapshot always
    /// returns `false`.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.data.len == 0
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

    /// Borrow one supported source record view without copying its text.
    pub fn source(&self, file_id: FileId) -> Option<SourceView<'_>> {
        let record = self.record(file_id)?;
        let path = self
            .data
            .metadata
            .physical_path(file_id)
            .expect("snapshot contents validated against metadata");
        Some(SourceView::new(path, record.text.as_str(), file_id))
    }

    /// Borrow supported source record views in caller-supplied order.
    pub fn files(
        &self,
    ) -> impl DoubleEndedIterator<Item = SourceView<'_>> + ExactSizeIterator + '_ {
        (0..self.data.len).map(|index| self.file_at(index))
    }

    /// Exact file identities appended by a direct additive successor.
    ///
    /// The lineage check is structural and the indexed walk touches only the
    /// appended suffix, preserving RUE-1112's O(delta) publication contract.
    pub(crate) fn direct_appended_file_ids_from(&self, base: &Self) -> Option<Vec<FileId>> {
        self.data
            .revision
            .module_segments()
            .direct_delta_from(base.data.revision.module_segments())?;
        Some(
            (base.data.len..self.data.len)
                .map(|index| self.record_at(index).file_id)
                .collect(),
        )
    }

    fn content(&self, file_id: FileId) -> Option<&Arc<String>> {
        self.record(file_id).map(|record| &record.text)
    }

    fn record(&self, file_id: FileId) -> Option<&SourceRecord> {
        let segment_position = self
            .data
            .segments
            .partition_point(|segment| segment.max_file_index < file_id.index());
        let segment = self.data.segments.get(segment_position)?;
        if file_id.index() < segment.min_file_index {
            return None;
        }
        let position = *segment.index.get(&file_id)?;
        let record = &segment.contents[position];
        debug_assert_eq!(record.file_id, file_id);
        Some(record)
    }

    fn record_at(&self, index: usize) -> &SourceRecord {
        let segment_position = self
            .data
            .segment_offsets
            .partition_point(|&start| start <= index)
            .checked_sub(1)
            .expect("segment offsets begin at zero");
        let segment = &self.data.segments[segment_position];
        &segment.contents[index - self.data.segment_offsets[segment_position]]
    }

    fn file_at(&self, index: usize) -> SourceView<'_> {
        let record = self.record_at(index);
        let file_id = record.file_id;
        let path = self
            .data
            .metadata
            .physical_path(file_id)
            .expect("snapshot contents validated against metadata");
        SourceView::new(path, record.text.as_str(), file_id)
    }

    /// Build a strictly-additive successor snapshot using bounded size-tiered
    /// indexes. Only appended sources are hashed and validated; occasional tier
    /// merges copy record/path metadata but never source bytes or identities.
    /// Appended file ids are assigned after the base's, so predecessor ids stay
    /// stable across the extension.
    pub(crate) fn extend_with_appended(
        base: &SourceSnapshot,
        appended: Vec<AppendedSource>,
    ) -> CompileResult<SourceSnapshot> {
        if appended.is_empty() {
            return Ok(base.clone());
        }
        validate_source_file_count(base.len().saturating_add(appended.len()))?;
        let mut physical_paths = HashMap::new();
        let mut logical_paths = HashMap::new();
        let mut trusted = HashSet::new();
        for entry in &appended {
            physical_paths.insert(entry.file_id, entry.physical_path.clone());
            logical_paths.insert(entry.file_id, entry.logical_path.clone());
            if entry.trusted_standard_library {
                trusted.insert(entry.file_id);
            }
        }
        let metadata =
            base.data
                .metadata
                .extend_with_appended(physical_paths, logical_paths, trusted)?;
        validate_source_lengths(
            appended
                .iter()
                .map(|entry| (entry.file_id, entry.text.len())),
            &metadata,
        )?;
        // Hash ONLY the appended sources.
        let candidates: Vec<_> = appended
            .into_iter()
            .map(|entry| {
                let module_id = metadata
                    .module_id(entry.file_id)
                    .expect("appended entries were just validated into the metadata");
                let source_id = SourceId::from_shared_text(entry.text);
                (entry.file_id, module_id, source_id)
            })
            .collect();
        let source_store = SourceStore::extend_with_ids(
            &base.data.source_store,
            candidates.iter().map(|(_, _, source_id)| source_id.clone()),
        );
        let records: Vec<_> = candidates
            .into_iter()
            .map(|(file_id, module_id, requested_id)| {
                let source_id = source_store
                    .get(&requested_id)
                    .expect("store was extended with every appended source")
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
        let revision = SourceRevision::extend_with_appended(
            &base.data.revision,
            records
                .iter()
                .map(|record| ModuleRevision {
                    module: record.module_id.clone(),
                    source: record.source_id.clone(),
                })
                .collect(),
        )?;
        let appended_len = records.len();
        let mut segments = base.data.segments.clone();
        crate::shared_segments::push_size_tiered_segment(
            &mut segments,
            Arc::new(SnapshotSegment::from_records(records)),
            |segment| segment.contents.len(),
            |left, right| Arc::new(SnapshotSegment::merge(left, right)),
        );
        let mut next_offset = 0;
        let segment_offsets = segments
            .iter()
            .map(|segment| {
                let offset = next_offset;
                next_offset += segment.contents.len();
                offset
            })
            .collect();
        let len = base.data.len + appended_len;
        Ok(SourceSnapshot {
            data: Arc::new(SourceSnapshotData {
                metadata,
                segments,
                segment_offsets,
                len,
                revision,
                source_store,
            }),
        })
    }
}

/// One source appended by a strictly-additive snapshot extension (RUE-1112).
pub(crate) struct AppendedSource {
    pub(crate) file_id: FileId,
    pub(crate) physical_path: String,
    pub(crate) logical_path: String,
    pub(crate) trusted_standard_library: bool,
    pub(crate) text: Arc<String>,
}

/// The largest number of source files one compilation can distinguish.
///
/// A [`FileId`] is a `u32` and `FileId(0)` is reserved for the default/unknown
/// file, so the usable identifiers are `1..=u32::MAX` (spec Appendix C.3:2,
/// C.6:1).
pub const MAX_SOURCE_FILES: usize = u32::MAX as usize;

/// Reject a compilation that would need more source files than the `u32` file
/// identifier can distinguish.
///
/// Discovery numbers files densely from `FileId(1)`, so without this check the
/// `usize -> u32` narrowing would wrap and hand two different sources the same
/// identifier — exactly the silent index wraparound spec C.1:2 forbids. This is
/// the one place both snapshot routes (full rebuild and strictly-additive
/// extension) pass through.
pub(crate) fn validate_source_file_count(count: usize) -> CompileResult<()> {
    if count > MAX_SOURCE_FILES {
        return Err(CompileError::without_span(
            ErrorKind::CompilerResourceLimit(format!(
                "this compilation reaches {count} source files, exceeding the maximum of \
                 {MAX_SOURCE_FILES} files one compilation can distinguish (file identifiers are \
                 u32 with FileId(0) reserved)"
            )),
        ));
    }
    Ok(())
}

fn validate_source_len(file_id: FileId, path: &str, len: usize) -> CompileResult<()> {
    if len > MAX_SOURCE_BYTES {
        return Err(CompileError::without_span(
            ErrorKind::CompilerResourceLimit(format!(
                "source text for file ID {} ({path:?}) is {len} bytes, exceeding the maximum supported length of {} bytes",
                file_id.index(),
                MAX_SOURCE_BYTES
            )),
        ));
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
    fn preserves_caller_order_for_source_views() {
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
        let file = snapshot.source(FileId::new(7)).unwrap();
        assert_eq!(file.path, "src/main.rue");
        assert_eq!(file.source, "fn main() {}");
        assert!(snapshot.source(FileId::new(8)).is_none());
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
        assert_eq!(old.source(root).unwrap().path, "/old/main.rue");
        assert_eq!(
            edited.source_text(root),
            Some("fn main() -> i32 { helper() + 1 }")
        );
        assert_eq!(edited.source(root).unwrap().path, "/new/main.rue");
    }

    #[test]
    fn source_snapshots_are_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<SourceSnapshot>();
    }

    #[test]
    fn one_import_per_round_keeps_all_source_indexes_logarithmic() {
        const DEPTH: u32 = 1024;
        let mut snapshot = SourceSnapshot::single(
            "/project/main.rue",
            "const next = @import(\"module_1\"); fn main() {}",
        )
        .unwrap();

        for index in 1..=DEPTH {
            let text = if index == DEPTH {
                "pub fn leaf() -> i32 { 42 }".to_owned()
            } else {
                format!("const next = @import(\"module_{}\");", index + 1)
            };
            let file_id = FileId::new(index);
            snapshot = SourceSnapshot::extend_with_appended(
                &snapshot,
                vec![AppendedSource {
                    file_id,
                    physical_path: format!("/project/module_{index}.rue"),
                    logical_path: format!("module_{index}.rue"),
                    trusted_standard_library: false,
                    text: Arc::new(text.clone()),
                }],
            )
            .unwrap();
            assert_eq!(snapshot.source_text(file_id), Some(text.as_str()));
        }

        let depths = [
            snapshot.data.segments.len(),
            snapshot.data.metadata.segment_depth(),
            snapshot.data.source_store.segment_depth(),
            snapshot
                .source_revision()
                .module_segments()
                .segments()
                .len(),
        ];
        assert!(depths.into_iter().all(|depth| depth <= 12), "{depths:?}");
        assert!(
            depths
                .into_iter()
                .all(|depth| depth <= crate::shared_segments::MAX_SIZE_TIERED_SEGMENTS)
        );
        assert_eq!(snapshot.files().count(), DEPTH as usize + 1);
        assert_eq!(
            snapshot
                .files()
                .next_back()
                .expect("the deepest module is present")
                .file_id,
            FileId::new(DEPTH)
        );
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
    fn parsed_module_reassembly_preserves_typed_module_origin() {
        let root = FileId::new(7);
        let standard_library = FileId::new(11);
        let physical = HashMap::from([
            (root, "/project/main.rue".to_owned()),
            (standard_library, "/sdk/strbuf.rue".to_owned()),
        ]);
        let logical = HashMap::from([
            (root, "app/main.rue".to_owned()),
            (standard_library, "\0rue-std/strbuf.rue".to_owned()),
        ]);
        let metadata = SourceMetadata::new_with_trusted_standard_library(
            root,
            physical,
            logical,
            HashSet::from([standard_library]),
        )
        .unwrap();
        let original = SourceSnapshot::new(
            metadata,
            vec![
                (root, Arc::new("fn main() {}".to_owned())),
                (
                    standard_library,
                    Arc::new("pub struct StrBuf {}".to_owned()),
                ),
            ],
        )
        .unwrap();
        let parsed = crate::parsed_modules::parse_source_snapshot_modules(&original).unwrap();

        let reconstructed = SourceSnapshot::from_parsed_modules(&parsed).unwrap();
        for module in parsed.modules() {
            assert_eq!(
                reconstructed.metadata().module_id(module.file_id()),
                Some(module.module_id().clone())
            );
        }
        assert!(
            reconstructed
                .module_id(standard_library)
                .unwrap()
                .is_trusted_standard_library()
        );
        assert!(
            !reconstructed
                .module_id(root)
                .unwrap()
                .is_trusted_standard_library()
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
    fn test_only_borrowed_assembly_copies_text_and_validates_paths() {
        let mut borrowed = "borrowed".to_owned();
        let descriptor = metadata(&[(3, "main.rue")]);
        let snapshot = SourceSnapshot::from_sources(
            &[SourceView::new("main.rue", &borrowed, FileId::new(3))],
            descriptor.clone(),
        )
        .unwrap();

        borrowed.push_str(" changed");
        assert_eq!(snapshot.source_text(FileId::new(3)), Some("borrowed"));

        let wrong_path = [SourceView::new("other.rue", "source", FileId::new(3))];
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
        validate_source_len(FileId::new(12), "large.rue", MAX_SOURCE_BYTES).unwrap();
        assert_eq!(
            validate_source_len(FileId::new(12), "large.rue", MAX_SOURCE_BYTES + 1)
                .unwrap_err()
                .to_string(),
            "compiler resource limit exceeded: source text for file ID 12 (\"large.rue\") is 4294967296 bytes, exceeding the maximum supported length of 4294967295 bytes"
        );
    }

    #[cfg(target_pointer_width = "64")]
    #[test]
    fn rejects_more_source_files_than_the_file_identifier_can_distinguish() {
        // Spec C.3:2/C.1:2: file identifiers are u32 with FileId(0) reserved,
        // so the count is checked before the usize -> u32 narrowing wraps.
        validate_source_file_count(MAX_SOURCE_FILES).unwrap();
        let error = validate_source_file_count(MAX_SOURCE_FILES + 1).unwrap_err();
        assert_eq!(
            error.kind.code(),
            rue_error::ErrorCode::COMPILER_RESOURCE_LIMIT
        );
        assert_eq!(
            error.to_string(),
            "compiler resource limit exceeded: this compilation reaches 4294967296 source files, \
             exceeding the maximum of 4294967295 files one compilation can distinguish (file \
             identifiers are u32 with FileId(0) reserved)"
        );
    }
}
