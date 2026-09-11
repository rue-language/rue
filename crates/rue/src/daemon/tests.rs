use std::fs;
use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::{Duration, Instant};

use rue_compiler::unstable::CompilationCancellation;

use super::*;

/// Runs the service on a thread of this process, so lifecycle logic is tested
/// without spawning the compiler executable; `launches` counts how many
/// services were brought up.
struct ThreadLauncher {
    launches: Arc<AtomicUsize>,
    stub: Arc<Stub>,
}

impl ThreadLauncher {
    fn new() -> Self {
        Self::with_stub(Stub::default())
    }

    fn with_stub(stub: Stub) -> Self {
        Self {
            launches: Arc::new(AtomicUsize::new(0)),
            stub: Arc::new(stub),
        }
    }
}

/// What the in-process service's executor does with a build: answer with
/// bytes right away, or hold the request until it is canceled or released.
#[derive(Default)]
struct Stub {
    /// Builds the executor has started.
    started: AtomicUsize,
    /// Builds that ended because their cancellation fired.
    canceled: AtomicUsize,
    /// Hold every build until `release` is set or the request is canceled.
    hold: AtomicBool,
    release: AtomicBool,
}

struct StubExecutor(Arc<Stub>);

impl BuildExecutor for StubExecutor {
    fn build(
        &mut self,
        request: &BuildRequest,
        cancellation: &CompilationCancellation,
    ) -> BuildOutput {
        self.0.started.fetch_add(1, Ordering::AcqRel);
        while self.0.hold.load(Ordering::Acquire) && !self.0.release.load(Ordering::Acquire) {
            if cancellation.is_canceled() {
                self.0.canceled.fetch_add(1, Ordering::AcqRel);
                return BuildOutput {
                    result: BuildResult::Canceled,
                    bytes: Vec::new(),
                };
            }
            thread::sleep(Duration::from_millis(5));
        }
        // A deterministic payload longer than one chunk, so the transfer is
        // exercised, derived from the request so a client can check it got
        // its own answer.
        let seed = request.root_source.len() as u8;
        let bytes: Vec<u8> = (0..(protocol::MAX_CHUNK_BYTES + 3))
            .map(|i| (i as u8).wrapping_add(seed))
            .collect();
        BuildOutput {
            result: BuildResult::Ready {
                stderr: format!("warning: stub built {}\n", request.root_source),
                target: request.target.clone(),
                destination: DestinationRecord {
                    path: request.output_path.clone(),
                    display_path: request.output_path.clone(),
                    source_paths: vec![request.root_source.clone()],
                },
                inputs: Vec::new(),
                bytes: bytes.len() as u64,
            },
            bytes,
        }
    }

    fn retained_hosts(&self) -> u32 {
        1
    }
}

