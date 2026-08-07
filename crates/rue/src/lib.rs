//! Canonical filesystem-backed host for the Rue compiler.
//!
//! The command-line compiler, retained-session benchmarks, and long-lived
//! product hosts share this owner. Filesystem observation remains outside the
//! compiler query graph while one retained `CompilerSession` owns every
//! compiler artifact across revisions.

mod host;
mod source_loader;

pub use host::{FilesystemCompilerHost, HostOpenRequest};
pub use source_loader::{HermeticDenialError, SourceLoadError, ToolchainIntegrityError};
