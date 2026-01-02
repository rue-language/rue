//! Stack frame information for debugging.
//!
//! This module provides types and functions for extracting stack frame layout
//! information from compiled code. This is useful for debugging ABI issues,
//! calling convention bugs, and understanding how values are laid out on the stack.

use lasso::ThreadedRodeo;
use rue_air::TypeInternPool;
use rue_cfg::Cfg;
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
        let fp_name = match self.target.arch() {
            Arch::X86_64 => "rbp",
            Arch::Aarch64 => "fp",
        };

        writeln!(f, "=== Stack Frame ({}) ===", self.function_name)?;
        writeln!(f)?;
        writeln!(f, "Frame size: {} bytes", self.frame_size)?;
        writeln!(f, "Alignment: {} bytes", self.alignment)?;
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
    cfg: &Cfg,
    function_name: &str,
    type_pool: &TypeInternPool,
    strings: &[String],
    interner: &ThreadedRodeo,
    target: Target,
) -> CompileResult<StackFrameInfo> {
    match target.arch() {
        Arch::X86_64 => {
            generate_x86_64_stack_frame(cfg, function_name, type_pool, strings, interner, target)
        }
        Arch::Aarch64 => {
            generate_aarch64_stack_frame(cfg, function_name, type_pool, strings, interner, target)
        }
    }
}

/// Generate stack frame info for x86-64.
fn generate_x86_64_stack_frame(
    cfg: &Cfg,
    function_name: &str,
    type_pool: &TypeInternPool,
    strings: &[String],
    interner: &ThreadedRodeo,
    target: Target,
) -> CompileResult<StackFrameInfo> {
    use crate::x86_64::{CfgLower, RegAlloc};

    let num_locals = cfg.num_locals();
    let num_params = cfg.num_params();

    // Lower CFG to X86Mir with virtual registers
    let mir = CfgLower::new(cfg, type_pool, strings, interner).lower();

    // Allocate physical registers (may add spill slots)
    let existing_slots = num_locals + num_params;
    let (_mir, num_spills, used_callee_saved) =
        RegAlloc::new(mir, existing_slots).allocate_with_spills()?;

    // Calculate stack layout
    let callee_saved_size = used_callee_saved.len() * 8;
    let total_slots = num_locals + num_spills + num_params.min(6);
    let needed_bytes = total_slots as i32 * 8;
    let current_offset = callee_saved_size as i32;
    let total_needed = current_offset + needed_bytes;
    let stack_size = ((total_needed + 15) / 16) * 16;

    let mut slots = Vec::new();

    // Add callee-saved registers
    for (i, reg) in used_callee_saved.iter().enumerate() {
        let offset = -((i as i32 + 1) * 8);
        slots.push(StackSlot {
            name: Some(format!("saved {}", reg)),
            offset,
            size: 8,
            ty: "i64".to_string(),
            kind: StackSlotKind::CalleeSaved,
        });
    }

    let callee_saved_size_i32 = callee_saved_size as i32;

    // Add local variables
    for i in 0..num_locals {
        let slot_offset = -callee_saved_size_i32 - ((i as i32 + 1) * 8);
        slots.push(StackSlot {
            name: None, // We don't have variable names from CFG yet
            offset: slot_offset,
            size: 8,
            ty: "i64".to_string(), // Generic - we don't track types at CFG level
            kind: StackSlotKind::Local,
        });
    }

    // Add parameter spill slots (for first 6 register params)
    #[allow(unused_variables)]
    let arg_regs = ["rdi", "rsi", "rdx", "rcx", "r8", "r9"];
    for i in 0..num_params.min(6) {
        let slot = num_locals + i;
        let slot_offset = -callee_saved_size_i32 - ((slot as i32 + 1) * 8);
        slots.push(StackSlot {
            name: None, // We don't have param names from CFG yet
            offset: slot_offset,
            size: 8,
            ty: "i64".to_string(),
            kind: StackSlotKind::Parameter,
        });
    }

    // Add spill slots
    for i in 0..num_spills {
        let slot = num_locals + num_params.min(6) + i;
        let slot_offset = -callee_saved_size_i32 - ((slot as i32 + 1) * 8);
        slots.push(StackSlot {
            name: None,
            offset: slot_offset,
            size: 8,
            ty: "i64".to_string(),
            kind: StackSlotKind::Spill,
        });
    }

    // Build argument locations
    let mut arguments = Vec::new();
    for i in 0..num_params as usize {
        let location = if i < 6 {
            ArgPassingLocation::Register(arg_regs[i].to_string())
        } else {
            // Stack arguments are at positive offsets from rbp
            // arg7 at [rbp+16], arg8 at [rbp+24], etc.
            let offset = 16 + ((i - 6) as i32) * 8;
            ArgPassingLocation::Stack { offset }
        };
        arguments.push(ArgumentLocation {
            index: i,
            name: None,
            ty: "i64".to_string(),
            location,
        });
    }

    // Return location
    let return_ty = format!("{:?}", cfg.return_type());
    let return_location = ReturnLocation {
        ty: return_ty,
        registers: vec!["rax".to_string()],
    };

    Ok(StackFrameInfo {
        function_name: function_name.to_string(),
        frame_size: stack_size as usize,
        alignment: 16,
        slots,
        arguments,
        return_location,
        target,
    })
}

