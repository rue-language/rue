//! The client side of compiling through the local compiler service
//! (ADR-0085 §2, §3, §5, §6): decide whether this invocation may use the
//! service, capture the request at the process boundary, submit it, and
//! publish what comes back exactly as the direct path would have.

use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};

use rue_compiler::{CompileOptions, LinkerMode};
use rue_driver::daemon::{
    self, BuildKind, BuildRequest, BuildResult, DaemonScope, DiagnosticFormat, EndpointRoot,
    InputRecord, ProcessLauncher, RequestMeasurement, ServiceInfo, StartOptions, Submission,
    SubmitError,
};
use rue_driver::{HostPathContext, WatchInput, WatchInputParts};
use rue_error::ErrorCode;
use rue_perf_schema::{
    DAEMON_PERFORMANCE_RECORD_KIND, DAEMON_PERFORMANCE_SCHEMA_VERSION, DaemonArtifact,
    DaemonEndpoint, DaemonIdentity, DaemonInvocationRecord, DaemonTiming, DaemonWork,
    ExecutionPath,
};
use sha2::{Digest, Sha256};

use crate::compile::{Announcement, announce};
use crate::emit::{self, EmitStage};
use crate::output::{PublicationDestination, PublishRequest, publish_watch_executable};
use crate::{
    DiagnosticOutput, DriverMode, ErrorFormat, Options, VERSION, compile_pool_jobs,
    driver_failure_exit_code, ice_diagnostic, render_driver_error, render_internal_error,
    test_mode, test_mode_jobs, test_repro_env, test_repro_flags,
};

/// `--daemon=<mode>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum DaemonMode {
    /// Compile in this process; never discover or contact a service.
    #[default]
    Off,
    /// Use or start the service for supported requests; run the rest, and a
    /// request the service refuses before any work, directly.
    Auto,
    /// The service must run the request; anything else is an error.
    Required,
}

impl DaemonMode {
    pub(crate) fn all_names() -> &'static str {
        "off, auto, required"
    }
}

impl std::str::FromStr for DaemonMode {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        match text {
            "off" => Ok(Self::Off),
            "auto" => Ok(Self::Auto),
            "required" => Ok(Self::Required),
            other => Err(format!(
                "unknown --daemon mode `{other}` (valid modes: {})",
                Self::all_names()
            )),
        }
    }
}

/// Why an invocation cannot run through the service, if it cannot. This is
/// the support table of ADR-0085 §2: each of these keeps its existing direct
/// stream and lifecycle contract until its own slice moves it.
pub(crate) fn unsupported_reason(options: &Options) -> Option<&'static str> {
    if options.watch {
        return Some("--watch runs directly");
    }
    if !options.emit_stages.is_empty() && options.emit_stages != [EmitStage::Air] {
        return Some("--emit stages other than a sole `air` run directly");
    }
    if !matches!(options.linker, LinkerMode::Internal) {
        return Some("a system linker runs directly");
    }
    if options.time_passes {
        return Some("--time-passes runs directly");
    }
    if options.benchmark_json {
        return Some("--benchmark-json runs directly");
    }
    if options.log_level != crate::LogLevel::Off {
        return Some("compiler tracing (--log-level) runs directly");
    }
    if std::env::var_os("RUST_LOG").is_some_and(|value| !value.is_empty()) {
        return Some("compiler tracing (RUST_LOG) runs directly");
    }
    None
}

/// How a service attempt ended.
pub(crate) enum Outcome {
    /// The invocation is complete; exit with this status.
    Exit(i32),
    /// The service did not take the request and no work began; compile
    /// directly.
    Direct,
}

/// Write the explicit performance sidecar without touching the normal output
/// streams. The runner pairs these records; all identity and phase fields are
/// derived here from the actual request/service reply.
pub(crate) struct PerformanceObservation<'a> {
    pub(crate) artifact: BuildKind,
    pub(crate) path: ExecutionPath,
    pub(crate) service: Option<&'a ServiceInfo>,
    pub(crate) measurement: Option<&'a RequestMeasurement>,
    pub(crate) direct_workers: Option<u32>,
    pub(crate) accepted_input_identity: Option<&'a str>,
    pub(crate) transfer_ns: Option<u64>,
}

