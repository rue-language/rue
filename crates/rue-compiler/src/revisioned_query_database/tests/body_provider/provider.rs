use super::super::*;

// ---- RUE-1091 r2: type-syntax/nominal ProviderFacts differentials -------
//
// These prove `ProviderTypeFacts` (BodyFactProvider + overlay) resolves every
// type-syntax shape in r2's scope to the same durable type the production
// binder assigned, and materializes each consulted nominal into the overlay
// with byte-identical durable metadata. The reference truth is the
// production durable declaration set (the semantic-nucleus batch projection
// behind `production_declarations`), never the same provider terminal, so
// agreement is a real cross-path proof.

/// Resolve `syntax` through `ProviderTypeFacts` inside one probe, returning
/// the resolved durable type (or `None` when resolution failed / deferred),
/// the overlay metadata materialized for `materialized_key`, and the exact
/// query edges the resolution recorded.
fn resolve_type_via_provider(
    database: &RevisionedQueryDatabase,
    revision: Revision,
    scope: &ModuleId,
    syntax: &str,
    materialized_key: Option<&StableDefinitionKey>,
) -> (
    Option<crate::DurableType>,
    Option<crate::DurableDeclarationPayload>,
    Vec<rue_query::NodeIdentity>,
) {
    let source = format!("fn probe(value: {syntax}) {{}}");
    let (tokens, interner) = rue_lexer::Lexer::new(&source).tokenize().unwrap();
    let (ast, interner) = rue_parser::Parser::new(tokens, interner).parse().unwrap();
    let rue_parser::ast::Item::Function(function) = &ast.items[0] else {
        panic!("type fixture parses as a function");
    };
    let mut builder = rue_rir::RirTypeSyntaxBuilder::default();
    let root = builder
        .push_parser_type(&function.params[0].ty, |symbol| {
            Arc::<str>::from(interner.resolve(&symbol))
        })
        .unwrap();
    let arena = builder.finish();
    let key = materialized_key.cloned();
    // The probe node is memoized by label, so each resolution needs a
    // distinct label or a repeat would reuse the first probe's terminal and
    // never run its closure.
    let label = format!("type-syntax:{syntax}");
    let outcome =
        database.probe_ready_body_facts(revision, semantic_configuration(), &label, |provider| {
            let mut overlay = crate::ProviderMaterialization::default();
            let mut facts = ProviderTypeFacts::new(provider, &mut overlay);
            let resolved =
                rue_air::resolve_structured_semantic_type_syntax(&mut facts, scope, &arena, root)
                    .ok();
            // Resolve a second time through the same overlay: resolution is
            // idempotent and a repeated consultation materializes no second
            // copy (the overlay's minted-once contract).
            let count_after_first = overlay.materialized_nominal_count();
            let mut facts = ProviderTypeFacts::new(provider, &mut overlay);
            let re_resolved =
                rue_air::resolve_structured_semantic_type_syntax(&mut facts, scope, &arena, root)
                    .ok();
            assert_eq!(
                resolved, re_resolved,
                "repeat resolution of {syntax} diverged"
            );
            assert_eq!(
                overlay.materialized_nominal_count(),
                count_after_first,
                "repeat resolution of {syntax} materialized a second overlay copy"
            );
            let materialized = key
                .as_ref()
                .and_then(|key| overlay.materialized_nominal(key).cloned());
            (resolved, materialized)
        });
    let (resolved, materialized) = outcome.result;
    (resolved, materialized, outcome.dependencies)
}

#[test]
fn provider_type_facts_resolve_nominals_and_alias_match_epoch() {
    use crate::DurableType as T;
    use crate::StableDefinitionKind as K;
    let source = "pub struct Point { x: i32, y: i32 }\n\
                      pub enum Shape { Circle, Square }\n\
                      pub const Alias: type = Point;\n\
                      fn main() -> i32 { 0 }\n";
    let snapshot = source_snapshot(&[(1, "/m.rue", "m.rue", source)], 1);
    let scope = ModuleId::from_logical_path("m.rue").unwrap();
    let decls = production_declarations(&snapshot);
    let point = durable_decl(&decls, K::Struct, "Point");
    let shape = durable_decl(&decls, K::Enum, "Shape");

    let mut database = RevisionedQueryDatabase::default();
    let revision = revision_for(&mut database, &snapshot);

    // Root struct: resolves to the exact stable identity the production binder
    // assigned to `Point`, via the module-index lookup — not the epoch table —
    // and materializes `Point`'s durable metadata into the overlay byte for
    // byte.
    let (resolved, materialized, deps) =
        resolve_type_via_provider(&database, revision, &scope, "Point", Some(&point.key));
    assert_eq!(resolved, Some(T::Nominal(point.key.clone())));
    assert_eq!(
        materialized.as_ref(),
        Some(&point.payload),
        "the overlay materialized Point's durable metadata identically to production"
    );
    // Edges land at materialization: the resolution recorded the `Point`
    // name-lookup terminal and its declaration (semantic-nucleus) terminal.
    assert!(
        deps.iter()
            .any(|node| node.family() == "compiler.lookup-name" && node.key().contains("Point")),
        "recorded the Point name-lookup edge: {deps:?}"
    );
    assert!(
        deps.iter()
            .any(|node| node.family() == "compiler.semantic-nucleus"),
        "recorded a declaration (signature/identity) edge at materialization: {deps:?}"
    );
    // The resolved fact's `is_public`/`defining_file` are provider-sourced
    // but not differentially checked here: they are consumed by
    // resolution/visibility, not by durable nominal identity, and their
    // byte-identity is covered by the render/visibility differential.

    // Root enum: same cross-path identity agreement + materialization.
    let (resolved, materialized, _deps) =
        resolve_type_via_provider(&database, revision, &scope, "Shape", Some(&shape.key));
    assert_eq!(resolved, Some(T::Nominal(shape.key.clone())));
    assert_eq!(materialized.as_ref(), Some(&shape.payload));

    // Root type alias: a `const` bound to a type resolves to that type — here
    // the nominal the alias points at, so `Alias` and `Point` collapse to the
    // same durable identity, exactly as the epoch resolves an alias head.
    let (resolved, _materialized, _deps) =
        resolve_type_via_provider(&database, revision, &scope, "Alias", None);
    assert_eq!(resolved, Some(T::Nominal(point.key.clone())));
}

#[test]
fn provider_type_facts_resolve_primitive_and_structural_shapes() {
    use crate::DurableType as T;
    let source = "fn main() -> i32 { 0 }\n";
    let snapshot = source_snapshot(&[(1, "/m.rue", "m.rue", source)], 1);
    let scope = ModuleId::from_logical_path("m.rue").unwrap();
    let mut database = RevisionedQueryDatabase::default();
    let revision = revision_for(&mut database, &snapshot);

    // Enumerate the `SemanticImportType` (durable) arms r2 covers structurally.
    // Each is resolved through the shared logic driven by ProviderTypeFacts;
    // primitives and structural wrappers consult no declaration fact, so they
    // record no dependency edge (proven below).
    let primitive_cases: &[(&str, T)] = &[
        ("i8", T::I8),
        ("i16", T::I16),
        ("i32", T::I32),
        ("i64", T::I64),
        ("u8", T::U8),
        ("u16", T::U16),
        ("u32", T::U32),
        ("u64", T::U64),
        ("usize", T::U64),
        ("isize", T::I64),
        ("bool", T::Bool),
        ("()", T::Unit),
        ("!", T::Never),
        ("type", T::ComptimeType),
    ];
    for (syntax, expected) in primitive_cases {
        let (resolved, _materialized, deps) =
            resolve_type_via_provider(&database, revision, &scope, syntax, None);
        assert_eq!(resolved.as_ref(), Some(expected), "primitive `{syntax}`");
        assert!(
            deps.is_empty(),
            "a primitive consults no declaration fact and records no edge: `{syntax}` -> {deps:?}"
        );
    }

    let structural_cases: &[(&str, T)] = &[
        (
            "[i32; 2]",
            T::Array {
                element: Arc::new(T::I32),
                len: 2,
            },
        ),
        (
            "[u8; 4]",
            T::Array {
                element: Arc::new(T::U8),
                len: 4,
            },
        ),
        ("ptr const i32", T::PtrConst(Arc::new(T::I32))),
        ("ptr mut u64", T::PtrMut(Arc::new(T::U64))),
        (
            "ptr const [i32; 2]",
            T::PtrConst(Arc::new(T::Array {
                element: Arc::new(T::I32),
                len: 2,
            })),
        ),
    ];
    for (syntax, expected) in structural_cases {
        let (resolved, _materialized, deps) =
            resolve_type_via_provider(&database, revision, &scope, syntax, None);
        assert_eq!(resolved.as_ref(), Some(expected), "structural `{syntax}`");
        assert!(
            deps.is_empty(),
            "structural `{syntax}` records no edge: {deps:?}"
        );
    }
}

#[test]
fn provider_type_facts_absent_and_kind_mismatch_do_not_resolve() {
    let source = "pub struct Point { x: i32 }\n\
                      pub enum Shape { A, B }\n\
                      fn main() -> i32 { 0 }\n";
    let snapshot = source_snapshot(&[(1, "/m.rue", "m.rue", source)], 1);
    let scope = ModuleId::from_logical_path("m.rue").unwrap();
    let mut database = RevisionedQueryDatabase::default();
    let revision = revision_for(&mut database, &snapshot);

    // An absent name has no candidate and does not resolve (UnknownType).
    let (resolved, _m, _d) =
        resolve_type_via_provider(&database, revision, &scope, "Missing", None);
    assert_eq!(resolved, None, "an absent type name does not resolve");

    // A name that exists but as the wrong kind (a function used as a type) is
    // kind-filtered out of the nominal candidate set and does not resolve — the
    // candidate-set-not-winner contract, applied in the shared logic.
    let (resolved, _m, _d) = resolve_type_via_provider(&database, revision, &scope, "main", None);
    assert_eq!(resolved, None, "a function name does not resolve as a type");
}

// The builtin `str` and slice `[T]` name facts — RUE-1091 r6a flips these two
// arms from documented gaps to positive differentials: their durable identity
// is a pure durable fact (a `BuiltinNominal` name+kind for `str`, a
// `Slice { element, name: syntax }` for a slice) needing no new boundary op,
// matching what `export_type_local` reproduces for the epoch's materialized
// `str`/slice struct.
#[test]
fn provider_type_facts_builtin_str_and_slice_names_match_epoch() {
    use crate::DurableType as T;
    let source = "pub struct Point { x: i32 }\n\
                      fn main() -> i32 { 0 }\n";
    let snapshot = source_snapshot(&[(1, "/m.rue", "m.rue", source)], 1);
    let scope = ModuleId::from_logical_path("m.rue").unwrap();
    let mut database = RevisionedQueryDatabase::default();
    let revision = revision_for(&mut database, &snapshot);

    // `str` resolves to the durable builtin-nominal identity — the exact form
    // `export_type_local` reproduces for the epoch's `str` struct.
    let (resolved, _m, deps) = resolve_type_via_provider(&database, revision, &scope, "str", None);
    assert_eq!(
        resolved,
        Some(T::BuiltinNominal {
            kind: rue_air::SemanticImportNominalKind::Struct,
            name: Arc::from("str"),
        }),
        "`str` resolves to the builtin-nominal durable identity"
    );
    // A pool/overlay-answered name fact records no provider query edge (edge
    // honesty — the builtin identity is not a boundary lookup).
    assert!(
        deps.is_empty(),
        "resolving `str` records no provider edge: {deps:?}"
    );

    // `[i32]` resolves to the durable slice identity whose name IS the slice
    // syntax and whose element is the resolved element type.
    let (resolved, _m, deps) =
        resolve_type_via_provider(&database, revision, &scope, "[i32]", None);
    assert_eq!(
        resolved,
        Some(T::Slice {
            element: Arc::new(T::I32),
            name: Arc::from("[i32]"),
        }),
        "`[i32]` resolves to the slice durable identity keyed by the slice syntax"
    );
    assert!(
        deps.is_empty(),
        "resolving `[i32]` records no provider edge: {deps:?}"
    );
}

// Explicit enumeration of the `SemanticImportType` arms this family does NOT
// yet cover, each with the boundary fact it waits on. Documented as
// not-yet-resolvable (never silently green): a deferred shape resolves to
// `None` through ProviderTypeFacts today, and the differential pins that so a
// later slice that adds the fact flips the arm deliberately.
//   - BuiltinNominal `Str(N)`: a generated fixed-capacity struct whose durable
//     identity is a generated-struct classification (`export_type_local`
//     rejects it as a `ForeignLocalType`). RUE-1091 r6b MINTS it in the pool
//     (`BodyIdentityPool::get_or_create_str_fixed`), but the TYPE-SYNTAX
//     resolution to that durable identity still needs the generated-struct
//     classification — deferred here (owner: Str(N) type-syntax classification).
//   - AnonymousNominal: produced by a body / a comptime call reducing to an
//     anonymous struct (`Pair()` below). RUE-1091 r6b MINTS it in the pool
//     (`find_or_create_anon`, proven cross-path in
//     `provider_endpoint_facts_anonymous_arm_mints_after_registration`), but the
//     anonymous reduction result is a body-level durable value the production
//     declaration binder rejects exporting (`AnonymousNominalType`), so the
//     type-syntax resolution stays deferred here (owner: body-level anonymous
//     type-syntax resolution).
//   - Module / GenericParameter: not reachable as a resolved type-syntax leaf.
// `str` and slice `[T]` are NO LONGER gaps — r6a flipped them (see
// `provider_type_facts_builtin_str_and_slice_names_match_epoch`).
// The comptime type-call arm itself is NO LONGER a gap — r5a flipped it (see
// `provider_type_facts_comptime_calls_match_epoch`); only the anonymous-nominal
// RESULT of such a call is still deferred at the type-syntax boundary (the pool
// mints the identity), and that deferral is pinned here.
#[test]
fn provider_type_facts_deferred_shapes_are_documented_gaps() {
    let source = "pub struct Point { x: i32 }\n\
                      fn Pair() -> type { struct { a: i32 } }\n\
                      fn main() -> i32 { 0 }\n";
    let snapshot = source_snapshot(&[(1, "/m.rue", "m.rue", source)], 1);
    let scope = ModuleId::from_logical_path("m.rue").unwrap();
    let mut database = RevisionedQueryDatabase::default();
    let revision = revision_for(&mut database, &snapshot);

    // Two type-syntax resolutions the POOL keystone mints but whose
    // declaration-level type-syntax resolution stays deferred in this slice:
    //  - `Str(8)`: a generated fixed-capacity string struct whose durable
    //    identity is a generated-struct classification (`export_type_local`
    //    rejects it as a `ForeignLocalType`). The pool mints it
    //    (`BodyIdentityPool::get_or_create_str_fixed`, r6b); the type-syntax
    //    resolution to that durable identity needs the generated-struct
    //    classification the r6a report deferred (owner: the Str(N) type-syntax
    //    classification follow-up).
    //  - `Pair()`: reduces to an ANONYMOUS nominal. The pool mints that
    //    identity (`find_or_create_anon`, proven cross-path in
    //    `provider_endpoint_facts_anonymous_arm_mints_after_registration`), but
    //    the anonymous reduction result is a BODY-level durable value the
    //    production declaration binder rejects exporting (`AnonymousNominalType`),
    //    so the type-syntax resolution has no declaration-level cross-path
    //    truth and stays deferred here (owner: body-level anonymous type-syntax
    //    resolution follow-up).
    for deferred in ["Pair()", "Str(8)"] {
        let (resolved, _m, _d) =
            resolve_type_via_provider(&database, revision, &scope, deferred, None);
        assert_eq!(
            resolved, None,
            "`{deferred}` type-syntax resolution is a documented deferral (the pool mints it; \
                 the type-syntax resolution stays deferred)"
        );
    }

    // A covered nominal in the same body still resolves — the deferrals do not
    // poison the family.
    let decls = production_declarations(&snapshot);
    let point = durable_decl(&decls, crate::StableDefinitionKind::Struct, "Point");
    let (resolved, _m, _d) = resolve_type_via_provider(&database, revision, &scope, "Point", None);
    assert_eq!(
        resolved,
        Some(crate::DurableType::Nominal(point.key.clone()))
    );
}

// ---- RUE-1091 r5a: SignatureFacts comptime-call differentials ------------
//
// These prove the flipped `ProviderTypeFacts` comptime-call arms (backed by
// `SignatureFacts` + the argument-parameterized comptime-call boundary op)
// reduce a comptime type/value call to the same durable type/value the
// production nucleus assigned. The reference truth is the production durable
// const declaration whose initializer IS the call, produced independently by
// the semantic-nucleus batch projection, never the same provider terminal.

/// The `Const { value }` durable value the production binder assigned to the
/// value-const named `name`.
fn production_const_value(
    decls: &[crate::durable_semantics::DurableDeclarationSemantic],
    name: &str,
) -> crate::DurableConstValue {
    let decl = durable_decl(decls, crate::StableDefinitionKind::ValueConst, name);
    match &decl.payload {
        crate::durable_semantics::DurableDeclarationPayload::Const { value, .. } => value.clone(),
        other => panic!("const `{name}` is not a value const: {other:?}"),
    }
}

