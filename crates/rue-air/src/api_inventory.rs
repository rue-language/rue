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
fn type_syntax_dependency_admission_indexes_only_the_large_case() {
    let typeck = include_str!("sema/typeck.rs");

    assert!(
        typeck.contains("observed_type_dependency_index: Option<AHashSet<ObservedTypeDependency>>")
    );
    assert!(typeck.contains("const LINEAR_ADMISSION_LIMIT: usize = 8"));
    assert!(typeck.contains("if index.insert(dependency.clone())"));
    assert!(typeck.contains("index.clear()"));
    assert!(typeck.contains(".len() == LINEAR_ADMISSION_LIMIT"));
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
    // The declaration subject is a rule too: 6.6:3 names the same form
    // everywhere.
    assert!(
        !producers.contains("\"a `-> borrow` accessor\""),
        "an accessor producer regained its own 6.6:3 gate subject"
    );

    // Signature legality is decided before either body-analysis host can run:
    // declaration binding owns the whole-RIR path, and the durable signature
    // query owns the provider path. The ordinary engine therefore must not
    // become a third signature producer again (RUE-1233).
    assert!(
        !include_str!("sema/ordinary_engine.rs").contains("accessor_signature("),
        "the ordinary body engine regained an accessor signature re-check"
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

    // Semantic consumers receive an immutable RIR: the provider host reads
    // bodies through `BodyRirView`/`BodyRirBundle` borrows and no semantic
    // construction takes a mutable RIR, so no body-analysis entry point is a
    // payload escape hatch.
    let identity = include_str!("sema/body_identity.rs");
    assert!(identity.contains("pub fn from_parts(rir: &'a Rir,"));
    let sema_sources = [
        include_str!("sema/mod.rs"),
        include_str!("sema/body_identity.rs"),
        include_str!("sema/provider_body_host.rs"),
        include_str!("sema/ordinary_engine.rs"),
    ]
    .concat();
    assert!(!sema_sources.contains("&'a mut Rir"));
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
    assert!(import.contains("self.type_pool.complete_type_handles()"));
    assert!(
        !import.contains("self.type_pool.clone().freeze()"),
        "local materialization must not deep-clone the semantic type universe before publication"
    );
    for provider in ["ProviderSpecializedBody", "ProviderAnonymousBody"] {
        let body = providers
            .split(&format!("pub struct {provider}"))
            .nth(1)
            .and_then(|source| source.split("\n}").next())
            .expect("provider result declaration");
        assert!(body.contains("pub function: AnalyzedFunction"));
        assert!(body.contains("pub type_pool: Rc<TypeInternPool>"));
        // The symbol interner is revision-shared and crosses worker threads
        // (ADR-0076); the type pool stays body-private and `Rc`.
        assert!(body.contains("pub interner: Arc<ThreadedRodeo>"));
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
fn retired_source_owned_sema_plane_cannot_return() {
    // RUE-1538: the source-owned `Sema` declaration/body plane is deleted.
    // Body analysis has exactly one production shape: the compiler's query
    // graph supplies durable facts through `BodyFactProvider`, and the
    // provider host drives the shared engine over one demanded body. This
    // gate holds the deletion closed three ways.
    //
    // (a) No retired driver, phase type, or install entry point may reappear
    // anywhere in AIR's production sources. Needles are spelled with
    // `concat!` so the gate does not match itself; `_tests.rs` files and the
    // test fixture are excluded because they spell needle fragments while
    // documenting the migration.
    let production = [
        include_str!("lib.rs"),
        include_str!("specialize.rs"),
        include_str!("sema/aggregate_resolution.rs"),
        include_str!("sema/aggregates.rs"),
        include_str!("sema/analysis.rs"),
        include_str!("sema/analysis/builtin_ops.rs"),
        include_str!("sema/analysis/calls.rs"),
        include_str!("sema/analysis/instructions.rs"),
        include_str!("sema/analysis/intrinsics.rs"),
        include_str!("sema/analysis/ownership.rs"),
        include_str!("sema/analysis/pointers.rs"),
        include_str!("sema/analysis/type_inference.rs"),
        include_str!("sema/analyze_ops.rs"),
        include_str!("sema/anon_structs.rs"),
        include_str!("sema/binding_manifest.rs"),
        include_str!("sema/body_endpoint.rs"),
        include_str!("sema/body_identity.rs"),
        include_str!("sema/call_resolution.rs"),
        include_str!("sema/comptime_eval.rs"),
        include_str!("sema/context.rs"),
        include_str!("sema/control_flow.rs"),
        include_str!("sema/declaration_index.rs"),
        include_str!("sema/declarations.rs"),
        include_str!("sema/fact_mode.rs"),
        include_str!("sema/inference_ctx.rs"),
        include_str!("sema/info.rs"),
        include_str!("sema/known_symbols.rs"),
        include_str!("sema/mod.rs"),
        include_str!("sema/ordinary_engine.rs"),
        include_str!("sema/output.rs"),
        include_str!("sema/provider.rs"),
        include_str!("sema/provider_body_host.rs"),
        include_str!("sema/provider_module_registry.rs"),
        include_str!("sema/semantic_body_export.rs"),
        include_str!("sema/typeck.rs"),
        include_str!("sema/visibility.rs"),
    ]
    .concat();
    for retired in [
        concat!("new_", "synthetic"),
        concat!("bind_", "declarations"),
        concat!("analyze_all", "_for_test"),
        concat!("analyze_all", "_bodies"),
        concat!("analyze_all", "_function_bodies"),
        concat!("predeclare_", "declaration_shells"),
        concat!("analyze_function_bodies", "_lazy"),
        concat!("compose_", "queried_bodies"),
        concat!("resolve_", "declarations"),
        concat!("install_", "declaration_semantics"),
        concat!("install_", "ordinary_body_candidates"),
        concat!("install_", "stable_identity_endpoints"),
        concat!("install_", "body_owner_tokens"),
        concat!("run_", "to_fixpoint"),
        concat!("create_", "specialized_function"),
        concat!("Bound", "Sema"),
        concat!("Body", "Sema"),
        concat!("Sema", "<"),
        concat!("Declaration", "Shells"),
        concat!("Declaration", "Phase"),
        concat!("Mutable", "Declarations"),
        concat!("Source", "Declarations"),
        concat!("Speci", "alizer"),
    ] {
        assert!(
            !production.contains(retired),
            "retired source-owned Sema plane returned: {retired}"
        );
    }

    // (b) Exactly one implementation of the body-analysis host contract
    // exists, and it is the provider host. A renamed source-owned host
    // cannot reappear without tripping this count.
    assert_eq!(
        production.matches("OrdinaryBodyAnalysisHost for").count(),
        1,
        "body analysis must have exactly one host implementation"
    );
    let provider_host = include_str!("sema/provider_body_host.rs");
    assert!(
        provider_host.contains("OrdinaryBodyAnalysisHost for ProviderBodyHost<"),
        "the one host implementation must be the provider host"
    );

    // (c) The production ordinary-body entry point exists and drives the
    // shared engine.
    assert!(
        provider_host.contains("pub fn analyze_provider_ordinary_body"),
        "the provider ordinary-body entry point is the production driver"
    );
    assert!(
        provider_host.contains("OrdinaryBodyEngine::new"),
        "the provider host must construct the shared body engine"
    );
}
