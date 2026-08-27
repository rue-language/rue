//! Canonical per-function CFG query values.
//!
//! The unoptimized family owns AIR-to-CFG lowering. The optimized family owns
//! only the selected optimization pipeline and observes the exact unoptimized
//! terminal. Both publish stable relocation domains and own the body-local AIR,
//! type pool, symbols, strings, and local atoms required by their CFG.

use rue_air::Node;
use std::hash::{Hash, Hasher};
use std::num::NonZeroUsize;
use std::sync::{Arc, OnceLock};

use ahash::{AHashMap, AHashSet};
use lasso::Key as _;
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
    #[cfg(test)]
    pub(crate) interner_limit: Option<usize>,
    #[cfg(test)]
    pub(crate) force_failure: bool,
}

impl PartialEq for CfgBodyInput {
    fn eq(&self, other: &Self) -> bool {
        self.function == other.function && self.canonical == other.canonical && {
            #[cfg(test)]
            {
                self.interner_limit == other.interner_limit
                    && self.force_failure == other.force_failure
            }
            #[cfg(not(test))]
            {
                true
            }
        }
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
    /// Formatted only when something asks what this node is called
    /// (ADR-0074); ordinary compilation never reads it.
    display_identity: OnceLock<Arc<str>>,
}

impl CfgQueryKey {
    pub(crate) fn new(
        function: crate::FunctionInstanceKey,
        configuration: crate::semantic_query_nucleus::SemanticQueryConfiguration,
        semantic_input: CfgSemanticInput,
    ) -> Self {
        #[cfg(test)]
        CFG_QUERY_KEY_CONSTRUCTIONS.with(|count| count.set(count.get() + 1));

        let mut hasher = rue_query::StableHasher::new();
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
        Self {
            function,
            configuration,
            semantic_input,
            memo_hash,
            display_identity: OnceLock::new(),
        }
    }

    fn format_identity(&self) -> String {
        let function = &self.function;
        format!(
            "{function:?};target={:?};preview={:?}",
            self.configuration.target, self.configuration.preview_features
        )
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
        self.format_identity()
    }

    fn shared_stable_identity(&self) -> Arc<str> {
        self.display_identity
            .get_or_init(|| self.format_identity().into())
            .clone()
    }

    /// Function and configuration, which is exactly what `memo_hash` and the
    /// display identity above already summarize. The retained semantic
    /// versions of one function deliberately share a digest, as they already
    /// share a memo bucket and a rendered name; typed `Eq` separates them.
    fn stable_hash(&self, hasher: &mut rue_query::StableHasher) {
        self.function.hash(hasher);
        self.configuration.hash(hasher);
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
    /// Lazy for the same reason as [`CfgQueryKey`]: building it forced the
    /// inner CFG key's identity too, so every optimized-CFG key formatted a
    /// recursive function identity that normal compilation never reads.
    display_identity: OnceLock<Arc<str>>,
}

impl OptimizedCfgQueryKey {
    pub(crate) fn new(
        cfg: CfgQueryKey,
        opt_level: rue_cfg::OptLevel,
        accessor_dependencies: Arc<[CfgQueryKey]>,
    ) -> Self {
        Self {
            cfg,
            opt_level,
            accessor_dependencies,
            display_identity: OnceLock::new(),
        }
    }

    fn format_identity(&self) -> String {
        let cfg_identity = self.cfg.shared_stable_identity();
        let opt_level = self.opt_level;
        format!(
            "{cfg_identity};opt={opt_level:?};accessors={}",
            self.accessor_dependencies.len()
        )
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
        self.format_identity()
    }

    fn shared_stable_identity(&self) -> Arc<str> {
        self.display_identity
            .get_or_init(|| self.format_identity().into())
            .clone()
    }

