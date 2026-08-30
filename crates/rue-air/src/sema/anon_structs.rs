//! Anonymous struct handling.
//!
//! Anonymous `struct` and `enum` declaration expressions are *producer-nominal*
//! (ADR-0066). Their identity is the selected declaration expression under its
//! static enclosing comptime specialization — the `AnonymousNominalKey` — not a
//! structural comparison of fields, variants, method signatures, or bodies. Two
//! distinct producers that declare the same shape are distinct, non-assignable
//! types. There is no cross-producer structural search or stable-minimum
//! representative: each full producer key owns exactly one entity. Different
//! anchors under the same producer remain distinct; only semantically empty
//! specialization wrappers on the producer spine are canonical aliases.
//!
//! It also implements the enum analog (`find_or_create_anon_enum`): anonymous
//! enum types produced by comptime type functions like
//! `fn Option(comptime T: type) -> type { enum { Some(T), None } }`
//! (ADR-0038, RUE-6 phase 2).

use std::cmp::Ordering;

/// The trusted standard-library producer family a `?` operand or enclosing
/// return type is an exact specialization of (RUE-1112).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TrustedTryProducer {
    Option,
    Result,
}

pub(crate) type IssuedAnonymousNominalKey =
    crate::AnonymousNominalKey<crate::SemanticDefinitionToken, crate::SemanticModuleToken>;

pub(crate) type IssuedFunctionInstanceKey =
    crate::FunctionInstanceKey<crate::SemanticDefinitionToken, crate::SemanticModuleToken>;

pub(crate) type IssuedTypeInstanceKey =
    crate::TypeInstanceKey<crate::SemanticDefinitionToken, crate::SemanticModuleToken>;

pub(crate) type IssuedCanonicalArguments =
    crate::CanonicalArguments<crate::SemanticDefinitionToken, crate::SemanticModuleToken>;

pub(crate) type IssuedStableProducerId =
    crate::StableProducerId<crate::SemanticDefinitionToken, crate::SemanticModuleToken>;

/// The one spelling of a generated anonymous struct's name.
///
/// The name is a total function of the producer digest and nothing else, which
/// is what lets a whole revision share one rendering of it, and what makes the
/// pool's anonymity registry rather than the name the authority for whether a
/// struct is generated (RUE-1050, RUE-1193). Both mints — the producer-nominal
/// pool and the import epoch it byte-mirrors — spell it here, so the two cannot
/// drift apart (RUE-1236).
pub(crate) fn anonymous_struct_name(digest: u128) -> String {
    format!("__anon_struct_{digest:032x}")
}

/// The one spelling of a generated anonymous enum's name.
///
/// The digest is the complete source symbol. Variant and payload vocabulary
/// remains in the enum definition itself rather than in its nominal spelling,
/// so the epoch and durable producer mints can share this exact helper.
pub(crate) fn anonymous_enum_name(digest: u128) -> String {
    format!("__anon_enum_{digest:032x}")
}

/// The root source definition a producer-nominal key ultimately derives from,
/// unwinding function specializations, anonymous members, and drop glue down to
/// the owning definition token. `None` for a producer with no source definition
/// root (a builtin or primitive owner).
///
/// This is the single place the AIR domain walks a `StableProducerId` to its
/// definition. It backs both the deterministic export ordering
/// ([`anonymous_key_cmp`]) and trusted-producer recognition under `?`
/// (RUE-1112): a `Type` whose anonymous key roots at the trusted
/// `std/option.rue::Option` (or `std/result.rue::Result`) function definition is
/// an exact std producer, everything else a lookalike.
pub(crate) fn anonymous_producer_root(
    producer: &IssuedStableProducerId,
) -> Option<crate::SemanticDefinitionToken> {
    fn type_root(value: &IssuedTypeInstanceKey) -> Option<crate::SemanticDefinitionToken> {
        use crate::{NominalInstanceKey as N, TypeInstanceKey as T};
        match value {
            T::Nominal(N::Named(value)) => Some(*value),
            T::Nominal(N::Anonymous(value)) => anonymous_producer_root(&value.producer),
            T::Array { element, .. } | T::PtrConst(element) | T::PtrMut(element) => {
                type_root(element)
            }
            _ => None,
        }
    }
    fn function_root(value: &IssuedFunctionInstanceKey) -> Option<crate::SemanticDefinitionToken> {
        use crate::FunctionInstanceKey as F;
        match value {
            F::Definition(value) => Some(*value),
            F::Specialization { base, .. } => function_root(base),
            F::AnonymousMember { owner, .. } | F::DropGlue(owner) => type_root(owner),
        }
    }
    match producer {
        crate::StableProducerId::Definition(value) => Some(*value),
        crate::StableProducerId::Function(value) => function_root(value),
    }
}

