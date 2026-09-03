//! The declared test-candidate list supplied by `--test-candidates`
//! (ADR-0083, "The boundary").
//!
//! Everything about discovery, analysis, and execution lives in the compiler.
//! Build integration adds exactly one optional input: the set of files a build
//! target declared, which the `rue_program` rule writes from its `srcs`. It
//! powers the unimported-test-file warning and nothing else — with no list, that
//! warning degrades to a one-line notice and no other behavior changes.
//!
//! The derived source manifest cannot serve as this list. It omits files nothing
//! read — exactly the orphans the warning is about — and it contains all of std.
//! So the list is the declared `srcs` set, read here and handed to the compiler
//! as candidates rather than as sources.
//!
//! The file is UTF-8 text, one project-root-relative path per line. Blank lines
//! and `#` comments are ignored, matching `--source-manifest`'s line grammar so
//! a build rule can write either file the same way.

use std::fs;

use crate::source_loader::parse_source_manifest_entry;

/// Read the declared candidate paths from a `--test-candidates` file.
///
/// The list is a build-system input, so a missing or unreadable file is an
/// ordinary driver error naming the path — never a silent empty list. Silently
/// treating an unreadable list as "no candidates declared" would turn a broken
/// build rule into a warning that never fires.
pub fn load_declared_candidates(path: &str) -> Result<Vec<String>, String> {
    let content = fs::read_to_string(path)
        .map_err(|error| format!("Error reading test candidates '{}': {}", path, error))?;
    let mut candidates = Vec::new();
    for raw_line in content.lines() {
        let entry = parse_source_manifest_entry(raw_line);
        if entry.is_empty() {
            continue;
        }
        candidates.push(entry);
    }
    // A repeated `srcs` entry is one declared candidate, not two observations.
    candidates.sort();
    candidates.dedup();
    Ok(candidates)
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("rue-{name}-{}-{unique}", std::process::id()));
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn write(dir: &Path, name: &str, contents: &str) -> String {
        let path = dir.join(name);
        fs::write(&path, contents).unwrap();
        path.to_str().unwrap().to_owned()
    }

    #[test]
    fn ignores_blank_lines_and_comments() {
        let dir = scratch("candidate-list-comments");
        let list = write(
            &dir,
            "test-candidates.list",
            "# declared by //app:app\napp/main.rue\n\n  app/parser_tests.rue  \napp/x.rue # trailing\n",
        );
        assert_eq!(
            load_declared_candidates(&list).unwrap(),
            vec![
                "app/main.rue".to_owned(),
                "app/parser_tests.rue".to_owned(),
                "app/x.rue".to_owned(),
            ]
        );
    }

    #[test]
    fn deduplicates_repeated_entries() {
        let dir = scratch("candidate-list-dedup");
        let list = write(
            &dir,
            "test-candidates.list",
            "app/a.rue\napp/a.rue\napp/b.rue\n",
        );
        assert_eq!(
            load_declared_candidates(&list).unwrap(),
            vec!["app/a.rue".to_owned(), "app/b.rue".to_owned()]
        );
    }

    #[test]
    fn missing_file_names_the_path() {
        let dir = scratch("candidate-list-missing");
        let missing = dir.join("absent.list");
        let error = load_declared_candidates(missing.to_str().unwrap()).unwrap_err();
        assert!(
            error.contains("absent.list"),
            "the error must name the unreadable list: {error}"
        );
        assert!(
            error.starts_with("Error reading test candidates"),
            "{error}"
        );
    }
}
