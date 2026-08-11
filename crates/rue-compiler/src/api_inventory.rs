//! Semantic review gate for the curated compiler facade.
//!
//! RUE-869 replaces the old source-line budget with an exact inventory of
//! every root export and every public `CompilerSession`/
//! `CompilerSessionUpdate` signature. The inventory records ownership,
//! stability, category, and approved consumers so intentional API changes are
//! reviewed as semantic one-line diffs.

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
    ("durable_body", include_str!("durable_body.rs")),
    ("durable_cfg", include_str!("durable_cfg.rs")),
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
        "rue_air::SemanticImportEpoch::new_local(",
        ".materialize_local_body_with_types(",
        "FunctionInstanceKey::AnonymousMember",
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
        "rules::accessor_preview_gate(",
        "rules::accessor_signature(",
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
    let database = include_str!("revisioned_query_database.rs");
    for required in [
        "CfgSemanticInput::Body",
        "materialize_canonical_body(",
        "materialize_semantic_body(",
        "synthesize_canonical_drop_glue(",
        "CfgDomainProjection::from_local_body(",
        "pub(crate) type_pool: rue_air::FrozenTypeInternPool",
        "pub(crate) interner: Arc<lasso::ThreadedRodeo>",
        "pub(crate) local_atoms:",
        "&record.type_pool",
    ] {
        assert!(
            cfg.contains(required),
            "CFG ownership boundary lost: {required}"
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
    let runtime = include_str!("revisioned_query_database.rs");
    let projector = source_between_exact_boundaries(
        runtime,
        "fn warning_static_call_heads(",
        "\nstruct WarningStaticCallCollector",
    );
    for forbidden in [
        "lower_owned_body_input(",
        "lower_module_rir",
        "OwnedBodyInput",
        "ModuleRir",
    ] {
        assert!(
            !projector.contains(forbidden),
            "warning AST projector crossed into RIR/body lowering: {forbidden}"
        );
    }
    for required in [
        ".definitions()",
        ".declaration_locator(candidate)",
        "module.ast().items",
    ] {
        assert!(
            projector.contains(required),
            "warning AST projector lost exact parsed-candidate selection: {required}"
        );
    }

    let evaluator = source_between_exact_boundaries(
        runtime,
        "let parse_for_warning_syntax = parse_modules.clone();",
        "        // Body analysis is a canonical registered evaluator.",
    );
    for forbidden in [
        "raw_declaration_bodies",
        "raw_declaration_signatures",
        "body_inputs",
        "body_transactions",
        "module_rirs",
        "lower_owned_body_input",
        "lower_module_rir",
    ] {
        assert!(
            !evaluator.contains(forbidden),
            "warning query family crossed into RIR/body analysis: {forbidden}"
        );
    }
    for required in [
        "compiler.warning-body-syntax",
        "parse_for_warning_syntax",
        "WarningBodySyntaxQueryKey",
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
    "DurableAirInstData",
    "DurableBodyWork",
    "DurableProjection",
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
            "AcceptedImportSource"
            | "ImportDiscoveryPlan"
            | "ImportDiscoveryRequest"
            | "ImportObservation"
            | "ImportObservationLedger"
            | "ImportObservationStatus" => ("compatibility-boundary", "legacy-embedders"),
            "AcceptedReadManifest"
            | "AcceptedReadManifestEntry"
            | "FileMetadataFingerprint"
            | "ImportCandidateRole"
            | "ImportDiscoveryContext"
            | "ImportOccurrenceKey"
            | "PhysicalFileIdentity" => ("dependency-artifact", "source-loaders+embedders"),
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
            "downstream_invalidated" => "session-status",
            _ => panic!("unclassified stable CompilerSessionUpdate method: {symbol}"),
        }
    } else {
        match symbol {
            "new" | "update" => "session-operation",
            "import_discovery_plan" | "stage_import_discovery" | "close_import_discovery" => {
                return ("stable", "compatibility-boundary", "legacy-embedders");
            }
            "published"
            | "committed_import_graph"
            | "import_diagnostics"
            | "import_graph"
            | "rir"
            | "semantic"
            | "executable" => "artifact-query",
            "latest_diagnostics"
            | "latest_successful_diagnostics"
            | "last_good_semantic_diagnostics" => "diagnostic-query",
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
    for family in [
        "compiler.body-transaction",
        "compiler.canonical-body",
        "compiler.body-references",
    ] {
        assert!(
            runtime.contains(family),
            "missing independent family {family}"
        );
    }
    let semantic_entry = item(
        include_str!("canonical_semantic.rs"),
        "pub struct CanonicalSemanticOutput",
    );
    assert!(!semantic_entry.contains("successful_body_cache"));
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
        "SemanticView",
        "FunctionView",
        "CfgBlockView",
        "CfgInstructionView",
        "CfgSuccessorView",
        "CfgView",
        "SourceIdentityView",
        "SourceLocationView",
        "TypeView",
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
        "CfgInstructionView",
        "CfgSuccessorView",
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
    ];
    for removed in forbidden {
        assert!(
            !production.contains(&removed),
            "removed compiler entry point returned: {removed}"
        );
    }
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
    assert!(
        code_identifiers(unstable).contains(&"codegen_products"),
        "unstable presentation must project the canonical CodegenUnit terminal"
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
            "pubusecrate::import_discovery::{DiscoverySourceAssembler,ImportDemandFrontier,ImportDemandMode,ImportDemandRoots,ImportInputRevision,};",
            "pubusecrate::session::{ClosedDiscoveryContinuation,RootedParkOutcome,SemanticParkOutcome,TrustedSuccessorDelta,};",
        ],
        "unstable may reexport only reviewed presentation, source-assembly, and Phase-2 demand helpers"
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
        public_signatures(inherent_impl(
            include_str!("canonical_semantic.rs"),
            "CanonicalSemanticOutput",
        )),
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
    assert!(facade.contains("mod durable_body;"));
    assert!(facade.contains("mod durable_semantics;"));
    assert!(!facade.contains("pub mod durable_body;"));
    assert!(!facade.contains("pub mod durable_semantics;"));

    let session = include_str!("session.rs");
    let semantic_public = public_signatures(inherent_impl(
        include_str!("canonical_semantic.rs"),
        "CanonicalSemanticOutput",
    ));
    for raw_accessor in [
        "durable_specialized_body_payloads",
        "durable_ordinary_bodies",
    ] {
        assert!(
            !session.contains(raw_accessor)
                && !semantic_public.contains(&format!("pub fn {raw_accessor}")),
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
                ".evaluate_raw_const_syntax(",
                ".evaluate_raw_declaration_signature(",
                ".evaluate_raw_declaration_body(",
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
            "declaration_candidate"
                | "parsed_modules"
                | "revisioned_query_database"
                | "semantic_query_nucleus"
        ) {
            assert!(
                !source.contains("RawConstSyntax"),
                "compiler production module {name} escaped the raw-constant authority allowlist"
            );
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
                "compiler production module {name} escaped the raw-signature authority allowlist"
            );
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
                !code_identifiers(source).contains(&"RawDeclarationBody"),
                "compiler production module {name} escaped the raw-body authority allowlist"
            );
        }
        if !matches!(
            *name,
            "declaration_candidate"
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
    assert!(canonical.contains("predeclare_imported_declaration_shells"));
    assert!(!canonical.contains("fn analyze_canonical_program("));
    assert_eq!(runtime.matches(".evaluate_declaration_shell(").count(), 1);
    assert_eq!(parsed.matches("fn evaluate_declaration_shell(").count(), 1);
    assert_eq!(runtime.matches(".evaluate_raw_const_syntax(").count(), 1);
    assert_eq!(parsed.matches("fn evaluate_raw_const_syntax(").count(), 1);
    assert_eq!(runtime.matches("\"compiler.raw-const-syntax\"").count(), 1);
    assert_eq!(parsed.matches("RawConstSyntax {").count(), 1);
    assert_eq!(
        runtime
            .matches(".evaluate_raw_declaration_signature(")
            .count(),
        1
    );
    assert_eq!(
        parsed
            .matches("fn evaluate_raw_declaration_signature(")
            .count(),
        1
    );
    assert_eq!(
        runtime
            .matches("\"compiler.raw-declaration-signature\"")
            .count(),
        1
    );
    assert_eq!(parsed.matches("RawDeclarationSignatureSyntax {").count(), 1);
    assert_eq!(
        runtime.matches(".evaluate_raw_declaration_body(").count(),
        1
    );
    assert_eq!(
        parsed.matches("fn evaluate_raw_declaration_body(").count(),
        1
    );
    assert_eq!(
        runtime.matches("\"compiler.raw-declaration-body\"").count(),
        1
    );
    assert_eq!(parsed.matches("RawDeclarationBodySyntax {").count(), 1);
    assert_eq!(runtime.matches(".declaration_import(").count(), 1);
    assert_eq!(parsed.matches("fn declaration_import(").count(), 1);
    assert_eq!(
        runtime.matches("\"compiler.declaration-import\"").count(),
        1
    );
    assert!(!runtime.contains("Vec::remove"));

    let raw_body_evaluator = runtime
        .split("let occurrences_for_raw_body")
        .nth(1)
        .and_then(|tail| tail.split("let index_for_lookup").next())
        .unwrap();
    for forbidden in [
        "module_rirs",
        "lower_module_rir",
        "CanonicalMergedProgram",
        "SemanticView",
    ] {
        assert!(
            !raw_body_evaluator.contains(forbidden),
            "raw-body syntax evaluator gained a broad compiler dependency: {forbidden}"
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
        .and_then(|tail| tail.split("struct RawConstSyntaxQueryKey").next())
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
    let raw_const_terminal = runtime
        .split("enum RawConstSyntaxQueryValue")
        .nth(1)
        .and_then(|tail| tail.split("struct RawDeclarationSignatureQueryKey").next())
        .unwrap();
    for forbidden in [
        "CompileErrors",
        "Span",
        "FileId",
        "Spur",
        "InstRef",
        "Rir",
        "SemanticDefinitionToken",
        "SemanticModuleToken",
    ] {
        assert!(
            !raw_const_terminal.contains(forbidden),
            "raw-constant terminal regained positioned/live parser or semantic payload: {forbidden}"
        );
    }
    let raw_signature_terminal = runtime
        .split("enum RawDeclarationSignatureQueryValue")
        .nth(1)
        .and_then(|tail| tail.split("struct RawDeclarationBodyQueryKey").next())
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
            !raw_signature_terminal.contains(forbidden),
            "raw-signature terminal regained positioned/live parser or semantic payload: {forbidden}"
        );
    }
    let raw_body_terminal = runtime
        .split("enum RawDeclarationBodyQueryValue")
        .nth(1)
        .and_then(|tail| {
            tail.split("pub(crate) enum DeclarationShellBatchFailure")
                .next()
        })
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
            !raw_body_terminal.contains(forbidden),
            "raw-body terminal regained positioned/live parser or semantic payload: {forbidden}"
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
        .and_then(|tail| tail.split("struct ImportHostOperationKey").next())
        .unwrap();
    for forbidden in ["CompileErrors", "ModuleRevision", "Span", "FileId", "Spur"] {
        assert!(
            !lookup_value.contains(forbidden),
            "LookupName terminal regained locator/live payload: {forbidden}"
        );
    }
    let raw_const_payload = module("declaration_candidate")
        .split("pub(crate) struct RawConstSyntax")
        .nth(1)
        .and_then(|tail| tail.split("pub(crate) enum RawConstSyntaxFailure").next())
        .unwrap();
    for forbidden in ["Span", "FileId", "Spur", "Ast", "Rir", "InstRef"] {
        assert!(
            !raw_const_payload.contains(forbidden),
            "raw-constant payload regained a positioned or live parser handle: {forbidden}"
        );
    }
    let raw_signature_payload = module("declaration_candidate")
        .split("pub(crate) struct RawDeclarationSignatureSyntax")
        .nth(1)
        .and_then(|tail| {
            tail.split("pub(crate) enum RawDeclarationSignatureFailure")
                .next()
        })
        .unwrap();
    for forbidden in ["Span", "FileId", "Spur", "Ast", "Rir", "InstRef", "TypeId"] {
        assert!(
            !raw_signature_payload.contains(forbidden),
            "raw-signature payload regained a positioned or live parser/semantic handle: {forbidden}"
        );
    }
    let raw_body_payload = module("declaration_candidate")
        .split("pub(crate) struct RawDeclarationBodySyntax")
        .nth(1)
        .and_then(|tail| {
            tail.split("pub(crate) enum RawDeclarationBodyFailure")
                .next()
        })
        .unwrap();
    for forbidden in ["Span", "FileId", "Spur", "Ast", "Rir", "InstRef", "TypeId"] {
        assert!(
            !raw_body_payload.contains(forbidden),
            "raw-body payload regained a positioned or live parser/semantic handle: {forbidden}"
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
    let raw_signature_locator = parsed
        .split("enum RawDeclarationSignatureLocator")
        .nth(1)
        .and_then(|tail| {
            tail.split("pub(crate) struct ParsedDeclarationLocator")
                .next()
        })
        .unwrap();
    for forbidden in ["Vec<", "Arc<[Span]>", "Box<"] {
        assert!(
            !raw_signature_locator.contains(forbidden),
            "raw-signature parser locator regained per-declaration heap storage: {forbidden}"
        );
    }
    for required in ["Contiguous", "SplitStruct", "Extern"] {
        assert!(
            raw_signature_locator.contains(required),
            "raw-signature parser locator lost bounded inline shape {required}"
        );
    }
    let declaration_import_locator = parsed
        .split("struct RawDeclarationImportRange")
        .nth(1)
        .and_then(|tail| {
            tail.split("pub(crate) enum ParsedDeclarationImportFailure")
                .next()
        })
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
        1,
        "only canonical query preparation may import query-owned shells"
    );
    for (name, source) in &production {
        if *name == "canonical_semantic" {
            continue;
        }
        let production_source = if *name == "bound_definitions" {
            source
                .split(
                    "\n#[cfg(test)]\npub(crate) fn compare_canonical_durable_declaration_install",
                )
                .next()
                .unwrap()
        } else {
            source
        };
        assert!(
            !production_source.contains("predeclare_imported_declaration_shells"),
            "compiler production module {name} gained a peer shell importer"
        );
    }
    assert!(!session.contains("DeclarationShellFact"));
}

#[test]
fn canonical_semantic_body_has_no_compiler_owned_peer_algebra() {
    let durable_body = include_str!("durable_body.rs");
    for removed in [
        "pub enum DurableAirInstData",
        "pub struct DurableAirInst",
        "pub struct DurablePlace",
        "pub enum DurableProjection",
        "pub enum DurablePattern",
        "pub struct DurableBodyAnchor",
        "pub struct DurableSpecializationIdentity",
    ] {
        assert!(
            !durable_body.contains(removed),
            "compiler-owned canonical body mirror returned: {removed}"
        );
    }
    assert!(durable_body.contains("pub type DurableProjection"));
    assert!(durable_body.contains("pub type DurableAirInstData"));
    for removed in [
        "DurableOrdinaryBodyPayload",
        "DurableSpecializedBodyPayload",
        "convert_semantic_body_exports",
        "convert_semantic_specialized_body_exports",
    ] {
        assert!(
            !durable_body.contains(removed),
            "removed peer body-export payload returned: {removed}"
        );
    }
}

#[test]
fn rue_1027_production_body_authority_is_query_owned_and_import_only() {
    let canonical = include_str!("canonical_semantic.rs");
    let session = include_str!("session.rs");
    let database = include_str!("revisioned_query_database.rs");
    let body_query = include_str!("body_query.rs");

    let finish = canonical
        .split("fn finish_canonical_analysis(")
        .nth(1)
        .and_then(|source| source.split("fn recover_declaration_failure(").next())
        .expect("canonical finalization body");
    assert!(finish.contains(".compose_queried_bodies("));
    assert!(!finish.contains("analyze_all_bodies"));
    assert!(!finish.contains("if no_queried_candidates"));
    assert_eq!(finish.matches("compose_queried_bodies").count(), 1);
    let recovery = canonical
        .split("fn recover_declaration_failure(")
        .nth(1)
        .and_then(|source| source.split("\n#[cfg(test)]\nmod tests").next())
        .expect("declaration recovery body");
    assert!(!recovery.contains("analyze_all_bodies"));
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
        "compiler.canonical-body",
        "compiler.body-references",
        "compiler.body-produced-anonymous",
    ] {
        assert!(
            database.contains(family),
            "missing RUE-1027 family: {family}"
        );
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
