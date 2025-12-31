mod setup;

use criterion::{black_box, criterion_group, criterion_main, BatchSize, Criterion, Throughput};
use setup::{ResolverFilledSetup, INPUT, NUM_THREADS};

fn resolver(c: &mut Criterion) {
    let mut group = c.benchmark_group("RodeoResolver 1 Thread");
    group.throughput(Throughput::Bytes(INPUT.len() as u64));

    let setup = ResolverFilledSetup::new();
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

    let setup = ResolverFilledSetup::new();
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

    let setup = ResolverFilledSetup::new();
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

fn resolver_threaded(c: &mut Criterion) {
    let mut group = c.benchmark_group("RodeoResolver 24 Thread");
    group.throughput(Throughput::Bytes(INPUT.len() as u64));

    group.bench_function("resolve", |b| {
        b.iter_custom(|iters| {
            setup::run_resolver_filled(
                |reader, keys| {
                    for key in keys {
                        black_box(reader.resolve(key));
                    }
                },
                NUM_THREADS,
                iters,
            )
        })
    });

    group.bench_function("try_resolve", |b| {
        b.iter_custom(|iters| {
            setup::run_resolver_filled(
                |reader, keys| {
                    for key in keys {
                        black_box(reader.try_resolve(key).unwrap());
                    }
                },
                NUM_THREADS,
                iters,
            )
        })
    });

    group.bench_function("resolve_unchecked", |b| {
        b.iter_custom(|iters| {
            setup::run_resolver_filled(
                |reader, keys| {
                    for key in keys {
                        unsafe { black_box(reader.resolve_unchecked(key)) };
                    }
                },
                NUM_THREADS,
                iters,
            )
        })
    });

    group.finish();
}

criterion_group!(benches, resolver, resolver_threaded);
criterion_main!(benches);
