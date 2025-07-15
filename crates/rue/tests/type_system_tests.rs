use std::fs;
use std::path::Path;
use std::process::Command;

/// Get the project root directory, compatible with both Cargo and Buck2
fn get_project_root() -> &'static Path {
    // Try to use CARGO_MANIFEST_DIR if available
    if let Ok(manifest_dir) = std::env::var("CARGO_MANIFEST_DIR") {
        let manifest_path = Path::new(&manifest_dir);

        // If CARGO_MANIFEST_DIR is "." (Buck2 case), resolve to absolute path
        let manifest_path = if manifest_path == Path::new(".") {
            std::env::current_dir().expect("Failed to get current directory")
        } else {
            manifest_path.to_path_buf()
        };

        // For Cargo: navigate up from crates/rue to project root
        // For Buck2: current dir should already be project root
        let project_root = if manifest_path.ends_with("crates/rue") {
            manifest_path
                .parent()
                .unwrap()
                .parent()
                .unwrap()
                .to_path_buf()
        } else {
            // Buck2 case or already at project root
            manifest_path
        };

        return Box::leak(project_root.into_boxed_path());
    }

    // Fallback for Buck2: find project root by looking for Cargo.toml
    let current_dir = std::env::current_dir().expect("Failed to get current directory");
    let mut dir = current_dir.as_path();

    loop {
        if dir.join("Cargo.toml").exists() && dir.join("crates").exists() {
            return Box::leak(dir.to_path_buf().into_boxed_path());
        }

        if let Some(parent) = dir.parent() {
            dir = parent;
        } else {
            panic!("Could not find project root directory");
        }
    }
}

/// Test that a rue program compiles successfully
fn test_compiles(name: &str, program: &str) {
    let project_root = get_project_root();
    let test_file = project_root.join(format!("test_type_system_temp_{name}.rue"));
    let executable_path = project_root.join(format!("test_type_system_temp_{name}"));

    // Write the test program
    fs::write(&test_file, program).expect("Failed to write test file");

    // Clean up any existing executable
    if executable_path.exists() {
        fs::remove_file(&executable_path).expect("Failed to remove existing executable");
    }

    // Compile the rue program
    let compile_output = if std::env::var("CARGO_MANIFEST_DIR").is_err() {
        // Buck2 build environment
        Command::new("buck2")
            .args(["run", "//crates/rue:rue", "--"])
            .arg(&test_file)
            .current_dir(project_root)
            .output()
            .expect("Failed to execute rue compiler via Buck2")
    } else {
        // Cargo build environment
        Command::new("cargo")
            .args(["run", "-p", "rue", "--"])
            .arg(&test_file)
            .current_dir(project_root)
            .output()
            .expect("Failed to execute rue compiler via Cargo")
    };

    // Clean up test file
    fs::remove_file(&test_file).expect("Failed to remove test file");

    assert!(
        compile_output.status.success(),
        "Compilation failed for test '{}'\nProgram:\n{}\nstdout: {}\nstderr: {}",
        name,
        program,
        String::from_utf8_lossy(&compile_output.stdout),
        String::from_utf8_lossy(&compile_output.stderr)
    );

    // Clean up executable if created
    if executable_path.exists() {
        fs::remove_file(&executable_path).expect("Failed to remove executable");
    }
}

/// Test that a rue program fails to compile with expected error message
fn test_compile_error(name: &str, program: &str, expected_error: &str) {
    let project_root = get_project_root();
    let test_file = project_root.join(format!("test_type_system_temp_{name}.rue"));

    // Write the test program
    fs::write(&test_file, program).expect("Failed to write test file");

    // Try to compile the rue program
    let compile_output = if std::env::var("CARGO_MANIFEST_DIR").is_err() {
        // Buck2 build environment
        Command::new("buck2")
            .args(["run", "//crates/rue:rue", "--"])
            .arg(&test_file)
            .current_dir(project_root)
            .output()
            .expect("Failed to execute rue compiler via Buck2")
    } else {
        // Cargo build environment
        Command::new("cargo")
            .args(["run", "-p", "rue", "--"])
            .arg(&test_file)
            .current_dir(project_root)
            .output()
            .expect("Failed to execute rue compiler via Cargo")
    };

    // Clean up test file
    fs::remove_file(&test_file).expect("Failed to remove test file");

    assert!(
        !compile_output.status.success(),
        "Expected compilation to fail for test '{name}' but it succeeded!\nProgram:\n{program}"
    );

    let stderr = String::from_utf8_lossy(&compile_output.stderr);
    assert!(
        stderr.contains(expected_error),
        "Expected error message to contain '{expected_error}' for test '{name}'\nActual stderr:\n{stderr}"
    );
}

