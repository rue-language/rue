//! Stable, immutable identities for source and externally supplied compiler inputs.

use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use rue_air::normalize_module_path;
use rue_cfg::OptLevel;
use rue_error::{CompileError, CompileResult, ErrorKind, PreviewFeatures};
use rue_target::Target;
use sha2::{Digest, Sha256};

#[cfg(test)]
use crate::CompileOptions;
#[cfg(test)]
use crate::LinkerMode;
use crate::{SourceMetadata, SourceSnapshot};

const SOURCE_DOMAIN_V1: &[u8] = b"rue.source\0v1\0sha256\0";

trait SourceDigester {
    fn digest(&self, text: &[u8]) -> [u8; 32];
}

struct Sha256Digester;

impl SourceDigester for Sha256Digester {
    fn digest(&self, text: &[u8]) -> [u8; 32] {
        let mut hasher = Sha256::new();
        hasher.update(SOURCE_DOMAIN_V1);
        hasher.update((text.len() as u64).to_le_bytes());
        hasher.update(text);
        hasher.finalize().into()
    }
}

/// Version of Rue's source-content identity scheme.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum SourceIdVersion {
    /// Domain-separated SHA-256 over the exact UTF-8 bytes.
    V1Sha256,
}

#[derive(Debug)]
struct SourceIdentity {
    version: SourceIdVersion,
    digest: [u8; 32],
    text: Arc<String>,
}

/// Exact source-content identity.
///
/// Cloning an ID is constant time but intentionally pins the shared source
/// text. Exact bytes participate in equality so digest collisions cannot make
/// distinct sources equal. Its textual form contains only the versioned digest
/// and may therefore collide; use `SourceId` equality, not `Display`, for exact
/// identity.
#[derive(Clone)]
pub struct SourceId(Arc<SourceIdentity>);

impl SourceId {
    /// Identify exact shared UTF-8 source text with the current production scheme.
    ///
    /// The returned ID retains this `Arc`; cloning the ID continues to pin the
    /// source allocation without copying its bytes.
    pub fn from_shared_text(text: Arc<String>) -> Self {
        Self::from_shared_text_with(text, &Sha256Digester)
    }

    fn from_shared_text_with(text: Arc<String>, digester: &dyn SourceDigester) -> Self {
        Self(Arc::new(SourceIdentity {
            version: SourceIdVersion::V1Sha256,
            digest: digester.digest(text.as_bytes()),
            text,
        }))
    }

    /// Identity scheme version.
    pub fn version(&self) -> SourceIdVersion {
        self.0.version
    }

    /// Versioned digest accelerator. Exact identity still compares bytes.
    pub fn digest(&self) -> [u8; 32] {
        self.0.digest
    }

    pub(crate) fn shared_text(&self) -> Arc<String> {
        self.0.text.clone()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct SourceBucketKey {
    version: SourceIdVersion,
    digest: [u8; 32],
}

impl From<&SourceId> for SourceBucketKey {
    fn from(source: &SourceId) -> Self {
        Self {
            version: source.version(),
            digest: source.digest(),
        }
    }
}

/// Immutable exact source storage owned by one compiler snapshot.
///
/// The versioned digest selects only a collision bucket. Every public lookup
/// takes a complete [`SourceId`] and compares exact source bytes inside that
/// bucket, so a digest can never select source text by itself. Equal byte
/// strings share one retained [`Arc<String>`], while true digest collisions
/// remain distinct entries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceStore {
    /// `Arc`-shared size-tiered bucket segments, oldest first.
    buckets: Vec<Arc<SourceStoreSegment>>,
    len: usize,
}

#[derive(Debug, PartialEq, Eq)]
struct SourceStoreSegment {
    buckets: BTreeMap<SourceBucketKey, Arc<[SourceId]>>,
    len: usize,
}

impl SourceStoreSegment {
    fn from_sources(sources: Vec<SourceId>) -> Self {
        let len = sources.len();
        let mut buckets = BTreeMap::<_, Vec<_>>::new();
        for source in sources {
            buckets
                .entry(SourceBucketKey::from(&source))
                .or_default()
                .push(source);
        }
        Self {
            buckets: buckets
                .into_iter()
                .map(|(key, bucket)| (key, bucket.into()))
                .collect(),
            len,
        }
    }

