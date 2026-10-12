//! Whole-program checking through retained filesystem hosts.
//!
//! A check is the driver's own cycle stopped at ADR-0068's codegen-ready
//! endpoint: reobserve the root's accepted-read closure, acquire reached
//! toolchain modules, and analyze the rooted program. Each root keeps one
//! `FilesystemCompilerHost`, so a save re-runs only the queries the edit
//! invalidated. Diagnostics are the compiler's structured JSON records
//! (`docs/process/diagnostics.md`) mapped to LSP coordinates; nothing here
//! formats or classifies a diagnostic itself.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use rue_compiler::unstable::{JsonDiagnostic, JsonSpan, MultiFileJsonFormatter, SourceInfo};
use rue_compiler::{
    CompileErrors, CompileOptions, CompileWarning, CompilerSessionConfig, LinkerMode,
    PreviewFeatures, RootSelection, SourceSnapshot,
};
use rue_driver::{
    FilesystemCompilerHost, HostOpenRequest, HostPathContext, SourceLoadError,
    with_import_migration_helps,
};
use rue_error::ErrorCode;
use serde_json::{Value, json};

use crate::syntax_index::ModuleIndex;
use crate::text::{LineIndex, Position, Range, path_to_uri};

/// An editor host is long-lived, so it uses the compiler service's explicit
/// retention budget rather than the much larger one-shot defaults.
const RETAINED_BYTE_BUDGET: u64 = 256 * 1024 * 1024;
const DEPENDENCY_PIN_BUDGET: u64 = 1_000_000;
const WORKERS: usize = 4;
/// Roots retained at once. Opening another evicts the least recently checked.
const MAX_PROJECTS: usize = 4;

const SEVERITY_ERROR: u32 = 1;
const SEVERITY_WARNING: u32 = 2;

#[derive(Clone, Debug, Default)]
pub struct CheckSettings {
    /// The standard library root, as `RUE_STD_PATH` supplies it to the CLI.
    pub std_root: Option<PathBuf>,
    pub preview_features: PreviewFeatures,
}

/// The result of checking one root.
pub struct CheckOutcome {
    /// Every file of the checked closure, each with its diagnostics (possibly
    /// none, which clears what the client showed before). A load failure that
    /// produced no closure reports against the root alone.
    pub files: BTreeMap<PathBuf, Vec<Value>>,
}

struct Project {
    root: PathBuf,
    selection: RootSelection,
    host: FilesystemCompilerHost,
    modules: Vec<ModuleIndex>,
    last_checked: u64,
}

pub struct Checker {
    settings: CheckSettings,
    path_context: HostPathContext,
    projects: Vec<Project>,
    clock: u64,
}

impl Checker {
    pub fn new(settings: CheckSettings, path_context: HostPathContext) -> Self {
        Self {
            settings,
            path_context,
            projects: Vec::new(),
            clock: 0,
        }
    }

    /// The root and selection of a retained project whose closure contains
    /// `path`, most recently checked first.
    pub fn project_containing(&self, path: &Path) -> Option<(PathBuf, RootSelection)> {
        self.projects
            .iter()
            .filter(|project| project.modules.iter().any(|module| module.path == path))
            .max_by_key(|project| project.last_checked)
            .map(|project| (project.root.clone(), project.selection))
    }

    /// Syntax of every file in every retained closure, as last saved.
    pub fn modules(&self) -> impl Iterator<Item = &ModuleIndex> {
        self.projects
            .iter()
            .flat_map(|project| project.modules.iter())
    }

