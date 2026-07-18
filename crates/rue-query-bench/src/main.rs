//! Reproducible structural and latency microbenchmark for `rue-query`.

use std::env;
use std::process;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::Instant;

use rue_query::{
    CancellationToken, QueryDiagnostic, QueryKey, QueryOutput, QueryRuntime, Revision, WorkItem,
};

const DEFAULT_KEYS: usize = 256;
const DEFAULT_WORKERS: usize = 4;

#[derive(Clone, PartialEq, Eq, Hash)]
struct Key(usize);

impl QueryKey for Key {
    fn stable_identity(&self) -> String {
        self.0.to_string()
    }
}

fn main() {
    let (keys, workers) = parse_args();
    let runtime = QueryRuntime::new(workers);
    let family = runtime
        .family::<Key, u64>("prototype-benchmark", keys * 2 + 1)
        .expect("benchmark family has a unique name");
    let checksum = AtomicU64::new(0);

    let started = Instant::now();
    run_batch(
        &runtime,
        &family,
        Revision::new(1, 1),
        keys,
        workers,
        &checksum,
    );
    let cold_micros = started.elapsed().as_micros();

    let started = Instant::now();
    run_batch(
        &runtime,
        &family,
        Revision::new(2, 1),
        keys,
        workers,
        &checksum,
    );
    let reuse_micros = started.elapsed().as_micros();

    let started = Instant::now();
    run_batch(
        &runtime,
        &family,
        Revision::new(3, 3),
        keys,
        workers,
        &checksum,
    );
    let red_micros = started.elapsed().as_micros();

    let started = Instant::now();
    let join_stamp = run_hot_join(&runtime, &family, keys, workers);
    let join_micros = started.elapsed().as_micros();

    let metrics = runtime.metrics();
    let retention = family.retention();
    println!(
        concat!(
            "{{\"schema\":1,\"keys\":{},\"workers\":{},",
            "\"cold_micros\":{},\"reuse_micros\":{},\"red_micros\":{},\"join_micros\":{},",
            "\"checksum\":{},\"join_stamp\":{},\"claims\":{},\"joins\":{},\"reuses\":{},",
            "\"green_publications\":{},\"red_publications\":{},",
            "\"peak_active_bodies\":{},\"retained_terminals\":{},",
            "\"retained_nodes\":{},\"evictions\":{}}}"
        ),
        keys,
        workers,
        cold_micros,
        reuse_micros,
        red_micros,
        join_micros,
        checksum.load(Ordering::Relaxed),
        join_stamp,
        metrics.claims,
        metrics.joins,
        metrics.reuses,
        metrics.green_publications,
        metrics.red_publications,
        metrics.peak_active_bodies,
        retention.terminals,
        retention.memo_nodes,
        metrics.evictions,
    );
}

fn run_hot_join(
    runtime: &QueryRuntime,
    family: &rue_query::QueryFamily<Key, u64>,
    keys: usize,
    workers: usize,
) -> u64 {
    let revision = Revision::new(4, 4);
    let key = Key(keys);
    let (started_tx, started_rx) = mpsc::channel();
    let (finish_tx, finish_rx) = mpsc::channel();
    let owner_runtime = runtime.clone();
    let owner_family = family.clone();
    let owner_key = key.clone();
    let owner = thread::spawn(move || {
        owner_runtime.query(
            &owner_family,
            revision,
            owner_key,
            CancellationToken::new(),
            |_| {
                started_tx.send(()).expect("benchmark coordinator is live");
                finish_rx.recv().expect("benchmark coordinator is live");
                Ok(QueryOutput::success(1))
            },
        )
    });
    started_rx.recv().expect("benchmark owner started");
    let joins_before = runtime.metrics().joins;
    let mut joiners = Vec::new();
    for _ in 0..workers {
        let runtime = runtime.clone();
        let family = family.clone();
        let key = key.clone();
        joiners.push(thread::spawn(move || {
            runtime.query(&family, revision, key, CancellationToken::new(), |_| {
                panic!("hot-key joiner must not compute")
            })
        }));
    }
    while runtime.metrics().joins < joins_before + workers as u64 {
        thread::yield_now();
    }
    finish_tx.send(()).expect("benchmark owner is live");
    let owner = owner.join().expect("owner thread did not panic").unwrap();
    let stamp = owner.stamp();
    for joiner in joiners {
        let terminal = joiner.join().expect("joiner thread did not panic").unwrap();
        assert_eq!(terminal.stamp(), stamp);
    }
    stamp
}

fn run_batch(
    runtime: &QueryRuntime,
    family: &rue_query::QueryFamily<Key, u64>,
    revision: Revision,
    keys: usize,
    workers: usize,
    checksum: &AtomicU64,
) {
    let next = AtomicUsize::new(0);
    thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| {
                loop {
                    let key = next.fetch_add(1, Ordering::Relaxed);
                    if key >= keys {
                        break;
                    }
                    let terminal = runtime
                        .query(family, revision, Key(key), CancellationToken::new(), |_| {
                            Ok(QueryOutput::success(key as u64)
                                .with_diagnostics(vec![QueryDiagnostic::new(
                                    format!("key-{key}"),
                                    "representative",
                                    None,
                                )])
                                .with_work(vec![
                                    WorkItem::new("key", 1),
                                    WorkItem::new("operations", (key % 7 + 1) as u64),
                                ]))
                        })
                        .expect("benchmark queries do not cancel or cycle");
                    checksum.fetch_add(terminal.stamp() + key as u64, Ordering::Relaxed);
                }
            });
        }
    });
}

fn parse_args() -> (usize, usize) {
    let mut keys = DEFAULT_KEYS;
    let mut workers = DEFAULT_WORKERS;
    let mut args = env::args().skip(1);
    while let Some(argument) = args.next() {
        let value = args.next().unwrap_or_else(|| usage(&argument));
        match argument.as_str() {
            "--keys" => keys = parse_positive(&argument, &value),
            "--workers" => workers = parse_positive(&argument, &value),
            _ => usage(&argument),
        }
    }
    (keys, workers)
}

fn parse_positive(flag: &str, value: &str) -> usize {
    value
        .parse::<usize>()
        .ok()
        .filter(|value| *value > 0)
        .unwrap_or_else(|| usage(flag))
}

fn usage(argument: &str) -> ! {
    eprintln!("invalid argument {argument:?}");
    eprintln!("usage: rue-query-bench [--keys N] [--workers N]");
    process::exit(2);
}
