//! AIR to CFG lowering.
//!
//! This module converts the structured control flow in AIR (Branch, Loop)
//! into explicit basic blocks with terminators.

use lasso::{Spur, ThreadedRodeo};
use rue_air::{
    AirArgMode, AirInstData, AirPattern, AirPlaceBase, AirPlaceRef, AirProjection, AirRef,
    AnalyzedCallableKind, ArgConvention, FrozenTypeInternPool, NativeCallAbi, ParamSlotModes,
    SourceParamAbi, StructId, Type, TypeKind, ValidatedAir,
};
use rue_error::{CompileError, CompileWarning, ErrorKind, WarningKind};

use crate::CfgOutput;
use crate::inst::{
    BlockId, Cfg, CfgArgMode, CfgCallArg, CfgEditError, CfgInst, CfgInstData, CfgValue, Place,
    PlaceBase, Projection, Terminator,
};
use crate::payload::{
    CfgArrayElements, CfgCallArgs, CfgElseArgs, CfgEnumPayload, CfgGotoArgs, CfgIntrinsicArgs,
    CfgProjections, CfgStructFields, CfgSwitchCases, CfgThenArgs,
};

/// Result of lowering an expression.
struct ExprResult {
    /// The value produced (if any - statements like Store don't produce values)
    value: Option<CfgValue>,
    /// Whether control flow continues after this expression
    continuation: Continuation,
}

/// How control flow continues after an expression.
enum Continuation {
    /// Control continues normally (can add more instructions)
    Continues,
    /// Control flow diverged (return, break, continue) - no more instructions
    Diverged,
}

/// Loop context for break/continue handling.
struct LoopContext {
    /// Block to jump to for continue (loop header)
    header: BlockId,
    /// Block to jump to for break (loop exit)
    exit: BlockId,
    /// The scope depth when entering the loop (before the loop body scope).
    /// Used to know how many scopes to drop on break/continue.
    /// For break/continue, we drop scopes from current down to (but not including)
    /// this depth.
    scope_depth: usize,
}

/// Information about a slot that became live in a scope.
/// Used for drop elaboration.
#[derive(Debug, Clone)]
struct LiveSlot {
    /// The slot number
    slot: u32,
    /// The type of value stored in the slot
    ty: Type,
    /// The span where the slot became live (for error reporting)
    span: rue_span::Span,
    /// Whether the slot's initializer completed on the current lowering path.
    initialized: bool,
}

/// A storage location whose contents may have been moved out.
/// Used by drop elaboration to suppress drops of moved-out values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum MovedSlot {
    /// A local variable slot
    Local(u32),
    /// A parameter ABI slot
    Param(u32),
}

/// A struct field path inside a slot, as declaration indices from the
/// outermost struct inward: `o.a` → `[0]`, `o.a.b` → `[0, 1]` (for `a` and
/// `b` at declaration index 0 and 1 of their respective structs).
/// Array element paths (RUE-186) reuse the representation with the element
/// index as the segment: `xs[2]` → `[2]`. Whether a segment is a field or an
/// element is determined by the type at that level.
type FieldPath = Vec<u32>;
type MovedPathKey = (MovedSlot, FieldPath);
type MovedPathMap = std::collections::HashMap<MovedSlot, std::collections::HashSet<FieldPath>>;

#[cfg(test)]
#[derive(Debug, Clone, Default)]
struct MoveStateStats {
    slot_path_visits: std::cell::Cell<usize>,
}

/// Move-out state for drop elaboration: whole slots and struct field paths
/// whose contents were moved out.
///
/// `slots` tracks whole-value moves (`let t = s;`). `fields` tracks partial
/// moves of a struct field path of any depth (`eat(o.a)`, `eat(o.a.b)`,
/// RUE-62/RUE-157) — the slot itself stays live, but its scope-exit drop
/// must skip the moved path. `maybe_fields` additionally remembers paths
/// moved on only SOME tracked path: those drops stay in the schedule but
/// run behind a per-path runtime drop flag (RUE-156).
#[derive(Debug, Clone, Default)]
struct MoveState {
    /// Slots whose ENTIRE contents were moved out.
    slots: std::collections::HashSet<MovedSlot>,
    /// `(slot, path)` pairs for field paths moved out (on EVERY tracked
    /// path) of a slot that is itself still live. Join: intersection.
    fields: MovedPathMap,
    /// `(slot, path)` pairs for field paths moved out on SOME tracked path.
    /// Always a superset of `fields`. Join: union. A path here but not in
    /// `fields` is path-dependent: its scope-exit drop is emitted behind
    /// that path's runtime drop flag.
    maybe_fields: MovedPathMap,
    #[cfg(test)]
    stats: MoveStateStats,
}

impl MoveState {
    #[cfg(test)]
    fn record_slot_path_visits(&self, count: usize) {
        self.stats
            .slot_path_visits
            .set(self.stats.slot_path_visits.get() + count);
    }

    /// Record that the slot's whole value moved out.
    fn mark_slot(&mut self, slot: MovedSlot) {
        self.slots.insert(slot);
        // Whole-value move subsumes any per-field moves (and the slot's
        // whole-value drop flag takes over at runtime).
        #[cfg(test)]
        self.record_slot_path_visits(self.fields.get(&slot).map_or(0, |paths| paths.len()));
        self.fields.remove(&slot);
        #[cfg(test)]
        self.record_slot_path_visits(self.maybe_fields.get(&slot).map_or(0, |paths| paths.len()));
        self.maybe_fields.remove(&slot);
    }

    /// Record that one struct field path of the slot moved out.
    fn mark_path(&mut self, slot: MovedSlot, path: FieldPath) {
        self.fields.entry(slot).or_default().insert(path.clone());
        self.maybe_fields.entry(slot).or_default().insert(path);
    }

    /// The slot was (re)initialized with a fresh value: clear all move-out
    /// state for it (whole-slot and per-field), so the new occupant is
    /// dropped at scope exit.
    fn clear_slot(&mut self, slot: MovedSlot) {
        self.slots.remove(&slot);
        #[cfg(test)]
        self.record_slot_path_visits(self.fields.get(&slot).map_or(0, |paths| paths.len()));
        self.fields.remove(&slot);
        #[cfg(test)]
        self.record_slot_path_visits(self.maybe_fields.get(&slot).map_or(0, |paths| paths.len()));
        self.maybe_fields.remove(&slot);
    }

    /// One top-level field of the slot was reassigned: that field (and
    /// everything nested inside it) holds a fresh value again and must be
    /// dropped at scope exit.
    fn clear_field(&mut self, slot: MovedSlot, field: u32) {
        #[cfg(test)]
        self.record_slot_path_visits(
            self.fields.get(&slot).map_or(0, |paths| paths.len())
                + self.maybe_fields.get(&slot).map_or(0, |paths| paths.len()),
        );
        Self::clear_field_from(&mut self.fields, slot, field);
        Self::clear_field_from(&mut self.maybe_fields, slot, field);
    }

    fn clear_field_from(paths_by_slot: &mut MovedPathMap, slot: MovedSlot, field: u32) {
        let remove_partition = if let Some(paths) = paths_by_slot.get_mut(&slot) {
            paths.retain(|path| path.first() != Some(&field));
            paths.is_empty()
        } else {
            false
        };
        if remove_partition {
            paths_by_slot.remove(&slot);
        }
    }

    /// Was the slot's whole value moved out (on every tracked path)?
    fn is_slot_moved(&self, slot: MovedSlot) -> bool {
        self.slots.contains(&slot)
    }

    /// Was this exact field path moved out on every tracked path?
    fn is_path_moved(&self, key: &MovedPathKey) -> bool {
        self.fields
            .get(&key.0)
            .is_some_and(|paths| paths.contains(&key.1))
    }

    /// Was this exact field path moved out on any tracked path?
    fn is_path_maybe_moved(&self, key: &MovedPathKey) -> bool {
        self.maybe_fields
            .get(&key.0)
            .is_some_and(|paths| paths.contains(&key.1))
    }
    /// Field paths moved out of `slot` on EVERY tracked path.
    fn moved_paths_of(&self, slot: MovedSlot) -> std::collections::HashSet<FieldPath> {
        #[cfg(test)]
        self.record_slot_path_visits(self.fields.get(&slot).map_or(0, |paths| paths.len()));
        self.fields.get(&slot).cloned().unwrap_or_default()
    }

    /// Field paths moved out of `slot` on SOME tracked path (superset of
    /// `moved_paths_of`).
    fn maybe_moved_paths_of(&self, slot: MovedSlot) -> std::collections::HashSet<FieldPath> {
        #[cfg(test)]
        self.record_slot_path_visits(self.maybe_fields.get(&slot).map_or(0, |paths| paths.len()));
        self.maybe_fields.get(&slot).cloned().unwrap_or_default()
    }

    /// Join two path states: definite state (`slots`, `fields`) is kept
    /// only if present in BOTH ("moved on ALL paths" — see the `moved`
    /// field docs); possible state (`maybe_fields`) is kept if present in
    /// EITHER ("moved on SOME path").
    fn intersect(&self, other: &MoveState) -> MoveState {
        let fields = self
            .fields
            .iter()
            .filter_map(|(slot, paths)| {
                let other_paths = other.fields.get(slot)?;
                let common = paths
                    .intersection(other_paths)
                    .cloned()
                    .collect::<std::collections::HashSet<_>>();
                (!common.is_empty()).then_some((*slot, common))
            })
            .collect();
        let mut maybe_fields = self.maybe_fields.clone();
        for (slot, paths) in &other.maybe_fields {
            maybe_fields
                .entry(*slot)
                .or_default()
                .extend(paths.iter().cloned());
        }
        MoveState {
            slots: self.slots.intersection(&other.slots).copied().collect(),
            fields,
            maybe_fields,
            #[cfg(test)]
            stats: MoveStateStats::default(),
        }
    }
}

/// Builder that converts AIR to CFG.
pub struct CfgBuilder<'a> {
    air: &'a ValidatedAir,
    cfg: Cfg,
    /// Canonical pool for struct, enum, array, and pointer definitions.
    type_pool: &'a FrozenTypeInternPool,
    /// Interner for resolving symbols to strings
    interner: &'a ThreadedRodeo,
    /// Compiler-owned projection of legacy live callable names to canonical
    /// machine symbols. Cleanup lowering consumes this same authority as AIR.
    source_symbol_resolver: Box<dyn Fn(&str) -> Option<Spur> + 'a>,
    /// Current block we're building
    current_block: BlockId,
    /// Stack of loop contexts for nested loops
    loop_stack: Vec<LoopContext>,
    /// Cache: maps AIR refs to CFG values (for already-lowered instructions)
    value_cache: Vec<Option<CfgValue>>,
    /// Warnings collected during CFG construction (e.g., unreachable code)
    warnings: Vec<CompileWarning>,
    /// Whether this function suppresses CFG unreachable-code warnings.
    allow_unreachable_code: bool,
    /// Internal-compiler-error diagnostics collected during CFG construction
    /// (RUE-7). These signal malformed AIR that upstream passes should have
    /// ruled out (e.g. an un-specialized `CallGeneric`); rather than panicking
    /// deep in lowering, we record a clean ICE here and let the driver abort
    /// with a proper diagnostic (exit 1) instead of a process abort.
    errors: Vec<CompileError>,
    /// Stack of scopes for drop elaboration.
    /// Each scope contains the slots that became live in that scope.
    /// Used to emit StorageDead (and Drop if needed) at scope exit.
    scope_stack: Vec<Vec<LiveSlot>>,
    /// Runtime drop flags (RUE-108): one hidden i32 frame slot per droppable
    /// slot that is moved ANYWHERE in the function. The flag is 1 while the
    /// slot owns its value and 0 after a move; scope-exit drops for flagged
    /// slots are emitted behind an `if flag != 0` guard, which makes
    /// branch-divergent moves (moved in one arm only), conservative loop
    /// joins, and short-circuit edges drop-exactly-once at RUNTIME even
    /// where the static all-paths analysis must stay conservative.
    drop_flags: std::collections::HashMap<MovedSlot, u32>,
    /// Per-field-path runtime drop flags (RUE-156): like `drop_flags`, but
    /// one flag per `(slot, field path)` with a field-level MarkMoved
    /// anywhere in the function. Armed when the slot is (re)initialized,
    /// cleared at the path's move site; `emit_partial_struct_drop` guards
    /// each possibly-moved field's drop with its flag.
    field_drop_flags: std::collections::HashMap<MovedPathKey, u32>,
    /// Slots with a whole-value MarkMoved anywhere in the AIR (pre-scanned),
    /// i.e. candidates for a drop flag.
    ever_moved: std::collections::HashSet<MovedSlot>,
    /// `(slot, field path)` pairs with a field-level MarkMoved anywhere in
    /// the AIR whose moved type needs drop (pre-scanned), i.e. candidates
    /// for a per-field drop flag.
    ever_field_moved: std::collections::HashSet<MovedPathKey>,
    /// Slots (and struct field paths of any depth, RUE-62/RUE-157) whose
    /// contents have definitely been moved out on every path reaching the
    /// current lowering position. Drop elaboration skips these (the new
    /// owner of the value is responsible for dropping it).
    ///
    /// Maintained path-sensitively: each branch of an if/match is lowered
    /// starting from the pre-branch state, and the post-construct state is
    /// the intersection of the branch exit states ("moved on ALL paths").
    /// A value moved on only SOME paths stays in the drop schedule, but its
    /// scope-exit drop is emitted behind a runtime drop-flag guard (see
    /// `drop_flags` for whole slots, `field_drop_flags` plus the
    /// union-joined `MoveState::maybe_fields` for field paths), so the
    /// moving path skips it at runtime — drop exactly once on every path,
    /// and never a leak.
    moved: MoveState,
    implicit_named_destructors: std::collections::HashSet<StructId>,
    /// Aggregate types whose destructor dependencies have already been
    /// discovered for this CFG body. Keeps repeated drops linear in the size
    /// of the reachable type graph rather than rewalking the same subgraphs.
    implicit_destructor_types: std::collections::HashSet<Type>,
    anonymous_destructor_dependency_incomplete: bool,
    callable_kind: AnalyzedCallableKind,
}

/// Derive the grouped per-source-parameter ABI descriptors (ADR-0052 phase 5.8,
/// RUE-1005) from the AIR and the per-slot by-reference vector.
///
/// The parameter type at each slot span comes from the AIR: every by-value
/// (`Normal`) parameter — used or not — is recorded in `param_drops` with its
/// start slot and type, and any additional used parameter (a destructor `self`,
/// whose drops are cleared) is recovered from its `Param` instruction. Each
/// parameter's incoming-register crossing width is decided by the native
/// call-ABI classifier and stored as a plain integer; the descriptor carries no
/// `Type`, so a pointer-only consumer's CFG stays layout-independent and
/// reusable when a pointee struct's layout changes. Walking the slots and
/// advancing by each parameter's decomposition width reconstructs the exact
/// grouping identically for a fresh analysis and a durable/imported body, since
/// both rebuild from the same AIR.
fn derive_source_param_abi(builder: &CfgBuilder<'_>) -> Vec<SourceParamAbi> {
    let air = builder.air;
    let type_pool = builder.type_pool;
    let num_params = builder.cfg.num_params();
    let by_ref: Vec<bool> = builder.cfg.param_modes().to_vec();
    let abi = NativeCallAbi::for_arguments(type_pool);

    // Drop glue and destructors are invoked exclusively through the cleanup
    // call convention (`CallPlan::from_slot_values`), which passes an
    // aggregate's already-materialized slots DIRECTLY and flattened (RUE-998 /
    // RUE-311), never the ordinary indirect compact-aggregate transport
    // (RUE-1005). Their by-value parameters must therefore home one incoming
    // register per slot; letting the compact classifier force a multi-slot
    // element parameter indirect (one pointer) desynchronizes the direct-slot
    // caller from an indirect-unmarshalling callee and miscompiles — an array
    // drop glue dereferenced its element values as addresses and overflowed the
    // stack (RUE-1035 M3). These synthetic symbols are compiler-reserved
    // (`__rue_drop_*` glue, `<Type>.__drop` destructors), so this is the same
    // identity codegen already resolves them by. Gate-off the classifier is
    // already Direct, so this is inert there.
    let direct_slot_abi = builder.callable_kind.uses_direct_slot_abi();

    // Slot -> source type. `param_drops` covers every Normal by-value parameter
    // (including unused ones); `Param` instructions supplement any used
    // parameter whose drop entry was cleared (destructor receivers).
    let mut ty_at: std::collections::HashMap<u32, Type> = std::collections::HashMap::new();
    for &(slot, ty) in air.param_drops() {
        ty_at.entry(slot).or_insert(ty);
    }
    for i in 0..air.len() {
        let inst = air.get(AirRef::from_raw(i as u32));
        if let AirInstData::Param { index } = inst.data {
            ty_at.entry(index).or_insert(inst.ty);
        }
    }

    let mut descriptors = Vec::new();
    let mut slot = 0u32;
    while slot < num_params {
        let is_by_ref = by_ref.get(slot as usize).copied().unwrap_or(false);
        let (slot_count, crossing_regs, ty) = if is_by_ref {
            // A by-reference parameter is always one pointer slot; its type is
            // never consulted by code generation.
            (1, 1, None)
        } else if let Some(&ty) = ty_at.get(&slot) {
            let width = type_pool.abi_slot_count(ty).max(1);
            let crossing = if direct_slot_abi {
                // Cleanup-slot callees receive every slot directly (see above),
                // so the incoming-register width is the full decomposition.
                width
            } else {
                abi.classify_arg(ty, ArgConvention::ByValue)
                    .crossing_slots()
                    .max(1)
            };
            // Carry the type only when the parameter crosses indirectly (one
            // pointer over a multi-slot span), so a direct parameter's CFG stays
            // layout-independent.
            let carried_ty = (crossing < width).then_some(ty);
            (width, crossing, carried_ty)
        } else {
            // No recorded type for a by-value slot: a single direct slot, which
            // homes exactly as the historical prologue.
            (1, 1, None)
        };
        descriptors.push(SourceParamAbi {
            start_slot: slot,
            slot_count,
            crossing_regs,
            ty,
        });
        slot += slot_count;
    }
    descriptors
}

impl<'a> CfgBuilder<'a> {
    fn payload_or<T>(
        &mut self,
        result: Result<T, CfgEditError>,
        fallback: T,
        span: rue_span::Span,
    ) -> T {
        match result {
            Ok(value) => value,
            Err(error) => {
                // A payload range that outgrew the compact `u32` representation
                // is an implementation-limit rejection (E1401), not an ICE
                // (spec C.1:2); only a malformed builder request stays internal.
                self.errors.push(CompileError::new(
                    error.error_kind("CFG payload construction failed"),
                    span,
                ));
                fallback
            }
        }
    }

    fn runtime_air_type(&self, ty: Type) -> Option<rue_air::RuntimeAirType> {
        use rue_air::RuntimeAirType as R;

        if ty == Type::UNIT {
            return Some(R::Unit);
        }
        if ty == Type::I64 {
            return Some(R::I64);
        }
        if ty == Type::U64 {
            return Some(R::U64);
        }
        if ty == Type::U32 {
            return Some(R::U32);
        }
        if ty == Type::BOOL {
            return Some(R::Bool);
        }
        if ty == Type::NEVER {
            return Some(R::Never);
        }
        if ty.is_signed() {
            return Some(R::SignedInteger);
        }
        if ty.is_unsigned() {
            return Some(R::UnsignedInteger);
        }
        if let TypeKind::Struct(struct_id) = ty.kind() {
            let name: &str = &self.type_pool.struct_def(struct_id).name;
            if self.type_pool.is_strbuf(struct_id) || rue_air::is_string_view_struct_name(name) {
                return Some(R::Text);
            }
        }
        if let Some(ptr) = ty.as_ptr_const()
            && self.type_pool.ptr_const_def(ptr) == Type::U8
        {
            return Some(R::ConstBytePointer);
        }
        if let Some(ptr) = ty.as_ptr_mut() {
            return Some(if self.type_pool.ptr_mut_def(ptr) == Type::U8 {
                R::MutBytePointer
            } else {
                R::MutPointer
            });
        }
        None
    }

