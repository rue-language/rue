//! Minimal, API stable CLI parser
//!
//! Inspired by [lexopt](https://crates.io/crates/lexopt), `lexarg` simplifies the formula down
//! further so it can be used for CLI plugin systems.
//!
//! ## Example
//!
//! ```no_run
#![doc = include_str!("../examples/hello-parser.rs")]
//! ```

#![cfg_attr(docsrs, feature(doc_auto_cfg))]
#![allow(clippy::result_unit_err)]
#![warn(missing_debug_implementations)]
#![warn(missing_docs)]
#![warn(clippy::print_stderr)]
#![warn(clippy::print_stdout)]

mod ext;

use std::ffi::OsStr;

use ext::OsStrExt as _;

/// A parser for command line arguments.
#[derive(Debug, Clone)]
pub struct Parser<'a> {
    raw: &'a dyn RawArgs,
    current: usize,
    state: Option<State<'a>>,
    was_attached: bool,
}

impl<'a> Parser<'a> {
    /// Create a parser from an iterator. This is useful for testing among other things.
    ///
    /// The first item from the iterator **must** be the binary name, as from [`std::env::args_os`].
    ///
    /// The iterator is consumed immediately.
    ///
    /// # Example
    /// ```
    /// let args = ["myapp", "-n", "10", "./foo.bar"];
    /// let mut parser = lexarg_parser::Parser::new(&&args[1..]);
    /// ```
    pub fn new(raw: &'a dyn RawArgs) -> Self {
        Parser {
            raw,
            current: 0,
            state: None,
            was_attached: false,
        }
    }

    /// Get the next option or positional [`Arg`].
    ///
    /// Returns `None` if the command line has been exhausted.
    ///
    /// Returns [`Arg::Unexpected`] on failure
    ///
    /// Notes:
    /// - `=` is always accepted as a [`Arg::Short("=")`].  If that isn't the case in your
    ///   application, you may want to special case the error for that.
    pub fn next_arg(&mut self) -> Option<Arg<'a>> {
        // Always reset
        self.was_attached = false;

