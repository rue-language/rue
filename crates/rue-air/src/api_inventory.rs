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
