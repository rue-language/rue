use super::body::*;
use super::semantic::*;
use super::*;
#[cfg(test)]
pub(crate) fn execution(attempt: &QueryRequestAttempt<impl Sized>) -> RequestExecution {
    attempt.execution()
}

// ---------------------------------------------------------------------------
// RUE-1091 slice 3b — the exact body-fact provider.
//
// `CompilerBodyFactProvider` implements the rue-air `BodyFactProvider` boundary
// inside a `BodyTransaction`-style query context: every op requests its exact
// backing terminal through `context.query_registered`, so the typed provider
// call *is* the dependency observation (ADR-0066 §4). It converts the private
// query-terminal values into rue-air's owned candidate-set / durable facts.
//
// The provider is production-compiled and is request-scoped; the observation
// probe below remains test-only. It consumes only the promoted exact lookup
// and import families and never retains the database or coordinator.
// ---------------------------------------------------------------------------

/// Owned receiver-type identity the provider keys method/operator candidates
/// and drop/`@copy` metadata on. It is exactly the `(receiver-type identity,
/// member name)` key the 3a review binds these ops to — never a per-body
/// universe walk and never a new module-index column.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) struct ReceiverTypeIdentity {
    pub(super) module: ModuleId,
    pub(super) type_name: Arc<str>,
    pub(super) type_category: crate::declaration_candidate::DeclarationCandidateCategory,
}

#[allow(dead_code)]
impl ReceiverTypeIdentity {
    pub(crate) fn new(
        module: ModuleId,
        type_name: impl Into<Arc<str>>,
        type_category: crate::declaration_candidate::DeclarationCandidateCategory,
    ) -> Self {
        Self {
            module,
            type_name: type_name.into(),
            type_category,
        }
    }
}

/// Key for the test-only provider-observation probe task. One task hosts a
/// batch of provider ops so the driven body's recorded query edges are
/// inspectable through the terminal's `dependencies()`.
#[cfg(test)]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) struct ProviderProbeKey {
    pub(super) label: Arc<str>,
}

#[cfg(test)]
impl QueryKey for ProviderProbeKey {
    fn stable_identity(&self) -> String {
        format!("provider-probe:{}", self.label)
    }

    fn stable_hash(&self, hasher: &mut rue_query::StableHasher) {
        self.label.hash(hasher);
    }
}

#[cfg(test)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ProviderProbeValue;

#[cfg(test)]
impl RetainedCharge for ProviderProbeValue {
    fn retained_charge(&self) -> u64 {
        0
    }
}

/// Convert a rue-air provider namespace to the compiler's presemantic
/// namespace, and back, so a body names a namespace without depending on the
/// compiler's candidate model.
#[allow(dead_code)]
pub(super) fn provider_namespace_to_definition(
    namespace: rue_air::ProviderNamespace,
) -> DefinitionNamespace {
    match namespace {
        rue_air::ProviderNamespace::ModuleItem => DefinitionNamespace::ModuleItem,
        rue_air::ProviderNamespace::Destructor => DefinitionNamespace::Destructor,
        rue_air::ProviderNamespace::Test => DefinitionNamespace::Test,
    }
}

#[allow(dead_code)]
pub(super) fn definition_namespace_to_provider(
    namespace: DefinitionNamespace,
) -> rue_air::ProviderNamespace {
    match namespace {
        DefinitionNamespace::ModuleItem => rue_air::ProviderNamespace::ModuleItem,
        DefinitionNamespace::Destructor => rue_air::ProviderNamespace::Destructor,
        DefinitionNamespace::Test => rue_air::ProviderNamespace::Test,
    }
}

#[allow(dead_code)]
pub(super) fn definition_kind_to_provider(kind: DefinitionKind) -> rue_air::ProviderDefinitionKind {
    match kind {
        DefinitionKind::Function => rue_air::ProviderDefinitionKind::Function,
        DefinitionKind::Struct => rue_air::ProviderDefinitionKind::Struct,
        DefinitionKind::Enum => rue_air::ProviderDefinitionKind::Enum,
        DefinitionKind::Destructor => rue_air::ProviderDefinitionKind::Destructor,
        DefinitionKind::Const => rue_air::ProviderDefinitionKind::Const,
        DefinitionKind::Test => rue_air::ProviderDefinitionKind::Test,
    }
}

/// Project one retained `LookupNameFact` into an owned rue-air candidate.
#[allow(dead_code)]
pub(super) fn name_candidate_from_fact(fact: &LookupNameFact) -> rue_air::NameCandidate {
    rue_air::NameCandidate {
        namespace: definition_namespace_to_provider(fact.namespace),
        kind: definition_kind_to_provider(fact.kind),
        is_public: fact.visibility == Some(rue_parser::ast::Visibility::Public),
        name: fact.name.clone(),
        language_item: fact.language_item,
    }
}

/// Classify a retained `LookupName` value into the owned rue-air candidate set.
#[allow(dead_code)]
pub(super) fn name_resolution_from_value(value: &LookupNameValue) -> rue_air::NameResolution {
    match &value.0 {
        Err(LookupNameFailure::ModuleIndexUnavailable(_)) => {
            rue_air::NameResolution::IndexUnavailable
        }
        Ok(facts) => rue_air::NameResolution::from_candidates(
            facts.iter().map(name_candidate_from_fact).collect(),
        ),
    }
}

/// Classify a retained `LookupImport` value into the owned rue-air result.
#[allow(dead_code)]
pub(super) fn import_resolution_from_value(value: &LookupImportValue) -> rue_air::ImportResolution {
    match &value.0 {
        Ok(binding) => rue_air::ImportResolution::Resolved {
            normalized_specifier: binding.normalized_specifier.clone(),
        },
        Err(ImportBindingFailure::Absent) => rue_air::ImportResolution::Absent,
        Err(ImportBindingFailure::Rejected) => rue_air::ImportResolution::Rejected,
    }
}

/// The compiler-side implementation of the rue-air exact provider boundary.
///
/// Bound entirely inside one query task: the cloneable query bundle records
/// each edge and owns the exact family handles/configuration/status needed by
/// the provider. No database or coordinator handle is retained by the provider.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CompilerBodyProviderIncomplete {
    Canceled,
    MissingInput(rue_query::InputIdentity),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CompilerBodyProviderStatus {
    Ready,
    Incomplete(CompilerBodyProviderIncomplete),
    Fatal(QueryAbort),
}

pub(super) fn provider_status_should_replace(
    current: &CompilerBodyProviderStatus,
    next: &CompilerBodyProviderStatus,
) -> bool {
    match (current, next) {
        (CompilerBodyProviderStatus::Fatal(_), _) => false,
        (_, CompilerBodyProviderStatus::Fatal(_)) => true,
        (CompilerBodyProviderStatus::Ready, _) => true,
        (CompilerBodyProviderStatus::Incomplete(_), _) => false,
    }
}

#[derive(Clone)]
pub(crate) struct CompilerBodyProviderQueries<'a> {
    pub(super) context: &'a rue_query::QueryContext,
    pub(super) parse_modules: QueryFamily<ModuleQueryKey, ParseModuleValue>,
    pub(super) module_source_bases:
        QueryFamily<ModuleQueryKey, Option<rue_air::DurableBodySourceLocator>>,
    pub(super) lookup_names: QueryFamily<LookupNameKey, LookupNameValue>,
    pub(super) lookup_imports: QueryFamily<LookupImportKey, LookupImportValue>,
    #[allow(dead_code)] // consumed by the canonical durable AIR probe authority
    pub(super) declaration_body_plan_artifacts:
        QueryFamily<DeclarationBodyPlanQueryKey, DeclarationBodyPlanArtifactsValue>,
    pub(super) semantic_nucleus: QueryFamily<
        crate::semantic_query_nucleus::SemanticNucleusKey,
        crate::semantic_query_nucleus::SemanticNucleusValue,
    >,
    pub(super) body_produced_anonymous:
        QueryFamily<crate::body_query::BodyQueryKey, crate::body_query::ProducedAnonymous>,
    pub(super) body_toolchain_demands:
        QueryFamily<crate::body_query::BodyQueryKey, crate::BodyToolchainDemand>,
    pub(super) configuration: crate::semantic_query_nucleus::SemanticQueryConfiguration,
    pub(super) status: std::rc::Rc<std::cell::RefCell<CompilerBodyProviderStatus>>,
    pub(super) deferred_anonymous_producers:
        std::rc::Rc<std::cell::RefCell<BTreeSet<crate::FunctionInstanceKey>>>,
    pub(super) producer_transport_failure: std::rc::Rc<
        std::cell::RefCell<Option<Box<crate::semantic_query_nucleus::SemanticNucleusFailure>>>,
    >,
    pub(super) observed: std::rc::Rc<std::cell::RefCell<ObservedLookupRoot>>,
    pub(super) positive_references:
        std::rc::Rc<std::cell::RefCell<BTreeSet<crate::body_query::BodyReference>>>,
    pub(super) meter: Arc<ProviderObservationCounters>,
    pub(super) shared_durable_payloads: Arc<SharedDurablePayloadCache>,
}

#[allow(dead_code)]
impl<'a> CompilerBodyProviderQueries<'a> {
    pub(super) fn with_deferred_anonymous_producers(
        mut self,
        deferred: std::rc::Rc<std::cell::RefCell<BTreeSet<crate::FunctionInstanceKey>>>,
    ) -> Self {
        self.deferred_anonymous_producers = deferred;
        self
    }

    pub(super) fn with_producer_transport_failure(
        mut self,
        failure: std::rc::Rc<
            std::cell::RefCell<Option<Box<crate::semantic_query_nucleus::SemanticNucleusFailure>>>,
        >,
    ) -> Self {
        self.producer_transport_failure = failure;
        self
    }

    pub(super) fn finish_status(&self) -> Result<(), CompilerBodyProviderStatus> {
        match self.status.borrow().clone() {
            CompilerBodyProviderStatus::Ready => Ok(()),
            status => Err(status),
        }
    }
}

pub(crate) struct CompilerBodyFactProvider<'a> {
    pub(super) queries: CompilerBodyProviderQueries<'a>,
    // One provider belongs to one query task. Its first read records the exact
    // terminal edge; an equal repeat may therefore use that already-observed
    // terminal without crossing the runtime again. Direct mapping bounds this
    // optimization independently of body size, and collisions only cause a
    // canonical query miss.
    pub(super) nucleus_cache:
        std::cell::RefCell<[Option<SemanticNucleusCacheEntry>; SEMANTIC_NUCLEUS_CACHE_SLOTS]>,
    // Name lookups have the same task-local terminal lifetime as nucleus reads.
    // Probe with borrowed text so a hit avoids allocating the owned query key.
    pub(super) lookup_name_cache:
        std::cell::RefCell<[Option<LookupNameCacheEntry>; LOOKUP_NAME_CACHE_SLOTS]>,
    #[cfg(test)]
    pub(super) nucleus_cache_hits: std::cell::Cell<u64>,
    #[cfg(test)]
    pub(super) lookup_name_cache_hits: std::cell::Cell<u64>,
    #[cfg(test)]
    pub(super) frontier_rendezvous_arrived: std::cell::Cell<bool>,
}

pub(super) struct SemanticNucleusCacheEntry {
    pub(super) key: crate::semantic_query_nucleus::SemanticNucleusKey,
    pub(crate) terminal:
        Arc<rue_query::QueryTerminal<crate::semantic_query_nucleus::SemanticNucleusValue>>,
}

pub(super) struct LookupNameCacheEntry {
    pub(super) key: LookupNameKey,
    pub(super) resolution: rue_air::NameResolution,
}

// The maintained cold scaling curve selects 16 slots. Larger capacities remove
// more query work but measurably raise peak RSS; 16 is the largest tested point
// with neutral memory. Fixed keys make cache collisions, and therefore
// published work counters, deterministic. Exact key equality remains
// authoritative at every slot.
pub(super) const SEMANTIC_NUCLEUS_CACHE_SLOTS: usize = 16;
pub(super) const SEMANTIC_NUCLEUS_CACHE_HASHER: RandomState = RandomState::with_seeds(0, 1, 2, 3);

// The cold Lattice work curve removes 33,023 query reuses at 8 slots, 35,063
// at 16, 39,537 at 32, and 40,655 at 64. Thirty-two is the knee: doubling it
// retains twice as many owned lookup keys and terminals for only 1,118 fewer
// reuses. Fixed keys keep the work reduction reproducible.
pub(super) const LOOKUP_NAME_CACHE_SLOTS: usize = 32;
pub(super) const LOOKUP_NAME_CACHE_HASHER: RandomState = RandomState::with_seeds(4, 5, 6, 7);

#[derive(Default)]
pub(super) struct CanonicalAnonymousNominalRegistry {
    // This registry is queried only by canonical identity; its entries are
    // never iterated, so map order cannot affect the durable projection.
    pub(super) by_identity:
        AHashMap<crate::AnonymousNominalKey, Rc<crate::durable_semantics::DurableAnonymousNominal>>,
    conflicting: AHashSet<crate::AnonymousNominalKey>,
}

impl CanonicalAnonymousNominalRegistry {
    /// Admit every nominal this fact carries that the registry does not already
    /// hold in at least as rich a form.
    ///
    /// Taking the nominals by reference is the point. Cloning a
    /// `DurableAnonymousNominal` copies its fields, its methods and its
    /// captures, and a by-value signature made every caller pay that copy for
    /// every nominal it offered — including the roughly one in three the
    /// registry already holds in at least as rich a form, whose copy is dropped
    /// unread. The copy now happens where the decision is made.
    pub(super) fn extend<'nominal>(
        &mut self,
        nominals: impl IntoIterator<Item = &'nominal crate::durable_semantics::DurableAnonymousNominal>,
    ) {
        for nominal in nominals {
            let nominal = nominal.with_canonical_identity();
            let identity = nominal.identity.clone();
            if self.conflicting.contains(&identity) {
                continue;
            }
            match self.by_identity.entry(identity.clone()) {
                std::collections::hash_map::Entry::Vacant(entry) => {
                    entry.insert(Rc::new(nominal));
                }
                std::collections::hash_map::Entry::Occupied(mut entry) => {
                    // Declaration and producer-body projections may omit
                    // capture/method metadata they do not consume. Reconcile
                    // only that explicit thin/rich relation; disagreeing
                    // shapes or two different non-empty metadata payloads
                    // poison the identity permanently.
                    match crate::durable_semantics::reconcile_anonymous_nominals(
                        entry.get(),
                        &nominal,
                    ) {
                        Ok(reconciled) => {
                            if entry.get().as_ref() != &reconciled {
                                entry.insert(Rc::new(reconciled));
                            }
                        }
                        Err(_) => {
                            entry.remove();
                            self.conflicting.insert(identity);
                        }
                    }
                }
            }
        }
    }

    pub(super) fn get(
        &self,
        identity: &crate::AnonymousNominalKey,
    ) -> Result<
        Option<Rc<crate::durable_semantics::DurableAnonymousNominal>>,
        crate::AnonymousNominalKey,
    > {
        let identity = identity.with_canonical_producer();
        if self.conflicting.contains(identity.as_ref()) {
            return Err(identity.into_owned());
        }
        Ok(self.by_identity.get(identity.as_ref()).cloned())
    }
}

#[derive(Default)]
pub(super) struct BodyDurablePayloadCache {
    /// One body transaction can ask for the same canonical declaration payload
    /// through type minting, endpoint installation, and export. Keep the exact
    /// provider result once per durable key so those consumers share immutable
    /// signature payloads instead of rebuilding their candidate projection.
    pub(super) named_nominals: AHashMap<
        crate::StableDefinitionKey,
        rue_air::DurableNominal<crate::StableDefinitionKey, ModuleId>,
    >,
    pub(super) named_functions: AHashMap<
        crate::StableDefinitionKey,
        rue_air::DurableFunction<crate::StableDefinitionKey, ModuleId>,
    >,
}

#[derive(Clone, PartialEq, Eq, Hash)]
pub(super) struct SharedPayloadGeneration {
    pub(super) revision: rue_query::Revision,
    pub(super) configuration: crate::semantic_query_nucleus::SemanticQueryConfiguration,
}

pub(super) struct SharedNominalPayload {
    pub(super) nominal: Arc<rue_air::DurableNominal<crate::StableDefinitionKey, ModuleId>>,
    pub(crate) anonymous_nominals: Arc<[crate::durable_semantics::DurableAnonymousNominal]>,
}

pub(super) struct SharedFunctionPayload {
    pub(super) function: Arc<rue_air::DurableFunction<crate::StableDefinitionKey, ModuleId>>,
    pub(super) anonymous_nominals: Arc<[crate::durable_semantics::DurableAnonymousNominal]>,
}

#[derive(Default)]
pub(super) struct SharedDurablePayloadCacheState {
    pub(super) enabled: bool,
    pub(super) generation: Option<SharedPayloadGeneration>,
    pub(super) named_nominals: AHashMap<crate::StableDefinitionKey, SharedNominalPayload>,
    pub(super) named_functions: AHashMap<crate::StableDefinitionKey, SharedFunctionPayload>,
}

#[derive(Default)]
pub(super) struct SharedDurablePayloadCache {
    pub(super) state: Mutex<SharedDurablePayloadCacheState>,
}

impl SharedDurablePayloadCache {
    pub(super) fn reset(&self, enabled: bool) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.enabled = enabled;
        state.generation = None;
        state.named_nominals.clear();
        state.named_functions.clear();
    }

    pub(super) fn prepare(
        state: &mut SharedDurablePayloadCacheState,
        generation: SharedPayloadGeneration,
    ) {
        if state.generation.as_ref() != Some(&generation) {
            state.generation = Some(generation);
            state.named_nominals.clear();
            state.named_functions.clear();
        }
    }

    pub(super) fn nominal(
        &self,
        generation: SharedPayloadGeneration,
        key: &crate::StableDefinitionKey,
    ) -> Option<(
        Arc<rue_air::DurableNominal<crate::StableDefinitionKey, ModuleId>>,
        Arc<[crate::durable_semantics::DurableAnonymousNominal]>,
    )> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !state.enabled {
            return None;
        }
        Self::prepare(&mut state, generation);
        state.named_nominals.get(key).map(|payload| {
            (
                Arc::clone(&payload.nominal),
                Arc::clone(&payload.anonymous_nominals),
            )
        })
    }

    pub(super) fn insert_nominal(
        &self,
        generation: SharedPayloadGeneration,
        key: crate::StableDefinitionKey,
        nominal: Arc<rue_air::DurableNominal<crate::StableDefinitionKey, ModuleId>>,
        anonymous_nominals: Arc<[crate::durable_semantics::DurableAnonymousNominal]>,
    ) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !state.enabled {
            return;
        }
        Self::prepare(&mut state, generation);
        state
            .named_nominals
            .entry(key)
            .or_insert(SharedNominalPayload {
                nominal,
                anonymous_nominals,
            });
    }

    pub(super) fn function(
        &self,
        generation: SharedPayloadGeneration,
        key: &crate::StableDefinitionKey,
    ) -> Option<(
        Arc<rue_air::DurableFunction<crate::StableDefinitionKey, ModuleId>>,
        Arc<[crate::durable_semantics::DurableAnonymousNominal]>,
    )> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !state.enabled {
            return None;
        }
        Self::prepare(&mut state, generation);
        state.named_functions.get(key).map(|payload| {
            (
                Arc::clone(&payload.function),
                Arc::clone(&payload.anonymous_nominals),
            )
        })
    }

    pub(super) fn insert_function(
        &self,
        generation: SharedPayloadGeneration,
        key: crate::StableDefinitionKey,
        function: Arc<rue_air::DurableFunction<crate::StableDefinitionKey, ModuleId>>,
        anonymous_nominals: Arc<[crate::durable_semantics::DurableAnonymousNominal]>,
    ) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !state.enabled {
            return;
        }
        Self::prepare(&mut state, generation);
        state
            .named_functions
            .entry(key)
            .or_insert(SharedFunctionPayload {
                function,
                anonymous_nominals,
            });
    }
}

