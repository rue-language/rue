//! Exact, fail-closed corpus model-gap registries.
//!
//! A typed oracle model gap is accepted only when the corpus case identity,
//! exact [`ModelGapKind`], and platform scope all match an explicit registry
//! entry. The audit is generic over case identity so the same policy can be
//! reused by other corpora without weakening it to strings or paths.

pub(crate) mod cli;

use rue_oracle::ModelGapKind;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

/// Whether this invocation intends to enumerate the corpus's complete
/// authoritative inventory or only a caller-selected subset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InventoryScope {
    Authoritative,
    Partial,
}

/// One accepted item of oracle coverage debt.
///
/// The gap is deliberately a [`ModelGapKind`], not an `UnsupportedKind` or a
/// display string. Resource limits, compiler/oracle contract violations, and
/// harness observation gaps therefore cannot be registered by construction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ModelGapRegistration<I> {
    identity: I,
    kind: ModelGapKind,
    only_on: Vec<String>,
}

impl<I> ModelGapRegistration<I> {
    pub(crate) fn new(
        identity: I,
        kind: ModelGapKind,
        only_on: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        Self {
            identity,
            kind,
            only_on: canonical_scope(only_on),
        }
    }
}

/// The part of a corpus outcome relevant to model-gap registry policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Observation {
    ModelGap(ModelGapKind),
    Agreement,
    /// The case is inactive solely because its `only_on` scope excludes this
    /// host. It remains valid registered debt when that exact scope matches.
    InactiveHost,
    /// Some other eligibility rule now prevents the case from reaching the
    /// oracle. A registered gap must not silently turn into such a skip.
    OtherIneligible,
    /// An unmodeled harness observation such as an unparsed trap expectation
    /// or another unsupported assertion. These are not oracle implementation debt.
    Unregistrable(&'static str),
    /// A front-end failure, hard oracle failure, or disagreement already makes
    /// the enclosing corpus report fail. Avoid adding a misleading stale-gap
    /// diagnosis on top of the primary failure.
    HardFailure,
}

type RenderRegistration<I> = fn(&I, ModelGapKind, &[String]) -> String;

/// Stateful exact-registry audit for one corpus invocation.
pub(crate) struct ModelGapAudit<I> {
    corpus: &'static str,
    scope: InventoryScope,
    registry: BTreeMap<I, ModelGapRegistration<I>>,
    discovered: BTreeSet<I>,
    failures: Vec<String>,
    render_registration: RenderRegistration<I>,
}

impl<I> ModelGapAudit<I>
where
    I: Clone + Ord + fmt::Display,
{
    pub(crate) fn new(
        corpus: &'static str,
        scope: InventoryScope,
        registrations: impl IntoIterator<Item = ModelGapRegistration<I>>,
        render_registration: RenderRegistration<I>,
    ) -> Self {
        let mut registry = BTreeMap::new();
        let mut failures = Vec::new();
        for registration in registrations {
            let identity = registration.identity.clone();
            if let Some(previous) = registry.insert(identity.clone(), registration) {
                failures.push(format!(
                    "{corpus} model-gap registry has duplicate identity {identity}; first entry \
                     was {:?} on {:?}",
                    previous.kind, previous.only_on
                ));
            }
        }

        Self {
            corpus,
            scope,
            registry,
            discovered: BTreeSet::new(),
            failures,
            render_registration,
        }
    }

    /// Record one discovered, expanded corpus case and its terminal outcome.
    pub(crate) fn observe(&mut self, identity: I, only_on: &[String], observation: Observation) {
        if !self.discovered.insert(identity.clone()) {
            self.failures.push(format!(
                "{} corpus discovered duplicate case identity {identity}; identities are \
                 (section.id, expanded case.name), never file paths",
                self.corpus
            ));
            return;
        }

        let actual_scope = canonical_scope(only_on.iter().cloned());
        let registration = self.registry.get(&identity);

        match (registration, observation) {
            (None, Observation::ModelGap(kind)) => self.failures.push(format!(
                "unknown {} oracle model gap for {identity}: {kind:?}\n      add:\n{}",
                self.corpus,
                (self.render_registration)(&identity, kind, &actual_scope)
            )),
            (Some(registration), Observation::ModelGap(actual)) => {
                let kind_drift = registration.kind != actual;
                let scope_drift = registration.only_on != actual_scope;
                if kind_drift || scope_drift {
                    let drift = match (kind_drift, scope_drift) {
                        (true, true) => "kind and scope drift",
                        (true, false) => "kind drift",
                        (false, true) => "scope drift",
                        (false, false) => unreachable!(),
                    };
                    self.failures.push(format!(
                        "{} model-gap {drift} for {identity}: registered {:?} on {:?}, observed \
                         {actual:?} on {:?}\n      replace it with:\n{}",
                        self.corpus,
                        registration.kind,
                        registration.only_on,
                        actual_scope,
                        (self.render_registration)(&identity, actual, &actual_scope)
                    ));
                }
            }
            (Some(_), Observation::Agreement) => self.failures.push(format!(
                "stale {} model-gap registry entry for {identity}: the oracle now agrees; \
                 delete the entry",
                self.corpus
            )),
            (Some(_), Observation::OtherIneligible) => self.failures.push(format!(
                "{} model-gap eligibility drift for {identity}: the registered case is now \
                 ineligible for a reason other than its declared only_on host scope",
                self.corpus
            )),
            (_, Observation::Unregistrable(reason)) => self.failures.push(format!(
                "unregistrable {} unmodeled observation for {identity}: {reason}; this is \
                 harness coverage, not typed oracle model debt",
                self.corpus
            )),
            (Some(registration), Observation::InactiveHost | Observation::HardFailure)
                if registration.only_on != actual_scope =>
            {
                self.failures.push(format!(
                    "{} model-gap registry scope drift for {identity}: registered only_on {:?}, \
                     corpus declares {:?}\n      replace it with:\n{}",
                    self.corpus,
                    registration.only_on,
                    actual_scope,
                    (self.render_registration)(&identity, registration.kind, &actual_scope)
                ));
            }
            (
                None,
                Observation::Agreement
                | Observation::InactiveHost
                | Observation::OtherIneligible
                | Observation::HardFailure,
            )
            | (Some(_), Observation::InactiveHost | Observation::HardFailure) => {}
        }
    }

    /// Complete the audit.
    ///
    /// Orphans are meaningful only for a full authoritative run whose input
    /// discovery and parsing completed. Partial runs still enforce every seen
    /// identity but never treat unseen registry entries as stale or missing.
    pub(crate) fn finish(mut self, inventory_complete: bool) -> Vec<String> {
        if self.scope == InventoryScope::Authoritative && inventory_complete {
            for identity in self.registry.keys() {
                if !self.discovered.contains(identity) {
                    self.failures.push(format!(
                        "orphaned {} model-gap registry entry for {identity}: no case with this \
                         (section.id, expanded case.name) identity was discovered; delete or \
                         correct the entry",
                        self.corpus
                    ));
                }
            }
        }
        self.failures.sort();
        self.failures
    }
}

