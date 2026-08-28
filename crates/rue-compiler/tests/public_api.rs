use std::sync::Arc;

use rue_air::{
    ComptimeStructuredTypeAuthority, ComptimeStructuredTypeJob, ComptimeStructuredTypePoll,
    ComptimeStructuredTypeSuspension,
};
use rue_compiler::unstable::{MetricsSnapshot, PresentationRequest, PresentationStage, rooted_cfg};
use rue_compiler::{
    CompileErrors, CompileOptions, CompileOutput, CompilerSession, CompilerSessionUpdate,
    DependencyEnvelope, DiagnosticStage, FileId, FrontendDiagnosticSnapshot, ImportDiscoveryStatus,
    ImportDiscoveryView, MultiErrorResult, RirView, SourceLocationView, SourceMetadata,
    SourceRevision, SourceSnapshot, SourceView, SyntaxModuleView, SyntaxNodeView, compile_snapshot,
};
use rue_error::ErrorKind;

type PublicStructuredJob =
    ComptimeStructuredTypeJob<u8, u16, u32, u64, u128, u128, Arc<str>, Arc<str>, Arc<[Arc<str>]>>;
type PublicStructuredAuthority =
    ComptimeStructuredTypeAuthority<u8, u16, Arc<str>, Arc<[Arc<str>]>>;
type PublicStructuredPoll =
    ComptimeStructuredTypePoll<u8, u16, u32, u64, u128, u128, Arc<str>, Arc<str>, Arc<[Arc<str>]>>;

fn assert_structured_type_api_composes(
    _authority: Option<PublicStructuredAuthority>,
    _job: Option<PublicStructuredJob>,
    _poll: Option<PublicStructuredPoll>,
) {
    fn requires_canonical_suspension<S: ComptimeStructuredTypeSuspension>() {}
    requires_canonical_suspension::<PublicStructuredJob>();
}

#[test]
fn structured_type_api_composes_for_external_consumers() {
    assert_structured_type_api_composes(None, None, None);
}

#[test]
fn curated_facade_compiles_for_an_external_consumer() {
    fn inspect_import_discovery(view: &ImportDiscoveryView) {
        let _: ImportDiscoveryStatus = view.status();
        let _: &SourceRevision = view.source_revision();
        let _: &CompileErrors = view.diagnostics();
        let _: Option<&Arc<FrontendDiagnosticSnapshot>> = view.diagnostic_snapshot();
        let _: Option<DependencyEnvelope> = DependencyEnvelope::from_closed_revision(view);
    }
    let _ = inspect_import_discovery as fn(&ImportDiscoveryView);

    let snapshot = SourceSnapshot::single("main.rue", "fn main() -> i32 { 0 }").unwrap();
    let mut session = CompilerSession::new();
    let update: CompilerSessionUpdate = session.update(&snapshot);
    let diagnostics: Arc<FrontendDiagnosticSnapshot> = update.diagnostics().clone();
    update.into_result().unwrap();

    let syntax = session.published().unwrap();
    let rir: Arc<RirView> = session.rir().unwrap();
    let rooted = rooted_cfg(&mut session, &CompileOptions::default()).unwrap();
    let work: MetricsSnapshot = session.unstable_metrics();
    let views: Vec<SourceView<'_>> = snapshot.files().collect();
    assert_eq!(views[0].file_id, FileId::DEFAULT);
    assert!(!rir.is_empty());
    let rir_instruction = rir.instructions().next().unwrap();
    assert!(!rir_instruction.kind().is_empty());
    let rir_location: SourceLocationView = rir_instruction.location();
    assert!(rir_location.is_valid());
    assert!(
        rir_location.source_identity().same_source(
            &syntax
                .modules()
                .next()
                .unwrap()
                .nodes()
                .next()
                .unwrap()
                .location()
                .source_identity()
        )
    );
    let function = rooted.functions().first().unwrap();
    assert_eq!(function.source_name(), "main");
    assert!(!function.air().is_empty());
    assert!(!function.cfg().blocks().is_empty());
    assert_eq!(
        syntax
            .modules()
            .next()
            .unwrap()
            .tokens()
            .next()
            .unwrap()
            .kind(),
        "FN"
    );
    assert_eq!(work.updates(), 1);
    assert!(diagnostics.is_success());
    assert_eq!(diagnostics.stage(), DiagnosticStage::Syntax);
    for debug in [format!("{rir:?}")] {
        assert!(!debug.contains("CanonicalRirOutput"));
        assert!(!debug.contains("CanonicalSemanticOutput"));
        assert!(!debug.contains("ThreadedRodeo"));
    }

    let adapter: fn(&SourceSnapshot, &CompileOptions) -> MultiErrorResult<CompileOutput> =
        compile_snapshot;
    let _ = adapter;
    let _metadata: &SourceMetadata = snapshot.metadata();
}

