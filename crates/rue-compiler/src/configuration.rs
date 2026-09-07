//! Immutable configuration for a compiler session.

use std::fmt;

use rue_query::{DEFAULT_DEPENDENCY_PIN_BUDGET, DEFAULT_RETAINED_BYTE_BUDGET, RetentionBudgets};

/// The largest explicit query worker count accepted by the compiler.
pub const MAX_QUERY_WORKERS: usize = 256;

/// An invalid compiler session configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompilerConfigurationError {
    /// An explicit worker count was outside the supported range.
    WorkerCountOutOfRange { requested: usize, maximum: usize },
}

impl fmt::Display for CompilerConfigurationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WorkerCountOutOfRange { requested, maximum } => write!(
                formatter,
                "compiler worker count must be 0 (automatic) or 1..={maximum}, got {requested}"
            ),
        }
    }
}

impl std::error::Error for CompilerConfigurationError {}

/// Validated, immutable resources owned by one [`crate::CompilerSession`].
///
/// A worker count of zero requests host-parallel automatic selection. The
/// stored value is always resolved, so reporting the configuration is stable
/// for the lifetime of the session and never consults process-global state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompilerSessionConfig {
    workers: usize,
    retained_byte_budget: u64,
    dependency_pin_budget: u64,
}

impl Default for CompilerSessionConfig {
    fn default() -> Self {
        Self::with_workers_and_retention(
            0,
            DEFAULT_RETAINED_BYTE_BUDGET,
            DEFAULT_DEPENDENCY_PIN_BUDGET,
        )
        .expect("default compiler configuration is valid")
    }
}

impl CompilerSessionConfig {
    /// Construct and validate a session configuration.
    ///
    /// Retention values are soft accounting targets and accept the full `u64`
    /// range. Zero asks the runtime to retain no unprotected idle artifacts;
    /// active work and protected results may still exceed either target.
    pub fn with_workers_and_retention(
        workers: usize,
        retained_byte_budget: u64,
        dependency_pin_budget: u64,
    ) -> Result<Self, CompilerConfigurationError> {
        if workers > MAX_QUERY_WORKERS {
            return Err(CompilerConfigurationError::WorkerCountOutOfRange {
                requested: workers,
                maximum: MAX_QUERY_WORKERS,
            });
        }
        let workers = if workers == 0 {
            std::thread::available_parallelism()
                .map(std::num::NonZeroUsize::get)
                .unwrap_or(1)
        } else {
            workers
        };
        Ok(Self {
            workers,
            retained_byte_budget,
            dependency_pin_budget,
        })
    }

    /// Construct a configuration with the established retention defaults.
    pub fn with_workers(workers: usize) -> Result<Self, CompilerConfigurationError> {
        Self::with_workers_and_retention(
            workers,
            DEFAULT_RETAINED_BYTE_BUDGET,
            DEFAULT_DEPENDENCY_PIN_BUDGET,
        )
    }

    /// The resolved number of query workers owned by this configuration.
    pub fn workers(&self) -> usize {
        self.workers
    }

    /// The runtime-wide retained artifact byte budget.
    pub fn retained_byte_budget(&self) -> u64 {
        self.retained_byte_budget
    }

    /// The runtime-wide dependency and input observation budget.
    pub fn dependency_pin_budget(&self) -> u64 {
        self.dependency_pin_budget
    }

    pub(crate) fn retention_budgets(self) -> RetentionBudgets {
        RetentionBudgets {
            retained_bytes: self.retained_byte_budget,
            dependency_pins: self.dependency_pin_budget,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_explicit_worker_counts_and_resolves_automatic() {
        assert_eq!(
            CompilerSessionConfig::with_workers(0).unwrap().workers(),
            std::thread::available_parallelism()
                .map(std::num::NonZeroUsize::get)
                .unwrap_or(1)
        );
        assert!(matches!(
            CompilerSessionConfig::with_workers(MAX_QUERY_WORKERS + 1),
            Err(CompilerConfigurationError::WorkerCountOutOfRange { .. })
        ));
        assert!(matches!(
            CompilerSessionConfig::with_workers(usize::MAX),
            Err(CompilerConfigurationError::WorkerCountOutOfRange { .. })
        ));
    }

    #[test]
    fn accepts_soft_budget_extremes_without_allocating_them() {
        let configuration =
            CompilerSessionConfig::with_workers_and_retention(1, 0, u64::MAX).unwrap();
        assert_eq!(configuration.retained_byte_budget(), 0);
        assert_eq!(configuration.dependency_pin_budget(), u64::MAX);
    }
}
