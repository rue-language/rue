#![allow(rustc::default_hash_types, rustc::potential_query_instability)]

// Derived from code in LLVM, which is:
// Part of the LLVM Project, under the Apache License v2.0 with LLVM Exceptions.
// See https://llvm.org/LICENSE.txt for license information.
// SPDX-License-Identifier: Apache-2.0 WITH LLVM-exception

// Derived from:
// * https://github.com/llvm/llvm-project/blob/3d3ef9d073e1e27ea57480b371b7f5a9f5642ed2/llvm/include/llvm/Object/ArchiveWriter.h
// * https://github.com/llvm/llvm-project/blob/3d3ef9d073e1e27ea57480b371b7f5a9f5642ed2/llvm/lib/Object/ArchiveWriter.cpp

use std::collections::HashMap;
use std::io::{self, Cursor, Seek, Write};

use object::{Object, ObjectSymbol};

use crate::alignment::*;
use crate::archive::*;

pub struct NewArchiveMember<'a> {
    pub buf: Box<dyn AsRef<[u8]> + 'a>,
    pub get_symbols: fn(buf: &[u8], f: &mut dyn FnMut(&[u8]) -> io::Result<()>) -> io::Result<bool>,
    pub member_name: String,
    pub mtime: u64,
    pub uid: u32,
    pub gid: u32,
    pub perms: u32,
}

fn is_darwin(kind: ArchiveKind) -> bool {
    matches!(kind, ArchiveKind::Darwin | ArchiveKind::Darwin64)
}

fn is_aix_big_archive(kind: ArchiveKind) -> bool {
    kind == ArchiveKind::AixBig
}

fn is_bsd_like(kind: ArchiveKind) -> bool {
    match kind {
        ArchiveKind::Gnu | ArchiveKind::Gnu64 | ArchiveKind::AixBig => false,
        ArchiveKind::Bsd | ArchiveKind::Darwin | ArchiveKind::Darwin64 => true,
        ArchiveKind::Coff => panic!("not supported for writing"),
    }
}

fn print_rest_of_member_header<W: Write>(
    w: &mut W,
    mtime: u64,
    uid: u32,
    gid: u32,
    perms: u32,
    size: u64,
) -> io::Result<()> {
    // The format has only 6 chars for uid and gid. Truncate if the provided
    // values don't fit.
    write!(
        w,
        "{:<12}{:<6}{:<6}{:<8o}{:<10}`\n",
        mtime,
        uid % 1000000,
        gid % 1000000,
        perms,
        size
    )
}

fn print_gnu_small_member_header<W: Write>(
    w: &mut W,
    name: String,
    mtime: u64,
    uid: u32,
    gid: u32,
    perms: u32,
    size: u64,
) -> io::Result<()> {
    write!(w, "{:<16}", name + "/")?;
    print_rest_of_member_header(w, mtime, uid, gid, perms, size)
}

fn print_bsd_member_header<W: Write>(
    w: &mut W,
    pos: u64,
    name: &str,
    mtime: u64,
    uid: u32,
    gid: u32,
    perms: u32,
    size: u64,
) -> io::Result<()> {
    let pos_after_header = pos + 60 + u64::try_from(name.len()).unwrap();
    // Pad so that even 64 bit object files are aligned.
    let pad = offset_to_alignment(pos_after_header, 8);
    let name_with_padding = u64::try_from(name.len()).unwrap() + pad;
    write!(w, "#1/{:<13}", name_with_padding)?;
    print_rest_of_member_header(w, mtime, uid, gid, perms, name_with_padding + size)?;
    write!(w, "{}", name)?;
    write!(
        w,
        "{nil:\0<pad$}",
        nil = "",
        pad = usize::try_from(pad).unwrap()
    )
}

fn print_big_archive_member_header<W: Write>(
    w: &mut W,
    name: &str,
    mtime: u64,
    uid: u32,
    gid: u32,
    perms: u32,
    size: u64,
    prev_offset: u64,
    next_offset: u64,
) -> io::Result<()> {
    write!(
        w,
        "{:<20}{:<20}{:<20}{:<12}{:<12}{:<12}{:<12o}{:<4}",
        size,
        next_offset,
        prev_offset,
        mtime,
        u64::from(uid) % 1000000000000u64,
        u64::from(gid) % 1000000000000u64,
        perms,
        name.len(),
    )?;

    if !name.is_empty() {
        write!(w, "{}", name)?;

        if name.len() % 2 != 0 {
            write!(w, "\0")?;
        }
    }

    write!(w, "`\n")?;

    Ok(())
}

