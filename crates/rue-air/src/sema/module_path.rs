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
//! 3. **Simple file match** - look for `foo.rue`
//! 4. **Facade module** - look for `foo/_foo.rue` (the directory module's entry point, inside the directory — RUE-137)
//!
//! Both module forms existing at once (`foo.rue` AND `foo/_foo.rue`) is an
//! ambiguity error (E0708), mirroring Rust's E0761 — see [`ModulePath::resolve_in_dirs`].
//!
//! # Importer-relative resolution (spec 10.2:2)
//!
//! Resolution is **importer-relative**, not program-global. A relative import
//! candidate is joined against a list of base directories, searched in order:
//! first the directory containing the *importing* file, then the directory
//! containing the *root* file. The first base directory that yields a match
//! wins, and ambiguity is judged **within** that base directory. This is why
//! two unrelated `@import("foo")` sites in different directories each resolve
//! to their own `foo.rue` (no false cross-directory ambiguity), and why the
//! same source text always imports the file next to it rather than an
//! unrelated same-named file elsewhere in the program (RUE-266).

use std::path::{Component, Path, PathBuf};

/// Lexically normalize a path for equivalence comparison by dropping `.`
/// (current-directory) components, so `./foo.rue`, `sub/./foo.rue` and
/// `sub/foo.rue` compare equal. Parent (`..`) and normal components are
/// preserved, so the normalization is purely lexical and never touches the
/// filesystem. Mirrors `file_paths::normalize_module_path`.
fn normalize(path: &str) -> String {
    let mut out = PathBuf::new();
    for comp in Path::new(path).components() {
        match comp {
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out.to_string_lossy().into_owned()
}

/// Join a base directory with a relative path. An empty base directory yields
/// the relative path unchanged (the importer sits at the search root).
fn join_base(base: &str, rel: &str) -> String {
    if base.is_empty() {
        rel.to_string()
    } else {
        Path::new(base).join(rel).to_string_lossy().into_owned()
    }
}

/// The outcome of resolving an import path against the loaded files, anchored
/// to a set of base directories.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DirResolution {
    /// A unique loaded file matched. Carries the original (un-normalized)
    /// loaded path so downstream lookups key into `file_paths` as stored.
    Resolved(String),
    /// Both a file module (`{path}.rue`) and a directory-module facade
    /// (`{path}/_{basename}.rue`) exist within the same base directory. Callers
    /// raise E0708. Carries both original paths.
    Ambiguous {
        file_module: String,
        dir_module: String,
    },
    /// No loaded file matched in any base directory.
    NotFound,
}

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
    /// The path is taken as-is (joined against a base directory) and matched
    /// against loaded file paths.
    ExplicitRue { path: String },

    /// Simple module import: `@import("foo")` or `@import("utils/strings")`
    ///
    /// Resolution tries, within each base directory:
    /// 1. `{path}.rue` - standard file
    /// 2. `{path}/_{basename}.rue` - in-directory facade for directory modules
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

    /// Resolve this import path against the loaded files, anchored to
    /// `base_dirs` (searched in order — importer's directory first, then the
    /// root file's directory; see spec 10.2:2).
    ///
    /// The first base directory that yields any candidate determines the
    /// result: if both a file module and a directory-module facade exist
    /// *there*, the result is [`DirResolution::Ambiguous`]; otherwise the
    /// single match is [`DirResolution::Resolved`]. A base directory with no
    /// candidate is skipped in favor of the next. If no base directory yields a
    /// match, the result is [`DirResolution::NotFound`].
    ///
    /// Matching is by lexically-normalized path equality (dropping `.`
    /// components), so `./foo.rue` and `foo.rue` are the same file (spec
    /// 10.2:4). The returned string is the original loaded path.
    pub fn resolve_in_dirs<'a, I>(&self, base_dirs: &[&str], loaded_paths: I) -> DirResolution
    where
        I: Iterator<Item = &'a String>,
    {
        // Pair each loaded path with its normalized form once.
        let normalized: Vec<(String, &String)> = loaded_paths.map(|p| (normalize(p), p)).collect();

        let find_exact = |candidate: &str| -> Option<String> {
            let cand = normalize(candidate);
            normalized
                .iter()
                .find(|(norm, _)| *norm == cand)
                .map(|(_, orig)| (*orig).clone())
        };

        match self {
            ModulePath::Std => {
                // The standard library's entry point is `_std.rue`. The driver
                // loads it from $RUE_STD_PATH or an adjacent std/ directory
                // (see discover_and_load_imports in the rue crate); here we
                // just match it among the loaded files. Std is not
                // importer-relative, so base_dirs are ignored.
                for (_, orig) in &normalized {
                    if boundary_ends(orig, "_std.rue") {
                        return DirResolution::Resolved((*orig).clone());
                    }
                }
                DirResolution::NotFound
            }
            ModulePath::ExplicitRue { path } => {
                for base in base_dirs {
                    if let Some(hit) = find_exact(&join_base(base, path)) {
                        return DirResolution::Resolved(hit);
                    }
                }
                DirResolution::NotFound
            }
            ModulePath::Simple { path } => {
                let basename = Path::new(path)
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or(path);
                for base in base_dirs {
                    let file_module = find_exact(&join_base(base, &format!("{path}.rue")));
                    let dir_module =
                        find_exact(&join_base(base, &format!("{path}/_{basename}.rue")));
                    match (file_module, dir_module) {
                        (Some(file_module), Some(dir_module)) => {
                            // Both forms present in the SAME base directory:
                            // ambiguous, not a precedence question (E0708),
                            // mirroring Rust's E0761.
                            return DirResolution::Ambiguous {
                                file_module,
                                dir_module,
                            };
                        }
                        (Some(f), None) => return DirResolution::Resolved(f),
                        (None, Some(d)) => return DirResolution::Resolved(d),
                        (None, None) => continue,
                    }
                }
                DirResolution::NotFound
            }
        }
    }
}

