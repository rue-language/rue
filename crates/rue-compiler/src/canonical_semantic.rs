//! One-pass canonical declaration binding, body analysis, and CFG lowering.

use rue_air::{
    BodyAnalysisWork, DeclarationBindingWork, RirDeclarationIndexWork, SemanticBindingManifestWork,
};
use tracing::info_span;

use crate::{
    BoundDefinitionSet, BoundDefinitionWork, CanonicalMergedProgram, CanonicalRirOutput,
    CodegenInputDescriptor, CompileOptions, CompileWarning, FunctionWithCfg, MultiErrorResult,
    SemanticInputDescriptor, TypeInternPool,
    bound_definitions::{configure_canonical_sema, issue_bound_definitions},
    build_functions_and_cfgs,
};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
/// Structural work from one canonical semantic request.
pub struct CanonicalSemanticWork {
    /// One request-local RIR declaration-index construction.
    pub declaration_index: RirDeclarationIndexWork,
    /// Completed declaration binding, independent of optional manifest work.
    pub binding: DeclarationBindingWork,
    /// Optional stable-ID manifest traversal; zero when IDs were not requested.
    pub manifest: SemanticBindingManifestWork,
    /// Stable identity issuance work, absent when IDs were not requested.
    pub bound_definitions: Option<BoundDefinitionWork>,
    /// Demand-driven function-body analysis work.
    pub body_analysis: BodyAnalysisWork,
    /// Whether this request asked for stable source definition IDs.
    pub stable_ids_requested: bool,
}

/// Owned semantic and optimized CFG artifacts from the canonical frontend.
#[derive(Debug)]
pub struct CanonicalSemanticOutput {
    input: CodegenInputDescriptor,
    functions: Vec<FunctionWithCfg>,
    type_pool: TypeInternPool,
    strings: Vec<String>,
    warnings: Vec<CompileWarning>,
    bound_definitions: Option<BoundDefinitionSet>,
    work: CanonicalSemanticWork,
}

impl CanonicalSemanticOutput {
    /// Exact semantic and optimization identity of this output.
    pub fn input(&self) -> &CodegenInputDescriptor {
        &self.input
    }
    /// Analyzed functions paired with optimized CFGs in machine-symbol order.
    pub fn functions(&self) -> &[FunctionWithCfg] {
        &self.functions
    }
    /// Request-local type universe retained by the semantic output.
    pub fn type_pool(&self) -> &TypeInternPool {
        &self.type_pool
    }
    /// String literals indexed by AIR string-constant index.
    pub fn strings(&self) -> &[String] {
        &self.strings
    }
    /// Semantic and CFG warnings in canonical output order.
    pub fn warnings(&self) -> &[CompileWarning] {
        &self.warnings
    }
    /// Stable definition identities when requested for this run.
    pub fn bound_definitions(&self) -> Option<&BoundDefinitionSet> {
        self.bound_definitions.as_ref()
    }
    /// Structural work performed by this request.
    pub fn work(&self) -> CanonicalSemanticWork {
        self.work
    }
}

