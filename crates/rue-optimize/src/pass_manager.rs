//! Pass Manager Framework for MIR optimization passes
//!
//! This module provides a framework for managing and executing optimization passes
//! on MIR programs. It supports configurable pass ordering, fixed-point iteration,
//! optimization levels, and comprehensive pass statistics.

use rue_ir::mir::MirProgram;
use std::time::{Duration, Instant};
use tracing::{debug, info, trace, warn};

/// Result of running an optimization pass
#[derive(Debug, Clone, PartialEq)]
pub struct PassResult {
    /// Whether the pass made any changes to the program
    pub changed: bool,
    /// Time taken to execute the pass
    pub duration: Duration,
    /// Number of transformations applied (pass-specific)
    pub transformations: u32,
}

impl PassResult {
    /// Create a new PassResult indicating no changes
    pub fn no_change(duration: Duration) -> Self {
        Self {
            changed: false,
            duration,
            transformations: 0,
        }
    }

    /// Create a new PassResult indicating changes were made
    pub fn changed(duration: Duration, transformations: u32) -> Self {
        Self {
            changed: true,
            duration,
            transformations,
        }
    }
}

/// Trait for optimization passes that operate on MIR programs
pub trait Pass {
    /// Get the name of this pass for logging and statistics
    fn name(&self) -> &'static str;

    /// Run the pass on a MIR program, returning whether any changes were made
    ///
    /// This method should analyze and transform the program as needed, returning
    /// a PassResult that indicates whether changes were made and provides statistics.
    fn run(&mut self, program: &mut MirProgram) -> PassResult;

    /// Get a description of what this pass does (for debugging/logging)
    fn description(&self) -> &'static str {
        "No description available"
    }

    /// Whether this pass can be run multiple times safely
    /// Some passes might only be effective once, while others benefit from repetition
    fn can_repeat(&self) -> bool {
        true
    }
}

/// Configuration for pass execution
#[derive(Debug, Clone)]
pub struct PassConfig {
    /// Maximum number of iterations for fixed-point algorithm
    pub max_iterations: usize,
    /// Whether to enable detailed logging
    pub verbose_logging: bool,
    /// Whether to collect pass statistics
    pub collect_statistics: bool,
    /// Minimum time threshold for reporting slow passes (in milliseconds)
    pub slow_pass_threshold_ms: u64,
}

impl Default for PassConfig {
    fn default() -> Self {
        Self {
            max_iterations: 10,
            verbose_logging: false,
            collect_statistics: true,
            slow_pass_threshold_ms: 100,
        }
    }
}

/// Optimization level configuration
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OptimizationLevel {
    /// No optimizations (-O0)
    None,
    /// Basic optimizations (-O1)
    Basic,
    /// Full optimizations (-O2)
    Full,
}

impl OptimizationLevel {
    /// Get the maximum number of iterations for this optimization level
    pub fn max_iterations(self) -> usize {
        match self {
            OptimizationLevel::None => 1,
            OptimizationLevel::Basic => 3,
            OptimizationLevel::Full => 10,
        }
    }

    /// Get the pass configuration for this optimization level
    pub fn pass_config(self) -> PassConfig {
        PassConfig {
            max_iterations: self.max_iterations(),
            verbose_logging: matches!(self, OptimizationLevel::Full),
            ..Default::default()
        }
    }
}

/// Statistics collected during pass execution
#[derive(Debug, Clone, Default)]
pub struct PassStatistics {
    /// Total number of passes executed
    pub total_passes: u32,
    /// Total number of iterations
    pub total_iterations: u32,
    /// Total time spent in optimization
    pub total_duration: Duration,
    /// Per-pass statistics
    pub pass_stats: Vec<PassStat>,
    /// Whether fixed-point was reached
    pub converged: bool,
}

/// Statistics for an individual pass execution
#[derive(Debug, Clone)]
pub struct PassStat {
    /// Name of the pass
    pub name: String,
    /// Number of times the pass was executed
    pub executions: u32,
    /// Total time spent in this pass
    pub total_duration: Duration,
    /// Average time per execution
    pub avg_duration: Duration,
    /// Total transformations applied
    pub total_transformations: u32,
    /// Number of times the pass made changes
    pub times_changed: u32,
}