    fn merge(left: &Self, right: &Self) -> Self {
        let mut buckets = left.buckets.clone();
        for (key, right_bucket) in &right.buckets {
            let bucket = buckets.entry(*key).or_default();
            let mut merged = bucket
                .iter()
                .chain(right_bucket.iter())
                .cloned()
                .collect::<Vec<_>>();
            merged.sort();
            merged.dedup();
            *bucket = merged.into();
        }
        Self {
            buckets,
            len: left.len + right.len,
        }
    }
}

impl SourceStore {
    /// Build deterministic snapshot-local storage from shared source buffers.
    pub fn new(texts: impl IntoIterator<Item = Arc<String>>) -> Self {
        Self::new_with(texts, &Sha256Digester)
    }

    fn new_with(
        texts: impl IntoIterator<Item = Arc<String>>,
        digester: &dyn SourceDigester,
    ) -> Self {
        Self::from_ids(
            texts
                .into_iter()
                .map(|text| SourceId::from_shared_text_with(text, digester)),
        )
    }

    pub(crate) fn from_ids(sources: impl IntoIterator<Item = SourceId>) -> Self {
        let mut sources: Vec<_> = sources.into_iter().collect();
        sources.sort();
        sources.dedup();
        let len = sources.len();
        Self {
            buckets: vec![Arc::new(SourceStoreSegment::from_sources(sources))],
            len,
        }
    }

    /// Build a strictly-additive successor store holding only genuinely new
    /// identities. Untouched bucket tiers remain shared; equal-magnitude tail
    /// tiers are re-bucketed to keep lookup depth bounded.
    pub(crate) fn extend_with_ids(
        base: &SourceStore,
        appended: impl IntoIterator<Item = SourceId>,
    ) -> Self {
        let mut appended: Vec<_> = appended
            .into_iter()
            .filter(|source| base.get(source).is_none())
            .collect();
        appended.sort();
        appended.dedup();
        if appended.is_empty() {
            return base.clone();
        }
        let len = base.len + appended.len();
        let mut buckets = base.buckets.clone();
        crate::shared_segments::push_size_tiered_segment(
            &mut buckets,
            Arc::new(SourceStoreSegment::from_sources(appended)),
            |segment| segment.len,
            |left, right| Arc::new(SourceStoreSegment::merge(left, right)),
        );
        Self { buckets, len }
    }

    /// Number of distinct exact source byte strings.
    pub fn len(&self) -> usize {
        self.len
    }

    /// Whether this store contains no source text.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    #[cfg(test)]
    pub(crate) fn segment_depth(&self) -> usize {
        self.buckets.len()
    }

    /// Return this store's canonical exact identity for `source`.
    pub fn get(&self, source: &SourceId) -> Option<&SourceId> {
        let key = SourceBucketKey::from(source);
        self.buckets.iter().find_map(|segment| {
            let bucket = segment.buckets.get(&key)?;
            bucket
                .binary_search(source)
                .ok()
                .map(|index| &bucket[index])
        })
    }

    /// Share the exact retained source allocation identified by `source`.
    pub fn shared_text(&self, source: &SourceId) -> Option<Arc<String>> {
        self.get(source).map(|source| source.0.text.clone())
    }

    /// Iterate exact identities, segment by segment; within each segment the
    /// order is version, digest, then byte order.
    pub fn iter(&self) -> impl Iterator<Item = &SourceId> + '_ {
        self.buckets
            .iter()
            .flat_map(|segment| segment.buckets.values().flat_map(|bucket| bucket.iter()))
    }
}

impl PartialEq for SourceId {
    fn eq(&self, other: &Self) -> bool {
        self.0.version == other.0.version
            && self.0.digest == other.0.digest
            && self.0.text.as_bytes() == other.0.text.as_bytes()
    }
}
impl Eq for SourceId {}
impl Hash for SourceId {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.0.version.hash(state);
        self.0.digest.hash(state);
        self.0.text.len().hash(state);
    }
}
impl PartialOrd for SourceId {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for SourceId {
    fn cmp(&self, other: &Self) -> Ordering {
        (self.0.version, self.0.digest, self.0.text.as_bytes()).cmp(&(
            other.0.version,
            other.0.digest,
            other.0.text.as_bytes(),
        ))
    }
}
impl fmt::Debug for SourceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SourceId({self})")
    }
}
impl fmt::Display for SourceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("rue-source-v1-sha256:")?;
        for byte in self.0.digest {
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}

