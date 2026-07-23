//! Task-owned body-local semantic overlay (RUE-1091, ADR-0066 §4).
//!
//! Each body evaluation creates one `BodySemanticOverlay`. The overlay owns the
//! body-local compact token space: it mints its own issuer-scoped
//! `SemanticDefinitionToken`/`SemanticModuleToken` slots and records the
//! stable-fact → local-ID mapping so repeated reads within the body are
//! constant-time. Materializing a recipe mints the *consuming* overlay's local
//! token; issuer-scoped tokens are never shared between two overlays, and no
//! local ID, slot pool, or minted token is retained in a recipe. Publication
//! converts the overlay result back to the durable body artifact through the
//! shared `BodyOutcomeResolver`, so compact overlay IDs never cross the query
//! boundary and the published diagnostics keep their upstream stable order.
//!
//! Slice 2b keeps this machinery inert to the production path: the production
//! `analyze_body_query` still publishes through the whole-epoch
//! `BoundDefinitionSet`. The overlay is exercised only by tests (and the
//! test-only overlay driver in `canonical_semantic`).

#![allow(dead_code)] // RUE-1091 slice 2b is inert: driven by later slices.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

use rue_air::{
    BodyOwnerToken, SemanticDefinitionToken, SemanticModuleToken, SemanticStableResolutionFailure,
};

use crate::canonical_semantic::BodyOutcomeResolver;
use crate::declaration_recipe::{DeclarationRecipe, EndpointRecipe, RecipeLogicalKey};
use crate::{ModuleId, StableDefinitionKey};

/// The overlay-local token minted for one imported recipe. It is scoped to the
/// overlay's issuer and is meaningless in any other overlay.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LocalToken {
    Definition(SemanticDefinitionToken),
    Module(SemanticModuleToken),
}

/// A fresh overlay issuer per body. The counter only needs to make distinct
/// overlays mint distinct issuer-scoped token spaces; the value is never
/// compared across overlays except by inequality.
static NEXT_OVERLAY_ISSUER: AtomicU64 = AtomicU64::new(1);

/// One task-owned body-local semantic overlay.
///
/// The overlay owns every compact ID it hands out. Definition slots are dense
/// indices into `definition_keys` (the reverse map used by publication) and are
/// interned through `definition_slots` (the forward map used to drive analysis);
/// module slots are dense indices into `module_ids`. `recipe_tokens` is the
/// body-local stable-fact → local-ID cache guaranteeing constant-time repeated
/// imports of the same recipe.
pub(crate) struct BodySemanticOverlay {
    issuer: u64,
    definition_keys: Vec<Option<StableDefinitionKey>>,
    definition_slots: HashMap<StableDefinitionKey, u32>,
    module_ids: Vec<ModuleId>,
    module_slots: HashMap<ModuleId, u32>,
    recipe_tokens: HashMap<RecipeLogicalKey, LocalToken>,
}

impl BodySemanticOverlay {
    /// Create an empty overlay with a fresh, unshared local token space.
    pub(crate) fn new() -> Self {
        Self {
            issuer: NEXT_OVERLAY_ISSUER.fetch_add(1, Ordering::Relaxed),
            definition_keys: Vec::new(),
            definition_slots: HashMap::new(),
            module_ids: Vec::new(),
            module_slots: HashMap::new(),
            recipe_tokens: HashMap::new(),
        }
    }

    /// This overlay's issuer. Two overlays never share an issuer, so a token
    /// minted here is rejected by any other overlay's resolver.
    pub(crate) fn issuer(&self) -> u64 {
        self.issuer
    }

    /// Mint (or return the existing) overlay-local definition token for a stable
    /// key. Interning in the caller's order assigns dense slots; seeding in the
    /// bound definition universe's sorted order therefore yields slots that
    /// mirror the whole-epoch token space, so publication resolves identically.
    pub(crate) fn intern_definition(&mut self, key: &StableDefinitionKey) -> SemanticDefinitionToken {
        if let Some(&slot) = self.definition_slots.get(key) {
            return SemanticDefinitionToken::new(self.issuer, slot);
        }
        let slot = self.definition_keys.len() as u32;
        self.definition_keys.push(Some(key.clone()));
        self.definition_slots.insert(key.clone(), slot);
        SemanticDefinitionToken::new(self.issuer, slot)
    }

