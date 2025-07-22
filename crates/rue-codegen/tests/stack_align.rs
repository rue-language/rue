#[test]
fn loops_and_recursion_dont_segfault() {
    let source = r#"
fn fac(n: i32) -> i32 {
    if n <= 1 { 1 } else { n * fac(n - 1) }
}
fn main() -> i32 {
    let i: i32 = 0;
    while i <= 3 {
        i = i + 1;
    };
    fac(5)
}
"#;
    // compile_hir_to_executable returns Vec<u8> ELF; we just need it not to panic
    let mut lexer = rue_lexer::Lexer::new(source);
    let tokens = lexer.tokenize().unwrap();
    let ast = rue_parser::parse(tokens).unwrap();
    let analysis = rue_semantic::analyze_cst(&ast).unwrap();
    rue_codegen::compile_hir_to_executable(&analysis.hir).unwrap();
}
