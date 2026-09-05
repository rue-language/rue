//! Runtime-owned physical workers for registered query batches.

use std::fmt;
use std::io;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Instant;

use crate::{REGISTERED_BATCH_WORKER_STACK_BYTES, lock};

type Job = Box<dyn FnOnce() + Send + 'static>;

/// Physical query-runtime worker threads alive in this process right now.
///
/// The count is process-wide rather than per-runtime because the resource it
/// tracks is: every runtime in the process draws its worker threads from one
/// host budget, and it is the *total* that a host refuses to grow.
static LIVE_WORKER_THREADS: AtomicUsize = AtomicUsize::new(0);

/// The largest value [`LIVE_WORKER_THREADS`] has ever held in this process.
static PEAK_WORKER_THREADS: AtomicUsize = AtomicUsize::new(0);

/// Physical query-runtime worker threads currently alive process-wide.
///
/// A long-running harness that compiles many programs in one process expects
/// this to return to a small steady state between compilations: worker threads
/// belong to a runtime, and a runtime releases them when it is dropped.
pub fn live_query_worker_threads() -> usize {
    LIVE_WORKER_THREADS.load(Ordering::Relaxed)
}

/// High-water mark of [`live_query_worker_threads`] for this process.
///
/// This is the number a report needs when the host refuses a thread: it says
/// whether the refusal met a process that was accumulating threads or one that
/// was already running at a steady, modest budget (RUE-2043).
pub fn peak_query_worker_threads() -> usize {
    PEAK_WORKER_THREADS.load(Ordering::Relaxed)
}

fn record_worker_birth() {
    let live = LIVE_WORKER_THREADS.fetch_add(1, Ordering::Relaxed) + 1;
    PEAK_WORKER_THREADS.fetch_max(live, Ordering::Relaxed);
}

fn record_worker_death() {
    LIVE_WORKER_THREADS.fetch_sub(1, Ordering::Relaxed);
}

/// Stable opening of a [`WorkerSpawnFailure`] the HOST caused.
///
/// Drivers publish the rendering as an internal-error diagnostic and harnesses
/// match this prefix to separate a loaded host from a compiler defect, so the
/// text is a contract rather than an incidental message. It is exactly the
/// distinction [`WorkerSpawnFailure::is_host_refusal`] draws in-process: a
/// refusal the compiler itself made must not carry this prefix, or a harness
/// reading stderr would report a compiler bug as a busy machine.
pub const WORKER_SPAWN_MESSAGE_PREFIX: &str = "query runtime could not spawn a worker thread";

/// The executor refused to dispatch a registered-batch job.
///
/// Thread creation is the one step of registered-batch dispatch that depends on
/// a resource the compiler does not own. `EAGAIN` under host thread or
/// address-space pressure is a condition of the machine, not a compiler defect,
/// so it is reported as a typed value the caller can turn into a diagnostic
/// rather than an abort of the process. The same typing covers dispatch onto a
/// runtime whose owner already ended its workers, which is a defect but still
/// must not take the process down.
///
/// The payload is a value type rather than an `io::Error` because
/// [`QueryAbort`](crate::QueryAbort) is `Clone + Eq`: the refusal is captured
/// as its rendering, its raw OS code, and the worker budget that was live when
/// it happened.
#[derive(Clone, PartialEq, Eq)]
pub struct WorkerSpawnFailure {
    cause: RefusalCause,
    live_workers: usize,
    worker_stack_bytes: usize,
}

/// What stopped the dispatch.
#[derive(Clone, PartialEq, Eq)]
enum RefusalCause {
    /// The host declined a new physical worker thread.
    Host {
        os_error: String,
        raw_os_error: Option<i32>,
    },
    /// The runtime's owner had already ended its workers. Only a defect
    /// reaches this — an owner shuts down when it is finished with the runtime
    /// — so it renders WITHOUT [`WORKER_SPAWN_MESSAGE_PREFIX`]: that prefix is
    /// how a harness tells a loaded machine from a compiler bug, and this is
    /// the compiler bug.
    OwnerShutdown,
}

