use super::*;

#[test]
fn foreign_signature_agreement_uses_resolved_identity_mode_and_comptime_not_names() {
    use crate::durable_semantics::{
        DurableParameterMode as Mode, DurableSemanticParameter as Parameter, DurableType as Type,
    };

    let parameter = |name: &str, ty: Type, mode: Mode, is_comptime: bool| Parameter {
        name: Arc::from(name),
        ty,
        mode,
        is_comptime,
    };
    let left = [parameter("left", Type::I64, Mode::Value, false)];
    let renamed = [parameter("right", Type::I64, Mode::Value, false)];
    assert!(foreign_signatures_agree(
        &left,
        &Type::I64,
        &renamed,
        &Type::I64
    ));

    let borrowed = [parameter("left", Type::I64, Mode::Borrow, false)];
    assert_eq!(
        foreign_signature_display(&borrowed, &Type::I64),
        "fn(borrow i64) -> i64"
    );
    assert!(!foreign_signatures_agree(
        &left,
        &Type::I64,
        &borrowed,
        &Type::I64
    ));
    let comptime = [parameter("left", Type::I64, Mode::Value, true)];
    assert_eq!(
        foreign_signature_display(&comptime, &Type::I64),
        "fn(comptime i64) -> i64"
    );
    assert!(!foreign_signatures_agree(
        &left,
        &Type::I64,
        &comptime,
        &Type::I64
    ));

    let nominal = |module: &str| {
        Type::Nominal(crate::StableDefinitionKey::from_stable_parts(
            ModuleId::from_logical_path(module).unwrap(),
            crate::StableDefinitionNamespace::Type,
            crate::StableDefinitionKind::Struct,
            Arc::from("Point"),
            None,
        ))
    };
    let first_nominal = [parameter("point", nominal("left.rue"), Mode::Value, false)];
    let second_nominal = [parameter("point", nominal("right.rue"), Mode::Value, false)];
    assert!(!foreign_signatures_agree(
        &first_nominal,
        &Type::I32,
        &second_nominal,
        &Type::I32
    ));
}

#[test]
fn stable_definition_kinds_have_fixed_syntax_candidate_sets() {
    use crate::StableDefinitionKind as K;
    use crate::declaration_candidate::DeclarationCandidateCategory as C;

    let module = ModuleId::from_logical_path("main.rue").unwrap();
    for (kind, owner, expected) in [
        (K::Function, None, &[C::Function, C::ExternFunction][..]),
        (K::Struct, None, &[C::Struct][..]),
        (K::Enum, None, &[C::Enum][..]),
        (K::ValueConst, None, &[C::ConstCandidate][..]),
        (K::ModuleBinding, None, &[C::ConstCandidate][..]),
        (
            K::Method,
            Some((K::Struct, Arc::from("Owner"))),
            &[C::Method][..],
        ),
        (
            K::AssociatedFunction,
            Some((K::Struct, Arc::from("Owner"))),
            &[C::AssociatedFunction][..],
        ),
        (
            K::Destructor,
            Some((K::Struct, Arc::from("Owner"))),
            &[C::Destructor][..],
        ),
    ] {
        let key = StableDefinitionKey::from_stable_parts(
            module.clone(),
            crate::StableDefinitionNamespace::Value,
            kind,
            "item",
            owner.clone(),
        );
        let candidates = stable_syntax_candidate_set(&key)
            .expect("every well-formed stable definition kind has a syntax candidate set")
            .into_iter()
            .flatten()
            .collect::<Vec<_>>();
        assert_eq!(
            candidates
                .iter()
                .map(|candidate| candidate.category)
                .collect::<Vec<_>>(),
            expected,
            "{kind:?}"
        );
        for candidate in candidates {
            assert_eq!(candidate.module, module);
            assert_eq!(candidate.name.as_ref(), "item");
            assert!(Arc::ptr_eq(&candidate.name, key.shared_name()));
            assert_eq!(candidate.duplicate_discriminator, 0);
            assert_eq!(
                candidate.owner.as_ref().map(|owner| owner.name.as_ref()),
                owner.as_ref().map(|(_, name)| name.as_ref())
            );
            if let Some(candidate_owner) = &candidate.owner {
                assert!(Arc::ptr_eq(
                    &candidate_owner.name,
                    key.owner().unwrap().shared_name()
                ));
            }
        }
    }
}

#[test]
fn stable_declaration_classification_is_narrow_green_and_multiplicity_sensitive() {
    let first = source_snapshot(
        &[(
            1,
            "/main.rue",
            "main.rue",
            "fn helper(value: i32) -> i32 { value + 1 }\nfn main() -> i32 { helper(1) }",
        )],
        1,
    );
    let unrelated = source_snapshot(
        &[(
            1,
            "/main.rue",
            "main.rue",
            "fn helper(value: i32) -> i32 { value + 1 }\nfn extra() -> i32 { 9 }\nfn main() -> i32 { helper(1) }",
        )],
        1,
    );
    let duplicate = source_snapshot(
        &[(
            1,
            "/main.rue",
            "main.rue",
            "fn helper(value: i32) -> i32 { value + 1 }\nfn helper(value: i32) -> i32 { value + 2 }\nfn main() -> i32 { helper(1) }",
        )],
        1,
    );
    let module = ModuleId::from_logical_path("main.rue").unwrap();
    let key = StableDeclarationClassificationQueryKey(StableDefinitionKey::from_stable_parts(
        module,
        crate::StableDefinitionNamespace::Value,
        crate::StableDefinitionKind::Function,
        "helper",
        None,
    ));
    let mut database = RevisionedQueryDatabase::default();
    let first_revision =
        database.source_revision(&crate::session::ExactSourceInput::new(&first), &first);
    let first = database.runtime.request_registered(
        &database.stable_declaration_classifications,
        first_revision,
        key.clone(),
        CancellationToken::new(),
    );
    let first_terminal = first.terminal().unwrap();
    let first_stamp = first_terminal.stamp();
    assert!(matches!(
        first_terminal.outcome(),
        rue_query::QueryOutcome::Success(
            StableDeclarationClassificationQueryValue::Selected(candidate)
        ) if candidate.category
            == crate::declaration_candidate::DeclarationCandidateCategory::Function
    ));
    assert_eq!(
        first
            .dependencies()
            .iter()
            .map(|dependency| dependency.node.family())
            .collect::<Vec<_>>(),
        vec![
            "compiler.declaration-occurrence-index",
            "compiler.declaration-shell"
        ]
    );

    let unrelated_revision = database.source_revision(
        &crate::session::ExactSourceInput::new(&unrelated),
        &unrelated,
    );
    let unrelated = database.runtime.request_registered(
        &database.stable_declaration_classifications,
        unrelated_revision,
        key.clone(),
        CancellationToken::new(),
    );
    assert_eq!(
        unrelated.terminal().unwrap().stamp(),
        first_stamp,
        "an unrelated declaration may rebuild the module occurrence index but must keep the \
             narrow classification green"
    );

    let duplicate_revision = database.source_revision(
        &crate::session::ExactSourceInput::new(&duplicate),
        &duplicate,
    );
    let duplicate = database.runtime.request_registered(
        &database.stable_declaration_classifications,
        duplicate_revision,
        key,
        CancellationToken::new(),
    );
    let duplicate_terminal = duplicate.terminal().unwrap();
    assert_ne!(duplicate_terminal.stamp(), first_stamp);
    assert!(matches!(
        duplicate_terminal.outcome(),
        rue_query::QueryOutcome::Success(StableDeclarationClassificationQueryValue::Invalid(
            StableDeclarationClassificationFailure::DuplicateMultiplicity {
                multiplicity: 2,
                ..
            }
        ))
    ));
}

#[test]
fn anonymous_nominal_traversal_visits_each_shared_identity_exactly_once() {
    // RUE-1555: canonical argument slices are shared through `Arc`, so one
    // instance key reaches the same nested identity through many paths and
    // the visited set is consulted far more often than it grows. It used
    // to be a `Vec` scanned linearly, which made the traversal quadratic
    // in the identities a key reaches; membership is now constant-time.
    //
    // What must not change is the traversal: every reachable identity
    // produced exactly once, by pointer identity rather than structural
    // equality, with the scratch buffer reused across calls.
    const LEAVES: u32 = 256;
    const SHARERS: u32 = 64;

    let module = ModuleId::from_logical_path("anon.rue").unwrap();
    let definition = |name: &str| {
        crate::StableDefinitionKey::from_stable_parts(
            module.clone(),
            crate::StableDefinitionNamespace::Type,
            crate::StableDefinitionKind::Struct,
            Arc::from(name),
            None,
        )
    };
    let leaf = |ordinal: u32| crate::AnonymousNominalKey {
        kind: crate::semantic_identity::AnonymousNominalKind::Struct,
        producer: crate::StableProducerId::Definition(definition("make")),
        anchor: crate::semantic_identity::StructuralAnchor::new(vec![
            crate::semantic_identity::StructuralPathSegment::AnonymousType(ordinal),
        ]),
    };

    // One slice, cloned into every specialization below, so each of the
    // SHARERS levels re-walks the very same LEAVES addresses. That is the
    // adversarial shape: LEAVES * SHARERS visit attempts against a visited
    // set that only ever holds LEAVES entries.
    let shared: Arc<[crate::TypeInstanceKey]> = (0..LEAVES)
        .map(|ordinal| {
            crate::TypeInstanceKey::Nominal(crate::NominalInstanceKey::Anonymous(Node::new(leaf(
                ordinal,
            ))))
        })
        .collect::<Vec<_>>()
        .into();
    let wide = |base: crate::FunctionInstanceKey| crate::FunctionInstanceKey::Specialization {
        base: Node::new(base),
        arguments: crate::CanonicalArguments {
            types: shared.clone(),
            values: Arc::new([]),
        },
    };

    let mut key = free_function_instance(&module, "root");
    for _ in 0..SHARERS {
        key = wide(key);
    }
    // Deep nesting on top of the fan-out: an identity whose producer is
    // itself a specialization carrying the same slice, so the whole leaf
    // set is reachable a second time through a different kind of edge.
    let nested = crate::AnonymousNominalKey {
        kind: crate::semantic_identity::AnonymousNominalKind::Struct,
        producer: crate::StableProducerId::Function(Node::new(wide(free_function_instance(
            &module, "nested",
        )))),
        anchor: crate::semantic_identity::StructuralAnchor::new(vec![
            crate::semantic_identity::StructuralPathSegment::AnonymousType(LEAVES),
        ]),
    };
    let key = crate::FunctionInstanceKey::Specialization {
        base: Node::new(key),
        arguments: crate::CanonicalArguments {
            types: Arc::from([crate::TypeInstanceKey::Nominal(
                crate::NominalInstanceKey::Anonymous(Node::new(nested.clone())),
            )]),
            values: Arc::new([]),
        },
    };

    let mut scratch = AHashSet::new();
    let mut visited = Vec::new();
    visit_instance_anonymous_nominals(&key, &mut scratch, |identity| {
        visited.push(identity.clone());
    });

    assert_eq!(
        visited.len(),
        LEAVES as usize + 1,
        "every leaf plus the nested identity is produced exactly once, \
             however many paths reach it"
    );
    let distinct: BTreeSet<crate::AnonymousNominalKey> = visited.iter().cloned().collect();
    assert_eq!(
        distinct.len(),
        visited.len(),
        "a repeat visit must be suppressed, not merely deduplicated later"
    );
    assert!(
        distinct.contains(&nested),
        "the identity reached through a Function producer is still visited"
    );
    assert_eq!(
        collect_instance_anonymous_nominals(&key),
        distinct,
        "the collecting wrapper agrees with the raw traversal"
    );

    // Scratch reuse: the buffer is cleared at entry, so a second traversal
    // through the same one produces the same result rather than a
    // truncated one.
    let mut reused = Vec::new();
    visit_instance_anonymous_nominals(&key, &mut scratch, |identity| {
        reused.push(identity.clone());
    });
    assert_eq!(
        reused, visited,
        "the scratch set must be cleared and reused between traversals"
    );
}

fn assert_direct_semantic_observation(
    label: &str,
    attempt: &QueryRequestAttempt<crate::semantic_query_nucleus::SemanticNucleusValue>,
    required_families: &[&str],
    allowed_families: &[&str],
    maximum_dependencies: usize,
) {
    let actual = attempt
        .dependencies()
        .iter()
        .map(|dependency| dependency.node.family())
        .collect::<BTreeSet<_>>();
    let required = required_families.iter().copied().collect::<BTreeSet<_>>();
    let allowed = allowed_families.iter().copied().collect::<BTreeSet<_>>();
    assert!(
        required.is_subset(&actual),
        "{label} omitted a required direct dependency family: required={required:?}, actual={actual:?}"
    );
    assert!(
        actual.is_subset(&allowed),
        "{label} observed an unexpected dependency family: actual={actual:?}, allowed={allowed:?}; batch, root, full-plan, and unrelated discovery dependencies are forbidden"
    );
    assert!(
        attempt.dependencies().len() <= maximum_dependencies,
        "{label} observed broad same-family discovery: dependencies={:?}",
        attempt.dependencies()
    );
    assert!(
        attempt.inputs().is_empty(),
        "{label} read inputs directly instead of through its precise query dependencies: {:?}",
        attempt.inputs()
    );
}

fn nucleus_failure_message(
    value: &crate::semantic_query_nucleus::SemanticNucleusValue,
) -> Option<String> {
    use crate::semantic_query_nucleus::{SemanticNucleusFailure as F, SemanticNucleusValue as V};
    match value {
        V::Failure(
            F::Diagnostic(kind)
            | F::DiagnosticAtParameter { kind, .. }
            | F::DiagnosticAtDeclaration { kind, .. }
            | F::OwnershipGate { kind, .. }
            | F::DiagnosticWithHelp { kind, .. }
            | F::DiagnosticWithNote { kind, .. },
        ) => Some(kind.to_string()),
        _ => None,
    }
}

#[test]
fn direct_identity_and_signature_families_are_complete_per_declaration() {
    use crate::declaration_candidate::DeclarationCandidateCategory as Category;
    use crate::durable_semantics::{DurableParameterMode, DurableType};
    use crate::semantic_query_nucleus::{
        DeclarationSignatureProjection as Sig, SemanticNucleusKey as Key, SemanticNucleusValue as V,
    };

    let source = source_snapshot(
        &[(
            1,
            "/main.rue",
            "main.rue",
            "struct S { value: i32, fn get(borrow self, delta: i32) -> i32 { self.value + delta } fn make(value: i32) -> S { S { value } } } enum E { A, B } drop fn S(self) {} fn free(value: i32) -> i32 { value } fn main() {}",
        )],
        1,
    );
    let module = ModuleId::from_logical_path("main.rue").unwrap();
    let mut database = RevisionedQueryDatabase::default();
    let revision =
        database.source_revision(&crate::session::ExactSourceInput::new(&source), &source);

    for (category, kind, name, owner) in [
        (
            Category::Function,
            crate::StableDefinitionKind::Function,
            "free",
            None,
        ),
        (
            Category::Struct,
            crate::StableDefinitionKind::Struct,
            "S",
            None,
        ),
        (Category::Enum, crate::StableDefinitionKind::Enum, "E", None),
        (
            Category::Method,
            crate::StableDefinitionKind::Method,
            "get",
            Some("S"),
        ),
        (
            Category::AssociatedFunction,
            crate::StableDefinitionKind::AssociatedFunction,
            "make",
            Some("S"),
        ),
        (
            Category::Destructor,
            crate::StableDefinitionKind::Destructor,
            "S",
            Some("S"),
        ),
    ] {
        let declaration = declaration_candidate(&database, revision, &module, category, name);
        let query = crate::semantic_query_nucleus::DeclarationSemanticQueryKey {
            declaration,
            configuration: semantic_configuration(),
        };
        let (identity, identity_attempt) =
            request_semantic_nucleus_observed(&database, revision, Key::Identity(query.clone()));
        if category == Category::Destructor {
            assert_direct_semantic_observation(
                "destructor identity",
                &identity_attempt,
                &["compiler.declaration-shell", "compiler.semantic-nucleus"],
                &[
                    "compiler.declaration-occurrence-index",
                    "compiler.declaration-shell",
                    "compiler.lookup-name",
                    "compiler.module-index",
                    "compiler.parse-module",
                    "compiler.semantic-nucleus",
                ],
                7,
            );
        } else {
            assert_direct_semantic_observation(
                "direct identity",
                &identity_attempt,
                &["compiler.declaration-shell"],
                &[
                    "compiler.declaration-occurrence-index",
                    "compiler.declaration-shell",
                    "compiler.parse-module",
                ],
                3,
            );
        }
        let V::Identity(identity) = identity else {
            panic!("direct identity failed for {kind:?} {name}: {identity:?}")
        };
        assert_eq!(identity.key.namespace(), kind.namespace());
        assert_eq!(identity.key.kind(), kind);
        assert_eq!(identity.key.name(), name);
        assert_eq!(identity.key.module(), &module);
        assert_eq!(identity.key.owner().map(|owner| owner.name()), owner);
        assert!(
            !identity.is_public,
            "no declaration in this fixture is `pub`"
        );

        let (signature, signature_attempt) =
            request_semantic_nucleus_observed(&database, revision, Key::Signature(query));
        match category {
            Category::Destructor => assert_direct_semantic_observation(
                "destructor signature",
                &signature_attempt,
                &[
                    "compiler.declaration-shell",
                    "compiler.lookup-name",
                    "compiler.parse-module",
                ],
                &[
                    "compiler.declaration-occurrence-index",
                    "compiler.declaration-shell",
                    "compiler.lookup-name",
                    "compiler.module-index",
                    "compiler.parse-module",
                    "compiler.semantic-nucleus",
                ],
                10,
            ),
            Category::Method | Category::AssociatedFunction => assert_direct_semantic_observation(
                "owned callable signature",
                &signature_attempt,
                &[
                    "compiler.declaration-shell",
                    "compiler.parse-module",
                    "compiler.semantic-nucleus",
                ],
                &[
                    "compiler.declaration-occurrence-index",
                    "compiler.declaration-shell",
                    "compiler.lookup-name",
                    "compiler.module-index",
                    "compiler.parse-module",
                    "compiler.semantic-nucleus",
                ],
                9,
            ),
            _ => assert_direct_semantic_observation(
                "direct signature",
                &signature_attempt,
                &["compiler.declaration-shell", "compiler.parse-module"],
                &[
                    "compiler.declaration-occurrence-index",
                    "compiler.declaration-shell",
                    "compiler.lookup-name",
                    "compiler.parse-module",
                ],
                5,
            ),
        }
        let V::Signature(signature) = signature else {
            panic!("direct signature failed for {kind:?} {name}: {signature:?}")
        };
        let signature = &signature.signature;
        match (kind, name) {
            (crate::StableDefinitionKind::Function, "free") => {
                let Sig::Callable {
                    parameters,
                    result,
                    has_self,
                    self_mode,
                    is_accessor,
                    is_unchecked,
                    is_extern,
                    is_c_export,
                    ..
                } = signature
                else {
                    panic!("free must project a callable signature: {signature:?}")
                };
                let [value] = parameters.as_ref() else {
                    panic!("free has one parameter: {parameters:?}")
                };
                assert_eq!(value.name.as_ref(), "value");
                assert_eq!(value.ty, DurableType::I32);
                assert_eq!(value.mode, DurableParameterMode::Value);
                assert!(!value.is_comptime);
                assert_eq!(result, &DurableType::I32);
                assert!(!has_self);
                assert_eq!(self_mode, &DurableParameterMode::Value);
                assert!(!is_accessor && !is_unchecked && !is_extern && !is_c_export);
            }
            (crate::StableDefinitionKind::Struct, "S") => {
                let Sig::Struct {
                    fields,
                    is_copy,
                    is_linear,
                    is_repr_c,
                } = signature
                else {
                    panic!("S must project a struct signature: {signature:?}")
                };
                let [(field, ty)] = fields.as_ref() else {
                    panic!("S has one field: {fields:?}")
                };
                assert_eq!(field.as_ref(), "value");
                assert_eq!(ty, &DurableType::I32);
                assert!(!is_copy, "a destructor-bearing struct is not copyable");
                assert!(!is_linear && !is_repr_c);
            }
            (crate::StableDefinitionKind::Enum, "E") => {
                let Sig::Enum { variants, .. } = signature else {
                    panic!("E must project an enum signature: {signature:?}")
                };
                let rendered = variants
                    .iter()
                    .map(|(name, payload)| (name.as_ref(), payload.len()))
                    .collect::<Vec<_>>();
                assert_eq!(rendered, [("A", 0), ("B", 0)]);
            }
            (crate::StableDefinitionKind::Method, "get") => {
                let Sig::Callable {
                    parameters,
                    result,
                    has_self,
                    self_mode,
                    ..
                } = signature
                else {
                    panic!("get must project a callable signature: {signature:?}")
                };
                let [delta] = parameters.as_ref() else {
                    panic!("get has one explicit parameter: {parameters:?}")
                };
                assert_eq!(delta.name.as_ref(), "delta");
                assert_eq!(delta.ty, DurableType::I32);
                assert_eq!(result, &DurableType::I32);
                assert!(has_self);
                assert_eq!(self_mode, &DurableParameterMode::Borrow);
            }
            (crate::StableDefinitionKind::AssociatedFunction, "make") => {
                let Sig::Callable {
                    parameters,
                    result,
                    has_self,
                    ..
                } = signature
                else {
                    panic!("make must project a callable signature: {signature:?}")
                };
                assert_eq!(parameters.len(), 1);
                assert!(!has_self);
                let DurableType::Nominal(owner_key) = result else {
                    panic!("make returns the owning nominal: {result:?}")
                };
                assert_eq!(owner_key.name(), "S");
                assert_eq!(owner_key.kind(), crate::StableDefinitionKind::Struct);
            }
            (crate::StableDefinitionKind::Destructor, "S") => {
                assert_eq!(signature, &Sig::Destructor);
            }
            other => panic!("unexpected fixture declaration {other:?}"),
        }
    }
}