/// The declared type of parameter `index` of the value-const-time signature
/// the production binder assigned to the callable named `name`.
fn production_signature_parameter(
    decls: &[crate::durable_semantics::DurableDeclarationSemantic],
    name: &str,
    index: usize,
) -> crate::DurableType {
    let decl = durable_decl(decls, crate::StableDefinitionKind::Function, name);
    match &decl.payload {
        crate::durable_semantics::DurableDeclarationPayload::Callable { parameters, .. } => {
            parameters[index].ty.clone()
        }
        other => panic!("`{name}` is not callable: {other:?}"),
    }
}

#[test]
fn provider_type_facts_comptime_calls_match_epoch() {
    use crate::DurableType as T;
    // `Id`/`Nth` reduce a comptime TYPE argument to a passthrough type (the
    // nucleus comptime-call terminal the boundary op drives can reduce these).
    // `Nth` additionally binds a comptime VALUE argument — a literal and a
    // scoped const — so `resolve_value_argument`/`const_value_fact` are on the
    // passing path. Each is declared as `const C: type = <call>`, so the
    // production const value is the independent cross-path truth. `Buffer`
    // additionally proves that the candidate-RIR evaluator preserves the
    // expression/type disambiguation for `[T; n]`: an array repeat whose
    // element is a comptime type constructs the array type.
    let source = "pub struct Point { x: i32 }\n\
                      fn Id(comptime T: type) -> type { T }\n\
                      pub fn Nth(comptime T: type, comptime k: i32) -> type { T }\n\
                      pub fn Buffer(comptime n: i32) -> type { [i32; n] }\n\
                      pub const N: i32 = 3;\n\
                      pub const IdPoint: type = Id(Point);\n\
                      pub const IdI32: type = Id(i32);\n\
                      pub const NthP2: type = Nth(Point, 2);\n\
                      pub const NthPN: type = Nth(Point, N);\n\
                      pub const Buffer2: type = Buffer(2);\n\
                      fn main() -> i32 { 0 }\n";
    let snapshot = source_snapshot(&[(1, "/m.rue", "m.rue", source)], 1);
    let scope = ModuleId::from_logical_path("m.rue").unwrap();
    let decls = production_declarations(&snapshot);
    let point = durable_decl(&decls, crate::StableDefinitionKind::Struct, "Point");
    let point_type = T::Nominal(point.key.clone());

    let mut database = RevisionedQueryDatabase::default();
    let revision = revision_for(&mut database, &snapshot);

    // `Id(Point)` — a comptime TYPE argument reduced to the exact nominal
    // identity the production const `IdPoint` holds. Records the comptime-call
    // reduction (semantic-nucleus) edge.
    let (resolved, _m, deps) =
        resolve_type_via_provider(&database, revision, &scope, "Id(Point)", None);
    assert_eq!(resolved, Some(point_type.clone()));
    assert_eq!(
        production_const_value(&decls, "IdPoint"),
        crate::DurableConstValue::Type(point_type.clone()),
        "cross-path: the production const holds the same reduced nominal",
    );
    assert!(
        deps.iter()
            .any(|node| node.family() == "compiler.semantic-nucleus"),
        "a comptime call records its reduction (semantic-nucleus) edge: {deps:?}"
    );

    // `Id(i32)` — the reduction collapses to a primitive; cross-checked
    // against the production const `IdI32`.
    let (resolved, _m, _d) =
        resolve_type_via_provider(&database, revision, &scope, "Id(i32)", None);
    assert_eq!(resolved, Some(T::I32));
    assert_eq!(
        production_const_value(&decls, "IdI32"),
        crate::DurableConstValue::Type(T::I32),
    );

    // `Nth(Point, 2)` — a comptime VALUE argument from a LITERAL flows through
    // `resolve_value_argument`; the reduction passes the type argument through.
    let (resolved, _m, _d) =
        resolve_type_via_provider(&database, revision, &scope, "Nth(Point, 2)", None);
    assert_eq!(resolved, Some(point_type.clone()));
    assert_eq!(
        production_const_value(&decls, "NthP2"),
        crate::DurableConstValue::Type(point_type.clone()),
    );

    // `Nth(Point, N)` — a comptime VALUE argument resolved through a SCOPED
    // CONST (`value_argument_fact` -> `const_value_fact`), cross-checked
    // against the production const `NthPN`.
    let (resolved, _m, _d) =
        resolve_type_via_provider(&database, revision, &scope, "Nth(Point, N)", None);
    assert_eq!(resolved, Some(point_type.clone()));
    assert_eq!(
        production_const_value(&decls, "NthPN"),
        crate::DurableConstValue::Type(point_type),
    );

    // `Buffer(2)` — an array-constructing type ctor reduces through the same
    // candidate artifact, including its comptime value parameter.
    let (resolved, _m, _d) =
        resolve_type_via_provider(&database, revision, &scope, "Buffer(2)", None);
    let buffer_type = T::Array {
        element: Arc::new(T::I32),
        len: 2,
    };
    assert_eq!(
        resolved,
        Some(buffer_type.clone()),
        "an array-constructing comptime type ctor reduces through the candidate artifact"
    );
    assert_eq!(
        production_const_value(&decls, "Buffer2"),
        crate::DurableConstValue::Type(buffer_type),
    );
}

#[test]
fn provider_type_facts_named_array_length_matches_epoch() {
    use crate::DurableType as T;
    // A named array length that is a scoped `const` now resolves through
    // `SignatureFacts::const_value_fact` (r5a flip of `resolve_array_length`).
    // The production binder's own resolution of the same `[i32; N]` in a
    // signature is the independent cross-path truth. A comptime CALL in length
    // position stays deferred (r6).
    let source = "const N: i32 = 3;\n\
                      fn use_len(a: [i32; N]) -> i32 { a[0] }\n\
                      fn main() -> i32 { 0 }\n";
    let snapshot = source_snapshot(&[(1, "/m.rue", "m.rue", source)], 1);
    let scope = ModuleId::from_logical_path("m.rue").unwrap();
    let decls = production_declarations(&snapshot);
    let mut database = RevisionedQueryDatabase::default();
    let revision = revision_for(&mut database, &snapshot);

    // `[i32; N]` — the named length `N` resolves to the scoped const's value,
    // matching the durable signature the binder assigned to `use_len`.
    let (resolved, _m, _d) =
        resolve_type_via_provider(&database, revision, &scope, "[i32; N]", None);
    let expected = T::Array {
        element: Arc::new(T::I32),
        len: 3,
    };
    assert_eq!(resolved, Some(expected.clone()));
    assert_eq!(
        production_signature_parameter(&decls, "use_len", 0),
        expected
    );

    // `[i32; missing]` — an unresolvable named length stays a deferred/None
    // resolution (no scoped const `missing` exists).
    let (resolved, _m, _d) =
        resolve_type_via_provider(&database, revision, &scope, "[i32; missing]", None);
    assert_eq!(resolved, None);
}

#[test]
fn signature_facts_constructor_head_carries_named_typed_parameters() {
    use rue_air::BodyFactProvider;
    // SignatureFacts reconstructs the constructor head from `signature()`
    // alone: the durable parameter names (part 1) become the head's parameter
    // names, and `is_type` is `is_comptime && ty == comptime type` — the same
    // predicate the epoch's `constructor_fact` applies from the shell.
    let source = "fn Wrap(comptime T: type, comptime n: i32) -> type { [T; n] }\n\
                      fn main() -> i32 { 0 }\n";
    let snapshot = source_snapshot(&[(1, "/m.rue", "m.rue", source)], 1);
    let scope = ModuleId::from_logical_path("m.rue").unwrap();
    let mut database = RevisionedQueryDatabase::default();
    let revision = revision_for(&mut database, &snapshot);

    let outcome = database.probe_ready_body_facts(
        revision,
        semantic_configuration(),
        "signature-facts:Wrap",
        |provider| {
            let facts = SignatureFacts::new(provider);
            let resolution =
                provider.lookup_unqualified(&scope, rue_air::ProviderNamespace::ModuleItem, "Wrap");
            let head = facts
                .constructor_head_fact(&scope, resolution, "Wrap")
                .expect("Wrap resolves to a constructor head");
            let names = head
                .parameters
                .iter()
                .map(|p| (p.name.to_string(), p.is_comptime, p.is_type))
                .collect::<Vec<_>>();
            (head.returns_type, names)
        },
    );
    let (returns_type, names) = outcome.result;
    assert!(returns_type, "`Wrap` returns a type");
    assert_eq!(
        names,
        vec![
            ("T".to_string(), true, true),  // comptime type parameter
            ("n".to_string(), true, false), // comptime value parameter
        ],
        "head carries durable parameter names and the type/value split"
    );
    // Absent / non-callable heads do not resolve.
    let outcome = database.probe_ready_body_facts(
        revision,
        semantic_configuration(),
        "signature-facts:absent",
        |provider| {
            let facts = SignatureFacts::new(provider);
            let resolution = provider.lookup_unqualified(
                &scope,
                rue_air::ProviderNamespace::ModuleItem,
                "Missing",
            );
            facts
                .constructor_head_fact(&scope, resolution, "Missing")
                .is_some()
        },
    );
    assert!(!outcome.result, "an absent name has no constructor head");
}

#[test]
fn provider_declaration_facts_match_production_epoch() {
    use crate::declaration_candidate::DeclarationCandidateCategory as Cat;
    use rue_air::BodyFactProvider;
    let snapshot = source_snapshot(
        &[(
            1,
            "/m.rue",
            "m.rue",
            "@copy struct Box { value: i32, fn get(borrow self) -> i32 { self.value } }\n\
                 struct Res { handle: i32 }\n\
                 drop fn Res(self) {}\n\
                 fn helper(x: i32) -> i32 { x }\n\
                 fn main() -> i32 { 0 }\n",
        )],
        1,
    );
    let m = ModuleId::from_logical_path("m.rue").unwrap();
    let mut database = RevisionedQueryDatabase::default();
    let revision = revision_for(&mut database, &snapshot);
    let config = semantic_configuration();

    let helper = declaration_candidate(&database, revision, &m, Cat::Function, "helper");
    let helper_instance = free_function_instance(&m, "helper");
    let box_struct = declaration_candidate(&database, revision, &m, Cat::Struct, "Box");
    let copy_receiver = ReceiverTypeIdentity::new(m.clone(), "Box", Cat::Struct);
    let res_receiver = ReceiverTypeIdentity::new(m.clone(), "Res", Cat::Struct);

    let helper_probe = helper.clone();
    let box_probe = box_struct.clone();
    let copy_probe = copy_receiver.clone();
    let res_probe = res_receiver.clone();
    let metrics_before = database.provider_observation_metrics();
    let outcome =
        database.probe_ready_body_facts(revision, config.clone(), "decl-facts", move |provider| {
            (
                provider.declaration_identity(&helper_probe),
                provider.signature(&helper_probe),
                provider.nominal_well_formedness(&box_probe),
                provider.signature(&box_probe),
                provider.anonymous_facts(&helper_probe),
                provider.language_item(&m, rue_air::ProviderNamespace::ModuleItem, "Box"),
                provider.drop_copy_metadata(&copy_probe),
                provider.drop_copy_metadata(&res_probe),
                provider.trusted_toolchain_facts(&helper_instance),
            )
        });
    let (
        identity,
        signature,
        well_formed,
        box_sig,
        anon,
        lang_item,
        copy_meta,
        res_meta,
        toolchain,
    ) = outcome.result;
    let metrics_after = database.provider_observation_metrics();
    let identity_facts = metrics_after.identity_facts - metrics_before.identity_facts;
    let signature_facts = metrics_after.signature_facts - metrics_before.signature_facts;
    let type_facts = metrics_after.type_facts - metrics_before.type_facts;
    let const_facts = metrics_after.const_facts - metrics_before.const_facts;
    let declaration_facts = metrics_after.declaration_facts - metrics_before.declaration_facts;
    assert_eq!(identity_facts, 1);
    assert_eq!(signature_facts, 4, "two direct and two drop/copy reads");
    assert_eq!(type_facts, 1);
    assert_eq!(const_facts, 0);
    assert_eq!(
        declaration_facts,
        identity_facts + signature_facts + type_facts + const_facts,
        "the declaration total is exactly partitioned by backing fact family"
    );

    // Identity / signature differential against the semantic-nucleus epoch.
    let epoch_identity = request_semantic_nucleus(
        &database,
        revision,
        crate::semantic_query_nucleus::SemanticNucleusKey::Identity(
            crate::semantic_query_nucleus::DeclarationSemanticQueryKey {
                declaration: helper.clone(),
                configuration: config.clone(),
            },
        ),
    );
    let crate::semantic_query_nucleus::SemanticNucleusValue::Identity(epoch_identity) =
        epoch_identity
    else {
        panic!("helper has an identity")
    };
    assert_eq!(identity, Some(epoch_identity));

    let epoch_signature = request_semantic_nucleus(
        &database,
        revision,
        crate::semantic_query_nucleus::SemanticNucleusKey::Signature(
            crate::semantic_query_nucleus::DeclarationSemanticQueryKey {
                declaration: helper.clone(),
                configuration: config.clone(),
            },
        ),
    );
    let crate::semantic_query_nucleus::SemanticNucleusValue::Signature(epoch_signature) =
        epoch_signature
    else {
        panic!("helper has a signature")
    };
    assert_eq!(signature.as_ref(), Some(&epoch_signature));
    // Anonymous facts are the signature's own anonymous nominals.
    assert_eq!(anon, Some(epoch_signature.anonymous_nominals.clone()));

    // A well-formed nominal; `@copy` and its destructor are exact facts.
    assert_eq!(
        well_formed,
        Some(rue_air::NominalWellFormedness::WellFormed)
    );
    assert!(matches!(
        box_sig.as_ref().map(|sig| &sig.signature),
        Some(
            crate::semantic_query_nucleus::DeclarationSignatureProjection::Struct {
                is_copy: true,
                ..
            }
        )
    ));
    // `@copy` Box has no destructor; Res has a destructor and is not copy.
    // Both facts are sourced from the destructor lookup + struct signature.
    assert_eq!(
        copy_meta,
        Some(rue_air::DropCopyMetadata {
            has_destructor: false,
            is_copy: true,
        })
    );
    assert_eq!(
        res_meta,
        Some(rue_air::DropCopyMetadata {
            has_destructor: true,
            is_copy: false,
        })
    );
    // A user nominal is not a language item.
    assert_eq!(lang_item, None);
    // A plain function demands no trusted-toolchain module.
    assert!(toolchain.modules().is_empty());

    // Edge-recording proof: declaration facts observe semantic-nucleus (and,
    // for drop metadata, a destructor lookup-name) terminals only.
    let families: std::collections::BTreeSet<&str> = outcome
        .dependencies
        .iter()
        .map(|node| node.family())
        .collect();
    assert!(
        families.contains("compiler.semantic-nucleus"),
        "{families:?}"
    );
    assert!(
        families
            .iter()
            .all(|family| *family == "compiler.semantic-nucleus"
                || *family == "compiler.lookup-name"
                || *family == "compiler.body-toolchain-demands"),
        "declaration facts observe only their exact backing terminals: {families:?}"
    );
}

#[test]
fn provider_repeated_nucleus_fact_reuses_the_request_local_terminal() {
    use crate::declaration_candidate::DeclarationCandidateCategory as Cat;
    use rue_air::BodyFactProvider;
    let snapshot = source_snapshot(
        &[(
            1,
            "/m.rue",
            "m.rue",
            "fn helper(x: i32) -> i32 { x }\nfn main() -> i32 { helper(0) }\n",
        )],
        1,
    );
    let m = ModuleId::from_logical_path("m.rue").unwrap();
    let mut database = RevisionedQueryDatabase::default();
    let revision = revision_for(&mut database, &snapshot);
    let helper = declaration_candidate(&database, revision, &m, Cat::Function, "helper");
    let outcome = database.probe_ready_body_facts(
        revision,
        semantic_configuration(),
        "repeated-nucleus-fact",
        move |provider| {
            let first = provider.signature(&helper);
            let second = provider.signature(&helper);
            (first, second, provider.nucleus_cache_hits.get())
        },
    );

    assert_eq!(outcome.result.0, outcome.result.1);
    assert_eq!(outcome.result.2, 1);
}

#[test]
fn provider_repeated_name_lookup_reuses_the_request_local_terminal() {
    use rue_air::BodyFactProvider;
    let snapshot = source_snapshot(
        &[(
            1,
            "/m.rue",
            "m.rue",
            "fn helper(x: i32) -> i32 { x }\nfn main() -> i32 { helper(0) }\n",
        )],
        1,
    );
    let m = ModuleId::from_logical_path("m.rue").unwrap();
    let mut database = RevisionedQueryDatabase::default();
    let revision = revision_for(&mut database, &snapshot);
    let outcome = database.probe_ready_body_facts(
        revision,
        semantic_configuration(),
        "repeated-name-lookup",
        move |provider| {
            let first =
                provider.lookup_unqualified(&m, rue_air::ProviderNamespace::ModuleItem, "helper");
            let second =
                provider.lookup_unqualified(&m, rue_air::ProviderNamespace::ModuleItem, "helper");
            (first, second, provider.lookup_name_cache_hits.get())
        },
    );

    assert_eq!(outcome.result.0, outcome.result.1);
    assert_eq!(outcome.result.2, 1);
    assert_eq!(
        outcome
            .dependencies
            .iter()
            .filter(|node| node.family() == "compiler.lookup-name")
            .count(),
        1,
        "the cache hit reuses the request's already-observed lookup edge"
    );
}

