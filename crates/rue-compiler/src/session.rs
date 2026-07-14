//! In-process canonical parse, merge, and RIR query orchestration.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::Arc;

use rue_air::{DeclarationBindingWork, SemanticBindingManifestWork};
use rue_span::Span;
use sha2::{Digest, Sha256};

use crate::{
    BoundDefinitionSet, BoundDefinitionWork, CanonicalImportGraph, CanonicalImportGraphValidation,
    CanonicalImportResolution, CanonicalMergeWork, CanonicalMergedProgram, CanonicalParseSession,
    CanonicalRirOutput, CanonicalRirWork, CanonicalSemanticOutput, CanonicalSemanticWork,
    CodegenInputDescriptor, CompileError, CompileErrors, CompileOptions, CompileWarning,
    DurableDeclarationSemantic, ErrorKind, ModuleResolutionInputs, ParseInvalidationSummary,
    ParsedModulesWork, SemanticInputDescriptor, SourceRevision, SourceSnapshot,
    StableDefinitionKey, StableDefinitionKind, StableDefinitionNamespace, StablePreviewFeatures,
    bound_definitions::bind_canonical_definitions_with_work,
    canonical_merge::merge_parsed_modules_reusing_definitions,
    canonical_semantic::{
        analyze_prepared_canonical_program_reusing_declarations,
        analyze_prepared_canonical_program_with_durable_export, prepare_canonical_declarations,
    },
    lower_canonical_rir,
    parsed_modules::ParsedProgram,
    resolve_canonical_import_graph, validate_canonical_import_graph,
};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FrontendQueryWork {
    pub calls: usize,
    pub executions: usize,
    pub reuses: usize,
}

/// Session-owned historical artifacts retained after the latest query.
///
/// These are gauges, not cumulative work counters. Caller-owned
/// [`Arc<FrontendDiagnosticSnapshot>`] values are deliberately excluded: once
/// returned, their lifetime is controlled by the caller rather than the
/// session's eviction policy.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FrontendRetentionMetrics {
    /// Diagnostic snapshots strongly owned by the session.
    pub diagnostic_entries: usize,
    /// Distinct diagnostic source attempts strongly owned by the session.
    pub diagnostic_source_attempts: usize,
    /// Source bytes across those distinct attempts (shared stages count once).
    pub diagnostic_source_bytes: usize,
    /// Distinct dependency manifests strongly owned by all session caches.
    pub dependency_manifests: usize,
    /// Recent semantic invalidation plans strongly owned by the session.
    pub invalidation_plans: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CompilerSessionWork {
    pub updates: usize,
    pub last_parse: ParsedModulesWork,
    pub last_invalidation: ParseInvalidationSummary,
    pub imports: FrontendQueryWork,
    pub import_entries: usize,
    pub import_entries_invalidated: usize,
    pub merge: FrontendQueryWork,
    pub rir: FrontendQueryWork,
    pub downstream_invalidations: usize,
    pub last_merge: CanonicalMergeWork,
    pub last_rir: CanonicalRirWork,
    pub semantic: FrontendQueryWork,
    pub semantic_entries: usize,
    pub semantic_entries_invalidated: usize,
    pub semantic_records: Vec<SemanticQueryRecord>,
    pub definitions: FrontendQueryWork,
    pub definition_entries: usize,
    pub definition_entries_invalidated: usize,
    pub definition_records: Vec<DefinitionQueryRecord>,
    pub diagnostic_publications: usize,
    pub diagnostic_reuses: usize,
    pub diagnostic_invalidations: usize,
    pub dependency_manifests: FrontendQueryWork,
    pub dependency_manifest_records_visited: usize,
    pub dependency_manifest_import_records_visited: usize,
    pub invalidation_plans: FrontendQueryWork,
    pub declaration_reuse_plans: usize,
    pub durable_records_compared: usize,
    pub durable_records_reused: usize,
    pub ordinary_declaration_resolutions_skipped: usize,
    pub durable_installs: usize,
    pub declaration_reuse_fallbacks: usize,
    /// Current bounded-retention gauges for long-lived service integrations.
    pub retention: FrontendRetentionMetrics,
}

/// Maximum number of diagnostic snapshots owned by a frontend session.
///
/// Eviction is deterministic insertion order, except that the latest attempt,
/// latest successful query, and last successful semantic query are protected.
/// Those three protected entries fit within this limit. Callers can explicitly
/// pin any returned snapshot by retaining its `Arc` after session eviction.
pub const FRONTEND_DIAGNOSTIC_RETENTION_LIMIT: usize = 16;

