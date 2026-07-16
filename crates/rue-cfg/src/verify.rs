//! CFG structural verifier (RUE-227).
//!
//! After the CFG is built (and optimized) but before it is lowered to machine
//! code, this pass asserts the structural invariants that codegen relies on.
//! Its whole purpose is to convert a *class* of latent lowering bugs — most
//! notoriously "block has no terminator", which historically surfaced as a
//! `SIGABRT` deep inside `cfg_lower.rs` (RUE-213 / RUE-217 / RUE-224) — into a
//! **loud, early, localized** compiler-bug report naming the function and the
//! offending block.
//!
//! # Why a hard panic (not a `debug_assert`)
//!
//! A CFG that violates these invariants is malformed AIR→CFG output: continuing
//! anyway miscompiles the program (wrong ABI, double-free, silent corruption).
//! Per RUE-45 the guard must fire in **release** builds too, so this uses
//! `panic!`/`assert!`, never `debug_assert!`.
//!
//! # Invariants checked
//!
//! Every arena value has exactly one legal attachment, block parameters agree
//! with their attachment metadata, and every operand is defined before use and
//! dominated by its definition. Targets, variable-length storage slices,
//! local/parameter slots, places, projections, edge arguments, conditions, and
//! returns are validated before any getter or graph traversal can index them.
//!
//! These checks apply to unreachable blocks too. The sole reachability-based
//! exemption is an unreachable block's `None` terminator: construction can
//! leave an orphan block unfinished, but all of its existing contents still
//! have to be structurally valid. Optimization runs strict verification before
//! DCE can detach dead arena values, then verifies the remaining live graph
//! again after all passes.

use crate::inst::{BlockId, Cfg, CfgInstData, CfgValue, Place, PlaceBase, Projection, Terminator};
use rue_air::{FrozenTypeInternPool, Type, TypeKind};

impl Cfg {
    /// Count every operand use of `needle` across instructions and
    /// terminators.
    ///
    /// The instruction walk mirrors the verifier's exhaustive operand list so
    /// provenance-sensitive consumers retain the established counting behavior.
    pub fn value_use_count(&self, needle: CfgValue) -> usize {
        let mut count = 0;
        for block in self.blocks() {
            for &value in &block.insts {
                let mut operands = Vec::new();
                self.collect_inst_operands(&self.get_inst(value).data, &mut operands);
                count += operands
                    .iter()
                    .filter(|operand| **operand == needle)
                    .count();
            }
            match &block.terminator {
                Terminator::Goto {
                    args_start,
                    args_len,
                    ..
                } => {
                    count += self
                        .get_extra(*args_start, *args_len)
                        .iter()
                        .filter(|value| **value == needle)
                        .count();
                }
                Terminator::Branch {
                    cond,
                    then_args_start,
                    then_args_len,
                    else_args_start,
                    else_args_len,
                    ..
                } => {
                    count += usize::from(*cond == needle);
                    count += self
                        .get_extra(*then_args_start, *then_args_len)
                        .iter()
                        .filter(|value| **value == needle)
                        .count();
                    count += self
                        .get_extra(*else_args_start, *else_args_len)
                        .iter()
                        .filter(|value| **value == needle)
                        .count();
                }
                Terminator::Switch { scrutinee, .. } => {
                    count += usize::from(*scrutinee == needle);
                }
                Terminator::Return { value } => {
                    count += usize::from(*value == Some(needle));
                }
                Terminator::Unreachable | Terminator::None => {}
            }
        }
        count
    }

    /// Verify the CFG's structural invariants, panicking with a precise,
    /// function- and block-localized message on any violation.
    ///
    /// See the module docs for the invariants and the rationale for the hard
    /// panic. This is a compiler-bug guard: a well-formed pipeline never trips
    /// it; the check is paid to pin future lowering regressions to their source.
    pub fn verify(&self) {
        Verifier::new(self, None, true).verify();
    }

    /// Verify with the active semantic type pool, enabling exact aggregate
    /// layouts and projection-chain validation. Production callers must use
    /// this entry point.
    pub fn verify_with_type_pool(&self, type_pool: &FrozenTypeInternPool) {
        Verifier::new(self, Some(type_pool), true).verify();
    }

    /// Verify the optimized live graph. DCE intentionally retains dead values
    /// in the arena after detaching them from blocks, so attachment completeness
    /// is checked before optimization while every remaining attachment and use
    /// is checked again afterward.
    pub(crate) fn verify_after_optimization_with_type_pool(
        &self,
        type_pool: &FrozenTypeInternPool,
    ) {
        Verifier::new(self, Some(type_pool), false).verify();
    }

    /// Collect every `CfgValue` operand referenced by an instruction into
    /// `out`. This mirrors the `CfgInstData` variants; a new variant with value
    /// operands must be added here so the verifier keeps seeing all references.
    fn collect_inst_operands(&self, data: &CfgInstData, out: &mut Vec<CfgValue>) {
        match data {
            // No value operands.
            CfgInstData::Const(_)
            | CfgInstData::BoolConst(_)
            | CfgInstData::StringConst(_)
            | CfgInstData::Param { .. }
            | CfgInstData::BlockParam { .. }
            | CfgInstData::Load { .. }
            | CfgInstData::StorageLive { .. }
            | CfgInstData::StorageDead { .. } => {}

            // Binary operations.
            CfgInstData::Add(a, b)
            | CfgInstData::Sub(a, b)
            | CfgInstData::Mul(a, b)
            | CfgInstData::Div(a, b)
            | CfgInstData::Mod(a, b)
            | CfgInstData::Eq(a, b)
            | CfgInstData::Ne(a, b)
            | CfgInstData::Lt(a, b)
            | CfgInstData::Gt(a, b)
            | CfgInstData::Le(a, b)
            | CfgInstData::Ge(a, b)
            | CfgInstData::BitAnd(a, b)
            | CfgInstData::BitOr(a, b)
            | CfgInstData::BitXor(a, b)
            | CfgInstData::Shl(a, b)
            | CfgInstData::Shr(a, b) => {
                out.push(*a);
                out.push(*b);
            }

            // Unary operations.
            CfgInstData::Neg(v) | CfgInstData::Not(v) | CfgInstData::BitNot(v) => out.push(*v),

            CfgInstData::Alloc { init, .. } => out.push(*init),
            CfgInstData::Store { value, .. } => out.push(*value),
            CfgInstData::ParamStore { value, .. } => out.push(*value),

            CfgInstData::PlaceRead { place } => self.collect_place_operands(place, out),
            CfgInstData::PlaceWrite { place, value } => {
                self.collect_place_operands(place, out);
                out.push(*value);
            }

            CfgInstData::Call {
                args_start,
                args_len,
                ..
            } => {
                for arg in self.get_call_args(*args_start, *args_len) {
                    out.push(arg.value);
                }
            }
            CfgInstData::Intrinsic {
                args_start,
                args_len,
                ..
            } => out.extend_from_slice(self.get_extra(*args_start, *args_len)),

            CfgInstData::StructInit {
                fields_start,
                fields_len,
                ..
            } => out.extend_from_slice(self.get_extra(*fields_start, *fields_len)),
            CfgInstData::ArrayInit {
                elements_start,
                elements_len,
            } => out.extend_from_slice(self.get_extra(*elements_start, *elements_len)),
            CfgInstData::EnumVariant {
                payload_start,
                payload_len,
                ..
            } => out.extend_from_slice(self.get_extra(*payload_start, *payload_len)),
            CfgInstData::EnumPayloadGet { base, .. } => out.push(*base),

            CfgInstData::IntCast { value, .. } => out.push(*value),
            CfgInstData::Drop { value } => out.push(*value),
        }
    }

    /// Collect the `CfgValue` operands hiding inside a place's `Index`
    /// projections.
    fn collect_place_operands(&self, place: &crate::inst::Place, out: &mut Vec<CfgValue>) {
        for proj in self.get_place_projections(place) {
            if let Projection::Index { index, .. } = proj {
                out.push(*index);
            }
        }
    }
}

#[derive(Clone, Copy)]
enum Attachment {
    Param { block: BlockId },
    Inst { block: BlockId, position: usize },
}

struct Verifier<'a> {
    cfg: &'a Cfg,
    type_pool: Option<&'a FrozenTypeInternPool>,
    require_complete_attachments: bool,
    attachments: Vec<Option<Attachment>>,
    reachable: Vec<bool>,
    dominators: Vec<Vec<bool>>,
}

impl<'a> Verifier<'a> {
    fn new(
        cfg: &'a Cfg,
        type_pool: Option<&'a FrozenTypeInternPool>,
        require_complete_attachments: bool,
    ) -> Self {
        Self {
            cfg,
            type_pool,
            require_complete_attachments,
            attachments: vec![None; cfg.value_count()],
            reachable: vec![false; cfg.block_count()],
            dominators: Vec::new(),
        }
    }

