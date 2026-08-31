//! The typed authority for intrinsic semantics.
//!
//! An intrinsic's spelling is retained on AIR for diagnostics and stable
//! presentation, but this operation is the only value consumed by later
//! phases.  In particular, runtime helper identity is derived from this value
//! rather than carried in a second optional channel.

use crate::{
    Air, AirArgMode, AirInstData, AirProjection, AirRef, EnumDef, FrozenTypeInternPool,
    RuntimeAirArgument, RuntimeAirType, RuntimeCallKind, Type, TypeInternPool, TypeKind,
};

/// The structural origin of an AIR intrinsic operand.
///
/// Most intrinsics consume ordinary rvalues. Address-taking intrinsics instead
/// require a durable place read, and `@field_ptr` additionally requires that
/// the read's final projection select a field. Keeping this evidence beside
/// the operand's type and mode lets durable import and CFG construction apply
/// one complete contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntrinsicAirArgumentSource {
    Value,
    Load,
    Param,
    PlaceRead { terminal_field: bool },
}

/// One argument presented to the shared operation-level AIR validator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IntrinsicAirArgument {
    pub ty: Type,
    pub mode: AirArgMode,
    pub source: IntrinsicAirArgumentSource,
}

impl IntrinsicAirArgument {
    /// Describe an ordinary computed value. Tests and post-AIR consumers that
    /// do not carry a place operand use this constructor.
    pub const fn value(ty: Type, mode: AirArgMode) -> Self {
        Self {
            ty,
            mode,
            source: IntrinsicAirArgumentSource::Value,
        }
    }

    #[cfg(test)]
    const fn load(ty: Type, mode: AirArgMode) -> Self {
        Self {
            ty,
            mode,
            source: IntrinsicAirArgumentSource::Load,
        }
    }

    #[cfg(test)]
    const fn field_place_read(ty: Type, mode: AirArgMode) -> Self {
        Self {
            ty,
            mode,
            source: IntrinsicAirArgumentSource::PlaceRead {
                terminal_field: true,
            },
        }
    }
}

/// Build the exact operand descriptor consumed by [`IntrinsicOperation::validate_call`].
/// Both durable import and CFG construction call this function on their AIR,
/// so provenance cannot drift between the two trust boundaries.
pub fn intrinsic_air_argument_with_place_lookup(
    air: &Air,
    value: AirRef,
    mode: AirArgMode,
    terminal_field: impl FnOnce(crate::AirPlaceRef) -> bool,
) -> IntrinsicAirArgument {
    let instruction = air.get(value);
    let source = match &instruction.data {
        AirInstData::Load { .. } => IntrinsicAirArgumentSource::Load,
        AirInstData::Param { .. } => IntrinsicAirArgumentSource::Param,
        AirInstData::PlaceRead { place } => IntrinsicAirArgumentSource::PlaceRead {
            terminal_field: terminal_field(*place),
        },
        _ => IntrinsicAirArgumentSource::Value,
    };
    IntrinsicAirArgument {
        ty: instruction.ty,
        mode,
        source,
    }
}

/// Build an intrinsic operand descriptor from a fully constructed AIR owner.
pub fn intrinsic_air_argument(air: &Air, value: AirRef, mode: AirArgMode) -> IntrinsicAirArgument {
    intrinsic_air_argument_with_place_lookup(air, value, mode, |place| {
        let place = air.get_place(place);
        matches!(
            air.get_place_projections(place).last(),
            Some(AirProjection::Field { .. })
        )
    })
}

/// Type-pool operations needed to validate and marshal an intrinsic call.
/// Implementations for both mutable semantic-import pools and frozen CFG pools
/// keep the operation validator single-sourced across phase boundaries.
pub trait RuntimeAirTypePool {
    fn runtime_air_type(&self, ty: Type) -> Option<RuntimeAirType>;
    fn runtime_air_result_type(&self, ty: Type) -> Option<RuntimeAirType>;
    fn ptr_const_def(&self, id: crate::PtrConstTypeId) -> Type;
    fn ptr_mut_def(&self, id: crate::PtrMutTypeId) -> Type;
}

/// Convert an AIR value type to the compact type vocabulary used by the
/// runtime-call manifest.  This is shared by durable import and CFG so the
/// manifest remains the sole authority for runtime intrinsic call shapes.
fn runtime_air_type_in_pool(pool: &TypeInternPool, ty: Type) -> Option<RuntimeAirType> {
    if ty == Type::UNIT {
        return Some(RuntimeAirType::Unit);
    }
    if ty == Type::I64 {
        return Some(RuntimeAirType::I64);
    }
    if ty == Type::U64 {
        return Some(RuntimeAirType::U64);
    }
    if ty == Type::U32 {
        return Some(RuntimeAirType::U32);
    }
    if ty == Type::BOOL {
        return Some(RuntimeAirType::Bool);
    }
    if ty == Type::NEVER {
        return Some(RuntimeAirType::Never);
    }
    if ty.is_signed() {
        return Some(RuntimeAirType::SignedInteger);
    }
    if ty.is_unsigned() {
        return Some(RuntimeAirType::UnsignedInteger);
    }
    if let TypeKind::Struct(struct_id) = ty.kind() {
        let name: &str = &pool.struct_def(struct_id).name;
        if pool.is_strbuf(struct_id) || crate::is_string_view_struct_name(name) {
            return Some(RuntimeAirType::Text);
        }
    }
    if let Some(ptr) = ty.as_ptr_const()
        && pool.ptr_const_def(ptr) == Type::U8
    {
        return Some(RuntimeAirType::ConstBytePointer);
    }
    if let Some(ptr) = ty.as_ptr_mut() {
        return Some(if pool.ptr_mut_def(ptr) == Type::U8 {
            RuntimeAirType::MutBytePointer
        } else {
            RuntimeAirType::MutPointer
        });
    }
    None
}

/// Convert an intrinsic result type to the runtime manifest vocabulary.
fn runtime_air_result_type_in_pool(pool: &TypeInternPool, ty: Type) -> Option<RuntimeAirType> {
    if ty == Type::U8 {
        return Some(RuntimeAirType::U8);
    }
    if let TypeKind::Struct(struct_id) = ty.kind()
        && pool.is_strbuf(struct_id)
    {
        return Some(RuntimeAirType::StrBuf);
    }
    if let TypeKind::Enum(enum_id) = ty.kind() {
        let definition = pool.enum_def(enum_id);
        let payload = exact_option_payload(&definition)?;
        if let TypeKind::Struct(struct_id) = payload.kind()
            && pool.is_strbuf(struct_id)
        {
            return Some(RuntimeAirType::OptionStrBuf);
        }
        return Some(match payload {
            Type::I32 => RuntimeAirType::OptionI32,
            Type::I64 => RuntimeAirType::OptionI64,
            Type::U32 => RuntimeAirType::OptionU32,
            Type::U64 => RuntimeAirType::OptionU64,
            _ => return None,
        });
    }
    runtime_air_type_in_pool(pool, ty)
}

/// Return the payload of the one accepted Option layout. Runtime return
/// marshalling assumes a two-variant `Some(T) | None` enum, so accepting a
/// lookalike with extra variants or a payload-bearing `None` would make its
/// result-slot layout unsound.
fn exact_option_payload(definition: &EnumDef) -> Option<Type> {
    if definition.variant_count() != 2 {
        return None;
    }
    let mut some = None;
    let mut none = None;
    for (index, name) in definition.variants.iter().enumerate() {
        match name.as_ref() {
            "Some" if some.replace(index).is_none() => {}
            "None" if none.replace(index).is_none() => {}
            _ => return None,
        }
    }
    let some = some?;
    let none = none?;
    let [payload] = definition.variant_payload(some) else {
        return None;
    };
    if !definition.variant_payload(none).is_empty() {
        return None;
    }
    Some(*payload)
}

