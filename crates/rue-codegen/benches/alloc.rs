use criterion::{Criterion, black_box, criterion_group, criterion_main};
use rue_codegen::compile_to_executable;
use rue_lexer::Lexer;

fn benchmark_factorial_allocations(c: &mut Criterion) {
    let factorial_source = r#"
fn factorial(n: i32) -> i32 {
    if n <= 1 {
        1
    } else {
        n * factorial(n - 1)
    }
}

fn main() -> i32 {
    factorial(10)
}
"#;

    c.bench_function("factorial_compile", |b| {
        b.iter(|| {
            let mut lexer = Lexer::new(factorial_source);
            let tokens = lexer.tokenize();
            let ast = rue_parser::parse(tokens).unwrap();
            let scope = rue_semantic::analyze_cst(&ast).unwrap();

            let result = compile_to_executable(&ast, &scope);
            let _ = black_box(result);
        });
    });
}

fn benchmark_large_program(c: &mut Criterion) {
    // Generate a large program to test pre-allocation effectiveness
    let mut large_program = String::from("fn main() -> i32 {\n");
    for i in 0..500 {
        large_program.push_str(&format!("    let x{i}: i32 = {i};\n"));
    }
    large_program.push_str("    x499\n}\n");

    c.bench_function("large_program_compile", |b| {
        b.iter(|| {
            let mut lexer = Lexer::new(&large_program);
            let tokens = lexer.tokenize();
            let ast = rue_parser::parse(tokens).unwrap();
            let scope = rue_semantic::analyze_cst(&ast).unwrap();

            let result = compile_to_executable(&ast, &scope);
            let _ = black_box(result);
        });
    });
}

fn benchmark_comparison_ops(c: &mut Criterion) {
    let comparison_source = r#"
fn main() -> bool {
    let a: i32 = 10;
    let b: i32 = 20;
    a < b
}
"#;

    c.bench_function("comparison_ops_compile", |b| {
        b.iter(|| {
            let mut lexer = Lexer::new(comparison_source);
            let tokens = lexer.tokenize();
            let ast = rue_parser::parse(tokens).unwrap();
            let scope = rue_semantic::analyze_cst(&ast).unwrap();

            let result = compile_to_executable(&ast, &scope);
            let _ = black_box(result);
        });
    });
}

criterion_group!(
    benches,
    benchmark_factorial_allocations,
    benchmark_large_program,
    benchmark_comparison_ops
);
criterion_main!(benches);