#[test]
fn direct_const_family_evaluates_the_annotated_initializer() {
    use crate::declaration_candidate::DeclarationCandidateCategory as Category;
    use crate::semantic_query_nucleus::{
        ConstResolutionProjection as Resolution, SemanticNucleusKey as Key,
        SemanticNucleusValue as V,
    };

    let source = source_snapshot(
        &[(
            1,
            "/main.rue",
            "main.rue",
            "const SELECTED: i32 = 40 + 2; fn main() -> i32 { SELECTED }",
        )],
        1,
    );
    let module = ModuleId::from_logical_path("main.rue").unwrap();
    let mut database = RevisionedQueryDatabase::default();
    let revision =
        database.source_revision(&crate::session::ExactSourceInput::new(&source), &source);
    let declaration = declaration_candidate(
        &database,
        revision,
        &module,
        Category::ConstCandidate,
        "SELECTED",
    );
    let configuration = semantic_configuration();
    let (keyed, keyed_attempt) = request_semantic_nucleus_observed(
        &database,
        revision,
        Key::ConstResolution(crate::semantic_query_nucleus::DeclarationSemanticQueryKey {
            declaration,
            configuration,
        }),
    );
    assert_direct_semantic_observation(
        "const evaluation",
        &keyed_attempt,
        &[
            "compiler.declaration-body-plan-artifacts",
            "compiler.declaration-shell",
            "compiler.lookup-name",
        ],
        &[
            "compiler.declaration-body-plan-artifacts",
            "compiler.declaration-occurrence-index",
            "compiler.declaration-shell",
            "compiler.lookup-name",
            "compiler.module-index",
            "compiler.parse-module",
        ],
        6,
    );
    let V::ConstResolution(Resolution::Value {
        ty: keyed_ty,
        value,
        ..
    }) = keyed
    else {
        panic!("direct const terminal failed: {keyed:?}")
    };
    let crate::durable_semantics::DurableConstValue::Integer(keyed_value) = *value else {
        panic!("direct const terminal returned a non-integer value")
    };
    assert_eq!(keyed_ty, crate::durable_semantics::DurableType::I32);
    assert_eq!(keyed_value, 42, "`40 + 2` evaluates at declaration time");
}

#[test]
#[ignore = "platform_native_ host coverage; run by rue-compiler-platform-native-test"]
fn platform_native_direct_target_selected_comptime_evaluates_under_the_host_arch() {
    use crate::declaration_candidate::DeclarationCandidateCategory as Category;
    use crate::semantic_query_nucleus::{
        ComptimeCallQueryKey, ComptimeCallResultProjection as ResultProjection,
        SemanticNucleusKey as Key, SemanticNucleusValue as V,
    };

    let source = source_snapshot(
        &[(
            1,
            "/main.rue",
            "main.rue",
            "fn selected(comptime seed: i32) -> i32 { match @target_arch() { Arch.X86_64 => seed + 64, Arch.Aarch64 => seed + 32 } } fn main() -> i32 { selected(0) }",
        )],
        1,
    );
    let target = rue_target::Target::host().expect("test host is a supported target");
    // `@target_arch()` selects the match arm from the configured target, so
    // the expected value follows directly from the host architecture.
    let expected = match target.arch() {
        rue_target::Arch::X86_64 => 64,
        rue_target::Arch::Aarch64 => 32,
    };
    let module = ModuleId::from_logical_path("main.rue").unwrap();
    let mut database = RevisionedQueryDatabase::default();
    let revision =
        database.source_revision(&crate::session::ExactSourceInput::new(&source), &source);
    let mut configuration = semantic_configuration();
    configuration.target = target;
    let (keyed, keyed_attempt) = request_semantic_nucleus_observed(
        &database,
        revision,
        Key::ComptimeCall(ComptimeCallQueryKey {
            declaration: crate::semantic_query_nucleus::DeclarationSemanticQueryKey {
                declaration: declaration_candidate(
                    &database,
                    revision,
                    &module,
                    Category::Function,
                    "selected",
                ),
                configuration,
            },
            type_arguments: Arc::from([]),
            value_arguments: Arc::from([(
                Arc::from("seed"),
                crate::durable_semantics::DurableConstValue::Integer(0),
            )]),
        }),
    );
    assert_direct_semantic_observation(
        "target-selected comptime call",
        &keyed_attempt,
        &[
            "compiler.declaration-body-plan-artifacts",
            "compiler.declaration-shell",
            "compiler.semantic-nucleus",
        ],
        &[
            "compiler.declaration-body-plan-artifacts",
            "compiler.declaration-occurrence-index",
            "compiler.declaration-shell",
            "compiler.parse-module",
            "compiler.semantic-nucleus",
        ],
        7,
    );
    let V::ComptimeCall(crate::semantic_query_nucleus::ComptimeCallProjection {
        result: ResultProjection::Value(crate::durable_semantics::DurableConstValue::Integer(keyed)),
        ..
    }) = keyed
    else {
        panic!("direct target-selected const failed: {keyed:?}")
    };
    assert_eq!(i128::from(expected), keyed);
}

/// RUE-1112 demand-resolves proof. Once the trusted `\0rue-std/option.rue`
/// module is present in the snapshot's module set — exactly as the host
/// publishes it on the successor after satisfying a
/// `TrustedToolchainModuleDemand` — a directly-rooted `ComptimeCall` for
/// `\0rue-std/option.rue::Option(i64)` resolves the real materialized
/// nominal with std provenance. This is the proven key shape:
///
/// `DeclarationCandidateKey { module: from_trusted_standard_library_path(..),
///  Function, "Option", None }` -> `DeclarationSemanticQueryKey` ->
/// `ComptimeCallQueryKey { type_arguments: [("T", DurableType::I64)] }`.
///
/// The consumption track wires this into AIR; here we only prove
/// resolvability against a present trusted module.
#[test]
fn trusted_std_option_comptime_call_resolves_for_i64() {
    use crate::declaration_candidate::DeclarationCandidateCategory as Category;
    use crate::durable_semantics::{DurableAnonymousNominalShape as Shape, DurableType};
    use crate::semantic_query_nucleus::{
        ComptimeCallQueryKey, ComptimeCallResultProjection as ResultProjection,
        SemanticNucleusKey as Key, SemanticNucleusValue as V,
    };

    // The freestanding fallible-intrinsic program plus the trusted Option
    // module the host published on the successor. `main.rue` names a bare
    // `@parse_i64`, which is the reason the demand was emitted upstream.
    let root = FileId::new(1);
    let option = FileId::new(2);
    let physical = AHashMap::from([
        (root, "/project/main.rue".to_owned()),
        (option, "/sdk/option.rue".to_owned()),
    ]);
    let logical = AHashMap::from([
        (root, "main.rue".to_owned()),
        (option, crate::OPTION_MODULE_LOGICAL_PATH.to_owned()),
    ]);
    let metadata = SourceMetadata::new_with_trusted_standard_library(
        root,
        physical,
        logical,
        AHashSet::from([option]),
    )
    .unwrap();
    let source = SourceSnapshot::new(
        metadata,
        vec![
            (
                root,
                Arc::new("fn main() -> i32 { let x: i32 = 0; x }".to_owned()),
            ),
            (
                option,
                Arc::new(
                    "pub fn Option(comptime T: type) -> type { enum { Some(T), None } }".to_owned(),
                ),
            ),
        ],
    )
    .unwrap();

    let module =
        ModuleId::from_trusted_standard_library_path(crate::OPTION_MODULE_LOGICAL_PATH).unwrap();
    assert!(
        module.is_trusted_standard_library(),
        "the demand resolves against a trusted std module"
    );

    let mut database = RevisionedQueryDatabase::default();
    let revision =
        database.source_revision(&crate::session::ExactSourceInput::new(&source), &source);
    let value = request_semantic_nucleus(
        &database,
        revision,
        Key::ComptimeCall(ComptimeCallQueryKey {
            declaration: crate::semantic_query_nucleus::DeclarationSemanticQueryKey {
                declaration: declaration_candidate(
                    &database,
                    revision,
                    &module,
                    Category::Function,
                    "Option",
                ),
                configuration: semantic_configuration(),
            },
            type_arguments: Arc::from([(Arc::from("T"), DurableType::I64)]),
            value_arguments: Arc::from([]),
        }),
    );

    let V::ComptimeCall(projection) = value else {
        panic!("trusted Option(i64) comptime call did not resolve: {value:?}");
    };
    assert!(
        matches!(projection.result, ResultProjection::Type(_)),
        "Option(i64) must resolve to a materialized type, got {:?}",
        projection.result
    );
    // The real materialized nominal: an Option enum whose `Some` carries the
    // requested `i64` payload and whose `None` is empty.
    let materialized_option = projection.anonymous_nominals.iter().any(|nominal| {
        matches!(
            &nominal.shape,
            Shape::Enum { variants }
                if variants.len() == 2
                    && variants.iter().any(|(name, payload)| {
                        name.as_ref() == "Some"
                            && payload.len() == 1
                            && payload[0] == DurableType::I64
                    })
                    && variants.iter().any(|(name, payload)| {
                        name.as_ref() == "None" && payload.is_empty()
                    })
        )
    });
    assert!(
        materialized_option,
        "Option(i64) must materialize a real Some(i64)/None nominal: {:?}",
        projection.anonymous_nominals
    );
}

#[test]
fn cold_foreign_comptime_probe_admits_owned_program_without_value_evaluation() {
    let source = source_snapshot(
        &[(
            1,
            "/main.rue",
            "main.rue",
            "fn target() -> i32 { @import(\"dep\"); 1 }",
        )],
        1,
    );
    let module = ModuleId::from_logical_path("main.rue").unwrap();
    let producer = StableDefinitionKey::from_stable_parts(
        module,
        crate::StableDefinitionNamespace::Value,
        crate::StableDefinitionKind::Function,
        "target",
        None,
    );
    let mut database = RevisionedQueryDatabase::default();
    let revision =
        database.source_revision(&crate::session::ExactSourceInput::new(&source), &source);
    let astgen_before = database
        .declaration_body_plan_astgen_evaluations
        .load(std::sync::atomic::Ordering::Relaxed);
    let probe = database
        .probe_ready_body_facts(
            revision,
            semantic_configuration(),
            "cold-foreign-comptime-probe",
            |provider| provider.probe_comptime_call(&producer, &[], &[]),
        )
        .result
        .unwrap();
    let crate::body_query::ForeignComptimeCallLookup::Admitted(program) = probe else {
        panic!("cold foreign comptime lookup should admit its owned body plan");
    };
    assert_eq!(program.plan.key.declaration, producer);
    assert_eq!(
        program.plan.key.configuration,
        semantic_configuration(),
        "owned admission must retain the exact requested configuration"
    );
    assert_eq!(
        program.callable().expect("callable root").context.as_str(),
        "main.rue"
    );
    assert_eq!(program.imports.imports.len(), 1);
    assert_eq!(
        database
            .declaration_body_plan_astgen_evaluations
            .load(std::sync::atomic::Ordering::Relaxed),
        astgen_before + 1
    );
    let candidate = declaration_candidate_for_stable_key(&producer).unwrap();
    let comptime_key = crate::semantic_query_nucleus::SemanticNucleusKey::ComptimeCall(
        crate::semantic_query_nucleus::ComptimeCallQueryKey {
            declaration: crate::semantic_query_nucleus::DeclarationSemanticQueryKey {
                declaration: candidate,
                configuration: semantic_configuration(),
            },
            type_arguments: Arc::from([]),
            value_arguments: Arc::from([]),
        },
    );
    assert!(
        !database
            .semantic_nucleus
            .contains_retained_key(&comptime_key),
        "a cold ready-only probe must not demand or evaluate its comptime value"
    );
}

#[test]
fn ready_foreign_comptime_probe_reuses_full_projection_without_body_materialization() {
    use crate::declaration_candidate::DeclarationCandidateCategory as Category;
    use crate::semantic_query_nucleus::{
        ComptimeCallQueryKey, ComptimeCallResultProjection, SemanticNucleusKey,
        SemanticNucleusValue,
    };

    let source = source_snapshot(
        &[(
            1,
            "/main.rue",
            "main.rue",
            "fn selected(comptime seed: i32) -> i32 { seed + 64 }",
        )],
        1,
    );
    let module = ModuleId::from_logical_path("main.rue").unwrap();
    let mut database = RevisionedQueryDatabase::default();
    let revision =
        database.source_revision(&crate::session::ExactSourceInput::new(&source), &source);
    let producer = StableDefinitionKey::from_stable_parts(
        module.clone(),
        crate::StableDefinitionNamespace::Value,
        crate::StableDefinitionKind::Function,
        "selected",
        None,
    );
    let declaration =
        declaration_candidate(&database, revision, &module, Category::Function, "selected");
    let key = SemanticNucleusKey::ComptimeCall(ComptimeCallQueryKey {
        declaration: crate::semantic_query_nucleus::DeclarationSemanticQueryKey {
            declaration,
            configuration: semantic_configuration(),
        },
        type_arguments: Arc::from([]),
        value_arguments: Arc::from([(
            Arc::from("seed"),
            crate::durable_semantics::DurableConstValue::Integer(0),
        )]),
    });
    let value = request_semantic_nucleus(&database, revision, key.clone());
    let SemanticNucleusValue::ComptimeCall(projection) = value else {
        panic!("the setup comptime call must publish a projection");
    };
    assert!(matches!(
        &projection.result,
        ComptimeCallResultProjection::Value(crate::durable_semantics::DurableConstValue::Integer(
            64
        ))
    ));
    let astgen_after_setup = database
        .declaration_body_plan_astgen_evaluations
        .load(std::sync::atomic::Ordering::Relaxed);
    let probe = database.probe_ready_body_facts(
        revision,
        semantic_configuration(),
        "ready-foreign-comptime-probe",
        |provider| {
            provider.probe_comptime_call(
                &producer,
                &[],
                &[(
                    Arc::from("seed"),
                    crate::durable_semantics::DurableConstValue::Integer(0),
                )],
            )
        },
    );
    let outcome = probe.result.unwrap();
    let crate::body_query::ForeignComptimeCallLookup::Ready(observed) = outcome else {
        panic!("the ready probe must retain the published projection");
    };
    assert_eq!(observed, projection);
    assert!(
        probe
            .dependencies
            .iter()
            .any(|dependency| dependency.family() == "compiler.semantic-nucleus")
    );
    assert!(
        database
            .declaration_body_plan_astgen_evaluations
            .load(std::sync::atomic::Ordering::Relaxed)
            == astgen_after_setup,
        "a ready hit must not materialize the body-plan artifact"
    );
    assert!(
        database.semantic_nucleus.contains_retained_key(&key),
        "the exact semantic nucleus key remains the observed dependency"
    );
}

#[test]
fn noncomputing_foreign_probe_adapter_does_not_admit_not_ready() {
    let called = std::cell::Cell::new(false);
    let result =
        foreign_comptime_miss_or_not_ready(rue_query::ReadyQueryProbe::<()>::NotReady, || {
            called.set(true);
            panic!("NotReady must not construct the cold-miss admission");
        })
        .unwrap();
    assert!(matches!(
        result,
        crate::body_query::ForeignComptimeCallLookup::NotReady
    ));
    assert!(!called.get());
}

#[test]
fn noncomputing_foreign_probe_adapter_admits_a_cold_miss_once() {
    let calls = std::cell::Cell::new(0);
    let result = foreign_comptime_miss_or_not_ready(rue_query::ReadyQueryProbe::<()>::Miss, || {
        calls.set(calls.get() + 1);
        Ok(crate::body_query::ForeignComptimeCallLookup::NotReady)
    })
    .unwrap();
    assert!(matches!(
        result,
        crate::body_query::ForeignComptimeCallLookup::NotReady
    ));
    assert_eq!(calls.get(), 1);
}

/// Anonymous identity comes from the canonical candidate artifact. A source
/// comment that used to activate the retired reparse-transport fault seam is
/// now inert and cannot alter the producer's structural anchor.
#[test]
fn comptime_anchor_identity_comes_from_the_candidate_artifact() {
    use crate::declaration_candidate::DeclarationCandidateCategory as Category;
    use crate::semantic_query_nucleus::{
        ComptimeCallQueryKey, ComptimeCallResultProjection, SemanticNucleusKey as Key,
        SemanticNucleusValue as V,
    };

    let program = "fn Option(comptime T: type) -> type { enum { Some(T), None } }\n\
             fn Wrap(comptime T: type) -> type {\n\
                 // __RUE1089_FAULT_DIVERGE__\n\
                 struct {\n\
                     inner: Option(T),\n\
                     fn get_or(self, d: T) -> T {\n\
                         let O = Option(T);\n\
                         match self.inner { O.Some(v) => v, O.None => d }\n\
                     }\n\
                 }\n\
             }\n\
             fn main() -> i32 { let W = Wrap(i32); let O = Option(i32); let w: W = W { inner: O.Some(42) }; w.get_or(0) }";
    let source = source_snapshot(&[(1, "/main.rue", "main.rue", program)], 1);
    let module = ModuleId::from_logical_path("main.rue").unwrap();
    let mut database = RevisionedQueryDatabase::default();
    let revision =
        database.source_revision(&crate::session::ExactSourceInput::new(&source), &source);
    let value = request_semantic_nucleus(
        &database,
        revision,
        Key::ComptimeCall(ComptimeCallQueryKey {
            declaration: crate::semantic_query_nucleus::DeclarationSemanticQueryKey {
                declaration: declaration_candidate(
                    &database,
                    revision,
                    &module,
                    Category::Function,
                    "Wrap",
                ),
                configuration: semantic_configuration(),
            },
            type_arguments: Arc::from([(
                Arc::from("T"),
                crate::durable_semantics::DurableType::I32,
            )]),
            value_arguments: Arc::from([]),
        }),
    );
    match value {
        V::ComptimeCall(projection) => {
            assert!(matches!(
                projection.result,
                ComptimeCallResultProjection::Type(_)
            ));
            assert!(!projection.anonymous_nominals.is_empty());
        }
        other => panic!("candidate-artifact anchor evaluation failed: {other:?}"),
    }
}

#[test]
fn direct_ownership_terminals_accept_droppable_and_reject_linear_payloads() {
    use crate::declaration_candidate::DeclarationCandidateCategory as Category;
    use crate::semantic_query_nucleus::{SemanticNucleusKey as Key, SemanticNucleusValue as V};

    for (source_text, expected_failure) in [
        (
            "enum Maybe { Some, None } fn Gated(comptime T: type) -> type { @require_droppable(T); T } const G = Gated(Maybe); fn main() {}",
            None,
        ),
        (
            "linear struct Token { v: i32 } fn Gated(comptime T: type) -> type { @require_droppable(T); T } const G = Gated(Token); fn main() {}",
            Some(
                "`@require_droppable` requires a trivially-droppable type, but `Token` is `linear` — an owning growable container (e.g. `ArrayBuf`) cannot yet track element linearity, so the element would be leaked (RUE-388)",
            ),
        ),
    ] {
        let source = source_snapshot(&[(1, "/main.rue", "main.rue", source_text)], 1);
        let module = ModuleId::from_logical_path("main.rue").unwrap();
        let mut database = RevisionedQueryDatabase::default();
        let revision =
            database.source_revision(&crate::session::ExactSourceInput::new(&source), &source);
        let producer = crate::semantic_query_nucleus::DeclarationSemanticQueryKey {
            declaration: declaration_candidate(
                &database,
                revision,
                &module,
                Category::ConstCandidate,
                "G",
            ),
            configuration: semantic_configuration(),
        };
        let (resolution, resolution_attempt) = request_semantic_nucleus_observed(
            &database,
            revision,
            Key::ConstResolution(producer.clone()),
        );
        assert_direct_semantic_observation(
            "ownership-gated const producer",
            &resolution_attempt,
            &[
                "compiler.declaration-body-plan-artifacts",
                "compiler.declaration-shell",
                "compiler.lookup-name",
                "compiler.semantic-nucleus",
            ],
            &[
                "compiler.declaration-body-plan-artifacts",
                "compiler.declaration-occurrence-index",
                "compiler.declaration-shell",
                "compiler.lookup-name",
                "compiler.module-index",
                "compiler.parse-module",
                "compiler.semantic-nucleus",
            ],
            14,
        );
        let V::ConstResolution(crate::semantic_query_nucleus::ConstResolutionProjection::Value {
            deferred_ownership,
            ..
        }) = resolution
        else {
            panic!("direct const producer failed before its ownership gate: {resolution:?}")
        };
        let [gate] = deferred_ownership.as_ref() else {
            panic!("expected one direct ownership gate: {deferred_ownership:?}")
        };
        let (keyed, keyed_attempt) = request_semantic_nucleus_observed(
            &database,
            revision,
            Key::DeferredOwnership(crate::semantic_query_nucleus::DeferredOwnershipQueryKey {
                producer,
                gate: gate.clone(),
            }),
        );
        assert_direct_semantic_observation(
            "deferred ownership terminal",
            &keyed_attempt,
            &[
                "compiler.declaration-shell",
                "compiler.lookup-name",
                "compiler.semantic-nucleus",
            ],
            &[
                "compiler.declaration-body-plan-artifacts",
                "compiler.declaration-occurrence-index",
                "compiler.declaration-shell",
                "compiler.lookup-name",
                "compiler.module-index",
                "compiler.parse-module",
                "compiler.semantic-nucleus",
            ],
            18,
        );
        match expected_failure {
            None => assert_eq!(keyed, V::DeferredOwnership),
            Some(expected) => {
                assert_eq!(nucleus_failure_message(&keyed).as_deref(), Some(expected));
            }
        }
    }
}

