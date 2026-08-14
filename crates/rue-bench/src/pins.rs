//! Resolving what was actually measured.
//!
//! The epoch *declares* content hashes; this module *resolves* them from the
//! tree in front of it. Validation then compares the two. That is the whole
//! mechanism by which a configuration-invalid observation becomes unappendable
//! rather than merely detectable: a workload edit changes its resolved hash,
//! the comparison fails, and appending requires a maintainer to declare the
//! next suite revision deliberately.
//!
//! Resolving must therefore never consult the epoch. A resolver that fell back
//! to the declared value on error would defeat the check it exists to feed.
//!
//! Not everything resolved here is compared. [`stdlib_hash`] is recorded on the
//! run and gates nothing: `std` is part of the product being measured, so a
//! change to it moves the series rather than invalidating it, and the dashboard
//! annotates where that happened. [`workload_source_hash`] excludes std reads
//! for the same reason. See ADR-0067, "The product boundary".

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use sha2::{Digest, Sha256};

/// A source closure could not be resolved.
#[derive(Debug)]
pub struct PinError {
    /// What was being resolved.
    pub subject: String,
    /// Why it failed.
    pub detail: String,
}

impl std::fmt::Display for PinError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "could not resolve {}: {}", self.subject, self.detail)
    }
}

/// Hash a single file's contents.
fn hash_file(path: &Path) -> Result<String, PinError> {
    let bytes = std::fs::read(path).map_err(|error| PinError {
        subject: path.display().to_string(),
        detail: error.to_string(),
    })?;
    Ok(hex(Sha256::digest(&bytes).as_slice()))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Fold a sorted set of `(name, content hash)` pairs into one hash.
///
/// Names are included, and lengths are framed, so that renaming a file or
/// splitting one into two changes the result. Concatenating unframed values
/// would let `("ab", "c")` and `("a", "bc")` collide.
fn fold(entries: &BTreeMap<String, String>) -> String {
    let mut hasher = Sha256::new();
    for (name, content) in entries {
        hasher.update(name.len().to_le_bytes());
        hasher.update(name.as_bytes());
        hasher.update(content.len().to_le_bytes());
        hasher.update(content.as_bytes());
    }
    hex(hasher.finalize().as_slice())
}

/// Hash of the toolchain the compiler was built with.
///
/// Deliberately *not* the compiler binary. The compiler revision is a point
/// within a series, never part of its identity, so hashing the artifact under
/// test would make every commit fail its own epoch's pin check and no series
/// could ever contain two points.
///
/// What must stay fixed is the build environment: the pinned Rust toolchain.
/// Changing it changes generated code and therefore measurements, and it
/// changes rarely and deliberately — exactly the profile a pin wants.
pub fn toolchain_hash(repo_root: &Path) -> Result<String, PinError> {
    hash_file(&repo_root.join("rust-toolchain.toml"))
}

/// Hash of the standard library's source tree.
///
/// Walks deterministically and keys each file by its path relative to the
/// standard-library root, so the hash does not depend on where the checkout
/// lives.
pub fn stdlib_hash(std_root: &Path) -> Result<String, PinError> {
    let mut entries = BTreeMap::new();
    collect_tree(std_root, std_root, &mut entries)?;
    Ok(fold(&entries))
}

fn collect_tree(
    root: &Path,
    directory: &Path,
    entries: &mut BTreeMap<String, String>,
) -> Result<(), PinError> {
    let listing = std::fs::read_dir(directory).map_err(|error| PinError {
        subject: directory.display().to_string(),
        detail: error.to_string(),
    })?;
    for entry in listing {
        let entry = entry.map_err(|error| PinError {
            subject: directory.display().to_string(),
            detail: error.to_string(),
        })?;
        let path = entry.path();
        if path.is_dir() {
            collect_tree(root, &path, entries)?;
            continue;
        }
        let relative = path
            .strip_prefix(root)
            .unwrap_or(&path)
            .to_string_lossy()
            .into_owned();
        entries.insert(relative, hash_file(&path)?);
    }
    Ok(())
}

/// The compiler's dependency report, of which only the resolved read set
/// matters here.
#[derive(serde::Deserialize)]
struct DependencyReport {
    status: String,
    accepted_reads: Vec<AcceptedRead>,
}

#[derive(serde::Deserialize)]
struct AcceptedRead {
    module: String,
    canonical_path: PathBuf,
}

/// Hash a workload's own transitive source closure, excluding the standard
/// library.
///
/// The closure comes from the compiler's own `--emit deps`, so it is the set
/// the compiler would actually read rather than a guess at the import graph.
/// Contents are then hashed here with SHA-256 rather than trusting the report's
/// own 64-bit fingerprints, which are sized for change detection inside one
/// process rather than for identity across machines.
///
/// Files are keyed by module path, which is relative to the project root, so
/// two checkouts of the same tree at different absolute paths resolve to the
/// same hash.
///
/// Standard-library reads are excluded deliberately. `std` is part of the
/// product being measured, not an input pinned against it, so a std edit must
/// move the series the way a compiler change does. Folding std into this hash
/// would invalidate every series whose workload reads it, and would invalidate
/// a std-compiling workload on every std commit.
pub fn workload_source_hash(
    compiler: &Path,
    source: &Path,
    std_root: Option<&Path>,
) -> Result<String, PinError> {
    let subject = source.display().to_string();
    let mut command = Command::new(compiler);
    crate::environment::sanitize_compiler_environment(&mut command);
    command.arg("--emit").arg("deps").arg(source);
    if let Some(std_root) = std_root {
        command.env("RUE_STD_PATH", std_root);
    }
    let output = command.output().map_err(|error| PinError {
        subject: subject.clone(),
        detail: format!("could not run the compiler: {error}"),
    })?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let report: DependencyReport =
        serde_json::from_str(stdout.trim()).map_err(|error| PinError {
            subject: subject.clone(),
            detail: format!(
                "could not read the dependency report ({error}); stderr: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ),
        })?;

    // A partial closure would hash to something stable but wrong, and every
    // later run would silently agree with it.
    if report.status != "complete" {
        return Err(PinError {
            subject,
            detail: format!("dependency discovery reported status {:?}", report.status),
        });
    }

    let std_root = std_root.map(resolve_for_comparison);
    let mut entries = BTreeMap::new();
    let mut standard_library_reads = 0usize;
    for read in &report.accepted_reads {
        if is_stdlib_read(&read.canonical_path, std_root.as_deref()) {
            standard_library_reads += 1;
            continue;
        }
        entries.insert(read.module.clone(), hash_file(&read.canonical_path)?);
    }
    if entries.is_empty() {
        return Err(PinError {
            subject,
            detail: if standard_library_reads > 0 {
                // Distinguished from "no reads at all": a workload whose whole
                // closure is std has no identity of its own to pin, which is a
                // corpus mistake rather than a discovery failure.
                format!(
                    "dependency discovery accepted {standard_library_reads} read(s), all within \
                     the standard library, so the workload has no source identity of its own"
                )
            } else {
                "dependency discovery accepted no reads".to_string()
            },
        });
    }
    Ok(fold(&entries))
}

/// Whether a resolved read belongs to the standard library rather than to the
/// workload itself.
fn is_stdlib_read(canonical_path: &Path, std_root: Option<&Path>) -> bool {
    std_root.is_some_and(|root| canonical_path.starts_with(root))
}

/// Resolve a path so it can be prefix-compared against the dependency report.
///
/// The report's paths are canonical, so the standard-library root must be too.
/// A relative or symlinked `--std-root` would otherwise fail every comparison
/// and silently fold the whole standard library back into the workload's
/// identity, which looks like nothing at all until the next std edit.
fn resolve_for_comparison(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folding_is_order_independent_but_content_sensitive() {
        let mut one = BTreeMap::new();
        one.insert("a.rue".to_string(), "aaa".to_string());
        one.insert("b.rue".to_string(), "bbb".to_string());

        let mut reversed = BTreeMap::new();
        reversed.insert("b.rue".to_string(), "bbb".to_string());
        reversed.insert("a.rue".to_string(), "aaa".to_string());

        assert_eq!(fold(&one), fold(&reversed));

        let mut changed = one.clone();
        changed.insert("b.rue".to_string(), "ccc".to_string());
        assert_ne!(fold(&one), fold(&changed));
    }

    #[test]
    fn renaming_a_file_changes_the_closure_hash() {
        // Names participate, so moving a module is a source-identity change and
        // therefore needs a new suite revision.
        let mut original = BTreeMap::new();
        original.insert("util.rue".to_string(), "aaa".to_string());
        let mut renamed = BTreeMap::new();
        renamed.insert("helper.rue".to_string(), "aaa".to_string());
        assert_ne!(fold(&original), fold(&renamed));
    }

    #[test]
    fn framing_prevents_boundary_collisions() {
        // Without length framing these two would concatenate identically.
        let mut left = BTreeMap::new();
        left.insert("ab".to_string(), "c".to_string());
        let mut right = BTreeMap::new();
        right.insert("a".to_string(), "bc".to_string());
        assert_ne!(fold(&left), fold(&right));
    }

    #[test]
    fn adding_a_file_changes_the_closure_hash() {
        let mut before = BTreeMap::new();
        before.insert("main.rue".to_string(), "aaa".to_string());
        let mut after = before.clone();
        after.insert("extra.rue".to_string(), "bbb".to_string());
        assert_ne!(fold(&before), fold(&after));
    }

    #[test]
    fn hashing_a_missing_file_is_an_error_rather_than_a_default() {
        // Silently hashing "nothing" would make a deleted workload resolve to a
        // stable value that every later run would agree with.
        let error = hash_file(Path::new("/definitely/not/here.rue")).unwrap_err();
        assert!(error.to_string().contains("could not resolve"));
    }

    #[test]
    fn a_stdlib_hash_is_stable_and_covers_its_files() {
        let directory = tempfile::tempdir().expect("temp dir");
        std::fs::write(directory.path().join("a.rue"), "fn a() {}").unwrap();
        std::fs::create_dir(directory.path().join("nested")).unwrap();
        std::fs::write(directory.path().join("nested/b.rue"), "fn b() {}").unwrap();

        let first = stdlib_hash(directory.path()).unwrap();
        assert_eq!(first, stdlib_hash(directory.path()).unwrap());

        std::fs::write(directory.path().join("nested/b.rue"), "fn b() { 1 }").unwrap();
        assert_ne!(first, stdlib_hash(directory.path()).unwrap());
    }

    #[test]
    fn standard_library_reads_are_excluded_from_a_workloads_identity() {
        let std_root = Path::new("/repo/std");
        assert!(is_stdlib_read(
            Path::new("/repo/std/math.rue"),
            Some(std_root)
        ));
        assert!(is_stdlib_read(
            Path::new("/repo/std/nested/deep.rue"),
            Some(std_root)
        ));
        assert!(!is_stdlib_read(
            Path::new("/repo/performance/workloads/startup/main.rue"),
            Some(std_root)
        ));
    }

    #[test]
    fn a_sibling_directory_sharing_a_prefix_is_not_the_standard_library() {
        // Component-wise, not textual: `/repo/stdlib-tests` merely starts with
        // the same characters as `/repo/std`, and excluding it would silently
        // drop real workload sources from the hash.
        assert!(!is_stdlib_read(
            Path::new("/repo/stdlib-tests/main.rue"),
            Some(Path::new("/repo/std"))
        ));
    }

    #[test]
    fn without_a_standard_library_root_nothing_is_excluded() {
        assert!(!is_stdlib_read(Path::new("/repo/std/math.rue"), None));
    }

    #[test]
    fn the_standard_library_root_is_resolved_before_comparison() {
        // The dependency report's paths are canonical. A relative std root that
        // stayed relative would match nothing, folding std back into every
        // workload's identity without any visible symptom until a std edit.
        let directory = tempfile::tempdir().expect("temp dir");
        let std_root = directory.path().join("std");
        std::fs::create_dir(&std_root).unwrap();

        let resolved = resolve_for_comparison(&std_root);
        assert!(resolved.is_absolute());
        assert!(is_stdlib_read(
            &resolved.join("math.rue"),
            Some(resolved.as_path())
        ));
    }

    #[test]
    fn a_path_that_cannot_be_resolved_is_kept_as_given() {
        // Falling back rather than failing: an unresolvable std root is not
        // this function's error to report.
        let missing = Path::new("/definitely/not/here");
        assert_eq!(resolve_for_comparison(missing), missing.to_path_buf());
    }

    #[test]
    fn a_stdlib_hash_does_not_depend_on_where_the_checkout_lives() {
        let make = |root: &Path| {
            std::fs::write(root.join("a.rue"), "fn a() {}").unwrap();
            stdlib_hash(root).unwrap()
        };
        let one = tempfile::tempdir().expect("temp dir");
        let two = tempfile::tempdir().expect("temp dir");
        assert_eq!(make(one.path()), make(two.path()));
    }
}
