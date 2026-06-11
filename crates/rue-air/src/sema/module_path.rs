//! Structured import path resolution.
//!
//! This module provides a structured approach to resolving import paths in Rue.
//! Instead of ad-hoc string matching with many special cases, it uses a typed
//! representation of different import path kinds and explicit resolution order.
//!
//! # Resolution Order
//!
//! When resolving an import path like `@import("foo")`, we check in this order:
//!
//! 1. **Standard library** - if the path is exactly "std"
//! 2. **Exact path with extension** - if path includes ".rue" extension
//! 3. **Simple file match** - look for `foo.rue` or path ending with `foo.rue`
//! 4. **Facade module** - look for `foo/_foo.rue` (the directory module's entry point, inside the directory — RUE-137)
//!
//! Both module forms existing at once (`foo.rue` AND `foo/_foo.rue`) is an
//! ambiguity error (E0708), mirroring Rust's E0761 — see [`ModulePath::find_ambiguity`].

use std::path::Path;

/// Represents a parsed import path with its resolution strategy.
///
/// This enum categorizes import paths to determine how they should be resolved.
/// Each variant corresponds to a different resolution strategy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModulePath {
    /// Standard library import: `@import("std")`
    ///
    /// Resolves to the stdlib facade `_std.rue`, loaded by the driver from
    /// `$RUE_STD_PATH` or an adjacent `std/` directory.
    Std,

    /// Import with explicit `.rue` extension: `@import("foo.rue")`
    ///
    /// The path is taken as-is and matched against loaded file paths.
    ExplicitRue { path: String },

    /// Simple module import: `@import("foo")` or `@import("utils/strings")`
    ///
    /// Resolution tries:
    /// 1. `{path}.rue` - standard file
    /// 2. `{path}/_{basename}.rue` - in-directory facade for directory modules
    ///
    /// For nested paths like `utils/strings`, we look for `utils/strings.rue`.
    Simple { path: String },
}

impl ModulePath {
    /// Parse an import path string into a structured `ModulePath`.
    ///
    /// This determines the kind of import based on the path format.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// ModulePath::parse("std") => ModulePath::Std
    /// ModulePath::parse("foo.rue") => ModulePath::ExplicitRue { path: "foo.rue" }
    /// ModulePath::parse("foo") => ModulePath::Simple { path: "foo" }
    /// ModulePath::parse("utils/strings") => ModulePath::Simple { path: "utils/strings" }
    /// ```
    pub fn parse(import_path: &str) -> Self {
        // Check for standard library
        if import_path == "std" {
            return ModulePath::Std;
        }

        // Check for explicit .rue extension
        if import_path.ends_with(".rue") {
            return ModulePath::ExplicitRue {
                path: import_path.to_string(),
            };
        }

        // Otherwise, it's a simple module import
        ModulePath::Simple {
            path: import_path.to_string(),
        }
    }

    /// Resolve this import path against a collection of loaded file paths.
    ///
    /// Returns `Some(resolved_path)` if a match is found, or `None` if the
    /// module cannot be found.
    ///
    /// The resolution order is:
    /// 1. Exact match (for ExplicitRue)
    /// 2. Standard file match (`{path}.rue`)
    /// 3. Path suffix match (for nested paths)
    /// 4. In-directory facade match (`{basename}/_{basename}.rue`)
    pub fn resolve<'a, I>(&self, loaded_paths: I) -> Option<String>
    where
        I: Iterator<Item = &'a String>,
    {
        match self {
            ModulePath::Std => {
                // The standard library's entry point is std/_std.rue. The
                // driver loads it from $RUE_STD_PATH or an adjacent std/
                // directory (see discover_and_load_imports in the rue crate);
                // here we just match it among the loaded files.
                self.resolve_explicit("_std.rue", loaded_paths)
            }
            ModulePath::ExplicitRue { path } => self.resolve_explicit(path, loaded_paths),
            ModulePath::Simple { path } => self.resolve_simple(path, loaded_paths),
        }
    }

    /// Detect the dual-entity ambiguity for a simple import: BOTH a file
    /// module `{path}.rue` and a directory module `{path}/_{basename}.rue`
    /// exist among the loaded files. Mirrors Rust's E0761 (module file found
    /// at both locations): silently preferring one is a footgun, so callers
    /// turn this into a hard error. Returns (file_module, dir_module).
    pub fn find_ambiguity<'a, I>(&self, loaded_paths: I) -> Option<(String, String)>
    where
        I: Iterator<Item = &'a String>,
    {
        let ModulePath::Simple { path } = self else {
            return None;
        };
        let import_with_rue = format!("{}.rue", path);
        let basename = Path::new(path)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or(path);
        let facade_suffix = format!("{}/_{}.rue", basename, basename);

        let boundary_match = |candidate: &str, suffix: &str| -> bool {
            candidate.ends_with(suffix) && {
                let prefix_len = candidate.len() - suffix.len();
                prefix_len == 0 || candidate.as_bytes()[prefix_len - 1] == b'/'
            }
        };

        let mut file_module = None;
        let mut dir_module = None;
        for p in loaded_paths {
            if file_module.is_none() && boundary_match(p, &import_with_rue) {
                file_module = Some(p.clone());
            } else if dir_module.is_none() && boundary_match(p, &facade_suffix) {
                dir_module = Some(p.clone());
            }
        }
        file_module.zip(dir_module)
    }

