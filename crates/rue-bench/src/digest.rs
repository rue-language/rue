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
