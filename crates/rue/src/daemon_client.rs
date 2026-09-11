//! The client side of compiling through the local compiler service
//! (ADR-0085 §2, §3, §5, §6): decide whether this invocation may use the
//! service, capture the request at the process boundary, submit it, and
//! publish what comes back exactly as the direct path would have.

use std::io::IsTerminal;
use std::path::{Path, PathBuf};

use rue_compiler::{CompileOptions, LinkerMode};
use rue_driver::daemon::{
    self, BuildRequest, BuildResult, DaemonScope, DiagnosticFormat, EndpointRoot, InputRecord,
    ProcessLauncher, StartOptions, Submission, SubmitError,
};
use rue_driver::{HostPathContext, WatchInput, WatchInputParts};
use rue_error::ErrorCode;

use crate::compile::{Announcement, announce};
use crate::output::{PublicationDestination, PublishRequest, publish_watch_executable};
use crate::{
    DiagnosticOutput, DriverMode, ErrorFormat, Options, VERSION, compile_pool_jobs,
    driver_failure_exit_code, ice_diagnostic, render_driver_error, render_internal_error,
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
    if options.mode == DriverMode::Test {
        return Some("`rue test` runs directly until its service slice lands");
    }
    if options.watch {
        return Some("--watch runs directly");
    }
    if !options.emit_stages.is_empty() {
        return Some("--emit runs directly");
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

    let request = capture(options, path_context);
    let scope_dir = match &options.daemon_scope {
        Some(scope) => path_context.anchor(Path::new(scope)),
        None => {
            let root = path_context.anchor(Path::new(&options.source_path));
            root.parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| path_context.working_directory().to_path_buf())
        }
    };
    let scope = match DaemonScope::new(&scope_dir, options.daemon_isolation.as_deref()) {
        Ok(scope) => scope,
        Err(error) => return fallback(format!("no service scope: {error}")),
    };
    let root = match EndpointRoot::resolve() {
        Ok(root) => root,
        Err(error) => return fallback(format!("no service endpoint: {error}")),
    };
    let launcher = match ProcessLauncher::current_executable() {
        Ok(launcher) => launcher,
        Err(error) => return fallback(format!("no service launcher: {error}")),
    };
    let mut started =
        match daemon::start_connection(scope, &root, &StartOptions::default(), &launcher) {
            Ok(started) => started,
            Err(error) => return fallback(format!("the service is unavailable: {error}")),
        };
    let ticket = match started.connection.submit_build(request) {
        Ok(Submission::Accepted { ticket, .. }) => ticket,
        Ok(Submission::Rejected { reason }) => {
            return fallback(format!("the service refused the request: {reason}"));
        }
        Err(SubmitError::BeforeSubmission(error)) => {
            return fallback(format!("the request could not be submitted: {error}"));
        }
        // The service may have started the request: not a fallback, however
        // the invocation was configured (ADR-0085 §6).
        Err(error @ SubmitError::Ambiguous(_)) => return fail(error.to_string()),
    };
    let (result, bytes) = match started.connection.await_build(ticket) {
        Ok(answer) => answer,
        Err(error) => {
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
            return fail(format!(
                "the compiler service ended the request without an answer: {error}"
            ));
        }
    };
    drop(started);

    match result {
        BuildResult::Rejected { stderr } => {
            eprint!("{stderr}");
            Outcome::Exit(1)
        }
        BuildResult::Canceled => fail("the compiler service canceled the request".into()),
        BuildResult::Failed { message, internal } => {
            if internal {
                eprintln!("{}", render_internal_error(format, message));
                Outcome::Exit(failure_exit)
            } else {
                fail(message)
            }
        }
        BuildResult::Ready {
            stderr,
            target,
            destination,
            inputs,
            ..
        } => {
            eprint!("{stderr}");
            let target = match target.parse::<rue_target::Target>() {
                Ok(target) => target,
                Err(error) => return fail(format!("the service named target `{target}`: {error}")),
            };
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
            match publication {
                Ok(()) => {
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
                // `InputsChanged` included: a source that changed between
                // the compile and the rename is refused, not installed, and
                // the last successfully published file stays (ADR-0085 §5).
                Err(error) => {
                    let diagnostics = DiagnosticOutput::new(format, Vec::new());
                    eprintln!("{}", diagnostics.render_error(&error.into_compile_error()));
                    Outcome::Exit(1)
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
fn capture(options: &Options, path_context: &HostPathContext) -> BuildRequest {
    let mut preview_features: Vec<String> = options
        .preview_features
        .iter()
        .map(ToString::to_string)
        .collect();
    preview_features.sort();
    BuildRequest {
        working_directory: path_context.working_directory().display().to_string(),
        root_source: options.source_path.clone(),
        output_path: options.output_path.clone(),
        source_manifest_path: options.source_manifest_path.clone(),
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
