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

use lasso::Spur;
use rue_air::{EnumId, StructId, TypeKind};
use rue_cfg::{CfgInstData, CfgValue, Place, PlaceBase, Projection, Type};

use crate::call_plan::CallPlan;
use crate::cfg_lower::CfgLowerContext;
use crate::vreg::VReg;

/// Target-neutral materialized value owned by the shared lowering core.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MaterializedValue {
    pub primary: VReg,
    pub slots: Vec<VReg>,
}

/// Result returned by a value-domain event. Side-effect-only CFG nodes have no
/// represented machine value and therefore are intentionally not cached.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValueResult {
    Materialized(MaterializedValue),
    SideEffect,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComparisonOp {
    Eq,
    Ne,
    Lt,
    Gt,
    Le,
    Ge,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BitwiseOp {
    And,
    Or,
    Xor,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShiftOp {
    Left,
    Right,
}

/// The residual value domain. Calls, intrinsics, checked arithmetic, and traps
/// are intentionally excluded and use their own event hooks.
#[derive(Debug, Clone)]
pub enum ResidualValuePlan {
    Const {
        value: u64,
    },
    BoolConst {
        value: bool,
    },
    StringConst {
        string_id: u32,
    },
    Param {
        index: u32,
    },
    BlockParam {
        index: u32,
    },
    Comparison {
        op: ComparisonOp,
        lhs: MaterializedValue,
        rhs: MaterializedValue,
        leaf_types: Vec<Type>,
    },
    Bitwise {
        op: BitwiseOp,
        lhs: VReg,
        rhs: VReg,
    },
    Shift {
        op: ShiftOp,
        lhs: VReg,
        rhs: VReg,
        constant: Option<u64>,
    },
    Not {
        value: VReg,
    },
    BitNot {
        value: VReg,
    },
    Alloc {
        slot: u32,
        init: MaterializedValue,
        init_shape: ValueShape,
    },
    Load {
        slot: u32,
    },
    Store {
        destination: StoreDestination,
        value: MaterializedValue,
        value_shape: ValueShape,
    },
    ParamStore {
        param_slot: u32,
        value: MaterializedValue,
        value_shape: ValueShape,
    },
    StructInit {
        struct_id: StructId,
        fields: Vec<(MaterializedValue, ValueShape)>,
    },
    ArrayInit {
        elements: Vec<(MaterializedValue, ValueShape)>,
    },
    EnumVariant {
        enum_id: EnumId,
        variant_index: u32,
        payload: Vec<(MaterializedValue, ValueShape)>,
        total_slots: u32,
    },
    EnumPayloadGet {
        base_slots: Vec<VReg>,
        field_offset: u32,
        field_slots: u32,
    },
    IntCast {
        value: VReg,
        from_width: IntegerWidth,
        trap_symbol: &'static str,
    },
    Drop {
        actions: Vec<DropAction>,
    },
    StorageLive {
        slot: u32,
        local_ty: Type,
    },
    StorageDead {
        slot: u32,
        local_ty: Type,
    },
    PlaceRead {
        place: PlacePlan,
    },
    PlaceWrite {
        place: PlacePlan,
        value: MaterializedValue,
        value_shape: ValueShape,
    },
}

/// The shared destination decision for a whole-value store. A parameter slot
/// can be a received by-reference pointer; adapters must consume this fact
/// rather than rediscovering it from CFG slot metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoreDestination {
    FrameSlot(u32),
    ByRefParam(u32),
}

/// One already-decided cleanup call. The shared planner chooses the symbol,
/// action order, and logical ABI slot order; adapters only marshal these slots
/// using their target call convention.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DropAction {
    pub symbol: String,
    pub slots: Vec<VReg>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArithmeticPlan {
    pub operation: ArithmeticOperation,
    pub trap_symbols: crate::allocation::RuntimeTrapSymbols,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArithmeticOperation {
    Add {
        lhs: VReg,
        rhs: VReg,
        width: IntegerWidth,
    },
    Sub {
        lhs: VReg,
        rhs: VReg,
        width: IntegerWidth,
    },
    Mul {
        lhs: VReg,
        rhs: VReg,
        width: IntegerWidth,
        shift: Option<(VReg, u8)>,
    },
    Div {
        lhs: VReg,
        rhs: VReg,
        width: IntegerWidth,
    },
    Mod {
        lhs: VReg,
        rhs: VReg,
        width: IntegerWidth,
    },
    Neg {
        value: VReg,
        width: IntegerWidth,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrapPlan {
    Panic {
        message: Option<MaterializedValue>,
        symbol: String,
    },
    Assert {
        condition: VReg,
        message: Option<MaterializedValue>,
        symbol: String,
    },
}

#[derive(Debug, Clone)]
pub struct IntrinsicArgPlan {
    pub primary: VReg,
    pub slots: Vec<VReg>,
    pub slot_count: u32,
    pub integer_extension: IntegerExtension,
    pub place: Option<PlacePlan>,
    pub debug: DebugValuePlan,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DebugValuePlan {
    Bool,
    Integer(IntegerWidth),
    String,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OptionIntrinsic {
    ReadLine,
    ParseI32,
    ParseI64,
    ParseU32,
    ParseU64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntrinsicOperation {
    Option {
        intrinsic: OptionIntrinsic,
        some_discriminant: u64,
        none_discriminant: u64,
    },
    RandomU32,
    RandomU64,
    PtrToInt,
    IntToPtr,
    PtrRead,
    PtrWrite,
    PtrOffset,
    Alloc {
        element_size: u64,
    },
    Free {
        element_size: u64,
    },
    Realloc {
        element_size: u64,
    },
    AllocBytes,
    FreeBytes,
    ReallocBytes,
    ByteRead,
    ByteWrite,
    PlaceAddress,
    Debug,
    Syscall,
}

#[derive(Debug, Clone)]
pub struct IntrinsicPlan {
    pub operation: IntrinsicOperation,
    /// Shared runtime entry selected from the intrinsic semantics. Target
    /// adapters marshal this symbol through their call leaf and do not map
    /// language intrinsics to runtime names themselves.
    pub runtime_symbol: Option<String>,
    pub args: Vec<IntrinsicArgPlan>,
    pub result_ty: Type,
    pub result_slots: u32,
    pub scale: Option<crate::allocation::ScalePlan>,
}

/// Place with every dynamic index already materialized. The plan contains no
/// CFG value handles; the existing place leaf consumes this semantic record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlacePlan {
    pub base: PlaceBasePlan,
    pub base_type: Type,
    pub projections: Vec<ProjectionPlan>,
}

/// The shared address classification for a by-reference call argument. The
/// plan contains no CFG value or argument mode; projections already carry
/// materialized index vregs and parameter policy is decided here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ByRefAddressPlan {
    FrameSlot {
        slot: u32,
        low_shift: u32,
    },
    Parameter {
        slot: u32,
        by_ref: bool,
        low_shift: u32,
    },
    Place(PlacePlan),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaceBasePlan {
    Local(u32),
    Param { slot: u32, by_ref: bool },
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectionPlan {
    Field {
        struct_id: StructId,
        field_index: u32,
    },
    Index {
        array_type: Type,
        index: VReg,
    },
}

/// Shared value-domain event plan. Its operand data is normalized to vregs and
/// logical slot vectors before either adapter sees it.
#[derive(Debug, Clone)]
pub struct ValueEmissionPlan {
    pub value: ResidualValuePlan,
    pub ty: Type,
    pub policy: ValuePlan,
}

/// Hooks are result-returning domain events. The core, not an adapter, owns
/// associating the returned result with the source CFG value.
pub trait ValueLowerAdapter:
    crate::call_plan::CallMaterializer + crate::terminator_plan::TerminatorAdapter
{
    fn value_is_lowered(&self, value: CfgValue) -> bool;
    fn reserve_value_result(&mut self) -> VReg;
    fn resolve_symbol(&self, symbol: Spur) -> String;
    fn call_arg_register_budget(&self) -> usize;
    fn return_register_budget(&self) -> u32;
    fn emit_value(&mut self, plan: ValueEmissionPlan) -> ValueResult;
    fn emit_call(&mut self, plan: CallPlan) -> ValueResult;
    fn emit_intrinsic(&mut self, plan: IntrinsicPlan) -> ValueResult;
    fn emit_checked_arithmetic(&mut self, plan: ArithmeticPlan) -> ValueResult;
    fn emit_trap(&mut self, plan: TrapPlan) -> ValueResult;
    fn cache_value(&mut self, value: CfgValue, result: MaterializedValue);
}

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

/// Normalized extension required when an integer value feeds a 64-bit pointer
/// arithmetic leaf. The shared planner selects the language-level fact; each
/// adapter supplies its target instruction for that fact.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntegerExtension {
    None,
    Sign8,
    Zero8,
    Sign16,
    Zero16,
    Sign32,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AggregatePrimary {
    FirstSlot,
    Zero,
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
    pub shape: ValueShape,
    pub requirement: MaterializationRequirement,
    pub integer_width: Option<IntegerWidth>,
    /// The source width selected for an integer cast, when this value is a
    /// cast. Keeping both sides here prevents an adapter from re-deriving
    /// cast width or signedness from the source type.
    pub source_integer_width: Option<IntegerWidth>,
    pub shift_count_mask: Option<u64>,
    pub comparison: Option<ComparisonPreparation>,
    pub storage: StoragePolicy,
    pub aggregate_primary: AggregatePrimary,
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
            shift_count_mask: match &inst.data {
                CfgInstData::Shl(..) | CfgInstData::Shr(..) => Some(shift_count_mask(ty)),
                _ => None,
            },
            comparison: None,
            storage: StoragePolicy::None,
            aggregate_primary: if matches!(inst.data, CfgInstData::ArrayInit { .. }) {
                AggregatePrimary::Zero
            } else {
                AggregatePrimary::FirstSlot
            },
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
                | CfgInstData::Alloc { .. }
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

/// Build a normalized place. Dynamic indices are recursively materialized here
/// so the place leaf never needs to inspect the CFG.
fn place_plan<A: ValueLowerAdapter>(
    ctx: &CfgLowerContext<'_>,
    adapter: &mut A,
    place: Place,
) -> PlacePlan {
    let base = match place.base {
        PlaceBase::Local(slot) => PlaceBasePlan::Local(slot),
        PlaceBase::Param(slot) => PlaceBasePlan::Param {
            slot,
            by_ref: ctx.cfg.is_param_by_ref(slot),
        },
    };
    let projections = ctx
        .cfg
        .get_place_projections(&place)
        .iter()
        .map(|projection| match *projection {
            Projection::Field {
                struct_id,
                field_index,
            } => ProjectionPlan::Field {
                struct_id,
                field_index,
            },
            Projection::Index { array_type, index } => ProjectionPlan::Index {
                array_type,
                index: adapter
                    .materialize_value(index, ValuePlan::for_value(ctx, index))
                    .primary,
            },
        })
        .collect();
    PlacePlan {
        base,
        base_type: place.base_type,
        projections,
    }
}

fn operand<A: ValueLowerAdapter>(
    ctx: &CfgLowerContext<'_>,
    adapter: &mut A,
    value: CfgValue,
) -> MaterializedValue {
    adapter.materialize_value(value, ValuePlan::for_value(ctx, value))
}

fn comparison_plan<A: ValueLowerAdapter>(
    ctx: &CfgLowerContext<'_>,
    adapter: &mut A,
    op: ComparisonOp,
    lhs: CfgValue,
    rhs: CfgValue,
) -> ResidualValuePlan {
    let lhs_ty = ctx.cfg.get_inst(lhs).ty;
    let leaf_types = if ctx.is_multislot_aggregate(lhs_ty) {
        crate::types::aggregate_leaf_types(ctx.type_pool, lhs_ty)
    } else {
        Vec::new()
    };
    ResidualValuePlan::Comparison {
        op,
        lhs: operand(ctx, adapter, lhs),
        rhs: operand(ctx, adapter, rhs),
        leaf_types,
    }
}

/// Classify one already-materialized CFG value that can supply an address.
/// This is the only raw CFG addressability classifier; projection semantics
/// for `PlaceRead` remain exclusively in `place_plan`.
fn addressable_value_plan<A: ValueLowerAdapter>(
    ctx: &CfgLowerContext<'_>,
    adapter: &mut A,
    value: CfgValue,
) -> Option<ByRefAddressPlan> {
    let inst = ctx.cfg.get_inst(value);
    match inst.data {
        CfgInstData::PlaceRead { place } => {
            Some(ByRefAddressPlan::Place(place_plan(ctx, adapter, place)))
        }
        CfgInstData::Load { slot } => Some(ByRefAddressPlan::FrameSlot {
            slot,
            low_shift: ctx.type_slot_count(inst.ty).saturating_sub(1),
        }),
        CfgInstData::Param { index } => Some(ByRefAddressPlan::Parameter {
            slot: index,
            by_ref: ctx.cfg.is_param_by_ref(index),
            low_shift: ctx.type_slot_count(inst.ty).saturating_sub(1),
        }),
        _ => None,
    }
}

fn place_from_value<A: ValueLowerAdapter>(
    ctx: &CfgLowerContext<'_>,
    adapter: &mut A,
    value: CfgValue,
) -> Option<PlacePlan> {
    let base_type = ctx.cfg.get_inst(value).ty;
    addressable_value_plan(ctx, adapter, value).map(|plan| match plan {
        ByRefAddressPlan::FrameSlot { slot, .. } => PlacePlan {
            base: PlaceBasePlan::Local(slot),
            base_type,
            projections: Vec::new(),
        },
        ByRefAddressPlan::Parameter { slot, by_ref, .. } => PlacePlan {
            base: PlaceBasePlan::Param { slot, by_ref },
            base_type,
            projections: Vec::new(),
        },
        ByRefAddressPlan::Place(place) => place,
    })
}

fn drop_plan<A: ValueLowerAdapter>(
    ctx: &CfgLowerContext<'_>,
    adapter: &mut A,
    value: CfgValue,
) -> Vec<DropAction> {
    let result = operand(ctx, adapter, value);
    let shape = ValuePlan::for_value(ctx, value).shape;
    let mut slots = if shape.slot_count() == 0 {
        Vec::new()
    } else if result.slots.is_empty() {
        vec![result.primary]
    } else {
        result.slots
    };
    let dropped_ty = ctx.cfg.get_inst(value).ty;
    let mut actions = Vec::new();

    match dropped_ty.kind() {
        TypeKind::Struct(struct_id) => {
            let struct_def = ctx.type_pool.struct_def(struct_id);
            if struct_def.is_builtin {
                if let Some(destructor) = &struct_def.destructor {
                    actions.push(DropAction {
                        symbol: destructor.clone(),
                        slots,
                    });
                }
            } else {
                if let Some(destructor) = &struct_def.destructor {
                    let mut destructor_slots = slots.clone();
                    destructor_slots.reverse();
                    actions.push(DropAction {
                        symbol: destructor.clone(),
                        slots: destructor_slots,
                    });
                }
                let mut glue_slots = slots;
                let mut offset = 0usize;
                for field in &struct_def.fields {
                    let count = ctx.type_slot_count(field.ty) as usize;
                    if count > 1 && offset + count <= glue_slots.len() {
                        glue_slots[offset..offset + count].reverse();
                    }
                    offset += count;
                }
                actions.push(DropAction {
                    symbol: format!("__rue_drop_{}", ctx.type_pool.struct_symbol_name(struct_id)),
                    slots: glue_slots,
                });
            }
        }
        TypeKind::Array(array_id) => {
            let element_slots = ctx.array_element_slot_count(dropped_ty) as usize;
            if element_slots > 1 {
                for chunk in slots.chunks_mut(element_slots) {
                    chunk.reverse();
                }
            }
            actions.push(DropAction {
                symbol: crate::types::array_drop_glue_name(array_id, ctx.type_pool),
                slots,
            });
        }
        TypeKind::Enum(enum_id) => actions.push(DropAction {
            symbol: format!("__rue_drop_{}", ctx.type_pool.enum_symbol_name(enum_id)),
            slots,
        }),
        _ => unreachable!("Drop instruction reached codegen for unexpected type: {dropped_ty:?}"),
    }

    actions
}

#[derive(Debug, Clone, Copy)]
enum ResidualInput {
    Const(u64),
    BoolConst(bool),
    StringConst(u32),
    Param(u32),
    BlockParam(u32),
    Eq(CfgValue, CfgValue),
    Ne(CfgValue, CfgValue),
    Lt(CfgValue, CfgValue),
    Gt(CfgValue, CfgValue),
    Le(CfgValue, CfgValue),
    Ge(CfgValue, CfgValue),
    BitAnd(CfgValue, CfgValue),
    BitOr(CfgValue, CfgValue),
    BitXor(CfgValue, CfgValue),
    Shl(CfgValue, CfgValue),
    Shr(CfgValue, CfgValue),
    Not(CfgValue),
    BitNot(CfgValue),
    Alloc {
        slot: u32,
        init: CfgValue,
    },
    Load {
        slot: u32,
    },
    Store {
        slot: u32,
        value: CfgValue,
    },
    ParamStore {
        param_slot: u32,
        value: CfgValue,
    },
    StructInit {
        struct_id: StructId,
        fields_start: u32,
        fields_len: u32,
    },
    ArrayInit {
        elements_start: u32,
        elements_len: u32,
    },
    EnumVariant {
        enum_id: EnumId,
        variant_index: u32,
        payload_start: u32,
        payload_len: u32,
    },
    EnumPayloadGet {
        base: CfgValue,
        enum_id: EnumId,
        variant_index: u32,
        field_index: u32,
    },
    IntCast {
        value: CfgValue,
        from_ty: Type,
    },
    Drop {
        value: CfgValue,
    },
    StorageLive {
        slot: u32,
        local_ty: Type,
    },
    StorageDead {
        slot: u32,
        local_ty: Type,
    },
    PlaceRead {
        place: Place,
    },
    PlaceWrite {
        place: Place,
        value: CfgValue,
    },
}

fn residual_plan<A: ValueLowerAdapter>(
    ctx: &CfgLowerContext<'_>,
    adapter: &mut A,
    value: CfgValue,
    data: ResidualInput,
) -> ResidualValuePlan {
    match data {
        ResidualInput::Const(value) => ResidualValuePlan::Const { value },
        ResidualInput::BoolConst(value) => ResidualValuePlan::BoolConst { value },
        ResidualInput::StringConst(string_id) => ResidualValuePlan::StringConst { string_id },
        ResidualInput::Param(index) => ResidualValuePlan::Param { index },
        ResidualInput::BlockParam(index) => ResidualValuePlan::BlockParam { index },
        ResidualInput::Eq(lhs, rhs) => comparison_plan(ctx, adapter, ComparisonOp::Eq, lhs, rhs),
        ResidualInput::Ne(lhs, rhs) => comparison_plan(ctx, adapter, ComparisonOp::Ne, lhs, rhs),
        ResidualInput::Lt(lhs, rhs) => comparison_plan(ctx, adapter, ComparisonOp::Lt, lhs, rhs),
        ResidualInput::Gt(lhs, rhs) => comparison_plan(ctx, adapter, ComparisonOp::Gt, lhs, rhs),
        ResidualInput::Le(lhs, rhs) => comparison_plan(ctx, adapter, ComparisonOp::Le, lhs, rhs),
        ResidualInput::Ge(lhs, rhs) => comparison_plan(ctx, adapter, ComparisonOp::Ge, lhs, rhs),
        ResidualInput::BitAnd(lhs, rhs) => ResidualValuePlan::Bitwise {
            op: BitwiseOp::And,
            lhs: operand(ctx, adapter, lhs).primary,
            rhs: operand(ctx, adapter, rhs).primary,
        },
        ResidualInput::BitOr(lhs, rhs) => ResidualValuePlan::Bitwise {
            op: BitwiseOp::Or,
            lhs: operand(ctx, adapter, lhs).primary,
            rhs: operand(ctx, adapter, rhs).primary,
        },
        ResidualInput::BitXor(lhs, rhs) => ResidualValuePlan::Bitwise {
            op: BitwiseOp::Xor,
            lhs: operand(ctx, adapter, lhs).primary,
            rhs: operand(ctx, adapter, rhs).primary,
        },
        ResidualInput::Shl(lhs, rhs) => ResidualValuePlan::Shift {
            op: ShiftOp::Left,
            lhs: operand(ctx, adapter, lhs).primary,
            rhs: operand(ctx, adapter, rhs).primary,
            constant: match ctx.cfg.get_inst(rhs).data {
                CfgInstData::Const(value) => Some(value),
                _ => None,
            },
        },
        ResidualInput::Shr(lhs, rhs) => ResidualValuePlan::Shift {
            op: ShiftOp::Right,
            lhs: operand(ctx, adapter, lhs).primary,
            rhs: operand(ctx, adapter, rhs).primary,
            constant: match ctx.cfg.get_inst(rhs).data {
                CfgInstData::Const(value) => Some(value),
                _ => None,
            },
        },
        ResidualInput::Not(value) => ResidualValuePlan::Not {
            value: operand(ctx, adapter, value).primary,
        },
        ResidualInput::BitNot(value) => ResidualValuePlan::BitNot {
            value: operand(ctx, adapter, value).primary,
        },
        ResidualInput::Alloc { slot, init } => ResidualValuePlan::Alloc {
            slot,
            init: operand(ctx, adapter, init),
            init_shape: ValuePlan::for_value(ctx, init).shape,
        },
        ResidualInput::Load { slot } => ResidualValuePlan::Load { slot },
        ResidualInput::Store { slot, value } => ResidualValuePlan::Store {
            destination: store_destination(ctx, slot),
            value: operand(ctx, adapter, value),
            value_shape: ValuePlan::for_value(ctx, value).shape,
        },
        ResidualInput::ParamStore { param_slot, value } => ResidualValuePlan::ParamStore {
            param_slot,
            value: operand(ctx, adapter, value),
            value_shape: ValuePlan::for_value(ctx, value).shape,
        },
        ResidualInput::StructInit {
            struct_id,
            fields_start,
            fields_len,
        } => ResidualValuePlan::StructInit {
            struct_id,
            fields: ctx
                .cfg
                .get_extra(fields_start, fields_len)
                .iter()
                .copied()
                .map(|field| {
                    (
                        operand(ctx, adapter, field),
                        ValuePlan::for_value(ctx, field).shape,
                    )
                })
                .collect(),
        },
        ResidualInput::ArrayInit {
            elements_start,
            elements_len,
        } => ResidualValuePlan::ArrayInit {
            elements: ctx
                .cfg
                .get_extra(elements_start, elements_len)
                .iter()
                .copied()
                .map(|element| {
                    (
                        operand(ctx, adapter, element),
                        ValuePlan::for_value(ctx, element).shape,
                    )
                })
                .collect(),
        },
        ResidualInput::EnumVariant {
            enum_id,
            variant_index,
            payload_start,
            payload_len,
        } => ResidualValuePlan::EnumVariant {
            enum_id,
            variant_index,
            payload: ctx
                .cfg
                .get_extra(payload_start, payload_len)
                .iter()
                .copied()
                .map(|field| {
                    (
                        operand(ctx, adapter, field),
                        ValuePlan::for_value(ctx, field).shape,
                    )
                })
                .collect(),
            total_slots: ctx.type_slot_count(ctx.cfg.get_inst(value).ty),
        },
        ResidualInput::EnumPayloadGet {
            base,
            enum_id,
            variant_index,
            field_index,
        } => {
            let base = operand(ctx, adapter, base);
            ResidualValuePlan::EnumPayloadGet {
                base_slots: base.slots,
                field_offset: crate::types::enum_payload_slot_offset(
                    ctx.type_pool,
                    enum_id,
                    variant_index,
                    field_index,
                ),
                field_slots: ctx.type_slot_count(ctx.cfg.get_inst(value).ty),
            }
        }
        ResidualInput::IntCast { value, from_ty } => ResidualValuePlan::IntCast {
            value: operand(ctx, adapter, value).primary,
            from_width: integer_width(from_ty).expect("integer cast source width"),
            trap_symbol: crate::allocation::RUNTIME_TRAP_SYMBOLS.intcast_overflow,
        },
        ResidualInput::Drop { value } => ResidualValuePlan::Drop {
            actions: drop_plan(ctx, adapter, value),
        },
        ResidualInput::StorageLive { slot, local_ty } => {
            ResidualValuePlan::StorageLive { slot, local_ty }
        }
        ResidualInput::StorageDead { slot, local_ty } => {
            ResidualValuePlan::StorageDead { slot, local_ty }
        }
        ResidualInput::PlaceRead { place } => ResidualValuePlan::PlaceRead {
            place: place_plan(ctx, adapter, place),
        },
        ResidualInput::PlaceWrite { place, value } => ResidualValuePlan::PlaceWrite {
            place: place_plan(ctx, adapter, place),
            value: operand(ctx, adapter, value),
            value_shape: ValuePlan::for_value(ctx, value).shape,
        },
    }
}

fn store_destination(ctx: &CfgLowerContext<'_>, slot: u32) -> StoreDestination {
    ctx.slot_to_param_index(slot)
        .filter(|&param_slot| ctx.cfg.is_param_by_ref(param_slot))
        .map(StoreDestination::ByRefParam)
        .unwrap_or(StoreDestination::FrameSlot(slot))
}

fn cache_result<A: ValueLowerAdapter>(adapter: &mut A, value: CfgValue, result: ValueResult) {
    if let ValueResult::Materialized(result) = result {
        adapter.cache_value(value, result);
    }
}

fn residual_kind(plan: &ResidualValuePlan) -> ValueKind {
    match plan {
        ResidualValuePlan::Const { .. } => ValueKind::Constant,
        ResidualValuePlan::BoolConst { .. } => ValueKind::BoolConstant,
        ResidualValuePlan::StringConst { .. } => ValueKind::StringConstant,
        ResidualValuePlan::Param { .. } => ValueKind::Parameter,
        ResidualValuePlan::BlockParam { .. } => ValueKind::BlockParameter,
        ResidualValuePlan::Comparison { .. } => ValueKind::Comparison,
        ResidualValuePlan::Bitwise { .. } => ValueKind::Bitwise,
        ResidualValuePlan::Shift { .. } => ValueKind::Shift,
        ResidualValuePlan::Not { .. } => ValueKind::UnaryArithmetic,
        ResidualValuePlan::BitNot { .. } => ValueKind::Bitwise,
        ResidualValuePlan::Alloc { .. } => ValueKind::Allocation,
        ResidualValuePlan::Load { .. } => ValueKind::Load,
        ResidualValuePlan::Store { .. } => ValueKind::Store,
        ResidualValuePlan::ParamStore { .. } => ValueKind::ParameterStore,
        ResidualValuePlan::StructInit { .. } => ValueKind::StructInit,
        ResidualValuePlan::ArrayInit { .. } => ValueKind::ArrayInit,
        ResidualValuePlan::EnumVariant { .. } => ValueKind::EnumVariant,
        ResidualValuePlan::EnumPayloadGet { .. } => ValueKind::EnumPayloadGet,
        ResidualValuePlan::IntCast { .. } => ValueKind::IntegerCast,
        ResidualValuePlan::Drop { .. } => ValueKind::Drop,
        ResidualValuePlan::StorageLive { .. } => ValueKind::StorageLive,
        ResidualValuePlan::StorageDead { .. } => ValueKind::StorageDead,
        ResidualValuePlan::PlaceRead { .. } => ValueKind::PlaceRead,
        ResidualValuePlan::PlaceWrite { .. } => ValueKind::PlaceWrite,
    }
}

/// The sole raw CFG semantic dispatcher and cache association owner.
pub fn lower_value<A: ValueLowerAdapter>(
    ctx: &CfgLowerContext<'_>,
    adapter: &mut A,
    value: CfgValue,
) -> Option<ValueKind> {
    if adapter.value_is_lowered(value) {
        return None;
    }
    let inst = ctx.cfg.get_inst(value).clone();
    let policy = ValuePlan::for_value(ctx, value);
    macro_rules! lower_residual {
        ($input:expr) => {{
            let plan = residual_plan(ctx, adapter, value, $input);
            let kind = residual_kind(&plan);
            let result = adapter.emit_value(ValueEmissionPlan {
                value: plan,
                ty: inst.ty,
                policy,
            });
            cache_result(adapter, value, result);
            Some(kind)
        }};
    }
    match inst.data {
        CfgInstData::Add(lhs, rhs) => {
            let width = policy.integer_width.expect("add width");
            let lhs = operand(ctx, adapter, lhs).primary;
            let rhs = operand(ctx, adapter, rhs).primary;
            let result = adapter.emit_checked_arithmetic(ArithmeticPlan {
                operation: ArithmeticOperation::Add { lhs, rhs, width },
                trap_symbols: crate::allocation::RUNTIME_TRAP_SYMBOLS,
            });
            cache_result(adapter, value, result);
            Some(ValueKind::BinaryArithmetic)
        }
        CfgInstData::Sub(lhs, rhs) => {
            let width = policy.integer_width.expect("sub width");
            let lhs = operand(ctx, adapter, lhs).primary;
            let rhs = operand(ctx, adapter, rhs).primary;
            let result = adapter.emit_checked_arithmetic(ArithmeticPlan {
                operation: ArithmeticOperation::Sub { lhs, rhs, width },
                trap_symbols: crate::allocation::RUNTIME_TRAP_SYMBOLS,
            });
            cache_result(adapter, value, result);
            Some(ValueKind::BinaryArithmetic)
        }
        CfgInstData::Mul(lhs, rhs) => {
            let width = policy.integer_width.expect("mul width");
            let lhs_result = operand(ctx, adapter, lhs);
            let rhs_result = operand(ctx, adapter, rhs);
            let shift = if width.bits >= 32 {
                power_of_two_shift(ctx, lhs)
                    .map(|shift| (rhs_result.primary, shift))
                    .or_else(|| {
                        power_of_two_shift(ctx, rhs).map(|shift| (lhs_result.primary, shift))
                    })
            } else {
                None
            };
            let result = adapter.emit_checked_arithmetic(ArithmeticPlan {
                operation: ArithmeticOperation::Mul {
                    lhs: lhs_result.primary,
                    rhs: rhs_result.primary,
                    width,
                    shift,
                },
                trap_symbols: crate::allocation::RUNTIME_TRAP_SYMBOLS,
            });
            cache_result(adapter, value, result);
            Some(ValueKind::BinaryArithmetic)
        }
        CfgInstData::Div(lhs, rhs) => {
            let width = policy.integer_width.expect("div width");
            let lhs = operand(ctx, adapter, lhs).primary;
            let rhs = operand(ctx, adapter, rhs).primary;
            let result = adapter.emit_checked_arithmetic(ArithmeticPlan {
                operation: ArithmeticOperation::Div { lhs, rhs, width },
                trap_symbols: crate::allocation::RUNTIME_TRAP_SYMBOLS,
            });
            cache_result(adapter, value, result);
            Some(ValueKind::BinaryArithmetic)
        }
        CfgInstData::Mod(lhs, rhs) => {
            let width = policy.integer_width.expect("mod width");
            let lhs = operand(ctx, adapter, lhs).primary;
            let rhs = operand(ctx, adapter, rhs).primary;
            let result = adapter.emit_checked_arithmetic(ArithmeticPlan {
                operation: ArithmeticOperation::Mod { lhs, rhs, width },
                trap_symbols: crate::allocation::RUNTIME_TRAP_SYMBOLS,
            });
            cache_result(adapter, value, result);
            Some(ValueKind::BinaryArithmetic)
        }
        CfgInstData::Neg(operand_value) => {
            let width = policy.integer_width.expect("neg width");
            let operand_vreg = operand(ctx, adapter, operand_value).primary;
            let result = adapter.emit_checked_arithmetic(ArithmeticPlan {
                operation: ArithmeticOperation::Neg {
                    value: operand_vreg,
                    width,
                },
                trap_symbols: crate::allocation::RUNTIME_TRAP_SYMBOLS,
            });
            cache_result(adapter, value, result);
            Some(ValueKind::UnaryArithmetic)
        }
        CfgInstData::Call {
            name,
            args_start,
            args_len,
        } => {
            let call_args = ctx.cfg.get_call_args(args_start, args_len);
            let by_ref_plans = call_args
                .iter()
                .map(|arg| match arg.mode {
                    rue_cfg::CfgArgMode::Inout | rue_cfg::CfgArgMode::Borrow => Some(
                        addressable_value_plan(ctx, adapter, arg.value).unwrap_or_else(|| {
                            panic!(
                                "malformed CFG: by-ref call argument is not an addressable place"
                            )
                        }),
                    ),
                    rue_cfg::CfgArgMode::Normal => None,
                })
                .collect::<Vec<_>>();
            let inputs = crate::call_plan::CallInputs::from_cfg(
                ctx.cfg,
                ctx.type_pool,
                inst.ty,
                call_args,
                &by_ref_plans,
                adapter.return_register_budget(),
            );
            let result_vreg = adapter.reserve_value_result();
            let symbol = adapter.resolve_symbol(name);
            let plan = crate::call_plan::CallPlan::from_inputs_with_result(
                &symbol,
                inputs.return_plan,
                &inputs.args,
                adapter.call_arg_register_budget(),
                adapter,
                Some(result_vreg),
            );
            let result = adapter.emit_call(plan);
            cache_result(adapter, value, result);
            Some(ValueKind::Call)
        }
        CfgInstData::Intrinsic {
            name,
            args_start,
            args_len,
        } => {
            let args = ctx.cfg.get_extra(args_start, args_len);
            let name_string = adapter.resolve_symbol(name);
            let values: Vec<IntrinsicArgPlan> = args
                .iter()
                .copied()
                .map(|arg| {
                    let result = operand(ctx, adapter, arg);
                    IntrinsicArgPlan {
                        primary: result.primary,
                        slots: result.slots,
                        slot_count: ValuePlan::for_value(ctx, arg).shape.slot_count(),
                        integer_extension: integer_extension(ctx.cfg.get_inst(arg).ty),
                        place: place_from_value(ctx, adapter, arg),
                        debug: debug_value_plan(ctx.cfg.get_inst(arg).ty),
                    }
                })
                .collect();
            if name_string == "panic" {
                let result = adapter.emit_trap(TrapPlan::Panic {
                    message: values.first().map(|arg| MaterializedValue {
                        primary: arg.primary,
                        slots: arg.slots.clone(),
                    }),
                    symbol: if values.first().is_some_and(|arg| arg.slots.len() >= 2) {
                        "__rue_panic".to_string()
                    } else {
                        "__rue_panic_no_msg".to_string()
                    },
                });
                cache_result(adapter, value, result);
                Some(ValueKind::Intrinsic)
            } else if name_string == "assert" {
                let result = adapter.emit_trap(TrapPlan::Assert {
                    condition: values[0].primary,
                    message: values.get(1).map(|arg| MaterializedValue {
                        primary: arg.primary,
                        slots: arg.slots.clone(),
                    }),
                    symbol: "__rue_assert_failed".to_string(),
                });
                cache_result(adapter, value, result);
                Some(ValueKind::Intrinsic)
            } else {
                let operation = match name_string.as_str() {
                    "read_line" => IntrinsicOperation::Option {
                        intrinsic: OptionIntrinsic::ReadLine,
                        some_discriminant: crate::types::option_variant_discriminants(
                            ctx.type_pool,
                            inst.ty,
                        )
                        .0,
                        none_discriminant: crate::types::option_variant_discriminants(
                            ctx.type_pool,
                            inst.ty,
                        )
                        .1,
                    },
                    "parse_i32" => IntrinsicOperation::Option {
                        intrinsic: OptionIntrinsic::ParseI32,
                        some_discriminant: crate::types::option_variant_discriminants(
                            ctx.type_pool,
                            inst.ty,
                        )
                        .0,
                        none_discriminant: crate::types::option_variant_discriminants(
                            ctx.type_pool,
                            inst.ty,
                        )
                        .1,
                    },
                    "parse_i64" => IntrinsicOperation::Option {
                        intrinsic: OptionIntrinsic::ParseI64,
                        some_discriminant: crate::types::option_variant_discriminants(
                            ctx.type_pool,
                            inst.ty,
                        )
                        .0,
                        none_discriminant: crate::types::option_variant_discriminants(
                            ctx.type_pool,
                            inst.ty,
                        )
                        .1,
                    },
                    "parse_u32" => IntrinsicOperation::Option {
                        intrinsic: OptionIntrinsic::ParseU32,
                        some_discriminant: crate::types::option_variant_discriminants(
                            ctx.type_pool,
                            inst.ty,
                        )
                        .0,
                        none_discriminant: crate::types::option_variant_discriminants(
                            ctx.type_pool,
                            inst.ty,
                        )
                        .1,
                    },
                    "parse_u64" => IntrinsicOperation::Option {
                        intrinsic: OptionIntrinsic::ParseU64,
                        some_discriminant: crate::types::option_variant_discriminants(
                            ctx.type_pool,
                            inst.ty,
                        )
                        .0,
                        none_discriminant: crate::types::option_variant_discriminants(
                            ctx.type_pool,
                            inst.ty,
                        )
                        .1,
                    },
                    "random_u32" => IntrinsicOperation::RandomU32,
                    "random_u64" => IntrinsicOperation::RandomU64,
                    "ptr_to_int" => IntrinsicOperation::PtrToInt,
                    "int_to_ptr" => IntrinsicOperation::IntToPtr,
                    "ptr_read" => IntrinsicOperation::PtrRead,
                    "ptr_write" => IntrinsicOperation::PtrWrite,
                    "ptr_offset" => IntrinsicOperation::PtrOffset,
                    "alloc" => IntrinsicOperation::Alloc { element_size: 8 },
                    "free" => IntrinsicOperation::Free { element_size: 8 },
                    "realloc" => IntrinsicOperation::Realloc { element_size: 8 },
                    "alloc_bytes" => IntrinsicOperation::AllocBytes,
                    "free_bytes" => IntrinsicOperation::FreeBytes,
                    "realloc_bytes" => IntrinsicOperation::ReallocBytes,
                    "byte_read" => IntrinsicOperation::ByteRead,
                    "byte_write" => IntrinsicOperation::ByteWrite,
                    "raw" | "raw_mut" | "field_ptr" => IntrinsicOperation::PlaceAddress,
                    "dbg" => IntrinsicOperation::Debug,
                    "syscall" => IntrinsicOperation::Syscall,
                    _ => panic!("unsupported intrinsic {name_string}"),
                };
                let scale = match operation {
                    IntrinsicOperation::PtrOffset if !args.is_empty() => {
                        Some(crate::allocation::pointer_offset_scale_plan(
                            ctx.type_pool,
                            ctx.cfg.get_inst(args[0]).ty,
                        ))
                    }
                    IntrinsicOperation::Alloc { .. } => Some(
                        crate::allocation::allocation_size_scale_plan(ctx.type_pool, inst.ty),
                    ),
                    IntrinsicOperation::Free { .. } | IntrinsicOperation::Realloc { .. } => {
                        Some(crate::allocation::allocation_size_scale_plan(
                            ctx.type_pool,
                            ctx.cfg.get_inst(args[0]).ty,
                        ))
                    }
                    _ => None,
                };
                let runtime_symbol = intrinsic_runtime_symbol(&operation, &values);
                let result = adapter.emit_intrinsic(IntrinsicPlan {
                    operation,
                    runtime_symbol,
                    args: values,
                    result_ty: inst.ty,
                    result_slots: ctx.type_slot_count(inst.ty),
                    scale,
                });
                cache_result(adapter, value, result);
                Some(ValueKind::Intrinsic)
            }
        }
        CfgInstData::Const(value) => lower_residual!(ResidualInput::Const(value)),
        CfgInstData::BoolConst(value) => lower_residual!(ResidualInput::BoolConst(value)),
        CfgInstData::StringConst(string_id) => {
            lower_residual!(ResidualInput::StringConst(string_id))
        }
        CfgInstData::Param { index } => lower_residual!(ResidualInput::Param(index)),
        CfgInstData::BlockParam { index } => lower_residual!(ResidualInput::BlockParam(index)),
        CfgInstData::Eq(lhs, rhs) => lower_residual!(ResidualInput::Eq(lhs, rhs)),
        CfgInstData::Ne(lhs, rhs) => lower_residual!(ResidualInput::Ne(lhs, rhs)),
        CfgInstData::Lt(lhs, rhs) => lower_residual!(ResidualInput::Lt(lhs, rhs)),
        CfgInstData::Gt(lhs, rhs) => lower_residual!(ResidualInput::Gt(lhs, rhs)),
        CfgInstData::Le(lhs, rhs) => lower_residual!(ResidualInput::Le(lhs, rhs)),
        CfgInstData::Ge(lhs, rhs) => lower_residual!(ResidualInput::Ge(lhs, rhs)),
        CfgInstData::BitAnd(lhs, rhs) => lower_residual!(ResidualInput::BitAnd(lhs, rhs)),
        CfgInstData::BitOr(lhs, rhs) => lower_residual!(ResidualInput::BitOr(lhs, rhs)),
        CfgInstData::BitXor(lhs, rhs) => lower_residual!(ResidualInput::BitXor(lhs, rhs)),
        CfgInstData::Shl(lhs, rhs) => lower_residual!(ResidualInput::Shl(lhs, rhs)),
        CfgInstData::Shr(lhs, rhs) => lower_residual!(ResidualInput::Shr(lhs, rhs)),
        CfgInstData::Not(value) => lower_residual!(ResidualInput::Not(value)),
        CfgInstData::BitNot(value) => lower_residual!(ResidualInput::BitNot(value)),
        CfgInstData::Alloc { slot, init } => {
            lower_residual!(ResidualInput::Alloc { slot, init })
        }
        CfgInstData::Load { slot } => lower_residual!(ResidualInput::Load { slot }),
        CfgInstData::Store { slot, value } => {
            lower_residual!(ResidualInput::Store { slot, value })
        }
        CfgInstData::ParamStore { param_slot, value } => {
            lower_residual!(ResidualInput::ParamStore { param_slot, value })
        }
        CfgInstData::StructInit {
            struct_id,
            fields_start,
            fields_len,
        } => lower_residual!(ResidualInput::StructInit {
            struct_id,
            fields_start,
            fields_len,
        }),
        CfgInstData::ArrayInit {
            elements_start,
            elements_len,
        } => lower_residual!(ResidualInput::ArrayInit {
            elements_start,
            elements_len,
        }),
        CfgInstData::EnumVariant {
            enum_id,
            variant_index,
            payload_start,
            payload_len,
        } => lower_residual!(ResidualInput::EnumVariant {
            enum_id,
            variant_index,
            payload_start,
            payload_len,
        }),
        CfgInstData::EnumPayloadGet {
            base,
            enum_id,
            variant_index,
            field_index,
        } => lower_residual!(ResidualInput::EnumPayloadGet {
            base,
            enum_id,
            variant_index,
            field_index,
        }),
        CfgInstData::IntCast { value, from_ty } => {
            lower_residual!(ResidualInput::IntCast { value, from_ty })
        }
        CfgInstData::Drop { value } => lower_residual!(ResidualInput::Drop { value }),
        CfgInstData::StorageLive { slot, local_ty } => {
            lower_residual!(ResidualInput::StorageLive { slot, local_ty })
        }
        CfgInstData::StorageDead { slot, local_ty } => {
            lower_residual!(ResidualInput::StorageDead { slot, local_ty })
        }
        CfgInstData::PlaceRead { place } => lower_residual!(ResidualInput::PlaceRead { place }),
        CfgInstData::PlaceWrite { place, value } => {
            lower_residual!(ResidualInput::PlaceWrite { place, value })
        }
    }
}

fn intrinsic_runtime_symbol(
    operation: &IntrinsicOperation,
    args: &[IntrinsicArgPlan],
) -> Option<String> {
    let symbol = match operation {
        IntrinsicOperation::Option { intrinsic, .. } => match intrinsic {
            OptionIntrinsic::ReadLine => "__rue_read_line",
            OptionIntrinsic::ParseI32 => "__rue_parse_i32",
            OptionIntrinsic::ParseI64 => "__rue_parse_i64",
            OptionIntrinsic::ParseU32 => "__rue_parse_u32",
            OptionIntrinsic::ParseU64 => "__rue_parse_u64",
        },
        IntrinsicOperation::RandomU32 => "__rue_random_u32",
        IntrinsicOperation::RandomU64 => "__rue_random_u64",
        IntrinsicOperation::Alloc { .. } | IntrinsicOperation::AllocBytes => "__rue_alloc",
        IntrinsicOperation::Free { .. } | IntrinsicOperation::FreeBytes => "__rue_free",
        IntrinsicOperation::Realloc { .. } | IntrinsicOperation::ReallocBytes => "__rue_realloc",
        IntrinsicOperation::Debug => {
            let arg = args.first()?;
            if arg.slots.len() >= 2 {
                "__rue_dbg_str"
            } else {
                match arg.debug {
                    DebugValuePlan::Bool => "__rue_dbg_bool",
                    DebugValuePlan::Integer(IntegerWidth { signed: true, .. }) => "__rue_dbg_i64",
                    DebugValuePlan::Integer(IntegerWidth { signed: false, .. }) => "__rue_dbg_u64",
                    DebugValuePlan::String | DebugValuePlan::Other => return None,
                }
            }
        }
        IntrinsicOperation::PtrToInt
        | IntrinsicOperation::IntToPtr
        | IntrinsicOperation::PtrRead
        | IntrinsicOperation::PtrWrite
        | IntrinsicOperation::PtrOffset
        | IntrinsicOperation::ByteRead
        | IntrinsicOperation::ByteWrite
        | IntrinsicOperation::PlaceAddress
        | IntrinsicOperation::Syscall => return None,
    };
    Some(symbol.to_string())
}

fn power_of_two_shift(ctx: &CfgLowerContext<'_>, value: CfgValue) -> Option<u8> {
    match ctx.cfg.get_inst(value).data {
        CfgInstData::Const(value) if value.is_power_of_two() => Some(value.trailing_zeros() as u8),
        _ => None,
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

/// Select the width used by a scalar comparison. Booleans and discriminant-only
/// enums are represented in the ordinary 32-bit unsigned scalar register form;
/// every other valid scalar comparison operand must be an integer. Do not
/// silently assign an integer width to malformed or unsupported types: doing so
/// would let the backends choose a target-specific interpretation of the same
/// invalid CFG.
pub fn comparison_integer_width(ty: Type) -> IntegerWidth {
    if ty == Type::BOOL || ty.is_enum() {
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

/// Select the extension needed before an integer becomes a 64-bit pointer
/// offset. Values already represented at full width need no instruction.
pub fn integer_extension(ty: Type) -> IntegerExtension {
    match integer_width(ty) {
        None => IntegerExtension::None,
        Some(IntegerWidth {
            bits: 8,
            signed: true,
        }) => IntegerExtension::Sign8,
        Some(IntegerWidth {
            bits: 8,
            signed: false,
        }) => IntegerExtension::Zero8,
        Some(IntegerWidth {
            bits: 16,
            signed: true,
        }) => IntegerExtension::Sign16,
        Some(IntegerWidth {
            bits: 16,
            signed: false,
        }) => IntegerExtension::Zero16,
        Some(IntegerWidth {
            bits: 32,
            signed: true,
        }) => IntegerExtension::Sign32,
        Some(_) => IntegerExtension::None,
    }
}

fn debug_value_plan(ty: Type) -> DebugValuePlan {
    if ty == Type::BOOL {
        DebugValuePlan::Bool
    } else if integer_width(ty).is_some() {
        DebugValuePlan::Integer(integer_width(ty).expect("integer width present"))
    } else if matches!(ty.kind(), TypeKind::Struct(_)) {
        DebugValuePlan::String
    } else {
        DebugValuePlan::Other
    }
}

/// Shared integer-width helper used by both target adapters.
pub fn type_bits(ty: Type) -> u32 {
    integer_width(ty)
        .map(|width| width.bits)
        .unwrap_or_else(|| panic!("type_bits called on non-integer type: {:?}", ty))
}

/// The language-level shift-count mask. Hardware details differ by target,
/// but the language operation has one width-derived count domain.
pub fn shift_count_mask(ty: Type) -> u64 {
    match type_bits(ty) {
        8 => 7,
        16 => 15,
        32 => 31,
        64 => 63,
        bits => panic!("invalid shift operand width: {bits}"),
    }
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
        ComparisonPreparation, IntegerWidth, MaterializationRequirement, StoragePolicy,
        StoreDestination, ValueKind, ValuePlan, ValueShape, assert_slot_policy,
        comparison_integer_width, integer_range, type_bits, type_range,
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
    fn enum_scalar_comparison_uses_unsigned_discriminant_width() {
        let pool = TypeInternPool::new();
        let interner = ThreadedRodeo::new();
        let enum_name = interner.get_or_intern("ComparisonEnum");
        let (enum_id, _) = pool.register_enum(
            enum_name,
            EnumDef {
                name: "ComparisonEnum".to_string(),
                variants: vec!["First".to_string(), "Second".to_string()],
                variant_payloads: vec![vec![], vec![]],
                is_pub: false,
                file_id: FileId::DEFAULT,
            },
        );
        let pool = pool.freeze();
        let enum_ty = Type::new_enum(enum_id);
        let values = vec![
            inst(CfgInstData::Const(0), enum_ty),
            inst(CfgInstData::Const(1), enum_ty),
            inst(
                CfgInstData::Eq(CfgValue::from_raw(0), CfgValue::from_raw(1)),
                Type::BOOL,
            ),
        ];
        let cfg = synthetic_cfg(values, 0, 0, vec![], CfgValue::from_raw(2));
        let ctx = crate::cfg_lower::CfgLowerContext::new(&cfg, &pool);
        assert_eq!(
            ValuePlan::for_value(&ctx, CfgValue::from_raw(2)).comparison,
            Some(ComparisonPreparation::Scalar {
                width: IntegerWidth {
                    bits: 32,
                    signed: false,
                },
            })
        );

        let x86 = X86CfgLower::new(&cfg, &pool, &interner)
            .lower()
            .expect("x86 enum comparison should lower");
        assert!(
            x86.instructions()
                .iter()
                .any(|instruction| matches!(instruction, X86Inst::CmpRR { .. }))
        );
        assert!(
            !x86.instructions()
                .iter()
                .any(|instruction| matches!(instruction, X86Inst::Cmp64RR { .. }))
        );

        let arm = Aarch64CfgLower::new(&cfg, &pool, &interner, Target::Aarch64Linux)
            .lower()
            .expect("AArch64 enum comparison should lower");
        assert!(
            arm.instructions()
                .iter()
                .any(|instruction| matches!(instruction, Aarch64Inst::CmpRR { .. }))
        );
        assert!(
            !arm.instructions()
                .iter()
                .any(|instruction| matches!(instruction, Aarch64Inst::Cmp64RR { .. }))
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
                shape: ValueShape::Scalar,
                requirement: MaterializationRequirement::Primary,
                integer_width: Some(IntegerWidth {
                    bits: 32,
                    signed: true,
                }),
                source_integer_width: None,
                shift_count_mask: None,
                comparison: None,
                storage: super::StoragePolicy::None,
                aggregate_primary: super::AggregatePrimary::FirstSlot,
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

        for (index, _) in cases.iter().enumerate() {
            let value = CfgValue::from_raw(index as u32);
            let _plan = ValuePlan::for_value(&ctx, value);
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

            assert_eq!(
                x86_decision.mir_insts.is_empty(),
                arm_decision.mir_insts.is_empty(),
                "both adapters must agree on whether a shared value is a no-op"
            );
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

    #[test]
    fn zero_slot_storage_operations_are_noops_on_both_adapters_and_byref_store_is_planned() {
        let pool = TypeInternPool::new();
        let array_id = pool.intern_array_from_type(Type::UNIT, 2);
        let pool = pool.freeze();
        let array_ty = Type::new_array(array_id);
        let mut cfg = Cfg::new(
            Type::UNIT,
            1,
            1,
            "zero_slot_storage_operations".to_string(),
            vec![false],
        );
        let entry = cfg.new_block();
        cfg.entry = entry;
        let zero = cfg.add_inst(inst(
            CfgInstData::ArrayInit {
                elements_start: 0,
                elements_len: 0,
            },
            array_ty,
        ));
        let alloc = cfg.add_inst(inst(
            CfgInstData::Alloc {
                slot: 0,
                init: zero,
            },
            Type::UNIT,
        ));
        let load = cfg.add_inst(inst(CfgInstData::Load { slot: 0 }, array_ty));
        let store = cfg.add_inst(inst(
            CfgInstData::Store {
                slot: 0,
                value: zero,
            },
            Type::UNIT,
        ));
        let param_store = cfg.add_inst(inst(
            CfgInstData::ParamStore {
                param_slot: 0,
                value: zero,
            },
            Type::UNIT,
        ));
        cfg.get_block_mut(entry)
            .insts
            .extend([zero, alloc, load, store, param_store]);
        cfg.set_terminator(entry, Terminator::Return { value: None });

        let ctx = crate::cfg_lower::CfgLowerContext::new(&cfg, &pool);
        assert_eq!(
            ValuePlan::for_value(&ctx, zero).shape,
            ValueShape::CompleteAggregate { slot_count: 0 }
        );
        assert_eq!(
            ValuePlan::for_value(&ctx, alloc).requirement,
            MaterializationRequirement::SideEffect
        );
        assert_eq!(
            ValuePlan::for_value(&ctx, load).shape,
            ValueShape::CompleteAggregate { slot_count: 0 }
        );
        assert_eq!(
            ValuePlan::for_value(&ctx, store).requirement,
            MaterializationRequirement::SideEffect
        );
        assert_eq!(
            ValuePlan::for_value(&ctx, param_store).requirement,
            MaterializationRequirement::SideEffect
        );

        let interner = ThreadedRodeo::new();
        let (_, x86_debug) = X86CfgLower::new(&cfg, &pool, &interner)
            .lower_with_debug()
            .expect("x86 zero-slot storage fixture should lower");
        let (_, arm_debug) = Aarch64CfgLower::new(&cfg, &pool, &interner, Target::Aarch64Linux)
            .lower_with_debug()
            .expect("AArch64 zero-slot storage fixture should lower");

        for value in [alloc, load, store, param_store] {
            let x86 = x86_debug
                .blocks
                .iter()
                .flat_map(|block| block.instructions.iter())
                .find(|decision| decision.cfg_value == value)
                .expect("x86 debug trace must contain zero-slot storage operation");
            let arm = arm_debug
                .blocks
                .iter()
                .flat_map(|block| block.instructions.iter())
                .find(|decision| decision.cfg_value == value)
                .expect("AArch64 debug trace must contain zero-slot storage operation");
            assert!(
                x86.mir_insts.is_empty() && arm.mir_insts.is_empty(),
                "zero-slot storage operation {value} must emit no memory MIR"
            );
        }

        let mut byref_cfg = Cfg::new(
            Type::UNIT,
            0,
            1,
            "zero_slot_byref_store".to_string(),
            vec![true],
        );
        let byref_entry = byref_cfg.new_block();
        byref_cfg.entry = byref_entry;
        let byref_zero = byref_cfg.add_inst(inst(
            CfgInstData::ArrayInit {
                elements_start: 0,
                elements_len: 0,
            },
            array_ty,
        ));
        let byref_store = byref_cfg.add_inst(inst(
            CfgInstData::Store {
                slot: 0,
                value: byref_zero,
            },
            Type::UNIT,
        ));
        byref_cfg
            .get_block_mut(byref_entry)
            .insts
            .extend([byref_zero, byref_store]);
        byref_cfg.set_terminator(byref_entry, Terminator::Return { value: None });
        let byref_ctx = crate::cfg_lower::CfgLowerContext::new(&byref_cfg, &pool);
        assert_eq!(
            super::store_destination(&byref_ctx, 0),
            StoreDestination::ByRefParam(0)
        );
    }
}