fn build_request(root_source: &str) -> BuildRequest {
    BuildRequest {
        working_directory: "/w".into(),
        root_source: root_source.into(),
        output_path: "out".into(),
        source_manifest_path: None,
        std_root: None,
        workers: 1,
        target: "x86_64-linux".into(),
        opt_level: "0".into(),
        preview_features: Vec::new(),
        link_archives: Vec::new(),
        error_format: DiagnosticFormat::Text,
        color: false,
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
        let stub = Arc::clone(&self.stub);
        Ok(Box::new(LaunchedThread(Some(thread::spawn(move || {
            serve(scope, &root, idle_timeout, Box::new(StubExecutor(stub)))
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
    assert_eq!(report.retained_hosts, 1);
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
    let server = thread::spawn(move || {
        serve(
            scope,
            &root,
            Duration::from_millis(1500),
            Box::new(StubExecutor(Arc::new(Stub::default()))),
        )
    });
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
        serve(
            fixture.scope(),
            &fixture.root,
            Duration::from_secs(60),
            Box::new(StubExecutor(Arc::new(Stub::default()))),
        )
        .unwrap(),
        ServeExit::AlreadyRunning
    );
    let report = status(fixture.scope(), &fixture.root).unwrap().unwrap();
    assert_eq!(report.service.pid, std::process::id());
    stop(fixture.scope(), &fixture.root, Duration::from_secs(10)).unwrap();
}

#[test]
fn a_build_is_admitted_answered_and_its_bytes_streamed() {
    let fixture = Fixture::new();
    let launcher = ThreadLauncher::new();
    let mut started = start_connection(
        fixture.scope(),
        &fixture.root,
        &fixture.options(),
        &launcher,
    )
    .unwrap();
    let submission = started
        .connection
        .submit_build(build_request("main.rue"))
        .unwrap();
    let Submission::Accepted {
        ticket,
        queued_ahead,
    } = submission
    else {
        panic!("a quiet service admits a build: {submission:?}");
    };
    assert_eq!(queued_ahead, 0);
    let (result, bytes) = started.connection.await_build(ticket).unwrap();
    let BuildResult::Ready {
        stderr,
        bytes: announced,
        destination,
        ..
    } = result
    else {
        panic!("the stub answers ready: {result:?}");
    };
    assert_eq!(stderr, "warning: stub built main.rue\n");
    assert_eq!(announced, bytes.len() as u64);
    assert_eq!(bytes.len(), protocol::MAX_CHUNK_BYTES + 3);
    assert_eq!(bytes[0], "main.rue".len() as u8);
    assert_eq!(destination.path, "out");
    assert_eq!(launcher.stub.started.load(Ordering::Acquire), 1);
    drop(started);

    let report = status(fixture.scope(), &fixture.root)
        .unwrap()
        .expect("the service is still up");
    assert_eq!(report.active_request, None);
    assert_eq!(report.queued_requests, 0);
    stop(fixture.scope(), &fixture.root, Duration::from_secs(10)).unwrap();
}

#[test]
fn admission_is_bounded_and_a_departed_client_cancels_its_request() {
    let fixture = Fixture::new();
    let stub = Stub::default();
    stub.hold.store(true, Ordering::Release);
    let launcher = ThreadLauncher::with_stub(stub);
    // One active build, held by the stub.
    let mut active = start_connection(
        fixture.scope(),
        &fixture.root,
        &fixture.options(),
        &launcher,
    )
    .unwrap();
    let Submission::Accepted {
        ticket: active_ticket,
        ..
    } = active
        .connection
        .submit_build(build_request("active.rue"))
        .unwrap()
    else {
        panic!("the first build is admitted");
    };
    assert!(wait_until(Duration::from_secs(10), || {
        launcher.stub.started.load(Ordering::Acquire) == 1
    }));
    let report = status(fixture.scope(), &fixture.root).unwrap().unwrap();
    let summary = report.active_request.expect("the held build is active");
    assert_eq!(summary.ticket, active_ticket);
    assert_eq!(summary.root_source, "active.rue");
    assert_eq!(report.queued_requests, 0);

    // Fill the queue behind it.
    let mut queued = Vec::new();
    for index in 0..MAX_QUEUED_REQUESTS {
        let mut connection = start_connection(
            fixture.scope(),
            &fixture.root,
            &fixture.options(),
            &launcher,
        )
        .unwrap();
        let ticket = match connection
            .connection
            .submit_build(build_request(&format!("queued{index}.rue")))
            .unwrap()
        {
            Submission::Accepted {
                ticket,
                queued_ahead,
            } => {
                assert_eq!(queued_ahead, index + 1, "the active build counts as ahead");
                ticket
            }
            Submission::Rejected { reason } => panic!("queue place {index} refused: {reason}"),
        };
        queued.push((connection, ticket));
    }
    let report = status(fixture.scope(), &fixture.root).unwrap().unwrap();
    assert_eq!(report.queued_requests, MAX_QUEUED_REQUESTS);

    // One more is refused before any work, so its client may go direct.
    let mut overflow = start_connection(
        fixture.scope(),
        &fixture.root,
        &fixture.options(),
        &launcher,
    )
    .unwrap();
    match overflow
        .connection
        .submit_build(build_request("overflow.rue"))
        .unwrap()
    {
        Submission::Rejected { reason } => assert!(reason.contains("waiting"), "{reason}"),
        Submission::Accepted { .. } => panic!("the queue bound admitted one too many"),
    }
    drop(overflow);

    // A queued client that leaves is canceled without being started; the
    // active client that leaves has its compile canceled.
    drop(queued.pop());
    drop(active);
    assert!(wait_until(Duration::from_secs(10), || {
        launcher.stub.canceled.load(Ordering::Acquire) == 1
    }));
    // The remaining queued builds now run; the stub still holds them, so
    // release it and let them finish.
    launcher.stub.release.store(true, Ordering::Release);
    for (mut connection, ticket) in queued {
        let (result, _) = connection.connection.await_build(ticket).unwrap();
        assert!(matches!(result, BuildResult::Ready { .. }), "{result:?}");
    }
    // Exactly one queued build was never started: the one whose client left.
    assert!(wait_until(Duration::from_secs(10), || {
        launcher.stub.started.load(Ordering::Acquire) as u32 == MAX_QUEUED_REQUESTS
    }));
    stop(fixture.scope(), &fixture.root, Duration::from_secs(10)).unwrap();
}

#[test]
fn a_crash_record_names_the_request_it_ended() {
    let fixture = Fixture::new();
    let identity = ServiceIdentity::current(fixture.scope()).unwrap();
    let endpoint = Endpoint::prepare(&fixture.root, &identity).unwrap();
    assert!(endpoint.crash_record().is_none());
    let record = CrashRecord {
        pid: 42,
        ticket: Some(7),
        message: "index out of bounds".into(),
        location: Some("crates/x.rs:1:1".into()),
    };
    fs::write(
        endpoint.directory().join(service::CRASH_RECORD_FILE),
        serde_json::to_vec(&record).unwrap(),
    )
    .unwrap();
    assert_eq!(endpoint.crash_record(), Some(record));
}
