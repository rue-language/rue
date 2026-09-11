//! `rue daemon <start|status|stop|serve>`: the command-line controls of the
//! local compiler service (ADR-0085 §2).
//!
//! Every control resolves the same service ordinary compilation will: the
//! scope directory (`--scope`, default the invocation directory) and an
//! optional isolation name. `status` and `stop` never start a service. `serve`
//! is the service itself and is launched detached by `start`; it is not meant
//! to be run by hand, but doing so runs the service in the foreground.

use std::path::{Path, PathBuf};
use std::time::Duration;

use rue_driver::daemon::{
    self, DaemonError, DaemonScope, EndpointRoot, ProcessLauncher, ServeExit, StartOptions,
    StatusReport, StopOutcome,
};

use crate::HostPathContext;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DaemonCommand {
    Start,
    Status,
    Stop,
    Serve,
}

/// One parsed `rue daemon` command line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DaemonInvocation {
    pub(crate) command: DaemonCommand,
    /// The scope directory as written; resolved against the invocation
    /// directory when the command runs.
    pub(crate) scope: Option<String>,
    pub(crate) isolation: Option<String>,
    /// An explicit endpoint root. `start` passes the root it resolved to the
    /// service it launches, so the two can never disagree about it.
    pub(crate) endpoint_root: Option<String>,
    pub(crate) idle_timeout: Option<Duration>,
    /// Report status as one JSON object instead of text.
    pub(crate) json: bool,
}

const USAGE: &str = "Usage: rue daemon <start|status|stop> [--scope <dir>] [--isolation <name>] \
                     [--idle-timeout-ms <N>] [--json]";

/// Parse the arguments after the `daemon` token.
pub(crate) fn parse_daemon_args(args: &[&str]) -> Result<DaemonInvocation, String> {
    let Some((command, rest)) = args.split_first() else {
        return Err(format!("`rue daemon` needs a command\n{USAGE}"));
    };
    let command = match *command {
        "start" => DaemonCommand::Start,
        "status" => DaemonCommand::Status,
        "stop" => DaemonCommand::Stop,
        "serve" => DaemonCommand::Serve,
        other => return Err(format!("unknown daemon command `{other}`\n{USAGE}")),
    };
    let mut invocation = DaemonInvocation {
        command,
        scope: None,
        isolation: None,
        endpoint_root: None,
        idle_timeout: None,
        json: false,
    };
    let mut iter = rest.iter();
    while let Some(arg) = iter.next() {
        let mut value = |flag: &str| -> Result<String, String> {
            iter.next()
                .map(|value| (*value).to_owned())
                .ok_or_else(|| format!("{flag} requires a value\n{USAGE}"))
        };
        match *arg {
            "--scope" => invocation.scope = Some(value("--scope")?),
            "--isolation" => invocation.isolation = Some(value("--isolation")?),
            "--endpoint-root" => invocation.endpoint_root = Some(value("--endpoint-root")?),
            "--idle-timeout-ms" => {
                let text = value("--idle-timeout-ms")?;
                let millis: u64 = text.parse().map_err(|_| {
                    format!(
                        "--idle-timeout-ms requires a whole number of milliseconds, got `{text}`"
                    )
                })?;
                if millis == 0 {
                    return Err("--idle-timeout-ms must be at least 1".into());
                }
                invocation.idle_timeout = Some(Duration::from_millis(millis));
            }
            "--json" => invocation.json = true,
            other => return Err(format!("unknown daemon option `{other}`\n{USAGE}")),
        }
    }
    if invocation.json && command != DaemonCommand::Status {
        return Err(format!(
            "--json applies to `rue daemon status` only\n{USAGE}"
        ));
    }
    if invocation.idle_timeout.is_some()
        && !matches!(command, DaemonCommand::Start | DaemonCommand::Serve)
    {
        return Err(format!(
            "--idle-timeout-ms applies to `rue daemon start` only\n{USAGE}"
        ));
    }
    Ok(invocation)
}

/// Run one daemon control and return the process exit status. Status reports
/// go to stdout; every failure and every "no service" answer is a stderr line.
pub(crate) fn run(invocation: &DaemonInvocation) -> i32 {
    match execute(invocation) {
        Ok(status) => status,
        Err(error) => {
            eprintln!("Error: {error}");
            1
        }
    }
}