        match self.state {
            Some(State::PendingValue(attached)) => {
                // Last time we got `--long=value`, and `value` hasn't been used.
                self.state = None;
                self.current += 1;
                Some(Arg::Unexpected(attached))
            }
            Some(State::PendingShorts(valid, invalid, index)) => {
                // We're somewhere inside a `-abc` chain. Because we're in `.next_arg()`, not `.next_flag_value()`, we
                // can assume that the next character is another option.
                if let Some(next_index) = ceil_char_boundary(valid, index) {
                    if next_index < valid.len() {
                        self.state = Some(State::PendingShorts(valid, invalid, next_index));
                    } else if !invalid.is_empty() {
                        self.state = Some(State::PendingValue(invalid));
                    } else {
                        // No more flags
                        self.state = None;
                        self.current += 1;
                    }
                    let flag = &valid[index..next_index];
                    Some(Arg::Short(flag))
                } else {
                    debug_assert_ne!(invalid, "");
                    if index == 0 {
                        panic!("there should have been a `-`")
                    } else if index == 1 {
                        // Like long flags, include `-`
                        let arg = self
                            .raw
                            .get(self.current)
                            .expect("`current` is valid if state is `Shorts`");
                        self.state = None;
                        self.current += 1;
                        Some(Arg::Unexpected(arg))
                    } else {
                        self.state = None;
                        self.current += 1;
                        Some(Arg::Unexpected(invalid))
                    }
                }
            }
            Some(State::Escaped) => {
                self.state = Some(State::Escaped);
                self.next_raw_().map(Arg::Value)
            }
            None => {
                let arg = self.raw.get(self.current)?;
                if arg == "--" {
                    self.state = Some(State::Escaped);
                    self.current += 1;
                    Some(Arg::Escape(arg.to_str().expect("`--` is valid UTF-8")))
                } else if arg == "-" {
                    self.state = None;
                    self.current += 1;
                    Some(Arg::Value(arg))
                } else if let Some(long) = arg.strip_prefix("--") {
                    let (name, value) = long
                        .split_once("=")
                        .map(|(n, v)| (n, Some(v)))
                        .unwrap_or((long, None));
                    if name.is_empty() {
                        self.state = None;
                        self.current += 1;
                        Some(Arg::Unexpected(arg))
                    } else if let Ok(name) = name.try_str() {
                        if let Some(value) = value {
                            self.state = Some(State::PendingValue(value));
                        } else {
                            self.state = None;
                            self.current += 1;
                        }
                        Some(Arg::Long(name))
                    } else {
                        self.state = None;
                        self.current += 1;
                        Some(Arg::Unexpected(arg))
                    }
                } else if arg.starts_with("-") {
                    let (valid, invalid) = split_nonutf8_once(arg);
                    let invalid = invalid.unwrap_or_default();
                    self.state = Some(State::PendingShorts(valid, invalid, 1));
                    self.next_arg()
                } else {
                    self.state = None;
                    self.current += 1;
                    Some(Arg::Value(arg))
                }
            }
        }
    }

    /// Get a flag's value
    ///
    /// This function should normally be called right after seeing a flag that expects a value;
    /// positional arguments should be collected with [`Parser::next_arg()`].
    ///
    /// A value is collected even if it looks like an option (i.e., starts with `-`).
    ///
    /// `None` is returned if there is not another applicable flag value, including:
    /// - No more arguments are present
    /// - `--` was encountered, meaning all remaining arguments are positional
    /// - Being called again when the first value was attached (`--flag=value`, `-Fvalue`, `-F=value`)
    pub fn next_flag_value(&mut self) -> Option<&'a OsStr> {
        if self.was_attached {
            debug_assert!(!self.has_pending());
            None
        } else if let Some(value) = self.next_attached_value() {
            Some(value)
        } else {
            self.next_detached_value()
        }
    }

    /// Get a flag's attached value (`--flag=value`, `-Fvalue`, `-F=value`)
    ///
    /// This is a more specialized variant of [`Parser::next_flag_value`] for when only attached
    /// values are allowed, e.g. `--color[=<when>]`.
    pub fn next_attached_value(&mut self) -> Option<&'a OsStr> {
        match self.state? {
            State::PendingValue(attached) => {
                self.state = None;
                self.current += 1;
                self.was_attached = true;
                Some(attached)
            }
            State::PendingShorts(_, _, index) => {
                let arg = self
                    .raw
                    .get(self.current)
                    .expect("`current` is valid if state is `Shorts`");
                self.state = None;
                self.current += 1;
                if index == arg.len() {
                    None
                } else {
                    // SAFETY: everything preceding `index` were a short flags, making them valid UTF-8
                    let remainder = unsafe { ext::split_at(arg, index) }.1;
                    let remainder = remainder.strip_prefix("=").unwrap_or(remainder);
                    self.was_attached = true;
                    Some(remainder)
                }
            }
            State::Escaped => None,
        }
    }

    fn next_detached_value(&mut self) -> Option<&'a OsStr> {
        if self.state == Some(State::Escaped) {
            // Escaped values are positional-only
            return None;
        }

        if self.peek_raw_()? == "--" {
            None
        } else {
            self.next_raw_()
        }
    }

    /// Get the next argument, independent of what it looks like
    ///
    /// Returns `Err(())` if an [attached value][Parser::next_attached_value] is present
    pub fn next_raw(&mut self) -> Result<Option<&'a OsStr>, ()> {
        if self.has_pending() {
            Err(())
        } else {
            self.was_attached = false;
            Ok(self.next_raw_())
        }
    }

    /// Collect all remaining arguments, independent of what they look like
    ///
    /// Returns `Err(())` if an [attached value][Parser::next_attached_value] is present
    pub fn remaining_raw(&mut self) -> Result<impl Iterator<Item = &'a OsStr> + '_, ()> {
        if self.has_pending() {
            Err(())
        } else {
            self.was_attached = false;
            Ok(std::iter::from_fn(|| self.next_raw_()))
        }
    }

    /// Get the next argument, independent of what it looks like
    ///
    /// Returns `Err(())` if an [attached value][Parser::next_attached_value] is present
    pub fn peek_raw(&self) -> Result<Option<&'a OsStr>, ()> {
        if self.has_pending() {
            Err(())
        } else {
            Ok(self.peek_raw_())
        }
    }

    fn peek_raw_(&self) -> Option<&'a OsStr> {
        self.raw.get(self.current)
    }

    fn next_raw_(&mut self) -> Option<&'a OsStr> {
        debug_assert!(!self.has_pending());
        debug_assert!(!self.was_attached);

        let next = self.raw.get(self.current)?;
        self.current += 1;
        Some(next)
    }

    fn has_pending(&self) -> bool {
        self.state.as_ref().map(State::has_pending).unwrap_or(false)
    }
}