#[derive(Clone)]
pub(crate) struct CompilerBodyDurableSource<'a> {
    pub(super) provider: &'a CompilerBodyFactProvider<'a>,
    pub(super) dynamic_anonymous: Rc<std::cell::RefCell<CanonicalAnonymousNominalRegistry>>,
    pub(super) durable_payloads: Rc<std::cell::RefCell<BodyDurablePayloadCache>>,
    pub(super) source_paths: Rc<std::cell::RefCell<AHashMap<crate::FileId, Arc<str>>>>,
    /// Visibility domains derived from `source_paths`, memoized per file
    /// (RUE-1840). Lives and dies with the path table it is derived from.
    pub(super) visibility_domains: Rc<rue_air::SemanticVisibilityDomainCache<crate::FileId>>,
    pub(crate) source_locators:
        Rc<std::cell::RefCell<AHashMap<ModuleId, rue_air::DurableBodySourceLocator>>>,
}

pub(super) struct ResolvedDeclarationCandidate {
    pub(super) declaration: crate::declaration_candidate::DeclarationCandidateKey,
    pub(super) identity: crate::semantic_query_nucleus::DeclarationIdentityProjection,
}

impl<'a> CompilerBodyDurableSource<'a> {
    #[allow(dead_code)]
    pub(super) fn with_anonymous(
        provider: &'a CompilerBodyFactProvider<'a>,
        anonymous: &'a [crate::durable_semantics::DurableAnonymousNominal],
        owner_source: Option<(ModuleId, rue_air::DurableBodySourceLocator)>,
    ) -> Self {
        let mut source_paths = AHashMap::new();
        let mut source_locators = AHashMap::new();
        let mut dynamic_anonymous = CanonicalAnonymousNominalRegistry::default();
        dynamic_anonymous.extend(anonymous.iter());
        if let Some((module, locator)) = owner_source {
            source_paths.insert(locator.file_id, locator.physical_path.clone());
            source_locators.insert(module, locator);
        }
        Self {
            provider,
            dynamic_anonymous: Rc::new(std::cell::RefCell::new(dynamic_anonymous)),
            durable_payloads: Rc::new(std::cell::RefCell::new(BodyDurablePayloadCache::default())),
            source_paths: Rc::new(std::cell::RefCell::new(source_paths)),
            visibility_domains: Rc::new(rue_air::SemanticVisibilityDomainCache::default()),
            source_locators: Rc::new(std::cell::RefCell::new(source_locators)),
        }
    }

    pub(super) fn candidate(
        &self,
        key: &crate::StableDefinitionKey,
    ) -> Option<ResolvedDeclarationCandidate> {
        use rue_air::BodyFactProvider;
        stable_syntax_candidate_set(key)?
            .into_iter()
            .flatten()
            .find_map(|declaration| {
                let identity = self.provider.declaration_identity(&declaration)?;
                (identity.key == *key).then_some(ResolvedDeclarationCandidate {
                    declaration,
                    identity,
                })
            })
    }

    pub(super) fn shared_payload_generation(&self) -> SharedPayloadGeneration {
        SharedPayloadGeneration {
            revision: self.provider.queries.context.revision(),
            configuration: self.provider.queries.configuration.clone(),
        }
    }

    fn try_anonymous_nominal(
        &self,
        key: &crate::AnonymousNominalKey,
    ) -> Result<
        Option<Rc<crate::durable_semantics::DurableAnonymousNominal>>,
        crate::AnonymousNominalKey,
    > {
        use rue_air::BodyFactProvider;
        let cached = self.dynamic_anonymous.borrow().get(key)?;
        let cached_has_methods = cached.as_ref().is_some_and(|nominal| {
            matches!(
                &nominal.shape,
                crate::durable_semantics::DurableAnonymousNominalShape::Struct {
                    methods,
                    ..
                } if !methods.is_empty()
            )
        });
        if cached_has_methods {
            return Ok(cached);
        }
        let body_producer: Option<std::borrow::Cow<'_, crate::FunctionInstanceKey>> =
            match &key.producer {
                crate::StableProducerId::Function(function) => {
                    Some(std::borrow::Cow::Borrowed(function.as_ref()))
                }
                crate::StableProducerId::Definition(definition)
                    if definition.kind() == crate::StableDefinitionKind::Function =>
                {
                    Some(std::borrow::Cow::Owned(
                        crate::FunctionInstanceKey::Definition(definition.clone()),
                    ))
                }
                crate::StableProducerId::Definition(_) => None,
            };
        if let Some(function) = body_producer {
            match self.provider.producer_body_facts(function.as_ref()) {
                Some(crate::body_query::ProducedAnonymous::Produced(produced)) => {
                    let mut dynamic = self.dynamic_anonymous.borrow_mut();
                    dynamic.extend(produced.0.iter());
                    if let Some(nominal) = dynamic.get(key)? {
                        return Ok(Some(nominal));
                    }
                }
                Some(crate::body_query::ProducedAnonymous::ProducerFailed(_)) | None => {}
            }
        }
        // A delegating `-> type` function can return an anonymous nominal
        // produced by a different nullary function. Its own comptime-call
        // projection carries the returned identity but does not claim the
        // nested producer's shape as locally produced, and ordinary body facts
        // likewise contain no entry for that foreign producer. Re-query the
        // exact nullary producer so its authoritative comptime projection can
        // populate this source's dynamic anonymous-shape registry.
        if let crate::StableProducerId::Function(function) = &key.producer
            && let crate::FunctionInstanceKey::Definition(definition) = function.as_ref()
        {
            let _ = <Self as rue_air::DurableBodyLookupSource<
                crate::StableDefinitionKey,
                ModuleId,
            >>::reduce_comptime_call(self, definition, &[], &[]);
            if let Some(nominal) = self.dynamic_anonymous.borrow().get(key)? {
                return Ok(Some(nominal));
            }
        }
        let producer = match &key.producer {
            crate::StableProducerId::Definition(definition) => definition,
            crate::StableProducerId::Function(function) => {
                let Some(producer) = function_definition_key(function) else {
                    return Ok(None);
                };
                producer
            }
        };
        let facts = self
            .candidate(producer)
            .and_then(|candidate| self.provider.anonymous_facts(&candidate.declaration))
            .unwrap_or_default();
        let mut dynamic = self.dynamic_anonymous.borrow_mut();
        dynamic.extend(facts.iter());
        dynamic.get(key)
    }

    pub(super) fn anonymous_nominal(
        &self,
        key: &crate::AnonymousNominalKey,
    ) -> Option<Rc<crate::durable_semantics::DurableAnonymousNominal>> {
        match self.try_anonymous_nominal(key) {
            Ok(nominal) => nominal,
            Err(identity) => {
                *self
                    .provider
                    .queries
                    .producer_transport_failure
                    .borrow_mut() = Some(Box::new(
                    crate::semantic_query_nucleus::SemanticNucleusFailure::Resolution(Arc::from(
                        format!("conflicting durable anonymous facts for {identity:?}"),
                    )),
                ));
                self.provider.observe_abort(QueryAbort::Canceled);
                None
            }
        }
    }

    pub(super) fn signature(
        &self,
        key: &crate::StableDefinitionKey,
    ) -> Option<crate::semantic_query_nucleus::ResolvedDeclarationSignature> {
        let candidate = self.candidate(key)?;
        self.signature_for_candidate(&candidate.declaration)
    }

    pub(super) fn signature_for_candidate(
        &self,
        candidate: &crate::declaration_candidate::DeclarationCandidateKey,
    ) -> Option<crate::semantic_query_nucleus::ResolvedDeclarationSignature> {
        use rue_air::BodyFactProvider;
        let signature = self.provider.signature(candidate)?;
        self.dynamic_anonymous
            .borrow_mut()
            .extend(signature.anonymous_nominals.iter());
        Some(signature)
    }

    pub(super) fn module_binding_from(
        &self,
        module: &ModuleId,
        name: &str,
        qualified: bool,
    ) -> Option<rue_air::DurableBodyModuleBinding<crate::StableDefinitionKey, ModuleId>> {
        use rue_air::BodyFactProvider;
        let resolution = if qualified {
            self.provider
                .lookup_qualified(module, rue_air::ProviderNamespace::ModuleItem, name)
        } else {
            self.provider
                .lookup_unqualified(module, rue_air::ProviderNamespace::ModuleItem, name)
        };
        let rue_air::NameResolution::Unique(candidate) =
            resolution.of_kind(rue_air::ProviderDefinitionKind::Const)
        else {
            return None;
        };
        let declaration = crate::declaration_candidate::DeclarationCandidateKey {
            module: module.clone(),
            category: crate::declaration_candidate::DeclarationCandidateCategory::ConstCandidate,
            name: Arc::from(name),
            owner: None,
            duplicate_discriminator: 0,
        };
        let crate::semantic_query_nucleus::ConstResolutionProjection::ModuleBinding { key, target } =
            self.provider.const_comptime(&declaration)?
        else {
            return None;
        };
        Some(rue_air::DurableBodyModuleBinding {
            definition: key,
            target,
            is_public: candidate.is_public,
        })
    }
}

pub(super) fn unique_named_member_candidate<D>(
    candidates: Vec<rue_air::MemberCandidate<D>>,
) -> Option<rue_air::MemberCandidate<D>> {
    // Uniqueness spans the complete method + associated-function namespace;
    // selecting within either receiver class first would hide mixed ambiguity.
    let mut candidates = candidates.into_iter();
    let candidate = candidates.next()?;
    candidates.next().is_none().then_some(candidate)
}

