//! Per-body well-known `Option(payload)` demand planning.
//!
//! Every fallible intrinsic (`@read_line`, `@parse_i32/i64/u32/u64`) yields the
//! exact trusted standard-library `Option(payload)`. When a trusted `Option`
//! module is present in the program, this planner computes the set of payloads
//! the program demands and roots the matching comptime-`Option` specializations
//! under each requesting body's lease (see `revisioned_query_database.rs`). The
//! resolved enums reach AIR through a narrow per-body `WellKnownTypes` input,
//! never the body's composition/import universe.
//!
//! Planning is two-staged. This module builds the whole-program *catalogue* of
//! demandable `Option(payload)` specializations (one per payload the program
//! uses anywhere), each tagged with its [`FalliblePayload`] kind. Each reached
//! body then derives its OWN exact payload set from its canonical raw body
//! ([`scan_body_payload_kinds`]) and roots only the catalogue entries that set
//! selects, so a body gains a query edge to a specialization only for a payload
//! it actually uses and cannot inherit failure or cancellation from an unrelated
//! body's specialization. The semantic nucleus memoizes each specialization, so
//! bodies sharing a payload share one terminal. A program with no fallible
//! intrinsic demands nothing at all.

use std::collections::BTreeSet;
use std::sync::Arc;

use crate::body_query::WellKnownOptionDemand;
use crate::canonical_lower::CanonicalRirOutput;
use crate::canonical_merge::CanonicalMergedProgram;
use crate::declaration_candidate::{DeclarationCandidateCategory, DeclarationCandidateKey};
use crate::durable_semantics::DurableType;
use crate::semantic_query_nucleus::{
    ComptimeCallQueryKey, DeclarationSemanticQueryKey, SemanticQueryConfiguration,
};
use crate::{ModuleId, StableDefinitionKey, StableDefinitionKind, StableDefinitionNamespace};

const TRUSTED_OPTION_MODULE_PATH: &str = "\0rue-std/option.rue";
const TRUSTED_STRBUF_MODULE_PATH: &str = "\0rue-std/strbuf.rue";

/// One fallible-intrinsic payload demanded by a body.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum FalliblePayload {
    I32,
    I64,
    U32,
    U64,
    StrBuf,
}

impl FalliblePayload {
    /// Map a fallible-intrinsic name (as spelled after `@`) to the payload of
    /// the `Option` it returns. Non-fallible intrinsics return `None`.
    fn from_intrinsic_name(name: &str) -> Option<Self> {
        Some(match name {
            "read_line" => Self::StrBuf,
            "parse_i32" => Self::I32,
            "parse_i64" => Self::I64,
            "parse_u32" => Self::U32,
            "parse_u64" => Self::U64,
            _ => return None,
        })
    }
}

/// The fallible-intrinsic payload kinds one body's raw source text demands.
///
/// Each reached body derives its own exact payload set from its canonical raw
/// body BEFORE its per-body query roots any well-known `Option` specialization,
/// so a body roots only the payloads it actually uses and gains no query edge to
/// payloads owned by unrelated bodies. The scan is lexical (an `At` token
/// followed by the intrinsic identifier), so an intrinsic named only in a
/// comment or string literal demands nothing.
pub(crate) fn scan_body_payload_kinds(body: &str) -> BTreeSet<FalliblePayload> {
    let mut payloads = BTreeSet::new();
    let Ok((tokens, interner)) = rue_lexer::Lexer::new(body).tokenize() else {
        return payloads;
    };
    for pair in tokens.windows(2) {
        if !matches!(pair[0].kind, rue_lexer::TokenKind::At) {
            continue;
        }
        if let rue_lexer::TokenKind::Ident(symbol) = pair[1].kind
            && let Some(payload) = FalliblePayload::from_intrinsic_name(interner.resolve(&symbol))
        {
            payloads.insert(payload);
        }
    }
    payloads
}

/// Whether `module` is present among the program's canonical modules.
fn module_present(merged: &CanonicalMergedProgram, module: &ModuleId) -> bool {
    merged
        .ast()
        .modules()
        .iter()
        .any(|candidate| candidate.module_id() == module)
}

/// Scan the whole-program RIR for every fallible-intrinsic call, returning the
/// sorted, deduplicated set of demanded payloads.
///
/// Every fallible intrinsic yields the exact trusted std `Option(payload)` in
/// every context, so every use — bare, contextually annotated, matched, or the
/// operand of a `?` — demands the canonical `Option(payload)`. The registry is
/// the single durable source for that identity; context only checks it. Scanning
/// all uses (not only `?`-operands) is what lets a plain `let x = @parse_i64(…)`
/// or `match @parse_i32(…)` resolve to the canonical trusted `Option` rather
/// than depending on a surrounding annotation to supply the nominal.
fn scan_fallible_payloads(rir: &CanonicalRirOutput) -> BTreeSet<FalliblePayload> {
    let interner = rir.semantic_symbols().interner();
    let mut payloads = BTreeSet::new();
    for (_, inst) in rir.rir().iter() {
        if let rue_rir::InstData::Intrinsic { name, .. } = &inst.data
            && let Some(payload) = FalliblePayload::from_intrinsic_name(interner.resolve(name))
        {
            payloads.insert(payload);
        }
    }
    payloads
}

