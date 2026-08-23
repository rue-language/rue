use libtest_lexarg::OutputFormat;

use crate::{cli, notify, Case, RunError, RunMode, TestContext};

pub trait HarnessState: sealed::_HarnessState_is_Sealed {}

pub struct Harness<State: HarnessState> {
    state: State,
}

pub struct StateInitial {
    start: std::time::Instant,
}
impl HarnessState for StateInitial {}
impl sealed::_HarnessState_is_Sealed for StateInitial {}

impl Harness<StateInitial> {
    pub fn new() -> Self {
        Self {
            state: StateInitial {
                start: std::time::Instant::now(),
            },
        }
    }

    pub fn with_env(self) -> std::io::Result<Harness<StateArgs>> {
        let raw = std::env::args_os();
        self.with_args(raw)
    }

    pub fn with_args(
        self,
        args: impl IntoIterator<Item = impl Into<std::ffi::OsString>>,
    ) -> std::io::Result<Harness<StateArgs>> {
        let raw = expand_args(args)?;
        Ok(Harness {
            state: StateArgs {
                start: self.state.start,
                raw,
            },
        })
    }
}

impl Default for Harness<StateInitial> {
    fn default() -> Self {
        Self::new()
    }
}

pub struct StateArgs {
    start: std::time::Instant,
    raw: Vec<std::ffi::OsString>,
}
impl HarnessState for StateArgs {}
impl sealed::_HarnessState_is_Sealed for StateArgs {}

impl Harness<StateArgs> {
    pub fn parse(&self) -> Result<Harness<StateParsed>, cli::LexError<'_>> {
        let mut parser = cli::Parser::new(&self.state.raw);
        let opts = parse(&mut parser)?;

        #[cfg(feature = "color")]
        match opts.color {
            libtest_lexarg::ColorConfig::AutoColor => anstream::ColorChoice::Auto,
            libtest_lexarg::ColorConfig::AlwaysColor => anstream::ColorChoice::Always,
            libtest_lexarg::ColorConfig::NeverColor => anstream::ColorChoice::Never,
        }
        .write_global();

        let notifier = notifier(&opts);

        Ok(Harness {
            state: StateParsed {
                start: self.state.start,
                opts,
                notifier,
            },
        })
    }
}

pub struct StateParsed {
    start: std::time::Instant,
    opts: libtest_lexarg::TestOpts,
    notifier: notify::ArcNotifier,
}
impl HarnessState for StateParsed {}
impl sealed::_HarnessState_is_Sealed for StateParsed {}

impl Harness<StateParsed> {
    pub fn discover(
        self,
        cases: impl IntoIterator<Item = impl Case + 'static>,
    ) -> std::io::Result<Harness<StateDiscovered>> {
        self.state.notifier.notify(
            notify::event::DiscoverStart {
                elapsed_s: Some(notify::Elapsed(self.state.start.elapsed())),
            }
            .into(),
        )?;

        let mut selected_cases = Vec::new();
        for case in cases {
            let selected = case_priority(&case, &self.state.opts).is_some();
            self.state.notifier.notify(
                notify::event::DiscoverCase {
                    name: case.name().to_owned(),
                    mode: RunMode::Test,
                    selected,
                    elapsed_s: Some(notify::Elapsed(self.state.start.elapsed())),
                }
                .into(),
            )?;
            if selected {
                selected_cases.push(Box::new(case) as Box<dyn Case>);
            }
        }

        selected_cases.sort_unstable_by_key(|case| {
            let priority = case_priority(case.as_ref(), &self.state.opts);
            let name = case.name().to_owned();
            (priority, name)
        });

        // A positive name filter is an assertion about the inventory, not
        // merely an optimization.  Treat an empty selection as a harness
        // error so a typo cannot report a passing (zero-test) run.  Leave
        // unfiltered discovery alone: platform selection, sharding, and
        // orchestration may intentionally produce an empty inventory.  The
        // check is after `--skip` filtering, so a filter whose only matches
        // are skipped is still an empty selection.
        if !self.state.opts.filters.is_empty() && selected_cases.is_empty() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!(
                    "no tests matched the given filter(s): {}",
                    self.state.opts.filters.join(", ")
                ),
            ));
        }

        self.state.notifier.notify(
            notify::event::DiscoverComplete {
                elapsed_s: Some(notify::Elapsed(self.state.start.elapsed())),
            }
            .into(),
        )?;

        Ok(Harness {
            state: StateDiscovered {
                start: self.state.start,
                opts: self.state.opts,
                notifier: self.state.notifier,
                cases: selected_cases,
            },
        })
    }
}

