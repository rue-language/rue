//! Canonical per-function CFG query values.
//!
//! The unoptimized family owns AIR-to-CFG lowering. The optimized family owns
//! only the selected optimization pipeline and observes the exact unoptimized
//! terminal. Both publish stable relocation domains; request-local AIR, type
//! indexes, symbols, strings, and spans are construction inputs, never memo
//! identity.

use std::hash::{Hash, Hasher};
use std::sync::Arc;

use rue_query::{QueryAbort, QueryContext, QueryFamily, QueryKey, QueryOutcome, QueryOutput};
use rue_span::Span;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CfgSemanticInput {
    Body(Arc<rue_air::SemanticBody<crate::StableDefinitionKey, crate::ModuleId>>),
    DropGlue {
        owner: crate::TypeInstanceKey,
        facts: Box<crate::type_queries::DropGlueFacts>,
    },
}

#[derive(Debug, Clone)]
pub(crate) struct CfgBodyInput {
    pub(crate) function: crate::FunctionInstanceKey,
    pub(crate) body: rue_air::SemanticBody<crate::StableDefinitionKey, crate::ModuleId>,
    pub(crate) body_span: Span,
}

#[derive(Debug)]
pub(crate) struct CfgLiveInput {
    pub(crate) function: Arc<rue_air::AnalyzedFunction>,
    pub(crate) type_pool: rue_air::FrozenTypeInternPool,
    pub(crate) interner: Arc<lasso::ThreadedRodeo>,
    pub(crate) domains: crate::durable_cfg::CfgDomainProjection,
    pub(crate) body_span: Span,
    pub(crate) aggregate_types:
        Arc<std::collections::HashMap<rue_air::Type, crate::TypeInstanceKey>>,
    pub(crate) implicit_destructor_dependencies_complete: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct CfgQueryKey {
    pub(crate) function: crate::FunctionInstanceKey,
    pub(crate) configuration: crate::semantic_query_nucleus::SemanticQueryConfiguration,
    pub(crate) semantic_input: CfgSemanticInput,
    pub(crate) layouts: Arc<
        [(
            crate::type_queries::TypeQueryKey,
            crate::type_queries::LayoutValue,
        )],
    >,
    pub(crate) type_facts: Arc<
        [(
            crate::type_queries::TypeQueryKey,
            crate::type_queries::TypeFactsValue,
        )],
    >,
    pub(crate) drop_glues: Arc<
        [(
            crate::type_queries::TypeQueryKey,
            crate::type_queries::DropGlueValue,
        )],
    >,
    pub(crate) call_abis: Arc<
        [(
            crate::type_queries::CallAbiQueryKey,
            crate::type_queries::CallAbiValue,
        )],
    >,
    pub(crate) live: Arc<CfgLiveInput>,
    memo_hash: u64,
}

impl CfgQueryKey {
    pub(crate) fn new(
        function: crate::FunctionInstanceKey,
        configuration: crate::semantic_query_nucleus::SemanticQueryConfiguration,
        semantic_input: CfgSemanticInput,
        layouts: Arc<
            [(
                crate::type_queries::TypeQueryKey,
                crate::type_queries::LayoutValue,
            )],
        >,
        type_facts: Arc<
            [(
                crate::type_queries::TypeQueryKey,
                crate::type_queries::TypeFactsValue,
            )],
        >,
        drop_glues: Arc<
            [(
                crate::type_queries::TypeQueryKey,
                crate::type_queries::DropGlueValue,
            )],
        >,
        call_abis: Arc<
            [(
                crate::type_queries::CallAbiQueryKey,
                crate::type_queries::CallAbiValue,
            )],
        >,
        live: Arc<CfgLiveInput>,
    ) -> Self {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        function.hash(&mut hasher);
        configuration.hash(&mut hasher);
        // These stable values deliberately do not implement `Hash`. Compute
        // their complete Debug framing once at key construction instead of on
        // every memo-table probe; equality still resolves hash collisions.
        format!("{semantic_input:?}").hash(&mut hasher);
        format!("{layouts:?}").hash(&mut hasher);
        format!("{type_facts:?}").hash(&mut hasher);
        format!("{drop_glues:?}").hash(&mut hasher);
        format!("{call_abis:?}").hash(&mut hasher);
        let memo_hash = hasher.finish();
        Self {
            function,
            configuration,
            semantic_input,
            layouts,
            type_facts,
            drop_glues,
            call_abis,
            live,
            memo_hash,
        }
    }
}

impl PartialEq for CfgQueryKey {
    fn eq(&self, other: &Self) -> bool {
        self.function == other.function
            && self.configuration == other.configuration
            && self.semantic_input == other.semantic_input
            && self.layouts == other.layouts
            && self.type_facts == other.type_facts
            && self.drop_glues == other.drop_glues
            && self.call_abis == other.call_abis
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
        format!(
            "{:?};target={:?};preview={:?}",
            self.function, self.configuration.target, self.configuration.preview_features
        )
    }
}

#[derive(Debug, Clone)]
pub(crate) struct OptimizedCfgQueryKey {
    pub(crate) cfg: CfgQueryKey,
    pub(crate) opt_level: rue_cfg::OptLevel,
    live_domain_hash: u64,
}

impl OptimizedCfgQueryKey {
    pub(crate) fn new(cfg: CfgQueryKey, opt_level: rue_cfg::OptLevel) -> Self {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        // Complete domains are relocatable by the collector and therefore do
        // not belong in optimized memo identity. An incomplete projection is
        // deliberately epoch-local: distinguish that one token so a successor
        // reruns the evaluator and takes its fail-closed rebuild path.
        cfg.live
            .domains
            .optimized_memo_domain_hash()
            .hash(&mut hasher);
        let live_domain_hash = hasher.finish();
        Self {
            cfg,
            opt_level,
            live_domain_hash,
        }
    }
}

impl PartialEq for OptimizedCfgQueryKey {
    fn eq(&self, other: &Self) -> bool {
        self.cfg == other.cfg
            && self.opt_level == other.opt_level
            && self
                .cfg
                .live
                .domains
                .same_optimized_memo_domain(&other.cfg.live.domains)
    }
}

impl Eq for OptimizedCfgQueryKey {}

impl std::hash::Hash for OptimizedCfgQueryKey {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.cfg.hash(state);
        std::mem::discriminant(&self.opt_level).hash(state);
        self.live_domain_hash.hash(state);
    }
}