/// Canonical logical identity of one module.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ModuleId {
    logical_path: Arc<str>,
    origin: ModuleOrigin,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum ModuleOrigin {
    Caller,
    StandardLibrary,
}

impl ModuleId {
    pub fn from_logical_path(path: impl AsRef<str>) -> CompileResult<Self> {
        let path = normalize_module_path(path.as_ref());
        if path.is_empty() {
            return Err(CompileError::without_span(ErrorKind::InvalidCompilerInput(
                "module ID has an empty logical path".into(),
            )));
        }
        Ok(Self {
            logical_path: path.into(),
            origin: ModuleOrigin::Caller,
        })
    }
    pub(crate) fn from_validated_canonical(path: &str) -> Self {
        debug_assert!(!path.is_empty());
        debug_assert_eq!(normalize_module_path(path), path);
        Self {
            logical_path: Arc::from(path),
            origin: ModuleOrigin::Caller,
        }
    }
    pub(crate) fn from_trusted_standard_library_path(path: impl AsRef<str>) -> CompileResult<Self> {
        let path = normalize_module_path(path.as_ref());
        if !path.starts_with("\0rue-std/") {
            return Err(CompileError::without_span(ErrorKind::InvalidCompilerInput(
                "trusted standard-library module is outside the standard-library namespace".into(),
            )));
        }
        Ok(Self {
            logical_path: path.into(),
            origin: ModuleOrigin::StandardLibrary,
        })
    }
    pub(crate) fn from_trusted_validated_canonical(path: &str) -> Self {
        debug_assert!(path.starts_with("\0rue-std/"));
        debug_assert_eq!(normalize_module_path(path), path);
        Self {
            logical_path: Arc::from(path),
            origin: ModuleOrigin::StandardLibrary,
        }
    }
    pub fn is_trusted_standard_library(&self) -> bool {
        self.origin == ModuleOrigin::StandardLibrary
    }
    pub fn logical_path(&self) -> &str {
        &self.logical_path
    }
    pub fn as_str(&self) -> &str {
        self.logical_path()
    }
}
impl AsRef<str> for ModuleId {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}
impl fmt::Display for ModuleId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One module/content pair in a source revision.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ModuleRevision {
    pub module: ModuleId,
    pub source: SourceId,
}

/// Root plus complete module/content mapping, sorted by logical module ID.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SourceRevision {
    root: ModuleId,
    modules: crate::shared_segments::SharedSegments<ModuleRevision>,
}

/// Canonical order for the module/source mapping: by module identity.
fn module_revision_order(a: &ModuleRevision, b: &ModuleRevision) -> std::cmp::Ordering {
    a.module.cmp(&b.module)
}

impl SourceRevision {
    pub fn root(&self) -> &ModuleId {
        &self.root
    }
    pub fn modules(&self) -> &[ModuleRevision] {
        self.modules.as_slice()
    }
    /// The shared segmented representation (RUE-1112 successor sharing).
    pub(crate) fn module_segments(
        &self,
    ) -> &crate::shared_segments::SharedSegments<ModuleRevision> {
        &self.modules
    }
    /// Build a complete, canonical module/source mapping.
    pub fn new(root: ModuleId, mut modules: Vec<ModuleRevision>) -> CompileResult<Self> {
        modules.sort_by(module_revision_order);
        if let Some(duplicate) = modules
            .windows(2)
            .find(|pair| pair[0].module == pair[1].module)
            .map(|pair| &pair[0].module)
        {
            return Err(CompileError::without_span(ErrorKind::InvalidCompilerInput(
                format!("source revision contains duplicate module ID {duplicate:?}"),
            )));
        }
        if modules
            .binary_search_by(|entry| entry.module.cmp(&root))
            .is_err()
        {
            return Err(CompileError::without_span(ErrorKind::InvalidCompilerInput(
                format!("source revision root module {root:?} is absent"),
            )));
        }
        Ok(Self {
            root,
            modules: crate::shared_segments::SharedSegments::flat(
                modules.into(),
                module_revision_order,
            ),
        })
    }

