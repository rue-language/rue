//! AIR to CFG lowering.
//!
//! This module converts the structured control flow in AIR (Branch, Loop)
//! into explicit basic blocks with terminators.

use lasso::ThreadedRodeo;
use rue_air::{
    Air, AirArgMode, AirInstData, AirPattern, AirPlaceBase, AirPlaceRef, AirProjection, AirRef,
    StructId, Type, TypeInternPool, TypeKind,
};
use rue_error::{CompileWarning, WarningKind};

use crate::CfgOutput;
use crate::inst::{
    BlockId, Cfg, CfgArgMode, CfgCallArg, CfgInst, CfgInstData, CfgValue, Place, PlaceBase,
    Projection, Terminator,
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
type FieldPath = Vec<u32>;

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
    fields: std::collections::HashSet<(MovedSlot, FieldPath)>,
    /// `(slot, path)` pairs for field paths moved out on SOME tracked path.
    /// Always a superset of `fields`. Join: union. A path here but not in
    /// `fields` is path-dependent: its scope-exit drop is emitted behind
    /// that path's runtime drop flag.
    maybe_fields: std::collections::HashSet<(MovedSlot, FieldPath)>,
}

impl MoveState {
    /// Record that the slot's whole value moved out.
    fn mark_slot(&mut self, slot: MovedSlot) {
        self.slots.insert(slot);
        // Whole-value move subsumes any per-field moves (and the slot's
        // whole-value drop flag takes over at runtime).
        self.fields.retain(|(s, _)| *s != slot);
        self.maybe_fields.retain(|(s, _)| *s != slot);
    }

    /// Record that one struct field path of the slot moved out.
    fn mark_path(&mut self, slot: MovedSlot, path: FieldPath) {
        self.fields.insert((slot, path.clone()));
        self.maybe_fields.insert((slot, path));
    }

    /// The slot was (re)initialized with a fresh value: clear all move-out
    /// state for it (whole-slot and per-field), so the new occupant is
    /// dropped at scope exit.
    fn clear_slot(&mut self, slot: MovedSlot) {
        self.slots.remove(&slot);
        self.fields.retain(|(s, _)| *s != slot);
        self.maybe_fields.retain(|(s, _)| *s != slot);
    }

    /// One top-level field of the slot was reassigned: that field (and
    /// everything nested inside it) holds a fresh value again and must be
    /// dropped at scope exit.
    fn clear_field(&mut self, slot: MovedSlot, field: u32) {
        self.fields
            .retain(|(s, p)| *s != slot || p.first() != Some(&field));
        self.maybe_fields
            .retain(|(s, p)| *s != slot || p.first() != Some(&field));
    }

    /// Was the slot's whole value moved out (on every tracked path)?
    fn is_slot_moved(&self, slot: MovedSlot) -> bool {
        self.slots.contains(&slot)
    }

    /// Field paths moved out of `slot` on EVERY tracked path.
    fn moved_paths_of(&self, slot: MovedSlot) -> std::collections::HashSet<FieldPath> {
        self.fields
            .iter()
            .filter(|(s, _)| *s == slot)
            .map(|(_, p)| p.clone())
            .collect()
    }

    /// Field paths moved out of `slot` on SOME tracked path (superset of
    /// `moved_paths_of`).
    fn maybe_moved_paths_of(&self, slot: MovedSlot) -> std::collections::HashSet<FieldPath> {
        self.maybe_fields
            .iter()
            .filter(|(s, _)| *s == slot)
            .map(|(_, p)| p.clone())
            .collect()
    }

    /// Join two path states: definite state (`slots`, `fields`) is kept
    /// only if present in BOTH ("moved on ALL paths" — see the `moved`
    /// field docs); possible state (`maybe_fields`) is kept if present in
    /// EITHER ("moved on SOME path").
    fn intersect(&self, other: &MoveState) -> MoveState {
        MoveState {
            slots: self.slots.intersection(&other.slots).copied().collect(),
            fields: self.fields.intersection(&other.fields).cloned().collect(),
            maybe_fields: self
                .maybe_fields
                .union(&other.maybe_fields)
                .cloned()
                .collect(),
        }
    }
}

/// Builder that converts AIR to CFG.
pub struct CfgBuilder<'a> {
    air: &'a Air,
    cfg: Cfg,
    /// Type intern pool for struct/enum/array lookups (Phase 2B ADR-0024)
    type_pool: &'a TypeInternPool,
    /// Interner for resolving symbols to strings
    interner: &'a ThreadedRodeo,
    /// Current block we're building
    current_block: BlockId,
    /// Stack of loop contexts for nested loops
    loop_stack: Vec<LoopContext>,
    /// Cache: maps AIR refs to CFG values (for already-lowered instructions)
    value_cache: Vec<Option<CfgValue>>,
    /// Warnings collected during CFG construction (e.g., unreachable code)
    warnings: Vec<CompileWarning>,
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
    field_drop_flags: std::collections::HashMap<(MovedSlot, FieldPath), u32>,
    /// Slots with a whole-value MarkMoved anywhere in the AIR (pre-scanned),
    /// i.e. candidates for a drop flag.
    ever_moved: std::collections::HashSet<MovedSlot>,
    /// `(slot, field path)` pairs with a field-level MarkMoved anywhere in
    /// the AIR whose moved type needs drop (pre-scanned), i.e. candidates
    /// for a per-field drop flag.
    ever_field_moved: std::collections::HashSet<(MovedSlot, FieldPath)>,
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
}

impl<'a> CfgBuilder<'a> {
    /// Build a CFG from AIR, returning the CFG and any warnings.
    ///
    /// The `type_pool` provides struct/enum/array definitions needed for queries like
    /// `type_needs_drop`. The `interner` is used to resolve Symbol values to strings for the CFG.
    pub fn build(
        air: &'a Air,
        num_locals: u32,
        num_params: u32,
        fn_name: &str,
        type_pool: &'a TypeInternPool,
        param_modes: Vec<bool>,
        interner: &'a ThreadedRodeo,
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
            current_block: BlockId(0),
            loop_stack: Vec::new(),
            value_cache: vec![None; air.len()],
            warnings: Vec::new(),
            scope_stack: vec![Vec::new()], // Start with one scope for the function body
            drop_flags: std::collections::HashMap::new(),
            field_drop_flags: std::collections::HashMap::new(),
            ever_moved: std::collections::HashSet::new(),
            ever_field_moved: std::collections::HashSet::new(),
            moved: MoveState::default(),
        };

