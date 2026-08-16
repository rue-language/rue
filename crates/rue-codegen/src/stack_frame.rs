//! Stack frame information for debugging.
//!
//! This module provides types and functions for extracting stack frame layout
//! information from compiled code. This is useful for debugging ABI issues,
//! calling convention bugs, and understanding how values are laid out on the stack.

use lasso::ThreadedRodeo;
use rue_air::FrozenTypeInternPool;
use rue_cfg::ValidatedCfg;
use rue_error::CompileResult;
use rue_target::{Arch, Target};

/// A slot on the stack (local variable or spill slot).
#[derive(Debug, Clone)]
pub struct StackSlot {
    /// Name of the variable (if known), or None for spill slots.
    pub name: Option<String>,
    /// Offset from the frame pointer (negative for locals/spills).
    pub offset: i32,
    /// Size in bytes.
    pub size: usize,
    /// Type description.
    pub ty: String,
    /// Kind of slot.
    pub kind: StackSlotKind,
}

/// The kind of stack slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StackSlotKind {
    /// A local variable.
    Local,
    /// A spill slot for a spilled register.
    Spill,
    /// A saved callee-saved register.
    CalleeSaved,
    /// A parameter slot (for register parameters spilled to stack).
    Parameter,
}

impl std::fmt::Display for StackSlotKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StackSlotKind::Local => write!(f, "local"),
            StackSlotKind::Spill => write!(f, "spill"),
            StackSlotKind::CalleeSaved => write!(f, "callee-saved"),
            StackSlotKind::Parameter => write!(f, "param"),
        }
    }
}

/// Location of a function argument.
#[derive(Debug, Clone)]
pub struct ArgumentLocation {
    /// Argument index (0-based).
    pub index: usize,
    /// Name of the parameter (if known).
    pub name: Option<String>,
    /// Type description.
    pub ty: String,
    /// Where the argument is passed.
    pub location: ArgPassingLocation,
}

/// How an argument is passed to a function.
#[derive(Debug, Clone)]
pub enum ArgPassingLocation {
    /// Passed in a register.
    Register(String),
    /// Passed on the stack at an offset from the frame pointer.
    Stack { offset: i32 },
}

impl std::fmt::Display for ArgPassingLocation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ArgPassingLocation::Register(reg) => write!(f, "{}", reg),
            ArgPassingLocation::Stack { offset } => {
                if *offset >= 0 {
                    write!(f, "[fp+{}]", offset)
                } else {
                    write!(f, "[fp{}]", offset)
                }
            }
        }
    }
}

/// Location of the return value.
#[derive(Debug, Clone)]
pub struct ReturnLocation {
    /// Type description.
    pub ty: String,
    /// Register(s) used for the return value.
    pub registers: Vec<String>,
}

impl std::fmt::Display for ReturnLocation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.registers.is_empty() {
            write!(f, "(none)")
        } else {
            write!(f, "{}", self.registers.join(", "))
        }
    }
}

/// Complete stack frame information for a function.
#[derive(Debug, Clone)]
pub struct StackFrameInfo {
    /// Name of the function.
    pub function_name: String,
    /// Total frame size in bytes.
    pub frame_size: usize,
    /// Required alignment in bytes.
    pub alignment: usize,
    /// Whether the function establishes a frame pointer. A frameless leaf
    /// (RUE-1171) has none, so its saved-register offsets are reported relative
    /// to the stack pointer on entry rather than to a frame pointer.
    pub uses_frame_pointer: bool,
    /// All stack slots (locals, spills, callee-saved, params).
    pub slots: Vec<StackSlot>,
    /// Argument passing locations.
    pub arguments: Vec<ArgumentLocation>,
    /// Return value location.
    pub return_location: ReturnLocation,
    /// Target architecture.
    pub target: Target,
}

impl std::fmt::Display for StackFrameInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // A frameless leaf has no frame pointer to report against; its saved
        // registers sit directly below the stack pointer as it was on entry,
        // at the same offsets the framed layout would have used (RUE-1171).
        let fp_name = if !self.uses_frame_pointer {
            "sp"
        } else {
            match self.target.arch() {
                Arch::X86_64 => "rbp",
                Arch::Aarch64 => "fp",
            }
        };

        writeln!(f, "=== Stack Frame ({}) ===", self.function_name)?;
        writeln!(f)?;
        writeln!(f, "Frame size: {} bytes", self.frame_size)?;
        writeln!(f, "Alignment: {} bytes", self.alignment)?;
        if !self.uses_frame_pointer {
            writeln!(f, "Frame pointer: none (frameless leaf)")?;
        }
        writeln!(f)?;

        // Group slots by kind
        let callee_saved: Vec<_> = self
            .slots
            .iter()
            .filter(|s| s.kind == StackSlotKind::CalleeSaved)
            .collect();
        let locals: Vec<_> = self
            .slots
            .iter()
            .filter(|s| s.kind == StackSlotKind::Local)
            .collect();
        let params: Vec<_> = self
            .slots
            .iter()
            .filter(|s| s.kind == StackSlotKind::Parameter)
            .collect();
        let spills: Vec<_> = self
            .slots
            .iter()
            .filter(|s| s.kind == StackSlotKind::Spill)
            .collect();

        writeln!(f, "Layout ({}-relative):", fp_name)?;

        // Callee-saved registers
        if !callee_saved.is_empty() {
            for slot in &callee_saved {
                writeln!(
                    f,
                    "  [{}{:+4}] : {} ({})",
                    fp_name,
                    slot.offset,
                    slot.name.as_deref().unwrap_or("?"),
                    slot.kind
                )?;
            }
        }

        // Local variables
        if !locals.is_empty() {
            for slot in &locals {
                writeln!(
                    f,
                    "  [{}{:+4}] : {} '{}' ({}, {} bytes)",
                    fp_name,
                    slot.offset,
                    slot.kind,
                    slot.name.as_deref().unwrap_or("?"),
                    slot.ty,
                    slot.size
                )?;
            }
        }

        // Parameter spill slots
        if !params.is_empty() {
            for slot in &params {
                writeln!(
                    f,
                    "  [{}{:+4}] : {} '{}' ({}, {} bytes)",
                    fp_name,
                    slot.offset,
                    slot.kind,
                    slot.name.as_deref().unwrap_or("?"),
                    slot.ty,
                    slot.size
                )?;
            }
        }

        // Spill slots
        if !spills.is_empty() {
            for slot in &spills {
                writeln!(
                    f,
                    "  [{}{:+4}] : spill slot ({})",
                    fp_name, slot.offset, slot.ty
                )?;
            }
        }

        writeln!(f)?;

        // Arguments on entry
        if !self.arguments.is_empty() {
            writeln!(f, "Arguments (on entry):")?;
            for arg in &self.arguments {
                writeln!(
                    f,
                    "  {}: arg{} '{}' ({})",
                    arg.location,
                    arg.index,
                    arg.name.as_deref().unwrap_or("?"),
                    arg.ty
                )?;
            }
            writeln!(f)?;
        }

        // Return value
        writeln!(
            f,
            "Return: {} ({})",
            self.return_location, self.return_location.ty
        )?;

        Ok(())
    }
}

/// Generate stack frame information for a function.
///
/// This function runs the codegen pipeline up to register allocation to determine
/// the actual stack layout, including spill slots and callee-saved registers.
pub fn generate_stack_frame_info(
    cfg: &ValidatedCfg,
    function_name: &str,
    type_pool: &FrozenTypeInternPool,
    interner: &ThreadedRodeo,
    target: Target,
) -> CompileResult<StackFrameInfo> {
    match target.arch() {
        Arch::X86_64 => {
            generate_x86_64_stack_frame(cfg, function_name, type_pool, interner, target)
        }
        Arch::Aarch64 => {
            generate_aarch64_stack_frame(cfg, function_name, type_pool, interner, target)
        }
    }
}