/// The trusted `StrBuf` nominal `DurableType`, when the trusted `StrBuf` module
/// is present. `@read_line`'s `Option(StrBuf)` cannot be demanded without it.
fn trusted_strbuf_type(merged: &CanonicalMergedProgram) -> Option<DurableType> {
    let module = ModuleId::from_trusted_standard_library_path(TRUSTED_STRBUF_MODULE_PATH).ok()?;
    module_present(merged, &module).then(|| {
        DurableType::Nominal(StableDefinitionKey::from_stable_parts(
            module,
            StableDefinitionNamespace::Type,
            StableDefinitionKind::Struct,
            "StrBuf",
            None,
        ))
    })
}

/// Plan the well-known `Option` demand catalogue for a semantic request.
///
/// Returns an empty catalogue whenever the trusted `Option` module is absent or
/// the program uses no fallible intrinsic; otherwise it holds one entry per
/// payload the program uses anywhere, each carrying the exact `ComptimeCall` to
/// root under a body's lease and the payload it satisfies. Per-body selection
/// from this catalogue happens in `body_transaction`.
pub(crate) fn plan_well_known_option_demands(
    merged: &CanonicalMergedProgram,
    rir: &CanonicalRirOutput,
    configuration: &SemanticQueryConfiguration,
) -> Arc<[WellKnownOptionDemand]> {
    let Ok(option_module) =
        ModuleId::from_trusted_standard_library_path(TRUSTED_OPTION_MODULE_PATH)
    else {
        return Arc::from(Vec::new());
    };
    // The trusted `Option` module must be present to name any specialization.
    // When it is absent the catalogue is empty and fallible-intrinsic resolution
    // uses its structural context check instead.
    if !module_present(merged, &option_module) {
        return Arc::from(Vec::new());
    }
    let payloads = scan_fallible_payloads(rir);
    if payloads.is_empty() {
        return Arc::from(Vec::new());
    }
    let option_candidate = DeclarationCandidateKey {
        module: option_module,
        category: DeclarationCandidateCategory::Function,
        name: Arc::from("Option"),
        owner: None,
        duplicate_discriminator: 0,
    };
    let strbuf = trusted_strbuf_type(merged);
    let mut demands = Vec::new();
    for payload in payloads {
        let payload_type = match payload {
            FalliblePayload::I32 => DurableType::I32,
            FalliblePayload::I64 => DurableType::I64,
            FalliblePayload::U32 => DurableType::U32,
            FalliblePayload::U64 => DurableType::U64,
            // Without the trusted StrBuf module the payload cannot be spelled;
            // the read_line demand is simply not rooted and falls through.
            FalliblePayload::StrBuf => match &strbuf {
                Some(strbuf) => strbuf.clone(),
                None => continue,
            },
        };
        let call = ComptimeCallQueryKey {
            declaration: DeclarationSemanticQueryKey {
                declaration: option_candidate.clone(),
                configuration: configuration.clone(),
            },
            type_arguments: Arc::from(vec![(Arc::<str>::from("T"), payload_type.clone())]),
            value_arguments: Arc::from(Vec::new()),
        };
        demands.push(WellKnownOptionDemand {
            kind: payload,
            payload: payload_type,
            call,
        });
    }
    Arc::from(demands)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn per_body_scan_derives_only_the_body_s_own_payloads() {
        // A body that uses `@parse_i32` demands exactly `I32` — not the payloads
        // of intrinsics that appear only in sibling bodies.
        assert_eq!(
            scan_body_payload_kinds("{ let x: i32 = @parse_i32(s)?; x }"),
            BTreeSet::from([FalliblePayload::I32]),
        );
        // Multiple distinct fallible intrinsics in one body accumulate.
        assert_eq!(
            scan_body_payload_kinds("{ let a = @parse_u64(s); let b = @read_line(); 0 }"),
            BTreeSet::from([FalliblePayload::U64, FalliblePayload::StrBuf]),
        );
    }

    #[test]
    fn per_body_scan_ignores_non_lexical_mentions_and_non_fallible_intrinsics() {
        // A fallible-intrinsic name in a comment or string literal is not a use.
        assert!(
            scan_body_payload_kinds("{ // @parse_i64 here\n let s = \"@parse_i32\"; 0 }")
                .is_empty()
        );
        // A non-fallible intrinsic demands nothing.
        assert!(scan_body_payload_kinds("{ let s = @to_string(1); 0 }").is_empty());
        // A body with no intrinsics at all demands nothing.
        assert!(scan_body_payload_kinds("{ let x = 1 + 2; x }").is_empty());
    }
}
