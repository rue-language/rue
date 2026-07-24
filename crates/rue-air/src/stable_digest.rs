//! Stable digests for durable AIR identities.
//!
//! During the RUE-1091 flip, both the semantic epoch and the provider-era
//! identity pool must derive anonymous nominal display names through this
//! module. Keeping the stable-content encoding here prevents the two paths
//! from drifting while the provider replaces the epoch.

use std::hash::{Hash, Hasher};

use crate::AnonymousNominalKey;

/// A fixed-seed FNV-1a 128-bit hasher.
///
/// Unlike the standard-library `DefaultHasher`, its algorithm and seed are
/// pinned in source, so the digest of one byte stream is identical across every
/// compile of the same program — warm, fresh, or differently scheduled. It is
/// used only to spell stable anonymous-symbol names; it is not a cryptographic
/// hash.
struct StableFnv1a128(u128);

impl StableFnv1a128 {
    /// The 128-bit FNV-1a offset basis.
    const OFFSET_BASIS: u128 = 0x6c62_272e_07bb_0142_62b8_2175_6295_c58d;
    /// The 128-bit FNV-1a prime.
    const PRIME: u128 = 0x0000_0000_0100_0000_0000_0000_0000_013b;

    fn new() -> Self {
        Self(Self::OFFSET_BASIS)
    }

    fn digest(self) -> u128 {
        self.0
    }
}

impl Hasher for StableFnv1a128 {
    fn finish(&self) -> u64 {
        // Truncation is never used for identity; `digest()` reads all 128 bits.
        self.0 as u64
    }

    fn write(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            self.0 ^= u128::from(byte);
            self.0 = self.0.wrapping_mul(Self::PRIME);
        }
    }
}

/// Computes the canonical digest of a stable-content anonymous nominal key.
///
/// Callers must first relocate any session-local definition and module tokens
/// to their stable string content. This function is the single encoding and
/// digest path shared by the semantic epoch and the provider-era identity pool
/// during the RUE-1091 flip.
pub fn stable_anonymous_identity_digest(identity: &AnonymousNominalKey<String, String>) -> u128 {
    let mut hasher = StableFnv1a128::new();
    identity.hash(&mut hasher);
    hasher.digest()
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use rue_rir::{RirStructuralAnchor, RirStructuralPathSegment};

    use super::stable_anonymous_identity_digest;
    use crate::{
        AnonymousNominalKey, AnonymousNominalKind, CanonicalArgumentValue, CanonicalArguments,
        StableProducerId,
    };

    #[test]
    fn anonymous_identity_digest_encoding_is_stable() {
        let identity = AnonymousNominalKey {
            kind: AnonymousNominalKind::Struct,
            producer: StableProducerId::Definition("root::make".to_string()),
            anchor: RirStructuralAnchor::new(vec![
                RirStructuralPathSegment::Body,
                RirStructuralPathSegment::AnonymousType(2),
            ]),
            arguments: CanonicalArguments {
                types: Arc::new([]),
                values: Arc::new([
                    CanonicalArgumentValue::Integer(42),
                    CanonicalArgumentValue::Bool(true),
                    CanonicalArgumentValue::String(Arc::from("rue")),
                ]),
            },
        };

        assert_eq!(
            stable_anonymous_identity_digest(&identity),
            0xdf10_a209_9a1d_9f9b_e7ee_009c_9e2d_4bfd
        );
    }
}
