use super::*;
use rue_compiler::unstable::{MultiFileFormatter, SourceInfo};
use rue_compiler::{CompilerSessionConfig, RootSelection};
use rue_driver::{HostOpenRequest, HostPathContext};
use std::fs;
use std::path::Path;

struct Project {
    directory: tempfile::TempDir,
}

impl Project {
    fn new(source: &str) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("main.rue");
        fs::write(&root, source).unwrap();
        Self { directory }
    }

    fn host(&self) -> FilesystemCompilerHost {
        let context = HostPathContext::from_working_directory(self.directory.path()).unwrap();
        FilesystemCompilerHost::open(HostOpenRequest {
            root_source: "main.rue",
            source_manifest_path: None,
            std_root: None,
            compiler_config: CompilerSessionConfig::with_workers(1).unwrap(),
            path_context: &context,
        })
        .unwrap()
    }

    fn options(&self, root_selection: RootSelection) -> CompileOptions {
        CompileOptions {
            root_selection,
            ..CompileOptions::default()
        }
    }
}

#[test]
fn executable_and_test_image_complete_after_host_drop() {
    let project = Project::new(
        "fn main() -> i32 { 0 }\n\
         test \"passes\" { let _value = 1; }\n",
    );
    let output = project.directory.path().join("out");
    let image = project.directory.path().join("image");
    let mut host = project.host();
    let options = project.options(RootSelection::Executable);
    let response = produce::<CompileOutput>(
        &mut host,
        &options,
        ErrorFormat::Text,
        Some("main.rue"),
        Path::new("out"),
        CycleObservation::OneShot,
    );
    assert!(
        !output.exists(),
        "production must defer publication until response completion"
    );

    fs::write(
        project.directory.path().join("main.rue"),
        "fn main() -> i32 { 1 }\n",
    )
    .unwrap();
    host.reobserve().unwrap();
    drop(host);
    assert!(matches!(response.complete(None), CycleReport::Published(_)));
    assert!(
        output.is_file(),
        "completion must publish using owned destination"
    );

    let fresh_output = project.directory.path().join("fresh-out");
    let mut fresh_host = project.host();
    let fresh_options = project.options(RootSelection::Executable);
    let fresh_response = produce::<CompileOutput>(
        &mut fresh_host,
        &fresh_options,
        ErrorFormat::Text,
        Some("main.rue"),
        Path::new("fresh-out"),
        CycleObservation::OneShot,
    );
    assert!(matches!(
        fresh_response.complete(None),
        CycleReport::Published(_)
    ));
    assert_ne!(
        fs::read(&output).unwrap(),
        fs::read(fresh_output).unwrap(),
        "the retained response must publish its original source revision"
    );

    let mut host = project.host();
    let options = project.options(RootSelection::Tests);
    let response = produce_test_cycle(TestCycleRequest {
        host: &mut host,
        options: &options,
        error_format: ErrorFormat::Text,
        candidates: None,
        image_path: Path::new("image"),
        observation: CycleObservation::OneShot,
    });
    assert!(!image.exists(), "test-image production must be deferred");
    drop(host);
    assert!(matches!(response.complete(None), CycleReport::Published(_)));
    assert!(
        image.is_file(),
        "test-image completion must publish after handoff"
    );
}

#[test]
fn superseded_owned_cycle_is_silent_and_preserves_existing_output() {
    let project = Project::new("fn main() -> i32 { 0 }\n");
    let output = project.directory.path().join("out");
    fs::write(&output, b"previous").unwrap();
    let mut host = project.host();
    let options = project.options(RootSelection::Executable);
    let inputs = host.watch_inputs();
    let cancellation = rue_compiler::unstable::CompilationCancellation::new();
    let response = produce::<CompileOutput>(
        &mut host,
        &options,
        ErrorFormat::Text,
        Some("main.rue"),
        Path::new("out"),
        CycleObservation::Watch {
            inputs,
            cancellation,
            superseded: &|| true,
        },
    );
    drop(host);
    assert!(matches!(
        response.complete(Some(Announcement::Completed)),
        CycleReport::Superseded(Supersession::BeforePublication)
    ));
    assert_eq!(fs::read(output).unwrap(), b"previous");
}

#[test]
fn owned_preflight_refuses_source_clobber_before_compile() {
    let project = Project::new("fn main() -> i32 { true }\n");
    let mut host = project.host();
    let options = project.options(RootSelection::Executable);
    let before = host.unstable_metrics().query_runtime();
    let response = produce::<CompileOutput>(
        &mut host,
        &options,
        ErrorFormat::Text,
        Some("main.rue"),
        &project.directory.path().join("main.rue"),
        CycleObservation::OneShot,
    );
    let after = host.unstable_metrics().query_runtime();
    assert!(matches!(
        response.result,
        OwnedCycleResult::Failed {
            errors: None,
            publication: Some(PublishError::WouldClobberSource { .. }),
        }
    ));
    assert_eq!(before, after, "preflight must run before compiler queries");
    assert_eq!(
        fs::read(project.directory.path().join("main.rue")).unwrap(),
        b"fn main() -> i32 { true }\n",
        "preflight must leave the source untouched"
    );
}

