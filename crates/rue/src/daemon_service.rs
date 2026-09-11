//! The compiler service's executor: one retained filesystem host, and each
//! admitted build run through the very cycle the direct driver uses
//! (ADR-0085 §1, §3, §4).
//!
//! The service never changes its own working directory or environment: a
//! request carries the client's cwd and every path as the client spelled it,
//! and a `HostPathContext` for that cwd resolves them exactly as the client's
//! own process would have.

use std::path::{Path, PathBuf};

use rue_compiler::PreviewFeature;
use rue_compiler::unstable::{ColorChoice, CompilationCancellation};
use rue_compiler::{CompileOptions, CompilerSessionConfig, LinkerMode, RootSelection};
use rue_driver::daemon::{
    BuildExecutor, BuildOutput, BuildRequest, BuildResult, DestinationRecord, DiagnosticFormat,
    InputRecord,
};
use rue_driver::{FilesystemCompilerHost, HostOpenRequest, HostPathContext, SourceLoadError};

use crate::compile::{CycleTransport, TransportOutcome, produce_transport};
use crate::{ErrorFormat, render_source_load_error_with_color};

/// What selects a retained host (ADR-0085 §3): the requested root and its
/// resolution context, the manifest selection, the configured standard
/// library root, and the compiler resource policy. A request with a
/// different key gets a different host; a same-key request reobserves the
/// retained one.
#[derive(Clone, Debug, PartialEq, Eq)]
struct HostKey {
    working_directory: String,
    root_source: String,
    source_manifest_path: Option<String>,
    std_root: Option<String>,
    workers: usize,
}

struct RetainedHost {
    key: HostKey,
    host: FilesystemCompilerHost,
}

/// The one-host executor of this slice. Bounded multi-host retention is the
/// qualification phase's (ADR-0085 §7, RUE-2129).
pub(crate) struct Executor {
    retained: Option<RetainedHost>,
}

impl Executor {
    pub(crate) fn new() -> Self {
        Self { retained: None }
    }
}

/// The parts of a request that must parse before any host is touched.
struct Parsed {
    options: CompileOptions,
    format: ErrorFormat,
    color: ColorChoice,
    compiler_config: CompilerSessionConfig,
    path_context: HostPathContext,
}

fn parse(request: &BuildRequest) -> Result<Parsed, String> {
    let target = request
        .target
        .parse::<rue_target::Target>()
        .map_err(|error| format!("target `{}`: {error}", request.target))?;
    let opt_level = request
        .opt_level
        .parse::<rue_compiler::OptLevel>()
        .map_err(|error| format!("optimization level `{}`: {error}", request.opt_level))?;
    let preview_features = request
        .preview_features
        .iter()
        .map(|name| {
            name.parse::<PreviewFeature>()
                .map_err(|error| format!("preview feature `{name}`: {error}"))
        })
        .collect::<Result<_, _>>()?;
    let compiler_config = CompilerSessionConfig::with_workers(request.workers)
        .map_err(|error| format!("workers {}: {error}", request.workers))?;
    let path_context = HostPathContext::from_working_directory(&request.working_directory)
        .map_err(|error| format!("working directory: {error}"))?;
    Ok(Parsed {
        options: CompileOptions {
            target,
            linker: LinkerMode::Internal,
            opt_level,
            preview_features,
            link_archives: request.link_archives.iter().map(PathBuf::from).collect(),
            root_selection: RootSelection::Executable,
        },
        format: match request.error_format {
            DiagnosticFormat::Text => ErrorFormat::Text,
            DiagnosticFormat::Json => ErrorFormat::Json,
        },
        color: if request.color {
            ColorChoice::Always
        } else {
            ColorChoice::Never
        },
        compiler_config,
        path_context,
    })
}

impl Executor {
    /// The host for `key`: the retained one reobserved when the key matches,
    /// a fresh one otherwise. A failed observation is the request's answer
    /// (rendered like the direct path's) and leaves no half-open host behind.
    fn host(
        &mut self,
        key: HostKey,
        request: &BuildRequest,
        parsed: &Parsed,
    ) -> Result<&mut FilesystemCompilerHost, SourceLoadError> {
        let reuse = self
            .retained
            .as_ref()
            .is_some_and(|retained| retained.key == key);
        if reuse {
            let retained = self.retained.as_mut().expect("checked above");
            // A pre-commit failure keeps the prior coherent closure, so the
            // host stays retained; the request is answered with the failure.
            retained.host.reobserve()?;
            return Ok(&mut retained.host);
        }
        self.retained = None;
        let host = FilesystemCompilerHost::open(HostOpenRequest {
            root_source: &request.root_source,
            source_manifest_path: request.source_manifest_path.as_deref(),
            std_root: request.std_root.as_deref().map(Path::new),
            compiler_config: parsed.compiler_config.clone(),
            path_context: &parsed.path_context,
        })?;
        let retained = self.retained.insert(RetainedHost { key, host });
        Ok(&mut retained.host)
    }
}

