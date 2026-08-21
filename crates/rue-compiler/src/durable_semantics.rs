//! Request-independent semantic values used at compiler query boundaries.
//!
//! These types deliberately have no conversion from `rue_air::Type`. Such a
//! conversion is only sound while the successful declaration binder, its type
//! pool, and the exact-revision stable-definition join are available together.

use std::hash::{Hash, Hasher};
use std::sync::Arc;

use rue_air::{SemanticImportConstValue, SemanticImportType};

use crate::retained_charge::RetainedCharge;
use crate::{ModuleId, StableDefinitionKey};

/// The durable specialization of rue-air's canonical type algebra.
pub type DurableType = SemanticImportType<StableDefinitionKey, ModuleId>;

/// The durable specialization of rue-air's canonical constant algebra.
pub type DurableConstValue = SemanticImportConstValue<StableDefinitionKey, ModuleId>;

/// Query-owned materialization payload for one anonymous nominal referenced by
/// declaration semantics. Identity remains separate from shape so recursive
/// uses and structurally equal aliases can be joined before fields are filled.
#[derive(Debug, Clone)]
pub struct DurableAnonymousNominal {
    pub identity: crate::AnonymousNominalKey,
    /// Canonical compact symbol spelling derived once with the durable fact.
    /// The structured identity remains the semantic authority; this cache only
    /// avoids repeatedly rendering and hashing it in body-local type pools.
    source_symbol: Arc<str>,
    /// Canonical digest retained alongside the source symbol so body-local
    /// pools can use the exact durable computation without relocating the key.
    /// Semantic equality and ordering intentionally ignore it below; hashing
    /// uses it as the bucket key, which stays consistent with equality because
    /// it is a pure function of `identity` and `identity` is part of the
    /// compared tuple.
    anonymous_digest: u128,
    pub shape: DurableAnonymousNominalShape,
    pub type_captures: Arc<[(Arc<str>, DurableType)]>,
    pub value_captures: Arc<[(Arc<str>, DurableConstValue)]>,
}

impl DurableAnonymousNominal {
    fn semantic_parts(
        &self,
    ) -> (
        &crate::AnonymousNominalKey,
        &DurableAnonymousNominalShape,
        &Arc<[(Arc<str>, DurableType)]>,
        &Arc<[(Arc<str>, DurableConstValue)]>,
    ) {
        (
            &self.identity,
            &self.shape,
            &self.type_captures,
            &self.value_captures,
        )
    }

    pub(crate) fn new(
        identity: crate::AnonymousNominalKey,
        shape: DurableAnonymousNominalShape,
        type_captures: Arc<[(Arc<str>, DurableType)]>,
        value_captures: Arc<[(Arc<str>, DurableConstValue)]>,
    ) -> Self {
        let anonymous_digest = crate::semantic_identity::anonymous_nominal_digest(&identity);
        let source_symbol = Arc::from(
            crate::semantic_identity::anonymous_nominal_source_symbol_from_digest(
                &identity,
                anonymous_digest,
            ),
        );
        Self {
            identity,
            source_symbol,
            anonymous_digest,
            shape,
            type_captures,
            value_captures,
        }
    }

    pub(crate) fn with_shape(&self, shape: DurableAnonymousNominalShape) -> Self {
        Self {
            identity: self.identity.clone(),
            source_symbol: self.source_symbol.clone(),
            anonymous_digest: self.anonymous_digest,
            shape,
            type_captures: self.type_captures.clone(),
            value_captures: self.value_captures.clone(),
        }
    }

    pub(crate) fn source_symbol(&self) -> &Arc<str> {
        &self.source_symbol
    }

    pub(crate) fn anonymous_identity_digest(&self) -> u128 {
        self.anonymous_digest
    }
}

// The carried symbol is a cache derived entirely from `identity`, not a new part
// of the durable fact's semantic identity. Keep equality, ordering, and hashing
// identical to the pre-cache representation so query keys do not hash the
// formatted name or invalidate merely because the cache representation changes.
impl PartialEq for DurableAnonymousNominal {
    fn eq(&self, other: &Self) -> bool {
        self.semantic_parts() == other.semantic_parts()
    }
}

