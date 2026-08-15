//! Stable source-definition identities issued only after semantic binding.

#[cfg(test)]
use std::collections::HashMap;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::sync::Arc;
#[cfg(test)]
use std::sync::atomic::{AtomicU64, Ordering};

#[cfg(test)]
use rue_air::DeclarationBindingWork;
#[cfg(test)]
use rue_air::{
    CanonicalImportView, Sema, SemanticBinding, SemanticBindingManifestWork,
    SemanticDeclarationShell,
};
#[cfg(test)]
use rue_error::{CompileError, CompileErrors, CompileResult, ErrorKind, MultiErrorResult};
#[cfg(test)]
use rue_parser::ast::{Item, Visibility};
#[cfg(test)]
use rue_span::{FileId, Span};
#[cfg(test)]
use rue_target::Target;
use sha2::{Digest, Sha256};

use crate::ModuleId;
#[cfg(test)]
use crate::{
    CanonicalMergedProgram, CanonicalRirOutput, DefinitionKind, DefinitionNamespace,
    DefinitionOccurrenceId, PreviewFeatures, SourceRevision,
};

#[cfg(test)]
struct CanonicalSemanticImportView<'a> {
    merged: &'a CanonicalMergedProgram,
    graph: &'a crate::CanonicalImportGraph,
}

#[cfg(test)]
impl CanonicalImportView for CanonicalSemanticImportView<'_> {
    fn visit_modules(
        &self,
        visitor: &mut dyn FnMut(&str, FileId, &str) -> CompileResult<()>,
    ) -> CompileResult<()> {
        for module in self.merged.ast().modules() {
            visitor(
                module.module_id().as_str(),
                module.file_id(),
                module.physical_path(),
            )?;
        }
        Ok(())
    }

    fn visit_resolved_sites(
        &self,
        visitor: &mut dyn FnMut(&str, u32, &str, &str) -> CompileResult<()>,
    ) -> CompileResult<()> {
        for directive in self.merged.ast().import_directives().iter() {
            let normalized = rue_air::normalize_module_path(directive.specifier());
            let record = self
                .graph
                .record_for(directive.importer(), &normalized)
                .ok_or_else(|| {
                    invalid("validated canonical graph does not cover a parsed import directive")
                })?;
            if let crate::CanonicalImportResolution::Resolved(target) = record.resolution() {
                visitor(
                    directive.importer().as_str(),
                    directive.source_offset(),
                    directive.specifier(),
                    target.as_str(),
                )?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
fn invalid(message: impl Into<String>) -> CompileError {
    CompileError::without_span(ErrorKind::InvalidCompilerInput(message.into()))
}

#[cfg(test)]
#[derive(Debug)]
struct DefinitionIssuer {
    id: u64,
}
#[cfg(test)]
static NEXT_DEFINITION_ISSUER: AtomicU64 = AtomicU64::new(1);

/// Durable-key name for the canonical semantic namespace taxonomy.
pub type StableDefinitionNamespace = rue_air::StableDefinitionNamespace;

/// Durable-key name for the canonical semantic kind taxonomy.
pub type StableDefinitionKind = rue_air::StableDefinitionKind;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct StableNamedTypeKey {
    module: ModuleId,
    kind: StableDefinitionKind,
    name: Arc<str>,
}

impl StableNamedTypeKey {
    pub fn module(&self) -> &ModuleId {
        &self.module
    }
    pub fn kind(&self) -> StableDefinitionKind {
        self.kind
    }
    pub fn name(&self) -> &str {
        &self.name
    }
    pub(crate) fn shared_name(&self) -> &Arc<str> {
        &self.name
    }
}

#[derive(Debug)]
struct StableDefinitionIdentity {
    module: ModuleId,
    namespace: StableDefinitionNamespace,
    kind: StableDefinitionKind,
    name: Arc<str>,
    owner: Option<StableNamedTypeKey>,
    hash_accelerator: [u8; 16],
}

/// Immutable durable identity for one bound definition.
///
/// These keys are copied through recursive type/function identities and then
/// hashed repeatedly by the query runtime. Sharing the exact field payload
/// keeps clones constant-size, while the cached collision-resistant
/// accelerator avoids re-hashing every module/name string at each lookup.
/// Equality and ordering remain authoritative over the complete fields: the
/// accelerator is only a bucket selector and cannot conflate distinct keys.
/// It is recomputed at issuance and is not a durable or serialized identity.
#[derive(Clone)]
pub struct StableDefinitionKey(Arc<StableDefinitionIdentity>);

struct DefinitionIdentityDigester(Sha256);

impl Hasher for DefinitionIdentityDigester {
    fn finish(&self) -> u64 {
        let digest = self.0.clone().finalize();
        u64::from_le_bytes(digest[..8].try_into().expect("SHA-256 prefix length"))
    }

    fn write(&mut self, bytes: &[u8]) {
        self.0.update(bytes);
    }
}

fn definition_hash_accelerator(
    module: &ModuleId,
    namespace: StableDefinitionNamespace,
    kind: StableDefinitionKind,
    name: &Arc<str>,
    owner: &Option<StableNamedTypeKey>,
) -> [u8; 16] {
    let mut hasher = DefinitionIdentityDigester(Sha256::new());
    hasher.write(b"rue.stable-definition-key\0v1\0sha256\0");
    module.hash(&mut hasher);
    namespace.hash(&mut hasher);
    kind.hash(&mut hasher);
    name.hash(&mut hasher);
    owner.hash(&mut hasher);
    let digest = hasher.0.finalize();
    digest[..16]
        .try_into()
        .expect("SHA-256 accelerator prefix length")
}

impl StableDefinitionKey {
    pub(crate) fn from_stable_parts(
        module: ModuleId,
        namespace: StableDefinitionNamespace,
        kind: StableDefinitionKind,
        name: impl Into<Arc<str>>,
        owner: Option<(StableDefinitionKind, Arc<str>)>,
    ) -> Self {
        let owner = owner.map(|(kind, name)| StableNamedTypeKey {
            module: module.clone(),
            kind,
            name,
        });
        let name = name.into();
        let hash_accelerator = definition_hash_accelerator(&module, namespace, kind, &name, &owner);
        Self(Arc::new(StableDefinitionIdentity {
            module,
            namespace,
            kind,
            name,
            owner,
            hash_accelerator,
        }))
    }

    pub fn module(&self) -> &ModuleId {
        &self.0.module
    }
    pub fn namespace(&self) -> StableDefinitionNamespace {
        self.0.namespace
    }
    pub fn kind(&self) -> StableDefinitionKind {
        self.0.kind
    }
    pub fn name(&self) -> &str {
        &self.0.name
    }
    pub(crate) fn shared_name(&self) -> &Arc<str> {
        &self.0.name
    }
    pub fn owner(&self) -> Option<&StableNamedTypeKey> {
        self.0.owner.as_ref()
    }

    #[cfg(test)]
    pub(crate) fn for_test(
        module: ModuleId,
        namespace: StableDefinitionNamespace,
        kind: StableDefinitionKind,
        name: impl Into<Arc<str>>,
        owner: Option<(StableDefinitionKind, Arc<str>)>,
    ) -> Self {
        Self::from_stable_parts(module, namespace, kind, name, owner)
    }
}

impl fmt::Debug for StableDefinitionKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("StableDefinitionKey")
            .field("module", &self.0.module)
            .field("namespace", &self.0.namespace)
            .field("kind", &self.0.kind)
            .field("name", &self.0.name)
            .field("owner", &self.0.owner)
            .finish()
    }
}

impl PartialEq for StableDefinitionKey {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
            || (self.0.module == other.0.module
                && self.0.namespace == other.0.namespace
                && self.0.kind == other.0.kind
                && self.0.name == other.0.name
                && self.0.owner == other.0.owner)
    }
}

impl Eq for StableDefinitionKey {}

impl PartialOrd for StableDefinitionKey {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for StableDefinitionKey {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // Stable keys propagate by cloning this immutable identity. Ordered
        // maps commonly compare a lookup key with that exact clone, so match
        // equality's constant-time path before walking the shared strings.
        if Arc::ptr_eq(&self.0, &other.0) {
            return std::cmp::Ordering::Equal;
        }
        (
            &self.0.module,
            self.0.namespace,
            self.0.kind,
            &self.0.name,
            &self.0.owner,
        )
            .cmp(&(
                &other.0.module,
                other.0.namespace,
                other.0.kind,
                &other.0.name,
                &other.0.owner,
            ))
    }
}

impl Hash for StableDefinitionKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        state.write(&self.0.hash_accelerator);
    }
}

// The compiler's production query graph issues `StableDefinitionKey` values
// directly. The snapshot-wide issuer below remains only as a differential test
// oracle for that identity contract; it is not a second semantic path.
#[cfg(test)]
pub(crate) use legacy_snapshot_oracle::{
    BoundDefinitionSet, bind_canonical_declaration_semantics, configure_canonical_sema,
    issue_shell_definitions,
};

#[cfg(test)]
mod legacy_snapshot_oracle {
    use super::*;

    #[derive(Debug, Clone)]
    pub struct SnapshotBoundDefinitionId {
        key: StableDefinitionKey,
        issuer: Arc<DefinitionIssuer>,
    }

    /// Compatibility name for an issuer-scoped, snapshot-local authorization ID.
    pub type BoundDefinitionId = SnapshotBoundDefinitionId;

    impl SnapshotBoundDefinitionId {
        pub fn stable_key(&self) -> &StableDefinitionKey {
            &self.key
        }
    }

