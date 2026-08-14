//! Exact per-body well-known `Option(payload)` query keys.
//!
//! Every fallible intrinsic (`@read_line`, `@parse_i32/i64/u32/u64`) yields the
//! exact trusted standard-library `Option(payload)`. Canonical RIR packing
//! derives the body's typed intrinsic set while visiting `InstData::Intrinsic`;
//! this module maps each kind directly to the exact trusted `Option` comptime-call
//! key with [`exact_option_query`]. No source rescan, whole-program RIR scan,
//! presence catalogue, or structural fallback participates in that identity.

use std::sync::Arc;

#[cfg(test)]
use std::collections::BTreeSet;

use crate::declaration_candidate::{DeclarationCandidateCategory, DeclarationCandidateKey};
use crate::durable_semantics::DurableType;
use crate::semantic_query_nucleus::{
    ComptimeCallQueryKey, DeclarationSemanticQueryKey, SemanticQueryConfiguration,
};
use crate::toolchain_module_demand::{OPTION_MODULE_LOGICAL_PATH, STRBUF_MODULE_LOGICAL_PATH};
use crate::{ModuleId, StableDefinitionKey, StableDefinitionKind, StableDefinitionNamespace};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExactOptionPrerequisite {
    pub(crate) stable: StableDefinitionKey,
    pub(crate) candidate: DeclarationCandidateKey,
}

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
    /// Map the canonical RIR artifact's typed intrinsic kind to its `Option`
    /// payload.
    pub(crate) fn from_rir(intrinsic: rue_rir::RirFallibleIntrinsic) -> Self {
        match intrinsic {
            rue_rir::RirFallibleIntrinsic::ParseI32 => Self::I32,
            rue_rir::RirFallibleIntrinsic::ParseI64 => Self::I64,
            rue_rir::RirFallibleIntrinsic::ParseU32 => Self::U32,
            rue_rir::RirFallibleIntrinsic::ParseU64 => Self::U64,
            rue_rir::RirFallibleIntrinsic::ReadLine => Self::StrBuf,
        }
    }

    #[cfg(test)]
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

/// Test-only lexical oracle for the typed artifact projection.
///
/// Production derives this set from typed RIR instructions during canonical
/// packing. The lexical helper remains only as independent test evidence that
/// comments and strings cannot manufacture an intrinsic node.
#[cfg(test)]
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

/// Map one body's canonical fallible payload kind to its exact trusted
/// `Option(payload)` comptime query.
///
/// This is a pure identity derivation. It performs no module-presence check and
/// accepts no merged program, RIR, or request-global catalogue. The session
/// parks missing trusted modules before the body transaction; once the body
/// enters, this exact key must resolve or the request fails closed.
pub(crate) fn exact_option_query(
    kind: FalliblePayload,
    configuration: &SemanticQueryConfiguration,
) -> (DurableType, ComptimeCallQueryKey) {
    let option_module = ModuleId::from_trusted_standard_library_path(OPTION_MODULE_LOGICAL_PATH)
        .expect("the canonical trusted Option module path is valid");
    let option_candidate = exact_option_candidate(option_module);
    let payload = exact_payload_type(kind);
    let call = ComptimeCallQueryKey {
        declaration: DeclarationSemanticQueryKey {
            declaration: option_candidate,
            configuration: configuration.clone(),
        },
        type_arguments: Arc::from([(Arc::<str>::from("T"), payload.clone())]),
        value_arguments: Arc::from([]),
    };
    (payload, call)
}

fn exact_option_candidate(option_module: ModuleId) -> DeclarationCandidateKey {
    DeclarationCandidateKey {
        module: option_module,
        category: DeclarationCandidateCategory::Function,
        name: Arc::from("Option"),
        owner: None,
        duplicate_discriminator: 0,
    }
}

fn exact_payload_type(kind: FalliblePayload) -> DurableType {
    match kind {
        FalliblePayload::I32 => DurableType::I32,
        FalliblePayload::I64 => DurableType::I64,
        FalliblePayload::U32 => DurableType::U32,
        FalliblePayload::U64 => DurableType::U64,
        FalliblePayload::StrBuf => DurableType::Nominal(StableDefinitionKey::from_stable_parts(
            ModuleId::from_trusted_standard_library_path(STRBUF_MODULE_LOGICAL_PATH)
                .expect("the canonical trusted StrBuf module path is valid"),
            StableDefinitionNamespace::Type,
            StableDefinitionKind::Struct,
            "StrBuf",
            None,
        )),
    }
}

