//! Common test utilities shared across test files

use std::path::Path;

/// Get the project root directory for Buck2
pub fn get_project_root() -> &'static Path {
    // Buck2 sets the working directory to the project root
    // We can also check for BUCK_BUILD_ID to confirm we're in Buck2
    if std::env::var("BUCK_BUILD_ID").is_ok() {
        // Running under Buck2 - working directory is project root
        let project_root = std::env::current_dir().expect("Failed to get current directory");
        return Box::leak(project_root.into_boxed_path());
    }

    // For local development: find project root by looking for .buckroot
    let current_dir = std::env::current_dir().expect("Failed to get current directory");
    let mut dir = current_dir.as_path();

    loop {
        if dir.join(".buckroot").exists() {
            return Box::leak(dir.to_path_buf().into_boxed_path());
        }

        if let Some(parent) = dir.parent() {
            dir = parent;
        } else {
            panic!("Could not find project root directory (no .buckroot found)");
        }
    }
}
