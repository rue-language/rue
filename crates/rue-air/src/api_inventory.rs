//! Structural guard for AIR's read-only canonical import consumption boundary.

#[test]
fn peer_one_body_authority_cannot_return() {
    let sources = [
        include_str!("lib.rs"),
        include_str!("sema/mod.rs"),
        include_str!("sema/binding_manifest.rs"),
    ]
    .concat();
    assert!(!sources.contains("mod one_body;"));
    assert!(!sources.contains("OneBodyTransactionOutcome"));
    assert!(!sources.contains("analyze_one_body"));
}

#[test]
fn retired_whole_program_body_driver_is_an_explicit_test_oracle_only() {
    let analysis = include_str!("sema/analysis.rs");
    let manifest = include_str!("sema/binding_manifest.rs");
    let sema = include_str!("sema/mod.rs");
    let sources = [analysis, manifest, sema].concat();

    for retired in [
        "fn analyze_all_function_bodies(",
        "fn analyze_all_function_bodies_with_work(",
        "fn analyze_all_function_bodies_mut(",
        "fn analyze_all_bodies(",
        "fn analyze_all_bodies_with_work(",
        ".analyze_all_bodies()",
        ".analyze_all_bodies_with_work()",
    ] {
        assert!(
            !sources.contains(retired),
            "retired production body authority returned: {retired}"
        );
    }
    for oracle in [
        "fn analyze_all_function_bodies_for_test(",
        "fn analyze_all_function_bodies_with_work_for_test(",
        "fn analyze_all_function_bodies_mut_for_test(",
        "fn analyze_all_bodies_for_test(",
    ] {
        assert!(
            sources.contains(oracle),
            "missing explicit whole-program test oracle: {oracle}"
        );
    }
}

#[test]
fn accessor_producers_do_not_spell_their_own_declaration_diagnostics() {
    // RUE-1232: the 6.6:3-6.6:7 accessor rules have one source of truth in
    // `declaration_validation`. Each producer lowers its own representation
    // into that vocabulary and reports what it hands back; none of them may
    // construct an accessor declaration diagnostic itself, which is how the
    // RIR walks and the driver's reparsed-AST walk stay unable to drift on
    // wording.
    let producers = [
        include_str!("sema/declarations.rs"),
        include_str!("sema/ordinary_engine.rs"),
        include_str!("sema/control_flow.rs"),
    ]
    .concat();

    for kind in [
        ["ErrorKind::Accessor", "RequiresBorrowSelf"].concat(),
        ["ErrorKind::Accessor", "ParamModeUnsupported"].concat(),
        ["ErrorKind::Accessor", "BodyMissingYield"].concat(),
        ["ErrorKind::Accessor", "BodyOtherExit"].concat(),
        ["ErrorKind::Accessor", "YieldNotReceiverRooted"].concat(),
    ] {
        assert!(
            !producers.contains(&kind),
            "an accessor producer regained its own copy of a declaration rule: {kind}"
        );
    }
    // The preview subject is a rule too: 6.6:3 names the same form everywhere.
    assert!(
        !producers.contains("\"a `-> borrow` accessor\""),
        "an accessor producer regained its own 6.6:3 gate subject"
    );
}

#[test]
fn canonical_import_consumers_do_not_grow_resolution_policy() {
    let consumers = [
        include_str!("canonical_imports.rs"),
        include_str!("sema/file_paths.rs"),
        include_str!("sema/analysis/intrinsics.rs"),
        include_str!("sema/declarations.rs"),
    ]
    .concat();

    assert_eq!(
        consumers.matches("pub trait CanonicalImportView {").count(),
        1,
        "AIR must expose exactly one borrowing import-consumption boundary"
    );
    assert!(consumers.contains("fn visit_resolved_sites("));
    assert!(consumers.contains("CanonicalImportContext"));

    for retired in [
        ["SemanticResolved", "Import"].concat(),
        ["SemanticModule", "Identity"].concat(),
        ["ParsedImport", "Site"].concat(),
        ["Module", "Path"].concat(),
        ["Dir", "Resolution"].concat(),
        ["Vec<(String, u32, String, String)>"].concat(),
    ] {
        assert!(
            !consumers.contains(&retired),
            "AIR import consumer regained a peer representation: {retired}"
        );
    }

    for forbidden_policy in [
        ["std::", "fs"].concat(),
        ["fs", "::"].concat(),
        ["std::", "env"].concat(),
        ["RUE_STD", "_PATH"].concat(),
        ["canonicalize", "("].concat(),
        [".exists", "("].concat(),
        ["candidate_", "groups"].concat(),
        ["resolve_explicit_", "candidates"].concat(),
    ] {
        assert!(
            !consumers.contains(&forbidden_policy),
            "AIR import consumer must not own discovery policy: {forbidden_policy}"
        );
    }
}

