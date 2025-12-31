mod setup;

use criterion::{black_box, criterion_group, criterion_main, BatchSize, Criterion, Throughput};
use setup::{RodeoEmptySetup, RodeoFilledSetup, INPUT};

fn rodeo_std(c: &mut Criterion) {
    use std::collections::hash_map::RandomState;

    let mut group = c.benchmark_group("Rodeo (std)");
    group.throughput(Throughput::Bytes(INPUT.len() as u64));

    let setup = RodeoEmptySetup::new(RandomState::default());
    group.bench_function("get_or_intern (empty)", |b| {
        b.iter_batched(
            || setup.empty_rodeo(),
            |mut rodeo| {
                for &line in setup.lines() {
                    black_box(rodeo.get_or_intern(line));
                }
            },
            BatchSize::PerIteration,
        )
    });

    let mut setup = RodeoFilledSetup::new(RandomState::default());
    group.bench_function("get_or_intern (filled)", |b| {
        b.iter(|| {
            for &line in setup.lines() {
                black_box(setup.filled_rodeo_mut().get_or_intern(line));
            }
        })
    });

    let setup = RodeoEmptySetup::new(RandomState::default());
    group.bench_function("try_get_or_intern (empty)", |b| {
        b.iter_batched(
            || setup.empty_rodeo(),
            |mut rodeo| {
                for &line in setup.lines() {
                    black_box(rodeo.try_get_or_intern(line).unwrap());
                }
            },
            BatchSize::PerIteration,
        )
    });

    let mut setup = RodeoFilledSetup::new(RandomState::default());
    group.bench_function("try_get_or_intern (filled)", |b| {
        b.iter(|| {
            for &line in setup.lines() {
                black_box(setup.filled_rodeo_mut().try_get_or_intern(line).unwrap());
            }
        })
    });

    let setup = RodeoEmptySetup::new(RandomState::default());
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

    let setup = RodeoFilledSetup::new(RandomState::default());
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

    let setup = RodeoFilledSetup::new(RandomState::default());
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

    let setup = RodeoFilledSetup::new(RandomState::default());
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

    let setup = RodeoFilledSetup::new(RandomState::default());
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

fn rodeo_ahash(c: &mut Criterion) {
    use ahash::RandomState;

    let mut group = c.benchmark_group("Rodeo (ahash)");
    group.throughput(Throughput::Bytes(INPUT.len() as u64));

    let setup = RodeoEmptySetup::new(RandomState::new());
    group.bench_function("get_or_intern (empty)", |b| {
        b.iter_batched(
            || setup.empty_rodeo(),
            |mut rodeo| {
                for &line in setup.lines() {
                    black_box(rodeo.get_or_intern(line));
                }
            },
            BatchSize::PerIteration,
        )
    });

    let mut setup = RodeoFilledSetup::new(RandomState::new());
    group.bench_function("get_or_intern (filled)", |b| {
        b.iter(|| {
            for &line in setup.lines() {
                black_box(setup.filled_rodeo_mut().get_or_intern(line));
            }
        })
    });

    let setup = RodeoEmptySetup::new(RandomState::new());
    group.bench_function("try_get_or_intern (empty)", |b| {
        b.iter_batched(
            || setup.empty_rodeo(),
            |mut rodeo| {
                for &line in setup.lines() {
                    black_box(rodeo.try_get_or_intern(line).unwrap());
                }
            },
            BatchSize::PerIteration,
        )
    });

    let mut setup = RodeoFilledSetup::new(RandomState::new());
    group.bench_function("try_get_or_intern (filled)", |b| {
        b.iter(|| {
            for &line in setup.lines() {
                black_box(setup.filled_rodeo_mut().try_get_or_intern(line).unwrap());
            }
        })
    });

    let setup = RodeoEmptySetup::new(RandomState::new());
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

    let setup = RodeoFilledSetup::new(RandomState::new());
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

    let setup = RodeoFilledSetup::new(RandomState::new());
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

    let setup = RodeoFilledSetup::new(RandomState::new());
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

    let setup = RodeoFilledSetup::new(RandomState::new());
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

fn rodeo_fxhash(c: &mut Criterion) {
    use fxhash::FxBuildHasher;

    let mut group = c.benchmark_group("Rodeo (fxhash)");
    group.throughput(Throughput::Bytes(INPUT.len() as u64));

    let setup = RodeoEmptySetup::new(FxBuildHasher::default());
    group.bench_function("get_or_intern (empty)", |b| {
        b.iter_batched(
            || setup.empty_rodeo(),
            |mut rodeo| {
                for &line in setup.lines() {
                    black_box(rodeo.get_or_intern(line));
                }
            },
            BatchSize::PerIteration,
        )
    });

    let mut setup = RodeoFilledSetup::new(FxBuildHasher::default());
    group.bench_function("get_or_intern (filled)", |b| {
        b.iter(|| {
            for &line in setup.lines() {
                black_box(setup.filled_rodeo_mut().get_or_intern(line));
            }
        })
    });

    let setup = RodeoEmptySetup::new(FxBuildHasher::default());
    group.bench_function("try_get_or_intern (empty)", |b| {
        b.iter_batched(
            || setup.empty_rodeo(),
            |mut rodeo| {
                for &line in setup.lines() {
                    black_box(rodeo.try_get_or_intern(line).unwrap());
                }
            },
            BatchSize::PerIteration,
        )
    });

    let mut setup = RodeoFilledSetup::new(FxBuildHasher::default());
    group.bench_function("try_get_or_intern (filled)", |b| {
        b.iter(|| {
            for &line in setup.lines() {
                black_box(setup.filled_rodeo_mut().try_get_or_intern(line).unwrap());
            }
        })
    });

    let setup = RodeoEmptySetup::new(FxBuildHasher::default());
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

    let setup = RodeoFilledSetup::new(FxBuildHasher::default());
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

    let setup = RodeoFilledSetup::new(FxBuildHasher::default());
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

    let setup = RodeoFilledSetup::new(FxBuildHasher::default());
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

    let setup = RodeoFilledSetup::new(FxBuildHasher::default());
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

criterion_group!(benches, rodeo_std, rodeo_ahash, rodeo_fxhash);
criterion_main!(benches);
