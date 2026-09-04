//! Turning an inventory into a run order (ADR-0083 §2).
//!
//! Three stages, always in this order: filter, then shard, then shuffle.
//!
//! The order is not arbitrary. Sharding a filtered set is what makes
//! `--shard K/N` partition *the run*, so the N shards of a filtered run
//! reconstitute exactly that run. Shuffling last is what keeps the shuffle from
//! changing which tests a shard owns — order is a scheduling decision, and
//! membership is not.
//!
//! Both the shard assignment and the shuffle are pure functions of values the
//! event stream publishes (the stable ID, and the seed), so a reader of an
//! event stream can reproduce the run order without asking the runner anything.

use std::num::NonZeroU64;

use rue_compiler::unstable::TestInventoryEntry;

/// FNV-1a over the stable ID's bytes, which is what `--shard` partitions on.
///
/// A named, fully specified hash rather than `DefaultHasher`: `SipHash`'s
/// `DefaultHasher` is explicitly not stable across Rust releases, and a shard
/// assignment that moves when the compiler is rebuilt would silently drop tests
/// from a sharded CI run. FNV-1a is two lines, has no dependency, and is
/// documented in `docs/process/test-events.md` so an external scheduler can
/// compute the same partition.
pub(crate) fn shard_hash(id: &str) -> u64 {
    const OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash = OFFSET_BASIS;
    for byte in id.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(PRIME);
    }
    hash
}

/// SplitMix64: the shuffle's PRNG.
///
/// Chosen for the same reason as the shard hash — it is a documented, seven-line
/// algorithm an external tool can reimplement from the schema doc, so `--seed`
/// reproduces an order outside this binary and not merely inside it.
struct SplitMix64(u64);

impl SplitMix64 {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    /// A uniform value in `0..bound`, by rejection so the modulo bias that
    /// would quietly favour low indices never enters the shuffle.
    ///
    /// `bound` is a [`NonZeroU64`] rather than a checked `u64`: the only
    /// unusable input is zero, and taking it in the type means the caller's
    /// obligation is discharged where the value is built instead of re-checked
    /// here on every draw.
    fn below(&mut self, bound: NonZeroU64) -> u64 {
        let bound = bound.get();
        let zone = u64::MAX - (u64::MAX % bound);
        loop {
            let draw = self.next();
            if draw < zone {
                return draw % bound;
            }
        }
    }
}

/// A `--shard K/N` selector: 1-based `index` of `count`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Shard {
    pub(crate) index: u64,
    pub(crate) count: u64,
}

impl Shard {
    /// Parse the `K/N` spelling, rejecting the off-by-one traps directly.
    pub(crate) fn parse(text: &str) -> Result<Self, String> {
        let (index, count) = text
            .split_once('/')
            .ok_or_else(|| format!("--shard value '{text}' must be spelled K/N"))?;
        let index = index
            .parse::<u64>()
            .map_err(|_| format!("--shard index '{index}' must be a positive integer"))?;
        let count = count
            .parse::<u64>()
            .map_err(|_| format!("--shard count '{count}' must be a positive integer"))?;
        if count == 0 {
            return Err("--shard count must be at least 1".to_owned());
        }
        if index == 0 || index > count {
            return Err(format!(
                "--shard index {index} is out of range for {count} shards (indices are 1-based)"
            ));
        }
        Ok(Self { index, count })
    }

    fn owns(self, id: &str) -> bool {
        shard_hash(id) % self.count == self.index - 1
    }
}

impl std::fmt::Display for Shard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}", self.index, self.count)
    }
}

/// Keep the entries at least one filter matches.
///
/// Matching is a substring test against the stable ID, and repeated filters
/// union — the rule `docs/process/test-events.md` states normatively. Substring
/// rather than glob or regex because the ID's own shape (`<module>::<name>`)
/// already gives a user the two prefixes they reach for, and a filter language
/// is a compatibility surface this ADR does not need.
pub(crate) fn matches_filters(id: &str, filters: &[String]) -> bool {
    filters.is_empty() || filters.iter().any(|filter| id.contains(filter.as_str()))
}