#[test]
fn const_classification_has_one_tagged_authority_and_no_retired_maps() {
    let air_production = [
        include_str!("call_abi.rs"),
        include_str!("canonical_imports.rs"),
        include_str!("drop_glue_names.rs"),
        include_str!("ffi_predicates.rs"),
        include_str!("inference/constraint.rs"),
        include_str!("inference/generate.rs"),
        include_str!("inference/mod.rs"),
        include_str!("inference/types.rs"),
        include_str!("inference/unify.rs"),
        include_str!("inst.rs"),
        include_str!("inst/payload_support.rs"),
        include_str!("intern_pool.rs"),
        include_str!("layout.rs"),
        include_str!("lib.rs"),
        include_str!("module_registry.rs"),
        include_str!("param_arena.rs"),
        include_str!("path_norm.rs"),
        include_str!("runtime_call.rs"),
        include_str!("scope.rs"),
        include_str!("sema/aggregates.rs"),
        include_str!("sema/analysis.rs"),
        include_str!("sema/analysis/anon_methods.rs"),
        include_str!("sema/analysis/builtin_ops.rs"),
        include_str!("sema/analysis/calls.rs"),
        include_str!("sema/analysis/functions.rs"),
        include_str!("sema/analysis/instructions.rs"),
        include_str!("sema/analysis/intrinsics.rs"),
        include_str!("sema/analysis/ownership.rs"),
        include_str!("sema/analysis/pointers.rs"),
        include_str!("sema/analysis/type_inference.rs"),
        include_str!("sema/analyze_ops.rs"),
        include_str!("sema/anon_structs.rs"),
        include_str!("sema/binding_manifest.rs"),
        include_str!("sema/builtins.rs"),
        include_str!("sema/comptime_eval.rs"),
        include_str!("sema/context.rs"),
        include_str!("sema/control_flow.rs"),
        include_str!("sema/declaration_index.rs"),
        include_str!("sema/declarations.rs"),
        include_str!("sema/file_paths.rs"),
        include_str!("sema/inference_ctx.rs"),
        include_str!("sema/info.rs"),
        include_str!("sema/known_symbols.rs"),
        include_str!("sema/metadata.rs"),
        include_str!("sema/mod.rs"),
        include_str!("sema/output.rs"),
        include_str!("sema/semantic_body_export.rs"),
        include_str!("sema/typeck.rs"),
        include_str!("sema/visibility.rs"),
        include_str!("semantic_body.rs"),
        include_str!("semantic_identity.rs"),
        include_str!("semantic_import.rs"),
        include_str!("semantic_type_resolution.rs"),
        include_str!("specialize.rs"),
        include_str!("type_encoding.rs"),
        include_str!("type_properties.rs"),
        include_str!("types.rs"),
    ]
    .concat();

    assert_eq!(
        air_production
            .matches("const_resolutions: HashMap<")
            .count(),
        1,
        "AIR must retain exactly one tagged const-resolution table"
    );
    assert_eq!(
        air_production
            .matches("pub(crate) enum ConstResolution")
            .count(),
        1,
        "AIR must classify value constants and module bindings in one internal enum"
    );
    for retired_map in [
        ["constants_by_", "file_name: HashMap"].concat(),
        ["module_", "bindings: HashMap"].concat(),
    ] {
        assert!(
            !air_production.contains(&retired_map),
            "retired const storage map returned: {retired_map}"
        );
    }
}

#[test]
fn const_resolution_uses_only_shell_bound_candidate_locators() {
    let declaration_index = include_str!("sema/declaration_index.rs")
        .split("#[cfg(test)]")
        .next()
        .unwrap();
    let resolver = include_str!("sema/declarations.rs");
    let shell_boundary = include_str!("sema/binding_manifest.rs");

    for retired_index_authority in [
        ["const_", "candidates: Vec<InstRef>"].concat(),
        ["const_candidates_by_", "file_name: HashMap"].concat(),
        ["fn all_const_", "candidates("].concat(),
    ] {
        assert!(
            !declaration_index.contains(&retired_index_authority),
            "RIR declaration index regained const lookup authority: {retired_index_authority}"
        );
    }
    for retired_resolver_read in [
        ["declaration_index.", "const_candidates("].concat(),
        ["declaration_index.", "all_const_candidates("].concat(),
    ] {
        assert!(
            !resolver.contains(&retired_resolver_read),
            "const resolver bypassed the shell-bound candidate set: {retired_resolver_read}"
        );
    }
    assert_eq!(
        shell_boundary
            .matches("self.sema.bound_const_candidates =")
            .count(),
        1,
        "declaration resolution must install the exact shell-bound const set once"
    );
    assert_eq!(
        resolver.matches(".bound_const_candidates").count(),
        4,
        "all const occurrence traversal and point lookup must use the bound set"
    );
}

