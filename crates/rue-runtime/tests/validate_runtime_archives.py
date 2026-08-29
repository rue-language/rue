#!/usr/bin/env python3
"""Validate the object format and architecture of each runtime archive."""

import os
import re
import struct
from pathlib import Path


AR_MAGIC = b"!<arch>\n"
ELF_MAGIC = b"\x7fELF"
MACHO_64_LE_MAGIC = b"\xcf\xfa\xed\xfe"
LC_SYMTAB = 0x2
N_EXT = 0x01
N_TYPE = 0x0E
N_SECT = 0x0E
N_PEXT = 0x10
CHUNKED_SYMBOLS = ("memcpy", "memmove", "memset", "memcmp", "bcmp", "__rue_str_eq")
RESERVED_SYMBOLS = ("memcpy", "memmove", "memset", "memcmp", "bcmp")
ELF_SHT_RELA = 4
ELF_SHT_REL = 9
ELF_SHT_SYMTAB = 2
MACHO_LC_SEGMENT_64 = 0x19
MACHO_CPU_ARM64 = 0x0100000C


def archive_members(path: Path):
    data = path.read_bytes()
    if not data.startswith(AR_MAGIC):
        raise AssertionError(f"{path}: not an ar archive")

    offset = len(AR_MAGIC)
    while offset < len(data):
        header = data[offset : offset + 60]
        if len(header) != 60 or header[58:60] != b"`\n":
            raise AssertionError(f"{path}: malformed member header at offset {offset}")

        try:
            size = int(header[48:58].decode("ascii").strip())
        except ValueError as error:
            raise AssertionError(f"{path}: invalid member size at offset {offset}") from error

        name = header[:16].decode("ascii", errors="replace").rstrip()
        start = offset + 60
        end = start + size
        if end > len(data):
            raise AssertionError(f"{path}: truncated member {name!r}")

        payload = data[start:end]
        if name.startswith("#1/"):
            name_size = int(name[3:])
            name = payload[:name_size].rstrip(b"\0").decode("utf-8", errors="replace")
            payload = payload[name_size:]

        yield name, payload
        offset = end + size % 2


def elf_machine(payload: bytes):
    if not payload.startswith(ELF_MAGIC):
        return None
    if len(payload) < 20:
        raise AssertionError("truncated ELF object")
    if payload[5] == 1:
        endian = "<"
    elif payload[5] == 2:
        endian = ">"
    else:
        raise AssertionError(f"invalid ELF endianness {payload[5]}")
    return struct.unpack_from(endian + "H", payload, 18)[0]


def macho_cpu(payload: bytes):
    byte_order = {
        b"\xce\xfa\xed\xfe": "<",
        b"\xcf\xfa\xed\xfe": "<",
        b"\xfe\xed\xfa\xce": ">",
        b"\xfe\xed\xfa\xcf": ">",
    }.get(payload[:4])
    if byte_order is None:
        return None
    if len(payload) < 8:
        raise AssertionError("truncated Mach-O object")
    return struct.unpack_from(byte_order + "I", payload, 4)[0]


