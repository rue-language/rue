//! Bounded compatibility matrix for every durable semantic schema family.
//!
//! The literal version tuple and exhaustive kind sets are deliberate review
//! gates: any schema bump or newly introduced canonical variant must update
//! this matrix in the same change.

use std::{collections::BTreeSet, sync::Arc};

use rue_air::{
    AirArgMode, RuntimeCallKind, SEMANTIC_BODY_INST_KINDS, SEMANTIC_IMPORT_CONST_KINDS,
    SEMANTIC_IMPORT_TYPE_KINDS, STABLE_DEFINITION_KINDS, STABLE_DEFINITION_NAMESPACES,
    SemanticBodyCallArg, SemanticBodyInstData as D, SemanticBodyMatchArm, SemanticBodyPattern,
    SemanticImportConstValue as C, SemanticImportNominalKind, SemanticImportType as T,
    SemanticSpecializationIdentity,
};
use rue_span::FileId;

use crate::*;

const REVIEWED_SEMANTIC_SCHEMA: DurableSemanticSchemaVersion = DurableSemanticSchemaVersion {
    major: 1,
    minor: 0,
    // Epoch 6: slice identity includes the resolved element type.
    implementation_epoch: 6,
};
// 10/9: the body algebra gained the recorded method-reference payload
// (RUE-1128); the payload-free 9/8 shapes fail closed to ordinary analysis.
const REVIEWED_ORDINARY_BODY_SCHEMA: u32 = 10;
const REVIEWED_SPECIALIZED_BODY_SCHEMA: u32 = 9;
const REVIEWED_CFG_SCHEMA: u32 = 4;
const REVIEWED_BODY_KINDS: usize = 58;
const REVIEWED_TYPE_KINDS: usize = 21;
const REVIEWED_CONST_KINDS: usize = 6;
const REVIEWED_DECLARATION_PAYLOAD_KINDS: usize = 6;
const REVIEWED_DEFINITION_KINDS: usize = 8;
const REVIEWED_DEFINITION_NAMESPACES: usize = 4;

fn module(path: &str) -> ModuleId {
    ModuleId::from_logical_path(path).unwrap()
}

fn key(
    kind: StableDefinitionKind,
    namespace: StableDefinitionNamespace,
    name: &str,
) -> StableDefinitionKey {
    StableDefinitionKey::for_test(module("pkg/main.rue"), namespace, kind, name, None)
}

fn owner() -> StableDefinitionKey {
    key(
        StableDefinitionKind::Function,
        StableDefinitionNamespace::Value,
        "main",
    )
}

fn fingerprint(owner: StableDefinitionKey) -> StableDefinitionInputFingerprint {
    StableDefinitionInputFingerprint {
        schema_version: 1,
        key: owner,
        declaration: StableDefinitionFingerprint::for_test(1),
        signature: StableDefinitionFingerprint::for_test(2),
        body_or_initializer: Some(StableDefinitionFingerprint::for_test(3)),
        precision: StableDefinitionFingerprintPrecision::SignatureAndBody,
    }
}

fn body_payload(data: D<StableDefinitionKey, ModuleId>) -> DurableOrdinaryBodyPayload {
    let owner = owner();
    DurableOrdinaryBodyPayload {
        schema_version: DURABLE_ORDINARY_BODY_SCHEMA_VERSION,
        semantic_schema_version: DURABLE_SEMANTIC_SCHEMA_VERSION,
        owner: owner.clone(),
        expected_inputs: fingerprint(owner),
        body: rue_air::SemanticBody {
            return_type: T::Unit,
            instructions: Arc::from([rue_air::SemanticBodyInst {
                data,
                ty: T::Unit,
                anchor: rue_air::SemanticBodyAnchor { start: 0, end: 0 },
            }]),
            places: Arc::from([]),
            strings: Arc::from([]),
            local_atoms: Arc::from([]),
            param_drops: Arc::from([]),
            borrow_slots: Arc::from([]),
            num_locals: 0,
            num_param_slots: 0,
            param_by_ref: Arc::from([]),
            param_writable: Arc::from([]),
            allow_unreachable_code: false,
            warnings: Arc::from([]),
            method_references: Arc::from([]),
        },
    }
}