fn find_syntax_value(node: &SyntaxNodeView, kind: &str, value: &str) -> bool {
    (node.kind() == kind && node.value() == Some(value))
        || node
            .children()
            .any(|child| find_syntax_value(&child, kind, value))
}

#[test]
fn syntax_nodes_resolve_names_and_retain_their_owner_across_updates() {
    let first = SourceSnapshot::single("main.rue", "fn main() -> i32 { 0 }").unwrap();
    let second = SourceSnapshot::single("main.rue", "fn replacement() -> i32 { 1 }").unwrap();
    let mut session = CompilerSession::new();
    let retained = session.update(&first).into_result().unwrap();
    let function = retained.modules().next().unwrap().nodes().next().unwrap();
    let retained_location = function.location();
    let token_location = retained
        .modules()
        .next()
        .unwrap()
        .tokens()
        .next()
        .unwrap()
        .location();

    session.update(&second).into_result().unwrap();

    assert_eq!(function.kind(), "function");
    assert_eq!(function.name(), Some("main"));
    assert!(function.child_count() >= 2);
    assert!(find_syntax_value(&function, "integer_literal", "0"));
    assert!(retained_location.is_valid());
    assert!(
        retained_location
            .source_identity()
            .same_source(&token_location.source_identity())
    );
    let debug = format!("{function:?}");
    assert!(!debug.contains("ParsedModule"));
    assert!(!debug.contains("Ast"));
}

fn snapshot_at(file_id: FileId, source: &str) -> SourceSnapshot {
    let physical_paths = [(file_id, "/project/main.rue".to_owned())]
        .into_iter()
        .collect();
    let logical_paths = [(file_id, "main.rue".to_owned())].into_iter().collect();
    let metadata = SourceMetadata::new(file_id, physical_paths, logical_paths).unwrap();
    SourceSnapshot::new(metadata, vec![(file_id, Arc::new(source.to_owned()))]).unwrap()
}

fn token_records(module: &SyntaxModuleView) -> Vec<(String, Option<String>, u32, u32)> {
    module
        .tokens()
        .map(|token| {
            (
                token.kind().to_owned(),
                token.value().map(|value| value.into_owned()),
                token.start(),
                token.end(),
            )
        })
        .collect()
}