pub(crate) fn write_performance_sidecar(
    options: &Options,
    path_context: &HostPathContext,
    observation: PerformanceObservation<'_>,
) {
    let Some(sidecar) = options.daemon_performance_json.as_deref() else {
        return;
    };
    let PerformanceObservation {
        artifact,
        path,
        service,
        measurement,
        direct_workers,
        accepted_input_identity,
        transfer_ns,
    } = observation;
    let (preparation_ns, execution_ns) = test_mode::performance_phases();
    let executed = test_mode::execution_count();
    let input_sha256 = accepted_input_identity.map(|identity| {
        let mut identity_input = format!(
            "{}\0{}\0{}\0{}\0{}\0{}",
            path_context.working_directory().display(),
            options.source_path,
            options.source_manifest_path.as_deref().unwrap_or(""),
            options.target,
            options.opt_level.name(),
            std::env::var_os("RUE_STD_PATH")
                .map(|value| value.to_string_lossy().into_owned())
                .unwrap_or_default(),
        );
        identity_input.push('\0');
        identity_input.push_str(identity);
        hex_digest(identity_input.as_bytes())
    });
    let (compiler_image_sha256, protocol_version, daemon_generation, session_generation) =
        match (path, service, measurement) {
            (ExecutionPath::Daemon, Some(service), Some(measurement)) => (
                None,
                service.protocol_version,
                format!("{}:{}", service.pid, service.started_at_unix_ms),
                measurement.session_generation.to_string(),
            ),
            (ExecutionPath::Daemon, Some(service), None) => (
                None,
                service.protocol_version,
                format!("{}:{}", service.pid, service.started_at_unix_ms),
                "unknown".into(),
            ),
            _ => (
                None,
                0,
                format!("fresh:{}", std::process::id()),
                "fresh".into(),
            ),
        };
    let artifact = match artifact {
        BuildKind::Executable => DaemonArtifact::Executable,
        BuildKind::Analysis | BuildKind::TestListing => DaemonArtifact::Analysis,
        BuildKind::TestImage => DaemonArtifact::TestImage,
    };
    let record = DaemonInvocationRecord {
        artifact,
        record_kind: DAEMON_PERFORMANCE_RECORD_KIND.into(),
        schema_version: DAEMON_PERFORMANCE_SCHEMA_VERSION,
        endpoint: DaemonEndpoint {
            path,
            identity: DaemonIdentity {
                compiler_image_sha256,
                compiler_version: VERSION.into(),
                compiler_build_profile: crate::compiler_build_profile(),
                protocol_version,
                request_ticket: measurement.map(|value| value.ticket),
                daemon_generation,
                session_generation,
                input_sha256,
                target: options.target.to_string(),
                requested_workers: compile_pool_jobs(&options.mode, options.jobs) as u32,
                workers: measurement
                    .map(|value| value.workers)
                    .or(direct_workers)
                    .unwrap_or(0),
                optimization: options.opt_level.name().into(),
                preview_features: {
                    let mut features: Vec<_> = options
                        .preview_features
                        .iter()
                        .map(ToString::to_string)
                        .collect();
                    features.sort();
                    features
                },
                test_jobs: if options.mode == DriverMode::Test {
                    test_mode_jobs(options.jobs) as u32
                } else {
                    0
                },
            },
            timing: DaemonTiming {
                client_started_ns: None,
                // The external runner owns the client clock. A compiler
                // sidecar must not present its process-local clock as that
                // boundary; the paired report fills it from the runner.
                client_spawn_to_exit_ns: None,
                executor_ns: measurement.map(|value| value.executor_ns),
                observation_ns: measurement.and_then(|value| value.observation_ns),
                link_ns: measurement.and_then(|value| value.link_ns),
                transfer_ns,
                test_preparation_ns: preparation_ns,
                test_execution_ns: execution_ns,
            },
            output_sha256: None,
            diagnostics_sha256: None,
            exit_code: None,
            behavior_sha256: None,
            prepared_image_sha256: test_mode::prepared_image_sha256(),
            tests_executed: executed > 0,
            tests_executed_count: (executed > 0).then_some(executed),
            execution_proof_sha256: None,
        },
        work: measurement.map(|value| DaemonWork {
            query_claims: value.query_claims,
            query_reuses: value.query_reuses,
            source_bytes: value.source_bytes,
            retained_charge_bytes: value.retained_charge_bytes,
            dependency_pins: value.dependency_pins,
            response_bytes: None,
        }),
    };
    let bytes = serde_json::to_vec_pretty(&record).unwrap_or_else(|error| {
        eprintln!("error: could not encode performance sidecar: {error}");
        std::process::exit(1);
    });
    // A sidecar always names a new file. Atomic exclusive creation also
    // protects sources, symlinks, and the compiled output if the destination
    // changes after the early preflight.
    let written = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(sidecar)
        .and_then(|mut file| file.write_all(&bytes));
    if let Err(error) = written {
        eprintln!(
            "error: could not write performance sidecar {}: {error}",
            Path::new(sidecar).display()
        );
        std::process::exit(1);
    }
}