impl fmt::Display for WorkerSpawnFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.cause {
            RefusalCause::Host { os_error, .. } => write!(
                formatter,
                "{WORKER_SPAWN_MESSAGE_PREFIX}: {os_error}; {} workers of {} MiB stack were live",
                self.live_workers,
                self.worker_stack_mib()
            ),
            RefusalCause::OwnerShutdown => write!(
                formatter,
                "query runtime received a batch job after its owner shut the workers down; \
                 {} workers of {} MiB stack were live",
                self.live_workers,
                self.worker_stack_mib()
            ),
        }
    }
}

/// The structural rendering is the diagnostic sentence.
///
/// Compiler abort-reporting sites render a [`QueryAbort`](crate::QueryAbort)
/// with `{:?}`, and this refusal must name the OS error and the worker budget
/// wherever it is reported — not only on the one path that formats it
/// deliberately.
impl fmt::Debug for WorkerSpawnFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, formatter)
    }
}

impl WorkerSpawnFailure {
    /// Captures one host refusal.
    ///
    /// `live_workers` is how many physical workers the runtime already owned,
    /// and `worker_stack_bytes` the reservation each of them asks for; together
    /// they are the budget a report needs in order to be actionable.
    pub fn new(error: &io::Error, live_workers: usize, worker_stack_bytes: usize) -> Self {
        Self {
            cause: RefusalCause::Host {
                os_error: error.to_string(),
                raw_os_error: error.raw_os_error(),
            },
            live_workers,
            worker_stack_bytes,
        }
    }

    /// Captures dispatch onto a runtime whose owner already shut it down.
    fn after_owner_shutdown(live_workers: usize, worker_stack_bytes: usize) -> Self {
        Self {
            cause: RefusalCause::OwnerShutdown,
            live_workers,
            worker_stack_bytes,
        }
    }

    /// Whether the machine, rather than the compiler, refused the dispatch.
    ///
    /// A harness separates a loaded host from a compiler defect on this
    /// distinction; across a process boundary it reads the same distinction off
    /// [`WORKER_SPAWN_MESSAGE_PREFIX`] in the rendered message.
    pub const fn is_host_refusal(&self) -> bool {
        matches!(self.cause, RefusalCause::Host { .. })
    }

    /// The operating system's own account of a host refusal.
    ///
    /// Empty when the compiler refused the dispatch itself; see
    /// [`is_host_refusal`](Self::is_host_refusal).
    pub fn os_error(&self) -> &str {
        match &self.cause {
            RefusalCause::Host { os_error, .. } => os_error,
            RefusalCause::OwnerShutdown => "",
        }
    }

    /// The raw `errno` when the host reported one.
    pub const fn raw_os_error(&self) -> Option<i32> {
        match &self.cause {
            RefusalCause::Host { raw_os_error, .. } => *raw_os_error,
            RefusalCause::OwnerShutdown => None,
        }
    }

    /// Physical workers this runtime already owned when the dispatch was
    /// refused.
    pub const fn live_workers(&self) -> usize {
        self.live_workers
    }

    /// Stack reservation each physical worker requests.
    pub const fn worker_stack_bytes(&self) -> usize {
        self.worker_stack_bytes
    }

    /// Per-worker stack reservation in whole MiB, for diagnostics.
    pub const fn worker_stack_mib(&self) -> usize {
        self.worker_stack_bytes / (1024 * 1024)
    }
}

/// A query-runtime-owned set of reusable physical workers.
///
/// This is deliberately not a concurrency authority. Registered batches must
/// first acquire slots from `BatchWorkerClaim`; the executor only maps those
/// already-granted slots onto long-lived operating-system threads.
pub(crate) struct ReusableBatchExecutor {
    /// `None` once the runtime's owner has shut the executor down. The slot is
    /// behind a lock because shutdown reaches the executor through the shared
    /// `Arc<RuntimeCore>` rather than through a unique borrow, and because it
    /// is what makes shutdown and dispatch exclusive of one another.
    sender: Mutex<Option<mpsc::Sender<Job>>>,
    receiver: Arc<Mutex<mpsc::Receiver<Job>>>,
    workers: Mutex<Vec<JoinHandle<()>>>,
    worker_capacity: usize,
    #[cfg(test)]
    forced_spawn_failure: Mutex<Option<io::Error>>,
}

impl fmt::Debug for ReusableBatchExecutor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ReusableBatchExecutor")
            .field("workers", &lock(&self.workers).len())
            .field("worker_capacity", &self.worker_capacity)
            .finish_non_exhaustive()
    }
}

