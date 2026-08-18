//! Canonical body to body-local AIR materialization.
//!
//! This adapter joins one query-owned body with exact declaration, anonymous
//! nominal, nominal-metadata, callable-symbol, and module facts. It deliberately
//! owns no retained state: request-local lookup preparation is discarded after
//! CFG keys are assembled, the returned AIR carries its issuing pool/interner,
//! and the temporary import epoch is consumed by the request. Target and preview
//! configuration are absent because relocation of already-analyzed AIR is
//! configuration-neutral; they remain part of the downstream CFG query key.

use std::hash::Hash;
use std::sync::Arc;

use ahash::AHashSet;

use crate::durable_semantics::{
    DurableAnonymousNominal, DurableAnonymousNominalShape, DurableDeclarationPayload,
    DurableDeclarationSemantic,
};
use crate::retained_charge::RetainedCharge;
use crate::{FunctionInstanceKey, ModuleId, StableDefinitionKey, StableDefinitionKind};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct LocalCallableFact {
    pub(crate) identity: FunctionInstanceKey,
    pub(crate) symbol: Arc<str>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct LocalFactSelectionIndexWork {
    pub(crate) declarations_scanned: usize,
    pub(crate) anonymous_nominals_scanned: usize,
    pub(crate) type_nodes_scanned: usize,
}

/// Immutable lookup substrate derived with one semantic-nucleus projection.
///
/// Reachability and every CFG input reuse this index over exact query-owned
/// facts. It is never semantic or query authority: selected facts are still
/// cloned into each exact CFG memo key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SharedDeclarationFactIndex {
    declarations: std::collections::HashMap<StableDefinitionKey, usize>,
    destructors: Vec<Option<usize>>,
    slice_sources: std::collections::HashMap<Arc<str>, crate::TypeInstanceKey>,
}

impl SharedDeclarationFactIndex {
    pub(crate) fn new(declarations: &[DurableDeclarationSemantic]) -> Self {
        let mut declaration_index = std::collections::HashMap::with_capacity(declarations.len());
        let mut destructors = vec![None; declarations.len()];
        let named_types = declarations
            .iter()
            .enumerate()
            .map(|(index, declaration)| {
                (
                    (
                        declaration.key.module(),
                        declaration.key.kind(),
                        declaration.key.name(),
                    ),
                    index,
                )
            })
            .collect::<std::collections::HashMap<_, _>>();
        let mut slice_sources = std::collections::HashMap::new();
        let mut work = LocalFactSelectionIndexWork::default();
        for (index, declaration) in declarations.iter().enumerate() {
            match &declaration.payload {
                DurableDeclarationPayload::Callable {
                    parameters, result, ..
                } => {
                    for parameter in parameters.iter() {
                        collect_slice_sources(&parameter.ty, &mut slice_sources, &mut work);
                    }
                    collect_slice_sources(result, &mut slice_sources, &mut work);
                }
                DurableDeclarationPayload::Struct { fields, .. } => {
                    for (_, ty) in fields.iter() {
                        collect_slice_sources(ty, &mut slice_sources, &mut work);
                    }
                }
                DurableDeclarationPayload::Enum { variants } => {
                    for (_, fields) in variants.iter() {
                        for ty in fields.iter() {
                            collect_slice_sources(ty, &mut slice_sources, &mut work);
                        }
                    }
                }
                DurableDeclarationPayload::Const { ty, .. } => {
                    collect_slice_sources(ty, &mut slice_sources, &mut work);
                }
                _ => {}
            }
            if matches!(declaration.payload, DurableDeclarationPayload::Destructor)
                && let Some(owner) = declaration.key.owner()
                && let Some(&owner_index) =
                    named_types.get(&(owner.module(), owner.kind(), owner.name()))
            {
                destructors[owner_index] = Some(index);
            }
            declaration_index.insert(declaration.key.clone(), index);
        }
        Self {
            declarations: declaration_index,
            destructors,
            slice_sources,
        }
    }

    pub(crate) fn declaration<'facts>(
        &self,
        declarations: &'facts [DurableDeclarationSemantic],
        key: &StableDefinitionKey,
    ) -> Option<&'facts DurableDeclarationSemantic> {
        self.declarations
            .get(key)
            .and_then(|&index| declarations.get(index))
    }

    fn destructor<'facts>(
        &self,
        declarations: &'facts [DurableDeclarationSemantic],
        owner: &StableDefinitionKey,
    ) -> Option<&'facts DurableDeclarationSemantic> {
        let owner_index = *self.declarations.get(owner)?;
        let destructor_index = *self.destructors.get(owner_index)?.as_ref()?;
        declarations.get(destructor_index)
    }
}

pub(crate) struct LocalFactSelectionIndex<'facts> {
    declarations: &'facts [DurableDeclarationSemantic],
    shared: &'facts SharedDeclarationFactIndex,
    anonymous: std::collections::HashMap<
        &'facts crate::AnonymousNominalKey,
        &'facts DurableAnonymousNominal,
    >,
}

impl<'facts> LocalFactSelectionIndex<'facts> {
    pub(crate) fn new(
        shared: &'facts SharedDeclarationFactIndex,
        declarations: &'facts [DurableDeclarationSemantic],
        anonymous_nominals: &'facts [DurableAnonymousNominal],
    ) -> (Self, LocalFactSelectionIndexWork) {
        let mut work = LocalFactSelectionIndexWork::default();
        let mut anonymous = std::collections::HashMap::with_capacity(anonymous_nominals.len());
        for nominal in anonymous_nominals {
            work.anonymous_nominals_scanned += 1;
            anonymous.insert(&nominal.identity, nominal);
        }
        (
            Self {
                declarations,
                shared,
                anonymous,
            },
            work,
        )
    }

    fn declaration(&self, key: &StableDefinitionKey) -> Option<&'facts DurableDeclarationSemantic> {
        self.shared.declaration(self.declarations, key)
    }

    fn destructor(
        &self,
        owner: &StableDefinitionKey,
    ) -> Option<&'facts DurableDeclarationSemantic> {
        self.shared.destructor(self.declarations, owner)
    }
}

impl RetainedCharge for SharedDeclarationFactIndex {
    fn retained_charge(&self) -> u64 {
        self.declarations
            .retained_charge()
            .saturating_add(self.destructors.retained_charge())
            .saturating_add(self.slice_sources.retained_charge())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct LocalMaterializationIndexes {
    destructors:
        std::collections::BTreeMap<(ModuleId, StableDefinitionKind, Arc<str>), FunctionInstanceKey>,
    nominal_metadata: std::collections::BTreeMap<StableDefinitionKey, Option<rue_air::LangItem>>,
    has_ambiguous_named_destructor: bool,
    has_duplicate_nominal_metadata: bool,
}

impl LocalMaterializationIndexes {
    pub(crate) fn new(
        declarations: &[DurableDeclarationSemantic],
        nominal_metadata: &[LocalNominalMetadataFact],
    ) -> Self {
        let mut indexes = Self::default();
        for candidate in declarations {
            if !matches!(candidate.payload, DurableDeclarationPayload::Destructor) {
                continue;
            }
            let Some(owner) = candidate.key.owner() else {
                continue;
            };
            if indexes
                .destructors
                .insert(
                    (
                        owner.module().clone(),
                        owner.kind(),
                        Arc::<str>::from(owner.name()),
                    ),
                    FunctionInstanceKey::Definition(candidate.key.clone()),
                )
                .is_some()
            {
                indexes.has_ambiguous_named_destructor = true;
            }
        }
        for fact in nominal_metadata {
            if indexes
                .nominal_metadata
                .insert(fact.identity().clone(), fact.lang_item())
                .is_some()
            {
                indexes.has_duplicate_nominal_metadata = true;
            }
        }
        indexes
    }

    fn destructor(&self, owner: &StableDefinitionKey) -> Option<FunctionInstanceKey> {
        self.destructors
            .get(&(
                owner.module().clone(),
                owner.kind(),
                Arc::<str>::from(owner.name()),
            ))
            .cloned()
    }

    fn nominal_metadata(
        &self,
        identity: &StableDefinitionKey,
    ) -> Option<Option<rue_air::LangItem>> {
        self.nominal_metadata.get(identity).copied()
    }
}

impl RetainedCharge for LocalMaterializationIndexes {
    fn retained_charge(&self) -> u64 {
        self.destructors
            .values()
            .map(RetainedCharge::retained_charge)
            .sum::<u64>()
            .saturating_add(
                (self.destructors.len()
                    * std::mem::size_of::<(
                        (ModuleId, StableDefinitionKind, Arc<str>),
                        FunctionInstanceKey,
                    )>()) as u64,
            )
            .saturating_add(
                self.nominal_metadata
                    .keys()
                    .map(RetainedCharge::retained_charge)
                    .sum::<u64>(),
            )
            .saturating_add(
                (self.nominal_metadata.len() * std::mem::size_of::<Option<rue_air::LangItem>>())
                    as u64,
            )
    }
}

fn collect_slice_sources(
    ty: &crate::durable_semantics::DurableType,
    output: &mut std::collections::HashMap<Arc<str>, crate::TypeInstanceKey>,
    work: &mut LocalFactSelectionIndexWork,
) {
    use rue_air::SemanticImportType as T;
    work.type_nodes_scanned += 1;
    match ty {
        T::Slice { element, name } => {
            if let Some(element) = crate::semantic_identity::type_instance_from_semantic(element) {
                output.insert(
                    name.clone(),
                    crate::TypeInstanceKey::Slice {
                        element: Box::new(element),
                        name: name.clone(),
                    },
                );
            }
            collect_slice_sources(element, output, work);
        }
        T::Array { element, .. } | T::PtrConst(element) | T::PtrMut(element) => {
            collect_slice_sources(element, output, work)
        }
        _ => {}
    }
}

/// Exact classification projected from the selected nominal's query-owned
/// lookup/index fact. `None` is meaningful: every supplied named nominal has a
/// record, so omission cannot silently strip compiler-recognized metadata.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct LocalNominalMetadataFact {
    identity: StableDefinitionKey,
    lang_item: Option<rue_air::LangItem>,
}

impl LocalNominalMetadataFact {
    pub(crate) fn new(identity: StableDefinitionKey, lang_item: Option<rue_air::LangItem>) -> Self {
        Self {
            identity,
            lang_item,
        }
    }