#[test]
fn reviewed_schema_versions_and_old_new_policy_are_explicit() {
    assert_eq!(DURABLE_SEMANTIC_SCHEMA_VERSION, REVIEWED_SEMANTIC_SCHEMA);
    assert_eq!(
        DURABLE_ORDINARY_BODY_SCHEMA_VERSION,
        REVIEWED_ORDINARY_BODY_SCHEMA
    );
    assert_eq!(
        DURABLE_SPECIALIZED_BODY_SCHEMA_VERSION,
        REVIEWED_SPECIALIZED_BODY_SCHEMA
    );
    assert_eq!(
        crate::queries::DURABLE_CFG_SCHEMA_VERSION,
        REVIEWED_CFG_SCHEMA
    );
    assert_eq!(SEMANTIC_BODY_INST_KINDS.len(), REVIEWED_BODY_KINDS);
    assert_eq!(SEMANTIC_IMPORT_TYPE_KINDS.len(), REVIEWED_TYPE_KINDS);
    assert_eq!(SEMANTIC_IMPORT_CONST_KINDS.len(), REVIEWED_CONST_KINDS);
    assert_eq!(STABLE_DEFINITION_KINDS.len(), REVIEWED_DEFINITION_KINDS);
    assert_eq!(
        STABLE_DEFINITION_NAMESPACES.len(),
        REVIEWED_DEFINITION_NAMESPACES
    );

    let current = DURABLE_SEMANTIC_SCHEMA_VERSION;
    assert!(current.accepts(current));
    for candidate in [
        DurableSemanticSchemaVersion {
            major: current.major.saturating_sub(1),
            ..current
        },
        DurableSemanticSchemaVersion {
            major: current.major + 1,
            ..current
        },
        DurableSemanticSchemaVersion {
            minor: current.minor + 1,
            ..current
        },
        DurableSemanticSchemaVersion {
            implementation_epoch: current.implementation_epoch.saturating_sub(1),
            ..current
        },
        DurableSemanticSchemaVersion {
            implementation_epoch: current.implementation_epoch + 1,
            ..current
        },
    ] {
        assert!(
            !current.accepts(candidate),
            "accepted unknown schema {candidate:?}"
        );
    }

    for version in [
        DURABLE_ORDINARY_BODY_SCHEMA_VERSION - 1,
        DURABLE_ORDINARY_BODY_SCHEMA_VERSION + 1,
        u32::MAX,
    ] {
        let mut payload = body_payload(D::UnitConst);
        payload.schema_version = version;
        let mut work = DurableBodyWork::default();
        assert_eq!(
            payload.project_semantic_body(&mut work),
            Err(DurableBodyProjectionFailure::SchemaVersionMismatch)
        );
        assert_eq!(work.projection_failures, 1);
        assert_eq!(work.projection_completions, 0);
    }
}

#[test]
fn pre_method_reference_payload_bodies_fail_closed_to_ordinary_analysis() {
    // A retained candidate from the payload-free shape (schema 9, before the
    // recorded method-reference payload landed) must never project into the
    // current shape with a silently empty recorded reference set; the stale
    // schema is rejected wholesale so the body falls back to ordinary
    // analysis.
    let mut payload = body_payload(D::UnitConst);
    payload.schema_version = 9;
    let mut work = DurableBodyWork::default();
    assert_eq!(
        payload.project_semantic_body(&mut work),
        Err(DurableBodyProjectionFailure::SchemaVersionMismatch)
    );
    assert_eq!(work.projection_attempts, 1);
    assert_eq!(work.projection_failures, 1);
    assert_eq!(work.projection_completions, 0);
}

