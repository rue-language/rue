use super::super::test_support::trusted_body_snapshot;
use super::*;

#[test]
fn backend_root_publication_gate_serializes_distinct_epochs() {
    let gate = Arc::new(BackendRootPublicationGate::default());
    let first_epoch = gate.enter();
    let attempted = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let entered = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let second_gate = gate.clone();
    let second_attempted = attempted.clone();
    let second_entered = entered.clone();
    let second_epoch = std::thread::spawn(move || {
        second_attempted.store(true, std::sync::atomic::Ordering::Release);
        let _publication = second_gate.enter();
        second_entered.store(true, std::sync::atomic::Ordering::Release);
    });
    while !attempted.load(std::sync::atomic::Ordering::Acquire) {
        std::thread::yield_now();
    }
    assert!(
        !entered.load(std::sync::atomic::Ordering::Acquire),
        "a distinct backend-root epoch cannot enter while its predecessor may roll back"
    );
    drop(first_epoch);
    second_epoch.join().unwrap();
    assert!(entered.load(std::sync::atomic::Ordering::Acquire));
}

#[test]
fn backend_root_publication_handoff_restores_last_good_root_on_rollback() {
    let root = Arc::new(Mutex::new(PublishedBackendRoot {
        publications: 7,
        additions: 11,
        deletions: 3,
        ..PublishedBackendRoot::default()
    }));
    let mut handoff = PublishedBackendRootHandoff {
        root: root.clone(),
        pending: Some(Arc::new(rue_query::RetainedPinSet::new())),
        functions: Some(BTreeSet::new()),
        cfg_terminals: 2,
        optimized_cfg_terminals: 1,
        codegen_unit_terminals: 1,
        object_projection_terminals: 1,
        previous: None,
        installed: false,
    };
    rue_query::QueryAttemptHandoff::commit(&mut handoff);
    assert_eq!(
        root.lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .publications,
        8
    );
    rue_query::QueryAttemptHandoff::abort(&mut handoff);
    let restored = root
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    assert_eq!(restored.publications, 7);
    assert_eq!(restored.additions, 11);
    assert_eq!(restored.deletions, 3);
}

fn named_type_instance(
    module: &ModuleId,
    name: &str,
    kind: crate::StableDefinitionKind,
) -> crate::TypeInstanceKey {
    crate::TypeInstanceKey::Nominal(crate::NominalInstanceKey::Named(
        crate::StableDefinitionKey::from_stable_parts(
            module.clone(),
            crate::StableDefinitionNamespace::Type,
            kind,
            name,
            None,
        ),
    ))
}

#[test]
fn durable_nominal_materialization_shares_the_canonical_signature_payload() {
    let snapshot = source_snapshot(
        &[(
            1,
            "/main.rue",
            "main.rue",
            "struct Point { x: i32, y: i64 }\nfn main() -> i32 { 0 }\n",
        )],
        1,
    );
    let module = ModuleId::from_logical_path("main.rue").unwrap();
    let point = crate::StableDefinitionKey::from_stable_parts(
        module,
        crate::StableDefinitionNamespace::Type,
        crate::StableDefinitionKind::Struct,
        Arc::from("Point"),
        None,
    );
    let mut database = RevisionedQueryDatabase::default();
    let revision = revision_for(&mut database, &snapshot);
    let outcome = database.probe_ready_body_facts(
        revision,
        semantic_configuration(),
        "durable-nominal-shared-payload",
        move |provider| {
            let source = CompilerBodyDurableSource::with_anonymous(provider, &[], None);
            let signature = source.signature(&point).expect("Point has a signature");
            let crate::semantic_query_nucleus::DeclarationSignatureProjection::Struct {
                fields: canonical_fields,
                ..
            } = signature.signature
            else {
                panic!("Point has a struct signature")
            };
            let nominal = rue_air::DurableNominalSource::nominal(&source, &point)
                .expect("Point has a durable nominal body");
            let rue_air::DurableNominalBody::Struct {
                fields: materialized_fields,
                ..
            } = nominal.body
            else {
                panic!("Point materializes as a struct")
            };
            Arc::ptr_eq(&canonical_fields, &materialized_fields)
        },
    );
    assert!(
        outcome.result,
        "the durable source must not rebuild an equivalent nominal field vector"
    );
}

#[test]
fn durable_function_materialization_shares_the_canonical_parameter_payload() {
    let snapshot = source_snapshot(
        &[(
            1,
            "/main.rue",
            "main.rue",
            "fn helper(value: i32, count: u64) -> i32 { value }\nfn main() -> i32 { helper(0, 1) }\n",
        )],
        1,
    );
    let module = ModuleId::from_logical_path("main.rue").unwrap();
    let helper = crate::StableDefinitionKey::from_stable_parts(
        module,
        crate::StableDefinitionNamespace::Value,
        crate::StableDefinitionKind::Function,
        Arc::from("helper"),
        None,
    );
    let mut database = RevisionedQueryDatabase::default();
    let revision = revision_for(&mut database, &snapshot);
    let outcome = database.probe_ready_body_facts(
        revision,
        semantic_configuration(),
        "durable-function-shared-parameters",
        move |provider| {
            let source = CompilerBodyDurableSource::with_anonymous(provider, &[], None);
            let signature = source.signature(&helper).expect("helper has a signature");
            let crate::semantic_query_nucleus::DeclarationSignatureProjection::Callable {
                parameters: canonical_parameters,
                ..
            } = signature.signature
            else {
                panic!("helper has a callable signature")
            };
            let function = rue_air::DurableCallableSource::function(&source, &helper)
                .expect("helper has a durable function body");
            Arc::ptr_eq(&canonical_parameters, &function.parameters)
        },
    );
    assert!(
        outcome.result,
        "the durable source must not rebuild an equivalent function-parameter vector"
    );
}

#[test]
fn durable_named_member_resolves_each_unique_candidate_with_one_probe() {
    let snapshot = source_snapshot(
        &[(
            1,
            "/main.rue",
            "main.rue",
            "struct Counter { value: i32, \
                 fn get(borrow self) -> i32 { self.value } \
                 fn make(value: i32) -> Counter { Counter { value: value } } }\n\
                 fn main() -> i32 { 0 }\n",
        )],
        1,
    );
    let counter = crate::StableDefinitionKey::from_stable_parts(
        ModuleId::from_logical_path("main.rue").unwrap(),
        crate::StableDefinitionNamespace::Type,
        crate::StableDefinitionKind::Struct,
        Arc::from("Counter"),
        None,
    );
    let mut database = RevisionedQueryDatabase::default();
    let revision = revision_for(&mut database, &snapshot);
    let outcome = database.probe_ready_body_facts(
        revision,
        semantic_configuration(),
        "durable-named-member-single-probes",
        move |provider| {
            let before = provider
                .meter()
                .method_candidates
                .load(std::sync::atomic::Ordering::Relaxed);
            let source = CompilerBodyDurableSource::with_anonymous(provider, &[], None);
            let get =
                rue_air::DurableBodyLookupSource::named_member(&source, &counter, "Counter", "get");
            let make = rue_air::DurableBodyLookupSource::named_member(
                &source, &counter, "Counter", "make",
            );
            let after = provider
                .meter()
                .method_candidates
                .load(std::sync::atomic::Ordering::Relaxed);
            (get, make, after - before)
        },
    );
    let (get, make, probes) = outcome.result;
    let (get, get_has_self) = get.expect("the sole instance method resolves");
    assert_eq!(get.kind(), crate::StableDefinitionKind::Method);
    assert!(get_has_self);
    let (make, make_has_self) = make.expect("the sole associated function resolves");
    assert_eq!(make.kind(), crate::StableDefinitionKind::AssociatedFunction);
    assert!(!make_has_self);
    assert_eq!(probes, 2, "each member name performs one candidate probe");
}

#[test]
fn durable_named_member_rejects_every_multi_candidate_shape() {
    fn candidate(declaration: u8, has_self_receiver: bool) -> rue_air::MemberCandidate<u8> {
        rue_air::MemberCandidate {
            declaration,
            name: Arc::from("conflict"),
            has_self_receiver,
            kind: if has_self_receiver {
                rue_air::MemberKind::Method
            } else {
                rue_air::MemberKind::AssociatedFunction
            },
            is_public: false,
        }
    }

    let sole_instance = unique_named_member_candidate(vec![candidate(1, true)])
        .expect("one instance method is unique");
    assert!(sole_instance.has_self_receiver);
    let sole_static = unique_named_member_candidate(vec![candidate(2, false)])
        .expect("one associated function is unique");
    assert!(!sole_static.has_self_receiver);
    assert!(
        unique_named_member_candidate::<u8>(Vec::new()).is_none(),
        "an absent member does not resolve"
    );
    assert!(
        unique_named_member_candidate(vec![candidate(3, true), candidate(4, true)]).is_none(),
        "two instance methods are ambiguous"
    );
    assert!(
        unique_named_member_candidate(vec![candidate(5, false), candidate(6, false)]).is_none(),
        "two associated functions are ambiguous"
    );
    assert!(
        unique_named_member_candidate(vec![candidate(7, true), candidate(8, false)]).is_none(),
        "an instance/static pair is ambiguous"
    );
}

