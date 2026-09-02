#!/usr/bin/env python3
"""Focused tests for the repo-owned rustc unused-dependency wrapper."""

from __future__ import annotations

import ast
import json
import os
import subprocess
import sys
import tempfile
import time
import unittest
from pathlib import Path
from unittest.mock import patch

from gatelib import load_script

WRAPPER = load_script("rustc-first-party-unused-deps.py", __file__)

EXPECTED_TOOLCHAINS = {
    ("prelude//os:linux", "prelude//cpu:arm64"): "toolchains//rust:hermetic-linux-aarch64",
    ("prelude//os:linux", "prelude//cpu:x86_64"): "toolchains//rust:hermetic-linux-x86_64",
    ("prelude//os:macos", "prelude//cpu:arm64"): "toolchains//rust:hermetic-macos-aarch64",
    ("prelude//os:macos", "prelude//cpu:x86_64"): "toolchains//rust:hermetic-macos-x86_64",
}


def calls_named(source: str, name: str):
    tree = ast.parse(source)
    return [
        node
        for node in ast.walk(tree)
        if isinstance(node, ast.Call)
        and isinstance(node.func, ast.Name)
        and node.func.id == name
    ]


def string_keyword(call, name: str) -> str:
    for keyword in call.keywords:
        if keyword.arg == name and isinstance(keyword.value, ast.Constant):
            if isinstance(keyword.value.value, str):
                return keyword.value.value
    raise ValueError(f"{name} must be a literal string")


def rust_alias_mapping(source: str):
    aliases = [
        call
        for call in calls_named(source, "toolchain_alias")
        if string_keyword(call, "name") == "rust"
    ]
    if len(aliases) != 1:
        raise ValueError("expected exactly one rust toolchain_alias")
    actual = next(
        (keyword.value for keyword in aliases[0].keywords if keyword.arg == "actual"), None
    )
    if not (
        isinstance(actual, ast.Call)
        and isinstance(actual.func, ast.Name)
        and actual.func.id == "select"
        and len(actual.args) == 1
        and isinstance(actual.args[0], ast.Dict)
    ):
        raise ValueError("rust toolchain_alias actual must be a nested select")
    result = {}
    for os_node, cpu_select in zip(actual.args[0].keys, actual.args[0].values):
        if not isinstance(os_node, ast.Constant) or not isinstance(os_node.value, str):
            raise ValueError("rust toolchain OS key must be a literal string")
        if not (
            isinstance(cpu_select, ast.Call)
            and isinstance(cpu_select.func, ast.Name)
            and cpu_select.func.id == "select"
            and len(cpu_select.args) == 1
            and isinstance(cpu_select.args[0], ast.Dict)
        ):
            raise ValueError("rust toolchain CPU mapping must be a nested select")
        for cpu_node, target_node in zip(cpu_select.args[0].keys, cpu_select.args[0].values):
            if not all(
                isinstance(node, ast.Constant) and isinstance(node.value, str)
                for node in (cpu_node, target_node)
            ):
                raise ValueError("rust toolchain CPU and target must be literal strings")
            result[(os_node.value, cpu_node.value)] = target_node.value
    return result


def declared_audited_toolchains(source: str):
    result = set()
    for call in calls_named(source, "hermetic_rust_toolchain"):
        name = string_keyword(call, "name")
        enabled = next(
            (keyword.value for keyword in call.keywords if keyword.arg == "report_unused_deps"),
            None,
        )
        if isinstance(enabled, ast.Constant) and enabled.value is True:
            result.add("toolchains//rust:" + name)
    return result


def validate_rule_forwarding(source: str) -> None:
    forwarding = "report_unused_deps = ctx.attrs.report_unused_deps,"
    attribute = '"report_unused_deps": attrs.bool(default = False),'
    if source.count(forwarding) != 1:
        raise ValueError("RustToolchainInfo must forward report_unused_deps exactly once")
    if source.count(attribute) != 1:
        raise ValueError("hermetic toolchain must declare report_unused_deps exactly once")


class WrapperTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        self.fake = self.root / "fake-compiler.py"
        self.fake.write_text(
            """#!/usr/bin/env python3
import json, os, sys
from pathlib import Path
path = os.environ.get("FAKE_ARGS")
if path:
    open(path, "w").write(json.dumps(sys.argv[1:]))
for arg in sys.argv[1:]:
    if arg.startswith("--emit=metadata="):
        Path(arg[len("--emit=metadata="):]).write_text("artifact")
raw = os.environ.get("FAKE_UNUSED", "")
if raw:
    print(json.dumps({"unused_extern_names": raw.split(","), "lint_level": "warn"}), file=sys.stderr)
extra = os.environ.get("FAKE_STDERR")
if extra:
    print(extra, file=sys.stderr)
print("compiler stdout")
if os.environ.get("FAKE_WAIT_READY"):
    import signal
    ready = Path(os.environ["FAKE_WAIT_READY"])
    received = Path(os.environ["FAKE_WAIT_RECEIVED"])
    def stop(signum, _frame):
        received.write_text(str(signum))
        raise SystemExit(0)
    signal.signal(signal.SIGTERM, stop)
    signal.signal(signal.SIGINT, stop)
    signal.signal(signal.SIGHUP, stop)
    ready.write_text(str(os.getpid()))
    while True:
        signal.pause()
if os.environ.get("FAKE_SIGNAL"):
    os.kill(os.getpid(), int(os.environ["FAKE_SIGNAL"]))
raise SystemExit(int(os.environ.get("FAKE_EXIT", "0")))
"""
        )
        self.fake.chmod(0o755)
        self.args_file = self.root / "args.json"

    def tearDown(self) -> None:
        self.temp.cleanup()

    @staticmethod
    def artifact(package: str, target: str) -> str:
        return f"buck-out/v2/art/root/{package}/__{target}__/hash/LPPM/lib.rmeta"

    def invoke(
        self,
        unused: str,
        externs,
        consumer: str = "root//crates/app:app",
        output: Path = None,
        extra_args=(),
        **env,
    ):
        argv = [
            sys.executable,
            str(Path(__file__).with_name("rustc-first-party-unused-deps.py")),
            str(self.fake),
            "-Dwarnings",
            "-Wunused-crate-dependencies",
            f"-Cmetadata={consumer}#configured-hash",
        ]
        if output is not None:
            argv.append("--emit=metadata=" + str(output))
        argv.extend(extra_args)
        argv.extend(f"--extern={name}={artifact}" for name, artifact in externs.items())
        process_env = os.environ.copy()
        process_env.update({"FAKE_UNUSED": unused, "FAKE_ARGS": str(self.args_file)})
        process_env.update(env)
        return subprocess.run(argv, text=True, capture_output=True, env=process_env)

    def test_unused_first_party_dependency_is_rejected_from_real_wrapper_process(self):
        result = self.invoke(
            "dep",
            {"dep": self.artifact("crates/dep", "dep")},
        )
        self.assertEqual(result.returncode, 1)
        records = [json.loads(line) for line in result.stderr.splitlines()]
        diagnostic = records[0]
        self.assertEqual(diagnostic["unused_extern_names"], ["dep"])
        self.assertEqual(diagnostic["lint_level"], "deny")
        self.assertEqual(records[1]["level"], "error")
        self.assertIn("root//crates/app:app -> root//crates/dep:dep", records[1]["message"])
        args = json.loads(self.args_file.read_text())
        self.assertIn("--force-warn=unused-crate-dependencies", args)
        self.assertNotIn("-Wunused-crate-dependencies", args)
        self.assertEqual(result.stdout, "compiler stdout\n")

    def test_unused_third_party_dependency_is_not_rejected(self):
        result = self.invoke(
            "serde",
            {"serde": self.artifact("third-party", "serde-1.0.0")},
        )
        self.assertEqual(result.returncode, 0)

        test_action = self.invoke(
            "dep",
            {"dep": self.artifact("crates/test-support", "test-support")},
            consumer="root//crates/app:app-test",
            extra_args=("--test",),
        )
        self.assertEqual(test_action.returncode, 0)
        self.assertEqual(result.stderr, "")

    def test_policy_failure_removes_rustc_output_but_third_party_finding_does_not(self):
        output = self.root / "artifact.rmeta"
        rejected = self.invoke(
            "dep", {"dep": self.artifact("crates/dep", "dep")}, output=output
        )
        self.assertEqual(rejected.returncode, 1)
        self.assertFalse(output.exists())

        accepted = self.invoke(
            "serde", {"serde": self.artifact("third-party", "serde")}, output=output
        )
        self.assertEqual(accepted.returncode, 0)
        self.assertEqual(output.read_text(), "artifact")

    def test_mixed_notification_retains_only_first_party_names_deterministically(self):
        result = self.invoke(
            "serde,dep",
            {
                "serde": self.artifact("third-party", "serde-1.0.0"),
                "dep": self.artifact("crates/dep", "dep"),
            },
        )
        self.assertEqual(result.returncode, 1)
        self.assertEqual(json.loads(result.stderr.splitlines()[0])["unused_extern_names"], ["dep"])

    def test_exact_baseline_edge_is_suppressed_but_neighbor_is_not(self):
        result = self.invoke(
            "rue_span,other",
            {
                "rue_span": self.artifact("crates/rue-span", "rue-span"),
                "other": self.artifact("crates/other", "other"),
            },
            consumer="root//crates/rue-codegen:rue-codegen",
        )
        self.assertEqual(result.returncode, 1)
        self.assertEqual(json.loads(result.stderr.splitlines()[0])["unused_extern_names"], ["other"])

    def test_manual_target_and_renamed_extern_use_concrete_artifact_owner(self):
        result = self.invoke(
            "wire_name",
            {"wire_name": self.artifact("crates/manual", "manual-support")},
        )
        self.assertEqual(result.returncode, 1)
        self.assertIn("wire_name", result.stderr)
        self.assertEqual(
            WRAPPER.classify_owner(self.artifact("crates/manual", "_internal_")),
            (WRAPPER.Ownership.FIRST_PARTY, "root//crates/manual:_internal_"),
        )

    def test_artifact_ownership_is_explicit_and_supports_buck_layouts(self):
        cases = {
            self.artifact("crates/generated/nested", "macro-provider"): (
                WRAPPER.Ownership.FIRST_PARTY,
                "root//crates/generated/nested:macro-provider",
            ),
            "/remote/worker/root/buck-out/v2/art/root/crates/dep/__dep__/hash/lib.rmeta": (
                WRAPPER.Ownership.FIRST_PARTY,
                "root//crates/dep:dep",
            ),
            "buck-out/v2/art/root/0123456789abcdef/crates/dep/__dep__/lib.rmeta": (
                WRAPPER.Ownership.FIRST_PARTY,
                "root//crates/dep:dep",
            ),
            self.artifact("third-party", "serde"): (
                WRAPPER.Ownership.KNOWN_NON_FIRST_PARTY,
                None,
            ),
            "buck-out/v2/art/toolchains/hash/rust/__rustc__/rustc": (
                WRAPPER.Ownership.KNOWN_NON_FIRST_PARTY,
                None,
            ),
            "buck-out/v2/art/external-cell/pkg/__dep__/lib.rmeta": (
                WRAPPER.Ownership.KNOWN_NON_FIRST_PARTY,
                None,
            ),
            "buck-out/v2/art/root/generated/__dep__/lib.rmeta": (
                WRAPPER.Ownership.UNKNOWN,
                None,
            ),
        }
        for artifact, expected in cases.items():
            with self.subTest(artifact=artifact):
                self.assertEqual(WRAPPER.classify_owner(artifact), expected)

    def test_unknown_artifact_ownership_fails_closed_only_for_audited_consumer(self):
        unknown = "buck-out/v2/art/root/generated/__dep__/lib.rmeta"
        result = self.invoke("dep", {"dep": unknown})
        self.assertEqual(result.returncode, 2)
        self.assertIn("unknown Buck artifact ownership", result.stderr)
        test_action = self.invoke(
            "dep", {"dep": unknown}, extra_args=("--test",)
        )
        self.assertEqual(test_action.returncode, 0)

    def test_relative_select_and_alias_spelling_do_not_affect_configured_identity(self):
        diagnostic = {"unused_extern_names": ["renamed"], "lint_level": "warn"}
        filtered, findings = WRAPPER.filter_unused(
            diagnostic,
            "root//crates/app:app",
            {"renamed": self.artifact("crates/dep", "concrete")},
        )
        self.assertIsNotNone(filtered)
        self.assertEqual(findings, [("root//crates/app:app", "root//crates/dep:concrete")])

    def test_test_only_and_non_crate_consumers_are_outside_production_policy(self):
        diagnostic = {"unused_extern_names": ["dep"], "lint_level": "warn"}
        filtered, findings = WRAPPER.filter_unused(
            diagnostic,
            "root//crates/app:app-test",
            {"dep": self.artifact("crates/test-support", "test-support")},
            audit_consumer=False,
        )
        self.assertIsNone(filtered)
        self.assertEqual(findings, [])

        result = self.invoke(
            "dep",
            {"dep": self.artifact("crates/dep", "dep")},
            consumer="root//third-party:build-script",
        )
        self.assertEqual(result.returncode, 0)

    def test_non_policy_stderr_and_compiler_exit_are_preserved(self):
        result = self.invoke("", {}, FAKE_STDERR="ordinary compiler error", FAKE_EXIT="7")
        self.assertEqual(result.returncode, 7)
        self.assertEqual(result.stderr, "ordinary compiler error\n")

    def test_compiler_signal_is_relayed(self):
        result = self.invoke("", {}, FAKE_SIGNAL="15")
        self.assertEqual(result.returncode, -15)

    def test_wrapper_termination_reaches_live_compiler_and_is_relayed(self):
        ready = self.root / "ready"
        received = self.root / "received"
        env = os.environ.copy()
        env.update(
            {
                "FAKE_WAIT_READY": str(ready),
                "FAKE_WAIT_RECEIVED": str(received),
            }
        )
        process = subprocess.Popen(
            [
                sys.executable,
                str(Path(__file__).with_name("rustc-first-party-unused-deps.py")),
                str(self.fake),
                "-Cmetadata=root//crates/app:app#configured-hash",
            ],
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            env=env,
        )
        for _ in range(100):
            if ready.exists():
                break
            time.sleep(0.02)
        self.assertTrue(ready.exists())
        process.terminate()
        process.communicate(timeout=5)
        self.assertEqual(process.returncode, -15)
        self.assertEqual(received.read_text(), "15")

    def test_missing_configured_consumer_fails_closed_for_first_party_only(self):
        result = self.invoke(
            "dep",
            {"dep": self.artifact("crates/dep", "dep")},
            consumer="not-a-buck-target",
        )
        self.assertEqual(result.returncode, 2)
        self.assertIn("no unique configured consumer", result.stderr)

    def test_ambiguous_extern_artifacts_fail_closed(self):
        with self.assertRaisesRegex(ValueError, "ambiguous"):
            WRAPPER.extern_artifacts(
                [
                    "--extern=dep=one",
                    "--extern",
                    "dep=two",
                ]
            )

    def test_response_file_is_inspected_and_rewritten_without_flattening(self):
        response = self.root / "rustc.args"
        response.write_text(
            "\n".join(
                [
                    "-Dwarnings",
                    "-Wunused-crate-dependencies",
                    "-Cmetadata=root//crates/app:app#configured-hash",
                    "--extern=dep=" + self.artifact("crates/dep", "dep"),
                ]
            )
            + "\n"
        )
        process_env = os.environ.copy()
        process_env.update({"FAKE_UNUSED": "dep", "FAKE_ARGS": str(self.args_file)})
        result = subprocess.run(
            [
                sys.executable,
                str(Path(__file__).with_name("rustc-first-party-unused-deps.py")),
                str(self.fake),
                "@" + str(response),
            ],
            text=True,
            capture_output=True,
            env=process_env,
        )
        self.assertEqual(result.returncode, 1)
        forwarded = json.loads(self.args_file.read_text())
        self.assertEqual(len(forwarded), 1)
        self.assertTrue(forwarded[0].startswith("@"))
        self.assertNotEqual(forwarded[0][1:], str(response))
        self.assertFalse(Path(forwarded[0][1:]).exists())
        self.assertIn("dep", result.stderr)

    def test_response_file_replaces_lint_before_invoking_compiler(self):
        response = self.root / "rustc.args"
        response.write_text("-Dwarnings\n-Wunused-crate-dependencies\n")
        args, temporary = WRAPPER.invocation_args(["@" + str(response)])
        try:
            self.assertEqual(len(args), 1)
            rewritten = Path(args[0][1:]).read_text().splitlines()
            self.assertEqual(rewritten, ["-Dwarnings", WRAPPER.FORCE_WARN])
        finally:
            for path in temporary:
                path.unlink()

    def test_output_parsing_covers_multiple_emit_and_output_forms(self):
        self.assertEqual(
            WRAPPER.compiler_output_paths(
                [
                    "--emit=metadata=one.rmeta,dep-info=one.d",
                    "--emit",
                    "link=one-bin",
                    "-o=two-bin",
                    "-o",
                    "three-bin",
                ]
            ),
            [
                Path("one.rmeta"),
                Path("one.d"),
                Path("one-bin"),
                Path("two-bin"),
                Path("three-bin"),
            ],
        )

    def test_output_cleanup_attempts_every_path_before_reporting(self):
        first = self.root / "first"
        second = self.root / "second"
        first.write_text("first")
        second.write_text("second")
        real_unlink = Path.unlink

        def selective_unlink(path, *args, **kwargs):
            if path == first:
                raise PermissionError("fixture refusal")
            return real_unlink(path, *args, **kwargs)

        with patch.object(Path, "unlink", selective_unlink):
            errors = WRAPPER.remove_compiler_outputs([first, second])
        self.assertEqual(len(errors), 1)
        self.assertIn("first", errors[0])
        self.assertTrue(first.exists())
        self.assertFalse(second.exists())

    def test_baseline_entries_are_exact_unique_and_reasoned(self):
        self.assertEqual(len(WRAPPER.BASELINE_ENTRIES), 8)
        self.assertEqual(len(WRAPPER.BASELINE), len(WRAPPER.BASELINE_ENTRIES))
        for entry in WRAPPER.BASELINE_ENTRIES:
            self.assertTrue(entry.consumer.startswith("root//crates/"))
            self.assertTrue(entry.dependency.startswith("root//crates/"))
            self.assertGreaterEqual(len(entry.reason.split()), 6)


