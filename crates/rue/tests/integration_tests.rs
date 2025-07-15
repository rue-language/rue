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

/// Test that compiles and runs a .rue program, verifying the exit code
fn test_rue_program(sample_name: &str, expected_exit_code: i32) {
    let project_root = get_project_root();

    let sample_path = project_root
        .join("samples")
        .join(format!("{sample_name}.rue"));
    let executable_path = project_root.join("samples").join(sample_name);

    // Ensure the sample file exists
    assert!(
        sample_path.exists(),
        "Sample file {sample_path:?} does not exist"
    );

    // Clean up any existing executable
    if executable_path.exists() {
        fs::remove_file(&executable_path).expect("Failed to remove existing executable");
    }

    // Compile the rue program using the rue compiler
    // Try Buck2 first, fall back to Cargo
    let compile_output = if std::env::var("CARGO_MANIFEST_DIR").is_err() {
        // Buck2 build environment
        Command::new("buck2")
            .args(["run", "//crates/rue:rue", "--"])
            .arg(&sample_path)
            .current_dir(project_root)
            .output()
            .expect("Failed to execute rue compiler via Buck2")
    } else {
        // Cargo build environment
        Command::new("cargo")
            .args(["run", "-p", "rue", "--"])
            .arg(&sample_path)
            .current_dir(project_root)
            .output()
            .expect("Failed to execute rue compiler via Cargo")
    };

    if !compile_output.status.success() {
        panic!(
            "Compilation failed for {}.rue:\nstdout: {}\nstderr: {}",
            sample_name,
            String::from_utf8_lossy(&compile_output.stdout),
            String::from_utf8_lossy(&compile_output.stderr)
        );
    }

    // Verify the executable was created
    assert!(
        executable_path.exists(),
        "Executable {executable_path:?} was not created"
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
        "Program {}.rue returned exit code {} but expected {}.\nstdout: {}\nstderr: {}",
        sample_name,
        actual_exit_code,
        expected_exit_code,
        String::from_utf8_lossy(&run_output.stdout),
        String::from_utf8_lossy(&run_output.stderr)
    );

    // Clean up the executable
    fs::remove_file(&executable_path).expect("Failed to remove executable after test");
}

#[test]
fn test_simple_program() {
    test_rue_program("simple", 42);
}

#[test]
fn test_factorial_program() {
    test_rue_program("factorial", 120); // factorial(5) = 120
}

#[test]
#[ignore] // for now, we don't compile this correctly yet
fn test_fibonacci_program() {
    test_rue_program("fibonacci", 55); // fibonacci(10) = 55
}

#[test]
fn test_while_loop_program() {
    test_rue_program("countdown", 42); // simple_while(10) returns 42 since 10 > 3
}

#[test]
fn test_assignment_demo_program() {
    test_rue_program("assignment_demo", 6); // test_assignment_in_while() returns 6
}

#[test]
fn test_division_program() {
    test_rue_program("division_test", 10); // 100 / 10 = 10
}

#[test]
fn test_large_immediate_program() {
    test_rue_program("large_immediate", 0); // Tests 64-bit immediate handling
}

#[test]
fn test_large_div_immediate_program() {
    test_rue_program("large_div_immediate", 1); // 100 / 2^31 = 0, then + 1 = 1
}

#[test]
fn test_all_samples_compile() {
    // Test samples that should compile successfully
    let successful_samples = [
        ("simple", 42),
        ("factorial", 120),
        ("countdown", 42),          // simple_while(10) returns 42 since 10 > 3
        ("if_demo", 5),             // if 1 <= 2 { 5 } else { 10 } returns 5
        ("assignment_demo", 6),     // test_assignment_in_while() returns sum of 0+1+2+3 = 6
        ("division_test", 10),      // 100 / 10 = 10
        ("large_immediate", 0),     // Tests 64-bit immediate handling
        ("large_div_immediate", 1), // Tests division by large immediate
    ];

    for (sample_name, expected_exit_code) in successful_samples {
        test_rue_program(sample_name, expected_exit_code);
    }
}
