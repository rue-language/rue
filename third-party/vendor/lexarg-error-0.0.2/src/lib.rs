//! Error type for use with lexarg
//!
//! Inspired by [lexopt](https://crates.io/crates/lexopt), `lexarg` simplifies the formula down
//! further so it can be used for CLI plugin systems.
//!
//! ## Example
//!
//! ```no_run
#![doc = include_str!("../examples/hello-error.rs")]
//! ```

#![cfg_attr(docsrs, feature(doc_auto_cfg))]
#![warn(missing_debug_implementations)]
#![warn(missing_docs)]
#![warn(clippy::print_stderr)]
#![warn(clippy::print_stdout)]

/// Collect context for creating an error
#[derive(Debug)]
pub struct LexError<'a> {
    msg: String,
    within: Option<lexarg_parser::Arg<'a>>,
    unexpected: Option<lexarg_parser::Arg<'a>>,
}

impl<'a> LexError<'a> {
    /// Create a new error object from a printable error message.
    #[cold]
    pub fn msg<M>(message: M) -> Self
    where
        M: std::fmt::Display,
    {
        Self {
            msg: message.to_string(),
            within: None,
            unexpected: None,
        }
    }

    /// [`Arg`][lexarg_parser::Arg] the error occurred within
    #[cold]
    pub fn within(mut self, within: lexarg_parser::Arg<'a>) -> Self {
        self.within = Some(within);
        self
    }

    /// The failing [`Arg`][lexarg_parser::Arg]
    #[cold]
    pub fn unexpected(mut self, unexpected: lexarg_parser::Arg<'a>) -> Self {
        self.unexpected = Some(unexpected);
        self
    }
}

impl<E> From<E> for LexError<'_>
where
    E: std::error::Error + Send + Sync + 'static,
{
    #[cold]
    fn from(error: E) -> Self {
        Self::msg(error)
    }
}

impl std::fmt::Display for LexError<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.msg.fmt(formatter)?;
        if let Some(unexpected) = &self.unexpected {
            write!(formatter, ", found `")?;
            match unexpected {
                lexarg_parser::Arg::Short(short) => write!(formatter, "-{short}")?,
                lexarg_parser::Arg::Long(long) => write!(formatter, "--{long}")?,
                lexarg_parser::Arg::Escape(value) => write!(formatter, "{value}")?,
                lexarg_parser::Arg::Value(value) | lexarg_parser::Arg::Unexpected(value) => {
                    write!(formatter, "{}", value.to_string_lossy())?;
                }
            }
            write!(formatter, "`")?;
        }
        if let Some(within) = &self.within {
            write!(formatter, " when parsing `")?;
            match within {
                lexarg_parser::Arg::Short(short) => write!(formatter, "-{short}")?,
                lexarg_parser::Arg::Long(long) => write!(formatter, "--{long}")?,
                lexarg_parser::Arg::Escape(value) => write!(formatter, "{value}")?,
                lexarg_parser::Arg::Value(value) | lexarg_parser::Arg::Unexpected(value) => {
                    write!(formatter, "{}", value.to_string_lossy())?;
                }
            }
            write!(formatter, "`")?;
        }
        Ok(())
    }
}

#[doc = include_str!("../README.md")]
#[cfg(doctest)]
pub struct ReadmeDoctests;