fn request_layout(
    database: &RevisionedQueryDatabase,
    revision: Revision,
    ty: crate::TypeInstanceKey,
) -> QueryRequestAttempt<crate::type_queries::LayoutValue> {
    request_layout_for_target(database, revision, ty, crate::Target::X86_64Linux)
}

fn request_layout_for_target(
    database: &RevisionedQueryDatabase,
    revision: Revision,
    ty: crate::TypeInstanceKey,
    target: crate::Target,
) -> QueryRequestAttempt<crate::type_queries::LayoutValue> {
    database.runtime.request_registered(
        &database.layouts,
        revision,
        crate::type_queries::TypeQueryKey {
            ty,
            configuration: crate::semantic_query_nucleus::SemanticQueryConfiguration {
                target,
                preview_features: crate::StablePreviewFeatures::new(
                    &crate::PreviewFeatures::default(),
                ),
            },
        },
        CancellationToken::new(),
    )
}

fn assert_layout_parity(canonical: &crate::type_queries::CanonicalLayout, live: &rue_air::Layout) {
    use crate::type_queries::CanonicalLayoutKind as C;
    use rue_air::LayoutKind as L;
    assert_eq!(
        (canonical.size, canonical.alignment, canonical.stride),
        (live.size, live.alignment, live.stride)
    );
    match (&canonical.kind, &live.kind) {
        (
            C::Array {
                element: Some(canonical_element),
                count: canonical_count,
            },
            L::Array {
                element: live_element,
                count: live_count,
            },
        ) => {
            assert_eq!(canonical_count, live_count);
            assert_layout_parity(canonical_element, live_element);
        }
        (
            C::Struct {
                field_offsets,
                padding_ranges,
            },
            L::Struct {
                field_offsets: live_offsets,
                padding_ranges: live_padding,
            },
        ) => {
            assert_eq!(field_offsets.as_ref(), live_offsets);
            assert_eq!(padding_ranges.as_ref(), live_padding);
        }
        (
            C::Enum {
                tag_size,
                payload_offset,
                variants,
            },
            L::Enum {
                tag,
                payload_offset: live_payload_offset,
                variants: live_variants,
            },
        ) => {
            assert_eq!(*tag_size, tag.size);
            assert_eq!(payload_offset, live_payload_offset);
            assert_eq!(
                variants
                    .iter()
                    .map(|variant| variant.to_vec())
                    .collect::<Vec<_>>(),
                *live_variants
            );
        }
        (C::Scalar, L::Scalar) => {}
        (canonical, live) => panic!("layout kind mismatch: {canonical:?} != {live:?}"),
    }
}

#[test]
fn canonical_layout_matches_frozen_pool_for_padding_nested_arrays_and_enums() {
    use lasso::ThreadedRodeo;
    use rue_air::{EnumDef, StructDef, StructField, Type, TypeInternPool};

    let source = source_snapshot(
        &[(
            1,
            "/main.rue",
            "main.rue",
            "struct Padded { first: u8, aligned: u64, tail: u16 }\n\
                 enum Choice { Small(u8, u64), Wide(u32, u16, u64) }",
        )],
        1,
    );
    let module = ModuleId::from_logical_path("main.rue").unwrap();
    let padded_key = named_type_instance(&module, "Padded", crate::StableDefinitionKind::Struct);
    let choice_key = named_type_instance(&module, "Choice", crate::StableDefinitionKind::Enum);
    let inner_array_key = crate::TypeInstanceKey::Array {
        element: Node::new(padded_key.clone()),
        len: 2,
    };
    let outer_array_key = crate::TypeInstanceKey::Array {
        element: Node::new(inner_array_key),
        len: 3,
    };

    let pool = TypeInternPool::new();
    let interner = ThreadedRodeo::new();
    let padded_id = pool
        .register_struct(
            interner.get_or_intern("Padded"),
            StructDef {
                name: "Padded".into(),
                fields: vec![
                    StructField {
                        name: "first".into(),
                        ty: Type::U8,
                    },
                    StructField {
                        name: "aligned".into(),
                        ty: Type::U64,
                    },
                    StructField {
                        name: "tail".into(),
                        ty: Type::U16,
                    },
                ],
                is_copy: false,
                is_linear: false,
                declared_linear: false,
                destructor: None,
                is_builtin: false,
                is_pub: false,
                file_id: rue_span::FileId::DEFAULT,
            },
        )
        .0;
    let padded_ty = Type::new_struct(padded_id);
    let inner_array = pool.intern_array_from_type(padded_ty, 2);
    let outer_array = pool.intern_array_from_type(Type::new_array(inner_array), 3);
    let choice_id = pool
        .register_enum(
            interner.get_or_intern("Choice"),
            EnumDef {
                name: "Choice".into(),
                variants: Arc::from(["Small".into(), "Wide".into()]),
                variant_payloads: vec![
                    vec![Type::U8, Type::U64],
                    vec![Type::U32, Type::U16, Type::U64],
                ],
                is_pub: false,
                is_non_exhaustive: false,
                file_id: rue_span::FileId::DEFAULT,
            },
        )
        .0;
    let pool = pool.freeze();

    let mut database = RevisionedQueryDatabase::default();
    let revision = revision_for(&mut database, &source);
    for target in [crate::Target::X86_64Linux, crate::Target::Aarch64Linux] {
        for (stable, live) in [
            (padded_key.clone(), padded_ty),
            (outer_array_key.clone(), Type::new_array(outer_array)),
            (choice_key.clone(), Type::new_enum(choice_id)),
        ] {
            let attempt = request_layout_for_target(&database, revision, stable, target);
            let terminal = attempt.terminal().unwrap();
            let rue_query::QueryOutcome::Success(crate::type_queries::LayoutValue::Available(
                canonical,
            )) = terminal.outcome()
            else {
                panic!("layout query failed: {terminal:?}");
            };
            assert_layout_parity(canonical, &pool.layout(live));
        }
    }
}

#[test]
fn layout_observes_only_structural_by_value_dependencies() {
    let module = ModuleId::from_logical_path("main.rue").unwrap();
    let source = |text| source_snapshot(&[(1, "/main.rue", "main.rue", text)], 1);
    let first = source("struct Foo { value: i64 }");
    let destructor_only = source("struct Foo { value: i64 }\ndrop fn Foo(self) {}");
    let linearity_only = source("linear struct Foo { value: i64 }\ndrop fn Foo(self) {}");
    let shape_edit = source("linear struct Foo { value: i64, extra: i64 }\ndrop fn Foo(self) {}");
    let foo = named_type_instance(&module, "Foo", crate::StableDefinitionKind::Struct);
    let pointer = crate::TypeInstanceKey::PtrConst(Node::new(foo.clone()));
    let slice = crate::TypeInstanceKey::Slice {
        element: Node::new(foo.clone()),
        name: Arc::from("FooSlice"),
    };
    let zero_array = crate::TypeInstanceKey::Array {
        element: Node::new(foo.clone()),
        len: 0,
    };
    let mut database = RevisionedQueryDatabase::default();

    let first_revision = revision_for(&mut database, &first);
    let cold = request_layout(&database, first_revision, foo.clone());
    let cold_stamp = cold.terminal().unwrap().stamp();
    assert_eq!(cold.execution(), RequestExecution::Computed);
    for ty in [pointer.clone(), slice.clone(), zero_array.clone()] {
        assert_eq!(
            request_layout(&database, first_revision, ty).execution(),
            RequestExecution::Computed
        );
    }

    let destructor_revision = revision_for(&mut database, &destructor_only);
    let destructor = request_layout(&database, destructor_revision, foo.clone());
    assert_eq!(destructor.execution(), RequestExecution::Reused);
    assert_eq!(destructor.terminal().unwrap().stamp(), cold_stamp);

    let linear_revision = revision_for(&mut database, &linearity_only);
    let linear = request_layout(&database, linear_revision, foo.clone());
    assert_eq!(linear.execution(), RequestExecution::Reused);
    assert_eq!(linear.terminal().unwrap().stamp(), cold_stamp);

    let shape_revision = revision_for(&mut database, &shape_edit);
    let shape = request_layout(&database, shape_revision, foo);
    assert_eq!(shape.execution(), RequestExecution::Computed);
    assert_ne!(shape.terminal().unwrap().stamp(), cold_stamp);
    for ty in [pointer, slice, zero_array] {
        assert_eq!(
            request_layout(&database, shape_revision, ty).execution(),
            RequestExecution::Reused,
            "non-by-value containment must not observe the element edit"
        );
    }
}