/// Maximum number of recent invalidation plans owned by a frontend session.
///
/// Each entry strongly owns both input manifests. Oldest insertion is evicted
/// first; weak references are intentionally not used because a plan's
/// dependency inputs must remain sound for as long as the cached plan exists.
pub const FRONTEND_INVALIDATION_PLAN_RETENTION_LIMIT: usize = 8;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SemanticDependencyManifestWork {
    pub definition_records_visited: usize,
    pub import_records_visited: usize,
    pub free_function_events_translated: usize,
    pub specialization_origins_validated: usize,
    pub named_method_events_translated: usize,
    pub named_destructor_events_translated: usize,
    pub declaration_type_events_translated: usize,
    pub declaration_type_call_head_events_translated: usize,
    pub builtin_type_call_head_inputs_translated: usize,
    pub named_const_events_translated: usize,
    pub implicit_named_destructor_events_translated: usize,
    pub body_owner_events_translated: usize,
    pub body_named_events_translated: usize,
    pub body_dependency_records_built: usize,
    pub durable_bodies: crate::DurableBodyWork,
    pub extra_rir_instructions_visited: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct StableFreeFunctionDependency {
    pub caller: StableDefinitionKey,
    pub callee: StableDefinitionKey,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum StableNamedMethodDependencyTarget {
    FreeFunction(StableDefinitionKey),
    NamedMethod(StableDefinitionKey),
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct StableNamedMethodDependency {
    pub caller: StableDefinitionKey,
    pub target: StableNamedMethodDependencyTarget,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct StableNamedDestructorDependency {
    pub caller: StableDefinitionKey,
    pub target: StableNamedMethodDependencyTarget,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct StableImplicitNamedDestructorDependency {
    pub source: StableDefinitionKey,
    pub target: StableDefinitionKey,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct StableDeclarationTypeDependency {
    pub source: StableDefinitionKey,
    pub target: StableDefinitionKey,
    pub kind: rue_air::DeclarationTypeDependencyKind,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct StableDeclarationTypeCallHeadDependency {
    pub source: StableDefinitionKey,
    pub callable: StableDefinitionKey,
    pub kind: rue_air::DeclarationTypeDependencyKind,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct StableBuiltinTypeCallHeadInput {
    pub source: StableDefinitionKey,
    pub builtin: rue_air::BuiltinTypeCallHead,
    pub kind: rue_air::DeclarationTypeDependencyKind,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum StableNamedConstDependencyTarget {
    ValueConst(StableDefinitionKey),
    FreeFunction(StableDefinitionKey),
    NamedType(StableDefinitionKey),
    ModuleBinding(StableDefinitionKey),
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct StableNamedConstDependency {
    pub source: StableDefinitionKey,
    pub target: StableNamedConstDependencyTarget,
}

/// Complete stable inputs observed for one successfully analyzed ordinary body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StableBodyDependencyInputRecord {
    owner: StableDefinitionKey,
    fingerprint: StableDefinitionInputFingerprint,
    target: crate::Target,
    preview_features: StablePreviewFeatures,
    direct_dependency_inputs: Arc<[StableDefinitionInputFingerprint]>,
    builtin_type_call_heads: Arc<[StableBuiltinTypeCallHeadInput]>,
    blockers: Arc<[SemanticDependencyBlocker]>,
}

impl StableBodyDependencyInputRecord {
    pub fn owner(&self) -> &StableDefinitionKey {
        &self.owner
    }
    pub fn fingerprint(&self) -> &StableDefinitionInputFingerprint {
        &self.fingerprint
    }
    pub fn target(&self) -> crate::Target {
        self.target
    }
    pub fn preview_features(&self) -> &StablePreviewFeatures {
        &self.preview_features
    }
    pub fn direct_dependency_inputs(&self) -> &[StableDefinitionInputFingerprint] {
        &self.direct_dependency_inputs
    }
    pub fn builtin_type_call_heads(&self) -> &[StableBuiltinTypeCallHeadInput] {
        &self.builtin_type_call_heads
    }
    pub fn blockers(&self) -> &[SemanticDependencyBlocker] {
        &self.blockers
    }
    pub fn reusable_boundary_supported(&self) -> bool {
        self.blockers.is_empty()
    }
}

#[cfg(test)]
mod durable_body_integration_tests {
    use std::{collections::HashMap, sync::Arc};

    use rue_span::FileId;

    use super::*;
    use crate::{SourceMetadata, SourceSnapshot};

    fn snapshot(entries: &[(u32, &str, &str, &str)], root: u32) -> SourceSnapshot {
        let physical = entries
            .iter()
            .map(|(id, path, _, _)| (FileId::new(*id), (*path).to_owned()))
            .collect::<HashMap<_, _>>();
        let logical = entries
            .iter()
            .map(|(id, _, logical, _)| (FileId::new(*id), (*logical).to_owned()))
            .collect::<HashMap<_, _>>();
        SourceSnapshot::new(
            SourceMetadata::new(FileId::new(root), physical, logical).unwrap(),
            entries
                .iter()
                .map(|(id, _, _, text)| (FileId::new(*id), Arc::new((*text).to_owned())))
                .collect(),
        )
        .unwrap()
    }

    fn durable_candidates(source: &SourceSnapshot) -> Arc<[crate::DurableOrdinaryBody]> {
        let mut session = CompilerSession::new();
        session.update(source).into_result().unwrap();
        let semantic = session.semantic(&CompileOptions::default()).unwrap();
        assert!(
            semantic.work().durable_bodies.conversion_completions > 0,
            "semantic work={:#?}",
            semantic.work().durable_bodies
        );
        assert!(
            session.durable_declaration_cache.is_some(),
            "reuse={:#?}",
            semantic.work().declaration_reuse
        );
        let manifest = session
            .semantic_dependency_inputs(&CompileOptions::default(), None)
            .unwrap();
        assert!(
            !manifest.durable_ordinary_bodies().is_empty(),
            "manifest work={:#?} blockers={:#?}",
            manifest.work().durable_bodies,
            manifest.body_dependency_blockers()
        );
        manifest.durable_ordinary_bodies.clone()
    }

    fn normalize_epoch_symbols(text: &str) -> String {
        let bytes = text.as_bytes();
        let mut result = String::with_capacity(text.len());
        let mut index = 0;
        while index < bytes.len() {
            if bytes[index] == b'@' {
                result.push('@');
                index += 1;
                if text[index..].starts_with("sym:") {
                    result.push_str("sym:");
                    index += 4;
                }
                if index < bytes.len() && bytes[index].is_ascii_digit() {
                    while index < bytes.len() && bytes[index].is_ascii_digit() {
                        index += 1;
                    }
                    result.push_str("<epoch-symbol>");
                    continue;
                }
                continue;
            }
            result.push(bytes[index] as char);
            index += 1;
        }
        result
    }

    #[test]
    fn durable_body_candidates_ignore_relocation_file_ids_and_input_order() {
        let a = "pub fn helper() -> i32 { 20 }";
        let b = "pub fn helper() -> i32 { 22 }";
        let main = r#"
            fn main() -> i32 {
                let left = @import("a.rue");
                let right = @import("b.rue");
                left.helper() + right.helper()
            }
        "#;
        let first = snapshot(
            &[
                (1, "/old/main.rue", "main.rue", main),
                (2, "/old/a.rue", "a.rue", a),
                (3, "/old/b.rue", "b.rue", b),
            ],
            1,
        );
        let relocated = snapshot(
            &[
                (93, "/new/b.rue", "b.rue", b),
                (91, "/new/main.rue", "main.rue", main),
                (92, "/new/a.rue", "a.rue", a),
            ],
            91,
        );
        let first = durable_candidates(&first);
        let second = durable_candidates(&relocated);
        assert_eq!(first.len(), 3, "first={first:#?}");
        assert_eq!(first, second);
        let mut helper_modules = first
            .iter()
            .filter(|body| body.payload.owner.name() == "helper")
            .map(|body| body.payload.owner.module().as_str())
            .collect::<Vec<_>>();
        helper_modules.sort_unstable();
        assert_eq!(helper_modules, ["a.rue", "b.rue"]);
    }

    #[test]
    fn durable_body_candidates_preserve_recursive_stable_calls() {
        let source = snapshot(
            &[(
                7,
                "/p/main.rue",
                "main.rue",
                r#"
            fn even(n: i32) -> bool { if n == 0 { true } else { odd(n - 1) } }
            fn odd(n: i32) -> bool { if n == 0 { false } else { even(n - 1) } }
            fn main() -> i32 { if even(8) { 0 } else { 1 } }
        "#,
            )],
            7,
        );
        let candidates = durable_candidates(&source);
        assert_eq!(candidates.len(), 3);
        let calls = candidates
            .iter()
            .flat_map(|body| body.payload.instructions.iter())
            .filter_map(|instruction| match &instruction.data {
                crate::DurableAirInstData::Call { function, .. } => Some(function.name()),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert!(calls.contains(&"even"));
        assert!(calls.contains(&"odd"));
    }

    #[test]
    fn durable_body_candidate_covers_owned_places_abi_and_fresh_import() {
        let source = snapshot(
            &[(
                5,
                "/p/main.rue",
                "main.rue",
                r#"
            struct Pair { x: i32, values: [i32; 2] }
            enum Choice { Value(i32), Empty }
            fn mutate(inout p: Pair) { p.x = 20; p.values[0] = 2; }
            fn read(borrow p: Pair) -> i32 { p.x + p.values[0] }
            fn main() -> i32 {
                let mut p = Pair { x: 1, values: [3, 4] };
                mutate(inout p);
                let choice = Choice.Value(read(borrow p));
                @dbg("durable");
                match choice { Choice.Value(v) => v, Choice.Empty => 0 }
            }
        "#,
            )],
            5,
        );
        let mut session = CompilerSession::new();
        session.update(&source).into_result().unwrap();
        let semantic = session.semantic(&CompileOptions::default()).unwrap();
        assert!(
            semantic.work().durable_bodies.conversion_completions > 0,
            "semantic work={:#?}",
            semantic.work().durable_bodies
        );
        let manifest = session
            .semantic_dependency_inputs(&CompileOptions::default(), None)
            .unwrap();
        let candidates = manifest.durable_ordinary_bodies();
        assert_eq!(
            candidates.len(),
            3,
            "work={:#?} blockers={:#?}",
            manifest.work().durable_bodies,
            manifest.body_dependency_blockers()
        );
        let instructions = candidates
            .iter()
            .flat_map(|body| body.payload.instructions.iter())
            .map(|instruction| &instruction.data)
            .collect::<Vec<_>>();
        assert!(
            instructions
                .iter()
                .any(|data| matches!(data, crate::DurableAirInstData::StructInit { .. }))
        );
        assert!(
            instructions
                .iter()
                .any(|data| matches!(data, crate::DurableAirInstData::ArrayInit { .. }))
        );
        assert!(
            instructions
                .iter()
                .any(|data| matches!(data, crate::DurableAirInstData::EnumVariant { .. }))
        );
        assert!(
            instructions
                .iter()
                .any(|data| matches!(data, crate::DurableAirInstData::StringConst(_)))
        );
        assert!(
            instructions
                .iter()
                .any(|data| matches!(data, crate::DurableAirInstData::Drop { .. }))
        );
        assert!(
            instructions
                .iter()
                .any(|data| matches!(data, crate::DurableAirInstData::PlaceWrite { .. }))
        );
        assert!(
            candidates
                .iter()
                .any(|body| body.payload.param_by_ref.iter().any(|mode| *mode))
        );
        assert!(
            candidates
                .iter()
                .any(|body| body.payload.param_writable.iter().any(|mode| *mode))
        );
        let work = manifest.work().durable_bodies;
        assert_eq!(work.import_attempts, candidates.len());
        assert_eq!(work.import_successes, candidates.len());
        assert_eq!(work.import_failures, 0);
        let declarations = session
            .durable_declaration_cache
            .as_ref()
            .unwrap()
            .semantics
            .clone();
        let epoch = crate::import_durable_declaration_semantics(&declarations).unwrap();
        let definitions = session
            .stable_definitions(&CompileOptions::default())
            .unwrap();
        for candidate in candidates {
            let mut work = crate::DurableBodyWork::default();
            let first = candidate.project_semantic_body(&mut work).unwrap();
            let second = candidate.project_semantic_body(&mut work).unwrap();
            assert_eq!(first, second);
            let record = definitions
                .definitions()
                .iter()
                .find(|record| record.stable_key() == &candidate.payload.owner)
                .unwrap();
            let imported = epoch
                .import_body(&first, record.body_span().unwrap())
                .unwrap();
            let ordinary = semantic
                .functions()
                .iter()
                .find(|function| {
                    function.analyzed.ordinary_owner.is_some_and(|token| {
                        semantic.body_owner_issuer().key_for_body_token(token).ok()
                            == Some(&candidate.payload.owner)
                    })
                })
                .unwrap();
            assert_eq!(
                normalize_epoch_symbols(&imported.air.to_string()),
                normalize_epoch_symbols(&ordinary.analyzed.air.to_string())
            );
            assert_eq!(
                imported.strings,
                candidate
                    .payload
                    .strings
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
            );
            assert_eq!(imported.num_locals, ordinary.analyzed.num_locals);
            assert_eq!(imported.num_param_slots, ordinary.analyzed.num_param_slots);
            assert_eq!(imported.param_modes, ordinary.analyzed.param_modes);
            assert_eq!(
                imported.allow_unreachable_code,
                ordinary.analyzed.allow_unreachable_code
            );
            assert_eq!(
                imported.air.param_drops(),
                ordinary.analyzed.air.param_drops()
            );
            for slot in 0..imported.num_locals.max(ordinary.analyzed.num_locals) {
                assert_eq!(
                    imported.air.is_borrow_slot(slot),
                    ordinary.analyzed.air.is_borrow_slot(slot)
                );
            }
            let imported_strings = imported
                .air
                .instructions()
                .iter()
                .filter_map(|instruction| match instruction.data {
                    rue_air::AirInstData::StringConst(index) => {
                        Some(imported.strings[index as usize].as_str())
                    }
                    _ => None,
                })
                .collect::<Vec<_>>();
            let ordinary_strings = ordinary
                .analyzed
                .air
                .instructions()
                .iter()
                .filter_map(|instruction| match instruction.data {
                    rue_air::AirInstData::StringConst(index) => {
                        Some(semantic.strings()[index as usize].as_str())
                    }
                    _ => None,
                })
                .collect::<Vec<_>>();
            assert_eq!(imported_strings, ordinary_strings);
            let mut cfg_output = rue_cfg::CfgBuilder::build(
                &imported.air,
                imported.num_locals,
                imported.num_param_slots,
                &ordinary.analyzed.name,
                epoch.type_pool(),
                imported.param_modes,
                epoch.interner(),
                imported.allow_unreachable_code,
            );
            assert!(cfg_output.errors.is_empty());
            rue_cfg::opt::optimize(&mut cfg_output.cfg, rue_cfg::OptLevel::O0);
            assert_eq!(
                normalize_epoch_symbols(&cfg_output.cfg.to_string()),
                normalize_epoch_symbols(&ordinary.cfg.to_string())
            );
            assert!(cfg_output.warnings.is_empty());
        }
        assert!(semantic.warnings().is_empty());
    }

    #[test]
    fn unsupported_generic_and_warning_bodies_publish_no_candidates_without_losing_analysis() {
        for source in [
            "fn identity(comptime T: type, x: T) -> T { x } fn main() -> i32 { identity(i32, 42) }",
            "fn main() -> i32 { let unused = 1; 42 }",
        ] {
            let source = snapshot(&[(1, "/p/main.rue", "main.rue", source)], 1);
            let mut session = CompilerSession::new();
            session.update(&source).into_result().unwrap();
            let manifest = session
                .semantic_dependency_inputs(&CompileOptions::default(), None)
                .unwrap();
            assert!(manifest.durable_ordinary_bodies().is_empty());
            assert!(!manifest.body_dependencies().is_empty());
            assert!(
                !session
                    .semantic(&CompileOptions::default())
                    .unwrap()
                    .functions()
                    .is_empty()
            );
        }
    }
}

/// Versioned digest of one immutable semantic input fragment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct StableDefinitionFingerprint([u8; 32]);

impl StableDefinitionFingerprint {
    pub fn bytes(self) -> [u8; 32] {
        self.0
    }
    #[cfg(test)]
    pub(crate) fn for_test(byte: u8) -> Self {
        Self([byte; 32])
    }
}

/// Precision of the parser-authored source partition represented by a
/// definition fingerprint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum StableDefinitionFingerprintPrecision {
    SignatureAndBody,
    SignatureAndInitializer,
    /// All declaration bytes are semantic signature input and there is no
    /// independently executable payload.
    ExactSignature,
    /// The parser has no authoritative executable-payload boundary for this
    /// declaration kind, so its complete declaration is hashed as a signature.
    ConservativeFullDeclaration,
}

/// Immutable, relocation-independent inputs for one stable definition.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct StableDefinitionInputFingerprint {
    /// Schema version for persisted consumers. Bump when domains or partition
    /// semantics change.
    pub schema_version: u16,
    pub key: StableDefinitionKey,
    /// Stable identity and visibility metadata, excluding source locations.
    pub declaration: StableDefinitionFingerprint,
    /// Signature/header bytes, or the full declaration under conservative
    /// precision.
    pub signature: StableDefinitionFingerprint,
    /// Function/method/destructor body or const initializer when exact parser
    /// boundaries are available.
    pub body_or_initializer: Option<StableDefinitionFingerprint>,
    pub precision: StableDefinitionFingerprintPrecision,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum StableModuleImportDependency {
    Resolved {
        importer: crate::ModuleId,
        normalized_specifier: Arc<str>,
        target: crate::ModuleId,
    },
    Missing {
        importer: crate::ModuleId,
        normalized_specifier: Arc<str>,
    },
    Ambiguous {
        importer: crate::ModuleId,
        normalized_specifier: Arc<str>,
        file_module: crate::ModuleId,
        directory_module: crate::ModuleId,
    },
}

/// A semantic dependency surface whose captured edges may be incomplete.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SemanticDependencySurface {
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

/// The production evidence which prevents a dependency surface from being trusted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SemanticDependencyIncompleteReason {
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

/// A deterministic, stable-keyed reason that semantic reuse must fail closed.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SemanticDependencyBlocker {
    owner: Option<StableDefinitionKey>,
    surface: SemanticDependencySurface,
    reason: SemanticDependencyIncompleteReason,
}

impl SemanticDependencyBlocker {
    pub fn owner(&self) -> Option<&StableDefinitionKey> {
        self.owner.as_ref()
    }
    pub fn surface(&self) -> SemanticDependencySurface {
        self.surface
    }
    pub fn reason(&self) -> SemanticDependencyIncompleteReason {
        self.reason
    }
}

#[derive(Debug, Clone)]
pub struct SemanticDependencyInputManifest {
    input: SemanticInputDescriptor,
    imports: CanonicalImportGraph,
    definitions: Arc<[StableDefinitionKey]>,
    definition_fingerprints: Arc<[StableDefinitionInputFingerprint]>,
    module_imports: Arc<[StableModuleImportDependency]>,
    free_function_dependencies: Arc<[StableFreeFunctionDependency]>,
    named_method_dependencies: Arc<[StableNamedMethodDependency]>,
    named_destructor_dependencies: Arc<[StableNamedDestructorDependency]>,
    implicit_named_destructor_dependencies: Arc<[StableImplicitNamedDestructorDependency]>,
    declaration_type_dependencies: Arc<[StableDeclarationTypeDependency]>,
    declaration_type_call_head_dependencies: Arc<[StableDeclarationTypeCallHeadDependency]>,
    builtin_type_call_head_inputs: Arc<[StableBuiltinTypeCallHeadInput]>,
    named_const_dependencies: Arc<[StableNamedConstDependency]>,
    body_dependencies: Arc<[StableBodyDependencyInputRecord]>,
    durable_ordinary_bodies: Arc<[crate::DurableOrdinaryBody]>,
    body_dependency_blockers: Arc<[SemanticDependencyBlocker]>,
    dependency_blockers: Arc<[SemanticDependencyBlocker]>,
    definition_universe_complete: bool,
    work: SemanticDependencyManifestWork,
}

impl SemanticDependencyInputManifest {
    pub fn input(&self) -> &SemanticInputDescriptor {
        &self.input
    }
    pub fn imports(&self) -> &CanonicalImportGraph {
        &self.imports
    }
    pub fn definitions(&self) -> &[StableDefinitionKey] {
        &self.definitions
    }
    pub fn definition_fingerprints(&self) -> &[StableDefinitionInputFingerprint] {
        &self.definition_fingerprints
    }
    pub fn module_imports(&self) -> &[StableModuleImportDependency] {
        &self.module_imports
    }
    pub fn free_function_dependencies(&self) -> &[StableFreeFunctionDependency] {
        &self.free_function_dependencies
    }
    pub fn free_function_caller_dependencies_complete(&self) -> bool {
        self.surface_complete(SemanticDependencySurface::FreeFunctionCall)
    }
    pub fn named_method_dependencies(&self) -> &[StableNamedMethodDependency] {
        &self.named_method_dependencies
    }
    pub fn non_generic_named_method_dependencies_complete(&self) -> bool {
        self.surface_complete(SemanticDependencySurface::NonGenericNamedMethodCall)
    }
    pub fn generic_named_method_dependencies_complete(&self) -> bool {
        self.surface_complete(SemanticDependencySurface::GenericNamedMethodCall)
    }
    pub fn named_destructor_dependencies(&self) -> &[StableNamedDestructorDependency] {
        &self.named_destructor_dependencies
    }
    pub fn named_destructor_dependencies_complete(&self) -> bool {
        self.surface_complete(SemanticDependencySurface::NamedDestructorCall)
    }
    pub fn implicit_named_destructor_dependencies(
        &self,
    ) -> &[StableImplicitNamedDestructorDependency] {
        &self.implicit_named_destructor_dependencies
    }
    pub fn implicit_named_destructor_dependencies_complete(&self) -> bool {
        self.surface_complete(SemanticDependencySurface::ImplicitNamedDestructor)
    }
    pub fn declaration_type_dependencies(&self) -> &[StableDeclarationTypeDependency] {
        &self.declaration_type_dependencies
    }
    pub fn declaration_type_dependencies_complete(&self) -> bool {
        self.surface_complete(SemanticDependencySurface::DeclarationType)
    }
    pub fn declaration_type_call_head_dependencies(
        &self,
    ) -> &[StableDeclarationTypeCallHeadDependency] {
        &self.declaration_type_call_head_dependencies
    }
    pub fn declaration_type_call_head_dependencies_complete(&self) -> bool {
        self.surface_complete(SemanticDependencySurface::DeclarationTypeCallHead)
    }
    pub fn builtin_type_call_head_inputs(&self) -> &[StableBuiltinTypeCallHeadInput] {
        &self.builtin_type_call_head_inputs
    }
    pub fn supported_type_call_heads_complete(&self) -> bool {
        self.surface_complete(SemanticDependencySurface::SupportedTypeCallHead)
    }
    pub fn named_const_dependencies(&self) -> &[StableNamedConstDependency] {
        &self.named_const_dependencies
    }
    pub fn body_dependencies(&self) -> &[StableBodyDependencyInputRecord] {
        &self.body_dependencies
    }
    /// Observation-only durable candidates. No production body query consumes
    /// these records in this slice.
    pub fn durable_ordinary_bodies(&self) -> &[crate::DurableOrdinaryBody] {
        &self.durable_ordinary_bodies
    }
    pub fn body_dependency_blockers(&self) -> &[SemanticDependencyBlocker] {
        &self.body_dependency_blockers
    }
    pub fn named_value_const_dependencies_complete(&self) -> bool {
        self.surface_complete(SemanticDependencySurface::NamedValueConst)
    }
    pub fn semantic_dependency_graph_complete(&self) -> bool {
        self.dependency_blockers.is_empty()
    }
    pub fn dependency_blockers(&self) -> &[SemanticDependencyBlocker] {
        &self.dependency_blockers
    }
    pub fn definition_universe_complete(&self) -> bool {
        self.definition_universe_complete
    }
    pub fn work(&self) -> SemanticDependencyManifestWork {
        self.work
    }

    fn surface_complete(&self, surface: SemanticDependencySurface) -> bool {
        !self
            .dependency_blockers
            .iter()
            .any(|blocker| blocker.surface == surface)
    }
}

/// A reason why semantic results cannot soundly be reused across two manifests.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SemanticFullInvalidationReason {
    RootChanged,
    ModuleImportsChanged,
    TargetChanged,
    PreviewFeaturesChanged,
    IncompleteDefinitionUniverse,
    IncompleteDependencyGraph(Arc<[SemanticDependencyBlocker]>),
}

/// Explicit work performed while planning semantic invalidation.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SemanticInvalidationWork {
    pub definition_fingerprints_compared: usize,
    pub dependency_edges_visited: usize,
    pub reverse_closure_nodes_visited: usize,
    pub extra_rir_instructions_visited: usize,
}

/// Immutable, stable-keyed invalidation decision for two semantic input manifests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SemanticInvalidationScope {
    Full {
        reasons: Arc<[SemanticFullInvalidationReason]>,
    },
    Incremental,
}

#[derive(Debug, Clone)]
pub struct SemanticInvalidationPlan {
    scope: SemanticInvalidationScope,
    added: Arc<[StableDefinitionKey]>,
    removed: Arc<[StableDefinitionKey]>,
    changed: Arc<[StableDefinitionKey]>,
    invalidated: Arc<[StableDefinitionKey]>,
    reusable: Arc<[StableDefinitionKey]>,
    work: SemanticInvalidationWork,
}

impl SemanticInvalidationPlan {
    pub fn scope(&self) -> &SemanticInvalidationScope {
        &self.scope
    }
    pub fn added(&self) -> &[StableDefinitionKey] {
        &self.added
    }
    pub fn removed(&self) -> &[StableDefinitionKey] {
        &self.removed
    }
    pub fn changed(&self) -> &[StableDefinitionKey] {
        &self.changed
    }
    pub fn invalidated(&self) -> &[StableDefinitionKey] {
        &self.invalidated
    }
    pub fn reusable(&self) -> &[StableDefinitionKey] {
        &self.reusable
    }
    pub fn work(&self) -> SemanticInvalidationWork {
        self.work
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum FrontendDiagnosticStage {
    Syntax,
    Merge,
    Semantic(CodegenInputDescriptor),
}

#[derive(Debug, Clone)]
pub struct FrontendDiagnosticSnapshot {
    source: SourceSnapshot,
    stage: FrontendDiagnosticStage,
    errors: Arc<[CompileError]>,
    warnings: Arc<[CompileWarning]>,
}

impl FrontendDiagnosticSnapshot {
    pub fn source(&self) -> &SourceSnapshot {
        &self.source
    }
    pub fn source_revision(&self) -> &SourceRevision {
        self.source.source_revision()
    }
    pub fn stage(&self) -> &FrontendDiagnosticStage {
        &self.stage
    }
    pub fn errors(&self) -> &[CompileError] {
        &self.errors
    }
    pub fn warnings(&self) -> &[CompileWarning] {
        &self.warnings
    }
    pub fn is_success(&self) -> bool {
        self.errors.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ImportGraphInputDescriptor {
    pub sources: SourceRevision,
    pub resolution: ModuleResolutionInputs,
    pub std_dir: Option<Arc<str>>,
}

#[derive(Debug, Clone)]
pub struct CanonicalImportGraphOutput {
    input: ImportGraphInputDescriptor,
    graph: CanonicalImportGraph,
    validation: CanonicalImportGraphValidation,
}

impl CanonicalImportGraphOutput {
    pub fn input(&self) -> &ImportGraphInputDescriptor {
        &self.input
    }
    pub fn graph(&self) -> &CanonicalImportGraph {
        &self.graph
    }
    pub fn validation(&self) -> &CanonicalImportGraphValidation {
        &self.validation
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SemanticQueryRecord {
    pub input: CodegenInputDescriptor,
    pub work: CanonicalSemanticWork,
    pub failed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DefinitionQueryRecord {
    pub input: SemanticInputDescriptor,
    pub binding: DeclarationBindingWork,
    pub manifest: SemanticBindingManifestWork,
    pub issuance: BoundDefinitionWork,
    pub failed: bool,
}

#[derive(Debug)]
pub struct CompilerSessionUpdate {
    result: Result<Arc<ParsedProgram>, CompileErrors>,
    work: ParsedModulesWork,
    invalidation: ParseInvalidationSummary,
    downstream_invalidated: bool,
    diagnostics: Arc<FrontendDiagnosticSnapshot>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportDiscoveryRevisionStatus {
    Open,
    ClosedAttempted,
    ClosedValid,
}

#[derive(Debug, Clone)]
pub struct ImportDiscoveryRevisionArtifact {
    status: ImportDiscoveryRevisionStatus,
    source_revision: SourceRevision,
    context: crate::ImportDiscoveryContext,
    snapshot: SourceSnapshot,
    program: Option<Arc<ParsedProgram>>,
    plan: Option<crate::ImportDiscoveryPlan>,
    ledger: crate::ImportObservationLedger,
    accepted_reads: Arc<[crate::AcceptedReadManifestEntry]>,
    graph: Option<Arc<CanonicalImportGraphOutput>>,
    diagnostics: CompileErrors,
}

impl ImportDiscoveryRevisionArtifact {
    pub fn status(&self) -> ImportDiscoveryRevisionStatus {
        self.status
    }
    pub fn source_revision(&self) -> &SourceRevision {
        &self.source_revision
    }
    pub fn context(&self) -> &crate::ImportDiscoveryContext {
        &self.context
    }
    pub fn snapshot(&self) -> &SourceSnapshot {
        &self.snapshot
    }
    pub fn plan(&self) -> Option<&crate::ImportDiscoveryPlan> {
        self.plan.as_ref()
    }
    pub fn ledger(&self) -> &crate::ImportObservationLedger {
        &self.ledger
    }
    pub fn accepted_read_manifest(&self) -> &[crate::AcceptedReadManifestEntry] {
        &self.accepted_reads
    }
    pub fn graph(&self) -> Option<&Arc<CanonicalImportGraphOutput>> {
        self.graph.as_ref()
    }
    pub fn diagnostics(&self) -> &CompileErrors {
        &self.diagnostics
    }
}

impl CompilerSessionUpdate {
    pub fn result(&self) -> Result<&Arc<ParsedProgram>, &CompileErrors> {
        self.result.as_ref()
    }
    pub fn into_result(self) -> Result<Arc<ParsedProgram>, CompileErrors> {
        self.result
    }
    pub fn work(&self) -> ParsedModulesWork {
        self.work
    }
    pub fn invalidation(&self) -> &ParseInvalidationSummary {
        &self.invalidation
    }
    pub fn downstream_invalidated(&self) -> bool {
        self.downstream_invalidated
    }
    pub fn diagnostics(&self) -> &Arc<FrontendDiagnosticSnapshot> {
        &self.diagnostics
    }
}

#[derive(Debug, Default)]
pub struct CompilerSession {
    parse: CanonicalParseSession,
    discovery_parse: CanonicalParseSession,
    discovery_staging_active: bool,
    discovery_attempt: Option<Arc<ImportDiscoveryRevisionArtifact>>,
    committed_discovery: Option<Arc<ImportDiscoveryRevisionArtifact>>,
    published: Option<Arc<ParsedProgram>>,
    published_snapshot: Option<SourceSnapshot>,
    batch_diagnostic_order: Option<Vec<crate::ModuleId>>,
    merge_cache: Option<Result<Arc<CanonicalMergedProgram>, CompileErrors>>,
    definition_shard_baseline: Option<crate::DefinitionSnapshot>,
    rir_cache: Option<Arc<CanonicalRirOutput>>,
    import_cache: Vec<ImportCacheEntry>,
    semantic_cache: Vec<SemanticCacheEntry>,
    definition_cache: Vec<DefinitionCacheEntry>,
    work: CompilerSessionWork,
    diagnostic_cache: VecDeque<Arc<FrontendDiagnosticSnapshot>>,
    latest_diagnostics: Option<Arc<FrontendDiagnosticSnapshot>>,
    latest_successful_diagnostics: Option<Arc<FrontendDiagnosticSnapshot>>,
    last_good_semantic_diagnostics: Option<Arc<FrontendDiagnosticSnapshot>>,
    dependency_manifest_cache: Vec<Arc<SemanticDependencyInputManifest>>,
    invalidation_plan_cache: VecDeque<InvalidationPlanCacheEntry>,
    durable_declaration_cache: Option<DurableDeclarationCache>,
}

#[derive(Debug)]
struct DurableDeclarationCache {
    root: crate::ModuleId,
    target: crate::Target,
    preview_features: crate::PreviewFeatures,
    fingerprints: Arc<[StableDefinitionInputFingerprint]>,
    semantics: Arc<[DurableDeclarationSemantic]>,
}

#[derive(Debug)]
struct InvalidationPlanCacheEntry {
    previous: Arc<SemanticDependencyInputManifest>,
    current: Arc<SemanticDependencyInputManifest>,
    plan: Arc<SemanticInvalidationPlan>,
}

#[derive(Debug)]
struct ImportCacheEntry {
    input: ImportGraphInputDescriptor,
    result: Result<Arc<CanonicalImportGraphOutput>, CompileErrors>,
}

#[derive(Debug)]
struct SemanticCacheEntry {
    input: CodegenInputDescriptor,
    result: Result<Arc<CanonicalSemanticOutput>, CompileErrors>,
}

#[derive(Debug)]
struct DefinitionCacheEntry {
    input: SemanticInputDescriptor,
    result: Result<Arc<BoundDefinitionSet>, CompileErrors>,
}

impl CompilerSession {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn published(&self) -> Option<&Arc<ParsedProgram>> {
        self.published.as_ref()
    }
    /// Derive the pre-closure import plan for the session's current parsed
    /// revision. Hosts may execute only the requests carried by this query.
    pub fn import_discovery_plan(
        &self,
        context: crate::ImportDiscoveryContext,
    ) -> crate::CompileResult<crate::ImportDiscoveryPlan> {
        let program = self.published.as_ref().ok_or_else(|| {
            CompileError::without_span(ErrorKind::InvalidCompilerInput(
                "import discovery requires a successfully parsed staging revision".into(),
            ))
        })?;
        crate::ImportDiscoveryPlan::new(program, context)
    }
    pub fn discovery_attempt(&self) -> Option<&Arc<ImportDiscoveryRevisionArtifact>> {
        self.discovery_attempt.as_ref()
    }
    pub fn last_good_discovery(&self) -> Option<&Arc<ImportDiscoveryRevisionArtifact>> {
        self.committed_import_discovery()
    }

    /// The exact closed-valid discovery revision adopted by this session.
    ///
    /// Downstream compilation must consume this artifact (or queries that
    /// delegate to it), rather than reconstructing import or standard-library
    /// resolution from the source snapshot alone.
    pub fn committed_import_discovery(&self) -> Option<&Arc<ImportDiscoveryRevisionArtifact>> {
        self.committed_discovery.as_ref()
    }

    /// Return the canonical graph and captured resolution context adopted for
    /// the current compiler revision.
    pub fn committed_import_graph(&self) -> Result<Arc<CanonicalImportGraphOutput>, CompileErrors> {
        let committed = self.committed_discovery.as_ref().ok_or_else(|| {
            CompileErrors::from(CompileError::without_span(ErrorKind::InvalidCompilerInput(
                "no closed-valid import discovery revision is committed".into(),
            )))
        })?;
        Ok(committed
            .graph()
            .expect("closed-valid discovery revisions retain their canonical graph")
            .clone())
    }

    /// Parse an immutable staging snapshot without publishing it to semantic
    /// or dependency queries.
    pub fn stage_import_discovery(
        &mut self,
        snapshot: &SourceSnapshot,
        context: crate::ImportDiscoveryContext,
        accepted_reads: Arc<[crate::AcceptedReadManifestEntry]>,
        carried_ledger: crate::ImportObservationLedger,
    ) -> Result<crate::ImportDiscoveryPlan, CompileErrors> {
        self.discovery_staging_active = true;
        let source_revision = snapshot.source_revision().clone();
        let publish_failed_attempt = |errors: CompileErrors| ImportDiscoveryRevisionArtifact {
            status: ImportDiscoveryRevisionStatus::ClosedAttempted,
            source_revision: source_revision.clone(),
            context: context.clone(),
            snapshot: snapshot.clone(),
            program: None,
            plan: None,
            ledger: carried_ledger.clone(),
            accepted_reads: accepted_reads.clone(),
            graph: None,
            diagnostics: errors,
        };
        if let Err(errors) = validate_accepted_read_manifest(snapshot, &accepted_reads) {
            self.discovery_attempt = Some(Arc::new(publish_failed_attempt(errors.clone())));
            return Err(errors);
        }
        let program = match self.discovery_parse.update(snapshot).into_result() {
            Ok(program) => program,
            Err(errors) => {
                self.discovery_attempt = Some(Arc::new(publish_failed_attempt(errors.clone())));
                return Err(errors);
            }
        };
        let plan = match crate::ImportDiscoveryPlan::new(&program, context.clone()) {
            Ok(plan) => plan,
            Err(error) => {
                let errors = CompileErrors::from(error);
                self.discovery_attempt = Some(Arc::new(publish_failed_attempt(errors.clone())));
                return Err(errors);
            }
        };
        self.discovery_attempt = Some(Arc::new(ImportDiscoveryRevisionArtifact {
            status: ImportDiscoveryRevisionStatus::Open,
            source_revision: program.source_revision().clone(),
            context,
            snapshot: snapshot.clone(),
            program: Some(program),
            plan: Some(plan.clone()),
            ledger: carried_ledger,
            accepted_reads,
            graph: None,
            diagnostics: CompileErrors::new(),
        }));
        Ok(plan)
    }

    /// Close the current staging revision. Missing, ambiguous, and malformed
    /// imports retain a closed attempted artifact; only a diagnostic-free graph
    /// is atomically adopted as the committed compiler revision.
    pub fn close_import_discovery(
        &mut self,
        ledger: crate::ImportObservationLedger,
    ) -> Result<Arc<ImportDiscoveryRevisionArtifact>, CompileErrors> {
        let open = self
            .discovery_attempt
            .as_deref()
            .filter(|artifact| artifact.status == ImportDiscoveryRevisionStatus::Open)
            .ok_or_else(|| CompileErrors::from(no_published_program()))?
            .clone();
        let plan = open
            .plan
            .as_ref()
            .expect("open discovery attempt retains its plan")
            .clone();
        let program = open
            .program
            .as_ref()
            .expect("open discovery attempt retains its program")
            .clone();
        plan.validate_ledger(&ledger).map_err(CompileErrors::from)?;
        if !plan.pending_requests(&ledger).is_empty() {
            let errors =
                CompileErrors::from(CompileError::without_span(ErrorKind::InvalidCompilerInput(
                    "import discovery ledger is incomplete; the revision remains open".into(),
                )));
            let artifact = Arc::new(ImportDiscoveryRevisionArtifact {
                ledger,
                diagnostics: errors.clone(),
                ..open
            });
            self.discovery_attempt = Some(artifact);
            return Err(errors);
        }
        let diagnostics = plan.diagnostics(&program, &ledger);
        if plan.failures(&ledger).next().is_some() {
            let artifact = Arc::new(ImportDiscoveryRevisionArtifact {
                ledger,
                diagnostics: diagnostics.clone(),
                ..open
            });
            self.discovery_attempt = Some(artifact);
            return Err(diagnostics);
        }

        let resolution = ModuleResolutionInputs::new(
            program.root().clone(),
            program
                .modules()
                .iter()
                .map(|module| crate::ModuleResolutionInput {
                    module: module.module_id().clone(),
                    physical_path: Arc::from(module.physical_path()),
                })
                .collect(),
        )?;
        let input = ImportGraphInputDescriptor {
            sources: program.source_revision().clone(),
            resolution,
            std_dir: open.context.std_root().map(Arc::from),
        };
        let graph = plan.reduce_graph(program.root().clone(), &ledger, &open.accepted_reads)?;
        let validation = validate_canonical_import_graph(&graph, &input.resolution);
        let graph = Arc::new(CanonicalImportGraphOutput {
            input,
            graph,
            validation,
        });
        let resolution_only = graph.validation().problems().iter().all(|problem| {
            matches!(
                problem,
                crate::CanonicalImportGraphProblem::MissingResolution { .. }
                    | crate::CanonicalImportGraphProblem::AmbiguousResolution { .. }
            )
        });
        if !resolution_only {
            let mut errors = diagnostics;
            errors.push(CompileError::without_span(ErrorKind::InvalidCompilerInput(
                "import discovery produced a structurally invalid canonical graph".into(),
            )));
            let artifact = Arc::new(ImportDiscoveryRevisionArtifact {
                ledger,
                diagnostics: errors.clone(),
                ..open
            });
            self.discovery_attempt = Some(artifact);
            return Err(errors);
        }
        if !graph.validation().is_valid() || !diagnostics.is_empty() {
            let artifact = Arc::new(ImportDiscoveryRevisionArtifact {
                status: ImportDiscoveryRevisionStatus::ClosedAttempted,
                ledger,
                graph: Some(graph),
                diagnostics: diagnostics.clone(),
                ..open
            });
            self.discovery_attempt = Some(artifact);
            return Err(diagnostics);
        }

        self.update_for_presentation(&open.snapshot).into_result()?;
        let artifact = Arc::new(ImportDiscoveryRevisionArtifact {
            status: ImportDiscoveryRevisionStatus::ClosedValid,
            ledger,
            graph: Some(graph),
            diagnostics,
            ..open
        });
        self.discovery_attempt = Some(artifact.clone());
        self.committed_discovery = Some(artifact.clone());
        self.discovery_staging_active = false;
        Ok(artifact)
    }

    fn require_closed_discovery(&self) -> Result<(), CompileErrors> {
        if self.discovery_staging_active {
            return Err(CompileErrors::from(CompileError::without_span(
                ErrorKind::InvalidCompilerInput(
                    "semantic and dependency queries require a closed valid discovery revision"
                        .into(),
                ),
            )));
        }
        Ok(())
    }
    pub fn work(&self) -> &CompilerSessionWork {
        &self.work
    }
    /// Diagnostic snapshot from the most recently attempted query, whether it
    /// succeeded or failed.
    pub fn latest_diagnostics(&self) -> Option<&Arc<FrontendDiagnosticSnapshot>> {
        self.latest_diagnostics.as_ref()
    }
    /// Most recently queried diagnostic snapshot with no errors.
    pub fn latest_successful_diagnostics(&self) -> Option<&Arc<FrontendDiagnosticSnapshot>> {
        self.latest_successful_diagnostics.as_ref()
    }
    /// Most recent successful semantic diagnostic snapshot.
    ///
    /// Syntax or semantic failures never replace this last-good semantic
    /// baseline. A caller may clone the returned `Arc` to pin it independently
    /// of later session eviction.
    pub fn last_good_semantic_diagnostics(&self) -> Option<&Arc<FrontendDiagnosticSnapshot>> {
        self.last_good_semantic_diagnostics.as_ref()
    }
    /// Look up an exact source-attempt and query-stage pair while it remains in
    /// the bounded recent cache.
    ///
    /// Clone a returned `Arc` when the artifact must outlive cache eviction.
    pub fn diagnostics_for(
        &self,
        source: &SourceSnapshot,
        stage: &FrontendDiagnosticStage,
    ) -> Option<&Arc<FrontendDiagnosticSnapshot>> {
        self.diagnostic_cache
            .iter()
            .rev()
            .find(|entry| same_attempt(entry.source(), source) && entry.stage() == stage)
    }

    fn publish_diagnostics(
        &mut self,
        source: &SourceSnapshot,
        stage: FrontendDiagnosticStage,
        errors: Option<&CompileErrors>,
        warnings: &[CompileWarning],
    ) -> Arc<FrontendDiagnosticSnapshot> {
        if let Some(existing) = self
            .diagnostic_cache
            .iter()
            .find(|entry| same_attempt(entry.source(), source) && entry.stage() == &stage)
            .cloned()
        {
            self.work.diagnostic_reuses += 1;
            self.latest_diagnostics = Some(existing.clone());
            if existing.is_success() {
                self.latest_successful_diagnostics = Some(existing.clone());
                if matches!(existing.stage(), FrontendDiagnosticStage::Semantic(_)) {
                    self.last_good_semantic_diagnostics = Some(existing.clone());
                }
            }
            self.evict_diagnostics();
            self.refresh_retention_metrics();
            return existing;
        }
        if self.latest_diagnostics.is_some() {
            self.work.diagnostic_invalidations += 1;
        }
        let snapshot = Arc::new(FrontendDiagnosticSnapshot {
            source: source.clone(),
            stage,
            errors: errors
                .map(|errors| errors.iter().cloned().collect::<Vec<_>>())
                .unwrap_or_default()
                .into(),
            warnings: warnings.to_vec().into(),
        });
        self.work.diagnostic_publications += 1;
        self.diagnostic_cache.push_back(snapshot.clone());
        self.latest_diagnostics = Some(snapshot.clone());
        if snapshot.is_success() {
            self.latest_successful_diagnostics = Some(snapshot.clone());
            if matches!(snapshot.stage(), FrontendDiagnosticStage::Semantic(_)) {
                self.last_good_semantic_diagnostics = Some(snapshot.clone());
            }
        }
        self.evict_diagnostics();
        self.refresh_retention_metrics();
        snapshot
    }

    fn evict_diagnostics(&mut self) {
        while self.diagnostic_cache.len() > FRONTEND_DIAGNOSTIC_RETENTION_LIMIT {
            let evict = self.diagnostic_cache.iter().position(|entry| {
                !self
                    .latest_diagnostics
                    .as_ref()
                    .is_some_and(|protected| Arc::ptr_eq(entry, protected))
                    && !self
                        .latest_successful_diagnostics
                        .as_ref()
                        .is_some_and(|protected| Arc::ptr_eq(entry, protected))
                    && !self
                        .last_good_semantic_diagnostics
                        .as_ref()
                        .is_some_and(|protected| Arc::ptr_eq(entry, protected))
            });
            let Some(evict) = evict else {
                break;
            };
            self.diagnostic_cache.remove(evict);
        }
        debug_assert!(self.diagnostic_cache.len() <= FRONTEND_DIAGNOSTIC_RETENTION_LIMIT);
    }

    fn refresh_retention_metrics(&mut self) {
        let mut attempts: Vec<&SourceSnapshot> = Vec::new();
        let mut diagnostic_source_bytes = 0;
        for diagnostic in &self.diagnostic_cache {
            if attempts
                .iter()
                .any(|source| same_attempt(source, diagnostic.source()))
            {
                continue;
            }
            diagnostic_source_bytes += diagnostic
                .source()
                .files()
                .map(|source| source.source.len())
                .sum::<usize>();
            attempts.push(diagnostic.source());
        }

        let mut manifests = BTreeSet::new();
        for manifest in &self.dependency_manifest_cache {
            manifests.insert(Arc::as_ptr(manifest) as usize);
        }
        for entry in &self.invalidation_plan_cache {
            manifests.insert(Arc::as_ptr(&entry.previous) as usize);
            manifests.insert(Arc::as_ptr(&entry.current) as usize);
        }
        self.work.retention = FrontendRetentionMetrics {
            diagnostic_entries: self.diagnostic_cache.len(),
            diagnostic_source_attempts: attempts.len(),
            diagnostic_source_bytes,
            dependency_manifests: manifests.len(),
            invalidation_plans: self.invalidation_plan_cache.len(),
        };
    }

    pub fn update(&mut self, snapshot: &SourceSnapshot) -> CompilerSessionUpdate {
        self.batch_diagnostic_order = None;
        let update = self.parse.update(snapshot);
        self.finish_update(snapshot, update)
    }

    /// Publish a snapshot while retaining its caller-selected presentation order.
    ///
    /// Query artifacts still use stable module identity. Only syntax and merge
    /// diagnostic ordering follows [`SourceSnapshot::files`], which is useful
    /// for command-line and other presentation-oriented consumers.
    pub fn update_for_presentation(&mut self, snapshot: &SourceSnapshot) -> CompilerSessionUpdate {
        self.batch_diagnostic_order = Some(
            snapshot
                .files()
                .map(|source| snapshot.module_id(source.file_id).unwrap().clone())
                .collect(),
        );
        let update = self.parse.update_for_batch(snapshot);
        self.finish_update(snapshot, update)
    }

    fn finish_update(
        &mut self,
        snapshot: &SourceSnapshot,
        update: crate::CanonicalParseUpdate,
    ) -> CompilerSessionUpdate {
        self.work.updates += 1;
        let parse_work = update.work();
        let invalidation = update.invalidation().clone();
        self.work.last_parse = parse_work;
        self.work.last_invalidation = invalidation.clone();
        let result = update.into_result();
        let diagnostics = self.publish_diagnostics(
            snapshot,
            FrontendDiagnosticStage::Syntax,
            result.as_ref().err(),
            &[],
        );
        match result {
            Ok(candidate) => {
                if self.discovery_attempt.as_deref().is_some_and(|artifact| {
                    artifact.source_revision != *candidate.source_revision()
                }) {
                    self.discovery_attempt = None;
                }
                if self.committed_discovery.as_deref().is_some_and(|artifact| {
                    artifact.source_revision != *candidate.source_revision()
                }) {
                    self.committed_discovery = None;
                }
                let exact = self.published.as_deref().is_some_and(|published| {
                    programs_are_pointer_equivalent(published, &candidate)
                });
                let downstream_invalidated = self.published.is_some() && !exact;
                if exact {
                    CompilerSessionUpdate {
                        result: Ok(self.published.as_ref().unwrap().clone()),
                        work: parse_work,
                        invalidation,
                        downstream_invalidated: false,
                        diagnostics,
                    }
                } else {
                    if downstream_invalidated {
                        self.work.downstream_invalidations += 1;
                    }
                    self.merge_cache = None;
                    self.rir_cache = None;
                    self.work.import_entries_invalidated += self.import_cache.len();
                    self.import_cache.clear();
                    self.work.import_entries = 0;
                    self.work.semantic_entries_invalidated += self.semantic_cache.len();
                    self.semantic_cache.clear();
                    self.work.definition_entries_invalidated += self.definition_cache.len();
                    self.definition_cache.clear();
                    self.work.last_merge = CanonicalMergeWork::default();
                    self.work.last_rir = CanonicalRirWork::default();
                    self.work.semantic_entries = 0;
                    self.work.semantic_records.clear();
                    self.work.definition_entries = 0;
                    self.work.definition_records.clear();
                    self.dependency_manifest_cache.clear();
                    self.refresh_retention_metrics();
                    self.published = Some(candidate.clone());
                    self.published_snapshot = Some(snapshot.clone());
                    CompilerSessionUpdate {
                        result: Ok(candidate),
                        work: parse_work,
                        invalidation,
                        downstream_invalidated,
                        diagnostics,
                    }
                }
            }
            Err(errors) => CompilerSessionUpdate {
                result: Err(errors),
                work: parse_work,
                invalidation,
                downstream_invalidated: false,
                diagnostics,
            },
        }
    }

    /// Resolve canonical parsed import sites without lowering or semantic work.
    pub fn import_graph(
        &mut self,
        std_dir: Option<&str>,
    ) -> Result<Arc<CanonicalImportGraphOutput>, CompileErrors> {
        self.require_closed_discovery()?;
        self.work.imports.calls += 1;
        if let Some(committed) = self.committed_discovery.as_ref() {
            let graph = committed
                .graph()
                .expect("closed-valid discovery revisions retain their canonical graph");
            if graph.input().std_dir.as_deref() != std_dir {
                return Err(CompileErrors::from(CompileError::without_span(
                    ErrorKind::InvalidCompilerInput(
                        "the requested standard-library context differs from the committed import discovery revision"
                            .into(),
                    ),
                )));
            }
            self.work.imports.reuses += 1;
            return Ok(graph.clone());
        }
        let parsed = self.published.as_deref().ok_or_else(no_published_program)?;
        let resolution = ModuleResolutionInputs::new(
            parsed.root().clone(),
            parsed
                .modules()
                .iter()
                .map(|module| crate::ModuleResolutionInput {
                    module: module.module_id().clone(),
                    physical_path: Arc::from(module.physical_path()),
                })
                .collect(),
        )
        .expect("published parsed modules have validated resolution inputs");
        let input = ImportGraphInputDescriptor {
            sources: parsed.source_revision().clone(),
            resolution,
            std_dir: std_dir.map(Arc::from),
        };
        if let Some(entry) = self.import_cache.iter().find(|entry| entry.input == input) {
            self.work.imports.reuses += 1;
            return entry.result.clone();
        }
        self.work.imports.executions += 1;
        let result =
            resolve_canonical_import_graph(parsed.import_directives(), &input.resolution, std_dir)
                .map(|graph| {
                    let validation = validate_canonical_import_graph(&graph, &input.resolution);
                    Arc::new(CanonicalImportGraphOutput {
                        input: input.clone(),
                        graph,
                        validation,
                    })
                })
                .map_err(CompileErrors::from);
        self.import_cache.push(ImportCacheEntry {
            input,
            result: result.clone(),
        });
        self.work.import_entries = self.import_cache.len();
        result
    }

    pub fn merge(&mut self) -> Result<Arc<CanonicalMergedProgram>, CompileErrors> {
        self.require_closed_discovery()?;
        self.work.merge.calls += 1;
        if let Some(cached) = &self.merge_cache {
            self.work.merge.reuses += 1;
            let cached = cached.clone();
            let source = self
                .published_snapshot
                .clone()
                .expect("published program retains source snapshot");
            self.publish_diagnostics(
                &source,
                FrontendDiagnosticStage::Merge,
                cached.as_ref().err(),
                &[],
            );
            return cached;
        }
        let parsed = self.published.as_deref().ok_or_else(no_published_program)?;
        self.work.merge.executions += 1;
        let merged = if let Some(order) = &self.batch_diagnostic_order {
            crate::canonical_merge::merge_parsed_modules_for_batch(parsed, order)
        } else {
            merge_parsed_modules_reusing_definitions(
                parsed,
                self.definition_shard_baseline.as_ref(),
            )
        }
        .map(Arc::new);
        if let Ok(merged) = &merged {
            debug_assert_eq!(merged.ast().source_revision(), parsed.source_revision());
            self.work.last_merge = merged.work();
            self.definition_shard_baseline = Some(merged.definitions().clone());
        }
        self.merge_cache = Some(merged.clone());
        let source = self
            .published_snapshot
            .clone()
            .expect("published program retains source snapshot");
        self.publish_diagnostics(
            &source,
            FrontendDiagnosticStage::Merge,
            merged.as_ref().err(),
            &[],
        );
        merged
    }

    pub fn rir(&mut self) -> Result<Arc<CanonicalRirOutput>, CompileErrors> {
        self.work.rir.calls += 1;
        if let Some(cached) = &self.rir_cache {
            self.work.rir.reuses += 1;
            return Ok(cached.clone());
        }
        let merged = self.merge()?;
        self.work.rir.executions += 1;
        let rir = Arc::new(lower_canonical_rir(&merged).map_err(CompileErrors::from)?);
        debug_assert_eq!(rir.source_revision(), merged.ast().source_revision());
        self.work.last_rir = rir.work();
        self.rir_cache = Some(rir.clone());
        Ok(rir)
    }

    /// Analyze the current published revision without issuing stable definition IDs.
    pub fn semantic(
        &mut self,
        options: &CompileOptions,
    ) -> Result<Arc<CanonicalSemanticOutput>, CompileErrors> {
        self.work.semantic.calls += 1;
        let rir = self.rir()?;
        let merged = match self.merge_cache.as_ref() {
            Some(Ok(merged)) => merged.clone(),
            Some(Err(errors)) => return Err(errors.clone()),
            None => unreachable!("successful RIR query retains its merge input"),
        };
        let input = CodegenInputDescriptor {
            semantic: SemanticInputDescriptor::new(
                merged.definitions().source_snapshot(),
                options.target,
                &options.preview_features,
            ),
            opt_level: options.opt_level.into(),
        };
        if let Some(entry) = self
            .semantic_cache
            .iter()
            .find(|entry| entry.input == input)
        {
            self.work.semantic.reuses += 1;
            let result = entry.result.clone();
            let source = self
                .published_snapshot
                .clone()
                .expect("semantic query retains source snapshot");
            self.publish_diagnostics(
                &source,
                FrontendDiagnosticStage::Semantic(input),
                result.as_ref().err(),
                result
                    .as_ref()
                    .map(|output| output.warnings())
                    .unwrap_or(&[]),
            );
            return result;
        }

        self.work.semantic.executions += 1;
        let prepared = prepare_canonical_declarations(&merged, &rir, options);
        let current_fingerprints: Result<Vec<StableDefinitionInputFingerprint>, CompileErrors> =
            match &prepared {
                Ok(definitions) => definitions
                    .definitions()
                    .definitions()
                    .iter()
                    .map(|record| {
                        stable_definition_input_fingerprint(
                            merged.definitions().source_snapshot(),
                            record,
                        )
                    })
                    .collect(),
                Err(errors) => Err(errors.clone()),
            };
        let reusable = self.durable_declaration_cache.as_ref().and_then(|cache| {
            self.work.declaration_reuse_plans += 1;
            if cache.root != *merged.ast().root()
                || cache.target != options.target
                || cache.preview_features != options.preview_features
            {
                return None;
            }
            let fingerprints = current_fingerprints.as_ref().ok()?;
            let (matches, compared) = declaration_surfaces_match(&cache.fingerprints, fingerprints);
            self.work.durable_records_compared += compared;
            matches.then(|| cache.semantics.clone())
        });
        let mut cold_durable = None;
        let result = prepared
            .and_then(|prepared| {
                if let Some(durable) = reusable {
                    let definitions = prepared.definitions().clone();
                    analyze_prepared_canonical_program_reusing_declarations(
                        &merged,
                        &rir,
                        options,
                        prepared,
                        &definitions,
                        &durable,
                    )
                } else {
                    analyze_prepared_canonical_program_with_durable_export(
                        &merged, &rir, options, prepared,
                    )
                    .map(|analysis| {
                        cold_durable = analysis
                            .durable_declarations
                            .map(|semantics| (analysis.definitions, semantics));
                        analysis.output
                    })
                }
            })
            .map(Arc::new);
        let semantic_work = result
            .as_ref()
            .map(|output| output.work())
            .unwrap_or_default();
        if let Ok(output) = &result {
            debug_assert_eq!(output.input(), &input);
            debug_assert_eq!(semantic_work.binding.bind_invocations, 1);
            debug_assert_eq!(semantic_work.manifest.build_invocations, 1);
            debug_assert!(!semantic_work.stable_ids_requested);
            let reuse = semantic_work.declaration_reuse;
            self.work.durable_records_reused += reuse.durable_records_reused;
            self.work.ordinary_declaration_resolutions_skipped +=
                reuse.ordinary_declaration_resolutions_skipped;
            self.work.durable_installs += reuse.install_invocations;
            self.work.declaration_reuse_fallbacks += reuse.fallbacks;

            // Populate or refresh the durable baseline only after a successful
            // ordinary execution. Reused executions retain the stable payload
            // and merely advance its exact provenance below.
            if reuse.ordinary_declaration_resolutions_skipped == 0 {
                if let Some((definitions, semantics)) = cold_durable {
                    if let Ok(fingerprints) = definitions
                        .definitions()
                        .iter()
                        .map(|record| {
                            stable_definition_input_fingerprint(
                                merged.definitions().source_snapshot(),
                                record,
                            )
                        })
                        .collect::<Result<Vec<_>, _>>()
                    {
                        self.durable_declaration_cache = Some(DurableDeclarationCache {
                            root: merged.ast().root().clone(),
                            target: options.target,
                            preview_features: options.preview_features.clone(),
                            fingerprints: fingerprints.into(),
                            semantics,
                        });
                    }
                }
            } else if let (Some(cache), Ok(fingerprints)) = (
                self.durable_declaration_cache.as_mut(),
                current_fingerprints,
            ) {
                cache.fingerprints = fingerprints.into();
            }
        }
        self.semantic_cache.push(SemanticCacheEntry {
            input: input.clone(),
            result: result.clone(),
        });
        self.work.semantic_entries = self.semantic_cache.len();
        self.work.semantic_records.push(SemanticQueryRecord {
            input: input.clone(),
            work: semantic_work,
            failed: result.is_err(),
        });
        let source = self
            .published_snapshot
            .clone()
            .expect("semantic query retains source snapshot");
        self.publish_diagnostics(
            &source,
            FrontendDiagnosticStage::Semantic(input),
            result.as_ref().err(),
            result
                .as_ref()
                .map(|output| output.warnings())
                .unwrap_or(&[]),
        );
        result
    }

    /// Issue stable definition IDs on demand for the current semantic input.
    ///
    /// Ordinary analysis consumes `BoundSema` without building its optional
    /// manifest. Retaining that mutable, RIR-borrowing value would duplicate
    /// substantial semantic state, so this query performs one explicit second
    /// declaration bind after reusing a successful ordinary body analysis.
    pub fn stable_definitions(
        &mut self,
        options: &CompileOptions,
    ) -> Result<Arc<BoundDefinitionSet>, CompileErrors> {
        self.work.definitions.calls += 1;
        let rir = self.rir()?;
        let merged = match self.merge_cache.as_ref() {
            Some(Ok(merged)) => merged.clone(),
            Some(Err(errors)) => return Err(errors.clone()),
            None => unreachable!("successful RIR query retains merge input"),
        };
        let input = SemanticInputDescriptor::new(
            merged.definitions().source_snapshot(),
            options.target,
            &options.preview_features,
        );

        // Body validity is independent of opt/linker. Reuse any ordinary
        // semantic result with the same binding inputs before doing ID work.
        if let Some(validation) = self
            .semantic_cache
            .iter()
            .find(|entry| entry.input.semantic == input && entry.result.is_ok())
            .map(|entry| entry.result.clone())
        {
            validation?;
        } else {
            self.semantic(options)?;
        }

        if let Some(entry) = self
            .definition_cache
            .iter()
            .find(|entry| entry.input == input)
        {
            self.work.definitions.reuses += 1;
            return entry.result.clone();
        }
        self.work.definitions.executions += 1;
        let query = bind_canonical_definitions_with_work(
            &merged,
            &rir,
            options.preview_features.clone(),
            options.target,
        );
        let (result, binding, manifest, issuance) = match query {
            Ok((definitions, binding)) => {
                let manifest = definitions.manifest_work();
                let issuance = definitions.work();
                (Ok(Arc::new(definitions)), binding, manifest, issuance)
            }
            Err(errors) => (
                Err(errors),
                DeclarationBindingWork::default(),
                SemanticBindingManifestWork::default(),
                BoundDefinitionWork::default(),
            ),
        };
        self.definition_cache.push(DefinitionCacheEntry {
            input: input.clone(),
            result: result.clone(),
        });
        self.work.definition_entries = self.definition_cache.len();
        self.work.definition_records.push(DefinitionQueryRecord {
            input,
            binding,
            manifest,
            issuance,
            failed: result.is_err(),
        });
        result
    }

    /// Materialize the stable semantic dependency manifest.
    ///
    /// The manifest contains the supported call, destructor, declaration-type,
    /// type-call-head, and named-constant edge families. Per-surface completeness
    /// flags and stable blockers make incomplete capture fail closed. The query
    /// shares the import and stable-definition inputs and performs no additional
    /// RIR traversal.
    pub fn semantic_dependency_inputs(
        &mut self,
        options: &CompileOptions,
        std_dir: Option<&str>,
    ) -> Result<Arc<SemanticDependencyInputManifest>, CompileErrors> {
        self.work.dependency_manifests.calls += 1;
        let imports = self.import_graph(std_dir)?;
        let snapshot = self
            .published_snapshot
            .as_ref()
            .expect("stable definitions retain a published source snapshot")
            .clone();
        let input =
            SemanticInputDescriptor::new(&snapshot, options.target, &options.preview_features);
        if let Some(cached) = self
            .dependency_manifest_cache
            .iter()
            .find(|manifest| manifest.input == input && manifest.imports == *imports.graph())
        {
            self.work.dependency_manifests.reuses += 1;
            return Ok(cached.clone());
        }
        self.work.dependency_manifests.executions += 1;
        let semantic = self.semantic(options);
        let definitions = self.stable_definitions(options);
        let definition_universe_complete = definitions.is_ok();
        let definition_records = definitions
            .as_ref()
            .map(|definitions| definitions.definitions())
            .unwrap_or(&[]);
        let mut keys = definition_records
            .iter()
            .map(|record| record.stable_key().clone())
            .collect::<Vec<_>>();
        keys.sort();
        keys.dedup();
        let definition_fingerprints = definition_records
            .iter()
            .map(|record| stable_definition_input_fingerprint(&snapshot, record))
            .collect::<Result<Vec<_>, _>>()?;
        let (
            mut free_function_dependencies,
            mut named_method_dependencies,
            mut named_destructor_dependencies,
            mut declaration_type_dependencies,
            mut declaration_type_call_head_dependencies,
            mut builtin_type_call_head_inputs,
            mut named_const_dependencies,
            mut implicit_named_destructor_dependencies,
            free_function_events_translated,
            specialization_origins_validated,
            named_method_events_translated,
            named_destructor_events_translated,
            declaration_type_events_translated,
            declaration_type_call_head_events_translated,
            builtin_type_call_head_inputs_translated,
            named_const_events_translated,
            implicit_named_destructor_events_translated,
            free_function_caller_dependencies_complete,
            named_method_dependencies_complete,
            generic_named_method_dependencies_complete,
            named_destructor_dependencies_complete,
            declaration_type_dependencies_complete,
            declaration_type_call_head_dependencies_complete,
            supported_type_call_heads_complete,
            named_value_const_dependencies_complete,
            implicit_named_destructor_dependencies_complete,
        ) = match (&semantic, &definitions) {
            (Ok(semantic), Ok(definitions)) => {
                if definitions.source_revision() != &input.sources {
                    return Err(invalid_dependency_manifest(
                        "semantic dependency translation used a foreign definition revision",
                    ));
                }
                if semantic.body_owner_issuer().source_revision() != &input.sources {
                    return Err(invalid_dependency_manifest(
                        "semantic dependency translation used a stale body-owner issuer revision",
                    ));
                }
                let mut edges = Vec::new();
                for origin in semantic.specialized_free_function_origins() {
                    stable_free_function_endpoint(
                        definitions,
                        origin.base_file,
                        &origin.base_name,
                    )?;
                }
                for event in semantic.ordinary_free_function_dependencies() {
                    let provenance = stable_free_function_endpoint(
                        definitions,
                        event.caller_file,
                        &event.caller_name,
                    )?;
                    edges.push(StableFreeFunctionDependency {
                        caller: stable_token_endpoint(semantic, event.caller_token, &provenance)?,
                        callee: stable_free_function_endpoint(
                            definitions,
                            event.callee_file,
                            &event.callee_name,
                        )?,
                    });
                }
                for event in semantic.specialized_free_function_dependencies() {
                    edges.push(StableFreeFunctionDependency {
                        caller: stable_free_function_endpoint(
                            definitions,
                            event.base_file,
                            &event.base_name,
                        )?,
                        callee: stable_free_function_endpoint(
                            definitions,
                            event.callee_file,
                            &event.callee_name,
                        )?,
                    });
                }
                let mut method_edges = Vec::new();
                for event in semantic.named_method_dependencies() {
                    let provenance = stable_named_method_endpoint(
                        definitions,
                        event.caller_file,
                        &event.caller_owner_name,
                        &event.caller_method_name,
                    )?;
                    let caller = stable_token_endpoint(semantic, event.caller_token, &provenance)?;
                    let target = match &event.target {
                        rue_air::NamedMethodDependencyTargetEvent::FreeFunction { file, name } => {
                            StableNamedMethodDependencyTarget::FreeFunction(
                                stable_free_function_endpoint(definitions, *file, name)?,
                            )
                        }
                        rue_air::NamedMethodDependencyTargetEvent::NamedMethod {
                            file,
                            owner_name,
                            method_name,
                        } => StableNamedMethodDependencyTarget::NamedMethod(
                            stable_named_method_endpoint(
                                definitions,
                                *file,
                                owner_name,
                                method_name,
                            )?,
                        ),
                    };
                    method_edges.push(StableNamedMethodDependency { caller, target });
                }
                let mut destructor_edges = Vec::new();
                for event in semantic.named_destructor_dependencies() {
                    let provenance = stable_named_destructor_endpoint(
                        definitions,
                        event.caller_file,
                        &event.caller_owner_name,
                    )?;
                    let caller = stable_token_endpoint(semantic, event.caller_token, &provenance)?;
                    let target = match &event.target {
                        rue_air::NamedMethodDependencyTargetEvent::FreeFunction { file, name } => {
                            StableNamedMethodDependencyTarget::FreeFunction(
                                stable_free_function_endpoint(definitions, *file, name)?,
                            )
                        }
                        rue_air::NamedMethodDependencyTargetEvent::NamedMethod {
                            file,
                            owner_name,
                            method_name,
                        } => StableNamedMethodDependencyTarget::NamedMethod(
                            stable_named_method_endpoint(
                                definitions,
                                *file,
                                owner_name,
                                method_name,
                            )?,
                        ),
                    };
                    destructor_edges.push(StableNamedDestructorDependency { caller, target });
                }
                let mut type_edges = Vec::new();
                for event in semantic.declaration_type_dependencies() {
                    let provenance = stable_declaration_source_endpoint(definitions, event)?;
                    type_edges.push(StableDeclarationTypeDependency {
                        source: match event.source_token {
                            Some(token) => stable_token_endpoint(semantic, token, &provenance)?,
                            None => provenance,
                        },
                        target: stable_named_type_endpoint(definitions, event)?,
                        kind: event.dependency_kind,
                    });
                }
                let mut type_call_head_edges = Vec::new();
                for event in semantic.declaration_type_call_head_dependencies() {
                    let provenance = stable_declaration_type_source_endpoint(
                        definitions,
                        event.source_file,
                        &event.source_name,
                        event.source_owner_name.as_deref(),
                        event.source_kind,
                    )?;
                    type_call_head_edges.push(StableDeclarationTypeCallHeadDependency {
                        source: match event.source_token {
                            Some(token) => stable_token_endpoint(semantic, token, &provenance)?,
                            None => provenance,
                        },
                        callable: stable_free_function_endpoint(
                            definitions,
                            event.callable_file,
                            &event.callable_name,
                        )?,
                        kind: event.dependency_kind,
                    });
                }
                let mut builtin_head_inputs = Vec::new();
                for event in semantic.declaration_builtin_type_call_head_dependencies() {
                    let provenance = stable_declaration_type_source_endpoint(
                        definitions,
                        event.source_file,
                        &event.source_name,
                        event.source_owner_name.as_deref(),
                        event.source_kind,
                    )?;
                    builtin_head_inputs.push(StableBuiltinTypeCallHeadInput {
                        source: match event.source_token {
                            Some(token) => stable_token_endpoint(semantic, token, &provenance)?,
                            None => provenance,
                        },
                        builtin: event.builtin,
                        kind: event.dependency_kind,
                    });
                }
                let mut const_edges = Vec::new();
                for event in semantic.named_const_dependencies() {
                    let source = stable_top_level_endpoint(
                        definitions,
                        event.source_file,
                        &event.source_name,
                        StableDefinitionNamespace::Value,
                        StableDefinitionKind::ValueConst,
                    )?;
                    let target = match &event.target {
                        rue_air::NamedConstDependencyTargetEvent::ValueConst { file, name } => {
                            StableNamedConstDependencyTarget::ValueConst(stable_top_level_endpoint(
                                definitions,
                                *file,
                                name,
                                StableDefinitionNamespace::Value,
                                StableDefinitionKind::ValueConst,
                            )?)
                        }
                        rue_air::NamedConstDependencyTargetEvent::FreeFunction { file, name } => {
                            StableNamedConstDependencyTarget::FreeFunction(
                                stable_free_function_endpoint(definitions, *file, name)?,
                            )
                        }
                        rue_air::NamedConstDependencyTargetEvent::NamedType {
                            file,
                            name,
                            kind,
                        } => {
                            let kind = match kind {
                                rue_air::DeclarationTypeDependencyTargetKind::Struct => {
                                    StableDefinitionKind::Struct
                                }
                                rue_air::DeclarationTypeDependencyTargetKind::Enum => {
                                    StableDefinitionKind::Enum
                                }
                                rue_air::DeclarationTypeDependencyTargetKind::ValueConst => {
                                    StableDefinitionKind::ValueConst
                                }
                            };
                            let namespace = if matches!(kind, StableDefinitionKind::ValueConst) {
                                StableDefinitionNamespace::Value
                            } else {
                                StableDefinitionNamespace::Type
                            };
                            StableNamedConstDependencyTarget::NamedType(stable_top_level_endpoint(
                                definitions,
                                *file,
                                name,
                                namespace,
                                kind,
                            )?)
                        }
                        rue_air::NamedConstDependencyTargetEvent::ModuleBinding { file, name } => {
                            StableNamedConstDependencyTarget::ModuleBinding(
                                stable_top_level_endpoint(
                                    definitions,
                                    *file,
                                    name,
                                    StableDefinitionNamespace::Value,
                                    StableDefinitionKind::ModuleBinding,
                                )?,
                            )
                        }
                    };
                    const_edges.push(StableNamedConstDependency { source, target });
                }
                let mut implicit_destructor_edges = Vec::new();
                for event in semantic.implicit_named_destructor_dependencies() {
                    implicit_destructor_edges.push(StableImplicitNamedDestructorDependency {
                        source: stable_implicit_drop_source_endpoint(
                            semantic,
                            definitions,
                            &event.source,
                        )?,
                        target: stable_named_destructor_endpoint(
                            definitions,
                            event.target_file,
                            &event.target_owner_name,
                        )?,
                    });
                }
                (
                    edges,
                    method_edges,
                    destructor_edges,
                    type_edges,
                    type_call_head_edges,
                    builtin_head_inputs,
                    const_edges,
                    implicit_destructor_edges,
                    semantic.ordinary_free_function_dependencies().len()
                        + semantic.specialized_free_function_dependencies().len(),
                    semantic.specialized_free_function_origins().len(),
                    semantic.named_method_dependencies().len(),
                    semantic.named_destructor_dependencies().len(),
                    semantic.declaration_type_dependencies().len(),
                    semantic.declaration_type_call_head_dependencies().len(),
                    semantic
                        .declaration_builtin_type_call_head_dependencies()
                        .len(),
                    semantic.named_const_dependencies().len(),
                    semantic.implicit_named_destructor_dependencies().len(),
                    semantic.ordinary_free_function_dependencies_complete()
                        && semantic.specialized_free_function_dependencies_complete(),
                    semantic.non_generic_named_method_dependencies_complete(),
                    semantic.generic_named_method_dependencies_complete(),
                    semantic.named_destructor_dependencies_complete(),
                    semantic.declaration_type_dependencies_complete(),
                    semantic.declaration_type_call_head_dependencies_complete(),
                    semantic.supported_type_call_heads_complete(),
                    semantic.named_value_const_dependencies_complete(),
                    semantic.implicit_named_destructor_dependencies_complete(),
                )
            }
            _ => (
                Vec::new(),
                Vec::new(),
                Vec::new(),
                Vec::new(),
                Vec::new(),
                Vec::new(),
                Vec::new(),
                Vec::new(),
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                false,
                false,
                false,
                false,
                false,
                false,
                false,
                false,
                false,
            ),
        };
        let (mut analyzed_body_owners, anonymous_body_owners) = match (&semantic, &definitions) {
            (Ok(semantic), Ok(definitions)) => {
                let mut owners = Vec::new();
                let mut anonymous = 0usize;
                for event in semantic.analyzed_body_owners() {
                    let owner = match stable_body_owner_endpoint(semantic, definitions, event)? {
                        Some(owner) => Some(owner),
                        None => {
                            anonymous += 1;
                            None
                        }
                    };
                    if let Some(owner) = owner {
                        owners.push(owner);
                    }
                }
                (owners, anonymous)
            }
            _ => (Vec::new(), 0),
        };
        analyzed_body_owners.sort();
        analyzed_body_owners.dedup();
        let mut body_named_dependencies = Vec::new();
        if let (Ok(semantic), Ok(definitions)) = (&semantic, &definitions) {
            for event in semantic.body_named_dependencies() {
                let Some((source, _)) =
                    stable_body_owner_endpoint(semantic, definitions, &event.source)?
                else {
                    continue;
                };
                let target = match &event.target {
                    rue_air::NamedConstDependencyTargetEvent::ValueConst { file, name } => {
                        stable_top_level_endpoint(
                            definitions,
                            *file,
                            name,
                            StableDefinitionNamespace::Value,
                            StableDefinitionKind::ValueConst,
                        )?
                    }
                    rue_air::NamedConstDependencyTargetEvent::ModuleBinding { file, name } => {
                        stable_top_level_endpoint(
                            definitions,
                            *file,
                            name,
                            StableDefinitionNamespace::Value,
                            StableDefinitionKind::ModuleBinding,
                        )?
                    }
                    // Body observers currently emit only value/module choices.
                    // Keep all other variants fail-closed if that contract changes.
                    _ => {
                        return Err(invalid_dependency_manifest(
                            "unsupported body-local named dependency target",
                        ));
                    }
                };
                body_named_dependencies.push((source, target));
            }
        }
        body_named_dependencies.sort();
        body_named_dependencies.dedup();
        free_function_dependencies.sort();
        free_function_dependencies.dedup();
        named_method_dependencies.sort();
        named_method_dependencies.dedup();
        named_destructor_dependencies.sort();
        named_destructor_dependencies.dedup();
        declaration_type_dependencies.sort();
        declaration_type_dependencies.dedup();
        declaration_type_call_head_dependencies.sort();
        declaration_type_call_head_dependencies.dedup();
        builtin_type_call_head_inputs.sort();
        builtin_type_call_head_inputs.dedup();
        named_const_dependencies.sort();
        named_const_dependencies.dedup();
        implicit_named_destructor_dependencies.sort();
        implicit_named_destructor_dependencies.dedup();
        // A per-body record cannot authorize reuse when an observer-backed
        // dependency surface for this semantic execution is incomplete. The
        // current completeness evidence is whole-graph rather than per-owner,
        // so conservatively retain its ownerless blockers on every record.
        let mut whole_graph_body_blockers = BTreeSet::new();
        let mut block_body_surface =
            |complete: bool,
             surface: SemanticDependencySurface,
             reason: SemanticDependencyIncompleteReason| {
                if !complete {
                    whole_graph_body_blockers.insert(SemanticDependencyBlocker {
                        owner: None,
                        surface,
                        reason,
                    });
                }
            };
        block_body_surface(
            free_function_caller_dependencies_complete,
            SemanticDependencySurface::FreeFunctionCall,
            SemanticDependencyIncompleteReason::CallerEndpointUnavailable,
        );
        block_body_surface(
            named_method_dependencies_complete,
            SemanticDependencySurface::NonGenericNamedMethodCall,
            SemanticDependencyIncompleteReason::CallerEndpointUnavailable,
        );
        block_body_surface(
            generic_named_method_dependencies_complete,
            SemanticDependencySurface::GenericNamedMethodCall,
            SemanticDependencyIncompleteReason::GenericSubstitutionIdentityUnavailable,
        );
        block_body_surface(
            named_destructor_dependencies_complete,
            SemanticDependencySurface::NamedDestructorCall,
            SemanticDependencyIncompleteReason::DestructorEndpointUnavailable,
        );
        block_body_surface(
            implicit_named_destructor_dependencies_complete,
            SemanticDependencySurface::ImplicitNamedDestructor,
            SemanticDependencyIncompleteReason::AnonymousDropOwnerUnavailable,
        );
        block_body_surface(
            declaration_type_dependencies_complete,
            SemanticDependencySurface::DeclarationType,
            SemanticDependencyIncompleteReason::ResolvedTypeIdentityUnavailable,
        );
        block_body_surface(
            declaration_type_call_head_dependencies_complete,
            SemanticDependencySurface::DeclarationTypeCallHead,
            SemanticDependencyIncompleteReason::TypeCallHeadIdentityUnavailable,
        );
        block_body_surface(
            supported_type_call_heads_complete,
            SemanticDependencySurface::SupportedTypeCallHead,
            SemanticDependencyIncompleteReason::UnsupportedDynamicTypeCallHead,
        );
        block_body_surface(
            named_value_const_dependencies_complete,
            SemanticDependencySurface::NamedValueConst,
            SemanticDependencyIncompleteReason::ConstEndpointUnavailable,
        );
        let mut body_dependencies = Vec::new();
        for (owner, generic) in &analyzed_body_owners {
            let fingerprint = definition_fingerprints
                .iter()
                .find(|fingerprint| &fingerprint.key == owner)
                .cloned()
                .ok_or_else(|| {
                    invalid_dependency_manifest(
                        "analyzed body owner is absent from definition fingerprints",
                    )
                })?;
            let mut direct_dependencies = Vec::new();
            direct_dependencies.extend(
                free_function_dependencies
                    .iter()
                    .filter(|edge| &edge.caller == owner)
                    .map(|edge| edge.callee.clone()),
            );
            for edge in named_method_dependencies
                .iter()
                .filter(|edge| &edge.caller == owner)
            {
                direct_dependencies.push(match &edge.target {
                    StableNamedMethodDependencyTarget::FreeFunction(target)
                    | StableNamedMethodDependencyTarget::NamedMethod(target) => target.clone(),
                });
            }
            for edge in named_destructor_dependencies
                .iter()
                .filter(|edge| &edge.caller == owner)
            {
                direct_dependencies.push(match &edge.target {
                    StableNamedMethodDependencyTarget::FreeFunction(target)
                    | StableNamedMethodDependencyTarget::NamedMethod(target) => target.clone(),
                });
            }
            direct_dependencies.extend(
                implicit_named_destructor_dependencies
                    .iter()
                    .filter(|edge| &edge.source == owner)
                    .map(|edge| edge.target.clone()),
            );
            direct_dependencies.extend(
                declaration_type_dependencies
                    .iter()
                    .filter(|edge| &edge.source == owner)
                    .map(|edge| edge.target.clone()),
            );
            direct_dependencies.extend(
                declaration_type_call_head_dependencies
                    .iter()
                    .filter(|edge| &edge.source == owner)
                    .map(|edge| edge.callable.clone()),
            );
            direct_dependencies.extend(
                body_named_dependencies
                    .iter()
                    .filter(|(source, _)| source == owner)
                    .map(|(_, target)| target.clone()),
            );
            direct_dependencies.sort();
            direct_dependencies.dedup();
            let direct_dependency_inputs = direct_dependencies
                .into_iter()
                .map(|dependency| {
                    definition_fingerprints
                        .iter()
                        .find(|fingerprint| fingerprint.key == dependency)
                        .cloned()
                        .ok_or_else(|| {
                            invalid_dependency_manifest(
                                "body dependency is absent from definition fingerprints",
                            )
                        })
                })
                .collect::<Result<Vec<_>, _>>()?;
            let builtin_inputs = builtin_type_call_head_inputs
                .iter()
                .filter(|input| &input.source == owner)
                .cloned()
                .collect::<Vec<_>>();
            let mut blockers = whole_graph_body_blockers
                .iter()
                .cloned()
                .collect::<Vec<_>>();
            if *generic {
                blockers.push(SemanticDependencyBlocker {
                    owner: Some(owner.clone()),
                    surface: SemanticDependencySurface::GenericNamedMethodCall,
                    reason:
                        SemanticDependencyIncompleteReason::GenericSubstitutionIdentityUnavailable,
                });
            }
            blockers.sort();
            blockers.dedup();
            body_dependencies.push(StableBodyDependencyInputRecord {
                owner: owner.clone(),
                fingerprint,
                target: input.target,
                preview_features: input.preview_features.clone(),
                direct_dependency_inputs: direct_dependency_inputs.into(),
                builtin_type_call_heads: builtin_inputs.into(),
                blockers: blockers.into(),
            });
        }
        body_dependencies.sort_by(|left, right| left.owner.cmp(&right.owner));
        let mut body_dependency_blockers = body_dependencies
            .iter()
            .flat_map(|record| record.blockers.iter().cloned())
            .collect::<Vec<_>>();
        if anonymous_body_owners != 0 {
            body_dependency_blockers.push(SemanticDependencyBlocker {
                owner: None,
                surface: SemanticDependencySurface::BodyOwner,
                reason: SemanticDependencyIncompleteReason::AnonymousBodyOwnerUnavailable,
            });
        }
        body_dependency_blockers.sort();
        body_dependency_blockers.dedup();
        let mut durable_body_work = crate::DurableBodyWork::default();
        let durable_ordinary_bodies = match &semantic {
            Ok(semantic) => match crate::finalize_durable_ordinary_bodies(
                semantic.durable_ordinary_body_payloads(),
                &body_dependencies,
                &mut durable_body_work,
            ) {
                Ok(candidates) if candidates.is_empty() => candidates,
                Ok(candidates) => match self
                    .durable_declaration_cache
                    .as_ref()
                    .map(|cache| cache.semantics.clone())
                {
                    None => {
                        durable_body_work.atomic_discards += 1;
                        Arc::from([])
                    }
                    Some(declarations) => {
                        match crate::import_durable_declaration_semantics(&declarations) {
                            Ok(epoch) => {
                                let mut installed_instructions = 0;
                                let mut installed_places = 0;
                                let mut installed_strings = 0;
                                let mut failed = false;
                                for candidate in candidates.iter() {
                                    let dto = match candidate
                                        .project_semantic_body(&mut durable_body_work)
                                    {
                                        Ok(dto) => dto,
                                        Err(_) => {
                                            durable_body_work.atomic_discards += 1;
                                            failed = true;
                                            break;
                                        }
                                    };
                                    let owner_records = definition_records
                                        .iter()
                                        .filter(|record| record.stable_key() == candidate.owner())
                                        .collect::<Vec<_>>();
                                    let [owner_record] = owner_records.as_slice() else {
                                        durable_body_work.import_failures += 1;
                                        durable_body_work.atomic_discards += 1;
                                        failed = true;
                                        break;
                                    };
                                    let Some(body_span) = owner_record.body_span() else {
                                        durable_body_work.import_failures += 1;
                                        durable_body_work.atomic_discards += 1;
                                        failed = true;
                                        break;
                                    };
                                    durable_body_work.import_attempts += 1;
                                    match epoch.import_body(&dto, body_span) {
                                        Ok(imported) => {
                                            durable_body_work.import_successes += 1;
                                            installed_instructions += imported.air.len();
                                            installed_places += imported.air.places().len();
                                            installed_strings += imported.strings.len();
                                        }
                                        Err(_) => {
                                            durable_body_work.import_failures += 1;
                                            durable_body_work.atomic_discards += 1;
                                            failed = true;
                                            break;
                                        }
                                    }
                                }
                                if failed {
                                    durable_body_work.installed_instructions +=
                                        installed_instructions;
                                    durable_body_work.installed_places += installed_places;
                                    durable_body_work.installed_strings += installed_strings;
                                    Arc::from([])
                                } else {
                                    durable_body_work.installed_instructions +=
                                        installed_instructions;
                                    durable_body_work.installed_places += installed_places;
                                    durable_body_work.installed_strings += installed_strings;
                                    candidates
                                }
                            }
                            Err(_) => {
                                durable_body_work.import_failures += 1;
                                durable_body_work.atomic_discards += 1;
                                Arc::from([])
                            }
                        }
                    }
                },
                Err(_) => Arc::from([]),
            },
            Err(_) => Arc::from([]),
        };
        let work = SemanticDependencyManifestWork {
            definition_records_visited: definition_records.len(),
            import_records_visited: imports.graph().records().len(),
            free_function_events_translated,
            specialization_origins_validated,
            named_method_events_translated,
            named_destructor_events_translated,
            declaration_type_events_translated,
            declaration_type_call_head_events_translated,
            builtin_type_call_head_inputs_translated,
            named_const_events_translated,
            implicit_named_destructor_events_translated,
            body_owner_events_translated: analyzed_body_owners.len() + anonymous_body_owners,
            body_named_events_translated: body_named_dependencies.len(),
            body_dependency_records_built: body_dependencies.len(),
            durable_bodies: durable_body_work,
            extra_rir_instructions_visited: 0,
        };
        self.work.dependency_manifest_records_visited += work.definition_records_visited;
        self.work.dependency_manifest_import_records_visited += work.import_records_visited;
        let module_imports = imports
            .graph()
            .records()
            .iter()
            .map(|record| match record.resolution() {
                CanonicalImportResolution::Resolved(target) => {
                    StableModuleImportDependency::Resolved {
                        importer: record.importer().clone(),
                        normalized_specifier: Arc::from(record.normalized_specifier()),
                        target: target.clone(),
                    }
                }
                CanonicalImportResolution::Missing => StableModuleImportDependency::Missing {
                    importer: record.importer().clone(),
                    normalized_specifier: Arc::from(record.normalized_specifier()),
                },
                CanonicalImportResolution::Ambiguous {
                    file_module,
                    directory_module,
                } => StableModuleImportDependency::Ambiguous {
                    importer: record.importer().clone(),
                    normalized_specifier: Arc::from(record.normalized_specifier()),
                    file_module: file_module.clone(),
                    directory_module: directory_module.clone(),
                },
            })
            .collect::<Vec<_>>();
        let mut dependency_blockers = whole_graph_body_blockers;
        let mut block = |complete: bool,
                         surface: SemanticDependencySurface,
                         reason: SemanticDependencyIncompleteReason| {
            if !complete {
                dependency_blockers.insert(SemanticDependencyBlocker {
                    owner: None,
                    surface,
                    reason,
                });
            }
        };
        block(
            free_function_caller_dependencies_complete,
            SemanticDependencySurface::FreeFunctionCall,
            SemanticDependencyIncompleteReason::CallerEndpointUnavailable,
        );
        block(
            named_method_dependencies_complete,
            SemanticDependencySurface::NonGenericNamedMethodCall,
            SemanticDependencyIncompleteReason::CallerEndpointUnavailable,
        );
        block(
            generic_named_method_dependencies_complete,
            SemanticDependencySurface::GenericNamedMethodCall,
            SemanticDependencyIncompleteReason::GenericSubstitutionIdentityUnavailable,
        );
        block(
            named_destructor_dependencies_complete,
            SemanticDependencySurface::NamedDestructorCall,
            SemanticDependencyIncompleteReason::DestructorEndpointUnavailable,
        );
        block(
            implicit_named_destructor_dependencies_complete,
            SemanticDependencySurface::ImplicitNamedDestructor,
            SemanticDependencyIncompleteReason::AnonymousDropOwnerUnavailable,
        );
        block(
            declaration_type_dependencies_complete,
            SemanticDependencySurface::DeclarationType,
            SemanticDependencyIncompleteReason::ResolvedTypeIdentityUnavailable,
        );
        block(
            declaration_type_call_head_dependencies_complete,
            SemanticDependencySurface::DeclarationTypeCallHead,
            SemanticDependencyIncompleteReason::TypeCallHeadIdentityUnavailable,
        );
        block(
            supported_type_call_heads_complete,
            SemanticDependencySurface::SupportedTypeCallHead,
            SemanticDependencyIncompleteReason::UnsupportedDynamicTypeCallHead,
        );
        block(
            named_value_const_dependencies_complete,
            SemanticDependencySurface::NamedValueConst,
            SemanticDependencyIncompleteReason::ConstEndpointUnavailable,
        );
        let manifest = Arc::new(SemanticDependencyInputManifest {
            input,
            imports: imports.graph().clone(),
            definitions: keys.into(),
            definition_fingerprints: definition_fingerprints.into(),
            module_imports: module_imports.into(),
            free_function_dependencies: free_function_dependencies.into(),
            named_method_dependencies: named_method_dependencies.into(),
            named_destructor_dependencies: named_destructor_dependencies.into(),
            implicit_named_destructor_dependencies: implicit_named_destructor_dependencies.into(),
            declaration_type_dependencies: declaration_type_dependencies.into(),
            declaration_type_call_head_dependencies: declaration_type_call_head_dependencies.into(),
            builtin_type_call_head_inputs: builtin_type_call_head_inputs.into(),
            named_const_dependencies: named_const_dependencies.into(),
            body_dependencies: body_dependencies.into(),
            durable_ordinary_bodies,
            body_dependency_blockers: body_dependency_blockers.into(),
            dependency_blockers: dependency_blockers.into_iter().collect::<Vec<_>>().into(),
            definition_universe_complete,
            work,
        });
        self.dependency_manifest_cache.push(manifest.clone());
        self.refresh_retention_metrics();
        Ok(manifest)
    }

    /// Compare two immutable semantic manifests without lowering or scanning RIR.
    ///
    /// Supported production manifests with complete dependency capture can produce
    /// an incremental invalidation plan. Unsupported dependency surfaces, incomplete
    /// capture, and global semantic-input changes fail closed to full invalidation.
    pub fn semantic_invalidation_plan(
        &mut self,
        previous: &Arc<SemanticDependencyInputManifest>,
        current: &Arc<SemanticDependencyInputManifest>,
    ) -> Arc<SemanticInvalidationPlan> {
        self.work.invalidation_plans.calls += 1;
        if let Some(entry) = self.invalidation_plan_cache.iter().find(|entry| {
            Arc::ptr_eq(&entry.previous, previous) && Arc::ptr_eq(&entry.current, current)
        }) {
            self.work.invalidation_plans.reuses += 1;
            return entry.plan.clone();
        }
        self.work.invalidation_plans.executions += 1;
        let plan = Arc::new(plan_semantic_invalidation(previous, current));
        self.invalidation_plan_cache
            .push_back(InvalidationPlanCacheEntry {
                previous: previous.clone(),
                current: current.clone(),
                plan: plan.clone(),
            });
        while self.invalidation_plan_cache.len() > FRONTEND_INVALIDATION_PLAN_RETENTION_LIMIT {
            self.invalidation_plan_cache.pop_front();
        }
        self.refresh_retention_metrics();
        plan
    }
}

fn plan_semantic_invalidation(
    previous: &SemanticDependencyInputManifest,
    current: &SemanticDependencyInputManifest,
) -> SemanticInvalidationPlan {
    let previous_fingerprints = previous
        .definition_fingerprints
        .iter()
        .map(|entry| (entry.key.clone(), entry))
        .collect::<BTreeMap<_, _>>();
    let current_fingerprints = current
        .definition_fingerprints
        .iter()
        .map(|entry| (entry.key.clone(), entry))
        .collect::<BTreeMap<_, _>>();
    let mut work = SemanticInvalidationWork::default();
    let mut added = BTreeSet::new();
    let mut removed = BTreeSet::new();
    let mut changed = BTreeSet::new();
    for (key, fingerprint) in &current_fingerprints {
        match previous_fingerprints.get(key) {
            None => {
                added.insert(key.clone());
            }
            Some(previous) => {
                work.definition_fingerprints_compared += 1;
                if *previous != *fingerprint {
                    changed.insert(key.clone());
                }
            }
        }
    }
    for key in previous_fingerprints.keys() {
        if !current_fingerprints.contains_key(key) {
            removed.insert(key.clone());
        }
    }

    let mut reasons = BTreeSet::new();
    if previous.input.sources.root() != current.input.sources.root() {
        reasons.insert(SemanticFullInvalidationReason::RootChanged);
    }
    if previous.module_imports != current.module_imports {
        reasons.insert(SemanticFullInvalidationReason::ModuleImportsChanged);
    }
    if previous.input.target != current.input.target {
        reasons.insert(SemanticFullInvalidationReason::TargetChanged);
    }
    if previous.input.preview_features != current.input.preview_features {
        reasons.insert(SemanticFullInvalidationReason::PreviewFeaturesChanged);
    }
    if !previous.definition_universe_complete || !current.definition_universe_complete {
        reasons.insert(SemanticFullInvalidationReason::IncompleteDefinitionUniverse);
    }
    let dependency_blockers = previous
        .dependency_blockers
        .iter()
        .chain(current.dependency_blockers.iter())
        .cloned()
        .collect::<BTreeSet<_>>();
    if !dependency_blockers.is_empty() {
        reasons.insert(SemanticFullInvalidationReason::IncompleteDependencyGraph(
            dependency_blockers.into_iter().collect::<Vec<_>>().into(),
        ));
    }

    let mut invalidated = BTreeSet::new();
    let mut reusable = BTreeSet::new();
    let scope = if reasons.is_empty() {
        invalidated.extend(added.iter().cloned());
        invalidated.extend(removed.iter().cloned());
        invalidated.extend(changed.iter().cloned());
        let mut reverse = BTreeMap::<StableDefinitionKey, BTreeSet<StableDefinitionKey>>::new();
        collect_reverse_dependencies(previous, &mut reverse, &mut work);
        collect_reverse_dependencies(current, &mut reverse, &mut work);
        let mut queue = invalidated.iter().cloned().collect::<VecDeque<_>>();
        while let Some(key) = queue.pop_front() {
            work.reverse_closure_nodes_visited += 1;
            if let Some(dependents) = reverse.get(&key) {
                for dependent in dependents {
                    if invalidated.insert(dependent.clone()) {
                        queue.push_back(dependent.clone());
                    }
                }
            }
        }
        reusable.extend(
            current_fingerprints
                .keys()
                .filter(|key| !invalidated.contains(*key))
                .cloned(),
        );
        SemanticInvalidationScope::Incremental
    } else {
        SemanticInvalidationScope::Full {
            reasons: reasons.into_iter().collect::<Vec<_>>().into(),
        }
    };
    SemanticInvalidationPlan {
        scope,
        added: added.into_iter().collect::<Vec<_>>().into(),
        removed: removed.into_iter().collect::<Vec<_>>().into(),
        changed: changed.into_iter().collect::<Vec<_>>().into(),
        invalidated: invalidated.into_iter().collect::<Vec<_>>().into(),
        reusable: reusable.into_iter().collect::<Vec<_>>().into(),
        work,
    }
}

fn collect_reverse_dependencies(
    manifest: &SemanticDependencyInputManifest,
    reverse: &mut BTreeMap<StableDefinitionKey, BTreeSet<StableDefinitionKey>>,
    work: &mut SemanticInvalidationWork,
) {
    let mut add = |source: &StableDefinitionKey, target: &StableDefinitionKey| {
        work.dependency_edges_visited += 1;
        reverse
            .entry(target.clone())
            .or_default()
            .insert(source.clone());
    };
    for edge in manifest.free_function_dependencies.iter() {
        add(&edge.caller, &edge.callee);
    }
    for edge in manifest.named_method_dependencies.iter() {
        let target = match &edge.target {
            StableNamedMethodDependencyTarget::FreeFunction(key)
            | StableNamedMethodDependencyTarget::NamedMethod(key) => key,
        };
        add(&edge.caller, target);
    }
    for edge in manifest.named_destructor_dependencies.iter() {
        let target = match &edge.target {
            StableNamedMethodDependencyTarget::FreeFunction(key)
            | StableNamedMethodDependencyTarget::NamedMethod(key) => key,
        };
        add(&edge.caller, target);
    }
    for edge in manifest.implicit_named_destructor_dependencies.iter() {
        add(&edge.source, &edge.target);
    }
    for edge in manifest.declaration_type_dependencies.iter() {
        add(&edge.source, &edge.target);
    }
    for edge in manifest.declaration_type_call_head_dependencies.iter() {
        add(&edge.source, &edge.callable);
    }
    for edge in manifest.named_const_dependencies.iter() {
        let target = match &edge.target {
            StableNamedConstDependencyTarget::ValueConst(key)
            | StableNamedConstDependencyTarget::FreeFunction(key)
            | StableNamedConstDependencyTarget::NamedType(key)
            | StableNamedConstDependencyTarget::ModuleBinding(key) => key,
        };
        add(&edge.source, target);
    }
}

const DEFINITION_FINGERPRINT_SCHEMA_V2: u16 = 2;
const DEFINITION_DECLARATION_DOMAIN_V2: &[u8] = b"rue.definition.declaration\0v2\0sha256\0";
const DEFINITION_SIGNATURE_DOMAIN_V2: &[u8] = b"rue.definition.signature\0v2\0sha256\0";
const DEFINITION_BODY_DOMAIN_V2: &[u8] = b"rue.definition.body-or-initializer\0v2\0sha256\0";

pub(crate) fn stable_definition_input_fingerprint(
    snapshot: &SourceSnapshot,
    record: &crate::BoundDefinitionRecord,
) -> Result<StableDefinitionInputFingerprint, CompileErrors> {
    let source_fragment = |span: Span| -> Result<&str, CompileErrors> {
        let source = snapshot.source_text(span.file_id).ok_or_else(|| {
            invalid_dependency_manifest("definition fingerprint span references an absent source")
        })?;
        let start = usize::try_from(span.start).map_err(|_| {
            invalid_dependency_manifest(
                "definition fingerprint span start cannot address this host",
            )
        })?;
        let end = usize::try_from(span.end).map_err(|_| {
            invalid_dependency_manifest("definition fingerprint span end cannot address this host")
        })?;
        source.get(start..end).ok_or_else(|| {
            invalid_dependency_manifest(
                "definition fingerprint span is reversed, out of bounds, or not on UTF-8 boundaries",
            )
        })
    };

    let mut declaration = FramedDefinitionHasher::new(DEFINITION_DECLARATION_DOMAIN_V2);
    hash_stable_definition_key(&mut declaration, record.stable_key());
    declaration.frame(&[match record.visibility() {
        None => 0,
        Some(rue_parser::ast::Visibility::Private) => 1,
        Some(rue_parser::ast::Visibility::Public) => 2,
    }]);
    let (signature_spans, payload_span, precision) = match record.input_partition() {
        crate::bound_definitions::BoundDefinitionInputPartition::Body { signature, body } => (
            vec![signature],
            Some(body),
            StableDefinitionFingerprintPrecision::SignatureAndBody,
        ),
        crate::bound_definitions::BoundDefinitionInputPartition::Initializer {
            signature,
            initializer,
        } => (
            vec![signature],
            Some(initializer),
            StableDefinitionFingerprintPrecision::SignatureAndInitializer,
        ),
        crate::bound_definitions::BoundDefinitionInputPartition::ExactSignature(spans) => (
            spans.to_vec(),
            None,
            StableDefinitionFingerprintPrecision::ExactSignature,
        ),
    };
    let mut signature = FramedDefinitionHasher::new(DEFINITION_SIGNATURE_DOMAIN_V2);
    for span in signature_spans {
        signature.frame(source_fragment(span)?.as_bytes());
    }
    let body_or_initializer = payload_span
        .map(|span| {
            let mut payload = FramedDefinitionHasher::new(DEFINITION_BODY_DOMAIN_V2);
            payload.frame(source_fragment(span)?.as_bytes());
            Ok::<_, CompileErrors>(payload.finish())
        })
        .transpose()?;
    Ok(StableDefinitionInputFingerprint {
        schema_version: DEFINITION_FINGERPRINT_SCHEMA_V2,
        key: record.stable_key().clone(),
        declaration: declaration.finish(),
        signature: signature.finish(),
        body_or_initializer,
        precision,
    })
}

fn declaration_surfaces_match(
    previous: &[StableDefinitionInputFingerprint],
    current: &[StableDefinitionInputFingerprint],
) -> (bool, usize) {
    if previous.len() != current.len() {
        return (false, 0);
    }
    let mut compared = 0;
    for (left, right) in previous.iter().zip(current) {
        compared += 1;
        let supported = matches!(
            left.key.kind(),
            StableDefinitionKind::Function
                | StableDefinitionKind::Struct
                | StableDefinitionKind::Enum
                | StableDefinitionKind::Destructor
                | StableDefinitionKind::Method
                | StableDefinitionKind::AssociatedFunction
        ) && !matches!(
            left.precision,
            StableDefinitionFingerprintPrecision::SignatureAndInitializer
                | StableDefinitionFingerprintPrecision::ConservativeFullDeclaration
        );
        let matches = supported
            && left.schema_version == right.schema_version
            && left.key == right.key
            && left.declaration == right.declaration
            && left.signature == right.signature
            && left.precision == right.precision;
        if !matches {
            return (false, compared);
        }
    }
    (true, compared)
}

struct FramedDefinitionHasher(Sha256);

impl FramedDefinitionHasher {
    fn new(domain: &[u8]) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(domain);
        Self(hasher)
    }

    fn frame(&mut self, bytes: &[u8]) {
        self.0.update((bytes.len() as u64).to_le_bytes());
        self.0.update(bytes);
    }

    fn finish(self) -> StableDefinitionFingerprint {
        StableDefinitionFingerprint(self.0.finalize().into())
    }
}

fn hash_stable_definition_key(hasher: &mut FramedDefinitionHasher, key: &StableDefinitionKey) {
    hasher.frame(key.module().as_str().as_bytes());
    hasher.frame(&[stable_namespace_tag(key.namespace())]);
    hasher.frame(&[stable_kind_tag(key.kind())]);
    hasher.frame(key.name().as_bytes());
    match key.owner() {
        None => hasher.frame(&[]),
        Some(owner) => {
            hasher.frame(&[1]);
            hasher.frame(owner.module().as_str().as_bytes());
            hasher.frame(&[stable_kind_tag(owner.kind())]);
            hasher.frame(owner.name().as_bytes());
        }
    }
}

fn stable_namespace_tag(namespace: StableDefinitionNamespace) -> u8 {
    match namespace {
        StableDefinitionNamespace::Value => 0,
        StableDefinitionNamespace::Type => 1,
        StableDefinitionNamespace::Destructor => 2,
        StableDefinitionNamespace::Method => 3,
    }
}

fn stable_kind_tag(kind: StableDefinitionKind) -> u8 {
    match kind {
        StableDefinitionKind::Function => 0,
        StableDefinitionKind::Struct => 1,
        StableDefinitionKind::Enum => 2,
        StableDefinitionKind::ValueConst => 3,
        StableDefinitionKind::ModuleBinding => 4,
        StableDefinitionKind::Destructor => 5,
        StableDefinitionKind::Method => 6,
        StableDefinitionKind::AssociatedFunction => 7,
    }
}

fn stable_free_function_endpoint(
    definitions: &BoundDefinitionSet,
    file: u32,
    name: &str,
) -> Result<StableDefinitionKey, CompileErrors> {
    let matches = definitions
        .definitions()
        .iter()
        .filter(|record| {
            record.declaration_span().file_id.index() == file
                && record.stable_key().name() == name
                && record.stable_key().namespace() == StableDefinitionNamespace::Value
                && record.stable_key().kind() == StableDefinitionKind::Function
        })
        .collect::<Vec<_>>();
    let [record] = matches.as_slice() else {
        return Err(invalid_dependency_manifest(&format!(
            "free-function dependency endpoint ({file}, '{name}') did not join exactly one bound function",
        )));
    };
    Ok(record.stable_key().clone())
}

fn stable_body_owner_endpoint(
    semantic: &CanonicalSemanticOutput,
    definitions: &BoundDefinitionSet,
    event: &rue_air::AnalyzedBodyOwnerEvent,
) -> Result<Option<(StableDefinitionKey, bool)>, CompileErrors> {
    let (token, provenance, generic) = match event {
        rue_air::AnalyzedBodyOwnerEvent::FreeFunction { token, file, name } => (
            *token,
            stable_free_function_endpoint(definitions, *file, name)?,
            false,
        ),
        rue_air::AnalyzedBodyOwnerEvent::NamedMethod {
            token,
            file,
            owner_name,
            method_name,
            generic,
        } => (
            *token,
            stable_named_method_endpoint(definitions, *file, owner_name, method_name)?,
            *generic,
        ),
        rue_air::AnalyzedBodyOwnerEvent::NamedDestructor {
            token,
            file,
            owner_name,
        } => (
            *token,
            stable_named_destructor_endpoint(definitions, *file, owner_name)?,
            false,
        ),
        rue_air::AnalyzedBodyOwnerEvent::Anonymous => return Ok(None),
    };
    let authoritative = semantic
        .body_owner_issuer()
        .key_for_body_token(token)
        .map_err(CompileErrors::from)?;
    if authoritative != &provenance {
        return Err(invalid_dependency_manifest(
            "body owner token does not match its checked source provenance",
        ));
    }
    Ok(Some((authoritative.clone(), generic)))
}

fn stable_token_endpoint(
    semantic: &CanonicalSemanticOutput,
    token: rue_air::BodyOwnerToken,
    provenance: &StableDefinitionKey,
) -> Result<StableDefinitionKey, CompileErrors> {
    let authoritative = semantic
        .body_owner_issuer()
        .key_for_body_token(token)
        .map_err(CompileErrors::from)?;
    if authoritative != provenance {
        return Err(invalid_dependency_manifest(
            "body-local observation token does not match its checked source provenance",
        ));
    }
    Ok(authoritative.clone())
}

fn stable_implicit_drop_source_endpoint(
    semantic: &CanonicalSemanticOutput,
    definitions: &BoundDefinitionSet,
    source: &rue_air::ImplicitDropDependencySourceEvent,
) -> Result<StableDefinitionKey, CompileErrors> {
    match source {
        rue_air::ImplicitDropDependencySourceEvent::Anonymous => Err(invalid_dependency_manifest(
            "anonymous drop-dependency source has no stable endpoint",
        )),
        rue_air::ImplicitDropDependencySourceEvent::FreeFunction { token, file, name } => {
            let provenance = stable_free_function_endpoint(definitions, *file, name)?;
            stable_token_endpoint(semantic, *token, &provenance)
        }
        rue_air::ImplicitDropDependencySourceEvent::NamedMethod {
            token,
            file,
            owner_name,
            method_name,
        } => {
            let provenance =
                stable_named_method_endpoint(definitions, *file, owner_name, method_name)?;
            stable_token_endpoint(semantic, *token, &provenance)
        }
        rue_air::ImplicitDropDependencySourceEvent::NamedDestructor {
            token,
            file,
            owner_name,
        } => {
            let provenance = stable_named_destructor_endpoint(definitions, *file, owner_name)?;
            stable_token_endpoint(semantic, *token, &provenance)
        }
        rue_air::ImplicitDropDependencySourceEvent::NamedStruct { file, name } => {
            stable_top_level_endpoint(
                definitions,
                *file,
                name,
                StableDefinitionNamespace::Type,
                StableDefinitionKind::Struct,
            )
        }
        rue_air::ImplicitDropDependencySourceEvent::NamedEnum { file, name } => {
            stable_top_level_endpoint(
                definitions,
                *file,
                name,
                StableDefinitionNamespace::Type,
                StableDefinitionKind::Enum,
            )
        }
    }
}

fn stable_named_method_endpoint(
    definitions: &BoundDefinitionSet,
    file: u32,
    owner_name: &str,
    method_name: &str,
) -> Result<StableDefinitionKey, CompileErrors> {
    let matches = definitions
        .definitions()
        .iter()
        .filter(|record| {
            let key = record.stable_key();
            record.declaration_span().file_id.index() == file
                && key.name() == method_name
                && key.namespace() == StableDefinitionNamespace::Method
                && matches!(
                    key.kind(),
                    StableDefinitionKind::Method | StableDefinitionKind::AssociatedFunction
                )
                && key.owner().is_some_and(|owner| owner.name() == owner_name)
        })
        .collect::<Vec<_>>();
    let [record] = matches.as_slice() else {
        return Err(invalid_dependency_manifest(&format!(
            "named-method dependency endpoint ({file}, '{owner_name}', '{method_name}') did not join exactly one bound method",
        )));
    };
    Ok(record.stable_key().clone())
}

fn stable_named_destructor_endpoint(
    definitions: &BoundDefinitionSet,
    file: u32,
    owner_name: &str,
) -> Result<StableDefinitionKey, CompileErrors> {
    let matches = definitions
        .definitions()
        .iter()
        .filter(|record| {
            let key = record.stable_key();
            record.declaration_span().file_id.index() == file
                && key.name() == owner_name
                && key.namespace() == StableDefinitionNamespace::Destructor
                && key.kind() == StableDefinitionKind::Destructor
                && key.owner().is_some_and(|owner| owner.name() == owner_name)
        })
        .collect::<Vec<_>>();
    let [record] = matches.as_slice() else {
        return Err(invalid_dependency_manifest(&format!(
            "named-destructor dependency endpoint ({file}, '{owner_name}') did not join exactly one bound destructor",
        )));
    };
    Ok(record.stable_key().clone())
}

fn stable_declaration_source_endpoint(
    definitions: &BoundDefinitionSet,
    event: &rue_air::DeclarationTypeDependencyEvent,
) -> Result<StableDefinitionKey, CompileErrors> {
    stable_declaration_type_source_endpoint(
        definitions,
        event.source_file,
        &event.source_name,
        event.source_owner_name.as_deref(),
        event.source_kind,
    )
}

fn stable_declaration_type_source_endpoint(
    definitions: &BoundDefinitionSet,
    source: u32,
    source_name: &str,
    source_owner_name: Option<&str>,
    source_kind: rue_air::DeclarationTypeDependencySourceKind,
) -> Result<StableDefinitionKey, CompileErrors> {
    use rue_air::DeclarationTypeDependencySourceKind as K;
    match source_kind {
        K::Function => stable_free_function_endpoint(definitions, source, source_name),
        K::Method | K::AssociatedFunction => stable_named_method_endpoint(
            definitions,
            source,
            source_owner_name.unwrap_or(""),
            source_name,
        ),
        K::Destructor => stable_named_destructor_endpoint(
            definitions,
            source,
            source_owner_name.unwrap_or(source_name),
        ),
        K::Struct => stable_top_level_endpoint(
            definitions,
            source,
            source_name,
            StableDefinitionNamespace::Type,
            StableDefinitionKind::Struct,
        ),
        K::Enum => stable_top_level_endpoint(
            definitions,
            source,
            source_name,
            StableDefinitionNamespace::Type,
            StableDefinitionKind::Enum,
        ),
        K::ValueConst => stable_top_level_endpoint(
            definitions,
            source,
            source_name,
            StableDefinitionNamespace::Value,
            StableDefinitionKind::ValueConst,
        ),
    }
}

fn stable_named_type_endpoint(
    definitions: &BoundDefinitionSet,
    event: &rue_air::DeclarationTypeDependencyEvent,
) -> Result<StableDefinitionKey, CompileErrors> {
    let kind = match event.target_kind {
        rue_air::DeclarationTypeDependencyTargetKind::Struct => StableDefinitionKind::Struct,
        rue_air::DeclarationTypeDependencyTargetKind::Enum => StableDefinitionKind::Enum,
        rue_air::DeclarationTypeDependencyTargetKind::ValueConst => {
            return stable_top_level_endpoint(
                definitions,
                event.target_file,
                &event.target_name,
                StableDefinitionNamespace::Value,
                StableDefinitionKind::ValueConst,
            );
        }
    };
    stable_top_level_endpoint(
        definitions,
        event.target_file,
        &event.target_name,
        StableDefinitionNamespace::Type,
        kind,
    )
}

fn stable_top_level_endpoint(
    definitions: &BoundDefinitionSet,
    file: u32,
    name: &str,
    namespace: StableDefinitionNamespace,
    kind: StableDefinitionKind,
) -> Result<StableDefinitionKey, CompileErrors> {
    let matches = definitions
        .definitions()
        .iter()
        .filter(|record| {
            record.declaration_span().file_id.index() == file
                && record.stable_key().name() == name
                && record.stable_key().namespace() == namespace
                && record.stable_key().kind() == kind
                && record.stable_key().owner().is_none()
        })
        .collect::<Vec<_>>();
    let [record] = matches.as_slice() else {
        return Err(invalid_dependency_manifest(
            "declaration-type dependency endpoint did not join exactly one stable definition",
        ));
    };
    Ok(record.stable_key().clone())
}

fn invalid_dependency_manifest(reason: &str) -> CompileErrors {
    CompileErrors::from(CompileError::without_span(ErrorKind::InvalidCompilerInput(
        reason.to_owned(),
    )))
}

fn same_attempt(left: &SourceSnapshot, right: &SourceSnapshot) -> bool {
    left.source_revision() == right.source_revision() && left.metadata() == right.metadata()
}

fn programs_are_pointer_equivalent(left: &ParsedProgram, right: &ParsedProgram) -> bool {
    left.source_revision() == right.source_revision()
        && left.modules().len() == right.modules().len()
        && left
            .modules()
            .iter()
            .zip(right.modules())
            .all(|(left, right)| Arc::ptr_eq(left, right))
}

fn validate_accepted_read_manifest(
    snapshot: &SourceSnapshot,
    accepted_reads: &[crate::AcceptedReadManifestEntry],
) -> Result<(), CompileErrors> {
    if accepted_reads.len() != snapshot.len() {
        return Err(CompileErrors::from(CompileError::without_span(
            ErrorKind::InvalidCompilerInput(
                "accepted read manifest does not cover the staging source snapshot".into(),
            ),
        )));
    }
    let entries = accepted_reads
        .iter()
        .map(|entry| (entry.module(), entry))
        .collect::<BTreeMap<_, _>>();
    if entries.len() != accepted_reads.len() {
        return Err(CompileErrors::from(CompileError::without_span(
            ErrorKind::InvalidCompilerInput(
                "accepted read manifest contains duplicate logical modules".into(),
            ),
        )));
    }
    for source in snapshot.files() {
        let module = snapshot
            .module_id(source.file_id)
            .expect("snapshot files have logical module IDs");
        let Some(entry) = entries.get(module) else {
            return Err(CompileErrors::from(CompileError::without_span(
                ErrorKind::InvalidCompilerInput(format!(
                    "accepted read manifest is missing logical module {module}"
                )),
            )));
        };
        if entry.content_fingerprint() != crate::import_discovery::source_fingerprint(source.source)
        {
            return Err(CompileErrors::from(CompileError::without_span(
                ErrorKind::InvalidCompilerInput(format!(
                    "accepted read manifest content does not match logical module {module}"
                )),
            )));
        }
    }
    Ok(())
}

fn no_published_program() -> CompileErrors {
    CompileErrors::from(CompileError::without_span(ErrorKind::InvalidCompilerInput(
        "frontend query session has no successful parsed program".to_string(),
    )))
}

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, sync::Arc};

    use rue_span::FileId;

    use super::*;
    use crate::{
        CanonicalImportResolution, LinkerMode, ModuleId, OptLevel, PreviewFeature, PreviewFeatures,
        SourceMetadata, SourceSnapshot, Target,
    };

    fn snapshot(entries: &[(u32, &str, &str, &str)], root: u32) -> SourceSnapshot {
        let physical = entries
            .iter()
            .map(|(id, path, _, _)| (FileId::new(*id), (*path).to_owned()))
            .collect::<HashMap<_, _>>();
        let logical = entries
            .iter()
            .map(|(id, _, logical, _)| (FileId::new(*id), (*logical).to_owned()))
            .collect::<HashMap<_, _>>();
        let metadata = SourceMetadata::new(FileId::new(root), physical, logical).unwrap();
        SourceSnapshot::new(
            metadata,
            entries
                .iter()
                .map(|(id, _, _, text)| (FileId::new(*id), Arc::new((*text).to_owned())))
                .collect(),
        )
        .unwrap()
    }

    fn base() -> SourceSnapshot {
        snapshot(
            &[
                (7, "/p/main.rue", "main.rue", "fn main() -> i32 { 0 }"),
                (2, "/p/a.rue", "a.rue", "fn a() {}"),
            ],
            7,
        )
    }

    fn function_modules(count: usize, edited: Option<usize>) -> SourceSnapshot {
        let owned = (0..count)
            .map(|index| {
                let id = u32::try_from(index + 1).unwrap();
                let logical = if index == 0 {
                    "main.rue".to_owned()
                } else {
                    format!("m{index}.rue")
                };
                let physical = format!("/p/{logical}");
                let body = if index == 0 {
                    format!(
                        "fn main() -> i32 {{ {} }}",
                        usize::from(edited == Some(index))
                    )
                } else {
                    format!(
                        "fn f{index}() -> i32 {{ {} }}",
                        if edited == Some(index) {
                            index + 1
                        } else {
                            index
                        }
                    )
                };
                (id, physical, logical, body)
            })
            .collect::<Vec<_>>();
        let borrowed = owned
            .iter()
            .map(|(id, physical, logical, body)| {
                (*id, physical.as_str(), logical.as_str(), body.as_str())
            })
            .collect::<Vec<_>>();
        snapshot(&borrowed, 1)
    }

    #[test]
    fn leaf_body_edit_reuses_128_durable_declarations_and_skips_ordinary_resolution() {
        let options = CompileOptions::default();
        let first = function_modules(128, None);
        // Edit the reachable entry body while retaining all 128 declarations;
        // this proves reuse does not accidentally pass by changing dead code.
        let second = function_modules(128, Some(0));
        let mut session = CompilerSession::new();
        session.update(&first).into_result().unwrap();
        let cold = session.semantic(&options).unwrap();
        assert_eq!(cold.work().binding.bind_invocations, 1);
        assert_eq!(cold.work().binding.declaration_resolution_invocations, 1);
        assert_eq!(
            cold.work()
                .declaration_reuse
                .durable_cache_population_exports,
            1
        );
        assert_eq!(cold.work().manifest.rir_instructions_visited, 256);
        session.update(&second).into_result().unwrap();
        let reused = session.semantic(&options).unwrap();

        assert_eq!(reused.work().binding.declaration_resolution_invocations, 0);
        assert_eq!(reused.work().binding.bind_invocations, 1);
        assert_eq!(reused.work().declaration_reuse.semantic_epochs_started, 1);
        assert_eq!(reused.work().declaration_reuse.declaration_indexes_built, 1);
        assert_eq!(
            reused.work().declaration_reuse.shell_predeclaration_epochs,
            1
        );
        assert_eq!(reused.work().declaration_reuse.fallback_epochs_started, 0);
        assert_eq!(reused.work().binding.durable_payloads_installed, 128);
        assert_eq!(reused.work().declaration_reuse.durable_records_reused, 128);
        assert_eq!(
            reused
                .work()
                .declaration_reuse
                .ordinary_declaration_resolutions_skipped,
            1
        );
        let mut fresh = CompilerSession::new();
        fresh.update(&second).into_result().unwrap();
        let ordinary = fresh.semantic(&options).unwrap();
        assert_eq!(
            ordinary.work().binding.declaration_resolution_invocations,
            1
        );
        assert_eq!(reused.work().body_analysis, ordinary.work().body_analysis);
        assert_eq!(
            format!("{:?}", reused.functions()),
            format!("{:?}", ordinary.functions())
        );
        assert_eq!(reused.strings(), ordinary.strings());
        assert_eq!(
            format!("{:?}", reused.warnings()),
            format!("{:?}", ordinary.warnings())
        );
    }

    #[test]
    fn const_presence_forces_fresh_ordinary_resolution_without_partial_install() {
        let first = snapshot(
            &[(
                1,
                "/p/main.rue",
                "main.rue",
                "const n: i32 = 1; fn main() -> i32 { n }",
            )],
            1,
        );
        let second = snapshot(
            &[(
                1,
                "/p/main.rue",
                "main.rue",
                "const n: i32 = 1; fn main() -> i32 { n + 1 }",
            )],
            1,
        );
        let options = CompileOptions::default();
        let mut session = CompilerSession::new();
        session.update(&first).into_result().unwrap();
        let first_output = session.semantic(&options).unwrap();
        let first_issuer = first_output.analyzed_body_owners()[0]
            .token()
            .unwrap()
            .issuer();
        session.update(&second).into_result().unwrap();
        let output = session.semantic(&options).unwrap();
        let second_issuer = output.analyzed_body_owners()[0].token().unwrap().issuer();
        assert_ne!(first_issuer, second_issuer);
        assert_eq!(output.work().binding.declaration_resolution_invocations, 1);
        assert_eq!(output.work().binding.durable_install_invocations, 0);
        assert_eq!(output.work().declaration_reuse.durable_records_reused, 0);
    }

    fn assert_semantic_artifact_parity(
        session: &CompilerSession,
        actual: &CanonicalSemanticOutput,
        fresh: &CanonicalSemanticOutput,
    ) {
        assert_eq!(
            format!("{:?}", actual.functions()),
            format!("{:?}", fresh.functions())
        );
        assert_eq!(actual.strings(), fresh.strings());
        assert_eq!(
            format!("{:?}", actual.warnings()),
            format!("{:?}", fresh.warnings())
        );
        let diagnostics = session
            .latest_diagnostics()
            .expect("semantic query publishes diagnostics");
        assert!(diagnostics.is_success());
        assert_eq!(
            format!("{:?}", diagnostics.warnings()),
            format!("{:?}", fresh.warnings())
        );
    }

    #[test]
    fn generic_named_method_reuse_fails_closed_without_poisoning_recovery() {
        let source = |body: &str| snapshot(&[(1, "/p/main.rue", "main.rue", body)], 1);
        let first = source(
            "struct Value { fn choose(borrow self, comptime n: i32) -> i32 { n } } fn main() -> i32 { let value = Value {}; value.choose(1) }",
        );
        let edited = source(
            "struct Value { fn choose(borrow self, comptime n: i32) -> i32 { n + 1 } } fn main() -> i32 { let value = Value {}; value.choose(1) }",
        );
        let supported = source("fn main() -> i32 { 1 }");
        let supported_edit = source("fn main() -> i32 { 2 }");
        let options = CompileOptions::default();
        let mut session = CompilerSession::new();
        session.update(&first).into_result().unwrap();
        let cold = session.semantic(&options).unwrap();
        assert_eq!(cold.work().binding.declaration_resolution_invocations, 1);
        assert_eq!(cold.work().binding.durable_install_invocations, 0);

        session.update(&edited).into_result().unwrap();
        let ordinary = session.semantic(&options).unwrap();
        assert_eq!(
            ordinary.work().binding.declaration_resolution_invocations,
            1
        );
        assert_eq!(ordinary.work().binding.durable_install_invocations, 0);
        assert_eq!(ordinary.work().declaration_reuse.durable_records_reused, 0);
        let mut fresh = CompilerSession::new();
        fresh.update(&edited).into_result().unwrap();
        let expected = fresh.semantic(&options).unwrap();
        assert_semantic_artifact_parity(&session, &ordinary, &expected);

        // Unsupported revisions did not leave a partial baseline: a supported
        // revision seeds normally, and its next body edit can reuse normally.
        session.update(&supported).into_result().unwrap();
        let seeded = session.semantic(&options).unwrap();
        assert_eq!(seeded.work().binding.declaration_resolution_invocations, 1);
        assert_eq!(seeded.work().binding.durable_install_invocations, 0);
        session.update(&supported_edit).into_result().unwrap();
        let recovered = session.semantic(&options).unwrap();
        assert_eq!(
            recovered.work().binding.declaration_resolution_invocations,
            0
        );
        assert_eq!(recovered.work().binding.durable_payloads_installed, 1);
    }

    #[test]
    fn anonymous_structural_reuse_fails_closed_without_partial_install() {
        let first = snapshot(
            &[(
                1,
                "/p/main.rue",
                "main.rue",
                "fn Box(comptime T: type) -> type { struct { value: T, fn get(borrow self) -> T { self.value } } } fn main() -> i32 { let B = Box(i32); let value = B { value: 1 }; value.get() }",
            )],
            1,
        );
        let edited = snapshot(
            &[(
                1,
                "/p/main.rue",
                "main.rue",
                "fn Box(comptime T: type) -> type { struct { value: T, fn get(borrow self) -> T { self.value } } } fn main() -> i32 { let B = Box(i32); let value = B { value: 2 }; value.get() }",
            )],
            1,
        );
        let options = CompileOptions::default();
        let mut session = CompilerSession::new();
        session.update(&first).into_result().unwrap();
        let cold = session.semantic(&options).unwrap();
        assert_eq!(cold.work().binding.declaration_resolution_invocations, 1);
        assert_eq!(cold.work().binding.durable_install_invocations, 0);

        session.update(&edited).into_result().unwrap();
        let ordinary = session.semantic(&options).unwrap();
        assert_eq!(
            ordinary.work().binding.declaration_resolution_invocations,
            1
        );
        assert_eq!(ordinary.work().binding.durable_install_invocations, 0);
        assert_eq!(ordinary.work().declaration_reuse.durable_records_reused, 0);
        let mut fresh = CompilerSession::new();
        fresh.update(&edited).into_result().unwrap();
        let expected = fresh.semantic(&options).unwrap();
        assert_semantic_artifact_parity(&session, &ordinary, &expected);

        let supported = snapshot(
            &[(1, "/p/main.rue", "main.rue", "fn main() -> i32 { 1 }")],
            1,
        );
        let supported_edit = snapshot(
            &[(1, "/p/main.rue", "main.rue", "fn main() -> i32 { 2 }")],
            1,
        );
        session.update(&supported).into_result().unwrap();
        let seeded = session.semantic(&options).unwrap();
        assert_eq!(seeded.work().binding.declaration_resolution_invocations, 1);
        assert_eq!(seeded.work().binding.durable_install_invocations, 0);
        session.update(&supported_edit).into_result().unwrap();
        let recovered = session.semantic(&options).unwrap();
        assert_eq!(
            recovered.work().binding.declaration_resolution_invocations,
            0
        );
        assert_eq!(recovered.work().binding.durable_payloads_installed, 1);
    }

    #[test]
    fn signature_target_and_failed_body_changes_fail_closed_and_recovery_reuses() {
        let base = snapshot(
            &[(
                1,
                "/p/main.rue",
                "main.rue",
                "fn value() -> i32 { 1 } fn main() { value(); }",
            )],
            1,
        );
        let signature = snapshot(
            &[(
                1,
                "/p/main.rue",
                "main.rue",
                "fn value() -> i64 { 1 } fn main() { value(); }",
            )],
            1,
        );
        let broken_body = snapshot(
            &[(
                1,
                "/p/main.rue",
                "main.rue",
                "fn value() -> i64 { missing } fn main() { value(); }",
            )],
            1,
        );
        let recovered = snapshot(
            &[(
                1,
                "/p/main.rue",
                "main.rue",
                "fn value() -> i64 { 2 } fn main() { value(); }",
            )],
            1,
        );
        let options = CompileOptions::default();
        let mut session = CompilerSession::new();
        session.update(&base).into_result().unwrap();
        session.semantic(&options).unwrap();

        session.update(&signature).into_result().unwrap();
        let changed = session.semantic(&options).unwrap();
        assert_eq!(changed.work().binding.declaration_resolution_invocations, 1);

        session.update(&broken_body).into_result().unwrap();
        assert!(session.semantic(&options).is_err());
        session.update(&recovered).into_result().unwrap();
        let recovered = session.semantic(&options).unwrap();
        assert_eq!(
            recovered.work().binding.declaration_resolution_invocations,
            0
        );
        assert_eq!(recovered.work().declaration_reuse.durable_records_reused, 2);

        let mut other_target = options.clone();
        other_target.target = *Target::all()
            .iter()
            .find(|target| **target != options.target)
            .unwrap();
        let target_changed = session.semantic(&other_target).unwrap();
        assert_eq!(
            target_changed
                .work()
                .binding
                .declaration_resolution_invocations,
            1
        );
    }

    #[test]
    fn repeated_queries_and_noop_update_retain_pointer_identity() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<CompilerSession>();
        assert_send_sync::<CanonicalMergedProgram>();
        assert_send_sync::<CanonicalRirOutput>();

        let source = base();
        let mut session = CompilerSession::new();
        let first_program = session.update(&source).into_result().unwrap();
        let first_merge = session.merge().unwrap();
        let second_merge = session.merge().unwrap();
        let first_rir = session.rir().unwrap();
        let second_rir = session.rir().unwrap();
        assert!(Arc::ptr_eq(&first_merge, &second_merge));
        assert!(Arc::ptr_eq(&first_rir, &second_rir));

        let noop = session.update(&source);
        assert!(!noop.downstream_invalidated());
        let second_program = noop.into_result().unwrap();
        assert!(Arc::ptr_eq(&first_program, &second_program));
        assert!(Arc::ptr_eq(&first_merge, &session.merge().unwrap()));
        assert!(Arc::ptr_eq(&first_rir, &session.rir().unwrap()));
        assert_eq!(session.work().merge.executions, 1);
        assert_eq!(session.work().rir.executions, 1);
        assert_eq!(session.work().downstream_invalidations, 0);

        let published = session.published().unwrap().clone();
        let merged = first_merge.clone();
        let rir = first_rir.clone();
        std::thread::spawn(move || {
            assert_eq!(published.modules().len(), 2);
            assert_eq!(merged.ast().modules().len(), 2);
            assert!(!rir.rir().is_empty());
        })
        .join()
        .unwrap();
    }

    #[test]
    fn one_edit_among_128_recomputes_downstream_once() {
        let make = |edited: bool| {
            let physical = (0..128)
                .map(|index| (FileId::new(index), format!("/p/m{index}.rue")))
                .collect();
            let logical = (0..128)
                .map(|index| (FileId::new(index), format!("m{index}.rue")))
                .collect();
            let metadata = SourceMetadata::new(FileId::new(0), physical, logical).unwrap();
            SourceSnapshot::new(
                metadata,
                (0..128)
                    .map(|index| {
                        let value = if edited && index == 81 { 2 } else { 1 };
                        (
                            FileId::new(index),
                            Arc::new(format!("fn f{index}() -> i32 {{ {value} }}")),
                        )
                    })
                    .collect(),
            )
            .unwrap()
        };
        let mut session = CompilerSession::new();
        session.update(&make(false)).into_result().unwrap();
        session.rir().unwrap();
        let first_shards = session
            .definition_shard_baseline
            .as_ref()
            .unwrap()
            .shards()
            .to_vec();
        let update = session.update(&make(true));
        assert!(update.downstream_invalidated());
        assert_eq!(update.work().modules_reused, 127);
        assert_eq!(update.work().modules_reparsed, 1);
        session.rir().unwrap();
        let second_shards = session.definition_shard_baseline.as_ref().unwrap().shards();
        assert!(
            first_shards
                .iter()
                .zip(second_shards)
                .all(|(first, second)| Arc::ptr_eq(first, second))
        );
        assert_eq!(session.work().last_merge.definition_shards_indexed, 128);
        assert_eq!(session.work().last_merge.definition_shards_reused, 128);
        assert_eq!(session.work().last_merge.definition_shards_rebuilt, 0);
        session.rir().unwrap();
        assert_eq!(session.work().merge.executions, 2);
        assert_eq!(session.work().rir.executions, 2);
        assert_eq!(session.work().downstream_invalidations, 1);
    }

    #[test]
    fn definition_shards_fail_closed_on_surface_identity_changes() {
        let initial = snapshot(
            &[
                (1, "/p/main.rue", "main.rue", "fn main() -> i32 { 0 }"),
                (2, "/p/a.rue", "a.rue", "fn a() -> i32 { 1 }"),
            ],
            1,
        );
        let body = snapshot(
            &[
                (1, "/p/main.rue", "main.rue", "fn main() -> i32 { 0 }"),
                (2, "/p/a.rue", "a.rue", "fn a() -> i32 { 2 }"),
            ],
            1,
        );
        let renamed_definition = snapshot(
            &[
                (1, "/p/main.rue", "main.rue", "fn main() -> i32 { 0 }"),
                (2, "/p/a.rue", "a.rue", "fn b() -> i32 { 2 }"),
            ],
            1,
        );
        let relocated = snapshot(
            &[
                (1, "/m/main.rue", "main.rue", "fn main() -> i32 { 0 }"),
                (2, "/m/a.rue", "a.rue", "fn b() -> i32 { 2 }"),
            ],
            1,
        );
        let reassigned = snapshot(
            &[
                (11, "/m/main.rue", "main.rue", "fn main() -> i32 { 0 }"),
                (12, "/m/a.rue", "a.rue", "fn b() -> i32 { 2 }"),
            ],
            11,
        );
        let mut session = CompilerSession::new();
        session.update(&initial).into_result().unwrap();
        session.merge().unwrap();

        session.update(&body).into_result().unwrap();
        session.merge().unwrap();
        assert_eq!(session.work().last_merge.definition_shards_reused, 2);
        assert_eq!(session.work().last_merge.definition_shards_rebuilt, 0);

        session.update(&renamed_definition).into_result().unwrap();
        session.merge().unwrap();
        assert_eq!(session.work().last_merge.definition_shards_reused, 1);
        assert_eq!(session.work().last_merge.definition_shards_rebuilt, 1);

        session.update(&relocated).into_result().unwrap();
        session.merge().unwrap();
        assert_eq!(session.work().last_merge.definition_shards_reused, 2);
        assert_eq!(session.work().last_merge.definition_shards_rebuilt, 0);

        session.update(&reassigned).into_result().unwrap();
        session.merge().unwrap();
        assert_eq!(session.work().last_merge.definition_shards_reused, 0);
        assert_eq!(session.work().last_merge.definition_shards_rebuilt, 2);
    }

    #[test]
    fn syntax_failure_preserves_published_revision_and_cached_queries() {
        let source = base();
        let broken = snapshot(
            &[
                (7, "/p/main.rue", "main.rue", "fn main( {"),
                (2, "/p/a.rue", "a.rue", "fn a() {}"),
            ],
            7,
        );
        let mut session = CompilerSession::new();
        let program = session.update(&source).into_result().unwrap();
        let merged = session.merge().unwrap();
        let rir = session.rir().unwrap();
        let failed = session.update(&broken);
        assert!(failed.result().is_err());
        assert!(!failed.downstream_invalidated());
        assert!(Arc::ptr_eq(session.published().unwrap(), &program));
        assert!(Arc::ptr_eq(&session.merge().unwrap(), &merged));
        assert!(Arc::ptr_eq(&session.rir().unwrap(), &rir));
    }

    #[test]
    fn duplicate_merge_error_is_memoized_and_recovery_invalidates_it() {
        let duplicate = snapshot(
            &[(
                1,
                "/p/main.rue",
                "main.rue",
                "fn same() {} fn same() {} fn main() {}",
            )],
            1,
        );
        let fixed = snapshot(
            &[(1, "/p/main.rue", "main.rue", "fn main() -> i32 { 0 }")],
            1,
        );
        let mut session = CompilerSession::new();
        session.update(&duplicate).into_result().unwrap();
        let first = session.merge().unwrap_err();
        let second = session.merge().unwrap_err();
        assert_eq!(format!("{first:?}"), format!("{second:?}"));
        assert!(session.rir().is_err());
        assert!(session.semantic(&CompileOptions::default()).is_err());
        assert_eq!(session.work().merge.executions, 1);
        assert_eq!(session.work().rir.executions, 0);
        assert_eq!(session.work().semantic.executions, 0);

        let update = session.update(&fixed);
        assert!(update.downstream_invalidated());
        update.into_result().unwrap();
        assert!(session.rir().is_ok());
        assert_eq!(session.work().merge.executions, 2);
        assert_eq!(session.work().rir.executions, 1);
    }

    #[test]
    fn root_relocation_file_id_and_logical_changes_invalidate_correctly() {
        let base = snapshot(
            &[
                (1, "/old/a.rue", "a.rue", "fn a() {}"),
                (2, "/old/b.rue", "b.rue", "fn b() {}"),
            ],
            1,
        );
        let root_only = snapshot(
            &[
                (1, "/old/a.rue", "a.rue", "fn a() {}"),
                (2, "/old/b.rue", "b.rue", "fn b() {}"),
            ],
            2,
        );
        let relocated = snapshot(
            &[
                (1, "/new/a.rue", "a.rue", "fn a() {}"),
                (2, "/new/b.rue", "b.rue", "fn b() {}"),
            ],
            2,
        );
        let reassigned = snapshot(
            &[
                (11, "/new/a.rue", "a.rue", "fn a() {}"),
                (12, "/new/b.rue", "b.rue", "fn b() {}"),
            ],
            12,
        );
        let renamed = snapshot(
            &[
                (11, "/new/a2.rue", "a2.rue", "fn a() {}"),
                (12, "/new/b.rue", "b.rue", "fn b() {}"),
            ],
            12,
        );
        let mut session = CompilerSession::new();
        session.update(&base).into_result().unwrap();
        session.rir().unwrap();

        let root = session.update(&root_only);
        assert!(root.downstream_invalidated());
        assert_eq!(root.work().modules_reused, 2);
        root.into_result().unwrap();
        session.rir().unwrap();
        let moved = session.update(&relocated);
        assert!(moved.downstream_invalidated());
        assert_eq!(moved.work().modules_rebound, 2);
        moved.into_result().unwrap();
        session.rir().unwrap();
        let ids = session.update(&reassigned);
        assert!(ids.downstream_invalidated());
        assert_eq!(ids.work().modules_reparsed, 2);
        ids.into_result().unwrap();
        session.rir().unwrap();
        let rename = session.update(&renamed);
        assert!(rename.downstream_invalidated());
        assert_eq!(rename.invalidation().added.len(), 1);
        assert_eq!(rename.invalidation().removed.len(), 1);
        assert_eq!(rename.work().modules_rebound, 1);
    }

    #[test]
    fn semantic_queries_reuse_by_codegen_identity_and_ignore_linker() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<CanonicalSemanticOutput>();

        let source = base();
        let mut session = CompilerSession::new();
        session.update(&source).into_result().unwrap();
        let options = CompileOptions::default();
        let first = session.semantic(&options).unwrap();
        let second = session.semantic(&options).unwrap();
        assert!(Arc::ptr_eq(&first, &second));

        let linker_only = CompileOptions {
            linker: LinkerMode::System("unused-linker".to_string()),
            ..options.clone()
        };
        assert!(Arc::ptr_eq(
            &first,
            &session.semantic(&linker_only).unwrap()
        ));
        assert_eq!(session.work().semantic.executions, 1);
        assert_eq!(session.work().semantic.reuses, 2);
        assert_eq!(session.work().semantic_entries, 1);
        assert_eq!(session.work().merge.executions, 1);
        assert_eq!(session.work().rir.executions, 1);
        assert_eq!(first.work().binding.bind_invocations, 1);
        assert_eq!(first.work().manifest.build_invocations, 1);

        let published = first.clone();
        std::thread::spawn(move || assert!(!published.functions().is_empty()))
            .join()
            .unwrap();
    }

    #[test]
    fn semantic_option_variants_create_deterministic_distinct_entries() {
        let source = base();
        let mut session = CompilerSession::new();
        session.update(&source).into_result().unwrap();
        let default = CompileOptions::default();
        session.semantic(&default).unwrap();
        session
            .semantic(&CompileOptions {
                opt_level: OptLevel::O1,
                ..default.clone()
            })
            .unwrap();
        let other_target = *Target::all()
            .iter()
            .find(|&&target| target != default.target)
            .expect("multiple compiler targets");
        session
            .semantic(&CompileOptions {
                target: other_target,
                ..default.clone()
            })
            .unwrap();
        session
            .semantic(&CompileOptions {
                preview_features: PreviewFeatures::from([PreviewFeature::TestInfra]),
                ..default
            })
            .unwrap();

        let work = session.work();
        assert_eq!(work.semantic.executions, 4);
        assert_eq!(work.semantic_entries, 4);
        assert_eq!(work.semantic_records.len(), 4);
        assert!(work.semantic_records.iter().all(|record| {
            !record.failed
                && record.work.binding.bind_invocations == 1
                && record.work.manifest.build_invocations == 1
        }));
        for (index, left) in work.semantic_records.iter().enumerate() {
            assert!(
                work.semantic_records[index + 1..]
                    .iter()
                    .all(|right| left.input != right.input)
            );
        }
    }

    #[test]
    fn semantic_cache_invalidates_on_edit_but_survives_failed_parse() {
        let source = base();
        let edited = snapshot(
            &[
                (7, "/p/main.rue", "main.rue", "fn main() -> i32 { 1 }"),
                (2, "/p/a.rue", "a.rue", "fn a() {}"),
            ],
            7,
        );
        let broken = snapshot(
            &[
                (7, "/p/main.rue", "main.rue", "fn main( {"),
                (2, "/p/a.rue", "a.rue", "fn a() {}"),
            ],
            7,
        );
        let options = CompileOptions::default();
        let mut session = CompilerSession::new();
        session.update(&source).into_result().unwrap();
        let first = session.semantic(&options).unwrap();
        assert!(session.update(&broken).result().is_err());
        assert!(Arc::ptr_eq(&first, &session.semantic(&options).unwrap()));
        let update = session.update(&edited);
        assert!(update.downstream_invalidated());
        update.into_result().unwrap();
        let second = session.semantic(&options).unwrap();
        assert!(!Arc::ptr_eq(&first, &second));
        assert_eq!(session.work().semantic.executions, 2);
        assert_eq!(session.work().semantic_entries_invalidated, 1);
    }

    #[test]
    fn semantic_errors_are_memoized_and_recovery_reexecutes() {
        let invalid = snapshot(
            &[(
                1,
                "/p/main.rue",
                "main.rue",
                "fn main() -> i32 { missing_name }",
            )],
            1,
        );
        let valid = snapshot(
            &[(1, "/p/main.rue", "main.rue", "fn main() -> i32 { 0 }")],
            1,
        );
        let options = CompileOptions::default();
        let mut session = CompilerSession::new();
        session.update(&invalid).into_result().unwrap();
        let first = session.semantic(&options).unwrap_err();
        let second = session.semantic(&options).unwrap_err();
        assert_eq!(format!("{first:?}"), format!("{second:?}"));
        assert_eq!(session.work().semantic.executions, 1);
        assert_eq!(session.work().semantic.reuses, 1);
        assert_eq!(session.work().semantic_records.len(), 1);
        assert!(session.work().semantic_records[0].failed);

        session.update(&valid).into_result().unwrap();
        assert!(session.semantic(&options).is_ok());
        assert_eq!(session.work().semantic.executions, 2);
        assert_eq!(session.work().semantic_entries, 1);
        assert_eq!(session.work().semantic_entries_invalidated, 1);
    }

    #[test]
    fn token_preparation_error_recovery_publishes_only_failure_diagnostics() {
        let source = snapshot(
            &[(
                1,
                "/p/main.rue",
                "main.rue",
                "const value: i32 = 1; const value: i32 = 2; fn main() {}",
            )],
            1,
        );
        let mut session = CompilerSession::new();
        session.update(&source).into_result().unwrap();
        let errors = session.semantic(&CompileOptions::default()).unwrap_err();
        assert!(
            errors
                .iter()
                .all(|error| error.kind.code().to_string() != "E1400")
        );
        assert_eq!(session.work().semantic_entries, 1);
        assert_eq!(session.work().semantic_records.len(), 1);
        assert!(session.work().semantic_records[0].failed);
        let diagnostics = session.latest_diagnostics().unwrap();
        assert!(!diagnostics.is_success());
        assert!(diagnostics.warnings().is_empty());
    }

    #[test]
    fn stable_definitions_are_lazy_reused_and_make_two_bind_boundary_explicit() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<BoundDefinitionSet>();

        let source = base();
        let mut session = CompilerSession::new();
        session.update(&source).into_result().unwrap();
        let ordinary_options = CompileOptions::default();
        let ordinary = session.semantic(&ordinary_options).unwrap();
        assert_eq!(session.work().definitions.executions, 0);
        assert_eq!(session.work().definition_entries, 0);

        let id_options = CompileOptions {
            linker: LinkerMode::System("ignored".to_string()),
            opt_level: OptLevel::O1,
            ..ordinary_options.clone()
        };
        let first = session.stable_definitions(&id_options).unwrap();
        let second = session
            .stable_definitions(&CompileOptions {
                linker: LinkerMode::Internal,
                opt_level: OptLevel::O3,
                ..ordinary_options
            })
            .unwrap();
        assert!(Arc::ptr_eq(&first, &second));
        assert_eq!(session.work().semantic.executions, 1);
        assert_eq!(session.work().definitions.executions, 1);
        assert_eq!(session.work().definitions.reuses, 1);
        assert_eq!(session.work().definition_entries, 1);
        let record = &session.work().definition_records[0];
        assert_eq!(ordinary.work().binding.bind_invocations, 1);
        assert_eq!(record.binding.bind_invocations, 1);
        assert_eq!(record.manifest.build_invocations, 1);
        assert_eq!(first.manifest_work().build_invocations, 1);
        assert!(record.issuance.ids_issued > 0);
        assert!(!record.failed);

        let published = first.clone();
        std::thread::spawn(move || assert!(!published.definitions().is_empty()))
            .join()
            .unwrap();
    }

    #[test]
    fn stable_then_ordinary_reuses_the_validation_semantic_entry() {
        let source = base();
        let options = CompileOptions::default();
        let mut session = CompilerSession::new();
        session.update(&source).into_result().unwrap();
        session.stable_definitions(&options).unwrap();
        let semantic_executions = session.work().semantic.executions;
        let ordinary = session.semantic(&options).unwrap();

        assert!(!ordinary.functions().is_empty());
        assert_eq!(semantic_executions, 1);
        assert_eq!(session.work().semantic.executions, 1);
        assert_eq!(session.work().semantic.reuses, 1);
        assert_eq!(session.work().definitions.executions, 1);
        assert_eq!(
            session.work().definition_records[0]
                .binding
                .bind_invocations,
            1
        );
    }

    #[test]
    fn published_queries_support_stable_tooling_lookups() {
        let source = base();
        let options = CompileOptions::default();
        let mut session = CompilerSession::new();
        let published = session.update(&source).into_result().unwrap();

        let module_id = ModuleId::from_logical_path("a.rue").unwrap();
        let module = published.module(&module_id).expect("module by stable ID");
        assert_eq!(module.module_id(), &module_id);
        assert!(
            published
                .module(&ModuleId::from_logical_path("missing.rue").unwrap())
                .is_none()
        );

        let definitions = session.stable_definitions(&options).unwrap();
        let record = &definitions.definitions()[0];
        assert!(std::ptr::eq(
            definitions
                .definition_by_key(record.stable_key())
                .expect("definition by stable key"),
            record
        ));
    }

    #[test]
    fn import_graph_query_reuses_and_recomputes_only_after_resolution_changes() {
        let original = snapshot(
            &[
                (
                    1,
                    "/p/app/main.rue",
                    "app/main.rue",
                    "fn main() -> i32 { let h = @import(\"helper.rue\"); 0 }",
                ),
                (2, "/p/app/helper.rue", "app/helper.rue", "fn helper() {}"),
            ],
            1,
        );
        let relocated = snapshot(
            &[
                (
                    1,
                    "/p/app/main.rue",
                    "app/main.rue",
                    "fn main() -> i32 { let h = @import(\"helper.rue\"); 0 }",
                ),
                (2, "/else/helper.rue", "app/helper.rue", "fn helper() {}"),
            ],
            1,
        );
        let mut session = CompilerSession::new();
        session.update(&original).into_result().unwrap();
        let first = session.import_graph(None).unwrap();
        let reused = session.import_graph(None).unwrap();
        assert!(Arc::ptr_eq(&first, &reused));
        assert!(matches!(
            first.graph().records()[0].resolution(),
            CanonicalImportResolution::Resolved(module) if module.as_str() == "app/helper.rue"
        ));

        let update = session.update(&relocated);
        assert_eq!(update.work().syntax.lexer_invocations, 0);
        assert_eq!(update.work().syntax.parser_invocations, 0);
        assert_eq!(update.work().modules_rebound, 1);
        assert!(update.downstream_invalidated());
        update.into_result().unwrap();
        let moved = session.import_graph(None).unwrap();
        assert!(matches!(
            moved.graph().records()[0].resolution(),
            CanonicalImportResolution::Missing
        ));
        assert_eq!(session.work().imports.calls, 3);
        assert_eq!(session.work().imports.executions, 2);
        assert_eq!(session.work().imports.reuses, 1);
        assert_eq!(session.work().import_entries_invalidated, 1);
        assert_eq!(session.work().merge.executions, 0);
        assert_eq!(session.work().rir.executions, 0);
        assert_eq!(session.work().semantic.executions, 0);
        assert_eq!(session.work().definitions.executions, 0);
    }

    #[test]
    fn import_graph_keys_root_std_context_and_preserves_last_good_graph() {
        let source = snapshot(
            &[
                (
                    1,
                    "/p/main.rue",
                    "main.rue",
                    "fn main() -> i32 { let s = @import(\"std\"); 0 }",
                ),
                (2, "/sdk/_std.rue", "std/_std.rue", "fn helper() {}"),
            ],
            1,
        );
        let other_root = snapshot(
            &[
                (
                    1,
                    "/p/main.rue",
                    "main.rue",
                    "fn main() -> i32 { let s = @import(\"std\"); 0 }",
                ),
                (2, "/sdk/_std.rue", "std/_std.rue", "fn helper() {}"),
            ],
            2,
        );
        let broken = snapshot(&[(1, "/p/main.rue", "main.rue", "fn main( {")], 1);
        let mut session = CompilerSession::new();
        session.update(&source).into_result().unwrap();
        let missing = session.import_graph(None).unwrap();
        let resolved = session.import_graph(Some("/sdk")).unwrap();
        assert!(matches!(
            missing.graph().records()[0].resolution(),
            CanonicalImportResolution::Missing
        ));
        assert!(matches!(
            resolved.graph().records()[0].resolution(),
            CanonicalImportResolution::Resolved(_)
        ));
        assert_eq!(session.work().import_entries, 2);
        assert!(session.update(&broken).result().is_err());
        assert!(Arc::ptr_eq(
            &resolved,
            &session.import_graph(Some("/sdk")).unwrap()
        ));

        session.update(&other_root).into_result().unwrap();
        let rerooted = session.import_graph(Some("/sdk")).unwrap();
        assert_eq!(rerooted.graph().root().as_str(), "std/_std.rue");
        assert_ne!(
            resolved.input().sources.root(),
            rerooted.input().sources.root()
        );
    }

    #[test]
    fn empty_import_graph_is_send_sync_and_concurrently_readable() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<CanonicalImportGraphOutput>();
        let mut session = CompilerSession::new();
        session.update(&base()).into_result().unwrap();
        let graph = session.import_graph(None).unwrap();
        assert!(graph.graph().records().is_empty());
        std::thread::spawn(move || assert!(graph.validation().is_valid()))
            .join()
            .unwrap();
    }

    #[test]
    fn stable_definitions_prefers_a_successful_semantic_variant() {
        let source = base();
        let options = CompileOptions::default();
        let mut session = CompilerSession::new();
        session.update(&source).into_result().unwrap();
        session.semantic(&options).unwrap();

        let mut failed_input = session.semantic_cache[0].input.clone();
        failed_input.opt_level = crate::StableOptLevel::O1;
        session.semantic_cache.insert(
            0,
            SemanticCacheEntry {
                input: failed_input,
                result: Err(CompileErrors::from(CompileError::without_span(
                    ErrorKind::InvalidCompilerInput(
                        "synthetic prior failed opt variant".to_string(),
                    ),
                ))),
            },
        );

        let definitions = session
            .stable_definitions(&CompileOptions {
                opt_level: OptLevel::O2,
                ..options
            })
            .unwrap();

        assert!(!definitions.definitions().is_empty());
        assert_eq!(session.work().semantic.executions, 1);
        assert_eq!(session.work().definitions.executions, 1);
        let record = &session.work().definition_records[0];
        assert_eq!(record.binding.bind_invocations, 1);
        assert_eq!(record.manifest.build_invocations, 1);
    }

    #[test]
    fn dependency_input_manifest_is_stable_ordered_and_adds_no_rir_scan() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<SemanticDependencyInputManifest>();
        let source = base();
        let mut session = CompilerSession::new();
        session.update(&source).into_result().unwrap();
        let first = session
            .semantic_dependency_inputs(&CompileOptions::default(), None)
            .unwrap();
        let second = session
            .semantic_dependency_inputs(&CompileOptions::default(), None)
            .unwrap();
        assert!(Arc::ptr_eq(&first, &second));
        assert!(first.definitions().windows(2).all(|pair| pair[0] < pair[1]));
        assert_eq!(
            first.work().definition_records_visited,
            first.definitions().len()
        );
        assert_eq!(first.work().extra_rir_instructions_visited, 0);
        assert_eq!(session.work().dependency_manifests.executions, 1);
        assert_eq!(session.work().dependency_manifests.reuses, 1);
        assert_eq!(session.work().rir.executions, 1);
        assert_eq!(session.work().semantic.executions, 1);
    }

    #[test]
    fn definition_fingerprints_ignore_relocation_file_ids_and_input_order() {
        let first = snapshot(
            &[
                (7, "/one/main.rue", "main.rue", "fn main() -> i32 { 0 }"),
                (2, "/one/a.rue", "a.rue", "pub fn a() -> i32 { 1 }"),
            ],
            7,
        );
        let relocated = snapshot(
            &[
                (41, "/else/a.rue", "a.rue", "pub fn a() -> i32 { 1 }"),
                (99, "/else/main.rue", "main.rue", "fn main() -> i32 { 0 }"),
            ],
            99,
        );
        let mut left = CompilerSession::new();
        left.update(&first).into_result().unwrap();
        let left = left
            .semantic_dependency_inputs(&CompileOptions::default(), None)
            .unwrap();
        let mut right = CompilerSession::new();
        right.update(&relocated).into_result().unwrap();
        let right = right
            .semantic_dependency_inputs(&CompileOptions::default(), None)
            .unwrap();

        assert_eq!(
            left.definition_fingerprints(),
            right.definition_fingerprints()
        );
        assert!(
            left.definition_fingerprints()
                .iter()
                .all(|fingerprint| fingerprint.schema_version == DEFINITION_FINGERPRINT_SCHEMA_V2)
        );
    }

    #[test]
    fn definition_fingerprints_partition_function_signature_and_body_changes() {
        fn fingerprints(source: &str) -> StableDefinitionInputFingerprint {
            let source = snapshot(&[(7, "/p/main.rue", "main.rue", source)], 7);
            let mut session = CompilerSession::new();
            session.update(&source).into_result().unwrap();
            session
                .semantic_dependency_inputs(&CompileOptions::default(), None)
                .unwrap()
                .definition_fingerprints()
                .iter()
                .find(|fingerprint| fingerprint.key.name() == "value")
                .expect("value definition fingerprint")
                .clone()
        }

        let original = fingerprints("fn value() -> i32 { 0 } fn main() { value(); }");
        let body_changed = fingerprints("fn value() -> i32 { 1 } fn main() { value(); }");
        assert_eq!(original.key, body_changed.key);
        assert_eq!(original.declaration, body_changed.declaration);
        assert_eq!(original.signature, body_changed.signature);
        assert_ne!(
            original.body_or_initializer,
            body_changed.body_or_initializer
        );
        assert_eq!(
            original.precision,
            StableDefinitionFingerprintPrecision::SignatureAndBody
        );

        let visibility_changed = fingerprints("pub fn value() -> i32 { 0 } fn main() { value(); }");
        assert_eq!(original.key, visibility_changed.key);
        assert_ne!(original.declaration, visibility_changed.declaration);
        assert_ne!(original.signature, visibility_changed.signature);
        assert_eq!(
            original.body_or_initializer,
            visibility_changed.body_or_initializer
        );

        let signature_changed = fingerprints("fn value() -> i64 { 0 } fn main() { value(); }");
        assert_eq!(original.declaration, signature_changed.declaration);
        assert_ne!(original.signature, signature_changed.signature);
        assert_eq!(
            original.body_or_initializer,
            signature_changed.body_or_initializer
        );
    }

    #[test]
    fn definition_fingerprints_partition_all_authoritative_named_payloads() {
        fn fingerprint(
            source: &str,
            name: &str,
            kind: StableDefinitionKind,
        ) -> StableDefinitionInputFingerprint {
            let source = snapshot(&[(7, "/p/main.rue", "main.rue", source)], 7);
            let mut session = CompilerSession::new();
            session.update(&source).into_result().unwrap();
            let manifest = session
                .semantic_dependency_inputs(&CompileOptions::default(), None)
                .unwrap();
            manifest
                .definition_fingerprints()
                .iter()
                .find(|value| value.key.name() == name && value.key.kind() == kind)
                .unwrap_or_else(|| {
                    panic!(
                        "missing {name:?} {kind:?}; got {:?}",
                        manifest
                            .definition_fingerprints()
                            .iter()
                            .map(|value| (value.key.name(), value.key.kind()))
                            .collect::<Vec<_>>()
                    )
                })
                .clone()
        }
        fn assert_only_payload_changed(
            before: &StableDefinitionInputFingerprint,
            after: &StableDefinitionInputFingerprint,
            precision: StableDefinitionFingerprintPrecision,
        ) {
            assert_eq!(before.key, after.key);
            assert_eq!(before.declaration, after.declaration);
            assert_eq!(before.signature, after.signature);
            assert_ne!(before.body_or_initializer, after.body_or_initializer);
            assert_eq!(before.precision, precision);
            assert_eq!(after.precision, precision);
        }

        let constant = fingerprint(
            "const answer: i32 = 1; fn main() -> i32 { answer }",
            "answer",
            StableDefinitionKind::ValueConst,
        );
        let constant_changed = fingerprint(
            "const answer: i32 = 2; fn main() -> i32 { answer }",
            "answer",
            StableDefinitionKind::ValueConst,
        );
        assert_only_payload_changed(
            &constant,
            &constant_changed,
            StableDefinitionFingerprintPrecision::SignatureAndInitializer,
        );

        let method = fingerprint(
            "struct S { n: i32, fn get(self) -> i32 { self.n } fn make() -> S { S { n: 1 } } } fn main() -> i32 { S.make().get() }",
            "get",
            StableDefinitionKind::Method,
        );
        let method_changed = fingerprint(
            "struct S { n: i32, fn get(self) -> i32 { self.n + 1 } fn make() -> S { S { n: 1 } } } fn main() -> i32 { S.make().get() }",
            "get",
            StableDefinitionKind::Method,
        );
        assert_only_payload_changed(
            &method,
            &method_changed,
            StableDefinitionFingerprintPrecision::SignatureAndBody,
        );
        let method_owner = fingerprint(
            "struct S { n: i32, fn get(self) -> i32 { self.n } fn make() -> S { S { n: 1 } } } fn main() -> i32 { S.make().get() }",
            "S",
            StableDefinitionKind::Struct,
        );
        let method_owner_after_body_edit = fingerprint(
            "struct S { n: i32, fn get(self) -> i32 { self.n + 1 } fn make() -> S { S { n: 1 } } } fn main() -> i32 { S.make().get() }",
            "S",
            StableDefinitionKind::Struct,
        );
        assert_eq!(method_owner, method_owner_after_body_edit);

        let comptime_function = fingerprint(
            "fn id(comptime value: i32) -> i32 { value } fn main() -> i32 { id(1) }",
            "id",
            StableDefinitionKind::Function,
        );
        let runtime_function = fingerprint(
            "fn id(value: i32) -> i32 { value } fn main() -> i32 { id(1) }",
            "id",
            StableDefinitionKind::Function,
        );
        assert_eq!(comptime_function.declaration, runtime_function.declaration);
        assert_ne!(comptime_function.signature, runtime_function.signature);
        assert_eq!(
            comptime_function.body_or_initializer,
            runtime_function.body_or_initializer
        );

        let destructor = fingerprint(
            "struct S { n: i32 } drop fn S(self) {} fn main() -> i32 { let s = S { n: 1 }; 0 }",
            "S",
            StableDefinitionKind::Destructor,
        );
        let destructor_changed = fingerprint(
            "fn cleanup() {} struct S { n: i32 } drop fn S(self) { cleanup(); } fn main() -> i32 { let s = S { n: 1 }; 0 }",
            "S",
            StableDefinitionKind::Destructor,
        );
        assert_only_payload_changed(
            &destructor,
            &destructor_changed,
            StableDefinitionFingerprintPrecision::SignatureAndBody,
        );

        let structure = fingerprint(
            "struct S { n: i32 } fn main() -> i32 { 0 }",
            "S",
            StableDefinitionKind::Struct,
        );
        let structure_changed = fingerprint(
            "struct S { n: i64 } fn main() -> i32 { 0 }",
            "S",
            StableDefinitionKind::Struct,
        );
        assert_eq!(
            structure.precision,
            StableDefinitionFingerprintPrecision::ExactSignature
        );
        assert_ne!(structure.signature, structure_changed.signature);
        assert_eq!(structure.body_or_initializer, None);

        let enumeration = fingerprint(
            "enum E { A(i32), B } fn main() -> i32 { 0 }",
            "E",
            StableDefinitionKind::Enum,
        );
        let enumeration_changed = fingerprint(
            "enum E { A(i64), B } fn main() -> i32 { 0 }",
            "E",
            StableDefinitionKind::Enum,
        );
        assert_eq!(
            enumeration.precision,
            StableDefinitionFingerprintPrecision::ExactSignature
        );
        assert_ne!(enumeration.signature, enumeration_changed.signature);
        assert_eq!(enumeration.body_or_initializer, None);
    }

    fn synthetic_complete_manifest(
        manifest: &SemanticDependencyInputManifest,
    ) -> Arc<SemanticDependencyInputManifest> {
        let mut manifest = manifest.clone();
        manifest.dependency_blockers = Arc::from([]);
        manifest.definition_universe_complete = true;
        Arc::new(manifest)
    }

    fn definition_names(keys: &[StableDefinitionKey]) -> Vec<&str> {
        keys.iter().map(|key| key.name()).collect()
    }

    #[test]
    fn production_invalidation_is_cached_incremental_and_closes_reverse_dependencies_without_rir_work()
     {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<SemanticInvalidationPlan>();
        let source = snapshot(
            &[(
                7,
                "/p/main.rue",
                "main.rue",
                "fn leaf() -> i32 { 1 } fn main() -> i32 { leaf() }",
            )],
            7,
        );
        let changed = snapshot(
            &[(
                7,
                "/p/main.rue",
                "main.rue",
                "fn leaf() -> i32 { 2 } fn main() -> i32 { leaf() }",
            )],
            7,
        );
        let build = |source: &SourceSnapshot| {
            let mut session = CompilerSession::new();
            session.update(source).into_result().unwrap();
            session
                .semantic_dependency_inputs(&CompileOptions::default(), None)
                .unwrap()
        };
        let previous = build(&source);
        let current = build(&changed);
        let mut planner = CompilerSession::new();
        let first = planner.semantic_invalidation_plan(&previous, &current);
        let second = planner.semantic_invalidation_plan(&previous, &current);
        assert!(Arc::ptr_eq(&first, &second));
        assert_eq!(first.scope(), &SemanticInvalidationScope::Incremental);
        assert!(previous.dependency_blockers().is_empty());
        assert!(first.reusable().is_empty());
        assert_eq!(definition_names(first.invalidated()), vec!["leaf", "main"]);
        assert_eq!(definition_names(first.changed()), vec!["leaf"]);
        assert_eq!(first.work().dependency_edges_visited, 2);
        assert_eq!(first.work().reverse_closure_nodes_visited, 2);
        assert_eq!(first.work().extra_rir_instructions_visited, 0);
        assert_eq!(planner.work().invalidation_plans.executions, 1);
        assert_eq!(planner.work().invalidation_plans.reuses, 1);
        assert_eq!(planner.work().rir.executions, 0);

        let noop = planner.semantic_invalidation_plan(&previous, &previous);
        assert_eq!(noop.scope(), &SemanticInvalidationScope::Incremental);
        assert!(noop.changed().is_empty());
        assert!(noop.invalidated().is_empty());
        assert_eq!(definition_names(noop.reusable()), vec!["leaf", "main"]);
        assert_eq!(noop.work().extra_rir_instructions_visited, 0);
    }

    #[test]
    fn production_invalidation_closes_cross_module_edges_and_ignores_relocation_and_order() {
        let build = |main_id, lib_id, root: &str, reversed: bool, leaf_value: i32| {
            let main = (
                main_id,
                format!("{root}/main.rue"),
                "main.rue",
                r#"const lib = @import("lib.rue");
                   fn main() -> i32 { lib.leaf() }"#
                    .to_owned(),
            );
            let lib = (
                lib_id,
                format!("{root}/lib.rue"),
                "lib.rue",
                format!("pub fn leaf() -> i32 {{ {leaf_value} }}"),
            );
            let owned = if reversed {
                vec![lib, main]
            } else {
                vec![main, lib]
            };
            let entries = owned
                .iter()
                .map(|(id, path, module, text)| (*id, path.as_str(), *module, text.as_str()))
                .collect::<Vec<_>>();
            let source = snapshot(&entries, main_id);
            let mut session = CompilerSession::new();
            session.update(&source).into_result().unwrap();
            session
                .semantic_dependency_inputs(&CompileOptions::default(), None)
                .unwrap()
        };
        let previous = build(3, 8, "/one", false, 1);
        let relocated = build(91, 4, "/elsewhere", true, 1);
        let changed = build(91, 4, "/elsewhere", true, 2);
        let mut planner = CompilerSession::new();

        let moved = planner.semantic_invalidation_plan(&previous, &relocated);
        assert_eq!(moved.scope(), &SemanticInvalidationScope::Incremental);
        assert!(moved.invalidated().is_empty());
        assert_eq!(
            definition_names(moved.reusable()),
            vec!["leaf", "main", "lib"]
        );

        let plan = planner.semantic_invalidation_plan(&relocated, &changed);
        assert_eq!(plan.scope(), &SemanticInvalidationScope::Incremental);
        assert_eq!(definition_names(plan.changed()), vec!["leaf"]);
        assert_eq!(definition_names(plan.invalidated()), vec!["leaf", "main"]);
        assert_eq!(definition_names(plan.reusable()), vec!["lib"]);
        assert_eq!(plan.work().extra_rir_instructions_visited, 0);
        assert_eq!(planner.work().rir.executions, 0);
    }

    #[test]
    fn synthetic_complete_invalidation_computes_exact_delta_and_reverse_closure() {
        let build = |text: &str| {
            let source = snapshot(&[(7, "/p/main.rue", "main.rue", text)], 7);
            let mut session = CompilerSession::new();
            session.update(&source).into_result().unwrap();
            let manifest = session
                .semantic_dependency_inputs(&CompileOptions::default(), None)
                .unwrap();
            synthetic_complete_manifest(&manifest)
        };
        let previous = build(
            "fn leaf() -> i32 { 1 } fn middle() -> i32 { leaf() } fn main() -> i32 { middle() }",
        );
        let current = build(
            "fn leaf() -> i32 { 2 } fn middle() -> i32 { leaf() } fn main() -> i32 { middle() }",
        );
        let mut session = CompilerSession::new();
        let plan = session.semantic_invalidation_plan(&previous, &current);
        assert_eq!(plan.scope(), &SemanticInvalidationScope::Incremental);
        assert_eq!(definition_names(plan.changed()), vec!["leaf"]);
        assert_eq!(
            definition_names(plan.invalidated()),
            vec!["leaf", "main", "middle"]
        );
        assert!(plan.reusable().is_empty());
        assert_eq!(plan.work().definition_fingerprints_compared, 3);
        assert_eq!(plan.work().dependency_edges_visited, 4);
        assert_eq!(plan.work().reverse_closure_nodes_visited, 3);
        assert_eq!(plan.work().extra_rir_instructions_visited, 0);

        let removed_added = build(
            "fn new_leaf() -> i32 { 1 } fn middle() -> i32 { new_leaf() } fn main() -> i32 { middle() }",
        );
        let plan = session.semantic_invalidation_plan(&current, &removed_added);
        assert_eq!(definition_names(plan.added()), vec!["new_leaf"]);
        assert_eq!(definition_names(plan.removed()), vec!["leaf"]);
    }

    #[test]
    fn planner_ignores_relocation_but_rejects_global_semantic_input_changes() {
        let original = snapshot(
            &[
                (7, "/one/main.rue", "main.rue", "fn main() -> i32 { 0 }"),
                (2, "/one/a.rue", "a.rue", "fn a() -> i32 { 1 }"),
            ],
            7,
        );
        let relocated = snapshot(
            &[
                (41, "/else/a.rue", "a.rue", "fn a() -> i32 { 1 }"),
                (99, "/else/main.rue", "main.rue", "fn main() -> i32 { 0 }"),
            ],
            99,
        );
        let build = |source: &SourceSnapshot, options: &CompileOptions| {
            let mut session = CompilerSession::new();
            session.update(source).into_result().unwrap();
            session.semantic_dependency_inputs(options, None).unwrap()
        };
        let previous = build(&original, &CompileOptions::default());
        let moved = build(&relocated, &CompileOptions::default());
        let mut planner = CompilerSession::new();
        let plan = planner.semantic_invalidation_plan(&previous, &moved);
        assert_eq!(plan.scope(), &SemanticInvalidationScope::Incremental);
        assert!(plan.invalidated().is_empty());
        assert_eq!(plan.reusable().len(), 2);

        let alternative_target = *Target::all()
            .iter()
            .find(|&&target| target != moved.input().target)
            .expect("at least one supported target differs from the current target");
        assert_ne!(alternative_target, moved.input().target);
        let target = build(
            &relocated,
            &CompileOptions {
                target: alternative_target,
                ..CompileOptions::default()
            },
        );
        assert!(matches!(
            planner.semantic_invalidation_plan(&moved, &target).scope(),
            SemanticInvalidationScope::Full { reasons }
                if reasons.contains(&SemanticFullInvalidationReason::TargetChanged)
        ));
        let features = build(
            &relocated,
            &CompileOptions {
                preview_features: PreviewFeatures::from([PreviewFeature::TestInfra]),
                ..CompileOptions::default()
            },
        );
        assert!(matches!(
            planner.semantic_invalidation_plan(&moved, &features).scope(),
            SemanticInvalidationScope::Full { reasons }
                if reasons.contains(&SemanticFullInvalidationReason::PreviewFeaturesChanged)
        ));

        let mut root_changed = (*moved).clone();
        root_changed.input.sources = SourceRevision::new(
            ModuleId::from_logical_path("a.rue").unwrap(),
            root_changed.input.sources.modules().to_vec(),
        )
        .unwrap();
        let root_changed = Arc::new(root_changed);
        assert!(matches!(
            planner
                .semantic_invalidation_plan(&moved, &root_changed)
                .scope(),
            SemanticInvalidationScope::Full { reasons }
                if reasons.contains(&SemanticFullInvalidationReason::RootChanged)
        ));

        let mut imports_changed = (*moved).clone();
        imports_changed.module_imports = vec![StableModuleImportDependency::Missing {
            importer: ModuleId::from_logical_path("main.rue").unwrap(),
            normalized_specifier: Arc::from("a.rue"),
        }]
        .into();
        let imports_changed = Arc::new(imports_changed);
        let plan = planner.semantic_invalidation_plan(&moved, &imports_changed);
        assert!(matches!(
            plan.scope(),
            SemanticInvalidationScope::Full { reasons }
                if reasons.contains(&SemanticFullInvalidationReason::ModuleImportsChanged)
        ));
        assert!(plan.reusable().is_empty());
    }

    #[test]
    fn dependency_manifest_carries_resolved_and_fail_closed_module_edges() {
        let original = snapshot(
            &[
                (
                    1,
                    "/p/app/main.rue",
                    "app/main.rue",
                    "fn main() -> i32 { let h = @import(\"helper.rue\"); 0 }",
                ),
                (2, "/p/app/helper.rue", "app/helper.rue", "fn helper() {}"),
            ],
            1,
        );
        let moved = snapshot(
            &[
                (
                    1,
                    "/p/app/main.rue",
                    "app/main.rue",
                    "fn main() -> i32 { let h = @import(\"helper.rue\"); 0 }",
                ),
                (2, "/else/helper.rue", "app/helper.rue", "fn helper() {}"),
            ],
            1,
        );
        let mut session = CompilerSession::new();
        session.update(&original).into_result().unwrap();
        let resolved = session
            .semantic_dependency_inputs(&CompileOptions::default(), None)
            .unwrap();
        assert!(resolved.definition_universe_complete());
        assert!(matches!(
            &resolved.module_imports()[0],
            StableModuleImportDependency::Resolved { importer, target, .. }
                if importer.as_str() == "app/main.rue" && target.as_str() == "app/helper.rue"
        ));

        let update = session.update(&moved);
        assert_eq!(update.work().syntax.lexer_invocations, 0);
        update.into_result().unwrap();
        let missing = session
            .semantic_dependency_inputs(&CompileOptions::default(), None)
            .unwrap();
        assert!(!missing.definition_universe_complete());
        assert!(matches!(
            &missing.module_imports()[0],
            StableModuleImportDependency::Missing { importer, .. }
                if importer.as_str() == "app/main.rue"
        ));
        assert_eq!(missing.work().import_records_visited, 1);
        assert_eq!(missing.work().extra_rir_instructions_visited, 0);
        assert_eq!(session.work().dependency_manifests.executions, 2);
    }

    fn stable_edge_names(
        manifest: &SemanticDependencyInputManifest,
    ) -> Vec<(String, String, String, String)> {
        manifest
            .free_function_dependencies()
            .iter()
            .map(|edge| {
                (
                    edge.caller.module().as_str().to_owned(),
                    edge.caller.name().to_owned(),
                    edge.callee.module().as_str().to_owned(),
                    edge.callee.name().to_owned(),
                )
            })
            .collect()
    }

    #[test]
    fn specialized_free_function_edges_are_stable_and_deduplicate_instances() {
        let first = snapshot(
            &[
                (
                    9,
                    "/one/main.rue",
                    "main.rue",
                    r#"const lib = @import("lib.rue");
                       fn main() -> i32 { lib.wrap(1, 10) + lib.wrap(2, 20) }"#,
                ),
                (
                    3,
                    "/one/lib.rue",
                    "lib.rue",
                    r#"fn leaf(value: i32) -> i32 { value }
                       fn inner(comptime n: i32, value: i32) -> i32 { leaf(value) + n }
                       pub fn wrap(comptime n: i32, value: i32) -> i32 { inner(n, value) }"#,
                ),
            ],
            9,
        );
        let moved = snapshot(
            &[
                (
                    41,
                    "/else/lib.rue",
                    "lib.rue",
                    r#"fn leaf(value: i32) -> i32 { value }
                       fn inner(comptime n: i32, value: i32) -> i32 { leaf(value) + n }
                       pub fn wrap(comptime n: i32, value: i32) -> i32 { inner(n, value) }"#,
                ),
                (
                    7,
                    "/else/main.rue",
                    "main.rue",
                    r#"const lib = @import("lib.rue");
                       fn main() -> i32 { lib.wrap(1, 10) + lib.wrap(2, 20) }"#,
                ),
            ],
            7,
        );
        let build = |source: &SourceSnapshot| {
            let mut session = CompilerSession::new();
            session.update(source).into_result().unwrap();
            session
                .semantic_dependency_inputs(&CompileOptions::default(), None)
                .unwrap()
        };
        let first = build(&first);
        let moved = build(&moved);
        assert_eq!(stable_edge_names(&first), stable_edge_names(&moved));
        assert_eq!(
            stable_edge_names(&first),
            vec![
                (
                    "lib.rue".into(),
                    "inner".into(),
                    "lib.rue".into(),
                    "leaf".into()
                ),
                (
                    "lib.rue".into(),
                    "wrap".into(),
                    "lib.rue".into(),
                    "inner".into()
                ),
                (
                    "main.rue".into(),
                    "main".into(),
                    "lib.rue".into(),
                    "wrap".into()
                ),
            ]
        );
        assert!(first.free_function_caller_dependencies_complete());
        assert!(first.semantic_dependency_graph_complete());
        assert_eq!(first.work().specialization_origins_validated, 4);
        assert_eq!(first.work().free_function_events_translated, 5);
        assert_eq!(first.work().extra_rir_instructions_visited, 0);
    }

    #[test]
    fn recursive_specialization_edges_and_renames_are_exact() {
        let source = snapshot(
            &[(
                1,
                "/p/main.rue",
                "main.rue",
                r#"fn leaf(value: i32) -> i32 { value }
                   fn fib(comptime n: i32) -> i32 {
                       if n < 2 { leaf(n) } else { fib(n - 1) + fib(n - 2) }
                   }
                   fn main() -> i32 { fib(5) + fib(5) }"#,
            )],
            1,
        );
        let mut session = CompilerSession::new();
        session.update(&source).into_result().unwrap();
        let manifest = session
            .semantic_dependency_inputs(&CompileOptions::default(), None)
            .unwrap();
        assert_eq!(
            stable_edge_names(&manifest),
            vec![
                (
                    "main.rue".into(),
                    "fib".into(),
                    "main.rue".into(),
                    "fib".into()
                ),
                (
                    "main.rue".into(),
                    "fib".into(),
                    "main.rue".into(),
                    "leaf".into()
                ),
                (
                    "main.rue".into(),
                    "main".into(),
                    "main.rue".into(),
                    "fib".into()
                ),
            ]
        );
        assert_eq!(manifest.work().specialization_origins_validated, 6);
        assert!(manifest.work().free_function_events_translated > 3);

        let renamed = snapshot(
            &[(
                1,
                "/p/main.rue",
                "main.rue",
                r#"fn terminal(value: i32) -> i32 { value }
                   fn fib(comptime n: i32) -> i32 {
                       if n < 2 { terminal(n) } else { fib(n - 1) + fib(n - 2) }
                   }
                   fn main() -> i32 { fib(5) + fib(5) }"#,
            )],
            1,
        );
        session.update(&renamed).into_result().unwrap();
        let renamed = session
            .semantic_dependency_inputs(&CompileOptions::default(), None)
            .unwrap();
        assert!(
            stable_edge_names(&renamed)
                .iter()
                .any(|edge| edge.3 == "terminal")
        );
        assert!(
            !stable_edge_names(&renamed)
                .iter()
                .any(|edge| edge.3 == "leaf")
        );
    }

    #[test]
    fn dependency_endpoint_translation_fails_closed_for_missing_and_non_functions() {
        let source = snapshot(
            &[(
                1,
                "/p/main.rue",
                "main.rue",
                "const answer: i32 = 42; fn main() -> i32 { answer }",
            )],
            1,
        );
        let mut session = CompilerSession::new();
        session.update(&source).into_result().unwrap();
        let definitions = session
            .stable_definitions(&CompileOptions::default())
            .unwrap();
        assert!(stable_free_function_endpoint(&definitions, 1, "missing").is_err());
        assert!(stable_free_function_endpoint(&definitions, 1, "answer").is_err());
        assert!(stable_named_method_endpoint(&definitions, 1, "answer", "answer").is_err());

        let rejected = snapshot(
            &[(1, "/p/main.rue", "main.rue", "fn main() -> i32 { true }")],
            1,
        );
        session.update(&rejected).into_result().unwrap();
        let manifest = session
            .semantic_dependency_inputs(&CompileOptions::default(), None)
            .unwrap();
        assert!(!manifest.definition_universe_complete());
        assert!(!manifest.free_function_caller_dependencies_complete());
        assert!(manifest.free_function_dependencies().is_empty());
    }

    #[test]
    fn stable_named_method_dependency_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<StableNamedMethodDependency>();
        assert_send_sync::<StableNamedMethodDependencyTarget>();
        assert_send_sync::<StableNamedConstDependency>();
        assert_send_sync::<StableNamedConstDependencyTarget>();
        assert_send_sync::<StableBodyDependencyInputRecord>();
    }

    #[test]
    fn sibling_generic_owners_stay_distinct_and_non_function_calls_are_excluded() {
        let source = snapshot(
            &[
                (
                    1,
                    "/p/main.rue",
                    "main.rue",
                    r#"const left = @import("left.rue");
                       const right = @import("right.rue");
                       fn main() -> i32 { left.id(1) + right.id(2) }"#,
                ),
                (
                    2,
                    "/p/left.rue",
                    "left.rue",
                    r#"struct Box { value: i32, fn get(borrow self) -> i32 { self.value } }
                       fn leaf(value: i32) -> i32 { value }
                       pub fn id(comptime n: i32) -> i32 {
                           let value = Box { value: n };
                           @dbg(n);
                           leaf(value.get())
                       }"#,
                ),
                (
                    3,
                    "/p/right.rue",
                    "right.rue",
                    r#"fn leaf(value: i32) -> i32 { value }
                       pub fn id(comptime n: i32) -> i32 { leaf(n) }"#,
                ),
            ],
            1,
        );
        let mut session = CompilerSession::new();
        session.update(&source).into_result().unwrap();
        let manifest = session
            .semantic_dependency_inputs(&CompileOptions::default(), None)
            .unwrap();
        assert_eq!(
            stable_edge_names(&manifest),
            vec![
                (
                    "left.rue".into(),
                    "id".into(),
                    "left.rue".into(),
                    "leaf".into()
                ),
                (
                    "main.rue".into(),
                    "main".into(),
                    "left.rue".into(),
                    "id".into()
                ),
                (
                    "main.rue".into(),
                    "main".into(),
                    "right.rue".into(),
                    "id".into()
                ),
                (
                    "right.rue".into(),
                    "id".into(),
                    "right.rue".into(),
                    "leaf".into()
                ),
            ]
        );
        assert!(
            manifest
                .free_function_dependencies()
                .iter()
                .all(|edge| { edge.callee.name() != "get" && edge.callee.name() != "Box" })
        );
    }

    fn named_method_edge_names(
        manifest: &SemanticDependencyInputManifest,
    ) -> Vec<(String, String, String, String, String, String)> {
        manifest
            .named_method_dependencies()
            .iter()
            .map(|edge| {
                let caller_owner = edge.caller.owner().unwrap().name().to_owned();
                match &edge.target {
                    StableNamedMethodDependencyTarget::FreeFunction(target) => (
                        edge.caller.module().as_str().to_owned(),
                        caller_owner,
                        edge.caller.name().to_owned(),
                        "free".to_owned(),
                        target.module().as_str().to_owned(),
                        target.name().to_owned(),
                    ),
                    StableNamedMethodDependencyTarget::NamedMethod(target) => (
                        edge.caller.module().as_str().to_owned(),
                        caller_owner,
                        edge.caller.name().to_owned(),
                        target.owner().unwrap().name().to_owned(),
                        target.module().as_str().to_owned(),
                        target.name().to_owned(),
                    ),
                }
            })
            .collect()
    }

    #[test]
    fn named_method_edges_are_stable_exact_and_normalize_generic_free_callees() {
        let program = r#"fn helper() -> i32 { 1 }
            fn generic(comptime n: i32) -> i32 { helper() + n }
            struct B { value: i32, fn ping(borrow self) -> i32 { helper() + self.value } }
            struct A {
                value: i32,
                fn run(borrow self) -> i32 {
                    let b = B { value: self.value };
                    b.ping() + self.next()
                }
                fn next(borrow self) -> i32 { generic(2) + self.run() }
            }
            fn main() -> i32 { let a = A { value: 1 }; a.run() }"#;
        let first_source = snapshot(&[(9, "/one/main.rue", "main.rue", program)], 9);
        let moved_source = snapshot(&[(41, "/else/main.rue", "main.rue", program)], 41);
        let build = |source: &SourceSnapshot| {
            let mut session = CompilerSession::new();
            session.update(source).into_result().unwrap();
            session
                .semantic_dependency_inputs(&CompileOptions::default(), None)
                .unwrap()
        };
        let first = build(&first_source);
        let moved = build(&moved_source);
        assert_eq!(
            named_method_edge_names(&first),
            named_method_edge_names(&moved)
        );
        let edges = named_method_edge_names(&first);
        for expected in [
            ("main.rue", "A", "run", "B", "main.rue", "ping"),
            ("main.rue", "A", "run", "A", "main.rue", "next"),
            ("main.rue", "A", "next", "A", "main.rue", "run"),
            ("main.rue", "A", "next", "free", "main.rue", "generic"),
            ("main.rue", "B", "ping", "free", "main.rue", "helper"),
        ] {
            assert!(
                edges.contains(&(
                    expected.0.into(),
                    expected.1.into(),
                    expected.2.into(),
                    expected.3.into(),
                    expected.4.into(),
                    expected.5.into(),
                )),
                "missing {expected:?} from {edges:?}"
            );
        }
        assert!(first.non_generic_named_method_dependencies_complete());
        assert!(first.generic_named_method_dependencies_complete());
        assert!(!first.dependency_blockers().iter().any(|blocker| {
            blocker.surface() == SemanticDependencySurface::GenericNamedMethodCall
                && blocker.reason()
                    == SemanticDependencyIncompleteReason::GenericSubstitutionIdentityUnavailable
        }));
        assert_eq!(first.work().named_method_events_translated, edges.len());
        assert_eq!(first.work().extra_rir_instructions_visited, 0);
    }

    #[test]
    fn generic_named_method_uses_single_body_caller_and_needs_no_substitution_blocker() {
        let program = "fn helper() -> i32 { 1 } struct Value { fn choose(borrow self, comptime n: i32) -> i32 { helper() + n } } fn main() -> i32 { let value = Value {}; value.choose(1) + value.choose(2) }";
        let first_source = snapshot(&[(7, "/one/main.rue", "main.rue", program)], 7);
        let moved_source = snapshot(&[(71, "/else/main.rue", "main.rue", program)], 71);
        let build = |source: &SourceSnapshot| {
            let mut session = CompilerSession::new();
            session.update(source).into_result().unwrap();
            session
                .semantic_dependency_inputs(&CompileOptions::default(), None)
                .unwrap()
        };
        let first = build(&first_source);
        let moved = build(&moved_source);
        assert_eq!(
            named_method_edge_names(&first),
            named_method_edge_names(&moved)
        );
        assert_eq!(
            named_method_edge_names(&first),
            vec![(
                "main.rue".into(),
                "Value".into(),
                "choose".into(),
                "free".into(),
                "main.rue".into(),
                "helper".into(),
            )]
        );
        assert!(first.generic_named_method_dependencies_complete());
        assert!(!first.dependency_blockers().iter().any(|blocker| {
            blocker.reason()
                == SemanticDependencyIncompleteReason::GenericSubstitutionIdentityUnavailable
        }));
        assert_eq!(first.work().named_method_events_translated, 1);
        let choose = first
            .body_dependencies()
            .iter()
            .find(|record| record.owner().name() == "choose")
            .expect("analyzed named method has one body input record");
        assert!(!choose.reusable_boundary_supported());
        assert!(choose.blockers().iter().any(|blocker| {
            blocker.owner() == Some(choose.owner())
                && blocker.reason()
                    == SemanticDependencyIncompleteReason::GenericSubstitutionIdentityUnavailable
        }));
        assert!(
            first
                .body_dependency_blockers()
                .contains(&choose.blockers()[0])
        );
        assert_eq!(first.work().extra_rir_instructions_visited, 0);
    }

    #[test]
    fn ordinary_body_inputs_are_stable_complete_and_per_owner() {
        let program =
            "fn leaf() -> i32 { 1 } fn middle() -> i32 { leaf() } fn main() -> i32 { middle() }";
        let build = |file, path: &str| {
            let source = snapshot(&[(file, path, "main.rue", program)], file);
            let mut session = CompilerSession::new();
            session.update(&source).into_result().unwrap();
            session
                .semantic_dependency_inputs(&CompileOptions::default(), None)
                .unwrap()
        };
        let first = build(1, "/one/main.rue");
        let moved = build(99, "/else/main.rue");
        assert_eq!(first.body_dependencies(), moved.body_dependencies());
        assert_eq!(first.body_dependencies().len(), 3);
        let dependency_names = |owner: &str| {
            first
                .body_dependencies()
                .iter()
                .find(|record| record.owner().name() == owner)
                .unwrap()
                .direct_dependency_inputs()
                .iter()
                .map(|dependency| dependency.key.name().to_owned())
                .collect::<Vec<_>>()
        };
        assert_eq!(dependency_names("leaf"), Vec::<String>::new());
        assert_eq!(dependency_names("middle"), vec!["leaf"]);
        assert_eq!(dependency_names("main"), vec!["middle"]);
        assert!(first.body_dependencies().iter().all(|record| {
            record.reusable_boundary_supported()
                && record.fingerprint().body_or_initializer.is_some()
                && record.target() == crate::Target::default()
                && record.preview_features()
                    == &StablePreviewFeatures::new(&crate::PreviewFeatures::default())
        }));
        assert_eq!(first.work().body_owner_events_translated, 3);
        assert_eq!(first.work().body_dependency_records_built, 3);
        assert_eq!(first.work().extra_rir_instructions_visited, 0);
    }

    #[test]
    fn body_only_named_type_and_const_inputs_are_exact_and_relocation_stable() {
        let program = "struct Point { x: i32 } const ANSWER: i32 = 42; fn main() -> i32 { let p = Point { x: ANSWER }; p.x }";
        let build = |file, path: &str| {
            let source = snapshot(&[(file, path, "main.rue", program)], file);
            let mut session = CompilerSession::new();
            session.update(&source).into_result().unwrap();
            session
                .semantic_dependency_inputs(&CompileOptions::default(), None)
                .unwrap()
        };
        let first = build(7, "/one/main.rue");
        let moved = build(91, "/else/main.rue");
        assert_eq!(first.body_dependencies(), moved.body_dependencies());
        let main = first
            .body_dependencies()
            .iter()
            .find(|record| record.owner().name() == "main")
            .unwrap();
        let dependencies = main
            .direct_dependency_inputs()
            .iter()
            .map(|dependency| dependency.key.name())
            .collect::<BTreeSet<_>>();
        assert_eq!(dependencies, BTreeSet::from(["ANSWER", "Point"]));
        assert!(main.reusable_boundary_supported());
        assert_eq!(first.work().extra_rir_instructions_visited, 0);
    }

    #[test]
    fn body_owner_join_disambiguates_duplicate_names_across_modules_and_order() {
        let main = r#"const lib = @import("lib.rue");
                       const other = @import("other.rue");
                       fn main() -> i32 { lib.BASE + other.BASE }"#;
        let first_source = snapshot(
            &[
                (3, "/p/main.rue", "main.rue", main),
                (9, "/p/lib.rue", "lib.rue", "pub const BASE: i32 = 4;"),
                (11, "/p/other.rue", "other.rue", "pub const BASE: i32 = 5;"),
            ],
            3,
        );
        let moved_source = snapshot(
            &[
                (
                    81,
                    "/else/other.rue",
                    "other.rue",
                    "pub const BASE: i32 = 5;",
                ),
                (77, "/else/lib.rue", "lib.rue", "pub const BASE: i32 = 4;"),
                (99, "/else/main.rue", "main.rue", main),
            ],
            99,
        );
        let build = |source: &SourceSnapshot| {
            let mut session = CompilerSession::new();
            session.update(source).into_result().unwrap();
            session
                .semantic_dependency_inputs(&CompileOptions::default(), None)
                .unwrap()
        };
        let first = build(&first_source);
        let moved = build(&moved_source);
        assert_eq!(first.body_dependencies(), moved.body_dependencies());
        let main = first
            .body_dependencies()
            .iter()
            .find(|record| record.owner().name() == "main")
            .unwrap();
        let dependencies = main
            .direct_dependency_inputs()
            .iter()
            .map(|dependency| (dependency.key.module().as_str(), dependency.key.name()))
            .collect::<BTreeSet<_>>();
        assert_eq!(
            dependencies,
            BTreeSet::from([
                ("lib.rue", "BASE"),
                ("main.rue", "lib"),
                ("main.rue", "other"),
                ("other.rue", "BASE"),
            ])
        );
        assert_eq!(first.work().extra_rir_instructions_visited, 0);
    }

    #[test]
    fn named_destructor_edges_translate_to_stable_owner_and_target() {
        let program = "fn cleanup() {} struct Value { n: i32 } drop fn Value(self) { cleanup(); } fn main() -> i32 { let value = Value { n: 1 }; 0 }";
        let first_source = snapshot(&[(3, "/one/main.rue", "main.rue", program)], 3);
        let moved_source = snapshot(&[(71, "/else/main.rue", "main.rue", program)], 71);
        let build = |source: &SourceSnapshot| {
            let mut session = CompilerSession::new();
            session.update(source).into_result().unwrap();
            session
                .semantic_dependency_inputs(&CompileOptions::default(), None)
                .unwrap()
        };
        let first = build(&first_source);
        let moved = build(&moved_source);
        assert_eq!(
            first.named_destructor_dependencies(),
            moved.named_destructor_dependencies()
        );
        let [edge] = first.named_destructor_dependencies() else {
            panic!("expected one destructor dependency");
        };
        assert_eq!(edge.caller.owner().unwrap().name(), "Value");
        assert_eq!(edge.caller.kind(), StableDefinitionKind::Destructor);
        let StableNamedMethodDependencyTarget::FreeFunction(target) = &edge.target else {
            panic!("cleanup is a free function");
        };
        assert_eq!(target.name(), "cleanup");
        assert!(first.named_destructor_dependencies_complete());
        assert_eq!(first.work().named_destructor_events_translated, 1);
        assert_eq!(first.work().extra_rir_instructions_visited, 0);
    }

    #[test]
    fn implicit_drop_edges_distinguish_body_obligations_from_global_glue() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<StableImplicitNamedDestructorDependency>();
        let program = r#"
            struct Leaf { n: i32 }
            drop fn Leaf(self) {}
            struct Wrapper { leaves: [Leaf; 2] }
            fn consume() { let value = Wrapper { leaves: [Leaf { n: 1 }, Leaf { n: 2 }] }; }
            fn main() { consume(); }
        "#;
        let build = |id, physical: &str| {
            let source = snapshot(&[(id, physical, "main.rue", program)], id);
            let mut session = CompilerSession::new();
            session.update(&source).into_result().unwrap();
            session
                .semantic_dependency_inputs(&CompileOptions::default(), None)
                .unwrap()
        };
        let first = build(4, "/one/main.rue");
        let relocated = build(97, "/else/main.rue");
        assert_eq!(
            first.implicit_named_destructor_dependencies(),
            relocated.implicit_named_destructor_dependencies()
        );
        let names = first
            .implicit_named_destructor_dependencies()
            .iter()
            .map(|edge| {
                (
                    edge.source.kind(),
                    edge.source.name().to_string(),
                    edge.target.owner().unwrap().name().to_string(),
                )
            })
            .collect::<Vec<_>>();
        assert!(names.contains(&(
            StableDefinitionKind::Function,
            "consume".into(),
            "Leaf".into(),
        )));
        assert!(names.contains(&(StableDefinitionKind::Struct, "Leaf".into(), "Leaf".into(),)));
        assert!(first.implicit_named_destructor_dependencies_complete());
        assert_eq!(
            first.work().implicit_named_destructor_events_translated,
            first.implicit_named_destructor_dependencies().len()
        );
        assert_eq!(first.work().extra_rir_instructions_visited, 0);
    }

    #[test]
    fn anonymous_drop_owner_is_an_exact_production_completeness_blocker() {
        let source = snapshot(
            &[(
                4,
                "/p/main.rue",
                "main.rue",
                r#"
                    fn Box(comptime T: type) -> type {
                        struct { v: T, drop fn(self) {} }
                    }
                    fn main() { let B = Box(i32); let value = B { v: 1 }; }
                "#,
            )],
            4,
        );
        let mut session = CompilerSession::new();
        session.update(&source).into_result().unwrap();
        let manifest = session
            .semantic_dependency_inputs(&CompileOptions::default(), None)
            .unwrap();
        let blocker = manifest
            .dependency_blockers()
            .iter()
            .find(|blocker| blocker.surface() == SemanticDependencySurface::ImplicitNamedDestructor)
            .expect("anonymous drop source must fail closed");
        assert_eq!(blocker.owner(), None);
        assert_eq!(
            blocker.reason(),
            SemanticDependencyIncompleteReason::AnonymousDropOwnerUnavailable
        );
        assert!(!manifest.implicit_named_destructor_dependencies_complete());
        assert!(manifest.body_dependency_blockers().iter().any(|blocker| {
            blocker.owner().is_none()
                && blocker.surface() == SemanticDependencySurface::BodyOwner
                && blocker.reason()
                    == SemanticDependencyIncompleteReason::AnonymousBodyOwnerUnavailable
        }));
        assert_eq!(manifest.work().extra_rir_instructions_visited, 0);
        let plan = session.semantic_invalidation_plan(&manifest, &manifest);
        assert!(matches!(
            plan.scope(),
            SemanticInvalidationScope::Full { reasons }
                if reasons.as_ref() == [SemanticFullInvalidationReason::IncompleteDependencyGraph(
                    Arc::from([blocker.clone()]),
                )]
        ));
        assert!(plan.reusable().is_empty());
        assert!(plan.invalidated().is_empty());
        assert_eq!(plan.work().extra_rir_instructions_visited, 0);
    }

    #[test]
    fn resolved_declaration_type_edges_translate_without_rir_rescan() {
        let source = snapshot(
            &[(
                1,
                "/p/main.rue",
                "main.rue",
                r#"
                struct Leaf { n: i32 }
                struct Holder { leaf: Leaf, fn get(borrow self, value: Leaf) -> Leaf { value } }
                enum Choice { One(Leaf) }
                fn convert(value: Leaf) -> Holder { Holder { leaf: value } }
                drop fn Holder(self) {}
                fn main() -> i32 { 0 }
            "#,
            )],
            1,
        );
        let mut session = CompilerSession::new();
        session.update(&source).into_result().unwrap();
        let manifest = session
            .semantic_dependency_inputs(&CompileOptions::default(), None)
            .unwrap();
        let names = manifest
            .declaration_type_dependencies()
            .iter()
            .map(|edge| {
                (
                    edge.source.name().to_owned(),
                    edge.target.name().to_owned(),
                    edge.kind,
                )
            })
            .collect::<Vec<_>>();
        assert!(names.contains(&(
            "Holder".into(),
            "Leaf".into(),
            rue_air::DeclarationTypeDependencyKind::Field
        )));
        assert!(names.contains(&(
            "Choice".into(),
            "Leaf".into(),
            rue_air::DeclarationTypeDependencyKind::Payload
        )));
        assert!(names.contains(&(
            "convert".into(),
            "Leaf".into(),
            rue_air::DeclarationTypeDependencyKind::Signature
        )));
        assert!(names.contains(&(
            "get".into(),
            "Leaf".into(),
            rue_air::DeclarationTypeDependencyKind::Signature
        )));
        assert!(names.contains(&(
            "Holder".into(),
            "Holder".into(),
            rue_air::DeclarationTypeDependencyKind::Owner
        )));
        assert!(manifest.declaration_type_dependencies_complete());
        assert_eq!(manifest.work().extra_rir_instructions_visited, 0);
    }

    #[test]
    fn deferred_nested_type_call_heads_survive_placeholder_erasure() {
        let program = r#"
            fn Result(comptime T: type) -> type { enum { Ok(T), Err } }
            fn Option(comptime T: type) -> type { enum { Some(T), None } }
            fn consume(comptime T: type, value: Option(Result(T))) -> i32 { 0 }
            fn main() -> i32 { 0 }
        "#;
        let build = |file_id, path| {
            let source = snapshot(&[(file_id, path, "main.rue", program)], file_id);
            let mut session = CompilerSession::new();
            session.update(&source).into_result().unwrap();
            session
                .semantic_dependency_inputs(&CompileOptions::default(), None)
                .unwrap()
        };
        let first = build(7, "/one/main.rue");
        let moved = build(91, "/moved/main.rue");
        assert_eq!(
            first.declaration_type_call_head_dependencies(),
            moved.declaration_type_call_head_dependencies()
        );
        let names = first
            .declaration_type_call_head_dependencies()
            .iter()
            .map(|edge| (edge.source.name(), edge.callable.name(), edge.kind))
            .collect::<Vec<_>>();
        assert_eq!(
            names,
            vec![
                (
                    "consume",
                    "Option",
                    rue_air::DeclarationTypeDependencyKind::Signature
                ),
                (
                    "consume",
                    "Result",
                    rue_air::DeclarationTypeDependencyKind::Signature
                ),
            ]
        );
        assert!(first.declaration_type_call_head_dependencies_complete());
        assert_eq!(first.work().declaration_type_call_head_events_translated, 2);
        assert_eq!(first.work().extra_rir_instructions_visited, 0);
    }

    #[test]
    fn deferred_nested_nominal_and_alias_types_keep_stable_declaration_edges() {
        let build = |main_id, lib_id, root: &str, reversed: bool| {
            let main = (
                main_id,
                format!("{root}/main.rue"),
                "main.rue",
                r#"const lib = @import("lib.rue");
                   const Alias = lib.Leaf;
                   fn consume(comptime N: i32, values: [Alias; N]) -> i32 { 0 }
                   fn main() -> i32 { 0 }"#,
            );
            let lib = (
                lib_id,
                format!("{root}/lib.rue"),
                "lib.rue",
                "pub struct Leaf { value: i32 }",
            );
            let owned = if reversed {
                vec![lib, main]
            } else {
                vec![main, lib]
            };
            let entries = owned
                .iter()
                .map(|(id, path, module, text)| (*id, path.as_str(), *module, *text))
                .collect::<Vec<_>>();
            let source = snapshot(&entries, main_id);
            let mut session = CompilerSession::new();
            session.update(&source).into_result().unwrap();
            session
                .semantic_dependency_inputs(&CompileOptions::default(), None)
                .unwrap()
        };
        let first = build(3, 8, "/p", false);
        let moved = build(91, 4, "/elsewhere", true);
        assert_eq!(
            first.declaration_type_dependencies(),
            moved.declaration_type_dependencies()
        );
        let consume_targets = first
            .declaration_type_dependencies()
            .iter()
            .filter(|edge| edge.source.name() == "consume")
            .map(|edge| {
                (
                    edge.target.module().as_str(),
                    edge.target.name(),
                    edge.target.kind(),
                )
            })
            .collect::<Vec<_>>();
        assert!(consume_targets.contains(&("main.rue", "Alias", StableDefinitionKind::ValueConst)));
        assert!(consume_targets.contains(&("lib.rue", "Leaf", StableDefinitionKind::Struct)));
        assert!(first.declaration_type_dependencies_complete());
        assert!(
            !first
                .dependency_blockers()
                .iter()
                .any(|blocker| { blocker.surface() == SemanticDependencySurface::DeclarationType })
        );
        assert_eq!(first.work().extra_rir_instructions_visited, 0);
    }

    #[test]
    fn module_qualified_type_call_head_uses_exact_callable_endpoint() {
        let source = snapshot(
            &[
                (
                    3,
                    "/p/main.rue",
                    "main.rue",
                    r#"const lib = @import("lib.rue");
                       fn consume(comptime T: type, value: lib.Box(T)) -> i32 { 0 }
                       fn main() -> i32 { 0 }"#,
                ),
                (
                    8,
                    "/p/lib.rue",
                    "lib.rue",
                    "pub fn Box(comptime T: type) -> type { struct { value: T } }",
                ),
            ],
            3,
        );
        let mut session = CompilerSession::new();
        session.update(&source).into_result().unwrap();
        let manifest = session
            .semantic_dependency_inputs(&CompileOptions::default(), None)
            .unwrap();
        let [edge] = manifest.declaration_type_call_head_dependencies() else {
            panic!("expected one module-qualified type-call head");
        };
        assert_eq!(edge.source.module().as_str(), "main.rue");
        assert_eq!(edge.source.name(), "consume");
        assert_eq!(edge.callable.module().as_str(), "lib.rue");
        assert_eq!(edge.callable.name(), "Box");
        assert_eq!(edge.callable.kind(), StableDefinitionKind::Function);
    }

    #[test]
    fn fixed_string_type_head_is_a_builtin_input_not_a_definition() {
        let source = snapshot(
            &[(
                4,
                "/p/main.rue",
                "main.rue",
                "fn consume(value: Str(8)) -> i32 { 0 } fn main() -> i32 { 0 }",
            )],
            4,
        );
        let mut options = CompileOptions::default();
        options
            .preview_features
            .insert("string_trio".parse().unwrap());
        let mut session = CompilerSession::new();
        session.update(&source).into_result().unwrap();
        let manifest = session.semantic_dependency_inputs(&options, None).unwrap();
        let [input] = manifest.builtin_type_call_head_inputs() else {
            panic!("expected one fixed-string builtin input");
        };
        assert_eq!(input.source.name(), "consume");
        assert_eq!(
            input.builtin,
            rue_air::BuiltinTypeCallHead::FixedCapacityString
        );
        assert!(
            manifest
                .declaration_type_call_head_dependencies()
                .is_empty()
        );
        assert!(manifest.supported_type_call_heads_complete());
        assert!(manifest.declaration_type_call_head_dependencies_complete());
        assert_eq!(manifest.work().builtin_type_call_head_inputs_translated, 1);
        assert_eq!(manifest.work().extra_rir_instructions_visited, 0);
    }

    #[test]
    fn named_owner_associated_type_head_is_not_supported_type_syntax() {
        let source = snapshot(
            &[(
                6,
                "/p/main.rue",
                "main.rue",
                r#"struct Factory {
                       fn Make() -> type { struct { value: i32 } }
                   }
                   fn consume(value: Factory.Make()) -> i32 { 0 }
                   fn main() -> i32 { 0 }"#,
            )],
            6,
        );
        let mut session = CompilerSession::new();
        session.update(&source).into_result().unwrap();
        assert!(
            session.semantic(&CompileOptions::default()).is_err(),
            "dotted type-call heads are module-qualified free functions, not associated functions"
        );
    }

    #[test]
    fn named_const_initializer_edges_are_stable_direct_and_zero_scan() {
        let program = r#"
            struct Point { x: i32 }
            fn inc(comptime n: i32) -> i32 { n + 1 }
            const A: i32 = 1;
            const B: i32 = inc(A);
            const C: i32 = A + B;
            const D: i32 = B + C;
            const P = Point;
            fn main() -> i32 { D }
        "#;
        let build = |file, path| {
            let source = snapshot(&[(file, path, "main.rue", program)], file);
            let mut session = CompilerSession::new();
            session.update(&source).into_result().unwrap();
            session
                .semantic_dependency_inputs(&CompileOptions::default(), None)
                .unwrap()
        };
        let first = build(2, "/one/main.rue");
        let moved = build(88, "/moved/main.rue");
        assert_eq!(
            first.named_const_dependencies(),
            moved.named_const_dependencies()
        );
        let names = first
            .named_const_dependencies()
            .iter()
            .map(|edge| {
                let target = match &edge.target {
                    StableNamedConstDependencyTarget::ValueConst(key)
                    | StableNamedConstDependencyTarget::FreeFunction(key)
                    | StableNamedConstDependencyTarget::NamedType(key)
                    | StableNamedConstDependencyTarget::ModuleBinding(key) => key.name(),
                };
                (edge.source.name(), target)
            })
            .collect::<Vec<_>>();
        for edge in [
            ("B", "A"),
            ("B", "inc"),
            ("C", "A"),
            ("C", "B"),
            ("D", "B"),
            ("D", "C"),
            ("P", "Point"),
        ] {
            assert!(
                names.contains(&edge),
                "missing direct edge {edge:?}: {names:?}"
            );
        }
        assert!(first.named_value_const_dependencies_complete());
        assert_eq!(first.work().named_const_events_translated, names.len());
        assert_eq!(first.work().extra_rir_instructions_visited, 0);

        let renamed_program = program
            .replace("const A: i32 = 1", "const Z: i32 = 1")
            .replace("inc(A)", "inc(Z)")
            .replace("A + B", "Z + B");
        let renamed_source = snapshot(&[(2, "/one/main.rue", "main.rue", &renamed_program)], 2);
        let mut renamed_session = CompilerSession::new();
        renamed_session
            .update(&renamed_source)
            .into_result()
            .unwrap();
        let renamed = renamed_session
            .semantic_dependency_inputs(&CompileOptions::default(), None)
            .unwrap();
        assert_ne!(
            first.named_const_dependencies(),
            renamed.named_const_dependencies()
        );
        assert!(renamed.named_const_dependencies().iter().any(|edge| {
            edge.source.name() == "B"
                && matches!(&edge.target, StableNamedConstDependencyTarget::ValueConst(key) if key.name() == "Z")
        }));
    }

    #[test]
    fn cyclic_const_initializers_publish_no_partial_dependency_graph() {
        let source = snapshot(
            &[(
                1,
                "/p/main.rue",
                "main.rue",
                "const A: i32 = B; const B: i32 = A; fn main() -> i32 { 0 }",
            )],
            1,
        );
        let mut session = CompilerSession::new();
        session.update(&source).into_result().unwrap();
        assert!(session.semantic(&CompileOptions::default()).is_err());
        let manifest = session
            .semantic_dependency_inputs(&CompileOptions::default(), None)
            .unwrap();
        assert!(manifest.named_const_dependencies().is_empty());
        assert!(!manifest.named_value_const_dependencies_complete());
        assert!(!manifest.definition_universe_complete());
    }

    #[test]
    fn qualified_const_edges_keep_module_binding_and_exact_member_identity() {
        let source = snapshot(
            &[
                (
                    3,
                    "/p/main.rue",
                    "main.rue",
                    r#"const lib = @import("lib.rue");
                       const other = @import("other.rue");
                       const X: i32 = lib.BASE;
                       const Y: i32 = other.BASE;
                       const T = lib.Row;
                       fn main() -> i32 { X + Y }"#,
                ),
                (11, "/p/other.rue", "other.rue", "pub const BASE: i32 = 5;"),
                (
                    9,
                    "/p/lib.rue",
                    "lib.rue",
                    "pub const BASE: i32 = 4; pub struct Row { n: i32 }",
                ),
            ],
            3,
        );
        let mut session = CompilerSession::new();
        session.update(&source).into_result().unwrap();
        let manifest = session
            .semantic_dependency_inputs(&CompileOptions::default(), None)
            .unwrap();
        let tags = manifest
            .named_const_dependencies()
            .iter()
            .map(|edge| {
                let (tag, target) = match &edge.target {
                    StableNamedConstDependencyTarget::ValueConst(key) => ("const", key),
                    StableNamedConstDependencyTarget::NamedType(key) => ("type", key),
                    StableNamedConstDependencyTarget::ModuleBinding(key) => ("module", key),
                    StableNamedConstDependencyTarget::FreeFunction(key) => ("fn", key),
                };
                (
                    edge.source.name(),
                    tag,
                    target.module().as_str(),
                    target.name(),
                )
            })
            .collect::<Vec<_>>();
        for expected in [
            ("X", "module", "main.rue", "lib"),
            ("X", "const", "lib.rue", "BASE"),
            ("T", "module", "main.rue", "lib"),
            ("T", "type", "lib.rue", "Row"),
            ("Y", "module", "main.rue", "other"),
            ("Y", "const", "other.rue", "BASE"),
        ] {
            assert!(tags.contains(&expected), "missing {expected:?}: {tags:?}");
        }
        assert!(
            tags.iter()
                .all(|(source, _, _, _)| *source != "lib" && *source != "other")
        );
    }

    #[test]
    fn const_dependency_capture_work_is_edge_proportional() {
        let build = |extra: usize| {
            let mut program = "const A: i32 = 1; const B: i32 = A;".to_string();
            for i in 0..extra {
                program.push_str(&format!(" const UNUSED_{i}: i32 = {i};"));
            }
            program.push_str(" fn main() -> i32 { B }");
            let source = snapshot(&[(1, "/p/main.rue", "main.rue", &program)], 1);
            let mut session = CompilerSession::new();
            session.update(&source).into_result().unwrap();
            session
                .semantic_dependency_inputs(&CompileOptions::default(), None)
                .unwrap()
        };
        let one = build(1);
        let many = build(128);
        assert_eq!(
            one.named_const_dependencies(),
            many.named_const_dependencies()
        );
        assert_eq!(one.work().named_const_events_translated, 1);
        assert_eq!(many.work().named_const_events_translated, 1);
        assert_eq!(many.work().extra_rir_instructions_visited, 0);
    }

    #[test]
    fn stable_definition_target_and_feature_inputs_are_separate() {
        let source = base();
        let default = CompileOptions::default();
        let mut session = CompilerSession::new();
        session.update(&source).into_result().unwrap();
        session.stable_definitions(&default).unwrap();
        let other_target = *Target::all()
            .iter()
            .find(|&&target| target != default.target)
            .expect("multiple compiler targets");
        session
            .stable_definitions(&CompileOptions {
                target: other_target,
                ..default.clone()
            })
            .unwrap();
        session
            .stable_definitions(&CompileOptions {
                preview_features: PreviewFeatures::from([PreviewFeature::TestInfra]),
                ..default
            })
            .unwrap();

        assert_eq!(session.work().definitions.executions, 3);
        assert_eq!(session.work().definition_entries, 3);
        assert_eq!(session.work().definition_records.len(), 3);
        assert!(session.work().definition_records.iter().all(|record| {
            record.binding.bind_invocations == 1
                && record.manifest.build_invocations == 1
                && !record.failed
        }));
    }

    #[test]
    fn definition_keys_ignore_opt_linker_relocation_file_ids_and_order() {
        let original = snapshot(
            &[
                (7, "/old/main.rue", "main.rue", "fn main() -> i32 { 0 }"),
                (2, "/old/a.rue", "a.rue", "fn a() {}"),
            ],
            7,
        );
        let moved = snapshot(
            &[
                (90, "/new/a.rue", "a.rue", "fn a() {}"),
                (40, "/new/main.rue", "main.rue", "fn main() -> i32 { 0 }"),
            ],
            40,
        );
        let renamed = snapshot(
            &[
                (90, "/new/lib/a.rue", "lib/a.rue", "fn a() {}"),
                (40, "/new/main.rue", "main.rue", "fn main() -> i32 { 0 }"),
            ],
            40,
        );
        let mut session = CompilerSession::new();
        session.update(&original).into_result().unwrap();
        let first = session
            .stable_definitions(&CompileOptions {
                linker: LinkerMode::System("x".to_string()),
                opt_level: OptLevel::O2,
                ..CompileOptions::default()
            })
            .unwrap();
        let keys = |set: &BoundDefinitionSet| {
            set.definitions()
                .iter()
                .map(|record| record.stable_key().clone())
                .collect::<Vec<_>>()
        };
        let first_keys = keys(&first);

        session.update(&moved).into_result().unwrap();
        let second = session
            .stable_definitions(&CompileOptions::default())
            .unwrap();
        assert_eq!(keys(&second), first_keys);
        assert_eq!(session.work().definition_entries_invalidated, 1);

        session.update(&renamed).into_result().unwrap();
        let third = session
            .stable_definitions(&CompileOptions::default())
            .unwrap();
        assert_ne!(keys(&third), first_keys);
    }

    #[test]
    fn failed_parse_preserves_ids_while_semantic_rejection_issues_none() {
        let valid = base();
        let syntax_bad = snapshot(
            &[
                (7, "/p/main.rue", "main.rue", "fn main( {"),
                (2, "/p/a.rue", "a.rue", "fn a() {}"),
            ],
            7,
        );
        let semantic_bad = snapshot(
            &[(
                7,
                "/p/main.rue",
                "main.rue",
                "fn main() -> i32 { missing_name }",
            )],
            7,
        );
        let options = CompileOptions::default();
        let mut session = CompilerSession::new();
        session.update(&valid).into_result().unwrap();
        let ids = session.stable_definitions(&options).unwrap();
        assert!(session.update(&syntax_bad).result().is_err());
        assert!(Arc::ptr_eq(
            &ids,
            &session.stable_definitions(&options).unwrap()
        ));

        session.update(&semantic_bad).into_result().unwrap();
        let first = session.stable_definitions(&options).unwrap_err();
        let second = session.stable_definitions(&options).unwrap_err();
        assert_eq!(format!("{first:?}"), format!("{second:?}"));
        assert_eq!(session.work().definitions.executions, 1);
        assert_eq!(session.work().definition_entries, 0);
        assert_eq!(session.work().semantic_records.len(), 1);
        assert!(session.work().semantic_records[0].failed);

        session.update(&valid).into_result().unwrap();
        assert!(session.stable_definitions(&options).is_ok());
        assert_eq!(session.work().definitions.executions, 2);
    }

    #[test]
    fn diagnostic_artifacts_retain_attempt_provenance_and_query_identity() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<FrontendDiagnosticSnapshot>();

        let valid = base();
        let syntax_bad = snapshot(&[(7, "/attempt/bad.rue", "bad.rue", "fn main( {")], 7);
        let semantic_bad = snapshot(
            &[(
                7,
                "/attempt/semantic.rue",
                "semantic.rue",
                "fn main() -> i32 { missing_name }",
            )],
            7,
        );
        let warning_source = snapshot(
            &[(
                7,
                "/attempt/warning.rue",
                "warning.rue",
                "fn main() -> i32 { let unused = 1; 0 }",
            )],
            7,
        );
        let mut session = CompilerSession::new();
        session.update(&valid).into_result().unwrap();
        let published = session.published().unwrap().clone();

        let failed = session.update(&syntax_bad);
        let syntax_diagnostics = failed.diagnostics().clone();
        assert_eq!(
            syntax_diagnostics.source().metadata(),
            syntax_bad.metadata()
        );
        assert_eq!(
            syntax_diagnostics.source_revision(),
            syntax_bad.source_revision()
        );
        assert!(!syntax_diagnostics.errors().is_empty());
        assert!(Arc::ptr_eq(session.published().unwrap(), &published));
        assert!(Arc::ptr_eq(
            session
                .diagnostics_for(&syntax_bad, &FrontendDiagnosticStage::Syntax)
                .unwrap(),
            &syntax_diagnostics
        ));

        session.update(&semantic_bad).into_result().unwrap();
        let options = CompileOptions::default();
        session.semantic(&options).unwrap_err();
        let first = session.latest_diagnostics().unwrap().clone();
        let first_fingerprint = first
            .errors()
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>();
        session.semantic(&options).unwrap_err();
        let reused = session.latest_diagnostics().unwrap().clone();
        assert!(Arc::ptr_eq(&first, &reused));
        assert_eq!(
            first_fingerprint,
            reused
                .errors()
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
        );
        let FrontendDiagnosticStage::Semantic(input) = first.stage() else {
            panic!("semantic diagnostic stage");
        };
        assert_eq!(input.opt_level, crate::StableOptLevel::O0);

        session.update(&warning_source).into_result().unwrap();
        session.semantic(&options).unwrap();
        let warning = session.latest_diagnostics().unwrap().clone();
        assert!(warning.is_success());
        assert!(!warning.warnings().is_empty());
        session
            .semantic(&CompileOptions {
                linker: LinkerMode::Internal,
                ..options.clone()
            })
            .unwrap();
        assert!(Arc::ptr_eq(&warning, session.latest_diagnostics().unwrap()));
        session
            .semantic(&CompileOptions {
                opt_level: OptLevel::O1,
                ..options.clone()
            })
            .unwrap();
        let optimized = session.latest_diagnostics().unwrap().clone();
        assert!(!Arc::ptr_eq(&warning, &optimized));
        session
            .semantic(&CompileOptions {
                preview_features: PreviewFeatures::from([PreviewFeature::TestInfra]),
                ..options.clone()
            })
            .unwrap();
        let featured = session.latest_diagnostics().unwrap().clone();
        assert!(!Arc::ptr_eq(&warning, &featured));
        let other_target = *Target::all()
            .iter()
            .find(|&&target| target != options.target)
            .unwrap();
        session
            .semantic(&CompileOptions {
                target: other_target,
                ..options
            })
            .unwrap();
        assert!(!Arc::ptr_eq(
            &warning,
            session.latest_diagnostics().unwrap()
        ));
        let old = syntax_diagnostics.clone();
        std::thread::spawn(move || {
            assert_eq!(old.source().metadata().root_file_id(), FileId::new(7));
            assert!(!old.errors().is_empty());
        })
        .join()
        .unwrap();
    }

    #[test]
    fn merge_diagnostics_are_memoized_pointer_identically() {
        let duplicate = snapshot(
            &[(1, "/p/main.rue", "main.rue", "fn main() {} fn main() {}")],
            1,
        );
        let mut session = CompilerSession::new();
        session.update(&duplicate).into_result().unwrap();
        session.merge().unwrap_err();
        let first = session.latest_diagnostics().unwrap().clone();
        session.merge().unwrap_err();
        let second = session.latest_diagnostics().unwrap().clone();
        assert!(Arc::ptr_eq(&first, &second));
        assert!(matches!(first.stage(), FrontendDiagnosticStage::Merge));
        assert_eq!(session.work().merge.executions, 1);
        assert_eq!(session.work().diagnostic_reuses, 1);
    }

    #[test]
    fn long_failure_recovery_sequence_bounds_diagnostics_and_preserves_last_good() {
        let options = CompileOptions::default();
        let source = |text: &str| snapshot(&[(7, "/p/main.rue", "main.rue", text)], 7);
        let initial = source("fn main() -> i32 { 0 }");
        let mut session = CompilerSession::new();
        session.update(&initial).into_result().unwrap();
        session.semantic(&options).unwrap();
        assert_eq!(session.work().retention.diagnostic_entries, 3);
        assert_eq!(session.work().retention.diagnostic_source_attempts, 1);
        assert_eq!(
            session.work().retention.diagnostic_source_bytes,
            initial.files().map(|file| file.source.len()).sum::<usize>()
        );
        let initial_good = session.last_good_semantic_diagnostics().unwrap().clone();
        assert!(initial_good.is_success());

        let first_bad = source("fn main( {");
        let first_update = session.update(&first_bad);
        assert!(first_update.result().is_err());
        let caller_pinned = first_update.diagnostics().clone();
        assert!(Arc::ptr_eq(
            session.last_good_semantic_diagnostics().unwrap(),
            &initial_good
        ));

        let mut maximum_attempt_bytes: usize = initial.files().map(|file| file.source.len()).sum();
        for revision in 1..=32 {
            let syntax_text = format!("// {}\nfn main( {{", "x".repeat(revision));
            maximum_attempt_bytes = maximum_attempt_bytes.max(syntax_text.len());
            let syntax_bad = source(&syntax_text);
            assert!(session.update(&syntax_bad).result().is_err());
            let before_semantic_failure = session.last_good_semantic_diagnostics().unwrap().clone();

            let semantic_text = format!("fn main() -> i32 {{ missing_{revision} }}");
            maximum_attempt_bytes = maximum_attempt_bytes.max(semantic_text.len());
            let semantic_bad = source(&semantic_text);
            session.update(&semantic_bad).into_result().unwrap();
            session.semantic(&options).unwrap_err();
            assert!(Arc::ptr_eq(
                session.last_good_semantic_diagnostics().unwrap(),
                &before_semantic_failure
            ));
            assert!(!session.latest_diagnostics().unwrap().is_success());

            let valid_text = format!("fn main() -> i32 {{ {revision} }}");
            maximum_attempt_bytes = maximum_attempt_bytes.max(valid_text.len());
            let valid = source(&valid_text);
            session.update(&valid).into_result().unwrap();
            let recovered = session.semantic(&options).unwrap();
            let recovered_diagnostics = session.latest_diagnostics().unwrap();
            assert!(recovered_diagnostics.is_success());
            assert!(Arc::ptr_eq(
                recovered_diagnostics,
                session.latest_successful_diagnostics().unwrap()
            ));
            assert!(Arc::ptr_eq(
                recovered_diagnostics,
                session.last_good_semantic_diagnostics().unwrap()
            ));
            assert!(Arc::ptr_eq(
                recovered_diagnostics,
                session
                    .diagnostics_for(
                        &valid,
                        &FrontendDiagnosticStage::Semantic(recovered.input().clone())
                    )
                    .unwrap()
            ));
        }

        let retention = session.work().retention;
        assert!(retention.diagnostic_entries <= FRONTEND_DIAGNOSTIC_RETENTION_LIMIT);
        assert!(retention.diagnostic_source_attempts <= retention.diagnostic_entries);
        assert!(
            retention.diagnostic_source_bytes
                <= FRONTEND_DIAGNOSTIC_RETENTION_LIMIT * maximum_attempt_bytes
        );
        assert!(
            session
                .diagnostics_for(&first_bad, &FrontendDiagnosticStage::Syntax)
                .is_none(),
            "unpinned old cache entry should be evicted"
        );
        assert_eq!(caller_pinned.source_revision(), first_bad.source_revision());
        assert!(!caller_pinned.errors().is_empty());

        let final_source = source("fn main() -> i32 { 32 }");
        let mut fresh = CompilerSession::new();
        fresh.update(&final_source).into_result().unwrap();
        let fresh_output = fresh.semantic(&options).unwrap();
        let retained_output = session.semantic(&options).unwrap();
        assert_eq!(
            format!("{:?}", retained_output.functions()),
            format!("{:?}", fresh_output.functions())
        );
        assert_eq!(retained_output.strings(), fresh_output.strings());
    }

    #[test]
    fn invalidation_plan_retention_is_fifo_bounded_with_strong_manifest_ownership() {
        let source = snapshot(
            &[(7, "/p/main.rue", "main.rue", "fn main() -> i32 { 0 }")],
            7,
        );
        let mut builder = CompilerSession::new();
        builder.update(&source).into_result().unwrap();
        let base = builder
            .semantic_dependency_inputs(&CompileOptions::default(), None)
            .unwrap();
        let manifests = (0..=FRONTEND_INVALIDATION_PLAN_RETENTION_LIMIT + 3)
            .map(|_| Arc::new((*base).clone()))
            .collect::<Vec<_>>();
        let mut planner = CompilerSession::new();
        let first = planner.semantic_invalidation_plan(&manifests[0], &manifests[1]);
        let mut last = first.clone();
        for pair in manifests.windows(2).skip(1) {
            last = planner.semantic_invalidation_plan(&pair[0], &pair[1]);
        }

        assert_eq!(
            planner.work().retention.invalidation_plans,
            FRONTEND_INVALIDATION_PLAN_RETENTION_LIMIT
        );
        assert_eq!(
            planner.work().retention.dependency_manifests,
            FRONTEND_INVALIDATION_PLAN_RETENTION_LIMIT + 1
        );
        let executions = planner.work().invalidation_plans.executions;
        let recomputed = planner.semantic_invalidation_plan(&manifests[0], &manifests[1]);
        assert!(!Arc::ptr_eq(&first, &recomputed));
        assert_eq!(planner.work().invalidation_plans.executions, executions + 1);
        let reused = planner
            .semantic_invalidation_plan(&manifests[manifests.len() - 2], manifests.last().unwrap());
        assert!(Arc::ptr_eq(&last, &reused));
        assert_eq!(
            planner.work().retention.invalidation_plans,
            FRONTEND_INVALIDATION_PLAN_RETENTION_LIMIT
        );
        assert_eq!(
            planner.work().retention.dependency_manifests,
            FRONTEND_INVALIDATION_PLAN_RETENTION_LIMIT + 2
        );
    }

    #[test]
    fn durable_ordinary_body_candidates_are_observational_and_round_trip_fresh_epoch() {
        let source = snapshot(
            &[(
                41,
                "/relocated/main.rue",
                "main.rue",
                "fn helper(x: i32) -> i32 { x + 1 }\nfn main() -> i32 { helper(41) }",
            )],
            41,
        );
        let mut session = CompilerSession::new();
        session.update(&source).into_result().unwrap();
        let manifest = session
            .semantic_dependency_inputs(&CompileOptions::default(), None)
            .unwrap();
        assert_eq!(manifest.durable_ordinary_bodies().len(), 2);
        let work = manifest.work().durable_bodies;
        assert_eq!(work.finalization_attempts, 2);
        assert_eq!(work.finalization_completions, 2);
        assert_eq!(work.finalization_failures, 0);
        assert_eq!(work.projection_attempts, 2);
        assert_eq!(work.projection_completions, 2);
        assert_eq!(work.import_attempts, 2);
        assert_eq!(work.import_successes, 2);
        assert_eq!(work.import_failures, 0);
        assert_eq!(work.atomic_discards, 0);
        assert!(work.installed_instructions > 0);
        // This boundary validates candidates but never skips ordinary work.
        let semantic_work = session.work().semantic_records.last().unwrap().work;
        assert_eq!(semantic_work.body_analysis.bodies_attempted, 2);
        assert_eq!(semantic_work.body_analysis.bodies_succeeded, 2);
        assert_eq!(semantic_work.durable_bodies.export_attempts, 2);
        assert_eq!(semantic_work.durable_bodies.export_successes, 2);
        assert_eq!(semantic_work.durable_bodies.conversion_attempts, 2);
        assert_eq!(semantic_work.durable_bodies.conversion_completions, 2);
        assert_eq!(semantic_work.durable_bodies.reused_bodies, 0);
        assert_eq!(semantic_work.durable_bodies.skipped_body_analyses, 0);
    }
}