#[test]
fn every_type_const_and_specialization_identity_round_trips_same_version() {
    let structure = key(
        StableDefinitionKind::Struct,
        StableDefinitionNamespace::Type,
        "Record",
    );
    let function = key(
        StableDefinitionKind::Function,
        StableDefinitionNamespace::Value,
        "helper",
    );
    let dependency = module("pkg/dependency.rue");
    let types = vec![
        T::I8,
        T::I16,
        T::I32,
        T::I64,
        T::U8,
        T::U16,
        T::U32,
        T::U64,
        T::Bool,
        T::Unit,
        T::Never,
        T::ComptimeType,
        T::BuiltinNominal {
            name: Arc::from("str"),
            kind: SemanticImportNominalKind::Struct,
        },
        T::Nominal(structure.clone()),
        T::Array {
            element: Box::new(T::I32),
            len: 4,
        },
        T::Slice {
            element: Box::new(T::I32),
            name: Arc::from("[i32]"),
        },
        T::PtrConst(Box::new(T::I32)),
        T::PtrMut(Box::new(T::I32)),
        T::Module(dependency.clone()),
        T::GenericParameter(0),
        T::AnonymousNominal(rue_air::AnonymousNominalKey {
            kind: rue_air::AnonymousNominalKind::Struct,
            producer: rue_air::StableProducerId::Function(Box::new(
                rue_air::FunctionInstanceKey::Specialization {
                    base: Box::new(rue_air::FunctionInstanceKey::Definition(function.clone())),
                    arguments: rue_air::CanonicalArguments {
                        types: Arc::from([rue_air::TypeInstanceKey::Nominal(
                            rue_air::NominalInstanceKey::Named(structure.clone()),
                        )]),
                        values: Arc::from([rue_air::CanonicalArgumentValue::Type(Box::new(
                            rue_air::TypeInstanceKey::Module(dependency.clone()),
                        ))]),
                    },
                },
            )),
            anchor: rue_rir::RirStructuralAnchor::new(vec![
                rue_rir::RirStructuralPathSegment::Body,
                rue_rir::RirStructuralPathSegment::AnonymousType(0),
            ]),
            arguments: rue_air::CanonicalArguments::default(),
        }),
    ];
    assert_eq!(
        types.iter().map(T::kind).collect::<BTreeSet<_>>(),
        SEMANTIC_IMPORT_TYPE_KINDS.iter().copied().collect()
    );

    for value in &types {
        let relocated = value
            .try_map_identities(&|key| Ok::<_, ()>(key.clone()), &|module| {
                Ok::<_, ()>(Arc::<str>::from(module.as_str()))
            })
            .unwrap();
        let restored = relocated
            .try_map_identities(&|key| Ok::<_, ()>(key.clone()), &|path| {
                Ok::<_, ()>(ModuleId::from_logical_path(path).unwrap())
            })
            .unwrap();
        assert_eq!(&restored, value);
    }

    let consts = vec![
        C::Integer(i128::MAX),
        C::Bool(true),
        C::Type(types[14].clone()),
        C::Function(function.clone()),
        C::Unit,
        C::String(Arc::from("hello")),
    ];
    assert_eq!(
        consts.iter().map(C::kind).collect::<BTreeSet<_>>(),
        SEMANTIC_IMPORT_CONST_KINDS.iter().copied().collect()
    );
    let identity = SemanticSpecializationIdentity {
        base: function,
        type_arguments: types.into(),
        value_arguments: consts.into(),
    };
    let relocated = identity
        .try_map_keys(&|key| Ok::<_, ()>(key.clone()), &|module| {
            Ok::<_, ()>(Arc::<str>::from(module.as_str()))
        })
        .unwrap();
    let restored = relocated
        .try_map_keys(&|key| Ok::<_, ()>(key.clone()), &|path| {
            Ok::<_, ()>(ModuleId::from_logical_path(path).unwrap())
        })
        .unwrap();
    assert_eq!(restored, identity);
}