impl RuntimeAirTypePool for TypeInternPool {
    fn runtime_air_type(&self, ty: Type) -> Option<RuntimeAirType> {
        runtime_air_type_in_pool(self, ty)
    }

    fn runtime_air_result_type(&self, ty: Type) -> Option<RuntimeAirType> {
        runtime_air_result_type_in_pool(self, ty)
    }

    fn ptr_const_def(&self, id: crate::PtrConstTypeId) -> Type {
        self.ptr_const_def(id)
    }

    fn ptr_mut_def(&self, id: crate::PtrMutTypeId) -> Type {
        self.ptr_mut_def(id)
    }
}

impl RuntimeAirTypePool for FrozenTypeInternPool {
    fn runtime_air_type(&self, ty: Type) -> Option<RuntimeAirType> {
        if ty == Type::UNIT {
            return Some(RuntimeAirType::Unit);
        }
        if ty == Type::I64 {
            return Some(RuntimeAirType::I64);
        }
        if ty == Type::U64 {
            return Some(RuntimeAirType::U64);
        }
        if ty == Type::U32 {
            return Some(RuntimeAirType::U32);
        }
        if ty == Type::BOOL {
            return Some(RuntimeAirType::Bool);
        }
        if ty == Type::NEVER {
            return Some(RuntimeAirType::Never);
        }
        if ty.is_signed() {
            return Some(RuntimeAirType::SignedInteger);
        }
        if ty.is_unsigned() {
            return Some(RuntimeAirType::UnsignedInteger);
        }
        if let TypeKind::Struct(struct_id) = ty.kind() {
            let name: &str = &self.struct_def(struct_id).name;
            if self.is_strbuf(struct_id) || crate::is_string_view_struct_name(name) {
                return Some(RuntimeAirType::Text);
            }
        }
        if let Some(ptr) = ty.as_ptr_const()
            && self.ptr_const_def(ptr) == Type::U8
        {
            return Some(RuntimeAirType::ConstBytePointer);
        }
        if let Some(ptr) = ty.as_ptr_mut() {
            return Some(if self.ptr_mut_def(ptr) == Type::U8 {
                RuntimeAirType::MutBytePointer
            } else {
                RuntimeAirType::MutPointer
            });
        }
        None
    }

    fn runtime_air_result_type(&self, ty: Type) -> Option<RuntimeAirType> {
        if ty == Type::U8 {
            return Some(RuntimeAirType::U8);
        }
        if let TypeKind::Struct(struct_id) = ty.kind()
            && self.is_strbuf(struct_id)
        {
            return Some(RuntimeAirType::StrBuf);
        }
        if let TypeKind::Enum(enum_id) = ty.kind() {
            let definition = self.enum_def(enum_id);
            let payload = exact_option_payload(definition)?;
            if let TypeKind::Struct(struct_id) = payload.kind()
                && self.is_strbuf(struct_id)
            {
                return Some(RuntimeAirType::OptionStrBuf);
            }
            return Some(match payload {
                Type::I32 => RuntimeAirType::OptionI32,
                Type::I64 => RuntimeAirType::OptionI64,
                Type::U32 => RuntimeAirType::OptionU32,
                Type::U64 => RuntimeAirType::OptionU64,
                _ => return None,
            });
        }
        self.runtime_air_type(ty)
    }

    fn ptr_const_def(&self, id: crate::PtrConstTypeId) -> Type {
        self.ptr_const_def(id)
    }

    fn ptr_mut_def(&self, id: crate::PtrMutTypeId) -> Type {
        self.ptr_mut_def(id)
    }
}

pub fn runtime_air_type(pool: &impl RuntimeAirTypePool, ty: Type) -> Option<RuntimeAirType> {
    pool.runtime_air_type(ty)
}

pub fn runtime_air_result_type(pool: &impl RuntimeAirTypePool, ty: Type) -> Option<RuntimeAirType> {
    pool.runtime_air_result_type(ty)
}

/// A total, semantically selected intrinsic operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IntrinsicOperation {
    PanicNoMessage,
    Panic,
    AssertFailed,
    AssertWithMessage,
    BoundsCheck,
    DebugI64,
    DebugU64,
    DebugBool,
    DebugStr,
    ReadLine,
    ParseI32,
    ParseI64,
    ParseU32,
    ParseU64,
    RandomU32,
    RandomU64,
    PtrToInt,
    IntToPtr,
    PtrRead,
    PtrReadUnaligned,
    PtrWrite,
    PtrWriteUnaligned,
    PtrOffset,
    Alloc,
    AllocZeroed,
    Free,
    Realloc,
    Resize,
    ByteCopy,
    ByteMove,
    ByteSet,
    ArgCount,
    ArgPtr,
    ArgLen,
    EnvCount,
    EnvPtr,
    EnvLen,
    Raw,
    RawMut,
    FieldPtr,
    Syscall,
    BitCast,
}

impl IntrinsicOperation {
    /// Every semantic identity, kept in one place so consumers and tests can
    /// prove that dispatch remains exhaustive when a new intrinsic is added.
    pub const ALL: [Self; 42] = [
        Self::PanicNoMessage,
        Self::Panic,
        Self::AssertFailed,
        Self::AssertWithMessage,
        Self::BoundsCheck,
        Self::DebugI64,
        Self::DebugU64,
        Self::DebugBool,
        Self::DebugStr,
        Self::ReadLine,
        Self::ParseI32,
        Self::ParseI64,
        Self::ParseU32,
        Self::ParseU64,
        Self::RandomU32,
        Self::RandomU64,
        Self::PtrToInt,
        Self::IntToPtr,
        Self::PtrRead,
        Self::PtrReadUnaligned,
        Self::PtrWrite,
        Self::PtrWriteUnaligned,
        Self::PtrOffset,
        Self::Alloc,
        Self::AllocZeroed,
        Self::Free,
        Self::Realloc,
        Self::Resize,
        Self::ByteCopy,
        Self::ByteMove,
        Self::ByteSet,
        Self::ArgCount,
        Self::ArgPtr,
        Self::ArgLen,
        Self::EnvCount,
        Self::EnvPtr,
        Self::EnvLen,
        Self::Raw,
        Self::RawMut,
        Self::FieldPtr,
        Self::Syscall,
        Self::BitCast,
    ];

