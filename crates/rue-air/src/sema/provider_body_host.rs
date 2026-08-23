//! Concrete body-local host for query-backed ordinary body evaluation.
//!
//! This receiver owns only body analysis's RIR and compact semantic state. Durable
//! declaration facts are materialized through the provider-owned fact state as they are
//! consulted; no declaration epoch or whole-program analyzer is reachable here.

use std::cell::{Cell, RefCell};
use std::hash::Hash;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Instant;

use ahash::{AHashMap, AHashSet};
use lasso::{Spur, ThreadedRodeo};
use rue_error::{CompileError, CompileResult, PreviewFeatures};
use rue_rir::{
    InstData, InstRef, Rir, RirParam, RirParamMode, RirTypeSyntaxBuilder, RirTypeSyntaxRef,
    SymbolHandle,
};
use rue_span::{FileId, Span};
use rue_target::Target;

use super::aggregate_resolution::AggregateFacts as AggregateFactsTrait;
use super::body_endpoint::BodyEndpointProvider;
use super::call_resolution::CallResolutionFacts;
use super::fact_mode::{ArrayLengthRequest, ModulePrefixRequest, StructuredTypeSyntaxRequest};
use super::inference_ctx::{InferenceFactSource, InferenceGeneratedNominalOverlays};
use super::info::{FunctionCallInfo, MethodCallInfo};
use super::ordinary_engine::{
    ExpressionAnalysisBreakdown, OrdinaryBodyAnalysisHost, OrdinaryBodyEngine,
};
use super::semantic_body_export::SemanticBodyExportHost;
use super::typeck::{
    SemaTypeResolutionContext, TypeRootAuthority, TypeSyntaxHost, TypeSyntaxNamedKind,
    TypeSyntaxProvider, semantic_type_syntax_compile_error,
};
use super::{
    AnalyzedFunction, BodyAnalysisWork, BodyFactProvider, ConstInfo, ConstValue,
    DeclarationTypeDependencyKind, DeclarationTypeDependencySourceKind, FunctionInfo,
    HostInferenceFacts, InferenceContext, KnownSymbols, MethodInfo, ProviderAggregateFacts,
    ProviderBodyAnalysisState, ProviderCallFacts, ProviderEndpointFacts,
};
use crate::Node;
use crate::inference::{FunctionSig, MethodSig};
use crate::intern_pool::TypeInternPool;
use crate::types::{ArrayTypeId, EnumId, ModuleDef, ModuleId, StructId, Type, TypeKind};
use crate::{
    BodyRirBundle, CanonicalArgumentValue, DurableAnonymousSource, DurableCallableSource,
    DurableConstSource, DurableNominalSource, FunctionInstanceKey, ParamRange, ParamRangeData,
    SemanticDefinitionToken, SemanticModuleToken, TypeInstanceKey,
};

fn elapsed_ns(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX)
}

fn publish_provider_body_breakdown(
    host_setup_ns: u64,
    expression_engine_ns: u64,
    specialization_selection_ns: u64,
    body_export_ns: u64,
    result_projection_ns: u64,
    expression: ExpressionAnalysisBreakdown,
) {
    let precompute = expression.precompute_work;
    tracing::event!(
        name: "semantic_provider_breakdown",
        target: "rue::timing",
        tracing::Level::INFO,
        host_setup_ns,
        expression_engine_ns,
        specialization_selection_ns,
        body_export_ns,
        result_projection_ns,
        setup_ns = expression.setup_ns,
        inference_precompute_ns = expression.inference_precompute_ns,
        inference_precompute_structural_ns = expression.inference_precompute_structural_ns,
        inference_precompute_eval_provider_ns = expression.inference_precompute_eval_provider_ns,
        precompute_alias_nodes_visited = precompute.alias_nodes_visited,
        precompute_alias_block_statements = precompute.alias_block_statements,
        precompute_alias_allocations_examined = precompute.alias_allocations_examined,
        precompute_alias_filter_accepts = precompute.alias_filter_accepts,
        precompute_alias_filter_skips = precompute.alias_filter_skips,
        precompute_alias_eval_attempts = precompute.alias_eval_attempts,
        precompute_alias_type_successes = precompute.alias_type_successes,
        precompute_inline_scan_pops = precompute.inline_scan_pops,
        precompute_inline_scan_child_edges = precompute.inline_scan_child_edges,
        precompute_inline_scan_bodies = precompute.inline_scan_bodies,
        precompute_inline_raw_candidates = precompute.inline_raw_candidates,
        precompute_inline_final_candidates = precompute.inline_final_candidates,
        precompute_inline_eval_attempts = precompute.inline_eval_attempts,
        precompute_inline_type_successes = precompute.inline_type_successes,
        constraint_generation_ns = expression.constraint_generation_ns,
        unification_resolution_ns = expression.unification_resolution_ns,
        air_emission_validation_ns = expression.air_emission_validation_ns,
    );
}

fn intern_synthetic_argument_name(interner: &ThreadedRodeo, index: usize) -> Spur {
    let mut bytes = [0_u8; 3 + 20];
    bytes[..3].copy_from_slice(b"arg");
    let mut value = index;
    let mut start = bytes.len();
    loop {
        start -= 1;
        bytes[start] = b'0' + (value % 10) as u8;
        value /= 10;
        if value == 0 {
            break;
        }
    }
    let digits = bytes.len() - start;
    bytes.copy_within(start.., 3);
    let end = 3 + digits;
    let name = std::str::from_utf8(&bytes[..end]).expect("synthetic argument name is ASCII");
    interner.get_or_intern(name)
}

/// The one spelling of a member callable name, built from an already-rendered
/// owner component.
///
/// Taking the owner by reference is what lets a caller that spells several
/// members of the same owner render that owner once; the exact final capacity
/// is reserved up front, so joining never reallocates the way appending onto an
/// exactly-sized owner string did.
fn member_callable_name_for_owner(owner: &str, method: &str, has_self: bool) -> String {
    let separator = if has_self { "." } else { "::" };
    let mut name = String::with_capacity(owner.len() + separator.len() + method.len());
    name.push_str(owner);
    name.push_str(separator);
    name.push_str(method);
    name
}

fn append_file_callable_name(mut module_path: String, name: &str) -> String {
    module_path.reserve(1 + name.len());
    module_path.push('$');
    module_path.push_str(name);
    module_path
}

fn issue_anonymous_identity<K, M>(
    durable: &crate::AnonymousNominalKey<K, M>,
    definition: &impl Fn(&K) -> Result<SemanticDefinitionToken, ()>,
    module: &impl Fn(&M) -> Result<SemanticModuleToken, ()>,
) -> Option<super::anon_structs::IssuedAnonymousNominalKey> {
    durable.try_map_identities(definition, module).ok()
}

#[cfg(test)]
#[test]
fn synthetic_argument_names_match_the_canonical_spelling_without_a_heap_buffer() {
    let interner = ThreadedRodeo::new();
    for index in [0, 9, 10, 1_024, usize::MAX] {
        let symbol = intern_synthetic_argument_name(&interner, index);
        assert_eq!(interner.resolve(&symbol), format!("arg{index}"));
    }
}

#[cfg(test)]
#[test]
fn member_callable_names_extend_the_rendered_owner_spelling() {
    assert_eq!(
        member_callable_name_for_owner("Owner", "method", true),
        "Owner.method"
    );
    assert_eq!(
        member_callable_name_for_owner("Owner", "make", false),
        "Owner::make"
    );
    // The anonymous owner spelling the installation loops hoist, with the
    // three member shapes they install: method, associated function, and
    // destructor. One rendered owner spells all of them.
    let owner = "__anon_struct_0123456789abcdef0123456789abcdef";
    assert_eq!(
        member_callable_name_for_owner(owner, "len", true),
        "__anon_struct_0123456789abcdef0123456789abcdef.len"
    );
    assert_eq!(
        member_callable_name_for_owner(owner, "make", false),
        "__anon_struct_0123456789abcdef0123456789abcdef::make"
    );
    assert_eq!(
        member_callable_name_for_owner(owner, "__drop", true),
        "__anon_struct_0123456789abcdef0123456789abcdef.__drop"
    );
}

#[cfg(test)]
#[test]
fn file_callable_names_extend_the_owned_module_path() {
    assert_eq!(
        append_file_callable_name("pkg/support.rue".to_owned(), "build"),
        "pkg/support.rue$build"
    );
}

#[cfg(test)]
fn nested_anonymous_identity() -> crate::AnonymousNominalKey<&'static str, &'static str> {
    use crate::{
        AnonymousMemberKey, AnonymousMemberKind, AnonymousNominalKey, AnonymousNominalKind,
        CanonicalArgumentValue, CanonicalArguments, FunctionInstanceKey, NominalInstanceKey,
        StableProducerId, TypeInstanceKey,
    };

    // Comptime arguments reach an anonymous key through the producer
    // specialization that consumed them, which is the only place they live
    // (RUE-1699). Both levels are specializations so the corpus still reaches
    // a definition, a module, and a function-valued argument through them.
    let nested = AnonymousNominalKey {
        kind: AnonymousNominalKind::Struct,
        producer: StableProducerId::Function(Node::new(FunctionInstanceKey::Specialization {
            base: Node::new(FunctionInstanceKey::Definition("nested-producer")),
            arguments: CanonicalArguments {
                types: Arc::from([TypeInstanceKey::Module("nested-module")]),
                values: Arc::new([]),
            },
        })),
        anchor: rue_rir::RirStructuralAnchor::new(Vec::new()),
    };
    AnonymousNominalKey {
        kind: AnonymousNominalKind::Struct,
        producer: StableProducerId::Function(Node::new(FunctionInstanceKey::Specialization {
            base: Node::new(FunctionInstanceKey::AnonymousMember {
                owner: Node::new(TypeInstanceKey::Nominal(NominalInstanceKey::Anonymous(
                    Node::new(nested),
                ))),
                member: AnonymousMemberKey {
                    kind: AnonymousMemberKind::Method,
                    name: Arc::from("value"),
                },
            }),
            arguments: CanonicalArguments {
                types: Arc::from([
                    TypeInstanceKey::Nominal(NominalInstanceKey::Named("outer-type")),
                    TypeInstanceKey::Module("outer-module"),
                ]),
                values: Arc::from([CanonicalArgumentValue::Function(Node::new(
                    FunctionInstanceKey::Definition("argument-function"),
                ))]),
            },
        })),
        anchor: rue_rir::RirStructuralAnchor::new(Vec::new()),
    }
}

#[cfg(test)]
#[test]
fn anonymous_identity_issuance_visits_nested_graph_once_in_structural_order() {
    let durable = nested_anonymous_identity();
    let visited = RefCell::new(Vec::new());
    let issue = || {
        issue_anonymous_identity(
            &durable,
            &|definition| {
                visited
                    .borrow_mut()
                    .push(format!("definition:{definition}"));
                Ok(SemanticDefinitionToken::new(1, 1))
            },
            &|module| {
                visited.borrow_mut().push(format!("module:{module}"));
                Ok(SemanticModuleToken::new(1, 1))
            },
        )
        .expect("the complete identity graph issues")
    };

    let first = issue();
    assert_eq!(
        visited.take(),
        [
            "definition:nested-producer",
            "module:nested-module",
            "definition:outer-type",
            "module:outer-module",
            "definition:argument-function",
        ]
    );
    let second = issue();
    assert_eq!(first, second, "repeat issuance preserves the exact key");
    assert_eq!(
        visited.take().len(),
        5,
        "repeat issuance still performs exactly one graph traversal"
    );
}

#[cfg(test)]
#[test]
fn anonymous_identity_issuance_preserves_partial_failure_order() {
    let durable = nested_anonymous_identity();
    let visited = RefCell::new(Vec::new());
    let issued = issue_anonymous_identity(
        &durable,
        &|definition| {
            visited
                .borrow_mut()
                .push(format!("definition:{definition}"));
            (definition != &"outer-type")
                .then_some(SemanticDefinitionToken::new(1, 1))
                .ok_or(())
        },
        &|module| {
            visited.borrow_mut().push(format!("module:{module}"));
            Ok(SemanticModuleToken::new(1, 1))
        },
    );

    assert!(issued.is_none(), "a missing nested token fails closed");
    assert_eq!(
        visited.take(),
        [
            "definition:nested-producer",
            "module:nested-module",
            "definition:outer-type",
        ],
        "callbacks before the failure remain ordered and later nodes are untouched"
    );
}