    /// Build a strictly-additive successor mapping from an exact appended delta.
    /// Validates only the appended entries; untouched size tiers remain shared
    /// and equal-magnitude tails compact in canonical order.
    pub(crate) fn extend_with_appended(
        base: &SourceRevision,
        appended: Vec<ModuleRevision>,
    ) -> CompileResult<Self> {
        for entry in &appended {
            if base
                .modules
                .contains_by(|candidate| candidate.module.cmp(&entry.module))
            {
                return Err(CompileError::without_span(ErrorKind::InvalidCompilerInput(
                    format!(
                        "source revision successor re-adds existing module ID {:?}",
                        entry.module
                    ),
                )));
            }
        }
        let mut sorted = appended;
        sorted.sort_by(module_revision_order);
        if let Some(duplicate) = sorted
            .windows(2)
            .find(|pair| pair[0].module == pair[1].module)
            .map(|pair| &pair[0].module)
        {
            return Err(CompileError::without_span(ErrorKind::InvalidCompilerInput(
                format!("source revision contains duplicate module ID {duplicate:?}"),
            )));
        }
        Ok(Self {
            root: base.root.clone(),
            modules: crate::shared_segments::SharedSegments::extend(&base.modules, sorted),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ModuleResolutionInput {
    pub module: ModuleId,
    pub physical_path: Arc<str>,
}
/// Canonical order for module-resolution inputs: by module identity.
fn module_input_order(a: &ModuleResolutionInput, b: &ModuleResolutionInput) -> std::cmp::Ordering {
    a.module.cmp(&b.module)
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ModuleResolutionInputs {
    root: ModuleId,
    modules: crate::shared_segments::SharedSegments<ModuleResolutionInput>,
}
impl ModuleResolutionInputs {
    pub fn root(&self) -> &ModuleId {
        &self.root
    }
    pub fn modules(&self) -> &[ModuleResolutionInput] {
        self.modules.as_slice()
    }

    /// Whether `module` is in the resolution set, by binary search over the
    /// shared segments — O(log n) with no materialization (RUE-1112).
    pub(crate) fn contains(&self, module: &ModuleId) -> bool {
        self.modules.contains_by(|entry| entry.module.cmp(module))
    }

    /// The shared module-table representation (RUE-1112 successor sharing).
    pub(crate) fn module_segments(
        &self,
    ) -> &crate::shared_segments::SharedSegments<ModuleResolutionInput> {
        &self.modules
    }

    /// Build a strictly-additive successor by structurally sharing `base`'s
    /// module table and appending `delta` (RUE-1112). Only `delta` is validated
    /// and sorted; `base`'s table is carried by reference. `delta` must be
    /// disjoint from `base` (the modules added since the predecessor close).
    pub(crate) fn extend_successor(
        base: &ModuleResolutionInputs,
        delta: Vec<ModuleResolutionInput>,
    ) -> CompileResult<Self> {
        if let Some(module) = delta
            .iter()
            .find(|entry| entry.physical_path.is_empty())
            .map(|entry| &entry.module)
        {
            return Err(CompileError::without_span(ErrorKind::InvalidCompilerInput(
                format!("module resolution input {module:?} has an empty physical path"),
            )));
        }
        let mut sorted = delta.clone();
        sorted.sort_by(module_input_order);
        if let Some(duplicate) = sorted
            .windows(2)
            .find(|pair| pair[0].module == pair[1].module)
            .map(|pair| &pair[0].module)
        {
            return Err(CompileError::without_span(ErrorKind::InvalidCompilerInput(
                format!("module resolution inputs contain duplicate module ID {duplicate:?}"),
            )));
        }
        Ok(Self {
            root: base.root.clone(),
            modules: crate::shared_segments::SharedSegments::extend(&base.modules, delta),
        })
    }
    /// Build canonical explicit module-resolution inputs.
    pub fn new(root: ModuleId, mut modules: Vec<ModuleResolutionInput>) -> CompileResult<Self> {
        modules.sort_by(|a, b| a.module.cmp(&b.module));
        if let Some(module) = modules
            .iter()
            .find(|entry| entry.physical_path.is_empty())
            .map(|entry| &entry.module)
        {
            return Err(CompileError::without_span(ErrorKind::InvalidCompilerInput(
                format!("module resolution input {module:?} has an empty physical path"),
            )));
        }
        if let Some(duplicate) = modules
            .windows(2)
            .find(|pair| pair[0].module == pair[1].module)
            .map(|pair| &pair[0].module)
        {
            return Err(CompileError::without_span(ErrorKind::InvalidCompilerInput(
                format!("module resolution inputs contain duplicate module ID {duplicate:?}"),
            )));
        }
        if modules
            .binary_search_by(|entry| entry.module.cmp(&root))
            .is_err()
        {
            return Err(CompileError::without_span(ErrorKind::InvalidCompilerInput(
                format!("module resolution root {root:?} is absent"),
            )));
        }
        let mut physical_paths: Vec<_> = modules
            .iter()
            .map(|entry| (normalize_module_path(&entry.physical_path), &entry.module))
            .collect();
        physical_paths.sort();
        if let Some((path, _)) = physical_paths
            .windows(2)
            .find(|pair| pair[0].0 == pair[1].0)
            .map(|pair| &pair[0])
        {
            return Err(CompileError::without_span(ErrorKind::InvalidCompilerInput(
                format!("module resolution inputs contain duplicate physical path {path:?}"),
            )));
        }
        Ok(Self {
            root,
            modules: crate::shared_segments::SharedSegments::flat(
                modules.into(),
                module_input_order,
            ),
        })
    }

    pub fn physical_path(&self, module: &ModuleId) -> Option<&str> {
        self.modules
            .find_by(|entry| entry.module.cmp(module))
            .map(|entry| entry.physical_path.as_ref())
    }

    pub fn from_metadata(metadata: &SourceMetadata) -> Self {
        let root = metadata.root_module_id();
        let modules: Vec<_> = metadata
            .file_ids()
            .map(|id| ModuleResolutionInput {
                module: metadata.module_id(id).unwrap(),
                physical_path: Arc::from(metadata.physical_path(id).unwrap()),
            })
            .collect();
        Self::new(root, modules).expect("validated source metadata is canonical")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct StablePreviewFeatures(Arc<[Arc<str>]>);
impl StablePreviewFeatures {
    pub fn new(features: &PreviewFeatures) -> Self {
        let mut names: Vec<Arc<str>> = features.iter().map(|f| Arc::from(f.name())).collect();
        names.sort();
        names.dedup();
        Self(names.into())
    }
    pub(crate) fn contains(&self, feature: rue_error::PreviewFeature) -> bool {
        self.0
            .binary_search_by(|name| name.as_ref().cmp(feature.name()))
            .is_ok()
    }
    pub(crate) fn names(&self) -> &[Arc<str>] {
        &self.0
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StableOptLevel {
    O0,
    O1,
    O2,
    O3,
}
impl From<OptLevel> for StableOptLevel {
    fn from(value: OptLevel) -> Self {
        match value {
            OptLevel::O0 => Self::O0,
            OptLevel::O1 => Self::O1,
            OptLevel::O2 => Self::O2,
            OptLevel::O3 => Self::O3,
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg(test)]
pub enum StableLinkerInput {
    Internal,
    System(Arc<str>),
}
#[cfg(test)]
impl From<&LinkerMode> for StableLinkerInput {
    fn from(value: &LinkerMode) -> Self {
        match value {
            LinkerMode::Internal => Self::Internal,
            LinkerMode::System(s) => Self::System(Arc::from(s.as_str())),
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SemanticInputDescriptor {
    pub sources: SourceRevision,
    pub resolution: ModuleResolutionInputs,
    pub target: Target,
    pub preview_features: StablePreviewFeatures,
}
impl SemanticInputDescriptor {
    pub fn new(snapshot: &SourceSnapshot, target: Target, features: &PreviewFeatures) -> Self {
        Self {
            sources: snapshot.source_revision().clone(),
            resolution: ModuleResolutionInputs::from_metadata(snapshot.metadata()),
            target,
            preview_features: StablePreviewFeatures::new(features),
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CodegenInputDescriptor {
    pub semantic: SemanticInputDescriptor,
    pub opt_level: StableOptLevel,
}
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg(test)]
pub struct LinkInputDescriptor {
    pub codegen: CodegenInputDescriptor,
    pub linker: StableLinkerInput,
}
#[cfg(test)]
impl LinkInputDescriptor {
    pub fn from_compile_options(snapshot: &SourceSnapshot, options: &CompileOptions) -> Self {
        Self {
            codegen: CodegenInputDescriptor {
                semantic: SemanticInputDescriptor::new(
                    snapshot,
                    options.target,
                    &options.preview_features,
                ),
                opt_level: options.opt_level.into(),
            },
            linker: (&options.linker).into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rue_error::PreviewFeature;
    use std::collections::hash_map::DefaultHasher;
    use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
    struct Constant;
    impl SourceDigester for Constant {
        fn digest(&self, _: &[u8]) -> [u8; 32] {
            [7; 32]
        }
    }
    struct Counting(AtomicUsize);
    impl SourceDigester for Counting {
        fn digest(&self, text: &[u8]) -> [u8; 32] {
            self.0.fetch_add(1, AtomicOrdering::Relaxed);
            Sha256Digester.digest(text)
        }
    }
    fn hash(value: &SourceId) -> u64 {
        let mut h = DefaultHasher::new();
        value.hash(&mut h);
        h.finish()
    }
    #[test]
    fn collisions_do_not_equal_or_replace_text() {
        let a = SourceId::from_shared_text_with(Arc::new("a".into()), &Constant);
        let b = SourceId::from_shared_text_with(Arc::new("b".into()), &Constant);
        assert_ne!(a, b);
        assert_ne!(a.cmp(&b), Ordering::Equal);
        assert_eq!(a.digest(), b.digest());
        assert_eq!(a.shared_text().as_str(), "a");
        assert_eq!(b.shared_text().as_str(), "b");
    }
    #[test]
    fn equal_values_have_equal_hashes_and_share_text() {
        let text = Arc::new(String::from("same"));
        let a = SourceId::from_shared_text_with(text.clone(), &Constant);
        let b = SourceId::from_shared_text_with(text.clone(), &Constant);
        assert_eq!(a, b);
        assert_eq!(hash(&a), hash(&b));
        assert!(Arc::ptr_eq(&a.shared_text(), &text));
    }

    #[test]
    fn identical_bytes_in_distinct_allocations_have_identical_value_semantics() {
        let left_text = Arc::new(String::from("identical"));
        let right_text = Arc::new(String::from("identical"));
        assert!(!Arc::ptr_eq(&left_text, &right_text));
        let left = SourceId::from_shared_text(left_text);
        let right = SourceId::from_shared_text(right_text);
        assert_eq!(left, right);
        assert_eq!(left.cmp(&right), Ordering::Equal);
        assert_eq!(hash(&left), hash(&right));
    }

    #[test]
    fn source_store_deduplicates_exact_bytes_and_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<SourceStore>();

        let retained = Arc::new(String::from("same"));
        let duplicate = Arc::new(String::from("same"));
        let store = SourceStore::new([retained.clone(), duplicate.clone()]);
        let lookup = SourceId::from_shared_text(duplicate);
        let stored = store.shared_text(&lookup).unwrap();

        assert_eq!(store.len(), 1);
        assert!(Arc::ptr_eq(&stored, &retained));
        assert!(Arc::ptr_eq(
            &store.get(&lookup).unwrap().shared_text(),
            &retained
        ));
    }

    #[test]
    fn source_store_constructs_one_identity_per_input() {
        let digester = Counting(AtomicUsize::new(0));
        let store = SourceStore::new_with(
            ["one", "two", "one"].map(|text| Arc::new(text.to_owned())),
            &digester,
        );
        assert_eq!(digester.0.load(AtomicOrdering::Relaxed), 3);
        assert_eq!(store.len(), 2);
    }

    #[test]
    fn source_store_retains_and_exactly_retrieves_forced_collisions() {
        let store = SourceStore::new_with(
            [Arc::new(String::from("b")), Arc::new(String::from("a"))],
            &Constant,
        );
        let a = SourceId::from_shared_text_with(Arc::new(String::from("a")), &Constant);
        let b = SourceId::from_shared_text_with(Arc::new(String::from("b")), &Constant);

        assert_eq!(store.len(), 2);
        assert_eq!(store.shared_text(&a).unwrap().as_str(), "a");
        assert_eq!(store.shared_text(&b).unwrap().as_str(), "b");
        assert_eq!(
            store
                .iter()
                .map(|source| source.shared_text().as_str().to_owned())
                .collect::<Vec<_>>(),
            ["a", "b"]
        );
    }

    #[test]
    fn source_store_value_and_iteration_ignore_input_order() {
        let make = |texts: &[&str]| {
            SourceStore::new(texts.iter().map(|text| Arc::new((*text).to_owned())))
                .iter()
                .cloned()
                .collect::<Vec<_>>()
        };
        assert_eq!(
            make(&["three", "one", "two"]),
            make(&["two", "three", "one"])
        );
    }

    #[test]
    fn v1_digest_framing_is_stable() {
        let id = SourceId::from_shared_text(Arc::new("abc".to_owned()));
        assert_eq!(
            id.to_string(),
            "rue-source-v1-sha256:4aa48dd8273d312c382c549796d09c5dc9b528b6563d3d1913554fcfce735e5e"
        );
    }

    #[test]
    fn preview_feature_identity_ignores_hash_set_insertion_order() {
        let left = PreviewFeatures::from([PreviewFeature::Slices, PreviewFeature::TestInfra]);
        let right = PreviewFeatures::from([PreviewFeature::TestInfra, PreviewFeature::Slices]);
        let left = StablePreviewFeatures::new(&left);
        let right = StablePreviewFeatures::new(&right);
        assert_eq!(left, right);
        assert_eq!(
            left.names().iter().map(AsRef::as_ref).collect::<Vec<_>>(),
            ["slices", "test_infra"]
        );
    }

    #[test]
    fn source_revision_rejects_duplicate_modules_and_missing_root() {
        let root = ModuleId::from_logical_path("root.rue").unwrap();
        let other = ModuleId::from_logical_path("other.rue").unwrap();
        let source = SourceId::from_shared_text(Arc::new("fn main() {}".to_owned()));
        let duplicate = SourceRevision::new(
            root.clone(),
            vec![
                ModuleRevision {
                    module: root.clone(),
                    source: source.clone(),
                },
                ModuleRevision {
                    module: root.clone(),
                    source: source.clone(),
                },
            ],
        )
        .unwrap_err();
        assert!(duplicate.to_string().contains("duplicate module ID"));
        let missing = SourceRevision::new(
            root,
            vec![ModuleRevision {
                module: other,
                source,
            }],
        )
        .unwrap_err();
        assert!(missing.to_string().contains("root module"));
    }

    #[test]
    fn module_resolution_inputs_are_canonical_and_reject_ambiguous_provenance() {
        let root = ModuleId::from_logical_path("root.rue").unwrap();
        let helper = ModuleId::from_logical_path("helper.rue").unwrap();
        let inputs = ModuleResolutionInputs::new(
            root.clone(),
            vec![
                ModuleResolutionInput {
                    module: helper.clone(),
                    physical_path: Arc::from("/p/helper.rue"),
                },
                ModuleResolutionInput {
                    module: root.clone(),
                    physical_path: Arc::from("/p/root.rue"),
                },
            ],
        )
        .unwrap();
        assert_eq!(inputs.modules()[0].module, helper);
        assert_eq!(inputs.physical_path(&root), Some("/p/root.rue"));

        let duplicate_path = ModuleResolutionInputs::new(
            root.clone(),
            vec![
                ModuleResolutionInput {
                    module: root,
                    physical_path: Arc::from("/p/dir/../same.rue"),
                },
                ModuleResolutionInput {
                    module: ModuleId::from_logical_path("other.rue").unwrap(),
                    physical_path: Arc::from("/p/same.rue"),
                },
            ],
        )
        .unwrap_err();
        assert!(
            duplicate_path
                .to_string()
                .contains("duplicate physical path")
        );
    }
}
