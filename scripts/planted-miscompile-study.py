#!/usr/bin/env python3
"""Reproduce the RUE-1816 planted-miscompile coverage study.

The lab never adds a planting switch to a production crate.  It compiles a
temporary source tree after applying one reviewed source patch, runs existing test
nets against that compiler, and writes every command/output plus a JSON ledger.
"""

import argparse
import datetime
import hashlib
import json
import os
from pathlib import Path
import platform
import signal
import shutil
import subprocess
import sys
import tempfile
from typing import Dict, List, Optional, Tuple


DEFECTS = ("RUE-348", "RUE-914", "RUE-1758")
PATCH_TARGETS = {
    "RUE-348": "crates/rue-cfg/src/opt/constfold.rs",
    "RUE-914": "crates/rue-cfg/src/opt/cse.rs",
    "RUE-1758": "crates/rue-codegen/src/terminator_plan.rs",
}
PATCH_PREIMAGE_SHA256 = {
    "RUE-348": "5ee3154a1c860db866743d1e8a7a59123ad153e3b4f97888f8fef8d0cf1eb6d0",
    "RUE-914": "29be4acc44becb5df841089d0bfd7af545cec69da08d08ded392f35df7bde2bd",
    "RUE-1758": "cdb907ba09b8f17243aa6671d249f4f72b24cec00ac33854d3b0e07a83ad13cf",
}
FOCUSED_CLI = {
    "RUE-348": "enum_payload_equality_across_opt_levels",
    "RUE-914": "param_raw_mut_write_reread_across_opt_levels",
    "RUE-1758": "inlined_continuation_lowering_order",
}
REPLAY_CASE = {
    "RUE-348": "cli.differential_opt::enum_payload_equality_across_opt_levels",
    "RUE-914": "cli.differential_opt::param_raw_mut_write_reread_across_opt_levels",
    "RUE-1758": (
        "cli.inlined_continuation_lowering_order::"
        "present_and_absent_keys_both_answer_correctly"
    ),
}
FOCUSED_SPEC = {
    "RUE-348": "enum_payload_equality",
    "RUE-914": "ptr_write_and_read",
    # No specification case is a backend-order test. The CLI replay below is
    # the source-semantics baseline and the native observation in one harness.
    "RUE-1758": None,
}

COMMAND_TIMEOUTS = {
    "build": 1800,
    "oracle-corpus": 3600,
    "oracle-fuzz": 1800,
    "cli": 1800,
    "spec": 900,
    "rue-fuzz": 600,
}
EXPECTED = {
    "RUE-348": {
        "oracle-o1": "caught",
        "oracle-o2": "caught",
        "oracle-o3": "caught",
        "oracle-fuzz-0-15": "missed",
        "cli-differential-opt": "caught",
        "cli-focused": "caught",
        "spec-focused-o0": "inapplicable",
        "rue-fuzz-x86-64-o1": "harness-gap",
    },
    "RUE-914": {
        "oracle-o1": "inapplicable",
        "oracle-o2": "caught",
        "oracle-o3": "caught",
        "oracle-fuzz-0-15": "missed",
        "cli-differential-opt": "caught",
        "cli-focused": "caught",
        "spec-focused-o0": "inapplicable",
        "rue-fuzz-x86-64-o1": "inapplicable",
    },
    "RUE-1758": {
        "oracle-o1": "inapplicable",
        "oracle-o2": "caught",
        "oracle-o3": "caught",
        "oracle-fuzz-0-15": "missed",
        "cli-differential-opt": "missed",
        "cli-focused": "caught",
        "spec-focused-o0": "inapplicable",
        "rue-fuzz-x86-64-o1": "inapplicable",
    },
}


def repo_root() -> Path:
    result = subprocess.run(
        ["git", "rev-parse", "--show-toplevel"],
        check=True,
        stdout=subprocess.PIPE,
        text=True,
        timeout=30,
    )
    return Path(result.stdout.strip()).resolve()


def patch_paths(root: Path) -> Dict[str, Path]:
    base = root / "crates/rue-planted-miscompiles/patches"
    return {defect: base / (defect + ".patch") for defect in DEFECTS}


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        while True:
            chunk = source.read(1024 * 1024)
            if not chunk:
                return digest.hexdigest()
            digest.update(chunk)


