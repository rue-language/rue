pub(crate) use crate::*;

pub trait Case: Send + Sync + 'static {
    /// The name of a test
    ///
    /// By convention this follows the rules for rust paths; i.e., it should be a series of
    /// identifiers separated by double colons. This way if some test runner wants to arrange the
    /// tests hierarchically it may.
    fn name(&self) -> &str;
    fn kind(&self) -> TestKind;
    fn source(&self) -> Option<&Source>;
    /// This case cannot run in parallel to other cases within this binary
    fn exclusive(&self, state: &TestContext) -> bool;

    fn run(&self, state: &TestContext) -> Result<(), RunError>;
}

/// Type of the test according to the [rust book](https://doc.rust-lang.org/cargo/guide/tests.html)
/// conventions.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum TestKind {
    /// Unit-tests are expected to be in the `src` folder of the crate.
    UnitTest,
    /// Integration-style tests are expected to be in the `tests` folder of the crate.
    IntegrationTest,
    /// Doctests are created by the `librustdoc` manually, so it's a different type of test.
    DocTest,
    /// Tests for the sources that don't follow the project layout convention
    /// (e.g. tests in raw `main.rs` compiled by calling `rustc --test` directly).
    Unknown,
}

impl Default for TestKind {
    fn default() -> Self {
        Self::Unknown
    }
}

#[derive(Debug)]
#[non_exhaustive]
pub enum Source {
    Rust {
        source_file: std::path::PathBuf,
        start_line: usize,
        start_col: usize,
        end_line: usize,
        end_col: usize,
    },
    Path(std::path::PathBuf),
}

pub type RunResult = Result<(), RunError>;

#[derive(Debug)]
pub struct RunError {
    status: notify::MessageKind,
    cause: Option<Box<dyn std::error::Error + Send + Sync + 'static>>,
}

impl RunError {
    pub fn with_cause(cause: impl std::error::Error + Send + Sync + 'static) -> Self {
        Self {
            status: notify::MessageKind::Error,
            cause: Some(Box::new(cause)),
        }
    }

    pub fn fail(cause: impl std::fmt::Display) -> Self {
        Self::with_cause(Message(cause.to_string()))
    }

    /// Should not be called with `libtest_lexarg::RunIgnored::Yes`
    pub fn ignore() -> Self {
        Self {
            status: notify::MessageKind::Ignored,
            cause: None,
        }
    }

    /// Should not be called with `libtest_lexarg::RunIgnored::Yes`
    pub fn ignore_for(reason: String) -> Self {
        Self {
            status: notify::MessageKind::Ignored,
            cause: Some(Box::new(Message(reason))),
        }
    }

    pub(crate) fn status(&self) -> notify::MessageKind {
        self.status
    }

    pub(crate) fn cause(&self) -> Option<&(dyn std::error::Error + Send + Sync)> {
        self.cause.as_ref().map(|b| b.as_ref())
    }
}

impl<E> From<E> for RunError
where
    E: std::error::Error + Send + Sync + 'static,
{
    fn from(error: E) -> Self {
        Self::with_cause(error)
    }
}

#[derive(Debug)]
struct Message(String);

impl std::fmt::Display for Message {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(formatter)
    }
}

impl std::error::Error for Message {}