/// The measurement surface is intentionally narrower than the compiler CLI.
/// These combinations need additional owned input identities or repeated-run
/// semantics before they can produce a comparable performance record.
pub(crate) fn validate_performance_options(
    options: &Options,
    path_context: &HostPathContext,
) -> Result<(), String> {
    let Some(sidecar) = &options.daemon_performance_json else {
        return Ok(());
    };
    if options.watch
        || options.benchmark_json
        || options.time_passes
        || options.module_manifest_path.is_some()
        || options.test_candidates_path.is_some()
        || !options.link_archives.is_empty()
        || options.linker != LinkerMode::Internal
        || options.test.list
        || (!options.emit_stages.is_empty() && options.emit_stages != [EmitStage::Air])
    {
        return Err("Error: --daemon-performance-json supports one-shot executable, AIR, and test runs with the internal linker; watch, benchmark/timing output, module manifests, test candidates, archives, and test listings are not measured".into());
    }
    let path = path_context.anchor(Path::new(sidecar));
    match std::fs::symlink_metadata(&path) {
        Ok(_) => {
            return Err(format!(
                "Error: performance sidecar must name a new file: {}",
                path.display()
            ));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(format!(
                "Error: cannot inspect performance sidecar {}: {error}",
                path.display()
            ));
        }
    }
    let output = path_context.anchor(Path::new(&options.output_path));
    let output = output.to_string_lossy();
    crate::output::preflight_destination_with_display(&path, Path::new(sidecar), [output.as_ref()])
        .map_err(|_| {
            "Error: performance sidecar cannot also be the executable output".to_string()
        })?;
    Ok(())
}

/// Hash the exact accepted read closure used by both fresh and daemon
/// callers. Watch inputs retain requested/canonical spellings, fingerprints,
/// and symlink routes, which covers source, standard-library, manifest, and
/// negative observations without rereading any path after compilation.
pub(crate) fn watch_inputs_identity(inputs: &[WatchInput]) -> String {
    let mut entries = inputs
        .iter()
        .cloned()
        .map(|input| {
            let parts = input.into_parts();
            (
                parts.requested_path.display().to_string(),
                parts.canonical_path.display().to_string(),
                parts.fingerprint,
                parts
                    .symlink_boundary
                    .map(|path| path.display().to_string()),
                parts.symlink_route,
            )
        })
        .collect::<Vec<_>>();
    entries.sort();
    let bytes = serde_json::to_vec(&entries).unwrap_or_default();
    hex_digest(&bytes)
}

fn hex_digest(bytes: &[u8]) -> String {
    let mut digest = Sha256::new();
    digest.update(bytes);
    format!("{:x}", digest.finalize())
}