def macho_symbols(payload: bytes):
    """Return (name, n_type, n_sect) for symbols in a 64-bit little-endian Mach-O."""
    if not payload.startswith(MACHO_64_LE_MAGIC):
        return []
    if len(payload) < 32:
        raise AssertionError("truncated 64-bit Mach-O header")

    ncmds, sizeofcmds = struct.unpack_from("<II", payload, 16)
    commands_end = 32 + sizeofcmds
    if commands_end > len(payload):
        raise AssertionError("truncated Mach-O load commands")

    symoff = nsyms = stroff = strsize = None
    offset = 32
    for _ in range(ncmds):
        if offset + 8 > commands_end:
            raise AssertionError("truncated Mach-O load command")
        cmd, cmdsize = struct.unpack_from("<II", payload, offset)
        if cmdsize < 8 or offset + cmdsize > commands_end:
            raise AssertionError("invalid Mach-O load command size")
        if cmd == LC_SYMTAB:
            if cmdsize < 24:
                raise AssertionError("truncated Mach-O symbol-table command")
            symoff, nsyms, stroff, strsize = struct.unpack_from(
                "<IIII", payload, offset + 8
            )
        offset += cmdsize

    if symoff is None:
        raise AssertionError("Mach-O object has no symbol table")
    symbols_end = symoff + nsyms * 16
    strings_end = stroff + strsize
    if symbols_end > len(payload) or strings_end > len(payload):
        raise AssertionError("truncated Mach-O symbol table")

    symbols = []
    for index in range(nsyms):
        entry = symoff + index * 16
        strx, n_type, n_sect = struct.unpack_from("<IBB", payload, entry)
        if strx >= strsize:
            raise AssertionError("Mach-O symbol has invalid string-table index")
        name_start = stroff + strx
        name_end = payload.find(b"\0", name_start, strings_end)
        if name_end < 0:
            raise AssertionError("unterminated Mach-O symbol name")
        symbols.append((payload[name_start:name_end].decode("utf-8"), n_type, n_sect))
    return symbols


def _bounded_slice(data: bytes, start: int, size: int, description: str) -> bytes:
    end = start + size
    if start < 0 or size < 0 or end > len(data):
        raise AssertionError(f"{description}: range [{start}, {end}) is outside object")
    return data[start:end]


def _function_body(source: str, function_name: str) -> str:
    marker = f"fn {function_name}"
    start = source.find(marker)
    if start < 0:
        raise AssertionError(f"source is missing {function_name}")
    opening = source.find("{", start)
    if opening < 0:
        raise AssertionError(f"source has no body for {function_name}")
    depth = 0
    for index in range(opening, len(source)):
        if source[index] == "{":
            depth += 1
        elif source[index] == "}":
            depth -= 1
            if depth == 0:
                return source[opening + 1:index]
    raise AssertionError(f"source has an unterminated body for {function_name}")


def _strip_rust_comments(source: str) -> str:
    """Remove comments for the narrow source-shape check.

    The runtime body contains no string literals relevant to this invariant;
    stripping both Rust comment forms keeps formatting/comment changes from
    changing the ownership check while leaving the expression structure intact.
    """
    source = re.sub(r"/\*.*?\*/", "", source, flags=re.DOTALL)
    return re.sub(r"//[^\n]*", "", source)


def validate_str_eq_source(path: Path):
    """Enforce the source-level ownership contract for string equality.

    LTO can inline the comparison authority, so object shape alone cannot
    prove provenance. The source contract requires a direct call to bcmp and
    forbids an independent loop or chunk primitive in the exported body.
    """
    code = _strip_rust_comments(path.read_text())
    body = _function_body(code, "__rue_str_eq")
    bcmp_calls = re.findall(r"crate::memory::bcmp\s*\(", body)
    if len(bcmp_calls) != 1:
        raise AssertionError(f"{path}: __rue_str_eq must delegate to crate::memory::bcmp")
    if re.search(r"\b(?:memcpy|memmove|memset|memcmp|bcmp)\s*\(", body.replace(
            "crate::memory::bcmp", "")):
        raise AssertionError(f"{path}: __rue_str_eq uses another memory authority")
    if not re.search(
            r"(?:return\s+)?\(\s*unsafe\s*\{\s*crate::memory::bcmp\s*"
            r"\([^;]*\)\s*==\s*0\s*\}\s*\)\s*as\s+u64\s*;?\s*\Z",
            body,
            re.DOTALL,
    ):
        raise AssertionError(f"{path}: bcmp result is not the returned equality")
    if re.search(r"\b(?:while|for)\b|read_chunk|write_chunk|read_unaligned|write_unaligned", body):
        raise AssertionError(f"{path}: __rue_str_eq contains an independent comparison loop")