/// Whether `candidate` ends with `suffix` at a path boundary (start of string
/// or immediately after a `/`).
fn boundary_ends(candidate: &str, suffix: &str) -> bool {
    candidate.ends_with(suffix) && {
        let prefix_len = candidate.len() - suffix.len();
        prefix_len == 0 || candidate.as_bytes()[prefix_len - 1] == b'/'
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn owned(paths: &[&str]) -> Vec<String> {
        paths.iter().map(|s| s.to_string()).collect()
    }

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
    fn test_resolve_std_not_present() {
        let paths = owned(&["main.rue"]);
        let module = ModulePath::Std;
        assert_eq!(
            module.resolve_in_dirs(&[""], paths.iter()),
            DirResolution::NotFound
        );
    }

    #[test]
    fn test_resolve_std_present() {
        let paths = owned(&["main.rue", "std/_std.rue"]);
        let module = ModulePath::Std;
        assert_eq!(
            module.resolve_in_dirs(&[""], paths.iter()),
            DirResolution::Resolved("std/_std.rue".to_string())
        );
    }

    // =========================================================================
    // Resolution tests - Explicit .rue extension
    // =========================================================================

    #[test]
    fn test_resolve_explicit_in_importer_dir() {
        let paths = owned(&["a/foo.rue", "b/foo.rue"]);
        let module = ModulePath::ExplicitRue {
            path: "foo.rue".to_string(),
        };
        // Importer in `a/` gets a/foo.rue; importer in `b/` gets b/foo.rue.
        assert_eq!(
            module.resolve_in_dirs(&["a"], paths.iter()),
            DirResolution::Resolved("a/foo.rue".to_string())
        );
        assert_eq!(
            module.resolve_in_dirs(&["b"], paths.iter()),
            DirResolution::Resolved("b/foo.rue".to_string())
        );
    }

    #[test]
    fn test_resolve_explicit_no_false_substring_match() {
        // "foo.rue" from base "" should NOT match "xfoo.rue".
        let paths = owned(&["xfoo.rue"]);
        let module = ModulePath::ExplicitRue {
            path: "foo.rue".to_string(),
        };
        assert_eq!(
            module.resolve_in_dirs(&[""], paths.iter()),
            DirResolution::NotFound
        );
    }

    #[test]
    fn test_resolve_explicit_nested_path() {
        let paths = owned(&["proj/utils/strings.rue"]);
        let module = ModulePath::ExplicitRue {
            path: "utils/strings.rue".to_string(),
        };
        assert_eq!(
            module.resolve_in_dirs(&["proj"], paths.iter()),
            DirResolution::Resolved("proj/utils/strings.rue".to_string())
        );
    }

    // =========================================================================
    // Resolution tests - Simple (no extension)
    // =========================================================================

    #[test]
    fn test_resolve_simple_in_importer_dir() {
        let paths = owned(&["foo.rue"]);
        let module = ModulePath::Simple {
            path: "foo".to_string(),
        };
        assert_eq!(
            module.resolve_in_dirs(&[""], paths.iter()),
            DirResolution::Resolved("foo.rue".to_string())
        );
    }

    #[test]
    fn test_resolve_simple_importer_relative_disambiguates() {
        // Face A (RUE-266): two foo.rue in different dirs; each importer gets
        // its OWN foo, not whichever happens to iterate first.
        let paths = owned(&["a/foo.rue", "b/foo.rue"]);
        let module = ModulePath::Simple {
            path: "foo".to_string(),
        };
        assert_eq!(
            module.resolve_in_dirs(&["a"], paths.iter()),
            DirResolution::Resolved("a/foo.rue".to_string())
        );
        assert_eq!(
            module.resolve_in_dirs(&["b"], paths.iter()),
            DirResolution::Resolved("b/foo.rue".to_string())
        );
    }

    #[test]
    fn test_resolve_simple_nested_path() {
        let paths = owned(&["proj/utils/strings.rue"]);
        let module = ModulePath::Simple {
            path: "utils/strings".to_string(),
        };
        assert_eq!(
            module.resolve_in_dirs(&["proj"], paths.iter()),
            DirResolution::Resolved("proj/utils/strings.rue".to_string())
        );
    }

    #[test]
    fn test_resolve_simple_facade_file() {
        // The facade lives INSIDE the directory (RUE-137): utils/_utils.rue.
        let paths = owned(&["utils/_utils.rue"]);
        let module = ModulePath::Simple {
            path: "utils".to_string(),
        };
        assert_eq!(
            module.resolve_in_dirs(&[""], paths.iter()),
            DirResolution::Resolved("utils/_utils.rue".to_string())
        );
    }

    #[test]
    fn test_resolve_simple_sibling_facade_rejected() {
        // The pre-RUE-137 sibling layout is NOT a directory module.
        let paths = owned(&["_utils.rue"]);
        let module = ModulePath::Simple {
            path: "utils".to_string(),
        };
        assert_eq!(
            module.resolve_in_dirs(&[""], paths.iter()),
            DirResolution::NotFound
        );
    }

    #[test]
    fn test_resolve_simple_no_cross_dir_basename_match() {
        // The old program-global resolver matched "math" against "src/math.rue"
        // by bare basename regardless of the importer's directory — the root of
        // Face A. Anchored resolution refuses it: from base "" there is no
        // math.rue.
        let paths = owned(&["src/math.rue"]);
        let module = ModulePath::Simple {
            path: "math".to_string(),
        };
        assert_eq!(
            module.resolve_in_dirs(&[""], paths.iter()),
            DirResolution::NotFound
        );
        // ...but from base "src" it resolves.
        assert_eq!(
            module.resolve_in_dirs(&["src"], paths.iter()),
            DirResolution::Resolved("src/math.rue".to_string())
        );
    }

    #[test]
    fn test_ambiguity_within_one_dir() {
        // Both "foo.rue" and "foo/_foo.rue" in the SAME base dir: ambiguous.
        let paths = owned(&["d/foo/_foo.rue", "d/foo.rue"]);
        let module = ModulePath::Simple {
            path: "foo".to_string(),
        };
        assert_eq!(
            module.resolve_in_dirs(&["d"], paths.iter()),
            DirResolution::Ambiguous {
                file_module: "d/foo.rue".to_string(),
                dir_module: "d/foo/_foo.rue".to_string(),
            }
        );
    }

    #[test]
    fn test_no_false_cross_dir_ambiguity() {
        // Face B (RUE-266): a file module in one dir and a facade in ANOTHER dir
        // are two unrelated, individually-unambiguous imports — not a
        // collision. Each importer resolves within its own dir.
        let paths = owned(&["sub/foo.rue", "facdir/foo/_foo.rue"]);
        let module = ModulePath::Simple {
            path: "foo".to_string(),
        };
        assert_eq!(
            module.resolve_in_dirs(&["sub"], paths.iter()),
            DirResolution::Resolved("sub/foo.rue".to_string())
        );
        assert_eq!(
            module.resolve_in_dirs(&["facdir"], paths.iter()),
            DirResolution::Resolved("facdir/foo/_foo.rue".to_string())
        );
    }

    #[test]
    fn test_root_dir_fallback() {
        // Importer's own dir has no match; the root dir (second base) does
        // (spec 10.2:2).
        let paths = owned(&["shared.rue", "sub/user.rue"]);
        let module = ModulePath::Simple {
            path: "shared".to_string(),
        };
        // base_dirs = [importer "sub", root ""]
        assert_eq!(
            module.resolve_in_dirs(&["sub", ""], paths.iter()),
            DirResolution::Resolved("shared.rue".to_string())
        );
    }

    #[test]
    fn test_importer_dir_takes_precedence_over_root() {
        // A module exists both next to the importer and at the root; the
        // importer-relative one wins (spec 10.2:2).
        let paths = owned(&["foo.rue", "sub/foo.rue"]);
        let module = ModulePath::Simple {
            path: "foo".to_string(),
        };
        assert_eq!(
            module.resolve_in_dirs(&["sub", ""], paths.iter()),
            DirResolution::Resolved("sub/foo.rue".to_string())
        );
    }

    #[test]
    fn test_resolve_simple_no_false_substring_match() {
        // "math" should NOT match "mathematics.rue".
        let paths = owned(&["mathematics.rue"]);
        let module = ModulePath::Simple {
            path: "math".to_string(),
        };
        assert_eq!(
            module.resolve_in_dirs(&[""], paths.iter()),
            DirResolution::NotFound
        );
    }

    #[test]
    fn test_normalized_dot_components_match() {
        // Loaded path spelled with a leading "./" still matches (spec 10.2:4).
        let paths = owned(&["./helper.rue"]);
        let module = ModulePath::Simple {
            path: "helper".to_string(),
        };
        assert_eq!(
            module.resolve_in_dirs(&["."], paths.iter()),
            DirResolution::Resolved("./helper.rue".to_string())
        );
    }

    // =========================================================================
    // Edge case tests
    // =========================================================================

    #[test]
    fn test_resolve_not_found() {
        let paths = owned(&["other.rue"]);
        let module = ModulePath::Simple {
            path: "foo".to_string(),
        };
        assert_eq!(
            module.resolve_in_dirs(&[""], paths.iter()),
            DirResolution::NotFound
        );
    }

    #[test]
    fn test_resolve_empty_paths() {
        let paths: Vec<String> = vec![];
        let module = ModulePath::Simple {
            path: "foo".to_string(),
        };
        assert_eq!(
            module.resolve_in_dirs(&[""], paths.iter()),
            DirResolution::NotFound
        );
    }
}