#[test]
fn ownership_property_memo_preserves_decisions_across_repeats_and_recursion() {
    use crate::declaration_candidate::DeclarationCandidateCategory as Category;
    use crate::semantic_query_nucleus::{SemanticNucleusKey as Key, SemanticNucleusValue as V};

    const LINEAR: &str = "`@require_droppable` requires a trivially-droppable type, but \
             `Top` is `linear` — an owning growable container (e.g. `ArrayBuf`) cannot yet \
             track element linearity, so the element would be leaked (RUE-388)";
    const LINEAR_A: &str = "`@require_droppable` requires a trivially-droppable type, but \
             `A` is `linear` — an owning growable container (e.g. `ArrayBuf`) cannot yet \
             track element linearity, so the element would be leaked (RUE-388)";

    // Each case gates one type behind `@require_droppable`, which runs the
    // recursive ownership walks. The memo answers the repeated mentions
    // from the first walk, so these pin that a reused answer is the same
    // answer.
    for (name, gated, source_text, expected_failure) in [
        (
            // Every aggregate is mentioned three times, so `Mid` and
            // `Leaf` are each walked once and reused twice.
            "repeated non-linear aggregates stay droppable",
            "G",
            "struct Leaf { a: i64, b: i64 }\n\
                 struct Mid { p: Leaf, q: Leaf, r: Leaf }\n\
                 struct Top { x: Mid, y: Mid, z: Mid }\n\
                 fn Gated(comptime T: type) -> type { @require_droppable(T); T }\n\
                 const G = Gated(Top);\n\
                 fn main() {}",
            None,
        ),
        (
            // `Mid` carries a linear field and is mentioned twice. A memo
            // that stored the wrong answer for the second mention would
            // let this pass.
            "linearity survives a reused aggregate answer",
            "G",
            "linear struct Token { v: i32 }\n\
                 struct Mid { a: Token, b: i64 }\n\
                 struct Top { x: Mid, y: Mid }\n\
                 fn Gated(comptime T: type) -> type { @require_droppable(T); T }\n\
                 const G = Gated(Top);\n\
                 fn main() {}",
            Some(LINEAR),
        ),
        (
            // Mutually recursive through pointers. `B` reaches `A` and `A`
            // reaches `B`, and only `A` owns the linear field, so the two
            // must not share one answer.
            "mutually recursive aggregate without the linear field passes",
            "GB",
            "linear struct T { v: i32 }\n\
                 struct B { q: ptr const A, v: i32 }\n\
                 struct A { p: ptr const B, t: T }\n\
                 fn Gated(comptime X: type) -> type { @require_droppable(X); X }\n\
                 const GB = Gated(B);\n\
                 fn main() {}",
            None,
        ),
        (
            "mutually recursive aggregate with the linear field is rejected",
            "GA",
            "linear struct T { v: i32 }\n\
                 struct B { q: ptr const A, v: i32 }\n\
                 struct A { p: ptr const B, t: T }\n\
                 fn Gated(comptime X: type) -> type { @require_droppable(X); X }\n\
                 const GA = Gated(A);\n\
                 fn main() {}",
            Some(LINEAR_A),
        ),
    ] {
        let source = source_snapshot(&[(1, "/main.rue", "main.rue", source_text)], 1);
        let module = ModuleId::from_logical_path("main.rue").unwrap();
        let mut database = RevisionedQueryDatabase::default();
        let revision =
            database.source_revision(&crate::session::ExactSourceInput::new(&source), &source);
        let producer = crate::semantic_query_nucleus::DeclarationSemanticQueryKey {
            declaration: declaration_candidate(
                &database,
                revision,
                &module,
                Category::ConstCandidate,
                gated,
            ),
            configuration: semantic_configuration(),
        };
        let (resolution, _) = request_semantic_nucleus_observed(
            &database,
            revision,
            Key::ConstResolution(producer.clone()),
        );
        let V::ConstResolution(crate::semantic_query_nucleus::ConstResolutionProjection::Value {
            deferred_ownership,
            ..
        }) = resolution
        else {
            panic!("{name}: producer failed before its ownership gate: {resolution:?}")
        };
        let [gate] = deferred_ownership.as_ref() else {
            panic!("{name}: expected one ownership gate: {deferred_ownership:?}")
        };
        let (keyed, _) = request_semantic_nucleus_observed(
            &database,
            revision,
            Key::DeferredOwnership(crate::semantic_query_nucleus::DeferredOwnershipQueryKey {
                producer,
                gate: gate.clone(),
            }),
        );
        match expected_failure {
            None => assert_eq!(keyed, V::DeferredOwnership, "{name}"),
            Some(expected) => {
                assert_eq!(
                    nucleus_failure_message(&keyed).as_deref(),
                    Some(expected),
                    "{name}"
                );
            }
        }
    }
}

#[test]
fn direct_family_failures_are_deterministic_without_root_prevalidation() {
    use crate::declaration_candidate::DeclarationCandidateCategory as Category;
    use crate::semantic_query_nucleus::{SemanticNucleusKey as Key, SemanticNucleusValue as V};

    for (source_text, category, name, identity_terminal, expected) in [
        (
            "drop fn Missing(self) {} fn main() {}",
            Category::Destructor,
            "Missing",
            false,
            "unknown type 'Missing' in destructor",
        ),
        (
            "struct S {} drop fn S(self) {} drop fn S(self) {} fn main() {}",
            Category::Destructor,
            "S",
            true,
            "duplicate destructor for type 'S'",
        ),
        (
            "struct S { fn make(a: i32, a: i32) {} } fn main() {}",
            Category::AssociatedFunction,
            "make",
            false,
            "duplicate parameter name 'a'",
        ),
    ] {
        let source = source_snapshot(&[(1, "/main.rue", "main.rue", source_text)], 1);
        let module = ModuleId::from_logical_path("main.rue").unwrap();
        let mut database = RevisionedQueryDatabase::default();
        let revision =
            database.source_revision(&crate::session::ExactSourceInput::new(&source), &source);
        let query = crate::semantic_query_nucleus::DeclarationSemanticQueryKey {
            declaration: declaration_candidate(&database, revision, &module, category, name),
            configuration: semantic_configuration(),
        };
        let (keyed, keyed_attempt) = request_semantic_nucleus_observed(
            &database,
            revision,
            if identity_terminal {
                Key::Identity(query)
            } else {
                Key::Signature(query)
            },
        );
        if identity_terminal {
            assert_direct_semantic_observation(
                "deterministic destructor identity failure",
                &keyed_attempt,
                &["compiler.declaration-shell", "compiler.semantic-nucleus"],
                &[
                    "compiler.declaration-occurrence-index",
                    "compiler.declaration-shell",
                    "compiler.lookup-name",
                    "compiler.module-index",
                    "compiler.parse-module",
                    "compiler.semantic-nucleus",
                ],
                6,
            );
        } else if category == Category::Destructor {
            assert_direct_semantic_observation(
                "deterministic destructor signature failure",
                &keyed_attempt,
                &["compiler.declaration-shell", "compiler.lookup-name"],
                &[
                    "compiler.declaration-occurrence-index",
                    "compiler.declaration-shell",
                    "compiler.lookup-name",
                    "compiler.module-index",
                    "compiler.parse-module",
                ],
                5,
            );
        } else {
            assert_direct_semantic_observation(
                "deterministic parameter failure",
                &keyed_attempt,
                &["compiler.declaration-shell", "compiler.parse-module"],
                &[
                    "compiler.declaration-occurrence-index",
                    "compiler.declaration-shell",
                    "compiler.parse-module",
                ],
                4,
            );
        }
        assert!(matches!(keyed, V::Failure(_)));
        assert_eq!(
            nucleus_failure_message(&keyed).as_deref(),
            Some(expected),
            "direct keyed failure diverged for {category:?} {name}: {keyed:?}"
        );
    }
}

#[test]
fn direct_declaration_import_family_matches_independent_import_graph_oracle() {
    use crate::declaration_candidate::DeclarationCandidateCategory as Category;

    let source = source_snapshot(
        &[
            (
                1,
                "/project/main.rue",
                "main.rue",
                "const dep = @import(\"dep.rue\"); fn main() -> i32 { dep.value }",
            ),
            (
                2,
                "/project/dep.rue",
                "dep.rue",
                "pub const value: i32 = 42;",
            ),
        ],
        1,
    );
    let discovered = crate::test_support::test_import_graph(&source).unwrap();
    let main = ModuleId::from_logical_path("main.rue").unwrap();
    let expected = discovered
        .records()
        .iter()
        .find(|record| record.importer() == &main && record.normalized_specifier() == "dep.rue")
        .expect("discovered import graph omitted dep.rue")
        .resolution()
        .clone();

    let mut database = RevisionedQueryDatabase::default();
    let revision =
        database.source_revision(&crate::session::ExactSourceInput::new(&source), &source);
    database.adopt_test_import_graph_for_revision(revision, discovered);
    let revision = database.current_semantic_revision().unwrap();
    let requested = database.runtime.request_registered(
        &database.declaration_imports,
        revision,
        declaration_import_key(&main, Category::ConstCandidate, "dep", None, 0, "dep.rue"),
        CancellationToken::new(),
    );
    let rue_query::QueryOutcome::Success(DeclarationImportQueryValue::Available(actual)) =
        requested.terminal().unwrap().outcome()
    else {
        panic!("direct declaration-import terminal failed: {requested:?}")
    };
    assert_eq!(actual, &expected);
    assert_eq!(
        requested
            .dependencies()
            .iter()
            .map(|dependency| dependency.node.family())
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([
            "compiler.declaration-occurrence-index",
            "compiler.declaration-shell",
            "compiler.parse-module",
        ]),
        "direct import oracle must not pass through a batch/root semantic adapter"
    );
    assert_eq!(requested.dependencies().len(), 3);
    assert_eq!(requested.inputs().len(), 1);
    assert_eq!(requested.inputs()[0].input, test_import_graph_input());
}

#[test]
fn direct_semantic_keys_own_declaration_validity() {
    use crate::declaration_candidate::DeclarationCandidateCategory as Category;
    use crate::semantic_query_nucleus::{
        SemanticNucleusFailure as Failure, SemanticNucleusKey as Key, SemanticNucleusValue as Value,
    };

    let cases = [
        (
            "struct S { x: i32, x: i64 }",
            Category::Struct,
            "S",
            "duplicate-field",
        ),
        ("enum E { A, A }", Category::Enum, "E", "duplicate-variant"),
        (
            "@copy linear struct L { x: i32 }",
            Category::Struct,
            "L",
            "linear-copy",
        ),
    ];
    for (source_text, category, name, expected) in cases {
        let source = source_snapshot(&[(1, "/main.rue", "main.rue", source_text)], 1);
        let module = ModuleId::from_logical_path("main.rue").unwrap();
        let mut database = RevisionedQueryDatabase::default();
        let revision =
            database.source_revision(&crate::session::ExactSourceInput::new(&source), &source);
        let declaration = declaration_candidate(&database, revision, &module, category, name);
        let value = request_semantic_nucleus(
            &database,
            revision,
            Key::Signature(crate::semantic_query_nucleus::DeclarationSemanticQueryKey {
                declaration,
                configuration: semantic_configuration(),
            }),
        );
        let valid = matches!(
            (&*expected, &value),
            (
                "duplicate-field",
                Value::Failure(Failure::Diagnostic(
                    rue_error::ErrorKind::DuplicateField { .. }
                ))
            ) | (
                "duplicate-variant",
                Value::Failure(Failure::Diagnostic(
                    rue_error::ErrorKind::DuplicateVariant { .. }
                ))
            ) | (
                "linear-copy",
                Value::Failure(Failure::Diagnostic(rue_error::ErrorKind::LinearStructCopy(
                    _
                )))
            )
        );
        assert!(valid, "direct signature did not own {expected}: {value:?}");
    }

    for (source_text, name, expected) in [
        (
            "drop fn Missing(self) {}",
            "Missing",
            "unknown-destructor-owner",
        ),
        (
            "struct S {} drop fn S(self) {} drop fn S(self) {}",
            "S",
            "duplicate-destructor",
        ),
    ] {
        let source = source_snapshot(&[(1, "/main.rue", "main.rue", source_text)], 1);
        let module = ModuleId::from_logical_path("main.rue").unwrap();
        let mut database = RevisionedQueryDatabase::default();
        let revision =
            database.source_revision(&crate::session::ExactSourceInput::new(&source), &source);
        let declaration =
            declaration_candidate(&database, revision, &module, Category::Destructor, name);
        let query = crate::semantic_query_nucleus::DeclarationSemanticQueryKey {
            declaration,
            configuration: semantic_configuration(),
        };
        for key in [Key::Signature(query.clone()), Key::Identity(query.clone())] {
            let value = request_semantic_nucleus(&database, revision, key);
            let valid = match expected {
                "unknown-destructor-owner" => matches!(
                    &value,
                    Value::Failure(Failure::Diagnostic(
                        rue_error::ErrorKind::DestructorUnknownType { .. }
                    ))
                ),
                "duplicate-destructor" => matches!(
                    &value,
                    Value::Failure(Failure::DiagnosticAtDeclaration {
                        kind: rue_error::ErrorKind::DuplicateDestructor { .. },
                        declaration,
                    }) if declaration.duplicate_discriminator == 1
                ),
                _ => false,
            };
            assert!(
                valid,
                "direct destructor terminal did not own {expected}: {value:?}"
            );
        }
    }

    for (source_text, category, name) in [
        (
            "struct S { fn m(self, a: i32, a: i32) {} }",
            Category::Method,
            "m",
        ),
        (
            "struct S { fn make(a: i32, a: i32) {} }",
            Category::AssociatedFunction,
            "make",
        ),
    ] {
        let source = source_snapshot(&[(1, "/main.rue", "main.rue", source_text)], 1);
        let module = ModuleId::from_logical_path("main.rue").unwrap();
        let mut database = RevisionedQueryDatabase::default();
        let revision =
            database.source_revision(&crate::session::ExactSourceInput::new(&source), &source);
        let declaration = declaration_candidate(&database, revision, &module, category, name);
        let value = request_semantic_nucleus(
            &database,
            revision,
            Key::Signature(crate::semantic_query_nucleus::DeclarationSemanticQueryKey {
                declaration,
                configuration: semantic_configuration(),
            }),
        );
        assert!(
            matches!(
                value,
                Value::Failure(Failure::DiagnosticAtParameter {
                    kind: rue_error::ErrorKind::DuplicateParameter { .. },
                    ordinal: 1,
                })
            ),
            "direct nested signature lost its duplicate occurrence: {value:?}"
        );
    }

    for (source_text, expected_duplicate) in [
        ("const C: i32 = 1; const C: i32 = 2;", true),
        ("fn C() -> i32 { 0 } const C: i32 = 1;", false),
    ] {
        let source = source_snapshot(&[(1, "/main.rue", "main.rue", source_text)], 1);
        let module = ModuleId::from_logical_path("main.rue").unwrap();
        let mut database = RevisionedQueryDatabase::default();
        let revision =
            database.source_revision(&crate::session::ExactSourceInput::new(&source), &source);
        let declaration =
            declaration_candidate(&database, revision, &module, Category::ConstCandidate, "C");
        let value = request_semantic_nucleus(
            &database,
            revision,
            Key::ConstResolution(crate::semantic_query_nucleus::DeclarationSemanticQueryKey {
                declaration,
                configuration: semantic_configuration(),
            }),
        );
        assert!(
            if expected_duplicate {
                matches!(
                    value,
                    Value::Failure(Failure::Diagnostic(
                        rue_error::ErrorKind::DuplicateConstant { .. }
                    ))
                )
            } else {
                matches!(
                    value,
                    Value::Failure(Failure::Diagnostic(
                        rue_error::ErrorKind::DuplicateMixedKindDefinition { .. }
                    ))
                )
            },
            "direct const key did not own name validity: {value:?}"
        );
    }
}

#[test]
fn direct_const_keys_preserve_structured_evaluator_failures() {
    use crate::declaration_candidate::DeclarationCandidateCategory as Category;
    use crate::semantic_query_nucleus::{
        ConstResolutionProjection, SemanticNucleusFailure as Failure, SemanticNucleusKey as Key,
        SemanticNucleusValue as Value,
    };
    let source = source_snapshot(
        &[(
            1,
            "/main.rue",
            "main.rue",
            "struct P { x: i32 }\
                 const SIZE: i32 = @size_of(i32);\
                 const AGG: P = P { x: 1 };\
                 const ZERO: i32 = 5 / 0;\
                 const OVF: i32 = 2147483647 + 1;\
                 const LOCAL: u8 = { let y: u8 = 255; y + 1 };\
                 const TARGET: i32 = if @target_arch() == Arch.Linux { 1 } else { 0 };\
                 const BOOL: bool = true != false;",
        )],
        1,
    );
    let module = ModuleId::from_logical_path("main.rue").unwrap();
    let mut database = RevisionedQueryDatabase::default();
    let revision =
        database.source_revision(&crate::session::ExactSourceInput::new(&source), &source);
    let query = |name: &str| {
        let declaration =
            declaration_candidate(&database, revision, &module, Category::ConstCandidate, name);
        request_semantic_nucleus(
            &database,
            revision,
            Key::ConstResolution(crate::semantic_query_nucleus::DeclarationSemanticQueryKey {
                declaration,
                configuration: semantic_configuration(),
            }),
        )
    };
    assert!(matches!(
        query("SIZE"),
        Value::Failure(Failure::Diagnostic(
            rue_error::ErrorKind::ConstExprNotSupported { .. }
        ))
    ));
    assert!(matches!(
        query("AGG"),
        Value::Failure(Failure::Diagnostic(
            rue_error::ErrorKind::ConstExprNotSupported { .. }
        ))
    ));
    for name in ["ZERO", "OVF", "LOCAL"] {
        assert!(matches!(
            query(name),
            Value::Failure(Failure::Diagnostic(
                rue_error::ErrorKind::ComptimeEvaluationFailed { .. }
            ))
        ));
    }
    assert!(matches!(
        query("TARGET"),
        Value::Failure(Failure::Diagnostic(
            rue_error::ErrorKind::UnknownVariant { .. }
        ))
    ));
    assert!(matches!(
        query("BOOL"),
        Value::ConstResolution(ConstResolutionProjection::Value {
            value,
            ..
        }) if matches!(*value, crate::durable_semantics::DurableConstValue::Bool(true))
    ));
}

#[test]
fn direct_const_named_array_length_uses_the_live_evaluator_policy() {
    use crate::declaration_candidate::DeclarationCandidateCategory as Category;
    use crate::semantic_query_nucleus::{
        ConstResolutionProjection, SemanticNucleusFailure as Failure, SemanticNucleusKey as Key,
        SemanticNucleusValue as Value,
    };

    let source = source_snapshot(
        &[(
            1,
            "/main.rue",
            "main.rue",
            "const N: i32 = 3;\n\
                 const GLOBAL: type = [i32; N];\n\
                 fn Shadow(comptime N: i32) -> type { [i32; N] }\n\
                 const LOCAL: type = Shadow(4);\n\
                     const NEG_N: i32 = -1;\n\
                     const NEG: type = [i32; NEG_N];\n\
                     const HUGE_N: i32 = 1;\n\
                     const HUGE: type = [i32; HUGE_N];\n\
                     fn main() -> i32 { 0 }\n",
        )],
        1,
    );
    let module = ModuleId::from_logical_path("main.rue").unwrap();
    let mut database = RevisionedQueryDatabase::default();
    let revision =
        database.source_revision(&crate::session::ExactSourceInput::new(&source), &source);
    let query = |name: &str| {
        let declaration =
            declaration_candidate(&database, revision, &module, Category::ConstCandidate, name);
        request_semantic_nucleus(
            &database,
            revision,
            Key::ConstResolution(crate::semantic_query_nucleus::DeclarationSemanticQueryKey {
                declaration,
                configuration: semantic_configuration(),
            }),
        )
    };
    let expected = crate::durable_semantics::DurableConstValue::Type(
        crate::durable_semantics::DurableType::Array {
            element: Arc::new(crate::durable_semantics::DurableType::I32),
            len: 3,
        },
    );
    let (global, global_attempt) = {
        let declaration = declaration_candidate(
            &database,
            revision,
            &module,
            Category::ConstCandidate,
            "GLOBAL",
        );
        request_semantic_nucleus_observed(
            &database,
            revision,
            Key::ConstResolution(crate::semantic_query_nucleus::DeclarationSemanticQueryKey {
                declaration,
                configuration: semantic_configuration(),
            }),
        )
    };
    assert!(matches!(
        global,
        Value::ConstResolution(ConstResolutionProjection::Value { value, .. })
            if *value == expected
    ));
    assert_eq!(
        global_attempt
            .dependencies()
            .iter()
            .filter(|dependency| {
                dependency.node.family() == "compiler.semantic-nucleus"
                    && dependency
                        .node
                        .key()
                        .contains("const:8:main.rue:ConstCandidate:1:N:")
            })
            .count(),
        1,
        "unbound global named length must observe exactly one const dependency: {:?}",
        global_attempt.dependencies()
    );
    let expected_local = crate::durable_semantics::DurableConstValue::Type(
        crate::durable_semantics::DurableType::Array {
            element: Arc::new(crate::durable_semantics::DurableType::I32),
            len: 4,
        },
    );
    assert!(matches!(
        query("LOCAL"),
        Value::ConstResolution(ConstResolutionProjection::Value { value, .. })
            if *value == expected_local
    ));
    assert!(matches!(
        query("NEG"),
        Value::Failure(Failure::Diagnostic(
            rue_error::ErrorKind::InvalidArrayLength { reason }
        )) if reason == "array length expression 'NEG_N' is negative or too large"
    ));
    // The source language has no integer literal wider than u64. Inject
    // the out-of-range semantic value after the real evaluator resolves
    // HUGE_N, so this still exercises the live ArrayRepeat consumer.
    let _huge_override =
        TestSemanticComptimeArrayLengthOverrideGuard::set(Some(i128::from(u64::MAX) + 1));
    let huge_declaration = declaration_candidate(
        &database,
        revision,
        &module,
        Category::ConstCandidate,
        "HUGE",
    );
    let (huge, huge_attempt) = request_semantic_nucleus_observed(
        &database,
        revision,
        Key::ConstResolution(crate::semantic_query_nucleus::DeclarationSemanticQueryKey {
            declaration: huge_declaration,
            configuration: semantic_configuration(),
        }),
    );
    assert!(matches!(
        huge,
        Value::Failure(Failure::Diagnostic(
            rue_error::ErrorKind::InvalidArrayLength { reason }
        )) if reason == "array length expression 'HUGE_N' is negative or too large"
    ));
    assert_eq!(
        huge_attempt
            .dependencies()
            .iter()
            .filter(|dependency| {
                dependency
                    .node
                    .key()
                    .contains("const:8:main.rue:ConstCandidate:6:HUGE_N:")
            })
            .count(),
        1,
        "too-large conversion must follow the live HUGE_N lookup: {:?}",
        huge_attempt.dependencies()
    );
}