// ---- RUE-1091 r4b-1: call-resolution ProviderFacts differentials --------
//
// These prove `rue_air::ProviderCallFacts` (the provider-driven realization
// of the r1b `CallResolutionFacts` seam) assembles the family-1C identities
// from the exact body-fact provider (`CompilerBodyFactProvider`) + the body
// identity pool. The durable source the pool consults is built from the
// production durable declaration set (the semantic-nucleus batch
// projection, r2's stable-keyed metadata), so agreement is a real
// cross-path proof, not the same provider terminal.
//
// Scope landed here: free-function and nominal-member info composition
// (including associated functions), lookup selection, callable-symbol
// reversal, const overlays, and the body-local module registry. The
// production cutover owns assembling and registering those body-local facts.

/// Render a pool `Type` to a comparable display through the minted pool, the
/// index-independent parity the 2a/2b contract asserts (never a pool-relative
/// index).
fn render_pool_type(pool: &rue_air::TypeInternPool, ty: rue_air::Type) -> String {
    use rue_air::TypeKind;
    match ty.kind() {
        TypeKind::I8 => "i8".into(),
        TypeKind::I16 => "i16".into(),
        TypeKind::I32 => "i32".into(),
        TypeKind::I64 => "i64".into(),
        TypeKind::U8 => "u8".into(),
        TypeKind::U16 => "u16".into(),
        TypeKind::U32 => "u32".into(),
        TypeKind::U64 => "u64".into(),
        TypeKind::Bool => "bool".into(),
        TypeKind::Unit => "()".into(),
        TypeKind::Never => "!".into(),
        TypeKind::Struct(id) => pool.struct_def(id).name.to_string(),
        TypeKind::Enum(id) => pool.enum_def(id).name.to_string(),
        other => format!("{other:?}"),
    }
}

#[test]
fn provider_call_facts_function_info_is_assembled_from_durable_truth() {
    use crate::StableDefinitionKind as Kind;
    // A free function whose first parameter is a NOMINAL (`Point`): its type
    // resolves through the pool's 2a nominal machinery, its `n`/return through
    // the primitive arms — the full 2a+2b+2c compose behind the seam.
    let source = "pub struct Point { x: i64, y: i64 }\n\
                      @allow(unused_function)\n\
                      pub fn make(p: Point, n: i32) -> i64 { 0 }\n\
                      fn main() -> i32 { 0 }\n";
    let snapshot = source_snapshot(&[(1, "/m.rue", "m.rue", source)], 1);
    let file = FileId::new(1);
    let decls = production_declarations(&snapshot);
    let make = durable_decl(&decls, Kind::Function, "make");
    let make_key = make.key.clone();

    // The RIR + its interner are body-query inputs the driver fills the
    // request/RIR handle from.
    let stages = crate::test_support::test_frontend_stages(&snapshot).unwrap();
    let rir = &stages.rir;
    let interner = rir.semantic_symbols().interner();
    let rir_ref = rir.rir();
    let mut database = RevisionedQueryDatabase::default();
    let revision = revision_for(&mut database, &snapshot);
    let source_adapter = DurableDeclSource::from_declarations(&decls);

    let outcome = database.probe_ready_body_facts(
        revision,
        semantic_configuration(),
        "call-fn-info",
        move |provider| {
            let facts = rue_air::ProviderCallFacts::new(
                provider,
                source_adapter,
                rue_air::BodyRirView::from_parts(rir_ref, interner),
            );
            let info = facts
                .function_info(&make_key, "make", file)
                .expect("make resolves through the provider path");
            // Double consult: idempotent, the pool mints nothing new.
            let second = facts
                .function_info(&make_key, "make", file)
                .expect("repeat consult resolves");
            // `FunctionInfo` is not `PartialEq`; compare the load-bearing
            // identity fields to prove the repeat consult is stable.
            assert_eq!(second.declaration, info.declaration);
            assert_eq!(second.body, info.body);
            assert_eq!(
                second.params, info.params,
                "repeat consult re-minted params"
            );

            // Parameter vocabulary (2b), types resolved through 2a — asserted
            // through the index-independent render/name reads (the pool mints
            // its own ids; parity is a display property, not a raw index).
            let (names, types, modes) = facts.with_param_arena(|arena| {
                (
                    arena.names(info.params).to_vec(),
                    arena.types(info.params).to_vec(),
                    arena.modes(info.params).to_vec(),
                )
            });
            assert_eq!(info.params.len(), 2, "two explicit params");
            assert_eq!(facts.resolve_symbol(names[0]), "p");
            assert_eq!(facts.resolve_symbol(names[1]), "n");
            facts.with_type_pool(|pool| {
                assert_eq!(
                    render_pool_type(pool, types[0]),
                    "Point",
                    "the nominal param minted through 2a"
                );
                assert_eq!(render_pool_type(pool, types[1]), "i32");
                assert_eq!(render_pool_type(pool, info.return_type), "i64");
            });
            assert_eq!(modes[0], rue_rir::RirParamMode::Normal);
            info
        },
    );
    let info = outcome.result;

    // The assembled handles resolve back into the exact source declaration:
    // the r4a-2c span contract sources `FunctionInfo.span` from the shell's
    // declaration span, so it must slice to the declaration text, and the
    // declaration/body refs must name the RIR instructions at those spans.
    assert_eq!(
        &source[info.span.start as usize..info.span.end as usize],
        "@allow(unused_function)\npub fn make(p: Point, n: i32) -> i64 { 0 }",
        "assembled span slices to the attributed declaration text"
    );
    let declaration = rir_ref.get(info.declaration);
    assert!(
        matches!(declaration.data, rue_rir::InstData::FnDecl { .. }),
        "the declaration handle names the FnDecl instruction"
    );
    assert_eq!(
        info.span, declaration.span,
        "the assembled span is the declaration instruction's own"
    );
    let body_span = rir_ref.get(info.body).span;
    assert_eq!(
        &source[body_span.start as usize..body_span.end as usize],
        "0",
        "the body handle names the body expression"
    );
    assert_eq!(info.file_id, file);
    assert!(info.is_pub, "make is declared pub");
    assert!(!info.is_generic, "make has no comptime parameters");
    assert!(!info.is_unchecked);
    assert_eq!(
        rir_ref
            .type_syntax()
            .render_type_with(info.return_type_syntax, |symbol| interner.resolve(symbol))
            .as_deref(),
        Some("i64"),
        "the pre-resolution return syntax spells the annotated type"
    );

    // The P-op path consults the pool (durable source) + the RIR handle, not
    // the live provider terminals, so it records no provider query edge — the
    // pool is answered-by-metadata, and edge honesty is a C/B-op property
    // (pinned by the callable-symbol and name-lookup differentials).
    assert!(
        outcome.dependencies.is_empty(),
        "a pool-answered function_info records no provider edge: {:?}",
        outcome.dependencies
    );
}

#[test]
fn provider_call_facts_function_contains_selects_from_the_candidate_set() {
    let source = "pub struct Point { x: i32 }\n\
                      pub fn helper() -> i32 { 0 }\n\
                      fn main() -> i32 { 0 }\n";
    let snapshot = source_snapshot(&[(1, "/m.rue", "m.rue", source)], 1);
    let m = ModuleId::from_logical_path("m.rue").unwrap();
    let decls = production_declarations(&snapshot);
    let (rir_out, _semantic, _) =
        crate::test_support::test_frontend_snapshot(&snapshot, &crate::CompileOptions::default())
            .expect("frontend compiles");
    let rir = rir_out.rir();
    let interner = rir_out.semantic_symbols().interner();

    let mut database = RevisionedQueryDatabase::default();
    let revision = revision_for(&mut database, &snapshot);
    let source_adapter = DurableDeclSource::from_declarations(&decls);

    let outcome = database.probe_ready_body_facts(
        revision,
        semantic_configuration(),
        "call-fn-contains",
        move |provider| {
            let facts = rue_air::ProviderCallFacts::new(
                provider,
                source_adapter,
                rue_air::BodyRirView::from_parts(rir, interner),
            );
            (
                // A declared free function is present.
                facts.function_contains_in_module(&m, "helper"),
                // A struct name is NOT a free function (kind-filtered out of
                // the candidate set — the candidate-sets-not-winners contract).
                facts.function_contains_in_module(&m, "Point"),
                // An absent name.
                facts.function_contains_in_module(&m, "missing"),
            )
        },
    );
    let (helper, point, missing) = outcome.result;
    assert!(helper, "helper is a declared free function");
    assert!(!point, "a struct is not a free function");
    assert!(!missing, "an absent name is not a free function");
    // The lookups are observed through the provider's exact name terminal.
    assert!(
        outcome
            .dependencies
            .iter()
            .any(|node| node.family() == "compiler.lookup-name"),
        "function_contains observes the name-lookup terminal: {:?}",
        outcome.dependencies
    );
}

#[test]
fn provider_call_facts_method_info_is_assembled_from_durable_truth() {
    use crate::StableDefinitionKind as Kind;
    // A named method whose receiver (`Widget`), one explicit param (`Point`),
    // and return (`i64`) all resolve through the pool's 2a nominal machinery
    // — the r4b-3 backlog item: the receiver preimage `(owner_file,
    // owner_type_name)` threads through the durable method key, recovered by
    // joining the method key's `owner()` back to the owner nominal's durable
    // key (the `DurableDeclSource::method` receiver join).
    let source = "pub struct Point { x: i64, y: i64 }\n\
                      pub struct Widget { id: i64, \
                        fn shift(borrow self, p: Point, n: i32) -> i64 { self.id } }\n\
                      fn main() -> i32 { 0 }\n";
    let snapshot = source_snapshot(&[(1, "/m.rue", "m.rue", source)], 1);
    let file = FileId::new(1);
    let decls = production_declarations(&snapshot);
    let shift = durable_decl(&decls, Kind::Method, "shift");
    let shift_key = shift.key.clone();

    let stages = crate::test_support::test_frontend_stages(&snapshot).unwrap();
    let rir = &stages.rir;
    let interner = rir.semantic_symbols().interner();
    let rir_ref = rir.rir();
    let mut database = RevisionedQueryDatabase::default();
    let revision = revision_for(&mut database, &snapshot);
    let source_adapter = DurableDeclSource::from_declarations(&decls);

    let outcome = database.probe_ready_body_facts(
        revision,
        semantic_configuration(),
        "call-method-info",
        move |provider| {
            let identity = rue_air::ProviderIdentityContext::new(source_adapter);
            let view = rue_air::BodyRirView::from_parts(rir_ref, interner);
            let facts =
                rue_air::ProviderCallFacts::with_identity(provider, identity.clone(), view.clone());
            let endpoints = rue_air::ProviderEndpointFacts::with_identity(provider, identity, view);
            // `named_method_info` coincides over the named differential scope
            // (no anonymous fallback), mirroring the epoch's `methods.get`.
            let named = facts
                .named_method_info(&shift_key, file, "Widget", "shift")
                .expect("named_method_info resolves");
            let compact_owner = named.struct_type.as_struct().expect("Widget is a struct");
            let compact_name = facts.name_symbol("shift").expect("shift is interned");
            assert_eq!(
                endpoints
                    .method_info(compact_owner, compact_name)
                    .expect("the endpoint facts observe the named registration")
                    .body,
                named.body,
                "endpoint method lookup falls back to the shared named entry"
            );
            let anonymous = rue_air::MethodInfo {
                body: rue_rir::InstRef::from_raw(named.body.as_u32() + 1),
                ..named
            };
            assert!(
                facts
                    .register_anonymous_method(file, "Widget", "shift", anonymous)
                    .expect("shift name is admitted"),
                "the anonymous method registers atomically under both lookup keys"
            );
            let info = facts
                .method_info(&shift_key, file, "Widget", "shift")
                .expect("anonymous method wins over the named collision");
            assert_eq!(info.body, anonymous.body, "anonymous method has precedence");
            assert_ne!(named.body, info.body, "the collision is observable");
            assert_eq!(
                endpoints
                    .method_info(compact_owner, compact_name)
                    .expect("the endpoint facts observe the anonymous registration")
                    .body,
                anonymous.body,
                "endpoint and call facts agree on anonymous-first precedence"
            );
            // Double consult: idempotent, the pool re-mints nothing.
            let second = facts
                .method_info(&shift_key, file, "Widget", "shift")
                .expect("repeat consult resolves");
            assert_eq!(second.body, info.body);
            assert_eq!(
                second.params, info.params,
                "repeat consult re-minted params"
            );

            // Explicit params (self excluded): one nominal (`Point` through
            // 2a) and one primitive, asserted through the index-independent
            // render / resolved-name reads.
            let (names, types, modes) = facts.with_param_arena(|arena| {
                (
                    arena.names(info.params).to_vec(),
                    arena.types(info.params).to_vec(),
                    arena.modes(info.params).to_vec(),
                )
            });
            assert_eq!(info.params.len(), 2, "self is excluded from params");
            assert_eq!(facts.resolve_symbol(names[0]), "p");
            assert_eq!(facts.resolve_symbol(names[1]), "n");
            let (receiver, ret) = facts.with_type_pool(|pool| {
                assert_eq!(render_pool_type(pool, types[0]), "Point");
                assert_eq!(render_pool_type(pool, types[1]), "i32");
                (
                    render_pool_type(pool, info.struct_type),
                    render_pool_type(pool, info.return_type),
                )
            });
            assert_eq!(modes[0], rue_rir::RirParamMode::Normal);

            (info, named, receiver, ret)
        },
    );
    let (info, named, receiver, ret) = outcome.result;

    // The assembled metadata resolves back into the exact source
    // declaration: pool-relative types by index-independent render, RIR
    // handles by the source text at their spans.
    assert_eq!(receiver, "Widget", "receiver is the owning nominal");
    assert_eq!(ret, "i64", "return renders as the annotated type");
    assert!(info.has_self, "shift takes self");
    assert_eq!(info.self_mode, rue_rir::RirParamMode::Borrow);
    assert!(!info.self_is_mut, "shift's receiver is not `mut self`");
    let named_body_span = rir_ref.get(named.body).span;
    assert_eq!(
        &source[named_body_span.start as usize..named_body_span.end as usize],
        "self.id",
        "the named method body handle names shift's body expression"
    );
    assert_ne!(
        info.body, named.body,
        "anonymous collision remains selected"
    );
    assert_eq!(
        &source[info.span.start as usize..info.span.end as usize],
        "fn shift(borrow self, p: Point, n: i32) -> i64 { self.id }",
        "the method span slices to the declaration text"
    );

    // The P-op path consults the pool + the RIR handle, not the live provider
    // terminals, so it records no provider edge (pool answered-by-metadata).
    assert!(
        outcome.dependencies.is_empty(),
        "a pool-answered method_info records no provider edge: {:?}",
        outcome.dependencies
    );
}

#[test]
fn provider_call_facts_associated_function_is_assembled_from_durable_truth() {
    use crate::StableDefinitionKind as Kind;

    let source = "pub struct Counter { value: i32, \
                        fn make(value: i32) -> Counter { Counter { value: value } } }\n\
                      fn main() -> i32 { 0 }\n";
    let snapshot = source_snapshot(&[(1, "/m.rue", "m.rue", source)], 1);
    let file = FileId::new(1);
    let decls = production_declarations(&snapshot);
    let make_key = durable_decl(&decls, Kind::AssociatedFunction, "make")
        .key
        .clone();

    let stages = crate::test_support::test_frontend_stages(&snapshot).unwrap();
    let rir = &stages.rir;
    let interner = rir.semantic_symbols().interner();
    let rir_ref = rir.rir();
    let mut database = RevisionedQueryDatabase::default();
    let revision = revision_for(&mut database, &snapshot);
    let outcome = database.probe_ready_body_facts(
        revision,
        semantic_configuration(),
        "call-associated-info",
        move |provider| {
            let facts = rue_air::ProviderCallFacts::new(
                provider,
                DurableDeclSource::from_declarations(&decls),
                rue_air::BodyRirView::from_parts(rir_ref, interner),
            );
            let info = facts
                .method_info(&make_key, file, "Counter", "make")
                .expect("associated function resolves through the method namespace");
            let types = facts.with_type_pool(|pool| {
                (
                    render_pool_type(pool, info.struct_type),
                    render_pool_type(pool, info.return_type),
                )
            });
            (info, types)
        },
    );
    let (provider, provider_types) = outcome.result;

    assert_eq!(
        provider_types,
        ("Counter".to_owned(), "Counter".to_owned()),
        "owner and return both render as the owning nominal"
    );
    assert!(!provider.has_self, "Counter.make is an associated function");
    assert_eq!(
        provider.params.len(),
        1,
        "make takes one explicit parameter"
    );
    let body_span = rir_ref.get(provider.body).span;
    assert_eq!(
        &source[body_span.start as usize..body_span.end as usize],
        "Counter { value: value }",
        "the body handle names make's body expression"
    );
    assert_eq!(
        &source[provider.span.start as usize..provider.span.end as usize],
        "fn make(value: i32) -> Counter { Counter { value: value } }",
        "the assembled span slices to the declaration text"
    );
    assert!(
        outcome.dependencies.is_empty(),
        "associated assembly uses durable metadata + RIR only: {:?}",
        outcome.dependencies
    );
}

