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

/// The durable specialization of a struct-shaped declaration's interface
/// facts (spec 6.7).
pub type DurableConformanceFacts = rue_air::DurableConformanceFacts<StableDefinitionKey, ModuleId>;

/// The durable specialization of one resolved freestanding conformance
/// assertion (`Type is I;`, spec 6.7:9).
pub type DurableConformanceAssertion =
    rue_air::DurableConformanceAssertion<StableDefinitionKey, ModuleId>;

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

    /// Rebuild this fact under the one canonical spelling of its producer.
    /// Empty-specialization aliases and concrete references to the owning type
    /// in method signatures are transport spellings, not distinct anonymous
    /// content at durable merge boundaries.
    pub(crate) fn with_canonical_identity(&self) -> Self {
        let identity = self.identity.with_canonical_producer().into_owned();
        let shape = normalize_anonymous_shape(&identity, &self.shape);
        if identity == self.identity && shape == self.shape {
            return self.clone();
        }
        Self::new(
            identity,
            shape,
            self.type_captures.clone(),
            self.value_captures.clone(),
        )
    }

    pub(crate) fn source_symbol(&self) -> &Arc<str> {
        &self.source_symbol
    }

    pub(crate) fn anonymous_identity_digest(&self) -> u128 {
        self.anonymous_digest
    }
}

fn reconcile_optional_projection<T: Eq>(left: &Arc<[T]>, right: &Arc<[T]>) -> Option<Arc<[T]>> {
    if left == right || right.is_empty() {
        Some(left.clone())
    } else if left.is_empty() {
        Some(right.clone())
    } else {
        None
    }
}

fn normalize_anonymous_method_type(
    owner: &crate::AnonymousNominalKey,
    ty: &DurableAnonymousMethodType,
) -> DurableAnonymousMethodType {
    match ty {
        DurableAnonymousMethodType::Concrete(DurableType::AnonymousNominal(identity))
            if identity.with_canonical_producer().as_ref() == owner =>
        {
            DurableAnonymousMethodType::SelfType
        }
        _ => ty.clone(),
    }
}

fn anonymous_method_type_needs_normalization(
    owner: &crate::AnonymousNominalKey,
    ty: &DurableAnonymousMethodType,
) -> bool {
    matches!(
        ty,
        DurableAnonymousMethodType::Concrete(DurableType::AnonymousNominal(identity))
            if identity.with_canonical_producer().as_ref() == owner
    )
}

fn normalize_anonymous_methods(
    owner: &crate::AnonymousNominalKey,
    methods: &Arc<[DurableAnonymousMethodSignature]>,
) -> Arc<[DurableAnonymousMethodSignature]> {
    if !methods.iter().any(|method| {
        anonymous_method_type_needs_normalization(owner, &method.result)
            || method
                .parameters
                .iter()
                .any(|(ty, _, _)| anonymous_method_type_needs_normalization(owner, ty))
    }) {
        return methods.clone();
    }
    methods
        .iter()
        .map(|method| DurableAnonymousMethodSignature {
            name: method.name.clone(),
            has_self: method.has_self,
            self_mode: method.self_mode,
            returns_borrow: method.returns_borrow,
            returns_inout: method.returns_inout,
            parameters: method
                .parameters
                .iter()
                .map(|(ty, mode, comptime)| {
                    (normalize_anonymous_method_type(owner, ty), *mode, *comptime)
                })
                .collect::<Vec<_>>()
                .into(),
            result: normalize_anonymous_method_type(owner, &method.result),
            has_body: method.has_body,
        })
        .collect::<Vec<_>>()
        .into()
}

fn normalize_anonymous_shape(
    owner: &crate::AnonymousNominalKey,
    shape: &DurableAnonymousNominalShape,
) -> DurableAnonymousNominalShape {
    match shape {
        DurableAnonymousNominalShape::Struct { fields, methods } => {
            DurableAnonymousNominalShape::Struct {
                fields: fields.clone(),
                methods: normalize_anonymous_methods(owner, methods),
            }
        }
        DurableAnonymousNominalShape::Enum { variants } => DurableAnonymousNominalShape::Enum {
            variants: variants.clone(),
        },
    }
}

