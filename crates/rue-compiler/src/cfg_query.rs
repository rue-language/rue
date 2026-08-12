//! Canonical per-function CFG query values.
//!
//! The unoptimized family owns AIR-to-CFG lowering. The optimized family owns
//! only the selected optimization pipeline and observes the exact unoptimized
//! terminal. Both publish stable relocation domains and own the body-local AIR,
//! type pool, symbols, strings, and local atoms required by their CFG.

use std::hash::{Hash, Hasher};
use std::sync::Arc;

use rue_query::{QueryAbort, QueryContext, QueryFamily, QueryKey, QueryOutcome, QueryOutput};
use rue_span::Span;

use crate::retained_charge::RetainedCharge;

#[cfg(test)]
thread_local! {
    static CFG_QUERY_KEY_CONSTRUCTIONS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
pub(crate) fn with_test_cfg_query_key_construction_count<T>(run: impl FnOnce() -> T) -> (T, usize) {
    struct Reset(usize);
    impl Drop for Reset {
        fn drop(&mut self) {
            CFG_QUERY_KEY_CONSTRUCTIONS.with(|count| count.set(self.0));
        }
    }

    CFG_QUERY_KEY_CONSTRUCTIONS.with(|count| {
        let reset = Reset(count.replace(0));
        let output = run();
        let constructions = count.get();
        drop(reset);
        (output, constructions)
    })
}

#[derive(Debug, Clone)]
pub(crate) enum CfgSemanticInput {
    Body {
        input: Arc<CfgBodyInput>,
        materialization: Arc<crate::local_semantic_materialization::LocalMaterializationFacts>,
    },
    DropGlue {
        owner: crate::TypeInstanceKey,
        facts: Box<crate::type_queries::DropGlueFacts>,
        materialization: Arc<crate::local_semantic_materialization::LocalMaterializationFacts>,
        body_span: Span,
    },
}

#[derive(Debug, Clone)]
pub(crate) struct CfgBodyInput {
    pub(crate) function: crate::FunctionInstanceKey,
    pub(crate) canonical: Arc<crate::body_query::CanonicalBody>,
    pub(crate) body_span: Span,
}

impl PartialEq for CfgBodyInput {
    fn eq(&self, other: &Self) -> bool {
        self.function == other.function && self.canonical == other.canonical
    }
}

impl Eq for CfgBodyInput {}

impl PartialEq for CfgSemanticInput {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (
                Self::Body {
                    input: left_input,
                    materialization: left_materialization,
                },
                Self::Body {
                    input: right_input,
                    materialization: right_materialization,
                },
            ) => left_input == right_input && left_materialization == right_materialization,
            (
                Self::DropGlue {
                    owner: left_owner,
                    facts: left_facts,
                    materialization: left_materialization,
                    ..
                },
                Self::DropGlue {
                    owner: right_owner,
                    facts: right_facts,
                    materialization: right_materialization,
                    ..
                },
            ) => {
                left_owner == right_owner
                    && left_facts == right_facts
                    && left_materialization == right_materialization
            }
            _ => false,
        }
    }
}

impl Eq for CfgSemanticInput {}

#[derive(Debug, Clone)]
pub(crate) struct CfgQueryKey {
    pub(crate) function: crate::FunctionInstanceKey,
    pub(crate) configuration: crate::semantic_query_nucleus::SemanticQueryConfiguration,
    pub(crate) semantic_input: CfgSemanticInput,
    memo_hash: u64,
    display_identity: Arc<str>,
}

impl CfgQueryKey {
    pub(crate) fn new(
        function: crate::FunctionInstanceKey,
        configuration: crate::semantic_query_nucleus::SemanticQueryConfiguration,
        semantic_input: CfgSemanticInput,
    ) -> Self {
        #[cfg(test)]
        CFG_QUERY_KEY_CONSTRUCTIONS.with(|count| count.set(count.get() + 1));

        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        // The family index needs a well-distributed in-process partition, not a
        // durable fingerprint of the whole semantic payload. Function and
        // configuration distribute independent CFGs across shards; the small
        // bounded set of retained semantic versions for one function may share
        // a bucket. Typed `Eq` below remains authoritative under those deliberate
        // collisions. Avoiding a Debug render of the complete body and local
        // materialization facts keeps key construction allocation-free and
        // proportional to the stable function identity rather than body size.
        function.hash(&mut hasher);
        configuration.hash(&mut hasher);
        let memo_hash = hasher.finish();
        let display_identity: Arc<str> = format!(
            "{function:?};target={:?};preview={:?}",
            configuration.target, configuration.preview_features
        )
        .into();
        Self {
            function,
            configuration,
            semantic_input,
            memo_hash,
            display_identity,
        }
    }
}

impl PartialEq for CfgQueryKey {
    fn eq(&self, other: &Self) -> bool {
        self.function == other.function
            && self.configuration == other.configuration
            && self.semantic_input == other.semantic_input
    }
}

impl Eq for CfgQueryKey {}

impl Hash for CfgQueryKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.memo_hash.hash(state);
    }
}

impl QueryKey for CfgQueryKey {
    fn stable_identity(&self) -> String {
        self.display_identity.to_string()
    }

    fn shared_stable_identity(&self) -> Arc<str> {
        self.display_identity.clone()
    }
}

#[derive(Debug)]
pub(crate) struct AccessorCfgSubgraph {
    pub(crate) roots: std::collections::BTreeMap<crate::FunctionInstanceKey, CfgQueryKey>,
    pub(crate) dependencies:
        std::collections::BTreeMap<crate::FunctionInstanceKey, Arc<[CfgQueryKey]>>,
    pub(crate) accessors: std::collections::BTreeSet<crate::FunctionInstanceKey>,
}

#[derive(Debug)]
pub(crate) enum AccessorCfgSubgraphFailure {
    Missing(crate::FunctionInstanceKey),
    Cycle(crate::FunctionInstanceKey),
}

#[derive(Debug)]
enum AccessorDagFailure<K> {
    Missing(K),
    Cycle(K),
}

#[derive(Debug, Default)]
struct AccessorGraphWork {
    #[cfg(test)]
    validation_edges: usize,
    #[cfg(test)]
    closure_edges: usize,
}

impl AccessorGraphWork {
    fn validation_edge(&mut self) {
        #[cfg(test)]
        {
            self.validation_edges += 1;
        }
    }