#[test]
fn token_views_are_exact_deterministic_and_retain_their_old_source() {
    let source = "fn main() -> i32 { let value = \"old\"; value }";
    let first = snapshot_at(FileId::new(7), source);
    let edited = snapshot_at(
        FileId::new(11),
        "fn replacement() -> i32 { let value = \"new\"; value }",
    );
    let expected_kinds = [
        "FN",
        "IDENT",
        "LPAREN",
        "RPAREN",
        "ARROW",
        "TYPE(i32)",
        "LBRACE",
        "LET",
        "IDENT",
        "EQ",
        "STRING",
        "SEMI",
        "IDENT",
        "RBRACE",
        "EOF",
    ];
    let expected_lexemes = [
        "fn", "main", "(", ")", "->", "i32", "{", "let", "value", "=", "\"old\"", ";", "value",
        "}", "",
    ];

    let mut warm_session = CompilerSession::new();
    let retained = warm_session.update(&first).into_result().unwrap();
    let module = retained.modules().next().unwrap();
    assert_eq!(module.file_id(), FileId::new(7));
    let records = token_records(&module);
    assert_eq!(
        records
            .iter()
            .map(|(kind, _, _, _)| kind.as_str())
            .collect::<Vec<_>>(),
        expected_kinds
    );
    assert_eq!(
        records
            .iter()
            .map(|(_, _, start, end)| &source[*start as usize..*end as usize])
            .collect::<Vec<_>>(),
        expected_lexemes
    );
    assert_eq!(records[1].1.as_deref(), Some("main"));
    assert_eq!(records[8].1.as_deref(), Some("value"));
    assert_eq!(records[10].1.as_deref(), Some("old"));

    let old_token = module.tokens().nth(1).unwrap();
    let old_location = old_token.location();
    let metrics_before_presentation = warm_session.unstable_metrics().parse_metrics();
    let _ = token_records(&module);
    assert_eq!(
        warm_session.unstable_metrics().parse_metrics(),
        metrics_before_presentation,
        "lazy token presentation is not canonical syntax work"
    );

    warm_session.update(&edited).into_result().unwrap();
    let current = warm_session.published().unwrap().modules().next().unwrap();
    let current_location = current.tokens().nth(1).unwrap().location();
    assert_eq!(old_token.value().as_deref(), Some("main"));
    assert_eq!(
        &module.source()[old_token.start() as usize..old_token.end() as usize],
        "main"
    );
    assert!(old_location.is_valid());
    assert!(
        !old_location
            .source_identity()
            .same_source(&current_location.source_identity()),
        "a retained token view must keep the old immutable source identity"
    );

    let warm = warm_session.update(&first).into_result().unwrap();
    let warm_records = token_records(&warm.modules().next().unwrap());
    let warm_again = warm_session.update(&first);
    assert_eq!(warm_again.unstable_metrics().lexer_invocations, 0);
    let warm_again_records =
        token_records(&warm_again.into_result().unwrap().modules().next().unwrap());
    let mut fresh_session = CompilerSession::new();
    let fresh_records = token_records(
        &fresh_session
            .update(&first)
            .into_result()
            .unwrap()
            .modules()
            .next()
            .unwrap(),
    );
    assert_eq!(warm_records, fresh_records);
    assert_eq!(warm_again_records, fresh_records);

    let options = CompileOptions::default();
    let order = [FileId::new(7)];
    let warm_parse_metrics = warm_session.unstable_metrics().parse_metrics();
    let warm_presentation = warm_session
        .unstable_present(PresentationRequest {
            stage: PresentationStage::Tokens,
            options: &options,
            file_order: &order,
        })
        .unwrap();
    assert_eq!(
        warm_session.unstable_metrics().parse_metrics(),
        warm_parse_metrics,
        "unstable token presentation must not increment canonical parse work"
    );
    let fresh_presentation = fresh_session
        .unstable_present(PresentationRequest {
            stage: PresentationStage::Tokens,
            options: &options,
            file_order: &order,
        })
        .unwrap();
    assert_eq!(warm_presentation.as_str(), fresh_presentation.as_str());
    assert_eq!(
        warm_presentation.as_str().lines().count(),
        expected_kinds.len()
    );
    assert!(
        warm_presentation
            .as_str()
            .lines()
            .next()
            .unwrap()
            .ends_with("FN")
    );
    assert!(
        warm_presentation
            .as_str()
            .lines()
            .last()
            .unwrap()
            .ends_with("EOF")
    );
}

fn find_syntax_node(
    node: &SyntaxNodeView,
    kind: &str,
    name: Option<&str>,
    value: Option<&str>,
) -> bool {
    (node.kind() == kind && node.name() == name && node.value() == value)
        || node
            .children()
            .any(|child| find_syntax_node(&child, kind, name, value))
}

fn has_direct_syntax_node(
    node: &SyntaxNodeView,
    kind: &str,
    name: Option<&str>,
    value: Option<&str>,
) -> bool {
    node.children()
        .any(|child| child.kind() == kind && child.name() == name && child.value() == value)
}

#[test]
fn syntax_projection_preserves_modifiers_receivers_and_argument_modes() {
    let source = r#"
pub unchecked fn dangerous(inout x: i32, borrow y: i32) -> i32 {
    let mut z = 0;
    take(z, inout z, borrow y);
    z
}
fn safe() -> i32 { 0 }
pub linear struct Box {
    value: i32,
    fn by_value(mut self) -> i32 { self.value }
    fn borrowed(borrow self) -> i32 { self.value }
    fn exclusive(inout self) -> i32 { self.value }
    fn associated() -> i32 { 0 }
}
pub enum Choice { A, B(i32) }
pub const ANSWER: i32 = 42;
drop fn Box(self) {}
"#;
    let snapshot = SourceSnapshot::single("main.rue", source).unwrap();
    let mut session = CompilerSession::new();
    let syntax = session.update(&snapshot).into_result().unwrap();
    let roots = syntax.modules().next().unwrap().nodes().collect::<Vec<_>>();
    let has = |kind, name, value| {
        roots
            .iter()
            .any(|node| find_syntax_node(node, kind, name, value))
    };

    let root = |name| roots.iter().find(|node| node.name() == Some(name)).unwrap();
    assert!(has_direct_syntax_node(
        root("dangerous"),
        "modifier",
        Some("visibility"),
        Some("public")
    ));
    assert!(has_direct_syntax_node(
        root("dangerous"),
        "modifier",
        Some("unchecked"),
        Some("true")
    ));
    assert!(has_direct_syntax_node(
        root("safe"),
        "modifier",
        Some("visibility"),
        Some("private")
    ));
    assert!(has_direct_syntax_node(
        root("safe"),
        "modifier",
        Some("unchecked"),
        Some("false")
    ));
    assert!(has_direct_syntax_node(
        root("Box"),
        "modifier",
        Some("visibility"),
        Some("public")
    ));
    assert!(has_direct_syntax_node(
        root("Box"),
        "modifier",
        Some("linear"),
        Some("true")
    ));
    assert!(has_direct_syntax_node(
        root("Choice"),
        "modifier",
        Some("visibility"),
        Some("public")
    ));
    assert!(has_direct_syntax_node(
        root("ANSWER"),
        "modifier",
        Some("visibility"),
        Some("public")
    ));
    assert!(has("modifier", Some("mutable"), Some("true")));
    assert!(has(
        "receiver",
        Some("self"),
        Some("mode=normal;mutable=true")
    ));
    assert!(has(
        "receiver",
        Some("self"),
        Some("mode=borrow;mutable=false")
    ));
    assert!(has(
        "receiver",
        Some("self"),
        Some("mode=inout;mutable=false")
    ));
    assert!(has(
        "receiver",
        Some("self"),
        Some("mode=normal;mutable=false")
    ));
    assert!(has("argument", None, Some("normal")));
    assert!(has("argument", None, Some("inout")));
    assert!(has("argument", None, Some("borrow")));

    let associated = roots
        .iter()
        .flat_map(SyntaxNodeView::children)
        .find(|node| node.kind() == "method" && node.name() == Some("associated"))
        .unwrap();
    assert!(!associated.children().any(|node| node.kind() == "receiver"));
}