    fn runtime_air_result_type(&self, ty: Type) -> Option<rue_air::RuntimeAirType> {
        use rue_air::RuntimeAirType as R;

        if ty == Type::U8 {
            return Some(R::U8);
        }
        if let TypeKind::Struct(struct_id) = ty.kind()
            && self.type_pool.is_strbuf(struct_id)
        {
            return Some(R::StrBuf);
        }
        if let TypeKind::Enum(enum_id) = ty.kind() {
            let definition = self.type_pool.enum_def(enum_id);
            let some = definition.find_variant("Some")?;
            definition.find_variant("None")?;
            let [payload] = definition.variant_payload(some) else {
                return None;
            };
            if let TypeKind::Struct(struct_id) = payload.kind()
                && self.type_pool.is_strbuf(struct_id)
            {
                return Some(R::OptionStrBuf);
            }
            return Some(match *payload {
                Type::I32 => R::OptionI32,
                Type::I64 => R::OptionI64,
                Type::U32 => R::OptionU32,
                Type::U64 => R::OptionU64,
                _ => return None,
            });
        }
        self.runtime_air_type(ty)
    }

    fn assert_valid_runtime_call_args(
        &self,
        runtime: rue_air::RuntimeCallKind,
        args: impl IntoIterator<Item = (AirRef, AirArgMode)>,
        result: Type,
    ) {
        let args = args
            .into_iter()
            .map(|(value, mode)| {
                self.runtime_air_type(self.air.get(value).ty)
                    .map(|ty| rue_air::RuntimeAirArgument { ty, mode })
            })
            .collect::<Option<Vec<_>>>();
        let Some(args) = args else {
            panic!("runtime call {runtime:?} has an AIR argument with no runtime type");
        };
        let Some(result) = self.runtime_air_result_type(result) else {
            panic!("runtime call {runtime:?} has an AIR result with no runtime type");
        };
        assert!(
            runtime.validate() && runtime.validate_air_call(&args, result),
            "runtime call {runtime:?} has invalid AIR arguments {args:?} or result {result:?}"
        );
    }

    /// Build a CFG from AIR, returning the CFG and any warnings.
    ///
    /// The `type_pool` provides struct/enum/array definitions needed for queries like
    /// `type_needs_drop`. The `interner` is used to resolve Symbol values to strings for the CFG.
    pub fn build(
        air: &'a ValidatedAir,
        num_locals: u32,
        num_params: u32,
        fn_name: &str,
        type_pool: &'a FrozenTypeInternPool,
        param_modes: impl Into<ParamSlotModes>,
        interner: &'a ThreadedRodeo,
        allow_unreachable_code: bool,
        callable_kind: AnalyzedCallableKind,
    ) -> CfgOutput {
        Self::build_with_symbol_resolver(
            air,
            num_locals,
            num_params,
            fn_name,
            type_pool,
            param_modes,
            interner,
            allow_unreachable_code,
            callable_kind,
            |name| Some(interner.get_or_intern(name)),
        )
    }

    pub fn build_with_symbol_resolver(
        air: &'a ValidatedAir,
        num_locals: u32,
        num_params: u32,
        fn_name: &str,
        type_pool: &'a FrozenTypeInternPool,
        param_modes: impl Into<ParamSlotModes>,
        interner: &'a ThreadedRodeo,
        allow_unreachable_code: bool,
        callable_kind: AnalyzedCallableKind,
        source_symbol_resolver: impl Fn(&str) -> Option<Spur> + 'a,
    ) -> CfgOutput {
        let mut builder = CfgBuilder {
            air,
            cfg: Cfg::new(
                air.return_type(),
                num_locals,
                num_params,
                fn_name.to_string(),
                param_modes,
            ),
            type_pool,
            interner,
            source_symbol_resolver: Box::new(source_symbol_resolver),
            current_block: BlockId(0),
            loop_stack: Vec::new(),
            value_cache: vec![None; air.len()],
            warnings: Vec::new(),
            allow_unreachable_code,
            errors: Vec::new(),
            scope_stack: vec![Vec::new()], // Start with one scope for the function body
            drop_flags: std::collections::HashMap::new(),
            field_drop_flags: std::collections::HashMap::new(),
            ever_moved: std::collections::HashSet::new(),
            ever_field_moved: std::collections::HashSet::new(),
            moved: MoveState::default(),
            implicit_named_destructors: std::collections::HashSet::new(),
            implicit_destructor_types: std::collections::HashSet::new(),
            anonymous_destructor_dependency_incomplete: false,
            callable_kind,
        };

        // Create entry block
        builder.current_block = builder.cfg.new_block();
        builder.cfg.entry = builder.current_block;

        // Pre-scan for moves so drop flags can be initialized at the
        // value's init site, before the move is reached (RUE-108).
        // Whole-value MarkMoveds nominate the slot for a whole-slot flag;
        // path-level MarkMoveds (struct field paths, and constant-index
        // array elements per RUE-186) nominate their (slot, path) for a
        // per-path flag (RUE-156) when the moved type needs dropping.
        for i in 0..air.len() {
            let inst = air.get(AirRef::from_raw(i as u32));
            if let AirInstData::MarkMoved {
                slot,
                is_param,
                place,
                ..
            } = &inst.data
            {
                let key = if *is_param {
                    MovedSlot::Param(*slot)
                } else {
                    MovedSlot::Local(*slot)
                };
                match place {
                    None => {
                        builder.ever_moved.insert(key);
                    }
                    Some(place_ref) => {
                        if builder.type_needs_drop(inst.ty) {
                            let path = builder.moved_field_path(*place_ref);
                            builder.ever_field_moved.insert((key, path));
                        }
                    }
                }
            }
        }

        // Owned by-value params are "initialized" at entry: arm their drop
        // flags here so a flag is live before any move site is reached.
        for &(abi_slot, ty) in &builder.air.param_drops().to_vec() {
            let key = MovedSlot::Param(abi_slot);
            if builder.ever_moved.contains(&key) && builder.type_needs_drop(ty) {
                builder.set_drop_flag(key, true, rue_span::Span::default());
            }
            builder.arm_field_drop_flags(key, rue_span::Span::default());
        }

        // Find the root (should be Ret as last instruction)
        if air.len() > 0 {
            let root = AirRef::from_raw((air.len() - 1) as u32);
            builder.lower_inst(root);
        }

        let implicit_named_destructors = builder
            .type_pool
            .all_struct_ids()
            .filter(|id| builder.implicit_named_destructors.contains(id))
            .collect();

        // Derive the grouped per-source-parameter ABI descriptors (RUE-1005)
        // from the AIR, which both a fresh analysis and a durable/imported body
        // rebuild identically, so the CFG's grouping matches across reuse.
        let source_param_abi = derive_source_param_abi(&builder);
        builder.cfg.set_source_param_abi(source_param_abi);

        // Report the published per-function block/value ceiling before
        // verification: a latched graph holds identities that were handed back
        // instead of allocated, so every structural finding downstream is a
        // consequence of the limit rather than a producer bug. Spec C.1:2 wants
        // the limit named (E1401), not the E9000 the verifier would raise.
        let cfg = if let Some(error) = builder.cfg.latched_capacity_error() {
            builder.errors.push(CompileError::new(
                error.error_kind("CFG construction failed"),
                rue_span::Span::default(),
            ));
            None
        } else {
            match builder.cfg.finish(builder.type_pool) {
                Ok(cfg) => Some(cfg),
                Err(error) => {
                    builder.errors.push(CompileError::new(
                        ErrorKind::InternalError(error.to_string()),
                        rue_span::Span::default(),
                    ));
                    None
                }
            }
        };
        CfgOutput {
            cfg,
            warnings: builder.warnings,
            errors: builder.errors,
            implicit_named_destructors,
            anonymous_destructor_dependency_incomplete: builder
                .anonymous_destructor_dependency_incomplete,
        }
    }