/// Test that compiles and runs a program, verifying the exit code
fn test_runs_with_exit_code(name: &str, program: &str, expected_exit_code: i32) {
    let project_root = get_project_root();
    let test_file = project_root.join(format!("test_type_system_temp_{name}.rue"));
    let executable_path = project_root.join(format!("test_type_system_temp_{name}"));

    // Write the test program
    fs::write(&test_file, program).expect("Failed to write test file");

    // Clean up any existing executable
    if executable_path.exists() {
        fs::remove_file(&executable_path).expect("Failed to remove existing executable");
    }

    // Compile the rue program
    let compile_output = if std::env::var("CARGO_MANIFEST_DIR").is_err() {
        // Buck2 build environment
        Command::new("buck2")
            .args(["run", "//crates/rue:rue", "--"])
            .arg(&test_file)
            .current_dir(project_root)
            .output()
            .expect("Failed to execute rue compiler via Buck2")
    } else {
        // Cargo build environment
        Command::new("cargo")
            .args(["run", "-p", "rue", "--"])
            .arg(&test_file)
            .current_dir(project_root)
            .output()
            .expect("Failed to execute rue compiler via Cargo")
    };

    // Clean up test file
    fs::remove_file(&test_file).expect("Failed to remove test file");

    assert!(
        compile_output.status.success(),
        "Compilation failed for test '{}'\nProgram:\n{}\nstdout: {}\nstderr: {}",
        name,
        program,
        String::from_utf8_lossy(&compile_output.stdout),
        String::from_utf8_lossy(&compile_output.stderr)
    );

    // Run the compiled executable
    let run_output = Command::new(&executable_path)
        .current_dir(project_root)
        .output()
        .expect("Failed to execute compiled program");

    // Check the exit code
    let actual_exit_code = run_output.status.code().unwrap_or(-1);
    assert_eq!(
        actual_exit_code,
        expected_exit_code,
        "Program '{}' returned exit code {} but expected {}.\nProgram:\n{}\nstdout: {}\nstderr: {}",
        name,
        actual_exit_code,
        expected_exit_code,
        program,
        String::from_utf8_lossy(&run_output.stdout),
        String::from_utf8_lossy(&run_output.stderr)
    );

    // Clean up the executable
    fs::remove_file(&executable_path).expect("Failed to remove executable");
}

#[test]
fn test_basic_type_annotations() {
    // Variable with i32 type
    test_compiles(
        "i32_variable",
        r#"
fn main() -> i32 {
    let x: i32 = 42;
    x
}
"#,
    );

    // Variable with i64 type
    test_compiles(
        "i64_variable",
        r#"
fn main() -> i64 {
    let x: i64 = 42;
    x
}
"#,
    );

    // Variable with bool type
    test_compiles(
        "bool_variable",
        r#"
fn main() -> i32 {
    let x: bool = true;
    if x {
        1
    } else {
        0
    }
}
"#,
    );
}

#[test]
fn test_type_mismatch_errors() {
    // Assigning i64 value to i32 variable
    test_compile_error(
        "i64_to_i32_mismatch",
        r#"
fn main() -> i32 {
    let x: i32 = 42;
    let y: i64 = x;
    0
}
"#,
        "Type mismatch",
    );

    // Assigning bool to i32
    test_compile_error(
        "bool_to_i32_mismatch",
        r#"
fn main() -> i32 {
    let x: i32 = true;
    x
}
"#,
        "Type mismatch",
    );

    // Wrong return type
    test_compile_error(
        "wrong_return_type",
        r#"
fn main() -> i32 {
    let x: i64 = 42;
    x
}
"#,
        "Type mismatch",
    );
}

