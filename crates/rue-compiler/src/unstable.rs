//! Explicitly unstable compiler instrumentation and test-support views.
//!
//! Nothing in this module is covered by the supported facade's compatibility
//! policy. These owned snapshots and opaque session products cannot be
//! installed into a session or used as query keys.

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::sync::Arc;

use crate::canonical_semantic::CanonicalSemanticFailurePhase as SemanticFailurePhase;

/// Opaque equality status for session-owned durable artifacts.
///
/// The fingerprint is instrumentation, not a cache key or durable schema. It
/// deliberately has no import/install operation.
#[derive(Clone, PartialEq, Eq)]
pub struct DurableArtifactStatus {
    count: usize,
    fingerprint: [u8; 32],
}

impl std::fmt::Debug for DurableArtifactStatus {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DurableArtifactStatus")
            .field("count", &self.count)
            .finish_non_exhaustive()
    }
}

/// Owned dependency-manifest counters. No manifest records or cache payloads
/// are retained by this snapshot.
#[derive(Debug, Clone, PartialEq)]
pub struct DependencyManifestMetrics {
    pub body_owner_events_translated: usize,
    pub body_named_events_translated: usize,
    pub body_dependency_records_built: usize,
    pub extra_rir_instructions_visited: usize,
    pub durable_bodies: DurableBodyMetrics,
}

/// Durable-body reuse counters without the durable cache records themselves.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct DurableBodyMetrics {
    pub candidate_comparisons: usize,
    pub candidate_fallbacks: usize,
    pub specialized_mapping_attempts: usize,
    pub specialized_mapping_successes: usize,
    pub specialized_mapping_failures: usize,
    pub export_attempts: usize,
    pub export_successes: usize,
    pub export_rejections: usize,
    pub instructions_exported: usize,
    pub places_exported: usize,
    pub strings_exported: usize,
    pub conversion_attempts: usize,
    pub conversion_completions: usize,
    pub conversion_failures: usize,
    pub stable_key_joins: usize,
    pub finalization_attempts: usize,
    pub finalization_completions: usize,
    pub finalization_failures: usize,
    pub projection_attempts: usize,
    pub projection_completions: usize,
    pub projection_failures: usize,
    pub instructions_projected: usize,
    pub places_projected: usize,
    pub strings_projected: usize,
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

impl From<crate::DurableBodyWork> for DurableBodyMetrics {
    fn from(work: crate::DurableBodyWork) -> Self {
        Self {
            candidate_comparisons: work.candidate_comparisons,
            candidate_fallbacks: work.candidate_fallbacks,
            specialized_mapping_attempts: work.specialized_mapping_attempts,
            specialized_mapping_successes: work.specialized_mapping_successes,
            specialized_mapping_failures: work.specialized_mapping_failures,
            export_attempts: work.export_attempts,
            export_successes: work.export_successes,
            export_rejections: work.export_rejections,
            instructions_exported: work.instructions_exported,
            places_exported: work.places_exported,
            strings_exported: work.strings_exported,
            conversion_attempts: work.conversion_attempts,
            conversion_completions: work.conversion_completions,
            conversion_failures: work.conversion_failures,
            stable_key_joins: work.stable_key_joins,
            finalization_attempts: work.finalization_attempts,
            finalization_completions: work.finalization_completions,
            finalization_failures: work.finalization_failures,
            projection_attempts: work.projection_attempts,
            projection_completions: work.projection_completions,
            projection_failures: work.projection_failures,
            instructions_projected: work.instructions_projected,
            places_projected: work.places_projected,
            strings_projected: work.strings_projected,
            import_attempts: work.import_attempts,
            import_successes: work.import_successes,
            import_failures: work.import_failures,
            installed_instructions: work.installed_instructions,
            installed_places: work.installed_places,
            installed_strings: work.installed_strings,
            atomic_discards: work.atomic_discards,
            reused_bodies: work.reused_bodies,
            skipped_body_analyses: work.skipped_body_analyses,
        }
    }
}

impl DependencyManifestMetrics {
    pub(crate) fn from_work(work: crate::session::SemanticDependencyManifestWork) -> Self {
        let durable = work.durable_bodies;
        Self {
            body_owner_events_translated: work.body_owner_events_translated,
            body_named_events_translated: work.body_named_events_translated,
            body_dependency_records_built: work.body_dependency_records_built,
            extra_rir_instructions_visited: work.extra_rir_instructions_visited,
            durable_bodies: durable.into(),
        }
    }
}

impl DurableArtifactStatus {
    pub fn count(&self) -> usize {
        self.count
    }

    pub(crate) fn from_debug<T: std::fmt::Debug>(artifacts: &[T]) -> Self {
        let mut hasher = Sha256::new();
        for artifact in artifacts {
            hasher.update(format!("{artifact:?}").as_bytes());
            hasher.update([0]);
        }
        Self {
            count: artifacts.len(),
            fingerprint: hasher.finalize().into(),
        }
    }
}

/// Query lifecycle counters contained in [`MetricsSnapshot`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct QueryMetrics {
    pub calls: usize,
    pub executions: usize,
    pub reuses: usize,
}

