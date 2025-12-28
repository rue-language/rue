//! A low-level interface for writing out JSON
//!
//! # Example
//!
//! ```rust
//! use json_write::JsonWrite as _;
//!
//! # fn main() -> std::fmt::Result {
//! let mut output = String::new();
//! output.open_object()?;
//! output.newline()?;
//!
//! output.space()?;
//! output.space()?;
//! output.key("key")?;
//! output.keyval_sep()?;
//! output.space()?;
//! output.value("value")?;
//! output.newline()?;
//!
//! output.close_object()?;
//! output.newline()?;
//!
//! assert_eq!(output, r#"{
//!   "key": "value"
//! }
//! "#);
//! #   Ok(())
//! # }
//! ```

#![cfg_attr(all(not(feature = "std"), not(test)), no_std)]
#![cfg_attr(docsrs, feature(doc_auto_cfg))]
#![warn(clippy::std_instead_of_core)]
#![warn(clippy::std_instead_of_alloc)]
#![warn(clippy::print_stderr)]
#![warn(clippy::print_stdout)]

#[cfg(feature = "alloc")]
extern crate alloc;

mod key;
mod value;
mod write;

#[cfg(feature = "alloc")]
pub use key::ToJsonKey;
pub use key::WriteJsonKey;
#[cfg(feature = "alloc")]
pub use value::ToJsonValue;
pub use value::WriteJsonValue;
pub use write::JsonWrite;

#[doc = include_str!("../README.md")]
#[cfg(doctest)]
pub struct ReadmeDoctests;
