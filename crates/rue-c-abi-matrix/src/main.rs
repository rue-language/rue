//! The generated C-boundary conformance matrix (RUE-2035).
//!
//! Rue's existing C-boundary execution evidence is hand-written: a handful of
//! CLI cases whose C side is hand-assembled machine code in
//! `crates/rue-cli-tests`. That proves a boundary works; it does not scale to a
//! matrix, and the native-ABI work this exists to protect changes the placement
//! of every argument. So this harness *generates* the matrix instead.
//!
//! For one direction and one ABI spelling it emits a paired `.c` and `.rue`
//! program covering every shape at every position (see [`grid`]), compiles the
//! C side with the host `cc`, compiles the Rue side with the real compiler
//! binary, links the two with `--linker cc --link-archive`, runs the executable,
//! and compares its stdout — line for line — with values this process computed
//! from the same table the sources were emitted from. A mismatch names the
//! direction, the shape, the position, the ABI spelling, and the function.
//!
//! # Why the system linker
//!
//! The C objects `cc` produces carry relocation and section kinds Rue's
//! internal linker's static subset does not promise to handle, so every program
//! here links through `--linker cc`, which passes `-nostdlib` (plus `-static` on
//! ELF), the supplied archives, and the Rue runtime archive to the system
//! driver. That is also why the generated C is freestanding: no headers, no
//! libc, fixed-width typedefs spelled from the target's own data model, and
//! `_Static_assert`s that fail the compile if that data model is not what the
//! generator assumed.
//!
//! # Scope
//!
//! Host-only by construction: the matrix compiles for and executes on the
//! machine running it, and the native lanes are what give it AArch64 Linux and
//! Apple arm64 coverage. Without a `cc` driver on `PATH` every trial reports
//! *ignored* rather than failing, the same rule the CLI corpus's
//! `requires_system_linker` cases follow.

mod grid;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;
use std::time::Duration;

use libtest2_mimic::{Harness, RunContext, RunError, Trial};
use rue_target::{CallingConvention, DataModel, Target};
use rue_test_runner::{compiler_command, find_rue_binary, ice_message, run_with_timeout};

use grid::{Direction, Program};

/// Freestanding C: no headers, no libc, no builtins, and no stack protector —
/// the object is linked with `-nostdlib`, so a protector call would be an
/// undefined `__stack_chk_fail`. Position-independent code keeps the object
/// acceptable to a driver defaulting to PIE. Identical on every row; the
/// generated C carries no platform conditionals.
const C_FLAGS: &[&str] = &[
    "-std=c11",
    "-ffreestanding",
    "-nostdlib",
    "-fno-builtin",
    "-fno-stack-protector",
    "-fPIC",
    "-O2",
    "-c",
];

const C_COMPILE_TIMEOUT: Duration = Duration::from_secs(120);
const ARCHIVE_TIMEOUT: Duration = Duration::from_secs(60);
const RUE_COMPILE_TIMEOUT: Duration = Duration::from_secs(300);
const RUN_TIMEOUT: Duration = Duration::from_secs(120);

/// Whether a system `cc` driver and an `ar` archiver are both on `PATH`, probed
/// once per process. Both are needed: `cc` compiles the C side and performs the
/// final link, and `ar` wraps the object in the static archive `--link-archive`
/// documents as its input.
fn toolchain_available() -> bool {
    static AVAILABLE: OnceLock<bool> = OnceLock::new();
    *AVAILABLE.get_or_init(|| ["cc", "ar"].iter().all(|tool| responds_to_version(tool)))
}

