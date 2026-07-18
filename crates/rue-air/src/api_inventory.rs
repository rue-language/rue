//! Structural guard for AIR's read-only canonical import consumption boundary.

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
