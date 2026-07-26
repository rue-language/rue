//! Consistency tests for RIR traversal.
//!
//! The sema module traverses the RIR twice with parallel match statements:
//! 1. Constraint generation (inference/generate.rs) - Walks RIR to generate type constraints
//! 2. AIR emission (sema/analysis/ modules) - Walks RIR again to emit typed AIR
//!
//! These tests ensure both passes handle the same instruction types, preventing:
//! - Duplication risk: Easy to add handling for a new instruction in one pass but forget the other
//! - Consistency bugs: Subtle differences in how the two passes interpret the same instruction

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    /// All InstData variants that exist in the RIR.
    ///
    /// This list must be kept in sync with rue-rir's InstData enum.
    /// When adding a new variant to InstData, add it here - the test will
    /// then fail if either pass doesn't handle it.
    const ALL_INSTDATA_VARIANTS: &[&str] = &[
        // Constants
        "IntConst",
        "BoolConst",
        "StringConst",
        "UnitConst",
        // Binary arithmetic
        "Add",
        "Sub",
        "Mul",
        "Div",
        "Mod",
        // Comparisons
        "Eq",
        "Ne",
        "Lt",
        "Gt",
        "Le",
        "Ge",
        // Logical
        "And",
        "Or",
        // Bitwise
        "BitAnd",
        "BitOr",
        "BitXor",
        "Shl",
        "Shr",
        // Unary
        "Neg",
        "Not",
        "BitNot",
        // Control flow
        "Branch",
        "Loop",
        "InfiniteLoop",
        "Match",
        "Break",
        "Continue",
        // Functions
        "FnDecl",
        "Call",
        "Intrinsic",
        "TypeIntrinsic",
        "OffsetOf",
        "Ret",
        // Blocks
        "Block",
        // Variables
        "Alloc",
        "VarRef",
        "Assign",
        // Structs
        "StructDecl",
        "StructInit",
        "FieldGet",
        "FieldSet",
        // Enums
        "EnumDecl",
        "EnumVariant",
        // Arrays
        "ArrayInit",
        "ArrayRepeat",
        "IndexGet",
        "IndexSet",
        // Methods
        "MethodCall",
        "DropFnDecl",
    ];

    // Include the source files at compile time
    // These paths are relative to the current source file
    const GENERATE_SOURCE: &str = include_str!("../inference/generate.rs");
    // The AIR-emission pass was split out of `analysis.rs` into per-category
    // submodules (RUE-4); the RIR-walking `InstData::` match arms now live in
    // those submodules, so scan the whole `analysis/` tree, not just the root.
    const ANALYSIS_SOURCE: &str = concat!(
        include_str!("analysis.rs"),
        include_str!("analysis/functions.rs"),
        include_str!("analysis/type_inference.rs"),
        include_str!("analysis/instructions.rs"),
        include_str!("analysis/calls.rs"),
        include_str!("analysis/intrinsics.rs"),
        include_str!("analysis/builtin_ops.rs"),
        include_str!("analysis/ownership.rs"),
        include_str!("analysis/anon_methods.rs"),
        include_str!("analysis/pointers.rs"),
        include_str!("aggregates.rs"),
        include_str!("control_flow.rs"),
    );

    const ANALYZE_OPS_SOURCE: &str = include_str!("analyze_ops.rs");
    const AGGREGATES_SOURCE: &str = include_str!("aggregates.rs");
    const BUILTIN_OPS_SOURCE: &str = include_str!("analysis/builtin_ops.rs");
    const CALLS_SOURCE: &str = include_str!("analysis/calls.rs");
    const FUNCTIONS_SOURCE: &str = include_str!("analysis/functions.rs");
    const INTRINSICS_SOURCE: &str = include_str!("analysis/intrinsics.rs");
    const SEMA_ROOT_SOURCE: &str = include_str!("mod.rs");
    const ANALYSIS_ROOT_SOURCE: &str = include_str!("analysis.rs");
    const INSTRUCTIONS_SOURCE: &str = include_str!("analysis/instructions.rs");
    const OWNERSHIP_SOURCE: &str = include_str!("analysis/ownership.rs");
    const TYPE_INFERENCE_SOURCE: &str = include_str!("analysis/type_inference.rs");
    const CONTROL_FLOW_SOURCE: &str = include_str!("control_flow.rs");
    const FACT_MODE_SOURCE: &str = include_str!("fact_mode.rs");
    const BODY_ENDPOINT_SOURCE: &str = include_str!("body_endpoint.rs");
    const CALL_RESOLUTION_SOURCE: &str = include_str!("call_resolution.rs");
    const AGGREGATE_RESOLUTION_SOURCE: &str = include_str!("aggregate_resolution.rs");
    const INFERENCE_CONTEXT_SOURCE: &str = include_str!("inference_ctx.rs");
    const TYPECK_SOURCE: &str = include_str!("typeck.rs");
    const DECLARATION_BASE_SOURCE: &str = include_str!("declaration_base.rs");
    const SEMANTIC_BODY_EXPORT_SOURCE: &str = include_str!("semantic_body_export.rs");
    const VISIBILITY_SOURCE: &str = include_str!("visibility.rs");
    const ONE_BODY_SOURCE: &str = include_str!("one_body.rs");
    const BINDING_MANIFEST_SOURCE: &str = include_str!("binding_manifest.rs");
    const CALL_INTRINSIC_PEER_SOURCE: &str = concat!(
        include_str!("mod.rs"),
        include_str!("aggregates.rs"),
        include_str!("analysis.rs"),
        include_str!("analysis/anon_methods.rs"),
        include_str!("analysis/builtin_ops.rs"),
        include_str!("analysis/functions.rs"),
        include_str!("analysis/instructions.rs"),
        include_str!("analysis/ownership.rs"),
        include_str!("analysis/pointers.rs"),
        include_str!("analysis/type_inference.rs"),
        include_str!("analyze_ops.rs"),
        include_str!("anon_structs.rs"),
        include_str!("binding_manifest.rs"),
        include_str!("builtins.rs"),
        include_str!("comptime_eval.rs"),
        include_str!("context.rs"),
        include_str!("control_flow.rs"),
        include_str!("declaration_index.rs"),
        include_str!("declarations.rs"),
        include_str!("file_paths.rs"),
        include_str!("inference_ctx.rs"),
        include_str!("info.rs"),
        include_str!("known_symbols.rs"),
        include_str!("metadata.rs"),
        include_str!("output.rs"),
        include_str!("semantic_body_export.rs"),
        include_str!("typeck.rs"),
        include_str!("visibility.rs"),
    );
    const OWNERSHIP_PEER_SOURCES: &[(&str, &str)] = &[
        ("analysis.rs", ANALYSIS_ROOT_SOURCE),
        ("aggregates.rs", AGGREGATES_SOURCE),
        ("analyze_ops.rs", ANALYZE_OPS_SOURCE),
        ("anon_methods.rs", include_str!("analysis/anon_methods.rs")),
        ("builtin_ops.rs", include_str!("analysis/builtin_ops.rs")),
        ("calls.rs", include_str!("analysis/calls.rs")),
        ("functions.rs", include_str!("analysis/functions.rs")),
        ("instructions.rs", include_str!("analysis/instructions.rs")),
        ("intrinsics.rs", include_str!("analysis/intrinsics.rs")),
        ("pointers.rs", include_str!("analysis/pointers.rs")),
        (
            "type_inference.rs",
            include_str!("analysis/type_inference.rs"),
        ),
    ];

    /// Extract InstData variant names from source code.
    ///
    /// Looks for patterns like `InstData::VariantName` and extracts `VariantName`.
    /// Excludes matches that are actually `AirInstData::` (which contains `InstData::` as substring).
    fn extract_instdata_variants(source: &str) -> HashSet<String> {
        let mut variants = HashSet::new();

        // Simple regex-like extraction using string matching
        // Looking for "InstData::" followed by variant name (alphanumeric)
        // But NOT "AirInstData::" which contains our pattern as a substring
        for line in source.lines() {
            let mut remaining = line;
            while let Some(idx) = remaining.find("InstData::") {
                // Check if this is actually "AirInstData::" by looking back
                let is_air_instdata = idx >= 3 && remaining[idx - 3..idx] == *"Air";

                if !is_air_instdata {
                    let after_prefix = &remaining[idx + "InstData::".len()..];
                    // Extract the variant name (alphanumeric characters)
                    let variant: String = after_prefix
                        .chars()
                        .take_while(|c| c.is_alphanumeric() || *c == '_')
                        .collect();
                    if !variant.is_empty() {
                        variants.insert(variant);
                    }
                }
                // Move past this match to find more on the same line
                remaining = &remaining[idx + "InstData::".len()..];
            }
        }

        variants
    }

    #[test]
    fn generate_and_analysis_handle_same_instdata_variants() {
        // Extract variants from each file
        let generate_variants = extract_instdata_variants(GENERATE_SOURCE);
        let analysis_variants = extract_instdata_variants(ANALYSIS_SOURCE);

        // Find variants handled by generate.rs but not analysis.rs
        let mut only_in_generate: Vec<_> = generate_variants
            .difference(&analysis_variants)
            .cloned()
            .collect();
        only_in_generate.sort();

        // Find variants handled by analysis.rs but not generate.rs
        let mut only_in_analysis: Vec<_> = analysis_variants
            .difference(&generate_variants)
            .cloned()
            .collect();
        only_in_analysis.sort();

        // Build error message if there are differences
        let mut errors = Vec::new();
        if !only_in_generate.is_empty() {
            errors.push(format!(
                "Variants in generate.rs but not analysis.rs: {:?}",
                only_in_generate
            ));
        }
        if !only_in_analysis.is_empty() {
            errors.push(format!(
                "Variants in analysis.rs but not generate.rs: {:?}",
                only_in_analysis
            ));
        }

        assert!(
            errors.is_empty(),
            "InstData variant handling mismatch between constraint generation and AIR emission:\n{}",
            errors.join("\n")
        );
    }

    #[test]
    fn body_epoch_derivation_has_a_data_only_base_and_fresh_local_overlays() {
        let base = SEMA_ROOT_SOURCE
            .split("pub(super) struct BodySemanticBase")
            .nth(1)
            .and_then(|source| source.split("pub(super) struct BodyAnalysisState").next())
            .expect("BodySemanticBase definition is present");
        for forbidden in ["Sema<", "Sema::", "&Sema", "BodySema", "BoundSema", "Deref"] {
            assert!(
                !base.contains(forbidden),
                "BodySemanticBase must be data-only, not retain {forbidden}"
            );
        }

        let local = SEMA_ROOT_SOURCE
            .split("struct BodyLocalSeed")
            .nth(1)
            .and_then(|source| source.split("/// Semantic analyzer").next())
            .expect("BodyLocalSeed definition is present");
        for forbidden in ["Sema<", "Sema::", "&Sema", "BodySema", "BoundSema", "Deref"] {
            assert!(
                !local.contains(forbidden),
                "BodyLocalSeed must be data-only, not retain {forbidden}"
            );
        }

        let derive = DECLARATION_BASE_SOURCE
            .split("pub fn derive_body_epoch")
            .nth(1)
            .and_then(|source| source.split("/// Type-pool entries").next())
            .expect("body derivation is present");
        assert!(derive.contains("body_base"));
        assert!(derive.contains("derive_from_body_base"));
        assert!(!derive.contains("self.clone()"));
        assert!(!SEMA_ROOT_SOURCE.contains("impl<'a, D> Clone for Sema"));
        assert!(!include_str!("binding_manifest.rs").contains("Clone for BoundSema"));

        let construction = SEMA_ROOT_SOURCE
            .split("fn derive_from_body_semantic_base")
            .nth(1)
            .and_then(|source| {
                source
                    .split("impl<D: DeclarationPhase> std::ops::Deref")
                    .next()
            })
            .expect("base construction is present");
        assert!(construction.contains("type_pool: base.type_pool.derive_overlay()"));
        assert!(construction.contains("param_arena: base.param_arena.derive_overlay()"));
        assert!(construction.contains("body_analysis_work: BodyAnalysisWork::default()"));
        assert!(construction.contains("body_named_dependencies: Vec::new()"));
        assert!(DECLARATION_BASE_SOURCE.contains("+ local.deferred_ownership_gates.len()"));
        assert!(DECLARATION_BASE_SOURCE.contains("forced_anonymous_digest_entries"));
    }

    #[test]
    fn ordinary_owner_identity_uses_the_owned_body_analysis_state() {
        let state = SEMA_ROOT_SOURCE
            .split("pub(super) struct BodyAnalysisState")
            .nth(1)
            .and_then(|source| source.split("/// The declaration-time portion").next())
            .expect("BodyAnalysisState definition is present");
        for forbidden in [
            "Sema<",
            "Sema::",
            "&Sema",
            "BodySema<",
            "BodySema::",
            "BoundSema",
            "sema:",
            "Deref",
        ] {
            assert!(
                !state.contains(forbidden),
                "BodyAnalysisState must not retain {forbidden}"
            );
        }

        let ordinary = ONE_BODY_SOURCE
            .split("fn analyze_definition(")
            .nth(1)
            .and_then(|source| source.split("fn analyze_named_method(").next())
            .expect("ordinary definition analyzer is present");
        assert!(ordinary.contains("state: &BodyAnalysisState<'_>"));
        assert!(ordinary.contains("let owner = state.body_owner_token("));
        assert!(!ordinary.contains("BodyAnalysisState::from_body_semantic_base"));
    }

    #[test]
    fn body_publication_and_shared_identity_have_exact_receivers() {
        fn impl_items(source: &str) -> Vec<&str> {
            let mut items = Vec::new();
            let mut remaining = source;
            while let Some(start) = remaining.find("\nimpl") {
                let item = &remaining[start + 1..];
                let open = item.find('{').expect("impl body opens");
                let mut depth = 0;
                let mut end = None;
                for (offset, ch) in item[open..].char_indices() {
                    match ch {
                        '{' => depth += 1,
                        '}' => {
                            depth -= 1;
                            if depth == 0 {
                                end = Some(open + offset + 1);
                                break;
                            }
                        }
                        _ => {}
                    }
                }
                let end = end.expect("impl body closes");
                items.push(&item[..end]);
                remaining = &item[end..];
            }
            items
        }

        fn method_names(item: &str) -> Vec<&str> {
            item.lines()
                .filter_map(|line| {
                    let line = line.strip_prefix("    ")?;
                    let declaration = line
                        .strip_prefix("fn ")
                        .or_else(|| line.split_once(" fn ").map(|(_, declaration)| declaration))?;
                    declaration.split_once('(').map(|(name, _)| name)
                })
                .collect()
        }

        let impls = impl_items(SEMANTIC_BODY_EXPORT_SOURCE);
        assert_eq!(
            impls.len(),
            5,
            "receiver partition has five cohesive blocks"
        );
        assert_eq!(
            impls
                .iter()
                .filter(|item| item.starts_with("impl BodySema<'_>"))
                .count(),
            3
        );
        assert_eq!(
            impls
                .iter()
                .filter(|item| item.starts_with("impl<D: DeclarationPhase> Sema<'_, D>"))
                .count(),
            2
        );

        let body_methods = [
            "export_specialized_body",
            "export_ordinary_body",
            "export_one_body_with_specializations",
            "export_anonymous_body_with_specializations",
            "export_body",
            "export_body_type",
            "body_function_identity",
            "body_struct_identity",
            "body_enum_identity",
        ];
        for method in body_methods {
            let needle = format!("fn {method}(");
            let owners = impls
                .iter()
                .filter(|item| item.contains(&needle))
                .collect::<Vec<_>>();
            assert_eq!(owners.len(), 1, "{method} has one implementation");
            assert!(
                owners[0].starts_with("impl BodySema<'_>"),
                "{method} must be body-only"
            );
        }

        let shared_methods = [
            "function_identity",
            "struct_identity",
            "enum_identity",
            "stable_definition_token",
        ];
        for method in shared_methods {
            let needle = format!("fn {method}(");
            let owners = impls
                .iter()
                .filter(|item| item.contains(&needle))
                .collect::<Vec<_>>();
            assert_eq!(owners.len(), 1, "{method} has one implementation");
            assert!(
                owners[0].starts_with("impl<D: DeclarationPhase> Sema<'_, D>"),
                "{method} remains shared with declaration installation"
            );
        }

        let mut actual_body_methods = impls
            .iter()
            .filter(|item| item.starts_with("impl BodySema<'_>"))
            .flat_map(|item| method_names(item))
            .collect::<Vec<_>>();
        actual_body_methods.sort_unstable();
        let mut expected_body_methods = body_methods.to_vec();
        expected_body_methods.sort_unstable();
        assert_eq!(actual_body_methods, expected_body_methods);

        let mut actual_shared_methods = impls
            .iter()
            .filter(|item| item.starts_with("impl<D: DeclarationPhase> Sema<'_, D>"))
            .flat_map(|item| method_names(item))
            .collect::<Vec<_>>();
        actual_shared_methods.sort_unstable();
        let mut expected_shared_methods = shared_methods.to_vec();
        expected_shared_methods.sort_unstable();
        assert_eq!(actual_shared_methods, expected_shared_methods);

        for forbidden in ["Deref", "\ntrait "] {
            assert!(
                !SEMANTIC_BODY_EXPORT_SOURCE.contains(forbidden),
                "publication must not gain a wrapper authority through {forbidden}"
            );
        }

        for method in [
            "export_one_body_with_specializations",
            "export_specialized_body",
            "export_anonymous_body_with_specializations",
        ] {
            assert!(ONE_BODY_SOURCE.contains(&format!("sema.{method}(")));
        }
        assert!(ANALYSIS_ROOT_SOURCE.contains("sema.export_ordinary_body("));
        assert!(BINDING_MANIFEST_SOURCE.contains("self.stable_definition_token("));
    }

    #[test]
    fn string_shape_classification_has_exact_body_receiver() {
        fn impl_items(source: &str) -> Vec<&str> {
            let mut items = Vec::new();
            let mut remaining = source;
            while let Some(start) = remaining.find("\nimpl") {
                let item = &remaining[start + 1..];
                let open = item.find('{').expect("impl body opens");
                let mut depth = 0;
                let mut end = None;
                for (offset, ch) in item[open..].char_indices() {
                    match ch {
                        '{' => depth += 1,
                        '}' => {
                            depth -= 1;
                            if depth == 0 {
                                end = Some(open + offset + 1);
                                break;
                            }
                        }
                        _ => {}
                    }
                }
                let end = end.expect("impl body closes");
                items.push(&item[..end]);
                remaining = &item[end..];
            }
            items
        }

        fn method_names(item: &str) -> Vec<&str> {
            item.lines()
                .filter_map(|line| {
                    let line = line.strip_prefix("    ")?;
                    let declaration = line
                        .strip_prefix("fn ")
                        .or_else(|| line.split_once(" fn ").map(|(_, declaration)| declaration))?;
                    declaration.split_once('(').map(|(name, _)| name)
                })
                .collect()
        }

        let typeck_impls = impl_items(TYPECK_SOURCE);
        let classification_methods = [
            "is_str_struct",
            "is_str_fixed_struct",
            "str_fixed_capacity",
            "is_str_like",
        ];
        let body_owner = typeck_impls
            .iter()
            .find(|item| item.contains("fn is_str_struct("))
            .expect("string classification owner is present");
        assert!(
            body_owner.starts_with("impl BodySema<'_>"),
            "string classification must be body-only"
        );

        for method in classification_methods {
            let needle = format!("fn {method}(");
            let owners = typeck_impls
                .iter()
                .filter(|item| item.contains(&needle))
                .collect::<Vec<_>>();
            assert_eq!(owners.len(), 1, "{method} has one implementation");
            assert_eq!(
                *owners[0], *body_owner,
                "{method} shares the cohesive body-only owner"
            );
        }

        let mut actual_methods = method_names(body_owner);
        actual_methods.sort_unstable();
        let mut expected_methods = classification_methods.to_vec();
        expected_methods.sort_unstable();
        assert_eq!(actual_methods, expected_methods);

        for item in typeck_impls.iter().filter(|item| {
            let header = item.lines().next().expect("impl has a header");
            header.contains("DeclarationPhase") && header.contains("Sema<")
        }) {
            for method in classification_methods {
                assert!(
                    !item.contains(&format!("fn {method}(")),
                    "{method} must not remain generically available"
                );
            }
        }

        let constructor_owners = typeck_impls
            .iter()
            .filter(|item| item.contains("fn get_or_create_str_fixed_struct("))
            .collect::<Vec<_>>();
        assert_eq!(
            constructor_owners.len(),
            1,
            "fixed-string constructor has one implementation"
        );
        assert!(
            constructor_owners[0].starts_with("impl<'a, D: DeclarationPhase> Sema<'a, D>"),
            "fixed-string construction remains declaration-phase generic"
        );

        let type_syntax_call_owners = typeck_impls
            .iter()
            .filter(|item| item.contains("self.get_or_create_str_fixed_struct(capacity, span)"))
            .collect::<Vec<_>>();
        assert_eq!(
            type_syntax_call_owners.len(),
            1,
            "type-syntax fixed-string construction has one generic owner"
        );
        assert!(
            type_syntax_call_owners[0]
                .starts_with("impl<'source, D: DeclarationPhase> TypeSyntaxHost")
        );

        let binding_impls = impl_items(BINDING_MANIFEST_SOURCE);
        let declaration_install_owners = binding_impls
            .iter()
            .filter(|item| {
                item.contains(".get_or_create_str_fixed_struct(capacity, Span::default())")
            })
            .collect::<Vec<_>>();
        assert_eq!(
            declaration_install_owners.len(),
            1,
            "declaration installation has one fixed-string construction call"
        );
        assert!(
            declaration_install_owners[0].starts_with("impl<'a> DeclarationShells<'a>"),
            "declaration installation owns its generic-constructor call"
        );

        for (name, source, anchor) in [
            (
                "analyze_ops.rs",
                ANALYZE_OPS_SOURCE,
                "self.str_fixed_capacity(",
            ),
            ("aggregates.rs", AGGREGATES_SOURCE, "self.is_str_struct("),
            (
                "functions.rs",
                FUNCTIONS_SOURCE,
                "self.is_str_fixed_struct(",
            ),
            ("ownership.rs", OWNERSHIP_SOURCE, "self.is_str_like("),
            (
                "type_inference.rs",
                TYPE_INFERENCE_SOURCE,
                "self.is_str_fixed_struct(",
            ),
            ("intrinsics.rs", INTRINSICS_SOURCE, "self.is_str_like("),
            ("builtin_ops.rs", BUILTIN_OPS_SOURCE, "self.is_str_like("),
            (
                "control_flow.rs",
                CONTROL_FLOW_SOURCE,
                "self.is_str_struct(",
            ),
        ] {
            assert!(
                source.contains(anchor),
                "{name} keeps a representative body-only production caller"
            );
        }
    }

    #[test]
    fn materialized_layout_helpers_have_exact_body_receiver() {
        fn impl_items(source: &str) -> Vec<&str> {
            let mut items = Vec::new();
            let mut remaining = source;
            while let Some(start) = remaining.find("\nimpl") {
                let item = &remaining[start + 1..];
                let open = item.find('{').expect("impl body opens");
                let mut depth = 0;
                let mut end = None;
                for (offset, ch) in item[open..].char_indices() {
                    match ch {
                        '{' => depth += 1,
                        '}' => {
                            depth -= 1;
                            if depth == 0 {
                                end = Some(open + offset + 1);
                                break;
                            }
                        }
                        _ => {}
                    }
                }
                let end = end.expect("impl body closes");
                items.push(&item[..end]);
                remaining = &item[end..];
            }
            items
        }

        fn method_names(item: &str) -> Vec<&str> {
            item.lines()
                .filter_map(|line| {
                    let mut declaration = line.strip_prefix("    ")?;
                    if declaration.starts_with(char::is_whitespace) {
                        return None;
                    }
                    if let Some(rest) = declaration.strip_prefix("pub ") {
                        declaration = rest;
                    } else if declaration.starts_with("pub(") {
                        let visibility_end = declaration.find(") ")?;
                        declaration = &declaration[visibility_end + 2..];
                    }
                    let declaration = declaration.strip_prefix("fn ")?;
                    declaration.split_once('(').map(|(name, _)| name)
                })
                .collect()
        }

        assert_eq!(
            method_names(
                "impl Probe {\n\
                 \x20\x20\x20\x20fn private() {}\n\
                 \x20\x20\x20\x20pub fn public() {}\n\
                 \x20\x20\x20\x20pub(crate) fn crate_visible() {}\n\
                 \x20\x20\x20\x20pub(in crate::sema) fn scoped() {}\n\
                 \x20\x20\x20\x20\x20\x20\x20\x20fn nested() {}\n\
                 \x20\x20\x20\x20// pub(crate) fn comment() {}\n\
                 }"
            ),
            ["private", "public", "crate_visible", "scoped"]
        );

        fn method_item<'a>(item: &'a str, name: &str) -> &'a str {
            let needle = format!("fn {name}(");
            let start = item.find(&needle).expect("method is present");
            let open = start + item[start..].find('{').expect("method body opens");
            let mut depth = 0;
            let mut end = None;
            for (offset, ch) in item[open..].char_indices() {
                match ch {
                    '{' => depth += 1,
                    '}' => {
                        depth -= 1;
                        if depth == 0 {
                            end = Some(open + offset + 1);
                            break;
                        }
                    }
                    _ => {}
                }
            }
            &item[start..end.expect("method body closes")]
        }

        let typeck_impls = impl_items(TYPECK_SOURCE);
        let layout_methods = [
            "require_layout_slots",
            "reserve_frame_slots",
            "checked_abi_slot_count",
            "abi_slot_count",
        ];
        let body_owner = typeck_impls
            .iter()
            .find(|item| item.contains("fn require_layout_slots("))
            .expect("materialized-layout owner is present");
        assert!(
            body_owner.starts_with("impl BodySema<'_>"),
            "materialized layout policy must be body-only"
        );

        for method in layout_methods {
            let needle = format!("fn {method}(");
            let owners = typeck_impls
                .iter()
                .filter(|item| item.contains(&needle))
                .collect::<Vec<_>>();
            assert_eq!(owners.len(), 1, "{method} has one implementation");
            assert_eq!(
                *owners[0], *body_owner,
                "{method} shares the cohesive body-only owner"
            );
        }

        let mut actual_methods = method_names(body_owner);
        actual_methods.sort_unstable();
        let mut expected_methods = layout_methods.to_vec();
        expected_methods.sort_unstable();
        assert_eq!(actual_methods, expected_methods);

        for item in typeck_impls.iter().filter(|item| {
            let header = item.lines().next().expect("impl has a header");
            header.contains("DeclarationPhase") && header.contains("Sema<")
        }) {
            for method in layout_methods {
                assert!(
                    !item.contains(&format!("fn {method}(")),
                    "{method} must not remain generically available"
                );
            }
        }

        for method in [
            "get_or_create_array_type",
            "pre_create_array_types_from_infer_type",
            "infer_type_to_concrete_type_for_key",
        ] {
            let needle = format!("fn {method}(");
            let owners = typeck_impls
                .iter()
                .filter(|item| item.contains(&needle))
                .collect::<Vec<_>>();
            assert_eq!(owners.len(), 1, "{method} has one implementation");
            assert!(
                owners[0].starts_with("impl<'a, D: DeclarationPhase> Sema<'a, D>"),
                "{method} remains declaration-phase generic"
            );
        }

        let require = method_item(body_owner, "require_layout_slots");
        assert!(require.contains("match self.checked_abi_slot_count(ty)"));

        let reserve = method_item(body_owner, "reserve_frame_slots");
        assert!(reserve.contains("crate::layout::checked_function_frame_slots(start, additional)"));

        let checked = method_item(body_owner, "checked_abi_slot_count");
        for edge in [
            "self.checked_abi_slot_count(element_type)?",
            "self.checked_abi_slot_count(f.ty)?",
            "self.checked_abi_slot_count(vty)?",
            "u64::from(self.abi_slot_count(ty))",
        ] {
            assert!(
                checked.contains(edge),
                "checked layout retains its recursive or fallback edge: {edge}"
            );
        }

        let abi = method_item(body_owner, "abi_slot_count");
        assert!(
            abi.contains("self.type_pool.provisional_abi_slot_count(ty)"),
            "the Sema helper delegates to the distinct TypeInternPool computation"
        );

        for (name, source, anchors) in [
            (
                "intrinsics.rs",
                INTRINSICS_SOURCE,
                &["self.require_layout_slots("][..],
            ),
            (
                "aggregates.rs",
                AGGREGATES_SOURCE,
                &["self.require_layout_slots("][..],
            ),
            (
                "functions.rs",
                FUNCTIONS_SOURCE,
                &["self.require_layout_slots(", "self.reserve_frame_slots("][..],
            ),
            (
                "ownership.rs",
                OWNERSHIP_SOURCE,
                &["self.require_layout_slots(", "self.reserve_frame_slots("][..],
            ),
        ] {
            for anchor in anchors {
                assert!(
                    source.contains(anchor),
                    "{name} keeps its materialized-layout call to {anchor}"
                );
            }
        }
    }

    #[test]
    fn module_enum_visibility_has_exact_body_and_shared_receivers() {
        fn impl_items(source: &str) -> Vec<&str> {
            let mut items = Vec::new();
            let mut remaining = source;
            while let Some(start) = remaining.find("\nimpl") {
                let item = &remaining[start + 1..];
                let open = item.find('{').expect("impl body opens");
                let mut depth = 0;
                let mut end = None;
                for (offset, ch) in item[open..].char_indices() {
                    match ch {
                        '{' => depth += 1,
                        '}' => {
                            depth -= 1;
                            if depth == 0 {
                                end = Some(open + offset + 1);
                                break;
                            }
                        }
                        _ => {}
                    }
                }
                let end = end.expect("impl body closes");
                items.push(&item[..end]);
                remaining = &item[end..];
            }
            items
        }

        fn method_names(item: &str) -> Vec<&str> {
            item.lines()
                .filter_map(|line| {
                    let line = line.strip_prefix("    ")?;
                    let declaration = line
                        .strip_prefix("fn ")
                        .or_else(|| line.split_once(" fn ").map(|(_, declaration)| declaration))?;
                    declaration.split_once('(').map(|(name, _)| name)
                })
                .collect()
        }

        let impls = impl_items(VISIBILITY_SOURCE);
        assert_eq!(impls.len(), 2, "visibility has exactly two receiver blocks");
        let generic = impls
            .iter()
            .find(|item| item.starts_with("impl<D: DeclarationPhase> Sema<'_, D>"))
            .expect("shared visibility receiver exists");
        let body = impls
            .iter()
            .find(|item| item.starts_with("impl BodySema<'_>"))
            .expect("body visibility receiver exists");

        let mut generic_methods = method_names(generic);
        generic_methods.sort_unstable();
        assert_eq!(
            generic_methods,
            ["check_unqualified_visibility", "is_accessible"]
        );
        let mut body_methods = method_names(body);
        body_methods.sort_unstable();
        assert_eq!(
            body_methods,
            ["module_file_for_ref", "resolve_enum_through_module"]
        );

        for method in ["resolve_enum_through_module", "module_file_for_ref"] {
            let needle = format!("fn {method}(");
            assert_eq!(
                VISIBILITY_SOURCE.matches(&needle).count(),
                1,
                "{method} has one implementation"
            );
            assert!(body.contains(&needle), "{method} is body-only");
            assert!(
                !generic.contains(&needle),
                "{method} has no generic forwarding shim"
            );
        }
        for forbidden in ["Deref", "\ntrait "] {
            assert!(
                !VISIBILITY_SOURCE.contains(forbidden),
                "visibility must not gain an authority escape through {forbidden}"
            );
        }
        assert!(AGGREGATES_SOURCE.contains("self.resolve_enum_through_module("));
        assert!(CONTROL_FLOW_SOURCE.contains("self.resolve_enum_through_module("));
    }

    #[test]
    fn both_passes_handle_all_instdata_variants() {
        // Extract variants from each file
        let generate_variants = extract_instdata_variants(GENERATE_SOURCE);
        let analysis_variants = extract_instdata_variants(ANALYSIS_SOURCE);

        // Get all expected variants
        let all_variants: HashSet<String> = ALL_INSTDATA_VARIANTS
            .iter()
            .map(|s| s.to_string())
            .collect();

        // Find variants not handled by generate.rs
        let mut missing_in_generate: Vec<_> = all_variants
            .difference(&generate_variants)
            .cloned()
            .collect();
        missing_in_generate.sort();

        // Find variants not handled by analysis.rs
        let mut missing_in_analysis: Vec<_> = all_variants
            .difference(&analysis_variants)
            .cloned()
            .collect();
        missing_in_analysis.sort();

        // Build error message if there are missing handlers
        let mut errors = Vec::new();
        if !missing_in_generate.is_empty() {
            errors.push(format!(
                "InstData variants missing from generate.rs: {:?}",
                missing_in_generate
            ));
        }
        if !missing_in_analysis.is_empty() {
            errors.push(format!(
                "InstData variants missing from analysis.rs: {:?}",
                missing_in_analysis
            ));
        }

        assert!(
            errors.is_empty(),
            "Not all InstData variants are handled:\n{}\n\
             \nIf a new variant was added to InstData, add it to ALL_INSTDATA_VARIANTS \
             and ensure both generate.rs and analysis.rs handle it.",
            errors.join("\n")
        );
    }

    // NOTE: We cannot automatically check ALL_INSTDATA_VARIANTS against the actual
    // InstData enum in rue-rir because Buck2's sandboxed build environment doesn't
    // allow include_str! paths across crate boundaries. The solution is to keep
    // ALL_INSTDATA_VARIANTS manually in sync with rue_rir::InstData.
    //
    // When adding a new InstData variant:
    // 1. Add it to rue-rir/src/inst.rs (InstData enum)
    // 2. Add it to ALL_INSTDATA_VARIANTS above
    // 3. Handle it in inference/generate.rs
    // 4. Handle it in sema/analysis.rs
    //
    // The tests below will catch if steps 3 or 4 are missed.

    #[test]
    fn extract_instdata_variants_works() {
        // Unit test for the extraction function
        let source = r#"
            match inst.data {
                InstData::IntConst(_) => {},
                InstData::Add { lhs, rhs } | InstData::Sub { lhs, rhs } => {},
                InstData::Call { name, .. } => {},
            }
        "#;

        let variants = extract_instdata_variants(source);
        assert!(variants.contains("IntConst"));
        assert!(variants.contains("Add"));
        assert!(variants.contains("Sub"));
        assert!(variants.contains("Call"));
        assert_eq!(variants.len(), 4);
    }

    #[test]
    fn control_flow_analysis_has_one_cohesive_owner() {
        for method in [
            "analyze_control_flow",
            "analyze_branch",
            "analyze_while_loop",
            "analyze_infinite_loop",
            "analyze_match",
            "analyze_try",
            "analyze_return",
            "analyze_block",
            "resolve_pattern_enum",
        ] {
            let definition = format!("fn {method}(");
            assert!(
                CONTROL_FLOW_SOURCE.contains(&definition),
                "control_flow.rs must own {method}"
            );
            assert!(
                !ANALYZE_OPS_SOURCE.contains(&definition),
                "analyze_ops.rs must not retain {method}"
            );
        }

        assert!(CONTROL_FLOW_SOURCE.contains("impl<'a> BodySema<'a>"));
        assert!(!CONTROL_FLOW_SOURCE.contains("struct BodySema"));
    }

    #[test]
    fn one_body_host_contract_is_representation_agnostic() {
        assert!(FACT_MODE_SOURCE.contains("trait BodyAnalysisHost"));
        assert!(FACT_MODE_SOURCE.contains("type EndpointFacts"));
        assert!(FACT_MODE_SOURCE.contains("type CallFacts"));
        assert!(FACT_MODE_SOURCE.contains("type AggregateFacts"));
        assert!(FACT_MODE_SOURCE.contains("type InferenceFacts"));
        assert!(FACT_MODE_SOURCE.contains("struct TypeSyntaxRequest"));
        assert!(FACT_MODE_SOURCE.contains("struct ModulePrefixRequest"));
        assert!(FACT_MODE_SOURCE.contains("struct ArrayLengthRequest"));
        assert!(FACT_MODE_SOURCE.contains("struct DeferredTypeRequest"));
        assert!(FACT_MODE_SOURCE.contains("fn resolve_type_syntax("));
        assert!(FACT_MODE_SOURCE.contains("fn resolve_type_module_prefix"));
        assert!(FACT_MODE_SOURCE.contains("fn resolve_array_length("));
        assert!(FACT_MODE_SOURCE.contains("fn validate_deferred_type("));
        let contract = FACT_MODE_SOURCE
            .split("pub(crate) trait BodyAnalysisHost")
            .nth(1)
            .and_then(|source| source.split("/// The canonical epoch host").next())
            .expect("host contract is present");
        for forbidden in ["Sema", "BodySema", "DeclarationPhase", "Epoch"] {
            assert!(
                !contract.contains(forbidden),
                "host contract must not name {forbidden}"
            );
        }
        assert!(!FACT_MODE_SOURCE.contains("analyze_one_body"));
        assert!(!SEMA_ROOT_SOURCE.contains("\n    fact_mode:"));
        assert!(SEMA_ROOT_SOURCE.contains("fn endpoint_facts("));
        assert!(SEMA_ROOT_SOURCE.contains("fn call_facts("));
        assert!(SEMA_ROOT_SOURCE.contains("fn aggregate_facts("));
        assert!(SEMA_ROOT_SOURCE.contains("fn inference_facts"));
    }

    #[test]
    fn one_body_fact_adapters_are_generic_and_retain_no_concrete_analyzer() {
        fn item<'source>(source: &'source str, header: &str) -> &'source str {
            let start = source.find(header).expect("item header is present");
            let rest = &source[start..];
            let open = rest.find('{').expect("item body opens");
            let mut depth = 0;
            for (offset, ch) in rest[open..].char_indices() {
                match ch {
                    '{' => depth += 1,
                    '}' => {
                        depth -= 1;
                        if depth == 0 {
                            return &rest[..open + offset + 1];
                        }
                    }
                    _ => {}
                }
            }
            panic!("item body closes");
        }

        let adapters = [
            (
                BODY_ENDPOINT_SOURCE,
                "pub(crate) struct EpochFacts<'host, H: super::fact_mode::BodyAnalysisReadHost>",
                "impl<H: super::fact_mode::BodyAnalysisReadHost> BodyEndpointProvider for EpochFacts<'_, H>",
            ),
            (
                CALL_RESOLUTION_SOURCE,
                "pub(crate) struct EpochFacts<'host, H: super::fact_mode::BodyAnalysisReadHost>",
                "impl<H: super::fact_mode::BodyAnalysisReadHost> CallResolutionFacts for EpochFacts<'_, H>",
            ),
            (
                AGGREGATE_RESOLUTION_SOURCE,
                "pub(crate) struct EpochFacts<'host, H: super::fact_mode::BodyAnalysisReadHost>",
                "impl<H: super::fact_mode::BodyAnalysisReadHost> AggregateFacts for EpochFacts<'_, H>",
            ),
            (
                INFERENCE_CONTEXT_SOURCE,
                "pub(crate) struct HostInferenceFacts<'a, H: BodyAnalysisReadHost>",
                "impl<H: BodyAnalysisReadHost> LazyInferenceFacts for HostInferenceFacts<'_, H>",
            ),
        ];
        for (source, struct_header, impl_header) in adapters {
            for adapter_item in [item(source, struct_header), item(source, impl_header)] {
                for forbidden in [
                    "Sema<'",
                    "Sema::",
                    "&Sema",
                    "BodySema",
                    "BoundSema",
                    "Deref",
                ] {
                    assert!(
                        !adapter_item.contains(forbidden),
                        "generic adapter must not retain {forbidden}"
                    );
                }
            }
        }
        assert!(FACT_MODE_SOURCE.contains("trait BodyAnalysisReadHost"));
        assert!(FACT_MODE_SOURCE.contains("= EpochEndpointFacts<'a, Self>"));
        assert!(FACT_MODE_SOURCE.contains("= EpochCallFacts<'a, Self>"));
        assert!(FACT_MODE_SOURCE.contains("= EpochAggregateFacts<'a, Self>"));
        assert!(FACT_MODE_SOURCE.contains("= HostInferenceFacts<'a, Self>"));
    }

    #[test]
    fn frozen_type_syntax_uses_only_the_declaration_phase_extension() {
        fn item<'source>(source: &'source str, header: &str) -> &'source str {
            let start = source.find(header).expect("item header is present");
            let rest = &source[start..];
            let open = rest.find('{').expect("item body opens");
            let mut depth = 0;
            for (offset, ch) in rest[open..].char_indices() {
                match ch {
                    '{' => depth += 1,
                    '}' => {
                        depth -= 1;
                        if depth == 0 {
                            return &rest[..open + offset + 1];
                        }
                    }
                    _ => {}
                }
            }
            panic!("item body closes");
        }

        for (extension, count) in [
            ("D::resolve_indexed_const", 3),
            ("D::collect_free_function_signature", 2),
        ] {
            assert_eq!(
                TYPECK_SOURCE.matches(extension).count(),
                count,
                "type syntax must use {extension} at its exact recovery sites"
            );
        }
        for forbidden in [
            "binding_impl",
            "declaration_index",
            "during_binding",
            "declaration_binding_active",
        ] {
            assert!(
                !TYPECK_SOURCE.contains(forbidden),
                "type syntax must not select binding recovery with {forbidden}"
            );
        }

        let source_phase = item(
            SEMA_ROOT_SOURCE,
            "impl DeclarationPhase for SourceDeclarations",
        );
        for method_name in ["resolve_indexed_const", "collect_free_function_signature"] {
            let method = item(source_phase, &format!("fn {method_name}("));
            let normalized = method
                .chars()
                .filter(|ch| !ch.is_whitespace())
                .collect::<String>();
            assert!(
                normalized.contains("_sema:&mutSema<'_,Self>"),
                "frozen {method_name} must receive but not use the analyzer"
            );
            let open = method.find('{').expect("method body opens");
            let body = &method[open + 1..method.len() - 1];
            assert!(
                body.chars()
                    .filter(|ch| !ch.is_whitespace())
                    .eq("Ok(None)".chars()),
                "frozen {method_name} must make misses authoritative"
            );
        }
    }

    #[test]
    fn type_syntax_evaluator_is_generic_and_has_one_explicit_host() {
        fn item<'source>(source: &'source str, header: &str) -> &'source str {
            let start = source.find(header).expect("item header is present");
            let rest = &source[start..];
            let open = rest.find('{').expect("item body opens");
            let mut depth = 0;
            for (offset, ch) in rest[open..].char_indices() {
                match ch {
                    '{' => depth += 1,
                    '}' => {
                        depth -= 1;
                        if depth == 0 {
                            return &rest[..open + offset + 1];
                        }
                    }
                    _ => {}
                }
            }
            panic!("item body closes");
        }

        let host = item(TYPECK_SOURCE, "trait TypeSyntaxHost");
        for forbidden in [
            "Sema<",
            "Sema::",
            "BodySema",
            "Deref",
            "as_sema",
            "fn sema",
            "type_syntax_deferred_array_length",
            "type_syntax_deferred_value_argument",
        ] {
            assert!(
                !host.contains(forbidden),
                "the type-syntax host exposes no full analyzer accessor: {forbidden}"
            );
        }
        assert_eq!(
            TYPECK_SOURCE.matches("TypeSyntaxHost for Sema<").count(),
            1,
            "only the production epoch implements the type-syntax host"
        );
        assert!(TYPECK_SOURCE.contains("pub(super) trait TypeSyntaxHost"));
        assert!(TYPECK_SOURCE.contains("pub(super) struct TypeSyntaxProvider"));
        assert!(TYPECK_SOURCE.contains("pub(super) struct DeferredTypeSyntaxProvider"));
        assert!(TYPECK_SOURCE.contains("pub(super) enum SemaTypeResolutionContext"));
        assert!(TYPECK_SOURCE.contains("pub(super) fn resolve_array_length_fact("));
        assert_eq!(
            TYPECK_SOURCE
                .matches("pub(super) fn flush_observed_type_dependencies(")
                .count(),
            2,
            "both generic evaluators expose exact dependency flushing to sibling hosts"
        );
        assert!(!TYPECK_SOURCE.contains("type_syntax_deferred_array_length"));
        assert!(!TYPECK_SOURCE.contains("type_syntax_deferred_value_argument"));

        for evaluator in [
            item(TYPECK_SOURCE, "struct TypeSyntaxProvider<"),
            item(TYPECK_SOURCE, "struct DeferredTypeSyntaxProvider<"),
            item(
                TYPECK_SOURCE,
                "impl<H: TypeSyntaxHost> crate::SemanticModulePathProvider<FileId, crate::types::ModuleId, FileId>\n    for TypeSyntaxProvider",
            ),
            item(
                TYPECK_SOURCE,
                "impl<H: TypeSyntaxHost>\n    crate::SemanticTypeSyntaxProvider<\n        FileId,\n        crate::types::ModuleId,\n        FileId,\n        Spur,\n        Spur,\n        Type,\n        ConstValue,\n    > for TypeSyntaxProvider",
            ),
            item(
                TYPECK_SOURCE,
                "impl<'s, 'c, H: TypeSyntaxHost> TypeSyntaxProvider<'s, 'c, H>",
            ),
            item(
                TYPECK_SOURCE,
                "impl<H: TypeSyntaxHost> crate::SemanticModulePathProvider<FileId, crate::types::ModuleId, FileId>\n    for DeferredTypeSyntaxProvider",
            ),
            item(
                TYPECK_SOURCE,
                "impl<H: TypeSyntaxHost>\n    crate::SemanticTypeSyntaxProvider<\n        FileId,\n        crate::types::ModuleId,\n        FileId,\n        Spur,\n        Spur,\n        DeferredTypeResolution,\n        DeferredValueResolution,\n    > for DeferredTypeSyntaxProvider",
            ),
            item(
                TYPECK_SOURCE,
                "impl<'s, 'c, H: TypeSyntaxHost> DeferredTypeSyntaxProvider<'s, 'c, H>",
            ),
        ] {
            for forbidden in [
                "Sema<",
                "Sema::",
                "BodySema",
                "DeclarationPhase",
                "Deref",
                "SemaTypeSyntaxProvider",
                "DeferredSemaTypeSyntaxProvider",
                "type_syntax_deferred_array_length",
                "type_syntax_deferred_value_argument",
            ] {
                assert!(
                    !evaluator.contains(forbidden),
                    "generic type-syntax evaluator must not retain {forbidden}"
                );
            }
        }

        let deferred = item(
            TYPECK_SOURCE,
            "impl<'s, 'c, H: TypeSyntaxHost> DeferredTypeSyntaxProvider<'s, 'c, H>",
        );
        for required in [
            "fn deferred_argument_expected(",
            "fn validate_value_position(",
            "resolve_semantic_comptime_call(",
            "type_syntax_signature_substitutions_are_ready(",
            "type_syntax_resolve_substituted_parameter_type(",
            "type_syntax_resolve_substituted_return_type(",
            "type_syntax_validate_deferred_value(",
        ] {
            assert!(
                deferred.contains(required),
                "generic deferred evaluator owns {required}"
            );
        }

        for (method, provider) in [
            (
                "resolve_type_syntax_with_epoch_facts",
                "TypeSyntaxProvider::new",
            ),
            (
                "resolve_type_module_prefix_with_epoch_facts",
                "TypeSyntaxProvider::new",
            ),
            (
                "resolve_array_length_with_epoch_facts",
                "TypeSyntaxProvider::new",
            ),
            (
                "validate_deferred_type_position_with_epoch_facts",
                "DeferredTypeSyntaxProvider::new",
            ),
        ] {
            let method = item(TYPECK_SOURCE, &format!("fn {method}("));
            assert!(
                method.contains(provider),
                "{method} constructs the generic provider"
            );
            assert!(!method.contains("SemaTypeSyntaxProvider"));
        }
        let bare_value = deferred;
        assert!(bare_value.contains("TypeSyntaxProvider::new"));
        assert!(bare_value.contains("DeferredTypeSyntaxProvider::new"));
        assert!(!TYPECK_SOURCE.contains("SemaTypeSyntaxProvider"));
        assert!(!TYPECK_SOURCE.contains("DeferredSemaTypeSyntaxProvider"));
    }

    #[test]
    fn place_and_ownership_analysis_has_one_cohesive_owner() {
        for method in [
            "snapshot_move_state",
            "restore_move_state",
            "restore_move_state_and_cancel",
            "analyze_with_borrow_root",
            "is_addressable_read",
            "require_addressable_read",
            "allocate_local_storage",
            "try_read_traced_place",
            "materialize_borrow_argument",
            "project_strbuf_text_fields",
            "peel_projected_rvalue_scope",
            "emit_projected_rvalue_read",
            "try_trace_place",
            "build_place_ref",
            "build_move_marker_place_ref",
            "analyze_variable_ops",
            "analyze_alloc",
            "analyze_var_ref",
            "analyze_assign",
            "reject_move_out_of_byref_param",
            "analyze_field_get",
            "analyze_field_set",
            "analyze_index_get",
            "analyze_index_set",
        ] {
            let definition = format!("fn {method}(");
            assert!(
                OWNERSHIP_SOURCE.contains(&definition),
                "analysis/ownership.rs must own {method}"
            );
            assert!(
                !ANALYZE_OPS_SOURCE.contains(&definition),
                "analyze_ops.rs must not retain {method}"
            );
            assert!(
                !INSTRUCTIONS_SOURCE.contains(&definition),
                "analysis/instructions.rs must not retain {method}"
            );
        }

        assert!(OWNERSHIP_SOURCE.contains("struct PlaceTrace"));
        assert!(OWNERSHIP_SOURCE.contains("struct ProjectionInfo"));
        assert!(OWNERSHIP_SOURCE.contains("fn moved_state<'ctx>("));
        assert!(OWNERSHIP_SOURCE.contains("impl<'a> BodySema<'a>"));
        assert!(!OWNERSHIP_SOURCE.contains("struct BodySema"));
        assert!(!OWNERSHIP_SOURCE.contains("analyze_field_set_impl"));
        assert!(!OWNERSHIP_SOURCE.contains("analyze_index_set_impl"));
        assert_eq!(
            OWNERSHIP_SOURCE
                .matches("consider making parameter")
                .count(),
            1,
            "immutable parameter diagnostics must share the receiver-aware ownership helper"
        );

        // Phase-specific consumers must use ownership.rs APIs instead of
        // growing peer authorities for place construction, move-state
        // mutation, scoped borrow state, or move-marker cancellation.
        for (name, source) in OWNERSHIP_PEER_SOURCES {
            for forbidden in [
                "ctx.moved_vars",
                "AirInstData::PlaceRead",
                ".make_place(",
                ".cancel_move_marker(",
                "std::mem::replace(&mut ctx.byref_arg_root",
                "ctx.byref_arg_root.replace(",
            ] {
                assert!(
                    !source.contains(forbidden),
                    "analysis/{name} must route `{forbidden}` through ownership.rs"
                );
            }
        }

        // Control-flow joins deliberately snapshot and merge the complete move
        // lattice. They are the sole non-ownership exception because their
        // authority is path convergence, not individual place semantics.
        assert!(CONTROL_FLOW_SOURCE.contains("ctx.moved_vars"));

        for (name, source) in OWNERSHIP_PEER_SOURCES
            .iter()
            .copied()
            .chain(std::iter::once(("control_flow.rs", CONTROL_FLOW_SOURCE)))
        {
            for forbidden in ["data: AirInstData::StorageLive", "data: AirInstData::Alloc"] {
                assert!(
                    !source.contains(forbidden),
                    "sema/{name} must route `{forbidden}` through ownership.rs"
                );
            }
        }

        // Semantic export only decodes existing AIR, while tests inspect AIR
        // shapes; neither site emits ownership or storage-lifetime operations.
        assert!(include_str!("semantic_body_export.rs").contains("AirInstData::Alloc"));
        assert!(include_str!("tests.rs").contains("AirInstData::StorageLive"));
    }

    #[test]
    fn aggregate_analysis_has_one_cohesive_owner() {
        for method in [
            "analyze_struct_ops",
            "analyze_struct_init",
            "analyze_module_type_member_access",
            "analyze_array_ops",
            "reject_non_runtime_array_element",
            "is_runtime_value_binding",
            "try_module_id_of",
            "try_analyze_module_qualified_type_call",
            "try_analyze_module_dotted_enum_variant",
            "try_analyze_dotted_enum_variant",
            "resolve_enum_type_name",
            "resolve_struct_type_name",
            "analyze_enum_variant_construction",
            "analyze_array_init",
            "analyze_array_repeat",
            "analyze_enum_ops",
            "validate_equality_operand_type",
            "try_prepare_aggregate_equality",
        ] {
            let definition = format!("fn {method}(");
            assert!(
                AGGREGATES_SOURCE.contains(&definition),
                "aggregates.rs must own {method}"
            );
            assert!(
                !ANALYZE_OPS_SOURCE.contains(&definition),
                "analyze_ops.rs must not retain {method}"
            );
            assert!(
                !BUILTIN_OPS_SOURCE.contains(&definition),
                "analysis/builtin_ops.rs must not retain {method}"
            );
        }

        for dispatch in [
            "self.analyze_field_get(",
            "self.analyze_field_set(",
            "self.analyze_index_get(",
            "self.analyze_index_set(",
        ] {
            assert!(
                AGGREGATES_SOURCE.contains(dispatch),
                "aggregate dispatch must route {dispatch} through ownership.rs"
            );
        }
        for forbidden in [
            "ctx.moved_vars",
            "AirInstData::PlaceRead",
            "AirInstData::PlaceWrite",
            "struct PlaceTrace",
            "struct ProjectionInfo",
            ".make_place(",
        ] {
            assert!(
                !AGGREGATES_SOURCE.contains(forbidden),
                "aggregates.rs must not duplicate ownership authority: {forbidden}"
            );
        }
    }

    #[test]
    fn call_and_intrinsic_analysis_have_cohesive_owners() {
        for method in [
            "analyze_call_ops",
            "analyze_call",
            "analyze_resolved_function_call",
            "analyze_method_call",
            "analyze_method_call_impl",
            "analyze_module_member_call_impl",
            "analyze_assoc_fn_call",
            "analyze_assoc_fn_call_impl",
            "validate_call_contract",
            "analyze_call_operands",
            "emit_call_result",
        ] {
            let definition = format!("fn {method}(");
            assert!(
                CALLS_SOURCE.contains(&definition),
                "analysis/calls.rs must own {method}"
            );
            assert!(
                !CALL_INTRINSIC_PEER_SOURCE.contains(&definition),
                "call-analysis peers must not define {method}"
            );
        }

        // The four runtime call paths (free, method, module-qualified, and
        // associated) must all pass through the same contract, operand, and
        // result seams. Exact call-site counts make a dormant helper plus a
        // reintroduced hand-rolled path fail this inventory.
        for seam in [
            "self.validate_call_contract(",
            "self.analyze_call_operands(",
            "self.emit_call_result(",
        ] {
            assert_eq!(
                CALLS_SOURCE.matches(seam).count(),
                4,
                "all four call forms must route through {seam}"
            );
        }
        assert_eq!(
            CALLS_SOURCE.matches("air.add_call(").count(),
            1,
            "only emit_call_result may construct an ordinary AIR call"
        );
        assert_eq!(
            CALLS_SOURCE
                .matches("self.analyze_call_args_coerced(")
                .count(),
            1,
            "only analyze_call_operands may enter ownership argument coercion"
        );

        for method in [
            "analyze_intrinsic_ops",
            "analyze_intrinsic",
            "analyze_intrinsic_impl",
            "analyze_internal_intrinsic_impl",
            "analyze_type_intrinsic",
            "analyze_offset_of",
        ] {
            let definition = format!("fn {method}(");
            assert!(
                INTRINSICS_SOURCE.contains(&definition),
                "analysis/intrinsics.rs must own {method}"
            );
            assert!(
                !CALL_INTRINSIC_PEER_SOURCE.contains(&definition),
                "intrinsic-analysis peers must not define {method}"
            );
        }

        // Calls coordinate source modes and results, but place construction,
        // loans, moves, and coercing by-reference operands remain canonical in
        // ownership.rs (RUE-857).
        for ownership_method in ["analyze_call_args_coerced", "check_exclusive_access"] {
            let definition = format!("fn {ownership_method}");
            assert!(
                OWNERSHIP_SOURCE.contains(&definition),
                "analysis/ownership.rs must own {ownership_method}"
            );
            assert!(
                !CALLS_SOURCE.contains(&definition),
                "analysis/calls.rs must not duplicate {ownership_method}"
            );
        }

        assert!(SEMA_ROOT_SOURCE.contains("fn validate_explicit_call_modes"));
        assert!(!CALLS_SOURCE.contains("fn validate_explicit_call_modes"));

        assert!(CALLS_SOURCE.contains("self.analyze_call_args_coerced("));
        assert!(CALLS_SOURCE.contains("fn check_module_member_access("));
        assert!(!ANALYSIS_ROOT_SOURCE.contains("fn check_module_member_call("));
        assert!(!ANALYSIS_ROOT_SOURCE.contains("fn emit_module_member_call("));
        assert!(INTRINSICS_SOURCE.contains("let known = &self.known;"));
        assert!(!ANALYZE_OPS_SOURCE.contains("KnownSymbols"));
    }
}