    /// Check the program rooted at `root`.
    pub fn check(&mut self, root: &Path, selection: RootSelection) -> CheckOutcome {
        self.clock += 1;
        let options = CompileOptions {
            linker: LinkerMode::Internal,
            preview_features: self.settings.preview_features.clone(),
            root_selection: selection,
            ..CompileOptions::default()
        };
        let position = self
            .projects
            .iter()
            .position(|project| project.root == root && project.selection == selection);
        let position = match position {
            Some(position) => {
                // A failed reobserve keeps the prior coherent closure, so the
                // host stays retained and the failure is this check's answer.
                if let Err(error) = self.projects[position].host.reobserve() {
                    return load_failure(root, error, &|path| {
                        self.path_context.resolve(Path::new(path))
                    });
                }
                position
            }
            None => match self.open(root) {
                Ok(host) => {
                    if self.projects.len() >= MAX_PROJECTS {
                        let oldest = self
                            .projects
                            .iter()
                            .enumerate()
                            .min_by_key(|(_, project)| project.last_checked)
                            .map(|(index, _)| index)
                            .expect("at least one project is retained");
                        self.projects.swap_remove(oldest);
                    }
                    self.projects.push(Project {
                        root: root.to_owned(),
                        selection,
                        host,
                        modules: Vec::new(),
                        last_checked: 0,
                    });
                    self.projects.len() - 1
                }
                Err(error) => {
                    return load_failure(root, error, &|path| {
                        self.path_context.resolve(Path::new(path))
                    });
                }
            },
        };
        let path_context = &self.path_context;
        let resolve = |path: &str| path_context.resolve(Path::new(path));
        let project = &mut self.projects[position];
        project.last_checked = self.clock;
        project.modules = project
            .host
            .syntax()
            .map(|syntax| {
                syntax
                    .modules()
                    .map(|module| ModuleIndex::new(&module, resolve(module.path())))
                    .collect()
            })
            .unwrap_or_default();
        let host = &mut project.host;
        let outcome = match host.acquire_reached_toolchain_modules(&options) {
            Err(error) => load_failure(root, error, &resolve),
            Ok(()) => {
                let snapshot = host.source_snapshot().clone();
                match host.codegen_ready(&options) {
                    Ok(ready) => diagnostics(
                        root,
                        &snapshot,
                        &CompileErrors::new(),
                        ready.warnings(),
                        &resolve,
                    ),
                    Err(errors) => diagnostics(root, &snapshot, &errors, &[], &resolve),
                }
            }
        };
        self.enforce_retention_budget();
        outcome
    }

    fn open(&self, root: &Path) -> Result<FilesystemCompilerHost, SourceLoadError> {
        let compiler_config = CompilerSessionConfig::with_workers_and_retention(
            WORKERS,
            RETAINED_BYTE_BUDGET,
            DEPENDENCY_PIN_BUDGET,
        )
        .expect("the editor host's resource policy is valid");
        let root_source = root.to_string_lossy();
        FilesystemCompilerHost::open(HostOpenRequest {
            root_source: &root_source,
            source_manifest_path: None,
            std_root: self.settings.std_root.as_deref(),
            compiler_config,
            path_context: &self.path_context,
        })
    }

    /// Drop any host whose retained artifacts exceed the budget; the next
    /// check of that root reopens it cold.
    fn enforce_retention_budget(&mut self) {
        self.projects.retain(|project| {
            let retention = project.host.unstable_metrics().retention();
            retention.retained_bytes as u64 <= RETAINED_BYTE_BUDGET
                && retention.dependency_pins as u64 <= DEPENDENCY_PIN_BUDGET
        });
    }
}

/// The compiler spells a closure's files relative to the host's working
/// directory; the client needs absolute paths.
type Resolve<'a> = &'a dyn Fn(&str) -> PathBuf;

fn load_failure(root: &Path, error: SourceLoadError, resolve: Resolve<'_>) -> CheckOutcome {
    let (code, message) = match error {
        SourceLoadError::Compiler { snapshot, errors } => {
            return match snapshot {
                Some(snapshot) => diagnostics(root, &snapshot, &errors, &[], resolve),
                None => unlocated(root, &errors),
            };
        }
        SourceLoadError::Message(message) => (ErrorCode::DRIVER_SOURCE_LOAD, message),
        SourceLoadError::Toolchain(error) => {
            (ErrorCode::DRIVER_TOOLCHAIN_INTEGRITY, error.to_string())
        }
        SourceLoadError::HermeticDenial(error) => {
            (ErrorCode::DRIVER_HERMETIC_DENIAL, error.to_string())
        }
        SourceLoadError::Superseded => (
            ErrorCode::DRIVER_SOURCE_LOAD,
            "source observation superseded by a newer revision".to_owned(),
        ),
    };
    let diagnostic = JsonDiagnostic {
        code: code.to_string(),
        message,
        severity: "error",
        spans: Vec::new(),
        suggestions: Vec::new(),
        notes: Vec::new(),
        helps: Vec::new(),
    };
    let lsp = Renderer::empty().to_lsp(&diagnostic, None);
    CheckOutcome {
        files: BTreeMap::from([(root.to_owned(), vec![lsp])]),
    }
}

/// Diagnostics whose sources the loader did not retain: their messages are
/// still reported, against the root.
fn unlocated(root: &Path, errors: &CompileErrors) -> CheckOutcome {
    let formatter = MultiFileJsonFormatter::new(std::iter::empty());
    let renderer = Renderer::empty();
    let diagnostics = errors
        .iter()
        .map(|error| renderer.to_lsp(&formatter.format_error(error), None))
        .collect();
    CheckOutcome {
        files: BTreeMap::from([(root.to_owned(), diagnostics)]),
    }
}

