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
use rue_compiler::unstable::{
    ColorChoice, CompilationCancellation, SourceInfo, TestCandidateInventory,
};
use rue_compiler::{CompileOptions, CompilerSessionConfig, LinkerMode, RootSelection};
use rue_driver::daemon::{
    BuildExecutor, BuildKind, BuildOutput, BuildRequest, BuildResult, DestinationRecord,
    DiagnosticFormat, InputRecord, TestImageRecord,
};
use rue_driver::{
    FilesystemCompilerHost, HostOpenRequest, HostPathContext, SourceLoadError,
    load_declared_candidates_with_context,
};

use crate::compile::{CycleTransport, TransportOutcome, produce_test_transport, produce_transport};
use crate::test_mode::{inventory_entry_record, prepare_image, produce_listing_transport};
use crate::{DiagnosticOutput, ErrorFormat, render_source_load_error_with_color};

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

/// Daemon sessions use an explicit provisional budget so a long-lived host
/// cannot silently inherit the much larger one-shot defaults. Release
/// calibration may tighten these values without changing ordinary sessions.
const DAEMON_RETAINED_BYTE_BUDGET: u64 = 256 * 1024 * 1024;
const DAEMON_DEPENDENCY_PIN_BUDGET: u64 = 1_000_000;
const DAEMON_AUTOMATIC_WORKERS: usize = 4;

/// The one-host executor of this slice. Bounded multi-host retention is the
/// qualification phase's (ADR-0085 §7, RUE-2129).
pub(crate) struct Executor {
    retained: Option<RetainedHost>,
}

impl Executor {
    pub(crate) fn new() -> Self {
        Self { retained: None }
    }

    fn trim_over_budget(&mut self, byte_budget: u64, pin_budget: u64) {
        let over = self.retained.as_ref().is_some_and(|retained| {
            let retention = retained.host.unstable_metrics().retention();
            retention.retained_bytes as u64 > byte_budget
                || retention.dependency_pins as u64 > pin_budget
        });
        if over {
            self.retained = None;
        }
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
    let workers = if request.workers == 0 {
        DAEMON_AUTOMATIC_WORKERS
    } else {
        request.workers
    };
    let compiler_config = CompilerSessionConfig::with_workers_and_retention(
        workers,
        DAEMON_RETAINED_BYTE_BUDGET,
        DAEMON_DEPENDENCY_PIN_BUDGET,
    )
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
            // A test request roots every test item in the closure; an
            // executable request roots none of them (ADR-0083 §1). Root
            // selection is request data over the one shared host.
            root_selection: match request.artifact {
                BuildKind::Executable | BuildKind::Analysis => RootSelection::Executable,
                BuildKind::TestImage | BuildKind::TestListing => RootSelection::Tests,
            },
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
        // The declared candidate inventory is acquired under the host's read
        // policy in every mode, as the direct path does, so a build rule that
        // writes a broken list fails the build it broke (ADR-0083 §1).
        let candidates = match acquire_candidates(host, request, &parsed) {
            Ok(candidates) => candidates,
            Err(stderr) => {
                return BuildOutput {
                    result: BuildResult::Rejected { stderr },
                    bytes: Vec::new(),
                };
            }
        };
        let output = match request.artifact {
            BuildKind::Executable => {
                let transport = produce_transport(
                    host,
                    &parsed.options,
                    parsed.format,
                    parsed.color,
                    &request.root_source,
                    Path::new(&request.output_path),
                    cancellation.clone(),
                );
                ready_output(transport, |_| None)
            }
            BuildKind::TestImage => {
                let transport = produce_test_transport(
                    host,
                    &parsed.options,
                    parsed.format,
                    parsed.color,
                    candidates.as_ref(),
                    Path::new(&request.output_path),
                    cancellation.clone(),
                );
                let multi_module_closure = host.published_user_module_count() > 1;
                let color = parsed.color;
                ready_output(transport, move |published| {
                    Some(Box::new(
                        prepare_image(published, multi_module_closure, color).into_record(),
                    ))
                })
            }
            BuildKind::Analysis => {
                match crate::emit::produce_transport(
                    host,
                    &[crate::emit::EmitStage::Air],
                    parsed.options.clone(),
                    parsed.format,
                    parsed.color,
                    cancellation,
                ) {
                    None => BuildOutput {
                        result: BuildResult::Canceled,
                        bytes: Vec::new(),
                    },
                    Some(presentation) => BuildOutput {
                        result: BuildResult::Presentation {
                            ok: presentation.ok,
                            writes: presentation.writes,
                        },
                        bytes: Vec::new(),
                    },
                }
            }
            BuildKind::TestListing => {
                match produce_listing_transport(
                    host,
                    &parsed.options,
                    parsed.format,
                    parsed.color,
                    cancellation,
                ) {
                    None => BuildOutput {
                        result: BuildResult::Canceled,
                        bytes: Vec::new(),
                    },
                    Some(listing) => BuildOutput {
                        result: BuildResult::Listing {
                            stderr: listing.stderr,
                            entries: listing.entries.map(|entries| {
                                entries.into_iter().map(inventory_entry_record).collect()
                            }),
                        },
                        bytes: Vec::new(),
                    },
                }
            }
        };
        output
    }