#[test]
fn every_declaration_payload_and_parameter_mode_imports_in_one_bounded_epoch() {
    let structure = key(
        StableDefinitionKind::Struct,
        StableDefinitionNamespace::Type,
        "Record",
    );
    let enumeration = key(
        StableDefinitionKind::Enum,
        StableDefinitionNamespace::Type,
        "Choice",
    );
    let function = key(
        StableDefinitionKind::Function,
        StableDefinitionNamespace::Value,
        "compute",
    );
    let constant = key(
        StableDefinitionKind::ValueConst,
        StableDefinitionNamespace::Value,
        "ANSWER",
    );
    let module_binding = key(
        StableDefinitionKind::ModuleBinding,
        StableDefinitionNamespace::Value,
        "support",
    );
    let destructor = key(
        StableDefinitionKind::Destructor,
        StableDefinitionNamespace::Destructor,
        "Record",
    );
    let method = StableDefinitionKey::for_test(
        module("pkg/main.rue"),
        StableDefinitionNamespace::Method,
        StableDefinitionKind::Method,
        "read",
        Some((StableDefinitionKind::Struct, Arc::from("Record"))),
    );
    let associated = StableDefinitionKey::for_test(
        module("pkg/main.rue"),
        StableDefinitionNamespace::Method,
        StableDefinitionKind::AssociatedFunction,
        "new",
        Some((StableDefinitionKind::Struct, Arc::from("Record"))),
    );
    let parameters = [
        (DurableParameterMode::Value, false),
        (DurableParameterMode::Borrow, false),
        (DurableParameterMode::Inout, true),
    ]
    .into_iter()
    .map(|(mode, is_comptime)| DurableSemanticParameter {
        name: Arc::from("param"),
        ty: T::PtrConst(Box::new(T::Nominal(structure.clone()))),
        mode,
        is_comptime,
    })
    .collect::<Vec<_>>();
    let declarations = vec![
        DurableDeclarationSemantic {
            key: function.clone(),
            is_public: true,
            payload: DurableDeclarationPayload::Callable {
                parameters: parameters.into(),
                result: T::Nominal(enumeration.clone()),
                has_self: false,
                self_mode: DurableParameterMode::Value,
                is_unchecked: false,
            },
        },
        DurableDeclarationSemantic {
            key: structure.clone(),
            is_public: true,
            payload: DurableDeclarationPayload::Struct {
                fields: Arc::from([(Arc::from("value"), T::I32)]),
                is_copy: false,
                is_linear: false,
            },
        },
        DurableDeclarationSemantic {
            key: enumeration,
            is_public: false,
            payload: DurableDeclarationPayload::Enum {
                variants: Arc::from([(Arc::from("Some"), Arc::from([T::I32]))]),
            },
        },
        DurableDeclarationSemantic {
            key: constant,
            is_public: false,
            payload: DurableDeclarationPayload::Const {
                ty: T::I32,
                value: C::Integer(42),
            },
        },
        DurableDeclarationSemantic {
            key: module_binding,
            is_public: false,
            payload: DurableDeclarationPayload::ModuleBinding {
                target: module("pkg/support.rue"),
            },
        },
        DurableDeclarationSemantic {
            key: destructor,
            is_public: false,
            payload: DurableDeclarationPayload::Destructor,
        },
        DurableDeclarationSemantic {
            key: method,
            is_public: false,
            payload: DurableDeclarationPayload::Callable {
                parameters: Arc::from([]),
                result: T::I32,
                has_self: true,
                self_mode: DurableParameterMode::Borrow,
                is_unchecked: false,
            },
        },
        DurableDeclarationSemantic {
            key: associated,
            is_public: false,
            payload: DurableDeclarationPayload::Callable {
                parameters: Arc::from([]),
                result: T::Nominal(structure.clone()),
                has_self: false,
                self_mode: DurableParameterMode::Value,
                is_unchecked: false,
            },
        },
    ];
    let payload_kinds = declarations
        .iter()
        .map(|declaration| match declaration.payload {
            DurableDeclarationPayload::Callable { .. } => "callable",
            DurableDeclarationPayload::Struct { .. } => "struct",
            DurableDeclarationPayload::Enum { .. } => "enum",
            DurableDeclarationPayload::Const { .. } => "const",
            DurableDeclarationPayload::ModuleBinding { .. } => "module_binding",
            DurableDeclarationPayload::Destructor => "destructor",
        })
        .collect::<BTreeSet<_>>();
    assert_eq!(payload_kinds.len(), REVIEWED_DECLARATION_PAYLOAD_KINDS);
    let identity_kinds = [
        (
            StableDefinitionKind::Function,
            StableDefinitionNamespace::Value,
        ),
        (
            StableDefinitionKind::Struct,
            StableDefinitionNamespace::Type,
        ),
        (StableDefinitionKind::Enum, StableDefinitionNamespace::Type),
        (
            StableDefinitionKind::ValueConst,
            StableDefinitionNamespace::Value,
        ),
        (
            StableDefinitionKind::ModuleBinding,
            StableDefinitionNamespace::Value,
        ),
        (
            StableDefinitionKind::Destructor,
            StableDefinitionNamespace::Destructor,
        ),
        (
            StableDefinitionKind::Method,
            StableDefinitionNamespace::Method,
        ),
        (
            StableDefinitionKind::AssociatedFunction,
            StableDefinitionNamespace::Method,
        ),
    ];
    assert_eq!(identity_kinds.len(), REVIEWED_DEFINITION_KINDS);
    assert_eq!(
        identity_kinds
            .iter()
            .map(|(kind, _)| *kind)
            .collect::<BTreeSet<_>>(),
        STABLE_DEFINITION_KINDS.iter().copied().collect()
    );
    assert_eq!(
        identity_kinds
            .iter()
            .map(|(_, namespace)| *namespace)
            .collect::<BTreeSet<_>>(),
        STABLE_DEFINITION_NAMESPACES.iter().copied().collect()
    );
    for (kind, namespace) in identity_kinds {
        assert_eq!(kind.namespace(), namespace);
    }
    import_durable_declaration_semantics(&declarations).unwrap();
}