#[test]
fn direct_const_named_array_length_live_local_kinds_do_not_fall_through() {
    use crate::declaration_candidate::DeclarationCandidateCategory as Category;
    use crate::semantic_query_nucleus::{
        SemanticNucleusFailure as Failure, SemanticNucleusKey as Key, SemanticNucleusValue as Value,
    };

    let source = source_snapshot(
        &[
            (
                1,
                "/main.rue",
                "main.rue",
                "const N: i32 = 3;\n\
                     fn BoolShadow(comptime N: bool) -> type { [i32; N] }\n\
                     fn TypeShadow(comptime N: type) -> type { [i32; N] }\n\
                     const BOOL: type = BoolShadow(true);\n\
                     const TYPE: type = TypeShadow(i32);\n\
                     const MODULE: type = { let N = @import(\"dep.rue\"); [i32; N] };\n\
                     const TARGET: type = { let N = @target_arch(); [i32; N] };\n\
                     const CYCLE_A: i32 = CYCLE_B;\n\
                     const CYCLE_B: i32 = CYCLE_A;\n\
                     const CYCLE: type = [i32; CYCLE_A];\n\
                     fn main() -> i32 { 0 }\n",
            ),
            (2, "/dep.rue", "dep.rue", "pub const VALUE: i32 = 1;\n"),
        ],
        1,
    );
    let module = ModuleId::from_logical_path("main.rue").unwrap();
    let mut database = RevisionedQueryDatabase::default();
    let revision =
        database.source_revision(&crate::session::ExactSourceInput::new(&source), &source);
    let discovered = crate::test_support::test_import_graph(&source).unwrap();
    database.adopt_test_import_graph_for_revision(revision, discovered);
    let revision = database.current_semantic_revision().unwrap();
    let query = |name: &str| {
        let declaration =
            declaration_candidate(&database, revision, &module, Category::ConstCandidate, name);
        request_semantic_nucleus_observed(
            &database,
            revision,
            Key::ConstResolution(crate::semantic_query_nucleus::DeclarationSemanticQueryKey {
                declaration,
                configuration: semantic_configuration(),
            }),
        )
    };

    let expected = [
        (
            "BOOL",
            Failure::Diagnostic(rue_error::ErrorKind::InvalidArrayLength {
                reason: "array length expression 'N' is not an integer".into(),
            }),
        ),
        (
            "TYPE",
            Failure::Diagnostic(rue_error::ErrorKind::InvalidArrayLength {
                reason: "array length expression 'N' is not an integer".into(),
            }),
        ),
        (
            "MODULE",
            Failure::Resolution("module used where a value is required".into()),
        ),
        (
            "TARGET",
            Failure::Resolution(
                "target descriptor used where a durable const value is required".into(),
            ),
        ),
    ];
    for (name, expected) in expected {
        let (value, attempt) = query(name);
        assert!(
            matches!(value, Value::Failure(ref actual) if actual == &expected),
            "{name} must preserve the exact live local-kind failure: {value:?}"
        );
        assert!(
            attempt
                .dependencies()
                .iter()
                .all(|dependency| !dependency.node.key().contains(":ConstCandidate:1:N:")),
            "{name} must not query the same-named global length: {:?}",
            attempt.dependencies()
        );
    }

    let (cycle, cycle_attempt) = query("CYCLE");
    assert!(
        matches!(cycle, Value::Failure(Failure::Cycle(_))),
        "live evaluator cycle must remain an explicit terminal: {cycle:?}"
    );
    assert!(
        cycle_attempt
            .dependencies()
            .iter()
            .any(|dependency| dependency.node.key().contains("CYCLE_A")),
        "cycle terminal should retain the exact named-length observation: {:?}",
        cycle_attempt.dependencies()
    );
}

#[test]
fn live_evaluator_named_global_cancellation_preserves_abort_channel() {
    use crate::declaration_candidate::DeclarationCandidateCategory as Category;
    use crate::semantic_query_nucleus::SemanticNucleusKey as Key;

    let source = source_snapshot(
        &[(
            1,
            "/main.rue",
            "main.rue",
            "const GLOBAL: i32 = 3;\n\
                 const CANCELED: type = [i32; GLOBAL];\n\
                 fn main() -> i32 { 0 }\n",
        )],
        1,
    );
    let module = ModuleId::from_logical_path("main.rue").unwrap();
    let mut database = RevisionedQueryDatabase::default();
    let revision =
        database.source_revision(&crate::session::ExactSourceInput::new(&source), &source);
    let declaration = declaration_candidate(
        &database,
        revision,
        &module,
        Category::ConstCandidate,
        "CANCELED",
    );
    let checks_before = TEST_NAMED_VALUE_CHECKS.with(std::cell::Cell::get);
    let _cancel_named_value = TestSemanticComptimeNamedValueCancelGuard::set(true);
    let attempt = database.runtime.request_registered(
        &database.semantic_nucleus,
        revision,
        Key::ConstResolution(crate::semantic_query_nucleus::DeclarationSemanticQueryKey {
            declaration,
            configuration: semantic_configuration(),
        }),
        CancellationToken::new(),
    );
    let checks_after = TEST_NAMED_VALUE_CHECKS.with(std::cell::Cell::get);
    assert!(
        checks_after > checks_before,
        "the live evaluator must reach named-value evaluation before cancellation"
    );
    assert!(
        matches!(attempt.abort(), Some(QueryAbort::Canceled)),
        "named-global cancellation must remain the exact query abort: {:?}",
        attempt.abort()
    );
    assert!(
        attempt
            .dependencies()
            .iter()
            .all(|dependency| !dependency.node.key().contains(":ConstCandidate:1:GLOBAL:")),
        "cancellation before named-value conversion must publish no global dependency: {:?}",
        attempt.dependencies()
    );
}

#[test]
fn live_type_provider_named_array_length_cases_preserve_substitution_and_lookup_channels() {
    use crate::declaration_candidate::DeclarationCandidateCategory as Category;
    use crate::semantic_query_nucleus::{
        SemanticNucleusFailure as Failure, SemanticNucleusKey as Key, SemanticNucleusValue as Value,
    };

    let source = source_snapshot(
        &[(
            1,
            "/main.rue",
            "main.rue",
            "const N: i32 = 3;\n\
                 fn Global(value: [i32; N]) -> i32 { value[0] }\n\
                 fn Good(comptime N: i32, value: [i32; N]) -> i32 { value[0] }\n\
                 fn Bad(comptime N: bool, value: [i32; N]) -> i32 { value[0] }\n\
                 const CYCLE_A: i32 = CYCLE_B;\n\
                 const CYCLE_B: i32 = CYCLE_A;\n\
                 fn Cycle(value: [i32; CYCLE_A]) -> i32 { value[0] }\n\
                 fn main() -> i32 { 0 }\n",
        )],
        1,
    );
    let module = ModuleId::from_logical_path("main.rue").unwrap();
    let mut database = RevisionedQueryDatabase::default();
    let revision =
        database.source_revision(&crate::session::ExactSourceInput::new(&source), &source);
    let signature = |name: &str| {
        let declaration =
            declaration_candidate(&database, revision, &module, Category::Function, name);
        request_semantic_nucleus_observed(
            &database,
            revision,
            Key::Signature(crate::semantic_query_nucleus::DeclarationSemanticQueryKey {
                declaration,
                configuration: semantic_configuration(),
            }),
        )
    };

    let (global, global_attempt) = signature("Global");
    assert!(
        matches!(global, Value::Signature(_)),
        "unbound global integer length must resolve through the live provider: {global:?}"
    );
    assert_eq!(
        global_attempt
            .dependencies()
            .iter()
            .filter(|dependency| {
                dependency.node.family() == "compiler.semantic-nucleus"
                    && dependency
                        .node
                        .key()
                        .contains("const:8:main.rue:ConstCandidate:1:N:")
            })
            .count(),
        1,
        "global length must observe exactly one const dependency: {:?}",
        global_attempt.dependencies()
    );

    let (good, good_attempt) = signature("Good");
    assert!(
        matches!(good, Value::Signature(_)),
        "deferred integer substitution must remain a live provider result: {good:?}"
    );
    assert!(
        !good_attempt
            .dependencies()
            .iter()
            .any(|dependency| dependency.node.key().contains(":ConstCandidate:1:N:")),
        "integer substitution must not fall through to the global const: {:?}",
        good_attempt.dependencies()
    );

    let (bad, bad_attempt) = signature("Bad");
    let bad_debug = format!("{bad:?}");
    assert!(
        bad_debug.contains("non-integer type") || bad_debug.contains("not an integer"),
        "non-integer substitution must preserve the live provider diagnostic: {bad_debug}"
    );
    assert!(
        !bad_attempt
            .dependencies()
            .iter()
            .any(|dependency| dependency.node.key().contains(":ConstCandidate:1:N:")),
        "non-integer substitution must not query the global const: {:?}",
        bad_attempt.dependencies()
    );

    let (cycle, cycle_attempt) = signature("Cycle");
    let cycle_debug = format!("{cycle:?}");
    assert!(
        matches!(cycle, Value::Failure(Failure::Cycle(_))),
        "provider cycle/abort must remain a terminal rather than a global fallback: {cycle_debug}"
    );
    assert!(
        cycle_attempt
            .dependencies()
            .iter()
            .all(|dependency| !dependency.node.key().contains(":ConstCandidate:1:N:")),
        "cycle failure must not perform an extra unrelated global lookup: {:?}",
        cycle_attempt.dependencies()
    );
}

#[test]
fn live_type_provider_array_length_adapter_preserves_integer_boundaries_without_rir() {
    use crate::durable_semantics::{DurableConstValue as V, DurableType as T};
    use crate::semantic_query_nucleus::SemanticNucleusFailure as Failure;
    use rue_air::SemanticTypeSyntaxProvider;

    let source = source_snapshot(
        &[(
            1,
            "/main.rue",
            "main.rue",
            "const HUGE: u64 = 1;\n\
                 const BOOL: i32 = 2;\n\
                 const NEG: i32 = 3;\n\
                 fn main() -> i32 { 0 }\n",
        )],
        1,
    );
    let module = ModuleId::from_logical_path("main.rue").unwrap();
    let mut database = RevisionedQueryDatabase::default();
    let revision =
        database.source_revision(&crate::session::ExactSourceInput::new(&source), &source);
    let captured = std::cell::RefCell::new(None);
    let attempt = database.runtime.query(
        &database.provider_probe,
        revision,
        ProviderProbeKey {
            label: Arc::from("live-array-length-boundaries"),
        },
        CancellationToken::new(),
        |context| {
            let mut value_substitutions = BTreeMap::new();
            value_substitutions.insert(Arc::from("HUGE"), V::Integer(i128::from(u64::MAX) + 1));
            value_substitutions.insert(Arc::from("BOOL"), V::Bool(true));
            value_substitutions.insert(Arc::from("NEG"), V::Integer(-1));
            let mut deferred_value_parameters = BTreeMap::new();
            deferred_value_parameters.insert(Arc::from("DEFERRED_I"), T::I32);
            deferred_value_parameters.insert(Arc::from("DEFERRED_B"), T::Bool);
            let dependency_source = StableDefinitionKey::from_stable_parts(
                module.clone(),
                crate::StableDefinitionNamespace::Value,
                crate::StableDefinitionKind::Function,
                "probe",
                None,
            );
            let mut provider = SemanticNucleusTypeProvider {
                context,
                family: &database.semantic_nucleus,
                shells: &database.declaration_shells,
                names: &database.lookup_names,
                configuration: semantic_configuration(),
                substitutions: BTreeMap::new(),
                value_substitutions,
                deferred_value_parameters,
                anonymous_nominals: BTreeMap::new(),
                dependency_source,
                dependency_kind: rue_air::DeclarationTypeDependencyKind::Signature,
                dependencies: BTreeSet::new(),
                deferred_ownership: BTreeSet::new(),
                ownership_properties: BTreeMap::new(),
            };
            let mut resolve = |name: &'static str| {
                <SemanticNucleusTypeProvider<'_> as SemanticTypeSyntaxProvider<
                    ModuleId,
                    ModuleId,
                    StableDefinitionKey,
                    StableDefinitionKey,
                    Arc<str>,
                    crate::DurableType,
                    crate::DurableConstValue,
                >>::resolve_array_length(
                    &mut provider,
                    &module,
                    rue_air::SemanticValueSyntax::Name(name),
                )
            };
            let values = (
                resolve("HUGE"),
                resolve("BOOL"),
                resolve("NEG"),
                resolve("DEFERRED_I"),
                resolve("DEFERRED_B"),
                provider.dependencies.clone(),
            );
            *captured.borrow_mut() = Some(values);
            Ok(rue_query::QueryOutput::success(ProviderProbeValue))
        },
    );
    let attempt = attempt.expect("live provider probe must publish");
    assert!(
        matches!(attempt.outcome(), rue_query::QueryOutcome::Success(_)),
        "live provider probe must publish"
    );
    let (huge, bool_value, negative, deferred_integer, deferred_bool, dependencies) = captured
        .into_inner()
        .expect("provider probe captured its values");
    assert!(matches!(
        huge,
        Err(rue_air::SemanticProviderError::Failure(Failure::Diagnostic(
            rue_error::ErrorKind::InvalidArrayLength { ref reason }
        ))) if reason == "array length 'HUGE' (18446744073709551616) is too large"
    ));
    assert!(matches!(
        bool_value,
        Err(rue_air::SemanticProviderError::Failure(Failure::Resolution(reason)))
            if reason.as_ref() == "array length `BOOL` is not an integer"
    ));
    assert!(matches!(
        negative,
        Err(rue_air::SemanticProviderError::Failure(Failure::Diagnostic(
            rue_error::ErrorKind::InvalidArrayLength { ref reason }
        ))) if reason == "array length 'NEG' is negative (-1)"
    ));
    assert!(matches!(deferred_integer, Ok(None)));
    assert!(matches!(
        deferred_bool,
        Err(rue_air::SemanticProviderError::Failure(Failure::Diagnostic(
            rue_error::ErrorKind::InvalidArrayLength { ref reason }
        ))) if reason == "array length expression 'DEFERRED_B' has non-integer type bool"
    ));
    assert!(
        dependencies.is_empty(),
        "all lexical substitutions must avoid global lookup/dependency: {dependencies:?}"
    );

    let cancellation = CancellationToken::new();
    let cancel_in_closure = cancellation.clone();
    let canceled_value = std::cell::RefCell::new(None);
    let _canceled_attempt = database.runtime.query(
        &database.provider_probe,
        revision,
        ProviderProbeKey {
            label: Arc::from("live-array-length-canceled-global"),
        },
        cancellation,
        |context| {
            let dependency_source = StableDefinitionKey::from_stable_parts(
                module.clone(),
                crate::StableDefinitionNamespace::Value,
                crate::StableDefinitionKind::Function,
                "canceled_probe",
                None,
            );
            let mut provider = SemanticNucleusTypeProvider {
                context,
                family: &database.semantic_nucleus,
                shells: &database.declaration_shells,
                names: &database.lookup_names,
                configuration: semantic_configuration(),
                substitutions: BTreeMap::new(),
                value_substitutions: BTreeMap::new(),
                deferred_value_parameters: BTreeMap::new(),
                anonymous_nominals: BTreeMap::new(),
                dependency_source,
                dependency_kind: rue_air::DeclarationTypeDependencyKind::Signature,
                dependencies: BTreeSet::new(),
                deferred_ownership: BTreeSet::new(),
                ownership_properties: BTreeMap::new(),
            };
            cancel_in_closure.cancel();
            let result = <SemanticNucleusTypeProvider<'_> as SemanticTypeSyntaxProvider<
                ModuleId,
                ModuleId,
                StableDefinitionKey,
                StableDefinitionKey,
                Arc<str>,
                crate::DurableType,
                crate::DurableConstValue,
            >>::resolve_array_length(
                &mut provider,
                &module,
                rue_air::SemanticValueSyntax::Name("UNBOUND"),
            );
            *canceled_value.borrow_mut() = Some(result);
            Ok(rue_query::QueryOutput::success(ProviderProbeValue))
        },
    );
    assert!(matches!(
        canceled_value.into_inner(),
        Some(Err(rue_air::SemanticProviderError::Abort(
            QueryAbort::Canceled
        )))
    ));
}

#[test]
#[should_panic(expected = "controlled operation panic")]
fn restored_state_kernel_restores_exact_state_when_operation_panics() {
    use std::cell::RefCell;
    use std::rc::Rc;

    struct RestorationAssertion {
        state: Rc<RefCell<BTreeMap<Arc<str>, i32>>>,
        expected: BTreeMap<Arc<str>, i32>,
    }

    impl Drop for RestorationAssertion {
        fn drop(&mut self) {
            assert_eq!(*self.state.borrow(), self.expected);
        }
    }

    let expected = BTreeMap::from([(Arc::from("OLD"), 7_i32)]);
    let state = Rc::new(RefCell::new(expected.clone()));
    let _assertion = RestorationAssertion {
        state: Rc::clone(&state),
        expected,
    };
    let mut active = Rc::clone(&state);
    super::with_restored_state(
        &mut active,
        |state| {
            std::mem::replace(
                &mut *state.borrow_mut(),
                BTreeMap::from([(Arc::from("TRANSIENT"), 9)]),
            )
        },
        |_state| -> () { panic!("controlled operation panic") },
        |state, old| *state.borrow_mut() = old,
    );
}

#[test]
fn live_root_authority_resolves_keyed_substitutions_and_restores_provider_state() {
    use crate::body_query::{DurableComptimeProgramPlan, OwnedComptimeProgramCore};
    use crate::durable_semantics::{DurableConstValue as V, DurableType as T};
    use rue_rir::InstData;

    let snapshot = SourceSnapshot::single(
            "root-type-seam.rue",
            "struct NamedType {} fn target(value: T, count: [i32; N], bad: [i32; MISSING], named: [NamedType; N2]) -> i32 { 1 }",
        )
        .unwrap();
    let module = crate::parsed_modules::parse_source_snapshot_modules(&snapshot)
        .unwrap()
        .modules()[0]
        .clone();
    let candidate = module
        .definitions()
        .declaration_keys_in_source_order()
        .find(|candidate| candidate.name.as_ref() == "target")
        .unwrap()
        .clone();
    let artifacts =
        crate::canonical_lower::lower_parsed_declaration_body_plan(&module, &candidate, || Ok(()))
            .unwrap();
    let configuration = semantic_configuration();
    let program_key = crate::body_query::DurableComptimeProgramKey {
        declaration: StableDefinitionKey::from_stable_parts(
            candidate.module.clone(),
            crate::StableDefinitionNamespace::Value,
            crate::StableDefinitionKind::Function,
            "target",
            None,
        ),
        configuration: configuration.clone(),
    };
    let core = OwnedComptimeProgramCore::from_callable_body_plan_without_imports(
        DurableComptimeProgramPlan {
            key: program_key.clone(),
            candidate: candidate.clone(),
        },
        &artifacts,
        || Ok(()),
    )
    .unwrap();
    // Build a deliberately qualified enum node directly in RIR.  The
    // source parser's dotted expression is a FieldGet until semantic
    // resolution, so a source-only fixture cannot exercise AIR's
    // pre-child EnumVariant admission contract.
    let mut qualified_editor = rue_rir::RirEditor::new();
    let qualified_interner = lasso::ThreadedRodeo::new();
    let module_symbol = qualified_interner.get_or_intern("module");
    let type_symbol = qualified_interner.get_or_intern("Arch");
    let variant_symbol = qualified_interner.get_or_intern("X86_64");
    let module_ref = qualified_editor.add_inst(rue_rir::Inst {
        data: InstData::VarRef {
            name: module_symbol,
            anchor: None,
        },
        span: rue_span::Span::new(0, 6),
    });
    let qualified_body = qualified_editor.add_inst(rue_rir::Inst {
        data: InstData::EnumVariant {
            module: Some(module_ref),
            type_name: type_symbol,
            variant: variant_symbol,
        },
        span: rue_span::Span::new(0, 14),
    });
    let qualified_rir = rue_rir::ValidatedRir::finish(
        qualified_editor,
        &rue_rir::RirValidationContext {
            symbol_count: qualified_interner.len(),
            source_lengths: &[(FileId::DEFAULT, 32)],
        },
    )
    .unwrap();
    let qualified_symbols: Arc<[Arc<str>]> = (0..qualified_interner.len())
        .map(|index| {
            Arc::from(
                qualified_interner
                    .resolve(&lasso::Spur::try_from_usize(index).unwrap())
                    .to_owned(),
            )
        })
        .collect();
    let qualified_key = crate::body_query::DurableComptimeProgramKey {
        declaration: StableDefinitionKey::from_stable_parts(
            candidate.module.clone(),
            crate::StableDefinitionNamespace::Value,
            crate::StableDefinitionKind::Function,
            "qualified_test",
            None,
        ),
        configuration: configuration.clone(),
    };
    let qualified_core = OwnedComptimeProgramCore::from_test_rir(
        DurableComptimeProgramPlan {
            key: qualified_key.clone(),
            candidate: candidate.clone(),
        },
        qualified_rir,
        qualified_symbols,
        qualified_body,
        qualified_body,
    );
    let root = core.callable().unwrap().root;
    let (type_syntax, value_syntax, abort_syntax, named_syntax) = match &core.rir.get(root).data {
        InstData::FnDecl { params, .. } => {
            let params = core.rir.params(params).iter();
            let params = params.collect::<Vec<_>>();
            (params[0].ty, params[1].ty, params[2].ty, params[3].ty)
        }
        other => panic!("callable core has unexpected root: {other:?}"),
    };

    let mut database = RevisionedQueryDatabase::default();
    let revision =
        database.source_revision(&crate::session::ExactSourceInput::new(&snapshot), &snapshot);
    let cancellation = CancellationToken::new();
    let cancel_in_closure = cancellation.clone();
    let captured = std::cell::RefCell::new(None);
    let attempt = database.runtime.query(
        &database.provider_probe,
        revision,
        ProviderProbeKey {
            label: Arc::from("live-root-authority-type-substitutions"),
        },
        cancellation,
        |context| {
            let dependency_source = program_key.declaration.clone();
            let provider = SemanticNucleusTypeProvider {
                context,
                family: &database.semantic_nucleus,
                shells: &database.declaration_shells,
                names: &database.lookup_names,
                configuration: configuration.clone(),
                substitutions: BTreeMap::from([(Arc::from("OLD"), T::I8)]),
                value_substitutions: BTreeMap::from([(Arc::from("OLD"), V::Integer(7))]),
                deferred_value_parameters: BTreeMap::new(),
                anonymous_nominals: BTreeMap::new(),
                dependency_source,
                dependency_kind: rue_air::DeclarationTypeDependencyKind::Signature,
                dependencies: BTreeSet::new(),
                deferred_ownership: BTreeSet::new(),
                ownership_properties: BTreeMap::new(),
            };
            let session = crate::durable_comptime::DurableComptimeSession::new(
                program_key.declaration.clone(),
                candidate.clone(),
            )
            .unwrap();
            let mut authority = DurableComptimeRootAuthority {
                provider,
                imports: database.declaration_imports.clone(),
                session,
                foreign: DurableComptimeForeignQueryAuthority {
                    context,
                    semantic_nucleus: &database.semantic_nucleus,
                    declaration_body_plan_artifacts: &database.declaration_body_plan_artifacts,
                    configuration: &configuration,
                },
            };
            authority.session.register_program(&core).unwrap();
            authority.session.register_program(&qualified_core).unwrap();

            // Exercise the canonical AIR host with exact registered RIR.
            // Production roots use the same engine/host path below.
            let callable = core.callable().expect("callable fixture").clone();
            let mut host = crate::durable_comptime::DurableComptimeHost::new(&mut authority);
            let mut env = rue_air::ComptimeEnv::new();
            let outcome = rue_air::ComptimeEngine::new(&mut host).evaluate(
                rue_air::ComptimeFrame::expression(program_key.clone(), callable.body),
                &mut env,
            );
            assert!(matches!(
                outcome,
                rue_air::ComptimeOutcome::Known(
                    crate::durable_comptime::EvaluatedSemanticConst::Value(value)
                ) if matches!(value.value, crate::durable_semantics::DurableConstValue::Integer(1))
            ));

            drop(host);

            let expected_types = BTreeMap::from([(Arc::from("OLD"), T::I8)]);
            let expected_values = BTreeMap::from([(Arc::from("OLD"), V::Integer(7))]);
            let wrong_program = crate::body_query::DurableComptimeProgramKey {
                declaration: StableDefinitionKey::from_stable_parts(
                    candidate.module.clone(),
                    crate::StableDefinitionNamespace::Value,
                    crate::StableDefinitionKind::Function,
                    "wrong",
                    None,
                ),
                configuration: configuration.clone(),
            };
            let (type_result, value_result, restored_after_success) = {
                let mut services =
                    crate::durable_comptime::DurableComptimeServices::new(&mut authority);
                let type_result = services
                    .resolve_type_syntax_with_substitutions(
                        &program_key,
                        type_syntax,
                        &[(Arc::from("T"), T::I64)],
                        &[(Arc::from("T"), V::Integer(9))],
                    )
                    .unwrap();
                let value_result = services
                    .resolve_type_syntax_with_substitutions(
                        &program_key,
                        value_syntax,
                        &[],
                        &[(Arc::from("N"), V::Integer(3))],
                    )
                    .unwrap();
                let restored = authority.provider.substitutions == expected_types
                    && authority.provider.value_substitutions == expected_values;
                (type_result, value_result, restored)
            };

            let provider_failure = {
                let mut services =
                    crate::durable_comptime::DurableComptimeServices::new(&mut authority);
                services.resolve_type_syntax_with_substitutions(
                    &wrong_program,
                    type_syntax,
                    &[(Arc::from("T"), T::U8)],
                    &[(Arc::from("N"), V::Integer(4))],
                )
            };
            assert!(matches!(
                &provider_failure,
                Err(rue_air::SemanticTypeSyntaxError::ProviderFailure(_))
            ));
            assert_eq!(authority.provider.substitutions, expected_types);
            assert_eq!(authority.provider.value_substitutions, expected_values);

            let dependencies_before = authority.provider.dependencies.clone();
            let named_result = {
                let mut services =
                    crate::durable_comptime::DurableComptimeServices::new(&mut authority);
                services.resolve_type_syntax_with_substitutions(
                    &program_key,
                    named_syntax,
                    &[],
                    &[(Arc::from("N2"), V::Integer(2))],
                )
            };
            assert!(matches!(
                &named_result,
                Ok(T::Array { element, len: 2 })
                    if matches!(element.as_ref(), T::Nominal(key) if key.name() == "NamedType")
            ));
            let new_dependencies = authority
                .provider
                .dependencies
                .difference(&dependencies_before)
                .cloned()
                .collect::<BTreeSet<_>>();
            assert_eq!(new_dependencies.len(), 1);
            assert!(new_dependencies.iter().all(|dependency| matches!(
                &dependency.target,
                crate::semantic_query_nucleus::SemanticDeclarationDependencyTarget::NamedType(
                    target
                ) if target.module() == &candidate.module
                    && target.name() == "NamedType"
            )));
            assert_eq!(authority.provider.substitutions, expected_types);
            assert_eq!(authority.provider.value_substitutions, expected_values);

            // The qualified EnumVariant fixture is registered above and
            // evaluated through the canonical AIR dispatcher. Cancellation
            // is armed by the host at the admission boundary as a
            // child-evaluation tripwire: if AIR ever evaluates
            // `module_ref` before admission rejects the path, its eval
            // checkpoint must return QueryAbort::Canceled rather than the
            // expected typed semantic HostFailure.
            crate::durable_comptime::set_enum_variant_child_tripwire(Some(
                cancel_in_closure.clone(),
            ));
            let dependencies_before_qualified = authority.provider.dependencies.clone();
            let mut qualified_env = rue_air::ComptimeEnv::new();
            let mut qualified_host =
                crate::durable_comptime::DurableComptimeHost::new(&mut authority);
            let qualified_outcome = rue_air::ComptimeEngine::new(&mut qualified_host).evaluate(
                rue_air::ComptimeFrame::expression(qualified_key, qualified_body),
                &mut qualified_env,
            );
            let rue_air::ComptimeOutcome::HostFailure(qualified_failure) = qualified_outcome else {
                panic!("qualified enum should be a durable host failure: {qualified_outcome:?}");
            };
            assert!(matches!(
                qualified_failure.semantic_failure(),
                Some(crate::semantic_query_nucleus::SemanticNucleusFailure::Resolution(
                    message
                )) if message.as_ref() == "expression is not supported in declaration-time comptime"
            ));
            drop(qualified_host);
            crate::durable_comptime::set_enum_variant_child_tripwire(None);
            assert_eq!(
                authority.provider.dependencies, dependencies_before_qualified,
                "qualified enum admission must not evaluate its module child"
            );
            cancel_in_closure.cancel();
            let aborted = {
                let mut services =
                    crate::durable_comptime::DurableComptimeServices::new(&mut authority);
                services.resolve_type_syntax_with_substitutions(
                    &program_key,
                    abort_syntax,
                    &[],
                    &[],
                )
            };
            *captured.borrow_mut() = Some((
                type_result,
                value_result,
                restored_after_success,
                provider_failure,
                named_result,
                aborted,
                authority.provider.substitutions.clone(),
                authority.provider.value_substitutions.clone(),
            ));
            Ok(rue_query::QueryOutput::success(ProviderProbeValue))
        },
    );
    assert!(
        attempt.is_err(),
        "the injected cancellation should abort the probe"
    );
    let (
        type_result,
        value_result,
        restored,
        provider_failure,
        named_result,
        aborted,
        restored_types,
        restored_values,
    ) = captured.into_inner().unwrap();
    assert_eq!(type_result, T::I64);
    assert_eq!(
        value_result,
        T::Array {
            element: Arc::new(T::I32),
            len: 3,
        }
    );
    assert!(restored);
    assert!(matches!(
        &provider_failure,
        Err(rue_air::SemanticTypeSyntaxError::ProviderFailure(_))
    ));
    assert!(named_result.is_ok());
    assert!(matches!(
        aborted,
        Err(rue_air::SemanticTypeSyntaxError::ProviderAbort(
            QueryAbort::Canceled
        ))
    ));
    assert_eq!(restored_types, BTreeMap::from([(Arc::from("OLD"), T::I8)]));
    assert_eq!(
        restored_values,
        BTreeMap::from([(Arc::from("OLD"), V::Integer(7))])
    );
}