    fn fail(&self, message: impl std::fmt::Display) -> ! {
        panic!(
            "internal compiler error: CFG verification failed in function `{}`: {}. This is a compiler bug (RUE-797).",
            self.cfg.fn_name(),
            message
        )
    }

    fn verify(mut self) {
        self.verify_block_table_and_attachments();
        self.verify_targets_and_slices();
        self.compute_reachability();
        self.compute_dominators();

        for block in self.cfg.blocks() {
            let block_index = block.id.as_u32() as usize;
            if self.reachable[block_index] && matches!(block.terminator, Terminator::None) {
                self.fail(format_args!(
                    "reachable block {} has no terminator",
                    block.id
                ));
            }

            for (position, &value) in block.insts.iter().enumerate() {
                self.verify_inst(block.id, position, value);
            }
            if !matches!(block.terminator, Terminator::None) {
                self.verify_terminator_use(block.id, &block.terminator);
            }
        }
    }

    fn verify_block_table_and_attachments(&mut self) {
        let block_count = self.cfg.block_count();
        if self.cfg.entry.as_u32() as usize >= block_count {
            self.fail(format_args!(
                "entry block {} is out of bounds (only {} blocks exist)",
                self.cfg.entry, block_count
            ));
        }

        for (expected, block) in self.cfg.blocks().iter().enumerate() {
            if block.id.as_u32() as usize != expected {
                self.fail(format_args!(
                    "block table slot {} contains mismatched id {}",
                    expected, block.id
                ));
            }
            for (index, &(value, stored_ty)) in block.params.iter().enumerate() {
                self.attach(
                    value,
                    Attachment::Param { block: block.id },
                    "block parameter",
                );
                let inst = self.inst(value, block.id, "block parameter");
                match inst.data {
                    CfgInstData::BlockParam { index: actual } if actual == index as u32 => {}
                    CfgInstData::BlockParam { index: actual } => self.fail(format_args!(
                        "block parameter {} in block {} is stored at index {} but declares index {}",
                        value, block.id, index, actual
                    )),
                    ref other => self.fail(format_args!(
                        "block parameter {} in block {} has non-BlockParam data {:?}",
                        value, block.id, other
                    )),
                }
                if inst.ty != stored_ty {
                    self.fail(format_args!(
                        "block parameter {} in block {} stores type {:?} but its value has type {:?}",
                        value, block.id, stored_ty, inst.ty
                    ));
                }
            }
            for (position, &value) in block.insts.iter().enumerate() {
                self.attach(
                    value,
                    Attachment::Inst {
                        block: block.id,
                        position,
                    },
                    "instruction",
                );
                if matches!(
                    self.inst(value, block.id, "instruction").data,
                    CfgInstData::BlockParam { .. }
                ) {
                    self.fail(format_args!(
                        "ordinary instruction {} in block {} has BlockParam data",
                        value, block.id
                    ));
                }
            }
        }

        if self.require_complete_attachments {
            for (index, attachment) in self.attachments.iter().enumerate() {
                if attachment.is_none() {
                    self.fail(format_args!(
                        "value v{} is unattached (every value must be exactly one block parameter or ordinary instruction)",
                        index
                    ));
                }
            }
        }
    }

    fn attach(&mut self, value: CfgValue, attachment: Attachment, role: &str) {
        let index = value.as_u32() as usize;
        if index >= self.attachments.len() {
            self.fail(format_args!(
                "{} {} references an undefined value (only {} values exist)",
                role,
                value,
                self.attachments.len()
            ));
        }
        if let Some(previous) = self.attachments[index] {
            self.fail(format_args!(
                "value {} has duplicate attachments ({}, then {})",
                value,
                Self::attachment_name(previous),
                Self::attachment_name(attachment)
            ));
        }
        self.attachments[index] = Some(attachment);
    }

    fn attachment_name(attachment: Attachment) -> String {
        match attachment {
            Attachment::Param { block } => format!("parameter in {block}"),
            Attachment::Inst { block, position } => {
                format!("instruction {position} in {block}")
            }
        }
    }

    fn inst(&self, value: CfgValue, block: BlockId, role: &str) -> &crate::inst::CfgInst {
        if value.as_u32() as usize >= self.cfg.value_count() {
            self.fail(format_args!(
                "{} {} in block {} is undefined (only {} values exist)",
                role,
                value,
                block,
                self.cfg.value_count()
            ));
        }
        self.cfg.get_inst(value)
    }

    fn verify_targets_and_slices(&self) {
        for block in self.cfg.blocks() {
            for &value in &block.insts {
                let data = &self.inst(value, block.id, "instruction").data;
                match data {
                    CfgInstData::Call {
                        args_start,
                        args_len,
                        ..
                    } => self.check_range(
                        *args_start,
                        *args_len,
                        self.cfg.call_args_len(),
                        block.id,
                        value,
                        "call-argument",
                    ),
                    CfgInstData::Intrinsic {
                        args_start,
                        args_len,
                        ..
                    } => self.check_extra(*args_start, *args_len, block.id, value, "intrinsic"),
                    CfgInstData::StructInit {
                        fields_start,
                        fields_len,
                        ..
                    } => self.check_extra(
                        *fields_start,
                        *fields_len,
                        block.id,
                        value,
                        "struct fields",
                    ),
                    CfgInstData::ArrayInit {
                        elements_start,
                        elements_len,
                    } => self.check_extra(
                        *elements_start,
                        *elements_len,
                        block.id,
                        value,
                        "array elements",
                    ),
                    CfgInstData::EnumVariant {
                        payload_start,
                        payload_len,
                        ..
                    } => self.check_extra(
                        *payload_start,
                        *payload_len,
                        block.id,
                        value,
                        "enum payload",
                    ),
                    CfgInstData::PlaceRead { place } | CfgInstData::PlaceWrite { place, .. } => {
                        self.check_range(
                            place.proj_start,
                            place.proj_len,
                            self.cfg.projections_len(),
                            block.id,
                            value,
                            "projection",
                        )
                    }
                    _ => {}
                }
            }
            match &block.terminator {
                Terminator::Goto {
                    target,
                    args_start,
                    args_len,
                } => {
                    self.check_target(block.id, *target, "goto");
                    self.check_term_extra(*args_start, *args_len, block.id, "goto arguments");
                }
                Terminator::Branch {
                    then_block,
                    then_args_start,
                    then_args_len,
                    else_block,
                    else_args_start,
                    else_args_len,
                    ..
                } => {
                    self.check_target(block.id, *then_block, "branch then");
                    self.check_target(block.id, *else_block, "branch else");
                    self.check_term_extra(
                        *then_args_start,
                        *then_args_len,
                        block.id,
                        "branch then arguments",
                    );
                    self.check_term_extra(
                        *else_args_start,
                        *else_args_len,
                        block.id,
                        "branch else arguments",
                    );
                }
                Terminator::Switch {
                    cases_start,
                    cases_len,
                    default,
                    ..
                } => {
                    self.check_range(
                        *cases_start,
                        *cases_len,
                        self.cfg.switch_cases_len(),
                        block.id,
                        CfgValue::from_raw(0),
                        "switch-case",
                    );
                    for &(_, target) in self.cfg.get_switch_cases(*cases_start, *cases_len) {
                        self.check_target(block.id, target, "switch case");
                    }
                    self.check_target(block.id, *default, "switch default");
                }
                Terminator::Return { .. } | Terminator::Unreachable | Terminator::None => {}
            }
        }
    }

    fn check_target(&self, from: BlockId, target: BlockId, role: &str) {
        if target.as_u32() as usize >= self.cfg.block_count() {
            self.fail(format_args!(
                "{} target {} from block {} is out of bounds (only {} blocks exist)",
                role,
                target,
                from,
                self.cfg.block_count()
            ));
        }
    }

    fn check_extra(&self, start: u32, len: u32, block: BlockId, value: CfgValue, role: &str) {
        self.check_range(start, len, self.cfg.extra_len(), block, value, role);
    }

    fn check_term_extra(&self, start: u32, len: u32, block: BlockId, role: &str) {
        let end = start.checked_add(len).map(|v| v as usize);
        if end.is_none_or(|end| start as usize > end || end > self.cfg.extra_len()) {
            self.fail(format_args!(
                "{} in terminator of block {} uses out-of-bounds slice {}..{} (storage length {})",
                role,
                block,
                start,
                end.map_or_else(|| "overflow".to_string(), |v| v.to_string()),
                self.cfg.extra_len()
            ));
        }
    }

