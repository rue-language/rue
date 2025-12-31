mod setup;

use criterion::{black_box, criterion_group, criterion_main, BatchSize, Criterion, Throughput};
use fxhash::FxBuildHasher;
use setup::{ReaderEmptySetup, ReaderFilledSetup, INPUT, NUM_THREADS};

fn reader_std(c: &mut Criterion) {
    use std::collections::hash_map::RandomState;

    let mut group = c.benchmark_group("RodeoReader 1 Thread (std)");
    group.throughput(Throughput::Bytes(INPUT.len() as u64));

    let setup = ReaderEmptySetup::new(RandomState::default());
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

    let setup = ReaderFilledSetup::new(RandomState::default());
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

    let setup = ReaderFilledSetup::new(RandomState::default());
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

    let setup = ReaderFilledSetup::new(RandomState::default());
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

    let setup = ReaderFilledSetup::new(RandomState::default());
    group.bench_function("resolve_unchecked", |b| {
        b.iter_batched(
            || setup.filled_rodeo(),
            |rodeo| {
                for key in setup.keys() {
                    unsafe { black_box(rodeo.resolve_unchecked(key)) };
                }
            },
            BatchSize::PerIteration,
        )
    });

    group.finish();
}

fn reader_std_threaded(c: &mut Criterion) {
    use std::collections::hash_map::RandomState;

    let mut group = c.benchmark_group("RodeoReader 24 Thread (std)");
    group.throughput(Throughput::Bytes(INPUT.len() as u64));

    group.bench_function("get", |b| {
        b.iter_custom(|iters| {
            setup::run_reader_filled(
                |reader, _| {
                    for &line in setup::bench_lines() {
                        black_box(reader.get(line));
                    }
                },
                RandomState::new(),
                NUM_THREADS,
                iters,
            )
        })
    });

    group.bench_function("resolve", |b| {
        b.iter_custom(|iters| {
            setup::run_reader_filled(
                |reader, keys| {
                    for key in keys {
                        black_box(reader.resolve(key));
                    }
                },
                RandomState::new(),
                NUM_THREADS,
                iters,
            )
        })
    });

    group.bench_function("try_resolve", |b| {
        b.iter_custom(|iters| {
            setup::run_reader_filled(
                |reader, keys| {
                    for key in keys {
                        black_box(reader.try_resolve(key).unwrap());
                    }
                },
                RandomState::new(),
                NUM_THREADS,
                iters,
            )
        })
    });

    group.bench_function("resolve_unchecked", |b| {
        b.iter_custom(|iters| {
            setup::run_reader_filled(
                |reader, keys| {
                    for key in keys {
                        unsafe { black_box(reader.resolve_unchecked(key)) };
                    }
                },
                RandomState::new(),
                NUM_THREADS,
                iters,
            )
        })
    });

    group.finish();
}

fn reader_ahash(c: &mut Criterion) {
    use ahash::RandomState;

    let mut group = c.benchmark_group("RodeoReader 1 Thread (ahash)");
    group.throughput(Throughput::Bytes(INPUT.len() as u64));

    let setup = ReaderEmptySetup::new(RandomState::new());
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

    let setup = ReaderFilledSetup::new(RandomState::new());
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

    let setup = ReaderFilledSetup::new(RandomState::new());
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

    let setup = ReaderFilledSetup::new(RandomState::new());
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

    let setup = ReaderFilledSetup::new(RandomState::new());
    group.bench_function("resolve_unchecked", |b| {
        b.iter_batched(
            || setup.filled_rodeo(),
            |rodeo| {
                for key in setup.keys() {
                    unsafe { black_box(rodeo.resolve_unchecked(key)) };
                }
            },
            BatchSize::PerIteration,
        )
    });

    group.finish();
}