#[test]
fn prepared_test_failures_survive_response_handoff() {
    let original_source = "test \"broken\" { let value: i32 = true; let _ = value; }\n\
         test \"fine\" { let _value = 1; }\n\
         fn main() -> i32 { 0 }\n";
    let project = Project::new(original_source);
    let mut host = project.host();
    let options = project.options(RootSelection::Tests);
    let original_snapshot = host.source_snapshot().clone();
    let response = produce_test_cycle(TestCycleRequest {
        host: &mut host,
        options: &options,
        error_format: ErrorFormat::Json,
        candidates: None,
        image_path: Path::new("image"),
        observation: CycleObservation::OneShot,
    });
    fs::write(
        project.directory.path().join("main.rue"),
        "\n\n\n\n\n\
         test \"broken\" { let value: i32 = false; let _ = value; }\n\
         test \"fine\" { let _value = 2; }\n\
         fn main() -> i32 { 0 }\n",
    )
    .unwrap();
    host.reobserve().unwrap();
    drop(host);
    let CycleReport::Published(image) = response.complete(None) else {
        panic!("a surviving test should publish an image");
    };
    assert_eq!(image.compile_failures.len(), 1);
    assert_eq!(image.compile_failures[0].entry.name, "broken");
    assert_eq!(image.inventory.entries.len(), 2);
    assert!(
        image
            .inventory
            .entries
            .iter()
            .any(|entry| entry.name == "fine"),
        "a valid sibling must survive the failed test closure"
    );
    assert!(!image.compile_failures[0].errors.is_empty());
    assert!(
        image.compile_failures[0]
            .errors
            .iter()
            .any(|error| { matches!(error.kind, rue_error::ErrorKind::TypeMismatch { .. }) })
    );
    let original_source = original_snapshot.files().next().unwrap();
    let original_error = image.compile_failures[0]
        .errors
        .iter()
        .find(|error| matches!(error.kind, rue_error::ErrorKind::TypeMismatch { .. }))
        .unwrap();
    let original_span = original_error.span().unwrap();
    assert_eq!(
        image
            .source_snapshot
            .files()
            .find(|source| source.file_id == original_source.file_id)
            .unwrap()
            .source,
        original_source.source
    );
    assert!(
        image
            .compile_failures
            .iter()
            .all(|failure| failure.entry.name != "fine")
    );
    let source_before_span = &original_source.source[..original_span.start as usize];
    let expected_line = source_before_span.matches('\n').count() as u32 + 1;
    let expected_column = source_before_span.rsplit('\n').next().unwrap().len() as u32 + 1;
    let sources = image
        .source_snapshot
        .files()
        .map(|source| (source.file_id, SourceInfo::new(source.source, source.path)))
        .collect::<Vec<_>>();
    let text = MultiFileFormatter::new(sources.iter().cloned())
        .format_errors(&image.compile_failures[0].errors);
    assert!(
        text.contains("true"),
        "retained diagnostics use old source: {text}"
    );
    assert!(
        !text.contains("false"),
        "diagnostics used successor source: {text}"
    );
    let output = DiagnosticOutput::new(ErrorFormat::Json, sources);
    let json = output.render_prepared_errors(&image.compile_failures[0].errors);
    assert!(
        json.contains("type mismatch"),
        "missing typed JSON diagnostic: {json}"
    );
    let diagnostics: Vec<serde_json::Value> = serde_json::from_str(&json).unwrap();
    assert_eq!(
        output.json_prepared_diagnostic_batches(&[&image.compile_failures[0].errors]),
        vec![diagnostics.clone()],
        "stderr and test-event diagnostic projections must agree after handoff"
    );
    let primary = diagnostics[0]["spans"]
        .as_array()
        .unwrap()
        .iter()
        .find(|span| span["primary"] == serde_json::Value::Bool(true))
        .unwrap();
    assert_eq!(primary["file"], original_source.path);
    assert_eq!(primary["start"], original_span.start);
    assert_eq!(primary["end"], original_span.end);
    assert_eq!(primary["line"], expected_line);
    assert_eq!(primary["column"], expected_column);
    assert!(
        !image
            .source_snapshot
            .files()
            .next()
            .unwrap()
            .source
            .is_empty()
    );
    assert_eq!(image.error_format, ErrorFormat::Json);
}
