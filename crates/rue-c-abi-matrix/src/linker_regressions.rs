//! Native C archive regressions for Mach-O symbol binding.
//!
//! These cases intentionally compile separate archive members with the host C
//! toolchain. The internal linker must use the Mach-O symbol metadata to pull
//! a private-external provider and to resolve duplicate weak definitions.

use std::process::Command;

use libtest2_mimic::{RunContext, RunError, Trial};
use rue_target::Target;
use rue_test_runner::compiler_command;

const C_FLAGS: &[&str] = &["-c", "-O0", "-fno-stack-protector"];

struct Case {
    name: &'static str,
    members: &'static [(&'static str, &'static str)],
    rue_source: &'static str,
    archive: &'static str,
}

const CASES: &[Case] = &[
    Case {
        name: "private_external_archive_member",
        members: &[
            (
                "a.c",
                "__attribute__((visibility(\"hidden\"))) int helper(void);\nint probe(void) { return helper(); }\n",
            ),
            (
                "b.c",
                "__attribute__((visibility(\"hidden\"))) int helper(void) { return 42; }\n",
            ),
        ],
        rue_source: "extern \"C\" { fn probe() -> i32; }\nfn main() -> i32 { let result = checked { probe() }; if result == 42 { 0 } else { 1 } }\n",
        archive: "libprivate.a",
    },
    Case {
        name: "weak_definitions_across_archive_members",
        members: &[
            (
                "a.c",
                "__attribute__((weak)) int shared(void) { return 42; }\nint probe_a(void) { return shared(); }\n",
            ),
            (
                "b.c",
                "__attribute__((weak)) int shared(void) { return 42; }\nint probe_b(void) { return shared(); }\n",
            ),
        ],
        rue_source: "extern \"C\" { fn probe_a() -> i32; fn probe_b() -> i32; }\nfn main() -> i32 { let a = checked { probe_a() }; let b = checked { probe_b() }; if a + b == 84 { 0 } else { 1 } }\n",
        archive: "libweak.a",
    },
];

pub(crate) fn trials() -> Vec<Trial> {
    CASES
        .iter()
        .map(|case| {
            Trial::test(
                format!("macho_symbol_binding::{}", case.name),
                move |context: RunContext<'_>| {
                    if !super::toolchain_available() {
                        return context.ignore_for("no system cc and ar toolchain on PATH");
                    }
                    let Some(target) = Target::host() else {
                        return context.ignore_for("no native Rue target describes this host");
                    };
                    if !target.is_macho() {
                        return context
                            .ignore_for("Mach-O archive regression requires a macOS host");
                    }
                    run_case(case).map_err(RunError::fail)
                },
            )
        })
        .collect()
}

fn run_case(case: &Case) -> Result<(), String> {
    let directory = tempfile::Builder::new()
        .prefix("rue-macho-symbol-binding-")
        .tempdir()
        .map_err(|error| format!("could not create temporary directory: {error}"))?;

    let mut object_names = Vec::with_capacity(case.members.len());
    for (source_name, source) in case.members {
        std::fs::write(directory.path().join(source_name), source)
            .map_err(|error| format!("could not write {source_name}: {error}"))?;
        let object_name = format!("{}.o", source_name);
        let mut cc = Command::new("cc");
        cc.current_dir(directory.path())
            .args(C_FLAGS)
            .arg(source_name)
            .arg("-o")
            .arg(&object_name);
        super::run_step("cc", cc)?;
        object_names.push(object_name);
    }

    let mut ar = Command::new("ar");
    ar.current_dir(directory.path())
        .arg("rcs")
        .arg(case.archive);
    ar.args(&object_names);
    super::run_step("ar", ar)?;

    std::fs::write(directory.path().join("main.rue"), case.rue_source)
        .map_err(|error| format!("could not write main.rue: {error}"))?;
    let rue = super::rue_binary()?;
    let mut compile = compiler_command(&rue);
    compile.current_dir(directory.path()).args([
        "main.rue",
        "--preview",
        "c_ffi",
        "--linker",
        "internal",
        "--link-archive",
        case.archive,
        "-o",
        "prog",
    ]);
    super::run_step("rue", compile)?;

    let mut run = Command::new(directory.path().join("prog"));
    run.current_dir(directory.path());
    super::run_step("program", run)?;
    Ok(())
}