fn use_string_table(thin: bool, name: &str) -> bool {
    thin || name.len() >= 16 || name.contains('/')
}

fn is_64bit_kind(kind: ArchiveKind) -> bool {
    match kind {
        ArchiveKind::Gnu | ArchiveKind::Bsd | ArchiveKind::Darwin | ArchiveKind::Coff => false,
        ArchiveKind::AixBig | ArchiveKind::Darwin64 | ArchiveKind::Gnu64 => true,
    }
}

fn print_member_header<'m, W: Write, T: Write + Seek>(
    w: &mut W,
    pos: u64,
    string_table: &mut T,
    member_names: &mut HashMap<&'m str, u64>,
    kind: ArchiveKind,
    thin: bool,
    m: &'m NewArchiveMember<'m>,
    mtime: u64,
    size: u64,
) -> io::Result<()> {
    if is_bsd_like(kind) {
        return print_bsd_member_header(w, pos, &m.member_name, mtime, m.uid, m.gid, m.perms, size);
    }

    if !use_string_table(thin, &m.member_name) {
        return print_gnu_small_member_header(
            w,
            m.member_name.clone(),
            mtime,
            m.uid,
            m.gid,
            m.perms,
            size,
        );
    }

    write!(w, "/")?;
    let name_pos;
    if thin {
        name_pos = string_table.stream_position()?;
        write!(string_table, "{}/\n", m.member_name)?;
    } else {
        if let Some(&pos) = member_names.get(&*m.member_name) {
            name_pos = pos;
        } else {
            name_pos = string_table.stream_position()?;
            member_names.insert(&m.member_name, name_pos);
            write!(string_table, "{}/\n", m.member_name)?;
        }
    }
    write!(w, "{:<15}", name_pos)?;
    print_rest_of_member_header(w, mtime, m.uid, m.gid, m.perms, size)
}

struct MemberData<'a> {
    symbols: Vec<u64>,
    header: Vec<u8>,
    data: &'a [u8],
    padding: &'static [u8],
}

fn compute_string_table(names: &[u8]) -> MemberData<'_> {
    let size = u64::try_from(names.len()).unwrap();
    let pad = offset_to_alignment(size, 2);
    let mut header = Vec::new();
    write!(header, "{:<48}", "//").unwrap();
    write!(header, "{:<10}", size + pad).unwrap();
    write!(header, "`\n").unwrap();
    MemberData {
        symbols: vec![],
        header,
        data: names,
        padding: if pad != 0 { b"\n" } else { b"" },
    }
}

const fn now() -> u64 {
    0
}

fn is_archive_symbol(sym: &object::read::Symbol<'_, '_>) -> bool {
    // FIXME Use a better equivalent of LLVM's SymbolRef::SF_FormatSpecific
    if sym.kind() == object::SymbolKind::Null
        || sym.kind() == object::SymbolKind::File
        || sym.kind() == object::SymbolKind::Section
    {
        return false;
    }
    if !sym.is_global() {
        return false;
    }
    if sym.is_undefined() {
        return false;
    }
    true
}

fn print_n_bits<W: Write>(w: &mut W, kind: ArchiveKind, val: u64) -> io::Result<()> {
    if is_64bit_kind(kind) {
        w.write_all(&if is_bsd_like(kind) {
            u64::to_le_bytes(val)
        } else {
            u64::to_be_bytes(val)
        })
    } else {
        w.write_all(&if is_bsd_like(kind) {
            u32::to_le_bytes(u32::try_from(val).unwrap())
        } else {
            u32::to_be_bytes(u32::try_from(val).unwrap())
        })
    }
}

fn compute_symbol_table_size_and_pad(
    kind: ArchiveKind,
    num_syms: u64,
    offset_size: u64,
    string_table: &[u8],
) -> (u64, u64) {
    assert!(
        offset_size == 4 || offset_size == 8,
        "Unsupported offset_size"
    );
    let mut size = offset_size; // Number of entries
    if is_bsd_like(kind) {
        size += num_syms * offset_size * 2; // Table
    } else {
        size += num_syms * offset_size; // Table
    }
    if is_bsd_like(kind) {
        size += offset_size; // byte count;
    }
    size += u64::try_from(string_table.len()).unwrap();
    // ld64 expects the members to be 8-byte aligned for 64-bit content and at
    // least 4-byte aligned for 32-bit content.  Opt for the larger encoding
    // uniformly.
    // We do this for all bsd formats because it simplifies aligning members.
    let pad = if is_aix_big_archive(kind) {
        0
    } else {
        offset_to_alignment(size, if is_bsd_like(kind) { 8 } else { 2 })
    };
    size += pad;
    (size, pad)
}