/// Exact declarations that must already be present before resolving one
/// body-owned `Option(payload)` specialization.
///
/// The stable classifier observes only these independently keyed declarations:
/// trusted `Option` for every payload, plus trusted `StrBuf` only for
/// `@read_line`. Host parking remains the acquisition authority; this is the
/// query-owned fail-closed check for callers that bypass or race that boundary.
pub(crate) fn exact_option_prerequisites(kind: FalliblePayload) -> Vec<ExactOptionPrerequisite> {
    let option_module = ModuleId::from_trusted_standard_library_path(OPTION_MODULE_LOGICAL_PATH)
        .expect("the canonical trusted Option module path is valid");
    let mut prerequisites = vec![ExactOptionPrerequisite {
        stable: StableDefinitionKey::from_stable_parts(
            option_module.clone(),
            StableDefinitionNamespace::Value,
            StableDefinitionKind::Function,
            "Option",
            None,
        ),
        candidate: exact_option_candidate(option_module),
    }];
    if kind == FalliblePayload::StrBuf {
        let module = ModuleId::from_trusted_standard_library_path(STRBUF_MODULE_LOGICAL_PATH)
            .expect("the canonical trusted StrBuf module path is valid");
        prerequisites.push(ExactOptionPrerequisite {
            stable: StableDefinitionKey::from_stable_parts(
                module.clone(),
                StableDefinitionNamespace::Type,
                StableDefinitionKind::Struct,
                "StrBuf",
                None,
            ),
            candidate: DeclarationCandidateKey {
                module,
                category: DeclarationCandidateCategory::Struct,
                name: Arc::from("StrBuf"),
                owner: None,
                duplicate_discriminator: 0,
            },
        });
    }
    prerequisites
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_payload_maps_to_one_exact_trusted_option_key() {
        let configuration = SemanticQueryConfiguration {
            target: rue_target::Target::X86_64Linux,
            preview_features: crate::StablePreviewFeatures::new(&crate::PreviewFeatures::default()),
        };
        for (kind, expected) in [
            (FalliblePayload::I32, DurableType::I32),
            (FalliblePayload::I64, DurableType::I64),
            (FalliblePayload::U32, DurableType::U32),
            (FalliblePayload::U64, DurableType::U64),
            (
                FalliblePayload::StrBuf,
                DurableType::Nominal(StableDefinitionKey::from_stable_parts(
                    ModuleId::from_trusted_standard_library_path(STRBUF_MODULE_LOGICAL_PATH)
                        .unwrap(),
                    StableDefinitionNamespace::Type,
                    StableDefinitionKind::Struct,
                    "StrBuf",
                    None,
                )),
            ),
        ] {
            let (payload, query) = exact_option_query(kind, &configuration);
            assert_eq!(payload, expected, "{kind:?}");
            assert_eq!(
                query.declaration.declaration.module.as_str(),
                OPTION_MODULE_LOGICAL_PATH
            );
            assert_eq!(
                query.declaration.declaration.category,
                DeclarationCandidateCategory::Function
            );
            assert_eq!(query.declaration.declaration.name.as_ref(), "Option");
            assert_eq!(query.declaration.declaration.duplicate_discriminator, 0);
            assert_eq!(query.declaration.configuration, configuration);
            assert_eq!(
                query.type_arguments.as_ref(),
                &[(Arc::<str>::from("T"), expected)]
            );
            assert!(query.value_arguments.is_empty());

            let prerequisites = exact_option_prerequisites(kind);
            assert_eq!(
                prerequisites[0].stable.module().as_str(),
                OPTION_MODULE_LOGICAL_PATH
            );
            assert_eq!(prerequisites[0].stable.name(), "Option");
            assert_eq!(prerequisites[0].candidate, query.declaration.declaration);
            if kind == FalliblePayload::StrBuf {
                assert_eq!(prerequisites.len(), 2);
                assert_eq!(
                    prerequisites[1].stable.module().as_str(),
                    STRBUF_MODULE_LOGICAL_PATH
                );
                assert_eq!(prerequisites[1].stable.name(), "StrBuf");
                assert_eq!(
                    prerequisites[1].candidate.category,
                    DeclarationCandidateCategory::Struct
                );
            } else {
                assert_eq!(prerequisites.len(), 1);
            }
        }
    }

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