fn reader_ahash_threaded(c: &mut Criterion) {
    use ahash::RandomState;

    let mut group = c.benchmark_group("RodeoReader 24 Thread (ahash)");
    group.throughput(Throughput::Bytes(INPUT.len() as u64));

    group.bench_function("get", |b| {
        b.iter_custom(|iters| {
            setup::run_reader_filled(
                |reader, _| {
                    for &line in setup::bench_lines() {
                        black_box(reader.get(line));
                    }
                },
                RandomState::new(),
                NUM_THREADS,
                iters,
            )
        })
    });

    group.bench_function("resolve", |b| {
        b.iter_custom(|iters| {
            setup::run_reader_filled(
                |reader, keys| {
                    for key in keys {
                        black_box(reader.resolve(key));
                    }
                },
                RandomState::new(),
                NUM_THREADS,
                iters,
            )
        })
    });

    group.bench_function("try_resolve", |b| {
        b.iter_custom(|iters| {
            setup::run_reader_filled(
                |reader, keys| {
                    for key in keys {
                        black_box(reader.try_resolve(key).unwrap());
                    }
                },
                RandomState::new(),
                NUM_THREADS,
                iters,
            )
        })
    });

    group.bench_function("resolve_unchecked", |b| {
        b.iter_custom(|iters| {
            setup::run_reader_filled(
                |reader, keys| {
                    for key in keys {
                        unsafe { black_box(reader.resolve_unchecked(key)) };
                    }
                },
                RandomState::new(),
                NUM_THREADS,
                iters,
            )
        })
    });

    group.finish();
}

fn reader_fxhash(c: &mut Criterion) {
    let mut group = c.benchmark_group("RodeoReader 1 Thread (fxhash)");
    group.throughput(Throughput::Bytes(INPUT.len() as u64));

    let setup = ReaderEmptySetup::new(FxBuildHasher::default());
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

    let setup = ReaderFilledSetup::new(FxBuildHasher::default());
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

    let setup = ReaderFilledSetup::new(FxBuildHasher::default());
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

    let setup = ReaderFilledSetup::new(FxBuildHasher::default());
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

    let setup = ReaderFilledSetup::new(FxBuildHasher::default());
    group.bench_function("resolve_unchecked", |b| {
        b.iter_batched(
            || setup.filled_rodeo(),
            |rodeo| {
                for key in setup.keys() {
                    unsafe { black_box(rodeo.resolve_unchecked(key)) };
                }
            },
            BatchSize::PerIteration,
        )
    });

    group.finish();
}

fn reader_fxhash_threaded(c: &mut Criterion) {
    let mut group = c.benchmark_group("RodeoReader 24 Thread (fxhash)");
    group.throughput(Throughput::Bytes(INPUT.len() as u64));

    group.bench_function("get", |b| {
        b.iter_custom(|iters| {
            setup::run_reader_filled(
                |reader, _| {
                    for &line in setup::bench_lines() {
                        black_box(reader.get(line));
                    }
                },
                FxBuildHasher::default(),
                NUM_THREADS,
                iters,
            )
        })
    });

    group.bench_function("resolve", |b| {
        b.iter_custom(|iters| {
            setup::run_reader_filled(
                |reader, keys| {
                    for key in keys {
                        black_box(reader.resolve(key));
                    }
                },
                FxBuildHasher::default(),
                NUM_THREADS,
                iters,
            )
        })
    });

    group.bench_function("try_resolve", |b| {
        b.iter_custom(|iters| {
            setup::run_reader_filled(
                |reader, keys| {
                    for key in keys {
                        black_box(reader.try_resolve(key).unwrap());
                    }
                },
                FxBuildHasher::default(),
                NUM_THREADS,
                iters,
            )
        })
    });

    group.bench_function("resolve_unchecked", |b| {
        b.iter_custom(|iters| {
            setup::run_reader_filled(
                |reader, keys| {
                    for key in keys {
                        unsafe { black_box(reader.resolve_unchecked(key)) };
                    }
                },
                FxBuildHasher::default(),
                NUM_THREADS,
                iters,
            )
        })
    });

    group.finish();
}

criterion_group!(
    benches,
    reader_std,
    reader_ahash,
    reader_fxhash,
    reader_std_threaded,
    reader_ahash_threaded,
    reader_fxhash_threaded
);
criterion_main!(benches);
