//! Cross-phase verification for ADR-0056 payload schemas.
//!
//! Phase crates own record layouts and malformed fixtures. This external test
//! owns the shared production pattern: compile bounded deterministic programs
//! through `CompilerSession`, require all three validated artifacts, then
//! clone and display the published CFGs. The explicit family inventories are
//! the deliberate review point when an owning schema gains a new family.

use rue_air::AIR_PAYLOAD_FAMILY_NAMES;
use rue_cfg::CFG_PAYLOAD_FAMILY_NAMES;
use rue_compiler::{CompileOptions, CompilerSession, SourceSnapshot};
use rue_rir::RIR_PAYLOAD_FAMILY_NAMES;

const EXPECTED_RIR: [&str; 17] = [
    "match arms",
    "directives",
    "parameters",
    "call arguments",
    "intrinsic arguments",
    "internal intrinsic arguments",
    "block instructions",
    "struct fields",
    "anonymous struct fields",
    "struct methods",
    "anonymous struct methods",
    "field initializers",
    "enum variants",
    "anonymous enum variants",
    "enum variant payloads",
    "anonymous enum variant payloads",
    "array elements",
];

const EXPECTED_AIR: [&str; 10] = [
    "match_arms",
    "call_args",
    "type_args",
    "const_values",
    "intrinsic_args",
    "block_statements",
    "struct_fields",
    "source_order",
    "array_elements",
    "enum_payload",
];

const EXPECTED_CFG: [&str; 10] = [
    "intrinsic arguments",
    "struct fields",
    "array elements",
    "enum payload",
    "goto arguments",
    "then arguments",
    "else arguments",
    "call arguments",
    "switch cases",
    "projections",
];

#[derive(Clone, Copy)]
struct CoverageRow {
    phase: &'static str,
    family: &'static str,
}

macro_rules! row {
    ($phase:literal, $family:literal) => {
        CoverageRow {
            phase: $phase,
            family: $family,
        }
    };
}

/// Deliberately exhaustive owner/family dispatch table for the executable
/// malformed-range and malformed-scalar probes below. Round-trip evidence is
/// owner-local so it constructs the private descriptor types; production
/// publication, clone/rewrite, and stable display are exercised below.
const COVERAGE_MATRIX: [CoverageRow; 37] = [
    row!("RIR", "match arms"),
    row!("RIR", "directives"),
    row!("RIR", "parameters"),
    row!("RIR", "call arguments"),
    row!("RIR", "intrinsic arguments"),
    row!("RIR", "internal intrinsic arguments"),
    row!("RIR", "block instructions"),
    row!("RIR", "struct fields"),
    row!("RIR", "anonymous struct fields"),
    row!("RIR", "struct methods"),
    row!("RIR", "anonymous struct methods"),
    row!("RIR", "field initializers"),
    row!("RIR", "enum variants"),
    row!("RIR", "anonymous enum variants"),
    row!("RIR", "enum variant payloads"),
    row!("RIR", "anonymous enum variant payloads"),
    row!("RIR", "array elements"),
    row!("AIR", "match_arms"),
    row!("AIR", "call_args"),
    row!("AIR", "type_args"),
    row!("AIR", "const_values"),
    row!("AIR", "intrinsic_args"),
    row!("AIR", "block_statements"),
    row!("AIR", "struct_fields"),
    row!("AIR", "source_order"),
    row!("AIR", "array_elements"),
    row!("AIR", "enum_payload"),
    row!("CFG", "intrinsic arguments"),
    row!("CFG", "struct fields"),
    row!("CFG", "array elements"),
    row!("CFG", "enum payload"),
    row!("CFG", "goto arguments"),
    row!("CFG", "then arguments"),
    row!("CFG", "else arguments"),
    row!("CFG", "call arguments"),
    row!("CFG", "switch cases"),
    row!("CFG", "projections"),
];

