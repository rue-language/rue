use std::env;
use std::path::PathBuf;

/// Get the path to the rue compiler binary
/// In Buck2 tests, this looks for the actual compiled rue binary
pub fn get_rue_binary() -> String {
    // Try to find the rue binary in buck-out
    let workspace_root = env::current_dir().expect("Failed to get current directory");

    // Search specifically in buck-out/v2 for the rue binary
    let buck_out = workspace_root.join("buck-out/v2");
    if buck_out.exists() {
        // Use find command to locate the rue binary
        let output = std::process::Command::new("find")
            .arg(&buck_out)
            .arg("-name")
            .arg("rue")
            .arg("-type")
            .arg("f")
            .arg("-path")
            .arg("*/crates/rue/*")
            .output()
            .expect("Failed to run find command");

        if output.status.success() {
            let paths = String::from_utf8_lossy(&output.stdout);
            if let Some(path) = paths.lines().next() {
                if !path.is_empty() && PathBuf::from(path).exists() {
                    return path.to_string();
                }
            }
        }
    }

    panic!(
        "Rue binary not found in buck-out. Make sure to build it first with: ./buck2 build //crates/rue:rue"
    );
}
