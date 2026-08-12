//! Body-query accounting retained by the canonical revisioned semantic runtime.

use crate::StableDefinitionKey;

/// Stable identity-domain aliases used by the revisioned CFG projection.
pub type DurableProjection = rue_air::SemanticBodyProjection<StableDefinitionKey, crate::ModuleId>;
pub type DurableAirInstData = rue_air::SemanticBodyInstData<StableDefinitionKey, crate::ModuleId>;

/// Work performed while importing exact body facts into a revision-owned
/// semantic transaction. The payload itself stays in the revisioned runtime;
/// this is deliberately only observational accounting.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DurableBodyWork {
    pub candidate_fallbacks: usize,
    pub export_attempts: usize,
    pub export_successes: usize,
    pub export_rejections: usize,
    pub instructions_exported: usize,
    pub places_exported: usize,
    pub strings_exported: usize,
    pub import_attempts: usize,
    pub import_successes: usize,
    pub import_failures: usize,
    pub installed_instructions: usize,
    pub installed_places: usize,
    pub installed_strings: usize,
    pub atomic_discards: usize,
    pub reused_bodies: usize,
    pub skipped_body_analyses: usize,
}

impl DurableBodyWork {
    #[cfg(test)]
    #[allow(dead_code)]
    pub(crate) fn record_import_failure(
        &mut self,
        _reason: rue_air::SemanticBodyImportFailureKind,
        count: usize,
    ) {
        self.import_failures += count;
    }
}
