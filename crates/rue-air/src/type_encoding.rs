//! Authoritative compact encoding for live [`Type`](crate::Type) handles.
//!
//! This module is the only place that assigns primitive values, composite
//! tags, payload widths, or reserved ranges. Construction and decoding must go
//! through these helpers so a raw value cannot acquire different meanings in
//! different consumers.

pub(crate) const TAG_MASK: u32 = 0xff;
pub(crate) const PAYLOAD_SHIFT: u32 = 8;
pub(crate) const MAX_PAYLOAD: u32 = u32::MAX >> PAYLOAD_SHIFT;
pub(crate) const RESERVED_AFTER_PRIMITIVES_START: u32 = 16;
pub(crate) const RESERVED_AFTER_PRIMITIVES_END: u32 = 99;
pub(crate) const RESERVED_AFTER_COMPOSITES_START: u32 = 106;
pub(crate) const RESERVED_AFTER_COMPOSITES_END: u32 = TAG_MASK;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum Primitive {
    I8 = 0,
    I16 = 1,
    I32 = 2,
    I64 = 3,
    U8 = 4,
    U16 = 5,
    U32 = 6,
    U64 = 7,
    Bool = 8,
    Unit = 9,
    Error = 10,
    Never = 11,
    ComptimeType = 12,
    F32 = 13,
    F64 = 14,
    ComptimeFloat = 15,
}

impl Primitive {
    pub(crate) const fn encode(self) -> u32 {
        self as u32
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum Composite {
    Struct = 100,
    Enum = 101,
    Array = 102,
    Module = 103,
    PtrConst = 104,
    PtrMut = 105,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Decoded {
    Primitive(Primitive),
    Composite { kind: Composite, payload: u32 },
}

pub(crate) const fn encode_composite(kind: Composite, payload: u32) -> Option<u32> {
    if payload > MAX_PAYLOAD {
        return None;
    }
    Some((payload << PAYLOAD_SHIFT) | kind as u32)
}

pub(crate) const fn decode(raw: u32) -> Option<Decoded> {
    // Primitive encodings have no payload. Checking the entire raw value here
    // prevents values such as 0x100 from silently decoding as `i8`.
    let primitive = match raw {
        0 => Some(Primitive::I8),
        1 => Some(Primitive::I16),
        2 => Some(Primitive::I32),
        3 => Some(Primitive::I64),
        4 => Some(Primitive::U8),
        5 => Some(Primitive::U16),
        6 => Some(Primitive::U32),
        7 => Some(Primitive::U64),
        8 => Some(Primitive::Bool),
        9 => Some(Primitive::Unit),
        10 => Some(Primitive::Error),
        11 => Some(Primitive::Never),
        12 => Some(Primitive::ComptimeType),
        13 => Some(Primitive::F32),
        14 => Some(Primitive::F64),
        15 => Some(Primitive::ComptimeFloat),
        _ => None,
    };
    if let Some(primitive) = primitive {
        return Some(Decoded::Primitive(primitive));
    }

    let tag = raw & TAG_MASK;
    if (tag >= RESERVED_AFTER_PRIMITIVES_START && tag <= RESERVED_AFTER_PRIMITIVES_END)
        || (tag >= RESERVED_AFTER_COMPOSITES_START && tag <= RESERVED_AFTER_COMPOSITES_END)
    {
        return None;
    }

    let kind = match tag {
        100 => Composite::Struct,
        101 => Composite::Enum,
        102 => Composite::Array,
        103 => Composite::Module,
        104 => Composite::PtrConst,
        105 => Composite::PtrMut,
        _ => return None,
    };
    Some(Decoded::Composite {
        kind,
        payload: raw >> PAYLOAD_SHIFT,
    })
}

pub(crate) const fn has_composite_kind(raw: u32, expected: Composite) -> bool {
    matches!(
        decode(raw),
        Some(Decoded::Composite { kind, .. }) if kind as u8 == expected as u8
    )
}