def verify_patch_shape(patch: Path, target: str) -> None:
    lines = patch.read_text(encoding="utf-8").splitlines()
    expected_diff = "diff --git a/{0} b/{0}".format(target)
    diff_headers = [line for line in lines if line.startswith("diff --git ")]
    old_paths = [line for line in lines if line.startswith("--- ")]
    new_paths = [line for line in lines if line.startswith("+++ ")]
    if diff_headers != [expected_diff]:
        raise RuntimeError(
            "{} must contain exactly one diff for {}; found {}".format(
                patch, target, diff_headers
            )
        )
    if old_paths != ["--- a/" + target] or new_paths != ["+++ b/" + target]:
        raise RuntimeError(
            "{} must modify {} in place; found old={} new={}".format(
                patch, target, old_paths, new_paths
            )
        )
    forbidden_metadata = (
        "new file mode ",
        "deleted file mode ",
        "rename from ",
        "rename to ",
        "copy from ",
        "copy to ",
        "GIT binary patch",
        "Binary files ",
    )
    if any(line.startswith(forbidden_metadata) for line in lines):
        raise RuntimeError("{} contains forbidden file-operation metadata".format(patch))


def verify_layout(root: Path) -> None:
    patches = patch_paths(root)
    for defect in DEFECTS:
        patch = patches[defect]
        if not patch.is_file():
            raise RuntimeError("missing planted-defect patch: {}".format(patch))
        target = PATCH_TARGETS[defect]
        verify_patch_shape(patch, target)
        target_path = root / target
        actual_preimage = sha256_file(target_path)
        expected_preimage = PATCH_PREIMAGE_SHA256[defect]
        if actual_preimage != expected_preimage:
            raise RuntimeError(
                "{} preimage drifted for {}: expected {}, found {}".format(
                    defect, target, expected_preimage, actual_preimage
                )
            )
        subprocess.run(
            ["git", "apply", "--check", str(patch)],
            cwd=root,
            check=True,
            timeout=30,
        )

    production_roots = [
        root / "crates/rue",
        root / "crates/rue-cfg",
        root / "crates/rue-codegen",
        root / "crates/rue-compiler",
    ]
    production_files = []
    for directory in production_roots:
        production_files.append(directory / "BUCK")
        production_files.extend(directory.rglob("*.rs"))
    forbidden = ("planted-miscompile", "RUE_PLANTED", "plant-defect")
    for path in production_files:
        text = path.read_text(encoding="utf-8")
        for marker in forbidden:
            if marker in text:
                raise RuntimeError(
                    "production build surface {} contains lab marker {!r}".format(
                        path.relative_to(root), marker
                    )
                )


def verify_study_source(root: Path) -> None:
    result = subprocess.run(
        ["git", "status", "--porcelain", "--untracked-files=all"],
        cwd=root,
        check=True,
        stdout=subprocess.PIPE,
        text=True,
        timeout=30,
    )
    if result.stdout:
        raise RuntimeError(
            "study measurements require a clean tracked commit; dirty paths:\n{}".format(
                result.stdout.rstrip()
            )
        )
    tracked = [Path(__file__).resolve()]
    tracked.extend(patch_paths(root).values())
    tracked.extend(
        root / "crates/rue-planted-miscompiles/repros" / (defect + ".rue")
        for defect in DEFECTS
    )
    subprocess.run(
        ["git", "ls-files", "--error-unmatch"]
        + [str(path.relative_to(root)) for path in tracked],
        cwd=root,
        check=True,
        stdout=subprocess.DEVNULL,
        timeout=30,
    )