fn write_symbol_table_header<W: Write + Seek>(
    w: &mut W,
    kind: ArchiveKind,
    size: u64,
    prev_member_offset: u64,
) -> io::Result<()> {
    if is_bsd_like(kind) {
        let name = if is_64bit_kind(kind) {
            "__.SYMDEF_64"
        } else {
            "__.SYMDEF"
        };
        let pos = w.stream_position()?;
        print_bsd_member_header(w, pos, name, now(), 0, 0, 0, size)
    } else if is_aix_big_archive(kind) {
        print_big_archive_member_header(w, "", now(), 0, 0, 0, size, prev_member_offset, 0)
    } else {
        let name = if is_64bit_kind(kind) { "/SYM64" } else { "" };
        print_gnu_small_member_header(w, name.to_string(), now(), 0, 0, 0, size)
    }
}

fn write_symbol_table<W: Write + Seek>(
    w: &mut W,
    kind: ArchiveKind,
    members: &[MemberData<'_>],
    string_table: &[u8],
    prev_member_offset: u64,
) -> io::Result<()> {
    // We don't write a symbol table on an archive with no members -- except on
    // Darwin, where the linker will abort unless the archive has a symbol table.
    if string_table.is_empty() && !is_darwin(kind) {
        return Ok(());
    }

    let num_syms = u64::try_from(members.iter().map(|m| m.symbols.len()).sum::<usize>()).unwrap();

    let offset_size = if is_64bit_kind(kind) { 8 } else { 4 };
    let (size, pad) = compute_symbol_table_size_and_pad(kind, num_syms, offset_size, string_table);
    write_symbol_table_header(w, kind, size, prev_member_offset)?;

    let mut pos = if is_aix_big_archive(kind) {
        u64::try_from(std::mem::size_of::<big_archive::FixLenHdr>()).unwrap()
    } else {
        w.stream_position()? + size
    };

    if is_bsd_like(kind) {
        print_n_bits(w, kind, num_syms * 2 * offset_size)?;
    } else {
        print_n_bits(w, kind, num_syms)?;
    }

    for m in members {
        for &string_offset in &m.symbols {
            if is_bsd_like(kind) {
                print_n_bits(w, kind, string_offset)?;
            }
            print_n_bits(w, kind, pos)?; // member offset
        }
        pos += u64::try_from(m.header.len() + m.data.len() + m.padding.len()).unwrap();
    }

    if is_bsd_like(kind) {
        // byte count of the string table
        print_n_bits(w, kind, u64::try_from(string_table.len()).unwrap())?;
    }

    w.write_all(string_table)?;

    write!(
        w,
        "{nil:\0<pad$}",
        nil = "",
        pad = usize::try_from(pad).unwrap()
    )
}

pub fn get_native_object_symbols(
    buf: &[u8],
    f: &mut dyn FnMut(&[u8]) -> io::Result<()>,
) -> io::Result<bool> {
    // FIXME match what LLVM does

    match object::File::parse(buf) {
        Ok(file) => {
            for sym in file.symbols() {
                if !is_archive_symbol(&sym) {
                    continue;
                }
                f(sym.name_bytes().expect("FIXME"))?;
            }
            Ok(true)
        }
        Err(_) => Ok(false),
    }
}

// NOTE: LLVM calls this getSymbols and has the get_native_symbols function inlined
fn write_symbols(
    buf: &[u8],
    get_symbols: fn(buf: &[u8], f: &mut dyn FnMut(&[u8]) -> io::Result<()>) -> io::Result<bool>,
    sym_names: &mut Cursor<Vec<u8>>,
    has_object: &mut bool,
) -> io::Result<Vec<u64>> {
    let mut ret = vec![];
    // We only set has_object if get_symbols determines it's looking at an
    // object file. This is because if we're creating an rlib, the archive will
    // always end in lib.rmeta, and cause has_object to always become false.
    if get_symbols(buf, &mut |sym| {
        ret.push(sym_names.stream_position()?);
        sym_names.write_all(sym)?;
        sym_names.write_all(&[0])?;
        Ok(())
    })? {
        *has_object = true;
    }
    Ok(ret)
}

fn compute_member_data<'a, S: Write + Seek>(
    string_table: &mut S,
    sym_names: &mut Cursor<Vec<u8>>,
    kind: ArchiveKind,
    thin: bool,
    new_members: &'a [NewArchiveMember<'a>],
) -> io::Result<Vec<MemberData<'a>>> {
    const PADDING_DATA: &[u8; 8] = &[b'\n'; 8];

    // This ignores the symbol table, but we only need the value mod 8 and the
    // symbol table is aligned to be a multiple of 8 bytes
    let mut pos = if is_aix_big_archive(kind) {
        u64::try_from(std::mem::size_of::<big_archive::FixLenHdr>()).unwrap()
    } else {
        0
    };

    let mut ret = vec![];
    let mut has_object = false;

    // Deduplicate long member names in the string table and reuse earlier name
    // offsets. This especially saves space for COFF Import libraries where all
    // members have the same name.
    let mut member_names = HashMap::<&str, u64>::new();

    // UniqueTimestamps is a special case to improve debugging on Darwin:
    //
    // The Darwin linker does not link debug info into the final
    // binary. Instead, it emits entries of type N_OSO in in the output
    // binary's symbol table, containing references to the linked-in
    // object files. Using that reference, the debugger can read the
    // debug data directly from the object files. Alternatively, an
    // invocation of 'dsymutil' will link the debug data from the object
    // files into a dSYM bundle, which can be loaded by the debugger,
    // instead of the object files.
    //
    // For an object file, the N_OSO entries contain the absolute path
    // path to the file, and the file's timestamp. For an object
    // included in an archive, the path is formatted like
    // "/absolute/path/to/archive.a(member.o)", and the timestamp is the
    // archive member's timestamp, rather than the archive's timestamp.
    //
    // However, this doesn't always uniquely identify an object within
    // an archive -- an archive file can have multiple entries with the
    // same filename. (This will happen commonly if the original object
    // files started in different directories.) The only way they get
    // distinguished, then, is via the timestamp. But this process is
    // unable to find the correct object file in the archive when there
    // are two files of the same name and timestamp.
    //
    // Additionally, timestamp==0 is treated specially, and causes the
    // timestamp to be ignored as a match criteria.
    //
    // That will "usually" work out okay when creating an archive not in
    // deterministic timestamp mode, because the objects will probably
    // have been created at different timestamps.
    //
    // To ameliorate this problem, in deterministic archive mode (which
    // is the default), on Darwin we will emit a unique non-zero
    // timestamp for each entry with a duplicated name. This is still
    // deterministic: the only thing affecting that timestamp is the
    // order of the files in the resultant archive.
    //
    // See also the functions that handle the lookup:
    // in lldb: ObjectContainerBSDArchive::Archive::FindObject()
    // in llvm/tools/dsymutil: BinaryHolder::GetArchiveMemberBuffers().
    let unique_timestamps = is_darwin(kind);
    let mut filename_count = HashMap::new();
    if unique_timestamps {
        for m in new_members {
            *filename_count.entry(&*m.member_name).or_insert(0) += 1;
        }
        for (_name, count) in filename_count.iter_mut() {
            *count = if *count > 1 { 1 } else { 0 };
        }
    }

    // The big archive format needs to know the offset of the previous member
    // header.
    let mut prev_offset = 0;
    for m in new_members {
        let mut header = Vec::new();

        let buf = m.buf.as_ref().as_ref();
        let data = if thin { &[][..] } else { buf };

        // ld64 expects the members to be 8-byte aligned for 64-bit content and at
        // least 4-byte aligned for 32-bit content.  Opt for the larger encoding
        // uniformly.  This matches the behaviour with cctools and ensures that ld64
        // is happy with archives that we generate.
        let member_padding = if is_darwin(kind) {
            offset_to_alignment(u64::try_from(data.len()).unwrap(), 8)
        } else {
            0
        };
        let tail_padding =
            offset_to_alignment(u64::try_from(data.len()).unwrap() + member_padding, 2);
        let padding = &PADDING_DATA[..usize::try_from(member_padding + tail_padding).unwrap()];

        let mtime = if unique_timestamps {
            // Increment timestamp for each file of a given name.
            *filename_count.get_mut(&*m.member_name).unwrap() += 1;
            filename_count[&*m.member_name] - 1
        } else {
            m.mtime
        };

        let size = u64::try_from(buf.len()).unwrap() + member_padding;
        if size > MAX_MEMBER_SIZE {
            return Err(io::Error::new(
                io::ErrorKind::Other,
                format!("Archive member {} is too big", m.member_name),
            ));
        }

        if is_aix_big_archive(kind) {
            let next_offset = pos
                + u64::try_from(std::mem::size_of::<big_archive::BigArMemHdrType>()).unwrap()
                + align_to(u64::try_from(m.member_name.len()).unwrap(), 2)
                + align_to(size, 2);
            print_big_archive_member_header(
                &mut header,
                &m.member_name,
                mtime,
                m.uid,
                m.gid,
                m.perms,
                size,
                prev_offset,
                next_offset,
            )?;
            prev_offset = pos;
        } else {
            print_member_header(
                &mut header,
                pos,
                string_table,
                &mut member_names,
                kind,
                thin,
                m,
                mtime,
                size,
            )?;
        }

        let symbols = write_symbols(buf, m.get_symbols, sym_names, &mut has_object)?;

        pos += u64::try_from(header.len() + data.len() + padding.len()).unwrap();
        ret.push(MemberData {
            symbols,
            header,
            data,
            padding,
        })
    }

    // If there are no symbols, emit an empty symbol table, to satisfy Solaris
    // tools, older versions of which expect a symbol table in a non-empty
    // archive, regardless of whether there are any symbols in it.
    if has_object && sym_names.stream_position()? == 0 {
        write!(sym_names, "\0\0\0")?;
    }

    Ok(ret)
}