pub struct StateDiscovered {
    start: std::time::Instant,
    opts: libtest_lexarg::TestOpts,
    notifier: notify::ArcNotifier,
    cases: Vec<Box<dyn Case>>,
}
impl HarnessState for StateDiscovered {}
impl sealed::_HarnessState_is_Sealed for StateDiscovered {}

impl Harness<StateDiscovered> {
    pub fn run(self) -> std::io::Result<bool> {
        if self.state.opts.list {
            Ok(true)
        } else {
            run(
                &self.state.start,
                &self.state.opts,
                self.state.cases,
                self.state.notifier,
            )
        }
    }
}

mod sealed {
    #[allow(unnameable_types)]
    #[allow(non_camel_case_types)]
    pub trait _HarnessState_is_Sealed {}
}

pub const ERROR_EXIT_CODE: i32 = 101;

fn expand_args(
    args: impl IntoIterator<Item = impl Into<std::ffi::OsString>>,
) -> std::io::Result<Vec<std::ffi::OsString>> {
    let mut expanded = Vec::new();
    for arg in args {
        let arg = arg.into();
        if let Some(argfile) = arg.to_str().and_then(|s| s.strip_prefix("@")) {
            expanded.extend(parse_argfile(std::path::Path::new(argfile))?);
        } else {
            expanded.push(arg);
        }
    }
    Ok(expanded)
}

fn parse_argfile(path: &std::path::Path) -> std::io::Result<Vec<std::ffi::OsString>> {
    // Logic taken from rust-lang/rust's `compiler/rustc_driver_impl/src/args.rs`
    let content = std::fs::read_to_string(path)?;
    Ok(content.lines().map(|s| s.into()).collect())
}

fn parse<'p>(parser: &mut cli::Parser<'p>) -> Result<libtest_lexarg::TestOpts, cli::LexError<'p>> {
    let mut test_opts = libtest_lexarg::TestOptsBuilder::new();

    let bin = parser
        .next_raw()
        .expect("first arg, no pending values")
        .unwrap_or(std::ffi::OsStr::new("test"));
    let mut prev_arg = cli::Arg::Value(bin);
    while let Some(arg) = parser.next_arg() {
        match arg {
            cli::Arg::Short("h") | cli::Arg::Long("help") => {
                let mut bin = std::path::Path::new(bin);
                if let Ok(current_dir) = std::env::current_dir() {
                    // abbreviate the path because cargo always uses absolute paths
                    bin = bin.strip_prefix(&current_dir).unwrap_or(bin);
                }
                let bin = bin.to_string_lossy();
                let options_help = libtest_lexarg::OPTIONS_HELP.trim();
                let after_help = libtest_lexarg::AFTER_HELP.trim();
                println!(
                    "Usage: {bin} [OPTIONS] [FILTER]...

{options_help}

{after_help}"
                );
                std::process::exit(0);
            }
            // All values are the same, whether escaped or not, so its a no-op
            cli::Arg::Escape(_) => {
                prev_arg = arg;
                continue;
            }
            cli::Arg::Unexpected(_) => {
                return Err(cli::LexError::msg("unexpected value")
                    .unexpected(arg)
                    .within(prev_arg));
            }
            _ => {}
        }
        prev_arg = arg;

        let arg = test_opts.parse_next(parser, arg)?;

        if let Some(arg) = arg {
            return Err(cli::LexError::msg("unexpected argument").unexpected(arg));
        }
    }

    let mut opts = test_opts.finish()?;
    // If the platform is single-threaded we're just going to run
    // the test synchronously, regardless of the concurrency
    // level.
    let supports_threads = !cfg!(target_os = "emscripten") && !cfg!(target_family = "wasm");
    opts.test_threads = if cfg!(feature = "threads") && supports_threads {
        opts.test_threads
            .or_else(|| std::thread::available_parallelism().ok())
    } else {
        None
    };
    Ok(opts)
}

fn notifier(opts: &libtest_lexarg::TestOpts) -> notify::ArcNotifier {
    #[cfg(feature = "color")]
    let stdout = anstream::stdout();
    #[cfg(not(feature = "color"))]
    let stdout = std::io::stdout();
    match opts.format {
        OutputFormat::Json => notify::ArcNotifier::new(notify::JsonNotifier::new(stdout)),
        _ if opts.list => notify::ArcNotifier::new(notify::TerseListNotifier::new(stdout)),
        OutputFormat::Pretty => notify::ArcNotifier::new(notify::PrettyRunNotifier::new(stdout)),
        OutputFormat::Terse => notify::ArcNotifier::new(notify::TerseRunNotifier::new(stdout)),
    }
}

