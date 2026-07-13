use std::sync::Arc;

use rue_compiler::{
    CanonicalRirOutput, CanonicalSemanticOutput, CompileOptions, CompileOutput, CompilerSession,
    CompilerSessionUpdate, CompilerSessionWork, FileId, FrontendDiagnosticSnapshot,
    MultiErrorResult, SourceMetadata, SourceSnapshot, SourceView, compile_snapshot,
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
    let work: &CompilerSessionWork = session.work();
    let diagnostics: Option<&Arc<FrontendDiagnosticSnapshot>> = session.latest_diagnostics();
    let views: Vec<SourceView<'_>> = snapshot.files().collect();
    assert_eq!(views[0].file_id, FileId::DEFAULT);
    let _rir_view = rir.rir();
    assert_eq!(semantic.functions().len(), 1);
    assert_eq!(work.updates, 1);
    assert!(diagnostics.is_some());

    let adapter: fn(&SourceSnapshot, &CompileOptions) -> MultiErrorResult<CompileOutput> =
        compile_snapshot;
    let _ = adapter;
    let _metadata: &SourceMetadata = snapshot.metadata();
}
