//! Owned, session-stable declaration recipes (RUE-1091, ADR-0066 §4).
//!
//! A declaration recipe is the owned, data-only form of one authoritative
//! semantic fact. It carries only stable identities and durable payloads that
//! are already suitable as a query result: stable definition and module
//! identities, durable types and comptime values, declaration shape,
//! visibility, language-item metadata, and drop/`@copy` flags.
//!
//! A recipe deliberately contains none of the request-scoped machinery the ADR
//! forbids: no `Sema`/`BoundSema`/`DeclarationShells`, no live or borrowed RIR,
//! no `Spur`, no compact AIR type or nominal IDs, no spans or file-local IDs, no
//! issuer-scoped installation tokens, no mutable namespace or endpoint tables,
//! and no whole-revision stamp. This is enforced by construction: every field
//! type below is itself a durable stable identity or an owned scalar/text value.
//! The negative structural test `recipe_debug_carries_no_forbidden_representation`
//! is the executable proof that no span, file ID, or issuer token leaks through
//! conversion even from source artifacts that carry them.
//!
//! Slice 2a is completely inert: this module defines the recipe data model and
//! its pure converters only. No production path constructs or consumes a recipe,
//! there is no cache, no overlay, and no provider. Materialization — minting a
//! consuming overlay's local token from a recipe — is a later slice; recipes
//! never carry issuer-scoped tokens themselves.

#![allow(dead_code)] // RUE-1091 slice 2a is inert: consumed by later slices.

use std::sync::Arc;

use rue_air::{BodyOwnerKind, LangItem};

use crate::durable_semantics::{
    DurableAnonymousNominal, DurableDeclarationPayload, DurableDeclarationSemantic,
};
use crate::revisioned_query_database::ModuleIndexEntry;
use crate::{
    DefinitionKind, DefinitionNamespace, ModuleId, StableDefinitionKey, StableDefinitionKind,
};

/// The stable visibility fact of a declaration. This is the durable,
/// session-independent form of both the semantic `is_public` boolean and the
/// parser-level `Visibility`, and never carries a span or file identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RecipeVisibility {
    Private,
    Public,
}

impl RecipeVisibility {
    /// Durable visibility from the semantic `is_public` fact carried by a
    /// bound declaration.
    pub fn from_is_public(is_public: bool) -> Self {
        if is_public {
            Self::Public
        } else {
            Self::Private
        }
    }

    /// Durable visibility from the parser-level `Visibility`. A module index
    /// entry may record no explicit visibility, which stays `None`.
    pub fn from_ast(visibility: Option<rue_parser::ast::Visibility>) -> Option<Self> {
        visibility.map(|visibility| match visibility {
            rue_parser::ast::Visibility::Public => Self::Public,
            rue_parser::ast::Visibility::Private => Self::Private,
        })
    }
}

/// The query family a recipe belongs to. Definition, module, body-owner, and
/// endpoint recipes each carry stable semantic identities and owned payloads
/// only (ADR-0066 §4 "Authoritative facts and body-local state").
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RecipeFamily {
    Definition,
    Module,
    BodyOwner,
    Endpoint,
}

/// The stable logical identity of one recipe. Cache identity in a later slice
/// pairs this with the exact terminal incarnation/stamp; the logical key itself
/// is session-independent and stamp-free.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RecipeLogicalKey {
    Definition(StableDefinitionKey),
    Module(ModuleId),
    BodyOwner(StableDefinitionKey),
    Endpoint(EndpointLogicalKey),
}

impl RecipeLogicalKey {
    pub fn family(&self) -> RecipeFamily {
        match self {
            Self::Definition(_) => RecipeFamily::Definition,
            Self::Module(_) => RecipeFamily::Module,
            Self::BodyOwner(_) => RecipeFamily::BodyOwner,
            Self::Endpoint(_) => RecipeFamily::Endpoint,
        }
    }
}

/// The stable logical identity of an endpoint recipe: a resolvable definition
/// identity or a module identity. It carries the durable identity only — never
/// the issuer-scoped token or file index the installed AIR endpoint attaches.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum EndpointLogicalKey {
    Definition {
        module: ModuleId,
        namespace: DefinitionNamespace,
        kind: DefinitionKind,
        name: Arc<str>,
    },
    Module(ModuleId),
}

/// One owned, session-stable declaration fact.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DeclarationRecipe {
    Definition(Box<DefinitionRecipe>),
    Module(ModuleRecipe),
    BodyOwner(BodyOwnerRecipe),
    Endpoint(EndpointRecipe),
}