fn request_call_abi(
    database: &RevisionedQueryDatabase,
    revision: Revision,
    callable: crate::FunctionInstanceKey,
    target: crate::Target,
) -> crate::type_queries::CallAbiFacts {
    let attempt = database.runtime.request_registered(
        &database.call_abis,
        revision,
        crate::type_queries::CallAbiQueryKey {
            callable,
            configuration: crate::semantic_query_nucleus::SemanticQueryConfiguration {
                target,
                preview_features: crate::StablePreviewFeatures::new(&crate::PreviewFeatures::from(
                    [rue_error::PreviewFeature::CFfi],
                )),
            },
        },
        CancellationToken::new(),
    );
    let terminal = attempt.terminal().unwrap();
    let rue_query::QueryOutcome::Success(crate::type_queries::CallAbiValue::Available(facts)) =
        terminal.outcome()
    else {
        panic!("call ABI query failed: {terminal:?}");
    };
    facts.clone()
}

/// The by-value C classifier must agree across every crossing site — calls,
/// returns, exports, and (once they exist) callbacks — for every supported type
/// shape, which is ADR-0064's ratified acceptance criterion. It must also agree
/// across the two planes: the live classifier walks a request-scoped type pool,
/// the stable query plane walks canonical layout values and revision-stable type
/// keys, and only the projections differ.
///
/// The shapes below are the ones that can disagree: every scalar kind and both
/// pointer flavors, and `@repr(c)`-shaped structs sitting on the eightbyte
/// boundaries (1, 2, 8, 9, 16, 17, 24 bytes), with interior padding, nested, and
/// with an array field. `@repr(c)` is a guarantee marker rather than a layout
/// change under the compact-layout default, so the source declares the shapes as
/// ordinary structs and the classification is byte-identical.
#[test]
fn the_c_by_value_classifier_agrees_across_sites_shapes_and_planes() {
    use lasso::ThreadedRodeo;
    use rue_air::{
        ArgConvention, CAbiTypeFacts, StructDef, StructField, Type, TypeInternPool,
        c_abi_type_facts, lower_c_signature,
    };
    use rue_codegen::export_thunk::ExportSignature;

    let source = source_snapshot(
        &[(
            1,
            "/main.rue",
            "main.rue",
            "struct One { a: u8 }\n\
             struct Two { a: u16 }\n\
             struct Eight { a: i64 }\n\
             struct Nine { a: [u8; 9] }\n\
             struct Sixteen { a: i64, b: i64 }\n\
             struct Seventeen { a: [u8; 17] }\n\
             struct TwentyFour { a: i64, b: i64, c: i64 }\n\
             struct Padded { a: u8, b: u32, c: u16 }\n\
             struct Nested { p: Padded, q: Eight }\n\
             struct Arrayed { xs: [i32; 5] }",
        )],
        1,
    );
    let module = ModuleId::from_logical_path("main.rue").unwrap();

    let pool = TypeInternPool::new();
    let interner = ThreadedRodeo::new();
    let register = |name: &str, fields: Vec<(&str, Type)>| {
        let id = pool
            .register_struct(
                interner.get_or_intern(name),
                StructDef {
                    name: name.into(),
                    fields: fields
                        .into_iter()
                        .map(|(field, ty)| StructField {
                            name: field.into(),
                            ty,
                        })
                        .collect(),
                    is_copy: false,
                    is_linear: false,
                    declared_linear: false,
                    destructor: None,
                    is_builtin: false,
                    is_pub: false,
                    file_id: rue_span::FileId::DEFAULT,
                },
            )
            .0;
        Type::new_struct(id)
    };
    let bytes9 = Type::new_array(pool.intern_array_from_type(Type::U8, 9));
    let bytes17 = Type::new_array(pool.intern_array_from_type(Type::U8, 17));
    let ints5 = Type::new_array(pool.intern_array_from_type(Type::I32, 5));
    let one = register("One", vec![("a", Type::U8)]);
    let two = register("Two", vec![("a", Type::U16)]);
    let eight = register("Eight", vec![("a", Type::I64)]);
    let nine = register("Nine", vec![("a", bytes9)]);
    let sixteen = register("Sixteen", vec![("a", Type::I64), ("b", Type::I64)]);
    let seventeen = register("Seventeen", vec![("a", bytes17)]);
    let twenty_four = register(
        "TwentyFour",
        vec![("a", Type::I64), ("b", Type::I64), ("c", Type::I64)],
    );
    let padded = register(
        "Padded",
        vec![("a", Type::U8), ("b", Type::U32), ("c", Type::U16)],
    );
    let nested = register("Nested", vec![("p", padded), ("q", eight)]);
    let arrayed = register("Arrayed", vec![("xs", ints5)]);
    let pointee = Type::new_ptr_const(pool.intern_ptr_const_from_type(Type::I32));
    let mut_pointee = Type::new_ptr_mut(pool.intern_ptr_mut_from_type(Type::I32));
    let pool = pool.freeze();

    let named =
        |name: &str| named_type_instance(&module, name, crate::StableDefinitionKind::Struct);
    let shapes: Vec<(&str, crate::TypeInstanceKey, Type)> = vec![
        ("i8", crate::TypeInstanceKey::I8, Type::I8),
        ("i16", crate::TypeInstanceKey::I16, Type::I16),
        ("i32", crate::TypeInstanceKey::I32, Type::I32),
        ("i64", crate::TypeInstanceKey::I64, Type::I64),
        ("u8", crate::TypeInstanceKey::U8, Type::U8),
        ("u16", crate::TypeInstanceKey::U16, Type::U16),
        ("u32", crate::TypeInstanceKey::U32, Type::U32),
        ("u64", crate::TypeInstanceKey::U64, Type::U64),
        ("bool", crate::TypeInstanceKey::Bool, Type::BOOL),
        (
            "*const i32",
            crate::TypeInstanceKey::PtrConst(Node::new(crate::TypeInstanceKey::I32)),
            pointee,
        ),
        (
            "*mut i32",
            crate::TypeInstanceKey::PtrMut(Node::new(crate::TypeInstanceKey::I32)),
            mut_pointee,
        ),
        ("One", named("One"), one),
        ("Two", named("Two"), two),
        ("Eight", named("Eight"), eight),
        ("Nine", named("Nine"), nine),
        ("Sixteen", named("Sixteen"), sixteen),
        ("Seventeen", named("Seventeen"), seventeen),
        ("TwentyFour", named("TwentyFour"), twenty_four),
        ("Padded", named("Padded"), padded),
        ("Nested", named("Nested"), nested),
        ("Arrayed", named("Arrayed"), arrayed),
    ];

    let mut database = RevisionedQueryDatabase::default();
    let revision = revision_for(&mut database, &source);
    // Coverage of the placement classes the shape set is chosen to reach, so a
    // future edit that quietly stops exercising one of them fails here rather
    // than passing vacuously.
    let mut seen_registers = false;
    let mut seen_stack = false;
    let mut seen_indirect = false;
    let mut seen_sret = false;
    for target in crate::Target::all().iter().copied() {
        let convention = target.c_calling_convention();
        for (name, stable_key, live_ty) in &shapes {
            let attempt =
                request_layout_for_target(&database, revision, stable_key.clone(), target);
            let terminal = attempt.terminal().unwrap();
            let rue_query::QueryOutcome::Success(crate::type_queries::LayoutValue::Available(
                canonical,
            )) = terminal.outcome()
            else {
                panic!("layout query failed for {name}: {terminal:?}");
            };
            let stable_facts =
                super::super::semantic::stable_c_abi_type_facts(canonical, stable_key);
            let live_facts = c_abi_type_facts(&pool, *live_ty);
            assert_eq!(
                stable_facts, live_facts,
                "{target:?}/{name}: the two planes must project one set of C facts"
            );

            // The signature under test: one by-value parameter of this shape
            // returning the same shape, plus a nine-scalar tail that forces the
            // argument roster to overflow, so every site is compared on a
            // register case and a stacked case at once.
            let mut parameters = vec![(live_facts, ArgConvention::ByValue)];
            parameters.extend(std::iter::repeat_n(
                (
                    CAbiTypeFacts::Scalar {
                        kind: rue_air::CAbiScalarKind::RegisterWidth,
                        class: rue_target::CRegisterClass::Gp,
                    },
                    ArgConvention::ByValue,
                ),
                9,
            ));
            let call = lower_c_signature(convention, &parameters, live_facts);

            let mut stable_parameters = parameters.clone();
            stable_parameters[0] = (stable_facts, ArgConvention::ByValue);
            assert_eq!(
                call,
                lower_c_signature(convention, &stable_parameters, stable_facts),
                "{target:?}/{name}: the stable plane must place every value identically"
            );

            let mut param_types = vec![*live_ty];
            param_types.extend(std::iter::repeat_n(Type::I64, 9));
            let export = ExportSignature::for_types(&pool, convention, &param_types, *live_ty);
            assert_eq!(
                call,
                export.lowered(),
                "{target:?}/{name}: an export must read the placement a call writes"
            );

            for argument in call.arguments() {
                match argument.location {
                    rue_air::ArgLocation::Registers { .. } => seen_registers = true,
                    rue_air::ArgLocation::Stack { .. } => seen_stack = true,
                    rue_air::ArgLocation::Indirect { .. } => seen_indirect = true,
                    rue_air::ArgLocation::Omitted => {}
                }
            }
            seen_sret |= call.ret().uses_sret();
        }
    }
    assert!(
        seen_registers && seen_stack && seen_indirect && seen_sret,
        "the shape set must reach every placement class: registers={seen_registers} \
         stack={seen_stack} indirect={seen_indirect} sret={seen_sret}"
    );
}