    fn stable_hash(&self, hasher: &mut rue_query::StableHasher) {
        self.cfg.stable_hash(hasher);
        std::mem::discriminant(&self.opt_level).hash(hasher);
        hasher.write_usize(self.accessor_dependencies.len());
        for dependency in self.accessor_dependencies.iter() {
            dependency.stable_hash(hasher);
        }
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
    /// O3 growth already spent while producing this per-function CFG. The
    /// whole-program batch carries this charge into general inlining.
    pub(crate) code_growth_used: u64,
    pub(crate) code_growth_blocks_used: u64,
    pub(crate) domains: crate::durable_cfg::CfgDomainProjection,
    pub(crate) type_pool: rue_air::FrozenTypeInternPool,
    pub(crate) interner: Arc<lasso::ThreadedRodeo>,
    /// Logical retained charge frozen with the immutable published interner.
    interner_retained_charge: u64,
    pub(crate) strings: Arc<[String]>,
    pub(crate) local_atoms:
        Arc<[rue_air::LocalAtomRecord<crate::StableDefinitionKey, crate::ModuleId>]>,
    /// Constant-time cardinalities from the original local import. Optimized
    /// projections preserve them for performance reporting; they are never
    /// semantic input or query authority.
    pub(crate) local_aggregate_type_aliases: usize,
    pub(crate) local_materialized_type_handles: usize,
    /// Owned current-domain aliases available while lowering this CFG. The
    /// domain includes cleanup aliases that optimization may leave unused; its
    /// stable identities and ABI classifications are still exact CFG-query
    /// dependencies, never a caller-owned program resolver.
    pub(crate) codegen: Arc<CfgCodegenDomain>,
    pub(crate) materialization_warnings: Arc<[rue_error::CompileWarning]>,
    pub(crate) body_span: Span,
    pub(crate) warnings: Arc<[rue_error::CompileWarning]>,
    pub(crate) implicit_destructor_targets: Arc<[crate::TypeInstanceKey]>,
    pub(crate) implicit_drop_glue_targets: Arc<[crate::TypeInstanceKey]>,
    pub(crate) implicit_destructor_dependencies_complete: bool,
    /// General interprocedural inlining changes the caller's CFG in a
    /// whole-program batch. Such a record is a backend result, not a durable
    /// function-local CFG and must not be admitted to the durable retention
    /// cone (ADR-0049 Phase 2).
    pub(crate) durable_reuse_allowed: bool,
}

#[cfg(test)]
impl CfgRecord {
    pub(crate) fn frozen_interner_retained_charge_for_test(&self) -> u64 {
        self.interner_retained_charge
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CfgCodegenDomain {
    pub(crate) defined_symbol: Arc<str>,
    pub(crate) symbol_mappings: Arc<std::collections::BTreeMap<String, String>>,
    pub(crate) foreign_symbols: Arc<std::collections::BTreeSet<String>>,
}

impl CfgCodegenDomain {
    /// Classify a source spelling using the exact symbol projection already
    /// consumed by codegen. A target-C callable is a legitimate external edge
    /// even when its callable identity has no body in this batch.
    pub(crate) fn is_foreign_source_symbol(&self, source: &str) -> bool {
        self.symbol_mappings
            .get(source)
            .is_some_and(|machine| self.foreign_symbols.contains(machine))
    }
}

#[derive(Debug, Clone)]
pub(crate) enum CfgValue {
    Available(Arc<CfgRecord>),
    Failure {
        errors: crate::CompileErrors,
        body_span: Span,
    },
    /// A caller's optimized CFG observed a raw accessor failure. `origin`
    /// keeps the callee basis needed to re-anchor retained diagnostics on
    /// reuse; the value itself is published under the caller's optimized key.
    AccessorFailure {
        errors: crate::CompileErrors,
        origin: CfgFailureOrigin,
    },
}

#[derive(Debug, Clone)]
pub(crate) struct CfgFailureOrigin {
    pub(crate) accessor: crate::FunctionInstanceKey,
    pub(crate) body_span: Span,
}

impl RetainedCharge for lasso::ThreadedRodeo {
    fn retained_charge(&self) -> u64 {
        measure_interner_retained_charge(self)
    }
}

fn measure_interner_retained_charge(interner: &lasso::ThreadedRodeo) -> u64 {
    let entries = interner.len() as u64;
    let utf8_bytes = interner.utf8_bytes() as u64;
    entries
        .saturating_mul(std::mem::size_of::<lasso::Spur>() as u64)
        .saturating_add(utf8_bytes)
}

fn interner_header_retained_charge(interner: &lasso::ThreadedRodeo) -> u64 {
    // `utf8_bytes` is measurement-only bookkeeping added to the vendored
    // interner. Exclude that one atomic from the logical artifact charge so
    // replacing the traversal does not change retention policy.
    (std::mem::size_of_val(interner) as u64)
        .saturating_sub(std::mem::size_of::<std::sync::atomic::AtomicUsize>() as u64)
}

fn frozen_interner_retained_charge(interner: &lasso::ThreadedRodeo) -> u64 {
    interner_header_retained_charge(interner)
        .saturating_add(measure_interner_retained_charge(interner))
}

#[derive(Debug)]
enum CfgInternerCopyFailure<E> {
    Checkpoint(E),
    InvalidSourceOrdinal(usize),
    Capacity(lasso::LassoErrorKind),
    OrdinalMismatch {
        expected: lasso::Spur,
        actual: lasso::Spur,
    },
    SourceChanged,
}

impl<E: std::fmt::Debug> std::fmt::Display for CfgInternerCopyFailure<E> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Checkpoint(error) => write!(formatter, "checkpoint failed: {error:?}"),
            Self::InvalidSourceOrdinal(ordinal) => {
                write!(formatter, "source has no symbol at ordinal {ordinal}")
            }
            Self::Capacity(kind) => write!(formatter, "interner allocation failed: {kind}"),
            Self::OrdinalMismatch { expected, actual } => write!(
                formatter,
                "copied symbol ordinal changed from {expected:?} to {actual:?}"
            ),
            Self::SourceChanged => formatter.write_str("source changed while it was copied"),
        }
    }
}

/// Copy one published CFG symbol universe into a private accessor-import
/// universe without changing any existing `Spur`. The source is observed at
/// both ends so an unexpected external mutation fails closed instead of
/// producing a mixed-generation copy.
fn copy_interner_preserving_ordinals<E>(
    source: &lasso::ThreadedRodeo,
    mut checkpoint: impl FnMut() -> Result<(), E>,
) -> Result<lasso::ThreadedRodeo, CfgInternerCopyFailure<E>> {
    let expected_entries = source.len();
    let expected_utf8_bytes = source.utf8_bytes();
    let capacity = lasso::Capacity::new(
        expected_entries,
        NonZeroUsize::new(expected_utf8_bytes).unwrap_or(NonZeroUsize::MIN),
    );
    let copy = lasso::ThreadedRodeo::with_capacity(capacity);

    for ordinal in 0..expected_entries {
        if ordinal % 64 == 0 {
            checkpoint().map_err(CfgInternerCopyFailure::Checkpoint)?;
        }
        let expected = lasso::Spur::try_from_usize(ordinal)
            .ok_or(CfgInternerCopyFailure::InvalidSourceOrdinal(ordinal))?;
        let spelling = source
            .try_resolve(&expected)
            .ok_or(CfgInternerCopyFailure::InvalidSourceOrdinal(ordinal))?;
        let actual = copy
            .try_get_or_intern(spelling)
            .map_err(|error| CfgInternerCopyFailure::Capacity(error.kind()))?;
        if actual != expected {
            return Err(CfgInternerCopyFailure::OrdinalMismatch { expected, actual });
        }
    }
    checkpoint().map_err(CfgInternerCopyFailure::Checkpoint)?;
    if source.len() != expected_entries || source.utf8_bytes() != expected_utf8_bytes {
        return Err(CfgInternerCopyFailure::SourceChanged);
    }
    Ok(copy)
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
            .saturating_add(self.interner_retained_charge)
            .saturating_add(self.strings.retained_charge())
            .saturating_add(self.local_atoms.retained_charge())
            .saturating_add(self.codegen.retained_charge())
            .saturating_add(self.materialization_warnings.retained_charge())
            .saturating_add(self.warnings.retained_charge())
            .saturating_add(self.implicit_destructor_targets.retained_charge())
            .saturating_add(self.implicit_drop_glue_targets.retained_charge())
    }
}

impl RetainedCharge for CfgValue {
    fn retained_charge(&self) -> u64 {
        match self {
            Self::Available(record) => record.retained_charge(),
            Self::Failure { errors, .. } => errors.retained_charge(),
            Self::AccessorFailure { errors, origin } => errors
                .retained_charge()
                .saturating_add(origin.accessor.retained_charge()),
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
        (
            CfgValue::AccessorFailure {
                errors: left_errors,
                origin: left_origin,
                ..
            },
            CfgValue::AccessorFailure {
                errors: right_errors,
                origin: right_origin,
                ..
            },
        ) => {
            // Position-only edits keep this terminal reusable; consumers
            // reproject the retained basis through `origin.accessor`.
            left_errors == right_errors && left_origin.accessor == right_origin.accessor
        }
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

pub(crate) fn import_accessor_failure(
    errors: &crate::CompileErrors,
    origin: &CfgFailureOrigin,
    key: &OptimizedCfgQueryKey,
) -> crate::CompileErrors {
    let current_accessor_span = key
        .accessor_dependencies
        .iter()
        .find(|dependency| dependency.function == origin.accessor)
        .map(|dependency| match &dependency.semantic_input {
            CfgSemanticInput::Body { input, .. } => input.body_span,
            CfgSemanticInput::DropGlue { body_span, .. } => *body_span,
        })
        .expect("published accessor failure must name a dependency");
    import_errors(errors, origin.body_span, current_accessor_span)
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
    output: &mut Vec<crate::TypeInstanceKey>,
) {
    output.push(type_instance_from_semantic(ty));
    // Array representation and CFG indexing depend on the element layout.
    // Pointer and slice representation does not depend on the pointee.
    if let rue_air::SemanticImportType::Array { element, .. } = ty {
        collect_type_dependencies(element, output);
    }
}

pub(crate) fn collect_type_and_drop_dependencies(
    ty: &rue_air::SemanticImportType<crate::StableDefinitionKey, crate::ModuleId>,
    layout_output: &mut Vec<crate::TypeInstanceKey>,
    drop_output: &mut Vec<crate::TypeInstanceKey>,
) {
    let instance = type_instance_from_semantic(ty);
    if type_may_need_drop_glue(ty) {
        drop_output.push(instance.clone());
    }
    layout_output.push(instance);
    // Array representation and CFG indexing depend on the element layout.
    // Pointer and slice representation does not depend on the pointee.
    if let rue_air::SemanticImportType::Array { element, .. } = ty {
        collect_type_dependencies(element, layout_output);
    }
}

/// Return whether a semantic type can have a drop plan of its own.
///
/// Scalars, pointers, slices, and zero-length arrays are unconditionally
/// dropless. Keeping them out of the CFG's drop-glue prerequisite family avoids
/// asking the ownership query for a terminal that can only publish
/// `DropGluePlan::None`; nominal types remain in the set because their fields or
/// destructor declarations may make them droppable, and arrays recurse so an
/// array of a droppable element keeps its own glue terminal.
fn type_may_need_drop_glue<K, M>(ty: &rue_air::SemanticImportType<K, M>) -> bool {
    use rue_air::SemanticImportType as T;

    match ty {
        T::Array { element, len } => *len != 0 && type_may_need_drop_glue(element),
        T::BuiltinNominal { .. } | T::Nominal(_) | T::AnonymousNominal(_) => true,
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
        | T::PtrConst(_)
        | T::PtrMut(_)
        | T::Slice { .. }
        | T::Module(_)
        | T::GenericParameter(_) => false,
    }
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
        T::AnonymousNominal(identity) => crate::TypeInstanceKey::Nominal(
            crate::NominalInstanceKey::Anonymous(Node::new(identity.clone())),
        ),
        T::Array { element, len } => crate::TypeInstanceKey::Array {
            element: Node::new(type_instance_from_semantic(element)),
            len: *len,
        },
        T::Slice { element, name } => crate::TypeInstanceKey::Slice {
            element: Node::new(type_instance_from_semantic(element)),
            name: name.clone(),
        },
        T::PtrConst(element) => {
            crate::TypeInstanceKey::PtrConst(Node::new(type_instance_from_semantic(element)))
        }
        T::PtrMut(element) => {
            crate::TypeInstanceKey::PtrMut(Node::new(type_instance_from_semantic(element)))
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
    #[cfg(test)]
    if let CfgSemanticInput::Body { input, .. } = &key.semantic_input {
        if input.force_failure {
            return Ok(QueryOutput::success(CfgValue::Failure {
                errors: crate::CompileError::new(
                    rue_error::ErrorKind::InternalError("test CFG accessor failure".to_owned()),
                    input.body_span,
                )
                .into(),
                body_span: input.body_span,
            })
            .with_terminal_kind(rue_query::QueryTerminalKind::Failure));
        }
    }
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

fn interner_copy_capacity_failure(kind: lasso::LassoErrorKind, body_span: Span) -> CfgValue {
    let message = format!("CFG interner isolation failed: {kind}");
    let kind = rue_lexer::interner_error_kind(kind, message);
    CfgValue::Failure {
        errors: crate::CompileError::new(kind, body_span).into(),
        body_span,
    }
}

fn interner_resource_failure(
    kind: lasso::LassoErrorKind,
    context: impl std::fmt::Display,
    body_span: Span,
) -> CfgValue {
    let message = format!("{context}: {kind}");
    let kind = rue_lexer::interner_error_kind(kind, message);
    CfgValue::Failure {
        errors: crate::CompileError::new(kind, body_span).into(),
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
            // Probed by type inside the drop-glue synthesizer and never
            // iterated, so this is a bucket selector, not an ordered map.
            let mut slots = ahash::AHashMap::new();
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
    #[cfg(test)]
    let local_interner_limit = match &key.semantic_input {
        CfgSemanticInput::Body { input, .. } => input.interner_limit,
        CfgSemanticInput::DropGlue { .. } => None,
    };
    // The local semantic epoch owns the actual insertion path. Tests inject
    // their request-local ceiling into that owner so a regression to an
    // infallible insertion cannot pass by merely checking the final length.
    let materialization_symbol_space = {
        #[cfg(test)]
        {
            local_interner_limit
                .map(rue_rir::SharedSymbolSpace::with_owner_bound)
                .unwrap_or_else(rue_rir::SharedSymbolSpace::private)
        }
        #[cfg(not(test))]
        {
            rue_rir::SharedSymbolSpace::private()
        }
    };
    // Both CFG inputs use the exact fact-side indexes prepared during
    // selection, keeping canonical-body and drop-glue materialization on one
    // indexed path.
    let materialized = match &key.semantic_input {
        CfgSemanticInput::Body { input, .. } => {
            crate::local_semantic_materialization::materialize_canonical_body_with_indexes_in_space(
                &input.canonical,
                body_span,
                &facts.declarations,
                &facts.anonymous_nominals,
                &facts.callables,
                &facts.nominal_metadata,
                &facts.modules,
                &builtin_facts,
                &facts.required_types,
                &facts.indexes,
                materialization_symbol_space,
            )
        }
        CfgSemanticInput::DropGlue { owner, .. } => {
            crate::local_semantic_materialization::materialize_semantic_body_with_indexes_in_space(
                crate::FunctionInstanceKey::DropGlue(Node::new(owner.clone())),
                body,
                body_span,
                &facts.declarations,
                &facts.anonymous_nominals,
                &facts.callables,
                &facts.nominal_metadata,
                &facts.modules,
                &builtin_facts,
                &facts.required_types,
                &facts.indexes,
                materialization_symbol_space,
            )
        }
    };
    let semantic_materialization_ns = elapsed_ns(materialization_started);
    let domain_prerequisites_started = std::time::Instant::now();
    let materialized = match materialized {
        Ok(value) => value,
        Err(error) => {
            context.record_work(rue_query::WorkItem::new("cfg.materialize.failures", 1));
            if let crate::local_semantic_materialization::LocalMaterializationFailure::Import(
                rue_air::SemanticImportFailure::Interner(kind),
            ) = &error
            {
                return Ok(interner_resource_failure(
                    *kind,
                    "request-local CFG symbol domain",
                    body_span,
                ));
            }
            if let crate::local_semantic_materialization::LocalMaterializationFailure::Body(
                rue_air::SemanticBodyImportFailure::Semantic(
                    rue_air::SemanticImportFailure::Interner(kind),
                ),
            ) = &error
            {
                return Ok(interner_resource_failure(
                    *kind,
                    "request-local CFG symbol domain",
                    body_span,
                ));
            }
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
    let mut callable_by_symbol = AHashMap::with_capacity(facts.callables.len());
    for fact in facts.callables.iter() {
        match callable_by_symbol.entry(fact.symbol.as_ref()) {
            std::collections::hash_map::Entry::Vacant(entry) => {
                entry.insert(&fact.identity);
            }
            std::collections::hash_map::Entry::Occupied(entry)
                if *entry.get() != &fact.identity =>
            {
                return Ok(internal_failure(
                    "canonical CFG callable facts contain conflicting symbols".to_string(),
                    body_span,
                ));
            }
            std::collections::hash_map::Entry::Occupied(_) => {}
        }
    }
    let domains =
        match crate::durable_cfg::CfgDomainProjection::from_local_body(&materialized, |symbol| {
            let name = materialized.interner.resolve(&symbol);
            callable_by_symbol
                .get(name)
                .map(|identity| (*identity).clone())
        }) {
            Ok(value) => value,
            Err(error) => {
                if let crate::durable_cfg::CfgDomainFailure::Interner(kind) = error {
                    return Ok(interner_resource_failure(
                        kind,
                        "canonical CFG domain interner",
                        body_span,
                    ));
                }
                return Ok(internal_failure(
                    format!("canonical CFG domain projection failed: {error:?}"),
                    body_span,
                ));
            }
        };
    let domain_projection_ns = elapsed_ns(domain_projection_started);

    let prerequisite_collection_started = std::time::Instant::now();
    // The local type pool is already deduplicated, so per-type tree inserts
    // buy nothing here: on the maintained workloads every scanned type yields
    // exactly one unique layout key (the only collisions are array elements,
    // whose keys the pool also carries). Collect into vectors and normalize
    // once; the sort matches the ordered-set iteration this replaced, so the
    // batch request order is unchanged.
    let stable_type_count = domains.stable_type_count();
    let mut layout_dependencies: Vec<crate::TypeInstanceKey> =
        Vec::with_capacity(stable_type_count);
    let mut drop_dependencies: Vec<crate::TypeInstanceKey> = Vec::with_capacity(stable_type_count);
    let mut stable_types_scanned = 0_u64;
    for ty in domains.stable_types() {
        stable_types_scanned = stable_types_scanned.saturating_add(1);
        collect_type_and_drop_dependencies(ty, &mut layout_dependencies, &mut drop_dependencies);
    }
    layout_dependencies.sort_unstable();
    layout_dependencies.dedup();
    drop_dependencies.sort_unstable();
    drop_dependencies.dedup();
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
    let drop_glue_terminals =
        context.query_registered_adaptive_batch(drop_glues, drop_dependencies.clone())?;
    // Both maps are built here, threaded straight to the codegen-domain
    // projection, and only ever probed by key there. Nothing iterates them, so
    // their order is not observable and a `BTreeMap` only buys a recursive
    // `TypeInstanceKey` comparison at every level of every probe.
    let mut drop_glue_symbols = ahash::AHashMap::new();
    let mut destructor_symbols = ahash::AHashMap::new();
    for (query, terminal) in drop_dependencies.iter().zip(drop_glue_terminals) {
        let QueryOutcome::Success(crate::type_queries::DropGlueValue::Available(facts)) =
            terminal.outcome()
        else {
            continue;
        };
        if let Some(symbol) = &facts.machine_symbol {
            drop_glue_symbols.insert(query.ty.clone(), symbol.clone());
        }
        if let Some(symbol) = &facts.destructor_symbol {
            destructor_symbols.insert(query.ty.clone(), symbol.clone());
        }
    }
    let prerequisite_queries_ns = elapsed_ns(prerequisite_queries_started);
    let breakdown = CfgConstructionBreakdown {
        input_preparation_ns,
        semantic_materialization_ns,
        domain_prerequisites_ns: elapsed_ns(domain_prerequisites_started),
        domain_projection_ns,
        prerequisite_collection_ns,
        prerequisite_queries_ns,
    };
    build_cfg(
        context,
        call_abis,
        key,
        materialized,
        domains,
        drop_glue_symbols,
        destructor_symbols,
        breakdown,
    )
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
    drop_glue_symbols: ahash::AHashMap<crate::TypeInstanceKey, Arc<str>>,
    destructor_symbols: ahash::AHashMap<crate::TypeInstanceKey, Arc<str>>,
    breakdown: CfgConstructionBreakdown,
) -> Result<CfgValue, QueryAbort> {
    context.record_work(rue_query::WorkItem::new("cfg.build.attempts", 1));
    context.record_work(rue_query::WorkItem::new(
        "cfg.air.instructions",
        materialized.air.instructions().len() as u64,
    ));
    let builder_started = std::time::Instant::now();
    // Canonical AIR already owns every source symbol. CFG projection must
    // resolve that body-local domain without extending it; compiler-generated
    // spellings are admitted by the semantic provider before this boundary.
    let output = rue_cfg::CfgBuilder::build_with_symbol_resolver(
        &materialized.air,
        materialized.num_locals,
        materialized.num_param_slots,
        &materialized.name,
        &materialized.type_pool,
        materialized.param_modes.clone(),
        &materialized.interner,
        materialized.allow_unreachable_code,
        materialized.callable_kind,
        |name| materialized.interner.get(name),
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
    // Probed by callable identity inside the codegen-domain projection and
    // never iterated, so this is a bucket selector, not an ordered map.
    let mut call_abi_facts = ahash::AHashMap::new();
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
        &drop_glue_symbols,
        &destructor_symbols,
    ) {
        Ok(value) => value,
        Err(error) => {
            return Ok(internal_failure(
                format!("canonical codegen domain projection failed: {error:?}"),
                materialized.body_span,
            ));
        }
    };
    let implicit_destructor_dependencies_complete =
        output.implicit_named_destructors.iter().all(|id| {
            materialized
                .aggregate_types
                .contains_key(&rue_air::Type::new_struct(*id))
        });
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
    let implicit_drop_glue_dependencies_complete = output
        .implicit_drop_glue_types
        .iter()
        .all(|ty| materialized.aggregate_types.contains_key(ty));
    let implicit_drop_glue_targets = output
        .implicit_drop_glue_types
        .iter()
        .filter_map(|ty| materialized.aggregate_types.get(ty).cloned())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    if let CfgSemanticInput::DropGlue { owner, facts, .. } = &key.semantic_input
        && facts.destructor.is_some()
    {
        // A synthesized struct glue body does not explicitly drop its owner,
        // but the exact DropGlue terminal records whether that owner has a
        // source destructor. Observe that local query fact instead of scanning
        // the live type pool for same-named structs outside this CFG's domain.
        implicit_destructor_targets.insert(owner.clone());
    }
    let local_aggregate_type_aliases = materialized.aggregate_types.len();
    let local_materialized_type_handles = materialized.materialized_types.len();
    let interner_retained_charge = frozen_interner_retained_charge(&materialized.interner);
    let value = CfgValue::Available(Arc::new(CfgRecord {
        air: Arc::new(materialized.air),
        source_name: materialized.name.into(),
        num_locals: materialized.num_locals,
        num_param_slots: materialized.num_param_slots,
        cfg: output
            .cfg
            .expect("successful CFG construction publishes a validated CFG"),
        code_growth_used: 0,
        code_growth_blocks_used: 0,
        domains,
        type_pool: materialized.type_pool,
        interner: materialized.interner,
        interner_retained_charge,
        strings: materialized.strings.into(),
        local_atoms: materialized.local_atoms.into(),
        local_aggregate_type_aliases,
        local_materialized_type_handles,
        codegen: Arc::new(codegen),
        materialization_warnings: materialized.warnings,
        body_span: materialized.body_span,
        warnings: output.warnings.into(),
        implicit_destructor_targets: implicit_destructor_targets
            .into_iter()
            .collect::<Vec<_>>()
            .into(),
        implicit_drop_glue_targets: implicit_drop_glue_targets.into(),
        implicit_destructor_dependencies_complete: materialized.completeness.is_complete()
            && implicit_destructor_dependencies_complete
            && implicit_drop_glue_dependencies_complete
            && !output.anonymous_destructor_dependency_incomplete,
        durable_reuse_allowed: true,
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
    let mut implicit_drop_glue_targets = record
        .implicit_drop_glue_targets
        .iter()
        .cloned()
        .collect::<std::collections::BTreeSet<_>>();
    let mut implicit_destructor_dependencies_complete =
        record.implicit_destructor_dependencies_complete;
    let accessor_terminals =
        context.query_registered_adaptive_batch_refs(cfgs, key.accessor_dependencies.iter())?;
    let mut accessor_cfgs = std::collections::BTreeMap::new();
    for (dependency, terminal) in key.accessor_dependencies.iter().zip(accessor_terminals) {
        let QueryOutcome::Success(value) = terminal.outcome() else {
            unreachable!("Cfg publishes typed values")
        };
        let dependency_body_span = match &dependency.semantic_input {
            CfgSemanticInput::Body { input, .. } => input.body_span,
            CfgSemanticInput::DropGlue { body_span, .. } => *body_span,
        };
        let CfgValue::Available(callee) = value else {
            let CfgValue::Failure {
                errors,
                body_span: old_span,
            } = value
            else {
                unreachable!("Cfg publishes typed values")
            };
            return Ok(QueryOutput::success(CfgValue::AccessorFailure {
                errors: import_errors(errors, *old_span, dependency_body_span),
                origin: CfgFailureOrigin {
                    accessor: dependency.function.clone(),
                    body_span: dependency_body_span,
                },
            })
            .with_terminal_kind(rue_query::QueryTerminalKind::Failure));
        };
        accessor_cfgs.insert(
            dependency.function.clone(),
            (callee.clone(), dependency_body_span),
        );
    }
    let interner =
        match copy_interner_preserving_ordinals(&record.interner, || context.check_canceled()) {
            Ok(interner) => Arc::new(interner),
            Err(CfgInternerCopyFailure::Checkpoint(abort)) => return Err(abort),
            Err(CfgInternerCopyFailure::Capacity(kind)) => {
                return Ok(QueryOutput::success(interner_copy_capacity_failure(
                    kind,
                    record.body_span,
                ))
                .with_terminal_kind(rue_query::QueryTerminalKind::Failure));
            }
            Err(error) => {
                return Ok(QueryOutput::success(internal_failure(
                    format!("CFG interner isolation failed: {error}"),
                    record.body_span,
                ))
                .with_terminal_kind(rue_query::QueryTerminalKind::Failure));
            }
        };
    context.record_work(rue_query::WorkItem::new(
        "cfg.accessor-interner-copy-symbols",
        interner.len() as u64,
    ));
    let mut accessor_calls: std::collections::VecDeque<_> =
        attached_accessor_calls(&current, 0, 0).into();
    let mut splice_block_redirects = AHashMap::new();
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
                    .collect::<AHashSet<_>>()
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
        implicit_drop_glue_targets.extend(callee.implicit_drop_glue_targets.iter().cloned());
        implicit_destructor_dependencies_complete &=
            callee.implicit_destructor_dependencies_complete;
        let callee_value_base = current.value_count() as u32;
        // `inline_call_in_block` appends copied callee blocks before its
        // continuation so projected-place definitions dominate continuation
        // consumers during lazy backend materialization.
        let callee_block_base = current.block_count() as u32;
        let continuation =
            rue_cfg::BlockId::from_raw(callee_block_base + callee_cfg.block_count() as u32);
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
    let interner_retained_charge = frozen_interner_retained_charge(&interner);
    Ok(finish_cfg_optimization(
        context,
        key,
        current,
        &record.type_pool,
        record.body_span,
        move |cfg, code_growth_used, code_growth_blocks_used| CfgRecord {
            air: record.air.clone(),
            source_name: record.source_name.clone(),
            num_locals: record.num_locals,
            num_param_slots: record.num_param_slots,
            cfg,
            code_growth_used,
            code_growth_blocks_used,
            domains,
            type_pool: record.type_pool.clone(),
            interner,
            interner_retained_charge,
            strings: strings.into(),
            local_atoms: local_atoms.into(),
            local_aggregate_type_aliases: record.local_aggregate_type_aliases,
            local_materialized_type_handles: record.local_materialized_type_handles,
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
            implicit_drop_glue_targets: implicit_drop_glue_targets
                .into_iter()
                .collect::<Vec<_>>()
                .into(),
            implicit_destructor_dependencies_complete,
            durable_reuse_allowed: record.durable_reuse_allowed,
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
        |cfg, code_growth_used, code_growth_blocks_used| CfgRecord {
            air: record.air.clone(),
            source_name: record.source_name.clone(),
            num_locals: record.num_locals,
            num_param_slots: record.num_param_slots,
            cfg,
            code_growth_used,
            code_growth_blocks_used,
            domains: record.domains.clone(),
            type_pool: record.type_pool.clone(),
            interner: record.interner.clone(),
            interner_retained_charge: record.interner_retained_charge,
            strings: record.strings.clone(),
            local_atoms: record.local_atoms.clone(),
            local_aggregate_type_aliases: record.local_aggregate_type_aliases,
            local_materialized_type_handles: record.local_materialized_type_handles,
            codegen: record.codegen.clone(),
            materialization_warnings: record.materialization_warnings.clone(),
            body_span: record.body_span,
            warnings: record.warnings.clone(),
            implicit_destructor_targets: record.implicit_destructor_targets.clone(),
            implicit_drop_glue_targets: record.implicit_drop_glue_targets.clone(),
            implicit_destructor_dependencies_complete: record
                .implicit_destructor_dependencies_complete,
            durable_reuse_allowed: record.durable_reuse_allowed,
        },
    )
}

/// Apply ADR-0049 Phase 2 to one optimized-CFG batch. The batch is the only
/// place where the complete reached set is available, so call discovery is
/// deliberately performed over the actual CFG instruction arenas here. This
/// keeps the semantic dependency graph out of the inlining decision and gives
/// callers a single deterministic whole-set result for both backends.
pub(crate) fn apply_general_inlining(
    context: &QueryContext,
    keys: &[OptimizedCfgQueryKey],
    values: &[CfgValue],
    roots: &[crate::FunctionInstanceKey],
) -> Result<
    (
        Vec<CfgValue>,
        std::collections::BTreeSet<crate::FunctionInstanceKey>,
        std::collections::BTreeSet<crate::FunctionInstanceKey>,
    ),
    QueryAbort,
> {
    if keys.first().is_none_or(|key| {
        key.opt_level == rue_cfg::OptLevel::O0 || key.opt_level == rue_cfg::OptLevel::O1
    }) {
        return Ok((
            values.to_vec(),
            std::collections::BTreeSet::new(),
            std::collections::BTreeSet::new(),
        ));
    }
    context.check_canceled()?;

    // Collecting sorts once and bulk-builds the tree; inserting one entry at a
    // time searched the growing tree for each, and every level of that search
    // is a recursive `FunctionInstanceKey` comparison. `BTreeMap`'s
    // `FromIterator` sorts stably and keeps the last of equal keys, so a
    // repeated function still resolves to the same record `insert` left.
    let Some(records) = keys
        .iter()
        .zip(values)
        .map(|(key, value)| match value {
            CfgValue::Available(record) => Some((key.cfg.function.clone(), record.clone())),
            _ => None,
        })
        .collect::<Option<std::collections::BTreeMap<_, _>>>()
    else {
        return Ok((
            values.to_vec(),
            std::collections::BTreeSet::new(),
            std::collections::BTreeSet::new(),
        ));
    };

    // Membership in the batch is asked once per call instruction across every
    // function, and again for every edge when the call graph is built.
    // `records` is a `BTreeMap` whose keys are recursive function identities,
    // so each probe walks a tree O(log n) times. A hash set answers the same
    // question once per probe; `records` keeps its ordered iteration, which
    // call-site selection depends on.
    let record_keys: ahash::AHashSet<crate::FunctionInstanceKey> =
        records.keys().cloned().collect();

    // Keep every edge, including duplicate edges. The multiplicity is useful
    // for deterministic call-site selection and makes the graph description
    // honest even though SCC detection only needs its distinct neighbors.
    // These three are only ever inserted into and looked up by key; nothing
    // iterates them, so their ordering is not observable and they do not need
    // to pay a recursive-identity comparison per probe. `records` and `graph`
    // stay ordered, because iteration order there does reach the output
    // through call-site selection and SCC detection.
    let mut edges: ahash::AHashMap<crate::FunctionInstanceKey, Vec<crate::FunctionInstanceKey>> =
        ahash::AHashMap::new();
    let mut callsites: ahash::AHashMap<
        crate::FunctionInstanceKey,
        Vec<(rue_cfg::CfgValue, crate::FunctionInstanceKey)>,
    > = ahash::AHashMap::new();
    let mut has_calls: ahash::AHashMap<crate::FunctionInstanceKey, bool> = ahash::AHashMap::new();
    for (function, record) in &records {
        let mut calls = Vec::new();
        // Every edge found in this iteration belongs to `function`, so the
        // callees accumulate locally and land in `edges` once. Going through
        // `edges.entry(function.clone())` per call instruction cost a
        // `BTreeMap` probe over a recursive identity plus a deep key clone,
        // for each of them.
        let mut function_edges: Vec<crate::FunctionInstanceKey> = Vec::new();
        let mut any_call = false;
        for block in record.cfg.blocks() {
            for &value in &block.insts {
                let rue_cfg::CfgInstData::Call { runtime, name, .. } =
                    record.cfg.get_inst(value).data
                else {
                    continue;
                };
                any_call = true;
                if runtime.is_some() {
                    continue;
                }
                let Some(callee) = record.domains.callable_for_symbol(name) else {
                    continue;
                };
                function_edges.push(callee.clone());
                if record_keys.contains(&callee) {
                    calls.push((value, callee));
                }
            }
        }
        if !function_edges.is_empty() {
            edges.insert(function.clone(), function_edges);
        }
        has_calls.insert(function.clone(), any_call);
        callsites.insert(function.clone(), calls);
    }

    // `records` is ordered and its keys are distinct, so this collect hands
    // `BTreeMap` an already-sorted sequence and bulk-builds the tree. Inserting
    // one node at a time instead searched the growing tree for every entry,
    // paying a recursive `FunctionInstanceKey` comparison at each level.
    let graph = records
        .keys()
        .map(|function| {
            (
                function.clone(),
                edges
                    .get(function)
                    .into_iter()
                    .flatten()
                    .filter(|callee| record_keys.contains(*callee))
                    .cloned()
                    .collect::<Vec<_>>(),
            )
        })
        .collect::<std::collections::BTreeMap<_, _>>();
    let recursive = recursive_scc_nodes(&graph);
    let batch_opt_level = keys
        .first()
        .map(|key| key.opt_level)
        .unwrap_or(rue_cfg::OptLevel::O0);
    // Membership and by-key lookup only; `records` keeps its ordered iteration
    // above, and `recursive` its ordered construction. Both are probed once per
    // call site below, where a `BTree` probe means walking a recursive identity.
    let recursive_set: ahash::AHashSet<&crate::FunctionInstanceKey> = recursive.iter().collect();
    let record_lookup: ahash::AHashMap<&crate::FunctionInstanceKey, _> = records.iter().collect();
    // `eligible` is asked once per call site, and used to end in a linear scan
    // of every key in the batch comparing recursive function identities. On
    // Lattice that is 1,280 keys per question, and it made this pass the
    // largest source of `FunctionInstanceKey` equality in the whole compile.
    // The first match wins here exactly as `Iterator::find` did.
    let mut key_by_function: ahash::AHashMap<&crate::FunctionInstanceKey, &OptimizedCfgQueryKey> =
        ahash::AHashMap::with_capacity(keys.len());
    for key in keys.iter() {
        key_by_function.entry(&key.cfg.function).or_insert(key);
    }
    let eligible = |function: &crate::FunctionInstanceKey| {
        let Some(record) = record_lookup.get(function).copied() else {
            return false;
        };
        if recursive_set.contains(function) || !is_true_free_function(function) {
            return false;
        }
        let size_eligible = match batch_opt_level {
            rue_cfg::OptLevel::O2 => {
                !has_calls.get(function).copied().unwrap_or(true)
                    && phase2_size_eligible(record.cfg.value_count())
            }
            rue_cfg::OptLevel::O3 => phase3_size_eligible(record.cfg.value_count()),
            rue_cfg::OptLevel::O0 | rue_cfg::OptLevel::O1 => false,
        };
        if !size_eligible {
            return false;
        }
        matches!(
            &key_by_function
                .get(function)
                .map(|key| &key.cfg.semantic_input),
            Some(CfgSemanticInput::Body { input, .. }) if !canonical_body(&input.canonical).is_accessor
        )
    };

    let mut output = values.to_vec();
    let mut changed = std::collections::BTreeSet::new();
    for (index, key) in keys.iter().enumerate() {
        context.check_canceled()?;
        let function = &key.cfg.function;
        let Some(sites) = callsites.get(function) else {
            continue;
        };
        // The caller's own record is the same for every site in this
        // iteration, so it is resolved once rather than per site.
        let caller_record = record_lookup.get(function).copied();
        let selected = sites
            .iter()
            .filter(|(call, callee)| {
                eligible(callee)
                    && record_lookup.get(callee).copied().is_some_and(|callee| {
                        // CFG calls carry physical argument values. A
                        // zero-width source parameter can therefore make
                        // the physical count differ; the Phase-2 policy
                        // excludes that ABI shape rather than handing it
                        // to the source-parameter splice primitive.
                        caller_record.is_some_and(|caller| {
                            caller
                                .cfg
                                .get_call_args(&caller.cfg.get_inst(*call).data)
                                .len()
                                == callee.cfg.source_param_abi().len()
                        })
                    })
            })
            .cloned()
            .collect::<Vec<_>>();
        if selected.is_empty() {
            continue;
        }
        let CfgValue::Available(record) = &output[index] else {
            continue;
        };
        let mut domains = record.domains.clone();
        let mut strings = record.strings.to_vec();
        let mut interner = match copy_interner_preserving_ordinals(&record.interner, || {
            context.check_canceled()
        }) {
            Ok(interner) => Arc::new(interner),
            Err(CfgInternerCopyFailure::Checkpoint(abort)) => return Err(abort),
            Err(error) => {
                output[index] = internal_failure(
                    format!("general inlining interner isolation failed: {error}"),
                    record.body_span,
                );
                continue;
            }
        };
        let mut local_atoms = record.local_atoms.to_vec();
        let mut local_atom_identities = local_atoms
            .iter()
            .map(|atom| atom.identity.clone())
            .collect::<AHashSet<_>>();
        let mut symbol_mappings = record.codegen.symbol_mappings.as_ref().clone();
        let mut foreign_symbols = record.codegen.foreign_symbols.as_ref().clone();
        let materialization_warnings = record.materialization_warnings.to_vec();
        let warnings = record.warnings.to_vec();
        let mut implicit_destructor_targets = record
            .implicit_destructor_targets
            .iter()
            .cloned()
            .collect::<std::collections::BTreeSet<_>>();
        let mut implicit_drop_glue_targets = record
            .implicit_drop_glue_targets
            .iter()
            .cloned()
            .collect::<std::collections::BTreeSet<_>>();
        let mut implicit_destructor_dependencies_complete =
            record.implicit_destructor_dependencies_complete;
        // Splices run against a plain `Cfg` and the batch is verified once,
        // below. `ValidatedCfg` can only be minted by verifying, so threading
        // it through this loop re-proved the whole caller after every splice,
        // over a caller each splice had just made bigger: 2,112 verifications
        // for 760 spliced functions, 150,324,778 instructions on a fresh
        // Lattice compile. Nothing read those proofs -- `optimize_with_budget` below
        // demands a `ValidatedCfg` and mints its own, so the graph that
        // reaches a consumer is verified exactly as strictly as before.
        let mut current: rue_cfg::Cfg = (*record.cfg).clone();
        let mut growth_budget = (key.opt_level == rue_cfg::OptLevel::O3).then(|| {
            rue_cfg::opt::CodeGrowthBudget::with_used(
                record.code_growth_used,
                record.code_growth_blocks_used,
            )
        });
        let mut spliced = false;
        let mut failed = None;
        let mut importability_cache = ahash::AHashMap::<
            crate::FunctionInstanceKey,
            Result<(), crate::durable_cfg::CfgDomainFailure>,
        >::new();
        for (call, callee_function) in selected {
            context.check_canceled()?;
            let Some(callee) = record_lookup.get(&callee_function).copied() else {
                continue;
            };
            let growth = match rue_cfg::splice_call_growth(&current, call, &callee.cfg) {
                Ok(growth) => growth,
                Err(error) => {
                    failed = Some(format!("general inline growth preflight failed: {error}"));
                    break;
                }
            };
            let growth_charge = rue_cfg::opt::CodeGrowthBudget::charge_for_growth(growth);
            context.record_work(rue_query::WorkItem::new(
                "cfg.general-inline-growth-preflights",
                1,
            ));
            if growth_budget
                .as_ref()
                .is_some_and(|budget| !budget.can_charge(growth_charge))
            {
                context.record_work(rue_query::WorkItem::new(
                    "cfg.general-inline-budget-refusals",
                    1,
                ));
                continue;
            }
            let importability = if let Some(result) = importability_cache.get(&callee_function) {
                result.clone()
            } else {
                context.record_work(rue_query::WorkItem::new(
                    "cfg.general-inline-importability-checks",
                    1,
                ));
                let result = domains.check_importable(&callee.domains);
                importability_cache.insert(callee_function.clone(), result.clone());
                result
            };
            if let Err(error) = importability {
                if matches!(
                    error,
                    crate::durable_cfg::CfgDomainFailure::MissingStableType(_)
                ) {
                    context.record_work(rue_query::WorkItem::new(
                        "cfg.general-inline-importability-refusals",
                        1,
                    ));
                    continue;
                }
                failed = Some(format!(
                    "general inline importability check failed: {error:?}"
                ));
                break;
            }
            let call_block = current
                .blocks()
                .iter()
                .find(|block| block.insts.contains(&call))
                .map(|block| block.id)
                .ok_or_else(|| format!("general inline call site {call} is detached"));
            let call_block = match call_block {
                Ok(block) => block,
                Err(error) => {
                    failed = Some(error);
                    break;
                }
            };
            // Domain import allocates symbols/strings and mutates its
            // projection before remapping the CFG. Keep those mutations in a
            // candidate projection until the splice itself succeeds; a
            // malformed or otherwise refused candidate must not publish
            // metadata for a call that was never accepted.
            let mut candidate_domains = domains.clone();
            let mut candidate_strings = strings.clone();
            // `import_accessor_cfg` interns symbols while it remaps the callee.
            // Keep that resource mutation transactional too: a later remap or
            // splice failure must not leave an unused symbol in a caller that
            // nevertheless publishes an earlier accepted splice.
            let candidate_interner =
                match copy_interner_preserving_ordinals(&interner, || context.check_canceled()) {
                    Ok(interner) => Arc::new(interner),
                    Err(CfgInternerCopyFailure::Checkpoint(abort)) => return Err(abort),
                    Err(error) => {
                        failed = Some(format!("general inlining interner staging failed: {error}"));
                        break;
                    }
                };
            context.record_work(rue_query::WorkItem::new(
                "cfg.general-inline-interner-stages",
                1,
            ));
            context.record_work(rue_query::WorkItem::new(
                "cfg.general-inline-import-attempts",
                1,
            ));
            let (callee_cfg, string_map) = match candidate_domains.import_accessor_cfg(
                &callee.domains,
                &callee.cfg,
                &callee.interner,
                &candidate_interner,
                &mut candidate_strings,
                callee.body_span,
            ) {
                Ok(value) => value,
                Err(crate::durable_cfg::CfgDomainFailure::MissingStableType(_)) => {
                    // A body-local type domain that is not present in the
                    // caller cannot be imported without widening the caller's
                    // immutable type pool. It is outside the conservative
                    // inlining domain.
                    continue;
                }
                Err(error) => {
                    failed = Some(format!(
                        "general inline CFG domain import failed: {error:?}"
                    ));
                    break;
                }
            };
            let callee_cfg = match callee_cfg.finish_after_optimization(&record.type_pool) {
                Ok(cfg) => cfg,
                Err(error) => {
                    failed = Some(format!(
                        "imported general inline CFG failed verification: {error}"
                    ));
                    break;
                }
            };
            let mut imported_atoms = Vec::new();
            let mut imported_atom_identities = ahash::AHashSet::new();
            for atom in callee.local_atoms.iter() {
                let Some(dense_id) = string_map.get(&atom.dense_id).copied() else {
                    failed = Some("general inline local atom has no imported string id".to_owned());
                    break;
                };
                let mut atom = atom.clone();
                atom.dense_id = dense_id;
                if !local_atom_identities.contains(&atom.identity)
                    && imported_atom_identities.insert(atom.identity.clone())
                {
                    imported_atoms.push(atom);
                }
            }
            if failed.is_some() {
                break;
            }
            let candidate = match rue_cfg::splice_call_in_block(
                &current,
                call,
                call_block,
                &callee_cfg,
                &record.type_pool,
            ) {
                Ok(cfg) => cfg,
                Err(error) => {
                    failed = Some(format!("general inline splice failed: {error}"));
                    break;
                }
            };
            if let Some(budget) = growth_budget.as_mut() {
                assert!(
                    budget.can_charge(growth_charge),
                    "general inline growth preflight must precede the splice"
                );
                if !budget.try_charge(growth_charge) {
                    failed = Some("general inline growth budget changed during splice".to_owned());
                    break;
                }
            }
            domains = candidate_domains;
            strings = candidate_strings;
            interner = candidate_interner;
            for atom in imported_atoms {
                local_atom_identities.insert(atom.identity.clone());
                local_atoms.push(atom);
            }
            symbol_mappings.extend(
                callee
                    .codegen
                    .symbol_mappings
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone())),
            );
            foreign_symbols.extend(callee.codegen.foreign_symbols.iter().cloned());
            implicit_destructor_targets.extend(callee.implicit_destructor_targets.iter().cloned());
            implicit_drop_glue_targets.extend(callee.implicit_drop_glue_targets.iter().cloned());
            implicit_destructor_dependencies_complete &=
                callee.implicit_destructor_dependencies_complete;
            current = candidate;
            spliced = true;
            if growth_budget.is_some() {
                context.record_work(rue_query::WorkItem::new(
                    "cfg.general-inline-code-growth",
                    growth_charge.values,
                ));
                context.record_work(rue_query::WorkItem::new(
                    "cfg.general-inline-code-growth-blocks",
                    growth_charge.blocks,
                ));
            }
            context.record_work(rue_query::WorkItem::new("cfg.general-inline-splices", 1));
        }
        if let Some(error) = failed {
            output[index] = internal_failure(error, record.body_span);
            continue;
        }
        if !spliced {
            continue;
        }
        // The whole batch is proved here, once.
        let current = match current.finish_after_optimization(&record.type_pool) {
            Ok(cfg) => cfg,
            Err(error) => {
                output[index] = internal_failure(
                    format!("general inline batch failed verification: {error}"),
                    record.body_span,
                );
                continue;
            }
        };
        let budget = growth_budget.unwrap_or_else(rue_cfg::opt::CodeGrowthBudget::o3);
        context.record_work(rue_query::WorkItem::new("cfg.reoptimize.attempts", 1));
        let (current, stats, budget) = match rue_cfg::opt::optimize_with_budget(
            current,
            key.opt_level,
            &record.type_pool,
            budget,
        ) {
            Ok(result) => result,
            Err(error) => {
                output[index] = internal_failure(
                    format!("general inline reoptimization failed: {error:?}"),
                    record.body_span,
                );
                continue;
            }
        };
        context.record_work(rue_query::WorkItem::new(
            "cfg.optimize.loops-analyzed",
            stats.loops_analyzed,
        ));
        context.record_work(rue_query::WorkItem::new(
            "cfg.optimize.loops-unrolled",
            stats.loops_unrolled,
        ));
        context.record_work(rue_query::WorkItem::new(
            "cfg.optimize.budget-refusals",
            stats.budget_refusals,
        ));
        context.record_work(rue_query::WorkItem::new(
            "cfg.reoptimize.code-growth-used",
            stats.code_growth_used,
        ));
        context.record_work(rue_query::WorkItem::new(
            "cfg.reoptimize.code-growth-blocks-used",
            stats.code_growth_blocks_used,
        ));
        context.record_work(rue_query::WorkItem::new("cfg.reoptimize.completions", 1));
        let interner_retained_charge = frozen_interner_retained_charge(&interner);
        output[index] = CfgValue::Available(Arc::new(CfgRecord {
            air: record.air.clone(),
            source_name: record.source_name.clone(),
            num_locals: record.num_locals,
            num_param_slots: record.num_param_slots,
            cfg: current,
            code_growth_used: budget.used(),
            code_growth_blocks_used: budget.used_blocks(),
            domains,
            type_pool: record.type_pool.clone(),
            interner,
            interner_retained_charge,
            strings: strings.into(),
            local_atoms: local_atoms.into(),
            local_aggregate_type_aliases: record.local_aggregate_type_aliases,
            local_materialized_type_handles: record.local_materialized_type_handles,
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
            implicit_drop_glue_targets: implicit_drop_glue_targets
                .into_iter()
                .collect::<Vec<_>>()
                .into(),
            implicit_destructor_dependencies_complete,
            durable_reuse_allowed: false,
        }));
        changed.insert(function.clone());
    }
    // Phase 5 consumes the post-inline CFGs produced above.  This is kept in
    // the same batch so the call-site graph and the reachability result share
    // one canonical domain projection and one deterministic function set.
    let mut final_records = std::collections::BTreeMap::new();
    for (key, value) in keys.iter().zip(output.iter()) {
        if let CfgValue::Available(record) = value {
            final_records.insert(key.cfg.function.clone(), record.clone());
        }
    }
    let final_keys: ahash::AHashSet<_> = final_records.keys().cloned().collect();
    let mut graph = std::collections::BTreeMap::<
        crate::FunctionInstanceKey,
        Vec<crate::FunctionInstanceKey>,
    >::new();
    let key_by_function = keys
        .iter()
        .map(|key| (key.cfg.function.clone(), key))
        .collect::<std::collections::BTreeMap<_, _>>();
    let mut dependency_complete = true;
    context.record_work(rue_query::WorkItem::new(
        "cfg.general-reachability.functions",
        final_records.len() as u64,
    ));
    for (function, record) in &final_records {
        context.check_canceled()?;
        if !record.implicit_destructor_dependencies_complete {
            dependency_complete = false;
        }
        let mut dependencies = Vec::new();
        for block in record.cfg.blocks() {
            for &value in &block.insts {
                let rue_cfg::CfgInstData::Call { runtime, name, .. } =
                    record.cfg.get_inst(value).data
                else {
                    continue;
                };
                if runtime.is_some() {
                    continue;
                }
                let edge = match record.domains.callable_for_symbol(name) {
                    Some(callee) => {
                        let in_batch = final_keys.contains(&callee);
                        let foreign = record
                            .codegen
                            .is_foreign_source_symbol(record.interner.resolve(&name));
                        classify_reachability_edge(Some(callee), in_batch, foreign, false)
                    }
                    None => classify_reachability_edge(
                        None,
                        false,
                        false,
                        record.domains.is_known_non_callable_symbol(name),
                    ),
                };
                match edge {
                    ReachabilityEdge::Internal(callee) => dependencies.push(callee),
                    ReachabilityEdge::External => {}
                    ReachabilityEdge::Incomplete => {
                        // An opaque call or a resolved native Rue callable
                        // missing from this batch makes dependency discovery
                        // incomplete. Keep every unit rather than treating
                        // that edge as dead.
                        dependency_complete = false;
                    }
                }
            }
        }
        for owner in record.implicit_destructor_targets.iter() {
            let drop_glue = crate::FunctionInstanceKey::DropGlue(Node::new(owner.clone()));
            if final_keys.contains(&drop_glue) {
                dependencies.push(drop_glue);
            } else {
                // A named-destructor edge without its synthesized glue is an
                // incomplete dependency projection. Keep the whole batch so
                // an unavailable edge can never turn into removed code.
                dependency_complete = false;
            }
        }
        for owner in record.implicit_drop_glue_targets.iter() {
            let drop_glue = crate::FunctionInstanceKey::DropGlue(Node::new(owner.clone()));
            if final_keys.contains(&drop_glue) {
                dependencies.push(drop_glue);
            } else {
                dependency_complete = false;
            }
        }
        // Drop glue lowers destructor and nested-glue calls through `Drop`
        // instructions, not ordinary CFG `Call` instructions. Preserve those
        // implicit edges from the same typed drop-glue facts used to build the
        // unit, so eliminating an uncalled-looking destructor cannot break a
        // retained cleanup path.
        if let Some(CfgSemanticInput::DropGlue { facts, .. }) = key_by_function
            .get(function)
            .map(|key| &key.cfg.semantic_input)
        {
            if let Some(destructor) = &facts.destructor {
                if final_keys.contains(destructor) {
                    dependencies.push(destructor.clone());
                } else {
                    dependency_complete = false;
                }
            }
            for owner in facts.nested.iter() {
                let nested = crate::FunctionInstanceKey::DropGlue(Node::new(owner.clone()));
                if final_keys.contains(&nested) {
                    dependencies.push(nested);
                } else {
                    dependency_complete = false;
                }
            }
        }
        dependencies.sort();
        dependencies.dedup();
        graph.insert(function.clone(), dependencies);
    }

    let removed =
        whole_program_unreachable_checked(&graph, roots, &final_keys, dependency_complete, || {
            context.check_canceled()
        })?
        .unwrap_or_default();
    Ok((output, changed, removed))
}

#[derive(Debug, PartialEq, Eq)]
enum ReachabilityEdge<K> {
    Internal(K),
    External,
    Incomplete,
}

/// Classify one scanned call edge before it enters the rooted graph. A
/// resolved target-C callable is external by ABI contract; a resolved native
/// callable absent from the batch is an incomplete projection and must fail
/// closed.
fn classify_reachability_edge<K>(
    callee: Option<K>,
    in_batch: bool,
    foreign: bool,
    known_non_callable: bool,
) -> ReachabilityEdge<K> {
    match callee {
        Some(callee) if in_batch => ReachabilityEdge::Internal(callee),
        Some(_) if foreign => ReachabilityEdge::External,
        Some(_) => ReachabilityEdge::Incomplete,
        None if known_non_callable => ReachabilityEdge::External,
        None => ReachabilityEdge::Incomplete,
    }
}

/// Compute the complement of the canonical rooted closure.  `None` means the
/// graph is incomplete and callers must keep every unit; this explicit result
/// prevents a missing edge from being mistaken for an unreachable function.
#[cfg(test)]
fn whole_program_unreachable<K: Clone + Ord + std::hash::Hash + Eq>(
    graph: &std::collections::BTreeMap<K, Vec<K>>,
    roots: &[K],
    all_functions: &ahash::AHashSet<K>,
    dependency_complete: bool,
) -> Option<std::collections::BTreeSet<K>> {
    match whole_program_unreachable_checked(
        graph,
        roots,
        all_functions,
        dependency_complete,
        || Ok(()),
    ) {
        Ok(result) => result,
        Err(_) => None,
    }
}

fn whole_program_unreachable_checked<
    K: Clone + Ord + std::hash::Hash + Eq,
    F: FnMut() -> Result<(), QueryAbort>,
>(
    graph: &std::collections::BTreeMap<K, Vec<K>>,
    roots: &[K],
    all_functions: &ahash::AHashSet<K>,
    dependency_complete: bool,
    mut checkpoint: F,
) -> Result<Option<std::collections::BTreeSet<K>>, QueryAbort> {
    if !dependency_complete || roots.iter().any(|root| !all_functions.contains(root)) {
        return Ok(None);
    }
    let mut reachable = std::collections::BTreeSet::new();
    let mut pending = roots.to_vec();
    pending.sort();
    pending.dedup();
    while let Some(function) = pending.pop() {
        checkpoint()?;
        if !reachable.insert(function.clone()) {
            continue;
        }
        if let Some(dependencies) = graph.get(&function) {
            pending.extend(dependencies.iter().cloned());
        }
    }
    Ok(Some(
        all_functions
            .iter()
            .filter(|function| !reachable.contains(*function))
            .cloned()
            .collect(),
    ))
}

const PHASE2_VALUE_CAP: usize = 32;
/// O3 admits larger bodies after measuring the checked-in standalone examples:
/// their O0 CFG bodies range from 23 to 75 values. A 96-value cap covers that
/// observed set while leaving room below the long-tail helpers.
const PHASE3_VALUE_CAP: usize = 96;

fn phase2_size_eligible(value_count: usize) -> bool {
    value_count <= PHASE2_VALUE_CAP
}

fn phase3_size_eligible(value_count: usize) -> bool {
    value_count <= PHASE3_VALUE_CAP
}

fn is_true_free_function(function: &crate::FunctionInstanceKey) -> bool {
    let definition = match function {
        crate::FunctionInstanceKey::Definition(definition) => definition,
        crate::FunctionInstanceKey::Specialization { base, .. } => {
            let crate::FunctionInstanceKey::Definition(definition) = base.as_ref() else {
                return false;
            };
            definition
        }
        crate::FunctionInstanceKey::AnonymousMember { .. }
        | crate::FunctionInstanceKey::DropGlue(_) => return false,
    };
    definition.kind() == crate::StableDefinitionKind::Function
}

/// Return every node in a cyclic strongly-connected component. Duplicate
/// edges are accepted because call-site multiplicity is a separate property;
/// SCC membership only depends on reachability.
fn recursive_scc_nodes<K: Clone + Ord>(
    graph: &std::collections::BTreeMap<K, Vec<K>>,
) -> std::collections::BTreeSet<K> {
    let mut reverse = graph
        .keys()
        .cloned()
        .map(|node| (node, Vec::new()))
        .collect::<std::collections::BTreeMap<_, _>>();
    for (caller, callees) in graph {
        for callee in callees {
            reverse
                .get_mut(callee)
                .expect("graph nodes are complete")
                .push(caller.clone());
        }
    }
    let mut visited = std::collections::BTreeSet::new();
    let mut order = Vec::with_capacity(graph.len());
    for start in graph.keys() {
        if !visited.insert(start.clone()) {
            continue;
        }
        // A frame retains the next edge index, so siblings are not marked
        // visited until their predecessor has been fully explored.
        let mut stack = vec![(start.clone(), 0usize)];
        while let Some((node, next_index)) = stack.last_mut() {
            let next = graph.get(node).and_then(|edges| edges.get(*next_index));
            let Some(next) = next else {
                let (node, _) = stack.pop().expect("DFS frame exists");
                order.push(node);
                continue;
            };
            *next_index += 1;
            if visited.insert(next.clone()) {
                stack.push((next.clone(), 0));
            }
        }
    }
    visited.clear();
    let mut recursive = std::collections::BTreeSet::new();
    for start in order.into_iter().rev() {
        if !visited.insert(start.clone()) {
            continue;
        }
        let mut component = Vec::new();
        let mut stack = vec![start.clone()];
        while let Some(node) = stack.pop() {
            component.push(node.clone());
            for next in reverse.get(&node).into_iter().flatten().rev() {
                if visited.insert(next.clone()) {
                    stack.push(next.clone());
                }
            }
        }
        let cyclic = component.len() > 1
            || graph
                .get(&start)
                .is_some_and(|callees| callees.iter().any(|callee| callee == &start));
        if cyclic {
            recursive.extend(component);
        }
    }
    recursive
}

fn finish_cfg_optimization(
    context: &QueryContext,
    key: &OptimizedCfgQueryKey,
    cfg: rue_cfg::ValidatedCfg,
    type_pool: &rue_air::FrozenTypeInternPool,
    body_span: Span,
    build_record: impl FnOnce(rue_cfg::ValidatedCfg, u64, u64) -> CfgRecord,
) -> QueryOutput<CfgValue> {
    context.record_work(rue_query::WorkItem::new("cfg.optimize.attempts", 1));
    context.record_work(rue_query::WorkItem::new(
        "cfg.optimize.nonzero-level",
        u64::from(key.opt_level != rue_cfg::OptLevel::O0),
    ));
    match rue_cfg::opt::optimize_with_budget(
        cfg,
        key.opt_level,
        type_pool,
        rue_cfg::opt::CodeGrowthBudget::o3(),
    ) {
        Ok((cfg, stats, budget)) => {
            context.record_work(rue_query::WorkItem::new("cfg.optimize.successes", 1));
            context.record_work(rue_query::WorkItem::new(
                "cfg.optimize.loops-analyzed",
                stats.loops_analyzed,
            ));
            context.record_work(rue_query::WorkItem::new(
                "cfg.optimize.loops-unrolled",
                stats.loops_unrolled,
            ));
            context.record_work(rue_query::WorkItem::new(
                "cfg.optimize.budget-refusals",
                stats.budget_refusals,
            ));
            context.record_work(rue_query::WorkItem::new(
                "cfg.optimize.code-growth-used",
                stats.code_growth_used,
            ));
            context.record_work(rue_query::WorkItem::new(
                "cfg.optimize.code-growth-blocks-used",
                stats.code_growth_blocks_used,
            ));
            QueryOutput::success(CfgValue::Available(Arc::new(build_record(
                cfg,
                budget.used(),
                budget.used_blocks(),
            ))))
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
    redirects: &mut AHashMap<rue_cfg::BlockId, rue_cfg::BlockId>,
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
    fn accessor_interner_copy_preserves_ordinals_without_mutating_the_source() {
        let source = lasso::ThreadedRodeo::new();
        let caller = source.get_or_intern("caller");
        let shared = source.get_or_intern("shared");
        let published_charge = frozen_interner_retained_charge(&source);

        let copy =
            copy_interner_preserving_ordinals(&source, || Ok::<_, std::convert::Infallible>(()))
                .unwrap();
        assert_eq!(copy.get("caller"), Some(caller));
        assert_eq!(copy.get("shared"), Some(shared));
        assert_eq!(copy.len(), source.len());
        for ordinal in 0..source.len() {
            let symbol = lasso::Spur::try_from_usize(ordinal).unwrap();
            assert_eq!(copy.resolve(&symbol), source.resolve(&symbol));
        }

        copy.get_or_intern("callee-only");
        assert_eq!(source.len(), 2);
        assert!(source.get("callee-only").is_none());
        assert_eq!(frozen_interner_retained_charge(&source), published_charge);
        assert!(frozen_interner_retained_charge(&copy) > published_charge);
    }

    #[test]
    fn accessor_interner_copy_cancels_at_bounded_symbol_intervals() {
        let source = lasso::ThreadedRodeo::new();
        for ordinal in 0..130 {
            source.get_or_intern(format!("symbol-{ordinal}"));
        }
        let published_charge = frozen_interner_retained_charge(&source);
        let mut checkpoints = 0;
        let result = copy_interner_preserving_ordinals(&source, || {
            checkpoints += 1;
            (checkpoints < 3).then_some(()).ok_or("canceled")
        });

        assert!(matches!(
            result,
            Err(CfgInternerCopyFailure::Checkpoint("canceled"))
        ));
        assert_eq!(checkpoints, 3, "copy checks before symbols 0, 64, and 128");
        assert_eq!(source.len(), 130);
        assert_eq!(frozen_interner_retained_charge(&source), published_charge);
    }

    #[test]
    fn accessor_interner_copy_preserves_resource_failure_classes() {
        let body_span = Span::default();
        for kind in [
            lasso::LassoErrorKind::MemoryLimitReached,
            lasso::LassoErrorKind::KeySpaceExhaustion,
        ] {
            let CfgValue::Failure { errors, .. } = interner_copy_capacity_failure(kind, body_span)
            else {
                panic!("resource limit must publish a typed CFG failure");
            };
            assert!(matches!(
                errors.first().map(|error| &error.kind),
                Some(rue_error::ErrorKind::CompilerResourceLimit(_))
            ));
        }

        let CfgValue::Failure { errors, .. } =
            interner_copy_capacity_failure(lasso::LassoErrorKind::FailedAllocation, body_span)
        else {
            panic!("allocation failure must publish a typed CFG failure");
        };
        assert!(matches!(
            errors.first().map(|error| &error.kind),
            Some(rue_error::ErrorKind::CompilerResourceExhaustion(_))
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
    fn whole_program_reachability_is_ordered_and_fail_safe() {
        let graph = std::collections::BTreeMap::from([
            (1usize, vec![2]),
            (2, vec![3]),
            (3, vec![3]),
            (4, Vec::new()),
        ]);
        let all = [1, 2, 3, 4].into_iter().collect();
        assert_eq!(
            whole_program_unreachable(&graph, &[1], &all, true),
            Some(std::collections::BTreeSet::from([4]))
        );
        assert_eq!(whole_program_unreachable(&graph, &[1], &all, false), None);
        assert_eq!(whole_program_unreachable(&graph, &[99], &all, true), None);
        let mut checkpoints = 0;
        assert!(matches!(
            whole_program_unreachable_checked(&graph, &[1], &all, true, || {
                checkpoints += 1;
                (checkpoints < 2).then_some(()).ok_or(QueryAbort::Canceled)
            }),
            Err(QueryAbort::Canceled)
        ));
        assert_eq!(checkpoints, 2);
    }

    #[test]
    fn reachability_scanner_classifies_foreign_and_missing_internal_edges() {
        // Production domain construction rejects a missing live symbol before
        // the batch scanner can publish it. Exercise the scanner seam directly
        // for that fail-closed case while using the real codegen-domain foreign
        // projection for the legitimate external case.
        let foreign_domain = CfgCodegenDomain {
            defined_symbol: Arc::from("caller"),
            symbol_mappings: Arc::new(std::collections::BTreeMap::from([(
                "foreign".to_owned(),
                "ffi_symbol".to_owned(),
            )])),
            foreign_symbols: Arc::new(std::collections::BTreeSet::from(["ffi_symbol".to_owned()])),
        };
        assert!(foreign_domain.is_foreign_source_symbol("foreign"));
        assert!(!foreign_domain.is_foreign_source_symbol("missing"));
        assert_eq!(
            classify_reachability_edge(Some(7usize), false, true, false),
            ReachabilityEdge::External
        );
        assert_eq!(
            classify_reachability_edge(Some(7usize), false, false, false),
            ReachabilityEdge::Incomplete
        );
        assert_eq!(
            classify_reachability_edge(None::<usize>, false, false, true),
            ReachabilityEdge::External
        );
        assert_eq!(
            classify_reachability_edge(Some(7usize), true, true, false),
            ReachabilityEdge::Internal(7)
        );
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
    fn general_inline_scc_detection_handles_cycles_diamonds_and_duplicates() {
        let cases = [
            (
                std::collections::BTreeMap::from([(0, vec![0])]),
                [0].as_slice(),
            ),
            (
                std::collections::BTreeMap::from([(0, vec![1]), (1, vec![0])]),
                &[0, 1][..],
            ),
            (
                std::collections::BTreeMap::from([(0, vec![1, 2]), (1, vec![2]), (2, Vec::new())]),
                &[][..],
            ),
            (
                std::collections::BTreeMap::from([
                    (0, Vec::new()),
                    (1, Vec::new()),
                    (2, Vec::new()),
                ]),
                &[][..],
            ),
            (
                std::collections::BTreeMap::from([(0, vec![1, 1]), (1, vec![0])]),
                &[0, 1][..],
            ),
        ];
        for (graph, expected) in cases {
            assert_eq!(
                recursive_scc_nodes(&graph),
                expected
                    .iter()
                    .copied()
                    .collect::<std::collections::BTreeSet<_>>()
            );
        }
    }

    #[test]
    fn phase2_policy_keeps_the_32_value_boundary_exact() {
        assert!(phase2_size_eligible(PHASE2_VALUE_CAP));
        assert!(!phase2_size_eligible(PHASE2_VALUE_CAP + 1));
    }

    #[test]
    fn phase3_policy_is_larger_and_inclusive_at_96_values() {
        assert!(phase3_size_eligible(PHASE2_VALUE_CAP + 1));
        assert!(phase3_size_eligible(PHASE3_VALUE_CAP));
        assert!(!phase3_size_eligible(PHASE3_VALUE_CAP + 1));
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
            .split_once("pub(crate) fn stable_types(")
            .unwrap()
            .0;
        assert!(lookup.contains(".partition_point("));
        assert!(!lookup.contains(".iter().find"));
    }
}
