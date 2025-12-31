#![allow(dead_code)]

use lasso::{Capacity, Rodeo, RodeoReader, RodeoResolver, Spur};
use std::{
    collections::hash_map::RandomState,
    hash::BuildHasher,
    num::NonZeroUsize,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Barrier,
    },
    thread,
    time::{Duration, Instant},
};

pub const NUM_THREADS: usize = 24;

pub static INPUT: &str = include_str!("input.txt");

lazy_static::lazy_static! {
    pub static ref INPUT_LINES: Vec<&'static str> =
        INPUT.split_whitespace().collect::<Vec<&str>>();
}

pub fn bench_lines() -> &'static [&'static str] {
    &INPUT_LINES
}

pub struct RodeoEmptySetup<S: BuildHasher + Clone> {
    lines: &'static [&'static str],
    build_hasher: S,
}

impl<S: BuildHasher + Clone> RodeoEmptySetup<S> {
    pub fn new(build_hasher: S) -> Self {
        let lines = bench_lines();

        Self {
            lines,
            build_hasher,
        }
    }

    pub fn empty_rodeo(&self) -> Rodeo<Spur, S> {
        Rodeo::with_capacity_and_hasher(Capacity::default(), self.build_hasher.clone())
    }

    pub fn lines(&self) -> &'static [&'static str] {
        self.lines
    }
}

pub struct RodeoFilledSetup<S: BuildHasher + Clone> {
    lines: &'static [&'static str],
    rodeo: Rodeo<Spur, S>,
    keys: Vec<Spur>,
}

impl<S: BuildHasher + Clone> RodeoFilledSetup<S> {
    pub fn new(hash_builder: S) -> Self {
        let lines = bench_lines();
        let mut rodeo = Rodeo::with_capacity_and_hasher(
            Capacity::new(
                lines.len(),
                NonZeroUsize::new(lines.iter().map(|l| l.as_bytes().len()).sum()).unwrap(),
            ),
            hash_builder,
        );
        let keys = lines
            .iter()
            .map(|&line| rodeo.get_or_intern(line))
            .collect::<Vec<_>>();

        Self { lines, rodeo, keys }
    }

    pub fn lines(&self) -> &'static [&'static str] {
        self.lines
    }

    pub fn filled_rodeo(&self) -> &Rodeo<Spur, S> {
        &self.rodeo
    }

    pub fn filled_rodeo_mut(&mut self) -> &mut Rodeo<Spur, S> {
        &mut self.rodeo
    }

    pub fn keys(&self) -> &[Spur] {
        &self.keys
    }
}

pub struct ReaderEmptySetup<S: BuildHasher + Clone> {
    lines: &'static [&'static str],
    build_hasher: S,
}

impl<S: BuildHasher + Clone> ReaderEmptySetup<S> {
    pub fn new(build_hasher: S) -> Self {
        let lines = bench_lines();

        Self {
            lines,
            build_hasher,
        }
    }

    pub fn empty_rodeo(&self) -> RodeoReader<Spur, S> {
        Rodeo::with_capacity_and_hasher(Capacity::default(), self.build_hasher.clone())
            .into_reader()
    }

    pub fn lines(&self) -> &'static [&'static str] {
        self.lines
    }
}

pub struct ReaderFilledSetup<S: BuildHasher + Clone> {
    lines: &'static [&'static str],
    reader: RodeoReader<Spur, S>,
    keys: Vec<Spur>,
}

impl<S: BuildHasher + Clone> ReaderFilledSetup<S> {
    pub fn new(hash_builder: S) -> Self {
        let lines = bench_lines();
        let mut rodeo = Rodeo::with_capacity_and_hasher(
            Capacity::new(
                lines.len(),
                NonZeroUsize::new(lines.iter().map(|l| l.as_bytes().len()).sum()).unwrap(),
            ),
            hash_builder,
        );
        let keys = lines
            .iter()
            .map(|&line| rodeo.get_or_intern(line))
            .collect::<Vec<_>>();
        let reader = rodeo.into_reader();

        Self {
            lines,
            reader,
            keys,
        }
    }