    pub(crate) fn identity(&self) -> &StableDefinitionKey {
        &self.identity
    }

    pub(crate) fn lang_item(&self) -> Option<rue_air::LangItem> {
        self.lang_item
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LocalMaterializationFacts {
    pub(crate) declarations: Arc<[DurableDeclarationSemantic]>,
    pub(crate) anonymous_nominals: Arc<[DurableAnonymousNominal]>,
    pub(crate) callables: Arc<[LocalCallableFact]>,
    pub(crate) nominal_metadata: Arc<[LocalNominalMetadataFact]>,
    pub(crate) modules: Arc<[ModuleId]>,
    pub(crate) builtin_nominals: Arc<[LocalBuiltinNominalRequest]>,
    /// Exact stable types that must exist in this local epoch even when they
    /// originate in an accessor body that will be mandatorily spliced here.
    pub(crate) required_types: Arc<[crate::durable_semantics::DurableType]>,
    pub(crate) indexes: Arc<LocalMaterializationIndexes>,
}

/// Shares equal immutable slices across the fact closures of one CFG-input
/// selection pass.
///
/// `select_materialization_facts` builds a fresh closure per body: seven
/// `Arc` slices plus an index derived from two of them. Whole closures never
/// repeat — every closure seeds its own callable identity, so each is unique
/// by construction — but the slices inside them repeat heavily, because they
/// are the transitive nominal universe a body reaches rather than anything
/// about the body. On the maintained Lattice workload, across 1,280
/// selections, `declarations` and `nominal_metadata` repeat 1,150 times each
/// and `anonymous_nominals` 1,234 times.
///
/// So the sharing is per slice, not per closure. Each slice is interned by
/// exact content in its existing deterministic order, so a shared slice is
/// indistinguishable from the one each body would have built alone.
///
/// The tables are request-local and bounded by the number of distinct slices
/// one pass selects. They are deliberately neither a process-global arena nor
/// a whole-program store — nothing here outlives the selection that produced
/// it, so an edit cannot find a stale slice to reuse.
#[derive(Default)]
pub(crate) struct LocalMaterializationFactInterner {
    declarations: SliceInterner<DurableDeclarationSemantic>,
    anonymous_nominals: SliceInterner<DurableAnonymousNominal>,
    callables: SliceInterner<LocalCallableFact>,
    nominal_metadata: SliceInterner<LocalNominalMetadataFact>,
    modules: SliceInterner<ModuleId>,
    builtin_nominals: SliceInterner<LocalBuiltinNominalRequest>,
    required_types: SliceInterner<crate::durable_semantics::DurableType>,
    /// Indexes keyed by the identity of the two interned slices they are
    /// derived from. Once those slices are shared, pointer identity is an
    /// exact key: equal content is the same allocation.
    indexes: std::collections::HashMap<(usize, usize), Arc<LocalMaterializationIndexes>>,
    /// Closures selected, slices newly allocated, and slice requests served
    /// from a slice an earlier selection already built.
    pub(crate) selections: usize,
    pub(crate) allocated: usize,
    pub(crate) reused: usize,
}

/// One slice kind's request-local content table.
///
/// Bucketed by content hash; a bucket resolves collisions by exact
/// comparison, so a hash collision costs one comparison rather than sharing
/// two slices that merely hash alike.
struct SliceInterner<T> {
    buckets: std::collections::HashMap<u64, Vec<Arc<[T]>>>,
}

impl<T> Default for SliceInterner<T> {
    fn default() -> Self {
        Self {
            buckets: std::collections::HashMap::new(),
        }
    }
}

impl<T: Clone + Eq + Hash> SliceInterner<T> {
    /// Share `values` with an equal slice from an earlier selection, or adopt
    /// it as the shared copy. Returns whether the slice was reused.
    fn intern(&mut self, values: Vec<T>) -> (Arc<[T]>, bool) {
        use std::hash::Hasher;
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        values.hash(&mut hasher);
        let bucket = self.buckets.entry(hasher.finish()).or_default();
        if let Some(shared) = bucket
            .iter()
            .find(|candidate| candidate.as_ref() == values.as_slice())
        {
            return (Arc::clone(shared), true);
        }
        let shared: Arc<[T]> = values.into();
        bucket.push(Arc::clone(&shared));
        (shared, false)
    }
}

impl LocalMaterializationFactInterner {
    /// Assemble one closure, sharing every slice that an earlier selection in
    /// this pass already built.
    ///
    /// The index is derived from `declarations` and `nominal_metadata` alone,
    /// so two closures sharing both share the index too — which also skips
    /// rebuilding it, the part of assembly that walks the declarations again.
    #[allow(clippy::too_many_arguments)]
    fn assemble(
        &mut self,
        declarations: Vec<DurableDeclarationSemantic>,
        anonymous_nominals: Vec<DurableAnonymousNominal>,
        callables: Vec<LocalCallableFact>,
        nominal_metadata: Vec<LocalNominalMetadataFact>,
        modules: Vec<ModuleId>,
        builtin_nominals: Vec<LocalBuiltinNominalRequest>,
        required_types: Vec<crate::durable_semantics::DurableType>,
    ) -> LocalMaterializationFacts {
        self.selections += 1;
        let mut account = |reused: bool| {
            if reused {
                self.reused += 1;
            } else {
                self.allocated += 1;
            }
        };

        let (declarations, hit) = self.declarations.intern(declarations);
        account(hit);
        let (nominal_metadata, hit) = self.nominal_metadata.intern(nominal_metadata);
        account(hit);
        let (anonymous_nominals, hit) = self.anonymous_nominals.intern(anonymous_nominals);
        account(hit);
        let (callables, hit) = self.callables.intern(callables);
        account(hit);
        let (modules, hit) = self.modules.intern(modules);
        account(hit);
        let (builtin_nominals, hit) = self.builtin_nominals.intern(builtin_nominals);
        account(hit);
        let (required_types, hit) = self.required_types.intern(required_types);
        account(hit);

        let index_key = (
            Arc::as_ptr(&declarations).cast::<()>() as usize,
            Arc::as_ptr(&nominal_metadata).cast::<()>() as usize,
        );
        let indexes = match self.indexes.get(&index_key) {
            Some(indexes) => {
                self.reused += 1;
                Arc::clone(indexes)
            }
            None => {
                self.allocated += 1;
                let indexes = Arc::new(LocalMaterializationIndexes::new(
                    &declarations,
                    &nominal_metadata,
                ));
                self.indexes.insert(index_key, Arc::clone(&indexes));
                indexes
            }
        };

        LocalMaterializationFacts {
            indexes,
            declarations,
            anonymous_nominals,
            callables,
            nominal_metadata,
            modules,
            builtin_nominals,
            required_types,
        }
    }
}

impl LocalMaterializationFacts {
    pub(crate) fn union<'a>(facts: impl IntoIterator<Item = &'a Self>) -> Self {
        fn extend_unique<'a, T: Clone + Eq + Hash>(
            destination: &mut Vec<T>,
            seen: &mut std::collections::HashSet<&'a T>,
            source: &'a [T],
        ) {
            for value in source {
                if seen.insert(value) {
                    destination.push(value.clone());
                }
            }
        }

        let mut declarations = Vec::new();
        let mut seen_declarations = std::collections::HashSet::new();
        let mut anonymous_nominals = Vec::new();
        let mut seen_anonymous_nominals = std::collections::HashSet::new();
        let mut callables = Vec::new();
        let mut seen_callables = std::collections::HashSet::new();
        let mut nominal_metadata = Vec::new();
        let mut seen_nominal_metadata = std::collections::HashSet::new();
        let mut modules = Vec::new();
        let mut seen_modules = std::collections::HashSet::new();
        let mut builtin_nominals = Vec::new();
        let mut seen_builtin_nominals = std::collections::HashSet::new();
        let mut required_types = Vec::new();
        let mut seen_required_types = std::collections::HashSet::new();
        for facts in facts {
            extend_unique(
                &mut declarations,
                &mut seen_declarations,
                &facts.declarations,
            );
            extend_unique(
                &mut anonymous_nominals,
                &mut seen_anonymous_nominals,
                &facts.anonymous_nominals,
            );
            extend_unique(&mut callables, &mut seen_callables, &facts.callables);
            extend_unique(
                &mut nominal_metadata,
                &mut seen_nominal_metadata,
                &facts.nominal_metadata,
            );
            extend_unique(&mut modules, &mut seen_modules, &facts.modules);
            extend_unique(
                &mut builtin_nominals,
                &mut seen_builtin_nominals,
                &facts.builtin_nominals,
            );
            extend_unique(
                &mut required_types,
                &mut seen_required_types,
                &facts.required_types,
            );
        }
        let declarations: Arc<[DurableDeclarationSemantic]> = declarations.into();
        let nominal_metadata: Arc<[LocalNominalMetadataFact]> = nominal_metadata.into();
        Self {
            indexes: Arc::new(LocalMaterializationIndexes::new(
                &declarations,
                &nominal_metadata,
            )),
            declarations,
            anonymous_nominals: anonymous_nominals.into(),
            callables: callables.into(),
            nominal_metadata,
            modules: modules.into(),
            builtin_nominals: builtin_nominals.into(),
            required_types: required_types.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct LocalBuiltinNominalRequest {
    pub(crate) kind: crate::AnonymousNominalKind,
    pub(crate) name: Arc<str>,
    pub(crate) query_ty: crate::TypeInstanceKey,
}

#[derive(Debug, Clone)]
pub(crate) struct LocalBuiltinNominalFact {
    pub(crate) request: LocalBuiltinNominalRequest,
    pub(crate) facts: crate::type_queries::TypeFacts,
}

impl RetainedCharge for LocalCallableFact {
    fn retained_charge(&self) -> u64 {
        self.identity
            .retained_charge()
            .saturating_add(self.symbol.retained_charge())
    }
}

impl RetainedCharge for LocalNominalMetadataFact {
    fn retained_charge(&self) -> u64 {
        self.identity.retained_charge()
    }
}

impl RetainedCharge for LocalBuiltinNominalRequest {
    fn retained_charge(&self) -> u64 {
        self.name
            .retained_charge()
            .saturating_add(self.query_ty.retained_charge())
    }
}

impl RetainedCharge for LocalMaterializationFacts {
    fn retained_charge(&self) -> u64 {
        self.declarations
            .retained_charge()
            .saturating_add(self.anonymous_nominals.retained_charge())
            .saturating_add(self.callables.retained_charge())
            .saturating_add(self.nominal_metadata.retained_charge())
            .saturating_add(self.modules.retained_charge())
            .saturating_add(self.builtin_nominals.retained_charge())
            .saturating_add(self.required_types.retained_charge())
            .saturating_add(self.indexes.retained_charge())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LocalFactSelectionFailure {
    MissingNamedNominal(StableDefinitionKey),
    MissingAnonymousNominal(crate::AnonymousNominalKey),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LocalMaterializationFailure {
    InvalidSpecializationIdentity,
    DuplicateNominalMetadata,
    MissingNominalMetadata,
    ExtraNominalMetadata,
    AmbiguousNamedDestructor,
    Import(rue_air::SemanticImportFailure),
    Body(rue_air::SemanticBodyImportFailure),
}

impl From<rue_air::SemanticImportFailure> for LocalMaterializationFailure {
    fn from(value: rue_air::SemanticImportFailure) -> Self {
        Self::Import(value)
    }
}

impl From<rue_air::SemanticBodyImportFailure> for LocalMaterializationFailure {
    fn from(value: rue_air::SemanticBodyImportFailure) -> Self {
        Self::Body(value)
    }
}

pub(crate) type LocalSemanticMaterialization =
    rue_air::SemanticLocalMaterialization<StableDefinitionKey, ModuleId>;

fn callable_kind_for_identity(identity: &FunctionInstanceKey) -> rue_air::AnalyzedCallableKind {
    match identity {
        FunctionInstanceKey::Definition(key) if key.kind() == StableDefinitionKind::Destructor => {
            rue_air::AnalyzedCallableKind::Destructor
        }
        FunctionInstanceKey::Specialization { base, .. } => callable_kind_for_identity(base),
        FunctionInstanceKey::AnonymousMember { member, .. }
            if member.kind == crate::AnonymousMemberKind::Destructor =>
        {
            rue_air::AnalyzedCallableKind::Destructor
        }
        FunctionInstanceKey::DropGlue(_) => rue_air::AnalyzedCallableKind::DropGlue,
        _ => rue_air::AnalyzedCallableKind::Ordinary,
    }
}

pub(crate) fn fallback_callable_symbol(identity: &FunctionInstanceKey) -> Arc<str> {
    if let Some(symbol) = crate::semantic_identity::anonymous_member_source_symbol(identity) {
        return Arc::from(symbol);
    }
    let encoded = crate::StableSymbolEncoder::encode(&crate::StableSymbolId::Callable(
        crate::StableCallableId::Function(identity.clone()),
    ));
    if callable_kind_for_identity(identity) == rue_air::AnalyzedCallableKind::Destructor {
        Arc::from(format!("{encoded}.__drop"))
    } else {
        Arc::from(encoded)
    }
}

/// Reconstruct the canonical live AIR symbol for a rooted callable.
///
/// The durable graph retains the declaration, owner, and specialization facts
/// that whole-program analysis uses for presentation names. A rooted local
/// epoch projects those names here while the CFG codegen domain continues to
/// own the separate stable machine-symbol encoding.
pub(crate) fn rooted_callable_symbol(identity: &FunctionInstanceKey) -> Arc<str> {
    live_callable_symbol(identity)
        .map(Arc::from)
        .unwrap_or_else(|| fallback_callable_symbol(identity))
}

fn live_callable_symbol(identity: &FunctionInstanceKey) -> Option<String> {
    match identity {
        FunctionInstanceKey::Definition(definition) => match definition.kind() {
            StableDefinitionKind::Function => {
                let module = rue_air::mangle_symbol_component(&rue_air::normalize_module_path(
                    definition.module().logical_path(),
                ));
                Some(format!("__rue_fn_{module}__{}", definition.name()))
            }
            StableDefinitionKind::Destructor
            | StableDefinitionKind::Method
            | StableDefinitionKind::AssociatedFunction => {
                let owner = named_type_live_symbol(definition.owner()?);
                let (separator, member) = match definition.kind() {
                    StableDefinitionKind::Destructor => (".", "__drop"),
                    StableDefinitionKind::Method => (".", definition.name()),
                    StableDefinitionKind::AssociatedFunction => ("::", definition.name()),
                    _ => unreachable!(),
                };
                Some(format!("{owner}{separator}{member}"))
            }
            _ => None,
        },
        FunctionInstanceKey::Specialization { base, arguments } => {
            let mut symbol = live_callable_symbol(base)?;
            for ty in arguments.types.iter() {
                symbol.push('.');
                symbol.push_str(&mangle_canonical_type(ty));
            }
            for value in arguments.values.iter() {
                symbol.push('.');
                symbol.push_str(&mangle_canonical_value(value));
            }
            Some(symbol)
        }
        FunctionInstanceKey::AnonymousMember { .. } => {
            crate::semantic_identity::anonymous_member_source_symbol(identity)
        }
        FunctionInstanceKey::DropGlue(owner) => {
            Some(format!("__rue_drop_{}", drop_glue_type_name(owner)?))
        }
    }
}

fn named_type_live_symbol(owner: &crate::bound_definitions::StableNamedTypeKey) -> String {
    let bare = match owner.kind() {
        StableDefinitionKind::Struct => {
            owner.module().is_trusted_standard_library()
                && rue_air::LangItem::from_standard_library_nominal(
                    owner.module().logical_path(),
                    owner.name(),
                )
                .is_some()
        }
        StableDefinitionKind::Enum => rue_builtins::is_reserved_enum_name(owner.name()),
        _ => false,
    };
    if bare {
        owner.name().to_owned()
    } else {
        format!(
            "{}${}",
            owner.name(),
            rue_air::mangle_symbol_component(&rue_air::normalize_module_path(
                owner.module().logical_path()
            ))
        )
    }
}

fn drop_glue_type_name(ty: &crate::TypeInstanceKey) -> Option<String> {
    use crate::{NominalInstanceKey, TypeInstanceKey};
    Some(match ty {
        TypeInstanceKey::I8 => "i8".to_owned(),
        TypeInstanceKey::I16 => "i16".to_owned(),
        TypeInstanceKey::I32 => "i32".to_owned(),
        TypeInstanceKey::I64 => "i64".to_owned(),
        TypeInstanceKey::U8 => "u8".to_owned(),
        TypeInstanceKey::U16 => "u16".to_owned(),
        TypeInstanceKey::U32 => "u32".to_owned(),
        TypeInstanceKey::U64 => "u64".to_owned(),
        TypeInstanceKey::Bool => "bool".to_owned(),
        TypeInstanceKey::Unit => "unit".to_owned(),
        TypeInstanceKey::Never => "never".to_owned(),
        TypeInstanceKey::ComptimeType => "comptime_type".to_owned(),
        TypeInstanceKey::BuiltinNominal { name, .. }
        | TypeInstanceKey::Nominal(NominalInstanceKey::Builtin { name, .. }) => name.to_string(),
        TypeInstanceKey::Nominal(NominalInstanceKey::Named(definition)) => {
            crate::semantic_identity::named_nominal_source_symbol(definition)?
        }
        TypeInstanceKey::Nominal(NominalInstanceKey::Anonymous(identity)) => {
            crate::semantic_identity::anonymous_nominal_source_symbol(identity)
        }
        TypeInstanceKey::Array { element, len } => {
            format!("array_{}_{len}", drop_glue_type_name(element)?)
        }
        TypeInstanceKey::PtrConst(pointee) => {
            format!("ptr_const_{}", drop_glue_type_name(pointee)?)
        }
        TypeInstanceKey::PtrMut(pointee) => {
            format!("ptr_mut_{}", drop_glue_type_name(pointee)?)
        }
        TypeInstanceKey::Slice { .. }
        | TypeInstanceKey::Module(_)
        | TypeInstanceKey::GenericParameter(_) => return None,
    })
}

fn mangle_canonical_type(ty: &crate::TypeInstanceKey) -> String {
    use crate::{NominalInstanceKey, TypeInstanceKey};
    match ty {
        TypeInstanceKey::I8 => "i8".to_owned(),
        TypeInstanceKey::I16 => "i16".to_owned(),
        TypeInstanceKey::I32 => "i32".to_owned(),
        TypeInstanceKey::I64 => "i64".to_owned(),
        TypeInstanceKey::U8 => "u8".to_owned(),
        TypeInstanceKey::U16 => "u16".to_owned(),
        TypeInstanceKey::U32 => "u32".to_owned(),
        TypeInstanceKey::U64 => "u64".to_owned(),
        TypeInstanceKey::Bool => "bool".to_owned(),
        TypeInstanceKey::Unit => "()".to_owned(),
        TypeInstanceKey::Never => "!".to_owned(),
        TypeInstanceKey::ComptimeType => "type".to_owned(),
        TypeInstanceKey::BuiltinNominal { kind, name }
        | TypeInstanceKey::Nominal(NominalInstanceKey::Builtin { kind, name }) => {
            format!("builtin{kind:?}{}", rue_air::mangle_symbol_component(name))
        }
        TypeInstanceKey::Nominal(NominalInstanceKey::Named(definition)) => format!(
            "{:?}{}{}",
            definition.kind(),
            rue_air::mangle_symbol_component(definition.module().logical_path()),
            rue_air::mangle_symbol_component(definition.name()),
        ),
        TypeInstanceKey::Nominal(NominalInstanceKey::Anonymous(identity)) => format!(
            "anon{:032x}",
            crate::semantic_identity::anonymous_nominal_digest(identity)
        ),
        TypeInstanceKey::Array { element, len } => {
            format!("array{}_{len}", mangle_canonical_type(element))
        }
        TypeInstanceKey::Slice { element, name } => format!(
            "slice{}_{}",
            mangle_canonical_type(element),
            rue_air::mangle_symbol_component(name),
        ),
        TypeInstanceKey::PtrConst(pointee) => {
            format!("ptr_const{}", mangle_canonical_type(pointee))
        }
        TypeInstanceKey::PtrMut(pointee) => {
            format!("ptr_mut{}", mangle_canonical_type(pointee))
        }
        TypeInstanceKey::Module(module) => format!(
            "module{}",
            rue_air::mangle_symbol_component(module.logical_path())
        ),
        TypeInstanceKey::GenericParameter(index) => format!("generic{index}"),
    }
}

fn mangle_canonical_value(value: &crate::CanonicalArgumentValue) -> String {
    use crate::CanonicalArgumentValue;
    match value {
        CanonicalArgumentValue::Integer(value) if *value < 0 => {
            format!("vm{}", value.unsigned_abs())
        }
        CanonicalArgumentValue::Integer(value) => format!("v{value}"),
        CanonicalArgumentValue::Bool(value) => format!("v{value}"),
        CanonicalArgumentValue::Type(ty) => format!("v{}", mangle_canonical_type(ty)),
        CanonicalArgumentValue::Function(function) => format!(
            "vfn{}",
            rue_air::mangle_symbol_component(&live_callable_symbol(function).unwrap_or_else(
                || {
                    crate::StableSymbolEncoder::encode(&crate::StableSymbolId::Callable(
                        crate::StableCallableId::Function(function.as_ref().clone()),
                    ))
                }
            ))
        ),
        CanonicalArgumentValue::Unit => "vunit".to_owned(),
        CanonicalArgumentValue::String(value) => {
            format!("vstr{}", rue_air::mangle_symbol_component(value))
        }
    }
}

/// Materialize one canonical body from exact query facts.
///
/// The declaration and anonymous slices may contain only the facts required by
/// this body. Missing transitive shapes/callables fail closed in `rue-air`;
/// this adapter never widens the request by discovering a program universe.
#[allow(dead_code)]
pub(crate) fn materialize_canonical_body(
    canonical: &crate::body_query::CanonicalBody,
    body_span: rue_span::Span,
    declarations: &[DurableDeclarationSemantic],
    anonymous_nominals: &[DurableAnonymousNominal],
    callable_facts: &[LocalCallableFact],
    nominal_metadata: &[LocalNominalMetadataFact],
    modules: &[ModuleId],
    builtin_facts: &[LocalBuiltinNominalFact],
    materialization_types: &[crate::durable_semantics::DurableType],
) -> Result<LocalSemanticMaterialization, LocalMaterializationFailure> {
    let indexes = LocalMaterializationIndexes::new(declarations, nominal_metadata);
    materialize_canonical_body_with_indexes(
        canonical,
        body_span,
        declarations,
        anonymous_nominals,
        callable_facts,
        nominal_metadata,
        modules,
        builtin_facts,
        materialization_types,
        &indexes,
    )
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn materialize_canonical_body_with_indexes(
    canonical: &crate::body_query::CanonicalBody,
    body_span: rue_span::Span,
    declarations: &[DurableDeclarationSemantic],
    anonymous_nominals: &[DurableAnonymousNominal],
    callable_facts: &[LocalCallableFact],
    nominal_metadata: &[LocalNominalMetadataFact],
    modules: &[ModuleId],
    builtin_facts: &[LocalBuiltinNominalFact],
    materialization_types: &[crate::durable_semantics::DurableType],
    indexes: &LocalMaterializationIndexes,
) -> Result<LocalSemanticMaterialization, LocalMaterializationFailure> {
    let (identity, body) = match canonical {
        crate::body_query::CanonicalBody::Ordinary { owner, body } => {
            (FunctionInstanceKey::Definition(owner.clone()), body)
        }
        crate::body_query::CanonicalBody::Anonymous { identity, body, .. } => {
            (identity.clone(), body)
        }
        crate::body_query::CanonicalBody::Specialization { identity, body, .. } => (
            crate::semantic_identity::function_instance_from_specialization(identity)
                .ok_or(LocalMaterializationFailure::InvalidSpecializationIdentity)?,
            body,
        ),
    };
    materialize_semantic_body_with_indexes(
        identity,
        body,
        body_span,
        declarations,
        anonymous_nominals,
        callable_facts,
        nominal_metadata,
        modules,
        builtin_facts,
        materialization_types,
        indexes,
    )
}

#[allow(clippy::too_many_arguments, dead_code)]
pub(crate) fn materialize_semantic_body(
    identity: FunctionInstanceKey,
    body: &rue_air::SemanticBody<StableDefinitionKey, ModuleId>,
    body_span: rue_span::Span,
    declarations: &[DurableDeclarationSemantic],
    anonymous_nominals: &[DurableAnonymousNominal],
    callable_facts: &[LocalCallableFact],
    nominal_metadata: &[LocalNominalMetadataFact],
    modules: &[ModuleId],
    builtin_facts: &[LocalBuiltinNominalFact],
    materialization_types: &[crate::durable_semantics::DurableType],
) -> Result<LocalSemanticMaterialization, LocalMaterializationFailure> {
    let indexes = LocalMaterializationIndexes::new(declarations, nominal_metadata);
    materialize_semantic_body_with_indexes(
        identity,
        body,
        body_span,
        declarations,
        anonymous_nominals,
        callable_facts,
        nominal_metadata,
        modules,
        builtin_facts,
        materialization_types,
        &indexes,
    )
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn materialize_semantic_body_with_indexes(
    identity: FunctionInstanceKey,
    body: &rue_air::SemanticBody<StableDefinitionKey, ModuleId>,
    body_span: rue_span::Span,
    declarations: &[DurableDeclarationSemantic],
    anonymous_nominals: &[DurableAnonymousNominal],
    callable_facts: &[LocalCallableFact],
    _nominal_metadata: &[LocalNominalMetadataFact],
    modules: &[ModuleId],
    builtin_facts: &[LocalBuiltinNominalFact],
    materialization_types: &[crate::durable_semantics::DurableType],
    indexes: &LocalMaterializationIndexes,
) -> Result<LocalSemanticMaterialization, LocalMaterializationFailure> {
    let callable_kind = if body.is_accessor {
        rue_air::AnalyzedCallableKind::Accessor
    } else {
        callable_kind_for_identity(&identity)
    };

    if indexes.has_ambiguous_named_destructor {
        return Err(LocalMaterializationFailure::AmbiguousNamedDestructor);
    }
    let destructor_for = |owner: &StableDefinitionKey| indexes.destructor(owner);
    if indexes.has_duplicate_nominal_metadata {
        return Err(LocalMaterializationFailure::DuplicateNominalMetadata);
    }
    let mut nominals = Vec::new();
    for declaration in declarations {
        let (kind, shape) = match &declaration.payload {
            DurableDeclarationPayload::Struct {
                fields,
                is_copy,
                is_linear,
            } => (
                rue_air::SemanticImportNominalKind::Struct,
                rue_air::SemanticLocalNominalShape::Struct {
                    fields: fields.clone(),
                    is_copy: *is_copy,
                    is_linear: *is_linear,
                    destructor: destructor_for(&declaration.key),
                },
            ),
            DurableDeclarationPayload::Enum { variants } => (
                rue_air::SemanticImportNominalKind::Enum,
                rue_air::SemanticLocalNominalShape::Enum {
                    variants: variants.clone(),
                },
            ),
            _ => continue,
        };
        let lang_item = indexes
            .nominal_metadata(&declaration.key)
            .ok_or(LocalMaterializationFailure::MissingNominalMetadata)?;
        nominals.push(rue_air::SemanticLocalNominal {
            key: rue_air::NominalInstanceKey::Named(declaration.key.clone()),
            module_path: Arc::from(declaration.key.module().as_str()),
            name: Arc::from(declaration.key.name()),
            kind,
            is_public: declaration.is_public,
            lang_item,
            shape,
        });
    }
    let named_nominal_count = declarations
        .iter()
        .filter(|declaration| {
            matches!(
                declaration.payload,
                DurableDeclarationPayload::Struct { .. } | DurableDeclarationPayload::Enum { .. }
            )
        })
        .count();
    if indexes.nominal_metadata.len() != named_nominal_count {
        return Err(LocalMaterializationFailure::ExtraNominalMetadata);
    }
    nominals.extend(anonymous_nominals.iter().map(|nominal| {
        let (kind, shape) = match &nominal.shape {
            DurableAnonymousNominalShape::Struct { fields, methods } => {
                let destructor = methods
                    .iter()
                    .find(|method| method.has_self && method.name.as_ref() == "__drop")
                    .map(|_| FunctionInstanceKey::AnonymousMember {
                        owner: Box::new(crate::TypeInstanceKey::Nominal(
                            crate::NominalInstanceKey::Anonymous(nominal.identity.clone()),
                        )),
                        member: crate::AnonymousMemberKey {
                            kind: crate::AnonymousMemberKind::Destructor,
                            name: Arc::from("__drop"),
                        },
                    });
                (
                    rue_air::SemanticImportNominalKind::Struct,
                    rue_air::SemanticLocalNominalShape::Struct {
                        fields: fields.clone(),
                        // Anonymous structs have no `copy` or `linear`
                        // declaration modifiers. Transitive ownership facts are
                        // derived by the completed local type pool.
                        is_copy: false,
                        is_linear: false,
                        destructor,
                    },
                )
            }
            DurableAnonymousNominalShape::Enum { variants } => (
                rue_air::SemanticImportNominalKind::Enum,
                rue_air::SemanticLocalNominalShape::Enum {
                    variants: variants.clone(),
                },
            ),
        };
        rue_air::SemanticLocalNominal {
            key: rue_air::NominalInstanceKey::Anonymous(nominal.identity.clone()),
            module_path: Arc::from("<anonymous>"),
            name: nominal.source_symbol().clone(),
            kind,
            is_public: false,
            lang_item: None,
            shape,
        }
    }));
    for fact in builtin_facts {
        let identity_kind = fact.request.kind;
        let name = fact.request.name.clone();
        let (kind, shape) = match (&identity_kind, &fact.facts.shape, &fact.request.query_ty) {
            (
                crate::AnonymousNominalKind::Struct,
                crate::type_queries::TypeShape::Struct { fields },
                _,
            ) => (
                rue_air::SemanticImportNominalKind::Struct,
                rue_air::SemanticLocalNominalShape::Struct {
                    fields: fields
                        .iter()
                        .map(|(name, ty)| {
                            (
                                name.clone(),
                                crate::drop_glue::semantic_type_from_instance(ty),
                            )
                        })
                        .collect::<Vec<_>>()
                        .into(),
                    is_copy: fact.facts.is_copy,
                    is_linear: fact.facts.carries_linear,
                    destructor: fact.facts.destructor.clone(),
                },
            ),
            (
                crate::AnonymousNominalKind::Enum,
                crate::type_queries::TypeShape::Enum { variants },
                _,
            ) => (
                rue_air::SemanticImportNominalKind::Enum,
                rue_air::SemanticLocalNominalShape::Enum {
                    variants: variants
                        .iter()
                        .map(|(name, fields)| {
                            (
                                name.clone(),
                                fields
                                    .iter()
                                    .map(crate::drop_glue::semantic_type_from_instance)
                                    .collect::<Vec<_>>()
                                    .into(),
                            )
                        })
                        .collect::<Vec<_>>()
                        .into(),
                },
            ),
            (
                crate::AnonymousNominalKind::Struct,
                crate::type_queries::TypeShape::Slice,
                crate::TypeInstanceKey::Slice { element, .. },
            ) => (
                rue_air::SemanticImportNominalKind::Struct,
                rue_air::SemanticLocalNominalShape::Struct {
                    fields: vec![
                        (
                            Arc::from("ptr"),
                            rue_air::SemanticImportType::PtrConst(Box::new(
                                crate::drop_glue::semantic_type_from_instance(element),
                            )),
                        ),
                        (Arc::from("len"), rue_air::SemanticImportType::U64),
                    ]
                    .into(),
                    is_copy: true,
                    is_linear: false,
                    destructor: None,
                },
            ),
            _ => {
                return Err(LocalMaterializationFailure::Import(
                    rue_air::SemanticImportFailure::NominalKindMismatch,
                ));
            }
        };
        nominals.push(rue_air::SemanticLocalNominal {
            key: rue_air::NominalInstanceKey::Builtin {
                kind: identity_kind,
                name: name.clone(),
            },
            module_path: Arc::from("<builtin>"),
            name,
            kind,
            is_public: true,
            lang_item: None,
            shape,
        });
    }
    let callables = callable_facts
        .iter()
        .map(|fact| rue_air::SemanticLocalCallable {
            key: fact.identity.clone(),
            symbol: fact.symbol.clone(),
        })
        .collect();
    let epoch = rue_air::SemanticImportEpoch::new_local(nominals, callables, modules.to_vec())?;
    Ok(epoch.materialize_local_body_with_types(
        identity,
        callable_kind,
        body,
        body_span,
        materialization_types,
    )?)
}

/// Select the transitive nominal and callable closure needed to materialize one
/// stable body. The returned value contains no whole-program collection: edits
/// outside this closure cannot change CFG memo identity.
pub(crate) fn select_materialization_facts(
    identity: &FunctionInstanceKey,
    body: &rue_air::SemanticBody<StableDefinitionKey, ModuleId>,
    index: &LocalFactSelectionIndex<'_>,
    callable_symbols: &std::collections::BTreeMap<FunctionInstanceKey, Arc<str>>,
    interner: &mut LocalMaterializationFactInterner,
) -> Result<LocalMaterializationFacts, LocalFactSelectionFailure> {
    use rue_air::SemanticBodyInstDependency as Dependency;

    struct Selection<'index, 'facts> {
        index: &'index LocalFactSelectionIndex<'facts>,
        selected_declarations:
            std::collections::BTreeMap<StableDefinitionKey, DurableDeclarationSemantic>,
        selected_anonymous:
            std::collections::BTreeMap<crate::AnonymousNominalKey, DurableAnonymousNominal>,
        pending_nominals: Vec<crate::NominalInstanceKey>,
        seen_nominals: std::collections::BTreeSet<crate::NominalInstanceKey>,
        opaque_nominals: std::collections::BTreeSet<crate::NominalInstanceKey>,
        callables: std::collections::BTreeSet<FunctionInstanceKey>,
        modules: std::collections::BTreeSet<ModuleId>,
        builtins: std::collections::BTreeSet<LocalBuiltinNominalRequest>,
        seen_semantic_types: AHashSet<crate::durable_semantics::DurableType>,
        seen_opaque_types: AHashSet<crate::durable_semantics::DurableType>,
    }

    impl Selection<'_, '_> {
        fn builtin(&mut self, kind: crate::AnonymousNominalKind, name: &Arc<str>) {
            // The local semantic epoch owns the canonical fixed builtins and
            // lazily materializes fixed-capacity strings. Supplying query
            // facts for either would shadow that canonical registry. Only
            // synthetic builtin-shaped values such as slice views need an
            // exact fact request here.
            if name.as_ref() == "str"
                || rue_builtins::get_builtin_enum(name).is_some()
                || rue_air::fixed_string_capacity(name).is_some()
            {
                return;
            }
            let query_ty = self
                .index
                .shared
                .slice_sources
                .get(name)
                .cloned()
                .unwrap_or_else(|| crate::TypeInstanceKey::BuiltinNominal {
                    kind,
                    name: name.clone(),
                });
            self.builtins.insert(LocalBuiltinNominalRequest {
                kind,
                name: name.clone(),
                query_ty,
            });
        }

        fn semantic_type(&mut self, ty: &crate::durable_semantics::DurableType) {
            if !self.seen_semantic_types.insert(ty.clone()) {
                return;
            }
            use rue_air::SemanticImportType as T;
            match ty {
                T::Nominal(key) => self.nominal(&crate::NominalInstanceKey::Named(key.clone())),
                T::AnonymousNominal(key) => {
                    self.nominal(&crate::NominalInstanceKey::Anonymous(key.clone()))
                }
                T::BuiltinNominal { kind, name } => {
                    self.builtin(
                        match kind {
                            rue_air::SemanticImportNominalKind::Struct => {
                                crate::AnonymousNominalKind::Struct
                            }
                            rue_air::SemanticImportNominalKind::Enum => {
                                crate::AnonymousNominalKind::Enum
                            }
                        },
                        name,
                    );
                }
                T::Array { element, .. } => self.semantic_type(element),
                T::PtrConst(element) | T::PtrMut(element) | T::Slice { element, .. } => {
                    self.opaque_semantic_type(element)
                }
                T::Module(module) => {
                    self.modules.insert(module.clone());
                }
                _ => {}
            }
        }

        fn opaque_semantic_type(&mut self, ty: &crate::durable_semantics::DurableType) {
            if !self.seen_opaque_types.insert(ty.clone()) {
                return;
            }
            use rue_air::SemanticImportType as T;
            match ty {
                T::Nominal(key) => {
                    self.opaque_nominals
                        .insert(crate::NominalInstanceKey::Named(key.clone()));
                }
                T::AnonymousNominal(key) => {
                    self.opaque_nominals
                        .insert(crate::NominalInstanceKey::Anonymous(key.clone()));
                }
                T::BuiltinNominal { kind, name } => {
                    self.builtin(
                        match kind {
                            rue_air::SemanticImportNominalKind::Struct => {
                                crate::AnonymousNominalKind::Struct
                            }
                            rue_air::SemanticImportNominalKind::Enum => {
                                crate::AnonymousNominalKind::Enum
                            }
                        },
                        name,
                    );
                }
                T::Array { element, .. }
                | T::PtrConst(element)
                | T::PtrMut(element)
                | T::Slice { element, .. } => self.opaque_semantic_type(element),
                T::Module(module) => {
                    self.modules.insert(module.clone());
                }
                _ => {}
            }
        }

        fn nominal(&mut self, nominal: &crate::NominalInstanceKey) {
            if self.seen_nominals.insert(nominal.clone()) {
                self.pending_nominals.push(nominal.clone());
            }
        }

        fn instance_type(&mut self, ty: &crate::TypeInstanceKey) {
            use crate::TypeInstanceKey as T;
            match ty {
                T::BuiltinNominal { kind, name } => {
                    self.builtin(*kind, name);
                }
                T::I8
                | T::I16
                | T::I32
                | T::I64
                | T::U8
                | T::U16
                | T::U32
                | T::U64
                | T::Bool
                | T::Unit
                | T::Never
                | T::ComptimeType
                | T::GenericParameter(_) => {}
                T::Nominal(crate::NominalInstanceKey::Builtin { kind, name }) => {
                    self.builtin(*kind, name);
                }
                T::Nominal(nominal) => self.nominal(nominal),
                T::Array { element, .. } => self.instance_type(element),
                T::Slice { element, .. } | T::PtrConst(element) | T::PtrMut(element) => {
                    self.opaque_instance_type(element)
                }
                T::Module(module) => {
                    self.modules.insert(module.clone());
                }
            }
        }

        fn opaque_instance_type(&mut self, ty: &crate::TypeInstanceKey) {
            use crate::TypeInstanceKey as T;
            match ty {
                T::BuiltinNominal { kind, name }
                | T::Nominal(crate::NominalInstanceKey::Builtin { kind, name }) => {
                    self.builtin(*kind, name);
                }
                T::Nominal(nominal) => {
                    self.opaque_nominals.insert(nominal.clone());
                }
                T::Array { element, .. }
                | T::Slice { element, .. }
                | T::PtrConst(element)
                | T::PtrMut(element) => self.opaque_instance_type(element),
                T::Module(module) => {
                    self.modules.insert(module.clone());
                }
                _ => {}
            }
        }

        fn arguments(&mut self, arguments: &crate::CanonicalArguments) {
            for ty in arguments.types.iter() {
                self.instance_type(ty);
            }
            for value in arguments.values.iter() {
                match value {
                    crate::CanonicalArgumentValue::Type(ty) => self.instance_type(ty),
                    crate::CanonicalArgumentValue::Function(function) => self.callable(function),
                    crate::CanonicalArgumentValue::Integer(_)
                    | crate::CanonicalArgumentValue::Bool(_)
                    | crate::CanonicalArgumentValue::Unit
                    | crate::CanonicalArgumentValue::String(_) => {}
                }
            }
        }

        fn callable(&mut self, callable: &FunctionInstanceKey) {
            if !self.callables.insert(callable.clone()) {
                return;
            }
            match callable {
                FunctionInstanceKey::Definition(definition) => {
                    self.modules.insert(definition.module().clone());
                }
                FunctionInstanceKey::Specialization { base, arguments } => {
                    if let FunctionInstanceKey::Definition(definition) = base.as_ref() {
                        self.modules.insert(definition.module().clone());
                    }
                    self.arguments(arguments);
                }
                FunctionInstanceKey::AnonymousMember { owner, .. }
                | FunctionInstanceKey::DropGlue(owner) => self.instance_type(owner),
            }
        }

        fn finish_nominals(&mut self) -> Result<(), LocalFactSelectionFailure> {
            while let Some(nominal) = self.pending_nominals.pop() {
                match nominal {
                    crate::NominalInstanceKey::Builtin { .. } => {}
                    crate::NominalInstanceKey::Named(key) => {
                        let declaration =
                            self.index.declaration(&key).cloned().ok_or_else(|| {
                                LocalFactSelectionFailure::MissingNamedNominal(key.clone())
                            })?;
                        self.modules.insert(key.module().clone());
                        self.selected_declarations
                            .insert(key.clone(), declaration.clone());
                        match &declaration.payload {
                            DurableDeclarationPayload::Struct { fields, .. } => {
                                for (_, ty) in fields.iter() {
                                    self.semantic_type(ty);
                                }
                                if let Some(destructor) = self.index.destructor(&key).cloned() {
                                    self.selected_declarations
                                        .insert(destructor.key.clone(), destructor.clone());
                                    self.callable(&FunctionInstanceKey::Definition(
                                        destructor.key.clone(),
                                    ));
                                }
                            }
                            DurableDeclarationPayload::Enum { variants } => {
                                for (_, fields) in variants.iter() {
                                    for ty in fields.iter() {
                                        self.semantic_type(ty);
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                    crate::NominalInstanceKey::Anonymous(key) => {
                        let nominal = self
                            .index
                            .anonymous
                            .get(&key)
                            .copied()
                            .cloned()
                            .ok_or_else(|| {
                                LocalFactSelectionFailure::MissingAnonymousNominal(key.clone())
                            })?;
                        self.selected_anonymous.insert(key.clone(), nominal.clone());
                        for (_, ty) in nominal.type_captures.iter() {
                            self.semantic_type(ty);
                        }
                        match &nominal.shape {
                            DurableAnonymousNominalShape::Struct { fields, methods } => {
                                for (_, ty) in fields.iter() {
                                    self.semantic_type(ty);
                                }
                                for method in methods.iter() {
                                    for (ty, _, _) in method.parameters.iter() {
                                        if let crate::durable_semantics::DurableAnonymousMethodType::Concrete(ty) = ty {
                                            self.semantic_type(ty);
                                        }
                                    }
                                    if let crate::durable_semantics::DurableAnonymousMethodType::Concrete(ty) = &method.result {
                                        self.semantic_type(ty);
                                    }
                                    if method.has_self && method.name.as_ref() == "__drop" {
                                        self.callable(&FunctionInstanceKey::AnonymousMember {
                                            owner: Box::new(crate::TypeInstanceKey::Nominal(
                                                crate::NominalInstanceKey::Anonymous(key.clone()),
                                            )),
                                            member: crate::AnonymousMemberKey {
                                                kind: crate::AnonymousMemberKind::Destructor,
                                                name: Arc::from("__drop"),
                                            },
                                        });
                                    }
                                }
                            }
                            DurableAnonymousNominalShape::Enum { variants } => {
                                for (_, fields) in variants.iter() {
                                    for ty in fields.iter() {
                                        self.semantic_type(ty);
                                    }
                                }
                            }
                        }
                    }
                }
            }
            for nominal in std::mem::take(&mut self.opaque_nominals) {
                if self.seen_nominals.contains(&nominal) {
                    continue;
                }
                match nominal {
                    crate::NominalInstanceKey::Builtin { .. } => {}
                    crate::NominalInstanceKey::Named(key) => {
                        let declaration = self.index.declaration(&key).ok_or_else(|| {
                            LocalFactSelectionFailure::MissingNamedNominal(key.clone())
                        })?;
                        let payload = match declaration.payload {
                            DurableDeclarationPayload::Struct { .. } => {
                                DurableDeclarationPayload::Struct {
                                    fields: Arc::new([]),
                                    is_copy: false,
                                    is_linear: false,
                                }
                            }
                            DurableDeclarationPayload::Enum { .. } => {
                                DurableDeclarationPayload::Enum {
                                    variants: Arc::new([]),
                                }
                            }
                            _ => continue,
                        };
                        self.modules.insert(key.module().clone());
                        self.selected_declarations.insert(
                            key.clone(),
                            DurableDeclarationSemantic {
                                key,
                                is_public: false,
                                payload,
                            },
                        );
                    }
                    crate::NominalInstanceKey::Anonymous(key) => {
                        let nominal = self.index.anonymous.get(&key).copied().ok_or_else(|| {
                            LocalFactSelectionFailure::MissingAnonymousNominal(key.clone())
                        })?;
                        let shape = match nominal.shape {
                            DurableAnonymousNominalShape::Struct { .. } => {
                                DurableAnonymousNominalShape::Struct {
                                    fields: Arc::new([]),
                                    methods: Arc::new([]),
                                }
                            }
                            DurableAnonymousNominalShape::Enum { .. } => {
                                DurableAnonymousNominalShape::Enum {
                                    variants: Arc::new([]),
                                }
                            }
                        };
                        self.selected_anonymous
                            .insert(key, nominal.with_shape(shape));
                    }
                }
            }
            Ok(())
        }
    }

    let mut selection = Selection {
        index,
        selected_declarations: std::collections::BTreeMap::new(),
        selected_anonymous: std::collections::BTreeMap::new(),
        pending_nominals: Vec::new(),
        seen_nominals: std::collections::BTreeSet::new(),
        opaque_nominals: std::collections::BTreeSet::new(),
        callables: std::collections::BTreeSet::new(),
        modules: std::collections::BTreeSet::new(),
        builtins: std::collections::BTreeSet::new(),
        seen_semantic_types: AHashSet::new(),
        seen_opaque_types: AHashSet::new(),
    };
    selection.callable(identity);
    let mut required_types = Vec::new();
    let mut seen_required_types = std::collections::HashSet::new();
    let mut require_type = |ty: &crate::durable_semantics::DurableType| {
        if seen_required_types.insert(ty.clone()) {
            required_types.push(ty.clone());
        }
    };
    require_type(&body.return_type);
    if !body.strings.is_empty() {
        require_type(&rue_air::SemanticImportType::PtrConst(Box::new(
            rue_air::SemanticImportType::U8,
        )));
    }
    selection.semantic_type(&body.return_type);
    for instruction in body.instructions.iter() {
        require_type(&instruction.ty);
        selection.semantic_type(&instruction.ty);
        if let rue_air::SemanticBodyInstData::CallSpecialized { identity, .. } = &instruction.data
            && let Some(callable) =
                crate::semantic_identity::function_instance_from_specialization(identity)
        {
            selection.callable(&callable);
        }
        instruction
            .data
            .visit_dependencies(&mut |dependency| match dependency {
                Dependency::Definition(definition) => {
                    selection.modules.insert(definition.module().clone());
                }
                Dependency::Nominal(nominal) => selection.nominal(nominal),
                Dependency::Function(function) => selection.callable(function),
                Dependency::Type(ty) => {
                    require_type(ty);
                    selection.semantic_type(ty);
                }
                Dependency::Instruction(_) | Dependency::Place(_) | Dependency::String(_) => {}
            });
    }
    for place in body.places.iter() {
        require_type(&place.base_type);
        selection.semantic_type(&place.base_type);
        for projection in place.projections.iter() {
            match projection {
                rue_air::SemanticBodyProjection::Field { struct_key, .. } => {
                    selection.nominal(struct_key)
                }
                rue_air::SemanticBodyProjection::Index { array_type, .. } => {
                    require_type(array_type);
                    selection.semantic_type(array_type)
                }
            }
        }
    }
    for (_, ty) in body.param_drops.iter() {
        require_type(ty);
        selection.semantic_type(ty);
    }
    for reference in body.method_references.iter() {
        selection.nominal(&reference.receiver);
    }
    selection.finish_nominals()?;

    let callables = selection
        .callables
        .into_iter()
        .map(|identity| {
            let symbol = callable_symbols
                .get(&identity)
                .cloned()
                .unwrap_or_else(|| fallback_callable_symbol(&identity));
            Ok(LocalCallableFact { identity, symbol })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let nominal_metadata = selection
        .selected_declarations
        .values()
        .filter(|declaration| {
            matches!(
                declaration.payload,
                DurableDeclarationPayload::Struct { .. } | DurableDeclarationPayload::Enum { .. }
            )
        })
        .map(|declaration| {
            let lang_item = (declaration.key.kind() == StableDefinitionKind::Struct
                && declaration.key.module().is_trusted_standard_library())
            .then(|| {
                rue_air::LangItem::from_standard_library_nominal(
                    declaration.key.module().as_str(),
                    declaration.key.name(),
                )
            })
            .flatten();
            LocalNominalMetadataFact::new(declaration.key.clone(), lang_item)
        })
        .collect::<Vec<_>>();
    let declarations = selection
        .selected_declarations
        .into_values()
        .collect::<Vec<_>>();
    Ok(interner.assemble(
        declarations,
        selection
            .selected_anonymous
            .into_values()
            .collect::<Vec<_>>(),
        callables,
        nominal_metadata,
        selection.modules.into_iter().collect::<Vec<_>>(),
        selection.builtins.into_iter().collect::<Vec<_>>(),
        required_types,
    ))
}

pub(crate) fn select_drop_glue_materialization_facts(
    owner: &crate::TypeInstanceKey,
    facts: &crate::type_queries::DropGlueFacts,
    index: &LocalFactSelectionIndex<'_>,
    callable_symbols: &std::collections::BTreeMap<FunctionInstanceKey, Arc<str>>,
    interner: &mut LocalMaterializationFactInterner,
) -> Result<LocalMaterializationFacts, LocalFactSelectionFailure> {
    let identity = FunctionInstanceKey::DropGlue(Box::new(owner.clone()));
    let mut roots = vec![(0, crate::drop_glue::semantic_type_from_instance(owner))];
    roots.extend(facts.nested.iter().enumerate().map(|(index, ty)| {
        (
            u32::try_from(index + 1).expect("drop-glue fact count fits u32"),
            crate::drop_glue::semantic_type_from_instance(ty),
        )
    }));
    let body = rue_air::SemanticBody {
        is_accessor: false,
        return_type: rue_air::SemanticImportType::Unit,
        instructions: Arc::new([]),
        places: Arc::new([]),
        strings: Arc::new([]),
        local_atoms: Arc::new([]),
        param_drops: roots.into(),
        borrow_slots: Arc::new([]),
        num_locals: 0,
        num_param_slots: 0,
        param_by_ref: Arc::new([]),
        param_writable: Arc::new([]),
        allow_unreachable_code: false,
        warnings: Arc::new([]),
        method_references: Arc::new([]),
    };
    select_materialization_facts(&identity, &body, index, callable_symbols, interner)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts_with_modules(modules: Vec<ModuleId>) -> LocalMaterializationFacts {
        LocalMaterializationFacts {
            declarations: Arc::new([]),
            anonymous_nominals: Arc::new([]),
            callables: Arc::new([]),
            nominal_metadata: Arc::new([]),
            modules: modules.into(),
            builtin_nominals: Arc::new([]),
            required_types: Arc::new([]),
            indexes: Arc::new(LocalMaterializationIndexes::default()),
        }
    }

    #[test]
    fn slice_interner_shares_equal_content_and_keeps_distinct_content_apart() {
        let a = ModuleId::from_validated_canonical("a.rue");
        let b = ModuleId::from_validated_canonical("b.rue");
        let mut interner = SliceInterner::<ModuleId>::default();

        let (first, reused) = interner.intern(vec![a.clone(), b.clone()]);
        assert!(!reused, "the first slice of its content is allocated");
        let (second, reused) = interner.intern(vec![a.clone(), b.clone()]);
        assert!(reused, "equal content is served from the first slice");
        assert!(
            Arc::ptr_eq(&first, &second),
            "equal content must be one allocation, not two equal ones"
        );

        // Order is part of identity: these slices have the same members and
        // are not interchangeable, because the order feeds body-local epoch
        // construction.
        let (reordered, reused) = interner.intern(vec![b.clone(), a.clone()]);
        assert!(!reused, "a reordered slice is different content");
        assert!(!Arc::ptr_eq(&first, &reordered));
        assert_eq!(reordered.as_ref(), [b.clone(), a.clone()].as_slice());

        let (subset, reused) = interner.intern(vec![a.clone()]);
        assert!(!reused);
        assert_eq!(subset.as_ref(), [a].as_slice());
        // The original is unchanged by anything interned after it.
        assert_eq!(first.as_ref(), [reordered[1].clone(), b].as_slice());
    }

    #[test]
    fn interned_closures_share_slices_without_sharing_whole_closures() {
        // Two closures that agree on six slices and differ on `callables` —
        // the shape every real pair of bodies has, because each closure seeds
        // its own callable identity. Whole-closure interning would share
        // nothing here; per-slice interning shares six of seven plus the
        // index.
        let module = ModuleId::from_validated_canonical("a.rue");
        let mut interner = LocalMaterializationFactInterner::default();
        let callable = |name: &str| LocalCallableFact {
            identity: FunctionInstanceKey::DropGlue(Box::new(crate::TypeInstanceKey::I32)),
            symbol: Arc::from(name),
        };

        let left = interner.assemble(
            Vec::new(),
            Vec::new(),
            vec![callable("left")],
            Vec::new(),
            vec![module.clone()],
            Vec::new(),
            Vec::new(),
        );
        let right = interner.assemble(
            Vec::new(),
            Vec::new(),
            vec![callable("right")],
            Vec::new(),
            vec![module],
            Vec::new(),
            Vec::new(),
        );

        assert!(Arc::ptr_eq(&left.declarations, &right.declarations));
        assert!(Arc::ptr_eq(
            &left.anonymous_nominals,
            &right.anonymous_nominals
        ));
        assert!(Arc::ptr_eq(&left.nominal_metadata, &right.nominal_metadata));
        assert!(Arc::ptr_eq(&left.modules, &right.modules));
        assert!(Arc::ptr_eq(&left.builtin_nominals, &right.builtin_nominals));
        assert!(Arc::ptr_eq(&left.required_types, &right.required_types));
        // Derived from `declarations` and `nominal_metadata`, both shared, so
        // the second closure skips rebuilding it.
        assert!(Arc::ptr_eq(&left.indexes, &right.indexes));
        // The one slice that genuinely differs stays separate.
        assert!(!Arc::ptr_eq(&left.callables, &right.callables));
        assert_eq!(left.callables[0].symbol.as_ref(), "left");
        assert_eq!(right.callables[0].symbol.as_ref(), "right");

        // Two selections, sixteen slice-or-index requests, seven of which are
        // the second closure reusing the first's.
        assert_eq!(interner.selections, 2);
        assert_eq!(interner.reused, 7);
        assert_eq!(interner.allocated, 9);
        assert_eq!(
            interner.allocated + interner.reused,
            interner.selections * 8
        );
    }

    #[test]
    fn materialization_union_preserves_first_seen_order() {
        let a = ModuleId::from_validated_canonical("a.rue");
        let b = ModuleId::from_validated_canonical("b.rue");
        let c = ModuleId::from_validated_canonical("c.rue");
        let left = facts_with_modules(vec![a.clone(), b.clone()]);
        let right = facts_with_modules(vec![b.clone(), c.clone(), a.clone()]);

        let union = LocalMaterializationFacts::union([&left, &right]);

        assert_eq!(union.modules.as_ref(), &[a, b, c]);
    }

    fn body() -> rue_air::SemanticBody<StableDefinitionKey, ModuleId> {
        use rue_air::{SemanticBody, SemanticBodyAnchor, SemanticBodyInst, SemanticBodyInstData};
        SemanticBody {
            is_accessor: false,
            return_type: rue_air::SemanticImportType::I32,
            instructions: vec![
                SemanticBodyInst {
                    data: SemanticBodyInstData::Const(7),
                    ty: rue_air::SemanticImportType::I32,
                    anchor: SemanticBodyAnchor { start: 1, end: 2 },
                },
                SemanticBodyInst {
                    data: SemanticBodyInstData::Ret(Some(0)),
                    ty: rue_air::SemanticImportType::I32,
                    anchor: SemanticBodyAnchor { start: 2, end: 3 },
                },
            ]
            .into(),
            places: Arc::new([]),
            strings: Arc::new([]),
            local_atoms: Arc::new([]),
            param_drops: Arc::new([]),
            borrow_slots: Arc::new([]),
            num_locals: 0,
            num_param_slots: 0,
            param_by_ref: Arc::new([]),
            param_writable: Arc::new([]),
            allow_unreachable_code: false,
            warnings: Arc::new([]),
            method_references: Arc::new([]),
        }
    }

    fn definition(
        module: ModuleId,
        kind: StableDefinitionKind,
        name: &str,
        owner: Option<(StableDefinitionKind, Arc<str>)>,
    ) -> StableDefinitionKey {
        StableDefinitionKey::for_test(module, kind.namespace(), kind, name, owner)
    }

    #[test]
    fn exact_metadata_restores_strbuf_language_item() {
        let module = ModuleId::from_trusted_validated_canonical("\0rue-std/strbuf.rue");
        let function = definition(
            module.clone(),
            StableDefinitionKind::Function,
            "probe",
            None,
        );
        let strbuf = definition(module.clone(), StableDefinitionKind::Struct, "StrBuf", None);
        let canonical = crate::body_query::CanonicalBody::Ordinary {
            owner: function.clone(),
            body: body(),
        };
        let declaration = DurableDeclarationSemantic {
            key: strbuf.clone(),
            is_public: true,
            payload: DurableDeclarationPayload::Struct {
                fields: Arc::new([]),
                is_copy: false,
                is_linear: false,
            },
        };
        assert_eq!(
            materialize_canonical_body(
                &canonical,
                rue_span::Span::with_file(rue_span::FileId::new(3), 100, 200),
                std::slice::from_ref(&declaration),
                &[],
                &[LocalCallableFact {
                    identity: FunctionInstanceKey::Definition(function.clone()),
                    symbol: Arc::from("probe"),
                }],
                &[],
                std::slice::from_ref(&module),
                &[],
                &[],
            )
            .err(),
            Some(LocalMaterializationFailure::MissingNominalMetadata)
        );
        let output = materialize_canonical_body(
            &canonical,
            rue_span::Span::with_file(rue_span::FileId::new(3), 100, 200),
            std::slice::from_ref(&declaration),
            &[],
            &[LocalCallableFact {
                identity: FunctionInstanceKey::Definition(function),
                symbol: Arc::from("probe"),
            }],
            &[LocalNominalMetadataFact::new(
                strbuf.clone(),
                Some(rue_air::LangItem::StrBuf),
            )],
            std::slice::from_ref(&module),
            &[],
            &[],
        )
        .unwrap();
        let ty = output
            .aggregate_types
            .iter()
            .find_map(|(ty, identity)| {
                (identity
                    == &crate::TypeInstanceKey::Nominal(crate::NominalInstanceKey::Named(
                        strbuf.clone(),
                    )))
                    .then_some(*ty)
            })
            .expect("StrBuf stable identity is retained");
        assert_eq!(
            output.type_pool.struct_lang_item(ty.as_struct().unwrap()),
            Some(rue_air::LangItem::StrBuf)
        );
    }

    #[test]
    fn duplicate_named_destructor_facts_are_ambiguous() {
        let module = ModuleId::from_validated_canonical("main.rue");
        let function = definition(
            module.clone(),
            StableDefinitionKind::Function,
            "probe",
            None,
        );
        let record = definition(module.clone(), StableDefinitionKind::Struct, "Record", None);
        let destructor = definition(
            module.clone(),
            StableDefinitionKind::Destructor,
            "Record",
            Some((StableDefinitionKind::Struct, Arc::from("Record"))),
        );
        let declarations = vec![
            DurableDeclarationSemantic {
                key: record.clone(),
                is_public: false,
                payload: DurableDeclarationPayload::Struct {
                    fields: Arc::new([]),
                    is_copy: false,
                    is_linear: false,
                },
            },
            DurableDeclarationSemantic {
                key: destructor.clone(),
                is_public: false,
                payload: DurableDeclarationPayload::Destructor,
            },
            DurableDeclarationSemantic {
                key: destructor,
                is_public: false,
                payload: DurableDeclarationPayload::Destructor,
            },
        ];
        let error = materialize_canonical_body(
            &crate::body_query::CanonicalBody::Ordinary {
                owner: function.clone(),
                body: body(),
            },
            rue_span::Span::with_file(rue_span::FileId::new(3), 100, 200),
            &declarations,
            &[],
            &[LocalCallableFact {
                identity: FunctionInstanceKey::Definition(function),
                symbol: Arc::from("probe"),
            }],
            &[LocalNominalMetadataFact::new(record, None)],
            &[module],
            &[],
            &[],
        )
        .err()
        .expect("duplicate destructor records must not select the first");
        assert_eq!(error, LocalMaterializationFailure::AmbiguousNamedDestructor);
    }

    #[test]
    fn fallback_destructor_symbol_satisfies_the_source_naming_contract() {
        let module = ModuleId::from_validated_canonical("main.rue");
        let function = definition(
            module.clone(),
            StableDefinitionKind::Function,
            "probe",
            None,
        );
        let record = definition(module.clone(), StableDefinitionKind::Struct, "Record", None);
        let destructor = definition(
            module.clone(),
            StableDefinitionKind::Destructor,
            "Record",
            Some((StableDefinitionKind::Struct, Arc::from("Record"))),
        );
        let declarations = vec![
            DurableDeclarationSemantic {
                key: record.clone(),
                is_public: false,
                payload: DurableDeclarationPayload::Struct {
                    fields: Arc::new([]),
                    is_copy: false,
                    is_linear: false,
                },
            },
            DurableDeclarationSemantic {
                key: destructor.clone(),
                is_public: false,
                payload: DurableDeclarationPayload::Destructor,
            },
        ];
        let mut selection_body = body();
        selection_body.return_type = rue_air::SemanticImportType::Nominal(record);
        let shared = SharedDeclarationFactIndex::new(&declarations);
        let (index, work) = LocalFactSelectionIndex::new(&shared, &declarations, &[]);
        assert_eq!(work, LocalFactSelectionIndexWork::default());
        let facts = select_materialization_facts(
            &FunctionInstanceKey::Definition(function.clone()),
            &selection_body,
            &index,
            &std::collections::BTreeMap::from([(
                FunctionInstanceKey::Definition(function.clone()),
                Arc::from("probe"),
            )]),
            &mut LocalMaterializationFactInterner::default(),
        )
        .unwrap();

        let destructor_symbol = facts
            .callables
            .iter()
            .find(|fact| fact.identity == FunctionInstanceKey::Definition(destructor.clone()))
            .expect("the selected nominal contributes its destructor callable")
            .symbol
            .as_ref();
        assert!(destructor_symbol.ends_with(".__drop"));

        materialize_canonical_body(
            &crate::body_query::CanonicalBody::Ordinary {
                owner: function,
                body: body(),
            },
            rue_span::Span::with_file(rue_span::FileId::new(3), 100, 200),
            &facts.declarations,
            &facts.anonymous_nominals,
            &facts.callables,
            &facts.nominal_metadata,
            &facts.modules,
            &[],
            &[],
        )
        .expect("fallback destructor symbols materialize without violating AIR invariants");
    }

    #[test]
    fn anonymous_fact_selection_preserves_the_carried_source_symbol() {
        use rue_rir::{RirStructuralAnchor, RirStructuralPathSegment};

        let module = ModuleId::from_validated_canonical("main.rue");
        let function = definition(module, StableDefinitionKind::Function, "probe", None);
        let identity = crate::AnonymousNominalKey {
            kind: crate::AnonymousNominalKind::Struct,
            producer: crate::StableProducerId::Definition(function.clone()),
            anchor: RirStructuralAnchor::new(vec![
                RirStructuralPathSegment::Body,
                RirStructuralPathSegment::AnonymousType(0),
            ]),
            arguments: crate::CanonicalArguments::default(),
        };
        let nominal = DurableAnonymousNominal::new(
            identity.clone(),
            DurableAnonymousNominalShape::Struct {
                fields: Arc::from([(Arc::from("value"), rue_air::SemanticImportType::I32)]),
                methods: Arc::from([]),
            },
            Arc::from([]),
            Arc::from([]),
        );
        let expected_symbol = crate::semantic_identity::anonymous_nominal_source_symbol(&identity);
        assert_eq!(nominal.source_symbol().as_ref(), expected_symbol);
        assert_eq!(
            nominal.anonymous_identity_digest(),
            crate::semantic_identity::anonymous_nominal_digest(&identity)
        );
        let shared = SharedDeclarationFactIndex::new(&[]);
        let (index, _) = LocalFactSelectionIndex::new(&shared, &[], std::slice::from_ref(&nominal));
        let symbols = std::collections::BTreeMap::from([(
            FunctionInstanceKey::Definition(function.clone()),
            Arc::from("probe"),
        )]);

        // One interner across both selections, so this also exercises the
        // sharing path rather than only the cold one.
        let mut interner = LocalMaterializationFactInterner::default();
        let mut select = |return_type| {
            let mut body = body();
            body.return_type = return_type;
            select_materialization_facts(
                &FunctionInstanceKey::Definition(function.clone()),
                &body,
                &index,
                &symbols,
                &mut interner,
            )
            .unwrap()
        };
        let full = select(rue_air::SemanticImportType::AnonymousNominal(
            identity.clone(),
        ));
        let opaque = select(rue_air::SemanticImportType::PtrConst(Box::new(
            rue_air::SemanticImportType::AnonymousNominal(identity),
        )));

        assert!(Arc::ptr_eq(
            nominal.source_symbol(),
            full.anonymous_nominals[0].source_symbol()
        ));
        assert!(Arc::ptr_eq(
            nominal.source_symbol(),
            opaque.anonymous_nominals[0].source_symbol()
        ));
        assert!(matches!(
            &opaque.anonymous_nominals[0].shape,
            DurableAnonymousNominalShape::Struct { fields, methods }
                if fields.is_empty() && methods.is_empty()
        ));
    }

    #[test]
    fn anonymous_destructor_instances_keep_the_cleanup_callable_kind() {
        let destructor = FunctionInstanceKey::AnonymousMember {
            owner: Box::new(crate::TypeInstanceKey::I32),
            member: crate::AnonymousMemberKey {
                kind: crate::AnonymousMemberKind::Destructor,
                name: Arc::from("__drop"),
            },
        };
        assert_eq!(
            callable_kind_for_identity(&destructor),
            rue_air::AnalyzedCallableKind::Destructor
        );
        assert_eq!(
            callable_kind_for_identity(&FunctionInstanceKey::Specialization {
                base: Box::new(destructor),
                arguments: crate::CanonicalArguments::default(),
            }),
            rue_air::AnalyzedCallableKind::Destructor
        );
    }
}