impl rue_air::DurableBodyLookupSource<crate::StableDefinitionKey, ModuleId>
    for CompilerBodyDurableSource<'_>
{
    fn definition_module(&self, definition: &crate::StableDefinitionKey) -> Option<ModuleId> {
        Some(definition.module().clone())
    }

    fn anonymous_definition_module(
        &self,
        identity: &rue_air::AnonymousNominalKey<crate::StableDefinitionKey, ModuleId>,
    ) -> Option<ModuleId> {
        fn function_module(
            function: &rue_air::FunctionInstanceKey<crate::StableDefinitionKey, ModuleId>,
        ) -> Option<ModuleId> {
            match function {
                rue_air::FunctionInstanceKey::Definition(definition) => {
                    Some(definition.module().clone())
                }
                rue_air::FunctionInstanceKey::Specialization { base, .. } => function_module(base),
                rue_air::FunctionInstanceKey::AnonymousMember { .. }
                | rue_air::FunctionInstanceKey::DropGlue(_) => None,
            }
        }
        match &identity.producer {
            rue_air::StableProducerId::Definition(definition) => Some(definition.module().clone()),
            rue_air::StableProducerId::Function(function) => function_module(function),
        }
    }

    fn free_function(
        &self,
        current: &crate::StableDefinitionKey,
        name: &str,
    ) -> Option<crate::StableDefinitionKey> {
        use rue_air::BodyFactProvider;
        let resolution = self.provider.lookup_unqualified(
            current.module(),
            rue_air::ProviderNamespace::ModuleItem,
            name,
        );
        let rue_air::NameResolution::Unique(_) =
            resolution.of_kind(rue_air::ProviderDefinitionKind::Function)
        else {
            return None;
        };
        Some(crate::StableDefinitionKey::from_stable_parts(
            current.module().clone(),
            crate::StableDefinitionNamespace::Value,
            crate::StableDefinitionKind::Function,
            Arc::from(name),
            None,
        ))
    }

    fn value_const(
        &self,
        current: &crate::StableDefinitionKey,
        name: &str,
    ) -> Option<crate::StableDefinitionKey> {
        use rue_air::BodyFactProvider;
        let resolution = self.provider.lookup_unqualified(
            current.module(),
            rue_air::ProviderNamespace::ModuleItem,
            name,
        );
        let rue_air::NameResolution::Unique(_) =
            resolution.of_kind(rue_air::ProviderDefinitionKind::Const)
        else {
            return None;
        };
        let declaration = crate::declaration_candidate::DeclarationCandidateKey {
            module: current.module().clone(),
            category: crate::declaration_candidate::DeclarationCandidateCategory::ConstCandidate,
            name: Arc::from(name),
            owner: None,
            duplicate_discriminator: 0,
        };
        let crate::semantic_query_nucleus::ConstResolutionProjection::Value { key, .. } =
            self.provider.const_comptime(&declaration)?
        else {
            return None;
        };
        Some(key)
    }

    fn nominal(
        &self,
        current: &crate::StableDefinitionKey,
        name: &str,
    ) -> Option<(crate::StableDefinitionKey, crate::StableDefinitionKind)> {
        use rue_air::BodyFactProvider;
        let resolution = self.provider.lookup_unqualified(
            current.module(),
            rue_air::ProviderNamespace::ModuleItem,
            name,
        );
        let kind = match (
            resolution.of_kind(rue_air::ProviderDefinitionKind::Struct),
            resolution.of_kind(rue_air::ProviderDefinitionKind::Enum),
        ) {
            (rue_air::NameResolution::Unique(_), rue_air::NameResolution::Absent) => {
                crate::StableDefinitionKind::Struct
            }
            (rue_air::NameResolution::Absent, rue_air::NameResolution::Unique(_)) => {
                crate::StableDefinitionKind::Enum
            }
            _ => return None,
        };
        Some((
            crate::StableDefinitionKey::from_stable_parts(
                current.module().clone(),
                crate::StableDefinitionNamespace::Type,
                kind,
                Arc::from(name),
                None,
            ),
            kind,
        ))
    }

    fn named_member(
        &self,
        current: &crate::StableDefinitionKey,
        owner: &str,
        name: &str,
    ) -> Option<(crate::StableDefinitionKey, bool)> {
        use rue_air::BodyFactProvider;
        let receiver = ReceiverTypeIdentity::new(
            current.module().clone(),
            owner,
            crate::declaration_candidate::DeclarationCandidateCategory::Struct,
        );
        let candidate =
            unique_named_member_candidate(self.provider.method_candidates(&receiver, name))?;
        let has_self = candidate.has_self_receiver;
        let identity = self.provider.declaration_identity(&candidate.declaration)?;
        Some((identity.key, has_self))
    }

    fn root_module_binding(
        &self,
        current: &crate::StableDefinitionKey,
        name: &str,
    ) -> Option<rue_air::DurableBodyModuleBinding<crate::StableDefinitionKey, ModuleId>> {
        self.module_binding_from(current.module(), name, false)
    }

    fn module_binding(
        &self,
        module: &ModuleId,
        name: &str,
    ) -> Option<rue_air::DurableBodyModuleBinding<crate::StableDefinitionKey, ModuleId>> {
        self.module_binding_from(module, name, true)
    }

    fn qualified_free_function(
        &self,
        module: &ModuleId,
        name: &str,
    ) -> Option<crate::StableDefinitionKey> {
        use rue_air::BodyFactProvider;
        let resolution =
            self.provider
                .lookup_qualified(module, rue_air::ProviderNamespace::ModuleItem, name);
        let rue_air::NameResolution::Unique(_) =
            resolution.of_kind(rue_air::ProviderDefinitionKind::Function)
        else {
            return None;
        };
        Some(crate::StableDefinitionKey::from_stable_parts(
            module.clone(),
            crate::StableDefinitionNamespace::Value,
            crate::StableDefinitionKind::Function,
            Arc::from(name),
            None,
        ))
    }

    fn qualified_value_const(
        &self,
        module: &ModuleId,
        name: &str,
    ) -> Option<crate::StableDefinitionKey> {
        use rue_air::BodyFactProvider;
        let resolution =
            self.provider
                .lookup_qualified(module, rue_air::ProviderNamespace::ModuleItem, name);
        let rue_air::NameResolution::Unique(_) =
            resolution.of_kind(rue_air::ProviderDefinitionKind::Const)
        else {
            return None;
        };
        let declaration = crate::declaration_candidate::DeclarationCandidateKey {
            module: module.clone(),
            category: crate::declaration_candidate::DeclarationCandidateCategory::ConstCandidate,
            name: Arc::from(name),
            owner: None,
            duplicate_discriminator: 0,
        };
        let crate::semantic_query_nucleus::ConstResolutionProjection::Value { key, .. } =
            self.provider.const_comptime(&declaration)?
        else {
            return None;
        };
        Some(key)
    }

    fn qualified_nominal(
        &self,
        module: &ModuleId,
        name: &str,
    ) -> Option<(crate::StableDefinitionKey, crate::StableDefinitionKind)> {
        use rue_air::BodyFactProvider;
        let resolution =
            self.provider
                .lookup_qualified(module, rue_air::ProviderNamespace::ModuleItem, name);
        let kind = match (
            resolution.of_kind(rue_air::ProviderDefinitionKind::Struct),
            resolution.of_kind(rue_air::ProviderDefinitionKind::Enum),
        ) {
            (rue_air::NameResolution::Unique(_), rue_air::NameResolution::Absent) => {
                crate::StableDefinitionKind::Struct
            }
            (rue_air::NameResolution::Absent, rue_air::NameResolution::Unique(_)) => {
                crate::StableDefinitionKind::Enum
            }
            _ => return None,
        };
        Some((
            crate::StableDefinitionKey::from_stable_parts(
                module.clone(),
                crate::StableDefinitionNamespace::Type,
                kind,
                Arc::from(name),
                None,
            ),
            kind,
        ))
    }

    fn language_item_nominal(
        &self,
        current: &crate::StableDefinitionKey,
        lang_item: rue_air::LangItem,
    ) -> Option<crate::StableDefinitionKey> {
        use rue_air::BodyFactProvider;
        // The canonical trusted-standard-library home of each language item. A
        // language item's defining module is fixed by the toolchain, so the
        // last-resort fallback consults that path directly.
        let (name, trusted_path) = match lang_item {
            rue_air::LangItem::StrBuf => ("StrBuf", "\0rue-std/strbuf.rue"),
        };
        let module = self
            .canonical_import(current, "std/strbuf.rue")
            .or_else(|| {
                let instance = crate::FunctionInstanceKey::Definition(current.clone());
                self.provider
                    .trusted_toolchain_facts(&instance)
                    .modules()
                    .iter()
                    .find(|demand| {
                        demand.logical_path()
                            == crate::toolchain_module_demand::STRBUF_MODULE_LOGICAL_PATH
                    })
                    .and_then(|demand| demand.trusted_module_id().ok())
            })
            .or_else(|| {
                ModuleId::from_trusted_standard_library_path(trusted_path)
                    .ok()
                    .filter(|module| self.provider.has_module_source(module))
            })?;
        let (key, kind) = self.qualified_nominal(&module, name)?;
        (kind == crate::StableDefinitionKind::Struct).then_some(key)
    }

    fn module_path(&self, module: &ModuleId) -> String {
        module.logical_path().to_owned()
    }

    fn definition_source(
        &self,
        definition: &crate::StableDefinitionKey,
    ) -> Option<rue_air::DurableBodySourceLocator> {
        self.module_source(definition.module())
    }

    fn module_source(&self, module: &ModuleId) -> Option<rue_air::DurableBodySourceLocator> {
        if let Some(locator) = self.source_locators.borrow().get(module).cloned() {
            return Some(locator);
        }
        let locator = self.provider.source_locator(module)?;
        self.source_paths
            .borrow_mut()
            .insert(locator.file_id, locator.physical_path.clone());
        self.source_locators
            .borrow_mut()
            .insert(module.clone(), locator.clone());
        Some(locator)
    }

    fn source_path(&self, file: crate::FileId) -> Option<Arc<str>> {
        self.source_paths.borrow().get(&file).cloned()
    }

    /// Memoized override of the deriving default: `from_file_path` is a path
    /// parse and an `Arc<str>` allocation, and the resolver asks for the same
    /// few files once per named-type resolution (RUE-1840).
    fn visibility_domain(&self, file: crate::FileId) -> Option<rue_air::SemanticVisibilityDomain> {
        self.visibility_domains
            .domain(file, || self.source_path(file))
    }

    fn out_of_scope_integer_const_paths(
        &self,
        current: &crate::StableDefinitionKey,
        name: &str,
    ) -> Vec<Arc<str>> {
        let mut pending = vec![current.module().clone()];
        let mut visited = BTreeSet::new();
        let mut paths = Vec::new();
        while let Some(module) = pending.pop() {
            if !visited.insert(module.clone()) {
                continue;
            }
            let Some(specifiers) = self.provider.import_specifiers(&module) else {
                continue;
            };
            for specifier in specifiers {
                let Some(target) = self.provider.import_target(&module, &specifier) else {
                    continue;
                };
                pending.push(target.clone());
                let Some(key) = self.qualified_value_const(&target, name) else {
                    continue;
                };
                let Some(constant) = rue_air::DurableConstSource::constant(self, &key) else {
                    continue;
                };
                if constant.is_public
                    && matches!(
                        constant.value,
                        rue_air::SemanticImportConstValue::Integer(_)
                    )
                    && let Some(source) = self.module_source(&target)
                {
                    paths.push(source.physical_path);
                }
            }
        }
        paths
    }

    fn foreign_function_module(
        &self,
        current: &crate::StableDefinitionKey,
        function: &crate::StableDefinitionKey,
    ) -> Option<ModuleId> {
        (current.module() != function.module()).then(|| function.module().clone())
    }

    fn foreign_definition_module(
        &self,
        current: &crate::StableDefinitionKey,
        definition: &crate::StableDefinitionKey,
    ) -> Option<ModuleId> {
        (current.module() != definition.module()).then(|| definition.module().clone())
    }

    fn definition_kind(
        &self,
        definition: &crate::StableDefinitionKey,
    ) -> Option<crate::StableDefinitionKind> {
        Some(definition.kind())
    }

    fn definition_owner_name(&self, definition: &crate::StableDefinitionKey) -> Option<Arc<str>> {
        definition.owner().map(|owner| owner.shared_name().clone())
    }

    fn canonical_import(
        &self,
        current: &crate::StableDefinitionKey,
        specifier: &str,
    ) -> Option<ModuleId> {
        self.provider.import_target(current.module(), specifier)
    }

    fn trusted_try_producer(
        &self,
        identity: &crate::AnonymousNominalKey,
    ) -> Option<rue_air::DurableTryProducer> {
        let definition = match &identity.producer {
            crate::StableProducerId::Definition(definition) => definition,
            crate::StableProducerId::Function(function) => function_definition_key(function)?,
        };
        if definition.owner().is_some()
            || definition.kind() != crate::StableDefinitionKind::Function
            || !definition.module().is_trusted_standard_library()
        {
            return None;
        }
        match (definition.module().logical_path(), definition.name()) {
            ("\0rue-std/option.rue", "Option") => Some(rue_air::DurableTryProducer::Option),
            ("\0rue-std/result.rue", "Result") => Some(rue_air::DurableTryProducer::Result),
            _ => None,
        }
    }

    fn definition_name(&self, definition: &crate::StableDefinitionKey) -> Option<Arc<str>> {
        Some(definition.shared_name().clone())
    }

    fn reduce_comptime_call(
        &self,
        definition: &crate::StableDefinitionKey,
        type_arguments: &[(Arc<str>, crate::DurableType)],
        value_arguments: &[(Arc<str>, crate::DurableConstValue)],
    ) -> rue_air::DurableComptimeCallOutcome<crate::StableDefinitionKey, ModuleId> {
        let Some(candidate) = self.candidate(definition) else {
            return rue_air::DurableComptimeCallOutcome::NotReduced;
        };
        let declaration = self.provider.declaration_query_key(&candidate.declaration);
        let query = crate::semantic_query_nucleus::SemanticNucleusKey::ComptimeCall(
            crate::semantic_query_nucleus::ComptimeCallQueryKey {
                declaration: declaration.clone(),
                type_arguments: type_arguments.to_vec().into(),
                value_arguments: value_arguments.to_vec().into(),
            },
        );
        let value = match self.provider.nucleus_result(query) {
            Ok(Some(value)) => value,
            Ok(None) => return rue_air::DurableComptimeCallOutcome::NotReduced,
            Err(QueryAbort::Cycle(_)) => {
                return rue_air::DurableComptimeCallOutcome::Diagnostic(
                    rue_air::DurableComptimeDiagnostic {
                        kind: rue_error::ErrorKind::ComptimeEvaluationFailed {
                            reason: format!(
                                "specialization of '{}' exceeded the maximum nesting depth ({}); is a comptime-recursive function missing a compile-time-known base case, or a generic function recursively instantiating itself with new types?",
                                definition.name(),
                                rue_air::MAX_COMPTIME_CALL_DEPTH,
                            ),
                        },
                        span: None,
                    },
                );
            }
            Err(abort) => {
                self.provider.observe_abort(abort);
                return rue_air::DurableComptimeCallOutcome::NotReduced;
            }
        };
        let projection = match value {
            crate::semantic_query_nucleus::SemanticNucleusValue::ComptimeCall(projection) => {
                projection
            }
            crate::semantic_query_nucleus::SemanticNucleusValue::Failure(failure)
                if semantic_nucleus_failure_is_internal_error(&failure) =>
            {
                *self
                    .provider
                    .queries
                    .producer_transport_failure
                    .borrow_mut() = Some(Box::new(failure));
                self.provider.observe_abort(QueryAbort::Canceled);
                return rue_air::DurableComptimeCallOutcome::NotReduced;
            }
            crate::semantic_query_nucleus::SemanticNucleusValue::Failure(
                crate::semantic_query_nucleus::SemanticNucleusFailure::Diagnostic(kind),
            ) => {
                return rue_air::DurableComptimeCallOutcome::Diagnostic(
                    rue_air::DurableComptimeDiagnostic { kind, span: None },
                );
            }
            crate::semantic_query_nucleus::SemanticNucleusValue::Failure(
                crate::semantic_query_nucleus::SemanticNucleusFailure::DiagnosticAtProducerRange {
                    kind,
                    producer,
                    start,
                    end,
                },
            ) => {
                let span = self.provider.producer_relative_span(&producer, start, end);
                return rue_air::DurableComptimeCallOutcome::Diagnostic(
                    rue_air::DurableComptimeDiagnostic { kind, span },
                );
            }
            crate::semantic_query_nucleus::SemanticNucleusValue::Failure(
                crate::semantic_query_nucleus::SemanticNucleusFailure::Cycle(_),
            ) => {
                return rue_air::DurableComptimeCallOutcome::Diagnostic(
                    rue_air::DurableComptimeDiagnostic {
                        kind: rue_error::ErrorKind::ComptimeEvaluationFailed {
                            reason: format!(
                                "specialization of '{}' exceeded the maximum nesting depth ({}); is a comptime-recursive function missing a compile-time-known base case, or a generic function recursively instantiating itself with new types?",
                                definition.name(),
                                rue_air::MAX_COMPTIME_CALL_DEPTH,
                            ),
                        },
                        span: None,
                    },
                );
            }
            _ => return rue_air::DurableComptimeCallOutcome::NotReduced,
        };
        for gate in projection.deferred_ownership.iter() {
            let ownership = match self.provider.nucleus_result(
                crate::semantic_query_nucleus::SemanticNucleusKey::DeferredOwnership(
                    crate::semantic_query_nucleus::DeferredOwnershipQueryKey {
                        producer: declaration.clone(),
                        gate: gate.clone(),
                    },
                ),
            ) {
                Ok(Some(value)) => Some(value),
                Ok(None) | Err(QueryAbort::Cycle(_)) => None,
                Err(abort) => {
                    self.provider.observe_abort(abort);
                    return rue_air::DurableComptimeCallOutcome::NotReduced;
                }
            };
            match ownership {
                None => {}
                Some(crate::semantic_query_nucleus::SemanticNucleusValue::DeferredOwnership) => {}
                Some(crate::semantic_query_nucleus::SemanticNucleusValue::Failure(
                    crate::semantic_query_nucleus::SemanticNucleusFailure::OwnershipGate {
                        kind,
                        gate,
                    },
                )) => {
                    let span = self.provider.producer_relative_span(
                        &gate.source.declaration,
                        gate.source.start,
                        gate.source.end,
                    );
                    return rue_air::DurableComptimeCallOutcome::Diagnostic(
                        rue_air::DurableComptimeDiagnostic { kind, span },
                    );
                }
                Some(crate::semantic_query_nucleus::SemanticNucleusValue::Failure(failure))
                    if semantic_nucleus_failure_is_internal_error(&failure) =>
                {
                    *self
                        .provider
                        .queries
                        .producer_transport_failure
                        .borrow_mut() = Some(Box::new(failure));
                    self.provider.observe_abort(QueryAbort::Canceled);
                    return rue_air::DurableComptimeCallOutcome::NotReduced;
                }
                Some(_) => {}
            }
        }
        if !projection.anonymous_nominals.is_empty() {
            let producer = match crate::durable_comptime::canonical_specialized_function_instance(
                definition,
                type_arguments,
                value_arguments,
            ) {
                Ok(producer) => producer,
                Err(_) => return rue_air::DurableComptimeCallOutcome::NotReduced,
            };
            self.provider
                .queries
                .positive_references
                .borrow_mut()
                .insert(crate::body_query::BodyReference::Callable(producer.clone()));
            let Some(produced_facts) =
                rue_air::BodyFactProvider::producer_body_facts(self.provider, &producer)
            else {
                return rue_air::DurableComptimeCallOutcome::NotReduced;
            };
            match produced_facts {
                crate::body_query::ProducedAnonymous::Produced(produced) => {
                    self.dynamic_anonymous
                        .borrow_mut()
                        .extend(produced.0.iter());
                }
                crate::body_query::ProducedAnonymous::ProducerFailed(failure) => {
                    *self
                        .provider
                        .queries
                        .producer_transport_failure
                        .borrow_mut() = Some(failure);
                    self.provider.observe_abort(QueryAbort::Canceled);
                    return rue_air::DurableComptimeCallOutcome::NotReduced;
                }
            }
        }
        let result = match projection.result {
            crate::semantic_query_nucleus::ComptimeCallResultProjection::Type(ty) => {
                rue_air::SemanticComptimeCallResult::Type(ty)
            }
            crate::semantic_query_nucleus::ComptimeCallResultProjection::Value(value) => {
                rue_air::SemanticComptimeCallResult::Value(value)
            }
        };
        rue_air::DurableComptimeCallOutcome::Reduced(rue_air::DurableReducedComptimeCall { result })
    }
}

impl rue_air::DurableConstSource<crate::StableDefinitionKey, ModuleId>
    for CompilerBodyDurableSource<'_>
{
    fn constant(
        &self,
        key: &crate::StableDefinitionKey,
    ) -> Option<rue_air::DurableConst<crate::StableDefinitionKey, ModuleId>> {
        self.provider
            .meter()
            .const_materializations
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        use rue_air::BodyFactProvider;
        let candidate = self.candidate(key)?;
        let crate::semantic_query_nucleus::ConstResolutionProjection::Value {
            ty,
            value,
            anonymous_nominals,
            ..
        } = self.provider.const_comptime(&candidate.declaration)?
        else {
            return None;
        };
        self.dynamic_anonymous
            .borrow_mut()
            .extend(anonymous_nominals.iter());
        Some(rue_air::DurableConst {
            is_public: candidate.identity.is_public,
            ty,
            value: *value,
        })
    }

    fn function_name(&self, key: &crate::StableDefinitionKey) -> Option<Arc<str>> {
        (key.kind() == crate::StableDefinitionKind::Function).then(|| Arc::from(key.name()))
    }
}

impl rue_air::DurableNominalSource<crate::StableDefinitionKey, ModuleId>
    for CompilerBodyDurableSource<'_>
{
    fn module_is_trusted_standard_library(&self, module: &ModuleId) -> bool {
        module.is_trusted_standard_library()
    }

    fn nominal_file_id(&self, key: &crate::StableDefinitionKey) -> Option<crate::FileId> {
        rue_air::DurableBodyLookupSource::module_source(self, key.module())
            .map(|source| source.file_id)
    }

    fn nominal(
        &self,
        key: &crate::StableDefinitionKey,
    ) -> Option<rue_air::DurableNominal<crate::StableDefinitionKey, ModuleId>> {
        if let Some(nominal) = self
            .durable_payloads
            .borrow()
            .named_nominals
            .get(key)
            .cloned()
        {
            self.provider
                .meter()
                .nominal_materialization_reuses
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            return Some(nominal);
        }
        if let Some((nominal, anonymous_nominals)) = self
            .provider
            .queries
            .shared_durable_payloads
            .nominal(self.shared_payload_generation(), key)
        {
            let candidate = self.candidate(key)?;
            let _signature = self.signature_for_candidate(&candidate.declaration)?;
            self.dynamic_anonymous
                .borrow_mut()
                .extend(anonymous_nominals.iter());
            self.provider.record_definition_reference(key.clone());
            let mut nominal = (*nominal).clone();
            nominal.lang_item = self.provider.language_item(
                key.module(),
                rue_air::ProviderNamespace::ModuleItem,
                key.name(),
            );
            nominal.has_destructor = matches!(
                self.provider
                    .lookup_unqualified(
                        key.module(),
                        rue_air::ProviderNamespace::Destructor,
                        key.name(),
                    )
                    .of_kind(rue_air::ProviderDefinitionKind::Destructor),
                rue_air::NameResolution::Unique(_)
            );
            self.durable_payloads
                .borrow_mut()
                .named_nominals
                .insert(key.clone(), nominal.clone());
            self.provider
                .meter()
                .nominal_materialization_reuses
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            return Some(nominal);
        }
        self.provider
            .meter()
            .nominal_materializations
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        use crate::semantic_query_nucleus::DeclarationSignatureProjection as Projection;
        use rue_air::BodyFactProvider;

        let candidate = self.candidate(key)?;
        let signature = self.provider.signature(&candidate.declaration)?;
        let is_repr_c = matches!(
            signature.signature,
            Projection::Struct {
                is_repr_c: true,
                ..
            }
        );
        let body = match signature.signature {
            Projection::Struct {
                fields,
                is_copy,
                is_linear,
                ..
            } => rue_air::DurableNominalBody::Struct {
                fields,
                is_copy,
                is_linear,
            },
            Projection::Enum {
                variants,
                is_non_exhaustive,
            } => rue_air::DurableNominalBody::Enum {
                variants,
                is_non_exhaustive,
            },
            _ => return None,
        };
        let nominal = rue_air::DurableNominal {
            name: Arc::from(key.name()),
            module_path: Arc::from(key.module().logical_path()),
            is_public: candidate.identity.is_public,
            is_builtin: false,
            lang_item: self.provider.language_item(
                key.module(),
                rue_air::ProviderNamespace::ModuleItem,
                key.name(),
            ),
            is_repr_c,
            has_destructor: matches!(
                self.provider
                    .lookup_unqualified(
                        key.module(),
                        rue_air::ProviderNamespace::Destructor,
                        key.name(),
                    )
                    .of_kind(rue_air::ProviderDefinitionKind::Destructor),
                rue_air::NameResolution::Unique(_)
            ),
            body,
        };
        self.durable_payloads
            .borrow_mut()
            .named_nominals
            .insert(key.clone(), nominal.clone());
        self.provider
            .queries
            .shared_durable_payloads
            .insert_nominal(
                self.shared_payload_generation(),
                key.clone(),
                Arc::new(nominal.clone()),
                signature.anonymous_nominals,
            );
        Some(nominal)
    }
}

impl rue_air::DurableCallableSource<crate::StableDefinitionKey, ModuleId>
    for CompilerBodyDurableSource<'_>
{
    fn function(
        &self,
        key: &crate::StableDefinitionKey,
    ) -> Option<rue_air::DurableFunction<crate::StableDefinitionKey, ModuleId>> {
        if let Some(function) = self
            .durable_payloads
            .borrow()
            .named_functions
            .get(key)
            .cloned()
        {
            self.provider
                .meter()
                .function_materialization_reuses
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            return Some(function);
        }
        if key.kind().requires_owner() {
            return None;
        }
        if let Some((function, anonymous_nominals)) = self
            .provider
            .queries
            .shared_durable_payloads
            .function(self.shared_payload_generation(), key)
        {
            let candidate = self.candidate(key)?;
            let _signature = self.signature_for_candidate(&candidate.declaration)?;
            self.dynamic_anonymous
                .borrow_mut()
                .extend(anonymous_nominals.iter());
            self.provider.record_definition_reference(key.clone());
            let function = (*function).clone();
            self.durable_payloads
                .borrow_mut()
                .named_functions
                .insert(key.clone(), function.clone());
            self.provider
                .meter()
                .function_materialization_reuses
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            return Some(function);
        }
        self.provider
            .meter()
            .function_materializations
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let candidate = self.candidate(key)?;
        let signature = self.signature_for_candidate(&candidate.declaration)?;
        let type_syntax = signature.callable_type_syntax;
        let crate::semantic_query_nucleus::DeclarationSignatureProjection::Callable {
            parameters,
            result,
            is_unchecked,
            is_extern,
            ..
        } = signature.signature
        else {
            return None;
        };
        let function = rue_air::DurableFunction {
            parameters,
            result,
            type_syntax,
            is_public: candidate.identity.is_public,
            is_unchecked,
            is_extern,
        };
        self.durable_payloads
            .borrow_mut()
            .named_functions
            .insert(key.clone(), function.clone());
        self.provider
            .queries
            .shared_durable_payloads
            .insert_function(
                self.shared_payload_generation(),
                key.clone(),
                Arc::new(function.clone()),
                signature.anonymous_nominals,
            );
        Some(function)
    }

    fn method(
        &self,
        key: &crate::StableDefinitionKey,
    ) -> Option<rue_air::DurableMethod<crate::StableDefinitionKey, ModuleId>> {
        self.provider
            .meter()
            .method_materializations
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let owner = key.owner()?;
        let signature = self.signature(key)?;
        let type_syntax = signature.callable_type_syntax;
        let crate::semantic_query_nucleus::DeclarationSignatureProjection::Callable {
            parameters,
            result,
            has_self,
            self_mode,
            is_accessor,
            accessor_result_mode,
            ..
        } = signature.signature
        else {
            return None;
        };
        let owner = crate::StableDefinitionKey::from_stable_parts(
            owner.module().clone(),
            crate::StableDefinitionNamespace::Type,
            owner.kind(),
            Arc::from(owner.name()),
            None,
        );
        Some(rue_air::DurableMethod {
            receiver: rue_air::SemanticImportType::Nominal(owner),
            parameters,
            result,
            type_syntax,
            has_self,
            self_mode,
            is_accessor,
            returns_borrow: accessor_result_mode
                == crate::durable_semantics::DurableParameterMode::Borrow,
            returns_inout: accessor_result_mode
                == crate::durable_semantics::DurableParameterMode::Inout,
        })
    }

    fn uses_deferred_body_type_placeholders(&self) -> bool {
        true
    }
}

