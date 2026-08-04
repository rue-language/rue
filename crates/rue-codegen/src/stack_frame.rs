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
    let num_locals = cfg.num_locals();
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
    let num_spills = prepared.total_locals - prepared.num_locals_original;
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
    let num_locals = cfg.num_locals();
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
    let num_spills = prepared.total_locals - prepared.num_locals_original;
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
    use rue_air::{AirEditor, AirValidationContext, FrozenTypeInternPool, Type};
    use rue_cfg::CfgBuilder;
    use rue_span::Span;

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

    /// Assembly and reported layout for `name` on `target`, from the same
    /// production backend the compiler runs.
    fn frame_projection(source: &str, name: &str, target: Target) -> (String, StackFrameInfo) {
        let (cfg, type_pool, interner) = compile_named_fn(source, name);
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
        let info = generate_stack_frame_info(&cfg, name, &type_pool, &interner, target)
            .expect("stack frame projection should succeed");
        (product.artifacts.asm.expect("assembly projection"), info)
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
        let source = "\
fn frameless(n: i32) -> i32 {
    n
}

fn bump(inout n: i32) {
    n = n + 1;
}

fn framed() -> i32 {
    let mut total = 41;
    bump(inout total);
    total
}

fn main() -> i32 {
    frameless(7) + framed()
}
";

        // Frame-pointer setup and slot allocation, per backend. These are the
        // exact mnemonics the prologue/epilogue emit.
        let frame_markers = |target: Target| -> &'static [&'static str] {
            match target.arch() {
                Arch::X86_64 => &["push rbp", "mov rbp, rsp", "pop rbp", "lea rsp, [rbp"],
                Arch::Aarch64 => &["stp x29, x30", "mov x29, sp", "ldp x29, x30"],
            }
        };

        for target in [Target::X86_64Linux, Target::Aarch64Linux] {
            let (asm, info) = frame_projection(source, "frameless", target);
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

            let (asm, info) = frame_projection(source, "framed", target);
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

    /// Compile a Rue function to a validated CFG for the codegen tests.
    fn compile_named_fn(
        source: &str,
        name: &str,
    ) -> (rue_cfg::ValidatedCfg, FrozenTypeInternPool, ThreadedRodeo) {
        use rue_air::{Sema, SemaMetadata};
        use rue_error::PreviewFeatures;
        use rue_lexer::Lexer;
        use rue_parser::Parser;
        use rue_rir::AstGen;

        let (tokens, interner) = Lexer::new(source).tokenize().unwrap();
        let (ast, mut interner) = Parser::new(tokens, interner).parse().unwrap();
        let mut astgen = AstGen::with_symbol_normalizer(&interner, |symbol| symbol);
        astgen.append_items(&ast.items);
        let rir = astgen.finish();
        let output = Sema::new_synthetic(&rir, &mut interner, PreviewFeatures::new())
            .analyze_all_for_test()
            .unwrap();
        let symbol = SemaMetadata::synthetic_root_function_symbol(name);
        let func = output
            .functions
            .iter()
            .find(|f| f.name == symbol)
            .expect("requested test function should exist");
        let cfg = CfgBuilder::build(
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
        .expect("test function CFG must build");
        (cfg, output.type_pool, interner)
    }

    /// RUE-774: the reported AArch64 slots must match the offsets in the emitted
    /// prologue/body. `gcd`'s iterative body forces callee-saved registers, and
    /// its two-slot aggregate parameter keeps its frame home (scalar register
    /// arguments no longer get one, RUE-1170), so it exercises callee-saved,
    /// local, and parameter slots together.
    #[test]
    fn aarch64_reported_slots_match_emitted_instructions() {
        let source = "\
struct Pair {
    a: i32,
    b: i32,
}

fn gcd(p: Pair) -> i32 {
    let mut x = p.a;
    let mut y = p.b;
    while y != 0 {
        let temp = y;
        y = x % y;
        x = temp;
    }
    x
}

fn main() -> i32 {
    gcd(Pair { a: 1071, b: 462 })
}
";
        let (cfg, type_pool, interner) = compile_named_fn(source, "gcd");
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