#[test]
fn provider_named_destructor_metadata_is_retained_on_the_minted_nominal() {
    use crate::StableDefinitionKind as Kind;
    use rue_air::{
        NominalInstanceKey, SemanticDefinitionToken, SemanticModuleToken, TypeInstanceKey,
    };

    let source = "pub struct Box { value: i32 }\n\
                      drop fn Box(self) {}\n\
                      fn main() -> i32 { 0 }\n";
    let snapshot = source_snapshot(&[(1, "/m.rue", "m.rue", source)], 1);
    let file = FileId::new(1);
    let decls = production_declarations(&snapshot);
    let box_key = durable_decl(&decls, Kind::Struct, "Box").key.clone();

    let stages = crate::test_support::test_frontend_stages(&snapshot).unwrap();
    let rir = &stages.rir;
    let interner = rir.semantic_symbols().interner();
    let rir_ref = rir.rir();
    let mut database = RevisionedQueryDatabase::default();
    let revision = revision_for(&mut database, &snapshot);
    let outcome = database.probe_ready_body_facts(
        revision,
        semantic_configuration(),
        "endpoint-destructor-metadata",
        move |provider| {
            let facts = rue_air::ProviderEndpointFacts::new(
                provider,
                DurableDeclSource::from_declarations(&decls),
                rue_air::BodyRirView::from_parts(rir_ref, interner),
            );
            let token = facts
                .register_named_nominal(box_key, file.index(), "Box", Kind::Struct)
                .expect("Box name is admitted");
            let ty = facts
                .resolve_instance_type(&TypeInstanceKey::<
                    SemanticDefinitionToken,
                    SemanticModuleToken,
                >::Nominal(NominalInstanceKey::Named(
                    token,
                )))
                .expect("provider pool mints Box");
            facts.with_type_pool(|pool| {
                pool.struct_def(ty.as_struct().expect("Box is a struct"))
                    .destructor
                    .clone()
            })
        },
    );

    assert_eq!(
        outcome.result.as_deref(),
        Some("Box$m_2erue.__drop"),
        "the destructor-bearing nominal must retain its stable destructor symbol"
    );
}

// ---- RUE-1091 r4b-2: endpoint ProviderFacts coverage ---------------------
//
// These prove `rue_air::ProviderEndpointFacts` (the provider-driven
// realization of the family-1A `BodyEndpointProvider` seam) resolves every
// `TypeInstanceKey` arm the body identity pool supports. The driver REUSES
// the same provider-generic `resolve_instance_type` logic production runs
// (`body_endpoint.rs`), driven over the pool + an overlay token space. The
// durable source (`DurableDeclSource`, shared with the r4b-1 block) is
// built from the production durable declaration set, and each resolution is
// pinned through its index-independent render — never a pool-relative
// index.
//
// Scope landed here: `resolve_instance_type` over primitives, named
// struct/enum (the by-file-name lookup + endpoint token space), builtin `str`
// (builtin classification), and the structural array / `ptr const` / `ptr
// mut` wrappers (P); the three RIR ops (R, thin `BodyRirIndex` delegation);
// the provider-boundary nominal-presence check (C). Deferred with cause
// (pinned, never silently answered wrong): module identity → r4b-3 / the
// flip; generic parameter → r5/r6 substitution; anonymous mint-from-digest
// and well-known `Option` → r6; builtin / slice names beyond the pool's
// pre-registered `BUILTIN_ENUMS` + `str` set → r6; the `(StructId, name)`
// endpoint-trait seam → r4b-3.

#[test]
fn provider_endpoint_facts_resolve_instance_type_mints_the_declared_surface() {
    use crate::StableDefinitionKind as Kind;
    use rue_air::{
        NominalInstanceKey as N, SemanticDefinitionToken as DTok, SemanticModuleToken as MTok,
        TypeInstanceKey as T,
    };
    // The full nominal / structural surface: `Point` is a non-copy nominal
    // (its fields resolve through the pool's 2a machinery); `Holder` embeds a
    // nominal field plus the array / `ptr const` / `ptr mut` structural arms;
    // `Color` is a named enum. Each is minted by the provider path and its
    // index-independent render is pinned against the declared source shape.
    let source = "pub struct Point { x: i64, y: i64 }\n\
                      pub enum Color { Red, Green }\n\
                      pub struct Holder { p: Point, arr: [i64; 3], pc: ptr const Point, pm: ptr mut i64 }\n\
                      fn main() -> i32 { 0 }\n";
    let snapshot = source_snapshot(&[(1, "/m.rue", "m.rue", source)], 1);
    let file = FileId::new(1);
    let decls = production_declarations(&snapshot);
    let point_key = durable_decl(&decls, Kind::Struct, "Point").key.clone();
    let holder_key = durable_decl(&decls, Kind::Struct, "Holder").key.clone();
    let color_key = durable_decl(&decls, Kind::Enum, "Color").key.clone();

    let stages = crate::test_support::test_frontend_stages(&snapshot).unwrap();
    let merged = &stages.merged;
    let rir = &stages.rir;
    let interner = rir.semantic_symbols().interner();
    let durable_module = merged.ast().modules()[0].module_id().clone();
    let conflicting_durable_module =
        crate::ModuleId::from_logical_path("other.rue").expect("second durable module id");

    let rir_ref = rir.rir();
    let mut database = RevisionedQueryDatabase::default();
    let revision = revision_for(&mut database, &snapshot);
    let adapter = DurableDeclSource::from_declarations(&decls);
    let call_adapter = DurableDeclSource::from_declarations(&decls);
    let aggregate_adapter = DurableDeclSource::from_declarations(&decls);
    let durable_module_for_fact = durable_module.clone();

    let outcome = database.probe_ready_body_facts(
        revision,
        semantic_configuration(),
        "endpoint-resolve",
        move |provider| {
            let facts = rue_air::ProviderEndpointFacts::new(
                provider,
                adapter,
                rue_air::BodyRirView::from_parts(rir_ref, interner),
            );
            let named = |token: DTok| -> T<DTok, MTok> { T::Nominal(N::Named(token)) };
            let point_token = facts
                .register_named_nominal(point_key.clone(), 1, "Point", Kind::Struct)
                .expect("Point name is admitted");
            let holder_token = facts
                .register_named_nominal(holder_key.clone(), 1, "Holder", Kind::Struct)
                .expect("Holder name is admitted");
            let color_token = facts
                .register_named_nominal(color_key.clone(), 1, "Color", Kind::Enum)
                .expect("Color name is admitted");

            let point_ty = facts
                .resolve_instance_type(&named(point_token))
                .expect("Point resolves through the provider path");
            // Double consult: the pool dedups, resolution is stable.
            let point_again = facts
                .resolve_instance_type(&named(point_token))
                .expect("repeat consult resolves");
            assert_eq!(
                point_ty, point_again,
                "repeat consult re-minted the nominal"
            );
            let holder_ty = facts
                .resolve_instance_type(&named(holder_token))
                .expect("Holder resolves (nominal field + structural fields)");
            let color_ty = facts
                .resolve_instance_type(&named(color_token))
                .expect("Color resolves through the provider path");

            // Top-level structural / primitive arms of the SHARED
            // `resolve_instance_type` walk.
            let array_ty = facts
                .resolve_instance_type(&T::Array {
                    element: Node::new(T::I64),
                    len: 3,
                })
                .expect("array arm resolves");
            let ptr_const_ty = facts
                .resolve_instance_type(&T::PtrConst(Node::new(named(point_token))))
                .expect("ptr const arm resolves over a nominal");
            let ptr_mut_ty = facts
                .resolve_instance_type(&T::PtrMut(Node::new(T::I64)))
                .expect("ptr mut arm resolves");
            let i64_ty = facts
                .resolve_instance_type(&T::I64)
                .expect("primitive arm resolves");
            let str_ty = facts
                .resolve_instance_type(&T::BuiltinNominal {
                    kind: rue_air::AnonymousNominalKind::Struct,
                    name: std::sync::Arc::from("str"),
                })
                .expect("builtin str resolves through the pool's pre-registered set");

            let module_import_path = durable_module.logical_path().to_owned();
            let module_token = facts
                .register_module(
                    durable_module.clone(),
                    file,
                    "/m.rue",
                    &module_import_path,
                    &module_import_path,
                )
                .expect("durable module registration is consistent");
            assert_eq!(
                facts.register_module(
                    durable_module.clone(),
                    file,
                    "/m.rue",
                    &module_import_path,
                    &module_import_path,
                ),
                Some(module_token),
                "repeat durable module registration dedups"
            );
            assert!(
                facts
                    .register_module(
                        durable_module.clone(),
                        FileId::new(2),
                        "/other.rue",
                        &module_import_path,
                        &module_import_path,
                    )
                    .is_none(),
                "one durable module cannot acquire a conflicting file"
            );
            assert!(
                facts
                    .register_module(
                        conflicting_durable_module,
                        file,
                        "/other.rue",
                        "other.rue",
                        "other.rue",
                    )
                    .is_none(),
                "a second durable module cannot claim an already-registered file"
            );
            let module_ty = facts
                .resolve_instance_type(&T::Module(module_token))
                .expect("module arm resolves through provider module facts");
            let module_file = facts
                .module_file(module_ty)
                .expect("provider module type reverses to its current file");

            let endpoint_render = facts.with_type_pool(|pool| {
                (
                    endpoint_nominal_render(pool, point_ty),
                    endpoint_nominal_render(pool, holder_ty),
                    endpoint_nominal_render(pool, color_ty),
                    endpoint_display(pool, array_ty),
                    endpoint_display(pool, ptr_const_ty),
                    endpoint_display(pool, ptr_mut_ty),
                    endpoint_display(pool, i64_ty),
                    endpoint_display(pool, str_ty),
                    module_file,
                )
            });

            let call_facts = rue_air::ProviderCallFacts::new(
                provider,
                call_adapter,
                rue_air::BodyRirView::from_parts(rir_ref, interner),
            );
            let call_module = call_facts
                .register_module(
                    durable_module.clone(),
                    file,
                    "/m.rue",
                    &module_import_path,
                    &module_import_path,
                )
                .expect("call driver registers durable module facts");
            let call_module_fact = call_facts
                .module_def(call_module)
                .map(|definition| {
                    (
                        definition.file_id,
                        definition.file_path,
                        definition.import_path,
                        definition.durable_id,
                    )
                })
                .expect("call driver answers module_def");

            let aggregate_facts = rue_air::ProviderAggregateFacts::new(aggregate_adapter);
            let aggregate_module = aggregate_facts
                .register_module(
                    durable_module,
                    file,
                    "/m.rue",
                    &module_import_path,
                    &module_import_path,
                )
                .expect("aggregate driver registers durable module facts");
            let aggregate_module_fact = aggregate_facts
                .module_fact(aggregate_module)
                .expect("aggregate driver answers module facts");

            (endpoint_render, call_module_fact, aggregate_module_fact)
        },
    );
    let (endpoint_render, call_module_fact, aggregate_module_fact) = outcome.result;
    let (
        point_r,
        holder_r,
        color_r,
        array_d,
        ptr_const_d,
        ptr_mut_d,
        i64_d,
        str_d,
        provider_module_file,
    ) = endpoint_render;

    // Named-nominal arms: the full index-independent render is pinned to the
    // declared source shape.
    assert_eq!(
        point_r,
        EndpointNominalRender {
            display: "Point".to_owned(),
            is_copy: false,
            is_pub: true,
            symbol: "Point$m_2erue".to_owned(),
            members: vec![
                ("x".to_owned(), "i64".to_owned()),
                ("y".to_owned(), "i64".to_owned()),
            ],
        },
        "Point resolution renders the declared struct"
    );
    assert_eq!(
        holder_r,
        EndpointNominalRender {
            display: "Holder".to_owned(),
            is_copy: false,
            is_pub: true,
            symbol: "Holder$m_2erue".to_owned(),
            members: vec![
                ("p".to_owned(), "Point".to_owned()),
                ("arr".to_owned(), "[i64; 3]".to_owned()),
                ("pc".to_owned(), "ptr const Point".to_owned()),
                ("pm".to_owned(), "ptr mut i64".to_owned()),
            ],
        },
        "Holder (nominal + structural fields) renders the declared struct"
    );
    assert_eq!(
        color_r,
        EndpointNominalRender {
            display: "Color".to_owned(),
            is_copy: true,
            is_pub: true,
            symbol: "Color$m_2erue".to_owned(),
            members: vec![
                ("Red".to_owned(), String::new()),
                ("Green".to_owned(), String::new()),
            ],
        },
        "Color enum renders the declared variants"
    );

    // Structural / primitive arms of the SHARED `resolve_instance_type`
    // walk render their canonical spellings.
    assert_eq!(array_d, "[i64; 3]", "array arm renders the declared array");
    assert_eq!(ptr_const_d, "ptr const Point");
    assert_eq!(ptr_mut_d, "ptr mut i64");
    assert_eq!(i64_d, "i64", "primitive arm renders directly");
    // Builtin arm: `str` is pre-registered in the pool.
    assert_eq!(
        str_d, "str",
        "builtin str renders as the pre-registered nominal"
    );
    assert_eq!(
        provider_module_file, file,
        "module endpoint + registry resolution reverses to the registered file"
    );
    assert_eq!(
        call_module_fact,
        (
            file,
            "/m.rue".to_owned(),
            "m.rue".to_owned(),
            durable_module_for_fact.as_str().to_owned(),
        ),
        "call module_def carries the registered module facts"
    );
    assert_eq!(
        aggregate_module_fact,
        (file, "/m.rue".to_owned(), "m.rue".to_owned()),
        "aggregate module facts carry the registered paths"
    );

    // The P-op path consults the pool (durable source) + overlay, not the
    // live provider terminals, so it records no provider query edge — edge
    // honesty is a C-op property (pinned by the presence differential below).
    assert!(
        outcome.dependencies.is_empty(),
        "a pool-answered resolution records no provider edge: {:?}",
        outcome.dependencies
    );
}

#[test]
fn provider_endpoint_facts_deferred_arms_are_pinned_gaps() {
    use crate::StableDefinitionKind as Kind;
    use rue_air::{
        AnonymousNominalKey, AnonymousNominalKind, NominalInstanceKey as N,
        SemanticModuleToken as MTok, StableProducerId, TypeInstanceKey as T,
    };
    // A one-nominal program so the anonymous producer has a real definition
    // token to name; every arm below is a documented pool deferral that must
    // fail closed (never resolve wrong).
    let source = "pub struct Point { x: i64 }\n\
                      fn main() -> i32 { 0 }\n";
    let snapshot = source_snapshot(&[(1, "/m.rue", "m.rue", source)], 1);
    let file = FileId::new(1);
    let decls = production_declarations(&snapshot);
    let point_key = durable_decl(&decls, Kind::Struct, "Point").key.clone();
    let (rir_out, _semantic, _) =
        crate::test_support::test_frontend_snapshot(&snapshot, &crate::CompileOptions::default())
            .expect("frontend compiles");
    let rir = rir_out.rir();
    let interner = rir_out.semantic_symbols().interner();

    let mut database = RevisionedQueryDatabase::default();
    let revision = revision_for(&mut database, &snapshot);
    let adapter = DurableDeclSource::from_declarations(&decls);

    let outcome = database.probe_ready_body_facts(
        revision,
        semantic_configuration(),
        "endpoint-deferred",
        move |provider| {
            let facts = rue_air::ProviderEndpointFacts::new(
                provider,
                adapter,
                rue_air::BodyRirView::from_parts(rir, interner),
            );
            let point_token = facts
                .register_named_nominal(point_key.clone(), file.index(), "Point", Kind::Struct)
                .expect("Point name is admitted");

            // Module identity — r4b-3 / the flip (pool-refused arm).
            let module = facts.resolve_instance_type(&T::Module(MTok::new(0, 0)));
            // Generic parameter — r5 substitution.
            let generic = facts.resolve_instance_type(&T::GenericParameter(0));
            // A slice whose generated struct was NOT seeded still fails closed:
            // the r6a `Slice` arm resolves only AFTER `register_generated_slice`
            // (positive differential in
            // `provider_endpoint_facts_slice_arm_resolves_after_registration`).
            let slice = facts.resolve_instance_type(&T::Slice {
                element: Node::new(T::I64),
                name: std::sync::Arc::from("[]i64"),
            });
            // A genuine non-builtin name (not any builtin under any regime)
            // fails closed — a permanent gap, not an r6 deferral.
            let unknown_builtin = facts.resolve_instance_type(&T::BuiltinNominal {
                kind: AnonymousNominalKind::Struct,
                name: std::sync::Arc::from("NotABuiltin"),
            });
            // An UNSEEDED anonymous key fails closed: the r6b arm mints only
            // for a durable identity seeded by `register_anonymous_nominal`
            // (the positive differential is
            // `provider_endpoint_facts_anonymous_arm_mints_after_registration`),
            // exactly as the unseeded `Slice` arm above fails closed. The pool
            // never invents an anonymous identity.
            let anonymous = facts.resolve_instance_type(&T::Nominal(N::Anonymous(Node::new(
                AnonymousNominalKey {
                    kind: AnonymousNominalKind::Struct,
                    producer: StableProducerId::Definition(point_token),
                    anchor: rue_rir::RirStructuralAnchor::new(Vec::new()),
                },
            ))));
            (
                module.is_err(),
                generic.is_err(),
                slice.is_err(),
                unknown_builtin.is_err(),
                anonymous.is_err(),
            )
        },
    );
    let (module, generic, slice, unknown_builtin, anonymous) = outcome.result;
    assert!(module, "module identity fails closed (r4b-3 / flip)");
    assert!(generic, "generic parameter fails closed (r5)");
    assert!(
        slice,
        "an unseeded slice generated-struct name fails closed"
    );
    assert!(
        unknown_builtin,
        "a non-builtin name fails closed (permanent)"
    );
    assert!(
        anonymous,
        "an unseeded anonymous key fails closed (the seeded mint is the r6b positive differential)"
    );
}

