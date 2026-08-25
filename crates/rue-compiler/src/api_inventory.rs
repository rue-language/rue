//! Semantic review gate for the curated compiler facade.
//!
//! RUE-869 replaces the old source-line budget with an exact inventory of
//! every root export and every public `CompilerSession`/
//! `CompilerSessionUpdate` signature. The inventory records ownership,
//! stability, category, and approved consumers so intentional API changes are
//! reviewed as semantic one-line diffs.

#[cfg(test)]
mod comptime_public_contract_tests {
    use rue_air::{
        ComptimeAnonymousKind, ComptimeArgMode, ComptimeCallAdmission, ComptimeCallArgument,
        ComptimeCallKey, ComptimeCallMemoLookup, ComptimeCompletedCallMemo, ComptimeEngine,
        ComptimeEnv, ComptimeField, ComptimeFile, ComptimeFrame, ComptimeHost, ComptimeHostResult,
        ComptimeIdentity, ComptimeMemoizedOutcome, ComptimeName, ComptimeOutcome, ComptimeProgram,
        ComptimeProgramKey, ComptimeProgramRegistry, ComptimeType, ComptimeValue,
    };
    use rue_rir::{Inst, InstData, InstRef, RirEditor, RirValidationContext, ValidatedRir};
    use rue_span::Span;
    use std::hash::{Hash, Hasher};
    use std::sync::Arc;

    #[derive(Clone, Debug, PartialEq, Eq)]
    struct FakeType;
    impl ComptimeType for FakeType {}

    #[derive(Clone, Debug, PartialEq, Eq)]
    enum FakeValue {
        Integer(i128),
        Boolean(bool),
        Unit,
        Type(FakeType),
    }
    impl ComptimeValue for FakeValue {
        type Type = FakeType;
        fn integer(value: i128) -> Self {
            Self::Integer(value)
        }
        fn boolean(value: bool) -> Self {
            Self::Boolean(value)
        }
        fn unit() -> Self {
            Self::Unit
        }
        fn type_value(value: <Self as ComptimeValue>::Type) -> Self {
            Self::Type(value)
        }
        fn as_integer(&self) -> Option<i128> {
            match self {
                Self::Integer(value) => Some(*value),
                _ => None,
            }
        }
        fn as_boolean(&self) -> Option<bool> {
            match self {
                Self::Boolean(value) => Some(*value),
                _ => None,
            }
        }
        fn as_type(&self) -> Option<<Self as ComptimeValue>::Type> {
            match self {
                Self::Type(value) => Some(value.clone()),
                _ => None,
            }
        }
    }

    #[derive(Clone, Debug, PartialEq, Eq)]
    struct FakeName;
    impl Hash for FakeName {
        fn hash<H: Hasher>(&self, state: &mut H) {
            0_u8.hash(state);
        }
    }
    impl ComptimeName for FakeName {}

    #[derive(Clone, Debug, PartialEq, Eq)]
    struct FakeFile;
    impl Hash for FakeFile {
        fn hash<H: Hasher>(&self, state: &mut H) {
            0_u8.hash(state);
        }
    }
    impl ComptimeFile for FakeFile {}

    #[derive(Clone, Debug, PartialEq, Eq)]
    struct FakeIdentity;
    impl ComptimeIdentity for FakeIdentity {}

    #[allow(dead_code)]
    fn generic_engine_entry<H: ComptimeHost>(
        host: &mut H,
        program: H::ProgramKey,
        root: InstRef,
    ) -> ComptimeOutcome<H::Value, H::Failure> {
        let _checkpoint: ComptimeHostResult<(), H::Failure> = host.check_canceled();
        let mut env =
            ComptimeEnv::<H::Value, H::Type, H::Name, H::File, H::CanonicalIdentity>::new();
        ComptimeEngine::new(host).evaluate(ComptimeFrame::expression(program, root), &mut env)
    }

    #[allow(dead_code)]
    fn generic_call_argument_contract<V>(argument: &ComptimeCallArgument<V>) {
        let _value = argument.value();
        let _direct_unit_literal = argument.is_direct_unit_literal();
    }

    #[test]
    fn public_domain_and_engine_contract_is_instantiable() {
        let _env: ComptimeEnv<'static, FakeValue, FakeType, FakeName, FakeFile, FakeIdentity> =
            ComptimeEnv::new();
        let _frame = ComptimeFrame {
            program: (),
            body: InstRef::from_raw(0),
            name: Some(FakeName),
            context: Some(FakeFile),
            span: Span::new(0, 0),
            function_span: Span::new(0, 0),
            type_bindings: ahash::AHashMap::<FakeName, FakeType>::new(),
            value_bindings: ahash::AHashMap::<FakeName, FakeValue>::new(),
            name_bindings: ahash::AHashMap::<FakeName, FakeName>::new(),
            call_identity: Some(FakeIdentity),
            expected_result: None,
        };
        let _field = ComptimeField {
            name: FakeName,
            ty: FakeType,
        };
        let _named_value_resolution = rue_air::ComptimeNamedValueResolution::Known(FakeValue::Unit);
        let _admission = ComptimeCallAdmission {
            name: FakeName,
            payload: (),
        };
        let _arg_mode: ComptimeArgMode = (rue_rir::RirArgMode::Normal, Span::new(0, 0));
        let _anonymous_kind = ComptimeAnonymousKind::Struct;
    }

    #[test]
    fn public_program_admission_contract_is_instantiable() {
        type Registry = ComptimeProgramRegistry<u8, u8, u8, u8>;
        type Memo = ComptimeCompletedCallMemo<u8, u8, u8, u8, u8>;
        let program_key = ComptimeProgramKey {
            declaration: 1,
            configuration: 2,
        };
        let mut registry = Registry::new();
        assert!(registry.get(&program_key).is_none());
        let mut editor = RirEditor::new();
        let body = editor.add_inst(Inst {
            data: InstData::IntConst(7),
            span: Span::new(0, 0),
        });
        let context = RirValidationContext {
            symbol_count: 0,
            source_lengths: &[(rue_span::FileId::DEFAULT, 1)],
        };
        let program = ComptimeProgram {
            rir: Arc::new(ValidatedRir::finish(editor, &context).unwrap()),
            symbols: Arc::from([]),
            imports: 0_u8,
        };
        registry.register(program_key.clone(), program).unwrap();
        assert_eq!(
            registry.get(&program_key).unwrap().rir.get(body).data,
            InstData::IntConst(7)
        );

        let call_key = ComptimeCallKey {
            declaration: 1,
            configuration: 2,
            type_arguments: std::sync::Arc::from([3]),
            value_arguments: std::sync::Arc::from([4]),
        };
        let mut memo = Memo::new();
        assert!(matches!(
            memo.lookup(&call_key),
            ComptimeCallMemoLookup::Miss
        ));
        memo.insert(call_key.clone(), ComptimeMemoizedOutcome::NotReady)
            .unwrap();
        assert!(matches!(
            memo.lookup(&call_key),
            ComptimeCallMemoLookup::Memoized(ComptimeMemoizedOutcome::NotReady)
        ));
        assert_eq!(registry.len(), 1);
        assert!(!registry.is_empty());
    }
}

const PRODUCTION_MODULES: &[(&str, &str)] = &[
    ("artifact_views", include_str!("artifact_views.rs")),
    ("backend", include_str!("backend.rs")),
    ("body_query", include_str!("body_query.rs")),
    ("bound_definitions", include_str!("bound_definitions.rs")),
    ("canonical_lower", include_str!("canonical_lower.rs")),
    ("canonical_merge", include_str!("canonical_merge.rs")),
    ("canonical_semantic", include_str!("canonical_semantic.rs")),
    ("cfg_query", include_str!("cfg_query.rs")),
    ("codegen_query", include_str!("codegen_query.rs")),
    ("content_digest", include_str!("content_digest.rs")),
    ("object_query", include_str!("object_query.rs")),
    (
        "declaration_candidate",
        include_str!("declaration_candidate.rs"),
    ),
    (
        "definition_snapshot",
        include_str!("definition_snapshot.rs"),
    ),
    (
        "dependency_envelope",
        include_str!("dependency_envelope.rs"),
    ),
    ("diagnostic", include_str!("diagnostic.rs")),
    (
        "diagnostic_attempt_store",
        include_str!("diagnostic_attempt_store.rs"),
    ),
    ("drop_glue", include_str!("drop_glue.rs")),
    ("durable_cfg", include_str!("durable_cfg.rs")),
    ("durable_comptime", include_str!("durable_comptime.rs")),
    ("durable_semantics", include_str!("durable_semantics.rs")),
    ("import_discovery", include_str!("import_discovery.rs")),
    ("import_graph", include_str!("import_graph.rs")),
    ("linking", include_str!("linking.rs")),
    (
        "local_semantic_materialization",
        include_str!("local_semantic_materialization.rs"),
    ),
    ("parsed_modules", include_str!("parsed_modules.rs")),
    ("program_image_plan", include_str!("program_image_plan.rs")),
    ("queries", include_str!("queries.rs")),
    (
        "revisioned_query_database",
        include_str!("revisioned_query_database.rs"),
    ),
    (
        "semantic_query_nucleus",
        include_str!("semantic_query_nucleus.rs"),
    ),
    ("semantic_symbols", include_str!("semantic_symbols.rs")),
    ("semantic_identity", include_str!("semantic_identity.rs")),
    ("session", include_str!("session.rs")),
    ("shared_segments", include_str!("shared_segments.rs")),
    ("source_identity", include_str!("source_identity.rs")),
    ("source_metadata", include_str!("source_metadata.rs")),
    ("source_snapshot", include_str!("source_snapshot.rs")),
    ("syntax", include_str!("syntax.rs")),
    (
        "toolchain_module_demand",
        include_str!("toolchain_module_demand.rs"),
    ),
    ("type_queries", include_str!("type_queries.rs")),
    ("typed_query_store", include_str!("typed_query_store.rs")),
    ("unstable", include_str!("unstable.rs")),
    ("well_known_option", include_str!("well_known_option.rs")),
];

#[test]
fn local_semantic_materialization_is_an_inert_exact_fact_boundary() {
    let local = include_str!("local_semantic_materialization.rs");

    for required in [
        "canonical: &crate::body_query::CanonicalBody",
        "declarations: &[DurableDeclarationSemantic]",
        "anonymous_nominals: &[DurableAnonymousNominal]",
        "callable_facts: &[LocalCallableFact]",
        "nominal_metadata: &[LocalNominalMetadataFact]",
        "pub(crate) fn new(",
        "pub(crate) fn identity(&self) -> &StableDefinitionKey",
        "pub(crate) fn lang_item(&self) -> Option<rue_air::LangItem>",
        "rue_air::SemanticImportEpoch::new_local_in_space(",
        ".materialize_local_body_with_types(",
        "FunctionInstanceKey::AnonymousMember",
        "materialize_canonical_body_with_indexes(",
        "materialize_semantic_body_with_indexes(",
    ] {
        assert!(
            local.contains(required),
            "local semantic boundary lost exact input or identity support: {required}"
        );
    }
    for forbidden in [
        "CanonicalSemanticOutput",
        "CanonicalMergedProgram",
        "SemaOutput",
        "QueryFamily<",
        "Mutex<",
        "RwLock<",
        "cache:",
        "materialize_canonical_body(",
        "materialize_semantic_body(",
    ] {
        assert!(
            !local.contains(forbidden),
            "local semantic boundary gained a peer authority or cache: {forbidden}"
        );
    }
}

#[test]
fn the_driver_reads_accessor_declaration_rules_from_the_shared_rule_module() {
    // RUE-1232: this producer walks its own reparsed AST, but the 6.6:3-6.6:7
    // rules it applies — which forms are illegal, in which order, and how each
    // diagnostic reads — are `rue_air::declaration_validation`'s, shared with
    // the RIR producers. Spelling one here is how the two declaration-time
    // producers drifted before.
    let driver = [
        include_str!("revisioned_query_database.rs"),
        include_str!("semantic_query_nucleus.rs"),
    ]
    .concat();
    for kind in [
        ["ErrorKind::Accessor", "RequiresBorrowSelf"].concat(),
        ["ErrorKind::Accessor", "ParamModeUnsupported"].concat(),
        ["ErrorKind::Accessor", "BodyMissingYield"].concat(),
        ["ErrorKind::Accessor", "BodyOtherExit"].concat(),
        ["ErrorKind::Accessor", "YieldNotReceiverRooted"].concat(),
    ] {
        assert!(
            !driver.contains(&kind),
            "the driver regained its own copy of an accessor declaration rule: {kind}"
        );
    }
    assert!(
        !driver.contains("\"a `-> borrow` accessor\""),
        "the driver regained its own 6.6:3 gate subject"
    );
    for required in [
        "use rue_air::declaration_validation as rules;",
        "rules::accessor_signature_for_mode(",
        "rules::accessor_body_error(",
        "accessor_method_link_error(",
    ] {
        assert!(
            driver.contains(required),
            "the driver stopped reading an accessor rule from the shared module: {required}"
        );
    }
}

#[test]
fn cfg_queries_own_local_semantic_materialization_and_terminal_domains() {
    let cfg = include_str!("cfg_query.rs");
    let domains = include_str!("durable_cfg.rs");
    let database = include_str!("revisioned_query_database.rs");
    for required in [
        "CfgSemanticInput::Body",
        "synthesize_canonical_drop_glue(",
        "CfgDomainProjection::from_local_body(",
        "pub(crate) type_pool: rue_air::FrozenTypeInternPool",
        "pub(crate) interner: Arc<lasso::ThreadedRodeo>",
        "pub(crate) local_atoms:",
        "&record.type_pool",
        "callable_by_symbol",
    ] {
        assert!(
            cfg.contains(required),
            "CFG ownership boundary lost: {required}"
        );
    }
    for required in [
        "crate::local_semantic_materialization::materialize_canonical_body_with_indexes_in_space(",
        "crate::local_semantic_materialization::materialize_semantic_body_with_indexes_in_space(",
    ] {
        let live_calls = cfg
            .lines()
            .filter(|line| line.trim_start().starts_with(required))
            .count();
        assert_eq!(
            live_calls, 1,
            "CFG query must have exactly one live indexed materialization call: {required}"
        );
    }
    for forbidden in ["materialize_canonical_body(", "materialize_semantic_body("] {
        assert!(
            !cfg.contains(forbidden),
            "CFG query regained a direct materialization adapter call: {forbidden}"
        );
    }
    for forbidden in [
        "struct CfgLiveInput",
        "pub(crate) live:",
        "key.live",
        "same_optimized_memo_domain",
        "optimized_memo_domain_hash",
    ] {
        assert!(
            !cfg.contains(forbidden),
            "CFG key regained request-local identity state: {forbidden}"
        );
    }
    assert!(
        !database.contains("live: Arc<crate::cfg_query::CfgLiveInput>"),
        "the registered request facade must not require caller-owned CFG live input"
    );
    assert!(domains.contains("for (_, current) in air.iter()"));
    assert!(domains.contains("stable_callable(*name)"));
    assert!(domains.contains("for (current, stable) in &materialization.materialized_types"));
    assert!(!cfg.contains(".find(|fact| fact.symbol.as_ref() == name)"));
    for forbidden in [
        "SemanticImportedBodyDomains",
        "from_body_parts(",
        "air.iter().zip(body.instructions.iter())",
        "air.places().iter().zip(body.places.iter())",
    ] {
        assert!(
            !domains.contains(forbidden),
            "CFG domains regained a second AIR/body reconstruction pass: {forbidden}"
        );
    }

    let production = cfg.rsplit_once("#[cfg(test)]").unwrap().0;
    let optimization = production
        .split_once("pub(crate) fn evaluate_optimized_cfg(")
        .unwrap()
        .1
        .split_once("fn optimize_cfg_without_accessors(")
        .unwrap()
        .0;
    assert!(
        optimization.contains("copy_interner_preserving_ordinals(&record.interner"),
        "accessor optimization must isolate the published CFG symbol universe"
    );
    for forbidden in [
        "InternerChargeRefresh",
        "refresh_interner_retained_charge",
        "let interner = record.interner.clone()",
        "record.interner.get_or_intern",
    ] {
        assert!(
            !production.contains(forbidden),
            "published CFG interners regained a mutation path: {forbidden}"
        );
    }
}

#[test]
fn codegen_queries_consume_only_registered_optimized_cfg_domains() {
    let cfg = include_str!("cfg_query.rs");
    let codegen = include_str!("codegen_query.rs");
    let database = include_str!("revisioned_query_database.rs");
    for required in [
        "pub(crate) codegen: Arc<CfgCodegenDomain>",
        "record.codegen.symbol_mappings",
        "&record.cfg",
        "&record.type_pool",
        "&record.strings",
        "&record.interner",
        "key.optimized_cfg.clone()",
        "pub(crate) function: crate::FunctionInstanceKey",
    ] {
        assert!(
            cfg.contains(required) || codegen.contains(required),
            "codegen ownership boundary lost: {required}"
        );
    }
    for forbidden in [
        "struct CodegenLiveInput",
        "pub(crate) live:",
        "key.live",
        "pub(crate) function: crate::FunctionWithCfg",
    ] {
        assert!(
            !codegen.contains(forbidden),
            "codegen key regained caller-owned live state: {forbidden}"
        );
    }
    assert!(
        !database.contains("function: crate::FunctionWithCfg")
            && !database.contains("type_pool: crate::FrozenTypeInternPool")
            && !database.contains("symbol_mappings: Arc<BTreeMap<String, String>>"),
        "the registered codegen request facade must not require semantic/global live inputs"
    );
    let session = include_str!("session.rs");
    assert!(
        session.contains("this adapter enumerates the")
            && session.contains("semantic functions only so focused tests can inspect units")
            && !session.contains("_foreign_symbols: &[String]"),
        "collection must retain only the test-only pre-object inspection adapter"
    );
}

#[test]
fn bounded_symbol_space_constructor_is_owner_only() {
    // rue-rir is a separate crate, so the revision owner cannot use a
    // crate-private constructor. The constructor is deliberately doc-hidden;
    // this inventory is the scoped-authority gate that keeps its only normal
    // build caller inside RevisionSymbolSpace, while the public test seam
    // remains CompilerSession::with_interner_limit (cfg(test) only).
    let database = include_str!("revisioned_query_database.rs");
    let production = database.rsplit_once("#[cfg(test)]").unwrap().0;
    assert_eq!(
        production
            .matches("next_generation_with_owner_bound")
            .count(),
        1,
        "only the revision owner may inject the test bound"
    );
}

const RUE_868_RAW_FACADE_VOCABULARY: &[&str] = &[
    "Lexer",
    "Token",
    "TokenKind",
    "Ast",
    "Rir",
    "ThreadedRodeo",
    "FrozenTypeInternPool",
    "TypeInternPool",
    "SemanticSymbol",
    "SemanticSymbolUniverse",
    "CanonicalRirOutput",
    "CanonicalSemanticOutput",
    "CanonicalMergedProgram",
    "ParsedProgram",
    "FunctionWithCfg",
    "Mir",
    "generate_emitted_asm",
    "generate_liveness_info",
    "generate_lowering_info",
    "generate_mir",
    "generate_regalloc_info",
    "generate_stack_frame_info",
    "LoweringDebugInfo",
    "RegAllocDebugInfo",
    "StackFrameInfo",
];

fn source_between_exact_boundaries<'a>(source: &'a str, start: &str, next: &str) -> &'a str {
    let start = source.find(start).expect("source boundary starts");
    let tail = &source[start..];
    let end = tail.find(next).expect("source boundary ends");
    &tail[..end]
}