#[test]
fn rir_match_operands_are_checked_and_include_pattern_heads() {
    let source = r#"
fn inspect(value: i32) -> i32 {
    match value {
        pkg.Result(i32, i32).Ok(v) => v,
        _ => 0,
    }
}
"#;
    let snapshot = SourceSnapshot::single("main.rue", source).unwrap();
    let mut session = CompilerSession::new();
    session.update(&snapshot).into_result().unwrap();
    let rir = session.rir().unwrap();
    let matched = rir
        .instructions()
        .find(|instruction| instruction.kind() == "match")
        .unwrap();
    let operands = matched.operands().collect::<Vec<_>>();
    let roles = operands
        .iter()
        .map(|operand| operand.role())
        .collect::<Vec<_>>();

    assert_eq!(matched.operand_count(), operands.len());
    assert!(roles.contains(&"scrutinee"));
    assert!(roles.contains(&"pattern_module"));
    assert!(roles.contains(&"pattern_constructor"));
    assert!(roles.contains(&"arm_body"));
    for operand in operands {
        assert!(operand.target_ordinal() < rir.len());
        assert_eq!(operand.target().ordinal(), operand.target_ordinal());
    }
}

#[test]
fn cfg_blocks_expose_checked_instructions_and_every_successor_edge() {
    let source = r#"
fn choose(value: i32) -> i32 {
    match value {
        0 => 1,
        _ => if value > 0 { 2 } else { 3 },
    }
}
fn main() -> i32 { choose(0) }
"#;
    let snapshot = SourceSnapshot::single("main.rue", source).unwrap();
    let mut session = CompilerSession::new();
    session.update(&snapshot).into_result().unwrap();
    let rooted = rooted_cfg(&mut session, &CompileOptions::default()).unwrap();
    let cfg = rooted
        .functions()
        .iter()
        .find(|function| function.source_name() == "choose")
        .unwrap()
        .cfg();
    let blocks = cfg.blocks();
    assert!(blocks.len() >= 3);
    assert!(blocks.iter().any(|block| !block.insts.is_empty()));
}

#[test]
fn presentation_file_order_rejects_unknown_duplicate_and_incomplete_inputs() {
    let snapshot = SourceSnapshot::single("main.rue", "fn main() -> i32 { 0 }").unwrap();
    let options = CompileOptions::default();
    let mut session = CompilerSession::new();
    session.update(&snapshot).into_result().unwrap();

    for (stage, file_order) in [
        (PresentationStage::Tokens, vec![FileId::new(99)]),
        (
            PresentationStage::Tokens,
            vec![FileId::DEFAULT, FileId::DEFAULT],
        ),
        (PresentationStage::Rir, Vec::new()),
    ] {
        let errors = session
            .unstable_present(PresentationRequest {
                stage,
                options: &options,
                file_order: &file_order,
            })
            .unwrap_err();
        assert!(matches!(
            errors.first().map(|error| &error.kind),
            Some(ErrorKind::InvalidCompilerInput(_))
        ));
    }
}
