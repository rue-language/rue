// Derived from object's round_trip.rs:
// https://github.com/gimli-rs/object/blob/0.32.0/tests/round_trip/mod.rs

use ar_archive_writer::ArchiveKind;
use object::{read, write};
use object::{
    Architecture, BinaryFormat, Endianness, RelocationEncoding, RelocationKind, SymbolFlags,
    SymbolKind, SymbolScope,
};
use pretty_assertions::assert_eq;

mod common;

fn round_trip_and_diff(
    test_name: &str,
    object: write::Object<'_>,
    archive_kind: ArchiveKind,
    trim_output_bytes: usize,
) {
    let tmpdir = common::create_tmp_dir(test_name);
    let input_bytes = object.write().unwrap();

    // Create a new archive using ar_archive_writer.
    let output_archive_bytes = common::create_archive_with_ar_archive_writer(
        &tmpdir,
        archive_kind,
        [("input.o", input_bytes.as_slice())],
        false,
    );

    // Read the archive and member using object and diff with original data.
    {
        let output_archive =
            read::archive::ArchiveFile::parse(output_archive_bytes.as_slice()).unwrap();
        let output_member = output_archive.members().next().unwrap().unwrap();
        assert_eq!(output_member.name(), b"input.o");
        let output_bytes = output_member.data(output_archive_bytes.as_slice()).unwrap();

        // Apply fixup if required.
        let output_bytes = &output_bytes[..output_bytes.len() - trim_output_bytes];

        assert_eq!(
            &input_bytes, output_bytes,
            "Comparing object after round-trip. Test case: build {:?} for {:?}",
            archive_kind, object
        );
    }

    // Use llvm-ar to create the archive and diff with ar_archive_writer.
    let output_llvm_ar_bytes = common::create_archive_with_llvm_ar(
        &tmpdir,
        archive_kind,
        [("input.o", input_bytes.as_slice())],
        false,
    );
    assert_eq!(
        output_archive_bytes, output_llvm_ar_bytes,
        "Comparing ar_archive_writer to llvm-ar. Test case: build {:?} for {:?}",
        archive_kind, object
    );
}

#[test]
fn coff_x86_64() {
    let mut object =
        write::Object::new(BinaryFormat::Coff, Architecture::X86_64, Endianness::Little);

    object.add_file_symbol(b"file.c".to_vec());

    let text = object.section_id(write::StandardSection::Text);
    object.append_section_data(text, &[1; 30], 4);

    let func1_offset = object.append_section_data(text, &[1; 30], 4);
    assert_eq!(func1_offset, 32);
    let func1_symbol = object.add_symbol(write::Symbol {
        name: b"func1".to_vec(),
        value: func1_offset,
        size: 32,
        kind: SymbolKind::Text,
        scope: SymbolScope::Linkage,
        weak: false,
        section: write::SymbolSection::Section(text),
        flags: SymbolFlags::None,
    });
    object
        .add_relocation(
            text,
            write::Relocation {
                offset: 8,
                size: 64,
                kind: RelocationKind::Absolute,
                encoding: RelocationEncoding::Generic,
                symbol: func1_symbol,
                addend: 0,
            },
        )
        .unwrap();

    round_trip_and_diff("coff_x86_64", object, ArchiveKind::Gnu, 0);
}

#[test]
fn elf_x86_64() {
    let mut object =
        write::Object::new(BinaryFormat::Elf, Architecture::X86_64, Endianness::Little);

    object.add_file_symbol(b"file.c".to_vec());

    let text = object.section_id(write::StandardSection::Text);
    object.append_section_data(text, &[1; 30], 4);

    let func1_offset = object.append_section_data(text, &[1; 30], 4);
    assert_eq!(func1_offset, 32);
    let func1_symbol = object.add_symbol(write::Symbol {
        name: b"func1".to_vec(),
        value: func1_offset,
        size: 32,
        kind: SymbolKind::Text,
        scope: SymbolScope::Linkage,
        weak: false,
        section: write::SymbolSection::Section(text),
        flags: SymbolFlags::None,
    });
    object
        .add_relocation(
            text,
            write::Relocation {
                offset: 8,
                size: 64,
                kind: RelocationKind::Absolute,
                encoding: RelocationEncoding::Generic,
                symbol: func1_symbol,
                addend: 0,
            },
        )
        .unwrap();

    round_trip_and_diff("elf_x86_64", object, ArchiveKind::Gnu, 0);
}

