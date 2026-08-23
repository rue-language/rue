//! CFG→CFG inlining splice primitive (ADR-0049 Phase 1).
//!
//! [`inline_call`] replaces exactly one `Call` instruction in a caller with a
//! copy of the callee's body, renumbered onto the caller's value, block, and
//! frame-slot spaces. It is a pure structural primitive: candidate selection,
//! recursion refusal, thresholds, and cache-key composition are the driver's
//! responsibility (ADR-0049 Phase 2+). The splice runs strictly after drop
//! elaboration and strictly before MIR lowering: the callee body it copies
//! already contains its elaborated `Drop`/`StorageLive`/`StorageDead`
//! instructions, and the splice never re-elaborates drops.
//!
//! # Parameter lowering follows the physical ABI
//!
//! Each callee parameter is lowered by its physical convention, queried via
//! [`Cfg::is_param_by_ref`] — never the logical `Normal`/`Inout`/`Borrow`
//! argument mode, which can diverge from it (a slice `borrow s: [T]` is a
//! fat pointer passed by value through the aggregate ABI).
//!
//! - Physically by-value: the argument is materialized into a fresh caller
//!   frame region at the parameter's **full ABI slot width** (aggregates
//!   included), bracketed by a callee-lifetime `StorageLive` before the
//!   spliced body and `StorageDead` after the continuation join.
//! - Physically by-reference: the argument is a place; parameter accesses are
//!   redirected to its root and any caller projection prefix is composed with
//!   the callee's own projections.
//!
//! # Worked elaboration example: the materialized-parameter drop
//!
//! The callee owns its by-value parameters and drops them at exit unless they
//! are moved out (RUE-61); that obligation is already elaborated into the
//! callee body when the splice runs. Before the splice:
//!
//! ```text
//! fn callee(s: StrBuf) -> i64         fn caller() -> i64
//! bb0:                                bb0:
//!   v0 : i64 = const 0                  v0 : StrBuf = ...make...
//!   v1 : StrBuf = param 0               v1 : i64 = call callee(v0)
//!   v2 : ()  = drop v1   ; exit drop    return v1
//!   return v0
//! ```
//!
//! After `inline_call` (the caller frame gains one materialized region `$m`
//! at the parameter's full ABI width):
//!
//! ```text
//! bb0:
//!   v0 : StrBuf = ...make...
//!   storage_live $m          ; callee-lifetime storage begins
//!   alloc $m, v0             ; the argument value moves into the slot
//!   goto bb2
//! bb2:                       ; copied callee body, values/slots rebased
//!   v3 : i64 = const 0
//!   v4 : StrBuf = load $m    ; was `param 0`
//!   v5 : ()  = drop v4       ; the callee's OWN copied exit drop
//!   goto bb1(v3)
//! bb1(v6 : i64):             ; continuation join
//!   storage_dead $m          ; callee-lifetime storage ends
//!   return v6                ; was `return v1`
//! ```
//!
//! One materialized slot, one live range bounded by the inlined body, one
//! drop discharged by the callee's own copied drop instructions (ADR-0049
//! §3). The splice must not synthesize an additional caller-scope drop for
//! `$m` — the value would be dropped twice — and must not drop the caller's
//! original argument value again, because move semantics transferred
//! ownership into the materialized slot. A moved-out parameter keeps the
//! callee's copied suppression (or drop-flag guard), and a `Copy` parameter
//! that needs no drop gets none: the copied body is already correct, so the
//! splice adds only the storage bracket.
//!
//! # Return wiring
//!
//! The call block is split: the instructions after the call move to a fresh
//! continuation block that takes one block parameter of the call's result
//! type (none for unit or never results). Each copied `Return` becomes a
//! `Goto` to the continuation carrying the returned value, reusing the
//! block-parameter join mechanism the CFG already has. A callee that never
//! returns produces no continuation edges, leaving the continuation
//! unreachable — the RUE-347 divergence shape.

use ahash::AHashMap;
use rue_air::{FrozenTypeInternPool, Type};
use rue_span::Span;

use crate::inst::{
    BlockId, Cfg, CfgCallArg, CfgEditError, CfgInst, CfgInstData, CfgValue, Place, PlaceBase,
    Projection, Terminator, ValidatedCfg,
};
use crate::verify::CfgVerificationError;

/// Recoverable failure from [`inline_call`]. The caller CFG is never
/// modified on failure.
#[derive(Debug)]
pub enum CfgInlineError {
    /// The requested value does not exist or is not attached to any block.
    CallSiteNotFound { call: CfgValue },
    /// The requested value is not a `Call` instruction.
    NotACall { call: CfgValue },
    /// The call site targets a typed runtime helper, which has no Rue-level
    /// body and is never an inlining candidate (ADR-0049 §6).
    RuntimeCall { call: CfgValue },
    /// The call passes a different number of arguments than the callee has
    /// source parameters.
    ArityMismatch {
        call_args: usize,
        callee_params: usize,
    },
    /// The call instruction's result type does not match the callee's return
    /// type.
    ReturnTypeMismatch {
        call_type: Type,
        callee_return_type: Type,
    },
    /// A physically by-reference argument is not a place read at all — a
    /// violated sema/CFG invariant at the call site (RUE-760).
    NonPlaceByRefArgument { arg_index: usize },
    /// The copied callee references a parameter ABI slot that is not the
    /// start slot of any of its source parameters.
    UnmappedCalleeParamSlot { slot: u32 },
    /// The callee's entry block has block parameters, which a function entry
    /// never has.
    CalleeEntryHasParams,
    /// A payload copy could not be represented or allocated.
    Edit(CfgEditError),
    /// The spliced result failed CFG verification.
    Verification(CfgVerificationError),
}

impl std::fmt::Display for CfgInlineError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::CallSiteNotFound { call } => {
                write!(f, "inline call site {call} is not attached to any block")
            }
            Self::NotACall { call } => {
                write!(f, "inline target {call} is not a Call instruction")
            }
            Self::RuntimeCall { call } => {
                write!(f, "inline target {call} is a runtime helper call")
            }
            Self::ArityMismatch {
                call_args,
                callee_params,
            } => write!(
                f,
                "call passes {call_args} arguments but the callee has {callee_params} source parameters"
            ),
            Self::ReturnTypeMismatch {
                call_type,
                callee_return_type,
            } => write!(
                f,
                "call result type {} does not match callee return type {}",
                call_type.name(),
                callee_return_type.name()
            ),
            Self::NonPlaceByRefArgument { arg_index } => {
                write!(f, "by-ref argument {arg_index} is not a place read")
            }
            Self::UnmappedCalleeParamSlot { slot } => write!(
                f,
                "callee references parameter slot {slot}, which is not a source-parameter start slot"
            ),
            Self::CalleeEntryHasParams => {
                write!(f, "callee entry block has block parameters")
            }
            Self::Edit(error) => write!(f, "inline splice edit was rejected: {error:?}"),
            Self::Verification(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for CfgInlineError {}

impl From<CfgEditError> for CfgInlineError {
    fn from(error: CfgEditError) -> Self {
        Self::Edit(error)
    }
}

/// Where accesses to one callee source parameter land in the caller.
#[derive(Debug, Clone)]
enum ParamRedirect {
    /// Physically by-value: the argument was materialized into this caller
    /// frame slot (the base of a full-ABI-width region).
    Materialized { slot: u32 },
    /// Physically by-reference with a simple caller-local root.
    ByRefPlace {
        base: PlaceBase,
        base_type: Type,
        projections: Vec<Projection>,
    },
}

/// One callee source parameter: its start ABI slot and full slot width.
struct CalleeParam {
    start_slot: u32,
    slot_count: u32,
}

/// Rebasing context for one splice: uniform value/block shifts, the local
/// frame offset, and the per-parameter redirects.
struct Splice<'a> {
    value_base: u32,
    local_base: u32,
    block_base: u32,
    redirects: &'a AHashMap<u32, ParamRedirect>,
}

impl Splice<'_> {
    fn value(&self, value: CfgValue) -> CfgValue {
        CfgValue::from_raw(value.as_u32() + self.value_base)
    }

    fn block(&self, block: BlockId) -> BlockId {
        BlockId::from_raw(block.as_u32() + self.block_base)
    }

    fn local(&self, slot: u32) -> u32 {
        slot + self.local_base
    }

    fn param(&self, slot: u32) -> Result<ParamRedirect, CfgInlineError> {
        self.redirects
            .get(&slot)
            .cloned()
            .ok_or(CfgInlineError::UnmappedCalleeParamSlot { slot })
    }
}

/// Inline exactly one `call` site of `caller`, whose target's built CFG is
/// `callee`, returning the spliced and re-verified caller. Both CFGs must
/// come from the same compilation session (shared type pool, interner, and
/// string table); the driver that owns the whole function set guarantees
/// this.
///
/// The result carries a detached value husk for the replaced `Call` (like
/// the husks optimization passes leave for DCE), so it is verified under the
/// post-optimization contract. Existing caller values keep their indices, and
/// callee arena value `v` is appended at `caller.value_count() + v`. Drivers
/// may use that stable offset to discover work introduced by the splice. The
/// copied callee blocks are appended before the continuation. Keeping the
/// defining callee blocks before the continuation is significant: lazy backend
/// materialization of a projected place's index must occur in a dominating
/// block before a by-reference consumer in the continuation.
pub fn inline_call(
    caller: &ValidatedCfg,
    call: CfgValue,
    callee: &ValidatedCfg,
    type_pool: &FrozenTypeInternPool,
) -> Result<ValidatedCfg, CfgInlineError> {
    let call_block = caller
        .blocks()
        .iter()
        .find(|block| block.insts.contains(&call))
        .map(|block| block.id)
        .ok_or(CfgInlineError::CallSiteNotFound { call })?;
    inline_call_in_block(caller, call, call_block, callee, type_pool)
}

