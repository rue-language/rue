#!/usr/bin/env python3

import importlib.util
import json
import os
from pathlib import Path
import tempfile
import unittest


SCRIPT = Path(__file__).with_name("check-reproducible-build-metadata.py")
SPEC = importlib.util.spec_from_file_location("repro_metadata", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
repro = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(repro)


def ar_member(name, payload, mtime="0", uid="0", gid="0", mode="100644"):
    name_field = (name + "/").encode().ljust(16)
    header = (
        name_field
        + mtime.encode().ljust(12)
        + uid.encode().ljust(6)
        + gid.encode().ljust(6)
        + mode.encode().ljust(8)
        + str(len(payload)).encode().ljust(10)
        + b"`\n"
    )
    return header + payload + (b"\n" if len(payload) % 2 else b"")


def records_for(first_bytes, second_bytes, replacements=()):
    temporary = tempfile.TemporaryDirectory()
    root = Path(temporary.name)
    first = root / "first.rlib"
    second = root / "second.rlib"
    first.write_bytes(first_bytes)
    second.write_bytes(second_bytes)
    return (
        temporary,
        repro.file_record("archive", "rust-library", "root//x:lib", first, replacements),
        repro.file_record("archive", "rust-library", "root//x:lib", second, replacements),
    )


class ReproducibleBuildMetadataTests(unittest.TestCase):
    def test_graph_contracts_are_exact_and_deduplicated(self):
        graph = {
            "root//x:lib (cfg#1)": {"buck.type": "rust_library", "name": "lib", "proc_macro": False},
            "root//x:lib (cfg#2)": {"buck.type": "rust_library", "name": "lib", "proc_macro": False},
            "root//x:derive (cfg#1)": {"buck.type": "rust_library", "name": "derive", "proc_macro": True},
            "root//x:build (cfg#1)": {"buck.type": "rust_binary", "name": "build", "crate": "build_script_build"},
            "root//x:run (cfg#1)": {"buck.type": "_cargo_buildscript_rule", "name": "run"},
            "root//x:generated (cfg#1)": {"buck.type": "genrule", "name": "generated"},
            "root//x:ordinary-bin (cfg#1)": {"buck.type": "rust_binary", "name": "ordinary-bin"},
            "toolchains//:rust (cfg#1)": {"buck.type": "rust_toolchain", "name": "rust"},
        }
        contracts = repro.graph_contracts(graph)
        self.assertEqual(
            [item["category"] for item in contracts],
            ["build-script-executable", "proc-macro", "generated-output", "rust-library", "build-script-output"],
        )
        library = next(item for item in contracts if item["name"] == "lib")
        self.assertEqual(
            library["configured_labels"],
            [
                {"hash": "1", "label": "root//x:lib (cfg#1)"},
                {"hash": "2", "label": "root//x:lib (cfg#2)"},
            ],
        )

    def test_unrecognized_eligible_root_label_fails_closed(self):
        with self.assertRaisesRegex(repro.DiagnosticError, "cannot parse eligible"):
            repro.graph_contracts(
                {"root//malformed": {"buck.type": "rust_library", "name": "x"}}
            )

    def test_metadata_materialization_is_limited_to_reachable_rust_libraries(self):
        contracts = [
            {"label": "root//x:lib", "category": "rust-library"},
            {"label": "root//x:lib", "category": "rust-library"},
            {"label": "root//x:derive", "category": "proc-macro"},
            {"label": "root//x:generated", "category": "generated-output"},
        ]
        self.assertEqual(
            repro.metadata_check_targets(contracts),
            ["root//x:lib[check]"],
        )

    def test_ar_parser_preserves_member_order_metadata_and_payload(self):
        archive = b"!<arch>\n" + ar_member("one.o", b"abc", mtime="12") + ar_member("one.o", b"defg", mode="100755")
        members = repro.parse_ar(archive)
        self.assertEqual([(m["name"], m["occurrence"]) for m in members], [("one.o", 0), ("one.o", 1)])
        self.assertEqual(members[0]["mtime"], "12")
        self.assertEqual(members[1]["mode"], "100755")
        self.assertEqual(members[1]["payload"], b"defg")

    def test_comparison_separates_mtime_path_and_archive_metadata(self):
        first_root = b"/tmp/a"
        second_root = b"/tmp/much-longer-b"
        replacements = (
            (first_root, b"<SOURCE_ROOT>"),
            (second_root, b"<SOURCE_ROOT>"),
        )
        temporary, base, other = records_for(
            b"!<arch>\n" + ar_member("a.o", first_root, mtime="0"),
            b"!<arch>\n" + ar_member("a.o", second_root, mtime="3"),
            replacements,
        )
        self.addCleanup(temporary.cleanup)
        os.utime(Path(temporary.name) / "first.rlib", ns=(1, 1))
        os.utime(Path(temporary.name) / "second.rlib", ns=(2, 2))
        base["observed_mtime_ns"] = 1
        other["observed_mtime_ns"] = 2
        report = repro.compare_records([base], [other])
        self.assertEqual(len(report["filesystem_mtime_differences"]), 1)
        self.assertEqual(len(report["archive_metadata_differences"]), 1)
        self.assertEqual(len(report["path_only_payload_differences"]), 1)
        self.assertGreaterEqual(len(report["embedded_path_leaks"]), 1)
        self.assertFalse(report["reproducible"])

    def test_archive_order_name_encoding_padding_and_raw_fallback_are_classified(self):
        temporary, ordered, reordered = records_for(
            b"!<arch>\n" + ar_member("a.o", b"a") + ar_member("b.o", b"b"),
            b"!<arch>\n" + ar_member("b.o", b"b") + ar_member("a.o", b"a"),
        )
        self.addCleanup(temporary.cleanup)
        self.assertTrue(repro.compare_records([ordered], [reordered])["archive_format_differences"])

        temporary2, newline_padding, nul_padding = records_for(
            b"!<arch>\n" + ar_member("a.o", b"a"),
            b"!<arch>\n" + ar_member("a.o", b"a")[:-1] + b"\0",
        )
        self.addCleanup(temporary2.cleanup)
        self.assertTrue(
            repro.compare_records([newline_padding], [nul_padding])["archive_format_differences"]
        )

        fallback = dict(ordered)
        fallback["sha256"] = "different-unparsed-byte"
        fallback["normalized_sha256"] = "different-unparsed-byte"
        self.assertTrue(
            repro.compare_records([ordered], [fallback])["archive_format_differences"]
        )

    def test_path_only_bsd_member_name_is_classified_separately(self):
        first_name = b"/tmp/a/member.o"
        second_name = b"/tmp/much-longer-b/member.o"

        def bsd(name):
            raw = "#1/{}".format(len(name)).encode().ljust(16)
            body = name + b"payload"
            header = raw + b"0".ljust(12) + b"0".ljust(6) + b"0".ljust(6)
            header += b"100644".ljust(8) + str(len(body)).encode().ljust(10) + b"`\n"
            return header + body + (b"\n" if len(body) % 2 else b"")

        temporary, first, second = records_for(
            b"!<arch>\n" + bsd(first_name),
            b"!<arch>\n" + bsd(second_name),
            ((b"/tmp/a", b"<SOURCE_ROOT>"), (b"/tmp/much-longer-b", b"<SOURCE_ROOT>")),
        )
        self.addCleanup(temporary.cleanup)
        report = repro.compare_records([first], [second])
        self.assertTrue(report["path_only_archive_name_differences"])

    def test_path_only_name_does_not_hide_independent_padding_change(self):
        first_name = b"/tmp/aa/member.o"
        second_name = b"/tmp/bb/member.o"

        def bsd(name, padding):
            raw = "#1/{}".format(len(name)).encode().ljust(16)
            body = name + b"x"
            header = raw + b"0".ljust(12) + b"0".ljust(6) + b"0".ljust(6)
            header += b"100644".ljust(8) + str(len(body)).encode().ljust(10) + b"`\n"
            return b"!<arch>\n" + header + body + padding

        temporary, first, second = records_for(
            bsd(first_name, b"\n"),
            bsd(second_name, b"\0"),
            ((b"/tmp/aa", b"<SOURCE_ROOT>"), (b"/tmp/bb", b"<SOURCE_ROOT>")),
        )
        self.addCleanup(temporary.cleanup)
        report = repro.compare_records([first], [second])
        self.assertTrue(report["path_only_archive_name_differences"])
        self.assertTrue(report["archive_format_differences"])

    def test_path_only_name_does_not_hide_noncanonical_bsd_header_fields(self):
        first_name = b"/tmp/aa/member.o"
        second_name = b"/tmp/bb/member.o"

        def bsd(name, name_field, size_field):
            body = name + b"payload"
            header = name_field.ljust(16) + b"0".ljust(12) + b"0".ljust(6)
            header += b"0".ljust(6) + b"100644".ljust(8) + size_field.ljust(10)
            return b"!<arch>\n" + header + b"`\n" + body

        size = str(len(first_name) + len(b"payload")).encode()
        temporary, first, second = records_for(
            bsd(first_name, b"#1/017", size.rjust(10, b"0")),
            bsd(second_name, b"#1/17", size),
            ((b"/tmp/aa", b"<SOURCE_ROOT>"), (b"/tmp/bb", b"<SOURCE_ROOT>")),
        )
        self.addCleanup(temporary.cleanup)
        report = repro.compare_records([first], [second])
        self.assertTrue(report["path_only_archive_name_differences"])
        self.assertTrue(report["archive_format_differences"])

    def test_gnu_long_names_are_safe_and_classified_as_names(self):
        first_name = b"/tmp/aa/member.o"
        second_name = b"/tmp/bb/member.o"

        def gnu(name):
            def raw_member(name_field, payload):
                header = name_field.ljust(16) + b"0".ljust(12) + b"0".ljust(6)
                header += b"0".ljust(6) + b"100644".ljust(8)
                header += str(len(payload)).encode().ljust(10) + b"`\n"
                return header + payload + (b"\n" if len(payload) % 2 else b"")

            table = name + b"/\n"
            return (
                b"!<arch>\n"
                + raw_member(b"//", table)
                + raw_member(b"/0", b"payload")
            )

        temporary, first, second = records_for(
            gnu(first_name),
            gnu(second_name),
            ((b"/tmp/aa", b"<SOURCE_ROOT>"), (b"/tmp/bb", b"<SOURCE_ROOT>")),
        )
        self.addCleanup(temporary.cleanup)
        report = repro.compare_records([first], [second])
        self.assertTrue(report["path_only_archive_name_differences"])
        self.assertFalse(report["path_only_payload_differences"])
        serialized = json.dumps({"observation": first, "comparison": report})
        self.assertNotIn("/tmp/aa", serialized)

    def test_serialized_reports_never_disclose_lexical_or_canonical_roots(self):
        with tempfile.TemporaryDirectory() as temporary:
            real = Path(temporary) / "canonical-root"
            real.mkdir()
            lexical = Path(temporary) / "lexical-root"
            lexical.symlink_to(real, target_is_directory=True)
            lexical_name = str(lexical / "member.o").encode()
            canonical_name = str(real.resolve() / "member.o").encode()

            def bsd(name, payload):
                raw = "#1/{}".format(len(name)).encode().ljust(16)
                body = name + payload
                header = raw + b"0".ljust(12) + b"0".ljust(6) + b"0".ljust(6)
                header += b"100644".ljust(8) + str(len(body)).encode().ljust(10) + b"`\n"
                return b"!<arch>\n" + header + body + (b"\n" if len(body) % 2 else b"")

            replacements = repro.path_replacements(lexical, "i")
            archive_a = Path(temporary) / "a.rlib"
            archive_b = Path(temporary) / "b.rlib"
            archive_a.write_bytes(bsd(lexical_name, lexical_name + b" payload"))
            archive_b.write_bytes(bsd(canonical_name, canonical_name + b" payload"))
            first = repro.file_record(
                "archive", "rust-library", "root//x:lib", archive_a, replacements
            )
            second = repro.file_record(
                "archive", "rust-library", "root//x:lib", archive_b, replacements
            )
            report = repro.compare_records([first], [second])
            serialized = json.dumps(
                {
                    "first_observations": first,
                    "second_observations": second,
                    "first_manifest": repro.normalized_manifest([first], "abc"),
                    "second_manifest": repro.normalized_manifest([second], "abc"),
                    "comparison": report,
                },
                sort_keys=True,
            )
            self.assertNotIn(str(lexical), serialized)
            self.assertNotIn(str(real.resolve()), serialized)
            self.assertIn("<SOURCE_ROOT>/member.o", serialized)

    def test_filesystem_mode_difference_is_blocking(self):
        base = {"key": "x", "kind": "file", "mode": 0o644, "sha256": "a"}
        executable = dict(base, mode=0o755)
        report = repro.compare_records([base], [executable])
        self.assertTrue(report["filesystem_mode_differences"])
        self.assertFalse(report["reproducible"])

    def test_contract_output_selection_ignores_scaffolding(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            target = root / "buck-out/i/art/root/abc/x/__run__"
            (target / "OUT_DIR").mkdir(parents=True)
            (target / "OUT_DIR/generated.rs").write_text("generated")
            (target / "rustc_flags").write_text("--cfg=x")
            (target / "__rustc_shim.sh").write_text("shim")
            unrelated = root / "buck-out/i/art/root/def/x/__run__"
            unrelated.mkdir(parents=True)
            (unrelated / "rustc_flags").write_text("must not be inventoried")
            contract = {
                "package": "x",
                "name": "run",
                "category": "build-script-output",
                "configured_labels": [
                    {"hash": "abc", "label": "root//x:run (cfg#abc)"}
                ],
            }
            expected = {
                "root//x:run (cfg#abc)": [
                    "buck-out/i/art/root/abc/x/__run__/OUT_DIR",
                    "buck-out/i/art/root/abc/x/__run__/rustc_flags",
                ]
            }
            outputs = repro.contract_outputs(root, "i", contract, expected)
            self.assertEqual(
                [name for name, _ in outputs],
                [
                    "configured-abc/art/root/abc/x/__run__/OUT_DIR/generated.rs",
                    "configured-abc/art/root/abc/x/__run__/rustc_flags",
                ],
            )

    def test_inventory_fails_when_one_configured_variant_has_no_output(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            target = root / "buck-out/i/art/root/abc/x/__lib__/LPPF"
            target.mkdir(parents=True)
            (target / "libx.rlib").write_bytes(b"!<arch>\n")
            graph = {
                "root//x:lib (cfg#abc)": {"buck.type": "rust_library", "name": "lib"},
                "root//x:lib (cfg#def)": {"buck.type": "rust_library", "name": "lib"},
            }
            with self.assertRaisesRegex(repro.DiagnosticError, "cfg#def"):
                repro.inventory(
                    root,
                    "i",
                    graph,
                    {
                        "root//x:lib (cfg#abc)": [
                            "buck-out/i/art/root/abc/x/__lib__/LPPF/libx.rlib"
                        ],
                        "root//x:lib (cfg#def)": [
                            "buck-out/i/art/root/def/x/__lib__/LPPF/libx.rlib"
                        ],
                    },
                )

    def test_rust_contract_selects_rlib_rmeta_but_ignores_dependency_links(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            target = root / "buck-out/i/art/root/abc/x/__lib__"
            (target / "out/LPPMD").mkdir(parents=True)
            (target / "out/LPPMD/libx.rmeta").write_bytes(b"metadata")
            (target / "LPPL").mkdir()
            (target / "LPPL/libx.rlib").write_bytes(b"!<arch>\n")
            (target / "LPPF-depslink-symlinked_dirs/0").mkdir(parents=True)
            (target / "LPPF-depslink-symlinked_dirs/0/libdep.rlib").write_bytes(
                b"!<arch>\n"
            )
            contract = {
                "package": "x",
                "name": "lib",
                "category": "rust-library",
                "configured_labels": [
                    {"hash": "abc", "label": "root//x:lib (cfg#abc)"}
                ],
            }
            expected = {
                "root//x:lib (cfg#abc)": [
                    "buck-out/i/art/root/abc/x/__lib__/out/LPPMD/libx.rmeta",
                    "buck-out/i/art/root/abc/x/__lib__/LPPL/libx.rlib",
                    "buck-out/i/art/root/abc/x/__lib__/LPPF-depslink-symlinked_dirs/0/libdep.rlib",
                ]
            }
            outputs = repro.contract_outputs(root, "i", contract, expected)
            self.assertEqual(
                [logical for logical, _ in outputs],
                [
                    "configured-abc/art/root/abc/x/__lib__/LPPL/libx.rlib",
                    "configured-abc/art/root/abc/x/__lib__/out/LPPMD/libx.rmeta",
                ],
            )

    def test_unmaterialized_provider_alternative_is_ignored(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            target = root / "buck-out/i/art/root/abc/x/__lib__/LPPF"
            target.mkdir(parents=True)
            (target / "libx.rlib").write_bytes(b"!<arch>\n")
            contract = {
                "package": "x",
                "name": "lib",
                "category": "rust-library",
                "configured_labels": [
                    {"hash": "abc", "label": "root//x:lib (cfg#abc)"}
                ],
            }
            expected = {
                "root//x:lib (cfg#abc)": [
                    "buck-out/i/art/root/abc/x/__lib__/LPPF/libx.rlib",
                    "buck-out/i/art/root/abc/x/__lib__/LPPM/libx.rmeta",
                ]
            }
            outputs = repro.contract_outputs(root, "i", contract, expected)
            self.assertEqual(
                [logical for logical, _ in outputs],
                ["configured-abc/art/root/abc/x/__lib__/LPPF/libx.rlib"],
            )

    def test_provider_parser_preserves_each_configured_variant(self):
        contracts = [
            {
                "category": "rust-library",
                "configured_labels": [
                    {"label": "root//x:lib (cfg#abc)", "hash": "abc"},
                    {"label": "root//x:lib (cfg#def)", "hash": "def"},
                ]
            }
        ]
        text = "\n".join(
            (
                "      default_outputs=[ <build artifact a.rlib bound to root//x:lib (cfg#abc)> ]",
                "          default_outputs=[ <build artifact a.rmeta bound to root//x:lib (cfg#abc)> ]",
                "      default_outputs=[ <build artifact b.rlib bound to root//x:lib (cfg#def)> ]",
                "          default_outputs=[ <build artifact ignored.rlib bound to root//x:lib (cfg#def)> ]",
            )
        )
        parsed = repro.provider_artifacts(text, contracts)
        self.assertEqual(len(parsed["root//x:lib (cfg#abc)"]), 2)
        self.assertEqual(len(parsed["root//x:lib (cfg#def)"]), 2)

    def test_path_replacements_include_lexical_and_canonical_spellings(self):
        with tempfile.TemporaryDirectory() as temporary:
            real = Path(temporary) / "real"
            real.mkdir()
            link = Path(temporary) / "link"
            link.symlink_to(real, target_is_directory=True)
            replacements = dict(repro.path_replacements(link, "i"))
            self.assertIn(str(link).encode(), replacements)
            self.assertIn(str(real.resolve()).encode(), replacements)
            self.assertIn(str(link / "buck-out/i").encode(), replacements)
            self.assertIn(str(real.resolve() / "buck-out/i").encode(), replacements)

    def test_hardened_buck_environment_cannot_reach_cache_credentials(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            previous = os.environ.get("BUILDBUDDY_API_KEY")
            os.environ["BUILDBUDDY_API_KEY"] = "must-not-survive"
            try:
                env = repro.hardened_buck_env(root, "UTC")
            finally:
                if previous is None:
                    os.environ.pop("BUILDBUDDY_API_KEY", None)
                else:
                    os.environ["BUILDBUDDY_API_KEY"] = previous
            self.assertNotIn("BUILDBUDDY_API_KEY", env)
            self.assertEqual(
                env["RUE_BUILDBUDDY_CONFIG"],
                str(root / ".diagnostic-no-buildbuddy-config"),
            )
            self.assertFalse(Path(env["RUE_BUILDBUDDY_CONFIG"]).exists())
            self.assertEqual(env["SOURCE_DATE_EPOCH"], repro.SOURCE_DATE_EPOCH)

    def test_wrapper_sentinel_mutation_fails_closed(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            sentinel = root / ".buckconfig.local"
            sentinel.write_bytes(b"")
            sentinel.chmod(0o600)
            wrapper = root / "buck2"
            wrapper.write_text("#!/bin/sh\nchmod 644 .buckconfig.local\n")
            wrapper.chmod(0o755)
            env = repro.hardened_buck_env(root, "UTC")
            with self.assertRaisesRegex(repro.DiagnosticError, "changed during"):
                repro.run_buck([], root, env, repro.sentinel_state(root))

    def test_normalized_manifest_separates_raw_relocation_observations(self):
        records = [
            {
                "key": "generated-output/root//x:g/out/x",
                "category": "generated-output",
                "label": "root//x:g",
                "kind": "file",
                "size": 19,
                "normalized_size": 13,
                "sha256": "raw-root-dependent",
                "normalized_sha256": "stable",
                "mode": 0o644,
                "observed_mtime_ns": 123,
                "embedded_paths": ["<SOURCE_ROOT>"],
            }
        ]
        manifest = repro.normalized_manifest(records, "abc123")
        artifact = manifest["artifacts"][0]
        self.assertEqual(manifest["revision"], "abc123")
        self.assertNotIn("sha256", artifact)
        self.assertNotIn("size", artifact)
        self.assertNotIn("observed_mtime_ns", artifact)
        self.assertEqual(artifact["normalized_size"], 13)


if __name__ == "__main__":
    unittest.main()