#[test]
fn production_root_authority_keyed_admission_preserves_identity_and_dependency() {
    use crate::declaration_candidate::DeclarationCandidateCategory as Category;
    use crate::semantic_query_nucleus::{
        SemanticDeclarationDependency, SemanticDeclarationDependencyTarget as Target,
        SemanticNucleusFailure as Failure,
    };

    let source = source_snapshot(
        &[
            (1, "/left.rue", "left.rue", "fn target() -> i32 { 1 }\n"),
            (2, "/right.rue", "right.rue", "fn target() -> i32 { 2 }\n"),
        ],
        1,
    );
    let left_module = ModuleId::from_logical_path("left.rue").unwrap();
    let right_module = ModuleId::from_logical_path("right.rue").unwrap();
    let mut database = RevisionedQueryDatabase::default();
    let revision =
        database.source_revision(&crate::session::ExactSourceInput::new(&source), &source);
    let left_candidate = declaration_candidate(
        &database,
        revision,
        &left_module,
        Category::Function,
        "target",
    );
    let right_candidate = declaration_candidate(
        &database,
        revision,
        &right_module,
        Category::Function,
        "target",
    );
    let configuration = semantic_configuration();
    let left_head = StableDefinitionKey::from_stable_parts(
        left_module.clone(),
        crate::StableDefinitionNamespace::Value,
        crate::StableDefinitionKind::Function,
        "target",
        None,
    );
    let right_head = StableDefinitionKey::from_stable_parts(
        right_module,
        crate::StableDefinitionNamespace::Value,
        crate::StableDefinitionKind::Function,
        "target",
        None,
    );
    let accessing_source = StableDefinitionKey::from_stable_parts(
        left_module,
        crate::StableDefinitionNamespace::Value,
        crate::StableDefinitionKind::Function,
        "caller",
        None,
    );
    let unknown_head = StableDefinitionKey::from_stable_parts(
        accessing_source.module().clone(),
        crate::StableDefinitionNamespace::Value,
        crate::StableDefinitionKind::Function,
        "missing",
        None,
    );
    let mismatched_head = StableDefinitionKey::from_stable_parts(
        accessing_source.module().clone(),
        crate::StableDefinitionNamespace::Type,
        crate::StableDefinitionKind::Function,
        "target",
        None,
    );

    let captured = std::cell::RefCell::new(None);
    let attempt = database.runtime.query(
        &database.provider_probe,
        revision,
        ProviderProbeKey {
            label: Arc::from("production-keyed-call-admission"),
        },
        CancellationToken::new(),
        |context| {
            let provider = SemanticNucleusTypeProvider {
                context,
                family: &database.semantic_nucleus,
                shells: &database.declaration_shells,
                names: &database.lookup_names,
                configuration: configuration.clone(),
                substitutions: BTreeMap::new(),
                value_substitutions: BTreeMap::new(),
                deferred_value_parameters: BTreeMap::new(),
                anonymous_nominals: BTreeMap::new(),
                dependency_source: accessing_source.clone(),
                dependency_kind: rue_air::DeclarationTypeDependencyKind::Body,
                dependencies: BTreeSet::new(),
                deferred_ownership: BTreeSet::new(),
                ownership_properties: BTreeMap::new(),
            };
            let session = crate::durable_comptime::DurableComptimeSession::new(
                left_head.clone(),
                left_candidate.clone(),
            )
            .unwrap();
            let mut authority = DurableComptimeRootAuthority {
                provider,
                imports: database.declaration_imports.clone(),
                session,
                foreign: DurableComptimeForeignQueryAuthority {
                    context,
                    semantic_nucleus: &database.semantic_nucleus,
                    declaration_body_plan_artifacts: &database.declaration_body_plan_artifacts,
                    configuration: &configuration,
                },
            };
            let first = {
                let services =
                    crate::durable_comptime::DurableComptimeServices::new(&mut authority);
                services.begin_comptime_call_admission_for_key(&accessing_source, &left_head)
            };
            let second = {
                let services =
                    crate::durable_comptime::DurableComptimeServices::new(&mut authority);
                services.begin_comptime_call_admission_for_key(&accessing_source, &right_head)
            };
            let unknown = {
                let services =
                    crate::durable_comptime::DurableComptimeServices::new(&mut authority);
                services.begin_comptime_call_admission_for_key(&accessing_source, &unknown_head)
            };
            let mismatched = {
                let services =
                    crate::durable_comptime::DurableComptimeServices::new(&mut authority);
                services.begin_comptime_call_admission_for_key(&accessing_source, &mismatched_head)
            };
            *captured.borrow_mut() = Some((first, second, unknown, mismatched));
            Ok(rue_query::QueryOutput::success(ProviderProbeValue))
        },
    );
    assert!(
        attempt.is_ok(),
        "production authority probe should complete"
    );
    let (first, second, unknown, mismatched) = captured.into_inner().unwrap();
    let first = first.unwrap();
    let second = second.unwrap();
    assert_eq!(first.candidate, left_candidate);
    assert_eq!(second.candidate, right_candidate);
    assert_eq!(first.identity.key, left_head);
    assert_eq!(second.identity.key, right_head);
    assert_eq!(first.configuration, configuration);
    assert_eq!(second.configuration, configuration);
    for (admission, head) in [(&first, &left_head), (&second, &right_head)] {
        assert_eq!(
            admission.dependency,
            SemanticDeclarationDependency {
                source: accessing_source.clone(),
                kind: rue_air::DeclarationTypeDependencyKind::Body,
                target: Target::NamedValue(head.clone()),
            }
        );
    }
    assert!(
        matches!(&unknown, Err(rue_air::SemanticProviderError::Failure(_))),
        "unexpected unknown-head result: {unknown:?}"
    );
    assert!(
        format!("{unknown:?}").contains("missing"),
        "unknown-head failure lost its requested spelling: {unknown:?}"
    );
    assert!(matches!(
        &mismatched,
        Err(rue_air::SemanticProviderError::Failure(Failure::Resolution(reason)))
            if reason.as_ref() == "comptime function identity does not match requested key"
    ));

    let cancellation = CancellationToken::new();
    let cancel_in_closure = cancellation.clone();
    let aborted = std::cell::RefCell::new(None);
    let _attempt = database.runtime.query(
        &database.provider_probe,
        revision,
        ProviderProbeKey {
            label: Arc::from("production-keyed-call-admission-abort"),
        },
        cancellation,
        |context| {
            cancel_in_closure.cancel();
            let provider = SemanticNucleusTypeProvider {
                context,
                family: &database.semantic_nucleus,
                shells: &database.declaration_shells,
                names: &database.lookup_names,
                configuration: configuration.clone(),
                substitutions: BTreeMap::new(),
                value_substitutions: BTreeMap::new(),
                deferred_value_parameters: BTreeMap::new(),
                anonymous_nominals: BTreeMap::new(),
                dependency_source: accessing_source.clone(),
                dependency_kind: rue_air::DeclarationTypeDependencyKind::Body,
                dependencies: BTreeSet::new(),
                deferred_ownership: BTreeSet::new(),
                ownership_properties: BTreeMap::new(),
            };
            let session = crate::durable_comptime::DurableComptimeSession::new(
                left_head.clone(),
                left_candidate.clone(),
            )
            .unwrap();
            let mut authority = DurableComptimeRootAuthority {
                provider,
                imports: database.declaration_imports.clone(),
                session,
                foreign: DurableComptimeForeignQueryAuthority {
                    context,
                    semantic_nucleus: &database.semantic_nucleus,
                    declaration_body_plan_artifacts: &database.declaration_body_plan_artifacts,
                    configuration: &configuration,
                },
            };
            let result = {
                let services =
                    crate::durable_comptime::DurableComptimeServices::new(&mut authority);
                services.begin_comptime_call_admission_for_key(&accessing_source, &left_head)
            };
            *aborted.borrow_mut() = Some(result);
            Ok(rue_query::QueryOutput::success(ProviderProbeValue))
        },
    );
    assert!(matches!(
        aborted.into_inner(),
        Some(Err(rue_air::SemanticProviderError::Abort(
            QueryAbort::Canceled
        )))
    ));
}

#[test]
fn durable_callable_admission_pipeline_preserves_policy_table() {
    use crate::declaration_candidate::DeclarationCandidateCategory as Category;
    use crate::semantic_query_nucleus::{
        ComptimeCallQueryKey, SemanticNucleusKey as Key, SemanticNucleusValue as Value,
    };

    let cases = [
        (
            "undefined",
            "fn outer() -> i32 { missing() }",
            "undefined comptime function `missing`",
        ),
        (
            "arity",
            "fn target(comptime x: i32) -> i32 { x } fn outer() -> i32 { target(1 / 0, 2) }",
            "wrong arity",
        ),
        (
            "borrow mode",
            "fn target(borrow x: i32) -> i32 { x } fn outer() -> i32 { target(1) }",
            "BorrowKeywordMissing",
        ),
        (
            "inout mode",
            "fn target(inout x: i32) -> i32 { x } fn outer() -> i32 { target(1) }",
            "InoutKeywordMissing",
        ),
        (
            "unexpected borrow",
            "fn target(x: i32) -> i32 { x } fn outer() -> i32 { target(borrow 1) }",
            "UnexpectedCallArgumentMode",
        ),
        (
            "unexpected inout",
            "fn target(x: i32) -> i32 { x } fn outer() -> i32 { target(inout 1) }",
            "UnexpectedCallArgumentMode",
        ),
        (
            "nullary value",
            "fn target() -> i32 { 1 } fn outer() -> i32 { target() }",
            "ConstExprNotSupported",
        ),
        (
            "mixed comptime/runtime",
            "fn target(comptime x: i32, y: i32) -> i32 { x + y } fn outer() -> i32 { target(1, 2) }",
            "ConstExprNotSupported",
        ),
    ];

    for (label, source_text, expected) in cases {
        let source = source_snapshot(&[(1, "/main.rue", "main.rue", source_text)], 1);
        let module = ModuleId::from_logical_path("main.rue").unwrap();
        let mut database = RevisionedQueryDatabase::default();
        let revision =
            database.source_revision(&crate::session::ExactSourceInput::new(&source), &source);
        let declaration =
            declaration_candidate(&database, revision, &module, Category::Function, "outer");
        let value = request_semantic_nucleus(
            &database,
            revision,
            Key::ComptimeCall(ComptimeCallQueryKey {
                declaration: crate::semantic_query_nucleus::DeclarationSemanticQueryKey {
                    declaration,
                    configuration: semantic_configuration(),
                },
                type_arguments: Arc::from([]),
                value_arguments: Arc::from([]),
            }),
        );
        let diagnostic = format!("{value:?}");
        assert!(
            diagnostic.contains(expected),
            "{label} lost its canonical admission diagnostic: {diagnostic}"
        );
    }

    let source = source_snapshot(
        &[(
            1,
            "/main.rue",
            "main.rue",
            "fn target() -> type { i32 } fn outer() -> type { target() }",
        )],
        1,
    );
    let module = ModuleId::from_logical_path("main.rue").unwrap();
    let mut database = RevisionedQueryDatabase::default();
    let revision =
        database.source_revision(&crate::session::ExactSourceInput::new(&source), &source);
    let declaration =
        declaration_candidate(&database, revision, &module, Category::Function, "outer");
    let value = request_semantic_nucleus(
        &database,
        revision,
        Key::ComptimeCall(ComptimeCallQueryKey {
            declaration: crate::semantic_query_nucleus::DeclarationSemanticQueryKey {
                declaration,
                configuration: semantic_configuration(),
            },
            type_arguments: Arc::from([]),
            value_arguments: Arc::from([]),
        }),
    );
    assert!(
        matches!(
            value,
            Value::ComptimeCall(crate::semantic_query_nucleus::ComptimeCallProjection {
                result: crate::semantic_query_nucleus::ComptimeCallResultProjection::Type(
                    crate::durable_semantics::DurableType::I32
                ),
                ..
            })
        ),
        "nullary type function should remain an admitted callable: {value:?}"
    );

    let source = source_snapshot(
        &[(
            1,
            "/main.rue",
            "main.rue",
            "fn target(comptime x: i32, comptime y: i32) -> i32 { x * 10 + y } fn outer() -> i32 { target(1, 2) }",
        )],
        1,
    );
    let mut database = RevisionedQueryDatabase::default();
    let revision =
        database.source_revision(&crate::session::ExactSourceInput::new(&source), &source);
    let declaration =
        declaration_candidate(&database, revision, &module, Category::Function, "outer");
    let target = declaration_candidate(&database, revision, &module, Category::Function, "target");
    let identity = |candidate: crate::declaration_candidate::DeclarationCandidateKey| {
        let value = request_semantic_nucleus(
            &database,
            revision,
            Key::Identity(crate::semantic_query_nucleus::DeclarationSemanticQueryKey {
                declaration: candidate,
                configuration: semantic_configuration(),
            }),
        );
        let Value::Identity(identity) = value else {
            panic!("expected callable identity, got {value:?}")
        };
        identity.key
    };
    let outer_identity = identity(declaration.clone());
    let target_identity = identity(target);
    let value = request_semantic_nucleus(
        &database,
        revision,
        Key::ComptimeCall(ComptimeCallQueryKey {
            declaration: crate::semantic_query_nucleus::DeclarationSemanticQueryKey {
                declaration,
                configuration: semantic_configuration(),
            },
            type_arguments: Arc::from([]),
            value_arguments: Arc::from([]),
        }),
    );
    assert!(
        matches!(
            value,
            Value::ComptimeCall(crate::semantic_query_nucleus::ComptimeCallProjection {
                result: crate::semantic_query_nucleus::ComptimeCallResultProjection::Value(
                    crate::durable_semantics::DurableConstValue::Integer(12)
                ),
                ref dependencies,
                ..
            })
            if dependencies.as_ref()
                == [crate::semantic_query_nucleus::SemanticDeclarationDependency {
                    source: outer_identity,
                    kind: rue_air::DeclarationTypeDependencyKind::Body,
                    target: crate::semantic_query_nucleus::SemanticDeclarationDependencyTarget::NamedValue(
                        target_identity,
                    ),
                }]
        ),
        "ordered parameters and exact published dependency should survive admission: {value:?}"
    );
}