fn case_priority(case: &dyn Case, opts: &libtest_lexarg::TestOpts) -> Option<usize> {
    let filtered_out =
        !opts.skip.is_empty() && opts.skip.iter().any(|sf| matches_filter(case, sf, opts));
    if filtered_out {
        None
    } else if opts.filters.is_empty() {
        Some(0)
    } else {
        opts.filters
            .iter()
            .position(|filter| matches_filter(case, filter, opts))
    }
}

fn matches_filter(case: &dyn Case, filter: &str, opts: &libtest_lexarg::TestOpts) -> bool {
    let test_name = case.name();

    match opts.filter_exact {
        true => test_name == filter,
        false => test_name.contains(filter),
    }
}

fn run(
    start: &std::time::Instant,
    opts: &libtest_lexarg::TestOpts,
    cases: Vec<Box<dyn Case>>,
    notifier: notify::ArcNotifier,
) -> std::io::Result<bool> {
    notifier.notify(
        notify::event::RunStart {
            elapsed_s: Some(notify::Elapsed(start.elapsed())),
        }
        .into(),
    )?;

    if opts.no_capture {
        return Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "`--no-capture` is not supported at this time",
        ));
    }
    if opts.show_output {
        return Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "`--show-output` is not supported at this time",
        ));
    }

    let threads = opts.test_threads.map(|t| t.get()).unwrap_or(1);

    let run_ignored = match opts.run_ignored {
        libtest_lexarg::RunIgnored::Yes | libtest_lexarg::RunIgnored::Only => true,
        libtest_lexarg::RunIgnored::No => false,
    };
    let mode = match (opts.run_tests, opts.bench_benchmarks) {
        (true, true) => {
            return Err(std::io::Error::other(
                "`--test` and `-bench` are mutually exclusive",
            ));
        }
        (true, false) => RunMode::Test,
        (false, true) => RunMode::Bench,
        (false, false) => unreachable!("libtest-lexarg` should always ensure at least one is set"),
    };
    let context = TestContext {
        start: *start,
        mode,
        run_ignored,
        notifier,
        test_name: String::new(),
    };

    let mut success = true;

    let (exclusive_cases, concurrent_cases) = if threads == 1 || cases.len() == 1 {
        (cases, vec![])
    } else {
        cases
            .into_iter()
            .partition::<Vec<_>, _>(|c| c.exclusive(&context))
    };
    if !concurrent_cases.is_empty() {
        context.notifier().threaded(true);

        // Use a deterministic hasher
        type TestMap = std::collections::HashMap<
            String,
            std::thread::JoinHandle<std::io::Result<bool>>,
            std::hash::BuildHasherDefault<std::collections::hash_map::DefaultHasher>,
        >;

        let failure_seen = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let mut running: TestMap = Default::default();
        let (tx, rx) = std::sync::mpsc::channel::<String>();
        let mut remaining = std::collections::VecDeque::from(concurrent_cases);
        let mut stop_spawning = false;
        let mut scheduler_error = None;
        while !running.is_empty() || !remaining.is_empty() {
            while !stop_spawning && running.len() < threads && !remaining.is_empty() {
                if opts.fail_fast
                    && failure_seen.load(std::sync::atomic::Ordering::Acquire)
                {
                    stop_spawning = true;
                    remaining.clear();
                    break;
                }
                let case = remaining.pop_front().unwrap();
                let case = std::sync::Arc::new(case);
                let name = case.name().to_owned();

                let cfg = std::thread::Builder::new().name(name.clone());
                let thread_tx = tx.clone();
                let thread_case = case.clone();
                let mut thread_context = context.clone();
                thread_context.test_name = name.clone();
                let thread_failure_seen = failure_seen.clone();
                let join_handle = cfg.spawn(move || {
                    let status = run_case(thread_case.as_ref().as_ref(), &thread_context);
                    if !matches!(status, Ok(true)) {
                        thread_failure_seen.store(true, std::sync::atomic::Ordering::Release);
                    }
                    let _ = thread_tx.send(thread_case.name().to_owned());
                    status
                });
                match join_handle {
                    Ok(join_handle) => {
                        running.insert(name.clone(), join_handle);
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        // `ErrorKind::WouldBlock` means hitting the thread limit on some
                        // platforms, so run the test synchronously here instead.
                        match run_case(case.as_ref().as_ref(), &context) {
                            Ok(case_success) => {
                                success &= case_success;
                                if !case_success {
                                    failure_seen.store(true, std::sync::atomic::Ordering::Release);
                                    if opts.fail_fast {
                                        stop_spawning = true;
                                        remaining.clear();
                                    }
                                }
                            }
                            Err(error) => {
                                failure_seen.store(true, std::sync::atomic::Ordering::Release);
                                scheduler_error = Some(error);
                                stop_spawning = true;
                                remaining.clear();
                            }
                        }
                    }
                    Err(e) => {
                        scheduler_error = Some(e);
                        stop_spawning = true;
                        remaining.clear();
                    }
                }
            }

            if running.is_empty() {
                break;
            }

            let test_name = rx
                .recv()
                .map_err(|error| std::io::Error::other(error.to_string()))?;
            let running_test = running.remove(&test_name).unwrap();
            match running_test.join() {
                Ok(Ok(case_success)) => {
                    success &= case_success;
                    if !case_success && opts.fail_fast {
                        stop_spawning = true;
                        remaining.clear();
                    }
                }
                Ok(Err(error)) => {
                    if scheduler_error.is_none() {
                        scheduler_error = Some(error);
                    }
                    stop_spawning = true;
                    remaining.clear();
                }
                Err(_) => {
                    if scheduler_error.is_none() {
                        scheduler_error = Some(std::io::Error::other(format!(
                            "test worker '{test_name}' panicked outside the case runner"
                        )));
                    }
                    stop_spawning = true;
                    remaining.clear();
                }
            }
        }

        if let Some(error) = scheduler_error {
            return Err(error);
        }
    }

    if !exclusive_cases.is_empty() && !(opts.fail_fast && !success) {
        context.notifier().threaded(false);
        for case in exclusive_cases {
            success &= run_case(case.as_ref(), &context)?;
            if !success && opts.fail_fast {
                break;
            }
        }
    }

    context.notifier().notify(
        notify::event::RunComplete {
            elapsed_s: Some(notify::Elapsed(start.elapsed())),
        }
        .into(),
    )?;

    Ok(success)
}

