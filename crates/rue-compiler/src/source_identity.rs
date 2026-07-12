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

use crate::{CompileOptions, LinkerMode, SourceMetadata, SourceSnapshot};

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
    buckets: Arc<BTreeMap<SourceBucketKey, Arc<[SourceId]>>>,
    len: usize,
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
        let mut buckets = BTreeMap::<_, Vec<_>>::new();
        for source in sources {
            buckets
                .entry(SourceBucketKey::from(&source))
                .or_default()
                .push(source);
        }
        Self {
            buckets: Arc::new(
                buckets
                    .into_iter()
                    .map(|(key, bucket)| (key, bucket.into()))
                    .collect(),
            ),
            len,
        }
    }

    /// Number of distinct exact source byte strings.
    pub fn len(&self) -> usize {
        self.len
    }

    /// Whether this store contains no source text.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Return this store's canonical exact identity for `source`.
    pub fn get(&self, source: &SourceId) -> Option<&SourceId> {
        let bucket = self.buckets.get(&SourceBucketKey::from(source))?;
        bucket
            .binary_search(source)
            .ok()
            .map(|index| &bucket[index])
    }

    /// Share the exact retained source allocation identified by `source`.
    pub fn shared_text(&self, source: &SourceId) -> Option<Arc<String>> {
        self.get(source).map(|source| source.0.text.clone())
    }

    /// Iterate exact identities in version, digest, then byte order.
    pub fn iter(&self) -> impl Iterator<Item = &SourceId> + '_ {
        self.buckets.values().flat_map(|bucket| bucket.iter())
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
        })
    }
    pub(crate) fn from_validated_canonical(path: &str) -> Self {
        debug_assert!(!path.is_empty());
        debug_assert_eq!(normalize_module_path(path), path);
        Self {
            logical_path: Arc::from(path),
        }
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
    modules: Arc<[ModuleRevision]>,
}
impl SourceRevision {
    pub fn root(&self) -> &ModuleId {
        &self.root
    }
    pub fn modules(&self) -> &[ModuleRevision] {
        &self.modules
    }
    /// Build a complete, canonical module/source mapping.
    pub fn new(root: ModuleId, mut modules: Vec<ModuleRevision>) -> CompileResult<Self> {
        modules.sort_by(|a, b| a.module.cmp(&b.module));
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
            modules: modules.into(),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ModuleResolutionInput {
    pub module: ModuleId,
    pub physical_path: Arc<str>,
}
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ModuleResolutionInputs {
    root: ModuleId,
    modules: Arc<[ModuleResolutionInput]>,
}
impl ModuleResolutionInputs {
    pub fn root(&self) -> &ModuleId {
        &self.root
    }
    pub fn modules(&self) -> &[ModuleResolutionInput] {
        &self.modules
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
            modules: modules.into(),
        })
    }

    pub fn physical_path(&self, module: &ModuleId) -> Option<&str> {
        self.modules
            .binary_search_by(|entry| entry.module.cmp(module))
            .ok()
            .map(|index| self.modules[index].physical_path.as_ref())
    }

    pub fn from_metadata(metadata: &SourceMetadata) -> Self {
        let root = ModuleId::from_validated_canonical(
            metadata.logical_path(metadata.root_file_id()).unwrap(),
        );
        let modules: Vec<_> = metadata
            .file_ids()
            .map(|id| ModuleResolutionInput {
                module: ModuleId::from_validated_canonical(metadata.logical_path(id).unwrap()),
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
    pub fn names(&self) -> &[Arc<str>] {
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
pub enum StableLinkerInput {
    Internal,
    System(Arc<str>),
}
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
pub struct LinkInputDescriptor {
    pub codegen: CodegenInputDescriptor,
    pub linker: StableLinkerInput,
}
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