impl From<crate::session::FrontendQueryWork> for QueryMetrics {
    fn from(work: crate::session::FrontendQueryWork) -> Self {
        Self {
            calls: work.calls,
            executions: work.executions,
            reuses: work.reuses,
        }
    }
}

/// Retention gauges contained in [`MetricsSnapshot`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RetentionMetrics {
    pub diagnostic_entries: usize,
    pub diagnostic_source_attempts: usize,
    pub diagnostic_source_bytes: usize,
    pub dependency_manifests: usize,
    pub invalidation_plans: usize,
}

/// Merge counters used by the in-tree benchmark.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MergeMetrics {
    pub definition_shards_indexed: usize,
    pub definition_shards_reused: usize,
    pub definition_shards_rebuilt: usize,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ParseMetrics {
    pub lexer_invocations: usize,
    pub parser_invocations: usize,
    pub lexed_bytes: usize,
    pub tokens: usize,
    pub modules_considered: usize,
    pub modules_reused: usize,
    pub modules_rebound: usize,
    pub modules_reparsed: usize,
    pub source_text_clones: usize,
    pub source_bytes_rehashed: usize,
}

impl ParseMetrics {
    pub(crate) fn from_work(work: crate::ParsedModulesWork) -> Self {
        Self {
            lexer_invocations: work.syntax.lexer_invocations,
            parser_invocations: work.syntax.parser_invocations,
            lexed_bytes: work.syntax.lexed_bytes,
            tokens: work.syntax.tokens,
            modules_considered: work.modules_considered,
            modules_reused: work.modules_reused,
            modules_rebound: work.modules_rebound,
            modules_reparsed: work.modules_reparsed,
            source_text_clones: work.source_text_clones,
            source_bytes_rehashed: work.source_bytes_rehashed,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LowerMetrics {
    pub parser_invocations: usize,
    pub ast_payload_clones: usize,
}

impl LowerMetrics {
    pub(crate) fn from_work(work: crate::CanonicalRirWork) -> Self {
        Self {
            parser_invocations: work.parser_invocations,
            ast_payload_clones: work.ast_payload_clones,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SemanticBindingMetrics {
    pub bind_invocations: usize,
}
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SemanticManifestMetrics {
    pub build_invocations: usize,
}
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SemanticCfgMetrics {
    pub cfg_builds_attempted: usize,
    pub cfg_builds_succeeded: usize,
    pub cfg_builds_failed: usize,
}
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SemanticMetrics {
    pub binding: SemanticBindingMetrics,
    pub manifest: SemanticManifestMetrics,
    pub cfg: SemanticCfgMetrics,
}

impl SemanticMetrics {
    pub(crate) fn from_work(work: crate::CanonicalSemanticWork) -> Self {
        Self {
            binding: SemanticBindingMetrics {
                bind_invocations: work.binding.bind_invocations,
            },
            manifest: SemanticManifestMetrics {
                build_invocations: work.manifest.build_invocations,
            },
            cfg: SemanticCfgMetrics {
                cfg_builds_attempted: work.cfg.cfg_builds_attempted,
                cfg_builds_succeeded: work.cfg.cfg_builds_succeeded,
                cfg_builds_failed: work.cfg.cfg_builds_failed,
            },
        }
    }
}

/// Metrics captured by the one-shot compilation adapter.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct OneShotMetrics {
    pub files: usize,
    pub bytes: usize,
    pub lines: usize,
    pub tokens: usize,
    pub parsed: ParseMetrics,
    pub lowered: LowerMetrics,
    pub semantic: SemanticMetrics,
}

impl OneShotMetrics {
    pub(crate) fn new(stats: crate::SourceStats, work: crate::PipelineWork) -> Self {
        Self {
            files: stats.files,
            bytes: stats.bytes,
            lines: stats.lines,
            tokens: stats.tokens,
            parsed: ParseMetrics::from_work(work.parsed),
            lowered: LowerMetrics::from_work(work.lowered),
            semantic: SemanticMetrics::from_work(work.semantic),
        }
    }
}

/// Owned compiler metrics snapshot with query records and inputs projected out.
#[derive(Debug, Clone)]
pub struct MetricsSnapshot {
    inner: crate::session::CompilerSessionWork,
}

impl MetricsSnapshot {
    pub(crate) fn new(inner: crate::session::CompilerSessionWork) -> Self {
        Self { inner }
    }
    pub fn updates(&self) -> usize {
        self.inner.updates
    }
    pub fn merge(&self) -> QueryMetrics {
        self.inner.merge.into()
    }
    pub fn rir(&self) -> QueryMetrics {
        self.inner.rir.into()
    }
    pub fn semantic(&self) -> QueryMetrics {
        self.inner.semantic.into()
    }
    pub fn definitions(&self) -> QueryMetrics {
        self.inner.definitions.into()
    }
    pub fn dependency_manifests(&self) -> QueryMetrics {
        self.inner.dependency_manifests.into()
    }
    pub fn invalidation_plans(&self) -> QueryMetrics {
        self.inner.invalidation_plans.into()
    }
    pub fn downstream_invalidations(&self) -> usize {
        self.inner.downstream_invalidations
    }
    pub fn semantic_entries_invalidated(&self) -> usize {
        self.inner.semantic_entries_invalidated
    }
    pub fn definition_entries_invalidated(&self) -> usize {
        self.inner.definition_entries_invalidated
    }
    pub fn declaration_reuse_plans(&self) -> usize {
        self.inner.declaration_reuse_plans
    }
    pub fn durable_records_compared(&self) -> usize {
        self.inner.durable_records_compared
    }
    pub fn durable_records_reused(&self) -> usize {
        self.inner.durable_records_reused
    }
    pub fn ordinary_declaration_resolutions_skipped(&self) -> usize {
        self.inner.ordinary_declaration_resolutions_skipped
    }
    pub fn durable_installs(&self) -> usize {
        self.inner.durable_installs
    }
    pub fn declaration_reuse_fallbacks(&self) -> usize {
        self.inner.declaration_reuse_fallbacks
    }
    pub fn semantic_record_count(&self) -> usize {
        self.inner.semantic_records.len()
    }
    pub fn definition_record_count(&self) -> usize {
        self.inner.definition_records.len()
    }
    pub fn merge_metrics(&self) -> MergeMetrics {
        MergeMetrics {
            definition_shards_indexed: self.inner.last_merge.definition_shards_indexed,
            definition_shards_reused: self.inner.last_merge.definition_shards_reused,
            definition_shards_rebuilt: self.inner.last_merge.definition_shards_rebuilt,
        }
    }
    pub fn parse_metrics(&self) -> ParseMetrics {
        ParseMetrics::from_work(self.inner.last_parse)
    }
    pub fn lower_metrics(&self) -> LowerMetrics {
        LowerMetrics {
            parser_invocations: self.inner.last_rir.parser_invocations,
            ast_payload_clones: self.inner.last_rir.ast_payload_clones,
        }
    }
    pub fn retention(&self) -> RetentionMetrics {
        RetentionMetrics {
            diagnostic_entries: self.inner.retention.diagnostic_entries,
            diagnostic_source_attempts: self.inner.retention.diagnostic_source_attempts,
            diagnostic_source_bytes: self.inner.retention.diagnostic_source_bytes,
            dependency_manifests: self.inner.retention.dependency_manifests,
            invalidation_plans: self.inner.retention.invalidation_plans,
        }
    }
    pub fn semantic_work_json(&self, from: usize) -> Value {
        semantic_work_json(&self.inner, from)
    }
    pub fn definition_work_json(&self, from: usize) -> Value {
        definition_work_json(&self.inner, from)
    }
}

fn semantic_work_json(work: &crate::session::CompilerSessionWork, from: usize) -> Value {
    let records = &work.semantic_records[from..];
    json!({
        "failed_requests": records.iter().filter(|record| record.failure.is_some()).count(),
        "failure_phases": {
            "declaration": records.iter().filter(|record| matches!(record.failure.map(|failure| failure.phase), Some(SemanticFailurePhase::Declaration))).count(),
            "body_analysis": records.iter().filter(|record| matches!(record.failure.map(|failure| failure.phase), Some(SemanticFailurePhase::BodyAnalysis))).count(),
            "cfg_construction": records.iter().filter(|record| matches!(record.failure.map(|failure| failure.phase), Some(SemanticFailurePhase::CfgConstruction))).count(),
        },
        "bind_invocations": records.iter().map(|record| record.work.binding.bind_invocations).sum::<usize>(),
        "declaration_resolution_invocations": records.iter().map(|record| record.work.binding.declaration_resolution_invocations).sum::<usize>(),
        "declaration_resolution_failures": records.iter().map(|record| record.work.binding.declaration_resolution_failures).sum::<usize>(),
        "body_readiness_finalization_invocations": records.iter().map(|record| record.work.binding.body_readiness_finalization_invocations).sum::<usize>(),
        "body_free_function_lookups": records.iter().map(|record| record.work.body_analysis.free_function_record_lookups).sum::<usize>(),
        "bodies_attempted": records.iter().map(|record| record.work.body_analysis.bodies_attempted).sum::<usize>(),
        "bodies_succeeded": records.iter().map(|record| record.work.body_analysis.bodies_succeeded).sum::<usize>(),
        "bodies_failed": records.iter().map(|record| record.work.body_analysis.bodies_failed).sum::<usize>(),
        "air_instructions_produced": records.iter().map(|record| record.work.body_analysis.air_instructions_produced).sum::<usize>(),
        "body_dependency_air_instructions_observed": records.iter().map(|record| record.work.body_analysis.body_dependency_air_instructions_observed).sum::<usize>(),
        "local_strings_produced": records.iter().map(|record| record.work.body_analysis.local_strings_produced).sum::<usize>(),
        "string_ids_remapped": records.iter().map(|record| record.work.body_analysis.string_ids_remapped).sum::<usize>(),
        "specialization_air_instructions_scanned": records.iter().map(|record| record.work.body_analysis.specialization_air_instructions_scanned).sum::<usize>(),
        "generic_calls_observed": records.iter().map(|record| record.work.body_analysis.generic_calls_observed).sum::<usize>(),
        "specialization_requests_unique": records.iter().map(|record| record.work.body_analysis.specialization_requests_unique).sum::<usize>(),
        "specialization_requests_duplicate": records.iter().map(|record| record.work.body_analysis.specialization_requests_duplicate).sum::<usize>(),
        "specialization_rewrites": records.iter().map(|record| record.work.body_analysis.specialization_rewrites).sum::<usize>(),
        "specialization_rounds": records.iter().map(|record| record.work.body_analysis.specialization_rounds).sum::<usize>(),
        "specialization_driver_failures": records.iter().map(|record| record.work.body_analysis.specialization_driver_failures).sum::<usize>(),
        "specialized_bodies_attempted": records.iter().map(|record| record.work.body_analysis.specialized_bodies_attempted).sum::<usize>(),
        "specialized_bodies_succeeded": records.iter().map(|record| record.work.body_analysis.specialized_bodies_succeeded).sum::<usize>(),
        "specialized_bodies_failed": records.iter().map(|record| record.work.body_analysis.specialized_bodies_failed).sum::<usize>(),
        "durable_bodies": {
            "candidate_comparisons": records.iter().map(|record| record.work.durable_bodies.candidate_comparisons).sum::<usize>(),
            "candidate_fallbacks": records.iter().map(|record| record.work.durable_bodies.candidate_fallbacks).sum::<usize>(),
            "specialized_mapping_attempts": records.iter().map(|record| record.work.durable_bodies.specialized_mapping_attempts).sum::<usize>(),
            "specialized_mapping_successes": records.iter().map(|record| record.work.durable_bodies.specialized_mapping_successes).sum::<usize>(),
            "specialized_mapping_failures": records.iter().map(|record| record.work.durable_bodies.specialized_mapping_failures).sum::<usize>(),
            "export_attempts": records.iter().map(|record| record.work.durable_bodies.export_attempts).sum::<usize>(),
            "export_successes": records.iter().map(|record| record.work.durable_bodies.export_successes).sum::<usize>(),
            "export_rejections": records.iter().map(|record| record.work.durable_bodies.export_rejections).sum::<usize>(),
            "instructions_exported": records.iter().map(|record| record.work.durable_bodies.instructions_exported).sum::<usize>(),
            "places_exported": records.iter().map(|record| record.work.durable_bodies.places_exported).sum::<usize>(),
            "strings_exported": records.iter().map(|record| record.work.durable_bodies.strings_exported).sum::<usize>(),
            "conversion_attempts": records.iter().map(|record| record.work.durable_bodies.conversion_attempts).sum::<usize>(),
            "conversion_completions": records.iter().map(|record| record.work.durable_bodies.conversion_completions).sum::<usize>(),
            "conversion_failures": records.iter().map(|record| record.work.durable_bodies.conversion_failures).sum::<usize>(),
            "finalization_attempts": records.iter().map(|record| record.work.durable_bodies.finalization_attempts).sum::<usize>(),
            "finalization_completions": records.iter().map(|record| record.work.durable_bodies.finalization_completions).sum::<usize>(),
            "finalization_failures": records.iter().map(|record| record.work.durable_bodies.finalization_failures).sum::<usize>(),
            "stable_key_joins": records.iter().map(|record| record.work.durable_bodies.stable_key_joins).sum::<usize>(),
            "projection_attempts": records.iter().map(|record| record.work.durable_bodies.projection_attempts).sum::<usize>(),
            "projection_completions": records.iter().map(|record| record.work.durable_bodies.projection_completions).sum::<usize>(),
            "projection_failures": records.iter().map(|record| record.work.durable_bodies.projection_failures).sum::<usize>(),
            "instructions_projected": records.iter().map(|record| record.work.durable_bodies.instructions_projected).sum::<usize>(),
            "places_projected": records.iter().map(|record| record.work.durable_bodies.places_projected).sum::<usize>(),
            "strings_projected": records.iter().map(|record| record.work.durable_bodies.strings_projected).sum::<usize>(),
            "import_attempts": records.iter().map(|record| record.work.durable_bodies.import_attempts).sum::<usize>(),
            "import_successes": records.iter().map(|record| record.work.durable_bodies.import_successes).sum::<usize>(),
            "import_failures": records.iter().map(|record| record.work.durable_bodies.import_failures).sum::<usize>(),
            "installed_instructions": records.iter().map(|record| record.work.durable_bodies.installed_instructions).sum::<usize>(),
            "installed_places": records.iter().map(|record| record.work.durable_bodies.installed_places).sum::<usize>(),
            "installed_strings": records.iter().map(|record| record.work.durable_bodies.installed_strings).sum::<usize>(),
            "atomic_discards": records.iter().map(|record| record.work.durable_bodies.atomic_discards).sum::<usize>(),
            "reused_bodies": records.iter().map(|record| record.work.durable_bodies.reused_bodies).sum::<usize>(),
            "skipped_body_analyses": records.iter().map(|record| record.work.durable_bodies.skipped_body_analyses).sum::<usize>(),
        },
        "cfg": {
            "drop_glue_functions_synthesized": records.iter().map(|record| record.work.cfg.drop_glue_functions_synthesized).sum::<usize>(),
            "functions_considered": records.iter().map(|record| record.work.cfg.functions_considered).sum::<usize>(),
            "comptime_functions_filtered": records.iter().map(|record| record.work.cfg.comptime_functions_filtered).sum::<usize>(),
            "builds_attempted": records.iter().map(|record| record.work.cfg.cfg_builds_attempted).sum::<usize>(),
            "builds_succeeded": records.iter().map(|record| record.work.cfg.cfg_builds_succeeded).sum::<usize>(),
            "builds_failed": records.iter().map(|record| record.work.cfg.cfg_builds_failed).sum::<usize>(),
            "air_instructions_consumed": records.iter().map(|record| record.work.cfg.air_instructions_consumed).sum::<usize>(),
            "optimization_attempts": records.iter().map(|record| record.work.cfg.optimization_attempts).sum::<usize>(),
            "optimization_completions": records.iter().map(|record| record.work.cfg.optimization_completions).sum::<usize>(),
            "optimized_level_attempts": records.iter().map(|record| record.work.cfg.optimized_level_attempts).sum::<usize>(),
            "warnings_emitted": records.iter().map(|record| record.work.cfg.cfg_warnings_emitted).sum::<usize>(),
            "implicit_destructor_targets_emitted": records.iter().map(|record| record.work.cfg.implicit_destructor_targets_emitted).sum::<usize>(),
            "reuse_candidates": records.iter().map(|record| record.work.cfg.cfg_reuse_candidates).sum::<usize>(),
            "import_attempts": records.iter().map(|record| record.work.cfg.cfg_import_attempts).sum::<usize>(),
            "import_successes": records.iter().map(|record| record.work.cfg.cfg_import_successes).sum::<usize>(),
            "import_failures": records.iter().map(|record| record.work.cfg.cfg_import_failures).sum::<usize>(),
            "reuses": records.iter().map(|record| record.work.cfg.cfg_reuses).sum::<usize>(),
            "fallbacks": records.iter().map(|record| record.work.cfg.cfg_fallbacks).sum::<usize>(),
            "warnings_reused": records.iter().map(|record| record.work.cfg.cfg_warnings_reused).sum::<usize>(),
            "implicit_destructor_targets_reused": records.iter().map(|record| record.work.cfg.implicit_destructor_targets_reused).sum::<usize>(),
            "export_attempts": records.iter().map(|record| record.work.cfg.cfg_export_attempts).sum::<usize>(),
            "export_successes": records.iter().map(|record| record.work.cfg.cfg_export_successes).sum::<usize>(),
            "export_rejections": records.iter().map(|record| record.work.cfg.cfg_export_rejections).sum::<usize>(),
        },
        "ordinary_free_function_dependency_events": records.iter().map(|record| record.work.body_analysis.ordinary_free_function_dependency_events).sum::<usize>(),
        "specialized_origin_records": records.iter().map(|record| record.work.body_analysis.specialized_origin_records).sum::<usize>(),
        "specialized_free_function_dependency_events": records.iter().map(|record| record.work.body_analysis.specialized_free_function_dependency_events).sum::<usize>(),
        "named_method_dependency_events": records.iter().map(|record| record.work.body_analysis.named_method_dependency_events).sum::<usize>(),
        "named_destructor_dependency_events": records.iter().map(|record| record.work.body_analysis.named_destructor_dependency_events).sum::<usize>(),
        "declaration_type_dependency_events": records.iter().map(|record| record.work.body_analysis.declaration_type_dependency_events).sum::<usize>(),
        "declaration_type_call_head_dependency_events": records.iter().map(|record| record.work.body_analysis.declaration_type_call_head_dependency_events).sum::<usize>(),
        "named_const_dependency_events": records.iter().map(|record| record.work.body_analysis.named_const_dependency_events).sum::<usize>(),
        "manifest_build_invocations": records.iter().map(|record| record.work.manifest.build_invocations).sum::<usize>(),
        "body_owner_tokens": {
            "provisional_slots": records.iter().map(|record| record.work.body_owner_tokens.provisional_slots).sum::<usize>(),
            "authoritative_slots": records.iter().map(|record| record.work.body_owner_tokens.authoritative_slots).sum::<usize>(),
            "slots_validated": records.iter().map(|record| record.work.body_owner_tokens.slots_validated).sum::<usize>(),
            "tokens_installed": records.iter().map(|record| record.work.body_owner_tokens.tokens_installed).sum::<usize>(),
            "validation_failures": records.iter().map(|record| record.work.body_owner_tokens.validation_failures).sum::<usize>(),
        },
        "declaration_reuse": {
            "plan_executions": records.iter().map(|record| record.work.declaration_reuse.plan_executions).sum::<usize>(),
            "durable_records_compared": records.iter().map(|record| record.work.declaration_reuse.durable_records_compared).sum::<usize>(),
            "durable_records_reused": records.iter().map(|record| record.work.declaration_reuse.durable_records_reused).sum::<usize>(),
            "ordinary_declaration_resolutions_skipped": records.iter().map(|record| record.work.declaration_reuse.ordinary_declaration_resolutions_skipped).sum::<usize>(),
            "install_invocations": records.iter().map(|record| record.work.declaration_reuse.install_invocations).sum::<usize>(),
            "fallbacks": records.iter().map(|record| record.work.declaration_reuse.fallbacks).sum::<usize>(),
            "semantic_epochs_started": records.iter().map(|record| record.work.declaration_reuse.semantic_epochs_started).sum::<usize>(),
            "declaration_indexes_built": records.iter().map(|record| record.work.declaration_reuse.declaration_indexes_built).sum::<usize>(),
            "shell_predeclaration_epochs": records.iter().map(|record| record.work.declaration_reuse.shell_predeclaration_epochs).sum::<usize>(),
            "durable_cache_population_exports": records.iter().map(|record| record.work.declaration_reuse.durable_cache_population_exports).sum::<usize>(),
            "fallback_epochs_started": records.iter().map(|record| record.work.declaration_reuse.fallback_epochs_started).sum::<usize>(),
        },
        "semantic_epochs_started": records.iter().map(|record| record.work.declaration_reuse.semantic_epochs_started).sum::<usize>(),
        "declaration_indexes_built": records.iter().map(|record| record.work.declaration_reuse.declaration_indexes_built).sum::<usize>(),
        "shell_predeclaration_epochs": records.iter().map(|record| record.work.declaration_reuse.shell_predeclaration_epochs).sum::<usize>(),
        "durable_cache_population_exports": records.iter().map(|record| record.work.declaration_reuse.durable_cache_population_exports).sum::<usize>(),
        "fallback_epochs_started": records.iter().map(|record| record.work.declaration_reuse.fallback_epochs_started).sum::<usize>(),
    })
}

fn definition_work_json(work: &crate::session::CompilerSessionWork, from: usize) -> Value {
    let records = &work.definition_records[from..];
    json!({
        "bind_invocations": records.iter().map(|record| record.binding.bind_invocations).sum::<usize>(),
        "manifest_build_invocations": records.iter().map(|record| record.manifest.build_invocations).sum::<usize>(),
        "manifest_bindings_visited": records.iter().map(|record| record.issuance.manifest_bindings_visited).sum::<usize>(),
        "ids_issued": records.iter().map(|record| record.issuance.ids_issued).sum::<usize>(),
    })
}

/// Opaque import-discovery product retained by the session.
#[derive(Debug, Clone)]
pub struct ImportDiscoveryRevision {
    pub(crate) inner: Arc<crate::session::ImportDiscoveryRevisionArtifact>,
}

/// Read-only status of an import-discovery product.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportDiscoveryStatus {
    Open,
    ClosedAttempted,
    ClosedValid,
}

impl ImportDiscoveryRevision {
    #[cfg(not(test))]
    pub(crate) fn new(inner: Arc<crate::session::ImportDiscoveryRevisionArtifact>) -> Self {
        Self { inner }
    }

    pub fn status(&self) -> ImportDiscoveryStatus {
        match self.inner.status() {
            crate::session::ImportDiscoveryRevisionStatus::Open => ImportDiscoveryStatus::Open,
            crate::session::ImportDiscoveryRevisionStatus::ClosedAttempted => {
                ImportDiscoveryStatus::ClosedAttempted
            }
            crate::session::ImportDiscoveryRevisionStatus::ClosedValid => {
                ImportDiscoveryStatus::ClosedValid
            }
        }
    }

    pub fn source_revision(&self) -> &crate::SourceRevision {
        self.inner.source_revision()
    }

    pub fn diagnostics(&self) -> &crate::CompileErrors {
        self.inner.diagnostics()
    }

    pub fn diagnostic_snapshot(&self) -> Option<&Arc<crate::FrontendDiagnosticSnapshot>> {
        self.inner.diagnostic_snapshot()
    }

    pub fn unstable_metrics(&self) -> ParseMetrics {
        ParseMetrics::from_work(self.inner.parse_work())
    }

    pub fn graph_input_debug(&self) -> Option<String> {
        self.inner
            .graph()
            .map(|graph| format!("{:?}", graph.input()))
    }

    pub fn accepted_reads_debug(&self) -> String {
        format!("{:?}", self.inner.accepted_read_manifest())
    }

    pub fn observation_ledger_debug(&self) -> String {
        format!("{:?}", self.inner.ledger())
    }
}

/// Deliberate fault selection for the differential incremental oracle.
#[doc(hidden)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DifferentialOracleFault {
    Semantic,
    Diagnostic,
    Import,
}

/// Opaque dependency baseline used only by unstable invalidation benchmarks.
#[derive(Debug, Clone)]
pub struct DependencyBaseline {
    pub(crate) inner: std::sync::Arc<crate::SemanticDependencyInputManifest>,
    owner: Arc<()>,
}

impl DependencyBaseline {
    pub(crate) fn new(
        inner: std::sync::Arc<crate::SemanticDependencyInputManifest>,
        owner: Arc<()>,
    ) -> Self {
        Self { inner, owner }
    }
    pub(crate) fn belongs_to(&self, owner: &Arc<()>) -> bool {
        Arc::ptr_eq(&self.owner, owner)
    }
    pub fn unstable_metrics(&self) -> DependencyManifestMetrics {
        self.inner.unstable_metrics()
    }
    pub fn unstable_durable_artifact_status(&self) -> DurableArtifactStatus {
        self.inner.unstable_durable_artifact_status()
    }
    pub fn input(&self) -> String {
        format!("{:?}", self.inner.input())
    }
    pub fn imports(&self) -> String {
        format!("{:?}", self.inner.imports())
    }
    pub fn definitions(&self) -> String {
        format!("{:?}", self.inner.definitions())
    }
    pub fn definition_fingerprints(&self) -> String {
        format!("{:?}", self.inner.definition_fingerprints())
    }
    pub fn module_imports(&self) -> String {
        format!("{:?}", self.inner.module_imports())
    }
    pub fn free_function_dependencies(&self) -> String {
        format!("{:?}", self.inner.free_function_dependencies())
    }
    pub fn named_method_dependencies(&self) -> String {
        format!("{:?}", self.inner.named_method_dependencies())
    }
    pub fn named_destructor_dependencies(&self) -> String {
        format!("{:?}", self.inner.named_destructor_dependencies())
    }
    pub fn implicit_named_destructor_dependencies(&self) -> String {
        format!("{:?}", self.inner.implicit_named_destructor_dependencies())
    }
    pub fn declaration_type_dependencies(&self) -> String {
        format!("{:?}", self.inner.declaration_type_dependencies())
    }
    pub fn declaration_type_call_head_dependencies(&self) -> String {
        format!("{:?}", self.inner.declaration_type_call_head_dependencies())
    }
    pub fn builtin_type_call_head_inputs(&self) -> String {
        format!("{:?}", self.inner.builtin_type_call_head_inputs())
    }
    pub fn named_const_dependencies(&self) -> String {
        format!("{:?}", self.inner.named_const_dependencies())
    }
    pub fn body_dependencies(&self) -> String {
        format!("{:?}", self.inner.body_dependencies())
    }
    pub fn body_dependency_blockers(&self) -> String {
        format!("{:?}", self.inner.body_dependency_blockers())
    }
    pub fn dependency_blockers(&self) -> String {
        format!("{:?}", self.inner.dependency_blockers())
    }
    pub fn free_function_caller_dependencies_complete(&self) -> bool {
        self.inner.free_function_caller_dependencies_complete()
    }
    pub fn non_generic_named_method_dependencies_complete(&self) -> bool {
        self.inner.non_generic_named_method_dependencies_complete()
    }
    pub fn generic_named_method_dependencies_complete(&self) -> bool {
        self.inner.generic_named_method_dependencies_complete()
    }
    pub fn named_destructor_dependencies_complete(&self) -> bool {
        self.inner.named_destructor_dependencies_complete()
    }
    pub fn implicit_named_destructor_dependencies_complete(&self) -> bool {
        self.inner.implicit_named_destructor_dependencies_complete()
    }
    pub fn declaration_type_dependencies_complete(&self) -> bool {
        self.inner.declaration_type_dependencies_complete()
    }
    pub fn declaration_type_call_head_dependencies_complete(&self) -> bool {
        self.inner
            .declaration_type_call_head_dependencies_complete()
    }
    pub fn supported_type_call_heads_complete(&self) -> bool {
        self.inner.supported_type_call_heads_complete()
    }
    pub fn named_value_const_dependencies_complete(&self) -> bool {
        self.inner.named_value_const_dependencies_complete()
    }
    pub fn semantic_dependency_graph_complete(&self) -> bool {
        self.inner.semantic_dependency_graph_complete()
    }
    pub fn definition_universe_complete(&self) -> bool {
        self.inner.definition_universe_complete()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DependencySurface {
    BodyOwner,
    FreeFunctionCall,
    NonGenericNamedMethodCall,
    GenericNamedMethodCall,
    NamedDestructorCall,
    ImplicitNamedDestructor,
    DeclarationType,
    DeclarationTypeCallHead,
    SupportedTypeCallHead,
    NamedValueConst,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DependencyIncompleteReason {
    AnonymousBodyOwnerUnavailable,
    CallerEndpointUnavailable,
    GenericSubstitutionIdentityUnavailable,
    DestructorEndpointUnavailable,
    AnonymousDropOwnerUnavailable,
    ResolvedTypeIdentityUnavailable,
    TypeCallHeadIdentityUnavailable,
    UnsupportedDynamicTypeCallHead,
    ConstEndpointUnavailable,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DependencyBlocker {
    owner: Option<String>,
    surface: DependencySurface,
    reason: DependencyIncompleteReason,
}
impl DependencyBlocker {
    pub fn owner(&self) -> Option<&str> {
        self.owner.as_deref()
    }
    pub fn surface(&self) -> DependencySurface {
        self.surface
    }
    pub fn reason(&self) -> DependencyIncompleteReason {
        self.reason
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FullInvalidationReason {
    RootChanged,
    ModuleImportsChanged,
    TargetChanged,
    PreviewFeaturesChanged,
    IncompleteDefinitionUniverse,
    IncompleteDependencyGraph(std::sync::Arc<[DependencyBlocker]>),
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InvalidationScope {
    Full {
        reasons: std::sync::Arc<[FullInvalidationReason]>,
    },
    Incremental,
}
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct InvalidationWorkMetrics {
    pub definition_fingerprints_compared: usize,
    pub dependency_edges_visited: usize,
    pub reverse_closure_nodes_visited: usize,
    pub extra_rir_instructions_visited: usize,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidationMetrics {
    scope: InvalidationScope,
    added: Vec<()>,
    removed: Vec<()>,
    changed: Vec<()>,
    invalidated: Vec<()>,
    reusable: Vec<()>,
    work: InvalidationWorkMetrics,
}
impl InvalidationMetrics {
    pub(crate) fn from_plan(plan: &crate::SemanticInvalidationPlan) -> Self {
        fn surface(value: crate::SemanticDependencySurface) -> DependencySurface {
            use crate::SemanticDependencySurface as S;
            match value {
                S::BodyOwner => DependencySurface::BodyOwner,
                S::FreeFunctionCall => DependencySurface::FreeFunctionCall,
                S::NonGenericNamedMethodCall => DependencySurface::NonGenericNamedMethodCall,
                S::GenericNamedMethodCall => DependencySurface::GenericNamedMethodCall,
                S::NamedDestructorCall => DependencySurface::NamedDestructorCall,
                S::ImplicitNamedDestructor => DependencySurface::ImplicitNamedDestructor,
                S::DeclarationType => DependencySurface::DeclarationType,
                S::DeclarationTypeCallHead => DependencySurface::DeclarationTypeCallHead,
                S::SupportedTypeCallHead => DependencySurface::SupportedTypeCallHead,
                S::NamedValueConst => DependencySurface::NamedValueConst,
            }
        }
        fn reason(value: crate::SemanticDependencyIncompleteReason) -> DependencyIncompleteReason {
            use crate::SemanticDependencyIncompleteReason as R;
            match value {
                R::AnonymousBodyOwnerUnavailable => {
                    DependencyIncompleteReason::AnonymousBodyOwnerUnavailable
                }
                R::CallerEndpointUnavailable => {
                    DependencyIncompleteReason::CallerEndpointUnavailable
                }
                R::GenericSubstitutionIdentityUnavailable => {
                    DependencyIncompleteReason::GenericSubstitutionIdentityUnavailable
                }
                R::DestructorEndpointUnavailable => {
                    DependencyIncompleteReason::DestructorEndpointUnavailable
                }
                R::AnonymousDropOwnerUnavailable => {
                    DependencyIncompleteReason::AnonymousDropOwnerUnavailable
                }
                R::ResolvedTypeIdentityUnavailable => {
                    DependencyIncompleteReason::ResolvedTypeIdentityUnavailable
                }
                R::TypeCallHeadIdentityUnavailable => {
                    DependencyIncompleteReason::TypeCallHeadIdentityUnavailable
                }
                R::UnsupportedDynamicTypeCallHead => {
                    DependencyIncompleteReason::UnsupportedDynamicTypeCallHead
                }
                R::ConstEndpointUnavailable => DependencyIncompleteReason::ConstEndpointUnavailable,
            }
        }
        let scope = match plan.scope() {
            crate::SemanticInvalidationScope::Incremental => InvalidationScope::Incremental,
            crate::SemanticInvalidationScope::Full { reasons } => InvalidationScope::Full {
                reasons: reasons
                    .iter()
                    .map(|value| match value {
                        crate::SemanticFullInvalidationReason::RootChanged => {
                            FullInvalidationReason::RootChanged
                        }
                        crate::SemanticFullInvalidationReason::ModuleImportsChanged => {
                            FullInvalidationReason::ModuleImportsChanged
                        }
                        crate::SemanticFullInvalidationReason::TargetChanged => {
                            FullInvalidationReason::TargetChanged
                        }
                        crate::SemanticFullInvalidationReason::PreviewFeaturesChanged => {
                            FullInvalidationReason::PreviewFeaturesChanged
                        }
                        crate::SemanticFullInvalidationReason::IncompleteDefinitionUniverse => {
                            FullInvalidationReason::IncompleteDefinitionUniverse
                        }
                        crate::SemanticFullInvalidationReason::IncompleteDependencyGraph(
                            blockers,
                        ) => FullInvalidationReason::IncompleteDependencyGraph(
                            blockers
                                .iter()
                                .map(|b| DependencyBlocker {
                                    owner: b.owner().map(|o| o.name().to_owned()),
                                    surface: surface(b.surface()),
                                    reason: reason(b.reason()),
                                })
                                .collect(),
                        ),
                    })
                    .collect(),
            },
        };
        let work = plan.work();
        Self {
            scope,
            added: vec![(); plan.added().len()],
            removed: vec![(); plan.removed().len()],
            changed: vec![(); plan.changed().len()],
            invalidated: vec![(); plan.invalidated().len()],
            reusable: vec![(); plan.reusable().len()],
            work: InvalidationWorkMetrics {
                definition_fingerprints_compared: work.definition_fingerprints_compared,
                dependency_edges_visited: work.dependency_edges_visited,
                reverse_closure_nodes_visited: work.reverse_closure_nodes_visited,
                extra_rir_instructions_visited: work.extra_rir_instructions_visited,
            },
        }
    }
    pub fn scope(&self) -> &InvalidationScope {
        &self.scope
    }
    pub fn added(&self) -> &[()] {
        &self.added
    }
    pub fn removed(&self) -> &[()] {
        &self.removed
    }
    pub fn changed(&self) -> &[()] {
        &self.changed
    }
    pub fn invalidated(&self) -> &[()] {
        &self.invalidated
    }
    pub fn reusable(&self) -> &[()] {
        &self.reusable
    }
    pub fn work(&self) -> InvalidationWorkMetrics {
        self.work
    }
}