def run_logged(
    command: List[str],
    cwd: Path,
    log: Path,
    env: Optional[Dict[str, str]] = None,
    timeout_seconds: float = 3600,
) -> Dict[str, object]:
    log.parent.mkdir(parents=True, exist_ok=True)
    merged_env = os.environ.copy()
    if env:
        merged_env.update(env)
    with log.open("w", encoding="utf-8") as output:
        output.write("cwd: {}\ncommand: {}\n\n".format(cwd, " ".join(command)))
        if env:
            output.write("selected environment:\n")
            for name, value in sorted(env.items()):
                output.write("  {}={}\n".format(name, value))
            output.write("\n")
        output.flush()
        process = subprocess.Popen(
            command,
            cwd=cwd,
            env=merged_env,
            stdout=output,
            stderr=subprocess.STDOUT,
            start_new_session=True,
        )
        try:
            status = process.wait(timeout=timeout_seconds)
        except subprocess.TimeoutExpired:
            try:
                os.killpg(process.pid, signal.SIGTERM)
            except ProcessLookupError:
                pass
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                try:
                    os.killpg(process.pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
                process.wait()
            output.write("\nTIMEOUT after {} seconds\n".format(timeout_seconds))
            output.flush()
            raise RuntimeError(
                "command timed out after {} seconds; see {}".format(
                    timeout_seconds, log
                )
            )
        output.write("\nexit status: {}\n".format(status))
    return {
        "command": command,
        "selected_environment": env or {},
        "exit_status": status,
        "timeout_seconds": timeout_seconds,
        "log": log.name,
        "log_sha256": sha256_file(log),
    }


def require_log_markers(text: str, markers: Tuple[str, ...], net: str) -> None:
    missing = [marker for marker in markers if marker not in text]
    if missing:
        raise RuntimeError(
            "{} did not emit required evidence markers: {}".format(net, missing)
        )


def classify(defect: str, net: str, status: int, log: Path) -> Tuple[str, str]:
    text = log.read_text(encoding="utf-8", errors="replace")
    expected = EXPECTED[defect][net]
    replay = REPLAY_CASE[defect]
    if expected == "caught":
        if status == 0:
            raise RuntimeError("{} unexpectedly passed instead of catching {}".format(net, defect))
        if net.startswith("oracle-"):
            level = net.rsplit("-", 1)[1].upper()
            require_log_markers(
                text,
                (
                    "HARNESS FAILURES: 0",
                    "FRONTEND FAILURES: 0",
                    "ORACLE FAILURES:   0",
                    "DISAGREEMENTS:",
                    "✗ rue-cli-tests {} [{}]".format(replay, level),
                    "optimized native observation:",
                ),
                net,
            )
        else:
            require_log_markers(
                text,
                (
                    "---- {} ----".format(replay),
                    "program stdout mismatch:",
                    "test result: FAILED.",
                ),
                net,
            )
        return "caught", "observable disagreement names the historical replay"

    if status != 0:
        raise RuntimeError(
            "{} failed without an expected planted-defect catch; see {}".format(net, log)
        )
    if net.startswith("oracle-") and net != "oracle-fuzz-0-15":
        require_log_markers(
            text,
            (
                "HARNESS FAILURES: 0",
                "FRONTEND FAILURES: 0",
                "ORACLE FAILURES:   0",
                "DISAGREEMENTS:    0",
            ),
            net,
        )
    elif net == "oracle-fuzz-0-15":
        require_log_markers(
            text,
            ("GENERATOR CONTRACT FAILURES: 0", "DISAGREEMENTS:    0"),
            net,
        )
    elif net.startswith("cli-") or net == "spec-focused-o0":
        require_log_markers(text, ("test result: ok.",), net)
    elif net == "rue-fuzz-x86-64-o1":
        require_log_markers(
            text,
            ("Fuzzing complete: runs: 1, crashes: 0",),
            net,
        )

    if expected in ("inapplicable", "harness-gap"):
        return expected, {
            "inapplicable": (
                "the registered endpoint does not activate the defect's "
                "optimization level"
            ),
            "harness-gap": "the endpoint asserts compile/ICE safety, not executable semantics",
        }[expected]
    return "missed", "bounded run completed without an observable disagreement"


def write_skipped_log(log: Path, reason: str) -> Dict[str, object]:
    log.write_text("not run: {}\n\nexit status: 0\n".format(reason), encoding="utf-8")
    return {
        "command": [],
        "selected_environment": {},
        "exit_status": 0,
        "timeout_seconds": 0,
        "log": log.name,
        "log_sha256": sha256_file(log),
    }


def expect_runtime_error(action, label: str) -> None:
    try:
        action()
    except RuntimeError:
        return
    raise RuntimeError("self-test expected RuntimeError: {}".format(label))


def run_self_tests() -> None:
    with tempfile.TemporaryDirectory(prefix="rue-1816-self-test-") as directory:
        scratch = Path(directory)

        cli_caught = scratch / "cli-caught.log"
        cli_caught.write_text(
            "---- {} ----\nprogram stdout mismatch:\ntest result: FAILED.\n".format(
                REPLAY_CASE["RUE-348"]
            ),
            encoding="utf-8",
        )
        observed, _ = classify("RUE-348", "cli-focused", 1, cli_caught)
        if observed != "caught":
            raise RuntimeError("self-test failed to classify the exact CLI replay")

        command_header_only = scratch / "command-header-only.log"
        command_header_only.write_text(
            "command: rue-cli-tests {}\nBUILD FAILED\n".format(
                FOCUSED_CLI["RUE-348"]
            ),
            encoding="utf-8",
        )
        expect_runtime_error(
            lambda: classify("RUE-348", "cli-focused", 1, command_header_only),
            "unrelated CLI build failure",
        )

        wrong_test = scratch / "wrong-test.log"
        wrong_test.write_text(
            "---- cli.other::case ----\nprogram stdout mismatch:\n"
            "test result: FAILED.\n",
            encoding="utf-8",
        )
        expect_runtime_error(
            lambda: classify("RUE-348", "cli-focused", 1, wrong_test),
            "wrong failing CLI test",
        )

        clean_miss = scratch / "clean-miss.log"
        clean_miss.write_text("test result: ok.\n", encoding="utf-8")
        observed, _ = classify(
            "RUE-1758", "cli-differential-opt", 0, clean_miss
        )
        if observed != "missed":
            raise RuntimeError("self-test failed to classify a clean bounded miss")

        oracle_caught = scratch / "oracle-caught.log"
        oracle_caught.write_text(
            "HARNESS FAILURES: 0\nFRONTEND FAILURES: 0\n"
            "ORACLE FAILURES:   0\nDISAGREEMENTS:    1\n"
            "✗ rue-cli-tests {} [O2]\noptimized native observation:\n".format(
                REPLAY_CASE["RUE-914"]
            ),
            encoding="utf-8",
        )
        observed, _ = classify("RUE-914", "oracle-o2", 1, oracle_caught)
        if observed != "caught":
            raise RuntimeError("self-test failed to classify the exact oracle replay")

        timeout_log = scratch / "timeout.log"
        expect_runtime_error(
            lambda: run_logged(
                [sys.executable, "-c", "import time; time.sleep(1)"],
                scratch,
                timeout_log,
                timeout_seconds=0.01,
            ),
            "outer command timeout",
        )
        if "TIMEOUT" not in timeout_log.read_text(encoding="utf-8"):
            raise RuntimeError("self-test timeout did not preserve evidence")


def oracle_command() -> List[str]:
    return ["./buck2", "run", "//crates/rue-oracle-diff:rue-oracle-diff", "--"]


def run_study(root: Path, defect: str, output: Path, keep_worktree: bool) -> None:
    verify_layout(root)
    verify_study_source(root)
    output.mkdir(parents=True, exist_ok=False)
    worktree_parent = Path(tempfile.mkdtemp(prefix="rue-1816-"))
    worktree = worktree_parent / "repo"
    archive = worktree_parent / "source.tar"
    patch = patch_paths(root)[defect]
    repro = root / "crates/rue-planted-miscompiles/repros" / (defect + ".rue")
    ledger = []
    try:
        # `git archive` gives the lab an immutable tracked-source snapshot and
        # does not register another checkout in the user's repository metadata.
        # That makes the runner usable from restricted CI/test sandboxes too.
        subprocess.run(
            ["git", "archive", "--format=tar", "-o", str(archive), "HEAD"],
            cwd=root,
            check=True,
            timeout=300,
        )
        worktree.mkdir()
        subprocess.run(
            ["tar", "-xf", str(archive), "-C", str(worktree)],
            check=True,
            timeout=300,
        )
        archived_patch = worktree / patch.relative_to(root)
        archived_repro = worktree / repro.relative_to(root)
        subprocess.run(
            ["git", "apply", str(archived_patch)],
            cwd=worktree,
            check=True,
            timeout=30,
        )
        build_log = output / "build.log"
        build_evidence = run_logged(
            ["scripts/rue", "build"],
            worktree,
            build_log,
            timeout_seconds=COMMAND_TIMEOUTS["build"],
        )
        status = int(build_evidence["exit_status"])
        if status != 0:
            raise RuntimeError("planted compiler build failed; see {}".format(build_log))
        compiler = subprocess.run(
            ["scripts/rue-bin"],
            cwd=worktree,
            check=True,
            stdout=subprocess.PIPE,
            text=True,
            timeout=60,
        ).stdout.strip()
        compiler_path = Path(compiler)

        inputs = output / "inputs"
        inputs.mkdir()
        copied_runner = inputs / "planted-miscompile-study.py"
        copied_patch = inputs / patch.name
        copied_repro = inputs / repro.name
        shutil.copy2(Path(__file__).resolve(), copied_runner)
        shutil.copy2(archived_patch, copied_patch)
        shutil.copy2(archived_repro, copied_repro)

        oracle_env = {
            "RUE_BINARY": compiler,
            "RUE_ORACLE_DIFF_CASES": str(worktree / "crates/rue-cli-tests/cases"),
            "RUE_ORACLE_DIFF_EXECUTION_CONTRACTS": str(
                worktree / "crates/rue-cli-tests/cases/execution_contracts.toml"
            ),
            "RUE_ORACLE_DIFF_STD": str(worktree / "std"),
        }
        for level in ("O1", "O2", "O3"):
            net = "oracle-{}".format(level.lower())
            log = output / (net + ".log")
            env = dict(oracle_env)
            env["RUE_ORACLE_DIFF_OPT_LEVEL"] = level
            evidence = run_logged(
                oracle_command(),
                worktree,
                log,
                env,
                timeout_seconds=COMMAND_TIMEOUTS["oracle-corpus"],
            )
            observed, reason = classify(
                defect, net, int(evidence["exit_status"]), log
            )
            ledger.append(
                {"net": net, "observed": observed, "reason": reason, **evidence}
            )

        fuzz_net = "oracle-fuzz-0-15"
        fuzz_log = output / (fuzz_net + ".log")
        fuzz_command = oracle_command() + [
            "fuzz",
            "--start",
            "0",
            "--seeds",
            "16",
            "--timeout",
            "10",
            "--crash-dir",
            str(output / "oracle-fuzz-findings"),
        ]
        evidence = run_logged(
            fuzz_command,
            worktree,
            fuzz_log,
            oracle_env,
            timeout_seconds=COMMAND_TIMEOUTS["oracle-fuzz"],
        )
        observed, reason = classify(
            defect, fuzz_net, int(evidence["exit_status"]), fuzz_log
        )
        ledger.append(
            {"net": fuzz_net, "observed": observed, "reason": reason, **evidence}
        )

        for net, filter_text in (
            ("cli-differential-opt", "differential_opt"),
            ("cli-focused", FOCUSED_CLI[defect]),
        ):
            log = output / (net + ".log")
            evidence = run_logged(
                [
                    "./buck2",
                    "run",
                    "//crates/rue-cli-tests:cli",
                    "--",
                    "--quiet",
                    filter_text,
                ],
                worktree,
                log,
                timeout_seconds=COMMAND_TIMEOUTS["cli"],
            )
            observed, reason = classify(
                defect, net, int(evidence["exit_status"]), log
            )
            ledger.append(
                {"net": net, "observed": observed, "reason": reason, **evidence}
            )

        spec_net = "spec-focused-o0"
        spec_log = output / (spec_net + ".log")
        spec_filter = FOCUSED_SPEC[defect]
        if spec_filter is None:
            evidence = write_skipped_log(
                spec_log,
                "no specification endpoint maps to this backend lowering-order defect",
            )
            observed = "inapplicable"
            reason = "no relevant specification endpoint exists for this backend-order defect"
        else:
            evidence = run_logged(
                [
                    "./buck2",
                    "run",
                    "//crates/rue-spec:spec",
                    "--",
                    "--quiet",
                    spec_filter,
                ],
                worktree,
                spec_log,
                timeout_seconds=COMMAND_TIMEOUTS["spec"],
            )
            observed, reason = classify(
                defect, spec_net, int(evidence["exit_status"]), spec_log
            )
        ledger.append(
            {"net": spec_net, "observed": observed, "reason": reason, **evidence}
        )

        fuzz_input = output / "rue-fuzz-input"
        fuzz_input.mkdir()
        shutil.copy2(archived_repro, fuzz_input)
        rue_fuzz_net = "rue-fuzz-x86-64-o1"
        rue_fuzz_log = output / (rue_fuzz_net + ".log")
        evidence = run_logged(
            [
                "./buck2",
                "run",
                "//crates/rue-fuzz:rue-fuzz",
                "--",
                "--max-runs=1",
                "compiler_x86_64_o1",
                str(fuzz_input),
            ],
            worktree,
            rue_fuzz_log,
            timeout_seconds=COMMAND_TIMEOUTS["rue-fuzz"],
        )
        status = int(evidence["exit_status"])
        if status != 0:
            raise RuntimeError(
                "rue-fuzz harness failed; see {}".format(rue_fuzz_log)
            )
        observed, reason = classify(defect, rue_fuzz_net, status, rue_fuzz_log)
        ledger.append(
            {
                "net": rue_fuzz_net,
                "observed": observed,
                "reason": reason,
                **evidence,
            }
        )

        base = subprocess.run(
            ["git", "rev-parse", "HEAD"],
            cwd=root,
            check=True,
            stdout=subprocess.PIPE,
            text=True,
            timeout=30,
        ).stdout.strip()
        result = {
            "schema": 2,
            "defect": defect,
            "base_commit": base,
            "platform": {
                "system": platform.system(),
                "release": platform.release(),
                "machine": platform.machine(),
                "python": platform.python_version(),
            },
            "inputs": {
                "patch": {
                    "path": str(copied_patch.relative_to(output)),
                    "sha256": sha256_file(copied_patch),
                },
                "repro": {
                    "path": str(copied_repro.relative_to(output)),
                    "sha256": sha256_file(copied_repro),
                },
                "runner": {
                    "path": str(copied_runner.relative_to(output)),
                    "sha256": sha256_file(copied_runner),
                },
            },
            "compiler_sha256": sha256_file(compiler_path),
            "build": build_evidence,
            "oracle_fuzz_seeds": {"start": 0, "count": 16},
            "results": ledger,
        }
        (output / "ledger.json").write_text(
            json.dumps(result, indent=2, sort_keys=True) + "\n", encoding="utf-8"
        )

        mismatches = []
        for row in ledger:
            expected = EXPECTED[defect][row["net"]]
            if row["observed"] != expected:
                mismatches.append(
                    "{}: expected {}, observed {}".format(
                        row["net"], expected, row["observed"]
                    )
                )
        if mismatches:
            raise RuntimeError("unexpected study results:\n  " + "\n  ".join(mismatches))

        manifest_entries = []
        for path in sorted(output.rglob("*")):
            if path.is_file() and path.name != "SHA256SUMS":
                manifest_entries.append(
                    "{}  {}".format(sha256_file(path), path.relative_to(output))
                )
        (output / "SHA256SUMS").write_text(
            "\n".join(manifest_entries) + "\n", encoding="utf-8"
        )
    finally:
        if keep_worktree:
            print("kept planted source tree at {}".format(worktree), file=sys.stderr)
        elif worktree_parent.exists():
            shutil.rmtree(str(worktree_parent), ignore_errors=True)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--verify-layout", action="store_true")
    parser.add_argument("--self-test", action="store_true")
    parser.add_argument("--defect", choices=DEFECTS)
    parser.add_argument("--output", type=Path)
    parser.add_argument("--keep-worktree", action="store_true")
    args = parser.parse_args()
    root = repo_root()
    checked = False
    if args.verify_layout:
        verify_layout(root)
        print("planted-miscompile lab is isolated from production targets")
        checked = True
    if args.self_test:
        run_self_tests()
        print("planted-miscompile runner self-tests passed")
        checked = True
    if checked and not args.defect:
        return 0
    if not args.defect:
        parser.error("--defect is required unless --verify-layout is used")
    if args.output:
        output = args.output.resolve()
    else:
        stamp = datetime.datetime.utcnow().strftime("%Y%m%dT%H%M%SZ")
        output = Path(tempfile.gettempdir()) / "rue-1816-{}-{}".format(args.defect, stamp)
    run_study(root, args.defect, output, args.keep_worktree)
    print(output)
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (RuntimeError, subprocess.CalledProcessError) as error:
        print("error: {}".format(error), file=sys.stderr)
        sys.exit(1)
