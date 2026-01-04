//! Consistency tests for RIR traversal.
//!
//! The sema module traverses the RIR twice with parallel match statements:
//! 1. Constraint generation (inference/generate.rs) - Walks RIR to generate type constraints
//! 2. AIR emission (sema/analysis.rs) - Walks RIR again to emit typed AIR
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
        "ParamRef",
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
        "IndexGet",
        "IndexSet",
        // Methods
        "MethodCall",
        "AssocFnCall",
        "DropFnDecl",
    ];

    // Include the source files at compile time
    // These paths are relative to the current source file
    const GENERATE_SOURCE: &str = include_str!("../inference/generate.rs");
    const ANALYSIS_SOURCE: &str = include_str!("analysis.rs");

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
}
