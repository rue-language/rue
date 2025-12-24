//! Definition of the json output for libtest

#![cfg_attr(docsrs, feature(doc_auto_cfg))]
#![warn(clippy::print_stderr)]
#![warn(clippy::print_stdout)]

pub mod event;

pub use event::Elapsed;
pub use event::Event;
pub use event::MessageKind;
pub use event::RunMode;

#[doc = include_str!("../README.md")]
#[cfg(doctest)]
pub struct ReadmeDoctests;