/// Map the compiler's JSON diagnostics for one snapshot onto its files.
fn diagnostics(
    root: &Path,
    snapshot: &SourceSnapshot,
    errors: &CompileErrors,
    warnings: &[CompileWarning],
    resolve: Resolve<'_>,
) -> CheckOutcome {
    let renderer = Renderer {
        sources: snapshot
            .files()
            .map(|source| {
                (
                    resolve(source.path),
                    SourceText {
                        text: source.source,
                        lines: LineIndex::new(source.source),
                    },
                )
            })
            .collect(),
        resolve,
    };
    let formatter = MultiFileJsonFormatter::new(
        snapshot
            .files()
            .map(|source| (source.file_id, SourceInfo::new(source.source, source.path))),
    );
    let errors = with_import_migration_helps(errors);
    let mut files: BTreeMap<PathBuf, Vec<Value>> = renderer
        .sources
        .keys()
        .map(|path| (path.clone(), Vec::new()))
        .collect();
    let rendered = errors
        .iter()
        .map(|error| formatter.format_error(error))
        .chain(
            warnings
                .iter()
                .map(|warning| formatter.format_warning(warning)),
        );
    for diagnostic in rendered {
        let primary = diagnostic.spans.iter().find(|span| span.primary);
        let file = primary.map_or_else(|| root.to_owned(), |span| resolve(&span.file));
        let lsp = renderer.to_lsp(&diagnostic, primary);
        files.entry(file).or_default().push(lsp);
    }
    CheckOutcome { files }
}

struct SourceText<'a> {
    text: &'a str,
    lines: LineIndex,
}

struct Renderer<'a> {
    sources: BTreeMap<PathBuf, SourceText<'a>>,
    resolve: Resolve<'a>,
}

impl Renderer<'static> {
    fn empty() -> Self {
        Self {
            sources: BTreeMap::new(),
            resolve: &|path| PathBuf::from(path),
        }
    }
}

impl Renderer<'_> {
    fn range(&self, file: &str, start: u32, end: u32) -> Range {
        match self.sources.get(&(self.resolve)(file)) {
            Some(source) => source
                .lines
                .range(source.text, start as usize, end as usize),
            None => Range {
                start: Position::default(),
                end: Position::default(),
            },
        }
    }

    fn uri(&self, file: &str) -> String {
        path_to_uri(&(self.resolve)(file))
    }

    /// One compiler diagnostic as an LSP `Diagnostic`. Notes and helps
    /// follow the message as the text renderer prints them; secondary spans
    /// become related information; suggestions ride in `data` so a
    /// code-action request can offer them without the server remembering
    /// what it published.
    fn to_lsp(&self, diagnostic: &JsonDiagnostic, primary: Option<&JsonSpan>) -> Value {
        let range = primary.map_or_else(
            || Range {
                start: Position::default(),
                end: Position::default(),
            },
            |span| self.range(&span.file, span.start, span.end),
        );
        let mut message = diagnostic.message.clone();
        for note in &diagnostic.notes {
            message.push_str("\nnote: ");
            message.push_str(note);
        }
        for help in &diagnostic.helps {
            message.push_str("\nhelp: ");
            message.push_str(help);
        }
        let related: Vec<Value> = diagnostic
            .spans
            .iter()
            .filter(|span| !span.primary)
            .map(|span| {
                json!({
                    "location": {
                        "uri": self.uri(&span.file),
                        "range": self.range(&span.file, span.start, span.end).to_json(),
                    },
                    "message": span.label.clone().unwrap_or_default(),
                })
            })
            .collect();
        let suggestions: Vec<Value> = diagnostic
            .suggestions
            .iter()
            .map(|suggestion| {
                json!({
                    "message": suggestion.message,
                    "uri": self.uri(&suggestion.file),
                    "range": self
                        .range(&suggestion.file, suggestion.start, suggestion.end)
                        .to_json(),
                    "newText": suggestion.replacement,
                    "applicability": suggestion.applicability,
                })
            })
            .collect();
        let severity = if diagnostic.severity == "warning" {
            SEVERITY_WARNING
        } else {
            SEVERITY_ERROR
        };
        let mut lsp = json!({
            "range": range.to_json(),
            "severity": severity,
            "code": diagnostic.code,
            "source": "rue",
            "message": message,
        });
        if !related.is_empty() {
            lsp["relatedInformation"] = Value::Array(related);
        }
        if !suggestions.is_empty() {
            lsp["data"] = json!({ "suggestions": suggestions });
        }
        lsp
    }
}