#[test]
fn call_abi_classifies_native_target_c_named_destructor_and_drop_glue_on_both_targets() {
    use crate::type_queries::{CallAbiArgumentClass as A, CallAbiReturnClass as R};
    use rue_target::CallingConvention as C;
    let source = source_snapshot(
        &[(
            1,
            "/main.rue",
            "main.rue",
            "fn native(value: [u64; 7]) -> [u64; 7] { value }\n\
                 extern \"C\" { fn foreign(value: u32) -> u32; }\n\
                 pub extern \"C\" fn exported(value: u32) -> u32 { value }\n\
                 pub struct Owner { value: i64 }\n\
                 drop fn Owner(self) {}",
        )],
        1,
    );
    let module = ModuleId::from_logical_path("main.rue").unwrap();
    let mut database = RevisionedQueryDatabase::default();
    let revision = revision_for(&mut database, &source);
    let native = free_function_instance(&module, "native");
    let foreign = free_function_instance(&module, "foreign");
    let exported = free_function_instance(&module, "exported");
    let owner = named_type_instance(&module, "Owner", crate::StableDefinitionKind::Struct);
    let destructor =
        crate::FunctionInstanceKey::Definition(crate::StableDefinitionKey::from_stable_parts(
            module,
            crate::StableDefinitionNamespace::Destructor,
            crate::StableDefinitionKind::Destructor,
            "Owner",
            Some((crate::StableDefinitionKind::Struct, Arc::from("Owner"))),
        ));
    for target in [crate::Target::X86_64Linux, crate::Target::Aarch64Linux] {
        let native = request_call_abi(&database, revision, native.clone(), target);
        assert_eq!(native.convention, C::Rue);
        assert!(native.native_symbol.is_some());
        assert_eq!(
            native.return_class,
            if target == crate::Target::X86_64Linux {
                R::NativeIndirect { slots: 7 }
            } else {
                R::NativeRegisters { slots: 7 }
            }
        );

        let foreign = request_call_abi(&database, revision, foreign.clone(), target);
        assert_eq!(foreign.convention, C::c_for_target(target));
        assert!(foreign.native_symbol.is_none());
        assert_eq!(
            foreign.return_class,
            R::Scalar {
                extension: rue_air::ScalarAbiExtension::Unsigned { from_bits: 32 }
            }
        );
        assert!(matches!(
            foreign.arguments[0].class,
            A::CScalar {
                extension: rue_air::ScalarAbiExtension::Unsigned { from_bits: 32 }
            }
        ));

        let exported = request_call_abi(&database, revision, exported.clone(), target);
        assert_eq!(exported.convention, C::Rue);
        assert_eq!(
            exported.return_class,
            R::Scalar {
                extension: rue_air::ScalarAbiExtension::None
            }
        );

        let destructor = request_call_abi(&database, revision, destructor.clone(), target);
        assert_eq!(destructor.convention, C::Rue);
        assert_eq!(destructor.arguments.len(), 1);
        assert!(matches!(
            destructor.arguments[0].class,
            A::NativeDirect { slots: 1 }
        ));

        let glue = request_call_abi(
            &database,
            revision,
            crate::FunctionInstanceKey::DropGlue(Node::new(owner.clone())),
            target,
        );
        assert_eq!(glue.convention, C::Rue);
        assert_eq!(glue.return_class, R::ZeroSized);
        assert!(matches!(
            glue.arguments[0].class,
            A::NativeDirect { slots: 1 }
        ));
    }
}

#[test]
fn call_abi_batches_layouts_across_mixed_modes_and_duplicate_parameter_types() {
    use crate::type_queries::{CallAbiArgumentClass as A, CallAbiReturnClass as R};
    use rue_target::CallingConvention as C;
    // `mixed` interleaves reference and by-value parameters and repeats
    // `[u64; 7]`, so the batch is sparser than the parameter list and
    // carries a duplicate key. `scalars` repeats `u32` under Target-C.
    let source = source_snapshot(
        &[(
            1,
            "/main.rue",
            "main.rue",
            "struct Pair { a: i64, b: i64 }\n\
                 fn mixed(borrow left: Pair, first: [u64; 7], inout right: Pair, \
                 second: [u64; 7], tail: i32) -> [u64; 7] { second }\n\
                 extern \"C\" { fn scalars(first: u32, second: u32, third: u64) -> u32; }",
        )],
        1,
    );
    let module = ModuleId::from_logical_path("main.rue").unwrap();
    let mut database = RevisionedQueryDatabase::default();
    let revision = revision_for(&mut database, &source);
    let mixed = free_function_instance(&module, "mixed");
    let scalars = free_function_instance(&module, "scalars");

    for target in [crate::Target::X86_64Linux, crate::Target::Aarch64Linux] {
        let mixed = request_call_abi(&database, revision, mixed.clone(), target);
        assert_eq!(mixed.convention, C::Rue);
        assert_eq!(mixed.arguments.len(), 5);

        // Reference parameters stay layout-free and keep one value slot,
        // and the by-value parameters keep their signature positions even
        // though only they contributed a batch key.
        for index in [0, 2] {
            assert_eq!(mixed.arguments[index].class, A::Reference);
            assert_eq!(mixed.arguments[index].value_slots, 1);
        }
        // The duplicated `[u64; 7]` classifies identically at both
        // positions: one repeated key, one answer.
        assert_eq!(mixed.arguments[1].class, mixed.arguments[3].class);
        assert_eq!(mixed.arguments[1].value_slots, 7);
        assert_eq!(mixed.arguments[3].value_slots, 7);
        assert!(matches!(
            mixed.arguments[1].class,
            A::NativeDirect { slots: 7 } | A::NativeIndirect
        ));
        assert_eq!(mixed.arguments[4].class, A::NativeDirect { slots: 1 });
        // The result layout is the last entry of the same batch.
        assert_eq!(
            mixed.return_class,
            if target == crate::Target::X86_64Linux {
                R::NativeIndirect { slots: 7 }
            } else {
                R::NativeRegisters { slots: 7 }
            }
        );

        let scalars = request_call_abi(&database, revision, scalars.clone(), target);
        assert_eq!(scalars.convention, C::c_for_target(target));
        assert_eq!(scalars.arguments.len(), 3);
        assert_eq!(scalars.arguments[0].class, scalars.arguments[1].class);
        assert_eq!(
            scalars.arguments[0].class,
            A::CScalar {
                extension: rue_air::ScalarAbiExtension::Unsigned { from_bits: 32 }
            }
        );
        assert_eq!(
            scalars.arguments[2].class,
            A::CScalar {
                extension: rue_air::ScalarAbiExtension::None
            }
        );
        assert_eq!(
            scalars.return_class,
            R::Scalar {
                extension: rue_air::ScalarAbiExtension::Unsigned { from_bits: 32 }
            }
        );
    }
}

#[test]
fn call_abi_resolves_value_specialized_array_layout_on_both_targets() {
    use crate::type_queries::{CallAbiArgumentClass as A, CallAbiReturnClass as R};
    use rue_target::CallingConvention as C;
    let source = source_snapshot(
        &[(
            1,
            "/main.rue",
            "main.rue",
            "fn named(comptime N: i32, value: u64) -> u64 { value + N }\n\
                 fn sized(comptime N: i32, value: [u64; N]) -> [u64; N] { value }",
        )],
        1,
    );
    let module = ModuleId::from_logical_path("main.rue").unwrap();
    let named = free_function_instance(&module, "named");
    let callable = crate::FunctionInstanceKey::Specialization {
        base: Node::new(free_function_instance(&module, "sized")),
        arguments: crate::CanonicalArguments {
            types: Arc::from([]),
            values: Arc::from([crate::CanonicalArgumentValue::Integer(7)]),
        },
    };
    let mut database = RevisionedQueryDatabase::default();
    let revision = revision_for(&mut database, &source);
    for target in [crate::Target::X86_64Linux, crate::Target::Aarch64Linux] {
        let named = request_call_abi(&database, revision, named.clone(), target);
        assert_eq!(named.convention, C::Rue);
        assert_eq!(
            named.return_class,
            R::Scalar {
                extension: rue_air::ScalarAbiExtension::None
            }
        );
        assert_eq!(named.arguments.len(), 2);
        assert!(
            named
                .arguments
                .iter()
                .all(|argument| matches!(argument.class, A::NativeDirect { slots: 1 }))
        );

        let facts = request_call_abi(&database, revision, callable.clone(), target);
        assert_eq!(facts.convention, C::Rue);
        assert_eq!(
            facts.return_class,
            if target == crate::Target::X86_64Linux {
                R::NativeIndirect { slots: 7 }
            } else {
                R::NativeRegisters { slots: 7 }
            }
        );
        assert_eq!(facts.arguments.len(), 2);
        assert!(matches!(
            facts.arguments[0].class,
            A::NativeDirect { slots: 1 }
        ));
        assert_eq!(facts.arguments[0].value_slots, 1);
        assert!(matches!(
            facts.arguments[1].class,
            A::NativeDirect { slots: 7 }
        ));
        assert_eq!(facts.arguments[1].value_slots, 7);
    }
}

