use rue_test_runner::ShardSelector;
use serde::Deserialize;
use std::collections::{BTreeMap, HashMap};
use std::path::Path;

const WEIGHTS_VERSION: u64 = 1;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ShardWeights {
    version: u64,
    default_ms: u64,
    #[serde(default)]
    common: BTreeMap<String, u64>,
    #[serde(default)]
    platforms: BTreeMap<String, BTreeMap<String, u64>>,
}

/// A deterministic, cost-balanced assignment of every discovered CLI case.
pub struct CliShardPlan {
    selector: ShardSelector,
    assignments: HashMap<String, u64>,
    estimated_load_ms: Vec<u64>,
    case_counts: Vec<usize>,
    platform: String,
}

impl CliShardPlan {
    pub fn load(
        selector: ShardSelector,
        names: impl IntoIterator<Item = String>,
        path: &Path,
    ) -> Result<Self, String> {
        let contents = std::fs::read_to_string(path)
            .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
        let weights: ShardWeights = serde_json::from_str(&contents)
            .map_err(|error| format!("cannot parse {}: {error}", path.display()))?;
        if weights.version != WEIGHTS_VERSION {
            return Err(format!(
                "{} has version {}, expected {WEIGHTS_VERSION}",
                path.display(),
                weights.version
            ));
        }
        if weights.default_ms == 0 {
            return Err(format!("{}: default_ms must be positive", path.display()));
        }
        for (name, weight) in weights.common.iter().chain(
            weights
                .platforms
                .values()
                .flat_map(|platform| platform.iter()),
        ) {
            if *weight == 0 {
                return Err(format!(
                    "{}: case {name:?} has a zero weight",
                    path.display()
                ));
            }
        }

        let platform = host_platform().to_string();
        let platform_weights = weights.platforms.get(&platform);
        let weighted_names = names.into_iter().map(|name| {
            let weight = platform_weights
                .and_then(|platform| platform.get(&name))
                .or_else(|| weights.common.get(&name))
                .copied()
                .unwrap_or(weights.default_ms);
            (name, weight)
        });
        Self::from_weighted_names(selector, &platform, weighted_names)
    }

    fn from_weighted_names(
        selector: ShardSelector,
        platform: &str,
        names: impl IntoIterator<Item = (String, u64)>,
    ) -> Result<Self, String> {
        let mut names: Vec<(String, u64)> = names.into_iter().collect();
        names.sort_by(|(left_name, left_weight), (right_name, right_weight)| {
            right_weight
                .cmp(left_weight)
                .then_with(|| left_name.cmp(right_name))
        });

        let shard_count = usize::try_from(selector.count())
            .map_err(|_| format!("shard count {} does not fit usize", selector.count()))?;
        let mut assignments = HashMap::with_capacity(names.len());
        let mut estimated_load_ms = vec![0u64; shard_count];
        let mut case_counts = vec![0usize; shard_count];

        for (name, weight) in names {
            if assignments.contains_key(&name) {
                return Err(format!("duplicate discovered CLI test name {name:?}"));
            }
            let shard = (0..shard_count)
                .min_by_key(|&index| (estimated_load_ms[index], case_counts[index], index))
                .expect("ShardSelector rejects a zero shard count");
            assignments.insert(name, shard as u64);
            estimated_load_ms[shard] = estimated_load_ms[shard]
                .checked_add(weight)
                .ok_or_else(|| "estimated CLI shard load overflowed u64".to_string())?;
            case_counts[shard] += 1;
        }

        let plan = Self {
            selector,
            assignments,
            estimated_load_ms,
            case_counts,
            platform: platform.to_string(),
        };
        plan.validate_skew()?;
        Ok(plan)
    }

    pub fn includes(&self, name: &str) -> bool {
        self.assignments.get(name) == Some(&self.selector.index())
    }

    pub fn selected_estimated_load_ms(&self) -> u64 {
        self.estimated_load_ms[self.selector.index() as usize]
    }

    pub fn selected_case_count(&self) -> usize {
        self.case_counts[self.selector.index() as usize]
    }

