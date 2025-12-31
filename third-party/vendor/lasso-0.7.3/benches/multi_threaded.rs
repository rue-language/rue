mod setup;

use ahash::RandomState as AhashRandomState;
use criterion::{black_box, criterion_group, criterion_main, BatchSize, Criterion, Throughput};
use fxhash::FxBuildHasher;
use lasso::{Capacity, Spur, ThreadedRodeo};
use setup::{bench_lines, INPUT, NUM_THREADS};
use std::{
    collections::hash_map::RandomState,
    hash::BuildHasher,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Barrier,
    },
    thread,
    time::{Duration, Instant},
};

fn rodeo_std(c: &mut Criterion) {
    let mut group = c.benchmark_group("ThreadedRodeo 1 Thread (std)");
    group.throughput(Throughput::Bytes(INPUT.len() as u64));

    let setup = ThreadedRodeoEmptySetup::new(RandomState::default());
    group.bench_function("get_or_intern (empty)", |b| {
        b.iter_batched(
            || setup.empty_rodeo(),
            |rodeo| {
                for &line in setup.lines() {
                    black_box(rodeo.get_or_intern(line));
                }
            },
            BatchSize::PerIteration,
        )
    });

    let mut setup = ThreadedRodeoFilledSetup::new(RandomState::default());
    group.bench_function("get_or_intern (filled)", |b| {
        b.iter(|| {
            for &line in setup.lines() {
                black_box(setup.filled_rodeo_mut().get_or_intern(line));
            }
        })
    });

    let setup = ThreadedRodeoEmptySetup::new(RandomState::default());
    group.bench_function("try_get_or_intern (empty)", |b| {
        b.iter_batched(
            || setup.empty_rodeo(),
            |rodeo| {
                for &line in setup.lines() {
                    black_box(rodeo.try_get_or_intern(line).unwrap());
                }
            },
            BatchSize::PerIteration,
        )
    });

    let mut setup = ThreadedRodeoFilledSetup::new(RandomState::default());
    group.bench_function("try_get_or_intern (filled)", |b| {
        b.iter(|| {
            for &line in setup.lines() {
                black_box(setup.filled_rodeo_mut().try_get_or_intern(line).unwrap());
            }
        })
    });

    let setup = ThreadedRodeoEmptySetup::new(RandomState::default());
    group.bench_function("get (empty)", |b| {
        b.iter_batched(
            || setup.empty_rodeo(),
            |rodeo| {
                for &line in setup.lines() {
                    black_box(rodeo.get(line));
                }
            },
            BatchSize::PerIteration,
        )
    });

    let setup = ThreadedRodeoFilledSetup::new(RandomState::default());
    group.bench_function("get (filled)", |b| {
        b.iter_batched(
            || setup.filled_rodeo(),
            |rodeo| {
                for &line in setup.lines() {
                    black_box(rodeo.get(line));
                }
            },
            BatchSize::PerIteration,
        )
    });

    let setup = ThreadedRodeoFilledSetup::new(RandomState::default());
    group.bench_function("resolve", |b| {
        b.iter_batched(
            || setup.filled_rodeo(),
            |rodeo| {
                for key in setup.keys() {
                    black_box(rodeo.resolve(key));
                }
            },
            BatchSize::PerIteration,
        )
    });

    let setup = ThreadedRodeoFilledSetup::new(RandomState::default());
    group.bench_function("try_resolve", |b| {
        b.iter_batched(
            || setup.filled_rodeo(),
            |rodeo| {
                for key in setup.keys() {
                    black_box(rodeo.try_resolve(key).unwrap());
                }
            },
            BatchSize::PerIteration,
        )
    });

    group.finish();
}

