use std::fs;
use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::{Duration, Instant};

use super::*;

/// Runs the service on a thread of this process, so lifecycle logic is tested
/// without spawning the compiler executable; `launches` counts how many
/// services were brought up.
struct ThreadLauncher {
    launches: Arc<AtomicUsize>,
}

impl ThreadLauncher {
    fn new() -> Self {
        Self {
            launches: Arc::new(AtomicUsize::new(0)),
        }
    }
}

struct LaunchedThread(Option<thread::JoinHandle<Result<ServeExit, DaemonError>>>);

impl LaunchedService for LaunchedThread {
    fn ended(&mut self) -> Option<String> {
        let finished = self.0.as_ref().is_some_and(thread::JoinHandle::is_finished);
        if !finished {
            return None;
        }
        let handle = self.0.take()?;
        Some(match handle.join() {
            Ok(Ok(exit)) => format!("{exit:?}"),
            Ok(Err(error)) => error.to_string(),
            Err(_) => "the service thread panicked".into(),
        })
    }
}

impl ServiceLauncher for ThreadLauncher {
    fn launch(&self, plan: &LaunchPlan) -> Result<Box<dyn LaunchedService>, DaemonError> {
        self.launches.fetch_add(1, Ordering::AcqRel);
        let scope = plan.scope.clone();
        let root = plan.root.clone();
        let idle_timeout = plan.idle_timeout;
        Ok(Box::new(LaunchedThread(Some(thread::spawn(move || {
            serve(scope, &root, idle_timeout)
        })))))
    }
}

struct Fixture {
    _dir: tempfile::TempDir,
    scope_dir: PathBuf,
    root: EndpointRoot,
}

impl Fixture {
    fn new() -> Self {
        let dir = short_private_temp_dir();
        let scope_dir = dir.path().join("scope");
        fs::create_dir(&scope_dir).unwrap();
        let root = EndpointRoot::explicit(dir.path().join("r"));
        Self {
            _dir: dir,
            scope_dir,
            root,
        }
    }

    fn scope(&self) -> DaemonScope {
        DaemonScope::new(&self.scope_dir, None).unwrap()
    }

    fn options(&self) -> StartOptions {
        StartOptions {
            idle_timeout: Duration::from_secs(60),
            startup_timeout: Duration::from_secs(20),
        }
    }
}

/// A private temporary directory short enough to hold an endpoint: Unix
/// socket paths are bounded at about a hundred bytes, and the system
/// temporary directory under Buck2 is a deep scratch path, so `/tmp` is tried
/// first and the system directory is the fallback.
fn short_private_temp_dir() -> tempfile::TempDir {
    let short = Path::new("/tmp");
    if short.is_dir() {
        if let Ok(dir) = tempfile::Builder::new().prefix("rd").tempdir_in(short) {
            return dir;
        }
    }
    tempfile::Builder::new().prefix("rd").tempdir().unwrap()
}

fn wait_until(deadline: Duration, mut condition: impl FnMut() -> bool) -> bool {
    let started = Instant::now();
    while started.elapsed() < deadline {
        if condition() {
            return true;
        }
        thread::sleep(Duration::from_millis(10));
    }
    condition()
}

#[test]
fn service_identity_depends_on_scope_isolation_and_image() {
    let fixture = Fixture::new();
    let other_dir = fixture._dir.path().join("other");
    fs::create_dir(&other_dir).unwrap();
    let base = ServiceIdentity::current(fixture.scope()).unwrap();
    let again = ServiceIdentity::current(fixture.scope()).unwrap();
    assert_eq!(
        base.record(),
        again.record(),
        "one scope and image: one identity"
    );
    assert_eq!(base.endpoint_name().len(), 16);
    assert!(
        base.endpoint_name()
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    );

    let isolated =
        ServiceIdentity::current(DaemonScope::new(&fixture.scope_dir, Some("a")).unwrap()).unwrap();
    assert_ne!(base.endpoint_name(), isolated.endpoint_name());
    let elsewhere = ServiceIdentity::current(DaemonScope::new(&other_dir, None).unwrap()).unwrap();
    assert_ne!(base.endpoint_name(), elsewhere.endpoint_name());
    // The image participates: the same user and scope under another image
    // names another endpoint.
    let other_image = ServiceIdentity::with_image(
        fixture.scope(),
        crate::running_image::RunningImageIdentity::for_test(vec![1, 2, 3]),
    );
    assert_ne!(base.endpoint_name(), other_image.endpoint_name());
    assert_ne!(base.record().image_hex, other_image.record().image_hex);

    // A scope is canonical: another spelling of the directory is the same one.
    let dotted = fixture.scope_dir.join(".");
    let via_dot = ServiceIdentity::current(DaemonScope::new(&dotted, None).unwrap()).unwrap();
    assert_eq!(base.endpoint_name(), via_dot.endpoint_name());
}