impl BuildExecutor for Executor {
    fn build(
        &mut self,
        request: &BuildRequest,
        cancellation: &CompilationCancellation,
    ) -> BuildOutput {
        let parsed = match parse(request) {
            Ok(parsed) => parsed,
            Err(message) => return BuildOutput::failed(format!("invalid request: {message}")),
        };
        let key = HostKey {
            working_directory: request.working_directory.clone(),
            root_source: request.root_source.clone(),
            source_manifest_path: request.source_manifest_path.clone(),
            std_root: request.std_root.clone(),
            workers: request.workers,
        };
        let rejected = |error: SourceLoadError| BuildOutput {
            result: BuildResult::Rejected {
                stderr: format!(
                    "{}\n",
                    render_source_load_error_with_color(error, parsed.format, parsed.color)
                ),
            },
            bytes: Vec::new(),
        };
        let host = match self.host(key, request, &parsed) {
            Ok(host) => host,
            Err(error) => return rejected(error),
        };
        match host.acquire_reached_toolchain_modules_cancellable(&parsed.options, cancellation) {
            Ok(()) => {}
            Err(SourceLoadError::Superseded) => {
                return BuildOutput {
                    result: BuildResult::Canceled,
                    bytes: Vec::new(),
                };
            }
            Err(error) => return rejected(error),
        }
        let CycleTransport { stderr, outcome } = produce_transport(
            host,
            &parsed.options,
            parsed.format,
            parsed.color,
            &request.root_source,
            Path::new(&request.output_path),
            cancellation.clone(),
        );
        match outcome {
            TransportOutcome::Rejected => BuildOutput {
                result: BuildResult::Rejected { stderr },
                bytes: Vec::new(),
            },
            TransportOutcome::Canceled => BuildOutput {
                result: BuildResult::Canceled,
                bytes: Vec::new(),
            },
            TransportOutcome::Ready {
                target,
                bytes,
                destination,
                inputs,
            } => {
                let (path, display_path, source_paths) = destination.into_parts();
                BuildOutput {
                    result: BuildResult::Ready {
                        stderr,
                        target: target.to_string(),
                        destination: DestinationRecord {
                            path: path.display().to_string(),
                            display_path: display_path.display().to_string(),
                            source_paths: source_paths
                                .into_iter()
                                .map(|path| path.display().to_string())
                                .collect(),
                        },
                        inputs: inputs.into_iter().map(input_record).collect(),
                        bytes: bytes.len() as u64,
                    },
                    bytes,
                }
            }
        }
    }

    fn retained_hosts(&self) -> u32 {
        u32::from(self.retained.is_some())
    }
}