#[test]
fn module_binding_declaration_import_is_explicitly_unsupported_and_fails_closed() {
    let module_binding = DurableDeclarationSemantic {
        key: key(
            StableDefinitionKind::ModuleBinding,
            StableDefinitionNamespace::Value,
            "dependency",
        ),
        is_public: true,
        payload: DurableDeclarationPayload::Const {
            ty: T::Module(module("pkg/dependency.rue")),
            value: C::Unit,
        },
    };
    assert!(
        matches!(
            import_durable_declaration_semantics(&[module_binding]),
            Err(rue_air::SemanticImportFailure::DeclarationKindMismatch)
        ),
        "module bindings have stable dependency keys but no durable declaration payload; a const-shaped payload must fail closed"
    );
}

#[test]
fn valid_source_order_permutations_remain_distinct_and_projectable() {
    let structure = key(
        StableDefinitionKind::Struct,
        StableDefinitionNamespace::Type,
        "Pair",
    );
    let payload = |source_order: Arc<[u32]>| {
        let mut payload = body_payload(D::UnitConst);
        payload.body.instructions = Arc::from([
            rue_air::SemanticBodyInst {
                data: D::UnitConst,
                ty: T::Unit,
                anchor: rue_air::SemanticBodyAnchor { start: 0, end: 0 },
            },
            rue_air::SemanticBodyInst {
                data: D::UnitConst,
                ty: T::Unit,
                anchor: rue_air::SemanticBodyAnchor { start: 0, end: 0 },
            },
            rue_air::SemanticBodyInst {
                data: D::StructInit {
                    struct_key: rue_air::NominalInstanceKey::Named(structure.clone()),
                    fields: Arc::from([0, 1]),
                    source_order,
                },
                ty: T::Nominal(structure.clone()),
                anchor: rue_air::SemanticBodyAnchor { start: 0, end: 0 },
            },
        ]);
        payload
    };
    let declaration_order = payload(Arc::from([0, 1]));
    let source_order = payload(Arc::from([1, 0]));
    for candidate in [&declaration_order, &source_order] {
        let mut work = DurableBodyWork::default();
        assert!(candidate.project_semantic_body(&mut work).is_ok());
        assert_eq!(work.projection_completions, 1);
        assert_eq!(work.projection_failures, 0);
    }
    assert_ne!(declaration_order.body, source_order.body);
}