#[test]
fn test_numeric_literal_defaults() {
    // Numeric literal in let binding defaults to i32
    test_compiles(
        "numeric_literal_default",
        r#"
fn main() -> i32 {
    let x: i32 = 42;
    x
}
"#,
    );

    // Integer literals can be used as i64 when that's the expected type
    test_compiles(
        "numeric_literal_to_i64",
        r#"
fn main() -> i32 {
    let x: i64 = 42;
    0
}
"#,
    );

    // But without type annotation, numeric literals default to i32
    test_compile_error(
        "numeric_literal_default_in_binary_op",
        r#"
fn main() -> i32 {
    let x: i64 = 100;
    let y = x + 42;
    0
}
"#,
        "Type mismatch",
    );
}

#[test]
fn test_all_supported_types() {
    // Test i32 operations
    test_runs_with_exit_code(
        "i32_operations",
        r#"
fn main() -> i32 {
    let a: i32 = 10;
    let b: i32 = 20;
    let c: i32 = a + b;
    c
}
"#,
        30,
    );

    // Test i64 operations
    test_runs_with_exit_code(
        "i64_operations",
        r#"
fn main() -> i64 {
    let a: i64 = 100;
    let b: i64 = 50;
    let c: i64 = a + b;
    c
}
"#,
        150,
    );

    // Test bool operations
    test_runs_with_exit_code(
        "bool_operations",
        r#"
fn main() -> i32 {
    let a: bool = true;
    let b: bool = false;
    let c: bool = a != b;
    if c {
        1
    } else {
        0
    }
}
"#,
        1,
    );

    // Test unit type (functions with no return)
    test_compiles(
        "unit_type_function",
        r#"
fn do_nothing() {
    let x: i32 = 42;
}

fn main() -> i32 {
    do_nothing();
    0
}
"#,
    );
}

#[test]
fn test_function_type_checking() {
    // Function with typed parameters
    test_compiles(
        "typed_parameters",
        r#"
fn add(a: i32, b: i32) -> i32 {
    a + b
}

fn main() -> i32 {
    add(10, 20)
}
"#,
    );

    // Type mismatch in function arguments
    test_compile_error(
        "function_arg_type_mismatch",
        r#"
fn add(a: i32, b: i32) -> i32 {
    a + b
}

fn main() -> i32 {
    let x: i64 = 10;
    add(x, 20)
}
"#,
        "Type mismatch",
    );

    // Wrong number of arguments
    test_compile_error(
        "wrong_arg_count",
        r#"
fn add(a: i32, b: i32) -> i32 {
    a + b
}

fn main() -> i32 {
    add(10)
}
"#,
        "Expected 2 arguments",
    );
}

#[test]
fn test_main_function_return_types() {
    // main returning i32
    test_runs_with_exit_code(
        "main_returns_i32",
        r#"
fn main() -> i32 {
    42
}
"#,
        42,
    );

    // main returning i64
    test_runs_with_exit_code(
        "main_returns_i64",
        r#"
fn main() -> i64 {
    100
}
"#,
        100,
    );

    // main returning unit (implicit)
    test_runs_with_exit_code(
        "main_returns_unit",
        r#"
fn main() {
    let x: i32 = 42;
}
"#,
        0,
    );

    // main returning bool
    test_runs_with_exit_code(
        "main_returns_bool_true",
        r#"
fn main() -> bool {
    true
}
"#,
        1,
    );

    test_runs_with_exit_code(
        "main_returns_bool_false",
        r#"
fn main() -> bool {
    false
}
"#,
        0,
    );
}

// Additional tests for complex scenarios
#[test]
fn test_complex_type_scenarios() {
    // Mixed types in if-else
    test_compile_error(
        "if_else_type_mismatch",
        r#"
fn main() -> i32 {
    let cond: bool = true;
    let x: i32 = if cond {
        42
    } else {
        true
    };
    x
}
"#,
        "If expression branches must have the same type",
    );

    // Type checking with while loops
    test_compiles(
        "while_with_types",
        r#"
fn main() -> i32 {
    let x: i32 = 10;
    while x > 0 {
        0;
    };
    x
}
"#,
    );

    // Comparison operators produce bool
    test_compiles(
        "comparison_produces_bool",
        r#"
fn main() -> i32 {
    let a: i32 = 10;
    let b: i32 = 20;
    let c: bool = a < b;
    if c {
        1
    } else {
        0
    }
}
"#,
    );
}