fn rodeo_std_threaded(c: &mut Criterion) {
    let mut group = c.benchmark_group("ThreadedRodeo 24 Thread (std)");
    group.throughput(Throughput::Bytes(INPUT.len() as u64));

    group.bench_function("get_or_intern", |b| {
        b.iter_custom(|iters| {
            run_threaded_filled(
                |rodeo, _| {
                    for &line in setup::bench_lines() {
                        black_box(rodeo.get_or_intern(line));
                    }
                },
                NUM_THREADS,
                iters,
                RandomState::new(),
            )
        })
    });

    group.bench_function("try_get_or_intern", |b| {
        b.iter_custom(|iters| {
            run_threaded_filled(
                |rodeo, _| {
                    for &line in setup::bench_lines() {
                        black_box(rodeo.try_get_or_intern(line).unwrap());
                    }
                },
                NUM_THREADS,
                iters,
                RandomState::new(),
            )
        })
    });

    group.bench_function("get", |b| {
        b.iter_custom(|iters| {
            run_threaded_filled(
                |rodeo, _| {
                    for &line in setup::bench_lines() {
                        black_box(rodeo.get(line));
                    }
                },
                NUM_THREADS,
                iters,
                RandomState::new(),
            )
        })
    });

    group.bench_function("resolve", |b| {
        b.iter_custom(|iters| {
            run_threaded_filled(
                |rodeo, keys| {
                    for key in keys {
                        black_box(rodeo.resolve(key));
                    }
                },
                NUM_THREADS,
                iters,
                RandomState::new(),
            )
        })
    });

    group.bench_function("try_resolve", |b| {
        b.iter_custom(|iters| {
            run_threaded_filled(
                |rodeo, keys| {
                    for key in keys {
                        black_box(rodeo.try_resolve(key).unwrap());
                    }
                },
                NUM_THREADS,
                iters,
                RandomState::new(),
            )
        })
    });

    group.finish();
}

fn rodeo_ahash(c: &mut Criterion) {
    let mut group = c.benchmark_group("ThreadedRodeo 1 Thread (ahash)");
    group.throughput(Throughput::Bytes(INPUT.len() as u64));

    let setup = ThreadedRodeoEmptySetup::new(AhashRandomState::new());
    group.bench_function("get_or_intern (empty)", |b| {
        b.iter_batched(
            || setup.empty_rodeo(),
            |rodeo| {
                for &line in setup.lines() {
                    black_box(rodeo.get_or_intern(line));
                }
            },
            BatchSize::PerIteration,
        )
    });

    let mut setup = ThreadedRodeoFilledSetup::new(AhashRandomState::new());
    group.bench_function("get_or_intern (filled)", |b| {
        b.iter(|| {
            for &line in setup.lines() {
                black_box(setup.filled_rodeo_mut().get_or_intern(line));
            }
        })
    });

    let setup = ThreadedRodeoEmptySetup::new(AhashRandomState::new());
    group.bench_function("try_get_or_intern (empty)", |b| {
        b.iter_batched(
            || setup.empty_rodeo(),
            |rodeo| {
                for &line in setup.lines() {
                    black_box(rodeo.try_get_or_intern(line).unwrap());
                }
            },
            BatchSize::PerIteration,
        )
    });

    let mut setup = ThreadedRodeoFilledSetup::new(AhashRandomState::new());
    group.bench_function("try_get_or_intern (filled)", |b| {
        b.iter(|| {
            for &line in setup.lines() {
                black_box(setup.filled_rodeo_mut().try_get_or_intern(line).unwrap());
            }
        })
    });

    let setup = ThreadedRodeoEmptySetup::new(AhashRandomState::new());
    group.bench_function("get (empty)", |b| {
        b.iter_batched(
            || setup.empty_rodeo(),
            |rodeo| {
                for &line in setup.lines() {
                    black_box(rodeo.get(line));
                }
            },
            BatchSize::PerIteration,
        )
    });

    let setup = ThreadedRodeoFilledSetup::new(AhashRandomState::new());
    group.bench_function("get (filled)", |b| {
        b.iter_batched(
            || setup.filled_rodeo(),
            |rodeo| {
                for &line in setup.lines() {
                    black_box(rodeo.get(line));
                }
            },
            BatchSize::PerIteration,
        )
    });

    let setup = ThreadedRodeoFilledSetup::new(AhashRandomState::new());
    group.bench_function("resolve", |b| {
        b.iter_batched(
            || setup.filled_rodeo(),
            |rodeo| {
                for key in setup.keys() {
                    black_box(rodeo.resolve(key));
                }
            },
            BatchSize::PerIteration,
        )
    });

    let setup = ThreadedRodeoFilledSetup::new(AhashRandomState::new());
    group.bench_function("try_resolve", |b| {
        b.iter_batched(
            || setup.filled_rodeo(),
            |rodeo| {
                for key in setup.keys() {
                    black_box(rodeo.try_resolve(key).unwrap());
                }
            },
            BatchSize::PerIteration,
        )
    });

    group.finish();
}