pub fn write_archive_to_stream<W: Write + Seek>(
    w: &mut W,
    new_members: &[NewArchiveMember<'_>],
    mut kind: ArchiveKind,
    thin: bool,
) -> io::Result<()> {
    assert!(
        !thin || !is_bsd_like(kind),
        "Only the gnu format has a thin mode"
    );

    let mut sym_names = Cursor::new(Vec::new());
    let mut string_table = Cursor::new(Vec::new());

    let mut data = compute_member_data(&mut string_table, &mut sym_names, kind, thin, new_members)?;

    let sym_names = sym_names.into_inner();

    let string_table = string_table.into_inner();
    if !string_table.is_empty() && !is_aix_big_archive(kind) {
        data.insert(0, compute_string_table(&string_table));
    }

    // We would like to detect if we need to switch to a 64-bit symbol table.
    let mut last_member_end_offset = if is_aix_big_archive(kind) {
        u64::try_from(std::mem::size_of::<big_archive::FixLenHdr>()).unwrap()
    } else {
        8
    };
    let mut last_member_header_offset = last_member_end_offset;
    let mut num_syms = 0;
    for m in &data {
        // Record the start of the member's offset
        last_member_header_offset = last_member_end_offset;
        // Account for the size of each part associated with the member.
        last_member_end_offset +=
            u64::try_from(m.header.len() + m.data.len() + m.padding.len()).unwrap();
        num_syms += u64::try_from(m.symbols.len()).unwrap();
    }

    // The symbol table is put at the end of the big archive file. The symbol
    // table is at the start of the archive file for other archive formats.
    if !is_aix_big_archive(kind) {
        // We assume 32-bit offsets to see if 32-bit symbols are possible or not.
        let (symtab_size, _pad) = compute_symbol_table_size_and_pad(kind, num_syms, 4, &sym_names);
        last_member_header_offset += {
            // FIXME avoid allocating memory here
            let mut tmp = Cursor::new(vec![]);
            write_symbol_table_header(&mut tmp, kind, symtab_size, 0).unwrap();
            u64::try_from(tmp.into_inner().len()).unwrap()
        } + symtab_size;

        // The SYM64 format is used when an archive's member offsets are larger than
        // 32-bits can hold. The need for this shift in format is detected by
        // writeArchive. To test this we need to generate a file with a member that
        // has an offset larger than 32-bits but this demands a very slow test. To
        // speed the test up we use this environment variable to pretend like the
        // cutoff happens before 32-bits and instead happens at some much smaller
        // value.
        // FIXME allow lowering the threshold for tests
        const SYM64_THRESHOLD: u64 = 1 << 32;

        // If LastMemberHeaderOffset isn't going to fit in a 32-bit varible we need
        // to switch to 64-bit. Note that the file can be larger than 4GB as long as
        // the last member starts before the 4GB offset.
        if last_member_header_offset >= SYM64_THRESHOLD {
            if kind == ArchiveKind::Darwin {
                kind = ArchiveKind::Darwin64;
            } else {
                kind = ArchiveKind::Gnu64;
            }
        }
    }

    if thin {
        write!(w, "!<thin>\n")?;
    } else if is_aix_big_archive(kind) {
        write!(w, "<bigaf>\n")?;
    } else {
        write!(w, "!<arch>\n")?;
    }

    if !is_aix_big_archive(kind) {
        write_symbol_table(w, kind, &data, &sym_names, 0)?;

        for m in data {
            w.write_all(&m.header)?;
            w.write_all(m.data)?;
            w.write_all(m.padding)?;
        }
    } else {
        // For the big archive (AIX) format, compute a table of member names and
        // offsets, used in the member table.
        let mut member_table_name_str_tbl_size = 0;
        let mut member_offsets = vec![];
        let mut member_names = vec![];

        // Loop across object to find offset and names.
        let mut member_end_offset =
            u64::try_from(std::mem::size_of::<big_archive::FixLenHdr>()).unwrap();
        for i in 0..new_members.len() {
            let member = &new_members[i];
            member_table_name_str_tbl_size += member.member_name.len() + 1;
            member_offsets.push(member_end_offset);
            member_names.push(&member.member_name);
            // File member name ended with "`\n". The length is included in
            // BigArMemHdrType.
            member_end_offset += u64::try_from(std::mem::size_of::<big_archive::BigArMemHdrType>())
                .unwrap()
                + align_to(u64::try_from(data[i].data.len()).unwrap(), 2)
                + align_to(u64::try_from(member.member_name.len()).unwrap(), 2);
        }

        // AIX member table size.
        let member_table_size =
            u64::try_from(20 + 20 * member_offsets.len() + member_table_name_str_tbl_size).unwrap();

        let global_symbol_offset = if num_syms > 0 {
            last_member_end_offset
                + align_to(
                    u64::try_from(std::mem::size_of::<big_archive::BigArMemHdrType>()).unwrap()
                        + member_table_size,
                    2,
                )
        } else {
            0
        };

        // Fixed Sized Header.
        // Offset to member table
        write!(
            w,
            "{:<20}",
            if !new_members.is_empty() {
                last_member_end_offset
            } else {
                0
            }
        )?;
        // If there are no file members in the archive, there will be no global
        // symbol table.
        write!(
            w,
            "{:<20}",
            if !new_members.is_empty() {
                global_symbol_offset
            } else {
                0
            }
        )?;
        // Offset to 64 bits global symbol table - Not supported yet
        write!(w, "{:<20}", 0)?;
        // Offset to first archive member
        write!(
            w,
            "{:<20}",
            if !new_members.is_empty() {
                u64::try_from(std::mem::size_of::<big_archive::FixLenHdr>()).unwrap()
            } else {
                0
            }
        )?;
        // Offset to last archive member
        write!(
            w,
            "{:<20}",
            if !new_members.is_empty() {
                last_member_header_offset
            } else {
                0
            }
        )?;
        // Offset to first member of free list - Not supported yet
        write!(w, "{:<20}", 0)?;

        for m in &data {
            w.write_all(&m.header)?;
            w.write_all(m.data)?;
            if m.data.len() % 2 != 0 {
                w.write_all(&[0])?;
            }
        }

        if !new_members.is_empty() {
            // Member table.
            print_big_archive_member_header(
                w,
                "",
                0,
                0,
                0,
                0,
                member_table_size,
                last_member_header_offset,
                global_symbol_offset,
            )?;
            write!(w, "{:<20}", member_offsets.len())?; // Number of members
            for member_offset in member_offsets {
                write!(w, "{:<20}", member_offset)?;
            }
            for member_name in member_names {
                w.write_all(member_name.as_bytes())?;
                w.write_all(&[0])?;
            }

            if member_table_name_str_tbl_size % 2 != 0 {
                // Name table must be tail padded to an even number of
                // bytes.
                w.write_all(&[0])?;
            }

            if num_syms > 0 {
                write_symbol_table(w, kind, &data, &sym_names, last_member_end_offset)?;
            }
        }
    }

    w.flush()
}
