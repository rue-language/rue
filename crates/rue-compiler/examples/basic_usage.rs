//! Example demonstrating basic usage of the rue compiler with Salsa 0.22

use rue_compiler::{RueDatabase, SourceFile, parse_file};
use salsa::Setter;

fn main() {
    // Create a Salsa database
    let mut db = RueDatabase::default();

    // Create a source file
    let file = SourceFile::new(
        &db,
        "main.rue".to_string(),
        r#"
fn main() {
    let x = 42;
    if x <= 1 {
        1
    } else {
        x * 2
    }
}
        "#
        .to_string(),
    );

    // Parse the file
    let result = parse_file(&db, file);

    // Check results
    match result {
        Ok(ast) => {
            println!("Successfully parsed file: {}", file.path(&db));
            println!("AST contains {} top-level items", ast.items.len());
        }
        Err(error) => {
            println!("Parse error: {error}");
        }
    }

    // Demonstrate incremental recompilation
    println!("\nUpdating file content...");
    file.set_text(&mut db).to(r#"
fn main() {
    99
}
    "#
    .to_string());

    // Parse again - Salsa will only recompute what's necessary
    let result2 = parse_file(&db, file);

    match result2 {
        Ok(ast) => {
            println!("Successfully re-parsed updated file");
            println!("AST contains {} top-level items", ast.items.len());
        }
        Err(error) => println!("Re-parse error: {error}"),
    }
}