fn run_case(case: &dyn Case, context: &TestContext) -> std::io::Result<bool> {
    context.notifier().notify(
        notify::event::CaseStart {
            name: case.name().to_owned(),
            elapsed_s: Some(context.elapased_s()),
        }
        .into(),
    )?;

    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        __rust_begin_short_backtrace(|| case.run(context))
    }))
    .unwrap_or_else(|e| {
        // The `panic` information is just an `Any` object representing the
        // value the panic was invoked with. For most panics (which use
        // `panic!` like `println!`), this is either `&str` or `String`.
        let payload = e
            .downcast_ref::<String>()
            .map(|s| s.as_str())
            .or_else(|| e.downcast_ref::<&str>().copied());

        let msg = match payload {
            Some(payload) => format!("test panicked: {payload}"),
            None => "test panicked".to_owned(),
        };
        Err(RunError::fail(msg))
    });

    let mut case_status = None;
    if let Some(err) = outcome.as_ref().err() {
        let kind = err.status();
        case_status = Some(kind);
        let message = err.cause().map(|c| c.to_string());
        context.notifier().notify(
            notify::event::CaseMessage {
                name: case.name().to_owned(),
                kind,
                message,
                elapsed_s: Some(context.elapased_s()),
            }
            .into(),
        )?;
    }

    context.notifier().notify(
        notify::event::CaseComplete {
            name: case.name().to_owned(),
            elapsed_s: Some(context.elapased_s()),
        }
        .into(),
    )?;

    Ok(case_status != Some(notify::MessageKind::Error))
}