/// Inline one call whose current attached block is already known.
///
/// This is the indexed driver entry point. It preserves the value- and block-
/// append contract documented on [`inline_call`] without rediscovering the
/// call's block by scanning the whole caller.
pub fn inline_call_in_block(
    caller: &ValidatedCfg,
    call: CfgValue,
    call_block: BlockId,
    callee: &ValidatedCfg,
    type_pool: &FrozenTypeInternPool,
) -> Result<ValidatedCfg, CfgInlineError> {
    let mut dst: Cfg = (**caller).clone();

    // -- Locate and validate the call site. ---------------------------------
    if call.as_u32() as usize >= dst.value_count() {
        return Err(CfgInlineError::CallSiteNotFound { call });
    }
    let (call_ty, call_span, call_args, is_accessor) = {
        let inst = dst.get_inst(call);
        match &inst.data {
            CfgInstData::Call { runtime, args, .. } => {
                if runtime.is_some() {
                    return Err(CfgInlineError::RuntimeCall { call });
                }
                (inst.ty, inst.span, dst.call_args(args).to_vec(), false)
            }
            CfgInstData::AccessorCall { args, .. } => {
                (inst.ty, inst.span, dst.call_args(args).to_vec(), true)
            }
            _ => return Err(CfgInlineError::NotACall { call }),
        }
    };
    if call_block.as_u32() as usize >= dst.block_count() {
        return Err(CfgInlineError::CallSiteNotFound { call });
    }
    let Some(call_position) = dst
        .get_block(call_block)
        .insts
        .iter()
        .position(|&value| value == call)
    else {
        return Err(CfgInlineError::CallSiteNotFound { call });
    };
    if callee.return_type() != call_ty {
        return Err(CfgInlineError::ReturnTypeMismatch {
            call_type: call_ty,
            callee_return_type: callee.return_type(),
        });
    }
    if !callee.get_block(callee.entry).params.is_empty() {
        return Err(CfgInlineError::CalleeEntryHasParams);
    }

    // -- Lower each parameter by its physical ABI. --------------------------
    let params = callee_params(callee);
    if params.len() != call_args.len() {
        return Err(CfgInlineError::ArityMismatch {
            call_args: call_args.len(),
            callee_params: params.len(),
        });
    }
    let mut redirects: AHashMap<u32, ParamRedirect> = AHashMap::new();
    // Materialized by-value parameter regions: (base slot, argument, type).
    let mut materialized: Vec<(u32, CfgValue, Type)> = Vec::new();
    for (index, (param, arg)) in params.iter().zip(&call_args).enumerate() {
        let redirect = if callee.is_param_by_ref(param.start_slot) {
            // The argument is a place; only a simple local/parameter root is
            // redirectable in this phase (module docs, ADR-0049 §2/§3).
            let place = byref_argument_place(&dst, arg.value, index)?;
            match place.base {
                PlaceBase::Local(slot) => {
                    if callee.is_param_address_taken(param.start_slot) {
                        dst.mark_address_taken(slot);
                    }
                }
                PlaceBase::Param(slot) => {
                    if callee.is_param_address_taken(param.start_slot) {
                        dst.mark_param_address_taken(slot);
                    }
                }
                // A nested accessor may be encountered before its producer is
                // spliced. Preserve that second-class root through this splice;
                // substituting the producer later composes the final place.
                PlaceBase::Accessor(_) => {}
                // Trusted std accessors yield an indirect place. A nested
                // accessor reborrows that exact storage, so its pointer root
                // must survive the following mandatory splice.
                PlaceBase::Indirect(_) => {}
            }
            ParamRedirect::ByRefPlace {
                base: place.base,
                base_type: place.base_type,
                projections: dst.get_place_projections(&place).to_vec(),
            }
        } else {
            let arg_ty = dst.get_inst(arg.value).ty;
            // Reserve the parameter's complete ABI region: an aggregate or
            // slice spans every decomposed slot, and reserving only the
            // first would let the following region be overwritten.
            let width = param
                .slot_count
                .max(type_pool.abi_slot_count(arg_ty))
                .max(1);
            let slot = dst.alloc_temp_local_slots(width);
            if callee.is_param_address_taken(param.start_slot) {
                // The callee took the parameter's address; the escape fact
                // must carry to the materialized slot (RUE-521).
                dst.mark_address_taken(slot);
            }
            materialized.push((slot, arg.value, arg_ty));
            ParamRedirect::Materialized { slot }
        };
        redirects.insert(param.start_slot, redirect);
    }

    // -- Rebase the callee's frame onto the caller's. -----------------------
    let local_base = if callee.num_locals() > 0 {
        dst.alloc_temp_local_slots(callee.num_locals())
    } else {
        dst.num_locals()
    };
    for slot in 0..callee.num_locals() {
        if callee.is_address_taken(slot) {
            dst.mark_address_taken(slot + local_base);
        }
    }

    // -- Copy the callee arena with uniform shifts. -------------------------
    let block_base = dst.block_count() as u32;
    for _ in 0..callee.block_count() {
        dst.new_block();
    }
    let continuation = dst.new_block();
    let splice = Splice {
        value_base: dst.value_count() as u32,
        local_base,
        block_base,
        redirects: &redirects,
    };
    let yielded = if is_accessor {
        let returned = callee
            .blocks()
            .iter()
            .find_map(|block| match block.terminator {
                Terminator::Return { value: Some(value) } => Some(value),
                _ => None,
            })
            .ok_or(CfgInlineError::ReturnTypeMismatch {
                call_type: call_ty,
                callee_return_type: callee.return_type(),
            })?;
        let CfgInstData::PlaceRead { place } = &callee.get_inst(returned).data else {
            return Err(CfgInlineError::ReturnTypeMismatch {
                call_type: call_ty,
                callee_return_type: callee.return_type(),
            });
        };
        Some((
            translate_place(&mut dst, callee, place, &splice)?,
            splice.value(returned),
        ))
    } else {
        None
    };
    for index in 0..callee.value_count() {
        let source = callee.get_inst(CfgValue::from_raw(index as u32));
        let data = translate_data(&mut dst, callee, &source.data, &splice)?;
        // Spans are copied verbatim so a future location-carrying trap
        // mechanism inherits the callee's real source position (ADR-0049 §7).
        dst.add_inst(CfgInst {
            data,
            ty: source.ty,
            span: source.span,
        });
    }

    // -- Wire the continuation join. ----------------------------------------
    // The continuation takes the call result as an explicit block parameter;
    // unit results use a fresh unit constant instead (the builder's join
    // convention), and never results have no continuation edge (RUE-347).
    let replacement = if is_accessor {
        None
    } else if call_ty != Type::UNIT && call_ty != Type::NEVER {
        Some(dst.add_block_param(continuation, call_ty))
    } else if call_ty == Type::UNIT {
        Some(dst.add_inst(CfgInst {
            data: CfgInstData::Const(0),
            ty: Type::UNIT,
            span: call_span,
        }))
    } else {
        None
    };
    let continuation_takes_value = !is_accessor && call_ty != Type::UNIT && call_ty != Type::NEVER;

    // -- Attach the copied blocks. ------------------------------------------
    for source in callee.blocks() {
        let terminator = translate_terminator(
            &mut dst,
            callee,
            &source.terminator,
            &splice,
            continuation,
            continuation_takes_value,
        )?;
        let params = source
            .params
            .iter()
            .map(|&(value, ty)| (splice.value(value), ty))
            .collect();
        let insts = source
            .insts
            .iter()
            .map(|&value| splice.value(value))
            .collect();
        let destination = dst.get_block_mut(splice.block(source.id));
        destination.params = params;
        destination.insts = insts;
        destination.terminator = terminator;
    }

    // -- Split the call block around the removed call. ----------------------
    let (mut moved_tail, original_terminator) = {
        let block = dst.get_block_mut(call_block);
        let tail = block.insts.split_off(call_position);
        let terminator = std::mem::replace(&mut block.terminator, Terminator::None);
        (tail, terminator)
    };
    // The call instruction itself is replaced by the splice; its arena entry
    // remains as a detached husk for DCE, like any optimized-away value.
    moved_tail.remove(0);

    // Callee-lifetime storage for materialized by-value parameters: live
    // immediately before the spliced body, argument moved in, dead after the
    // continuation join (ADR-0049 §3).
    for &(slot, argument, ty) in &materialized {
        let live = storage_inst(
            &mut dst,
            CfgInstData::StorageLive { slot, local_ty: ty },
            call_span,
        );
        let init = storage_inst(
            &mut dst,
            CfgInstData::Alloc {
                slot,
                init: argument,
            },
            call_span,
        );
        let block = dst.get_block_mut(call_block);
        block.insts.push(live);
        block.insts.push(init);
    }
    dst.try_set_goto(call_block, splice.block(callee.entry), std::iter::empty())?;

    let mut continuation_insts = Vec::new();
    if let (Some(value), true) = (replacement, call_ty == Type::UNIT) {
        continuation_insts.push(value);
    }
    let mut dead_materialized = Vec::with_capacity(materialized.len());
    for &(slot, _, ty) in &materialized {
        dead_materialized.push(storage_inst(
            &mut dst,
            CfgInstData::StorageDead { slot, local_ty: ty },
            call_span,
        ));
    }
    if is_accessor {
        // The yielded place may use a by-value parameter in one of its
        // projections (for example `self.items[i]`). Keep those materialized
        // parameter slots alive through the caller instructions that consume
        // the substituted place; ending them at continuation entry leaves the
        // projection index dangling on backends that rematerialize the load.
        continuation_insts.extend(moved_tail);
        continuation_insts.extend(dead_materialized);
    } else {
        continuation_insts.extend(dead_materialized);
        continuation_insts.extend(moved_tail);
    }
    {
        let block = dst.get_block_mut(continuation);
        block.insts = continuation_insts;
        block.terminator = original_terminator;
    }

    // Every use of the former call result now reads the continuation's join
    // value (or the unit constant).
    if let Some(replacement) = replacement {
        dst.rewrite_value_uses(|value| if value == call { replacement } else { value })?;
    }
    if let Some((yielded_place, yielded_value)) = yielded {
        substitute_accessor_places(&mut dst, call, &yielded_place, yielded_value)?;
    }

    dst.finish_after_optimization(type_pool)
        .map_err(CfgInlineError::Verification)
}

fn substitute_accessor_places(
    cfg: &mut Cfg,
    call: CfgValue,
    yielded: &Place,
    yielded_value: CfgValue,
) -> Result<(), CfgInlineError> {
    let mut replacements = Vec::new();
    for index in 0..cfg.value_count() {
        let value = CfgValue::from_raw(index as u32);
        let place = match &cfg.get_inst(value).data {
            CfgInstData::PlaceRead { place } | CfgInstData::PlaceWrite { place, .. }
                if place.base == PlaceBase::Accessor(call) =>
            {
                place
            }
            _ => continue,
        };
        let mut projections = cfg.get_place_projections(yielded).to_vec();
        projections.extend_from_slice(cfg.get_place_projections(place));
        let composed = cfg.make_place(yielded.base, yielded.base_type, projections)?;
        replacements.push((value, composed));
    }
    for (value, place) in replacements {
        match &mut cfg.get_inst_mut(value).data {
            CfgInstData::PlaceRead { place: target }
            | CfgInstData::PlaceWrite { place: target, .. } => *target = place,
            _ => unreachable!(),
        }
    }

    // The callee's trailing `PlaceRead` is the CFG encoding of `yield place`,
    // not a dynamic read that survives expansion. Every caller consumer now
    // names the composed yielded place directly, matching 6.6:12's reduction.
    // Detaching this structural read is also required for nested accessors:
    // otherwise its indirect pointer can be lazily materialized in an
    // earlier-numbered continuation block and then used before definition in
    // the appended inner guard blocks.
    for block_index in 0..cfg.block_count() {
        let block = BlockId::from_raw(block_index as u32);
        cfg.get_block_mut(block)
            .insts
            .retain(|value| *value != yielded_value);
    }
    Ok(())
}

/// The callee's per-source-parameter grouping: the recorded ABI descriptors
/// when present, else the synthetic one-slot-per-parameter contract used by
/// directly constructed CFGs (see `Cfg::source_param_abi`).
fn callee_params(callee: &Cfg) -> Vec<CalleeParam> {
    let descriptors = callee.source_param_abi();
    if !descriptors.is_empty() {
        descriptors
            .iter()
            .map(|descriptor| CalleeParam {
                start_slot: descriptor.start_slot,
                slot_count: descriptor.slot_count,
            })
            .collect()
    } else {
        (0..callee.num_params())
            .map(|slot| CalleeParam {
                start_slot: slot,
                slot_count: 1,
            })
            .collect()
    }
}

/// Classify a physically by-reference argument's place root. Sema only
/// accepts places here (RUE-760): a plain variable (`Load`/`Param`) or a
/// projection chain (`PlaceRead`). Projected places retain their complete
/// prefix so callee projections compose onto the caller's exact storage.
fn byref_argument_place(
    caller: &Cfg,
    argument: CfgValue,
    arg_index: usize,
) -> Result<Place, CfgInlineError> {
    match &caller.get_inst(argument).data {
        CfgInstData::Load { slot } => Ok(Place::local(*slot, caller.get_inst(argument).ty)),
        CfgInstData::Param { index } => Ok(Place::param(*index, caller.get_inst(argument).ty)),
        CfgInstData::PlaceRead { place } => Ok(place.duplicate_with_owner()),
        _ => Err(CfgInlineError::NonPlaceByRefArgument { arg_index }),
    }
}

fn storage_inst(dst: &mut Cfg, data: CfgInstData, span: Span) -> CfgValue {
    dst.add_inst(CfgInst {
        data,
        ty: Type::UNIT,
        span,
    })
}

