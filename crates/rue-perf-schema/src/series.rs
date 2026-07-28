//! Series identity: what makes two observations comparable.

use serde::{Deserialize, Serialize};

/// A tracked quantity.
///
/// Each metric gets the same per-workload treatment and its own headline index.
/// Indexes for different metrics are never mixed, and none of them is a
/// wall-clock quantity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Metric {
    /// Wall-clock latency of a complete fresh compilation.
    Latency,
    /// Peak resident memory during a complete fresh compilation.
    PeakMemory,
    /// Size of the produced binary.
    BinarySize,
}

impl Metric {
    /// Every tracked metric.
    pub const ALL: [Metric; 3] = [Metric::Latency, Metric::PeakMemory, Metric::BinarySize];

    /// The stable wire name used in chart data and raw-record queries.
    pub const fn wire_name(self) -> &'static str {
        match self {
            Metric::Latency => "latency",
            Metric::PeakMemory => "peak_memory",
            Metric::BinarySize => "binary_size",
        }
    }
}

/// The identity of one comparable sequence of observations.
///
/// A series is `(epoch, workload, metric)`. The suite revision and platform
/// epoch pin everything else that could invalidate a comparison — workload
/// source identity, invocation, toolchain, sampling policy, environment policy
/// — so nothing further belongs in the identity.
///
/// The compiler revision is deliberately absent: it is a *point within* a
/// series, never part of it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SeriesId {
    /// The platform the epoch describes.
    pub platform: String,
    /// The platform epoch, which transitively pins the suite revision.
    pub epoch: u32,
    /// The workload's logical identifier.
    pub workload: String,
    /// The tracked quantity.
    pub metric: Metric,
}

impl SeriesId {
    /// Construct a series identity.
    pub fn new(
        platform: impl Into<String>,
        epoch: u32,
        workload: impl Into<String>,
        metric: Metric,
    ) -> Self {
        SeriesId {
            platform: platform.into(),
            epoch,
            workload: workload.into(),
            metric,
        }
    }
}

impl std::fmt::Display for SeriesId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}/epoch-{}/{}/{}",
            self.platform,
            self.epoch,
            self.workload,
            self.metric.wire_name()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn series_identity_excludes_the_compiler_revision() {
        // Two observations at different compiler revisions are points in one
        // series, so nothing in the identity can distinguish them.
        let one = SeriesId::new("x86_64-linux", 1, "startup", Metric::Latency);
        let two = SeriesId::new("x86_64-linux", 1, "startup", Metric::Latency);
        assert_eq!(one, two);
    }

    #[test]
    fn each_component_separates_series() {
        let base = SeriesId::new("x86_64-linux", 1, "startup", Metric::Latency);
        assert_ne!(
            base,
            SeriesId::new("aarch64-macos", 1, "startup", Metric::Latency)
        );
        assert_ne!(
            base,
            SeriesId::new("x86_64-linux", 2, "startup", Metric::Latency)
        );
        assert_ne!(
            base,
            SeriesId::new("x86_64-linux", 1, "caldera", Metric::Latency)
        );
        assert_ne!(
            base,
            SeriesId::new("x86_64-linux", 1, "startup", Metric::PeakMemory)
        );
    }

    #[test]
    fn display_is_stable_and_readable() {
        let series = SeriesId::new("x86_64-linux", 3, "caldera", Metric::PeakMemory);
        assert_eq!(
            series.to_string(),
            "x86_64-linux/epoch-3/caldera/peak_memory"
        );
    }

    #[test]
    fn metric_wire_names_are_distinct() {
        let mut names: Vec<&str> = Metric::ALL
            .iter()
            .map(|metric| metric.wire_name())
            .collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), Metric::ALL.len());
    }
}