    /// Lower an AIR instruction, returning its result.
    fn lower_inst(&mut self, air_ref: AirRef) -> ExprResult {
        // Check cache first
        if let Some(cached) = self.value_cache[air_ref.as_u32() as usize] {
            return ExprResult {
                value: Some(cached),
                continuation: Continuation::Continues,
            };
        }

        let inst = self.air.get(air_ref);
        let span = inst.span;
        let ty = inst.ty;

        match &inst.data {
            AirInstData::Const(v) => {
                let value = self.emit(CfgInstData::Const(*v), ty, span);
                self.cache(air_ref, value);
                ExprResult {
                    value: Some(value),
                    continuation: Continuation::Continues,
                }
            }

            AirInstData::BoolConst(v) => {
                let value = self.emit(CfgInstData::BoolConst(*v), ty, span);
                self.cache(air_ref, value);
                ExprResult {
                    value: Some(value),
                    continuation: Continuation::Continues,
                }
            }

            AirInstData::StringConst(string_id) => {
                let value = self.emit(CfgInstData::StringConst(*string_id), ty, span);
                self.cache(air_ref, value);
                ExprResult {
                    value: Some(value),
                    continuation: Continuation::Continues,
                }
            }

            AirInstData::UnitConst => {
                // Unit constants have no runtime representation.
                // We emit a dummy const 0 with unit type for uniformity,
                // but codegen will ignore values of unit type.
                let value = self.emit(CfgInstData::Const(0), ty, span);
                self.cache(air_ref, value);
                ExprResult {
                    value: Some(value),
                    continuation: Continuation::Continues,
                }
            }

            AirInstData::TypeConst(_) => {
                // TypeConst instructions are compile-time-only. They can appear in the AIR
                // in several valid scenarios:
                // 1. As arguments to generic functions (substituted during specialization)
                // 2. As the result of comptime type-returning functions (stored in comptime_type_vars)
                //
                // At CFG building time, any TypeConst that remains is simply a no-op -
                // type values don't exist at runtime. We return Unit with no value to indicate
                // this instruction doesn't produce runtime code.
                ExprResult {
                    value: None,
                    continuation: Continuation::Continues,
                }
            }

            AirInstData::CallGeneric { .. } => {
                // CallGeneric instructions must be specialized (rewritten to a
                // regular Call) by the specialization pass (see specialize.rs)
                // before CFG building. Reaching here means specialization left a
                // generic call behind — malformed AIR, i.e. a compiler bug.
                //
                // RUE-7: rather than panicking (a process abort deep in
                // lowering), record a clean internal-compiler-error diagnostic
                // on the error channel. The driver checks `CfgOutput.errors`
                // and aborts with a proper ICE (exit 1, carrying this span)
                // before optimizing or lowering the CFG further. We still emit a
                // placeholder value so lowering can finish without cascading
                // panics; the resulting CFG is discarded once the error surfaces.
                self.errors.push(CompileError::new(
                    ErrorKind::InternalError(
                        "CallGeneric instruction reached CFG building; it must be \
                         specialized to a regular Call before codegen (phase: \
                         cfg_builder). This is a compiler bug."
                            .to_string(),
                    ),
                    span,
                ));
                let value = self.emit(CfgInstData::Const(0), ty, span);
                self.cache(air_ref, value);
                ExprResult {
                    value: Some(value),
                    continuation: Continuation::Continues,
                }
            }

            AirInstData::Param { index } => {
                let value = self.emit(CfgInstData::Param { index: *index }, ty, span);
                self.cache(air_ref, value);
                ExprResult {
                    value: Some(value),
                    continuation: Continuation::Continues,
                }
            }

            AirInstData::Add(lhs, rhs) => {
                let Some(lhs_val) = self.lower_value(*lhs) else {
                    return Self::diverged();
                };
                let Some(rhs_val) = self.lower_value(*rhs) else {
                    return Self::diverged();
                };
                let value = self.emit(CfgInstData::Add(lhs_val, rhs_val), ty, span);
                self.cache(air_ref, value);
                ExprResult {
                    value: Some(value),
                    continuation: Continuation::Continues,
                }
            }

            AirInstData::Sub(lhs, rhs) => {
                let Some(lhs_val) = self.lower_value(*lhs) else {
                    return Self::diverged();
                };
                let Some(rhs_val) = self.lower_value(*rhs) else {
                    return Self::diverged();
                };
                let value = self.emit(CfgInstData::Sub(lhs_val, rhs_val), ty, span);
                self.cache(air_ref, value);
                ExprResult {
                    value: Some(value),
                    continuation: Continuation::Continues,
                }
            }

            AirInstData::Mul(lhs, rhs) => {
                let Some(lhs_val) = self.lower_value(*lhs) else {
                    return Self::diverged();
                };
                let Some(rhs_val) = self.lower_value(*rhs) else {
                    return Self::diverged();
                };
                let value = self.emit(CfgInstData::Mul(lhs_val, rhs_val), ty, span);
                self.cache(air_ref, value);
                ExprResult {
                    value: Some(value),
                    continuation: Continuation::Continues,
                }
            }

            AirInstData::WrappingAdd(lhs, rhs) => {
                let Some(lhs_val) = self.lower_value(*lhs) else {
                    return Self::diverged();
                };
                let Some(rhs_val) = self.lower_value(*rhs) else {
                    return Self::diverged();
                };
                let value = self.emit(CfgInstData::WrappingAdd(lhs_val, rhs_val), ty, span);
                self.cache(air_ref, value);
                ExprResult {
                    value: Some(value),
                    continuation: Continuation::Continues,
                }
            }

            AirInstData::WrappingSub(lhs, rhs) => {
                let Some(lhs_val) = self.lower_value(*lhs) else {
                    return Self::diverged();
                };
                let Some(rhs_val) = self.lower_value(*rhs) else {
                    return Self::diverged();
                };
                let value = self.emit(CfgInstData::WrappingSub(lhs_val, rhs_val), ty, span);
                self.cache(air_ref, value);
                ExprResult {
                    value: Some(value),
                    continuation: Continuation::Continues,
                }
            }

            AirInstData::WrappingMul(lhs, rhs) => {
                let Some(lhs_val) = self.lower_value(*lhs) else {
                    return Self::diverged();
                };
                let Some(rhs_val) = self.lower_value(*rhs) else {
                    return Self::diverged();
                };
                let value = self.emit(CfgInstData::WrappingMul(lhs_val, rhs_val), ty, span);
                self.cache(air_ref, value);
                ExprResult {
                    value: Some(value),
                    continuation: Continuation::Continues,
                }
            }

            AirInstData::Div(lhs, rhs) => {
                let Some(lhs_val) = self.lower_value(*lhs) else {
                    return Self::diverged();
                };
                let Some(rhs_val) = self.lower_value(*rhs) else {
                    return Self::diverged();
                };
                let value = self.emit(CfgInstData::Div(lhs_val, rhs_val), ty, span);
                self.cache(air_ref, value);
                ExprResult {
                    value: Some(value),
                    continuation: Continuation::Continues,
                }
            }

            AirInstData::Mod(lhs, rhs) => {
                let Some(lhs_val) = self.lower_value(*lhs) else {
                    return Self::diverged();
                };
                let Some(rhs_val) = self.lower_value(*rhs) else {
                    return Self::diverged();
                };
                let value = self.emit(CfgInstData::Mod(lhs_val, rhs_val), ty, span);
                self.cache(air_ref, value);
                ExprResult {
                    value: Some(value),
                    continuation: Continuation::Continues,
                }
            }

            AirInstData::Eq(lhs, rhs) => {
                let Some(lhs_val) = self.lower_value(*lhs) else {
                    return Self::diverged();
                };
                let Some(rhs_val) = self.lower_value(*rhs) else {
                    return Self::diverged();
                };
                let value = self.emit(CfgInstData::Eq(lhs_val, rhs_val), ty, span);
                self.cache(air_ref, value);
                ExprResult {
                    value: Some(value),
                    continuation: Continuation::Continues,
                }
            }

            AirInstData::Ne(lhs, rhs) => {
                let Some(lhs_val) = self.lower_value(*lhs) else {
                    return Self::diverged();
                };
                let Some(rhs_val) = self.lower_value(*rhs) else {
                    return Self::diverged();
                };
                let value = self.emit(CfgInstData::Ne(lhs_val, rhs_val), ty, span);
                self.cache(air_ref, value);
                ExprResult {
                    value: Some(value),
                    continuation: Continuation::Continues,
                }
            }

            AirInstData::Lt(lhs, rhs) => {
                let Some(lhs_val) = self.lower_value(*lhs) else {
                    return Self::diverged();
                };
                let Some(rhs_val) = self.lower_value(*rhs) else {
                    return Self::diverged();
                };
                let value = self.emit(CfgInstData::Lt(lhs_val, rhs_val), ty, span);
                self.cache(air_ref, value);
                ExprResult {
                    value: Some(value),
                    continuation: Continuation::Continues,
                }
            }

            AirInstData::Gt(lhs, rhs) => {
                let Some(lhs_val) = self.lower_value(*lhs) else {
                    return Self::diverged();
                };
                let Some(rhs_val) = self.lower_value(*rhs) else {
                    return Self::diverged();
                };
                let value = self.emit(CfgInstData::Gt(lhs_val, rhs_val), ty, span);
                self.cache(air_ref, value);
                ExprResult {
                    value: Some(value),
                    continuation: Continuation::Continues,
                }
            }

            AirInstData::Le(lhs, rhs) => {
                let Some(lhs_val) = self.lower_value(*lhs) else {
                    return Self::diverged();
                };
                let Some(rhs_val) = self.lower_value(*rhs) else {
                    return Self::diverged();
                };
                let value = self.emit(CfgInstData::Le(lhs_val, rhs_val), ty, span);
                self.cache(air_ref, value);
                ExprResult {
                    value: Some(value),
                    continuation: Continuation::Continues,
                }
            }

            AirInstData::Ge(lhs, rhs) => {
                let Some(lhs_val) = self.lower_value(*lhs) else {
                    return Self::diverged();
                };
                let Some(rhs_val) = self.lower_value(*rhs) else {
                    return Self::diverged();
                };
                let value = self.emit(CfgInstData::Ge(lhs_val, rhs_val), ty, span);
                self.cache(air_ref, value);
                ExprResult {
                    value: Some(value),
                    continuation: Continuation::Continues,
                }
            }

            AirInstData::And(lhs, rhs) => self.lower_short_circuit(air_ref, *lhs, *rhs, true, span),

            AirInstData::Or(lhs, rhs) => self.lower_short_circuit(air_ref, *lhs, *rhs, false, span),

            AirInstData::Neg(operand) => {
                let Some(op_val) = self.lower_value(*operand) else {
                    return Self::diverged();
                };
                let value = self.emit(CfgInstData::Neg(op_val), ty, span);
                self.cache(air_ref, value);
                ExprResult {
                    value: Some(value),
                    continuation: Continuation::Continues,
                }
            }

            AirInstData::Not(operand) => {
                let Some(op_val) = self.lower_value(*operand) else {
                    return Self::diverged();
                };
                let value = self.emit(CfgInstData::Not(op_val), ty, span);
                self.cache(air_ref, value);
                ExprResult {
                    value: Some(value),
                    continuation: Continuation::Continues,
                }
            }

            AirInstData::BitNot(operand) => {
                let Some(op_val) = self.lower_value(*operand) else {
                    return Self::diverged();
                };
                let value = self.emit(CfgInstData::BitNot(op_val), ty, span);
                self.cache(air_ref, value);
                ExprResult {
                    value: Some(value),
                    continuation: Continuation::Continues,
                }
            }

            AirInstData::BitAnd(lhs, rhs) => {
                let Some(lhs_val) = self.lower_value(*lhs) else {
                    return Self::diverged();
                };
                let Some(rhs_val) = self.lower_value(*rhs) else {
                    return Self::diverged();
                };
                let value = self.emit(CfgInstData::BitAnd(lhs_val, rhs_val), ty, span);
                self.cache(air_ref, value);
                ExprResult {
                    value: Some(value),
                    continuation: Continuation::Continues,
                }
            }

            AirInstData::BitOr(lhs, rhs) => {
                let Some(lhs_val) = self.lower_value(*lhs) else {
                    return Self::diverged();
                };
                let Some(rhs_val) = self.lower_value(*rhs) else {
                    return Self::diverged();
                };
                let value = self.emit(CfgInstData::BitOr(lhs_val, rhs_val), ty, span);
                self.cache(air_ref, value);
                ExprResult {
                    value: Some(value),
                    continuation: Continuation::Continues,
                }
            }

            AirInstData::BitXor(lhs, rhs) => {
                let Some(lhs_val) = self.lower_value(*lhs) else {
                    return Self::diverged();
                };
                let Some(rhs_val) = self.lower_value(*rhs) else {
                    return Self::diverged();
                };
                let value = self.emit(CfgInstData::BitXor(lhs_val, rhs_val), ty, span);
                self.cache(air_ref, value);
                ExprResult {
                    value: Some(value),
                    continuation: Continuation::Continues,
                }
            }

            AirInstData::Shl(lhs, rhs) => {
                let Some(lhs_val) = self.lower_value(*lhs) else {
                    return Self::diverged();
                };
                let Some(rhs_val) = self.lower_value(*rhs) else {
                    return Self::diverged();
                };
                let value = self.emit(CfgInstData::Shl(lhs_val, rhs_val), ty, span);
                self.cache(air_ref, value);
                ExprResult {
                    value: Some(value),
                    continuation: Continuation::Continues,
                }
            }

            AirInstData::Shr(lhs, rhs) => {
                let Some(lhs_val) = self.lower_value(*lhs) else {
                    return Self::diverged();
                };
                let Some(rhs_val) = self.lower_value(*rhs) else {
                    return Self::diverged();
                };
                let value = self.emit(CfgInstData::Shr(lhs_val, rhs_val), ty, span);
                self.cache(air_ref, value);
                ExprResult {
                    value: Some(value),
                    continuation: Continuation::Continues,
                }
            }

            AirInstData::Alloc { slot, init } => {
                let init_result = self.lower_inst(*init);
                // If the initializer diverges (e.g. `let x = { return 7; 2 };`),
                // the binding never happens and everything after the `let` is
                // unreachable. Propagate the divergence — previously this arm
                // always reported Continues, so the builder kept lowering the
                // rest of the block and silently OVERWROTE the initializer's
                // Return terminator with the later code's. (RUE-128)
                if matches!(init_result.continuation, Continuation::Diverged) {
                    return Self::diverged();
                }
                // If init produces a value, use it; otherwise use a dummy Unit value
                let init_val = init_result
                    .value
                    .unwrap_or_else(|| self.emit(CfgInstData::Const(0), Type::UNIT, span));
                self.emit(
                    CfgInstData::Alloc {
                        slot: *slot,
                        init: init_val,
                    },
                    Type::UNIT,
                    span,
                );
                // Initialization fills the slot with a fresh value: any
                // moved-out state from a previous occupant is stale.
                self.moved.clear_slot(MovedSlot::Local(*slot));
                let init_ty = self.air.get(*init).ty;
                self.arm_drop_flag_if_needed(MovedSlot::Local(*slot), init_ty, span);

                // StorageLive starts the storage lifetime, but the slot does
                // not own a value until its initializer completes. In
                // particular, `let value = fallible()?` can return from the
                // initializer before this Alloc is reached. Marking ownership
                // only now keeps those early-exit paths from loading and
                // dropping uninitialized storage while still letting them
                // emit StorageDead.
                if let Some(live_slot) = self
                    .scope_stack
                    .iter_mut()
                    .rev()
                    .find_map(|scope| scope.iter_mut().rev().find(|live| live.slot == *slot))
                {
                    live_slot.initialized = true;
                }
                ExprResult {
                    value: None,
                    continuation: Continuation::Continues,
                }
            }

            AirInstData::Load { slot } => {
                let value = self.emit(CfgInstData::Load { slot: *slot }, ty, span);
                self.cache(air_ref, value);
                ExprResult {
                    value: Some(value),
                    continuation: Continuation::Continues,
                }
            }

            AirInstData::Store { slot, value } => {
                let Some(val) = self.lower_value(*value) else {
                    return Self::diverged();
                };
                // The old value, if still owned, is dropped before being
                // overwritten (RUE-64): evaluate RHS, drop old, store new.
                let val_ty = self.air.get(*value).ty;
                self.emit_overwrite_drop(MovedSlot::Local(*slot), val_ty, span);
                self.emit(
                    CfgInstData::Store {
                        slot: *slot,
                        value: val,
                    },
                    Type::UNIT,
                    span,
                );
                // Assigning to the slot re-initializes it: a previously
                // moved-out value has been replaced, so it must be dropped
                // again at scope exit.
                self.moved.clear_slot(MovedSlot::Local(*slot));
                self.update_drop_flag(MovedSlot::Local(*slot), true, span);
                ExprResult {
                    value: None,
                    continuation: Continuation::Continues,
                }
            }

            AirInstData::ParamStore { param_slot, value } => {
                let Some(val) = self.lower_value(*value) else {
                    return Self::diverged();
                };
                // Drop the overwritten old value first (RUE-64); for an
                // `inout` param this is the caller's value being replaced.
                let val_ty = self.air.get(*value).ty;
                self.emit_overwrite_drop(MovedSlot::Param(*param_slot), val_ty, span);
                self.emit(
                    CfgInstData::ParamStore {
                        param_slot: *param_slot,
                        value: val,
                    },
                    Type::UNIT,
                    span,
                );
                // Re-initialization: see the Store arm above.
                self.moved.clear_slot(MovedSlot::Param(*param_slot));
                self.update_drop_flag(MovedSlot::Param(*param_slot), true, span);
                ExprResult {
                    value: None,
                    continuation: Continuation::Continues,
                }
            }

            AirInstData::Call {
                runtime,
                name,
                args,
            } => {
                if let Some(runtime) = runtime {
                    self.assert_valid_runtime_call_args(
                        *runtime,
                        self.air
                            .get_call_args(args)
                            .map(|arg| (arg.value, arg.mode)),
                        ty,
                    );
                }
                let mut arg_vals = Vec::new();
                for arg in self.air.get_call_args(args) {
                    let Some(value) = self.lower_value(arg.value) else {
                        return Self::diverged();
                    };
                    arg_vals.push(CfgCallArg {
                        value,
                        mode: Self::convert_arg_mode(arg.mode),
                    });
                }
                // Store args in extra array
                let args_result = self.cfg.push_call_args(arg_vals);
                let args = self.payload_or(args_result, CfgCallArgs::EMPTY, span);
                let value = self.emit(
                    CfgInstData::Call {
                        runtime: *runtime,
                        name: *name,
                        args,
                    },
                    ty,
                    span,
                );
                // A call to a `-> !` function never returns: end the block
                // here and diverge, exactly like `return`. Handing back a
                // NEVER-typed "result" would let an enclosing if/match join
                // thread it into a block-parameter of the other arm's type —
                // an ill-typed edge (RUE-347).
                if ty == Type::NEVER {
                    self.cfg
                        .set_terminator(self.current_block, Terminator::Unreachable);
                    return ExprResult {
                        value: None,
                        continuation: Continuation::Diverged,
                    };
                }
                self.cache(air_ref, value);
                ExprResult {
                    value: Some(value),
                    continuation: Continuation::Continues,
                }
            }

            AirInstData::AccessorCall { name, args } => {
                let mut arg_vals = Vec::new();
                for arg in self.air.get_call_args(args) {
                    let Some(value) = self.lower_value(arg.value) else {
                        return Self::diverged();
                    };
                    arg_vals.push(CfgCallArg {
                        value,
                        mode: Self::convert_arg_mode(arg.mode),
                    });
                }
                let args_result = self.cfg.push_call_args(arg_vals);
                let args = self.payload_or(args_result, CfgCallArgs::EMPTY, span);
                let value = self.emit(CfgInstData::AccessorCall { name: *name, args }, ty, span);
                self.cache(air_ref, value);
                ExprResult {
                    value: Some(value),
                    continuation: Continuation::Continues,
                }
            }

            AirInstData::Intrinsic {
                runtime,
                name,
                args,
            } => {
                if let Some(runtime) = runtime {
                    self.assert_valid_runtime_call_args(
                        *runtime,
                        self.air
                            .get_intrinsic_args(args)
                            .map(|arg| (arg, AirArgMode::Normal)),
                        ty,
                    );
                }
                let mut arg_vals = Vec::new();
                for arg in self.air.get_intrinsic_args(args) {
                    let Some(val) = self.lower_value(arg) else {
                        return Self::diverged();
                    };
                    arg_vals.push(val);
                }
                // @raw/@raw_mut/@field_ptr take the ADDRESS of their place
                // operand (arg 0): codegen requires that operand to still be a
                // `Load`/`PlaceRead` when it lowers the intrinsic. Pin the
                // operand's base slot so optimization passes (constopt) never
                // rewrite its loads into constants — a `Const` operand would be
                // dereferenced as an address (RUE-521 O1+ segfault).
                if matches!(self.interner.resolve(name), "raw" | "raw_mut" | "field_ptr")
                    && let Some(&place_val) = arg_vals.first()
                {
                    match &self.cfg.get_inst(place_val).data {
                        CfgInstData::Load { slot } => self.cfg.mark_address_taken(*slot),
                        CfgInstData::PlaceRead { place } => match place.base {
                            PlaceBase::Local(slot) => self.cfg.mark_address_taken(slot),
                            // A parameter's address escaping means repeated
                            // Param reads are no longer interchangeable
                            // (@ptr_write can mutate the storage between
                            // them) — record it so CSE's param keying skips
                            // the slot (RUE-914 hunt finding).
                            PlaceBase::Param(slot) => self.cfg.mark_param_address_taken(slot),
                            PlaceBase::Accessor(_) => {}
                        },
                        // A bare scalar parameter lowers directly to a Param
                        // value with no backing local; its address escaping
                        // must be recorded on the PARAM slot.
                        CfgInstData::Param { index } => {
                            self.cfg.mark_param_address_taken(*index);
                        }
                        _ => {}
                    }
                }
                // Store args in extra array
                let args_result = self.cfg.push_intrinsic_args(arg_vals);
                let args = self.payload_or(args_result, CfgIntrinsicArgs::EMPTY, span);
                let intrinsic_value = self.emit(
                    CfgInstData::Intrinsic {
                        runtime: *runtime,
                        name: *name,
                        args,
                    },
                    ty,
                    span,
                );
                // A never-typed intrinsic (`@panic`) aborts and never returns.
                // Keep the intrinsic in the block so cfg_lower still emits its
                // abort call, then end the block and diverge exactly like a
                // `-> !` call (RUE-347/RUE-512). Handing back a NEVER "result"
                // would let an enclosing if/match join thread it into the other
                // arm's block-parameter type — an ill-typed edge.
                if ty == Type::NEVER {
                    self.cfg
                        .set_terminator(self.current_block, Terminator::Unreachable);
                    return ExprResult {
                        value: None,
                        continuation: Continuation::Diverged,
                    };
                }
                // Unit has no runtime representation, and side-effect-only
                // intrinsics such as @assert and @dbg deliberately do not define
                // a backend vreg. Preserve the intrinsic itself in the block for
                // its side effect, but use the same dummy unit value as
                // UnitConst whenever the expression needs a value (notably as the
                // tail of a unit-returning function).
                let value = if ty == Type::UNIT {
                    self.emit(CfgInstData::Const(0), Type::UNIT, span)
                } else {
                    intrinsic_value
                };
                self.cache(air_ref, value);
                ExprResult {
                    value: Some(value),
                    continuation: Continuation::Continues,
                }
            }

            AirInstData::StructInit {
                struct_id,
                fields,
                source_order,
            } => {
                // Evaluate field initializers in SOURCE ORDER (spec 4.0:8)
                // The source_order tells us which declaration-order index to evaluate at each step
                let fields: Vec<AirRef> = self.air.get_struct_fields(fields).collect();
                let source_order: Vec<usize> = self.air.get_source_order(source_order).collect();

                let mut lowered_fields: Vec<Option<CfgValue>> = vec![None; fields.len()];
                for decl_idx in source_order {
                    let Some(lowered) = self.lower_value(fields[decl_idx]) else {
                        return Self::diverged();
                    };
                    lowered_fields[decl_idx] = Some(lowered);
                }

                // Collect in declaration order for storage layout
                let field_vals: Vec<CfgValue> = lowered_fields
                    .into_iter()
                    .map(|opt: Option<CfgValue>| opt.expect("all fields should be lowered"))
                    .collect();

                // Store fields in extra array
                let fields_result = self.cfg.push_struct_fields(field_vals);
                let fields = self.payload_or(fields_result, CfgStructFields::EMPTY, span);
                let value = self.emit(
                    CfgInstData::StructInit {
                        struct_id: *struct_id,
                        fields,
                    },
                    ty,
                    span,
                );
                self.cache(air_ref, value);
                ExprResult {
                    value: Some(value),
                    continuation: Continuation::Continues,
                }
            }

            AirInstData::Block { statements, value } => {
                // Collect statements into a Vec for iteration (needed for checking remaining)
                let statements: Vec<AirRef> = self.air.get_block_statements(statements).collect();

                // A binding's storage wrapper — `Block { [StorageLive s], Alloc s }`,
                // emitted by `let` lowering to pair a slot's storage annotation with
                // its initializer — must NOT open a drop scope: the binding belongs
                // to the enclosing block, which is where its drop is scheduled.
                //
                // The shape is matched exactly: every statement is a `StorageLive`
                // AND the block's value is the `Alloc` that initializes one of those
                // slots. A block whose statements are `StorageLive`s but whose value
                // is an ordinary expression is a real drop scope. That is what a
                // borrow-operand temporary is (RUE-953): its `StorageLive` is hoisted
                // to a wrapper around the call so the temporary's drop lands *after*
                // the call, while its `Alloc` stays at the operand's argument
                // position so evaluation order is unchanged.
                let value_alloc_slot = match self.air.get(*value).data {
                    AirInstData::Alloc { slot, .. } => Some(slot),
                    _ => None,
                };
                let is_storage_live_wrapper = statements.iter().all(|stmt| {
                    matches!(self.air.get(*stmt).data, AirInstData::StorageLive { .. })
                }) && statements.iter().any(|stmt| {
                    matches!(
                        self.air.get(*stmt).data,
                        AirInstData::StorageLive { slot } if Some(slot) == value_alloc_slot
                    )
                });

                // Only push a scope if this is a real syntactic block (not a StorageLive wrapper)
                if !is_storage_live_wrapper {
                    self.scope_stack.push(Vec::new());
                }

                // Lower each statement.
                //
                // Design decision: When a statement diverges (break/continue/return), we only
                // warn about the *first* unreachable statement or value expression following it.
                // This matches Rust's behavior and avoids flooding the user with redundant
                // warnings for code like:
                //   break;
                //   x = 1;  // warn about this
                //   y = 2;  // don't warn about this (already covered by first warning)
                for (i, stmt) in statements.iter().enumerate() {
                    let result = self.lower_inst(*stmt);
                    if matches!(result.continuation, Continuation::Diverged) {
                        // Get the span of the diverging statement for the secondary label
                        let diverging_span = self.air.get(*stmt).span;

                        // Check if there are remaining statements or a value expression
                        // that will never be executed
                        let remaining = &statements[i + 1..];
                        if !remaining.is_empty() {
                            // Warn about the first unreachable statement
                            let unreachable_stmt = remaining[0];
                            let unreachable_span = self.air.get(unreachable_stmt).span;
                            self.emit_unreachable_warning(unreachable_span, diverging_span);
                        } else {
                            // The final value expression is unreachable.
                            // However, don't warn about synthetic unit values (created by parser
                            // when a block has no trailing expression). These have zero-length
                            // spans pointing at the closing brace.
                            let value_span = self.air.get(*value).span;
                            let is_synthetic = value_span.start == value_span.end;
                            if !is_synthetic {
                                self.emit_unreachable_warning(value_span, diverging_span);
                            }
                        }
                        // Note: drops were already emitted by the diverging statement
                        // (break/continue/return handle their own drops), so we pop
                        // our scope WITHOUT emitting cleanup. Popping is still
                        // required: a leaked entry unbalances the LIFO pairing, so a
                        // still-reachable enclosing block (e.g. the loop-exit path
                        // after a `break`) would later pop THIS scope instead of its
                        // own and re-drop these slots on a live path.
                        if !is_storage_live_wrapper {
                            self.scope_stack.pop();
                        }
                        return ExprResult {
                            value: None,
                            continuation: Continuation::Diverged,
                        };
                    }

                    // A statement's discarded result is a temporary the
                    // statement owns (`D { v: 7 };`, `make();`, `let _ = …`,
                    // a discarded if/match result): drop it at the end of
                    // the statement (RUE-65, RUE-66). Values forwarded into
                    // calls are NOT affected — the callee owns and drops
                    // those; only the statement's own result is dropped
                    // here. Moves out of locals (`d;`) were already marked
                    // by MarkMoved, so this drop is the move's destination.
                    if let Some(temp_val) = result.value {
                        let stmt_ty = self.air.get(*stmt).ty;
                        if self.type_needs_drop(stmt_ty) {
                            let stmt_span = self.air.get(*stmt).span;
                            self.emit(CfgInstData::Drop { value: temp_val }, Type::UNIT, stmt_span);
                        }
                    }
                }

                // Lower the final value
                let mut result = self.lower_inst(*value);

                // Pop scope and emit StorageDead (with Drop if needed) in reverse order.
                // BUT: if the value diverged (break/continue/return), the diverging
                // instruction already emitted drops for all scopes via emit_drops_for_all_scopes,
                // so we must NOT emit duplicate StorageDead here.
                if !is_storage_live_wrapper {
                    if let Some(scope_slots) = self.scope_stack.pop() {
                        // Only emit scope cleanup if the value didn't diverge
                        if !matches!(result.continuation, Continuation::Diverged) {
                            // Preserve a block result in frame storage before
                            // running its scope cleanup. A multi-slot result is
                            // not safe to keep only in backend vregs here:
                            // destructor lowering introduces more aggregate
                            // operands, and register coalescing can otherwise
                            // make those cleanup values reuse a result slot.
                            //
                            // This scratch region is not a second owner. It is
                            // the block expression's value crossing the cleanup
                            // boundary, so it deliberately is not added to the
                            // drop scope. The post-cleanup Load continues the
                            // same value and the eventual consumer owns it.
                            let preserved = if !scope_slots.is_empty()
                                && let Some(value) = result.value
                                && ty != Type::UNIT
                                && ty != Type::NEVER
                                && self.type_pool.abi_slot_count(ty) > 1
                            {
                                let width = self.type_pool.abi_slot_count(ty).max(1);
                                let slot = self.cfg.alloc_temp_local_slots(width);
                                self.emit(
                                    CfgInstData::StorageLive { slot, local_ty: ty },
                                    Type::UNIT,
                                    span,
                                );
                                self.emit(
                                    CfgInstData::Alloc { slot, init: value },
                                    Type::UNIT,
                                    span,
                                );
                                Some(slot)
                            } else {
                                None
                            };

                            for live_slot in scope_slots.into_iter().rev() {
                                let slot_span = live_slot.span;
                                self.emit_drop_for_slot(&live_slot, slot_span);
                            }

                            if let Some(slot) = preserved {
                                let value = self.emit(CfgInstData::Load { slot }, ty, span);
                                self.emit(
                                    CfgInstData::StorageDead { slot, local_ty: ty },
                                    Type::UNIT,
                                    span,
                                );
                                result.value = Some(value);
                            }
                        }
                    }
                }

                result
            }

            AirInstData::Branch {
                cond,
                then_value,
                else_value,
            } => {
                let Some(cond_val) = self.lower_value(*cond) else {
                    return Self::diverged();
                };

                let then_block = self.cfg.new_block();
                let else_block = self.cfg.new_block();
                let join_block = self.cfg.new_block();

                // Get types for then/else
                let then_type = self.air.get(*then_value).ty;
                let else_type = else_value.map(|e| self.air.get(e).ty);

                // Branch to then/else
                let then_result = self.cfg.push_then_args(std::iter::empty());
                let then_args = self.payload_or(then_result, CfgThenArgs::EMPTY, span);
                let else_result = self.cfg.push_else_args(std::iter::empty());
                let else_args = self.payload_or(else_result, CfgElseArgs::EMPTY, span);
                self.cfg.set_terminator(
                    self.current_block,
                    Terminator::Branch {
                        cond: cond_val,
                        then_block,
                        then_args,
                        else_block,
                        else_args,
                    },
                );

                // Each branch starts move tracking from the pre-branch state.
                let moved_before = self.moved.clone();

                // Lower then branch
                self.current_block = then_block;
                let then_result = self.lower_inst(*then_value);
                let then_exit_block = self.current_block;
                let then_diverged = matches!(then_result.continuation, Continuation::Diverged);
                let moved_then = std::mem::replace(&mut self.moved, moved_before);

                // Lower else branch
                self.current_block = else_block;
                let else_result = if let Some(else_val) = else_value {
                    self.lower_inst(*else_val)
                } else {
                    // No else - emit unit
                    let unit_val = self.emit(CfgInstData::Const(0), Type::UNIT, span);
                    ExprResult {
                        value: Some(unit_val),
                        continuation: Continuation::Continues,
                    }
                };
                let else_exit_block = self.current_block;
                let else_diverged = matches!(else_result.continuation, Continuation::Diverged);

                // Merge move state at the join: a slot counts as moved after
                // the if only when it is moved on every path reaching the
                // join. A diverged branch contributes no path to the join,
                // so only the other branch's state matters.
                // (self.moved currently holds the else-branch state.)
                match (then_diverged, else_diverged) {
                    (true, true) => {}  // join unreachable; state is irrelevant
                    (true, false) => {} // keep else state
                    (false, true) => self.moved = moved_then,
                    (false, false) => {
                        self.moved = self.moved.intersect(&moved_then);
                    }
                }

                // If both branches diverge, mark join block as unreachable and diverge
                if then_diverged && else_diverged {
                    self.cfg.set_terminator(join_block, Terminator::Unreachable);
                    return ExprResult {
                        value: None,
                        continuation: Continuation::Diverged,
                    };
                }

                // Determine result type
                let result_type = if then_type.is_never() {
                    else_type.unwrap_or(Type::UNIT)
                } else {
                    then_type
                };

                // Add block parameter for result (if we have a value type)
                let result_param = if result_type != Type::UNIT && result_type != Type::NEVER {
                    Some(self.cfg.add_block_param(join_block, result_type))
                } else {
                    None
                };

                // Wire up non-divergent branches to join
                if !then_diverged {
                    self.goto_join(then_exit_block, join_block, result_param, then_result.value);
                }

                if !else_diverged {
                    self.goto_join(else_exit_block, join_block, result_param, else_result.value);
                }

                self.current_block = join_block;

                if let Some(param) = result_param {
                    self.cache(air_ref, param);
                }

                ExprResult {
                    value: result_param,
                    continuation: Continuation::Continues,
                }
            }

            AirInstData::Loop { cond, body } => {
                let header_block = self.cfg.new_block();
                let body_block = self.cfg.new_block();
                let exit_block = self.cfg.new_block();

                // The body may execute zero times, so moves inside the
                // condition/body are not "moved on all paths" at the loop
                // exit: restore the pre-loop move state after lowering.
                // (Sema's back-edge move recheck already rejects moving an
                // outer variable inside a loop, so this conservatism cannot
                // actually reintroduce a double-drop in accepted programs.)
                let moved_before_loop = self.moved.clone();

                // Jump to header
                self.goto_no_args(self.current_block, header_block);

                // Lower condition in header — BEFORE pushing this while's loop
                // context, so a break/continue inside the condition resolves via
                // loop_stack.last() to the ENCLOSING loop, not to the while being
                // constructed. This matches sema, which treats a while-condition as
                // lexically OUTSIDE the while's loop scope (RUE-208; spec 4.8:7,21).
                self.current_block = header_block;
                let Some(cond_val) = self.lower_value(*cond) else {
                    // The condition itself diverges — either a plain divergence
                    // (e.g. `while return 8 {}`) or a break/continue that targeted an
                    // enclosing loop and already set this block's terminator. Either
                    // way the loop body and everything after the loop are unreachable.
                    // The body/exit blocks allocated above are now orphaned — mark
                    // them Unreachable so codegen does not assert on a missing
                    // terminator. No loop context was pushed yet, so nothing to pop.
                    self.cfg.set_terminator(body_block, Terminator::Unreachable);
                    self.cfg.set_terminator(exit_block, Terminator::Unreachable);
                    return Self::diverged();
                };

                // Push loop context with current scope depth. Pushed AFTER the
                // condition (see above) so break/continue in the BODY target this
                // loop. The scope depth is captured before the loop body is lowered,
                // so break/continue will drop all slots in scopes created INSIDE the
                // loop.
                self.loop_stack.push(LoopContext {
                    header: header_block,
                    exit: exit_block,
                    scope_depth: self.scope_stack.len(),
                });

                // Branch: if true go to body, if false exit
                let then_result = self.cfg.push_then_args(std::iter::empty());
                let then_args = self.payload_or(then_result, CfgThenArgs::EMPTY, span);
                let else_result = self.cfg.push_else_args(std::iter::empty());
                let else_args = self.payload_or(else_result, CfgElseArgs::EMPTY, span);
                self.cfg.set_terminator(
                    self.current_block,
                    Terminator::Branch {
                        cond: cond_val,
                        then_block: body_block,
                        then_args,
                        else_block: exit_block,
                        else_args,
                    },
                );

                // Lower body
                self.current_block = body_block;
                let body_result = self.lower_inst(*body);

                // After body, go back to header (unless diverged)
                if !matches!(body_result.continuation, Continuation::Diverged) {
                    self.goto_no_args(self.current_block, header_block);
                }

                self.loop_stack.pop();
                self.moved = moved_before_loop;

                // Continue after loop
                self.current_block = exit_block;

                // Loops produce a unit value (for use in unit-returning functions)
                let unit_val = self.emit(CfgInstData::Const(0), Type::UNIT, span);
                ExprResult {
                    value: Some(unit_val),
                    continuation: Continuation::Continues,
                }
            }

            AirInstData::InfiniteLoop { body } => {
                // Infinite loop: loop { body }
                //
                // Structure (2 blocks, not 3):
                //   body_block: execute body, then goto body_block
                //   exit_block: only reachable via break
                //
                // Unlike while loops, there's no condition check, so we don't need
                // a separate header block. The body_block serves as both the loop
                // entry point and the continue target.
                let body_block = self.cfg.new_block();
                let exit_block = self.cfg.new_block();

                // The exit is only reached via break, and different breaks
                // may have different move states; conservatively restore the
                // pre-loop state after lowering (see the Loop arm above).
                let moved_before_loop = self.moved.clone();

                // Jump to body
                self.goto_no_args(self.current_block, body_block);

                // Push loop context (body_block is the continue target).
                // The scope depth is captured BEFORE the loop body is lowered,
                // so break/continue will drop all slots in scopes created INSIDE the loop.
                self.loop_stack.push(LoopContext {
                    header: body_block,
                    exit: exit_block,
                    scope_depth: self.scope_stack.len(),
                });

                // Lower body
                self.current_block = body_block;
                let body_result = self.lower_inst(*body);

                // After body, go back to start (unless diverged via return/break/continue)
                if !matches!(body_result.continuation, Continuation::Diverged) {
                    self.goto_no_args(self.current_block, body_block);
                }

                self.loop_stack.pop();
                self.moved = moved_before_loop;

                // Continue after loop (only reachable via break).
                // Set Unreachable as the initial terminator. If there's code after the loop
                // (which requires a break to be reachable), the subsequent Ret instruction
                // will overwrite this with the correct Return terminator. If there's no break,
                // the block is truly unreachable and Unreachable is correct.
                self.current_block = exit_block;
                self.cfg
                    .set_terminator(self.current_block, Terminator::Unreachable);

                // A loop containing a break has type `()`: control continues
                // at exit_block (reached only via break) and the loop's value
                // is unit. A break-less loop is `!` (spec 4.8:17/4.8:21): its
                // exit block is unreachable, and handing back a unit value
                // here would let an enclosing if/match join thread it into a
                // block-parameter of the *other* arm's type — an ill-typed
                // edge (RUE-347). Diverge instead, like any `!` expression.
                if ty == Type::NEVER {
                    return ExprResult {
                        value: None,
                        continuation: Continuation::Diverged,
                    };
                }
                let unit_val = self.emit(CfgInstData::Const(0), Type::UNIT, span);
                ExprResult {
                    value: Some(unit_val),
                    continuation: Continuation::Continues,
                }
            }

            AirInstData::Match { scrutinee, arms } => {
                // Lower the scrutinee
                let Some(scrutinee_val) = self.lower_value(*scrutinee) else {
                    return Self::diverged();
                };
                // A match evaluates its scrutinee exactly once. In particular,
                // synthetic wrapper blocks may own temporaries whose cleanup
                // runs while producing the enum value. Payload reads in the
                // arms refer back to the same AIR scrutinee; cache its already
                // lowered value so those reads cannot replay the wrapper and
                // drop its temporaries a second time. The switch block
                // dominates every arm, so this cached CFG value is valid on
                // all of those paths.
                self.cache(*scrutinee, scrutinee_val);

                // Collect arms into a Vec for iteration
                let arms: Vec<(AirPattern, AirRef)> = self.air.get_match_arms(arms).collect();

                // `match v {}` on a zero-variant enum (RUE-169): the scrutinee
                // type is uninhabited, so this point can never be reached with
                // a value. Sema verified exhaustiveness (vacuously); no switch
                // is needed and control diverges.
                if arms.is_empty() {
                    self.cfg
                        .set_terminator(self.current_block, Terminator::Unreachable);
                    return Self::diverged();
                }

                // Create blocks for each arm and a join block
                let arm_blocks: Vec<_> = arms.iter().map(|_| self.cfg.new_block()).collect();
                let join_block = self.cfg.new_block();

                // Get result type (from first non-Never arm)
                let result_type = arms
                    .iter()
                    .map(|(_, body)| self.air.get(*body).ty)
                    .find(|ty| !ty.is_never())
                    .unwrap_or(Type::NEVER);

                // Create the switch terminator
                // Build cases: for each arm, check pattern and jump to corresponding block
                let mut switch_cases = Vec::new();
                let mut default_block = None;

                for (i, (pattern, _)) in arms.iter().enumerate() {
                    match pattern {
                        AirPattern::Wildcard => {
                            default_block = Some(arm_blocks[i]);
                            // Wildcard matches everything - any patterns after this are unreachable
                            break;
                        }
                        AirPattern::Int(n) => {
                            switch_cases.push((*n, arm_blocks[i]));
                        }
                        AirPattern::Bool(b) => {
                            // Booleans are 0 or 1
                            let val = if *b { 1 } else { 0 };
                            switch_cases.push((val, arm_blocks[i]));
                        }
                        AirPattern::EnumVariant { variant_index, .. } => {
                            // Enum variants are matched by their discriminant (variant index)
                            switch_cases.push((*variant_index as i64, arm_blocks[i]));
                        }
                    }
                }

                // If no explicit wildcard, use the last arm as default
                // This handles exhaustive matches like `true => ..., false => ...`
                // where semantics verified exhaustiveness but we need a default for codegen
                let default = default_block.unwrap_or_else(|| {
                    // Pop the last case to use as default
                    let (_, last_block) = switch_cases
                        .pop()
                        .expect("match must have at least one arm");
                    last_block
                });

                // Set the switch terminator on current block
                let cases_result = self.cfg.push_switch_cases(switch_cases);
                let cases = self.payload_or(cases_result, CfgSwitchCases::EMPTY, span);
                self.cfg.set_terminator(
                    self.current_block,
                    Terminator::Switch {
                        scrutinee: scrutinee_val,
                        cases,
                        default,
                    },
                );

                // Lower each arm and wire to join block.
                // Each arm starts move tracking from the pre-match state; the
                // state after the match is the intersection of the exit
                // states of the arms that reach the join ("moved on ALL
                // paths" — see the `moved` field docs).
                let mut all_diverged = true;
                let mut arm_results = Vec::new();
                let moved_before = self.moved.clone();
                let mut moved_join: Option<MoveState> = None;

                for (i, (_, body)) in arms.iter().enumerate() {
                    self.current_block = arm_blocks[i];
                    self.moved = moved_before.clone();
                    let body_result = self.lower_inst(*body);
                    let exit_block = self.current_block;
                    let diverged = matches!(body_result.continuation, Continuation::Diverged);

                    if !diverged {
                        all_diverged = false;
                        moved_join = Some(match moved_join.take() {
                            None => self.moved.clone(),
                            Some(acc) => acc.intersect(&self.moved),
                        });
                    }

                    arm_results.push((exit_block, body_result, diverged));
                }

                self.moved = moved_join.unwrap_or(moved_before);

                // If all arms diverge, mark join block unreachable
                if all_diverged {
                    self.cfg.set_terminator(join_block, Terminator::Unreachable);
                    return ExprResult {
                        value: None,
                        continuation: Continuation::Diverged,
                    };
                }

                // Add block parameter for result (if we have a value type)
                let result_param = if result_type != Type::UNIT && result_type != Type::NEVER {
                    Some(self.cfg.add_block_param(join_block, result_type))
                } else {
                    None
                };

                // Wire up non-divergent arms to join
                for (exit_block, body_result, diverged) in arm_results {
                    if !diverged {
                        self.goto_join(exit_block, join_block, result_param, body_result.value);
                    }
                }

                self.current_block = join_block;

                if let Some(param) = result_param {
                    self.cache(air_ref, param);
                }

                ExprResult {
                    value: result_param,
                    continuation: Continuation::Continues,
                }
            }

            AirInstData::Break => {
                // Emit drops for slots in scopes created inside the loop
                let loop_ctx = self.loop_stack.last().expect("break outside loop");
                let target_depth = loop_ctx.scope_depth;
                let exit_block = loop_ctx.exit;
                self.emit_drops_for_loop_exit(target_depth, span);

                self.goto_no_args(self.current_block, exit_block);

                ExprResult {
                    value: None,
                    continuation: Continuation::Diverged,
                }
            }

            AirInstData::Continue => {
                // Emit drops for slots in scopes created inside the loop
                let loop_ctx = self.loop_stack.last().expect("continue outside loop");
                let target_depth = loop_ctx.scope_depth;
                let header_block = loop_ctx.header;
                self.emit_drops_for_loop_exit(target_depth, span);

                self.goto_no_args(self.current_block, header_block);

                ExprResult {
                    value: None,
                    continuation: Continuation::Diverged,
                }
            }

            AirInstData::Ret(value) => {
                let val = match value {
                    Some(v) => {
                        let result = self.lower_inst(*v);
                        if matches!(result.continuation, Continuation::Diverged) {
                            // The return value expression itself diverged (e.g., a block
                            // containing an earlier return). The terminator was already set
                            // by the inner diverging expression, so just propagate divergence.
                            return Self::diverged();
                        }
                        // result.value may be None for Unit-typed expressions - that's OK
                        result.value
                    }
                    None => None,
                };

                // Unit has no runtime value. Keeping a path-local synthetic
                // unit value on Return makes that operand appear to cross a
                // join without a block parameter and violates SSA dominance.
                let val = if self.cfg.return_type() == Type::UNIT {
                    None
                } else {
                    val
                };

                // Emit drops for all live slots before returning
                self.emit_drops_for_all_scopes(span);

                self.cfg
                    .set_terminator(self.current_block, Terminator::Return { value: val });

                ExprResult {
                    value: None,
                    continuation: Continuation::Diverged,
                }
            }

            AirInstData::ArrayInit { elements } => {
                let mut element_vals = Vec::new();
                for elem in self.air.get_array_elements(elements) {
                    let Some(val) = self.lower_value(elem) else {
                        return Self::diverged();
                    };
                    element_vals.push(val);
                }
                // Store elements in extra array
                let elements_result = self.cfg.push_array_elements(element_vals);
                let elements = self.payload_or(elements_result, CfgArrayElements::EMPTY, span);
                let value = self.emit(CfgInstData::ArrayInit { elements }, ty, span);
                self.cache(air_ref, value);
                ExprResult {
                    value: Some(value),
                    continuation: Continuation::Continues,
                }
            }

            // Projected reads and writes lower exclusively through places.
            AirInstData::PlaceRead { place } => {
                // Convert AIR place to CFG place
                let Some(cfg_place) = self.lower_air_place(*place) else {
                    return Self::diverged();
                };
                let value = self.emit(CfgInstData::PlaceRead { place: cfg_place }, ty, span);
                self.cache(air_ref, value);
                ExprResult {
                    value: Some(value),
                    continuation: Continuation::Continues,
                }
            }

            AirInstData::PlaceWrite { place, value } => {
                // Lower the value first
                let Some(val) = self.lower_value(*value) else {
                    return Self::diverged();
                };
                // Convert AIR place to CFG place
                let Some(cfg_place) = self.lower_air_place(*place) else {
                    return Self::diverged();
                };
                // The destination's old value, if still owned, is dropped
                // before being overwritten (RUE-64): evaluate RHS, drop old,
                // store new. A whole-variable write reuses the slot-overwrite
                // drop (partial-move aware); a projected write (one field or
                // array element) reads the old value through the place. A
                // top-level field whose contents were statically moved out
                // (RUE-62) holds nothing to drop.
                let air_place = self.air.get_place(*place);
                let val_ty = self.air.get(*value).ty;
                let base_key = match air_place.base {
                    AirPlaceBase::Local(slot) => MovedSlot::Local(slot),
                    AirPlaceBase::Param(slot) => MovedSlot::Param(slot),
                    AirPlaceBase::Accessor(_) => {
                        unreachable!("semantic analysis rejects accessor-place writes")
                    }
                };
                if air_place.projection_count() == 0 {
                    self.emit_overwrite_drop(base_key, val_ty, span);
                } else {
                    // A single top-level field OR constant-index element write:
                    // skip the old-value drop when that path was definitely
                    // moved out, and guard it with the path's runtime drop flag
                    // when the move was path-dependent (RUE-156 x RUE-64
                    // interaction). Constant array elements share the field-path
                    // representation (index K -> segment K, RUE-186), so
                    // `arr[0] = arr[0]` must not drop element 0 mid-write after
                    // the RHS moved it out (RUE-228).
                    let mut field_flag: Option<u32> = None;
                    let single_path: Option<FieldPath> =
                        match self.air.get_place_projections(air_place) {
                            [AirProjection::Field { field_index, .. }] => Some(vec![*field_index]),
                            [AirProjection::Index { index, .. }] => match self.air.get(*index).data
                            {
                                AirInstData::Const(k) => Some(vec![k as u32]),
                                // A dynamic index can't identify a single element,
                                // so there is no per-element move to skip: drop the
                                // old value as usual.
                                _ => None,
                            },
                            _ => None,
                        };
                    let field_moved = match single_path {
                        Some(path) => {
                            let path_key = (base_key, path);
                            if self.moved.is_path_moved(&path_key) {
                                true
                            } else {
                                if self.moved.is_path_maybe_moved(&path_key) {
                                    field_flag = self.field_drop_flags.get(&path_key).copied();
                                }
                                false
                            }
                        }
                        None => false,
                    };
                    self.emit_projected_overwrite_drop(
                        base_key,
                        cfg_place.duplicate_with_owner(),
                        val_ty,
                        field_moved,
                        field_flag,
                        span,
                    );
                }
                self.emit(
                    CfgInstData::PlaceWrite {
                        place: cfg_place,
                        value: val,
                    },
                    Type::UNIT,
                    span,
                );
                // A whole-variable write (no projections) re-initializes the
                // slot, so a previously moved-out value must be dropped again
                // at scope exit. Projected writes (one field/element) don't
                // restore a fully moved-out variable — but a write to exactly
                // one top-level field (`o.a = ...`) does re-initialize that
                // field, so its per-field moved state is cleared (RUE-62).
                if let Some(slot) = air_place.as_local() {
                    self.moved.clear_slot(MovedSlot::Local(slot));
                    self.update_drop_flag(MovedSlot::Local(slot), true, span);
                } else if let Some(slot) = air_place.as_param() {
                    self.moved.clear_slot(MovedSlot::Param(slot));
                    self.update_drop_flag(MovedSlot::Param(slot), true, span);
                } else if air_place.projection_count() == 1 {
                    match self.air.get_place_projections(air_place) {
                        [AirProjection::Field { field_index, .. }] => {
                            self.moved.clear_field(base_key, *field_index);
                            // The field is re-initialized: re-arm its runtime
                            // drop flag so scope-exit (and later overwrite)
                            // drops fire again (RUE-156).
                            self.update_field_drop_flag(base_key, &[*field_index], true, span);
                        }
                        // A constant-index element write re-initializes that
                        // element: clear its moved state and re-arm its drop
                        // flag so it is dropped once at scope exit (RUE-228).
                        [AirProjection::Index { index, .. }] => {
                            if let AirInstData::Const(k) = self.air.get(*index).data {
                                let seg = k as u32;
                                self.moved.clear_field(base_key, seg);
                                self.update_field_drop_flag(base_key, &[seg], true, span);
                            }
                        }
                        _ => {}
                    }
                }
                ExprResult {
                    value: None,
                    continuation: Continuation::Continues,
                }
            }

            AirInstData::EnumVariant {
                enum_id,
                variant_index,
                payload,
            } => {
                // Lower payload operands (RUE-221) in order, then store them in
                // the Cfg's extra array. A discriminant-only variant has no
                // payload and lowers to just its discriminant value.
                let payload_refs = self.air.get_enum_payload(payload);
                let mut payload_vals: Vec<CfgValue> = Vec::with_capacity(payload.len());
                for pref in payload_refs {
                    let Some(v) = self.lower_value(pref) else {
                        return Self::diverged();
                    };
                    payload_vals.push(v);
                }
                let payload_result = self.cfg.push_enum_payload(payload_vals);
                let payload = self.payload_or(payload_result, CfgEnumPayload::EMPTY, span);
                let value = self.emit(
                    CfgInstData::EnumVariant {
                        enum_id: *enum_id,
                        variant_index: *variant_index,
                        payload,
                    },
                    ty,
                    span,
                );
                self.cache(air_ref, value);
                ExprResult {
                    value: Some(value),
                    continuation: Continuation::Continues,
                }
            }

            AirInstData::EnumPayloadGet {
                base,
                enum_id,
                variant_index,
                field_index,
            } => {
                let Some(base_val) = self.lower_value(*base) else {
                    return Self::diverged();
                };
                let value = self.emit(
                    CfgInstData::EnumPayloadGet {
                        base: base_val,
                        enum_id: *enum_id,
                        variant_index: *variant_index,
                        field_index: *field_index,
                    },
                    ty,
                    span,
                );
                self.cache(air_ref, value);
                ExprResult {
                    value: Some(value),
                    continuation: Continuation::Continues,
                }
            }

            AirInstData::IntCast { value, from_ty } => {
                // Lower the value to cast
                let Some(val) = self.lower_value(*value) else {
                    return Self::diverged();
                };

                // Emit the IntCast instruction
                let result = self.emit(
                    CfgInstData::IntCast {
                        value: val,
                        from_ty: *from_ty,
                    },
                    ty,
                    span,
                );
                self.cache(air_ref, result);
                ExprResult {
                    value: Some(result),
                    continuation: Continuation::Continues,
                }
            }

            AirInstData::Drop { value } => {
                // Lower the value to drop
                let Some(val) = self.lower_value(*value) else {
                    return Self::diverged();
                };
                let val_ty = self.air.get(*value).ty;

                // Emit Drop only when recursive type metadata says cleanup is
                // required; trivially droppable values need no instruction.
                if self.type_needs_drop(val_ty) {
                    self.emit(CfgInstData::Drop { value: val }, Type::UNIT, span);
                }

                // Drop is a statement, produces no value
                ExprResult {
                    value: None,
                    continuation: Continuation::Continues,
                }
            }

            AirInstData::StorageLive { slot } => {
                // Emit StorageLive to CFG
                self.emit(
                    CfgInstData::StorageLive {
                        slot: *slot,
                        local_ty: ty,
                    },
                    Type::UNIT,
                    span,
                );

                // Fresh storage holds a fresh (not-moved-out) value.
                // Slots are not currently reused, so this is a no-op today,
                // but it keeps the moved-slot state correct if reuse lands.
                self.moved.clear_slot(MovedSlot::Local(*slot));

                // Record the storage lifetime immediately, but defer ownership
                // until Alloc successfully completes. Diverging initializers
                // still end their storage without attempting to drop its
                // uninitialized contents.
                if let Some(scope) = self.scope_stack.last_mut() {
                    scope.push(LiveSlot {
                        slot: *slot,
                        ty,
                        span,
                        initialized: false,
                    });
                }

                ExprResult {
                    value: None,
                    continuation: Continuation::Continues,
                }
            }

            AirInstData::StorageDead { slot } => {
                // StorageDead in AIR is a hint; CFG builder emits these at scope exit
                // This case handles explicit StorageDead if any (currently unused)
                self.emit(
                    CfgInstData::StorageDead {
                        slot: *slot,
                        local_ty: ty,
                    },
                    Type::UNIT,
                    span,
                );
                ExprResult {
                    value: None,
                    continuation: Continuation::Continues,
                }
            }

            AirInstData::MarkMoved {
                value,
                slot,
                is_param,
                place,
            } => {
                // Pure passthrough at runtime: lower the wrapped use and
                // record that the slot's contents (or one field path or
                // array element of them, RUE-62/RUE-157/RUE-186) were
                // moved out on this path, so
                // drop elaboration skips the slot (or just that path) at
                // scope exit.
                let result = self.lower_inst(*value);
                if !matches!(result.continuation, Continuation::Diverged) {
                    let key = if *is_param {
                        MovedSlot::Param(*slot)
                    } else {
                        MovedSlot::Local(*slot)
                    };
                    match place {
                        Some(place_ref) => {
                            let path = self.moved_field_path(*place_ref);
                            // Clear the path's runtime drop flag on this
                            // path (RUE-156); if another path doesn't move
                            // the field, its flag stays 1 and the guarded
                            // per-field exit drop still runs.
                            self.update_field_drop_flag(key, &path, false, span);
                            self.moved.mark_path(key, path);
                        }
                        None => {
                            self.moved.mark_slot(key);
                            // Clear the runtime drop flag on this path; if
                            // another path doesn't move the value, its flag
                            // stays 1 and the guarded exit drop still runs.
                            self.update_drop_flag(key, false, span);
                        }
                    }
                }
                if let Some(val) = result.value {
                    self.cache(air_ref, val);
                }
                result
            }
        }
    }