#[test]
fn call_abi_derives_anonymous_destructor_signature_from_its_exact_producer() {
    use crate::type_queries::{CallAbiArgumentClass as A, CallAbiReturnClass as R};
    use rue_target::CallingConvention as C;
    let source = source_snapshot(
        &[(
            1,
            "/main.rue",
            "main.rue",
            "fn Box() -> type { struct { value: i64, drop fn(self) {} } }",
        )],
        1,
    );
    let module = ModuleId::from_logical_path("main.rue").unwrap();
    let producer = crate::FunctionInstanceKey::Specialization {
        base: Node::new(free_function_instance(&module, "Box")),
        arguments: crate::CanonicalArguments::default(),
    };
    let configuration = semantic_configuration();
    let mut database = RevisionedQueryDatabase::default();
    let revision = revision_for(&mut database, &source);
    let produced = database.runtime.request_registered(
        &database.body_produced_anonymous,
        revision,
        crate::body_query::BodyQueryKey::new(producer, configuration),
        CancellationToken::new(),
    );
    let terminal = produced.terminal().unwrap();
    let rue_query::QueryOutcome::Success(crate::body_query::ProducedAnonymous::Produced(produced)) =
        terminal.outcome()
    else {
        panic!("anonymous producer failed: {terminal:?}");
    };
    let owner = crate::TypeInstanceKey::Nominal(crate::NominalInstanceKey::Anonymous(Node::new(
        produced.0[0].identity.clone(),
    )));
    let callable = crate::FunctionInstanceKey::AnonymousMember {
        owner: Node::new(owner),
        member: crate::AnonymousMemberKey {
            kind: crate::AnonymousMemberKind::Destructor,
            name: Arc::from("__drop"),
        },
    };
    for target in [crate::Target::X86_64Linux, crate::Target::Aarch64Linux] {
        let facts = request_call_abi(&database, revision, callable.clone(), target);
        assert_eq!(facts.convention, C::Rue);
        assert_eq!(facts.return_class, R::ZeroSized);
        assert_eq!(facts.arguments.len(), 1);
        assert!(matches!(
            facts.arguments[0].class,
            A::NativeDirect { slots: 1 }
        ));
    }
}

/// One stable argument classification against the live classifier's answer
/// for the same type under the same convention.
fn assert_native_arg_parity(
    stable: &crate::type_queries::CallAbiArgument,
    live_class: rue_air::ArgClass,
    live_width: u32,
    context: &str,
) {
    use crate::type_queries::CallAbiArgumentClass as A;
    assert_eq!(
        stable.value_slots, live_width,
        "value-slot width parity for {context}"
    );
    match (stable.class, live_class) {
        (A::Omitted, rue_air::ArgClass::Omitted) => {}
        (A::NativeDirect { slots }, rue_air::ArgClass::Direct { slot_count }) => {
            assert_eq!(slots, slot_count, "direct slot parity for {context}");
        }
        (A::NativeIndirect, rue_air::ArgClass::Indirect) => {}
        (A::Reference, rue_air::ArgClass::Indirect) => {}
        (stable, live) => {
            panic!("argument classification parity mismatch for {context}: {stable:?} != {live:?}")
        }
    }
}

/// One stable return classification against the live classifier's answer.
fn assert_native_return_parity(
    stable: crate::type_queries::CallAbiReturnClass,
    live: rue_air::ReturnClass,
    context: &str,
) {
    use crate::type_queries::CallAbiReturnClass as R;
    match (stable, live) {
        (R::ZeroSized, rue_air::ReturnClass::ZeroSized) => {}
        (
            R::Scalar {
                extension: rue_air::ScalarAbiExtension::None,
            },
            rue_air::ReturnClass::Scalar,
        ) => {}
        (R::NativeRegisters { slots }, rue_air::ReturnClass::Registers { slot_count }) => {
            assert_eq!(slots, slot_count, "register slot parity for {context}");
        }
        (R::NativeIndirect { slots }, rue_air::ReturnClass::Indirect { slot_count }) => {
            assert_eq!(slots, slot_count, "indirect slot parity for {context}");
        }
        (stable, live) => {
            panic!("return classification parity mismatch for {context}: {stable:?} != {live:?}")
        }
    }
}