#[test]
fn durable_named_value_projection_covers_each_lookup_kind_and_dependency() {
    use crate::declaration_candidate::DeclarationCandidateCategory as Category;
    use crate::semantic_query_nucleus::{
        ConstResolutionProjection, SemanticDeclarationDependencyTarget, SemanticNucleusKey as Key,
        SemanticNucleusValue as Value,
    };

    let source = source_snapshot(
        &[(
            1,
            "/main.rue",
            "main.rue",
            "const SCALAR: i32 = 7;\
                 fn Callable() -> i32 { 1 }\
                 struct StructValue { field: i32 }\
                 enum EnumValue { One }\
                 const INNER: i32 = 5;\
                 const SOURCE: i32 = INNER;\
                 const OUT_CHAIN: i32 = SOURCE;\
                 const OUT_SCALAR: i32 = SCALAR;\
                 const OUT_CALLABLE = Callable;\
                 const OUT_STRUCT = StructValue;\
                 const OUT_ENUM = EnumValue;\
                 const OUT_LOCAL: i32 = { let SCALAR: i32 = 9; SCALAR };\
                 const OUT_UNDEFINED: i32 = MISSING;",
        )],
        1,
    );
    let module = ModuleId::from_logical_path("main.rue").unwrap();
    let mut database = RevisionedQueryDatabase::default();
    let revision =
        database.source_revision(&crate::session::ExactSourceInput::new(&source), &source);
    let query = |name: &str| {
        let declaration =
            declaration_candidate(&database, revision, &module, Category::ConstCandidate, name);
        request_semantic_nucleus(
            &database,
            revision,
            Key::ConstResolution(crate::semantic_query_nucleus::DeclarationSemanticQueryKey {
                declaration,
                configuration: semantic_configuration(),
            }),
        )
    };
    let identity = |name: &str| {
        let declaration =
            declaration_candidate(&database, revision, &module, Category::ConstCandidate, name);
        let Value::Identity(identity) = request_semantic_nucleus(
            &database,
            revision,
            Key::Identity(crate::semantic_query_nucleus::DeclarationSemanticQueryKey {
                declaration,
                configuration: semantic_configuration(),
            }),
        ) else {
            panic!("expected an identity for {name}");
        };
        identity.key
    };

    let stable_key = |namespace, kind, name| {
        crate::StableDefinitionKey::from_stable_parts(module.clone(), namespace, kind, name, None)
    };
    let assert_value = |name: &str,
                        expected_value: crate::durable_semantics::DurableConstValue,
                        expected_type: crate::durable_semantics::DurableType,
                        expected_target: crate::StableDefinitionKey| {
        let Value::ConstResolution(ConstResolutionProjection::Value {
            value,
            ty,
            dependencies,
            ..
        }) = query(name)
        else {
            panic!("expected a durable value projection for {name}");
        };
        assert_eq!(*value, expected_value);
        assert_eq!(ty, expected_type);
        assert_eq!(dependencies.len(), 1);
        assert_eq!(dependencies[0].source, identity(name));
        assert_eq!(
            dependencies[0].kind,
            rue_air::DeclarationTypeDependencyKind::Body
        );
        assert_eq!(
            dependencies[0].target,
            SemanticDeclarationDependencyTarget::NamedValue(expected_target)
        );
    };
    assert_value(
        "OUT_SCALAR",
        crate::durable_semantics::DurableConstValue::Integer(7),
        crate::durable_semantics::DurableType::I32,
        stable_key(
            crate::StableDefinitionNamespace::Value,
            crate::StableDefinitionKind::ValueConst,
            "SCALAR",
        ),
    );
    assert_value(
        "OUT_CALLABLE",
        crate::durable_semantics::DurableConstValue::Function(stable_key(
            crate::StableDefinitionNamespace::Value,
            crate::StableDefinitionKind::Function,
            "Callable",
        )),
        crate::durable_semantics::DurableType::ComptimeType,
        stable_key(
            crate::StableDefinitionNamespace::Value,
            crate::StableDefinitionKind::Function,
            "Callable",
        ),
    );
    for (name, nominal_kind, nominal_name) in [
        (
            "OUT_STRUCT",
            crate::StableDefinitionKind::Struct,
            "StructValue",
        ),
        ("OUT_ENUM", crate::StableDefinitionKind::Enum, "EnumValue"),
    ] {
        let Value::ConstResolution(ConstResolutionProjection::Value {
            value,
            ty,
            dependencies,
            ..
        }) = query(name)
        else {
            panic!("expected a durable nominal projection for {name}");
        };
        let nominal_key = stable_key(
            crate::StableDefinitionNamespace::Type,
            nominal_kind,
            nominal_name,
        );
        assert_eq!(
            *value,
            crate::durable_semantics::DurableConstValue::Type(
                crate::durable_semantics::DurableType::Nominal(nominal_key.clone())
            )
        );
        assert_eq!(ty, crate::durable_semantics::DurableType::ComptimeType);
        assert_eq!(dependencies.len(), 1);
        assert_eq!(dependencies[0].source, identity(name));
        assert_eq!(
            dependencies[0].kind,
            rue_air::DeclarationTypeDependencyKind::Body
        );
        assert_eq!(
            dependencies[0].target,
            SemanticDeclarationDependencyTarget::NamedValue(nominal_key)
        );
    }
    assert_value(
        "OUT_CHAIN",
        crate::durable_semantics::DurableConstValue::Integer(5),
        crate::durable_semantics::DurableType::I32,
        stable_key(
            crate::StableDefinitionNamespace::Value,
            crate::StableDefinitionKind::ValueConst,
            "SOURCE",
        ),
    );
    let Value::ConstResolution(ConstResolutionProjection::Value {
        value,
        ty,
        dependencies,
        ..
    }) = query("OUT_LOCAL")
    else {
        panic!("expected a local shadowing projection");
    };
    assert_eq!(
        *value,
        crate::durable_semantics::DurableConstValue::Integer(9)
    );
    assert_eq!(ty, crate::durable_semantics::DurableType::I32);
    assert!(
        dependencies.is_empty(),
        "locals must not become named-value dependencies"
    );
    let undefined = format!("{:?}", query("OUT_UNDEFINED"));
    assert!(undefined.contains("undefined constant"), "{undefined}");
}

#[test]
fn durable_named_value_projection_preserves_real_module_binding_identity() {
    use crate::declaration_candidate::DeclarationCandidateCategory as Category;
    use crate::semantic_query_nucleus::{
        ConstResolutionProjection, SemanticNucleusKey as Key, SemanticNucleusValue as Value,
    };

    let context =
        ImportDiscoveryContext::new(902, "/project", Some("/sdk"), "test-policy").unwrap();
    let mut assembler = DiscoverySourceAssembler::new(
        context.clone(),
        "/project/main.rue",
        "/project/main.rue",
        PhysicalFileIdentity::new(1, 1),
        FileMetadataFingerprint::new(1, 2, 3),
        Arc::new(
            "const M = @import(\"dep.rue\");\
                 const OUT_MODULE = M;"
                .to_owned(),
        ),
    )
    .unwrap();
    assembler
        .add_explicit(
            "/project/dep.rue",
            "/project/dep.rue",
            PhysicalFileIdentity::new(2, 1),
            FileMetadataFingerprint::new(4, 5, 6),
            Arc::new("const INNER: i32 = 1;".to_owned()),
        )
        .unwrap();
    let mut database = RevisionedQueryDatabase::default();
    let (snapshot, reads, import_revision, plan) =
        begin_database_plan(&mut database, &mut assembler, context);
    let import_revision =
        publish_manifest_observations(&mut database, &snapshot, reads, &plan, import_revision);
    let revision = Revision::new(
        import_revision.revision_id,
        import_revision.compatibility_token,
    );
    let module = ModuleId::from_logical_path("main.rue").unwrap();
    let declaration = declaration_candidate(
        &database,
        revision,
        &module,
        Category::ConstCandidate,
        "OUT_MODULE",
    );
    let Value::ConstResolution(ConstResolutionProjection::ModuleBinding { key, target }) =
        request_semantic_nucleus(
            &database,
            revision,
            Key::ConstResolution(crate::semantic_query_nucleus::DeclarationSemanticQueryKey {
                declaration,
                configuration: semantic_configuration(),
            }),
        )
    else {
        panic!("expected a real imported module binding projection");
    };
    assert_eq!(
        key,
        crate::StableDefinitionKey::from_stable_parts(
            module,
            crate::StableDefinitionNamespace::Value,
            crate::StableDefinitionKind::ModuleBinding,
            "OUT_MODULE",
            None,
        )
    );
    assert_eq!(target, ModuleId::from_logical_path("dep.rue").unwrap());
}

#[test]
fn durable_module_member_projection_preserves_order_types_and_dependencies() {
    use crate::declaration_candidate::DeclarationCandidateCategory as Category;
    use crate::semantic_query_nucleus::{
        ConstResolutionProjection, SemanticDeclarationDependencyTarget,
        SemanticNucleusFailure as Failure, SemanticNucleusKey as Key,
        SemanticNucleusValue as Value,
    };

    let context =
        ImportDiscoveryContext::new(903, "/project", Some("/sdk"), "test-policy").unwrap();
    let mut assembler = DiscoverySourceAssembler::new(
        context.clone(),
        "/project/main.rue",
        "/project/main.rue",
        PhysicalFileIdentity::new(11, 1),
        FileMetadataFingerprint::new(11, 2, 3),
        Arc::new(
            "const M = @import(\"dep.rue\");\
                 const OUT_SCALAR: i32 = M.SCALAR;\
                 const OUT_CALLABLE = M.Callable;\
                 const OUT_STRUCT = M.StructValue;\
                 const OUT_ENUM = M.EnumValue;\
                 const OUT_NESTED = M.NESTED;\
                 const OUT_UNKNOWN: i32 = M.MISSING;\
                 const OUT_NONMODULE: i32 = OUT_SCALAR.missing;"
                .to_owned(),
        ),
    )
    .unwrap();
    assembler
        .add_explicit(
            "/project/dep.rue",
            "/project/dep.rue",
            PhysicalFileIdentity::new(12, 1),
            FileMetadataFingerprint::new(14, 5, 6),
            Arc::new(
                "pub const INNER: i32 = 2;\
                     pub const SCALAR: i32 = INNER + 5;\
                     pub fn Callable() -> i32 { 1 }\
                     pub struct StructValue { field: i32 }\
                     pub enum EnumValue { One }\
                     pub const NESTED = @import(\"nested.rue\");"
                    .to_owned(),
            ),
        )
        .unwrap();
    assembler
        .add_explicit(
            "/project/nested.rue",
            "/project/nested.rue",
            PhysicalFileIdentity::new(13, 1),
            FileMetadataFingerprint::new(17, 8, 9),
            Arc::new("const LEAF: i32 = 1;".to_owned()),
        )
        .unwrap();
    let mut database = RevisionedQueryDatabase::default();
    let (snapshot, reads, import_revision, plan) =
        begin_database_plan(&mut database, &mut assembler, context);
    let import_revision =
        publish_manifest_observations(&mut database, &snapshot, reads, &plan, import_revision);
    let revision = Revision::new(
        import_revision.revision_id,
        import_revision.compatibility_token,
    );
    let root = ModuleId::from_logical_path("main.rue").unwrap();
    let dep = ModuleId::from_logical_path("dep.rue").unwrap();
    let nested = ModuleId::from_logical_path("nested.rue").unwrap();
    let query = |name: &str| {
        let declaration =
            declaration_candidate(&database, revision, &root, Category::ConstCandidate, name);
        request_semantic_nucleus(
            &database,
            revision,
            Key::ConstResolution(crate::semantic_query_nucleus::DeclarationSemanticQueryKey {
                declaration,
                configuration: semantic_configuration(),
            }),
        )
    };
    let stable_key = |module: ModuleId, namespace, kind, name: &str| {
        crate::StableDefinitionKey::from_stable_parts(module, namespace, kind, name, None)
    };
    let direct_target =
        |name: &str, module: ModuleId, namespace, kind| stable_key(module, namespace, kind, name);
    let expected_dependencies = |name: &str, target: crate::StableDefinitionKey| {
        vec![
            crate::semantic_query_nucleus::SemanticDeclarationDependency {
                source: stable_key(
                    root.clone(),
                    crate::StableDefinitionNamespace::Value,
                    crate::StableDefinitionKind::ValueConst,
                    name,
                ),
                kind: rue_air::DeclarationTypeDependencyKind::Body,
                target: SemanticDeclarationDependencyTarget::NamedValue(stable_key(
                    root.clone(),
                    crate::StableDefinitionNamespace::Value,
                    crate::StableDefinitionKind::ModuleBinding,
                    "M",
                )),
            },
            crate::semantic_query_nucleus::SemanticDeclarationDependency {
                source: stable_key(
                    root.clone(),
                    crate::StableDefinitionNamespace::Value,
                    crate::StableDefinitionKind::ValueConst,
                    name,
                ),
                kind: rue_air::DeclarationTypeDependencyKind::Body,
                target: SemanticDeclarationDependencyTarget::NamedValue(target),
            },
        ]
    };
    let assert_direct_value = |name: &str, value, ty, target: crate::StableDefinitionKey| {
        let Value::ConstResolution(ConstResolutionProjection::Value {
            value: actual,
            ty: actual_ty,
            dependencies,
            ..
        }) = query(name)
        else {
            panic!("expected a value projection for {name}");
        };
        assert_eq!(*actual, value);
        assert_eq!(actual_ty, ty);
        assert_eq!(
            dependencies.as_ref(),
            expected_dependencies(name, target).as_slice()
        );
    };
    assert_direct_value(
        "OUT_SCALAR",
        crate::durable_semantics::DurableConstValue::Integer(7),
        crate::durable_semantics::DurableType::I32,
        direct_target(
            "SCALAR",
            dep.clone(),
            crate::StableDefinitionNamespace::Value,
            crate::StableDefinitionKind::ValueConst,
        ),
    );
    assert_direct_value(
        "OUT_CALLABLE",
        crate::durable_semantics::DurableConstValue::Function(direct_target(
            "Callable",
            dep.clone(),
            crate::StableDefinitionNamespace::Value,
            crate::StableDefinitionKind::Function,
        )),
        crate::durable_semantics::DurableType::ComptimeType,
        direct_target(
            "Callable",
            dep.clone(),
            crate::StableDefinitionNamespace::Value,
            crate::StableDefinitionKind::Function,
        ),
    );
    for (name, kind, member) in [
        (
            "OUT_STRUCT",
            crate::StableDefinitionKind::Struct,
            "StructValue",
        ),
        ("OUT_ENUM", crate::StableDefinitionKind::Enum, "EnumValue"),
    ] {
        let target = direct_target(
            member,
            dep.clone(),
            crate::StableDefinitionNamespace::Type,
            kind,
        );
        assert_direct_value(
            name,
            crate::durable_semantics::DurableConstValue::Type(
                crate::durable_semantics::DurableType::Nominal(target.clone()),
            ),
            crate::durable_semantics::DurableType::ComptimeType,
            target,
        );
    }
    let Value::ConstResolution(ConstResolutionProjection::ModuleBinding { key, target }) =
        query("OUT_NESTED")
    else {
        panic!("expected a nested module binding projection");
    };
    assert_eq!(
        key,
        stable_key(
            root.clone(),
            crate::StableDefinitionNamespace::Value,
            crate::StableDefinitionKind::ModuleBinding,
            "OUT_NESTED",
        )
    );
    assert_eq!(target, nested);
    assert!(matches!(
        query("OUT_UNKNOWN"),
        Value::Failure(Failure::Diagnostic(
            rue_error::ErrorKind::UnknownModuleMember {
                ref module_name,
                ref member_name,
            }
        )) if module_name == "dep.rue" && member_name == "MISSING"
    ));
    assert!(matches!(
        query("OUT_NONMODULE"),
        Value::Failure(Failure::Resolution(message))
            if message.as_ref() == "member access on a non-module const value"
    ));
}

#[test]
fn semantic_nucleus_resolves_exact_signatures_without_whole_module_semantics() {
    use crate::declaration_candidate::DeclarationCandidateCategory as Category;
    use crate::durable_semantics::DurableType as T;
    use crate::semantic_query_nucleus::{
        DeclarationSignatureProjection as Signature, SemanticNucleusKey as Key,
        SemanticNucleusValue as Value,
    };

    let source = source_snapshot(
        &[(
            1,
            "/main.rue",
            "main.rue",
            "struct Node { next: ptr const Node, } fn choose(comptime T: type, value: T) -> T { value }",
        )],
        1,
    );
    let module = ModuleId::from_logical_path("main.rue").unwrap();
    let mut database = RevisionedQueryDatabase::default();
    let revision =
        database.source_revision(&crate::session::ExactSourceInput::new(&source), &source);
    let configuration = semantic_configuration();

    let node = declaration_candidate(&database, revision, &module, Category::Struct, "Node");
    let node_query = crate::semantic_query_nucleus::DeclarationSemanticQueryKey {
        declaration: node,
        configuration: configuration.clone(),
    };
    let identity = request_semantic_nucleus(&database, revision, Key::Identity(node_query.clone()));
    let Value::Identity(identity) = identity else {
        panic!("expected Node identity, got {identity:?}")
    };
    let signature = request_semantic_nucleus(&database, revision, Key::Signature(node_query));
    assert_eq!(
            signature,
            Value::Signature(crate::semantic_query_nucleus::ResolvedDeclarationSignature {
                definition: identity.key.clone(),
                signature: Signature::Struct {
                    fields: vec![(
                        Arc::from("next"),
                        T::PtrConst(Arc::new(T::Nominal(identity.key.clone())))
                    )]
                    .into(),
                    is_copy: false,
                    is_linear: false,
                    is_repr_c: false,
                },
                callable_type_syntax: None,
                anonymous_nominals: Arc::from([]),
                dependencies: vec![
                    crate::semantic_query_nucleus::SemanticDeclarationDependency {
                        source: identity.key.clone(),
                        kind: rue_air::DeclarationTypeDependencyKind::Field,
                        target: crate::semantic_query_nucleus::SemanticDeclarationDependencyTarget::NamedType(
                            identity.key,
                        ),
                    },
                ]
                .into(),
                deferred_ownership: Arc::from([]),
            })
        );

    let choose = declaration_candidate(&database, revision, &module, Category::Function, "choose");
    let signature = request_semantic_nucleus(
        &database,
        revision,
        Key::Signature(crate::semantic_query_nucleus::DeclarationSemanticQueryKey {
            declaration: choose,
            configuration,
        }),
    );
    let Value::Signature(crate::semantic_query_nucleus::ResolvedDeclarationSignature {
        signature: Signature::Callable {
            parameters, result, ..
        },
        callable_type_syntax,
        ..
    }) = signature
    else {
        panic!("expected callable signature, got {signature:?}")
    };
    assert_eq!(parameters[0].ty, T::ComptimeType);
    assert_eq!(parameters[1].ty, T::GenericParameter(0));
    assert_eq!(result, T::GenericParameter(0));
    let callable_type_syntax = callable_type_syntax.expect("choose is callable");
    assert_eq!(
        callable_type_syntax
            .parameters
            .iter()
            .map(|root| callable_type_syntax.syntax.render_type(*root).unwrap())
            .collect::<Vec<_>>(),
        ["type", "T"]
    );
    assert_eq!(
        callable_type_syntax
            .syntax
            .render_type(callable_type_syntax.result)
            .unwrap(),
        "T"
    );
}

#[test]
fn nominal_well_formedness_is_a_keyed_query_and_preserves_indirection() {
    use crate::declaration_candidate::DeclarationCandidateCategory as Category;
    use crate::semantic_query_nucleus::{
        SemanticNucleusFailure as Failure, SemanticNucleusKey as Key, SemanticNucleusValue as Value,
    };

    let source = source_snapshot(
        &[(
            1,
            "/main.rue",
            "main.rue",
            "struct Bad { next: [Bad; 0] } struct Good { next: ptr const Good }",
        )],
        1,
    );
    let module = ModuleId::from_logical_path("main.rue").unwrap();
    let mut database = RevisionedQueryDatabase::default();
    let revision =
        database.source_revision(&crate::session::ExactSourceInput::new(&source), &source);
    let query = |declaration| crate::semantic_query_nucleus::DeclarationSemanticQueryKey {
        declaration,
        configuration: semantic_configuration(),
    };

    let bad = declaration_candidate(&database, revision, &module, Category::Struct, "Bad");
    assert!(matches!(
        request_semantic_nucleus(
            &database,
            revision,
            Key::NominalWellFormedness(query(bad)),
        ),
        Value::Failure(Failure::Diagnostic(
            rue_error::ErrorKind::RecursiveTypeInfiniteSize { ref name, ref cycle }
        )) if name == "Bad" && cycle == "Bad -> Bad"
    ));

    let good = declaration_candidate(&database, revision, &module, Category::Struct, "Good");
    assert_eq!(
        request_semantic_nucleus(&database, revision, Key::NominalWellFormedness(query(good)),),
        Value::NominalWellFormedness,
    );
}

#[test]
fn require_droppable_propagates_signature_cycles_and_accepts_deferred_pointer_graphs() {
    use crate::declaration_candidate::DeclarationCandidateCategory as Category;
    use crate::semantic_query_nucleus::{
        SemanticNucleusFailure as Failure, SemanticNucleusKey as Key, SemanticNucleusValue as Value,
    };

    let cycle_source = source_snapshot(
        &[(
            1,
            "/main.rue",
            "main.rue",
            "fn Loop(comptime T: type) -> type { @require_droppable(Loop(T)); struct { value: ptr const T } } const X = Loop(i32);",
        )],
        1,
    );
    let module = ModuleId::from_logical_path("main.rue").unwrap();
    let mut cycle_database = RevisionedQueryDatabase::default();
    let cycle_revision = cycle_database.source_revision(
        &crate::session::ExactSourceInput::new(&cycle_source),
        &cycle_source,
    );
    let alias = declaration_candidate(
        &cycle_database,
        cycle_revision,
        &module,
        Category::ConstCandidate,
        "X",
    );
    let cycle = request_semantic_nucleus(
        &cycle_database,
        cycle_revision,
        Key::ConstResolution(crate::semantic_query_nucleus::DeclarationSemanticQueryKey {
            declaration: alias,
            configuration: semantic_configuration(),
        }),
    );
    assert!(
        matches!(
            &cycle,
            Value::Failure(Failure::Cycle(nodes))
                if nodes.iter().any(|name| name.as_ref() == "Loop")
        ),
        "unexpected cycle result: {cycle:?}"
    );

    let control_source = source_snapshot(
        &[(
            1,
            "/main.rue",
            "main.rue",
            "fn Wrap(comptime T: type) -> type { @require_droppable(T); struct { value: ptr const T } } struct Node { next: ptr const Wrap(Node) }",
        )],
        1,
    );
    let mut control_database = RevisionedQueryDatabase::default();
    let control_revision = control_database.source_revision(
        &crate::session::ExactSourceInput::new(&control_source),
        &control_source,
    );
    let node = declaration_candidate(
        &control_database,
        control_revision,
        &module,
        Category::Struct,
        "Node",
    );
    let producer = crate::semantic_query_nucleus::DeclarationSemanticQueryKey {
        declaration: node,
        configuration: semantic_configuration(),
    };
    let signature = request_semantic_nucleus(
        &control_database,
        control_revision,
        Key::Signature(producer.clone()),
    );
    let Value::Signature(signature) = signature else {
        panic!("expected deferred pointer signature, got {signature:?}")
    };
    let [gate] = signature.deferred_ownership.as_ref() else {
        panic!("expected one deferred ownership gate: {signature:?}")
    };
    assert_eq!(
        request_semantic_nucleus(
            &control_database,
            control_revision,
            Key::DeferredOwnership(crate::semantic_query_nucleus::DeferredOwnershipQueryKey {
                producer,
                gate: gate.clone(),
            }),
        ),
        Value::DeferredOwnership,
    );
}

#[test]
fn signature_engine_cycles_publish_family_owned_domain_failures() {
    use crate::declaration_candidate::DeclarationCandidateCategory as Category;
    use crate::semantic_query_nucleus::{
        SemanticNucleusFailure as Failure, SemanticNucleusKey as Key, SemanticNucleusValue as Value,
    };

    let source = source_snapshot(
        &[(
            1,
            "/main.rue",
            "main.rue",
            "fn A(x: B(i32)) -> i32 { 0 } fn B(x: A(i32)) -> i32 { 0 }",
        )],
        1,
    );
    let module = ModuleId::from_logical_path("main.rue").unwrap();
    let mut database = RevisionedQueryDatabase::default();
    let revision =
        database.source_revision(&crate::session::ExactSourceInput::new(&source), &source);
    let declaration = declaration_candidate(&database, revision, &module, Category::Function, "A");
    let result = request_semantic_nucleus(
        &database,
        revision,
        Key::Signature(crate::semantic_query_nucleus::DeclarationSemanticQueryKey {
            declaration,
            configuration: semantic_configuration(),
        }),
    );
    assert!(
        matches!(
            &result,
            Value::Failure(Failure::SignatureReentry { signature, cycle })
                if signature.name() == "B"
                    && cycle.as_ref() == [Arc::from("A"), Arc::from("B"), Arc::from("A")]
        ),
        "unexpected cycle diagnostic: {result:?}"
    );
}