/// Bind declarations once, optionally issue stable IDs, then consume the same
/// transient bound Sema for body analysis and CFG construction.
pub fn analyze_canonical_program(
    merged: &CanonicalMergedProgram,
    rir: &CanonicalRirOutput,
    options: &CompileOptions,
    request_stable_ids: bool,
) -> MultiErrorResult<CanonicalSemanticOutput> {
    let input = CodegenInputDescriptor {
        semantic: SemanticInputDescriptor::new(
            merged.definitions().source_snapshot(),
            options.target,
            &options.preview_features,
        ),
        opt_level: options.opt_level.into(),
    };
    let sema = {
        let _span =
            info_span!("rir_declaration_index", instruction_count = rir.rir().len()).entered();
        configure_canonical_sema(
            merged,
            rir,
            options.preview_features.clone(),
            options.target,
        )?
    };
    let sema_span = info_span!("sema").entered();
    let declaration_index = sema.rir_declaration_index_work();
    let bound = sema.bind_declarations()?;
    let binding = bound.binding_work();

    let (bound_definitions, manifest_work) = if request_stable_ids {
        let manifest = bound.binding_manifest();
        let definitions = issue_bound_definitions(
            merged,
            rir.source_revision(),
            manifest.bindings(),
            manifest.work(),
        )
        .map_err(crate::CompileErrors::from)?;
        (Some(definitions), manifest.work())
    } else {
        debug_assert!(!bound.manifest_is_materialized());
        (None, SemanticBindingManifestWork::default())
    };

    let sema_output = bound.analyze_all_bodies()?;
    let body_analysis = sema_output.body_analysis_work;
    drop(sema_span);
    let cfg = build_functions_and_cfgs(
        sema_output,
        options.opt_level,
        rir.semantic_symbols().interner(),
    )?;
    let work = CanonicalSemanticWork {
        declaration_index,
        binding,
        manifest: manifest_work,
        bound_definitions: bound_definitions.as_ref().map(BoundDefinitionSet::work),
        body_analysis,
        stable_ids_requested: request_stable_ids,
    };
    Ok(CanonicalSemanticOutput {
        input,
        functions: cfg.functions,
        type_pool: cfg.type_pool,
        strings: cfg.strings,
        warnings: cfg.warnings,
        bound_definitions,
        work,
    })
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;

    use rue_span::FileId;

    use super::{CanonicalSemanticOutput, CanonicalSemanticWork, analyze_canonical_program};
    use crate::parsed_modules::parse_source_snapshot_modules;
    use crate::{
        CanonicalRirOutput, CompilationUnit, CompileOptions, FunctionWithCfg, SourceMetadata,
        SourceSnapshot, lower_canonical_rir, merge_parsed_modules,
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

    fn canonical(
        snapshot: &SourceSnapshot,
        options: &CompileOptions,
        ids: bool,
    ) -> (CanonicalSemanticOutput, CanonicalRirOutput) {
        let parsed = parse_source_snapshot_modules(snapshot).unwrap();
        let merged = merge_parsed_modules(&parsed).unwrap();
        let rir = lower_canonical_rir(&merged).unwrap();
        let output = analyze_canonical_program(&merged, &rir, options, ids).unwrap();
        (output, rir)
    }

    fn function_fingerprint(
        functions: &[FunctionWithCfg],
        interner: &crate::ThreadedRodeo,
    ) -> Vec<String> {
        functions
            .iter()
            .map(|function| {
                format!(
                    "{}|{}",
                    function.analyzed.name,
                    function.cfg.display_with_interner(interner)
                )
            })
            .collect()
    }

    #[test]
    fn canonical_semantic_and_cfg_artifacts_match_existing_unit_pipeline() {
        let source = snapshot(
            &[
                (
                    9,
                    "/p/main.rue",
                    "main.rue",
                    "const helper = @import(\"helper.rue\"); fn main() -> i32 { helper.answer() }",
                ),
                (
                    3,
                    "/p/helper.rue",
                    "helper.rue",
                    "pub fn answer() -> i32 { 42 }",
                ),
            ],
            9,
        );
        let options = CompileOptions::default();
        let (canonical, canonical_rir) = canonical(&source, &options, false);
        let mut legacy = CompilationUnit::from_source_snapshot(source, options);
        legacy.run_frontend().unwrap();

        assert_eq!(
            function_fingerprint(
                canonical.functions(),
                canonical_rir.semantic_symbols().interner()
            ),
            function_fingerprint(legacy.functions(), legacy.interner())
        );
        assert_eq!(canonical.type_pool().stats(), legacy.type_pool().stats());
        assert_eq!(canonical.strings(), legacy.strings());
        assert_eq!(
            canonical
                .warnings()
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>(),
            legacy
                .warnings()
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
        );
        assert_eq!(canonical.work().binding.bind_invocations, 1);
        assert_eq!(canonical.work().manifest.build_invocations, 0);
        assert!(canonical.bound_definitions().is_none());
    }

    #[test]
    fn requesting_ids_materializes_manifest_without_rebinding() {
        let source = snapshot(
            &[(
                1,
                "/main.rue",
                "main.rue",
                "struct Value { n: i32 } fn main() -> i32 { 42 }",
            )],
            1,
        );
        let options = CompileOptions::default();
        let (ordinary, ordinary_rir) = canonical(&source, &options, false);
        let (with_ids, with_ids_rir) = canonical(&source, &options, true);
        assert_eq!(ordinary.work().binding.bind_invocations, 1);
        assert_eq!(with_ids.work().binding.bind_invocations, 1);
        assert_eq!(ordinary.work().manifest.build_invocations, 0);
        assert_eq!(with_ids.work().manifest.build_invocations, 1);
        assert!(ordinary.bound_definitions().is_none());
        assert!(with_ids.bound_definitions().is_some());
        assert_eq!(
            function_fingerprint(
                ordinary.functions(),
                ordinary_rir.semantic_symbols().interner()
            ),
            function_fingerprint(
                with_ids.functions(),
                with_ids_rir.semantic_symbols().interner()
            )
        );
    }

    fn irrelevant_declarations(count: usize) -> CanonicalSemanticWork {
        let mut source = String::from("fn main() -> i32 { 42 }");
        for index in 0..count {
            source.push_str(&format!(" fn irrelevant{index}() -> i32 {{ {index} }}"));
        }
        let snapshot = snapshot(&[(1, "/main.rue", "main.rue", &source)], 1);
        canonical(&snapshot, &CompileOptions::default(), false)
            .0
            .work()
    }

    #[test]
    fn binding_and_reachable_dispatch_are_constant_with_128_irrelevant_declarations() {
        let one = irrelevant_declarations(1);
        let many = irrelevant_declarations(128);
        assert_eq!(one.binding.bind_invocations, 1);
        assert_eq!(many.binding.bind_invocations, 1);
        assert_eq!(one.declaration_index.build_invocations, 1);
        assert_eq!(many.declaration_index.build_invocations, 1);
        assert_eq!(one.manifest.build_invocations, 0);
        assert_eq!(many.manifest.build_invocations, 0);
        assert_eq!(
            one.body_analysis.free_function_record_lookups,
            many.body_analysis.free_function_record_lookups
        );
        assert_eq!(one.body_analysis.reachable_declaration_rir_visits, 0);
        assert_eq!(many.body_analysis.reachable_declaration_rir_visits, 0);
        assert!(many.binding.input_rir_instructions > one.binding.input_rir_instructions);
        assert!(many.binding.indexed_free_functions > one.binding.indexed_free_functions);
    }

    #[test]
    fn codegen_input_tracks_root_paths_and_options_but_not_linker() {
        let sources = [
            (
                1,
                "/old/main.rue",
                "main.rue",
                "const h = @import(\"helper.rue\"); fn main() -> i32 { h.helper() }",
            ),
            (
                2,
                "/old/helper.rue",
                "helper.rue",
                "pub fn helper() -> i32 { 42 }",
            ),
        ];
        let base_snapshot = snapshot(&sources, 1);
        let base_options = CompileOptions::default();
        let (base, _) = canonical(&base_snapshot, &base_options, false);

        let mut linker = base_options.clone();
        linker.linker = crate::LinkerMode::System("clang".to_owned());
        let (linker, _) = canonical(&base_snapshot, &linker, false);
        assert_eq!(base.input(), linker.input());

        let mut optimized = base_options.clone();
        optimized.opt_level = crate::OptLevel::O1;
        let (optimized, _) = canonical(&base_snapshot, &optimized, false);
        assert_ne!(base.input(), optimized.input());

        let relocated = snapshot(
            &[
                (1, "/new/main.rue", "main.rue", sources[0].3),
                (2, "/new/helper.rue", "helper.rue", sources[1].3),
            ],
            1,
        );
        let (relocated, _) = canonical(&relocated, &base_options, false);
        assert_ne!(base.input(), relocated.input());

        let different_root = snapshot(&sources, 2);
        let (different_root, _) = canonical(&different_root, &base_options, false);
        assert_ne!(base.input(), different_root.input());
    }
}