    /// Mint an overlay-local module token for a module id.
    pub(crate) fn intern_module(&mut self, module: &ModuleId) -> SemanticModuleToken {
        if let Some(&slot) = self.module_slots.get(module) {
            return SemanticModuleToken::new(self.issuer, slot);
        }
        let slot = self.module_ids.len() as u32;
        self.module_ids.push(module.clone());
        self.module_slots.insert(module.clone(), slot);
        SemanticModuleToken::new(self.issuer, slot)
    }

    /// A definition endpoint recipe carries a resolvable identity but not the
    /// full stable definition key. It still mints an owned local slot so the
    /// token space stays dense; the reverse map records no key, which publication
    /// treats as unresolved (analysis never issues such a token for a real body).
    fn mint_endpoint_definition(&mut self) -> SemanticDefinitionToken {
        let slot = self.definition_keys.len() as u32;
        self.definition_keys.push(None);
        SemanticDefinitionToken::new(self.issuer, slot)
    }

    /// Lazily import a recipe, minting this overlay's local token on first
    /// consumption and recording the stable-fact → local-ID mapping. A repeated
    /// import of the same recipe is a constant-time cache read that returns the
    /// same local token and never mints a second slot.
    pub(crate) fn import_recipe(&mut self, recipe: &DeclarationRecipe) -> LocalToken {
        let logical = recipe.logical_key();
        if let Some(token) = self.recipe_tokens.get(&logical) {
            return *token;
        }
        let token = match recipe {
            DeclarationRecipe::Definition(recipe) => {
                LocalToken::Definition(self.intern_definition(&recipe.key))
            }
            DeclarationRecipe::BodyOwner(recipe) => {
                LocalToken::Definition(self.intern_definition(&recipe.key))
            }
            DeclarationRecipe::Module(recipe) => {
                LocalToken::Module(self.intern_module(&recipe.module))
            }
            DeclarationRecipe::Endpoint(EndpointRecipe::Module(recipe)) => {
                LocalToken::Module(self.intern_module(&recipe.module))
            }
            DeclarationRecipe::Endpoint(EndpointRecipe::Definition(_)) => {
                LocalToken::Definition(self.mint_endpoint_definition())
            }
        };
        self.recipe_tokens.insert(logical, token);
        token
    }

    /// The overlay-local definition token previously minted for a stable key, or
    /// `Missing` if the body never imported that fact. Used to drive analysis.
    pub(crate) fn semantic_token_for_key(
        &self,
        key: &StableDefinitionKey,
    ) -> Result<SemanticDefinitionToken, SemanticStableResolutionFailure> {
        self.definition_slots
            .get(key)
            .map(|&slot| SemanticDefinitionToken::new(self.issuer, slot))
            .ok_or(SemanticStableResolutionFailure::Missing)
    }

    /// The overlay-local module token previously minted for a module id.
    pub(crate) fn module_token_for(
        &self,
        module: &ModuleId,
    ) -> Result<SemanticModuleToken, SemanticStableResolutionFailure> {
        self.module_slots
            .get(module)
            .map(|&slot| SemanticModuleToken::new(self.issuer, slot))
            .ok_or(SemanticStableResolutionFailure::Missing)
    }

    /// The stable identity mapping recorded for a previously imported recipe, if
    /// any. Two overlays that imported the same recipe map it to distinct local
    /// tokens but to the same stable identity through their reverse maps.
    #[cfg(test)]
    pub(crate) fn imported_local_token(&self, logical: &RecipeLogicalKey) -> Option<LocalToken> {
        self.recipe_tokens.get(logical).copied()
    }

    #[cfg(test)]
    pub(crate) fn definition_slot_count(&self) -> usize {
        self.definition_keys.len()
    }

    #[cfg(test)]
    pub(crate) fn module_slot_count(&self) -> usize {
        self.module_ids.len()
    }
}

impl BodyOutcomeResolver for BodySemanticOverlay {
    fn key_for_semantic_token(
        &self,
        token: SemanticDefinitionToken,
    ) -> Result<StableDefinitionKey, SemanticStableResolutionFailure> {
        if token.issuer() != self.issuer {
            return Err(SemanticStableResolutionFailure::ForeignIssuer);
        }
        self.definition_keys
            .get(token.slot() as usize)
            .and_then(|key| key.clone())
            .ok_or(SemanticStableResolutionFailure::Missing)
    }

