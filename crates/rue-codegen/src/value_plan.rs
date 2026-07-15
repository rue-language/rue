//! Target-independent CFG value materialization policy.
//!
//! `ValuePlan` is the policy record shared by both concrete MIR lowerers.  It
//! deliberately contains no CFG value references and no target state: the
//! lowerers use the CFG value only as the lookup key, then consume the same
//! decided shape, width, and requirements while selecting instructions.
//!
//! This module is intentionally a decision tree rather than a generic MIR
//! abstraction.  Adding a `CfgInstData` variant that participates in value
//! materialization requires updating the exhaustive match below, so an
//! architecture cannot quietly invent a different scalar/aggregate policy.

use rue_air::TypeKind;
use rue_cfg::{CfgInstData, CfgValue, Type};

use crate::cfg_lower::CfgLowerContext;

/// The representation required by a value consumer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValueShape {
    /// No storage slots exist.  A primary vreg may still be used as a
    /// never-read placeholder for CFG bookkeeping.  A zero-slot struct or
    /// array is represented by `CompleteAggregate { slot_count: 0 }` instead;
    /// this shape is reserved for non-aggregate zero-sized values.
    ZeroSized,
    /// Exactly one logical slot; the primary vreg is the complete value.
    Scalar,
    /// Every logical slot must be present.  The first slot is the primary
    /// vreg, and the vector is always in ascending logical order.
    CompleteAggregate { slot_count: u32 },
}

impl ValueShape {
    pub const fn slot_count(self) -> u32 {
        match self {
            Self::ZeroSized => 0,
            Self::Scalar => 1,
            Self::CompleteAggregate { slot_count } => slot_count,
        }
    }

    pub const fn requires_complete_slots(self) -> bool {
        matches!(self, Self::CompleteAggregate { .. })
    }
}

/// Integer width and signedness selected once for arithmetic/comparison and
/// coercion preparation.  Non-integer values use `None`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IntegerWidth {
    pub bits: u32,
    pub signed: bool,
}

/// The language operation whose target adapter must emit instructions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValueKind {
    Constant,
    BoolConstant,
    StringConstant,
    Parameter,
    BlockParameter,
    BinaryArithmetic,
    Comparison,
    UnaryArithmetic,
    Bitwise,
    Shift,
    Allocation,
    Load,
    Store,
    ParameterStore,
    Call,
    Intrinsic,
    StructInit,
    ArrayInit,
    EnumVariant,
    EnumPayloadGet,
    IntegerCast,
    Drop,
    StorageLive,
    StorageDead,
    PlaceRead,
    PlaceWrite,
}

/// Extra language policy required by a materialization site.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MaterializationRequirement {
    /// The primary vreg is sufficient.
    Primary,
    /// The complete logical slot vector must be materialized and consumed.
    CompleteSlots,
    /// The operation is a side effect and its value is only a bookkeeping
    /// placeholder (for example a store or storage marker).
    SideEffect,
}

/// How a comparison must be prepared before target-specific condition codes
/// are selected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComparisonPreparation {
    Unit,
    Scalar { width: IntegerWidth },
    Aggregate { slot_count: u32 },
    StringContent { slot_count: u32 },
}

/// How a value is rooted in storage.  This is shared because by-ref loading,
/// frame loading, and place loading must agree on whether a pointer is a
/// value or an address of the value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoragePolicy {
    None,
    LocalSlot { slot: u32 },
    ParameterSlot { slot: u32, by_ref: bool },
    Place,
}

/// A complete target-neutral value materialization decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ValuePlan {
    pub kind: ValueKind,
    pub shape: ValueShape,
    pub requirement: MaterializationRequirement,
    pub integer_width: Option<IntegerWidth>,
    /// The source width selected for an integer cast, when this value is a
    /// cast. Keeping both sides here prevents an adapter from re-deriving
    /// cast width or signedness from the source type.
    pub source_integer_width: Option<IntegerWidth>,
    pub comparison: Option<ComparisonPreparation>,
    pub storage: StoragePolicy,
    /// StrBuf has a fixed three-slot ownership representation even though it
    /// is otherwise handled through the ordinary aggregate slot machinery.
    pub is_strbuf: bool,
}

impl ValuePlan {
    /// Decide the policy for one CFG instruction.  `value` is only used to
    /// inspect operand types; it is not retained in the returned plan.
    pub fn for_value(ctx: &CfgLowerContext<'_>, value: CfgValue) -> Self {
        let inst = ctx.cfg.get_inst(value);
        let ty = inst.ty;
        let shape = shape(ctx, ty);
        let value_width = integer_width(ty);
        let mut plan = Self {
            kind: kind(&inst.data),
            shape,
            requirement: if shape.requires_complete_slots() {
                MaterializationRequirement::CompleteSlots
            } else {
                MaterializationRequirement::Primary
            },
            integer_width: value_width,
            source_integer_width: match &inst.data {
                CfgInstData::IntCast { from_ty, .. } => integer_width(*from_ty),
                _ => None,
            },
            comparison: None,
            storage: StoragePolicy::None,
            is_strbuf: ctx.is_strbuf(ty),
        };

        plan.storage = match &inst.data {
            CfgInstData::Alloc { slot, .. }
            | CfgInstData::Load { slot }
            | CfgInstData::StorageLive { slot, .. }
            | CfgInstData::StorageDead { slot, .. } => StoragePolicy::LocalSlot { slot: *slot },
            CfgInstData::Param { index } => {
                let by_ref = ctx.cfg.is_param_by_ref(*index);
                StoragePolicy::ParameterSlot {
                    slot: *index,
                    by_ref,
                }
            }
            CfgInstData::ParamStore { param_slot, .. } => StoragePolicy::ParameterSlot {
                slot: *param_slot,
                by_ref: true,
            },
            CfgInstData::PlaceRead { .. } | CfgInstData::PlaceWrite { .. } => StoragePolicy::Place,
            _ => StoragePolicy::None,
        };

        plan.comparison = match &inst.data {
            CfgInstData::Eq(lhs, _)
            | CfgInstData::Ne(lhs, _)
            | CfgInstData::Lt(lhs, _)
            | CfgInstData::Gt(lhs, _)
            | CfgInstData::Le(lhs, _)
            | CfgInstData::Ge(lhs, _) => {
                let lhs_ty = ctx.cfg.get_inst(*lhs).ty;
                if ctx.is_string_like_for_equality(lhs_ty) {
                    Some(ComparisonPreparation::StringContent {
                        slot_count: ctx.type_slot_count(lhs_ty),
                    })
                } else if ctx.is_multislot_aggregate(lhs_ty) {
                    Some(ComparisonPreparation::Aggregate {
                        slot_count: ctx.type_slot_count(lhs_ty),
                    })
                } else if lhs_ty == Type::UNIT {
                    Some(ComparisonPreparation::Unit)
                } else {
                    Some(ComparisonPreparation::Scalar {
                        width: comparison_integer_width(lhs_ty),
                    })
                }
            }
            _ => None,
        };

        if matches!(
            inst.data,
            CfgInstData::Store { .. }
                | CfgInstData::ParamStore { .. }
                | CfgInstData::PlaceWrite { .. }
                | CfgInstData::StorageLive { .. }
                | CfgInstData::StorageDead { .. }
                | CfgInstData::Drop { .. }
        ) {
            plan.requirement = MaterializationRequirement::SideEffect;
        }

        plan
    }

