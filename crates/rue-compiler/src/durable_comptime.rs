//! Durable compile-time values shared by the legacy evaluator and the AIR
//! adapter boundary.
//!
//! The value cases in this module are deliberately the existing durable
//! evaluator representation.  The AIR implementations below are only an
//! adapter over that representation; they do not define a second value
//! algebra or perform evaluation.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use rue_air::{ComptimeType, ComptimeValue};
use rue_query::QueryAbort;

use crate::ModuleId;
use crate::body_query::ForeignComptimeCallLookup;
use crate::declaration_candidate::{DeclarationCandidateKey, DeclarationImportFailure};
use crate::durable_semantics::{DurableConstValue, DurableType};
use crate::semantic_query_nucleus::SemanticNucleusFailure;

type DurableAnonymousNominal = crate::durable_semantics::DurableAnonymousNominal;
type SemanticDeclarationDependency = crate::semantic_query_nucleus::SemanticDeclarationDependency;
type DeferredOwnershipGate = crate::semantic_query_nucleus::DeferredOwnershipGate;
type DeferredOwnershipApplication = crate::semantic_query_nucleus::DeferredOwnershipApplication;

/// Effects observed while reducing one durable comptime root.
///
/// The collections deliberately use the same canonical keys and ordering as
/// the semantic nucleus. Anonymous nominals replace an earlier observation at
/// the same identity, while dependencies and ownership gates are set-unioned;
/// this is the existing publication behavior, expressed as one operation
/// boundary for the future AIR host.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct DurableComptimeEffects {
    anonymous_nominals: BTreeMap<crate::AnonymousNominalKey, DurableAnonymousNominal>,
    dependencies: BTreeSet<SemanticDeclarationDependency>,
    deferred_ownership: BTreeSet<DeferredOwnershipGate>,
}

impl DurableComptimeEffects {
    pub(crate) fn observe_anonymous_nominal(&mut self, nominal: DurableAnonymousNominal) {
        self.anonymous_nominals
            .insert(nominal.identity.clone(), nominal);
    }

    pub(crate) fn observe_dependency(&mut self, dependency: SemanticDeclarationDependency) {
        self.dependencies.insert(dependency);
    }

    pub(crate) fn observe_deferred_ownership(&mut self, gate: DeferredOwnershipGate) {
        self.deferred_ownership.insert(gate);
    }

    #[allow(dead_code)] // used by the root-integrated AIR host in the next slice
    pub(crate) fn merge_child(
        &mut self,
        child: DurableComptimeEffects,
        application: Option<DeferredOwnershipApplication>,
    ) {
        merge_effects_into(
            &mut self.anonymous_nominals,
            &mut self.dependencies,
            &mut self.deferred_ownership,
            child.anonymous_nominals.into_values(),
            child.dependencies,
            child.deferred_ownership,
            application,
        );
    }

    pub(crate) fn merge_projection(
        &mut self,
        anonymous_nominals: &[DurableAnonymousNominal],
        dependencies: &[SemanticDeclarationDependency],
        deferred_ownership: &[DeferredOwnershipGate],
        application: Option<DeferredOwnershipApplication>,
    ) {
        merge_effects_into(
            &mut self.anonymous_nominals,
            &mut self.dependencies,
            &mut self.deferred_ownership,
            anonymous_nominals.iter().cloned(),
            dependencies.iter().cloned(),
            deferred_ownership.iter().cloned(),
            application,
        );
    }

    #[allow(dead_code)] // publication adapters consume the canonical projection directly
    pub(crate) fn anonymous_nominals(&self) -> impl Iterator<Item = &DurableAnonymousNominal> {
        self.anonymous_nominals.values()
    }

    #[allow(dead_code)] // publication adapters consume the canonical projection directly
    pub(crate) fn dependencies(&self) -> impl Iterator<Item = &SemanticDeclarationDependency> {
        self.dependencies.iter()
    }

    #[allow(dead_code)] // publication adapters consume the canonical projection directly
    pub(crate) fn deferred_ownership(&self) -> impl Iterator<Item = &DeferredOwnershipGate> {
        self.deferred_ownership.iter()
    }

    #[allow(dead_code)] // root publication owns the empty-result fast path
    pub(crate) fn is_empty(&self) -> bool {
        self.anonymous_nominals.is_empty()
            && self.dependencies.is_empty()
            && self.deferred_ownership.is_empty()
    }

    pub(crate) fn apply_to(
        self,
        anonymous_nominals: &mut BTreeMap<crate::AnonymousNominalKey, DurableAnonymousNominal>,
        dependencies: &mut BTreeSet<SemanticDeclarationDependency>,
        deferred_ownership: &mut BTreeSet<DeferredOwnershipGate>,
        application: Option<DeferredOwnershipApplication>,
    ) {
        merge_effects_into(
            anonymous_nominals,
            dependencies,
            deferred_ownership,
            self.anonymous_nominals.into_values(),
            self.dependencies,
            self.deferred_ownership,
            application,
        );
    }
}

/// The one canonical publication kernel for durable comptime effects.
/// Every root-local merge, borrowed projection merge, and provider publication
/// uses this operation so nominal replacement, set union, and deferred
/// application filling cannot drift between entry points.
fn merge_effects_into(
    anonymous_nominals: &mut BTreeMap<crate::AnonymousNominalKey, DurableAnonymousNominal>,
    dependencies: &mut BTreeSet<SemanticDeclarationDependency>,
    deferred_ownership: &mut BTreeSet<DeferredOwnershipGate>,
    observed_nominals: impl IntoIterator<Item = DurableAnonymousNominal>,
    observed_dependencies: impl IntoIterator<Item = SemanticDeclarationDependency>,
    observed_deferred: impl IntoIterator<Item = DeferredOwnershipGate>,
    application: Option<DeferredOwnershipApplication>,
) {
    for nominal in observed_nominals {
        anonymous_nominals.insert(nominal.identity.clone(), nominal);
    }
    dependencies.extend(observed_dependencies);
    deferred_ownership.extend(observed_deferred.into_iter().map(|mut gate| {
        if gate.application.is_none() {
            gate.application = application.clone();
        }
        gate
    }));
}