#[test]
fn warning_body_projection_stays_parse_only_and_below_rir_body_analysis() {
    let parsed = include_str!("parsed_modules.rs");
    let projector = source_between_exact_boundaries(
        parsed,
        "struct ParsedBodyProjectionCollector<'a>",
        "\nfn validate_pair(",
    );
    for forbidden in ["lower_module_rir", "OwnedBodyInput", "ModuleRir"] {
        assert!(
            !projector.contains(forbidden),
            "warning syntax projection crossed into RIR/body lowering: {forbidden}"
        );
    }
    for required in [
        "fn collect_module_projections(",
        "fn visit_callable(",
        "fn visit_expr(",
        "ParsedDeclarationAstLocator::StructMethod",
    ] {
        assert!(
            projector.contains(required),
            "parser-owned warning projection lost its canonical syntax edge: {required}"
        );
    }

    let runtime = include_str!("revisioned_query_database.rs");
    for forbidden in [
        "compiler.warning-body-syntax",
        "WarningBodySyntaxQueryKey",
        "WarningStaticCallCollector",
        "fn warning_static_call_heads(",
    ] {
        assert!(
            !runtime.contains(forbidden),
            "warning reachability regained a peer syntax path: {forbidden}"
        );
    }
    let projection_family = source_between_exact_boundaries(
        runtime,
        "let parse_for_warning_call_heads = parse_modules.clone();",
        "        let classifications_for_warning_references =",
    );
    for required in [
        "compiler.warning-call-head-projection",
        "parse_for_warning_call_heads",
        ".declaration_warning_call_heads(candidate)",
    ] {
        assert!(
            projection_family.contains(required),
            "warning call-head terminal lost its thin parser projection: {required}"
        );
    }
    for forbidden in [".ast()", "rue_parser", "AstGen", "lower_module_rir"] {
        assert!(
            !projection_family.contains(forbidden),
            "warning call-head terminal regained syntax/lowering work: {forbidden}"
        );
    }
    let evaluator = source_between_exact_boundaries(
        runtime,
        "let classifications_for_warning_references =",
        "        // Body analysis is a canonical registered evaluator.",
    );
    for forbidden in [
        "raw_declaration_bodies",
        "declaration_signature_projections",
        "body_inputs",
        "body_transactions",
        "module_rirs",
        "lower_module_rir",
        ".ast()",
    ] {
        assert!(
            !evaluator.contains(forbidden),
            "warning query family crossed into a peer syntax/RIR/body path: {forbidden}"
        );
    }
    for required in [
        "call_heads_for_warning_references",
        "WarningCallHeadProjectionQueryKey",
        "DeclarationImportQueryKey",
        "CanonicalImportResolution::Resolved",
    ] {
        assert!(
            evaluator.contains(required),
            "warning query lost canonical parse/import dependency: {required}"
        );
    }
}

#[test]
fn exact_source_boundaries_ignore_braces_in_comments_and_strings() {
    let fixture = r#"
    fn selected() {
        let _ = "}";
        // }}} must not truncate the selected method
        observed();
    }

    fn next() {}
"#;
    let selected = source_between_exact_boundaries(fixture, "    fn selected()", "\n    fn next()");
    assert!(selected.contains("observed();"));
    assert!(!selected.contains("fn next"));
}

#[test]
fn semantic_signatures_preserve_parser_type_structure_without_a_text_grammar() {
    let projection_source = include_str!("semantic_query_nucleus.rs");
    let projection = source_between_exact_boundaries(
        projection_source,
        "pub(crate) fn project_semantic_signature(",
        "\n#[derive(Debug, Clone, PartialEq, Eq, Hash)]\npub(crate) struct SemanticQueryConfiguration",
    );
    for required in [
        "RirTypeSyntaxBuilder::default()",
        "push_parser_type",
        "declaration_ast(key)",
        "resolve_raw_symbol",
    ] {
        assert!(
            projection.contains(required),
            "signature projection lost its canonical parsed-structure edge: {required}"
        );
    }
    for forbidden in [
        "source_text",
        "parse_semantic_signature",
        "resolve_semantic_type_syntax",
        "render_type",
        "rue_lexer",
        "Lexer::",
        "Parser::",
        "split(",
        "trim(",
    ] {
        assert!(
            !projection.contains(forbidden),
            "signature projection regained a source-text grammar: {forbidden}"
        );
    }

    let runtime = include_str!("revisioned_query_database.rs");
    let provider = source_between_exact_boundaries(
        runtime,
        "impl rue_air::SemanticTypeSyntaxProvider<",
        "\nimpl ResolveSemanticSignatureError",
    );
    for forbidden in [
        "parse_type_call_syntax",
        "resolve_semantic_type_syntax(",
        "rue_lexer",
        "rue_parser",
        ".split(",
        ".split_once(",
        ".trim(",
    ] {
        assert!(
            !provider.contains(forbidden),
            "semantic nucleus provider regained a rendered-type grammar: {forbidden}"
        );
    }
    let resolver = source_between_exact_boundaries(
        runtime,
        "fn resolve_parsed_semantic_signature(",
        "\nimpl RevisionedQueryDatabase {",
    );
    assert!(resolver.contains("resolve_structured_semantic_type_syntax"));
    for forbidden in [
        "resolve_semantic_type_syntax(",
        "parse_type_call_syntax",
        "rue_lexer",
        "rue_parser",
        ".split(",
        ".split_once(",
        ".trim(",
        ".strip_prefix(",
        ".starts_with(",
    ] {
        assert!(
            !resolver.contains(forbidden),
            "semantic signature resolution regained a handwritten text grammar: {forbidden}"
        );
    }

    let projection_value = source_between_exact_boundaries(
        projection_source,
        "pub(crate) enum ParsedSemanticSignature",
        "\nimpl ParsedSemanticSignature",
    );
    for required in ["RirTypeSyntaxArena<Arc<str>>", "RirTypeSyntaxRef"] {
        assert!(
            projection_value.contains(required),
            "signature value lost dense type-syntax ownership: {required}"
        );
    }
    for forbidden in [
        "Span",
        "FileId",
        "ThreadedRodeo",
        "rue_parser",
        "source_text",
        "fragment",
    ] {
        assert!(
            !projection_value.contains(forbidden),
            "signature value retained parser/source provenance: {forbidden}"
        );
    }
}

#[test]
fn compiler_uses_air_synthetic_type_identity_policy() {
    for (name, source) in [
        (
            "local_semantic_materialization.rs",
            include_str!("local_semantic_materialization.rs"),
        ),
        (
            "revisioned_query_database.rs",
            include_str!("revisioned_query_database.rs"),
        ),
    ] {
        for peer in [".strip_prefix(\"Str(\")", ".starts_with(\"Str(\")"] {
            assert!(
                !source.contains(peer),
                "{name} regained handwritten synthetic-type identity policy: {peer}"
            );
        }
    }
}

#[test]
fn body_transaction_has_no_complete_declaration_candidate_map() {
    let runtime = include_str!("revisioned_query_database.rs");
    let method = source_between_exact_boundaries(
        runtime,
        "struct BodyTransactionEvaluator {",
        "\nimpl RevisionedQueryDatabase {",
    );
    assert!(
        !method.contains("declaration_candidates"),
        "body_transaction must not regain the coordinator's complete candidate map"
    );
    let compact = method
        .chars()
        .filter(|ch| !ch.is_whitespace())
        .collect::<String>();
    assert!(
        !compact.contains(
            "Arc<BTreeMap<crate::StableDefinitionKey,crate::declaration_candidate::DeclarationCandidateKey>>"
        ),
        "body_transaction must derive exact shell candidates from stable keys"
    );
    for required in [
        "&self.stable_declaration_classifications",
        "StableDeclarationClassificationQueryValue::Selected",
        "&self.declaration_shells",
        "DeclarationShellQueryValue::Available",
    ] {
        assert!(
            method.contains(required),
            "body_transaction lost candidate-set shell classification: {required}"
        );
    }
    for forbidden in [
        "declaration_occurrence_indexes",
        "DeclarationOccurrenceIndex",
        "stable_syntax_candidate_set",
    ] {
        assert!(
            !method.contains(forbidden),
            "body_transaction bypassed the narrow stable classifier: {forbidden}"
        );
    }
}

#[test]
fn anonymous_body_transaction_has_one_candidate_artifact_path_and_no_frontend_reentry() {
    let runtime = include_str!("revisioned_query_database.rs");
    let transaction = source_between_exact_boundaries(
        runtime,
        "struct BodyTransactionEvaluator {",
        "\nimpl RevisionedQueryDatabase {",
    );
    for required in [
        "resolve_producer_artifact",
        "materialize_body_rir_bundle_with_declaration",
        "analyze_provider_anonymous_body",
    ] {
        assert!(
            transaction.contains(required),
            "anonymous transaction lost its canonical candidate path: {required}",
        );
    }
    for forbidden in [
        "lower_anonymous_member_body_input",
        "AnonymousMemberLowering",
        "parse_source_snapshot_module",
        "SourceSnapshot",
        "AstGen",
        "lower_parsed_declaration_body_plan_with_anonymous_anchors",
    ] {
        assert!(
            !transaction.contains(forbidden),
            "anonymous transaction regained a frontend/rematerialization path: {forbidden}",
        );
    }

    let production = runtime.split("\n#[cfg(test)]\nmod tests {").next().unwrap();
    for removed in [
        "lower_anonymous_member_body_input",
        "AnonymousMemberLowering",
        "DurableAnonymousMemberBodySyntax",
        "RawDeclarationSignatureSyntax",
        "RawAccessorSignatureSyntax",
    ] {
        assert!(
            !production.contains(removed),
            "deleted anonymous/signature carrier returned to production: {removed}",
        );
    }
}

#[test]
fn well_known_option_resolution_stays_per_body_exact_and_fail_closed() {
    let runtime = include_str!("revisioned_query_database.rs");
    let method = source_between_exact_boundaries(
        runtime,
        "struct BodyTransactionEvaluator {",
        "\nimpl RevisionedQueryDatabase {",
    );
    for required in [
        "&self.body_toolchain_demands",
        "exact_option_prerequisites(",
        "exact_option_query(",
        "WellKnownOptionResolutionFailure::Incomplete",
        "WellKnownOptionResolutionFailure::Semantic",
        "WellKnownOptionResolutionFailure::WrongProjection",
    ] {
        assert!(
            method.contains(required),
            "body_transaction lost exact atomic Option resolution: {required}"
        );
    }
    for forbidden in [
        "well_known_demands",
        "plan_well_known_option_demands",
        "WellKnownOptionDemand",
    ] {
        assert!(
            !method.contains(forbidden),
            "body_transaction regained request-global Option planning: {forbidden}"
        );
    }

    let exact_keys = include_str!("well_known_option.rs");
    for forbidden in [
        "CanonicalRirOutput",
        "CanonicalMergedProgram",
        "plan_well_known_option_demands",
        "WellKnownOptionDemand",
    ] {
        assert!(
            !exact_keys.contains(forbidden),
            "exact Option-key derivation regained whole-request input: {forbidden}"
        );
    }
}

const RUE_869_INTERNAL_ROOT_VOCABULARY: &[&str] = &[
    "BoundDefinitionId",
    "BoundDefinitionRecord",
    "BoundDefinitionSet",
    "DefinitionId",
    "DefinitionKind",
    "DefinitionNameKey",
    "DefinitionNamespace",
    "DefinitionOccurrenceId",
    "DefinitionRecord",
    "DefinitionShard",
    "DefinitionSnapshot",
    "DependencyAcceptedRead",
    "DependencyContext",
    "DependencyObservation",
    "DependencyObservationOutcome",
    "DependencyRequest",
    "DiscoverySourceAssembler",
    "IMPORT_DISCOVERY_POLICY_VERSION",
    "DiagnosticFormatter",
    "JsonDiagnostic",
    "JsonDiagnosticFormatter",
    "MultiFileFormatter",
    "MultiFileJsonFormatter",
    "CompileError",
    "CompileResult",
    "Diagnostic",
    "ErrorCode",
    "ErrorKind",
    "Suggestion",
    "WarningKind",
    "Span",
];

fn code_identifiers(source: &str) -> Vec<&str> {
    source
        .lines()
        .map(|line| line.split_once("//").map_or(line, |(code, _)| code))
        .flat_map(|line| {
            line.split(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '_'))
                .filter(|token| !token.is_empty())
        })
        .collect()
}

fn public_compile_functions(source: &str) -> Vec<String> {
    let mut names = Vec::new();
    for line in source.lines() {
        let code = line.split_once("//").map_or(line, |(code, _)| code).trim();
        if code.starts_with("pub(") {
            continue;
        }
        let identifiers = code
            .split(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '_'))
            .filter(|token| !token.is_empty())
            .collect::<Vec<_>>();
        if identifiers.first() != Some(&"pub") {
            continue;
        }
        let Some(fn_index) = identifiers.iter().position(|token| *token == "fn") else {
            continue;
        };
        if let Some(name) = identifiers
            .get(fn_index + 1)
            .filter(|name| name.starts_with("compile_"))
        {
            names.push((*name).to_owned());
        }
    }
    names
}

const RUE_866_INTERNAL_VOCABULARY: &[&str] = &[
    "CompilerSessionWork",
    "FrontendQueryWork",
    "FrontendRetentionMetrics",
    "FRONTEND_DIAGNOSTIC_RETENTION_LIMIT",
    "FRONTEND_INVALIDATION_PLAN_RETENTION_LIMIT",
    "DefinitionQueryRecord",
    "SemanticQueryRecord",
    "ImportDiagnosticInputDescriptor",
    "ImportGraphInputDescriptor",
    "ImportDiscoveryRevisionArtifact",
    "ImportDiscoveryRevisionStatus",
    "FrontendDiagnosticIdentity",
    "ResolvedCodegenRevision",
    "ResolvedLinkRevision",
    "ResolvedProgramRevision",
    "CodegenInputDescriptor",
    "LinkInputDescriptor",
    "SemanticInputDescriptor",
    "ModuleResolutionInput",
    "ModuleResolutionInputs",
    "SourceStore",
    "StableLinkerInput",
    "StableOptLevel",
    "StablePreviewFeatures",
    "ParseInvalidationSummary",
    "ParsedModulesWork",
    "CanonicalMergeWork",
    "CanonicalRirWork",
    "CanonicalSemanticFailurePhase",
    "CanonicalSemanticFailureWork",
    "CanonicalSemanticWork",
    "PipelineWork",
    "SourceStats",
];

const RUE_867_DURABLE_VOCABULARY: &[&str] = &[
    "DurableBodyWork",
    "DurableConstValue",
    "DurableDeclarationPayload",
    "DurableDeclarationSemantic",
    "DurableSemanticProjectionFailure",
    "DurableSemanticProjectionWork",
    "DurableType",
];

fn public_declarations(source: &str) -> Vec<String> {
    let mut declarations = Vec::new();
    let mut lines = source.lines();
    let mut api_attributes = Vec::new();
    while let Some(line) = lines.next() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("#[") {
            if trimmed.starts_with("#[cfg(") || trimmed.starts_with("#[cfg_attr(") {
                api_attributes.push(trimmed.to_owned());
            }
            continue;
        }
        if trimmed.is_empty() || trimmed.starts_with("///") || trimmed.starts_with("//!") {
            continue;
        }
        if !trimmed.starts_with("pub ") || trimmed.starts_with("pub(crate)") {
            api_attributes.clear();
            continue;
        }

        let identifiers = code_identifiers(trimmed);
        let owns_body = identifiers
            .iter()
            .any(|identifier| matches!(*identifier, "struct" | "enum" | "union" | "trait"));
        let field_owned_body = identifiers
            .iter()
            .any(|identifier| matches!(*identifier, "struct" | "union"));
        let field = !identifiers.iter().any(|identifier| {
            matches!(
                *identifier,
                "fn" | "struct"
                    | "enum"
                    | "union"
                    | "trait"
                    | "type"
                    | "use"
                    | "const"
                    | "static"
                    | "mod"
            )
        });
        let mut declaration = api_attributes.join("\n");
        api_attributes.clear();
        if !declaration.is_empty() {
            declaration.push('\n');
        }
        let mut brace_depth = 0isize;
        let mut opened_body = false;
        let mut current = Some(line);
        loop {
            let declaration_line = current.take().expect("public declaration has a line");
            declaration.push_str(declaration_line);
            declaration.push('\n');
            for byte in declaration_line.bytes() {
                match byte {
                    b'{' => {
                        opened_body = true;
                        brace_depth += 1;
                    }
                    b'}' if opened_body => brace_depth -= 1,
                    _ => {}
                }
            }

            let complete = if owns_body {
                (opened_body && brace_depth == 0)
                    || (!opened_body && declaration_line.contains(';'))
            } else if field {
                declaration_line.contains(',')
            } else {
                declaration_line.contains(';') || declaration_line.contains('{')
            };
            if complete {
                break;
            }
            current = lines.next();
            if current.is_none() {
                break;
            }
        }
        if field_owned_body && opened_body {
            let open = declaration.find('{').expect("opened aggregate has a body");
            let close = declaration
                .rfind('}')
                .expect("balanced aggregate has a body");
            let mut public_shape = declaration[..=open].to_owned();
            public_shape.push_str(&public_declarations(&declaration[open + 1..close]).concat());
            public_shape.push('}');
            declarations.push(public_shape);
        } else {
            declarations.push(declaration);
        }
    }
    declarations
}

fn public_signatures(source: &str) -> String {
    public_declarations(source).concat()
}

fn public_declaration_name(declaration: &str) -> Option<&str> {
    let identifiers = code_identifiers(declaration);
    let keyword = identifiers.iter().position(|identifier| {
        matches!(
            *identifier,
            "fn" | "struct" | "enum" | "union" | "trait" | "type" | "const" | "static" | "mod"
        )
    })?;
    identifiers.get(keyword + 1).copied()
}

fn impl_blocks(source: &str) -> Vec<&str> {
    let mut blocks = Vec::new();
    let mut offset = 0;
    for line in source.split_inclusive('\n') {
        let trimmed = line.trim_start();
        if trimmed.starts_with("impl ") || trimmed.starts_with("impl<") {
            let leading = line.len() - trimmed.len();
            let start = offset + leading;
            let rest = &source[start..];
            let Some(open) = rest.find('{') else {
                offset += line.len();
                continue;
            };
            let mut depth = 0usize;
            for (index, byte) in rest[open..].bytes().enumerate() {
                match byte {
                    b'{' => depth += 1,
                    b'}' => {
                        depth -= 1;
                        if depth == 0 {
                            blocks.push(&rest[..open + index + 1]);
                            break;
                        }
                    }
                    _ => {}
                }
            }
        }
        offset += line.len();
    }
    blocks
}

fn macro_blocks(source: &str) -> Vec<&str> {
    let mut blocks = Vec::new();
    let mut offset = 0;
    for line in source.split_inclusive('\n') {
        let trimmed = line.trim_start();
        if trimmed.starts_with("macro_rules!") || trimmed.starts_with("pub macro ") {
            let leading = line.len() - trimmed.len();
            let start = offset + leading;
            let rest = &source[start..];
            let Some(open) = rest.find('{') else {
                offset += line.len();
                continue;
            };
            let mut depth = 0usize;
            for (index, byte) in rest[open..].bytes().enumerate() {
                match byte {
                    b'{' => depth += 1,
                    b'}' => {
                        depth -= 1;
                        if depth == 0 {
                            blocks.push(&rest[..open + index + 1]);
                            break;
                        }
                    }
                    _ => {}
                }
            }
        }
        offset += line.len();
    }
    blocks
}

fn supported_public_surface(facade: &str, modules: &[(&str, &str)]) -> String {
    let root_declarations = public_declarations(facade);
    let public_module_names = root_declarations
        .iter()
        .filter_map(|declaration| {
            declaration
                .trim_start()
                .strip_prefix("pub mod ")
                .and_then(|module| module.split(';').next())
        })
        .collect::<std::collections::BTreeSet<_>>();
    let public_uses = public_use_declarations(facade);
    let exported_identifiers = public_uses
        .iter()
        .flat_map(|declaration| code_identifiers(declaration))
        .filter(|identifier| {
            !matches!(
                *identifier,
                "pub" | "use" | "self" | "crate" | "super" | "as"
            )
        })
        .map(str::to_owned)
        .collect::<std::collections::BTreeSet<_>>();
    let mut surface = root_declarations.concat();

    for (module, source) in modules {
        if public_module_names.contains(module) {
            surface.push_str(&public_signatures(source));
        }
        for declaration in public_declarations(source) {
            if public_declaration_name(&declaration)
                .is_some_and(|name| exported_identifiers.contains(name))
            {
                surface.push_str(&declaration);
            }
        }
        for implementation in impl_blocks(source) {
            if code_identifiers(implementation)
                .into_iter()
                .take_while(|identifier| *identifier != "pub")
                .any(|identifier| exported_identifiers.contains(identifier))
            {
                surface.push_str(&public_signatures(implementation));
            }
        }
    }
    surface
}