#[test]
fn elf_any() {
    for (arch, endian, archive_kinds) in [
        (
            Architecture::Aarch64,
            Endianness::Little,
            &[ArchiveKind::Gnu][..],
        ),
        (
            Architecture::Aarch64_Ilp32,
            Endianness::Little,
            &[ArchiveKind::Gnu],
        ),
        (Architecture::Arm, Endianness::Little, &[ArchiveKind::Gnu]),
        (Architecture::Avr, Endianness::Little, &[ArchiveKind::Gnu]),
        (Architecture::Bpf, Endianness::Little, &[ArchiveKind::Gnu]),
        (Architecture::Csky, Endianness::Little, &[ArchiveKind::Gnu]),
        (Architecture::I386, Endianness::Little, &[ArchiveKind::Gnu]),
        (
            Architecture::X86_64,
            Endianness::Little,
            &[ArchiveKind::Gnu],
        ),
        (
            Architecture::X86_64_X32,
            Endianness::Little,
            &[ArchiveKind::Gnu],
        ),
        (
            Architecture::Hexagon,
            Endianness::Little,
            &[ArchiveKind::Gnu],
        ),
        (
            Architecture::LoongArch64,
            Endianness::Little,
            &[ArchiveKind::Gnu],
        ),
        (Architecture::Mips, Endianness::Little, &[ArchiveKind::Gnu]),
        (
            Architecture::Mips64,
            Endianness::Little,
            &[ArchiveKind::Gnu],
        ),
        (
            Architecture::Msp430,
            Endianness::Little,
            &[ArchiveKind::Gnu],
        ),
        (Architecture::PowerPc, Endianness::Big, &[ArchiveKind::Gnu]),
        (
            Architecture::PowerPc64,
            Endianness::Big,
            &[ArchiveKind::Gnu, ArchiveKind::AixBig],
        ),
        (
            Architecture::Riscv32,
            Endianness::Little,
            &[ArchiveKind::Gnu],
        ),
        (
            Architecture::Riscv64,
            Endianness::Little,
            &[ArchiveKind::Gnu],
        ),
        (Architecture::S390x, Endianness::Big, &[ArchiveKind::Gnu]),
        (Architecture::Sbf, Endianness::Little, &[ArchiveKind::Gnu]),
        (Architecture::Sparc64, Endianness::Big, &[ArchiveKind::Gnu]),
        (
            Architecture::Xtensa,
            Endianness::Little,
            &[ArchiveKind::Gnu],
        ),
    ]
    .iter()
    .copied()
    {
        for archive_kind in archive_kinds {
            let mut object = write::Object::new(BinaryFormat::Elf, arch, endian);

            let section = object.section_id(write::StandardSection::Data);
            object.append_section_data(section, &[1; 30], 4);
            let symbol = object.section_symbol(section);

            object
                .add_relocation(
                    section,
                    write::Relocation {
                        offset: 8,
                        size: 32,
                        kind: RelocationKind::Absolute,
                        encoding: RelocationEncoding::Generic,
                        symbol,
                        addend: 0,
                    },
                )
                .unwrap();
            if arch.address_size().unwrap().bytes() >= 8 {
                object
                    .add_relocation(
                        section,
                        write::Relocation {
                            offset: 16,
                            size: 64,
                            kind: RelocationKind::Absolute,
                            encoding: RelocationEncoding::Generic,
                            symbol,
                            addend: 0,
                        },
                    )
                    .unwrap();
            }

            round_trip_and_diff("elf_any", object, *archive_kind, 0);
        }
    }
}

