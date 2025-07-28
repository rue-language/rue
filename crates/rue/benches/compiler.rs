//! Performance benchmarks for the Rue compiler

use criterion::{Criterion, black_box, criterion_group, criterion_main};
use rue_compiler::{RueDatabase, SourceFile, compile_file};
use std::time::Duration;
use tempfile::NamedTempFile;

/// Helper to compile a source string
fn compile_source(source: &str) -> Result<Vec<u8>, String> {
    // Write source to temporary file
    let source_file = NamedTempFile::new().map_err(|e| e.to_string())?;
    std::fs::write(&source_file, source).map_err(|e| e.to_string())?;

    // Create database
    let db = RueDatabase::default();
    let file = SourceFile::new(
        &db,
        source_file.path().to_str().unwrap().to_string(),
        source.to_string(),
    );

    // Compile
    compile_file(&db, file)
        .map(|arc| (*arc).clone())
        .map_err(|e| e.message.clone())
}

/// Benchmark compilation of a simple program
fn bench_simple_program(c: &mut Criterion) {
    let source = r#"
fn main() -> i32 {
    42
}
"#;

    c.bench_function("simple_program", |b| {
        b.iter(|| {
            let result = compile_source(black_box(source));
            assert!(result.is_ok());
        });
    });
}

/// Benchmark compilation of a program with arithmetic
fn bench_arithmetic(c: &mut Criterion) {
    let source = r#"
fn main() -> i32 {
    let x: i32 = 10;
    let y: i32 = 20;
    let z: i32 = x + y * 2;
    z - 5
}
"#;

    c.bench_function("arithmetic", |b| {
        b.iter(|| {
            let result = compile_source(black_box(source));
            assert!(result.is_ok());
        });
    });
}

/// Benchmark compilation of a program with control flow
fn bench_control_flow(c: &mut Criterion) {
    let source = r#"
fn main() -> i32 {
    let x: i32 = 10;
    if x > 5 {
        x * 2
    } else {
        x / 2
    }
}
"#;

    c.bench_function("control_flow", |b| {
        b.iter(|| {
            let result = compile_source(black_box(source));
            assert!(result.is_ok());
        });
    });
}

/// Benchmark compilation of a program with loops
fn bench_loops(c: &mut Criterion) {
    let source = r#"
fn main() -> i32 {
    let sum: i32 = 0;
    let i: i32 = 0;
    while i < 10 {
        sum = sum + i;
        i = i + 1;
    };
    sum
}
"#;

    c.bench_function("loops", |b| {
        b.iter(|| {
            let result = compile_source(black_box(source));
            assert!(result.is_ok());
        });
    });
}

/// Benchmark compilation of a program with functions
fn bench_functions(c: &mut Criterion) {
    let source = r#"
fn add(a: i32, b: i32) -> i32 {
    a + b
}

fn multiply(a: i32, b: i32) -> i32 {
    a * b
}

fn main() -> i32 {
    let x: i32 = add(10, 20);
    let y: i32 = multiply(x, 3);
    y - 10
}
"#;

    c.bench_function("functions", |b| {
        b.iter(|| {
            let result = compile_source(black_box(source));
            assert!(result.is_ok());
        });
    });
}

/// Benchmark compilation of a large program
fn bench_large_program(c: &mut Criterion) {
    let source = r#"
fn factorial(n: i32) -> i32 {
    if n <= 1 {
        1
    } else {
        n * factorial(n - 1)
    }
}

fn fibonacci(n: i32) -> i32 {
    if n <= 1 {
        n
    } else {
        fibonacci(n - 1) + fibonacci(n - 2)
    }
}

fn gcd(a: i32, b: i32) -> i32 {
    if b == 0 {
        a
    } else {
        gcd(b, a % b)
    }
}

fn main() -> i32 {
    let f5: i32 = factorial(5);
    let fib10: i32 = fibonacci(10);
    let g: i32 = gcd(48, 18);
    
    f5 + fib10 + g
}
"#;

    c.bench_function("large_program", |b| {
        b.iter(|| {
            let result = compile_source(black_box(source));
            assert!(result.is_ok());
        });
    });
}

criterion_group! {
    name = benches;
    config = Criterion::default()
        .measurement_time(Duration::from_secs(10))
        .warm_up_time(Duration::from_secs(3));
    targets = bench_simple_program, bench_arithmetic, bench_control_flow,
              bench_loops, bench_functions, bench_large_program
}

criterion_main!(benches);
