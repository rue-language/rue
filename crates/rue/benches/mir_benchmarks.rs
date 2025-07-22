//! Performance benchmarks comparing direct HIR compilation vs MIR compilation

use criterion::{Criterion, black_box, criterion_group, criterion_main};
use rue_compiler::{RueDatabase, SourceFile, compile_file, compile_file_via_mir};
use std::time::Duration;
use tempfile::NamedTempFile;

/// Helper to compile a source string
fn compile_source(source: &str, use_mir: bool) -> Result<Vec<u8>, String> {
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
    if use_mir {
        compile_file_via_mir(&db, file)
            .map(|arc| (*arc).clone())
            .map_err(|e| e.message.clone())
    } else {
        compile_file(&db, file)
            .map(|arc| (*arc).clone())
            .map_err(|e| e.message.clone())
    }
}

/// Benchmark compilation of a simple program
fn bench_simple_program(c: &mut Criterion) {
    let source = r#"
fn main() -> i32 {
    42
}
"#;

    let mut group = c.benchmark_group("simple_program");

    group.bench_function("direct_hir", |b| {
        b.iter(|| {
            let result = compile_source(black_box(source), false);
            assert!(result.is_ok());
        });
    });

    group.bench_function("via_mir", |b| {
        b.iter(|| {
            let result = compile_source(black_box(source), true);
            assert!(result.is_ok());
        });
    });

    group.finish();
}

/// Benchmark compilation of arithmetic-heavy program
fn bench_arithmetic_program(c: &mut Criterion) {
    let source = r#"
fn main() -> i32 {
    let a: i32 = 10;
    let b: i32 = 20;
    let c: i32 = a + b;
    let d: i32 = c * 2;
    let e: i32 = d - 5;
    let f: i32 = e / 3;
    f + a + b + c + d + e
}
"#;

    let mut group = c.benchmark_group("arithmetic_program");

    group.bench_function("direct_hir", |b| {
        b.iter(|| {
            let result = compile_source(black_box(source), false);
            assert!(result.is_ok());
        });
    });

    group.bench_function("via_mir", |b| {
        b.iter(|| {
            let result = compile_source(black_box(source), true);
            assert!(result.is_ok());
        });
    });

    group.finish();
}

/// Benchmark compilation with optimization opportunities
fn bench_optimization_opportunities(c: &mut Criterion) {
    // Program with constants that can be propagated and folded
    let source = r#"
fn main() -> i32 {
    let x: i32 = 10;
    let y: i32 = 20;
    let a: i32 = x + y;  // Should fold to 30
    let b: i32 = x + y;  // CSE opportunity - same as a
    let c: i32 = a * 2;  // Should fold to 60
    let dead: i32 = 999; // Dead code - never used
    a + b + c            // Should optimize well
}
"#;

    let mut group = c.benchmark_group("optimization_opportunities");

    group.bench_function("direct_hir", |b| {
        b.iter(|| {
            let result = compile_source(black_box(source), false);
            assert!(result.is_ok());
        });
    });

    group.bench_function("via_mir", |b| {
        b.iter(|| {
            let result = compile_source(black_box(source), true);
            assert!(result.is_ok());
        });
    });

    group.finish();
}

/// Benchmark compilation of control flow heavy program
fn bench_control_flow_program(c: &mut Criterion) {
    let source = r#"
fn max3(a: i32, b: i32, c: i32) -> i32 {
    if a > b {
        if a > c { a } else { c }
    } else {
        if b > c { b } else { c }
    }
}

fn main() -> i32 {
    max3(10, 20, 15)
}
"#;

    let mut group = c.benchmark_group("control_flow_program");

    group.bench_function("direct_hir", |b| {
        b.iter(|| {
            let result = compile_source(black_box(source), false);
            assert!(result.is_ok());
        });
    });

    group.bench_function("via_mir", |b| {
        b.iter(|| {
            let result = compile_source(black_box(source), true);
            assert!(result.is_ok());
        });
    });

    group.finish();
}

/// Benchmark compilation of recursive program
fn bench_recursive_program(c: &mut Criterion) {
    let source = r#"
fn factorial(n: i32) -> i32 {
    if n <= 1 {
        1
    } else {
        n * factorial(n - 1)
    }
}

fn main() -> i32 {
    factorial(5)
}
"#;

    let mut group = c.benchmark_group("recursive_program");

    group.bench_function("direct_hir", |b| {
        b.iter(|| {
            let result = compile_source(black_box(source), false);
            assert!(result.is_ok());
        });
    });

    group.bench_function("via_mir", |b| {
        b.iter(|| {
            let result = compile_source(black_box(source), true);
            assert!(result.is_ok());
        });
    });

    group.finish();
}

/// Benchmark compilation of loop-heavy program
fn bench_loop_program(c: &mut Criterion) {
    let source = r#"
fn sum_to_n(n: i32) -> i32 {
    let sum: i32 = 0;
    let i: i32 = 1;
    while i <= n {
        sum = sum + i;
        i = i + 1;
    };
    sum
}

fn main() -> i32 {
    sum_to_n(10)
}
"#;

    let mut group = c.benchmark_group("loop_program");

    group.bench_function("direct_hir", |b| {
        b.iter(|| {
            let result = compile_source(black_box(source), false);
            assert!(result.is_ok());
        });
    });

    group.bench_function("via_mir", |b| {
        b.iter(|| {
            let result = compile_source(black_box(source), true);
            assert!(result.is_ok());
        });
    });

    group.finish();
}

/// Benchmark a larger, more realistic program
fn bench_larger_program(c: &mut Criterion) {
    let source = r#"
fn gcd(a: i32, b: i32) -> i32 {
    if b == 0 {
        a
    } else {
        gcd(b, a % b)
    }
}

fn lcm(a: i32, b: i32) -> i32 {
    (a * b) / gcd(a, b)
}

fn fibonacci(n: i32) -> i32 {
    if n <= 1 {
        n
    } else {
        fibonacci(n - 1) + fibonacci(n - 2)
    }
}

fn is_prime(n: i32) -> bool {
    if n <= 1 {
        false
    } else {
        let i: i32 = 2;
        while i * i <= n {
            if n % i == 0 {
                return false;
            };
            i = i + 1;
        };
        true
    }
}

fn main() -> i32 {
    let a: i32 = gcd(48, 18);
    let b: i32 = lcm(12, 8);
    let c: i32 = fibonacci(7);
    let d: i32 = if is_prime(17) { 1 } else { 0 };
    a + b + c + d
}
"#;

    let mut group = c.benchmark_group("larger_program");
    group.measurement_time(Duration::from_secs(10));

    group.bench_function("direct_hir", |b| {
        b.iter(|| {
            let result = compile_source(black_box(source), false);
            assert!(result.is_ok());
        });
    });

    group.bench_function("via_mir", |b| {
        b.iter(|| {
            let result = compile_source(black_box(source), true);
            assert!(result.is_ok());
        });
    });

    group.finish();
}

criterion_group!(
    benches,
    bench_simple_program,
    bench_arithmetic_program,
    bench_optimization_opportunities,
    bench_control_flow_program,
    bench_recursive_program,
    bench_loop_program,
    bench_larger_program
);
criterion_main!(benches);
