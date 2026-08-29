#!/usr/bin/env python3
"""Host-independent unit tests for the runtime archive shape guard."""

import os
import re
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
import validate_runtime_archives as validator  # noqa: E402


def body(name, code, relocs=()):
    return {"name": name, "code": bytes(code), "relocs": list(relocs)}


class InstructionShapeTests(unittest.TestCase):
    def test_stack_spill_is_not_word_access(self):
        self.assertFalse(validator.has_non_stack_word_access(bytes.fromhex("488b442408"), 62))
        self.assertFalse(validator.has_non_stack_word_access((0xF94003E0).to_bytes(4, "little"), 183))

    def test_non_stack_word_access_is_detected(self):
        self.assertTrue(validator.has_non_stack_word_access(bytes.fromhex("488b07"), 62))
        self.assertTrue(validator.has_non_stack_word_access((0xF9400020).to_bytes(4, "little"), 183))

    def test_immediate_bytes_and_frame_pointer_spills_are_not_accesses(self):
        # movabs rax, 0x00000000078b4801; the embedded 48 8b 07 is data.
        self.assertFalse(validator.has_non_stack_word_access(bytes.fromhex("48b801488b0700000000c3"), 62))
        # lea rax, [abs 0x00000000078b4801]; SIB no-base consumes disp32.
        self.assertFalse(validator.has_non_stack_word_access(bytes.fromhex("488d0425488b0700c3"), 62))
        # test al, 0x48; its immediate contains the start of a fake REX.W load.
        self.assertFalse(validator.has_non_stack_word_access(bytes.fromhex("f6c0488b07c3"), 62))
        # test eax, 0x00078b48; F7 /0 consumes a four-byte immediate.
        self.assertFalse(validator.has_non_stack_word_access(bytes.fromhex("f7c0488b0700c3"), 62))
        self.assertFalse(validator.has_non_stack_word_access((0xF94003A0).to_bytes(4, "little"), 183))

    def test_control_transfer_offsets(self):
        self.assertEqual(
            validator.relocation_is_control_transfer(bytes.fromhex("e900000000"), 1, 62, 2, "elf"),
            "branch",
        )
        self.assertEqual(
            validator.relocation_is_control_transfer(
                (0x14000000).to_bytes(4, "little"), 0, 183, 282, "elf"
            ),
            "branch",
        )

    def test_data_relocations_are_not_control_transfers(self):
        # movq (%rdi), %rax; subq $external_data, %rax; ret. The R_X86_64_32S
        # relocation starts at the subq immediate, not at a call instruction.
        x86 = bytes.fromhex("488b074881e800000000c3")
        self.assertIsNone(validator.relocation_is_control_transfer(x86, 6, 62, 11, "elf"))
        # The RIP-relative memory form is a data relocation as well.
        self.assertIsNone(
            validator.relocation_is_control_transfer(
                bytes.fromhex("482b0500000000c3"), 3, 62, 2, "elf"
            )
        )
        self.assertIsNone(
            validator.relocation_is_control_transfer(
                (0x90000000).to_bytes(4, "little"), 0, 183, 275, "elf"
            )
        )


