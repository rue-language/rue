//! Recording what the measurement environment actually was.
//!
//! An epoch pins the environment *policy*, not the exact machine. Hosted
//! runners change underneath a policy, so every run records what it found and
//! the dashboard renders comparisons across a change as advisory. Nothing here
//! prevents drift; it makes drift visible, which is the honest arrangement for
//! hardware nobody controls.
//!
//! Every probe degrades to a readable placeholder rather than failing. A run
//! that measured successfully must not be thrown away because `/proc/meminfo`
//! looked unfamiliar, and an `unknown` in the fingerprint is itself a fact
//! worth recording.

use std::process::Command;

use rue_perf_schema::EnvironmentFingerprint;

/// What a probe reports when the host does not answer.
const UNKNOWN: &str = "unknown";

/// Collect the current environment fingerprint.
///
/// The runner label and image come from the environment rather than being
/// detected, because they name a *policy* the epoch declares. GitHub-hosted
/// runners export `ImageOS` and `ImageVersion`; a local run says so plainly
/// instead of impersonating CI.
pub fn fingerprint() -> EnvironmentFingerprint {
    EnvironmentFingerprint {
        runner_label: var("RUE_BENCH_RUNNER_LABEL").unwrap_or_else(|| "local".to_string()),
        runner_image: var("RUE_BENCH_RUNNER_IMAGE")
            .or_else(|| var("ImageOS"))
            .unwrap_or_else(|| "local".to_string()),
        runner_image_version: var("ImageVersion").unwrap_or_else(|| UNKNOWN.to_string()),
        cpu_model: cpu_model(),
        core_count: core_count(),
        memory_bytes: memory_bytes(),
        kernel_version: command_output("uname", &["-r"]).unwrap_or_else(|| UNKNOWN.to_string()),
        os_version: os_version(),
        architecture: std::env::consts::ARCH.to_string(),
    }
}

fn var(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|value| !value.is_empty())
}

fn command_output(program: &str, args: &[&str]) -> Option<String> {
    let output = Command::new(program).args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!text.is_empty()).then_some(text)
}

fn core_count() -> u32 {
    std::thread::available_parallelism()
        .map(|count| count.get() as u32)
        .unwrap_or(0)
}

fn cpu_model() -> String {
    if cfg!(target_os = "macos") {
        return command_output("sysctl", &["-n", "machdep.cpu.brand_string"])
            .unwrap_or_else(|| UNKNOWN.to_string());
    }
    let Ok(cpuinfo) = std::fs::read_to_string("/proc/cpuinfo") else {
        return UNKNOWN.to_string();
    };
    cpuinfo
        .lines()
        .find_map(|line| line.strip_prefix("model name"))
        .and_then(|rest| rest.split_once(':'))
        .map(|(_, value)| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| UNKNOWN.to_string())
}

fn memory_bytes() -> u64 {
    if cfg!(target_os = "macos") {
        return command_output("sysctl", &["-n", "hw.memsize"])
            .and_then(|value| value.parse().ok())
            .unwrap_or(0);
    }
    let Ok(meminfo) = std::fs::read_to_string("/proc/meminfo") else {
        return 0;
    };
    // `MemTotal:       16072456 kB`
    meminfo
        .lines()
        .find_map(|line| line.strip_prefix("MemTotal:"))
        .and_then(|rest| rest.split_whitespace().next())
        .and_then(|value| value.parse::<u64>().ok())
        .map(|kilobytes| kilobytes.saturating_mul(1024))
        .unwrap_or(0)
}

fn os_version() -> String {
    if cfg!(target_os = "macos") {
        return command_output("sw_vers", &["-productVersion"])
            .map(|version| format!("macOS {version}"))
            .unwrap_or_else(|| UNKNOWN.to_string());
    }
    let Ok(release) = std::fs::read_to_string("/etc/os-release") else {
        return UNKNOWN.to_string();
    };
    release
        .lines()
        .find_map(|line| line.strip_prefix("PRETTY_NAME="))
        .map(|value| value.trim_matches('"').to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| UNKNOWN.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fingerprint_is_collectable_on_this_host() {
        let fingerprint = fingerprint();
        // Architecture is compiled in, so it is the one field that can never be
        // unknown. Everything else may legitimately degrade.
        assert!(!fingerprint.architecture.is_empty());
        assert!(!fingerprint.runner_label.is_empty());
        assert!(!fingerprint.runner_image.is_empty());
        assert!(!fingerprint.cpu_model.is_empty());
        assert!(!fingerprint.kernel_version.is_empty());
        assert!(!fingerprint.os_version.is_empty());
    }

    #[test]
    fn a_local_run_does_not_impersonate_a_hosted_runner() {
        // Only an explicit environment variable may claim a runner class. A
        // developer's laptop silently labelled `github-hosted` would violate the
        // epoch's environment policy invisibly.
        if var("RUE_BENCH_RUNNER_LABEL").is_none() {
            assert_eq!(fingerprint().runner_label, "local");
        }
    }

    #[test]
    fn the_fingerprint_identity_changes_with_the_environment() {
        let base = fingerprint();
        let mut drifted = base.clone();
        drifted.runner_image_version = "probe/99999999.9.0".to_string();
        assert_ne!(
            base.fingerprint_id().unwrap(),
            drifted.fingerprint_id().unwrap()
        );
    }

    #[test]
    fn probes_degrade_to_a_readable_placeholder_rather_than_failing() {
        // A missing program must yield `None`, which callers turn into
        // `unknown`, rather than propagating an error that would discard a
        // successful measurement.
        assert_eq!(
            command_output("definitely-not-a-real-program-xyz", &[]),
            None
        );
    }
}