/// The exact identity carried by an admitted foreign call.
///
/// The fields are private deliberately: a caller cannot construct a query
/// whose configuration, ordered arguments, producer, and program disagree.
/// The only production constructor derives all of them from the owned
/// admission payload.
#[allow(dead_code)] // carried by the root-integrated AIR host in the next slice
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DurableComptimeCallContext {
    query: crate::semantic_query_nucleus::ComptimeCallQueryKey,
    call_ordinal: u32,
    parent_producer: crate::StableDefinitionKey,
    parent_declaration: crate::declaration_candidate::DeclarationCandidateKey,
    child_producer: crate::StableDefinitionKey,
    program: crate::body_query::ForeignComptimeProgramKey,
}

impl DurableComptimeCallContext {
    #[allow(dead_code)]
    pub(crate) fn from_admitted(
        admitted: &crate::body_query::OwnedForeignComptimeProgram,
        parent_producer: crate::StableDefinitionKey,
        parent_declaration: crate::declaration_candidate::DeclarationCandidateKey,
        call_ordinal: u32,
    ) -> Result<Self, DurableComptimeLifecycleError> {
        let child_producer = admitted.plan.key.producer.clone();
        let Some(child_declaration) =
            crate::revisioned_query_database::declaration_candidate_for_stable_key(&child_producer)
        else {
            return Err(DurableComptimeLifecycleError::InvalidContext);
        };
        if child_declaration != admitted.plan.candidate
            || admitted.callable.context != child_declaration.module
        {
            return Err(DurableComptimeLifecycleError::InvalidContext);
        }
        let Some(expected_parent) =
            crate::revisioned_query_database::declaration_candidate_for_stable_key(
                &parent_producer,
            )
        else {
            return Err(DurableComptimeLifecycleError::InvalidContext);
        };
        if expected_parent != parent_declaration {
            return Err(DurableComptimeLifecycleError::InvalidContext);
        }
        let configuration = admitted.plan.key.configuration.clone();
        let query = crate::semantic_query_nucleus::ComptimeCallQueryKey {
            declaration: crate::semantic_query_nucleus::DeclarationSemanticQueryKey {
                declaration: child_declaration,
                configuration: configuration.clone(),
            },
            type_arguments: admitted.seed.type_arguments.clone(),
            value_arguments: admitted.seed.value_arguments.clone(),
        };
        Ok(Self {
            query,
            call_ordinal,
            parent_producer,
            parent_declaration,
            child_producer: child_producer.clone(),
            program: crate::body_query::ForeignComptimeProgramKey {
                producer: child_producer,
                configuration,
            },
        })
    }

    #[cfg(test)]
    fn for_test(
        parent_producer: crate::StableDefinitionKey,
        parent_declaration: crate::declaration_candidate::DeclarationCandidateKey,
        child_producer: crate::StableDefinitionKey,
        call_ordinal: u32,
    ) -> Self {
        let configuration = crate::semantic_query_nucleus::SemanticQueryConfiguration {
            target: rue_target::Target::X86_64Linux,
            preview_features: crate::StablePreviewFeatures::new(&crate::PreviewFeatures::default()),
        };
        let child_declaration =
            crate::revisioned_query_database::declaration_candidate_for_stable_key(&child_producer)
                .unwrap();
        Self {
            query: crate::semantic_query_nucleus::ComptimeCallQueryKey {
                declaration: crate::semantic_query_nucleus::DeclarationSemanticQueryKey {
                    declaration: child_declaration,
                    configuration: configuration.clone(),
                },
                type_arguments: Arc::from([]),
                value_arguments: Arc::from([]),
            },
            call_ordinal,
            parent_producer,
            parent_declaration,
            child_producer: child_producer.clone(),
            program: crate::body_query::ForeignComptimeProgramKey {
                producer: child_producer,
                configuration,
            },
        }
    }
}