class BodyValidationTests(unittest.TestCase):
    def test_tail_recursion_with_spill_is_rejected(self):
        recursive = body("memcpy", bytes.fromhex("488b442408e900000000"), [(6, "memcpy", 4)])
        with self.assertRaisesRegex(AssertionError, "recursively transfers"):
            validator._validate_bodies(Path("synthetic"), {"memcpy": recursive}, "elf", 62)

    def test_local_backedge_with_chunk_access_is_valid(self):
        loop = body("memcpy", bytes.fromhex("488b07e900000000"), [(4, "local_loop", 4)])
        local_loop = body("local_loop", bytes.fromhex("c3"))
        validator._validate_bodies(
            Path("synthetic"), {"memcpy": loop, "local_loop": local_loop}, "elf", 62
        )

    def test_wrapper_follows_canonical_body(self):
        wrapper = body("memcpy", bytes.fromhex("e900000000"), [(1, "canonical", 4)])
        canonical = body("_ZN11rue_runtime6memory6memcpy17h1234567890abcdefE", bytes.fromhex("488b07"))
        validator._validate_bodies(
            Path("synthetic"), {"memcpy": wrapper, "canonical": canonical}, "elf", 62
        )

    def test_unrelated_word_access_helper_is_not_chunk_proof(self):
        wrapper = body("memcpy", bytes.fromhex("e900000000"), [(1, "helper", 4)])
        helper = body("helper", bytes.fromhex("488b07"))
        with self.assertRaisesRegex(AssertionError, "no non-stack machine-word access"):
            validator._validate_bodies(
                Path("synthetic"), {"memcpy": wrapper, "helper": helper}, "elf", 62
            )

    def test_later_branch_indirect_recursion_is_rejected(self):
        wrapper = body(
            "memcpy",
            bytes.fromhex("e800000000e800000000"),
            [(1, "good", 4), (6, "later", 4)],
        )
        good = body("good", bytes.fromhex("488b07"))
        later = body("later", bytes.fromhex("e900000000"), [(1, "memcpy", 4)])
        with self.assertRaisesRegex(AssertionError, "recursively transfers"):
            validator._validate_bodies(
                Path("synthetic"),
                {"memcpy": wrapper, "good": good, "later": later},
                "elf",
                62,
            )

    def test_unresolved_elf_control_transfer_is_rejected(self):
        # A chunk access must not make an unresolved call look safe: a helper
        # in another archive member could conceal a reserved-symbol cycle.
        root = body("memcpy", bytes.fromhex("488b07e800000000"), [(4, "unknown_helper", 4)])
        with self.assertRaisesRegex(AssertionError, "unresolved reachable control transfer"):
            validator._validate_bodies(Path("synthetic.a:member.o"), {"memcpy": root}, "elf", 62)

    def test_unresolved_x86_indirect_transfers_are_rejected(self):
        fixtures = [
            # call *external_helper: SIB no-base absolute disp32 (R_X86_64_32S).
            ("488b07ff142500000000", 6, 11),
            # call *external_helper(%rax): base + disp32 (R_X86_64_32S).
            ("488b07ff90" + "00000000", 5, 11),
            # call *external_helper@GOTPCREL(%rip): RIP-relative disp32 (GOTPCREL).
            ("488b07ff1500000000", 5, 9),
        ]
        for encoded, offset, kind in fixtures:
            root = body("memcpy", bytes.fromhex(encoded), [(offset, "unknown_helper", kind)])
            with self.assertRaisesRegex(AssertionError, "unresolved reachable control transfer"):
                validator._validate_bodies(Path("synthetic.a:member.o"), {"memcpy": root}, "elf", 62)

    def test_x86_rodata_jump_table_is_not_a_helper_edge(self):
        # FF /4 through a SIB no-base address is also how LLVM emits a local
        # jump table. Its resolved .rodata target is not a callable body.
        root = body("memcpy", bytes.fromhex("488b07ff24d500000000"), [(6, ".rodata.table", 11)])
        validator._validate_bodies(Path("synthetic"), {"memcpy": root}, "elf", 62)

    def test_x86_rodata_function_pointer_is_still_a_call_edge(self):
        # FF /2 through the same SIB encoding is an indirect call, even when
        # its function-pointer storage happens to reside in .rodata.
        root = body("memcpy", bytes.fromhex("488b07ff142500000000"), [(6, ".rodata.function_pointer", 11)])
        with self.assertRaisesRegex(AssertionError, "unresolved reachable control transfer"):
            validator._validate_bodies(Path("synthetic"), {"memcpy": root}, "elf", 62)