#[test]
fn call_abi_native_classification_matches_the_live_classifier_on_both_targets() {
    use lasso::ThreadedRodeo;
    use rue_air::{
        ArgConvention, EnumDef, NativeCallAbi, StructDef, StructField, Type, TypeInternPool,
    };
    let source = source_snapshot(
        &[(
            1,
            "/main.rue",
            "main.rue",
            "struct Empty {}\n\
                 struct Wide { a: u64, b: u64 }\n\
                 struct Narrow { a: u32, b: u32 }\n\
                 struct OneNarrow { a: u32 }\n\
                 struct Nested { inner: Wide, tail: [u64; 2] }\n\
                 struct NestedNarrow { inner: Narrow, tail: u64 }\n\
                 enum Flag { A, B }\n\
                 enum Choice { Small(u8, u64), Wide(u32, u16, u64) }\n\
                 fn scalars(a: i32, b: u32, c: bool, d: i64) -> i64 { d }\n\
                 fn zero(e: Empty) -> Empty { e }\n\
                 fn refs(inout a: i64, borrow b: Wide) {}\n\
                 fn five(v: [u64; 5]) -> [u64; 5] { v }\n\
                 fn six(v: [u64; 6]) -> [u64; 6] { v }\n\
                 fn seven(v: [u64; 7]) -> [u64; 7] { v }\n\
                 fn eight(v: [u64; 8]) -> [u64; 8] { v }\n\
                 fn nine(v: [u64; 9]) -> [u64; 9] { v }\n\
                 fn wide(v: Wide) -> Wide { v }\n\
                 fn narrow(v: Narrow) -> Narrow { v }\n\
                 fn one_narrow(v: OneNarrow) -> OneNarrow { v }\n\
                 fn nested(v: Nested) -> Nested { v }\n\
                 fn nested_narrow(v: NestedNarrow) -> NestedNarrow { v }\n\
                 fn flag(v: Flag) -> Flag { v }\n\
                 fn choice(v: Choice) -> Choice { v }",
        )],
        1,
    );
    let module = ModuleId::from_logical_path("main.rue").unwrap();

    // Mirror the source types into a live pool so the stable query can be
    // compared against the live classifier's answer for the same shapes.
    let pool = TypeInternPool::new();
    let interner = ThreadedRodeo::new();
    let struct_def = |name: &str, fields: Vec<(&str, Type)>| StructDef {
        name: name.into(),
        fields: fields
            .into_iter()
            .map(|(name, ty)| StructField {
                name: name.into(),
                ty,
            })
            .collect(),
        is_copy: false,
        is_linear: false,
        declared_linear: false,
        destructor: None,
        is_builtin: false,
        is_pub: false,
        file_id: rue_span::FileId::DEFAULT,
    };
    let register = |name: &str, fields: Vec<(&str, Type)>| {
        Type::new_struct(
            pool.register_struct(interner.get_or_intern(name), struct_def(name, fields))
                .0,
        )
    };
    let empty = register("Empty", vec![]);
    let wide = register("Wide", vec![("a", Type::U64), ("b", Type::U64)]);
    let narrow = register("Narrow", vec![("a", Type::U32), ("b", Type::U32)]);
    let one_narrow = register("OneNarrow", vec![("a", Type::U32)]);
    let tail = Type::new_array(pool.intern_array_from_type(Type::U64, 2));
    let nested = register("Nested", vec![("inner", wide), ("tail", tail)]);
    let nested_narrow = register("NestedNarrow", vec![("inner", narrow), ("tail", Type::U64)]);
    let flag = Type::new_enum(
        pool.register_enum(
            interner.get_or_intern("Flag"),
            EnumDef {
                name: "Flag".into(),
                variants: Arc::from(["A".into(), "B".into()]),
                variant_payloads: vec![vec![], vec![]],
                is_pub: false,
                is_non_exhaustive: false,
                file_id: rue_span::FileId::DEFAULT,
            },
        )
        .0,
    );
    let choice = Type::new_enum(
        pool.register_enum(
            interner.get_or_intern("Choice"),
            EnumDef {
                name: "Choice".into(),
                variants: Arc::from(["Small".into(), "Wide".into()]),
                variant_payloads: vec![
                    vec![Type::U8, Type::U64],
                    vec![Type::U32, Type::U16, Type::U64],
                ],
                is_pub: false,
                is_non_exhaustive: false,
                file_id: rue_span::FileId::DEFAULT,
            },
        )
        .0,
    );
    let arrays: Vec<Type> = (5u64..=9)
        .map(|len| Type::new_array(pool.intern_array_from_type(Type::U64, len)))
        .collect();
    let pool = pool.freeze();

    let mut database = RevisionedQueryDatabase::default();
    let revision = revision_for(&mut database, &source);
    for (target, budget) in [
        (crate::Target::X86_64Linux, 6u32),
        (crate::Target::Aarch64Linux, 8u32),
    ] {
        let live = NativeCallAbi::new(&pool, budget);
        let by_value = ArgConvention::ByValue;
        let cases: Vec<(&str, Vec<(ArgConvention, Type)>, Type)> = vec![
            (
                "scalars",
                vec![
                    (by_value, Type::I32),
                    (by_value, Type::U32),
                    (by_value, Type::BOOL),
                    (by_value, Type::I64),
                ],
                Type::I64,
            ),
            ("zero", vec![(by_value, empty)], empty),
            (
                "refs",
                vec![
                    (ArgConvention::ByReference, Type::I64),
                    (ArgConvention::ByReference, wide),
                ],
                Type::UNIT,
            ),
            ("five", vec![(by_value, arrays[0])], arrays[0]),
            ("six", vec![(by_value, arrays[1])], arrays[1]),
            ("seven", vec![(by_value, arrays[2])], arrays[2]),
            ("eight", vec![(by_value, arrays[3])], arrays[3]),
            ("nine", vec![(by_value, arrays[4])], arrays[4]),
            ("wide", vec![(by_value, wide)], wide),
            ("narrow", vec![(by_value, narrow)], narrow),
            ("one_narrow", vec![(by_value, one_narrow)], one_narrow),
            ("nested", vec![(by_value, nested)], nested),
            (
                "nested_narrow",
                vec![(by_value, nested_narrow)],
                nested_narrow,
            ),
            ("choice", vec![(by_value, choice)], choice),
        ];
        for (name, params, result) in &cases {
            let facts = request_call_abi(
                &database,
                revision,
                free_function_instance(&module, name),
                target,
            );
            assert_eq!(
                facts.convention,
                rue_target::CallingConvention::Rue,
                "{name} is a native callable"
            );
            assert_eq!(facts.arguments.len(), params.len(), "arity of {name}");
            for (argument, (convention, ty)) in facts.arguments.iter().zip(params) {
                assert_native_arg_parity(
                    argument,
                    live.classify_arg(*ty, *convention),
                    live.arg_slot_width(*ty, *convention),
                    &format!("{name} on {target:?}"),
                );
            }
            assert_native_return_parity(
                facts.return_class,
                live.classify_return(*result),
                &format!("{name} return on {target:?}"),
            );
        }

        // The discriminant-only enum is the one deliberate projection
        // divergence between the planes: the live classifier reports its
        // single tag slot as `Scalar`, while the stable projection keeps
        // reporting the aggregate as one register slot. The physical
        // crossing is identical (one register); this pin keeps the
        // divergence visible instead of letting it drift silently.
        let flag_facts = request_call_abi(
            &database,
            revision,
            free_function_instance(&module, "flag"),
            target,
        );
        assert_eq!(
            live.classify_return(flag),
            rue_air::ReturnClass::Scalar,
            "live plane reports a discriminant-only enum return as a scalar"
        );
        assert_eq!(
            flag_facts.return_class,
            crate::type_queries::CallAbiReturnClass::NativeRegisters { slots: 1 },
            "stable plane projects a discriminant-only enum return as one register slot"
        );
        assert_native_arg_parity(
            &flag_facts.arguments[0],
            live.classify_arg(flag, ArgConvention::ByValue),
            live.arg_slot_width(flag, ArgConvention::ByValue),
            &format!("flag argument on {target:?}"),
        );

        // Pin the classification outcomes themselves, not only the
        // cross-plane agreement: zero-sized values vanish, a slot-identical
        // aggregate stays direct, a multi-slot narrow-leaf aggregate is
        // forced indirect by the compact memory-first rule, and a
        // single-slot narrow aggregate stays direct (RUE-1035).
        use crate::type_queries::{CallAbiArgumentClass as A, CallAbiReturnClass as R};
        let request = |name: &str| {
            request_call_abi(
                &database,
                revision,
                free_function_instance(&module, name),
                target,
            )
        };
        let zero = request("zero");
        assert!(matches!(zero.arguments[0].class, A::Omitted));
        assert_eq!(zero.return_class, R::ZeroSized);
        let wide_facts = request("wide");
        assert!(matches!(
            wide_facts.arguments[0].class,
            A::NativeDirect { slots: 2 }
        ));
        assert_eq!(wide_facts.return_class, R::NativeRegisters { slots: 2 });
        let narrow_facts = request("narrow");
        assert!(matches!(narrow_facts.arguments[0].class, A::NativeIndirect));
        assert_eq!(narrow_facts.return_class, R::NativeIndirect { slots: 2 });
        let one_narrow_facts = request("one_narrow");
        assert!(matches!(
            one_narrow_facts.arguments[0].class,
            A::NativeDirect { slots: 1 }
        ));
        assert_eq!(
            one_narrow_facts.return_class,
            R::NativeRegisters { slots: 1 }
        );
        let nested_narrow_facts = request("nested_narrow");
        assert!(matches!(
            nested_narrow_facts.arguments[0].class,
            A::NativeIndirect
        ));
        let refs_facts = request("refs");
        assert!(matches!(refs_facts.arguments[0].class, A::Reference));
        assert!(matches!(refs_facts.arguments[1].class, A::Reference));

        // Pin the return-register budget boundary explicitly: budget - 1
        // and budget fit in registers, budget + 1 goes indirect.
        let boundary = |name: &str| request(name).return_class;
        match target {
            crate::Target::X86_64Linux => {
                assert_eq!(boundary("five"), R::NativeRegisters { slots: 5 });
                assert_eq!(boundary("six"), R::NativeRegisters { slots: 6 });
                assert_eq!(boundary("seven"), R::NativeIndirect { slots: 7 });
            }
            _ => {
                assert_eq!(boundary("seven"), R::NativeRegisters { slots: 7 });
                assert_eq!(boundary("eight"), R::NativeRegisters { slots: 8 });
                assert_eq!(boundary("nine"), R::NativeIndirect { slots: 9 });
            }
        }
    }
}