/// Generate stack frame info for AArch64.
fn generate_aarch64_stack_frame(
    cfg: &Cfg,
    function_name: &str,
    type_pool: &TypeInternPool,
    strings: &[String],
    interner: &ThreadedRodeo,
    target: Target,
) -> CompileResult<StackFrameInfo> {
    use crate::aarch64::{CfgLower, RegAlloc};

    let num_locals = cfg.num_locals();
    let num_params = cfg.num_params();

    // Lower CFG to Aarch64Mir with virtual registers
    let mir = CfgLower::new(cfg, type_pool, strings, interner).lower();

    // Allocate physical registers (may add spill slots)
    let existing_slots = num_locals + num_params;
    let (_mir, num_spills, used_callee_saved) =
        RegAlloc::new(mir, existing_slots).allocate_with_spills()?;

    // Calculate stack layout for AArch64
    // Callee-saved registers are saved in pairs (16 bytes per pair)
    let num_callee_regs = used_callee_saved.len();
    let callee_saved_pairs = (num_callee_regs + 1) / 2;
    let callee_saved_size = callee_saved_pairs * 16;

    // FP and LR are saved separately (16 bytes)
    let fp_lr_size = 16;

    let total_slots = num_locals + num_spills + num_params.min(8);
    let locals_size = ((total_slots as i32 * 8 + 15) / 16) * 16;

    let frame_size = fp_lr_size + callee_saved_size + locals_size as usize;

    let mut slots = Vec::new();

    // Add callee-saved registers (in pairs, starting after FP/LR save area)
    // Note: FP/LR are at [SP, #-16]! at the very top of the frame
    let mut reg_offset = -(fp_lr_size as i32); // Start after FP/LR
    let mut i = 0;
    while i + 1 < used_callee_saved.len() {
        reg_offset -= 16;
        slots.push(StackSlot {
            name: Some(format!("saved {}", used_callee_saved[i])),
            offset: reg_offset,
            size: 8,
            ty: "i64".to_string(),
            kind: StackSlotKind::CalleeSaved,
        });
        slots.push(StackSlot {
            name: Some(format!("saved {}", used_callee_saved[i + 1])),
            offset: reg_offset + 8,
            size: 8,
            ty: "i64".to_string(),
            kind: StackSlotKind::CalleeSaved,
        });
        i += 2;
    }
    // Handle odd register
    if i < used_callee_saved.len() {
        reg_offset -= 16;
        slots.push(StackSlot {
            name: Some(format!("saved {}", used_callee_saved[i])),
            offset: reg_offset,
            size: 8,
            ty: "i64".to_string(),
            kind: StackSlotKind::CalleeSaved,
        });
    }

    let locals_base_offset = -(fp_lr_size as i32 + callee_saved_size as i32);

    // Add local variables
    for i in 0..num_locals {
        let slot_offset = locals_base_offset - ((i as i32 + 1) * 8);
        slots.push(StackSlot {
            name: None,
            offset: slot_offset,
            size: 8,
            ty: "i64".to_string(),
            kind: StackSlotKind::Local,
        });
    }

    // Add parameter spill slots (for first 8 register params on AArch64)
    for i in 0..num_params.min(8) {
        let slot = num_locals + i;
        let slot_offset = locals_base_offset - ((slot as i32 + 1) * 8);
        slots.push(StackSlot {
            name: None,
            offset: slot_offset,
            size: 8,
            ty: "i64".to_string(),
            kind: StackSlotKind::Parameter,
        });
    }

    // Add spill slots
    for i in 0..num_spills {
        let slot = num_locals + num_params.min(8) + i;
        let slot_offset = locals_base_offset - ((slot as i32 + 1) * 8);
        slots.push(StackSlot {
            name: None,
            offset: slot_offset,
            size: 8,
            ty: "i64".to_string(),
            kind: StackSlotKind::Spill,
        });
    }

    // Build argument locations (x0-x7 for first 8 args on AArch64)
    let arg_regs = ["x0", "x1", "x2", "x3", "x4", "x5", "x6", "x7"];
    let mut arguments = Vec::new();
    for i in 0..num_params as usize {
        let location = if i < 8 {
            ArgPassingLocation::Register(arg_regs[i].to_string())
        } else {
            // Stack arguments on AArch64
            let offset = ((i - 8) as i32) * 8;
            ArgPassingLocation::Stack { offset }
        };
        arguments.push(ArgumentLocation {
            index: i,
            name: None,
            ty: "i64".to_string(),
            location,
        });
    }

    // Return location
    let return_ty = format!("{:?}", cfg.return_type());
    let return_location = ReturnLocation {
        ty: return_ty,
        registers: vec!["x0".to_string()],
    };

    Ok(StackFrameInfo {
        function_name: function_name.to_string(),
        frame_size,
        alignment: 16,
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
    use rue_air::{Air, AirInst, AirInstData, Type, TypeInternPool};
    use rue_cfg::CfgBuilder;
    use rue_span::Span;

    fn create_simple_cfg() -> (rue_cfg::Cfg, TypeInternPool, ThreadedRodeo) {
        let mut air = Air::new(Type::I32);

        let const_ref = air.add_inst(AirInst {
            data: AirInstData::Const(42),
            ty: Type::I32,
            span: Span::new(0, 2),
        });

        air.add_inst(AirInst {
            data: AirInstData::Ret(Some(const_ref)),
            ty: Type::I32,
            span: Span::new(0, 2),
        });

        let interner = ThreadedRodeo::new();
        let type_pool = TypeInternPool::new();
        let cfg_output = CfgBuilder::build(&air, 0, 0, "test", &type_pool, vec![], &interner);
        (cfg_output.cfg, type_pool, interner)
    }

    #[test]
    fn test_generate_stack_frame_info_x86_64() {
        let (cfg, type_pool, interner) = create_simple_cfg();
        let target = Target::X86_64Linux;

        let info =
            generate_stack_frame_info(&cfg, "test", &type_pool, &[], &interner, target).unwrap();

        assert_eq!(info.function_name, "test");
        assert_eq!(info.alignment, 16);
        assert!(!info.return_location.registers.is_empty());
    }

    #[test]
    fn test_stack_frame_display() {
        let info = StackFrameInfo {
            function_name: "main".to_string(),
            frame_size: 32,
            alignment: 16,
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
}