impl Eq for DurableAnonymousNominal {}

impl PartialOrd for DurableAnonymousNominal {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for DurableAnonymousNominal {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.semantic_parts().cmp(&other.semantic_parts())
    }
}

impl Hash for DurableAnonymousNominal {
    fn hash<H: Hasher>(&self, state: &mut H) {
        // Hash the construction-time digest instead of walking the identity,
        // shape, and capture slices. Equal values share an identity and
        // therefore a digest, so this stays consistent with `Eq`; values that
        // share an identity but differ in shape (`with_shape` produces them)
        // collide into the same bucket and are separated by the full equality
        // comparison. Per-body fact selection hashes every selected
        // anonymous-nominal fact once, so the deep walk this replaces was paid
        // per selecting body rather than per distinct nominal (RUE-1587).
        self.anonymous_digest.hash(state);
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DurableAnonymousNominalShape {
    Struct {
        fields: Arc<[(Arc<str>, DurableType)]>,
        methods: Arc<[DurableAnonymousMethodSignature]>,
    },
    Enum {
        variants: Arc<[(Arc<str>, Arc<[DurableType]>)]>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DurableAnonymousMethodSignature {
    pub name: Arc<str>,
    pub has_self: bool,
    pub self_mode: DurableParameterMode,
    pub returns_borrow: bool,
    pub returns_inout: bool,
    pub parameters: Arc<[(DurableAnonymousMethodType, DurableParameterMode, bool)]>,
    pub result: DurableAnonymousMethodType,
    /// Whether the producer declared a body for this member. The body itself
    /// stays in the producer's canonical candidate artifact; anonymous-member
    /// transactions select that exact nested declaration by owner anchor,
    /// member name, and member kind instead of retaining or reparsing text.
    pub has_body: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DurableAnonymousMethodType {
    SelfType,
    Concrete(DurableType),
}

/// The canonical durable parameter mode shared with the rue-air consumer.
pub type DurableParameterMode = rue_air::SemanticParameterMode;

/// The durable specialization of rue-air's canonical signature parameter.
///
/// Keeping this as the boundary type lets retained declaration-signature
/// payloads flow into body analysis by sharing their immutable slice instead
/// of allocating and cloning every parameter for every materialization.
pub type DurableSemanticParameter =
    rue_air::DurableSignatureParameter<StableDefinitionKey, ModuleId>;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DurableDeclarationPayload {
    Callable {
        parameters: Arc<[DurableSemanticParameter]>,
        result: DurableType,
        has_self: bool,
        self_mode: DurableParameterMode,
        is_unchecked: bool,
    },
    Struct {
        fields: Arc<[(Arc<str>, DurableType)]>,
        is_copy: bool,
        is_linear: bool,
    },
    Enum {
        variants: Arc<[(Arc<str>, Arc<[DurableType]>)]>,
    },
    Const {
        ty: DurableType,
        value: DurableConstValue,
    },
    /// The resolved canonical target of a top-level module-valued constant.
    ModuleBinding {
        target: ModuleId,
    },
    Destructor,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DurableDeclarationSemantic {
    pub key: StableDefinitionKey,
    pub is_public: bool,
    pub payload: DurableDeclarationPayload,
}

impl RetainedCharge for DurableAnonymousNominal {
    fn retained_charge(&self) -> u64 {
        self.identity
            .retained_charge()
            .saturating_add(self.source_symbol.retained_charge())
            .saturating_add(self.shape.retained_charge())
            .saturating_add(self.type_captures.retained_charge())
            .saturating_add(self.value_captures.retained_charge())
    }
}

impl RetainedCharge for DurableAnonymousNominalShape {
    fn retained_charge(&self) -> u64 {
        match self {
            Self::Struct { fields, methods } => fields
                .retained_charge()
                .saturating_add(methods.retained_charge()),
            Self::Enum { variants } => variants.retained_charge(),
        }
    }
}

impl RetainedCharge for DurableAnonymousMethodSignature {
    fn retained_charge(&self) -> u64 {
        self.name
            .retained_charge()
            .saturating_add(self.parameters.retained_charge())
            .saturating_add(self.result.retained_charge())
    }
}

impl RetainedCharge for DurableAnonymousMethodType {
    fn retained_charge(&self) -> u64 {
        match self {
            Self::SelfType => 0,
            Self::Concrete(ty) => ty.retained_charge(),
        }
    }
}

impl RetainedCharge for DurableParameterMode {
    fn retained_charge(&self) -> u64 {
        0
    }
}

impl RetainedCharge for DurableSemanticParameter {
    fn retained_charge(&self) -> u64 {
        self.name
            .retained_charge()
            .saturating_add(self.ty.retained_charge())
    }
}

impl RetainedCharge for DurableDeclarationPayload {
    fn retained_charge(&self) -> u64 {
        match self {
            Self::Callable {
                parameters, result, ..
            } => parameters
                .retained_charge()
                .saturating_add(result.retained_charge()),
            Self::Struct { fields, .. } => fields.retained_charge(),
            Self::Enum { variants } => variants.retained_charge(),
            Self::Const { ty, value } => {
                ty.retained_charge().saturating_add(value.retained_charge())
            }
            Self::ModuleBinding { target } => target.retained_charge(),
            Self::Destructor => 0,
        }
    }
}

impl RetainedCharge for DurableDeclarationSemantic {
    fn retained_charge(&self) -> u64 {
        self.key
            .retained_charge()
            .saturating_add(self.payload.retained_charge())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::hash_map::DefaultHasher;
    use std::hash::Hasher;

    fn identity() -> crate::AnonymousNominalKey {
        let module = crate::ModuleId::from_logical_path("digest-test.rue").unwrap();
        let definition = crate::StableDefinitionKey::from_stable_parts(
            module,
            crate::StableDefinitionNamespace::Value,
            crate::StableDefinitionKind::Function,
            Arc::from("probe"),
            None,
        );
        crate::AnonymousNominalKey {
            kind: rue_air::AnonymousNominalKind::Struct,
            producer: crate::StableProducerId::Definition(definition),
            anchor: rue_rir::RirStructuralAnchor::new(vec![
                rue_rir::RirStructuralPathSegment::Body,
                rue_rir::RirStructuralPathSegment::AnonymousType(0),
            ]),
            arguments: crate::CanonicalArguments::default(),
        }
    }

    #[test]
    fn cached_digest_matches_canonical_digest_without_changing_semantic_equality() {
        let identity = identity();
        let shape = DurableAnonymousNominalShape::Struct {
            fields: Arc::from([]),
            methods: Arc::from([]),
        };
        let nominal = DurableAnonymousNominal::new(
            identity.clone(),
            shape.clone(),
            Arc::from([]),
            Arc::from([]),
        );
        assert_eq!(
            nominal.anonymous_identity_digest(),
            crate::semantic_identity::anonymous_nominal_digest(&identity)
        );

        // The symbol cache is not a durable semantic fact: a deliberately
        // stale spelling compares exactly like the original.
        let mut stale = nominal.clone();
        stale.source_symbol = Arc::from("stale");
        assert_eq!(nominal, stale);

        // The digest, by contrast, is the hash bucket key. Every constructor
        // derives it from `identity`, so independently constructed equal
        // values hash identically, and a shape variant of the same identity
        // (`with_shape`) shares the bucket while remaining unequal — the
        // collision the full equality comparison exists to separate.
        let rebuilt =
            DurableAnonymousNominal::new(identity.clone(), shape, Arc::from([]), Arc::from([]));
        assert_eq!(nominal, rebuilt);
        let hash_of = |value: &DurableAnonymousNominal| {
            let mut hasher = DefaultHasher::new();
            value.hash(&mut hasher);
            hasher.finish()
        };
        assert_eq!(hash_of(&nominal), hash_of(&rebuilt));
        let variant = nominal.with_shape(DurableAnonymousNominalShape::Enum {
            variants: Arc::from([]),
        });
        assert_ne!(nominal, variant);
        assert_eq!(hash_of(&nominal), hash_of(&variant));
    }
}
