//! Root-local application policy and deterministic effect publication.

use super::*;

/// The ownership-site policy is issued with an admitted call rather than
/// inferred while finishing it. Expression calls may attribute still-deferred
/// gates to their parent call; structured type calls deliberately preserve
/// missing applications for an enclosing expression call to fill.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DurableComptimeApplicationPolicy {
    Preserve,
    ApplyAtParentCall {
        application: DeferredOwnershipApplication,
    },
}

impl DurableComptimeApplicationPolicy {
    pub(crate) fn preserve() -> Self {
        Self::Preserve
    }

    pub(crate) fn apply_at_parent_call(
        declaration: crate::declaration_candidate::DeclarationCandidateKey,
        call_ordinal: u32,
    ) -> Self {
        Self::ApplyAtParentCall {
            application: DeferredOwnershipApplication {
                declaration,
                call_ordinal,
            },
        }
    }

    fn application(&self) -> Option<DeferredOwnershipApplication> {
        match self {
            Self::Preserve => None,
            Self::ApplyAtParentCall { application } => Some(application.clone()),
        }
    }
}

/// Effects observed while reducing one durable comptime root.
///
/// The collections deliberately use the same canonical keys and ordering as
/// the semantic nucleus. Anonymous nominals replace an earlier observation at
/// the same identity, while dependencies and ownership gates are set-unioned;
/// this is the existing publication behavior, expressed as one operation
/// boundary for the AIR host.
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

    pub(crate) fn merge_child(
        &mut self,
        child: DurableComptimeEffects,
        policy: &DurableComptimeApplicationPolicy,
    ) {
        merge_effects_into(
            &mut self.anonymous_nominals,
            &mut self.dependencies,
            &mut self.deferred_ownership,
            child.anonymous_nominals.into_values(),
            child.dependencies,
            child.deferred_ownership,
            policy.application(),
        );
    }

    pub(crate) fn merge_projection(
        &mut self,
        anonymous_nominals: &[DurableAnonymousNominal],
        dependencies: &[SemanticDeclarationDependency],
        deferred_ownership: &[DeferredOwnershipGate],
        policy: &DurableComptimeApplicationPolicy,
    ) {
        merge_effects_into(
            &mut self.anonymous_nominals,
            &mut self.dependencies,
            &mut self.deferred_ownership,
            anonymous_nominals.iter().cloned(),
            dependencies.iter().cloned(),
            deferred_ownership.iter().cloned(),
            policy.application(),
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
        policy: &DurableComptimeApplicationPolicy,
    ) {
        merge_effects_into(
            anonymous_nominals,
            dependencies,
            deferred_ownership,
            self.anonymous_nominals.into_values(),
            self.dependencies,
            self.deferred_ownership,
            policy.application(),
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