/// Deterministic export/presentation ordering of two producer-nominal keys.
///
/// This is a *presentation* order only — it decides how anonymous exports are
/// listed and sorted (see the `provider_body_host.rs` sort), never type identity.
/// Identity is the producer-nominal `AnonymousNominalKey` itself (ADR-0066);
/// two keys that order equal here are still distinct types unless they are the
/// same key. Since RUE-1112 deleted the last min-selection consumer
/// (`find_compatible_anon_enum`), no code treats this ordering as identity
/// authority.
pub(crate) fn anonymous_key_cmp(
    left: &IssuedAnonymousNominalKey,
    right: &IssuedAnonymousNominalKey,
) -> Ordering {
    anonymous_producer_root(&left.producer)
        .cmp(&anonymous_producer_root(&right.producer))
        .then_with(|| left.cmp(right))
}

/// Total presentation order for complete durable anonymous exports.
///
/// The producer-root/key prefix preserves the established presentation order.
/// Live pool ids never cross this boundary and identical duplicates are
/// interchangeable. Conflicting payloads for one canonical identity are
/// rejected before this presentation order is imposed.
fn anonymous_export_cmp(
    left: &crate::SemanticProducedAnonymousNominal,
    right: &crate::SemanticProducedAnonymousNominal,
) -> Ordering {
    anonymous_key_cmp(&left.identity, &right.identity).then_with(|| left.cmp(right))
}

/// Project live anonymous entries completely before imposing presentation
/// order. This keeps body-local pool allocation out of the exported artifact.
pub(crate) fn collect_anonymous_exports(
    entries: impl IntoIterator<
        Item = Result<crate::SemanticProducedAnonymousNominal, crate::SemanticBodyExportFailure>,
    >,
) -> Result<
    std::sync::Arc<[crate::SemanticProducedAnonymousNominal]>,
    crate::SemanticBodyExportFailure,
> {
    let mut by_identity = std::collections::BTreeMap::new();
    for result in entries {
        let mut export = result?;
        export.identity = export.identity.with_canonical_producer().into_owned();
        match by_identity.entry(export.identity.clone()) {
            std::collections::btree_map::Entry::Vacant(entry) => {
                entry.insert(export);
            }
            std::collections::btree_map::Entry::Occupied(entry) if entry.get() == &export => {}
            std::collections::btree_map::Entry::Occupied(_) => {
                return Err(crate::SemanticBodyExportFailure::AmbiguousStableIdentity);
            }
        }
    }
    let mut exports = by_identity.into_values().collect::<Vec<_>>();
    exports.sort_by(anonymous_export_cmp);
    Ok(exports.into())
}