pub(super) fn provider_definition_symbol_component(key: &crate::StableDefinitionKey) -> String {
    rue_air::stable_digest::stable_definition_component(
        key.module().logical_path(),
        key.name(),
        key.owner().map(|owner| owner.name()),
        key.kind() as u8,
    )
}

/// Render a compiler-owned anonymous identity into the same stable-content
/// domain used by `CompilerBodyDurableSource`, then take the single AIR digest.
/// Body closure aggregation calls this before any CFG/codegen consumer can
/// materialize the collected nominal set.
#[cfg(test)]
pub(super) fn compiler_anonymous_identity_digest(identity: &crate::AnonymousNominalKey) -> u128 {
    crate::semantic_identity::anonymous_nominal_digest(identity)
}

pub(super) fn register_body_closure_anonymous_digest(
    owners: &mut BTreeMap<u128, crate::AnonymousNominalKey>,
    collision: &mut Option<(u128, crate::AnonymousNominalKey, crate::AnonymousNominalKey)>,
    digest: u128,
    identity: &crate::AnonymousNominalKey,
) {
    // AIR deduplicates anonymous identities in canonical-producer form before
    // digest ownership is checked. Store and compare that same logical exact
    // key here, so an empty-specialization wrapper cannot manufacture a
    // collision with its canonical base identity. Typed failures therefore
    // also carry canonical keys.
    let identity = identity.with_canonical_producer().into_owned();
    match owners.entry(digest) {
        std::collections::btree_map::Entry::Vacant(entry) => {
            entry.insert(identity);
        }
        std::collections::btree_map::Entry::Occupied(mut entry) if entry.get() != &identity => {
            let (first, second) = if entry.get() < &identity {
                (entry.get().clone(), identity)
            } else {
                let first = identity;
                let second = entry.get().clone();
                entry.insert(first.clone());
                (first, second)
            };
            let candidate = (digest, first, second);
            if collision
                .as_ref()
                .is_none_or(|current| &candidate < current)
            {
                *collision = Some(candidate);
            }
        }
        std::collections::btree_map::Entry::Occupied(_) => {}
    }
}

impl rue_air::DurableAnonymousSource<crate::StableDefinitionKey, ModuleId>
    for CompilerBodyDurableSource<'_>
{
    fn anonymous_shape(
        &self,
        key: &crate::AnonymousNominalKey,
    ) -> Option<rue_air::DurableAnonymousShape<crate::StableDefinitionKey, ModuleId>> {
        self.anonymous_nominal(key)
            .as_ref()
            .map(|nominal| project_provider_anonymous_shape(nominal))
    }

    fn anonymous_shape_and_digest(
        &self,
        key: &crate::AnonymousNominalKey,
    ) -> Option<(
        rue_air::DurableAnonymousShape<crate::StableDefinitionKey, ModuleId>,
        u128,
    )> {
        let nominal = self.anonymous_nominal(key)?;
        Some((
            project_provider_anonymous_shape(&nominal),
            nominal.anonymous_identity_digest(),
        ))
    }

    fn anonymous_methods(
        &self,
        key: &crate::AnonymousNominalKey,
    ) -> Vec<rue_air::DurableAnonymousMethod<crate::StableDefinitionKey, ModuleId>> {
        self.anonymous_nominal(key)
            .as_ref()
            .map(|nominal| project_provider_anonymous_methods(nominal))
            .unwrap_or_default()
    }

    fn anonymous_method_return_modes(
        &self,
        key: &crate::AnonymousNominalKey,
        name: &str,
    ) -> Option<(bool, bool)> {
        // The two flags are carried verbatim by the durable signature, so this
        // reads them where they are written instead of projecting every
        // parameter and result type of every method to find them again.
        // Method order is the declaration order the projection preserves, so
        // the first match here is the first match there.
        let nominal = self.anonymous_nominal(key)?;
        let crate::durable_semantics::DurableAnonymousNominalShape::Struct { methods, .. } =
            &nominal.shape
        else {
            return None;
        };
        methods
            .iter()
            .find(|method| method.name.as_ref() == name)
            .map(|method| (method.returns_borrow, method.returns_inout))
    }

    fn anonymous_type_captures(
        &self,
        key: &crate::AnonymousNominalKey,
    ) -> Vec<(
        Arc<str>,
        rue_air::SemanticImportType<crate::StableDefinitionKey, ModuleId>,
    )> {
        self.anonymous_nominal(key)
            .map(|nominal| nominal.type_captures.iter().cloned().collect())
            .unwrap_or_default()
    }

    fn anonymous_value_captures(
        &self,
        key: &crate::AnonymousNominalKey,
    ) -> Vec<(
        Arc<str>,
        rue_air::SemanticImportConstValue<crate::StableDefinitionKey, ModuleId>,
    )> {
        self.anonymous_nominal(key)
            .map(|nominal| nominal.value_captures.iter().cloned().collect())
            .unwrap_or_default()
    }

    fn definition_symbol_component(&self, key: &crate::StableDefinitionKey) -> String {
        provider_definition_symbol_component(key)
    }

    fn module_symbol_component(&self, module: &ModuleId) -> String {
        rue_air::stable_digest::stable_module_component(module.logical_path())
    }
}

pub(super) fn project_provider_anonymous_methods(
    nominal: &crate::durable_semantics::DurableAnonymousNominal,
) -> Vec<rue_air::DurableAnonymousMethod<crate::StableDefinitionKey, ModuleId>> {
    use crate::durable_semantics::{
        DurableAnonymousMethodType as SourceType, DurableAnonymousNominalShape,
        DurableParameterMode,
    };
    let DurableAnonymousNominalShape::Struct { methods, .. } = &nominal.shape else {
        return Vec::new();
    };
    let mode = |mode| match mode {
        DurableParameterMode::Value => rue_rir::RirParamMode::Normal,
        DurableParameterMode::Borrow => rue_rir::RirParamMode::Borrow,
        DurableParameterMode::Inout => rue_rir::RirParamMode::Inout,
    };
    let concrete_type_arguments = nominal
        .type_captures
        .iter()
        .map(|(_, ty)| ty.clone())
        .collect::<Vec<_>>();
    // The owner's canonical form does not change across the projection, and
    // deriving it walks the producer spine. It used to be re-derived inside the
    // closure below, which runs once per parameter type of every method.
    let canonical_owner = nominal.identity.with_canonical_producer();
    let ty = |ty: &SourceType| match ty {
        SourceType::SelfType => rue_air::DurableAnonymousMethodType::SelfType,
        SourceType::Concrete(crate::DurableType::AnonymousNominal(identity))
            if identity.with_canonical_producer().as_ref() == canonical_owner.as_ref() =>
        {
            rue_air::DurableAnonymousMethodType::SelfType
        }
        SourceType::Concrete(ty) => rue_air::DurableAnonymousMethodType::Concrete(
            substitute_durable_generics(ty, &concrete_type_arguments),
        ),
    };
    methods
        .iter()
        .map(|method| rue_air::DurableAnonymousMethod {
            name: method.name.clone(),
            has_self: method.has_self,
            self_mode: mode(method.self_mode),
            returns_borrow: method.returns_borrow,
            returns_inout: method.returns_inout,
            parameters: method
                .parameters
                .iter()
                .map(|(parameter, parameter_mode, comptime)| {
                    (ty(parameter), mode(*parameter_mode), *comptime)
                })
                .collect(),
            result: ty(&method.result),
        })
        .collect()
}

pub(super) fn project_provider_anonymous_shape(
    nominal: &crate::durable_semantics::DurableAnonymousNominal,
) -> rue_air::DurableAnonymousShape<crate::StableDefinitionKey, ModuleId> {
    match &nominal.shape {
        crate::durable_semantics::DurableAnonymousNominalShape::Struct { fields, methods } => {
            rue_air::DurableAnonymousShape::Struct {
                fields: fields
                    .iter()
                    .map(|(name, ty)| (name.clone(), ty.clone()))
                    .collect(),
                struct_method_names: methods.iter().map(|method| method.name.clone()).collect(),
            }
        }
        crate::durable_semantics::DurableAnonymousNominalShape::Enum { variants } => {
            rue_air::DurableAnonymousShape::Enum {
                variants: variants
                    .iter()
                    .map(|(name, payload)| (name.clone(), payload.to_vec()))
                    .collect(),
            }
        }
    }
}

pub(super) fn project_provider_produced_anonymous_nominals(
    values: &[rue_air::SemanticProducedAnonymousNominal],
    definitions: &AHashMap<rue_air::SemanticDefinitionToken, crate::StableDefinitionKey>,
    modules: &AHashMap<rue_air::SemanticModuleToken, ModuleId>,
) -> Result<
    crate::body_query::BodyProducedAnonymousNominals,
    rue_air::SemanticStableResolutionFailure,