/// Fixed frame used to clean the backtrace with `RUST_BACKTRACE=1`.
#[inline(never)]
fn __rust_begin_short_backtrace<T, F: FnOnce() -> T>(f: F) -> T {
    let result = f();

    // prevent this frame from being tail-call optimised away
    std::hint::black_box(result)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Arc, Barrier};
    use std::time::Duration;

    use super::*;
    use crate::{Source, TestKind};

    struct SchedulerCase {
        name: &'static str,
        exclusive: bool,
        runner: Arc<dyn Fn(&TestContext) -> Result<(), RunError> + Send + Sync>,
    }

    impl SchedulerCase {
        fn concurrent(
            name: &'static str,
            runner: impl Fn(&TestContext) -> Result<(), RunError> + Send + Sync + 'static,
        ) -> Self {
            Self {
                name,
                exclusive: false,
                runner: Arc::new(runner),
            }
        }

        fn exclusive(
            name: &'static str,
            runner: impl Fn(&TestContext) -> Result<(), RunError> + Send + Sync + 'static,
        ) -> Self {
            Self {
                name,
                exclusive: true,
                runner: Arc::new(runner),
            }
        }
    }

    impl Case for SchedulerCase {
        fn name(&self) -> &str {
            self.name
        }

        fn kind(&self) -> TestKind {
            TestKind::Unknown
        }

        fn source(&self) -> Option<&Source> {
            None
        }

        fn exclusive(&self, _state: &TestContext) -> bool {
            self.exclusive
        }

        fn run(&self, context: &TestContext) -> Result<(), RunError> {
            (self.runner)(context)
        }
    }

    fn run_scheduler(cases: Vec<SchedulerCase>, fail_fast: bool) -> bool {
        let mut args = vec!["scheduler-test", "--test-threads=2"];
        if fail_fast {
            args.push("--fail-fast");
        }
        Harness::new()
            .with_args(args)
            .unwrap()
            .parse()
            .unwrap()
            .discover(cases)
            .unwrap()
            .run()
            .unwrap()
    }

    #[test]
    fn exclusive_lane_starts_only_after_concurrent_workers_finish() {
        let barrier = Arc::new(Barrier::new(2));
        let active = Arc::new(AtomicUsize::new(0));
        let overlap = Arc::new(AtomicBool::new(false));
        let exclusive_ran = Arc::new(AtomicBool::new(false));

        let ordinary = |name| {
            let barrier = barrier.clone();
            let active = active.clone();
            SchedulerCase::concurrent(name, move |_| {
                active.fetch_add(1, Ordering::SeqCst);
                barrier.wait();
                std::thread::sleep(Duration::from_millis(25));
                active.fetch_sub(1, Ordering::SeqCst);
                Ok(())
            })
        };
        let exclusive = {
            let active = active.clone();
            let overlap = overlap.clone();
            let exclusive_ran = exclusive_ran.clone();
            SchedulerCase::exclusive("z_exclusive", move |_| {
                exclusive_ran.store(true, Ordering::SeqCst);
                if active.load(Ordering::SeqCst) != 0 {
                    overlap.store(true, Ordering::SeqCst);
                }
                Ok(())
            })
        };

        assert!(run_scheduler(
            vec![ordinary("a_ordinary"), ordinary("b_ordinary"), exclusive],
            false,
        ));
        assert!(exclusive_ran.load(Ordering::SeqCst));
        assert!(!overlap.load(Ordering::SeqCst));
        assert_eq!(active.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn fail_fast_drains_started_workers_and_skips_all_later_work() {
        let barrier = Arc::new(Barrier::new(2));
        let active = Arc::new(AtomicUsize::new(0));
        let slow_finished = Arc::new(AtomicBool::new(false));
        let unstarted_ran = Arc::new(AtomicBool::new(false));
        let exclusive_ran = Arc::new(AtomicBool::new(false));
        let overlap = Arc::new(AtomicBool::new(false));

        let failing = {
            let barrier = barrier.clone();
            SchedulerCase::concurrent("a_failing", move |_| {
                barrier.wait();
                Err(RunError::fail("expected failure"))
            })
        };
        let slow = {
            let barrier = barrier.clone();
            let active = active.clone();
            let slow_finished = slow_finished.clone();
            SchedulerCase::concurrent("b_slow", move |_| {
                active.fetch_add(1, Ordering::SeqCst);
                barrier.wait();
                std::thread::sleep(Duration::from_millis(250));
                active.fetch_sub(1, Ordering::SeqCst);
                slow_finished.store(true, Ordering::SeqCst);
                Ok(())
            })
        };
        let unstarted = {
            let unstarted_ran = unstarted_ran.clone();
            SchedulerCase::concurrent("c_unstarted", move |_| {
                unstarted_ran.store(true, Ordering::SeqCst);
                Ok(())
            })
        };
        let exclusive = {
            let active = active.clone();
            let exclusive_ran = exclusive_ran.clone();
            let overlap = overlap.clone();
            SchedulerCase::exclusive("z_exclusive", move |_| {
                exclusive_ran.store(true, Ordering::SeqCst);
                if active.load(Ordering::SeqCst) != 0 {
                    overlap.store(true, Ordering::SeqCst);
                }
                Ok(())
            })
        };

        assert!(!run_scheduler(
            vec![failing, slow, unstarted, exclusive],
            true,
        ));
        assert!(slow_finished.load(Ordering::SeqCst));
        assert_eq!(active.load(Ordering::SeqCst), 0);
        assert!(!unstarted_ran.load(Ordering::SeqCst));
        assert!(!exclusive_ran.load(Ordering::SeqCst));
        assert!(!overlap.load(Ordering::SeqCst));
    }

    #[test]
    fn non_fail_fast_still_runs_remaining_and_exclusive_cases() {
        let remaining_ran = Arc::new(AtomicBool::new(false));
        let exclusive_ran = Arc::new(AtomicBool::new(false));

        let failing = SchedulerCase::concurrent("a_failing", |_| {
            Err(RunError::fail("expected failure"))
        });
        let remaining = {
            let remaining_ran = remaining_ran.clone();
            SchedulerCase::concurrent("b_remaining", move |_| {
                remaining_ran.store(true, Ordering::SeqCst);
                Ok(())
            })
        };
        let exclusive = {
            let exclusive_ran = exclusive_ran.clone();
            SchedulerCase::exclusive("z_exclusive", move |_| {
                exclusive_ran.store(true, Ordering::SeqCst);
                Ok(())
            })
        };

        assert!(!run_scheduler(
            vec![failing, remaining, exclusive],
            false,
        ));
        assert!(remaining_ran.load(Ordering::SeqCst));
        assert!(exclusive_ran.load(Ordering::SeqCst));
    }

    fn discover_with_args(
        args: impl IntoIterator<Item = impl Into<std::ffi::OsString>>,
        cases: Vec<SchedulerCase>,
    ) -> std::io::Result<Harness<StateDiscovered>> {
        Harness::new()
            .with_args(args)?
            .parse()
            .expect("test arguments should parse")
            .discover(cases)
    }

    fn passing_case(name: &'static str) -> SchedulerCase {
        SchedulerCase::concurrent(name, |_| Ok(()))
    }

    #[test]
    fn empty_unfiltered_discovery_is_still_allowed() {
        let discovered = discover_with_args(["empty"], vec![]).unwrap();
        assert!(discovered.run().unwrap());
    }

    #[test]
    fn skip_only_empty_selection_is_still_allowed() {
        let discovered = discover_with_args(
            ["skip-only", "--skip", "selected"],
            vec![passing_case("selected")],
        )
        .unwrap();
        assert!(discovered.run().unwrap());
    }

    #[test]
    fn a_positive_filter_eliminated_by_skip_is_an_error() {
        let error = discover_with_args(
            ["positive-and-skip", "--skip", "selected", "selected"],
            vec![passing_case("selected")],
        )
        .err()
        .unwrap();
        assert!(error.to_string().contains("no tests matched"));
        assert!(error.to_string().contains("selected"));
    }

    #[test]
    fn exact_filters_require_the_complete_case_name() {
        assert!(discover_with_args(
            ["exact", "--exact", "exact::child"],
            vec![passing_case("exact::child")],
        )
        .unwrap()
        .run()
        .unwrap());

        let error = discover_with_args(
            ["exact-partial", "--exact", "child"],
            vec![passing_case("exact::child")],
        )
        .err()
        .unwrap();
        assert!(error.to_string().contains("no tests matched"));
    }

    #[test]
    fn multiple_positive_filters_are_or_combined() {
        assert!(discover_with_args(
            ["multiple", "missing", "selected"],
            vec![passing_case("selected")],
        )
        .unwrap()
        .run()
        .unwrap());
    }

    #[test]
    fn list_does_not_turn_an_unmatched_filter_into_a_pass() {
        let error = discover_with_args(
            ["list", "--list", "missing"],
            vec![passing_case("selected")],
        )
        .err()
        .unwrap();
        assert!(error.to_string().contains("no tests matched"));
    }

    #[test]
    fn argfile_filters_are_checked_after_argument_expansion() {
        let path = std::env::temp_dir().join(format!(
            "libtest2-harness-filter-{}.args",
            std::process::id()
        ));
        std::fs::write(&path, "missing\n").unwrap();
        let argfile = format!("@{}", path.display());
        let error = discover_with_args(
            ["argfile", argfile.as_str()],
            vec![passing_case("selected")],
        )
        .err()
        .unwrap();
        let _ = std::fs::remove_file(path);
        assert!(error.to_string().contains("no tests matched"));
        assert!(error.to_string().contains("missing"));
    }
}