impl PassStatistics {
    /// Create a new empty statistics collection
    pub fn new() -> Self {
        Self::default()
    }

    /// Record the execution of a pass
    pub fn record_pass(&mut self, pass_name: &str, result: &PassResult) {
        self.total_passes += 1;

        // Find or create pass stat
        let pass_stat = self
            .pass_stats
            .iter_mut()
            .find(|stat| stat.name == pass_name);

        if let Some(stat) = pass_stat {
            stat.executions += 1;
            stat.total_duration += result.duration;
            stat.avg_duration = stat.total_duration / stat.executions;
            stat.total_transformations += result.transformations;
            if result.changed {
                stat.times_changed += 1;
            }
        } else {
            self.pass_stats.push(PassStat {
                name: pass_name.to_string(),
                executions: 1,
                total_duration: result.duration,
                avg_duration: result.duration,
                total_transformations: result.transformations,
                times_changed: if result.changed { 1 } else { 0 },
            });
        }
    }

    /// Log a summary of the collected statistics
    pub fn log_summary(&self) {
        info!(
            target: "rue::optimize::stats",
            total_passes = self.total_passes,
            total_iterations = self.total_iterations,
            total_duration_ms = self.total_duration.as_millis(),
            converged = self.converged,
            "Pass manager execution summary"
        );

        for stat in &self.pass_stats {
            debug!(
                target: "rue::optimize::stats",
                pass = %stat.name,
                executions = stat.executions,
                total_duration_ms = stat.total_duration.as_millis(),
                avg_duration_ms = stat.avg_duration.as_millis(),
                transformations = stat.total_transformations,
                times_changed = stat.times_changed,
                "Pass statistics"
            );
        }
    }
}

/// The main pass manager that orchestrates optimization pass execution
pub struct PassManager {
    /// Configuration for pass execution
    config: PassConfig,
    /// Collected statistics (if enabled)
    statistics: PassStatistics,
}

impl PassManager {
    /// Create a new pass manager with default configuration
    pub fn new() -> Self {
        Self {
            config: PassConfig::default(),
            statistics: PassStatistics::new(),
        }
    }

    /// Create a new pass manager with the given configuration
    pub fn with_config(config: PassConfig) -> Self {
        Self {
            config,
            statistics: PassStatistics::new(),
        }
    }

    /// Create a new pass manager for the given optimization level
    pub fn for_optimization_level(level: OptimizationLevel) -> Self {
        Self::with_config(level.pass_config())
    }

    /// Run a sequence of passes to fixed-point
    ///
    /// This method runs the given passes repeatedly until either:
    /// 1. No pass makes any changes (fixed-point reached)
    /// 2. Maximum iterations exceeded
    pub fn run_to_fixpoint<P>(
        &mut self,
        program: &mut MirProgram,
        passes: &mut [P],
    ) -> &PassStatistics
    where
        P: Pass,
    {
        let start_time = Instant::now();

        info!(
            target: "rue::optimize",
            max_iterations = self.config.max_iterations,
            num_passes = passes.len(),
            "Starting optimization pass manager"
        );

        for iteration in 0..self.config.max_iterations {
            let iteration_start = Instant::now();
            let mut any_changed = false;

            if self.config.verbose_logging {
                debug!(
                    target: "rue::optimize",
                    iteration = iteration + 1,
                    "Starting optimization iteration"
                );
            }

            for (pass_idx, pass) in passes.iter_mut().enumerate() {
                let pass_name = pass.name();

                trace!(
                    target: "rue::optimize",
                    pass = %pass_name,
                    iteration = iteration + 1,
                    pass_index = pass_idx,
                    "Running pass"
                );

                let result = pass.run(program);

                if result.changed {
                    any_changed = true;
                    if self.config.verbose_logging {
                        debug!(
                            target: "rue::optimize",
                            pass = %pass_name,
                            transformations = result.transformations,
                            duration_ms = result.duration.as_millis(),
                            "Pass made changes"
                        );
                    }
                } else if self.config.verbose_logging {
                    trace!(
                        target: "rue::optimize",
                        pass = %pass_name,
                        duration_ms = result.duration.as_millis(),
                        "Pass made no changes"
                    );
                }

                // Warn about slow passes
                if result.duration.as_millis() > self.config.slow_pass_threshold_ms as u128 {
                    warn!(
                        target: "rue::optimize",
                        pass = %pass_name,
                        duration_ms = result.duration.as_millis(),
                        "Slow pass detected"
                    );
                }

                // Record statistics
                if self.config.collect_statistics {
                    self.statistics.record_pass(pass_name, &result);
                }
            }

            self.statistics.total_iterations += 1;

            let iteration_duration = iteration_start.elapsed();
            if self.config.verbose_logging {
                debug!(
                    target: "rue::optimize",
                    iteration = iteration + 1,
                    duration_ms = iteration_duration.as_millis(),
                    changed = any_changed,
                    "Completed optimization iteration"
                );
            }

            // Check for convergence
            if !any_changed {
                info!(
                    target: "rue::optimize",
                    iterations = iteration + 1,
                    "Optimization converged (fixed-point reached)"
                );
                self.statistics.converged = true;
                break;
            }
        }

        self.statistics.total_duration = start_time.elapsed();

        if !self.statistics.converged {
            warn!(
                target: "rue::optimize",
                max_iterations = self.config.max_iterations,
                "Optimization did not converge within maximum iterations"
            );
        }

        info!(
            target: "rue::optimize",
            total_duration_ms = self.statistics.total_duration.as_millis(),
            total_passes = self.statistics.total_passes,
            iterations = self.statistics.total_iterations,
            converged = self.statistics.converged,
            "Completed optimization pass manager"
        );

        // Log detailed statistics if requested
        if self.config.collect_statistics {
            self.statistics.log_summary();
        }

        &self.statistics
    }

