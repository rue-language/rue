//! Deterministic fixture generation for runtime workloads (ADR-0072 Phase 1).
//!
//! A runtime benchmark needs a large input, and a repository does not want tens
//! of megabytes of committed prose. The compromise is a checked-in *generator*:
//! the manifest pins a seed and this module's revision, the bytes are produced
//! at fixture-preparation time — outside the timed window — and the digest of
//! what was actually produced is recorded with every observation.
//!
//! Two properties make that compromise safe.
//!
//! **Byte-exact determinism, everywhere.** The same seed and size produce the
//! same bytes on every host and in every build. That is why nothing here uses
//! floating point: a power-law draw computed with `powf` can differ in its last
//! bit between targets, and a fixture that differs by one byte between two
//! runners silently makes their series incomparable. The distribution is built
//! from integer bit tricks instead.
//!
//! **A revision that moves when the output moves.** Any change to the algorithm
//! below must bump [`ZIPF_ASCII_TEXT_REVISION`], because the committed golden
//! output is a function of these bytes. The runner refuses to generate a
//! fixture whose declared revision is not the one it implements, so a manifest
//! and a generator can never drift apart quietly.
//!
//! The text is shaped for the workload it feeds: ASCII letter runs separated by
//! spaces, commas, full stops, and newlines, with word ranks drawn from a
//! harmonic (Zipf-like) distribution so a few words dominate and a long tail
//! fills the map. That is what makes `wordfreq` spend its time counting words
//! and pressing on a hash map rather than starting up.

/// Name of the generator this module implements.
pub const ZIPF_ASCII_TEXT: &str = "zipf_ascii_text";

/// Revision of that generator.
///
/// Bump on any change to the produced bytes. The committed golden output and
/// every recorded fixture digest are functions of this.
pub const ZIPF_ASCII_TEXT_REVISION: u32 = 1;

/// SplitMix64: a small, fast, fully specified integer mixer.
///
/// Chosen over a library RNG so the fixture's bytes are a property of this
/// source file rather than of a dependency's version.
struct SplitMix64 {
    state: u64,
}

impl SplitMix64 {
    fn new(seed: u64) -> SplitMix64 {
        SplitMix64 { state: seed }
    }

    fn next(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        mix(self.state)
    }

    /// A value in `[0, bound)`. `bound` of zero yields zero.
    fn below(&mut self, bound: u64) -> u64 {
        if bound == 0 {
            return 0;
        }
        // Multiply-shift rather than modulo: uniform enough for a text fixture
        // and free of the modulo bias that would skew the rarest ranks.
        ((u128::from(self.next()) * u128::from(bound)) >> 64) as u64
    }
}

