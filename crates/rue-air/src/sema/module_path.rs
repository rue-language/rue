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

use std::path::Path;
use std::{fs, io};

use crate::path_norm::normalize_module_path as normalize;

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

    /// Build ordered filesystem candidate groups for this import path.
    ///
    /// Each inner group contains candidates that should be considered
    /// together within one precedence level. If any candidate in a group
    /// exists, later groups must not be probed. This lets the CLI load both
    /// `foo.rue` and `foo/_foo.rue` from the same base directory so sema can
    /// report the ambiguity, while still preserving importer-dir-before-root
    /// precedence.
    ///
    /// `std_dir`, when present, is the `$RUE_STD_PATH` directory and is probed
    /// before importer/root-relative stdlib facades.
    pub fn candidate_groups(
        &self,
        base_dirs: &[String],
        std_dir: Option<&str>,
    ) -> Vec<Vec<String>> {
        let mut groups = Vec::new();

        match self {
            ModulePath::Std => {
                if let Some(std_dir) = std_dir {
                    groups.push(vec![join_base(std_dir, "_std.rue")]);
                }
                for base in base_dirs {
                    groups.push(vec![join_base(base, "std/_std.rue")]);
                }
            }
            ModulePath::ExplicitRue { path } => {
                for base in base_dirs {
                    groups.push(vec![join_base(base, path)]);
                }
            }
            ModulePath::Simple { path } => {
                let basename = Path::new(path)
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or(path);
                for base in base_dirs {
                    groups.push(vec![
                        join_base(base, &format!("{path}.rue")),
                        join_base(base, &format!("{path}/_{basename}.rue")),
                    ]);
                }
            }
        }

        groups
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
    /// components and collapsing `..`), so `./foo.rue` and `foo.rue`, or a
    /// `../`-relative import and a command-line-listed file, are the same file
    /// (spec 10.2:4, RUE-317). The returned string is the original loaded path.
    pub fn resolve_in_dirs<'a, I>(&self, base_dirs: &[&str], loaded_paths: I) -> DirResolution
    where
        I: Iterator<Item = &'a String>,
    {
        self.resolve_in_dirs_with_std_dir(base_dirs, loaded_paths, None)
    }

    /// Like [`Self::resolve_in_dirs`], but lets callers provide the
    /// `$RUE_STD_PATH` directory used by the driver when resolving
    /// `@import("std")`.
    pub fn resolve_in_dirs_with_std_dir<'a, I>(
        &self,
        base_dirs: &[&str],
        loaded_paths: I,
        std_dir: Option<&str>,
    ) -> DirResolution
    where
        I: Iterator<Item = &'a String>,
    {
        // Pair each loaded path with its normalized form once. When the path
        // exists on disk, also keep its filesystem-canonical identity so
        // relative/absolute aliases and symlinks still resolve to the one
        // already-loaded module (RUE-360). The lexical key remains the primary
        // comparison so pure in-memory tests and virtual paths keep working.
        let normalized: Vec<(String, Option<String>, &String)> = loaded_paths
            .map(|p| (normalize(p), canonical_key(p), p))
            .collect();

        let find_exact = |candidate: &str| -> Option<String> {
            let cand = normalize(candidate);
            let cand_canonical = canonical_key(candidate);
            normalized
                .iter()
                .find(|(norm, canonical, _)| {
                    *norm == cand
                        || matches!(
                            (cand_canonical.as_ref(), canonical.as_ref()),
                            (Some(a), Some(b)) if a == b
                        )
                })
                .map(|(_, _, orig)| (*orig).clone())
        };

        match self {
            ModulePath::Std => {
                // Match only the standard-library candidates the driver would
                // have loaded, not any unrelated loaded file named `_std.rue`
                // (RUE-493).
                let base_dirs = base_dirs
                    .iter()
                    .map(|base| (*base).to_string())
                    .collect::<Vec<_>>();
                for group in self.candidate_groups(&base_dirs, std_dir) {
                    for candidate in group {
                        if let Some(hit) = find_exact(&candidate) {
                            return DirResolution::Resolved(hit);
                        }
                    }
                }
                DirResolution::NotFound
            }
            ModulePath::ExplicitRue { .. } => {
                let base_dirs = base_dirs
                    .iter()
                    .map(|base| (*base).to_string())
                    .collect::<Vec<_>>();
                for group in self.candidate_groups(&base_dirs, None) {
                    let candidate = &group[0];
                    if let Some(hit) = find_exact(candidate) {
                        return DirResolution::Resolved(hit);
                    }
                }
                DirResolution::NotFound
            }
            ModulePath::Simple { .. } => {
                let base_dirs = base_dirs
                    .iter()
                    .map(|base| (*base).to_string())
                    .collect::<Vec<_>>();
                for group in self.candidate_groups(&base_dirs, None) {
                    let file_module = find_exact(&group[0]);
                    let dir_module = find_exact(&group[1]);
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

/// Build ordered filesystem candidate groups for an import path string.
///
/// This is the public driver-facing entry point for the same candidate order
/// used by [`ModulePath::resolve_in_dirs`].
pub fn import_candidate_groups(
    import_path: &str,
    base_dirs: &[String],
    std_dir: Option<&str>,
) -> Vec<Vec<String>> {
    ModulePath::parse(import_path).candidate_groups(base_dirs, std_dir)
}

fn canonical_key(path: &str) -> Option<String> {
    match fs::canonicalize(path) {
        Ok(path) => Some(path.to_string_lossy().into_owned()),
        Err(err) if err.kind() == io::ErrorKind::NotFound => None,
        Err(_) => None,
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
    // Candidate group tests
    // =========================================================================

    #[test]
    fn test_candidate_groups_explicit_rue() {
        assert_eq!(
            import_candidate_groups("foo.rue", &owned(&["a", ""]), None),
            vec![vec!["a/foo.rue".to_string()], vec!["foo.rue".to_string()]]
        );
    }

    #[test]
    fn test_candidate_groups_simple_groups_file_and_facade_per_base() {
        assert_eq!(
            import_candidate_groups("foo", &owned(&["a", ""]), None),
            vec![
                vec!["a/foo.rue".to_string(), "a/foo/_foo.rue".to_string()],
                vec!["foo.rue".to_string(), "foo/_foo.rue".to_string()],
            ]
        );
    }

    #[test]
    fn test_candidate_groups_nested_simple_uses_leaf_facade_name() {
        assert_eq!(
            import_candidate_groups("utils/strings", &owned(&["src"]), None),
            vec![vec![
                "src/utils/strings.rue".to_string(),
                "src/utils/strings/_strings.rue".to_string(),
            ]]
        );
    }

    #[test]
    fn test_candidate_groups_std_prefers_installed_facade() {
        assert_eq!(
            import_candidate_groups("std", &owned(&["a", ""]), Some("/opt/rue/std")),
            vec![
                vec!["/opt/rue/std/_std.rue".to_string()],
                vec!["a/std/_std.rue".to_string()],
                vec!["std/_std.rue".to_string()],
            ]
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

    #[test]
    fn test_resolve_std_ignores_unrelated_loaded_std_facade_name() {
        let paths = owned(&["main.rue", "helper/_std.rue"]);
        let module = ModulePath::Std;
        assert_eq!(
            module.resolve_in_dirs(&[""], paths.iter()),
            DirResolution::NotFound
        );
    }

    #[test]
    fn test_resolve_std_accepts_explicit_std_dir_candidate() {
        let paths = owned(&["main.rue", "/opt/rue/lib/_std.rue"]);
        let module = ModulePath::Std;
        assert_eq!(
            module.resolve_in_dirs_with_std_dir(&[""], paths.iter(), Some("/opt/rue/lib")),
            DirResolution::Resolved("/opt/rue/lib/_std.rue".to_string())
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

    #[test]
    fn test_filesystem_canonical_alias_matches_loaded_file() {
        let root = temp_test_dir("rue_module_alias_abs");
        let lib = root.join("lib.rue");
        std::fs::write(&lib, "pub fn v() -> i32 { 3 }").unwrap();

        let loaded_spelling = lib.canonicalize().unwrap().to_string_lossy().into_owned();
        let paths = vec![loaded_spelling.clone()];
        let module = ModulePath::ExplicitRue {
            path: "lib.rue".to_string(),
        };

        assert_eq!(
            module.resolve_in_dirs(&[root.to_str().unwrap()], paths.iter()),
            DirResolution::Resolved(loaded_spelling)
        );

        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn test_filesystem_canonical_symlink_alias_matches_loaded_file() {
        use std::os::unix::fs::symlink;

        let root = temp_test_dir("rue_module_alias_symlink");
        let lib = root.join("lib.rue");
        let link = root.join("link.rue");
        std::fs::write(&lib, "pub fn v() -> i32 { 3 }").unwrap();
        symlink(&lib, &link).unwrap();

        let loaded_spelling = lib.canonicalize().unwrap().to_string_lossy().into_owned();
        let paths = vec![loaded_spelling.clone()];
        let module = ModulePath::ExplicitRue {
            path: "link.rue".to_string(),
        };

        assert_eq!(
            module.resolve_in_dirs(&[root.to_str().unwrap()], paths.iter()),
            DirResolution::Resolved(loaded_spelling)
        );

        let _ = std::fs::remove_dir_all(root);
    }

    fn temp_test_dir(name: &str) -> std::path::PathBuf {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "{name}_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&path).unwrap();
        path
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
