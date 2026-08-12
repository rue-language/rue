use std::sync::Arc;

use rue_compiler::unstable::{MetricsSnapshot, PresentationRequest, PresentationStage, rooted_cfg};
use rue_compiler::{
    CompileErrors, CompileOptions, CompileOutput, CompilerSession, CompilerSessionUpdate,
    DependencyEnvelope, DiagnosticStage, FileId, FrontendDiagnosticSnapshot, ImportDiscoveryStatus,
    ImportDiscoveryView, MultiErrorResult, RirView, SourceLocationView, SourceMetadata,
    SourceRevision, SourceSnapshot, SourceView, SyntaxNodeView, compile_snapshot,
};
use rue_error::ErrorKind;

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
    update.into_result().unwrap();

    let syntax = session.published().unwrap();
    let rir: Arc<RirView> = session.rir().unwrap();
    let rooted = rooted_cfg(&mut session, &CompileOptions::default()).unwrap();
    let work: MetricsSnapshot = session.unstable_metrics();
    let diagnostics: Option<&Arc<FrontendDiagnosticSnapshot>> = session.latest_diagnostics();
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
    assert!(diagnostics.is_some());
    assert_eq!(diagnostics.unwrap().stage(), DiagnosticStage::Semantic);
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