    /// Run passes once (no iteration)
    pub fn run_once<P>(&mut self, program: &mut MirProgram, passes: &mut [P])
    where
        P: Pass,
    {
        let start_time = Instant::now();

        info!(
            target: "rue::optimize",
            num_passes = passes.len(),
            "Running optimization passes once (no iteration)"
        );

        for (pass_idx, pass) in passes.iter_mut().enumerate() {
            let pass_name = pass.name();

            trace!(
                target: "rue::optimize",
                pass = %pass_name,
                pass_index = pass_idx,
                "Running pass"
            );

            let result = pass.run(program);

            if result.changed {
                if self.config.verbose_logging {
                    debug!(
                        target: "rue::optimize",
                        pass = %pass_name,
                        transformations = result.transformations,
                        duration_ms = result.duration.as_millis(),
                        "Pass made changes"
                    );
                }
            } else if self.config.verbose_logging {
                trace!(
                    target: "rue::optimize",
                    pass = %pass_name,
                    duration_ms = result.duration.as_millis(),
                    "Pass made no changes"
                );
            }

            // Record statistics
            if self.config.collect_statistics {
                self.statistics.record_pass(pass_name, &result);
            }
        }

        self.statistics.total_iterations = 1;
        self.statistics.total_duration = start_time.elapsed();
        self.statistics.converged = true; // Single iteration is always "converged"

        info!(
            target: "rue::optimize",
            total_duration_ms = self.statistics.total_duration.as_millis(),
            total_passes = self.statistics.total_passes,
            "Completed single-pass optimization"
        );
    }

    /// Get a reference to the collected statistics
    pub fn statistics(&self) -> &PassStatistics {
        &self.statistics
    }

    /// Get a mutable reference to the configuration
    pub fn config_mut(&mut self) -> &mut PassConfig {
        &mut self.config
    }

    /// Reset statistics (useful for running multiple programs)
    pub fn reset_statistics(&mut self) {
        self.statistics = PassStatistics::new();
    }
}

