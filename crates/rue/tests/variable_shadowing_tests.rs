mod common;

use common::get_project_root;
use std::fs;
use std::process::Command;

fn test_program_succeeds(name: &str, program: &str, expected_exit_code: i32) {
    let project_root = get_project_root();
    let test_file = project_root.join(format!("test_shadowing_temp_{name}.rue"));

    // Write the test program
    fs::write(&test_file, program).expect("Failed to write test file");

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
        "Compilation failed for test '{name}':\n{}",
        String::from_utf8_lossy(&compile_output.stderr)
    );

    // Run the compiled program
    let exe_path = test_file.with_extension("");
    if exe_path.exists() {
        let run_output = Command::new(&exe_path)
            .output()
            .expect("Failed to execute compiled program");

        // Clean up executable
        fs::remove_file(&exe_path).expect("Failed to remove executable");

        // Check exit code
        let exit_code = run_output.status.code().unwrap_or(-1);
        assert_eq!(
            exit_code, expected_exit_code,
            "Test '{name}' failed: Expected exit code {expected_exit_code}, got {exit_code}"
        );
    } else {
        panic!("Compiled executable not found for test '{name}'");
    }
}

#[test]
fn test_basic_variable_shadowing() {
    // Variable shadowing in nested blocks
    test_program_succeeds(
        "basic_shadowing",
        r#"
fn main() -> i32 {
    let x: i32 = 10;
    if true {
        let x: i32 = 20;  // Shadows outer x
        x  // Returns 20
    } else {
        x  // Would return 10
    }
}
"#,
        20,
    );
}

#[test]
fn test_multiple_level_shadowing() {
    // Multiple levels of shadowing
    test_program_succeeds(
        "multiple_level_shadowing",
        r#"
fn main() -> i32 {
    let x: i32 = 1;
    if true {
        let x: i32 = 2;
        if true {
            let x: i32 = 3;
            x  // Returns 3
        } else {
            x  // Would return 2
        }
    } else {
        x  // Would return 1
    }
}
"#,
        3,
    );
}

#[test]
fn test_shadowing_with_assignment() {
    // Shadowing with assignment
    test_program_succeeds(
        "shadowing_with_assignment",
        r#"
fn main() -> i32 {
    let x: i32 = 5;
    x = 10;  // Modifies outer x
    if true {
        let x: i32 = 20;  // Shadows outer x
        x = 30;  // Modifies inner x
        x  // Returns 30
    } else {
        x  // Would return 10
    }
}
"#,
        30,
    );
}

#[test]
fn test_outer_variable_still_accessible() {
    // Outer variable is still accessible after inner scope
    test_program_succeeds(
        "outer_var_accessible",
        r#"
fn main() -> i32 {
    let x: i32 = 100;
    if true {
        let x: i32 = 200;
        x;  // Inner x = 200
        0  // Need to return same type from both branches
    } else {
        0
    };
    x  // Outer x = 100
}
"#,
        100,
    );
}

#[test]
fn test_while_loop_shadowing() {
    // Variable shadowing in while loops
    test_program_succeeds(
        "while_loop_shadowing",
        r#"
fn main() -> i32 {
    let x: i32 = 50;
    let count: i32 = 0;
    while count < 1 {
        let x: i32 = 75;  // Shadows outer x
        count = count + 1;
        x;  // Inner x = 75
    };
    x  // Outer x = 50
}
"#,
        50,
    );
}

#[test]
fn test_function_parameter_shadowing() {
    // Function parameters can be shadowed
    test_program_succeeds(
        "param_shadowing",
        r#"
fn process(x: i32) -> i32 {
    if x > 10 {
        let x: i32 = 10;  // Shadows parameter
        x  // Returns 10
    } else {
        x  // Returns parameter value
    }
}

fn main() -> i32 {
    process(20)  // Should return 10
}
"#,
        10,
    );
}

#[test]
fn test_shadowing_different_types() {
    // Variables can be shadowed with different types
    test_program_succeeds(
        "shadow_different_type",
        r#"
fn main() -> i32 {
    let x: i32 = 42;
    if true {
        let x: bool = true;  // Shadows with different type
        if x {
            1
        } else {
            0
        }
    } else {
        x  // Original i32 x
    }
}
"#,
        1,
    );
}

#[test]
fn test_same_scope_shadowing() {
    // Variables can be shadowed within the same scope
    test_program_succeeds(
        "same_scope_shadow",
        r#"
fn main() -> i32 {
    let x: i32 = 10;
    let x: i32 = 20;  // Shadows in same scope
    let x: i32 = 30;  // Shadows again
    x  // Returns 30
}
"#,
        30,
    );
}

#[test]
fn test_complex_shadowing_scenario() {
    // Complex scenario with multiple variables and shadowing
    test_program_succeeds(
        "complex_shadowing",
        r#"
fn main() -> i32 {
    let a: i32 = 1;
    let b: i32 = 2;
    
    if true {
        let a: i32 = 10;  // Shadow a
        
        if true {
            let b: i32 = 20;  // Shadow b
            a + b  // 10 + 20 = 30
        } else {
            a + b  // Would be 10 + 2 = 12
        }
    } else {
        a + b  // Would be 1 + 2 = 3
    }
}
"#,
        30,
    );
}