> {
    use crate::durable_semantics::{
        DurableAnonymousMethodSignature as Method, DurableAnonymousMethodType as MethodType,
        DurableAnonymousNominal as Nominal, DurableAnonymousNominalShape as Shape,
        DurableParameterMode as Mode,
    };
    let definition = |token: &rue_air::SemanticDefinitionToken| {
        definitions
            .get(token)
            .cloned()
            .ok_or(rue_air::SemanticStableResolutionFailure::Missing)
    };
    let module = |token: &rue_air::SemanticModuleToken| {
        modules
            .get(token)
            .cloned()
            .ok_or(rue_air::SemanticStableResolutionFailure::Missing)
    };
    let map_type = |ty: &rue_air::TypeInstanceKey<
        rue_air::SemanticDefinitionToken,
        rue_air::SemanticModuleToken,
    >| {
        let ty = ty.try_map_identities(&definition, &module)?;
        durable_type_from_instance_key(&ty)
            .ok_or(rue_air::SemanticStableResolutionFailure::WrongKind)
    };
    let map_value = |value: &rue_air::CanonicalArgumentValue<
        rue_air::SemanticDefinitionToken,
        rue_air::SemanticModuleToken,
    >| {
        let value = value.try_map_identities(&definition, &module)?;
        durable_value_from_argument(&value)
            .ok_or(rue_air::SemanticStableResolutionFailure::WrongKind)
    };
    let mode = |mode| match mode {
        rue_air::SemanticParameterMode::Value => Mode::Value,
        rue_air::SemanticParameterMode::Borrow => Mode::Borrow,
        rue_air::SemanticParameterMode::Inout => Mode::Inout,
    };
    let projected = values
        .iter()
        .map(|value| {
            let identity = value
                .identity
                .try_map_identities(&definition, &module)?
                .with_canonical_producer()
                .into_owned();
            let method_type = |ty: &rue_air::SemanticProducedAnonymousMethodType| {
                Ok(match ty {
                    rue_air::SemanticProducedAnonymousMethodType::SelfType => MethodType::SelfType,
                    rue_air::SemanticProducedAnonymousMethodType::Concrete(ty) => {
                        MethodType::Concrete(map_type(ty)?)
                    }
                })
            };
            let shape = match &value.shape {
                rue_air::SemanticProducedAnonymousNominalShape::Struct { fields, methods } => {
                    Shape::Struct {
                        fields: fields
                            .iter()
                            .map(|(name, ty)| Ok((name.clone(), map_type(ty)?)))
                            .collect::<Result<Vec<_>, _>>()?
                            .into(),
                        methods: methods
                            .iter()
                            .map(|method| {
                                Ok(Method {
                                    name: method.name.clone(),
                                    has_self: method.has_self,
                                    self_mode: mode(method.self_mode),
                                    returns_borrow: method.returns_borrow,
                                    returns_inout: method.returns_inout,
                                    parameters: method
                                        .parameters
                                        .iter()
                                        .map(|(ty, parameter_mode, comptime)| {
                                            Ok((method_type(ty)?, mode(*parameter_mode), *comptime))
                                        })
                                        .collect::<Result<Vec<_>, _>>()?
                                        .into(),
                                    result: method_type(&method.result)?,
                                    has_body: true,
                                })
                            })
                            .collect::<Result<Vec<_>, _>>()?
                            .into(),
                    }
                }
                rue_air::SemanticProducedAnonymousNominalShape::Enum { variants } => Shape::Enum {
                    variants: variants
                        .iter()
                        .map(|(name, payload)| {
                            Ok((
                                name.clone(),
                                payload
                                    .iter()
                                    .map(&map_type)
                                    .collect::<Result<Vec<_>, _>>()?
                                    .into(),
                            ))
                        })
                        .collect::<Result<Vec<_>, _>>()?
                        .into(),
                },
            };
            Ok(Nominal::new(
                identity,
                shape,
                value
                    .type_captures
                    .iter()
                    .map(|(name, ty)| Ok((name.clone(), map_type(ty)?)))
                    .collect::<Result<Vec<_>, _>>()?
                    .into(),
                value
                    .value_captures
                    .iter()
                    .map(|(name, value)| Ok((name.clone(), map_value(value)?)))
                    .collect::<Result<Vec<_>, _>>()?
                    .into(),
            ))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mut by_identity = BTreeMap::new();
    for nominal in &projected {
        crate::durable_semantics::merge_complete_anonymous_nominal(&mut by_identity, nominal)
            .map_err(|_| rue_air::SemanticStableResolutionFailure::Ambiguous)?;
    }
    Ok(crate::body_query::BodyProducedAnonymousNominals(
        by_identity.into_values().collect::<Vec<_>>().into(),
    ))
}

#[allow(dead_code)]
impl<'a> CompilerBodyFactProvider<'a> {
    pub(crate) fn new(queries: CompilerBodyProviderQueries<'a>) -> Self {
        Self {
            queries,
            nucleus_cache: std::cell::RefCell::new(std::array::from_fn(|_| None)),
            lookup_name_cache: std::cell::RefCell::new(std::array::from_fn(|_| None)),
            #[cfg(test)]
            nucleus_cache_hits: std::cell::Cell::new(0),
            #[cfg(test)]
            lookup_name_cache_hits: std::cell::Cell::new(0),
            #[cfg(test)]
            frontier_rendezvous_arrived: std::cell::Cell::new(false),
        }
    }

    /// Take the observed lookup-pin set for promotion into the session lease.
    pub(super) fn take_observed_root(&self) -> ObservedLookupRoot {
        self.queries.observed.replace(ObservedLookupRoot::new())
    }

    pub(super) fn observe_abort(&self, abort: QueryAbort) {
        let status = match abort {
            QueryAbort::Canceled => {
                CompilerBodyProviderStatus::Incomplete(CompilerBodyProviderIncomplete::Canceled)
            }
            QueryAbort::MissingInput(input) => CompilerBodyProviderStatus::Incomplete(
                CompilerBodyProviderIncomplete::MissingInput(input),
            ),
            fatal => CompilerBodyProviderStatus::Fatal(fatal),
        };
        let mut current = self.queries.status.borrow_mut();
        if provider_status_should_replace(&current, &status) {
            *current = status;
        }
    }

    pub(crate) fn finish_status(&self) -> Result<(), CompilerBodyProviderStatus> {
        self.queries.finish_status()
    }

    pub(super) fn meter(&self) -> &ProviderObservationCounters {
        &self.queries.meter
    }

    pub(super) fn source_locator(
        &self,
        module: &ModuleId,
    ) -> Option<rue_air::DurableBodySourceLocator> {
        let terminal = match self.queries.context.query_registered(
            &self.queries.module_source_bases,
            ModuleQueryKey(module.clone()),
        ) {
            Ok(terminal) => terminal,
            Err(abort) => {
                self.observe_abort(abort);
                return None;
            }
        };
        let rue_query::QueryOutcome::Success(Some(locator)) = terminal.outcome() else {
            return None;
        };
        Some(locator.clone())
    }

    pub(super) fn producer_relative_span(
        &self,
        candidate: &crate::declaration_candidate::DeclarationCandidateKey,
        start: u32,
        end: u32,
    ) -> Option<rue_span::Span> {
        let terminal = match self.queries.context.query_registered(
            &self.queries.parse_modules,
            ModuleQueryKey(candidate.module.clone()),
        ) {
            Ok(terminal) => terminal,
            Err(abort) => {
                self.observe_abort(abort);
                return None;
            }
        };
        let rue_query::QueryOutcome::Success(ParseModuleValue {
            result: Ok(parsed), ..
        }) = terminal.outcome()
        else {
            return None;
        };
        let producer = parsed
            .definitions()
            .declaration_locator(candidate)?
            .declaration_span;
        let absolute_start = producer.start.checked_add(start)?;
        let absolute_end = producer.start.checked_add(end)?;
        (absolute_start <= absolute_end && absolute_end <= producer.end)
            .then(|| rue_span::Span::with_file(parsed.file_id(), absolute_start, absolute_end))
    }

    pub(super) fn import_specifiers(&self, module: &ModuleId) -> Option<Vec<Arc<str>>> {
        let terminal = match self
            .queries
            .context
            .query_registered(&self.queries.parse_modules, ModuleQueryKey(module.clone()))
        {
            Ok(terminal) => terminal,
            Err(abort) => {
                self.observe_abort(abort);
                return None;
            }
        };
        let rue_query::QueryOutcome::Success(ParseModuleValue {
            result: Ok(parsed), ..
        }) = terminal.outcome()
        else {
            return None;
        };
        Some(
            parsed
                .imports()
                .iter()
                .map(|import| Arc::from(import.specifier()))
                .collect(),
        )
    }

    pub(super) fn has_module_source(&self, module: &ModuleId) -> bool {
        self.queries
            .context
            .optional_input(module_source_input(module))
            .is_some()
    }

    pub(super) fn record_definition_reference(&self, key: crate::StableDefinitionKey) {
        use crate::body_query::BodyReference;
        let reference = match key.kind() {
            crate::StableDefinitionKind::Function
            | crate::StableDefinitionKind::Method
            | crate::StableDefinitionKind::AssociatedFunction
            | crate::StableDefinitionKind::Destructor => {
                BodyReference::Callable(crate::FunctionInstanceKey::Definition(key))
            }
            crate::StableDefinitionKind::Struct | crate::StableDefinitionKind::Enum => {
                BodyReference::Type(crate::TypeInstanceKey::Nominal(
                    crate::NominalInstanceKey::Named(key),
                ))
            }
            _ => BodyReference::Definition(key),
        };
        self.queries
            .positive_references
            .borrow_mut()
            .insert(reference);
    }

    pub(super) fn body_query_key(
        &self,
        instance: &crate::FunctionInstanceKey,
    ) -> crate::body_query::BodyQueryKey {
        crate::body_query::BodyQueryKey::new(instance.clone(), self.queries.configuration.clone())
    }

    /// Observe the exact `LookupName` terminal for a consulted key, recording
    /// its edge and classifying the candidate set.
    pub(super) fn name_resolution(
        &self,
        module: &ModuleId,
        namespace: rue_air::ProviderNamespace,
        name: &str,
    ) -> rue_air::NameResolution {
        let namespace = provider_namespace_to_definition(namespace);
        let cache_slot = LOOKUP_NAME_CACHE_HASHER.hash_one((module, namespace, name)) as usize
            % LOOKUP_NAME_CACHE_SLOTS;
        if let Some(entry) = &self.lookup_name_cache.borrow()[cache_slot]
            && entry.key.module == *module
            && entry.key.namespace == namespace
            && entry.key.name.as_ref() == name
        {
            #[cfg(test)]
            self.lookup_name_cache_hits
                .set(self.lookup_name_cache_hits.get() + 1);
            return entry.resolution.clone();
        }
        let key = LookupNameKey {
            module: module.clone(),
            namespace,
            name: Arc::from(name),
        };
        match self
            .queries
            .context
            .query_registered(&self.queries.lookup_names, key.clone())
        {
            Ok(terminal) => {
                // Pin the observed lookup-name terminal while the request lease
                // still protects it, so the promoted set transfers it with no
                // birth-eviction window.
                self.queries.observed.borrow_mut().record(
                    &self.queries.lookup_names,
                    &terminal,
                    LookupObservationKey::Name(key.clone()),
                );
                let resolution = match terminal.outcome() {
                    rue_query::QueryOutcome::Success(value) => name_resolution_from_value(value),
                    _ => rue_air::NameResolution::IndexUnavailable,
                };
                self.lookup_name_cache.borrow_mut()[cache_slot] = Some(LookupNameCacheEntry {
                    key,
                    resolution: resolution.clone(),
                });
                resolution
            }
            Err(abort) => {
                self.observe_abort(abort);
                rue_air::NameResolution::IndexUnavailable
            }
        }
    }

    /// Observe one semantic-nucleus terminal, recording its edge. Deterministic
    /// failures publish as `Success(Failure)`; a `QueryAbort` returns the
    /// trait's absence-shaped value only provisionally and records a typed
    /// provider status that the request boundary must check before publication.
    pub(super) fn nucleus_result(
        &self,
        key: crate::semantic_query_nucleus::SemanticNucleusKey,
    ) -> Result<Option<crate::semantic_query_nucleus::SemanticNucleusValue>, QueryAbort> {
        let terminal = self.nucleus_terminal_result(key)?;
        Ok(match terminal.outcome() {
            rue_query::QueryOutcome::Success(value) => Some(value.clone()),
            _ => None,
        })
    }

    pub(super) fn nucleus_terminal_result(
        &self,
        key: crate::semantic_query_nucleus::SemanticNucleusKey,
    ) -> Result<
        Arc<rue_query::QueryTerminal<crate::semantic_query_nucleus::SemanticNucleusValue>>,
        QueryAbort,
    > {
        self.queries
            .context
            .query_registered(&self.queries.semantic_nucleus, key)
    }
}

/// The single query-side authority for non-computing foreign comptime
/// admission. Every caller supplies the exact query context and registered
/// families; this authority never owns a database or invokes a query body.
#[allow(dead_code)] // activated by the canonical durable AIR host
pub(super) struct DurableComptimeForeignQueryAuthority<'a> {
    pub(super) context: &'a QueryContext,
    pub(super) semantic_nucleus: &'a SemanticNucleusFamily,
    pub(super) declaration_body_plan_artifacts:
        &'a QueryFamily<DeclarationBodyPlanQueryKey, DeclarationBodyPlanArtifactsValue>,
    pub(super) configuration: &'a crate::semantic_query_nucleus::SemanticQueryConfiguration,
}

/// Keep the non-computing probe decision separate from the expensive cold-miss
/// admission.  In particular, NotReady is a terminal for this observer and
/// must not invoke the body-plan query closure.
pub(super) fn foreign_comptime_miss_or_not_ready<T>(
    probe: rue_query::ReadyQueryProbe<T>,
    on_miss: impl FnOnce() -> Result<crate::body_query::ForeignComptimeCallLookup, QueryAbort>,
) -> Result<crate::body_query::ForeignComptimeCallLookup, QueryAbort> {
    match probe {
        rue_query::ReadyQueryProbe::NotReady => {
            Ok(crate::body_query::ForeignComptimeCallLookup::NotReady)
        }
        rue_query::ReadyQueryProbe::Miss => on_miss(),
        rue_query::ReadyQueryProbe::Ready(_) => {
            unreachable!("ready probes are converted by the caller")
        }
    }
}

#[allow(dead_code)] // activated by the canonical durable AIR host
impl crate::durable_comptime::DurableComptimeForeignCallAuthority
    for DurableComptimeForeignQueryAuthority<'_>
{
    fn probe_comptime_call(
        &self,
        producer: &crate::StableDefinitionKey,
        type_arguments: &[(Arc<str>, crate::durable_semantics::DurableType)],
        value_arguments: &[(Arc<str>, crate::durable_semantics::DurableConstValue)],
    ) -> Result<crate::body_query::ForeignComptimeCallLookup, QueryAbort> {
        let Some(declaration) = declaration_candidate_for_stable_key(producer) else {
            return Ok(
                crate::body_query::ForeignComptimeCallLookup::AdmissionFailure(
                    crate::body_query::ComptimeProgramProjectionFailure::InvalidProducer(
                        producer.clone(),
                    ),
                ),
            );
        };
        let key = crate::semantic_query_nucleus::ComptimeCallQueryKey {
            declaration: crate::semantic_query_nucleus::DeclarationSemanticQueryKey {
                declaration: declaration.clone(),
                configuration: self.configuration.clone(),
            },
            type_arguments: type_arguments.to_vec().into(),
            value_arguments: value_arguments.to_vec().into(),
        };
        let foreign_plan = crate::body_query::DurableComptimeProgramPlan {
            key: crate::body_query::DurableComptimeProgramKey {
                declaration: producer.clone(),
                configuration: self.configuration.clone(),
            },
            candidate: declaration,
        };
        match self.context.join_registered_noncomputing(
            self.semantic_nucleus,
            crate::semantic_query_nucleus::SemanticNucleusKey::ComptimeCall(key.clone()),
        )? {
            rue_query::ReadyQueryProbe::Ready(terminal) => match terminal.outcome() {
                rue_query::QueryOutcome::Success(
                    crate::semantic_query_nucleus::SemanticNucleusValue::ComptimeCall(value),
                ) => Ok(crate::body_query::ForeignComptimeCallLookup::Ready(
                    value.clone(),
                )),
                rue_query::QueryOutcome::Success(
                    crate::semantic_query_nucleus::SemanticNucleusValue::Failure(failure),
                ) => Ok(crate::body_query::ForeignComptimeCallLookup::ReadyFailure(
                    failure.clone(),
                )),
                rue_query::QueryOutcome::Failure(failure) => Ok(
                    crate::body_query::ForeignComptimeCallLookup::ReadyQueryFailure(
                        failure.clone(),
                    ),
                ),
                _ => Ok(crate::body_query::ForeignComptimeCallLookup::UnexpectedReadyProjection),
            },
            probe @ (rue_query::ReadyQueryProbe::NotReady | rue_query::ReadyQueryProbe::Miss) => {
                foreign_comptime_miss_or_not_ready(probe, || {
                    let artifacts = self.context.query_registered(
                        self.declaration_body_plan_artifacts,
                        DeclarationBodyPlanQueryKey(foreign_plan.candidate.clone()),
                    )?;
                    let artifacts = match artifacts.outcome() {
                        rue_query::QueryOutcome::Success(
                            DeclarationBodyPlanArtifactsValue::Available(artifacts),
                        ) => artifacts,
                        rue_query::QueryOutcome::Success(
                            DeclarationBodyPlanArtifactsValue::Failure(failure),
                        ) => {
                            return Ok(
                                crate::body_query::ForeignComptimeCallLookup::AdmissionFailure(
                                    crate::body_query::ComptimeProgramProjectionFailure::Artifact(
                                        failure.clone(),
                                    ),
                                ),
                            );
                        }
                        rue_query::QueryOutcome::Failure(failure) => {
                            return Ok(crate::body_query::ForeignComptimeCallLookup::AdmissionFailure(
                            crate::body_query::ComptimeProgramProjectionFailure::ArtifactQueryFailure(
                                failure.clone(),
                            ),
                        ));
                        }
                    };
                    let seed = crate::body_query::ForeignComptimeCallSeed {
                        type_arguments: type_arguments.to_vec().into(),
                        value_arguments: value_arguments.to_vec().into(),
                    };
                    match crate::body_query::OwnedForeignComptimeProgram::from_body_plan(
                        foreign_plan,
                        artifacts,
                        seed,
                        || self.context.check_canceled(),
                    ) {
                        Ok(program) => Ok(crate::body_query::ForeignComptimeCallLookup::Admitted(
                            program,
                        )),
                        Err(
                            crate::body_query::ComptimeProgramProjectionFailure::Materialization(
                                crate::canonical_lower::BodyPlanMaterializationFailure::Query(
                                    abort,
                                ),
                            ),
                        ) => Err(abort),
                        Err(error) => Ok(
                            crate::body_query::ForeignComptimeCallLookup::AdmissionFailure(error),
                        ),
                    }
                })
            }
        }
    }
}

impl CompilerBodyFactProvider<'_> {
    #[allow(dead_code)] // activated by the canonical durable AIR host
    pub(crate) fn probe_comptime_call(
        &self,
        producer: &crate::StableDefinitionKey,
        type_arguments: &[(Arc<str>, crate::durable_semantics::DurableType)],
        value_arguments: &[(Arc<str>, crate::durable_semantics::DurableConstValue)],
    ) -> Result<crate::body_query::ForeignComptimeCallLookup, QueryAbort> {
        let mut authority = DurableComptimeForeignQueryAuthority {
            context: self.queries.context,
            semantic_nucleus: &self.queries.semantic_nucleus,
            declaration_body_plan_artifacts: &self.queries.declaration_body_plan_artifacts,
            configuration: &self.queries.configuration,
        };
        crate::durable_comptime::DurableComptimeServices::new(&mut authority).probe_comptime_call(
            producer,
            type_arguments,
            value_arguments,
        )
    }

    pub(super) fn nucleus(
        &self,
        key: crate::semantic_query_nucleus::SemanticNucleusKey,
    ) -> Option<crate::semantic_query_nucleus::SemanticNucleusValue> {
        let cache_slot = SEMANTIC_NUCLEUS_CACHE_HASHER.hash_one(&key) as usize
            % self.nucleus_cache.borrow().len();
        if let Some(entry) = &self.nucleus_cache.borrow()[cache_slot]
            && entry.key == key
        {
            #[cfg(test)]
            self.nucleus_cache_hits
                .set(self.nucleus_cache_hits.get() + 1);
            return match entry.terminal.outcome() {
                rue_query::QueryOutcome::Success(value) => Some(value.clone()),
                _ => None,
            };
        }
        match self.nucleus_terminal_result(key.clone()) {
            Ok(terminal) => {
                let value = match terminal.outcome() {
                    rue_query::QueryOutcome::Success(value) => Some(value.clone()),
                    _ => None,
                };
                self.nucleus_cache.borrow_mut()[cache_slot] =
                    Some(SemanticNucleusCacheEntry { key, terminal });
                value
            }
            Err(abort) => {
                self.observe_abort(abort);
                None
            }
        }
    }

    pub(super) fn import_target(&self, module: &ModuleId, specifier: &str) -> Option<ModuleId> {
        let key = LookupImportKey {
            module: module.clone(),
            specifier: Arc::from(specifier),
        };
        let terminal = match self
            .queries
            .context
            .query_registered(&self.queries.lookup_imports, key.clone())
        {
            Ok(terminal) => terminal,
            Err(abort) => {
                self.observe_abort(abort);
                return None;
            }
        };
        self.queries.observed.borrow_mut().record(
            &self.queries.lookup_imports,
            &terminal,
            LookupObservationKey::Import(key),
        );
        let rue_query::QueryOutcome::Success(LookupImportValue(Ok(binding))) = terminal.outcome()
        else {
            return None;
        };
        binding.target.clone()
    }

    pub(super) fn declaration_query_key(
        &self,
        decl: &crate::declaration_candidate::DeclarationCandidateKey,
    ) -> crate::semantic_query_nucleus::DeclarationSemanticQueryKey {
        crate::semantic_query_nucleus::DeclarationSemanticQueryKey {
            declaration: decl.clone(),
            configuration: self.queries.configuration.clone(),
        }
    }

    /// The candidate key for one member of a receiver in a given syntactic
    /// category (method or associated function), keyed on the receiver-type
    /// identity plus the member name.
    pub(super) fn member_candidate_key(
        &self,
        receiver: &ReceiverTypeIdentity,
        category: crate::declaration_candidate::DeclarationCandidateCategory,
        name: &str,
    ) -> crate::declaration_candidate::DeclarationCandidateKey {
        crate::declaration_candidate::DeclarationCandidateKey {
            module: receiver.module.clone(),
            category,
            name: Arc::from(name),
            owner: Some(crate::declaration_candidate::DeclarationCandidateOwner {
                category: receiver.type_category,
                name: receiver.type_name.clone(),
            }),
            duplicate_discriminator: 0,
        }
    }

    /// Collect every member of `receiver` named `name`, spanning BOTH the method
    /// and associated-function categories (they share the compiler's method
    /// table). For each present member this observes its semantic-nucleus
    /// identity terminal (presence + visibility) and its signature terminal, and
    /// sources `has_self` honestly from the signature's callable projection —
    /// never inferred from the syntactic category. Absent members contribute no
    /// candidate; the returned set is the §4 candidate set, not a winner.
    pub(super) fn member_candidates(
        &self,
        receiver: &ReceiverTypeIdentity,
        name: &str,
    ) -> Vec<MemberObservation> {
        use crate::declaration_candidate::DeclarationCandidateCategory as Cat;
        let mut found = Vec::new();
        for (category, kind) in [
            (Cat::Method, rue_air::MemberKind::Method),
            (
                Cat::AssociatedFunction,
                rue_air::MemberKind::AssociatedFunction,
            ),
        ] {
            let key = self.member_candidate_key(receiver, category, name);
            let Some(crate::semantic_query_nucleus::SemanticNucleusValue::Identity(identity)) =
                self.nucleus(crate::semantic_query_nucleus::SemanticNucleusKey::Identity(
                    crate::semantic_query_nucleus::DeclarationSemanticQueryKey {
                        declaration: key.clone(),
                        configuration: self.queries.configuration.clone(),
                    },
                ))
            else {
                continue;
            };
            // `has_self` is authoritative from the signature, not the category.
            let has_self_receiver = match self.nucleus(
                crate::semantic_query_nucleus::SemanticNucleusKey::Signature(
                    crate::semantic_query_nucleus::DeclarationSemanticQueryKey {
                        declaration: key.clone(),
                        configuration: self.queries.configuration.clone(),
                    },
                ),
            ) {
                Some(crate::semantic_query_nucleus::SemanticNucleusValue::Signature(signature)) => {
                    matches!(
                        signature.signature,
                        crate::semantic_query_nucleus::DeclarationSignatureProjection::Callable {
                            has_self: true,
                            ..
                        }
                    )
                }
                _ => matches!(category, Cat::Method),
            };
            found.push(MemberObservation {
                declaration: key,
                kind,
                has_self_receiver,
                is_public: identity.is_public,
            });
        }
        found
    }
}

/// One observed receiver member: its declaration handle, syntactic kind,
/// `self`-receiver classification (from the signature), and visibility.
pub(super) struct MemberObservation {
    pub(super) declaration: crate::declaration_candidate::DeclarationCandidateKey,
    pub(super) kind: rue_air::MemberKind,
    pub(super) has_self_receiver: bool,
    pub(super) is_public: bool,
}