#[test]
fn call_abi_target_c_classification_matches_the_live_classifier_on_both_targets() {
    use lasso::ThreadedRodeo;
    use rue_air::{StructDef, StructField, TargetCCallAbi, Type, TypeInternPool};
    let source = source_snapshot(
        &[(
            1,
            "/main.rue",
            "main.rue",
            "@repr(c)\n\
                 struct CInner { a: i32, b: i32 }\n\
                 @repr(c)\n\
                 struct CTwelve { a: i32, b: i32, c: i32 }\n\
                 @repr(c)\n\
                 struct CNested { inner: CInner, tail: i64 }\n\
                 @repr(c)\n\
                 struct CLarge { a: i64, b: i64, c: i64 }\n\
                 extern \"C\" {\n\
                     fn c_signed(a: i8, b: i16, c: i32, d: i64) -> i16;\n\
                     fn c_unsigned(a: u8, b: u16, c: u32, d: u64, e: bool) -> u16;\n\
                     fn c_pointers(p: ptr const u8, q: ptr mut u8) -> ptr mut u8;\n\
                     fn c_eight(v: CInner) -> CInner;\n\
                     fn c_twelve(v: CTwelve) -> CTwelve;\n\
                     fn c_sixteen(v: CNested) -> CNested;\n\
                     fn c_large(v: CLarge) -> CLarge;\n\
                 }",
        )],
        1,
    );
    let module = ModuleId::from_logical_path("main.rue").unwrap();

    let pool = TypeInternPool::new();
    let interner = ThreadedRodeo::new();
    let register = |name: &str, fields: Vec<(&str, Type)>| {
        Type::new_struct(
            pool.register_struct(
                interner.get_or_intern(name),
                StructDef {
                    name: name.into(),
                    fields: fields
                        .into_iter()
                        .map(|(name, ty)| StructField {
                            name: name.into(),
                            ty,
                        })
                        .collect(),
                    is_copy: false,
                    is_linear: false,
                    declared_linear: false,
                    destructor: None,
                    is_builtin: false,
                    is_pub: false,
                    file_id: rue_span::FileId::DEFAULT,
                },
            )
            .0,
        )
    };
    let c_inner = register("CInner", vec![("a", Type::I32), ("b", Type::I32)]);
    let c_twelve = register(
        "CTwelve",
        vec![("a", Type::I32), ("b", Type::I32), ("c", Type::I32)],
    );
    let c_nested = register("CNested", vec![("inner", c_inner), ("tail", Type::I64)]);
    let c_large = register(
        "CLarge",
        vec![("a", Type::I64), ("b", Type::I64), ("c", Type::I64)],
    );
    let ptr_const_u8 = Type::new_ptr_const(pool.intern_ptr_const_from_type(Type::U8));
    let ptr_mut_u8 = Type::new_ptr_mut(pool.intern_ptr_mut_from_type(Type::U8));
    let pool = pool.freeze();

    let assert_scalar_args = |facts: &crate::type_queries::CallAbiFacts,
                              abi: &TargetCCallAbi,
                              live_params: &[Type],
                              live_result: Type,
                              name: &str| {
        use crate::type_queries::{CallAbiArgumentClass as A, CallAbiReturnClass as R};
        assert_eq!(facts.arguments.len(), live_params.len(), "arity of {name}");
        for (argument, live_ty) in facts.arguments.iter().zip(live_params) {
            let A::CScalar { extension } = argument.class else {
                panic!("{name} argument is a target-C scalar: {:?}", argument.class);
            };
            assert_eq!(
                extension,
                abi.scalar_arg_extension(*live_ty),
                "argument extension parity for {name}"
            );
        }
        let R::Scalar { extension } = facts.return_class else {
            panic!(
                "{name} return is a target-C scalar: {:?}",
                facts.return_class
            );
        };
        assert_eq!(
            extension,
            abi.scalar_return_extension(live_result),
            "return extension parity for {name}"
        );
    };
    // The stable plane's aggregate classes must name the same places the live
    // plane's signature lowering does, for the one-parameter signature each of
    // these declarations has.
    let assert_aggregate = |facts: &crate::type_queries::CallAbiFacts,
                            convention: rue_target::CallingConvention,
                            live_ty: Type,
                            name: &str| {
        use crate::type_queries::{CallAbiArgumentClass as A, CallAbiReturnClass as R};
        let live_facts = rue_air::c_abi_type_facts(&pool, live_ty);
        let lowered = rue_air::lower_c_signature(
            convention,
            &[(live_facts, rue_air::ArgConvention::ByValue)],
            live_facts,
        );
        match (facts.arguments[0].class, lowered.arguments()[0].location) {
            (A::CIntegerRegisters { eightbytes }, rue_air::ArgLocation::Registers { pieces }) => {
                assert_eq!(eightbytes, pieces.len(), "eightbyte parity for {name}")
            }
            (
                A::CByValueStack { size, alignment },
                rue_air::ArgLocation::Stack {
                    size: live_size,
                    align: live_align,
                    ..
                },
            ) => assert_eq!(
                (size, alignment),
                (live_size, live_align),
                "byval parity for {name}"
            ),
            (
                A::CByReferenceCopy { size, alignment },
                rue_air::ArgLocation::Indirect {
                    size: live_size,
                    align: live_align,
                    ..
                },
            ) => assert_eq!(
                (size, alignment),
                (live_size, live_align),
                "reference-copy parity for {name}"
            ),
            (stable, live) => {
                panic!("aggregate argument parity mismatch for {name}: {stable:?} != {live:?}")
            }
        }
        match (facts.return_class, lowered.ret()) {
            (
                R::CIntegerRegisters { eightbytes },
                rue_air::LoweredReturn::Registers { count, .. },
            ) => assert_eq!(eightbytes, count, "return eightbyte parity for {name}"),
            (
                R::CIndirect { size, alignment },
                rue_air::LoweredReturn::Sret {
                    size: live_size,
                    align: live_align,
                    ..
                },
            ) => assert_eq!(
                (size, alignment),
                (live_size, live_align),
                "sret parity for {name}"
            ),
            (stable, live) => {
                panic!("aggregate return parity mismatch for {name}: {stable:?} != {live:?}")
            }
        }
    };

    let mut database = RevisionedQueryDatabase::default();
    let revision = revision_for(&mut database, &source);
    for target in [crate::Target::X86_64Linux, crate::Target::Aarch64Linux] {
        let convention = rue_target::CallingConvention::c_for_target(target);
        let abi = TargetCCallAbi::new(convention);
        let request = |name: &str| {
            request_call_abi(
                &database,
                revision,
                free_function_instance(&module, name),
                target,
            )
        };

        let signed = request("c_signed");
        assert_eq!(signed.convention, convention);
        assert_scalar_args(
            &signed,
            &abi,
            &[Type::I8, Type::I16, Type::I32, Type::I64],
            Type::I16,
            "c_signed",
        );
        assert_scalar_args(
            &request("c_unsigned"),
            &abi,
            &[Type::U8, Type::U16, Type::U32, Type::U64, Type::BOOL],
            Type::U16,
            "c_unsigned",
        );
        assert_scalar_args(
            &request("c_pointers"),
            &abi,
            &[ptr_const_u8, ptr_mut_u8],
            ptr_mut_u8,
            "c_pointers",
        );

        // Aggregates at 8, 12, 16, and 24 bytes: one eightbyte, rounding
        // up to two, exactly two, and past the 16-byte register limit
        // where the psABIs diverge (SysV byval stack, AAPCS64 reference
        // to a caller copy; sret for returns on both).
        assert_aggregate(&request("c_eight"), convention, c_inner, "c_eight");
        assert_aggregate(&request("c_twelve"), convention, c_twelve, "c_twelve");
        assert_aggregate(&request("c_sixteen"), convention, c_nested, "c_sixteen");
        assert_aggregate(&request("c_large"), convention, c_large, "c_large");
    }
}

#[test]
fn call_abi_strbuf_return_uses_sret_on_both_planes() {
    use lasso::ThreadedRodeo;
    use rue_air::{ArgConvention, NativeCallAbi, StructDef, StructField, Type, TypeInternPool};
    let snapshot = trusted_body_snapshot(
        "fn main() -> i32 { 0 }",
        None,
        Some((
            FileId::new(3),
            "pub struct StrBuf { buf: ptr mut u8, cap: u64, len: u64 }\n\
                 pub fn echo(v: StrBuf) -> StrBuf { v }",
        )),
    );
    let strbuf_module =
        ModuleId::from_trusted_standard_library_path(crate::STRBUF_MODULE_LOGICAL_PATH)
            .expect("the strbuf module path is inside the standard-library namespace");

    let pool = TypeInternPool::new();
    let interner = ThreadedRodeo::new();
    let ptr_mut_u8 = Type::new_ptr_mut(pool.intern_ptr_mut_from_type(Type::U8));
    let (strbuf_id, _) = pool.register_struct(
        interner.get_or_intern("StrBuf"),
        StructDef {
            name: "StrBuf".into(),
            fields: vec![
                StructField {
                    name: "buf".into(),
                    ty: ptr_mut_u8,
                },
                StructField {
                    name: "cap".into(),
                    ty: Type::U64,
                },
                StructField {
                    name: "len".into(),
                    ty: Type::U64,
                },
            ],
            is_copy: false,
            is_linear: false,
            declared_linear: false,
            destructor: None,
            is_builtin: false,
            is_pub: true,
            file_id: rue_span::FileId::DEFAULT,
        },
    );
    pool.set_struct_lang_item(strbuf_id, rue_air::LangItem::StrBuf);
    let strbuf = Type::new_struct(strbuf_id);
    let pool = pool.freeze();

    let mut database = RevisionedQueryDatabase::default();
    let revision =
        database.source_revision(&crate::session::ExactSourceInput::new(&snapshot), &snapshot);
    for (target, budget) in [
        (crate::Target::X86_64Linux, 6u32),
        (crate::Target::Aarch64Linux, 8u32),
    ] {
        let live = NativeCallAbi::new(&pool, budget);
        // The canonical StrBuf always returns through sret even though its
        // three slots fit the return-register budget, and its slot-identical
        // layout keeps by-value arguments direct.
        assert_eq!(
            live.classify_return(strbuf),
            rue_air::ReturnClass::Indirect { slot_count: 3 }
        );
        assert_eq!(
            live.classify_arg(strbuf, ArgConvention::ByValue),
            rue_air::ArgClass::Direct { slot_count: 3 }
        );
        let facts = request_call_abi(
            &database,
            revision,
            free_function_instance(&strbuf_module, "echo"),
            target,
        );
        assert_eq!(facts.convention, rue_target::CallingConvention::Rue);
        assert_eq!(
            facts.return_class,
            crate::type_queries::CallAbiReturnClass::NativeIndirect { slots: 3 }
        );
        assert_native_arg_parity(
            &facts.arguments[0],
            live.classify_arg(strbuf, ArgConvention::ByValue),
            live.arg_slot_width(strbuf, ArgConvention::ByValue),
            &format!("StrBuf echo argument on {target:?}"),
        );
    }
}

#[test]
fn anonymous_producer_preserves_a_deterministic_body_diagnostic_as_a_typed_failure() {
    let source = source_snapshot(
        &[(
            1,
            "/main.rue",
            "main.rue",
            "fn Bad() -> type {\n\
                     struct {\n\
                         x: i32,\n\
                         fn get(self) -> i32 { self.x }\n\
                         fn get(self) -> i32 { 0 }\n\
                     }\n\
                 }\n\
                 fn main() -> i32 {\n\
                     let B = Bad();\n\
                     0\n\
                 }",
        )],
        1,
    );
    let module = ModuleId::from_logical_path("main.rue").unwrap();
    let mut database = RevisionedQueryDatabase::default();
    let revision = revision_for(&mut database, &source);

    let request = database
        .body_closure(
            revision,
            crate::body_query::BodyClosureQueryKey {
                modules: Arc::from([module.clone()]),
                roots: Arc::from([free_function_instance(&module, "main")]),
                configuration: semantic_configuration(),
            },
            CancellationToken::new(),
        )
        .expect("a deterministic producer diagnostic is not query cancellation");
    let rue_query::QueryOutcome::Success(output) = request.terminal.outcome() else {
        unreachable!("BodyClosure publishes typed values")
    };
    assert!(output.bodies.iter().any(|body| matches!(
        body.bundle.outcome(),
        rue_query::QueryOutcome::Success(crate::body_query::BodyAnalysisBundle {
            transaction: crate::body_query::BodyTransaction::DeterministicFailure { .. },
            ..
        })
    )));
}