    fn module_for_semantic_token(
        &self,
        token: SemanticModuleToken,
    ) -> Result<ModuleId, SemanticStableResolutionFailure> {
        if token.issuer() != self.issuer {
            return Err(SemanticStableResolutionFailure::ForeignIssuer);
        }
        self.module_ids
            .get(token.slot() as usize)
            .cloned()
            .ok_or(SemanticStableResolutionFailure::Missing)
    }

    fn definition_key_for_body_token(
        &self,
        token: BodyOwnerToken,
    ) -> Result<StableDefinitionKey, String> {
        if token.issuer() != self.issuer {
            return Err("body owner token belongs to a foreign overlay issuer".to_owned());
        }
        self.definition_keys
            .get(token.slot() as usize)
            .and_then(|key| key.clone())
            .ok_or_else(|| "body owner token has an invalid overlay slot".to_owned())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::declaration_recipe::{DeclarationRecipe, DefinitionRecipe};
    use crate::durable_semantics::{
        DurableDeclarationPayload, DurableDeclarationSemantic, DurableType,
    };
    use crate::{
        CompileOptions, StableDefinitionKind, StableDefinitionNamespace, StablePreviewFeatures,
        SourceMetadata, SourceSnapshot,
    };
    use rue_span::FileId;

    fn definition_recipe(name: &str) -> DeclarationRecipe {
        let key = StableDefinitionKey::for_test(
            ModuleId::from_logical_path("pkg/main.rue").unwrap(),
            StableDefinitionNamespace::Value,
            StableDefinitionKind::Function,
            name,
            None,
        );
        DeclarationRecipe::Definition(Box::new(DefinitionRecipe::from_durable(
            &DurableDeclarationSemantic {
                key,
                is_public: true,
                payload: DurableDeclarationPayload::Callable {
                    parameters: Arc::from([]),
                    result: DurableType::Unit,
                    has_self: false,
                    is_unchecked: false,
                },
            },
            &[],
        )))
    }

    // Two overlays that materialize the SAME recipe mint distinct local tokens
    // (distinct issuer-scoped token spaces) while recording the same stable
    // identity. This is the local-token-minting invariant: a recipe carries no
    // token, and each consuming overlay mints its own.
    #[test]
    fn same_recipe_mints_distinct_local_tokens_with_equal_stable_identity() {
        let recipe = definition_recipe("apply");
        let logical = recipe.logical_key();

        let mut first = BodySemanticOverlay::new();
        let mut second = BodySemanticOverlay::new();
        assert_ne!(first.issuer(), second.issuer(), "overlays share no issuer");

        let first_token = first.import_recipe(&recipe);
        let second_token = second.import_recipe(&recipe);
        assert_ne!(
            first_token, second_token,
            "the same recipe must mint distinct local tokens in two overlays"
        );

        let (LocalToken::Definition(first_token), LocalToken::Definition(second_token)) =
            (first_token, second_token)
        else {
            panic!("a definition recipe mints a definition token");
        };
        // Same stable identity through each overlay's own reverse map.
        assert_eq!(
            first.key_for_semantic_token(first_token).unwrap(),
            second.key_for_semantic_token(second_token).unwrap()
        );
        // The body-local cache makes a repeated import a constant-time read that
        // returns the same token and mints no second slot.
        assert_eq!(first.import_recipe(&recipe), LocalToken::Definition(first_token));
        assert_eq!(first.definition_slot_count(), 1);
        assert_eq!(first.imported_local_token(&logical), Some(LocalToken::Definition(first_token)));
    }

    // Two overlays for different bodies share no local ID, slot pool, or recorded
    // mapping. Mutating one overlay is invisible to the other, proven by both
    // construction (distinct issuers) and assertion (independent maps).
    #[test]
    fn two_overlays_share_no_local_state() {
        let shared = definition_recipe("shared");
        let shared_logical = shared.logical_key();
        let private = definition_recipe("private_to_first");
        let private_logical = private.logical_key();

        let mut first = BodySemanticOverlay::new();
        let mut second = BodySemanticOverlay::new();

        first.import_recipe(&shared);
        second.import_recipe(&shared);

        // Mutate only the first overlay.
        first.import_recipe(&private);

        assert_eq!(first.definition_slot_count(), 2);
        assert_eq!(second.definition_slot_count(), 1, "second overlay is untouched");
        assert!(second.imported_local_token(&private_logical).is_none());

        // Distinct token spaces: the first overlay's shared token is a foreign
        // issuer to the second overlay's resolver, so it never resolves there.
        let LocalToken::Definition(first_shared) =
            first.imported_local_token(&shared_logical).unwrap()
        else {
            unreachable!()
        };
        assert!(
            second.key_for_semantic_token(first_shared).is_err(),
            "one overlay's token must be foreign to another overlay"
        );
    }

    fn snapshot(source: &str) -> SourceSnapshot {
        let file = FileId::new(1);
        let metadata = SourceMetadata::new(
            file,
            [(file, "/main.rue".to_owned())].into_iter().collect(),
            [(file, "main.rue".to_owned())].into_iter().collect(),
        )
        .unwrap();
        SourceSnapshot::new(
            metadata,
            [(file, Arc::new(source.to_owned()))].into_iter().collect(),
        )
        .unwrap()
    }

    struct BodyQueryInputs {
        merged: crate::CanonicalMergedProgram,
        rir: crate::CanonicalRirOutput,
        options: CompileOptions,
        imports: crate::CanonicalImportGraph,
        query_shells: Vec<rue_air::SemanticDeclarationShell>,
        query_declarations: Arc<[DurableDeclarationSemantic]>,
        definitions: crate::bound_definitions::BoundDefinitionSet,
    }

    impl BodyQueryInputs {
        fn build(source: &str) -> Self {
            let snapshot = snapshot(source);
            let parsed = crate::parsed_modules::parse_source_snapshot_modules(&snapshot).unwrap();
            let merged = crate::merge_parsed_modules(&parsed).unwrap();
            let rir = crate::lower_canonical_rir(&merged).unwrap();
            let options = CompileOptions::default();
            let imports = crate::bound_definitions::test_fixture_import_graph(&merged).unwrap();
            let (definitions, query_declarations, _) =
                crate::bound_definitions::bind_canonical_declaration_semantics(
                    &merged,
                    &rir,
                    options.preview_features.clone(),
                    options.target,
                )
                .unwrap();
            let query_shells =
                crate::canonical_semantic::query_owned_declaration_shells_for_test(
                    &merged,
                    &rir,
                    options.preview_features.clone(),
                    options.target,
                    &imports,
                )
                .unwrap()
                .declaration_shells()
                .cloned()
                .collect::<Vec<_>>();
            Self {
                merged,
                rir,
                options,
                imports,
                query_shells,
                query_declarations,
                definitions,
            }
        }

        fn body_key(&self, kind: StableDefinitionKind, name: &str) -> crate::body_query::BodyQueryKey {
            let definition = self
                .definitions
                .definitions()
                .iter()
                .find(|record| {
                    record.stable_key().kind() == kind && record.stable_key().name() == name
                })
                .unwrap_or_else(|| panic!("no {kind:?} named {name}"))
                .stable_key()
                .clone();
            crate::body_query::BodyQueryKey {
                instance: crate::FunctionInstanceKey::Definition(definition),
                configuration: crate::semantic_query_nucleus::SemanticQueryConfiguration {
                    target: self.options.target,
                    preview_features: StablePreviewFeatures::new(&self.options.preview_features),
                },
            }
        }

        fn production(
            &self,
            key: &crate::body_query::BodyQueryKey,
            cancellation: &rue_query::CancellationToken,
        ) -> Result<crate::body_query::BodyTransaction, rue_query::QueryAbort> {
            let mut work = rue_air::PerBodyDeclarationContextWork::default();
            crate::canonical_semantic::analyze_body_query(
                &self.merged,
                &self.rir,
                &self.options,
                &self.imports,
                &self.query_shells,
                &self.query_declarations,
                &[],
                &crate::body_query::WellKnownOptionResolution::default(),
                key,
                cancellation,
                &mut work,
            )
        }

        fn overlay(
            &self,
            key: &crate::body_query::BodyQueryKey,
            cancellation: &rue_query::CancellationToken,
            overlay: &mut BodySemanticOverlay,
        ) -> Result<crate::body_query::BodyTransaction, rue_query::QueryAbort> {
            crate::canonical_semantic::analyze_body_via_overlay(
                &self.merged,
                &self.rir,
                &self.options,
                &self.imports,
                &self.query_shells,
                &self.query_declarations,
                &[],
                &crate::body_query::WellKnownOptionResolution::default(),
                key,
                cancellation,
                overlay,
            )
        }
    }

    // An overlay-built body result publishes a durable `BodyTransaction` equal to
    // what the production `BoundDefinitionSet` path publishes for the same body,
    // across representative bodies: a plain free function, a method with a
    // receiver, and a producer body that materializes a producer-nominal
    // anonymous type. Equality includes references, produced nominals, and the
    // canonical body itself.
    #[test]
    fn overlay_publication_equals_production_for_representative_bodies() {
        let cases: &[(&str, StableDefinitionKind, &str)] = &[
            (
                "fn add(x: i32, y: i32) -> i32 { x + y } fn main() -> i32 { add(1, 2) }",
                StableDefinitionKind::Function,
                "main",
            ),
            (
                "struct Counter { value: i32, fn get(self) -> i32 { self.value } } \
                 fn main() -> i32 { Counter { value: 3 }.get() }",
                StableDefinitionKind::Method,
                "get",
            ),
            (
                "fn Pair() -> type { struct { a: i32, b: i32 } } \
                 fn main() -> i32 { let P = Pair(); let p: P = P { a: 1, b: 2 }; p.a + p.b }",
                StableDefinitionKind::Function,
                "Pair",
            ),
        ];
        for (source, kind, name) in cases {
            let inputs = BodyQueryInputs::build(source);
            let key = inputs.body_key(*kind, name);
            let cancellation = rue_query::CancellationToken::new();
            let production = inputs.production(&key, &cancellation).unwrap();
            let mut overlay = BodySemanticOverlay::new();
            let overlaid = inputs.overlay(&key, &cancellation, &mut overlay).unwrap();
            assert!(
                crate::body_query::transaction_equal(&production, &overlaid),
                "overlay publication diverged from production for body `{name}`:\n  \
                 production={production:?}\n  overlay={overlaid:?}"
            );
            // The overlay owns a token space disjoint from any epoch's.
            assert!(overlay.definition_slot_count() > 0);
        }
    }

    // A body that fails semantic analysis through the overlay yields the same
    // deterministic failure artifact and the same ordered diagnostics as the
    // production path. `transaction_equal` compares the rendered diagnostic
    // stream, so an ordering difference would fail this assertion.
    #[test]
    fn overlay_publication_equals_production_for_a_failing_body() {
        let inputs = BodyQueryInputs::build("fn main() -> i32 { true }");
        let key = inputs.body_key(StableDefinitionKind::Function, "main");
        let cancellation = rue_query::CancellationToken::new();
        let production = inputs.production(&key, &cancellation).unwrap();
        assert!(matches!(
            production,
            crate::body_query::BodyTransaction::DeterministicFailure { .. }
        ));
        let mut overlay = BodySemanticOverlay::new();
        let overlaid = inputs.overlay(&key, &cancellation, &mut overlay).unwrap();
        assert!(
            crate::body_query::transaction_equal(&production, &overlaid),
            "overlay failure diverged from production:\n  production={production:?}\n  \
             overlay={overlaid:?}"
        );
    }

    // A canceled overlay evaluation publishes nothing and retains no state: the
    // request control state dominates before any recipe is materialized, so the
    // overlay's token space stays empty and no `BodyTransaction` is produced.
    #[test]
    fn canceled_overlay_publishes_nothing_and_leaks_no_state() {
        let inputs = BodyQueryInputs::build("fn main() -> i32 { 0 }");
        let key = inputs.body_key(StableDefinitionKind::Function, "main");
        let cancellation = rue_query::CancellationToken::new();
        cancellation.cancel();
        let mut overlay = BodySemanticOverlay::new();
        let result = inputs.overlay(&key, &cancellation, &mut overlay);
        assert!(matches!(result, Err(rue_query::QueryAbort::Canceled)));
        assert_eq!(
            overlay.definition_slot_count(),
            0,
            "a canceled overlay materializes no recipe and retains no local ID"
        );
        assert_eq!(overlay.module_slot_count(), 0);
    }
}
