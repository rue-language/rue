//! Stable, domain-separated content identities for immutable compiler artifacts.

use sha2::{Digest, Sha256};

/// Collision-resistant, deterministic content identity.
pub(crate) type ContentDigest = [u8; 32];

/// SHA-256 over a domain-separated, length-framed byte encoding.
pub(crate) fn bytes_digest(domain: &[u8], bytes: &[u8]) -> ContentDigest {
    let mut digest = Sha256::new();
    digest.update(domain);
    digest.update((bytes.len() as u64).to_le_bytes());
    digest.update(bytes);
    digest.finalize().into()
}
