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
    (a * b) / gcd(a, b)
}

fn is_prime(n: i32) -> bool {
    if n <= 1 {
        false
    } else {
        let i: i32 = 2;
        while i * i <= n {
            if n % i == 0 {
                return false;
            }
            i = i + 1;
        }
        true
    }
}

fn nth_prime(n: i32) -> i32 {
    let count: i32 = 0;
    let candidate: i32 = 2;
    while count < n {
        if is_prime(candidate) {
            count = count + 1;
            if count == n {
                return candidate;
            }
        }
        candidate = candidate + 1;
    }
    0
}

fn collatz_length(n: i32) -> i32 {
    let steps: i32 = 0;
    let current: i32 = n;
    while current != 1 {
        if current % 2 == 0 {
            current = current / 2;
        } else {
            current = current * 3 + 1;
        }
        steps = steps + 1;
    }
    steps
}

fn sum_of_divisors(n: i32) -> i32 {
    let sum: i32 = 0;
    let i: i32 = 1;
    while i <= n / 2 {
        if n % i == 0 {
            sum = sum + i;
        }
        i = i + 1;
    }
    sum + n
}

fn is_perfect(n: i32) -> bool {
    sum_of_divisors(n) - n == n
}

fn factorial(n: i32) -> i32 {
    if n <= 1 {
        1
    } else {
        n * factorial(n - 1)
    }
}

fn power(base: i32, exp: i32) -> i32 {
    if exp == 0 {
        1
    } else if exp % 2 == 0 {
        let half: i32 = power(base, exp / 2);
        half * half
    } else {
        base * power(base, exp - 1)
    }
}

fn main() -> i32 {
    let a: i32 = gcd(48, 18);
    let b: i32 = lcm(12, 8);
    let c: i32 = nth_prime(10);
    let d: i32 = collatz_length(27);
    let e: i32 = sum_of_divisors(28);
    let f: i32 = factorial(6);
    let g: i32 = power(2, 10);
    
    a + b + c + d + e + f + g
}
"#;

fn bench_compilation_paths(c: &mut Criterion) {
    let mut group = c.benchmark_group("compilation");
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
    bench_compilation_paths,
    bench_memory_usage,
    bench_throughput
);
criterion_main!(benches);
