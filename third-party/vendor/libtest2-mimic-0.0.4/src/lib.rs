//! An experimental replacement for
//! [libtest-mimic](https://docs.rs/libtest-mimic/latest/libtest_mimic/)
//!
//! Write your own tests that look and behave like built-in tests!
//!
//! This is a simple and small test harness that mimics the original `libtest`
//! (used by `cargo test`/`rustc --test`). That means: all output looks pretty
//! much like `cargo test` and most CLI arguments are understood and used. With
//! that plumbing work out of the way, your test runner can focus on the actual
//! testing.
//!
//! For a small real world example, see [`examples/mimic-tidy.rs`][1].
//!
//! [1]: https://github.com/assert-rs/libtest2/blob/main/crates/libtest2-mimic/examples/mimic-tidy.rs
//!
//! # Usage
//!
//! To use this, you most likely want to add a manual `[[test]]` section to
//! `Cargo.toml` and set `harness = false`. For example:
//!
//! ```toml
//! [[test]]
//! name = "mytest"
//! path = "tests/mytest.rs"
//! harness = false
//! ```
//!
//! And in `tests/mytest.rs` you would call [`Harness::main`] in the `main` function:
//!
//! ```no_run
//! # use libtest2_mimic::Trial;
//! # use libtest2_mimic::Harness;
//! # use libtest2_mimic::RunError;
//! Harness::with_env()
//!     .discover([
//!         Trial::test("succeeding_test", move |_| Ok(())),
//!         Trial::test("failing_test", move |_| Err(RunError::fail("Woops"))),
//!     ])
//!     .main();
//! ```
//! Instead of returning `Ok` or `Err` directly, you want to actually perform
//! your tests, of course. See [`Trial::test`] for more information on how to
//! define a test. You can of course list all your tests manually. But in many
//! cases it is useful to generate one test per file in a directory, for
//! example.
//!
//! You can then run `cargo test --test mytest` to run it. To see the CLI
//! arguments supported by this crate, run `cargo test --test mytest -- -h`.
//!
//! # Known limitations and differences to the official test harness
//!
//! `libtest2-mimic` aims to be fully compatible with stable, non-deprecated parts of `libtest`
//! but there are differences for now.
//!
//! Some of the notable differences:
//!
//! - Output capture and `--no-capture`: simply not supported. The official
//!   `libtest` uses internal `std` functions to temporarily redirect output.
//!   `libtest-mimic` cannot use those, see also [libtest2#12](https://github.com/assert-rs/libtest2/issues/12)
//! - `--format=json` (unstable): our schema is part of an experiment to see what should be
//!   stabilized for `libtest`, see also [libtest2#42](https://github.com/assert-rs/libtest2/issues/42)

#![cfg_attr(docsrs, feature(doc_auto_cfg))]
//#![warn(clippy::print_stderr)]
#![warn(clippy::print_stdout)]

pub struct Harness {
    raw: Vec<std::ffi::OsString>,
    cases: Vec<Trial>,
}

impl Harness {
    /// Read the process's CLI arguments
    pub fn with_env() -> Self {
        let raw = std::env::args_os();
        Self::with_args(raw)
    }

    /// Manually specify CLI arguments
    pub fn with_args(args: impl IntoIterator<Item = impl Into<std::ffi::OsString>>) -> Self {
        Self {
            raw: args.into_iter().map(|a| a.into()).collect(),
            cases: Vec::new(),
        }
    }

    /// Enumerate all test [`Trial`]s
    pub fn discover(mut self, cases: impl IntoIterator<Item = Trial>) -> Self {
        self.cases.extend(cases);
        self
    }

    /// Perform the tests and exit
    pub fn main(self) -> ! {
        match self.run() {
            Ok(true) => std::process::exit(0),
            Ok(false) => std::process::exit(libtest2_harness::ERROR_EXIT_CODE),
            Err(err) => {
                eprintln!("{err}");
                std::process::exit(libtest2_harness::ERROR_EXIT_CODE)
            }
        }
    }

    fn run(self) -> std::io::Result<bool> {
        let harness = libtest2_harness::Harness::new();
        let harness = match harness.with_args(self.raw) {
            Ok(harness) => harness,
            Err(err) => {
                eprintln!("{err}");
                std::process::exit(1);
            }
        };
        let harness = match harness.parse() {
            Ok(harness) => harness,
            Err(err) => {
                eprintln!("{err}");
                std::process::exit(1);
            }
        };
        let harness = harness.discover(self.cases.into_iter().map(|t| TrialCase { inner: t }))?;
        harness.run()
    }
}

/// A test case to be run
pub struct Trial {
    name: String,
    #[allow(clippy::type_complexity)]
    runner: Box<dyn Fn(RunContext<'_>) -> Result<(), RunError> + Send + Sync>,
}

impl Trial {
    pub fn test(
        name: impl Into<String>,
        runner: impl Fn(RunContext<'_>) -> Result<(), RunError> + Send + Sync + 'static,
    ) -> Self {
        Self {
            name: name.into(),
            runner: Box::new(runner),
        }
    }
}

struct TrialCase {
    inner: Trial,
}

impl libtest2_harness::Case for TrialCase {
    fn name(&self) -> &str {
        &self.inner.name
    }
    fn kind(&self) -> libtest2_harness::TestKind {
        Default::default()
    }
    fn source(&self) -> Option<&libtest2_harness::Source> {
        None
    }
    fn exclusive(&self, _: &libtest2_harness::TestContext) -> bool {
        false
    }

    fn run(
        &self,
        context: &libtest2_harness::TestContext,
    ) -> Result<(), libtest2_harness::RunError> {
        (self.inner.runner)(RunContext { inner: context }).map_err(|e| e.inner)
    }
}

pub type RunResult = Result<(), RunError>;

#[derive(Debug)]
pub struct RunError {
    inner: libtest2_harness::RunError,
}

impl RunError {
    pub fn with_cause(cause: impl std::error::Error + Send + Sync + 'static) -> Self {
        Self {
            inner: libtest2_harness::RunError::with_cause(cause),
        }
    }

    pub fn fail(cause: impl std::fmt::Display) -> Self {
        Self {
            inner: libtest2_harness::RunError::fail(cause),
        }
    }
}

pub struct RunContext<'t> {
    inner: &'t libtest2_harness::TestContext,
}

impl<'t> RunContext<'t> {
    /// Request this test to be ignored
    ///
    /// May be overridden by the CLI
    ///
    /// **Note:** prefer [`RunContext::ignore_for`]
    pub fn ignore(&self) -> Result<(), RunError> {
        self.inner.ignore().map_err(|e| RunError { inner: e })
    }

    /// Request this test to be ignored
    ///
    /// May be overridden by the CLI
    pub fn ignore_for(&self, reason: impl std::fmt::Display) -> Result<(), RunError> {
        self.inner
            .ignore_for(reason)
            .map_err(|e| RunError { inner: e })
    }
}

#[doc = include_str!("../README.md")]
#[cfg(doctest)]
pub struct ReadmeDoctests;