fn public_use_declarations(source: &str) -> Vec<String> {
    let mut declarations = Vec::new();
    let mut declaration = String::new();
    for line in source.lines() {
        let code = line.split_once("//").map_or(line, |(code, _)| code);
        if declaration.is_empty() && !code.trim_start().starts_with("pub use ") {
            continue;
        }
        declaration.push_str(code);
        declaration.push('\n');
        if code.contains(';') {
            declarations.push(std::mem::take(&mut declaration));
        }
    }
    declarations
}

fn macro_invocation_path(line: &str) -> Option<&str> {
    let (path, _) = line.split_once('!')?;
    let path = path.trim();
    if path == "macro_rules" || path.is_empty() {
        return None;
    }
    path.chars()
        .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == ':')
        .then_some(path)
}

fn unsupported_api_layout(source: &str, root: bool) -> Option<String> {
    let mut conditional_export = false;
    let mut test_only_condition = false;
    for line in source.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("#[cfg(") || trimmed.starts_with("#[cfg_attr(") {
            if !trimmed.ends_with(']') {
                return Some(trimmed.to_owned());
            }
            test_only_condition = !conditional_export && trimmed == "#[cfg(test)]";
            conditional_export = true;
            continue;
        }
        if trimmed.starts_with("#[macro_export") {
            return Some(trimmed.to_owned());
        }
        if trimmed.starts_with("#[") {
            continue;
        }
        if trimmed.is_empty() || trimmed.starts_with("///") || trimmed.starts_with("//!") {
            continue;
        }
        if trimmed == "pub"
            || trimmed.starts_with("pub\t")
            || trimmed.starts_with("pub/*")
            || trimmed == "impl"
            || trimmed.starts_with("impl\t")
            || trimmed.starts_with("impl/*")
        {
            return Some(trimmed.to_owned());
        }
        if trimmed.starts_with("include!(") || trimmed.starts_with("pub macro ") {
            return Some(trimmed.to_owned());
        }
        if line.len() == trimmed.len()
            && let Some(path) = macro_invocation_path(trimmed)
        {
            let name = path.rsplit("::").next().unwrap_or(path);
            if root
                || !matches!(
                    name,
                    "thread_local" | "session_query_metrics_family" | "query_value_charge"
                )
            {
                return Some(trimmed.to_owned());
            }
        }
        if root && conditional_export && trimmed.starts_with("pub use ") {
            return Some(trimmed.to_owned());
        }
        let identifiers = code_identifiers(trimmed);
        if conditional_export
            && !test_only_condition
            && (trimmed.starts_with("impl ") || trimmed.starts_with("impl<"))
            && (identifiers.contains(&"CompilerSession")
                || identifiers.contains(&"CompilerSessionUpdate"))
        {
            return Some(trimmed.to_owned());
        }
        conditional_export = false;
        test_only_condition = false;
    }
    for block in macro_blocks(source) {
        if root || code_identifiers(block).contains(&"pub") {
            return Some(block.lines().next().unwrap_or("macro_rules!").to_owned());
        }
    }
    for implementation in impl_blocks(source) {
        if impl_owner(implementation).is_none() {
            continue;
        }
        for line in implementation.lines().skip(1) {
            let Some(direct) = line.strip_prefix("    ") else {
                continue;
            };
            if direct.chars().next().is_some_and(char::is_whitespace) {
                continue;
            }
            if macro_invocation_path(direct).is_some() {
                return Some(direct.to_owned());
            }
        }
    }
    None
}

fn assert_no_public_use_globs(source: &str) {
    for declaration in public_use_declarations(source) {
        assert!(
            !public_use_is_glob(&declaration),
            "public glob reexports bypass the supported facade inventory: {declaration}"
        );
    }
}

fn public_use_is_glob(declaration: &str) -> bool {
    declaration
        .split(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '_' || ch == '*'))
        .filter(|token| !token.is_empty())
        .any(|token| token == "*")
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct ApiInventoryEntry {
    stability: String,
    owner: String,
    class: String,
    consumer: String,
    symbol: String,
    signature: String,
}

impl ApiInventoryEntry {
    fn new(
        stability: &str,
        owner: impl Into<String>,
        class: &str,
        consumer: &str,
        symbol: impl Into<String>,
        signature: impl Into<String>,
    ) -> Self {
        Self {
            stability: stability.to_owned(),
            owner: owner.into(),
            class: class.to_owned(),
            consumer: consumer.to_owned(),
            symbol: symbol.into(),
            signature: signature.into(),
        }
    }

    fn render(&self) -> String {
        for field in [
            &self.stability,
            &self.owner,
            &self.class,
            &self.consumer,
            &self.symbol,
            &self.signature,
        ] {
            assert!(
                !field.contains('|') && !field.contains('\n'),
                "semantic API inventory fields must remain one-line and pipe-free: {field:?}"
            );
        }
        format!(
            "{}|{}|{}|{}|{}|{}",
            self.stability, self.owner, self.class, self.consumer, self.symbol, self.signature
        )
    }
}

fn canonical_signature(declaration: &str) -> String {
    let declaration = declaration
        .lines()
        .map(|line| line.split_once("//").map_or(line, |(code, _)| code))
        .collect::<String>();
    let end = declaration
        .find('{')
        .or_else(|| declaration.find(';'))
        .unwrap_or(declaration.len());
    let source = &declaration[..end];
    let bytes = source.as_bytes();
    let mut tokens = Vec::<(String, bool)>::new();
    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        if byte.is_ascii_whitespace() {
            index += 1;
            continue;
        }
        if byte.is_ascii_alphanumeric() || byte == b'_' {
            let start = index;
            index += 1;
            while index < bytes.len()
                && (bytes[index].is_ascii_alphanumeric() || bytes[index] == b'_')
            {
                index += 1;
            }
            tokens.push((source[start..index].to_owned(), true));
            continue;
        }
        if byte == b'\'' && index + 1 < bytes.len() && bytes[index + 1].is_ascii_alphabetic() {
            let start = index;
            index += 2;
            while index < bytes.len()
                && (bytes[index].is_ascii_alphanumeric() || bytes[index] == b'_')
            {
                index += 1;
            }
            tokens.push((source[start..index].to_owned(), true));
            continue;
        }
        let two = (index + 1 < bytes.len()).then(|| &source[index..index + 2]);
        if matches!(
            two,
            Some("->" | "::" | "=>" | ".." | "<=" | ">=" | "==" | "!=")
        ) {
            tokens.push((two.unwrap().to_owned(), false));
            index += 2;
        } else {
            tokens.push((source[index..index + 1].to_owned(), false));
            index += 1;
        }
    }

    let mut rendered = String::new();
    let mut previous_word = false;
    for (token, word) in tokens {
        if previous_word && word {
            rendered.push(' ');
        }
        rendered.push_str(&token);
        previous_word = word;
    }
    rendered
}

fn root_export_metadata(owner: &str, symbol: &str) -> (&'static str, &'static str) {
    match owner {
        "artifact_views" => ("artifact-view", "embedders+tooling"),
        "dependency_envelope" => match symbol {
            "DependencyEnvelope"
            | "DependencyEnvelopeStatus"
            | "DependencyResolutionOutcome"
            | "DependencyTopology"
            | "DependencyTopologyRecord" => ("dependency-artifact", "source-loaders+embedders"),
            _ => panic!("unclassified dependency facade export: {symbol}"),
        },
        "import_discovery" => match symbol {
            "AcceptedReadManifest"
            | "AcceptedReadManifestEntry"
            | "FileMetadataFingerprint"
            | "ImportCandidateRole"
            | "ImportDiscoveryContext"
            | "ImportOccurrenceKey"
            | "PhysicalFileIdentity" => ("dependency-artifact", "source-loaders+embedders"),
            // The host discovery-protocol records (AcceptedImportSource,
            // ImportDiscoveryPlan/Request, ImportObservation*) live under
            // `unstable` with the protocol functions that consume them. Any
            // return to the stable root is unclassified and fails here.
            _ => panic!("unclassified import-discovery facade export: {symbol}"),
        },
        "import_graph" => match symbol {
            "CanonicalImportCycle"
            | "CanonicalImportGraph"
            | "CanonicalImportGraphProblem"
            | "CanonicalImportGraphValidation"
            | "CanonicalImportRecord"
            | "CanonicalImportResolution"
            | "ImportDirective"
            | "ImportDirectives" => ("dependency-artifact", "source-loaders+embedders"),
            _ => panic!("unclassified import-graph facade export: {symbol}"),
        },
        "diagnostic_attempt_store" => ("diagnostic", "cli+embedders"),
        "rue_error" => match symbol {
            "CompileErrors" | "CompileWarning" | "MultiErrorResult" | "PreviewFeature"
            | "PreviewFeatures" | "VERSION" => ("diagnostic", "cli+embedders"),
            _ => panic!("raw diagnostic type returned to the stable facade: {symbol}"),
        },
        "queries" => match symbol {
            "CompileOptions" | "LinkerMode" => ("compilation-config", "cli+embedders"),
            "compile_snapshot" => ("one-shot-operation", "cli+embedders"),
            "CompileOutput" | "SourceView" => ("compile-artifact", "cli+embedders"),
            _ => panic!("unclassified queries facade export: {symbol}"),
        },
        "session" => match symbol {
            "CompilerSession" | "CompilerSessionUpdate" => ("session-owner", "embedders"),
            "CanonicalImportGraphOutput" => ("dependency-artifact", "source-loaders+embedders"),
            _ => panic!("unclassified session facade export: {symbol}"),
        },
        "source_identity" | "source_metadata" | "source_snapshot" | "rue_span" => {
            ("source-input", "cli+embedders")
        }
        "toolchain_module_demand" => match symbol {
            "OPTION_MODULE_LOGICAL_PATH"
            | "ParkedToolchainModules"
            | "STRBUF_MODULE_LOGICAL_PATH"
            | "TrustedToolchainModuleDemand" => {
                ("toolchain-module-demand", "source-loaders+embedders")
            }
            _ => panic!("unclassified toolchain-module-demand facade export: {symbol}"),
        },
        "rue_cfg" | "rue_target" => ("compilation-config", "cli+embedders"),
        _ => panic!("unclassified facade export owner: {owner}::{symbol}"),
    }
}

fn root_use_exports(facade: &str) -> Vec<ApiInventoryEntry> {
    let mut entries = Vec::new();
    for declaration in public_use_declarations(facade) {
        assert!(
            !public_use_is_glob(&declaration),
            "public glob reexports bypass the semantic API inventory: {declaration}"
        );
        assert!(
            !code_identifiers(&declaration).contains(&"as"),
            "public aliases require explicit semantic inventory support: {declaration}"
        );
        let compact = declaration
            .chars()
            .filter(|ch| !ch.is_whitespace())
            .collect::<String>();
        let path = compact
            .strip_prefix("pubuse")
            .and_then(|path| path.strip_suffix(';'))
            .expect("public-use scanner returns complete declarations");
        let (owner, symbols) = if let Some((owner, symbols)) = path.split_once("::{") {
            let symbols = symbols
                .strip_suffix('}')
                .expect("grouped public use closes its brace")
                .split(',')
                .filter(|symbol| !symbol.is_empty())
                .collect::<Vec<_>>();
            (owner, symbols)
        } else {
            let (owner, symbol) = path
                .rsplit_once("::")
                .expect("public reexport has an owning path");
            (owner, vec![symbol])
        };
        for symbol in symbols {
            let (class, consumer) = root_export_metadata(owner, symbol);
            entries.push(ApiInventoryEntry::new(
                "stable",
                owner,
                class,
                consumer,
                symbol,
                format!("pub use {owner}::{symbol}"),
            ));
        }
    }
    entries
}

fn impl_owner(implementation: &str) -> Option<&'static str> {
    let header = implementation.split_once('{')?.0;
    let identifiers = code_identifiers(header);
    if identifiers.contains(&"CompilerSessionUpdate") {
        Some("CompilerSessionUpdate")
    } else if identifiers.contains(&"CompilerSession") {
        Some("CompilerSession")
    } else {
        None
    }
}

fn session_method_metadata(
    owner: &str,
    module: &str,
    symbol: &str,
    _signature: &str,
) -> (&'static str, &'static str, &'static str) {
    let unstable = module == "unstable" || symbol.starts_with("unstable_");
    if unstable {
        return ("unstable", "debug-tooling", "in-tree-tooling");
    }
    let class = if owner == "CompilerSessionUpdate" {
        match symbol {
            "result" | "into_result" | "diagnostics" => "artifact-result",
            _ => panic!("unclassified stable CompilerSessionUpdate method: {symbol}"),
        }
    } else {
        match symbol {
            "new" | "update" => "session-operation",
            "published" | "committed_import_graph" | "import_diagnostics" | "rir" | "semantic" => {
                "artifact-query"
            }
            _ => panic!("unclassified stable CompilerSession method: {symbol}"),
        }
    };
    ("stable", class, "embedders")
}

fn semantic_api_inventory(facade: &str, modules: &[(&str, &str)]) -> Vec<ApiInventoryEntry> {
    assert!(
        unsupported_api_layout(facade, true).is_none(),
        "root API uses a visibility, impl, include, or macro form the exact inventory does not parse"
    );
    for (module, source) in modules {
        assert!(
            unsupported_api_layout(source, false).is_none(),
            "{module} uses a split visibility or impl form the exact inventory does not parse"
        );
    }
    let mut entries = root_use_exports(facade);
    for declaration in public_declarations(facade) {
        let signature = canonical_signature(&declaration);
        if signature.starts_with("pub use ") || signature.starts_with("pub use") {
            continue;
        }
        let symbol = public_declaration_name(&declaration)
            .expect("every direct root declaration has a name");
        if signature == "pub mod unstable" {
            entries.push(ApiInventoryEntry::new(
                "unstable",
                "unstable",
                "debug-module",
                "in-tree-tooling",
                symbol,
                signature,
            ));
        } else if symbol == "configure_thread_pool" {
            entries.push(ApiInventoryEntry::new(
                "stable",
                "lib",
                "runtime-config",
                "cli+embedders",
                symbol,
                signature,
            ));
        } else {
            panic!("unclassified direct root public declaration: {signature}");
        }
    }

    for (module, source) in modules {
        for implementation in impl_blocks(source) {
            let Some(owner) = impl_owner(implementation) else {
                continue;
            };
            for declaration in public_declarations(implementation) {
                let symbol = public_declaration_name(&declaration)
                    .expect("every public session declaration has a name");
                let signature = canonical_signature(&declaration);
                let (stability, class, consumer) =
                    session_method_metadata(owner, module, symbol, &signature);
                entries.push(ApiInventoryEntry::new(
                    stability, owner, class, consumer, symbol, signature,
                ));
            }
        }
    }

    entries.sort();
    let mut rendered = std::collections::BTreeSet::new();
    for entry in &entries {
        assert!(
            rendered.insert(entry.render()),
            "duplicate semantic API inventory entry: {}",
            entry.render()
        );
    }
    entries
}

fn render_semantic_api_inventory(facade: &str, modules: &[(&str, &str)]) -> String {
    semantic_api_inventory(facade, modules)
        .into_iter()
        .map(|entry| entry.render())
        .collect::<Vec<_>>()
        .join("\n")
}

fn inherent_impl<'a>(source: &'a str, owner: &str) -> &'a str {
    let marker = format!("impl {owner} {{");
    let start = source.find(&marker).expect("reviewed owner impl exists");
    let rest = &source[start..];
    let mut depth = 0usize;
    for (offset, byte) in rest.bytes().enumerate() {
        match byte {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return &rest[..=offset];
                }
            }
            _ => {}
        }
    }
    panic!("reviewed owner impl is balanced")
}

#[test]
fn facade_stays_small_and_session_centered() {
    let facade = include_str!("lib.rs");
    let mut declared_modules = facade
        .lines()
        .filter_map(|line| {
            line.trim()
                .strip_prefix("mod ")
                .or_else(|| line.trim().strip_prefix("pub mod "))
        })
        .filter_map(|module| module.strip_suffix(';'))
        .collect::<Vec<_>>();
    declared_modules.sort_unstable();
    let mut inventoried_modules = PRODUCTION_MODULES
        .iter()
        .map(|(name, _)| *name)
        .chain([
            "api_inventory",
            "integration_tests",
            "pipeline_tests",
            "producer_nominal_acceptance_tests",
            "retained_charge",
            "scaling_harness",
            "supported_api_inventory",
            "test_support",
        ])
        .collect::<Vec<_>>();
    inventoried_modules.sort_unstable();
    assert_eq!(
        declared_modules, inventoried_modules,
        "every new module must be classified and added to the API inventory"
    );

    let facade_compile_names = code_identifiers(facade)
        .into_iter()
        .filter(|name| name.starts_with("compile_"))
        .collect::<Vec<_>>();
    assert_eq!(
        facade_compile_names,
        ["compile_snapshot"],
        "the facade may reexport exactly one one-shot compilation adapter"
    );

    let public_compilers = PRODUCTION_MODULES
        .iter()
        .flat_map(|(module, source)| {
            public_compile_functions(source)
                .into_iter()
                .map(move |function| (*module, function))
        })
        .collect::<Vec<_>>();
    assert_eq!(
        public_compilers,
        [("queries", "compile_snapshot".to_owned())],
        "production modules may define exactly one public compile function"
    );
}

#[test]
fn per_body_query_boundary_is_stable_independent_and_cache_free() {
    fn item<'a>(source: &'a str, marker: &str) -> &'a str {
        let start = source
            .find(marker)
            .expect("reviewed body query item exists");
        let rest = &source[start..];
        let open = rest.find('{').expect("reviewed item has a body");
        let mut depth = 0usize;
        for (offset, byte) in rest[open..].bytes().enumerate() {
            match byte {
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        return &rest[..open + offset + 1];
                    }
                }
                _ => {}
            }
        }
        panic!("reviewed body query item is balanced")
    }
    let body = include_str!("body_query.rs");
    let key = item(body, "pub(crate) struct BodyQueryKey");
    assert!(key.contains("instance: crate::FunctionInstanceKey"));
    assert!(
        key.contains("configuration: crate::semantic_query_nucleus::SemanticQueryConfiguration")
    );
    for forbidden in [
        "Revision",
        "fingerprint",
        "Span",
        "BodyOwnerToken",
        "TypeId",
    ] {
        assert!(
            !key.contains(forbidden),
            "BodyQueryKey retained unstable identity component {forbidden}"
        );
    }

    let transaction = item(body, "pub(crate) enum BodyTransaction {");
    assert!(transaction.contains("Success"));
    assert!(transaction.contains("DeterministicFailure"));
    assert!(!transaction.contains("Canceled"));
    assert!(!transaction.contains("NonTerminal"));

    let runtime = include_str!("revisioned_query_database.rs");
    assert!(runtime.contains("compiler.body-transaction"));
    for redundant_projection in [
        "\"compiler.body-references\"",
        "\"compiler.canonical-body\"",
    ] {
        assert!(
            !runtime.contains(redundant_projection),
            "rooted consumers must retain the already-observed BodyTransaction directly: {redundant_projection}"
        );
    }
    let session = include_str!("session.rs");
    assert!(!session.contains("pub fn semantic("));
    assert!(!session.contains("semantic_view_from_rooted"));
    let body_transaction = source_between_exact_boundaries(
        runtime,
        "struct BodyTransactionEvaluator {",
        "\nimpl RevisionedQueryDatabase {",
    );
    assert!(body_transaction.contains("analyze_provider_ordinary_body"));
    assert!(!body_transaction.contains("analyze_body_query"));
    assert!(!body_transaction.contains("SharedDeclarationBase"));
    assert!(!body_transaction.contains("BoundBodyEpoch"));
}

#[test]
fn semantic_api_inventory_matches_every_root_export_and_session_signature() {
    let actual = render_semantic_api_inventory(include_str!("lib.rs"), PRODUCTION_MODULES);
    let approved = crate::supported_api_inventory::APPROVED.trim();
    assert_eq!(
        actual, approved,
        "the public compiler facade changed without a reviewed semantic inventory diff; \
         classify the owner, stability, category, and approved consumer in \
         supported_api_inventory.rs\n\nactual inventory:\n{actual}"
    );
}