        // Create entry block
        builder.current_block = builder.cfg.new_block();
        builder.cfg.entry = builder.current_block;

        // Pre-scan for moves so drop flags can be initialized at the
        // value's init site, before the move is reached (RUE-108).
        // Whole-value MarkMoveds nominate the slot for a whole-slot flag;
        // field-level MarkMoveds nominate their (slot, field path) for a
        // per-field flag (RUE-156) when the moved type needs dropping.
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

        CfgOutput {
            cfg: builder.cfg,
            warnings: builder.warnings,
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
                // CallGeneric instructions must be specialized (rewritten to Call)
                // before CFG building. If we reach here, specialization didn't run.
                //
                // TODO(ICE): This should be converted to:
                //   return Err(ice_error!("CallGeneric not specialized", phase: "cfg_builder"));
                // But that requires refactoring build() to return CompileResult.
                panic!(
                    "CallGeneric instruction reached CFG building - this is a compiler bug. \
                     CallGeneric must be specialized to regular Call before codegen."
                );
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

            AirInstData::And(lhs, rhs) => {
                // Short-circuit: if lhs is false, result is false
                // We need to create blocks for this
                let Some(lhs_val) = self.lower_value(*lhs) else {
                    return Self::diverged();
                };

                let rhs_block = self.cfg.new_block();
                let join_block = self.cfg.new_block();

                // Add block parameter for the result
                let result_param = self.cfg.add_block_param(join_block, Type::BOOL);

                // Branch: if lhs is false, go to join with false; else evaluate rhs
                let false_val = self.emit(CfgInstData::BoolConst(false), Type::BOOL, span);
                let (then_args_start, then_args_len) = self.cfg.push_extra(std::iter::empty());
                let (else_args_start, else_args_len) =
                    self.cfg.push_extra(std::iter::once(false_val));
                self.cfg.set_terminator(
                    self.current_block,
                    Terminator::Branch {
                        cond: lhs_val,
                        then_block: rhs_block,
                        then_args_start,
                        then_args_len,
                        else_block: join_block,
                        else_args_start,
                        else_args_len,
                    },
                );

                // In rhs_block, evaluate rhs and go to join.
                // If the rhs diverges (e.g. `true && return 5`), its block already
                // ends in the diverging terminator and contributes no edge to the
                // join — but the join is STILL reachable via the short-circuit
                // (lhs-false) edge, so we must continue lowering there rather than
                // propagate divergence (which would leave the join unterminated).
                // The rhs only runs on one of the two paths into the join, so
                // moves inside it are not "moved on all paths": restore the
                // pre-rhs move state afterwards.
                let moved_before_rhs = self.moved.clone();
                self.current_block = rhs_block;
                if let Some(rhs_val) = self.lower_value(*rhs) {
                    let (args_start, args_len) = self.cfg.push_extra(std::iter::once(rhs_val));
                    self.cfg.set_terminator(
                        self.current_block,
                        Terminator::Goto {
                            target: join_block,
                            args_start,
                            args_len,
                        },
                    );
                }

                // Continue in join block
                self.moved = moved_before_rhs;
                self.current_block = join_block;
                self.cache(air_ref, result_param);
                ExprResult {
                    value: Some(result_param),
                    continuation: Continuation::Continues,
                }
            }

            AirInstData::Or(lhs, rhs) => {
                // Short-circuit: if lhs is true, result is true
                let Some(lhs_val) = self.lower_value(*lhs) else {
                    return Self::diverged();
                };

                let rhs_block = self.cfg.new_block();
                let join_block = self.cfg.new_block();

                // Add block parameter for the result
                let result_param = self.cfg.add_block_param(join_block, Type::BOOL);

                // Branch: if lhs is true, go to join with true; else evaluate rhs
                let true_val = self.emit(CfgInstData::BoolConst(true), Type::BOOL, span);
                let (then_args_start, then_args_len) =
                    self.cfg.push_extra(std::iter::once(true_val));
                let (else_args_start, else_args_len) = self.cfg.push_extra(std::iter::empty());
                self.cfg.set_terminator(
                    self.current_block,
                    Terminator::Branch {
                        cond: lhs_val,
                        then_block: join_block,
                        then_args_start,
                        then_args_len,
                        else_block: rhs_block,
                        else_args_start,
                        else_args_len,
                    },
                );

                // In rhs_block, evaluate rhs and go to join.
                // If the rhs diverges (e.g. `false || return 7`), its block already
                // ends in the diverging terminator and contributes no edge to the
                // join — but the join is STILL reachable via the short-circuit
                // (lhs-true) edge, so we must continue lowering there rather than
                // propagate divergence (which would leave the join unterminated).
                // The rhs only runs on one of the two paths into the join:
                // restore the pre-rhs move state afterwards (see And above).
                let moved_before_rhs = self.moved.clone();
                self.current_block = rhs_block;
                if let Some(rhs_val) = self.lower_value(*rhs) {
                    let (args_start, args_len) = self.cfg.push_extra(std::iter::once(rhs_val));
                    self.cfg.set_terminator(
                        self.current_block,
                        Terminator::Goto {
                            target: join_block,
                            args_start,
                            args_len,
                        },
                    );
                }

                // Continue in join block
                self.moved = moved_before_rhs;
                self.current_block = join_block;
                self.cache(air_ref, result_param);
                ExprResult {
                    value: Some(result_param),
                    continuation: Continuation::Continues,
                }
            }

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
                name,
                args_start,
                args_len,
            } => {
                let mut arg_vals = Vec::new();
                for arg in self.air.get_call_args(*args_start, *args_len) {
                    let Some(value) = self.lower_value(arg.value) else {
                        return Self::diverged();
                    };
                    arg_vals.push(CfgCallArg {
                        value,
                        mode: Self::convert_arg_mode(arg.mode),
                    });
                }
                // Store args in extra array
                let (args_start, args_len) = self.cfg.push_call_args(arg_vals);
                let value = self.emit(
                    CfgInstData::Call {
                        name: *name,
                        args_start,
                        args_len,
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

            AirInstData::Intrinsic {
                name,
                args_start,
                args_len,
            } => {
                let mut arg_vals = Vec::new();
                for arg in self.air.get_air_refs(*args_start, *args_len) {
                    let Some(val) = self.lower_value(arg) else {
                        return Self::diverged();
                    };
                    arg_vals.push(val);
                }
                // Store args in extra array
                let (args_start, args_len) = self.cfg.push_extra(arg_vals);
                let value = self.emit(
                    CfgInstData::Intrinsic {
                        name: *name,
                        args_start,
                        args_len,
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

            AirInstData::StructInit {
                struct_id,
                fields_start,
                fields_len,
                source_order_start,
            } => {
                // Evaluate field initializers in SOURCE ORDER (spec 4.0:8)
                // The source_order tells us which declaration-order index to evaluate at each step
                let (fields, source_order) =
                    self.air
                        .get_struct_init(*fields_start, *fields_len, *source_order_start);
                let fields: Vec<AirRef> = fields.collect();
                let source_order: Vec<usize> = source_order.collect();

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
                let (fields_start, fields_len) = self.cfg.push_extra(field_vals);
                let value = self.emit(
                    CfgInstData::StructInit {
                        struct_id: *struct_id,
                        fields_start,
                        fields_len,
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

            AirInstData::FieldGet {
                base,
                struct_id,
                field_index,
            } => {
                // ADR-0030 Phase 3: Try to use PlaceRead for field access
                if let Some(value) = self.lower_place_read(air_ref, ty, span) {
                    self.cache(air_ref, value);
                    return ExprResult {
                        value: Some(value),
                        continuation: Continuation::Continues,
                    };
                }

                // ADR-0030 Phase 6: Spill computed struct to temp, then use PlaceRead
                // This handles cases like `get_struct().field` or `method().field`
                // where the base is a computed value, not a local variable.
                let Some(base_val) = self.lower_value(*base) else {
                    return Self::diverged();
                };

                // Allocate a temporary slot for the struct
                let temp_slot = self.cfg.alloc_temp_local();

                // Emit StorageLive, Alloc to store the computed struct
                self.emit(
                    CfgInstData::StorageLive { slot: temp_slot },
                    Type::UNIT,
                    span,
                );
                self.emit(
                    CfgInstData::Alloc {
                        slot: temp_slot,
                        init: base_val,
                    },
                    Type::UNIT,
                    span,
                );

                // Create a PlaceRead from the temp slot with Field projection
                let place = self.cfg.make_place(
                    PlaceBase::Local(temp_slot),
                    std::iter::once(Projection::Field {
                        struct_id: *struct_id,
                        field_index: *field_index,
                    }),
                );
                let value = self.emit(CfgInstData::PlaceRead { place }, ty, span);

                // Emit StorageDead for the temp
                self.emit(
                    CfgInstData::StorageDead { slot: temp_slot },
                    Type::UNIT,
                    span,
                );

                self.cache(air_ref, value);
                ExprResult {
                    value: Some(value),
                    continuation: Continuation::Continues,
                }
            }

            AirInstData::FieldSet {
                slot,
                struct_id,
                field_index,
                value,
            } => {
                let Some(val) = self.lower_value(*value) else {
                    return Self::diverged();
                };
                self.emit(
                    CfgInstData::FieldSet {
                        slot: *slot,
                        struct_id: *struct_id,
                        field_index: *field_index,
                        value: val,
                    },
                    Type::UNIT,
                    span,
                );
                ExprResult {
                    value: None,
                    continuation: Continuation::Continues,
                }
            }

            AirInstData::ParamFieldSet {
                param_slot,
                inner_offset,
                struct_id,
                field_index,
                value,
            } => {
                let Some(val) = self.lower_value(*value) else {
                    return Self::diverged();
                };
                self.emit(
                    CfgInstData::ParamFieldSet {
                        param_slot: *param_slot,
                        inner_offset: *inner_offset,
                        struct_id: *struct_id,
                        field_index: *field_index,
                        value: val,
                    },
                    Type::UNIT,
                    span,
                );
                ExprResult {
                    value: None,
                    continuation: Continuation::Continues,
                }
            }

            AirInstData::Block {
                stmts_start,
                stmts_len,
                value,
            } => {
                // Collect statements into a Vec for iteration (needed for checking remaining)
                let statements: Vec<AirRef> =
                    self.air.get_air_refs(*stmts_start, *stmts_len).collect();

                // Check if this is a "wrapper block" that only contains StorageLive statements.
                // These are synthetic blocks created to pair StorageLive with Alloc, and they
                // should NOT create a new scope for drop elaboration.
                let is_storage_live_wrapper = statements.iter().all(|stmt| {
                    matches!(self.air.get(*stmt).data, AirInstData::StorageLive { .. })
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
                            self.warnings.push(
                                CompileWarning::new(WarningKind::UnreachableCode, unreachable_span)
                                    .with_label(
                                        "any code following this expression is unreachable",
                                        diverging_span,
                                    )
                                    .with_note(
                                        "this warning occurs because the preceding expression \
                                         diverges (e.g., returns, breaks, or continues)",
                                    ),
                            );
                        } else {
                            // The final value expression is unreachable.
                            // However, don't warn about synthetic unit values (created by parser
                            // when a block has no trailing expression). These have zero-length
                            // spans pointing at the closing brace.
                            let value_span = self.air.get(*value).span;
                            let is_synthetic = value_span.start == value_span.end;
                            if !is_synthetic {
                                self.warnings.push(
                                    CompileWarning::new(WarningKind::UnreachableCode, value_span)
                                        .with_label(
                                            "any code following this expression is unreachable",
                                            diverging_span,
                                        )
                                        .with_note(
                                            "this warning occurs because the preceding expression \
                                             diverges (e.g., returns, breaks, or continues)",
                                        ),
                                );
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
                let result = self.lower_inst(*value);

                // Pop scope and emit StorageDead (with Drop if needed) in reverse order.
                // BUT: if the value diverged (break/continue/return), the diverging
                // instruction already emitted drops for all scopes via emit_drops_for_all_scopes,
                // so we must NOT emit duplicate StorageDead here.
                if !is_storage_live_wrapper {
                    if let Some(scope_slots) = self.scope_stack.pop() {
                        // Only emit scope cleanup if the value didn't diverge
                        if !matches!(result.continuation, Continuation::Diverged) {
                            for live_slot in scope_slots.into_iter().rev() {
                                let slot_span = live_slot.span;
                                self.emit_drop_for_slot(&live_slot, slot_span);
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
                let (then_args_start, then_args_len) = self.cfg.push_extra(std::iter::empty());
                let (else_args_start, else_args_len) = self.cfg.push_extra(std::iter::empty());
                self.cfg.set_terminator(
                    self.current_block,
                    Terminator::Branch {
                        cond: cond_val,
                        then_block,
                        then_args_start,
                        then_args_len,
                        else_block,
                        else_args_start,
                        else_args_len,
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
                    let args: Vec<CfgValue> = if let Some(val) = then_result.value {
                        if result_param.is_some() {
                            vec![val]
                        } else {
                            vec![]
                        }
                    } else {
                        vec![]
                    };
                    let (args_start, args_len) = self.cfg.push_extra(args);
                    self.cfg.set_terminator(
                        then_exit_block,
                        Terminator::Goto {
                            target: join_block,
                            args_start,
                            args_len,
                        },
                    );
                }

                if !else_diverged {
                    let args: Vec<CfgValue> = if let Some(val) = else_result.value {
                        if result_param.is_some() {
                            vec![val]
                        } else {
                            vec![]
                        }
                    } else {
                        vec![]
                    };
                    let (args_start, args_len) = self.cfg.push_extra(args);
                    self.cfg.set_terminator(
                        else_exit_block,
                        Terminator::Goto {
                            target: join_block,
                            args_start,
                            args_len,
                        },
                    );
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
                let (args_start, args_len) = self.cfg.push_extra(std::iter::empty());
                self.cfg.set_terminator(
                    self.current_block,
                    Terminator::Goto {
                        target: header_block,
                        args_start,
                        args_len,
                    },
                );

                // Push loop context with current scope depth.
                // The scope depth is captured BEFORE the loop body is lowered,
                // so break/continue will drop all slots in scopes created INSIDE the loop.
                self.loop_stack.push(LoopContext {
                    header: header_block,
                    exit: exit_block,
                    scope_depth: self.scope_stack.len(),
                });

                // Lower condition in header
                self.current_block = header_block;
                let Some(cond_val) = self.lower_value(*cond) else {
                    // The condition itself diverges (e.g. `while return 8 {}`), so the
                    // loop body and everything after the loop are unreachable. The
                    // body/exit blocks allocated above are now orphaned — mark them
                    // Unreachable so codegen does not assert on a missing terminator —
                    // and pop the loop context we pushed to keep loop_stack balanced.
                    self.cfg.set_terminator(body_block, Terminator::Unreachable);
                    self.cfg.set_terminator(exit_block, Terminator::Unreachable);
                    self.loop_stack.pop();
                    return Self::diverged();
                };

                // Branch: if true go to body, if false exit
                let (then_args_start, then_args_len) = self.cfg.push_extra(std::iter::empty());
                let (else_args_start, else_args_len) = self.cfg.push_extra(std::iter::empty());
                self.cfg.set_terminator(
                    self.current_block,
                    Terminator::Branch {
                        cond: cond_val,
                        then_block: body_block,
                        then_args_start,
                        then_args_len,
                        else_block: exit_block,
                        else_args_start,
                        else_args_len,
                    },
                );

                // Lower body
                self.current_block = body_block;
                let body_result = self.lower_inst(*body);

                // After body, go back to header (unless diverged)
                if !matches!(body_result.continuation, Continuation::Diverged) {
                    let (args_start, args_len) = self.cfg.push_extra(std::iter::empty());
                    self.cfg.set_terminator(
                        self.current_block,
                        Terminator::Goto {
                            target: header_block,
                            args_start,
                            args_len,
                        },
                    );
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
                let (args_start, args_len) = self.cfg.push_extra(std::iter::empty());
                self.cfg.set_terminator(
                    self.current_block,
                    Terminator::Goto {
                        target: body_block,
                        args_start,
                        args_len,
                    },
                );

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
                    let (args_start, args_len) = self.cfg.push_extra(std::iter::empty());
                    self.cfg.set_terminator(
                        self.current_block,
                        Terminator::Goto {
                            target: body_block,
                            args_start,
                            args_len,
                        },
                    );
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

                // A loop containing a break has type () (a loop without one is
                // !, and its exit block is unreachable). Either way, emit a
                // unit value as the loop expression's result for the
                // reached-via-break path.
                let unit_val = self.emit(CfgInstData::Const(0), Type::UNIT, span);
                ExprResult {
                    value: Some(unit_val),
                    continuation: Continuation::Continues,
                }
            }

            AirInstData::Match {
                scrutinee,
                arms_start,
                arms_len,
            } => {
                // Lower the scrutinee
                let Some(scrutinee_val) = self.lower_value(*scrutinee) else {
                    return Self::diverged();
                };

                // Collect arms into a Vec for iteration
                let arms: Vec<(AirPattern, AirRef)> =
                    self.air.get_match_arms(*arms_start, *arms_len).collect();

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
                let (cases_start, cases_len) = self.cfg.push_switch_cases(switch_cases);
                self.cfg.set_terminator(
                    self.current_block,
                    Terminator::Switch {
                        scrutinee: scrutinee_val,
                        cases_start,
                        cases_len,
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
                        let args: Vec<CfgValue> = if let Some(val) = body_result.value {
                            if result_param.is_some() {
                                vec![val]
                            } else {
                                vec![]
                            }
                        } else {
                            vec![]
                        };
                        let (args_start, args_len) = self.cfg.push_extra(args);
                        self.cfg.set_terminator(
                            exit_block,
                            Terminator::Goto {
                                target: join_block,
                                args_start,
                                args_len,
                            },
                        );
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

                let (args_start, args_len) = self.cfg.push_extra(std::iter::empty());
                self.cfg.set_terminator(
                    self.current_block,
                    Terminator::Goto {
                        target: exit_block,
                        args_start,
                        args_len,
                    },
                );

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

                let (args_start, args_len) = self.cfg.push_extra(std::iter::empty());
                self.cfg.set_terminator(
                    self.current_block,
                    Terminator::Goto {
                        target: header_block,
                        args_start,
                        args_len,
                    },
                );

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

                // Emit drops for all live slots before returning
                self.emit_drops_for_all_scopes(span);

                self.cfg
                    .set_terminator(self.current_block, Terminator::Return { value: val });

                ExprResult {
                    value: None,
                    continuation: Continuation::Diverged,
                }
            }

            AirInstData::ArrayInit {
                elems_start,
                elems_len,
            } => {
                let mut element_vals = Vec::new();
                for elem in self.air.get_air_refs(*elems_start, *elems_len) {
                    let Some(val) = self.lower_value(elem) else {
                        return Self::diverged();
                    };
                    element_vals.push(val);
                }
                // Store elements in extra array
                let (elements_start, elements_len) = self.cfg.push_extra(element_vals);
                let value = self.emit(
                    CfgInstData::ArrayInit {
                        elements_start,
                        elements_len,
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

            AirInstData::IndexGet {
                base,
                array_type,
                index,
            } => {
                // ADR-0030 Phase 3: Try to use PlaceRead for array indexing
                if let Some(value) = self.lower_place_read(air_ref, ty, span) {
                    self.cache(air_ref, value);
                    return ExprResult {
                        value: Some(value),
                        continuation: Continuation::Continues,
                    };
                }

                // ADR-0030 Phase 6: Spill computed array to temp, then use PlaceRead
                // This handles cases like `get_array()[i]` where the base is a computed
                // value, not a local variable.
                // Note: Currently Rue can't return arrays (see issue rue-b79f), but this
                // handles the case for when that's fixed.
                let Some(base_val) = self.lower_value(*base) else {
                    return Self::diverged();
                };
                let Some(index_val) = self.lower_value(*index) else {
                    return Self::diverged();
                };

                // Allocate a temporary slot for the array
                let temp_slot = self.cfg.alloc_temp_local();

                // Emit StorageLive, Alloc to store the computed array
                self.emit(
                    CfgInstData::StorageLive { slot: temp_slot },
                    Type::UNIT,
                    span,
                );
                self.emit(
                    CfgInstData::Alloc {
                        slot: temp_slot,
                        init: base_val,
                    },
                    Type::UNIT,
                    span,
                );

                // Create a PlaceRead from the temp slot with Index projection
                let place = self.cfg.make_place(
                    PlaceBase::Local(temp_slot),
                    std::iter::once(Projection::Index {
                        array_type: *array_type,
                        index: index_val,
                    }),
                );
                let value = self.emit(CfgInstData::PlaceRead { place }, ty, span);

                // Emit StorageDead for the temp
                self.emit(
                    CfgInstData::StorageDead { slot: temp_slot },
                    Type::UNIT,
                    span,
                );

                self.cache(air_ref, value);
                ExprResult {
                    value: Some(value),
                    continuation: Continuation::Continues,
                }
            }

            AirInstData::IndexSet {
                slot,
                array_type,
                index,
                value,
            } => {
                let Some(index_val) = self.lower_value(*index) else {
                    return Self::diverged();
                };
                let Some(val) = self.lower_value(*value) else {
                    return Self::diverged();
                };
                // The overwritten element, if still owned, is dropped before
                // the store (RUE-64). (Element assignment currently reaches
                // CFG as PlaceWrite; kept correct here too.)
                let elem_ty = self.air.get(*value).ty;
                let elem_place = self.cfg.make_place(
                    PlaceBase::Local(*slot),
                    std::iter::once(Projection::Index {
                        array_type: *array_type,
                        index: index_val,
                    }),
                );
                self.emit_projected_overwrite_drop(
                    MovedSlot::Local(*slot),
                    elem_place,
                    elem_ty,
                    false,
                    None,
                    span,
                );
                self.emit(
                    CfgInstData::IndexSet {
                        slot: *slot,
                        array_type: *array_type,
                        index: index_val,
                        value: val,
                    },
                    Type::UNIT,
                    span,
                );
                ExprResult {
                    value: None,
                    continuation: Continuation::Continues,
                }
            }

            AirInstData::ParamIndexSet {
                param_slot,
                array_type,
                index,
                value,
            } => {
                let Some(index_val) = self.lower_value(*index) else {
                    return Self::diverged();
                };
                let Some(val) = self.lower_value(*value) else {
                    return Self::diverged();
                };
                // See the IndexSet arm: drop the overwritten element first
                // (RUE-64).
                let elem_ty = self.air.get(*value).ty;
                let elem_place = self.cfg.make_place(
                    PlaceBase::Param(*param_slot),
                    std::iter::once(Projection::Index {
                        array_type: *array_type,
                        index: index_val,
                    }),
                );
                self.emit_projected_overwrite_drop(
                    MovedSlot::Param(*param_slot),
                    elem_place,
                    elem_ty,
                    false,
                    None,
                    span,
                );
                self.emit(
                    CfgInstData::ParamIndexSet {
                        param_slot: *param_slot,
                        array_type: *array_type,
                        index: index_val,
                        value: val,
                    },
                    Type::UNIT,
                    span,
                );
                ExprResult {
                    value: None,
                    continuation: Continuation::Continues,
                }
            }

            // ADR-0030 Phase 8: Handle AIR place-based instructions
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
                };
                if air_place.projections_len == 0 {
                    self.emit_overwrite_drop(base_key, val_ty, span);
                } else {
                    // A single top-level field write: skip the old-value
                    // drop when the field was definitely moved out, and guard
                    // it with the field's runtime drop flag when the move was
                    // path-dependent (RUE-156 x RUE-64 interaction).
                    let mut field_flag: Option<u32> = None;
                    let field_moved = match self
                        .air
                        .get_projections(air_place.projections_start, air_place.projections_len)
                    {
                        [AirProjection::Field { field_index, .. }] => {
                            let path = vec![*field_index];
                            if self.moved.moved_paths_of(base_key).contains(&path) {
                                true
                            } else {
                                if self.moved.maybe_moved_paths_of(base_key).contains(&path) {
                                    field_flag =
                                        self.field_drop_flags.get(&(base_key, path)).copied();
                                }
                                false
                            }
                        }
                        _ => false,
                    };
                    self.emit_projected_overwrite_drop(
                        base_key,
                        cfg_place,
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
                } else if air_place.projections_len == 1 {
                    if let [AirProjection::Field { field_index, .. }] = self
                        .air
                        .get_projections(air_place.projections_start, air_place.projections_len)
                    {
                        self.moved.clear_field(base_key, *field_index);
                        // The field is re-initialized: re-arm its runtime
                        // drop flag so scope-exit (and later overwrite)
                        // drops fire again (RUE-156).
                        self.update_field_drop_flag(base_key, &[*field_index], true, span);
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
            } => {
                // Enum variants are just their discriminant value
                let value = self.emit(
                    CfgInstData::EnumVariant {
                        enum_id: *enum_id,
                        variant_index: *variant_index,
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

                // Only emit a Drop instruction if the type needs drop.
                // For trivially droppable types, this is a no-op.
                // We use self.type_needs_drop() which has access to struct/array
                // definitions to recursively check if fields need drop.
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
                self.emit(CfgInstData::StorageLive { slot: *slot }, Type::UNIT, span);

                // Fresh storage holds a fresh (not-moved-out) value.
                // Slots are not currently reused, so this is a no-op today,
                // but it keeps the moved-slot state correct if reuse lands.
                self.moved.clear_slot(MovedSlot::Local(*slot));

                // Record this slot as live in the current scope for drop elaboration
                if let Some(scope) = self.scope_stack.last_mut() {
                    scope.push(LiveSlot {
                        slot: *slot,
                        ty,
                        span,
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
                self.emit(CfgInstData::StorageDead { slot: *slot }, Type::UNIT, span);
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
                // record that the slot's contents (or one field path of
                // them, RUE-62/RUE-157) were moved out on this path, so
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
        self.cfg
            .add_inst_to_block(self.current_block, CfgInst { data, ty, span })
    }

    /// Cache a value for an AIR ref.
    fn cache(&mut self, air_ref: AirRef, value: CfgValue) {
        self.value_cache[air_ref.as_u32() as usize] = Some(value);
    }

    /// Lower an instruction and return its value, or None if it diverged.
    /// This is a helper for use with the `?` operator when processing operands.
    /// If the operand diverged, the caller should propagate the divergence.
    fn lower_value(&mut self, air_ref: AirRef) -> Option<CfgValue> {
        let result = self.lower_inst(air_ref);
        if matches!(result.continuation, Continuation::Diverged) {
            None
        } else {
            result.value
        }
    }

    /// Create a diverged ExprResult. Used when an operand diverges.
    fn diverged() -> ExprResult {
        ExprResult {
            value: None,
            continuation: Continuation::Diverged,
        }
    }

    /// Check if a type needs to be dropped (has a destructor).
    ///
    /// This method has access to struct and array definitions, allowing it to
    /// recursively check if struct fields or array elements need drop.
    ///
    /// A type needs drop if dropping it requires cleanup actions:
    /// - Primitives, bool, unit, never, error, enums: trivially droppable (no)
    /// - String: will need drop when mutable strings land (currently no)
    /// - Struct: needs drop if any field needs drop
    /// - Array: needs drop if element type needs drop
    fn type_needs_drop(&self, ty: Type) -> bool {
        match ty.kind() {
            // Primitive types are trivially droppable
            // ComptimeType is a comptime-only type and has no runtime representation
            TypeKind::I8
            | TypeKind::I16
            | TypeKind::I32
            | TypeKind::I64
            | TypeKind::U8
            | TypeKind::U16
            | TypeKind::U32
            | TypeKind::U64
            | TypeKind::Bool
            | TypeKind::Unit
            | TypeKind::Never
            | TypeKind::Error
            | TypeKind::ComptimeType => false,

            // Enum types are trivially droppable (just discriminant values)
            TypeKind::Enum(_) => false,

            // Struct types need drop if they have a destructor (e.g., builtin String)
            // or if any field needs drop
            TypeKind::Struct(struct_id) => {
                let struct_def = self.type_pool.struct_def(struct_id);
                // Builtins with destructors (like String) need drop
                if struct_def.destructor.is_some() {
                    return true;
                }
                // Otherwise, check if any field needs drop
                struct_def.fields.iter().any(|f| self.type_needs_drop(f.ty))
            }

            // Note: String is now Type::Struct with is_builtin=true, handled above

            // Array types need drop if element type needs drop
            TypeKind::Array(array_id) => {
                let (element_type, _length) = self.type_pool.array_def(array_id);
                self.type_needs_drop(element_type)
            }

            // Pointer types don't need drop (they're just addresses)
            TypeKind::PtrConst(_) | TypeKind::PtrMut(_) => false,

            // Module types don't need drop (compile-time only)
            TypeKind::Module(_) => false,
        }
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
        let paths: Vec<FieldPath> = self
            .ever_field_moved
            .iter()
            .filter(|(s, _)| *s == key)
            .map(|(_, p)| p.clone())
            .collect();
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

    /// The field path (declaration indices, outermost first) named by a
    /// field-level MarkMoved's place. Sema only exports markers for pure
    /// `Field` projection chains.
    fn moved_field_path(&self, place_ref: AirPlaceRef) -> FieldPath {
        let place = self.air.get_place(place_ref);
        self.air
            .get_place_projections(place)
            .iter()
            .map(|proj| match proj {
                AirProjection::Field { field_index, .. } => *field_index,
                AirProjection::Index { .. } => {
                    unreachable!("MarkMoved place contains a non-field projection")
                }
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
                    if !b.emit_partial_struct_drop(key, ty, span) {
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
        let (then_args_start, then_args_len) = self.cfg.push_extra(std::iter::empty());
        let (else_args_start, else_args_len) = self.cfg.push_extra(std::iter::empty());
        self.cfg.set_terminator(
            self.current_block,
            Terminator::Branch {
                cond,
                then_block: drop_block,
                then_args_start,
                then_args_len,
                else_block: cont_block,
                else_args_start,
                else_args_len,
            },
        );
        self.current_block = drop_block;
        cont_block
    }

    /// Close a diamond opened by [`Self::begin_flag_guard`]: terminate the
    /// guarded body block with a jump to the continuation block and continue
    /// building there.
    fn end_flag_guard(&mut self, cont_block: BlockId) {
        let (args_start, args_len) = self.cfg.push_extra(std::iter::empty());
        self.cfg.set_terminator(
            self.current_block,
            Terminator::Goto {
                target: cont_block,
                args_start,
                args_len,
            },
        );
        self.current_block = cont_block;
    }

    /// Emit Drop and StorageDead for a single slot.
    /// The Drop is suppressed when the slot's value was moved out on every
    /// path reaching this point (the new owner drops it); the StorageDead
    /// is still emitted to end the slot's storage lifetime. A struct slot
    /// with moved-out FIELDS is dropped field-granularly instead (RUE-62).
    fn emit_drop_for_slot(&mut self, live_slot: &LiveSlot, span: rue_span::Span) {
        // Emit Drop if the type needs it and the value wasn't moved out
        let key = MovedSlot::Local(live_slot.slot);
        if !self.moved.is_slot_moved(key) && self.type_needs_drop(live_slot.ty) {
            let (slot, ty) = (live_slot.slot, live_slot.ty);
            self.emit_guarded(key, span, |b| {
                // Fast path: nothing partially moved — drop the whole value.
                if !b.emit_partial_struct_drop(key, ty, span) {
                    let slot_val = b.emit(CfgInstData::Load { slot }, ty, span);
                    b.emit(CfgInstData::Drop { value: slot_val }, Type::UNIT, span);
                }
            });
        }
        self.emit(
            CfgInstData::StorageDead {
                slot: live_slot.slot,
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
            if !b.emit_partial_struct_drop(key, ty, span) {
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

    /// Field-granular drop of a partially-moved struct (RUE-62, RUE-156,
    /// RUE-157).
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
    /// Returns `false` (caller emits the normal whole-value drop) when the
    /// slot has no (possibly-)moved-out field paths or is not a struct.
    /// Note each destructor still receives the WHOLE struct, including the
    /// moved-out part's stale slots — reading a moved field in a destructor
    /// is a use-after-move just like any other.
    fn emit_partial_struct_drop(&mut self, key: MovedSlot, ty: Type, span: rue_span::Span) -> bool {
        // `maybe` ⊇ `definite`: paths possibly vs. definitely moved out on
        // the paths reaching this exit.
        let definite = self.moved.moved_paths_of(key);
        let maybe = self.moved.maybe_moved_paths_of(key);
        if maybe.is_empty() && definite.is_empty() {
            return false;
        }
        let TypeKind::Struct(struct_id) = ty.kind() else {
            // Per-field move markers are only emitted for struct fields, so
            // this is unreachable today; fall back to the whole-value drop.
            return false;
        };
        self.emit_partial_struct_drop_level(
            key,
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

    /// One struct level of [`Self::emit_partial_struct_drop`]: destructor
    /// (without glue) plus per-field drops for the struct at field path
    /// `path` (projections `projs`) inside slot `key`.
    #[allow(clippy::too_many_arguments)]
    fn emit_partial_struct_drop_level(
        &mut self,
        key: MovedSlot,
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
            let whole_val = if projs.is_empty() {
                match key {
                    MovedSlot::Local(slot) => self.emit(CfgInstData::Load { slot }, ty, span),
                    MovedSlot::Param(index) => self.emit(CfgInstData::Param { index }, ty, span),
                }
            } else {
                let place = self.cfg.make_place(base, projs.iter().copied());
                self.emit(CfgInstData::PlaceRead { place }, ty, span)
            };
            let name = self.interner.get_or_intern(destructor_name);
            let (args_start, args_len) = self.cfg.push_call_args(std::iter::once(CfgCallArg {
                value: whole_val,
                mode: CfgArgMode::Normal,
            }));
            self.emit(
                CfgInstData::Call {
                    name,
                    args_start,
                    args_len,
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
                if let TypeKind::Struct(field_struct_id) = field.ty.kind() {
                    self.emit_partial_struct_drop_level(
                        key,
                        field_struct_id,
                        field.ty,
                        path,
                        projs,
                        definite,
                        maybe,
                        span,
                    );
                    true
                } else {
                    // Deeper paths only exist through struct fields; keep a
                    // whole drop as a defensive fallback.
                    false
                }
            } else {
                false
            };
            if !recursed {
                let place = self.cfg.make_place(base, projs.iter().copied());
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

    // ============================================================================
    // Place Expression Tracing (ADR-0030)
    // ============================================================================

    /// Try to trace an AIR expression back to a Place.
    ///
    /// Returns `Some((base, projections))` if the expression represents a place
    /// (lvalue) that can be read from or written to. Returns `None` if the
    /// expression is not a simple place (e.g., a function call result).
    ///
    /// This function traces chains like `arr[i][j].field` into a `PlaceBase` and
    /// a list of `Projection`s. The projections are returned in order from the
    /// base outward (e.g., for `arr[i].x`, the projections are `[Index(i), Field(x)]`).
    ///
    /// The returned CfgValue indices for Index projections are the already-lowered
    /// index values, which must be computed before calling this function.
    fn try_trace_place(
        &mut self,
        air_ref: AirRef,
    ) -> Option<(PlaceBase, Vec<(Projection, Option<CfgValue>)>)> {
        let inst = self.air.get(air_ref);

        match &inst.data {
            // Base case: Load from a local variable
            AirInstData::Load { slot } => Some((PlaceBase::Local(*slot), Vec::new())),

            // Base case: Parameter reference
            AirInstData::Param { index } => Some((PlaceBase::Param(*index), Vec::new())),

            // Recursive case: Array index
            AirInstData::IndexGet {
                base,
                array_type,
                index,
            } => {
                // Recursively trace the base
                let (base_place, mut projections) = self.try_trace_place(*base)?;

                // Lower the index expression to get the CfgValue
                let index_val = self.lower_value(*index)?;

                // Add the Index projection
                projections.push((
                    Projection::Index {
                        array_type: *array_type,
                        index: index_val,
                    },
                    Some(index_val),
                ));

                Some((base_place, projections))
            }

            // Recursive case: Field access
            AirInstData::FieldGet {
                base,
                struct_id,
                field_index,
            } => {
                // Recursively trace the base
                let (base_place, mut projections) = self.try_trace_place(*base)?;

                // Add the Field projection
                projections.push((
                    Projection::Field {
                        struct_id: *struct_id,
                        field_index: *field_index,
                    },
                    None,
                ));

                Some((base_place, projections))
            }

            // Not a simple place expression
            _ => None,
        }
    }

    /// Lower a place expression from AIR to a CFG PlaceRead instruction.
    ///
    /// This is called when we detect that an IndexGet or FieldGet chain can be
    /// represented as a single PlaceRead, avoiding redundant Load instructions.
    fn lower_place_read(
        &mut self,
        air_ref: AirRef,
        ty: Type,
        span: rue_span::Span,
    ) -> Option<CfgValue> {
        // Try to trace the expression to a place
        let (base, projections) = self.try_trace_place(air_ref)?;

        // Build the Place with all projections
        let proj_iter = projections.into_iter().map(|(proj, _)| proj);
        let place = self.cfg.make_place(base, proj_iter);

        // Emit the PlaceRead instruction
        let value = self.emit(CfgInstData::PlaceRead { place }, ty, span);

        Some(value)
    }

    /// Lower an AIR place reference to a CFG Place.
    ///
    /// This converts AirPlaceRef -> AirPlace -> CFG Place, translating projections
    /// and lowering any index expressions to CFG values.
    ///
    /// ADR-0030 Phase 8: This is the bridge between AIR's PlaceRead/PlaceWrite
    /// and CFG's PlaceRead/PlaceWrite.
    fn lower_air_place(&mut self, place_ref: AirPlaceRef) -> Option<Place> {
        let air_place = self.air.get_place(place_ref);

        // Convert the base
        let base = match air_place.base {
            AirPlaceBase::Local(slot) => PlaceBase::Local(slot),
            AirPlaceBase::Param(slot) => PlaceBase::Param(slot),
        };

        // Convert projections, lowering any index expressions
        let air_projections = self.air.get_place_projections(air_place);
        let mut cfg_projections = Vec::with_capacity(air_projections.len());

        for proj in air_projections {
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
        let place = self.cfg.make_place(base, cfg_projections);

        Some(place)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rue_air::Sema;
    use rue_error::PreviewFeatures;
    use rue_lexer::Lexer;
    use rue_parser::Parser;
    use rue_rir::AstGen;

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
        build_cfg_select(source, |functions| {
            functions
                .iter()
                .find(|f| f.name == name)
                .unwrap_or_else(|| panic!("no analyzed function named '{}'", name))
        })
    }

    /// Build the CFG for the analyzed function of `source` chosen by `select`.
    fn build_cfg_select(
        source: &str,
        select: impl for<'f> Fn(&'f [rue_air::AnalyzedFunction]) -> &'f rue_air::AnalyzedFunction,
    ) -> Cfg {
        let lexer = Lexer::new(source);
        let (tokens, interner) = lexer.tokenize().unwrap();
        let parser = Parser::new(tokens, interner);
        let (ast, mut interner) = parser.parse().unwrap();

        let astgen = AstGen::new(&ast, &mut interner);
        let rir = astgen.generate();

        let sema = Sema::new(&rir, &mut interner, PreviewFeatures::new());
        let output = sema.analyze_all().unwrap();

        let func = select(&output.functions);
        CfgBuilder::build(
            &func.air,
            func.num_locals,
            func.num_param_slots,
            &func.name,
            &output.type_pool,
            func.param_modes.clone(),
            &interner,
        )
        .cfg
    }

    /// Count the Drop instructions in a CFG.
    fn count_drops(cfg: &Cfg) -> usize {
        cfg.blocks()
            .iter()
            .flat_map(|b| b.insts.iter())
            .filter(|v| matches!(cfg.get_inst(**v).data, CfgInstData::Drop { .. }))
            .count()
    }

    /// Count the Call instructions in a CFG.
    fn count_calls(cfg: &Cfg) -> usize {
        cfg.blocks()
            .iter()
            .flat_map(|b| b.insts.iter())
            .filter(|v| matches!(cfg.get_inst(**v).data, CfgInstData::Call { .. }))
            .count()
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
                 let mut s = String::with_capacity(8);\n\
                 loop {\n\
                     let mut t = String::with_capacity(8);\n\
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
    fn test_moved_local_not_dropped_at_source() {
        // RUE-61: `let t = s;` moves s into t — only t's slot is dropped at
        // scope exit; s's drop is suppressed by the MarkMoved marker.
        let cfg = build_cfg(
            "fn main() -> i32 {\n\
                 let s = String::with_capacity(8);\n\
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
        let cfg = build_cfg_for(
            "fn f(s: String) -> i32 { 0 }\n\
             fn main() -> i32 { 0 }",
            0,
        );
        assert_eq!(count_drops(&cfg), 1, "owned String param must be dropped");
    }

    #[test]
    fn test_moved_param_not_dropped_at_exit() {
        // A param moved into a local is dropped via the local, not again as
        // a param at exit.
        let cfg = build_cfg_for(
            "fn f(s: String) -> i32 { let t = s; 0 }\n\
             fn main() -> i32 { 0 }",
            0,
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
                 let s = String::with_capacity(8);\n\
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
        let source = "fn consume(s: String) -> i32 { 0 }\n\
             fn main() -> i32 {\n\
                 let s = String::with_capacity(8);\n\
                 let c = true;\n\
                 if c {\n\
                     consume(s);\n\
                 } else {\n\
                     consume(s);\n\
                 }\n\
                 0\n\
             }";
        let consume_cfg = build_cfg_for(source, 0);
        let main_cfg = build_cfg_for(source, 1);
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
        let source = "fn eat(s: String) -> i32 { 0 }\n\
             struct H { a: String, b: String }\n\
             fn main() -> i32 {\n\
                 let h = H { a: String::with_capacity(8), b: String::with_capacity(8) };\n\
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
    fn test_partial_field_move_still_calls_struct_destructor() {
        // RUE-62: the struct's OWN destructor still runs for a partially
        // moved struct — emitted as a plain call (without the field glue).
        // main contains exactly two calls: eat(o.a) and O.__drop(o); and
        // zero Drops (field a moved out, field b is a trivially-droppable
        // i32, and field-granular dropping replaces the whole-struct Drop).
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
        let main_cfg = build_cfg_named(source, "main");
        assert_eq!(count_drops(&main_cfg), 0, "no whole-struct or field Drop");
        assert_eq!(
            count_calls(&main_cfg),
            2,
            "eat(o.a) plus the destructor-only call O.__drop(o)"
        );
        assert_all_blocks_terminated(&main_cfg);
    }

    #[test]
    fn test_field_reassignment_restores_whole_struct_drop() {
        // Writing a fresh value into a moved-out field re-initializes it:
        // the per-field moved state is cleared by the projected PlaceWrite,
        // so scope exit is back on the whole-struct fast path — ONE Drop
        // whose operand is a whole-slot Load (covering both fields via the
        // drop glue), not a field-granular PlaceRead drop of just field b.
        let source = "fn eat(s: String) -> i32 { 0 }\n\
             struct H { a: String, b: String }\n\
             fn main() -> i32 {\n\
                 let mut h = H { a: String::with_capacity(8), b: String::with_capacity(8) };\n\
                 eat(h.a);\n\
                 h.a = String::with_capacity(4);\n\
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
}