class ToolchainRegistrationTests(unittest.TestCase):
    def test_every_supported_registration_uses_an_audited_toolchain(self):
        registration = Path(os.environ["RUE_RUST_TOOLCHAIN_REGISTRATION"]).read_text()
        declarations = Path(os.environ["RUE_RUST_TOOLCHAIN_DECLARATIONS"]).read_text()
        mapping = rust_alias_mapping(registration)
        self.assertEqual(mapping, EXPECTED_TOOLCHAINS)
        self.assertEqual(declared_audited_toolchains(declarations), set(mapping.values()))

    def test_compiler_and_clippy_are_both_wrapped_by_the_policy_tool(self):
        source = Path(os.environ["RUE_RUST_TOOLCHAIN_RULE"]).read_text()
        validate_rule_forwarding(source)
        self.assertIn("compiler = RunInfo(args = cmd_args(\n        unused_deps_wrapper,\n        rustc_bin,", source)
        self.assertIn('unused_deps_wrapper = ctx.attrs.unused_deps_wrapper[DefaultInfo].default_outputs[0]', source)
        self.assertIn('                "exec python3 ",', source)
        self.assertIn('                " \\\"$@\\\"",\n                delimiter = "",', source)
        self.assertIn('default = "root//:rustc-first-party-unused-deps-wrapper"', source)

    def test_report_unused_deps_forwarding_and_attribute_fail_closed_on_mutation(self):
        source = Path(os.environ["RUE_RUST_TOOLCHAIN_RULE"]).read_text()
        mutations = (
            source.replace(
                "report_unused_deps = ctx.attrs.report_unused_deps,", "", 1
            ),
            source.replace(
                "report_unused_deps = ctx.attrs.report_unused_deps,",
                "report_unused_deps = False,",
                1,
            ),
            source.replace('"report_unused_deps": attrs.bool(default = False),', "", 1),
            source.replace(
                '"report_unused_deps": attrs.bool(default = False),',
                '"report_unused_deps": attrs.string(default = "false"),',
                1,
            ),
        )
        for mutation in mutations:
            with self.subTest():
                with self.assertRaises(ValueError):
                    validate_rule_forwarding(mutation)

    def test_future_or_unregistered_toolchain_cannot_be_silently_omitted(self):
        declarations = Path(os.environ["RUE_RUST_TOOLCHAIN_DECLARATIONS"]).read_text()
        incomplete = declarations.replace(
            '    report_unused_deps = True,\n    deny_lints = ["warnings"],',
            '    report_unused_deps = False,\n    deny_lints = ["warnings"],',
            1,
        )
        self.assertNotEqual(
            declared_audited_toolchains(incomplete), set(EXPECTED_TOOLCHAINS.values())
        )

        registration = Path(os.environ["RUE_RUST_TOOLCHAIN_REGISTRATION"]).read_text()
        future = registration.replace(
            '"prelude//cpu:x86_64": "toolchains//rust:hermetic-linux-x86_64",',
            '"prelude//cpu:x86_64": "toolchains//rust:future-linux-x86_64",',
            1,
        )
        self.assertNotEqual(rust_alias_mapping(future), EXPECTED_TOOLCHAINS)


if __name__ == "__main__":
    unittest.main()