fn execute(invocation: &DaemonInvocation) -> Result<i32, DaemonError> {
    let path_context = HostPathContext::capture().map_err(DaemonError::InvalidConfiguration)?;
    let scope_dir: PathBuf = match &invocation.scope {
        Some(scope) => path_context.anchor(Path::new(scope)),
        None => path_context.working_directory().to_path_buf(),
    };
    let scope = DaemonScope::new(&scope_dir, invocation.isolation.as_deref())?;
    let root = match &invocation.endpoint_root {
        Some(root) => EndpointRoot::explicit(path_context.anchor(Path::new(root))),
        None => EndpointRoot::resolve()?,
    };
    let idle_timeout = invocation
        .idle_timeout
        .unwrap_or(daemon::DEFAULT_IDLE_TIMEOUT);
    match invocation.command {
        DaemonCommand::Start => {
            let launcher = ProcessLauncher::current_executable()?;
            let options = StartOptions {
                idle_timeout,
                ..StartOptions::default()
            };
            let outcome = daemon::start(scope.clone(), &root, &options, &launcher)?;
            if outcome.launched {
                println!(
                    "rue daemon: started service pid {} for {scope}",
                    outcome.service.pid
                );
            } else {
                println!(
                    "rue daemon: service pid {} is already running for {scope}",
                    outcome.service.pid
                );
            }
            Ok(0)
        }
        DaemonCommand::Status => match daemon::status(scope.clone(), &root)? {
            Some(report) => {
                if invocation.json {
                    println!(
                        "{}",
                        serde_json::to_string(&report).expect("a status report serializes")
                    );
                } else {
                    print!("{}", render_status(&report));
                }
                Ok(0)
            }
            None => {
                eprintln!("rue daemon: no service is running for {scope}");
                Ok(1)
            }
        },
        DaemonCommand::Stop => match daemon::stop(scope.clone(), &root, STOP_TIMEOUT)? {
            StopOutcome::Stopped { pid } => {
                println!("rue daemon: stopped service pid {pid} for {scope}");
                Ok(0)
            }
            StopOutcome::NotRunning => {
                println!("rue daemon: no service was running for {scope}");
                Ok(0)
            }
        },
        DaemonCommand::Serve => {
            let executor = Box::new(crate::daemon_service::Executor::new());
            match daemon::serve(scope, &root, idle_timeout, executor)? {
                ServeExit::Stopped | ServeExit::Idle | ServeExit::AlreadyRunning => Ok(0),
            }
        }
    }
}

/// How long `stop` waits for the service to release its endpoint.
const STOP_TIMEOUT: Duration = Duration::from_secs(30);

