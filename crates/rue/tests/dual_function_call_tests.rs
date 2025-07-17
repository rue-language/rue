use std::fs;
use std::path::PathBuf;
use std::process::Command;

/// Helper function to compile a rue source string to executable
fn compile_rue_source(source: &str, output_name: &str) -> PathBuf {
    let temp_dir = std::env::temp_dir();
    let source_path = temp_dir.join(format!("{output_name}.rue"));
    let executable_path = temp_dir.join(output_name);

    // Write source to temporary file
    fs::write(&source_path, source).expect("Failed to write source file");

    // Find project root
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let manifest_path = PathBuf::from(manifest_dir);
    let project_root = manifest_path.parent().unwrap().parent().unwrap();

    // Compile using rue compiler
    let compile_output = Command::new("cargo")
        .args(["run", "-p", "rue", "--"])
        .arg(&source_path)
        .current_dir(project_root)
        .output()
        .expect("Failed to execute rue compiler");

    if !compile_output.status.success() {
        panic!(
            "Compilation failed for {}:\nstdout: {}\nstderr: {}",
            output_name,
            String::from_utf8_lossy(&compile_output.stdout),
            String::from_utf8_lossy(&compile_output.stderr)
        );
    }

    // Clean up source file
    fs::remove_file(&source_path).expect("Failed to remove source file");

    executable_path
}

/// Test to ensure dual recursive function calls work correctly
/// This is a regression test for the fibonacci calculation bug
#[test]
fn test_dual_recursive_calls_separated() {
    let source = r#"fn fibonacci(n: i32) -> i32 {
    if n <= 1 {
        n
    } else {
        let left: i32 = fibonacci(n - 1);
        let right: i32 = fibonacci(n - 2);
        left + right
    }
}

fn main() -> i32 {
    fibonacci(5)
}"#;

    let executable_path = compile_rue_source(source, "test_fib_separated");

    // Run and check exit code
    let output = Command::new(&executable_path)
        .output()
        .expect("Failed to execute compiled program");

    let exit_code = output.status.code().unwrap_or(-1);
    assert_eq!(exit_code, 5, "fibonacci(5) should return 5");

    // Clean up
    fs::remove_file(&executable_path).expect("Failed to clean up");
}

/// Test dual function calls with simple functions (non-recursive)
#[test]
fn test_dual_function_calls_simple() {
    let source = r#"fn get_three() -> i32 {
    3
}

fn get_five() -> i32 {
    5
}

fn main() -> i32 {
    get_three() + get_five()
}"#;

    let executable_path = compile_rue_source(source, "test_dual_simple");

    // Run and check exit code
    let output = Command::new(&executable_path)
        .output()
        .expect("Failed to execute compiled program");

    let exit_code = output.status.code().unwrap_or(-1);
    assert_eq!(exit_code, 8, "get_three() + get_five() should return 8");

    // Clean up
    fs::remove_file(&executable_path).expect("Failed to clean up");
}
