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
    use ahash::AHashSet;

    /// All InstData variants that exist in the RIR.
    ///
    /// This list must be kept in sync with rue-rir's InstData enum.
    /// When adding a new variant to InstData, add it here - the test will
    /// then fail if either pass doesn't handle it.
    const ALL_INSTDATA_VARIANTS: &[&str] = &[
        // Constants
        "IntConst",
        // `FloatConst` is handled by both passes as a *rejection* until
        // ADR-0065 Phase 4 types it: constraint generation reports `!` and AIR
        // emission reports the preview gate or E1109. Listing it here keeps
        // that pairing enforced — if Phase 4 gives it a real inference rule and
        // forgets the emission side (or vice versa), this test fails.
        "FloatConst",
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
        include_str!("analysis/type_inference.rs"),
        include_str!("analysis/instructions.rs"),
        include_str!("analysis/calls.rs"),
        include_str!("analysis/intrinsics.rs"),
        include_str!("analysis/builtin_ops.rs"),
        include_str!("analysis/ownership.rs"),
        include_str!("analysis/pointers.rs"),
        include_str!("aggregates.rs"),
        include_str!("control_flow.rs"),
    );

    const ANALYZE_OPS_SOURCE: &str = include_str!("analyze_ops.rs");
    const AGGREGATES_SOURCE: &str = include_str!("aggregates.rs");
    const BUILTIN_OPS_SOURCE: &str = include_str!("analysis/builtin_ops.rs");
    const CALLS_SOURCE: &str = include_str!("analysis/calls.rs");
    const INTRINSICS_SOURCE: &str = include_str!("analysis/intrinsics.rs");
    const ORDINARY_ENGINE_SOURCE: &str = include_str!("ordinary_engine.rs");
    const COMPTIME_EVAL_SOURCE: &str = include_str!("comptime_eval.rs");
    const COMPTIME_ENGINE_SOURCE: &str = include_str!("comptime/execution.rs");
    const COMPTIME_STRUCTURED_TYPE_SOURCE: &str = include_str!("comptime/structured_type.rs");
    const SEMA_ROOT_SOURCE: &str = include_str!("mod.rs");
    const ANALYSIS_ROOT_SOURCE: &str = include_str!("analysis.rs");
    const INSTRUCTIONS_SOURCE: &str = include_str!("analysis/instructions.rs");
    const OWNERSHIP_SOURCE: &str = include_str!("analysis/ownership.rs");
    const TYPE_INFERENCE_SOURCE: &str = include_str!("analysis/type_inference.rs");
    const CONTROL_FLOW_SOURCE: &str = include_str!("control_flow.rs");
    const TYPECK_SOURCE: &str = include_str!("typeck.rs");
    const FACT_MODE_SOURCE: &str = include_str!("fact_mode.rs");
    const AGGREGATE_RESOLUTION_SOURCE: &str = include_str!("aggregate_resolution.rs");
    const CALL_RESOLUTION_SOURCE: &str = include_str!("call_resolution.rs");
    const BODY_ENDPOINT_SOURCE: &str = include_str!("body_endpoint.rs");
    const PROVIDER_BODY_HOST_SOURCE: &str = include_str!("provider_body_host.rs");
    const SEMANTIC_TYPE_RESOLUTION_SOURCE: &str = include_str!("../semantic_type_resolution.rs");
    const TYPES_SOURCE: &str = include_str!("../types.rs");
    const AIR_LIB_SOURCE: &str = include_str!("../lib.rs");
    const VISIBILITY_SOURCE: &str = include_str!("visibility.rs");
    const BINDING_MANIFEST_SOURCE: &str = include_str!("binding_manifest.rs");
    const CALL_INTRINSIC_PEER_SOURCE: &str = concat!(
        include_str!("mod.rs"),
        include_str!("aggregates.rs"),
        include_str!("analysis.rs"),
        include_str!("analysis/builtin_ops.rs"),
        include_str!("analysis/instructions.rs"),
        include_str!("analysis/ownership.rs"),
        include_str!("analysis/pointers.rs"),
        include_str!("analysis/type_inference.rs"),
        include_str!("analyze_ops.rs"),
        include_str!("anon_structs.rs"),
        include_str!("binding_manifest.rs"),
        include_str!("comptime_eval.rs"),
        include_str!("context.rs"),
        include_str!("control_flow.rs"),
        include_str!("declaration_index.rs"),
        include_str!("declarations.rs"),
        include_str!("inference_ctx.rs"),
        include_str!("info.rs"),
        include_str!("known_symbols.rs"),
        include_str!("output.rs"),
        include_str!("semantic_body_export.rs"),
        include_str!("typeck.rs"),
        include_str!("visibility.rs"),
    );
    const OWNERSHIP_PEER_SOURCES: &[(&str, &str)] = &[
        ("analysis.rs", ANALYSIS_ROOT_SOURCE),
        ("aggregates.rs", AGGREGATES_SOURCE),
        ("analyze_ops.rs", ANALYZE_OPS_SOURCE),
        ("builtin_ops.rs", include_str!("analysis/builtin_ops.rs")),
        ("calls.rs", include_str!("analysis/calls.rs")),
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
    fn extract_instdata_variants(source: &str) -> AHashSet<String> {
        let mut variants = AHashSet::new();

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
    fn ordinary_engine_is_the_single_canonical_body_algorithm() {
        let engine = include_str!("ordinary_engine.rs");
        assert!(engine.contains("pub(crate) trait OrdinaryBodyAnalysisHost"));
        assert!(engine.contains("pub(crate) struct OrdinaryBodyEngine"));
        assert!(engine.contains("pub(crate) fn analyze_single_function_resolved"));
        assert!(engine.contains("pub(crate) fn analyze_named_method_resolved"));
        assert!(engine.contains("pub(crate) fn analyze_function_internal"));
        assert!(!engine.contains("#[allow(dead_code)]"));
        assert!(engine.contains("semantic_type_syntax_compile_error("));
        assert!(!engine.contains("InternalError(format!(\"{failure:?}\"))"));
        assert!(engine.contains("self.body_type_pool().type_carries_linear(ty)"));
        assert!(engine.contains("self.storage.known_linear_during_binding(ty)"));
        assert!(engine.contains("self.storage.known_drop_glue_during_binding(ty)"));
        assert!(engine.contains("self.storage.resolve_canonical_import(path, span)"));
        assert!(!engine.contains("resolve(import_path, Span::default())"));
        assert!(!engine.contains(".resolve(import_path, Span::default()).ok()"));
        for forbidden in [
            "ProviderBodyAnalysisState",
            "ProviderInferenceFacts",
            "impl std::ops::Deref for OrdinaryBodyEngine",
            "analyze_function_internal_legacy",
        ] {
            assert!(
                !engine.contains(forbidden),
                "C0 canonical engine retains provider/legacy escape hatch: {forbidden}"
            );
        }
        let provider = include_str!("provider_body_host.rs");
        assert!(provider.contains(".analyze_single_function_resolved("));
        assert!(provider.contains(".analyze_named_method_resolved("));
        assert!(
            !provider.contains(".analyze_single_function("),
            "provider-owned functions must not re-resolve their exact durable signatures"
        );
        assert!(
            !provider.contains(".analyze_named_method("),
            "provider-owned methods must not regain a syntax-resolving facade"
        );
        assert!(
            !engine.contains("pub(crate) fn analyze_named_method("),
            "the obsolete named-method facade must not return"
        );
    }

    #[test]
    fn comptime_body_adapter_cannot_reintroduce_rir_dispatch() {
        let source = COMPTIME_EVAL_SOURCE;
        let engine = COMPTIME_ENGINE_SOURCE;
        assert!(!engine.contains("OrdinaryBodyEngine"));
        assert!(!engine.contains("eval_const_expr"));
        assert_eq!(
            engine.matches("    fn eval(\n").count(),
            1,
            "the canonical engine must have exactly one dispatcher"
        );
        assert_eq!(
            engine.matches("match &inst.data").count(),
            1,
            "all supported RIR recursion must live in one dispatcher"
        );
        assert_eq!(
            engine.matches("match data").count(),
            1,
            "the recursion trampoline must have one routing match"
        );
        assert_eq!(
            engine.matches("fn eval_dispatch(").count(),
            1,
            "the instruction dispatcher must have one implementation"
        );
        assert_eq!(
            engine
                .matches("host_value!(self.host.check_canceled());")
                .count(),
            1,
            "each evaluation node must have exactly one cancellation checkpoint"
        );
        let eval = engine
            .split("    fn eval(\n")
            .nth(1)
            .and_then(|source| {
                source
                    .split("\n    #[inline(never)]\n    fn eval_block")
                    .next()
            })
            .expect("canonical eval source");
        let eval_body = eval
            .split_once("    ) -> ComptimeOutcome<H::Value, H::Failure> {")
            .map(|(_, body)| body.trim_start())
            .expect("canonical eval body");
        assert!(
            eval_body.starts_with("host_value!(self.host.check_canceled());"),
            "cancellation must precede the first RIR read"
        );
        let dispatch = engine
            .split("    fn eval_dispatch(\n")
            .nth(1)
            .and_then(|source| source.split("\n    fn evaluate_method_call").next())
            .expect("canonical dispatch source");
        assert!(
            !dispatch.contains("check_canceled"),
            "cancellation must not be duplicated in the lower dispatcher"
        );
        assert!(
            engine.contains("InstData::Comptime { .. }\n            | InstData::Block { .. }")
                && engine.contains("InstData::Call { .. } => unreachable!"),
            "routed recursive arms must not regain lower dispatcher bodies"
        );
        assert_eq!(
            source.matches("fn eval_const_expr").count(),
            1,
            "the body adapter must have one canonical entry point"
        );
        assert!(
            !source.contains("fn eval_comptime_type_call"),
            "the deleted recursive type-call evaluator must not return"
        );
        assert!(
            source.contains("fn admit_comptime_call")
                && source.contains("type CallBinding")
                && source.contains("type BoundCall")
                && source.contains("fn begin_comptime_call_binding")
                && source.contains("fn bind_comptime_call_argument")
                && source.contains("fn finish_comptime_call_binding")
                && source.contains("fn prepare_comptime_call")
                && source.contains("fn finish_comptime_call"),
            "call admission, binding, preparation, and completion must be named hooks"
        );
        fn function_body<'a>(source: &'a str, signature: &str) -> &'a str {
            let start = source.find(signature).expect("semantic hook exists");
            let body = &source[start..];
            let open = body.find('{').expect("semantic hook body opens");
            let mut depth = 0;
            for (offset, ch) in body[open..].char_indices() {
                match ch {
                    '{' => depth += 1,
                    '}' => {
                        depth -= 1;
                        if depth == 0 {
                            return &body[..open + offset + 1];
                        }
                    }
                    _ => {}
                }
            }
            panic!("semantic hook body closes");
        }
        for signature in [
            "fn admit_comptime_call(",
            "fn begin_comptime_call_binding(",
            "fn bind_comptime_call_argument(",
            "fn finish_comptime_call_binding(",
            "fn prepare_comptime_call(",
            "fn resolve_comptime_type_path(",
            "fn resolve_module_comptime_callable(",
            "fn finish_comptime_call(",
        ] {
            let body = function_body(source, signature);
            assert!(
                !body.contains("InstRef"),
                "{signature} may not receive child RIR refs"
            );
            assert!(
                !body.contains("RirCallArg")
                    && !body.contains("eval_const_expr")
                    && !body.contains("ComptimeEngine::new"),
                "{signature} must not recurse or invoke an evaluator"
            );
        }
        let adapter = function_body(source, "pub(crate) fn eval_const_expr(");
        assert!(
            !adapter.contains("match &inst.data") && !adapter.contains("InstData::"),
            "the body adapter must delegate; RIR dispatch belongs to ComptimeEngine"
        );
        assert!(
            !ORDINARY_ENGINE_SOURCE.contains("eval_const_expr"),
            "ordinary body analysis must not retain a comptime evaluator"
        );
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
        let engine_owner = typeck_impls
            .iter()
            .find(|item| item.contains("fn is_str_struct("))
            .expect("string classification owner is present");
        assert!(
            engine_owner.starts_with("impl<H: OrdinaryBodyAnalysisHost> OrdinaryBodyEngine<'_, H>"),
            "string classification must belong to the neutral ordinary-body engine"
        );

        for method in classification_methods {
            let needle = format!("fn {method}(");
            let owners = typeck_impls
                .iter()
                .filter(|item| item.contains(&needle))
                .collect::<Vec<_>>();
            assert_eq!(owners.len(), 1, "{method} has one implementation");
            assert_eq!(
                *owners[0], *engine_owner,
                "{method} shares the cohesive ordinary-engine owner"
            );
        }

        let mut actual_methods = method_names(engine_owner);
        actual_methods.sort_unstable();
        let mut expected_methods = classification_methods.to_vec();
        expected_methods.sort_unstable();
        assert_eq!(actual_methods, expected_methods);

        for (name, source, anchor) in [
            (
                "analyze_ops.rs",
                ANALYZE_OPS_SOURCE,
                "self.str_fixed_capacity(",
            ),
            ("aggregates.rs", AGGREGATES_SOURCE, "self.is_str_struct("),
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
        let engine_owner = typeck_impls
            .iter()
            .find(|item| item.contains("fn require_layout_slots("))
            .expect("materialized-layout owner is present");
        assert!(
            engine_owner.starts_with("impl<H: OrdinaryBodyAnalysisHost> OrdinaryBodyEngine<'_, H>"),
            "materialized layout policy must belong to the neutral ordinary-body engine"
        );

        for method in layout_methods {
            let needle = format!("fn {method}(");
            let owners = typeck_impls
                .iter()
                .filter(|item| item.contains(&needle))
                .collect::<Vec<_>>();
            assert_eq!(owners.len(), 1, "{method} has one implementation");
            assert_eq!(
                *owners[0], *engine_owner,
                "{method} shares the cohesive ordinary-engine owner"
            );
        }

        let mut actual_methods = method_names(engine_owner);
        actual_methods.sort_unstable();
        let mut expected_methods = layout_methods.to_vec();
        expected_methods.sort_unstable();
        assert_eq!(actual_methods, expected_methods);

        for method in [
            "pre_create_array_types_from_infer_type",
            "infer_type_to_concrete_type_for_key",
        ] {
            assert_eq!(
                ORDINARY_ENGINE_SOURCE
                    .matches(&format!("fn {method}("))
                    .count(),
                1,
                "{method} has one ordinary-engine implementation"
            );
            assert!(
                !TYPECK_SOURCE.contains(&format!("fn {method}(")),
                "{method} must not retain a peer declaration-phase implementation"
            );
        }

        let require = method_item(engine_owner, "require_layout_slots");
        assert!(require.contains("match self.checked_abi_slot_count(ty)"));

        let reserve = method_item(engine_owner, "reserve_frame_slots");
        assert!(reserve.contains("crate::layout::checked_function_frame_slots(start, additional)"));

        let checked = method_item(engine_owner, "checked_abi_slot_count");
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

        let abi = method_item(engine_owner, "abi_slot_count");
        assert!(
            abi.contains("self.body_type_pool().provisional_abi_slot_count(ty)"),
            "the engine delegates to the host's canonical TypeInternPool computation"
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
                "ordinary_engine.rs",
                ORDINARY_ENGINE_SOURCE,
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
        assert_eq!(impls.len(), 1, "visibility has exactly one receiver block");
        let engine = impls
            .iter()
            .find(|item| {
                item.starts_with("impl<H: OrdinaryBodyAnalysisHost> OrdinaryBodyEngine<'_, H>")
            })
            .expect("ordinary-engine visibility receiver exists");

        let mut engine_methods = method_names(engine);
        engine_methods.sort_unstable();
        assert_eq!(
            engine_methods,
            ["module_file_for_ref", "resolve_enum_through_module"]
        );

        for method in ["resolve_enum_through_module", "module_file_for_ref"] {
            let needle = format!("fn {method}(");
            assert_eq!(
                VISIBILITY_SOURCE.matches(&needle).count(),
                1,
                "{method} has one implementation"
            );
            assert!(
                engine.contains(&needle),
                "{method} belongs to the ordinary engine"
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
        let all_variants: AHashSet<String> = ALL_INSTDATA_VARIANTS
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

        assert!(
            CONTROL_FLOW_SOURCE
                .contains("impl<H: OrdinaryBodyAnalysisHost> OrdinaryBodyEngine<'_, H>")
        );
        assert!(!CONTROL_FLOW_SOURCE.contains("struct BodySema"));
    }

    #[test]
    fn structured_type_syntax_has_one_policy_and_one_fact_host() {
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
        assert!(
            !TYPECK_SOURCE.contains("TypeSyntaxHost for"),
            "typeck hosts the fact vocabulary, not a fact-host implementation"
        );
        assert_eq!(
            PROVIDER_BODY_HOST_SOURCE
                .matches("impl<P, S, K, M> TypeSyntaxHost for ProviderBodyHost<")
                .count(),
            1,
            "the query-backed body host implements the shared fact host exactly once"
        );
        assert!(TYPECK_SOURCE.contains("pub(super) trait TypeSyntaxHost"));
        let provider_state = item(TYPECK_SOURCE, "pub(super) struct TypeSyntaxProviderState");
        assert!(!provider_state.contains("&mut H"));
        assert!(!provider_state.contains("host:"));
        assert!(!provider_state.contains("Option<&AHashMap"));
        assert!(TYPECK_SOURCE.contains("pub(super) struct TypeSyntaxProvider"));
        assert!(TYPECK_SOURCE.contains("pub(super) enum SemaTypeResolutionContext"));
        assert!(TYPECK_SOURCE.contains("pub(super) fn resolve_array_length_fact("));
        assert_eq!(
            TYPECK_SOURCE
                .matches("pub(super) fn flush_observed_type_dependencies(")
                .count(),
            1,
            "the generic evaluator exposes exact dependency flushing to sibling hosts"
        );
        assert!(!TYPECK_SOURCE.contains("type_syntax_deferred_array_length"));
        assert!(!TYPECK_SOURCE.contains("type_syntax_deferred_value_argument"));

        for evaluator in [
            item(TYPECK_SOURCE, "struct TypeSyntaxProvider<"),
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
                ".parse::<",
                ".trim(",
            ] {
                assert!(
                    !evaluator.contains(forbidden),
                    "generic type-syntax evaluator must not retain {forbidden}"
                );
            }
        }

        for (method, provider) in [
            (
                "fn resolve_structured_type_syntax(",
                "TypeSyntaxProvider::new",
            ),
            ("fn resolve_type_module_prefix(", "TypeSyntaxProvider::new"),
            ("fn resolve_array_length(", "TypeSyntaxProvider::new"),
        ] {
            let method = item(PROVIDER_BODY_HOST_SOURCE, method);
            assert!(
                method.contains(provider),
                "the provider host constructs the generic evaluator"
            );
            assert!(
                method.contains("TypeSyntaxProviderState::new"),
                "the provider host owns state before borrowing the semantic host"
            );
            assert!(!method.contains("SemaTypeSyntaxProvider"));
        }
        assert!(!TYPECK_SOURCE.contains("SemaTypeSyntaxProvider"));
        assert!(!TYPECK_SOURCE.contains("DeferredSemaTypeSyntaxProvider"));
        assert_eq!(
            SEMANTIC_TYPE_RESOLUTION_SOURCE
                .matches("pub fn resolve_structured_semantic_type_syntax_with<")
                .count(),
            1,
            "all storage hosts share one structured-type machine"
        );
        let structured_resolver = item(
            SEMANTIC_TYPE_RESOLUTION_SOURCE,
            "pub fn resolve_structured_semantic_type_syntax_with<",
        );
        assert!(
            !structured_resolver.contains("StructuredTypeWork::Evaluate"),
            "the synchronous resolver must remain a traversal-free wrapper"
        );
        assert_eq!(
            structured_resolver.matches("reduce_comptime_call(").count(),
            1,
            "the synchronous resolver must be the sole reduction adapter"
        );
        assert_eq!(
            structured_resolver
                .matches("resolve_structured_semantic_type_syntax_with(")
                .count(),
            0,
            "the canonical structured resolver must not self-dispatch recursively"
        );
        let structured_machine = item(
            SEMANTIC_TYPE_RESOLUTION_SOURCE,
            "fn poll_structured_type_machine<",
        );
        assert!(
            structured_machine.contains("StructuredTypeWork::Evaluate"),
            "the private poller must own the structured work-stack dispatch"
        );
        assert_eq!(
            structured_machine.matches("reduce_comptime_call(").count(),
            0,
            "the private poller must not reduce comptime calls"
        );
        for declaration in [
            "enum StructuredTypePoll<",
            "struct StructuredTypeSuspension<",
            "struct SemanticComptimeCallRequestView<",
        ] {
            let line = SEMANTIC_TYPE_RESOLUTION_SOURCE
                .lines()
                .find(|line| line.contains(declaration))
                .unwrap_or_else(|| panic!("missing typed suspension item: {declaration}"));
            assert!(
                line.trim_start().starts_with(declaration),
                "the typed suspension seam must remain private until it is keyed: {line}"
            );
        }
        for declaration in [
            "enum StructuredTypePoll<",
            "struct StructuredTypeSuspension<",
            "struct SemanticComptimeCallRequestView<",
            "pub struct ComptimeStructuredTypeJob<",
        ] {
            let offset = SEMANTIC_TYPE_RESOLUTION_SOURCE
                .find(declaration)
                .expect("typed suspension item");
            let attribute_start = SEMANTIC_TYPE_RESOLUTION_SOURCE[..offset]
                .rfind("\n\n")
                .map_or(0, |offset| offset + 2);
            assert!(
                !SEMANTIC_TYPE_RESOLUTION_SOURCE[attribute_start..offset].contains("Clone"),
                "typed suspension state must not derive Clone: {declaration}"
            );
        }
        assert!(
            !SEMANTIC_TYPE_RESOLUTION_SOURCE.contains("Clone for StructuredTypeSuspension"),
            "the paired suspension must be consumed exactly once"
        );
        let keyed_resume = item(SEMANTIC_TYPE_RESOLUTION_SOURCE, "pub fn resume<M, Q>(");
        let keyed_signature = keyed_resume
            .split_once("{")
            .map_or(keyed_resume, |(signature, _)| signature);
        for forbidden in ["root_scope", "arena", "resolve_symbol", "request"] {
            assert!(
                !keyed_signature.contains(forbidden),
                "keyed suspension resume must retain its owned authority: {forbidden}"
            );
        }
        assert_eq!(
            COMPTIME_ENGINE_SOURCE
                .matches("fn drive_structured_type(")
                .count(),
            1,
            "the engine must have one private structured continuation driver"
        );
        assert_eq!(
            COMPTIME_ENGINE_SOURCE
                .matches("pub fn drive_structured_type(")
                .count(),
            0,
            "structured continuation driving must remain engine-private"
        );
        assert_eq!(
            COMPTIME_STRUCTURED_TYPE_SOURCE
                .matches("impl<P, S, C, N, A, T, V, Sym, R> ComptimeStructuredTypeSuspension")
                .count(),
            1,
            "production exposes one canonical suspension implementation"
        );
        assert_eq!(
            SEMANTIC_TYPE_RESOLUTION_SOURCE
                .matches("struct ComptimeStructuredTypeJob")
                .count(),
            1,
            "the keyed resolver job has one declaration"
        );
        let semantic_type_production = SEMANTIC_TYPE_RESOLUTION_SOURCE
            .split("#[cfg(test)]")
            .next()
            .expect("structured resolver production precedes its tests");
        let job = item(
            semantic_type_production,
            "pub struct ComptimeStructuredTypeJob<",
        );
        let job_fields = job
            .split_once('{')
            .and_then(|(_, body)| body.strip_suffix('}'))
            .expect("structured resolver job fields");
        for required in [
            "type_substitutions: Vec<(N, T)>",
            "value_substitutions: Vec<(N, V)>",
        ] {
            assert!(
                job_fields.contains(required),
                "the consuming job must own its substitution scope: {required}"
            );
        }
        assert!(
            !job_fields.contains("pub "),
            "substitution scope must remain private to the consuming job"
        );
        assert_eq!(
            semantic_type_production
                .matches("fn with_comptime_substitutions<R>(")
                .count(),
            1,
            "the production type provider has one substitution-scoping seam"
        );
        let keyed_begin = item(semantic_type_production, "pub fn begin<M, Q>(");
        for required in [
            "type_substitutions: Vec<(N, T)>",
            "value_substitutions: Vec<(N, V)>",
            "with_comptime_substitutions(",
        ] {
            assert!(
                keyed_begin.contains(required),
                "structured begin must capture and install its scope once: {required}"
            );
        }
        let job_resume = item(semantic_type_production, "pub fn resume<M, Q>(");
        let keyed_resume_signature = job_resume
            .split_once('{')
            .map_or(job_resume, |(signature, _)| signature);
        for forbidden in ["type_substitutions", "value_substitutions"] {
            assert!(
                !keyed_resume_signature.contains(forbidden),
                "structured resume must not accept a replacement scope: {forbidden}"
            );
        }
        assert!(
            !SEMANTIC_TYPE_RESOLUTION_SOURCE.contains("Clone for ComptimeStructuredTypeJob"),
            "the keyed resolver job must remain consuming"
        );
        assert_eq!(
            COMPTIME_ENGINE_SOURCE
                .matches("self.evaluate_comptime_type_syntax(")
                .count(),
            6,
            "all type-bearing RIR arms and anonymous method descriptors must use the engine helper"
        );
        for forbidden in [
            "pub fn resolve_semantic_type_syntax",
            "pub fn resolve_semantic_comptime_call",
            "resolve_structured_semantic_comptime_call(",
            "resolve_semantic_comptime_call_core(",
            "SemanticValueSyntax::Rendered",
            "parse_array_type_syntax(",
            "parse_pointer_type_syntax(",
            "parse_type_call_syntax(",
        ] {
            assert!(
                !SEMANTIC_TYPE_RESOLUTION_SOURCE.contains(forbidden),
                "the canonical resolver has no rendered-string peer: {forbidden}"
            );
        }
        for forbidden in [
            "parse_array_type_syntax",
            "parse_pointer_type_syntax",
            "parse_type_call_syntax",
        ] {
            assert!(
                !TYPES_SOURCE.contains(forbidden),
                "rue-air must not retain a rendered type-syntax parser: {forbidden}"
            );
            assert!(
                !AIR_LIB_SOURCE.contains(forbidden),
                "rue-air must not export a rendered type-syntax parser: {forbidden}"
            );
            assert!(
                !GENERATE_SOURCE.contains(forbidden),
                "inference must consume structured type syntax: {forbidden}"
            );
        }
        let provider_type_facts = item(
            PROVIDER_BODY_HOST_SOURCE,
            "impl<P, S, K, M> TypeSyntaxHost for ProviderBodyHost<",
        );
        for forbidden in [
            "fn resolve_rir_type_syntax",
            "fn resolve_type_name_in_file",
            "parse_array_type_syntax(",
            "parse_pointer_type_syntax(",
            "parse_type_call_syntax(",
            "RirTypeSyntaxNode::",
        ] {
            assert!(
                !provider_type_facts.contains(forbidden),
                "the query-backed host supplies facts but owns no peer policy: {forbidden}"
            );
        }

        let inference_hint = item(GENERATE_SOURCE, "fn infer_type_hint(");
        for required in [
            "RirTypeSyntaxNode::Qualified { .. }",
            "RirTypeSyntaxNode::TypeCall { .. }",
            "RirTypeSyntaxNode::ValueCall { .. }",
            "=> None",
        ] {
            assert!(
                inference_hint.contains(required),
                "constraint generation must defer fact-bearing syntax to semantic resolution: {required}"
            );
        }
        for forbidden in ["render_type", ".parse::<", "parse_type_syntax"] {
            assert!(
                !inference_hint.contains(forbidden),
                "constraint hints must consume structured syntax without becoming a peer resolver: {forbidden}"
            );
        }

        let provider_type_host = item(
            PROVIDER_BODY_HOST_SOURCE,
            "impl<P, S, K, M> TypeResolutionHost for ProviderBodyHost<",
        );
        for required in [
            "resolve_structured_semantic_type_syntax_with(",
            "resolve_semantic_module_path(",
            "resolve_array_length_fact(",
            "Some(definition.file_id)",
        ] {
            assert!(
                provider_type_host.contains(required),
                "the query-backed host lost the shared type policy: {required}"
            );
        }
        for forbidden in [
            "fn resolve_array_length_in_file(",
            "for segment in request.segments",
            "Some(resolved.site)",
        ] {
            assert!(
                !PROVIDER_BODY_HOST_SOURCE.contains(forbidden),
                "the query-backed host regained handwritten type policy: {forbidden}"
            );
        }
    }

    #[test]
    fn body_fact_contracts_have_no_forwarding_object_layer() {
        let fact_sources = concat!(
            include_str!("fact_mode.rs"),
            include_str!("aggregate_resolution.rs"),
            include_str!("call_resolution.rs"),
            include_str!("body_endpoint.rs"),
            include_str!("ordinary_engine.rs"),
            include_str!("mod.rs"),
            include_str!("provider_body_host.rs"),
        );
        for forbidden in [
            "struct EpochFacts",
            "AggregateFactSource",
            "CallResolutionFactSource",
            "BodyEndpointFactSource",
            "BodyEndpointReadView",
            "trait BodyAnalysisHost",
        ] {
            assert!(
                !fact_sources.contains(forbidden),
                "body analysis must not regain a pass-through fact layer: {forbidden}"
            );
        }

        // The query-backed host and the pool-backed provider drivers are the
        // only implementations of the body-fact contracts; no epoch-owned
        // analyzer implements them.
        for (name, source, contract, provider_impl) in [
            (
                "aggregate_resolution.rs",
                AGGREGATE_RESOLUTION_SOURCE,
                "AggregateFacts for",
                "AggregateFacts for ProviderAggregateFacts<",
            ),
            (
                "body_endpoint.rs",
                BODY_ENDPOINT_SOURCE,
                "BodyEndpointProvider for",
                "BodyEndpointProvider for ProviderEndpointFacts<",
            ),
        ] {
            assert_eq!(
                source.matches(contract).count(),
                1,
                "{name} hosts exactly the pool-backed provider driver"
            );
            assert_eq!(source.matches(provider_impl).count(), 1);
        }
        assert_eq!(
            CALL_RESOLUTION_SOURCE
                .matches("CallResolutionFacts for")
                .count(),
            0,
            "call_resolution.rs defines the fact vocabulary without hosting an implementation"
        );
        for contract in [
            "BodyEndpointProvider for ProviderBodyHost<",
            "CallResolutionFacts for ProviderBodyHost<",
            "AggregateFactsTrait for ProviderBodyHost<",
        ] {
            assert_eq!(
                PROVIDER_BODY_HOST_SOURCE.matches(contract).count(),
                1,
                "the query-backed host implements {contract} directly exactly once"
            );
        }
        assert!(FACT_MODE_SOURCE.contains(
            "BodyEndpointProvider + CallResolutionFacts + AggregateFacts + InferenceFactSource"
        ));
    }

    #[test]
    fn synthetic_type_names_have_one_identity_policy() {
        let consumers = [
            ("inference/generate.rs", GENERATE_SOURCE),
            ("semantic_import.rs", include_str!("../semantic_import.rs")),
            ("analysis.rs", ANALYSIS_ROOT_SOURCE),
            ("binding_manifest.rs", BINDING_MANIFEST_SOURCE),
            ("body_identity.rs", include_str!("body_identity.rs")),
            ("ordinary_engine.rs", ORDINARY_ENGINE_SOURCE),
            (
                "semantic_body_export.rs",
                include_str!("semantic_body_export.rs"),
            ),
            ("typeck.rs", TYPECK_SOURCE),
        ];
        for (name, source) in consumers {
            for peer in [
                ".strip_prefix(\"Str(\")",
                ".starts_with(\"Str(\")",
                ".starts_with('[')",
            ] {
                assert!(
                    !source.contains(peer),
                    "{name} regained handwritten synthetic-type identity policy: {peer}"
                );
            }
        }
        for helper in [
            "pub fn fixed_string_capacity(",
            "pub fn is_slice_struct_name(",
            "pub fn is_string_view_struct_name(",
        ] {
            assert_eq!(
                TYPES_SOURCE.matches(helper).count(),
                1,
                "synthetic type identity must have one owner: {helper}"
            );
        }
    }

    /// Every source-name resolution carries an explicit lexical scope.
    ///
    /// RUE-497 retired graph-global name lookup: an unqualified mention is
    /// resolved against the referencing file's declarations and imports, so two
    /// sibling modules may share a type or constructor name. RUE-1126 deleted
    /// the last resolver family that could bypass that — a `GlobalSpeculative`
    /// root authority plus a spanless `resolve_type_for_comptime*` family that
    /// anchored every lookup at `FileId::DEFAULT`.
    ///
    /// The primary guard is structural: `TypeRootAuthority` is a `pub(super)`
    /// newtype with a private field and `in_file` as its only constructor, so
    /// "no scope" is not expressible. This test guards the parts the type
    /// system cannot: that no scope-free variant, spanless resolver entry
    /// point, or `FileId::DEFAULT` fallback reappears.
    #[test]
    fn source_name_resolution_always_carries_an_explicit_scope() {
        const RESOLVER_SOURCES: &[(&str, &str)] = &[
            ("typeck.rs", TYPECK_SOURCE),
            ("ordinary_engine.rs", ORDINARY_ENGINE_SOURCE),
            ("fact_mode.rs", include_str!("fact_mode.rs")),
            ("comptime_eval.rs", include_str!("comptime_eval.rs")),
            ("comptime.rs", crate::sema::COMPTIME_SOURCE),
            ("declarations.rs", include_str!("declarations.rs")),
            (
                "provider_body_host.rs",
                include_str!("provider_body_host.rs"),
            ),
            ("binding_manifest.rs", BINDING_MANIFEST_SOURCE),
            ("analysis/instructions.rs", INSTRUCTIONS_SOURCE),
            ("mod.rs", SEMA_ROOT_SOURCE),
        ];

        // The scope-free root authority and the spanless comptime resolver
        // family it fed. `resolve_type_for_comptime_with_subst_and_values_at_span`
        // is the surviving scoped form and must not match these needles.
        for (file, source) in RESOLVER_SOURCES {
            assert!(
                !source.contains("extract_anon_method_sigs"),
                "{file} retains a compiler-side anonymous method signature decoder"
            );
            for retired in [
                "GlobalSpeculative",
                "resolve_type_for_comptime(",
                "resolve_type_for_comptime_with_subst(",
                "resolve_type_for_comptime_with_subst_and_values(",
                "register_anon_struct_methods(",
                "register_anon_struct_methods_for_comptime(",
            ] {
                assert!(
                    !source.contains(retired),
                    "{file} reintroduces the scope-free resolver surface: {retired}"
                );
            }
            if let Some(start) = source.find("fn register_anon_struct_method_bodies(") {
                let registration = &source[start..];
                let registration = registration
                    .find("\n    fn ")
                    .map_or(registration, |end| &registration[..end]);
                assert!(
                    !registration.contains("params(&")
                        && !registration.contains("return_type,")
                        && !registration.contains("RirTypeSyntaxRef")
                        && !registration.contains("resolve_rir_type"),
                    "{file} reintroduces RIR signature decoding in method registration"
                );
            }
        }

        // `TypeRootAuthority` stays a single-field request-state newtype owned
        // by typeck. The two fact hosts may construct it only through `in_file`.
        assert!(
            TYPECK_SOURCE.contains("pub(super) struct TypeRootAuthority {\n    file: FileId,\n}")
        );
        assert!(TYPECK_SOURCE.contains("pub(super) fn in_file(file: FileId) -> Self {"));
        for (file, source) in RESOLVER_SOURCES {
            if matches!(*file, "typeck.rs" | "provider_body_host.rs") {
                continue;
            }
            assert!(
                !source.contains("TypeRootAuthority"),
                "{file} names request-state authority outside the two fact hosts"
            );
        }
        for construction in TYPECK_SOURCE.match_indices("TypeRootAuthority::") {
            let tail = &TYPECK_SOURCE[construction.0 + "TypeRootAuthority::".len()..];
            assert!(
                tail.starts_with("in_file(") || tail.starts_with("file("),
                "the root authority exposes only a scoped constructor and accessor"
            );
        }

        // No resolver may invent a scope by defaulting to the root file. The
        // deleted family did exactly this: `FileId::DEFAULT` plus a default
        // span, so a name written in any module resolved as if written in the
        // program's root.
        for (file, source) in RESOLVER_SOURCES {
            for forged in [
                "in_file(FileId::DEFAULT)",
                "root_file: FileId::DEFAULT",
                "resolve_type_syntax_with_substitutions(\n            &syntax,\n            FileId::DEFAULT,",
                "resolve_function_name_local(name, FileId::DEFAULT)",
            ] {
                assert!(
                    !source.contains(forged),
                    "{file} anchors a name lookup at the root file instead of a real scope: {forged}"
                );
            }
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
        assert!(
            OWNERSHIP_SOURCE
                .contains("impl<H: OrdinaryBodyAnalysisHost> OrdinaryBodyEngine<'_, H>")
        );
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
                ".moved_vars",
                "AirInstData::PlaceRead",
                ".make_place(",
                ".cancel_move_marker(",
                ".byref_arg_root",
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
        assert!(CONTROL_FLOW_SOURCE.contains("ctx.ownership.moved_vars"));

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
        assert!(
            include_str!("provider_strings_ownership_tests.rs")
                .contains("AirInstData::StorageLive")
        );
    }

    #[test]
    fn successful_field_lookup_keeps_the_interned_name_borrowed() {
        for (file, source) in [
            ("analysis/type_inference.rs", TYPE_INFERENCE_SOURCE),
            ("analysis/ownership.rs", OWNERSHIP_SOURCE),
        ] {
            assert!(
                !source.contains("resolve(&field).to_string()"),
                "{file} eagerly allocates an owned field name before lookup"
            );
        }

        assert_eq!(
            TYPE_INFERENCE_SOURCE
                .matches("field_name: field_name.to_string()")
                .count(),
            1,
            "type inference must allocate the field name only for its unknown-field diagnostic"
        );
        assert_eq!(
            OWNERSHIP_SOURCE
                .matches("field_name: field_name.to_string()")
                .count(),
            2,
            "ownership analysis must allocate field names only for its unknown-field diagnostics"
        );
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
            ".moved_vars",
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

        assert!(ORDINARY_ENGINE_SOURCE.contains("fn validate_explicit_call_modes"));
        assert!(!CALLS_SOURCE.contains("fn validate_explicit_call_modes"));

        assert!(CALLS_SOURCE.contains("self.analyze_call_args_coerced("));
        assert!(CALLS_SOURCE.contains("fn check_module_member_access("));
        assert!(!ANALYSIS_ROOT_SOURCE.contains("fn check_module_member_call("));
        assert!(!ANALYSIS_ROOT_SOURCE.contains("fn emit_module_member_call("));
        assert!(INTRINSICS_SOURCE.contains("let known = &self.known_symbols();"));
        assert!(!ANALYZE_OPS_SOURCE.contains("KnownSymbols"));
    }
}