impl rue_air::BodyFactProvider for CompilerBodyFactProvider<'_> {
    type ModuleRef = ModuleId;
    type DeclarationRef = crate::declaration_candidate::DeclarationCandidateKey;
    type BodyInstanceRef = crate::FunctionInstanceKey;
    type ReceiverType = ReceiverTypeIdentity;

    type DeclarationIdentity = crate::semantic_query_nucleus::DeclarationIdentityProjection;
    type Signature = crate::semantic_query_nucleus::ResolvedDeclarationSignature;
    type ConstComptime = crate::semantic_query_nucleus::ConstResolutionProjection;
    type ComptimeType = crate::durable_semantics::DurableType;
    type ComptimeValue = crate::durable_semantics::DurableConstValue;
    type ComptimeCall = crate::semantic_query_nucleus::ComptimeCallResultProjection;
    type AnonymousFacts = Arc<[crate::durable_semantics::DurableAnonymousNominal]>;
    type ProducerBodyFacts = crate::body_query::ProducedAnonymous;
    type ToolchainFacts = crate::BodyToolchainDemand;

    fn is_canceled(&self) -> bool {
        #[cfg(test)]
        {
            let remaining = TEST_CGEN_CANCEL_AFTER.load(std::sync::atomic::Ordering::SeqCst);
            if remaining != usize::MAX
                && (!TEST_CGEN_FRONTIER_ONLY.load(std::sync::atomic::Ordering::SeqCst)
                    || TEST_CGEN_FRONTIER_STARTED.load(std::sync::atomic::Ordering::SeqCst))
            {
                let visit = TEST_CGEN_VISITS.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
                TEST_CGEN_PHASE.store(1, std::sync::atomic::Ordering::SeqCst);
                if visit >= remaining {
                    self.observe_abort(QueryAbort::Canceled);
                    return true;
                }
            }
        }
        if self.queries.context.check_canceled().is_err() {
            // Preserve query abort semantics even when the cancellation is
            // observed during a local AIR frontier walk rather than while a
            // provider lookup is crossing a child query.
            self.observe_abort(QueryAbort::Canceled);
            true
        } else {
            false
        }
    }

    #[cfg(test)]
    fn staged_frontier_started(&self) {
        TEST_CGEN_FRONTIER_STARTED.store(true, std::sync::atomic::Ordering::SeqCst);
        TEST_CGEN_PHASE.store(1, std::sync::atomic::Ordering::SeqCst);
        if self.frontier_rendezvous_arrived.replace(true) {
            return;
        }
        let rendezvous = TEST_FRONTIER_RENDEZVOUS
            .get_or_init(|| Mutex::new(None))
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        if let Some(rendezvous) = rendezvous {
            rendezvous.arrive_and_wait();
        }
    }

    #[cfg(test)]
    fn staged_sibling_attempt(&self) {
        TEST_CGEN_ATTEMPTED_SIBLINGS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let cancel_after = TEST_CGEN_CANCEL_AFTER.load(std::sync::atomic::Ordering::SeqCst);
        if cancel_after != usize::MAX
            && TEST_CGEN_VISITS.load(std::sync::atomic::Ordering::SeqCst) >= cancel_after
        {
            TEST_CGEN_POST_CANCEL_ATTEMPTS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
    }

    fn lookup_unqualified(
        &self,
        module: &ModuleId,
        namespace: rue_air::ProviderNamespace,
        name: &str,
    ) -> rue_air::NameResolution {
        self.meter()
            .name_lookups
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.name_resolution(module, namespace, name)
    }

    fn lookup_qualified(
        &self,
        module: &ModuleId,
        namespace: rue_air::ProviderNamespace,
        name: &str,
    ) -> rue_air::NameResolution {
        self.meter()
            .name_lookups
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.name_resolution(module, namespace, name)
    }

    fn method_candidates(
        &self,
        receiver: &ReceiverTypeIdentity,
        name: &str,
    ) -> Vec<rue_air::MemberCandidate<crate::declaration_candidate::DeclarationCandidateKey>> {
        self.meter()
            .method_candidates
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.member_candidates(receiver, name)
            .into_iter()
            .map(|observed| rue_air::MemberCandidate {
                declaration: observed.declaration,
                name: Arc::from(name),
                has_self_receiver: observed.has_self_receiver,
                kind: observed.kind,
                is_public: observed.is_public,
            })
            .collect()
    }

    fn operator_candidates(
        &self,
        receiver: &ReceiverTypeIdentity,
        operator: rue_air::OperatorName,
    ) -> Vec<rue_air::OperatorMemberCandidate<crate::declaration_candidate::DeclarationCandidateKey>>
    {
        self.meter()
            .operator_candidates
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.member_candidates(receiver, operator.method_name())
            .into_iter()
            .map(|observed| rue_air::OperatorMemberCandidate {
                declaration: observed.declaration,
                operator,
                has_self_receiver: observed.has_self_receiver,
                is_public: observed.is_public,
            })
            .collect()
    }

    fn declaration_identity(
        &self,
        decl: &crate::declaration_candidate::DeclarationCandidateKey,
    ) -> Option<crate::semantic_query_nucleus::DeclarationIdentityProjection> {
        self.meter()
            .identity_facts
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let query = crate::semantic_query_nucleus::SemanticNucleusKey::Identity(
            self.declaration_query_key(decl),
        );
        match self.nucleus(query) {
            Some(crate::semantic_query_nucleus::SemanticNucleusValue::Identity(identity)) => {
                Some(identity)
            }
            _ => None,
        }
    }

    fn signature(
        &self,
        decl: &crate::declaration_candidate::DeclarationCandidateKey,
    ) -> Option<crate::semantic_query_nucleus::ResolvedDeclarationSignature> {
        self.meter()
            .signature_facts
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let query = crate::semantic_query_nucleus::SemanticNucleusKey::Signature(
            self.declaration_query_key(decl),
        );
        match self.nucleus(query) {
            Some(crate::semantic_query_nucleus::SemanticNucleusValue::Signature(signature)) => {
                self.record_definition_reference(signature.definition.clone());
                Some(signature)
            }
            _ => None,
        }
    }

    fn const_comptime(
        &self,
        decl: &crate::declaration_candidate::DeclarationCandidateKey,
    ) -> Option<crate::semantic_query_nucleus::ConstResolutionProjection> {
        self.meter()
            .const_facts
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let query = crate::semantic_query_nucleus::SemanticNucleusKey::ConstResolution(
            self.declaration_query_key(decl),
        );
        match self.nucleus(query) {
            Some(crate::semantic_query_nucleus::SemanticNucleusValue::ConstResolution(value)) => {
                match &value {
                    crate::semantic_query_nucleus::ConstResolutionProjection::Value {
                        key, ..
                    }
                    | crate::semantic_query_nucleus::ConstResolutionProjection::ModuleBinding {
                        key,
                        ..
                    } => self.record_definition_reference(key.clone()),
                }
                Some(value)
            }
            _ => None,
        }
    }

    fn reduce_comptime_call(
        &self,
        decl: &crate::declaration_candidate::DeclarationCandidateKey,
        type_arguments: &[(Arc<str>, crate::durable_semantics::DurableType)],
        value_arguments: &[(Arc<str>, crate::durable_semantics::DurableConstValue)],
    ) -> Option<crate::semantic_query_nucleus::ComptimeCallResultProjection> {
        // A comptime-call reduction is a declaration-level fact keyed on the head
        // declaration plus its bound arguments; it observes the same
        // semantic-nucleus terminal family the signature/const ops do, so it is
        // metered as a declaration fact.
        self.meter()
            .const_facts
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let query = crate::semantic_query_nucleus::SemanticNucleusKey::ComptimeCall(
            crate::semantic_query_nucleus::ComptimeCallQueryKey {
                declaration: self.declaration_query_key(decl),
                type_arguments: type_arguments.to_vec().into(),
                value_arguments: value_arguments.to_vec().into(),
            },
        );
        match self.nucleus(query) {
            Some(crate::semantic_query_nucleus::SemanticNucleusValue::ComptimeCall(value)) => {
                Some(value.result)
            }
            Some(crate::semantic_query_nucleus::SemanticNucleusValue::Failure(
                crate::semantic_query_nucleus::SemanticNucleusFailure::Diagnostic(_),
            )) => None,
            _ => None,
        }
    }

    fn nominal_well_formedness(
        &self,
        decl: &crate::declaration_candidate::DeclarationCandidateKey,
    ) -> Option<rue_air::NominalWellFormedness> {
        self.meter()
            .type_facts
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let query = crate::semantic_query_nucleus::SemanticNucleusKey::NominalWellFormedness(
            self.declaration_query_key(decl),
        );
        match self.nucleus(query) {
            Some(crate::semantic_query_nucleus::SemanticNucleusValue::NominalWellFormedness) => {
                Some(rue_air::NominalWellFormedness::WellFormed)
            }
            Some(crate::semantic_query_nucleus::SemanticNucleusValue::Failure(_)) => {
                Some(rue_air::NominalWellFormedness::IllFormed)
            }
            _ => None,
        }
    }

    fn anonymous_facts(
        &self,
        decl: &crate::declaration_candidate::DeclarationCandidateKey,
    ) -> Option<Arc<[crate::durable_semantics::DurableAnonymousNominal]>> {
        self.meter()
            .anonymous_facts
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let query = crate::semantic_query_nucleus::SemanticNucleusKey::Signature(
            self.declaration_query_key(decl),
        );
        match self.nucleus(query) {
            Some(crate::semantic_query_nucleus::SemanticNucleusValue::Signature(signature)) => {
                Some(signature.anonymous_nominals.clone())
            }
            _ => None,
        }
    }

    fn language_item(
        &self,
        module: &ModuleId,
        namespace: rue_air::ProviderNamespace,
        name: &str,
    ) -> Option<rue_air::LangItem> {
        self.meter()
            .name_lookups
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        match self.name_resolution(module, namespace, name) {
            rue_air::NameResolution::Unique(candidate) => candidate.language_item,
            rue_air::NameResolution::Ambiguous(candidates) => candidates
                .iter()
                .find_map(|candidate| candidate.language_item),
            rue_air::NameResolution::Absent | rue_air::NameResolution::IndexUnavailable => None,
        }
    }

    fn drop_copy_metadata(
        &self,
        receiver: &ReceiverTypeIdentity,
    ) -> Option<rue_air::DropCopyMetadata> {
        // A destructor is a first-class name lookup in the Destructor namespace.
        self.meter()
            .name_lookups
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let destructor = self.name_resolution(
            &receiver.module,
            rue_air::ProviderNamespace::Destructor,
            &receiver.type_name,
        );
        let has_destructor = !destructor.candidates().is_empty();
        // `@copy` is carried on the receiver type's own struct signature.
        self.meter()
            .signature_facts
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let type_key = crate::declaration_candidate::DeclarationCandidateKey {
            module: receiver.module.clone(),
            category: receiver.type_category,
            name: receiver.type_name.clone(),
            owner: None,
            duplicate_discriminator: 0,
        };
        let query = crate::semantic_query_nucleus::SemanticNucleusKey::Signature(
            self.declaration_query_key(&type_key),
        );
        let is_copy = match self.nucleus(query) {
            Some(crate::semantic_query_nucleus::SemanticNucleusValue::Signature(signature)) => {
                matches!(
                    signature.signature,
                    crate::semantic_query_nucleus::DeclarationSignatureProjection::Struct {
                        is_copy: true,
                        ..
                    }
                )
            }
            _ => false,
        };
        Some(rue_air::DropCopyMetadata {
            has_destructor,
            is_copy,
        })
    }

    fn resolve_import(&self, module: &ModuleId, specifier: &str) -> rue_air::ImportResolution {
        self.meter()
            .import_lookups
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let key = LookupImportKey {
            module: module.clone(),
            specifier: Arc::from(specifier),
        };
        match self
            .queries
            .context
            .query_registered(&self.queries.lookup_imports, key.clone())
        {
            Ok(terminal) => {
                // Pin the observed lookup-import terminal under the live request
                // lease, exactly as the name-lookup op does.
                self.queries.observed.borrow_mut().record(
                    &self.queries.lookup_imports,
                    &terminal,
                    LookupObservationKey::Import(key),
                );
                match terminal.outcome() {
                    rue_query::QueryOutcome::Success(value) => import_resolution_from_value(value),
                    _ => rue_air::ImportResolution::Absent,
                }
            }
            Err(abort) => {
                self.observe_abort(abort);
                rue_air::ImportResolution::Absent
            }
        }
    }

    fn producer_body_facts(
        &self,
        instance: &crate::FunctionInstanceKey,
    ) -> Option<crate::body_query::ProducedAnonymous> {
        self.queries
            .positive_references
            .borrow_mut()
            .insert(crate::body_query::BodyReference::Callable(instance.clone()));
        self.meter()
            .producer_facts
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        match self.queries.context.query_registered(
            &self.queries.body_produced_anonymous,
            self.body_query_key(instance),
        ) {
            Ok(terminal) => match terminal.outcome() {
                rue_query::QueryOutcome::Success(value) => Some(value.clone()),
                _ => None,
            },
            // A body with no producer-owned anonymous projection may leave this
            // terminal incomplete. The trait remains abort-free, so the
            // request-local status records the typed incomplete state and the
            // terminal boundary rejects publication before any result is used.
            Err(QueryAbort::Canceled) => {
                self.queries
                    .deferred_anonymous_producers
                    .borrow_mut()
                    .insert(instance.clone());
                self.observe_abort(QueryAbort::Canceled);
                None
            }
            Err(abort) => {
                self.observe_abort(abort);
                None
            }
        }
    }

    fn trusted_toolchain_facts(
        &self,
        instance: &crate::FunctionInstanceKey,
    ) -> crate::BodyToolchainDemand {
        self.meter()
            .toolchain_facts
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let empty = crate::BodyToolchainDemand::from_payload_kinds([], None, false);
        match self.queries.context.query_registered(
            &self.queries.body_toolchain_demands,
            self.body_query_key(instance),
        ) {
            Ok(terminal) => match terminal.outcome() {
                rue_query::QueryOutcome::Success(value) => value.clone(),
                _ => empty,
            },
            Err(abort) => {
                self.observe_abort(abort);
                empty
            }
        }
    }
}

// ---------------------------------------------------------------------------
// `ProviderTypeFacts` — the type-syntax/nominal ProviderFacts (RUE-1091 r2).
//
// A second implementation of rue-air's provider-generic type-syntax resolution
// traits (`SemanticModulePathProvider`/`SemanticTypeSyntaxProvider`), the pair
// the production `SemaTypeSyntaxProvider` (rue-air `typeck.rs`) also implements.
// Where production reads the whole-epoch tables (`structs_by_file_name`,
// `enums_by_file_name`, `value_const`, `type_pool`), this impl answers every
// point query from the exact body-fact provider (`CompilerBodyFactProvider`) and
// materializes each consulted nominal into the task-owned overlay. The shared
// One structured resolver consumes both fact sources, so the differential
// proves the storage adapters resolve every type-syntax shape identically.
//
// Owned type domain: T = `DurableType` (`SemanticImportType`), the pool-free
// durable type algebra that IS the byte-identity representation the published
// body compares on. A demand-materialized overlay assigns nominal identities by
// stable key, never by an epoch `type_pool` index, so the differential compares
// the resolved durable structure and the materialized nominal metadata, not a
// pool-relative `StructId`.
//
// Scope of r2 (per the plan's r2 section): primitives, root/module struct/enum,
// root/module type aliases, and the structural wrappers array/`ptr const`/`ptr
// mut`. Deferred with cause (differential documents each): comptime type-ctor
// calls and their value arguments (the boundary exposes no argument-parameterized
// comptime-call op, and `DurableSemanticParameter` carries no parameter name —
// r5); builtin `str` and slice generated-struct names are ANSWERED as of r6a
// (pure durable name facts); `Str(N)` (a generated fixed-capacity struct) →
// r6b with the generated-struct/anonymous family; anonymous producer nominals
// (r4) and well-known `Option` (r6b). These operations are production-compiled
// test fact hosts; production body evaluation uses the direct host contracts.
// ---------------------------------------------------------------------------

/// A recoverable failure surfaced by [`ProviderTypeFacts`]. Aborts never reach
/// the trait surface — `CompilerBodyFactProvider` captures them in its typed
/// request status and returns only a provisional absence-shaped value — so the
/// abort associated type is [`Infallible`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg(test)]
pub(crate) enum ProviderTypeFactsFailure {
    /// A shape whose facts the body-fact boundary does not yet expose in r2. The
    /// message names the deferring slice so a differential can assert the exact
    /// boundary rather than a generic miss.
    Deferred(&'static str),
}

/// The type-syntax/nominal ProviderFacts: resolves type syntax from the exact
/// body-fact provider and materializes consulted nominals into the overlay.
#[cfg(test)]
pub(crate) struct ProviderTypeFacts<'p, 'o, 'db> {
    pub(super) provider: &'p CompilerBodyFactProvider<'db>,
    pub(super) overlay: &'o mut crate::ProviderMaterialization,
}

#[cfg(test)]
pub(super) fn provider_definition_category(
    kind: rue_air::ProviderDefinitionKind,
) -> crate::declaration_candidate::DeclarationCandidateCategory {
    use crate::declaration_candidate::DeclarationCandidateCategory as Cat;
    match kind {
        rue_air::ProviderDefinitionKind::Function => Cat::Function,
        rue_air::ProviderDefinitionKind::Struct => Cat::Struct,
        rue_air::ProviderDefinitionKind::Enum => Cat::Enum,
        rue_air::ProviderDefinitionKind::Const => Cat::ConstCandidate,
        rue_air::ProviderDefinitionKind::Destructor => Cat::Destructor,
        rue_air::ProviderDefinitionKind::Test => Cat::Test,
    }
}

/// The durable type for a primitive type-syntax name, mirroring
/// `rue_air::Type::from_primitive_name` in the durable algebra.
#[cfg(test)]
pub(super) fn primitive_durable_type(name: &str) -> Option<crate::DurableType> {
    use crate::DurableType as T;
    Some(match name {
        "i8" => T::I8,
        "i16" => T::I16,
        "i32" => T::I32,
        "i64" => T::I64,
        "u8" => T::U8,
        "u16" => T::U16,
        "u32" => T::U32,
        "u64" => T::U64,
        "usize" => T::U64,
        "isize" => T::I64,
        "bool" => T::Bool,
        "()" => T::Unit,
        "!" => T::Never,
        "type" => T::ComptimeType,
        _ => return None,
    })
}

#[cfg(test)]
impl<'p, 'o, 'db> ProviderTypeFacts<'p, 'o, 'db> {
    pub(crate) fn new(
        provider: &'p CompilerBodyFactProvider<'db>,
        overlay: &'o mut crate::ProviderMaterialization,
    ) -> Self {
        Self { provider, overlay }
    }

    pub(super) fn candidate_key(
        module: &ModuleId,
        category: crate::declaration_candidate::DeclarationCandidateCategory,
        name: &str,
    ) -> crate::declaration_candidate::DeclarationCandidateKey {
        crate::declaration_candidate::DeclarationCandidateKey {
            module: module.clone(),
            category,
            name: Arc::from(name),
            owner: None,
            duplicate_discriminator: 0,
        }
    }

    pub(super) fn visibility_domain(module: &ModuleId) -> rue_air::SemanticVisibilityDomain {
        rue_air::SemanticVisibilityDomain::from_file_path(Some(module.logical_path()))
    }

    /// Resolve a nominal (`want` = Struct or Enum) named in `module` from the
    /// candidate set, materialize its durable metadata into the overlay, and
    /// return the type fact the shared logic filters on. Absent, kind-mismatched,
    /// and ambiguous candidate sets all resolve to `None`, exactly as a missing
    /// epoch-table entry does.
    pub(super) fn nominal_fact(
        &mut self,
        module: &ModuleId,
        resolution: rue_air::NameResolution,
        name: &str,
        want: rue_air::ProviderDefinitionKind,
    ) -> Option<rue_air::SemanticTypeFact<crate::DurableType, ModuleId>> {
        use rue_air::BodyFactProvider;
        let candidate = match resolution.of_kind(want) {
            rue_air::NameResolution::Unique(candidate) => candidate,
            _ => return None,
        };
        let decl = Self::candidate_key(module, provider_definition_category(want), name);
        let identity = self.provider.declaration_identity(&decl)?;
        let signature = self.provider.signature(&decl)?;
        let key = identity.key.clone();
        let payload = crate::semantic_query_nucleus::DeclarationSemanticValue::from_signature(
            identity,
            signature.signature,
        )
        .payload;
        self.overlay.materialize_nominal(&key, &payload);
        Some(rue_air::SemanticTypeFact {
            value: crate::DurableType::Nominal(key),
            site: module.clone(),
            is_public: candidate.is_public,
            defining_domain: Self::visibility_domain(module),
            defining_file: Arc::from(module.logical_path()),
        })
    }

    /// Resolve a type alias (a `const` whose comptime value is a type) named in
    /// `module`, returning the aliased durable type. A non-type const, or a const
    /// absent from the candidate set, resolves to `None`.
    pub(super) fn alias_fact(
        &mut self,
        module: &ModuleId,
        resolution: rue_air::NameResolution,
        name: &str,
    ) -> Option<rue_air::SemanticTypeFact<crate::DurableType, ModuleId>> {
        use crate::semantic_query_nucleus::ConstResolutionProjection;
        use rue_air::BodyFactProvider;
        let candidate = match resolution.of_kind(rue_air::ProviderDefinitionKind::Const) {
            rue_air::NameResolution::Unique(candidate) => candidate,
            _ => return None,
        };
        let decl = Self::candidate_key(
            module,
            crate::declaration_candidate::DeclarationCandidateCategory::ConstCandidate,
            name,
        );
        let ConstResolutionProjection::Value { value, .. } = self.provider.const_comptime(&decl)?
        else {
            return None;
        };
        let crate::DurableConstValue::Type(ty) = *value else {
            return None;
        };
        Some(rue_air::SemanticTypeFact {
            value: ty,
            site: module.clone(),
            is_public: candidate.is_public,
            defining_domain: Self::visibility_domain(module),
            defining_file: Arc::from(module.logical_path()),
        })
    }

    /// Resolve a module binding (a `const` bound to `@import(...)`) named in
    /// `module`, the value-world analog of the epoch's
    /// `resolve_module_binding_in_file`.
    pub(super) fn module_binding_fact(
        &mut self,
        module: &ModuleId,
        name: &str,
        qualified: bool,
    ) -> Option<rue_air::SemanticModuleBinding<ModuleId, ModuleId>> {
        use crate::semantic_query_nucleus::ConstResolutionProjection;
        use rue_air::BodyFactProvider;
        let resolution = if qualified {
            self.provider
                .lookup_qualified(module, rue_air::ProviderNamespace::ModuleItem, name)
        } else {
            self.provider
                .lookup_unqualified(module, rue_air::ProviderNamespace::ModuleItem, name)
        };
        let candidate = match resolution.of_kind(rue_air::ProviderDefinitionKind::Const) {
            rue_air::NameResolution::Unique(candidate) => candidate,
            _ => return None,
        };
        let decl = Self::candidate_key(
            module,
            crate::declaration_candidate::DeclarationCandidateCategory::ConstCandidate,
            name,
        );
        let ConstResolutionProjection::ModuleBinding { target, .. } =
            self.provider.const_comptime(&decl)?
        else {
            return None;
        };
        Some(rue_air::SemanticModuleBinding {
            target,
            site: module.clone(),
            is_public: candidate.is_public,
            defining_domain: Self::visibility_domain(module),
            defining_file: Arc::from(module.logical_path()),
        })
    }
}

