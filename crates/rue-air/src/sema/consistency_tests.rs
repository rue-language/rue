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
    const INTRINSICS_SOURCE: &str = include_str!("analysis/intrinsics.rs");
    const SEMA_ROOT_SOURCE: &str = include_str!("mod.rs");
    const ANALYSIS_ROOT_SOURCE: &str = include_str!("analysis.rs");
    const INSTRUCTIONS_SOURCE: &str = include_str!("analysis/instructions.rs");
    const OWNERSHIP_SOURCE: &str = include_str!("analysis/ownership.rs");
    const CONTROL_FLOW_SOURCE: &str = include_str!("control_flow.rs");
    const FACT_MODE_SOURCE: &str = include_str!("fact_mode.rs");
    const BODY_ENDPOINT_SOURCE: &str = include_str!("body_endpoint.rs");
    const CALL_RESOLUTION_SOURCE: &str = include_str!("call_resolution.rs");
    const AGGREGATE_RESOLUTION_SOURCE: &str = include_str!("aggregate_resolution.rs");
    const INFERENCE_CONTEXT_SOURCE: &str = include_str!("inference_ctx.rs");
    const TYPECK_SOURCE: &str = include_str!("typeck.rs");
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
        assert!(FACT_MODE_SOURCE.contains("struct DeferredValueRequest"));
        assert!(FACT_MODE_SOURCE.contains("fn resolve_type_syntax("));
        assert!(FACT_MODE_SOURCE.contains("fn resolve_type_module_prefix"));
        assert!(FACT_MODE_SOURCE.contains("fn resolve_array_length("));
        assert!(FACT_MODE_SOURCE.contains("fn validate_deferred_type("));
        assert!(FACT_MODE_SOURCE.contains("fn validate_deferred_value("));
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
            ("D::resolve_indexed_const", 4),
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