#[test]
fn every_body_instruction_variant_preserves_kind_and_identity_on_roundtrip() {
    let function = key(
        StableDefinitionKind::Function,
        StableDefinitionNamespace::Value,
        "helper",
    );
    let structure = key(
        StableDefinitionKind::Struct,
        StableDefinitionNamespace::Type,
        "Record",
    );
    let enumeration = key(
        StableDefinitionKind::Enum,
        StableDefinitionNamespace::Type,
        "Choice",
    );
    let args: Arc<[SemanticBodyCallArg]> = Arc::from([SemanticBodyCallArg {
        value: 0,
        mode: AirArgMode::Borrow,
    }]);
    let identity = SemanticSpecializationIdentity {
        base: function.clone(),
        type_arguments: Arc::from([T::Module(module("pkg/dependency.rue"))]),
        value_arguments: Arc::from([C::Function(function.clone())]),
    };
    let mut values = vec![
        D::Const(1),
        D::BoolConst(true),
        D::StringConst(0),
        D::UnitConst,
        D::TypeConst(T::Nominal(structure.clone())),
        D::Add(0, 0),
        D::Sub(0, 0),
        D::Mul(0, 0),
        D::WrappingAdd(0, 0),
        D::WrappingSub(0, 0),
        D::WrappingMul(0, 0),
        D::Div(0, 0),
        D::Mod(0, 0),
        D::Eq(0, 0),
        D::Ne(0, 0),
        D::Lt(0, 0),
        D::Gt(0, 0),
        D::Le(0, 0),
        D::Ge(0, 0),
        D::And(0, 0),
        D::Or(0, 0),
        D::BitAnd(0, 0),
        D::BitOr(0, 0),
        D::BitXor(0, 0),
        D::Shl(0, 0),
        D::Shr(0, 0),
        D::Neg(0),
        D::Not(0),
        D::BitNot(0),
        D::Branch {
            cond: 0,
            then_value: 0,
            else_value: Some(0),
        },
        D::Loop { cond: 0, body: 0 },
        D::InfiniteLoop { body: 0 },
        D::Match {
            scrutinee: 0,
            arms: Arc::from([SemanticBodyMatchArm {
                pattern: SemanticBodyPattern::EnumVariant {
                    enum_key: rue_air::NominalInstanceKey::Named(enumeration.clone()),
                    variant_index: 0,
                },
                body: 0,
            }]),
        },
        D::Break,
        D::Continue,
        D::Alloc { slot: 0, init: 0 },
        D::Load { slot: 0 },
        D::Store { slot: 0, value: 0 },
        D::ParamStore {
            param_slot: 0,
            value: 0,
        },
        D::Ret(Some(0)),
        D::Call {
            function: rue_air::FunctionInstanceKey::Definition(function.clone()),
            args: args.clone(),
        },
        D::RuntimeCall {
            runtime: RuntimeCallKind::ToString,
            args: args.clone(),
        },
        D::CallSpecialized {
            identity,
            args: args.clone(),
        },
        D::CallGeneric,
        D::Intrinsic {
            runtime: None,
            name: Arc::from("test"),
            args: args.clone(),
        },
        D::Param { index: 0 },
        D::Block {
            statements: Arc::from([0]),
            value: 0,
        },
        D::StructInit {
            struct_key: rue_air::NominalInstanceKey::Named(structure.clone()),
            fields: Arc::from([0]),
            source_order: Arc::from([0]),
        },
        D::ArrayInit {
            elements: Arc::from([0]),
        },
        D::PlaceRead { place: 0 },
        D::PlaceWrite { place: 0, value: 0 },
        D::EnumVariant {
            enum_key: rue_air::NominalInstanceKey::Named(enumeration.clone()),
            variant_index: 0,
            payload: Arc::from([0]),
        },
        D::EnumPayloadGet {
            base: 0,
            enum_key: rue_air::NominalInstanceKey::Named(enumeration),
            variant_index: 0,
            field_index: 0,
        },
        D::IntCast {
            value: 0,
            from_ty: T::I32,
        },
        D::Drop { value: 0 },
        D::StorageLive { slot: 0 },
        D::StorageDead { slot: 0 },
        D::MarkMoved {
            value: 0,
            slot: 0,
            is_param: false,
            place: Some(0),
        },
    ];
    values.sort_by_key(D::kind);
    assert_eq!(
        values.iter().map(D::kind).collect::<Vec<_>>(),
        SEMANTIC_BODY_INST_KINDS
    );
    for value in values {
        let kind = value.kind();
        let relocated = value
            .try_map_keys(&|key| Ok::<_, ()>(key.clone()), &|module| {
                Ok::<_, ()>(Arc::<str>::from(module.as_str()))
            })
            .unwrap();
        let restored = relocated
            .try_map_keys(&|key| Ok::<_, ()>(key.clone()), &|path| {
                Ok::<_, ()>(ModuleId::from_logical_path(path).unwrap())
            })
            .unwrap();
        assert_eq!(restored.kind(), kind);
        assert_eq!(restored, value);
    }
}