/// Accessor for unparsed arguments
pub trait RawArgs: std::fmt::Debug + private::Sealed {
    /// Returns a reference to an element or subslice depending on the type of index.
    ///
    /// - If given a position, returns a reference to the element at that position or None if out
    ///   of bounds.
    /// - If given a range, returns the subslice corresponding to that range, or None if out
    ///   of bounds.
    fn get(&self, index: usize) -> Option<&OsStr>;

    /// Returns the number of elements in the slice.
    fn len(&self) -> usize;

    /// Returns `true` if the slice has a length of 0.
    fn is_empty(&self) -> bool;
}

impl<const C: usize, S> RawArgs for [S; C]
where
    S: AsRef<OsStr> + std::fmt::Debug,
{
    #[inline]
    fn get(&self, index: usize) -> Option<&OsStr> {
        self.as_slice().get(index).map(|s| s.as_ref())
    }

    #[inline]
    fn len(&self) -> usize {
        C
    }

    #[inline]
    fn is_empty(&self) -> bool {
        C != 0
    }
}

impl<S> RawArgs for &'_ [S]
where
    S: AsRef<OsStr> + std::fmt::Debug,
{
    #[inline]
    fn get(&self, index: usize) -> Option<&OsStr> {
        (*self).get(index).map(|s| s.as_ref())
    }

    #[inline]
    fn len(&self) -> usize {
        (*self).len()
    }

    #[inline]
    fn is_empty(&self) -> bool {
        (*self).is_empty()
    }
}

