//! Anonymous struct handling.
//!
//! Anonymous `struct` and `enum` declaration expressions are *producer-nominal*
//! (ADR-0066). Their identity is the selected declaration expression under its
//! static enclosing comptime specialization — the `AnonymousNominalKey` — not a
//! structural comparison of fields, variants, method signatures, or bodies. Two
//! distinct producers that declare the same shape are distinct, non-assignable
//! types. There is no cross-producer structural search or stable-minimum
//! representative: each producer key owns exactly one entity. Anchor variants of
//! the *same* producer (a body reached under different definition-relative
//! anchor prefixes) alias to that one entity, which is all the alias map now
//! retains.
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

/// The one spelling of the prefix an anonymous enum's name opens with, before
/// its variants and their rendered payloads.
///
/// Only the prefix is a function of the digest; the rest of the name renders
/// the variants through the minting pool, so the whole name is not shareable
/// the way [`anonymous_struct_name`] is.
pub(crate) fn anonymous_enum_name_prefix(digest: u128) -> String {
    format!("__anon_enum_{digest:032x} {{ ")
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
