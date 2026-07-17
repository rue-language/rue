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
    assert!(pool.contains("pub(crate) fn mark_struct_linear("));
    assert!(pool.contains("pub(crate) fn set_struct_destructor("));
    assert!(pool.contains("pub(crate) fn requalify_struct_destructor("));
}