/// Generate stack frame info for x86-64.
fn generate_x86_64_stack_frame(
    cfg: &ValidatedCfg,
    function_name: &str,
    type_pool: &FrozenTypeInternPool,
    interner: &ThreadedRodeo,
    target: Target,
) -> CompileResult<StackFrameInfo> {
    let num_params = cfg.num_params();
    // sret returns reserve one extra frame slot for the incoming buffer
    // pointer and shift user args by one ABI slot (RUE-106).
    let prepared = crate::x86_64::prepare_backend(
        cfg,
        type_pool,
        interner,
        crate::MachineSymbolResolver::default(),
    )?;
    let has_sret = prepared.has_sret;
    let sret_slots = u32::from(has_sret);
    let num_spills = prepared.total_locals - prepared.frame_local_slots;
    // Frame slots the local area occupies after marker-driven slot sharing
    // (RUE-768); locals with provably disjoint storage windows overlay one
    // another, so this is at most `cfg.num_locals()`.
    let num_locals = prepared.frame_local_slots;
    let used_callee_saved = &prepared.used_callee_saved;

    // Calculate stack layout through the byte-based frame-layout authority
    // (ALL params get frame slots: the prologue copies stack-passed args into
    // the frame param area). Every slot cell is one frame cell today.
    use crate::frame_layout::frame_cell_bytes;
    let frame = prepared.frame_layout;
    let stack_size = frame.frame_size() as i32;
    // An eligible frameless leaf establishes no RBP; its saved registers sit
    // below the entry stack pointer at the offsets computed below (RUE-1171).
    let uses_frame_pointer =
        frame.frame_pointer() == crate::frame_layout::FramePointer::Established;

    let mut slots = Vec::new();

    // Add callee-saved registers (each an 8-byte push just below rbp).
    for (i, reg) in used_callee_saved.iter().enumerate() {
        let offset = crate::frame_layout::slot_offset_pre_saved(i as u32);
        slots.push(StackSlot {
            name: Some(format!("saved {}", reg)),
            offset,
            size: frame_cell_bytes() as usize,
            ty: "i64".to_string(),
            kind: StackSlotKind::CalleeSaved,
        });
    }

    // Add local variables
    for i in 0..num_locals {
        slots.push(StackSlot {
            name: None, // We don't have variable names from CFG yet
            offset: frame.slot_offset(i),
            size: frame.slot_size(i) as usize,
            ty: "i64".to_string(), // Generic - we don't track types at CFG level
            kind: StackSlotKind::Local,
        });
    }

    // Add parameter home slots. Only homed parameters occupy frame slots:
    // register-only parameters (RUE-1170) live in a callee-saved register or
    // an ordinary spill slot and are reported under argument locations only.
    for i in 0..num_params {
        let Some(area_slot) = prepared.param_storage.area_slot(i) else {
            continue;
        };
        let slot = num_locals + area_slot;
        slots.push(StackSlot {
            name: None, // We don't have param names from CFG yet
            offset: frame.slot_offset(slot),
            size: frame.slot_size(slot) as usize,
            ty: "i64".to_string(),
            kind: StackSlotKind::Parameter,
        });
    }
    let homed_params = prepared.param_storage.homed_area_slots();

    // Add the sret pointer slot (one past the compacted param area)
    if has_sret {
        let slot = num_locals + homed_params;
        slots.push(StackSlot {
            name: Some("sret ptr".to_string()),
            offset: frame.slot_offset(slot),
            size: frame.slot_size(slot) as usize,
            ty: "ptr".to_string(),
            kind: StackSlotKind::Parameter,
        });
    }

    // Add spill slots
    for i in 0..num_spills {
        let slot = num_locals + homed_params + sret_slots + i;
        slots.push(StackSlot {
            name: None,
            offset: frame.slot_offset(slot),
            size: frame.slot_size(slot) as usize,
            ty: "i64".to_string(),
            kind: StackSlotKind::Spill,
        });
    }

    #[allow(unused_variables)]
    let arg_regs = ["rdi", "rsi", "rdx", "rcx", "r8", "r9"];

    // Build argument locations (the hidden sret pointer occupies the first
    // ABI slot when present, shifting user args by one)
    let mut arguments = Vec::new();
    for i in 0..num_params as usize {
        let abi_index = i + has_sret as usize;
        let location = if abi_index < 6 {
            ArgPassingLocation::Register(arg_regs[abi_index].to_string())
        } else {
            // Stack arguments are at positive offsets from rbp
            // ABI slot 6 at [rbp+16], slot 7 at [rbp+24], etc.
            let offset = 16 + ((abi_index - 6) as i32) * 8;
            ArgPassingLocation::Stack { offset }
        };
        arguments.push(ArgumentLocation {
            index: i,
            name: None,
            ty: "i64".to_string(),
            location,
        });
    }

    // Return location (sret returns go through the caller-allocated buffer
    // whose address arrived in rdi)
    let return_ty = format!("{:?}", cfg.return_type());
    let return_location = ReturnLocation {
        ty: return_ty,
        registers: if has_sret {
            vec!["sret buffer (via rdi)".to_string()]
        } else {
            vec!["rax".to_string()]
        },
    };

    Ok(StackFrameInfo {
        function_name: function_name.to_string(),
        frame_size: stack_size as usize,
        alignment: frame.alignment() as usize,
        uses_frame_pointer,
        slots,
        arguments,
        return_location,
        target,
    })
}