impl DeclarationRecipe {
    pub fn family(&self) -> RecipeFamily {
        match self {
            Self::Definition(_) => RecipeFamily::Definition,
            Self::Module(_) => RecipeFamily::Module,
            Self::BodyOwner(_) => RecipeFamily::BodyOwner,
            Self::Endpoint(_) => RecipeFamily::Endpoint,
        }
    }

    pub fn logical_key(&self) -> RecipeLogicalKey {
        match self {
            Self::Definition(recipe) => RecipeLogicalKey::Definition(recipe.key.clone()),
            Self::Module(recipe) => RecipeLogicalKey::Module(recipe.module.clone()),
            Self::BodyOwner(recipe) => RecipeLogicalKey::BodyOwner(recipe.key.clone()),
            Self::Endpoint(recipe) => RecipeLogicalKey::Endpoint(recipe.logical_key()),
        }
    }
}

/// The durable semantics of one bound declaration: its stable key, visibility,
/// declaration shape and durable payload (signature, constant/comptime value,
/// struct/enum shape with drop/`@copy` flags), its language-item identity, and
/// the producer-owned anonymous nominals its declaration materializes.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DefinitionRecipe {
    pub key: StableDefinitionKey,
    pub visibility: RecipeVisibility,
    pub payload: DurableDeclarationPayload,
    pub language_item: Option<LangItem>,
    pub produced_anonymous: Arc<[DurableAnonymousNominal]>,
}

impl DefinitionRecipe {
    /// Convert a durable declaration semantic (and the producer-owned anonymous
    /// nominals it materializes) into a definition recipe. The durable payload
    /// already holds the declaration shape and drop/`@copy` flags; visibility and
    /// language-item identity are computed here from the stable key.
    pub fn from_durable(
        declaration: &DurableDeclarationSemantic,
        produced_anonymous: &[DurableAnonymousNominal],
    ) -> Self {
        Self {
            key: declaration.key.clone(),
            visibility: RecipeVisibility::from_is_public(declaration.is_public),
            payload: declaration.payload.clone(),
            language_item: definition_language_item(&declaration.key),
            produced_anonymous: Arc::from(produced_anonymous.to_vec()),
        }
    }
}

/// The stable identity of a module-valued fact: the canonical module identity
/// and whether it is a trusted standard-library module.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ModuleRecipe {
    pub module: ModuleId,
    pub is_trusted_standard_library: bool,
}

impl ModuleRecipe {
    pub fn from_module(module: &ModuleId) -> Self {
        Self {
            module: module.clone(),
            is_trusted_standard_library: module.is_trusted_standard_library(),
        }
    }
}

/// A body-owning declaration's stable identity and shape. This is the durable
/// form of `BodyOwnerEndpoint` with the installation token and file index
/// removed: only the owning definition key, the body-owner kind, and the source
/// name/owner-name remain.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BodyOwnerRecipe {
    pub key: StableDefinitionKey,
    pub kind: BodyOwnerKind,
    pub name: Arc<str>,
    pub owner_name: Option<Arc<str>>,
}

impl BodyOwnerRecipe {
    /// Convert a durable declaration into a body-owner recipe, mirroring the
    /// kind/name/owner selection of `body_owner_endpoints` but carrying no
    /// issuer-scoped token or file index. Non-body-owning declarations convert
    /// to `None`.
    pub fn from_durable(declaration: &DurableDeclarationSemantic) -> Option<Self> {
        let key = &declaration.key;
        let (kind, name, owner_name) = match key.kind() {
            StableDefinitionKind::Function => {
                (BodyOwnerKind::FreeFunction, Arc::from(key.name()), None)
            }
            StableDefinitionKind::Method => (
                BodyOwnerKind::Method,
                Arc::from(key.name()),
                Some(Arc::from(key.owner()?.name())),
            ),
            StableDefinitionKind::AssociatedFunction => (
                BodyOwnerKind::AssociatedFunction,
                Arc::from(key.name()),
                Some(Arc::from(key.owner()?.name())),
            ),
            StableDefinitionKind::Destructor => {
                let owner: Arc<str> = key
                    .owner()
                    .map(|owner| Arc::from(owner.name()))
                    .unwrap_or_else(|| Arc::from(key.name()));
                (BodyOwnerKind::Destructor, owner.clone(), Some(owner))
            }
            _ => return None,
        };
        Some(Self {
            key: key.clone(),
            kind,
            name,
            owner_name,
        })
    }
}

/// A stable resolvable installation endpoint: either a definition identity or a
/// module identity. The AIR endpoint that this recipe seeds carries an
/// issuer-scoped token and a file index; the recipe carries neither. Those are
/// minted per-body during materialization in a later slice.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum EndpointRecipe {
    Definition(DefinitionEndpointRecipe),
    Module(ModuleEndpointRecipe),
}