    /// Emit an instruction in the current block.
    fn emit(&mut self, data: CfgInstData, ty: Type, span: rue_span::Span) -> CfgValue {
        if let CfgInstData::Drop { value } = &data {
            self.record_implicit_destructors(self.cfg.get_inst(*value).ty);
        }
        self.cfg
            .add_inst_to_block(self.current_block, CfgInst { data, ty, span })
    }

    fn record_implicit_destructors(&mut self, ty: Type) {
        let mut pending = vec![ty];
        while let Some(ty) = pending.pop() {
            if !self.implicit_destructor_types.insert(ty) {
                continue;
            }
            match ty.kind() {
                TypeKind::Struct(struct_id) => {
                    let def = self.type_pool.struct_def(struct_id);
                    if def.destructor.is_some() && !def.is_builtin {
                        // Membership, not the generated-name prefix: a source
                        // struct may legally be called `__anon_struct_N`, and
                        // its destructor is an ordinary named one (RUE-1050).
                        if self.type_pool.is_anonymous_struct(struct_id) {
                            self.anonymous_destructor_dependency_incomplete = true;
                        } else {
                            self.implicit_named_destructors.insert(struct_id);
                        }
                    }
                    pending.extend(def.fields.iter().map(|field| field.ty));
                }
                TypeKind::Enum(enum_id) => {
                    let def = self.type_pool.enum_def(enum_id);
                    pending.extend(def.variant_payloads.iter().flatten().copied());
                }
                TypeKind::Array(array_id) => {
                    let (element, len) = self.type_pool.array_def(array_id);
                    if len != 0 {
                        pending.push(element);
                    }
                }
                _ => {}
            }
        }
    }

