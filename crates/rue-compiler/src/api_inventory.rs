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
    (
        "revisioned_query_database",
        include_str!("revisioned_query_database.rs"),
    ),
    ("semantic_symbols", include_str!("semantic_symbols.rs")),
    ("session", include_str!("session.rs")),
    ("source_identity", include_str!("source_identity.rs")),
    ("source_metadata", include_str!("source_metadata.rs")),
    ("source_snapshot", include_str!("source_snapshot.rs")),
    ("syntax", include_str!("syntax.rs")),
    ("typed_query_store", include_str!("typed_query_store.rs")),
    ("unstable", include_str!("unstable.rs")),
];

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
            if root || !matches!(name, "thread_local" | "session_query_metrics_family") {
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
            "AcceptedReadManifestEntry"
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
            "durable_compatibility_tests",
            "integration_tests",
            "pipeline_tests",
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
fn revisioned_parse_family_has_no_peer_legacy_authority() {
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
    assert!(session.contains("revisioned.source_revision(&source)"));
    assert!(session.contains("revisioned.select_parse(&attempt)"));
    assert!(runtime.contains("parse: RevisionedFamily<super::session::ParseQuery>"));
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
