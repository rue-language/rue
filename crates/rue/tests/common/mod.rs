//! Common test utilities shared across test files

use std::path::Path;

/// Get the project root directory, compatible with both Cargo and Buck2
pub fn get_project_root() -> &'static Path {
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