impl<S> RawArgs for Vec<S>
where
    S: AsRef<OsStr> + std::fmt::Debug,
{
    #[inline]
    fn get(&self, index: usize) -> Option<&OsStr> {
        self.as_slice().get(index).map(|s| s.as_ref())
    }

    #[inline]
    fn len(&self) -> usize {
        self.len()
    }

    #[inline]
    fn is_empty(&self) -> bool {
        self.is_empty()
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
enum State<'a> {
    /// We have a value left over from `--option=value`
    PendingValue(&'a OsStr),
    /// We're in the middle of `-abc`
    ///
    /// On Windows and other non-UTF8-OsString platforms this Vec should
    /// only ever contain valid UTF-8 (and could instead be a String).
    PendingShorts(&'a str, &'a OsStr, usize),
    /// We saw `--` and know no more options are coming.
    Escaped,
}

impl State<'_> {
    fn has_pending(&self) -> bool {
        match self {
            Self::PendingValue(_) | Self::PendingShorts(_, _, _) => true,
            Self::Escaped => false,
        }
    }
}

/// A command line argument found by [`Parser`], either an option or a positional argument
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum Arg<'a> {
    /// A short option, e.g. `Short("q")` for `-q`
    Short(&'a str),
    /// A long option, e.g. `Long("verbose")` for `--verbose`
    ///
    /// The dashes are not included
    Long(&'a str),
    /// A positional argument, e.g. `/dev/null`
    Value(&'a OsStr),
    /// Marks the following values have been escaped with `--`
    Escape(&'a str),
    /// User passed something in that doesn't work
    Unexpected(&'a OsStr),
}

fn split_nonutf8_once(b: &OsStr) -> (&str, Option<&OsStr>) {
    match b.try_str() {
        Ok(s) => (s, None),
        Err(err) => {
            // SAFETY: `char_indices` ensures `index` is at a valid UTF-8 boundary
            let (valid, after_valid) = unsafe { ext::split_at(b, err.valid_up_to()) };
            let valid = valid.try_str().unwrap();
            (valid, Some(after_valid))
        }
    }
}

fn ceil_char_boundary(s: &str, curr_boundary: usize) -> Option<usize> {
    (curr_boundary + 1..=s.len()).find(|i| s.is_char_boundary(*i))
}

mod private {
    use super::OsStr;

    #[allow(unnameable_types)]
    pub trait Sealed {}
    impl<const C: usize, S> Sealed for [S; C] where S: AsRef<OsStr> + std::fmt::Debug {}
    impl<S> Sealed for &'_ [S] where S: AsRef<OsStr> + std::fmt::Debug {}
    impl<S> Sealed for Vec<S> where S: AsRef<OsStr> + std::fmt::Debug {}
}

#[cfg(test)]
mod tests {
    use super::Arg::*;
    use super::*;

    #[test]
    fn test_basic() {
        let mut p = Parser::new(&["-n", "10", "foo", "-", "--", "baz", "-qux"]);
        assert_eq!(p.next_arg().unwrap(), Short("n"));
        assert_eq!(p.next_flag_value().unwrap(), "10");
        assert_eq!(p.next_arg().unwrap(), Value(OsStr::new("foo")));
        assert_eq!(p.next_arg().unwrap(), Value(OsStr::new("-")));
        assert_eq!(p.next_arg().unwrap(), Escape("--"));
        assert_eq!(p.next_arg().unwrap(), Value(OsStr::new("baz")));
        assert_eq!(p.next_arg().unwrap(), Value(OsStr::new("-qux")));
        assert_eq!(p.next_arg(), None);
        assert_eq!(p.next_arg(), None);
        assert_eq!(p.next_arg(), None);
    }

    #[test]
    fn test_combined() {
        let mut p = Parser::new(&["-abc", "-fvalue", "-xfvalue"]);
        assert_eq!(p.next_arg().unwrap(), Short("a"));
        assert_eq!(p.next_arg().unwrap(), Short("b"));
        assert_eq!(p.next_arg().unwrap(), Short("c"));
        assert_eq!(p.next_arg().unwrap(), Short("f"));
        assert_eq!(p.next_flag_value().unwrap(), "value");
        assert_eq!(p.next_arg().unwrap(), Short("x"));
        assert_eq!(p.next_arg().unwrap(), Short("f"));
        assert_eq!(p.next_flag_value().unwrap(), "value");
        assert_eq!(p.next_arg(), None);
    }

    #[test]
    fn test_long() {
        let mut p = Parser::new(&["--foo", "--bar=qux", "--foobar=qux=baz"]);
        assert_eq!(p.next_arg().unwrap(), Long("foo"));
        assert_eq!(p.next_arg().unwrap(), Long("bar"));
        assert_eq!(p.next_flag_value().unwrap(), "qux");
        assert_eq!(p.next_flag_value(), None);
        assert_eq!(p.next_arg().unwrap(), Long("foobar"));
        assert_eq!(p.next_arg().unwrap(), Unexpected(OsStr::new("qux=baz")));
        assert_eq!(p.next_arg(), None);
    }

    #[test]
    fn test_dash_args() {
        // "--" should indicate the end of the options
        let mut p = Parser::new(&["-x", "--", "-y"]);
        assert_eq!(p.next_arg().unwrap(), Short("x"));
        assert_eq!(p.next_arg().unwrap(), Escape("--"));
        assert_eq!(p.next_arg().unwrap(), Value(OsStr::new("-y")));
        assert_eq!(p.next_arg(), None);

        // ...even if it's an argument of an option
        let mut p = Parser::new(&["-x", "--", "-y"]);
        assert_eq!(p.next_arg().unwrap(), Short("x"));
        assert_eq!(p.next_flag_value(), None);
        assert_eq!(p.next_arg().unwrap(), Escape("--"));
        assert_eq!(p.next_arg().unwrap(), Value(OsStr::new("-y")));
        assert_eq!(p.next_arg(), None);

        // "-" is a valid value that should not be treated as an option
        let mut p = Parser::new(&["-x", "-", "-y"]);
        assert_eq!(p.next_arg().unwrap(), Short("x"));
        assert_eq!(p.next_arg().unwrap(), Value(OsStr::new("-")));
        assert_eq!(p.next_arg().unwrap(), Short("y"));
        assert_eq!(p.next_arg(), None);

        // '-' is a silly and hard to use short option, but other parsers treat
        // it like an option in this position
        let mut p = Parser::new(&["-x-y"]);
        assert_eq!(p.next_arg().unwrap(), Short("x"));
        assert_eq!(p.next_arg().unwrap(), Short("-"));
        assert_eq!(p.next_arg().unwrap(), Short("y"));
        assert_eq!(p.next_arg(), None);
    }

    #[test]
    fn test_missing_value() {
        let mut p = Parser::new(&["-o"]);
        assert_eq!(p.next_arg().unwrap(), Short("o"));
        assert_eq!(p.next_flag_value(), None);

        let mut q = Parser::new(&["--out"]);
        assert_eq!(q.next_arg().unwrap(), Long("out"));
        assert_eq!(q.next_flag_value(), None);

        let args: [&OsStr; 0] = [];
        let mut r = Parser::new(&args);
        assert_eq!(r.next_flag_value(), None);
    }

    #[test]
    fn test_weird_args() {
        let mut p = Parser::new(&[
            "--=", "--=3", "-", "-x", "--", "-", "-x", "--", "", "-", "-x",
        ]);
        assert_eq!(p.next_arg().unwrap(), Unexpected(OsStr::new("--=")));
        assert_eq!(p.next_arg().unwrap(), Unexpected(OsStr::new("--=3")));
        assert_eq!(p.next_arg().unwrap(), Value(OsStr::new("-")));
        assert_eq!(p.next_arg().unwrap(), Short("x"));
        assert_eq!(p.next_arg().unwrap(), Escape("--"));
        assert_eq!(p.next_arg().unwrap(), Value(OsStr::new("-")));
        assert_eq!(p.next_arg().unwrap(), Value(OsStr::new("-x")));
        assert_eq!(p.next_arg().unwrap(), Value(OsStr::new("--")));
        assert_eq!(p.next_arg().unwrap(), Value(OsStr::new("")));
        assert_eq!(p.next_arg().unwrap(), Value(OsStr::new("-")));
        assert_eq!(p.next_arg().unwrap(), Value(OsStr::new("-x")));
        assert_eq!(p.next_arg(), None);

        let bad = bad_string("--=@");
        let args = [&bad];
        let mut q = Parser::new(&args);
        assert_eq!(q.next_arg().unwrap(), Unexpected(OsStr::new(&bad)));

        let mut r = Parser::new(&[""]);
        assert_eq!(r.next_arg().unwrap(), Value(OsStr::new("")));
    }

    #[test]
    fn test_unicode() {
        let mut p = Parser::new(&["-aµ", "--µ=10", "µ", "--foo=µ"]);
        assert_eq!(p.next_arg().unwrap(), Short("a"));
        assert_eq!(p.next_arg().unwrap(), Short("µ"));
        assert_eq!(p.next_arg().unwrap(), Long("µ"));
        assert_eq!(p.next_flag_value().unwrap(), "10");
        assert_eq!(p.next_arg().unwrap(), Value(OsStr::new("µ")));
        assert_eq!(p.next_arg().unwrap(), Long("foo"));
        assert_eq!(p.next_flag_value().unwrap(), "µ");
    }

    #[cfg(any(unix, target_os = "wasi", windows))]
    #[test]
    fn test_mixed_invalid() {
        let args = [bad_string("--foo=@@@")];
        let mut p = Parser::new(&args);
        assert_eq!(p.next_arg().unwrap(), Long("foo"));
        assert_eq!(p.next_flag_value().unwrap(), bad_string("@@@"));

        let args = [bad_string("-💣@@@")];
        let mut q = Parser::new(&args);
        assert_eq!(q.next_arg().unwrap(), Short("💣"));
        assert_eq!(q.next_flag_value().unwrap(), bad_string("@@@"));

        let args = [bad_string("-f@@@")];
        let mut r = Parser::new(&args);
        assert_eq!(r.next_arg().unwrap(), Short("f"));
        assert_eq!(r.next_arg().unwrap(), Unexpected(&bad_string("@@@")));
        assert_eq!(r.next_arg(), None);

        let args = [bad_string("--foo=bar=@@@")];
        let mut s = Parser::new(&args);
        assert_eq!(s.next_arg().unwrap(), Long("foo"));
        assert_eq!(s.next_flag_value().unwrap(), bad_string("bar=@@@"));
    }

    #[cfg(any(unix, target_os = "wasi", windows))]
    #[test]
    fn test_separate_invalid() {
        let args = [bad_string("--foo"), bad_string("@@@")];
        let mut p = Parser::new(&args);
        assert_eq!(p.next_arg().unwrap(), Long("foo"));
        assert_eq!(p.next_flag_value().unwrap(), bad_string("@@@"));
    }

    #[cfg(any(unix, target_os = "wasi", windows))]
    #[test]
    fn test_invalid_long_option() {
        let args = [bad_string("--@=10")];
        let mut p = Parser::new(&args);
        assert_eq!(p.next_arg().unwrap(), Unexpected(&args[0]));
        assert_eq!(p.next_arg(), None);

        let args = [bad_string("--@")];
        let mut p = Parser::new(&args);
        assert_eq!(p.next_arg().unwrap(), Unexpected(&args[0]));
        assert_eq!(p.next_arg(), None);
    }

    #[cfg(any(unix, target_os = "wasi", windows))]
    #[test]
    fn test_invalid_short_option() {
        let args = [bad_string("-@")];
        let mut p = Parser::new(&args);
        assert_eq!(p.next_arg().unwrap(), Unexpected(&args[0]));
        assert_eq!(p.next_arg(), None);
    }

    #[test]
    fn short_opt_equals_sign() {
        let mut p = Parser::new(&["-a=b"]);
        assert_eq!(p.next_arg().unwrap(), Short("a"));
        assert_eq!(p.next_flag_value().unwrap(), OsStr::new("b"));
        assert_eq!(p.next_arg(), None);

        let mut p = Parser::new(&["-a=b", "c"]);
        assert_eq!(p.next_arg().unwrap(), Short("a"));
        assert_eq!(p.next_flag_value().unwrap(), OsStr::new("b"));
        assert_eq!(p.next_flag_value(), None);
        assert_eq!(p.next_arg().unwrap(), Value(OsStr::new("c")));
        assert_eq!(p.next_arg(), None);

        let mut p = Parser::new(&["-a=b"]);
        assert_eq!(p.next_arg().unwrap(), Short("a"));
        assert_eq!(p.next_arg().unwrap(), Short("="));
        assert_eq!(p.next_arg().unwrap(), Short("b"));
        assert_eq!(p.next_arg(), None);

        let mut p = Parser::new(&["-a="]);
        assert_eq!(p.next_arg().unwrap(), Short("a"));
        assert_eq!(p.next_flag_value().unwrap(), OsStr::new(""));
        assert_eq!(p.next_arg(), None);

        let mut p = Parser::new(&["-a=="]);
        assert_eq!(p.next_arg().unwrap(), Short("a"));
        assert_eq!(p.next_flag_value().unwrap(), OsStr::new("="));
        assert_eq!(p.next_arg(), None);

        let mut p = Parser::new(&["-abc=de"]);
        assert_eq!(p.next_arg().unwrap(), Short("a"));
        assert_eq!(p.next_flag_value().unwrap(), OsStr::new("bc=de"));
        assert_eq!(p.next_arg(), None);

        let mut p = Parser::new(&["-abc==de"]);
        assert_eq!(p.next_arg().unwrap(), Short("a"));
        assert_eq!(p.next_arg().unwrap(), Short("b"));
        assert_eq!(p.next_arg().unwrap(), Short("c"));
        assert_eq!(p.next_flag_value().unwrap(), OsStr::new("=de"));
        assert_eq!(p.next_arg(), None);

        let mut p = Parser::new(&["-a="]);
        assert_eq!(p.next_arg().unwrap(), Short("a"));
        assert_eq!(p.next_arg().unwrap(), Short("="));
        assert_eq!(p.next_arg(), None);

        let mut p = Parser::new(&["-="]);
        assert_eq!(p.next_arg().unwrap(), Short("="));
        assert_eq!(p.next_arg(), None);

        let mut p = Parser::new(&["-=a"]);
        assert_eq!(p.next_arg().unwrap(), Short("="));
        assert_eq!(p.next_arg().unwrap(), Short("a"));
        assert_eq!(p.next_arg(), None);
    }

    #[cfg(any(unix, target_os = "wasi", windows))]
    #[test]
    fn short_opt_equals_sign_invalid() {
        let bad = bad_string("@");
        let args = [bad_string("-a=@")];
        let mut p = Parser::new(&args);
        assert_eq!(p.next_arg().unwrap(), Short("a"));
        assert_eq!(p.next_flag_value().unwrap(), bad_string("@"));
        assert_eq!(p.next_arg(), None);

        let mut p = Parser::new(&args);
        assert_eq!(p.next_arg().unwrap(), Short("a"));
        assert_eq!(p.next_arg().unwrap(), Short("="));
        assert_eq!(p.next_arg().unwrap(), Unexpected(&bad));
        assert_eq!(p.next_arg(), None);
    }

    #[test]
    fn remaining_raw() {
        let mut p = Parser::new(&["-a", "b", "c", "d"]);
        assert_eq!(
            p.remaining_raw().unwrap().collect::<Vec<_>>(),
            &["-a", "b", "c", "d"]
        );
        // Consumed all
        assert!(p.next_arg().is_none());
        assert!(p.remaining_raw().is_ok());
        assert_eq!(p.remaining_raw().unwrap().collect::<Vec<_>>().len(), 0);

        let mut p = Parser::new(&["-ab", "c", "d"]);
        p.next_arg().unwrap();
        // Attached value
        assert!(p.remaining_raw().is_err());
        p.next_attached_value().unwrap();
        assert_eq!(p.remaining_raw().unwrap().collect::<Vec<_>>(), &["c", "d"]);
        // Consumed all
        assert!(p.next_arg().is_none());
        assert_eq!(p.remaining_raw().unwrap().collect::<Vec<_>>().len(), 0);
    }

    /// Transform @ characters into invalid unicode.
    fn bad_string(text: &str) -> std::ffi::OsString {
        #[cfg(any(unix, target_os = "wasi"))]
        {
            #[cfg(unix)]
            use std::os::unix::ffi::OsStringExt;
            #[cfg(target_os = "wasi")]
            use std::os::wasi::ffi::OsStringExt;
            let mut text = text.as_bytes().to_vec();
            for ch in &mut text {
                if *ch == b'@' {
                    *ch = b'\xFF';
                }
            }
            std::ffi::OsString::from_vec(text)
        }
        #[cfg(windows)]
        {
            use std::os::windows::ffi::OsStringExt;
            let mut out = Vec::new();
            for ch in text.chars() {
                if ch == '@' {
                    out.push(0xD800);
                } else {
                    let mut buf = [0; 2];
                    out.extend(&*ch.encode_utf16(&mut buf));
                }
            }
            std::ffi::OsString::from_wide(&out)
        }
        #[cfg(not(any(unix, target_os = "wasi", windows)))]
        {
            if text.contains('@') {
                unimplemented!("Don't know how to create invalid OsStrings on this platform");
            }
            text.into()
        }
    }

    /// Basic exhaustive testing of short combinations of "interesting"
    /// arguments. They should not panic, not hang, and pass some checks.
    ///
    /// The advantage compared to full fuzzing is that it runs on all platforms
    /// and together with the other tests. cargo-fuzz doesn't work on Windows
    /// and requires a special incantation.
    ///
    /// A disadvantage is that it's still limited by arguments I could think of
    /// and only does very short sequences. Another is that it's bad at
    /// reporting failure, though the println!() helps.
    ///
    /// This test takes a while to run.
    #[test]
    fn basic_fuzz() {
        #[cfg(any(windows, unix, target_os = "wasi"))]
        const VOCABULARY: &[&str] = &[
            "", "-", "--", "---", "a", "-a", "-aa", "@", "-@", "-a@", "-@a", "--a", "--@", "--a=a",
            "--a=", "--a=@", "--@=a", "--=", "--=@", "--=a", "-@@", "-a=a", "-a=", "-=", "-a-",
        ];
        #[cfg(not(any(windows, unix, target_os = "wasi")))]
        const VOCABULARY: &[&str] = &[
            "", "-", "--", "---", "a", "-a", "-aa", "--a", "--a=a", "--a=", "--=", "--=a", "-a=a",
            "-a=", "-=", "-a-",
        ];
        let args: [&OsStr; 0] = [];
        exhaust(Parser::new(&args), vec![]);
        let vocabulary: Vec<std::ffi::OsString> =
            VOCABULARY.iter().map(|&s| bad_string(s)).collect();
        let mut permutations = vec![vec![]];
        for _ in 0..3 {
            let mut new = Vec::new();
            for old in permutations {
                for word in &vocabulary {
                    let mut extended = old.clone();
                    extended.push(word);
                    new.push(extended);
                }
            }
            permutations = new;
            for permutation in &permutations {
                println!("Starting {permutation:?}");
                let p = Parser::new(permutation);
                exhaust(p, vec![]);
            }
        }
    }

    /// Run many sequences of methods on a Parser.
    fn exhaust(parser: Parser<'_>, path: Vec<String>) {
        if path.len() > 100 {
            panic!("Stuck in loop: {path:?}");
        }

        if parser.has_pending() {
            {
                let mut parser = parser.clone();
                let next = parser.next_arg();
                assert!(
                    matches!(next, Some(Unexpected(_)) | Some(Short(_))),
                    "{next:?} via {path:?}",
                );
                let mut path = path.clone();
                path.push(format!("pending-next-{next:?}"));
                exhaust(parser, path);
            }

            {
                let mut parser = parser.clone();
                let next = parser.next_flag_value();
                assert!(next.is_some(), "{next:?} via {path:?}",);
                let mut path = path;
                path.push(format!("pending-value-{next:?}"));
                exhaust(parser, path);
            }
        } else {
            {
                let mut parser = parser.clone();
                let next = parser.next_arg();
                match &next {
                    None => {
                        assert!(
                            matches!(parser.state, None | Some(State::Escaped)),
                            "{next:?} via {path:?}",
                        );
                        assert_eq!(parser.current, parser.raw.len(), "{next:?} via {path:?}",);
                    }
                    _ => {
                        let mut path = path.clone();
                        path.push(format!("next-{next:?}"));
                        exhaust(parser, path);
                    }
                }
            }

            {
                let mut parser = parser.clone();
                let next = parser.next_flag_value();
                match &next {
                    None => {
                        assert!(
                            matches!(parser.state, None | Some(State::Escaped)),
                            "{next:?} via {path:?}",
                        );
                        if parser.state.is_none()
                            && !parser.was_attached
                            && parser.peek_raw_() != Some(OsStr::new("--"))
                        {
                            assert_eq!(parser.current, parser.raw.len(), "{next:?} via {path:?}",);
                        }
                    }
                    Some(_) => {
                        assert!(
                            matches!(parser.state, None | Some(State::Escaped)),
                            "{next:?} via {path:?}",
                        );
                        let mut path = path;
                        path.push(format!("value-{next:?}"));
                        exhaust(parser, path);
                    }
                }
            }
        }
    }
}

#[doc = include_str!("../README.md")]
#[cfg(doctest)]
pub struct ReadmeDoctests;