    impl PartialEq for SnapshotBoundDefinitionId {
        fn eq(&self, other: &Self) -> bool {
            self.key == other.key && Arc::ptr_eq(&self.issuer, &other.issuer)
        }
    }
    impl Eq for SnapshotBoundDefinitionId {}

    #[derive(Debug, Clone)]
    pub struct BoundDefinitionRecord {
        id: BoundDefinitionId,
        occurrence: Option<DefinitionOccurrenceId>,
        declaration_span: Span,
        visibility: Option<Visibility>,
    }

    /// Parser-authored boundaries for hashing a declaration without treating its
    /// executable payload as part of its signature.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub(crate) enum BoundDefinitionInputPartition {
        Body { signature: Span, body: Span },
        Initializer { signature: Span, initializer: Span },
        ExactSignature(Arc<[Span]>),
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    struct SyntaxPartitionIdentity {
        namespace: StableDefinitionNamespace,
        declaration_span: Span,
    }

    #[derive(Debug, Clone, Copy)]
    enum SyntaxPartitionSource<'a> {
        Body {
            declaration: Span,
            body: Span,
        },
        Initializer {
            declaration: Span,
            initializer: Span,
        },
        ExactSignature(Span),
        Struct(&'a rue_parser::ast::StructDecl),
    }