    /// Lower a boolean short-circuit operation.
    ///
    /// `is_and` selects `&&` semantics (`lhs == false` short-circuits to
    /// false). Otherwise this lowers `||` (`lhs == true` short-circuits to
    /// true). In both cases the RHS runs on only one path, so move state from
    /// that path is restored before continuing at the join block.
    fn lower_short_circuit(
        &mut self,
        air_ref: AirRef,
        lhs: AirRef,
        rhs: AirRef,
        is_and: bool,
        span: rue_span::Span,
    ) -> ExprResult {
        let Some(lhs_val) = self.lower_value(lhs) else {
            return Self::diverged();
        };

        let rhs_block = self.cfg.new_block();
        let join_block = self.cfg.new_block();
        let result_param = self.cfg.add_block_param(join_block, Type::BOOL);
        let short_circuit_value = self.emit(CfgInstData::BoolConst(!is_and), Type::BOOL, span);

        let (then_block, then_values, else_block, else_values) = if is_and {
            (rhs_block, vec![], join_block, vec![short_circuit_value])
        } else {
            (join_block, vec![short_circuit_value], rhs_block, vec![])
        };
        let then_result = self.cfg.push_then_args(then_values);
        let then_args = self.payload_or(then_result, CfgThenArgs::EMPTY, span);
        let else_result = self.cfg.push_else_args(else_values);
        let else_args = self.payload_or(else_result, CfgElseArgs::EMPTY, span);

        self.cfg.set_terminator(
            self.current_block,
            Terminator::Branch {
                cond: lhs_val,
                then_block,
                then_args,
                else_block,
                else_args,
            },
        );

        // If the RHS diverges, its block already has a diverging terminator and
        // contributes no edge to the join. The join is still reachable via the
        // short-circuit edge, so continue there rather than propagating divergence.
        let moved_before_rhs = self.moved.clone();
        self.current_block = rhs_block;
        if let Some(rhs_val) = self.lower_value(rhs) {
            let args_result = self.cfg.push_goto_args(std::iter::once(rhs_val));
            let args = self.payload_or(args_result, CfgGotoArgs::EMPTY, span);
            self.cfg.set_terminator(
                self.current_block,
                Terminator::Goto {
                    target: join_block,
                    args,
                },
            );
        }

        self.moved = moved_before_rhs;
        self.current_block = join_block;
        self.cache(air_ref, result_param);
        ExprResult {
            value: Some(result_param),
            continuation: Continuation::Continues,
        }
    }

    /// Terminate `exit_block` with a `Goto` to `target` and no block args.
    fn goto_no_args(&mut self, exit_block: BlockId, target: BlockId) {
        let args_result = self.cfg.push_goto_args(std::iter::empty::<CfgValue>());
        let args = self.payload_or(args_result, CfgGotoArgs::EMPTY, rue_span::Span::default());
        self.cfg
            .set_terminator(exit_block, Terminator::Goto { target, args });
    }

    /// Terminate `exit_block` with a `Goto` into `join_block`, passing the
    /// branch's value as the single block arg IFF the join has a result
    /// param. This is the ONE place the value-branch/join-param arity
    /// contract is encoded — if/else arms and match arms all wire through
    /// here, so the contract cannot drift between constructs (the RUE-347
    /// bug class lives exactly on this seam).
    fn goto_join(
        &mut self,
        exit_block: BlockId,
        join_block: BlockId,
        result_param: Option<CfgValue>,
        value: Option<CfgValue>,
    ) {
        let args: Vec<CfgValue> = match (value, result_param) {
            (Some(val), Some(_)) => vec![val],
            _ => vec![],
        };
        let args_result = self.cfg.push_goto_args(args);
        let args = self.payload_or(args_result, CfgGotoArgs::EMPTY, rue_span::Span::default());
        self.cfg.set_terminator(
            exit_block,
            Terminator::Goto {
                target: join_block,
                args,
            },
        );
    }

    /// Cache a value for an AIR ref.
    fn cache(&mut self, air_ref: AirRef, value: CfgValue) {
        self.value_cache[air_ref.as_u32() as usize] = Some(value);
    }

    /// Lower an instruction used in **value position** and return its value, or
    /// `None` **only** if it diverged (return/break/continue).
    ///
    /// Callers use the `let Some(v) = self.lower_value(x) else { return
    /// Self::diverged(); }` idiom, so `None` here means "control did not reach
    /// this point" and the caller propagates the divergence (which terminates
    /// the block). That idiom is only sound if `None` ⇔ divergence.
    ///
    /// The trap it guards against (RUE-227, the "block has no terminator" ICE
    /// class): some AIR forms legitimately produce **no** CFG value while
    /// control *continues* — statement-only, Unit-typed forms such as `Store`,
    /// `ParamStore`, `Alloc`, and `StorageLive`/`StorageDead`. If such a form is
    /// used in value position (e.g. `let x = (s.push(b));` where the mutation is
    /// Unit — RUE-224), returning its raw `None` would make the caller wrongly
    /// believe control diverged and bail out mid-block, leaving the block
    /// unterminated → SIGABRT in codegen. Instead, when the expression
    /// *continues* but yielded no value, we materialize a real Unit value here.
    /// A well-typed program only reaches this branch for a genuinely Unit-typed
    /// expression (sema rejects a non-Unit value being consumed where none is
    /// produced), so a Unit `Const` is the correct, ABI-neutral filler.
    fn lower_value(&mut self, air_ref: AirRef) -> Option<CfgValue> {
        let result = self.lower_inst(air_ref);
        match result.continuation {
            Continuation::Diverged => None,
            Continuation::Continues => Some(result.value.unwrap_or_else(|| {
                let inst = self.air.get(air_ref);
                let (ty, span) = (inst.ty, inst.span);
                self.emit(CfgInstData::Const(0), ty, span)
            })),
        }
    }

    /// Create a diverged ExprResult. Used when an operand diverges.
    fn diverged() -> ExprResult {
        ExprResult {
            value: None,
            continuation: Continuation::Diverged,
        }
    }

    /// Check whether dropping a type requires cleanup.
    ///
    /// This method has access to struct and array definitions, allowing it to
    /// recursively check if struct fields or array elements need drop.
    ///
    /// A type needs drop if dropping it requires cleanup actions:
    /// - Primitives, bool, unit, never, error: trivially droppable (no)
    /// - Enum: needs drop if any variant payload needs drop
    /// - Struct: needs drop when it declares a destructor or any field needs drop
    /// - Array: needs drop if element type needs drop
    fn type_needs_drop(&self, ty: Type) -> bool {
        self.type_pool.type_needs_drop(ty)
    }

    /// Convert AIR argument mode to CFG argument mode.
    fn convert_arg_mode(mode: AirArgMode) -> CfgArgMode {
        match mode {
            AirArgMode::Normal => CfgArgMode::Normal,
            AirArgMode::Inout => CfgArgMode::Inout,
            AirArgMode::Borrow => CfgArgMode::Borrow,
        }
    }

    /// Emit drops for all live slots in all scopes (for return).
    /// Drops are emitted in reverse order (LIFO) across all scopes.
    /// Owned (pass-by-value) parameters are dropped after all locals, in
    /// reverse declaration order — the callee owns its by-value arguments.
    /// Set a slot's runtime drop flag (RUE-108), allocating the hidden flag
    /// slot on first use. `live = true` (1) when the slot owns its value,
    /// `false` (0) after a move. Only slots in `ever_moved` get flags, and
    /// only when their type actually needs dropping — callers pass the type
    /// when known; `set_drop_flag` is the allocate-or-update primitive.
    fn set_drop_flag(&mut self, key: MovedSlot, live: bool, span: rue_span::Span) {
        let flag_slot = match self.drop_flags.get(&key) {
            Some(&f) => f,
            None => {
                let f = self.cfg.alloc_temp_local();
                self.drop_flags.insert(key, f);
                f
            }
        };
        let val = self.emit(
            CfgInstData::Const(if live { 1 } else { 0 }),
            Type::I32,
            span,
        );
        self.emit(
            CfgInstData::Store {
                slot: flag_slot,
                value: val,
            },
            Type::UNIT,
            span,
        );
    }

    /// Update an EXISTING drop flag; no-op when the slot has none (its type
    /// needs no drop, or it is never moved). Arming (`live = true`) marks a
    /// whole-slot (re)initialization, which re-initializes every field too,
    /// so the slot's per-field drop flags (RUE-156) are re-armed as well.
    /// Clearing (`live = false`) marks a whole-slot move-out and leaves the
    /// per-field flags alone: the per-field exit drops only run inside the
    /// whole-slot flag's guard, which the cleared flag already skips.
    fn update_drop_flag(&mut self, key: MovedSlot, live: bool, span: rue_span::Span) {
        if self.drop_flags.contains_key(&key) {
            self.set_drop_flag(key, live, span);
        }
        if live {
            self.arm_field_drop_flags(key, span);
        }
    }

    /// Arm the drop flag at a value's (re)initialization site, when the slot
    /// is moved somewhere in this function and its type needs dropping. The
    /// slot's per-field drop flags (RUE-156) are armed too: a fresh whole
    /// value owns all of its fields.
    fn arm_drop_flag_if_needed(&mut self, key: MovedSlot, ty: Type, span: rue_span::Span) {
        if self.ever_moved.contains(&key) && self.type_needs_drop(ty) {
            self.set_drop_flag(key, true, span);
        }
        self.arm_field_drop_flags(key, span);
    }

    /// Arm (allocating on first use) the per-field drop flags for every
    /// field path of `key` that is moved somewhere in this function
    /// (RUE-156). Called at the slot's (re)initialization sites.
    fn arm_field_drop_flags(&mut self, key: MovedSlot, span: rue_span::Span) {
        let mut paths: Vec<FieldPath> = self
            .ever_field_moved
            .iter()
            .filter(|(s, _)| *s == key)
            .map(|(_, p)| p.clone())
            .collect();
        // `ever_field_moved` is a `HashSet`, so its iteration order varies per
        // process. `set_field_drop_flag` allocates a hidden local the first time
        // it sees a path, which would make the flag slots — and therefore every
        // frame offset after them — depend on hash order. Sorting pins the
        // allocation order to the field path itself. Emitted output must be a
        // function of the source, and several parity gates compare artifacts
        // byte-for-byte.
        paths.sort();
        for path in paths {
            self.set_field_drop_flag(key, path, true, span);
        }
    }

    /// Set a field path's runtime drop flag (RUE-156), allocating the hidden
    /// flag slot on first use. The per-field analogue of `set_drop_flag`.
    fn set_field_drop_flag(
        &mut self,
        key: MovedSlot,
        path: FieldPath,
        live: bool,
        span: rue_span::Span,
    ) {
        let flag_slot = match self.field_drop_flags.get(&(key, path.clone())) {
            Some(&f) => f,
            None => {
                let f = self.cfg.alloc_temp_local();
                self.field_drop_flags.insert((key, path), f);
                f
            }
        };
        let val = self.emit(
            CfgInstData::Const(if live { 1 } else { 0 }),
            Type::I32,
            span,
        );
        self.emit(
            CfgInstData::Store {
                slot: flag_slot,
                value: val,
            },
            Type::UNIT,
            span,
        );
    }

    /// Update an EXISTING per-field drop flag; no-op when the path has none
    /// (its type needs no drop). The flag is allocated and armed at the
    /// slot's initialization site, which always precedes the move.
    fn update_field_drop_flag(
        &mut self,
        key: MovedSlot,
        path: &[u32],
        live: bool,
        span: rue_span::Span,
    ) {
        if self.field_drop_flags.contains_key(&(key, path.to_vec())) {
            self.set_field_drop_flag(key, path.to_vec(), live, span);
        }
    }

    /// The path (declaration/element indices, outermost first) named by a
    /// path-level MarkMoved's place. Sema only exports markers for pure
    /// `Field` projection chains and for single CONSTANT-index projections
    /// (per-element array moves, RUE-186): the marker's index operand is a
    /// dedicated `Const` instruction, resolved back to the element index
    /// here.
    fn moved_field_path(&self, place_ref: AirPlaceRef) -> FieldPath {
        let place = self.air.get_place(place_ref);
        self.air
            .get_place_projections(place)
            .iter()
            .map(|proj| match proj {
                AirProjection::Field { field_index, .. } => *field_index,
                AirProjection::Index { index, .. } => match self.air.get(*index).data {
                    AirInstData::Const(k) => k as u32,
                    _ => unreachable!("MarkMoved place contains a non-constant index projection"),
                },
            })
            .collect()
    }

