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
use std::cell::Cell;

#[cfg(test)]
thread_local! {
    static INJECT_CALL_ABI_FAILURE: Cell<bool> = const { Cell::new(false) };
}

#[cfg(test)]
pub(crate) fn with_test_call_abi_failure_injection<T>(run: impl FnOnce() -> T) -> T {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            INJECT_CALL_ABI_FAILURE.with(|enabled| enabled.set(false));
        }
    }
    INJECT_CALL_ABI_FAILURE.with(|enabled| {
        assert!(
            !enabled.replace(true),
            "call-ABI failure injection is not nestable"
        );
    });
    let _reset = Reset;
    run()
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
}

impl CfgQueryKey {
    pub(crate) fn new(
        function: crate::FunctionInstanceKey,
        configuration: crate::semantic_query_nucleus::SemanticQueryConfiguration,
        semantic_input: CfgSemanticInput,
    ) -> Self {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        function.hash(&mut hasher);
        configuration.hash(&mut hasher);
        // These stable values deliberately do not implement `Hash`. Compute
        // their complete Debug framing once at key construction instead of on
        // every memo-table probe; equality still resolves hash collisions.
        match &semantic_input {
            CfgSemanticInput::Body {
                input,
                materialization,
            } => {
                format!("{:?};{:?}", input.canonical, materialization).hash(&mut hasher);
            }
            CfgSemanticInput::DropGlue {
                owner,
                facts,
                materialization,
                ..
            } => {
                format!("{owner:?};{facts:?};{materialization:?}").hash(&mut hasher);
            }
        }
        let memo_hash = hasher.finish();
        Self {
            function,
            configuration,
            semantic_input,
            memo_hash,
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
}

impl OptimizedCfgQueryKey {
    pub(crate) fn new(cfg: CfgQueryKey, opt_level: rue_cfg::OptLevel) -> Self {
        Self { cfg, opt_level }
    }
}

impl PartialEq for OptimizedCfgQueryKey {
    fn eq(&self, other: &Self) -> bool {
        self.cfg == other.cfg && self.opt_level == other.opt_level
    }
}

impl Eq for OptimizedCfgQueryKey {}

impl std::hash::Hash for OptimizedCfgQueryKey {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.cfg.hash(state);
        std::mem::discriminant(&self.opt_level).hash(state);
    }
}

impl QueryKey for OptimizedCfgQueryKey {
    fn stable_identity(&self) -> String {
        format!("{};opt={:?}", self.cfg.stable_identity(), self.opt_level)
    }
}

#[derive(Debug, Clone)]
pub(crate) struct CfgRecord {
    /// Exact body-local AIR consumed by CFG construction. Semantic presentation
    /// reads this owned artifact instead of rematerializing a program epoch.
    pub(crate) air: Arc<rue_air::ValidatedAir>,
    pub(crate) source_name: Arc<str>,
    pub(crate) cfg: rue_cfg::ValidatedCfg,
    pub(crate) domains: crate::durable_cfg::CfgDomainProjection,
    pub(crate) type_pool: rue_air::FrozenTypeInternPool,
    pub(crate) interner: Arc<lasso::ThreadedRodeo>,
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
        let entries = (self.len() * std::mem::size_of::<lasso::Spur>()) as u64;
        self.strings().fold(entries, |charge, value| {
            charge.saturating_add(value.len() as u64)
        })
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
            .saturating_add(self.interner.retained_charge())
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
            for ty in collect_plan_types(owner, facts) {
                let dependency = crate::type_queries::TypeQueryKey {
                    ty: ty.clone(),
                    configuration: key.configuration.clone(),
                };
                let terminal = context.query_registered(layouts, dependency)?;
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
    for request in facts.builtin_nominals.iter() {
        let dependency = crate::type_queries::TypeQueryKey {
            ty: request.query_ty.clone(),
            configuration: key.configuration.clone(),
        };
        let terminal = context.query_registered(type_facts, dependency)?;
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
            )
        }
    };
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

    let mut layout_dependencies = std::collections::BTreeSet::new();
    let mut drop_dependencies = std::collections::BTreeSet::new();
    for ty in domains.stable_types() {
        collect_type_dependencies(ty, &mut layout_dependencies);
        collect_drop_type_dependency(ty, &mut drop_dependencies);
    }
    for ty in layout_dependencies {
        context.query_registered(
            layouts,
            crate::type_queries::TypeQueryKey {
                ty,
                configuration: key.configuration.clone(),
            },
        )?;
    }
    for ty in drop_dependencies {
        let dependency = crate::type_queries::TypeQueryKey {
            ty,
            configuration: key.configuration.clone(),
        };
        context.query_registered(type_facts, dependency.clone())?;
        context.query_registered(drop_glues, dependency)?;
    }
    build_cfg(context, call_abis, key, materialized, domains)
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
) -> Result<CfgValue, QueryAbort> {
    context.record_work(rue_query::WorkItem::new("cfg.build.attempts", 1));
    context.record_work(rue_query::WorkItem::new(
        "cfg.air.instructions",
        materialized.air.instructions().len() as u64,
    ));
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
    let mut call_abi_facts = std::collections::BTreeMap::new();
    for callable in callables {
        let terminal = context.query_registered(
            call_abis,
            crate::type_queries::CallAbiQueryKey {
                callable: callable.clone(),
                configuration: key.configuration.clone(),
            },
        )?;
        let QueryOutcome::Success(value) = terminal.outcome() else {
            unreachable!("CallAbi publishes typed values")
        };
        #[cfg(test)]
        let injected;
        #[cfg(test)]
        let value = if INJECT_CALL_ABI_FAILURE.with(Cell::get) {
            injected = crate::type_queries::CallAbiValue::Failure(
                crate::type_queries::TypeQueryFailure::Unavailable(Arc::from(
                    "injected call ABI failure",
                )),
            );
            &injected
        } else {
            value
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
    Ok(CfgValue::Available(Arc::new(CfgRecord {
        air: Arc::new(materialized.air),
        source_name: materialized.name.into(),
        cfg: output
            .cfg
            .expect("successful CFG construction publishes a validated CFG"),
        domains,
        type_pool: materialized.type_pool,
        interner: materialized.interner,
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
    })))
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
    let current = record.cfg.clone();
    context.record_work(rue_query::WorkItem::new("cfg.optimize.attempts", 1));
    context.record_work(rue_query::WorkItem::new(
        "cfg.optimize.nonzero-level",
        u64::from(key.opt_level != rue_cfg::OptLevel::O0),
    ));
    match rue_cfg::opt::optimize(current, key.opt_level, &record.type_pool) {
        Ok(cfg) => {
            context.record_work(rue_query::WorkItem::new("cfg.optimize.successes", 1));
            Ok(QueryOutput::success(CfgValue::Available(Arc::new(
                CfgRecord {
                    air: record.air.clone(),
                    source_name: record.source_name.clone(),
                    cfg,
                    domains: record.domains.clone(),
                    type_pool: record.type_pool.clone(),
                    interner: record.interner.clone(),
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
                body_span: record.body_span,
            })
            .with_terminal_kind(rue_query::QueryTerminalKind::Failure))
        }
    }
}