#[cfg(test)]
impl<'p, 'o, 'db> rue_air::SemanticModulePathProvider<ModuleId, ModuleId, ModuleId>
    for ProviderTypeFacts<'p, 'o, 'db>
{
    type Abort = std::convert::Infallible;
    type Failure = ProviderTypeFactsFailure;

    fn root_module_binding(
        &mut self,
        scope: &ModuleId,
        name: &str,
    ) -> rue_air::SemanticProviderResult<
        Option<rue_air::SemanticModuleBinding<ModuleId, ModuleId>>,
        Self::Abort,
        Self::Failure,
    > {
        Ok(self.module_binding_fact(scope, name, false))
    }

    fn module_binding(
        &mut self,
        module: &ModuleId,
        name: &str,
    ) -> rue_air::SemanticProviderResult<
        Option<rue_air::SemanticModuleBinding<ModuleId, ModuleId>>,
        Self::Abort,
        Self::Failure,
    > {
        Ok(self.module_binding_fact(module, name, true))
    }

    fn module_display_name(&self, module: &ModuleId) -> Arc<str> {
        Arc::from(module.logical_path())
    }

    fn accessing_domain(&self, scope: &ModuleId) -> rue_air::SemanticVisibilityDomain {
        Self::visibility_domain(scope)
    }
}

#[cfg(test)]
impl<'p, 'o, 'db>
    rue_air::SemanticTypeSyntaxProvider<
        ModuleId,
        ModuleId,
        ModuleId,
        crate::declaration_candidate::DeclarationCandidateKey,
        Arc<str>,
        crate::DurableType,
        crate::DurableConstValue,
    > for ProviderTypeFacts<'p, 'o, 'db>
{
    fn substituted_type(
        &mut self,
        _scope: &ModuleId,
        _name: &str,
    ) -> rue_air::SemanticProviderResult<Option<crate::DurableType>, Self::Abort, Self::Failure>
    {
        // No lexical comptime substitutions in a bare type-syntax resolution;
        // substitution-bearing scopes are the inference/comptime slices (r5).
        Ok(None)
    }

    fn primitive_type(
        &mut self,
        name: &str,
    ) -> rue_air::SemanticProviderResult<Option<crate::DurableType>, Self::Abort, Self::Failure>
    {
        Ok(primitive_durable_type(name))
    }

    fn builtin_type(
        &mut self,
        _scope: &ModuleId,
        name: &str,
    ) -> rue_air::SemanticProviderResult<Option<crate::DurableType>, Self::Abort, Self::Failure>
    {
        // `str` is the sole builtin nominal reachable as bare type-syntax (the
        // production `builtin_type` answers only `str`; the builtin enums resolve
        // as root nominals). Its durable identity IS the `BuiltinNominal`
        // name+kind — a pure durable fact needing no boundary op: the overlay/pool
        // resolves it to the pre-registered `str` identity exactly as a fresh
        // import epoch does, and `export_type_local` reproduces the same
        // `BuiltinNominal { Struct, "str" }` for the epoch's `str` struct
        // (RUE-1091 r6a — builtin name facts).
        Ok((name == "str").then(|| crate::DurableType::BuiltinNominal {
            kind: rue_air::SemanticImportNominalKind::Struct,
            name: Arc::from("str"),
        }))
    }

    fn root_struct_type(
        &mut self,
        scope: &ModuleId,
        name: &str,
    ) -> rue_air::SemanticProviderResult<
        Option<rue_air::SemanticTypeFact<crate::DurableType, ModuleId>>,
        Self::Abort,
        Self::Failure,
    > {
        use rue_air::BodyFactProvider;
        let resolution =
            self.provider
                .lookup_unqualified(scope, rue_air::ProviderNamespace::ModuleItem, name);
        Ok(self.nominal_fact(
            scope,
            resolution,
            name,
            rue_air::ProviderDefinitionKind::Struct,
        ))
    }

    fn root_enum_type(
        &mut self,
        scope: &ModuleId,
        name: &str,
    ) -> rue_air::SemanticProviderResult<
        Option<rue_air::SemanticTypeFact<crate::DurableType, ModuleId>>,
        Self::Abort,
        Self::Failure,
    > {
        use rue_air::BodyFactProvider;
        let resolution =
            self.provider
                .lookup_unqualified(scope, rue_air::ProviderNamespace::ModuleItem, name);
        Ok(self.nominal_fact(
            scope,
            resolution,
            name,
            rue_air::ProviderDefinitionKind::Enum,
        ))
    }

    fn root_type_alias(
        &mut self,
        scope: &ModuleId,
        name: &str,
    ) -> rue_air::SemanticProviderResult<
        Option<rue_air::SemanticTypeFact<crate::DurableType, ModuleId>>,
        Self::Abort,
        Self::Failure,
    > {
        use rue_air::BodyFactProvider;
        let resolution =
            self.provider
                .lookup_unqualified(scope, rue_air::ProviderNamespace::ModuleItem, name);
        Ok(self.alias_fact(scope, resolution, name))
    }

    fn module_struct_type(
        &mut self,
        module: &ModuleId,
        name: &str,
    ) -> rue_air::SemanticProviderResult<
        Option<rue_air::SemanticTypeFact<crate::DurableType, ModuleId>>,
        Self::Abort,
        Self::Failure,
    > {
        use rue_air::BodyFactProvider;
        let resolution =
            self.provider
                .lookup_qualified(module, rue_air::ProviderNamespace::ModuleItem, name);
        Ok(self.nominal_fact(
            module,
            resolution,
            name,
            rue_air::ProviderDefinitionKind::Struct,
        ))
    }

    fn module_enum_type(
        &mut self,
        module: &ModuleId,
        name: &str,
    ) -> rue_air::SemanticProviderResult<
        Option<rue_air::SemanticTypeFact<crate::DurableType, ModuleId>>,
        Self::Abort,
        Self::Failure,
    > {
        use rue_air::BodyFactProvider;
        let resolution =
            self.provider
                .lookup_qualified(module, rue_air::ProviderNamespace::ModuleItem, name);
        Ok(self.nominal_fact(
            module,
            resolution,
            name,
            rue_air::ProviderDefinitionKind::Enum,
        ))
    }

    fn module_type_alias(
        &mut self,
        module: &ModuleId,
        name: &str,
    ) -> rue_air::SemanticProviderResult<
        Option<rue_air::SemanticTypeFact<crate::DurableType, ModuleId>>,
        Self::Abort,
        Self::Failure,
    > {
        use rue_air::BodyFactProvider;
        let resolution =
            self.provider
                .lookup_qualified(module, rue_air::ProviderNamespace::ModuleItem, name);
        Ok(self.alias_fact(module, resolution, name))
    }

    fn observe_selected_named_type(
        &mut self,
        _name: &str,
        _kind: rue_air::SemanticTypeFactKind,
        _fact: &rue_air::SemanticTypeFact<crate::DurableType, ModuleId>,
    ) -> rue_air::SemanticProviderResult<(), Self::Abort, Self::Failure> {
        // The dependency edge is recorded by the provider op that materialized
        // the fact (lookup / signature / const-comptime terminal), never at this
        // observation and never at render.
        Ok(())
    }

    fn observe_materialized_type(
        &mut self,
        _ty: &crate::DurableType,
    ) -> rue_air::SemanticProviderResult<(), Self::Abort, Self::Failure> {
        Ok(())
    }

    fn allows_qualified_paths(&self, _scope: &ModuleId) -> bool {
        true
    }

    fn resolve_array_length(
        &mut self,
        scope: &ModuleId,
        length: rue_air::SemanticValueSyntax<'_>,
    ) -> rue_air::SemanticProviderResult<Option<u64>, Self::Abort, Self::Failure> {
        // r5a tripwire flip: a named array length that is an integer literal or a
        // scoped `const` now resolves through the boundary (`SignatureFacts`),
        // matching the epoch's literal/const arms of `resolve_array_length_fact`.
        // A comptime CALL in length position (`[T; f(n)]`) still routes through
        // constructor resolution and is honestly deferred here (r6) — this arm
        // covers only the literal and scoped-const facts.
        match length {
            rue_air::SemanticValueSyntax::Integer(value) => {
                u64::try_from(value).map(Some).map_err(|_| {
                    rue_air::SemanticProviderError::Failure(ProviderTypeFactsFailure::Deferred(
                        "invalid integer array length",
                    ))
                })
            }
            rue_air::SemanticValueSyntax::Name(name) => {
                if let Ok(value) = name.parse::<u64>() {
                    return Ok(Some(value));
                }
                match SignatureFacts::new(self.provider).const_value_fact(scope, name) {
                    Some(crate::DurableConstValue::Integer(value)) if value >= 0 => {
                        Ok(Some(value as u64))
                    }
                    _ => Err(rue_air::SemanticProviderError::Failure(
                        ProviderTypeFactsFailure::Deferred(
                            "named array length that is not a literal or scoped const (r6)",
                        ),
                    )),
                }
            }
        }
    }

    fn array_length_from_value(
        &mut self,
        _scope: &ModuleId,
        value: &crate::DurableConstValue,
    ) -> rue_air::SemanticProviderResult<Option<u64>, Self::Abort, Self::Failure> {
        match value {
            crate::DurableConstValue::Integer(value) => {
                u64::try_from(*value).map(Some).map_err(|_| {
                    rue_air::SemanticProviderError::Failure(ProviderTypeFactsFailure::Deferred(
                        "invalid integer array length",
                    ))
                })
            }
            _ => Err(rue_air::SemanticProviderError::Failure(
                ProviderTypeFactsFailure::Deferred("non-integer array length"),
            )),
        }
    }

    fn array_type(
        &mut self,
        element: crate::DurableType,
        length: Option<u64>,
    ) -> rue_air::SemanticProviderResult<crate::DurableType, Self::Abort, Self::Failure> {
        Ok(crate::DurableType::Array {
            element: Arc::new(element),
            len: length.expect("concrete type resolution always resolves array lengths"),
        })
    }

    fn ptr_const_type(
        &mut self,
        pointee: crate::DurableType,
    ) -> rue_air::SemanticProviderResult<crate::DurableType, Self::Abort, Self::Failure> {
        Ok(crate::DurableType::PtrConst(Arc::new(pointee)))
    }

    fn ptr_mut_type(
        &mut self,
        pointee: crate::DurableType,
    ) -> rue_air::SemanticProviderResult<crate::DurableType, Self::Abort, Self::Failure> {
        Ok(crate::DurableType::PtrMut(Arc::new(pointee)))
    }

    fn slice_type(
        &mut self,
        _scope: &ModuleId,
        syntax: &str,
        element: crate::DurableType,
    ) -> rue_air::SemanticProviderResult<crate::DurableType, Self::Abort, Self::Failure> {
        // The generated slice-struct name IS the slice syntax: the epoch's
        // `get_or_create_slice_struct_from_element` keys the fat-pointer struct by
        // `syntax`, and `export_type_local` reproduces it as
        // `Slice { element, name: syntax }`. So the durable form is a pure durable
        // fact needing no boundary op — the overlay/pool mints the same
        // fat-pointer struct on materialization (RUE-1091 r6a — slice name facts).
        Ok(crate::DurableType::Slice {
            element: Arc::new(element),
            name: Arc::from(syntax),
        })
    }

    fn builtin_type_call(
        &mut self,
        _scope: &ModuleId,
        _name: &str,
        _arguments: &[rue_air::SemanticValueSyntax<'_>],
    ) -> rue_air::SemanticProviderResult<Option<crate::DurableType>, Self::Abort, Self::Failure>
    {
        // `Str(N)` is a builtin fixed-capacity string constructor materialized in
        // the epoch; deferred with the other builtin-nominal facts.
        Ok(None)
    }

    fn root_constructor(
        &mut self,
        scope: &ModuleId,
        name: &str,
    ) -> rue_air::SemanticProviderResult<
        Option<
            rue_air::SemanticTypeConstructorHead<
                crate::declaration_candidate::DeclarationCandidateKey,
                Arc<str>,
                ModuleId,
            >,
        >,
        Self::Abort,
        Self::Failure,
    > {
        // r5a tripwire flip: the two capabilities this arm waited on now exist —
        // durable parameters carry names (part 1) and the boundary exposes an
        // argument-parameterized comptime-call op (part 2) — so a faithful
        // constructor head is reconstructed from `signature()` alone through
        // `SignatureFacts`, no declaration shell required.
        use rue_air::BodyFactProvider;
        let resolution =
            self.provider
                .lookup_unqualified(scope, rue_air::ProviderNamespace::ModuleItem, name);
        Ok(SignatureFacts::new(self.provider).constructor_head_fact(scope, resolution, name))
    }

    fn module_constructor(
        &mut self,
        module: &ModuleId,
        name: &str,
    ) -> rue_air::SemanticProviderResult<
        Option<
            rue_air::SemanticTypeConstructorHead<
                crate::declaration_candidate::DeclarationCandidateKey,
                Arc<str>,
                ModuleId,
            >,
        >,
        Self::Abort,
        Self::Failure,
    > {
        use rue_air::BodyFactProvider;
        let resolution =
            self.provider
                .lookup_qualified(module, rue_air::ProviderNamespace::ModuleItem, name);
        Ok(SignatureFacts::new(self.provider).constructor_head_fact(module, resolution, name))
    }

    fn resolve_value_argument(
        &mut self,
        scope: &ModuleId,
        _constructor: &str,
        _head: &rue_air::SemanticTypeConstructorHead<
            crate::declaration_candidate::DeclarationCandidateKey,
            Arc<str>,
            ModuleId,
        >,
        _parameter_index: usize,
        type_arguments: &[(Arc<str>, crate::DurableType)],
        value_arguments: &[(Arc<str>, crate::DurableConstValue)],
        syntax: rue_air::SemanticValueSyntax<'_>,
    ) -> rue_air::SemanticProviderResult<crate::DurableConstValue, Self::Abort, Self::Failure> {
        // r5a tripwire flip: a comptime value argument (literal, a previously
        // bound argument name, or a scoped `const`) resolves through the boundary.
        let syntax = match syntax {
            rue_air::SemanticValueSyntax::Integer(value) => {
                return Ok(crate::DurableConstValue::Integer(value));
            }
            rue_air::SemanticValueSyntax::Name(syntax) => syntax,
        };
        SignatureFacts::new(self.provider)
            .value_argument_fact(scope, syntax, type_arguments, value_arguments)
            .ok_or(rue_air::SemanticProviderError::Failure(
                ProviderTypeFactsFailure::Deferred(
                    "comptime value argument not resolvable from the boundary (r5a covers \
                     literals, bound arguments, and scoped consts)",
                ),
            ))
    }

    fn reduce_comptime_call(
        &mut self,
        head: &rue_air::SemanticTypeConstructorHead<
            crate::declaration_candidate::DeclarationCandidateKey,
            Arc<str>,
            ModuleId,
        >,
        type_arguments: &[(Arc<str>, crate::DurableType)],
        value_arguments: &[(Arc<str>, crate::DurableConstValue)],
    ) -> rue_air::SemanticProviderResult<
        Option<rue_air::SemanticComptimeCallResult<crate::DurableType, crate::DurableConstValue>>,
        Self::Abort,
        Self::Failure,
    > {
        // r5a tripwire flip: reduction runs through the argument-parameterized
        // comptime-call boundary op (part 2). A reduction whose result is (or
        // structurally contains) an anonymous nominal stays deferred in the
        // type-syntax path (RUE-1091 r6b: the endpoint pool mints the identity, but
        // the anonymous reduction result is a body-level durable value with no
        // declaration-level cross-path truth), so the differential records it as an
        // honest gap rather than a silent divergence.
        match SignatureFacts::new(self.provider).reduce_fact(head, type_arguments, value_arguments)
        {
            SignatureReduceOutcome::Reduced(result) => Ok(Some(result)),
            SignatureReduceOutcome::DidNotReduce => Ok(None),
            SignatureReduceOutcome::DeferredAnonymous => Err(
                rue_air::SemanticProviderError::Failure(ProviderTypeFactsFailure::Deferred(
                    "comptime call reducing to an anonymous nominal (body-level durable value)",
                )),
            ),
        }
    }
}

// ---------------------------------------------------------------------------
// `SignatureFacts` — the comptime type-constructor / value-argument ProviderFacts
// (RUE-1091 r5a).
//
// The value-world analog of `ProviderTypeFacts`'s nominal/alias facts, mirroring
// its `nominal_fact`/`alias_fact` conventions: each method answers one signature-
// level fact from the exact body-fact provider (`CompilerBodyFactProvider`) and
// returns owned durable data. It backs the r5a flip of `ProviderTypeFacts`'s
// comptime-call arms (`root_constructor`/`module_constructor`/
// `resolve_value_argument`/`reduce_comptime_call`) and the named-const arm of
// `resolve_array_length`.
//
// Two capabilities this slice landed make it possible: durable parameters now
// carry their source name (part 1), so a constructor head is reconstructed from
// `signature()` alone — the boundary never exposes the declaration shell the
// epoch's `constructor_fact` reads — and the boundary exposes an argument-
// parameterized comptime-call reduction op (part 2), so the reduction runs
// through the same `ComptimeCall` nucleus terminal the production const path
// drives.
//
// SignatureFacts consults only read-only provider terminals and never mints an
// overlay identity, so it borrows `&CompilerBodyFactProvider` alone — a comptime
// call that reduces to an anonymous nominal is deferred to the overlay-owning
// slices (r4/r6), reported as `SignatureReduceOutcome::DeferredAnonymous`.
// ---------------------------------------------------------------------------

/// The comptime type-constructor / value-argument ProviderFacts. Resolves
/// signature-level comptime facts from the exact body-fact provider.
pub(crate) struct SignatureFacts<'p, 'db> {
    pub(super) provider: &'p CompilerBodyFactProvider<'db>,
}

/// Whether a durable type is, or structurally contains, an anonymous nominal.
/// A comptime call reducing to such a type is deferred in the TYPE-SYNTAX path
/// (the anonymous reduction result is a body-level durable value production's
/// declaration binder rejects exporting); the endpoint pool mints the identity
/// itself (RUE-1091 r6b).
pub(super) fn durable_type_uses_anonymous_nominal(ty: &crate::DurableType) -> bool {
    use crate::DurableType as T;
    match ty {
        T::AnonymousNominal(_) => true,
        T::Array { element, .. }
        | T::Slice { element, .. }
        | T::PtrConst(element)
        | T::PtrMut(element) => durable_type_uses_anonymous_nominal(element),
        _ => false,
    }
}

/// The outcome of a boundary comptime-call reduction: a reduced non-anonymous
/// type or value, an anonymous-nominal result deferred in the type-syntax path,
/// or a head that did not reduce.
pub(super) enum SignatureReduceOutcome {
    #[allow(dead_code)]
    Reduced(rue_air::SemanticComptimeCallResult<crate::DurableType, crate::DurableConstValue>),
    DeferredAnonymous,
    DidNotReduce,
}

#[allow(dead_code)]
impl<'p, 'db> SignatureFacts<'p, 'db> {
    pub(crate) fn new(provider: &'p CompilerBodyFactProvider<'db>) -> Self {
        Self { provider }
    }

    pub(super) fn candidate_key(
        module: &ModuleId,
        category: crate::declaration_candidate::DeclarationCandidateCategory,
        name: &str,
    ) -> crate::declaration_candidate::DeclarationCandidateKey {
        crate::declaration_candidate::DeclarationCandidateKey {
            module: module.clone(),
            category,
            name: Arc::from(name),
            owner: None,
            duplicate_discriminator: 0,
        }
    }

    pub(super) fn visibility_domain(module: &ModuleId) -> rue_air::SemanticVisibilityDomain {
        rue_air::SemanticVisibilityDomain::from_file_path(Some(module.logical_path()))
    }

    /// The comptime type-constructor head for a callable named in `module`,
    /// reconstructed from the boundary `signature()` alone. Mirrors
    /// `ProviderTypeFacts::nominal_fact`: a non-unique or non-callable candidate
    /// set resolves to `None`, exactly as a missing epoch entry does. The head's
    /// `parameters` carry each source name (durable, part 1) and the `is_type`
    /// classification the shared comptime-call logic routes arguments on —
    /// `is_comptime && ty == comptime type`, the same predicate the epoch's
    /// `constructor_fact` applies.
    pub(super) fn constructor_head_fact(
        &self,
        module: &ModuleId,
        resolution: rue_air::NameResolution,
        name: &str,
    ) -> Option<
        rue_air::SemanticTypeConstructorHead<
            crate::declaration_candidate::DeclarationCandidateKey,
            Arc<str>,
            ModuleId,
        >,
    > {
        use rue_air::BodyFactProvider;
        let rue_air::NameResolution::Unique(_) =
            resolution.of_kind(rue_air::ProviderDefinitionKind::Function)
        else {
            return None;
        };
        let decl = Self::candidate_key(
            module,
            crate::declaration_candidate::DeclarationCandidateCategory::Function,
            name,
        );
        // Observe the identity terminal (visibility) and the signature terminal
        // (parameters + result) — the same two facts the epoch's `constructor_fact`
        // reads, recorded as dependency edges before the head is returned.
        let identity = self.provider.declaration_identity(&decl)?;
        let signature = self.provider.signature(&decl)?;
        let crate::semantic_query_nucleus::DeclarationSignatureProjection::Callable {
            parameters,
            result,
            ..
        } = signature.signature
        else {
            return None;
        };
        let parameters = parameters
            .iter()
            .map(|parameter| rue_air::SemanticTypeConstructorParameter {
                name: parameter.name.clone(),
                is_comptime: parameter.is_comptime,
                is_type: parameter.is_comptime
                    && parameter.ty == crate::durable_semantics::DurableType::ComptimeType,
            })
            .collect::<Vec<_>>();
        Some(rue_air::SemanticTypeConstructorHead {
            key: decl,
            site: module.clone(),
            parameters: parameters.into(),
            returns_type: result == crate::durable_semantics::DurableType::ComptimeType,
            is_public: identity.is_public,
            defining_domain: Self::visibility_domain(module),
            defining_file: Arc::from(module.logical_path()),
        })
    }

    /// The durable value for one comptime value argument. Mirrors the literal /
    /// bound-argument / scoped-`const` arms of the epoch's `resolve_value_argument`
    /// (`typeck.rs`) and its nucleus twin: an integer or boolean literal, a value
    /// already bound to a parameter name, a type argument named by an earlier
    /// parameter, or a scoped constant resolved through the boundary. Anything
    /// else is `None` (the caller turns that into an honest deferral).
    pub(super) fn value_argument_fact(
        &self,
        scope: &ModuleId,
        syntax: &str,
        type_arguments: &[(Arc<str>, crate::DurableType)],
        value_arguments: &[(Arc<str>, crate::DurableConstValue)],
    ) -> Option<crate::DurableConstValue> {
        use crate::DurableConstValue as V;
        let syntax = syntax.trim();
        if let Ok(value) = syntax.parse::<i128>() {
            return Some(V::Integer(value));
        }
        if syntax == "true" || syntax == "false" {
            return Some(V::Bool(syntax == "true"));
        }
        if let Some((_, value)) = value_arguments
            .iter()
            .find(|(name, _)| name.as_ref() == syntax)
        {
            return Some(value.clone());
        }
        if let Some((_, ty)) = type_arguments
            .iter()
            .find(|(name, _)| name.as_ref() == syntax)
        {
            return Some(V::Type(ty.clone()));
        }
        self.const_value_fact(scope, syntax)
    }

    /// The durable value of a scoped `const` named `name`, resolved through the
    /// boundary lookup + `const_comptime` terminals — the value-world analog of
    /// `ProviderTypeFacts::alias_fact`. `None` for an absent, ambiguous, or
    /// non-value const.
    pub(super) fn const_value_fact(
        &self,
        scope: &ModuleId,
        name: &str,
    ) -> Option<crate::DurableConstValue> {
        use crate::semantic_query_nucleus::ConstResolutionProjection;
        use rue_air::BodyFactProvider;
        let resolution =
            self.provider
                .lookup_unqualified(scope, rue_air::ProviderNamespace::ModuleItem, name);
        let rue_air::NameResolution::Unique(_) =
            resolution.of_kind(rue_air::ProviderDefinitionKind::Const)
        else {
            return None;
        };
        let decl = Self::candidate_key(
            scope,
            crate::declaration_candidate::DeclarationCandidateCategory::ConstCandidate,
            name,
        );
        let ConstResolutionProjection::Value { value, .. } = self.provider.const_comptime(&decl)?
        else {
            return None;
        };
        Some(*value)
    }

    /// Reduce a comptime call at the boundary through the argument-parameterized
    /// comptime-call op, honestly deferring an anonymous-nominal result to r4/r6.
    pub(super) fn reduce_fact(
        &self,
        head: &rue_air::SemanticTypeConstructorHead<
            crate::declaration_candidate::DeclarationCandidateKey,
            Arc<str>,
            ModuleId,
        >,
        type_arguments: &[(Arc<str>, crate::DurableType)],
        value_arguments: &[(Arc<str>, crate::DurableConstValue)],
    ) -> SignatureReduceOutcome {
        use crate::semantic_query_nucleus::ComptimeCallResultProjection as P;
        use rue_air::BodyFactProvider;
        match self
            .provider
            .reduce_comptime_call(&head.key, type_arguments, value_arguments)
        {
            None => SignatureReduceOutcome::DidNotReduce,
            // A reduction whose result is (or structurally contains) an anonymous
            // nominal stays deferred in the TYPE-SYNTAX path. RUE-1091 r6b: the
            // endpoint pool DOES mint the anonymous identity
            // (`BodyIdentityPool::find_or_create_anon`, proven cross-path in
            // `provider_endpoint_facts_anonymous_arm_mints_after_registration`),
            // but the anonymous reduction result is a BODY-level durable value —
            // the production DECLARATION binder rejects exporting it
            // (`AnonymousNominalType`), so the type-syntax resolution has no
            // declaration-level cross-path truth to validate against and stays a
            // documented gap here (owner: the body-level anonymous type-syntax
            // resolution follow-up).
            Some(P::Type(ty)) if durable_type_uses_anonymous_nominal(&ty) => {
                SignatureReduceOutcome::DeferredAnonymous
            }
            Some(P::Type(ty)) => {
                SignatureReduceOutcome::Reduced(rue_air::SemanticComptimeCallResult::Type(ty))
            }
            Some(P::Value(value)) => {
                SignatureReduceOutcome::Reduced(rue_air::SemanticComptimeCallResult::Value(value))
            }
        }
    }
}

#[allow(dead_code)]
impl RevisionedQueryDatabase {
    pub(super) fn compiler_body_provider_queries<'a>(
        &self,
        context: &'a rue_query::QueryContext,
        configuration: crate::semantic_query_nucleus::SemanticQueryConfiguration,
    ) -> CompilerBodyProviderQueries<'a> {
        CompilerBodyProviderQueries {
            context,
            parse_modules: self.parse_modules.clone(),
            module_source_bases: self.module_source_bases.clone(),
            lookup_names: self.lookup_names.clone(),
            lookup_imports: self.lookup_imports.clone(),
            declaration_body_plan_artifacts: self.declaration_body_plan_artifacts.clone(),
            semantic_nucleus: self.semantic_nucleus.clone(),
            body_produced_anonymous: self.body_produced_anonymous.clone(),
            body_toolchain_demands: self.body_toolchain_demands.clone(),
            configuration,
            status: std::rc::Rc::new(std::cell::RefCell::new(CompilerBodyProviderStatus::Ready)),
            deferred_anonymous_producers: std::rc::Rc::new(
                std::cell::RefCell::new(BTreeSet::new()),
            ),
            producer_transport_failure: std::rc::Rc::new(std::cell::RefCell::new(None)),
            observed: std::rc::Rc::new(std::cell::RefCell::new(ObservedLookupRoot::new())),
            positive_references: std::rc::Rc::new(std::cell::RefCell::new(BTreeSet::new())),
            meter: self.provider_observation_meter.clone(),
            shared_durable_payloads: Arc::new(SharedDurablePayloadCache::default()),
        }
    }
}