fn mix(value: u64) -> u64 {
    let mut z = value;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// The vocabulary word at `rank`, as ASCII lowercase letters.
///
/// Derived from the seed and the rank alone, so the same rank is the same word
/// for the whole fixture without materializing a word list. Lengths run three
/// to eleven letters, which keeps tokenization realistic without making the
/// file mostly separators.
fn word_for_rank(seed: u64, rank: u64) -> Vec<u8> {
    let mut hash = mix(seed ^ rank.wrapping_mul(0x9E37_79B9_7F4A_7C15));
    let length = 3 + (hash % 9) as usize;
    let mut word = Vec::with_capacity(length);
    for index in 0..length {
        if index % 12 == 0 {
            hash = mix(hash ^ (index as u64).wrapping_add(0x5851_F42D_4C95_7F2D));
        }
        word.push(b'a' + ((hash >> (5 * (index % 12))) % 26) as u8);
    }
    word
}

/// The highest bucket index a vocabulary of this size supports.
fn top_bucket(vocabulary_size: u32) -> u32 {
    // `vocabulary_size >= 1`, checked by the caller, so this cannot underflow.
    63 - u64::from(vocabulary_size).leading_zeros().min(63)
}

/// Draw one word rank from a harmonic distribution over `[0, vocabulary_size)`.
///
/// A bucket is chosen uniformly and a rank uniformly within it, where bucket
/// `b` covers `2^b` ranks. Probability per rank is therefore proportional to
/// `1/rank`, the classic Zipf shape: the commonest word appears in a few
/// percent of positions and the tail still reaches the rarest words often
/// enough for them to exist in the counts.
///
/// Deliberately not a geometric bucket draw, which would give `1/rank^2` and
/// leave the vocabulary's tail unvisited — the map would stay small and the
/// benchmark would measure a handful of hot keys instead of map pressure.
fn draw_rank(rng: &mut SplitMix64, vocabulary_size: u32) -> u64 {
    let top = top_bucket(vocabulary_size);
    let bucket = rng.below(u64::from(top) + 1) as u32;
    let low = (1u64 << bucket) - 1;
    let high = ((1u64 << (bucket + 1)) - 1).min(u64::from(vocabulary_size));
    low + rng.below(high - low)
}

/// Generate exactly `bytes` of deterministic ASCII text.
///
/// Exactly, not approximately: the size is part of the workload contract that a
/// suite revision pins, and a generator that overshot by a word would make the
/// committed golden output depend on where the last word happened to land.
pub fn generate_zipf_ascii_text(seed: u64, bytes: u64, vocabulary_size: u32) -> Vec<u8> {
    let capacity = usize::try_from(bytes).unwrap_or(usize::MAX);
    let mut text = Vec::with_capacity(capacity);
    if capacity == 0 || vocabulary_size == 0 {
        return text;
    }
    let mut rng = SplitMix64::new(mix(seed));
    // A separate stream for punctuation, so changing the sentence shape later
    // would not also reshuffle every word.
    let mut punctuation = SplitMix64::new(mix(seed ^ 0xA5A5_A5A5_A5A5_A5A5));

    let mut words_in_sentence = 0u32;
    while text.len() < capacity {
        let word = word_for_rank(seed, draw_rank(&mut rng, vocabulary_size));
        // Stop before a partial word: a truncated word would be a different
        // token and would move the golden output for a size change of one byte.
        if text.len() + word.len() + 1 > capacity {
            break;
        }
        text.extend_from_slice(&word);
        words_in_sentence += 1;

        // Separators are non-letters, so they all terminate a token. Their mix
        // exists to keep the input shaped like prose rather than like one
        // enormous space-separated line.
        let roll = punctuation.below(64);
        if words_in_sentence >= 8 && roll < 12 {
            text.push(b'.');
            text.push(b'\n');
            words_in_sentence = 0;
        } else if roll < 20 {
            text.push(b',');
            text.push(b' ');
        } else {
            text.push(b' ');
        }
        // The pair separators may have pushed one byte past the reservation.
        text.truncate(capacity);
    }
    // Pad with separators rather than letters: padding must not invent a word.
    while text.len() + 1 < capacity {
        text.push(b'.');
    }
    if text.len() < capacity {
        text.push(b'\n');
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn generation_is_byte_exact_across_calls() {
        // The property the whole design rests on: two runners must produce the
        // same fixture, or their series are not comparable.
        let first = generate_zipf_ascii_text(20260813, 64 * 1024, 4096);
        let second = generate_zipf_ascii_text(20260813, 64 * 1024, 4096);
        assert_eq!(first, second);
    }

    #[test]
    fn a_different_seed_produces_different_text() {
        let first = generate_zipf_ascii_text(1, 64 * 1024, 4096);
        let second = generate_zipf_ascii_text(2, 64 * 1024, 4096);
        assert_ne!(first, second);
        assert_eq!(first.len(), second.len());
    }

    #[test]
    fn the_fixture_is_exactly_the_requested_size() {
        // Approximate sizing would make the committed golden output depend on
        // where the last word happened to fall.
        for bytes in [1u64, 2, 17, 1024, 65_537] {
            let text = generate_zipf_ascii_text(20260813, bytes, 4096);
            assert_eq!(text.len() as u64, bytes, "at {bytes} bytes");
        }
    }

    #[test]
    fn the_text_is_ascii_letters_and_separators_only() {
        let text = generate_zipf_ascii_text(20260813, 128 * 1024, 4096);
        assert!(
            text.iter()
                .all(|byte| byte.is_ascii_lowercase() || matches!(byte, b' ' | b',' | b'.' | b'\n')),
            "the fixture must contain nothing the workload's tokenizer has not been shown"
        );
    }

    #[test]
    fn a_prefix_of_the_text_is_not_a_prefix_of_a_longer_generation() {
        // Documents a real constraint rather than a nicety: the golden output
        // is a function of the exact declared size, so changing `bytes` is a
        // workload change and must be a suite-revision event.
        let short = generate_zipf_ascii_text(20260813, 4096, 4096);
        let long = generate_zipf_ascii_text(20260813, 8192, 4096);
        assert_ne!(&long[..short.len()], &short[..]);
    }

    #[test]
    fn the_distribution_reaches_the_whole_vocabulary() {
        // The reason the bucket draw is uniform rather than geometric. A
        // 1/rank^2 shape leaves most of the vocabulary unvisited, and the
        // workload then measures a few hot keys instead of map pressure.
        let vocabulary = 1024u32;
        let text = generate_zipf_ascii_text(20260813, 2 * 1024 * 1024, vocabulary);
        let distinct: BTreeSet<&[u8]> = text
            .split(|byte| !byte.is_ascii_lowercase())
            .filter(|word| !word.is_empty())
            .collect();
        assert!(
            distinct.len() > (vocabulary as usize) / 2,
            "only {} of {vocabulary} vocabulary words appeared",
            distinct.len()
        );
    }

    #[test]
    fn the_distribution_is_skewed_rather_than_uniform() {
        // The other half of the shape. A uniform draw would make every count
        // equal and the tie-breaking the workload exists to exercise moot.
        let text = generate_zipf_ascii_text(20260813, 1024 * 1024, 1024);
        let mut counts: std::collections::BTreeMap<&[u8], usize> =
            std::collections::BTreeMap::new();
        let mut total = 0usize;
        for word in text
            .split(|byte| !byte.is_ascii_lowercase())
            .filter(|word| !word.is_empty())
        {
            *counts.entry(word).or_default() += 1;
            total += 1;
        }
        let top = counts.values().max().copied().unwrap_or(0);
        assert!(
            top * 20 > total,
            "the commonest of {} words took only {top} of {total} positions",
            counts.len()
        );
    }

    #[test]
    fn a_rank_always_names_the_same_word() {
        assert_eq!(word_for_rank(7, 12), word_for_rank(7, 12));
        assert_ne!(word_for_rank(7, 12), word_for_rank(7, 13));
        assert_ne!(word_for_rank(7, 12), word_for_rank(8, 12));
    }

    #[test]
    fn every_generated_word_is_a_plausible_token() {
        for rank in 0..2048u64 {
            let word = word_for_rank(20260813, rank);
            assert!((3..=11).contains(&word.len()), "{word:?}");
            assert!(word.iter().all(u8::is_ascii_lowercase), "{word:?}");
        }
    }

    #[test]
    fn an_empty_request_generates_nothing_rather_than_panicking() {
        assert!(generate_zipf_ascii_text(1, 0, 1024).is_empty());
        assert!(generate_zipf_ascii_text(1, 1024, 0).is_empty());
    }
}