fn rodeo_ahash_threaded(c: &mut Criterion) {
    let mut group = c.benchmark_group("ThreadedRodeo 24 Thread (ahash)");
    group.throughput(Throughput::Bytes(INPUT.len() as u64));

    group.bench_function("get_or_intern", |b| {
        b.iter_custom(|iters| {
            run_threaded_filled(
                |rodeo, _| {
                    for &line in setup::bench_lines() {
                        black_box(rodeo.get_or_intern(line));
                    }
                },
                NUM_THREADS,
                iters,
                AhashRandomState::new(),
            )
        })
    });

    group.bench_function("try_get_or_intern", |b| {
        b.iter_custom(|iters| {
            run_threaded_filled(
                |rodeo, _| {
                    for &line in setup::bench_lines() {
                        black_box(rodeo.try_get_or_intern(line).unwrap());
                    }
                },
                NUM_THREADS,
                iters,
                AhashRandomState::new(),
            )
        })
    });

    group.bench_function("get", |b| {
        b.iter_custom(|iters| {
            run_threaded_filled(
                |rodeo, _| {
                    for &line in setup::bench_lines() {
                        black_box(rodeo.get(line));
                    }
                },
                NUM_THREADS,
                iters,
                AhashRandomState::new(),
            )
        })
    });

    group.bench_function("resolve", |b| {
        b.iter_custom(|iters| {
            run_threaded_filled(
                |rodeo, keys| {
                    for key in keys {
                        black_box(rodeo.resolve(key));
                    }
                },
                NUM_THREADS,
                iters,
                AhashRandomState::new(),
            )
        })
    });

    group.bench_function("try_resolve", |b| {
        b.iter_custom(|iters| {
            run_threaded_filled(
                |rodeo, keys| {
                    for key in keys {
                        black_box(rodeo.try_resolve(key).unwrap());
                    }
                },
                NUM_THREADS,
                iters,
                AhashRandomState::new(),
            )
        })
    });

    group.finish();
}