/// What a test run needs beyond the compile: decided before submission so
/// the request names the image's staging path.
struct TestPlan {
    seed: u64,
    run_root: PathBuf,
    image_path: PathBuf,
}

/// Compile `options` through the service, or report why not.
pub(crate) fn run(options: &Options, path_context: &HostPathContext) -> Outcome {
    let format = options.error_format;
    let failure_exit = driver_failure_exit_code(&options.mode);
    let fail = |message: String| -> Outcome {
        eprintln!("{}", render_daemon_error(format, message));
        Outcome::Exit(failure_exit)
    };
    // Falling through to the direct path is only ever allowed while it is
    // proven that no work began (ADR-0085 §6).
    let fallback = |reason: String| -> Outcome {
        match options.daemon {
            DaemonMode::Required => fail(format!("--daemon=required: {reason}")),
            DaemonMode::Auto | DaemonMode::Off => Outcome::Direct,
        }
    };

    if let Some(reason) = unsupported_reason(options) {
        return fallback(format!(
            "{reason}; this invocation is not supported by the service"
        ));
    }

    let kind = match (&options.mode, options.test.list) {
        (DriverMode::Compile, _) if !options.emit_stages.is_empty() => BuildKind::Analysis,
        (DriverMode::Compile, _) => BuildKind::Executable,
        (DriverMode::Test, true) => BuildKind::TestListing,
        (DriverMode::Test, false) => BuildKind::TestImage,
    };
    // A test image is staged in the run's own private directory, never at a
    // path the user named (ADR-0083 §2); it exists before submission so the
    // service's destination preflight sees it.
    let test_plan = match kind {
        BuildKind::TestImage => {
            let seed = options.test.seed.unwrap_or_else(test_mode::fresh_seed);
            match test_mode::service_run_root(seed) {
                Ok((run_root, image_path)) => Some(TestPlan {
                    seed,
                    run_root,
                    image_path,
                }),
                Err(message) => {
                    eprintln!("error: {message}");
                    return Outcome::Exit(failure_exit);
                }
            }
        }
        BuildKind::Executable | BuildKind::TestListing | BuildKind::Analysis => None,
    };
    let output_path = match &test_plan {
        Some(plan) => plan.image_path.display().to_string(),
        None => options.output_path.clone(),
    };
    let discard_plan = |plan: &TestPlan| {
        let _ = std::fs::remove_file(&plan.image_path);
        let _ = std::fs::remove_dir(&plan.run_root);
    };

    let request = capture(options, path_context, kind, output_path);
    let scope_dir = match &options.daemon_scope {
        Some(scope) => path_context.anchor(Path::new(scope)),
        None => {
            let root = path_context.anchor(Path::new(&options.source_path));
            root.parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| path_context.working_directory().to_path_buf())
        }
    };
    let submitted = (|| -> Result<
        (
            BuildResult,
            Vec<u8>,
            Option<rue_driver::daemon::RequestMeasurement>,
            rue_driver::daemon::ServiceInfo,
            Option<u64>,
        ),
        Outcome,
    > {
        let scope = DaemonScope::new(&scope_dir, options.daemon_isolation.as_deref())
            .map_err(|error| fallback(format!("no service scope: {error}")))?;
        let root = EndpointRoot::resolve()
            .map_err(|error| fallback(format!("no service endpoint: {error}")))?;
        let launcher = ProcessLauncher::current_executable()
            .map_err(|error| fallback(format!("no service launcher: {error}")))?;
        let mut started =
            daemon::start_connection(scope, &root, &StartOptions::default(), &launcher)
                .map_err(|error| fallback(format!("the service is unavailable: {error}")))?;
        let service = started.connection.service().clone();
        let ticket = match started.connection.submit_build(request) {
            Ok(Submission::Accepted { ticket, .. }) => ticket,
            Ok(Submission::Rejected { reason }) => {
                return Err(fallback(format!(
                    "the service refused the request: {reason}"
                )));
            }
            Err(SubmitError::BeforeSubmission(error)) => {
                return Err(fallback(format!(
                    "the request could not be submitted: {error}"
                )));
            }
            // The service may have started the request: not a fallback,
            // however the invocation was configured (ADR-0085 §6).
            Err(error @ SubmitError::Ambiguous(_)) => return Err(fail(error.to_string())),
        };
        started
            .connection
            .await_build_with_measurement(ticket, options.daemon_performance_json.is_some())
            .map(|(result, bytes, measurement, transfer_ns)| {
                (
                    result,
                    bytes,
                    measurement,
                    service,
                    transfer_ns,
                )
            })
            .map_err(|error| {
            // A compiler panic aborts the service; it leaves a record naming
            // the request it ended, which is the client's only account of
            // the defect.
            let pid = started.connection.service().pid;
            if let Some(record) = started.endpoint.crash_record()
                && record.pid == pid
                && record.ticket == Some(ticket)
            {
                eprintln!(
                    "{}",
                    render_service_panic(format, &record.message, record.location)
                );
                return Outcome::Exit(failure_exit);
            }
            fail(format!(
                "the compiler service ended the request without an answer: {error}"
            ))
            })
    })();
    let (result, bytes, measurement, service, transfer_ns) = match submitted {
        Ok(answer) => answer,
        Err(outcome) => {
            if let Some(plan) = &test_plan {
                discard_plan(plan);
            }
            return outcome;
        }
    };

    match result {
        BuildResult::Rejected { stderr } => {
            eprint!("{stderr}");
            let accepted_input_identity = measurement
                .as_ref()
                .and_then(|value| value.input_sha256.as_deref());
            write_performance_sidecar(
                options,
                path_context,
                PerformanceObservation {
                    artifact: kind,
                    path: ExecutionPath::Daemon,
                    service: Some(&service),
                    measurement: measurement.as_ref(),
                    direct_workers: None,
                    accepted_input_identity,
                    transfer_ns,
                },
            );
            if let Some(plan) = &test_plan {
                discard_plan(plan);
            }
            // A rejected program: compile mode's ordinary status, or the test
            // runner's "the run did not happen" (ADR-0083 §2).
            Outcome::Exit(failure_exit)
        }
        BuildResult::Listing { stderr, entries } => {
            let exit =
                test_mode::run_service_listing(stderr.clone(), entries, &options.test).code();
            let accepted_input_identity = measurement
                .as_ref()
                .and_then(|value| value.input_sha256.as_deref());
            write_performance_sidecar(
                options,
                path_context,
                PerformanceObservation {
                    artifact: kind,
                    path: ExecutionPath::Daemon,
                    service: Some(&service),
                    measurement: measurement.as_ref(),
                    direct_workers: None,
                    accepted_input_identity,
                    transfer_ns,
                },
            );
            Outcome::Exit(exit)
        }
        BuildResult::Presentation { ok, writes } => {
            let accepted_input_identity = measurement
                .as_ref()
                .and_then(|value| value.input_sha256.as_deref());
            write_performance_sidecar(
                options,
                path_context,
                PerformanceObservation {
                    artifact: kind,
                    path: ExecutionPath::Daemon,
                    service: Some(&service),
                    measurement: measurement.as_ref(),
                    direct_workers: None,
                    accepted_input_identity,
                    transfer_ns,
                },
            );
            match emit::replay(emit::EmitTransport { ok, writes }) {
                Ok(()) => Outcome::Exit(0),
                Err(()) => Outcome::Exit(1),
            }
        }
        BuildResult::Canceled => fail("the compiler service canceled the request".into()),
        BuildResult::Failed { message, internal } => {
            if let Some(plan) = &test_plan {
                discard_plan(plan);
            }
            if internal {
                eprintln!("{}", render_internal_error(format, message));
                Outcome::Exit(failure_exit)
            } else {
                write_performance_sidecar(
                    options,
                    path_context,
                    PerformanceObservation {
                        artifact: kind,
                        path: ExecutionPath::Daemon,
                        service: Some(&service),
                        measurement: measurement.as_ref(),
                        direct_workers: None,
                        accepted_input_identity: measurement
                            .as_ref()
                            .and_then(|value| value.input_sha256.as_deref()),
                        transfer_ns,
                    },
                );
                fail(message)
            }
        }
        BuildResult::Ready {
            stderr,
            target,
            destination,
            inputs,
            test_image,
            ..
        } => {
            eprint!("{stderr}");
            let target = match target.parse::<rue_target::Target>() {
                Ok(target) => target,
                Err(error) => return fail(format!("the service named target `{target}`: {error}")),
            };
            let accepted_input_identity = measurement
                .as_ref()
                .and_then(|value| value.input_sha256.as_deref());
            let inputs: Vec<WatchInput> = inputs.into_iter().map(watch_input).collect();
            let destination = PublicationDestination::from_parts(
                PathBuf::from(destination.path),
                PathBuf::from(destination.display_path),
                destination
                    .source_paths
                    .into_iter()
                    .map(PathBuf::from)
                    .collect(),
            );
            let publication = {
                let _span = tracing::info_span!("output_write", driver_phase = true).entered();
                publish_watch_executable(
                    PublishRequest {
                        destination,
                        bytes: &bytes,
                        target,
                    },
                    &inputs,
                )
            };
            // `InputsChanged` included: a source that changed between the
            // compile and the rename is refused, not installed, and the last
            // successfully published file stays (ADR-0085 §5).
            if let Err(error) = publication {
                let diagnostics = DiagnosticOutput::new(format, Vec::new());
                eprintln!("{}", diagnostics.render_error(&error.into_compile_error()));
                if let Some(plan) = &test_plan {
                    discard_plan(plan);
                }
                return Outcome::Exit(failure_exit);
            }
            match test_plan {
                None => {
                    write_performance_sidecar(
                        options,
                        path_context,
                        PerformanceObservation {
                            artifact: kind,
                            path: ExecutionPath::Daemon,
                            service: Some(&service),
                            measurement: measurement.as_ref(),
                            direct_workers: None,
                            accepted_input_identity,
                            transfer_ns,
                        },
                    );
                    announce(
                        Announcement::Completed,
                        &options.source_path,
                        &options.output_path,
                        &CompileOptions {
                            target,
                            linker: LinkerMode::Internal,
                            ..CompileOptions::default()
                        },
                    );
                    Outcome::Exit(0)
                }
                Some(plan) => {
                    let Some(record) = test_image else {
                        discard_plan(&plan);
                        return fail(
                            "the compiler service answered a test request without its inventory"
                                .into(),
                        );
                    };
                    let std_root = std::env::var_os("RUE_STD_PATH").map(PathBuf::from);
                    let exit = test_mode::run_service_image(test_mode::ServiceRun {
                        record: *record,
                        image_path: &plan.image_path,
                        run_root: &plan.run_root,
                        options: &options.test,
                        root: &options.source_path,
                        repro_root: &test_mode::absolute_spelling_at(
                            Path::new(&options.source_path),
                            path_context.working_directory(),
                        ),
                        repro_flags: &test_repro_flags(options, path_context),
                        repro_env: &test_repro_env(std_root.as_deref()),
                        jobs: test_mode_jobs(options.jobs),
                        target: options.target,
                        opt_level: options.opt_level,
                        candidates_declared: options.test_candidates_path.is_some(),
                        seed: plan.seed,
                    });
                    write_performance_sidecar(
                        options,
                        path_context,
                        PerformanceObservation {
                            artifact: kind,
                            path: ExecutionPath::Daemon,
                            service: Some(&service),
                            measurement: measurement.as_ref(),
                            direct_workers: None,
                            accepted_input_identity,
                            transfer_ns,
                        },
                    );
                    Outcome::Exit(exit.code())
                }
            }
        }
    }
}

