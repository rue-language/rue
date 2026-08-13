//! Rules every measurement record kind answers to.
//!
//! These are not rules about what a particular record *contains* — those belong
//! with the record — but about what a *measurement* is. A sample that took zero
//! time did not happen. An observation carrying more samples than its policy
//! permits was not taken under that policy. A digest that is not a digest names
//! nothing, and an instant with two spellings gives one measurement two content
//! addresses.
//!
//! ADR-0067 settled each of these for the compile-time suites. ADR-0072's
//! runtime records answer to exactly the same ones, so they are decided here
//! once rather than in each validator, where the two copies would drift and the
//! looser one would be the one admitting a bad record into a store that cannot
//! delete it.

/// Accepts exactly `YYYY-MM-DDTHH:MM:SSZ`.
///
/// One spelling, so two identical measurements cannot produce two content
/// addresses by formatting the same instant differently.
pub fn is_utc_timestamp(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() != 20 {
        return false;
    }
    let digits = [0, 1, 2, 3, 5, 6, 8, 9, 11, 12, 14, 15, 17, 18];
    let dashes = [4, 7];
    let colons = [13, 16];
    digits.iter().all(|&index| bytes[index].is_ascii_digit())
        && dashes.iter().all(|&index| bytes[index] == b'-')
        && colons.iter().all(|&index| bytes[index] == b':')
        && bytes[10] == b'T'
        && bytes[19] == b'Z'
}

/// Whether a measurement's start and end are in that order.
///
/// Both timestamps must already be well-formed; the fixed-width, zero-padded
/// spelling [`is_utc_timestamp`] enforces makes lexicographic order the same as
/// chronological order, which is why this is a string comparison.
///
/// Not pedantry: consumers order points by completion time, so a record whose
/// interval runs backwards would place itself arbitrarily in its own series.
pub fn is_ordered_interval(started_at: &str, finished_at: &str) -> bool {
    is_utc_timestamp(started_at) && is_utc_timestamp(finished_at) && started_at <= finished_at
}

/// Whether a value is a SHA-256 digest in the one spelling records use:
/// 64 lowercase hexadecimal characters.
///
/// Digests are how a record names things it cannot contain — the input it read,
/// the binary it ran, the output it produced. A digest that is merely non-empty
/// names nothing and compares equal to nothing, so checking only for emptiness
/// admits a record that looks segmentable and is not.
pub fn is_sha256_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// Whether a value is a 40-character hexadecimal source revision.
pub fn is_commit(value: &str) -> bool {
    value.len() == 40 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// Whether a measured duration could have come from a real process.
///
/// Zero is the case worth naming. A monotonic clock read either side of a
/// process that genuinely ran cannot produce it, so a zero-length sample is
/// evidence that the measurement did not happen — and it is exactly the value
/// that would drag a median down while looking like a spectacular result.
pub fn is_measurable_duration(nanoseconds: u64) -> bool {
    nanoseconds > 0
}

/// How many samples an observation has beyond what its policy permits.
///
/// `None` when it is within policy. A protocol violation rather than a
/// measurement problem: the epoch declares how many samples a workload
/// contributes, and a run that took more was not taken under the policy the
/// series is comparing against, whatever the extra samples happen to say.
pub fn samples_beyond_policy(observed: usize, allowed: u32) -> Option<u32> {
    let observed = u32::try_from(observed).unwrap_or(u32::MAX);
    (observed > allowed).then_some(observed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_timestamp_spelling_is_accepted() {
        assert!(is_utc_timestamp("2026-08-13T00:00:00Z"));
        // Sub-second precision would give one instant two content addresses.
        assert!(!is_utc_timestamp("2026-08-13T00:00:00.000Z"));
        assert!(!is_utc_timestamp("2026-08-13 00:00:00Z"));
        assert!(!is_utc_timestamp("2026-08-13T00:00:00"));
        assert!(!is_utc_timestamp(""));
    }

    #[test]
    fn an_interval_may_not_run_backwards() {
        assert!(is_ordered_interval(
            "2026-08-13T00:00:00Z",
            "2026-08-13T00:01:00Z"
        ));
        // An instantaneous run is odd but not impossible at second resolution.
        assert!(is_ordered_interval(
            "2026-08-13T00:00:00Z",
            "2026-08-13T00:00:00Z"
        ));
        assert!(!is_ordered_interval(
            "2026-08-13T00:01:00Z",
            "2026-08-13T00:00:00Z"
        ));
        assert!(!is_ordered_interval("nonsense", "2026-08-13T00:00:00Z"));
    }

    #[test]
    fn a_digest_must_actually_be_one() {
        assert!(is_sha256_digest(&"a".repeat(64)));
        assert!(is_sha256_digest(&"0123456789abcdef".repeat(4)));
        // The case the runtime series depends on: a digest is the only thing
        // making two raw medians comparable, so a placeholder must not pass.
        assert!(!is_sha256_digest("not-a-hash"));
        assert!(!is_sha256_digest(""));
        assert!(
            !is_sha256_digest(&"A".repeat(64)),
            "uppercase is a second spelling"
        );
        assert!(!is_sha256_digest(&"a".repeat(63)));
    }

    #[test]
    fn a_commit_is_a_full_revision() {
        assert!(is_commit(&"a".repeat(40)));
        assert!(!is_commit(&"a".repeat(39)));
        assert!(!is_commit("not-a-commit"));
    }

    #[test]
    fn a_zero_length_measurement_did_not_happen() {
        assert!(!is_measurable_duration(0));
        assert!(is_measurable_duration(1));
    }

    #[test]
    fn samples_beyond_the_policy_are_counted() {
        assert_eq!(samples_beyond_policy(3, 3), None);
        assert_eq!(samples_beyond_policy(2, 3), None);
        assert_eq!(samples_beyond_policy(10, 3), Some(10));
    }
}