fn render_status(report: &StatusReport) -> String {
    let service = &report.service;
    let mut text = String::new();
    text.push_str("rue daemon: running\n");
    text.push_str(&format!("  pid: {}\n", service.pid));
    text.push_str(&format!("  scope: {}\n", service.scope_directory));
    text.push_str(&format!(
        "  isolation: {}\n",
        service.isolation.as_deref().unwrap_or("none")
    ));
    text.push_str(&format!("  endpoint: {}\n", service.endpoint_directory));
    text.push_str(&format!("  protocol: {}\n", service.protocol_version));
    text.push_str(&format!(
        "  image: {} {} {}\n",
        service.identity.architecture, service.identity.scheme, service.identity.image_hex
    ));
    text.push_str(&format!(
        "  service identity: {}\n",
        service.identity.service_hex
    ));
    text.push_str(&format!("  uptime: {} ms\n", report.uptime_ms));
    text.push_str(&format!("  idle timeout: {} ms\n", report.idle_timeout_ms));
    text.push_str(&format!("  connections: {}\n", report.connections));
    match &report.active_request {
        Some(active) => text.push_str(&format!(
            "  active request: #{} {} (in {}, {} ms)\n",
            active.ticket, active.root_source, active.working_directory, active.elapsed_ms
        )),
        None => text.push_str("  active request: none\n"),
    }
    text.push_str(&format!("  queued requests: {}\n", report.queued_requests));
    text.push_str(&format!("  retained hosts: {}\n", report.retained_hosts));
    let policy = &report.resource_policy;
    let pressure = &report.resource_pressure;
    text.push_str(&format!(
        "  resource policy: connections {}/{}; queued {}/{}; hosts {}/{}; response bytes {}/{}; retained charge {}/{}; dependency pins {}/{}\n",
        pressure.connections,
        policy.max_connections,
        pressure.queued_requests,
        policy.max_queued_requests,
        pressure.retained_hosts,
        policy.max_retained_hosts,
        pressure.response_bytes,
        policy.max_response_bytes,
        pressure.retained_charge_bytes,
        policy.max_retained_charge_bytes,
        pressure.dependency_pins,
        policy.max_dependency_pins,
    ));
    text.push_str(&format!(
        "  source snapshot: {} files, {} bytes; response peak: {} bytes; peak charge: {} bytes; peak pins: {}\n",
        pressure.source_files,
        pressure.source_bytes,
        pressure.peak_response_bytes,
        pressure.peak_retained_charge_bytes,
        pressure.peak_dependency_pins
    ));
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_and_options_parse() {
        let parsed = parse_daemon_args(&[
            "start",
            "--scope",
            "proj",
            "--isolation",
            "ci",
            "--idle-timeout-ms",
            "1500",
        ])
        .unwrap();
        assert_eq!(parsed.command, DaemonCommand::Start);
        assert_eq!(parsed.scope.as_deref(), Some("proj"));
        assert_eq!(parsed.isolation.as_deref(), Some("ci"));
        assert_eq!(parsed.idle_timeout, Some(Duration::from_millis(1500)));
        assert!(!parsed.json);

        let parsed = parse_daemon_args(&["status", "--json"]).unwrap();
        assert_eq!(parsed.command, DaemonCommand::Status);
        assert!(parsed.json);
        assert!(parsed.scope.is_none());

        let parsed =
            parse_daemon_args(&["serve", "--endpoint-root", "/tmp/r", "--scope", "/p"]).unwrap();
        assert_eq!(parsed.command, DaemonCommand::Serve);
        assert_eq!(parsed.endpoint_root.as_deref(), Some("/tmp/r"));
        assert_eq!(
            parse_daemon_args(&["stop"]).unwrap().command,
            DaemonCommand::Stop
        );
    }

    #[test]
    fn malformed_command_lines_are_refused_with_usage() {
        for args in [
            &[][..],
            &["restart"],
            &["start", "--scope"],
            &["start", "--bogus"],
            &["start", "--idle-timeout-ms", "soon"],
            &["start", "--idle-timeout-ms", "0"],
            &["stop", "--json"],
            &["status", "--idle-timeout-ms", "5"],
        ] {
            let error = parse_daemon_args(args).unwrap_err();
            assert!(!error.is_empty(), "{args:?}");
        }
        assert!(parse_daemon_args(&["restart"]).unwrap_err().contains(USAGE));
    }

    #[test]
    fn status_renders_every_field_a_reader_needs() {
        let report = StatusReport {
            service: daemon::ServiceInfo {
                pid: 4242,
                protocol_version: daemon::DAEMON_PROTOCOL_VERSION,
                identity: daemon::IdentityRecord {
                    scheme_version: 1,
                    architecture: "X86_64".into(),
                    scheme: "LinuxProcSelfExeSha256".into(),
                    image_hex: "abcd".into(),
                    service_hex: "0123".into(),
                },
                scope_directory: "/work/project".into(),
                isolation: Some("ci".into()),
                endpoint_directory: "/run/rue/0123".into(),
                started_at_unix_ms: 0,
            },
            uptime_ms: 12,
            idle_timeout_ms: 1_800_000,
            connections: 1,
            active_request: None,
            queued_requests: 0,
            retained_hosts: 0,
            resource_policy: daemon::ResourcePolicy {
                max_connections: 33,
                max_queued_requests: 8,
                max_retained_hosts: 1,
                max_retained_charge_bytes: 256 * 1024 * 1024,
                max_dependency_pins: 1_000_000,
                max_response_bytes: daemon::MAX_RESPONSE_BYTES as u64,
                control_read_timeout_ms: 5_000,
                response_write_timeout_ms: 5_000,
            },
            resource_pressure: daemon::ResourcePressure::default(),
        };
        let text = render_status(&report);
        for needle in [
            "pid: 4242",
            "scope: /work/project",
            "isolation: ci",
            "endpoint: /run/rue/0123",
            "protocol: 2",
            "image: X86_64 LinuxProcSelfExeSha256 abcd",
            "idle timeout: 1800000 ms",
            "active request: none",
            "retained hosts: 0",
            "resource policy: connections 0/33; queued 0/8; hosts 0/1",
            "retained charge 0/268435456; dependency pins 0/1000000",
            "source snapshot: 0 files, 0 bytes; response peak: 0 bytes",
        ] {
            assert!(text.contains(needle), "missing {needle:?} in:\n{text}");
        }
    }
}