#[test]
fn scope_and_isolation_are_validated() {
    let fixture = Fixture::new();
    assert!(DaemonScope::new(&fixture._dir.path().join("missing"), None).is_err());
    for bad in ["", "has space", "a/b", &"x".repeat(65)] {
        assert!(
            DaemonScope::new(&fixture.scope_dir, Some(bad)).is_err(),
            "{bad:?} must be refused"
        );
    }
    assert!(DaemonScope::new(&fixture.scope_dir, Some("build-1.x_y")).is_ok());
}

#[test]
fn endpoint_root_must_be_private_to_the_user() {
    let fixture = Fixture::new();
    let shared = fixture._dir.path().join("shared");
    fs::create_dir(&shared).unwrap();
    fs::set_permissions(&shared, fs::Permissions::from_mode(0o755)).unwrap();
    let error = EndpointRoot::explicit(shared).ensure().unwrap_err();
    assert!(
        matches!(error, DaemonError::InsecureEndpoint(..)),
        "{error}"
    );

    fixture.root.ensure().unwrap();
    let mode = fs::metadata(fixture.root.path())
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o700, "a created root is private");
    fixture.root.ensure().unwrap();
}

#[test]
fn status_never_creates_an_endpoint() {
    let fixture = Fixture::new();
    assert!(status(fixture.scope(), &fixture.root).unwrap().is_none());
    assert!(
        !fixture.root.path().exists(),
        "status must not create the root"
    );
    assert_eq!(
        stop(fixture.scope(), &fixture.root, Duration::from_secs(1)).unwrap(),
        StopOutcome::NotRunning
    );
    assert!(
        !fixture.root.path().exists(),
        "stop must not create the root"
    );
}

#[test]
fn start_status_and_stop_round_trip() {
    let fixture = Fixture::new();
    let launcher = ThreadLauncher::new();
    let started = start(
        fixture.scope(),
        &fixture.root,
        &fixture.options(),
        &launcher,
    )
    .unwrap();
    assert!(started.launched);
    assert_eq!(started.service.pid, std::process::id());
    assert_eq!(started.service.protocol_version, DAEMON_PROTOCOL_VERSION);
    assert_eq!(
        started.service.scope_directory,
        fixture.scope().directory().display().to_string()
    );

    let report = status(fixture.scope(), &fixture.root)
        .unwrap()
        .expect("a started service reports");
    assert_eq!(report.service, started.service);
    assert!(report.connections >= 1, "the status connection counts");
    assert_eq!(report.active_request, None);
    assert_eq!(report.retained_hosts, 0);
    assert_eq!(report.idle_timeout_ms, 60_000);

    let again = start(
        fixture.scope(),
        &fixture.root,
        &fixture.options(),
        &launcher,
    )
    .unwrap();
    assert!(!again.launched, "a running service is used, not relaunched");
    assert_eq!(again.service, started.service);
    assert_eq!(launcher.launches.load(Ordering::Acquire), 1);

    let endpoint = Endpoint::locate(
        &fixture.root,
        &ServiceIdentity::current(fixture.scope()).unwrap(),
    )
    .unwrap();
    assert!(endpoint.socket().exists());
    let record: ServiceInfo =
        serde_json::from_slice(&fs::read(endpoint.directory().join("service.json")).unwrap())
            .unwrap();
    assert_eq!(record, started.service);

    assert_eq!(
        stop(fixture.scope(), &fixture.root, Duration::from_secs(10)).unwrap(),
        StopOutcome::Stopped {
            pid: std::process::id()
        }
    );
    assert!(status(fixture.scope(), &fixture.root).unwrap().is_none());
    assert!(
        !endpoint.socket().exists(),
        "a stopped service removes its socket"
    );
    assert!(!endpoint.directory().join("service.json").exists());
    assert_eq!(
        stop(fixture.scope(), &fixture.root, Duration::from_secs(1)).unwrap(),
        StopOutcome::NotRunning
    );
}

#[test]
fn a_stale_endpoint_is_recovered_before_launching() {
    let fixture = Fixture::new();
    let identity = ServiceIdentity::current(fixture.scope()).unwrap();
    let endpoint = Endpoint::prepare(&fixture.root, &identity).unwrap();
    // A dead service's leavings: a socket nothing listens on and a record.
    let _stale = std::os::unix::net::UnixListener::bind(endpoint.socket()).unwrap();
    drop(_stale);
    fs::write(endpoint.directory().join("service.json"), b"{}").unwrap();
    assert!(status(fixture.scope(), &fixture.root).unwrap().is_none());

    let launcher = ThreadLauncher::new();
    let started = start(
        fixture.scope(),
        &fixture.root,
        &fixture.options(),
        &launcher,
    )
    .unwrap();
    assert!(started.launched);
    let report = status(fixture.scope(), &fixture.root).unwrap().unwrap();
    assert_eq!(report.service.pid, std::process::id());
    stop(fixture.scope(), &fixture.root, Duration::from_secs(10)).unwrap();
}

