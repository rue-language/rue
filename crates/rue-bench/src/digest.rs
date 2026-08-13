//! The one spelling of a digest this runner writes.
//!
//! Every record kind names things it cannot contain — a fixture, a binary, a
//! program's output — by SHA-256, and validation checks that name against a
//! fixed format: 64 lowercase hexadecimal characters. Three copies of the
//! formatting would be three chances to write a digest some validator will
//! later refuse, so there is one.

use std::path::Path;

use sha2::{Digest, Sha256};

/// Digest some bytes, lowercase hexadecimal.
pub fn sha256_bytes(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// Digest a file's contents.
pub fn sha256_file(path: &Path) -> Result<String, String> {
    std::fs::read(path)
        .map(|bytes| sha256_bytes(&bytes))
        .map_err(|error| format!("could not hash {}: {error}", path.display()))
}

/// Every file in a tree, by relative path, size, and contents.
///
/// The name of an artifact a workload EMITS rather than one it reads, so it
/// answers the questions such an artifact raises: paths are hashed, so a file
/// that only moved changes the digest, and sizes are hashed, so no file can be
/// truncated to another's bytes unnoticed. One traversal order — sorted
/// relative paths — because two orderings would be two digests for one tree.
pub fn sha256_tree(root: &Path) -> Result<String, String> {
    let mut files = Vec::new();
    collect(root, root, &mut files)?;
    files.sort();
    let mut hasher = Sha256::new();
    for relative in &files {
        let path = root.join(relative);
        let bytes = std::fs::read(&path)
            .map_err(|error| format!("could not hash {}: {error}", path.display()))?;
        hasher.update(relative.as_bytes());
        hasher.update(format!("\0{}\0", bytes.len()).as_bytes());
        hasher.update(&bytes);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

/// How many files a tree holds, for the structural checks a record carries.
pub fn count_tree(root: &Path) -> Result<u64, String> {
    let mut files = Vec::new();
    collect(root, root, &mut files)?;
    Ok(files.len() as u64)
}

fn collect(root: &Path, directory: &Path, found: &mut Vec<String>) -> Result<(), String> {
    let entries = std::fs::read_dir(directory)
        .map_err(|error| format!("could not read {}: {error}", directory.display()))?;
    for entry in entries {
        let entry = entry.map_err(|error| format!("could not read a directory entry: {error}"))?;
        let path = entry.path();
        let kind = entry
            .file_type()
            .map_err(|error| format!("could not stat {}: {error}", path.display()))?;
        if kind.is_dir() {
            collect(root, &path, found)?;
        } else {
            let relative = path
                .strip_prefix(root)
                .map_err(|_| format!("{} escaped its root", path.display()))?;
            found.push(relative.to_string_lossy().replace('\\', "/"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digests_are_lowercase_hex_of_the_expected_width() {
        // The format validation checks for. A `{:X}` here would produce digests
        // every validator in the system rejects.
        let digest = sha256_bytes(b"");
        assert_eq!(
            digest,
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(digest.len(), 64);
        assert!(
            digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        );
    }

    #[test]
    fn hashing_a_missing_file_is_an_error_rather_than_a_default() {
        assert!(sha256_file(Path::new("definitely-not-a-real-file-xyz")).is_err());
    }
}