fn responds_to_version(tool: &str) -> bool {
    Command::new(tool)
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

/// The compiler path, made absolute: every subprocess below runs with the
/// program's temp directory as its working directory, where a Buck-supplied
/// relative `RUE_BINARY` would no longer resolve.
fn rue_binary() -> Result<PathBuf, String> {
    let binary = find_rue_binary();
    std::fs::canonicalize(&binary).map_err(|error| {
        format!(
            "RUE_BINARY `{}` does not resolve: {error}",
            binary.display()
        )
    })
}

fn host_target() -> Result<Target, String> {
    let target =
        Target::host().ok_or("no Rue target describes this host; the matrix runs natively only")?;
    if target.data_model() != DataModel::Lp64 {
        return Err(format!(
            "the generated C spells `long` as the 64-bit integer, which holds only under LP64; \
             {target} uses {}",
            target.data_model()
        ));
    }
    Ok(target)
}

fn step_timeout(step: &str) -> Duration {
    match step {
        "cc" => C_COMPILE_TIMEOUT,
        "ar" => ARCHIVE_TIMEOUT,
        "rue" => RUE_COMPILE_TIMEOUT,
        _ => RUN_TIMEOUT,
    }
}

/// Run one step, turning a nonzero status, a compiler ICE, or a timeout into a
/// failure that names the step.
fn run_step(step: &str, command: Command) -> Result<String, String> {
    let output = run_with_timeout(command, step_timeout(step), None)
        .map_err(|failure| format!("{step}: {failure}"))?;
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    if let Some(ice) = ice_message(&output.status, &stderr) {
        return Err(format!("{step}: {ice}"));
    }
    if !output.status.success() {
        return Err(format!(
            "{step} failed ({})\n--- stderr ---\n{stderr}\n--- stdout ---\n{}",
            output.status,
            String::from_utf8_lossy(&output.stdout)
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

/// Compile, link, and run one generated program, then compare its output with
/// the checksums the generator computed.
fn run_program(program: &Program, directory: &Path, rue: &Path) -> Result<(), String> {
    std::fs::write(directory.join("boundary.c"), &program.c_source)
        .map_err(|error| format!("could not write boundary.c: {error}"))?;
    std::fs::write(directory.join("main.rue"), &program.rue_source)
        .map_err(|error| format!("could not write main.rue: {error}"))?;

    let mut cc = Command::new("cc");
    cc.current_dir(directory)
        .args(C_FLAGS)
        .args(["boundary.c", "-o", "boundary.o"]);
    run_step("cc", cc)?;

    let mut ar = Command::new("ar");
    ar.current_dir(directory)
        .args(["rcs", "libboundary.a", "boundary.o"]);
    run_step("ar", ar)?;

    let mut compile = compiler_command(rue);
    compile.current_dir(directory).args([
        "main.rue",
        "--preview",
        "c_ffi",
        "--linker",
        "cc",
        "--link-archive",
        "libboundary.a",
        "-o",
        "prog",
    ]);
    run_step("rue", compile)?;

    let mut run = Command::new(directory.join("prog"));
    run.current_dir(directory);
    let stdout = run_step("program", run)?;

    compare(program, &stdout)
}

/// Compare the program's stdout with the expected checksums, reporting every
/// disagreeing cell by what it crosses rather than by line number.
fn compare(program: &Program, stdout: &str) -> Result<(), String> {
    let actual: Vec<&str> = stdout.lines().collect();
    if actual.len() != program.expected.len() {
        return Err(format!(
            "the program printed {} lines; the grid has {} cells. Either a cell did not run, or \
             the program aborted partway through.\n--- stdout ---\n{stdout}",
            actual.len(),
            program.expected.len()
        ));
    }

    let mismatches: Vec<String> = program
        .cells
        .iter()
        .zip(&program.expected)
        .zip(&actual)
        .filter(|((_, expected), actual)| expected.as_str() != **actual)
        .map(|((cell, expected), actual)| {
            format!(
                "  {}\n    expected {expected}, got {actual}",
                cell.describe()
            )
        })
        .collect();

    if mismatches.is_empty() {
        return Ok(());
    }
    Err(format!(
        "{} of {} C-boundary cells produced the wrong checksum:\n{}",
        mismatches.len(),
        program.expected.len(),
        mismatches.join("\n")
    ))
}

fn run_matrix(direction: Direction, abi: &str) -> Result<(), String> {
    let target = host_target()?;
    let spec = CallingConvention::c_for_target(target).c_spec();
    let program = grid::generate(direction, abi, &spec);

    let directory = tempfile::Builder::new()
        .prefix("rue-c-abi-matrix-")
        .tempdir()
        .map_err(|error| format!("could not create a temp directory: {error}"))?;
    let rue = rue_binary()?;
    match run_program(&program, directory.path(), &rue) {
        Ok(()) => Ok(()),
        Err(failure) => {
            // Keep the generated pair on failure: the sources are the only way
            // to reduce a failing cell to a minimal repro.
            let kept = directory.keep();
            Err(format!(
                "{failure}\n\nGenerated sources kept at {}",
                kept.display()
            ))
        }
    }
}

fn trial(direction: Direction, abi: &'static str) -> Trial {
    let name = format!(
        "c_abi_matrix::{}_{}",
        direction.key(),
        abi.replace('-', "_")
    );
    Trial::test(name, move |context: RunContext<'_>| {
        // A host without a C toolchain cannot prove anything here, and saying so
        // is not the same as passing — the rule RUE-1173 set for the CLI
        // corpus's `--linker cc` cases.
        if !toolchain_available() {
            return context.ignore_for("no system `cc` and `ar` toolchain on PATH");
        }
        run_matrix(direction, abi).map_err(RunError::fail)
    })
}

fn main() {
    // The host's own C row, spelled both ways a declaration may spell it.
    // Proving the two separately by execution is what makes `"C"` an alias
    // rather than a second convention.
    let explicit = match Target::host() {
        Some(target) => CallingConvention::c_for_target(target).name(),
        // No host row: the `"C"` trials still register and report that from
        // `host_target`, so the harness never silently runs nothing.
        None => "C",
    };

    let mut trials = vec![trial(Direction::Import, "C"), trial(Direction::Export, "C")];
    if explicit != "C" {
        trials.push(trial(Direction::Import, explicit));
        trials.push(trial(Direction::Export, explicit));
    }

    Harness::with_env().discover(trials).main();
}

#[cfg(test)]
mod tests {
    use super::grid::{
        Bank, Direction, Leaf, PROBE_BYTES, Position, SHAPES, Shape, Ty, Value, generate,
        positions_for,
    };
    use rue_air::{
        AggregateLeaves, ArgConvention, ArgLocation, CAbiLeaf, CAbiLeafKind, CAbiScalarKind,
        CAbiTypeFacts, PointerLocation, lower_c_signature,
    };
    use rue_target::{CConventionSpec, CRegisterClass, CallingConvention, Target};

    fn spec() -> CConventionSpec {
        CallingConvention::X86_64SysV.c_spec()
    }

    /// Every C row the grid is generated for. The host runs one of them; the
    /// generator's own answers are checkable for all three from any host.
    const ROWS: [CallingConvention; 3] = [
        CallingConvention::X86_64SysV,
        CallingConvention::Aarch64Aapcs,
        CallingConvention::Aarch64AapcsDarwin,
    ];

    /// Every shape's argument positions under `spec`, as one flat list.
    fn argument_positions(spec: &CConventionSpec) -> Vec<(&'static Shape, Position)> {
        SHAPES
            .iter()
            .flat_map(|shape| {
                positions_for(spec, shape)
                    .into_iter()
                    .map(move |position| (shape, position))
            })
            .filter(|(_, position)| matches!(position, Position::Argument { .. }))
            .collect()
    }

    /// The width of one leaf in the C layout both generated sources declare.
    fn leaf_bytes(leaf: Leaf) -> u64 {
        match leaf {
            Leaf::I8 | Leaf::U8 | Leaf::Bool => 1,
            Leaf::I16 | Leaf::U16 => 2,
            Leaf::I32 | Leaf::U32 | Leaf::F32 => 4,
            Leaf::I64 | Leaf::U64 | Leaf::F64 | Leaf::Ptr => 8,
        }
    }

    /// A shape's alignment: its leaf's own, or its widest field's.
    fn align_of(ty: Ty) -> u64 {
        match ty {
            Ty::Leaf(leaf) | Ty::Array(leaf, _) => leaf_bytes(leaf),
            Ty::Struct(def) => def
                .fields
                .iter()
                .map(|field| align_of(field.ty))
                .max()
                .expect("every grid struct has at least one field"),
        }
    }

    /// A shape's `sizeof`: fields at their own alignments in declaration order,
    /// the whole padded to the shape's alignment.
    fn size_of(ty: Ty) -> u64 {
        match ty {
            Ty::Leaf(leaf) => leaf_bytes(leaf),
            Ty::Array(leaf, len) => leaf_bytes(leaf) * len as u64,
            Ty::Struct(_) => {
                let align = align_of(ty);
                let mut end = 0;
                collect_leaves(ty, 0, &mut end, &mut Vec::new());
                end.div_ceil(align) * align
            }
        }
    }

    /// Append every scalar leaf of `ty` at its byte offset from `base`, leaving
    /// the byte after the last one in `end`.
    fn collect_leaves(ty: Ty, base: u64, end: &mut u64, out: &mut Vec<CAbiLeaf>) {
        match ty {
            Ty::Leaf(leaf) => {
                let width = leaf_bytes(leaf);
                out.push(CAbiLeaf {
                    offset: base,
                    width,
                    kind: match leaf {
                        Leaf::F32 => CAbiLeafKind::F32,
                        Leaf::F64 => CAbiLeafKind::F64,
                        _ => CAbiLeafKind::Integer,
                    },
                });
                *end = base + width;
            }
            Ty::Array(leaf, len) => {
                for index in 0..len as u64 {
                    collect_leaves(Ty::Leaf(leaf), base + index * leaf_bytes(leaf), end, out);
                }
            }
            Ty::Struct(def) => {
                let mut offset = base;
                for field in def.fields {
                    let align = align_of(field.ty);
                    offset = offset.div_ceil(align) * align;
                    collect_leaves(field.ty, offset, end, out);
                    offset = *end;
                }
                *end = offset;
            }
        }
    }

    /// The classifier's view of one shape or filler type.
    fn facts(ty: Ty) -> CAbiTypeFacts {
        if let Ty::Leaf(leaf) = ty {
            let kind = match leaf {
                Leaf::I8 => CAbiScalarKind::I8,
                Leaf::I16 => CAbiScalarKind::I16,
                Leaf::I32 => CAbiScalarKind::I32,
                Leaf::U8 => CAbiScalarKind::U8,
                Leaf::U16 => CAbiScalarKind::U16,
                Leaf::U32 => CAbiScalarKind::U32,
                Leaf::Bool => CAbiScalarKind::Bool,
                Leaf::F32 => CAbiScalarKind::F32,
                Leaf::F64 => CAbiScalarKind::F64,
                Leaf::I64 | Leaf::U64 | Leaf::Ptr => CAbiScalarKind::RegisterWidth,
            };
            return CAbiTypeFacts::Scalar {
                kind,
                class: kind.register_class(),
            };
        }
        let size = size_of(ty);
        let mut leaves = Vec::new();
        collect_leaves(ty, 0, &mut 0, &mut leaves);
        CAbiTypeFacts::Aggregate {
            size,
            align: align_of(ty),
            leaves: AggregateLeaves::from_leaves(size, leaves),
        }
    }

    /// Where `shape` lands when a cell passes it at `position`, according to the
    /// one classifier the compiler places by.
    fn placement(
        convention: CallingConvention,
        shape: &Shape,
        position: Position,
    ) -> Option<ArgLocation> {
        let Position::Argument { index, arity, .. } = position else {
            return None;
        };
        let parameters: Vec<(CAbiTypeFacts, ArgConvention)> = (0..arity)
            .map(|slot| {
                let ty = if slot == index {
                    shape.ty
                } else {
                    Ty::Leaf(position.filler_leaf(slot))
                };
                (facts(ty), ArgConvention::ByValue)
            })
            .collect();
        // Every argument cell answers the `u64` checksum, so no row's hidden
        // indirect-result pointer shifts the argument registers.
        let lowered = lower_c_signature(convention, &parameters, facts(Ty::Leaf(Leaf::U64)));
        Some(lowered.arguments()[index].location)
    }

    /// Whether a placement reaches the callee without the outgoing argument
    /// area: in registers, or as a by-reference copy whose pointer is in one.
    fn in_registers(location: ArgLocation) -> bool {
        match location {
            ArgLocation::Registers { .. } => true,
            ArgLocation::Indirect { pointer, .. } => {
                matches!(pointer, PointerLocation::Register { .. })
            }
            ArgLocation::Stack { .. } | ArgLocation::Omitted => false,
        }
    }

    #[test]
    fn every_shape_reaches_every_position_in_both_directions() {
        let cells: usize = SHAPES
            .iter()
            .map(|shape| positions_for(&spec(), shape).len())
            .sum();
        for direction in [Direction::Import, Direction::Export] {
            let program = generate(direction, "C", &spec());
            assert_eq!(
                program.cells.len(),
                cells,
                "the grid must be every shape's whole position set"
            );
            assert_eq!(program.cells.len(), program.expected.len());
            for shape in SHAPES {
                for position in positions_for(&spec(), shape) {
                    assert!(
                        program
                            .cells
                            .iter()
                            .any(|cell| cell.shape == shape.key && cell.position == position.key()),
                        "no cell for {} at {}",
                        shape.key,
                        position.key()
                    );
                }
            }
        }
    }

    #[test]
    fn positions_come_from_both_register_rosters() {
        for convention in ROWS {
            let spec = convention.c_spec();
            let gp = spec.gp_argument_registers as usize;
            let fp = spec.fp_argument_registers as usize;
            for shape in SHAPES {
                let float_leaved = shape
                    .ty
                    .leaves()
                    .iter()
                    .any(|(_, leaf)| matches!(leaf, Leaf::F32 | Leaf::F64));
                let places: Vec<(&'static str, Bank, usize, usize)> = positions_for(&spec, shape)
                    .into_iter()
                    .filter_map(|position| match position {
                        Position::Argument {
                            key,
                            bank,
                            index,
                            arity,
                        } => Some((key, bank, index, arity)),
                        Position::Return => None,
                    })
                    .collect();
                let mut expected = vec![
                    ("arg0", Bank::Gp, 0, gp + 5),
                    ("gp_last_reg", Bank::Gp, gp - 1, gp + 5),
                    ("gp_first_stack", Bank::Gp, gp, gp + 5),
                    ("gp_deep_stack", Bank::Gp, gp + 3, gp + 5),
                ];
                if float_leaved {
                    expected.push(("fp_last_reg", Bank::Fp, fp - 1, fp + 5));
                }
                expected.push(("fp_first_stack", Bank::Fp, fp, fp + 5));
                if float_leaved {
                    expected.push(("fp_deep_stack", Bank::Fp, fp + 3, fp + 5));
                }
                assert_eq!(places, expected, "positions for {}", shape.key);
            }
        }
    }

    #[test]
    fn a_filler_run_spends_its_own_roster_and_ends_in_the_other() {
        for convention in ROWS {
            let spec = convention.c_spec();
            for (shape, position) in argument_positions(&spec) {
                let Position::Argument {
                    key, bank, arity, ..
                } = position
                else {
                    unreachable!("argument_positions keeps only argument positions");
                };
                let own = match bank {
                    Bank::Gp => CRegisterClass::Gp,
                    Bank::Fp => CRegisterClass::Fp,
                };
                let class = |slot: usize| match position.filler_leaf(slot) {
                    Leaf::F32 | Leaf::F64 => CRegisterClass::Fp,
                    _ => CRegisterClass::Gp,
                };
                assert!(
                    (0..arity - 1).all(|slot| class(slot) == own),
                    "{}/{key}: every filler before the last must be the position's own bank",
                    shape.key
                );
                assert_ne!(
                    class(arity - 1),
                    own,
                    "{}/{key}: the last filler must be the other bank's",
                    shape.key
                );
                // The run reaches three registers past its roster, and the list
                // adds the shape's own slot and the cross-bank filler on top, so
                // every cell stacks arguments whatever the shape's placement is.
                assert_eq!(
                    arity,
                    spec.argument_registers(own) as usize + 5,
                    "{}/{key}: the run must overflow its roster",
                    shape.key
                );
            }
        }
    }

    /// The coverage property the position set exists for: every shape is
    /// stacked somewhere, and every shape that can travel in registers at all
    /// does so somewhere. Both answers come from `lower_c_signature`, the one
    /// classifier the compiler places by, rather than from reading the table.
    #[test]
    fn every_shape_is_stacked_and_registered_where_the_classifier_allows() {
        for convention in ROWS {
            let spec = convention.c_spec();
            let mut cells = 0usize;
            for shape in SHAPES {
                let positions = positions_for(&spec, shape);
                cells += positions.len();
                let placements: Vec<(Position, ArgLocation)> = positions
                    .iter()
                    .filter_map(|position| {
                        placement(convention, shape, *position)
                            .map(|location| (*position, location))
                    })
                    .collect();
                assert!(
                    placements
                        .iter()
                        .any(|(_, location)| !in_registers(*location)),
                    "{}: no position of the {} row puts it on the stack",
                    shape.key,
                    convention.name()
                );
                // A shape travels in registers somewhere exactly when it does at
                // the one position where neither roster is spent; anything else
                // is a shape the row always passes through memory.
                let ever_registers = placements
                    .iter()
                    .any(|(_, location)| in_registers(*location));
                let registers_with_empty_rosters = placements
                    .iter()
                    .find(|(position, _)| position.key() == "arg0")
                    .map(|(_, location)| in_registers(*location))
                    .expect("every shape has the arg0 position");
                assert_eq!(
                    ever_registers,
                    registers_with_empty_rosters,
                    "{}: the {} row's register placement must be reachable from the position set",
                    shape.key,
                    convention.name()
                );
            }
            println!(
                "{}: {cells} cells per generated program over {} shapes",
                convention.name(),
                SHAPES.len()
            );
        }
    }

    #[test]
    fn cell_names_are_unique_within_a_program() {
        let program = generate(Direction::Import, "C", &spec());
        let mut names: Vec<&str> = program
            .cells
            .iter()
            .map(|cell| cell.function.as_str())
            .collect();
        names.sort_unstable();
        let count = names.len();
        names.dedup();
        assert_eq!(names.len(), count, "generated function names collide");
    }

    #[test]
    fn a_generated_program_declares_every_struct_before_it_is_used() {
        let program = generate(Direction::Export, "C", &spec());
        for shape in SHAPES {
            if let Ty::Struct(def) = shape.ty {
                assert!(
                    program
                        .rue_source
                        .contains(&format!("struct {} {{", def.name)),
                    "{} is missing from the Rue source",
                    def.name
                );
                assert!(
                    program.c_source.contains(&format!("}} {};", def.name)),
                    "{} is missing from the C source",
                    def.name
                );
            }
        }
        // The nested struct's inner type must be declared before the outer one
        // in C, where a typedef is not usable before its definition.
        let inner = program.c_source.find("} AbiI32I32;").expect("inner struct");
        let outer = program.c_source.find("} AbiNested;").expect("outer struct");
        assert!(inner < outer);
    }

    /// Whether `literal` appears in `source` as a whole number token rather
    /// than as the tail of a longer one.
    fn contains_literal(source: &str, literal: &str) -> bool {
        let bytes = source.as_bytes();
        let mut from = 0;
        while let Some(offset) = source[from..].find(literal) {
            let start = from + offset;
            let end = start + literal.len();
            let separated_before = start == 0 || !bytes[start - 1].is_ascii_digit();
            let separated_after = end == bytes.len() || !bytes[end].is_ascii_digit();
            if separated_before && separated_after {
                return true;
            }
            from = start + 1;
        }
        false
    }

    #[test]
    fn signed_values_stay_off_the_type_minimum_so_they_negate() {
        // Every emitted signed literal must be writable as a negated positive in
        // both languages, which the type minimum is not.
        for direction in [Direction::Import, Direction::Export] {
            let program = generate(direction, "C", &spec());
            for source in [&program.rue_source, &program.c_source] {
                for minimum in ["-9223372036854775808", "-2147483648", "-32768", "-128"] {
                    assert!(
                        !contains_literal(source, minimum),
                        "{minimum} is a type minimum and has no negated-positive spelling"
                    );
                }
            }
        }
    }

    #[test]
    fn the_whole_token_literal_search_ignores_longer_numbers() {
        assert!(contains_literal("v = -128;", "-128"));
        assert!(!contains_literal("v = -1284;", "-128"));
        assert!(!contains_literal("v = -3128;", "-128"));
    }

    #[test]
    fn a_pointer_contributes_its_pointee_not_its_address() {
        for (index, byte) in PROBE_BYTES.iter().enumerate() {
            assert_eq!(
                Value::Ptr(index as u8).contribution(Leaf::Ptr),
                u64::from(*byte)
            );
        }
    }

    #[test]
    fn narrow_signed_leaves_contribute_sign_extended_patterns() {
        assert_eq!(Value::Int(-1).contribution(Leaf::I8), u64::MAX);
        assert_eq!(Value::Int(-1).contribution(Leaf::I32), u64::MAX);
        assert_eq!(Value::Int(255).contribution(Leaf::U8), 255);
        assert_eq!(
            Value::Int(i128::from(u32::MAX)).contribution(Leaf::U32),
            u64::from(u32::MAX)
        );
    }

    #[test]
    fn the_host_row_is_a_convention_row_rather_than_the_alias() {
        if let Some(target) = Target::host() {
            let convention = CallingConvention::c_for_target(target);
            assert!(convention.is_c());
            assert_ne!(convention.name(), "C", "the alias is not a row name");
        }
    }
}