    pub fn into_inner(self) -> RodeoReader<Spur, S> {
        self.reader
    }

    pub fn lines(&self) -> &'static [&'static str] {
        self.lines
    }

    pub fn filled_rodeo(&self) -> &RodeoReader<Spur, S> {
        &self.reader
    }

    pub fn filled_rodeo_mut(&mut self) -> &mut RodeoReader<Spur, S> {
        &mut self.reader
    }

    pub fn keys(&self) -> &[Spur] {
        &self.keys
    }
}

pub fn run_reader_filled<F, S>(func: F, hash: S, num_threads: usize, iters: u64) -> Duration
where
    F: FnOnce(&RodeoReader<Spur, S>, &[Spur]) + Send + 'static + Clone + Copy,
    S: 'static + BuildHasher + Clone + Send + Sync,
{
    let setup = ReaderFilledSetup::new(hash);
    let keys = setup.keys().to_vec();
    let reader = Arc::new(setup.into_inner());
    let barrier = Arc::new(Barrier::new(num_threads));
    let mut threads = Vec::with_capacity(num_threads - 1);
    let running = Arc::new(AtomicBool::new(true));

    for _ in 0..num_threads - 1 {
        let barrier = barrier.clone();
        let reader = reader.clone();
        let running = running.clone();
        let keys = keys.clone();

        threads.push(thread::spawn(move || {
            let reader: &RodeoReader<Spur, S> = &reader;
            barrier.wait();
            while running.load(Ordering::Relaxed) {
                func(reader, &keys)
            }
        }));
    }

    let reader: &RodeoReader<Spur, S> = &reader;
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

pub fn run_resolver_filled<F>(func: F, num_threads: usize, iters: u64) -> Duration
where
    F: FnOnce(&RodeoResolver<Spur>, &[Spur]) + Send + 'static + Clone + Copy,
{
    let setup = ResolverFilledSetup::new();
    let keys = setup.keys().to_vec();
    let reader = Arc::new(setup.into_inner());
    let barrier = Arc::new(Barrier::new(num_threads));
    let mut threads = Vec::with_capacity(num_threads - 1);
    let running = Arc::new(AtomicBool::new(true));

    for _ in 0..num_threads - 1 {
        let barrier = barrier.clone();
        let reader = reader.clone();
        let running = running.clone();
        let keys = keys.clone();

        threads.push(thread::spawn(move || {
            let reader: &RodeoResolver<Spur> = &reader;
            barrier.wait();
            while running.load(Ordering::Relaxed) {
                func(reader, &keys)
            }
        }));
    }

    let reader: &RodeoResolver<Spur> = &reader;
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

pub struct ResolverFilledSetup {
    lines: &'static [&'static str],
    resolver: RodeoResolver<Spur>,
    keys: Vec<Spur>,
}

impl ResolverFilledSetup {
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        let lines = bench_lines();
        let mut rodeo = Rodeo::with_capacity_and_hasher(
            Capacity::new(
                lines.len(),
                NonZeroUsize::new(lines.iter().map(|l| l.as_bytes().len()).sum()).unwrap(),
            ),
            RandomState::new(),
        );
        let keys = lines
            .iter()
            .map(|&line| rodeo.get_or_intern(line))
            .collect::<Vec<_>>();
        let resolver = rodeo.into_resolver();

        Self {
            lines,
            resolver,
            keys,
        }
    }

    pub fn into_inner(self) -> RodeoResolver<Spur> {
        self.resolver
    }

    pub fn lines(&self) -> &'static [&'static str] {
        self.lines
    }

    pub fn filled_rodeo(&self) -> &RodeoResolver<Spur> {
        &self.resolver
    }

    pub fn filled_rodeo_mut(&mut self) -> &mut RodeoResolver<Spur> {
        &mut self.resolver
    }

    pub fn keys(&self) -> &[Spur] {
        &self.keys
    }
}