fn canonical_scope(values: impl IntoIterator<Item = impl Into<String>>) -> Vec<String> {
    let mut values: Vec<String> = values.into_iter().map(Into::into).collect();
    values.sort();
    values.dedup();
    values
}

#[cfg(test)]
mod tests {
    use super::*;
    use rue_oracle::{ExternalDependencyKind, ModelGapKind};

    const STDIN: ModelGapKind =
        ModelGapKind::ExternalDependency(ExternalDependencyKind::StandardInput);
    const RANDOM: ModelGapKind =
        ModelGapKind::ExternalDependency(ExternalDependencyKind::RandomU32);

    #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
    struct Id(&'static str);

    impl fmt::Display for Id {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str(self.0)
        }
    }

    fn registration(
        id: &'static str,
        kind: ModelGapKind,
        only_on: &[&str],
    ) -> ModelGapRegistration<Id> {
        ModelGapRegistration::new(Id(id), kind, only_on.iter().copied())
    }

    fn render(id: &Id, kind: ModelGapKind, only_on: &[String]) -> String {
        format!("entry({:?}, {kind:?}, {only_on:?}),", id.0)
    }

    fn audit(
        scope: InventoryScope,
        entries: impl IntoIterator<Item = ModelGapRegistration<Id>>,
    ) -> ModelGapAudit<Id> {
        ModelGapAudit::new("test", scope, entries, render)
    }

    #[test]
    fn exact_registered_gap_is_accepted() {
        let mut audit = audit(
            InventoryScope::Authoritative,
            [registration("case", STDIN, &[])],
        );
        audit.observe(Id("case"), &[], Observation::ModelGap(STDIN));
        assert!(audit.finish(true).is_empty());
    }

    #[test]
    fn unknown_gap_is_fail_closed_with_paste_ready_entry() {
        let mut audit = audit(InventoryScope::Partial, []);
        audit.observe(
            Id("new"),
            &["aarch64-macos".to_string()],
            Observation::ModelGap(STDIN),
        );
        let failures = audit.finish(true);
        assert_eq!(failures.len(), 1);
        assert!(failures[0].contains("unknown test oracle model gap"));
        assert!(failures[0].contains("entry(\"new\", ExternalDependency(StandardInput)"));
        assert!(failures[0].contains("aarch64-macos"));
    }

    #[test]
    fn exact_kind_drift_is_rejected() {
        let mut audit = audit(InventoryScope::Partial, [registration("case", STDIN, &[])]);
        audit.observe(Id("case"), &[], Observation::ModelGap(RANDOM));
        let failures = audit.finish(true);
        assert_eq!(failures.len(), 1);
        assert!(failures[0].contains("kind drift"));
        assert!(failures[0].contains("ExternalDependency(RandomU32)"));
    }