/// Select the canonical live type for a consulted anonymous identity.
///
/// Empty-specialization producer aliases can give the same complete identity
/// more than one issued spelling. Every matching spelling must name the same
/// live type; disagreement is counterfeit/ambiguous and lookup fails closed.
/// The full canonical key remains the identity authority, so anchors and kinds
/// are never collapsed merely because their producers match.
pub(crate) fn canonical_consulted_type<'a, I>(
    entries: I,
    identity: &IssuedAnonymousNominalKey,
    kind: crate::AnonymousNominalKind,
) -> Result<Option<crate::Type>, crate::SemanticBodyExportFailure>
where
    I: Iterator<Item = (&'a crate::Type, &'a IssuedAnonymousNominalKey)>,
{
    let canonical = identity.with_canonical_producer();
    if canonical.kind != kind {
        return Err(crate::SemanticBodyExportFailure::WrongStableIdentityKind);
    }
    let mut selected = None;
    for (ty, candidate) in entries {
        if candidate.with_canonical_producer().as_ref() != canonical.as_ref() {
            continue;
        }
        let matching_kind = match kind {
            crate::AnonymousNominalKind::Struct => ty.as_struct().is_some(),
            crate::AnonymousNominalKind::Enum => ty.as_enum().is_some(),
        };
        if !matching_kind {
            return Err(crate::SemanticBodyExportFailure::WrongStableIdentityKind);
        }
        match selected {
            None => selected = Some(*ty),
            Some(selected) if selected == *ty => {}
            Some(_) => {
                return Err(crate::SemanticBodyExportFailure::AmbiguousStableIdentity);
            }
        }
    }
    Ok(selected)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ahash::{AHashMap, RandomState};
    use std::sync::Arc;

    fn definition_identity(kind: crate::AnonymousNominalKind) -> IssuedAnonymousNominalKey {
        crate::AnonymousNominalKey {
            kind,
            producer: crate::StableProducerId::Definition(crate::SemanticDefinitionToken::new(
                7, 11,
            )),
            anchor: rue_rir::RirStructuralAnchor::new(Vec::new()),
        }
    }

    fn produced(
        identity: &IssuedAnonymousNominalKey,
        field: &str,
        ty: crate::TypeInstanceKey<crate::SemanticDefinitionToken, crate::SemanticModuleToken>,
    ) -> crate::SemanticProducedAnonymousNominal {
        crate::SemanticProducedAnonymousNominal {
            identity: identity.clone(),
            shape: crate::SemanticProducedAnonymousNominalShape::Struct {
                fields: Arc::from([(Arc::from(field), ty)]),
                methods: Arc::new([]),
            },
            type_captures: Arc::new([]),
            value_captures: Arc::new([]),
        }
    }

    #[test]
    fn anonymous_exports_order_and_dedup_by_complete_durable_facts() {
        fn run(
            seeds: [u64; 4],
            reverse_insertion: bool,
            reverse_live_ids: bool,
        ) -> Arc<[crate::SemanticProducedAnonymousNominal]> {
            let first_identity = definition_identity(crate::AnonymousNominalKind::Struct);
            let mut second_identity = first_identity.clone();
            second_identity.anchor = rue_rir::RirStructuralAnchor::new(vec![
                rue_rir::RirStructuralPathSegment::AnonymousType(1),
            ]);
            let first = produced(&first_identity, "a", crate::TypeInstanceKey::I32);
            let second = produced(&second_identity, "b", crate::TypeInstanceKey::I64);
            let low = crate::Type::new_struct(crate::StructId::from_pool_index(2));
            let middle = crate::Type::new_struct(crate::StructId::from_pool_index(5));
            let high = crate::Type::new_struct(crate::StructId::from_pool_index(9));
            let (first_ty, second_ty) = if reverse_live_ids {
                (high, low)
            } else {
                (low, high)
            };
            let mut entries = vec![
                (first_ty, first.clone()),
                (second_ty, second),
                (middle, first),
            ];
            if reverse_insertion {
                entries.reverse();
            }
            let mut table = AHashMap::with_hasher(RandomState::with_seeds(
                seeds[0], seeds[1], seeds[2], seeds[3],
            ));
            for (ty, entry) in entries {
                table.insert(ty, entry);
            }

            collect_anonymous_exports(table.iter().map(|(ty, _)| {
                Ok::<_, crate::SemanticBodyExportFailure>(
                    table.get(ty).expect("projected entry").clone(),
                )
            }))
            .expect("durable projection is infallible")
        }

        let forward = run([1, 2, 3, 4], false, false);
        let reversed = run([9, 8, 7, 6], true, true);
        assert_eq!(forward, reversed);
        assert_eq!(forward.len(), 2, "identical durable duplicates collapse");
        assert!(forward[0] < forward[1]);
    }

    #[test]
    fn anonymous_exports_reject_conflicting_payloads_for_one_canonical_identity() {
        let (direct, specialized) = producer_aliases(crate::AnonymousNominalKind::Struct);
        let direct = produced(&direct, "a", crate::TypeInstanceKey::I32);
        let counterfeit = produced(&specialized, "b", crate::TypeInstanceKey::I64);
        assert_eq!(
            collect_anonymous_exports([Ok(direct), Ok(counterfeit)]),
            Err(crate::SemanticBodyExportFailure::AmbiguousStableIdentity)
        );
    }

    fn producer_aliases(
        kind: crate::AnonymousNominalKind,
    ) -> (IssuedAnonymousNominalKey, IssuedAnonymousNominalKey) {
        let definition = crate::SemanticDefinitionToken::new(7, 11);
        let base = crate::FunctionInstanceKey::Definition(definition);
        let direct = crate::AnonymousNominalKey {
            kind,
            producer: crate::StableProducerId::Function(crate::Node::new(base.clone())),
            anchor: rue_rir::RirStructuralAnchor::new(vec![
                rue_rir::RirStructuralPathSegment::AnonymousType(0),
            ]),
        };
        let specialized = crate::AnonymousNominalKey {
            kind,
            producer: crate::StableProducerId::Function(crate::Node::new(
                crate::FunctionInstanceKey::Specialization {
                    base: crate::Node::new(base),
                    arguments: crate::CanonicalArguments {
                        types: Arc::new([]),
                        values: Arc::new([]),
                    },
                },
            )),
            anchor: direct.anchor.clone(),
        };
        assert_eq!(
            direct.with_canonical_producer(),
            specialized.with_canonical_producer()
        );
        (direct, specialized)
    }

    fn live_type(kind: crate::AnonymousNominalKind, index: u32) -> crate::Type {
        match kind {
            crate::AnonymousNominalKind::Struct => {
                crate::Type::new_struct(crate::StructId::from_pool_index(index))
            }
            crate::AnonymousNominalKind::Enum => {
                crate::Type::new_enum(crate::EnumId::from_pool_index(index))
            }
        }
    }

    #[test]
    fn consulted_anonymous_selection_canonicalizes_empty_specialization_spelling() {
        for kind in [
            crate::AnonymousNominalKind::Struct,
            crate::AnonymousNominalKind::Enum,
        ] {
            let (direct, specialized) = producer_aliases(kind);
            let selected = live_type(kind, 9);
            let entries = [(selected, direct.clone()), (selected, specialized.clone())];
            for reverse in [false, true] {
                for requested_alias in [&direct, &specialized] {
                    let selected_alias = if reverse {
                        canonical_consulted_type(
                            entries.iter().rev().map(|(ty, key)| (ty, key)),
                            requested_alias,
                            kind,
                        )
                    } else {
                        canonical_consulted_type(
                            entries.iter().map(|(ty, key)| (ty, key)),
                            requested_alias,
                            kind,
                        )
                    };
                    assert_eq!(
                        selected_alias,
                        Ok(Some(selected)),
                        "canonical producer spelling must not change lookup for {kind:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn consulted_anonymous_selection_keeps_different_anchors_distinct() {
        for kind in [
            crate::AnonymousNominalKind::Struct,
            crate::AnonymousNominalKind::Enum,
        ] {
            let (direct, _) = producer_aliases(kind);
            let mut sibling = direct.clone();
            sibling.anchor = rue_rir::RirStructuralAnchor::new(vec![
                rue_rir::RirStructuralPathSegment::AnonymousType(1),
            ]);
            let direct_ty = live_type(kind, 9);
            let sibling_ty = live_type(kind, 2);
            let entries = [(direct_ty, direct.clone()), (sibling_ty, sibling.clone())];
            for reverse in [false, true] {
                let selected_direct = if reverse {
                    canonical_consulted_type(
                        entries.iter().rev().map(|(ty, key)| (ty, key)),
                        &direct,
                        kind,
                    )
                } else {
                    canonical_consulted_type(
                        entries.iter().map(|(ty, key)| (ty, key)),
                        &direct,
                        kind,
                    )
                };
                assert_eq!(selected_direct, Ok(Some(direct_ty)));
                let selected_sibling = if reverse {
                    canonical_consulted_type(
                        entries.iter().rev().map(|(ty, key)| (ty, key)),
                        &sibling,
                        kind,
                    )
                } else {
                    canonical_consulted_type(
                        entries.iter().map(|(ty, key)| (ty, key)),
                        &sibling,
                        kind,
                    )
                };
                assert_eq!(selected_sibling, Ok(Some(sibling_ty)));
            }
        }
    }

    #[test]
    fn consulted_anonymous_selection_rejects_distinct_live_types_for_one_identity() {
        for kind in [
            crate::AnonymousNominalKind::Struct,
            crate::AnonymousNominalKind::Enum,
        ] {
            let (direct, specialized) = producer_aliases(kind);
            let entries = [
                (live_type(kind, 8), direct.clone()),
                (live_type(kind, 1), specialized),
            ];
            assert_eq!(
                canonical_consulted_type(entries.iter().map(|(ty, key)| (ty, key)), &direct, kind),
                Err(crate::SemanticBodyExportFailure::AmbiguousStableIdentity)
            );
            assert_eq!(
                canonical_consulted_type(
                    entries.iter().rev().map(|(ty, key)| (ty, key)),
                    &direct,
                    kind,
                ),
                Err(crate::SemanticBodyExportFailure::AmbiguousStableIdentity)
            );
        }
    }

    #[test]
    fn consulted_anonymous_selection_rejects_wrong_live_kind_for_exact_identity() {
        let identity = definition_identity(crate::AnonymousNominalKind::Struct);
        let wrong = live_type(crate::AnonymousNominalKind::Enum, 3);
        assert_eq!(
            canonical_consulted_type(
                [(&wrong, &identity)].into_iter(),
                &identity,
                crate::AnonymousNominalKind::Struct,
            ),
            Err(crate::SemanticBodyExportFailure::WrongStableIdentityKind)
        );
    }
}