    fn check_range(
        &self,
        start: u32,
        len: u32,
        storage_len: usize,
        block: BlockId,
        value: CfgValue,
        role: &str,
    ) {
        let end = start.checked_add(len).map(|v| v as usize);
        if end.is_none_or(|end| start as usize > end || end > storage_len) {
            self.fail(format_args!(
                "{} slice for instruction {} in block {} is out of bounds: {}..{} (storage length {})",
                role,
                value,
                block,
                start,
                end.map_or_else(|| "overflow".to_string(), |v| v.to_string()),
                storage_len
            ));
        }
    }

    fn compute_reachability(&mut self) {
        let mut stack = vec![self.cfg.entry];
        while let Some(id) = stack.pop() {
            let index = id.as_u32() as usize;
            if self.reachable[index] {
                continue;
            }
            self.reachable[index] = true;
            self.for_each_successor(id, |successor| stack.push(successor));
        }
    }

    fn for_each_successor(&self, block: BlockId, mut f: impl FnMut(BlockId)) {
        match &self.cfg.get_block(block).terminator {
            Terminator::Goto { target, .. } => f(*target),
            Terminator::Branch {
                then_block,
                else_block,
                ..
            } => {
                f(*then_block);
                f(*else_block);
            }
            Terminator::Switch {
                cases_start,
                cases_len,
                default,
                ..
            } => {
                for &(_, target) in self.cfg.get_switch_cases(*cases_start, *cases_len) {
                    f(target);
                }
                f(*default);
            }
            Terminator::Return { .. } | Terminator::Unreachable | Terminator::None => {}
        }
    }