    /// The canonical spelling used for diagnostics and display. This is not
    /// used for semantic dispatch (several operations intentionally share a
    /// spelling, such as the panic and debug families).
    pub const fn expected_spelling(self) -> &'static str {
        match self {
            Self::PanicNoMessage | Self::Panic => "panic",
            Self::AssertFailed | Self::AssertWithMessage => "assert",
            Self::BoundsCheck => "assert",
            Self::DebugI64 | Self::DebugU64 | Self::DebugBool | Self::DebugStr => "dbg",
            Self::ReadLine => "read_line",
            Self::ParseI32 => "parse_i32",
            Self::ParseI64 => "parse_i64",
            Self::ParseU32 => "parse_u32",
            Self::ParseU64 => "parse_u64",
            Self::RandomU32 => "random_u32",
            Self::RandomU64 => "random_u64",
            Self::PtrToInt => "ptr_to_int",
            Self::IntToPtr => "int_to_ptr",
            Self::PtrRead => "ptr_read",
            Self::PtrReadUnaligned => "ptr_read_unaligned",
            Self::PtrWrite => "ptr_write",
            Self::PtrWriteUnaligned => "ptr_write_unaligned",
            Self::PtrOffset => "ptr_offset",
            Self::Alloc => "alloc",
            Self::AllocZeroed => "alloc_zeroed",
            Self::Free => "free",
            Self::Realloc => "realloc",
            Self::Resize => "resize",
            Self::ByteCopy => "byte_copy",
            Self::ByteMove => "byte_move",
            Self::ByteSet => "byte_set",
            Self::ArgCount => "arg_count",
            Self::ArgPtr => "arg_ptr",
            Self::ArgLen => "arg_len",
            Self::EnvCount => "env_count",
            Self::EnvPtr => "env_ptr",
            Self::EnvLen => "env_len",
            Self::Raw => "raw",
            Self::RawMut => "raw_mut",
            Self::FieldPtr => "field_ptr",
            Self::Syscall => "syscall",
            Self::BitCast => "bitCast",
        }
    }

    pub fn from_runtime_call(runtime: RuntimeCallKind) -> Option<Self> {
        Some(match runtime {
            RuntimeCallKind::PanicNoMessage => Self::PanicNoMessage,
            RuntimeCallKind::Panic => Self::Panic,
            RuntimeCallKind::AssertFailed => Self::AssertFailed,
            RuntimeCallKind::AssertWithMessage => Self::AssertWithMessage,
            RuntimeCallKind::BoundsCheck => Self::BoundsCheck,
            RuntimeCallKind::DebugI64 => Self::DebugI64,
            RuntimeCallKind::DebugU64 => Self::DebugU64,
            RuntimeCallKind::DebugBool => Self::DebugBool,
            RuntimeCallKind::DebugStr => Self::DebugStr,
            RuntimeCallKind::ReadLine => Self::ReadLine,
            RuntimeCallKind::ParseI32 => Self::ParseI32,
            RuntimeCallKind::ParseI64 => Self::ParseI64,
            RuntimeCallKind::ParseU32 => Self::ParseU32,
            RuntimeCallKind::ParseU64 => Self::ParseU64,
            RuntimeCallKind::RandomU32 => Self::RandomU32,
            RuntimeCallKind::RandomU64 => Self::RandomU64,
            RuntimeCallKind::Alloc => Self::Alloc,
            RuntimeCallKind::AllocZeroed => Self::AllocZeroed,
            RuntimeCallKind::Free => Self::Free,
            RuntimeCallKind::Realloc => Self::Realloc,
            RuntimeCallKind::Resize => Self::Resize,
            RuntimeCallKind::ArgCount => Self::ArgCount,
            RuntimeCallKind::ArgPtr => Self::ArgPtr,
            RuntimeCallKind::ArgLen => Self::ArgLen,
            RuntimeCallKind::EnvCount => Self::EnvCount,
            RuntimeCallKind::EnvPtr => Self::EnvPtr,
            RuntimeCallKind::EnvLen => Self::EnvLen,
            RuntimeCallKind::ByteCopy => Self::ByteCopy,
            RuntimeCallKind::ByteMove => Self::ByteMove,
            RuntimeCallKind::ByteSet => Self::ByteSet,
            RuntimeCallKind::StrByteAt
            | RuntimeCallKind::StrCharScalar
            | RuntimeCallKind::StrCharNext
            | RuntimeCallKind::StrCharScalarLossy
            | RuntimeCallKind::StrCharNextLossy
            | RuntimeCallKind::ToString
            | RuntimeCallKind::ToStringUnsigned
            | RuntimeCallKind::StrPrintAggregate
            | RuntimeCallKind::StrPrintProjected
            | RuntimeCallKind::StrPrintlnAggregate
            | RuntimeCallKind::StrPrintlnProjected => return None,
        })
    }

    pub fn takes_place_address(self) -> bool {
        matches!(self, Self::Raw | Self::RawMut | Self::FieldPtr)
    }

    /// Runtime-backed operations retain their ABI identity in this enum. Pure
    /// operations intentionally return `None` and never acquire a runtime
    /// metadata side channel.
    pub fn runtime_call(self) -> Option<RuntimeCallKind> {
        Some(match self {
            Self::PanicNoMessage => RuntimeCallKind::PanicNoMessage,
            Self::Panic => RuntimeCallKind::Panic,
            Self::AssertFailed => RuntimeCallKind::AssertFailed,
            Self::AssertWithMessage => RuntimeCallKind::AssertWithMessage,
            Self::BoundsCheck => RuntimeCallKind::BoundsCheck,
            Self::ReadLine => RuntimeCallKind::ReadLine,
            Self::ParseI32 => RuntimeCallKind::ParseI32,
            Self::ParseI64 => RuntimeCallKind::ParseI64,
            Self::ParseU32 => RuntimeCallKind::ParseU32,
            Self::ParseU64 => RuntimeCallKind::ParseU64,
            Self::RandomU32 => RuntimeCallKind::RandomU32,
            Self::RandomU64 => RuntimeCallKind::RandomU64,
            Self::Alloc => RuntimeCallKind::Alloc,
            Self::AllocZeroed => RuntimeCallKind::AllocZeroed,
            Self::Free => RuntimeCallKind::Free,
            Self::Realloc => RuntimeCallKind::Realloc,
            Self::Resize => RuntimeCallKind::Resize,
            Self::ArgCount => RuntimeCallKind::ArgCount,
            Self::ArgPtr => RuntimeCallKind::ArgPtr,
            Self::ArgLen => RuntimeCallKind::ArgLen,
            Self::EnvCount => RuntimeCallKind::EnvCount,
            Self::EnvPtr => RuntimeCallKind::EnvPtr,
            Self::EnvLen => RuntimeCallKind::EnvLen,
            Self::ByteCopy => RuntimeCallKind::ByteCopy,
            Self::ByteMove => RuntimeCallKind::ByteMove,
            Self::ByteSet => RuntimeCallKind::ByteSet,
            Self::DebugI64 => RuntimeCallKind::DebugI64,
            Self::DebugU64 => RuntimeCallKind::DebugU64,
            Self::DebugBool => RuntimeCallKind::DebugBool,
            Self::DebugStr => RuntimeCallKind::DebugStr,
            Self::PtrToInt
            | Self::IntToPtr
            | Self::PtrRead
            | Self::PtrReadUnaligned
            | Self::PtrWrite
            | Self::PtrWriteUnaligned
            | Self::PtrOffset
            | Self::Raw
            | Self::RawMut
            | Self::FieldPtr
            | Self::Syscall
            | Self::BitCast => return None,
        })
    }

    /// Alias named after the ABI concept for callers that need to derive a
    /// runtime helper from the typed operation.
    pub fn runtime_call_kind(self) -> Option<RuntimeCallKind> {
        self.runtime_call()
    }

    pub fn panic_no_message(self) -> bool {
        matches!(self, Self::PanicNoMessage)
    }

    /// Validate an intrinsic's complete AIR call shape. Runtime-backed
    /// operations delegate to the RuntimeCallKind manifest; pure operations
    /// keep their pointer/provenance relationships here as well.
    pub fn validate_call(
        self,
        pool: &impl RuntimeAirTypePool,
        args: &[IntrinsicAirArgument],
        result: Type,
    ) -> bool {
        if args.iter().any(|arg| arg.mode != AirArgMode::Normal) {
            return false;
        }
        if let Some(runtime) = self.runtime_call_kind() {
            let Some(arguments) = args
                .iter()
                .map(|arg| {
                    runtime_air_type(pool, arg.ty)
                        .map(|ty| RuntimeAirArgument { ty, mode: arg.mode })
                })
                .collect::<Option<Vec<_>>>()
            else {
                return false;
            };
            return runtime.validate()
                && runtime_air_result_type(pool, result)
                    .is_some_and(|result| runtime.validate_air_call(&arguments, result));
        }
        let Some(first) = args.first() else {
            return false;
        };
        match self {
            Self::PtrToInt => {
                args.len() == 1
                    && (first.ty.is_ptr() || first.ty == Type::NEVER)
                    && result == Type::U64
            }
            Self::IntToPtr => {
                args.len() == 1
                    && (first.ty == Type::U64 || first.ty == Type::NEVER)
                    && (result.as_ptr_mut().is_some() || result == Type::NEVER)
            }
            Self::PtrRead | Self::PtrReadUnaligned => {
                args.len() == 1
                    && match first.ty.kind() {
                        TypeKind::PtrConst(id) => pool.ptr_const_def(id) == result,
                        TypeKind::PtrMut(id) => pool.ptr_mut_def(id) == result,
                        _ => false,
                    }
            }
            Self::PtrWrite | Self::PtrWriteUnaligned => {
                args.len() == 2
                    && result == Type::UNIT
                    && match first.ty.kind() {
                        TypeKind::PtrMut(id) => {
                            let pointee = pool.ptr_mut_def(id);
                            args[1].ty == pointee || args[1].ty == Type::NEVER
                        }
                        _ => false,
                    }
            }
            Self::PtrOffset => {
                args.len() == 2
                    && (args[1].ty.is_integer() || args[1].ty == Type::NEVER)
                    && if first.ty == Type::NEVER {
                        result == Type::NEVER
                    } else {
                        first.ty.is_ptr() && result == first.ty
                    }
            }
            Self::Raw => {
                args.len() == 1
                    && matches!(
                        first.source,
                        IntrinsicAirArgumentSource::Load
                            | IntrinsicAirArgumentSource::Param
                            | IntrinsicAirArgumentSource::PlaceRead { .. }
                    )
                    && match result.kind() {
                        TypeKind::PtrConst(id) => pool.ptr_const_def(id) == first.ty,
                        _ => false,
                    }
            }
            Self::RawMut => {
                args.len() == 1
                    && matches!(
                        first.source,
                        IntrinsicAirArgumentSource::Load
                            | IntrinsicAirArgumentSource::Param
                            | IntrinsicAirArgumentSource::PlaceRead { .. }
                    )
                    && match result.kind() {
                        TypeKind::PtrMut(id) => pool.ptr_mut_def(id) == first.ty,
                        _ => false,
                    }
            }
            Self::FieldPtr => {
                args.len() == 1
                    && matches!(
                        first.source,
                        IntrinsicAirArgumentSource::PlaceRead {
                            terminal_field: true
                        }
                    )
                    && match result.kind() {
                        TypeKind::PtrMut(id) => pool.ptr_mut_def(id) == first.ty,
                        _ => false,
                    }
            }
            Self::Syscall => {
                (1..=7).contains(&args.len())
                    && args
                        .iter()
                        .all(|arg| arg.ty == Type::U64 || arg.ty == Type::NEVER)
                    && result == Type::I64
            }
            Self::BitCast => {
                args.len() == 1
                    && first.ty.is_integer()
                    && first.ty.integer_semantics().map(|integer| integer.bits())
                        == result.integer_semantics().map(|integer| integer.bits())
            }
            Self::PanicNoMessage
            | Self::Panic
            | Self::AssertFailed
            | Self::AssertWithMessage
            | Self::BoundsCheck
            | Self::DebugI64
            | Self::DebugU64
            | Self::DebugBool
            | Self::DebugStr
            | Self::ReadLine
            | Self::ParseI32
            | Self::ParseI64
            | Self::ParseU32
            | Self::ParseU64
            | Self::RandomU32
            | Self::RandomU64
            | Self::Alloc
            | Self::AllocZeroed
            | Self::Free
            | Self::Realloc
            | Self::Resize
            | Self::ByteCopy
            | Self::ByteMove
            | Self::ByteSet
            | Self::ArgCount
            | Self::ArgPtr
            | Self::ArgLen
            | Self::EnvCount
            | Self::EnvPtr
            | Self::EnvLen => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{EnumDef, LangItem, StructDef};
    use lasso::ThreadedRodeo;
    use rue_span::FileId;
    use std::collections::BTreeSet;
    use std::sync::Arc;

    const EXACT_OPERATIONS: [(IntrinsicOperation, &str); 42] = [
        (IntrinsicOperation::PanicNoMessage, "panic"),
        (IntrinsicOperation::Panic, "panic"),
        (IntrinsicOperation::AssertFailed, "assert"),
        (IntrinsicOperation::AssertWithMessage, "assert"),
        (IntrinsicOperation::BoundsCheck, "assert"),
        (IntrinsicOperation::DebugI64, "dbg"),
        (IntrinsicOperation::DebugU64, "dbg"),
        (IntrinsicOperation::DebugBool, "dbg"),
        (IntrinsicOperation::DebugStr, "dbg"),
        (IntrinsicOperation::ReadLine, "read_line"),
        (IntrinsicOperation::ParseI32, "parse_i32"),
        (IntrinsicOperation::ParseI64, "parse_i64"),
        (IntrinsicOperation::ParseU32, "parse_u32"),
        (IntrinsicOperation::ParseU64, "parse_u64"),
        (IntrinsicOperation::RandomU32, "random_u32"),
        (IntrinsicOperation::RandomU64, "random_u64"),
        (IntrinsicOperation::PtrToInt, "ptr_to_int"),
        (IntrinsicOperation::IntToPtr, "int_to_ptr"),
        (IntrinsicOperation::PtrRead, "ptr_read"),
        (IntrinsicOperation::PtrReadUnaligned, "ptr_read_unaligned"),
        (IntrinsicOperation::PtrWrite, "ptr_write"),
        (IntrinsicOperation::PtrWriteUnaligned, "ptr_write_unaligned"),
        (IntrinsicOperation::PtrOffset, "ptr_offset"),
        (IntrinsicOperation::Alloc, "alloc"),
        (IntrinsicOperation::AllocZeroed, "alloc_zeroed"),
        (IntrinsicOperation::Free, "free"),
        (IntrinsicOperation::Realloc, "realloc"),
        (IntrinsicOperation::Resize, "resize"),
        (IntrinsicOperation::ByteCopy, "byte_copy"),
        (IntrinsicOperation::ByteMove, "byte_move"),
        (IntrinsicOperation::ByteSet, "byte_set"),
        (IntrinsicOperation::ArgCount, "arg_count"),
        (IntrinsicOperation::ArgPtr, "arg_ptr"),
        (IntrinsicOperation::ArgLen, "arg_len"),
        (IntrinsicOperation::EnvCount, "env_count"),
        (IntrinsicOperation::EnvPtr, "env_ptr"),
        (IntrinsicOperation::EnvLen, "env_len"),
        (IntrinsicOperation::Raw, "raw"),
        (IntrinsicOperation::RawMut, "raw_mut"),
        (IntrinsicOperation::FieldPtr, "field_ptr"),
        (IntrinsicOperation::Syscall, "syscall"),
        (IntrinsicOperation::BitCast, "bitCast"),
    ];

    const EXACT_RUNTIME_MAPPINGS: [(IntrinsicOperation, RuntimeCallKind); 30] = [
        (
            IntrinsicOperation::PanicNoMessage,
            RuntimeCallKind::PanicNoMessage,
        ),
        (IntrinsicOperation::Panic, RuntimeCallKind::Panic),
        (
            IntrinsicOperation::AssertFailed,
            RuntimeCallKind::AssertFailed,
        ),
        (
            IntrinsicOperation::AssertWithMessage,
            RuntimeCallKind::AssertWithMessage,
        ),
        (
            IntrinsicOperation::BoundsCheck,
            RuntimeCallKind::BoundsCheck,
        ),
        (IntrinsicOperation::DebugI64, RuntimeCallKind::DebugI64),
        (IntrinsicOperation::DebugU64, RuntimeCallKind::DebugU64),
        (IntrinsicOperation::DebugBool, RuntimeCallKind::DebugBool),
        (IntrinsicOperation::DebugStr, RuntimeCallKind::DebugStr),
        (IntrinsicOperation::ReadLine, RuntimeCallKind::ReadLine),
        (IntrinsicOperation::ParseI32, RuntimeCallKind::ParseI32),
        (IntrinsicOperation::ParseI64, RuntimeCallKind::ParseI64),
        (IntrinsicOperation::ParseU32, RuntimeCallKind::ParseU32),
        (IntrinsicOperation::ParseU64, RuntimeCallKind::ParseU64),
        (IntrinsicOperation::RandomU32, RuntimeCallKind::RandomU32),
        (IntrinsicOperation::RandomU64, RuntimeCallKind::RandomU64),
        (IntrinsicOperation::Alloc, RuntimeCallKind::Alloc),
        (
            IntrinsicOperation::AllocZeroed,
            RuntimeCallKind::AllocZeroed,
        ),
        (IntrinsicOperation::Free, RuntimeCallKind::Free),
        (IntrinsicOperation::Realloc, RuntimeCallKind::Realloc),
        (IntrinsicOperation::Resize, RuntimeCallKind::Resize),
        (IntrinsicOperation::ByteCopy, RuntimeCallKind::ByteCopy),
        (IntrinsicOperation::ByteMove, RuntimeCallKind::ByteMove),
        (IntrinsicOperation::ByteSet, RuntimeCallKind::ByteSet),
        (IntrinsicOperation::ArgCount, RuntimeCallKind::ArgCount),
        (IntrinsicOperation::ArgPtr, RuntimeCallKind::ArgPtr),
        (IntrinsicOperation::ArgLen, RuntimeCallKind::ArgLen),
        (IntrinsicOperation::EnvCount, RuntimeCallKind::EnvCount),
        (IntrinsicOperation::EnvPtr, RuntimeCallKind::EnvPtr),
        (IntrinsicOperation::EnvLen, RuntimeCallKind::EnvLen),
    ];

    const ORDINARY_CALL_ONLY: [RuntimeCallKind; 11] = [
        RuntimeCallKind::StrByteAt,
        RuntimeCallKind::StrCharScalar,
        RuntimeCallKind::StrCharNext,
        RuntimeCallKind::StrCharScalarLossy,
        RuntimeCallKind::StrCharNextLossy,
        RuntimeCallKind::ToString,
        RuntimeCallKind::ToStringUnsigned,
        RuntimeCallKind::StrPrintAggregate,
        RuntimeCallKind::StrPrintProjected,
        RuntimeCallKind::StrPrintlnAggregate,
        RuntimeCallKind::StrPrintlnProjected,
    ];

    #[test]
    fn all_operations_are_unique_and_runtime_mapping_is_symmetric() {
        assert_eq!(IntrinsicOperation::ALL.len(), 42);
        assert_eq!(
            IntrinsicOperation::ALL,
            EXACT_OPERATIONS.map(|(operation, _)| operation)
        );
        for (index, operation) in IntrinsicOperation::ALL.iter().enumerate() {
            assert_eq!(
                IntrinsicOperation::ALL[..index]
                    .iter()
                    .filter(|previous| *previous == operation)
                    .count(),
                0,
                "duplicate intrinsic operation at index {index}"
            );
            if let Some(runtime) = operation.runtime_call_kind() {
                assert_eq!(
                    IntrinsicOperation::from_runtime_call(runtime),
                    Some(*operation)
                );
            }
            assert_eq!(
                operation.expected_spelling(),
                EXACT_OPERATIONS[index].1,
                "spelling drifted for {operation:?}"
            );
        }
    }

    #[test]
    fn inventory_and_runtime_partition_are_exact() {
        const SPELLINGS: [&str; 36] = [
            "panic",
            "assert",
            "dbg",
            "read_line",
            "parse_i32",
            "parse_i64",
            "parse_u32",
            "parse_u64",
            "random_u32",
            "random_u64",
            "ptr_to_int",
            "int_to_ptr",
            "ptr_read",
            "ptr_read_unaligned",
            "ptr_write",
            "ptr_write_unaligned",
            "ptr_offset",
            "alloc",
            "alloc_zeroed",
            "free",
            "realloc",
            "resize",
            "byte_copy",
            "byte_move",
            "byte_set",
            "arg_count",
            "arg_ptr",
            "arg_len",
            "env_count",
            "env_ptr",
            "env_len",
            "raw",
            "raw_mut",
            "field_ptr",
            "syscall",
            "bitCast",
        ];
        let spellings = IntrinsicOperation::ALL
            .iter()
            .map(|operation| operation.expected_spelling())
            .collect::<BTreeSet<_>>();
        assert_eq!(spellings.len(), SPELLINGS.len());
        assert_eq!(spellings, SPELLINGS.into_iter().collect());

        let runtime = IntrinsicOperation::ALL
            .iter()
            .filter_map(|operation| operation.runtime_call_kind())
            .collect::<Vec<_>>();
        assert_eq!(runtime.len(), 30);
        assert_eq!(
            IntrinsicOperation::ALL
                .iter()
                .filter(|operation| operation.runtime_call_kind().is_none())
                .count(),
            12
        );
        assert_eq!(
            runtime
                .iter()
                .enumerate()
                .filter(|(index, kind)| !runtime[..*index].contains(kind))
                .count(),
            30
        );
        assert_eq!(
            runtime,
            EXACT_RUNTIME_MAPPINGS.map(|(_, runtime)| runtime),
            "the exact intrinsic-to-runtime map drifted"
        );
        assert_eq!(RuntimeCallKind::ALL.len(), 41);
        for kind in RuntimeCallKind::ALL {
            assert_eq!(
                IntrinsicOperation::from_runtime_call(kind).is_some(),
                !ORDINARY_CALL_ONLY.contains(&kind),
                "runtime mapping partition drifted for {kind:?}"
            );
        }
        assert_eq!(
            ORDINARY_CALL_ONLY.len(),
            RuntimeCallKind::ALL.len() - runtime.len()
        );
        for (operation, runtime) in EXACT_RUNTIME_MAPPINGS {
            assert_eq!(operation.runtime_call_kind(), Some(runtime));
            assert_eq!(
                IntrinsicOperation::from_runtime_call(runtime),
                Some(operation)
            );
        }
        for runtime in ORDINARY_CALL_ONLY {
            assert_eq!(IntrinsicOperation::from_runtime_call(runtime), None);
        }
    }

    #[test]
    fn pure_validator_checks_every_shape_and_relation() {
        let pool = TypeInternPool::new();
        let ptr_const_i32 = Type::new_ptr_const(pool.intern_ptr_const_from_type(Type::I32));
        let ptr_mut_i32 = Type::new_ptr_mut(pool.intern_ptr_mut_from_type(Type::I32));
        let ptr_mut_u64 = Type::new_ptr_mut(pool.intern_ptr_mut_from_type(Type::U64));
        let normal = crate::AirArgMode::Normal;

        let describe = |operation: IntrinsicOperation, args: Vec<(Type, AirArgMode)>| {
            args.into_iter()
                .enumerate()
                .map(|(index, (ty, mode))| match (operation, index) {
                    (IntrinsicOperation::Raw | IntrinsicOperation::RawMut, 0) => {
                        IntrinsicAirArgument::load(ty, mode)
                    }
                    (IntrinsicOperation::FieldPtr, 0) => {
                        IntrinsicAirArgument::field_place_read(ty, mode)
                    }
                    _ => IntrinsicAirArgument::value(ty, mode),
                })
                .collect::<Vec<_>>()
        };

        for (operation, args, result) in [
            (
                IntrinsicOperation::PtrToInt,
                vec![(ptr_const_i32, normal)],
                Type::U64,
            ),
            (
                IntrinsicOperation::IntToPtr,
                vec![(Type::U64, normal)],
                ptr_mut_i32,
            ),
            (
                IntrinsicOperation::PtrRead,
                vec![(ptr_const_i32, normal)],
                Type::I32,
            ),
            (
                IntrinsicOperation::PtrReadUnaligned,
                vec![(ptr_mut_i32, normal)],
                Type::I32,
            ),
            (
                IntrinsicOperation::PtrWrite,
                vec![(ptr_mut_i32, normal), (Type::I32, normal)],
                Type::UNIT,
            ),
            (
                IntrinsicOperation::PtrWriteUnaligned,
                vec![(ptr_mut_i32, normal), (Type::I32, normal)],
                Type::UNIT,
            ),
            (
                IntrinsicOperation::PtrOffset,
                vec![(ptr_const_i32, normal), (Type::I64, normal)],
                ptr_const_i32,
            ),
            (
                IntrinsicOperation::Raw,
                vec![(Type::I32, normal)],
                ptr_const_i32,
            ),
            (
                IntrinsicOperation::RawMut,
                vec![(Type::I32, normal)],
                ptr_mut_i32,
            ),
            (
                IntrinsicOperation::FieldPtr,
                vec![(Type::I32, normal)],
                ptr_mut_i32,
            ),
            (
                IntrinsicOperation::Syscall,
                vec![(Type::U64, normal)],
                Type::I64,
            ),
            (
                IntrinsicOperation::Syscall,
                vec![(Type::U64, normal); 7],
                Type::I64,
            ),
            (
                IntrinsicOperation::BitCast,
                vec![(Type::I32, normal)],
                Type::U32,
            ),
            (
                IntrinsicOperation::BitCast,
                vec![(Type::I64, normal)],
                Type::U64,
            ),
        ] {
            let args = describe(operation, args);
            assert!(
                operation.validate_call(&pool, &args, result),
                "canonical pure call rejected: {operation:?} {args:?} -> {result:?}"
            );
            let mut wrong_mode = args.clone();
            wrong_mode[0].mode = crate::AirArgMode::Borrow;
            assert!(!operation.validate_call(&pool, &wrong_mode, result));
        }

        let invalid = [
            (
                IntrinsicOperation::PtrToInt,
                vec![(Type::I32, normal)],
                Type::U64,
            ),
            (
                IntrinsicOperation::PtrToInt,
                vec![(ptr_const_i32, normal)],
                Type::I64,
            ),
            (
                IntrinsicOperation::IntToPtr,
                vec![(Type::I64, normal)],
                ptr_mut_i32,
            ),
            (
                IntrinsicOperation::IntToPtr,
                vec![(Type::U64, normal)],
                ptr_const_i32,
            ),
            (
                IntrinsicOperation::PtrRead,
                vec![(ptr_const_i32, normal)],
                Type::U64,
            ),
            (
                IntrinsicOperation::PtrWrite,
                vec![(ptr_const_i32, normal), (Type::I32, normal)],
                Type::UNIT,
            ),
            (
                IntrinsicOperation::PtrWrite,
                vec![(ptr_mut_i32, normal), (Type::U64, normal)],
                Type::UNIT,
            ),
            (
                IntrinsicOperation::PtrOffset,
                vec![(ptr_const_i32, normal), (Type::BOOL, normal)],
                ptr_const_i32,
            ),
            (
                IntrinsicOperation::PtrOffset,
                vec![(ptr_const_i32, normal), (Type::I64, normal)],
                ptr_mut_i32,
            ),
            (
                IntrinsicOperation::Raw,
                vec![(Type::I32, normal)],
                ptr_mut_i32,
            ),
            (
                IntrinsicOperation::RawMut,
                vec![(Type::I32, normal)],
                ptr_mut_u64,
            ),
            (
                IntrinsicOperation::FieldPtr,
                vec![(Type::I32, normal)],
                ptr_mut_u64,
            ),
            (IntrinsicOperation::Syscall, vec![], Type::I64),
            (
                IntrinsicOperation::Syscall,
                vec![(Type::U64, normal); 8],
                Type::I64,
            ),
            (
                IntrinsicOperation::Syscall,
                vec![(Type::I64, normal)],
                Type::I64,
            ),
            (
                IntrinsicOperation::BitCast,
                vec![(Type::I32, normal)],
                Type::U64,
            ),
            (
                IntrinsicOperation::BitCast,
                vec![(Type::BOOL, normal)],
                Type::BOOL,
            ),
        ];
        for (operation, args, result) in invalid {
            let args = describe(operation, args);
            assert!(
                !operation.validate_call(&pool, &args, result),
                "counterfeit pure call accepted: {operation:?} {args:?} -> {result:?}"
            );
        }
        for operation in IntrinsicOperation::ALL {
            assert!(!operation.validate_call(&pool, &[], Type::UNIT));
        }
    }

    #[test]
    fn pure_validator_matches_bottom_coercion_and_place_provenance_matrix() {
        use IntrinsicAirArgumentSource as S;

        let pool = TypeInternPool::new();
        let ptr_const_i32 = Type::new_ptr_const(pool.intern_ptr_const_from_type(Type::I32));
        let ptr_mut_i32 = Type::new_ptr_mut(pool.intern_ptr_mut_from_type(Type::I32));
        let ptr_const_never = Type::new_ptr_const(pool.intern_ptr_const_from_type(Type::NEVER));
        let ptr_mut_never = Type::new_ptr_mut(pool.intern_ptr_mut_from_type(Type::NEVER));
        let n = AirArgMode::Normal;
        let value = |ty| IntrinsicAirArgument::value(ty, n);
        let source = |ty, source| IntrinsicAirArgument {
            ty,
            mode: n,
            source,
        };

        let accepted = [
            (
                IntrinsicOperation::PtrToInt,
                vec![value(Type::NEVER)],
                Type::U64,
            ),
            (
                IntrinsicOperation::IntToPtr,
                vec![value(Type::NEVER)],
                ptr_mut_i32,
            ),
            (
                IntrinsicOperation::IntToPtr,
                vec![value(Type::U64)],
                Type::NEVER,
            ),
            (
                IntrinsicOperation::IntToPtr,
                vec![value(Type::NEVER)],
                Type::NEVER,
            ),
            (
                IntrinsicOperation::PtrRead,
                vec![value(ptr_const_never)],
                Type::NEVER,
            ),
            (
                IntrinsicOperation::PtrWrite,
                vec![value(ptr_mut_i32), value(Type::NEVER)],
                Type::UNIT,
            ),
            (
                IntrinsicOperation::PtrWriteUnaligned,
                vec![value(ptr_mut_never), value(Type::NEVER)],
                Type::UNIT,
            ),
            (
                IntrinsicOperation::PtrOffset,
                vec![value(Type::NEVER), value(Type::I64)],
                Type::NEVER,
            ),
            (
                IntrinsicOperation::PtrOffset,
                vec![value(Type::NEVER), value(Type::NEVER)],
                Type::NEVER,
            ),
            (
                IntrinsicOperation::PtrOffset,
                vec![value(ptr_const_i32), value(Type::NEVER)],
                ptr_const_i32,
            ),
            (
                IntrinsicOperation::Raw,
                vec![source(Type::NEVER, S::Load)],
                ptr_const_never,
            ),
            (
                IntrinsicOperation::RawMut,
                vec![source(Type::NEVER, S::Param)],
                ptr_mut_never,
            ),
            (
                IntrinsicOperation::RawMut,
                vec![source(
                    Type::I32,
                    S::PlaceRead {
                        terminal_field: false,
                    },
                )],
                ptr_mut_i32,
            ),
            (
                IntrinsicOperation::FieldPtr,
                vec![source(
                    Type::NEVER,
                    S::PlaceRead {
                        terminal_field: true,
                    },
                )],
                ptr_mut_never,
            ),
            (
                IntrinsicOperation::Syscall,
                vec![value(Type::NEVER)],
                Type::I64,
            ),
            (
                IntrinsicOperation::Syscall,
                vec![value(Type::U64), value(Type::NEVER), value(Type::U64)],
                Type::I64,
            ),
        ];
        for (operation, args, result) in accepted {
            assert!(
                operation.validate_call(&pool, &args, result),
                "bottom/place call rejected: {operation:?} {args:?} -> {result:?}"
            );
        }

        let rejected = [
            (
                IntrinsicOperation::PtrToInt,
                vec![value(Type::NEVER)],
                Type::I64,
            ),
            (
                IntrinsicOperation::IntToPtr,
                vec![value(Type::I32)],
                ptr_mut_i32,
            ),
            (
                IntrinsicOperation::IntToPtr,
                vec![value(Type::NEVER)],
                ptr_const_i32,
            ),
            (
                IntrinsicOperation::PtrRead,
                vec![value(Type::NEVER)],
                Type::NEVER,
            ),
            (
                IntrinsicOperation::PtrWrite,
                vec![value(Type::NEVER), value(Type::I32)],
                Type::UNIT,
            ),
            (
                IntrinsicOperation::PtrWrite,
                vec![value(ptr_mut_i32), value(Type::I64)],
                Type::UNIT,
            ),
            (
                IntrinsicOperation::PtrOffset,
                vec![value(Type::NEVER), value(Type::I64)],
                ptr_const_i32,
            ),
            (
                IntrinsicOperation::PtrOffset,
                vec![value(ptr_const_i32), value(Type::BOOL)],
                ptr_const_i32,
            ),
            (
                IntrinsicOperation::PtrOffset,
                vec![value(Type::I32), value(Type::NEVER)],
                Type::I32,
            ),
            (
                IntrinsicOperation::Raw,
                vec![value(Type::I32)],
                ptr_const_i32,
            ),
            (
                IntrinsicOperation::RawMut,
                vec![value(Type::I32)],
                ptr_mut_i32,
            ),
            (
                IntrinsicOperation::FieldPtr,
                vec![source(Type::I32, S::Load)],
                ptr_mut_i32,
            ),
            (
                IntrinsicOperation::FieldPtr,
                vec![source(Type::I32, S::Param)],
                ptr_mut_i32,
            ),
            (
                IntrinsicOperation::FieldPtr,
                vec![source(
                    Type::I32,
                    S::PlaceRead {
                        terminal_field: false,
                    },
                )],
                ptr_mut_i32,
            ),
            (
                IntrinsicOperation::Syscall,
                vec![value(Type::U64), value(Type::BOOL)],
                Type::I64,
            ),
            (
                IntrinsicOperation::BitCast,
                vec![value(Type::NEVER)],
                Type::U64,
            ),
        ];
        for (operation, args, result) in rejected {
            assert!(
                !operation.validate_call(&pool, &args, result),
                "counterfeit bottom/place call accepted: {operation:?} {args:?} -> {result:?}"
            );
        }
    }

    #[test]
    fn runtime_option_results_require_the_exact_shape_in_mutable_and_frozen_pools() {
        let pool = TypeInternPool::new();
        let interner = ThreadedRodeo::new();
        let (strbuf_id, _) = pool.register_struct(
            interner.get_or_intern("StrBuf"),
            StructDef {
                name: "StrBuf".into(),
                fields: vec![],
                is_copy: false,
                is_linear: false,
                declared_linear: false,
                destructor: None,
                is_builtin: true,
                is_pub: true,
                file_id: FileId::DEFAULT,
            },
        );
        pool.set_struct_lang_item(strbuf_id, LangItem::StrBuf);
        let strbuf = Type::new_struct(strbuf_id);
        let register = |name: &'static str, variants: &[(&'static str, Vec<Type>)]| {
            let (id, _) = pool.register_enum(
                interner.get_or_intern(name),
                EnumDef {
                    name: name.into(),
                    variants: variants
                        .iter()
                        .map(|(name, _)| Arc::<str>::from(*name))
                        .collect::<Vec<_>>()
                        .into(),
                    variant_payloads: variants
                        .iter()
                        .map(|(_, payload)| payload.clone())
                        .collect(),
                    is_pub: true,
                    is_non_exhaustive: false,
                    file_id: FileId::DEFAULT,
                },
            );
            Type::new_enum(id)
        };

        let canonical = [
            (
                register("OptionI32", &[("Some", vec![Type::I32]), ("None", vec![])]),
                RuntimeAirType::OptionI32,
            ),
            (
                register("OptionI64", &[("Some", vec![Type::I64]), ("None", vec![])]),
                RuntimeAirType::OptionI64,
            ),
            (
                register("OptionU32", &[("Some", vec![Type::U32]), ("None", vec![])]),
                RuntimeAirType::OptionU32,
            ),
            (
                register("OptionU64", &[("Some", vec![Type::U64]), ("None", vec![])]),
                RuntimeAirType::OptionU64,
            ),
            (
                register("OptionStrBuf", &[("Some", vec![strbuf]), ("None", vec![])]),
                RuntimeAirType::OptionStrBuf,
            ),
            (
                register("Reversed", &[("None", vec![]), ("Some", vec![Type::I32])]),
                RuntimeAirType::OptionI32,
            ),
        ];
        let malformed = [
            register(
                "PayloadNoneExtra",
                &[
                    ("Some", vec![Type::I32]),
                    ("None", vec![Type::I64, Type::I64]),
                    ("Extra", vec![]),
                ],
            ),
            register(
                "PayloadNone",
                &[("Some", vec![Type::I32]), ("None", vec![Type::I64])],
            ),
            register(
                "Extra",
                &[
                    ("Some", vec![Type::I32]),
                    ("None", vec![]),
                    ("Extra", vec![]),
                ],
            ),
            register("MissingNone", &[("Some", vec![Type::I32])]),
            register(
                "DuplicateSome",
                &[("Some", vec![Type::I32]), ("Some", vec![])],
            ),
            register("MissingSome", &[("None", vec![]), ("Other", vec![])]),
            register("EmptySome", &[("Some", vec![]), ("None", vec![])]),
            register(
                "WideSome",
                &[("Some", vec![Type::I32, Type::I32]), ("None", vec![])],
            ),
            register(
                "UnsupportedSome",
                &[("Some", vec![Type::BOOL]), ("None", vec![])],
            ),
        ];

        for (ty, expected) in canonical {
            assert_eq!(runtime_air_result_type(&pool, ty), Some(expected));
        }
        for ty in malformed {
            assert_eq!(runtime_air_result_type(&pool, ty), None);
        }

        let pool = pool.freeze();
        for (ty, expected) in canonical {
            assert_eq!(runtime_air_result_type(&pool, ty), Some(expected));
        }
        for ty in malformed {
            assert_eq!(runtime_air_result_type(&pool, ty), None);
        }
    }

    #[test]
    fn runtime_validator_accepts_exact_manifest_shapes_and_rejects_counterfeits() {
        let pool = TypeInternPool::new();
        let interner = ThreadedRodeo::new();
        let register_struct = |name: &'static str| {
            let (id, _) = pool.register_struct(
                interner.get_or_intern(name),
                StructDef {
                    name: name.into(),
                    fields: vec![],
                    is_copy: true,
                    is_linear: false,
                    declared_linear: false,
                    destructor: None,
                    is_builtin: true,
                    is_pub: true,
                    file_id: FileId::DEFAULT,
                },
            );
            Type::new_struct(id)
        };
        let text = register_struct("str");
        let strbuf = register_struct("StrBuf");
        pool.set_struct_lang_item(strbuf.as_struct().unwrap(), LangItem::StrBuf);
        let option = |name: &'static str, payload: Type| {
            let (id, _) = pool.register_enum(
                interner.get_or_intern(name),
                EnumDef {
                    name: name.into(),
                    variants: Arc::from([Arc::from("Some"), Arc::from("None")]),
                    variant_payloads: vec![vec![payload], vec![]],
                    is_pub: true,
                    is_non_exhaustive: false,
                    file_id: FileId::DEFAULT,
                },
            );
            Type::new_enum(id)
        };
        let option_strbuf = option("OptionStrBuf", strbuf);
        let option_i32 = option("OptionI32", Type::I32);
        let option_i64 = option("OptionI64", Type::I64);
        let option_u32 = option("OptionU32", Type::U32);
        let option_u64 = option("OptionU64", Type::U64);
        let const_u8 = Type::new_ptr_const(pool.intern_ptr_const_from_type(Type::U8));
        let mut_u8 = Type::new_ptr_mut(pool.intern_ptr_mut_from_type(Type::U8));
        let n = crate::AirArgMode::Normal;
        let signatures = [
            (IntrinsicOperation::PanicNoMessage, vec![], Type::NEVER),
            (IntrinsicOperation::Panic, vec![(text, n)], Type::NEVER),
            (
                IntrinsicOperation::AssertFailed,
                vec![(Type::BOOL, n)],
                Type::UNIT,
            ),
            (
                IntrinsicOperation::AssertWithMessage,
                vec![(Type::BOOL, n), (text, n)],
                Type::UNIT,
            ),
            (
                IntrinsicOperation::BoundsCheck,
                vec![(Type::BOOL, n)],
                Type::UNIT,
            ),
            (
                IntrinsicOperation::DebugI64,
                vec![(Type::I64, n)],
                Type::UNIT,
            ),
            (
                IntrinsicOperation::DebugU64,
                vec![(Type::U64, n)],
                Type::UNIT,
            ),
            (
                IntrinsicOperation::DebugBool,
                vec![(Type::BOOL, n)],
                Type::UNIT,
            ),
            (IntrinsicOperation::DebugStr, vec![(text, n)], Type::UNIT),
            (IntrinsicOperation::ReadLine, vec![], option_strbuf),
            (IntrinsicOperation::ParseI32, vec![(text, n)], option_i32),
            (IntrinsicOperation::ParseI64, vec![(text, n)], option_i64),
            (IntrinsicOperation::ParseU32, vec![(text, n)], option_u32),
            (IntrinsicOperation::ParseU64, vec![(text, n)], option_u64),
            (IntrinsicOperation::RandomU32, vec![], Type::U32),
            (IntrinsicOperation::RandomU64, vec![], Type::U64),
            (
                IntrinsicOperation::Alloc,
                vec![(Type::U64, n), (Type::U64, n)],
                mut_u8,
            ),
            (
                IntrinsicOperation::AllocZeroed,
                vec![(Type::U64, n), (Type::U64, n)],
                mut_u8,
            ),
            (
                IntrinsicOperation::Free,
                vec![(mut_u8, n), (Type::U64, n), (Type::U64, n)],
                Type::UNIT,
            ),
            (
                IntrinsicOperation::Realloc,
                vec![(mut_u8, n), (Type::U64, n), (Type::U64, n), (Type::U64, n)],
                mut_u8,
            ),
            (
                IntrinsicOperation::Resize,
                vec![(mut_u8, n), (Type::U64, n), (Type::U64, n), (Type::U64, n)],
                Type::BOOL,
            ),
            (
                IntrinsicOperation::ByteCopy,
                vec![(mut_u8, n), (const_u8, n), (Type::U64, n)],
                Type::UNIT,
            ),
            (
                IntrinsicOperation::ByteMove,
                vec![(mut_u8, n), (const_u8, n), (Type::U64, n)],
                Type::UNIT,
            ),
            (
                IntrinsicOperation::ByteSet,
                vec![(mut_u8, n), (Type::U8, n), (Type::U64, n)],
                Type::UNIT,
            ),
            (IntrinsicOperation::ArgCount, vec![], Type::U64),
            (IntrinsicOperation::ArgPtr, vec![(Type::U64, n)], mut_u8),
            (IntrinsicOperation::ArgLen, vec![(Type::U64, n)], Type::U64),
            (IntrinsicOperation::EnvCount, vec![], Type::U64),
            (IntrinsicOperation::EnvPtr, vec![(Type::U64, n)], mut_u8),
            (IntrinsicOperation::EnvLen, vec![(Type::U64, n)], Type::U64),
        ];
        assert_eq!(signatures.len(), EXACT_RUNTIME_MAPPINGS.len());
        for (index, (operation, args, result)) in signatures.into_iter().enumerate() {
            assert_eq!(operation, EXACT_RUNTIME_MAPPINGS[index].0);
            let args = args
                .into_iter()
                .map(|(ty, mode)| IntrinsicAirArgument::value(ty, mode))
                .collect::<Vec<_>>();
            assert!(
                operation.validate_call(&pool, &args, result),
                "canonical runtime call rejected: {operation:?} {args:?} -> {result:?}"
            );
            let mut extra = args.clone();
            extra.push(IntrinsicAirArgument::value(Type::I32, n));
            assert!(!operation.validate_call(&pool, &extra, result));
            assert!(!operation.validate_call(&pool, &args, Type::ERROR));
            if !args.is_empty() {
                let mut wrong_type = args.clone();
                wrong_type[0].ty = Type::ERROR;
                assert!(!operation.validate_call(&pool, &wrong_type, result));
                let mut wrong_mode = args.clone();
                wrong_mode[0].mode = crate::AirArgMode::Inout;
                assert!(!operation.validate_call(&pool, &wrong_mode, result));
                let short = &args[..args.len() - 1];
                assert!(!operation.validate_call(&pool, short, result));
            }
        }
    }
}
