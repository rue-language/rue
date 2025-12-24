//! libtest-compatible argument parser
//!
//! This does not drive parsing but provides [`TestOptsBuilder`] to plug into the parsing,
//! allowing additional parsers to be integrated.
//!
//! ## Example
//!
//! ```no_run
#![doc = include_str!("../examples/libtest-cli.rs")]
//! ```

#![cfg_attr(docsrs, feature(doc_auto_cfg))]
#![forbid(unsafe_code)]
#![warn(missing_debug_implementations, elided_lifetimes_in_paths)]

use lexarg::Arg;
use lexarg_error::LexError;

/// Parsed command-line options
///
/// To parse, see [`TestOptsBuilder`]
#[derive(Debug, Default)]
#[non_exhaustive]
pub struct TestOpts {
    pub list: bool,
    pub filters: Vec<String>,
    pub filter_exact: bool,
    pub run_ignored: RunIgnored,
    pub run_tests: bool,
    pub bench_benchmarks: bool,
    pub no_capture: bool,
    pub show_output: bool,
    pub color: ColorConfig,
    pub format: OutputFormat,
    pub test_threads: Option<std::num::NonZeroUsize>,
    pub skip: Vec<String>,
    /// Stop at first failing test.
    /// May run a few more tests due to threading, but will
    /// abort as soon as possible.
    pub fail_fast: bool,
    pub allowed_unstable: Vec<String>,
}

/// Whether ignored test should be run or not (see [`TestOpts::run_ignored`])
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum RunIgnored {
    Yes,
    No,
    /// Run only ignored tests
    Only,
}

impl Default for RunIgnored {
    fn default() -> Self {
        Self::No
    }
}

/// Whether should console output be colored or not (see [`TestOpts::color`])
#[derive(Copy, Clone, Debug)]
pub enum ColorConfig {
    AutoColor,
    AlwaysColor,
    NeverColor,
}

impl Default for ColorConfig {
    fn default() -> Self {
        Self::AutoColor
    }
}

/// Format of the test results output (see [`TestOpts::format`])
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum OutputFormat {
    /// Verbose output
    Pretty,
    /// Quiet output
    Terse,
    /// JSON output
    Json,
}

impl Default for OutputFormat {
    fn default() -> Self {
        Self::Pretty
    }
}

pub const UNSTABLE_OPTIONS: &str = "unstable-options";

pub const OPTIONS_HELP: &str = r#"
Arguments:
      [FILTER]...     Skip tests whose name does not match one of the filters

Options:
      --fail-fast     Don't start new tests after the first failure
      --skip FILTER   Skip tests whose names contain FILTER
                      (this flag can be used multiple times)
      --exact         Exactly match filters rather than by substring
      --ignored       Run only ignored tests
      --include-ignored 
                      Run ignored and not ignored tests
      --test          Run tests and not benchmarks
      --bench         Run benchmarks instead of tests
      --no-capture    don't capture stdout/stderr of each task,
                      allow printing directly
      --show-output   Show captured stdout of successful tests
      --list          List all tests and benchmarks
      --test-threads NUM
                      Number of threads used for running tests in parallel
                      (default is >1)
      --format <pretty|terse|json>
                      Configure formatting of output:
                      - pretty: Print verbose output
                      - terse: Display one character per test
                      - json: Output a json document
  -q, --quiet         Display one character per test instead of one line
                      (alias to --format=terse)
      --color <auto|always|never>
                      Configure coloring of output:
                      - auto: detect terminal support (default)
                      - always: always colorize output
                      - never: never colorize output
  -Z FLAG             Enable nightly-only flags:
                      - unstable-options: Allow use of experimental features
"#;

pub const AFTER_HELP: &str = r#""#;

/// Intermediate CLI parser state for [`TestOpts`]
///
/// See [`TestOptsBuilder::parse_next`]
#[derive(Debug, Default)]
pub struct TestOptsBuilder {
    opts: TestOpts,
    quiet: bool,
    format: Option<OutputFormat>,
    include_ignored: bool,
    ignored: bool,
}

impl TestOptsBuilder {
    pub fn new() -> Self {
        Default::default()
    }