    fn emit_drops_for_all_scopes(&mut self, span: rue_span::Span) {
        // Collect all live slots in reverse order across all scopes
        let all_slots: Vec<LiveSlot> = self
            .scope_stack
            .iter()
            .rev()
            .flat_map(|scope| scope.iter().rev().cloned())
            .collect();

        for live_slot in all_slots {
            self.emit_drop_for_slot(&live_slot, span);
        }

        // Drop owned parameters (unless their value was moved out on every
        // path reaching this exit). The list is empty for destructors and
        // drop-glue functions, which must not re-drop their own parameter.
        for &(abi_slot, ty) in self.air.param_drops().to_vec().iter().rev() {
            let key = MovedSlot::Param(abi_slot);
            if self.moved.is_slot_moved(key) {
                continue;
            }
            if self.type_needs_drop(ty) {
                self.emit_guarded(key, span, |b| {
                    if !b.emit_partial_drop(key, ty, span) {
                        let param_val = b.emit(CfgInstData::Param { index: abi_slot }, ty, span);
                        b.emit(CfgInstData::Drop { value: param_val }, Type::UNIT, span);
                    }
                });
            }
        }
    }

    /// Emit drops for slots in scopes created inside the current loop (for break/continue).
    /// Only drops slots from the current scope depth down to (but not including) `target_depth`.
    /// This ensures that slots declared outside the loop are NOT dropped.
    fn emit_drops_for_loop_exit(&mut self, target_depth: usize, span: rue_span::Span) {
        // Collect slots from scopes created inside the loop (depth >= target_depth)
        // in reverse order (LIFO)
        let loop_slots: Vec<LiveSlot> = self
            .scope_stack
            .iter()
            .skip(target_depth)
            .rev()
            .flat_map(|scope| scope.iter().rev().cloned())
            .collect();

        for live_slot in loop_slots {
            self.emit_drop_for_slot(&live_slot, span);
        }
    }

    /// Emit the drop body produced by `emit_body` behind an `if flag != 0`
    /// guard when `key` has a runtime drop flag and its move-out is not
    /// already statically decided (RUE-108). Returns true if a guard was
    /// emitted (the body ran in its own block).
    fn emit_guarded(
        &mut self,
        key: MovedSlot,
        span: rue_span::Span,
        emit_body: impl FnOnce(&mut Self),
    ) {
        let Some(&flag_slot) = self.drop_flags.get(&key) else {
            emit_body(self);
            return;
        };
        // Statically moved-on-all-paths is filtered by the callers; reaching
        // here with a flag means the move is path-dependent (or downstream of
        // a conservative join), so test the flag at runtime.
        let cont_block = self.begin_flag_guard(flag_slot, span);
        emit_body(self);
        self.end_flag_guard(cont_block);
    }

    /// Open an `if flag != 0` diamond: emits the flag test and branch,
    /// leaves the current block positioned at the guarded body block, and
    /// returns the continuation block to pass to [`Self::end_flag_guard`].
    fn begin_flag_guard(&mut self, flag_slot: u32, span: rue_span::Span) -> BlockId {
        let flag_val = self.emit(CfgInstData::Load { slot: flag_slot }, Type::I32, span);
        let zero = self.emit(CfgInstData::Const(0), Type::I32, span);
        let cond = self.emit(CfgInstData::Ne(flag_val, zero), Type::BOOL, span);

        let drop_block = self.cfg.new_block();
        let cont_block = self.cfg.new_block();
        let then_result = self.cfg.push_then_args(std::iter::empty());
        let then_args = self.payload_or(then_result, CfgThenArgs::EMPTY, span);
        let else_result = self.cfg.push_else_args(std::iter::empty());
        let else_args = self.payload_or(else_result, CfgElseArgs::EMPTY, span);
        self.cfg.set_terminator(
            self.current_block,
            Terminator::Branch {
                cond,
                then_block: drop_block,
                then_args,
                else_block: cont_block,
                else_args,
            },
        );
        self.current_block = drop_block;
        cont_block
    }

    /// Close a diamond opened by [`Self::begin_flag_guard`]: terminate the
    /// guarded body block with a jump to the continuation block and continue
    /// building there.
    fn end_flag_guard(&mut self, cont_block: BlockId) {
        self.goto_no_args(self.current_block, cont_block);
        self.current_block = cont_block;
    }

    /// Emit Drop and StorageDead for a single slot.
    /// The Drop is suppressed when the slot's value was moved out on every
    /// path reaching this point (the new owner drops it); the StorageDead
    /// is still emitted to end the slot's storage lifetime. A struct slot
    /// with moved-out FIELDS is dropped field-granularly instead (RUE-62).
    fn emit_drop_for_slot(&mut self, live_slot: &LiveSlot, span: rue_span::Span) {
        // A `for`-loop element binder over a non-Copy collection is a
        // non-owning borrow (spec 4.8:26): it aliases an element the collection
        // still owns and drops, so dropping it here would double-free
        // (RUE-259). Its storage still ends normally (StorageDead below).
        let key = MovedSlot::Local(live_slot.slot);
        if live_slot.initialized
            && !self.air.is_borrow_slot(live_slot.slot)
            && !self.moved.is_slot_moved(key)
            && self.type_needs_drop(live_slot.ty)
        {
            let (slot, ty) = (live_slot.slot, live_slot.ty);
            self.emit_guarded(key, span, |b| {
                // Fast path: nothing partially moved — drop the whole value.
                if !b.emit_partial_drop(key, ty, span) {
                    let slot_val = b.emit(CfgInstData::Load { slot }, ty, span);
                    b.emit(CfgInstData::Drop { value: slot_val }, Type::UNIT, span);
                }
            });
        }
        self.emit(
            CfgInstData::StorageDead {
                slot: live_slot.slot,
                local_ty: live_slot.ty,
            },
            Type::UNIT,
            span,
        );
    }

    /// Drop the live value about to be overwritten by a whole-slot
    /// assignment (RUE-64). Assignment semantics follow Rust: the RHS is
    /// fully evaluated first, then the destination's old value is dropped,
    /// then the new value is stored — so callers invoke this after lowering
    /// the RHS and before emitting the store.
    ///
    /// The drop is skipped when the old value was moved out on every path
    /// (`d = f(d)` — the RHS itself consumed it); path-dependent moves are
    /// handled at runtime by the drop-flag guard. A slot with moved-out
    /// FIELDS is dropped field-granularly (RUE-62), like at scope exit.
    fn emit_overwrite_drop(&mut self, key: MovedSlot, ty: Type, span: rue_span::Span) {
        if self.moved.is_slot_moved(key) || !self.type_needs_drop(ty) {
            return;
        }
        self.emit_guarded(key, span, |b| {
            if !b.emit_partial_drop(key, ty, span) {
                let old_val = match key {
                    MovedSlot::Local(slot) => b.emit(CfgInstData::Load { slot }, ty, span),
                    MovedSlot::Param(index) => b.emit(CfgInstData::Param { index }, ty, span),
                };
                b.emit(CfgInstData::Drop { value: old_val }, Type::UNIT, span);
            }
        });
    }

    /// Drop the live value about to be overwritten through a PROJECTED place
    /// (field or array-element assignment, RUE-64). Same ordering contract
    /// as [`emit_overwrite_drop`]. `base_key` is the place's base slot: a
    /// statically moved-out base means the old projected value is gone, and
    /// a path-dependent whole-base move is guarded by the base's drop flag.
    /// `field_moved` lets the caller suppress the drop when this exact
    /// top-level field was statically moved out (RUE-62).
    fn emit_projected_overwrite_drop(
        &mut self,
        base_key: MovedSlot,
        place: Place,
        ty: Type,
        field_moved: bool,
        field_flag: Option<u32>,
        span: rue_span::Span,
    ) {
        if self.moved.is_slot_moved(base_key) || field_moved || !self.type_needs_drop(ty) {
            return;
        }
        self.emit_guarded(base_key, span, |b| {
            if let Some(flag) = field_flag {
                // The exact field was moved on some paths only: its per-path
                // runtime flag (RUE-156) says whether the old value is live.
                let cont = b.begin_flag_guard(flag, span);
                let old_val = b.emit(CfgInstData::PlaceRead { place }, ty, span);
                b.emit(CfgInstData::Drop { value: old_val }, Type::UNIT, span);
                b.end_flag_guard(cont);
            } else {
                let old_val = b.emit(CfgInstData::PlaceRead { place }, ty, span);
                b.emit(CfgInstData::Drop { value: old_val }, Type::UNIT, span);
            }
        });
    }

    /// Path-granular drop of a partially-moved struct or array (RUE-62,
    /// RUE-156, RUE-157, RUE-186).
    ///
    /// When one or more field paths of a struct slot were (or may have
    /// been) moved out (`eat(o.a)`, `eat(o.a.b)`, `if c { eat(o.a) }`), the
    /// whole-value `Drop` (destructor + drop glue over ALL fields) would
    /// re-drop the moved part. Instead this emits, recursively per struct
    /// level:
    ///
    /// 1. a plain call to the struct's own destructor (if any) — the
    ///    destructor is an ordinary `{Type}.__drop` function taking `self`
    ///    by value, and the CFG `Call` machinery already passes struct
    ///    arguments flattened, so no dedicated codegen op is needed; the
    ///    drop GLUE (which would re-walk every field) is deliberately NOT
    ///    called;
    /// 2. per droppable field, in declaration order (matching glue order):
    ///    - moved out on EVERY path: nothing (the new owner drops it);
    ///    - moved out on SOME path (RUE-156): its drop behind the field
    ///      path's runtime drop-flag guard;
    ///    - holding a deeper moved path (RUE-157): a recursive
    ///      field-granular drop of the field instead of a whole `Drop`;
    ///    - untouched: a plain `Drop` of the field read via `PlaceRead`.
    ///
    /// Arrays get the same treatment per ELEMENT (RUE-186): elements moved
    /// out on every path are skipped, elements moved on some path are
    /// dropped behind their runtime drop flag, untouched elements get a
    /// plain drop. Arrays have no destructor of their own, so there is no
    /// step-1 destructor call at array levels.
    ///
    /// Returns `false` (caller emits the normal whole-value drop) when the
    /// slot has no (possibly-)moved-out paths or is neither a struct nor an
    /// array. Note each destructor still receives the WHOLE struct,
    /// including the moved-out part's stale slots — reading a moved field
    /// in a destructor is a use-after-move just like any other.
    fn emit_partial_drop(&mut self, key: MovedSlot, ty: Type, span: rue_span::Span) -> bool {
        // `maybe` ⊇ `definite`: paths possibly vs. definitely moved out on
        // the paths reaching this exit.
        let definite = self.moved.moved_paths_of(key);
        let maybe = self.moved.maybe_moved_paths_of(key);
        if maybe.is_empty() && definite.is_empty() {
            return false;
        }
        match ty.kind() {
            TypeKind::Struct(struct_id) => {
                self.emit_partial_struct_drop_level(
                    key,
                    ty,
                    struct_id,
                    ty,
                    &mut Vec::new(),
                    &mut Vec::new(),
                    &definite,
                    &maybe,
                    span,
                );
                true
            }
            TypeKind::Array(_) => {
                self.emit_partial_array_drop_level(
                    key,
                    ty,
                    ty,
                    &mut Vec::new(),
                    &mut Vec::new(),
                    &definite,
                    &maybe,
                    span,
                );
                true
            }
            _ => {
                // Path-level move markers are only emitted for struct fields
                // and array elements, so this is unreachable today; fall
                // back to the whole-value drop.
                false
            }
        }
    }

    /// One struct level of [`Self::emit_partial_drop`]: destructor
    /// (without glue) plus per-field drops for the struct at field path
    /// `path` (projections `projs`) inside slot `key`.
    #[allow(clippy::too_many_arguments)]
    fn emit_partial_struct_drop_level(
        &mut self,
        key: MovedSlot,
        root_type: Type,
        struct_id: StructId,
        ty: Type,
        path: &mut FieldPath,
        projs: &mut Vec<Projection>,
        definite: &std::collections::HashSet<FieldPath>,
        maybe: &std::collections::HashSet<FieldPath>,
        span: rue_span::Span,
    ) {
        let struct_def = self.type_pool.struct_def(struct_id);
        let base = match key {
            MovedSlot::Local(slot) => PlaceBase::Local(slot),
            MovedSlot::Param(slot) => PlaceBase::Param(slot),
        };

        // 1. Run this struct's own destructor (without the field glue).
        if let Some(ref destructor_name) = struct_def.destructor {
            self.record_implicit_destructors(ty);
            let whole_val = if projs.is_empty() {
                match key {
                    MovedSlot::Local(slot) => self.emit(CfgInstData::Load { slot }, ty, span),
                    MovedSlot::Param(index) => self.emit(CfgInstData::Param { index }, ty, span),
                }
            } else {
                let place_result = self.cfg.make_place(base, root_type, projs.iter().copied());
                let place = self.payload_or(
                    place_result,
                    Place {
                        base,
                        base_type: root_type,
                        projections: CfgProjections::EMPTY,
                    },
                    span,
                );
                self.emit(CfgInstData::PlaceRead { place }, ty, span)
            };
            let Some(name) = (self.source_symbol_resolver)(destructor_name) else {
                self.errors.push(CompileError::new(
                    ErrorKind::InternalError(format!(
                        "cleanup destructor '{destructor_name}' has no canonical symbol mapping"
                    )),
                    span,
                ));
                return;
            };
            let args_result = self.cfg.push_call_args(std::iter::once(CfgCallArg {
                value: whole_val,
                mode: CfgArgMode::Normal,
            }));
            let args = self.payload_or(args_result, CfgCallArgs::EMPTY, span);
            self.emit(
                CfgInstData::Call {
                    runtime: None,
                    name,
                    args,
                },
                Type::UNIT,
                span,
            );
        }

        // 2. Handle the droppable fields in declaration order.
        for (field_index, field) in struct_def.fields.iter().enumerate() {
            let field_index = field_index as u32;
            if !self.type_needs_drop(field.ty) {
                continue;
            }
            path.push(field_index);
            projs.push(Projection::Field {
                struct_id,
                field_index,
            });

            if definite.contains(path) {
                // Moved out on every path: the new owner drops it.
                path.pop();
                projs.pop();
                continue;
            }
            // A strictly deeper (possibly-)moved path means this field
            // can't take a whole `Drop`; recurse instead.
            let has_deeper_move = maybe
                .iter()
                .any(|p| p.len() > path.len() && p.starts_with(path));
            // Possibly (but not definitely) moved: guard the drop with the
            // field path's runtime drop flag (RUE-156). The flag exists
            // whenever a droppable path has a move site (armed at the
            // slot's init); a missing flag falls through to an unguarded
            // drop as a defensive default.
            let guard_flag = if maybe.contains(path) {
                self.field_drop_flags.get(&(key, path.clone())).copied()
            } else {
                None
            };

            let cont_block = guard_flag.map(|flag| self.begin_flag_guard(flag, span));
            let recursed = if has_deeper_move {
                match field.ty.kind() {
                    TypeKind::Struct(field_struct_id) => {
                        self.emit_partial_struct_drop_level(
                            key,
                            root_type,
                            field_struct_id,
                            field.ty,
                            path,
                            projs,
                            definite,
                            maybe,
                            span,
                        );
                        true
                    }
                    TypeKind::Array(_) => {
                        self.emit_partial_array_drop_level(
                            key, root_type, field.ty, path, projs, definite, maybe, span,
                        );
                        true
                    }
                    _ => {
                        // Deeper paths only exist through struct fields and
                        // array elements; keep a whole drop as a defensive
                        // fallback.
                        false
                    }
                }
            } else {
                false
            };
            if !recursed {
                let place_result = self.cfg.make_place(base, root_type, projs.iter().copied());
                let place = self.payload_or(
                    place_result,
                    Place {
                        base,
                        base_type: root_type,
                        projections: CfgProjections::EMPTY,
                    },
                    span,
                );
                let field_val = self.emit(CfgInstData::PlaceRead { place }, field.ty, span);
                self.emit(CfgInstData::Drop { value: field_val }, Type::UNIT, span);
            }
            if let Some(cont_block) = cont_block {
                self.end_flag_guard(cont_block);
            }

            path.pop();
            projs.pop();
        }
    }

    /// One array level of [`Self::emit_partial_drop`] (RUE-186): per-element
    /// drops for the array at path `path` (projections `projs`) inside slot
    /// `key`, in ascending element order (matching drop-glue order). The
    /// per-element cases mirror the struct-field cases: definitely moved →
    /// skipped, possibly moved → guarded by the element path's runtime drop
    /// flag, deeper moved path → recursive path-granular drop, untouched →
    /// plain `Drop`. Arrays have no destructor of their own.
    #[allow(clippy::too_many_arguments)]
    fn emit_partial_array_drop_level(
        &mut self,
        key: MovedSlot,
        root_type: Type,
        array_ty: Type,
        path: &mut FieldPath,
        projs: &mut Vec<Projection>,
        definite: &std::collections::HashSet<FieldPath>,
        maybe: &std::collections::HashSet<FieldPath>,
        span: rue_span::Span,
    ) {
        let TypeKind::Array(array_id) = array_ty.kind() else {
            unreachable!("emit_partial_array_drop_level called on non-array type");
        };
        let (elem_ty, len) = self.type_pool.array_def(array_id);
        if !self.type_needs_drop(elem_ty) {
            return;
        }
        let base = match key {
            MovedSlot::Local(slot) => PlaceBase::Local(slot),
            MovedSlot::Param(slot) => PlaceBase::Param(slot),
        };

        for k in 0..len {
            path.push(k as u32);
            if definite.contains(path) {
                // Moved out on every path: the new owner drops it.
                path.pop();
                continue;
            }
            let has_deeper_move = maybe
                .iter()
                .any(|p| p.len() > path.len() && p.starts_with(path));
            let guard_flag = if maybe.contains(path) {
                self.field_drop_flags.get(&(key, path.clone())).copied()
            } else {
                None
            };

            let index_val = self.emit(CfgInstData::Const(k), Type::U64, span);
            projs.push(Projection::Index {
                array_type: array_ty,
                index: index_val,
            });

            let cont_block = guard_flag.map(|flag| self.begin_flag_guard(flag, span));
            let recursed = if has_deeper_move {
                match elem_ty.kind() {
                    TypeKind::Struct(elem_struct_id) => {
                        self.emit_partial_struct_drop_level(
                            key,
                            root_type,
                            elem_struct_id,
                            elem_ty,
                            path,
                            projs,
                            definite,
                            maybe,
                            span,
                        );
                        true
                    }
                    TypeKind::Array(_) => {
                        self.emit_partial_array_drop_level(
                            key, root_type, elem_ty, path, projs, definite, maybe, span,
                        );
                        true
                    }
                    _ => false,
                }
            } else {
                false
            };
            if !recursed {
                let place_result = self.cfg.make_place(base, root_type, projs.iter().copied());
                let place = self.payload_or(
                    place_result,
                    Place {
                        base,
                        base_type: root_type,
                        projections: CfgProjections::EMPTY,
                    },
                    span,
                );
                let elem_val = self.emit(CfgInstData::PlaceRead { place }, elem_ty, span);
                self.emit(CfgInstData::Drop { value: elem_val }, Type::UNIT, span);
            }
            if let Some(cont_block) = cont_block {
                self.end_flag_guard(cont_block);
            }

            path.pop();
            projs.pop();
        }
    }

    fn emit_unreachable_warning(
        &mut self,
        unreachable_span: rue_span::Span,
        diverging_span: rue_span::Span,
    ) {
        if self.allow_unreachable_code {
            return;
        }

        self.warnings.push(
            CompileWarning::new(WarningKind::UnreachableCode, unreachable_span)
                .with_label(
                    "any code following this expression is unreachable",
                    diverging_span,
                )
                .with_note(
                    "this warning occurs because the preceding expression diverges \
                     (e.g., returns, breaks, or continues)",
                ),
        );
    }