    /// Enforce the no-single-slot fallback invariant at the shared boundary.
    pub fn assert_complete_slots(self, actual: usize) {
        if let ValueShape::CompleteAggregate { slot_count } = self.shape {
            assert_eq!(
                actual, slot_count as usize,
                "complete aggregate plan requires {slot_count} slots, got {actual}"
            );
        }
    }

    /// Return the already-decided width for a scalar comparison.  Aggregate,
    /// string, and unit comparisons are handled by their own shared modes.
    pub fn comparison_width(self) -> IntegerWidth {
        match self.comparison {
            Some(ComparisonPreparation::Scalar { width }) => width,
            other => panic!("scalar comparison width requested for {other:?}"),
        }
    }
}

/// Classify every current language-level CFG value variant.  Keeping this
/// match separate from target MIR code makes the classification auditable and
/// prevents either backend from silently adding a scalar fallback.
pub const fn kind(data: &CfgInstData) -> ValueKind {
    match data {
        CfgInstData::Const(_) => ValueKind::Constant,
        CfgInstData::BoolConst(_) => ValueKind::BoolConstant,
        CfgInstData::StringConst(_) => ValueKind::StringConstant,
        CfgInstData::Param { .. } => ValueKind::Parameter,
        CfgInstData::BlockParam { .. } => ValueKind::BlockParameter,
        CfgInstData::Add(..)
        | CfgInstData::Sub(..)
        | CfgInstData::Mul(..)
        | CfgInstData::Div(..)
        | CfgInstData::Mod(..) => ValueKind::BinaryArithmetic,
        CfgInstData::Eq(..)
        | CfgInstData::Ne(..)
        | CfgInstData::Lt(..)
        | CfgInstData::Gt(..)
        | CfgInstData::Le(..)
        | CfgInstData::Ge(..) => ValueKind::Comparison,
        CfgInstData::Neg(..) | CfgInstData::Not(..) => ValueKind::UnaryArithmetic,
        CfgInstData::BitNot(..) => ValueKind::Bitwise,
        CfgInstData::BitAnd(..) | CfgInstData::BitOr(..) | CfgInstData::BitXor(..) => {
            ValueKind::Bitwise
        }
        CfgInstData::Shl(..) | CfgInstData::Shr(..) => ValueKind::Shift,
        CfgInstData::Alloc { .. } => ValueKind::Allocation,
        CfgInstData::Load { .. } => ValueKind::Load,
        CfgInstData::Store { .. } => ValueKind::Store,
        CfgInstData::ParamStore { .. } => ValueKind::ParameterStore,
        CfgInstData::Call { .. } => ValueKind::Call,
        CfgInstData::Intrinsic { .. } => ValueKind::Intrinsic,
        CfgInstData::StructInit { .. } => ValueKind::StructInit,
        CfgInstData::ArrayInit { .. } => ValueKind::ArrayInit,
        CfgInstData::EnumVariant { .. } => ValueKind::EnumVariant,
        CfgInstData::EnumPayloadGet { .. } => ValueKind::EnumPayloadGet,
        CfgInstData::IntCast { .. } => ValueKind::IntegerCast,
        CfgInstData::Drop { .. } => ValueKind::Drop,
        CfgInstData::StorageLive { .. } => ValueKind::StorageLive,
        CfgInstData::StorageDead { .. } => ValueKind::StorageDead,
        CfgInstData::PlaceRead { .. } => ValueKind::PlaceRead,
        CfgInstData::PlaceWrite { .. } => ValueKind::PlaceWrite,
    }
}

fn shape(ctx: &CfgLowerContext<'_>, ty: Type) -> ValueShape {
    let slots = ctx.type_slot_count(ty);
    if ctx.is_multislot_aggregate(ty) {
        // A zero-slot struct/array is still an aggregate.  Its complete
        // representation is the empty vector, not a scalar placeholder.
        ValueShape::CompleteAggregate { slot_count: slots }
    } else if slots == 0 {
        ValueShape::ZeroSized
    } else {
        ValueShape::Scalar
    }
}