fn input_record(input: rue_driver::WatchInput) -> InputRecord {
    let parts = input.into_parts();
    InputRecord {
        requested_path: parts.requested_path.display().to_string(),
        canonical_path: parts.canonical_path.display().to_string(),
        fingerprint: parts.fingerprint,
        symlink_boundary: parts
            .symlink_boundary
            .map(|path| path.display().to_string()),
        symlink_route: parts.symlink_route,
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    struct Project {
        directory: tempfile::TempDir,
    }

    impl Project {
        fn new(source: &str) -> Self {
            let directory = tempfile::tempdir().unwrap();
            fs::write(directory.path().join("main.rue"), source).unwrap();
            Self { directory }
        }

        fn write(&self, source: &str) {
            fs::write(self.directory.path().join("main.rue"), source).unwrap();
        }

        fn request(&self, output: &str) -> BuildRequest {
            BuildRequest {
                working_directory: self.directory.path().display().to_string(),
                root_source: "main.rue".into(),
                output_path: output.into(),
                source_manifest_path: None,
                std_root: None,
                workers: 1,
                target: rue_target::Target::host().unwrap().to_string(),
                opt_level: "O0".into(),
                preview_features: Vec::new(),
                link_archives: Vec::new(),
                error_format: DiagnosticFormat::Text,
                color: false,
            }
        }
    }

    const GOOD: &str = "fn main() -> i32 { 0 }\n";
    const BROKEN: &str = "fn main() -> i32 {\n    let x: i32 = \"nope\";\n    x\n}\n";

    #[test]
    fn a_retained_host_serves_edit_fix_and_revert_with_fresh_parity() {
        let project = Project::new(GOOD);
        let mut executor = Executor::new();
        let cancellation = CompilationCancellation::new();
        assert_eq!(executor.retained_hosts(), 0);

        let first = executor.build(&project.request("app"), &cancellation);
        let BuildResult::Ready {
            stderr,
            bytes: announced,
            destination,
            inputs,
            ..
        } = &first.result
        else {
            panic!("a good program is ready: {:?}", first.result);
        };
        assert_eq!(stderr, "");
        assert_eq!(*announced, first.bytes.len() as u64);
        assert!(!first.bytes.is_empty());
        assert_eq!(destination.display_path, "app");
        assert!(
            destination.path.ends_with("/app"),
            "the destination is anchored at the request's cwd: {}",
            destination.path
        );
        assert!(
            inputs
                .iter()
                .any(|input| input.requested_path.ends_with("main.rue")),
            "the root is an observed input"
        );
        assert_eq!(executor.retained_hosts(), 1);
        assert!(
            !project.directory.path().join("app").exists(),
            "the service publishes nothing; the client does"
        );

        // An edit that breaks the program is answered exactly as a fresh
        // host answers it.
        project.write(BROKEN);
        let broken = executor.build(&project.request("app"), &cancellation);
        let BuildResult::Rejected { stderr } = &broken.result else {
            panic!("a broken program is rejected: {:?}", broken.result);
        };
        assert!(stderr.contains("E0206"), "{stderr}");
        let fresh = Executor::new().build(&project.request("app"), &cancellation);
        let BuildResult::Rejected {
            stderr: fresh_stderr,
        } = &fresh.result
        else {
            panic!("fresh host: {:?}", fresh.result);
        };
        assert_eq!(stderr, fresh_stderr);
        assert_eq!(
            executor.retained_hosts(),
            1,
            "a rejected program keeps the host"
        );

        // The fix, then the revert, over the same host.
        project.write(GOOD);
        let fixed = executor.build(&project.request("app"), &cancellation);
        assert!(matches!(fixed.result, BuildResult::Ready { .. }));
        assert_eq!(
            fixed.bytes, first.bytes,
            "the same program links to the same bytes"
        );
        assert_eq!(executor.retained_hosts(), 1);
    }

    #[test]
    fn a_changed_host_key_replaces_the_host_and_a_bad_request_touches_none() {
        let project = Project::new(GOOD);
        let mut executor = Executor::new();
        let cancellation = CompilationCancellation::new();
        let mut bad = project.request("app");
        bad.target = "z80-cpm".into();
        let answer = executor.build(&bad, &cancellation);
        assert!(
            matches!(
                &answer.result,
                BuildResult::Failed { message, internal: false } if message.contains("target `z80-cpm`")
            ),
            "{:?}",
            answer.result
        );
        assert_eq!(executor.retained_hosts(), 0);

        let first = executor.build(&project.request("app"), &cancellation);
        assert!(matches!(first.result, BuildResult::Ready { .. }));
        let mut other_policy = project.request("app");
        other_policy.workers = 2;
        let second = executor.build(&other_policy, &cancellation);
        assert!(matches!(second.result, BuildResult::Ready { .. }));
        assert_eq!(
            executor.retained_hosts(),
            1,
            "one host at a time: the new key replaced the old host"
        );
        assert_eq!(first.bytes, second.bytes);
    }

    #[test]
    fn a_canceled_request_produces_nothing_and_the_host_recovers() {
        let project = Project::new(GOOD);
        let mut executor = Executor::new();
        let canceled = CompilationCancellation::new();
        canceled.cancel();
        let answer = executor.build(&project.request("app"), &canceled);
        assert!(
            matches!(answer.result, BuildResult::Canceled),
            "{:?}",
            answer.result
        );
        assert!(answer.bytes.is_empty());
        let live = executor.build(&project.request("app"), &CompilationCancellation::new());
        assert!(matches!(live.result, BuildResult::Ready { .. }));
    }

    #[test]
    fn a_missing_root_is_the_direct_source_load_failure() {
        let project = Project::new(GOOD);
        let mut executor = Executor::new();
        let mut request = project.request("app");
        request.root_source = "absent.rue".into();
        let answer = executor.build(&request, &CompilationCancellation::new());
        let BuildResult::Rejected { stderr } = &answer.result else {
            panic!("{:?}", answer.result);
        };
        assert!(stderr.contains("absent.rue"), "{stderr}");
        assert!(stderr.ends_with('\n'));
        assert_eq!(executor.retained_hosts(), 0);
    }
}