impl ReusableBatchExecutor {
    pub(crate) fn new(worker_capacity: usize) -> Self {
        let (sender, receiver) = mpsc::channel::<Job>();
        Self {
            sender: Mutex::new(Some(sender)),
            receiver: Arc::new(Mutex::new(receiver)),
            workers: Mutex::new(Vec::with_capacity(worker_capacity)),
            worker_capacity,
            #[cfg(test)]
            forced_spawn_failure: Mutex::new(None),
        }
    }

    /// Dispatches one job onto this runtime's physical workers.
    ///
    /// Returns the refusal when the host declines a new worker thread, or when
    /// the runtime's owner already shut the workers down. Nothing is queued in
    /// either case, so the caller owes no join for this job and may surface the
    /// condition without leaving batch state behind.
    pub(crate) fn submit<R>(
        &self,
        job: impl FnOnce() -> R + Send + 'static,
    ) -> Result<(BatchJobHandle<R>, u64), WorkerSpawnFailure>
    where
        R: Send + 'static,
    {
        // The queue slot stays locked across the whole dispatch. That is what
        // makes shutdown and submission exclusive of one another: a `Sender`
        // cloned out of the slot would keep the channel connected, and
        // `shutdown` would then join workers that never observe the close.
        let sender = lock(&self.sender);
        let Some(sender) = sender.as_ref() else {
            return Err(WorkerSpawnFailure::after_owner_shutdown(
                lock(&self.workers).len(),
                REGISTERED_BATCH_WORKER_STACK_BYTES,
            ));
        };
        let mut thread_births = 0;
        {
            let mut workers = lock(&self.workers);
            if workers.len() < self.worker_capacity {
                let index = workers.len();
                let worker = self.spawn_worker(index).map_err(|error| {
                    WorkerSpawnFailure::new(&error, index, REGISTERED_BATCH_WORKER_STACK_BYTES)
                })?;
                workers.push(worker);
                thread_births = 1;
            }
        }
        let (completed, receiver) = mpsc::sync_channel(1);
        sender
            .send(Box::new(move || {
                let result = catch_unwind(AssertUnwindSafe(job));
                let finished_at = Instant::now();
                let _ = completed.send((result, finished_at));
            }))
            .expect("the executor owns the receiver its queue delivers to");
        Ok((
            BatchJobHandle {
                receiver: Some(receiver),
                #[cfg(test)]
                drop_wait_started: None,
            },
            thread_births,
        ))
    }

    /// Creates one physical worker, or reports the host's refusal.
    ///
    /// Tests arm [`force_next_worker_spawn_failure`] here rather than shrinking
    /// a process-wide thread limit, so the refusal path is exercised without a
    /// resource the test would have to restore.
    ///
    /// [`force_next_worker_spawn_failure`]: Self::force_next_worker_spawn_failure
    fn spawn_worker(&self, index: usize) -> io::Result<JoinHandle<()>> {
        #[cfg(test)]
        if let Some(error) = lock(&self.forced_spawn_failure).take() {
            return Err(error);
        }
        let receiver = self.receiver.clone();
        // The gauge is raised before the thread exists and lowered by a guard
        // the thread owns, so the pair cannot race: a worker that exits
        // immediately can only ever lower a count this call already raised.
        record_worker_birth();
        thread::Builder::new()
            .name(format!("rue-query-worker-{index}"))
            .stack_size(REGISTERED_BATCH_WORKER_STACK_BYTES)
            .spawn(move || {
                // The gauge is lowered by this guard rather than at the
                // `return` below so an unwinding worker is still accounted for.
                let _accounting = WorkerLifetime;
                loop {
                    let job = {
                        // Exactly one idle worker waits on the receiver;
                        // after it takes a job, the next idle worker takes
                        // its place. Executing jobs never holds this lock.
                        let receiver = lock(&receiver);
                        receiver.recv()
                    };
                    let Ok(job) = job else {
                        return;
                    };
                    job();
                }
            })
            .inspect_err(|_| record_worker_death())
    }