#[test]
fn macho_x86_64() {
    let mut object = write::Object::new(
        BinaryFormat::MachO,
        Architecture::X86_64,
        Endianness::Little,
    );

    object.add_file_symbol(b"file.c".to_vec());

    let text = object.section_id(write::StandardSection::Text);
    object.append_section_data(text, &[1; 30], 4);

    let func1_offset = object.append_section_data(text, &[1; 30], 4);
    assert_eq!(func1_offset, 32);
    let func1_symbol = object.add_symbol(write::Symbol {
        name: b"func1".to_vec(),
        value: func1_offset,
        size: 32,
        kind: SymbolKind::Text,
        scope: SymbolScope::Linkage,
        weak: false,
        section: write::SymbolSection::Section(text),
        flags: SymbolFlags::None,
    });
    object
        .add_relocation(
            text,
            write::Relocation {
                offset: 8,
                size: 64,
                kind: RelocationKind::Absolute,
                encoding: RelocationEncoding::Generic,
                symbol: func1_symbol,
                addend: 0,
            },
        )
        .unwrap();
    object
        .add_relocation(
            text,
            write::Relocation {
                offset: 16,
                size: 32,
                kind: RelocationKind::Relative,
                encoding: RelocationEncoding::Generic,
                symbol: func1_symbol,
                addend: -4,
            },
        )
        .unwrap();

    round_trip_and_diff("macho_x86_64", object, ArchiveKind::Darwin, 0);
}

#[test]
fn macho_any() {
    // 32-bit object files get additional padding after the round-trip:
    // https://github.com/llvm/llvm-project/blob/3d3ef9d073e1e27ea57480b371b7f5a9f5642ed2/llvm/lib/Object/ArchiveWriter.cpp#L560-L565
    for (arch, endian, trim_output_bytes) in [
        (Architecture::Aarch64, Endianness::Little, 0),
        (Architecture::Aarch64_Ilp32, Endianness::Little, 4),
        /* TODO:
        (Architecture::Arm, Endianness::Little),
        */
        (Architecture::I386, Endianness::Little, 4),
        (Architecture::X86_64, Endianness::Little, 0),
        /* TODO:
        (Architecture::PowerPc, Endianness::Big),
        (Architecture::PowerPc64, Endianness::Big),
        */
    ]
    .iter()
    .copied()
    {
        let mut object = write::Object::new(BinaryFormat::MachO, arch, endian);

        let section = object.section_id(write::StandardSection::Data);
        object.append_section_data(section, &[1; 30], 4);
        let symbol = object.section_symbol(section);

        object
            .add_relocation(
                section,
                write::Relocation {
                    offset: 8,
                    size: 32,
                    kind: RelocationKind::Absolute,
                    encoding: RelocationEncoding::Generic,
                    symbol,
                    addend: 0,
                },
            )
            .unwrap();
        if arch.address_size().unwrap().bytes() >= 8 {
            object
                .add_relocation(
                    section,
                    write::Relocation {
                        offset: 16,
                        size: 64,
                        kind: RelocationKind::Absolute,
                        encoding: RelocationEncoding::Generic,
                        symbol,
                        addend: 0,
                    },
                )
                .unwrap();
        }

        round_trip_and_diff("macho_any", object, ArchiveKind::Darwin, trim_output_bytes);
    }
}

#[test]
fn xcoff_powerpc() {
    for arch in [Architecture::PowerPc, Architecture::PowerPc64] {
        let mut object = write::Object::new(BinaryFormat::Xcoff, arch, Endianness::Big);

        object.add_file_symbol(b"file.c".to_vec());

        let text = object.section_id(write::StandardSection::Text);
        object.append_section_data(text, &[1; 30], 4);

        let func1_offset = object.append_section_data(text, &[1; 30], 4);
        assert_eq!(func1_offset, 32);
        let func1_symbol = object.add_symbol(write::Symbol {
            name: b"func1".to_vec(),
            value: func1_offset,
            size: 32,
            kind: SymbolKind::Text,
            scope: SymbolScope::Linkage,
            weak: false,
            section: write::SymbolSection::Section(text),
            flags: SymbolFlags::None,
        });

        object
            .add_relocation(
                text,
                write::Relocation {
                    offset: 8,
                    size: 64,
                    kind: RelocationKind::Absolute,
                    encoding: RelocationEncoding::Generic,
                    symbol: func1_symbol,
                    addend: 0,
                },
            )
            .unwrap();

        round_trip_and_diff("xcoff_powerpc", object, ArchiveKind::Gnu, 0);
    }
}