#[test]
fn canonical_type_surface_has_one_checked_handle_and_private_storage_ids() {
    let types = include_str!("types.rs");
    let pool = include_str!("intern_pool.rs");
    let encoding = include_str!("type_encoding.rs");
    let exports = include_str!("lib.rs");
    let public_surface = [types, pool, encoding, exports].concat();

    let peer_handle = ["Interned", "Type"].concat();
    let compatibility_module = ["mod ", "compatibility"].concat();
    for retired in [
        peer_handle,
        ["type_to_", "interned"].concat(),
        ["interned_to_", "type"].concat(),
        compatibility_module,
        ["update_", "struct_def"].concat(),
        ["update_", "enum_def"].concat(),
    ] {
        assert!(
            !public_surface.contains(&retired),
            "AIR regained a peer type representation: {retired}"
        );
    }

    for line in public_surface.lines().map(str::trim) {
        for raw_api in [
            "pub const fn from_pool_index(",
            "pub fn from_pool_index(",
            "pub const fn pool_index(",
            "pub fn pool_index(",
            "pub const fn raw_encoding(",
            "pub fn raw_encoding(",
            "pub const fn from_u32(",
            "pub fn from_u32(",
            "pub const fn as_u32(",
            "pub fn as_u32(",
        ] {
            assert!(
                !line.starts_with(raw_api),
                "AIR exposed an unchecked raw type API: {line}"
            );
        }
    }

    for line in types.lines().map(str::trim) {
        for id in [
            "StructId",
            "EnumId",
            "ArrayTypeId",
            "PtrConstTypeId",
            "PtrMutTypeId",
        ] {
            assert!(
                !line.starts_with(&format!("pub struct {id}(pub ")),
                "AIR exposed {id}'s raw storage field"
            );
            assert!(
                !types.contains(&format!("Display for {id}")),
                "AIR exposed {id}'s raw numeric display"
            );
        }
    }

    assert_eq!(encoding.matches("enum Primitive").count(), 1);
    assert_eq!(encoding.matches("enum Composite").count(), 1);
    let consumers = [types, pool, exports].concat();
    for duplicated_tag in [
        "Struct = 100",
        "Enum = 101",
        "Array = 102",
        "Module = 103",
        "PtrConst = 104",
        "PtrMut = 105",
    ] {
        assert!(
            !consumers.contains(duplicated_tag),
            "composite tag escaped the authoritative encoding: {duplicated_tag}"
        );
    }

    assert!(types.contains("pub fn try_from_u32(v: u32) -> Option<Self>"));
    assert!(types.contains("pub fn try_kind(&self) -> Option<TypeKind>"));
    assert!(public_surface.contains("pub struct TypeInternPool"));
    assert!(pool.contains("pub fn all_types(&self) -> impl ExactSizeIterator<Item = Type> + '_"));
    assert!(pool.contains("pub(crate) fn set_struct_destructor("));
    assert!(pool.contains("pub(crate) fn requalify_struct_destructor("));
}

