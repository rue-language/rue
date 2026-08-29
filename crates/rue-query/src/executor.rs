//! Runtime-owned physical workers for registered query batches.

use std::fmt;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Instant;

use crate::{REGISTERED_BATCH_WORKER_STACK_BYTES, lock};

type Job = Box<dyn FnOnce() + Send + 'static>;

/// A query-runtime-owned set of reusable physical workers.
///
/// This is deliberately not a concurrency authority. Registered batches must
/// first acquire slots from `BatchWorkerClaim`; the executor only maps those
/// already-granted slots onto long-lived operating-system threads.
pub(crate) struct ReusableBatchExecutor {
    sender: Option<mpsc::Sender<Job>>,
    receiver: Arc<Mutex<mpsc::Receiver<Job>>>,
    workers: Mutex<Vec<JoinHandle<()>>>,
    worker_capacity: usize,
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
            sender: Some(sender),
            receiver: Arc::new(Mutex::new(receiver)),
            workers: Mutex::new(Vec::with_capacity(worker_capacity)),
            worker_capacity,
        }
    }

    pub(crate) fn submit<R>(
        &self,
        job: impl FnOnce() -> R + Send + 'static,
    ) -> (BatchJobHandle<R>, u64)
    where
        R: Send + 'static,
    {
        let mut thread_births = 0;
        {
            let mut workers = lock(&self.workers);
            if workers.len() < self.worker_capacity {
                let receiver = self.receiver.clone();
                let index = workers.len();
                let worker = thread::Builder::new()
                    .name(format!("rue-query-worker-{index}"))
                    .stack_size(REGISTERED_BATCH_WORKER_STACK_BYTES)
                    .spawn(move || {
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
                    .expect("query runtime worker thread must spawn");
                workers.push(worker);
                thread_births = 1;
            }
        }
        let (completed, receiver) = mpsc::sync_channel(1);
        self.sender
            .as_ref()
            .expect("query runtime executor is live while its core is live")
            .send(Box::new(move || {
                let result = catch_unwind(AssertUnwindSafe(job));
                let finished_at = Instant::now();
                let _ = completed.send((result, finished_at));
            }))
            .expect("query runtime executor workers remain live with the runtime");
        (
            BatchJobHandle {
                receiver: Some(receiver),
                #[cfg(test)]
                drop_wait_started: None,
            },
            thread_births,
        )
    }
}

impl Drop for ReusableBatchExecutor {
    fn drop(&mut self) {
        // Disconnecting the queue wakes the idle receiver. Each worker then
        // observes the same closed queue in turn and exits before runtime-owned
        // state is destroyed.
        self.sender.take();
        for worker in lock(&self.workers).drain(..) {
            worker
                .join()
                .expect("query runtime workers contain job panics");
        }
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
    fn submitted_jobs_reuse_one_physical_worker() {
        let executor = ReusableBatchExecutor::new(1);

        let (first, first_births) = executor.submit(|| 41);
        let (second, second_births) = executor.submit(|| 42);
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
        let (mut handle, births) = executor.submit(move || {
            job_started_sender.send(()).unwrap();
            release.recv().unwrap();
            completed_by_job.store(true, Ordering::Release);
        });
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