#[test]
fn corrupt_truncated_unknown_and_unsupported_bodies_fail_closed_individually() {
    let cases = [
        (
            D::Add(0, 0),
            DurableBodyProjectionFailure::ForwardInstructionReference,
        ),
        (
            D::StringConst(0),
            DurableBodyProjectionFailure::InvalidStringReference,
        ),
        (
            D::PlaceRead { place: 0 },
            DurableBodyProjectionFailure::InvalidPlaceReference,
        ),
        (
            D::CallGeneric,
            DurableBodyProjectionFailure::InvalidInstructionReference,
        ),
        (
            D::StructInit {
                struct_key: rue_air::NominalInstanceKey::Named(key(
                    StableDefinitionKind::Struct,
                    StableDefinitionNamespace::Type,
                    "S",
                )),
                fields: Arc::from([]),
                source_order: Arc::from([0]),
            },
            DurableBodyProjectionFailure::InvalidSourceOrder,
        ),
    ];
    for (data, failure) in cases {
        let payload = body_payload(data);
        let mut work = DurableBodyWork::default();
        assert_eq!(payload.project_semantic_body(&mut work), Err(failure));
        assert_eq!(work.projection_attempts, 1);
        assert_eq!(work.projection_failures, 1);
        assert_eq!(work.projection_completions, 0);
    }

    let mut truncated = body_payload(D::UnitConst);
    truncated.body.num_param_slots = 1;
    let mut work = DurableBodyWork::default();
    assert_eq!(
        truncated.project_semantic_body(&mut work),
        Err(DurableBodyProjectionFailure::InvalidParameterModes)
    );

    let mut corrupt_anchor = body_payload(D::UnitConst);
    Arc::make_mut(&mut corrupt_anchor.body.instructions)[0].anchor =
        rue_air::SemanticBodyAnchor { start: 2, end: 1 };
    let mut work = DurableBodyWork::default();
    assert_eq!(
        corrupt_anchor.project_semantic_body(&mut work),
        Err(DurableBodyProjectionFailure::InvalidAnchor)
    );
}

fn snapshot(entries: &[(u32, &str, &str, &str)], root: u32) -> SourceSnapshot {
    let physical = entries
        .iter()
        .map(|(id, path, _, _)| (FileId::new(*id), (*path).to_owned()))
        .collect();
    let logical = entries
        .iter()
        .map(|(id, _, path, _)| (FileId::new(*id), (*path).to_owned()))
        .collect();
    SourceSnapshot::new(
        SourceMetadata::new(FileId::new(root), physical, logical).unwrap(),
        entries
            .iter()
            .map(|(id, _, _, source)| (FileId::new(*id), Arc::new((*source).to_owned())))
            .collect(),
    )
    .unwrap()
}