#[test]
fn air_payload_ownership_and_validation_boundary_cannot_regress() {
    let inst = include_str!("inst.rs");
    let exports = include_str!("lib.rs");
    let semantic_output = include_str!("sema/output.rs");
    let imported_body = include_str!("semantic_body.rs");

    assert!(inst.contains("pub struct AirEditor {"));
    assert!(inst.contains("pub struct ValidatedAir {"));
    assert!(inst.contains("impl std::ops::Deref for ValidatedAir"));
    assert!(!inst.contains("impl std::ops::DerefMut for ValidatedAir"));
    assert!(!inst.contains("pub fn add_inst("));
    assert!(!inst.contains("pub fn add_extra("));
    assert!(!inst.contains("pub fn get_extra("));
    let validated_impl = inst
        .split("impl ValidatedAir {")
        .nth(1)
        .and_then(|rest| rest.split("\n}\n\nimpl Air {").next())
        .expect("validated AIR implementation");
    assert!(validated_impl.contains("pub fn into_editor(self) -> AirEditor"));
    assert!(!validated_impl.contains("&mut self"));

    // Payload ranges expose logical lengths, but their positions and
    // construction remain owner-private. They are deliberately non-Copy so a
    // detached token cannot be casually duplicated into another owner.
    let range_macro = inst
        .split("macro_rules! word_range")
        .nth(1)
        .and_then(|rest| rest.split("word_range!(AirMatchArms)").next())
        .expect("typed AIR range macro");
    assert!(range_macro.contains("start: u32"));
    assert!(range_macro.contains("extent: u32"));
    assert!(!range_macro.contains("pub start"));
    assert!(!range_macro.contains("pub extent"));
    assert!(!range_macro.contains("Clone"));
    assert!(!range_macro.contains("Copy"));

    assert!(exports.contains("AirEditor"));
    assert!(exports.contains("ValidatedAir"));
    assert!(semantic_output.contains("pub air: crate::ValidatedAir"));
    assert!(imported_body.contains("pub air: crate::ValidatedAir"));

    let air = inst
        .split("pub struct Air {")
        .nth(1)
        .and_then(|rest| rest.split("\n}").next())
        .expect("AIR owner declaration");
    for store in ["instructions", "extra", "projections", "places"] {
        assert!(
            !air.contains(&format!("pub {store}:")),
            "AIR exposed {store} store"
        );
    }
    let place = inst
        .split("pub struct AirPlace {")
        .nth(1)
        .and_then(|rest| rest.split("\n}").next())
        .expect("AIR place declaration");
    assert!(!place.contains("pub projections:"));
    for raw_api in [
        "pub fn add_extra(",
        "pub fn get_extra(",
        "pub fn extra_mut(",
        "pub fn projection_store_mut(",
        "pub fn from_parts(",
    ] {
        assert!(
            !inst.contains(raw_api),
            "AIR exposed raw payload API: {raw_api}"
        );
    }
    assert_eq!(inst.matches("word_range!(Air").count(), 10);
    assert_eq!(crate::AIR_PAYLOAD_FAMILY_NAMES.len(), 10);

    // Semantic consumers receive an immutable RIR. Its payload fields and
    // stores remain inaccessible, so this lower-level entry point is not a
    // payload escape hatch.
    let sema = include_str!("sema/mod.rs");
    assert!(sema.contains("pub fn new_synthetic(\n        rir: &'a Rir,"));
    assert!(!sema.contains("&'a mut Rir"));
}

#[test]
fn semantic_schema_scaffolding_stays_exhaustive_and_reviewable() {
    let body = include_str!("semantic_body.rs");
    let import = include_str!("semantic_import.rs");

    assert!(body.contains("macro_rules! semantic_body_inst_schema"));
    assert!(body.contains("pub enum SemanticBodyInstKind"));
    assert!(body.contains("pub fn try_map_keys<K2, M2, E>("));
    assert!(body.contains("pub fn visit_dependencies("));
    assert!(body.contains("pub struct SemanticBodyInstFailureContext<E>"));
    assert!(body.contains("let data = inst.data.try_map_keys(key, module)?;"));
    assert!(import.contains("macro_rules! semantic_import_type_schema"));
    assert!(import.contains("macro_rules! semantic_import_const_schema"));
    assert!(import.contains("pub enum SemanticImportTypeKind"));
    assert!(import.contains("pub enum SemanticImportConstKind"));

    let generated_kind = body
        .split("pub const fn kind(&self) -> SemanticBodyInstKind")
        .nth(1)
        .and_then(|source| source.split("semantic_body_inst_schema!(").next())
        .expect("generated semantic instruction kind implementation");
    assert!(generated_kind.contains("match self"));
    assert!(!generated_kind.contains("_ =>"));
}

