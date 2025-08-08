use criterion::{BenchmarkId, Criterion, black_box, criterion_group, criterion_main};
use rue_compiler::compile_with_ast;
use std::time::Duration;

const SMALL_PROGRAM: &str = r#"
fn main() -> i32 {
    42
}
"#;

const MEDIUM_PROGRAM: &str = r#"
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

fn main() -> i32 {
    let x: i32 = factorial(5);
    let y: i32 = fibonacci(10);
    x + y
}
"#;

const LARGE_PROGRAM: &str = r#"
fn gcd(a: i32, b: i32) -> i32 {
    if b == 0 {
        a
    } else {
        gcd(b, a % b)
    }
}

fn lcm(a: i32, b: i32) -> i32 {
    a * b / gcd(a, b)
}

fn is_prime(n: i32) -> i32 {
    if n <= 1 {
        0
    } else {
        is_prime_helper(n, 2)
    }
}

fn is_prime_helper(n: i32, div: i32) -> i32 {
    if div * div > n {
        1
    } else {
        if n % div == 0 {
            0
        } else {
            is_prime_helper(n, div + 1)
        }
    }
}

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

fn ackermann(m: i32, n: i32) -> i32 {
    if m == 0 {
        n + 1
    } else {
        if n == 0 {
            ackermann(m - 1, 1)
        } else {
            ackermann(m - 1, ackermann(m, n - 1))
        }
    }
}

fn collatz(n: i32) -> i32 {
    if n == 1 {
        0
    } else {
        if n % 2 == 0 {
            1 + collatz(n / 2)
        } else {
            1 + collatz(3 * n + 1)
        }
    }
}

fn sum_of_divisors(n: i32) -> i32 {
    sum_of_divisors_helper(n, 1, 0)
}

fn sum_of_divisors_helper(n: i32, div: i32, acc: i32) -> i32 {
    if div >= n {
        acc
    } else {
        if n % div == 0 {
            sum_of_divisors_helper(n, div + 1, acc + div)
        } else {
            sum_of_divisors_helper(n, div + 1, acc)
        }
    }
}

fn main() -> i32 {
    let a: i32 = factorial(5);
    let b: i32 = fibonacci(10);
    let c: i32 = gcd(48, 18);
    let d: i32 = lcm(12, 18);
    let e: i32 = is_prime(17);
    let f: i32 = ackermann(2, 2);
    let g: i32 = collatz(27);
    let h: i32 = sum_of_divisors(28);
    
    a + b + c + d + e + f + g + h
}
"#;

fn bench_compilation(c: &mut Criterion) {
    let mut group = c.benchmark_group("compilation");
    group.warm_up_time(Duration::from_secs(1));
    group.measurement_time(Duration::from_secs(10));

    for (name, source) in &[
        ("small", SMALL_PROGRAM),
        ("medium", MEDIUM_PROGRAM),
        ("large", LARGE_PROGRAM),
    ] {
        group.bench_with_input(BenchmarkId::new("compile", name), source, |b, source| {
            b.iter(|| {
                let result = compile_with_ast(black_box(*source));
                black_box(result)
            });
        });
    }

    group.finish();
}

fn bench_memory_usage(c: &mut Criterion) {
    let mut group = c.benchmark_group("memory_usage");
    group.measurement_time(Duration::from_secs(5));

    // Memory usage benchmarks require custom measurement
    // We'll measure peak allocations during compilation

    group.bench_function("compile_memory", |b| {
        b.iter(|| {
            // Compile large program
            let result = compile_with_ast(black_box(LARGE_PROGRAM));
            black_box(result)
        });
    });

    group.finish();
}

fn bench_throughput(c: &mut Criterion) {
    let mut group = c.benchmark_group("throughput");
    group.measurement_time(Duration::from_secs(10));

    // Create a very large program by repeating functions
    let mut very_large = String::new();
    for i in 0..100 {
        very_large.push_str(&format!(
            r#"
fn function_{}(x: i32) -> i32 {{
    if x > 0 {{
        x * 2 + function_{}(x - 1)
    }} else {{
        1
    }}
}}
"#,
            i,
            (i + 99) % 100
        ));
    }
    very_large.push_str(
        r#"
fn main() -> i32 {
    function_0(10)
}
"#,
    );

    group.throughput(criterion::Throughput::Bytes(very_large.len() as u64));

    group.bench_function("compile_throughput", |b| {
        b.iter(|| {
            let result = compile_with_ast(black_box(&very_large));
            black_box(result)
        });
    });

    group.finish();
}

criterion_group!(
    benches,
    bench_compilation,
    bench_throughput,
    bench_memory_usage
);
criterion_main!(benches);
