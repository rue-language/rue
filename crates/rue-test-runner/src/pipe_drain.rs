//! Bounded, non-blocking draining of a child process's pipes.
//!
//! One implementation serves every harness in the tree and the `rue test`
//! runner in the compiler driver (ADR-0083 §3, which adopts these mechanics as
//! contract). It lives in its own module because a second copy is exactly the
//! kind of duplicate that drifts: the budget arithmetic, the incremental
//! hand-off, and the bounded finish are each load-bearing for a different
//! failure mode, and a copy that loses one of them fails only under load.
//!
//! Three properties are why this is not `read_to_end` on a joined thread:
//!
//! - **Chunks are handed over incrementally**, not at EOF. If a daemonized
//!   descendant inherits the write end, the reader never sees EOF, and a
//!   caller that joined the thread would block forever instead of reporting
//!   the bytes it already has.
//! - **The retention budget is enforced as bytes arrive**, not after. A test
//!   that writes a gigabyte must cost a bounded amount of the runner's memory,
//!   which checking `Output::stdout.len()` afterwards cannot deliver.
//! - **The true byte count is tracked separately from what is retained.** A
//!   consumer publishing capture metadata needs to say how much the process
//!   actually wrote, which is not the size of the prefix that was kept.

use std::io::Read as IoRead;
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// How long a caller should wait for a drain helper after the child has exited
/// or been killed.
///
/// A well-behaved child closes its streams promptly, so this normally just
/// observes [`DrainMessage::Done`]. If a descendant process inherited a pipe fd
/// and keeps it open, the reader thread may block forever; bounding collection
/// keeps the caller moving and returns whatever bytes were already drained.
pub const PIPE_DRAIN_FINISH_TIMEOUT: Duration = Duration::from_millis(500);

/// Bytes read from the pipe in one `read(2)`, and how many of them were kept.
///
/// `read` is the true size of the chunk and `retained` the prefix that fit
/// inside the budget; the two differ only once the budget is exhausted.
enum DrainMessage {
    Chunk { retained: Vec<u8>, read: usize },
    Overflow,
    Done,
}

/// A pipe being drained on a helper thread.
///
/// Created by [`spawn_pipe_drain`]. The owner calls [`PipeDrain::poll`] while
/// waiting on the child, then [`PipeDrain::finish`] once it has exited.
pub struct PipeDrain {
    rx: mpsc::Receiver<DrainMessage>,
    bytes: Vec<u8>,
    bytes_total: u64,
    done: bool,
    overflowed: bool,
}

impl PipeDrain {
    /// Collect whatever the reader thread has already produced, without
    /// blocking.
    pub fn poll(&mut self) {
        // Bound work per child-status poll. An unbounded producer must not keep
        // the caller's wait loop inside `try_recv` forever.
        for _ in 0..64 {
            match self.rx.try_recv() {
                Ok(message) => self.handle(message),
                Err(mpsc::TryRecvError::Empty) => return,
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.done = true;
                    return;
                }
            }
        }
    }

    /// Collect the remaining output, giving up after `timeout`.
    ///
    /// The bound is the whole point: an escaped descendant holding the write
    /// end open means EOF never arrives, and the caller still owes its own
    /// caller an answer.
    pub fn finish(&mut self, timeout: Duration) {
        let deadline = Instant::now() + timeout;
        while !self.done {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return;
            }
            match self.rx.recv_timeout(remaining) {
                Ok(message) => self.handle(message),
                Err(_) => {
                    self.done = true;
                    return;
                }
            }
        }
    }

    /// The retained prefix: at most the configured budget.
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Take the retained prefix, consuming the drain.
    pub fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }

    /// Every byte the process wrote to this stream, budget or no budget.
    ///
    /// This is the count a capture record publishes; `bytes().len()` is only
    /// what was kept.
    pub fn bytes_total(&self) -> u64 {
        self.bytes_total
    }

    /// Whether the stream exceeded its retention budget.
    pub fn overflowed(&self) -> bool {
        self.overflowed
    }

    /// Whether the reader thread reached end of stream.
    pub fn is_done(&self) -> bool {
        self.done
    }

    fn handle(&mut self, message: DrainMessage) {
        match message {
            DrainMessage::Chunk { retained, read } => {
                self.bytes.extend(retained);
                self.bytes_total = self.bytes_total.saturating_add(read as u64);
            }
            DrainMessage::Overflow => self.overflowed = true,
            DrainMessage::Done => self.done = true,
        }
    }
}

