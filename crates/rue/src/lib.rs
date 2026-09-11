//! Canonical filesystem-backed host for the Rue compiler.
//!
//! The command-line compiler, retained-session benchmarks, and long-lived
//! product hosts share this owner. Filesystem observation remains outside the
//! compiler query graph while one retained `CompilerSession` owns every
//! compiler artifact across revisions.

pub mod daemon;
mod host;
#[cfg(test)]
mod host_workflow_tests;
mod running_image;
mod source_loader;
mod test_candidates;

pub use host::{FilesystemCompilerHost, HostOpenRequest, HostPathContext};
pub use running_image::{
    RUNNING_IMAGE_IDENTITY_SCHEME_VERSION, RunningImageArchitecture, RunningImageIdentity,
    RunningImageIdentityError, RunningImageIdentityScheme, running_image_identity,
};
pub use source_loader::{
    AttemptedRead, HermeticDenialError, SourceLoadError, ToolchainIntegrityError, WatchFingerprint,
    WatchInput, WatchInputParts, watch_input_fingerprints, watch_inputs_changed,
    watch_inputs_changed_with_reader, with_import_migration_helps,
    with_import_migration_helps_batches,
};
pub use test_candidates::{
    load_declared_candidates, load_declared_candidates_at, load_declared_candidates_with_context,
};
