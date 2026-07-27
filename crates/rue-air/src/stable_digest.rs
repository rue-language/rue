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

/// The stable-content string of one INSTALLED definition endpoint:
/// `D\u{1}{module_path}\u{1}{name}\u{1}{owner}\u{1}{kind}`, with an absent
/// owner rendered as the empty string.
///
/// This is the single assembly of the definition-component format both
/// relocation paths feed into [`stable_anonymous_identity_digest`]: the
/// semantic epoch's `stable_definition_symbol_component` (`anon_structs.rs`,
/// installed-endpoint arm) and the provider durable adapter that formats the
/// same four parts from a durable definition key. Every byte here is
/// digest-critical — two paths spelling the same producer must hash the same
/// content. The epoch's session-local `d\u{1}…` fallback is a separate,
/// deliberately non-durable namespace and is not assembled here.
pub fn stable_definition_component(
    module_path: &str,
    name: &str,
    owner: Option<&str>,
    kind: u8,
) -> String {
    format!(
        "D\u{1}{module_path}\u{1}{name}\u{1}{}\u{1}{kind}",
        owner.unwrap_or("")
    )
}

/// The stable-content string of one INSTALLED module endpoint:
/// `M\u{1}{module_path}`. The module analog of
/// [`stable_definition_component`], shared by the same two relocation paths;
/// the epoch's session-local `m\u{1}…` fallback is not assembled here.
pub fn stable_module_component(module_path: &str) -> String {
    format!("M\u{1}{module_path}")
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

    use super::{
        stable_anonymous_identity_digest, stable_definition_component, stable_module_component,
    };
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

    /// The installed-endpoint component encodings, pinned byte-for-byte: the
    /// `D`/`M` formats are digest inputs shared by the epoch and the durable
    /// adapter, so any byte change here is an identity break, not a refactor.
    #[test]
    fn stable_symbol_component_encodings_are_pinned() {
        // Owner-absent definition: the owner slot renders as the empty string
        // between its two separators.
        assert_eq!(
            stable_definition_component("pkg/m.rue", "make", None, 3),
            "D\u{1}pkg/m.rue\u{1}make\u{1}\u{1}3"
        );
        // Owner-present definition (an owned method / associated definition).
        assert_eq!(
            stable_definition_component("pkg/m.rue", "get", Some("Holder"), 4),
            "D\u{1}pkg/m.rue\u{1}get\u{1}Holder\u{1}4"
        );
        // Module component.
        assert_eq!(stable_module_component("pkg/m.rue"), "M\u{1}pkg/m.rue");
    }
}