    fn retained_hosts(&self) -> u32 {
        u32::from(self.retained.is_some())
    }

    fn retained_charge_bytes(&self) -> u64 {
        self.retained
            .as_ref()
            .map(|retained| retained.host.unstable_metrics().retention().retained_bytes as u64)
            .unwrap_or(0)
    }

    fn dependency_pins(&self) -> u64 {
        self.retained
            .as_ref()
            .map(|retained| retained.host.unstable_metrics().retention().dependency_pins as u64)
            .unwrap_or(0)
    }

    fn retained_byte_budget(&self) -> u64 {
        DAEMON_RETAINED_BYTE_BUDGET
    }

    fn dependency_pin_budget(&self) -> u64 {
        DAEMON_DEPENDENCY_PIN_BUDGET
    }

    fn source_bytes(&self) -> u64 {
        self.retained
            .as_ref()
            .map(|retained| {
                retained
                    .host
                    .source_snapshot()
                    .files()
                    .map(|source| source.source.len() as u64)
                    .sum()
            })
            .unwrap_or(0)
    }

    fn source_files(&self) -> u32 {
        self.retained
            .as_ref()
            .map(|retained| retained.host.source_snapshot().files().count() as u32)
            .unwrap_or(0)
    }

    fn enforce_retention_budget(&mut self) {
        self.trim_over_budget(DAEMON_RETAINED_BYTE_BUDGET, DAEMON_DEPENDENCY_PIN_BUDGET);
    }
}

/// Load and acquire the request's declared test candidates, rendering a
/// failure as the direct path would print it.
fn acquire_candidates(
    host: &mut FilesystemCompilerHost,
    request: &BuildRequest,
    parsed: &Parsed,
) -> Result<Option<TestCandidateInventory>, String> {
    let Some(path) = request.test_candidates_path.as_deref() else {
        return Ok(None);
    };
    let declared = load_declared_candidates_with_context(path, &parsed.path_context)
        .map_err(|message| format!("{message}\n"))?;
    host.acquire_test_candidates(&declared)
        .map(Some)
        .map_err(|errors| {
            let snapshot = host.source_snapshot();
            let sources = snapshot
                .files()
                .map(|source| (source.file_id, SourceInfo::new(source.source, source.path)))
                .collect();
            let diagnostics = DiagnosticOutput::with_color(parsed.format, sources, parsed.color);
            format!("{}\n", diagnostics.render_errors(&errors))
        })
}