/// Which tests an invocation acts on: filter, then shard, in inventory order.
///
/// The membership half of the pipeline, shared by `plan` and `--list` so the
/// listing cannot answer a different question from the run it previews. Order
/// is left alone here because the two consumers want different orders — a run
/// shuffles, a listing must not — and order is not membership.
pub(crate) fn select<'a>(
    entries: &'a [TestInventoryEntry],
    filters: &[String],
    shard: Option<Shard>,
) -> Vec<&'a TestInventoryEntry> {
    entries
        .iter()
        .filter(|entry| matches_filters(&entry.id, filters))
        .filter(|entry| shard.is_none_or(|shard| shard.owns(&entry.id)))
        .collect()
}

/// Filter, shard, and shuffle one inventory into the order tests will run in.
pub(crate) fn plan(
    entries: &[TestInventoryEntry],
    filters: &[String],
    shard: Option<Shard>,
    seed: u64,
) -> Vec<TestInventoryEntry> {
    let mut selected: Vec<TestInventoryEntry> = select(entries, filters, shard)
        .into_iter()
        .cloned()
        .collect();
    shuffle(&mut selected, seed);
    selected
}

/// Fisher-Yates over a SplitMix64 stream seeded by `--seed`.
///
/// The loop starts at index 1, so each draw's exclusive bound is at least two
/// and the `NonZeroU64` below is built from a value the range itself
/// guarantees.
fn shuffle(entries: &mut [TestInventoryEntry], seed: u64) {
    let mut rng = SplitMix64(seed);
    for position in (1..entries.len()).rev() {
        let bound = NonZeroU64::new(position as u64 + 1).expect("position >= 1, so bound >= 2");
        entries.swap(position, rng.below(bound) as usize);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: &str) -> TestInventoryEntry {
        TestInventoryEntry {
            id: id.to_owned(),
            module: "m.rue".to_owned(),
            name: id.to_owned(),
            file: "m.rue".to_owned(),
            line: 1,
            column: 1,
            ordinal: 0,
        }
    }

    fn inventory(count: usize) -> Vec<TestInventoryEntry> {
        (0..count)
            .map(|index| entry(&format!("app/m.rue::test {index}")))
            .collect()
    }

    fn ids(entries: &[TestInventoryEntry]) -> Vec<String> {
        entries.iter().map(|entry| entry.id.clone()).collect()
    }

    /// The hash is a published contract: an external scheduler computes the
    /// same partition from the schema doc, so its values are pinned here.
    #[test]
    fn the_shard_hash_is_pinned_fnv_1a() {
        assert_eq!(shard_hash(""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(shard_hash("a"), 0xaf63_dc4c_8601_ec8c);
        assert_eq!(shard_hash("foobar"), 0x8594_4171_f739_67e8);
    }

    #[test]
    fn no_filter_selects_everything() {
        assert!(matches_filters("app/m.rue::anything", &[]));
    }

    #[test]
    fn filters_match_substrings_of_the_stable_id_and_union() {
        let filters = vec!["parse_port".to_owned(), "lexer".to_owned()];
        assert!(matches_filters("app/p.rue::parse_port accepts", &filters));
        assert!(matches_filters("app/l.rue::lexer eats spaces", &filters));
        assert!(!matches_filters("app/x.rue::something else", &filters));
        // The module half of the ID is matchable too, which is how a whole
        // file's tests are selected without naming each one.
        assert!(matches_filters(
            "app/lexer_tests.rue::whatever",
            &["app/lexer_tests.rue".to_owned()]
        ));
    }

    /// A run's shards partition it: every test in exactly one shard.
    #[test]
    fn shards_partition_the_selection() {
        let entries = inventory(40);
        let mut union: Vec<String> = Vec::new();
        for index in 1..=4 {
            let shard = Shard { index, count: 4 };
            union.extend(ids(&plan(&entries, &[], Some(shard), 0)));
        }
        union.sort();
        let mut all = ids(&entries);
        all.sort();
        assert_eq!(union, all);
    }

    /// Sharding happens after filtering, so the shards of a filtered run
    /// reconstitute that run and not the whole inventory.
    #[test]
    fn shards_partition_the_filtered_run_not_the_inventory() {
        let entries = inventory(40);
        let filters = vec!["test 1".to_owned()];
        let mut union: Vec<String> = Vec::new();
        for index in 1..=2 {
            union.extend(ids(&plan(
                &entries,
                &filters,
                Some(Shard { index, count: 2 }),
                7,
            )));
        }
        union.sort();
        let mut expected = ids(&plan(&entries, &filters, None, 7));
        expected.sort();
        assert_eq!(union, expected);
        assert!(expected.len() < 40, "the filter must actually narrow");
    }

    #[test]
    fn a_seed_reproduces_an_order_and_different_seeds_generally_differ() {
        let entries = inventory(24);
        assert_eq!(
            ids(&plan(&entries, &[], None, 417)),
            ids(&plan(&entries, &[], None, 417))
        );
        assert_ne!(
            ids(&plan(&entries, &[], None, 417)),
            ids(&plan(&entries, &[], None, 418))
        );
    }

    /// Shuffling never changes membership, only order.
    #[test]
    fn the_shuffle_is_a_permutation() {
        let entries = inventory(31);
        let mut shuffled = ids(&plan(&entries, &[], None, 99));
        shuffled.sort();
        let mut all = ids(&entries);
        all.sort();
        assert_eq!(shuffled, all);
    }

    #[test]
    fn shard_parsing_rejects_the_off_by_one_traps() {
        assert_eq!(Shard::parse("1/2").unwrap(), Shard { index: 1, count: 2 });
        assert!(Shard::parse("0/2").unwrap_err().contains("1-based"));
        assert!(Shard::parse("3/2").unwrap_err().contains("out of range"));
        assert!(Shard::parse("1/0").unwrap_err().contains("at least 1"));
        assert!(Shard::parse("1").unwrap_err().contains("K/N"));
        assert!(
            Shard::parse("a/2")
                .unwrap_err()
                .contains("positive integer")
        );
    }

    /// What `--list` publishes: the same membership a run would have, in
    /// inventory order. A listing is compared against another listing, so the
    /// shuffle belongs to `plan` alone.
    #[test]
    fn a_selection_keeps_inventory_order_while_a_plan_shuffles_it() {
        let entries = inventory(24);
        let selected: Vec<String> = select(&entries, &[], None)
            .into_iter()
            .map(|entry| entry.id.clone())
            .collect();
        assert_eq!(selected, ids(&entries));
        assert_ne!(
            selected,
            ids(&plan(&entries, &[], None, 417)),
            "a run's order is shuffled; a listing's is not"
        );
    }

    /// The listing and the run answer the same membership question, filters and
    /// shard included — one computation, two consumers.
    #[test]
    fn a_selection_and_a_plan_agree_on_membership() {
        let entries = inventory(40);
        let filters = vec!["test 1".to_owned()];
        let shard = Some(Shard { index: 2, count: 3 });
        let mut selected: Vec<String> = select(&entries, &filters, shard)
            .into_iter()
            .map(|entry| entry.id.clone())
            .collect();
        let mut planned = ids(&plan(&entries, &filters, shard, 417));
        assert!(!selected.is_empty(), "the fixture must select something");
        selected.sort();
        planned.sort();
        assert_eq!(selected, planned);
    }

    #[test]
    fn an_empty_or_single_selection_shuffles_without_panicking() {
        assert!(plan(&[], &[], None, 5).is_empty());
        assert_eq!(plan(&inventory(1), &[], None, 5).len(), 1);
    }
}