impl EndpointRecipe {
    pub fn logical_key(&self) -> EndpointLogicalKey {
        match self {
            Self::Definition(recipe) => EndpointLogicalKey::Definition {
                module: recipe.module.clone(),
                namespace: recipe.namespace,
                kind: recipe.kind,
                name: recipe.name.clone(),
            },
            Self::Module(recipe) => EndpointLogicalKey::Module(recipe.module.clone()),
        }
    }
}

/// The stable identity of a definition installation endpoint. Built from a
/// per-module name index entry, it keeps the resolution-relevant columns
/// (namespace, kind, name, visibility) and drops the entry's name and
/// declaration spans.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DefinitionEndpointRecipe {
    pub module: ModuleId,
    pub namespace: DefinitionNamespace,
    pub kind: DefinitionKind,
    pub name: Arc<str>,
    pub visibility: Option<RecipeVisibility>,
}

impl DefinitionEndpointRecipe {
    /// Convert a per-module name-index entry into a definition endpoint recipe.
    /// The entry's `name_span` and `declaration_span` are file-local spans and
    /// are intentionally dropped; only the durable resolution columns survive.
    pub fn from_module_index_entry(module: &ModuleId, entry: &ModuleIndexEntry) -> Self {
        Self {
            module: module.clone(),
            namespace: entry.namespace,
            kind: entry.kind,
            name: entry.name.clone(),
            visibility: RecipeVisibility::from_ast(entry.visibility),
        }
    }
}

/// The stable identity of a module installation endpoint: the canonical module
/// identity, with no issuer token or file index.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ModuleEndpointRecipe {
    pub module: ModuleId,
}

impl ModuleEndpointRecipe {
    pub fn from_module(module: &ModuleId) -> Self {
        Self {
            module: module.clone(),
        }
    }
}

/// Standard-library language-item classification, shared by every rue-compiler
/// site that maps a declaration to a [`LangItem`] (the recipe converter below
/// and the module name index in `revisioned_query_database`). A candidate is a
/// language item only when it is a struct whose module has crossed the trusted
/// standard-library provenance boundary; [`LangItem::from_standard_library_nominal`]
/// remains the name authority. The two rue-air mirrors of this rule
/// (`durable_semantics` durable-import classification and the `LangItem` name
/// table itself) apply the same struct-only predicate, so a future non-struct
/// language item must widen every site together. The caller passes the
/// already-computed struct flag because kind is spelled with different enums
/// (`StableDefinitionKind` vs `DefinitionKind`) at the two call sites.
pub(crate) fn standard_library_language_item(
    is_struct: bool,
    module: &ModuleId,
    name: &str,
) -> Option<LangItem> {
    if is_struct && module.is_trusted_standard_library() {
        LangItem::from_standard_library_nominal(module.as_str(), name)
    } else {
        None
    }
}