/// LSP diagnostics for the errors of a snapshot holding one live buffer.
pub fn buffer_diagnostics(snapshot: &SourceSnapshot, errors: &CompileErrors) -> Vec<Value> {
    let outcome = diagnostics(Path::new(""), snapshot, errors, &[], &|path| {
        PathBuf::from(path)
    });
    outcome.files.into_values().flatten().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Project {
        dir: tempfile::TempDir,
    }

    impl Project {
        fn new(files: &[(&str, &str)]) -> Self {
            let dir = tempfile::tempdir().unwrap();
            for (name, text) in files {
                std::fs::write(dir.path().join(name), text).unwrap();
            }
            Self { dir }
        }

        fn path(&self, name: &str) -> PathBuf {
            self.dir.path().canonicalize().unwrap().join(name)
        }
    }

    fn checker(project: &Project) -> Checker {
        Checker::new(
            CheckSettings::default(),
            HostPathContext::from_working_directory(project.path("")).unwrap(),
        )
    }

    #[test]
    fn a_type_error_is_reported_at_its_span() {
        let project = Project::new(&[(
            "main.rue",
            "fn main() -> i32 {\n    let x: i32 = true;\n    x\n}\n",
        )]);
        let mut checker = checker(&project);
        let root = project.path("main.rue");
        let outcome = checker.check(&root, RootSelection::Executable);
        let diagnostics = &outcome.files[&root];
        assert_eq!(diagnostics.len(), 1, "{diagnostics:#?}");
        let diagnostic = &diagnostics[0];
        assert_eq!(diagnostic["severity"], SEVERITY_ERROR);
        assert_eq!(diagnostic["source"], "rue");
        assert!(diagnostic["code"].as_str().unwrap().starts_with('E'));
        assert_eq!(diagnostic["range"]["start"]["line"], 1);
    }

    #[test]
    fn a_clean_analysis_reports_its_warnings() {
        let project = Project::new(&[(
            "main.rue",
            "fn main() -> i32 {\n    let unused = 3;\n    0\n}\n",
        )]);
        let mut checker = checker(&project);
        let root = project.path("main.rue");
        let outcome = checker.check(&root, RootSelection::Executable);
        let diagnostics = &outcome.files[&root];
        assert_eq!(diagnostics.len(), 1, "{diagnostics:#?}");
        assert_eq!(diagnostics[0]["severity"], SEVERITY_WARNING);
        assert_eq!(diagnostics[0]["range"]["start"]["line"], 1);
    }

    #[test]
    fn a_fix_on_disk_clears_the_retained_project() {
        let project = Project::new(&[
            ("main.rue", "fn main() -> i32 { 0 }\n"),
            ("broken.rue", "fn main() -> i32 { nope }\n"),
        ]);
        let mut checker = checker(&project);
        let root = project.path("broken.rue");
        let first = checker.check(&root, RootSelection::Executable);
        assert!(!first.files[&root].is_empty());
        std::fs::write(&root, "fn main() -> i32 { 1 }\n").unwrap();
        let second = checker.check(&root, RootSelection::Executable);
        assert_eq!(second.files[&root], Vec::<Value>::new());
        assert_eq!(
            checker.project_containing(&root),
            Some((root.clone(), RootSelection::Executable))
        );
    }

    #[test]
    fn an_imported_module_error_lands_in_that_module() {
        let project = Project::new(&[
            (
                "main.rue",
                "const helper = @import(\"helper.rue\");\nfn main() -> i32 { helper.value() }\n",
            ),
            ("helper.rue", "pub fn value() -> i32 { false }\n"),
        ]);
        let mut checker = checker(&project);
        let root = project.path("main.rue");
        let outcome = checker.check(&root, RootSelection::Executable);
        let helper = project.path("helper.rue");
        assert_eq!(
            outcome.files[&root],
            Vec::<Value>::new(),
            "{:#?}",
            outcome.files
        );
        assert_eq!(outcome.files[&helper].len(), 1, "{:#?}", outcome.files);
        assert!(checker.modules().any(|module| module.path == helper));
    }

    #[test]
    fn a_missing_root_reports_a_driver_error() {
        let project = Project::new(&[]);
        let mut checker = checker(&project);
        let root = project.path("absent.rue");
        let outcome = checker.check(&root, RootSelection::Executable);
        assert_eq!(outcome.files[&root].len(), 1);
        assert_eq!(
            outcome.files[&root][0]["code"],
            ErrorCode::DRIVER_SOURCE_LOAD.to_string()
        );
    }
}