#[test]
fn semantic_nucleus_evaluates_only_selected_const_dependencies_and_reports_cycles() {
    use crate::declaration_candidate::DeclarationCandidateCategory as Category;
    use crate::durable_semantics::DurableConstValue as Const;
    use crate::semantic_query_nucleus::{
        ConstResolutionProjection as Resolution, SemanticNucleusFailure as Failure,
        SemanticNucleusKey as Key, SemanticNucleusValue as Value,
    };

    let source = source_snapshot(
        &[(
            1,
            "/main.rue",
            "main.rue",
            "const base: i32 = 20; const selected: i32 = if true { base + 22 } else { missing }; const left: i32 = right; const right: i32 = left;",
        )],
        1,
    );
    let module = ModuleId::from_logical_path("main.rue").unwrap();
    let mut database = RevisionedQueryDatabase::default();
    let revision =
        database.source_revision(&crate::session::ExactSourceInput::new(&source), &source);
    let configuration = semantic_configuration();
    let query = |name: &str| {
        Key::ConstResolution(crate::semantic_query_nucleus::DeclarationSemanticQueryKey {
            declaration: declaration_candidate(
                &database,
                revision,
                &module,
                Category::ConstCandidate,
                name,
            ),
            configuration: configuration.clone(),
        })
    };

    let selected = request_semantic_nucleus(&database, revision, query("selected"));
    assert!(matches!(
        selected,
        Value::ConstResolution(Resolution::Value {
            value,
            ..
        }) if matches!(value.as_ref(), Const::Integer(42))
    ));
    let cycle = request_semantic_nucleus(&database, revision, query("left"));
    assert!(
        matches!(cycle, Value::Failure(Failure::Cycle(ref nodes)) if !nodes.is_empty()),
        "expected a domain cycle, got {cycle:?}"
    );
}

#[test]
fn semantic_nucleus_selects_declaration_time_target_branches_from_exact_configuration() {
    use crate::declaration_candidate::DeclarationCandidateCategory as Category;
    use crate::durable_semantics::{DurableConstValue as Const, DurableType as Type};
    use crate::semantic_query_nucleus::{
        ConstResolutionProjection as Resolution, SemanticNucleusKey as Key,
        SemanticNucleusValue as Value,
    };

    let source = source_snapshot(
        &[(
            1,
            "/main.rue",
            "main.rue",
            "const arch: i32 = match @target_arch() { Arch.X86_64 => 64, Arch.Aarch64 => 32 }; const os: i32 = if @target_os() == Os.Macos { 2 } else { 1 }; const model = match @target_data_model() { DataModel.Ilp32 => i8, DataModel.Lp64 => i64, DataModel.Llp64 => i16 };",
        )],
        1,
    );
    let module = ModuleId::from_logical_path("main.rue").unwrap();
    let mut database = RevisionedQueryDatabase::default();
    let revision =
        database.source_revision(&crate::session::ExactSourceInput::new(&source), &source);
    let request = |database: &RevisionedQueryDatabase, target: rue_target::Target, name: &str| {
        let mut configuration = semantic_configuration();
        configuration.target = target;
        request_semantic_nucleus(
            database,
            revision,
            Key::ConstResolution(crate::semantic_query_nucleus::DeclarationSemanticQueryKey {
                declaration: declaration_candidate(
                    database,
                    revision,
                    &module,
                    Category::ConstCandidate,
                    name,
                ),
                configuration,
            }),
        )
    };

    assert!(matches!(
        request(&database, rue_target::Target::X86_64Linux, "arch"),
        Value::ConstResolution(Resolution::Value {
            value,
            ty: Type::I32,
            ..
        }) if matches!(value.as_ref(), Const::Integer(64))
    ));
    assert!(matches!(
        request(&database, rue_target::Target::Aarch64Macos, "arch"),
        Value::ConstResolution(Resolution::Value {
            value,
            ty: Type::I32,
            ..
        }) if matches!(value.as_ref(), Const::Integer(32))
    ));
    assert!(matches!(
        request(&database, rue_target::Target::Aarch64Macos, "os"),
        Value::ConstResolution(Resolution::Value {
            value,
            ty: Type::I32,
            ..
        }) if matches!(value.as_ref(), Const::Integer(2))
    ));
    assert!(matches!(
        request(&database, rue_target::Target::Aarch64Linux, "model"),
        Value::ConstResolution(Resolution::Value {
            value,
            ty: Type::ComptimeType,
            ..
        }) if matches!(value.as_ref(), Const::Type(Type::I64))
    ));
}

#[test]
fn semantic_nucleus_demand_does_not_touch_unrelated_declarations() {
    use crate::declaration_candidate::DeclarationCandidateCategory as Category;
    use crate::durable_semantics::DurableConstValue as Const;
    use crate::semantic_query_nucleus::{
        ConstResolutionProjection as Resolution, SemanticNucleusKey as Key,
        SemanticNucleusValue as Value,
    };

    let mut text = String::from("const base: i32 = 20; const selected: i32 = base + 22;\n");
    for index in 0..128 {
        text.push_str(&format!("const unrelated{index}: i32 = missing{index};\n"));
    }
    let source = source_snapshot(&[(1, "/main.rue", "main.rue", &text)], 1);
    let module = ModuleId::from_logical_path("main.rue").unwrap();
    let mut database = RevisionedQueryDatabase::default();
    let revision =
        database.source_revision(&crate::session::ExactSourceInput::new(&source), &source);
    let selected = declaration_candidate(
        &database,
        revision,
        &module,
        Category::ConstCandidate,
        "selected",
    );
    let value = request_semantic_nucleus(
        &database,
        revision,
        Key::ConstResolution(crate::semantic_query_nucleus::DeclarationSemanticQueryKey {
            declaration: selected,
            configuration: semantic_configuration(),
        }),
    );
    assert!(matches!(
        value,
        Value::ConstResolution(Resolution::Value {
            value,
            ..
        }) if matches!(value.as_ref(), Const::Integer(42))
    ));
    assert_eq!(
        database.semantic_nucleus.retention().terminals,
        2,
        "only `selected` and its exact `base` dependency may publish semantic terminals"
    );
}

#[test]
fn semantic_nucleus_lifecycle_distinguishes_terminals_from_control_flow() {
    use crate::declaration_candidate::DeclarationCandidateCategory as Category;
    use crate::durable_semantics::DurableConstValue as Const;
    use crate::semantic_query_nucleus::{
        ConstResolutionProjection as Resolution, SemanticNucleusFailure as Failure,
        SemanticNucleusKey as Key, SemanticNucleusValue as Value,
    };

    let source_text = (0..=MODULE_QUERY_MEMO_RETENTION)
        .map(|index| format!("const c{index}: i32 = {index};"))
        .chain([
            "const bad: i32 = missing;".to_owned(),
            "const canceled: i32 = 7;".to_owned(),
        ])
        .collect::<Vec<_>>()
        .join("\n");
    let source = source_snapshot(&[(1, "/main.rue", "main.rue", &source_text)], 1);
    let module = ModuleId::from_logical_path("main.rue").unwrap();
    let mut database =
        RevisionedQueryDatabase::with_declaration_memo_retention(MODULE_QUERY_MEMO_RETENTION);
    let revision =
        database.source_revision(&crate::session::ExactSourceInput::new(&source), &source);
    let configuration = semantic_configuration();
    let query = |name: &str| crate::semantic_query_nucleus::DeclarationSemanticQueryKey {
        declaration: declaration_candidate(
            &database,
            revision,
            &module,
            Category::ConstCandidate,
            name,
        ),
        configuration: configuration.clone(),
    };

    let c0 = Key::ConstResolution(query("c0"));
    let cold = database.runtime.request_registered(
        &database.semantic_nucleus,
        revision,
        c0.clone(),
        CancellationToken::new(),
    );
    assert_eq!(execution(&cold), RequestExecution::Computed);
    let cold_terminal = cold.terminal().unwrap();
    let cold_stamp = cold_terminal.stamp();
    let rue_query::QueryOutcome::Success(cold_value) = cold_terminal.outcome() else {
        unreachable!()
    };
    assert!(matches!(
        cold_value,
        Value::ConstResolution(Resolution::Value {
            value,
            ..
        }) if matches!(value.as_ref(), Const::Integer(0))
    ));

    let warm = database.runtime.request_registered(
        &database.semantic_nucleus,
        revision,
        c0.clone(),
        CancellationToken::new(),
    );
    assert_eq!(execution(&warm), RequestExecution::Reused);
    assert_eq!(warm.terminal().unwrap().stamp(), cold_stamp);
    assert_eq!(warm.terminal().unwrap().outcome(), cold_terminal.outcome());

    let bad = Key::ConstResolution(query("bad"));
    let failed = database.runtime.request_registered(
        &database.semantic_nucleus,
        revision,
        bad.clone(),
        CancellationToken::new(),
    );
    let failed_terminal = failed.terminal().unwrap();
    assert_eq!(failed_terminal.kind(), QueryTerminalKind::Failure);
    assert!(matches!(
        failed_terminal.outcome(),
        rue_query::QueryOutcome::Success(Value::Failure(Failure::Resolution(_)))
    ));
    let failed_again = database.runtime.request_registered(
        &database.semantic_nucleus,
        revision,
        bad,
        CancellationToken::new(),
    );
    assert_eq!(execution(&failed_again), RequestExecution::Reused);
    assert_eq!(
        failed_again.terminal().unwrap().stamp(),
        failed_terminal.stamp(),
        "deterministic semantic failures are reusable terminals"
    );

    let canceled_key = Key::ConstResolution(query("canceled"));
    let cancellation = CancellationToken::new();
    cancellation.cancel();
    let canceled = database.runtime.request_registered(
        &database.semantic_nucleus,
        revision,
        canceled_key.clone(),
        cancellation,
    );
    assert_eq!(execution(&canceled), RequestExecution::Aborted);
    assert!(canceled.terminal().is_none());
    let after_cancel = database.runtime.request_registered(
        &database.semantic_nucleus,
        revision,
        canceled_key,
        CancellationToken::new(),
    );
    assert_eq!(execution(&after_cancel), RequestExecution::Computed);

    let cycle = database.runtime.request_registered(
        &database.semantic_nucleus,
        revision,
        Key::EngineCycleProbe(query("canceled")),
        CancellationToken::new(),
    );
    assert_eq!(execution(&cycle), RequestExecution::Aborted);
    assert!(matches!(cycle.abort(), Some(QueryAbort::Cycle(_))));
    assert!(cycle.terminal().is_none());

    for index in 1..=MODULE_QUERY_MEMO_RETENTION {
        let requested = database.runtime.request_registered(
            &database.semantic_nucleus,
            revision,
            Key::ConstResolution(query(&format!("c{index}"))),
            CancellationToken::new(),
        );
        assert!(requested.terminal().is_some());
    }
    assert_eq!(
        database.semantic_nucleus.retention().terminals,
        MODULE_QUERY_MEMO_RETENTION
    );
    let after_eviction = database.runtime.request_registered(
        &database.semantic_nucleus,
        revision,
        c0,
        CancellationToken::new(),
    );
    assert_eq!(execution(&after_eviction), RequestExecution::Computed);
    assert_eq!(
        after_eviction.terminal().unwrap().outcome(),
        cold_terminal.outcome()
    );

    let broken = source_snapshot(
        &[(1, "/main.rue", "main.rue", "const value: i32 = missing;")],
        1,
    );
    let fixed = source_snapshot(&[(1, "/main.rue", "main.rue", "const value: i32 = 42;")], 1);
    let mut recovery = RevisionedQueryDatabase::default();
    let broken_revision =
        recovery.source_revision(&crate::session::ExactSourceInput::new(&broken), &broken);
    let broken_query = crate::semantic_query_nucleus::DeclarationSemanticQueryKey {
        declaration: declaration_candidate(
            &recovery,
            broken_revision,
            &module,
            Category::ConstCandidate,
            "value",
        ),
        configuration: configuration.clone(),
    };
    assert!(matches!(
        request_semantic_nucleus(
            &recovery,
            broken_revision,
            Key::ConstResolution(broken_query)
        ),
        Value::Failure(Failure::Resolution(_))
    ));
    let fixed_revision =
        recovery.source_revision(&crate::session::ExactSourceInput::new(&fixed), &fixed);
    let fixed_query = crate::semantic_query_nucleus::DeclarationSemanticQueryKey {
        declaration: declaration_candidate(
            &recovery,
            fixed_revision,
            &module,
            Category::ConstCandidate,
            "value",
        ),
        configuration,
    };
    assert!(matches!(
        request_semantic_nucleus(&recovery, fixed_revision, Key::ConstResolution(fixed_query)),
        Value::ConstResolution(Resolution::Value {
            value,
            ..
        }) if matches!(value.as_ref(), Const::Integer(42))
    ));
}

#[test]
fn declaration_shell_queries_are_keyed_exact_and_payload_stable() {
    let first = source_snapshot(
        &[(
            1,
            "/main.rue",
            "main.rue",
            "struct Box { fn get(self) -> i32 { 1 } } const item = 1; fn main() {}",
        )],
        1,
    );
    let edited = source_snapshot(
        &[(
            1,
            "/main.rue",
            "main.rue",
            "// shifted file\nstruct Box { fn // comment-only signature trivia\n get(self) -> i32 { 999 } } const item = @import(\"x.rue\"); // shifted again\n fn main() { let x = 2; }",
        )],
        1,
    );
    let main = ModuleId::from_logical_path("main.rue").unwrap();
    let mut database = RevisionedQueryDatabase::default();
    let first_revision =
        database.source_revision(&crate::session::ExactSourceInput::new(&first), &first);
    let indexed = database.runtime.request_registered(
        &database.declaration_occurrence_indexes,
        first_revision,
        ModuleQueryKey(main.clone()),
        CancellationToken::new(),
    );
    assert_eq!(execution(&indexed), RequestExecution::Computed);
    assert_eq!(indexed.dependencies().len(), 1);
    let terminal = indexed.terminal().unwrap();
    let rue_query::QueryOutcome::Success(indexed_value) = terminal.outcome() else {
        unreachable!()
    };
    let DeclarationOccurrenceIndexValue::Available(indexed_value) = indexed_value else {
        panic!("expected available occurrence index")
    };
    let keys = indexed_value
        .capabilities
        .keys()
        .cloned()
        .collect::<Vec<_>>();
    assert_eq!(keys.len(), 4);
    let mut shell_stamps = BTreeMap::new();
    for key in &keys {
        let first = database.runtime.request_registered(
            &database.declaration_shells,
            first_revision,
            DeclarationShellQueryKey(key.clone()),
            CancellationToken::new(),
        );
        assert_eq!(execution(&first), RequestExecution::Computed);
        shell_stamps.insert(key.stable_identity(), first.terminal().unwrap().stamp());
        assert_eq!(
            first
                .dependencies()
                .iter()
                .map(|dependency| dependency.node.family())
                .collect::<Vec<_>>(),
            vec![
                "compiler.declaration-occurrence-index",
                "compiler.parse-module"
            ]
        );
        let warm = database.runtime.request_registered(
            &database.declaration_shells,
            first_revision,
            DeclarationShellQueryKey(key.clone()),
            CancellationToken::new(),
        );
        assert_eq!(execution(&warm), RequestExecution::Reused);
    }

    let edited_revision =
        database.source_revision(&crate::session::ExactSourceInput::new(&edited), &edited);
    let edited_index = database.runtime.request_registered(
        &database.declaration_occurrence_indexes,
        edited_revision,
        ModuleQueryKey(main),
        CancellationToken::new(),
    );
    let rue_query::QueryOutcome::Success(edited_value) = edited_index.terminal().unwrap().outcome()
    else {
        unreachable!()
    };
    let DeclarationOccurrenceIndexValue::Available(edited_value) = edited_value else {
        panic!("expected available edited occurrence index")
    };
    assert_eq!(&indexed_value.capabilities, &edited_value.capabilities);
    for key in &keys {
        let revalidated = database.runtime.request_registered(
            &database.declaration_shells,
            edited_revision,
            DeclarationShellQueryKey(key.clone()),
            CancellationToken::new(),
        );
        let terminal = revalidated.terminal().unwrap();
        assert_eq!(
            terminal.stamp(),
            shell_stamps[&key.stable_identity()],
            "payload-only edits must preserve the shell publication stamp"
        );
    }
}

#[test]
fn canceled_declaration_shell_request_publishes_no_terminal_and_recovers() {
    let source = source_snapshot(&[(1, "/main.rue", "main.rue", "fn main() {}")], 1);
    let main = ModuleId::from_logical_path("main.rue").unwrap();
    let mut database = RevisionedQueryDatabase::default();
    let revision =
        database.source_revision(&crate::session::ExactSourceInput::new(&source), &source);
    let indexed = database.runtime.request_registered(
        &database.declaration_occurrence_indexes,
        revision,
        ModuleQueryKey(main),
        CancellationToken::new(),
    );
    let rue_query::QueryOutcome::Success(indexed) = indexed.terminal().unwrap().outcome() else {
        unreachable!()
    };
    let DeclarationOccurrenceIndexValue::Available(indexed) = indexed else {
        panic!("expected available occurrence index")
    };
    let key = indexed.capabilities.keys().next().unwrap().clone();
    let canceled = CancellationToken::new();
    canceled.cancel();
    let aborted = database.runtime.request_registered(
        &database.declaration_shells,
        revision,
        DeclarationShellQueryKey(key.clone()),
        canceled,
    );
    assert_eq!(execution(&aborted), RequestExecution::Aborted);
    assert!(aborted.terminal().is_none());
    let recovered = database.runtime.request_registered(
        &database.declaration_shells,
        revision,
        DeclarationShellQueryKey(key),
        CancellationToken::new(),
    );
    assert_eq!(execution(&recovered), RequestExecution::Computed);
    assert!(recovered.terminal().is_some());
}

#[test]
fn absent_declaration_shell_is_a_typed_position_free_failure_terminal() {
    let source = source_snapshot(&[(1, "/main.rue", "main.rue", "fn main() {}")], 1);
    let module = ModuleId::from_logical_path("main.rue").unwrap();
    let mut database = RevisionedQueryDatabase::default();
    let revision =
        database.source_revision(&crate::session::ExactSourceInput::new(&source), &source);
    let key = crate::declaration_candidate::DeclarationCandidateKey {
        module,
        category: crate::declaration_candidate::DeclarationCandidateCategory::Function,
        name: Arc::from("missing"),
        owner: None,
        duplicate_discriminator: 0,
    };
    let requested = database.runtime.request_registered(
        &database.declaration_shells,
        revision,
        DeclarationShellQueryKey(key.clone()),
        CancellationToken::new(),
    );
    let terminal = requested.terminal().unwrap();
    assert_eq!(terminal.kind(), QueryTerminalKind::Failure);
    assert!(terminal.diagnostics().is_empty());
    assert!(matches!(
        terminal.outcome(),
        rue_query::QueryOutcome::Success(DeclarationShellQueryValue::Failure(
            crate::declaration_candidate::DeclarationShellFailure::Absent(absent)
        )) if absent == &key
    ));
}

fn project_signature_for_test(
    database: &RevisionedQueryDatabase,
    revision: Revision,
    key: &crate::declaration_candidate::DeclarationCandidateKey,
) -> crate::semantic_query_nucleus::ParsedSemanticSignature {
    let parsed = database.runtime.request_registered(
        &database.parse_modules,
        revision,
        ModuleQueryKey(key.module.clone()),
        CancellationToken::new(),
    );
    let rue_query::QueryOutcome::Success(parsed) =
        parsed.terminal().expect("parse terminal").outcome()
    else {
        panic!("parse query publishes typed values")
    };
    let module = parsed.result.as_ref().expect("module parses");
    crate::semantic_query_nucleus::project_semantic_signature(module, key)
        .expect("exact signature projects")
}

fn request_signature_for_test(
    database: &RevisionedQueryDatabase,
    revision: Revision,
    declaration: crate::declaration_candidate::DeclarationCandidateKey,
    cancellation: CancellationToken,
) -> QueryRequestAttempt<crate::semantic_query_nucleus::SemanticNucleusValue> {
    database.runtime.request_registered(
        &database.semantic_nucleus,
        revision,
        crate::semantic_query_nucleus::SemanticNucleusKey::Signature(
            crate::semantic_query_nucleus::DeclarationSemanticQueryKey {
                declaration,
                configuration: semantic_configuration(),
            },
        ),
        cancellation,
    )
}

#[test]
fn declaration_signature_is_exact_lazy_and_red_green() {
    fn program(selected_type: &str, unrelated_body: u32) -> String {
        let mut source = String::new();
        for index in 0..128 {
            let body = if index == 64 { unrelated_body } else { index };
            source.push_str(&format!("fn unrelated{index}() -> i32 {{ {body} }}\n"));
        }
        source.push_str(&format!(
            "fn selected(value: {selected_type}) -> {selected_type} {{ value }}\n"
        ));
        source
    }

    let first_text = program("i32", 64);
    let unrelated_edit_text = program("i32", 999);
    let selected_edit_text = program("i64", 999);
    let first = source_snapshot(&[(1, "/main.rue", "main.rue", &first_text)], 1);
    let unrelated_edit = source_snapshot(&[(1, "/main.rue", "main.rue", &unrelated_edit_text)], 1);
    let selected_edit = source_snapshot(&[(1, "/main.rue", "main.rue", &selected_edit_text)], 1);
    let module = ModuleId::from_logical_path("main.rue").unwrap();
    let key = crate::declaration_candidate::DeclarationCandidateKey {
        module,
        category: crate::declaration_candidate::DeclarationCandidateCategory::Function,
        name: Arc::from("selected"),
        owner: None,
        duplicate_discriminator: 0,
    };
    let mut database = RevisionedQueryDatabase::default();

    let first_revision = revision_for(&mut database, &first);
    let first_request = request_signature_for_test(
        &database,
        first_revision,
        key.clone(),
        CancellationToken::new(),
    );
    assert_eq!(execution(&first_request), RequestExecution::Computed);
    assert!(
        first_request
            .dependencies()
            .iter()
            .any(|dependency| dependency.node.family() == "compiler.parse-module")
    );
    let first_terminal = first_request.terminal().expect("signature terminal");
    let first_stamp = first_terminal.stamp();
    let rue_query::QueryOutcome::Success(
        crate::semantic_query_nucleus::SemanticNucleusValue::Signature(first_signature),
    ) = first_terminal.outcome()
    else {
        panic!("selected signature resolves")
    };
    let syntax = first_signature
        .callable_type_syntax
        .as_ref()
        .expect("callable syntax is retained with the resolved signature");
    assert_eq!(
        syntax.syntax.render_type(syntax.parameters[0]).unwrap(),
        "i32"
    );
    assert_eq!(syntax.syntax.render_type(syntax.result).unwrap(), "i32");

    let warm = request_signature_for_test(
        &database,
        first_revision,
        key.clone(),
        CancellationToken::new(),
    );
    assert_eq!(execution(&warm), RequestExecution::Reused);

    let unrelated_revision = revision_for(&mut database, &unrelated_edit);
    let unrelated_request = request_signature_for_test(
        &database,
        unrelated_revision,
        key.clone(),
        CancellationToken::new(),
    );
    assert_eq!(
        unrelated_request
            .terminal()
            .expect("signature terminal")
            .stamp(),
        first_stamp,
        "an unrelated body edit must preserve the authoritative signature stamp"
    );

    let selected_revision = revision_for(&mut database, &selected_edit);
    let selected_request =
        request_signature_for_test(&database, selected_revision, key, CancellationToken::new());
    let selected_terminal = selected_request.terminal().expect("signature terminal");
    assert_ne!(selected_terminal.stamp(), first_stamp);
    let rue_query::QueryOutcome::Success(
        crate::semantic_query_nucleus::SemanticNucleusValue::Signature(selected_signature),
    ) = selected_terminal.outcome()
    else {
        panic!("edited signature resolves")
    };
    let syntax = selected_signature
        .callable_type_syntax
        .as_ref()
        .expect("callable syntax is retained");
    assert_eq!(
        syntax.syntax.render_type(syntax.parameters[0]).unwrap(),
        "i64"
    );
    assert_eq!(syntax.syntax.render_type(syntax.result).unwrap(), "i64");
    assert_eq!(
        database
            .declaration_body_plan_astgen_evaluations
            .load(std::sync::atomic::Ordering::Relaxed),
        0,
        "signature evaluation must not lower a body"
    );
}