/// Non-clone lifecycle capability issued only after ordered call admission.
/// Its fields remain private so a host cannot reconstruct a ticket from an
/// unordered binding map or use a ticket for another call.
#[allow(dead_code)] // opaque capability consumed by the root-integrated AIR host
#[derive(Debug)]
pub(crate) struct DurableComptimeCallTicket {
    owner: u64,
    serial: u64,
    context: DurableComptimeCallContext,
    expected_parent: Option<(u64, u64)>,
    consumed: bool,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DurableTicketState {
    Entered,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DurableComptimeLifecycleError {
    TicketMismatch,
    NotEntered,
    OutOfOrder,
    TicketReused,
    InvalidContext,
}

#[allow(dead_code)]
static NEXT_DURABLE_LIFECYCLE_ID: AtomicU64 = AtomicU64::new(1);

/// Root-local call/effect authority for a durable comptime host.
///
/// `finish` consumes the ticket and accepts the child outcome opaquely. The
/// outcome is intentionally generic: cleanup happens for every AIR terminal,
/// while effects publish only for a known result, without copying AIR's
/// outcome algebra into the compiler.
#[allow(dead_code)] // AIR owns the active root lifecycle until compiler cutover
#[derive(Debug)]
pub(crate) struct DurableComptimeCallLifecycle {
    owner: u64,
    next_serial: u64,
    parent_producer: crate::StableDefinitionKey,
    parent_declaration: crate::declaration_candidate::DeclarationCandidateKey,
    active: Vec<(u64, u64)>,
    states: BTreeMap<(u64, u64), DurableTicketState>,
    contexts: BTreeMap<(u64, u64), DurableComptimeCallContext>,
    effects: DurableComptimeEffects,
}

#[allow(dead_code)]
impl DurableComptimeCallLifecycle {
    pub(crate) fn new(
        parent_producer: crate::StableDefinitionKey,
        parent_declaration: crate::declaration_candidate::DeclarationCandidateKey,
    ) -> Result<Self, DurableComptimeLifecycleError> {
        let Some(expected_parent) =
            crate::revisioned_query_database::declaration_candidate_for_stable_key(
                &parent_producer,
            )
        else {
            return Err(DurableComptimeLifecycleError::InvalidContext);
        };
        if expected_parent != parent_declaration {
            return Err(DurableComptimeLifecycleError::InvalidContext);
        }
        Ok(Self {
            owner: NEXT_DURABLE_LIFECYCLE_ID.fetch_add(1, Ordering::Relaxed),
            next_serial: 0,
            parent_producer,
            parent_declaration,
            active: Vec::new(),
            states: BTreeMap::new(),
            contexts: BTreeMap::new(),
            effects: DurableComptimeEffects::default(),
        })
    }

    pub(crate) fn prepare(
        &mut self,
        context: DurableComptimeCallContext,
    ) -> Result<DurableComptimeCallTicket, DurableComptimeLifecycleError> {
        let expected_parent = self.active.last().copied();
        let (expected_producer, expected_declaration) = self
            .active
            .last()
            .and_then(|key| self.contexts.get(key))
            .map(|context| {
                (
                    context.child_producer.clone(),
                    context.query.declaration.declaration.clone(),
                )
            })
            .unwrap_or_else(|| {
                (
                    self.parent_producer.clone(),
                    self.parent_declaration.clone(),
                )
            });
        if context.parent_producer != expected_producer
            || context.parent_declaration != expected_declaration
        {
            return Err(DurableComptimeLifecycleError::InvalidContext);
        }
        let serial = self.next_serial;
        self.next_serial = self.next_serial.saturating_add(1);
        Ok(DurableComptimeCallTicket {
            owner: self.owner,
            serial,
            context,
            expected_parent,
            consumed: false,
        })
    }

    pub(crate) fn enter(
        &mut self,
        ticket: &DurableComptimeCallTicket,
    ) -> Result<(), DurableComptimeLifecycleError> {
        let key = (ticket.owner, ticket.serial);
        if ticket.owner != self.owner {
            return Err(DurableComptimeLifecycleError::TicketMismatch);
        }
        if ticket.consumed {
            return Err(DurableComptimeLifecycleError::TicketReused);
        }
        if ticket.serial >= self.next_serial {
            return Err(DurableComptimeLifecycleError::TicketMismatch);
        }
        match self.states.get(&key).copied() {
            None => {
                if self.active.last().copied() != ticket.expected_parent {
                    return Err(DurableComptimeLifecycleError::InvalidContext);
                }
                self.states.insert(key, DurableTicketState::Entered);
                self.contexts.insert(key, ticket.context.clone());
                self.active.push(key);
                Ok(())
            }
            Some(DurableTicketState::Entered) => Err(DurableComptimeLifecycleError::TicketReused),
        }
    }

    pub(crate) fn validate_finish(
        &self,
        ticket: &DurableComptimeCallTicket,
    ) -> Result<(), DurableComptimeLifecycleError> {
        let key = (ticket.owner, ticket.serial);
        if ticket.owner != self.owner {
            return Err(DurableComptimeLifecycleError::TicketMismatch);
        }
        if ticket.consumed {
            return Err(DurableComptimeLifecycleError::TicketReused);
        }
        match self.states.get(&key).copied() {
            Some(DurableTicketState::Entered) => {}
            None => return Err(DurableComptimeLifecycleError::NotEntered),
        }
        if self.active.last().copied() != Some(key) {
            return Err(DurableComptimeLifecycleError::OutOfOrder);
        }
        Ok(())
    }

    pub(crate) fn finish<V, F>(
        &mut self,
        ticket: &mut DurableComptimeCallTicket,
        outcome: &rue_air::ComptimeOutcome<V, F>,
        child: DurableComptimeEffects,
    ) -> Result<(), (DurableComptimeLifecycleError, DurableComptimeEffects)> {
        if let Err(error) = self.validate_finish(ticket) {
            return Err((error, child));
        }
        let key = (ticket.owner, ticket.serial);
        ticket.consumed = true;
        self.active.pop();
        self.states.remove(&key);
        self.contexts.remove(&key);
        let application = Some(DeferredOwnershipApplication {
            declaration: ticket.context.parent_declaration.clone(),
            call_ordinal: ticket.context.call_ordinal,
        });
        if matches!(outcome, rue_air::ComptimeOutcome::Known(_)) {
            self.effects.merge_child(child, application);
        }
        Ok(())
    }

    pub(crate) fn effects(&self) -> &DurableComptimeEffects {
        &self.effects
    }

    pub(crate) fn finish_root(
        self,
    ) -> Result<DurableComptimeEffects, (Self, DurableComptimeLifecycleError)> {
        if self.active.is_empty() {
            Ok(self.effects)
        } else {
            Err((self, DurableComptimeLifecycleError::OutOfOrder))
        }
    }
}

/// The exact semantic site consumed by the durable import authority.
///
/// This intentionally carries the declaration identity, semantic occurrence,
/// and decoded specifier only. It cannot name or evaluate an RIR instruction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DurableImportSite {
    pub(crate) declaration: DeclarationCandidateKey,
    pub(crate) occurrence: u32,
    pub(crate) specifier: Arc<str>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DurableImportResolution {
    Resolved(ModuleId),
    Missing,
    Failure(DeclarationImportFailure),
}

/// Canonical semantic services needed by durable comptime entry points.
///
/// Implementations live beside the query authorities. This facade is an
/// operation boundary, not an evaluator: neither trait accepts an instruction
/// reference, instruction data, or callback capable of evaluating a child.
pub(crate) trait DurableComptimeSemanticAuthority {
    fn check_canceled(&self) -> Result<(), QueryAbort>;

    /// Resolve a declaration through the canonical name/shell authority.
    /// Visibility and shell disagreement diagnostics are produced by the
    /// authority; the facade only carries the semantic request and result.
    fn resolve_candidate(
        &self,
        module: &ModuleId,
        name: &str,
        kind: crate::DefinitionKind,
    ) -> Result<
        Option<DeclarationCandidateKey>,
        rue_air::SemanticProviderError<QueryAbort, SemanticNucleusFailure>,
    >;

    fn resolve_identity(
        &self,
        declaration: DeclarationCandidateKey,
    ) -> Result<
        crate::semantic_query_nucleus::DeclarationIdentityProjection,
        rue_air::SemanticProviderError<QueryAbort, SemanticNucleusFailure>,
    >;

    fn resolve_const(
        &self,
        declaration: DeclarationCandidateKey,
    ) -> Result<
        crate::semantic_query_nucleus::ConstResolutionProjection,
        rue_air::SemanticProviderError<QueryAbort, SemanticNucleusFailure>,
    >;

    fn resolve_import(
        &self,
        site: &DurableImportSite,
    ) -> Result<DurableImportResolution, QueryAbort>;
}

pub(crate) trait DurableComptimeForeignCallAuthority {
    fn probe_comptime_call(
        &self,
        producer: &crate::StableDefinitionKey,
        type_arguments: &[(Arc<str>, DurableType)],
        value_arguments: &[(Arc<str>, DurableConstValue)],
    ) -> Result<ForeignComptimeCallLookup, QueryAbort>;
}

pub(crate) struct DurableComptimeServices<'a, A: ?Sized> {
    authority: &'a A,
}

impl<'a, A: ?Sized> DurableComptimeServices<'a, A> {
    pub(crate) fn new(authority: &'a A) -> Self {
        Self { authority }
    }
}

impl<A: DurableComptimeSemanticAuthority + ?Sized> DurableComptimeServices<'_, A> {
    pub(crate) fn check_canceled(&self) -> Result<(), QueryAbort> {
        self.authority.check_canceled()
    }