#[test]
fn stable_facade_carries_no_compatibility_boundary_classifications() {
    // RUE-1479 removed the import-discovery compatibility facade. The
    // classifiers no longer produce these classes, so any reintroduction —
    // through a classifier arm or an approved-inventory line — fails here.
    let actual = render_semantic_api_inventory(include_str!("lib.rs"), PRODUCTION_MODULES);
    for inventory in [actual.as_str(), crate::supported_api_inventory::APPROVED] {
        for forbidden in ["compatibility-boundary", "legacy-embedders"] {
            assert!(
                !inventory.contains(forbidden),
                "a stable compatibility classification returned to the facade inventory: {forbidden}"
            );
        }
    }
}

#[test]
fn semantic_inventory_detects_root_and_signature_changes() {
    let facade = "pub use queries::CompileOptions;";
    let expanded = "pub use queries::{CompileOptions, LinkerMode};";
    assert_eq!(root_use_exports(facade).len(), 1);
    assert_eq!(root_use_exports(expanded).len(), 2);

    let original = render_semantic_api_inventory(
        "pub use session::CompilerSession;",
        &[(
            "session",
            "impl CompilerSession {\n    pub fn new() -> Self { todo!() }\n}",
        )],
    );
    let changed = render_semantic_api_inventory(
        "pub use session::CompilerSession;",
        &[(
            "session",
            "impl CompilerSession {\n    pub fn new(capacity: usize) -> Self { todo!() }\n}",
        )],
    );
    assert_ne!(
        original, changed,
        "public signature changes must alter the inventory"
    );
    assert!(original.contains("pub fn new()->Self"));
    assert!(changed.contains("pub fn new(capacity:usize)->Self"));
    assert!(render_semantic_api_inventory(facade, &[]).contains("CompileOptions"));
}

#[test]
fn semantic_inventory_rejects_aliases_and_globs() {
    let alias = public_use_declarations("pub use queries::CompileOptions as Options;");
    assert!(code_identifiers(&alias[0]).contains(&"as"));
    let glob = public_use_declarations("pub use queries::*;");
    assert!(public_use_is_glob(&glob[0]));
}

#[test]
fn semantic_inventory_rejects_unparsed_layouts_and_records_cfg_attributes() {
    for source in [
        "pub\nfn hidden() {}",
        "pub\tfn hidden() {}",
        "impl\nCompilerSession {}",
        "impl\tCompilerSession {}",
    ] {
        assert!(
            unsupported_api_layout(source, false).is_some(),
            "split API syntax bypassed the fail-closed layout guard: {source:?}"
        );
    }
    for source in [
        "include!(\"api.rs\");",
        "macro_rules! api { () => {} }",
        "generated_api!();",
        "crate::generated_api!();",
        "foo::bar!();",
    ] {
        assert!(unsupported_api_layout(source, true).is_some());
    }
    for source in [
        "include!(\"session_api.rs\");",
        "macro_rules! api { () => { pub fn escaped() {} } }",
        "generated_session_impl!();",
        "#[macro_export]\nmacro_rules! api { () => {} }",
        "#[macro_export(local_inner_macros)]\nmacro_rules! api { () => {} }",
        "impl CompilerSession {\n    generated_methods!();\n}",
        "#[cfg(any(\nunix,\nwindows\n))]\npub fn escaped() {}",
        "#[cfg(unix)]\nimpl CompilerSession {\n    pub fn new() -> Self { todo!() }\n}",
        "#[cfg(unix)]\nimpl crate::CompilerSession {\n    pub fn new() -> Self { todo!() }\n}",
    ] {
        assert!(unsupported_api_layout(source, false).is_some());
    }
    assert!(
        unsupported_api_layout("#[cfg(unix)]\npub use session::CompilerSession;", true).is_some()
    );

    let inventory = render_semantic_api_inventory(
        "pub use session::CompilerSession;",
        &[(
            "session",
            "impl CompilerSession {\n#[cfg(unix)]\npub fn new() -> Self { todo!() }\n}",
        )],
    );
    assert!(inventory.contains("#[cfg(unix)]pub fn new()->Self"));
}

#[test]
fn raw_phase_owners_and_backend_drivers_cannot_return_to_the_stable_facade() {
    let facade = include_str!("lib.rs");
    let root_exports = public_use_declarations(facade).concat();
    let session = public_signatures(inherent_impl(include_str!("session.rs"), "CompilerSession"));
    let update = public_signatures(inherent_impl(
        include_str!("session.rs"),
        "CompilerSessionUpdate",
    ));
    let views = public_declarations(include_str!("artifact_views.rs")).concat();
    let stable_surface = [root_exports, session.clone(), update, views].concat();
    let identifiers = code_identifiers(&stable_surface);

    for forbidden in RUE_868_RAW_FACADE_VOCABULARY {
        assert!(
            !identifiers.contains(forbidden),
            "raw phase owner or backend presentation symbol returned to the stable facade: {forbidden}"
        );
    }

    let session_methods = code_identifiers(&session);
    for internal in [
        "merge",
        "stable_definitions",
        "update_for_presentation",
        "inject_stale_query_for_oracle",
    ] {
        assert!(
            !session_methods.contains(&internal),
            "internal orchestration method returned to CompilerSession's supported surface: {internal}"
        );
    }

    let root_identifiers = code_identifiers(facade);
    for required in [
        "TokenView",
        "SyntaxNodeView",
        "SyntaxView",
        "RirInstructionView",
        "RirOperandView",
        "RirView",
        "SourceIdentityView",
        "SourceLocationView",
    ] {
        assert!(
            root_identifiers.contains(&required),
            "stable owner-bound artifact view is missing from the root: {required}"
        );
    }

    let artifact_views = include_str!("artifact_views.rs");
    for raw_presentation in [
        "impl std::fmt::Display for TokenView",
        "impl fmt::Display for TokenView",
        "pub fn text(",
        "pub fn air_text(",
    ] {
        assert!(
            !artifact_views.contains(raw_presentation),
            "raw phase presentation returned through stable views: {raw_presentation}"
        );
    }

    for view in [
        "SourceLocationView",
        "SyntaxNodeView",
        "RirInstructionView",
        "RirOperandView",
    ] {
        let signatures = public_signatures(inherent_impl(artifact_views, view));
        assert!(
            !code_identifiers(&signatures).contains(&"Span"),
            "stable owner-bound view exposes a raw Span: {view}"
        );
    }
}

#[test]
fn final_internal_records_and_presenters_cannot_return_to_the_stable_root() {
    let root_exports = public_use_declarations(include_str!("lib.rs")).concat();
    let identifiers = code_identifiers(&root_exports);
    for forbidden in RUE_869_INTERNAL_ROOT_VOCABULARY {
        assert!(
            !identifiers.contains(forbidden),
            "RUE-869 internal record or presenter returned to the stable root: {forbidden}"
        );
    }
}

#[test]
fn removed_parallel_entry_points_cannot_return() {
    let production = PRODUCTION_MODULES
        .iter()
        .map(|(_, source)| *source)
        .collect::<String>();
    let forbidden = [
        ["Compilation", "Unit"].concat(),
        ["Legacy", "Rir"].concat(),
        ["compile_", "frontend", "_from_ast"].concat(),
        ["parse_all", "_files"].concat(),
        ["merge_", "symbols"].concat(),
        ["pub fn compile_", "frontend"].concat(),
        ["pub fn compile_multi", "_file"].concat(),
        ["pub fn compile_to", "_"].concat(),
        ["pub fn from_", "sources"].concat(),
        ["pub fn source_", "file"].concat(),
        ["Source", "File"].concat(),
        "CanonicalSemanticOutput".to_owned(),
        "FunctionWithCfg".to_owned(),
        "canonical_semantic_with_cancellation".to_owned(),
        "semantic_attempt".to_owned(),
        "analyze_prepared_canonical_program_reusing_declarations".to_owned(),
        "collect_function_cfg_queries".to_owned(),
        "finish_canonical_analysis".to_owned(),
        "compose_queried_bodies".to_owned(),
        ["Rooted", "Semantic", "Output"].concat(),
        ["rooted_", "semantic"].concat(),
        ["semantic_", "projection_", "for_test"].concat(),
        ["Semantic", "View"].concat(),
        ["Function", "View"].concat(),
        ["Cfg", "View"].concat(),
        ["CfgBlock", "View"].concat(),
        ["CfgInstruction", "View"].concat(),
        ["CfgSuccessor", "View"].concat(),
        ["Type", "View"].concat(),
    ];
    for removed in forbidden {
        assert!(
            !production.contains(&removed),
            "removed compiler entry point returned: {removed}"
        );
    }

    let session = include_str!("session.rs");
    assert!(!session.contains("pub fn semantic("));
    assert!(!session.contains("semantic_view_from_rooted"));
}

#[test]
fn compiler_parallelism_has_one_query_budget_and_no_peer_parallel_frontier() {
    let root = include_str!("lib.rs");
    let cfg = include_str!("queries.rs");
    let backend = include_str!("backend.rs");
    let database = include_str!("revisioned_query_database.rs");
    assert!(
        root.contains("QUERY_CONCURRENCY")
            && root.contains("configure_thread_pool")
            && database.contains("crate::query_concurrency()")
            && database.contains("QueryRuntime::new(query_concurrency)"),
        "compiler runtime configuration must feed the canonical query database budget"
    );
    for (module, source) in [("queries", cfg), ("backend", backend)] {
        for forbidden in ["rayon", ".par_iter(", ".into_par_iter("] {
            assert!(
                !source.contains(forbidden),
                "{module} reintroduced a process-global parallel frontier through {forbidden}"
            );
        }
    }
}

#[test]
fn orphaned_backend_inspection_exports_cannot_return() {
    let facade = include_str!("lib.rs");
    let backend = include_str!("backend.rs");
    let unstable = include_str!("unstable.rs");
    let removed = [
        "generate_allocated_mir",
        "generate_emitted_asm",
        "generate_liveness_info",
        "generate_lowering_info",
        "generate_mir",
        "generate_regalloc_info",
    ];

    for name in removed {
        assert!(
            !code_identifiers(facade).contains(&name),
            "test-only backend helper returned to the production facade: {name}"
        );
        assert!(
            !code_identifiers(backend).contains(&name),
            "orphaned backend inspection path returned: {name}"
        );
        assert!(
            !code_identifiers(unstable).contains(&name),
            "unstable presentation regained a parallel backend path: {name}"
        );
    }
    for (module, source) in PRODUCTION_MODULES {
        for retired in [
            "FunctionBackendProduct",
            "backend_product",
            "generate_backend_products",
            "codegen_products",
            "SectionContents",
            "presentation_fingerprint",
        ] {
            assert!(
                !code_identifiers(source).contains(&retired),
                "retired CodegenUnit ownership identifier remains in {module}: {retired}"
            );
        }
    }
    assert!(
        code_identifiers(backend).contains(&"project_backend_object"),
        "object projection must accept the canonical CodegenUnit"
    );
}

#[test]
fn query_engine_records_cannot_return_to_the_supported_root() {
    let facade = include_str!("lib.rs");
    let mut supported_exports = String::new();
    let mut in_supported_export = false;
    for line in facade.lines() {
        if line.trim_start().starts_with("pub use ") {
            in_supported_export = true;
        }
        if in_supported_export {
            supported_exports.push_str(line);
            supported_exports.push('\n');
            if line.contains(';') {
                in_supported_export = false;
            }
        }
    }
    for forbidden in RUE_866_INTERNAL_VOCABULARY
        .iter()
        .chain(RUE_867_DURABLE_VOCABULARY)
    {
        assert!(
            !supported_exports.contains(forbidden),
            "query-engine implementation record returned to the supported root: {forbidden}"
        );
    }
}

#[test]
fn unstable_views_do_not_alias_query_engine_records() {
    let unstable = include_str!("unstable.rs");
    assert!(!unstable.contains("pub type "));
    let reexports = public_use_declarations(unstable)
        .into_iter()
        .map(|declaration| {
            declaration
                .chars()
                .filter(|ch| !ch.is_whitespace())
                .collect::<String>()
        })
        .collect::<Vec<_>>();
    assert_eq!(
        reexports,
        [
            "pubusecrate::diagnostic::{ColorChoice,DiagnosticFormatter,JsonDiagnostic,JsonDiagnosticFormatter,JsonSpan,JsonSuggestion,MultiFileFormatter,MultiFileJsonFormatter,SourceInfo,};",
            "pubusecrate::import_discovery::{AcceptedImportSource,DiscoverySourceAssembler,ImportDemandFrontier,ImportDemandMode,ImportDemandRoots,ImportDiscoveryPlan,ImportDiscoveryRequest,ImportDiscoveryWave,ImportInputRevision,ImportObservation,ImportObservationLedger,ImportObservationStatus,};",
            "pubuserue_span::Span;",
            "pubusecrate::session::{ClosedDiscoveryContinuation,RootedCfgOutput,RootedCfgUnit,RootedParkOutcome,TrustedSuccessorDelta,};",
        ],
        "unstable may reexport only reviewed presentation, source-assembly, and host discovery-protocol records"
    );

    let facade = include_str!("lib.rs");
    assert_no_public_use_globs(facade);
    let public_modules = facade
        .lines()
        .filter_map(|line| line.trim().strip_prefix("pub mod "))
        .collect::<Vec<_>>();
    assert_eq!(public_modules, ["unstable;"]);

    let session = include_str!("session.rs");
    let diagnostic = include_str!("diagnostic_attempt_store.rs");
    let reviewed_public = [
        public_signatures(inherent_impl(session, "CompilerSession")),
        public_signatures(inherent_impl(session, "CompilerSessionUpdate")),
        public_signatures(inherent_impl(diagnostic, "FrontendDiagnosticSnapshot")),
        public_signatures(inherent_impl(include_str!("queries.rs"), "CompileOutput")),
        public_signatures(inherent_impl(session, "CanonicalImportGraphOutput")),
    ]
    .concat();
    for forbidden in RUE_866_INTERNAL_VOCABULARY {
        assert!(
            !reviewed_public.contains(forbidden),
            "internal vocabulary leaked through a supported public signature or field: {forbidden}"
        );
    }
    let supported_public = supported_public_surface(facade, PRODUCTION_MODULES);
    for forbidden in RUE_867_DURABLE_VOCABULARY {
        assert!(
            !supported_public.contains(forbidden),
            "durable cache schema leaked through the complete supported public surface: {forbidden}"
        );
    }
    assert!(reviewed_public.contains("pub fn stage(&self) -> DiagnosticStage"));

    for forbidden in [
        "pub fn work(&self) -> CompilerSessionWork",
        "pub fn semantic_dependency_inputs",
        "pub fn discovery_attempt(&self) -> Option<&Arc<ImportDiscoveryRevisionArtifact>>",
        "pub fn last_good_discovery(&self) -> Option<&Arc<ImportDiscoveryRevisionArtifact>>",
        "pub fn committed_import_discovery(&self) -> Option<&Arc<ImportDiscoveryRevisionArtifact>>",
    ] {
        assert!(
            !session.contains(forbidden),
            "session implementation record leaked through a public signature: {forbidden}"
        );
    }

    let queries = include_str!("queries.rs");
    assert!(!queries.contains("pub source_stats: SourceStats"));
    assert!(!queries.contains("pub work: PipelineWork"));
}

#[test]
fn qualified_public_globs_are_rejected() {
    for fixture in ["pub use session::*;", "pub use unstable::{Metrics, *};"] {
        assert!(
            public_use_declarations(fixture)
                .iter()
                .any(|declaration| public_use_is_glob(declaration)),
            "qualified glob fixture escaped the public-use guard: {fixture}"
        );
    }
}

#[test]
fn supported_surface_scanner_closes_multiline_and_alias_escapes() {
    let facade = r#"
pub mod unstable;
pub use session::{Exported, ExportedRecord};
pub type Alias = durable_body::DurableType;
"#;
    let session = r#"
pub struct ExportedRecord {
    pub safe: usize,
    pub leaked:
        DurableOrdinaryBody,
}
pub struct Exported;
impl Exported {
    pub fn leaked(
        &self,
    ) -> DurableSpecializedBodyPayload {
        unreachable!()
    }
}
"#;
    let unstable = r#"
pub fn leaked(
    value: usize,
) -> DurableType {
    unreachable!()
}
"#;
    let surface = supported_public_surface(facade, &[("session", session), ("unstable", unstable)]);
    for escaped in [
        "durable_body::DurableType",
        "DurableOrdinaryBody",
        "DurableSpecializedBodyPayload",
        "DurableType",
    ] {
        assert!(
            surface.contains(escaped),
            "balanced public-surface scanner missed adversarial escape: {escaped}"
        );
    }
}

#[test]
fn durable_cache_schema_cannot_return_to_the_public_facade() {
    let facade = include_str!("lib.rs");
    // `durable_body` held only inert accounting and was removed by RUE-1541.
    // Asserting the module is absent is stronger than asserting it stayed
    // private: the schema cannot leak from a module that does not exist.
    assert!(!facade.contains("mod durable_body;"));
    assert!(facade.contains("mod durable_semantics;"));
    assert!(!facade.contains("pub mod durable_semantics;"));

    let session = include_str!("session.rs");
    for raw_accessor in [
        "durable_specialized_body_payloads",
        "durable_ordinary_bodies",
    ] {
        assert!(
            !session.contains(raw_accessor),
            "raw durable schema accessor returned to a public signature: {raw_accessor}"
        );
    }
}

#[test]
fn query_attempts_have_one_family_owned_representation() {
    let production = PRODUCTION_MODULES
        .iter()
        .map(|(_, source)| *source)
        .collect::<String>();
    for peer in [
        ["QueryAttempt", "Ledger"].concat(),
        ["QueryAttempt", "Work"].concat(),
        ["QueryAttempt", "Identity"].concat(),
        ["attempt_", "origin"].concat(),
        ["origins: Vec", "Deque<Arc<FrontendDiagnosticSnapshot>>"].concat(),
        ["SyntaxParse", "Producer"].concat(),
        ["CanonicalParse", "Session"].concat(),
        ["QueryPublication", "Snapshot"].concat(),
    ] {
        assert!(
            !production.contains(&peer),
            "parallel query-attempt ownership returned: {peer}"
        );
    }
    assert!(
        production.contains("attempt: Arc<dyn AttemptView>"),
        "diagnostic and metrics indexes must retain the family-owned attempt Arc"
    );
}

#[test]
fn revisioned_parse_family_is_runtime_registered_without_a_selection_wrapper() {
    let session = include_str!("session.rs");
    let runtime = include_str!("revisioned_query_database.rs");
    for removed in [
        "parse: TypedQueryStore<ParseQuery>",
        "parse_inputs",
        "ParsedProgramLookup",
        "queries.parse",
        "QueryNodeFamily::Parse",
    ] {
        assert!(
            !session.contains(removed),
            "legacy parse query authority returned: {removed}"
        );
    }
    assert!(session.contains(".source_revision(&source, snapshot)"));
    assert!(session.contains("revisioned.select_parse(&attempt)"));
    assert!(!runtime.contains("RevisionedFamily"));
    assert!(!runtime.contains("selected_state_shim"));
    assert!(runtime.contains("parse: QueryFamily<"));
    assert!(runtime.contains("parse_selection: QuerySelection<"));
    assert!(runtime.contains("content_addressed_family_with_equality("));
}