#[test]
fn owner_schema_inventories_are_complete_and_deliberate() {
    assert_eq!(RIR_PAYLOAD_FAMILY_NAMES, EXPECTED_RIR);
    assert_eq!(AIR_PAYLOAD_FAMILY_NAMES, EXPECTED_AIR);
    assert_eq!(CFG_PAYLOAD_FAMILY_NAMES, EXPECTED_CFG);
    let mapped = COVERAGE_MATRIX
        .iter()
        .map(|row| row.family)
        .collect::<Vec<_>>();
    assert_eq!(&mapped[..17], EXPECTED_RIR);
    assert_eq!(&mapped[17..27], EXPECTED_AIR);
    assert_eq!(&mapped[27..], EXPECTED_CFG);
    for row in COVERAGE_MATRIX {
        let index = match row.phase {
            "RIR" => EXPECTED_RIR.iter().position(|family| *family == row.family),
            "AIR" => EXPECTED_AIR.iter().position(|family| *family == row.family),
            "CFG" => EXPECTED_CFG.iter().position(|family| *family == row.family),
            phase => panic!("unknown payload phase {phase}"),
        }
        .expect("coverage row family is in its owner inventory");
        let input = [u8::try_from(index).unwrap(), 0];
        let error_family = match row.phase {
            "RIR" => {
                rue_rir_fuzz_support::Rir::fuzz_payload_corruption(&input)
                    .unwrap_err()
                    .family
            }
            "AIR" => {
                rue_air_fuzz_support::Air::fuzz_payload_corruption(&input)
                    .unwrap_err()
                    .family
            }
            "CFG" => rue_cfg_fuzz_support::fuzz_payload_corruption(&input)
                .unwrap_err()
                .family(),
            _ => unreachable!(),
        };
        assert_eq!(error_family, row.family);

        // A second probe is in-bounds and scalar-corrupt. Unlike the range
        // boundary above, this reaches each owner's family-specific decoder
        // (tag/record/context validation where that family has such fields).
        let scalar_input = [u8::try_from(index).unwrap(), 3, 0xff, 0xff, 0xff, 0xff];
        match row.phase {
            "RIR" => {
                let _ = std::hint::black_box(rue_rir_fuzz_support::Rir::fuzz_payload_corruption(
                    &scalar_input,
                ));
            }
            "AIR" => {
                let _ = std::hint::black_box(rue_air_fuzz_support::Air::fuzz_payload_corruption(
                    &scalar_input,
                ));
            }
            "CFG" => {
                let _ = std::hint::black_box(rue_cfg_fuzz_support::fuzz_payload_corruption(
                    &scalar_input,
                ));
            }
            _ => unreachable!(),
        }
    }
}

fn observe(source: &str) -> Vec<String> {
    let snapshot = SourceSnapshot::single("<payload-verification>", source).unwrap();
    let mut session = CompilerSession::new();
    session.update(&snapshot).into_result().unwrap();
    rue_compiler::unstable::rooted_cfg(&mut session, &CompileOptions::default())
        .unwrap()
        .functions()
        .iter()
        .map(|function| {
            let before = function.cfg().to_string();
            let cloned = function.cfg().clone();
            assert_eq!(cloned.to_string(), before);
            before
        })
        .collect()
}

#[test]
fn representative_builders_views_clone_rewrite_and_display_are_stable() {
    let source = r#"
struct Pair { left: i32, right: i32 }
enum Choice { None, One(i32), Pair(i32, i32) }

fn add(inout value: i32, delta: i32) { value = value + delta; }

fn choose(value: Choice) -> i32 {
    match value {
        Choice.None => 0,
        Choice.One(x) => x,
        Choice.Pair(x, y) => x + y,
    }
}

fn main() -> i32 {
    let mut total = 0;
    let values = [1, 2, 3, 4];
    let pair = Pair { right: values[1], left: values[0] };
    if pair.left < pair.right { add(inout total, pair.right); }
    let mut i = 0;
    while i < 4 { total = total + values[i]; i = i + 1; }
    total + choose(Choice.Pair(5, 6))
}
"#;
    let first = observe(source);
    let second = observe(source);
    assert_eq!(
        first, second,
        "validated CFG display changed between builds"
    );
    assert!(!first.is_empty());
}

fn generated_sequence(mut seed: u64, operations: usize) -> String {
    let mut body = String::from("fn main() -> i32 { let mut x = 1;\n");
    for index in 0..operations {
        // Fixed LCG: deterministic on every host and bounded independently of
        // the fuzzing engine.
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
        let value = ((seed >> 32) % 31) as i32 + 1;
        match seed & 3 {
            0 => body.push_str(&format!("x = x + {value};\n")),
            1 => body.push_str(&format!("x = x ^ {value};\n")),
            2 => body.push_str(&format!("if x < {value} {{ x = x + {index}; }}\n")),
            _ => body.push_str(&format!("x = (x * {value}) / {value};\n")),
        }
    }
    body.push_str("x\n}");
    body
}

#[test]
fn bounded_randomized_instruction_sequences_publish_deterministically() {
    for seed in 0..32 {
        let source = generated_sequence(seed, 24);
        assert_eq!(observe(&source), observe(&source), "seed {seed}");
    }
}