/// Generate stack frame info for AArch64.
fn generate_aarch64_stack_frame(
    cfg: &ValidatedCfg,
    function_name: &str,
    type_pool: &FrozenTypeInternPool,
    interner: &ThreadedRodeo,
    target: Target,
) -> CompileResult<StackFrameInfo> {
    let num_params = cfg.num_params();
    // sret returns reserve one extra frame slot for the incoming buffer
    // pointer and shift user args by one ABI slot (RUE-106).
    let prepared = crate::aarch64::prepare_backend(
        cfg,
        type_pool,
        interner,
        target,
        crate::MachineSymbolResolver::default(),
    )?;
    let has_sret = prepared.has_sret;
    let sret_slots = u32::from(has_sret);
    let num_spills = prepared.total_locals - prepared.frame_local_slots;
    // Frame slots the local area occupies after marker-driven slot sharing
    // (RUE-768); locals with provably disjoint storage windows overlay one
    // another, so this is at most `cfg.num_locals()`.
    let num_locals = prepared.frame_local_slots;
    let used_callee_saved = &prepared.used_callee_saved;

    // Calculate stack layout for AArch64 through the byte-based frame-layout
    // authority. Callee-saved registers are saved in pairs (16 bytes per pair)
    // and the FP/LR pair (16 bytes) sits above them; ALL params get frame slots
    // (the prologue copies stack-passed args into the frame param area).
    let frame = prepared.frame_layout;
    let frame_size = frame.frame_size() as usize;
    // An eligible frameless leaf establishes no FP and saves no FP/LR pair; its
    // callee-saved pairs sit below the entry stack pointer (RUE-1171).
    let uses_frame_pointer =
        frame.frame_pointer() == crate::frame_layout::FramePointer::Established;

    // Every FP-relative location below is derived from the same frame-layout
    // authority the emitter uses (RUE-774), so the reported slots match the
    // prologue/body instructions exactly. AArch64 sets FP *at* the saved FP/LR
    // pair, so the FP/LR bytes sit at and above FP and are not part of the
    // below-FP saved area — only the callee-saved pairs are.
    use crate::frame_layout::{aarch64_callee_saved_pair_offset, aarch64_slot_offset};
    let num_callee_saved = used_callee_saved.len();
    let slot_offset = |slot: u32| aarch64_slot_offset(num_callee_saved, slot);

    let mut slots = Vec::new();

    // Callee-saved registers, stored in pairs by the prologue starting at
    // `[fp -16]`; a trailing odd register occupies the low half of the next
    // pair slot.
    let mut i = 0;
    let mut pair_index = 0;
    while i + 1 < num_callee_saved {
        let base = aarch64_callee_saved_pair_offset(pair_index);
        slots.push(StackSlot {
            name: Some(format!("saved {}", used_callee_saved[i])),
            offset: base,
            size: 8,
            ty: "i64".to_string(),
            kind: StackSlotKind::CalleeSaved,
        });
        slots.push(StackSlot {
            name: Some(format!("saved {}", used_callee_saved[i + 1])),
            offset: base + 8,
            size: 8,
            ty: "i64".to_string(),
            kind: StackSlotKind::CalleeSaved,
        });
        i += 2;
        pair_index += 1;
    }
    // Handle odd register
    if i < num_callee_saved {
        slots.push(StackSlot {
            name: Some(format!("saved {}", used_callee_saved[i])),
            offset: aarch64_callee_saved_pair_offset(pair_index),
            size: 8,
            ty: "i64".to_string(),
            kind: StackSlotKind::CalleeSaved,
        });
    }

    // Add local variables
    for i in 0..num_locals {
        slots.push(StackSlot {
            name: None,
            offset: slot_offset(i),
            size: frame.slot_size(i) as usize,
            ty: "i64".to_string(),
            kind: StackSlotKind::Local,
        });
    }

    // Add parameter home slots. Only homed parameters occupy frame slots:
    // register-only parameters (RUE-1170) live in a callee-saved register or
    // an ordinary spill slot and are reported under argument locations only.
    for i in 0..num_params {
        let Some(area_slot) = prepared.param_storage.area_slot(i) else {
            continue;
        };
        let slot = num_locals + area_slot;
        slots.push(StackSlot {
            name: None,
            offset: slot_offset(slot),
            size: frame.slot_size(slot) as usize,
            ty: "i64".to_string(),
            kind: StackSlotKind::Parameter,
        });
    }
    let homed_params = prepared.param_storage.homed_area_slots();

    // Add the sret pointer slot (one past the compacted param area)
    if has_sret {
        let slot = num_locals + homed_params;
        slots.push(StackSlot {
            name: Some("sret ptr".to_string()),
            offset: slot_offset(slot),
            size: frame.slot_size(slot) as usize,
            ty: "ptr".to_string(),
            kind: StackSlotKind::Parameter,
        });
    }

    // Add spill slots
    for i in 0..num_spills {
        let slot = num_locals + homed_params + sret_slots + i;
        slots.push(StackSlot {
            name: None,
            offset: slot_offset(slot),
            size: frame.slot_size(slot) as usize,
            ty: "i64".to_string(),
            kind: StackSlotKind::Spill,
        });
    }

    // Build argument locations (x0-x7 for first 8 ABI slots on AArch64; the
    // hidden sret pointer occupies the first slot when present)
    let arg_regs = ["x0", "x1", "x2", "x3", "x4", "x5", "x6", "x7"];
    let mut arguments = Vec::new();
    for i in 0..num_params as usize {
        let abi_index = i + has_sret as usize;
        let location = if abi_index < 8 {
            ArgPassingLocation::Register(arg_regs[abi_index].to_string())
        } else {
            // Stack arguments on AArch64
            let offset = ((abi_index - 8) as i32) * 8;
            ArgPassingLocation::Stack { offset }
        };
        arguments.push(ArgumentLocation {
            index: i,
            name: None,
            ty: "i64".to_string(),
            location,
        });
    }

    // Return location (sret returns go through the caller-allocated buffer
    // whose address arrived in x0)
    let return_ty = format!("{:?}", cfg.return_type());
    let return_location = ReturnLocation {
        ty: return_ty,
        registers: if has_sret {
            vec!["sret buffer (via x0)".to_string()]
        } else {
            vec!["x0".to_string()]
        },
    };

    Ok(StackFrameInfo {
        function_name: function_name.to_string(),
        frame_size,
        alignment: frame.alignment() as usize,
        uses_frame_pointer,
        slots,
        arguments,
        return_location,
        target,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use lasso::ThreadedRodeo;
    use rue_air::{
        AirEditor, AirValidationContext, EnumDef, EnumId, FrozenTypeInternPool, ParamSlotModes,
        SourceParamAbi, StructDef, StructField, StructId, Type, TypeInternPool,
    };
    use rue_cfg::{
        BlockId, Cfg, CfgArgMode, CfgBuilder, CfgCallArg, CfgInst, CfgInstData, CfgValue,
        PlaceBase, Projection, ValidatedCfg,
    };
    use rue_span::{FileId, Span};

    fn span() -> Span {
        Span::new(0, 0)
    }

    fn value(cfg: &mut Cfg, block: BlockId, data: CfgInstData, ty: Type) -> CfgValue {
        cfg.append_inst(
            block,
            CfgInst {
                data,
                ty,
                span: span(),
            },
        )
    }

    fn konst(cfg: &mut Cfg, block: BlockId, literal: u64, ty: Type) -> CfgValue {
        value(cfg, block, CfgInstData::Const(literal), ty)
    }

    fn unit_const(cfg: &mut Cfg, block: BlockId) -> CfgValue {
        konst(cfg, block, 0, Type::UNIT)
    }

    fn storage_live(cfg: &mut Cfg, block: BlockId, slot: u32, local_ty: Type) {
        value(
            cfg,
            block,
            CfgInstData::StorageLive { slot, local_ty },
            Type::UNIT,
        );
    }

    fn storage_dead(cfg: &mut Cfg, block: BlockId, slot: u32, local_ty: Type) {
        value(
            cfg,
            block,
            CfgInstData::StorageDead { slot, local_ty },
            Type::UNIT,
        );
    }

    fn alloc_slot(cfg: &mut Cfg, block: BlockId, slot: u32, init: CfgValue) {
        value(cfg, block, CfgInstData::Alloc { slot, init }, Type::UNIT);
    }

    fn load_slot(cfg: &mut Cfg, block: BlockId, slot: u32, ty: Type) -> CfgValue {
        value(cfg, block, CfgInstData::Load { slot }, ty)
    }

    fn store_slot(cfg: &mut Cfg, block: BlockId, slot: u32, stored: CfgValue) {
        value(
            cfg,
            block,
            CfgInstData::Store {
                slot,
                value: stored,
            },
            Type::UNIT,
        );
    }

    /// One direct scalar ABI descriptor per parameter slot.
    fn scalar_param_abi(count: u32) -> Vec<SourceParamAbi> {
        (0..count)
            .map(|slot| SourceParamAbi {
                start_slot: slot,
                slot_count: 1,
                crossing_regs: 1,
                ty: None,
            })
            .collect()
    }

    fn register_struct(
        pool: &TypeInternPool,
        interner: &ThreadedRodeo,
        name: &str,
        fields: &[(&str, Type)],
    ) -> StructId {
        let (id, _) = pool.register_struct(
            interner.get_or_intern(name),
            StructDef {
                name: name.into(),
                fields: fields
                    .iter()
                    .map(|(field, ty)| StructField {
                        name: (*field).to_string(),
                        ty: *ty,
                    })
                    .collect(),
                is_copy: false,
                is_linear: false,
                destructor: None,
                is_builtin: false,
                is_pub: false,
                file_id: FileId::DEFAULT,
            },
        );
        id
    }

    fn register_enum(
        pool: &TypeInternPool,
        interner: &ThreadedRodeo,
        name: &str,
        variants: &[(&str, Vec<Type>)],
    ) -> EnumId {
        let (id, _) = pool.register_enum(
            interner.get_or_intern(name),
            EnumDef {
                name: name.into(),
                variants: variants
                    .iter()
                    .map(|(variant, _)| (*variant).into())
                    .collect(),
                variant_payloads: variants
                    .iter()
                    .map(|(_, payload)| payload.clone())
                    .collect(),
                is_pub: false,
                file_id: FileId::DEFAULT,
            },
        );
        id
    }

    fn create_simple_cfg() -> (rue_cfg::ValidatedCfg, FrozenTypeInternPool, ThreadedRodeo) {
        let mut air = AirEditor::new(Type::I32);

        let const_ref = air.add_const(42, Type::I32, Span::new(0, 2));
        air.add_ret(Some(const_ref), Type::I32, Span::new(0, 2));

        let interner = ThreadedRodeo::new();
        let type_pool = FrozenTypeInternPool::new();
        let air = air
            .finish(AirValidationContext::Canonical(&type_pool))
            .expect("test AIR must validate");
        let cfg_output = CfgBuilder::build(
            &air,
            0,
            0,
            "test",
            &type_pool,
            vec![],
            &interner,
            false,
            rue_air::AnalyzedCallableKind::Ordinary,
        );
        (cfg_output.cfg.unwrap(), type_pool, interner)
    }

    #[test]
    fn test_generate_stack_frame_info_x86_64() {
        let (cfg, type_pool, interner) = create_simple_cfg();
        let target = Target::X86_64Linux;

        let info = generate_stack_frame_info(&cfg, "test", &type_pool, &interner, target).unwrap();

        assert_eq!(info.function_name, "test");
        assert!(!info.return_location.registers.is_empty());
        // `return 42` is a leaf with no locals, parameters, spills, or sret
        // pointer, so it gets no frame pointer and no slot region: the frame is
        // just the callee-saved push the allocator needed (RUE-1171).
        assert!(!info.uses_frame_pointer);
        assert_eq!(info.alignment, 8);
        assert!(
            info.slots
                .iter()
                .all(|slot| slot.kind == StackSlotKind::CalleeSaved),
            "a frameless leaf reports no slot-region cells: {:?}",
            info.slots
        );
        let output = info.to_string();
        assert!(output.contains("Frame pointer: none (frameless leaf)"));
        assert!(output.contains("Layout (sp-relative):"));
    }

    #[test]
    fn test_stack_frame_display() {
        let info = StackFrameInfo {
            function_name: "main".to_string(),
            frame_size: 32,
            alignment: 16,
            uses_frame_pointer: true,
            slots: vec![
                StackSlot {
                    name: Some("saved rbx".to_string()),
                    offset: -8,
                    size: 8,
                    ty: "i64".to_string(),
                    kind: StackSlotKind::CalleeSaved,
                },
                StackSlot {
                    name: Some("x".to_string()),
                    offset: -16,
                    size: 4,
                    ty: "i32".to_string(),
                    kind: StackSlotKind::Local,
                },
            ],
            arguments: vec![ArgumentLocation {
                index: 0,
                name: Some("n".to_string()),
                ty: "i32".to_string(),
                location: ArgPassingLocation::Register("rdi".to_string()),
            }],
            return_location: ReturnLocation {
                ty: "i32".to_string(),
                registers: vec!["rax".to_string()],
            },
            target: Target::X86_64Linux,
        };

        let output = info.to_string();
        assert!(output.contains("Stack Frame (main)"));
        assert!(output.contains("Frame size: 32 bytes"));
        assert!(output.contains("saved rbx"));
        assert!(output.contains("rdi"));
        assert!(output.contains("rax"));
    }

    /// Assembly and reported layout for a validated CFG on `target`, from the
    /// same production backend the compiler runs.
    fn frame_projection(
        cfg: &ValidatedCfg,
        type_pool: &FrozenTypeInternPool,
        interner: &ThreadedRodeo,
        target: Target,
    ) -> (String, StackFrameInfo) {
        let request = crate::BackendArtifactRequest {
            asm: true,
            ..Default::default()
        };
        let product = match target.arch() {
            Arch::X86_64 => crate::x86_64::generate_product_with_symbols_and_atoms(
                &cfg,
                &type_pool,
                &[],
                &interner,
                crate::MachineSymbolResolver::default(),
                &[],
                request,
            ),
            Arch::Aarch64 => crate::aarch64::generate_product_with_symbols_and_atoms(
                &cfg,
                &type_pool,
                &[],
                &interner,
                target,
                crate::MachineSymbolResolver::default(),
                &[],
                request,
            ),
        }
        .expect("backend generation should succeed");
        let info = generate_stack_frame_info(cfg, cfg.fn_name(), type_pool, interner, target)
            .expect("stack frame projection should succeed");
        (product.artifacts.asm.expect("assembly projection"), info)
    }

    /// `fn frameless(n: i32) -> i32 { n }`: a leaf whose scalar register
    /// argument is read directly (RUE-1170), never homed to the frame.
    fn frameless_cfg() -> (ValidatedCfg, FrozenTypeInternPool, ThreadedRodeo) {
        let interner = ThreadedRodeo::new();
        let pool = FrozenTypeInternPool::new();
        let mut cfg = Cfg::new(
            Type::I32,
            0,
            1,
            "frameless".to_string(),
            ParamSlotModes::new(vec![false], vec![false]),
        );
        cfg.set_source_param_abi(scalar_param_abi(1));
        let entry = cfg.new_block();
        cfg.entry = entry;
        let n = value(&mut cfg, entry, CfgInstData::Param { index: 0 }, Type::I32);
        cfg.set_return(entry, Some(n));
        (
            cfg.finish(&pool).expect("test CFG must verify"),
            pool,
            interner,
        )
    }

    /// `fn framed() -> i32 { let mut total = 41; bump(inout total); total }`:
    /// the local's address escapes through the by-reference call argument, so
    /// it needs a real frame slot and the full prologue/epilogue.
    fn framed_cfg() -> (ValidatedCfg, FrozenTypeInternPool, ThreadedRodeo) {
        let interner = ThreadedRodeo::new();
        let pool = FrozenTypeInternPool::new();
        let mut cfg = Cfg::new(Type::I32, 1, 0, "framed".to_string(), Vec::new());
        let entry = cfg.new_block();
        cfg.entry = entry;
        storage_live(&mut cfg, entry, 0, Type::I32);
        let init = konst(&mut cfg, entry, 41, Type::I32);
        alloc_slot(&mut cfg, entry, 0, init);
        let arg = load_slot(&mut cfg, entry, 0, Type::I32);
        cfg.append_call(
            entry,
            None,
            interner.get_or_intern("bump"),
            [CfgCallArg {
                value: arg,
                mode: CfgArgMode::Inout,
            }],
            Type::UNIT,
            span(),
        )
        .unwrap();
        let result = load_slot(&mut cfg, entry, 0, Type::I32);
        storage_dead(&mut cfg, entry, 0, Type::I32);
        cfg.set_return(entry, Some(result));
        (
            cfg.finish(&pool).expect("test CFG must verify"),
            pool,
            interner,
        )
    }

    /// RUE-1171: a leaf function with no locals, spills, homed parameters, sret
    /// pointer, calls, or stack arguments emits no frame allocation and no
    /// frame-pointer setup on either backend. Adding one real frame consumer —
    /// here a homed parameter — restores the full prologue and epilogue. The
    /// reported stack frame agrees with the emitted code in both cases.
    #[test]
    fn frameless_leaves_elide_the_frame_and_one_consumer_restores_it() {
        // `frameless` reads its argument straight from its incoming register
        // (RUE-1170), so even a parameter-consuming leaf elides the frame. A
        // local that must be addressable — its address escapes through a
        // by-reference call argument in `framed` — is what restores the
        // frame: it needs a real frame slot.
        let (frameless, frameless_pool, frameless_interner) = frameless_cfg();
        let (framed, framed_pool, framed_interner) = framed_cfg();

        // Frame-pointer setup and slot allocation, per backend. These are the
        // exact mnemonics the prologue/epilogue emit.
        let frame_markers = |target: Target| -> &'static [&'static str] {
            match target.arch() {
                Arch::X86_64 => &["push rbp", "mov rbp, rsp", "pop rbp", "lea rsp, [rbp"],
                Arch::Aarch64 => &["stp x29, x30", "mov x29, sp", "ldp x29, x30"],
            }
        };

        for target in [Target::X86_64Linux, Target::Aarch64Linux] {
            let (asm, info) =
                frame_projection(&frameless, &frameless_pool, &frameless_interner, target);
            for marker in frame_markers(target) {
                assert!(
                    !asm.contains(marker),
                    "frameless leaf still emits `{marker}` on {target:?}:\n{asm}"
                );
            }
            assert!(
                !asm.contains("sub rsp") && !asm.contains("sub sp, sp"),
                "frameless leaf still allocates a frame on {target:?}:\n{asm}"
            );
            // Only the callee-saved registers the allocator actually used are
            // saved, and the reported frame is exactly those pushes.
            assert!(!info.uses_frame_pointer);
            assert!(
                info.slots
                    .iter()
                    .all(|slot| slot.kind == StackSlotKind::CalleeSaved),
                "frameless leaf reports slot-region cells on {target:?}: {:?}",
                info.slots
            );
            let scheme = match target.arch() {
                Arch::X86_64 => crate::frame_layout::SavedRegScheme::X86_64,
                Arch::Aarch64 => crate::frame_layout::SavedRegScheme::Aarch64,
            };
            let saved_bytes = scheme.callee_saved_bytes(saved_register_count(&info));
            assert_eq!(
                info.frame_size, saved_bytes as usize,
                "reported frame on {target:?} must be exactly the callee-saved pushes"
            );

            let (asm, info) = frame_projection(&framed, &framed_pool, &framed_interner, target);
            for marker in frame_markers(target) {
                assert!(
                    asm.contains(marker),
                    "one addressable local must restore `{marker}` on {target:?}:\n{asm}"
                );
            }
            assert!(info.uses_frame_pointer);
            assert!(
                info.slots
                    .iter()
                    .any(|slot| slot.kind == StackSlotKind::Local),
                "the restored frame must report its local slot on {target:?}"
            );
        }
    }

    fn saved_register_count(info: &StackFrameInfo) -> usize {
        info.slots
            .iter()
            .filter(|slot| slot.kind == StackSlotKind::CalleeSaved)
            .count()
    }

    /// The marker-driven local frame-slot plan for a validated CFG, with the
    /// CFG's own local slot count for comparison (RUE-768).
    fn local_plan(
        cfg: &ValidatedCfg,
        type_pool: &FrozenTypeInternPool,
        interner: &ThreadedRodeo,
    ) -> (crate::local_storage::LocalSlotPlan, u32) {
        let plan = crate::local_storage::LocalSlotPlan::plan(cfg, type_pool, interner);
        (plan, cfg.num_locals())
    }

    /// A whole-function CFG with the marker pattern the pipeline emits for
    /// `total` (slot 0, live throughout) plus two inner blocks whose pairs of
    /// scalar temporaries (slots 1/2 and 3/4) open and close their storage
    /// windows before the next block's open:
    ///
    /// ```text
    /// fn main() -> i32 {
    ///     let mut total = 0;
    ///     { let a = 1; let b = 2; total = total + a + b; }
    ///     { let c = 3; let d = 4; total = total + c + d; }
    ///     0
    /// }
    /// ```
    fn disjoint_windows_cfg() -> (ValidatedCfg, FrozenTypeInternPool, ThreadedRodeo) {
        let interner = ThreadedRodeo::new();
        let pool = FrozenTypeInternPool::new();
        let mut cfg = Cfg::new(Type::I32, 5, 0, "main".to_string(), Vec::new());
        let entry = cfg.new_block();
        cfg.entry = entry;
        storage_live(&mut cfg, entry, 0, Type::I32);
        let zero = konst(&mut cfg, entry, 0, Type::I32);
        alloc_slot(&mut cfg, entry, 0, zero);
        for (base_slot, literals) in [(1, [1u64, 2]), (3, [3, 4])] {
            for (offset, literal) in literals.into_iter().enumerate() {
                let slot = base_slot + offset as u32;
                storage_live(&mut cfg, entry, slot, Type::I32);
                let init = konst(&mut cfg, entry, literal, Type::I32);
                alloc_slot(&mut cfg, entry, slot, init);
            }
            let mut sum = load_slot(&mut cfg, entry, 0, Type::I32);
            for offset in 0..2 {
                let operand = load_slot(&mut cfg, entry, base_slot + offset, Type::I32);
                sum = value(&mut cfg, entry, CfgInstData::Add(sum, operand), Type::I32);
            }
            store_slot(&mut cfg, entry, 0, sum);
            unit_const(&mut cfg, entry);
            storage_dead(&mut cfg, entry, base_slot + 1, Type::I32);
            storage_dead(&mut cfg, entry, base_slot, Type::I32);
        }
        let result = konst(&mut cfg, entry, 0, Type::I32);
        storage_dead(&mut cfg, entry, 0, Type::I32);
        cfg.set_return(entry, Some(result));
        (
            cfg.finish(&pool).expect("test CFG must verify"),
            pool,
            interner,
        )
    }

    /// RUE-768: locals whose storage windows the markers prove disjoint land on
    /// the same frame cells; a local live across both of them does not.
    #[test]
    fn disjoint_storage_windows_share_frame_cells() {
        let (cfg, type_pool, interner) = disjoint_windows_cfg();
        let (plan, num_locals) = local_plan(&cfg, &type_pool, &interner);
        assert_eq!(num_locals, 5, "`total` plus two pairs of block temporaries");
        assert_eq!(plan.frame_local_slots(), 3);
        // `total` (slot 0) is live throughout and keeps a cell of its own; the
        // two pairs overlay each other exactly.
        assert_eq!(plan.frame_slot(0), 0);
        assert_eq!(plan.frame_slot(1), plan.frame_slot(3));
        assert_eq!(plan.frame_slot(2), plan.frame_slot(4));
        assert_ne!(plan.frame_slot(0), plan.frame_slot(1));
        assert_ne!(plan.frame_slot(1), plan.frame_slot(2));
    }

    /// Four scalar locals whose storage windows all stay open until the end of
    /// the function — the marker pattern for
    /// `let a = 1; let b = 2; let c = 3; let d = 4; a + b + c + d`.
    fn overlapping_windows_cfg() -> (ValidatedCfg, FrozenTypeInternPool, ThreadedRodeo) {
        let interner = ThreadedRodeo::new();
        let pool = FrozenTypeInternPool::new();
        let mut cfg = Cfg::new(Type::I32, 4, 0, "main".to_string(), Vec::new());
        let entry = cfg.new_block();
        cfg.entry = entry;
        for slot in 0..4 {
            storage_live(&mut cfg, entry, slot, Type::I32);
            let init = konst(&mut cfg, entry, u64::from(slot) + 1, Type::I32);
            alloc_slot(&mut cfg, entry, slot, init);
        }
        let mut sum = load_slot(&mut cfg, entry, 0, Type::I32);
        for slot in 1..4 {
            let operand = load_slot(&mut cfg, entry, slot, Type::I32);
            sum = value(&mut cfg, entry, CfgInstData::Add(sum, operand), Type::I32);
        }
        for slot in (0..4).rev() {
            storage_dead(&mut cfg, entry, slot, Type::I32);
        }
        cfg.set_return(entry, Some(sum));
        (
            cfg.finish(&pool).expect("test CFG must verify"),
            pool,
            interner,
        )
    }

    /// RUE-768: simultaneously live locals must never be merged. This is the
    /// silent-corruption case, so it is asserted slot by slot rather than only
    /// through the frame total.
    #[test]
    fn overlapping_storage_windows_never_share() {
        let (cfg, type_pool, interner) = overlapping_windows_cfg();
        let (plan, num_locals) = local_plan(&cfg, &type_pool, &interner);
        assert_eq!(num_locals, 4);
        assert_eq!(plan.frame_local_slots(), 4, "nothing may be merged");
        let cells: Vec<u32> = (0..num_locals).map(|slot| plan.frame_slot(slot)).collect();
        let mut sorted = cells.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), cells.len(), "every window keeps its own cell");
    }

    /// RUE-768: a borrow that keeps a local in use after another local's whole
    /// scope has come and gone holds the first local's storage window open, so
    /// the two must not share. A last-direct-use analysis would merge them —
    /// `held`'s last direct read is `held.a`, before `scoped` even exists.
    #[test]
    fn borrow_extended_lifetime_keeps_its_own_cells() {
        // The CFG models:
        //
        //     let held = Pair { a: 1, b: 2 };   // slots 0..2
        //     let direct = held.a;              // slot 2
        //     let mut sum = direct;             // slot 3
        //     {
        //         let scoped = Pair { a: 100, b: 200 };   // slots 4..6
        //         sum = sum + total(borrow scoped);
        //     }
        //     sum + total(borrow held)
        //
        // `held`'s last direct read (`held.a`) happens before `scoped` exists,
        // but its storage window stays open across `scoped`'s whole scope for
        // the final borrow argument.
        let interner = ThreadedRodeo::new();
        let pool = TypeInternPool::new();
        let pair_id = register_struct(
            &pool,
            &interner,
            "Pair",
            &[("a", Type::I32), ("b", Type::I32)],
        );
        let pair_ty = Type::new_struct(pair_id);
        let pool = pool.freeze();
        let total = interner.get_or_intern("total");
        let mut cfg = Cfg::new(Type::I32, 6, 0, "main".to_string(), Vec::new());
        let entry = cfg.new_block();
        cfg.entry = entry;
        storage_live(&mut cfg, entry, 0, pair_ty);
        let one = konst(&mut cfg, entry, 1, Type::I32);
        let two = konst(&mut cfg, entry, 2, Type::I32);
        let held = cfg
            .append_struct_init(entry, pair_id, [one, two], pair_ty, span())
            .unwrap();
        alloc_slot(&mut cfg, entry, 0, held);
        storage_live(&mut cfg, entry, 2, Type::I32);
        let held_a = cfg
            .append_place_read(
                entry,
                PlaceBase::Local(0),
                pair_ty,
                [Projection::Field {
                    struct_id: pair_id,
                    field_index: 0,
                }],
                Type::I32,
                span(),
            )
            .unwrap();
        alloc_slot(&mut cfg, entry, 2, held_a);
        storage_live(&mut cfg, entry, 3, Type::I32);
        let direct = load_slot(&mut cfg, entry, 2, Type::I32);
        alloc_slot(&mut cfg, entry, 3, direct);
        storage_live(&mut cfg, entry, 4, pair_ty);
        let hundred = konst(&mut cfg, entry, 100, Type::I32);
        let two_hundred = konst(&mut cfg, entry, 200, Type::I32);
        let scoped = cfg
            .append_struct_init(entry, pair_id, [hundred, two_hundred], pair_ty, span())
            .unwrap();
        alloc_slot(&mut cfg, entry, 4, scoped);
        let sum = load_slot(&mut cfg, entry, 3, Type::I32);
        let scoped_arg = load_slot(&mut cfg, entry, 4, pair_ty);
        let scoped_total = cfg
            .append_call(
                entry,
                None,
                total,
                [CfgCallArg {
                    value: scoped_arg,
                    mode: CfgArgMode::Borrow,
                }],
                Type::I32,
                span(),
            )
            .unwrap();
        let new_sum = value(
            &mut cfg,
            entry,
            CfgInstData::Add(sum, scoped_total),
            Type::I32,
        );
        store_slot(&mut cfg, entry, 3, new_sum);
        unit_const(&mut cfg, entry);
        storage_dead(&mut cfg, entry, 4, pair_ty);
        let sum = load_slot(&mut cfg, entry, 3, Type::I32);
        let held_arg = load_slot(&mut cfg, entry, 0, pair_ty);
        let held_total = cfg
            .append_call(
                entry,
                None,
                total,
                [CfgCallArg {
                    value: held_arg,
                    mode: CfgArgMode::Borrow,
                }],
                Type::I32,
                span(),
            )
            .unwrap();
        let result = value(
            &mut cfg,
            entry,
            CfgInstData::Add(sum, held_total),
            Type::I32,
        );
        storage_dead(&mut cfg, entry, 3, Type::I32);
        storage_dead(&mut cfg, entry, 2, Type::I32);
        storage_dead(&mut cfg, entry, 0, pair_ty);
        cfg.set_return(entry, Some(result));
        let cfg = cfg.finish(&pool).expect("test CFG must verify");

        let (plan, num_locals) = local_plan(&cfg, &pool, &interner);
        // `held` occupies slots 0..2, `direct` 2, `sum` 3, `scoped` 4..6.
        assert_eq!(num_locals, 6);
        assert_eq!(
            plan.frame_local_slots(),
            6,
            "the borrow keeps `held` live across `scoped`'s whole scope"
        );
        assert_ne!(plan.frame_slot(0), plan.frame_slot(4));
        assert_ne!(plan.frame_slot(1), plan.frame_slot(5));
    }

    /// RUE-768: a multi-slot aggregate moves as one contiguous run, so an
    /// aggregate that overlays another covers exactly its whole span and its
    /// fields stay in order. A per-slot merge would interleave two structs.
    #[test]
    fn multi_slot_aggregates_share_as_whole_runs() {
        // The CFG models:
        //
        //     let mut sum = 0;                            // slot 0
        //     { let x = Triple { a: 1, b: 2, c: 3 };      // slots 1..4
        //       sum = sum + x.a + x.b + x.c; }
        //     { let y = Triple { a: 4, b: 5, c: 6 };      // slots 4..7
        //       sum = sum + y.a + y.b + y.c; }
        //     sum
        let interner = ThreadedRodeo::new();
        let pool = TypeInternPool::new();
        let triple_id = register_struct(
            &pool,
            &interner,
            "Triple",
            &[("a", Type::I32), ("b", Type::I32), ("c", Type::I32)],
        );
        let triple_ty = Type::new_struct(triple_id);
        let pool = pool.freeze();
        let mut cfg = Cfg::new(Type::I32, 7, 0, "main".to_string(), Vec::new());
        let entry = cfg.new_block();
        cfg.entry = entry;
        storage_live(&mut cfg, entry, 0, Type::I32);
        let zero = konst(&mut cfg, entry, 0, Type::I32);
        alloc_slot(&mut cfg, entry, 0, zero);
        for (base_slot, literals) in [(1u32, [1u64, 2, 3]), (4, [4, 5, 6])] {
            storage_live(&mut cfg, entry, base_slot, triple_ty);
            let fields: Vec<CfgValue> = literals
                .into_iter()
                .map(|literal| konst(&mut cfg, entry, literal, Type::I32))
                .collect();
            let aggregate = cfg
                .append_struct_init(entry, triple_id, fields, triple_ty, span())
                .unwrap();
            alloc_slot(&mut cfg, entry, base_slot, aggregate);
            let mut sum = load_slot(&mut cfg, entry, 0, Type::I32);
            for field_index in 0..3 {
                let field = cfg
                    .append_place_read(
                        entry,
                        PlaceBase::Local(base_slot),
                        triple_ty,
                        [Projection::Field {
                            struct_id: triple_id,
                            field_index,
                        }],
                        Type::I32,
                        span(),
                    )
                    .unwrap();
                sum = value(&mut cfg, entry, CfgInstData::Add(sum, field), Type::I32);
            }
            store_slot(&mut cfg, entry, 0, sum);
            unit_const(&mut cfg, entry);
            storage_dead(&mut cfg, entry, base_slot, triple_ty);
        }
        let result = load_slot(&mut cfg, entry, 0, Type::I32);
        storage_dead(&mut cfg, entry, 0, Type::I32);
        cfg.set_return(entry, Some(result));
        let cfg = cfg.finish(&pool).expect("test CFG must verify");

        let (plan, num_locals) = local_plan(&cfg, &pool, &interner);
        assert_eq!(num_locals, 7, "`sum` plus two three-slot structs");
        assert_eq!(plan.frame_local_slots(), 4);
        let x = plan.frame_slot(1);
        let y = plan.frame_slot(4);
        assert_eq!(x, y, "the two structs overlay each other");
        // Each struct stays contiguous and ascending, which is what the
        // `frame_slot(base) + k` addressing in both backends assumes.
        for k in 0..3 {
            assert_eq!(plan.frame_slot(1 + k), x + k);
            assert_eq!(plan.frame_slot(4 + k), y + k);
        }
    }

    /// RUE-768: `@raw_mut` hands out a first-class pointer the planner cannot
    /// follow, so its operand keeps a private cell even when its storage window
    /// is disjoint from a later local's.
    #[test]
    fn raw_pointer_operands_keep_private_cells() {
        // The CFG models:
        //
        //     let mut sum = 0;                              // slot 0
        //     { let mut probe: i32 = 5;                     // slot 1
        //       let p: ptr mut i32 = @raw_mut(probe);       // slot 2
        //       @ptr_write(p, 9);
        //       sum = sum + probe; }
        //     { let other: i32 = 41;                        // slot 3
        //       sum = sum + other; }
        //     sum
        //
        // `probe`'s address escapes through `@raw_mut`, which also marks the
        // slot address-taken on the CFG, exactly as the builder does.
        let interner = ThreadedRodeo::new();
        let pool = TypeInternPool::new();
        let ptr_ty = Type::new_ptr_mut(pool.intern_ptr_mut_from_type(Type::I32));
        let pool = pool.freeze();
        let mut cfg = Cfg::new(Type::I32, 4, 0, "main".to_string(), Vec::new());
        cfg.mark_address_taken(1);
        let entry = cfg.new_block();
        cfg.entry = entry;
        storage_live(&mut cfg, entry, 0, Type::I32);
        let zero = konst(&mut cfg, entry, 0, Type::I32);
        alloc_slot(&mut cfg, entry, 0, zero);
        storage_live(&mut cfg, entry, 1, Type::I32);
        let five = konst(&mut cfg, entry, 5, Type::I32);
        alloc_slot(&mut cfg, entry, 1, five);
        storage_live(&mut cfg, entry, 2, ptr_ty);
        let probe = load_slot(&mut cfg, entry, 1, Type::I32);
        let raw = cfg
            .append_intrinsic(
                entry,
                None,
                interner.get_or_intern("raw_mut"),
                [probe],
                ptr_ty,
                span(),
            )
            .unwrap();
        alloc_slot(&mut cfg, entry, 2, raw);
        let pointer = load_slot(&mut cfg, entry, 2, ptr_ty);
        let nine = konst(&mut cfg, entry, 9, Type::I32);
        cfg.append_intrinsic(
            entry,
            None,
            interner.get_or_intern("ptr_write"),
            [pointer, nine],
            Type::UNIT,
            span(),
        )
        .unwrap();
        unit_const(&mut cfg, entry);
        unit_const(&mut cfg, entry);
        let sum = load_slot(&mut cfg, entry, 0, Type::I32);
        let probe = load_slot(&mut cfg, entry, 1, Type::I32);
        let new_sum = value(&mut cfg, entry, CfgInstData::Add(sum, probe), Type::I32);
        store_slot(&mut cfg, entry, 0, new_sum);
        unit_const(&mut cfg, entry);
        storage_dead(&mut cfg, entry, 2, ptr_ty);
        storage_dead(&mut cfg, entry, 1, Type::I32);
        storage_live(&mut cfg, entry, 3, Type::I32);
        let forty_one = konst(&mut cfg, entry, 41, Type::I32);
        alloc_slot(&mut cfg, entry, 3, forty_one);
        let sum = load_slot(&mut cfg, entry, 0, Type::I32);
        let other = load_slot(&mut cfg, entry, 3, Type::I32);
        let new_sum = value(&mut cfg, entry, CfgInstData::Add(sum, other), Type::I32);
        store_slot(&mut cfg, entry, 0, new_sum);
        unit_const(&mut cfg, entry);
        storage_dead(&mut cfg, entry, 3, Type::I32);
        let result = load_slot(&mut cfg, entry, 0, Type::I32);
        storage_dead(&mut cfg, entry, 0, Type::I32);
        cfg.set_return(entry, Some(result));
        let cfg = cfg.finish(&pool).expect("test CFG must verify");

        let (plan, num_locals) = local_plan(&cfg, &pool, &interner);
        // `sum` 0, `probe` 1, `p` 2, `other` 3.
        assert_eq!(num_locals, 4);
        assert_ne!(
            plan.frame_slot(1),
            plan.frame_slot(3),
            "an address-escaping local must not be merged"
        );
        // The pointer local itself has no escaping address, so it still shares
        // with the disjoint `other`.
        assert_eq!(plan.frame_slot(2), plan.frame_slot(3));
        assert_eq!(plan.frame_local_slots(), 3);
    }

    /// RUE-768: both backends must agree on the shared layout, and both must
    /// report only the cells the plan kept. `area`'s per-arm payload bindings
    /// are the shapes.rue shape that motivated the issue.
    /// The CFG the pipeline builds for the shapes.rue `area` function:
    ///
    /// ```text
    /// fn area(s: Shape) -> i32 {
    ///     match s {
    ///         Shape.Circle(r) => 3 * r * r,
    ///         Shape.Rect(w, h) => w * h,
    ///         Shape.Square(side) => side * side,
    ///     }
    /// }
    /// ```
    ///
    /// A switch on the discriminant with one payload-binding local per arm
    /// (`r` 0, `w`/`h` 1/2, `side` 3), each arm opening and closing its own
    /// storage window before joining on a block parameter.
    fn shapes_area_cfg() -> (ValidatedCfg, FrozenTypeInternPool, ThreadedRodeo) {
        let interner = ThreadedRodeo::new();
        let pool = TypeInternPool::new();
        let shape_id = register_enum(
            &pool,
            &interner,
            "Shape",
            &[
                ("Circle", vec![Type::I32]),
                ("Rect", vec![Type::I32, Type::I32]),
                ("Square", vec![Type::I32]),
            ],
        );
        let shape_ty = Type::new_enum(shape_id);
        let pool = pool.freeze();
        let mut cfg = Cfg::new(
            Type::I32,
            4,
            3,
            "area".to_string(),
            ParamSlotModes::new(vec![false; 3], vec![false; 3]),
        );
        cfg.set_source_param_abi(vec![SourceParamAbi {
            start_slot: 0,
            slot_count: 3,
            crossing_regs: 1,
            ty: Some(shape_ty),
        }]);
        let entry = cfg.new_block();
        cfg.entry = entry;
        let circle_arm = cfg.new_block();
        let rect_arm = cfg.new_block();
        let square_arm = cfg.new_block();
        let join = cfg.new_block();
        let join_value = cfg.add_block_param(join, Type::I32);

        let scrutinee = value(&mut cfg, entry, CfgInstData::Param { index: 0 }, shape_ty);
        cfg.set_switch(
            entry,
            scrutinee,
            [(0, circle_arm), (1, rect_arm)],
            square_arm,
        );

        // Circle(r): 3 * r * r, binding `r` in slot 0.
        storage_live(&mut cfg, circle_arm, 0, Type::I32);
        let payload = value(
            &mut cfg,
            circle_arm,
            CfgInstData::EnumPayloadGet {
                base: scrutinee,
                enum_id: shape_id,
                variant_index: 0,
                field_index: 0,
            },
            Type::I32,
        );
        alloc_slot(&mut cfg, circle_arm, 0, payload);
        let three = konst(&mut cfg, circle_arm, 3, Type::I32);
        let r = load_slot(&mut cfg, circle_arm, 0, Type::I32);
        let partial = value(&mut cfg, circle_arm, CfgInstData::Mul(three, r), Type::I32);
        let r = load_slot(&mut cfg, circle_arm, 0, Type::I32);
        let product = value(
            &mut cfg,
            circle_arm,
            CfgInstData::Mul(partial, r),
            Type::I32,
        );
        storage_dead(&mut cfg, circle_arm, 0, Type::I32);
        cfg.set_goto(circle_arm, join, [product]);

        // Rect(w, h): w * h, binding `w`/`h` in slots 1/2.
        for (slot, field_index) in [(1u32, 0u32), (2, 1)] {
            storage_live(&mut cfg, rect_arm, slot, Type::I32);
            let payload = value(
                &mut cfg,
                rect_arm,
                CfgInstData::EnumPayloadGet {
                    base: scrutinee,
                    enum_id: shape_id,
                    variant_index: 1,
                    field_index,
                },
                Type::I32,
            );
            alloc_slot(&mut cfg, rect_arm, slot, payload);
        }
        let w = load_slot(&mut cfg, rect_arm, 1, Type::I32);
        let h = load_slot(&mut cfg, rect_arm, 2, Type::I32);
        let product = value(&mut cfg, rect_arm, CfgInstData::Mul(w, h), Type::I32);
        storage_dead(&mut cfg, rect_arm, 2, Type::I32);
        storage_dead(&mut cfg, rect_arm, 1, Type::I32);
        cfg.set_goto(rect_arm, join, [product]);

        // Square(side): side * side, binding `side` in slot 3.
        storage_live(&mut cfg, square_arm, 3, Type::I32);
        let payload = value(
            &mut cfg,
            square_arm,
            CfgInstData::EnumPayloadGet {
                base: scrutinee,
                enum_id: shape_id,
                variant_index: 2,
                field_index: 0,
            },
            Type::I32,
        );
        alloc_slot(&mut cfg, square_arm, 3, payload);
        let side = load_slot(&mut cfg, square_arm, 3, Type::I32);
        let side_again = load_slot(&mut cfg, square_arm, 3, Type::I32);
        let product = value(
            &mut cfg,
            square_arm,
            CfgInstData::Mul(side, side_again),
            Type::I32,
        );
        storage_dead(&mut cfg, square_arm, 3, Type::I32);
        cfg.set_goto(square_arm, join, [product]);

        cfg.set_return(join, Some(join_value));
        (
            cfg.finish(&pool).expect("test CFG must verify"),
            pool,
            interner,
        )
    }

    #[test]
    fn both_backends_report_the_same_shared_local_area() {
        let (cfg, type_pool, interner) = shapes_area_cfg();
        let (plan, num_locals) = local_plan(&cfg, &type_pool, &interner);
        assert_eq!(
            num_locals, 4,
            "one payload binding per arm, plus Rect's two"
        );
        assert_eq!(
            plan.frame_local_slots(),
            2,
            "at most two arm bindings are ever live at once"
        );

        for target in [Target::X86_64Linux, Target::Aarch64Linux] {
            let (_, info) = frame_projection(&cfg, &type_pool, &interner, target);
            let locals = info
                .slots
                .iter()
                .filter(|slot| slot.kind == StackSlotKind::Local)
                .count();
            assert_eq!(
                locals, 2,
                "{target:?} must report the shared local area, not the CFG's slot count"
            );
        }
    }

    /// RUE-774: the reported AArch64 slots must match the offsets in the emitted
    /// prologue/body. `gcd`'s iterative body forces callee-saved registers, and
    /// its two-slot aggregate parameter keeps its frame home (scalar register
    /// arguments no longer get one, RUE-1170), so it exercises callee-saved,
    /// local, and parameter slots together.
    #[test]
    fn aarch64_reported_slots_match_emitted_instructions() {
        // The CFG models the iterative Euclid loop:
        //
        //     fn gcd(p: Pair) -> i32 {
        //         let mut x = p.a;      // slot 0
        //         let mut y = p.b;      // slot 1
        //         while y != 0 {
        //             let temp = y;     // slot 2
        //             y = x % y;
        //             x = temp;
        //         }
        //         x
        //     }
        //
        // The two-slot `Pair` parameter crosses as one indirect pointer
        // (`crossing_regs: 1` over `slot_count: 2`), so the callee unmarshals
        // it into homed frame slots at entry.
        let interner = ThreadedRodeo::new();
        let pool = TypeInternPool::new();
        let pair_id = register_struct(
            &pool,
            &interner,
            "Pair",
            &[("a", Type::I32), ("b", Type::I32)],
        );
        let pair_ty = Type::new_struct(pair_id);
        let type_pool = pool.freeze();
        let mut cfg = Cfg::new(
            Type::I32,
            3,
            2,
            "gcd".to_string(),
            ParamSlotModes::new(vec![false; 2], vec![false; 2]),
        );
        cfg.set_source_param_abi(vec![SourceParamAbi {
            start_slot: 0,
            slot_count: 2,
            crossing_regs: 1,
            ty: Some(pair_ty),
        }]);
        let entry = cfg.new_block();
        cfg.entry = entry;
        let header = cfg.new_block();
        let body = cfg.new_block();
        let exit = cfg.new_block();

        for (slot, field_index) in [(0u32, 0u32), (1, 1)] {
            storage_live(&mut cfg, entry, slot, Type::I32);
            let field = cfg
                .append_place_read(
                    entry,
                    PlaceBase::Param(0),
                    pair_ty,
                    [Projection::Field {
                        struct_id: pair_id,
                        field_index,
                    }],
                    Type::I32,
                    span(),
                )
                .unwrap();
            alloc_slot(&mut cfg, entry, slot, field);
        }
        cfg.set_goto(entry, header, []);

        let y = load_slot(&mut cfg, header, 1, Type::I32);
        let zero = konst(&mut cfg, header, 0, Type::I32);
        let condition = value(&mut cfg, header, CfgInstData::Ne(y, zero), Type::BOOL);
        cfg.set_branch(header, condition, body, [], exit, []);

        storage_live(&mut cfg, body, 2, Type::I32);
        let y = load_slot(&mut cfg, body, 1, Type::I32);
        alloc_slot(&mut cfg, body, 2, y);
        let x = load_slot(&mut cfg, body, 0, Type::I32);
        let y = load_slot(&mut cfg, body, 1, Type::I32);
        let remainder = value(&mut cfg, body, CfgInstData::Mod(x, y), Type::I32);
        store_slot(&mut cfg, body, 1, remainder);
        let temp = load_slot(&mut cfg, body, 2, Type::I32);
        store_slot(&mut cfg, body, 0, temp);
        unit_const(&mut cfg, body);
        storage_dead(&mut cfg, body, 2, Type::I32);
        cfg.set_goto(body, header, []);

        unit_const(&mut cfg, exit);
        let result = load_slot(&mut cfg, exit, 0, Type::I32);
        storage_dead(&mut cfg, exit, 1, Type::I32);
        storage_dead(&mut cfg, exit, 0, Type::I32);
        cfg.set_return(exit, Some(result));
        let cfg = cfg.finish(&type_pool).expect("test CFG must verify");
        let target = Target::Aarch64Linux;

        let info = generate_stack_frame_info(&cfg, "gcd", &type_pool, &interner, target).unwrap();
        let product = crate::aarch64::generate_product_with_symbols_and_atoms(
            &cfg,
            &type_pool,
            &[],
            &interner,
            target,
            crate::MachineSymbolResolver::default(),
            &[],
            crate::BackendArtifactRequest {
                asm: true,
                ..Default::default()
            },
        )
        .unwrap();
        let asm = product.artifacts.asm.expect("assembly projection");

        // Parameter slots are written at their reported FP-relative offsets:
        // the prologue homes the aggregate's incoming pointer with a
        // `str x0, [x29, #N]`, and the body's unmarshalling fills the
        // remaining slots through `[fp, #N]` stores (RUE-1005). Either way,
        // every reported parameter slot must appear at that exact offset in
        // the emitted assembly.
        let params: Vec<i32> = info
            .slots
            .iter()
            .filter(|s| s.kind == StackSlotKind::Parameter)
            .map(|s| s.offset)
            .collect();
        assert_eq!(params.len(), 2, "gcd homes its aggregate's two slots");
        for offset in &params {
            let prologue_needle = format!("[x29, #{offset}]");
            let body_needle = format!("[fp, #{offset}]");
            assert!(
                asm.contains(&prologue_needle) || asm.contains(&body_needle),
                "reported parameter slot at fp{offset} is not written at that offset in:\n{asm}"
            );
        }

        // Callee-saved registers are pushed in `stp .., [sp, #-16]!` pairs after
        // the FP/LR pair; the reported count must match the emitted pushes and
        // the first pair must land at [fp -16] (the pre-RUE-774 bug reported
        // [fp -32]).
        let saved: Vec<i32> = info
            .slots
            .iter()
            .filter(|s| s.kind == StackSlotKind::CalleeSaved)
            .map(|s| s.offset)
            .collect();
        assert!(
            !saved.is_empty(),
            "gcd must allocate callee-saved registers"
        );
        assert_eq!(saved[0], -16, "first callee-saved slot must be at [fp -16]");
        let predecrement_pushes = asm.matches("[sp, #-16]!").count();
        let callee_saved_pairs = saved.len().div_ceil(2);
        assert_eq!(
            predecrement_pushes,
            1 + callee_saved_pairs,
            "one FP/LR push plus one push per callee-saved pair must be emitted"
        );

        // Nothing should still be reported below the frame's own size.
        for slot in &info.slots {
            assert!(
                slot.offset >= -(info.frame_size as i32),
                "slot offset {} escapes the {}-byte frame",
                slot.offset,
                info.frame_size
            );
        }
    }
}