    impl SyntaxPartitionSource<'_> {
        fn partition(self) -> Result<BoundDefinitionInputPartition, CompileError> {
            match self {
                Self::Body { declaration, body } => partition_prefix(declaration, body)
                    .map(|signature| BoundDefinitionInputPartition::Body { signature, body }),
                Self::Initializer {
                    declaration,
                    initializer,
                } => partition_prefix(declaration, initializer).map(|signature| {
                    BoundDefinitionInputPartition::Initializer {
                        signature,
                        initializer,
                    }
                }),
                Self::ExactSignature(span) => Ok(BoundDefinitionInputPartition::ExactSignature(
                    vec![span].into(),
                )),
                Self::Struct(structure) => Ok(BoundDefinitionInputPartition::ExactSignature(
                    signature_fragments_excluding_method_bodies(structure)?.into(),
                )),
            }
        }
    }

    #[derive(Debug)]
    struct SyntaxPartitionIndex<'a> {
        by_identity: HashMap<SyntaxPartitionIdentity, SyntaxPartitionSource<'a>>,
        ast_nodes_visited: usize,
    }

    impl<'a> SyntaxPartitionIndex<'a> {
        fn new(module: &'a crate::parsed_modules::ParsedModule) -> Self {
            let mut index = Self {
                by_identity: HashMap::new(),
                ast_nodes_visited: 0,
            };
            for item in &module.ast().items {
                index.ast_nodes_visited += 1;
                match item {
                    Item::Function(value) => index.insert(
                        StableDefinitionNamespace::Value,
                        value.span,
                        SyntaxPartitionSource::Body {
                            declaration: value.span,
                            body: value.body.span(),
                        },
                    ),
                    Item::Struct(value) => {
                        index.insert(
                            StableDefinitionNamespace::Type,
                            value.span,
                            SyntaxPartitionSource::Struct(value),
                        );
                        for method in &value.methods {
                            index.ast_nodes_visited += 1;
                            index.insert(
                                StableDefinitionNamespace::Method,
                                method.span,
                                SyntaxPartitionSource::Body {
                                    declaration: method.span,
                                    body: method.body.span(),
                                },
                            );
                        }
                    }
                    Item::Enum(value) => index.insert(
                        StableDefinitionNamespace::Type,
                        value.span,
                        SyntaxPartitionSource::ExactSignature(value.span),
                    ),
                    Item::DropFn(value) => index.insert(
                        StableDefinitionNamespace::Destructor,
                        value.span,
                        SyntaxPartitionSource::Body {
                            declaration: value.span,
                            body: value.body.span(),
                        },
                    ),
                    Item::Extern(block) => {
                        for foreign in &block.fns {
                            index.ast_nodes_visited += 1;
                            index.insert(
                                StableDefinitionNamespace::Value,
                                foreign.span,
                                SyntaxPartitionSource::ExactSignature(foreign.span),
                            );
                        }
                    }
                    Item::Const(value) => index.insert(
                        StableDefinitionNamespace::Value,
                        value.span,
                        SyntaxPartitionSource::Initializer {
                            declaration: value.span,
                            initializer: value.init.span(),
                        },
                    ),
                    Item::Error(_) => {}
                }
            }
            index
        }

        fn insert(
            &mut self,
            namespace: StableDefinitionNamespace,
            declaration_span: Span,
            source: SyntaxPartitionSource<'a>,
        ) {
            // Preserve the source-ordered first-match behavior of the former AST
            // scans if malformed canonical syntax repeats an exact identity.
            self.by_identity
                .entry(SyntaxPartitionIdentity {
                    namespace,
                    declaration_span,
                })
                .or_insert(source);
        }

        fn partition_for(
            &self,
            binding: &SemanticBinding,
        ) -> Result<BoundDefinitionInputPartition, CompileError> {
            if binding.namespace == StableDefinitionNamespace::Method && binding.owner.is_none() {
                return Err(invalid("named method binding has no owner"));
            }
            let source = self
                .by_identity
                .get(&SyntaxPartitionIdentity {
                    namespace: binding.namespace,
                    declaration_span: binding.declaration_span,
                })
                .copied()
                .ok_or_else(|| {
                    if binding.namespace == StableDefinitionNamespace::Method {
                        invalid("named method binding does not join canonical syntax")
                    } else {
                        invalid(
                            "semantic binding does not join an authoritative canonical syntax item",
                        )
                    }
                })?;
            source.partition()
        }
    }

    impl BoundDefinitionRecord {
        #[cfg(test)]
        pub fn id(&self) -> &BoundDefinitionId {
            &self.id
        }
        pub fn stable_key(&self) -> &StableDefinitionKey {
            self.id.stable_key()
        }
        #[cfg(test)]
        pub fn occurrence(&self) -> Option<DefinitionOccurrenceId> {
            self.occurrence
        }
        pub fn declaration_span(&self) -> Span {
            self.declaration_span
        }
        #[cfg(test)]
        pub fn visibility(&self) -> Option<Visibility> {
            self.visibility
        }
    }

    #[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
    pub struct BoundDefinitionWork {
        pub modules_validated: usize,
        pub manifest_bindings_visited: usize,
        pub syntax_partition_ast_nodes_visited: usize,
        pub syntax_partitions_indexed: usize,
        pub syntax_partition_lookups: usize,
        pub top_level_occurrences_joined: usize,
        pub named_methods_issued: usize,
        pub anonymous_methods_deferred: usize,
        pub ids_issued: usize,
        pub parser_invocations: usize,
        pub ast_payload_clones: usize,
        pub source_text_clones: usize,
    }

    #[derive(Debug, Clone)]
    pub struct BoundDefinitionSet {
        source_revision: SourceRevision,
        issuer: Arc<DefinitionIssuer>,
        definitions: Arc<[BoundDefinitionRecord]>,
        #[allow(dead_code)]
        manifest_work: SemanticBindingManifestWork,
        work: BoundDefinitionWork,
    }

    /// Fail-closed outcomes at the snapshot-local to durable identity join.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub(crate) enum DefinitionIdentityJoinFailure {
        MissingSyntaxOccurrence,
        AmbiguousSyntaxOccurrence,
        DuplicateStableDefinition(StableDefinitionKey),
    }

    impl fmt::Display for DefinitionIdentityJoinFailure {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            match self {
                Self::MissingSyntaxOccurrence => {
                    formatter.write_str("semantic winner has no matching syntax occurrence")
                }
                Self::AmbiguousSyntaxOccurrence => {
                    formatter.write_str("semantic winner joins multiple syntax occurrences")
                }
                Self::DuplicateStableDefinition(key) => write!(
                    formatter,
                    "semantic bindings duplicate durable key {}::{:?}::{:?}::{}",
                    key.module().as_str(),
                    key.namespace(),
                    key.kind(),
                    key.name()
                ),
            }
        }
    }

    fn join_syntax_occurrence<'a>(
        definitions: &'a crate::DefinitionSnapshot,
        name_key: &crate::DefinitionNameKey,
        kind: DefinitionKind,
    ) -> Result<&'a crate::DefinitionRecord, DefinitionIdentityJoinFailure> {
        let mut matches = definitions
            .definitions_named(name_key)
            .filter(|record| record.kind() == kind);
        let first = matches
            .next()
            .ok_or(DefinitionIdentityJoinFailure::MissingSyntaxOccurrence)?;
        if matches.next().is_some() {
            return Err(DefinitionIdentityJoinFailure::AmbiguousSyntaxOccurrence);
        }
        Ok(first)
    }

    fn reject_duplicate_stable_definitions(
        records: &[BoundDefinitionRecord],
    ) -> Result<(), DefinitionIdentityJoinFailure> {
        if let Some(pair) = records
            .windows(2)
            .find(|pair| pair[0].stable_key() == pair[1].stable_key())
        {
            return Err(DefinitionIdentityJoinFailure::DuplicateStableDefinition(
                pair[0].stable_key().clone(),
            ));
        }
        Ok(())
    }

    impl BoundDefinitionSet {
        pub fn source_revision(&self) -> &SourceRevision {
            &self.source_revision
        }
        pub fn definitions(&self) -> &[BoundDefinitionRecord] {
            &self.definitions
        }
        pub fn work(&self) -> BoundDefinitionWork {
            self.work
        }

        pub(crate) fn body_owner_endpoints(&self) -> Vec<rue_air::BodyOwnerEndpoint> {
            self.definitions
                .iter()
                .enumerate()
                .filter_map(|(slot, record)| {
                    let key = record.stable_key();
                    let (kind, name, owner_name) = match key.kind() {
                        StableDefinitionKind::Function => (
                            rue_air::BodyOwnerKind::FreeFunction,
                            key.name().to_owned(),
                            None,
                        ),
                        StableDefinitionKind::Method => (
                            rue_air::BodyOwnerKind::Method,
                            key.name().to_owned(),
                            Some(key.owner()?.name().to_owned()),
                        ),
                        StableDefinitionKind::AssociatedFunction => (
                            rue_air::BodyOwnerKind::AssociatedFunction,
                            key.name().to_owned(),
                            Some(key.owner()?.name().to_owned()),
                        ),
                        StableDefinitionKind::Destructor => {
                            let owner = key.owner().map(|o| o.name()).unwrap_or(key.name());
                            (
                                rue_air::BodyOwnerKind::Destructor,
                                owner.to_owned(),
                                Some(owner.to_owned()),
                            )
                        }
                        _ => return None,
                    };
                    Some(rue_air::BodyOwnerEndpoint {
                        token: rue_air::BodyOwnerToken::new(self.issuer.id, slot as u32),
                        kind,
                        file: record.declaration_span.file_id.index(),
                        name,
                        owner_name,
                    })
                })
                .collect()
        }

        pub(crate) fn semantic_definition_endpoints(
            &self,
        ) -> Vec<rue_air::SemanticDefinitionEndpoint> {
            self.definitions
                .iter()
                .enumerate()
                .map(|(slot, record)| {
                    let key = record.stable_key();
                    rue_air::SemanticDefinitionEndpoint {
                        token: rue_air::SemanticDefinitionToken::new(self.issuer.id, slot as u32),
                        file: record.declaration_span.file_id.index(),
                        name: Arc::from(key.name()),
                        kind: key.kind(),
                        owner: key.owner().map(|owner| Arc::from(owner.name())),
                    }
                })
                .collect()
        }

        pub(crate) fn semantic_module_endpoints(
            &self,
            merged: &CanonicalMergedProgram,
        ) -> Vec<rue_air::SemanticModuleEndpoint> {
            merged
                .ast()
                .modules()
                .iter()
                .enumerate()
                .map(|(slot, module)| rue_air::SemanticModuleEndpoint {
                    token: rue_air::SemanticModuleToken::new(self.issuer.id, slot as u32),
                    file: module.file_id().index(),
                })
                .collect()
        }

        pub(crate) fn module_token_for(
            &self,
            merged: &CanonicalMergedProgram,
            module: &ModuleId,
        ) -> Result<rue_air::SemanticModuleToken, rue_air::SemanticStableResolutionFailure>
        {
            let slot = merged
                .ast()
                .modules()
                .binary_search_by(|candidate| candidate.module_id().cmp(module))
                .map_err(|_| rue_air::SemanticStableResolutionFailure::Missing)?;
            Ok(rue_air::SemanticModuleToken::new(
                self.issuer.id,
                slot as u32,
            ))
        }

        pub(crate) fn module_for_semantic_token<'a>(
            &self,
            merged: &'a CanonicalMergedProgram,
            token: rue_air::SemanticModuleToken,
        ) -> Result<&'a ModuleId, rue_air::SemanticStableResolutionFailure> {
            if token.issuer() != self.issuer.id {
                return Err(rue_air::SemanticStableResolutionFailure::ForeignIssuer);
            }
            merged
                .ast()
                .modules()
                .get(token.slot() as usize)
                .map(|module| module.module_id())
                .ok_or(rue_air::SemanticStableResolutionFailure::Missing)
        }

        #[cfg(test)]
        pub(crate) fn key_for_body_token(
            &self,
            token: rue_air::BodyOwnerToken,
        ) -> Result<&StableDefinitionKey, CompileError> {
            self.definition_for_body_token(token)
                .map(BoundDefinitionRecord::stable_key)
        }

        #[cfg(test)]
        pub(crate) fn definition_for_body_token(
            &self,
            token: rue_air::BodyOwnerToken,
        ) -> Result<&BoundDefinitionRecord, CompileError> {
            if token.issuer() != self.issuer.id {
                return Err(invalid("body owner token belongs to a foreign issuer"));
            }
            let record = self
                .definitions
                .get(token.slot() as usize)
                .ok_or_else(|| invalid("body owner token has an invalid slot"))?;
            if !matches!(
                record.stable_key().kind(),
                StableDefinitionKind::Function
                    | StableDefinitionKind::Method
                    | StableDefinitionKind::AssociatedFunction
                    | StableDefinitionKind::Destructor
            ) {
                return Err(invalid(
                    "body owner token refers to a non-body definition kind",
                ));
            }
            Ok(record)
        }

        /// Look up a definition by its stable, issuer-independent source key.
        pub fn definition_by_key(
            &self,
            key: &StableDefinitionKey,
        ) -> Option<&BoundDefinitionRecord> {
            self.definitions
                .binary_search_by(|record| record.stable_key().cmp(key))
                .ok()
                .map(|index| &self.definitions[index])
        }

        #[cfg(test)]
        pub fn definition<'a>(
            &'a self,
            id: &BoundDefinitionId,
            revision: &SourceRevision,
        ) -> Result<&'a BoundDefinitionRecord, CompileError> {
            if revision != &self.source_revision {
                return Err(invalid(
                    "bound definition lookup used a foreign source revision",
                ));
            }
            if !Arc::ptr_eq(&self.issuer, &id.issuer) {
                return Err(invalid("bound definition ID belongs to a foreign issuer"));
            }
            self.definition_by_key(&id.key)
                .ok_or_else(|| invalid("bound definition ID is absent from its issuing set"))
        }
    }

    #[cfg(test)]
    pub(crate) fn bind_canonical_definitions(
        merged: &CanonicalMergedProgram,
        rir: &CanonicalRirOutput,
        preview_features: PreviewFeatures,
        target: Target,
    ) -> MultiErrorResult<BoundDefinitionSet> {
        let imports = crate::import_graph::import_free_canonical_graph(merged.ast())?;
        bind_canonical_definitions_with_work(merged, rir, preview_features, target, &imports)
            .map(|(definitions, _)| definitions)
    }

    /// Bind once and export stable, request-independent declaration semantics
    /// before the successful binder is consumed. This performs no body analysis,
    /// second bind, syntax reconstruction, or additional RIR traversal.
    #[cfg(test)]
    pub(crate) fn bind_canonical_declaration_semantics(
        merged: &CanonicalMergedProgram,
        rir: &CanonicalRirOutput,
        preview_features: PreviewFeatures,
        target: Target,
        imports: &crate::CanonicalImportGraph,
    ) -> MultiErrorResult<(
        BoundDefinitionSet,
        Arc<[crate::DurableDeclarationSemantic]>,
        rue_air::SemanticDeclarationExportWork,
    )> {
        let bound = crate::canonical_semantic::bind_query_owned_declarations_for_test(
            merged,
            rir,
            preview_features,
            target,
            imports,
        )?;
        let manifest = bound.binding_manifest();
        let definitions = issue_bound_definitions(
            merged,
            rir.source_revision(),
            manifest.bindings(),
            manifest.work(),
        )
        .map_err(CompileErrors::from)?;
        let converted = bound
            .with_declaration_semantics(|records, work| {
                (
                    crate::durable_semantics::convert_declaration_semantics(
                        merged,
                        &definitions,
                        records,
                    ),
                    work,
                )
            })
            .map_err(|failure| {
                CompileErrors::from(invalid(&format!(
                    "durable semantic AIR export failed: {failure:?}"
                )))
            })?;
        let semantics = converted.0.map_err(|failure| {
            CompileErrors::from(invalid(&format!(
                "durable semantic conversion failed: {failure:?}"
            )))
        })?;
        Ok((definitions, semantics, converted.1))
    }

    /// Comparison-only proof seam for the durable projection/installation path.
    ///
    /// This deliberately does not cache or skip production work. It resolves an
    /// authoritative ordinary epoch, projects its durable result into fresh
    /// current-revision shells, installs atomically, and proves that re-exporting
    /// the installed epoch produces the same stable semantics before exercising
    /// body analysis.
    #[cfg(test)]
    pub(crate) fn compare_canonical_durable_declaration_install(
        merged: &CanonicalMergedProgram,
        rir: &CanonicalRirOutput,
        preview_features: PreviewFeatures,
        target: Target,
    ) -> MultiErrorResult<(crate::DurableSemanticProjectionWork, DeclarationBindingWork)> {
        let imports = crate::import_graph::import_free_canonical_graph(merged.ast())?;
        let ordinary = crate::canonical_semantic::bind_query_owned_declarations_for_test(
            merged,
            rir,
            preview_features.clone(),
            target,
            &imports,
        )?;
        let manifest = ordinary.binding_manifest();
        let definitions = issue_bound_definitions(
            merged,
            rir.source_revision(),
            manifest.bindings(),
            manifest.work(),
        )
        .map_err(CompileErrors::from)?;
        let durable = ordinary
            .with_declaration_semantics(|records, _| {
                crate::durable_semantics::convert_declaration_semantics(
                    merged,
                    &definitions,
                    records,
                )
            })
            .map_err(|failure| {
                CompileErrors::from(invalid(format!(
                    "ordinary semantic AIR export failed: {failure:?}"
                )))
            })?
            .map_err(|failure| {
                CompileErrors::from(invalid(format!(
                    "ordinary durable semantic conversion failed: {failure:?}"
                )))
            })?;
        let query_shells =
            crate::revisioned_query_database::projected_declaration_shells_for_test(merged)?;
        let shells = configure_canonical_sema(merged, rir, preview_features, target, &imports)?
            .predeclare_imported_declaration_shells(&query_shells)?;
        let shell_records = shells.declaration_shells().cloned().collect::<Vec<_>>();
        let provisional = issue_shell_definitions(merged, rir.source_revision(), &shell_records)
            .map_err(CompileErrors::from)?;
        let shells = shells
            .install_stable_identity_endpoints(
                &provisional.semantic_definition_endpoints(),
                &provisional.semantic_module_endpoints(merged),
            )
            .map_err(|failure| {
                CompileErrors::from(invalid(format!(
                    "comparison stable endpoint install failed: {failure:?}"
                )))
            })?;
        let (projected, projection_work) = crate::project_durable_declaration_semantics(
            merged,
            &definitions,
            &shell_records,
            &durable,
        )
        .map_err(|failure| {
            CompileErrors::from(invalid(format!(
                "durable semantic projection failed: {failure:?}"
            )))
        })?;
        let installed = shells
            .install_declaration_semantics(&projected)
            .map_err(|failure| {
                CompileErrors::from(invalid(format!(
                    "durable semantic installation failed: {failure:?}"
                )))
            })?;
        let installed = installed
            .install_body_owner_tokens(&definitions.body_owner_endpoints())
            .map_err(|failure| {
                CompileErrors::from(invalid(format!(
                    "comparison body-owner endpoint install failed: {failure:?}"
                )))
            })?;
        let binding_work = installed.binding_work();
        let installed_durable = installed
            .with_declaration_semantics(|records, _| {
                crate::durable_semantics::convert_declaration_semantics(
                    merged,
                    &definitions,
                    records,
                )
            })
            .map_err(|failure| {
                CompileErrors::from(invalid(format!(
                    "installed semantic AIR export failed: {failure:?}"
                )))
            })?
            .map_err(|failure| {
                CompileErrors::from(invalid(format!(
                    "installed durable semantic conversion failed: {failure:?}"
                )))
            })?;
        if installed_durable != durable {
            return Err(CompileErrors::from(invalid(
                "projected durable install changed declaration semantics",
            )));
        }
        let ordinary_bodies = ordinary.analyze_all_bodies_for_test();
        let installed_bodies = installed.analyze_all_bodies_for_test();
        match (ordinary_bodies, installed_bodies) {
            (Ok(ordinary), Ok(installed)) => {
                if format!("{:?}", ordinary.functions) != format!("{:?}", installed.functions)
                    || format!("{:?}", ordinary.warnings) != format!("{:?}", installed.warnings)
                    || ordinary.strings != installed.strings
                {
                    return Err(CompileErrors::from(invalid(
                        "projected durable install changed body or diagnostic artifacts",
                    )));
                }
            }
            (Err(ordinary), Err(installed))
                if format!("{ordinary:?}") == format!("{installed:?}") => {}
            _ => {
                return Err(CompileErrors::from(invalid(
                    "projected durable install changed body-analysis diagnostics",
                )));
            }
        }
        Ok((projection_work, binding_work))
    }

    #[cfg(test)]
    pub(crate) fn bind_canonical_definitions_with_work(
        merged: &CanonicalMergedProgram,
        rir: &CanonicalRirOutput,
        preview_features: PreviewFeatures,
        target: Target,
        imports: &crate::CanonicalImportGraph,
    ) -> MultiErrorResult<(BoundDefinitionSet, DeclarationBindingWork)> {
        let bound = crate::canonical_semantic::bind_query_owned_declarations_for_test(
            merged,
            rir,
            preview_features,
            target,
            imports,
        )?;
        let binding_work = bound.binding_work();
        let manifest = bound.binding_manifest();
        let definitions = issue_bound_definitions(
            merged,
            rir.source_revision(),
            manifest.bindings(),
            manifest.work(),
        )
        .map_err(CompileErrors::from)?;
        Ok((definitions, binding_work))
    }

    pub(crate) fn configure_canonical_sema<'a>(
        merged: &CanonicalMergedProgram,
        rir: &'a CanonicalRirOutput,
        preview_features: PreviewFeatures,
        target: Target,
        graph: &crate::CanonicalImportGraph,
    ) -> MultiErrorResult<Sema<'a>> {
        if merged.ast().source_revision() != rir.source_revision() {
            return Err(CompileErrors::from(invalid(
                "canonical syntax and RIR have different source revisions",
            )));
        }
        if !rir
            .semantic_symbols()
            .admits_exact_modules(merged.ast().modules())
        {
            return Err(CompileErrors::from(invalid(
                "canonical RIR belongs to foreign parsed module artifacts",
            )));
        }

        let root = merged
            .ast()
            .modules()
            .iter()
            .find(|module| module.module_id() == merged.ast().root())
            .expect("canonical root module is admitted");
        let metadata = rue_air::SemaMetadata::new(
            root.file_id(),
            merged
                .ast()
                .modules()
                .iter()
                .map(|module| (module.file_id(), module.physical_path().to_owned()))
                .collect(),
            merged
                .ast()
                .modules()
                .iter()
                .map(|module| (module.file_id(), module.module_id().as_str().to_owned()))
                .collect(),
        )
        .map_err(CompileErrors::from)?;
        let mut sema = Sema::new_for_target(
            rir.rir(),
            rir.semantic_symbols().interner(),
            preview_features,
            metadata,
            target,
        );
        if graph.root() != merged.ast().root() {
            return Err(CompileErrors::from(invalid(
                "canonical semantic import graph has a foreign root",
            )));
        }
        let view = CanonicalSemanticImportView { merged, graph };
        sema.set_canonical_imports(&view)
            .map_err(CompileErrors::from)?;
        sema.set_trusted_standard_library_files(
            merged
                .ast()
                .modules()
                .iter()
                .filter_map(|module| {
                    module
                        .module_id()
                        .is_trusted_standard_library()
                        .then_some(module.file_id())
                })
                .collect(),
        );
        Ok(sema)
    }

    pub(crate) fn issue_bound_definitions(
        merged: &CanonicalMergedProgram,
        revision: &SourceRevision,
        bindings: &[SemanticBinding],
        manifest_work: SemanticBindingManifestWork,
    ) -> Result<BoundDefinitionSet, CompileError> {
        let modules = merged.ast().modules();
        let by_file = modules
            .iter()
            .map(|module| (module.file_id(), module))
            .collect::<HashMap<_, _>>();
        let issuer = Arc::new(DefinitionIssuer {
            id: NEXT_DEFINITION_ISSUER.fetch_add(1, Ordering::Relaxed),
        });
        let mut records = Vec::with_capacity(bindings.len());
        let mut work = BoundDefinitionWork {
            modules_validated: modules.len(),
            manifest_bindings_visited: bindings.len(),
            anonymous_methods_deferred: manifest_work.anonymous_methods_deferred,
            ..BoundDefinitionWork::default()
        };
        let partition_by_file = modules
            .iter()
            .map(|module| {
                let index = SyntaxPartitionIndex::new(module);
                work.syntax_partition_ast_nodes_visited += index.ast_nodes_visited;
                work.syntax_partitions_indexed += index.by_identity.len();
                (module.file_id(), index)
            })
            .collect::<HashMap<_, _>>();
        for binding in bindings {
            validate_binding_shape(binding)?;
            let module = by_file.get(&binding.file_id).ok_or_else(|| {
                invalid("semantic binding references a file absent from canonical syntax")
            })?;
            if binding.declaration_span.file_id != binding.file_id {
                return Err(invalid("semantic binding span has a mismatched file ID"));
            }
            let stable_kind = binding.kind;
            let key = StableDefinitionKey::from_stable_parts(
                module.module_id().clone(),
                binding.namespace,
                stable_kind,
                binding.name.clone(),
                binding
                    .owner
                    .as_ref()
                    .map(|owner| (StableDefinitionKind::Struct, owner.clone())),
            );
            let (occurrence, visibility) = if binding.namespace == StableDefinitionNamespace::Method
            {
                work.named_methods_issued += 1;
                (
                    None,
                    Some(if binding.is_public {
                        Visibility::Public
                    } else {
                        Visibility::Private
                    }),
                )
            } else {
                let syntax_kind = syntax_kind(binding.kind);
                let namespace = if binding.kind == StableDefinitionKind::Destructor {
                    DefinitionNamespace::Destructor
                } else {
                    DefinitionNamespace::ModuleItem
                };
                let name_key = crate::DefinitionNameKey::new(
                    module.module_id().clone(),
                    namespace,
                    binding.name.as_ref(),
                );
                let winner = join_syntax_occurrence(merged.definitions(), &name_key, syntax_kind)
                    .map_err(|failure| invalid(failure.to_string()))?;
                if winner.declaration_span() != binding.declaration_span {
                    return Err(invalid(
                        "semantic winner span does not match its syntax occurrence",
                    ));
                }
                work.top_level_occurrences_joined += 1;
                let expected_public = winner.visibility() == Some(Visibility::Public);
                if binding.kind != StableDefinitionKind::Destructor
                    && binding.is_public != expected_public
                {
                    return Err(invalid(
                        "semantic winner visibility does not match its syntax occurrence",
                    ));
                }
                (Some(winner.id()), winner.visibility())
            };
            work.syntax_partition_lookups += 1;
            // Still resolved for its validation: partition_for rejects a
            // binding whose syntax occurrence cannot be partitioned, even
            // though the partition itself is no longer retained.
            partition_by_file
                .get(&binding.file_id)
                .expect("syntax partition index covers every canonical module")
                .partition_for(binding)?;
            records.push(BoundDefinitionRecord {
                id: SnapshotBoundDefinitionId {
                    key,
                    issuer: issuer.clone(),
                },
                occurrence,
                declaration_span: binding.declaration_span,
                visibility,
            });
        }
        records.sort_by(|left, right| left.stable_key().cmp(right.stable_key()));
        reject_duplicate_stable_definitions(&records)
            .map_err(|failure| invalid(failure.to_string()))?;
        for record in &records {
            if let Some(owner) = record.stable_key().owner() {
                let owner_key = StableDefinitionKey::from_stable_parts(
                    owner.module.clone(),
                    StableDefinitionNamespace::Type,
                    owner.kind,
                    owner.name.clone(),
                    None,
                );
                if records
                    .binary_search_by(|candidate| candidate.stable_key().cmp(&owner_key))
                    .is_err()
                {
                    return Err(invalid(
                        "member or destructor owner has no bound named-type definition",
                    ));
                }
            }
        }
        work.ids_issued = records.len();
        Ok(BoundDefinitionSet {
            source_revision: revision.clone(),
            issuer,
            definitions: records.into(),
            manifest_work,
            work,
        })
    }

    /// Issue the current revision's stable definition universe from declaration
    /// shells, before payload resolution. Shells contain the same authoritative
    /// identity, span, ownership, and visibility fields consumed by the ordinary
    /// post-binding manifest; no semantic type/value is inferred here.
    pub(crate) fn issue_shell_definitions(
        merged: &CanonicalMergedProgram,
        revision: &SourceRevision,
        shells: &[SemanticDeclarationShell],
    ) -> Result<BoundDefinitionSet, CompileError> {
        let bindings = shells
            .iter()
            // A syntax shell can only prove that a source `const` is a candidate.
            // Its final stable kind may be ValueConst or ModuleBinding after
            // initializer evaluation, so no provisional stable ID is issued.
            .filter(|shell| shell.identity.kind != StableDefinitionKind::ValueConst)
            .map(|shell| SemanticBinding {
                file_id: shell.declaration_span.file_id,
                declaration_span: shell.declaration_span,
                namespace: shell.identity.namespace,
                kind: shell.identity.kind,
                name: shell.identity.name.clone(),
                owner: shell.identity.owner.clone(),
                is_public: shell.is_public,
            })
            .collect::<Vec<_>>();
        issue_bound_definitions(
            merged,
            revision,
            &bindings,
            SemanticBindingManifestWork::default(),
        )
    }

    fn signature_fragments_excluding_method_bodies(
        structure: &rue_parser::ast::StructDecl,
    ) -> Result<Vec<Span>, CompileError> {
        let mut fragments = Vec::with_capacity(structure.methods.len() + 1);
        let mut cursor = structure.span.start;
        for method in &structure.methods {
            let body = method.body.span();
            if body.file_id != structure.span.file_id
                || body.start < cursor
                || body.end > structure.span.end
                || body.start >= body.end
            {
                return Err(invalid(
                    "struct method body span is not ordered within its declaration",
                ));
            }
            fragments.push(Span::with_file(structure.span.file_id, cursor, body.start));
            cursor = body.end;
        }
        fragments.push(Span::with_file(
            structure.span.file_id,
            cursor,
            structure.span.end,
        ));
        Ok(fragments)
    }

    fn partition_prefix(declaration: Span, payload: Span) -> Result<Span, CompileError> {
        if declaration.file_id != payload.file_id
            || payload.start < declaration.start
            || payload.end > declaration.end
            || payload.start >= payload.end
        {
            return Err(invalid(
                "semantic input payload span is not contained by its declaration",
            ));
        }
        Ok(Span::with_file(
            declaration.file_id,
            declaration.start,
            payload.start,
        ))
    }

    fn validate_binding_shape(binding: &SemanticBinding) -> Result<(), CompileError> {
        let owner_shape_matches = binding.owner.is_some() == binding.kind.requires_owner();
        let destructor_shape_matches = binding.kind != StableDefinitionKind::Destructor
            || (binding.owner.as_deref() == Some(binding.name.as_ref()) && !binding.is_public);
        let valid = binding.namespace == binding.kind.namespace()
            && owner_shape_matches
            && destructor_shape_matches;
        if valid {
            Ok(())
        } else {
            Err(invalid(
                "semantic binding has an impossible namespace/kind/owner shape",
            ))
        }
    }

    fn syntax_kind(value: StableDefinitionKind) -> DefinitionKind {
        match value {
            StableDefinitionKind::Function => DefinitionKind::Function,
            StableDefinitionKind::Struct => DefinitionKind::Struct,
            StableDefinitionKind::Enum => DefinitionKind::Enum,
            StableDefinitionKind::ValueConst | StableDefinitionKind::ModuleBinding => {
                DefinitionKind::Const
            }
            StableDefinitionKind::Destructor => DefinitionKind::Destructor,
            StableDefinitionKind::Method | StableDefinitionKind::AssociatedFunction => {
                unreachable!("methods have no top-level syntax occurrence")
            }
        }
    }

    fn invalid(message: impl Into<String>) -> CompileError {
        CompileError::without_span(ErrorKind::InvalidCompilerInput(message.into()))
    }

    #[cfg(test)]
    mod tests {
        use std::collections::HashMap;
        use std::fmt::Write as _;
        use std::hash::Hasher;

        use rue_error::PreviewFeature;
        use rue_span::FileId;

        use super::*;
        use crate::{SourceMetadata, SourceSnapshot};

        #[derive(Default)]
        struct ByteCountingHasher {
            bytes: usize,
        }

        impl Hasher for ByteCountingHasher {
            fn finish(&self) -> u64 {
                self.bytes as u64
            }

            fn write(&mut self, bytes: &[u8]) {
                self.bytes += bytes.len();
            }
        }

        #[test]
        fn stable_definition_hashing_is_constant_size_and_collision_safe() {
            let module = ModuleId::from_logical_path("a/very/long/module/path").unwrap();
            let first = StableDefinitionKey::for_test(
                module.clone(),
                StableDefinitionNamespace::Value,
                StableDefinitionKind::Function,
                "a_very_long_function_name",
                None,
            );
            let cloned = first.clone();
            assert!(Arc::ptr_eq(&first.0, &cloned.0));
            assert_eq!(first.cmp(&cloned), std::cmp::Ordering::Equal);

            let independently_issued_equal = StableDefinitionKey::for_test(
                module.clone(),
                StableDefinitionNamespace::Value,
                StableDefinitionKind::Function,
                "a_very_long_function_name",
                None,
            );
            assert!(!Arc::ptr_eq(&first.0, &independently_issued_equal.0));
            assert_eq!(first, independently_issued_equal);
            assert_eq!(
                first.cmp(&independently_issued_equal),
                std::cmp::Ordering::Equal
            );

            let mut hasher = ByteCountingHasher::default();
            first.hash(&mut hasher);
            assert_eq!(hasher.bytes, 16);

            let mut second = StableDefinitionKey::for_test(
                module,
                StableDefinitionNamespace::Value,
                StableDefinitionKind::Function,
                "another_function",
                None,
            );
            Arc::get_mut(&mut second.0).unwrap().hash_accelerator = first.0.hash_accelerator;
            assert_ne!(first, second);
            assert_ne!(first.cmp(&second), std::cmp::Ordering::Equal);
            assert_eq!(first.cmp(&second), second.cmp(&first).reverse());

            let mut colliding = HashMap::new();
            colliding.insert(first, 1);
            colliding.insert(second, 2);
            assert_eq!(colliding.len(), 2);
        }

        fn snapshot(entries: &[(u32, &str, &str, &str)], root: u32) -> SourceSnapshot {
            let physical = entries
                .iter()
                .map(|(id, path, _, _)| (FileId::new(*id), (*path).to_owned()))
                .collect::<HashMap<_, _>>();
            let logical = entries
                .iter()
                .map(|(id, _, logical, _)| (FileId::new(*id), (*logical).to_owned()))
                .collect::<HashMap<_, _>>();
            let metadata = SourceMetadata::new(FileId::new(root), physical, logical).unwrap();
            SourceSnapshot::new(
                metadata,
                entries
                    .iter()
                    .map(|(id, _, _, text)| (FileId::new(*id), Arc::new((*text).to_owned())))
                    .collect(),
            )
            .unwrap()
        }

        fn bind(snapshot: &SourceSnapshot) -> BoundDefinitionSet {
            let stages = crate::test_support::test_frontend_stages(snapshot).unwrap();
            let merged = &stages.merged;
            let rir = &stages.rir;
            bind_canonical_definitions(merged, rir, PreviewFeatures::new(), Target::default())
                .unwrap()
        }

        fn export(
            snapshot: &SourceSnapshot,
        ) -> (
            BoundDefinitionSet,
            Arc<[crate::DurableDeclarationSemantic]>,
            rue_air::SemanticDeclarationExportWork,
        ) {
            let stages = crate::test_support::test_frontend_stages(snapshot).unwrap();
            let merged = &stages.merged;
            let rir = &stages.rir;
            let imports = crate::import_graph::import_free_canonical_graph(merged.ast()).unwrap();
            bind_canonical_declaration_semantics(
                merged,
                rir,
                PreviewFeatures::new(),
                Target::default(),
                &imports,
            )
            .unwrap()
        }

        fn keys(set: &BoundDefinitionSet) -> Vec<StableDefinitionKey> {
            set.definitions()
                .iter()
                .map(|record| record.stable_key().clone())
                .collect()
        }

        fn compare(
            snapshot: &SourceSnapshot,
        ) -> (crate::DurableSemanticProjectionWork, DeclarationBindingWork) {
            let stages = crate::test_support::test_frontend_stages(snapshot).unwrap();
            let merged = &stages.merged;
            let rir = &stages.rir;
            compare_canonical_durable_declaration_install(
                merged,
                rir,
                PreviewFeatures::new(),
                Target::default(),
            )
            .unwrap()
        }

        #[test]
        fn canonical_semantic_import_join_is_logarithmic_and_preserves_source_order() {
            const DIRECTIVES: usize = 2_048;
            let mut root_source = String::new();
            for index in (0..DIRECTIVES).rev() {
                writeln!(
                    root_source,
                    "const imported_{index:04} = @import(\"module_{index:04}.rue\");"
                )
                .unwrap();
            }
            root_source.push_str("fn main() {}");

            let mut owned = Vec::with_capacity(DIRECTIVES + 1);
            owned.push((
                1_u32,
                "/src/main.rue".to_owned(),
                "main.rue".to_owned(),
                root_source,
            ));
            for index in 0..DIRECTIVES {
                owned.push((
                    index as u32 + 2,
                    format!("/src/module_{index:04}.rue"),
                    format!("module_{index:04}.rue"),
                    String::new(),
                ));
            }
            let borrowed = owned
                .iter()
                .map(|(id, physical, logical, source)| {
                    (*id, physical.as_str(), logical.as_str(), source.as_str())
                })
                .collect::<Vec<_>>();
            let input = snapshot(&borrowed, 1);
            let stages = crate::test_support::test_frontend_stages(&input).unwrap();
            let graph = crate::test_support::test_import_graph(&input).unwrap();
            assert_eq!(graph.records().len(), DIRECTIVES);

            let view = CanonicalSemanticImportView {
                merged: &stages.merged,
                graph: &graph,
            };
            let mut visited = Vec::with_capacity(DIRECTIVES);
            view.visit_resolved_sites(&mut |_, source_offset, specifier, target| {
                visited.push((source_offset, specifier.to_owned(), target.to_owned()));
                Ok(())
            })
            .unwrap();

            assert_eq!(visited.len(), DIRECTIVES);
            assert!(
                visited.windows(2).all(|pair| pair[0].0 < pair[1].0),
                "resolved sites retain directive source order"
            );
            assert_eq!(visited[0].1, "module_2047.rue");
            assert_eq!(visited[DIRECTIVES - 1].1, "module_0000.rue");
            let comparisons = stages
                .merged
                .ast()
                .import_directives()
                .iter()
                .map(|directive| {
                    let normalized = rue_air::normalize_module_path(directive.specifier());
                    let (record, comparisons) =
                        graph.record_for_with_comparison_count(directive.importer(), &normalized);
                    assert!(record.is_some());
                    comparisons
                })
                .sum::<usize>();
            let comparisons_per_lookup =
                (usize::BITS - (DIRECTIVES - 1).leading_zeros()) as usize + 1;
            assert!(
                comparisons <= DIRECTIVES * comparisons_per_lookup,
                "{} comparisons exceeded {} logarithmic lookup probes",
                comparisons,
                DIRECTIVES * comparisons_per_lookup
            );
        }

        #[test]
        fn canonical_semantic_import_join_still_rejects_a_missing_record() {
            let input = snapshot(
                &[
                    (
                        1,
                        "/src/main.rue",
                        "main.rue",
                        "const imported = @import(\"module.rue\"); fn main() {}",
                    ),
                    (2, "/src/module.rue", "module.rue", ""),
                ],
                1,
            );
            let stages = crate::test_support::test_frontend_stages(&input).unwrap();
            let incomplete = crate::CanonicalImportGraph::from_discovery_records(
                stages.merged.ast().root().clone(),
                Vec::new(),
            );
            let view = CanonicalSemanticImportView {
                merged: &stages.merged,
                graph: &incomplete,
            };
            let error = view
                .visit_resolved_sites(&mut |_, _, _, _| Ok(()))
                .unwrap_err();
            assert!(matches!(
                error.kind,
                ErrorKind::InvalidCompilerInput(ref message)
                    if message == "validated canonical graph does not cover a parsed import directive"
            ));
        }

        #[test]
        fn canonical_module_tokens_use_the_sorted_module_slot() {
            let input = snapshot(
                &[
                    (
                        1,
                        "/src/main.rue",
                        "main.rue",
                        "const a = @import(\"a.rue\"); const z = @import(\"z.rue\"); fn main() {}",
                    ),
                    (2, "/src/a.rue", "a.rue", ""),
                    (3, "/src/z.rue", "z.rue", ""),
                ],
                1,
            );
            let stages = crate::test_support::test_frontend_stages(&input).unwrap();
            let graph = crate::test_support::test_import_graph(&input).unwrap();
            let (definitions, _) = bind_canonical_definitions_with_work(
                &stages.merged,
                &stages.rir,
                PreviewFeatures::new(),
                Target::default(),
                &graph,
            )
            .unwrap();

            for (slot, module) in stages.merged.ast().modules().iter().enumerate() {
                let token = definitions
                    .module_token_for(&stages.merged, module.module_id())
                    .unwrap();
                assert_eq!(token.slot() as usize, slot);
                assert_eq!(
                    definitions
                        .module_for_semantic_token(&stages.merged, token)
                        .unwrap(),
                    module.module_id()
                );
            }
            let missing = ModuleId::from_logical_path("missing.rue").unwrap();
            assert_eq!(
                definitions.module_token_for(&stages.merged, &missing),
                Err(rue_air::SemanticStableResolutionFailure::Missing)
            );

            let source = include_str!("bound_definitions.rs");
            let method = source
                .split_once("pub(crate) fn module_token_for(")
                .unwrap()
                .1
                .split_once("pub(crate) fn module_for_semantic_token")
                .unwrap()
                .0;
            assert!(method.contains(".binary_search_by("));
            assert!(!method.contains(".position("));
        }

        #[test]
        fn syntax_partition_join_is_linear_for_functions_methods_and_externs() {
            const EACH_KIND: usize = 512;
            let mut source = String::new();
            for index in 0..EACH_KIND {
                writeln!(source, "fn ordinary_{index:04}() -> i32 {{ {index} }}").unwrap();
            }
            source.push_str("struct Methods {\n");
            for index in 0..EACH_KIND {
                writeln!(source, "fn method_{index:04}(self) -> i32 {{ {index} }}").unwrap();
            }
            source.push_str("}\nextern \"C\" {\n");
            for index in 0..EACH_KIND {
                writeln!(source, "fn foreign_{index:04}() -> i32;").unwrap();
            }
            source.push_str("}\nfn main() {}\n");

            let input = snapshot(&[(1, "/main.rue", "main.rue", &source)], 1);
            let stages = crate::test_support::test_frontend_stages(&input).unwrap();
            let definitions = bind_canonical_definitions(
                &stages.merged,
                &stages.rir,
                PreviewFeatures::from([PreviewFeature::CFfi]),
                Target::default(),
            )
            .unwrap();
            let work = definitions.work();
            let expected_definitions = EACH_KIND * 3 + 2;

            assert_eq!(definitions.definitions().len(), expected_definitions);
            assert_eq!(work.syntax_partitions_indexed, expected_definitions);
            assert_eq!(work.syntax_partition_lookups, expected_definitions);
            assert_eq!(
                work.syntax_partition_ast_nodes_visited,
                expected_definitions + 1,
                "the extern block is the only visited AST node without its own partition"
            );
            assert!(
                work.syntax_partition_ast_nodes_visited + work.syntax_partition_lookups
                    <= expected_definitions * 2 + 1
            );
        }

        #[test]
        fn projected_install_matches_ordinary_across_relocation_order_modules_methods_and_drop() {
            let root = r#"
            struct Resource {
                value: i32,
                fn get(self) -> i32 { self.value }
                fn make(value: i32) -> Resource { Resource { value } }
            }
            enum Choice { None, Some(Resource) }
            drop fn Resource(self) {}
            fn main() -> i32 { Resource.make(1).get() }
        "#;
            let sibling = "struct Sibling { value: bool } fn helper(value: i32) -> i32 { value }";
            let first = snapshot(
                &[
                    (9, "/old/sibling.rue", "sibling.rue", sibling),
                    (2, "/old/main.rue", "main.rue", root),
                ],
                2,
            );
            let relocated = snapshot(
                &[
                    (71, "/new/main.rue", "main.rue", root),
                    (4, "/new/sibling.rue", "sibling.rue", sibling),
                ],
                71,
            );
            for input in [&first, &relocated] {
                let (projection, install) = compare(input);
                assert_eq!(projection.projection_invocations, 1);
                assert_eq!(projection.rir_instructions_visited, 0);
                assert_eq!(
                    projection.definition_records_indexed,
                    projection.shells_visited
                );
                assert_eq!(
                    projection.definition_lookup_probes,
                    projection.shells_visited
                );
                assert_eq!(
                    projection.shells_visited,
                    projection.durable_records_visited
                );
                assert_eq!(install.declaration_resolution_invocations, 0);
                assert_eq!(install.durable_install_invocations, 1);
                assert_eq!(
                    install.durable_payloads_installed,
                    projection.shells_visited
                );
            }

            let (_, old_durable, _) = export(&first);
            let stages = crate::test_support::test_frontend_stages(&relocated).unwrap();
            let merged = &stages.merged;
            let rir = &stages.rir;
            let current_definitions = bind(&relocated);
            let imports = crate::import_graph::import_free_canonical_graph(merged.ast()).unwrap();
            let shells = crate::canonical_semantic::query_owned_declaration_shells_for_test(
                merged,
                rir,
                PreviewFeatures::new(),
                Target::default(),
                &imports,
            )
            .unwrap();
            let shell_records = shells.declaration_shells().cloned().collect::<Vec<_>>();
            let (projected, work) = crate::project_durable_declaration_semantics(
                merged,
                &current_definitions,
                &shell_records,
                &old_durable,
            )
            .unwrap();
            assert_eq!(projected.len(), old_durable.len());
            assert_eq!(work.definition_records_indexed, old_durable.len());
            assert_eq!(work.definition_lookup_probes, old_durable.len());
        }

        #[test]
        fn projection_definition_join_work_is_linear_for_128_modules() {
            let owned = (0..128_u32)
                .map(|index| {
                    let id = index + 1;
                    let physical = format!("/src/module_{index}.rue");
                    let logical = format!("module_{index}.rue");
                    let source = if index == 0 {
                        "fn main() -> i32 { 0 }".to_owned()
                    } else {
                        format!("fn f{index}() -> i32 {{ {index} }}")
                    };
                    (id, physical, logical, source)
                })
                .collect::<Vec<_>>();
            let borrowed = owned
                .iter()
                .map(|(id, physical, logical, source)| {
                    (*id, physical.as_str(), logical.as_str(), source.as_str())
                })
                .collect::<Vec<_>>();
            let input = snapshot(&borrowed, 1);
            let (definitions, durable, _) = export(&input);
            let stages = crate::test_support::test_frontend_stages(&input).unwrap();
            let merged = &stages.merged;
            let rir = &stages.rir;
            let imports = crate::import_graph::import_free_canonical_graph(merged.ast()).unwrap();
            let shells = crate::canonical_semantic::query_owned_declaration_shells_for_test(
                merged,
                rir,
                PreviewFeatures::new(),
                Target::default(),
                &imports,
            )
            .unwrap();
            let shell_records = shells.declaration_shells().cloned().collect::<Vec<_>>();
            let (projected, projection) = crate::project_durable_declaration_semantics(
                merged,
                &definitions,
                &shell_records,
                &durable,
            )
            .unwrap();

            assert_eq!(projection.shells_visited, 128);
            assert_eq!(projection.durable_records_visited, 128);
            assert_eq!(projection.definition_records_indexed, 128);
            assert_eq!(projection.definition_lookup_probes, 128);
            assert_eq!(projection.rir_instructions_visited, 0);
            assert_eq!(projected.len(), 128);
        }

        #[test]
        fn duplicate_projection_input_fails_before_shells_are_consumed() {
            let input = snapshot(&[(1, "/main.rue", "main.rue", "fn main() -> i32 { 0 }")], 1);
            let (definitions, durable, _) = export(&input);
            let stages = crate::test_support::test_frontend_stages(&input).unwrap();
            let merged = &stages.merged;
            let rir = &stages.rir;
            let imports = crate::import_graph::import_free_canonical_graph(merged.ast()).unwrap();
            let shells = crate::canonical_semantic::query_owned_declaration_shells_for_test(
                merged,
                rir,
                PreviewFeatures::new(),
                Target::default(),
                &imports,
            )
            .unwrap();
            let shell_records = shells.declaration_shells().cloned().collect::<Vec<_>>();
            let duplicated = durable
                .iter()
                .cloned()
                .chain(durable.iter().cloned())
                .collect::<Vec<_>>();

            assert_eq!(
                crate::project_durable_declaration_semantics(
                    merged,
                    &definitions,
                    &shell_records,
                    &duplicated,
                )
                .unwrap_err(),
                crate::DurableSemanticProjectionFailure::DuplicateDefinition
            );
            let (projected, _) = crate::project_durable_declaration_semantics(
                merged,
                &definitions,
                &shell_records,
                &durable,
            )
            .unwrap();
            let installed = shells.install_declaration_semantics(&projected).unwrap();
            assert_eq!(installed.binding_work().durable_payloads_installed, 1);
        }

        #[test]
        fn const_projection_installs_without_partial_fallback() {
            let input = snapshot(
                &[(
                    1,
                    "/main.rue",
                    "main.rue",
                    "const X: i32 = 1; fn main() -> i32 { X }",
                )],
                1,
            );
            let stages = crate::test_support::test_frontend_stages(&input).unwrap();
            let merged = &stages.merged;
            let rir = &stages.rir;
            compare_canonical_durable_declaration_install(
                merged,
                rir,
                PreviewFeatures::new(),
                Target::default(),
            )
            .unwrap();
            // A fresh oracle epoch remains valid after the query-owned install.
            let imports = crate::import_graph::import_free_canonical_graph(merged.ast()).unwrap();
            crate::canonical_semantic::bind_query_owned_declarations_for_test(
                merged,
                rir,
                PreviewFeatures::new(),
                Target::default(),
                &imports,
            )
            .unwrap()
            .analyze_all_bodies_for_test()
            .unwrap();
        }

        const PROGRAM: &str = r#"
        struct Resource {
            value: i32,
            fn get(self) -> i32 { self.value }
            fn make() -> Resource { Resource { value: 0 } }
        }
        enum Choice { None, Some(i32) }
        const LIMIT: i32 = 4;
        drop fn Resource(self) {}
        fn main() -> i32 { Resource.make().get() + LIMIT }
    "#;

        #[test]
        fn durable_declaration_export_is_relocation_and_order_stable_without_extra_rir() {
            let first = snapshot(
                &[
                    (
                        9,
                        "/old/z.rue",
                        "z.rue",
                        "fn helper(x: ptr const i32) -> bool { true } const alias = helper;",
                    ),
                    (2, "/old/main.rue", "main.rue", PROGRAM),
                ],
                2,
            );
            let moved = snapshot(
                &[
                    (71, "/new/main.rue", "main.rue", PROGRAM),
                    (
                        4,
                        "/new/z.rue",
                        "z.rue",
                        "fn helper(x: ptr const i32) -> bool { true } const alias = helper;",
                    ),
                ],
                71,
            );
            let (_, first, first_work) = export(&first);
            let (_, moved, moved_work) = export(&moved);
            assert_eq!(first, moved);
            assert_eq!(first_work, moved_work);
            assert_eq!(first_work.build_invocations, 1);
            assert_eq!(first_work.rir_instructions_visited, 0);
            assert!(first.iter().any(|record| matches!(
                record.payload,
                crate::DurableDeclarationPayload::Const {
                    value: crate::DurableConstValue::Function(_),
                    ..
                }
            )));
        }

        #[test]
        fn durable_export_keeps_a_real_root_file_zero_nominal_distinct_from_builtins() {
            let source = snapshot(
                &[(
                    0,
                    "/workspace/main.rue",
                    "main.rue",
                    "struct Root { value: i32 } fn make() -> Root { Root { value: 0 } }",
                )],
                0,
            );
            let (_, declarations, _) = export(&source);
            let root = declarations
                .iter()
                .find(|declaration| declaration.key.name() == "Root")
                .expect("root nominal must be durably exported")
                .key
                .clone();
            let make = declarations
                .iter()
                .find(|declaration| declaration.key.name() == "make")
                .expect("function returning the root nominal must be durably exported");
            assert!(matches!(
                &make.payload,
                crate::DurableDeclarationPayload::Callable { result, .. }
                    if result == &crate::DurableType::Nominal(root)
            ));
        }

        #[test]
        fn durable_module_value_export_joins_logical_identity_after_physical_relocation() {
            let source = snapshot(
                &[
                    (
                        0,
                        "/relocated/project/main.rue",
                        "main.rue",
                        "const imported = @import(\"lib.rue\"); fn main() -> i32 { 0 }",
                    ),
                    (
                        9,
                        "/relocated/project/lib.rue",
                        "lib.rue",
                        "fn value() -> i32 { 1 }",
                    ),
                ],
                0,
            );
            let merged = crate::test_support::test_merged_program(&source).unwrap();
            let rue_air::SemanticExportType::Module(logical_identity) =
                rue_air::SemanticExportType::Module(Arc::from("lib.rue"))
            else {
                unreachable!()
            };
            assert_eq!(
                crate::durable_semantics::durable_module_type(&logical_identity, &merged),
                Ok(crate::DurableType::Module(
                    crate::ModuleId::from_logical_path("lib.rue").unwrap(),
                ))
            );
            assert_eq!(
                crate::durable_semantics::durable_module_type(
                    "/relocated/project/lib.rue",
                    &merged,
                ),
                Err(crate::durable_semantics::DurableSemanticExportFailure::UnresolvedModule)
            );
        }

        #[test]
        fn durable_member_export_joins_same_named_members_through_their_stable_owner() {
            const MEMBERS: &str = r#"
            struct Alpha {
                fn shared(self) -> i32 { 1 }
                fn make() -> i32 { 2 }
            }
            struct Beta {
                fn shared(self) -> bool { true }
                fn make() -> bool { false }
            }
            fn main() {}
        "#;
            let first = snapshot(
                &[
                    (9, "/old/z.rue", "z.rue", "fn helper() {}"),
                    (2, "/old/main.rue", "main.rue", MEMBERS),
                ],
                2,
            );
            let moved = snapshot(
                &[
                    (71, "/new/main.rue", "main.rue", MEMBERS),
                    (4, "/new/z.rue", "z.rue", "fn helper() {}"),
                ],
                71,
            );
            let (_, first, _) = export(&first);
            let (_, moved, _) = export(&moved);
            assert_eq!(first, moved);

            let members = first
                .iter()
                .filter(|record| {
                    matches!(
                        record.key.kind(),
                        StableDefinitionKind::Method | StableDefinitionKind::AssociatedFunction
                    )
                })
                .map(|record| {
                    (
                        record.key.kind(),
                        record.key.name().to_owned(),
                        record.key.owner().unwrap().name().to_owned(),
                        record.payload.clone(),
                    )
                })
                .collect::<Vec<_>>();
            assert_eq!(members.len(), 4);
            for name in ["make", "shared"] {
                let same_name = members
                    .iter()
                    .filter(|member| member.1 == name)
                    .collect::<Vec<_>>();
                assert_eq!(same_name.len(), 2);
                assert_ne!(same_name[0].2, same_name[1].2);
                assert_ne!(same_name[0].3, same_name[1].3);
            }
        }

        #[test]
        fn stable_keys_ignore_relocation_file_ids_and_batch_order() {
            let first = snapshot(
                &[
                    (9, "/old/z.rue", "z.rue", "fn helper() -> i32 { 1 }"),
                    (2, "/old/main.rue", "main.rue", PROGRAM),
                ],
                2,
            );
            let second = snapshot(
                &[
                    (71, "/new/main.rue", "main.rue", PROGRAM),
                    (4, "/new/z.rue", "z.rue", "fn helper() -> i32 { 1 }"),
                ],
                71,
            );
            let first = bind(&first);
            let second = bind(&second);
            assert_eq!(first.source_revision(), second.source_revision());
            assert_eq!(keys(&first), keys(&second));
            assert_eq!(first.work(), second.work());
        }

        /// `AmbiguousSyntaxOccurrence` is defensive: merge refuses a program with
        /// duplicate names, so a definition snapshot never carries two records for
        /// one name key. The reachable join outcomes are asserted here; the
        /// duplicate program is asserted to be refused instead of joined.
        #[test]
        fn snapshot_to_durable_join_reports_missing_and_duplicate_outcomes_by_type() {
            let input = snapshot(
                &[(
                    1,
                    "/main.rue",
                    "main.rue",
                    "fn repeated() {} fn repeated() {} fn main() {}",
                )],
                1,
            );
            assert!(crate::test_support::test_merged_program(&input).is_err());

            let merged = crate::test_support::test_merged_program(&snapshot(
                &[(1, "/main.rue", "main.rue", "fn once() {}")],
                1,
            ))
            .unwrap();
            let definitions = merged.definitions();
            let once = crate::DefinitionNameKey::new(
                crate::ModuleId::from_logical_path("main.rue").unwrap(),
                DefinitionNamespace::ModuleItem,
                "once",
            );
            join_syntax_occurrence(definitions, &once, DefinitionKind::Function).unwrap();
            assert_eq!(
                join_syntax_occurrence(definitions, &once, DefinitionKind::Struct).unwrap_err(),
                DefinitionIdentityJoinFailure::MissingSyntaxOccurrence
            );

            let set = bind(&snapshot(&[(7, "/one.rue", "one.rue", "fn main() {}")], 7));
            let record = set.definitions()[0].clone();
            let duplicate_key = record.stable_key().clone();
            assert_eq!(
                reject_duplicate_stable_definitions(&[record.clone(), record]).unwrap_err(),
                DefinitionIdentityJoinFailure::DuplicateStableDefinition(duplicate_key)
            );
        }

        #[test]
        fn rename_and_module_move_change_only_the_stable_identity_components() {
            let original = bind(&snapshot(&[(1, "/a.rue", "a.rue", "fn main() {}")], 1));
            let renamed = bind(&snapshot(&[(7, "/a.rue", "a.rue", "fn renamed() {}")], 7));
            let moved = bind(&snapshot(&[(8, "/b.rue", "b.rue", "fn main() {}")], 8));
            assert_ne!(keys(&original), keys(&renamed));
            assert_ne!(keys(&original), keys(&moved));
            assert_eq!(original.definitions()[0].stable_key().name(), "main");
            assert_eq!(
                moved.definitions()[0].stable_key().module().as_str(),
                "b.rue"
            );
        }

        #[test]
        fn issuer_and_revision_provenance_fail_closed() {
            let source = snapshot(&[(1, "/a.rue", "a.rue", "fn main() {}")], 1);
            let first = bind(&source);
            let second = bind(&source);
            let first_id = first.definitions()[0].id().clone();
            assert_eq!(first_id, first_id.clone());
            assert_ne!(first_id, second.definitions()[0].id().clone());
            assert!(
                second
                    .definition(&first_id, second.source_revision())
                    .is_err()
            );

            let foreign = bind(&snapshot(&[(2, "/a.rue", "a.rue", "fn renamed() {}")], 2));
            assert!(
                first
                    .definition(&first_id, foreign.source_revision())
                    .is_err()
            );
            assert!(first.definition(&first_id, first.source_revision()).is_ok());

            let first_token = first.body_owner_endpoints()[0].token;
            let second_token = second.body_owner_endpoints()[0].token;
            assert!(first.key_for_body_token(first_token).is_ok());
            assert_eq!(
                first
                    .definition_for_body_token(first_token)
                    .unwrap()
                    .stable_key(),
                first.key_for_body_token(first_token).unwrap(),
            );
            assert!(first.key_for_body_token(second_token).is_err());
            assert!(
                first
                    .key_for_body_token(rue_air::BodyOwnerToken::new(
                        first_token.issuer(),
                        u32::MAX,
                    ))
                    .is_err()
            );
        }

        #[test]
        fn occurrences_methods_owners_and_metrics_are_retained_separately() {
            let set = bind(&snapshot(&[(1, "/main.rue", "main.rue", PROGRAM)], 1));
            let method = set
                .definitions()
                .iter()
                .find(|record| record.stable_key().name() == "get")
                .unwrap();
            assert_eq!(method.stable_key().kind(), StableDefinitionKind::Method);
            assert_eq!(method.stable_key().owner().unwrap().name(), "Resource");
            assert!(method.occurrence().is_none());
            assert_eq!(method.visibility(), Some(Visibility::Private));
            assert!(
                set.definitions()
                    .iter()
                    .filter(|record| record.stable_key().namespace()
                        != StableDefinitionNamespace::Method)
                    .all(|record| record.occurrence().is_some())
            );
            assert_eq!(set.work().ids_issued, set.definitions().len());
            assert_eq!(set.work().named_methods_issued, 2);
            assert_eq!(set.work().anonymous_methods_deferred, 0);
            assert_eq!(set.work().parser_invocations, 0);
            assert_eq!(set.work().ast_payload_clones, 0);
            assert_eq!(set.work().source_text_clones, 0);
        }

        #[test]
        fn semantic_rejection_and_foreign_canonical_artifacts_issue_no_ids() {
            let collision = snapshot(
                &[(
                    1,
                    "/main.rue",
                    "main.rue",
                    "const same: i32 = 1; fn same() {} fn main() {}",
                )],
                1,
            );
            let stages = crate::test_support::test_frontend_stages(&collision).unwrap();
            let merged = &stages.merged;
            let rir = &stages.rir;
            assert!(
                bind_canonical_definitions(merged, rir, PreviewFeatures::new(), Target::default())
                    .is_err()
            );

            let source = snapshot(&[(2, "/main.rue", "main.rue", "fn main() {}")], 2);
            let first = crate::test_support::test_frontend_stages(&source).unwrap();
            let second = crate::test_support::test_frontend_stages(&source).unwrap();
            let first = &first.merged;
            let foreign_rir = &second.rir;
            assert!(
                bind_canonical_definitions(
                    first,
                    foreign_rir,
                    PreviewFeatures::new(),
                    Target::default()
                )
                .is_err()
            );
        }

        #[test]
        fn bound_definition_set_is_send_and_sync() {
            fn assert_send_sync<T: Send + Sync>() {}
            assert_send_sync::<BoundDefinitionSet>();
        }

        #[test]
        fn authoritative_payload_partition_rejects_foreign_reversed_and_empty_spans() {
            let declaration = Span::with_file(FileId::new(1), 10, 30);
            assert!(
                partition_prefix(declaration, Span::with_file(FileId::new(2), 20, 25)).is_err()
            );
            assert!(
                partition_prefix(declaration, Span::with_file(FileId::new(1), 25, 20)).is_err()
            );
            assert!(
                partition_prefix(declaration, Span::with_file(FileId::new(1), 20, 20)).is_err()
            );
            assert!(partition_prefix(declaration, Span::with_file(FileId::new(1), 9, 20)).is_err());
            assert!(
                partition_prefix(declaration, Span::with_file(FileId::new(1), 20, 31)).is_err()
            );
        }
    }
}