    pub fn platform(&self) -> &str {
        &self.platform
    }

    fn validate_skew(&self) -> Result<(), String> {
        let total: u128 = self
            .estimated_load_ms
            .iter()
            .map(|&load| u128::from(load))
            .sum();
        let Some(&maximum) = self.estimated_load_ms.iter().max() else {
            return Ok(());
        };
        // maximum <= 1.25 * mean, kept in integer arithmetic:
        // maximum * shard_count * 4 <= total * 5.
        if u128::from(maximum) * self.estimated_load_ms.len() as u128 * 4 > total * 5 {
            return Err(format!(
                "estimated CLI shard loads for {} exceed the 25% skew budget: {:?}",
                self.platform, self.estimated_load_ms
            ));
        }
        Ok(())
    }
}

fn host_platform() -> &'static str {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => "macos",
        ("linux", "aarch64") => "linux-arm64",
        ("linux", "x86_64") => "linux-x64",
        _ => "other",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn selector(index: u64, count: u64) -> ShardSelector {
        ShardSelector::parse(Some(&format!("{index}/{count}")))
            .unwrap()
            .unwrap()
    }

    #[test]
    fn lpt_assignment_is_deterministic_exhaustive_and_disjoint() {
        let weighted = [
            ("slow", 80),
            ("medium", 40),
            ("small-a", 10),
            ("small-b", 10),
            ("small-c", 10),
            ("small-d", 10),
        ];
        let plans: Vec<CliShardPlan> = (0..2)
            .map(|index| {
                CliShardPlan::from_weighted_names(
                    selector(index, 2),
                    "test",
                    weighted
                        .iter()
                        .map(|(name, weight)| ((*name).to_string(), *weight)),
                )
                .unwrap()
            })
            .collect();

        for (name, _) in weighted {
            assert_eq!(
                plans.iter().filter(|plan| plan.includes(name)).count(),
                1,
                "{name} must belong to exactly one shard"
            );
        }
        assert_eq!(plans[0].estimated_load_ms, plans[1].estimated_load_ms);
        assert_eq!(plans[0].estimated_load_ms, vec![80, 80]);
    }

    #[test]
    fn equal_weights_balance_case_counts_with_stable_ties() {
        let plan = CliShardPlan::from_weighted_names(
            selector(0, 4),
            "test",
            (0..10).rev().map(|index| (format!("case-{index}"), 1)),
        )
        .unwrap();
        assert_eq!(plan.case_counts, vec![3, 3, 2, 2]);
        assert!(plan.includes("case-0"));
    }

    #[test]
    fn duplicate_names_are_rejected() {
        let error = CliShardPlan::from_weighted_names(
            selector(0, 2),
            "test",
            [("same".to_string(), 1), ("same".to_string(), 2)],
        )
        .err()
        .unwrap();
        assert!(error.contains("duplicate"));
    }

    #[test]
    fn excessive_skew_is_rejected() {
        let error = CliShardPlan::from_weighted_names(
            selector(0, 4),
            "test",
            [
                ("huge".to_string(), 100),
                ("a".to_string(), 1),
                ("b".to_string(), 1),
                ("c".to_string(), 1),
            ],
        )
        .err()
        .unwrap();
        assert!(error.contains("25% skew"));
    }

    #[test]
    fn host_specific_weights_override_common_weights() {
        let file = tempfile::NamedTempFile::new().unwrap();
        let platform = host_platform();
        std::fs::write(
            file.path(),
            format!(
                r#"{{
                    "version": 1,
                    "default_ms": 1,
                    "common": {{"a": 1, "b": 1, "c": 1}},
                    "platforms": {{
                        "{platform}": {{"a": 100, "b": 60, "c": 40}}
                    }}
                }}"#
            ),
        )
        .unwrap();

        let plan = CliShardPlan::load(
            selector(0, 2),
            ["a", "b", "c"].into_iter().map(str::to_string),
            file.path(),
        )
        .unwrap();
        assert_eq!(plan.estimated_load_ms, vec![100, 100]);
        assert_eq!(plan.platform(), platform);
    }
}
