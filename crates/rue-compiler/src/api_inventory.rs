//! RUE-736 review gate for the curated compiler facade.

const PRODUCTION_MODULES: &[(&str, &str)] = &[
    ("backend", include_str!("backend.rs")),
    ("bound_definitions", include_str!("bound_definitions.rs")),
    ("canonical_lower", include_str!("canonical_lower.rs")),
    ("canonical_merge", include_str!("canonical_merge.rs")),
    ("canonical_semantic", include_str!("canonical_semantic.rs")),
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
    ("parsed_modules", include_str!("parsed_modules.rs")),
    ("queries", include_str!("queries.rs")),
    ("query_graph", include_str!("query_graph.rs")),
    ("semantic_symbols", include_str!("semantic_symbols.rs")),
    ("session", include_str!("session.rs")),
    ("source_identity", include_str!("source_identity.rs")),
    ("source_metadata", include_str!("source_metadata.rs")),
    ("source_snapshot", include_str!("source_snapshot.rs")),
    ("syntax", include_str!("syntax.rs")),
    ("typed_query_store", include_str!("typed_query_store.rs")),
    ("unstable", include_str!("unstable.rs")),
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
    "SemanticDependencyBlocker",
    "SemanticDependencyIncompleteReason",
    "SemanticDependencyInputManifest",
    "SemanticDependencyManifestWork",
    "SemanticDependencySurface",
    "SemanticFullInvalidationReason",
    "SemanticInvalidationPlan",
    "SemanticInvalidationScope",
    "SemanticInvalidationWork",
    "StableBodyDependencyInputRecord",
    "StableBuiltinTypeCallHeadInput",
    "StableDeclarationTypeCallHeadDependency",
    "StableDeclarationTypeDependency",
    "StableDefinitionFingerprint",
    "StableDefinitionFingerprintPrecision",
    "StableDefinitionInputFingerprint",
    "StableFreeFunctionDependency",
    "StableModuleImportDependency",
    "StableNamedConstDependency",
    "StableNamedConstDependencyTarget",
    "StableNamedDestructorDependency",
    "StableNamedMethodDependency",
    "StableNamedMethodDependencyTarget",
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
    "ParsedAstPresentationWork",
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
    "DURABLE_ORDINARY_BODY_SCHEMA_VERSION",
    "DURABLE_SPECIALIZED_BODY_SCHEMA_VERSION",
    "DurableAirInst",
    "DurableAirInstData",
    "DurableAirRef",
    "DurableBodyAnchor",
    "DurableBodyConversionFailure",
    "DurableBodyProjectionFailure",
    "DurableBodyWork",
    "DurableCallArg",
    "DurableMatchArm",
    "DurableOrdinaryBody",
    "DurableOrdinaryBodyPayload",
    "DurablePattern",
    "DurablePlace",
    "DurablePlaceRef",
    "DurableProjection",
    "DurableSpecializedBody",
    "DurableSpecializedBodyPayload",
    "convert_semantic_specialized_body_exports",
    "DURABLE_SEMANTIC_SCHEMA_VERSION",
    "DurableConstValue",
    "DurableDeclarationPayload",
    "DurableDeclarationSemantic",
    "DurableParameterMode",
    "DurableSemanticExportFailure",
    "DurableSemanticImportEpoch",
    "DurableSemanticParameter",
    "DurableSemanticProjectionFailure",
    "DurableSemanticProjectionWork",
    "DurableSemanticSchemaVersion",
    "DurableType",
];

fn public_declarations(source: &str) -> Vec<String> {
    let mut declarations = Vec::new();
    let mut lines = source.lines();
    while let Some(line) = lines.next() {
        let trimmed = line.trim_start();
        if !trimmed.starts_with("pub ") || trimmed.starts_with("pub(crate)") {
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
        let mut declaration = String::new();
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
    assert!(
        facade.lines().count() <= 260,
        "rue-compiler's facade grew beyond its reviewed inventory"
    );

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
            "durable_compatibility_tests",
            "integration_tests",
            "pipeline_tests",
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
fn orphaned_backend_inspection_exports_cannot_return() {
    let facade = include_str!("lib.rs");
    let backend = include_str!("backend.rs");
    let removed = ["generate_allocated_mir"];

    for name in removed {
        assert!(
            !code_identifiers(facade).contains(&name),
            "test-only backend helper returned to the production facade: {name}"
        );
        assert!(
            !code_identifiers(backend).contains(&name),
            "orphaned backend inspection path returned: {name}"
        );
    }
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
    assert!(
        !unstable.contains("pub use crate::"),
        "unstable views must own their projections instead of aliasing compiler records"
    );
    assert!(!unstable.contains("pub type "));
    assert!(!unstable.contains("pub use "));

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
    let manifest_public =
        public_signatures(inherent_impl(session, "SemanticDependencyInputManifest"));
    let semantic_public = public_signatures(inherent_impl(
        include_str!("canonical_semantic.rs"),
        "CanonicalSemanticOutput",
    ));
    for raw_accessor in [
        "durable_specialized_body_payloads",
        "durable_ordinary_bodies",
    ] {
        assert!(
            !manifest_public.contains(&format!("pub fn {raw_accessor}"))
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
    assert!(
        durable_body
            .contains("pub body: rue_air::SemanticBody<StableDefinitionKey, crate::ModuleId>"),
        "durable envelope must retain rue-air's canonical body algebra directly"
    );
    assert!(
        durable_body.contains(
            "rue_air::SemanticSpecializationIdentity<StableDefinitionKey, crate::ModuleId>"
        ),
        "specialized envelopes must retain rue-air's canonical identity directly"
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
        ["Module", "Path"].concat(),
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