/// Select the language integer width and signedness used by all adapters.
pub fn integer_width(ty: Type) -> Option<IntegerWidth> {
    let bits = match ty.kind() {
        TypeKind::I8 | TypeKind::U8 => 8,
        TypeKind::I16 | TypeKind::U16 => 16,
        TypeKind::I32 | TypeKind::U32 => 32,
        TypeKind::I64 | TypeKind::U64 => 64,
        _ => return None,
    };
    Some(IntegerWidth {
        bits,
        signed: ty.is_signed(),
    })
}

/// Select the width used by a scalar comparison. Booleans are represented in
/// the ordinary 32-bit scalar register form; every other valid scalar
/// comparison operand must be an integer. Do not silently assign an integer
/// width to malformed or unsupported types: doing so would let the backends
/// choose a target-specific interpretation of the same invalid CFG.
pub fn comparison_integer_width(ty: Type) -> IntegerWidth {
    if ty == Type::BOOL {
        IntegerWidth {
            bits: 32,
            signed: false,
        }
    } else {
        integer_width(ty).unwrap_or_else(|| {
            panic!("scalar comparison requires an integer or bool type, got {ty:?}")
        })
    }
}

/// Shared signedness query for target encodings that need a flag or extension
/// form after the integer policy has been selected.
pub fn type_is_signed(ty: Type) -> bool {
    integer_width(ty).is_some_and(|width| width.signed)
}

/// Shared integer-width helper used by both target adapters.
pub fn type_bits(ty: Type) -> u32 {
    integer_width(ty)
        .map(|width| width.bits)
        .unwrap_or_else(|| panic!("type_bits called on non-integer type: {:?}", ty))
}

/// Return whether an integer type uses the machine's 64-bit value width.
///
/// This preserves the old `Type::is_64_bit` behavior for non-integer types:
/// those types are not 64-bit integers, so callers that are only selecting an
/// integer instruction width must receive `false` rather than forcing an
/// integer-only plan.
pub fn is_64_bit(ty: Type) -> bool {
    integer_width(ty).is_some_and(|width| width.bits == 64)
}

/// Shared integer range helper used by checked casts and division guards.
pub fn type_range(ty: Type) -> (i64, i64) {
    integer_width(ty)
        .map(integer_range)
        .unwrap_or_else(|| panic!("type_range called on non-integer type: {:?}", ty))
}

/// Return the representable range selected by a shared integer-width plan.
pub fn integer_range(width: IntegerWidth) -> (i64, i64) {
    match (width.bits, width.signed) {
        (8, true) => (i8::MIN as i64, i8::MAX as i64),
        (16, true) => (i16::MIN as i64, i16::MAX as i64),
        (32, true) => (i32::MIN as i64, i32::MAX as i64),
        (64, true) => (i64::MIN, i64::MAX),
        (8, false) => (0, u8::MAX as i64),
        (16, false) => (0, u16::MAX as i64),
        (32, false) => (0, u32::MAX as i64),
        (64, false) => (0, i64::MAX),
        _ => panic!("invalid integer width: {width:?}"),
    }
}

/// Return the by-reference parameter slots that must be preloaded before the
/// block walk.  Both backends use this exact policy and cache rule.
pub fn by_ref_param_slots(ctx: &CfgLowerContext<'_>) -> Vec<u32> {
    (0..ctx.num_params)
        .filter(|&slot| ctx.cfg.is_param_by_ref(slot))
        .collect()
}

