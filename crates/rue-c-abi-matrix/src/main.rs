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
        Direction, Leaf, PROBE_BYTES, Position, SHAPES, Ty, Value, generate, positions_for,
    };
    use rue_target::{CConventionSpec, CallingConvention, Target};

    fn spec() -> CConventionSpec {
        CallingConvention::X86_64SysV.c_spec()
    }

    #[test]
    fn every_shape_reaches_every_position_in_both_directions() {
        let layout = positions_for(&spec());
        for direction in [Direction::Import, Direction::Export] {
            let program = generate(direction, "C", &spec());
            assert_eq!(
                program.cells.len(),
                SHAPES.len() * layout.positions.len(),
                "the grid must be the full shape x position product"
            );
            assert_eq!(program.cells.len(), program.expected.len());
            for shape in SHAPES {
                for position in &layout.positions {
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
    fn positions_come_from_the_conventions_register_budget() {
        for convention in [
            CallingConvention::X86_64SysV,
            CallingConvention::Aarch64Aapcs,
            CallingConvention::Aarch64AapcsDarwin,
        ] {
            let registers = convention.c_spec().gp_argument_registers as usize;
            let layout = positions_for(&convention.c_spec());
            let indices: Vec<usize> = layout
                .positions
                .iter()
                .filter_map(|position| match position {
                    Position::Argument { index, .. } => Some(*index),
                    Position::Return => None,
                })
                .collect();
            assert_eq!(indices, vec![0, registers - 1, registers, registers + 3]);
            assert!(
                layout.arity > registers,
                "every cell must also stack arguments"
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