#[test]
fn concurrent_starts_share_one_service() {
    let fixture = Fixture::new();
    let launcher = Arc::new(ThreadLauncher::new());
    let barrier = Arc::new(Barrier::new(4));
    let mut starters = Vec::new();
    for _ in 0..4 {
        let scope = fixture.scope();
        let root = fixture.root.clone();
        let options = fixture.options();
        let launcher = Arc::clone(&launcher);
        let barrier = Arc::clone(&barrier);
        starters.push(thread::spawn(move || {
            barrier.wait();
            start(scope, &root, &options, &*launcher).unwrap()
        }));
    }
    let outcomes: Vec<StartOutcome> = starters
        .into_iter()
        .map(|handle| handle.join().unwrap())
        .collect();
    assert_eq!(launcher.launches.load(Ordering::Acquire), 1);
    assert_eq!(
        outcomes.iter().filter(|outcome| outcome.launched).count(),
        1,
        "exactly one start launches; the rest find the service"
    );
    for outcome in &outcomes {
        assert_eq!(outcome.service, outcomes[0].service);
    }
    stop(fixture.scope(), &fixture.root, Duration::from_secs(10)).unwrap();
}

#[test]
fn a_client_with_another_identity_or_protocol_is_refused() {
    let fixture = Fixture::new();
    let launcher = ThreadLauncher::new();
    start(
        fixture.scope(),
        &fixture.root,
        &fixture.options(),
        &launcher,
    )
    .unwrap();
    let identity = ServiceIdentity::current(fixture.scope()).unwrap();
    let endpoint = Endpoint::locate(&fixture.root, &identity).unwrap();

    let mut other_image = identity.hello();
    other_image.identity.image_hex.push_str("00");
    let refused = client::connect(endpoint.socket(), &other_image, Duration::from_secs(5));
    assert!(
        matches!(refused, Err(client::ConnectError::Rejected(ref reason)) if reason.contains("identity")),
        "{refused:?}"
    );

    let mut other_protocol = identity.hello();
    other_protocol.protocol_version += 1;
    let refused = client::connect(endpoint.socket(), &other_protocol, Duration::from_secs(5));
    assert!(
        matches!(refused, Err(client::ConnectError::Rejected(ref reason)) if reason.contains("protocol")),
        "{refused:?}"
    );

    // The service is unharmed by refused peers.
    let mut accepted =
        client::connect(endpoint.socket(), &identity.hello(), Duration::from_secs(5)).unwrap();
    accepted.ping().unwrap();
    drop(accepted);
    stop(fixture.scope(), &fixture.root, Duration::from_secs(10)).unwrap();
}

#[test]
fn malformed_peers_are_dropped_without_disturbing_the_service() {
    let fixture = Fixture::new();
    let launcher = ThreadLauncher::new();
    start(
        fixture.scope(),
        &fixture.root,
        &fixture.options(),
        &launcher,
    )
    .unwrap();
    let identity = ServiceIdentity::current(fixture.scope()).unwrap();
    let endpoint = Endpoint::locate(&fixture.root, &identity).unwrap();

    // An oversized frame announcement.
    let mut raw = UnixStream::connect(endpoint.socket()).unwrap();
    raw.write_all(&u32::MAX.to_be_bytes()).unwrap();
    raw.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let mut sink = Vec::new();
    let _ = raw.read_to_end(&mut sink);
    assert!(sink.is_empty(), "nothing is answered to an oversized frame");
    drop(raw);

    // A truncated hello.
    let mut raw = UnixStream::connect(endpoint.socket()).unwrap();
    raw.write_all(&40_u32.to_be_bytes()).unwrap();
    raw.write_all(b"{\"protocol_version\":1").unwrap();
    drop(raw);

    let report = status(fixture.scope(), &fixture.root).unwrap().unwrap();
    assert_eq!(report.service.pid, std::process::id());
    stop(fixture.scope(), &fixture.root, Duration::from_secs(10)).unwrap();
}

#[test]
fn an_idle_service_retires_itself_and_clears_its_endpoint() {
    let fixture = Fixture::new();
    let scope = fixture.scope();
    let root = fixture.root.clone();
    let server = thread::spawn(move || serve(scope, &root, Duration::from_millis(1500)));
    let identity = ServiceIdentity::current(fixture.scope()).unwrap();
    let endpoint = Endpoint::locate(&fixture.root, &identity).unwrap();
    assert!(wait_until(Duration::from_secs(10), || status(
        fixture.scope(),
        &fixture.root
    )
    .unwrap()
    .is_some()));
    assert_eq!(server.join().unwrap().unwrap(), ServeExit::Idle);
    assert!(!endpoint.socket().exists());
    assert!(status(fixture.scope(), &fixture.root).unwrap().is_none());
}

#[test]
fn a_second_server_on_a_held_endpoint_yields() {
    let fixture = Fixture::new();
    let launcher = ThreadLauncher::new();
    start(
        fixture.scope(),
        &fixture.root,
        &fixture.options(),
        &launcher,
    )
    .unwrap();
    assert_eq!(
        serve(fixture.scope(), &fixture.root, Duration::from_secs(60)).unwrap(),
        ServeExit::AlreadyRunning
    );
    let report = status(fixture.scope(), &fixture.root).unwrap().unwrap();
    assert_eq!(report.service.pid, std::process::id());
    stop(fixture.scope(), &fixture.root, Duration::from_secs(10)).unwrap();
}