#[test]
fn relocation_file_ids_and_input_order_preserve_same_version_body_values() {
    let original = snapshot(
        &[
            (
                91,
                "/old/main.rue",
                "main.rue",
                "fn left() -> i32 { 1 } fn right() -> i32 { 2 } fn main() -> i32 { left() + right() }",
            ),
            (92, "/old/dead.rue", "dead.rue", "fn dead() -> i32 { 0 }"),
        ],
        91,
    );
    let relocated = snapshot(
        &[
            (4, "/new/dead.rue", "dead.rue", "fn dead() -> i32 { 0 }"),
            (
                7,
                "/new/main.rue",
                "main.rue",
                "fn left() -> i32 { 1 } fn right() -> i32 { 2 } fn main() -> i32 { left() + right() }",
            ),
        ],
        7,
    );
    let options = CompileOptions::default();
    let mut warm = CompilerSession::new();
    warm.update(&original).into_result().unwrap();
    warm.canonical_semantic(&options).unwrap();
    warm.update(&relocated).into_result().unwrap();
    let reused = warm.canonical_semantic(&options).unwrap();
    assert_eq!(reused.work().durable_bodies.reused_bodies, 0);
    assert_eq!(reused.work().durable_bodies.candidate_fallbacks, 0);
    assert_eq!(reused.work().body_analysis.bodies_attempted, 0);
    assert_eq!(reused.work().body_analysis.ordinary_body_import_attempts, 3);
    assert_eq!(
        reused.work().body_analysis.ordinary_body_import_successes,
        3
    );

    let mut cold = CompilerSession::new();
    cold.update(&relocated).into_result().unwrap();
    let fresh = cold.canonical_semantic(&options).unwrap();
    assert_eq!(
        format!("{:?}", reused.functions()),
        format!("{:?}", fresh.functions())
    );
    assert_eq!(reused.strings(), fresh.strings());
    assert_eq!(reused.type_pool().stats(), fresh.type_pool().stats());
}

#[test]
fn representative_body_families_project_and_import_through_production_reuse() {
    let cases = [
        (
            "scalar-control-call",
            "fn helper(x: i32) -> i32 { if x > 0 { x + 1 } else { x - 1 } }\n\
             fn main() -> i32 { helper(41) }",
        ),
        (
            "aggregate-place-enum-string",
            r#"
                struct Pair { x: i32, values: [i32; 2] }
                enum Choice { Value(i32), Empty }
                fn mutate(inout p: Pair) { p.x = 20; p.values[0] = 2; }
                fn read(borrow p: Pair) -> i32 { p.x + p.values[0] }
                fn main() -> i32 {
                    let mut p = Pair { x: 1, values: [3, 4] };
                    mutate(inout p);
                    let choice = Choice.Value(read(borrow p));
                    @dbg("compatibility");
                    match choice { Choice.Value(v) => v, Choice.Empty => 0 }
                }
            "#,
        ),
    ];
    let options = CompileOptions::default();

    for (name, source) in cases {
        let original = snapshot(&[(71, "/old/main.rue", "main.rue", source)], 71);
        let relocated = snapshot(&[(3, "/new/main.rue", "main.rue", source)], 3);
        let mut warm = CompilerSession::new();
        warm.update(&original).into_result().unwrap();
        warm.canonical_semantic(&options).unwrap();
        warm.update(&relocated).into_result().unwrap();
        let reused = warm.canonical_semantic(&options).unwrap();

        let mut cold = CompilerSession::new();
        cold.update(&relocated).into_result().unwrap();
        let fresh = cold.canonical_semantic(&options).unwrap();
        assert_eq!(
            format!("{:?}", reused.functions()),
            format!("{:?}", fresh.functions()),
            "{name} production import diverged from fresh analysis"
        );
        assert_eq!(reused.strings(), fresh.strings());
        assert_eq!(reused.type_pool().stats(), fresh.type_pool().stats());
    }
}