// RUE-1091 r6b: the anonymous arm mints once a caller seeds the durable
// identity — the positive half of the deferral this slice flips (the r4b-2
// anonymous-arm pin). The pool relocates the durable producer key to its
// stable content, canonicalizes the producer wrapper on entry, and spells
// the `__anon_struct_{digest}` name; the render below pins that full
// materialization (digest name, symbol, flags, and field vocabulary).
#[test]
fn provider_endpoint_facts_anonymous_arm_mints_after_registration() {
    use rue_air::{
        AnonymousNominalKey, AnonymousNominalKind, NominalInstanceKey as N,
        SemanticDefinitionToken as DTok, StableProducerId, TypeInstanceKey as T,
    };
    // `Holder`'s field `p: Pair()` forces the epoch to instantiate the
    // comptime type function `Pair` at declaration bind, minting the anonymous
    // `struct { a: i32 }` whose producer roots at the INSTALLED function `Pair`
    // (an installed-endpoint producer — the pool's byte-equal minting scope).
    let source = "fn Pair() -> type { struct { a: i32 } }\n\
                      struct Holder { p: Pair() }\n\
                      fn main() -> i32 { 0 }\n";
    let snapshot = source_snapshot(&[(1, "/m.rue", "m.rue", source)], 1);

    // Independently produce the durable declaration set + the durable
    // anonymous nominal (the pool's inputs) through the nucleus projection.
    let stages = crate::test_support::test_frontend_stages(&snapshot).unwrap();
    let merged = &stages.merged;
    let mut proj_db = RevisionedQueryDatabase::default();
    let proj_revision = revision_for(&mut proj_db, &snapshot);
    let projection = proj_db
        .projected_declaration_semantics(
            proj_revision,
            merged.ast(),
            rue_target::Target::X86_64Linux,
            &crate::PreviewFeatures::default(),
            CancellationToken::new(),
        )
        .expect("declaration semantics project");
    assert_eq!(
        projection.anonymous_nominals.len(),
        1,
        "the program mints exactly one anonymous nominal"
    );
    // The durable identity is fed to the pool RAW: the declaration-SIGNATURE
    // projection retains the empty-argument specialization wrapper
    // (`Function(Specialization { base, args: [] })`) that production
    // body-export collapses to `Function(base)`
    // (`canonical_function_producer`). The pool canonicalizes ON ENTRY
    // (`find_or_create_anon` collapses via `with_canonical_producer`, and the
    // adapter keys shapes canonically), so handing the non-collapsed form
    // must dedup onto — and spell the digest of — the collapsed form. This is
    // the entry-canonicalization proof, not a de-quirked input.
    let durable_identity = projection.anonymous_nominals[0].identity.clone();

    let rir = &stages.rir;
    let rir_ref = rir.rir();
    let interner = rir.semantic_symbols().interner();
    let mut database = RevisionedQueryDatabase::default();
    let revision = revision_for(&mut database, &snapshot);
    let adapter = DurableDeclSource::from_declarations(&projection.declarations)
        .with_anonymous_nominals(&projection.anonymous_nominals);
    let identity_for_probe = durable_identity.clone();

    let outcome = database.probe_ready_body_facts(
        revision,
        semantic_configuration(),
        "endpoint-anon-mint",
        move |provider| {
            let facts = rue_air::ProviderEndpointFacts::new(
                provider,
                adapter,
                rue_air::BodyRirView::from_parts(rir_ref, interner),
            );
            // Direct pool mint (the keystone): mint the anonymous nominal from
            // its durable identity + shape.
            let minted = facts
                .mint_anonymous(&identity_for_probe)
                .expect("the pool mints the anonymous nominal");
            // Idempotency: a repeat consult re-mints nothing.
            let again = facts
                .mint_anonymous(&identity_for_probe)
                .expect("repeat consult re-resolves");
            assert_eq!(minted, again, "the pool re-minted the anonymous nominal");
            // Entry canonicalization: the RAW projected identity (with its
            // empty-argument specialization wrapper) and its collapsed
            // canonical-producer form dedup onto the same minted nominal.
            let collapsed = identity_for_probe.with_canonical_producer().into_owned();
            let canonical_mint = facts
                .mint_anonymous(&collapsed)
                .expect("the collapsed identity resolves by dedup");
            assert_eq!(
                minted, canonical_mint,
                "the RAW identity must collapse onto the canonical producer form"
            );

            // The resolve_instance_type anonymous arm: seed the issued→durable
            // map, then resolve an issued-domain anonymous key — the r4b-2
            // anonymous-arm flip. The issued key is an arbitrary lookup handle;
            // the durable key drives the mint / digest.
            let issued = AnonymousNominalKey {
                kind: AnonymousNominalKind::Struct,
                producer: StableProducerId::Definition(DTok::new(0x5b, 1)),
                anchor: rue_rir::RirStructuralAnchor::new(Vec::new()),
            };
            facts.register_anonymous_nominal(issued.clone(), identity_for_probe.clone());
            let via_arm = facts
                .resolve_instance_type(&T::Nominal(N::Anonymous(Node::new(issued))))
                .expect("the seeded anonymous arm resolves");
            assert_eq!(via_arm, minted, "the arm and direct mint agree");

            facts.with_type_pool(|pool| endpoint_nominal_render(pool, minted))
        },
    );
    let pool_render = outcome.result;

    // The full materialization is pinned: the `__anon_struct_{digest}` name
    // (stable digest over the durable identity), copyability, visibility,
    // mangled symbol, and field vocabulary.
    assert!(
        pool_render.display.starts_with("__anon_struct_"),
        "the pool spells the digest name: {}",
        pool_render.display
    );
    assert_eq!(
        pool_render.members,
        vec![("a".to_owned(), "i32".to_owned())],
        "the anonymous struct retains the produced field vocabulary"
    );
    assert_eq!(
        pool_render.symbol, "__anon_struct_5451c1711507279538bfbd6f415d97aa",
        "the mangled symbol is the stable digest spelling"
    );
    assert!(!pool_render.is_pub, "an anonymous nominal is not `pub`");
    assert!(
        pool_render.is_copy,
        "a single-`i32` anonymous struct is copyable"
    );
}

// RUE-1091 r6b: the ENUM analog of the anonymous mint. The pool mints
// through `mint_anon_enum` from the durable shape, spelling the
// `__anon_enum_{digest}` bare source symbol. The pool is fed the RAW
// projected identity (empty-argument specialization wrapper
// retained), so the enum path exercises entry canonicalization too.
#[test]
fn provider_endpoint_facts_anonymous_enum_mints_from_durable_identity() {
    use rue_air::{
        AnonymousNominalKey, AnonymousNominalKind, NominalInstanceKey as N,
        SemanticDefinitionToken as DTok, StableProducerId, TypeInstanceKey as T,
    };
    // `Holder`'s field `o: Wrap()` forces the epoch to instantiate the
    // comptime type function `Wrap` at declaration bind, minting the anonymous
    // `enum { Some(i32), None }` whose producer roots at the INSTALLED
    // function `Wrap`.
    let source = "fn Wrap() -> type { enum { Some(i32), None } }\n\
                      struct Holder { o: Wrap() }\n\
                      fn main() -> i32 { 0 }\n";
    let snapshot = source_snapshot(&[(1, "/m.rue", "m.rue", source)], 1);

    // Independently produce the durable declaration set + the durable
    // anonymous nominal (the pool's inputs) through the nucleus projection.
    let stages = crate::test_support::test_frontend_stages(&snapshot).unwrap();
    let merged = &stages.merged;
    let mut proj_db = RevisionedQueryDatabase::default();
    let proj_revision = revision_for(&mut proj_db, &snapshot);
    let projection = proj_db
        .projected_declaration_semantics(
            proj_revision,
            merged.ast(),
            rue_target::Target::X86_64Linux,
            &crate::PreviewFeatures::default(),
            CancellationToken::new(),
        )
        .expect("declaration semantics project");
    assert_eq!(
        projection.anonymous_nominals.len(),
        1,
        "the program mints exactly one anonymous nominal"
    );
    assert!(
        matches!(
            projection.anonymous_nominals[0].shape,
            crate::durable_semantics::DurableAnonymousNominalShape::Enum { .. }
        ),
        "the projected anonymous nominal is an enum"
    );
    // RAW identity — the wrapper collapse is the pool's entry obligation.
    let durable_identity = projection.anonymous_nominals[0].identity.clone();
    let durable_source_symbol = projection.anonymous_nominals[0].source_symbol().clone();
    let durable_drop_glue_symbol = crate::local_semantic_materialization::rooted_callable_symbol(
        &crate::FunctionInstanceKey::DropGlue(Node::new(crate::TypeInstanceKey::Nominal(
            crate::NominalInstanceKey::Anonymous(Node::new(durable_identity.clone())),
        ))),
    );

    let rir = &stages.rir;
    let rir_ref = rir.rir();
    let interner = rir.semantic_symbols().interner();
    let mut database = RevisionedQueryDatabase::default();
    let revision = revision_for(&mut database, &snapshot);
    let adapter = DurableDeclSource::from_declarations(&projection.declarations)
        .with_anonymous_nominals(&projection.anonymous_nominals);
    let identity_for_probe = durable_identity.clone();
    let expected_source_symbol = durable_source_symbol.clone();
    let expected_drop_glue_symbol = durable_drop_glue_symbol.clone();

    let outcome = database.probe_ready_body_facts(
        revision,
        semantic_configuration(),
        "endpoint-anon-enum-mint",
        move |provider| {
            let facts = rue_air::ProviderEndpointFacts::new(
                provider,
                adapter,
                rue_air::BodyRirView::from_parts(rir_ref, interner),
            );
            // Direct pool mint from the RAW durable identity + shape.
            let minted = facts
                .mint_anonymous(&identity_for_probe)
                .expect("the pool mints the anonymous enum");
            // Idempotency: a repeat consult re-mints nothing.
            let again = facts
                .mint_anonymous(&identity_for_probe)
                .expect("repeat consult re-resolves");
            assert_eq!(minted, again, "the pool re-minted the anonymous enum");
            // Entry canonicalization: the RAW projected identity and its
            // collapsed canonical-producer form dedup onto the same mint.
            let collapsed = identity_for_probe.with_canonical_producer().into_owned();
            let canonical_mint = facts
                .mint_anonymous(&collapsed)
                .expect("the collapsed identity resolves by dedup");
            assert_eq!(
                minted, canonical_mint,
                "the RAW identity must collapse onto the canonical producer form"
            );

            // The resolve_instance_type anonymous arm over an issued ENUM key.
            let issued = AnonymousNominalKey {
                kind: AnonymousNominalKind::Enum,
                producer: StableProducerId::Definition(DTok::new(0x5b, 1)),
                anchor: rue_rir::RirStructuralAnchor::new(Vec::new()),
            };
            facts.register_anonymous_nominal(issued.clone(), identity_for_probe.clone());
            let via_arm = facts
                .resolve_instance_type(&T::Nominal(N::Anonymous(Node::new(issued))))
                .expect("the seeded anonymous enum arm resolves");
            assert_eq!(via_arm, minted, "the arm and direct mint agree");

            facts.with_type_pool(|pool| {
                let render = endpoint_nominal_render(pool, minted);
                let frozen = pool.clone().freeze();
                let drop_glue = rue_air::drop_glue_names::enum_drop_glue_name(
                    minted.as_enum().expect("the minted nominal is an enum"),
                    &frozen,
                );
                (render, drop_glue)
            })
        },
    );
    let (pool_render, pool_drop_glue_symbol) = outcome.result;

    assert_eq!(
        pool_render.symbol,
        expected_source_symbol.as_ref(),
        "the live pool enum symbol must equal the durable source symbol"
    );
    assert_eq!(
        pool_drop_glue_symbol,
        expected_drop_glue_symbol.as_ref(),
        "the live enum drop glue must equal the durable DropGlue symbol"
    );

    // The full materialization is pinned: the bare `__anon_enum_{digest}`
    // name (stable digest over the durable identity), copyability,
    // visibility, and variant vocabulary.
    assert!(
        pool_render.display.starts_with("__anon_enum_"),
        "the pool spells the enum digest name: {}",
        pool_render.display
    );
    assert_eq!(pool_render.display.len(), "__anon_enum_".len() + 32);
    assert_eq!(
        pool_render
            .members
            .iter()
            .map(|(name, _)| name.as_str())
            .collect::<Vec<_>>(),
        ["Some", "None"],
        "the anonymous enum retains the produced variant vocabulary"
    );
    assert!(!pool_render.is_pub, "an anonymous nominal is not `pub`");
    assert!(
        pool_render.is_copy,
        "an `i32`-payload anonymous enum is copyable"
    );
}