    #[test]
    fn seen_agreement_makes_a_registration_stale() {
        let mut audit = audit(
            InventoryScope::Partial,
            [registration("case", STDIN, &["x86-64-linux"])],
        );
        audit.observe(Id("case"), &[], Observation::Agreement);
        let failures = audit.finish(true);
        assert_eq!(failures.len(), 1);
        assert!(failures[0].contains("stale"));
        assert!(!failures[0].contains("replace it with"));
        assert!(!failures[0].contains("scope drift"));
    }

    #[test]
    fn other_ineligibility_cannot_hide_registered_debt() {
        let mut audit = audit(
            InventoryScope::Partial,
            [registration("case", STDIN, &["x86-64-linux"])],
        );
        audit.observe(Id("case"), &[], Observation::OtherIneligible);
        let failures = audit.finish(true);
        assert_eq!(failures.len(), 1);
        assert!(failures[0].contains("eligibility drift"));
        assert!(!failures[0].contains("scope drift"));
    }

    #[test]
    fn inactive_host_is_not_stale_but_its_scope_is_exact() {
        let mut matching = audit(
            InventoryScope::Authoritative,
            [registration("case", STDIN, &["x86-64-linux"])],
        );
        matching.observe(
            Id("case"),
            &["x86-64-linux".to_string()],
            Observation::InactiveHost,
        );
        assert!(matching.finish(true).is_empty());

        let mut drifted = audit(
            InventoryScope::Partial,
            [registration("case", STDIN, &["aarch64-macos"])],
        );
        drifted.observe(
            Id("case"),
            &["x86-64-linux".to_string()],
            Observation::InactiveHost,
        );
        let failures = drifted.finish(true);
        assert_eq!(failures.len(), 1);
        assert!(failures[0].contains("scope drift"));
    }

    #[test]
    fn simultaneous_kind_and_scope_drift_has_one_current_replacement() {
        let mut audit = audit(
            InventoryScope::Partial,
            [registration("case", STDIN, &["x86-64-linux"])],
        );
        audit.observe(
            Id("case"),
            &["aarch64-macos".to_string()],
            Observation::ModelGap(RANDOM),
        );
        let failures = audit.finish(true);
        assert_eq!(failures.len(), 1);
        assert!(failures[0].contains("kind and scope drift"));
        assert!(failures[0].contains(concat!(
            "entry(\"case\", ExternalDependency(RandomU32), ",
            "[\"aarch64-macos\"]),"
        )));
    }

    #[test]
    fn only_on_is_compared_as_an_exact_platform_set() {
        let mut reordered = audit(
            InventoryScope::Partial,
            [registration(
                "case",
                STDIN,
                &["x86-64-linux", "aarch64-macos"],
            )],
        );
        reordered.observe(
            Id("case"),
            &["aarch64-macos".to_string(), "x86-64-linux".to_string()],
            Observation::ModelGap(STDIN),
        );
        assert!(reordered.finish(true).is_empty());

        let mut drifted = audit(
            InventoryScope::Partial,
            [registration("case", STDIN, &["x86-64-linux"])],
        );
        drifted.observe(Id("case"), &[], Observation::ModelGap(STDIN));
        assert_eq!(drifted.finish(true).len(), 1);
    }

    #[test]
    fn duplicate_registry_and_discovered_identities_are_rejected() {
        let duplicate_entries = audit(
            InventoryScope::Partial,
            [
                registration("case", STDIN, &[]),
                registration("case", RANDOM, &[]),
            ],
        )
        .finish(true);
        assert_eq!(duplicate_entries.len(), 1);
        assert!(duplicate_entries[0].contains("duplicate identity"));

        let mut duplicate_cases = audit(InventoryScope::Partial, []);
        duplicate_cases.observe(Id("case"), &[], Observation::Agreement);
        duplicate_cases.observe(Id("case"), &[], Observation::Agreement);
        let failures = duplicate_cases.finish(true);
        assert_eq!(failures.len(), 1);
        assert!(failures[0].contains("discovered duplicate case identity"));
    }

    #[test]
    fn only_complete_authoritative_inventory_checks_orphans() {
        let partial = audit(
            InventoryScope::Partial,
            [registration("unseen", STDIN, &[])],
        )
        .finish(true);
        assert!(partial.is_empty());

        let incomplete = audit(
            InventoryScope::Authoritative,
            [registration("unseen", STDIN, &[])],
        )
        .finish(false);
        assert!(incomplete.is_empty());

        let complete = audit(
            InventoryScope::Authoritative,
            [registration("unseen", STDIN, &[])],
        )
        .finish(true);
        assert_eq!(complete.len(), 1);
        assert!(complete[0].contains("orphaned"));
    }

    #[test]
    fn unregistrable_observations_are_always_hard_drift() {
        for reason in ["runtime trap expectation", "other harness expectation"] {
            let mut audit = audit(InventoryScope::Partial, []);
            audit.observe(Id("case"), &[], Observation::Unregistrable(reason));
            let failures = audit.finish(true);
            assert_eq!(failures.len(), 1);
            assert!(failures[0].contains("unregistrable"));
            assert!(failures[0].contains(reason));
        }
    }
}