#[test]
fn declaration_shell_queries_are_the_only_compiler_semantic_discovery_authority() {
    let before_tests = |source: &'static str| {
        let marker = "\n#[cfg(test)]\nmod ";
        source
            .rmatch_indices(marker)
            .find_map(|(index, _)| {
                let declaration = source[index + marker.len()..].lines().next().unwrap();
                declaration.ends_with('{').then_some(&source[..index])
            })
            .unwrap_or(source)
    };
    let production = PRODUCTION_MODULES
        .iter()
        .map(|(name, source)| (*name, before_tests(source)))
        .collect::<Vec<_>>();
    let module = |wanted: &str| {
        production
            .iter()
            .find_map(|(name, source)| (*name == wanted).then_some(*source))
            .unwrap()
    };
    let session = module("session");
    let canonical = before_tests(include_str!("canonical_semantic.rs"));
    let parsed = module("parsed_modules");
    let runtime = module("revisioned_query_database");

    let assert_test_gated_calls = |name: &str, source: &str, adapter: &str| {
        for (offset, _) in source.match_indices(adapter) {
            let lines = source[..offset].lines().collect::<Vec<_>>();
            let function_line = lines
                .iter()
                .rposition(|line| line.contains("fn "))
                .unwrap_or_else(|| panic!("{name} called {adapter} outside a function"));
            let mut preceding = lines[..function_line]
                .iter()
                .rev()
                .map(|line| line.trim())
                .filter(|line| !line.is_empty());
            assert_eq!(
                preceding.next(),
                Some("#[cfg(test)]"),
                "compiler module {name} called frozen declaration test adapter {adapter} from a production function"
            );
        }
    };

    for (name, source) in &production {
        for retired in [
            ".predeclare_declaration_shells()",
            ".bind_declarations()",
            ".analyze_all()",
            ".definitions().declarations()",
            ".candidate.fact()",
            "legacy_declaration_shell",
            "fallback_declaration_shell",
            "declaration_shell_cache",
            "warm_declaration_shell",
            "eager_declaration_shell",
            "legacy_raw_const_syntax",
            "fallback_raw_const_syntax",
            "raw_const_syntax_cache",
            "warm_raw_const_syntax",
            "eager_raw_const_syntax",
            "legacy_raw_declaration_signature",
            "fallback_raw_declaration_signature",
            "raw_declaration_signature_cache",
            "warm_raw_declaration_signature",
            "eager_raw_declaration_signature",
            "legacy_raw_declaration_body",
            "fallback_raw_declaration_body",
            "raw_declaration_body_cache",
            "warm_raw_declaration_body",
            "eager_raw_declaration_body",
            "legacy_declaration_import",
            "fallback_declaration_import",
            "declaration_import_cache",
            "warm_declaration_import",
            "eager_declaration_import",
            "Sema::new_synthetic(",
        ] {
            assert!(
                !source.contains(retired),
                "compiler production module {name} bypassed keyed shell authority through {retired}"
            );
        }
        for test_adapter in [
            ".predeclare_declaration_shells_for_test()",
            ".bind_declarations_for_test()",
            ".analyze_all_for_test()",
            ".resolve_declarations_for_test()",
            ".resolve_declarations_with_work_for_test()",
        ] {
            assert_test_gated_calls(name, source, test_adapter);
        }
        if !matches!(*name, "parsed_modules" | "revisioned_query_database") {
            for evaluator_only in [
                ".evaluate_declaration_shell(",
                ".evaluate_raw_declaration_signature(",
                ".declaration_import(",
                ".declaration_capabilities()",
                "project_semantic_shell(",
            ] {
                assert!(
                    !source.contains(evaluator_only),
                    "raw declaration evaluator/importer escaped into {name}: {evaluator_only}"
                );
            }
        }
        if !matches!(
            *name,
            "body_query"
                | "declaration_candidate"
                | "parsed_modules"
                | "revisioned_query_database"
                | "semantic_query_nucleus"
        ) {
            assert!(
                !code_identifiers(source).contains(&"RawDeclarationSignature"),
                "compiler production module {name} escaped the signature-projection authority allowlist"
            );
        }
        if !matches!(
            *name,
            "declaration_candidate"
                | "durable_comptime"
                | "parsed_modules"
                | "revisioned_query_database"
                | "semantic_query_nucleus"
        ) {
            assert!(
                !source.contains("DeclarationImport"),
                "compiler production module {name} escaped the declaration-import authority allowlist"
            );
        }
    }
    // The retired source-owned `Sema` plane is gone from rue-compiler entirely,
    // tests included: nothing may construct a `Sema` or call its frozen test
    // adapters in any build configuration.
    for (name, source) in PRODUCTION_MODULES {
        for retired in [
            "Sema::new_synthetic(",
            "Sema::new_for_target(",
            ".bind_declarations_for_test()",
            ".analyze_all_for_test()",
            ".analyze_all_for_test_with_stable_endpoints(",
            ".resolve_declarations_for_test()",
            ".resolve_declarations_with_work_for_test()",
            ".analyze_all_bodies_for_test()",
        ] {
            assert!(
                !source.contains(retired),
                "rue-compiler module {name} regained the retired Sema plane: {retired}"
            );
        }
    }
    assert!(
        !canonical.contains("predeclare_imported_declaration_shells"),
        "the retired shell-import recipe returned to canonical_semantic"
    );
    assert!(!canonical.contains("fn analyze_canonical_program("));
    assert_eq!(runtime.matches(".evaluate_declaration_shell(").count(), 1);
    assert_eq!(parsed.matches("fn evaluate_declaration_shell(").count(), 1);
    for (name, source) in &production {
        for removed in [
            "RawConstSyntax",
            "raw_const_syntax",
            "compiler.raw-const-syntax",
            "RawDeclarationBody",
            "raw_declaration_body",
            "compiler.raw-declaration-body",
        ] {
            assert!(
                !source.contains(removed),
                "the deleted raw-constant authority returned in {name}: {removed}"
            );
        }
    }
    assert_eq!(
        runtime
            .matches(".evaluate_raw_declaration_signature(")
            .count(),
        0,
        "the production signature projection must not reconstruct raw syntax"
    );
    assert!(!parsed.contains("fn evaluate_raw_declaration_signature("));
    assert!(!parsed.contains("RawDeclarationSignatureLocator"));
    assert!(
        !runtime.contains("compiler.declaration-signature-projection"),
        "the semantic nucleus must not retain a peer signature-projection family"
    );
    assert_eq!(
        runtime.matches("project_semantic_signature(").count(),
        1,
        "the signature query must project the exact borrowed parsed declaration"
    );
    assert!(!runtime.contains("parse_semantic_signature("));
    assert_eq!(runtime.matches(".declaration_import(").count(), 1);
    assert_eq!(parsed.matches("fn declaration_import(").count(), 1);
    assert_eq!(
        runtime.matches("\"compiler.declaration-import\"").count(),
        1
    );
    assert!(!runtime.contains("Vec::remove"));

    let toolchain_demand_evaluator = runtime
        .split("let artifacts_for_toolchain_demands")
        .nth(1)
        .and_then(|tail| tail.split("let transactions_for_produced_anonymous").next())
        .unwrap();
    assert!(toolchain_demand_evaluator.contains("DeclarationBodyPlanQueryKey"));
    assert!(toolchain_demand_evaluator.contains(".fallible_intrinsics()"));
    for forbidden in [
        "RawDeclarationBodyQueryKey",
        "scan_body_payload_kinds",
        "rue_lexer::Lexer",
    ] {
        assert!(
            !toolchain_demand_evaluator.contains(forbidden),
            "toolchain-demand evaluator regained a source-text scan: {forbidden}"
        );
    }

    let occurrence = runtime
        .split("struct DeclarationOccurrenceIndex {")
        .nth(1)
        .and_then(|tail| tail.split("enum DeclarationOccurrenceIndexValue").next())
        .unwrap();
    for forbidden in ["DeclarationShellFact", "CompileErrors", "Span", "FileId"] {
        assert!(
            !occurrence.contains(forbidden),
            "occurrence terminal regained shell/parser payload: {forbidden}"
        );
    }
    let terminal_algebra = runtime
        .split("enum DeclarationOccurrenceIndexValue")
        .nth(1)
        .and_then(|tail| tail.split("struct DeclarationShellQueryKey").next())
        .unwrap();
    assert!(!terminal_algebra.contains("CompileErrors"));
    let shell_terminal = runtime
        .split("enum DeclarationShellQueryValue")
        .nth(1)
        .and_then(|tail| tail.split("struct DeclarationBodyPlanQueryKey").next())
        .unwrap();
    for forbidden in [
        "CompileErrors",
        "Span",
        "FileId",
        "InstRef",
        "Rir",
        "SemanticDefinitionToken",
        "SemanticModuleToken",
    ] {
        assert!(
            !shell_terminal.contains(forbidden),
            "shell terminal regained positioned/live semantic payload: {forbidden}"
        );
    }
    let declaration_import_terminal = runtime
        .split("enum DeclarationImportQueryValue")
        .nth(1)
        .and_then(|tail| tail.split("struct LookupNameKey").next())
        .unwrap();
    for forbidden in [
        "CompileErrors",
        "Span",
        "FileId",
        "Spur",
        "Ast",
        "InstRef",
        "Rir",
        "TypeId",
        "SemanticDefinitionToken",
        "SemanticModuleToken",
    ] {
        assert!(
            !declaration_import_terminal.contains(forbidden),
            "declaration-import terminal regained positioned/live parser or semantic payload: {forbidden}"
        );
    }
    let lookup_fact = runtime
        .split("struct LookupNameFact")
        .nth(1)
        .and_then(|tail| tail.split("struct LookupNameValue").next())
        .unwrap();
    for forbidden in ["CompileErrors", "ModuleRevision", "Span", "FileId", "Spur"] {
        assert!(
            !lookup_fact.contains(forbidden),
            "LookupName retained fact regained locator/live payload: {forbidden}"
        );
    }
    let lookup_value = runtime
        .split("struct LookupNameValue")
        .nth(1)
        .and_then(|tail| {
            tail.split("/// The canonical §4 name-resolution outcome")
                .next()
        })
        .unwrap();
    for forbidden in ["CompileErrors", "ModuleRevision", "Span", "FileId", "Spur"] {
        assert!(
            !lookup_value.contains(forbidden),
            "LookupName terminal regained locator/live payload: {forbidden}"
        );
    }
    let declaration_import_payload = module("declaration_candidate")
        .split("pub(crate) struct DeclarationImportSiteKey")
        .nth(1)
        .and_then(|tail| tail.split("impl DeclarationCandidateKey").next())
        .unwrap();
    for forbidden in [
        "CompileErrors",
        "Span",
        "FileId",
        "Spur",
        "Ast",
        "Rir",
        "InstRef",
        "TypeId",
        "SemanticDefinitionToken",
        "SemanticModuleToken",
    ] {
        assert!(
            !declaration_import_payload.contains(forbidden),
            "declaration-import payload regained a positioned or live parser/semantic handle: {forbidden}"
        );
    }
    let declaration_import_locator = parsed
        .split("struct RawDeclarationImportRange")
        .nth(1)
        .and_then(|tail| tail.split("pub(crate) struct ParsedWarningCallHead").next())
        .unwrap();
    for forbidden in ["Vec<", "Arc<", "Box<"] {
        assert!(
            !declaration_import_locator.contains(forbidden),
            "declaration-import parser locator regained per-declaration heap storage: {forbidden}"
        );
    }
    for required in ["start: u32", "len: u32"] {
        assert!(
            declaration_import_locator.contains(required),
            "declaration-import parser locator lost fixed range field {required}"
        );
    }
    let declaration_import_evaluator = runtime
        .split("let occurrences_for_declaration_import")
        .nth(1)
        .and_then(|tail| tail.split("        Self {").next())
        .unwrap();
    for forbidden in [
        "module_rirs",
        "lower_module_rir",
        "CanonicalMergedProgram",
        "SemanticView",
    ] {
        assert!(
            !declaration_import_evaluator.contains(forbidden),
            "declaration-import evaluator gained a broad compiler dependency: {forbidden}"
        );
    }
    for required in [
        ".query_registered(",
        "ResolveImportKey",
        "ImportDemandMode::Rooted",
    ] {
        assert!(
            declaration_import_evaluator.contains(required),
            "declaration-import evaluator lost canonical query delegation: {required}"
        );
    }
    let resolve_import_evaluator = runtime
        .split("let index_for_import_resolution")
        .nth(1)
        .and_then(|tail| tail.split("let occurrences_for_declaration_import").next())
        .unwrap();
    for required in [
        "index.import_occurrence(&key.occurrence)",
        "exact_import_winner(",
        "resolve_exact_import_winner(",
        "accepted_import_provenance_input(",
        "source.metadata_identity()",
    ] {
        assert!(
            resolve_import_evaluator.contains(required),
            "resolve-import lost exact winning-provenance dependency: {required}"
        );
    }
    for forbidden in [
        "reduce_exact_import_graph(",
        "canonical ResolveImport inputs reduce deterministically",
    ] {
        assert!(
            !resolve_import_evaluator.contains(forbidden),
            "resolve-import regained broad or panicking reduction: {forbidden}"
        );
    }
    let physical_provenance_identity = runtime
        .split("fn accepted_import_provenance_input")
        .nth(1)
        .and_then(|tail| tail.split("fn import_observation_input").next())
        .unwrap();
    for required in ["identity.volume()", "identity.file()"] {
        assert!(
            physical_provenance_identity.contains(required),
            "accepted-import provenance input lost exact physical identity component: {required}"
        );
    }
    let failure_algebra = module("declaration_candidate")
        .split("pub(crate) enum DeclarationOccurrenceFailure")
        .nth(1)
        .and_then(|tail| tail.split("impl DeclarationCandidateKey").next())
        .unwrap()
        // The raw syntax terminals carry the durable, definition-relative
        // frontend anchor for each transported anonymous type literal
        // (RUE-1089). `RirStructuralAnchor` is position- and trivia-insensitive
        // by construction — the antithesis of a live IR handle — so it is
        // sanctioned here while raw `Rir`/`InstRef`/`Span` handles stay banned.
        .replace("rue_rir::RirStructuralAnchor", "<durable-anchor>");
    for forbidden in [
        "CompileErrors",
        "Span",
        "FileId",
        "InstRef",
        "Rir",
        "SemanticDefinitionToken",
        "SemanticModuleToken",
    ] {
        assert!(
            !failure_algebra.contains(forbidden),
            "declaration failure algebra regained position/live identity: {forbidden}"
        );
    }
    assert_eq!(
        canonical
            .matches("predeclare_imported_declaration_shells")
            .count(),
        0,
        "the retired shell-import recipe may not return to canonical_semantic"
    );
    for (name, source) in &production {
        assert!(
            !source.contains("predeclare_imported_declaration_shells"),
            "compiler production module {name} gained a peer shell importer"
        );
    }
    assert!(!session.contains("DeclarationShellFact"));
}

#[test]
fn canonical_semantic_body_has_no_compiler_owned_peer_algebra() {
    // This guard used to read `durable_body.rs` alone. RUE-1541 removed that
    // module, so the mirror would have to reappear in some other production
    // module — which is what this now checks.
    for (name, source) in PRODUCTION_MODULES {
        for removed in [
            "pub enum DurableAirInstData",
            "pub struct DurableAirInst",
            "pub struct DurablePlace",
            "pub enum DurableProjection",
            "pub enum DurablePattern",
            "pub struct DurableBodyAnchor",
            "pub struct DurableSpecializationIdentity",
            "pub type DurableProjection",
            "pub type DurableAirInstData",
        ] {
            assert!(
                !source.contains(removed),
                "compiler-owned canonical body mirror returned in {name}: {removed}"
            );
        }
        for removed in [
            "DurableOrdinaryBodyPayload",
            "DurableSpecializedBodyPayload",
            "convert_semantic_body_exports",
            "convert_semantic_specialized_body_exports",
        ] {
            assert!(
                !source.contains(removed),
                "removed peer body-export payload returned in {name}: {removed}"
            );
        }
    }
}

#[test]
fn rue_1027_production_body_authority_is_query_owned_and_import_only() {
    let canonical = include_str!("canonical_semantic.rs");
    let session = include_str!("session.rs");
    let database = include_str!("revisioned_query_database.rs");
    let body_query = include_str!("body_query.rs");

    for removed_peer_assembler in [
        "fn finish_canonical_analysis(",
        "compose_queried_bodies(",
        "analyze_all_bodies",
        "fn recover_declaration_failure(",
    ] {
        assert!(
            !canonical.contains(removed_peer_assembler),
            "canonical semantic peer assembly returned: {removed_peer_assembler}"
        );
    }
    assert!(!session.contains("pub fn semantic("));
    assert!(!session.contains("semantic_view_from_rooted"));
    assert!(!session.contains("requires_request_local_discovery"));

    let anonymous_branch = database
        .split("Key::AnonymousNominal(query) =>")
        .nth(1)
        .and_then(|source| source.split("Key::ComptimeCall(call) =>").next())
        .expect("SemanticNucleus anonymous nominal branch");
    assert!(anonymous_branch.contains("produced_anonymous_for_semantic_nucleus"));
    assert!(!anonymous_branch.contains("Key::ComptimeCall("));
    for family in [
        "compiler.body-transaction",
        "compiler.body-produced-anonymous",
    ] {
        assert!(
            database.contains(family),
            "missing RUE-1027 family: {family}"
        );
    }
    for redundant_projection in [
        "\"compiler.body-references\"",
        "\"compiler.canonical-body\"",
    ] {
        assert!(!database.contains(redundant_projection));
    }
    assert!(body_query.contains("pub(crate) struct BodyQueryKey"));
    assert!(body_query.contains("pub(crate) struct BodyProducedAnonymousNominals"));
    assert!(!body_query.contains("Span"));
}

#[test]
fn rue_1191_anonymous_digest_collision_authority_is_body_closure_owned() {
    let database = include_str!("revisioned_query_database.rs");
    let production = database
        .split("\n#[cfg(test)]\nmod tests")
        .next()
        .expect("production revisioned database source");
    let closure = database
        .split("let body_closures =")
        .nth(1)
        .and_then(|source| source.split("let closures_for_publication =").next())
        .expect("registered body-closure evaluator");

    assert!(closure.contains("anonymous_digest_owners"));
    assert!(closure.contains("compiler_anonymous_identity_digest"));
    assert!(closure.contains("BodyClosureFatal::AnonymousDigestCollision"));
    assert_eq!(
        closure
            .matches("register_body_closure_anonymous_digest(")
            .count(),
        2,
        "closure aggregation must register declaration and body-produced anonymous facts"
    );
    assert_eq!(
        production
            .matches("register_body_closure_anonymous_digest(")
            .count(),
        3,
        "the two closure call sites plus the helper definition are the complete production authority"
    );

    let body_local_projection = database
        .split("let body_produced_anonymous =")
        .nth(1)
        .and_then(|source| {
            source
                .split("let produced_anonymous_for_semantic_nucleus =")
                .next()
        })
        .expect("body-produced-anonymous evaluator");
    assert!(
        !body_local_projection.contains("register_body_closure_anonymous_digest("),
        "the per-body projection must not call the cross-body registrar"
    );
    let transaction_evaluator = production
        .split("impl BodyTransactionEvaluator {")
        .nth(1)
        .and_then(|source| source.split("\nimpl RevisionedQueryDatabase {").next())
        .expect("registered per-body transaction evaluator");
    assert!(
        !transaction_evaluator.contains("register_body_closure_anonymous_digest("),
        "the per-body evaluator must not call the cross-body registrar"
    );
}

#[test]
fn durable_const_integer_semantics_use_the_shared_kernel() {
    let source = include_str!("revisioned_query_database.rs");
    let fits = source
        .split("fn durable_const_fits_type(")
        .nth(1)
        .and_then(|source| source.split("fn durable_int_width(").next())
        .expect("durable integer fit adapter");
    let durable = include_str!("durable_comptime.rs");
    let canonical_fits = durable
        .split("pub(crate) fn durable_const_fits_type(")
        .nth(1)
        .and_then(|source| source.split("pub(crate) fn durable_int_width(").next())
        .expect("canonical durable integer fit kernel");
    let evaluator = source
        .split("impl SemanticConstEvaluator<'_, '_> {")
        .nth(1)
        .and_then(|source| source.split("impl SemanticNucleusTypeProvider<'_>").next())
        .expect("durable evaluator source");
    let arithmetic = evaluator
        .split("fn eval_binary(")
        .nth(1)
        .and_then(|source| source.split("fn eval_block(").next())
        .expect("durable arithmetic evaluator");
    let unary = evaluator
        .split("E::Neg { operand } | E::BitNot { operand }")
        .nth(1)
        .and_then(|source| source.split("E::Block { instructions }").next())
        .expect("durable unary evaluator");
    let arithmetic_policy = format!("{arithmetic}{unary}");

    assert!(fits.contains("durable_const_fits_type"));
    assert!(canonical_fits.contains("integer.fits_i128"));
    for duplicated_fit in [
        "i8::try_from",
        "i16::try_from",
        "i32::try_from",
        "i64::try_from",
        "u8::try_from",
        "u16::try_from",
        "u32::try_from",
        "u64::try_from",
    ] {
        assert!(
            !fits.contains(duplicated_fit),
            "durable const fit checks regained local integer policy: {duplicated_fit}"
        );
    }

    for required in [
        "durable_int_width",
        "checked_add_report_i128",
        "checked_sub_report_i128",
        "checked_mul_report_i128",
        "checked_div_report_i128",
        "checked_rem_report_i128",
        "checked_neg_report_i128",
        "checked_neg_literal_report_i128",
        "compare_i128",
    ] {
        assert!(
            evaluator.contains(required),
            "durable evaluator missing {required}"
        );
    }
    for forbidden in [
        ".checked_add(",
        ".checked_sub(",
        ".checked_mul(",
        ".checked_div(",
        ".checked_rem(",
        ".checked_neg(",
        "left < right",
        "left > right",
        "left <= right",
        "left >= right",
    ] {
        assert!(
            !arithmetic_policy.contains(forbidden),
            "durable evaluator regained local integer policy: {forbidden}"
        );
    }
}

