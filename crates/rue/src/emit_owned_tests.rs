use super::*;
use crate::ErrorFormat;
use rue_compiler::{CompilerSessionConfig, OptLevel};
use rue_driver::{HostOpenRequest, HostPathContext};
use std::fs;
use std::path::{Path, PathBuf};

struct Project {
    directory: tempfile::TempDir,
    path: PathBuf,
}

impl Project {
    fn new(source: &str) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let path = fs::canonicalize(directory.path()).unwrap();
        fs::create_dir(path.join("app")).unwrap();
        fs::write(path.join("app/main.rue"), source).unwrap();
        Self { directory, path }
    }

    fn open(&self, std_root: Option<&Path>) -> FilesystemCompilerHost {
        FilesystemCompilerHost::open(HostOpenRequest {
            root_source: "app/main.rue",
            source_manifest_path: None,
            std_root,
            compiler_config: CompilerSessionConfig::with_workers(1).unwrap(),
            path_context: &HostPathContext::from_working_directory(self.path.clone()).unwrap(),
        })
        .unwrap()
    }

    fn write(&self, source: &str) {
        fs::write(self.path.join("app/main.rue"), source).unwrap();
    }
}

fn response(host: &mut FilesystemCompilerHost, stages: &[EmitStage]) -> OwnedEmitResponse {
    // A completion must use the response's sources, even when the thin
    // caller's formatter has no source context of its own.
    let diagnostics = DiagnosticOutput::new(ErrorFormat::Json, Vec::new());
    produce(EmitRequest {
        host,
        stages,
        compile_options: CompileOptions {
            opt_level: OptLevel::O0,
            ..CompileOptions::default()
        },
        diagnostics: &diagnostics,
    })
}

fn stages(response: &OwnedEmitResponse) -> &Vec<OwnedEmitStage> {
    let OwnedEmitResult::Stages(stages) = &response.result else {
        panic!("the fixture must produce its requested presentation");
    };
    stages
}

fn errors(response: &OwnedEmitResponse) -> &CompileErrors {
    match &response.result {
        OwnedEmitResult::Failed(errors)
        | OwnedEmitResult::Dependencies {
            errors: Some(errors),
            ..
        } => errors,
        _ => panic!("the fixture must carry its import diagnostic"),
    }
}

fn render_errors(response: &OwnedEmitResponse, format: ErrorFormat) -> String {
    let sources = response
        .source_snapshot
        .files()
        .map(|source| {
            (
                source.file_id,
                rue_compiler::unstable::SourceInfo::new(source.source, source.path),
            )
        })
        .collect();
    DiagnosticOutput::new(format, sources).render_prepared_errors(errors(response))
}

#[test]
fn owned_air_remains_the_requested_revision_after_host_refresh_and_drop() {
    let project = Project::new("fn main() -> i32 { 1 }\n");
    let mut host = project.open(None);
    let owned = response(&mut host, &[EmitStage::Air]);
    assert_eq!(&owned.accepted_reads, host.accepted_reads());
    assert_eq!(owned.attempted_reads, host.attempted_reads());
    assert_eq!(owned.watch_inputs, host.watch_inputs());
    let original = stages(&owned)[0].output.as_str().to_owned();
    assert_eq!(stages(&owned)[0].stage, EmitStage::Air);

    project.write("fn main() -> i32 { 2 }\n");
    host.reobserve().unwrap();
    let successor = response(&mut host, &[EmitStage::Air]);
    assert_ne!(owned.accepted_reads, successor.accepted_reads);
    assert_ne!(stages(&successor)[0].output.as_str(), original);
    let fresh = response(&mut project.open(None), &[EmitStage::Air]);
    assert_eq!(stages(&successor)[0].output, stages(&fresh)[0].output);
    drop(host);
    project.directory.close().unwrap();

    assert_eq!(stages(&owned)[0].output.as_str(), original);
    assert_eq!(
        owned.source_snapshot.files().next().unwrap().source,
        "fn main() -> i32 { 1 }\n"
    );
    assert!(complete(owned).is_ok());
}

#[test]
fn owned_analysis_and_incomplete_dependencies_freeze_import_help_and_locations() {
    let project = Project::new("const sibling = @import(\"sibling\");\nfn main() -> i32 { 0 }\n");
    fs::write(
        project.path.join("app/sibling.rue"),
        "pub fn value() -> i32 { 1 }\n",
    )
    .unwrap();
    let mut host = project.open(None);
    let analysis = response(&mut host, &[EmitStage::Air]);
    let dependencies = response(&mut host, &[EmitStage::Deps]);
    assert!(errors(&analysis).iter().any(|error| {
        matches!(&error.kind, rue_error::ErrorKind::ModuleNotFound { path, .. } if path == "sibling")
    }));
    let text = render_errors(&analysis, ErrorFormat::Text);
    let json = render_errors(&analysis, ErrorFormat::Json);
    assert!(text.contains("@import(\"sibling\")"), "{text}");
    assert_eq!(text.matches("extensionless import names").count(), 1);
    assert_eq!(json.matches("extensionless import names").count(), 1);
    assert_eq!(render_errors(&dependencies, ErrorFormat::Json), json);
    let OwnedEmitResult::Dependencies { json: envelope, .. } = &dependencies.result else {
        panic!("incomplete discovery must retain its dependency envelope");
    };
    let envelope: serde_json::Value = serde_json::from_str(envelope).unwrap();
    assert_eq!(envelope["status"], "incomplete");

    fs::remove_file(project.path.join("app/sibling.rue")).unwrap();
    project.write("fn main() -> i32 { 99 }\n");
    host.reobserve().unwrap();
    assert!(matches!(
        response(&mut host, &[EmitStage::Air]).result,
        OwnedEmitResult::Stages(_)
    ));
    drop(host);
    project.directory.close().unwrap();

    assert_eq!(render_errors(&analysis, ErrorFormat::Text), text);
    assert_eq!(render_errors(&analysis, ErrorFormat::Json), json);
    assert_eq!(render_errors(&dependencies, ErrorFormat::Json), json);
    assert!(complete(analysis).is_err());
    assert!(complete(dependencies).is_err());
}

#[test]
fn owned_syntax_batch_keeps_stage_order_without_acquiring_missing_std() {
    let project = Project::new("fn main() -> i32 { let _ = @parse_i64(\"1\"); 0 }\n");
    let mut host = project.open(Some(Path::new("unavailable-std")));
    let attempted = host.attempted_reads().to_vec();
    let requested = [EmitStage::Ast, EmitStage::Rir, EmitStage::Tokens];
    let owned = response(&mut host, &requested);
    assert_eq!(
        stages(&owned)
            .iter()
            .map(|stage| stage.stage)
            .collect::<Vec<_>>(),
        requested
    );
    assert!(
        stages(&owned)[0]
            .file
            .as_ref()
            .unwrap()
            .ends_with("main.rue")
    );
    assert_eq!(stages(&owned)[1].file, None);
    assert!(
        stages(&owned)[2]
            .file
            .as_ref()
            .unwrap()
            .ends_with("main.rue")
    );
    assert_eq!(host.source_snapshot().len(), 1);
    assert_eq!(host.accepted_reads().len(), 1);
    assert_eq!(host.attempted_reads(), attempted.as_slice());
    drop(host);
    project.directory.close().unwrap();
    assert!(complete(owned).is_ok());
}