#[test]
fn local_semantic_materialization_owns_complete_cfg_inputs_without_a_peer_cache() {
    let import = include_str!("semantic_import.rs");
    let providers = include_str!("sema/provider_body_host.rs");

    for owned in [
        "pub air: crate::ValidatedAir",
        "pub type_pool: crate::FrozenTypeInternPool",
        "pub interner: Arc<ThreadedRodeo>",
        "pub aggregate_types:",
        "pub strings: Vec<String>",
        "pub body_span: Span",
        "pub completeness: SemanticLocalCompleteness",
    ] {
        assert!(
            import.contains(owned),
            "local semantic materialization lost owned CFG input: {owned}"
        );
    }
    assert!(import.contains("pub fn new_local("));
    assert!(import.contains("pub fn materialize_local_body("));
    assert!(import.contains("local_completeness: Option<SemanticLocalCompleteness>"));
    let materialize_signature = import
        .split("pub fn materialize_local_body(")
        .nth(1)
        .and_then(|source| source.split(") -> Result").next())
        .expect("local materialization signature");
    assert!(
        !materialize_signature.contains("completeness:"),
        "a caller-provided completeness witness could be crossed between epochs"
    );
    assert!(import.contains("NominalInstanceKey::Anonymous"));
    assert!(import.contains("SemanticImportFailure::BuiltinNominalShadow"));
    assert!(import.contains("specialization_key(specialization)"));
    for provider in ["ProviderSpecializedBody", "ProviderAnonymousBody"] {
        let body = providers
            .split(&format!("pub struct {provider}"))
            .nth(1)
            .and_then(|source| source.split("\n}").next())
            .expect("provider result declaration");
        assert!(body.contains("pub function: AnalyzedFunction"));
        assert!(body.contains("pub type_pool: Rc<TypeInternPool>"));
        assert!(body.contains("pub interner: Rc<ThreadedRodeo>"));
    }
    for forbidden in ["Mutex<", "RwLock<", "QueryFamily<", "cache:", "selected:"] {
        let materialization = import
            .split("pub struct SemanticLocalMaterialization")
            .nth(1)
            .and_then(|source| source.split("\n}").next())
            .expect("local materialization declaration");
        assert!(
            !materialization.contains(forbidden),
            "body-local materialization became peer state: {forbidden}"
        );
    }
}

#[test]
fn semantic_definition_taxonomy_has_one_enum_declaration() {
    let canonical = include_str!("semantic_identity.rs");
    let bindings = include_str!("sema/binding_manifest.rs");
    let bodies = include_str!("semantic_body.rs");

    assert!(canonical.contains("macro_rules! stable_definition_kind_schema"));
    assert_eq!(
        canonical.matches("pub enum StableDefinitionKind").count(),
        1
    );
    assert_eq!(
        canonical
            .matches("pub enum StableDefinitionNamespace")
            .count(),
        1
    );
    assert!(!bindings.contains("pub enum StableDefinitionKind"));
    assert!(!bindings.contains("pub enum StableDefinitionNamespace"));
    assert!(!bodies.contains("pub enum StableDefinitionKind"));
}

#[test]
fn source_owned_declaration_producers_are_test_only_entrypoints() {
    let sema = include_str!("sema/mod.rs");
    let shells = include_str!("sema/binding_manifest.rs");
    let declarations = include_str!("sema/declarations.rs");

    for gated_entrypoint in [
        "#[cfg(test)]\n    pub fn analyze_all(",
        "#[cfg(test)]\n    pub fn bind_declarations(",
    ] {
        assert!(
            sema.contains(gated_entrypoint),
            "source-owned producer escaped its test-only gate: {gated_entrypoint}"
        );
    }
    for gated_entrypoint in [
        "#[cfg(test)]\n    pub fn resolve_declarations(",
        "#[cfg(test)]\n    pub fn resolve_declarations_with_work(",
    ] {
        assert!(
            shells.contains(gated_entrypoint),
            "source-owned shell producer escaped its test-only gate: {gated_entrypoint}"
        );
    }
    assert!(
        declarations.contains("#[cfg(test)]\n    pub(crate) fn resolve_declarations("),
        "source-owned resolver escaped its test-only gate"
    );
    assert!(sema.contains("#[doc(hidden)]\n    pub fn predeclare_declaration_shells_for_test("));
    // The shell producer is consumed only by the frozen test-support adapters:
    // `bind_declarations_for_test` (predeclare + resolve) and
    // `analyze_all_for_test_with_stable_endpoints` (predeclare + authoritative
    // stable-identity endpoint install + resolve + analyze). Both are doc-hidden
    // `_for_test` entry points; no production path calls the shell producer.
    assert_eq!(
        sema.matches(".predeclare_declaration_shells_for_test()")
            .count(),
        2,
        "only the frozen test-support adapters may call the shell producer"
    );
    let shell_production = shells
        .split("\n#[cfg(test)]\nmod ")
        .next()
        .expect("binding manifest production prefix");
    assert!(
        !shell_production.contains(".predeclare_declaration_shells_for_test()"),
        "AIR production called the frozen declaration-shell adapter"
    );
}