#[test]
fn durable_comptime_services_are_named_authority_operations() {
    let facade = include_str!("durable_comptime.rs");
    let database = include_str!("revisioned_query_database.rs");
    let production_facade = facade
        .split("#[cfg(test)]\nmod tests")
        .next()
        .expect("durable comptime production source");
    for required in [
        "DurableImportSite",
        "DurableComptimeSemanticAuthority",
        "DurableComptimeForeignCallAuthority",
        "DurableComptimeEffects",
        "DurableComptimeCallLifecycle",
        "DurableComptimeCompletion",
        "DurableComptimeCallEdge",
        "accessing_source",
        "DurableComptimeCallTicket",
        "DurableComptimeLifecycleError",
        "DurableComptimeApplicationPolicy",
        "DurableComptimeDiagnosticSite",
        "DurableComptimeFailure",
        "DurableComptimeHostFailure",
        "DurableComptimeSession",
        "DurableComptimeName",
        "DurableComptimeIdentity",
        "DurableComptimeConstFrame",
        "DurableComptimeConstRootAdmissionError",
        "admit_const_root",
        "DurableComptimeForeignCall",
        "DurableComptimeForeignCallError",
        "DurableAnonymousNominalDescriptor",
        "DurableAnonymousNominalDescriptorShape",
        "project_durable_anonymous_nominal",
        "durable_parameter_mode",
        "consume_foreign_lookup",
        "into_host_error",
        "provider_error_as_host",
        "maximum_depth",
        "integer_literal_overflow",
        "arithmetic_overflow",
        "division_by_zero",
        "remainder_by_zero",
        "DurableComptimeCallableAdmission",
        "DurableComptimeCallableAdmissionStart",
        "DurableComptimeNamedValueKind",
        "DurableComptimeNamedValueOrder",
        "resolve_named_value_in_order",
        "resolve_module_member_in_order",
        "begin_comptime_call_admission",
        "finish_comptime_call_admission",
        "DurableComptimeNamedValueProjection",
        "resolve_named_value",
        "resolve_module_member",
        "resolve_target_intrinsic",
        "resolve_target_enum_variant",
        "probe_comptime_call",
        "observe_dependency",
        "observe_anonymous_nominal",
        "observe_deferred_ownership",
        "current_effects_mut",
        "merge_projection",
        "merge_ready_projection",
        "merge_ready_lookup",
        "prepare_expression_edge",
        "prepare_structured_edge",
        "ticket_from_admitted_edge",
        "merge_child",
        "complete_root",
        "begin_durable_structured_type",
        "resume_durable_structured_type",
        "impl ComptimeName for DurableComptimeName",
        "impl ComptimeFile for ModuleId",
        "impl ComptimeIdentity for DurableComptimeIdentity",
    ] {
        assert!(
            facade.contains(required),
            "durable facade missing {required}"
        );
    }
    let frame_adapter = production_facade
        .split("pub(crate) fn admit_const_root(")
        .nth(1)
        .and_then(|source| {
            source
                .split("\n    }\n\n    pub(crate) fn next_call_ordinal")
                .next()
        })
        .expect("const frame adapter");
    for required in [
        "ComptimeFrame",
        "core.plan.key.clone()",
        "body: init",
        "context: Some(core.plan.candidate.module.clone())",
        "call_identity: None",
        "expected_result",
    ] {
        assert!(
            frame_adapter.contains(required),
            "const frame adapter missing {required}"
        );
    }
    for forbidden in [
        "InstData",
        "ComptimeEngine",
        "SemanticConstEvaluator",
        "eval(",
    ] {
        assert!(
            !frame_adapter.contains(forbidden),
            "const frame adapter gained evaluation authority: {forbidden}"
        );
    }
    for forbidden in ["InstData", "eval_const_expr", "ComptimeEngine", "InstRef"] {
        assert!(
            !facade.contains(forbidden),
            "durable service facade must not become an evaluator: {forbidden}"
        );
    }
    let body_query = include_str!("body_query.rs");
    for required in [
        "OwnedComptimeProgramCore",
        "OwnedComptimeProgramRoot",
        "type DurableComptimeProgramKey = rue_air::ComptimeProgramKey",
        "DurableComptimeProgramMetadata",
        "rue_air::ComptimeProgram<Arc<str>, DurableComptimeProgramMetadata>",
        "type DurableComptimeProgramRegistry = rue_air::ComptimeProgramRegistry",
        "impl Deref for OwnedForeignComptimeProgram",
        "from_const_body_plan",
    ] {
        assert!(
            body_query.contains(required),
            "shared program core missing {required}"
        );
    }
    let core = body_query
        .split("pub(crate) struct OwnedComptimeProgramCore {")
        .nth(1)
        .and_then(|source| source.split("}\n\n/// Owned compiler/query-side").next())
        .expect("shared durable program core");
    assert!(core.contains("program: DurableComptimeProgram,"));
    assert!(!core.contains("root:"));
    let metadata = body_query
        .split("pub(crate) struct DurableComptimeProgramMetadata {")
        .nth(1)
        .and_then(|source| {
            source
                .split("}\n\npub(crate) type DurableComptimeProgram")
                .next()
        })
        .expect("single owning durable program metadata entry");
    let metadata_fields = metadata
        .lines()
        .map(str::trim)
        .filter(|line| line.ends_with(','))
        .collect::<Vec<_>>();
    assert_eq!(
        metadata_fields,
        [
            "pub(crate) imports: Arc<[DurableComptimeImportOccurrence]>,",
            "pub(crate) root: OwnedComptimeProgramRoot,",
        ],
        "durable program metadata owns exactly imports and root authority"
    );
    assert!(!body_query.contains("struct DurableComptimeProgramRegistry {"));
    assert!(!body_query.contains("let mut registry = rue_air::ComptimeProgramRegistry"));
    for forbidden in [
        "ValidatedRir",
        "symbols:",
        "import_occurrences:",
        "registry:",
    ] {
        assert!(
            !core.contains(forbidden),
            "compiler core duplicated AIR program authority: {forbidden}"
        );
    }
    let admission = body_query
        .split("fn from_body_plan(")
        .nth(2)
        .and_then(|source| source.split("/// Materialize a const declaration").next())
        .expect("shared comptime program admission kernel");
    assert_eq!(
        admission
            .matches("materialize_semantic_candidate_rir")
            .count(),
        1
    );
    assert_eq!(
        admission
            .matches("semantic_candidate_import_occurrences")
            .count(),
        1
    );
    let call_payload = body_query
        .split("pub(crate) struct OwnedForeignComptimeProgram {")
        .nth(1)
        .and_then(|source| source.split("}\n\nimpl Deref").next())
        .expect("foreign program payload");
    for forbidden in [
        "ValidatedRir",
        "symbols:",
        "import_occurrences:",
        "pub(crate) plan:",
        "from_const_body_plan",
    ] {
        assert!(
            !call_payload.contains(forbidden),
            "foreign payload duplicated shared program ownership: {forbidden}"
        );
    }
    let structured_adapter = production_facade
        .split("pub(crate) fn begin_durable_structured_type")
        .nth(1)
        .and_then(|source| source.split("impl ComptimeValue").next())
        .expect("durable structured adapter");
    for forbidden in [
        "InstData",
        "eval(",
        "ComptimeEngine",
        "SemanticConstEvaluator",
        "from_registered",
    ] {
        assert!(
            !structured_adapter.contains(forbidden),
            "structured adapter gained evaluator authority: {forbidden}"
        );
    }
    assert!(structured_adapter.contains(".structured_type_authority("));
    let session = production_facade
        .split("pub(crate) struct DurableComptimeSession {")
        .nth(1)
        .and_then(|source| source.split("}\n\n/// Engine-shaped semantic input").next())
        .expect("durable root session");
    let session_fields = session
        .lines()
        .map(str::trim)
        .filter(|line| line.ends_with(','))
        .collect::<Vec<_>>();
    assert_eq!(
        session_fields,
        [
            "lifecycle: DurableComptimeCallLifecycle,",
            "next_call: u32,",
            "programs: crate::body_query::DurableComptimeProgramRegistry,",
        ],
        "durable session retains its lifecycle and keyed AIR registry"
    );
    assert!(
        production_facade.contains("metadata_mut(key)"),
        "registry finalization must mutate metadata only"
    );
    assert!(
        !production_facade.contains("programs.get_mut(key)"),
        "durable session must not obtain whole-program mutable access"
    );
    assert!(!session.contains("pub(crate)"));
    for forbidden in ["InstData", "InstRef", "ComptimeEngine"] {
        assert!(
            !session.contains(forbidden),
            "durable session duplicated AIR frame authority: {forbidden}"
        );
    }
    let foreign_adapter = production_facade
        .split("pub(crate) enum DurableComptimeForeignCall {")
        .nth(1)
        .and_then(|source| source.split("impl DurableComptimeSession").next())
        .expect("foreign lookup adapter");
    assert!(
        foreign_adapter
            .contains("Ready(crate::semantic_query_nucleus::ComptimeCallResultProjection)")
    );
    assert!(foreign_adapter.contains("Enter {"));
    assert!(foreign_adapter.contains("ticket: Box<DurableComptimeCallTicket>"));
    assert!(foreign_adapter.contains("NotReady"));
    for required in [
        "ReadyFailure(crate::semantic_query_nucleus::SemanticNucleusFailure)",
        "ReadyQueryFailure(rue_query::QueryFailure)",
        "AdmissionFailure(crate::body_query::ComptimeProgramProjectionFailure)",
        "UnexpectedReadyProjection",
    ] {
        assert!(
            foreign_adapter.contains(required),
            "missing explicit lookup error: {required}"
        );
    }
    assert!(!foreign_adapter.contains("Lookup(ForeignComptimeCallLookup)"));
    assert!(!foreign_adapter.contains("SemanticConstEvaluator"));
    assert!(!foreign_adapter.contains("ComptimeEngine"));
    let comptime_probe = database
        .split("struct DurableComptimeForeignQueryAuthority<'a> {")
        .nth(1)
        .and_then(|source| source.split("impl CompilerBodyFactProvider").next())
        .expect("canonical compiler comptime probe authority");
    let probe_fields = database
        .split("struct DurableComptimeForeignQueryAuthority<'a> {")
        .nth(1)
        .and_then(|source| {
            source
                .split("}\n\n#[allow(dead_code)] // activated by the staged durable AIR host\nimpl")
                .next()
        })
        .expect("foreign probe authority fields");
    let probe_field_lines = probe_fields
        .lines()
        .map(str::trim)
        .filter(|line| line.ends_with(','))
        .collect::<Vec<_>>();
    assert_eq!(
        probe_field_lines,
        [
            "context: &'a QueryContext,",
            "semantic_nucleus: &'a SemanticNucleusFamily,",
            "&'a QueryFamily<DeclarationBodyPlanQueryKey, DeclarationBodyPlanArtifactsValue>,",
            "configuration: &'a crate::semantic_query_nucleus::SemanticQueryConfiguration,",
        ],
        "foreign probe authority must retain exactly four narrow query authorities"
    );
    assert!(probe_fields.contains(
        "declaration_body_plan_artifacts:\n        &'a QueryFamily<DeclarationBodyPlanQueryKey, DeclarationBodyPlanArtifactsValue>,"
    ));
    assert_eq!(
        comptime_probe
            .matches("join_registered_noncomputing(")
            .count(),
        1
    );
    assert_eq!(comptime_probe.matches("query_registered(").count(), 1);
    assert_eq!(
        comptime_probe
            .matches("OwnedForeignComptimeProgram::from_body_plan")
            .count(),
        1
    );
    let join_at = comptime_probe
        .find("join_registered_noncomputing(")
        .expect("foreign probe begins with a non-computing semantic lookup");
    let artifact_at = comptime_probe
        .find("query_registered(")
        .expect("foreign probe may query only the declaration body-plan family");
    let admission_at = comptime_probe
        .find("OwnedForeignComptimeProgram::from_body_plan")
        .expect("a cold foreign probe admits the owned body plan");
    assert!(join_at < artifact_at && artifact_at < admission_at);
    assert!(comptime_probe.contains("self.declaration_body_plan_artifacts"));
    assert!(!comptime_probe.contains("query_registered(self.semantic_nucleus"));
    assert_eq!(
        database
            .matches(
                "impl crate::durable_comptime::DurableComptimeForeignCallAuthority\n    for DurableComptimeForeignQueryAuthority"
            )
            .count(),
        1,
        "one narrow query authority must own foreign-call probe policy"
    );
    assert_eq!(
        database
            .matches(
                "impl crate::durable_comptime::DurableComptimeForeignCallAuthority for CompilerBodyFactProvider"
            )
            .count(),
        0,
        "the broad body provider must not implement foreign-call policy"
    );
    assert!(
        comptime_probe.contains("join_registered_noncomputing(")
            && !comptime_probe.contains("probe_registered_ready("),
        "comptime probe must join or reuse exact current registered work through the non-computing query seam"
    );
    assert!(
        !comptime_probe.contains("valid_for_revision")
            && !comptime_probe.contains("query_task_registered"),
        "comptime probe must not validate retained candidates or demand a peer query body"
    );
    for forbidden in [
        "SemanticConstEvaluator",
        "ComptimeEngine",
        "InstData",
        "InstRef",
        "query(&self.queries.semantic_nucleus",
        "query(&self.semantic_nucleus",
        "query_task_registered",
        "valid_for_revision",
    ] {
        assert!(
            !comptime_probe.contains(forbidden),
            "foreign probe authority gained forbidden computation authority: {forbidden}"
        );
    }
    for required in [
        "context: &'a QueryContext",
        "semantic_nucleus: &'a SemanticNucleusFamily",
        "declaration_body_plan_artifacts:",
        "configuration: &'a crate::semantic_query_nucleus::SemanticQueryConfiguration",
        "ReadyQueryProbe::Miss | rue_query::ReadyQueryProbe::NotReady",
        "OwnedForeignComptimeProgram::from_body_plan",
    ] {
        assert!(
            comptime_probe.contains(required),
            "foreign probe authority lost an exact query-side operation: {required}"
        );
    }
    assert!(database.contains("DurableComptimeServices::new(&mut authority).probe_comptime_call("));
    assert!(production_facade.contains("pub(crate) fn registered_program("));
    let registry_accessor = production_facade
        .split("pub(crate) fn registered_program(")
        .nth(1)
        .and_then(|source| source.split("\n    fn observe_anonymous_nominal").next())
        .expect("keyed durable program registry accessor");
    assert!(registry_accessor.contains("self.programs.get(key)"));
    assert!(!registry_accessor.contains("ComptimeEngine"));
    assert!(!registry_accessor.contains("InstData"));
    assert!(!registry_accessor.contains("InstRef"));
    let foreign_frame_admission = production_facade
        .split("pub(crate) fn admit_foreign_frame(")
        .nth(1)
        .and_then(|source| source.split("\n    fn observe_anonymous_nominal").next())
        .expect("single atomic foreign frame admission funnel");
    for required in [
        "registered.imports.root",
        "context.program == *key",
        "bound.type_arguments",
        "bound.typed_value_arguments",
        "expected_result: Some(bound.expected_result.into())",
        "call_identity: None",
        "function_span",
    ] {
        assert!(
            foreign_frame_admission.contains(required),
            "foreign frame admission lost keyed invariant: {required}"
        );
    }
    for forbidden in ["ComptimeEngine", "InstData", "self.lifecycle.enter("] {
        assert!(
            !foreign_frame_admission.contains(forbidden),
            "foreign frame admission activated or walked AIR directly: {forbidden}"
        );
    }
    assert!(!foreign_frame_admission.contains("prepared_value_bindings"));
    assert!(!foreign_frame_admission.contains("expected_result: Option"));
    assert!(!production_facade.contains("program_roots"));
    assert_eq!(
        foreign_frame_admission
            .matches("rue_air::ComptimeFrame {")
            .count(),
        1,
        "foreign frame construction has one canonical atomic funnel"
    );
    assert!(!production_facade.contains("impl Clone for DurableComptimeForeignCall"));
    assert!(!production_facade.contains("impl Clone for DurableComptimeCallTicket"));
    for (kind, non_replayable) in [
        ("struct", "DurableComptimeCallTicket"),
        ("enum", "DurableComptimeForeignCall"),
    ] {
        let derive = production_facade
            .split(&format!("pub(crate) {kind} {non_replayable}"))
            .next()
            .and_then(|source| source.rsplit("#[derive(").next())
            .and_then(|source| source.split(")]").next())
            .expect("non-replayable durable handoff derive");
        assert!(
            !derive.split(',').any(|item| item.trim() == "Clone"),
            "durable handoff capability became replayable: {non_replayable}"
        );
    }
    assert_eq!(
        production_facade
            .matches("pub(crate) fn next_call_ordinal(")
            .count(),
        1
    );
    assert!(production_facade.contains("pub(crate) fn lifecycle_mut("));
    assert!(production_facade.contains("pub(crate) fn prepare_expression_edge("));
    assert!(production_facade.contains("pub(crate) fn finish_ready_expression_edge("));
    let failure_carrier = production_facade
        .split("pub(crate) enum DurableComptimeFailure {")
        .nth(1)
        .and_then(|source| source.split("}\n\nimpl DurableComptimeFailure").next())
        .expect("durable failure carrier");
    for forbidden in [
        "InstData",
        "InstRef",
        "ComptimeFrame",
        "ComptimeEngine",
        "Span",
    ] {
        assert!(
            !failure_carrier.contains(forbidden),
            "durable failure carrier leaked revision/evaluator authority: {forbidden}"
        );
    }
    assert!(failure_carrier.contains("Abort(QueryAbort)"));
    assert!(failure_carrier.contains("Failure(Box<SemanticNucleusFailure>)"));
    let terminal_bridge = production_facade
        .split("/// A revision-independent diagnostic location")
        .nth(1)
        .and_then(|source| source.split("/// The ownership-site policy").next())
        .expect("durable terminal bridge");
    let bridge_without_future_trap = terminal_bridge.replace(
        "#[allow(dead_code)] // consumed when the durable AIR host is wired",
        "",
    );
    assert!(
        !bridge_without_future_trap.contains("allow(dead_code)"),
        "durable terminal bridge must be live production code except its staged future adapters"
    );
    assert!(terminal_bridge.contains("ComptimeHostError::HostFailure"));
    assert!(terminal_bridge.contains("ComptimeHostError::Abort"));
    assert_eq!(
        terminal_bridge
            .matches("ComptimeHostError::HostFailure(self)")
            .count(),
        1,
        "durable host-failure construction must have one canonical funnel"
    );
    assert_eq!(
        terminal_bridge
            .matches("ComptimeHostError::Abort(self)")
            .count(),
        1,
        "durable abort construction must have one canonical funnel"
    );
    assert!(!database.contains("ComptimeHostError::HostFailure"));
    assert!(!database.contains("ComptimeHostError::Abort"));
    for reason in [
        "division by zero (this operation would panic at runtime)",
        "remainder by zero (this operation would panic at runtime)",
    ] {
        assert_eq!(
            production_facade.matches(reason).count(),
            1,
            "durable arithmetic reason must have one canonical production spelling: {reason}"
        );
    }
    assert!(database.contains(
        "type EvaluateSemanticConstError = crate::durable_comptime::DurableComptimeFailure;"
    ));
    assert!(!database.contains("enum EvaluateSemanticConstError {"));
    for routed in [
        "DurableComptimeFailure::maximum_depth(",
        "DurableComptimeFailure::integer_literal_overflow(",
        "DurableComptimeFailure::arithmetic_overflow(",
        "DurableComptimeFailure::division_by_zero()",
        "DurableComptimeFailure::remainder_by_zero()",
        "DurableComptimeFailure::provider_error(error)",
        "DurableComptimeFailure::abort(abort)",
    ] {
        assert!(
            database.contains(routed),
            "legacy durable evaluator no longer routes {routed} through the canonical bridge"
        );
    }
    assert!(facade.contains("query: crate::semantic_query_nucleus::ComptimeCallQueryKey"));
    assert!(facade.contains("child_producer: crate::StableDefinitionKey"));
    let context_block = facade
        .split("pub(crate) struct DurableComptimeCallContext {")
        .nth(1)
        .and_then(|source| source.split("}\n\nimpl DurableComptimeCallContext").next())
        .expect("durable call context block");
    assert!(
        !context_block.contains("pub(crate)"),
        "durable call context identity fields must remain private"
    );
    assert!(context_block.contains("application_policy"));
    assert!(!context_block.contains("call_ordinal"));
    assert_eq!(facade.matches("fn merge_effects_into(").count(), 1);
    assert_eq!(facade.matches("merge_effects_into(").count(), 4);
    let ticket_block = facade
        .split("/// Non-clone edge capability issued after parent validation and before lookup.")
        .nth(1)
        .and_then(|source| {
            source
                .split("/// Non-clone lifecycle capability issued only after an edge is admitted.")
                .next()
        })
        .expect("durable call capability block");
    assert!(!ticket_block.contains("Clone"));
    assert!(!facade.contains("impl Clone for DurableComptimeCallEdge"));
    assert!(!facade.contains("impl Clone for DurableComptimeCallTicket"));
    let edge_fields = facade
        .split("pub(crate) struct DurableComptimeCallEdge {")
        .nth(1)
        .and_then(|source| source.split("}\n\nimpl DurableComptimeCallEdge").next())
        .expect("durable call edge block");
    assert!(!edge_fields.contains("pub(crate)"));
    assert!(!edge_fields.contains("context:"));
    assert!(edge_fields.contains("application_policy:"));
    let ready_projection = facade
        .split("pub(crate) fn merge_ready_projection(")
        .nth(1)
        .and_then(|source| {
            source
                .split("\n    /// Consume a foreign-call lookup")
                .next()
        })
        .expect("ready projection lifecycle operation");
    assert!(ready_projection.contains("edge: &mut DurableComptimeCallEdge"));
    assert!(!ready_projection.contains("policy:"));
    assert!(ready_projection.contains("edge.application_policy"));
    assert_eq!(
        production_facade
            .matches("self.validate_ready_edge(edge)?;")
            .count(),
        1,
        "ready-edge validation must have one canonical implementation"
    );
    assert_eq!(
        production_facade.matches("ready.merge_projection(").count(),
        1,
        "ready projection observation merging must have one canonical implementation"
    );
    assert_eq!(
        production_facade
            .matches(".merge_child(ready, &edge.application_policy);")
            .count(),
        1,
        "ready edge policy application must have one canonical implementation"
    );
    let owned_ready = production_facade
        .split("pub(crate) fn merge_ready_projection_owned(")
        .nth(1)
        .and_then(|source| {
            source
                .split("\n    /// Consume a foreign-call lookup")
                .next()
        })
        .expect("owned ready projection lifecycle operation");
    assert!(owned_ready.contains("self.merge_ready_projection(edge, &projection)?;"));
    assert!(facade.contains("pub(crate) fn prepare_expression_edge("));
    assert!(facade.contains("pub(crate) fn prepare_structured_edge("));
    let direct_prepare_prefix = facade
        .split("pub(crate) fn prepare(\n")
        .next()
        .expect("test-only direct ticket preparation helper");
    assert!(
        direct_prepare_prefix
            .rsplit("\n")
            .take(4)
            .any(|line| line.contains("#[cfg(test)]")),
        "direct ticket preparation bypasses the pre-lookup edge seam"
    );
    for helper in ["from_admitted_expression", "from_admitted_structured"] {
        let helper_prefix = facade
            .split(&format!("pub(crate) fn {helper}"))
            .next()
            .expect("test-only admitted helper");
        assert!(
            helper_prefix
                .rsplit("\n")
                .take(4)
                .any(|line| line.contains("#[cfg(test)]")),
            "admitted helper bypasses the pre-lookup edge seam: {helper}"
        );
    }
    assert!(facade.contains("ticket_from_admitted_edge"));
    let callable_authority = facade
        .split("pub(crate) struct DurableComptimeCallableAdmission {")
        .nth(1)
        .and_then(|source| source.split("}\n\n/// The exact durable projection").next())
        .expect("durable callable admission projection");
    for forbidden in ["InstData", "InstRef", "Rir", "callback", "evaluate("] {
        assert!(
            !callable_authority.contains(forbidden),
            "callable admission projection leaked evaluator authority: {forbidden}"
        );
    }
    let callable_start = facade
        .split("pub(crate) struct DurableComptimeCallableAdmissionStart {")
        .nth(1)
        .and_then(|source| source.split("}\n\n/// The immutable").next())
        .expect("durable callable admission start projection");
    assert!(!callable_start.contains("Clone"));
    assert!(facade.contains(
        "#[derive(Debug, PartialEq, Eq)]\npub(crate) struct DurableComptimeCallableAdmissionStart"
    ));
    assert!(!facade.contains(
        "#[derive(Debug, Clone, PartialEq, Eq)]\npub(crate) struct DurableComptimeCallableAdmissionStart"
    ));
    assert!(!facade.contains("impl Clone for DurableComptimeCallableAdmissionStart"));
    assert_eq!(
        production_facade
            .matches("gate.application.is_none()")
            .count(),
        1
    );
    let finish = facade
        .split("pub(crate) fn finish<V, F>(")
        .nth(1)
        .and_then(|source| source.split("\n    pub(crate) fn complete_root<").next())
        .expect("durable lifecycle finish implementation");
    assert!(!finish.contains("DeferredOwnershipApplication {"));
    assert!(finish.contains(".merge_child(scope, &context.application_policy)"));
    assert!(finish.contains("ComptimeOutcome::Known"));
    assert!(!finish.contains("DurableComptimeEffects"));
    let completion = facade
        .split("pub(crate) struct DurableComptimeCompletion")
        .nth(1)
        .and_then(|source| source.split("\n\n/// Engine-shaped semantic input").next())
        .expect("durable root completion");
    assert!(completion.contains("ComptimeOutcome<V, F>"));
    assert!(completion.contains("DurableComptimeEffects"));
    let production_completion = completion
        .split("#[cfg(test)]")
        .next()
        .expect("durable production completion surface");
    assert!(production_completion.contains("into_parts(self)"));
    assert!(!production_completion.contains("fn outcome("));
    assert!(!production_completion.contains("fn effects("));
    assert!(
        !completion.contains("Clone"),
        "durable completion must remain a one-shot, non-Clone handoff"
    );
    assert!(!facade.contains("impl Clone for DurableComptimeCompletion"));
    assert!(!facade.contains("pub(crate) fn finish_root("));
    assert!(!finish.contains("child: DurableComptimeEffects"));
    assert_eq!(
        production_facade.matches("fn current_effects_mut(").count(),
        1
    );
    let lifecycle_source = production_facade
        .split("impl DurableComptimeCallLifecycle {")
        .nth(1)
        .expect("durable lifecycle implementation");
    for observer in [
        "pub(crate) fn observe_dependency(",
        "pub(crate) fn observe_anonymous_nominal(",
        "pub(crate) fn observe_deferred_ownership(",
    ] {
        let body = lifecycle_source
            .split(observer)
            .nth(1)
            .and_then(|source| source.split("\n    }").next())
            .expect("durable lifecycle observer");
        assert!(
            body.contains("current_effects_mut()"),
            "durable observation bypasses the lifecycle effect scope: {observer}"
        );
    }
    assert!(finish.contains("if matches!(outcome, rue_air::ComptimeOutcome::Known(_))"));
    assert!(
        finish.contains("self.active.pop()")
            && finish.contains("self\n            .scopes\n            .remove(&key)"),
        "finish must consume only an entered lifecycle scope after validation"
    );
    let validate_at = finish
        .find("validate_finish(ticket)")
        .expect("finish validation");
    let consume_at = finish
        .find("ticket.consumed = true")
        .expect("ticket consumption");
    let pop_at = finish.find("self.active.pop()").expect("active pop");
    let context_at = finish
        .find("self\n            .contexts\n            .remove(&key)")
        .expect("context removal");
    let scope_at = finish
        .find("self\n            .scopes\n            .remove(&key)")
        .expect("scope removal");
    let known_at = finish
        .find("if matches!(outcome, rue_air::ComptimeOutcome::Known(_))")
        .expect("known transfer");
    let merge_at = finish
        .find(".merge_child(scope, &context.application_policy)")
        .expect("scope transfer");
    assert!(validate_at < consume_at);
    assert!(consume_at < pop_at && pop_at < context_at && context_at < scope_at);
    assert!(scope_at < known_at && known_at < merge_at);

    let root_authority = database
        .split("struct DurableComptimeRootAuthority<'db> {")
        .nth(1)
        .and_then(|source| {
            source
                .split("}\n\nimpl<'db> DurableComptimeRootAuthority")
                .next()
        })
        .expect("durable root authority definition");
    let root_authority_fields = root_authority
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>();
    assert_eq!(
        root_authority_fields,
        [
            "provider: SemanticNucleusTypeProvider<'db>,",
            "imports: QueryFamily<DeclarationImportQueryKey, DeclarationImportQueryValue>,",
            "session: crate::durable_comptime::DurableComptimeSession,",
            "legacy_effects: crate::durable_comptime::DurableComptimeEffects,",
        ],
        "durable root authority must own exactly the four canonical service fields"
    );
    assert_eq!(
        database
            .matches("impl crate::durable_comptime::DurableComptimeSemanticAuthority")
            .count(),
        1,
        "one root authority must own the durable semantic service implementation"
    );
    assert!(
        !database.contains("struct SemanticComptimeAuthority"),
        "the ephemeral durable semantic authority must stay deleted"
    );
    assert_eq!(
        database
            .matches("let mut authority = DurableComptimeRootAuthority {")
            .count(),
        2,
        "both durable evaluator roots must construct the root authority"
    );
    let root_authority_impl = database
        .split("impl<'db> DurableComptimeRootAuthority<'db> {")
        .nth(1)
        .and_then(|source| source.split("fn project_named_value_candidate").next())
        .expect("durable root authority implementation");
    let drain_at = root_authority_impl
        .find("drain_root_effects()")
        .expect("root authority drains session effects");
    let legacy_merge_at = root_authority_impl
        .find("self.legacy_effects.merge_child(")
        .expect("root authority merges session effects into legacy effects");
    let provider_merge_at = root_authority_impl
        .find("self.provider.merge_comptime_effects(")
        .expect("root authority publishes legacy effects once");
    for operation in [
        "drain_root_effects()",
        "self.legacy_effects.merge_child(",
        "self.provider.merge_comptime_effects(",
    ] {
        assert_eq!(
            root_authority_impl.matches(operation).count(),
            1,
            "root authority finalization must perform `{operation}` exactly once"
        );
    }
    assert!(drain_at < legacy_merge_at && legacy_merge_at < provider_merge_at);

    assert_eq!(
        database.matches("SemanticConstEvaluator {").count(),
        2,
        "durable roots must retain exactly two legacy evaluator constructions"
    );
    assert_eq!(
        database
            .matches("from_const_body_plan_without_imports(")
            .count(),
        1,
        "the const root must materialize one shared owning program core"
    );
    assert_eq!(
        database
            .matches("from_callable_body_plan_without_imports(")
            .count(),
        1,
        "the callable root must materialize one shared owning program core"
    );
    assert_eq!(
        database
            .matches("materialize_semantic_candidate_rir")
            .count(),
        0,
        "durable roots must not rematerialize a second legacy RIR authority"
    );
    let evaluator_roots = database
        .split("let mut evaluator = SemanticConstEvaluator {")
        .skip(1)
        .map(|source| {
            source
                .split("match result")
                .next()
                .expect("legacy evaluator root body")
        })
        .collect::<Vec<_>>();
    assert_eq!(evaluator_roots.len(), 2);
    assert_eq!(
        database
            .matches("finalize_registered_imports(&core)")
            .count(),
        1,
        "const root must finalize its registered import metadata exactly once"
    );
    let finalize_imports_at = database
        .find("finalize_registered_imports(&core)")
        .expect("const root finalizes its registered import metadata");
    let const_eval_at = database
        .find("let result = evaluator.eval(")
        .expect("const root evaluator");
    assert!(
        finalize_imports_at < const_eval_at,
        "const registry must receive finalized imports before evaluation"
    );
    let const_root_start = database
        .find("Key::ConstResolution(query) =>")
        .expect("const evaluator root");
    let const_root_end = database[const_root_start..]
        .find("Key::AnonymousNominal(query) =>")
        .map(|offset| const_root_start + offset)
        .expect("end of const evaluator root");
    assert!(const_root_start < const_root_end);
    let const_root = &database[const_root_start..const_root_end];
    assert!(
        const_root.contains("let declared_type_resolution = declared_type.as_ref().map(|syntax|")
            && const_root.contains("resolve_type_syntax(&program_key, *syntax)")
            && const_root.contains("match declared_type_resolution"),
        "const completion must validate its declared type from the keyed resolution"
    );
    assert_eq!(
        const_root
            .matches("resolve_type_syntax(&program_key, *syntax)")
            .count(),
        1,
        "const completion must reuse one exact keyed declared-type resolution"
    );
    assert!(
        !const_root.contains("resolve_semantic_candidate_type(")
            && !const_root.contains("resolve_structured_semantic_type_syntax_with("),
        "const completion must not re-pair declared syntax with loose evaluator authority"
    );
    assert!(!database.contains("fn resolve_semantic_candidate_type("));
    for root in evaluator_roots {
        assert!(root.contains("authority: &mut authority"));
        for operation in [
            "let result = evaluator.eval(",
            "drop(evaluator)",
            "authority.finish_legacy()",
        ] {
            assert_eq!(
                root.matches(operation).count(),
                1,
                "each legacy root must perform `{operation}` exactly once"
            );
        }
        let eval_at = root.find("let result = evaluator.eval(").unwrap();
        let drop_at = root.find("drop(evaluator)").unwrap();
        let finish_at = root.find("authority.finish_legacy()").unwrap();
        assert!(
            eval_at < drop_at && drop_at < finish_at,
            "each legacy root must evaluate, release its authority borrow, then finalize"
        );
        assert!(!root.contains("evaluator.effects"));
        assert!(!root.contains("drain_root_effects()"));
        assert!(!root.contains("DurableComptimeRootAuthority::new"));
        assert!(!root.contains("next_call:"));
        for forbidden in [
            "DurableComptimeCallLifecycle",
            "DurableComptimeCompletion",
            "complete_root(",
        ] {
            assert!(
                !root.contains(forbidden),
                "legacy root crossed lifecycle seam: {forbidden}"
            );
        }
    }
    let target_kernel = facade
        .split("pub(crate) fn resolve_target_intrinsic_facts")
        .nth(1)
        .and_then(|source| {
            source
                .split("pub(crate) fn resolve_target_enum_variant_facts")
                .next()
        })
        .expect("canonical target intrinsic kernel");
    let target_variant_kernel = facade
        .split("pub(crate) fn resolve_target_enum_variant_facts")
        .nth(1)
        .and_then(|source| {
            source
                .split("#[derive(Debug, Clone, PartialEq, Eq)]")
                .next()
        })
        .expect("canonical target variant kernel");
    for required in [
        "target_arch",
        "target_os",
        "target_data_model",
        "IntrinsicWrongArgCount",
        "unknown target descriptor intrinsic",
    ] {
        assert!(
            target_kernel.contains(required),
            "target intrinsic policy must remain in the canonical kernel: {required}"
        );
    }
    for required in [
        "VARIANTS",
        "unknown target descriptor enum",
        "UnknownVariant",
        "canonical_variant",
    ] {
        assert!(
            target_variant_kernel.contains(required),
            "target variant policy must remain in the canonical kernel: {required}"
        );
    }
    assert_eq!(production_facade.matches("\"target_arch\"").count(), 1);
    assert_eq!(production_facade.matches("\"target_os\"").count(), 1);
    assert_eq!(
        production_facade.matches("\"target_data_model\"").count(),
        1
    );
    let evaluator = database
        .split("impl SemanticConstEvaluator<'_, '_> {")
        .nth(1)
        .and_then(|source| source.split("impl SemanticNucleusTypeProvider<'_>").next())
        .expect("durable evaluator source");
    assert_eq!(
        evaluator
            .matches("project_durable_anonymous_nominal(")
            .count(),
        1,
        "anonymous nominal construction must use the canonical projection kernel"
    );
    assert!(
        !evaluator.contains("DurableAnonymousNominal::new("),
        "the durable evaluator must not duplicate anonymous nominal construction"
    );
    let anonymous_nominal_kernel = production_facade
        .split("pub(crate) fn project_durable_anonymous_nominal(")
        .nth(1)
        .and_then(|source| source.split("fn durable_parameter_mode(").next())
        .expect("canonical anonymous nominal projection kernel");
    for forbidden in [
        "InstData",
        "InstRef",
        "ValidatedRir",
        "SemanticConstEvaluator",
        "program_rir",
        "callback",
        "provider.",
        "self.effects",
    ] {
        assert!(
            !anonymous_nominal_kernel.contains(forbidden),
            "anonymous nominal kernel must not decode RIR, evaluate, or publish effects directly: {forbidden}"
        );
    }
    assert!(evaluator.contains("EvaluateSemanticConstError::comptime_failure_at("));
    let target_authority = database
        .split("impl crate::durable_comptime::DurableComptimeSemanticAuthority")
        .nth(1)
        .and_then(|source| source.split("thread_local! {").next())
        .expect("durable semantic authority source");
    for required in [
        "fn resolve_type_syntax(",
        "program: &crate::body_query::DurableComptimeProgramKey",
        "registered_program(program)",
        "program.declaration.module()",
        "resolve_structured_semantic_type_syntax_with(",
        "resolve_target_intrinsic_facts",
        "resolve_target_enum_variant_facts",
        "map_err(rue_air::SemanticProviderError::Failure)",
    ] {
        assert!(
            target_authority.contains(required),
            "target policy must remain in DurableComptimeRootAuthority: {required}"
        );
    }
    for forbidden in [
        "SemanticConstEvaluator",
        "SEMANTIC_COMPTIME",
        "ComptimeEngine",
        "ValidatedRir",
        "program_rir",
        "child_instructions",
        "InstData",
        "InstRef",
        ".eval(",
        "evaluate",
        "eval_const_expr",
        "SemanticComptimeCallDepthGuard",
        "callback",
    ] {
        assert!(
            !target_authority.contains(forbidden),
            "semantic authority must not become an evaluator: {forbidden}"
        );
    }
    let keyed_type_resolver = target_authority
        .split("fn resolve_type_syntax(")
        .nth(1)
        .and_then(|source| source.split("fn begin_comptime_call_admission(").next())
        .expect("keyed durable type resolver");
    for forbidden in [
        "RirTypeSyntaxArena",
        "arena: &",
        "symbols: &[",
        "module: &ModuleId",
        "RirTypeSyntaxNode",
        ".type_syntax().node",
    ] {
        assert!(
            !keyed_type_resolver.contains(forbidden),
            "keyed type resolver must not accept loose program authority: {forbidden}"
        );
    }
    for policy in [
        "\"target_arch\"",
        "\"target_os\"",
        "\"target_data_model\"",
        "IntrinsicWrongArgCount",
        "UnknownVariant",
        "unknown target descriptor intrinsic",
        "unknown target descriptor enum",
    ] {
        assert!(
            !target_authority.contains(policy),
            "target policy must occur only in the canonical kernel: {policy}"
        );
    }
    assert!(evaluator.contains("DurableComptimeServices::new(&mut *self.authority)"));
    assert!(evaluator.contains("let expected = self.resolve_type_syntax(*annotation)?;"));
    let type_literal = evaluator
        .split("fn eval_type_literal(")
        .nth(1)
        .and_then(|source| source.split("    fn eval(").next())
        .expect("durable type-literal evaluator");
    assert!(
        !type_literal.contains("resolve_semantic_candidate_type("),
        "type literals must not retain a loose module/arena/symbol resolver"
    );
    assert!(
        type_literal.contains("let input =") && type_literal.contains("TypeLiteralInput"),
        "type literals must decode syntax facts before invoking keyed resolution"
    );
    assert!(
        type_literal.contains("ty: self.resolve_type_syntax(ty)?"),
        "anonymous struct fields must use the keyed resolver"
    );
    let method_type = type_literal
        .split("let method_type = |this: &mut Self, ty|")
        .nth(1)
        .and_then(|source| source.split("let parameters = method").next())
        .expect("anonymous method type helper");
    assert_eq!(
        method_type.matches("this.resolve_type_syntax(ty)?").count(),
        1,
        "anonymous method parameters/results must share the keyed type helper"
    );
    assert!(
        type_literal.contains(".map(|ty| self.resolve_type_syntax(ty))"),
        "anonymous enum payloads must use the keyed resolver"
    );
    assert!(
        type_literal.contains("TypeLiteralInput::TypeConst(type_name)")
            && type_literal.contains("self.resolve_type_syntax(type_name)?"),
        "type constants must use the keyed resolver"
    );
    let named_call = evaluator
        .split("fn eval_named_call(")
        .nth(1)
        .and_then(|source| source.split("fn eval_type_literal(").next())
        .expect("durable named-call evaluator");
    assert!(
        named_call.contains("begin_comptime_call_admission("),
        "durable named calls must consume the canonical admission projection"
    );
    let begin = named_call
        .find("begin_comptime_call_admission(")
        .expect("named-call begin admission");
    let observed = named_call
        .find("observe_dependency(admission_start.dependency.clone())")
        .expect("named-call dependency observation");
    let finish = named_call
        .find("finish_comptime_call_admission(admission_start")
        .expect("named-call finish admission");
    assert!(begin < observed && observed < finish);
    for forbidden in [
        ".provider.candidate(",
        ".provider.identity(",
        ".provider.signature(",
        "DeclarationShellQueryKey",
        "query_registered(",
        "DeclarationSignatureProjection::Callable",
        "ConstExprNotSupported",
        "BorrowKeywordMissing",
        "InoutKeywordMissing",
        "UnexpectedCallArgumentMode",
    ] {
        assert!(
            !named_call.contains(forbidden),
            "durable named-call evaluator regained admission policy: {forbidden}"
        );
    }
    assert!(
        named_call.contains("let call_ordinal = self.authority.session.next_call_ordinal();"),
        "durable call ordinals must be allocated before admission"
    );
    assert!(
        named_call.contains("prepare_expression_edge(call_ordinal)"),
        "durable named calls must issue the lifecycle edge before query lookup"
    );
    assert!(
        named_call.contains("finish_ready_expression_edge(edge, value)"),
        "ready comptime projections must publish through the lifecycle edge"
    );
    assert!(
        !named_call.contains("self.authority.legacy_effects.merge_projection("),
        "named calls must not bypass lifecycle-owned ready projection effects"
    );
    let edge = named_call
        .find("prepare_expression_edge(call_ordinal)")
        .expect("named-call expression edge issuance");
    let query = named_call
        .find(".provider\n            .query(query)")
        .expect("named-call semantic query");
    let finish_edge = named_call
        .find("finish_ready_expression_edge(edge, value)")
        .expect("named-call ready edge completion");
    assert!(
        edge < query && query < finish_edge,
        "named-call edge must surround query lookup and ready projection completion"
    );
    let binding_header = production_facade
        .find("pub(crate) struct DurableComptimeBinding {")
        .and_then(|start| production_facade[..start].rfind("#[derive("))
        .expect("durable binding derive header");
    let binding_kernel = production_facade
        .get(binding_header..)
        .and_then(|source| {
            source
                .split("/// Match one already-evaluated durable argument")
                .next()
        })
        .expect("durable incremental binding kernel");
    let binding_start = production_facade
        .find("pub(crate) fn bind_durable_comptime_argument(")
        .expect("durable binding function");
    let binding_policy = production_facade
        .get(binding_start..)
        .and_then(|source| source.split("/// The exact durable projection").next())
        .expect("durable incremental binding policy");
    let value_fit_kernel = production_facade
        .split("pub(crate) fn durable_value_fit_failure(")
        .nth(1)
        .and_then(|source| source.split("pub(crate) fn durable_int_width").next())
        .expect("durable value-fit kernel");
    assert!(
        !binding_kernel.contains("Clone")
            && !production_facade.contains("impl Clone for DurableComptimeBinding")
            && !production_facade.contains("impl Clone for DurableComptimeBoundCall"),
        "durable binding state must remain non-Clone"
    );
    for required in [
        "type_arguments",
        "value_arguments",
        "typed_value_arguments",
        "pub(crate) fn finish(self, result: DurableType)",
        "pub(crate) struct DurableComptimeBoundCall",
    ] {
        assert!(
            binding_kernel.contains(required),
            "durable binding kernel missing {required}"
        );
    }
    for forbidden in ["InstData", "InstRef", "Rir", "ComptimeEngine", "callback"] {
        assert!(
            !binding_kernel.contains(forbidden),
            "durable binding kernel leaked evaluator authority: {forbidden}"
        );
        assert!(
            !binding_policy.contains(forbidden),
            "durable binding policy leaked evaluator authority: {forbidden}"
        );
        assert!(
            !value_fit_kernel.contains(forbidden),
            "durable value-fit kernel leaked evaluator authority: {forbidden}"
        );
    }
    for required in [
        "direct_unit_literal",
        "substitute_durable_generics",
        "durable_type_diagnostic_name",
    ] {
        assert!(
            binding_policy.contains(required),
            "durable binding policy missing {required}"
        );
    }
    for required in [
        "DurableComptimeValueFitFailure",
        "durable_const_fits_type",
        "durable_type_diagnostic_name",
    ] {
        assert!(
            value_fit_kernel.contains(required),
            "durable value-fit kernel missing {required}"
        );
    }
    for forbidden in [
        "DurableComptimeCallLifecycle",
        "DurableComptimeEffects",
        "DurableComptimeServices",
        "QueryAbort",
        "SemanticConstEvaluator",
    ] {
        assert!(
            !binding_policy.contains(forbidden),
            "durable binding policy gained query/effect authority: {forbidden}"
        );
    }
    let binding_calls = named_call
        .matches("bind_durable_comptime_argument(")
        .count();
    assert_eq!(
        binding_calls, 2,
        "durable argument binding must route both parameter paths through one kernel"
    );
    let first_eval = named_call
        .find("let evaluated = self.eval(argument.value)?")
        .expect("durable call evaluates each argument before binding");
    let first_bind = named_call
        .find("bind_durable_comptime_argument(")
        .expect("durable call uses incremental binding kernel");
    assert!(first_eval < first_bind);
    let structured_reducer = database
        .split("fn reduce_comptime_call(")
        .nth(1)
        .and_then(|source| source.split("fn resolve_value_argument(").next())
        .expect("durable structured comptime reducer");
    assert!(structured_reducer.contains("durable_value_fit_failure("));
    assert_eq!(
        database.matches("durable_value_fit_failure(").count(),
        1,
        "durable value-fit policy must have one structured-reducer consumer"
    );
    assert!(evaluator.contains(".resolve_import(&site)"));
    let named_value = evaluator
        .split("fn eval_identifier(")
        .nth(1)
        .and_then(|source| source.split("\n    fn eval_call(").next())
        .expect("durable identifier evaluator");
    assert_eq!(named_value.matches("resolve_named_value(").count(), 1);
    let locals = named_value
        .find("self.locals.get(")
        .expect("identifier lookup keeps locals-first behavior");
    let service = named_value
        .find("resolve_named_value(")
        .expect("identifier lookup uses the named-value service");
    let parts = named_value
        .find("projection.into_parts()")
        .expect("identifier lookup destructures a successful projection");
    let observed = named_value
        .find("self.authority.legacy_effects.observe_dependency(dependency)")
        .expect("identifier lookup observes its direct dependency");
    assert!(locals < service && service < parts && parts < observed);
    assert!(named_value.contains("let source = self.authority.provider.dependency_source.clone()"));
    for forbidden in [
        "DefinitionKind::Const",
        "DefinitionKind::Function",
        "DefinitionKind::Struct",
        "DefinitionKind::Enum",
        "SemanticDeclarationDependency {",
        "fn resolve_candidate(",
        "fn resolve_identity(",
        "fn resolve_const(",
    ] {
        assert!(
            !named_value.contains(forbidden),
            "named-value evaluator retained local lookup policy: {forbidden}"
        );
    }
    assert!(evaluator.contains(".resolve_target_intrinsic("));
    assert!(evaluator.contains(".resolve_target_enum_variant("));
    let field_get = evaluator
        .split("E::FieldGet { base, field }")
        .nth(1)
        .and_then(|source| source.split("E::StructInit").next())
        .expect("durable field-get evaluator");
    assert_eq!(field_get.matches("resolve_module_member(").count(), 1);
    let target_special = field_get
        .find("if let E::VarRef { name, .. }")
        .expect("field-get target descriptor special case");
    let base_eval = field_get
        .find("let EvaluatedSemanticConst::Module(module) = self.eval(*base)?")
        .expect("field-get evaluates its base after target descriptors");
    let non_module = field_get
        .find("member access on a non-module const value")
        .expect("field-get non-module rejection");
    let service = field_get
        .find("resolve_module_member(")
        .expect("field-get module-member service");
    let field_projection = field_get
        .find("projection.into_parts()")
        .expect("field-get destructures projection");
    let field_observation = field_get
        .find("self.authority.legacy_effects.observe_dependency(dependency)")
        .expect("field-get observes direct dependency");
    assert!(
        target_special < base_eval
            && base_eval < non_module
            && non_module < service
            && service < field_projection
            && field_projection < field_observation
    );
    for forbidden in [
        ".provider.candidate(",
        ".provider.identity(",
        ".provider.const_resolution(",
        "DefinitionKind::Const",
        "DefinitionKind::Struct",
        "DefinitionKind::Enum",
        "DefinitionKind::Function",
        "SemanticDeclarationDependency {",
    ] {
        assert!(
            !field_get.contains(forbidden),
            "field-get evaluator retained module-member policy: {forbidden}"
        );
    }
    let admission_authority = target_authority
        .split("fn begin_comptime_call_admission(")
        .nth(1)
        .and_then(|source| {
            source
                .split("\n    fn finish_comptime_call_admission(")
                .next()
        })
        .expect("call admission authority source");
    assert!(admission_authority.contains("candidate_from("));
    assert!(admission_authority.contains("accessing_source"));
    assert!(admission_authority.contains("source: accessing_source.clone()"));
    assert!(
        !admission_authority.contains("provider.dependency_source"),
        "call admission must not use root-fixed provider source"
    );
    let admission_finish_authority = target_authority
        .split("fn finish_comptime_call_admission(")
        .nth(1)
        .and_then(|source| source.split("\n    fn resolve_named_value(").next())
        .expect("call admission completion authority source");
    assert!(!admission_finish_authority.contains("provider.dependency_source"));
    let named_value_authority = format!(
        "{}{}",
        target_authority
            .split("fn resolve_named_value(")
            .nth(1)
            .and_then(|source| source.split("\n    fn resolve_import(").next())
            .expect("named-value authority source"),
        database
            .split("fn project_named_value_candidate(")
            .nth(1)
            .and_then(|source| {
                source
                    .split("\nimpl crate::durable_comptime::DurableComptimeSemanticAuthority")
                    .next()
            })
            .expect("named-value projection kernel source")
    );
    for required in [
        "resolve_named_value_in_order",
        "resolve_module_member_in_order",
        "UnknownModuleMember",
        "DefinitionKind::Const",
        "DefinitionKind::Function",
        "DefinitionKind::Struct",
        "DefinitionKind::Enum",
        "candidate_from(",
        "const_resolution(",
        "identity(",
        "source: accessing_source.clone()",
        "DurableComptimeNamedValueProjection::new",
    ] {
        assert!(
            named_value_authority.contains(required),
            "named-value policy missing from authority: {required}"
        );
    }
    assert_eq!(named_value_authority.matches("candidate_from(").count(), 3);
    assert_eq!(
        named_value_authority
            .matches("identity(candidate)?")
            .count(),
        2
    );
    assert!(named_value_authority.contains("const_resolution(candidate)?"));
    assert!(named_value_authority.contains("DurableComptimeNamedValueKind::Const"));
    assert!(named_value_authority.contains("DurableComptimeNamedValueKind::Function"));
    assert!(named_value_authority.contains("DurableComptimeNamedValueKind::Struct"));
    assert!(named_value_authority.contains("DurableComptimeNamedValueKind::Enum"));
    let named_value_kernel = production_facade
        .split("const DURABLE_COMPTIME_UNQUALIFIED_VALUE_KINDS")
        .nth(1)
        .and_then(|source| {
            source
                .split("impl DurableComptimeNamedValueProjection")
                .next()
        })
        .expect("named-value ordering kernel");
    let kind_positions = [
        "DurableComptimeNamedValueKind::Const",
        "DurableComptimeNamedValueKind::Function",
        "DurableComptimeNamedValueKind::Struct",
        "DurableComptimeNamedValueKind::Enum",
    ]
    .map(|kind| {
        named_value_kernel
            .find(kind)
            .unwrap_or_else(|| panic!("named-value kernel missing {kind}"))
    });
    assert!(kind_positions.windows(2).all(|pair| pair[0] < pair[1]));
    let module_value_kernel = production_facade
        .split("const DURABLE_COMPTIME_MODULE_MEMBER_KINDS")
        .nth(1)
        .and_then(|source| {
            source
                .split("/// Run the canonical named-value candidate order")
                .next()
        })
        .expect("module-member ordering kernel");
    let module_kind_positions = [
        "DurableComptimeNamedValueKind::Const",
        "DurableComptimeNamedValueKind::Struct",
        "DurableComptimeNamedValueKind::Enum",
        "DurableComptimeNamedValueKind::Function",
    ]
    .map(|kind| {
        module_value_kernel
            .find(kind)
            .unwrap_or_else(|| panic!("module-member kernel missing {kind}"))
    });
    assert!(
        module_kind_positions
            .windows(2)
            .all(|pair| pair[0] < pair[1])
    );
    for forbidden in [
        "InstData",
        "InstRef",
        "ComptimeEngine",
        "SemanticConstEvaluator",
        "callback",
        "query_demand",
        ".eval(",
    ] {
        assert!(
            !named_value_authority.contains(forbidden),
            "named-value authority leaked evaluator surface: {forbidden}"
        );
    }
    for forbidden in [
        "self.authority.provider.configuration.target",
        "rue_target::Arch",
        "rue_target::Os",
        "rue_target::DataModel",
        "IntrinsicWrongArgCount",
        "UnknownVariant",
        "unknown target descriptor intrinsic",
        "unknown target descriptor enum",
    ] {
        assert!(
            !evaluator.contains(forbidden),
            "durable evaluator regained target policy beside service calls: {forbidden}"
        );
    }
    assert!(evaluator.contains("self.authority.legacy_effects.observe_dependency("));
    assert!(!evaluator.contains("self.authority.legacy_effects.merge_projection("));
    for forbidden in [
        "self.authority.provider.dependencies",
        "self.authority.provider.anonymous_nominals",
        "self.authority.provider.deferred_ownership",
        "value.anonymous_nominals.iter().cloned().map",
        "value.dependencies.iter().cloned().map",
        "value.deferred_ownership.iter().cloned().map",
    ] {
        assert!(
            !evaluator.contains(forbidden),
            "durable evaluator must publish effects through DurableComptimeEffects: {forbidden}"
        );
    }
    assert!(database.contains("provider.merge_comptime_effects("));
    let provider = database
        .split("pub(crate) struct CompilerBodyFactProvider<'a>")
        .nth(1)
        .and_then(|source| source.split("impl rue_air::BodyFactProvider").next())
        .expect("body fact provider source");
    let provider_probe = provider
        .split("pub(crate) fn probe_comptime_call(")
        .nth(1)
        .and_then(|source| source.split("\n    fn nucleus(").next())
        .expect("broad provider probe adapter");
    assert!(provider_probe.contains("DurableComptimeForeignQueryAuthority {"));
    assert!(
        provider_probe.contains("DurableComptimeServices::new(&mut authority).probe_comptime_call")
    );
    for forbidden in [
        "join_registered_noncomputing(",
        "query_registered(",
        "OwnedForeignComptimeProgram::from_body_plan",
    ] {
        assert!(
            !provider_probe.contains(forbidden),
            "broad provider must not retain foreign probe policy: {forbidden}"
        );
    }
    assert!(provider.contains("DurableComptimeServices::new(&mut authority).probe_comptime_call"));
}

