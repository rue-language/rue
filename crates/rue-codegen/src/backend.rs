// Simplified RuntimeProvider that only provides archive path
//
// This module handles runtime library discovery for the Rue compiler.
// The runtime is now consolidated into a single library.

use crate::CodegenError;
use std::env;
use std::fs;
use std::path::Path;

/// RuntimeProvider handles runtime library discovery
pub struct RuntimeProvider {
    /// Path to the runtime archive if found
    runtime_archive_path: Option<String>,
}

impl RuntimeProvider {
    /// Create a new RuntimeProvider by discovering the runtime library
    pub fn new() -> Result<Self, CodegenError> {
        let runtime_path = Self::find_runtime_archive()?;

        Ok(Self {
            runtime_archive_path: runtime_path,
        })
    }

    /// Find a Buck2 library in the output directory
    fn find_buck2_library(lib_name: &str, target_path: &str) -> Option<String> {
        // Buck2 output is always in buck-out/v2/gen/root/<hash>/<target_path>
        // The hash changes, so we need to search for it
        let buck_out = "buck-out/v2/gen/root";

        if let Ok(entries) = fs::read_dir(buck_out) {
            for entry in entries.flatten() {
                let lib_path = entry.path().join(target_path);
                if let Ok(lib_entries) = fs::read_dir(&lib_path) {
                    for lib_entry in lib_entries.flatten() {
                        let filename = lib_entry.file_name();
                        let filename_str = filename.to_string_lossy();
                        if filename_str.starts_with(lib_name) && filename_str.ends_with(".a") {
                            return Some(lib_entry.path().to_string_lossy().to_string());
                        }
                    }
                }
            }
        }
        None
    }

    /// Find the runtime archive path
    fn find_runtime_archive() -> Result<Option<String>, CodegenError> {
        // Check environment variable (set by Buck2 tests)
        if let Ok(path) = env::var("RUE_RUNTIME_LIB") {
            if Path::new(&path).exists() {
                return Ok(Some(path));
            }
        }

        // Check Buck2 output directory
        Ok(Self::find_buck2_library(
            "librue_runtime-",
            "crates/rue-runtime/__rue-runtime__/SSTL",
        ))
    }

    /// Get the crt0 archive path if available (for backward compatibility, returns runtime path)
    pub fn crt0_archive_path(&self) -> Option<&str> {
        self.runtime_archive_path.as_deref()
    }

    /// Get the runtime archive path if available
    pub fn runtime_archive_path(&self) -> Option<&str> {
        self.runtime_archive_path.as_deref()
    }

    /// Check if external runtime is available
    pub fn has_external_runtime(&self) -> bool {
        self.runtime_archive_path.is_some()
    }
}

impl Default for RuntimeProvider {
    fn default() -> Self {
        Self::new().unwrap_or(Self {
            runtime_archive_path: None,
        })
    }
}