/// The recorded edges and captured result of one provider-observation probe.
#[cfg(test)]
pub(crate) struct ProviderProbeOutcome<R> {
    /// The value the probe closure produced.
    pub(crate) result: R,
    /// Every dependency node the provider ops recorded, in observation order.
    pub(crate) dependencies: Vec<rue_query::NodeIdentity>,
}

#[cfg(test)]
impl RevisionedQueryDatabase {
    /// Run `run` against a fresh [`CompilerBodyFactProvider`] inside one query
    /// task at `revision`. A non-ready provider returns its typed status and
    /// publishes no probe terminal or provisional result.
    pub(crate) fn probe_body_facts<R>(
        &self,
        revision: Revision,
        configuration: crate::semantic_query_nucleus::SemanticQueryConfiguration,
        label: &str,
        run: impl FnOnce(&CompilerBodyFactProvider<'_>) -> R,
    ) -> Result<ProviderProbeOutcome<R>, CompilerBodyProviderStatus> {
        let captured: std::cell::RefCell<Option<R>> = std::cell::RefCell::new(None);
        let non_ready: std::cell::RefCell<Option<CompilerBodyProviderStatus>> =
            std::cell::RefCell::new(None);
        let run_cell = std::cell::RefCell::new(Some(run));
        let terminal = match self.runtime.query(
            &self.provider_probe,
            revision,
            ProviderProbeKey {
                label: Arc::from(label),
            },
            CancellationToken::new(),
            |context| {
                let provider = CompilerBodyFactProvider::new(
                    self.compiler_body_provider_queries(context, configuration.clone()),
                );
                let run = run_cell.borrow_mut().take().expect("probe runs once");
                let result = run(&provider);
                match provider.finish_status() {
                    Ok(()) => {
                        *captured.borrow_mut() = Some(result);
                        Ok(QueryOutput::success(ProviderProbeValue))
                    }
                    Err(status) => {
                        *non_ready.borrow_mut() = Some(status.clone());
                        Err(match status {
                            CompilerBodyProviderStatus::Fatal(abort) => abort,
                            CompilerBodyProviderStatus::Incomplete(_) => {
                                // The query runtime has no typed incomplete
                                // channel. Keep the terminal absent here and
                                // return the captured provider status below.
                                QueryAbort::Canceled
                            }
                            CompilerBodyProviderStatus::Ready => unreachable!(),
                        })
                    }
                }
            },
        ) {
            Ok(terminal) => terminal,
            Err(abort) => {
                return Err(non_ready
                    .into_inner()
                    .unwrap_or(CompilerBodyProviderStatus::Fatal(abort)));
            }
        };
        let dependencies = terminal
            .dependencies()
            .iter()
            .map(|observation| observation.node.clone())
            .collect();
        Ok(ProviderProbeOutcome {
            result: captured.into_inner().expect("probe captured a result"),
            dependencies,
        })
    }

    /// Success-only convenience for differential tests whose fixture installs
    /// every exact provider prerequisite. Tests exercising incompleteness use
    /// [`Self::probe_body_facts`] and assert its typed status instead.
    pub(crate) fn probe_ready_body_facts<R>(
        &self,
        revision: Revision,
        configuration: crate::semantic_query_nucleus::SemanticQueryConfiguration,
        label: &str,
        run: impl FnOnce(&CompilerBodyFactProvider<'_>) -> R,
    ) -> ProviderProbeOutcome<R> {
        self.probe_body_facts(revision, configuration, label, run)
            .expect("ready provider fixture published its probe terminal")
    }

    /// Drive `run` (a set of provider lookups) as one rooted request under the
    /// unique probe node `probe_label`, then — unless `cancel` aborts the request
    /// before it publishes — promote the request's exact observed lookup-pin set
    /// into the session `PublishedRootLookupLease` under the logical `root` key,
    /// atomically superseding that root's prior published set (RUE-1091).
    ///
    /// The probe node label is kept distinct from the logical root key so a
    /// successor publication of the same logical root re-runs its lookups (its
    /// probe node is fresh) instead of reusing the predecessor probe terminal,
    /// while the promotion still targets the same evolving root. Returns whether a
    /// root was published (and therefore promoted); a canceled request publishes
    /// no root and its observed pins release with the request, never promoted.
    pub(crate) fn publish_lookup_root(
        &self,
        revision: Revision,
        configuration: crate::semantic_query_nucleus::SemanticQueryConfiguration,
        probe_label: &str,
        root: &str,
        cancel: bool,
        run: impl FnOnce(&CompilerBodyFactProvider<'_>),
    ) -> bool {
        let captured: std::cell::RefCell<Option<ObservedLookupRoot>> =
            std::cell::RefCell::new(None);
        let run_cell = std::cell::RefCell::new(Some(run));
        let result = self.runtime.query(
            &self.provider_probe,
            revision,
            ProviderProbeKey {
                label: Arc::from(probe_label),
            },
            CancellationToken::new(),
            |context| {
                let provider = CompilerBodyFactProvider::new(
                    self.compiler_body_provider_queries(context, configuration.clone()),
                );
                let run = run_cell.borrow_mut().take().expect("probe runs once");
                run(&provider);
                if cancel {
                    // Abort before publishing a root: the observed pins drop with
                    // the provider and are never promoted (the never-promote rule).
                    return Err(QueryAbort::Canceled);
                }
                if let Err(status) = provider.finish_status() {
                    return Err(match status {
                        CompilerBodyProviderStatus::Fatal(abort) => abort,
                        CompilerBodyProviderStatus::Incomplete(_) => QueryAbort::Canceled,
                        CompilerBodyProviderStatus::Ready => unreachable!(),
                    });
                }
                *captured.borrow_mut() = Some(provider.take_observed_root());
                Ok(QueryOutput::success(ProviderProbeValue))
            },
        );
        match result {
            Ok(_) => {
                let observed = captured
                    .into_inner()
                    .expect("a published root captured its observations");
                self.promote_published_lookup_root(root.to_owned(), observed);
                true
            }
            Err(_) => false,
        }
    }
}

impl RevisionedQueryDatabase {
    /// An owned snapshot of the live provider-op observation counters.
    pub(crate) fn provider_observation_metrics(
        &self,
    ) -> crate::unstable::ProviderObservationMetrics {
        self.provider_observation_meter.snapshot()
    }

    /// Promote a rooted request's exact observed lookup-terminal pin set into the
    /// session-held [`PublishedRootLookupLease`] at semantic-root publication —
    /// success or deterministic failure (RUE-1091, ADR-0066 §4).
    ///
    /// The successor set is installed for `root` FIRST — it already holds every
    /// pin, acquired while the request lease was live, so the terminals stay
    /// continuously protected — and only THEN is the superseded set for the same
    /// root batch-released, so an edit/error/fix loop's shared lookup terminals
    /// are never left unprotected across the swap (no birth-eviction window). A
    /// key re-derived after its prior incarnation was evicted is metered here by
    /// its fresh incarnation; the supersession's eviction delta is attributed
    /// across the release (grow-with-pressure is reported as a gauge at read
    /// time — the current excess of retained terminals over the floor).
    ///
    /// An empty observation is still the exact successor set and therefore
    /// retires every pin previously owned by this root.
    #[cfg(test)]
    pub(crate) fn promote_published_lookup_root(&self, root: String, observed: ObservedLookupRoot) {
        replace_published_lookup_root(&self.lookup_root_lease, &self.runtime, root, observed);
    }

    /// Reacquire the current terminal for every descriptor carried by a
    /// committed body value, then atomically replace that body's publication
    /// lease. This is the green-validation companion to the evaluator's
    /// attempt handoff: when a lookup recomputes to an equal value, the body
    /// remains green and keeps its semantic transaction, but publication still
    /// advances pin ownership to the lookup's current incarnation.
    pub(crate) fn refresh_published_body_lookup_root(
        &self,
        revision: Revision,
        key: &crate::body_query::BodyQueryKey,
        descriptors: &crate::body_query::BodyLookupObservations,
        cancellation: CancellationToken,
    ) -> Result<(), QueryAbort> {
        let mut observed = ObservedLookupRoot::new();
        for (descriptor, _) in descriptors.terminals.iter() {
            match descriptor {
                LookupObservationKey::Name(lookup) => {
                    let attempt = self.runtime.request_registered(
                        &self.lookup_names,
                        revision,
                        lookup.clone(),
                        cancellation.clone(),
                    );
                    let terminal = attempt.into_result()?;
                    observed.record(
                        &self.lookup_names,
                        &terminal,
                        LookupObservationKey::Name(lookup.clone()),
                    );
                }
                LookupObservationKey::Import(lookup) => {
                    let attempt = self.runtime.request_registered(
                        &self.lookup_imports,
                        revision,
                        lookup.clone(),
                        cancellation.clone(),
                    );
                    let terminal = attempt.into_result()?;
                    observed.record(
                        &self.lookup_imports,
                        &terminal,
                        LookupObservationKey::Import(lookup.clone()),
                    );
                }
            }
        }
        replace_published_lookup_root(
            &self.lookup_root_lease,
            &self.runtime,
            body_lookup_root_identity(key),
            observed,
        );
        Ok(())
    }

    /// An owned snapshot of the lookup-family pressure metrics (RUE-1091,
    /// ADR-0066 §4): the lease-scoped retained working set (published roots,
    /// leased terminals, distinct logical keys), the lookup families' currently
    /// retained nodes and terminals, and the lease-attributed grow-with-pressure,
    /// eviction, and rederivation-after-eviction counters.
    pub(crate) fn lookup_pressure_metrics(&self) -> crate::unstable::LookupPressureMetrics {
        let lease = self
            .lookup_root_lease
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let published_roots = lease.roots.len() as u64;
        let leased_terminals = lease
            .roots
            .values()
            .map(|entry| entry.observations.pins.len())
            .sum::<usize>() as u64;
        let retained_logical_keys = lease
            .roots
            .values()
            .flat_map(|entry| entry.observations.observed_keys.iter().map(|(key, _)| key))
            .collect::<BTreeSet<_>>()
            .len() as u64;
        let name_retention = self.lookup_names.retention();
        // Grow-with-pressure is a gauge: how far a lookup family's retained
        // terminals currently exceed its configured historical floor. The runtime
        // grows past the floor only when every eviction candidate is a protected
        // root, so any excess is exactly the current root's set held above the
        // floor — never eviction of a name merely because a large program
        // consults more than the floor. Zero on production: nothing pins a lookup
        // terminal above the floor without the lease.
        let name_growth =
            (name_retention.terminals as u64).saturating_sub(name_retention.terminal_limit as u64);
        let (extra_nodes, extra_terminals, extra_growth) = {
            let import_retention = self.lookup_imports.retention();
            (
                import_retention.memo_nodes as u64,
                import_retention.terminals as u64,
                (import_retention.terminals as u64)
                    .saturating_sub(import_retention.terminal_limit as u64),
            )
        };
        let retained_family_nodes = name_retention.memo_nodes as u64 + extra_nodes;
        let retained_family_terminals = name_retention.terminals as u64 + extra_terminals;
        let protected_growth = name_growth + extra_growth;
        crate::unstable::LookupPressureMetrics {
            published_roots,
            leased_terminals,
            retained_logical_keys,
            retained_family_nodes,
            retained_family_terminals,
            protected_growth,
            evictions: lease.supersession_evictions,
            rederivations_after_eviction: lease.rederivations_after_eviction,
        }
    }
}