    fn closure_edge(&mut self) {
        #[cfg(test)]
        {
            self.closure_edges += 1;
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum VisitState {
    Visiting,
    Complete,
}

/// Validate the whole accessor graph once. Keeping validation separate from
/// root closure construction prevents a chain of accessor-only functions from
/// being walked again from every intermediate accessor.
fn validate_accessor_dag<K: Clone + Ord>(
    direct: &std::collections::BTreeMap<K, Vec<K>>,
    mut contains_key: impl FnMut(&K) -> bool,
    work: &mut AccessorGraphWork,
) -> Result<(), AccessorDagFailure<K>> {
    let mut states = std::collections::BTreeMap::new();
    for start in direct.keys() {
        if states.get(start) == Some(&VisitState::Complete) {
            continue;
        }
        states.insert(start.clone(), VisitState::Visiting);
        let mut stack = vec![(start.clone(), 0usize)];
        while let Some((function, next)) = stack.last_mut() {
            let Some(callee) = direct
                .get(function)
                .and_then(|callees| callees.get(*next))
                .cloned()
            else {
                let function = function.clone();
                states.insert(function, VisitState::Complete);
                stack.pop();
                continue;
            };
            *next += 1;
            work.validation_edge();
            if !contains_key(&callee) {
                return Err(AccessorDagFailure::Missing(callee));
            }
            match states.get(&callee) {
                Some(VisitState::Visiting) => return Err(AccessorDagFailure::Cycle(callee)),
                Some(VisitState::Complete) => {}
                None => {
                    states.insert(callee.clone(), VisitState::Visiting);
                    stack.push((callee, 0));
                }
            }
        }
    }
    Ok(())
}

/// Return the unique transitive callees of one executable root in the same
/// callee-before-caller order used by mandatory splicing. The graph has already
/// been validated, so this walk only computes the flat query dependency list
/// that the optimized-CFG key must own.
fn accessor_postorder<K: Clone + Ord>(
    root: &K,
    direct: &std::collections::BTreeMap<K, Vec<K>>,
    work: &mut AccessorGraphWork,
) -> Vec<K> {
    let mut seen = std::collections::BTreeSet::from([root.clone()]);
    let mut stack = vec![(root.clone(), 0usize)];
    let mut output = Vec::new();
    while let Some((function, next)) = stack.last_mut() {
        let Some(callee) = direct
            .get(function)
            .and_then(|callees| callees.get(*next))
            .cloned()
        else {
            let (function, _) = stack.pop().expect("accessor traversal stack is nonempty");
            if &function != root {
                output.push(function);
            }
            continue;
        };
        *next += 1;
        work.closure_edge();
        if seen.insert(callee.clone()) {
            stack.push((callee, 0));
        }
    }
    output
}

pub(crate) fn accessor_source_name(identity: &crate::FunctionInstanceKey) -> String {
    match identity {
        crate::FunctionInstanceKey::Definition(definition) => definition.name().to_owned(),
        crate::FunctionInstanceKey::Specialization { base, .. } => accessor_source_name(base),
        crate::FunctionInstanceKey::AnonymousMember { member, .. } => member.name.to_string(),
        crate::FunctionInstanceKey::DropGlue(_) => "<accessor>".to_owned(),
    }
}

/// Build the exact accessor dependency closure shared by the session and
/// one-shot query collectors. Dependencies are in callee-before-caller
/// postorder so nested mandatory splices are deterministic.
pub(crate) fn accessor_cfg_subgraph(
    keys: std::collections::BTreeMap<crate::FunctionInstanceKey, CfgQueryKey>,
) -> Result<AccessorCfgSubgraph, AccessorCfgSubgraphFailure> {
    let direct = keys
        .iter()
        .map(|(function, key)| {
            let callees = match &key.semantic_input {
                CfgSemanticInput::Body { input, .. } => canonical_body(&input.canonical)
                    .instructions
                    .iter()
                    .filter_map(|instruction| match &instruction.data {
                        rue_air::SemanticBodyInstData::AccessorCall { function, .. } => {
                            Some(function.clone())
                        }
                        _ => None,
                    })
                    .collect(),
                CfgSemanticInput::DropGlue { .. } => Vec::new(),
            };
            (function.clone(), callees)
        })
        .collect::<std::collections::BTreeMap<_, Vec<_>>>();
    let mut accessors = keys
        .iter()
        .filter_map(|(function, key)| match &key.semantic_input {
            CfgSemanticInput::Body { input, .. }
                if canonical_body(&input.canonical).is_accessor =>
            {
                Some(function.clone())
            }
            _ => None,
        })
        .collect::<std::collections::BTreeSet<_>>();
    accessors.extend(direct.values().flatten().cloned());

    fn facts(
        key: &CfgQueryKey,
    ) -> Option<&crate::local_semantic_materialization::LocalMaterializationFacts> {
        match &key.semantic_input {
            CfgSemanticInput::Body {
                materialization, ..
            } => Some(materialization),
            CfgSemanticInput::DropGlue { .. } => None,
        }
    }
    fn with_facts(
        key: &CfgQueryKey,
        materialization: Arc<crate::local_semantic_materialization::LocalMaterializationFacts>,
    ) -> CfgQueryKey {
        let semantic_input = match &key.semantic_input {
            CfgSemanticInput::Body { input, .. } => CfgSemanticInput::Body {
                input: input.clone(),
                materialization,
            },
            CfgSemanticInput::DropGlue { .. } => key.semantic_input.clone(),
        };
        CfgQueryKey::new(
            key.function.clone(),
            key.configuration.clone(),
            semantic_input,
        )
    }

    let mut graph_work = AccessorGraphWork::default();
    validate_accessor_dag(
        &direct,
        |function| keys.contains_key(function),
        &mut graph_work,
    )
    .map_err(|failure| match failure {
        AccessorDagFailure::Missing(function) => AccessorCfgSubgraphFailure::Missing(function),
        AccessorDagFailure::Cycle(function) => AccessorCfgSubgraphFailure::Cycle(function),
    })?;

    let mut roots = std::collections::BTreeMap::new();
    let mut dependencies = std::collections::BTreeMap::new();
    // Accessors have no standalone ABI unit, so their own closure keys are
    // never consumed. Build flat closures only for executable roots.
    for function in keys
        .keys()
        .filter(|function| !accessors.contains(*function))
    {
        let output = accessor_postorder(function, &direct, &mut graph_work)
            .into_iter()
            .map(|callee| keys[&callee].clone())
            .collect::<Vec<_>>();
        let root = &keys[function];
        if output.is_empty() {
            roots.insert(function.clone(), root.clone());
            dependencies.insert(function.clone(), Arc::<[CfgQueryKey]>::from([]));
            continue;
        }
        let merged = Arc::new(
            crate::local_semantic_materialization::LocalMaterializationFacts::union(
                std::iter::once(root).chain(output.iter()).filter_map(facts),
            ),
        );
        roots.insert(function.clone(), with_facts(root, merged.clone()));
        dependencies.insert(function.clone(), output.iter().cloned().collect());
    }
    Ok(AccessorCfgSubgraph {
        roots,
        dependencies,
        accessors,
    })
}

#[derive(Debug, Clone)]
pub(crate) struct OptimizedCfgQueryKey {
    pub(crate) cfg: CfgQueryKey,
    pub(crate) opt_level: rue_cfg::OptLevel,
    pub(crate) accessor_dependencies: Arc<[CfgQueryKey]>,
    display_identity: Arc<str>,
}

impl OptimizedCfgQueryKey {
    pub(crate) fn new(
        cfg: CfgQueryKey,
        opt_level: rue_cfg::OptLevel,
        accessor_dependencies: Arc<[CfgQueryKey]>,
    ) -> Self {
        let cfg_identity = cfg.shared_stable_identity();
        let display_identity: Arc<str> = format!(
            "{cfg_identity};opt={opt_level:?};accessors={}",
            accessor_dependencies.len()
        )
        .into();
        Self {
            cfg,
            opt_level,
            accessor_dependencies,
            display_identity,
        }
    }
}

impl PartialEq for OptimizedCfgQueryKey {
    fn eq(&self, other: &Self) -> bool {
        self.cfg == other.cfg
            && self.opt_level == other.opt_level
            && self.accessor_dependencies == other.accessor_dependencies
    }
}

impl Eq for OptimizedCfgQueryKey {}

impl std::hash::Hash for OptimizedCfgQueryKey {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.cfg.hash(state);
        std::mem::discriminant(&self.opt_level).hash(state);
        self.accessor_dependencies.hash(state);
    }
}

impl QueryKey for OptimizedCfgQueryKey {
    fn stable_identity(&self) -> String {
        self.display_identity.to_string()
    }

    fn shared_stable_identity(&self) -> Arc<str> {
        self.display_identity.clone()
    }
}

#[derive(Debug, Clone)]
pub(crate) struct CfgRecord {
    /// Exact body-local AIR consumed by CFG construction. Semantic presentation
    /// reads this owned artifact instead of rematerializing a program epoch.
    pub(crate) air: Arc<rue_air::ValidatedAir>,
    pub(crate) source_name: Arc<str>,
    pub(crate) num_locals: u32,
    pub(crate) num_param_slots: u32,
    pub(crate) cfg: rue_cfg::ValidatedCfg,
    pub(crate) domains: crate::durable_cfg::CfgDomainProjection,
    pub(crate) type_pool: rue_air::FrozenTypeInternPool,
    pub(crate) interner: Arc<lasso::ThreadedRodeo>,
    /// Current logical retained charge of the shared append-only `interner`.
    /// `interner_retained_entries` records this CFG's publication width, so an
    /// accessor import refreshes the shared charge only after actual growth.
    interner_retained_charge: Arc<std::sync::atomic::AtomicU64>,
    interner_retained_entries: u64,
    pub(crate) strings: Arc<[String]>,
    pub(crate) local_atoms:
        Arc<[rue_air::LocalAtomRecord<crate::StableDefinitionKey, crate::ModuleId>]>,
    /// Owned current-domain aliases available while lowering this CFG. The
    /// domain includes cleanup aliases that optimization may leave unused; its
    /// stable identities and ABI classifications are still exact CFG-query
    /// dependencies, never a caller-owned program resolver.
    pub(crate) codegen: Arc<CfgCodegenDomain>,
    pub(crate) materialization_warnings: Arc<[rue_error::CompileWarning]>,
    pub(crate) body_span: Span,
    pub(crate) warnings: Arc<[rue_error::CompileWarning]>,
    pub(crate) implicit_destructor_targets: Arc<[crate::TypeInstanceKey]>,
    pub(crate) implicit_destructor_dependencies_complete: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CfgCodegenDomain {
    pub(crate) defined_symbol: Arc<str>,
    pub(crate) symbol_mappings: Arc<std::collections::BTreeMap<String, String>>,
    pub(crate) foreign_symbols: Arc<std::collections::BTreeSet<String>>,
}

#[derive(Debug, Clone)]
pub(crate) enum CfgValue {
    Available(Arc<CfgRecord>),
    Failure {
        errors: crate::CompileErrors,
        body_span: Span,
    },
}

impl RetainedCharge for lasso::ThreadedRodeo {
    fn retained_charge(&self) -> u64 {
        measure_interner_retained_charge(self).0
    }
}

fn measure_interner_retained_charge(interner: &lasso::ThreadedRodeo) -> (u64, u64) {
    let entries = interner.len() as u64;
    let utf8_bytes = interner.utf8_bytes() as u64;
    (
        entries
            .saturating_mul(std::mem::size_of::<lasso::Spur>() as u64)
            .saturating_add(utf8_bytes),
        entries,
    )
}

fn interner_header_retained_charge(interner: &lasso::ThreadedRodeo) -> u64 {
    // `utf8_bytes` is measurement-only bookkeeping added to the vendored
    // interner. Exclude that one atomic from the logical artifact charge so
    // replacing the traversal does not change retention policy.
    (std::mem::size_of_val(interner) as u64)
        .saturating_sub(std::mem::size_of::<std::sync::atomic::AtomicUsize>() as u64)
}

fn refresh_interner_retained_charge(
    interner: &lasso::ThreadedRodeo,
    charge: &std::sync::atomic::AtomicU64,
    published_entries: u64,
) -> bool {
    if interner.len() as u64 == published_entries {
        return false;
    }
    let (payload, _) = measure_interner_retained_charge(interner);
    charge.fetch_max(
        interner_header_retained_charge(interner).saturating_add(payload),
        std::sync::atomic::Ordering::Relaxed,
    );
    true
}

struct InternerChargeRefresh {
    interner: Arc<lasso::ThreadedRodeo>,
    charge: Arc<std::sync::atomic::AtomicU64>,
    published_entries: u64,
}

impl Drop for InternerChargeRefresh {
    fn drop(&mut self) {
        refresh_interner_retained_charge(&self.interner, &self.charge, self.published_entries);
    }
}

impl RetainedCharge for rue_air::ValidatedAir {
    fn retained_charge(&self) -> u64 {
        let payload = self.payload_store_stats();
        std::mem::size_of_val(self.instructions()) as u64
            + payload.word_store_logical_bytes as u64
            + payload.projection_store_logical_bytes as u64
            + payload.place_store_logical_bytes as u64
            + std::mem::size_of_val(self.param_drops()) as u64
    }
}

impl RetainedCharge for rue_cfg::ValidatedCfg {
    fn retained_charge(&self) -> u64 {
        let payload = self.payload_storage_stats();
        let blocks = std::mem::size_of_val(self.blocks()) as u64;
        let blocks = self.blocks().iter().fold(blocks, |charge, block| {
            charge
                .saturating_add(
                    (block.params.len() * std::mem::size_of::<(rue_cfg::CfgValue, rue_air::Type)>())
                        as u64,
                )
                .saturating_add(
                    (block.insts.len() * std::mem::size_of::<rue_cfg::CfgValue>()) as u64,
                )
        });
        blocks
            .saturating_add((self.value_count() * std::mem::size_of::<rue_cfg::CfgInst>()) as u64)
            .saturating_add(payload.value_store_logical_bytes as u64)
            .saturating_add(payload.call_store_logical_bytes as u64)
            .saturating_add(payload.switch_store_logical_bytes as u64)
            .saturating_add(payload.projection_store_logical_bytes as u64)
            .saturating_add(self.fn_name().len() as u64)
            .saturating_add((self.param_modes().len() * 2 * std::mem::size_of::<bool>()) as u64)
            .saturating_add(std::mem::size_of_val(self.source_param_abi()) as u64)
    }
}

impl RetainedCharge for rue_air::FrozenTypeInternPool {
    fn retained_charge(&self) -> u64 {
        let mut charge = (self.len() * std::mem::size_of::<rue_air::Type>()) as u64;
        for ty in self.all_types() {
            if let Some(id) = ty.as_struct() {
                let definition = self.struct_def(id);
                charge = charge
                    .saturating_add(definition.name.len() as u64)
                    .saturating_add(
                        (definition.fields.len() * std::mem::size_of::<rue_air::StructField>())
                            as u64,
                    )
                    .saturating_add(definition.destructor.retained_charge());
                charge = definition.fields.iter().fold(charge, |charge, field| {
                    charge.saturating_add(field.name.len() as u64)
                });
            } else if let Some(id) = ty.as_enum() {
                let definition = self.enum_def(id);
                charge = charge
                    .saturating_add(definition.name.len() as u64)
                    .saturating_add(definition.variants.retained_charge())
                    .saturating_add(
                        (definition.variant_payloads.len()
                            * std::mem::size_of::<Vec<rue_air::Type>>())
                            as u64,
                    );
                charge = definition
                    .variant_payloads
                    .iter()
                    .fold(charge, |charge, payload| {
                        charge.saturating_add(
                            (payload.len() * std::mem::size_of::<rue_air::Type>()) as u64,
                        )
                    });
            }
        }
        charge
    }
}

impl RetainedCharge for CfgBodyInput {
    fn retained_charge(&self) -> u64 {
        self.function
            .retained_charge()
            .saturating_add(self.canonical.retained_charge())
    }
}

impl RetainedCharge for CfgSemanticInput {
    fn retained_charge(&self) -> u64 {
        match self {
            Self::Body {
                input,
                materialization,
            } => input
                .retained_charge()
                .saturating_add(materialization.retained_charge()),
            Self::DropGlue {
                owner,
                facts,
                materialization,
                ..
            } => owner
                .retained_charge()
                .saturating_add(facts.retained_charge())
                .saturating_add(materialization.retained_charge()),
        }
    }
}

impl RetainedCharge for CfgCodegenDomain {
    fn retained_charge(&self) -> u64 {
        self.defined_symbol
            .retained_charge()
            .saturating_add(self.symbol_mappings.retained_charge())
            .saturating_add(self.foreign_symbols.retained_charge())
    }
}

impl RetainedCharge for CfgRecord {
    fn retained_charge(&self) -> u64 {
        self.air
            .retained_charge()
            .saturating_add(self.source_name.retained_charge())
            .saturating_add(self.cfg.retained_charge())
            .saturating_add(self.domains.retained_charge())
            .saturating_add(self.type_pool.retained_charge())
            .saturating_add(
                self.interner_retained_charge
                    .load(std::sync::atomic::Ordering::Relaxed),
            )
            .saturating_add(std::mem::size_of::<std::sync::atomic::AtomicU64>() as u64)
            .saturating_add(self.strings.retained_charge())
            .saturating_add(self.local_atoms.retained_charge())
            .saturating_add(self.codegen.retained_charge())
            .saturating_add(self.materialization_warnings.retained_charge())
            .saturating_add(self.warnings.retained_charge())
            .saturating_add(self.implicit_destructor_targets.retained_charge())
    }
}

impl RetainedCharge for CfgValue {
    fn retained_charge(&self) -> u64 {
        match self {
            Self::Available(record) => record.retained_charge(),
            Self::Failure { errors, .. } => errors.retained_charge(),
        }
    }
}

pub(crate) fn cfg_value_equal(left: &CfgValue, right: &CfgValue) -> bool {
    match (left, right) {
        // A computed Cfg terminal is the direct semantic input of OptimizedCfg.
        // If Cfg was forced to recompute, conservatively dirty its consumer;
        // exact-key hits are reused without invoking this equality hook.
        (CfgValue::Available(_), CfgValue::Available(_)) => false,
        (
            CfgValue::Failure {
                errors: left_errors,
                ..
            },
            CfgValue::Failure {
                errors: right_errors,
                ..
            },
        ) => left_errors == right_errors,
        _ => false,
    }
}

fn map_span(span: Span, old: Span, new: Span) -> Span {
    if span.file_id == old.file_id && span.start >= old.start && span.end <= old.end {
        Span {
            file_id: new.file_id,
            start: new.start + (span.start - old.start),
            end: new.start + (span.end - old.start),
        }
    } else {
        span
    }
}

pub(crate) fn import_errors(
    errors: &crate::CompileErrors,
    old: Span,
    new: Span,
) -> crate::CompileErrors {
    errors
        .iter()
        .cloned()
        .map(|error| error.map_spans(|span| map_span(span, old, new)))
        .collect::<Vec<_>>()
        .into()
}

pub(crate) fn import_warnings(
    warnings: &[rue_error::CompileWarning],
    old: Span,
    new: Span,
) -> Vec<rue_error::CompileWarning> {
    warnings
        .iter()
        .cloned()
        .map(|warning| warning.map_spans(|span| map_span(span, old, new)))
        .collect()
}

pub(crate) fn collect_type_dependencies(
    ty: &rue_air::SemanticImportType<crate::StableDefinitionKey, crate::ModuleId>,
    output: &mut std::collections::BTreeSet<crate::TypeInstanceKey>,
) {
    output.insert(type_instance_from_semantic(ty));
    // Array representation and CFG indexing depend on the element layout.
    // Pointer and slice representation does not depend on the pointee.
    if let rue_air::SemanticImportType::Array { element, .. } = ty {
        collect_type_dependencies(element, output);
    }
}

pub(crate) fn collect_drop_type_dependency(
    ty: &rue_air::SemanticImportType<crate::StableDefinitionKey, crate::ModuleId>,
    output: &mut std::collections::BTreeSet<crate::TypeInstanceKey>,
) {
    output.insert(type_instance_from_semantic(ty));
}

fn type_instance_from_semantic(
    ty: &rue_air::SemanticImportType<crate::StableDefinitionKey, crate::ModuleId>,
) -> crate::TypeInstanceKey {
    use rue_air::SemanticImportType as T;
    match ty {
        T::I8 => crate::TypeInstanceKey::I8,
        T::I16 => crate::TypeInstanceKey::I16,
        T::I32 => crate::TypeInstanceKey::I32,
        T::I64 => crate::TypeInstanceKey::I64,
        T::U8 => crate::TypeInstanceKey::U8,
        T::U16 => crate::TypeInstanceKey::U16,
        T::U32 => crate::TypeInstanceKey::U32,
        T::U64 => crate::TypeInstanceKey::U64,
        T::Bool => crate::TypeInstanceKey::Bool,
        T::Unit => crate::TypeInstanceKey::Unit,
        T::Never => crate::TypeInstanceKey::Never,
        T::ComptimeType => crate::TypeInstanceKey::ComptimeType,
        T::BuiltinNominal { kind, name } => crate::TypeInstanceKey::BuiltinNominal {
            kind: match kind {
                rue_air::SemanticImportNominalKind::Struct => crate::AnonymousNominalKind::Struct,
                rue_air::SemanticImportNominalKind::Enum => crate::AnonymousNominalKind::Enum,
            },
            name: name.clone(),
        },
        T::Nominal(definition) => {
            crate::TypeInstanceKey::Nominal(crate::NominalInstanceKey::Named(definition.clone()))
        }
        T::AnonymousNominal(identity) => {
            crate::TypeInstanceKey::Nominal(crate::NominalInstanceKey::Anonymous(identity.clone()))
        }
        T::Array { element, len } => crate::TypeInstanceKey::Array {
            element: Box::new(type_instance_from_semantic(element)),
            len: *len,
        },
        T::Slice { element, name } => crate::TypeInstanceKey::Slice {
            element: Box::new(type_instance_from_semantic(element)),
            name: name.clone(),
        },
        T::PtrConst(element) => {
            crate::TypeInstanceKey::PtrConst(Box::new(type_instance_from_semantic(element)))
        }
        T::PtrMut(element) => {
            crate::TypeInstanceKey::PtrMut(Box::new(type_instance_from_semantic(element)))
        }
        T::Module(module) => crate::TypeInstanceKey::Module(module.clone()),
        T::GenericParameter(index) => crate::TypeInstanceKey::GenericParameter(*index),
    }
}

pub(crate) fn evaluate_cfg(
    context: &QueryContext,
    layouts: &QueryFamily<crate::type_queries::TypeQueryKey, crate::type_queries::LayoutValue>,
    type_facts: &QueryFamily<
        crate::type_queries::TypeQueryKey,
        crate::type_queries::TypeFactsValue,
    >,
    drop_glues: &QueryFamily<crate::type_queries::TypeQueryKey, crate::type_queries::DropGlueValue>,
    call_abis: &QueryFamily<
        crate::type_queries::CallAbiQueryKey,
        crate::type_queries::CallAbiValue,
    >,
    key: &CfgQueryKey,
) -> Result<QueryOutput<CfgValue>, QueryAbort> {
    let _span = tracing::info_span!("cfg_construction", phase = "cfg_and_optimization").entered();
    let value =
        materialize_and_build_cfg(context, layouts, type_facts, drop_glues, call_abis, key)?;
    let kind = if matches!(value, CfgValue::Failure { .. }) {
        rue_query::QueryTerminalKind::Failure
    } else {
        rue_query::QueryTerminalKind::Success
    };
    Ok(QueryOutput::success(value).with_terminal_kind(kind))
}

fn internal_failure(message: impl Into<String>, body_span: Span) -> CfgValue {
    CfgValue::Failure {
        errors: crate::CompileError::new(
            rue_error::ErrorKind::InternalError(message.into()),
            body_span,
        )
        .into(),
        body_span,
    }
}

fn canonical_body(
    canonical: &crate::body_query::CanonicalBody,
) -> &rue_air::SemanticBody<crate::StableDefinitionKey, crate::ModuleId> {
    match canonical {
        crate::body_query::CanonicalBody::Ordinary { body, .. }
        | crate::body_query::CanonicalBody::Anonymous { body, .. }
        | crate::body_query::CanonicalBody::Specialization { body, .. } => body,
    }
}

fn collect_plan_types(
    owner: &crate::TypeInstanceKey,
    facts: &crate::type_queries::DropGlueFacts,
) -> std::collections::BTreeSet<crate::TypeInstanceKey> {
    let mut output = std::collections::BTreeSet::from([owner.clone()]);
    output.extend(facts.nested.iter().cloned());
    match &facts.plan {
        crate::type_queries::DropGluePlan::Struct { fields } => {
            output.extend(fields.iter().map(|field| field.ty.clone()));
        }
        crate::type_queries::DropGluePlan::Array { element, .. } => {
            output.insert(element.clone());
        }
        crate::type_queries::DropGluePlan::Enum { variants } => {
            output.extend(
                variants
                    .iter()
                    .flat_map(|variant| variant.fields.iter())
                    .map(|field| field.ty.clone()),
            );
        }
        crate::type_queries::DropGluePlan::None => {}
    }
    output
}

#[derive(Debug, Clone, Copy)]
struct CfgConstructionBreakdown {
    input_preparation_ns: u64,
    semantic_materialization_ns: u64,
    domain_prerequisites_ns: u64,
    domain_projection_ns: u64,
    prerequisite_collection_ns: u64,
    prerequisite_queries_ns: u64,
}

fn elapsed_ns(started: std::time::Instant) -> u64 {
    u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX)
}

fn materialize_and_build_cfg(
    context: &QueryContext,
    layouts: &QueryFamily<crate::type_queries::TypeQueryKey, crate::type_queries::LayoutValue>,
    type_facts: &QueryFamily<
        crate::type_queries::TypeQueryKey,
        crate::type_queries::TypeFactsValue,
    >,
    drop_glues: &QueryFamily<crate::type_queries::TypeQueryKey, crate::type_queries::DropGlueValue>,
    call_abis: &QueryFamily<
        crate::type_queries::CallAbiQueryKey,
        crate::type_queries::CallAbiValue,
    >,
    key: &CfgQueryKey,
) -> Result<CfgValue, QueryAbort> {
    let input_preparation_started = std::time::Instant::now();
    let synthesized;
    let (body, body_span, facts) = match &key.semantic_input {
        CfgSemanticInput::Body {
            input,
            materialization,
        } => (
            canonical_body(&input.canonical),
            input.body_span,
            materialization.as_ref(),
        ),
        CfgSemanticInput::DropGlue {
            owner,
            facts,
            materialization,
            body_span,
        } => {
            let mut slots = std::collections::BTreeMap::new();
            let plan_types = collect_plan_types(owner, facts)
                .into_iter()
                .collect::<Vec<_>>();
            let terminals = context.query_registered_adaptive_batch(
                layouts,
                plan_types
                    .iter()
                    .cloned()
                    .map(|ty| crate::type_queries::TypeQueryKey {
                        ty,
                        configuration: key.configuration.clone(),
                    }),
            )?;
            for (ty, terminal) in plan_types.into_iter().zip(terminals) {
                let QueryOutcome::Success(value) = terminal.outcome() else {
                    unreachable!("Layout publishes typed values")
                };
                let crate::type_queries::LayoutValue::Available(layout) = value else {
                    return Ok(internal_failure(
                        format!("drop-glue layout unavailable for {ty:?}: {value:?}"),
                        *body_span,
                    ));
                };
                slots.insert(ty, layout.abi_slots);
            }
            synthesized =
                match crate::drop_glue::synthesize_canonical_drop_glue(owner, facts, &slots) {
                    Ok(body) => body,
                    Err(error) => return Ok(internal_failure(error.as_ref(), *body_span)),
                };
            (&synthesized, *body_span, materialization.as_ref())
        }
    };
    let mut builtin_facts = Vec::with_capacity(facts.builtin_nominals.len());
    let builtin_terminals = context.query_registered_adaptive_batch(
        type_facts,
        facts
            .builtin_nominals
            .iter()
            .map(|request| crate::type_queries::TypeQueryKey {
                ty: request.query_ty.clone(),
                configuration: key.configuration.clone(),
            }),
    )?;
    for (request, terminal) in facts.builtin_nominals.iter().zip(builtin_terminals) {
        let QueryOutcome::Success(value) = terminal.outcome() else {
            unreachable!("TypeFacts publishes typed values")
        };
        let crate::type_queries::TypeFactsValue::Available(value) = value else {
            return Ok(internal_failure(
                format!("builtin nominal facts unavailable for {request:?}: {value:?}"),
                body_span,
            ));
        };
        builtin_facts.push(
            crate::local_semantic_materialization::LocalBuiltinNominalFact {
                request: request.clone(),
                facts: value.as_ref().clone(),
            },
        );
    }
    context.record_work(rue_query::WorkItem::new("cfg.materialize.attempts", 1));
    let input_preparation_ns = elapsed_ns(input_preparation_started);
    let materialization_started = std::time::Instant::now();
    let materialized = match &key.semantic_input {
        CfgSemanticInput::Body { input, .. } => {
            crate::local_semantic_materialization::materialize_canonical_body(
                &input.canonical,
                body_span,
                &facts.declarations,
                &facts.anonymous_nominals,
                &facts.callables,
                &facts.nominal_metadata,
                &facts.modules,
                &builtin_facts,
                &facts.required_types,
            )
        }
        CfgSemanticInput::DropGlue { owner, .. } => {
            crate::local_semantic_materialization::materialize_semantic_body(
                crate::FunctionInstanceKey::DropGlue(Box::new(owner.clone())),
                body,
                body_span,
                &facts.declarations,
                &facts.anonymous_nominals,
                &facts.callables,
                &facts.nominal_metadata,
                &facts.modules,
                &builtin_facts,
                &facts.required_types,
            )
        }
    };
    let semantic_materialization_ns = elapsed_ns(materialization_started);
    let domain_prerequisites_started = std::time::Instant::now();
    let materialized = match materialized {
        Ok(value) => value,
        Err(error) => {
            context.record_work(rue_query::WorkItem::new("cfg.materialize.failures", 1));
            return Ok(internal_failure(
                format!("canonical CFG materialization failed: {error:?}"),
                body_span,
            ));
        }
    };
    context.record_work(rue_query::WorkItem::new("cfg.materialize.successes", 1));

    // Backstop for the composite-type ceiling, checked before this body's type
    // graph is projected or its layouts, drop facts, and drop glues are
    // queried. A latched universe aliases later registrations onto the final
    // pool entry, and those backend-facing reads require a well-kinded graph,
    // so the check has to precede them rather than guard only `build_cfg`.
    // Declaration binding normally reports the limit first (spec C.1:2); this
    // covers a universe that latched after binding completed.
    if materialized.type_pool.capacity_exceeded() {
        return Ok(composite_type_limit_failure(materialized.body_span));
    }

    let domain_projection_started = std::time::Instant::now();
    let domains = match crate::durable_cfg::CfgDomainProjection::from_local_body(
        &materialized,
        body,
        |ty| {
            crate::durable_cfg::canonical_type_from_live(
                ty,
                &materialized.type_pool,
                &materialized.aggregate_types,
            )
        },
        |symbol| {
            let name = materialized.interner.resolve(&symbol);
            facts
                .callables
                .iter()
                .find(|fact| fact.symbol.as_ref() == name)
                .map(|fact| fact.identity.clone())
        },
    ) {
        Ok(value) => value,
        Err(error) => {
            return Ok(internal_failure(
                format!("canonical CFG domain projection failed: {error:?}"),
                body_span,
            ));
        }
    };
    let domain_projection_ns = elapsed_ns(domain_projection_started);

    let prerequisite_collection_started = std::time::Instant::now();
    let mut layout_dependencies = std::collections::BTreeSet::new();
    let mut drop_dependencies = std::collections::BTreeSet::new();
    let mut stable_types_scanned = 0_u64;
    for ty in domains.stable_types() {
        stable_types_scanned = stable_types_scanned.saturating_add(1);
        collect_type_dependencies(ty, &mut layout_dependencies);
        collect_drop_type_dependency(ty, &mut drop_dependencies);
    }
    let layout_prerequisite_requests = layout_dependencies.len() as u64;
    let drop_prerequisite_requests = drop_dependencies.len() as u64;
    context.record_work(rue_query::WorkItem::new(
        "cfg.prerequisite.stable-types-scanned",
        stable_types_scanned,
    ));
    context.record_work(rue_query::WorkItem::new(
        "cfg.prerequisite.layout-requests",
        layout_prerequisite_requests,
    ));
    context.record_work(rue_query::WorkItem::new(
        "cfg.prerequisite.drop-glue-requests",
        drop_prerequisite_requests,
    ));
    let prerequisite_collection_ns = elapsed_ns(prerequisite_collection_started);
    let prerequisite_queries_started = std::time::Instant::now();
    context.query_registered_adaptive_batch(
        layouts,
        layout_dependencies
            .into_iter()
            .map(|ty| crate::type_queries::TypeQueryKey {
                ty,
                configuration: key.configuration.clone(),
            }),
    )?;
    let drop_dependencies = drop_dependencies
        .into_iter()
        .map(|ty| crate::type_queries::TypeQueryKey {
            ty,
            configuration: key.configuration.clone(),
        })
        .collect::<Vec<_>>();
    // Every DropGlue terminal observes the exact TypeFacts terminal for the
    // same key. Keep one direct CFG edge to that transitive cone instead of
    // issuing and validating a duplicate top-level TypeFacts request.
    context.query_registered_adaptive_batch(drop_glues, drop_dependencies)?;
    let prerequisite_queries_ns = elapsed_ns(prerequisite_queries_started);
    let breakdown = CfgConstructionBreakdown {
        input_preparation_ns,
        semantic_materialization_ns,
        domain_prerequisites_ns: elapsed_ns(domain_prerequisites_started),
        domain_projection_ns,
        prerequisite_collection_ns,
        prerequisite_queries_ns,
    };
    build_cfg(context, call_abis, key, materialized, domains, breakdown)
}

/// The published composite-type ceiling was reached while this body's type
/// universe was being built (spec Appendix C.6:1).
///
/// Composite interning is infallible at hundreds of sites, so the pool latches
/// the rejection and stops growing; this is the query boundary that turns the
/// latch into the `E1401` diagnostic spec C.1:2 requires, instead of letting a
/// wrapped 24-bit index alias two distinct types.
fn composite_type_limit_failure(body_span: Span) -> CfgValue {
    CfgValue::Failure {
        errors: crate::CompileError::new(
            rue_error::ErrorKind::CompilerResourceLimit(rue_air::composite_type_limit_message()),
            body_span,
        )
        .into(),
        body_span,
    }
}

fn build_cfg(
    context: &QueryContext,
    call_abis: &QueryFamily<
        crate::type_queries::CallAbiQueryKey,
        crate::type_queries::CallAbiValue,
    >,
    key: &CfgQueryKey,
    materialized: crate::local_semantic_materialization::LocalSemanticMaterialization,
    domains: crate::durable_cfg::CfgDomainProjection,
    breakdown: CfgConstructionBreakdown,
) -> Result<CfgValue, QueryAbort> {
    context.record_work(rue_query::WorkItem::new("cfg.build.attempts", 1));
    context.record_work(rue_query::WorkItem::new(
        "cfg.air.instructions",
        materialized.air.instructions().len() as u64,
    ));
    let builder_started = std::time::Instant::now();
    let output = rue_cfg::CfgBuilder::build(
        &materialized.air,
        materialized.num_locals,
        materialized.num_param_slots,
        &materialized.name,
        &materialized.type_pool,
        materialized.param_modes.clone(),
        &materialized.interner,
        materialized.allow_unreachable_code,
        materialized.callable_kind,
    );
    let cfg_builder_ns = elapsed_ns(builder_started);
    let publication_started = std::time::Instant::now();
    if !output.errors.is_empty() {
        context.record_work(rue_query::WorkItem::new("cfg.build.failures", 1));
        return Ok(CfgValue::Failure {
            errors: output.errors.into(),
            body_span: materialized.body_span,
        });
    }
    context.record_work(rue_query::WorkItem::new("cfg.build.successes", 1));
    context.record_work(rue_query::WorkItem::new(
        "cfg.warnings",
        output.warnings.len() as u64,
    ));
    let cfg = output
        .cfg
        .as_ref()
        .expect("successful CFG construction publishes a validated CFG");
    let callables = match domains.runtime_callables(cfg) {
        Ok(value) => value,
        Err(error) => {
            return Ok(internal_failure(
                format!("canonical runtime-call projection failed: {error:?}"),
                materialized.body_span,
            ));
        }
    };
    let call_abi_terminals = context.query_registered_adaptive_batch(
        call_abis,
        callables
            .iter()
            .map(|callable| crate::type_queries::CallAbiQueryKey {
                callable: callable.clone(),
                configuration: key.configuration.clone(),
            }),
    )?;
    let mut call_abi_facts = std::collections::BTreeMap::new();
    for (callable, terminal) in callables.into_iter().zip(call_abi_terminals) {
        let QueryOutcome::Success(value) = terminal.outcome() else {
            unreachable!("CallAbi publishes typed values")
        };
        match value {
            crate::type_queries::CallAbiValue::Available(facts) => {
                call_abi_facts.insert(callable, facts.clone());
            }
            crate::type_queries::CallAbiValue::Failure(failure) => {
                let detail = match failure {
                    crate::type_queries::TypeQueryFailure::Unavailable(detail)
                    | crate::type_queries::TypeQueryFailure::Invalid(detail) => detail,
                };
                return Ok(internal_failure(
                    format!("call ABI unavailable for {callable:?}: {detail}"),
                    materialized.body_span,
                ));
            }
        }
    }
    let codegen = match domains.codegen_domain(
        &key.function,
        &materialized.name,
        &materialized.type_pool,
        &materialized.interner,
        &call_abi_facts,
    ) {
        Ok(value) => value,
        Err(error) => {
            return Ok(internal_failure(
                format!("canonical codegen domain projection failed: {error:?}"),
                materialized.body_span,
            ));
        }
    };
    let mut implicit_destructor_targets = output
        .implicit_named_destructors
        .iter()
        .filter_map(|id| {
            materialized
                .aggregate_types
                .get(&rue_air::Type::new_struct(*id))
                .cloned()
        })
        .collect::<std::collections::BTreeSet<_>>();
    if let CfgSemanticInput::DropGlue { owner, facts, .. } = &key.semantic_input
        && facts.destructor.is_some()
    {
        // A synthesized struct glue body does not explicitly drop its owner,
        // but the exact DropGlue terminal records whether that owner has a
        // source destructor. Observe that local query fact instead of scanning
        // the live type pool for same-named structs outside this CFG's domain.
        implicit_destructor_targets.insert(owner.clone());
    }
    let (interner_payload_charge, interner_entries) =
        measure_interner_retained_charge(&materialized.interner);
    let interner_retained_charge = Arc::new(std::sync::atomic::AtomicU64::new(
        interner_header_retained_charge(materialized.interner.as_ref())
            .saturating_add(interner_payload_charge),
    ));
    let value = CfgValue::Available(Arc::new(CfgRecord {
        air: Arc::new(materialized.air),
        source_name: materialized.name.into(),
        num_locals: materialized.num_locals,
        num_param_slots: materialized.num_param_slots,
        cfg: output
            .cfg
            .expect("successful CFG construction publishes a validated CFG"),
        domains,
        type_pool: materialized.type_pool,
        interner: materialized.interner,
        interner_retained_charge,
        interner_retained_entries: interner_entries,
        strings: materialized.strings.into(),
        local_atoms: materialized.local_atoms.into(),
        codegen: Arc::new(codegen),
        materialization_warnings: materialized.warnings,
        body_span: materialized.body_span,
        warnings: output.warnings.into(),
        implicit_destructor_targets: implicit_destructor_targets
            .into_iter()
            .collect::<Vec<_>>()
            .into(),
        implicit_destructor_dependencies_complete: materialized.completeness.is_complete()
            && !output.anonymous_destructor_dependency_incomplete,
    }));
    let cfg_publication_ns = elapsed_ns(publication_started);
    tracing::event!(
        name: "cfg_construction_breakdown",
        target: "rue::timing",
        tracing::Level::INFO,
        input_preparation_ns = breakdown.input_preparation_ns,
        semantic_materialization_ns = breakdown.semantic_materialization_ns,
        domain_prerequisites_ns = breakdown.domain_prerequisites_ns,
        domain_projection_ns = breakdown.domain_projection_ns,
        prerequisite_collection_ns = breakdown.prerequisite_collection_ns,
        prerequisite_queries_ns = breakdown.prerequisite_queries_ns,
        cfg_builder_ns,
        cfg_publication_ns,
    );
    Ok(value)
}

pub(crate) fn evaluate_optimized_cfg(
    context: &QueryContext,
    cfgs: &QueryFamily<CfgQueryKey, CfgValue>,
    key: &OptimizedCfgQueryKey,
) -> Result<QueryOutput<CfgValue>, QueryAbort> {
    let _attempts = context.retain_nested_attempts_for(&["compiler.cfg"]);
    let terminal = context.query_registered(cfgs, key.cfg.clone())?;
    let QueryOutcome::Success(value) = terminal.outcome() else {
        unreachable!("Cfg publishes typed values")
    };
    let CfgValue::Available(record) = value else {
        return Ok(QueryOutput::success(value.clone())
            .with_terminal_kind(rue_query::QueryTerminalKind::Failure));
    };
    let _span = tracing::info_span!("cfg_optimization", phase = "cfg_and_optimization").entered();
    if key.accessor_dependencies.is_empty() {
        return Ok(optimize_cfg_without_accessors(context, key, record));
    }
    let mut current = record.cfg.clone();
    let mut domains = record.domains.clone();
    let interner = record.interner.clone();
    // Accessor import is the sole operation that can extend this shared
    // append-only symbol universe. Refresh on every exit, including partial
    // import and optimization failures, before the returned terminal publishes.
    let _interner_charge_refresh = InternerChargeRefresh {
        interner: interner.clone(),
        charge: record.interner_retained_charge.clone(),
        published_entries: record.interner_retained_entries,
    };
    let mut strings = record.strings.to_vec();
    let mut local_atoms = record.local_atoms.to_vec();
    let mut local_atom_identities = None;
    let mut symbol_mappings = record.codegen.symbol_mappings.as_ref().clone();
    let mut foreign_symbols = record.codegen.foreign_symbols.as_ref().clone();
    let mut materialization_warnings = record.materialization_warnings.to_vec();
    let mut warnings = record.warnings.to_vec();
    let mut implicit_destructor_targets = record
        .implicit_destructor_targets
        .iter()
        .cloned()
        .collect::<std::collections::BTreeSet<_>>();
    let mut implicit_destructor_dependencies_complete =
        record.implicit_destructor_dependencies_complete;
    let accessor_terminals =
        context.query_registered_adaptive_batch(cfgs, key.accessor_dependencies.iter().cloned())?;
    let mut accessor_cfgs = std::collections::BTreeMap::new();
    for (dependency, terminal) in key.accessor_dependencies.iter().zip(accessor_terminals) {
        let QueryOutcome::Success(value) = terminal.outcome() else {
            unreachable!("Cfg publishes typed values")
        };
        let CfgValue::Available(callee) = value else {
            return Ok(QueryOutput::success(value.clone())
                .with_terminal_kind(rue_query::QueryTerminalKind::Failure));
        };
        let body_span = match &dependency.semantic_input {
            CfgSemanticInput::Body { input, .. } => input.body_span,
            CfgSemanticInput::DropGlue { body_span, .. } => *body_span,
        };
        accessor_cfgs.insert(dependency.function.clone(), (callee.clone(), body_span));
    }
    let mut accessor_calls: std::collections::VecDeque<_> =
        attached_accessor_calls(&current, 0, 0).into();
    let mut splice_block_redirects = std::collections::HashMap::new();
    while let Some((call, call_block)) = accessor_calls.pop_front() {
        let call_block = resolve_splice_block(call_block, &mut splice_block_redirects);
        let rue_cfg::CfgInstData::AccessorCall { name, .. } = current.get_inst(call).data else {
            unreachable!()
        };
        let source_name = record.interner.resolve(&name);
        let Some(identity) = domains.callable_for_symbol(name) else {
            return Ok(QueryOutput::success(internal_failure(
                format!("accessor call '{source_name}' has no stable callable identity"),
                record.body_span,
            ))
            .with_terminal_kind(rue_query::QueryTerminalKind::Failure));
        };
        let Some((callee, callee_body_span)) = accessor_cfgs.get(&identity) else {
            return Ok(QueryOutput::success(internal_failure(
                format!("accessor CFG dependency is unavailable for '{source_name}'"),
                record.body_span,
            ))
            .with_terminal_kind(rue_query::QueryTerminalKind::Failure));
        };
        let (callee_cfg, string_map) = match domains.import_accessor_cfg(
            &callee.domains,
            &callee.cfg,
            &callee.interner,
            &interner,
            &mut strings,
            *callee_body_span,
        ) {
            Ok(value) => value,
            Err(error) => {
                return Ok(QueryOutput::success(internal_failure(
                    format!("accessor CFG domain import failed: {error:?}"),
                    record.body_span,
                ))
                .with_terminal_kind(rue_query::QueryTerminalKind::Failure));
            }
        };
        let callee_cfg = match callee_cfg.finish_after_optimization(&record.type_pool) {
            Ok(cfg) => cfg,
            Err(error) => {
                return Ok(QueryOutput::success(internal_failure(
                    format!("imported accessor CFG failed verification: {error}"),
                    record.body_span,
                ))
                .with_terminal_kind(rue_query::QueryTerminalKind::Failure));
            }
        };
        for atom in callee.local_atoms.iter() {
            let mut atom = atom.clone();
            let Some(dense_id) = string_map.get(&atom.dense_id).copied() else {
                return Ok(QueryOutput::success(internal_failure(
                    "accessor local atom has no imported string id",
                    record.body_span,
                ))
                .with_terminal_kind(rue_query::QueryTerminalKind::Failure));
            };
            atom.dense_id = dense_id;
            let local_atom_identities = local_atom_identities.get_or_insert_with(|| {
                local_atoms
                    .iter()
                    .map(|atom| atom.identity.clone())
                    .collect::<std::collections::HashSet<_>>()
            });
            if local_atom_identities.insert(atom.identity.clone()) {
                local_atoms.push(atom);
            }
        }
        symbol_mappings.extend(
            callee
                .codegen
                .symbol_mappings
                .iter()
                .map(|(k, v)| (k.clone(), v.clone())),
        );
        foreign_symbols.extend(callee.codegen.foreign_symbols.iter().cloned());
        for warning in import_warnings(
            &callee.materialization_warnings,
            callee.body_span,
            *callee_body_span,
        ) {
            if !materialization_warnings.contains(&warning) {
                materialization_warnings.push(warning);
            }
        }
        for warning in import_warnings(&callee.warnings, callee.body_span, *callee_body_span) {
            if !warnings.contains(&warning) {
                warnings.push(warning);
            }
        }
        implicit_destructor_targets.extend(callee.implicit_destructor_targets.iter().cloned());
        implicit_destructor_dependencies_complete &=
            callee.implicit_destructor_dependencies_complete;
        let callee_value_base = current.value_count() as u32;
        let continuation = rue_cfg::BlockId::from_raw(current.block_count() as u32);
        let callee_block_base = continuation.as_u32() + 1;
        let introduced_calls =
            attached_accessor_calls(&callee_cfg, callee_value_base, callee_block_base);
        current = match rue_cfg::inline_call_in_block(
            &current,
            call,
            call_block,
            &callee_cfg,
            &record.type_pool,
        ) {
            Ok(cfg) => cfg,
            Err(error) => {
                return Ok(QueryOutput::success(internal_failure(
                    format!("mandatory accessor CFG splice failed: {error}"),
                    record.body_span,
                ))
                .with_terminal_kind(rue_query::QueryTerminalKind::Failure));
            }
        };
        splice_block_redirects.insert(call_block, continuation);
        accessor_calls.extend(introduced_calls);
        context.record_work(rue_query::WorkItem::new("cfg.accessor-splices", 1));
    }
    let interner_retained_entries = interner.len() as u64;
    Ok(finish_cfg_optimization(
        context,
        key,
        current,
        &record.type_pool,
        record.body_span,
        move |cfg| CfgRecord {
            air: record.air.clone(),
            source_name: record.source_name.clone(),
            num_locals: record.num_locals,
            num_param_slots: record.num_param_slots,
            cfg,
            domains,
            type_pool: record.type_pool.clone(),
            interner,
            interner_retained_charge: record.interner_retained_charge.clone(),
            interner_retained_entries,
            strings: strings.into(),
            local_atoms: local_atoms.into(),
            codegen: Arc::new(CfgCodegenDomain {
                defined_symbol: record.codegen.defined_symbol.clone(),
                symbol_mappings: Arc::new(symbol_mappings),
                foreign_symbols: Arc::new(foreign_symbols),
            }),
            materialization_warnings: materialization_warnings.into(),
            body_span: record.body_span,
            warnings: warnings.into(),
            implicit_destructor_targets: implicit_destructor_targets
                .into_iter()
                .collect::<Vec<_>>()
                .into(),
            implicit_destructor_dependencies_complete,
        },
    ))
}

/// Optimize an ordinary CFG without materializing mutable accessor-import
/// collections that cannot change when the dependency list is empty.
fn optimize_cfg_without_accessors(
    context: &QueryContext,
    key: &OptimizedCfgQueryKey,
    record: &Arc<CfgRecord>,
) -> QueryOutput<CfgValue> {
    assert!(
        key.accessor_dependencies.is_empty(),
        "ordinary CFG optimization cannot receive accessor dependencies"
    );
    finish_cfg_optimization(
        context,
        key,
        record.cfg.clone(),
        &record.type_pool,
        record.body_span,
        |cfg| CfgRecord {
            air: record.air.clone(),
            source_name: record.source_name.clone(),
            num_locals: record.num_locals,
            num_param_slots: record.num_param_slots,
            cfg,
            domains: record.domains.clone(),
            type_pool: record.type_pool.clone(),
            interner: record.interner.clone(),
            interner_retained_charge: record.interner_retained_charge.clone(),
            interner_retained_entries: record.interner_retained_entries,
            strings: record.strings.clone(),
            local_atoms: record.local_atoms.clone(),
            codegen: record.codegen.clone(),
            materialization_warnings: record.materialization_warnings.clone(),
            body_span: record.body_span,
            warnings: record.warnings.clone(),
            implicit_destructor_targets: record.implicit_destructor_targets.clone(),
            implicit_destructor_dependencies_complete: record
                .implicit_destructor_dependencies_complete,
        },
    )
}

fn finish_cfg_optimization(
    context: &QueryContext,
    key: &OptimizedCfgQueryKey,
    cfg: rue_cfg::ValidatedCfg,
    type_pool: &rue_air::FrozenTypeInternPool,
    body_span: Span,
    build_record: impl FnOnce(rue_cfg::ValidatedCfg) -> CfgRecord,
) -> QueryOutput<CfgValue> {
    context.record_work(rue_query::WorkItem::new("cfg.optimize.attempts", 1));
    context.record_work(rue_query::WorkItem::new(
        "cfg.optimize.nonzero-level",
        u64::from(key.opt_level != rue_cfg::OptLevel::O0),
    ));
    match rue_cfg::opt::optimize(cfg, key.opt_level, type_pool) {
        Ok(cfg) => {
            context.record_work(rue_query::WorkItem::new("cfg.optimize.successes", 1));
            QueryOutput::success(CfgValue::Available(Arc::new(build_record(cfg))))
        }
        Err(error) => {
            context.record_work(rue_query::WorkItem::new("cfg.optimize.failures", 1));
            QueryOutput::success(CfgValue::Failure {
                errors: crate::CompileErrors::from(crate::CompileError::without_span(
                    error.error_kind("CFG optimization failed"),
                )),
                body_span,
            })
            .with_terminal_kind(rue_query::QueryTerminalKind::Failure)
        }
    }
}

fn attached_accessor_calls(
    cfg: &rue_cfg::ValidatedCfg,
    value_base: u32,
    block_base: u32,
) -> Vec<(rue_cfg::CfgValue, rue_cfg::BlockId)> {
    cfg.blocks()
        .iter()
        .flat_map(|block| {
            block
                .insts
                .iter()
                .copied()
                .filter(|&value| {
                    matches!(
                        cfg.get_inst(value).data,
                        rue_cfg::CfgInstData::AccessorCall { .. }
                    )
                })
                .map(|value| {
                    (
                        rue_cfg::CfgValue::from_raw(value_base + value.as_u32()),
                        rue_cfg::BlockId::from_raw(block_base + block.id.as_u32()),
                    )
                })
        })
        .collect()
}

fn resolve_splice_block(
    original: rue_cfg::BlockId,
    redirects: &mut std::collections::HashMap<rue_cfg::BlockId, rue_cfg::BlockId>,
) -> rue_cfg::BlockId {
    let mut current = original;
    while let Some(next) = redirects.get(&current).copied() {
        current = next;
    }
    if current != original {
        redirects.insert(original, current);
    }
    current
}

#[cfg(test)]
mod accessor_graph_tests {
    use super::*;

    #[test]
    fn retained_interner_charge_refreshes_once_after_append() {
        use std::sync::atomic::{AtomicU64, Ordering};

        let interner = lasso::ThreadedRodeo::new();
        interner.get_or_intern("caller");
        let (payload, published_entries) = measure_interner_retained_charge(&interner);
        let charge =
            AtomicU64::new(interner_header_retained_charge(&interner).saturating_add(payload));

        interner.get_or_intern("callee-only");
        assert!(refresh_interner_retained_charge(
            &interner,
            &charge,
            published_entries
        ));
        let (payload, current_entries) = measure_interner_retained_charge(&interner);
        assert_eq!(
            charge.load(Ordering::Relaxed),
            interner_header_retained_charge(&interner).saturating_add(payload)
        );
        assert!(!refresh_interner_retained_charge(
            &interner,
            &charge,
            current_entries
        ));
    }

    #[test]
    fn interner_utf8_bytes_tracks_unique_dynamic_static_and_concurrent_strings() {
        let interner = Arc::new(lasso::ThreadedRodeo::<lasso::Spur>::new());
        interner.get_or_intern(String::from("dynamic"));
        interner.get_or_intern("dynamic");
        interner.get_or_intern_static("static");
        interner.get_or_intern("");
        interner.get_or_intern_static("");

        std::thread::scope(|scope| {
            for index in 0..8 {
                let interner = interner.clone();
                scope.spawn(move || {
                    interner.get_or_intern("shared");
                    interner.get_or_intern(format!("worker-{index}"));
                });
            }
        });

        let expected = "dynamic".len()
            + "static".len()
            + "shared".len()
            + (0..8)
                .map(|index| format!("worker-{index}").len())
                .sum::<usize>();
        assert_eq!(interner.len(), 12);
        assert_eq!(interner.utf8_bytes(), expected);
        assert_eq!(
            interner.strings().map(str::len).sum::<usize>(),
            interner.utf8_bytes()
        );
    }

    fn chain(size: usize) -> std::collections::BTreeMap<usize, Vec<usize>> {
        (0..size)
            .map(|node| {
                let callees = (node + 1 < size).then_some(node + 1).into_iter().collect();
                (node, callees)
            })
            .collect()
    }

    #[test]
    fn accessor_chain_work_is_linear_in_edges() {
        for size in [32, 64, 128] {
            let direct = chain(size);
            let mut work = AccessorGraphWork::default();
            validate_accessor_dag(&direct, |node| direct.contains_key(node), &mut work).unwrap();
            let output = accessor_postorder(&0, &direct, &mut work);

            assert_eq!(work.validation_edges, size - 1);
            assert_eq!(work.closure_edges, size - 1);
            assert_eq!(output, (1..size).rev().collect::<Vec<_>>());
        }
    }

    #[test]
    fn accessor_fanout_work_is_linear_in_edges() {
        for width in [32, 64, 128] {
            let mut direct = std::collections::BTreeMap::new();
            direct.insert(0, (1..=width).collect());
            direct.extend((1..=width).map(|node| (node, Vec::new())));

            let mut work = AccessorGraphWork::default();
            validate_accessor_dag(&direct, |node| direct.contains_key(node), &mut work).unwrap();
            let output = accessor_postorder(&0, &direct, &mut work);

            assert_eq!(work.validation_edges, width);
            assert_eq!(work.closure_edges, width);
            assert_eq!(output, (1..=width).collect::<Vec<_>>());
        }
    }

    #[test]
    fn accessor_dag_validation_preserves_missing_and_cycle_failures() {
        let mut missing = std::collections::BTreeMap::from([(0, vec![1])]);
        let mut work = AccessorGraphWork::default();
        assert!(matches!(
            validate_accessor_dag(&missing, |node| missing.contains_key(node), &mut work),
            Err(AccessorDagFailure::Missing(1))
        ));

        missing.insert(1, vec![0]);
        let mut work = AccessorGraphWork::default();
        assert!(matches!(
            validate_accessor_dag(&missing, |node| missing.contains_key(node), &mut work),
            Err(AccessorDagFailure::Cycle(0))
        ));
    }

    #[test]
    fn accessor_splicing_discovers_attached_calls_once() {
        let source = include_str!("cfg_query.rs");
        let evaluator = source
            .split_once("pub(crate) fn evaluate_optimized_cfg(")
            .unwrap()
            .1
            .split_once("fn attached_accessor_calls(")
            .unwrap()
            .0;
        assert_eq!(
            evaluator
                .matches("attached_accessor_calls(&current, 0, 0)")
                .count(),
            1
        );
        assert!(evaluator.contains("accessor_calls.pop_front()"));
        assert!(evaluator.contains("accessor_calls.extend(introduced_calls)"));
        assert!(evaluator.contains("resolve_splice_block("));
        assert!(evaluator.contains("inline_call_in_block("));
        assert!(!evaluator.contains("rue_cfg::inline_call(&current"));
        assert!(!evaluator.contains("loop {\n        let call = current.blocks()"));
        assert!(evaluator.contains("let mut local_atom_identities = None"));
        assert!(evaluator.contains("local_atom_identities.get_or_insert_with("));
        assert!(evaluator.contains("local_atom_identities.insert("));
        assert!(!evaluator.contains("local_atoms.iter().any("));

        let domains = include_str!("durable_cfg.rs");
        let lookup = domains
            .split_once("pub(crate) fn callable_for_symbol(")
            .unwrap()
            .1
            .split_once("pub(crate) fn same_live_domain")
            .unwrap()
            .0;
        assert!(lookup.contains(".partition_point("));
        assert!(!lookup.contains(".iter().find"));
    }
}