    /// Lower an AIR place reference to a CFG Place.
    ///
    /// This converts AirPlaceRef -> AirPlace -> CFG Place, translating projections
    /// and lowering any index expressions to CFG values.
    ///
    /// This is the bridge between AIR's canonical place operations and CFG's
    /// canonical place operations.
    fn lower_air_place(&mut self, place_ref: AirPlaceRef) -> Option<Place> {
        // Copy the compact descriptor before lowering index operands mutably.
        // In particular, `base_type` is still needed after that lowering.
        let air_place = self.air.get_place(place_ref);
        let air_base = air_place.base;
        let air_base_type = air_place.base_type;
        let air_projections = self.air.get_place_projections(air_place).to_vec();

        // Convert the base
        let base = match air_base {
            AirPlaceBase::Local(slot) => PlaceBase::Local(slot),
            AirPlaceBase::Param(slot) => PlaceBase::Param(slot),
            AirPlaceBase::Accessor(call) => PlaceBase::Accessor(self.lower_value(call)?),
        };

        // Convert projections, lowering any index expressions
        let mut cfg_projections = Vec::with_capacity(air_projections.len());

        for proj in &air_projections {
            let cfg_proj = match proj {
                AirProjection::Field {
                    struct_id,
                    field_index,
                } => Projection::Field {
                    struct_id: *struct_id,
                    field_index: *field_index,
                },
                AirProjection::Index { array_type, index } => {
                    // Lower the index expression to a CFG value
                    let index_val = self.lower_value(*index)?;
                    Projection::Index {
                        array_type: *array_type,
                        index: index_val,
                    }
                }
            };
            cfg_projections.push(cfg_proj);
        }

        // Create the CFG place
        match self.cfg.make_place(base, air_base_type, cfg_projections) {
            Ok(place) => Some(place),
            Err(error) => {
                self.errors.push(CompileError::new(
                    error.error_kind("CFG place construction failed"),
                    rue_span::Span::default(),
                ));
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rue_air::{AirEditor, AirValidationContext, Sema, SemaMetadata};
    use rue_error::PreviewFeatures;
    use rue_lexer::Lexer;
    use rue_parser::Parser;
    use rue_rir::AstGen;
    use rue_span::FileId;
    use std::collections::{HashMap, HashSet};

    #[test]
    fn exact_moved_path_queries_do_not_materialize_per_slot_sets() {
        let slot = MovedSlot::Local(7);
        let definite = vec![1, 2];
        let possible = vec![3, 4];
        let absent = vec![5, 6];
        let mut state = MoveState::default();
        state.mark_path(slot, definite.clone());
        state
            .maybe_fields
            .entry(slot)
            .or_default()
            .insert(possible.clone());

        assert!(state.is_path_moved(&(slot, definite.clone())));
        assert!(state.is_path_maybe_moved(&(slot, definite)));
        assert!(!state.is_path_moved(&(slot, possible.clone())));
        assert!(state.is_path_maybe_moved(&(slot, possible)));
        assert!(!state.is_path_moved(&(slot, absent.clone())));
        assert!(!state.is_path_maybe_moved(&(slot, absent)));

        let materialized_probe = ["moved_paths_of(base_key)", ".contains"].concat();
        let materialized_maybe_probe = ["maybe_moved_paths_of(base_key)", ".contains"].concat();
        let source = include_str!("build.rs");
        assert!(!source.contains(&materialized_probe));
        assert!(!source.contains(&materialized_maybe_probe));
    }

    #[test]
    fn slot_local_move_operations_ignore_unrelated_slot_partitions() {
        let mut state = MoveState::default();
        for index in 0..1_024 {
            state.mark_path(MovedSlot::Local(index), vec![index]);
        }
        let target = MovedSlot::Local(777);

        state.stats.slot_path_visits.set(0);
        assert_eq!(state.moved_paths_of(target), HashSet::from([vec![777]]));
        assert_eq!(state.stats.slot_path_visits.get(), 1);

        state.stats.slot_path_visits.set(0);
        assert_eq!(
            state.maybe_moved_paths_of(target),
            HashSet::from([vec![777]])
        );
        assert_eq!(state.stats.slot_path_visits.get(), 1);

        state.stats.slot_path_visits.set(0);
        state.clear_field(target, 777);
        assert_eq!(state.stats.slot_path_visits.get(), 2);
        assert!(!state.fields.contains_key(&target));
        assert!(!state.maybe_fields.contains_key(&target));
        assert!(state.is_path_moved(&(MovedSlot::Local(778), vec![778])));

        state.mark_path(target, vec![1]);
        state.stats.slot_path_visits.set(0);
        state.clear_slot(target);
        assert_eq!(state.stats.slot_path_visits.get(), 2);
        assert!(!state.fields.contains_key(&target));
        assert!(!state.maybe_fields.contains_key(&target));
    }

    #[test]
    fn partitioned_move_state_join_preserves_definite_and_possible_paths() {
        let slot = MovedSlot::Param(3);
        let shared = vec![1];
        let left_only = vec![2];
        let right_only = vec![3];
        let mut left = MoveState::default();
        left.mark_path(slot, shared.clone());
        left.mark_path(slot, left_only.clone());
        let mut right = MoveState::default();
        right.mark_path(slot, shared.clone());
        right.mark_path(slot, right_only.clone());

        let joined = left.intersect(&right);
        assert_eq!(joined.moved_paths_of(slot), HashSet::from([shared.clone()]));
        assert_eq!(
            joined.maybe_moved_paths_of(slot),
            HashSet::from([shared, left_only, right_only])
        );
    }

    fn build_cfg(source: &str) -> Cfg {
        build_cfg_for(source, 0)
    }

    /// Build the CFG for the `index`-th analyzed function in `source`
    /// (functions are analyzed in declaration order).
    fn build_cfg_for(source: &str, index: usize) -> Cfg {
        build_cfg_select(source, |functions| &functions[index])
    }

    /// Build the CFG for the analyzed function called `name` in `source`
    /// (robust to analysis order, unlike `build_cfg_for`).
    fn build_cfg_named(source: &str, name: &str) -> Cfg {
        let symbol = SemaMetadata::synthetic_root_function_symbol(name);
        build_cfg_select(source, |functions| {
            functions
                .iter()
                .find(|f| f.name == symbol)
                .unwrap_or_else(|| panic!("no analyzed function named '{}'", name))
        })
    }

    /// Build the CFG for the analyzed function of `source` chosen by `select`.
    fn build_cfg_select(
        source: &str,
        select: impl for<'f> Fn(&'f [rue_air::AnalyzedFunction]) -> &'f rue_air::AnalyzedFunction,
    ) -> Cfg {
        // Drop tests use an ordinary source nominal so they do not depend on
        // the trusted standard-library StrBuf language item.
        let source = format!(
            "struct StrBuf {{ cap: u64, fn with_capacity(cap: u64) -> StrBuf {{ StrBuf {{ cap: cap }} }} fn inspect(borrow self) {{}} }}\n\
             drop fn StrBuf(self) {{}}\n{source}"
        );
        let lexer = Lexer::new(&source);
        let (tokens, interner) = lexer.tokenize().unwrap();
        let parser = Parser::new(tokens, interner);
        let (ast, mut interner) = parser.parse().unwrap();

        let mut astgen = AstGen::with_symbol_normalizer(&interner, |symbol| symbol);
        astgen.append_items(&ast.items);
        let rir = astgen.finish();

        let sema = Sema::new_synthetic(&rir, &mut interner, PreviewFeatures::new());
        let output = sema.analyze_all_for_test().unwrap();

        let func = select(&output.functions);
        CfgBuilder::build(
            &func.air,
            func.num_locals,
            func.num_param_slots,
            &func.name,
            &output.type_pool,
            func.param_modes.clone(),
            &interner,
            func.allow_unreachable_code,
            func.callable_kind,
        )
        .cfg
        .unwrap()
        .into_editor()
    }

    /// The trusted standard-library `Option` producer, provided verbatim at the
    /// trusted `\0rue-std/option.rue` logical identity so a fixture's `?` binds
    /// the real std `Option`.
    const TRUSTED_OPTION_PRODUCER: &str =
        "pub fn Option(comptime T: type) -> type { enum { Some(T), None } }\n";

    /// Build the CFG for the function named `name`, compiling `source` as a
    /// trusted standard-library `\0rue-std/option.rue` module that also defines
    /// the std `Option` producer, so `?` on an `Option` specialization is legal.
    ///
    /// `?` legality is exact producer-module identity, not shape (RUE-1112): an
    /// `Option(T)` specialization is the std `Option` only when its producer
    /// roots at the trusted `\0rue-std/option.rue::Option` definition. The
    /// synthetic harness grants that identity by carrying the module's symbol
    /// path and trusted-standard-library provenance on the file that defines the
    /// producer. `StrBuf` remains an ordinary drop nominal: the sole lang item
    /// keys on `\0rue-std/strbuf.rue`, so a `StrBuf` declared here is never
    /// promoted to the trusted-std language item.
    fn build_cfg_named_with_trusted_option(source: &str, name: &str) -> Cfg {
        let module = format!(
            "{TRUSTED_OPTION_PRODUCER}\
             struct StrBuf {{ cap: u64, fn with_capacity(cap: u64) -> StrBuf {{ StrBuf {{ cap: cap }} }} fn inspect(borrow self) {{}} }}\n\
             drop fn StrBuf(self) {{}}\n{source}"
        );
        let module_file = FileId::DEFAULT;

        let lexer = Lexer::new(&module);
        let (tokens, interner) = lexer.tokenize().unwrap();
        let (ast, mut interner) = Parser::new(tokens, interner).parse().unwrap();
        let mut astgen = AstGen::with_symbol_normalizer(&interner, |symbol| symbol);
        astgen.append_items(&ast.items);
        let rir = astgen.finish();

        let mut sema = Sema::new_synthetic(&rir, &mut interner, PreviewFeatures::new());
        sema.set_root_file_id(module_file);
        sema.set_file_paths(HashMap::from([(module_file, "/std/option.rue".to_owned())]));
        sema.set_symbol_paths(HashMap::from([(
            module_file,
            "\0rue-std/option.rue".to_owned(),
        )]));
        sema.set_trusted_standard_library_files(HashSet::from([module_file]));
        let output = sema.analyze_all_for_test_with_stable_endpoints().unwrap();

        // The fixture's module carries an explicit trusted-standard-library
        // symbol path, so its ordinary functions qualify by that path.
        let symbol = format!(
            "__rue_fn_{}__{name}",
            rue_air::mangle_symbol_component(&rue_air::normalize_module_path(
                "\0rue-std/option.rue"
            ))
        );
        let func = output
            .functions
            .iter()
            .find(|f| f.name == symbol)
            .unwrap_or_else(|| panic!("no analyzed function named '{}'", name));
        CfgBuilder::build(
            &func.air,
            func.num_locals,
            func.num_param_slots,
            &func.name,
            &output.type_pool,
            func.param_modes.clone(),
            &interner,
            func.allow_unreachable_code,
            func.callable_kind,
        )
        .cfg
        .unwrap()
        .into_editor()
    }

    /// Count the Drop instructions in a CFG.
    fn count_drops(cfg: &Cfg) -> usize {
        cfg.blocks()
            .iter()
            .flat_map(|b| b.insts.iter())
            .filter(|v| matches!(cfg.get_inst(**v).data, CfgInstData::Drop { .. }))
            .count()
    }

    #[test]
    fn projected_only_parameter_preserves_logical_base_type() {
        let cfg = build_cfg_named(
            "struct Pair { a: i32, b: i32 }\n\
             fn read(borrow p: Pair) -> i32 { p.a }\n\
             fn main() -> i32 { let p = Pair { a: 1, b: 2 }; read(borrow p) }",
            "read",
        );
        let (place, struct_id) = cfg
            .blocks()
            .iter()
            .flat_map(|block| block.insts.iter().copied())
            .find_map(|value| match &cfg.get_inst(value).data {
                CfgInstData::PlaceRead { place } => {
                    let Projection::Field { struct_id, .. } = cfg.get_place_projections(place)[0]
                    else {
                        return None;
                    };
                    Some((place.duplicate_with_owner(), struct_id))
                }
                _ => None,
            })
            .expect("projected parameter read");

        assert_eq!(place.base, PlaceBase::Param(0));
        assert_eq!(place.base_type, Type::new_struct(struct_id));
        assert!(
            cfg.blocks()
                .iter()
                .flat_map(|block| block.insts.iter())
                .all(|value| !matches!(cfg.get_inst(*value).data, CfgInstData::Param { .. })),
            "the base type must not depend on a separate Param instruction"
        );
    }

    #[test]
    fn computed_aggregate_projection_reserves_the_complete_temp_width() {
        let cfg = build_cfg_named(
            "struct Pair { left: i32, right: i32 }\n\
             fn make() -> Pair { Pair { left: 10, right: 20 } }\n\
             fn use(n: i32) -> i32 { make().right + n }\n\
             fn main() -> i32 { use(5) }",
            "use",
        );

        assert_eq!(cfg.num_params(), 1);
        assert_eq!(
            cfg.num_locals(),
            2,
            "the two-slot Pair spill must end before the parameter area"
        );
        let place = cfg
            .blocks()
            .iter()
            .flat_map(|block| block.insts.iter().copied())
            .find_map(|value| match &cfg.get_inst(value).data {
                CfgInstData::PlaceRead { place } if matches!(place.base, PlaceBase::Local(0)) => {
                    Some(place.duplicate_with_owner())
                }
                _ => None,
            })
            .expect("computed Pair projection");
        assert!(matches!(
            cfg.get_place_projections(&place),
            [Projection::Field { field_index: 1, .. }]
        ));
    }

    #[test]
    fn aggregate_block_result_is_saved_while_scope_cleanup_runs() {
        let cfg = build_cfg_named(
            "struct Triple { a: u64, b: u64, c: u64 }\n\
             struct Guard { value: i32 }\n\
             drop fn Guard(self) { }\n\
             fn make() -> Triple { Triple { a: 1, b: 2, c: 3 } }\n\
             fn preserve() -> Triple { { let guard = Guard { value: 0 }; make() } }\n\
             fn main() -> i32 { let value = preserve(); @intCast(value.a + value.b + value.c) }",
            "preserve",
        );

        let instructions: Vec<_> = cfg
            .blocks()
            .iter()
            .flat_map(|block| block.insts.iter().copied())
            .collect();
        let drop_index = instructions
            .iter()
            .position(|value| matches!(cfg.get_inst(*value).data, CfgInstData::Drop { .. }))
            .expect("the inner Guard is cleaned up");
        let (alloc_index, scratch_slot) = instructions[..drop_index]
            .iter()
            .enumerate()
            .rev()
            .find_map(|(index, value)| match cfg.get_inst(*value).data {
                CfgInstData::Alloc { slot, .. } => Some((index, slot)),
                _ => None,
            })
            .expect("the aggregate result is saved before cleanup");
        let load_index = instructions[drop_index + 1..]
            .iter()
            .position(|value| {
                matches!(
                    cfg.get_inst(*value).data,
                    CfgInstData::Load { slot } if slot == scratch_slot
                )
            })
            .map(|index| index + drop_index + 1)
            .expect("the aggregate result is restored after cleanup");

        assert!(alloc_index < drop_index && drop_index < load_index);
        assert_eq!(
            cfg.num_locals(),
            4,
            "one Guard slot plus the complete three-slot result scratch"
        );
    }

    #[test]
    fn test_simple_return() {
        let cfg = build_cfg("fn main() -> i32 { 42 }");

        assert_eq!(cfg.block_count(), 1);
        assert_eq!(cfg.fn_name(), "main");

        let entry = cfg.get_block(cfg.entry);
        assert!(matches!(entry.terminator, Terminator::Return { .. }));
    }

    #[test]
    fn test_if_else() {
        let cfg = build_cfg("fn main() -> i32 { if true { 1 } else { 2 } }");

        // Should have: entry, then, else, join
        assert!(cfg.block_count() >= 3);
    }

    #[test]
    fn test_while_loop() {
        let cfg = build_cfg("fn main() -> i32 { let mut x = 0; while x < 10 { x = x + 1; } x }");

        // Should have: entry, header, body, exit, and possibly join blocks
        assert!(cfg.block_count() >= 3);
    }

    #[test]
    fn test_short_circuit_and() {
        let cfg = build_cfg("fn main() -> i32 { if true && false { 1 } else { 0 } }");

        // && creates extra blocks for short-circuit evaluation
        assert!(cfg.block_count() >= 3);
    }

    #[test]
    fn test_diverging_in_if_condition() {
        // Test that a diverging expression (block with return) in an if condition
        // is handled correctly without panicking.
        let cfg = build_cfg("fn main() -> i32 { if { return 1; true } { 2 } else { 3 } }");

        // Should have at least entry block
        assert!(cfg.block_count() >= 1);
        // The function should return from the block in the condition
        let entry = cfg.get_block(cfg.entry);
        assert!(matches!(entry.terminator, Terminator::Return { .. }));
    }

    #[test]
    fn test_diverging_in_loop_body() {
        // Test that a return inside a loop body is handled correctly.
        let cfg = build_cfg("fn main() -> i32 { loop { return 42; } }");

        // The function should return from within the loop
        assert!(cfg.block_count() >= 2);
    }

    /// Assert that every block in the CFG has a terminator. A `Terminator::None`
    /// left behind by the builder is the "block has no terminator" codegen ICE.
    fn assert_all_blocks_terminated(cfg: &Cfg) {
        for block in cfg.blocks() {
            assert!(
                !matches!(block.terminator, Terminator::None),
                "block {} has no terminator",
                block.id.0
            );
        }
    }

    #[test]
    fn test_andand_diverging_rhs_join_terminated() {
        // RUE-128: `true && return 5` — the rhs diverges, but the join block
        // (where the short-circuit value materializes) is still reachable via
        // the lhs-false edge and must be terminated.
        let cfg = build_cfg("fn main() -> i32 { let c = true && return 5; 3 }");
        assert_all_blocks_terminated(&cfg);
    }

    #[test]
    fn test_oror_diverging_rhs_join_terminated() {
        // RUE-128: `false || return 7` — same shape as the && case above.
        let cfg = build_cfg("fn main() -> i32 { let c = false || return 7; 3 }");
        assert_all_blocks_terminated(&cfg);
    }

    #[test]
    fn test_diverged_statement_mid_block_keeps_scope_stack_balanced() {
        // RUE-128: a statement diverging mid-block (here: `break` followed by
        // another statement) used to leak its block's scope_stack entry. The
        // enclosing block's pop then drained the LEAKED scope instead of its
        // own, re-emitting Drop/StorageDead for the inner slot on the loop-exit
        // path — a same-path double drop. With the leak fixed, each droppable
        // local (s and t) is dropped exactly once.
        let cfg = build_cfg(
            "fn main() -> i32 {\n\
                 let mut s = StrBuf.with_capacity(8);\n\
                 loop {\n\
                     let mut t = StrBuf.with_capacity(8);\n\
                     break;\n\
                     let unreachable_tail = 0;\n\
                 }\n\
                 0\n\
             }",
        );
        let drop_count = count_drops(&cfg);
        assert_eq!(
            drop_count, 2,
            "expected exactly one Drop per droppable local (s, t)"
        );
        assert_all_blocks_terminated(&cfg);
    }

    #[test]
    fn match_scrutinee_wrapper_drops_its_temporary_once() {
        let cfg = build_cfg(
            "fn O() -> type { enum { Some(i32), None } }\n\
             fn make() -> O() { let E = O(); E.Some(1) }\n\
             fn main() -> i32 {\n\
                 let E = O();\n\
                 match {\n\
                     let scratch = StrBuf.with_capacity(8);\n\
                     make()\n\
                 } {\n\
                     E.Some(value) => value,\n\
                     E.None => 0,\n\
                 }\n\
             }",
        );

        assert_eq!(
            count_drops(&cfg),
            1,
            "payload reads in match arms must not replay scrutinee cleanup"
        );
        assert_all_blocks_terminated(&cfg);
    }

    #[test]
    fn fallible_initializer_drops_local_only_after_successful_alloc() {
        let cfg = build_cfg_named_with_trusted_option(
            "fn maybe_buf() -> Option(StrBuf) {\n\
                 let O = Option(StrBuf);\n\
                 if true { O.Some(StrBuf.with_capacity(8)) } else { O.None }\n\
             }\n\
             fn read_num() -> Option(i64) {\n\
                 let O = Option(i64);\n\
                 let line = maybe_buf()?;\n\
                 O.Some(@intCast(line.cap))\n\
             }\n\
             fn main() -> i32 { let result = read_num(); 0 }",
            "read_num",
        );

        let line_drops = cfg
            .blocks()
            .iter()
            .flat_map(|block| block.insts.iter().copied())
            .filter(|value| match cfg.get_inst(*value).data {
                CfgInstData::Drop { value } => {
                    matches!(cfg.get_inst(value).data, CfgInstData::Load { slot: 0 })
                }
                _ => false,
            })
            .count();
        let line_storage_dead = cfg
            .blocks()
            .iter()
            .flat_map(|block| block.insts.iter().copied())
            .filter(|value| {
                matches!(
                    cfg.get_inst(*value).data,
                    CfgInstData::StorageDead { slot: 0, .. }
                )
            })
            .count();

        assert_eq!(
            line_drops, 1,
            "the line is dropped on the initialized success path, not the EOF early-return path"
        );
        assert_eq!(
            line_storage_dead, 2,
            "both the success and EOF paths must end the line's storage lifetime"
        );
        assert_all_blocks_terminated(&cfg);
    }

    #[test]
    fn diverging_initializer_never_makes_droppable_local_owned() {
        let cfg = build_cfg_named(
            "fn early() -> i32 {\n\
                 let value: StrBuf = { return 7; StrBuf.with_capacity(8) };\n\
                 0\n\
             }\n\
             fn main() -> i32 { early() }",
            "early",
        );

        assert_eq!(
            count_drops(&cfg),
            0,
            "a local whose initializer never completes must never be dropped"
        );
        assert!(
            cfg.blocks()
                .iter()
                .flat_map(|block| block.insts.iter().copied())
                .any(|value| matches!(
                    cfg.get_inst(value).data,
                    CfgInstData::StorageDead { slot: 0, .. }
                )),
            "the diverging path still ends the local's storage lifetime"
        );
        assert_all_blocks_terminated(&cfg);
    }

    #[test]
    fn test_moved_local_not_dropped_at_source() {
        // RUE-61: `let t = s;` moves s into t — only t's slot is dropped at
        // scope exit; s's drop is suppressed by the MarkMoved marker.
        let cfg = build_cfg(
            "fn main() -> i32 {\n\
                 let s = StrBuf.with_capacity(8);\n\
                 let t = s;\n\
                 0\n\
             }",
        );
        assert_eq!(count_drops(&cfg), 1, "moved-out s must not be dropped");
    }

    #[test]
    fn test_owned_param_dropped_at_exit() {
        // The callee owns its pass-by-value parameters and drops them at
        // function exit (unless moved out).
        let cfg = build_cfg_named(
            "fn f(s: StrBuf) -> i32 { 0 }\n\
             fn main() -> i32 { f(StrBuf.with_capacity(8)) }",
            "f",
        );
        assert_eq!(count_drops(&cfg), 1, "owned StrBuf param must be dropped");
    }

    #[test]
    fn test_moved_param_not_dropped_at_exit() {
        // A param moved into a local is dropped via the local, not again as
        // a param at exit.
        let cfg = build_cfg_named(
            "fn f(s: StrBuf) -> i32 { let t = s; 0 }\n\
             fn main() -> i32 { f(StrBuf.with_capacity(8)) }",
            "f",
        );
        assert_eq!(count_drops(&cfg), 1, "moved param must drop only via t");
    }

    #[test]
    fn test_branch_divergent_move_emits_guarded_drop() {
        // A value moved on only ONE path is NOT "moved on all paths": the
        // scope-exit drop is kept, but behind a runtime drop-flag guard
        // (RUE-108), so the moving path skips it at runtime. Statically the
        // CFG still contains both drops (t's, and s's guarded one) plus the
        // flag plumbing: a conditional branch on the flag around s's drop.
        let cfg = build_cfg(
            "fn main() -> i32 {\n\
                 let s = StrBuf.with_capacity(8);\n\
                 let c = true;\n\
                 if c {\n\
                     let t = s;\n\
                 }\n\
                 0\n\
             }",
        );
        // One drop for t inside the branch, one (flag-guarded) for s at exit.
        assert_eq!(
            count_drops(&cfg),
            2,
            "branch-divergent move keeps a guarded exit drop"
        );
        // The guard exists: at least one Ne comparison against the flag
        // feeding a conditional branch (the if itself uses the bool directly,
        // so an Ne is distinctive of the drop guard).
        let has_flag_compare = cfg
            .blocks()
            .iter()
            .flat_map(|b| b.insts.iter())
            .any(|i| matches!(cfg.get_inst(*i).data, CfgInstData::Ne(..)));
        assert!(has_flag_compare, "exit drop must be guarded by a flag test");
    }

    #[test]
    fn test_move_on_both_branches_suppresses_drop() {
        // Moved in BOTH branches => moved on all paths => exit drop suppressed.
        let source = "fn consume(s: StrBuf) -> i32 { 0 }\n\
             fn main() -> i32 {\n\
                 let s = StrBuf.with_capacity(8);\n\
                 let c = true;\n\
                 if c {\n\
                     consume(s);\n\
                 } else {\n\
                     consume(s);\n\
                 }\n\
                 0\n\
             }";
        let consume_cfg = build_cfg_named(source, "consume");
        let main_cfg = build_cfg_named(source, "main");
        assert_eq!(
            count_drops(&consume_cfg),
            1,
            "consume drops its owned param once"
        );
        assert_eq!(
            count_drops(&main_cfg),
            0,
            "s is moved on every path; main must not drop it"
        );
    }

    #[test]
    fn test_partial_field_move_drops_only_remaining_field() {
        // RUE-62: moving ONE field out of a struct makes the scope-exit drop
        // field-granular — only the still-owned droppable field is dropped
        // (one Drop), not the whole struct (which would re-drop the moved
        // field via the drop glue).
        let source = "fn eat(s: StrBuf) -> i32 { 0 }\n\
             struct H { a: StrBuf, b: StrBuf }\n\
             fn main() -> i32 {\n\
                 let h = H { a: StrBuf.with_capacity(8), b: StrBuf.with_capacity(8) };\n\
                 eat(h.a);\n\
                 0\n\
             }";
        let main_cfg = build_cfg_named(source, "main");
        assert_eq!(
            count_drops(&main_cfg),
            1,
            "moved field a is skipped; only field b drops at exit"
        );
    }

    #[test]
    fn test_partial_field_move_out_of_destructor_type_rejected_in_sema() {
        // RUE-158 supersedes the RUE-62 scenario this test used to pin
        // (destructor-only call O.__drop(o) for a partially moved struct
        // with its own `drop fn`): sema now rejects moving a field out of
        // a value whose type has a user-defined destructor (E0456), so
        // that elaboration shape is unreachable from legal source. This
        // pins the rejection so the CFG-level assumption stays valid.
        let source = "struct A { x: i32 }\n\
             struct O { a: A, b: i32 }\n\
             drop fn A(self) { }\n\
             drop fn O(self) { }\n\
             fn eat(a: A) -> i32 { 0 }\n\
             fn main() -> i32 {\n\
                 let o = O { a: A { x: 1 }, b: 2 };\n\
                 eat(o.a);\n\
                 0\n\
             }";
        let lexer = Lexer::new(source);
        let (tokens, interner) = lexer.tokenize().unwrap();
        let parser = Parser::new(tokens, interner);
        let (ast, mut interner) = parser.parse().unwrap();
        let mut astgen = AstGen::with_symbol_normalizer(&interner, |symbol| symbol);
        astgen.append_items(&ast.items);
        let rir = astgen.finish();
        let sema = Sema::new_synthetic(&rir, &mut interner, PreviewFeatures::new());
        let err = sema
            .analyze_all_for_test()
            .expect_err("field move out of a destructor-having struct must be rejected");
        assert!(
            format!("{err}").contains("cannot move field"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn test_field_reassignment_restores_whole_struct_drop() {
        // Writing a fresh value into a moved-out field re-initializes it:
        // the per-field moved state is cleared by the projected PlaceWrite,
        // so scope exit is back on the whole-struct fast path — ONE Drop
        // whose operand is a whole-slot Load (covering both fields via the
        // drop glue), not a field-granular PlaceRead drop of just field b.
        let source = "fn eat(s: StrBuf) -> i32 { 0 }\n\
             struct H { a: StrBuf, b: StrBuf }\n\
             fn main() -> i32 {\n\
                 let mut h = H { a: StrBuf.with_capacity(8), b: StrBuf.with_capacity(8) };\n\
                 eat(h.a);\n\
                 h.a = StrBuf.with_capacity(4);\n\
                 0\n\
             }";
        let main_cfg = build_cfg_named(source, "main");
        let dropped_values: Vec<_> = main_cfg
            .blocks()
            .iter()
            .flat_map(|b| b.insts.iter())
            .filter_map(|v| match main_cfg.get_inst(*v).data {
                CfgInstData::Drop { value } => Some(value),
                _ => None,
            })
            .collect();
        assert_eq!(dropped_values.len(), 1, "exactly one whole-struct drop");
        assert!(
            matches!(
                main_cfg.get_inst(dropped_values[0]).data,
                CfgInstData::Load { slot: 0 }
            ),
            "the drop covers the whole re-initialized struct, not one field"
        );
    }

    #[test]
    fn test_deep_field_move_skips_only_moved_leaf() {
        // RUE-157: a depth-2 field path move (`eat(t.mid.leaf)`) is exported
        // to drop elaboration. The exit drop recurses field-granularly: the
        // moved leaf is skipped and only the sibling leaf gets a Drop.
        let source = "struct Leaf { v: i32 }\n\
             struct Mid { leaf: Leaf, other: Leaf }\n\
             struct Top { mid: Mid }\n\
             drop fn Leaf(self) { }\n\
             fn eat(l: Leaf) -> i32 { 0 }\n\
             fn main() -> i32 {\n\
                 let t = Top { mid: Mid { leaf: Leaf { v: 5 }, other: Leaf { v: 6 } } };\n\
                 eat(t.mid.leaf);\n\
                 0\n\
             }";
        let main_cfg = build_cfg_named(source, "main");
        assert_eq!(
            count_drops(&main_cfg),
            1,
            "moved t.mid.leaf is skipped; only t.mid.other drops at exit"
        );
        assert_all_blocks_terminated(&main_cfg);
    }

    #[test]
    fn test_branch_divergent_field_move_emits_guarded_field_drop() {
        // RUE-156: a field moved in only ONE branch keeps its scope-exit
        // drop, but behind a per-field-path runtime drop flag. Statically
        // the CFG contains the guarded drop of field a plus the plain drop
        // of field b, and a distinctive Ne flag test.
        let source = "struct Inner { v: i32 }\n\
             struct Outer { a: Inner, b: Inner }\n\
             drop fn Inner(self) { }\n\
             fn eat(i: Inner) -> i32 { 0 }\n\
             fn main() -> i32 {\n\
                 let o = Outer { a: Inner { v: 1 }, b: Inner { v: 2 } };\n\
                 let c = true;\n\
                 if c {\n\
                     eat(o.a);\n\
                 }\n\
                 0\n\
             }";
        let main_cfg = build_cfg_named(source, "main");
        assert_eq!(
            count_drops(&main_cfg),
            2,
            "guarded drop of conditionally moved a, plain drop of b"
        );
        let has_flag_compare = main_cfg
            .blocks()
            .iter()
            .flat_map(|b| b.insts.iter())
            .any(|i| matches!(main_cfg.get_inst(*i).data, CfgInstData::Ne(..)));
        assert!(
            has_flag_compare,
            "field a's exit drop must be guarded by a flag test"
        );
        assert_all_blocks_terminated(&main_cfg);
    }

    #[test]
    fn cfg_preserves_logical_writability_separately_from_by_ref_abi() {
        let source = "struct Pair { a: i32, b: i32 }\n\
             fn borrowed(borrow p: Pair) -> i32 { p.a }\n\
             fn writable(inout p: Pair) -> i32 { p.a }\n\
             fn main() -> i32 {\n\
                 let mut p = Pair { a: 1, b: 2 };\n\
                 borrowed(borrow p) + writable(inout p)\n\
             }";

        let borrow_cfg = build_cfg_named(source, "borrowed");
        assert!(borrow_cfg.is_param_by_ref(0), "borrow uses the by-ref ABI");
        assert!(
            !borrow_cfg.is_param_writable(0),
            "borrow must not carry logical write permission"
        );

        let inout_cfg = build_cfg_named(source, "writable");
        assert!(inout_cfg.is_param_by_ref(0), "inout uses the by-ref ABI");
        assert!(
            inout_cfg.is_param_writable(0),
            "inout must preserve logical write permission"
        );
    }

    #[test]
    fn borrowed_computed_projection_drops_its_owner_once_after_the_consumer() {
        use rue_air::{AirPlaceBase, AirProjection, StructDef, StructField, TypeInternPool};
        use rue_span::{FileId, Span};

        for shape in ["direct", "nested", "index"] {
            let interner = ThreadedRodeo::default();
            let type_pool = TypeInternPool::new();
            let span = Span::new(0, 1);
            let struct_def = |name: &str, fields, destructor| StructDef {
                name: name.into(),
                fields,
                is_copy: false,
                is_linear: false,
                destructor,
                is_builtin: false,
                is_pub: false,
                file_id: FileId::DEFAULT,
            };
            let (leaf_id, _) = type_pool.register_struct(
                interner.get_or_intern("Leaf"),
                struct_def(
                    "Leaf",
                    vec![StructField {
                        name: "value".into(),
                        ty: Type::I32,
                    }],
                    None,
                ),
            );
            let leaf_ty = Type::new_struct(leaf_id);
            let array_id = type_pool.intern_array_from_type(leaf_ty, 1);
            let array_ty = Type::new_array(array_id);
            let owner_field_ty = match shape {
                "direct" => Type::I32,
                "nested" => leaf_ty,
                "index" => array_ty,
                _ => unreachable!(),
            };
            let (owner_id, _) = type_pool.register_struct(
                interner.get_or_intern("Owner"),
                struct_def(
                    "Owner",
                    vec![StructField {
                        name: "field".into(),
                        ty: owner_field_ty,
                    }],
                    Some("Owner.__drop".into()),
                ),
            );
            let owner_ty = Type::new_struct(owner_id);
            let type_pool = type_pool.freeze();

            let mut air = AirEditor::new(Type::UNIT);
            let constant = air.add_const(7, Type::I32, span);
            let leaf = air
                .add_struct_init(leaf_id, &[constant], &[0], leaf_ty, span)
                .unwrap();
            let array = air.add_array_init(&[leaf], array_ty, span).unwrap();
            let owner_field = match shape {
                "direct" => constant,
                "nested" => leaf,
                "index" => array,
                _ => unreachable!(),
            };
            let owner = air
                .add_struct_init(owner_id, &[owner_field], &[0], owner_ty, span)
                .unwrap();
            let live = air.add_storage_live(0, owner_ty, span);
            let alloc = air.add_alloc(0, owner, span);
            let index = air.add_const(0, Type::U64, span);
            let projections = match shape {
                "direct" => vec![AirProjection::Field {
                    struct_id: owner_id,
                    field_index: 0,
                }],
                "nested" => vec![
                    AirProjection::Field {
                        struct_id: owner_id,
                        field_index: 0,
                    },
                    AirProjection::Field {
                        struct_id: leaf_id,
                        field_index: 0,
                    },
                ],
                "index" => vec![
                    AirProjection::Field {
                        struct_id: owner_id,
                        field_index: 0,
                    },
                    AirProjection::Index {
                        array_type: array_ty,
                        index,
                    },
                    AirProjection::Field {
                        struct_id: leaf_id,
                        field_index: 0,
                    },
                ],
                _ => unreachable!(),
            };
            let place = air
                .make_place(AirPlaceBase::Local(0), owner_ty, projections)
                .unwrap();
            let read = air.add_place_read(place, Type::I32, span);
            let call = air
                .add_call(
                    None,
                    interner.get_or_intern("consume"),
                    &[rue_air::AirCallArg {
                        value: read,
                        mode: AirArgMode::Borrow,
                    }],
                    Type::UNIT,
                    span,
                )
                .unwrap();
            let block = air
                .add_block(&[live, alloc], call, Type::UNIT, span)
                .unwrap();
            air.add_ret(Some(block), Type::UNIT, span);

            let air = air
                .finish(AirValidationContext::Canonical(&type_pool))
                .expect("test AIR must validate");
            let cfg = CfgBuilder::build(
                &air,
                1,
                0,
                "probe",
                &type_pool,
                vec![],
                &interner,
                false,
                AnalyzedCallableKind::Ordinary,
            )
            .cfg
            .unwrap();
            let instructions = cfg
                .blocks()
                .iter()
                .flat_map(|block| block.insts.iter().copied())
                .collect::<Vec<_>>();
            let consumer = instructions
                .iter()
                .position(|value| matches!(cfg.get_inst(*value).data, CfgInstData::Call { .. }))
                .expect("consumer call");
            let drops = instructions
                .iter()
                .enumerate()
                .filter_map(|(i, value)| {
                    matches!(cfg.get_inst(*value).data, CfgInstData::Drop { .. }).then_some(i)
                })
                .collect::<Vec<_>>();
            assert_eq!(drops.len(), 1, "{shape}: exactly one owner drop");
            let CfgInstData::Drop { value } = cfg.get_inst(instructions[drops[0]]).data else {
                unreachable!()
            };
            assert!(
                matches!(cfg.get_inst(value).data, CfgInstData::Load { slot: 0 }),
                "{shape}: drop loads owner slot 0"
            );
            let dead = instructions
                .iter()
                .position(|value| {
                    matches!(
                        cfg.get_inst(*value).data,
                        CfgInstData::StorageDead { slot: 0, .. }
                    )
                })
                .expect("owner storage dead");
            assert!(
                consumer < drops[0] && drops[0] < dead,
                "{shape}: Call < Drop < StorageDead"
            );
            assert_all_blocks_terminated(&cfg);
        }
    }

    #[test]
    fn callgeneric_reaching_cfg_is_internal_error_not_panic() {
        // RUE-7: an un-specialized `CallGeneric` that survives to CFG building
        // is malformed AIR (the specialization pass should have rewritten it to
        // a regular `Call`). The builder must record a clean
        // internal-compiler-error diagnostic on the error channel rather than
        // panicking / aborting the process.
        use rue_span::Span;

        let interner = ThreadedRodeo::default();
        let name = interner.get_or_intern("generic_fn");
        let type_pool = FrozenTypeInternPool::new();

        let mut air = AirEditor::new(Type::I32);
        let call = air
            .add_call_generic(name, &[], &[], &[], Type::I32, Span::new(0, 1))
            .unwrap();
        // The root (last instruction) must be a Ret so `build` starts lowering
        // from it and reaches the CallGeneric operand.
        air.add_ret(Some(call), Type::I32, Span::new(0, 1));
        let air = air
            .finish(AirValidationContext::Canonical(&type_pool))
            .expect("test AIR must validate");

        let output = CfgBuilder::build(
            &air,
            0,
            0,
            "generic_fn",
            &type_pool,
            vec![],
            &interner,
            false,
            AnalyzedCallableKind::Ordinary,
        );

        assert!(
            !output.errors.is_empty(),
            "CallGeneric at CFG build time must record an internal-compiler-error \
             diagnostic instead of panicking"
        );
        let msg = output.errors[0].to_string();
        assert!(
            msg.contains("internal compiler error"),
            "diagnostic should be an ICE, got: {msg}"
        );
        assert!(
            msg.contains("CallGeneric"),
            "diagnostic should mention CallGeneric, got: {msg}"
        );
    }
}
