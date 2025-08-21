use std::path::PathBuf;
use std::process::Command;

/// Get the path to the rue compiler binary
/// This works with Buck2 and handles both CI and local development
pub fn get_rue_binary() -> String {
    // If we're running under Buck2 test, the rue binary should already be built
    // and available via TEST_BINARY environment variable or similar
    if let Ok(test_binary) = std::env::var("RUE_TEST_BINARY") {
        return test_binary;
    }

    // Check for CARGO_BIN_EXE_rue which Buck2 sets for tests
    if let Ok(test_binary) = std::env::var("CARGO_BIN_EXE_rue") {
        return test_binary;
    }

    // Check if the binary already exists in buck-out (the hash changes)
    // Search for any rue binary in the buck-out directory
    let buck_out = PathBuf::from("buck-out/v2/gen/root");
    if buck_out.exists() {
        for entry in std::fs::read_dir(&buck_out).ok().into_iter().flatten() {
            if let Ok(entry) = entry {
                let rue_path = entry.path().join("crates/rue/__rue__/rue");
                if rue_path.exists() {
                    return rue_path.to_string_lossy().to_string();
                }
            }
        }
    }

    // Otherwise, try to build it
    // Check if we're running under Buck2 (in CI or with buck2 test)
    let buck_cmd = if std::env::var("BUCK_BUILD_ID").is_ok() {
        // In Buck2 test environment, buck2 should be in PATH
        "buck2"
    } else {
        // Local development
        "./buck2"
    };

    // Build the rue compiler and get its path
    let output = Command::new(buck_cmd)
        .args(["build", "--show-output", "//crates/rue:rue"])
        .output()
        .unwrap_or_else(|e| {
            // If buck2 is not found, try to use the pre-built binary
            panic!("Failed to run buck2: {}. Please build rue first with: ./buck2 build //crates/rue:rue", e);
        });

    if !output.status.success() {
        panic!(
            "Failed to build rue compiler:\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    // Parse the output to find the binary path
    let output_str = String::from_utf8_lossy(&output.stdout);
    output_str
        .lines()
        .find(|line| line.contains("//crates/rue:rue"))
        .and_then(|line| line.split_whitespace().last())
        .expect("Failed to find rue binary path in buck2 output")
        .to_string()
}