/// A service failure as a driver diagnostic: the text form carries the
/// driver's `Error:` prefix like every other driver failure; the JSON form is
/// the E1503 diagnostic object (docs/process/diagnostics.md).
fn render_daemon_error(format: ErrorFormat, message: String) -> String {
    match format {
        ErrorFormat::Text => format!("Error: {message}"),
        ErrorFormat::Json => render_driver_error(ErrorCode::DRIVER_DAEMON, message, format),
    }
}

/// Capture this invocation as the service will see it (ADR-0085 §3): every
/// path as written together with the directory it was written in, and the
/// terminal policy of this process's stderr.
fn capture(
    options: &Options,
    path_context: &HostPathContext,
    kind: BuildKind,
    output_path: String,
) -> BuildRequest {
    let mut preview_features: Vec<String> = options
        .preview_features
        .iter()
        .map(ToString::to_string)
        .collect();
    preview_features.sort();
    BuildRequest {
        measure_performance: options.daemon_performance_json.is_some(),
        artifact: kind,
        working_directory: path_context.working_directory().display().to_string(),
        root_source: options.source_path.clone(),
        output_path,
        source_manifest_path: options.source_manifest_path.clone(),
        test_candidates_path: options.test_candidates_path.clone(),
        std_root: std::env::var_os("RUE_STD_PATH")
            .map(|value| value.to_string_lossy().into_owned()),
        workers: compile_pool_jobs(&options.mode, options.jobs),
        target: options.target.to_string(),
        opt_level: options.opt_level.name().to_owned(),
        preview_features,
        link_archives: options
            .link_archives
            .iter()
            .map(|path| path_context.anchor(path).display().to_string())
            .collect(),
        error_format: match options.error_format {
            ErrorFormat::Text => DiagnosticFormat::Text,
            ErrorFormat::Json => DiagnosticFormat::Json,
        },
        color: std::io::stderr().is_terminal(),
    }
}