/// Reconcile two transport projections of one durable anonymous fact.
///
/// Producer evaluation and provider-local body analysis both carry the full
/// materialized shape, but only the projection that needs an anonymous member
/// environment is required to retain capture and method metadata. An empty
/// metadata slice is therefore an admissible thin projection; a non-empty
/// slice enriches it. Method projections may also spell the owner's type as
/// either `SelfType` or the full anonymous identity; those spellings normalize
/// before comparison. The materialized fields/variants must always agree, and
/// two non-empty normalized metadata projections must agree exactly.
pub(crate) fn reconcile_anonymous_nominals(
    left: &DurableAnonymousNominal,
    right: &DurableAnonymousNominal,
) -> Result<DurableAnonymousNominal, crate::AnonymousNominalKey> {
    let left = left.with_canonical_identity();
    let right = right.with_canonical_identity();
    if left.identity != right.identity {
        return Err(right.identity);
    }
    let shape = match (&left.shape, &right.shape) {
        (
            DurableAnonymousNominalShape::Struct {
                fields: left_fields,
                methods: left_methods,
            },
            DurableAnonymousNominalShape::Struct {
                fields: right_fields,
                methods: right_methods,
            },
        ) if left_fields == right_fields => DurableAnonymousNominalShape::Struct {
            fields: left_fields.clone(),
            methods: reconcile_optional_projection(left_methods, right_methods)
                .ok_or_else(|| left.identity.clone())?,
        },
        (
            DurableAnonymousNominalShape::Enum {
                variants: left_variants,
            },
            DurableAnonymousNominalShape::Enum {
                variants: right_variants,
            },
        ) if left_variants == right_variants => DurableAnonymousNominalShape::Enum {
            variants: left_variants.clone(),
        },
        _ => return Err(left.identity),
    };
    let type_captures = reconcile_optional_projection(&left.type_captures, &right.type_captures)
        .ok_or_else(|| left.identity.clone())?;
    let value_captures = reconcile_optional_projection(&left.value_captures, &right.value_captures)
        .ok_or_else(|| left.identity.clone())?;
    Ok(DurableAnonymousNominal::new(
        left.identity,
        shape,
        type_captures,
        value_captures,
    ))
}

/// Insert one durable anonymous fact without permitting last-writer-wins
/// replacement. Exact duplicates and compatible thin/rich projections
/// reconcile; conflicting payloads for the same full canonical identity fail
/// closed.
pub(crate) fn merge_anonymous_nominal(
    values: &mut std::collections::BTreeMap<crate::AnonymousNominalKey, DurableAnonymousNominal>,
    nominal: &DurableAnonymousNominal,
) -> Result<(), crate::AnonymousNominalKey> {
    let nominal = nominal.with_canonical_identity();
    match values.entry(nominal.identity.clone()) {
        std::collections::btree_map::Entry::Vacant(entry) => {
            entry.insert(nominal);
            Ok(())
        }
        std::collections::btree_map::Entry::Occupied(mut entry) => {
            let reconciled = reconcile_anonymous_nominals(entry.get(), &nominal)?;
            if entry.get() != &reconciled {
                entry.insert(reconciled);
            }
            Ok(())
        }
    }
}

/// Insert a complete producer publication. Unlike downstream projections,
/// every producer export carries all method and capture metadata, so only an
/// exact duplicate may share its full canonical identity.
pub(crate) fn merge_complete_anonymous_nominal(
    values: &mut std::collections::BTreeMap<crate::AnonymousNominalKey, DurableAnonymousNominal>,
    nominal: &DurableAnonymousNominal,
) -> Result<(), crate::AnonymousNominalKey> {
    let nominal = nominal.with_canonical_identity();
    match values.entry(nominal.identity.clone()) {
        std::collections::btree_map::Entry::Vacant(entry) => {
            entry.insert(nominal);
            Ok(())
        }
        std::collections::btree_map::Entry::Occupied(entry) if entry.get() == &nominal => Ok(()),
        std::collections::btree_map::Entry::Occupied(entry) => Err(entry.key().clone()),
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
        conformance: DurableConformanceFacts,
    },
    Enum {
        variants: Arc<[(Arc<str>, Arc<[DurableType]>)]>,
        is_non_exhaustive: bool,
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
            Self::Enum { variants, .. } => variants.retained_charge(),
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
            .saturating_add(self.bounds.retained_charge())
    }
}

impl RetainedCharge for rue_air::DurableConformance<StableDefinitionKey> {
    fn retained_charge(&self) -> u64 {
        self.interface.retained_charge()
    }
}