fn request_drop_glue(
    database: &RevisionedQueryDatabase,
    revision: Revision,
    ty: crate::TypeInstanceKey,
) -> QueryRequestAttempt<crate::type_queries::DropGlueValue> {
    database.runtime.request_registered(
        &database.drop_glues,
        revision,
        crate::type_queries::TypeQueryKey {
            ty,
            configuration: semantic_configuration(),
        },
        CancellationToken::new(),
    )
}

#[test]
fn type_facts_leaf_drop_matrix_covers_every_nonaggregate_variant() {
    use crate::{NominalInstanceKey as N, TypeInstanceKey as T};
    use rue_air::AnonymousNominalKind as K;

    let source = source_snapshot(&[(1, "/main.rue", "main.rue", "fn main() {}")], 1);
    let module = ModuleId::from_logical_path("main.rue").unwrap();
    let missing = crate::StableDefinitionKey::from_stable_parts(
        module.clone(),
        crate::StableDefinitionNamespace::Type,
        crate::StableDefinitionKind::Struct,
        "Missing",
        None,
    );
    let pointee = T::Nominal(N::Named(missing));
    let cases = [
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
        T::F32,
        T::F64,
        T::ComptimeFloat,
        T::BuiltinNominal {
            kind: K::Struct,
            name: Arc::from("str"),
        },
        T::BuiltinNominal {
            kind: K::Struct,
            name: Arc::from("UnknownBuiltin"),
        },
        T::Nominal(N::Builtin {
            kind: K::Struct,
            name: Arc::from("str"),
        }),
        T::Nominal(N::Builtin {
            kind: K::Enum,
            name: Arc::from("UnknownBuiltin"),
        }),
        T::PtrConst(Node::new(pointee.clone())),
        T::PtrMut(Node::new(pointee.clone())),
        T::Slice {
            element: Node::new(pointee),
            name: Arc::from("[]Missing"),
        },
        T::Module(module),
        T::GenericParameter(17),
    ];
    let mut database = RevisionedQueryDatabase::default();
    let revision = revision_for(&mut database, &source);
    for ty in cases {
        let attempt = database.runtime.request_registered(
            &database.type_facts,
            revision,
            crate::type_queries::TypeQueryKey {
                ty: ty.clone(),
                configuration: semantic_configuration(),
            },
            CancellationToken::new(),
        );
        let rue_query::QueryOutcome::Success(crate::type_queries::TypeFactsValue::Available(facts)) =
            attempt.terminal().unwrap().outcome()
        else {
            panic!("leaf TypeFacts must be available for {ty:?}")
        };
        assert!(!facts.needs_drop, "leaf unexpectedly needs drop: {ty:?}");
    }
}

#[test]
fn drop_glue_reads_the_shape_carried_by_type_facts_instead_of_requesting_it() {
    // RUE-1556: `TypeFacts` already carries the canonical `TypeShape` for
    // its own key — `evaluate_type_facts` stamps the shape it queried onto
    // every value it publishes — so drop glue asking the shape family again
    // was a second lookup for a value already in hand. The saved dependency
    // is still observed transitively through type-facts, so invalidation is
    // unchanged; only the direct edge is gone.
    let module = ModuleId::from_logical_path("main.rue").unwrap();
    let source = source_snapshot(
        &[(
            1,
            "/main.rue",
            "main.rue",
            "struct Child { value: i64 }\n\
                 drop fn Child(self) {}\n\
                 struct Outer { first: Child, spacer: i64, second: Child }",
        )],
        1,
    );
    let outer = named_type_instance(&module, "Outer", crate::StableDefinitionKind::Struct);
    let mut database = RevisionedQueryDatabase::default();
    let revision = revision_for(&mut database, &source);

    let attempt = request_drop_glue(&database, revision, outer.clone());
    assert_eq!(attempt.execution(), RequestExecution::Computed);
    let dependencies = attempt.terminal().unwrap().dependencies();

    let families: Vec<&str> = dependencies
        .iter()
        .map(|observation| observation.node.family())
        .collect();
    assert!(
        families.contains(&"compiler.type-facts"),
        "drop glue still depends on the facts it reads the shape from, got {families:?}"
    );
    assert!(
        !families.contains(&"compiler.type-shape"),
        "one drop-glue request must not perform a shape-family lookup of its \
             own; the shape travels with the facts. Observed families: {families:?}"
    );

    // The control for that negative: type-facts does observe the shape
    // family for this same key, so a shape edge is something these
    // dependency lists demonstrably show when one exists.
    let facts_attempt = database.runtime.request_registered(
        &database.type_facts,
        revision,
        crate::type_queries::TypeQueryKey {
            ty: outer,
            configuration: semantic_configuration(),
        },
        CancellationToken::new(),
    );
    let facts_families: Vec<&str> = facts_attempt
        .terminal()
        .unwrap()
        .dependencies()
        .iter()
        .map(|observation| observation.node.family())
        .collect();
    assert!(
        facts_families.contains(&"compiler.type-shape"),
        "type-facts is where the shape is queried and stamped onto the value, \
             got {facts_families:?}"
    );

    // The plan itself is derived from that shape, so a correct read shows up
    // as the same field-granular ownership decisions the shape describes.
    let rue_query::QueryOutcome::Success(crate::type_queries::DropGlueValue::Available(facts)) =
        attempt.terminal().unwrap().outcome()
    else {
        panic!("drop-glue plan did not publish");
    };
    let crate::type_queries::DropGluePlan::Struct { fields } = &facts.plan else {
        panic!("outer must have a struct plan");
    };
    assert_eq!(
        fields
            .iter()
            .map(|field| (field.name.as_ref(), field.drop))
            .collect::<Vec<_>>(),
        [("first", true), ("spacer", false), ("second", true)],
        "the plan must still name every field in shape order"
    );
}

#[test]
fn drop_glue_plan_is_cold_reusable_and_changes_with_order_not_only_nested_set() {
    let module = ModuleId::from_logical_path("main.rue").unwrap();
    let source = |text| source_snapshot(&[(1, "/main.rue", "main.rue", text)], 1);
    let first = source(
        "struct Child { value: i64 }\n\
             drop fn Child(self) {}\n\
             struct Outer { first: Child, spacer: i64, second: Child }",
    );
    let reordered = source(
        "struct Child { value: i64 }\n\
             drop fn Child(self) {}\n\
             struct Outer { spacer: i64, first: Child, second: Child }",
    );
    let outer = named_type_instance(&module, "Outer", crate::StableDefinitionKind::Struct);
    let mut database = RevisionedQueryDatabase::default();
    let first_revision = revision_for(&mut database, &first);
    let cold = request_drop_glue(&database, first_revision, outer.clone());
    assert_eq!(cold.execution(), RequestExecution::Computed);
    let cold_stamp = cold.terminal().unwrap().stamp();
    let cold_machine_symbol = match cold.terminal().unwrap().outcome() {
        rue_query::QueryOutcome::Success(crate::type_queries::DropGlueValue::Available(facts)) => {
            facts
                .machine_symbol
                .clone()
                .expect("drop glue owns its symbol")
        }
        _ => panic!("drop-glue plan did not publish"),
    };
    let reused = request_drop_glue(&database, first_revision, outer.clone());
    assert_eq!(reused.execution(), RequestExecution::Reused);
    assert_eq!(reused.terminal().unwrap().stamp(), cold_stamp);
    assert_eq!(
        match reused.terminal().unwrap().outcome() {
            rue_query::QueryOutcome::Success(crate::type_queries::DropGlueValue::Available(
                facts,
            )) => facts.machine_symbol.as_deref(),
            _ => None,
        },
        Some(cold_machine_symbol.as_ref())
    );

    let reordered_revision = revision_for(&mut database, &reordered);
    let changed = request_drop_glue(&database, reordered_revision, outer);
    assert_eq!(changed.execution(), RequestExecution::Computed);
    assert_ne!(changed.terminal().unwrap().stamp(), cold_stamp);
    let rue_query::QueryOutcome::Success(crate::type_queries::DropGlueValue::Available(facts)) =
        changed.terminal().unwrap().outcome()
    else {
        panic!("drop-glue plan did not publish");
    };
    let crate::type_queries::DropGluePlan::Struct { fields } = &facts.plan else {
        panic!("outer must have a struct plan");
    };
    assert_eq!(
        fields
            .iter()
            .map(|field| (field.name.as_ref(), field.drop))
            .collect::<Vec<_>>(),
        [("spacer", false), ("first", true), ("second", true)]
    );
    assert_eq!(
        facts.machine_symbol.as_deref(),
        Some(cold_machine_symbol.as_ref())
    );
}