impl Default for PassManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rue_ir::mir::{BasicBlock, BlockId, MirFunction, MirTerminator};
    use rue_ir::types::RueType;
    use rue_lexer::Span;
    use std::collections::HashMap;

    /// A simple test pass that tracks how many times it runs
    struct TestPass {
        name: &'static str,
        change_iterations: Vec<bool>, // Whether to make changes on each iteration
        current_iteration: usize,
    }

    impl TestPass {
        fn new(name: &'static str, change_iterations: Vec<bool>) -> Self {
            Self {
                name,
                change_iterations,
                current_iteration: 0,
            }
        }
    }

    impl Pass for TestPass {
        fn name(&self) -> &'static str {
            self.name
        }

        fn run(&mut self, _program: &mut MirProgram) -> PassResult {
            let start = Instant::now();

            let should_change = self
                .change_iterations
                .get(self.current_iteration)
                .copied()
                .unwrap_or(false);

            self.current_iteration += 1;

            // Simulate some work
            std::thread::sleep(Duration::from_millis(1));

            let duration = start.elapsed();

            if should_change {
                PassResult::changed(duration, 1)
            } else {
                PassResult::no_change(duration)
            }
        }

        fn description(&self) -> &'static str {
            "Test pass for unit testing"
        }
    }

    fn create_test_program() -> MirProgram {
        MirProgram {
            functions: vec![MirFunction {
                name: "test".to_string(),
                params: vec![],
                return_type: RueType::I32,
                entry_block: BlockId(0),
                span: Span::dummy(),
                temp_types: HashMap::new(),
                blocks: vec![BasicBlock {
                    id: BlockId(0),
                    params: vec![],
                    statements: vec![],
                    terminator: MirTerminator::Return {
                        value: None,
                        span: None,
                    },
                }],
            }],
            function_signatures: HashMap::new(),
            type_context: rue_ir::types::TypeContext::new(),
        }
    }

    #[test]
    fn test_fixed_point_convergence() {
        let mut manager = PassManager::new();
        let mut program = create_test_program();

        // Create passes that make changes on first iteration only
        let mut passes = vec![
            TestPass::new("pass1", vec![true, false, false]),
            TestPass::new("pass2", vec![true, false, false]),
        ];

        let stats = manager.run_to_fixpoint(&mut program, &mut passes);

        // Should converge after 2 iterations
        assert_eq!(stats.total_iterations, 2);
        assert!(stats.converged);
        assert_eq!(stats.pass_stats.len(), 2);
    }

    #[test]
    fn test_max_iterations_limit() {
        let config = PassConfig {
            max_iterations: 3,
            ..Default::default()
        };
        let mut manager = PassManager::with_config(config);
        let mut program = create_test_program();

        // Create passes that always make changes
        let mut passes = vec![
            TestPass::new("pass1", vec![true, true, true, true, true]),
            TestPass::new("pass2", vec![true, true, true, true, true]),
        ];

        let stats = manager.run_to_fixpoint(&mut program, &mut passes);

        // Should stop at max iterations without converging
        assert_eq!(stats.total_iterations, 3);
        assert!(!stats.converged);
    }

    #[test]
    fn test_optimization_levels() {
        // Test different optimization levels
        for level in [
            OptimizationLevel::None,
            OptimizationLevel::Basic,
            OptimizationLevel::Full,
        ] {
            let mut manager = PassManager::for_optimization_level(level);
            let mut program = create_test_program();
            let mut passes = vec![TestPass::new("test", vec![false])];

            let stats = manager.run_to_fixpoint(&mut program, &mut passes);

            assert_eq!(stats.total_iterations, 1);
            assert!(stats.converged);
        }
    }

    #[test]
    fn test_pass_statistics() {
        let mut manager = PassManager::new();
        let mut program = create_test_program();

        let mut passes = vec![
            TestPass::new("pass1", vec![true, false]),
            TestPass::new("pass2", vec![false, false]),
        ];

        let stats = manager.run_to_fixpoint(&mut program, &mut passes);

        // Check overall statistics
        assert_eq!(stats.total_passes, 4); // 2 passes × 2 iterations
        assert_eq!(stats.total_iterations, 2);
        assert!(stats.converged);

        // Check per-pass statistics
        let pass1_stats = stats.pass_stats.iter().find(|s| s.name == "pass1").unwrap();
        assert_eq!(pass1_stats.executions, 2);
        assert_eq!(pass1_stats.times_changed, 1);
        assert_eq!(pass1_stats.total_transformations, 1);

        let pass2_stats = stats.pass_stats.iter().find(|s| s.name == "pass2").unwrap();
        assert_eq!(pass2_stats.executions, 2);
        assert_eq!(pass2_stats.times_changed, 0);
        assert_eq!(pass2_stats.total_transformations, 0);
    }
}