/// Copy one callee instruction into the caller: value operands shifted,
/// local slots rebased, parameter accesses redirected, and every opaque
/// payload re-pushed into the caller's stores.
fn translate_data(
    dst: &mut Cfg,
    callee: &Cfg,
    data: &CfgInstData,
    splice: &Splice<'_>,
) -> Result<CfgInstData, CfgInlineError> {
    use CfgInstData::*;
    Ok(match data {
        Const(value) => Const(*value),
        BoolConst(value) => BoolConst(*value),
        StringConst(index) => StringConst(*index),
        BlockParam { index } => BlockParam { index: *index },
        Param { index } => match splice.param(*index)? {
            ParamRedirect::Materialized { slot } => Load { slot },
            ParamRedirect::ByRefPlace {
                base,
                base_type,
                projections,
            } if projections.is_empty() => match base {
                PlaceBase::Local(slot) => Load { slot },
                PlaceBase::Param(index) => Param { index },
                PlaceBase::Accessor(_) | PlaceBase::Indirect(_) => PlaceRead {
                    place: dst.make_place(base, base_type, projections)?,
                },
            },
            ParamRedirect::ByRefPlace {
                base,
                base_type,
                projections,
            } => PlaceRead {
                place: dst.make_place(base, base_type, projections)?,
            },
        },
        Add(a, b) => Add(splice.value(*a), splice.value(*b)),
        Sub(a, b) => Sub(splice.value(*a), splice.value(*b)),
        Mul(a, b) => Mul(splice.value(*a), splice.value(*b)),
        WrappingAdd(a, b) => WrappingAdd(splice.value(*a), splice.value(*b)),
        WrappingSub(a, b) => WrappingSub(splice.value(*a), splice.value(*b)),
        WrappingMul(a, b) => WrappingMul(splice.value(*a), splice.value(*b)),
        Div(a, b) => Div(splice.value(*a), splice.value(*b)),
        Mod(a, b) => Mod(splice.value(*a), splice.value(*b)),
        Eq(a, b) => Eq(splice.value(*a), splice.value(*b)),
        Ne(a, b) => Ne(splice.value(*a), splice.value(*b)),
        Lt(a, b) => Lt(splice.value(*a), splice.value(*b)),
        Gt(a, b) => Gt(splice.value(*a), splice.value(*b)),
        Le(a, b) => Le(splice.value(*a), splice.value(*b)),
        Ge(a, b) => Ge(splice.value(*a), splice.value(*b)),
        BitAnd(a, b) => BitAnd(splice.value(*a), splice.value(*b)),
        BitOr(a, b) => BitOr(splice.value(*a), splice.value(*b)),
        BitXor(a, b) => BitXor(splice.value(*a), splice.value(*b)),
        Shl(a, b) => Shl(splice.value(*a), splice.value(*b)),
        Shr(a, b) => Shr(splice.value(*a), splice.value(*b)),
        Neg(value) => Neg(splice.value(*value)),
        Not(value) => Not(splice.value(*value)),
        BitNot(value) => BitNot(splice.value(*value)),
        Alloc { slot, init } => Alloc {
            slot: splice.local(*slot),
            init: splice.value(*init),
        },
        Load { slot } => Load {
            slot: splice.local(*slot),
        },
        Store { slot, value } => Store {
            slot: splice.local(*slot),
            value: splice.value(*value),
        },
        ParamStore { param_slot, value } => {
            let value = splice.value(*value);
            match splice.param(*param_slot)? {
                ParamRedirect::Materialized { slot } => Store { slot, value },
                ParamRedirect::ByRefPlace {
                    base,
                    base_type,
                    projections,
                } if projections.is_empty() => match base {
                    PlaceBase::Local(slot) => Store { slot, value },
                    PlaceBase::Param(param_slot) => ParamStore { param_slot, value },
                    PlaceBase::Accessor(_) | PlaceBase::Indirect(_) => PlaceWrite {
                        place: dst.make_place(base, base_type, projections)?,
                        value,
                    },
                },
                ParamRedirect::ByRefPlace {
                    base,
                    base_type,
                    projections,
                } => PlaceWrite {
                    place: dst.make_place(base, base_type, projections)?,
                    value,
                },
            }
        }
        PlaceRead { place } => PlaceRead {
            place: translate_place(dst, callee, place, splice)?,
        },
        PlaceWrite { place, value } => PlaceWrite {
            place: translate_place(dst, callee, place, splice)?,
            value: splice.value(*value),
        },
        Call {
            runtime,
            name,
            args,
        } => {
            let args: Vec<CfgCallArg> = callee
                .call_args(args)
                .iter()
                .map(|arg| CfgCallArg {
                    value: splice.value(arg.value),
                    mode: arg.mode,
                })
                .collect();
            Call {
                runtime: *runtime,
                name: *name,
                args: dst.push_call_args(args)?,
            }
        }
        AccessorCall { name, args } => {
            let args: Vec<CfgCallArg> = callee
                .call_args(args)
                .iter()
                .map(|arg| CfgCallArg {
                    value: splice.value(arg.value),
                    mode: arg.mode,
                })
                .collect();
            AccessorCall {
                name: *name,
                args: dst.push_call_args(args)?,
            }
        }
        Intrinsic {
            runtime,
            name,
            args,
        } => {
            let args: Vec<CfgValue> = callee
                .intrinsic_args(args)
                .iter()
                .map(|&value| splice.value(value))
                .collect();
            Intrinsic {
                runtime: *runtime,
                name: *name,
                args: dst.push_intrinsic_args(args)?,
            }
        }
        StructInit { struct_id, fields } => {
            let fields: Vec<CfgValue> = callee
                .struct_fields(fields)
                .iter()
                .map(|&value| splice.value(value))
                .collect();
            StructInit {
                struct_id: *struct_id,
                fields: dst.push_struct_fields(fields)?,
            }
        }
        ArrayInit { elements } => {
            let elements: Vec<CfgValue> = callee
                .array_elements(elements)
                .iter()
                .map(|&value| splice.value(value))
                .collect();
            ArrayInit {
                elements: dst.push_array_elements(elements)?,
            }
        }
        EnumVariant {
            enum_id,
            variant_index,
            payload,
        } => {
            let payload: Vec<CfgValue> = callee
                .enum_payload(payload)
                .iter()
                .map(|&value| splice.value(value))
                .collect();
            EnumVariant {
                enum_id: *enum_id,
                variant_index: *variant_index,
                payload: dst.push_enum_payload(payload)?,
            }
        }
        EnumPayloadGet {
            base,
            enum_id,
            variant_index,
            field_index,
        } => EnumPayloadGet {
            base: splice.value(*base),
            enum_id: *enum_id,
            variant_index: *variant_index,
            field_index: *field_index,
        },
        IntCast { value, from_ty } => IntCast {
            value: splice.value(*value),
            from_ty: *from_ty,
        },
        Drop { value } => Drop {
            value: splice.value(*value),
        },
        StorageLive { slot, local_ty } => StorageLive {
            slot: splice.local(*slot),
            local_ty: *local_ty,
        },
        StorageDead { slot, local_ty } => StorageDead {
            slot: splice.local(*slot),
            local_ty: *local_ty,
        },
    })
}

/// Copy a callee place: the base is rebased (locals) or redirected
/// (parameters), and the projection chain is re-pushed into the caller's
/// projection store with index operands shifted.
fn translate_place(
    dst: &mut Cfg,
    callee: &Cfg,
    place: &Place,
    splice: &Splice<'_>,
) -> Result<Place, CfgInlineError> {
    let translated: Vec<Projection> = callee
        .get_place_projections(place)
        .iter()
        .map(|projection| match projection {
            Projection::Field {
                struct_id,
                field_index,
            } => Projection::Field {
                struct_id: *struct_id,
                field_index: *field_index,
            },
            Projection::Index { array_type, index } => Projection::Index {
                array_type: *array_type,
                index: splice.value(*index),
            },
        })
        .collect();
    let (base, base_type, mut projections) = match place.base {
        PlaceBase::Local(slot) => (
            PlaceBase::Local(splice.local(slot)),
            place.base_type,
            Vec::new(),
        ),
        PlaceBase::Param(slot) => match splice.param(slot)? {
            ParamRedirect::Materialized { slot } => {
                (PlaceBase::Local(slot), place.base_type, Vec::new())
            }
            ParamRedirect::ByRefPlace {
                base,
                base_type,
                projections,
            } => (base, base_type, projections),
        },
        PlaceBase::Accessor(value) => (
            PlaceBase::Accessor(splice.value(value)),
            place.base_type,
            Vec::new(),
        ),
        PlaceBase::Indirect(value) => (
            PlaceBase::Indirect(splice.value(value)),
            place.base_type,
            Vec::new(),
        ),
    };
    projections.extend(translated);
    dst.make_place(base, base_type, projections)
        .map_err(CfgInlineError::Edit)
}