    fn compute_dominators(&mut self) {
        let count = self.cfg.block_count();
        let mut predecessors = vec![Vec::new(); count];
        for block in self.cfg.blocks() {
            if self.reachable[block.id.as_u32() as usize] {
                self.for_each_successor(block.id, |successor| {
                    predecessors[successor.as_u32() as usize].push(block.id)
                });
            }
        }
        self.dominators = vec![vec![false; count]; count];
        for block in self.cfg.blocks() {
            let index = block.id.as_u32() as usize;
            if !self.reachable[index] {
                continue;
            }
            if block.id == self.cfg.entry {
                self.dominators[index][index] = true;
            } else {
                for candidate in 0..count {
                    self.dominators[index][candidate] = self.reachable[candidate];
                }
            }
        }
        loop {
            let mut changed = false;
            for block in self.cfg.blocks() {
                let index = block.id.as_u32() as usize;
                if !self.reachable[index] || block.id == self.cfg.entry {
                    continue;
                }
                let mut next = vec![true; count];
                if predecessors[index].is_empty() {
                    next.fill(false);
                } else {
                    for pred in &predecessors[index] {
                        for (candidate, present) in next.iter_mut().enumerate() {
                            *present &= self.dominators[pred.as_u32() as usize][candidate];
                        }
                    }
                }
                next[index] = true;
                if next != self.dominators[index] {
                    self.dominators[index] = next;
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
    }

    fn verify_inst(&self, block: BlockId, position: usize, value: CfgValue) {
        let inst = self.inst(value, block, "instruction");
        match &inst.data {
            CfgInstData::Param { index } => {
                self.check_param_slot(*index, inst.ty, block, value, "Param")
            }
            CfgInstData::Alloc { slot, init } => self.check_local_slot(
                *slot,
                self.inst(*init, block, "allocation initializer").ty,
                block,
                value,
                "Alloc",
            ),
            CfgInstData::Load { slot } => {
                self.check_local_slot(*slot, inst.ty, block, value, "Load")
            }
            CfgInstData::Store {
                slot,
                value: stored,
            } => self.check_local_slot(
                *slot,
                self.inst(*stored, block, "stored value").ty,
                block,
                value,
                "Store",
            ),
            CfgInstData::StorageLive { slot, local_ty } => {
                self.check_local_slot(*slot, *local_ty, block, value, "StorageLive")
            }
            CfgInstData::StorageDead { slot, local_ty } => {
                self.check_local_slot(*slot, *local_ty, block, value, "StorageDead")
            }
            CfgInstData::ParamStore {
                param_slot,
                value: stored,
            } => self.check_param_slot(
                *param_slot,
                self.inst(*stored, block, "stored parameter value").ty,
                block,
                value,
                "ParamStore",
            ),
            CfgInstData::PlaceRead { place } | CfgInstData::PlaceWrite { place, .. } => {
                self.verify_place(block, value, place)
            }
            CfgInstData::BlockParam { .. } => unreachable!(),
            _ => {}
        }
        self.for_each_inst_operand(block, value, &inst.data, |operand, role| {
            self.verify_use(block, Some(position), operand, role)
        });
    }

    fn check_local_slot(&self, slot: u32, ty: Type, block: BlockId, value: CfgValue, role: &str) {
        let width = self.abi_slot_count(ty, block, value, role);
        let end = slot.checked_add(width);
        if end.is_none_or(|end| end > self.cfg.num_locals()) {
            self.fail(format_args!(
                "{} instruction {} in block {} uses local slot range {}..{} for type {:?}, but only {} local slots exist",
                role,
                value,
                block,
                slot,
                end.map_or_else(|| "overflow".to_string(), |end| end.to_string()),
                ty,
                self.cfg.num_locals()
            ));
        }
    }

    fn check_param_slot(&self, slot: u32, ty: Type, block: BlockId, value: CfgValue, role: &str) {
        // Borrowed and inout parameters carry one physical pointer slot even
        // when their logical type is a multi-slot aggregate. Places retain the
        // logical type so projection validation can follow the pointee shape.
        let width = if self.cfg.is_param_by_ref(slot) {
            1
        } else {
            self.abi_slot_count(ty, block, value, role)
        };
        let end = slot.checked_add(width);
        if end.is_none_or(|end| end > self.cfg.num_params()) {
            self.fail(format_args!(
                "{} instruction {} in block {} uses parameter slot range {}..{} for type {:?}, but only {} parameter slots exist",
                role,
                value,
                block,
                slot,
                end.map_or_else(|| "overflow".to_string(), |end| end.to_string()),
                ty,
                self.cfg.num_params()
            ));
        }
    }

    fn abi_slot_count(&self, ty: Type, block: BlockId, value: CfgValue, role: &str) -> u32 {
        match ty.kind() {
            TypeKind::I8
            | TypeKind::I16
            | TypeKind::I32
            | TypeKind::I64
            | TypeKind::U8
            | TypeKind::U16
            | TypeKind::U32
            | TypeKind::U64
            | TypeKind::Bool
            | TypeKind::Error
            | TypeKind::PtrConst(_)
            | TypeKind::PtrMut(_) => 1,
            TypeKind::Unit | TypeKind::Never | TypeKind::ComptimeType | TypeKind::Module(_) => 0,
            TypeKind::Struct(struct_id) => {
                let Some(pool) = self.type_pool else {
                    return 0;
                };
                let Some(def) = pool.try_struct_def(struct_id) else {
                    self.fail(format_args!(
                        "{} instruction {} in block {} references invalid struct type {:?}",
                        role, value, block, struct_id
                    ));
                };
                def.fields.iter().fold(0u32, |total, field| {
                    total.saturating_add(self.abi_slot_count(field.ty, block, value, role))
                })
            }
            TypeKind::Array(array_id) => {
                let Some(pool) = self.type_pool else {
                    return 0;
                };
                let Some((element, len)) = pool.try_array_def(array_id) else {
                    self.fail(format_args!(
                        "{} instruction {} in block {} references invalid array type {:?}",
                        role, value, block, array_id
                    ));
                };
                let element_width = u64::from(self.abi_slot_count(element, block, value, role));
                u32::try_from(element_width.saturating_mul(len)).unwrap_or(u32::MAX)
            }
            TypeKind::Enum(enum_id) => {
                let Some(pool) = self.type_pool else {
                    return 0;
                };
                let Some(def) = pool.try_enum_def(enum_id) else {
                    self.fail(format_args!(
                        "{} instruction {} in block {} references invalid enum type {:?}",
                        role, value, block, enum_id
                    ));
                };
                let payload_width = (0..def.variant_count())
                    .map(|index| {
                        def.variant_payload(index).iter().fold(0u32, |total, &ty| {
                            total.saturating_add(self.abi_slot_count(ty, block, value, role))
                        })
                    })
                    .max()
                    .unwrap_or(0);
                1u32.saturating_add(payload_width)
            }
        }
    }

    fn is_fixed_str_to_view_coercion(&self, source: Type, result: Type) -> bool {
        let Some(pool) = self.type_pool else {
            return false;
        };
        let (TypeKind::Struct(source_id), TypeKind::Struct(result_id)) =
            (source.kind(), result.kind())
        else {
            return false;
        };
        let (Some(source_def), Some(result_def)) = (
            pool.try_struct_def(source_id),
            pool.try_struct_def(result_id),
        ) else {
            return false;
        };
        let is_fixed_str = source_def
            .name
            .strip_prefix("Str(")
            .and_then(|rest| rest.strip_suffix(')'))
            .is_some_and(|capacity| capacity.parse::<u64>().is_ok());
        is_fixed_str
            && result_def.name == "str"
            && source_def.fields.len() == result_def.fields.len()
            && source_def
                .fields
                .iter()
                .zip(&result_def.fields)
                .all(|(source, result)| source.ty == result.ty)
    }

    fn verify_place(&self, block: BlockId, value: CfgValue, place: &Place) {
        match place.base {
            PlaceBase::Local(slot) => {
                self.check_local_slot(slot, place.base_type, block, value, "place")
            }
            PlaceBase::Param(slot) => {
                self.check_param_slot(slot, place.base_type, block, value, "place")
            }
        }
        let projections = self.cfg.get_place_projections(place);
        if let Some(pool) = self.type_pool {
            let mut current_ty = place.base_type;
            for (projection_index, projection) in projections.iter().enumerate() {
                current_ty = match projection {
                    Projection::Field {
                        struct_id,
                        field_index,
                    } => {
                        let Some(def) = pool.try_struct_def(*struct_id) else {
                            self.fail(format_args!(
                                "projection {} in place instruction {} in block {} references invalid struct id {:?}",
                                projection_index, value, block, struct_id
                            ));
                        };
                        let expected = Type::new_struct(*struct_id);
                        if current_ty != expected {
                            self.fail(format_args!(
                                "projection {} in place instruction {} in block {} expects container type {:?}, but previous link produced {:?}",
                                projection_index, value, block, expected, current_ty
                            ));
                        }
                        let Some(field) = def.fields.get(*field_index as usize) else {
                            self.fail(format_args!(
                                "projection {} in place instruction {} in block {} references field {} of struct {:?}, which has {} fields",
                                projection_index,
                                value,
                                block,
                                field_index,
                                struct_id,
                                def.fields.len()
                            ));
                        };
                        field.ty
                    }
                    Projection::Index { array_type, .. } => {
                        let TypeKind::Array(array_id) = array_type.kind() else {
                            self.fail(format_args!(
                                "projection {} in place instruction {} in block {} has non-array container type {:?}",
                                projection_index, value, block, array_type
                            ));
                        };
                        let Some((element_ty, _)) = pool.try_array_def(array_id) else {
                            self.fail(format_args!(
                                "projection {} in place instruction {} in block {} references invalid array id {:?}",
                                projection_index, value, block, array_id
                            ));
                        };
                        if current_ty != *array_type {
                            self.fail(format_args!(
                                "projection {} in place instruction {} in block {} expects container type {:?}, but previous link produced {:?}",
                                projection_index, value, block, array_type, current_ty
                            ));
                        }
                        element_ty
                    }
                };
            }

            let inst = self.cfg.get_inst(value);
            match &inst.data {
                CfgInstData::PlaceRead { .. }
                    if inst.ty != current_ty
                        && !self.is_fixed_str_to_view_coercion(current_ty, inst.ty) =>
                {
                    self.fail(format_args!(
                        "place-read instruction {} in block {} has result type {:?}, but its projection chain produces {:?}",
                        value, block, inst.ty, current_ty
                    ));
                }
                CfgInstData::PlaceWrite { value: stored, .. } => {
                    let stored_ty = self.inst(*stored, block, "place-write value").ty;
                    if stored_ty != current_ty {
                        self.fail(format_args!(
                            "place-write instruction {} in block {} stores type {:?}, but its projection chain produces {:?}",
                            value, block, stored_ty, current_ty
                        ));
                    }
                    if inst.ty != Type::UNIT {
                        self.fail(format_args!(
                            "place-write instruction {} in block {} has result type {:?}, expected unit",
                            value, block, inst.ty
                        ));
                    }
                }
                _ => {}
            }
        }

        if let Some(first) = projections.first() {
            let required = match first {
                Projection::Field { struct_id, .. } => Type::new_struct(*struct_id),
                Projection::Index { array_type, .. } => {
                    if !matches!(array_type.kind(), TypeKind::Array(_)) {
                        self.fail(format_args!(
                            "place in instruction {} in block {} has Index projection with non-array container type {:?}",
                            value, block, array_type
                        ));
                    }
                    *array_type
                }
            };
            if place.base_type != required {
                self.fail(format_args!(
                    "place in instruction {} in block {} has logical base type {:?}, but its first projection requires base type {:?}",
                    value, block, place.base_type, required
                ));
            }
        }
        for projection in projections {
            if let Projection::Index { array_type, index } = projection {
                if !matches!(array_type.kind(), TypeKind::Array(_)) {
                    self.fail(format_args!(
                        "place in instruction {} in block {} has Index projection with non-array container type {:?}",
                        value, block, array_type
                    ));
                }
                let index_ty = self.inst(*index, block, "projection index").ty;
                if !matches!(
                    index_ty.kind(),
                    TypeKind::I8
                        | TypeKind::I16
                        | TypeKind::I32
                        | TypeKind::I64
                        | TypeKind::U8
                        | TypeKind::U16
                        | TypeKind::U32
                        | TypeKind::U64
                ) {
                    self.fail(format_args!(
                        "projection index {} used by instruction {} in block {} has non-integer type {:?}",
                        index, value, block, index_ty
                    ));
                }
            }
        }
    }

    fn verify_terminator_use(&self, block: BlockId, term: &Terminator) {
        match term {
            Terminator::Goto {
                target,
                args_start,
                args_len,
            } => {
                let args = self.cfg.get_extra(*args_start, *args_len);
                for &arg in args {
                    self.verify_use(block, None, arg, "goto argument");
                }
                self.verify_edge(block, *target, args);
            }
            Terminator::Branch {
                cond,
                then_block,
                then_args_start,
                then_args_len,
                else_block,
                else_args_start,
                else_args_len,
            } => {
                self.verify_use(block, None, *cond, "branch condition");
                if self.inst(*cond, block, "branch condition").ty != Type::BOOL {
                    self.fail(format_args!(
                        "branch condition {} in block {} has type {:?}, expected bool",
                        cond,
                        block,
                        self.cfg.get_inst(*cond).ty
                    ));
                }
                let then_args = self.cfg.get_extra(*then_args_start, *then_args_len);
                for &arg in then_args {
                    self.verify_use(block, None, arg, "branch-then argument");
                }
                self.verify_edge(block, *then_block, then_args);
                let else_args = self.cfg.get_extra(*else_args_start, *else_args_len);
                for &arg in else_args {
                    self.verify_use(block, None, arg, "branch-else argument");
                }
                self.verify_edge(block, *else_block, else_args);
            }
            Terminator::Switch {
                scrutinee,
                cases_start,
                cases_len,
                default,
            } => {
                self.verify_use(block, None, *scrutinee, "switch scrutinee");
                for &(_, target) in self.cfg.get_switch_cases(*cases_start, *cases_len) {
                    self.verify_edge(block, target, &[]);
                }
                self.verify_edge(block, *default, &[]);
            }
            Terminator::Return { value } => match (self.cfg.return_type(), value) {
                (Type::UNIT, None) => {}
                (Type::UNIT, Some(value)) => self.fail(format_args!(
                    "return in block {} supplies unit value {}; unit-returning functions must use Return {{ value: None }}",
                    block, value
                )),
                (return_ty, None) => self.fail(format_args!(
                    "return in block {} has no value but function return type is {:?}",
                    block, return_ty
                )),
                (return_ty, Some(value)) => {
                    self.verify_use(block, None, *value, "return value");
                    let value_ty = self.cfg.get_inst(*value).ty;
                    if value_ty != return_ty {
                        self.fail(format_args!(
                            "return value {} in block {} has type {:?}, expected {:?}",
                            value, block, value_ty, return_ty
                        ));
                    }
                }
            },
            Terminator::Unreachable | Terminator::None => {}
        }
    }

    fn verify_edge(&self, from: BlockId, to: BlockId, args: &[CfgValue]) {
        let params = &self.cfg.get_block(to).params;
        if params.len() != args.len() {
            self.fail(format_args!(
                "edge {} -> {} passes {} block arguments but target expects {}",
                from,
                to,
                args.len(),
                params.len()
            ));
        }
        for (index, (&arg, &(_, param_ty))) in args.iter().zip(params).enumerate() {
            let arg_ty = self.cfg.get_inst(arg).ty;
            if arg_ty != param_ty {
                self.fail(format_args!(
                    "edge {} -> {} argument {} ({}) has type {:?}, target parameter has type {:?} (ill-typed edge)",
                    from, to, index, arg, arg_ty, param_ty
                ));
            }
        }
    }

    fn verify_use(
        &self,
        use_block: BlockId,
        use_position: Option<usize>,
        value: CfgValue,
        role: &str,
    ) {
        let _ = self.inst(value, use_block, role);
        let Some(attachment) = self.attachments[value.as_u32() as usize] else {
            self.fail(format_args!(
                "{} {} in block {} refers to an unattached value",
                role, value, use_block
            ));
        };
        match attachment {
            Attachment::Param { block } if block == use_block => {}
            Attachment::Inst { block, position } if block == use_block => {
                if use_position.is_some_and(|use_position| position >= use_position) {
                    self.fail(format_args!(
                        "{} {} in block {} is used before its definition at instruction position {}",
                        role, value, use_block, position
                    ));
                }
            }
            Attachment::Param { block: def_block }
            | Attachment::Inst {
                block: def_block, ..
            } => {
                let use_index = use_block.as_u32() as usize;
                if self.reachable[use_index]
                    && !self.dominators[use_index][def_block.as_u32() as usize]
                {
                    self.fail(format_args!(
                        "{} {} in reachable block {} is defined in block {}, which does not dominate the use",
                        role, value, use_block, def_block
                    ));
                }
            }
        }
    }

    fn for_each_inst_operand(
        &self,
        block: BlockId,
        value: CfgValue,
        data: &CfgInstData,
        mut f: impl FnMut(CfgValue, &'static str),
    ) {
        match data {
            CfgInstData::Const(_)
            | CfgInstData::BoolConst(_)
            | CfgInstData::StringConst(_)
            | CfgInstData::Param { .. }
            | CfgInstData::BlockParam { .. }
            | CfgInstData::Load { .. }
            | CfgInstData::StorageLive { .. }
            | CfgInstData::StorageDead { .. } => {}
            CfgInstData::Add(a, b)
            | CfgInstData::Sub(a, b)
            | CfgInstData::Mul(a, b)
            | CfgInstData::Div(a, b)
            | CfgInstData::Mod(a, b)
            | CfgInstData::Eq(a, b)
            | CfgInstData::Ne(a, b)
            | CfgInstData::Lt(a, b)
            | CfgInstData::Gt(a, b)
            | CfgInstData::Le(a, b)
            | CfgInstData::Ge(a, b)
            | CfgInstData::BitAnd(a, b)
            | CfgInstData::BitOr(a, b)
            | CfgInstData::BitXor(a, b)
            | CfgInstData::Shl(a, b)
            | CfgInstData::Shr(a, b) => {
                f(*a, "left operand");
                f(*b, "right operand");
            }
            CfgInstData::Neg(v) | CfgInstData::Not(v) | CfgInstData::BitNot(v) => {
                f(*v, "unary operand")
            }
            CfgInstData::Alloc { init, .. } => f(*init, "allocation initializer"),
            CfgInstData::Store { value, .. } | CfgInstData::ParamStore { value, .. } => {
                f(*value, "stored value")
            }
            CfgInstData::PlaceRead { place } => {
                for projection in self.cfg.get_place_projections(place) {
                    if let Projection::Index { index, .. } = projection {
                        f(*index, "projection index");
                    }
                }
            }
            CfgInstData::PlaceWrite { place, value } => {
                for projection in self.cfg.get_place_projections(place) {
                    if let Projection::Index { index, .. } = projection {
                        f(*index, "projection index");
                    }
                }
                f(*value, "place-write value");
            }
            CfgInstData::Call {
                args_start,
                args_len,
                ..
            } => {
                for arg in self.cfg.get_call_args(*args_start, *args_len) {
                    f(arg.value, "call argument");
                }
            }
            CfgInstData::Intrinsic {
                args_start,
                args_len,
                ..
            } => {
                for &operand in self.cfg.get_extra(*args_start, *args_len) {
                    f(operand, "intrinsic argument");
                }
            }
            CfgInstData::StructInit {
                fields_start,
                fields_len,
                ..
            } => {
                for &operand in self.cfg.get_extra(*fields_start, *fields_len) {
                    f(operand, "struct field");
                }
            }
            CfgInstData::ArrayInit {
                elements_start,
                elements_len,
            } => {
                for &operand in self.cfg.get_extra(*elements_start, *elements_len) {
                    f(operand, "array element");
                }
            }
            CfgInstData::EnumVariant {
                payload_start,
                payload_len,
                ..
            } => {
                for &operand in self.cfg.get_extra(*payload_start, *payload_len) {
                    f(operand, "enum payload");
                }
            }
            CfgInstData::EnumPayloadGet { base, .. } => f(*base, "enum base"),
            CfgInstData::IntCast { value, .. } => f(*value, "cast operand"),
            CfgInstData::Drop { value } => f(*value, "drop operand"),
        }
        let _ = (block, value);
    }
}

#[cfg(test)]
mod tests {
    use crate::inst::{
        BlockId, Cfg, CfgInst, CfgInstData, Place, PlaceBase, Projection, Terminator,
    };
    use crate::{OptLevel, opt};
    use lasso::ThreadedRodeo;
    use rue_air::{
        ArrayTypeId, FrozenTypeInternPool, StructDef, StructField, StructId, Type, TypeInternPool,
    };
    use rue_span::Span;

    fn unit_cfg() -> Cfg {
        Cfg::new(Type::I32, 0, 0, "test".to_string(), vec![])
    }

    fn register_struct(
        pool: &TypeInternPool,
        interner: &ThreadedRodeo,
        name: &str,
        field_types: &[Type],
    ) -> StructId {
        pool.register_struct(
            interner.get_or_intern(name),
            StructDef {
                name: name.to_string(),
                fields: field_types
                    .iter()
                    .enumerate()
                    .map(|(index, &ty)| StructField {
                        name: format!("field{index}"),
                        ty,
                    })
                    .collect(),
                is_copy: false,
                is_linear: false,
                destructor: None,
                is_builtin: false,
                is_pub: false,
                file_id: rue_span::FileId::DEFAULT,
            },
        )
        .0
    }

    #[test]
    fn verify_accepts_terminated_block() {
        let mut cfg = unit_cfg();
        let entry = cfg.new_block();
        cfg.entry = entry;
        let v = cfg.add_inst_to_block(
            entry,
            CfgInst {
                data: CfgInstData::Const(42),
                ty: Type::I32,
                span: Span::new(0, 2),
            },
        );
        cfg.set_terminator(entry, Terminator::Return { value: Some(v) });
        cfg.verify(); // must not panic
    }

    fn cfg_with_field_place(base_type: Type) -> Cfg {
        let mut cfg = Cfg::new(Type::I32, 1, 0, "field_place".to_string(), vec![]);
        let entry = cfg.new_block();
        cfg.entry = entry;
        let struct_id = StructId::from_pool_index(9);
        let place = cfg.make_place(
            PlaceBase::Local(0),
            base_type,
            [Projection::Field {
                struct_id,
                field_index: 0,
            }],
        );
        let read = cfg.add_inst_to_block(
            entry,
            CfgInst {
                data: CfgInstData::PlaceRead { place },
                ty: Type::I32,
                span: Span::new(0, 1),
            },
        );
        cfg.set_terminator(entry, Terminator::Return { value: Some(read) });
        cfg
    }

    #[test]
    fn verify_accepts_matching_place_base_type() {
        let struct_type = Type::new_struct(StructId::from_pool_index(9));
        cfg_with_field_place(struct_type).verify();
    }

    #[test]
    #[should_panic(expected = "first projection requires base type")]
    fn verify_rejects_field_projection_with_wrong_base_type() {
        cfg_with_field_place(Type::I32).verify();
    }

    #[test]
    #[should_panic(expected = "first projection requires base type")]
    fn verify_rejects_index_projection_with_wrong_base_type_on_write() {
        let mut cfg = Cfg::new(Type::UNIT, 1, 0, "index_place".to_string(), vec![]);
        let entry = cfg.new_block();
        cfg.entry = entry;
        let index = cfg.add_inst_to_block(
            entry,
            CfgInst {
                data: CfgInstData::Const(0),
                ty: Type::U64,
                span: Span::new(0, 1),
            },
        );
        let value = cfg.add_inst_to_block(
            entry,
            CfgInst {
                data: CfgInstData::Const(1),
                ty: Type::I32,
                span: Span::new(0, 1),
            },
        );
        let array_type = Type::new_array(ArrayTypeId::from_pool_index(10));
        let place = cfg.make_place(
            PlaceBase::Local(0),
            Type::I32,
            [Projection::Index { array_type, index }],
        );
        cfg.add_inst_to_block(
            entry,
            CfgInst {
                data: CfgInstData::PlaceWrite { place, value },
                ty: Type::UNIT,
                span: Span::new(0, 1),
            },
        );
        cfg.set_terminator(entry, Terminator::Return { value: None });

        cfg.verify();
    }

    #[test]
    #[should_panic(expected = "Index projection with non-array container type Type::I32")]
    fn verify_rejects_non_array_index_projection_type() {
        let mut cfg = Cfg::new(Type::I32, 1, 0, "index_type".to_string(), vec![]);
        let entry = cfg.new_block();
        cfg.entry = entry;
        let index = cfg.add_inst_to_block(
            entry,
            CfgInst {
                data: CfgInstData::Const(0),
                ty: Type::U64,
                span: Span::new(0, 1),
            },
        );
        let place = cfg.make_place(
            PlaceBase::Local(0),
            Type::I32,
            [Projection::Index {
                array_type: Type::I32,
                index,
            }],
        );
        let read = cfg.add_inst_to_block(
            entry,
            CfgInst {
                data: CfgInstData::PlaceRead { place },
                ty: Type::I32,
                span: Span::new(0, 1),
            },
        );
        cfg.set_terminator(entry, Terminator::Return { value: Some(read) });

        cfg.verify();
    }

    #[test]
    #[should_panic(expected = "has no terminator")]
    fn verify_catches_missing_terminator() {
        let mut cfg = unit_cfg();
        let entry = cfg.new_block();
        cfg.entry = entry;
        cfg.add_inst_to_block(
            entry,
            CfgInst {
                data: CfgInstData::Const(1),
                ty: Type::I32,
                span: Span::new(0, 1),
            },
        );
        // Deliberately leave the reachable entry block with Terminator::None.
        cfg.verify();
    }

    #[test]
    fn verify_ignores_unreachable_unterminated_block() {
        let mut cfg = unit_cfg();
        let entry = cfg.new_block();
        cfg.entry = entry;
        let value = cfg.add_inst_to_block(
            entry,
            CfgInst {
                data: CfgInstData::Const(0),
                ty: Type::I32,
                span: Span::new(0, 0),
            },
        );
        cfg.set_terminator(entry, Terminator::Return { value: Some(value) });
        // An orphan block, never wired in, with no terminator: must be skipped.
        let _orphan = cfg.new_block();
        cfg.verify();
    }

    #[test]
    #[should_panic(expected = "block arguments")]
    fn verify_catches_arity_mismatch() {
        let mut cfg = unit_cfg();
        let entry = cfg.new_block();
        cfg.entry = entry;
        let target = cfg.new_block();
        // Target expects one block parameter...
        let param = cfg.add_block_param(target, Type::I32);
        cfg.set_terminator(target, Terminator::Return { value: Some(param) });
        // ...but the goto edge passes zero arguments.
        cfg.set_terminator(
            entry,
            Terminator::Goto {
                target,
                args_start: 0,
                args_len: 0,
            },
        );
        cfg.verify();
    }

    /// Build `entry --goto(one arg of `arg_ty`)--> target(param: i32)`.
    fn cfg_with_typed_edge(arg_ty: Type) -> Cfg {
        let mut cfg = unit_cfg();
        let entry = cfg.new_block();
        cfg.entry = entry;
        let target = cfg.new_block();
        let param = cfg.add_block_param(target, Type::I32);
        cfg.set_terminator(target, Terminator::Return { value: Some(param) });
        let arg = cfg.add_inst_to_block(
            entry,
            CfgInst {
                data: CfgInstData::Const(0),
                ty: arg_ty,
                span: Span::new(0, 1),
            },
        );
        let (args_start, args_len) = cfg.push_extra(vec![arg]);
        cfg.set_terminator(
            entry,
            Terminator::Goto {
                target,
                args_start,
                args_len,
            },
        );
        cfg
    }

    #[test]
    fn verify_accepts_well_typed_edge() {
        cfg_with_typed_edge(Type::I32).verify(); // must not panic
    }

    #[test]
    #[should_panic(expected = "ill-typed edge")]
    fn verify_catches_edge_type_mismatch() {
        // The RUE-347 shape: a unit value passed into an i32 block parameter.
        cfg_with_typed_edge(Type::UNIT).verify();
    }

    #[test]
    #[should_panic(expected = "ill-typed edge")]
    fn verify_catches_edge_type_mismatch_in_unreachable_block() {
        // Ill-typed edges parked in unreachable blocks are exactly where
        // divergence-handling bugs hide (RUE-347) — they must still be caught.
        let mut cfg = unit_cfg();
        let entry = cfg.new_block();
        cfg.entry = entry;
        let entry_value = cfg.add_inst_to_block(
            entry,
            CfgInst {
                data: CfgInstData::Const(0),
                ty: Type::I32,
                span: Span::new(0, 0),
            },
        );
        cfg.set_terminator(
            entry,
            Terminator::Return {
                value: Some(entry_value),
            },
        );
        // Unreachable-but-terminated pair with a unit→i32 edge.
        let orphan_from = cfg.new_block();
        let orphan_to = cfg.new_block();
        let param = cfg.add_block_param(orphan_to, Type::I32);
        cfg.set_terminator(orphan_to, Terminator::Return { value: Some(param) });
        let arg = cfg.add_inst_to_block(
            orphan_from,
            CfgInst {
                data: CfgInstData::Const(0),
                ty: Type::UNIT,
                span: Span::new(0, 1),
            },
        );
        let (args_start, args_len) = cfg.push_extra(vec![arg]);
        cfg.set_terminator(
            orphan_from,
            Terminator::Goto {
                target: orphan_to,
                args_start,
                args_len,
            },
        );
        cfg.verify();
    }

    #[test]
    #[should_panic(expected = "value v0 is unattached")]
    fn verify_rejects_unattached_value() {
        let mut cfg = unit_cfg();
        let entry = cfg.new_block();
        cfg.entry = entry;
        cfg.add_inst(CfgInst {
            data: CfgInstData::Const(0),
            ty: Type::I32,
            span: Span::new(0, 0),
        });
        cfg.set_terminator(entry, Terminator::Unreachable);
        cfg.verify();
    }

    #[test]
    #[should_panic(expected = "duplicate attachments")]
    fn verify_rejects_duplicate_attachment() {
        let mut cfg = unit_cfg();
        let entry = cfg.new_block();
        cfg.entry = entry;
        let value = cfg.add_inst_to_block(
            entry,
            CfgInst {
                data: CfgInstData::Const(0),
                ty: Type::I32,
                span: Span::new(0, 0),
            },
        );
        cfg.get_block_mut(entry).insts.push(value);
        cfg.set_terminator(entry, Terminator::Return { value: Some(value) });
        cfg.verify();
    }

    #[test]
    #[should_panic(expected = "declares index 1")]
    fn verify_rejects_wrong_block_param_index() {
        let mut cfg = unit_cfg();
        let entry = cfg.new_block();
        cfg.entry = entry;
        let param = cfg.add_block_param(entry, Type::I32);
        cfg.get_inst_mut(param).data = CfgInstData::BlockParam { index: 1 };
        cfg.set_terminator(entry, Terminator::Return { value: Some(param) });
        cfg.verify();
    }

    #[test]
    #[should_panic(expected = "has non-BlockParam data")]
    fn verify_rejects_wrong_block_param_data() {
        let mut cfg = unit_cfg();
        let entry = cfg.new_block();
        cfg.entry = entry;
        let param = cfg.add_block_param(entry, Type::I32);
        cfg.get_inst_mut(param).data = CfgInstData::Const(0);
        cfg.set_terminator(entry, Terminator::Return { value: Some(param) });
        cfg.verify();
    }

    #[test]
    #[should_panic(expected = "stores type Type::U64")]
    fn verify_rejects_wrong_block_param_stored_type() {
        let mut cfg = unit_cfg();
        let entry = cfg.new_block();
        cfg.entry = entry;
        let param = cfg.add_block_param(entry, Type::I32);
        cfg.get_block_mut(entry).params[0].1 = Type::U64;
        cfg.set_terminator(entry, Terminator::Return { value: Some(param) });
        cfg.verify();
    }

    #[test]
    #[should_panic(expected = "used before its definition")]
    fn verify_rejects_use_before_definition() {
        let mut cfg = unit_cfg();
        let entry = cfg.new_block();
        cfg.entry = entry;
        let later = cfg.add_inst_to_block(
            entry,
            CfgInst {
                data: CfgInstData::Const(1),
                ty: Type::I32,
                span: Span::new(0, 0),
            },
        );
        let earlier = cfg.add_inst_to_block(
            entry,
            CfgInst {
                data: CfgInstData::Neg(later),
                ty: Type::I32,
                span: Span::new(0, 0),
            },
        );
        cfg.get_block_mut(entry).insts.swap(0, 1);
        cfg.set_terminator(
            entry,
            Terminator::Return {
                value: Some(earlier),
            },
        );
        cfg.verify();
    }

    #[test]
    #[should_panic(expected = "does not dominate the use")]
    fn verify_rejects_cross_block_non_dominating_use() {
        let mut cfg = unit_cfg();
        let entry = cfg.new_block();
        cfg.entry = entry;
        let left = cfg.new_block();
        let right = cfg.new_block();
        let cond = cfg.add_inst_to_block(
            entry,
            CfgInst {
                data: CfgInstData::BoolConst(true),
                ty: Type::BOOL,
                span: Span::new(0, 0),
            },
        );
        cfg.set_terminator(
            entry,
            Terminator::Branch {
                cond,
                then_block: left,
                then_args_start: 0,
                then_args_len: 0,
                else_block: right,
                else_args_start: 0,
                else_args_len: 0,
            },
        );
        let value = cfg.add_inst_to_block(
            left,
            CfgInst {
                data: CfgInstData::Const(1),
                ty: Type::I32,
                span: Span::new(0, 0),
            },
        );
        cfg.set_terminator(left, Terminator::Return { value: Some(value) });
        cfg.set_terminator(right, Terminator::Return { value: Some(value) });
        cfg.verify();
    }

    #[test]
    #[should_panic(expected = "branch condition")]
    fn verify_rejects_non_bool_branch_condition() {
        let mut cfg = unit_cfg();
        let entry = cfg.new_block();
        cfg.entry = entry;
        let target = cfg.new_block();
        let cond = cfg.add_inst_to_block(
            entry,
            CfgInst {
                data: CfgInstData::Const(1),
                ty: Type::I32,
                span: Span::new(0, 0),
            },
        );
        cfg.set_terminator(
            entry,
            Terminator::Branch {
                cond,
                then_block: target,
                then_args_start: 0,
                then_args_len: 0,
                else_block: target,
                else_args_start: 0,
                else_args_len: 0,
            },
        );
        cfg.set_terminator(target, Terminator::Unreachable);
        cfg.verify();
    }

    #[test]
    #[should_panic(expected = "has no value but function return type")]
    fn verify_rejects_missing_nonunit_return_value() {
        let mut cfg = unit_cfg();
        let entry = cfg.new_block();
        cfg.entry = entry;
        cfg.set_terminator(entry, Terminator::Return { value: None });
        cfg.verify();
    }

    #[test]
    #[should_panic(expected = "unit-returning functions must use")]
    fn verify_rejects_explicit_unit_return_value() {
        let mut cfg = Cfg::new(Type::UNIT, 0, 0, "unit_return".to_string(), vec![]);
        let entry = cfg.new_block();
        cfg.entry = entry;
        let value = cfg.add_inst_to_block(
            entry,
            CfgInst {
                data: CfgInstData::Const(0),
                ty: Type::UNIT,
                span: Span::new(0, 0),
            },
        );
        cfg.set_terminator(entry, Terminator::Return { value: Some(value) });
        cfg.verify();
    }

    #[test]
    #[should_panic(expected = "entry block bb7 is out of bounds")]
    fn verify_rejects_invalid_entry_before_indexing() {
        let mut cfg = unit_cfg();
        cfg.new_block();
        cfg.entry = BlockId::from_raw(7);
        cfg.verify();
    }

    #[test]
    #[should_panic(expected = "goto target bb9")]
    fn verify_rejects_invalid_target_before_indexing() {
        let mut cfg = unit_cfg();
        let entry = cfg.new_block();
        cfg.entry = entry;
        cfg.set_terminator(
            entry,
            Terminator::Goto {
                target: BlockId::from_raw(9),
                args_start: 0,
                args_len: 0,
            },
        );
        cfg.verify();
    }

    #[test]
    #[should_panic(expected = "local slot range 0..1")]
    fn verify_checks_slots_in_unreachable_blocks() {
        let mut cfg = unit_cfg();
        let entry = cfg.new_block();
        cfg.entry = entry;
        let value = cfg.add_inst_to_block(
            entry,
            CfgInst {
                data: CfgInstData::Const(0),
                ty: Type::I32,
                span: Span::new(0, 0),
            },
        );
        cfg.set_terminator(entry, Terminator::Return { value: Some(value) });
        let orphan = cfg.new_block();
        cfg.add_inst_to_block(
            orphan,
            CfgInst {
                data: CfgInstData::Load { slot: 0 },
                ty: Type::I32,
                span: Span::new(0, 0),
            },
        );
        cfg.verify_with_type_pool(&FrozenTypeInternPool::new());
    }

    #[test]
    #[should_panic(expected = "parameter slot range 0..1")]
    fn verify_rejects_invalid_parameter_slot() {
        let mut cfg = unit_cfg();
        let entry = cfg.new_block();
        cfg.entry = entry;
        cfg.add_inst_to_block(
            entry,
            CfgInst {
                data: CfgInstData::Param { index: 0 },
                ty: Type::I32,
                span: Span::new(0, 0),
            },
        );
        cfg.set_terminator(entry, Terminator::Unreachable);
        cfg.verify_with_type_pool(&FrozenTypeInternPool::new());
    }

    #[test]
    #[should_panic(expected = "local slot range 0..2")]
    fn verify_rejects_multi_slot_local_overflow() {
        let pool = TypeInternPool::new();
        let interner = ThreadedRodeo::default();
        let pair = register_struct(&pool, &interner, "Pair", &[Type::I32, Type::I32]);
        let pool = pool.freeze();
        let mut cfg = Cfg::new(Type::UNIT, 1, 0, "wide_local".to_string(), vec![]);
        let entry = cfg.new_block();
        cfg.entry = entry;
        cfg.add_inst_to_block(
            entry,
            CfgInst {
                data: CfgInstData::Load { slot: 0 },
                ty: Type::new_struct(pair),
                span: Span::new(0, 0),
            },
        );
        cfg.set_terminator(entry, Terminator::Unreachable);
        cfg.verify_with_type_pool(&pool);
    }

    #[test]
    fn verify_accepts_multi_slot_logical_type_in_one_by_ref_parameter_slot() {
        let pool = TypeInternPool::new();
        let interner = ThreadedRodeo::default();
        let pair = register_struct(&pool, &interner, "BorrowedPair", &[Type::I32, Type::I32]);
        let pool = pool.freeze();
        let pair_ty = Type::new_struct(pair);
        let mut cfg = Cfg::new(Type::UNIT, 0, 1, "borrowed_pair".to_string(), vec![true]);
        let entry = cfg.new_block();
        cfg.entry = entry;
        let place = cfg.make_place(
            PlaceBase::Param(0),
            pair_ty,
            [Projection::Field {
                struct_id: pair,
                field_index: 1,
            }],
        );
        cfg.add_inst_to_block(
            entry,
            CfgInst {
                data: CfgInstData::PlaceRead { place },
                ty: Type::I32,
                span: Span::new(0, 0),
            },
        );
        cfg.set_terminator(entry, Terminator::Return { value: None });
        cfg.verify_with_type_pool(&pool);
    }

    #[test]
    #[should_panic(expected = "references invalid struct id")]
    fn verify_rejects_invalid_struct_projection_id() {
        let pool = FrozenTypeInternPool::new();
        let mut cfg = Cfg::new(Type::UNIT, 1, 0, "invalid_struct".to_string(), vec![]);
        let entry = cfg.new_block();
        cfg.entry = entry;
        let place = cfg.make_place(
            PlaceBase::Local(0),
            Type::I32,
            [Projection::Field {
                struct_id: StructId::from_pool_index(99),
                field_index: 0,
            }],
        );
        cfg.add_inst_to_block(
            entry,
            CfgInst {
                data: CfgInstData::PlaceRead { place },
                ty: Type::I32,
                span: Span::new(0, 0),
            },
        );
        cfg.set_terminator(entry, Terminator::Unreachable);
        cfg.verify_with_type_pool(&pool);
    }

    #[test]
    #[should_panic(expected = "references invalid array id")]
    fn verify_rejects_invalid_array_projection_id() {
        let pool = FrozenTypeInternPool::new();
        let mut cfg = Cfg::new(Type::UNIT, 1, 0, "invalid_array".to_string(), vec![]);
        let entry = cfg.new_block();
        cfg.entry = entry;
        let index = cfg.add_inst_to_block(
            entry,
            CfgInst {
                data: CfgInstData::Const(0),
                ty: Type::U64,
                span: Span::new(0, 0),
            },
        );
        let array_type = Type::new_array(ArrayTypeId::from_pool_index(99));
        let place = cfg.make_place(
            PlaceBase::Local(0),
            Type::I32,
            [Projection::Index { array_type, index }],
        );
        cfg.add_inst_to_block(
            entry,
            CfgInst {
                data: CfgInstData::PlaceRead { place },
                ty: Type::I32,
                span: Span::new(0, 0),
            },
        );
        cfg.set_terminator(entry, Terminator::Unreachable);
        cfg.verify_with_type_pool(&pool);
    }

    #[test]
    #[should_panic(expected = "references field 1")]
    fn verify_rejects_out_of_bounds_field_projection() {
        let pool = TypeInternPool::new();
        let interner = ThreadedRodeo::default();
        let record = register_struct(&pool, &interner, "Record", &[Type::I32]);
        let pool = pool.freeze();
        let record_ty = Type::new_struct(record);
        let mut cfg = Cfg::new(Type::UNIT, 1, 0, "bad_field".to_string(), vec![]);
        let entry = cfg.new_block();
        cfg.entry = entry;
        let place = cfg.make_place(
            PlaceBase::Local(0),
            record_ty,
            [Projection::Field {
                struct_id: record,
                field_index: 1,
            }],
        );
        cfg.add_inst_to_block(
            entry,
            CfgInst {
                data: CfgInstData::PlaceRead { place },
                ty: Type::I32,
                span: Span::new(0, 0),
            },
        );
        cfg.set_terminator(entry, Terminator::Unreachable);
        cfg.verify_with_type_pool(&pool);
    }

    #[test]
    #[should_panic(expected = "previous link produced")]
    fn verify_rejects_broken_nested_projection_continuity() {
        let pool = TypeInternPool::new();
        let interner = ThreadedRodeo::default();
        let inner = register_struct(&pool, &interner, "Inner", &[Type::I32]);
        let wrong = register_struct(&pool, &interner, "Wrong", &[Type::I32]);
        let outer = register_struct(&pool, &interner, "Outer", &[Type::new_struct(inner)]);
        let pool = pool.freeze();
        let mut cfg = Cfg::new(Type::UNIT, 1, 0, "broken_chain".to_string(), vec![]);
        let entry = cfg.new_block();
        cfg.entry = entry;
        let place = cfg.make_place(
            PlaceBase::Local(0),
            Type::new_struct(outer),
            [
                Projection::Field {
                    struct_id: outer,
                    field_index: 0,
                },
                Projection::Field {
                    struct_id: wrong,
                    field_index: 0,
                },
            ],
        );
        cfg.add_inst_to_block(
            entry,
            CfgInst {
                data: CfgInstData::PlaceRead { place },
                ty: Type::I32,
                span: Span::new(0, 0),
            },
        );
        cfg.set_terminator(entry, Terminator::Unreachable);
        cfg.verify_with_type_pool(&pool);
    }

    #[test]
    #[should_panic(expected = "projection chain produces Type::I32")]
    fn verify_rejects_place_read_result_type_mismatch() {
        let pool = TypeInternPool::new();
        let interner = ThreadedRodeo::default();
        let record = register_struct(&pool, &interner, "ResultRecord", &[Type::I32]);
        let pool = pool.freeze();
        let mut cfg = Cfg::new(Type::UNIT, 1, 0, "bad_result".to_string(), vec![]);
        let entry = cfg.new_block();
        cfg.entry = entry;
        let place = cfg.make_place(
            PlaceBase::Local(0),
            Type::new_struct(record),
            [Projection::Field {
                struct_id: record,
                field_index: 0,
            }],
        );
        cfg.add_inst_to_block(
            entry,
            CfgInst {
                data: CfgInstData::PlaceRead { place },
                ty: Type::BOOL,
                span: Span::new(0, 0),
            },
        );
        cfg.set_terminator(entry, Terminator::Unreachable);
        cfg.verify_with_type_pool(&pool);
    }

    #[test]
    #[should_panic(expected = "array elements slice")]
    fn verify_rejects_invalid_extra_slice_before_slicing() {
        let mut cfg = unit_cfg();
        let entry = cfg.new_block();
        cfg.entry = entry;
        cfg.add_inst_to_block(
            entry,
            CfgInst {
                data: CfgInstData::ArrayInit {
                    elements_start: u32::MAX,
                    elements_len: 2,
                },
                ty: Type::I32,
                span: Span::new(0, 0),
            },
        );
        cfg.set_terminator(entry, Terminator::Unreachable);
        cfg.verify();
    }

    #[test]
    #[should_panic(expected = "call-argument slice")]
    fn verify_rejects_invalid_call_argument_slice_before_slicing() {
        let mut cfg = unit_cfg();
        let entry = cfg.new_block();
        cfg.entry = entry;
        cfg.add_inst_to_block(
            entry,
            CfgInst {
                data: CfgInstData::Call {
                    runtime: None,
                    name: lasso::Spur::default(),
                    args_start: 1,
                    args_len: 1,
                },
                ty: Type::I32,
                span: Span::new(0, 0),
            },
        );
        cfg.set_terminator(entry, Terminator::Unreachable);
        cfg.verify();
    }

    #[test]
    #[should_panic(expected = "switch-case slice")]
    fn verify_rejects_invalid_switch_case_slice_before_slicing() {
        let mut cfg = unit_cfg();
        let entry = cfg.new_block();
        cfg.entry = entry;
        let value = cfg.add_inst_to_block(
            entry,
            CfgInst {
                data: CfgInstData::Const(0),
                ty: Type::I32,
                span: Span::new(0, 0),
            },
        );
        cfg.set_terminator(
            entry,
            Terminator::Switch {
                scrutinee: value,
                cases_start: 1,
                cases_len: 1,
                default: entry,
            },
        );
        cfg.verify();
    }

    #[test]
    #[should_panic(expected = "projection slice")]
    fn verify_rejects_invalid_projection_slice_before_slicing() {
        let mut cfg = Cfg::new(Type::I32, 1, 0, "projection_slice".to_string(), vec![]);
        let entry = cfg.new_block();
        cfg.entry = entry;
        cfg.add_inst_to_block(
            entry,
            CfgInst {
                data: CfgInstData::PlaceRead {
                    place: Place {
                        base: PlaceBase::Local(0),
                        base_type: Type::I32,
                        proj_start: 1,
                        proj_len: 1,
                    },
                },
                ty: Type::I32,
                span: Span::new(0, 0),
            },
        );
        cfg.set_terminator(entry, Terminator::Unreachable);
        cfg.verify();
    }

    #[test]
    #[should_panic(expected = "projection index v0")]
    fn verify_rejects_non_integer_projection_index() {
        let mut cfg = Cfg::new(Type::I32, 1, 0, "projection_index".to_string(), vec![]);
        let entry = cfg.new_block();
        cfg.entry = entry;
        let index = cfg.add_inst_to_block(
            entry,
            CfgInst {
                data: CfgInstData::BoolConst(false),
                ty: Type::BOOL,
                span: Span::new(0, 0),
            },
        );
        let array_type = Type::new_array(ArrayTypeId::from_pool_index(1));
        let place = cfg.make_place(
            PlaceBase::Local(0),
            array_type,
            [Projection::Index { array_type, index }],
        );
        cfg.add_inst_to_block(
            entry,
            CfgInst {
                data: CfgInstData::PlaceRead { place },
                ty: Type::I32,
                span: Span::new(0, 0),
            },
        );
        cfg.set_terminator(entry, Terminator::Unreachable);
        cfg.verify();
    }

    #[test]
    fn optimize_verifies_before_and_after_dce() {
        for level in [OptLevel::O0, OptLevel::O1] {
            let mut cfg = unit_cfg();
            let entry = cfg.new_block();
            cfg.entry = entry;
            cfg.add_inst_to_block(
                entry,
                CfgInst {
                    data: CfgInstData::Const(99),
                    ty: Type::I32,
                    span: Span::new(0, 0),
                },
            );
            let result = cfg.add_inst_to_block(
                entry,
                CfgInst {
                    data: CfgInstData::Const(42),
                    ty: Type::I32,
                    span: Span::new(0, 0),
                },
            );
            cfg.set_terminator(
                entry,
                Terminator::Return {
                    value: Some(result),
                },
            );
            opt::optimize(&mut cfg, level, &FrozenTypeInternPool::new());
        }
    }

    #[test]
    #[should_panic(expected = "is unattached")]
    fn o1_cannot_hide_unattached_value_with_dce() {
        let mut cfg = unit_cfg();
        let entry = cfg.new_block();
        cfg.entry = entry;
        cfg.add_inst(CfgInst {
            data: CfgInstData::Const(99),
            ty: Type::I32,
            span: Span::new(0, 0),
        });
        let result = cfg.add_inst_to_block(
            entry,
            CfgInst {
                data: CfgInstData::Const(42),
                ty: Type::I32,
                span: Span::new(0, 0),
            },
        );
        cfg.set_terminator(
            entry,
            Terminator::Return {
                value: Some(result),
            },
        );
        opt::optimize(&mut cfg, OptLevel::O1, &FrozenTypeInternPool::new());
    }
}