    /// Check if `arg` is relevant to [`TestOpts`]
    pub fn parse_next<'a>(
        &mut self,
        parser: &mut lexarg::Parser<'a>,
        arg: Arg<'a>,
    ) -> Result<Option<Arg<'a>>, LexError<'a>> {
        use lexarg::prelude::*;

        match arg {
            Long("include-ignored") => {
                self.include_ignored = true;
            }
            Long("ignored") => self.ignored = true,
            Long("test") => {
                self.opts.run_tests = true;
            }
            Long("bench") => {
                self.opts.bench_benchmarks = true;
            }
            Long("list") => {
                self.opts.list = true;
            }
            Long("no-capture") => {
                self.opts.no_capture = true;
            }
            Long("test-threads") => {
                let test_threads = parser
                    .next_flag_value()
                    .ok_or_missing(Value(std::ffi::OsStr::new("NUM")))
                    .parse()
                    .within(arg)?;
                self.opts.test_threads = Some(test_threads);
            }
            Long("skip") => {
                let filter = parser
                    .next_flag_value()
                    .ok_or_missing(Value(std::ffi::OsStr::new("NAME")))
                    .string("NAME")
                    .within(arg)?;
                self.opts.skip.push(filter.to_owned());
            }
            Long("exact") => {
                self.opts.filter_exact = true;
            }
            Long("fail-fast") => {
                self.opts.fail_fast = true;
            }
            Long("color") => {
                let color = parser
                    .next_flag_value()
                    .ok_or_missing(Value(std::ffi::OsStr::new("WHEN")))
                    .one_of(&["auto", "always", "never"])
                    .within(arg)?;
                self.opts.color = match color {
                    "auto" => ColorConfig::AutoColor,
                    "always" => ColorConfig::AlwaysColor,
                    "never" => ColorConfig::NeverColor,
                    _ => unreachable!("`one_of` should prevent this"),
                };
            }
            Short("q") | Long("quiet") => {
                self.format = None;
                self.quiet = true;
            }
            Long("format") => {
                self.quiet = false;
                let format = parser
                    .next_flag_value()
                    .ok_or_missing(Value(std::ffi::OsStr::new("FORMAT")))
                    .one_of(&["pretty", "terse", "json"])
                    .within(arg)?;
                self.format = Some(match format {
                    "pretty" => OutputFormat::Pretty,
                    "terse" => OutputFormat::Terse,
                    "json" => OutputFormat::Json,
                    _ => unreachable!("`one_of` should prevent this"),
                });
            }
            Long("show-output") => {
                self.opts.show_output = true;
            }
            Short("Z") => {
                let feature = parser
                    .next_flag_value()
                    .ok_or_missing(Value(std::ffi::OsStr::new("FEATURE")))
                    .string("FEATURE")
                    .within(arg)?;
                if !is_nightly() {
                    return Err(LexError::msg("expected nightly compiler").unexpected(arg));
                }
                // Don't validate `feature` as other parsers might provide values
                self.opts.allowed_unstable.push(feature.to_owned());
            }
            Value(filter) => {
                let filter = filter.string("FILTER")?;
                self.opts.filters.push(filter.to_owned());
            }
            _ => {
                return Ok(Some(arg));
            }
        }
        Ok(None)
    }

    /// Finish parsing, resolving to [`TestOpts`]
    pub fn finish(mut self) -> Result<TestOpts, LexError<'static>> {
        let allow_unstable_options = self
            .opts
            .allowed_unstable
            .iter()
            .any(|f| f == UNSTABLE_OPTIONS);

        if self.format.is_some() && !allow_unstable_options {
            return Err(LexError::msg("`--format` requires `-Zunstable-options`"));
        }
        if let Some(format) = self.format {
            self.opts.format = format;
        } else if self.quiet {
            self.opts.format = OutputFormat::Terse;
        }

        self.opts.run_tests |= !self.opts.bench_benchmarks;

        self.opts.run_ignored = match (self.include_ignored, self.ignored) {
            (true, true) => {
                return Err(LexError::msg(
                    "`--include-ignored` and `--ignored` are mutually exclusive",
                ))
            }
            (true, false) => RunIgnored::Yes,
            (false, true) => RunIgnored::Only,
            (false, false) => RunIgnored::No,
        };

        let opts = self.opts;
        Ok(opts)
    }
}

// FIXME: Copied from librustc_ast until linkage errors are resolved. Issue #47566
fn is_nightly() -> bool {
    // Whether this is a feature-staged build, i.e., on the beta or stable channel
    let disable_unstable_features = option_env!("CFG_DISABLE_UNSTABLE_FEATURES")
        .map(|s| s != "0")
        .unwrap_or(false);
    // Whether we should enable unstable features for bootstrapping
    let bootstrap = std::env::var("RUSTC_BOOTSTRAP").is_ok();

    bootstrap || !disable_unstable_features
}

#[doc = include_str!("../README.md")]
#[cfg(doctest)]
pub struct ReadmeDoctests;