/// The language-item identity of a declaration, classified only after its module
/// has crossed a trusted standard-library provenance boundary. This mirrors the
/// classification performed during durable declaration import.
fn definition_language_item(key: &StableDefinitionKey) -> Option<LangItem> {
    standard_library_language_item(
        key.kind() == StableDefinitionKind::Struct,
        key.module(),
        key.name(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::StableDefinitionNamespace;
    use crate::durable_semantics::{
        DurableAnonymousMethodSignature, DurableAnonymousMethodType, DurableAnonymousNominalShape,
        DurableConstValue, DurableParameterMode, DurableSemanticParameter, DurableType,
    };
    use crate::{AnonymousNominalKey, StableProducerId, StructuralAnchor, StructuralPathSegment};
    use rue_air::{AnonymousNominalKind, CanonicalArguments};
    use std::hash::{DefaultHasher, Hash, Hasher};

    fn module(path: &str) -> ModuleId {
        ModuleId::from_logical_path(path).unwrap()
    }

    fn definition_key(
        module: ModuleId,
        namespace: StableDefinitionNamespace,
        kind: StableDefinitionKind,
        name: &str,
        owner: Option<(StableDefinitionKind, &str)>,
    ) -> StableDefinitionKey {
        StableDefinitionKey::for_test(
            module,
            namespace,
            kind,
            name,
            owner.map(|(kind, name)| (kind, Arc::from(name))),
        )
    }

    fn stable_hash<T: Hash>(value: &T) -> u64 {
        let mut hasher = DefaultHasher::new();
        value.hash(&mut hasher);
        hasher.finish()
    }

    // A recipe must be a legitimate query value: a stable, orderable, hashable,
    // thread-shareable owned value. This bound is a necessary condition, not a
    // proof of stability on its own — a small forbidden Copy type such as a
    // `lasso::Spur` or a `BodyOwnerToken { issuer: u64, slot: u32 }` derives
    // every one of these traits and would satisfy the bound. What the bound does
    // reject is a borrowed or handle-bearing form that is not `'static`/owned or
    // not totally ordered. Exclusion of the small forbidden Copy types instead
    // rests on field-type discipline — every recipe field is itself a durable
    // stable identity — guarded mechanically by the exhaustive-destructure test
    // (`recipe_types_expose_no_unaudited_field`) and the runtime Debug scan
    // (`recipe_debug_carries_no_forbidden_representation`).
    fn assert_recipe_query_value<T: Send + Sync + Clone + Eq + Ord + Hash + 'static>() {}

    #[test]
    fn recipe_types_are_stable_owned_query_values() {
        assert_recipe_query_value::<RecipeVisibility>();
        assert_recipe_query_value::<RecipeFamily>();
        assert_recipe_query_value::<RecipeLogicalKey>();
        assert_recipe_query_value::<EndpointLogicalKey>();
        assert_recipe_query_value::<DeclarationRecipe>();
        assert_recipe_query_value::<DefinitionRecipe>();
        assert_recipe_query_value::<ModuleRecipe>();
        assert_recipe_query_value::<BodyOwnerRecipe>();
        assert_recipe_query_value::<EndpointRecipe>();
        assert_recipe_query_value::<DefinitionEndpointRecipe>();
        assert_recipe_query_value::<ModuleEndpointRecipe>();
    }

    // Exhaustively destructure every recipe type's fields and variants with no
    // `..` rest patterns. This is the real mechanical guard the trait bound only
    // appears to give: adding any field or variant to a recipe becomes a compile
    // error here, forcing a conscious audit that the new shape carries no
    // forbidden representation (span, file ID, issuer token, compact AIR ID, or
    // interned handle) before this test can be made to compile again.
    #[test]
    fn recipe_types_expose_no_unaudited_field() {
        let module_id = module("pkg/main.rue");
        let function = DurableDeclarationSemantic {
            key: definition_key(
                module_id.clone(),
                StableDefinitionNamespace::Value,
                StableDefinitionKind::Function,
                "apply",
                None,
            ),
            is_public: true,
            payload: DurableDeclarationPayload::Callable {
                parameters: Arc::from([]),
                result: DurableType::Unit,
                has_self: false,
                is_unchecked: false,
            },
        };

        // Fieldless enums: an exhaustive arm list guards against a new variant.
        match RecipeVisibility::Public {
            RecipeVisibility::Private | RecipeVisibility::Public => {}
        }
        match RecipeFamily::Definition {
            RecipeFamily::Definition
            | RecipeFamily::Module
            | RecipeFamily::BodyOwner
            | RecipeFamily::Endpoint => {}
        }

        let definition = DefinitionRecipe::from_durable(&function, &[]);
        let DefinitionRecipe {
            key: _,
            visibility: _,
            payload: _,
            language_item: _,
            produced_anonymous: _,
        } = &definition;

        let module_recipe = ModuleRecipe::from_module(&module_id);
        let ModuleRecipe {
            module: _,
            is_trusted_standard_library: _,
        } = &module_recipe;

        let body_owner = BodyOwnerRecipe::from_durable(&function).unwrap();
        let BodyOwnerRecipe {
            key: _,
            kind: _,
            name: _,
            owner_name: _,
        } = &body_owner;

        let definition_endpoint = DefinitionEndpointRecipe::from_module_index_entry(
            &module_id,
            &module_index_entry(
                DefinitionNamespace::ModuleItem,
                DefinitionKind::Function,
                Some(rue_parser::ast::Visibility::Public),
                "apply",
            ),
        );
        let DefinitionEndpointRecipe {
            module: _,
            namespace: _,
            kind: _,
            name: _,
            visibility: _,
        } = &definition_endpoint;

        let module_endpoint = ModuleEndpointRecipe::from_module(&module_id);
        let ModuleEndpointRecipe { module: _ } = &module_endpoint;

        // Recipe-bearing enums: every variant and its full payload shape.
        match EndpointRecipe::Definition(definition_endpoint.clone()) {
            EndpointRecipe::Definition(DefinitionEndpointRecipe {
                module: _,
                namespace: _,
                kind: _,
                name: _,
                visibility: _,
            }) => {}
            EndpointRecipe::Module(ModuleEndpointRecipe { module: _ }) => {}
        }
        match DeclarationRecipe::Module(module_recipe.clone()) {
            DeclarationRecipe::Definition(inner) => {
                let DefinitionRecipe {
                    key: _,
                    visibility: _,
                    payload: _,
                    language_item: _,
                    produced_anonymous: _,
                } = *inner;
            }
            DeclarationRecipe::Module(ModuleRecipe {
                module: _,
                is_trusted_standard_library: _,
            }) => {}
            DeclarationRecipe::BodyOwner(BodyOwnerRecipe {
                key: _,
                kind: _,
                name: _,
                owner_name: _,
            }) => {}
            DeclarationRecipe::Endpoint(
                EndpointRecipe::Definition(_) | EndpointRecipe::Module(_),
            ) => {}
        }

        // Logical-key enums: every variant, and the endpoint key's struct shape.
        match RecipeLogicalKey::Module(module_id.clone()) {
            RecipeLogicalKey::Definition(_) => {}
            RecipeLogicalKey::Module(_) => {}
            RecipeLogicalKey::BodyOwner(_) => {}
            RecipeLogicalKey::Endpoint(
                EndpointLogicalKey::Definition {
                    module: _,
                    namespace: _,
                    kind: _,
                    name: _,
                }
                | EndpointLogicalKey::Module(_),
            ) => {}
        }
    }

    #[test]
    fn fn_signature_declaration_converts_to_definition_recipe() {
        let key = definition_key(
            module("pkg/main.rue"),
            StableDefinitionNamespace::Value,
            StableDefinitionKind::Function,
            "apply",
            None,
        );
        let payload = DurableDeclarationPayload::Callable {
            parameters: Arc::from([
                DurableSemanticParameter {
                    ty: DurableType::I32,
                    mode: DurableParameterMode::Borrow,
                    is_comptime: false,
                },
                DurableSemanticParameter {
                    ty: DurableType::Bool,
                    mode: DurableParameterMode::Value,
                    is_comptime: true,
                },
            ]),
            result: DurableType::Unit,
            has_self: false,
            is_unchecked: true,
        };
        let declaration = DurableDeclarationSemantic {
            key: key.clone(),
            is_public: true,
            payload: payload.clone(),
        };
        let recipe = DefinitionRecipe::from_durable(&declaration, &[]);
        assert_eq!(recipe.key, key);
        assert_eq!(recipe.visibility, RecipeVisibility::Public);
        assert_eq!(recipe.payload, payload);
        assert_eq!(recipe.language_item, None);
        assert!(recipe.produced_anonymous.is_empty());
        assert_eq!(
            DeclarationRecipe::Definition(Box::new(recipe)).logical_key(),
            RecipeLogicalKey::Definition(key)
        );
    }

    #[test]
    fn const_comptime_value_declaration_converts_to_definition_recipe() {
        let key = definition_key(
            module("pkg/main.rue"),
            StableDefinitionNamespace::Value,
            StableDefinitionKind::ValueConst,
            "WIDTH",
            None,
        );
        let payload = DurableDeclarationPayload::Const {
            ty: DurableType::ComptimeType,
            value: DurableConstValue::Type(DurableType::Array {
                element: Box::new(DurableType::U8),
                len: 4,
            }),
        };
        let declaration = DurableDeclarationSemantic {
            key,
            is_public: false,
            payload: payload.clone(),
        };
        let recipe = DefinitionRecipe::from_durable(&declaration, &[]);
        assert_eq!(recipe.visibility, RecipeVisibility::Private);
        assert_eq!(recipe.payload, payload);
    }

    #[test]
    fn struct_and_enum_nominals_preserve_drop_and_copy_flags() {
        let struct_key = definition_key(
            module("pkg/main.rue"),
            StableDefinitionNamespace::Type,
            StableDefinitionKind::Struct,
            "Record",
            None,
        );
        let struct_payload = DurableDeclarationPayload::Struct {
            fields: Arc::from([(Arc::from("count"), DurableType::U64)]),
            is_copy: true,
            is_linear: false,
        };
        let struct_recipe = DefinitionRecipe::from_durable(
            &DurableDeclarationSemantic {
                key: struct_key,
                is_public: true,
                payload: struct_payload.clone(),
            },
            &[],
        );
        assert_eq!(struct_recipe.payload, struct_payload);
        assert!(matches!(
            struct_recipe.payload,
            DurableDeclarationPayload::Struct {
                is_copy: true,
                is_linear: false,
                ..
            }
        ));
        // A linear (must-drop) struct carries its flag through unchanged.
        let linear_payload = DurableDeclarationPayload::Struct {
            fields: Arc::from([(Arc::from("handle"), DurableType::U64)]),
            is_copy: false,
            is_linear: true,
        };
        let linear_recipe = DefinitionRecipe::from_durable(
            &DurableDeclarationSemantic {
                key: definition_key(
                    module("pkg/main.rue"),
                    StableDefinitionNamespace::Type,
                    StableDefinitionKind::Struct,
                    "Guard",
                    None,
                ),
                is_public: false,
                payload: linear_payload.clone(),
            },
            &[],
        );
        assert_eq!(linear_recipe.payload, linear_payload);

        let enum_key = definition_key(
            module("pkg/main.rue"),
            StableDefinitionNamespace::Type,
            StableDefinitionKind::Enum,
            "Choice",
            None,
        );
        let enum_payload = DurableDeclarationPayload::Enum {
            variants: Arc::from([
                (Arc::from("None"), Arc::from([]) as Arc<[DurableType]>),
                (
                    Arc::from("Some"),
                    Arc::from([DurableType::I32]) as Arc<[DurableType]>,
                ),
            ]),
        };
        let enum_recipe = DefinitionRecipe::from_durable(
            &DurableDeclarationSemantic {
                key: enum_key,
                is_public: true,
                payload: enum_payload.clone(),
            },
            &[],
        );
        assert_eq!(enum_recipe.payload, enum_payload);
    }

    #[test]
    fn producer_nominal_anonymous_facts_ride_the_definition_recipe() {
        let producer = definition_key(
            module("pkg/main.rue"),
            StableDefinitionNamespace::Value,
            StableDefinitionKind::Function,
            "make",
            None,
        );
        let anonymous = DurableAnonymousNominal {
            identity: AnonymousNominalKey {
                kind: AnonymousNominalKind::Struct,
                producer: StableProducerId::Definition(producer.clone()),
                anchor: StructuralAnchor::new(vec![StructuralPathSegment::AnonymousType(0)]),
                arguments: CanonicalArguments {
                    types: Arc::from([]),
                    values: Arc::from([]),
                },
            },
            shape: DurableAnonymousNominalShape::Struct {
                fields: Arc::from([(Arc::from("value"), DurableType::I32)]),
                methods: Arc::from([DurableAnonymousMethodSignature {
                    name: Arc::from("get"),
                    has_self: true,
                    self_mode: DurableParameterMode::Borrow,
                    parameters: Arc::from([]),
                    result: DurableAnonymousMethodType::Concrete(DurableType::I32),
                }]),
            },
            type_captures: Arc::from([]),
            value_captures: Arc::from([]),
        };
        let recipe = DefinitionRecipe::from_durable(
            &DurableDeclarationSemantic {
                key: producer,
                is_public: false,
                payload: DurableDeclarationPayload::Callable {
                    parameters: Arc::from([]),
                    result: DurableType::Unit,
                    has_self: false,
                    is_unchecked: false,
                },
            },
            std::slice::from_ref(&anonymous),
        );
        assert_eq!(recipe.produced_anonymous.as_ref(), &[anonymous]);
    }

    #[test]
    fn trusted_std_struct_recipe_carries_the_language_item() {
        let key = StableDefinitionKey::for_test(
            ModuleId::from_trusted_standard_library_path("\0rue-std/strbuf.rue").unwrap(),
            StableDefinitionNamespace::Type,
            StableDefinitionKind::Struct,
            "StrBuf",
            None,
        );
        let recipe = DefinitionRecipe::from_durable(
            &DurableDeclarationSemantic {
                key,
                is_public: true,
                payload: DurableDeclarationPayload::Struct {
                    fields: Arc::from([(
                        Arc::from("buf"),
                        DurableType::PtrMut(Box::new(DurableType::U8)),
                    )]),
                    is_copy: false,
                    is_linear: false,
                },
            },
            &[],
        );
        assert_eq!(recipe.language_item, Some(LangItem::StrBuf));
        // A user struct that merely shares the spelling never acquires it.
        let user = DefinitionRecipe::from_durable(
            &DurableDeclarationSemantic {
                key: definition_key(
                    module("pkg/main.rue"),
                    StableDefinitionNamespace::Type,
                    StableDefinitionKind::Struct,
                    "StrBuf",
                    None,
                ),
                is_public: true,
                payload: DurableDeclarationPayload::Struct {
                    fields: Arc::from([]),
                    is_copy: false,
                    is_linear: false,
                },
            },
            &[],
        );
        assert_eq!(user.language_item, None);
    }

    #[test]
    fn body_owner_recipe_covers_every_body_owning_kind_without_tokens() {
        let free = BodyOwnerRecipe::from_durable(&DurableDeclarationSemantic {
            key: definition_key(
                module("pkg/main.rue"),
                StableDefinitionNamespace::Value,
                StableDefinitionKind::Function,
                "run",
                None,
            ),
            is_public: true,
            payload: DurableDeclarationPayload::Callable {
                parameters: Arc::from([]),
                result: DurableType::Unit,
                has_self: false,
                is_unchecked: false,
            },
        })
        .unwrap();
        assert_eq!(free.kind, BodyOwnerKind::FreeFunction);
        assert_eq!(free.name.as_ref(), "run");
        assert_eq!(free.owner_name, None);

        let method = BodyOwnerRecipe::from_durable(&DurableDeclarationSemantic {
            key: definition_key(
                module("pkg/main.rue"),
                StableDefinitionNamespace::Method,
                StableDefinitionKind::Method,
                "len",
                Some((StableDefinitionKind::Struct, "Record")),
            ),
            is_public: true,
            payload: DurableDeclarationPayload::Callable {
                parameters: Arc::from([]),
                result: DurableType::U64,
                has_self: true,
                is_unchecked: false,
            },
        })
        .unwrap();
        assert_eq!(method.kind, BodyOwnerKind::Method);
        assert_eq!(method.owner_name.as_deref(), Some("Record"));

        let destructor = BodyOwnerRecipe::from_durable(&DurableDeclarationSemantic {
            key: definition_key(
                module("pkg/main.rue"),
                StableDefinitionNamespace::Destructor,
                StableDefinitionKind::Destructor,
                "Record",
                Some((StableDefinitionKind::Struct, "Record")),
            ),
            is_public: false,
            payload: DurableDeclarationPayload::Destructor,
        })
        .unwrap();
        assert_eq!(destructor.kind, BodyOwnerKind::Destructor);
        assert_eq!(destructor.owner_name.as_deref(), Some("Record"));

        // A struct declaration owns no body and yields no body-owner recipe.
        assert!(
            BodyOwnerRecipe::from_durable(&DurableDeclarationSemantic {
                key: definition_key(
                    module("pkg/main.rue"),
                    StableDefinitionNamespace::Type,
                    StableDefinitionKind::Struct,
                    "Record",
                    None,
                ),
                is_public: true,
                payload: DurableDeclarationPayload::Struct {
                    fields: Arc::from([]),
                    is_copy: false,
                    is_linear: false,
                },
            })
            .is_none()
        );
    }

    #[test]
    fn module_recipe_records_trusted_provenance() {
        let caller = ModuleRecipe::from_module(&module("pkg/main.rue"));
        assert!(!caller.is_trusted_standard_library);
        let trusted = ModuleRecipe::from_module(
            &ModuleId::from_trusted_standard_library_path("\0rue-std/strbuf.rue").unwrap(),
        );
        assert!(trusted.is_trusted_standard_library);
    }

    fn module_index_entry(
        namespace: DefinitionNamespace,
        kind: DefinitionKind,
        visibility: Option<rue_parser::ast::Visibility>,
        name: &str,
    ) -> ModuleIndexEntry {
        // Distinctive non-zero spans/file identity so the negative structural
        // test would observe them if conversion ever leaked one.
        ModuleIndexEntry {
            namespace,
            kind,
            visibility,
            name: Arc::from(name),
            language_item: None,
            name_span: rue_span::Span::with_file(rue_span::FileId::new(7), 41, 48),
            declaration_span: rue_span::Span::with_file(rue_span::FileId::new(7), 12, 96),
        }
    }

    #[test]
    fn definition_endpoint_recipe_drops_spans_and_keeps_resolution_columns() {
        let entry = module_index_entry(
            DefinitionNamespace::ModuleItem,
            DefinitionKind::Function,
            Some(rue_parser::ast::Visibility::Public),
            "apply",
        );
        let recipe =
            DefinitionEndpointRecipe::from_module_index_entry(&module("pkg/main.rue"), &entry);
        assert_eq!(recipe.namespace, DefinitionNamespace::ModuleItem);
        assert_eq!(recipe.kind, DefinitionKind::Function);
        assert_eq!(recipe.name.as_ref(), "apply");
        assert_eq!(recipe.visibility, Some(RecipeVisibility::Public));
        assert_eq!(
            EndpointRecipe::Definition(recipe).logical_key(),
            EndpointLogicalKey::Definition {
                module: module("pkg/main.rue"),
                namespace: DefinitionNamespace::ModuleItem,
                kind: DefinitionKind::Function,
                name: Arc::from("apply"),
            }
        );
    }

    #[test]
    fn module_endpoint_recipe_carries_only_the_module_identity() {
        let recipe = ModuleEndpointRecipe::from_module(&module("pkg/dep.rue"));
        assert_eq!(recipe.module, module("pkg/dep.rue"));
        assert_eq!(
            EndpointRecipe::Module(recipe).logical_key(),
            EndpointLogicalKey::Module(module("pkg/dep.rue"))
        );
    }

    // The runtime half of the no-forbidden-representation rule: build every
    // recipe family from source artifacts that DO carry spans and a file index,
    // then assert the recipe's Debug form contains no span, file-identity, or
    // issuer-token marker. Conversion strips them by construction.
    #[test]
    fn recipe_debug_carries_no_forbidden_representation() {
        // Precise Debug signatures of the forbidden representations, chosen so
        // no durable type's own name (e.g. `DurableSemanticParameter`) matches:
        // a span renders `Span { file_id: FileId(..), .. }`, an interned string
        // as `Spur(..)`, and per-body semantic state as `Sema { .. }`/
        // `BoundSema { .. }`. An issuer-scoped installation token is caught by
        // its type name via the `Token` marker; its custom Debug prints only the
        // slot (`BodyOwnerToken(<slot>)`), never the field name `issuer`, so a
        // separate `issuer` marker would be vacuous.
        let forbidden = [
            "Span {",
            "FileId(",
            "file_id:",
            "Spur",
            "Token",
            "Sema {",
            "BoundSema",
        ];
        let assert_clean = |label: &str, rendered: String| {
            for marker in forbidden {
                assert!(
                    !rendered.contains(marker),
                    "recipe {label} leaked forbidden representation `{marker}`: {rendered}"
                );
            }
        };

        let definition = DeclarationRecipe::Definition(Box::new(DefinitionRecipe::from_durable(
            &DurableDeclarationSemantic {
                key: definition_key(
                    module("pkg/main.rue"),
                    StableDefinitionNamespace::Value,
                    StableDefinitionKind::Function,
                    "apply",
                    None,
                ),
                is_public: true,
                payload: DurableDeclarationPayload::Callable {
                    parameters: Arc::from([DurableSemanticParameter {
                        ty: DurableType::I32,
                        mode: DurableParameterMode::Value,
                        is_comptime: false,
                    }]),
                    result: DurableType::Unit,
                    has_self: false,
                    is_unchecked: false,
                },
            },
            &[],
        )));
        assert_clean("definition", format!("{definition:?}"));

        let endpoint = DeclarationRecipe::Endpoint(EndpointRecipe::Definition(
            DefinitionEndpointRecipe::from_module_index_entry(
                &module("pkg/main.rue"),
                &module_index_entry(
                    DefinitionNamespace::ModuleItem,
                    DefinitionKind::Struct,
                    Some(rue_parser::ast::Visibility::Private),
                    "Record",
                ),
            ),
        ));
        assert_clean("endpoint", format!("{endpoint:?}"));

        let body_owner = DeclarationRecipe::BodyOwner(
            BodyOwnerRecipe::from_durable(&DurableDeclarationSemantic {
                key: definition_key(
                    module("pkg/main.rue"),
                    StableDefinitionNamespace::Value,
                    StableDefinitionKind::Function,
                    "run",
                    None,
                ),
                is_public: true,
                payload: DurableDeclarationPayload::Callable {
                    parameters: Arc::from([]),
                    result: DurableType::Unit,
                    has_self: false,
                    is_unchecked: false,
                },
            })
            .unwrap(),
        );
        assert_clean("body-owner", format!("{body_owner:?}"));

        let module_recipe =
            DeclarationRecipe::Module(ModuleRecipe::from_module(&module("pkg/main.rue")));
        assert_clean("module", format!("{module_recipe:?}"));
    }

    // Recipes carry only session-independent stable identities: no issuer token,
    // no whole-revision stamp. Two recipes built from equal durable inputs in
    // two separate "sessions" (independently constructed identities) are equal
    // and hash equally.
    #[test]
    fn identical_declarations_yield_equal_recipes_across_sessions() {
        let build = || {
            DefinitionRecipe::from_durable(
                &DurableDeclarationSemantic {
                    key: definition_key(
                        module("pkg/main.rue"),
                        StableDefinitionNamespace::Type,
                        StableDefinitionKind::Struct,
                        "Record",
                        None,
                    ),
                    is_public: true,
                    payload: DurableDeclarationPayload::Struct {
                        fields: Arc::from([(Arc::from("count"), DurableType::U64)]),
                        is_copy: false,
                        is_linear: false,
                    },
                },
                &[],
            )
        };
        let first = build();
        let second = build();
        assert_eq!(first, second);
        assert_eq!(stable_hash(&first), stable_hash(&second));
        assert_eq!(
            first.logical_key_via_recipe(),
            second.logical_key_via_recipe()
        );
    }

    impl DefinitionRecipe {
        fn logical_key_via_recipe(&self) -> RecipeLogicalKey {
            DeclarationRecipe::Definition(Box::new(self.clone())).logical_key()
        }
    }
}