impl QueryKey for OptimizedCfgQueryKey {
    fn stable_identity(&self) -> String {
        format!("{};opt={:?}", self.cfg.stable_identity(), self.opt_level)
    }
}

#[derive(Debug, Clone)]
pub(crate) struct CfgRecord {
    pub(crate) cfg: rue_cfg::ValidatedCfg,
    pub(crate) domains: crate::durable_cfg::CfgDomainProjection,
    pub(crate) body_span: Span,
    pub(crate) warnings: Arc<[rue_error::CompileWarning]>,
    pub(crate) implicit_destructor_targets: Arc<[crate::TypeInstanceKey]>,
    pub(crate) implicit_destructor_dependencies_complete: bool,
}

#[derive(Debug, Clone)]
pub(crate) enum CfgValue {
    Available(Arc<CfgRecord>),
    Failure {
        errors: crate::CompileErrors,
        body_span: Span,
    },
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
    for (dependency, expected) in key.layouts.iter() {
        let terminal = context.query_registered(layouts, dependency.clone())?;
        let QueryOutcome::Success(actual) = terminal.outcome() else {
            unreachable!("Layout publishes typed values")
        };
        if actual != expected {
            return Err(QueryAbort::Canceled);
        }
    }
    for (dependency, expected) in key.type_facts.iter() {
        let terminal = context.query_registered(type_facts, dependency.clone())?;
        let QueryOutcome::Success(actual) = terminal.outcome() else {
            unreachable!("TypeFacts publishes typed values")
        };
        if actual != expected {
            return Err(QueryAbort::Canceled);
        }
    }
    for (dependency, expected) in key.drop_glues.iter() {
        let terminal = context.query_registered(drop_glues, dependency.clone())?;
        let QueryOutcome::Success(actual) = terminal.outcome() else {
            unreachable!("DropGlue publishes typed values")
        };
        if actual != expected {
            return Err(QueryAbort::Canceled);
        }
    }
    for (dependency, expected) in key.call_abis.iter() {
        let terminal = context.query_registered(call_abis, dependency.clone())?;
        let QueryOutcome::Success(actual) = terminal.outcome() else {
            unreachable!("CallAbi publishes typed values")
        };
        if actual != expected {
            return Err(QueryAbort::Canceled);
        }
    }
    let value = build_cfg(context, key);
    let kind = if matches!(value, CfgValue::Failure { .. }) {
        rue_query::QueryTerminalKind::Failure
    } else {
        rue_query::QueryTerminalKind::Success
    };
    Ok(QueryOutput::success(value).with_terminal_kind(kind))
}