#[test]
fn parsed_signature_projection_covers_every_category_and_exact_duplicate() {
    use crate::declaration_candidate::DeclarationCandidateCategory as Category;

    let source = source_snapshot(
        &[(
            1,
            "/main.rue",
            "main.rue",
            "@copy linear struct Box { value: i32, fn get(borrow self) -> i32 { self.value } fn make(value: i32) -> Box { Box { value } } }\n\
                 enum Choice { Empty, Value(i32, u64) }\n\
                 drop fn Box(self) {}\n\
                 extern \"C\" { fn foreign(value: ptr const u8) -> i32; }\n\
                 fn duplicate(value: i32) {} fn duplicate(value: i64) {}",
        )],
        1,
    );
    let module = ModuleId::from_logical_path("main.rue").unwrap();
    let owner = crate::declaration_candidate::DeclarationCandidateOwner {
        category: Category::Struct,
        name: Arc::from("Box"),
    };
    let key = |category, name: &'static str, owner, duplicate_discriminator| {
        crate::declaration_candidate::DeclarationCandidateKey {
            module: module.clone(),
            category,
            name: Arc::from(name),
            owner,
            duplicate_discriminator,
        }
    };
    let mut database = RevisionedQueryDatabase::default();
    let revision = revision_for(&mut database, &source);

    let structure =
        project_signature_for_test(&database, revision, &key(Category::Struct, "Box", None, 0));
    let crate::semantic_query_nucleus::ParsedSemanticSignature::Struct {
        fields,
        is_copy: true,
        is_linear: true,
        is_repr_c: false,
        ..
    } = &structure
    else {
        panic!("expected compact struct signature, got {structure:?}");
    };
    assert_eq!(fields.len(), 1);
    assert_eq!(structure.symbol(fields[0].name), "value");
    assert_eq!(structure.render_type(fields[0].ty), "i32");

    for (candidate, expected_result, expected_self) in [
        (
            key(Category::Method, "get", Some(owner.clone()), 0),
            "i32",
            true,
        ),
        (
            key(Category::AssociatedFunction, "make", Some(owner.clone()), 0),
            "Box",
            false,
        ),
    ] {
        let signature = project_signature_for_test(&database, revision, &candidate);
        assert!(matches!(
            &signature,
            crate::semantic_query_nucleus::ParsedSemanticSignature::Callable {
                result,
                has_self,
                is_extern: false,
                ..
            } if signature.render_type(*result) == expected_result && *has_self == expected_self
        ));
    }

    let enumeration =
        project_signature_for_test(&database, revision, &key(Category::Enum, "Choice", None, 0));
    let crate::semantic_query_nucleus::ParsedSemanticSignature::Enum {
        variants, payloads, ..
    } = &enumeration
    else {
        panic!("expected compact enum signature, got {enumeration:?}");
    };
    assert_eq!(variants.len(), 2);
    assert_eq!(enumeration.symbol(variants[0].name), "Empty");
    assert_eq!(enumeration.symbol(variants[1].name), "Value");
    let payload = &payloads[variants[1].payload_start as usize..variants[1].payload_end as usize];
    assert_eq!(
        payload
            .iter()
            .map(|value| enumeration.render_type(*value))
            .collect::<Vec<_>>(),
        ["i32", "u64"]
    );

    assert!(matches!(
        project_signature_for_test(
            &database,
            revision,
            &key(Category::Destructor, "Box", Some(owner), 0),
        ),
        crate::semantic_query_nucleus::ParsedSemanticSignature::Destructor
    ));
    let foreign = project_signature_for_test(
        &database,
        revision,
        &key(Category::ExternFunction, "foreign", None, 0),
    );
    assert!(matches!(
        &foreign,
        crate::semantic_query_nucleus::ParsedSemanticSignature::Callable {
            parameters,
            result,
            is_extern: true,
            ..
        } if parameters.len() == 1
            && foreign.render_type(parameters[0].ty) == "ptr const u8"
            && foreign.render_type(*result) == "i32"
    ));

    for (duplicate_discriminator, expected) in [(0, "i32"), (1, "i64")] {
        let signature = project_signature_for_test(
            &database,
            revision,
            &key(
                Category::Function,
                "duplicate",
                None,
                duplicate_discriminator,
            ),
        );
        assert!(matches!(
            &signature,
            crate::semantic_query_nucleus::ParsedSemanticSignature::Callable {
                parameters,
                ..
            } if parameters.len() == 1 && signature.render_type(parameters[0].ty) == expected
        ));
    }
}

#[test]
fn parsed_signature_projection_preserves_every_annotation_type_shape() {
    use crate::declaration_candidate::DeclarationCandidateCategory as Category;
    use rue_rir::RirTypeSyntaxNode as Node;

    let source = source_snapshot(
        &[(
            1,
            "/main.rue",
            "main.rue",
            "fn full(\
                    named: i32, \
                    qualified: lib.geo.Point, \
                    unit: (), \
                    never: !, \
                    array_literal: [i32; 4], \
                    array_named: [i32; N], \
                    array_call: [i32; Width(N, 2)], \
                    slice: [i32], \
                    const_pointer: ptr const i32, \
                    mutable_pointer: ptr mut ptr const u8, \
                    type_call: Pair(i32, [i32; 2]), \
                    qualified_call: lib.pair.Pair(i32), \
                    integer_argument: Buffer(-2), \
                 ) -> Str(8) { loop {} }",
        )],
        1,
    );
    let key = crate::declaration_candidate::DeclarationCandidateKey {
        module: ModuleId::from_logical_path("main.rue").unwrap(),
        category: Category::Function,
        name: Arc::from("full"),
        owner: None,
        duplicate_discriminator: 0,
    };
    let mut database = RevisionedQueryDatabase::default();
    let revision = revision_for(&mut database, &source);
    let signature = project_signature_for_test(&database, revision, &key);
    let crate::semantic_query_nucleus::ParsedSemanticSignature::Callable {
        syntax,
        parameters,
        result,
        ..
    } = &signature
    else {
        panic!("expected callable signature, got {signature:?}");
    };

    assert_eq!(
        parameters
            .iter()
            .map(|parameter| syntax.render_type(parameter.ty).unwrap())
            .collect::<Vec<_>>(),
        [
            "i32",
            "lib.geo.Point",
            "()",
            "!",
            "[i32; 4]",
            "[i32; N]",
            "[i32; Width(N, 2)]",
            "[i32]",
            "ptr const i32",
            "ptr mut ptr const u8",
            "Pair(i32, [i32; 2])",
            "lib.pair.Pair(i32)",
            "Buffer(-2)",
        ]
    );
    assert_eq!(syntax.render_type(*result).as_deref(), Some("Str(8)"));

    let nodes = syntax.nodes();
    for (name, present) in [
        (
            "named",
            nodes.iter().any(|node| matches!(node, Node::Named(_))),
        ),
        (
            "qualified",
            nodes
                .iter()
                .any(|node| matches!(node, Node::Qualified { .. })),
        ),
        ("unit", nodes.iter().any(|node| matches!(node, Node::Unit))),
        (
            "never",
            nodes.iter().any(|node| matches!(node, Node::Never)),
        ),
        (
            "array",
            nodes.iter().any(|node| matches!(node, Node::Array { .. })),
        ),
        (
            "slice",
            nodes.iter().any(|node| matches!(node, Node::Slice { .. })),
        ),
        (
            "const pointer",
            nodes
                .iter()
                .any(|node| matches!(node, Node::PointerConst { .. })),
        ),
        (
            "mutable pointer",
            nodes
                .iter()
                .any(|node| matches!(node, Node::PointerMut { .. })),
        ),
        (
            "type call",
            nodes
                .iter()
                .any(|node| matches!(node, Node::TypeCall { .. })),
        ),
        (
            "value call",
            nodes
                .iter()
                .any(|node| matches!(node, Node::ValueCall { .. })),
        ),
        (
            "integer argument",
            nodes.iter().any(|node| matches!(node, Node::Integer(_))),
        ),
    ] {
        assert!(present, "signature arena omitted {name}");
    }
    assert_eq!(
        syntax
            .symbols()
            .iter()
            .filter(|symbol| symbol.as_ref() == "i32")
            .count(),
        1,
        "the declaration-local spelling table must deduplicate leaf names"
    );
}

#[test]
fn parsed_signature_projection_excludes_body_peer_and_absolute_trivia() {
    use crate::declaration_candidate::DeclarationCandidateCategory as Category;

    let first = source_snapshot(
        &[(
            1,
            "/main.rue",
            "main.rue",
            "fn free(value: i32) -> i32 { value }\n\
                 struct Box { value: i32, fn get(borrow self) -> i32 { self.value } }",
        )],
        1,
    );
    let relocated = source_snapshot(
        &[(
            1,
            "/main.rue",
            "main.rue",
            "// moved prefix\n\
                 fn free(value: i32) -> i32 // boundary\n\
                     { value + 0 }\n\
                 struct Box { value: i32, fn get(borrow self) -> u64 { 0 } }",
        )],
        1,
    );
    let signature_edit = source_snapshot(
        &[(
            1,
            "/main.rue",
            "main.rue",
            "fn free(value: i64) -> i32 { 0 }\n\
                 struct Box { value: i32, fn get(borrow self) -> u64 { 0 } }",
        )],
        1,
    );
    let module = ModuleId::from_logical_path("main.rue").unwrap();
    let free = crate::declaration_candidate::DeclarationCandidateKey {
        module: module.clone(),
        category: Category::Function,
        name: Arc::from("free"),
        owner: None,
        duplicate_discriminator: 0,
    };
    let structure = crate::declaration_candidate::DeclarationCandidateKey {
        module,
        category: Category::Struct,
        name: Arc::from("Box"),
        owner: None,
        duplicate_discriminator: 0,
    };
    let mut database = RevisionedQueryDatabase::default();

    let first_revision = revision_for(&mut database, &first);
    let first_free = project_signature_for_test(&database, first_revision, &free);
    let first_structure = project_signature_for_test(&database, first_revision, &structure);

    let relocated_revision = revision_for(&mut database, &relocated);
    assert_eq!(
        project_signature_for_test(&database, relocated_revision, &free),
        first_free,
        "body and absolute-trivia motion must not change a signature"
    );
    assert_eq!(
        project_signature_for_test(&database, relocated_revision, &structure),
        first_structure,
        "a peer method signature must not enter the struct signature"
    );

    let signature_revision = revision_for(&mut database, &signature_edit);
    assert_ne!(
        project_signature_for_test(&database, signature_revision, &free),
        first_free,
        "an exact parameter-type edit must change the signature"
    );
}

#[test]
fn parsed_accessor_signature_uses_exact_owner_facts() {
    use crate::declaration_candidate::DeclarationCandidateCategory as Category;
    use rue_air::declaration_validation::AccessorBodyVerdict;

    let source = source_snapshot(
        &[(
            1,
            "/main.rue",
            "main.rue",
            "struct S { field: i32, fn selected(borrow self) -> borrow i32 { yield self.selected(); } }",
        )],
        1,
    );
    let module = ModuleId::from_logical_path("main.rue").unwrap();
    let key = crate::declaration_candidate::DeclarationCandidateKey {
        module,
        category: Category::Method,
        name: Arc::from("selected"),
        owner: Some(crate::declaration_candidate::DeclarationCandidateOwner {
            category: Category::Struct,
            name: Arc::from("S"),
        }),
        duplicate_discriminator: 0,
    };
    let mut database = RevisionedQueryDatabase::default();
    let revision = revision_for(&mut database, &source);
    let signature = project_signature_for_test(&database, revision, &key);
    assert!(matches!(
        signature,
        crate::semantic_query_nucleus::ParsedSemanticSignature::Callable {
            is_accessor: true,
            accessor_body: AccessorBodyVerdict::WellFormed,
            accessor_cycle: Some(name),
            ..
        } if name.as_ref() == "selected"
    ));
}

#[test]
fn authoritative_signature_cancellation_publishes_nothing_and_retries() {
    use crate::declaration_candidate::DeclarationCandidateCategory as Category;

    let source = source_snapshot(&[(1, "/main.rue", "main.rue", "fn present() {}")], 1);
    let module = ModuleId::from_logical_path("main.rue").unwrap();
    let key = crate::declaration_candidate::DeclarationCandidateKey {
        module,
        category: Category::Function,
        name: Arc::from("present"),
        owner: None,
        duplicate_discriminator: 0,
    };
    let mut database = RevisionedQueryDatabase::default();
    let revision = revision_for(&mut database, &source);

    let canceled = CancellationToken::new();
    canceled.cancel();
    let aborted = request_signature_for_test(&database, revision, key.clone(), canceled);
    assert_eq!(execution(&aborted), RequestExecution::Aborted);
    assert!(aborted.terminal().is_none());

    let recovered = request_signature_for_test(&database, revision, key, CancellationToken::new());
    assert_eq!(execution(&recovered), RequestExecution::Computed);
    assert!(matches!(
        recovered.terminal().expect("signature terminal").outcome(),
        rue_query::QueryOutcome::Success(
            crate::semantic_query_nucleus::SemanticNucleusValue::Signature(_)
        )
    ));
}

#[test]
fn declaration_shell_batches_over_64_entries_reuse_without_thrashing() {
    let source_text = (0..129)
        .map(|index| format!("fn f{index}() {{}}"))
        .collect::<Vec<_>>()
        .join("\n");
    let source = source_snapshot(&[(1, "/main.rue", "main.rue", source_text.as_str())], 1);
    let main = ModuleId::from_logical_path("main.rue").unwrap();
    let mut database = RevisionedQueryDatabase::default();
    let revision =
        database.source_revision(&crate::session::ExactSourceInput::new(&source), &source);
    let indexed = database.runtime.request_registered(
        &database.declaration_occurrence_indexes,
        revision,
        ModuleQueryKey(main),
        CancellationToken::new(),
    );
    let rue_query::QueryOutcome::Success(indexed) = indexed.terminal().unwrap().outcome() else {
        unreachable!()
    };
    let DeclarationOccurrenceIndexValue::Available(indexed) = indexed else {
        panic!("expected available occurrence index")
    };
    let keys = indexed.capabilities.keys().cloned().collect::<Vec<_>>();
    let mut first_stamps = Vec::with_capacity(keys.len());
    for key in &keys {
        let requested = database.runtime.request_registered(
            &database.declaration_shells,
            revision,
            DeclarationShellQueryKey(key.clone()),
            CancellationToken::new(),
        );
        assert_eq!(execution(&requested), RequestExecution::Computed);
        first_stamps.push(requested.terminal().unwrap().stamp());
    }
    for (key, first_stamp) in keys.iter().zip(first_stamps) {
        let warm = database.runtime.request_registered(
            &database.declaration_shells,
            revision,
            DeclarationShellQueryKey(key.clone()),
            CancellationToken::new(),
        );
        assert_eq!(execution(&warm), RequestExecution::Reused);
        assert_eq!(warm.terminal().unwrap().stamp(), first_stamp);
    }
}

#[test]
fn type_syntax_adapters_preserve_comptime_and_signature_diagnostics() {
    use rue_air::{SemanticResolutionError as E, SemanticTypeSyntaxFailure as F};

    let nested = E::ComptimeCallTypeArgument {
        constructor: Arc::from("Box"),
        argument_index: 0,
        argument: Arc::from("Sef"),
        error: Box::new(E::Semantic(F::UnknownType {
            syntax: Arc::from("Sef"),
        })),
    };
    let comptime = crate::durable_comptime::durable_comptime_type_syntax_failure(nested.clone());
    assert!(matches!(
        comptime,
        crate::durable_comptime::DurableComptimeFailure::Failure(value)
            if matches!(value.as_ref(), crate::semantic_query_nucleus::SemanticNucleusFailure::Resolution(reason)
                if reason.contains("Semantic(UnknownType"))
    ));

    let signature = semantic_type_query_failure(nested);
    assert!(matches!(
        signature,
        ResolveSemanticSignatureError::Failure(value)
            if matches!(value.as_ref(), crate::semantic_query_nucleus::SemanticNucleusFailure::Diagnostic(
                rue_error::ErrorKind::UnknownType(syntax)
            ) if syntax == "Sef")
    ));
}

#[test]
fn deferred_value_call_diagnostics_are_stable_and_keep_query_channels() {
    use rue_air::{
        SemanticComptimeCallExpectation as Expectation, SemanticResolutionError as E,
        SemanticTypeSyntaxFailure as F,
    };

    let site = StableDefinitionKey::from_stable_parts(
        ModuleId::from_logical_path("test.rue").unwrap(),
        crate::StableDefinitionNamespace::Value,
        crate::StableDefinitionKind::Function,
        Arc::from("callee"),
        None,
    );
    let classify = |failure| semantic_type_query_failure(E::Semantic(failure));
    let value_arity = classify(F::InvalidConstructorArity {
        constructor: Arc::from("value"),
        site: site.clone(),
        expected: 1,
        found: 0,
        expectation: Expectation::Value,
    });
    let ResolveSemanticSignatureError::Failure(value_arity) = value_arity else {
        panic!("value-call arity must be a stable diagnostic")
    };
    let crate::semantic_query_nucleus::SemanticNucleusFailure::Diagnostic(
        ErrorKind::ComptimeEvaluationFailed { reason },
    ) = *value_arity
    else {
        panic!("value-call arity must preserve E1200")
    };
    assert_eq!(
        reason,
        "value-returning comptime function `value` expects 1 comptime argument, but 0 were provided"
    );
    assert!(!reason.contains("type constructor"));
    assert!(!reason.contains("InvalidConstructorArity"));

    let runtime = classify(F::RuntimeConstructorParameter {
        constructor: Arc::from("runtime"),
        site: site.clone(),
        expected: 1,
        found: 1,
        expectation: Expectation::Value,
    });
    let ResolveSemanticSignatureError::Failure(runtime) = runtime else {
        panic!("runtime value-call rejection must be a stable diagnostic")
    };
    let crate::semantic_query_nucleus::SemanticNucleusFailure::Diagnostic(
        ErrorKind::ComptimeEvaluationFailed { reason },
    ) = *runtime
    else {
        panic!("runtime value-call rejection must preserve E1200")
    };
    assert_eq!(
        reason,
        "call `runtime(...)` is not a compile-time value because all parameters must be `comptime`"
    );
    assert!(!reason.contains("RuntimeConstructorParameter"));

    let type_arity = classify(F::InvalidConstructorArity {
        constructor: Arc::from("Box"),
        site,
        expected: 1,
        found: 0,
        expectation: Expectation::Type,
    });
    assert!(matches!(
        type_arity,
        ResolveSemanticSignatureError::Failure(value)
            if matches!(
                value.as_ref(),
                crate::semantic_query_nucleus::SemanticNucleusFailure::Resolution(reason)
                    if reason.as_ref() == "type constructor `Box` expects 1 comptime type argument(s), but 0 provided"
            )
    ));

    let type_runtime = classify(F::RuntimeConstructorParameter {
        constructor: Arc::from("RuntimeBox"),
        site: StableDefinitionKey::from_stable_parts(
            ModuleId::from_logical_path("test.rue").unwrap(),
            crate::StableDefinitionNamespace::Value,
            crate::StableDefinitionKind::Function,
            Arc::from("RuntimeBox"),
            None,
        ),
        expected: 1,
        found: 1,
        expectation: Expectation::Type,
    });
    assert!(matches!(
        type_runtime,
        ResolveSemanticSignatureError::Failure(value)
            if matches!(
                value.as_ref(),
                crate::semantic_query_nucleus::SemanticNucleusFailure::Resolution(reason)
                    if reason.as_ref() == "type constructor `RuntimeBox` cannot have runtime parameters; all parameters must be `comptime`"
            )
    ));

    let zero_parameter = classify(F::RuntimeConstructorParameter {
        constructor: Arc::from("zero"),
        site: StableDefinitionKey::from_stable_parts(
            ModuleId::from_logical_path("test.rue").unwrap(),
            crate::StableDefinitionNamespace::Value,
            crate::StableDefinitionKind::Function,
            Arc::from("zero"),
            None,
        ),
        expected: 0,
        found: 0,
        expectation: Expectation::Value,
    });
    assert!(matches!(
        zero_parameter,
        ResolveSemanticSignatureError::Failure(value)
            if matches!(
                value.as_ref(),
                crate::semantic_query_nucleus::SemanticNucleusFailure::Diagnostic(
                    ErrorKind::ComptimeEvaluationFailed { reason }
                ) if reason == "call `zero(...)` is not a compile-time value because its callee must declare at least one `comptime` parameter"
            )
    ));

    let provider_failure =
        crate::semantic_query_nucleus::SemanticNucleusFailure::Resolution(Arc::from("provider"));
    let preserved = semantic_type_query_failure(E::ProviderFailure(provider_failure.clone()));
    assert!(matches!(
        preserved,
        ResolveSemanticSignatureError::Failure(value) if *value == provider_failure
    ));
    assert!(matches!(
        semantic_type_query_failure(E::ProviderAbort(rue_query::QueryAbort::Canceled)),
        ResolveSemanticSignatureError::Abort(rue_query::QueryAbort::Canceled)
    ));
}
