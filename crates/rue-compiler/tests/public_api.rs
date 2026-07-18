use std::sync::Arc;

use rue_compiler::unstable::MetricsSnapshot;
use rue_compiler::{
    CanonicalRirOutput, CanonicalSemanticOutput, CompileOptions, CompileOutput, CompilerSession,
    CompilerSessionUpdate, DiagnosticStage, FileId, FrontendDiagnosticSnapshot, MultiErrorResult,
    SourceMetadata, SourceSnapshot, SourceView, compile_snapshot,
};

#[test]
fn curated_facade_compiles_for_an_external_consumer() {
    let snapshot = SourceSnapshot::single("main.rue", "fn main() -> i32 { 0 }").unwrap();
    let mut session = CompilerSession::new();
    let update: CompilerSessionUpdate = session.update_for_presentation(&snapshot);
    update.into_result().unwrap();

    let rir: Arc<CanonicalRirOutput> = session.rir().unwrap();
    let semantic: Arc<CanonicalSemanticOutput> =
        session.semantic(&CompileOptions::default()).unwrap();
    let work: MetricsSnapshot = session.unstable_metrics();
    let diagnostics: Option<&Arc<FrontendDiagnosticSnapshot>> = session.latest_diagnostics();
    let views: Vec<SourceView<'_>> = snapshot.files().collect();
    assert_eq!(views[0].file_id, FileId::DEFAULT);
    let _rir_view = rir.rir();
    assert_eq!(semantic.functions().len(), 1);
    assert_eq!(work.updates(), 1);
    assert!(diagnostics.is_some());
    assert_eq!(diagnostics.unwrap().stage(), DiagnosticStage::Semantic);

    let adapter: fn(&SourceSnapshot, &CompileOptions) -> MultiErrorResult<CompileOutput> =
        compile_snapshot;
    let _ = adapter;
    let _metadata: &SourceMetadata = snapshot.metadata();
}

#[test]
fn dependency_baselines_cannot_cross_session_ownership() {
    let snapshot = SourceSnapshot::single("main.rue", "fn main() -> i32 { 0 }").unwrap();
    let options = CompileOptions::default();
    let mut first = CompilerSession::new();
    let mut second = CompilerSession::new();
    first.update(&snapshot).into_result().unwrap();
    second.update(&snapshot).into_result().unwrap();
    let first_baseline = first.unstable_dependency_baseline(&options, None).unwrap();
    let second_baseline = second.unstable_dependency_baseline(&options, None).unwrap();
    let before = first.unstable_metrics().invalidation_plans();

    assert!(
        first
            .unstable_invalidation_metrics(&first_baseline, &second_baseline)
            .is_err()
    );
    assert_eq!(first.unstable_metrics().invalidation_plans(), before);
}
