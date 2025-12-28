use libtest2_mimic::RunContext;
use libtest2_mimic::RunError;
use libtest2_mimic::RunResult;
use libtest2_mimic::Trial;

fn main() {
    libtest2_mimic::Harness::with_env()
        .discover([
            Trial::test("check_toph", check_toph),
            Trial::test("check_katara", check_katara),
            Trial::test("check_sokka", check_sokka),
            Trial::test("long_computation", long_computation),
            Trial::test("compile_fail_dummy", compile_fail_dummy),
        ])
        .main();
}

// Tests

fn check_toph(_context: RunContext<'_>) -> RunResult {
    Ok(())
}
fn check_katara(_context: RunContext<'_>) -> RunResult {
    Ok(())
}
fn check_sokka(_context: RunContext<'_>) -> RunResult {
    Err(RunError::fail("Sokka tripped and fell :("))
}
fn long_computation(context: RunContext<'_>) -> RunResult {
    context.ignore_for("slow")?;

    std::thread::sleep(std::time::Duration::from_secs(1));
    Ok(())
}
fn compile_fail_dummy(_context: RunContext<'_>) -> RunResult {
    Ok(())
}
