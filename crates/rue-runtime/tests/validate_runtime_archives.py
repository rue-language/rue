#!/usr/bin/env python3
"""Validate the object format and architecture of each runtime archive."""

import os
import struct
from pathlib import Path


AR_MAGIC = b"!<arch>\n"
ELF_MAGIC = b"\x7fELF"


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


def main():
    validate_archive(Path(os.environ["RUNTIME_X86_64_LINUX"]), "elf", 62)
    validate_archive(Path(os.environ["RUNTIME_AARCH64_LINUX"]), "elf", 183)
    validate_archive(Path(os.environ["RUNTIME_AARCH64_MACOS"]), "macho", 0x0100000C)


if __name__ == "__main__":
    main()