fn watch_input(record: InputRecord) -> WatchInput {
    WatchInput::from_parts(WatchInputParts {
        requested_path: PathBuf::from(record.requested_path),
        canonical_path: PathBuf::from(record.canonical_path),
        fingerprint: record.fingerprint,
        symlink_boundary: record.symlink_boundary.map(PathBuf::from),
        symlink_route: record.symlink_route,
    })
}

/// Render a panic that ended the service during this request the way the
/// direct compiler's own panic hook would have presented it.
fn render_service_panic(format: ErrorFormat, message: &str, location: Option<String>) -> String {
    match format {
        ErrorFormat::Json => format!("[{}]", ice_diagnostic(message, location).to_json()),
        ErrorFormat::Text => {
            let mut text = String::new();
            text.push_str(
                "error: internal compiler error: the compiler panicked; this is a bug in rue\n",
            );
            text.push_str(&format!("note: the compiler service panicked: {message}\n"));
            if let Some(location) = location {
                text.push_str(&format!("note: panic at {location}\n"));
            }
            text.push_str(&format!("note: rue version {VERSION}\n"));
            text.push_str(
                "note: please report this at https://github.com/rue-language/rue/issues\n",
            );
            text.push_str("note: re-run with --daemon=off and RUST_BACKTRACE=1 for a backtrace");
            text
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn daemon_modes_parse() {
        assert_eq!("off".parse::<DaemonMode>().unwrap(), DaemonMode::Off);
        assert_eq!("auto".parse::<DaemonMode>().unwrap(), DaemonMode::Auto);
        assert_eq!(
            "required".parse::<DaemonMode>().unwrap(),
            DaemonMode::Required
        );
        let error = "always".parse::<DaemonMode>().unwrap_err();
        assert!(error.contains("off, auto, required"), "{error}");
    }

    #[test]
    fn a_service_panic_renders_as_an_ice_in_both_formats() {
        let text = render_service_panic(ErrorFormat::Text, "boom", Some("x.rs:1:2".into()));
        assert!(text.starts_with("error: internal compiler error"));
        assert!(text.contains("panic at x.rs:1:2"));
        let json = render_service_panic(ErrorFormat::Json, "boom", None);
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed[0]["code"], "E9000");
    }
}