/// Copy a callee terminator: block targets and value operands shifted, edge
/// payloads re-pushed, and each `Return` rewired as a `Goto` to the
/// continuation join carrying the returned value.
fn translate_terminator(
    dst: &mut Cfg,
    callee: &Cfg,
    terminator: &Terminator,
    splice: &Splice<'_>,
    continuation: BlockId,
    continuation_takes_value: bool,
) -> Result<Terminator, CfgInlineError> {
    Ok(match terminator {
        Terminator::Goto { target, args } => {
            let args: Vec<CfgValue> = callee
                .goto_args(args)
                .iter()
                .map(|&value| splice.value(value))
                .collect();
            Terminator::Goto {
                target: splice.block(*target),
                args: dst.push_goto_args(args)?,
            }
        }
        Terminator::Branch {
            cond,
            then_block,
            then_args,
            else_block,
            else_args,
        } => {
            let then_args_values: Vec<CfgValue> = callee
                .then_args(then_args)
                .iter()
                .map(|&value| splice.value(value))
                .collect();
            let else_args_values: Vec<CfgValue> = callee
                .else_args(else_args)
                .iter()
                .map(|&value| splice.value(value))
                .collect();
            Terminator::Branch {
                cond: splice.value(*cond),
                then_block: splice.block(*then_block),
                then_args: dst.push_then_args(then_args_values)?,
                else_block: splice.block(*else_block),
                else_args: dst.push_else_args(else_args_values)?,
            }
        }
        Terminator::Switch {
            scrutinee,
            cases,
            default,
        } => {
            let cases: Vec<(i64, BlockId)> = callee
                .switch_cases(cases)
                .iter()
                .map(|&(value, target)| (value, splice.block(target)))
                .collect();
            Terminator::Switch {
                scrutinee: splice.value(*scrutinee),
                cases: dst.push_switch_cases(cases)?,
                default: splice.block(*default),
            }
        }
        Terminator::Return { value } => {
            let args: Vec<CfgValue> = match value {
                Some(value) if continuation_takes_value => vec![splice.value(*value)],
                _ => Vec::new(),
            };
            Terminator::Goto {
                target: continuation,
                args: dst.push_goto_args(args)?,
            }
        }
        Terminator::Unreachable => Terminator::Unreachable,
        // A validated callee may keep an unfinished orphan block; its copy
        // stays unreachable in the caller too.
        Terminator::None => Terminator::None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CfgArgMode;
    use lasso::ThreadedRodeo;
    use rue_air::{
        ParamSlotModes, SourceParamAbi, StructDef, StructField, StructId, TypeInternPool,
    };
    use rue_span::FileId;

    /// A hand-built program: each function's drop-elaborated CFG, plus the
    /// shared type pool and interner the splice and its assertions need.
    struct Program {
        cfgs: AHashMap<&'static str, ValidatedCfg>,
        type_pool: rue_air::FrozenTypeInternPool,
        interner: ThreadedRodeo,
    }

    impl Program {
        fn new(type_pool: rue_air::FrozenTypeInternPool, interner: ThreadedRodeo) -> Self {
            Self {
                cfgs: AHashMap::new(),
                type_pool,
                interner,
            }
        }

        /// Verify and install one function's CFG.
        fn add(&mut self, name: &'static str, build: impl FnOnce(&ThreadedRodeo) -> Cfg) {
            let cfg = build(&self.interner);
            self.cfgs.insert(
                name,
                cfg.finish(&self.type_pool)
                    .unwrap_or_else(|error| panic!("CFG for '{name}' must verify: {error}")),
            );
        }

        fn cfg(&self, name: &str) -> &ValidatedCfg {
            self.cfgs
                .get(name)
                .unwrap_or_else(|| panic!("no CFG for function '{name}'"))
        }

        fn find_call(&self, caller: &str, callee: &str) -> CfgValue {
            let cfg = self.cfg(caller);
            attached_values(cfg)
                .find(|&value| match &cfg.get_inst(value).data {
                    CfgInstData::Call { name, .. } => self.interner.resolve(name) == callee,
                    _ => false,
                })
                .unwrap_or_else(|| panic!("no call to '{callee}' in '{caller}'"))
        }

        fn inline(&self, caller: &str, callee: &str) -> ValidatedCfg {
            self.try_inline(caller, callee)
                .unwrap_or_else(|error| panic!("inline of '{callee}' into '{caller}': {error}"))
        }

        fn try_inline(&self, caller: &str, callee: &str) -> Result<ValidatedCfg, CfgInlineError> {
            let call = self.find_call(caller, callee);
            inline_call(self.cfg(caller), call, self.cfg(callee), &self.type_pool)
        }
    }

    /// A program with an empty type pool, for scalar-only fixtures.
    fn scalar_program() -> Program {
        Program::new(TypeInternPool::new().freeze(), ThreadedRodeo::new())
    }

    /// A program whose pool holds a droppable single-slot resource struct
    /// (`Owned { cap: u64 }` with a destructor). Returns its value type.
    fn droppable_program() -> (Program, Type) {
        let interner = ThreadedRodeo::new();
        let type_pool = TypeInternPool::new();
        let (id, _) = type_pool.register_struct(
            interner.get_or_intern("Owned"),
            StructDef {
                name: "Owned".into(),
                fields: vec![StructField {
                    name: "cap".into(),
                    ty: Type::U64,
                }],
                is_copy: false,
                is_linear: false,
                declared_linear: false,
                destructor: Some("Owned.__drop".into()),
                is_builtin: false,
                is_pub: false,
                file_id: FileId::DEFAULT,
            },
        );
        let ty = Type::new_struct(id);
        (Program::new(type_pool.freeze(), interner), ty)
    }

    /// A program whose pool holds a two-slot `Pair { a: i64, b: i64 }`
    /// aggregate. Returns its struct id and value type.
    fn pair_program() -> (Program, StructId, Type) {
        let interner = ThreadedRodeo::new();
        let type_pool = TypeInternPool::new();
        let (id, _) = type_pool.register_struct(
            interner.get_or_intern("Pair"),
            StructDef {
                name: "Pair".into(),
                fields: vec![
                    StructField {
                        name: "a".into(),
                        ty: Type::I64,
                    },
                    StructField {
                        name: "b".into(),
                        ty: Type::I64,
                    },
                ],
                is_copy: false,
                is_linear: false,
                declared_linear: false,
                destructor: None,
                is_builtin: false,
                is_pub: false,
                file_id: FileId::DEFAULT,
            },
        );
        let ty = Type::new_struct(id);
        (Program::new(type_pool.freeze(), interner), id, ty)
    }

    fn inst(data: CfgInstData, ty: Type) -> CfgInst {
        CfgInst {
            data,
            ty,
            span: Span::new(0, 0),
        }
    }

    /// `fn caller() -> i64 { let s = <resource>; callee(s) }`, drop-elaborated:
    /// the moved-out local keeps its storage bracket but no caller drop, and
    /// the stores to the hidden slot 1 are its runtime drop flag (set while
    /// the local owns its value, cleared at the move).
    fn caller_moving_owned_arg(interner: &ThreadedRodeo, owned_ty: Type) -> Cfg {
        let mut cfg = Cfg::new(Type::I64, 2, 0, "caller".to_string(), Vec::<bool>::new());
        let entry = cfg.new_block();
        cfg.entry = entry;
        cfg.append_inst(
            entry,
            inst(
                CfgInstData::StorageLive {
                    slot: 0,
                    local_ty: owned_ty,
                },
                Type::UNIT,
            ),
        );
        let capacity = cfg.append_inst(entry, inst(CfgInstData::Const(8), Type::U64));
        let produced = cfg
            .append_call(
                entry,
                None,
                interner.get_or_intern("make_owned"),
                [CfgCallArg {
                    value: capacity,
                    mode: CfgArgMode::Normal,
                }],
                owned_ty,
                Span::new(0, 0),
            )
            .unwrap();
        cfg.append_inst(
            entry,
            inst(
                CfgInstData::Alloc {
                    slot: 0,
                    init: produced,
                },
                Type::UNIT,
            ),
        );
        let flag_set = cfg.append_inst(entry, inst(CfgInstData::Const(1), Type::I32));
        cfg.append_inst(
            entry,
            inst(
                CfgInstData::Store {
                    slot: 1,
                    value: flag_set,
                },
                Type::UNIT,
            ),
        );
        let argument = cfg.append_inst(entry, inst(CfgInstData::Load { slot: 0 }, owned_ty));
        let flag_clear = cfg.append_inst(entry, inst(CfgInstData::Const(0), Type::I32));
        cfg.append_inst(
            entry,
            inst(
                CfgInstData::Store {
                    slot: 1,
                    value: flag_clear,
                },
                Type::UNIT,
            ),
        );
        let call = cfg
            .append_call(
                entry,
                None,
                interner.get_or_intern("callee"),
                [CfgCallArg {
                    value: argument,
                    mode: CfgArgMode::Normal,
                }],
                Type::I64,
                Span::new(0, 0),
            )
            .unwrap();
        cfg.append_inst(
            entry,
            inst(
                CfgInstData::StorageDead {
                    slot: 0,
                    local_ty: owned_ty,
                },
                Type::UNIT,
            ),
        );
        cfg.set_return(entry, Some(call));
        cfg
    }

    /// Every value attached to a block, in block order.
    fn attached_values(cfg: &Cfg) -> impl Iterator<Item = CfgValue> + '_ {
        cfg.blocks()
            .iter()
            .flat_map(|block| block.insts.iter().copied())
    }

    #[test]
    fn many_accessor_reads_are_composed_and_structural_yield_is_detached() {
        let mut cfg = Cfg::new(Type::I64, 1, 0, "caller".to_string(), Vec::<bool>::new());
        let entry = cfg.new_block();
        cfg.entry = entry;
        let yielded_value = cfg.append_inst(
            entry,
            CfgInst {
                data: CfgInstData::Const(7),
                ty: Type::I64,
                span: Span::new(0, 0),
            },
        );
        let call = cfg.append_inst(
            entry,
            CfgInst {
                data: CfgInstData::Const(0),
                ty: Type::I64,
                span: Span::new(0, 0),
            },
        );

        let mut reads = Vec::new();
        let mut users = Vec::new();
        for _ in 0..128 {
            let read = cfg.append_inst(
                entry,
                CfgInst {
                    data: CfgInstData::PlaceRead {
                        place: Place::local(0, Type::I64).with_base(PlaceBase::Accessor(call)),
                    },
                    ty: Type::I64,
                    span: Span::new(0, 0),
                },
            );
            let user = cfg.append_inst(
                entry,
                CfgInst {
                    data: CfgInstData::Add(read, read),
                    ty: Type::I64,
                    span: Span::new(0, 0),
                },
            );
            reads.push(read);
            users.push(user);
        }
        cfg.set_return(entry, users.last().copied());

        let yielded = Place::local(0, Type::I64);
        substitute_accessor_places(&mut cfg, call, &yielded, yielded_value).unwrap();

        for (read, user) in reads.iter().copied().zip(users) {
            assert!(matches!(
                cfg.get_inst(user).data,
                CfgInstData::Add(left, right)
                    if left == read && right == read
            ));
            let CfgInstData::PlaceRead { place } = &cfg.get_inst(read).data else {
                panic!("accessor consumer must remain a place read");
            };
            assert_eq!(place.base, PlaceBase::Local(0));
        }
        let attached = cfg
            .get_block(entry)
            .insts
            .iter()
            .copied()
            .collect::<Vec<_>>();
        assert!(reads.iter().all(|read| attached.contains(read)));
        assert!(!attached.contains(&yielded_value));
    }

    fn count_matching(cfg: &Cfg, predicate: impl Fn(&CfgInstData) -> bool) -> usize {
        attached_values(cfg)
            .filter(|&value| predicate(&cfg.get_inst(value).data))
            .count()
    }

    fn count_drops(cfg: &Cfg) -> usize {
        count_matching(cfg, |data| matches!(data, CfgInstData::Drop { .. }))
    }

    fn count_calls(cfg: &Cfg) -> usize {
        count_matching(cfg, |data| matches!(data, CfgInstData::Call { .. }))
    }

    fn count_returns(cfg: &Cfg) -> usize {
        cfg.blocks()
            .iter()
            .filter(|block| matches!(block.terminator, Terminator::Return { .. }))
            .count()
    }

    fn assert_all_blocks_terminated(cfg: &Cfg) {
        for block in cfg.blocks() {
            assert!(
                !matches!(block.terminator, Terminator::None),
                "block {} has no terminator",
                block.id
            );
        }
    }

    // ------------------------------------------------------------------
    // Parameter drop matrix (ADR-0049 §3): moved-out / dropped-at-exit /
    // Copy. The double-drop hazard is the highest-risk surface.
    // ------------------------------------------------------------------

    #[test]
    fn dropped_at_exit_param_drops_exactly_once_via_copied_callee_drop() {
        // `fn callee(s: Owned) -> i64 { 0 }` — the untouched by-value param
        // is dropped by the callee at exit — inlined into a caller that moves
        // a local into the argument.
        let (mut program, owned_ty) = droppable_program();
        program.add("callee", |_| {
            let mut cfg = Cfg::new(Type::I64, 0, 1, "callee".to_string(), vec![false]);
            let entry = cfg.new_block();
            cfg.entry = entry;
            let zero = cfg.append_inst(entry, inst(CfgInstData::Const(0), Type::I64));
            let param = cfg.append_inst(entry, inst(CfgInstData::Param { index: 0 }, owned_ty));
            cfg.append_inst(entry, inst(CfgInstData::Drop { value: param }, Type::UNIT));
            cfg.set_return(entry, Some(zero));
            cfg
        });
        program.add("caller", |interner| {
            caller_moving_owned_arg(interner, owned_ty)
        });
        let caller = program.cfg("caller");
        let callee = program.cfg("callee");
        assert_eq!(count_drops(callee), 1, "callee owns and drops its param");
        let caller_locals = caller.num_locals();
        let inlined = program.inline("caller", "callee");

        // Exactly ONE drop total: the callee's copied exit drop, retargeted
        // to the materialized slot. No caller-scope drop was added and the
        // caller's moved-out argument is not dropped again.
        assert_eq!(count_drops(&inlined), 1);
        let drop_operand = attached_values(&inlined)
            .find_map(|value| match inlined.get_inst(value).data {
                CfgInstData::Drop { value } => Some(value),
                _ => None,
            })
            .unwrap();
        let CfgInstData::Load { slot } = inlined.get_inst(drop_operand).data else {
            panic!("the copied exit drop must load the materialized slot");
        };
        assert!(
            slot >= caller_locals,
            "the dropped value comes from a fresh caller slot, not an original one"
        );

        // Callee-lifetime storage bracket for the materialized slot.
        assert_eq!(
            count_matching(&inlined, |data| matches!(
                data,
                CfgInstData::StorageLive { slot: s, .. } if *s == slot
            )),
            1
        );
        assert_eq!(
            count_matching(&inlined, |data| matches!(
                data,
                CfgInstData::StorageDead { slot: s, .. } if *s == slot
            )),
            1
        );
        // Only the inlined call is consumed; the caller's StrBuf constructor
        // call is untouched.
        assert_eq!(count_calls(&inlined), count_calls(caller) - 1);
        assert_all_blocks_terminated(&inlined);
    }

    #[test]
    fn moved_out_param_drops_exactly_once_via_the_move_target() {
        // `fn callee(s: Owned) -> i64 { let t = s; 0 }`, drop-elaborated: the
        // param moves into local 0 (slot 1 holds its runtime drop flag), so
        // the exit drop covers the move target only.
        let (mut program, owned_ty) = droppable_program();
        program.add("callee", |_| {
            let mut cfg = Cfg::new(Type::I64, 2, 1, "callee".to_string(), vec![false]);
            let entry = cfg.new_block();
            cfg.entry = entry;
            let flag_set = cfg.append_inst(entry, inst(CfgInstData::Const(1), Type::I32));
            cfg.append_inst(
                entry,
                inst(
                    CfgInstData::Store {
                        slot: 1,
                        value: flag_set,
                    },
                    Type::UNIT,
                ),
            );
            cfg.append_inst(
                entry,
                inst(
                    CfgInstData::StorageLive {
                        slot: 0,
                        local_ty: owned_ty,
                    },
                    Type::UNIT,
                ),
            );
            let param = cfg.append_inst(entry, inst(CfgInstData::Param { index: 0 }, owned_ty));
            let flag_clear = cfg.append_inst(entry, inst(CfgInstData::Const(0), Type::I32));
            cfg.append_inst(
                entry,
                inst(
                    CfgInstData::Store {
                        slot: 1,
                        value: flag_clear,
                    },
                    Type::UNIT,
                ),
            );
            cfg.append_inst(
                entry,
                inst(
                    CfgInstData::Alloc {
                        slot: 0,
                        init: param,
                    },
                    Type::UNIT,
                ),
            );
            let zero = cfg.append_inst(entry, inst(CfgInstData::Const(0), Type::I64));
            let target = cfg.append_inst(entry, inst(CfgInstData::Load { slot: 0 }, owned_ty));
            cfg.append_inst(entry, inst(CfgInstData::Drop { value: target }, Type::UNIT));
            cfg.append_inst(
                entry,
                inst(
                    CfgInstData::StorageDead {
                        slot: 0,
                        local_ty: owned_ty,
                    },
                    Type::UNIT,
                ),
            );
            cfg.set_return(entry, Some(zero));
            cfg
        });
        program.add("caller", |interner| {
            caller_moving_owned_arg(interner, owned_ty)
        });
        assert_eq!(
            count_drops(program.cfg("callee")),
            1,
            "callee drops via t only; the moved-out param drop is suppressed"
        );
        let inlined = program.inline("caller", "callee");
        // Still exactly one drop after the splice: the copied t-drop. The
        // materialized parameter slot gets storage but no additional drop.
        assert_eq!(count_drops(&inlined), 1);
        assert_all_blocks_terminated(&inlined);
    }

    /// `fn callee(x: i64) -> i64 { x + 1 }`.
    fn scalar_increment_callee() -> Cfg {
        let mut cfg = Cfg::new(Type::I64, 0, 1, "callee".to_string(), vec![false]);
        let entry = cfg.new_block();
        cfg.entry = entry;
        let param = cfg.append_inst(entry, inst(CfgInstData::Param { index: 0 }, Type::I64));
        let one = cfg.append_inst(entry, inst(CfgInstData::Const(1), Type::I64));
        let sum = cfg.append_inst(entry, inst(CfgInstData::Add(param, one), Type::I64));
        cfg.set_return(entry, Some(sum));
        cfg
    }

    #[test]
    fn copy_param_materializes_storage_without_any_drop() {
        // `fn caller() -> i64 { callee(20) + 1 }` around the scalar-increment
        // callee.
        let mut program = scalar_program();
        program.add("callee", |_| scalar_increment_callee());
        program.add("caller", |interner| {
            let mut cfg = Cfg::new(Type::I64, 0, 0, "caller".to_string(), Vec::<bool>::new());
            let entry = cfg.new_block();
            cfg.entry = entry;
            let argument = cfg.append_inst(entry, inst(CfgInstData::Const(20), Type::I64));
            let call = cfg
                .append_call(
                    entry,
                    None,
                    interner.get_or_intern("callee"),
                    [CfgCallArg {
                        value: argument,
                        mode: CfgArgMode::Normal,
                    }],
                    Type::I64,
                    Span::new(0, 0),
                )
                .unwrap();
            let one = cfg.append_inst(entry, inst(CfgInstData::Const(1), Type::I64));
            let sum = cfg.append_inst(entry, inst(CfgInstData::Add(call, one), Type::I64));
            cfg.set_return(entry, Some(sum));
            cfg
        });
        let caller = program.cfg("caller");
        let callee = program.cfg("callee");
        let inlined = program.inline("caller", "callee");

        assert_eq!(count_drops(&inlined), 0, "a Copy parameter needs no drop");
        // The materialized region is exactly one i64 slot appended to the
        // caller frame, plus the callee's (empty) local frame.
        assert_eq!(
            inlined.num_locals(),
            caller.num_locals() + callee.num_locals() + 1
        );
        // Every callee Param read was rewritten; the caller has no parameters
        // of its own, so no Param instruction may remain attached.
        assert_eq!(
            count_matching(&inlined, |data| matches!(data, CfgInstData::Param { .. })),
            0
        );
        assert_eq!(count_calls(&inlined), 0);
        assert_all_blocks_terminated(&inlined);
    }

    // ------------------------------------------------------------------
    // Physical-ABI parameter lowering.
    // ------------------------------------------------------------------

    #[test]
    fn aggregate_by_value_param_materializes_at_full_abi_width() {
        // `fn get(p: Pair) -> i64 { p.a }` inlined into
        // `fn caller() -> i64 { let p = Pair { a: 1, b: 2 }; get(p) }`.
        let (mut program, pair_id, pair_ty) = pair_program();
        program.add("get", |_| {
            let mut cfg = Cfg::new(Type::I64, 0, 2, "get".to_string(), vec![false, false]);
            // One source parameter spans both flattened ABI slots.
            cfg.set_source_param_abi(vec![SourceParamAbi {
                start_slot: 0,
                slot_count: 2,
                crossing_regs: 2,
                ty: None,
            }]);
            let entry = cfg.new_block();
            cfg.entry = entry;
            let read = cfg
                .append_place_read(
                    entry,
                    PlaceBase::Param(0),
                    pair_ty,
                    [Projection::Field {
                        struct_id: pair_id,
                        field_index: 0,
                    }],
                    Type::I64,
                    Span::new(0, 0),
                )
                .unwrap();
            cfg.set_return(entry, Some(read));
            cfg
        });
        program.add("caller", |interner| {
            let mut cfg = Cfg::new(Type::I64, 2, 0, "caller".to_string(), Vec::<bool>::new());
            let entry = cfg.new_block();
            cfg.entry = entry;
            cfg.append_inst(
                entry,
                inst(
                    CfgInstData::StorageLive {
                        slot: 0,
                        local_ty: pair_ty,
                    },
                    Type::UNIT,
                ),
            );
            let one = cfg.append_inst(entry, inst(CfgInstData::Const(1), Type::I64));
            let two = cfg.append_inst(entry, inst(CfgInstData::Const(2), Type::I64));
            let pair = cfg
                .append_struct_init(entry, pair_id, [one, two], pair_ty, Span::new(0, 0))
                .unwrap();
            cfg.append_inst(
                entry,
                inst(
                    CfgInstData::Alloc {
                        slot: 0,
                        init: pair,
                    },
                    Type::UNIT,
                ),
            );
            let argument = cfg.append_inst(entry, inst(CfgInstData::Load { slot: 0 }, pair_ty));
            let call = cfg
                .append_call(
                    entry,
                    None,
                    interner.get_or_intern("get"),
                    [CfgCallArg {
                        value: argument,
                        mode: CfgArgMode::Normal,
                    }],
                    Type::I64,
                    Span::new(0, 0),
                )
                .unwrap();
            cfg.append_inst(
                entry,
                inst(
                    CfgInstData::StorageDead {
                        slot: 0,
                        local_ty: pair_ty,
                    },
                    Type::UNIT,
                ),
            );
            cfg.set_return(entry, Some(call));
            cfg
        });
        let caller = program.cfg("caller");
        let callee = program.cfg("get");
        let materialized_base = caller.num_locals();
        let inlined = program.inline("caller", "get");

        // Pair spans two ABI slots; the materialized region reserves both.
        assert_eq!(
            inlined.num_locals(),
            caller.num_locals() + callee.num_locals() + 2
        );
        // The callee's projected parameter read is redirected to the
        // materialized local root.
        assert!(
            attached_values(&inlined).any(|value| match &inlined.get_inst(value).data {
                CfgInstData::PlaceRead { place } =>
                    place.base == PlaceBase::Local(materialized_base),
                _ => false,
            }),
            "parameter place reads must root at the materialized slot"
        );
        assert_all_blocks_terminated(&inlined);
    }

    /// `fn bump(inout x: i64) { x = x + 1; }`: a by-ref scalar parameter
    /// whose body writes back through `ParamStore`.
    fn by_ref_bump_callee() -> Cfg {
        let mut cfg = Cfg::new(
            Type::UNIT,
            0,
            1,
            "bump".to_string(),
            ParamSlotModes::new(vec![true], vec![true]),
        );
        let entry = cfg.new_block();
        cfg.entry = entry;
        let param = cfg.append_inst(entry, inst(CfgInstData::Param { index: 0 }, Type::I64));
        let one = cfg.append_inst(entry, inst(CfgInstData::Const(1), Type::I64));
        let sum = cfg.append_inst(entry, inst(CfgInstData::Add(param, one), Type::I64));
        cfg.append_inst(
            entry,
            inst(
                CfgInstData::ParamStore {
                    param_slot: 0,
                    value: sum,
                },
                Type::UNIT,
            ),
        );
        cfg.append_inst(entry, inst(CfgInstData::Const(0), Type::UNIT));
        cfg.set_return(entry, None);
        cfg
    }

    #[test]
    fn by_ref_local_root_redirects_reads_and_write_backs() {
        // `fn caller() -> i64 { let mut v = 1; bump(inout v); v }`.
        let mut program = scalar_program();
        program.add("bump", |_| by_ref_bump_callee());
        program.add("caller", |interner| {
            let mut cfg = Cfg::new(Type::I64, 1, 0, "caller".to_string(), Vec::<bool>::new());
            let entry = cfg.new_block();
            cfg.entry = entry;
            cfg.append_inst(
                entry,
                inst(
                    CfgInstData::StorageLive {
                        slot: 0,
                        local_ty: Type::I64,
                    },
                    Type::UNIT,
                ),
            );
            let one = cfg.append_inst(entry, inst(CfgInstData::Const(1), Type::I64));
            cfg.append_inst(
                entry,
                inst(CfgInstData::Alloc { slot: 0, init: one }, Type::UNIT),
            );
            let argument = cfg.append_inst(entry, inst(CfgInstData::Load { slot: 0 }, Type::I64));
            cfg.append_call(
                entry,
                None,
                interner.get_or_intern("bump"),
                [CfgCallArg {
                    value: argument,
                    mode: CfgArgMode::Inout,
                }],
                Type::UNIT,
                Span::new(0, 0),
            )
            .unwrap();
            let result = cfg.append_inst(entry, inst(CfgInstData::Load { slot: 0 }, Type::I64));
            cfg.append_inst(
                entry,
                inst(
                    CfgInstData::StorageDead {
                        slot: 0,
                        local_ty: Type::I64,
                    },
                    Type::UNIT,
                ),
            );
            cfg.set_return(entry, Some(result));
            cfg
        });
        let caller = program.cfg("caller");
        let callee = program.cfg("bump");
        assert!(callee.is_param_by_ref(0), "inout uses the by-ref ABI");
        let caller_live = count_matching(caller, |data| {
            matches!(data, CfgInstData::StorageLive { .. })
        });
        let callee_live = count_matching(callee, |data| {
            matches!(data, CfgInstData::StorageLive { .. })
        });
        let inlined = program.inline("caller", "bump");

        // No materialization for a by-ref parameter: no storage was added
        // beyond what the two bodies already contained.
        assert_eq!(
            count_matching(&inlined, |data| matches!(
                data,
                CfgInstData::StorageLive { .. }
            )),
            caller_live + callee_live
        );
        // The callee's write-back became a plain store to the caller's local.
        assert_eq!(
            count_matching(&inlined, |data| matches!(
                data,
                CfgInstData::ParamStore { .. }
            )),
            0
        );
        assert!(
            count_matching(&inlined, |data| matches!(data, CfgInstData::Store { .. })) > 0,
            "the inout write-back must store to the caller local"
        );
        // Caller + callee blocks plus the continuation join.
        assert_eq!(
            inlined.block_count(),
            caller.block_count() + callee.block_count() + 1
        );
        assert_all_blocks_terminated(&inlined);
    }

    #[test]
    fn by_ref_param_root_redirects_to_the_caller_parameter() {
        // `fn wrapper(inout y: i64) -> i64 { bump(inout y); y }`: the callee's
        // by-ref argument roots at the wrapper's own by-ref parameter.
        let mut program = scalar_program();
        program.add("bump", |_| by_ref_bump_callee());
        program.add("wrapper", |interner| {
            let mut cfg = Cfg::new(
                Type::I64,
                0,
                1,
                "wrapper".to_string(),
                ParamSlotModes::new(vec![true], vec![true]),
            );
            let entry = cfg.new_block();
            cfg.entry = entry;
            let argument = cfg.append_inst(entry, inst(CfgInstData::Param { index: 0 }, Type::I64));
            cfg.append_call(
                entry,
                None,
                interner.get_or_intern("bump"),
                [CfgCallArg {
                    value: argument,
                    mode: CfgArgMode::Inout,
                }],
                Type::UNIT,
                Span::new(0, 0),
            )
            .unwrap();
            let result = cfg.append_inst(entry, inst(CfgInstData::Param { index: 0 }, Type::I64));
            cfg.set_return(entry, Some(result));
            cfg
        });
        let inlined = program.inline("wrapper", "bump");
        // The callee's write-back is redirected onto the caller's own by-ref
        // parameter slot.
        assert!(
            attached_values(&inlined).any(|value| matches!(
                inlined.get_inst(value).data,
                CfgInstData::ParamStore { param_slot: 0, .. }
            )),
            "the write-back must target the caller's parameter"
        );
        assert_all_blocks_terminated(&inlined);
    }

    #[test]
    fn projected_by_ref_argument_composes_onto_caller_place() {
        // `fn caller() -> i64 { let mut p = Pair { a: 1, b: 2 };
        //  bump(inout p.a); p.a }`: the inout argument is a projected place.
        let (mut program, pair_id, pair_ty) = pair_program();
        program.add("bump", |_| by_ref_bump_callee());
        program.add("caller", |interner| {
            let mut cfg = Cfg::new(Type::I64, 2, 0, "caller".to_string(), Vec::<bool>::new());
            let entry = cfg.new_block();
            cfg.entry = entry;
            cfg.append_inst(
                entry,
                inst(
                    CfgInstData::StorageLive {
                        slot: 0,
                        local_ty: pair_ty,
                    },
                    Type::UNIT,
                ),
            );
            let one = cfg.append_inst(entry, inst(CfgInstData::Const(1), Type::I64));
            let two = cfg.append_inst(entry, inst(CfgInstData::Const(2), Type::I64));
            let pair = cfg
                .append_struct_init(entry, pair_id, [one, two], pair_ty, Span::new(0, 0))
                .unwrap();
            cfg.append_inst(
                entry,
                inst(
                    CfgInstData::Alloc {
                        slot: 0,
                        init: pair,
                    },
                    Type::UNIT,
                ),
            );
            let argument = cfg
                .append_place_read(
                    entry,
                    PlaceBase::Local(0),
                    pair_ty,
                    [Projection::Field {
                        struct_id: pair_id,
                        field_index: 0,
                    }],
                    Type::I64,
                    Span::new(0, 0),
                )
                .unwrap();
            cfg.append_call(
                entry,
                None,
                interner.get_or_intern("bump"),
                [CfgCallArg {
                    value: argument,
                    mode: CfgArgMode::Inout,
                }],
                Type::UNIT,
                Span::new(0, 0),
            )
            .unwrap();
            let result = cfg
                .append_place_read(
                    entry,
                    PlaceBase::Local(0),
                    pair_ty,
                    [Projection::Field {
                        struct_id: pair_id,
                        field_index: 0,
                    }],
                    Type::I64,
                    Span::new(0, 0),
                )
                .unwrap();
            cfg.append_inst(
                entry,
                inst(
                    CfgInstData::StorageDead {
                        slot: 0,
                        local_ty: pair_ty,
                    },
                    Type::UNIT,
                ),
            );
            cfg.set_return(entry, Some(result));
            cfg
        });
        let inlined = program.inline("caller", "bump");
        assert!(
            attached_values(&inlined).any(|value| match &inlined.get_inst(value).data {
                CfgInstData::PlaceWrite { place, .. } =>
                    !inlined.get_place_projections(place).is_empty(),
                _ => false,
            }),
            "the callee write must retain the caller field projection"
        );
        assert_all_blocks_terminated(&inlined);
    }

    #[test]
    fn slice_borrow_is_lowered_by_the_physical_abi_not_the_logical_mode() {
        // A `borrow s: [i64]` slice is a fat pointer passed BY VALUE through
        // the aggregate ABI even though the logical mode is Borrow: the
        // splice must consult `is_param_by_ref` and materialize, not
        // redirect (ADR-0049 §2). The fixture models the fat pointer as a
        // two-slot `(ptr, len)` aggregate whose parameter is physically
        // by-value while the call site passes it with Borrow mode.
        let interner = ThreadedRodeo::new();
        let type_pool = TypeInternPool::new();
        let ptr_ty = Type::new_ptr_const(type_pool.intern_ptr_const_from_type(Type::I64));
        let (slice_id, _) = type_pool.register_struct(
            interner.get_or_intern("FatPointer"),
            StructDef {
                name: "FatPointer".into(),
                fields: vec![
                    StructField {
                        name: "ptr".into(),
                        ty: ptr_ty,
                    },
                    StructField {
                        name: "len".into(),
                        ty: Type::U64,
                    },
                ],
                is_copy: false,
                is_linear: false,
                declared_linear: false,
                destructor: None,
                is_builtin: false,
                is_pub: false,
                file_id: FileId::DEFAULT,
            },
        );
        let slice_ty = Type::new_struct(slice_id);
        let array_ty = Type::new_array(type_pool.intern_array_from_type(Type::I64, 3));
        let mut program = Program::new(type_pool.freeze(), interner);
        program.add("first", |interner| {
            // `fn first(borrow s: [i64]) -> i64 { s[0] }`: the fat pointer's
            // data pointer is projected out of the by-value parameter.
            let mut cfg = Cfg::new(Type::I64, 0, 2, "first".to_string(), vec![false, false]);
            // The fat pointer is one source parameter across two ABI slots.
            cfg.set_source_param_abi(vec![SourceParamAbi {
                start_slot: 0,
                slot_count: 2,
                crossing_regs: 2,
                ty: None,
            }]);
            let entry = cfg.new_block();
            cfg.entry = entry;
            cfg.append_inst(entry, inst(CfgInstData::Param { index: 0 }, slice_ty));
            let data = cfg
                .append_place_read(
                    entry,
                    PlaceBase::Param(0),
                    slice_ty,
                    [Projection::Field {
                        struct_id: slice_id,
                        field_index: 0,
                    }],
                    ptr_ty,
                    Span::new(0, 0),
                )
                .unwrap();
            let element = cfg
                .append_intrinsic(
                    entry,
                    None,
                    interner.get_or_intern("ptr_read"),
                    [data],
                    Type::I64,
                    Span::new(0, 0),
                )
                .unwrap();
            cfg.set_return(entry, Some(element));
            cfg
        });
        program.add("caller", |interner| {
            // `fn caller() -> i64 { let arr: [i64; 3] = [1, 2, 3];
            //  first(borrow arr) }`.
            let mut cfg = Cfg::new(Type::I64, 3, 0, "caller".to_string(), Vec::<bool>::new());
            let entry = cfg.new_block();
            cfg.entry = entry;
            cfg.append_inst(
                entry,
                inst(
                    CfgInstData::StorageLive {
                        slot: 0,
                        local_ty: array_ty,
                    },
                    Type::UNIT,
                ),
            );
            let one = cfg.append_inst(entry, inst(CfgInstData::Const(1), Type::I64));
            let two = cfg.append_inst(entry, inst(CfgInstData::Const(2), Type::I64));
            let three = cfg.append_inst(entry, inst(CfgInstData::Const(3), Type::I64));
            let array = cfg
                .append_array_init(entry, [one, two, three], array_ty, Span::new(0, 0))
                .unwrap();
            cfg.append_inst(
                entry,
                inst(
                    CfgInstData::Alloc {
                        slot: 0,
                        init: array,
                    },
                    Type::UNIT,
                ),
            );
            let zero = cfg.append_inst(entry, inst(CfgInstData::Const(0), Type::U64));
            let base = cfg
                .append_place_read(
                    entry,
                    PlaceBase::Local(0),
                    array_ty,
                    [Projection::Index {
                        array_type: array_ty,
                        index: zero,
                    }],
                    Type::I64,
                    Span::new(0, 0),
                )
                .unwrap();
            let data = cfg
                .append_intrinsic(
                    entry,
                    None,
                    interner.get_or_intern("raw"),
                    [base],
                    ptr_ty,
                    Span::new(0, 0),
                )
                .unwrap();
            let len = cfg.append_inst(entry, inst(CfgInstData::Const(3), Type::U64));
            let fat_pointer = cfg
                .append_struct_init(entry, slice_id, [data, len], slice_ty, Span::new(0, 0))
                .unwrap();
            let call = cfg
                .append_call(
                    entry,
                    None,
                    interner.get_or_intern("first"),
                    [CfgCallArg {
                        value: fat_pointer,
                        mode: CfgArgMode::Borrow,
                    }],
                    Type::I64,
                    Span::new(0, 0),
                )
                .unwrap();
            cfg.append_inst(
                entry,
                inst(
                    CfgInstData::StorageDead {
                        slot: 0,
                        local_ty: array_ty,
                    },
                    Type::UNIT,
                ),
            );
            cfg.set_return(entry, Some(call));
            cfg
        });
        let caller = program.cfg("caller");
        let callee = program.cfg("first");
        assert!(
            !callee.is_param_by_ref(0),
            "the slice borrow parameter is physically by-value"
        );
        let materialized_base = caller.num_locals();
        let inlined = program.inline("caller", "first");

        // The fat pointer occupies two slots; both are reserved and
        // bracketed by callee-lifetime storage.
        assert!(
            attached_values(&inlined).any(|value| matches!(
                inlined.get_inst(value).data,
                CfgInstData::StorageLive { slot, .. } if slot == materialized_base
            )),
            "the slice argument must be materialized into a fresh slot"
        );
        assert_eq!(
            count_matching(&inlined, |data| matches!(data, CfgInstData::Param { .. })),
            0,
            "every slice parameter access is rewritten to the materialized slot"
        );
        assert_all_blocks_terminated(&inlined);
    }

    // ------------------------------------------------------------------
    // Return wiring and divergence.
    // ------------------------------------------------------------------

    #[test]
    fn early_returns_join_at_the_continuation_block_param() {
        // `fn pick(c: bool) -> i64 { if c { return 1; } 2 }` inlined into
        // `fn caller() -> i64 { pick(true) }`.
        let mut program = scalar_program();
        program.add("pick", |_| {
            let mut cfg = Cfg::new(Type::I64, 0, 1, "pick".to_string(), vec![false]);
            let entry = cfg.new_block();
            let then_block = cfg.new_block();
            let else_block = cfg.new_block();
            let tail = cfg.new_block();
            cfg.entry = entry;
            let cond = cfg.append_inst(entry, inst(CfgInstData::Param { index: 0 }, Type::BOOL));
            cfg.set_branch(entry, cond, then_block, [], else_block, []);
            let one = cfg.append_inst(then_block, inst(CfgInstData::Const(1), Type::I64));
            cfg.set_return(then_block, Some(one));
            cfg.append_inst(else_block, inst(CfgInstData::Const(0), Type::UNIT));
            cfg.set_goto(else_block, tail, []);
            let two = cfg.append_inst(tail, inst(CfgInstData::Const(2), Type::I64));
            cfg.set_return(tail, Some(two));
            cfg
        });
        program.add("caller", |interner| {
            let mut cfg = Cfg::new(Type::I64, 0, 0, "caller".to_string(), Vec::<bool>::new());
            let entry = cfg.new_block();
            cfg.entry = entry;
            let argument = cfg.append_inst(entry, inst(CfgInstData::BoolConst(true), Type::BOOL));
            let call = cfg
                .append_call(
                    entry,
                    None,
                    interner.get_or_intern("pick"),
                    [CfgCallArg {
                        value: argument,
                        mode: CfgArgMode::Normal,
                    }],
                    Type::I64,
                    Span::new(0, 0),
                )
                .unwrap();
            cfg.set_return(entry, Some(call));
            cfg
        });
        let caller = program.cfg("caller");
        let callee = program.cfg("pick");
        assert_eq!(count_returns(callee), 2, "early return plus fall-through");
        let inlined = program.inline("caller", "pick");

        // Both callee returns were rewired into continuation edges; only the
        // caller's own return remains.
        assert_eq!(count_returns(&inlined), count_returns(caller));
        // The continuation join carries the i64 result as a block parameter.
        assert!(
            inlined
                .blocks()
                .iter()
                .any(|block| { block.params.len() == 1 && block.params[0].1 == Type::I64 }),
            "the continuation must take the result as a block parameter"
        );
        assert_all_blocks_terminated(&inlined);
    }

    #[test]
    fn never_returning_callee_leaves_the_continuation_unreachable() {
        // The callee's body diverges (RUE-347 shape): no Return terminator
        // exists, so the continuation gets no incoming edge and the caller's
        // use of the result stays confined to the unreachable join.
        let mut program = scalar_program();
        program.add("forever", |_| {
            // `fn forever() -> i64 { loop { } }`: a self-looping body block
            // and an unreachable exit stub, with no Return anywhere.
            let mut cfg = Cfg::new(Type::I64, 0, 0, "forever".to_string(), Vec::<bool>::new());
            let entry = cfg.new_block();
            let body = cfg.new_block();
            let exit = cfg.new_block();
            cfg.entry = entry;
            cfg.set_goto(entry, body, []);
            cfg.append_inst(body, inst(CfgInstData::Const(0), Type::UNIT));
            cfg.set_goto(body, body, []);
            cfg.set_unreachable(exit);
            cfg
        });
        program.add("caller", |interner| {
            let mut cfg = Cfg::new(Type::I64, 0, 0, "caller".to_string(), Vec::<bool>::new());
            let entry = cfg.new_block();
            cfg.entry = entry;
            let call = cfg
                .append_call(
                    entry,
                    None,
                    interner.get_or_intern("forever"),
                    [],
                    Type::I64,
                    Span::new(0, 0),
                )
                .unwrap();
            cfg.set_return(entry, Some(call));
            cfg
        });
        let callee = program.cfg("forever");
        assert_eq!(count_returns(callee), 0, "the callee never returns");
        let inlined = program.inline("caller", "forever");
        assert_eq!(count_returns(&inlined), 1, "only main's own return remains");
        assert_all_blocks_terminated(&inlined);
    }

    #[test]
    fn instructions_after_the_call_move_to_the_continuation() {
        // `fn callee(x: i64) -> i64 { x }` inlined into
        // `fn caller() -> i64 { let a = callee(1); a + 7 }`.
        let mut program = scalar_program();
        program.add("callee", |_| {
            let mut cfg = Cfg::new(Type::I64, 0, 1, "callee".to_string(), vec![false]);
            let entry = cfg.new_block();
            cfg.entry = entry;
            let param = cfg.append_inst(entry, inst(CfgInstData::Param { index: 0 }, Type::I64));
            cfg.set_return(entry, Some(param));
            cfg
        });
        program.add("caller", |interner| {
            let mut cfg = Cfg::new(Type::I64, 1, 0, "caller".to_string(), Vec::<bool>::new());
            let entry = cfg.new_block();
            cfg.entry = entry;
            cfg.append_inst(
                entry,
                inst(
                    CfgInstData::StorageLive {
                        slot: 0,
                        local_ty: Type::I64,
                    },
                    Type::UNIT,
                ),
            );
            let one = cfg.append_inst(entry, inst(CfgInstData::Const(1), Type::I64));
            let call = cfg
                .append_call(
                    entry,
                    None,
                    interner.get_or_intern("callee"),
                    [CfgCallArg {
                        value: one,
                        mode: CfgArgMode::Normal,
                    }],
                    Type::I64,
                    Span::new(0, 0),
                )
                .unwrap();
            cfg.append_inst(
                entry,
                inst(
                    CfgInstData::Alloc {
                        slot: 0,
                        init: call,
                    },
                    Type::UNIT,
                ),
            );
            let loaded = cfg.append_inst(entry, inst(CfgInstData::Load { slot: 0 }, Type::I64));
            let seven = cfg.append_inst(entry, inst(CfgInstData::Const(7), Type::I64));
            let sum = cfg.append_inst(entry, inst(CfgInstData::Add(loaded, seven), Type::I64));
            cfg.append_inst(
                entry,
                inst(
                    CfgInstData::StorageDead {
                        slot: 0,
                        local_ty: Type::I64,
                    },
                    Type::UNIT,
                ),
            );
            cfg.set_return(entry, Some(sum));
            cfg
        });
        let inlined = program.inline("caller", "callee");
        let entry = inlined.get_block(inlined.entry);
        assert!(
            matches!(entry.terminator, Terminator::Goto { .. }),
            "the split call block jumps into the spliced body"
        );
        let callee_entry = match &entry.terminator {
            Terminator::Goto { target, .. } => *target,
            _ => unreachable!(),
        };
        let continuation = inlined
            .blocks()
            .iter()
            .find(|block| {
                block
                    .insts
                    .iter()
                    .any(|&value| matches!(inlined.get_inst(value).data, CfgInstData::Add(..)))
            })
            .expect("the continuation contains the moved post-call computation")
            .id;
        assert!(
            callee_entry.as_u32() < continuation.as_u32(),
            "spliced callee definitions must precede the continuation so projected-place index dependencies dominate by-ref consumers"
        );
        assert!(
            entry
                .insts
                .iter()
                .all(|&value| !matches!(inlined.get_inst(value).data, CfgInstData::Add(..))),
            "the post-call Add must move to the continuation"
        );
        assert!(
            attached_values(&inlined)
                .any(|value| matches!(inlined.get_inst(value).data, CfgInstData::Add(..))),
            "the moved Add is still attached"
        );
        assert_all_blocks_terminated(&inlined);
    }

    #[test]
    fn unit_returning_callee_splices_without_a_join_param() {
        // `fn note() { }` inlined into `fn caller() -> i64 { note(); 0 }`.
        let mut program = scalar_program();
        program.add("note", |_| {
            let mut cfg = Cfg::new(Type::UNIT, 0, 0, "note".to_string(), Vec::<bool>::new());
            let entry = cfg.new_block();
            cfg.entry = entry;
            cfg.append_inst(entry, inst(CfgInstData::Const(0), Type::UNIT));
            cfg.set_return(entry, None);
            cfg
        });
        program.add("caller", |interner| {
            let mut cfg = Cfg::new(Type::I64, 0, 0, "caller".to_string(), Vec::<bool>::new());
            let entry = cfg.new_block();
            cfg.entry = entry;
            cfg.append_call(
                entry,
                None,
                interner.get_or_intern("note"),
                [],
                Type::UNIT,
                Span::new(0, 0),
            )
            .unwrap();
            let zero = cfg.append_inst(entry, inst(CfgInstData::Const(0), Type::I64));
            cfg.set_return(entry, Some(zero));
            cfg
        });
        let caller = program.cfg("caller");
        let callee = program.cfg("note");
        let inlined = program.inline("caller", "note");
        assert_eq!(
            inlined.block_count(),
            caller.block_count() + callee.block_count() + 1
        );
        assert_all_blocks_terminated(&inlined);
    }

    // ------------------------------------------------------------------
    // Index rebasing under non-trivial caller/callee shapes.
    // ------------------------------------------------------------------

    #[test]
    fn rebasing_survives_branchy_caller_and_callee_shapes() {
        // `fn callee(c: bool, x: i64) -> i64 { let y = x * 2;
        //  if c { y + 1 } else { let z = y - 1; z } }` inlined into
        // `fn caller() -> i64 { let a = 3;
        //  let r = if a > 2 { callee(true, a) } else { 0 }; r + a }` — both
        // sides branch and join through block parameters.
        let mut program = scalar_program();
        program.add("callee", |_| {
            let mut cfg = Cfg::new(Type::I64, 2, 2, "callee".to_string(), vec![false, false]);
            let entry = cfg.new_block();
            let then_block = cfg.new_block();
            let else_block = cfg.new_block();
            let join = cfg.new_block();
            cfg.entry = entry;
            cfg.append_inst(
                entry,
                inst(
                    CfgInstData::StorageLive {
                        slot: 0,
                        local_ty: Type::I64,
                    },
                    Type::UNIT,
                ),
            );
            let x = cfg.append_inst(entry, inst(CfgInstData::Param { index: 1 }, Type::I64));
            let two = cfg.append_inst(entry, inst(CfgInstData::Const(2), Type::I64));
            let doubled = cfg.append_inst(entry, inst(CfgInstData::Mul(x, two), Type::I64));
            cfg.append_inst(
                entry,
                inst(
                    CfgInstData::Alloc {
                        slot: 0,
                        init: doubled,
                    },
                    Type::UNIT,
                ),
            );
            let cond = cfg.append_inst(entry, inst(CfgInstData::Param { index: 0 }, Type::BOOL));
            cfg.set_branch(entry, cond, then_block, [], else_block, []);

            let y = cfg.append_inst(then_block, inst(CfgInstData::Load { slot: 0 }, Type::I64));
            let one = cfg.append_inst(then_block, inst(CfgInstData::Const(1), Type::I64));
            let bumped = cfg.append_inst(then_block, inst(CfgInstData::Add(y, one), Type::I64));
            cfg.set_goto(then_block, join, [bumped]);

            cfg.append_inst(
                else_block,
                inst(
                    CfgInstData::StorageLive {
                        slot: 1,
                        local_ty: Type::I64,
                    },
                    Type::UNIT,
                ),
            );
            let y = cfg.append_inst(else_block, inst(CfgInstData::Load { slot: 0 }, Type::I64));
            let one = cfg.append_inst(else_block, inst(CfgInstData::Const(1), Type::I64));
            let dropped = cfg.append_inst(else_block, inst(CfgInstData::Sub(y, one), Type::I64));
            cfg.append_inst(
                else_block,
                inst(
                    CfgInstData::Alloc {
                        slot: 1,
                        init: dropped,
                    },
                    Type::UNIT,
                ),
            );
            let z = cfg.append_inst(else_block, inst(CfgInstData::Load { slot: 1 }, Type::I64));
            cfg.append_inst(
                else_block,
                inst(
                    CfgInstData::StorageDead {
                        slot: 1,
                        local_ty: Type::I64,
                    },
                    Type::UNIT,
                ),
            );
            cfg.set_goto(else_block, join, [z]);

            let result = cfg.add_block_param(join, Type::I64);
            cfg.append_inst(
                join,
                inst(
                    CfgInstData::StorageDead {
                        slot: 0,
                        local_ty: Type::I64,
                    },
                    Type::UNIT,
                ),
            );
            cfg.set_return(join, Some(result));
            cfg
        });
        program.add("caller", |interner| {
            let mut cfg = Cfg::new(Type::I64, 2, 0, "caller".to_string(), Vec::<bool>::new());
            let entry = cfg.new_block();
            let then_block = cfg.new_block();
            let else_block = cfg.new_block();
            let join = cfg.new_block();
            cfg.entry = entry;
            cfg.append_inst(
                entry,
                inst(
                    CfgInstData::StorageLive {
                        slot: 0,
                        local_ty: Type::I64,
                    },
                    Type::UNIT,
                ),
            );
            let three = cfg.append_inst(entry, inst(CfgInstData::Const(3), Type::I64));
            cfg.append_inst(
                entry,
                inst(
                    CfgInstData::Alloc {
                        slot: 0,
                        init: three,
                    },
                    Type::UNIT,
                ),
            );
            cfg.append_inst(
                entry,
                inst(
                    CfgInstData::StorageLive {
                        slot: 1,
                        local_ty: Type::I64,
                    },
                    Type::UNIT,
                ),
            );
            let a = cfg.append_inst(entry, inst(CfgInstData::Load { slot: 0 }, Type::I64));
            let two = cfg.append_inst(entry, inst(CfgInstData::Const(2), Type::I64));
            let cond = cfg.append_inst(entry, inst(CfgInstData::Gt(a, two), Type::BOOL));
            cfg.set_branch(entry, cond, then_block, [], else_block, []);

            let flag = cfg.append_inst(then_block, inst(CfgInstData::BoolConst(true), Type::BOOL));
            let a = cfg.append_inst(then_block, inst(CfgInstData::Load { slot: 0 }, Type::I64));
            let call = cfg
                .append_call(
                    then_block,
                    None,
                    interner.get_or_intern("callee"),
                    [
                        CfgCallArg {
                            value: flag,
                            mode: CfgArgMode::Normal,
                        },
                        CfgCallArg {
                            value: a,
                            mode: CfgArgMode::Normal,
                        },
                    ],
                    Type::I64,
                    Span::new(0, 0),
                )
                .unwrap();
            cfg.set_goto(then_block, join, [call]);

            let zero = cfg.append_inst(else_block, inst(CfgInstData::Const(0), Type::I64));
            cfg.set_goto(else_block, join, [zero]);

            let r = cfg.add_block_param(join, Type::I64);
            cfg.append_inst(
                join,
                inst(CfgInstData::Alloc { slot: 1, init: r }, Type::UNIT),
            );
            let r_loaded = cfg.append_inst(join, inst(CfgInstData::Load { slot: 1 }, Type::I64));
            let a_loaded = cfg.append_inst(join, inst(CfgInstData::Load { slot: 0 }, Type::I64));
            let sum = cfg.append_inst(join, inst(CfgInstData::Add(r_loaded, a_loaded), Type::I64));
            cfg.append_inst(
                join,
                inst(
                    CfgInstData::StorageDead {
                        slot: 1,
                        local_ty: Type::I64,
                    },
                    Type::UNIT,
                ),
            );
            cfg.append_inst(
                join,
                inst(
                    CfgInstData::StorageDead {
                        slot: 0,
                        local_ty: Type::I64,
                    },
                    Type::UNIT,
                ),
            );
            cfg.set_return(join, Some(sum));
            cfg
        });
        let caller = program.cfg("caller");
        let callee = program.cfg("callee");
        let inlined = program.inline("caller", "callee");

        // Whole-graph verification already ran inside inline_call (types,
        // slots, dominance, edge arity); pin the shape arithmetic too.
        assert_eq!(
            inlined.block_count(),
            caller.block_count() + callee.block_count() + 1
        );
        assert!(inlined.value_count() >= caller.value_count() + callee.value_count());
        assert_eq!(
            inlined.num_locals(),
            caller.num_locals() + callee.num_locals() + 2,
            "two materialized scalar params plus the rebased callee frame"
        );
        assert_eq!(count_calls(&inlined), 0);
        assert_all_blocks_terminated(&inlined);
    }

    #[test]
    fn inlines_exactly_one_of_several_call_sites() {
        // `fn caller() -> i64 { callee(1) + callee(2) }` around the
        // scalar-increment callee.
        let mut program = scalar_program();
        program.add("callee", |_| scalar_increment_callee());
        program.add("caller", |interner| {
            let mut cfg = Cfg::new(Type::I64, 0, 0, "caller".to_string(), Vec::<bool>::new());
            let entry = cfg.new_block();
            cfg.entry = entry;
            let one = cfg.append_inst(entry, inst(CfgInstData::Const(1), Type::I64));
            let first = cfg
                .append_call(
                    entry,
                    None,
                    interner.get_or_intern("callee"),
                    [CfgCallArg {
                        value: one,
                        mode: CfgArgMode::Normal,
                    }],
                    Type::I64,
                    Span::new(0, 0),
                )
                .unwrap();
            let two = cfg.append_inst(entry, inst(CfgInstData::Const(2), Type::I64));
            let second = cfg
                .append_call(
                    entry,
                    None,
                    interner.get_or_intern("callee"),
                    [CfgCallArg {
                        value: two,
                        mode: CfgArgMode::Normal,
                    }],
                    Type::I64,
                    Span::new(0, 0),
                )
                .unwrap();
            let sum = cfg.append_inst(entry, inst(CfgInstData::Add(first, second), Type::I64));
            cfg.set_return(entry, Some(sum));
            cfg
        });
        assert_eq!(count_calls(program.cfg("caller")), 2);
        let inlined = program.inline("caller", "callee");
        assert_eq!(
            count_calls(&inlined),
            1,
            "the second call site is untouched"
        );
        assert_all_blocks_terminated(&inlined);
    }

    #[test]
    fn looping_callee_bodies_splice_intact() {
        // `fn sum_to(n: i64) -> i64 { let mut total = 0; let mut i = 0;
        //  while i < n { total = total + i; i = i + 1; } total }` inlined into
        // `fn caller() -> i64 { sum_to(10) }` — the callee's loop back edge
        // survives the splice.
        let mut program = scalar_program();
        program.add("sum_to", |_| {
            let mut cfg = Cfg::new(Type::I64, 2, 1, "sum_to".to_string(), vec![false]);
            let entry = cfg.new_block();
            let header = cfg.new_block();
            let body = cfg.new_block();
            let exit = cfg.new_block();
            cfg.entry = entry;
            cfg.append_inst(
                entry,
                inst(
                    CfgInstData::StorageLive {
                        slot: 0,
                        local_ty: Type::I64,
                    },
                    Type::UNIT,
                ),
            );
            let zero = cfg.append_inst(entry, inst(CfgInstData::Const(0), Type::I64));
            cfg.append_inst(
                entry,
                inst(
                    CfgInstData::Alloc {
                        slot: 0,
                        init: zero,
                    },
                    Type::UNIT,
                ),
            );
            cfg.append_inst(
                entry,
                inst(
                    CfgInstData::StorageLive {
                        slot: 1,
                        local_ty: Type::I64,
                    },
                    Type::UNIT,
                ),
            );
            let zero = cfg.append_inst(entry, inst(CfgInstData::Const(0), Type::I64));
            cfg.append_inst(
                entry,
                inst(
                    CfgInstData::Alloc {
                        slot: 1,
                        init: zero,
                    },
                    Type::UNIT,
                ),
            );
            cfg.set_goto(entry, header, []);

            let i = cfg.append_inst(header, inst(CfgInstData::Load { slot: 1 }, Type::I64));
            let n = cfg.append_inst(header, inst(CfgInstData::Param { index: 0 }, Type::I64));
            let cond = cfg.append_inst(header, inst(CfgInstData::Lt(i, n), Type::BOOL));
            cfg.set_branch(header, cond, body, [], exit, []);

            let total = cfg.append_inst(body, inst(CfgInstData::Load { slot: 0 }, Type::I64));
            let i = cfg.append_inst(body, inst(CfgInstData::Load { slot: 1 }, Type::I64));
            let summed = cfg.append_inst(body, inst(CfgInstData::Add(total, i), Type::I64));
            cfg.append_inst(
                body,
                inst(
                    CfgInstData::Store {
                        slot: 0,
                        value: summed,
                    },
                    Type::UNIT,
                ),
            );
            let i = cfg.append_inst(body, inst(CfgInstData::Load { slot: 1 }, Type::I64));
            let one = cfg.append_inst(body, inst(CfgInstData::Const(1), Type::I64));
            let bumped = cfg.append_inst(body, inst(CfgInstData::Add(i, one), Type::I64));
            cfg.append_inst(
                body,
                inst(
                    CfgInstData::Store {
                        slot: 1,
                        value: bumped,
                    },
                    Type::UNIT,
                ),
            );
            cfg.append_inst(body, inst(CfgInstData::Const(0), Type::UNIT));
            cfg.set_goto(body, header, []);

            cfg.append_inst(exit, inst(CfgInstData::Const(0), Type::UNIT));
            let total = cfg.append_inst(exit, inst(CfgInstData::Load { slot: 0 }, Type::I64));
            cfg.append_inst(
                exit,
                inst(
                    CfgInstData::StorageDead {
                        slot: 1,
                        local_ty: Type::I64,
                    },
                    Type::UNIT,
                ),
            );
            cfg.append_inst(
                exit,
                inst(
                    CfgInstData::StorageDead {
                        slot: 0,
                        local_ty: Type::I64,
                    },
                    Type::UNIT,
                ),
            );
            cfg.set_return(exit, Some(total));
            cfg
        });
        program.add("caller", |interner| {
            let mut cfg = Cfg::new(Type::I64, 0, 0, "caller".to_string(), Vec::<bool>::new());
            let entry = cfg.new_block();
            cfg.entry = entry;
            let ten = cfg.append_inst(entry, inst(CfgInstData::Const(10), Type::I64));
            let call = cfg
                .append_call(
                    entry,
                    None,
                    interner.get_or_intern("sum_to"),
                    [CfgCallArg {
                        value: ten,
                        mode: CfgArgMode::Normal,
                    }],
                    Type::I64,
                    Span::new(0, 0),
                )
                .unwrap();
            cfg.set_return(entry, Some(call));
            cfg
        });
        let caller = program.cfg("caller");
        let callee = program.cfg("sum_to");
        let inlined = program.inline("caller", "sum_to");
        assert_eq!(
            inlined.block_count(),
            caller.block_count() + callee.block_count() + 1
        );
        assert_all_blocks_terminated(&inlined);
    }

    // ------------------------------------------------------------------
    // Synthetic-CFG cases: escape-fact propagation, the physical-ABI
    // discriminator, and input validation.
    // ------------------------------------------------------------------

    fn synthetic_pool() -> rue_air::FrozenTypeInternPool {
        rue_air::TypeInternPool::new().freeze()
    }

    /// A directly constructed callee `fn callee(x: i64) -> i64 { x }` using
    /// the synthetic one-slot-per-parameter ABI contract.
    fn synthetic_identity_callee(
        pool: &rue_air::FrozenTypeInternPool,
        mark_param_escape: bool,
    ) -> ValidatedCfg {
        let mut cfg = Cfg::new(Type::I64, 0, 1, "callee".to_string(), vec![false]);
        let entry = cfg.new_block();
        cfg.entry = entry;
        if mark_param_escape {
            cfg.mark_param_address_taken(0);
        }
        let read = cfg.append_inst(
            entry,
            CfgInst {
                data: CfgInstData::Param { index: 0 },
                ty: Type::I64,
                span: Span::new(0, 0),
            },
        );
        cfg.set_return(entry, Some(read));
        cfg.finish(pool).unwrap()
    }

    /// A directly constructed caller with one `call callee(<const>)` whose
    /// argument uses `mode`.
    fn synthetic_caller(
        pool: &rue_air::FrozenTypeInternPool,
        interner: &ThreadedRodeo,
        mode: crate::CfgArgMode,
    ) -> (ValidatedCfg, CfgValue) {
        let mut cfg = Cfg::new(Type::I64, 0, 0, "main".to_string(), vec![]);
        let entry = cfg.new_block();
        cfg.entry = entry;
        let argument = cfg.append_inst(
            entry,
            CfgInst {
                data: CfgInstData::Const(5),
                ty: Type::I64,
                span: Span::new(0, 0),
            },
        );
        let call = cfg
            .append_call(
                entry,
                None,
                interner.get_or_intern("callee"),
                [CfgCallArg {
                    value: argument,
                    mode,
                }],
                Type::I64,
                Span::new(0, 0),
            )
            .unwrap();
        cfg.set_return(entry, Some(call));
        (cfg.finish(pool).unwrap(), call)
    }

    #[test]
    fn address_taken_param_escape_carries_to_the_materialized_slot() {
        let pool = synthetic_pool();
        let interner = ThreadedRodeo::new();
        let callee = synthetic_identity_callee(&pool, true);
        let (caller, call) = synthetic_caller(&pool, &interner, crate::CfgArgMode::Normal);
        let inlined = inline_call(&caller, call, &callee, &pool).unwrap();
        // The materialized region starts at the caller's first fresh slot.
        assert!(
            inlined.is_address_taken(0),
            "the callee's parameter escape fact must pin the materialized slot (RUE-521)"
        );
    }

    #[test]
    fn borrow_mode_with_by_value_abi_is_materialized_not_redirected() {
        // The argument mode says Borrow but the callee's physical ABI says
        // by-value: the discriminator is `is_param_by_ref`. A redirect
        // attempt would fail on the Const root; materialization succeeds.
        let pool = synthetic_pool();
        let interner = ThreadedRodeo::new();
        let callee = synthetic_identity_callee(&pool, false);
        let (caller, call) = synthetic_caller(&pool, &interner, crate::CfgArgMode::Borrow);
        let inlined = inline_call(&caller, call, &callee, &pool).unwrap();
        assert!(
            attached_values(&inlined).any(|value| matches!(
                inlined.get_inst(value).data,
                CfgInstData::StorageLive { slot: 0, .. }
            )),
            "the by-value ABI wins over the logical Borrow mode"
        );
    }

    #[test]
    fn non_call_values_and_arity_and_type_mismatches_are_rejected() {
        let pool = synthetic_pool();
        let interner = ThreadedRodeo::new();
        let callee = synthetic_identity_callee(&pool, false);
        let (caller, call) = synthetic_caller(&pool, &interner, crate::CfgArgMode::Normal);

        // Not a call: the argument constant.
        let argument = CfgValue::from_raw(call.as_u32() - 1);
        assert!(matches!(
            inline_call(&caller, argument, &callee, &pool),
            Err(CfgInlineError::NotACall { .. })
        ));

        // Out-of-range value.
        assert!(matches!(
            inline_call(&caller, CfgValue::from_raw(1000), &callee, &pool),
            Err(CfgInlineError::CallSiteNotFound { .. })
        ));

        // Arity mismatch: a callee with no parameters.
        let mut no_params = Cfg::new(Type::I64, 0, 0, "callee".to_string(), Vec::<bool>::new());
        let entry = no_params.new_block();
        no_params.entry = entry;
        let value = no_params.append_inst(
            entry,
            CfgInst {
                data: CfgInstData::Const(1),
                ty: Type::I64,
                span: Span::new(0, 0),
            },
        );
        no_params.set_return(entry, Some(value));
        let no_params = no_params.finish(&pool).unwrap();
        assert!(matches!(
            inline_call(&caller, call, &no_params, &pool),
            Err(CfgInlineError::ArityMismatch {
                call_args: 1,
                callee_params: 0
            })
        ));

        // Return-type mismatch: a bool-returning callee under an i64 call.
        let mut wrong_ty = Cfg::new(Type::BOOL, 0, 1, "callee".to_string(), vec![false]);
        let entry = wrong_ty.new_block();
        wrong_ty.entry = entry;
        let value = wrong_ty.append_inst(
            entry,
            CfgInst {
                data: CfgInstData::BoolConst(true),
                ty: Type::BOOL,
                span: Span::new(0, 0),
            },
        );
        wrong_ty.set_return(entry, Some(value));
        let wrong_ty = wrong_ty.finish(&pool).unwrap();
        assert!(matches!(
            inline_call(&caller, call, &wrong_ty, &pool),
            Err(CfgInlineError::ReturnTypeMismatch { .. })
        ));
    }

    #[test]
    fn runtime_helper_call_sites_are_rejected() {
        let pool = synthetic_pool();
        let interner = ThreadedRodeo::new();
        let callee = synthetic_identity_callee(&pool, false);
        let mut cfg = Cfg::new(Type::I64, 0, 0, "main".to_string(), Vec::<bool>::new());
        let entry = cfg.new_block();
        cfg.entry = entry;
        let argument = cfg.append_inst(
            entry,
            CfgInst {
                data: CfgInstData::Const(5),
                ty: Type::I64,
                span: Span::new(0, 0),
            },
        );
        let call = cfg
            .append_call(
                entry,
                Some(rue_air::RuntimeCallKind::Panic),
                interner.get_or_intern("callee"),
                [CfgCallArg {
                    value: argument,
                    mode: crate::CfgArgMode::Normal,
                }],
                Type::I64,
                Span::new(0, 0),
            )
            .unwrap();
        cfg.set_return(entry, Some(call));
        let caller = cfg.finish(&pool).unwrap();
        assert!(matches!(
            inline_call(&caller, call, &callee, &pool),
            Err(CfgInlineError::RuntimeCall { .. })
        ));
    }
}