impl RetainedCharge for DurableConformanceFacts {
    fn retained_charge(&self) -> u64 {
        self.conformances
            .retained_charge()
            .saturating_add(self.assoc_types.retained_charge())
            .saturating_add(self.requirements.retained_charge())
    }
}

impl RetainedCharge for DurableConformanceAssertion {
    fn retained_charge(&self) -> u64 {
        self.subject
            .retained_charge()
            .saturating_add(self.interfaces.retained_charge())
            .saturating_add(self.module.retained_charge())
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
            Self::Struct {
                fields,
                conformance,
                ..
            } => fields
                .retained_charge()
                .saturating_add(conformance.retained_charge()),
            Self::Enum { variants, .. } => variants.retained_charge(),
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

    #[test]
    fn anonymous_merge_reconciles_only_explicit_thin_projection_metadata() {
        let identity = identity();
        let fields: Arc<[(Arc<str>, DurableType)]> =
            Arc::from([(Arc::from("value"), DurableType::I32)]);
        let thin = DurableAnonymousNominal::new(
            identity.clone(),
            DurableAnonymousNominalShape::Struct {
                fields: fields.clone(),
                methods: Arc::from([]),
            },
            Arc::from([]),
            Arc::from([]),
        );
        let method = DurableAnonymousMethodSignature {
            name: Arc::from("get"),
            has_self: true,
            self_mode: DurableParameterMode::Value,
            returns_borrow: false,
            returns_inout: false,
            parameters: Arc::from([]),
            result: DurableAnonymousMethodType::Concrete(DurableType::I32),
            has_body: true,
        };
        let rich = DurableAnonymousNominal::new(
            identity.clone(),
            DurableAnonymousNominalShape::Struct {
                fields,
                methods: Arc::from([method]),
            },
            Arc::from([(Arc::from("T"), DurableType::I32)]),
            Arc::from([]),
        );
        for pair in [[&thin, &rich], [&rich, &thin]] {
            let mut merged = std::collections::BTreeMap::new();
            merge_anonymous_nominal(&mut merged, pair[0]).unwrap();
            merge_anonymous_nominal(&mut merged, pair[1]).unwrap();
            assert_eq!(merged.get(&identity), Some(&rich));
        }

        let conflicting_captures = DurableAnonymousNominal::new(
            identity.clone(),
            rich.shape.clone(),
            Arc::from([(Arc::from("T"), DurableType::I64)]),
            Arc::from([]),
        );
        let mut merged = std::collections::BTreeMap::new();
        merge_anonymous_nominal(&mut merged, &rich).unwrap();
        assert_eq!(
            merge_anonymous_nominal(&mut merged, &conflicting_captures),
            Err(identity)
        );
    }

    #[test]
    fn anonymous_merge_normalizes_self_method_type_spelling_in_both_orders() {
        let identity = identity();
        let method = |result| DurableAnonymousMethodSignature {
            name: Arc::from("make"),
            has_self: false,
            self_mode: DurableParameterMode::Value,
            returns_borrow: false,
            returns_inout: false,
            parameters: Arc::from([]),
            result,
            has_body: true,
        };
        let nominal = |result| {
            DurableAnonymousNominal::new(
                identity.clone(),
                DurableAnonymousNominalShape::Struct {
                    fields: Arc::from([]),
                    methods: Arc::from([method(result)]),
                },
                Arc::from([]),
                Arc::from([]),
            )
        };
        let self_spelled = nominal(DurableAnonymousMethodType::SelfType);
        let concrete_spelled = nominal(DurableAnonymousMethodType::Concrete(
            DurableType::AnonymousNominal(identity.clone()),
        ));

        for pair in [
            [&self_spelled, &concrete_spelled],
            [&concrete_spelled, &self_spelled],
        ] {
            let mut merged = std::collections::BTreeMap::new();
            merge_anonymous_nominal(&mut merged, pair[0]).unwrap();
            merge_anonymous_nominal(&mut merged, pair[1]).unwrap();
            assert_eq!(merged.get(&identity), Some(&self_spelled));

            let mut complete = std::collections::BTreeMap::new();
            merge_complete_anonymous_nominal(&mut complete, pair[0]).unwrap();
            merge_complete_anonymous_nominal(&mut complete, pair[1]).unwrap();
            assert_eq!(complete.get(&identity), Some(&self_spelled));
        }
    }
}