    /// Disconnects the job queue and joins every physical worker.
    ///
    /// Called when the runtime's owner is finished with it. `Drop` alone is not
    /// enough: the executor lives inside the `Arc<RuntimeCore>`, which an
    /// evaluator graph may hold in cycles, so the core is not reliably freed
    /// when a compilation ends; the full account is at the destructor of
    /// `rue-compiler`'s `RevisionedQueryDatabase` (RUE-2043).
    ///
    /// The queue slot is released before the joins, so a dispatch racing this
    /// teardown is refused with a typed error instead of blocking; the queue is
    /// already drained of jobs the racing caller might still be waiting on.
    pub(crate) fn shutdown(&self) {
        lock(&self.sender).take();
        // The handles are taken out from under the lock before any join. A job
        // still running is free to reach `submit`, whose refusal path reads the
        // worker list, and joining while holding that lock would deadlock
        // against the very thread being joined.
        let workers = lock(&self.workers).drain(..).collect::<Vec<_>>();
        for worker in workers {
            worker
                .join()
                .expect("query runtime workers contain job panics");
        }
    }

    /// Physical workers this executor still owns.
    ///
    /// Zero after [`shutdown`](Self::shutdown): every handle was joined and
    /// drained, so the operating-system threads are gone rather than merely
    /// unreferenced.
    pub(crate) fn live_worker_count(&self) -> usize {
        lock(&self.workers).len()
    }

    /// Makes the next physical worker creation fail with `error`.
    #[cfg(test)]
    pub(crate) fn force_next_worker_spawn_failure(&self, error: io::Error) {
        *lock(&self.forced_spawn_failure) = Some(error);
    }
}

/// Decrements the process-wide live-worker gauge when a worker thread ends.
struct WorkerLifetime;

impl Drop for WorkerLifetime {
    fn drop(&mut self) {
        record_worker_death();
    }
}

impl Drop for ReusableBatchExecutor {
    fn drop(&mut self) {
        // Disconnecting the queue wakes the idle receiver. Each worker then
        // observes the same closed queue in turn and exits before runtime-owned
        // state is destroyed. This is the backstop for a core that does become
        // unreachable — an executor built directly, or a runtime whose graph
        // holds no cycle. Where an owner already ran `shutdown`, draining an
        // emptied worker list is a no-op.
        self.shutdown();
    }
}

pub(crate) struct BatchJobHandle<R> {
    receiver: Option<mpsc::Receiver<(thread::Result<R>, Instant)>>,
    #[cfg(test)]
    drop_wait_started: Option<mpsc::SyncSender<()>>,
}

impl<R> BatchJobHandle<R> {
    pub(crate) fn join(mut self) -> (thread::Result<R>, Instant) {
        self.receiver
            .take()
            .expect("batch job handle joins at most once")
            .recv()
            .expect("a live query runtime worker completes every submitted job")
    }
}