/// The wire form of a cycle transport; `companion` projects what rides
/// beside a test image's bytes.
fn ready_output<Published>(
    transport: CycleTransport<Published>,
    companion: impl FnOnce(Published) -> Option<Box<TestImageRecord>>,
) -> BuildOutput {
    let CycleTransport { stderr, outcome } = transport;
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
            published,
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
                    test_image: companion(published),
                },
                bytes,
            }
        }
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
                artifact: BuildKind::Executable,
                working_directory: self.directory.path().display().to_string(),
                root_source: "main.rue".into(),
                output_path: output.into(),
                source_manifest_path: None,
                test_candidates_path: None,
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

    pub(super) const GOOD: &str = "fn main() -> i32 { 0 }\n";
    pub(super) const BROKEN: &str = "fn main() -> i32 {\n    let x: i32 = \"nope\";\n    x\n}\n";

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
    fn daemon_hosts_use_their_explicit_resource_budget() {
        let project = Project::new(GOOD);
        let mut executor = Executor::new();
        let request = project.request("app");
        let cancellation = CompilationCancellation::new();
        let output = executor.build(&request, &cancellation);
        assert!(matches!(output.result, BuildResult::Ready { .. }));
        let metrics = executor
            .retained
            .as_ref()
            .expect("a successful request retains its host")
            .host
            .unstable_metrics()
            .retention();
        assert_eq!(
            metrics.retained_byte_budget as u64,
            DAEMON_RETAINED_BYTE_BUDGET
        );
        assert_eq!(
            metrics.dependency_pin_budget as u64,
            DAEMON_DEPENDENCY_PIN_BUDGET
        );
    }

    #[test]
    fn daemon_executor_releases_runtime_after_error_and_repair() {
        let project = Project::new(GOOD);
        let mut executor = Executor::new();
        let cancellation = CompilationCancellation::new();
        let first = executor.build(&project.request("app"), &cancellation);
        assert!(matches!(first.result, BuildResult::Ready { .. }));
        project.write(BROKEN);
        let broken = executor.build(&project.request("app"), &cancellation);
        assert!(matches!(broken.result, BuildResult::Rejected { .. }));
        project.write(GOOD);
        let repaired = executor.build(&project.request("app"), &cancellation);
        assert!(matches!(repaired.result, BuildResult::Ready { .. }));
        let weak = executor
            .retained
            .as_ref()
            .expect("the repaired request retains its host")
            .host
            .unstable_query_runtime_weak();
        assert!(weak.is_alive(), "the live daemon host owns its runtime");
        drop(executor);
        assert!(
            !weak.is_alive(),
            "dropping the daemon executor must reclaim its query runtime"
        );
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
        let weak = executor
            .retained
            .as_ref()
            .expect("the canceled request still leaves a coherent host")
            .host
            .unstable_query_runtime_weak();
        assert!(weak.is_alive());
        drop(executor);
        assert!(!weak.is_alive());
    }

    #[test]
    fn rotating_the_root_reclaims_the_previous_query_runtime() {
        let project = Project::new(GOOD);
        let mut executor = Executor::new();
        let first = executor.build(
            &project.request("main-app"),
            &CompilationCancellation::new(),
        );
        assert!(matches!(first.result, BuildResult::Ready { .. }));
        let previous = executor
            .retained
            .as_ref()
            .expect("the first root is retained")
            .host
            .unstable_query_runtime_weak();
        assert!(previous.is_alive());

        fs::write(project.directory.path().join("other.rue"), GOOD).unwrap();
        let mut request = project.request("other-app");
        request.root_source = "other.rue".into();
        let second = executor.build(&request, &CompilationCancellation::new());
        assert!(matches!(second.result, BuildResult::Ready { .. }));
        assert!(
            !previous.is_alive(),
            "changing the root must retire the prior runtime"
        );
        let current = executor
            .retained
            .as_ref()
            .expect("the second root is retained")
            .host
            .unstable_query_runtime_weak();
        assert!(current.is_alive());
        drop(executor);
        assert!(!current.is_alive());
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

#[cfg(test)]
mod test_request_tests {
    use std::fs;

    use rue_driver::daemon::UnimportedRecord;

    use super::*;

    const SUITE: &str = "fn add(a: i32, b: i32) -> i32 { a + b }\n\
                         test \"adds\" { @assert(add(1, 2) == 3); }\n\
                         test \"broken\" { let x: i32 = \"nope\"; @assert(x == 1); }\n\
                         fn main() -> i32 { 0 }\n";

    struct Suite {
        directory: tempfile::TempDir,
    }

    impl Suite {
        fn new() -> Self {
            let directory = tempfile::tempdir().unwrap();
            fs::write(directory.path().join("main.rue"), SUITE).unwrap();
            fs::write(
                directory.path().join("extra_tests.rue"),
                "test \"never imported\" { }\n",
            )
            .unwrap();
            fs::write(
                directory.path().join("candidates.txt"),
                "main.rue\nextra_tests.rue\n",
            )
            .unwrap();
            Self { directory }
        }

        fn request(&self, artifact: BuildKind, output: &str) -> BuildRequest {
            BuildRequest {
                artifact,
                working_directory: self.directory.path().display().to_string(),
                root_source: "main.rue".into(),
                output_path: output.into(),
                source_manifest_path: None,
                test_candidates_path: None,
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

    #[test]
    fn a_test_image_carries_the_runner_companion_and_shares_the_host() {
        let suite = Suite::new();
        let mut executor = Executor::new();
        let cancellation = CompilationCancellation::new();

        let mut request = suite.request(BuildKind::TestImage, "image");
        request.test_candidates_path = Some("candidates.txt".into());
        let answer = executor.build(&request, &cancellation);
        let BuildResult::Ready {
            stderr,
            test_image: Some(record),
            bytes: announced,
            ..
        } = &answer.result
        else {
            panic!(
                "a test image is ready with its companion: {:?}",
                answer.result
            );
        };
        assert_eq!(*announced, answer.bytes.len() as u64);
        assert!(
            stderr.contains("E0206"),
            "the closure's analysis failures are on stderr: {stderr}"
        );
        let ids: Vec<&str> = record
            .entries
            .iter()
            .map(|entry| entry.id.as_str())
            .collect();
        assert_eq!(ids, ["main.rue::adds", "main.rue::broken"]);
        assert_eq!(record.compile_failures.len(), 1);
        let failure = &record.compile_failures[0];
        assert_eq!(
            failure.ordinal, record.entries[1].ordinal,
            "the failure is attributed to the broken test"
        );
        assert!(failure.xfail_eligible);
        assert!(failure.payload.starts_with("E0206: "));
        assert_eq!(failure.diagnostics[0]["code"], "E0206");
        assert_eq!(
            failure
                .location
                .as_ref()
                .map(|(file, line, _)| (file.as_str(), *line)),
            Some(("main.rue", 3))
        );
        assert!(!record.multi_module_closure);
        let UnimportedRecord::Files { stderr, files } = &record.unimported else {
            panic!(
                "declared candidates produce a report: {:?}",
                record.unimported
            );
        };
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, "extra_tests.rue");
        assert_eq!(files[0].tests, 1);
        assert!(stderr.contains("extra_tests.rue"), "{stderr}");
        assert_eq!(executor.retained_hosts(), 1);

        // The executable request over the same host neither sees the test
        // closure's failure nor links its tests (ADR-0083 §1).
        let executable =
            executor.build(&suite.request(BuildKind::Executable, "app"), &cancellation);
        let BuildResult::Ready {
            stderr,
            test_image: None,
            ..
        } = &executable.result
        else {
            panic!("{:?}", executable.result);
        };
        assert_eq!(
            stderr, "",
            "a test-only failure never poisons an executable"
        );
        assert_eq!(executor.retained_hosts(), 1);

        // And the listing, over the same host again, lists both and repeats
        // the analysis diagnostics.
        let listing = executor.build(
            &suite.request(BuildKind::TestListing, "unused"),
            &cancellation,
        );
        let BuildResult::Listing {
            stderr,
            entries: Some(entries),
        } = &listing.result
        else {
            panic!("{:?}", listing.result);
        };
        assert!(stderr.contains("E0206"), "{stderr}");
        assert_eq!(entries.len(), 2);
        assert!(listing.bytes.is_empty(), "a listing links nothing");
        assert_eq!(executor.retained_hosts(), 1);

        // A fresh executor renders the same image companion.
        let fresh = Executor::new().build(&request, &cancellation);
        let BuildResult::Ready {
            stderr: fresh_stderr,
            test_image: Some(fresh_record),
            ..
        } = fresh.result
        else {
            panic!("{:?}", fresh.result);
        };
        assert_eq!(&fresh_stderr, stderr_of(&answer.result));
        assert_eq!(*fresh_record, **record);
    }

    fn stderr_of(result: &BuildResult) -> &String {
        match result {
            BuildResult::Ready { stderr, .. }
            | BuildResult::Rejected { stderr }
            | BuildResult::Listing { stderr, .. } => stderr,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn an_analysis_request_renders_the_presentation_over_the_shared_host() {
        let suite = Suite::new();
        let mut executor = Executor::new();
        let cancellation = CompilationCancellation::new();
        let answer = executor.build(&suite.request(BuildKind::Analysis, "unused"), &cancellation);
        let BuildResult::Presentation { ok, writes } = &answer.result else {
            panic!("{:?}", answer.result);
        };
        assert!(ok);
        assert!(answer.bytes.is_empty(), "an analysis links nothing");
        let stdout: String = writes
            .iter()
            .filter(|write| write.stream == rue_driver::daemon::OutputStream::Stdout)
            .map(|write| write.text.as_str())
            .collect();
        assert!(stdout.starts_with("=== AIR ===\n"), "{stdout}");
        assert!(stdout.contains("function main:"), "{stdout}");
        // The executable root set: the test bodies' failure is not this
        // request's to report.
        assert!(
            writes
                .iter()
                .all(|write| write.stream == rue_driver::daemon::OutputStream::Stdout),
            "{writes:?}"
        );
        assert_eq!(executor.retained_hosts(), 1);

        // A fresh executor writes the same sequence.
        let fresh =
            Executor::new().build(&suite.request(BuildKind::Analysis, "unused"), &cancellation);
        let BuildResult::Presentation {
            writes: fresh_writes,
            ..
        } = fresh.result
        else {
            panic!("{:?}", fresh.result);
        };
        assert_eq!(&fresh_writes, writes);

        // A rejected program: its diagnostics on stderr, nothing on stdout,
        // not ok; then a canceled request, then recovery over the same host.
        fs::write(
            suite.directory.path().join("main.rue"),
            super::tests::BROKEN,
        )
        .unwrap();
        let rejected = executor.build(&suite.request(BuildKind::Analysis, "unused"), &cancellation);
        let BuildResult::Presentation { ok, writes } = &rejected.result else {
            panic!("{:?}", rejected.result);
        };
        assert!(!ok);
        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0].stream, rue_driver::daemon::OutputStream::Stderr);
        assert!(writes[0].text.contains("E0206"), "{}", writes[0].text);
        let canceled = CompilationCancellation::new();
        canceled.cancel();
        let answer = executor.build(&suite.request(BuildKind::Analysis, "unused"), &canceled);
        assert!(matches!(answer.result, BuildResult::Canceled));
        fs::write(suite.directory.path().join("main.rue"), SUITE).unwrap();
        let recovered =
            executor.build(&suite.request(BuildKind::Analysis, "unused"), &cancellation);
        assert!(matches!(
            recovered.result,
            BuildResult::Presentation { ok: true, .. }
        ));
        assert_eq!(executor.retained_hosts(), 1);
    }

    #[test]
    fn a_broken_candidate_list_is_the_direct_failure_and_a_listing_can_be_canceled() {
        let suite = Suite::new();
        let mut executor = Executor::new();
        let mut request = suite.request(BuildKind::TestImage, "image");
        request.test_candidates_path = Some("missing.txt".into());
        let answer = executor.build(&request, &CompilationCancellation::new());
        let BuildResult::Rejected { stderr } = &answer.result else {
            panic!("{:?}", answer.result);
        };
        assert!(stderr.contains("missing.txt"), "{stderr}");

        let canceled = CompilationCancellation::new();
        canceled.cancel();
        let listing = executor.build(&suite.request(BuildKind::TestListing, "unused"), &canceled);
        assert!(matches!(listing.result, BuildResult::Canceled));
        let live = executor.build(
            &suite.request(BuildKind::TestListing, "unused"),
            &CompilationCancellation::new(),
        );
        assert!(matches!(
            live.result,
            BuildResult::Listing {
                entries: Some(_),
                ..
            }
        ));
    }
}

#[cfg(test)]
#[path = "daemon_qualification_tests.rs"]
mod qualification_tests;