// RUE-1091 r6c: the well-known `Option` install. The trusted std
// `Option(payload)` specializations a body's fallible intrinsics demand
// (RUE-1112) are minted by the provider-side pool's
// `install_well_known_option_types` through `find_or_create_anon` via the
// real `DurableDeclSource` adapter, starting from the declaration-level
// durable truth (the nucleus `ComptimeCall` terminals the production demand
// loop roots). The full materializations are pinned: the digest-spelled
// `__anon_enum_{digest}` names, copyability, visibility, and variant
// vocabulary.
//
// The export-as-produced ruling: the pool records each installed canonical
// identity under `is_well_known_option_identity`, so the body publication
// path treats those identities as produced by the analyzed body, never as
// pre-existing imports.
#[test]
fn provider_well_known_option_install_mints_the_demanded_payloads() {
    use crate::semantic_query_nucleus::{
        ComptimeCallResultProjection as ResultProjection, SemanticNucleusKey as Key,
        SemanticNucleusValue as V,
    };

    // The freestanding fallible-intrinsic program plus the trusted `Option`
    // module published at its trusted logical path. `main` names
    // `@parse_i64` and `@parse_u32`, so its registered demand node names two
    // payloads and each maps directly to one exact comptime key.
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
    let snapshot = SourceSnapshot::new(
        metadata,
        vec![
            (
                root,
                Arc::new(
                    "fn main() -> i32 { let a = @parse_i64(\"1\"); \
                         let b = @parse_u32(\"2\"); 0 }"
                        .to_owned(),
                ),
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

    let stages = crate::test_support::test_frontend_stages(&snapshot).unwrap();
    let rir = &stages.rir;

    let configuration = semantic_configuration();
    let demands = [
        crate::well_known_option::FalliblePayload::I64,
        crate::well_known_option::FalliblePayload::U32,
    ]
    .map(|kind| crate::well_known_option::exact_option_query(kind, &configuration));

    // Resolve each demand through the nucleus — the declaration-level
    // durable truth BOTH installs consume — assembling the same
    // `WellKnownOptionResolution` the production `body_transaction` builds.
    let mut nucleus_db = RevisionedQueryDatabase::default();
    let nucleus_revision =
        nucleus_db.source_revision(&crate::session::ExactSourceInput::new(&snapshot), &snapshot);
    let mut option_by_payload = Vec::new();
    let mut nominals: BTreeMap<
        crate::AnonymousNominalKey,
        crate::durable_semantics::DurableAnonymousNominal,
    > = BTreeMap::new();
    for (payload, call) in demands {
        let value =
            request_semantic_nucleus(&nucleus_db, nucleus_revision, Key::ComptimeCall(call));
        let V::ComptimeCall(projection) = value else {
            panic!("trusted Option comptime call did not resolve: {value:?}");
        };
        let ResultProjection::Type(option_type) = &projection.result else {
            panic!(
                "Option(payload) must resolve to a type: {:?}",
                projection.result
            );
        };
        option_by_payload.push((payload, option_type.clone()));
        for nominal in projection.anonymous_nominals.iter() {
            nominals.insert(nominal.identity.clone(), nominal.clone());
        }
    }
    let resolution = crate::body_query::WellKnownOptionResolution {
        option_by_payload: Arc::from(option_by_payload),
        anonymous_nominals: Arc::from(nominals.into_values().collect::<Vec<_>>()),
    };
    assert_eq!(
        resolution.anonymous_nominals.len(),
        2,
        "one trusted Option enum per demanded payload"
    );
    assert!(
        resolution.anonymous_nominals.iter().all(|nominal| matches!(
            nominal.shape,
            crate::durable_semantics::DurableAnonymousNominalShape::Enum { .. }
        )),
        "the trusted registry holds enum shapes only"
    );

    // ------------------------------------------------------------------
    // The provider-side pool install: the demanded durable identities and
    // registry pairs, minted through `BodyIdentityPool::
    // install_well_known_option_types` over the real `DurableDeclSource`
    // adapter built from the production durable declarations.
    // ------------------------------------------------------------------
    let decls = production_declarations(&snapshot);
    let adapter = DurableDeclSource::from_declarations(&decls)
        .with_anonymous_nominals(&resolution.anonymous_nominals);
    let identities: Vec<crate::AnonymousNominalKey> = resolution
        .anonymous_nominals
        .iter()
        .map(|nominal| nominal.identity.clone())
        .collect();
    let pairs: Vec<(crate::DurableType, crate::DurableType)> =
        resolution.option_by_payload.iter().cloned().collect();

    let rir_ref = rir.rir();
    let interner = rir.semantic_symbols().interner();
    let mut database = RevisionedQueryDatabase::default();
    let revision = revision_for(&mut database, &snapshot);
    let outcome = database.probe_ready_body_facts(
        revision,
        semantic_configuration(),
        "well-known-option-install",
        move |provider| {
            let facts = rue_air::ProviderEndpointFacts::new(
                provider,
                adapter,
                rue_air::BodyRirView::from_parts(rir_ref, interner),
            );
            facts
                .install_well_known_option_types(&identities, &pairs)
                .expect("the pool installs the well-known registry");
            // Idempotent: a repeat install dedups onto the same identities.
            facts
                .install_well_known_option_types(&identities, &pairs)
                .expect("a repeat install is a pure dedup");

            // The export-as-produced ruling, pool side: each installed
            // identity answers the baseline-subtraction predicate.
            assert_eq!(facts.well_known_option_identity_count(), 2);
            for identity in &identities {
                assert!(
                    facts.is_well_known_option_identity(identity),
                    "an installed identity carries the produced ruling"
                );
            }

            // Materializations, fetched by dedup lookup (nothing re-mints).
            let renders: Vec<EndpointNominalRender> = identities
                .iter()
                .map(|identity| {
                    let ty = facts
                        .mint_anonymous(identity)
                        .expect("an installed identity resolves by dedup");
                    facts.with_type_pool(|pool| endpoint_nominal_render(pool, ty))
                })
                .collect();
            let i64_option = facts
                .well_known_option_for_payload(rue_air::Type::I64)
                .expect("the pool registry answers i64");
            let u32_option = facts
                .well_known_option_for_payload(rue_air::Type::U32)
                .expect("the pool registry answers u32");
            let i64_render = facts.with_type_pool(|pool| endpoint_nominal_render(pool, i64_option));
            let u32_render = facts.with_type_pool(|pool| endpoint_nominal_render(pool, u32_option));
            (renders, i64_render, u32_render)
        },
    );
    let (mut pool_renders, pool_i64_render, pool_u32_render) = outcome.result;
    pool_renders.sort_by(|a, b| a.display.cmp(&b.display));

    // The install materialized exactly the demanded Option enums: digest
    // names, variant vocabulary, copyability, and visibility are pinned.
    for render in &pool_renders {
        assert!(
            render.display.starts_with("__anon_enum_"),
            "the pool spells the digest name: {}",
            render.display
        );
        assert_eq!(
            render
                .members
                .iter()
                .map(|(name, _)| name.as_str())
                .collect::<Vec<_>>(),
            ["Some", "None"],
            "each Option enum retains the trusted variant vocabulary"
        );
        assert!(!render.is_pub, "an anonymous nominal is not `pub`");
        assert!(render.is_copy, "an integer-payload Option enum is copyable");
    }
    assert_eq!(pool_i64_render.display.len(), "__anon_enum_".len() + 32);
    assert_eq!(pool_u32_render.display.len(), "__anon_enum_".len() + 32);
    let mut registry_renders = vec![pool_i64_render, pool_u32_render];
    registry_renders.sort_by(|a, b| a.display.cmp(&b.display));
    assert_eq!(
        pool_renders, registry_renders,
        "the demand registry answers with the exact installed materializations"
    );
}

// RUE-1091 r6a: the `Slice` arm resolves once a caller seeds the generated
// slice struct with `register_generated_slice`, minting the fat-pointer
// struct — the positive half of the deferral this slice flips.
#[test]
fn provider_endpoint_facts_slice_arm_resolves_after_registration() {
    use rue_air::{SemanticImportType as D, TypeInstanceKey as T};
    // The signature slice `[i64]` names the generated slice struct the pool
    // mints (ADR-0043).
    let source = "fn take(s: [i64]) -> i64 { 0 }\n\
                      fn main() -> i32 { 0 }\n";
    let snapshot = source_snapshot(&[(1, "/m.rue", "m.rue", source)], 1);

    let stages = crate::test_support::test_frontend_stages(&snapshot).unwrap();
    let rir = &stages.rir;
    let interner = rir.semantic_symbols().interner();
    let rir_ref = rir.rir();
    let mut database = RevisionedQueryDatabase::default();
    let revision = revision_for(&mut database, &snapshot);
    // The slice mint needs no durable nominals (its element is a primitive),
    // so an empty durable source suffices.
    let adapter = DurableDeclSource::from_declarations(&[]);

    let outcome = database.probe_ready_body_facts(
        revision,
        semantic_configuration(),
        "endpoint-slice",
        move |provider| {
            let facts = rue_air::ProviderEndpointFacts::new(
                provider,
                adapter,
                rue_air::BodyRirView::from_parts(rir_ref, interner),
            );
            // Seed the generated slice, then resolve the `Slice` arm.
            facts
                .register_generated_slice(&D::I64, "[i64]")
                .expect("register mints the slice struct");
            let key = T::Slice {
                element: Node::new(T::I64),
                name: std::sync::Arc::from("[i64]"),
            };
            let first = facts.resolve_instance_type(&key).expect("slice resolves");
            // Idempotency: a repeat consult returns the same id.
            let second = facts
                .resolve_instance_type(&key)
                .expect("slice re-resolves");
            assert_eq!(first, second, "repeat slice resolution diverged");
            facts.with_type_pool(|pool| endpoint_nominal_render(pool, first))
        },
    );
    // The provider-minted slice renders the generated fat-pointer struct
    // (name, copyability, visibility, symbol, fields).
    assert_eq!(
        outcome.result,
        EndpointNominalRender {
            display: "[i64]".to_owned(),
            is_copy: true,
            is_pub: true,
            symbol: "[i64]".to_owned(),
            members: vec![
                ("ptr".to_owned(), "ptr const i64".to_owned()),
                ("len".to_owned(), "u64".to_owned()),
            ],
        },
        "the generated `[i64]` slice struct materialization is pinned"
    );
    // A pool-answered materialization records no provider query edge (edge
    // honesty — the slice identity is minted, not a boundary lookup).
    assert!(
        outcome.dependencies.is_empty(),
        "the seeded slice resolution records no provider edge: {:?}",
        outcome.dependencies
    );
}

#[test]
fn provider_endpoint_facts_rir_ops_and_nominal_presence() {
    // Two structs sharing a method name, a destructor, and a free function:
    // the three RIR ops must disambiguate by the owner preimage, and the
    // provider-boundary presence check must kind-filter nominals from
    // functions.
    let source = "struct Widget { id: u32, \
             fn bump(self) -> u32 { self.id } \
             fn reset() -> u32 { 0 } }\n\
             struct Gadget { n: i32, \
             fn bump(self) -> i32 { self.n } }\n\
             drop fn Widget(self) {}\n\
             fn helper() -> i32 { 0 }\n\
             fn main() -> i32 { 0 }\n";
    let snapshot = source_snapshot(&[(1, "/m.rue", "m.rue", source)], 1);
    let file = FileId::new(1);
    let m = ModuleId::from_logical_path("m.rue").unwrap();
    let decls = production_declarations(&snapshot);
    let (rir_out, _semantic, _) =
        crate::test_support::test_frontend_snapshot(&snapshot, &crate::CompileOptions::default())
            .expect("frontend compiles");
    let rir = rir_out.rir();
    let interner = rir_out.semantic_symbols().interner();

    let mut database = RevisionedQueryDatabase::default();
    let revision = revision_for(&mut database, &snapshot);
    let adapter = DurableDeclSource::from_declarations(&decls);

    let outcome = database.probe_ready_body_facts(
        revision,
        semantic_configuration(),
        "endpoint-rir-ops",
        move |provider| {
            let facts = rue_air::ProviderEndpointFacts::new(
                provider,
                adapter,
                rue_air::BodyRirView::from_parts(rir, interner),
            );

            // (R) first_free_function: a free function resolves; a method
            // name and an absent name do not.
            let helper = facts.first_free_function("helper", file);
            let bump_free = facts.first_free_function("bump", file);
            let absent_free = facts.first_free_function("nonexistent", file);

            // (R) named_method_declaration: same-named methods on distinct
            // owners stay distinct; an absent method fails closed.
            let widget_bump = facts.named_method_declaration(file, "Widget", "bump");
            let gadget_bump = facts.named_method_declaration(file, "Gadget", "bump");
            let widget_reset = facts.named_method_declaration(file, "Widget", "reset");
            let widget_absent = facts.named_method_declaration(file, "Widget", "nonexistent");

            // (R) destructor: present for Widget, absent for Gadget.
            let widget_drop = facts.destructor(file, "Widget");
            let gadget_drop = facts.destructor(file, "Gadget");

            // (C) nominal presence via the provider boundary (records the
            // lookup-name edge): a struct is present, a function is not (kind
            // filter), an absent name is not.
            let widget_present = facts.nominal_contains_in_module(
                &m,
                "Widget",
                rue_air::AnonymousNominalKind::Struct,
            );
            let helper_as_struct = facts.nominal_contains_in_module(
                &m,
                "helper",
                rue_air::AnonymousNominalKind::Struct,
            );
            let missing_present = facts.nominal_contains_in_module(
                &m,
                "Missing",
                rue_air::AnonymousNominalKind::Struct,
            );

            (
                helper.is_some(),
                bump_free.is_none(),
                absent_free.is_none(),
                widget_bump,
                gadget_bump,
                widget_reset.is_some(),
                widget_absent.is_none(),
                widget_drop.is_some(),
                gadget_drop.is_none(),
                widget_present,
                helper_as_struct,
                missing_present,
            )
        },
    );
    let (
        helper,
        bump_not_free,
        absent_free,
        widget_bump,
        gadget_bump,
        widget_reset,
        widget_absent,
        widget_drop,
        gadget_no_drop,
        widget_present,
        helper_as_struct,
        missing_present,
    ) = outcome.result;
    assert!(helper, "helper is a free function");
    assert!(bump_not_free, "bump is a method, not a free function");
    assert!(absent_free, "an absent free function fails closed");
    assert!(widget_bump.is_some() && gadget_bump.is_some());
    assert_ne!(
        widget_bump, gadget_bump,
        "same-named methods on distinct owners stay distinct declarations"
    );
    assert!(widget_reset, "Widget.reset resolves");
    assert!(widget_absent, "an absent method fails closed");
    assert!(widget_drop, "Widget has a destructor");
    assert!(gadget_no_drop, "Gadget has no destructor");
    assert!(widget_present, "Widget is a declared struct");
    assert!(!helper_as_struct, "a free function is not a struct");
    assert!(!missing_present, "an absent name is not a struct");

    // The presence check observes the provider's name-lookup terminal — the
    // post-flip edge truth the epoch's table lookup masks.
    assert!(
        outcome
            .dependencies
            .iter()
            .any(|node| node.family() == "compiler.lookup-name"),
        "nominal presence observes the name-lookup terminal: {:?}",
        outcome.dependencies
    );
}

// ---- RUE-1091 flip-prep: const identity differential --------------------

#[derive(Debug, Clone, PartialEq, Eq)]
struct ConstInfoRender {
    is_pub: bool,
    ty: String,
    value: String,
    span: rue_span::Span,
}

fn render_const_info(
    info: &rue_air::ConstInfo,
    pool: &rue_air::TypeInternPool,
    resolve_symbol: impl Fn(lasso::Spur) -> String,
) -> ConstInfoRender {
    use rue_air::ConstValue as V;
    let value = match info.value {
        V::Integer(value) => format!("integer:{value}"),
        V::Bool(value) => format!("bool:{value}"),
        V::Type(value) => format!("type:{}", endpoint_display(pool, value)),
        V::Function(value) => format!("function:{}", resolve_symbol(value.spur())),
        V::Unit => "unit".to_owned(),
        V::String(value) => format!("string:{}", resolve_symbol(value.spur())),
        V::Float(value) => format!("float:{}", resolve_symbol(value.spur())),
    };
    ConstInfoRender {
        is_pub: info.is_pub,
        ty: endpoint_display(pool, info.ty),
        value,
        span: info.span,
    }
}

#[test]
fn provider_const_info_assembly_composes_durable_truth_with_exact_spans() {
    use crate::StableDefinitionKind as Kind;

    // Exercise scalar, nominal type-valued, function-valued, and string
    // constants plus a module binding joined through the shared provider
    // module registry.
    let root = "pub struct Point { x: i32 }\n\
                    fn helper() -> i32 { 1 }\n\
                    pub const LIMIT: i64 = 7;\n\
                    const POINT_KIND: type = Point;\n\
                    const ALIAS = helper;\n\
                    const TEXT: str = \"hello\";\n\
                    const dep = @import(\"dep.rue\");\n\
                    fn main() -> i32 { 0 }\n";
    let dep = "pub const DEP_VALUE: i32 = 9;\n";
    let snapshot = source_snapshot(
        &[
            (1, "/project/main.rue", "main.rue", root),
            (2, "/project/dep.rue", "dep.rue", dep),
        ],
        1,
    );
    let file = FileId::new(1);
    let decls = production_declarations(&snapshot);
    let value_keys = ["LIMIT", "POINT_KIND", "ALIAS", "TEXT"].map(|name| {
        (
            name,
            durable_decl(&decls, Kind::ValueConst, name).key.clone(),
        )
    });
    let module_target = match &durable_decl(&decls, Kind::ModuleBinding, "dep").payload {
        crate::durable_semantics::DurableDeclarationPayload::ModuleBinding { target } => {
            target.clone()
        }
        _ => unreachable!(),
    };

    let stages = crate::test_support::test_frontend_stages(&snapshot).unwrap();
    let rir = &stages.rir;
    let interner = rir.semantic_symbols().interner();
    // Expected declaration spans, derived from the fixture text.
    let span_of = |text: &str| {
        let start = u32::try_from(root.find(text).unwrap()).unwrap();
        rue_span::Span::with_file(file, start, start + u32::try_from(text.len()).unwrap())
    };

    // Pool side: the production durable declaration adapter plus the real
    // ProviderEndpointFacts registration primitive, which composes the
    // exact `(file, name)` RIR handle with the durable const record.
    let rir_ref = rir.rir();
    let adapter = DurableDeclSource::from_declarations(&decls);
    let mut database = RevisionedQueryDatabase::default();
    let revision = revision_for(&mut database, &snapshot);
    let outcome = database.probe_ready_body_facts(
        revision,
        semantic_configuration(),
        "endpoint-const-info",
        move |provider| {
            let identity = rue_air::ProviderIdentityContext::new(adapter);
            let rir_view = rue_air::BodyRirView::from_parts(rir_ref, interner);
            let facts = rue_air::ProviderEndpointFacts::with_identity(
                provider,
                identity.clone(),
                rir_view.clone(),
            );
            let calls =
                rue_air::ProviderCallFacts::with_identity(provider, identity.clone(), rir_view);
            let aggregate = rue_air::ProviderAggregateFacts::with_identity(identity);
            let mut rendered = Vec::new();
            for (name, key) in value_keys {
                let info = facts
                    .const_info(&key, file, name)
                    .unwrap_or_else(|| panic!("pool resolves {name}"));
                let again = facts
                    .const_info(&key, file, name)
                    .unwrap_or_else(|| panic!("pool re-resolves {name}"));
                let first = facts.with_type_pool(|pool| {
                    render_const_info(&info, pool, |symbol| facts.resolve_const_symbol(symbol))
                });
                let second = facts.with_type_pool(|pool| {
                    render_const_info(&again, pool, |symbol| facts.resolve_const_symbol(symbol))
                });
                assert_eq!(first, second, "repeat consult re-minted {name}");
                calls.register_value_const(file, name, info.clone());
                aggregate.register_value_const(file, name, info);
                assert!(calls.value_const(file, name).is_some());
                assert!(matches!(
                    aggregate.select_module_type_member(file, name),
                    rue_air::ProviderModuleMember::Const
                ));
                let aggregate_info = aggregate
                    .value_const(file, name)
                    .expect("aggregate preserves the assembled const");
                let aggregate_render = aggregate.with_type_pool(|pool| {
                    render_const_info(&aggregate_info, pool, |symbol| {
                        facts.resolve_const_symbol(symbol)
                    })
                });
                assert_eq!(
                    aggregate_render, first,
                    "aggregate overlay changed the provider-assembled ConstInfo"
                );
                rendered.push((name, first));
            }

            facts
                .register_module(
                    module_target.clone(),
                    FileId::new(2),
                    "/project/dep.rue",
                    module_target.logical_path(),
                    module_target.logical_path(),
                )
                .expect("target module registers in the shared identity context");
            let module = facts
                .module_binding_info(file, "dep", &module_target, false)
                .expect("module binding joins its durable target to the registry");
            calls.register_module_binding(file, "dep", module.clone());
            aggregate.register_module_binding(file, "dep", module.clone());
            assert!(calls.module_binding(file, "dep").is_some());
            assert!(matches!(
                aggregate.select_module_type_member(file, "dep"),
                rue_air::ProviderModuleMember::Const
            ));
            let aggregate_module = aggregate
                .module_binding(file, "dep")
                .expect("aggregate preserves the assembled module binding");
            let module = facts.with_type_pool(|pool| {
                render_const_info(&module, pool, |symbol| facts.resolve_const_symbol(symbol))
            });
            let aggregate_module = aggregate.with_type_pool(|pool| {
                render_const_info(&aggregate_module, pool, |symbol| {
                    facts.resolve_const_symbol(symbol)
                })
            });
            assert_eq!(
                aggregate_module, module,
                "aggregate overlay changed the provider-assembled module ConstInfo"
            );
            (rendered, module)
        },
    );
    let expected = vec![
        (
            "LIMIT",
            ConstInfoRender {
                is_pub: true,
                ty: "i64".to_owned(),
                value: "integer:7".to_owned(),
                span: span_of("pub const LIMIT: i64 = 7;"),
            },
        ),
        (
            "POINT_KIND",
            ConstInfoRender {
                is_pub: false,
                ty: "type".to_owned(),
                value: "type:Point".to_owned(),
                span: span_of("const POINT_KIND: type = Point;"),
            },
        ),
        (
            "ALIAS",
            ConstInfoRender {
                is_pub: false,
                ty: "type".to_owned(),
                value: "function:helper".to_owned(),
                span: span_of("const ALIAS = helper;"),
            },
        ),
        (
            "TEXT",
            ConstInfoRender {
                is_pub: false,
                ty: "str".to_owned(),
                value: "string:hello".to_owned(),
                span: span_of("const TEXT: str = \"hello\";"),
            },
        ),
    ];
    assert_eq!(
        outcome.result.0, expected,
        "pool-assembled ConstInfo must compose the durable record with the exact RIR span"
    );
    assert_eq!(
        outcome.result.1,
        ConstInfoRender {
            is_pub: false,
            ty: "Module(ModuleId(0))".to_owned(),
            value: "type:Module(ModuleId(0))".to_owned(),
            span: span_of("const dep = @import(\"dep.rue\");"),
        },
        "the module binding joins its durable target to the shared registry"
    );
    assert!(
        outcome.dependencies.is_empty(),
        "const assembly uses durable metadata + RIR only: {:?}",
        outcome.dependencies
    );
}

// ---- RUE-1091 r4b-3: aggregate ProviderFacts coverage --------------------
//
// These prove `rue_air::ProviderAggregateFacts` (the provider-driven
// realization of the family-1D `AggregateFacts` seam) selects the declared
// aggregate/field/variant winner. The selection ORDER lives in the
// provider-generic free functions the driver merely supplies facts to
// (`select_module_type_member`'s struct→enum→const short-circuit,
// `select_qualified_type`'s enum→struct, `select_struct_literal_head`'s
// const→struct→builtin) — the exact r1c candidate order. The driver reuses
// the shared `DurableDeclSource` (the r4b-1 durable set) for its 2a pool;
// each winner is pinned through its index-independent render.
//
// Scope landed here: struct/enum-by-file-name (P, pool mint via the overlay
// reverse), builtins (P, pool pre-registered set), `is_accessible` (O,
// request-local file paths), const overlays, and the body-local module
// registry. The production cutover owns assembling and registering those
// request-local facts.

/// The tag + index-independent display of a [`rue_air::ProviderModuleMember`],
/// rendered through the pool that minted its type.
fn describe_member(
    member: &rue_air::ProviderModuleMember,
    pool: &rue_air::TypeInternPool,
) -> (&'static str, Option<String>) {
    match member {
        rue_air::ProviderModuleMember::Struct(ty) => ("struct", Some(endpoint_display(pool, *ty))),
        rue_air::ProviderModuleMember::Enum(ty) => ("enum", Some(endpoint_display(pool, *ty))),
        rue_air::ProviderModuleMember::Const => ("const", None),
        rue_air::ProviderModuleMember::Absent => ("absent", None),
    }
}

/// The tag + display of a [`rue_air::ProviderQualifiedType`].
fn describe_qualified(
    qualified: &rue_air::ProviderQualifiedType,
    pool: &rue_air::TypeInternPool,
) -> (&'static str, Option<String>) {
    match qualified {
        rue_air::ProviderQualifiedType::Enum(ty) => ("enum", Some(endpoint_display(pool, *ty))),
        rue_air::ProviderQualifiedType::Struct(ty) => ("struct", Some(endpoint_display(pool, *ty))),
        rue_air::ProviderQualifiedType::Absent => ("absent", None),
    }
}

#[test]
fn provider_aggregate_facts_resolve_nominals_and_builtins() {
    use crate::StableDefinitionKind as Kind;
    // A user struct and enum (minted through the pool's 2a machinery via the
    // `(file, name)` overlay reverse) plus the pool's pre-registered builtins.
    let source = "pub struct Point { x: i64, y: i64 }\n\
                      pub enum Color { Red, Green }\n\
                      fn main() -> i32 { 0 }\n";
    let snapshot = source_snapshot(&[(1, "/m.rue", "m.rue", source)], 1);
    let file = FileId::new(1);
    let decls = production_declarations(&snapshot);
    let point_key = durable_decl(&decls, Kind::Struct, "Point").key.clone();
    let color_key = durable_decl(&decls, Kind::Enum, "Color").key.clone();

    let mut facts =
        rue_air::ProviderAggregateFacts::new(DurableDeclSource::from_declarations(&decls));
    facts.register_named_nominal(point_key, file, "Point");
    facts.register_named_nominal(color_key, file, "Color");

    let point = facts.struct_in_file(file, "Point").expect("Point resolves");
    let point_again = facts
        .struct_in_file(file, "Point")
        .expect("repeat resolves");
    assert_eq!(point, point_again, "repeat consult dedups the nominal");
    let color = facts.enum_in_file(file, "Color").expect("Color resolves");
    let str_ty = facts.builtin_struct("str").expect("builtin str resolves");
    let arch_ty = facts
        .builtin_enum("Arch")
        .expect("builtin Arch enum resolves");

    // A struct is not an enum and vice versa (kind-filtered by the id kind).
    assert!(
        facts.enum_in_file(file, "Point").is_none(),
        "Point is not an enum"
    );
    assert!(
        facts.struct_in_file(file, "Color").is_none(),
        "Color is not a struct"
    );
    assert!(
        facts.struct_in_file(file, "Absent").is_none(),
        "absent fails closed"
    );
    assert!(
        facts.builtin_struct("NotABuiltin").is_none(),
        "unknown builtin fails closed"
    );

    facts.with_type_pool(|pool| {
        assert_eq!(endpoint_display(pool, point), "Point");
        assert_eq!(endpoint_display(pool, color), "Color");
        // Builtins are pre-registered in the pool.
        assert_eq!(endpoint_display(pool, str_ty), "str");
        assert_eq!(endpoint_display(pool, arch_ty), "Arch");
    });
}

#[test]
fn provider_aggregate_facts_selection_order_follows_the_candidate_ranking() {
    use crate::StableDefinitionKind as Kind;
    // A struct, an enum, and a value constant sharing one module exercise
    // the struct→enum→const short-circuit.
    let source = "pub struct Point { x: i64 }\n\
                      pub enum Color { Red, Green }\n\
                      pub const LIMIT: i64 = 7;\n\
                      fn main() -> i32 { 0 }\n";
    let snapshot = source_snapshot(&[(1, "/m.rue", "m.rue", source)], 1);
    let file = FileId::new(1);
    let decls = production_declarations(&snapshot);
    let point_key = durable_decl(&decls, Kind::Struct, "Point").key.clone();
    let color_key = durable_decl(&decls, Kind::Enum, "Color").key.clone();
    let limit_key = durable_decl(&decls, Kind::ValueConst, "LIMIT").key.clone();

    let stages = crate::test_support::test_frontend_stages(&snapshot).unwrap();
    let rir = &stages.rir;
    let interner = rir.semantic_symbols().interner();
    let expected_limit_span = {
        let text = "pub const LIMIT: i64 = 7;";
        let start = u32::try_from(source.find(text).unwrap()).unwrap();
        rue_span::Span::with_file(file, start, start + u32::try_from(text.len()).unwrap())
    };

    let rir_ref = rir.rir();
    let mut database = RevisionedQueryDatabase::default();
    let revision = revision_for(&mut database, &snapshot);
    let outcome = database.probe_ready_body_facts(
        revision,
        semantic_configuration(),
        "aggregate-selection-order",
        move |provider| {
            let identity =
                rue_air::ProviderIdentityContext::new(DurableDeclSource::from_declarations(&decls));
            let endpoint = rue_air::ProviderEndpointFacts::with_identity(
                provider,
                identity.clone(),
                rue_air::BodyRirView::from_parts(rir_ref, interner),
            );
            let mut facts = rue_air::ProviderAggregateFacts::with_identity(identity);
            facts.register_named_nominal(point_key, file, "Point");
            facts.register_named_nominal(color_key, file, "Color");
            let limit_info = endpoint
                .const_info(&limit_key, file, "LIMIT")
                .expect("endpoint assembles LIMIT from durable truth + exact RIR span");
            let endpoint_limit_render = endpoint.with_type_pool(|pool| {
                render_const_info(&limit_info, pool, |symbol| {
                    endpoint.resolve_const_symbol(symbol)
                })
            });
            facts.register_value_const(file, "LIMIT", limit_info);
            let aggregate_limit_info = facts
                .value_const(file, "LIMIT")
                .expect("aggregate retains LIMIT's complete ConstInfo");
            let aggregate_limit_render = facts.with_type_pool(|pool| {
                render_const_info(&aggregate_limit_info, pool, |symbol| {
                    endpoint.resolve_const_symbol(symbol)
                })
            });

            // select_module_type_member: struct wins first, enum second,
            // const third, absent last.
            let member_point = facts.select_module_type_member(file, "Point");
            let member_color = facts.select_module_type_member(file, "Color");
            let member_limit = facts.select_module_type_member(file, "LIMIT");
            let member_absent = facts.select_module_type_member(file, "Ghost");
            let qualified_color = facts.select_qualified_type(file, "Color");
            let qualified_point = facts.select_qualified_type(file, "Point");
            let qenum_color = facts.select_qualified_enum(file, "Color");
            let qenum_point = facts.select_qualified_enum(file, "Point");
            let head_point = facts.select_struct_literal_head(file, "Point");

            facts.with_type_pool(|pool| {
                (
                    describe_member(&member_point, pool),
                    describe_member(&member_color, pool),
                    describe_member(&member_limit, pool).0,
                    describe_member(&member_absent, pool).0,
                    describe_qualified(&qualified_color, pool),
                    describe_qualified(&qualified_point, pool),
                    qenum_color.is_some(),
                    qenum_point.is_some(),
                    match head_point {
                        rue_air::ProviderStructHead::Named(ty) => Some(endpoint_display(pool, ty)),
                        _ => None,
                    },
                    endpoint_limit_render,
                    aggregate_limit_render,
                )
            })
        },
    );
    let (
        (mp_tag, mp_disp),
        (mc_tag, mc_disp),
        ml_tag,
        ma_tag,
        (qc_tag, qc_disp),
        (qp_tag, qp_disp),
        qenum_color,
        qenum_point,
        head_point,
        endpoint_limit_render,
        aggregate_limit_render,
    ) = outcome.result;
    assert_eq!(
        endpoint_limit_render,
        ConstInfoRender {
            is_pub: true,
            ty: "i64".to_owned(),
            value: "integer:7".to_owned(),
            span: expected_limit_span,
        },
        "endpoint assembles LIMIT from durable truth plus the exact RIR span"
    );
    assert_eq!(
        aggregate_limit_render, endpoint_limit_render,
        "aggregate const overlay preserves provider-assembled type, value, visibility, and span"
    );

    // select_module_type_member winners: struct, enum, const, absent.
    assert_eq!(mp_tag, "struct");
    assert_eq!(mp_disp.as_deref(), Some("Point"));
    assert_eq!(mc_tag, "enum");
    assert_eq!(mc_disp.as_deref(), Some("Color"));
    assert_eq!(ml_tag, "const", "the provider selects the const member");
    assert_eq!(ma_tag, "absent");

    // Qualified selection: enum→struct order.
    assert_eq!((qc_tag, qc_disp.as_deref()), ("enum", Some("Color")));
    assert_eq!((qp_tag, qp_disp.as_deref()), ("struct", Some("Point")));

    // select_qualified_enum: enum resolves, struct does not.
    assert!(qenum_color, "Color qualified-enum resolves");
    assert!(!qenum_point, "Point is not a qualified enum");

    // select_struct_literal_head: unqualified struct head → Named.
    assert_eq!(head_point.as_deref(), Some("Point"));
}

#[test]
fn provider_aggregate_facts_is_accessible_follows_the_directory_domain() {
    // The visibility domain is the parent directory, so a private item is
    // visible within its own file and from a sibling file, but not across
    // directories; a public item is visible either way. The driver decides
    // from the registered physical paths (a request-local body-query input,
    // not a durable fact), proving the visibility short-circuit.
    let root_src = "pub struct A { x: i32 }\n\
             fn main() -> i32 { 0 }\n";
    let leaf_src = "pub struct B { y: i32 }\n";
    let sibling_src = "pub struct C { z: i32 }\n";
    let root_file = FileId::new(1);
    let leaf_file = FileId::new(2);
    let sibling_file = FileId::new(3);
    let unknown_file = FileId::new(4);
    let metadata = SourceMetadata::new_with_trusted_standard_library(
        root_file,
        AHashMap::from([
            (root_file, "/project/main.rue".to_owned()),
            (leaf_file, "/project/std/leaf.rue".to_owned()),
            (sibling_file, "/project/helper.rue".to_owned()),
        ]),
        AHashMap::from([
            (root_file, "main.rue".to_owned()),
            (leaf_file, "\0rue-std/leaf.rue".to_owned()),
            (sibling_file, "helper.rue".to_owned()),
        ]),
        AHashSet::from([leaf_file]),
    )
    .expect("trusted-std metadata is valid");
    let snapshot = SourceSnapshot::new(
        metadata,
        vec![
            (root_file, Arc::new(root_src.to_owned())),
            (leaf_file, Arc::new(leaf_src.to_owned())),
            (sibling_file, Arc::new(sibling_src.to_owned())),
        ],
    )
    .expect("three-file snapshot is valid");

    let decls = production_declarations(&snapshot);

    // No K-typed argument pins the pool key here (is_accessible is path-only),
    // so name the durable key / module explicitly.
    let mut facts = rue_air::ProviderAggregateFacts::<StableDefinitionKey, ModuleId, _>::new(
        DurableDeclSource::from_declarations(&decls),
    );
    // Register the snapshot's physical paths — the request-local body-query
    // input the visibility short-circuit consults.
    let physical_paths = [
        (root_file, "/project/main.rue"),
        (leaf_file, "/project/std/leaf.rue"),
        (sibling_file, "/project/helper.rue"),
    ];
    for (file, path) in physical_paths {
        facts.register_file_path(file, path);
    }

    // Every combination of (accessing, defining, is_public) follows the
    // parent-directory visibility rule: public is always visible, and a
    // private item is visible exactly when both files share a directory.
    let directory_of = |wanted: FileId| {
        physical_paths
            .iter()
            .find(|(file, _)| *file == wanted)
            .map(|(_, path)| &path[..path.rfind('/').unwrap()])
            .unwrap()
    };
    for &accessing in &[root_file, leaf_file, sibling_file] {
        for &defining in &[root_file, leaf_file, sibling_file] {
            for &is_public in &[false, true] {
                let expected = is_public || directory_of(accessing) == directory_of(defining);
                assert_eq!(
                    facts.is_accessible(accessing, defining, is_public),
                    expected,
                    "is_accessible for accessing={accessing:?} defining={defining:?} pub={is_public}"
                );
            }
        }
    }
    // Spot the load-bearing rows: same file sees private; cross-directory
    // private is hidden; public crosses.
    assert!(
        facts.is_accessible(root_file, root_file, false),
        "same file sees private"
    );
    assert!(
        facts.is_accessible(root_file, sibling_file, false),
        "same-directory sibling sees private"
    );
    assert!(
        !facts.is_accessible(root_file, leaf_file, false),
        "cross-dir private hidden"
    );
    assert!(
        facts.is_accessible(root_file, leaf_file, true),
        "public crosses directories"
    );
    assert!(
        facts.is_accessible(root_file, unknown_file, false)
            && facts.is_accessible(unknown_file, root_file, false),
        "an unknown path remains permissive"
    );
}

#[test]
fn provider_member_candidates_span_methods_and_assoc_fns_with_signature_handles() {
    use crate::declaration_candidate::DeclarationCandidateCategory as Cat;
    use rue_air::BodyFactProvider;
    // `get` is a method (self receiver); `make` is an associated function
    // (no self). Both share the compiler's method table and the production
    // resolver discriminates on `has_self` (MethodCalledAsAssocFn /
    // AssocFnCalledAsMethod). The provider must reach BOTH.
    let snapshot = source_snapshot(
        &[(
            1,
            "/m.rue",
            "m.rue",
            "struct Counter { value: i32, \
                 fn get(borrow self) -> i32 { self.value } \
                 fn make(start: i32) -> Counter { Counter { value: start } } }\n\
                 fn main() -> i32 { 0 }\n",
        )],
        1,
    );
    let m = ModuleId::from_logical_path("m.rue").unwrap();
    let mut database = RevisionedQueryDatabase::default();
    let revision = revision_for(&mut database, &snapshot);
    let config = semantic_configuration();
    let receiver = ReceiverTypeIdentity::new(m.clone(), "Counter", Cat::Struct);

    let receiver_probe = receiver.clone();
    let outcome =
        database.probe_ready_body_facts(revision, config.clone(), "members", move |provider| {
            (
                provider.method_candidates(&receiver_probe, "get"),
                provider.method_candidates(&receiver_probe, "make"),
                provider.method_candidates(&receiver_probe, "absent_member"),
                provider.operator_candidates(&receiver_probe, rue_air::OperatorName::Add),
            )
        });
    let (get, make, absent, add) = outcome.result;

    // `get` is a method (has_self); the candidate carries a follow-up handle.
    assert_eq!(get.len(), 1, "get is a candidate SET of one");
    let get_candidate = &get[0];
    assert_eq!(get_candidate.name.as_ref(), "get");
    assert_eq!(get_candidate.kind, rue_air::MemberKind::Method);
    assert!(
        get_candidate.has_self_receiver,
        "get takes a self receiver, sourced from its signature"
    );

    // `make` is an associated function (no self) and is reached through the
    // SAME member op — the BLOCKER-A category the old impl could not express.
    assert_eq!(make.len(), 1);
    let make_candidate = &make[0];
    assert_eq!(make_candidate.kind, rue_air::MemberKind::AssociatedFunction);
    assert!(
        !make_candidate.has_self_receiver,
        "make takes no self receiver — the MethodCalledAsAssocFn discriminator"
    );

    assert!(absent.is_empty());
    assert!(add.is_empty(), "Counter overloads no operator");

    // BLOCKER B: from a candidate's follow-up handle, the full signature is
    // reachable and equals the production epoch's — including receiver mode,
    // parameter modes, and return type.
    let sig_probe = get_candidate.declaration.clone();
    let sig_outcome =
        database.probe_ready_body_facts(revision, config.clone(), "member-sig", move |provider| {
            provider.signature(&sig_probe)
        });
    let provider_sig = sig_outcome.result.expect("get has a signature");
    let epoch_sig = request_semantic_nucleus(
        &database,
        revision,
        crate::semantic_query_nucleus::SemanticNucleusKey::Signature(
            crate::semantic_query_nucleus::DeclarationSemanticQueryKey {
                declaration: get_candidate.declaration.clone(),
                configuration: config.clone(),
            },
        ),
    );
    let crate::semantic_query_nucleus::SemanticNucleusValue::Signature(epoch_sig) = epoch_sig
    else {
        panic!("get has a signature")
    };
    assert_eq!(
        provider_sig, epoch_sig,
        "the candidate handle fetches the exact production signature (modes + return type)"
    );
    assert!(
        sig_outcome.dependencies.iter().any(|node| {
            node.family() == "compiler.semantic-nucleus"
                && node.key().starts_with("signature:")
                && node.key().contains("get")
        }),
        "signature facts observe the signature projection: {:?}",
        sig_outcome.dependencies
    );
    assert!(
        !sig_outcome.dependencies.iter().any(|node| {
            node.family() == "compiler.semantic-nucleus"
                && node.key().starts_with("identity:")
                && node.key().contains("get")
        }),
        "the resolved signature carries its definition key without a peer identity request: {:?}",
        sig_outcome.dependencies
    );

    // Differential: the candidate's visibility matches the method's own
    // semantic-nucleus identity terminal.
    let epoch_identity = request_semantic_nucleus(
        &database,
        revision,
        crate::semantic_query_nucleus::SemanticNucleusKey::Identity(
            crate::semantic_query_nucleus::DeclarationSemanticQueryKey {
                declaration: get_candidate.declaration.clone(),
                configuration: config,
            },
        ),
    );
    let crate::semantic_query_nucleus::SemanticNucleusValue::Identity(epoch_identity) =
        epoch_identity
    else {
        panic!("get has an identity")
    };
    assert_eq!(get_candidate.is_public, epoch_identity.is_public);

    // Edge-recording proof: candidates are sourced from semantic-nucleus,
    // for both the method and the associated-function member.
    assert!(
        outcome
            .dependencies
            .iter()
            .any(|node| node.family() == "compiler.semantic-nucleus" && node.key().contains("get")),
        "method candidate observes the method's nucleus terminal: {:?}",
        outcome.dependencies
    );
    assert!(
            outcome
                .dependencies
                .iter()
                .any(|node| node.family() == "compiler.semantic-nucleus"
                    && node.key().contains("make")),
            "assoc-fn candidate observes the assoc fn's nucleus terminal: {:?}",
            outcome.dependencies
        );
}

#[test]
fn provider_differential_over_representative_bodies() {
    use crate::declaration_candidate::DeclarationCandidateCategory as Cat;
    use rue_air::BodyFactProvider;
    // A body with a deterministic diagnostic: an ill-formed nominal naming an
    // undefined field type.
    let snapshot = source_snapshot(
        &[(
            1,
            "/m.rue",
            "m.rue",
            "struct Bad { field: Missing }\n\
                 struct Good { value: i32 }\n\
                 fn plain(x: i32) -> i32 { x }\n\
                 fn main() -> i32 { 0 }\n",
        )],
        1,
    );
    let m = ModuleId::from_logical_path("m.rue").unwrap();
    let mut database = RevisionedQueryDatabase::default();
    let revision = revision_for(&mut database, &snapshot);
    let config = semantic_configuration();

    let bad = declaration_candidate(&database, revision, &m, Cat::Struct, "Bad");
    let good = declaration_candidate(&database, revision, &m, Cat::Struct, "Good");
    let plain = declaration_candidate(&database, revision, &m, Cat::Function, "plain");

    let bad_probe = bad.clone();
    let good_probe = good.clone();
    let plain_probe = plain.clone();
    let outcome = database.probe_ready_body_facts(
        revision,
        config.clone(),
        "representative",
        move |provider| {
            (
                provider.nominal_well_formedness(&bad_probe),
                provider.nominal_well_formedness(&good_probe),
                provider.signature(&plain_probe),
            )
        },
    );
    let (bad_wf, good_wf, plain_sig) = outcome.result;

    // The diagnostics body's nominal is ill-formed; the good one is not. Both
    // match the semantic-nucleus well-formedness terminal.
    assert_eq!(bad_wf, Some(rue_air::NominalWellFormedness::IllFormed));
    assert_eq!(good_wf, Some(rue_air::NominalWellFormedness::WellFormed));
    let epoch_bad = request_semantic_nucleus(
        &database,
        revision,
        crate::semantic_query_nucleus::SemanticNucleusKey::NominalWellFormedness(
            crate::semantic_query_nucleus::DeclarationSemanticQueryKey {
                declaration: bad.clone(),
                configuration: config.clone(),
            },
        ),
    );
    assert!(
        matches!(
            epoch_bad,
            crate::semantic_query_nucleus::SemanticNucleusValue::Failure(_)
        ),
        "the production epoch also fails Bad's well-formedness"
    );

    assert!(plain_sig.is_some());

    let plain_instance = free_function_instance(&m, "plain");
    let producer = database.probe_body_facts(
        revision,
        config.clone(),
        "representative-producer",
        move |provider| provider.producer_body_facts(&plain_instance),
    );
    assert!(matches!(
        producer,
        Ok(ProviderProbeOutcome {
            result: Some(crate::body_query::ProducedAnonymous::Produced(_)),
            ..
        })
    ));

    let missing_module = ModuleId::from_logical_path("missing.rue").unwrap();
    let missing = database.probe_body_facts(
        revision,
        config,
        "representative-missing",
        move |provider| {
            provider.lookup_unqualified(
                &missing_module,
                rue_air::ProviderNamespace::ModuleItem,
                "never",
            )
        },
    );
    assert!(matches!(
        missing,
        Err(CompilerBodyProviderStatus::Incomplete(
            CompilerBodyProviderIncomplete::MissingInput(_)
        ))
    ));
}

#[test]
fn retained_provider_specialization_materializes_with_live_air_parity() {
    let snapshot = source_snapshot(
        &[(
            1,
            "/m.rue",
            "m.rue",
            "fn selected(comptime N: i32, value: i32) -> i32 { value + N }\n\
                 fn main() -> i32 { selected(7, 5) }\n",
        )],
        1,
    );
    let module = ModuleId::from_logical_path("m.rue").unwrap();
    let base_instance = free_function_instance(&module, "selected");
    let crate::FunctionInstanceKey::Definition(base) = base_instance else {
        unreachable!("free function helper returns a definition")
    };
    let arguments = crate::CanonicalArguments {
        types: Arc::from([]),
        values: Arc::from([crate::CanonicalArgumentValue::Integer(7)]),
    };
    let instance = crate::FunctionInstanceKey::Specialization {
        base: Node::new(crate::FunctionInstanceKey::Definition(base.clone())),
        arguments: arguments.clone(),
    };
    let configuration = semantic_configuration();
    let key = crate::body_query::BodyQueryKey::new(instance.clone(), configuration.clone());
    let mut database = RevisionedQueryDatabase::default();
    let revision = revision_for(&mut database, &snapshot);
    let input = database
        .body_input(revision, key, CancellationToken::new())
        .expect("specialized body input request completes");
    let rue_query::QueryOutcome::Success(crate::body_query::BodyInputValue::Available(input)) =
        input.outcome()
    else {
        panic!("specialized body input is available: {input:?}")
    };
    let input = input.clone();
    let bundle = input
        .artifacts
        .plan
        .materialize_body_rir_bundle(
            &rue_rir::SharedSymbolSpace::private(),
            input.source.file_id,
            input.source.declaration_start,
            input.source.source_length,
            || Ok(()),
        )
        .expect("canonical specialized body plan materializes");
    let preview = configuration
        .preview_features
        .names()
        .iter()
        .filter_map(|name| name.parse().ok())
        .collect();
    let owner_source = rue_air::DurableBodySourceLocator {
        file_id: input.source.file_id,
        physical_path: input.source.physical_path.clone(),
        source_length: input.source.source_length,
        source_text: input.source.source_text.clone(),
    };
    let probe_base = base.clone();
    let probe_arguments = arguments.clone();
    let probe_module = module.clone();
    let target = configuration.target;
    let outcome = database.probe_ready_body_facts(
        revision,
        configuration,
        "retained-specialization-local-materialization",
        move |provider| {
            let source = CompilerBodyDurableSource::with_anonymous(
                provider,
                &[],
                Some((probe_module, owner_source)),
            );
            rue_air::analyze_provider_specialized_body(
                provider,
                source,
                &bundle,
                probe_base.clone(),
                probe_base.name(),
                &probe_arguments,
                target,
                preview,
                &rue_air::ProviderWellKnownOptionFacts {
                    nominals: Vec::new(),
                    option_by_payload: Vec::new(),
                },
            )
        },
    );
    let analyzed = outcome
        .result
        .expect("real provider specialization analysis succeeds");

    // Capture the retained live semantic result before relocating its
    // durable export. These fields are deliberately the provider result's
    // issuing AIR, pool, interner, strings, and warnings — not a second
    // materialization wrapper.
    let live_name = analyzed.function.name.clone();
    let live_air = format!("{:?}", analyzed.function.air);
    let live_callable_kind = analyzed.function.callable_kind;
    let live_num_locals = analyzed.function.num_locals;
    let live_num_param_slots = analyzed.function.num_param_slots;
    let live_param_modes = analyzed.function.param_modes.clone();
    let live_allow_unreachable_code = analyzed.function.allow_unreachable_code;
    let live_body_start = analyzed
        .function
        .air
        .iter()
        .map(|(_, instruction)| instruction.span.start)
        .min()
        .expect("specialized AIR is non-empty");
    let live_body_end = analyzed
        .function
        .air
        .iter()
        .map(|(_, instruction)| instruction.span.end)
        .max()
        .expect("specialized AIR is non-empty");
    let live_pool_len = analyzed.type_pool.len();
    let live_source_symbol = analyzed
        .interner
        .get("value")
        .expect("retained provider interner owns analyzed source symbols");
    assert_eq!(analyzed.interner.resolve(&live_source_symbol), "value");
    let live_strings = analyzed.strings.clone();
    let live_warnings = format!("{:?}", analyzed.warnings);

    let definition_tokens = analyzed
        .definition_tokens
        .into_iter()
        .collect::<AHashMap<_, _>>();
    let module_tokens = analyzed
        .module_tokens
        .into_iter()
        .collect::<AHashMap<_, _>>();
    let definition = |token: &rue_air::SemanticDefinitionToken| {
        definition_tokens
            .get(token)
            .cloned()
            .ok_or(rue_air::SemanticStableResolutionFailure::Missing)
    };
    let relocate_module = |token: &rue_air::SemanticModuleToken| {
        module_tokens
            .get(token)
            .cloned()
            .ok_or(rue_air::SemanticStableResolutionFailure::Missing)
    };
    let live_identity = analyzed
        .function
        .identity
        .try_map_identities(&definition, &relocate_module)
        .expect("retained function identity relocates");
    let identity = analyzed
        .export
        .identity
        .try_map_keys(&definition, &relocate_module)
        .expect("specialization identity relocates");
    let body = analyzed
        .export
        .body
        .try_map_keys(&definition, &relocate_module)
        .expect("specialized body relocates");
    let dependencies = analyzed
        .export
        .dependencies
        .iter()
        .map(&definition)
        .collect::<Result<Vec<_>, _>>()
        .expect("specialization dependencies relocate");
    let canonical = crate::body_query::CanonicalBody::Specialization {
        identity,
        body,
        dependencies: dependencies.into(),
        dependency_boundary_complete: analyzed.export.dependency_boundary_complete,
    };
    let body_span = rue_span::Span::with_file(input.source.file_id, live_body_start, live_body_end);
    let callable = crate::local_semantic_materialization::LocalCallableFact {
        identity: instance,
        symbol: Arc::from(live_name.as_str()),
    };
    let materialized = crate::local_semantic_materialization::materialize_canonical_body_for_test(
        &canonical,
        body_span,
        &[],
        &[],
        std::slice::from_ref(&callable),
        &[],
        std::slice::from_ref(&module),
        &[],
        &[],
    )
    .expect("durable provider export materializes in a fresh local epoch");

    assert_eq!(materialized.identity, live_identity);
    assert_eq!(materialized.name, live_name);
    assert_eq!(materialized.callable_kind, live_callable_kind);
    assert_eq!(format!("{:?}", materialized.air), live_air);
    assert_eq!(materialized.num_locals, live_num_locals);
    assert_eq!(materialized.num_param_slots, live_num_param_slots);
    assert_eq!(materialized.param_modes, live_param_modes);
    assert_eq!(
        materialized.allow_unreachable_code,
        live_allow_unreachable_code
    );
    assert_eq!(materialized.type_pool.len(), live_pool_len);
    let local_name_symbol = materialized
        .interner
        .get(&materialized.name)
        .expect("local materialization interner owns the function symbol");
    assert_eq!(
        materialized.interner.resolve(&local_name_symbol),
        materialized.name
    );
    assert_eq!(materialized.strings, live_strings);
    assert_eq!(format!("{:?}", materialized.warnings), live_warnings);
}

#[test]
fn provider_producer_facts_preserve_specialization_instance_terminal() {
    use rue_air::BodyFactProvider;
    let snapshot = source_snapshot(
        &[(
            1,
            "/m.rue",
            "m.rue",
            "fn Pair() -> type { struct { value: i32 } }\n\
                 fn main() -> i32 { 0 }\n",
        )],
        1,
    );
    let module = ModuleId::from_logical_path("m.rue").unwrap();
    let mut database = RevisionedQueryDatabase::default();
    let revision = revision_for(&mut database, &snapshot);
    let configuration = semantic_configuration();
    let pair_instance = crate::FunctionInstanceKey::Specialization {
        base: Node::new(free_function_instance(&module, "Pair")),
        arguments: crate::CanonicalArguments::default(),
    };
    let registered_key =
        crate::body_query::BodyQueryKey::new(pair_instance.clone(), configuration.clone());

    let provider_instance = pair_instance.clone();
    let outcome = database.probe_ready_body_facts(
        revision,
        configuration,
        "producer-specialization-instance",
        move |provider| {
            (
                provider.producer_body_facts(&provider_instance),
                provider.trusted_toolchain_facts(&provider_instance),
            )
        },
    );
    let (provided, provider_toolchain) = outcome.result;
    let provided = provided.expect("the Pair specialization publishes producer facts");

    let direct = database.runtime.request_registered(
        &database.body_produced_anonymous,
        revision,
        registered_key.clone(),
        CancellationToken::new(),
    );
    let terminal = direct
        .terminal()
        .expect("the specialization-shaped registered terminal is retained");
    let rue_query::QueryOutcome::Success(expected) = terminal.outcome() else {
        panic!("the specialization-shaped registered terminal succeeds")
    };
    assert!(crate::body_query::produced_anonymous_equal(
        &provided, expected
    ));
    assert!(matches!(
        provided,
        crate::body_query::ProducedAnonymous::Produced(ref produced) if !produced.0.is_empty()
    ));
    let producer_edges = outcome
        .dependencies
        .iter()
        .filter(|node| node.family() == "compiler.body-produced-anonymous")
        .map(|node| node.key().to_owned())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        producer_edges,
        BTreeSet::from([registered_key.stable_identity()]),
        "the provider must observe only the exact specialization body terminal"
    );

    let direct_toolchain = database.runtime.request_registered(
        &database.body_toolchain_demands,
        revision,
        registered_key.clone(),
        CancellationToken::new(),
    );
    let toolchain_terminal = direct_toolchain
        .terminal()
        .expect("the specialization-shaped toolchain terminal is retained");
    let rue_query::QueryOutcome::Success(expected_toolchain) = toolchain_terminal.outcome() else {
        panic!("the specialization-shaped toolchain terminal succeeds")
    };
    assert_eq!(provider_toolchain, *expected_toolchain);
    let toolchain_edges = outcome
        .dependencies
        .iter()
        .filter(|node| node.family() == "compiler.body-toolchain-demands")
        .map(|node| node.key().to_owned())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        toolchain_edges,
        BTreeSet::from([registered_key.stable_identity()]),
        "the provider must observe only the exact specialization toolchain terminal"
    );
}
