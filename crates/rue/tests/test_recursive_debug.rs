#[test]
fn test_recursive_factorial_debug() {
    use rue_compiler::{RueDatabase, SourceFile, compile_file};

    let db = RueDatabase::default();

    let file = SourceFile::new(
        &db,
        "test.rue".to_string(),
        r#"
fn factorial(n: i32) -> i32 {
    if n <= 1 {
        1
    } else {
        n * factorial(n - 1)
    }
}

fn main() -> i32 {
    factorial(3)
}
"#
        .to_string(),
    );

    match compile_file(&db, file) {
        Ok(executable) => {
            println!("\nGenerated {} bytes of executable", executable.len());
            // Dump hex of code section (starts at offset 0x78)
            println!("\nCode section:");
            for (i, chunk) in executable[0x78..].chunks(16).enumerate() {
                print!("{:04x}: ", i * 16);
                for byte in chunk {
                    print!("{byte:02x} ");
                }
                println!();
            }
        }
        Err(e) => {
            panic!("Compilation failed: {}", e.message);
        }
    }
}