fn rodeo_fxhash(c: &mut Criterion) {
    let mut group = c.benchmark_group("ThreadedRodeo 1 Thread (fxhash)");
    group.throughput(Throughput::Bytes(INPUT.len() as u64));

    let setup = ThreadedRodeoEmptySetup::new(FxBuildHasher::default());
    group.bench_function("get_or_intern (empty)", |b| {
        b.iter_batched(
            || setup.empty_rodeo(),
            |rodeo| {
                for &line in setup.lines() {
                    black_box(rodeo.get_or_intern(line));
                }
            },
            BatchSize::PerIteration,
        )
    });

    let mut setup = ThreadedRodeoFilledSetup::new(FxBuildHasher::default());
    group.bench_function("get_or_intern (filled)", |b| {
        b.iter(|| {
            for &line in setup.lines() {
                black_box(setup.filled_rodeo_mut().get_or_intern(line));
            }
        })
    });

    let setup = ThreadedRodeoEmptySetup::new(FxBuildHasher::default());
    group.bench_function("try_get_or_intern (empty)", |b| {
        b.iter_batched(
            || setup.empty_rodeo(),
            |rodeo| {
                for &line in setup.lines() {
                    black_box(rodeo.try_get_or_intern(line).unwrap());
                }
            },
            BatchSize::PerIteration,
        )
    });

    let mut setup = ThreadedRodeoFilledSetup::new(FxBuildHasher::default());
    group.bench_function("try_get_or_intern (filled)", |b| {
        b.iter(|| {
            for &line in setup.lines() {
                black_box(setup.filled_rodeo_mut().try_get_or_intern(line).unwrap());
            }
        })
    });

    let setup = ThreadedRodeoEmptySetup::new(FxBuildHasher::default());
    group.bench_function("get (empty)", |b| {
        b.iter_batched(
            || setup.empty_rodeo(),
            |rodeo| {
                for &line in setup.lines() {
                    black_box(rodeo.get(line));
                }
            },
            BatchSize::PerIteration,
        )
    });

    let setup = ThreadedRodeoFilledSetup::new(FxBuildHasher::default());
    group.bench_function("get (filled)", |b| {
        b.iter_batched(
            || setup.filled_rodeo(),
            |rodeo| {
                for &line in setup.lines() {
                    black_box(rodeo.get(line));
                }
            },
            BatchSize::PerIteration,
        )
    });

    let setup = ThreadedRodeoFilledSetup::new(FxBuildHasher::default());
    group.bench_function("resolve", |b| {
        b.iter_batched(
            || setup.filled_rodeo(),
            |rodeo| {
                for key in setup.keys() {
                    black_box(rodeo.resolve(key));
                }
            },
            BatchSize::PerIteration,
        )
    });

    let setup = ThreadedRodeoFilledSetup::new(FxBuildHasher::default());
    group.bench_function("try_resolve", |b| {
        b.iter_batched(
            || setup.filled_rodeo(),
            |rodeo| {
                for key in setup.keys() {
                    black_box(rodeo.try_resolve(key).unwrap());
                }
            },
            BatchSize::PerIteration,
        )
    });

    group.finish();
}

fn rodeo_fxhash_threaded(c: &mut Criterion) {
    let mut group = c.benchmark_group("ThreadedRodeo 24 Thread (fxhash)");
    group.throughput(Throughput::Bytes(INPUT.len() as u64));

    group.bench_function("get_or_intern", |b| {
        b.iter_custom(|iters| {
            run_threaded_filled(
                |rodeo, _| {
                    for &line in setup::bench_lines() {
                        black_box(rodeo.get_or_intern(line));
                    }
                },
                NUM_THREADS,
                iters,
                FxBuildHasher::default(),
            )
        })
    });

    group.bench_function("try_get_or_intern", |b| {
        b.iter_custom(|iters| {
            run_threaded_filled(
                |rodeo, _| {
                    for &line in setup::bench_lines() {
                        black_box(rodeo.try_get_or_intern(line).unwrap());
                    }
                },
                NUM_THREADS,
                iters,
                FxBuildHasher::default(),
            )
        })
    });

    group.bench_function("get", |b| {
        b.iter_custom(|iters| {
            run_threaded_filled(
                |rodeo, _| {
                    for &line in setup::bench_lines() {
                        black_box(rodeo.get(line));
                    }
                },
                NUM_THREADS,
                iters,
                FxBuildHasher::default(),
            )
        })
    });

    group.bench_function("resolve", |b| {
        b.iter_custom(|iters| {
            run_threaded_filled(
                |rodeo, keys| {
                    for key in keys {
                        black_box(rodeo.resolve(key));
                    }
                },
                NUM_THREADS,
                iters,
                FxBuildHasher::default(),
            )
        })
    });

    group.bench_function("try_resolve", |b| {
        b.iter_custom(|iters| {
            run_threaded_filled(
                |rodeo, keys| {
                    for key in keys {
                        black_box(rodeo.try_resolve(key).unwrap());
                    }
                },
                NUM_THREADS,
                iters,
                FxBuildHasher::default(),
            )
        })
    });

    group.finish();
}

