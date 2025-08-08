use criterion::{Criterion, black_box, criterion_group, criterion_main};
use rue_compiler::pipeline::{compile_hir_via_mir_to_assembly, compile_hir2_via_mir_to_assembly};
use rue_parser::parse_with_recovery;
use rue_semantic::{analyze_cst, analyze_cst_v2, scope_to_type_context};

fn benchmark_hir_vs_hir2(c: &mut Criterion) {
    let sources = vec![
        ("simple", "fn main() -> i32 { 42 }"),
        ("arithmetic", "fn main() -> i32 { (1 + 2) * (3 + 4) }"),
        (
            "variables",
            r#"
            fn main() -> i32 {
                let x = 10;
                let y = 20;
                let z = x + y;
                z * 2
            }
        "#,
        ),
        (
            "function_call",
            r#"
            fn add(a: i32, b: i32) -> i32 {
                a + b
            }
            
            fn main() -> i32 {
                add(5, 10)
            }
        "#,
        ),
        (
            "nested_calls",
            r#"
            fn add(a: i32, b: i32) -> i32 {
                a + b
            }
            
            fn mul(a: i32, b: i32) -> i32 {
                a * b
            }
            
            fn main() -> i32 {
                mul(add(2, 3), add(4, 5))
            }
        "#,
        ),
        (
            "complex_expression",
            r#"
            fn main() -> i32 {
                let a = 5;
                let b = 10;
                let c = 15;
                let d = (a + b) * c;
                let e = d / (a + 1);
                let f = e - (b * 2);
                f + (c / 3)
            }
        "#,
        ),
    ];

    // Load factorial source if available
    let factorial_source = std::fs::read_to_string("samples/factorial.rue").ok();
    let mut all_sources = sources;
    if let Some(factorial) = factorial_source.as_ref() {
        all_sources.push(("factorial", factorial.as_str()));
    }

    for (name, source) in all_sources {
        let mut group = c.benchmark_group(format!("hir_{}", name));

        // Benchmark HIR path (tree-based)
        group.bench_function("tree_hir", |b| {
            b.iter(|| {
                let cst = parse_with_recovery(black_box(source), "bench.rue").unwrap();
                let analysis = analyze_cst(&cst).unwrap();
                compile_hir_via_mir_to_assembly(&analysis, false).unwrap()
            });
        });

        // Benchmark HIR2 path (instruction-based)
        group.bench_function("instruction_hir2", |b| {
            b.iter(|| {
                let cst = parse_with_recovery(black_box(source), "bench.rue").unwrap();
                let hir2 = analyze_cst_v2(&cst).unwrap();
                let analysis = analyze_cst(&cst).unwrap();
                let ctx = scope_to_type_context(&analysis.scope);
                compile_hir2_via_mir_to_assembly(&hir2, ctx, false).unwrap()
            });
        });

        group.finish();
    }
}

/// Benchmark memory allocation patterns
fn benchmark_memory_patterns(c: &mut Criterion) {
    let mut group = c.benchmark_group("memory_patterns");

    let complex_source = r#"
        fn fibonacci(n: i32) -> i32 {
            if n <= 1 {
                n
            } else {
                fibonacci(n - 1) + fibonacci(n - 2)
            }
        }
        
        fn factorial(n: i32) -> i32 {
            if n <= 1 {
                1
            } else {
                n * factorial(n - 1)
            }
        }
        
        fn main() -> i32 {
            let a = fibonacci(10);
            let b = factorial(5);
            a + b
        }
    "#;

    // Measure HIR tree allocation
    group.bench_function("hir_tree_alloc", |b| {
        b.iter(|| {
            let cst = parse_with_recovery(black_box(complex_source), "bench.rue").unwrap();
            let analysis = analyze_cst(&cst).unwrap();
            black_box(analysis);
        });
    });

    // Measure HIR2 instruction allocation
    group.bench_function("hir2_instruction_alloc", |b| {
        b.iter(|| {
            let cst = parse_with_recovery(black_box(complex_source), "bench.rue").unwrap();
            let hir2 = analyze_cst_v2(&cst).unwrap();
            black_box(hir2);
        });
    });

    group.finish();
}

/// Benchmark compilation throughput (instructions per second)
fn benchmark_throughput(c: &mut Criterion) {
    let mut group = c.benchmark_group("throughput");
    group.throughput(criterion::Throughput::Elements(1));

    // Generate a large source file with many simple functions
    let mut large_source = String::new();
    for i in 0..100 {
        large_source.push_str(&format!(
            "fn func_{}(x: i32, y: i32) -> i32 {{ x + y + {} }}\n",
            i, i
        ));
    }
    large_source.push_str("fn main() -> i32 { func_0(1, 2) }");

    group.bench_function("hir_throughput", |b| {
        b.iter(|| {
            let cst = parse_with_recovery(black_box(&large_source), "bench.rue").unwrap();
            let analysis = analyze_cst(&cst).unwrap();
            compile_hir_via_mir_to_assembly(&analysis, false).unwrap()
        });
    });

    group.bench_function("hir2_throughput", |b| {
        b.iter(|| {
            let cst = parse_with_recovery(black_box(&large_source), "bench.rue").unwrap();
            let hir2 = analyze_cst_v2(&cst).unwrap();
            let analysis = analyze_cst(&cst).unwrap();
            let ctx = scope_to_type_context(&analysis.scope);
            compile_hir2_via_mir_to_assembly(&hir2, ctx, false).unwrap()
        });
    });

    group.finish();
}

criterion_group!(
    benches,
    benchmark_hir_vs_hir2,
    benchmark_memory_patterns,
    benchmark_throughput
);
criterion_main!(benches);