#[test]
fn import_resolution_remains_discovery_owned() {
    let production = PRODUCTION_MODULES
        .iter()
        .map(|(_, source)| *source)
        .collect::<String>();
    assert_eq!(
        production.matches("pub struct ImportDirective {").count(),
        1,
        "the compiler must declare exactly one import-site representation"
    );
    assert_eq!(
        production
            .matches("pub enum CanonicalImportResolution {")
            .count(),
        1,
        "the compiler must declare exactly one canonical resolution outcome"
    );
    for removed in [
        ["ParsedImport", "Directive"].concat(),
        ["ParsedImport", "Site"].concat(),
        ["SemanticResolved", "Import"].concat(),
        ["SemanticModule", "Identity"].concat(),
        ["pub enum Import", "Resolution"].concat(),
        ["resolve_", "import_graph"].concat(),
        ["resolve_canonical_", "import_graph"].concat(),
        ["extract_import_", "directives"].concat(),
        ["Dir", "Resolution"].concat(),
    ] {
        assert!(
            !production.contains(&removed),
            "retired import representation or resolver returned: {removed}"
        );
    }

    let discovery = PRODUCTION_MODULES
        .iter()
        .find_map(|(name, source)| (*name == "import_discovery").then_some(*source))
        .unwrap();
    assert!(discovery.contains("pub struct ImportDiscoveryPlan"));
    assert!(discovery.contains("CanonicalImportGraph"));

    let compiler_import_policy = PRODUCTION_MODULES
        .iter()
        .filter(|(name, _)| {
            matches!(
                *name,
                "parsed_modules"
                    | "import_discovery"
                    | "import_graph"
                    | "bound_definitions"
                    | "canonical_semantic"
                    | "session"
            )
        })
        .map(|(_, source)| *source)
        .collect::<String>();
    for forbidden_probe in [
        ["std::", "fs"].concat(),
        ["fs", "::"].concat(),
        ["std::", "env"].concat(),
        ["RUE_STD", "_PATH"].concat(),
        ["canonicalize", "("].concat(),
        [".exists", "("].concat(),
    ] {
        assert!(
            !compiler_import_policy.contains(&forbidden_probe),
            "compiler import policy must not probe host state: {forbidden_probe}"
        );
    }

    let downstream_imports = PRODUCTION_MODULES
        .iter()
        .filter(|(name, _)| {
            matches!(
                *name,
                "parsed_modules"
                    | "import_graph"
                    | "bound_definitions"
                    | "canonical_semantic"
                    | "session"
            )
        })
        .map(|(_, source)| *source)
        .collect::<String>();
    for forbidden_policy in [
        ["candidate_", "groups"].concat(),
        ["resolve_explicit_", "candidates"].concat(),
    ] {
        assert!(
            !downstream_imports.contains(&forbidden_policy),
            "downstream compiler import consumer must not rediscover: {forbidden_policy}"
        );
    }
}