/// Drain a pipe on a helper thread, sending chunks as they arrive.
///
/// `output_limit` bounds the bytes retained, not the bytes read: reading
/// continues past the budget so the caller learns the true size and so the
/// child never blocks in `write(2)` against a full pipe while the caller is
/// deciding what to do about the overflow.
pub fn spawn_pipe_drain<R: IoRead + Send + 'static>(
    pipe: Option<R>,
    output_limit: Option<usize>,
) -> PipeDrain {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        if let Some(mut reader) = pipe {
            let mut buf = [0; 8192];
            let mut retained = 0usize;
            let mut overflowed = false;
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        let keep = if overflowed {
                            0
                        } else {
                            output_limit
                                .map(|limit| n.min(limit.saturating_sub(retained)))
                                .unwrap_or(n)
                        };
                        if tx
                            .send(DrainMessage::Chunk {
                                retained: buf[..keep].to_vec(),
                                read: n,
                            })
                            .is_err()
                        {
                            return;
                        }
                        retained += keep;
                        if keep < n && !overflowed {
                            overflowed = true;
                            if tx.send(DrainMessage::Overflow).is_err() {
                                return;
                            }
                        }
                    }
                    Err(_) => break,
                }
            }
        }
        let _ = tx.send(DrainMessage::Done);
    });
    PipeDrain {
        rx,
        bytes: Vec::new(),
        bytes_total: 0,
        done: false,
        overflowed: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The budget bounds what is kept, never what is counted: a consumer must
    /// be able to report how much the process actually wrote.
    #[test]
    fn retains_a_prefix_while_counting_every_byte() {
        let payload = vec![b'x'; 5000];
        let mut drain = spawn_pipe_drain(Some(std::io::Cursor::new(payload)), Some(1000));
        drain.finish(Duration::from_secs(5));
        assert!(drain.overflowed());
        assert_eq!(drain.bytes().len(), 1000);
        assert_eq!(drain.bytes_total(), 5000);
    }

    /// A stream inside its budget reports no overflow and a total equal to what
    /// it retained.
    #[test]
    fn a_stream_within_budget_never_reports_overflow() {
        let mut drain = spawn_pipe_drain(Some(std::io::Cursor::new(b"hello".to_vec())), Some(1000));
        drain.finish(Duration::from_secs(5));
        assert!(!drain.overflowed());
        assert_eq!(drain.bytes(), b"hello");
        assert_eq!(drain.bytes_total(), 5);
        assert!(drain.is_done());
    }

    /// No budget means everything is retained.
    #[test]
    fn an_unbounded_drain_retains_everything() {
        let payload = vec![b'y'; 20_000];
        let mut drain = spawn_pipe_drain(Some(std::io::Cursor::new(payload)), None);
        drain.finish(Duration::from_secs(5));
        assert!(!drain.overflowed());
        assert_eq!(drain.bytes().len(), 20_000);
        assert_eq!(drain.bytes_total(), 20_000);
    }

    /// An absent pipe is not an error: a caller that did not redirect a stream
    /// still gets a drain it can poll and finish.
    #[test]
    fn an_absent_pipe_finishes_empty() {
        let mut drain = spawn_pipe_drain(None::<std::io::Cursor<Vec<u8>>>, Some(16));
        drain.finish(Duration::from_secs(5));
        assert!(drain.is_done());
        assert!(drain.bytes().is_empty());
        assert_eq!(drain.bytes_total(), 0);
    }
}