pub struct ThreadedRodeoFilledSetup<S: BuildHasher + Clone> {
    lines: &'static [&'static str],
    rodeo: ThreadedRodeo<Spur, S>,
    keys: Vec<Spur>,
}

impl<S: BuildHasher + Clone> ThreadedRodeoFilledSetup<S> {
    pub fn new(hash_builder: S) -> Self {
        let lines = bench_lines();
        let rodeo = ThreadedRodeo::with_capacity_and_hasher(
            Capacity::for_strings(lines.len()),
            hash_builder,
        );
        let keys = lines
            .iter()
            .map(|&line| rodeo.get_or_intern(line))
            .collect::<Vec<_>>();

        Self { lines, rodeo, keys }
    }

    pub fn into_inner(self) -> ThreadedRodeo<Spur, S> {
        self.rodeo
    }

    pub fn lines(&self) -> &'static [&'static str] {
        self.lines
    }

    pub fn filled_rodeo(&self) -> &ThreadedRodeo<Spur, S> {
        &self.rodeo
    }

    pub fn filled_rodeo_mut(&mut self) -> &mut ThreadedRodeo<Spur, S> {
        &mut self.rodeo
    }

    pub fn keys(&self) -> &[Spur] {
        &self.keys
    }
}

pub struct ThreadedRodeoEmptySetup<S: BuildHasher + Clone> {
    lines: &'static [&'static str],
    build_hasher: S,
}

impl<S: BuildHasher + Clone> ThreadedRodeoEmptySetup<S> {
    pub fn new(build_hasher: S) -> Self {
        let lines = bench_lines();

        Self {
            lines,
            build_hasher,
        }
    }

    pub fn empty_rodeo(&self) -> ThreadedRodeo<Spur, S> {
        ThreadedRodeo::with_capacity_and_hasher(
            Capacity::for_strings(self.lines.len()),
            self.build_hasher.clone(),
        )
    }

    pub fn lines(&self) -> &'static [&'static str] {
        self.lines
    }
}

pub fn run_threaded_filled<F, S>(func: F, num_threads: usize, iters: u64, hash: S) -> Duration
where
    F: FnOnce(&ThreadedRodeo<Spur, S>, &[Spur]) + Send + 'static + Clone + Copy,
    S: BuildHasher + Clone + Send + Sync + 'static,
{
    let setup = ThreadedRodeoFilledSetup::new(hash);
    let keys = setup.keys().to_vec();
    let reader = Arc::new(setup.into_inner());
    let barrier = Arc::new(Barrier::new(num_threads));
    let mut threads = Vec::with_capacity(num_threads - 1);
    let running = Arc::new(AtomicBool::new(true));

    for _ in 0..num_threads - 1 {
        let barrier = barrier.clone();
        let reader = Arc::clone(&reader);
        let running = running.clone();
        let keys = keys.clone();

        threads.push(thread::spawn(move || {
            let reader: &ThreadedRodeo<Spur, S> = &reader;
            barrier.wait();
            while running.load(Ordering::Relaxed) {
                func(reader, &keys)
            }
        }));
    }

    let reader: &ThreadedRodeo<Spur, S> = &reader;
    barrier.wait();
    let start = Instant::now();
    for _ in 0..iters {
        func(reader, &keys);
    }
    let time = start.elapsed();

    running.store(false, Ordering::Relaxed);
    threads.into_iter().for_each(|x| x.join().unwrap());

    time
}

criterion_group!(
    benches,
    rodeo_std,
    rodeo_ahash,
    rodeo_fxhash,
    rodeo_std_threaded,
    rodeo_ahash_threaded,
    rodeo_fxhash_threaded
);
criterion_main!(benches);