impl<R> Drop for BatchJobHandle<R> {
    fn drop(&mut self) {
        let Some(receiver) = self.receiver.take() else {
            return;
        };
        // Submission can panic after earlier jobs were queued, most notably if
        // a later physical worker cannot be created. Joining every unconsumed
        // handle keeps those earlier jobs inside the structured batch scope:
        // wait edges, permit donation, and the worker claim cannot unwind
        // while a submitted job still owns batch/runtime state.
        #[cfg(test)]
        if let Some(drop_wait_started) = self.drop_wait_started.take() {
            drop_wait_started.send(()).unwrap();
        }
        let _ = receiver.recv();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    #[test]
    fn a_refused_worker_thread_is_a_typed_submission_error() {
        let executor = ReusableBatchExecutor::new(2);
        // One worker exists before the host starts refusing, so the reported
        // budget is the live count rather than the configured capacity.
        let (first, births) = executor.submit(|| 1).unwrap();
        assert_eq!(births, 1);
        assert_eq!(first.join().0.unwrap(), 1);

        executor.force_next_worker_spawn_failure(io::Error::from_raw_os_error(
            EAGAIN_LIKE_RAW_OS_ERROR,
        ));
        let Err(refused) = executor.submit(|| 2) else {
            panic!("a refused worker thread must not be reported as a dispatched job")
        };
        assert!(refused.is_host_refusal());
        assert_eq!(refused.raw_os_error(), Some(EAGAIN_LIKE_RAW_OS_ERROR));
        assert_eq!(refused.live_workers(), 1);
        assert_eq!(
            refused.worker_stack_bytes(),
            REGISTERED_BATCH_WORKER_STACK_BYTES
        );
        assert!(!refused.os_error().is_empty());
        let rendered = refused.to_string();
        assert!(
            rendered.starts_with(WORKER_SPAWN_MESSAGE_PREFIX),
            "a refusal must render with the contracted prefix: {rendered}"
        );
        assert!(
            rendered.ends_with("; 1 workers of 8 MiB stack were live"),
            "a refusal must name the live worker budget: {rendered}"
        );
        assert_eq!(format!("{refused:?}"), rendered);

        // The refusal queued nothing, so the executor still dispatches onto the
        // workers it already owns and drops without a job to join.
        let (next, next_births) = executor.submit(|| 3).unwrap();
        assert_eq!(next_births, 1);
        assert_eq!(next.join().0.unwrap(), 3);
    }

    /// macOS `EAGAIN`, the code a thread-exhausted host returns in the field.
    const EAGAIN_LIKE_RAW_OS_ERROR: i32 = 35;

    /// RUE-2043: after the owning runtime tears the executor down, dispatch is
    /// refused with a value the caller can report, and no worker is created for
    /// a job that will never run.
    #[test]
    fn dispatch_after_shutdown_is_a_typed_refusal_rather_than_a_panic() {
        let executor = ReusableBatchExecutor::new(2);
        let (first, births) = executor.submit(|| 1).unwrap();
        assert_eq!(births, 1);
        assert_eq!(first.join().0.unwrap(), 1);

        executor.shutdown();
        assert_eq!(
            executor.live_worker_count(),
            0,
            "shutdown must join the physical workers, not merely close the queue"
        );

        let Err(refused) = executor.submit(|| 2) else {
            panic!("a torn-down executor must not report a job as dispatched")
        };
        assert!(!refused.is_host_refusal());
        assert_eq!(refused.raw_os_error(), None);
        assert!(refused.os_error().is_empty());
        assert_eq!(refused.live_workers(), 0);
        let rendered = refused.to_string();
        assert!(
            !rendered.contains(WORKER_SPAWN_MESSAGE_PREFIX),
            "a teardown defect must not render as the host refusal a harness \
             classifies as a resource condition: {rendered}"
        );
        assert!(
            rendered.contains("after its owner shut the workers down"),
            "the refusal must name what actually happened: {rendered}"
        );
        assert_eq!(format!("{refused:?}"), rendered);
        assert_eq!(
            executor.live_worker_count(),
            0,
            "a refused dispatch must not create a worker for a job it never queued"
        );
    }

    #[test]
    fn submitted_jobs_reuse_one_physical_worker() {
        let executor = ReusableBatchExecutor::new(1);

        let (first, first_births) = executor.submit(|| 41).unwrap();
        let (second, second_births) = executor.submit(|| 42).unwrap();
        assert_eq!(first_births, 1);
        assert_eq!(second_births, 0);
        let (first, _) = first.join();
        let (second, _) = second.join();
        assert_eq!(first.unwrap(), 41);
        assert_eq!(second.unwrap(), 42);
    }

    #[test]
    fn dropping_an_unjoined_handle_waits_for_job_completion() {
        let executor = ReusableBatchExecutor::new(1);
        let (job_started_sender, job_started) = mpsc::sync_channel(1);
        let (release_sender, release) = mpsc::sync_channel(1);
        let completed = Arc::new(AtomicBool::new(false));
        let completed_by_job = completed.clone();
        let (mut handle, births) = executor
            .submit(move || {
                job_started_sender.send(()).unwrap();
                release.recv().unwrap();
                completed_by_job.store(true, Ordering::Release);
            })
            .unwrap();
        assert_eq!(births, 1);
        job_started.recv().unwrap();

        let (drop_started_sender, drop_started) = mpsc::sync_channel(1);
        handle.drop_wait_started = Some(drop_started_sender);
        let (drop_finished_sender, drop_finished) = mpsc::sync_channel(1);
        let dropper = thread::spawn(move || {
            drop(handle);
            drop_finished_sender.send(()).unwrap();
        });
        drop_started.recv().unwrap();
        assert!(
            drop_finished.try_recv().is_err(),
            "dropping a live handle must not detach its submitted job"
        );
        assert!(!completed.load(Ordering::Acquire));

        release_sender.send(()).unwrap();
        drop_finished.recv().unwrap();
        dropper.join().unwrap();
        assert!(completed.load(Ordering::Acquire));
    }
}