/// Validate the shared slot policy for a value and its materialized cache.
pub fn assert_slot_policy(plan: ValuePlan, actual: usize) {
    if plan.shape.requires_complete_slots() {
        plan.assert_complete_slots(actual);
    } else if actual > 1 {
        panic!("scalar value plan unexpectedly has {actual} materialized slots");
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ComparisonPreparation, IntegerWidth, MaterializationRequirement, StoragePolicy, ValueKind,
        ValuePlan, ValueShape, assert_slot_policy, comparison_integer_width, integer_range, kind,
        type_bits, type_range,
    };
    use lasso::{Spur, ThreadedRodeo};
    use rue_air::{EnumDef, LangItem, ParamSlotModes, StructDef, StructField, TypeInternPool};
    use rue_cfg::{Cfg, CfgInst, CfgInstData, CfgValue, Place, Terminator, Type};
    use rue_span::{FileId, Span};
    use rue_target::Target;

    use crate::aarch64::{Aarch64Inst, CfgLower as Aarch64CfgLower};
    use crate::x86_64::{CfgLower as X86CfgLower, X86Inst};

    fn synthetic_cfg(
        values: impl IntoIterator<Item = CfgInst>,
        num_locals: u32,
        num_params: u32,
        param_modes: Vec<bool>,
        return_value: CfgValue,
    ) -> Cfg {
        let mut cfg = Cfg::new(
            Type::I32,
            num_locals,
            num_params,
            "value_plan_test".to_string(),
            param_modes,
        );
        let entry = cfg.new_block();
        cfg.entry = entry;
        for (index, inst) in values.into_iter().enumerate() {
            let value = cfg.add_inst(inst);
            assert_eq!(value.as_u32(), index as u32);
            cfg.get_block_mut(entry).insts.push(value);
        }
        cfg.set_terminator(
            entry,
            Terminator::Return {
                value: Some(return_value),
            },
        );
        cfg
    }

    fn inst(data: CfgInstData, ty: Type) -> CfgInst {
        CfgInst {
            data,
            ty,
            span: Span::new(0, 0),
        }
    }

    fn dummy_value() -> CfgValue {
        CfgValue::from_raw(0)
    }

    fn all_value_kinds() -> &'static [ValueKind] {
        use ValueKind::*;
        &[
            Constant,
            BoolConstant,
            StringConstant,
            Parameter,
            BlockParameter,
            BinaryArithmetic,
            Comparison,
            UnaryArithmetic,
            Bitwise,
            Shift,
            Allocation,
            Load,
            Store,
            ParameterStore,
            Call,
            Intrinsic,
            StructInit,
            ArrayInit,
            EnumVariant,
            EnumPayloadGet,
            IntegerCast,
            Drop,
            StorageLive,
            StorageDead,
            PlaceRead,
            PlaceWrite,
        ]
    }

    /// Construct one valid CFG containing every current CfgInstData variant.
    /// The exhaustive `kind` match and `all_value_kinds` inventory make adding
    /// a language-level variant a compile-time/test-time coverage obligation;
    /// the two adapters then lower this same fixture and expose each value in
    /// their debug traces.
    fn every_cfg_value_fixture() -> (
        Cfg,
        rue_air::FrozenTypeInternPool,
        ThreadedRodeo,
        Vec<CfgValue>,
    ) {
        let pool = TypeInternPool::new();
        let interner = ThreadedRodeo::new();
        let (str_id, _) = pool.register_struct(
            interner.get_or_intern("str"),
            StructDef {
                name: "str".to_string(),
                fields: vec![
                    StructField {
                        name: "ptr".to_string(),
                        ty: Type::U64,
                    },
                    StructField {
                        name: "len".to_string(),
                        ty: Type::U64,
                    },
                ],
                is_copy: true,
                is_linear: false,
                destructor: None,
                is_builtin: true,
                is_pub: false,
                file_id: FileId::DEFAULT,
            },
        );
        let (struct_id, _) = pool.register_struct(
            interner.get_or_intern("CoverageStruct"),
            StructDef {
                name: "CoverageStruct".to_string(),
                fields: vec![
                    StructField {
                        name: "left".to_string(),
                        ty: Type::I32,
                    },
                    StructField {
                        name: "right".to_string(),
                        ty: Type::I32,
                    },
                ],
                is_copy: false,
                is_linear: false,
                destructor: None,
                is_builtin: false,
                is_pub: false,
                file_id: FileId::DEFAULT,
            },
        );
        let (enum_id, _) = pool.register_enum(
            interner.get_or_intern("CoverageEnum"),
            EnumDef {
                name: "CoverageEnum".to_string(),
                variants: vec!["None".to_string(), "Some".to_string()],
                variant_payloads: vec![vec![], vec![Type::I32]],
                is_pub: false,
                file_id: FileId::DEFAULT,
            },
        );
        let array_id = pool.intern_array_from_type(Type::I32, 2);
        let str_ty = Type::new_struct(str_id);
        let struct_ty = Type::new_struct(struct_id);
        let enum_ty = Type::new_enum(enum_id);
        let array_ty = Type::new_array(array_id);

        let mut cfg = Cfg::new(
            Type::I32,
            2,
            1,
            "every_cfg_value_variant".to_string(),
            ParamSlotModes::new(vec![true], vec![true]),
        );
        let entry = cfg.new_block();
        cfg.entry = entry;
        let mut values = Vec::new();
        let block_parameter = cfg.add_inst(inst(CfgInstData::BlockParam { index: 0 }, Type::I32));
        cfg.get_block_mut(entry)
            .params
            .push((block_parameter, Type::I32));
        values.push(block_parameter);
        let mut add = |cfg: &mut Cfg, data: CfgInstData, ty: Type| {
            let value = cfg.add_inst(inst(data, ty));
            cfg.get_block_mut(entry).insts.push(value);
            values.push(value);
            value
        };

        let constant = add(&mut cfg, CfgInstData::Const(7), Type::I32);
        let bool_constant = add(&mut cfg, CfgInstData::BoolConst(true), Type::BOOL);
        let _string_constant = add(&mut cfg, CfgInstData::StringConst(0), str_ty);
        let _parameter = add(&mut cfg, CfgInstData::Param { index: 0 }, Type::I32);

        for data in [
            CfgInstData::Add(constant, constant),
            CfgInstData::Sub(constant, constant),
            CfgInstData::Mul(constant, constant),
            CfgInstData::Div(constant, constant),
            CfgInstData::Mod(constant, constant),
            CfgInstData::Eq(constant, constant),
            CfgInstData::Ne(constant, constant),
            CfgInstData::Lt(constant, constant),
            CfgInstData::Gt(constant, constant),
            CfgInstData::Le(constant, constant),
            CfgInstData::Ge(constant, constant),
            CfgInstData::BitAnd(constant, constant),
            CfgInstData::BitOr(constant, constant),
            CfgInstData::BitXor(constant, constant),
            CfgInstData::Shl(constant, constant),
            CfgInstData::Shr(constant, constant),
            CfgInstData::Neg(constant),
            CfgInstData::BitNot(constant),
        ] {
            add(&mut cfg, data, Type::I32);
        }
        add(&mut cfg, CfgInstData::Not(bool_constant), Type::BOOL);
        add(
            &mut cfg,
            CfgInstData::Alloc {
                slot: 0,
                init: constant,
            },
            Type::UNIT,
        );
        let load = add(&mut cfg, CfgInstData::Load { slot: 0 }, Type::I32);
        add(
            &mut cfg,
            CfgInstData::Store {
                slot: 0,
                value: constant,
            },
            Type::UNIT,
        );
        add(
            &mut cfg,
            CfgInstData::ParamStore {
                param_slot: 0,
                value: constant,
            },
            Type::UNIT,
        );
        let call_name = interner.get_or_intern("coverage_call");
        add(
            &mut cfg,
            CfgInstData::Call {
                name: call_name,
                args_start: 0,
                args_len: 0,
            },
            Type::I32,
        );
        let panic_name = interner.get_or_intern("panic");
        add(
            &mut cfg,
            CfgInstData::Intrinsic {
                name: panic_name,
                args_start: 0,
                args_len: 0,
            },
            Type::UNIT,
        );
        let (fields_start, fields_len) = cfg.push_extra([constant, load]);
        let struct_value = add(
            &mut cfg,
            CfgInstData::StructInit {
                struct_id,
                fields_start,
                fields_len,
            },
            struct_ty,
        );
        let (elements_start, elements_len) = cfg.push_extra([constant, load]);
        add(
            &mut cfg,
            CfgInstData::ArrayInit {
                elements_start,
                elements_len,
            },
            array_ty,
        );
        let (payload_start, payload_len) = cfg.push_extra([constant]);
        let enum_value = add(
            &mut cfg,
            CfgInstData::EnumVariant {
                enum_id,
                variant_index: 1,
                payload_start,
                payload_len,
            },
            enum_ty,
        );
        add(
            &mut cfg,
            CfgInstData::EnumPayloadGet {
                base: enum_value,
                enum_id,
                variant_index: 1,
                field_index: 0,
            },
            Type::I32,
        );
        add(
            &mut cfg,
            CfgInstData::IntCast {
                value: constant,
                from_ty: Type::I32,
            },
            Type::U64,
        );
        add(
            &mut cfg,
            CfgInstData::Drop {
                value: struct_value,
            },
            Type::UNIT,
        );
        add(
            &mut cfg,
            CfgInstData::StorageLive {
                slot: 1,
                local_ty: Type::I32,
            },
            Type::UNIT,
        );
        add(
            &mut cfg,
            CfgInstData::StorageDead {
                slot: 1,
                local_ty: Type::I32,
            },
            Type::UNIT,
        );
        add(
            &mut cfg,
            CfgInstData::PlaceRead {
                place: Place::local(0, Type::I32),
            },
            Type::I32,
        );
        add(
            &mut cfg,
            CfgInstData::PlaceWrite {
                place: Place::local(0, Type::I32),
                value: constant,
            },
            Type::UNIT,
        );
        cfg.set_terminator(
            entry,
            Terminator::Return {
                value: Some(constant),
            },
        );

        assert_eq!(values.len(), 40, "fixture must contain every CFG variant");
        (cfg, pool.freeze(), interner, values)
    }

    #[test]
    fn aggregate_shape_has_no_single_slot_fallback() {
        let shape = ValueShape::CompleteAggregate { slot_count: 3 };
        assert!(shape.requires_complete_slots());
        assert_eq!(shape.slot_count(), 3);
        assert!(ValueShape::CompleteAggregate { slot_count: 0 }.requires_complete_slots());
    }

    #[test]
    fn shared_integer_policy_carries_width_and_signedness() {
        assert_eq!(type_bits(Type::I8), 8);
        assert_eq!(type_bits(Type::U64), 64);
        assert_eq!(type_range(Type::I16), (i16::MIN as i64, i16::MAX as i64));
        assert_eq!(type_range(Type::U32), (0, u32::MAX as i64));
        assert_eq!(
            integer_range(IntegerWidth {
                bits: 16,
                signed: false
            }),
            (0, 65535)
        );
        assert_eq!(
            comparison_integer_width(Type::BOOL),
            IntegerWidth {
                bits: 32,
                signed: false
            }
        );
    }

    #[test]
    #[should_panic(expected = "scalar comparison requires an integer or bool type")]
    fn invalid_scalar_comparison_type_is_not_defaulted_to_32_bits() {
        comparison_integer_width(Type::UNIT);
    }

    #[test]
    #[should_panic(expected = "scalar value plan unexpectedly has 2 materialized slots")]
    fn scalar_plan_cannot_accept_multiple_slots() {
        assert_slot_policy(
            ValuePlan {
                kind: ValueKind::Load,
                shape: ValueShape::Scalar,
                requirement: MaterializationRequirement::Primary,
                integer_width: Some(IntegerWidth {
                    bits: 32,
                    signed: true,
                }),
                source_integer_width: None,
                comparison: None,
                storage: super::StoragePolicy::None,
                is_strbuf: false,
            },
            2,
        );
    }

    #[test]
    fn planner_is_exhaustive_for_every_cfg_value_variant() {
        let v = dummy_value();
        let place = Place::local(0, Type::I32);
        let symbol = Spur::default();
        let cases = vec![
            (CfgInstData::Const(1), ValueKind::Constant),
            (CfgInstData::BoolConst(true), ValueKind::BoolConstant),
            (CfgInstData::StringConst(0), ValueKind::StringConstant),
            (CfgInstData::Param { index: 0 }, ValueKind::Parameter),
            (
                CfgInstData::BlockParam { index: 0 },
                ValueKind::BlockParameter,
            ),
            (CfgInstData::Add(v, v), ValueKind::BinaryArithmetic),
            (CfgInstData::Sub(v, v), ValueKind::BinaryArithmetic),
            (CfgInstData::Mul(v, v), ValueKind::BinaryArithmetic),
            (CfgInstData::Div(v, v), ValueKind::BinaryArithmetic),
            (CfgInstData::Mod(v, v), ValueKind::BinaryArithmetic),
            (CfgInstData::Eq(v, v), ValueKind::Comparison),
            (CfgInstData::Ne(v, v), ValueKind::Comparison),
            (CfgInstData::Lt(v, v), ValueKind::Comparison),
            (CfgInstData::Gt(v, v), ValueKind::Comparison),
            (CfgInstData::Le(v, v), ValueKind::Comparison),
            (CfgInstData::Ge(v, v), ValueKind::Comparison),
            (CfgInstData::BitAnd(v, v), ValueKind::Bitwise),
            (CfgInstData::BitOr(v, v), ValueKind::Bitwise),
            (CfgInstData::BitXor(v, v), ValueKind::Bitwise),
            (CfgInstData::Shl(v, v), ValueKind::Shift),
            (CfgInstData::Shr(v, v), ValueKind::Shift),
            (CfgInstData::Neg(v), ValueKind::UnaryArithmetic),
            (CfgInstData::Not(v), ValueKind::UnaryArithmetic),
            (CfgInstData::BitNot(v), ValueKind::Bitwise),
            (
                CfgInstData::Alloc { slot: 0, init: v },
                ValueKind::Allocation,
            ),
            (CfgInstData::Load { slot: 0 }, ValueKind::Load),
            (CfgInstData::Store { slot: 0, value: v }, ValueKind::Store),
            (
                CfgInstData::ParamStore {
                    param_slot: 0,
                    value: v,
                },
                ValueKind::ParameterStore,
            ),
            (
                CfgInstData::Call {
                    name: symbol,
                    args_start: 0,
                    args_len: 0,
                },
                ValueKind::Call,
            ),
            (
                CfgInstData::Intrinsic {
                    name: symbol,
                    args_start: 0,
                    args_len: 0,
                },
                ValueKind::Intrinsic,
            ),
            (
                CfgInstData::StructInit {
                    struct_id: rue_air::StructId(0),
                    fields_start: 0,
                    fields_len: 0,
                },
                ValueKind::StructInit,
            ),
            (
                CfgInstData::ArrayInit {
                    elements_start: 0,
                    elements_len: 0,
                },
                ValueKind::ArrayInit,
            ),
            (
                CfgInstData::EnumVariant {
                    enum_id: rue_air::EnumId(0),
                    variant_index: 0,
                    payload_start: 0,
                    payload_len: 0,
                },
                ValueKind::EnumVariant,
            ),
            (
                CfgInstData::EnumPayloadGet {
                    base: v,
                    enum_id: rue_air::EnumId(0),
                    variant_index: 0,
                    field_index: 0,
                },
                ValueKind::EnumPayloadGet,
            ),
            (
                CfgInstData::IntCast {
                    value: v,
                    from_ty: Type::I32,
                },
                ValueKind::IntegerCast,
            ),
            (CfgInstData::Drop { value: v }, ValueKind::Drop),
            (
                CfgInstData::StorageLive {
                    slot: 0,
                    local_ty: Type::I32,
                },
                ValueKind::StorageLive,
            ),
            (
                CfgInstData::StorageDead {
                    slot: 0,
                    local_ty: Type::I32,
                },
                ValueKind::StorageDead,
            ),
            (CfgInstData::PlaceRead { place }, ValueKind::PlaceRead),
            (
                CfgInstData::PlaceWrite { place, value: v },
                ValueKind::PlaceWrite,
            ),
        ];

        let values = cases
            .iter()
            .map(|(data, _)| inst(data.clone(), Type::I32))
            .collect::<Vec<_>>();
        let cfg = synthetic_cfg(values, 1, 1, vec![true], dummy_value());
        let pool = rue_air::FrozenTypeInternPool::new();
        let ctx = crate::cfg_lower::CfgLowerContext::new(&cfg, &pool);

        for (index, (_, expected)) in cases.iter().enumerate() {
            let value = CfgValue::from_raw(index as u32);
            let plan = ValuePlan::for_value(&ctx, value);
            assert_eq!(plan.kind, *expected, "variant at CFG value {value}");
            assert_eq!(plan.kind, kind(&cfg.get_inst(value).data));
        }
        assert_eq!(cases.len(), 40, "test must cover every CfgInstData variant");
    }

    #[test]
    fn planner_decides_storage_comparison_and_cast_policy_from_real_cfg_values() {
        let v = dummy_value();
        let values = vec![
            inst(CfgInstData::Const(7), Type::I64),
            inst(CfgInstData::Const(9), Type::I64),
            inst(
                CfgInstData::Lt(CfgValue::from_raw(0), CfgValue::from_raw(1)),
                Type::BOOL,
            ),
            inst(CfgInstData::Alloc { slot: 2, init: v }, Type::I64),
            inst(CfgInstData::Load { slot: 2 }, Type::I64),
            inst(CfgInstData::Store { slot: 2, value: v }, Type::UNIT),
            inst(
                CfgInstData::IntCast {
                    value: v,
                    from_ty: Type::I8,
                },
                Type::U64,
            ),
        ];
        let cfg = synthetic_cfg(values, 3, 1, vec![true], CfgValue::from_raw(4));
        let pool = rue_air::FrozenTypeInternPool::new();
        let ctx = crate::cfg_lower::CfgLowerContext::new(&cfg, &pool);

        assert_eq!(
            ValuePlan::for_value(&ctx, CfgValue::from_raw(0)).integer_width,
            Some(IntegerWidth {
                bits: 64,
                signed: true
            })
        );
        assert_eq!(
            ValuePlan::for_value(&ctx, CfgValue::from_raw(2)).comparison,
            Some(ComparisonPreparation::Scalar {
                width: IntegerWidth {
                    bits: 64,
                    signed: true
                }
            })
        );
        assert_eq!(
            ValuePlan::for_value(&ctx, CfgValue::from_raw(3)).storage,
            StoragePolicy::LocalSlot { slot: 2 }
        );
        assert_eq!(
            ValuePlan::for_value(&ctx, CfgValue::from_raw(4)).storage,
            StoragePolicy::LocalSlot { slot: 2 }
        );
        assert_eq!(
            ValuePlan::for_value(&ctx, CfgValue::from_raw(5)).requirement,
            MaterializationRequirement::SideEffect
        );
        assert_eq!(
            ValuePlan::for_value(&ctx, CfgValue::from_raw(6)).integer_width,
            Some(IntegerWidth {
                bits: 64,
                signed: false
            })
        );
        assert_eq!(
            ValuePlan::for_value(&ctx, CfgValue::from_raw(6)).source_integer_width,
            Some(IntegerWidth {
                bits: 8,
                signed: true
            })
        );

        let byref_cfg = synthetic_cfg(
            vec![inst(CfgInstData::Param { index: 0 }, Type::I32)],
            0,
            1,
            vec![true],
            dummy_value(),
        );
        let byref_pool = rue_air::FrozenTypeInternPool::new();
        let byref_ctx = crate::cfg_lower::CfgLowerContext::new(&byref_cfg, &byref_pool);
        assert_eq!(
            ValuePlan::for_value(&byref_ctx, dummy_value()).storage,
            StoragePolicy::ParameterSlot {
                slot: 0,
                by_ref: true
            }
        );

        let unsigned_cfg = synthetic_cfg(
            vec![
                inst(CfgInstData::Const(1), Type::U32),
                inst(CfgInstData::Const(2), Type::U32),
                inst(
                    CfgInstData::Lt(CfgValue::from_raw(0), CfgValue::from_raw(1)),
                    Type::BOOL,
                ),
            ],
            0,
            0,
            vec![],
            CfgValue::from_raw(2),
        );
        let unsigned_pool = rue_air::FrozenTypeInternPool::new();
        let unsigned_ctx = crate::cfg_lower::CfgLowerContext::new(&unsigned_cfg, &unsigned_pool);
        assert_eq!(
            ValuePlan::for_value(&unsigned_ctx, CfgValue::from_raw(2)).comparison,
            Some(ComparisonPreparation::Scalar {
                width: IntegerWidth {
                    bits: 32,
                    signed: false
                }
            })
        );
    }

    #[test]
    fn planner_preserves_zero_sized_aggregate_and_strbuf_layouts() {
        let empty_pool = TypeInternPool::new();
        let empty_array_id = empty_pool.intern_array_from_type(Type::UNIT, 2);
        let empty_pool = empty_pool.freeze();
        let empty_array_ty = Type::new_array(empty_array_id);
        let empty_cfg = synthetic_cfg(
            vec![inst(
                CfgInstData::ArrayInit {
                    elements_start: 0,
                    elements_len: 0,
                },
                empty_array_ty,
            )],
            0,
            0,
            vec![],
            dummy_value(),
        );
        let empty_ctx = crate::cfg_lower::CfgLowerContext::new(&empty_cfg, &empty_pool);
        let empty_plan = ValuePlan::for_value(&empty_ctx, dummy_value());
        assert_eq!(
            empty_plan.shape,
            ValueShape::CompleteAggregate { slot_count: 0 }
        );
        assert_eq!(
            empty_plan.requirement,
            MaterializationRequirement::CompleteSlots
        );
        assert_slot_policy(empty_plan, 0);

        let strbuf_pool = TypeInternPool::new();
        let interner = ThreadedRodeo::new();
        let name = interner.get_or_intern("StrBuf");
        let (struct_id, _) = strbuf_pool.register_struct(
            name,
            StructDef {
                name: "StrBuf".to_string(),
                fields: vec![
                    StructField {
                        name: "ptr".to_string(),
                        ty: Type::U64,
                    },
                    StructField {
                        name: "len".to_string(),
                        ty: Type::U64,
                    },
                    StructField {
                        name: "cap".to_string(),
                        ty: Type::U64,
                    },
                ],
                is_copy: false,
                is_linear: false,
                destructor: None,
                is_builtin: true,
                is_pub: false,
                file_id: FileId::DEFAULT,
            },
        );
        strbuf_pool.set_struct_lang_item(struct_id, LangItem::StrBuf);
        let strbuf_pool = strbuf_pool.freeze();
        let strbuf_ty = Type::new_struct(struct_id);
        let strbuf_cfg = synthetic_cfg(
            vec![inst(
                CfgInstData::StructInit {
                    struct_id,
                    fields_start: 0,
                    fields_len: 0,
                },
                strbuf_ty,
            )],
            0,
            0,
            vec![],
            dummy_value(),
        );
        let strbuf_ctx = crate::cfg_lower::CfgLowerContext::new(&strbuf_cfg, &strbuf_pool);
        let strbuf_plan = ValuePlan::for_value(&strbuf_ctx, dummy_value());
        assert!(strbuf_plan.is_strbuf);
        assert_eq!(strbuf_plan.shape.slot_count(), 3);
        assert!(strbuf_plan.shape.requires_complete_slots());
    }

    #[test]
    fn both_target_adapters_consume_shared_width_and_byref_policy() {
        let values = vec![
            inst(CfgInstData::Const(7), Type::I64),
            inst(CfgInstData::Const(9), Type::I64),
            inst(
                CfgInstData::Lt(CfgValue::from_raw(0), CfgValue::from_raw(1)),
                Type::BOOL,
            ),
            inst(
                CfgInstData::IntCast {
                    value: CfgValue::from_raw(0),
                    from_ty: Type::I64,
                },
                Type::U64,
            ),
            inst(CfgInstData::Const(1), Type::I32),
        ];
        let cfg = synthetic_cfg(values, 0, 0, vec![], CfgValue::from_raw(4));
        let pool = rue_air::FrozenTypeInternPool::new();
        let interner = ThreadedRodeo::new();
        let (x86, x86_debug) = X86CfgLower::new(&cfg, &pool, &interner)
            .lower_with_debug()
            .expect("x86 fixture should lower");
        let (arm, arm_debug) = Aarch64CfgLower::new(&cfg, &pool, &interner, Target::Aarch64Linux)
            .lower_with_debug()
            .expect("AArch64 fixture should lower");

        let x86_debug_cases = x86_debug
            .blocks
            .iter()
            .flat_map(|block| block.instructions.iter())
            .map(|decision| (&decision.cfg_inst_desc, &decision.cfg_type))
            .collect::<Vec<_>>();
        let arm_debug_cases = arm_debug
            .blocks
            .iter()
            .flat_map(|block| block.instructions.iter())
            .map(|decision| (&decision.cfg_inst_desc, &decision.cfg_type))
            .collect::<Vec<_>>();
        assert_eq!(x86_debug_cases, arm_debug_cases);

        assert_eq!(
            x86.instructions()
                .iter()
                .filter(|inst| matches!(inst, X86Inst::Cmp64RR { .. }))
                .count(),
            1
        );
        assert_eq!(
            arm.instructions()
                .iter()
                .filter(|inst| matches!(inst, Aarch64Inst::Cmp64RR { .. }))
                .count(),
            1
        );

        let byref_cfg = synthetic_cfg(
            vec![inst(CfgInstData::Param { index: 0 }, Type::I32)],
            0,
            1,
            vec![true],
            dummy_value(),
        );
        let x86_byref = X86CfgLower::new(&byref_cfg, &pool, &interner)
            .lower()
            .expect("x86 by-ref fixture should lower");
        let arm_byref = Aarch64CfgLower::new(&byref_cfg, &pool, &interner, Target::Aarch64Linux)
            .lower()
            .expect("AArch64 by-ref fixture should lower");
        assert!(
            x86_byref
                .instructions()
                .iter()
                .any(|inst| matches!(inst, X86Inst::MovRMIndexed { .. }))
        );
        assert!(
            arm_byref
                .instructions()
                .iter()
                .any(|inst| matches!(inst, Aarch64Inst::LdrIndexed { .. }))
        );
    }

    #[test]
    fn both_target_debug_traces_cover_every_cfg_value_variant() {
        let (cfg, pool, interner, values) = every_cfg_value_fixture();
        let (x86, x86_debug) = X86CfgLower::new(&cfg, &pool, &interner)
            .lower_with_debug()
            .expect("x86 coverage fixture should lower");
        let (arm, arm_debug) = Aarch64CfgLower::new(&cfg, &pool, &interner, Target::Aarch64Linux)
            .lower_with_debug()
            .expect("AArch64 coverage fixture should lower");

        let signature = |debug: &crate::LoweringDebugInfo| {
            let mut entries = debug
                .blocks
                .iter()
                .flat_map(|block| block.instructions.iter())
                .map(|decision| {
                    (
                        decision.cfg_value.as_u32(),
                        decision.cfg_inst_desc.clone(),
                        decision.cfg_type.clone(),
                    )
                })
                .collect::<Vec<_>>();
            entries.sort_by_key(|entry| entry.0);
            entries
        };
        assert_eq!(signature(&x86_debug), signature(&arm_debug));

        let mut expected_ids = values
            .iter()
            .map(|value| value.as_u32())
            .collect::<Vec<_>>();
        expected_ids.sort_unstable();
        let actual_ids = signature(&x86_debug)
            .iter()
            .map(|entry| entry.0)
            .collect::<Vec<_>>();
        assert_eq!(actual_ids, expected_ids);

        for value in &values {
            let expected_kind = kind(&cfg.get_inst(*value).data);
            assert!(
                all_value_kinds().contains(&expected_kind),
                "variant {expected_kind:?} must be in the exhaustive policy inventory"
            );
            let x86_decision = x86_debug
                .blocks
                .iter()
                .flat_map(|block| block.instructions.iter())
                .find(|decision| decision.cfg_value == *value)
                .expect("x86 debug trace must contain every CFG value");
            let arm_decision = arm_debug
                .blocks
                .iter()
                .flat_map(|block| block.instructions.iter())
                .find(|decision| decision.cfg_value == *value)
                .expect("AArch64 debug trace must contain every CFG value");

            // Only the three intentional no-op policies lack target MIR: a
            // preallocated block parameter and storage lifetime markers.
            if !matches!(
                expected_kind,
                ValueKind::BlockParameter | ValueKind::StorageLive | ValueKind::StorageDead
            ) {
                assert!(
                    !x86_decision.mir_insts.is_empty(),
                    "x86 variant {expected_kind:?} must produce MIR"
                );
                assert!(
                    !arm_decision.mir_insts.is_empty(),
                    "AArch64 variant {expected_kind:?} must produce MIR"
                );
            }
        }

        // These are the aggregate cases whose complete slot policy must be
        // visible in the same fixture rather than inferred from planner-only
        // assertions.
        for value in &values {
            let ty = cfg.get_inst(*value).ty;
            if ty.is_struct() || ty.is_array() || ty.is_enum() {
                let plan = ValuePlan::for_value(
                    &crate::cfg_lower::CfgLowerContext::new(&cfg, &pool),
                    *value,
                );
                assert!(plan.shape.requires_complete_slots());
                assert!(plan.shape.slot_count() > 0 || ty.is_struct() || ty.is_array());
            }
        }

        assert!(!x86.instructions().is_empty());
        assert!(!arm.instructions().is_empty());
    }

    #[test]
    fn zero_slot_aggregate_parameter_emits_no_frame_load_on_either_target() {
        let pool = TypeInternPool::new();
        let array_id = pool.intern_array_from_type(Type::UNIT, 2);
        let pool = pool.freeze();
        let array_ty = Type::new_array(array_id);
        let mut cfg = Cfg::new(array_ty, 0, 1, "zero_slot_param".to_string(), vec![false]);
        let entry = cfg.new_block();
        cfg.entry = entry;
        let param = cfg.add_inst(inst(CfgInstData::Param { index: 0 }, array_ty));
        cfg.get_block_mut(entry).insts.push(param);
        cfg.set_terminator(entry, Terminator::Return { value: Some(param) });
        let interner = ThreadedRodeo::new();

        let x86 = X86CfgLower::new(&cfg, &pool, &interner)
            .lower()
            .expect("x86 zero-slot parameter should lower");
        let arm = Aarch64CfgLower::new(&cfg, &pool, &interner, Target::Aarch64Linux)
            .lower()
            .expect("AArch64 zero-slot parameter should lower");
        assert!(
            !x86.instructions()
                .iter()
                .any(|inst| matches!(inst, X86Inst::MovRM { .. }))
        );
        assert!(
            !arm.instructions()
                .iter()
                .any(|inst| matches!(inst, Aarch64Inst::Ldr { .. }))
        );
    }
}