    pub(crate) fn resolve_candidate(
        &self,
        module: &ModuleId,
        name: &str,
        kind: crate::DefinitionKind,
    ) -> Result<
        Option<DeclarationCandidateKey>,
        rue_air::SemanticProviderError<QueryAbort, SemanticNucleusFailure>,
    > {
        self.authority.resolve_candidate(module, name, kind)
    }

    pub(crate) fn resolve_identity(
        &self,
        declaration: DeclarationCandidateKey,
    ) -> Result<
        crate::semantic_query_nucleus::DeclarationIdentityProjection,
        rue_air::SemanticProviderError<QueryAbort, SemanticNucleusFailure>,
    > {
        self.authority.resolve_identity(declaration)
    }

    pub(crate) fn resolve_const(
        &self,
        declaration: DeclarationCandidateKey,
    ) -> Result<
        crate::semantic_query_nucleus::ConstResolutionProjection,
        rue_air::SemanticProviderError<QueryAbort, SemanticNucleusFailure>,
    > {
        self.authority.resolve_const(declaration)
    }

    pub(crate) fn resolve_import(
        &self,
        site: &DurableImportSite,
    ) -> Result<DurableImportResolution, QueryAbort> {
        self.authority.resolve_import(site)
    }
}

impl<A: DurableComptimeForeignCallAuthority + ?Sized> DurableComptimeServices<'_, A> {
    /// Probe only an already-published foreign fact or admit its owned body
    /// frame. The authority owns dependency observation and cancellation; this
    /// method never demands a child comptime query.
    pub(crate) fn probe_comptime_call(
        &self,
        producer: &crate::StableDefinitionKey,
        type_arguments: &[(Arc<str>, DurableType)],
        value_arguments: &[(Arc<str>, DurableConstValue)],
    ) -> Result<ForeignComptimeCallLookup, QueryAbort> {
        self.authority
            .probe_comptime_call(producer, type_arguments, value_arguments)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum EvaluatedSemanticConst {
    Value(Arc<TypedSemanticConst>),
    Module(ModuleId),
    TargetEnum(TargetEnumValue),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TargetEnumValue {
    pub(crate) type_name: &'static str,
    pub(crate) variant: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TypedSemanticConst {
    pub(crate) value: DurableConstValue,
    /// `None` is reserved for an unconstrained integer literal. Every named
    /// value, local derived from one, and completed operation carries its
    /// canonical semantic type; consumers must never reconstruct it from the
    /// value's magnitude.
    pub(crate) ty: Option<DurableType>,
}

impl TypedSemanticConst {
    pub(crate) fn typed(value: DurableConstValue, ty: DurableType) -> Arc<Self> {
        Arc::new(Self {
            value,
            ty: Some(ty),
        })
    }

    pub(crate) fn integer_literal(value: i128) -> Arc<Self> {
        Arc::new(Self {
            value: DurableConstValue::Integer(value),
            ty: None,
        })
    }
}

/// AIR's type marker for a durable value.
///
/// Keeping the wrapper local avoids implementing an AIR trait for the generic
/// semantic-import type alias, which would violate Rust's orphan rules. The
/// conversion is lossless and intentionally carries no behavior of its own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DurableComptimeType(pub(crate) DurableType);

impl From<DurableType> for DurableComptimeType {
    fn from(value: DurableType) -> Self {
        Self(value)
    }
}

impl From<DurableComptimeType> for DurableType {
    fn from(value: DurableComptimeType) -> Self {
        value.0
    }
}

impl AsRef<DurableType> for DurableComptimeType {
    fn as_ref(&self) -> &DurableType {
        &self.0
    }
}

impl ComptimeType for DurableComptimeType {}

impl ComptimeValue for EvaluatedSemanticConst {
    type Type = DurableComptimeType;

    fn integer(value: i128) -> Self {
        Self::Value(TypedSemanticConst::integer_literal(value))
    }

    fn boolean(value: bool) -> Self {
        Self::Value(TypedSemanticConst::typed(
            DurableConstValue::Bool(value),
            DurableType::Bool,
        ))
    }

    fn unit() -> Self {
        Self::Value(TypedSemanticConst::typed(
            DurableConstValue::Unit,
            DurableType::Unit,
        ))
    }

    fn type_value(value: Self::Type) -> Self {
        Self::Value(TypedSemanticConst::typed(
            DurableConstValue::Type(value.0),
            DurableType::ComptimeType,
        ))
    }

    fn as_integer(&self) -> Option<i128> {
        let Self::Value(value) = self else {
            return None;
        };
        match value.value {
            DurableConstValue::Integer(value) => Some(value),
            DurableConstValue::Bool(_)
            | DurableConstValue::Type(_)
            | DurableConstValue::Function(_)
            | DurableConstValue::Unit
            | DurableConstValue::String(_) => None,
        }
    }

    fn as_boolean(&self) -> Option<bool> {
        let Self::Value(value) = self else {
            return None;
        };
        match value.value {
            DurableConstValue::Bool(value) => Some(value),
            DurableConstValue::Integer(_)
            | DurableConstValue::Type(_)
            | DurableConstValue::Function(_)
            | DurableConstValue::Unit
            | DurableConstValue::String(_) => None,
        }
    }

    fn as_type(&self) -> Option<Self::Type> {
        let Self::Value(value) = self else {
            return None;
        };
        match &value.value {
            DurableConstValue::Type(value) => Some(DurableComptimeType(value.clone())),
            DurableConstValue::Integer(_)
            | DurableConstValue::Bool(_)
            | DurableConstValue::Function(_)
            | DurableConstValue::Unit
            | DurableConstValue::String(_) => None,
        }
    }

    fn as_integer_type(&self) -> Option<Self::Type> {
        let Self::Value(value) = self else {
            return None;
        };
        if !matches!(value.value, DurableConstValue::Integer(_)) {
            return None;
        }
        value.ty.clone().map(DurableComptimeType)
    }

    fn integer_typed(value: i128, ty: Option<Self::Type>) -> Self {
        match ty {
            Some(ty) => Self::Value(TypedSemanticConst::typed(
                DurableConstValue::Integer(value),
                ty.0,
            )),
            None => Self::integer(value),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn value(value: DurableConstValue, ty: Option<DurableType>) -> EvaluatedSemanticConst {
        EvaluatedSemanticConst::Value(Arc::new(TypedSemanticConst { value, ty }))
    }

    #[test]
    fn scalar_constructors_preserve_the_existing_durable_forms() {
        assert_eq!(
            EvaluatedSemanticConst::integer(7),
            value(DurableConstValue::Integer(7), None)
        );
        assert_eq!(
            EvaluatedSemanticConst::boolean(true),
            value(DurableConstValue::Bool(true), Some(DurableType::Bool))
        );
        assert_eq!(
            EvaluatedSemanticConst::unit(),
            value(DurableConstValue::Unit, Some(DurableType::Unit))
        );
    }

    #[test]
    fn integer_metadata_is_optional_and_lossless() {
        let plain = EvaluatedSemanticConst::integer(9);
        assert_eq!(plain.as_integer(), Some(9));
        assert_eq!(plain.as_integer_type(), None);

        let typed =
            EvaluatedSemanticConst::integer_typed(9, Some(DurableComptimeType(DurableType::I16)));
        assert_eq!(typed.as_integer(), Some(9));
        assert_eq!(
            typed.as_integer_type(),
            Some(DurableComptimeType(DurableType::I16))
        );
        assert_eq!(
            typed,
            value(DurableConstValue::Integer(9), Some(DurableType::I16))
        );
    }

    #[test]
    fn type_values_round_trip_without_reinterpreting_other_variants() {
        let ty = DurableComptimeType(DurableType::Array {
            element: Arc::new(DurableType::U32),
            len: 3,
        });
        let type_value = EvaluatedSemanticConst::type_value(ty.clone());
        assert_eq!(type_value.as_type(), Some(ty.clone()));
        assert_eq!(
            type_value,
            value(
                DurableConstValue::Type(ty.0),
                Some(DurableType::ComptimeType)
            )
        );
        assert_eq!(type_value.as_integer(), None);
        assert_eq!(type_value.as_boolean(), None);
        assert_eq!(type_value.as_integer_type(), None);
    }

    #[test]
    fn module_and_target_enum_values_are_not_scalar_values() {
        let module = EvaluatedSemanticConst::Module(ModuleId::from_logical_path("m").unwrap());
        assert_eq!(module.as_integer(), None);
        assert_eq!(module.as_boolean(), None);
        assert_eq!(module.as_type(), None);
        assert_eq!(module.as_integer_type(), None);

        let target = EvaluatedSemanticConst::TargetEnum(TargetEnumValue {
            type_name: "Os",
            variant: "Macos",
        });
        assert_eq!(target.as_integer(), None);
        assert_eq!(target.as_boolean(), None);
        assert_eq!(target.as_type(), None);
        assert_eq!(target.as_integer_type(), None);
    }

    #[test]
    fn clone_and_conversions_preserve_representation() {
        let ty = DurableType::PtrMut(Arc::new(DurableType::I64));
        let wrapped = DurableComptimeType(ty.clone());
        let unwrapped: DurableType = wrapped.clone().into();
        assert_eq!(unwrapped, ty.clone());
        assert_eq!(DurableComptimeType::from(ty.clone()).as_ref(), &ty);

        let original =
            EvaluatedSemanticConst::integer_typed(-12, Some(DurableComptimeType(ty.clone())));
        assert_eq!(original.clone(), original);
        assert_eq!(original.as_integer_type(), Some(DurableComptimeType(ty)));
    }
}

#[cfg(test)]
mod effect_lifecycle_tests {
    use super::*;

    fn definition(name: &str) -> crate::StableDefinitionKey {
        crate::StableDefinitionKey::from_stable_parts(
            crate::ModuleId::from_logical_path("effects.rue").unwrap(),
            crate::StableDefinitionNamespace::Value,
            crate::StableDefinitionKind::Function,
            Arc::from(name),
            None,
        )
    }

    fn context(ordinal: u32) -> DurableComptimeCallContext {
        let parent_producer = definition("parent");
        context_with_parent(parent_producer, ordinal)
    }

    fn context_with_parent(
        parent_producer: crate::StableDefinitionKey,
        ordinal: u32,
    ) -> DurableComptimeCallContext {
        context_with_parent_and_child(parent_producer, definition("child"), ordinal)
    }

    fn context_with_parent_and_child(
        parent_producer: crate::StableDefinitionKey,
        child_producer: crate::StableDefinitionKey,
        ordinal: u32,
    ) -> DurableComptimeCallContext {
        let parent_declaration =
            crate::revisioned_query_database::declaration_candidate_for_stable_key(
                &parent_producer,
            )
            .unwrap();
        DurableComptimeCallContext::for_test(
            parent_producer,
            parent_declaration,
            child_producer,
            ordinal,
        )
    }

    fn lifecycle() -> DurableComptimeCallLifecycle {
        DurableComptimeCallLifecycle::new(
            definition("parent"),
            crate::revisioned_query_database::declaration_candidate_for_stable_key(&definition(
                "parent",
            ))
            .unwrap(),
        )
        .unwrap()
    }

    fn gate(ordinal: u32) -> DeferredOwnershipGate {
        DeferredOwnershipGate {
            kind: crate::semantic_query_nucleus::DeferredOwnershipGateKind::RequireDroppable,
            ty: DurableType::I32,
            source: Arc::new(crate::semantic_query_nucleus::DeferredOwnershipGateSource {
                declaration:
                    crate::revisioned_query_database::declaration_candidate_for_stable_key(
                        &definition("child"),
                    )
                    .unwrap(),
                start: ordinal,
                end: ordinal + 1,
            }),
            application: None,
        }
    }

    fn child_effects(ordinal: u32) -> DurableComptimeEffects {
        let mut effects = DurableComptimeEffects::default();
        effects.observe_deferred_ownership(gate(ordinal));
        effects
    }

    #[test]
    fn effects_merge_canonicalizes_nominal_collisions_and_observations() {
        assert!(DurableComptimeEffects::default().is_empty());
        let identity = crate::AnonymousNominalKey {
            kind: rue_air::AnonymousNominalKind::Struct,
            producer: crate::StableProducerId::Definition(definition("parent")),
            anchor: rue_rir::RirStructuralAnchor::new(Vec::new()),
        };
        let first = DurableAnonymousNominal::new(
            identity.clone(),
            crate::durable_semantics::DurableAnonymousNominalShape::Struct {
                fields: Arc::from([]),
                methods: Arc::from([]),
            },
            Arc::from([]),
            Arc::from([]),
        );
        let second = first.with_shape(
            crate::durable_semantics::DurableAnonymousNominalShape::Enum {
                variants: Arc::from([]),
            },
        );
        let mut effects = DurableComptimeEffects::default();
        effects.observe_anonymous_nominal(first);
        effects.observe_anonymous_nominal(second.clone());
        effects.observe_dependency(SemanticDeclarationDependency {
            source: definition("parent"),
            kind: rue_air::DeclarationTypeDependencyKind::Body,
            target: crate::semantic_query_nucleus::SemanticDeclarationDependencyTarget::NamedValue(
                definition("child"),
            ),
        });
        effects.observe_dependency(SemanticDeclarationDependency {
            source: definition("parent"),
            kind: rue_air::DeclarationTypeDependencyKind::Body,
            target: crate::semantic_query_nucleus::SemanticDeclarationDependencyTarget::NamedValue(
                definition("child"),
            ),
        });
        effects.observe_deferred_ownership(gate(1));
        assert_eq!(effects.anonymous_nominals().count(), 1);
        assert_eq!(effects.dependencies().count(), 1);
        assert_eq!(effects.anonymous_nominals().next(), Some(&second));
        assert_eq!(effects.deferred_ownership().count(), 1);
    }

    #[test]
    fn admitted_context_derives_the_exact_program_and_ordered_arguments() {
        let snapshot = crate::SourceSnapshot::single(
            "<durable-context>",
            "fn target() -> i32 { 1 } fn sibling() -> i32 { 2 }",
        )
        .unwrap();
        let module = crate::parsed_modules::parse_source_snapshot_modules(&snapshot)
            .unwrap()
            .modules()[0]
            .clone();
        let candidate = module
            .definitions()
            .declaration_keys_in_source_order()
            .find(|candidate| candidate.name.as_ref() == "target")
            .unwrap()
            .clone();
        let sibling = module
            .definitions()
            .declaration_keys_in_source_order()
            .find(|candidate| candidate.name.as_ref() == "sibling")
            .unwrap()
            .clone();
        let artifacts =
            crate::canonical_lower::lower_parsed_declaration_body_plan(&module, &candidate, || {
                Ok(())
            })
            .unwrap();
        let configuration = crate::semantic_query_nucleus::SemanticQueryConfiguration {
            target: rue_target::Target::X86_64Linux,
            preview_features: crate::StablePreviewFeatures::new(&crate::PreviewFeatures::default()),
        };
        let producer = crate::StableDefinitionKey::from_stable_parts(
            candidate.module.clone(),
            crate::StableDefinitionNamespace::Value,
            crate::StableDefinitionKind::Function,
            candidate.name.clone(),
            None,
        );
        let seed = crate::body_query::ForeignComptimeCallSeed {
            type_arguments: Arc::from([
                (Arc::from("z"), DurableType::I32),
                (Arc::from("a"), DurableType::I64),
            ]),
            value_arguments: Arc::from([
                (
                    Arc::from("z"),
                    crate::durable_semantics::DurableConstValue::Integer(9),
                ),
                (
                    Arc::from("a"),
                    crate::durable_semantics::DurableConstValue::Integer(1),
                ),
            ]),
        };
        let admitted = crate::body_query::OwnedForeignComptimeProgram::from_body_plan(
            crate::body_query::ForeignComptimeProgramPlan {
                key: crate::body_query::ForeignComptimeProgramKey {
                    producer: producer.clone(),
                    configuration: configuration.clone(),
                },
                candidate: candidate.clone(),
            },
            &artifacts,
            seed.clone(),
            || Ok(()),
        )
        .unwrap();
        let parent = definition("parent");
        let parent_declaration =
            crate::revisioned_query_database::declaration_candidate_for_stable_key(&parent)
                .unwrap();
        let context =
            DurableComptimeCallContext::from_admitted(&admitted, parent, parent_declaration, 42)
                .unwrap();
        assert_eq!(context.program, admitted.plan.key);
        assert_eq!(context.child_producer, producer);
        assert_eq!(
            context.query.declaration.declaration,
            admitted.plan.candidate
        );
        assert_eq!(
            context.query.declaration.configuration,
            admitted.plan.key.configuration
        );
        assert_eq!(context.query.type_arguments, seed.type_arguments);
        assert_eq!(context.query.value_arguments, seed.value_arguments);
        assert_eq!(context.call_ordinal, 42);

        let mut inconsistent = admitted.clone();
        inconsistent.plan.candidate = sibling;
        assert!(matches!(
            DurableComptimeCallContext::from_admitted(
                &inconsistent,
                definition("parent"),
                crate::revisioned_query_database::declaration_candidate_for_stable_key(
                    &definition("parent")
                )
                .unwrap(),
                42,
            ),
            Err(DurableComptimeLifecycleError::InvalidContext)
        ));
    }

    #[test]
    fn entered_calls_merge_once_in_lifo_order_and_fill_deferred_application() {
        let mut lifecycle = lifecycle();
        let outer_context = context(3);
        let inner_context = context_with_parent(definition("child"), 4);
        let mut outer = lifecycle.prepare(outer_context).unwrap();
        lifecycle.enter(&outer).unwrap();
        let mut inner = lifecycle.prepare(inner_context).unwrap();
        lifecycle.enter(&inner).unwrap();
        let inner_outcome = rue_air::ComptimeOutcome::<(), ()>::Known(());
        assert_eq!(
            lifecycle.finish(&mut inner, &inner_outcome, child_effects(4),),
            Ok(())
        );
        let outer_outcome = rue_air::ComptimeOutcome::<(), ()>::Known(());
        assert_eq!(
            lifecycle.finish(&mut outer, &outer_outcome, child_effects(3),),
            Ok(())
        );
        let effects = lifecycle.finish_root().unwrap();
        let applications = effects
            .deferred_ownership()
            .map(|gate| {
                let application = gate.application.as_ref().unwrap();
                (application.declaration.clone(), application.call_ordinal)
            })
            .collect::<Vec<_>>();
        assert_eq!(
            applications,
            vec![
                (
                    crate::revisioned_query_database::declaration_candidate_for_stable_key(
                        &definition("parent"),
                    )
                    .unwrap(),
                    3,
                ),
                (
                    crate::revisioned_query_database::declaration_candidate_for_stable_key(
                        &definition("child"),
                    )
                    .unwrap(),
                    4,
                ),
            ]
        );
    }

    #[test]
    fn mismatched_order_rejection_does_not_publish_child_effects() {
        let mut lifecycle = lifecycle();
        let outer_context = context(1);
        let inner_context = context_with_parent(definition("child"), 2);
        let mut outer = lifecycle.prepare(outer_context).unwrap();
        lifecycle.enter(&outer).unwrap();
        let mut inner = lifecycle.prepare(inner_context).unwrap();
        lifecycle.enter(&inner).unwrap();
        let outer_outcome = rue_air::ComptimeOutcome::<(), ()>::Known(());
        let inner_outcome = rue_air::ComptimeOutcome::<(), ()>::Known(());
        let Err((error, returned_child)) =
            lifecycle.finish(&mut outer, &outer_outcome, child_effects(1))
        else {
            panic!("out-of-order finish should return its inputs");
        };
        assert_eq!(error, DurableComptimeLifecycleError::OutOfOrder);
        assert_eq!(lifecycle.effects().deferred_ownership().count(), 0);
        lifecycle
            .finish(&mut inner, &inner_outcome, child_effects(2))
            .unwrap();
        lifecycle
            .finish(&mut outer, &outer_outcome, returned_child)
            .unwrap();
    }

    #[test]
    fn prepared_ticket_can_be_dropped_and_non_known_outcomes_do_not_publish() {
        let mut lifecycle = lifecycle();
        let prepared_context = context(0);
        let prepared = lifecycle.prepare(prepared_context).unwrap();
        drop(prepared);
        let prepared_context = context(0);
        let mut prepared = lifecycle.prepare(prepared_context).unwrap();
        let prepared_outcome = rue_air::ComptimeOutcome::<(), ()>::Known(());
        let Err((error, _)) = lifecycle.finish(&mut prepared, &prepared_outcome, child_effects(0))
        else {
            panic!("prepared finish should be rejected");
        };
        assert_eq!(error, DurableComptimeLifecycleError::NotEntered);

        let ticket_context = context(1);
        let mut ticket = lifecycle.prepare(ticket_context).unwrap();
        lifecycle.enter(&ticket).unwrap();
        assert_eq!(
            lifecycle.enter(&ticket),
            Err(DurableComptimeLifecycleError::TicketReused)
        );
        let abort_outcome = rue_air::ComptimeOutcome::<(), ()>::Abort(());
        lifecycle
            .finish(&mut ticket, &abort_outcome, child_effects(1))
            .unwrap();
        assert_eq!(lifecycle.effects().deferred_ownership().count(), 0);
    }

    #[test]
    fn rejected_finish_and_cross_owner_attempts_preserve_recovery() {
        let mut lifecycle = lifecycle();
        let mut other = DurableComptimeCallLifecycle::new(
            definition("other"),
            crate::revisioned_query_database::declaration_candidate_for_stable_key(&definition(
                "other",
            ))
            .unwrap(),
        )
        .unwrap();
        let mut ticket = lifecycle.prepare(context(8)).unwrap();
        lifecycle.enter(&ticket).unwrap();
        let outcome = rue_air::ComptimeOutcome::<(), ()>::Known(());
        let Err((error, returned_child)) = other.finish(&mut ticket, &outcome, child_effects(8))
        else {
            panic!("cross-owner finish should be rejected");
        };
        assert_eq!(error, DurableComptimeLifecycleError::TicketMismatch);
        let Err((returned_lifecycle, error)) = lifecycle.finish_root() else {
            panic!("active lifecycle must not finish as a root");
        };
        lifecycle = returned_lifecycle;
        assert_eq!(error, DurableComptimeLifecycleError::OutOfOrder);
        lifecycle
            .finish(&mut ticket, &outcome, returned_child)
            .unwrap();
        assert_eq!(
            lifecycle
                .finish_root()
                .unwrap()
                .deferred_ownership()
                .count(),
            1
        );
    }

    #[test]
    fn prepared_parent_slot_prevents_reordered_entry() {
        let mut lifecycle = lifecycle();
        let mut first = lifecycle
            .prepare(context_with_parent_and_child(
                definition("parent"),
                definition("child_a"),
                30,
            ))
            .unwrap();
        let mut second = lifecycle
            .prepare(context_with_parent_and_child(
                definition("parent"),
                definition("child_b"),
                31,
            ))
            .unwrap();
        lifecycle.enter(&second).unwrap();
        assert_eq!(
            lifecycle.enter(&first),
            Err(DurableComptimeLifecycleError::InvalidContext)
        );
        lifecycle
            .finish(
                &mut second,
                &rue_air::ComptimeOutcome::<(), ()>::Known(()),
                DurableComptimeEffects::default(),
            )
            .unwrap();
        lifecycle.enter(&first).unwrap();
        lifecycle
            .finish(
                &mut first,
                &rue_air::ComptimeOutcome::<(), ()>::Known(()),
                DurableComptimeEffects::default(),
            )
            .unwrap();
        assert!(lifecycle.finish_root().unwrap().is_empty());
    }

    #[test]
    fn all_non_known_terminals_cleanup_without_publishing_effects() {
        fn assert_not_published(outcome: rue_air::ComptimeOutcome<(), ()>) {
            let mut lifecycle = lifecycle();
            let mut ticket = lifecycle.prepare(context(20)).unwrap();
            lifecycle.enter(&ticket).unwrap();
            lifecycle
                .finish(&mut ticket, &outcome, child_effects(20))
                .unwrap();
            assert!(lifecycle.finish_root().unwrap().is_empty());
        }

        assert_not_published(rue_air::ComptimeOutcome::RuntimeDependent);
        assert_not_published(rue_air::ComptimeOutcome::NotReady);
        assert_not_published(rue_air::ComptimeOutcome::UnsupportedContext);
        assert_not_published(rue_air::ComptimeOutcome::Trap(rue_air::ComptimeTrap {
            operation: "test",
            span: rue_span::Span::new(0, 0),
        }));
        assert_not_published(rue_air::ComptimeOutcome::HostFailure(()));
        assert_not_published(rue_air::ComptimeOutcome::Abort(()));
    }

    #[test]
    fn preexisting_deferred_application_is_not_rewritten() {
        let mut lifecycle = lifecycle();
        let mut ticket = lifecycle.prepare(context(21)).unwrap();
        lifecycle.enter(&ticket).unwrap();
        let mut effects = DurableComptimeEffects::default();
        let mut gate = gate(21);
        gate.application = Some(
            crate::semantic_query_nucleus::DeferredOwnershipApplication {
                declaration:
                    crate::revisioned_query_database::declaration_candidate_for_stable_key(
                        &definition("child"),
                    )
                    .unwrap(),
                call_ordinal: 99,
            },
        );
        effects.observe_deferred_ownership(gate);
        lifecycle
            .finish(
                &mut ticket,
                &rue_air::ComptimeOutcome::<(), ()>::Known(()),
                effects,
            )
            .unwrap();
        let effects = lifecycle.finish_root().unwrap();
        assert_eq!(
            effects
                .deferred_ownership()
                .next()
                .unwrap()
                .application
                .as_ref()
                .unwrap()
                .call_ordinal,
            99
        );
    }

    #[test]
    fn invalid_root_identity_is_rejected_before_ticket_issuance() {
        let wrong = crate::revisioned_query_database::declaration_candidate_for_stable_key(
            &definition("child"),
        )
        .unwrap();
        assert!(matches!(
            DurableComptimeCallLifecycle::new(definition("parent"), wrong),
            Err(DurableComptimeLifecycleError::InvalidContext)
        ));
    }

    #[test]
    fn active_root_cannot_publish_effects_until_children_are_finished() {
        let mut lifecycle = lifecycle();
        let context = context(0);
        let mut ticket = lifecycle.prepare(context).unwrap();
        lifecycle.enter(&ticket).unwrap();
        let Err((returned_lifecycle, error)) = lifecycle.finish_root() else {
            panic!("active lifecycle must not finish as a root");
        };
        lifecycle = returned_lifecycle;
        assert_eq!(error, DurableComptimeLifecycleError::OutOfOrder);
        lifecycle
            .finish(
                &mut ticket,
                &rue_air::ComptimeOutcome::<(), ()>::Known(()),
                DurableComptimeEffects::default(),
            )
            .unwrap();
        assert!(lifecycle.finish_root().unwrap().is_empty());
    }
}