class TestConfigurationTests(unittest.TestCase):
    def test_libtest_disables_libc_shaped_reserved_exports(self):
        declared_source = os.environ.get("RUNTIME_LIB_SOURCE")
        source_path = Path(declared_source) if declared_source else None
        if source_path is None or not source_path.is_file():
            source_path = Path(__file__).parents[1] / "src" / "lib.rs"
        source = source_path.read_text()
        arm = re.search(
            r"macro_rules! declare_reserved_function \{.*?\n    \(\n        aarch64_macos",
            source,
            re.DOTALL,
        )
        self.assertIsNotNone(arm)
        self.assertRegex(
            arm.group(0),
            r"(?s)all unsafe .*?#\[cfg\(not\(test\)\)\]\s+#\[unsafe\(no_mangle\)\]",
        )

    def test_unresolved_macho_cross_member_control_transfer_is_rejected(self):
        root = body(
            "_memcpy",
            (0xF9400020).to_bytes(4, "little") + (0x14000000).to_bytes(4, "little"),
            [(4, "_cross_member_helper", 2)],
        )
        with self.assertRaisesRegex(AssertionError, "unresolved reachable control transfer"):
            validator._validate_bodies(
                Path("synthetic.a:member.o"), {"_memcpy": root}, "macho", validator.MACHO_CPU_ARM64
            )

    def test_non_control_external_relocation_is_ignored(self):
        # Data relocations do not establish a callable edge and remain valid.
        root = body("memcpy", bytes.fromhex("488b07"), [(0, "external_data", 1)])
        validator._validate_bodies(Path("synthetic"), {"memcpy": root}, "elf", 62)

    def test_valid_x86_data_relocation_does_not_hide_a_call(self):
        root = body("memcpy", bytes.fromhex("488b074881e800000000c3"), [(6, "external_data", 11)])
        validator._validate_bodies(Path("synthetic"), {"memcpy": root}, "elf", 62)

        rip_data = body("memcpy", bytes.fromhex("488b07482b0500000000c3"), [(6, "external_data", 2)])
        validator._validate_bodies(Path("synthetic"), {"memcpy": rip_data}, "elf", 62)

    def test_valid_aarch64_adrp_data_relocation_is_not_a_call(self):
        root = body(
            "memcpy",
            (0x90000000).to_bytes(4, "little") + (0xF9400020).to_bytes(4, "little"),
            [(0, "external_data", 275)],
        )
        validator._validate_bodies(Path("synthetic"), {"memcpy": root}, "elf", 183)

    def test_wrapper_skips_unrelated_outlined_helper(self):
        wrapper = body(
            "memset",
            bytes.fromhex("e800000000e800000000"),
            [(1, "outlined", 4), (6, "chunk", 4)],
        )
        outlined = body("outlined", bytes.fromhex("c3"))
        chunk = body("_ZN11rue_runtime6memory11write_chunk17h1234567890abcdefE", bytes.fromhex("488b07"))
        validator._validate_bodies(
            Path("synthetic"),
            {"memset": wrapper, "outlined": outlined, "chunk": chunk},
            "elf",
            62,
        )

    def test_str_eq_wrapper_follows_bcmp(self):
        wrapper = body("__rue_str_eq", bytes.fromhex("e900000000"), [(1, "bcmp", 4)])
        impl = body("bcmp", bytes.fromhex("488b07"))
        validator._validate_bodies(
            Path("synthetic"), {"__rue_str_eq": wrapper, "bcmp": impl}, "elf", 62
        )

    def test_str_eq_source_requires_canonical_delegation(self):
        import tempfile

        positive = """
        pub unsafe extern \"C\" fn __rue_str_eq(a: *const u8, b: *const u8) -> u64 {
            /* Formatting and comments do not change the authority. */
            ( unsafe {
                crate::memory::bcmp( // canonical comparison
                    a, b, 1
                ) /* result */ == 0
            } ) as u64;
        }
        """
        with tempfile.NamedTemporaryFile("w", delete=False) as file:
            file.write(positive)
            path = Path(file.name)
        try:
            validator.validate_str_eq_source(path)
        finally:
            path.unlink()

    def test_str_eq_source_rejects_independent_loop_and_unrelated_helper(self):
        import tempfile

        sources = [
            """
            fn __rue_str_eq(a: *const u8, b: *const u8) -> u64 {
                while false { let _ = crate::memory::bcmp(a, b, 1); }
                0
            }
            """,
            """
            fn __rue_str_eq(a: *const u8, b: *const u8) -> u64 {
                (crate::memory::memcmp(a, b, 1) == 0) as u64
            }
            """,
            """
            fn __rue_str_eq(a: *const u8, b: *const u8) -> u64 {
                let _ = crate::memory::bcmp(a, b, 1);
                (crate::memory::memcmp(a, b, 1) == 0) as u64
            }
            """,
            """
            fn __rue_str_eq(a: *const u8, b: *const u8) -> u64 {
                let _ = crate::memory::bcmp(a, b, 1);
                1
            }
            """,
        ]
        for source in sources:
            with tempfile.NamedTemporaryFile("w", delete=False) as file:
                file.write(source)
                path = Path(file.name)
            try:
                with self.assertRaises(AssertionError):
                    validator.validate_str_eq_source(path)
            finally:
                path.unlink()

    def test_elf_fixture_skips_undefined_then_keeps_defined_function(self):
        sections = [{}, {"flags": 0x4, "name": ".text.memcpy"}]
        entries = [
            {"name": "memcpy", "section": 0, "info": 2},
            {"name": "memcpy", "section": 1, "info": 2},
        ]
        defined = validator._defined_elf_functions(entries, sections)
        self.assertEqual([entry["section"] for entry in defined], [1])

    def test_defined_duplicate_functions_are_rejected(self):
        sections = [{}, {"flags": 0x4, "name": ".text.memcpy"}]
        entries = [
            {"name": "memcpy", "section": 1, "info": 2},
            {"name": "memcpy", "section": 1, "info": 2},
        ]
        with self.assertRaisesRegex(AssertionError, "ambiguous"):
            validator._defined_elf_functions(entries, sections)

    def test_macho_fixture_skips_undefined_then_keeps_defined_function(self):
        sections = [{"name": "__text", "flags": 0x80000000}]
        entries = [
            {"name": "_memcpy", "section": 0, "type": 0},
            {"name": "_memcpy", "section": 1, "type": validator.N_SECT},
        ]
        defined = validator._defined_macho_functions(entries, sections)
        self.assertEqual([entry["section"] for entry in defined], [1])

    def test_macho_duplicate_functions_are_rejected(self):
        sections = [{"name": "__text", "flags": 0x80000000}]
        entries = [
            {"name": "_memcpy", "section": 1, "type": validator.N_SECT},
            {"name": "_memcpy", "section": 1, "type": validator.N_SECT},
        ]
        with self.assertRaisesRegex(AssertionError, "ambiguous"):
            validator._defined_macho_functions(entries, sections)

    def test_cross_member_duplicate_definitions_are_rejected(self):
        one = {"memcpy": body("memcpy", bytes.fromhex("488b07"))}
        two = {"memcpy": body("memcpy", bytes.fromhex("488b07"))}
        with self.assertRaisesRegex(AssertionError, "ambiguous definition of memcpy"):
            validator._collect_archive_definitions(
                Path("synthetic.a"), [("one.o", one), ("two.o", two)], "elf"
            )


if __name__ == "__main__":
    unittest.main()