def parse_elf_object(payload: bytes):
    """Parse the ELF64 sections, symbols, and relocations needed by the guard."""
    if len(payload) < 64 or not payload.startswith(ELF_MAGIC) or payload[4] != 2:
        raise AssertionError("runtime archive member is not ELF64")
    endian = "<" if payload[5] == 1 else ">" if payload[5] == 2 else None
    if endian is None:
        raise AssertionError("ELF object has invalid endianness")
    section_offset = struct.unpack_from(endian + "Q", payload, 40)[0]
    section_entry_size, section_count, string_index = struct.unpack_from(
        endian + "HHH", payload, 58
    )
    if section_entry_size < 64:
        raise AssertionError("ELF object has short section headers")
    sections = []
    for index in range(section_count):
        base = section_offset + index * section_entry_size
        values = struct.unpack_from(endian + "IIQQQQIIQQ", payload, base)
        _, section_type, flags, address, offset, size, link, info, _, entry_size = values
        sections.append(
            {"type": section_type, "address": address, "offset": offset, "size": size,
             "flags": flags, "link": link, "info": info, "entry_size": entry_size,
             "relocs": []}
        )
    section_names = _bounded_slice(
        payload, sections[string_index]["offset"], sections[string_index]["size"], "ELF section names"
    )
    for index, section in enumerate(sections):
        name_offset = struct.unpack_from(
            endian + "I", payload, section_offset + index * section_entry_size
        )[0]
        end = section_names.find(b"\0", name_offset)
        if end < 0:
            raise AssertionError("ELF section has invalid name")
        section["name"] = section_names[name_offset:end].decode("ascii", errors="replace")

    symbol_tables = {}
    for index, section in enumerate(sections):
        if section["type"] != ELF_SHT_SYMTAB:
            continue
        if section["entry_size"] < 24:
            raise AssertionError("ELF symbol table has short entries")
        strings = sections[section["link"]]
        string_data = _bounded_slice(payload, strings["offset"], strings["size"], "ELF symbol strings")
        entries = []
        count = section["size"] // section["entry_size"]
        for symbol_index in range(count):
            base = section["offset"] + symbol_index * section["entry_size"]
            name_offset, info, _, section_index, value, size = struct.unpack_from(
                endian + "IBBHQQ", payload, base
            )
            name_end = string_data.find(b"\0", name_offset)
            if name_end < 0:
                raise AssertionError("ELF symbol has invalid name")
            entries.append({"name": string_data[name_offset:name_end].decode("utf-8", errors="replace"),
                            "section": section_index, "value": value, "size": size, "info": info})
        symbol_tables[index] = entries

    for section in sections:
        if section["type"] not in (ELF_SHT_RELA, ELF_SHT_REL):
            continue
        symbols = symbol_tables.get(section["link"], [])
        entry_size = section["entry_size"] or (24 if section["type"] == ELF_SHT_RELA else 16)
        for index in range(section["size"] // entry_size):
            base = section["offset"] + index * entry_size
            reloc_offset, info = struct.unpack_from(endian + "QQ", payload, base)
            symbol_index = info >> 32
            target_entry = symbols[symbol_index] if symbol_index < len(symbols) else None
            target = target_entry["name"] if target_entry else ""
            # Local ELF relocations commonly refer to a section symbol. Its
            # empty name still identifies the exact section/function body.
            if not target and target_entry and target_entry["section"] < len(sections):
                target = sections[target_entry["section"]]["name"]
            sections[section["info"]]["relocs"].append((reloc_offset, target, info & 0xffffffff))

    bodies = {}
    for entries in symbol_tables.values():
        by_section = {}
        for entry in _defined_elf_functions(entries, sections):
            by_section.setdefault(entry["section"], []).append(entry)
        for section_index, section_entries in by_section.items():
            section = sections[section_index]
            section_entries.sort(key=lambda entry: entry["value"])
            names = [entry["name"] for entry in section_entries]
            if len(names) != len(set(names)):
                raise AssertionError(
                    f"ELF section {section['name']}: ambiguous function definition"
                )
            for position, entry in enumerate(section_entries):
                relative = entry["value"] - section["address"]
                size = entry["size"]
                if size == 0:
                    next_value = section_entries[position + 1]["value"] if position + 1 < len(section_entries) else section["address"] + section["size"]
                    size = next_value - entry["value"]
                code = _bounded_slice(payload, section["offset"] + relative, size, f"ELF symbol {entry['name']}")
                relocs = [(offset - relative, target, kind) for offset, target, kind in section["relocs"] if relative <= offset < relative + size]
                body = {"name": entry["name"], "code": code, "relocs": relocs,
                        "machine": struct.unpack_from(endian + "H", payload, 18)[0]}
                bodies.setdefault(entry["name"], body)
                if entry["name"].startswith(".text."):
                    bodies.setdefault(entry["name"][6:], body)
    return bodies


def _defined_elf_functions(entries, sections):
    """Keep only defined STT_FUNC symbols in executable ELF sections."""
    result = []
    for entry in entries:
        symbol_type = entry["info"] & 0x0F
        section_index = entry["section"]
        if (not entry["name"] or symbol_type != 2 or section_index == 0
                or section_index >= len(sections)
                or not sections[section_index]["flags"] & 0x4):
            continue
        result.append(entry)
    names = [entry["name"] for entry in result]
    if len(names) != len(set(names)):
        raise AssertionError("ELF symbol table has an ambiguous function definition")
    return result


def parse_macho_object(payload: bytes):
    """Parse 64-bit little-endian Mach-O symbols, text, and relocations."""
    if len(payload) < 32 or not payload.startswith(MACHO_64_LE_MAGIC):
        raise AssertionError("runtime archive member is not 64-bit little-endian Mach-O")
    ncmds, sizeofcmds = struct.unpack_from("<II", payload, 16)
    commands_end = 32 + sizeofcmds
    if commands_end > len(payload):
        raise AssertionError("Mach-O load commands exceed object")
    sections = []
    symtab = None
    offset = 32
    for _ in range(ncmds):
        cmd, cmdsize = struct.unpack_from("<II", payload, offset)
        if cmdsize < 8 or offset + cmdsize > commands_end:
            raise AssertionError("Mach-O has invalid load command")
        if cmd == MACHO_LC_SEGMENT_64:
            nsects = struct.unpack_from("<I", payload, offset + 64)[0]
            for section_index in range(nsects):
                base = offset + 72 + section_index * 80
                name = payload[base:base + 16].split(b"\0", 1)[0].decode("ascii", errors="replace")
                address, size, file_offset, _, reloc_offset, reloc_count = struct.unpack_from(
                    "<QQIIII", payload, base + 32
                )
                flags = struct.unpack_from("<I", payload, base + 64)[0]
                sections.append({"name": name, "address": address, "size": size,
                                 "offset": file_offset, "flags": flags,
                                 "reloc_offset": reloc_offset, "reloc_count": reloc_count,
                                 "relocs": []})
        elif cmd == LC_SYMTAB:
            symtab = struct.unpack_from("<IIII", payload, offset + 8)
        offset += cmdsize
    if symtab is None:
        raise AssertionError("Mach-O object has no symbol table")
    symoff, nsyms, stroff, strsize = symtab
    string_data = _bounded_slice(payload, stroff, strsize, "Mach-O symbol strings")
    entries = []
    for index in range(nsyms):
        base = symoff + index * 16
        name_offset, n_type, section_index, _, value = struct.unpack_from(
            "<IBBHQ", payload, base
        )
        name_end = string_data.find(b"\0", name_offset)
        if name_end < 0:
            raise AssertionError("Mach-O symbol has invalid name")
        entries.append({"name": string_data[name_offset:name_end].decode("utf-8", errors="replace"), "section": section_index, "value": value, "size": 0, "type": n_type})
    for section in sections:
        for index in range(section["reloc_count"]):
            base = section["reloc_offset"] + index * 8
            address, info = struct.unpack_from("<iI", payload, base)
            symbol_index = info & 0x00ffffff
            target = entries[symbol_index]["name"] if info & 0x08000000 and symbol_index < len(entries) else ""
            section["relocs"].append((address, target, info >> 28))
    bodies = {}
    by_section = {}
    for entry in _defined_macho_functions(entries, sections):
        by_section.setdefault(entry["section"] - 1, []).append(entry)
    for section_index, section_entries in by_section.items():
        section = sections[section_index]
        section_entries.sort(key=lambda entry: entry["value"])
        names = [entry["name"] for entry in section_entries]
        if len(names) != len(set(names)):
            raise AssertionError(
                f"Mach-O section {section['name']}: ambiguous function definition"
            )
        for position, entry in enumerate(section_entries):
            next_value = section_entries[position + 1]["value"] if position + 1 < len(section_entries) else section["address"] + section["size"]
            size = next_value - entry["value"]
            relative = entry["value"] - section["address"]
            code = _bounded_slice(payload, section["offset"] + relative, size, f"Mach-O symbol {entry['name']}")
            relocs = [(address - relative, target, kind) for address, target, kind in section["relocs"] if relative <= address < relative + size]
            bodies.setdefault(entry["name"], {"name": entry["name"], "code": code, "relocs": relocs, "machine": MACHO_CPU_ARM64})
    return bodies


def _defined_macho_functions(entries, sections):
    """Keep only defined section symbols in the executable __text section."""
    result = []
    for entry in entries:
        section_index = entry["section"]
        if (not section_index or section_index > len(sections)
                or sections[section_index - 1]["name"] != "__text"
                or not sections[section_index - 1]["flags"] & 0x80000000
                or entry["type"] & N_TYPE != N_SECT):
            continue
        result.append(entry)
    names = [entry["name"] for entry in result]
    if len(names) != len(set(names)):
        raise AssertionError("Mach-O symbol table has an ambiguous function definition")
    return result


def _x86_modrm_end(code: bytes, index: int, modrm: int, rex: int):
    """Return (end, base-register-or-None) for a ModRM memory operand."""
    mod = modrm >> 6
    if mod == 3:
        return index, None
    rm = modrm & 7
    end = index
    base = rm | ((rex & 1) << 3)
    if rm == 4:
        if end >= len(code):
            return None
        sib = code[end]
        end += 1
        sib_base = sib & 7
        if sib_base == 5 and mod == 0:
            base = None
            end += 4
        else:
            base = sib_base | ((rex & 1) << 3)
    elif rm == 5 and mod == 0:
        base = None
    if mod == 1:
        end += 1
    elif mod == 2:
        end += 4
    elif mod == 0 and rm == 5:
        end += 4
    if end > len(code):
        return None
    return end, base


def _decode_x86_instruction(code: bytes, index: int):
    """Decode common x86-64 forms emitted by the optimized runtime.

    This is intentionally a small decoder, not a general disassembler.
    Unknown instructions terminate validation conservatively instead of
    scanning their immediate bytes for instruction-shaped substrings.
    """
    rex = 0
    operand_size = 4
    while index < len(code):
        byte = code[index]
        if 0x40 <= byte <= 0x4F:
            rex = byte
            index += 1
        elif byte == 0x66:
            operand_size = 2
            index += 1
        elif byte in (0xF2, 0xF3, 0xF0, 0x67):
            index += 1
        else:
            break
    if index >= len(code):
        return None
    opcode = code[index]
    index += 1
    if opcode in (0xC3, 0x90, 0xCC) or 0x50 <= opcode <= 0x5F:
        return index, False
    if 0x70 <= opcode <= 0x7F or opcode in (0xEB, 0x6A):
        end = index + 1
        return (end, False) if end <= len(code) else None
    if opcode in (0xE8, 0xE9):
        end = index + 4
        return (end, False) if end <= len(code) else None
    if opcode == 0x68:
        end = index + 4
        return (end, False) if end <= len(code) else None
    if 0xB0 <= opcode <= 0xB7:
        end = index + 1
        return (end, False) if end <= len(code) else None
    if 0xB8 <= opcode <= 0xBF:
        immediate_size = 8 if rex & 8 else operand_size
        end = index + immediate_size
        return (end, False) if end <= len(code) else None
    if opcode == 0x0F:
        if index >= len(code):
            return None
        extended = code[index]
        index += 1
        if 0x80 <= extended <= 0x8F:
            end = index + 4
            return (end, False) if end <= len(code) else None
        if index >= len(code):
            return None
        modrm = code[index]
        index += 1
        parsed = _x86_modrm_end(code, index, modrm, rex)
        if parsed is None:
            return None
        end, _ = parsed
        return end, False
    modrm_opcodes = {
        0x01, 0x03, 0x09, 0x0B, 0x21, 0x23, 0x29, 0x2B, 0x31, 0x33,
        0x39, 0x3B, 0x69, 0x6B, 0x80, 0x81, 0x83, 0x84, 0x85, 0x88,
        0x89, 0x8A, 0x8B, 0x8D, 0x8F, 0xC0, 0xC1, 0xC6, 0xC7, 0xD1,
        0xD3, 0xF6, 0xF7, 0xFE, 0xFF,
    }
    if opcode not in modrm_opcodes or index >= len(code):
        return None
    modrm = code[index]
    index += 1
    parsed = _x86_modrm_end(code, index, modrm, rex)
    if parsed is None:
        return None
    end, base = parsed
    access = opcode in (0x89, 0x8B) and rex & 8 and modrm >> 6 != 3
    if access:
        # RSP/RBP (and their extended forms) are stack bases. A frame-pointer
        # spill cannot satisfy the non-stack chunk-access guard.
        access = base not in (4, 5, 12, 13)
    immediate_size = {
        0x69: operand_size,
        0x6B: 1,
        0x80: 1,
        0x81: operand_size,
        0x83: 1,
        0xC0: 1,
        0xC1: 1,
        0xC6: 1,
        0xC7: operand_size,
    }.get(opcode, 0)
    # F6/F7 group /0 is TEST with an immediate. Consume it explicitly so
    # bytes such as 48 8b 07 inside that immediate cannot be rescanned as a
    # REX.W load. Other F6/F7 groups have no immediate relevant to this guard.
    if opcode == 0xF6 and (modrm >> 3) & 7 == 0:
        immediate_size = 1
    elif opcode == 0xF7 and (modrm >> 3) & 7 == 0:
        immediate_size = operand_size
    end += immediate_size
    return (end, access) if end <= len(code) else None


def has_non_stack_word_access(code: bytes, machine: int) -> bool:
    if machine == 62:
        index = 0
        while index < len(code):
            decoded = _decode_x86_instruction(code, index)
            if decoded is None:
                return False
            index, access = decoded
            if access:
                return True
        return False
    for index in range(0, len(code) - 3, 4):
        word = struct.unpack_from("<I", code, index)[0]
        if (word & 0xffc00000) in (0xf9400000, 0xf9000000, 0xf8400000, 0xf8000000, 0xa9400000, 0xa9000000):
            # SP (31) and the conventional frame pointer X29 are stack bases.
            if (word >> 5) & 31 not in (29, 31):
                return True
    return False


def relocation_is_control_transfer(code: bytes, offset: int, machine: int):
    if machine == 62:
        for opcode_offset in (offset - 1, offset):
            if 0 <= opcode_offset < len(code) and code[opcode_offset] in (0xE8, 0xE9, 0xEB):
                return "call" if code[opcode_offset] == 0xE8 else "branch"
        # Indirect calls/jumps use FF /2 or FF /4. Relocations are attached
        # to the displacement/immediate, so account for the opcode and ModRM
        # immediately before the relocation rather than treating every REX
        # prefix as a transfer.
        for opcode_offset in (offset - 2, offset - 1, offset):
            if 0 <= opcode_offset + 1 < len(code):
                opcode = code[opcode_offset]
                if 0x40 <= opcode <= 0x4F and opcode_offset + 2 < len(code):
                    opcode, modrm = code[opcode_offset + 1], code[opcode_offset + 2]
                else:
                    modrm = code[opcode_offset + 1]
                if opcode == 0xFF and ((modrm >> 3) & 7) in (2, 4):
                    return "call-or-branch"
        return None
    if offset < 0 or offset + 4 > len(code):
        return None
    word = struct.unpack_from("<I", code, offset)[0]
    if word & 0xfc000000 == 0x94000000:
        return "call"
    if word & 0xfc000000 == 0x14000000:
        return "branch"
    return None


def _normalized_target(target: str, expected_format: str) -> str:
    target = target[6:] if target.startswith(".text.") else target
    if expected_format == "macho" and target.startswith("_"):
        target = target[1:]
    return target


def _target_body(bodies: dict, target: str, expected_format: str):
    normalized = _normalized_target(target, expected_format)
    return bodies.get(target) or bodies.get(normalized) or bodies.get(
        ".text." + normalized if expected_format == "elf" else "_" + normalized
    )


def _is_chunk_accessor(name: str) -> bool:
    return bool(re.search(r"memory\d+(?:read_chunk|write_chunk)", name))


def _is_canonical_body(name: str, symbol: str) -> bool:
    return bool(re.search(r"memory\d+" + re.escape(symbol) + r"\d+h[0-9a-f]+E$", name))


def _validate_bodies(path: Path, bodies: dict, expected_format: str,
                     expected_machine: int):
    """Validate symbol bodies and wrapper targets in one object."""
    def audit_reachable(current, seen, symbol):
        if current["name"] in seen:
            return
        seen = seen | {current["name"]}
        # A reserved export must never call/jump to another reserved export,
        # even when it also contains a word access. This catches accidental
        # compiler-builtins recursion and tail recursion.
        for offset, target, _ in current["relocs"]:
            if not relocation_is_control_transfer(current["code"], offset,
                                                   expected_machine):
                continue
            normalized = _normalized_target(target, expected_format)
            if normalized in RESERVED_SYMBOLS and symbol in RESERVED_SYMBOLS:
                raise AssertionError(
                    f"{path}: {symbol} recursively transfers to reserved symbol {normalized}"
                )
            target_body = _target_body(bodies, target, expected_format)
            if target_body is None:
                destination = normalized or "<unnamed relocation>"
                raise AssertionError(
                    f"{path}: {symbol} has unresolved reachable control transfer "
                    f"to {destination}"
                )
            audit_reachable(target_body, seen, symbol)

    def reaches_accessor(current, seen, root_name, symbol):
        if current["name"] in seen:
            return False
        seen = seen | {current["name"]}
        is_approved_body = (
            current["name"] == root_name
            or _is_canonical_body(current["name"], symbol)
            or _is_chunk_accessor(current["name"])
            or (symbol == "__rue_str_eq" and current["name"] in ("bcmp", "_bcmp"))
        )
        if is_approved_body and has_non_stack_word_access(current["code"], expected_machine):
            return True
        if has_non_stack_word_access(current["code"], expected_machine):
            return False
        # Optimizers may outline an unrelated check into a local helper while
        # leaving the real chunk accessor as another relocation. Explore all
        # local control-transfer candidates, but only accept a canonical body or
        # the named chunk accessors as proof of the required implementation.
        for offset, target, _ in current["relocs"]:
            if not relocation_is_control_transfer(current["code"], offset,
                                                   expected_machine) or not target:
                continue
            target_body = _target_body(bodies, target, expected_format)
            if target_body is not None and reaches_accessor(
                    target_body, seen, root_name, symbol):
                return True
        return False

    for symbol in CHUNKED_SYMBOLS:
        lookup = symbol if expected_format == "elf" else "_" + symbol
        body = bodies.get(lookup)
        if body is None:
            continue
        audit_reachable(body, set(), symbol)
        if not reaches_accessor(body, set(), body["name"], symbol):
            raise AssertionError(f"{path}: {symbol} body has no non-stack machine-word access")


def validate_chunked_primitives(path: Path, expected_format: str, expected_machine: int):
    """Inspect exact symbol bodies without depending on host disassembly tools."""
    objects = []
    for name, payload in archive_members(path):
        machine = elf_machine(payload) if expected_format == "elf" else macho_cpu(payload)
        if machine != expected_machine:
            continue
        bodies = parse_elf_object(payload) if expected_format == "elf" else parse_macho_object(payload)
        objects.append((name, bodies))
    definitions = _collect_archive_definitions(path, objects, expected_format)
    for symbol, found in definitions.items():
        if not found:
            raise AssertionError(f"{path}: could not locate required symbol body {symbol}")
        if len(found) != 1:
            members = ", ".join(repr(name) for name, _ in found)
            raise AssertionError(f"{path}: ambiguous definition of {symbol} in {members}")
        member, bodies = found[0]
        _validate_bodies(
            f"{path}: member {member!r}", bodies, expected_format, expected_machine
        )


def _collect_archive_definitions(path: Path, objects, expected_format: str):
    """Collect every required definition so duplicate members fail closed."""
    definitions = {symbol: [] for symbol in CHUNKED_SYMBOLS}
    for name, bodies in objects:
        for symbol in CHUNKED_SYMBOLS:
            lookup = symbol if expected_format == "elf" else "_" + symbol
            if lookup in bodies:
                definitions[symbol].append((name, bodies))
    for symbol, found in definitions.items():
        if len(found) > 1:
            members = ", ".join(repr(name) for name, _ in found)
            raise AssertionError(f"{path}: ambiguous definition of {symbol} in {members}")
    return definitions


def validate_archive(path: Path, expected_format: str, expected_machine: int):
    object_count = 0
    for name, payload in archive_members(path):
        elf = elf_machine(payload)
        macho = macho_cpu(payload)
        if elf is not None:
            if expected_format != "elf":
                raise AssertionError(f"{path}: member {name!r} is ELF, expected Mach-O")
            if elf != expected_machine:
                raise AssertionError(
                    f"{path}: member {name!r} has ELF machine {elf}, expected {expected_machine}"
                )
            object_count += 1
        elif macho is not None:
            if expected_format != "macho":
                raise AssertionError(f"{path}: member {name!r} is Mach-O, expected ELF")
            if macho != expected_machine:
                raise AssertionError(
                    f"{path}: member {name!r} has Mach-O CPU {macho:#x}, "
                    f"expected {expected_machine:#x}"
                )
            object_count += 1

    if object_count == 0:
        raise AssertionError(f"{path}: archive contains no recognized object members")

    if expected_format == "macho":
        trampoline_symbols = []
        for name, payload in archive_members(path):
            for symbol, n_type, n_sect in macho_symbols(payload):
                if symbol == "_rue_darwin_sigtramp":
                    trampoline_symbols.append((name, n_type, n_sect))
        if len(trampoline_symbols) != 1:
            raise AssertionError(
                f"{path}: expected one _rue_darwin_sigtramp symbol, "
                f"found {len(trampoline_symbols)}"
            )
        member, n_type, n_sect = trampoline_symbols[0]
        if n_type & N_PEXT == 0:
            raise AssertionError(
                f"{path}: member {member!r} trampoline is not private external "
                f"(n_type={n_type:#x})"
            )
        if n_type & N_TYPE != N_SECT or n_sect == 0:
            raise AssertionError(
                f"{path}: member {member!r} trampoline is not a defined section "
                f"symbol (n_type={n_type:#x}, n_sect={n_sect})"
            )
        if n_type & N_EXT == 0:
            raise AssertionError(
                f"{path}: member {member!r} trampoline is not externally linkable "
                f"(n_type={n_type:#x})"
            )

    validate_chunked_primitives(path, expected_format, expected_machine)


def main():
    source = os.environ.get("RUNTIME_STRING_SOURCE")
    if source:
        validate_str_eq_source(Path(source))
    validate_archive(Path(os.environ["RUNTIME_X86_64_LINUX"]), "elf", 62)
    validate_archive(Path(os.environ["RUNTIME_AARCH64_LINUX"]), "elf", 183)
    validate_archive(Path(os.environ["RUNTIME_AARCH64_MACOS"]), "macho", 0x0100000C)


if __name__ == "__main__":
    main()