/// Stable, request-independent description of an anonymous nominal created by
/// one successful provider body transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SemanticProducedAnonymousNominal {
    pub identity: crate::AnonymousNominalKey<SemanticDefinitionToken, SemanticModuleToken>,
    pub shape: SemanticProducedAnonymousNominalShape,
    pub type_captures: Arc<
        [(
            Arc<str>,
            TypeInstanceKey<SemanticDefinitionToken, SemanticModuleToken>,
        )],
    >,
    pub value_captures: Arc<
        [(
            Arc<str>,
            crate::CanonicalArgumentValue<SemanticDefinitionToken, SemanticModuleToken>,
        )],
    >,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SemanticProducedAnonymousNominalShape {
    Struct {
        fields: Arc<
            [(
                Arc<str>,
                TypeInstanceKey<SemanticDefinitionToken, SemanticModuleToken>,
            )],
        >,
        methods: Arc<[SemanticProducedAnonymousMethodSignature]>,
    },
    Enum {
        variants: Arc<
            [(
                Arc<str>,
                Arc<[TypeInstanceKey<SemanticDefinitionToken, SemanticModuleToken>]>,
            )],
        >,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SemanticProducedAnonymousMethodSignature {
    pub name: Arc<str>,
    pub has_self: bool,
    pub self_mode: crate::SemanticParameterMode,
    pub returns_borrow: bool,
    pub returns_inout: bool,
    pub parameters: Arc<
        [(
            SemanticProducedAnonymousMethodType,
            crate::SemanticParameterMode,
            bool,
        )],
    >,
    pub result: SemanticProducedAnonymousMethodType,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SemanticProducedAnonymousMethodType {
    SelfType,
    Concrete(TypeInstanceKey<SemanticDefinitionToken, SemanticModuleToken>),
}

/// Minimal canonical result of one provider-backed ordinary free body. The
/// compiler relocates the issuer-local token vocabulary before publication.
pub struct ProviderOrdinaryBody<K, M> {
    pub owner: crate::BodyOwnerToken,
    pub work: ProviderBodyWork,
    pub export: crate::SemanticBodyExport,
    pub function: AnalyzedFunction,
    pub warnings: Vec<rue_error::CompileWarning>,
    pub strings: Vec<String>,
    pub referenced_functions: AHashSet<Spur>,
    pub referenced_methods: std::collections::HashSet<(StructId, Spur)>,
    pub referenced_definitions: Vec<K>,
    pub referenced_values: Vec<K>,
    pub referenced_specializations:
        Vec<FunctionInstanceKey<SemanticDefinitionToken, SemanticModuleToken>>,
    pub produced_anonymous_nominals: Arc<[crate::SemanticProducedAnonymousNominal]>,
    pub type_pool: Rc<TypeInternPool>,
    pub interner: Arc<ThreadedRodeo>,
    pub definition_tokens: Vec<(SemanticDefinitionToken, K)>,
    pub module_tokens: Vec<(SemanticModuleToken, M)>,
}

/// Canonical result of one provider-backed specialization transaction.
pub struct ProviderSpecializedBody<K, M> {
    pub work: ProviderBodyWork,
    pub export: crate::SemanticSpecializedBodyExport,
    /// Exact body-local AIR and its issuing domains. The compiler currently
    /// publishes the durable export; the local materializer/CFG cutover can
    /// consume these without reconstructing a reachable-program semantic epoch.
    pub function: AnalyzedFunction,
    pub warnings: Vec<rue_error::CompileWarning>,
    pub strings: Vec<String>,
    pub type_pool: Rc<TypeInternPool>,
    pub interner: Arc<ThreadedRodeo>,
    pub produced_anonymous_nominals: Arc<[crate::SemanticProducedAnonymousNominal]>,
    pub referenced_definitions: Vec<K>,
    pub referenced_values: Vec<K>,
    pub referenced_specializations:
        Vec<FunctionInstanceKey<SemanticDefinitionToken, SemanticModuleToken>>,
    pub definition_tokens: Vec<(SemanticDefinitionToken, K)>,
    pub module_tokens: Vec<(SemanticModuleToken, M)>,
}

/// Canonical result of one provider-backed anonymous member body.
pub struct ProviderAnonymousBody<K, M> {
    pub work: ProviderBodyWork,
    pub export: crate::SemanticAnonymousBodyExport,
    /// Exact current-source span of the selected anonymous member body.
    ///
    /// The compiler converts this to the producer body's relative coordinate
    /// before retaining the transaction; no absolute source coordinate crosses
    /// the query boundary.
    pub body_span: rue_span::Span,
    /// Exact body-local AIR and its issuing domains; retained for the canonical
    /// local-materialization boundary rather than discarded after export.
    pub function: AnalyzedFunction,
    pub warnings: Vec<rue_error::CompileWarning>,
    pub strings: Vec<String>,
    pub type_pool: Rc<TypeInternPool>,
    pub interner: Arc<ThreadedRodeo>,
    pub produced_anonymous_nominals: Arc<[crate::SemanticProducedAnonymousNominal]>,
    pub referenced_definitions: Vec<K>,
    pub referenced_values: Vec<K>,
    pub referenced_specializations:
        Vec<FunctionInstanceKey<SemanticDefinitionToken, SemanticModuleToken>>,
    pub definition_tokens: Vec<(SemanticDefinitionToken, K)>,
    pub module_tokens: Vec<(SemanticModuleToken, M)>,
}

/// Value-only structural work performed inside one provider-backed body
/// analysis. The compiler aggregates these counters only when the registered
/// body transaction actually computes; retained query reuse adds no work.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ProviderBodyWork {
    /// Top-level requests to install the nominal identities needed by an
    /// imported durable type.
    pub import_nominal_registration_requests: usize,
    /// Durable type nodes reached by those requests, including cache hits and
    /// primitive/container nodes.
    pub import_nominal_type_visits: usize,
    /// Named nominal nodes probed against the request-local registration cache.
    pub import_named_nominal_probes: usize,
    /// Named nominal probes satisfied by a fully installed closure.
    pub import_named_nominal_complete_hits: usize,
    /// Recursive named nominal probes stopped by an in-progress cycle marker.
    pub import_named_nominal_cycle_hits: usize,
    /// Named nominal closures installed completely in the body-local identity
    /// universe.
    pub import_named_nominals_registered: usize,
    /// Container-element and nominal-field edges actually traversed while
    /// installing fresh closures.
    pub import_nominal_type_edges_traversed: usize,
    /// Anonymous nominal identities installed through imported durable types.
    pub import_anonymous_nominals_registered: usize,
}

#[derive(Default)]
struct ProviderBodyWorkCounters {
    import_nominal_registration_requests: Cell<usize>,
    import_nominal_type_visits: Cell<usize>,
    import_named_nominal_probes: Cell<usize>,
    import_named_nominal_complete_hits: Cell<usize>,
    import_named_nominal_cycle_hits: Cell<usize>,
    import_named_nominals_registered: Cell<usize>,
    import_nominal_type_edges_traversed: Cell<usize>,
    import_anonymous_nominals_registered: Cell<usize>,
}

#[derive(Clone, Copy)]
enum ProviderBodyWorkEvent {
    RegistrationRequest,
    TypeVisit,
    NamedProbe,
    CompleteHit,
    CycleHit,
    NamedRegistered,
    TypeEdgeTraversed,
    AnonymousRegistered,
}

impl ProviderBodyWorkCounters {
    #[inline]
    fn record(&self, event: ProviderBodyWorkEvent) {
        let counter = match event {
            ProviderBodyWorkEvent::RegistrationRequest => {
                &self.import_nominal_registration_requests
            }
            ProviderBodyWorkEvent::TypeVisit => &self.import_nominal_type_visits,
            ProviderBodyWorkEvent::NamedProbe => &self.import_named_nominal_probes,
            ProviderBodyWorkEvent::CompleteHit => &self.import_named_nominal_complete_hits,
            ProviderBodyWorkEvent::CycleHit => &self.import_named_nominal_cycle_hits,
            ProviderBodyWorkEvent::NamedRegistered => &self.import_named_nominals_registered,
            ProviderBodyWorkEvent::TypeEdgeTraversed => &self.import_nominal_type_edges_traversed,
            ProviderBodyWorkEvent::AnonymousRegistered => {
                &self.import_anonymous_nominals_registered
            }
        };
        counter.set(counter.get() + 1);
    }

    fn snapshot(&self) -> ProviderBodyWork {
        ProviderBodyWork {
            import_nominal_registration_requests: self.import_nominal_registration_requests.get(),
            import_nominal_type_visits: self.import_nominal_type_visits.get(),
            import_named_nominal_probes: self.import_named_nominal_probes.get(),
            import_named_nominal_complete_hits: self.import_named_nominal_complete_hits.get(),
            import_named_nominal_cycle_hits: self.import_named_nominal_cycle_hits.get(),
            import_named_nominals_registered: self.import_named_nominals_registered.get(),
            import_nominal_type_edges_traversed: self.import_nominal_type_edges_traversed.get(),
            import_anonymous_nominals_registered: self.import_anonymous_nominals_registered.get(),
        }
    }
}

#[derive(Clone, Default)]
pub struct ProviderWellKnownOptionFacts<K, M> {
    pub nominals: Vec<crate::AnonymousNominalKey<K, M>>,
    pub option_by_payload: Vec<(
        crate::SemanticImportType<K, M>,
        crate::SemanticImportType<K, M>,
    )>,
}

#[derive(Clone)]
pub struct DurableBodyModuleBinding<K, M> {
    pub definition: K,
    pub target: M,
    pub is_public: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DurableBodySourceLocator {
    pub file_id: FileId,
    pub physical_path: Arc<str>,
    pub source_length: u32,
}

#[derive(Clone)]
pub struct DurableReducedComptimeCall<K, M> {
    pub result: crate::SemanticComptimeCallResult<
        crate::SemanticImportType<K, M>,
        crate::SemanticImportConstValue<K, M>,
    >,
}

pub struct DurableComptimeDiagnostic {
    pub kind: rue_error::ErrorKind,
    /// Exact producer-owned source anchor, when the semantic query identified
    /// one. Consumers use their call-site span only as a fallback.
    pub span: Option<Span>,
}

pub enum DurableComptimeCallOutcome<K, M> {
    Reduced(DurableReducedComptimeCall<K, M>),
    NotReduced,
    Diagnostic(DurableComptimeDiagnostic),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DurableTryProducer {
    Option,
    Result,
}

pub trait DurableBodyLookupSource<K, M>: Clone {
    /// Return the defining module for a body owner. The owner itself is not
    /// necessarily registered as an imported module in the request-local
    /// endpoint registry.
    fn definition_module(&self, _definition: &K) -> Option<M> {
        None
    }

    fn anonymous_definition_module(
        &self,
        _identity: &crate::AnonymousNominalKey<K, M>,
    ) -> Option<M> {
        None
    }

    fn free_function(&self, current: &K, name: &str) -> Option<K>;
    fn value_const(&self, current: &K, name: &str) -> Option<K>;
    fn nominal(&self, current: &K, name: &str) -> Option<(K, crate::StableDefinitionKind)>;
    fn named_member(&self, current: &K, owner: &str, name: &str) -> Option<(K, bool)>;
    fn root_module_binding(
        &self,
        current: &K,
        name: &str,
    ) -> Option<DurableBodyModuleBinding<K, M>>;
    fn module_binding(&self, module: &M, name: &str) -> Option<DurableBodyModuleBinding<K, M>>;
    fn qualified_free_function(&self, module: &M, name: &str) -> Option<K>;
    fn qualified_value_const(&self, module: &M, name: &str) -> Option<K>;
    fn qualified_nominal(&self, module: &M, name: &str)
    -> Option<(K, crate::StableDefinitionKind)>;
    fn module_path(&self, module: &M) -> String;
    fn definition_source(&self, _definition: &K) -> Option<DurableBodySourceLocator> {
        None
    }
    fn module_source(&self, _module: &M) -> Option<DurableBodySourceLocator> {
        None
    }
    fn source_path(&self, _file: FileId) -> Option<Arc<str>> {
        None
    }
    fn out_of_scope_integer_const_paths(&self, _current: &K, _name: &str) -> Vec<Arc<str>> {
        Vec::new()
    }
    fn foreign_function_module(&self, _current: &K, _function: &K) -> Option<M> {
        None
    }
    fn foreign_definition_module(&self, current: &K, definition: &K) -> Option<M> {
        self.foreign_function_module(current, definition)
    }
    fn definition_kind(&self, _definition: &K) -> Option<crate::StableDefinitionKind> {
        None
    }
    /// Shared source owner name for a durable definition.
    fn definition_owner_name(&self, _definition: &K) -> Option<Arc<str>> {
        None
    }
    fn canonical_import(&self, _current: &K, _specifier: &str) -> Option<M> {
        None
    }
    fn trusted_try_producer(
        &self,
        _identity: &crate::AnonymousNominalKey<K, M>,
    ) -> Option<DurableTryProducer> {
        None
    }
    fn language_item_nominal(&self, _current: &K, _lang_item: crate::LangItem) -> Option<K> {
        None
    }
    /// Shared source name for a durable definition.
    fn definition_name(&self, _definition: &K) -> Option<Arc<str>> {
        None
    }
    fn reduce_comptime_call(
        &self,
        _definition: &K,
        _type_arguments: &[(Arc<str>, crate::SemanticImportType<K, M>)],
        _value_arguments: &[(Arc<str>, crate::SemanticImportConstValue<K, M>)],
    ) -> DurableComptimeCallOutcome<K, M> {
        DurableComptimeCallOutcome::NotReduced
    }
}

impl<P, S, K, M> super::semantic_body_export::SemanticBodyExportHost
    for ProviderBodyHost<'_, P, S, K, M>
where
    P: BodyFactProvider,
    S: DurableNominalSource<K, M>
        + DurableAnonymousSource<K, M>
        + DurableCallableSource<K, M>
        + DurableConstSource<K, M>
        + DurableBodyLookupSource<K, M>,
    K: Clone + Eq + Hash + Ord,
    M: Clone + Eq + Hash + Ord,
{
    fn export_body_type(
        &self,
        ty: Type,
    ) -> Result<
        crate::SemanticImportType<SemanticDefinitionToken, SemanticModuleToken>,
        crate::SemanticBodyExportFailure,
    > {
        use crate::SemanticImportType as T;
        Ok(match ty.kind() {
            TypeKind::I8 => T::I8,
            TypeKind::I16 => T::I16,
            TypeKind::I32 => T::I32,
            TypeKind::I64 => T::I64,
            TypeKind::U8 => T::U8,
            TypeKind::U16 => T::U16,
            TypeKind::U32 => T::U32,
            TypeKind::U64 => T::U64,
            TypeKind::Bool => T::Bool,
            TypeKind::Unit => T::Unit,
            TypeKind::Never => T::Never,
            TypeKind::ComptimeType => T::ComptimeType,
            TypeKind::Array(id) => {
                let (element, len) = self.type_pool.array_def(id);
                T::Array {
                    element: Arc::new(self.export_body_type(element)?),
                    len,
                }
            }
            TypeKind::PtrConst(id) => T::PtrConst(Arc::new(
                self.export_body_type(self.type_pool.ptr_const_def(id))?,
            )),
            TypeKind::PtrMut(id) => T::PtrMut(Arc::new(
                self.export_body_type(self.type_pool.ptr_mut_def(id))?,
            )),
            TypeKind::Struct(id) => {
                let def = self.type_pool.struct_def(id);
                if let Some(identity) = self.issued_anonymous_identity_for_type(ty) {
                    T::AnonymousNominal(identity)
                } else if def.is_builtin || &*def.name == "str" {
                    T::BuiltinNominal {
                        name: def.name.clone(),
                        kind: crate::SemanticImportNominalKind::Struct,
                    }
                } else {
                    let (token, _) = self.ensure_named_nominal_identity(ty, &def.name)?;
                    T::Nominal(token)
                }
            }
            TypeKind::Enum(id) => {
                let def = self.type_pool.enum_def(id);
                if let Some(identity) = self.issued_anonymous_identity_for_type(ty) {
                    T::AnonymousNominal(identity)
                } else if rue_builtins::BUILTIN_ENUMS
                    .iter()
                    .any(|builtin| builtin.name == &*def.name)
                {
                    T::BuiltinNominal {
                        name: def.name.clone(),
                        kind: crate::SemanticImportNominalKind::Enum,
                    }
                } else {
                    let (token, _) = self.ensure_named_nominal_identity(ty, &def.name)?;
                    T::Nominal(token)
                }
            }
            TypeKind::Module(id) => {
                let (token, _) = self
                    .module_tokens
                    .borrow()
                    .get(&id)
                    .cloned()
                    .ok_or(crate::SemanticBodyExportFailure::MissingStableIdentity)?;
                T::Module(token)
            }
            TypeKind::Error => {
                return Err(crate::SemanticBodyExportFailure::MissingStableIdentity);
            }
        })
    }
    fn body_struct_identity(
        &self,
        id: StructId,
    ) -> Result<
        crate::NominalInstanceKey<SemanticDefinitionToken, SemanticModuleToken>,
        crate::SemanticBodyExportFailure,
    > {
        let ty = Type::new_struct(id);
        if let Some(identity) = self.issued_anonymous_identity_for_type(ty) {
            return Ok(crate::NominalInstanceKey::Anonymous(Node::new(identity)));
        }
        let def = self.type_pool.struct_def(id);
        if def.is_builtin || &*def.name == "str" {
            return Ok(crate::NominalInstanceKey::Builtin {
                kind: crate::AnonymousNominalKind::Struct,
                name: def.name.clone(),
            });
        }
        let (token, _) = self.ensure_named_nominal_identity(ty, &def.name)?;
        Ok(crate::NominalInstanceKey::Named(token))
    }
    fn body_enum_identity(
        &self,
        id: EnumId,
    ) -> Result<
        crate::NominalInstanceKey<SemanticDefinitionToken, SemanticModuleToken>,
        crate::SemanticBodyExportFailure,
    > {
        let ty = Type::new_enum(id);
        let def = self.type_pool.enum_def(id);
        if rue_builtins::BUILTIN_ENUMS
            .iter()
            .any(|builtin| builtin.name == &*def.name)
        {
            return Ok(crate::NominalInstanceKey::Builtin {
                kind: crate::AnonymousNominalKind::Enum,
                name: def.name.clone(),
            });
        }
        if let Some(identity) = self.issued_anonymous_identity_for_type(ty) {
            return Ok(crate::NominalInstanceKey::Anonymous(Node::new(identity)));
        }
        let (token, _) = self.ensure_named_nominal_identity(ty, &def.name)?;
        Ok(crate::NominalInstanceKey::Named(token))
    }
    fn body_function_identity(
        &self,
        symbol: Spur,
    ) -> Result<
        FunctionInstanceKey<SemanticDefinitionToken, SemanticModuleToken>,
        crate::SemanticBodyExportFailure,
    > {
        if symbol == self.function_symbol
            && let Some(identity) = &self.current_anonymous_identity
        {
            return Ok(identity.clone());
        }
        if let Some(identity) = self.specialized_function_identities.borrow().get(&symbol) {
            return Ok(identity.clone());
        }
        if let Some(identity) = self.anonymous_function_identities.borrow().get(&symbol) {
            return Ok(identity.clone());
        }
        Ok(FunctionInstanceKey::Definition(
            self.function_identity(symbol)?,
        ))
    }
    fn resolve_publication_symbol(&self, symbol: &Spur) -> &str {
        self.interner.resolve(symbol)
    }

    fn body_struct_symbol(&self, id: StructId) -> String {
        self.type_pool.struct_symbol_name(id)
    }
}

#[derive(Clone, Copy)]
enum ImportNominalRegistration {
    InProgress,
    Complete,
}

#[derive(Clone)]
struct ProviderCallableTypeSyntax {
    arena: rue_rir::RirTypeSyntaxArena<Spur>,
    parameters: Arc<[RirTypeSyntaxRef]>,
    result: RirTypeSyntaxRef,
}

struct ProviderBodyHost<'a, P, S, K, M> {
    endpoint: ProviderEndpointFacts<'a, P, S, K, M>,
    calls: ProviderCallFacts<'a, P, S, K, M>,
    aggregate: ProviderAggregateFacts<K, M, S>,
    state: ProviderBodyAnalysisState<K, M, S>,
    rir: super::BodyRirView<'a>,
    interner: Arc<ThreadedRodeo>,
    type_pool: Rc<TypeInternPool>,
    known: KnownSymbols,
    target: Target,
    preview: PreviewFeatures,
    owner: crate::BodyOwnerToken,
    function_symbol: Spur,
    owner_source_symbol: Spur,
    owner_kind: crate::StableDefinitionKind,
    owner_file: FileId,
    owner_name: Option<Arc<str>>,
    /// Exact declaration selected inside a producer candidate for an
    /// anonymous-member transaction. Named bodies continue to resolve through
    /// the request-local declaration index; anonymous members have no named
    /// source owner and therefore carry their already-validated `InstRef`
    /// directly.
    current_declaration_override: Option<InstRef>,
    source: S,
    key: K,
    // Request-local exact-lookup registries. Their iteration order is never a
    // semantic input, so independently keyed fast maps avoid paying the
    // standard hasher's collision-resistance cost on compiler-owned keys.
    function_infos: RefCell<AHashMap<Spur, FunctionCallInfo>>,
    function_tokens: RefCell<AHashMap<Spur, (SemanticDefinitionToken, K)>>,
    anonymous_definition_tokens: RefCell<AHashMap<K, SemanticDefinitionToken>>,
    function_alias_keys: RefCell<AHashMap<Spur, K>>,
    specialized_function_identities:
        RefCell<AHashMap<Spur, FunctionInstanceKey<SemanticDefinitionToken, SemanticModuleToken>>>,
    observed_comptime_producers:
        RefCell<AHashSet<FunctionInstanceKey<SemanticDefinitionToken, SemanticModuleToken>>>,
    anonymous_function_identities:
        RefCell<AHashMap<Spur, FunctionInstanceKey<SemanticDefinitionToken, SemanticModuleToken>>>,
    durable_comptime_type_flags: RefCell<AHashMap<ParamRange, Vec<bool>>>,
    durable_callable_type_syntax: RefCell<AHashMap<ParamRange, ProviderCallableTypeSyntax>>,
    durable_signature_files: RefCell<AHashMap<ParamRange, FileId>>,
    named_method_infos: RefCell<AHashMap<(StructId, Spur), MethodCallInfo>>,
    const_infos: RefCell<AHashMap<(FileId, Spur), ConstInfo>>,
    observed_named_definitions: RefCell<AHashSet<K>>,
    /// Request-local state for recursive named-import identity closures. The
    /// in-progress state breaks type cycles; the complete state skips repeated
    /// registrations only after every nested field succeeded. A failed outer
    /// walk clears the request-local cache, so no partial cycle member survives.
    /// Compact endpoint tokens avoid retaining a second copy of each durable key.
    import_nominal_registrations:
        RefCell<AHashMap<SemanticDefinitionToken, ImportNominalRegistration>>,
    provider_body_work: ProviderBodyWorkCounters,
    nominal_tokens: RefCell<AHashMap<Type, (SemanticDefinitionToken, K)>>,
    modules_by_file: RefCell<AHashMap<FileId, M>>,
    module_tokens: RefCell<AHashMap<ModuleId, (SemanticModuleToken, M)>>,
    module_tokens_by_target: RefCell<AHashMap<M, SemanticModuleToken>>,
    next_module_file: Cell<u32>,
    generated_structs: AHashMap<Spur, StructId>,
    generated_enums: AHashMap<Spur, EnumId>,
    anonymous_methods: RefCell<AHashMap<(StructId, Spur), MethodCallInfo>>,
    /// Anonymous method endpoints whose complete durable signature set has
    /// already been installed in this body request. Provider type resolution
    /// handles recursive nominal shells inside the type pool and does not call
    /// back into endpoint installation, so only successful walks are cached.
    anonymous_method_registrations: RefCell<AHashSet<Type>>,
    anonymous_struct_ids: AHashSet<StructId>,
    anonymous_enum_ids: AHashSet<EnumId>,
    anon_struct_identities: AHashMap<super::anon_structs::IssuedAnonymousNominalKey, StructId>,
    anon_enum_identities: AHashMap<super::anon_structs::IssuedAnonymousNominalKey, EnumId>,
    anonymous_digest_owners: AHashMap<u128, super::anon_structs::IssuedAnonymousNominalKey>,
    /// Kept on the std hasher, unlike its neighbours: `produced_anonymous_nominals`
    /// walks this map into the exported nominal payload and sorts that payload by
    /// identity alone, so two entries sharing an identity would keep whatever
    /// order the table iterated in. The sort key is not total over the entries,
    /// which makes the iteration order reachable from an emitted artifact.
    canonical_anonymous_types:
        std::collections::HashMap<Type, super::anon_structs::IssuedAnonymousNominalKey>,
    /// Kept on the std hasher for the same class of reason: `anonymous_struct_id`
    /// and `anonymous_enum_id` scan this map with `find_map`, which answers with
    /// an arbitrary member of the matching set rather than a canonical one.
    consulted_anonymous_types:
        RefCell<std::collections::HashMap<Type, super::anon_structs::IssuedAnonymousNominalKey>>,
    durable_anonymous_types: AHashMap<Type, crate::AnonymousNominalKey<K, M>>,
    anon_struct_method_sigs: AHashMap<StructId, Vec<super::AnonMethodSig>>,
    anon_struct_captured_values: AHashMap<StructId, AHashMap<Spur, ConstValue>>,
    anon_struct_type_subst: AHashMap<StructId, AHashMap<Spur, Type>>,
    active_anonymous_producer: Option<super::anon_structs::IssuedStableProducerId>,
    body_work: BodyAnalysisWork,
    expression_breakdown: Option<ExpressionAnalysisBreakdown>,
    recovered_errors: Vec<CompileError>,
    deferred_ownership: Vec<super::DeferredOwnershipGate>,
    ctor_displays: AHashMap<Type, String>,
    current_anonymous_identity:
        Option<FunctionInstanceKey<SemanticDefinitionToken, SemanticModuleToken>>,
}

impl<'a, P, S, K, M> ProviderBodyHost<'a, P, S, K, M>
where
    P: BodyFactProvider,
    S: DurableNominalSource<K, M>
        + DurableAnonymousSource<K, M>
        + DurableCallableSource<K, M>
        + DurableConstSource<K, M>
        + DurableBodyLookupSource<K, M>,
    K: Clone + Eq + Hash + Ord,
    M: Clone + Eq + Hash + Ord,
{
    fn issued_anonymous_identity_for_type(
        &self,
        ty: Type,
    ) -> Option<super::anon_structs::IssuedAnonymousNominalKey> {
        if let Some(identity) = self
            .canonical_anonymous_types
            .get(&ty)
            .cloned()
            .or_else(|| self.consulted_anonymous_types.borrow().get(&ty).cloned())
        {
            return Some(identity);
        }
        let durable = self.endpoint.durable_anonymous_identity(ty)?;
        let issued = self.register_and_issue_anonymous_identity(&durable)?;
        self.endpoint
            .register_anonymous_nominal(issued.clone(), durable);
        self.consulted_anonymous_types
            .borrow_mut()
            .insert(ty, issued.clone());
        Some(issued)
    }

    fn new(
        provider: &'a P,
        source: S,
        bundle: &'a BodyRirBundle,
        key: K,
        file: FileId,
        name: &str,
        owner_kind: crate::StableDefinitionKind,
        owner_name: Option<&str>,
        target: Target,
        preview: PreviewFeatures,
        well_known: &ProviderWellKnownOptionFacts<K, M>,
    ) -> Option<Self> {
        let state = bundle.provider_body_state(source.clone());
        let rir = bundle.view();
        let endpoint = ProviderEndpointFacts::with_state(provider, &state, rir.clone());
        let calls = ProviderCallFacts::with_state(provider, &state, rir.clone());
        let mut aggregate = ProviderAggregateFacts::with_state(&state);
        if let Some(locator) = source.definition_source(&key) {
            if locator.file_id != file {
                return None;
            }
            aggregate.register_file_path(file, &locator.physical_path);
        }
        let function_token =
            endpoint.register_body_owner(key.clone(), file, name, owner_kind, owner_name);
        let function_symbol = state.identity_context().name_symbol(name);
        let interner = state.interner();
        let type_pool = state.type_pool();
        let known = KnownSymbols::new(&interner);
        endpoint
            .install_well_known_option_types(&well_known.nominals, &well_known.option_by_payload)?;
        endpoint.finalize_containment_metadata()?;
        let mut host = Self {
            endpoint,
            calls,
            aggregate,
            state,
            rir,
            interner,
            type_pool,
            known,
            target,
            preview,
            owner: crate::BodyOwnerToken::new(function_token.issuer(), function_token.slot()),
            function_symbol,
            owner_source_symbol: function_symbol,
            owner_kind,
            owner_file: file,
            owner_name: owner_name.map(Arc::from),
            current_declaration_override: None,
            source,
            key: key.clone(),
            function_infos: RefCell::new(AHashMap::new()),
            function_tokens: RefCell::new(AHashMap::from([(
                function_symbol,
                (function_token, key),
            )])),
            anonymous_definition_tokens: RefCell::new(AHashMap::new()),
            function_alias_keys: RefCell::new(AHashMap::new()),
            specialized_function_identities: RefCell::new(AHashMap::new()),
            observed_comptime_producers: RefCell::new(AHashSet::new()),
            anonymous_function_identities: RefCell::new(AHashMap::new()),
            durable_comptime_type_flags: RefCell::new(AHashMap::new()),
            durable_callable_type_syntax: RefCell::new(AHashMap::new()),
            durable_signature_files: RefCell::new(AHashMap::new()),
            named_method_infos: RefCell::new(AHashMap::new()),
            const_infos: RefCell::new(AHashMap::new()),
            observed_named_definitions: RefCell::new(AHashSet::new()),
            import_nominal_registrations: RefCell::new(AHashMap::new()),
            provider_body_work: ProviderBodyWorkCounters::default(),
            nominal_tokens: RefCell::new(AHashMap::new()),
            modules_by_file: RefCell::new(AHashMap::new()),
            module_tokens: RefCell::new(AHashMap::new()),
            module_tokens_by_target: RefCell::new(AHashMap::new()),
            next_module_file: Cell::new(u32::MAX),
            generated_structs: AHashMap::new(),
            generated_enums: AHashMap::new(),
            anonymous_methods: RefCell::new(AHashMap::new()),
            anonymous_method_registrations: RefCell::new(AHashSet::new()),
            anonymous_struct_ids: AHashSet::new(),
            anonymous_enum_ids: AHashSet::new(),
            anon_struct_identities: AHashMap::new(),
            anon_enum_identities: AHashMap::new(),
            anonymous_digest_owners: AHashMap::new(),
            canonical_anonymous_types: std::collections::HashMap::new(),
            consulted_anonymous_types: RefCell::new(std::collections::HashMap::new()),
            durable_anonymous_types: AHashMap::new(),
            anon_struct_method_sigs: AHashMap::new(),
            anon_struct_captured_values: AHashMap::new(),
            anon_struct_type_subst: AHashMap::new(),
            active_anonymous_producer: None,
            body_work: BodyAnalysisWork::default(),
            expression_breakdown: None,
            recovered_errors: Vec::new(),
            deferred_ownership: Vec::new(),
            ctor_displays: AHashMap::new(),
            current_anonymous_identity: None,
        };
        for identity in &well_known.nominals {
            host.install_canonical_anonymous_identity(identity)?;
        }
        Some(host)
    }

    fn push_durable_type_syntax(
        &self,
        builder: &mut RirTypeSyntaxBuilder<Spur>,
        ty: &crate::SemanticImportType<K, M>,
        parameters: &[crate::DurableSignatureParameter<K, M>],
    ) -> Option<RirTypeSyntaxRef> {
        use crate::SemanticImportType as T;
        let named = |builder: &mut RirTypeSyntaxBuilder<Spur>, name: &str| {
            builder
                .push_named_type(self.interner.get_or_intern(name))
                .ok()
        };
        match ty {
            T::I8 => named(builder, "i8"),
            T::I16 => named(builder, "i16"),
            T::I32 => named(builder, "i32"),
            T::I64 => named(builder, "i64"),
            T::U8 => named(builder, "u8"),
            T::U16 => named(builder, "u16"),
            T::U32 => named(builder, "u32"),
            T::U64 => named(builder, "u64"),
            T::Bool => named(builder, "bool"),
            T::Unit => builder.push_unit_type().ok(),
            T::Never => builder.push_never_type().ok(),
            T::ComptimeType => named(builder, "type"),
            T::BuiltinNominal { name, .. } => named(builder, name),
            T::Nominal(definition) => named(builder, &self.source.definition_name(definition)?),
            T::AnonymousNominal(_) => {
                let resolved = self
                    .state
                    .identity_context()
                    .pool_mut()?
                    .resolve_provider_type(ty)
                    .ok()?;
                named(
                    builder,
                    &resolved.safe_name_with_pool(Some(&self.type_pool)),
                )
            }
            T::Array { element, len } => {
                let element = self.push_durable_type_syntax(builder, element, parameters)?;
                let length = builder.push_integer(i128::from(*len)).ok()?;
                builder.push_array_type(element, length).ok()
            }
            T::PtrConst(pointee) => {
                let pointee = self.push_durable_type_syntax(builder, pointee, parameters)?;
                builder.push_pointer_const_type(pointee).ok()
            }
            T::PtrMut(pointee) => {
                let pointee = self.push_durable_type_syntax(builder, pointee, parameters)?;
                builder.push_pointer_mut_type(pointee).ok()
            }
            T::Slice { element, .. } => {
                let element = self.push_durable_type_syntax(builder, element, parameters)?;
                builder.push_slice_type(element).ok()
            }
            T::Module(module) => {
                let path = self.source.module_path(module);
                named(builder, &path)
            }
            T::GenericParameter(index) => {
                named(builder, parameters.get(*index as usize)?.name.as_ref())
            }
        }
    }

    fn build_durable_callable_type_syntax(
        &self,
        function: &crate::DurableFunction<K, M>,
    ) -> Option<ProviderCallableTypeSyntax> {
        let mut builder = RirTypeSyntaxBuilder::default();
        let parameters = function
            .parameters
            .iter()
            .map(|parameter| {
                self.push_durable_type_syntax(&mut builder, &parameter.ty, &function.parameters)
            })
            .collect::<Option<Arc<[_]>>>()?;
        let result =
            self.push_durable_type_syntax(&mut builder, &function.result, &function.parameters)?;
        Some(ProviderCallableTypeSyntax {
            arena: builder.finish(),
            parameters,
            result,
        })
    }

    fn remap_callable_type_syntax(
        &self,
        syntax: &crate::DurableCallableTypeSyntax,
    ) -> Option<ProviderCallableTypeSyntax> {
        let mut builder = RirTypeSyntaxBuilder::default();
        let mapped = builder
            .append_remapped(
                &syntax.syntax,
                |symbol| self.interner.get_or_intern(symbol.as_ref()),
                || Ok::<_, std::convert::Infallible>(()),
            )
            .ok()?;
        let map = |reference: RirTypeSyntaxRef| mapped.get(reference.index()).copied();
        let parameters = syntax
            .parameters
            .iter()
            .copied()
            .map(map)
            .collect::<Option<Arc<[_]>>>()?;
        let result = map(syntax.result)?;
        Some(ProviderCallableTypeSyntax {
            arena: builder.finish(),
            parameters,
            result,
        })
    }

    fn callable_returns_type(
        &self,
        function: &crate::DurableFunction<K, M>,
        exact_type_syntax: Option<&crate::DurableCallableTypeSyntax>,
    ) -> bool {
        exact_type_syntax.map_or_else(
            || matches!(function.result, crate::SemanticImportType::ComptimeType),
            |syntax| {
                let Some(rue_rir::RirTypeSyntaxNode::Named(symbol)) =
                    syntax.syntax.node(syntax.result)
                else {
                    return false;
                };
                syntax
                    .syntax
                    .symbol(*symbol)
                    .is_some_and(|symbol| symbol.as_ref() == "type")
            },
        )
    }

    fn install_durable_callable_metadata(
        &self,
        info: FunctionCallInfo,
        function: &crate::DurableFunction<K, M>,
        retain_rir_type_syntax: bool,
        exact_type_syntax: Option<&crate::DurableCallableTypeSyntax>,
        signature_file: FileId,
    ) {
        self.durable_comptime_type_flags.borrow_mut().insert(
            info.params,
            function
                .parameters
                .iter()
                .map(|parameter| {
                    parameter.is_comptime
                        && matches!(parameter.ty, crate::SemanticImportType::ComptimeType)
                })
                .collect(),
        );
        if !retain_rir_type_syntax {
            self.durable_signature_files
                .borrow_mut()
                .insert(info.params, signature_file);
            let requires_deferred_syntax = |ty: &crate::SemanticImportType<K, M>| {
                // The semantic nucleus uses `ComptimeType` as the placeholder
                // for both type-parameter and value-parameter dependent
                // annotations (for example `[i32; N]`). Preserve exact syntax
                // for either form so specialization can substitute the
                // declaration instead of treating the placeholder as a type.
                matches!(ty, crate::SemanticImportType::ComptimeType)
                    || super::body_identity::semantic_import_type_mentions_generic_parameter(ty)
            };
            let has_deferred_type = function
                .parameters
                .iter()
                .any(|parameter| requires_deferred_syntax(&parameter.ty))
                || requires_deferred_syntax(&function.result);
            if has_deferred_type {
                if let Some(syntax) = exact_type_syntax
                    .and_then(|syntax| self.remap_callable_type_syntax(syntax))
                    .or_else(|| self.build_durable_callable_type_syntax(function))
                {
                    self.durable_callable_type_syntax
                        .borrow_mut()
                        .insert(info.params, syntax);
                }
            }
        }
    }

    fn current_callable_locator(&self) -> Option<(InstRef, InstRef, Span)> {
        let declaration = if let Some(declaration) = self.current_declaration_override {
            declaration
        } else {
            let name = self.interner.resolve(&self.owner_source_symbol);
            match self.owner_kind {
                crate::StableDefinitionKind::Function => {
                    self.endpoint.first_free_function(name, self.owner_file)?
                }
                crate::StableDefinitionKind::Method
                | crate::StableDefinitionKind::AssociatedFunction => self
                    .endpoint
                    .named_method_declaration(self.owner_file, self.owner_name.as_deref()?, name)?,
                crate::StableDefinitionKind::Destructor => self
                    .endpoint
                    .destructor(self.owner_file, self.owner_name.as_deref()?)?,
                _ => return None,
            }
        };
        let instruction = self.rir.rir().get(declaration);
        let body = match instruction.data {
            InstData::FnDecl { body, .. } | InstData::DropFnDecl { body, .. } => body,
            _ => return None,
        };
        Some((body, declaration, instruction.span))
    }

    fn function_info_for_symbol(&self, symbol: Spur) -> Option<FunctionCallInfo> {
        if let Some(info) = self.function_infos.borrow().get(&symbol).copied() {
            return Some(info);
        }
        if symbol == self.function_symbol {
            return self
                .endpoint
                .endpoint_function_info(symbol)
                .map(FunctionCallInfo::from_body);
        }
        let (key, file, name) =
            if let Some(key) = self.function_alias_keys.borrow().get(&symbol).cloned() {
                let file = match self.source.foreign_function_module(&self.key, &key) {
                    Some(module) => self.register_module_target(module)?.1,
                    None => self.owner_file,
                };
                let name = self.source.definition_name(&key)?;
                (key, file, name)
            } else {
                let name = self.interner.resolve(&symbol);
                (
                    self.source.free_function(&self.key, name)?,
                    self.owner_file,
                    Arc::from(name),
                )
            };
        let function = DurableCallableSource::function(&self.source, &key)?;
        for parameter in function.parameters.iter() {
            if !super::body_identity::semantic_import_type_mentions_generic_parameter(&parameter.ty)
            {
                self.register_import_nominal_identities(&parameter.ty)
                    .ok()?;
            }
        }
        if !super::body_identity::semantic_import_type_mentions_generic_parameter(&function.result)
        {
            self.register_import_nominal_identities(&function.result)
                .ok()?;
        }
        let exact_type_syntax = function.type_syntax.as_ref();
        let returns_type = self.callable_returns_type(&function, exact_type_syntax);
        let info = self
            .state
            .identity_context()
            .pool_mut()?
            .resolve_function_call_from(&key, &function, returns_type, file)
            .ok()?;
        self.install_durable_callable_metadata(info, &function, false, exact_type_syntax, file);
        let token = self.endpoint.register_function(key.clone(), file, &name);
        self.function_infos.borrow_mut().insert(symbol, info);
        self.function_tokens
            .borrow_mut()
            .insert(symbol, (token, key));
        Some(info)
    }

    fn issue_consulted_anonymous_identity(
        &self,
        durable: &crate::AnonymousNominalKey<K, M>,
    ) -> Option<super::anon_structs::IssuedAnonymousNominalKey> {
        issue_anonymous_identity(
            durable,
            &|definition| {
                self.anonymous_definition_tokens
                    .borrow()
                    .get(definition)
                    .copied()
                    .ok_or(())
            },
            &|module| {
                self.module_tokens_by_target
                    .borrow()
                    .get(module)
                    .copied()
                    .ok_or(())
            },
        )
    }

    fn register_and_issue_anonymous_identity(
        &self,
        durable: &crate::AnonymousNominalKey<K, M>,
    ) -> Option<super::anon_structs::IssuedAnonymousNominalKey> {
        // Each relocation callback publishes its token before returning it. The
        // host owns those maps for the request lifetime, so successful
        // registration has no later issuance-failure boundary. This keeps
        // partial-failure order and registration-before-mint intact while
        // constructing only the issued key for a successful graph.
        issue_anonymous_identity(
            durable,
            &|definition| {
                if let Some(token) = self
                    .anonymous_definition_tokens
                    .borrow()
                    .get(definition)
                    .copied()
                {
                    return Ok::<SemanticDefinitionToken, ()>(token);
                }
                let name = self.source.definition_name(definition).ok_or(())?;
                let kind = self.source.definition_kind(definition).ok_or(())?;
                let file = match self.source.foreign_definition_module(&self.key, definition) {
                    Some(module) => self.register_module_target(module).ok_or(())?.1,
                    None => self.owner_file,
                };
                let token = match kind {
                    crate::StableDefinitionKind::Struct | crate::StableDefinitionKind::Enum => self
                        .endpoint
                        .register_named_nominal(definition.clone(), file.index(), &name, kind),
                    crate::StableDefinitionKind::Function => {
                        self.endpoint
                            .register_function(definition.clone(), file, &name)
                    }
                    crate::StableDefinitionKind::ValueConst
                    | crate::StableDefinitionKind::ModuleBinding
                    | crate::StableDefinitionKind::Destructor
                    | crate::StableDefinitionKind::Method
                    | crate::StableDefinitionKind::AssociatedFunction => {
                        let owner = self.source.definition_owner_name(definition);
                        self.endpoint.register_body_owner(
                            definition.clone(),
                            file,
                            &name,
                            kind,
                            owner.as_deref(),
                        )
                    }
                };
                self.anonymous_definition_tokens
                    .borrow_mut()
                    .insert(definition.clone(), token);
                Ok::<SemanticDefinitionToken, ()>(token)
            },
            &|module| {
                self.register_module_target(module.clone()).ok_or(())?;
                self.module_tokens_by_target
                    .borrow()
                    .get(module)
                    .copied()
                    .ok_or(())
            },
        )
    }

    fn install_canonical_anonymous_identity(
        &mut self,
        durable: &crate::AnonymousNominalKey<K, M>,
    ) -> Option<Type> {
        let issued = self.register_and_issue_anonymous_identity(durable)?;
        self.endpoint
            .register_anonymous_nominal(issued.clone(), durable.clone());
        let ty = self.endpoint.mint_anonymous(durable)?;
        self.canonical_anonymous_types.insert(ty, issued.clone());
        self.durable_anonymous_types.insert(ty, durable.clone());
        match ty.kind() {
            TypeKind::Struct(id) => {
                self.anon_struct_identities.insert(issued.clone(), id);
                self.anonymous_struct_ids.insert(id);
            }
            TypeKind::Enum(id) => {
                self.anon_enum_identities.insert(issued.clone(), id);
                self.anonymous_enum_ids.insert(id);
            }
            _ => return None,
        }
        self.install_provider_anonymous_methods_with_issued(durable, ty, issued)?;
        Some(ty)
    }

    fn function_for_file_symbol(&self, file: FileId, source_symbol: Spur) -> Option<Spur> {
        if file == self.owner_file {
            self.function_info_for_symbol(source_symbol)?;
            return Some(source_symbol);
        }
        let module = self.modules_by_file.borrow().get(&file)?.clone();
        let name = self.interner.resolve(&source_symbol);
        let internal = self.interner.get_or_intern(append_file_callable_name(
            self.source.module_path(&module),
            name,
        ));
        if self.function_infos.borrow().contains_key(&internal) {
            return Some(internal);
        }
        let key = self.source.qualified_free_function(&module, name)?;
        let function = DurableCallableSource::function(&self.source, &key)?;
        for parameter in function.parameters.iter() {
            if !super::body_identity::semantic_import_type_mentions_generic_parameter(&parameter.ty)
            {
                self.register_import_nominal_identities(&parameter.ty)
                    .ok()?;
            }
        }
        if !super::body_identity::semantic_import_type_mentions_generic_parameter(&function.result)
        {
            self.register_import_nominal_identities(&function.result)
                .ok()?;
        }
        let exact_type_syntax = function.type_syntax.as_ref();
        let returns_type = self.callable_returns_type(&function, exact_type_syntax);
        let info = self
            .state
            .identity_context()
            .pool_mut()?
            .resolve_function_call_from(&key, &function, returns_type, file)
            .ok()?;
        self.install_durable_callable_metadata(info, &function, false, exact_type_syntax, file);
        let token = self.endpoint.register_function(key.clone(), file, name);
        self.function_infos.borrow_mut().insert(internal, info);
        self.function_tokens
            .borrow_mut()
            .insert(internal, (token, key));
        Some(internal)
    }

    fn register_module_target(&self, target: M) -> Option<(ModuleId, FileId)> {
        if let Some((id, (_, _))) = self
            .module_tokens
            .borrow()
            .iter()
            .find(|(_, (_, module))| module == &target)
        {
            let id = *id;
            let file = self.calls.module_def(id)?.file_id;
            return Some((id, file));
        }
        let locator = self.source.module_source(&target);
        let file = locator.as_ref().map_or_else(
            || {
                let mut candidate = self.next_module_file.get();
                while candidate == self.owner_file.index()
                    || self
                        .modules_by_file
                        .borrow()
                        .contains_key(&FileId::new(candidate))
                {
                    candidate = candidate.checked_sub(1)?;
                }
                self.next_module_file.set(candidate.saturating_sub(1));
                Some(FileId::new(candidate))
            },
            |locator| Some(locator.file_id),
        )?;
        let physical_path = locator
            .map(|locator| locator.physical_path.to_string())
            .unwrap_or_else(|| self.source.module_path(&target));
        let logical_path = self.source.module_path(&target);
        let token = self.endpoint.register_module(
            target.clone(),
            file,
            &physical_path,
            &logical_path,
            &logical_path,
        )?;
        let id = self.calls.register_module(
            target.clone(),
            file,
            &physical_path,
            &logical_path,
            &logical_path,
        )?;
        self.aggregate.register_module(
            target.clone(),
            file,
            &physical_path,
            &logical_path,
            &logical_path,
        )?;
        self.modules_by_file
            .borrow_mut()
            .insert(file, target.clone());
        self.module_tokens_by_target
            .borrow_mut()
            .insert(target.clone(), token);
        self.module_tokens.borrow_mut().insert(id, (token, target));
        Some((id, file))
    }

    fn module_binding_for_symbol(&self, file: FileId, symbol: Spur) -> Option<ConstInfo> {
        let name = self.interner.resolve(&symbol);
        if let Some(info) = self.calls.module_binding(file, name) {
            return Some(info);
        }
        let binding = if file == self.owner_file {
            self.source.root_module_binding(&self.key, name)?
        } else {
            let module = self.modules_by_file.borrow().get(&file)?.clone();
            self.source.module_binding(&module, name)?
        };
        let (module, _) = self.register_module_target(binding.target)?;
        let ty = Type::new_module(module);
        let info = ConstInfo {
            is_pub: binding.is_public,
            ty,
            value: ConstValue::Type(ty),
            span: if file == self.owner_file {
                self.current_callable_locator()?.2
            } else {
                Span::point_in_file(file, 0)
            },
        };
        self.calls.register_module_binding(file, name, info.clone());
        self.aggregate
            .register_module_binding(file, name, info.clone());
        Some(info)
    }

    fn function_token_for_symbol(&self, symbol: Spur) -> Option<(SemanticDefinitionToken, K)> {
        if let Some(token) = self.function_tokens.borrow().get(&symbol).cloned() {
            return Some(token);
        }
        self.function_info_for_symbol(symbol)?;
        self.function_tokens.borrow().get(&symbol).cloned()
    }

    /// The callable symbol `Owner.method` / `Owner::method` for a member of
    /// `struct_id`, with the owner component rendered by
    /// [`Self::member_callable_owner`] — which is
    /// [`TypeInternPool::struct_symbol_name`], the same renderer call-site
    /// analysis uses. Every map keyed by a member callable symbol
    /// (`anonymous_function_identities`, `function_tokens`) must write and
    /// read through the one spelling in
    /// [`member_callable_name_for_owner`] (RUE-1236); a second renderer would
    /// reintroduce a join that only holds while two policies agree.
    fn member_callable_name(&self, struct_id: StructId, method: &str, has_self: bool) -> String {
        member_callable_name_for_owner(&self.member_callable_owner(struct_id), method, has_self)
    }

    /// The owner component every member callable symbol of `struct_id` shares.
    ///
    /// A pool entry's name never changes after registration — completing a
    /// declared shell rewrites its fields, not its name, and anonymity
    /// registration precedes any member installation — so a caller spelling
    /// several members of one owner renders this once and reuses it instead of
    /// taking the pool's read lock and rebuilding the same string per member.
    fn member_callable_owner(&self, struct_id: StructId) -> String {
        self.type_pool.struct_symbol_name(struct_id)
    }

    fn member_callable_symbol(&self, struct_id: StructId, method: &str, has_self: bool) -> Spur {
        self.member_callable_symbol_for_owner(
            &self.member_callable_owner(struct_id),
            method,
            has_self,
        )
    }

    fn member_callable_symbol_for_owner(&self, owner: &str, method: &str, has_self: bool) -> Spur {
        self.interner
            .get_or_intern(member_callable_name_for_owner(owner, method, has_self))
    }

    /// The handle for a member callable of an owner whose own spelling the
    /// shared space has already issued a handle for.
    ///
    /// Sharing the symbol space (ADR-0076) made the interner call a lookup
    /// rather than an insertion, but a lookup still needs the rendered text, so
    /// every body that mentions an owner re-rendered and re-hashed the same
    /// fifty-character member name. The join is a total function of the owner
    /// spelling, the member spelling, and the separator, so the generation's
    /// derived-spelling memo holds that association and only the first body to
    /// spell a member renders it.
    ///
    /// `owner_symbol` and `method_symbol` are the handles the space already
    /// issued for the two components. A caller without an owner handle falls
    /// back to rendering, which is what
    /// [`Self::member_callable_symbol_for_owner`] has always done — both arms
    /// spell through [`member_callable_name_for_owner`], the one renderer
    /// (RUE-1236).
    fn member_callable_symbol_for_issued_owner(
        &self,
        owner_symbol: Option<Spur>,
        owner: &str,
        method_symbol: Spur,
        method: &str,
        has_self: bool,
    ) -> Spur {
        let Some(owner_symbol) = owner_symbol else {
            return self.member_callable_symbol_for_owner(owner, method, has_self);
        };
        self.state.symbol_space().derived_symbol(
            owner_symbol,
            method_symbol,
            u8::from(has_self),
            || member_callable_name_for_owner(owner, method, has_self),
        )
    }

    /// The handle the shared space already holds for an owner's own spelling,
    /// which is what keys its members' derived spellings.
    ///
    /// This is deliberately a lookup and never an insertion: the retention
    /// counters are read from the interner's length and byte count, so a
    /// spelling the unmemoized path never interned must not appear because the
    /// memo wanted a key. An owner that is not already interned simply takes
    /// the rendering path.
    fn member_callable_owner_symbol(&self, owner: &str) -> Option<Spur> {
        self.interner.get(owner)
    }

    fn named_method_info_for_symbol(
        &self,
        struct_id: StructId,
        symbol: Spur,
    ) -> Option<MethodCallInfo> {
        if let Some(info) = self
            .named_method_infos
            .borrow()
            .get(&(struct_id, symbol))
            .copied()
        {
            return Some(info);
        }
        let owner_type = Type::new_struct(struct_id);
        let receiver_key = self
            .nominal_tokens
            .borrow()
            .get(&owner_type)
            .map(|(_, key)| key.clone())
            .or_else(|| self.endpoint.durable_named_identity(owner_type))?;
        let owner = self.source.definition_name(&receiver_key)?;
        let name = self.interner.resolve(&symbol);
        let (key, has_self) = self.source.named_member(&receiver_key, &owner, name)?;
        // Durable method signatures carry the exact accessor result mode.
        // Request-local RIR remains authoritative when the owner declaration
        // is present, preserving parity for bodies materialized from a slice.
        let mut info = self.calls.method_signature_info(&key)?;
        if let Some(method_ref) = self.rir_struct_method_decl(struct_id, symbol)
            && let InstData::FnDecl {
                returns_borrow,
                returns_inout,
                ..
            } = &self.rir.rir().get(method_ref).data
        {
            info.returns_borrow = *returns_borrow;
            info.returns_inout = *returns_inout;
        }
        let full_symbol = self.member_callable_symbol(struct_id, name, has_self);
        let token = self.endpoint.register_body_owner(
            key.clone(),
            self.owner_file,
            name,
            if has_self {
                crate::StableDefinitionKind::Method
            } else {
                crate::StableDefinitionKind::AssociatedFunction
            },
            Some(&owner),
        );
        self.function_tokens
            .borrow_mut()
            .insert(full_symbol, (token, key));
        self.named_method_infos
            .borrow_mut()
            .insert((struct_id, symbol), info);
        Some(info)
    }

    /// Locate a named method's `FnDecl` by walking the request-local RIR's
    /// struct declarations. The provider's request RIR carries the owning
    /// `StructDecl` (types are part of every consumer's inputs) even when the
    /// method's own body query is a different request, so this is the one
    /// resolution path that can recover declaration-level facts — like the
    /// `-> borrow T` accessor flag (ADR-0062) — that the durable signature
    /// subset does not carry.
    fn rir_struct_method_decl(&self, struct_id: StructId, method: Spur) -> Option<InstRef> {
        let struct_def = self.type_pool.struct_def(struct_id);
        // The RIR names structs by their source name; qualify by declaring
        // file below so same-named structs in sibling files cannot collide.
        // Translate lookups through strings so a distinct semantic interner
        // cannot skew the Spurs.
        let owner_file = struct_def.file_id;
        // A cross-file-unique pool name is `Source$escaped_file`; the RIR
        // declaration carries the bare source name ('$' cannot appear in a
        // source identifier).
        let source_name = struct_def
            .name
            .split('$')
            .next()
            .expect("split yields at least one segment");
        let owner_sym = self.rir.rir_interner().get(source_name)?;
        let method_sym = self
            .rir
            .rir_interner()
            .get(self.interner.resolve(&method))?;
        let rir = self.rir.rir();
        for index in 0..rir.len() {
            let inst_ref = InstRef::from_raw(index as u32);
            if let InstData::StructDecl { name, methods, .. } = &rir.get(inst_ref).data
                && *name == owner_sym
                && rir.get(inst_ref).span.file_id == owner_file
            {
                for method_ref in rir.struct_methods(methods) {
                    if let InstData::FnDecl { name, .. } = &rir.get(method_ref).data
                        && *name == method_sym
                    {
                        return Some(method_ref);
                    }
                }
            }
        }
        None
    }

    fn named_method_definition(&self, struct_id: StructId, symbol: Spur) -> Option<K> {
        if self
            .anonymous_methods
            .borrow()
            .contains_key(&(struct_id, symbol))
        {
            return None;
        }
        let info = self.named_method_info_for_symbol(struct_id, symbol)?;
        let full_symbol =
            self.member_callable_symbol(struct_id, self.interner.resolve(&symbol), info.has_self);
        self.function_tokens
            .borrow()
            .get(&full_symbol)
            .map(|(_, key)| key.clone())
    }

    fn method_info_for_symbol(&self, struct_id: StructId, name: Spur) -> Option<MethodCallInfo> {
        if let Some(info) = self
            .anonymous_methods
            .borrow()
            .get(&(struct_id, name))
            .copied()
        {
            return Some(self.recover_trusted_anonymous_accessor(struct_id, name, info));
        }
        if let Some(info) = self
            .endpoint
            .method_info(struct_id, name)
            .map(MethodCallInfo::from_body)
        {
            return Some(self.recover_trusted_anonymous_accessor(struct_id, name, info));
        }
        if let Some(info) = self.named_method_info_for_symbol(struct_id, name) {
            return Some(info);
        }
        let owner_type = Type::new_struct(struct_id);
        let identity = self
            .durable_anonymous_types
            .get(&owner_type)
            .cloned()
            .or_else(|| self.endpoint.durable_anonymous_identity(owner_type))?;
        let issued = self.register_and_issue_anonymous_identity(&identity)?;
        self.endpoint
            .register_anonymous_nominal(issued.clone(), identity.clone());
        self.register_provider_anonymous_method_endpoints_with_issued(
            &identity, owner_type, issued,
        )?;
        self.anonymous_methods
            .borrow()
            .get(&(struct_id, name))
            .copied()
            .map(|info| self.recover_trusted_anonymous_accessor(struct_id, name, info))
            .or_else(|| {
                self.endpoint
                    .method_info(struct_id, name)
                    .map(MethodCallInfo::from_body)
                    .map(|info| self.recover_trusted_anonymous_accessor(struct_id, name, info))
            })
    }

    fn recover_trusted_anonymous_accessor(
        &self,
        struct_id: StructId,
        name: Spur,
        mut info: MethodCallInfo,
    ) -> MethodCallInfo {
        if info.returns_borrow || info.returns_inout {
            return info;
        }
        let owner_type = Type::new_struct(struct_id);
        let durable = self
            .durable_anonymous_types
            .get(&owner_type)
            .cloned()
            .or_else(|| self.endpoint.durable_anonymous_identity(owner_type));
        if let Some(durable) = durable {
            let trusted = self
                .source
                .anonymous_definition_module(&durable)
                .is_some_and(|module| self.source.module_is_trusted_standard_library(&module));
            if trusted
                && let Some((returns_borrow, returns_inout)) = self
                    .source
                    .anonymous_method_return_modes(&durable, self.interner.resolve(&name))
            {
                info.returns_borrow = returns_borrow;
                info.returns_inout = returns_inout;
                return info;
            }
        }
        let definition = self.rir_struct_method_decl(struct_id, name);
        let Some(definition) = definition else {
            return info;
        };
        let trusted = self.endpoint_file_is_trusted_standard_library(
            self.type_pool.struct_def(struct_id).file_id.index(),
        );
        if !trusted {
            return info;
        }
        if let InstData::FnDecl {
            returns_borrow,
            returns_inout,
            ..
        } = &self.rir.rir().get(definition).data
        {
            info.returns_borrow = *returns_borrow;
            info.returns_inout = *returns_inout;
        }
        info
    }

    fn const_info_for_symbol(&self, file: FileId, symbol: Spur) -> Option<ConstInfo> {
        if let Some(info) = self.const_infos.borrow().get(&(file, symbol)).cloned() {
            return Some(info);
        }
        let name = self.interner.resolve(&symbol);
        let key = if file == self.owner_file {
            self.source.value_const(&self.key, name)?
        } else {
            let module = self.modules_by_file.borrow().get(&file)?.clone();
            self.source.qualified_value_const(&module, name)?
        };
        self.observed_named_definitions
            .borrow_mut()
            .insert(key.clone());
        let constant = self.source.constant(&key)?;
        let function = match &constant.value {
            crate::SemanticImportConstValue::Function(function) => Some(function.clone()),
            _ => None,
        };
        let span = if file == self.owner_file {
            self.current_callable_locator()?.2
        } else {
            Span::point_in_file(file, 0)
        };
        self.register_import_nominal_identities(&constant.ty).ok()?;
        if let crate::SemanticImportConstValue::Type(ty) = &constant.value {
            self.register_import_nominal_identities(ty).ok()?;
        }
        let mut info = self
            .state
            .identity_context()
            .pool_mut()?
            .resolve_const(&key, super::body_identity::ConstIdentityHandle { span })
            .ok()?;
        if let (
            crate::SemanticImportConstValue::Type(crate::SemanticImportType::AnonymousNominal(
                identity,
            )),
            ConstValue::Type(resolved),
        ) = (&constant.value, info.value)
        {
            self.register_provider_anonymous_method_endpoints(identity, resolved)?;
        }
        if let Some(function) = function {
            let source_symbol = info.value.as_function()?.spur();
            let alias_symbol =
                if let Some(module) = self.source.foreign_function_module(&self.key, &function) {
                    self.interner.get_or_intern(&format!(
                        "{}${}",
                        self.source.module_path(&module),
                        self.source.definition_name(&function)?
                    ))
                } else {
                    source_symbol
                };
            info.value = ConstValue::Function(alias_symbol.into());
            self.function_alias_keys
                .borrow_mut()
                .insert(alias_symbol, function);
        }
        self.const_infos
            .borrow_mut()
            .insert((file, symbol), info.clone());
        Some(info)
    }

    fn nominal_type_for_symbol(&self, file: FileId, symbol: Spur) -> Option<Type> {
        if let Some(id) = self.generated_structs.get(&symbol) {
            return Some(Type::new_struct(*id));
        }
        if let Some(id) = self.generated_enums.get(&symbol) {
            return Some(Type::new_enum(*id));
        }
        if let Some(id) = self.endpoint.endpoint_builtin_or_generated_struct(symbol) {
            return Some(Type::new_struct(id));
        }
        if let Some(id) = self.endpoint.endpoint_builtin_enum(symbol) {
            return Some(Type::new_enum(id));
        }
        let name = self.interner.resolve(&symbol);
        let (key, kind) = if file == self.owner_file {
            DurableBodyLookupSource::nominal(&self.source, &self.key, name)?
        } else {
            let module = self.modules_by_file.borrow().get(&file)?.clone();
            self.source.qualified_nominal(&module, name)?
        };
        let token = self
            .endpoint
            .register_named_nominal(key.clone(), file.index(), name, kind);
        let imported = crate::SemanticImportType::Nominal(key.clone());
        self.register_import_nominal_identities(&imported).ok()?;
        let ty = self
            .state
            .identity_context()
            .pool_mut()?
            .resolve_provider_type(&imported)
            .ok()?;
        match kind {
            crate::StableDefinitionKind::Struct if ty.as_struct().is_some() => {}
            crate::StableDefinitionKind::Enum if ty.as_enum().is_some() => {}
            _ => return None,
        }
        self.nominal_tokens.borrow_mut().insert(ty, (token, key));
        Some(ty)
    }

    fn ensure_named_nominal_identity(
        &self,
        ty: Type,
        name: &str,
    ) -> Result<(SemanticDefinitionToken, K), crate::SemanticBodyExportFailure> {
        if let Some(identity) = self.nominal_tokens.borrow().get(&ty).cloned() {
            return Ok(identity);
        }
        if let Some(key) = self.endpoint.durable_named_identity(ty) {
            let nominal = DurableNominalSource::nominal(&self.source, &key)
                .ok_or(crate::SemanticBodyExportFailure::MissingStableIdentity)?;
            let kind = match nominal.body {
                crate::DurableNominalBody::Struct { .. } => crate::StableDefinitionKind::Struct,
                crate::DurableNominalBody::Enum { .. } => crate::StableDefinitionKind::Enum,
            };
            let token = self.endpoint.register_named_nominal(
                key.clone(),
                self.owner_file.index(),
                nominal.name.as_ref(),
                kind,
            );
            self.nominal_tokens
                .borrow_mut()
                .insert(ty, (token, key.clone()));
            return Ok((token, key));
        }
        let Some((key, kind)) = DurableBodyLookupSource::nominal(&self.source, &self.key, name)
        else {
            return Err(crate::SemanticBodyExportFailure::MissingStableIdentity);
        };
        let token =
            self.endpoint
                .register_named_nominal(key.clone(), self.owner_file.index(), name, kind);
        self.nominal_tokens
            .borrow_mut()
            .insert(ty, (token, key.clone()));
        Ok((token, key))
    }

    fn materialize_type_instance(
        &mut self,
        value: &TypeInstanceKey<K, M>,
    ) -> Result<Type, crate::SemanticBodyExportFailure> {
        use crate::NominalInstanceKey as N;
        use crate::SemanticImportType as S;
        use crate::TypeInstanceKey as T;
        let import = match value {
            T::I8 => S::I8,
            T::I16 => S::I16,
            T::I32 => S::I32,
            T::I64 => S::I64,
            T::U8 => S::U8,
            T::U16 => S::U16,
            T::U32 => S::U32,
            T::U64 => S::U64,
            T::Bool => S::Bool,
            T::Unit => S::Unit,
            T::Never => S::Never,
            T::ComptimeType => S::ComptimeType,
            T::BuiltinNominal { kind, name } => S::BuiltinNominal {
                kind: match kind {
                    crate::AnonymousNominalKind::Struct => crate::SemanticImportNominalKind::Struct,
                    crate::AnonymousNominalKind::Enum => crate::SemanticImportNominalKind::Enum,
                },
                name: name.clone(),
            },
            T::Nominal(N::Builtin { kind, name }) => S::BuiltinNominal {
                kind: match kind {
                    crate::AnonymousNominalKind::Struct => crate::SemanticImportNominalKind::Struct,
                    crate::AnonymousNominalKind::Enum => crate::SemanticImportNominalKind::Enum,
                },
                name: name.clone(),
            },
            T::Nominal(N::Named(key)) => S::Nominal(key.clone()),
            T::Nominal(N::Anonymous(identity)) => {
                let ty = self
                    .install_canonical_anonymous_identity(identity)
                    .ok_or(crate::SemanticBodyExportFailure::MissingStableIdentity)?;
                return Ok(ty);
            }
            T::Array { element, len } => S::Array {
                element: Arc::new(self.type_instance_import(element)?),
                len: *len,
            },
            T::Slice { element, name } => S::Slice {
                element: Arc::new(self.type_instance_import(element)?),
                name: name.clone(),
            },
            T::PtrConst(inner) => S::PtrConst(Arc::new(self.type_instance_import(inner)?)),
            T::PtrMut(inner) => S::PtrMut(Arc::new(self.type_instance_import(inner)?)),
            T::Module(module) => {
                let (id, _) = self
                    .register_module_target(module.clone())
                    .ok_or(crate::SemanticBodyExportFailure::MissingStableIdentity)?;
                return Ok(Type::new_module(id));
            }
            T::GenericParameter(_) => {
                return Err(crate::SemanticBodyExportFailure::MissingStableIdentity);
            }
        };
        self.install_anonymous_dependencies(&import, &mut AHashSet::new())?;
        let ty = self
            .state
            .identity_context()
            .pool_mut()
            .ok_or(crate::SemanticBodyExportFailure::MissingStableIdentity)?
            .resolve_provider_type(&import)
            .map_err(|_| crate::SemanticBodyExportFailure::MissingStableIdentity)?;
        self.register_import_nominal_identities(&import)?;
        if let T::Nominal(N::Named(key)) = value {
            let nominal = DurableNominalSource::nominal(&self.source, key)
                .ok_or(crate::SemanticBodyExportFailure::MissingStableIdentity)?;
            let kind = match nominal.body {
                crate::DurableNominalBody::Struct { .. } => crate::StableDefinitionKind::Struct,
                crate::DurableNominalBody::Enum { .. } => crate::StableDefinitionKind::Enum,
            };
            let token = self.endpoint.register_named_nominal(
                key.clone(),
                self.owner_file.index(),
                nominal.name.as_ref(),
                kind,
            );
            self.nominal_tokens
                .borrow_mut()
                .insert(ty, (token, key.clone()));
        }
        Ok(ty)
    }

    fn install_anonymous_dependencies(
        &mut self,
        value: &crate::SemanticImportType<K, M>,
        visited_named: &mut AHashSet<K>,
    ) -> Result<(), crate::SemanticBodyExportFailure> {
        use crate::SemanticImportType as T;
        match value {
            T::AnonymousNominal(identity) => {
                self.install_canonical_anonymous_identity(identity)
                    .ok_or(crate::SemanticBodyExportFailure::MissingStableIdentity)?;
            }
            T::Nominal(key) if visited_named.insert(key.clone()) => {
                let nominal = DurableNominalSource::nominal(&self.source, key)
                    .ok_or(crate::SemanticBodyExportFailure::MissingStableIdentity)?;
                match nominal.body {
                    crate::DurableNominalBody::Struct { fields, .. } => {
                        for (_, field) in fields.iter() {
                            self.install_anonymous_dependencies(field, visited_named)?;
                        }
                    }
                    crate::DurableNominalBody::Enum { variants } => {
                        for (_, payload) in variants.iter() {
                            for field in payload.iter() {
                                self.install_anonymous_dependencies(field, visited_named)?;
                            }
                        }
                    }
                }
            }
            T::Array { element, .. }
            | T::Slice { element, .. }
            | T::PtrConst(element)
            | T::PtrMut(element) => {
                self.install_anonymous_dependencies(element, visited_named)?;
            }
            _ => {}
        }
        Ok(())
    }

    fn register_import_nominal_identities(
        &self,
        value: &crate::SemanticImportType<K, M>,
    ) -> Result<(), crate::SemanticBodyExportFailure> {
        self.provider_body_work
            .record(ProviderBodyWorkEvent::RegistrationRequest);
        let result = self.register_import_nominal_identities_inner(value);
        if result.is_err() {
            self.import_nominal_registrations.borrow_mut().clear();
        }
        result
    }

    fn register_import_nominal_identities_inner(
        &self,
        value: &crate::SemanticImportType<K, M>,
    ) -> Result<(), crate::SemanticBodyExportFailure> {
        use crate::SemanticImportType as T;
        self.provider_body_work
            .record(ProviderBodyWorkEvent::TypeVisit);
        match value {
            T::Nominal(key) => {
                self.provider_body_work
                    .record(ProviderBodyWorkEvent::NamedProbe);
                if let Some(token) = self.endpoint.registered_named_nominal_token(key) {
                    match self.import_nominal_registrations.borrow().get(&token) {
                        Some(ImportNominalRegistration::Complete) => {
                            self.provider_body_work
                                .record(ProviderBodyWorkEvent::CompleteHit);
                            return Ok(());
                        }
                        Some(ImportNominalRegistration::InProgress) => {
                            self.provider_body_work
                                .record(ProviderBodyWorkEvent::CycleHit);
                            return Ok(());
                        }
                        None => {}
                    }
                }

                let mut registration_token = None;
                let result = (|| {
                    let nominal = DurableNominalSource::nominal(&self.source, key)
                        .ok_or(crate::SemanticBodyExportFailure::MissingStableIdentity)?;
                    let kind = match &nominal.body {
                        crate::DurableNominalBody::Struct { .. } => {
                            crate::StableDefinitionKind::Struct
                        }
                        crate::DurableNominalBody::Enum { .. } => crate::StableDefinitionKind::Enum,
                    };
                    let ty = self
                        .state
                        .identity_context()
                        .pool_mut()
                        .ok_or(crate::SemanticBodyExportFailure::MissingStableIdentity)?
                        .resolve_provider_type(value)
                        .map_err(|_| crate::SemanticBodyExportFailure::MissingStableIdentity)?;
                    let token = self.endpoint.register_named_nominal(
                        key.clone(),
                        self.owner_file.index(),
                        nominal.name.as_ref(),
                        kind,
                    );
                    registration_token = Some(token);
                    self.import_nominal_registrations
                        .borrow_mut()
                        .insert(token, ImportNominalRegistration::InProgress);
                    self.nominal_tokens
                        .borrow_mut()
                        .insert(ty, (token, key.clone()));
                    match &nominal.body {
                        crate::DurableNominalBody::Struct { fields, .. } => {
                            for (_, field) in fields.iter() {
                                self.provider_body_work
                                    .record(ProviderBodyWorkEvent::TypeEdgeTraversed);
                                self.register_import_nominal_identities_inner(field)?;
                            }
                        }
                        crate::DurableNominalBody::Enum { variants } => {
                            for (_, payload) in variants.iter() {
                                for field in payload.iter() {
                                    self.provider_body_work
                                        .record(ProviderBodyWorkEvent::TypeEdgeTraversed);
                                    self.register_import_nominal_identities_inner(field)?;
                                }
                            }
                        }
                    }
                    Ok(())
                })();
                if let Some(token) = registration_token {
                    if result.is_ok() {
                        self.provider_body_work
                            .record(ProviderBodyWorkEvent::NamedRegistered);
                        self.import_nominal_registrations
                            .borrow_mut()
                            .insert(token, ImportNominalRegistration::Complete);
                    } else {
                        self.import_nominal_registrations
                            .borrow_mut()
                            .remove(&token);
                    }
                }
                result?;
            }
            T::Array { element, .. }
            | T::Slice { element, .. }
            | T::PtrConst(element)
            | T::PtrMut(element) => {
                self.provider_body_work
                    .record(ProviderBodyWorkEvent::TypeEdgeTraversed);
                self.register_import_nominal_identities_inner(element)?;
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
            | T::BuiltinNominal { .. }
            | T::Module(_)
            | T::GenericParameter(_) => {}
            T::AnonymousNominal(identity) => {
                // Importing the same anonymous nominal through several
                // signatures is common in a body request. The first visit
                // installs its identity and method endpoints; once that
                // succeeds, replaying the durable relocation walk only adds
                // registration work and cannot change the result.
                let ty = self
                    .endpoint
                    .mint_anonymous(identity)
                    .ok_or(crate::SemanticBodyExportFailure::MissingStableIdentity)?;
                if self.canonical_anonymous_types.contains_key(&ty)
                    || self.consulted_anonymous_types.borrow().contains_key(&ty)
                {
                    return Ok(());
                }
                let issued = self
                    .register_and_issue_anonymous_identity(identity)
                    .ok_or(crate::SemanticBodyExportFailure::MissingStableIdentity)?;
                self.endpoint
                    .register_anonymous_nominal(issued.clone(), identity.clone());
                self.consulted_anonymous_types
                    .borrow_mut()
                    .insert(ty, issued.clone());
                self.register_provider_anonymous_method_endpoints_with_issued(identity, ty, issued)
                    .ok_or(crate::SemanticBodyExportFailure::MissingStableIdentity)?;
                self.provider_body_work
                    .record(ProviderBodyWorkEvent::AnonymousRegistered);
            }
        }
        Ok(())
    }

    fn type_instance_import(
        &self,
        value: &TypeInstanceKey<K, M>,
    ) -> Result<crate::SemanticImportType<K, M>, crate::SemanticBodyExportFailure> {
        use crate::NominalInstanceKey as N;
        use crate::SemanticImportType as S;
        use crate::TypeInstanceKey as T;
        Ok(match value {
            T::I8 => S::I8,
            T::I16 => S::I16,
            T::I32 => S::I32,
            T::I64 => S::I64,
            T::U8 => S::U8,
            T::U16 => S::U16,
            T::U32 => S::U32,
            T::U64 => S::U64,
            T::Bool => S::Bool,
            T::Unit => S::Unit,
            T::Never => S::Never,
            T::ComptimeType => S::ComptimeType,
            T::BuiltinNominal { kind, name } | T::Nominal(N::Builtin { kind, name }) => {
                S::BuiltinNominal {
                    kind: match kind {
                        crate::AnonymousNominalKind::Struct => {
                            crate::SemanticImportNominalKind::Struct
                        }
                        crate::AnonymousNominalKind::Enum => crate::SemanticImportNominalKind::Enum,
                    },
                    name: name.clone(),
                }
            }
            T::Nominal(N::Named(key)) => S::Nominal(key.clone()),
            T::Nominal(N::Anonymous(identity)) => S::AnonymousNominal((**identity).clone()),
            T::Array { element, len } => S::Array {
                element: Arc::new(self.type_instance_import(element)?),
                len: *len,
            },
            T::Slice { element, name } => S::Slice {
                element: Arc::new(self.type_instance_import(element)?),
                name: name.clone(),
            },
            T::PtrConst(inner) => S::PtrConst(Arc::new(self.type_instance_import(inner)?)),
            T::PtrMut(inner) => S::PtrMut(Arc::new(self.type_instance_import(inner)?)),
            T::Module(module) => S::Module(module.clone()),
            T::GenericParameter(index) => S::GenericParameter(*index),
        })
    }

    fn durable_type_from_concrete(&self, ty: Type) -> Option<crate::SemanticImportType<K, M>> {
        use crate::SemanticImportType as T;
        Some(match ty.kind() {
            TypeKind::I8 => T::I8,
            TypeKind::I16 => T::I16,
            TypeKind::I32 => T::I32,
            TypeKind::I64 => T::I64,
            TypeKind::U8 => T::U8,
            TypeKind::U16 => T::U16,
            TypeKind::U32 => T::U32,
            TypeKind::U64 => T::U64,
            TypeKind::Bool => T::Bool,
            TypeKind::Unit => T::Unit,
            TypeKind::Never => T::Never,
            TypeKind::ComptimeType => T::ComptimeType,
            TypeKind::Array(id) => {
                let (element, len) = self.type_pool.array_def(id);
                T::Array {
                    element: Arc::new(self.durable_type_from_concrete(element)?),
                    len,
                }
            }
            TypeKind::PtrConst(id) => T::PtrConst(Arc::new(
                self.durable_type_from_concrete(self.type_pool.ptr_const_def(id))?,
            )),
            TypeKind::PtrMut(id) => T::PtrMut(Arc::new(
                self.durable_type_from_concrete(self.type_pool.ptr_mut_def(id))?,
            )),
            TypeKind::Struct(_) | TypeKind::Enum(_) => {
                if let Some(identity) = self.durable_anonymous_types.get(&ty).cloned() {
                    T::AnonymousNominal(identity)
                } else if let Some(identity) = self.endpoint.durable_anonymous_identity(ty) {
                    T::AnonymousNominal(identity)
                } else {
                    let (_, definition) = self.nominal_tokens.borrow().get(&ty)?.clone();
                    T::Nominal(definition)
                }
            }
            TypeKind::Module(id) => {
                let (_, module) = self.module_tokens.borrow().get(&id)?.clone();
                T::Module(module)
            }
            TypeKind::Error => return None,
        })
    }

    fn durable_value_from_concrete(
        &self,
        value: ConstValue,
    ) -> Option<crate::SemanticImportConstValue<K, M>> {
        use crate::SemanticImportConstValue as V;
        Some(match value {
            ConstValue::Integer(value) => V::Integer(value),
            ConstValue::Bool(value) => V::Bool(value),
            ConstValue::Type(ty) => V::Type(self.durable_type_from_concrete(ty)?),
            ConstValue::Function(symbol) => {
                let (_, definition) = self.function_token_for_symbol(symbol.spur())?;
                V::Function(definition)
            }
            ConstValue::Unit => V::Unit,
            ConstValue::String(symbol) => {
                V::String(Arc::from(self.interner.resolve(&symbol.spur())))
            }
        })
    }

    fn materialize_durable_type(&mut self, ty: &crate::SemanticImportType<K, M>) -> Option<Type> {
        self.register_import_nominal_identities(ty).ok()?;
        let resolved = self
            .state
            .identity_context()
            .pool_mut()?
            .resolve_provider_type(ty)
            .ok()?;
        if let crate::SemanticImportType::AnonymousNominal(identity) = ty {
            self.install_provider_anonymous_methods(identity, resolved)?;
        }
        Some(resolved)
    }

    fn materialize_durable_const_value(
        &mut self,
        value: &crate::SemanticImportConstValue<K, M>,
    ) -> Option<ConstValue> {
        use crate::SemanticImportConstValue as V;
        Some(match value {
            V::Integer(value) => ConstValue::Integer(*value),
            V::Bool(value) => ConstValue::Bool(*value),
            V::Type(ty) => ConstValue::Type(self.materialize_durable_type(ty)?),
            V::Function(definition) => ConstValue::Function(
                self.interner
                    .get_or_intern(&self.source.definition_name(definition)?)
                    .into(),
            ),
            V::Unit => ConstValue::Unit,
            V::String(value) => {
                ConstValue::String(self.interner.get_or_intern(value.as_ref()).into())
            }
        })
    }

    fn produced_anonymous_nominals(
        &self,
        initial: &AHashSet<super::anon_structs::IssuedAnonymousNominalKey>,
    ) -> Result<Arc<[crate::SemanticProducedAnonymousNominal]>, crate::SemanticBodyExportFailure>
    {
        fn mode(value: RirParamMode) -> crate::SemanticParameterMode {
            match value {
                RirParamMode::Normal => crate::SemanticParameterMode::Value,
                RirParamMode::Borrow => crate::SemanticParameterMode::Borrow,
                RirParamMode::Inout => crate::SemanticParameterMode::Inout,
            }
        }
        fn method_type<P, S, K, M>(
            host: &ProviderBodyHost<'_, P, S, K, M>,
            ty: &super::AnonMethodType,
            owner: &super::anon_structs::IssuedAnonymousNominalKey,
        ) -> Result<crate::SemanticProducedAnonymousMethodType, crate::SemanticBodyExportFailure>
        where
            P: BodyFactProvider,
            S: DurableNominalSource<K, M>
                + DurableAnonymousSource<K, M>
                + DurableCallableSource<K, M>
                + DurableConstSource<K, M>
                + DurableBodyLookupSource<K, M>,
            K: Clone + Eq + Hash + Ord,
            M: Clone + Eq + Hash + Ord,
        {
            fn concrete<P, S, K, M>(
                host: &ProviderBodyHost<'_, P, S, K, M>,
                ty: &super::AnonMethodType,
                owner: &super::anon_structs::IssuedAnonymousNominalKey,
            ) -> Result<
                TypeInstanceKey<SemanticDefinitionToken, SemanticModuleToken>,
                crate::SemanticBodyExportFailure,
            >
            where
                P: BodyFactProvider,
                S: DurableNominalSource<K, M>
                    + DurableAnonymousSource<K, M>
                    + DurableCallableSource<K, M>
                    + DurableConstSource<K, M>
                    + DurableBodyLookupSource<K, M>,
                K: Clone + Eq + Hash + Ord,
                M: Clone + Eq + Hash + Ord,
            {
                Ok(match ty {
                    super::AnonMethodType::SelfType => TypeInstanceKey::Nominal(
                        crate::NominalInstanceKey::Anonymous(Node::new(owner.clone())),
                    ),
                    super::AnonMethodType::Concrete(ty) => host.canonical_type_instance(*ty)?,
                    super::AnonMethodType::Syntax(_) => {
                        return Err(crate::SemanticBodyExportFailure::UnsupportedType);
                    }
                })
            }
            Ok(match ty {
                super::AnonMethodType::SelfType => {
                    crate::SemanticProducedAnonymousMethodType::SelfType
                }
                _ => {
                    crate::SemanticProducedAnonymousMethodType::Concrete(concrete(host, ty, owner)?)
                }
            })
        }

        let mut identities = self
            .canonical_anonymous_types
            .iter()
            .filter(|(_, identity)| !initial.contains(*identity))
            .map(|(ty, identity)| (*ty, identity.clone()))
            .collect::<Vec<_>>();
        identities
            .sort_by(|(_, left), (_, right)| super::anon_structs::anonymous_key_cmp(left, right));
        identities
            .into_iter()
            .map(|(ty, identity)| {
                let (shape, type_captures, value_captures) = match ty.kind() {
                    TypeKind::Struct(struct_id) => {
                        let definition = self.type_pool.struct_def(struct_id);
                        let fields = definition
                            .fields
                            .iter()
                            .map(|field| {
                                Ok((
                                    Arc::from(field.name.as_str()),
                                    self.canonical_type_instance(field.ty)?,
                                ))
                            })
                            .collect::<Result<Vec<_>, crate::SemanticBodyExportFailure>>()?;
                        let methods = self
                            .anon_struct_method_sigs
                            .get(&struct_id)
                            .into_iter()
                            .flat_map(|methods| methods.iter())
                            .map(|method| {
                                Ok(crate::SemanticProducedAnonymousMethodSignature {
                                    name: Arc::from(self.interner.resolve(&method.name)),
                                    has_self: method.has_self,
                                    self_mode: mode(method.self_mode),
                                    returns_borrow: method.returns_borrow,
                                    returns_inout: method.returns_inout,
                                    parameters: method
                                        .param_types
                                        .iter()
                                        .zip(&method.param_modes)
                                        .zip(&method.param_comptime)
                                        .map(|((ty, parameter_mode), comptime)| {
                                            Ok((
                                                method_type(self, ty, &identity)?,
                                                mode(*parameter_mode),
                                                *comptime,
                                            ))
                                        })
                                        .collect::<Result<Vec<_>, _>>()?
                                        .into(),
                                    result: method_type(self, &method.return_type, &identity)?,
                                })
                            })
                            .collect::<Result<Vec<_>, crate::SemanticBodyExportFailure>>()?;
                        let mut type_captures: Vec<(
                            Arc<str>,
                            TypeInstanceKey<SemanticDefinitionToken, SemanticModuleToken>,
                        )> = self
                            .anon_struct_type_subst
                            .get(&struct_id)
                            .into_iter()
                            .flat_map(|captures| captures.iter())
                            .map(|(name, ty)| {
                                Ok((
                                    Arc::from(self.interner.resolve(name)),
                                    self.canonical_type_instance(*ty)?,
                                ))
                            })
                            .collect::<Result<Vec<_>, crate::SemanticBodyExportFailure>>()?;
                        type_captures.sort_by(|left, right| left.0.cmp(&right.0));
                        let mut value_captures: Vec<(
                            Arc<str>,
                            CanonicalArgumentValue<SemanticDefinitionToken, SemanticModuleToken>,
                        )> = self
                            .anon_struct_captured_values
                            .get(&struct_id)
                            .into_iter()
                            .flat_map(|captures| captures.iter())
                            .map(|(name, value)| {
                                Ok((
                                    Arc::from(self.interner.resolve(name)),
                                    self.canonical_argument_value(*value)?,
                                ))
                            })
                            .collect::<Result<Vec<_>, crate::SemanticBodyExportFailure>>()?;
                        value_captures.sort_by(|left, right| left.0.cmp(&right.0));
                        (
                            crate::SemanticProducedAnonymousNominalShape::Struct {
                                fields: fields.into(),
                                methods: methods.into(),
                            },
                            type_captures,
                            value_captures,
                        )
                    }
                    TypeKind::Enum(enum_id) => {
                        let definition = self.type_pool.enum_def(enum_id);
                        let variants = definition
                            .variants
                            .iter()
                            .enumerate()
                            .map(|(index, name)| {
                                Ok((
                                    name.clone(),
                                    definition
                                        .variant_payload(index)
                                        .iter()
                                        .map(|ty| self.canonical_type_instance(*ty))
                                        .collect::<Result<Vec<_>, _>>()?
                                        .into(),
                                ))
                            })
                            .collect::<Result<Vec<_>, crate::SemanticBodyExportFailure>>()?;
                        (
                            crate::SemanticProducedAnonymousNominalShape::Enum {
                                variants: variants.into(),
                            },
                            Vec::new(),
                            Vec::new(),
                        )
                    }
                    _ => return Err(crate::SemanticBodyExportFailure::UnsupportedType),
                };
                Ok(crate::SemanticProducedAnonymousNominal {
                    identity,
                    shape,
                    type_captures: type_captures.into(),
                    value_captures: value_captures.into(),
                })
            })
            .collect::<Result<Vec<_>, _>>()
            .map(Arc::from)
    }

    fn install_provider_anonymous_methods(
        &mut self,
        identity: &crate::AnonymousNominalKey<K, M>,
        owner_type: Type,
    ) -> Option<()> {
        let issued_identity = self.register_and_issue_anonymous_identity(identity)?;
        self.install_provider_anonymous_methods_with_issued(identity, owner_type, issued_identity)
    }

    fn install_provider_anonymous_methods_with_issued(
        &mut self,
        identity: &crate::AnonymousNominalKey<K, M>,
        owner_type: Type,
        issued_identity: super::anon_structs::IssuedAnonymousNominalKey,
    ) -> Option<()> {
        self.endpoint
            .register_anonymous_nominal(issued_identity.clone(), identity.clone());
        self.canonical_anonymous_types
            .insert(owner_type, issued_identity.clone());
        self.durable_anonymous_types
            .insert(owner_type, identity.clone());
        if let Some(enum_id) = owner_type.as_enum() {
            self.anon_enum_identities.insert(issued_identity, enum_id);
            return Some(());
        }
        let Some(struct_id) = owner_type.as_struct() else {
            return Some(());
        };
        self.anon_struct_identities
            .insert(issued_identity.clone(), struct_id);
        let methods = self.source.anonymous_methods(identity);
        if methods.is_empty() {
            return Some(());
        }
        let mut infos = Vec::with_capacity(methods.len());
        let mut signatures = Vec::with_capacity(methods.len());
        let owner_name = self.member_callable_owner(struct_id);
        let owner_symbol = self.member_callable_owner_symbol(&owner_name);
        for method in methods {
            let resolve = |ty: &crate::DurableAnonymousMethodType<K, M>| {
                Some(match ty {
                    crate::DurableAnonymousMethodType::SelfType => owner_type,
                    crate::DurableAnonymousMethodType::Concrete(ty) => self
                        .state
                        .identity_context()
                        .pool_mut()?
                        .resolve_provider_type(ty)
                        .ok()?,
                })
            };
            let parameter_types = method
                .parameters
                .iter()
                .map(|(ty, _, _)| resolve(ty))
                .collect::<Option<Vec<_>>>()?;
            let return_type = resolve(&method.result)?;
            let name = self.interner.get_or_intern(method.name.as_ref());
            let callable = self.member_callable_symbol_for_issued_owner(
                owner_symbol,
                &owner_name,
                name,
                &method.name,
                method.has_self,
            );
            let kind = if method.name.as_ref() == "__drop" {
                crate::AnonymousMemberKind::Destructor
            } else if method.has_self {
                crate::AnonymousMemberKind::Method
            } else {
                crate::AnonymousMemberKind::AssociatedFunction
            };
            self.anonymous_function_identities.borrow_mut().insert(
                callable,
                FunctionInstanceKey::AnonymousMember {
                    owner: Node::new(TypeInstanceKey::Nominal(
                        crate::NominalInstanceKey::Anonymous(Node::new(issued_identity.clone())),
                    )),
                    member: crate::AnonymousMemberKey {
                        kind,
                        name: method.name.clone(),
                    },
                },
            );
            // Comptime type expressions may be reduced once during inference
            // and again during instruction analysis. The exact anonymous
            // identity joins both reads, so its callable overlay is idempotent.
            if self.endpoint.method_info(struct_id, name).is_some() {
                continue;
            }
            let params = self.state.allocate_params(
                (0..parameter_types.len())
                    .map(|index| intern_synthetic_argument_name(&self.interner, index)),
                parameter_types.iter().copied(),
                method.parameters.iter().map(|(_, mode, _)| *mode),
                method.parameters.iter().map(|(_, _, comptime)| *comptime),
            );
            let returns_borrow = method.returns_borrow;
            let returns_inout = method.returns_inout;
            let trusted_std_accessor = self
                .source
                .anonymous_definition_module(identity)
                .is_some_and(|module| self.source.module_is_trusted_standard_library(&module));
            // Consumer analysis needs only the signature. The producer-owned
            // anonymous-member transaction later lowers and analyzes the exact
            // body fragment; these locators are therefore deliberately opaque
            // compatibility fields in MethodInfo and are never followed here.
            infos.push((
                (struct_id, name),
                MethodCallInfo {
                    struct_type: owner_type,
                    has_self: method.has_self,
                    self_mode: method.self_mode,
                    params,
                    return_type,
                    returns_borrow: trusted_std_accessor && returns_borrow,
                    returns_inout: trusted_std_accessor && returns_inout,
                },
            ));
            signatures.push(super::AnonMethodSig {
                name,
                has_self: method.has_self,
                self_mode: method.self_mode,
                returns_borrow,
                returns_inout,
                param_types: parameter_types
                    .into_iter()
                    .map(super::AnonMethodType::Concrete)
                    .collect(),
                param_modes: method.parameters.iter().map(|(_, mode, _)| *mode).collect(),
                param_comptime: method
                    .parameters
                    .iter()
                    .map(|(_, _, comptime)| *comptime)
                    .collect(),
                return_type: super::AnonMethodType::Concrete(return_type),
            });
        }
        self.anonymous_methods.borrow_mut().extend(infos);
        if !signatures.is_empty() {
            self.anon_struct_method_sigs.insert(struct_id, signatures);
        }
        self.anonymous_struct_ids.insert(struct_id);
        Some(())
    }

    fn register_provider_anonymous_method_endpoints(
        &self,
        identity: &crate::AnonymousNominalKey<K, M>,
        owner_type: Type,
    ) -> Option<()> {
        let issued_identity = self.issue_consulted_anonymous_identity(identity)?;
        self.register_provider_anonymous_method_endpoints_with_issued(
            identity,
            owner_type,
            issued_identity,
        )
    }

    fn register_provider_anonymous_method_endpoints_with_issued(
        &self,
        identity: &crate::AnonymousNominalKey<K, M>,
        owner_type: Type,
        issued_identity: super::anon_structs::IssuedAnonymousNominalKey,
    ) -> Option<()> {
        if self
            .anonymous_method_registrations
            .borrow()
            .contains(&owner_type)
        {
            return Some(());
        }
        let result = self.register_provider_anonymous_method_endpoints_inner(
            identity,
            owner_type,
            issued_identity,
        );
        if result.is_some() {
            self.anonymous_method_registrations
                .borrow_mut()
                .insert(owner_type);
        }
        result
    }

    fn register_provider_anonymous_method_endpoints_inner(
        &self,
        identity: &crate::AnonymousNominalKey<K, M>,
        owner_type: Type,
        issued_identity: super::anon_structs::IssuedAnonymousNominalKey,
    ) -> Option<()> {
        let Some(struct_id) = owner_type.as_struct() else {
            return Some(());
        };
        let methods = self.source.anonymous_methods(identity);
        if methods.is_empty() {
            return Some(());
        }
        self.anonymous_function_identities
            .borrow_mut()
            .reserve(methods.len());
        self.anonymous_methods.borrow_mut().reserve(methods.len());
        let owner_name = self.member_callable_owner(struct_id);
        let owner_symbol = self.member_callable_owner_symbol(&owner_name);
        let trusted_std_accessor_owner = self
            .source
            .anonymous_definition_module(identity)
            .is_some_and(|module| self.source.module_is_trusted_standard_library(&module));
        for method in methods {
            let name = self.interner.get_or_intern(method.name.as_ref());
            let callable = self.member_callable_symbol_for_issued_owner(
                owner_symbol,
                &owner_name,
                name,
                &method.name,
                method.has_self,
            );
            let kind = if method.name.as_ref() == "__drop" {
                crate::AnonymousMemberKind::Destructor
            } else if method.has_self {
                crate::AnonymousMemberKind::Method
            } else {
                crate::AnonymousMemberKind::AssociatedFunction
            };
            self.anonymous_function_identities.borrow_mut().insert(
                callable,
                FunctionInstanceKey::AnonymousMember {
                    owner: Node::new(TypeInstanceKey::Nominal(
                        crate::NominalInstanceKey::Anonymous(Node::new(issued_identity.clone())),
                    )),
                    member: crate::AnonymousMemberKey {
                        kind,
                        name: method.name.clone(),
                    },
                },
            );
            if self.endpoint.method_info(struct_id, name).is_some() {
                continue;
            }
            self.anonymous_methods.borrow_mut().insert(
                (struct_id, name),
                MethodCallInfo {
                    struct_type: owner_type,
                    has_self: method.has_self,
                    self_mode: method.self_mode,
                    // Anonymous accessors remain forbidden for user types.
                    // The durable std owner identity is the narrow exception,
                    // and its second-class result mode must survive every
                    // provider endpoint just like the direct declaration path.
                    returns_borrow: trusted_std_accessor_owner && method.returns_borrow,
                    returns_inout: trusted_std_accessor_owner && method.returns_inout,
                    params: self.state.allocate_params(
                        (0..method.parameters.len())
                            .map(|index| intern_synthetic_argument_name(&self.interner, index)),
                        method
                            .parameters
                            .iter()
                            .map(|(ty, _, _)| match ty {
                                crate::DurableAnonymousMethodType::SelfType => Some(owner_type),
                                crate::DurableAnonymousMethodType::Concrete(ty) => self
                                    .state
                                    .identity_context()
                                    .pool_mut()?
                                    .resolve_provider_type(ty)
                                    .ok(),
                            })
                            .collect::<Option<Vec<_>>>()?,
                        method.parameters.iter().map(|(_, mode, _)| *mode),
                        method.parameters.iter().map(|(_, _, comptime)| *comptime),
                    ),
                    return_type: match &method.result {
                        crate::DurableAnonymousMethodType::SelfType => owner_type,
                        crate::DurableAnonymousMethodType::Concrete(ty) => self
                            .state
                            .identity_context()
                            .pool_mut()?
                            .resolve_provider_type(ty)
                            .ok()?,
                    },
                },
            );
        }
        Some(())
    }

    fn materialize_argument_value(
        &mut self,
        value: &CanonicalArgumentValue<K, M>,
    ) -> Result<ConstValue, crate::SemanticBodyExportFailure> {
        Ok(match value {
            CanonicalArgumentValue::Integer(value) => ConstValue::Integer(*value),
            CanonicalArgumentValue::Bool(value) => ConstValue::Bool(*value),
            CanonicalArgumentValue::Type(ty) => {
                ConstValue::Type(self.materialize_type_instance(ty)?)
            }
            CanonicalArgumentValue::Unit => ConstValue::Unit,
            CanonicalArgumentValue::String(value) => {
                ConstValue::String(self.interner.get_or_intern(value.as_ref()).into())
            }
            CanonicalArgumentValue::Function(function) => {
                let FunctionInstanceKey::Definition(definition) = function.as_ref() else {
                    return Err(crate::SemanticBodyExportFailure::MissingStableIdentity);
                };
                let name = self
                    .source
                    .definition_name(definition)
                    .ok_or(crate::SemanticBodyExportFailure::MissingStableIdentity)?;
                let symbol = self.interner.get_or_intern(&name);
                let token =
                    self.endpoint
                        .register_function(definition.clone(), self.owner_file, &name);
                self.function_tokens
                    .borrow_mut()
                    .insert(symbol, (token, definition.clone()));
                ConstValue::Function(symbol.into())
            }
        })
    }
}

impl<P, S, K, M> BodyEndpointProvider for ProviderBodyHost<'_, P, S, K, M>
where
    P: BodyFactProvider,
    S: DurableNominalSource<K, M>
        + DurableAnonymousSource<K, M>
        + DurableCallableSource<K, M>
        + DurableConstSource<K, M>
        + DurableBodyLookupSource<K, M>,
    K: Clone + Eq + Hash + Ord,
    M: Clone + Eq + Hash + Ord,
{
    fn endpoint_name_symbol(&self, name: &str) -> Option<Spur> {
        self.endpoint.endpoint_name_symbol(name)
    }
    fn endpoint_definition_endpoint(
        &self,
        token: SemanticDefinitionToken,
    ) -> Option<crate::SemanticDefinitionEndpoint> {
        self.endpoint.endpoint_definition_endpoint(token)
    }
    fn endpoint_module_endpoint(
        &self,
        token: SemanticModuleToken,
    ) -> Option<crate::SemanticModuleEndpoint> {
        self.endpoint.endpoint_module_endpoint(token)
    }
    fn endpoint_struct_by_file_name(&self, file: FileId, name: Spur) -> Option<StructId> {
        self.nominal_type_for_symbol(file, name)?.as_struct()
    }
    fn endpoint_enum_by_file_name(&self, file: FileId, name: Spur) -> Option<EnumId> {
        self.nominal_type_for_symbol(file, name)?.as_enum()
    }
    fn endpoint_builtin_or_generated_struct(&self, name: Spur) -> Option<StructId> {
        self.endpoint.endpoint_builtin_or_generated_struct(name)
    }
    fn endpoint_generated_struct(&self, name: Spur) -> Option<StructId> {
        self.endpoint.endpoint_generated_struct(name)
    }
    fn endpoint_builtin_enum(&self, name: Spur) -> Option<EnumId> {
        self.endpoint.endpoint_builtin_enum(name)
    }
    fn endpoint_anon_struct(
        &self,
        identity: &super::anon_structs::IssuedAnonymousNominalKey,
    ) -> Option<StructId> {
        self.endpoint.endpoint_anon_struct(identity)
    }
    fn endpoint_anon_enum(
        &self,
        identity: &super::anon_structs::IssuedAnonymousNominalKey,
    ) -> Option<EnumId> {
        self.endpoint.endpoint_anon_enum(identity)
    }
    fn endpoint_function_info(&self, name: Spur) -> Option<FunctionInfo> {
        self.endpoint.endpoint_function_info(name)
    }
    fn endpoint_source_function_name(&self, name: Spur) -> Spur {
        self.endpoint.endpoint_source_function_name(name)
    }
    fn endpoint_module_id_for_file(&self, file: u32) -> Option<ModuleId> {
        self.endpoint.endpoint_module_id_for_file(file)
    }
    fn endpoint_module_is_trusted_standard_library(&self, module: ModuleId) -> bool {
        self.endpoint
            .endpoint_module_is_trusted_standard_library(module)
    }
    fn endpoint_file_is_trusted_standard_library(&self, file: u32) -> bool {
        if FileId::new(file) == self.owner_file {
            return self
                .source
                .definition_module(&self.key)
                .is_some_and(|module| self.source.module_is_trusted_standard_library(&module));
        }
        self.endpoint_module_id_for_file(file)
            .is_some_and(|module| self.endpoint_module_is_trusted_standard_library(module))
    }
    fn endpoint_intern_array(&self, element: Type, len: u64) -> Option<Type> {
        self.type_pool.try_intern_array(element, len).ok()
    }
    fn endpoint_intern_ptr_const(&self, pointee: Type) -> Option<Type> {
        self.type_pool.try_intern_ptr_const(pointee).ok()
    }
    fn endpoint_intern_ptr_mut(&self, pointee: Type) -> Option<Type> {
        self.type_pool.try_intern_ptr_mut(pointee).ok()
    }
}

impl<P, S, K, M> CallResolutionFacts for ProviderBodyHost<'_, P, S, K, M>
where
    P: BodyFactProvider,
    S: DurableNominalSource<K, M>
        + DurableAnonymousSource<K, M>
        + DurableCallableSource<K, M>
        + DurableConstSource<K, M>
        + DurableBodyLookupSource<K, M>,
    K: Clone + Eq + Hash + Ord,
    M: Clone + Eq + Hash + Ord,
{
    fn call_function_info(&self, name: Spur) -> Option<FunctionCallInfo> {
        self.function_info_for_symbol(name)
    }
    fn call_source_function_name(&self, name: Spur) -> Spur {
        self.endpoint.endpoint_source_function_name(name)
    }
    fn call_resolve_function_name_local(&self, name: Spur, file: FileId) -> Option<Spur> {
        self.function_for_file_symbol(file, name)
    }
    fn call_resolve_const_info_in_file(&self, name: Spur, file: FileId) -> Option<ConstInfo> {
        self.const_info_for_symbol(file, name)
    }
    fn call_value_const(&self, file: FileId, name: Spur) -> Option<ConstInfo> {
        self.const_info_for_symbol(file, name)
    }
    fn call_module_binding(&self, file: FileId, name: Spur) -> Option<ConstInfo> {
        self.module_binding_for_symbol(file, name)
    }
    fn call_method_info(&self, struct_id: StructId, name: Spur) -> Option<MethodCallInfo> {
        self.method_info_for_symbol(struct_id, name)
    }
    fn call_module_def(&self, module: ModuleId) -> ModuleDef {
        self.calls
            .module_def(module)
            .expect("provider body module must be registered before use")
    }
}

impl<P, S, K, M> AggregateFactsTrait for ProviderBodyHost<'_, P, S, K, M>
where
    P: BodyFactProvider,
    S: DurableNominalSource<K, M>
        + DurableAnonymousSource<K, M>
        + DurableCallableSource<K, M>
        + DurableConstSource<K, M>
        + DurableBodyLookupSource<K, M>,
    K: Clone + Eq + Hash + Ord,
    M: Clone + Eq + Hash + Ord,
{
    fn aggregate_value_const(&self, file: FileId, name: Spur) -> Option<ConstInfo> {
        self.const_info_for_symbol(file, name)
    }
    fn aggregate_module_binding(&self, file: FileId, name: Spur) -> Option<ConstInfo> {
        self.module_binding_for_symbol(file, name)
    }
    fn aggregate_struct_in_file(&self, file: FileId, name: Spur) -> Option<StructId> {
        self.nominal_type_for_symbol(file, name)?.as_struct()
    }
    fn aggregate_enum_in_file(&self, file: FileId, name: Spur) -> Option<EnumId> {
        self.nominal_type_for_symbol(file, name)?.as_enum()
    }
    fn aggregate_builtin_struct(&self, name: Spur) -> Option<StructId> {
        AggregateFactsTrait::aggregate_builtin_struct(&self.aggregate, name)
    }
    fn aggregate_builtin_enum(&self, name: Spur) -> Option<EnumId> {
        AggregateFactsTrait::aggregate_builtin_enum(&self.aggregate, name)
    }
    fn aggregate_module(
        &self,
        module: ModuleId,
    ) -> super::aggregate_resolution::AggregateModuleFact {
        AggregateFactsTrait::aggregate_module(&self.aggregate, module)
    }
    fn aggregate_file_path(&self, file: FileId) -> Option<&str> {
        AggregateFactsTrait::aggregate_file_path(&self.aggregate, file)
    }
    fn aggregate_source_path(&self, span: Span) -> Option<&str> {
        AggregateFactsTrait::aggregate_source_path(&self.aggregate, span)
    }
    fn aggregate_visibility_domain(&self, file: FileId) -> crate::SemanticVisibilityDomain {
        self.source.source_path(file).map_or_else(
            || AggregateFactsTrait::aggregate_visibility_domain(&self.aggregate, file),
            |path| crate::SemanticVisibilityDomain::from_file_path(Some(&path)),
        )
    }
}

fn provider_infer_type(pool: &TypeInternPool, ty: Type) -> crate::inference::InferType {
    match ty.kind() {
        TypeKind::Array(id) => {
            let (element, length) = pool.array_def(id);
            crate::inference::InferType::Array {
                element: Box::new(provider_infer_type(pool, element)),
                length,
            }
        }
        _ => crate::inference::InferType::Concrete(ty),
    }
}

impl<P, S, K, M> InferenceFactSource for ProviderBodyHost<'_, P, S, K, M>
where
    P: BodyFactProvider,
    S: DurableNominalSource<K, M>
        + DurableAnonymousSource<K, M>
        + DurableCallableSource<K, M>
        + DurableConstSource<K, M>
        + DurableBodyLookupSource<K, M>,
    K: Clone + Eq + Hash + Ord,
    M: Clone + Eq + Hash + Ord,
{
    fn inference_generated_nominal_overlays(&self) -> InferenceGeneratedNominalOverlays {
        let mut builtin_struct_types = AHashMap::new();
        let mut struct_types_by_file = AHashMap::new();
        for (name, id) in &self.generated_structs {
            let def = self.type_pool.struct_def(*id);
            if def.is_builtin {
                builtin_struct_types.insert(*name, Type::new_struct(*id));
            }
            struct_types_by_file.insert((def.file_id, *name), Type::new_struct(*id));
        }
        let mut enum_types_by_file = AHashMap::new();
        for (name, id) in &self.generated_enums {
            let def = self.type_pool.enum_def(*id);
            enum_types_by_file.insert((def.file_id, *name), Type::new_enum(*id));
        }
        InferenceGeneratedNominalOverlays {
            builtin_struct_types,
            struct_types_by_file,
            enum_types_by_file,
        }
    }
    fn uncached_function_sig(&self, name: Spur) -> Option<FunctionSig> {
        let info = self.function_info_for_symbol(name)?;
        let params = self.state.param_data(info.params);
        let param_type_syntax = (0..params.types().len())
            .map(|index| {
                <Self as OrdinaryBodyAnalysisHost>::function_param_type_syntax(self, &info, index)
            })
            .collect();
        let return_type_syntax =
            <Self as OrdinaryBodyAnalysisHost>::function_return_type_syntax(self, &info);
        let param_comptime_type = self
            .durable_comptime_type_flags
            .borrow()
            .get(&info.params)
            .cloned()
            .unwrap_or_else(|| {
                params
                    .types()
                    .iter()
                    .map(|ty| *ty == Type::COMPTIME_TYPE)
                    .collect()
            });
        Some(FunctionSig {
            param_types: params
                .types()
                .iter()
                .map(|ty| provider_infer_type(&self.type_pool, *ty))
                .collect(),
            return_type: provider_infer_type(&self.type_pool, info.return_type),
            is_generic: info.is_generic,
            param_modes: params.modes().to_vec(),
            param_comptime: params.comptime().to_vec(),
            param_comptime_type,
            param_names: params.names().to_vec(),
            param_type_syntax,
            return_type_syntax,
        })
    }
    fn uncached_method_sig(&self, key: (StructId, Spur)) -> Option<MethodSig> {
        let info = self.method_info_for_symbol(key.0, key.1)?;
        let params = self.state.param_data(info.params);
        Some(MethodSig {
            struct_type: info.struct_type,
            has_self: info.has_self,
            param_types: params
                .types()
                .iter()
                .map(|ty| provider_infer_type(&self.type_pool, *ty))
                .collect(),
            param_modes: params.modes().to_vec(),
            return_type: provider_infer_type(&self.type_pool, info.return_type),
        })
    }
    fn inference_builtin_struct_type(&self, name: Spur) -> Option<Type> {
        self.endpoint
            .endpoint_builtin_or_generated_struct(name)
            .map(Type::new_struct)
    }
    fn inference_struct_type_by_file(&self, key: (FileId, Spur)) -> Option<Type> {
        self.nominal_type_for_symbol(key.0, key.1)
            .filter(|ty| ty.as_struct().is_some())
    }
    fn inference_builtin_enum_type(&self, name: Spur) -> Option<Type> {
        self.endpoint
            .endpoint_builtin_enum(name)
            .map(Type::new_enum)
    }
    fn inference_enum_type_by_file(&self, key: (FileId, Spur)) -> Option<Type> {
        self.nominal_type_for_symbol(key.0, key.1)
            .filter(|ty| ty.as_enum().is_some())
    }
    fn inference_const_type(&self, key: (FileId, Spur)) -> Option<Type> {
        self.call_value_const(key.0, key.1)
            .map(|info| match info.value {
                ConstValue::Type(_) => Type::COMPTIME_TYPE,
                _ => info.ty,
            })
    }
    fn inference_const_type_alias(&self, key: (FileId, Spur)) -> Option<Type> {
        self.call_value_const(key.0, key.1)
            .and_then(|info| match info.value {
                ConstValue::Type(ty) => Some(ty),
                _ => None,
            })
    }
    fn inference_const_value(&self, key: (FileId, Spur)) -> Option<i128> {
        self.call_value_const(key.0, key.1)
            .and_then(|info| info.value.as_int_value())
    }
    fn inference_const_function_alias(&self, key: (FileId, Spur)) -> Option<Spur> {
        self.call_value_const(key.0, key.1)
            .and_then(|info| info.value.as_function())
            .map(SymbolHandle::spur)
    }
    fn inference_module_binding_type(&self, key: (FileId, Spur)) -> Option<Type> {
        self.call_module_binding(key.0, key.1).map(|info| info.ty)
    }
    fn inference_module_file_id(&self, module: ModuleId) -> Option<FileId> {
        self.calls.module_def(module).map(|def| def.file_id)
    }
    fn inference_function_by_file(&self, key: (FileId, Spur)) -> Option<Spur> {
        self.function_for_file_symbol(key.0, key.1)
    }
}

impl<P, S, K, M> TypeSyntaxHost for ProviderBodyHost<'_, P, S, K, M>
where
    P: BodyFactProvider,
    S: DurableNominalSource<K, M>
        + DurableAnonymousSource<K, M>
        + DurableCallableSource<K, M>
        + DurableConstSource<K, M>
        + DurableBodyLookupSource<K, M>,
    K: Clone + Eq + Hash + Ord,
    M: Clone + Eq + Hash + Ord,
{
    fn type_syntax_symbol(&mut self, name: &str) -> Spur {
        self.interner.get_or_intern(name)
    }

    fn type_syntax_module_binding(
        &mut self,
        authority: TypeRootAuthority,
        module: Option<ModuleId>,
        name: Spur,
    ) -> CompileResult<Option<crate::SemanticModuleBinding<ModuleId, FileId>>> {
        let file = module
            .and_then(|module| {
                self.calls
                    .module_def(module)
                    .map(|definition| definition.file_id)
            })
            .unwrap_or_else(|| authority.file());
        let Some(binding) = self.module_binding_for_symbol(file, name) else {
            return Ok(None);
        };
        let Some(target) = binding.ty.as_module() else {
            return Ok(None);
        };
        let defining_file = binding.span.file_id;
        let defining_path = self
            .source
            .source_path(defining_file)
            .unwrap_or_else(|| Arc::from("<unknown>"));
        Ok(Some(crate::SemanticModuleBinding {
            target,
            site: defining_file,
            is_public: binding.is_pub,
            defining_domain: crate::SemanticVisibilityDomain::from_file_path(Some(
                defining_path.as_ref(),
            )),
            defining_file: defining_path,
        }))
    }

    fn type_syntax_module_display_name(&self, module: ModuleId) -> Arc<str> {
        self.calls
            .module_def(module)
            .map(|definition| Arc::from(definition.import_path.as_str()))
            .unwrap_or_else(|| Arc::from("<unknown module>"))
    }

    fn type_syntax_accessing_domain(
        &self,
        authority: TypeRootAuthority,
    ) -> crate::SemanticVisibilityDomain {
        let path = self.source.source_path(authority.file());
        crate::SemanticVisibilityDomain::from_file_path(path.as_deref())
    }

    fn type_syntax_named_type(
        &mut self,
        authority: TypeRootAuthority,
        module: Option<ModuleId>,
        name: Spur,
        kind: TypeSyntaxNamedKind,
    ) -> CompileResult<Option<crate::SemanticTypeFact<Type, FileId>>> {
        let file = module
            .and_then(|module| {
                self.calls
                    .module_def(module)
                    .map(|definition| definition.file_id)
            })
            .unwrap_or_else(|| authority.file());
        let selected = match kind {
            TypeSyntaxNamedKind::Struct => self
                .nominal_type_for_symbol(file, name)
                .filter(|ty| ty.as_struct().is_some())
                .map(|ty| {
                    let metadata = self
                        .type_pool
                        .struct_metadata(ty.as_struct().expect("checked struct type"))
                        .expect("provider struct type has registered metadata");
                    (ty, metadata.file_id, metadata.is_pub)
                }),
            TypeSyntaxNamedKind::Enum => self
                .nominal_type_for_symbol(file, name)
                .filter(|ty| ty.as_enum().is_some())
                .map(|ty| {
                    let metadata = self
                        .type_pool
                        .enum_metadata(ty.as_enum().expect("checked enum type"))
                        .expect("provider enum type has registered metadata");
                    (ty, metadata.file_id, metadata.is_pub)
                }),
            TypeSyntaxNamedKind::Alias => {
                self.const_info_for_symbol(file, name)
                    .and_then(|info| match info.value {
                        ConstValue::Type(ty) => Some((ty, info.span.file_id, info.is_pub)),
                        _ => None,
                    })
            }
        };
        let Some((value, site, is_public)) = selected else {
            return Ok(None);
        };
        let defining_file = self
            .source
            .source_path(site)
            .unwrap_or_else(|| Arc::from("<unknown>"));
        Ok(Some(crate::SemanticTypeFact {
            value,
            site,
            is_public,
            defining_domain: crate::SemanticVisibilityDomain::from_file_path(Some(
                defining_file.as_ref(),
            )),
            defining_file,
        }))
    }

    fn type_syntax_make_str(&mut self, span: Span) -> CompileResult<Type> {
        let name = self.interner.get_or_intern("str");
        self.nominal_type_for_symbol(span.file_id, name)
            .filter(|ty| ty.as_struct().is_some())
            .ok_or_else(|| {
                CompileError::new(rue_error::ErrorKind::UnknownType("str".to_owned()), span)
            })
    }

    fn type_syntax_make_array(
        &mut self,
        element: Type,
        length: u64,
        span: Span,
    ) -> CompileResult<Type> {
        self.type_pool
            .try_intern_array(element, length)
            .map_err(|failure| {
                CompileError::new(
                    rue_error::ErrorKind::UnknownType(format!("array type: {failure:?}")),
                    span,
                )
            })
    }

    fn type_syntax_make_ptr_const(&mut self, pointee: Type, span: Span) -> CompileResult<Type> {
        self.type_pool
            .try_intern_ptr_const(pointee)
            .map_err(|failure| {
                CompileError::new(
                    rue_error::ErrorKind::UnknownType(format!("pointer type: {failure:?}")),
                    span,
                )
            })
    }

    fn type_syntax_make_ptr_mut(&mut self, pointee: Type, span: Span) -> CompileResult<Type> {
        self.type_pool
            .try_intern_ptr_mut(pointee)
            .map_err(|failure| {
                CompileError::new(
                    rue_error::ErrorKind::UnknownType(format!("pointer type: {failure:?}")),
                    span,
                )
            })
    }

    fn type_syntax_make_slice(
        &mut self,
        syntax: &str,
        element: Type,
        span: Span,
    ) -> CompileResult<Type> {
        let durable = self.durable_type_from_concrete(element).ok_or_else(|| {
            CompileError::new(rue_error::ErrorKind::UnknownType(syntax.to_owned()), span)
        })?;
        let id = self
            .endpoint
            .register_generated_slice(&durable, syntax)
            .ok_or_else(|| {
                CompileError::new(rue_error::ErrorKind::UnknownType(syntax.to_owned()), span)
            })?;
        Ok(Type::new_struct(id))
    }

    fn type_syntax_make_fixed_str(&mut self, capacity: u64, span: Span) -> CompileResult<Type> {
        let id = self
            .endpoint
            .register_generated_fixed_string(capacity)
            .ok_or_else(|| {
                CompileError::new(
                    rue_error::ErrorKind::UnknownType(format!("Str({capacity})")),
                    span,
                )
            })?;
        self.generated_structs
            .insert(self.interner.get_or_intern(&format!("Str({capacity})")), id);
        Ok(Type::new_struct(id))
    }

    fn type_syntax_record_builtin_call(&mut self) {}

    fn type_syntax_constructor(
        &mut self,
        authority: TypeRootAuthority,
        module: Option<ModuleId>,
        name: Spur,
    ) -> CompileResult<Option<crate::SemanticTypeConstructorHead<Spur, Spur, FileId>>> {
        let file = module
            .and_then(|module| {
                self.calls
                    .module_def(module)
                    .map(|definition| definition.file_id)
            })
            .unwrap_or_else(|| authority.file());
        let Some(symbol) = self.function_for_file_symbol(file, name) else {
            return Ok(None);
        };
        let Some((_, definition)) = self.function_token_for_symbol(symbol) else {
            return Ok(None);
        };
        let Some(signature) = DurableCallableSource::function(&self.source, &definition) else {
            return Ok(None);
        };
        let site = self
            .source
            .definition_source(&definition)
            .map_or(file, |locator| locator.file_id);
        let defining_file = self
            .source
            .source_path(site)
            .unwrap_or_else(|| Arc::from("<unknown>"));
        let parameters = signature
            .parameters
            .iter()
            .map(|parameter| crate::SemanticTypeConstructorParameter {
                name: self.interner.get_or_intern(parameter.name.as_ref()),
                is_comptime: parameter.is_comptime,
                is_type: matches!(parameter.ty, crate::SemanticImportType::ComptimeType),
            })
            .collect::<Vec<_>>();
        Ok(Some(crate::SemanticTypeConstructorHead {
            key: symbol,
            site,
            parameters: parameters.into(),
            returns_type: self.callable_returns_type(&signature, signature.type_syntax.as_ref()),
            is_public: signature.is_public,
            defining_domain: crate::SemanticVisibilityDomain::from_file_path(Some(
                defining_file.as_ref(),
            )),
            defining_file,
        }))
    }

    fn type_syntax_reduce_constructor(
        &mut self,
        head: &crate::SemanticTypeConstructorHead<Spur, Spur, FileId>,
        type_arguments: &[(Spur, Type)],
        value_arguments: &[(Spur, ConstValue)],
        span: Span,
    ) -> CompileResult<Option<ConstValue>> {
        let Some((_, definition)) = self.function_token_for_symbol(head.key) else {
            return Ok(None);
        };
        let mut durable_types = Vec::with_capacity(type_arguments.len());
        for &(name, value) in type_arguments {
            let Some(value) = self.durable_type_from_concrete(value) else {
                return Ok(None);
            };
            durable_types.push((Arc::from(self.interner.resolve(&name)), value));
        }
        let mut durable_values = Vec::with_capacity(value_arguments.len());
        for &(name, value) in value_arguments {
            let Some(value) = self.durable_value_from_concrete(value) else {
                return Ok(None);
            };
            durable_values.push((Arc::from(self.interner.resolve(&name)), value));
        }
        let reduced =
            match self
                .source
                .reduce_comptime_call(&definition, &durable_types, &durable_values)
            {
                DurableComptimeCallOutcome::Reduced(reduced) => reduced,
                DurableComptimeCallOutcome::NotReduced => return Ok(None),
                DurableComptimeCallOutcome::Diagnostic(diagnostic) => {
                    return Err(CompileError::new(
                        diagnostic.kind,
                        diagnostic.span.unwrap_or(span),
                    ));
                }
            };
        let value = match reduced.result {
            crate::SemanticComptimeCallResult::Type(ty) => {
                crate::SemanticImportConstValue::Type(ty)
            }
            crate::SemanticComptimeCallResult::Value(value) => value,
        };
        Ok(self.materialize_durable_const_value(&value))
    }

    fn type_syntax_value_const(&self, file: FileId, name: Spur) -> Option<ConstInfo> {
        self.const_info_for_symbol(file, name)
    }

    fn type_syntax_recover_const(
        &mut self,
        file: FileId,
        name: Spur,
    ) -> CompileResult<Option<ConstValue>> {
        Ok(self
            .const_info_for_symbol(file, name)
            .map(|info| info.value))
    }

    fn type_syntax_record_named_const_dependency(&mut self, _file: FileId, _name: String) {}

    fn type_syntax_out_of_scope_const_hint(&self, name: Spur, _exclude: FileId) -> String {
        let mut paths = self
            .source
            .out_of_scope_integer_const_paths(&self.key, self.interner.resolve(&name));
        paths.sort_unstable();
        paths.dedup();
        if paths.is_empty() {
            String::new()
        } else {
            format!(
                "; an integer constant of that name is declared in {} — import that module and bind a file-level `const` (for example `const {}: i32 = <module>.{};`) to use it as an array length here",
                paths
                    .iter()
                    .map(AsRef::as_ref)
                    .collect::<Vec<&str>>()
                    .join(", "),
                self.interner.resolve(&name),
                self.interner.resolve(&name),
            )
        }
    }

    fn type_syntax_dependencies(
        &self,
        _ty: Type,
    ) -> Vec<(FileId, String, super::DeclarationTypeDependencyTargetKind)> {
        Vec::new()
    }

    fn type_syntax_flush_dependency(
        &mut self,
        _file: FileId,
        _name: String,
        _kind: super::DeclarationTypeDependencyTargetKind,
    ) {
    }
}

impl<P, S, K, M> ProviderBodyHost<'_, P, S, K, M>
where
    P: BodyFactProvider,
    S: DurableNominalSource<K, M>
        + DurableAnonymousSource<K, M>
        + DurableCallableSource<K, M>
        + DurableConstSource<K, M>
        + DurableBodyLookupSource<K, M>,
    K: Clone + Eq + Hash + Ord,
    M: Clone + Eq + Hash + Ord,
{
    /// Apply the 6.6:3-6.6:5 accessor declaration rules to a free function
    /// whose body this host is about to analyze. Predeclaration already ran
    /// them over every declaration in the program; repeating them at the body
    /// entry keeps a host that analyzes a body it did not predeclare from
    /// admitting `-> borrow T` on a receiverless callable.
    fn reject_free_function_accessor(&self, declaration: InstRef) -> CompileResult<()> {
        super::declarations::check_accessor_declaration_shape(
            self.rir.rir(),
            &self.interner,
            declaration,
            None,
            false,
        )
    }
}

impl<P, S, K, M> OrdinaryBodyAnalysisHost for ProviderBodyHost<'_, P, S, K, M>
where
    P: BodyFactProvider,
    S: DurableNominalSource<K, M>
        + DurableAnonymousSource<K, M>
        + DurableCallableSource<K, M>
        + DurableConstSource<K, M>
        + DurableBodyLookupSource<K, M>,
    K: Clone + Eq + Hash + Ord,
    M: Clone + Eq + Hash + Ord,
{
    type InferenceFacts<'b>
        = HostInferenceFacts<'b, Self>
    where
        Self: 'b;

    fn inference_facts<'b>(&'b self, ctx: &'b InferenceContext) -> Self::InferenceFacts<'b> {
        HostInferenceFacts::new(ctx, self)
    }

    fn resolve_structured_type_syntax(
        &mut self,
        request: StructuredTypeSyntaxRequest<'_>,
    ) -> Result<
        Type,
        crate::SemanticTypeSyntaxError<std::convert::Infallible, CompileError, FileId, Spur>,
    > {
        let interner = Arc::clone(&self.interner);
        let mut provider = TypeSyntaxProvider::new(
            self,
            request.span,
            TypeRootAuthority::in_file(request.root_file),
            SemaTypeResolutionContext::Type,
            request.type_substitutions,
            request.value_substitutions,
        );
        let result = crate::resolve_structured_semantic_type_syntax_with(
            &mut provider,
            &request.root_file,
            &request.syntax.arena,
            request.syntax.root,
            |symbol| interner.resolve(symbol),
        );
        provider.flush_observed_type_dependencies();
        result
    }

    fn resolve_type_module_prefix(
        &mut self,
        request: ModulePrefixRequest<'_>,
    ) -> CompileResult<(ModuleId, Option<FileId>, String)> {
        let mut provider = TypeSyntaxProvider::new(
            self,
            request.span,
            TypeRootAuthority::in_file(request.root_file),
            SemaTypeResolutionContext::Type,
            None,
            None,
        );
        let resolved = crate::resolve_semantic_module_path(
            &mut provider,
            &request.root_file,
            request.segments,
        )
        .map_err(|failure| {
            super::typeck::module_path_resolution_compile_error(failure, request.span)
        })?;
        provider.flush_observed_type_dependencies();
        let definition = self.calls.module_def(resolved.module).ok_or_else(|| {
            CompileError::new(
                rue_error::ErrorKind::UnknownType(request.segments.join(".")),
                request.span,
            )
        })?;
        // `resolved.site` is the source file containing the final module
        // binding. Member lookup must continue from the target module's file;
        // imports commonly bind a module in a different file.
        Ok((
            resolved.module,
            Some(definition.file_id),
            definition.file_path,
        ))
    }

    fn resolve_array_length(&mut self, request: ArrayLengthRequest<'_>) -> CompileResult<u64> {
        let mut provider = TypeSyntaxProvider::new(
            self,
            request.span,
            TypeRootAuthority::in_file(request.span.file_id),
            SemaTypeResolutionContext::Type,
            None,
            request.value_substitutions,
        );
        let result = provider.resolve_array_length_fact(request.span.file_id, request.length);
        provider.flush_observed_type_dependencies();
        result
    }

    fn record_expression_analysis_breakdown(&mut self, breakdown: ExpressionAnalysisBreakdown) {
        self.expression_breakdown = Some(breakdown);
    }

    fn body_param_data(&self, range: ParamRange) -> ParamRangeData {
        self.state.param_data(range)
    }
    fn allocate_method_params(
        &mut self,
        names: impl IntoIterator<Item = Spur>,
        types: impl IntoIterator<Item = Type>,
        modes: impl IntoIterator<Item = RirParamMode>,
        comptime: impl IntoIterator<Item = bool>,
    ) -> ParamRange {
        self.state.allocate_params(names, types, modes, comptime)
    }
    fn install_anonymous_method(&mut self, key: (StructId, Spur), info: MethodInfo) {
        self.anonymous_methods
            .borrow_mut()
            .insert(key, MethodCallInfo::from_body(info));
    }
    fn known_symbols(&self) -> &KnownSymbols {
        &self.known
    }
    fn strbuf_type(&self) -> Option<Type> {
        if let Some(ty) = self.type_pool.lang_item_type(crate::LangItem::StrBuf) {
            return Some(ty);
        }
        let key = self
            .source
            .language_item_nominal(&self.key, crate::LangItem::StrBuf)?;
        self.register_import_nominal_identities(&crate::SemanticImportType::Nominal(key))
            .ok()?;
        self.type_pool.lang_item_type(crate::LangItem::StrBuf)
    }
    fn is_strbuf(&self, ty: Type) -> bool {
        matches!(ty.kind(), TypeKind::Struct(id) if self.type_pool.is_strbuf(id))
    }
    fn types_equivalent(&self, left: Type, right: Type) -> bool {
        left == right
    }
    fn generated_structs(&self) -> &AHashMap<Spur, StructId> {
        &self.generated_structs
    }
    fn body_interner(&self) -> &ThreadedRodeo {
        &self.interner
    }
    fn body_type_pool(&self) -> &TypeInternPool {
        // This accessor is the engine's containment-read boundary. The pool's
        // dirty check is O(1), so ordinary reads pay no graph walk; a batch that
        // declared or completed composite types pays one metered join before
        // the first containment-dependent read.
        self.type_pool
            .finalize_containment_metadata()
            .expect("provider type materialization must produce an acyclic containment graph");
        &self.type_pool
    }
    fn struct_id_for_name(&self, name: Spur) -> Option<StructId> {
        self.generated_structs.get(&name).copied().or_else(|| {
            self.nominal_type_for_symbol(self.owner_file, name)
                .and_then(|ty| ty.as_struct())
        })
    }
    fn generated_structs_mut(&mut self) -> &mut AHashMap<Spur, StructId> {
        &mut self.generated_structs
    }
    fn generated_enums_mut(&mut self) -> &mut AHashMap<Spur, EnumId> {
        &mut self.generated_enums
    }
    fn anonymous_struct_id(
        &self,
        identity: &super::anon_structs::IssuedAnonymousNominalKey,
    ) -> Option<StructId> {
        self.anon_struct_identities
            .get(identity)
            .copied()
            .or_else(|| {
                self.consulted_anonymous_types
                    .borrow()
                    .iter()
                    .find_map(|(ty, candidate)| {
                        (candidate.with_canonical_producer().as_ref()
                            == identity.with_canonical_producer().as_ref())
                        .then(|| ty.as_struct())
                        .flatten()
                    })
            })
    }
    fn anonymous_enum_id(
        &self,
        identity: &super::anon_structs::IssuedAnonymousNominalKey,
    ) -> Option<EnumId> {
        self.anon_enum_identities
            .get(identity)
            .copied()
            .or_else(|| {
                self.consulted_anonymous_types
                    .borrow()
                    .iter()
                    .find_map(|(ty, candidate)| {
                        (candidate.with_canonical_producer().as_ref()
                            == identity.with_canonical_producer().as_ref())
                        .then(|| ty.as_enum())
                        .flatten()
                    })
            })
    }
    fn anonymous_struct_identities_mut(
        &mut self,
    ) -> &mut AHashMap<super::anon_structs::IssuedAnonymousNominalKey, StructId> {
        &mut self.anon_struct_identities
    }
    fn anonymous_enum_identities_mut(
        &mut self,
    ) -> &mut AHashMap<super::anon_structs::IssuedAnonymousNominalKey, EnumId> {
        &mut self.anon_enum_identities
    }
    fn anonymous_digest_owner(
        &self,
        digest: u128,
    ) -> Option<&super::anon_structs::IssuedAnonymousNominalKey> {
        self.anonymous_digest_owners.get(&digest)
    }
    fn install_anonymous_digest_owner(
        &mut self,
        digest: u128,
        identity: super::anon_structs::IssuedAnonymousNominalKey,
    ) {
        self.anonymous_digest_owners.insert(digest, identity);
    }
    #[cfg(test)]
    fn forced_anonymous_digest(
        &self,
        _identity: &super::anon_structs::IssuedAnonymousNominalKey,
    ) -> Option<u128> {
        None
    }
    fn install_canonical_anonymous_type(
        &mut self,
        ty: Type,
        identity: super::anon_structs::IssuedAnonymousNominalKey,
    ) {
        self.canonical_anonymous_types.insert(ty, identity);
    }
    fn anonymous_struct_methods_mut(
        &mut self,
    ) -> &mut AHashMap<StructId, Vec<super::AnonMethodSig>> {
        &mut self.anon_struct_method_sigs
    }
    fn anonymous_struct_captures_mut(
        &mut self,
    ) -> &mut AHashMap<StructId, AHashMap<Spur, ConstValue>> {
        &mut self.anon_struct_captured_values
    }
    fn anonymous_struct_ids_mut(&mut self) -> &mut AHashSet<StructId> {
        &mut self.anonymous_struct_ids
    }
    fn anonymous_enum_ids_mut(&mut self) -> &mut AHashSet<EnumId> {
        &mut self.anonymous_enum_ids
    }
    fn canonical_type_instance(
        &self,
        ty: Type,
    ) -> Result<super::anon_structs::IssuedTypeInstanceKey, crate::SemanticBodyExportFailure> {
        let recurse = |ty| self.canonical_type_instance(ty);
        Ok(match ty.kind() {
            TypeKind::I8 => TypeInstanceKey::I8,
            TypeKind::I16 => TypeInstanceKey::I16,
            TypeKind::I32 => TypeInstanceKey::I32,
            TypeKind::I64 => TypeInstanceKey::I64,
            TypeKind::U8 => TypeInstanceKey::U8,
            TypeKind::U16 => TypeInstanceKey::U16,
            TypeKind::U32 => TypeInstanceKey::U32,
            TypeKind::U64 => TypeInstanceKey::U64,
            TypeKind::Bool => TypeInstanceKey::Bool,
            TypeKind::Unit => TypeInstanceKey::Unit,
            TypeKind::Never => TypeInstanceKey::Never,
            TypeKind::ComptimeType => TypeInstanceKey::ComptimeType,
            TypeKind::Array(id) => {
                let (element, len) = self.type_pool.array_def(id);
                TypeInstanceKey::Array {
                    element: Node::new(recurse(element)?),
                    len,
                }
            }
            TypeKind::PtrConst(id) => {
                TypeInstanceKey::PtrConst(Node::new(recurse(self.type_pool.ptr_const_def(id))?))
            }
            TypeKind::PtrMut(id) => {
                TypeInstanceKey::PtrMut(Node::new(recurse(self.type_pool.ptr_mut_def(id))?))
            }
            TypeKind::Struct(id) => match self.body_struct_identity(id)? {
                crate::NominalInstanceKey::Builtin { kind, name } => {
                    TypeInstanceKey::BuiltinNominal { kind, name }
                }
                identity => TypeInstanceKey::Nominal(identity),
            },
            TypeKind::Enum(id) => match self.body_enum_identity(id)? {
                crate::NominalInstanceKey::Builtin { kind, name } => {
                    TypeInstanceKey::BuiltinNominal { kind, name }
                }
                identity => TypeInstanceKey::Nominal(identity),
            },
            TypeKind::Module(id) => {
                let (token, _) = self
                    .module_tokens
                    .borrow()
                    .get(&id)
                    .cloned()
                    .ok_or(crate::SemanticBodyExportFailure::MissingStableIdentity)?;
                TypeInstanceKey::Module(token)
            }
            TypeKind::Error => {
                return Err(crate::SemanticBodyExportFailure::MissingStableIdentity);
            }
        })
    }
    fn canonical_argument_value(
        &self,
        value: ConstValue,
    ) -> Result<
        CanonicalArgumentValue<SemanticDefinitionToken, SemanticModuleToken>,
        crate::SemanticBodyExportFailure,
    > {
        Ok(match value {
            ConstValue::Integer(value) => CanonicalArgumentValue::Integer(value),
            ConstValue::Bool(value) => CanonicalArgumentValue::Bool(value),
            ConstValue::Type(ty) => {
                CanonicalArgumentValue::Type(Node::new(self.canonical_type_instance(ty)?))
            }
            ConstValue::Function(symbol) => CanonicalArgumentValue::Function(Node::new(
                FunctionInstanceKey::Definition(self.function_identity(symbol.spur())?),
            )),
            ConstValue::String(symbol) => {
                CanonicalArgumentValue::String(self.interner.resolve(&symbol.spur()).into())
            }
            ConstValue::Unit => CanonicalArgumentValue::Unit,
        })
    }
    fn function_identity(
        &self,
        symbol: Spur,
    ) -> Result<SemanticDefinitionToken, crate::SemanticBodyExportFailure> {
        let token = self
            .function_token_for_symbol(symbol)
            .map(|(token, _)| token);
        token.ok_or(crate::SemanticBodyExportFailure::MissingStableIdentity)
    }
    fn comptime_type_param_flags(&self, function: &FunctionCallInfo) -> Vec<bool> {
        if let Some(flags) = self
            .durable_comptime_type_flags
            .borrow()
            .get(&function.params)
            .filter(|flags| flags.len() == function.params.len())
        {
            return flags.clone();
        }
        let same_body = |candidate: FunctionInfo| candidate.params == function.params;
        let same_call = |candidate: FunctionCallInfo| candidate.params == function.params;
        let symbol = if self
            .endpoint
            .endpoint_function_info(self.function_symbol)
            .is_some_and(same_body)
        {
            Some(self.function_symbol)
        } else {
            self.function_infos
                .borrow()
                .iter()
                .find_map(|(symbol, info)| same_call(*info).then_some(*symbol))
        };
        if let Some(key) = symbol
            .and_then(|symbol| self.function_tokens.borrow().get(&symbol).cloned())
            .map(|(_, key)| key)
            && let Some(durable) = DurableCallableSource::function(&self.source, &key)
        {
            let flags = durable
                .parameters
                .iter()
                .map(|parameter| {
                    parameter.is_comptime
                        && matches!(parameter.ty, crate::SemanticImportType::ComptimeType)
                })
                .collect::<Vec<_>>();
            if flags.len() == function.params.len() {
                return flags;
            }
        }
        let flags = self
            .state
            .param_data(function.params)
            .types()
            .iter()
            .map(|ty| *ty == Type::COMPTIME_TYPE)
            .collect::<Vec<_>>();
        assert_eq!(flags.len(), function.params.len());
        flags
    }
    fn function_param_type_syntax(
        &self,
        function: &FunctionCallInfo,
        param_index: usize,
    ) -> Option<super::fact_mode::StructuredTypeSyntax> {
        if let Some(syntax) = self
            .durable_callable_type_syntax
            .borrow()
            .get(&function.params)
            .cloned()
        {
            return Some(super::fact_mode::StructuredTypeSyntax {
                root: *syntax.parameters.get(param_index)?,
                arena: syntax.arena,
            });
        }
        let local = self.endpoint.endpoint_function_info(self.function_symbol)?;
        if local.params != function.params {
            return None;
        }
        let InstData::FnDecl { params, .. } = &self.rir.rir().get(local.declaration).data else {
            return None;
        };
        let root = self.rir.rir().params(params).get(param_index)?.ty;
        Some(super::fact_mode::StructuredTypeSyntax {
            arena: self.rir.rir().type_syntax().clone(),
            root,
        })
    }
    fn function_return_type_syntax(
        &self,
        function: &FunctionCallInfo,
    ) -> Option<super::fact_mode::StructuredTypeSyntax> {
        if let Some(syntax) = self
            .durable_callable_type_syntax
            .borrow()
            .get(&function.params)
            .cloned()
        {
            return Some(super::fact_mode::StructuredTypeSyntax {
                arena: syntax.arena,
                root: syntax.result,
            });
        }
        let local = self.endpoint.endpoint_function_info(self.function_symbol)?;
        if local.params != function.params {
            return None;
        }
        let InstData::FnDecl { return_type, .. } = &self.rir.rir().get(local.declaration).data
        else {
            return None;
        };
        Some(super::fact_mode::StructuredTypeSyntax {
            arena: self.rir.rir().type_syntax().clone(),
            root: *return_type,
        })
    }
    fn function_signature_root_file(&self, function: &FunctionCallInfo) -> Option<FileId> {
        self.durable_signature_files
            .borrow()
            .get(&function.params)
            .copied()
    }
    fn reduce_external_comptime_call(
        &mut self,
        name: Spur,
        callee_types: &AHashMap<Spur, Type>,
        callee_values: &AHashMap<Spur, ConstValue>,
        span: Span,
    ) -> Option<CompileResult<Option<ConstValue>>> {
        let definition = if name == self.function_symbol {
            self.key.clone()
        } else {
            let (_, definition) = self.function_token_for_symbol(name)?;
            definition
        };
        let signature = DurableCallableSource::function(&self.source, &definition)?;
        // Runtime-valued self calls are request-local and use the ordinary
        // evaluator. A `-> type` self call, however, must ask the canonical
        // comptime query: a same-key recursion is a typed query cycle that the
        // required reduction below turns into an E1200 at this call site.
        if name == self.function_symbol
            && !matches!(signature.result, crate::SemanticImportType::ComptimeType)
        {
            return None;
        }
        let type_arguments = signature
            .parameters
            .iter()
            .filter_map(|parameter| {
                let symbol = self.interner.get(parameter.name.as_ref())?;
                callee_types
                    .get(&symbol)
                    .copied()
                    .map(|ty| (parameter.name.clone(), self.durable_type_from_concrete(ty)))
            })
            .map(|(name, value)| value.map(|value| (name, value)))
            .collect::<Option<Vec<_>>>();
        let value_arguments = signature
            .parameters
            .iter()
            .filter_map(|parameter| {
                let symbol = self.interner.get(parameter.name.as_ref())?;
                callee_values.get(&symbol).cloned().map(|value| {
                    (
                        parameter.name.clone(),
                        self.durable_value_from_concrete(value),
                    )
                })
            })
            .map(|(name, value)| value.map(|value| (name, value)))
            .collect::<Option<Vec<_>>>();
        let Some(type_arguments) = type_arguments else {
            return Some(Ok(None));
        };
        let Some(value_arguments) = value_arguments else {
            return Some(Ok(None));
        };
        let required_type_reduction =
            matches!(signature.result, crate::SemanticImportType::ComptimeType);
        let reduced =
            match self
                .source
                .reduce_comptime_call(&definition, &type_arguments, &value_arguments)
            {
                DurableComptimeCallOutcome::Reduced(reduced) => reduced,
                DurableComptimeCallOutcome::NotReduced => return Some(Ok(None)),
                DurableComptimeCallOutcome::Diagnostic(diagnostic) if required_type_reduction => {
                    return Some(Err(CompileError::new(
                        diagnostic.kind,
                        diagnostic.span.unwrap_or(span),
                    )));
                }
                DurableComptimeCallOutcome::Diagnostic(_) => return Some(Ok(None)),
            };
        let producer = (|| {
            Some(FunctionInstanceKey::Specialization {
                base: Node::new(FunctionInstanceKey::Definition(
                    self.function_tokens.borrow().get(&name)?.0,
                )),
                arguments: crate::CanonicalArguments {
                    types: signature
                        .parameters
                        .iter()
                        .filter(|parameter| {
                            matches!(parameter.ty, crate::SemanticImportType::ComptimeType)
                        })
                        .map(|parameter| {
                            let symbol = self.interner.get(parameter.name.as_ref())?;
                            self.canonical_type_instance(*callee_types.get(&symbol)?)
                                .ok()
                        })
                        .collect::<Option<Vec<_>>>()?
                        .into(),
                    values: signature
                        .parameters
                        .iter()
                        .filter(|parameter| {
                            !matches!(parameter.ty, crate::SemanticImportType::ComptimeType)
                        })
                        .map(|parameter| {
                            let symbol = self.interner.get(parameter.name.as_ref())?;
                            self.canonical_argument_value(*callee_values.get(&symbol)?)
                                .ok()
                        })
                        .collect::<Option<Vec<_>>>()?
                        .into(),
                },
            })
        })();
        if let Some(producer) = producer {
            self.observed_comptime_producers
                .borrow_mut()
                .insert(producer);
        }
        let value = match reduced.result {
            crate::SemanticComptimeCallResult::Type(ty) => {
                self.materialize_durable_type(&ty).map(ConstValue::Type)
            }
            crate::SemanticComptimeCallResult::Value(value) => {
                self.materialize_durable_const_value(&value)
            }
        };
        Some(Ok(value))
    }
    fn stable_definition_symbol_component(&self, token: &SemanticDefinitionToken) -> String {
        format!("d{}-{}", token.issuer(), token.slot())
    }
    fn stable_module_symbol_component(&self, token: &SemanticModuleToken) -> String {
        format!("m{}-{}", token.issuer(), token.slot())
    }
    fn resolve_body_type(&mut self, syntax: RirTypeSyntaxRef, span: Span) -> CompileResult<Type> {
        self.resolve_body_type_with_substitutions(syntax, span, None, None)
    }
    fn resolve_body_type_with_substitutions(
        &mut self,
        syntax: RirTypeSyntaxRef,
        span: Span,
        type_substitutions: Option<&AHashMap<Spur, Type>>,
        value_substitutions: Option<&AHashMap<Spur, ConstValue>>,
    ) -> CompileResult<Type> {
        let syntax = super::fact_mode::StructuredTypeSyntax {
            arena: self.rir.rir().type_syntax().clone(),
            root: syntax,
        };
        let interner = Arc::clone(&self.interner);
        OrdinaryBodyAnalysisHost::resolve_structured_type_syntax(
            self,
            StructuredTypeSyntaxRequest {
                syntax: &syntax,
                root_file: span.file_id,
                span,
                type_substitutions,
                value_substitutions,
            },
        )
        .map_err(|failure| semantic_type_syntax_compile_error(&interner, failure, span))
    }
    fn replace_active_anonymous_producer(
        &mut self,
        producer: Option<super::anon_structs::IssuedStableProducerId>,
    ) -> Option<super::anon_structs::IssuedStableProducerId> {
        std::mem::replace(&mut self.active_anonymous_producer, producer)
    }
    fn body_rir_ref(&self) -> &Rir {
        self.rir.rir()
    }
    fn body_inline_ctor_head_candidates(&self) -> usize {
        self.rir.rir_index().inline_ctor_head_candidates()
    }
    fn active_anonymous_producer(&self) -> Option<&super::anon_structs::IssuedStableProducerId> {
        self.active_anonymous_producer.as_ref()
    }
    fn body_declaration_type_observer(
        &self,
    ) -> Option<&(
        FileId,
        String,
        Option<String>,
        DeclarationTypeDependencySourceKind,
        DeclarationTypeDependencyKind,
    )> {
        None
    }
    fn body_analysis_work_mut(&mut self) -> &mut BodyAnalysisWork {
        &mut self.body_work
    }
    fn record_resolved_declaration_type(&mut self, _ty: Type) {}
    fn body_analysis_error_recovery(&self) -> bool {
        false
    }
    fn body_analysis_first_recovered_error(&self) -> Option<CompileError> {
        self.recovered_errors.first().cloned()
    }
    fn body_analysis_recovered_errors_mut(&mut self) -> &mut Vec<CompileError> {
        &mut self.recovered_errors
    }
    fn function_info(&self, name: Spur) -> Option<FunctionCallInfo> {
        self.function_info_for_symbol(name)
    }
    fn function_body_info(&self, name: Spur) -> Option<FunctionInfo> {
        self.endpoint.endpoint_function_info(name)
    }
    fn value_const(&self, file: FileId, name: Spur) -> Option<ConstInfo> {
        self.call_value_const(file, name)
    }
    fn source_function_name(&self, name: Spur) -> Spur {
        self.endpoint.endpoint_source_function_name(name)
    }
    fn resolve_function_name_local(&self, name: Spur, file: FileId) -> Option<Spur> {
        self.function_for_file_symbol(file, name)
    }
    fn module_def(&self, module: ModuleId) -> ModuleDef {
        self.calls
            .module_def(module)
            .expect("provider body module must be registered before use")
    }
    fn struct_in_file(&self, file: FileId, name: Spur) -> Option<StructId> {
        self.nominal_type_for_symbol(file, name)?.as_struct()
    }
    fn builtin_struct(&self, name: Spur) -> Option<StructId> {
        self.endpoint.endpoint_builtin_or_generated_struct(name)
    }
    fn target(&self) -> Target {
        self.target
    }
    fn builtin_arch_id(&self) -> Option<EnumId> {
        self.endpoint
            .endpoint_builtin_enum(self.interner.get_or_intern_static("Arch"))
    }
    fn builtin_os_id(&self) -> Option<EnumId> {
        self.endpoint
            .endpoint_builtin_enum(self.interner.get_or_intern_static("Os"))
    }
    fn builtin_data_model_id(&self) -> Option<EnumId> {
        self.endpoint
            .endpoint_builtin_enum(self.interner.get_or_intern_static("DataModel"))
    }
    fn destructor_span(&self, _struct_id: StructId) -> Option<Span> {
        None
    }
    fn infectious_linear_reason(&self, struct_id: StructId) -> Option<(String, String)> {
        let ty = Type::new_struct(struct_id);
        let explicitly_linear = self
            .endpoint
            .durable_named_identity(ty)
            .and_then(|key| DurableNominalSource::nominal(&self.source, &key))
            .is_some_and(|nominal| {
                matches!(
                    nominal.body,
                    crate::DurableNominalBody::Struct {
                        is_linear: true,
                        ..
                    }
                )
            });
        if explicitly_linear {
            return None;
        }
        let definition = self.type_pool.struct_def(struct_id);
        if !definition.is_linear {
            return None;
        }
        definition
            .fields
            .iter()
            .find(|field| self.type_pool.type_carries_linear(field.ty))
            .map(|field| {
                (
                    field.name.clone(),
                    field.ty.safe_name_with_pool(Some(&self.type_pool)),
                )
            })
    }
    fn well_known_option(&self, payload: Type) -> Option<Type> {
        self.endpoint.well_known_option_for_payload(payload)
    }
    fn set_anon_struct_type_subst(&mut self, struct_id: StructId, subst: AHashMap<Spur, Type>) {
        self.anon_struct_type_subst.insert(struct_id, subst);
    }
    fn anon_struct_type_subst(&self, struct_id: StructId) -> AHashMap<Spur, Type> {
        self.anon_struct_type_subst
            .get(&struct_id)
            .cloned()
            .unwrap_or_default()
    }
    fn anon_struct_captured_values(&self, struct_id: StructId) -> AHashMap<Spur, ConstValue> {
        self.anon_struct_captured_values
            .get(&struct_id)
            .cloned()
            .unwrap_or_default()
    }
    fn body_dependency_observer(&self) -> Option<super::AnalyzedBodyOwnerEvent> {
        None
    }
    fn record_body_named_dependency(&mut self, _target: super::NamedConstDependencyTargetEvent) {}
    fn record_body_callable_dependency(&mut self, _symbol: Spur) {}
    fn record_specialization_dependency(
        &mut self,
        _identity: FunctionInstanceKey<SemanticDefinitionToken, SemanticModuleToken>,
    ) {
    }
    fn intern_array_type(&mut self, element: Type, length: u64) -> ArrayTypeId {
        self.type_pool.intern_array_from_type(element, length)
    }
    fn require_preview(
        &self,
        feature: rue_error::PreviewFeature,
        what: &str,
        span: Span,
    ) -> CompileResult<()> {
        if self.preview.contains(&feature) {
            Ok(())
        } else {
            Err(CompileError::new(
                rue_error::ErrorKind::PreviewFeatureRequired {
                    feature,
                    what: what.to_owned(),
                },
                span,
            )
            .with_help(format!(
                "use --preview {} to enable this feature ({})",
                feature.name(),
                feature.adr()
            )))
        }
    }
    fn declaration_binding_active(&self) -> bool {
        false
    }
    fn known_linear_during_binding(&self, _ty: Type) -> Option<bool> {
        None
    }
    fn known_drop_glue_during_binding(&self, _ty: Type) -> Option<bool> {
        None
    }
    fn has_ctor_type_display(&self, ty: Type) -> bool {
        self.ctor_displays.contains_key(&ty)
    }
    fn record_body_ctor_type_display(&mut self, ty: Type, display: String) {
        self.ctor_displays.insert(ty, display);
    }
    fn trusted_try_producer(&self, ty: Type) -> Option<super::anon_structs::TrustedTryProducer> {
        match self
            .endpoint
            .durable_anonymous_identity(ty)
            .and_then(|identity| self.source.trusted_try_producer(&identity))
        {
            Some(DurableTryProducer::Option) => {
                Some(super::anon_structs::TrustedTryProducer::Option)
            }
            Some(DurableTryProducer::Result) => {
                Some(super::anon_structs::TrustedTryProducer::Result)
            }
            None => None,
        }
    }
    fn resolve_canonical_import(&self, import_path: &str, span: Span) -> CompileResult<ModuleId> {
        let target = self
            .source
            .canonical_import(&self.key, import_path)
            .ok_or_else(|| {
                CompileError::new(
                    rue_error::ErrorKind::UnknownType(import_path.to_owned()),
                    span,
                )
            })?;
        self.register_module_target(target)
            .map(|(id, _)| id)
            .ok_or_else(|| {
                CompileError::new(
                    rue_error::ErrorKind::InvalidCompilerInput(
                        "canonical import target could not be registered".into(),
                    ),
                    span,
                )
            })
    }
    fn deferred_ownership_gates_mut(&mut self) -> &mut Vec<super::DeferredOwnershipGate> {
        &mut self.deferred_ownership
    }
}

/// Run the canonical ordinary expression engine over one exact local RIR.
pub fn analyze_provider_ordinary_body<P, S, K, M>(
    provider: &P,
    source: S,
    bundle: &BodyRirBundle,
    key: K,
    name: &str,
    owner_kind: crate::StableDefinitionKind,
    owner_name: Option<&str>,
    target: Target,
    preview: PreviewFeatures,
    well_known: &ProviderWellKnownOptionFacts<K, M>,
) -> CompileResult<ProviderOrdinaryBody<K, M>>
where
    P: BodyFactProvider,
    S: DurableNominalSource<K, M>
        + DurableAnonymousSource<K, M>
        + DurableCallableSource<K, M>
        + DurableConstSource<K, M>
        + DurableBodyLookupSource<K, M>,
    K: Clone + Eq + Hash + Ord,
    M: Clone + Eq + Hash + Ord,
{
    let owner_file = bundle.source_file_id().ok_or_else(|| {
        CompileError::without_span(rue_error::ErrorKind::InvalidCompilerInput(
            "provider body RIR does not have one source file".into(),
        ))
    })?;
    let host_setup_started = Instant::now();
    let mut host = ProviderBodyHost::new(
        provider, source, bundle, key, owner_file, name, owner_kind, owner_name, target, preview,
        well_known,
    )
    .ok_or_else(|| {
        CompileError::without_span(rue_error::ErrorKind::InvalidCompilerInput(
            "provider body host could not be constructed".into(),
        ))
    })?;
    let initial_anonymous_identities = host
        .canonical_anonymous_types
        .values()
        .cloned()
        .collect::<AHashSet<_>>();
    let infer = InferenceContext::new(&host);
    let host_setup_ns = elapsed_ns(host_setup_started);
    let expression_engine_started = Instant::now();
    let (analyzed, body_span) = match owner_kind {
        crate::StableDefinitionKind::Function => {
            let info = host
                .endpoint
                .endpoint_function_info(host.function_symbol)
                .ok_or_else(|| {
                    CompileError::without_span(rue_error::ErrorKind::UndefinedFunction(
                        name.to_owned(),
                    ))
                })?;
            let declaration = host
                .endpoint
                .first_free_function(name, owner_file)
                .ok_or_else(|| {
                    CompileError::without_span(rue_error::ErrorKind::UndefinedFunction(
                        name.to_owned(),
                    ))
                })?;
            host.reject_free_function_accessor(declaration)?;
            let (return_type, body) = match &host.rir.rir().get(declaration).data {
                InstData::FnDecl {
                    return_type, body, ..
                } => (*return_type, *body),
                _ => unreachable!("registered provider function points at FnDecl"),
            };
            let body_span = host.rir.rir().get(body).span;
            // The durable call-site signature may normalize body-local types
            // such as `Str(N)` to their coercion surface. Resolve the exact
            // declared return spelling for body checking; explicit parameters
            // retain their exact canonical facts and need no second lookup.
            let return_type = host.resolve_body_type(return_type, info.span)?;
            let params = host
                .state
                .param_data(info.params)
                .iter()
                .map(|(name, ty, mode, comptime)| (*name, *ty, *mode, *comptime))
                .collect();
            host.endpoint
                .finalize_containment_metadata()
                .ok_or_else(|| {
                    CompileError::without_span(rue_error::ErrorKind::InvalidCompilerInput(
                        "provider function containment metadata is unavailable".into(),
                    ))
                })?;
            (
                OrdinaryBodyEngine::new(&mut host).analyze_single_function_resolved(
                    &infer,
                    name,
                    return_type,
                    params,
                    body,
                    info.span,
                    info.allow_unused_variable,
                    info.allow_unreachable_code,
                )?,
                body_span,
            )
        }
        crate::StableDefinitionKind::Method | crate::StableDefinitionKind::AssociatedFunction => {
            let owner_name = owner_name.ok_or_else(|| {
                CompileError::without_span(rue_error::ErrorKind::InvalidCompilerInput(
                    "provider member body has no owner".into(),
                ))
            })?;
            let declaration = host
                .endpoint
                .named_method_declaration(owner_file, owner_name, name)
                .ok_or_else(|| {
                    CompileError::without_span(rue_error::ErrorKind::UndefinedFunction(
                        name.to_owned(),
                    ))
                })?;
            let info = host
                .calls
                .method_info(&host.key, owner_file, owner_name, name)
                .ok_or_else(|| {
                    CompileError::without_span(rue_error::ErrorKind::UndefinedFunction(
                        name.to_owned(),
                    ))
                })?;
            let (return_type, body) = match &host.rir.rir().get(declaration).data {
                InstData::FnDecl {
                    return_type, body, ..
                } => (*return_type, *body),
                _ => unreachable!("registered provider member points at FnDecl"),
            };
            let body_span = host.rir.rir().get(body).span;
            let return_type = if matches!(
                host.rir.rir().type_syntax().node(return_type),
                Some(rue_rir::RirTypeSyntaxNode::Named(symbol))
                    if host.rir.rir().type_syntax().symbol(*symbol)
                        .is_some_and(|symbol| host.interner.resolve(symbol) == "Self")
            ) {
                info.struct_type
            } else {
                host.resolve_body_type(return_type, info.span)?
            };
            let params = host
                .state
                .param_data(info.params)
                .iter()
                .map(|(name, ty, mode, comptime)| (*name, *ty, *mode, *comptime))
                .collect();
            let full_name = host.member_callable_name(
                info.struct_type
                    .as_struct()
                    .expect("named method receiver must be a struct"),
                name,
                info.has_self,
            );
            let full_symbol = host.interner.get_or_intern(&full_name);
            let owner_token = host.function_tokens.borrow()[&host.function_symbol].clone();
            host.function_tokens
                .borrow_mut()
                .insert(full_symbol, owner_token);
            host.endpoint
                .finalize_containment_metadata()
                .ok_or_else(|| {
                    CompileError::without_span(rue_error::ErrorKind::InvalidCompilerInput(
                        "provider method containment metadata is unavailable".into(),
                    ))
                })?;
            (
                OrdinaryBodyEngine::new(&mut host).analyze_named_method_resolved(
                    &infer,
                    &full_name,
                    return_type,
                    params,
                    body,
                    info.span,
                    info.struct_type,
                    info.has_self,
                    info.self_mode,
                    info.self_is_mut,
                    info.returns_borrow,
                    info.returns_inout,
                )?,
                body_span,
            )
        }
        crate::StableDefinitionKind::Destructor => {
            let owner_name = owner_name.ok_or_else(|| {
                CompileError::without_span(rue_error::ErrorKind::InvalidCompilerInput(
                    "provider destructor body has no owner".into(),
                ))
            })?;
            let declaration = host
                .endpoint
                .destructor(owner_file, owner_name)
                .ok_or_else(|| {
                    CompileError::without_span(rue_error::ErrorKind::UndefinedFunction(format!(
                        "{owner_name}.__drop"
                    )))
                })?;
            let (body, declaration_span) = match &host.rir.rir().get(declaration).data {
                InstData::DropFnDecl { body, .. } => (*body, host.rir.rir().get(declaration).span),
                _ => unreachable!("registered provider destructor points at DropFnDecl"),
            };
            let body_span = host.rir.rir().get(body).span;
            let owner_symbol = host.interner.get_or_intern(owner_name);
            let owner_type = host
                .nominal_type_for_symbol(owner_file, owner_symbol)
                .ok_or_else(|| {
                    CompileError::without_span(rue_error::ErrorKind::UnknownType(
                        owner_name.to_owned(),
                    ))
                })?;
            let full_name = format!(
                "{}.__drop",
                host.type_pool.struct_symbol_name(
                    owner_type
                        .as_struct()
                        .expect("named destructor owner must be a struct")
                )
            );
            let full_symbol = host.interner.get_or_intern(&full_name);
            let owner_token = host.function_tokens.borrow()[&host.function_symbol].clone();
            host.function_tokens
                .borrow_mut()
                .insert(full_symbol, owner_token);
            host.endpoint
                .finalize_containment_metadata()
                .ok_or_else(|| {
                    CompileError::without_span(rue_error::ErrorKind::InvalidCompilerInput(
                        "provider destructor containment metadata is unavailable".into(),
                    ))
                })?;
            (
                OrdinaryBodyEngine::new(&mut host).analyze_named_destructor(
                    &infer,
                    &full_name,
                    body,
                    declaration_span,
                    owner_type,
                )?,
                body_span,
            )
        }
        _ => {
            return Err(CompileError::without_span(
                rue_error::ErrorKind::InvalidCompilerInput(
                    "provider body request does not own an executable body".into(),
                ),
            ));
        }
    };
    let expression_engine_ns = elapsed_ns(expression_engine_started);
    let expression_breakdown = host
        .expression_breakdown
        .expect("provider ordinary body analysis records its expression breakdown");
    let specialization_selection_started = Instant::now();
    let (mut function, warnings, strings, referenced_functions, referenced_methods) = analyzed;
    function.ordinary_owner = Some(host.owner);
    let (function, selected_calls, mut referenced_specializations) =
        crate::specialize::select_provider_body_specializations(&mut host, function)?;
    referenced_specializations.extend(referenced_methods.iter().filter_map(|(owner, method)| {
        let info = host.method_info_for_symbol(*owner, *method)?;
        let callable =
            host.member_callable_symbol(*owner, host.interner.resolve(method), info.has_self);
        host.anonymous_function_identities
            .borrow()
            .get(&callable)
            .cloned()
    }));
    referenced_specializations.extend(host.observed_comptime_producers.borrow().iter().cloned());
    host.specialized_function_identities.borrow_mut().extend(
        selected_calls
            .iter()
            .zip(referenced_specializations.iter())
            .map(|((symbol, _), instance)| (*symbol, instance.clone())),
    );
    let selected_calls = selected_calls.into_iter().collect::<AHashMap<_, _>>();
    let specialization_selection_ns = elapsed_ns(specialization_selection_started);
    let body_export_started = Instant::now();
    let export = super::semantic_body_export::export_body(
        &host,
        host.owner,
        body_span,
        &function,
        &strings,
        &warnings,
        Some(&selected_calls),
        &referenced_methods,
    )
    .map_err(|failure| {
        CompileError::without_span(rue_error::ErrorKind::OutputPublication(format!(
            "provider body export failed: {failure:?}"
        )))
    })?;
    let body_export_ns = elapsed_ns(body_export_started);
    let result_projection_started = Instant::now();
    let mut referenced_definitions = referenced_functions
        .iter()
        .filter_map(|symbol| {
            host.function_tokens
                .borrow()
                .get(symbol)
                .map(|(_, key)| key.clone())
        })
        .collect::<AHashSet<_>>();
    referenced_definitions.extend(
        referenced_methods
            .iter()
            .filter_map(|(owner, method)| host.named_method_definition(*owner, *method)),
    );
    let referenced_definitions = referenced_definitions.into_iter().collect();
    let referenced_values = host
        .observed_named_definitions
        .borrow()
        .iter()
        .cloned()
        .collect();
    let produced_anonymous_nominals = host
        .produced_anonymous_nominals(&initial_anonymous_identities)
        .map_err(|failure| {
            CompileError::without_span(rue_error::ErrorKind::OutputPublication(format!(
                "provider ordinary produced-nominal export failed: {failure:?}"
            )))
        })?;
    let work = host.provider_body_work.snapshot();
    let definition_tokens = host
        .function_tokens
        .into_inner()
        .into_values()
        .chain(host.nominal_tokens.into_inner().into_values())
        .chain(
            host.anonymous_definition_tokens
                .into_inner()
                .into_iter()
                .map(|(key, token)| (token, key)),
        )
        .collect();
    let module_tokens = host.module_tokens.into_inner().into_values().collect();
    let result_projection_ns = elapsed_ns(result_projection_started);
    publish_provider_body_breakdown(
        host_setup_ns,
        expression_engine_ns,
        specialization_selection_ns,
        body_export_ns,
        result_projection_ns,
        expression_breakdown,
    );
    Ok(ProviderOrdinaryBody {
        owner: host.owner,
        work,
        export,
        function,
        warnings,
        strings,
        referenced_functions,
        referenced_methods,
        referenced_definitions,
        referenced_values,
        referenced_specializations,
        produced_anonymous_nominals,
        type_pool: host.type_pool,
        interner: host.interner,
        definition_tokens,
        module_tokens,
    })
}

fn anonymous_member_in_producer(
    rir: &rue_rir::Rir,
    interner: &ThreadedRodeo,
    producer_root: InstRef,
    owner_anchor: &rue_rir::RirStructuralAnchor,
    member: &crate::AnonymousMemberKey,
) -> Result<InstRef, &'static str> {
    let member_symbol = interner
        .get(member.name.as_ref())
        .ok_or("anonymous member name is absent from its producer artifact")?;
    let mut pending = vec![producer_root];
    let mut visited = AHashSet::new();
    let mut owner_found = false;
    let mut declaration = None;
    while let Some(reference) = pending.pop() {
        if !visited.insert(reference) {
            continue;
        }
        let instruction = rir.get(reference);
        if let InstData::AnonStructType {
            anchor, methods, ..
        } = &instruction.data
        {
            // Anonymous method bodies are independent semantic producers. A
            // nested producer can legitimately reuse the same relative anchor,
            // so never cross method edges while locating an owner in this root.
            if anchor != owner_anchor {
                continue;
            }
            if owner_found {
                return Err("anonymous owner anchor is duplicated in its producer artifact");
            }
            owner_found = true;
            for method_ref in rir.anon_struct_methods(methods) {
                let InstData::FnDecl { name, has_self, .. } = &rir.get(method_ref).data else {
                    return Err("anonymous owner method edge does not reference a function");
                };
                let kind = if interner.resolve(name) == "__drop" {
                    crate::AnonymousMemberKind::Destructor
                } else if *has_self {
                    crate::AnonymousMemberKind::Method
                } else {
                    crate::AnonymousMemberKind::AssociatedFunction
                };
                if *name == member_symbol
                    && kind == member.kind
                    && declaration.replace(method_ref).is_some()
                {
                    return Err("anonymous member is duplicated in its producer artifact");
                }
            }
            continue;
        }
        rir.child_instructions(reference, &mut pending);
    }
    if !owner_found {
        return Err("anonymous owner anchor is absent from its producer artifact");
    }
    declaration.ok_or("anonymous member is absent from its producer artifact")
}

fn anonymous_producer_root<K, M>(
    rir: &rue_rir::Rir,
    interner: &ThreadedRodeo,
    candidate_root: InstRef,
    source_key: &K,
    producer: &crate::StableProducerId<K, M>,
) -> Result<InstRef, &'static str>
where
    K: Eq,
{
    match producer {
        crate::StableProducerId::Definition(key) if key == source_key => Ok(candidate_root),
        crate::StableProducerId::Definition(_) => {
            Err("anonymous producer definition disagrees with its candidate artifact")
        }
        crate::StableProducerId::Function(function) => {
            anonymous_function_producer_root(rir, interner, candidate_root, source_key, function)
        }
    }
}

fn anonymous_function_producer_root<K, M>(
    rir: &rue_rir::Rir,
    interner: &ThreadedRodeo,
    candidate_root: InstRef,
    source_key: &K,
    function: &crate::FunctionInstanceKey<K, M>,
) -> Result<InstRef, &'static str>
where
    K: Eq,
{
    match function {
        crate::FunctionInstanceKey::Definition(key) if key == source_key => Ok(candidate_root),
        crate::FunctionInstanceKey::Definition(_) => {
            Err("anonymous function producer disagrees with its candidate artifact")
        }
        crate::FunctionInstanceKey::Specialization { base, .. } => {
            anonymous_function_producer_root(rir, interner, candidate_root, source_key, base)
        }
        crate::FunctionInstanceKey::AnonymousMember { owner, member } => {
            let crate::TypeInstanceKey::Nominal(crate::NominalInstanceKey::Anonymous(owner)) =
                owner.as_ref()
            else {
                return Err("nested anonymous producer has a non-anonymous owner");
            };
            let producer_root = anonymous_producer_root(
                rir,
                interner,
                candidate_root,
                source_key,
                &owner.producer,
            )?;
            anonymous_member_in_producer(rir, interner, producer_root, &owner.anchor, member)
        }
        crate::FunctionInstanceKey::DropGlue(_) => {
            Err("drop glue cannot produce an anonymous member body")
        }
    }
}

/// Run one exact anonymous-member request from the canonical artifact of the
/// named declaration that ultimately produced its owner. Producer boundaries,
/// structural anchors, and exact member identity select the nested declaration
/// without source assembly, parsing, AstGen, or a fake named owner.
pub fn analyze_provider_anonymous_body<P, S, K, M>(
    provider: &P,
    source: S,
    bundle: &BodyRirBundle,
    candidate_root: InstRef,
    source_key: K,
    owner: &TypeInstanceKey<K, M>,
    member: &crate::AnonymousMemberKey,
    target: Target,
    preview: PreviewFeatures,
    well_known: &ProviderWellKnownOptionFacts<K, M>,
) -> CompileResult<ProviderAnonymousBody<K, M>>
where
    P: BodyFactProvider,
    S: DurableNominalSource<K, M>
        + DurableAnonymousSource<K, M>
        + DurableCallableSource<K, M>
        + DurableConstSource<K, M>
        + DurableBodyLookupSource<K, M>,
    K: Clone + Eq + Hash + Ord,
    M: Clone + Eq + Hash + Ord,
{
    let owner_file = bundle.source_file_id().ok_or_else(|| {
        CompileError::without_span(rue_error::ErrorKind::InvalidCompilerInput(
            "provider anonymous body RIR does not have one source file".into(),
        ))
    })?;
    let TypeInstanceKey::Nominal(crate::NominalInstanceKey::Anonymous(durable_owner)) = owner
    else {
        return Err(CompileError::without_span(
            rue_error::ErrorKind::InvalidCompilerInput(
                "anonymous member owner is not an anonymous nominal".into(),
            ),
        ));
    };
    let declaration = {
        let view = bundle.view();
        let producer_root = anonymous_producer_root(
            view.rir(),
            view.rir_interner(),
            candidate_root,
            &source_key,
            &durable_owner.producer,
        )
        .map_err(|detail| {
            CompileError::without_span(rue_error::ErrorKind::InvalidCompilerInput(detail.into()))
        })?;
        anonymous_member_in_producer(
            view.rir(),
            view.rir_interner(),
            producer_root,
            &durable_owner.anchor,
            member,
        )
        .map_err(|detail| {
            CompileError::without_span(rue_error::ErrorKind::InvalidCompilerInput(detail.into()))
        })?
    };
    let host_setup_started = Instant::now();
    let mut host = ProviderBodyHost::new(
        provider,
        source,
        bundle,
        source_key.clone(),
        owner_file,
        member.name.as_ref(),
        match member.kind {
            crate::AnonymousMemberKind::Method => crate::StableDefinitionKind::Method,
            crate::AnonymousMemberKind::AssociatedFunction => {
                crate::StableDefinitionKind::AssociatedFunction
            }
            crate::AnonymousMemberKind::Destructor => crate::StableDefinitionKind::Destructor,
        },
        None,
        target,
        preview,
        well_known,
    )
    .ok_or_else(|| {
        CompileError::without_span(rue_error::ErrorKind::InvalidCompilerInput(
            "provider anonymous body host could not be constructed".into(),
        ))
    })?;
    host.current_declaration_override = Some(declaration);
    let issued_owner = host
        .register_and_issue_anonymous_identity(durable_owner)
        .ok_or_else(|| {
            CompileError::without_span(rue_error::ErrorKind::InvalidCompilerInput(
                "anonymous member owner identities are unavailable".into(),
            ))
        })?;
    host.endpoint
        .register_anonymous_nominal(issued_owner.clone(), (**durable_owner).clone());
    let owner_type = host
        .state
        .identity_context()
        .pool_mut()
        .ok_or_else(|| {
            CompileError::without_span(rue_error::ErrorKind::InvalidCompilerInput(
                "anonymous member owner identity pool is unavailable".into(),
            ))
        })?
        .resolve_provider_type(&crate::SemanticImportType::AnonymousNominal(
            (**durable_owner).clone(),
        ))
        .map_err(|failure| {
            CompileError::without_span(rue_error::ErrorKind::InvalidCompilerInput(format!(
                "anonymous member owner shape is unavailable: {failure:?}"
            )))
        })?;
    host.endpoint
        .finalize_containment_metadata()
        .ok_or_else(|| {
            CompileError::without_span(rue_error::ErrorKind::InvalidCompilerInput(
                "anonymous member owner containment metadata is unavailable".into(),
            ))
        })?;
    let struct_id = owner_type.as_struct().ok_or_else(|| {
        CompileError::without_span(rue_error::ErrorKind::InvalidCompilerInput(
            "anonymous member owner is not a struct".into(),
        ))
    })?;
    host.canonical_anonymous_types
        .insert(owner_type, issued_owner.clone());
    host.anon_struct_identities
        .insert(issued_owner.clone(), struct_id);
    host.anonymous_struct_ids.insert(struct_id);
    let type_captures = host.source.anonymous_type_captures(durable_owner);
    let mut type_subst = AHashMap::with_capacity(type_captures.len());
    for (name, durable_type) in type_captures {
        let ty = host
            .state
            .identity_context()
            .pool_mut()
            .and_then(|mut pool| pool.resolve_provider_type(&durable_type).ok())
            .ok_or_else(|| {
                CompileError::without_span(rue_error::ErrorKind::InvalidCompilerInput(
                    "anonymous member type capture cannot be materialized".into(),
                ))
            })?;
        let symbol = host
            .interner
            .get_or_intern(&ty.safe_name_with_pool(Some(&host.type_pool)));
        if let Some(id) = ty.as_struct() {
            host.generated_structs.insert(symbol, id);
        } else if let Some(id) = ty.as_enum() {
            host.generated_enums.insert(symbol, id);
        }
        if host.endpoint.durable_anonymous_identity(ty).is_some() {
            host.issued_anonymous_identity_for_type(ty).ok_or_else(|| {
                CompileError::without_span(rue_error::ErrorKind::InvalidCompilerInput(
                    "anonymous member type capture identity cannot be issued".into(),
                ))
            })?;
        } else if host.endpoint.durable_named_identity(ty).is_some() {
            host.ensure_named_nominal_identity(ty, host.interner.resolve(&symbol))
                .map_err(|_| {
                    CompileError::without_span(rue_error::ErrorKind::InvalidCompilerInput(
                        "anonymous member type capture identity cannot be registered".into(),
                    ))
                })?;
        }
        type_subst.insert(host.interner.get_or_intern(name.as_ref()), ty);
    }
    if !type_subst.is_empty() {
        host.anon_struct_type_subst.insert(struct_id, type_subst);
    }
    let value_captures = host.source.anonymous_value_captures(durable_owner);
    let mut captured_values = AHashMap::with_capacity(value_captures.len());
    for (name, durable_value) in value_captures {
        let value = host
            .materialize_durable_const_value(&durable_value)
            .ok_or_else(|| {
                CompileError::without_span(rue_error::ErrorKind::InvalidCompilerInput(
                    "anonymous member value capture cannot be materialized".into(),
                ))
            })?;
        captured_values.insert(host.interner.get_or_intern(name.as_ref()), value);
    }
    if !captured_values.is_empty() {
        host.anon_struct_captured_values
            .insert(struct_id, captured_values);
    }
    let initial_anonymous_identities = host
        .canonical_anonymous_types
        .values()
        .cloned()
        .collect::<AHashSet<_>>();

    let (params, body, has_self, self_mode, self_is_mut, returns_borrow, returns_inout, span) =
        match &host.rir.rir().get(declaration).data {
            InstData::FnDecl {
                params,
                body,
                has_self,
                self_mode,
                self_is_mut,
                returns_borrow,
                returns_inout,
                ..
            } => (
                params.clone(),
                *body,
                *has_self,
                *self_mode,
                *self_is_mut,
                *returns_borrow,
                *returns_inout,
                host.rir.rir().get(declaration).span,
            ),
            _ => {
                return Err(CompileError::without_span(
                    rue_error::ErrorKind::InvalidCompilerInput(
                        "anonymous member fragment did not lower to a method".into(),
                    ),
                ));
            }
        };
    let expected_kind = if member.name.as_ref() == "__drop" {
        crate::AnonymousMemberKind::Destructor
    } else if has_self {
        crate::AnonymousMemberKind::Method
    } else {
        crate::AnonymousMemberKind::AssociatedFunction
    };
    if expected_kind != member.kind {
        return Err(CompileError::without_span(
            rue_error::ErrorKind::InvalidCompilerInput(
                "anonymous member kind disagrees with its producer fragment".into(),
            ),
        ));
    }
    let issued_identity = FunctionInstanceKey::AnonymousMember {
        owner: Node::new(TypeInstanceKey::Nominal(
            crate::NominalInstanceKey::Anonymous(Node::new(issued_owner.clone())),
        )),
        member: member.clone(),
    };
    host.current_anonymous_identity = Some(issued_identity.clone());
    host.register_provider_anonymous_method_endpoints_with_issued(
        durable_owner,
        owner_type,
        issued_owner,
    )
    .ok_or_else(|| {
        CompileError::without_span(rue_error::ErrorKind::InvalidCompilerInput(
            "anonymous member sibling endpoints are unavailable".into(),
        ))
    })?;
    let full_name = host.member_callable_name(struct_id, &member.name, has_self);
    let full_symbol = host.interner.get_or_intern(&full_name);
    host.function_symbol = full_symbol;
    let owner_token = host
        .function_tokens
        .borrow()
        .values()
        .next()
        .cloned()
        .ok_or_else(|| {
            CompileError::without_span(rue_error::ErrorKind::InvalidCompilerInput(
                "anonymous member has no producer token".into(),
            ))
        })?;
    host.function_tokens
        .borrow_mut()
        .insert(full_symbol, owner_token);

    let params = host
        .rir
        .rir()
        .params(&params)
        .values()
        .collect::<Vec<RirParam>>();
    let projected = host
        .source
        .anonymous_methods(durable_owner)
        .into_iter()
        .find(|candidate| candidate.name == member.name)
        .ok_or_else(|| {
            CompileError::without_span(rue_error::ErrorKind::InvalidCompilerInput(
                "anonymous member signature is unavailable from its producer".into(),
            ))
        })?;
    if projected.has_self != has_self
        || projected.self_mode != self_mode
        || projected.parameters.len() != params.len()
        || !projected
            .parameters
            .iter()
            .zip(&params)
            .all(|((_, mode, comptime), parameter)| {
                *mode == parameter.mode && *comptime == parameter.is_comptime
            })
    {
        return Err(CompileError::without_span(
            rue_error::ErrorKind::InvalidCompilerInput(
                "anonymous member signature disagrees with its producer fragment".into(),
            ),
        ));
    }
    let materialize = |host: &mut ProviderBodyHost<'_, P, S, K, M>,
                       ty: &crate::DurableAnonymousMethodType<K, M>| {
        let ty = match ty {
            crate::DurableAnonymousMethodType::SelfType => owner_type,
            crate::DurableAnonymousMethodType::Concrete(ty) => host
                .state
                .identity_context()
                .pool_mut()?
                .resolve_provider_type(ty)
                .ok()?,
        };
        let symbol = host
            .interner
            .get_or_intern(&ty.safe_name_with_pool(Some(&host.type_pool)));
        if host.endpoint.durable_anonymous_identity(ty).is_some() {
            host.issued_anonymous_identity_for_type(ty)?;
        } else if host.endpoint.durable_named_identity(ty).is_some() {
            host.ensure_named_nominal_identity(ty, host.interner.resolve(&symbol))
                .ok()?;
        }
        Some(ty)
    };
    let mut resolved_params = params
        .iter()
        .zip(&projected.parameters)
        .map(|(parameter, (projected_type, _, _))| {
            materialize(&mut host, projected_type)
                .map(|ty| (parameter.name, ty, parameter.mode, parameter.is_comptime))
                .ok_or_else(|| {
                    CompileError::without_span(rue_error::ErrorKind::InvalidCompilerInput(
                        "anonymous member parameter type cannot be materialized".into(),
                    ))
                })
        })
        .collect::<CompileResult<Vec<_>>>()?;
    let return_type = materialize(&mut host, &projected.result).ok_or_else(|| {
        CompileError::without_span(rue_error::ErrorKind::InvalidCompilerInput(
            "anonymous member result type cannot be materialized".into(),
        ))
    })?;
    let infer = InferenceContext::new(&host);
    let host_setup_ns = elapsed_ns(host_setup_started);
    let expression_engine_started = Instant::now();
    if has_self {
        resolved_params.insert(
            0,
            (
                host.interner.get_or_intern("self"),
                owner_type,
                self_mode,
                false,
            ),
        );
    }
    let analyzed = OrdinaryBodyEngine::new(&mut host).analyze_method_with_identity_kind_resolved(
        &infer,
        issued_identity.clone(),
        &full_name,
        return_type,
        resolved_params,
        body,
        span,
        owner_type,
        self_is_mut,
        member.kind == crate::AnonymousMemberKind::Destructor,
        returns_borrow || returns_inout,
    )?;
    let expression_engine_ns = elapsed_ns(expression_engine_started);
    let expression_breakdown = host
        .expression_breakdown
        .expect("provider anonymous body analysis records its expression breakdown");
    let specialization_selection_started = Instant::now();
    let (function, warnings, strings, referenced_functions, referenced_methods) = analyzed;
    let (function, selected_calls, mut referenced_specializations) =
        crate::specialize::select_provider_body_specializations(&mut host, function)?;
    referenced_specializations.extend(referenced_methods.iter().filter_map(|(owner, method)| {
        let info = host.method_info_for_symbol(*owner, *method)?;
        let callable =
            host.member_callable_symbol(*owner, host.interner.resolve(method), info.has_self);
        host.anonymous_function_identities
            .borrow()
            .get(&callable)
            .cloned()
    }));
    referenced_specializations.extend(host.observed_comptime_producers.borrow().iter().cloned());
    host.specialized_function_identities.borrow_mut().extend(
        selected_calls
            .iter()
            .zip(referenced_specializations.iter())
            .map(|((symbol, _), instance)| (*symbol, instance.clone())),
    );
    let selected_calls = selected_calls.into_iter().collect::<AHashMap<_, _>>();
    let body_span = host.rir.rir().get(body).span;
    let specialization_selection_ns = elapsed_ns(specialization_selection_started);
    let body_export_started = Instant::now();
    let export = super::semantic_body_export::export_body(
        &host,
        crate::BodyOwnerToken::new(0, 0),
        body_span,
        &function,
        &strings,
        &warnings,
        Some(&selected_calls),
        &referenced_methods,
    )
    .map(|export| crate::SemanticAnonymousBodyExport {
        identity: issued_identity,
        body: export.body,
    })
    .map_err(|failure| {
        CompileError::without_span(rue_error::ErrorKind::OutputPublication(format!(
            "provider anonymous body export failed: {failure:?}"
        )))
    })?;
    let body_export_ns = elapsed_ns(body_export_started);
    let result_projection_started = Instant::now();
    let produced_anonymous_nominals = host
        .produced_anonymous_nominals(&initial_anonymous_identities)
        .map_err(|failure| {
            CompileError::without_span(rue_error::ErrorKind::OutputPublication(format!(
                "provider anonymous produced-nominal export failed: {failure:?}"
            )))
        })?;
    let mut referenced_definitions = referenced_functions
        .iter()
        .filter_map(|symbol| {
            host.function_tokens
                .borrow()
                .get(symbol)
                .map(|(_, key)| key.clone())
        })
        .collect::<AHashSet<_>>();
    referenced_definitions.extend(
        referenced_methods
            .iter()
            .filter_map(|(owner, method)| host.named_method_definition(*owner, *method)),
    );
    let referenced_definitions = referenced_definitions.into_iter().collect();
    let referenced_values = host
        .observed_named_definitions
        .borrow()
        .iter()
        .cloned()
        .collect();
    let work = host.provider_body_work.snapshot();
    let definition_tokens = host
        .function_tokens
        .into_inner()
        .into_values()
        .chain(host.nominal_tokens.into_inner().into_values())
        .chain(
            host.anonymous_definition_tokens
                .into_inner()
                .into_iter()
                .map(|(key, token)| (token, key)),
        )
        .collect();
    let module_tokens = host.module_tokens.into_inner().into_values().collect();
    let result_projection_ns = elapsed_ns(result_projection_started);
    publish_provider_body_breakdown(
        host_setup_ns,
        expression_engine_ns,
        specialization_selection_ns,
        body_export_ns,
        result_projection_ns,
        expression_breakdown,
    );
    Ok(ProviderAnonymousBody {
        work,
        export,
        body_span,
        function,
        warnings,
        strings,
        type_pool: host.type_pool,
        interner: host.interner,
        produced_anonymous_nominals,
        referenced_definitions,
        referenced_values,
        referenced_specializations,
        definition_tokens,
        module_tokens,
    })
}

/// Run one exact specialization request through the provider-backed body host.
pub fn analyze_provider_specialized_body<P, S, K, M>(
    provider: &P,
    source: S,
    bundle: &BodyRirBundle,
    base: K,
    name: &str,
    arguments: &crate::CanonicalArguments<K, M>,
    target: Target,
    preview: PreviewFeatures,
    well_known: &ProviderWellKnownOptionFacts<K, M>,
) -> CompileResult<ProviderSpecializedBody<K, M>>
where
    P: BodyFactProvider,
    S: DurableNominalSource<K, M>
        + DurableAnonymousSource<K, M>
        + DurableCallableSource<K, M>
        + DurableConstSource<K, M>
        + DurableBodyLookupSource<K, M>,
    K: Clone + Eq + Hash + Ord,
    M: Clone + Eq + Hash + Ord,
{
    let owner_file = bundle.source_file_id().ok_or_else(|| {
        CompileError::without_span(rue_error::ErrorKind::InvalidCompilerInput(
            "provider specialized body RIR does not have one source file".into(),
        ))
    })?;
    let host_setup_started = Instant::now();
    let mut host = ProviderBodyHost::new(
        provider,
        source,
        bundle,
        base,
        owner_file,
        name,
        crate::StableDefinitionKind::Function,
        None,
        target,
        preview,
        well_known,
    )
    .ok_or_else(|| {
        CompileError::without_span(rue_error::ErrorKind::InvalidCompilerInput(
            "provider specialization host could not be constructed".into(),
        ))
    })?;
    // A generic free function reaches analysis only through its
    // specializations, so the 6.6:3/6.6:4 accessor gate runs here as well.
    if let Some(declaration) = host.endpoint.first_free_function(name, owner_file) {
        host.reject_free_function_accessor(declaration)?;
    }
    let initial_anonymous_identities = host
        .canonical_anonymous_types
        .values()
        .cloned()
        .collect::<AHashSet<_>>();
    let type_args = arguments
        .types
        .iter()
        .map(|ty| host.materialize_type_instance(ty))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|failure| {
            CompileError::without_span(rue_error::ErrorKind::InvalidCompilerInput(format!(
                "provider specialization type argument is unavailable: {failure:?}"
            )))
        })?;
    let value_args = arguments
        .values
        .iter()
        .map(|value| host.materialize_argument_value(value))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|failure| {
            CompileError::without_span(rue_error::ErrorKind::InvalidCompilerInput(format!(
                "provider specialization value argument is unavailable: {failure:?}"
            )))
        })?;
    let key = crate::specialize::SpecializationKey {
        base_name: host.function_symbol,
        type_args,
        value_args,
    };
    host.endpoint
        .finalize_containment_metadata()
        .ok_or_else(|| {
            CompileError::without_span(rue_error::ErrorKind::InvalidCompilerInput(
                "provider specialization containment metadata is unavailable".into(),
            ))
        })?;
    let infer = InferenceContext::new(&host);
    let host_setup_ns = elapsed_ns(host_setup_started);
    let expression_engine_started = Instant::now();
    let specialized =
        crate::specialize::analyze_one_specialization_with_host(&mut host, &infer, key)?;
    let expression_engine_ns = elapsed_ns(expression_engine_started);
    let expression_breakdown = host
        .expression_breakdown
        .expect("provider specialized body analysis records its expression breakdown");
    let body_span = host
        .rir
        .rir()
        .get(
            host.endpoint
                .endpoint_function_info(host.function_symbol)
                .ok_or_else(|| {
                    CompileError::without_span(rue_error::ErrorKind::UndefinedFunction(
                        name.to_owned(),
                    ))
                })?
                .body,
        )
        .span;
    let specialization_selection_started = Instant::now();
    let (function, selected_calls, referenced_specializations) =
        crate::specialize::select_provider_body_specializations(&mut host, specialized.function)?;
    host.specialized_function_identities.borrow_mut().extend(
        selected_calls
            .iter()
            .zip(referenced_specializations.iter())
            .map(|((symbol, _), instance)| (*symbol, instance.clone())),
    );
    let selected_calls = selected_calls.into_iter().collect::<AHashMap<_, _>>();
    let specialization_selection_ns = elapsed_ns(specialization_selection_started);
    let body_export_started = Instant::now();
    let body = super::semantic_body_export::export_body(
        &host,
        host.owner,
        body_span,
        &function,
        &specialized.local_strings,
        &specialized.warnings,
        Some(&selected_calls),
        &specialized.referenced_methods,
    )
    .map_err(|failure| {
        CompileError::without_span(rue_error::ErrorKind::OutputPublication(format!(
            "provider specialization export failed: {failure:?}"
        )))
    })?
    .body;
    let body_export_ns = elapsed_ns(body_export_started);
    let result_projection_started = Instant::now();
    let mut referenced_definitions = specialized
        .referenced_functions
        .iter()
        .filter_map(|symbol| {
            host.function_tokens
                .borrow()
                .get(symbol)
                .map(|(_, key)| key.clone())
        })
        .collect::<AHashSet<_>>();
    referenced_definitions.extend(
        specialized
            .referenced_methods
            .iter()
            .filter_map(|(owner, method)| host.named_method_definition(*owner, *method)),
    );
    let referenced_definitions = referenced_definitions.into_iter().collect();
    let referenced_values = host
        .observed_named_definitions
        .borrow()
        .iter()
        .cloned()
        .collect();
    let export = crate::SemanticSpecializedBodyExport {
        identity: specialized.identity,
        body,
        dependencies: specialized.dependencies.into(),
        dependency_boundary_complete: specialized.dependency_boundary_complete,
    };
    let produced_anonymous_nominals = host
        .produced_anonymous_nominals(&initial_anonymous_identities)
        .map_err(|failure| {
            CompileError::without_span(rue_error::ErrorKind::OutputPublication(format!(
                "provider specialization produced-nominal export failed: {failure:?}"
            )))
        })?;
    let work = host.provider_body_work.snapshot();
    let definition_tokens = host
        .function_tokens
        .into_inner()
        .into_values()
        .chain(host.nominal_tokens.into_inner().into_values())
        .chain(
            host.anonymous_definition_tokens
                .into_inner()
                .into_iter()
                .map(|(key, token)| (token, key)),
        )
        .collect();
    let module_tokens = host.module_tokens.into_inner().into_values().collect();
    let result_projection_ns = elapsed_ns(result_projection_started);
    publish_provider_body_breakdown(
        host_setup_ns,
        expression_engine_ns,
        specialization_selection_ns,
        body_export_ns,
        result_projection_ns,
        expression_breakdown,
    );
    Ok(ProviderSpecializedBody {
        work,
        export,
        function,
        warnings: specialized.warnings,
        strings: specialized.local_strings,
        type_pool: host.type_pool,
        interner: host.interner,
        produced_anonymous_nominals,
        referenced_definitions,
        referenced_values,
        referenced_specializations,
        definition_tokens,
        module_tokens,
    })
}