    /// Resolve an explicit `.rue` path.
    fn resolve_explicit<'a, I>(&self, import_path: &str, loaded_paths: I) -> Option<String>
    where
        I: Iterator<Item = &'a String>,
    {
        let collected: Vec<_> = loaded_paths.collect();

        // Priority 1: Exact match
        for file_path in &collected {
            if *file_path == import_path {
                return Some((*file_path).clone());
            }
        }

        // Priority 2: Path ends with import_path
        // This handles cases like "foo.rue" matching "/path/to/foo.rue"
        for file_path in &collected {
            if file_path.ends_with(import_path) {
                // Verify it's a proper path boundary (preceded by / or start of string)
                let prefix_len = file_path.len() - import_path.len();
                if prefix_len == 0 || file_path.as_bytes()[prefix_len - 1] == b'/' {
                    return Some((*file_path).clone());
                }
            }
        }

        None
    }

    /// Resolve a simple (no extension) import path.
    fn resolve_simple<'a, I>(&self, import_path: &str, loaded_paths: I) -> Option<String>
    where
        I: Iterator<Item = &'a String>,
    {
        let import_with_rue = format!("{}.rue", import_path);
        let collected: Vec<_> = loaded_paths.collect();

        // The directory-module facade lives INSIDE the directory:
        // `@import("utils")` -> `utils/_utils.rue`, like the stdlib's
        // `std/_std.rue` (placement ratified in RUE-137). Matching the bare
        // basename would let a stray sibling `_utils.rue` masquerade as a
        // directory module.
        let basename = Path::new(import_path)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or(import_path);
        let facade_suffix = format!("{}/_{}.rue", basename, basename);

        // Priority 1: Look for exact {path}.rue
        for file_path in &collected {
            if *file_path == &import_with_rue {
                return Some((*file_path).clone());
            }
        }

        // Priority 2: Look for path ending with {path}.rue
        // This handles "utils/strings" matching "/project/utils/strings.rue"
        for file_path in &collected {
            if file_path.ends_with(&import_with_rue) {
                // Verify it's a proper path boundary
                let prefix_len = file_path.len() - import_with_rue.len();
                if prefix_len == 0 || file_path.as_bytes()[prefix_len - 1] == b'/' {
                    return Some((*file_path).clone());
                }
            }
        }

        // Priority 3: Look for files matching just the basename (e.g., "math" matches "src/math.rue")
        for file_path in &collected {
            if let Some(file_name) = Path::new(file_path.as_str())
                .file_stem()
                .and_then(|s| s.to_str())
            {
                if file_name == basename {
                    return Some((*file_path).clone());
                }
            }
        }

        // Priority 4: Look for the in-directory facade ({foo}/_{foo}.rue)
        for file_path in &collected {
            if file_path.ends_with(&facade_suffix) {
                // Verify it's a proper path boundary
                let prefix_len = file_path.len() - facade_suffix.len();
                if prefix_len == 0 || file_path.as_bytes()[prefix_len - 1] == b'/' {
                    return Some((*file_path).clone());
                }
            }
        }

        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // =========================================================================
    // Parsing tests
    // =========================================================================

    #[test]
    fn test_parse_std() {
        assert_eq!(ModulePath::parse("std"), ModulePath::Std);
    }

    #[test]
    fn test_parse_explicit_rue() {
        assert_eq!(
            ModulePath::parse("foo.rue"),
            ModulePath::ExplicitRue {
                path: "foo.rue".to_string()
            }
        );
        assert_eq!(
            ModulePath::parse("utils/strings.rue"),
            ModulePath::ExplicitRue {
                path: "utils/strings.rue".to_string()
            }
        );
    }

    #[test]
    fn test_parse_simple() {
        assert_eq!(
            ModulePath::parse("foo"),
            ModulePath::Simple {
                path: "foo".to_string()
            }
        );
        assert_eq!(
            ModulePath::parse("utils/strings"),
            ModulePath::Simple {
                path: "utils/strings".to_string()
            }
        );
    }

    // =========================================================================
    // Resolution tests - Standard library
    // =========================================================================

    #[test]
    fn test_resolve_std_not_supported() {
        let paths = vec!["main.rue".to_string()];
        let module = ModulePath::Std;
        assert_eq!(module.resolve(paths.iter()), None);
    }

    // =========================================================================
    // Resolution tests - Explicit .rue extension
    // =========================================================================

    #[test]
    fn test_resolve_explicit_exact_match() {
        let paths = vec!["foo.rue".to_string(), "bar.rue".to_string()];
        let module = ModulePath::ExplicitRue {
            path: "foo.rue".to_string(),
        };
        assert_eq!(module.resolve(paths.iter()), Some("foo.rue".to_string()));
    }

    #[test]
    fn test_resolve_explicit_suffix_match() {
        let paths = vec!["/project/src/foo.rue".to_string()];
        let module = ModulePath::ExplicitRue {
            path: "foo.rue".to_string(),
        };
        assert_eq!(
            module.resolve(paths.iter()),
            Some("/project/src/foo.rue".to_string())
        );
    }

    #[test]
    fn test_resolve_explicit_no_false_substring_match() {
        // "foo.rue" should NOT match "xfoo.rue" (no path boundary)
        let paths = vec!["xfoo.rue".to_string()];
        let module = ModulePath::ExplicitRue {
            path: "foo.rue".to_string(),
        };
        assert_eq!(module.resolve(paths.iter()), None);
    }

    #[test]
    fn test_resolve_explicit_nested_path() {
        let paths = vec!["/project/utils/strings.rue".to_string()];
        let module = ModulePath::ExplicitRue {
            path: "utils/strings.rue".to_string(),
        };
        assert_eq!(
            module.resolve(paths.iter()),
            Some("/project/utils/strings.rue".to_string())
        );
    }

    // =========================================================================
    // Resolution tests - Simple (no extension)
    // =========================================================================

    #[test]
    fn test_resolve_simple_exact_match() {
        let paths = vec!["foo.rue".to_string()];
        let module = ModulePath::Simple {
            path: "foo".to_string(),
        };
        assert_eq!(module.resolve(paths.iter()), Some("foo.rue".to_string()));
    }

    #[test]
    fn test_resolve_simple_suffix_match() {
        let paths = vec!["/project/src/foo.rue".to_string()];
        let module = ModulePath::Simple {
            path: "foo".to_string(),
        };
        assert_eq!(
            module.resolve(paths.iter()),
            Some("/project/src/foo.rue".to_string())
        );
    }

    #[test]
    fn test_resolve_simple_nested_path() {
        let paths = vec!["/project/utils/strings.rue".to_string()];
        let module = ModulePath::Simple {
            path: "utils/strings".to_string(),
        };
        assert_eq!(
            module.resolve(paths.iter()),
            Some("/project/utils/strings.rue".to_string())
        );
    }

    #[test]
    fn test_resolve_simple_basename_match() {
        // "math" should match "src/math.rue" by basename
        let paths = vec!["src/math.rue".to_string()];
        let module = ModulePath::Simple {
            path: "math".to_string(),
        };
        assert_eq!(
            module.resolve(paths.iter()),
            Some("src/math.rue".to_string())
        );
    }

    #[test]
    fn test_resolve_simple_facade_file() {
        // The facade lives INSIDE the directory (RUE-137): utils/_utils.rue.
        let paths = vec!["utils/_utils.rue".to_string()];
        let module = ModulePath::Simple {
            path: "utils".to_string(),
        };
        assert_eq!(
            module.resolve(paths.iter()),
            Some("utils/_utils.rue".to_string())
        );
    }

    #[test]
    fn test_resolve_simple_sibling_facade_rejected() {
        // The pre-RUE-137 sibling layout is NOT a directory module.
        let paths = vec!["_utils.rue".to_string()];
        let module = ModulePath::Simple {
            path: "utils".to_string(),
        };
        assert_eq!(module.resolve(paths.iter()), None);
    }

    #[test]
    fn test_find_ambiguity_both_forms() {
        // Both "foo.rue" and "foo/_foo.rue" loaded: ambiguous, not a
        // precedence question (callers raise E0708).
        let paths = vec!["foo/_foo.rue".to_string(), "foo.rue".to_string()];
        let module = ModulePath::Simple {
            path: "foo".to_string(),
        };
        assert_eq!(
            module.find_ambiguity(paths.iter()),
            Some(("foo.rue".to_string(), "foo/_foo.rue".to_string()))
        );
    }

    #[test]
    fn test_find_ambiguity_single_form_is_none() {
        let paths = vec!["foo.rue".to_string()];
        let module = ModulePath::Simple {
            path: "foo".to_string(),
        };
        assert_eq!(module.find_ambiguity(paths.iter()), None);
    }

    #[test]
    fn test_resolve_simple_no_false_substring_match() {
        // "math" should NOT match "mathematics.rue"
        let paths = vec!["mathematics.rue".to_string()];
        let module = ModulePath::Simple {
            path: "math".to_string(),
        };
        // The basename "mathematics" != "math", so no match
        assert_eq!(module.resolve(paths.iter()), None);
    }

    // =========================================================================
    // Edge case tests
    // =========================================================================

    #[test]
    fn test_resolve_not_found() {
        let paths = vec!["other.rue".to_string()];
        let module = ModulePath::Simple {
            path: "foo".to_string(),
        };
        assert_eq!(module.resolve(paths.iter()), None);
    }

    #[test]
    fn test_resolve_empty_paths() {
        let paths: Vec<String> = vec![];
        let module = ModulePath::Simple {
            path: "foo".to_string(),
        };
        assert_eq!(module.resolve(paths.iter()), None);
    }
}
