use rue_test_runner::ShardSelector;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::path::Path;

const WEIGHTS_VERSION: u64 = 1;
const SHARD_LOADS_VERSION: u64 = 1;

/// The measured per-case weight file, `shard-weights.json`: a `common`
/// baseline, per-platform overlays, and a `default_ms` fallback for any
/// discovered case the file does not name.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShardWeights {
    version: u64,
    default_ms: u64,
    #[serde(default)]
    common: BTreeMap<String, u64>,
    #[serde(default)]
    platforms: BTreeMap<String, BTreeMap<String, u64>>,
}

impl ShardWeights {
    pub fn load(path: &Path) -> Result<Self, String> {
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
        Ok(weights)
    }

    /// The platforms this file models with a dedicated overlay — the set the
    /// shard-loads report covers, matching what the timeout-policy gate checks.
    pub fn platform_names(&self) -> impl Iterator<Item = &str> {
        self.platforms.keys().map(String::as_str)
    }

    /// The expected cost of `name` on `platform`: the platform overlay first,
    /// then the common baseline, then `default_ms` for an unmeasured case.
    fn weight_for(&self, platform: &str, name: &str) -> u64 {
        self.platforms
            .get(platform)
            .and_then(|platform| platform.get(name))
            .or_else(|| self.common.get(name))
            .copied()
            .unwrap_or(self.default_ms)
    }
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
        let weights = ShardWeights::load(path)?;
        Self::for_platform(selector, host_platform(), names, &weights)
    }

    /// The single population-and-packing rule, parameterized by platform name.
    ///
    /// The runtime path calls this through [`CliShardPlan::load`] with the
    /// detected host; the shard-loads emit mode calls it once per platform the
    /// weights file models. Keeping one body is the point: every discovered
    /// name is weighted (overlay, then common, then `default_ms`) and packed
    /// LPT-style, so a derived deadline can never model a different corpus
    /// than the one the harness runs.
    pub fn for_platform(
        selector: ShardSelector,
        platform: &str,
        names: impl IntoIterator<Item = String>,
        weights: &ShardWeights,
    ) -> Result<Self, String> {
        let weighted_names = names.into_iter().map(|name| {
            let weight = weights.weight_for(platform, &name);
            (name, weight)
        });
        Self::from_weighted_names(selector, platform, weighted_names)
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

        Ok(Self {
            selector,
            assignments,
            estimated_load_ms,
            case_counts,
            platform: platform.to_string(),
        })
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

    pub fn estimated_load_ms(&self) -> &[u64] {
        &self.estimated_load_ms
    }

    pub fn case_counts(&self) -> &[usize] {
        &self.case_counts
    }
}

/// The machine-readable shard-loads report the emit mode prints: for every
/// platform the weights file models, the per-shard expected costs the runtime
/// packing would produce there. `scripts/cli-timeout-policy.py` consumes this
/// (via the `//:cli-shard-loads-json` genrule) instead of re-deriving the
/// packing, so the packing exists exactly once — here.
#[derive(Debug, Serialize)]
pub struct ShardLoadsReport {
    version: u64,
    shard_count: u64,
    platforms: BTreeMap<String, PlatformShardLoads>,
}

#[derive(Debug, Serialize)]
pub struct PlatformShardLoads {
    loads_ms: Vec<u64>,
    case_counts: Vec<usize>,
    total_cases: usize,
}

/// Pack `names` into `shard_count` shards once per modeled platform.
///
/// Every platform reuses the exact runtime packing ([`CliShardPlan`]). Balance
/// is validated from independently observed lane wall time by
/// `scripts/plan-cli-shards.py`; checking these estimated serial weights
/// against themselves cannot detect stale weights or parallel makespan skew.
pub fn shard_loads_report(
    shard_count: u64,
    names: &[String],
    weights: &ShardWeights,
) -> Result<ShardLoadsReport, String> {
    // The packing is index-independent; selector index 0 is arbitrary.
    let selector = ShardSelector::parse(Some(&format!("0/{shard_count}")))
        .map_err(|error| format!("invalid shard count {shard_count}: {error}"))?
        .expect("a spelled-out spec is never the unset case");
    let mut platforms = BTreeMap::new();
    for platform in weights.platform_names() {
        let plan = CliShardPlan::for_platform(selector, platform, names.iter().cloned(), weights)?;
        platforms.insert(
            platform.to_string(),
            PlatformShardLoads {
                loads_ms: plan.estimated_load_ms().to_vec(),
                case_counts: plan.case_counts().to_vec(),
                total_cases: plan.case_counts().iter().sum(),
            },
        );
    }
    if platforms.is_empty() {
        return Err("shard weights model no platforms; nothing to report".to_string());
    }
    Ok(ShardLoadsReport {
        version: SHARD_LOADS_VERSION,
        shard_count,
        platforms,
    })
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

    fn weights_from_json(json: &str) -> ShardWeights {
        let file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(file.path(), json).unwrap();
        ShardWeights::load(file.path()).unwrap()
    }

    #[test]
    fn unweighted_names_fall_back_to_default_ms_per_platform() {
        // The population rule the emit mode must share with the runtime path:
        // a discovered case absent from the weights file still costs
        // `default_ms`, and a platform overlay beats the common baseline.
        let weights = weights_from_json(
            r#"{
                "version": 1,
                "default_ms": 7,
                "common": {"a": 100},
                "platforms": {"fast": {"a": 10}, "slow": {}}
            }"#,
        );
        let names = ["a", "unmeasured"].map(str::to_string);
        let fast =
            CliShardPlan::for_platform(selector(0, 1), "fast", names.clone(), &weights).unwrap();
        assert_eq!(fast.estimated_load_ms(), &[17]);
        let slow = CliShardPlan::for_platform(selector(0, 1), "slow", names, &weights).unwrap();
        assert_eq!(slow.estimated_load_ms(), &[107]);
    }

    #[test]
    fn shard_loads_report_covers_every_modeled_platform() {
        let weights = weights_from_json(
            r#"{
                "version": 1,
                "default_ms": 10,
                "common": {"a": 80, "b": 40, "c": 40},
                "platforms": {"fast": {"a": 40, "b": 40, "c": 40}, "slow": {}}
            }"#,
        );
        let names: Vec<String> = ["a", "b", "c", "d"].map(str::to_string).into();
        let report = shard_loads_report(2, &names, &weights).unwrap();
        let value = serde_json::to_value(&report).unwrap();
        assert_eq!(value["version"], 1);
        assert_eq!(value["shard_count"], 2);
        // fast overlay: a=b=c=40, unweighted d=10 -> a:0, b:1, c:0 (load tie,
        // index breaks it), d:1. slow (common only): a=80, b=c=40, d=10 ->
        // a:0, b:1, c:1, d:0 (load tie at 80/80, case-count breaks it).
        assert_eq!(
            value["platforms"]["fast"]["loads_ms"],
            serde_json::json!([80, 50])
        );
        assert_eq!(
            value["platforms"]["slow"]["loads_ms"],
            serde_json::json!([90, 80])
        );
        assert_eq!(value["platforms"]["slow"]["total_cases"], 4);
        assert_eq!(
            value["platforms"]["fast"]["case_counts"],
            serde_json::json!([2, 2])
        );
    }

    #[test]
    fn shard_loads_report_rejects_a_zero_shard_count() {
        let weights = weights_from_json(
            r#"{"version": 1, "default_ms": 1, "common": {"a": 1}, "platforms": {"p": {}}}"#,
        );
        let error = shard_loads_report(0, &["a".to_string()], &weights)
            .err()
            .unwrap();
        assert!(error.contains("invalid shard count"));
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