fn build_cfg(context: &QueryContext, key: &CfgQueryKey) -> CfgValue {
    context.record_work(rue_query::WorkItem::new("cfg.build.attempts", 1));
    context.record_work(rue_query::WorkItem::new(
        "cfg.air.instructions",
        key.live.function.air.instructions().len() as u64,
    ));
    let output = rue_cfg::CfgBuilder::build(
        &key.live.function.air,
        key.live.function.num_locals,
        key.live.function.num_param_slots,
        &key.live.function.name,
        &key.live.type_pool,
        key.live.function.param_modes.clone(),
        &key.live.interner,
        key.live.function.allow_unreachable_code,
        key.live.function.callable_kind,
    );
    if !output.errors.is_empty() {
        context.record_work(rue_query::WorkItem::new("cfg.build.failures", 1));
        return CfgValue::Failure {
            errors: output.errors.into(),
            body_span: key.live.body_span,
        };
    }
    context.record_work(rue_query::WorkItem::new("cfg.build.successes", 1));
    context.record_work(rue_query::WorkItem::new(
        "cfg.warnings",
        output.warnings.len() as u64,
    ));
    let mut implicit_destructor_targets = output
        .implicit_named_destructors
        .iter()
        .filter_map(|id| {
            key.live
                .aggregate_types
                .get(&rue_air::Type::new_struct(*id))
                .cloned()
        })
        .collect::<std::collections::BTreeSet<_>>();
    if let CfgSemanticInput::DropGlue { owner, facts } = &key.semantic_input
        && facts.destructor.is_some()
    {
        // A synthesized struct glue body does not explicitly drop its owner,
        // but the exact DropGlue terminal records whether that owner has a
        // source destructor. Observe that local query fact instead of scanning
        // the live type pool for same-named structs outside this CFG's domain.
        implicit_destructor_targets.insert(owner.clone());
    }
    CfgValue::Available(Arc::new(CfgRecord {
        cfg: output
            .cfg
            .expect("successful CFG construction publishes a validated CFG"),
        domains: key.live.domains.clone(),
        body_span: key.live.body_span,
        warnings: output.warnings.into(),
        implicit_destructor_targets: implicit_destructor_targets
            .into_iter()
            .collect::<Vec<_>>()
            .into(),
        implicit_destructor_dependencies_complete: key
            .live
            .implicit_destructor_dependencies_complete
            && !output.anonymous_destructor_dependency_incomplete,
    }))
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
    let (current, record) = if record.domains.same_live_domain(&key.cfg.live.domains) {
        (record.cfg.clone(), record.clone())
    } else {
        context.record_work(rue_query::WorkItem::new("cfg.import.attempts", 1));
        match crate::durable_cfg::CfgDomainProjection::import_cfg(
            &record.domains,
            &key.cfg.live.domains,
            &record.cfg,
            key.cfg.live.body_span,
        )
        .and_then(|editor| {
            editor
                .finish_after_optimization(&key.cfg.live.type_pool)
                .map_err(|_| crate::durable_cfg::CfgDomainFailure::Shape)
        }) {
            Ok(current) => {
                context.record_work(rue_query::WorkItem::new("cfg.import.successes", 1));
                (current, record.clone())
            }
            Err(_) => {
                // A relocation-domain miss must not turn a valid current body
                // into an ICE. Rebuild from the exact current AIR and continue
                // optimization; the regular Cfg family remains the canonical
                // fast path, while this fail-closed path preserves correctness
                // if a newly introduced CFG domain escaped projection.
                context.record_work(rue_query::WorkItem::new("cfg.import.failures", 1));
                context.record_work(rue_query::WorkItem::new("cfg.fallbacks", 1));
                match build_cfg(context, &key.cfg) {
                    CfgValue::Available(record) => (record.cfg.clone(), record.clone()),
                    rebuilt @ CfgValue::Failure { .. } => {
                        return Ok(QueryOutput::success(rebuilt)
                            .with_terminal_kind(rue_query::QueryTerminalKind::Failure));
                    }
                }
            }
        }
    };
    context.record_work(rue_query::WorkItem::new("cfg.optimize.attempts", 1));
    context.record_work(rue_query::WorkItem::new(
        "cfg.optimize.nonzero-level",
        u64::from(key.opt_level != rue_cfg::OptLevel::O0),
    ));
    match rue_cfg::opt::optimize(current, key.opt_level, &key.cfg.live.type_pool) {
        Ok(cfg) => {
            context.record_work(rue_query::WorkItem::new("cfg.optimize.successes", 1));
            Ok(QueryOutput::success(CfgValue::Available(Arc::new(
                CfgRecord {
                    cfg,
                    domains: key.cfg.live.domains.clone(),
                    body_span: key.cfg.live.body_span,
                    warnings: record.warnings.clone(),
                    implicit_destructor_targets: record.implicit_destructor_targets.clone(),
                    implicit_destructor_dependencies_complete: record
                        .implicit_destructor_dependencies_complete,
                },
            ))))
        }
        Err(error) => {
            context.record_work(rue_query::WorkItem::new("cfg.optimize.failures", 1));
            Ok(QueryOutput::success(CfgValue::Failure {
                errors: crate::CompileErrors::from(crate::CompileError::without_span(
                    rue_error::ErrorKind::InternalError(format!(
                        "CFG optimization failed: {error}"
                    )),
                )),
                body_span: key.cfg.live.body_span,
            })
            .with_terminal_kind(rue_query::QueryTerminalKind::Failure))
        }
    }
}
