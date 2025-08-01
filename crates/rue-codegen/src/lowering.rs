use crate::regalloc::RegisterAllocator;
use crate::{BinOp, Instruction, Label, VReg, Value};
use rue_ir::target::{ConditionCode, LabelRef, MachineInstr, Register};
use std::collections::HashMap;

/// Errors that can occur during lowering
#[derive(Debug, Clone, PartialEq)]
pub enum LoweringError {
    /// Register allocation failed
    RegisterAllocation(String),
    /// Too many arguments for function call
    TooManyArguments,
    /// Cannot pop from empty stack
    StackUnderflow,
    /// Unsupported value type in operation
    UnsupportedValueType(&'static str),
    /// No available scratch register
    NoScratchRegister,
}

impl std::fmt::Display for LoweringError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LoweringError::RegisterAllocation(msg) => {
                write!(f, "Register allocation error: {msg}")
            }
            LoweringError::TooManyArguments => write!(f, "Too many arguments for function call"),
            LoweringError::StackUnderflow => {
                write!(f, "Cannot pop from empty stack: push_count would underflow")
            }
            LoweringError::UnsupportedValueType(op) => {
                write!(f, "PhysicalReg not supported in {op}")
            }
            LoweringError::NoScratchRegister => write!(f, "No available scratch register"),
        }
    }
}

impl std::error::Error for LoweringError {}

impl From<String> for LoweringError {
    fn from(s: String) -> Self {
        LoweringError::RegisterAllocation(s)
    }
}

/// Exit code for division by zero
const EXIT_DIV_ZERO: i64 = 250;

/// The Lowering pass converts high-level IR instructions with virtual registers
/// into x86-specific machine instructions with concrete registers.
/// It delegates all spilling decisions to the RegisterAllocator.
pub struct Lowering<'a> {
    allocator: &'a mut RegisterAllocator,
    instructions: Vec<MachineInstr>,
    label_map: HashMap<Label, u32>,
    next_label_id: u32,
    /// External label map to use (if provided)
    external_label_map: Option<&'a HashMap<Label, u32>>,
    /// Track number of pushes since function entry for stack alignment
    push_count: i32,
    /// Track which stack slots are for block parameters (set during Store instructions)
    block_param_offsets: std::collections::HashSet<i64>,
}

impl<'a> Lowering<'a> {
    pub fn new(allocator: &'a mut RegisterAllocator, first_label_id: u32) -> Self {
        Self {
            allocator,
            instructions: Vec::new(),
            label_map: HashMap::new(),
            next_label_id: first_label_id,
            external_label_map: None,
            push_count: 0,
            block_param_offsets: std::collections::HashSet::new(),
            // Block parameters handled via Load/Store with "always spill" approach
        }
    }

    /// Mark a stack offset as being used for block parameters
    pub fn mark_block_param_offset(&mut self, offset: i64) {
        self.block_param_offsets.insert(offset);
    }

    /// Set an external label map to use for label resolution
    pub fn set_label_map(&mut self, label_map: &'a HashMap<Label, u32>) {
        self.external_label_map = Some(label_map);
    }

    // Removed: mark_as_block_param and mark_as_return_param - no longer needed

    /// Get the next label ID that would be assigned
    pub fn next_label_id(&self) -> u32 {
        self.next_label_id
    }

    /// Create a new label and increment the counter
    pub fn new_label(&mut self) -> u32 {
        let id = self.next_label_id;
        self.next_label_id += 1;
        id
    }

    /// Lower a sequence of high-level instructions to machine instructions
    pub fn lower(
        &mut self,
        ir_instructions: &[Instruction],
    ) -> Result<Vec<MachineInstr>, LoweringError> {
        let start_len = self.instructions.len();

        // Process instructions - BlockParamAssign is no longer used with "always spill" approach
        for instr in ir_instructions {
            self.lower_instruction(instr)?;
        }

        Ok(self.instructions[start_len..].to_vec())
    }

    /// Patch stack allocation instructions with actual stack space needed
    pub fn patch_stack_allocation(
        instructions: &mut [MachineInstr],
        allocator: &RegisterAllocator,
    ) {
        let stack_size = allocator.get_stack_size();

        // Find and patch AllocStack instructions
        for instr in instructions.iter_mut() {
            if let MachineInstr::AllocStack { size } = instr {
                *size = stack_size;
            }
        }
    }

    fn lower_instruction(&mut self, instr: &Instruction) -> Result<(), LoweringError> {
        match instr {
            Instruction::Copy { dest, src } => self.lower_copy(*dest, src),
            Instruction::BlockParamAssign { .. } => {
                // With "always spill" approach, block parameters are handled via Load/Store
                // This instruction should not be generated anymore
                Err(LoweringError::RegisterAllocation(
                    "BlockParamAssign should not be generated with always-spill approach"
                        .to_string(),
                ))
            }
            Instruction::BinaryOp { dest, lhs, rhs, op } => {
                self.lower_binary_op(*dest, lhs, rhs, op)
            }
            Instruction::TypedBinaryOp {
                dest,
                lhs,
                rhs,
                op,
                ty,
            } => self.lower_typed_binary_op(*dest, lhs, rhs, op, ty),
            Instruction::Load { dest, offset } => self.lower_load(*dest, *offset),
            Instruction::Store { src, offset } => self.lower_store(*src, *offset),
            Instruction::Push { src } => self.lower_push(src),
            Instruction::Pop { dest } => self.lower_pop(*dest),
            Instruction::Label(_) => {
                // We are about to start a fresh basic block, so flush
                // all pending stores from the previous one.
                self.allocator.flush_stores();
                self.emit_spill_reload_ops();

                // Labels are handled externally in compile_to_executable
                // to ensure consistent numbering across functions
                Ok(())
            }
            Instruction::Jump(target) => self.lower_jump(*target),
            Instruction::Branch {
                condition,
                true_label,
                false_label,
            } => self.lower_branch(*condition, *true_label, *false_label),
            Instruction::Call {
                dest,
                function,
                args,
            } => self.lower_call(dest.as_ref(), function, args),
            Instruction::Return { value } => self.lower_return(value.as_ref()),
            Instruction::Syscall {
                result,
                syscall_num,
                args,
            } => self.lower_syscall(*result, *syscall_num, args),
            Instruction::SaveRegisters { registers } => self.lower_save_registers(registers),
            Instruction::RestoreRegisters { registers } => self.lower_restore_registers(registers),
            Instruction::EnterFrame => self.lower_enter_frame(),
            Instruction::LeaveFrame => self.lower_leave_frame(),
        }
    }

    // Removed: lower_block_param_assign - no longer needed with "always spill" approach

    // Removed: lower_parallel_block_assigns - no longer needed with "always spill" approach

    fn lower_copy(&mut self, dest: VReg, src: &Value) -> Result<(), LoweringError> {
        match src {
            Value::SignedImm(imm) => {
                let dest_reg = self.allocator.ensure_reg(dest, &[])?;
                self.emit_spill_reload_ops();
                if *imm >= i32::MIN as i64 && *imm <= i32::MAX as i64 {
                    self.emit(MachineInstr::MovRI32 {
                        dest: dest_reg,
                        imm: *imm as i32,
                    });
                } else {
                    self.emit(MachineInstr::MovRI64 {
                        dest: dest_reg,
                        imm: *imm,
                    });
                }
                // Mark the destination as dirty after writing to it
                self.allocator.schedule_store(dest, dest_reg);
            }
            Value::VReg(src_vreg) => {
                // Special case: self-copy (used after loading block parameters)
                if *src_vreg == dest {
                    // Self-copy is a semantic no-op: the value is already in a
                    // register (and marked Dirty by the preceding `Load`) and
                    // will be flushed correctly later. Do **nothing**.
                    // IMPORTANT: The VReg retains its dirty state from the Load,
                    // ensuring it will be stored back to memory when needed.
                    return Ok(());
                } else {
                    // Normal copy between different VRegs
                    let src_reg = self.allocator.ensure_reg(*src_vreg, &[])?;
                    self.emit_spill_reload_ops();

                    // With "always spill" approach, all copies are regular copies
                    let dest_reg = self.allocator.ensure_reg(dest, &[src_reg])?;

                    self.emit_spill_reload_ops();
                    if src_reg != dest_reg {
                        self.emit(MachineInstr::MovRR {
                            dest: dest_reg,
                            src: src_reg,
                        });
                    }
                    // Mark the destination as dirty after writing to it
                    self.allocator.schedule_store(dest, dest_reg);
                }
            }
            Value::UnsignedImm(imm) => {
                let dest_reg = self.allocator.ensure_reg(dest, &[])?;
                self.emit_spill_reload_ops();

                // For unsigned immediates, we need to check the range
                if *imm <= i32::MAX as u64 {
                    self.emit(MachineInstr::MovRI32 {
                        dest: dest_reg,
                        imm: *imm as i32,
                    });
                } else {
                    self.emit(MachineInstr::MovRI64 {
                        dest: dest_reg,
                        imm: *imm as i64,
                    });
                }
                self.allocator.schedule_store(dest, dest_reg);
            }
            Value::PhysicalReg(reg) => {
                // Use assign_reg_for_def since we're defining a new value (not reading old one)
                let dest_reg = self.allocator.assign_reg_for_def(dest, &[*reg])?;
                self.emit_spill_reload_ops();
                if *reg != dest_reg {
                    self.emit(MachineInstr::MovRR {
                        dest: dest_reg,
                        src: *reg,
                    });
                }
                // Note: assign_reg_for_def already marks the register as dirty
            }
        }
        Ok(())
    }

    fn lower_binary_op(
        &mut self,
        dest: VReg,
        lhs: &Value,
        rhs: &Value,
        op: &BinOp,
    ) -> Result<(), LoweringError> {
        match op {
            BinOp::Add | BinOp::Sub | BinOp::Mul => {
                // Get lhs into dest register first
                let dest_reg = match lhs {
                    Value::VReg(vreg) => {
                        let lhs_reg = self.allocator.ensure_reg(*vreg, &[])?;
                        let dest_reg = self.allocator.assign_reg_for_def(dest, &[lhs_reg])?;
                        // Emit any pending spill/reload operations before moving
                        self.emit_spill_reload_ops();
                        if lhs_reg != dest_reg {
                            self.emit(MachineInstr::MovRR {
                                dest: dest_reg,
                                src: lhs_reg,
                            });
                        }
                        // Note: assign_reg_for_def already marks the register as dirty
                        dest_reg
                    }
                    Value::SignedImm(imm) => {
                        let dest_reg = self.allocator.assign_reg_for_def(dest, &[])?;
                        // Emit any pending spill/reload operations before moving
                        self.emit_spill_reload_ops();
                        if *imm >= i32::MIN as i64 && *imm <= i32::MAX as i64 {
                            self.emit(MachineInstr::MovRI32 {
                                dest: dest_reg,
                                imm: *imm as i32,
                            });
                        } else {
                            self.emit(MachineInstr::MovRI64 {
                                dest: dest_reg,
                                imm: *imm,
                            });
                        }
                        // Note: assign_reg_for_def already marks the register as dirty
                        dest_reg
                    }
                    Value::UnsignedImm(imm) => {
                        let dest_reg = self.allocator.assign_reg_for_def(dest, &[])?;
                        // Emit any pending spill/reload operations before moving
                        self.emit_spill_reload_ops();

                        if *imm <= i32::MAX as u64 {
                            self.emit(MachineInstr::MovRI32 {
                                dest: dest_reg,
                                imm: *imm as i32,
                            });
                        } else {
                            self.emit(MachineInstr::MovRI64 {
                                dest: dest_reg,
                                imm: *imm as i64,
                            });
                        }
                        // Note: assign_reg_for_def already marks the register as dirty
                        dest_reg
                    }
                    Value::PhysicalReg(_) => {
                        return Err(LoweringError::UnsupportedValueType("binary operations"));
                    }
                };

                // Apply operation with rhs
                match (op, rhs) {
                    (BinOp::Add, Value::VReg(rhs_vreg)) => {
                        // Ensure RHS gets a different register than the destination
                        // This prevents the "add rax, rax" issue when both operands
                        // happen to be in the same register
                        let rhs_reg = self.allocator.ensure_reg(*rhs_vreg, &[dest_reg])?;
                        self.emit_spill_reload_ops();

                        // Emit the addition
                        self.emit(MachineInstr::AddRR {
                            dest: dest_reg,
                            src: rhs_reg,
                        });
                        // Note: operations on already-dirty registers remain dirty
                    }
                    (BinOp::Add, Value::SignedImm(imm)) => {
                        if *imm >= i32::MIN as i64 && *imm <= i32::MAX as i64 {
                            self.emit(MachineInstr::AddRI {
                                dest: dest_reg,
                                imm: *imm as i32,
                            });
                        } else {
                            // Load large immediate to scratch register
                            let scratch = self.get_scratch_register(&[dest_reg])?;
                            self.emit(MachineInstr::MovRI64 {
                                dest: scratch,
                                imm: *imm,
                            });
                            self.emit(MachineInstr::AddRR {
                                dest: dest_reg,
                                src: scratch,
                            });
                        }
                        // Note: operations on already-dirty registers remain dirty
                    }
                    (BinOp::Sub, Value::VReg(rhs_vreg)) => {
                        let rhs_reg = self.allocator.ensure_reg(*rhs_vreg, &[dest_reg])?;
                        self.emit_spill_reload_ops();
                        self.emit(MachineInstr::SubRR {
                            dest: dest_reg,
                            src: rhs_reg,
                        });
                        // Note: operations on already-dirty registers remain dirty
                    }
                    (BinOp::Sub, Value::SignedImm(imm)) => {
                        if *imm >= i32::MIN as i64 && *imm <= i32::MAX as i64 {
                            self.emit(MachineInstr::SubRI {
                                dest: dest_reg,
                                imm: *imm as i32,
                            });
                        } else {
                            let scratch = self.get_scratch_register(&[dest_reg])?;
                            self.emit(MachineInstr::MovRI64 {
                                dest: scratch,
                                imm: *imm,
                            });
                            self.emit(MachineInstr::SubRR {
                                dest: dest_reg,
                                src: scratch,
                            });
                        }
                        // Note: operations on already-dirty registers remain dirty
                    }
                    (BinOp::Mul, Value::VReg(rhs_vreg)) => {
                        let rhs_reg = self.allocator.ensure_reg(*rhs_vreg, &[dest_reg])?;
                        self.emit_spill_reload_ops();
                        self.emit(MachineInstr::ImulRR {
                            dest: dest_reg,
                            src: rhs_reg,
                        });
                        // Note: operations on already-dirty registers remain dirty
                    }
                    (BinOp::Mul, Value::SignedImm(imm)) => {
                        if *imm >= i32::MIN as i64 && *imm <= i32::MAX as i64 {
                            self.emit(MachineInstr::ImulRI {
                                dest: dest_reg,
                                imm: *imm as i32,
                            });
                        } else {
                            let scratch = self.get_scratch_register(&[dest_reg])?;
                            self.emit(MachineInstr::MovRI64 {
                                dest: scratch,
                                imm: *imm,
                            });
                            self.emit(MachineInstr::ImulRR {
                                dest: dest_reg,
                                src: scratch,
                            });
                        }
                        // Note: operations on already-dirty registers remain dirty
                    }
                    (BinOp::Add, Value::UnsignedImm(imm)) => {
                        if *imm <= i32::MAX as u64 {
                            self.emit(MachineInstr::AddRI {
                                dest: dest_reg,
                                imm: *imm as i32,
                            });
                        } else {
                            // Load large immediate to scratch register
                            let scratch = self.get_scratch_register(&[dest_reg])?;
                            self.emit(MachineInstr::MovRI64 {
                                dest: scratch,
                                imm: *imm as i64,
                            });
                            self.emit(MachineInstr::AddRR {
                                dest: dest_reg,
                                src: scratch,
                            });
                        }
                        // Note: operations on already-dirty registers remain dirty
                    }
                    (BinOp::Sub, Value::UnsignedImm(imm)) => {
                        if *imm <= i32::MAX as u64 {
                            self.emit(MachineInstr::SubRI {
                                dest: dest_reg,
                                imm: *imm as i32,
                            });
                        } else {
                            let scratch = self.get_scratch_register(&[dest_reg])?;
                            self.emit(MachineInstr::MovRI64 {
                                dest: scratch,
                                imm: *imm as i64,
                            });
                            self.emit(MachineInstr::SubRR {
                                dest: dest_reg,
                                src: scratch,
                            });
                        }
                        // Note: operations on already-dirty registers remain dirty
                    }
                    (BinOp::Mul, Value::UnsignedImm(imm)) => {
                        if *imm <= i32::MAX as u64 {
                            self.emit(MachineInstr::ImulRI {
                                dest: dest_reg,
                                imm: *imm as i32,
                            });
                        } else {
                            let scratch = self.get_scratch_register(&[dest_reg])?;
                            self.emit(MachineInstr::MovRI64 {
                                dest: scratch,
                                imm: *imm as i64,
                            });
                            self.emit(MachineInstr::ImulRR {
                                dest: dest_reg,
                                src: scratch,
                            });
                        }
                        // Note: operations on already-dirty registers remain dirty
                    }
                    _ => unreachable!(),
                }
                Ok(())
            }
            BinOp::Div => self.lower_division(dest, lhs, rhs),
            BinOp::Mod => self.lower_modulo(dest, lhs, rhs),
            BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge | BinOp::Eq | BinOp::Ne => {
                self.lower_comparison(dest, lhs, rhs, op)
            }
        }
    }

    /// Lower a typed binary operation, handling i32 overflow properly
    fn lower_typed_binary_op(
        &mut self,
        dest: VReg,
        lhs: &Value,
        rhs: &Value,
        op: &BinOp,
        ty: &rue_ir::types::RueType,
    ) -> Result<(), LoweringError> {
        use rue_ir::types::RueType;

        if std::env::var("RUE_DEBUG").is_ok() {
            eprintln!("lower_typed_binary_op: dest={dest:?}, op={op:?}, ty={ty:?}");
        }

        // For non-arithmetic operations, delegate to regular binary operation
        if matches!(
            op,
            BinOp::Lt
                | BinOp::Le
                | BinOp::Gt
                | BinOp::Ge
                | BinOp::Eq
                | BinOp::Ne
                | BinOp::Div
                | BinOp::Mod
        ) {
            return self.lower_binary_op(dest, lhs, rhs, op);
        }

        // First, perform the operation using regular binary operation logic
        self.lower_binary_op(dest, lhs, rhs, op)?;

        // For i32 operations, add truncation to handle overflow correctly
        if matches!(ty, RueType::I32) {
            let dest_reg = self.allocator.ensure_reg(dest, &[])?;
            self.emit_spill_reload_ops();

            if std::env::var("RUE_DEBUG").is_ok() {
                eprintln!("Adding i32 truncation: dest_reg={dest_reg:?}");
            }

            // Truncate to 32-bit and sign-extend back to 64-bit
            // This ensures i32 overflow wraps correctly (movsxd automatically truncates and sign-extends)
            self.emit(MachineInstr::Movsxd {
                dest: dest_reg,
                src: dest_reg,
            });
        }

        Ok(())
    }

    fn lower_modulo(&mut self, dest: VReg, lhs: &Value, rhs: &Value) -> Result<(), LoweringError> {
        // Modulo is like division but we want the remainder from RDX
        let rax = Register::Rax;
        let rdx = Register::Rdx;

        // Move lhs to RAX
        match lhs {
            Value::VReg(vreg) => {
                let lhs_reg = self.allocator.ensure_reg(*vreg, &[])?;
                self.emit_spill_reload_ops();
                if lhs_reg != rax {
                    self.emit(MachineInstr::MovRR {
                        dest: rax,
                        src: lhs_reg,
                    });
                }
            }
            Value::SignedImm(imm) => {
                if *imm >= i32::MIN as i64 && *imm <= i32::MAX as i64 {
                    self.emit(MachineInstr::MovRI32 {
                        dest: rax,
                        imm: *imm as i32,
                    });
                } else {
                    self.emit(MachineInstr::MovRI64 {
                        dest: rax,
                        imm: *imm,
                    });
                }
            }
            Value::UnsignedImm(imm) => {
                if *imm <= i32::MAX as u64 {
                    self.emit(MachineInstr::MovRI32 {
                        dest: rax,
                        imm: *imm as i32,
                    });
                } else {
                    self.emit(MachineInstr::MovRI64 {
                        dest: rax,
                        imm: *imm as i64,
                    });
                }
            }
            Value::PhysicalReg(_) => return Err(LoweringError::UnsupportedValueType("modulo")),
        }

        // Sign extend RAX to RDX:RAX
        self.emit(MachineInstr::Cqo);

        // Get divisor in a register
        let divisor_reg = match rhs {
            Value::VReg(vreg) => {
                let reg = self.allocator.ensure_reg(*vreg, &[rax, rdx])?;
                self.emit_spill_reload_ops();
                reg
            }
            Value::SignedImm(imm) => {
                let scratch = self.get_scratch_register(&[rax, rdx])?;
                if *imm >= i32::MIN as i64 && *imm <= i32::MAX as i64 {
                    self.emit(MachineInstr::MovRI32 {
                        dest: scratch,
                        imm: *imm as i32,
                    });
                } else {
                    self.emit(MachineInstr::MovRI64 {
                        dest: scratch,
                        imm: *imm,
                    });
                }
                scratch
            }
            Value::UnsignedImm(imm) => {
                let scratch = self.get_scratch_register(&[rax, rdx])?;
                if *imm <= i32::MAX as u64 {
                    self.emit(MachineInstr::MovRI32 {
                        dest: scratch,
                        imm: *imm as i32,
                    });
                } else {
                    self.emit(MachineInstr::MovRI64 {
                        dest: scratch,
                        imm: *imm as i64,
                    });
                }
                scratch
            }
            Value::PhysicalReg(_) => {
                return Err(LoweringError::UnsupportedValueType("division"));
            }
        };

        // Check for division by zero (modulo by zero)
        self.emit(MachineInstr::CmpRI {
            reg: divisor_reg,
            imm: 0,
        });

        // Jump if not zero
        let mod_ok_label = self.new_label();
        self.emit(MachineInstr::JmpCC {
            cc: ConditionCode::NotEqual,
            target: LabelRef::Local(mod_ok_label),
        });

        // Modulo by zero - exit with code EXIT_DIV_ZERO
        self.emit(MachineInstr::MovRI64 {
            dest: Register::Rdi,
            imm: EXIT_DIV_ZERO,
        });
        self.emit(MachineInstr::MovRI64 {
            dest: Register::Rax,
            imm: 60, // sys_exit
        });
        self.emit(MachineInstr::Syscall);
        // Mark unreachable - sys_exit never returns
        self.emit(MachineInstr::Ud2);

        // Continue with division
        self.emit(MachineInstr::Label { id: mod_ok_label });

        // Perform division
        self.emit(MachineInstr::Idiv {
            divisor: divisor_reg,
        });

        // CRITICAL: Mark RDX as dirty since idiv writes remainder to it
        // This prevents the allocator from thinking RDX is clean
        self.allocator.invalidate_register(rdx); // Force spill of any value in RDX

        // Move remainder from RDX to dest (this is the key difference from division)
        let dest_reg = self.allocator.ensure_reg(dest, &[])?;
        self.emit_spill_reload_ops();
        if dest_reg != rdx {
            self.emit(MachineInstr::MovRR {
                dest: dest_reg,
                src: rdx,
            });
        }
        // Mark the destination as dirty after writing to it
        self.allocator.schedule_store(dest, dest_reg);

        Ok(())
    }

    fn lower_division(
        &mut self,
        dest: VReg,
        lhs: &Value,
        rhs: &Value,
    ) -> Result<(), LoweringError> {
        // Division requires dividend in RAX
        let rax = Register::Rax;
        let rdx = Register::Rdx;

        // Move lhs to RAX
        match lhs {
            Value::VReg(vreg) => {
                let lhs_reg = self.allocator.ensure_reg(*vreg, &[])?;
                self.emit_spill_reload_ops();
                if lhs_reg != rax {
                    self.emit(MachineInstr::MovRR {
                        dest: rax,
                        src: lhs_reg,
                    });
                }
            }
            Value::SignedImm(imm) => {
                if *imm >= i32::MIN as i64 && *imm <= i32::MAX as i64 {
                    self.emit(MachineInstr::MovRI32 {
                        dest: rax,
                        imm: *imm as i32,
                    });
                } else {
                    self.emit(MachineInstr::MovRI64 {
                        dest: rax,
                        imm: *imm,
                    });
                }
            }
            Value::UnsignedImm(imm) => {
                if *imm <= i32::MAX as u64 {
                    self.emit(MachineInstr::MovRI32 {
                        dest: rax,
                        imm: *imm as i32,
                    });
                } else {
                    self.emit(MachineInstr::MovRI64 {
                        dest: rax,
                        imm: *imm as i64,
                    });
                }
            }
            Value::PhysicalReg(_) => {
                return Err(LoweringError::UnsupportedValueType("division"));
            }
        }

        // Sign extend RAX to RDX:RAX
        self.emit(MachineInstr::Cqo);

        // Get divisor in a register
        let divisor_reg = match rhs {
            Value::VReg(vreg) => {
                let reg = self.allocator.ensure_reg(*vreg, &[rax, rdx])?;
                self.emit_spill_reload_ops();
                reg
            }
            Value::SignedImm(imm) => {
                let scratch = self.get_scratch_register(&[rax, rdx])?;
                if *imm >= i32::MIN as i64 && *imm <= i32::MAX as i64 {
                    self.emit(MachineInstr::MovRI32 {
                        dest: scratch,
                        imm: *imm as i32,
                    });
                } else {
                    self.emit(MachineInstr::MovRI64 {
                        dest: scratch,
                        imm: *imm,
                    });
                }
                scratch
            }
            Value::UnsignedImm(imm) => {
                let scratch = self.get_scratch_register(&[rax, rdx])?;
                if *imm <= i32::MAX as u64 {
                    self.emit(MachineInstr::MovRI32 {
                        dest: scratch,
                        imm: *imm as i32,
                    });
                } else {
                    self.emit(MachineInstr::MovRI64 {
                        dest: scratch,
                        imm: *imm as i64,
                    });
                }
                scratch
            }
            Value::PhysicalReg(_) => {
                return Err(LoweringError::UnsupportedValueType("division"));
            }
        };

        // Check for division by zero
        self.emit(MachineInstr::CmpRI {
            reg: divisor_reg,
            imm: 0,
        });

        // Jump if not zero
        let div_ok_label = self.new_label();
        self.emit(MachineInstr::JmpCC {
            cc: ConditionCode::NotEqual,
            target: LabelRef::Local(div_ok_label),
        });

        // Division by zero - exit with code EXIT_DIV_ZERO
        self.emit(MachineInstr::MovRI64 {
            dest: Register::Rdi,
            imm: EXIT_DIV_ZERO,
        });
        self.emit(MachineInstr::MovRI64 {
            dest: Register::Rax,
            imm: 60, // sys_exit
        });
        self.emit(MachineInstr::Syscall);
        // Mark unreachable - sys_exit never returns
        self.emit(MachineInstr::Ud2);

        // Continue with division
        self.emit(MachineInstr::Label { id: div_ok_label });

        // Perform division
        self.emit(MachineInstr::Idiv {
            divisor: divisor_reg,
        });

        // CRITICAL: Mark RDX as dirty since idiv writes remainder to it
        // This prevents the allocator from thinking RDX is clean
        self.allocator.invalidate_register(rdx); // Force spill of any value in RDX

        // Move result from RAX to dest
        let dest_reg = self.allocator.ensure_reg(dest, &[])?;
        self.emit_spill_reload_ops();
        if dest_reg != rax {
            self.emit(MachineInstr::MovRR {
                dest: dest_reg,
                src: rax,
            });
        }
        // Mark the destination as dirty after writing to it
        self.allocator.schedule_store(dest, dest_reg);

        Ok(())
    }

    fn lower_comparison(
        &mut self,
        dest: VReg,
        lhs: &Value,
        rhs: &Value,
        op: &BinOp,
    ) -> Result<(), LoweringError> {
        // Get lhs into a register
        let lhs_reg = match lhs {
            Value::VReg(vreg) => {
                let reg = self.allocator.ensure_reg(*vreg, &[])?;
                self.emit_spill_reload_ops();
                reg
            }
            Value::SignedImm(imm) => {
                let scratch = self.get_scratch_register(&[])?;
                if *imm >= i32::MIN as i64 && *imm <= i32::MAX as i64 {
                    self.emit(MachineInstr::MovRI32 {
                        dest: scratch,
                        imm: *imm as i32,
                    });
                } else {
                    self.emit(MachineInstr::MovRI64 {
                        dest: scratch,
                        imm: *imm,
                    });
                }
                scratch
            }
            Value::UnsignedImm(imm) => {
                let scratch = self.get_scratch_register(&[])?;
                if *imm <= i32::MAX as u64 {
                    self.emit(MachineInstr::MovRI32 {
                        dest: scratch,
                        imm: *imm as i32,
                    });
                } else {
                    self.emit(MachineInstr::MovRI64 {
                        dest: scratch,
                        imm: *imm as i64,
                    });
                }
                scratch
            }
            Value::PhysicalReg(_) => {
                return Err(LoweringError::UnsupportedValueType("comparison"));
            }
        };

        // Compare with rhs
        match rhs {
            Value::VReg(rhs_vreg) => {
                let rhs_reg = self.allocator.ensure_reg(*rhs_vreg, &[lhs_reg])?;
                self.emit_spill_reload_ops();
                self.emit(MachineInstr::CmpRR {
                    left: lhs_reg,
                    right: rhs_reg,
                });
            }
            Value::SignedImm(imm) => {
                if *imm >= i32::MIN as i64 && *imm <= i32::MAX as i64 {
                    self.emit(MachineInstr::CmpRI {
                        reg: lhs_reg,
                        imm: *imm as i32,
                    });
                } else {
                    let scratch = self.get_scratch_register(&[lhs_reg])?;
                    self.emit(MachineInstr::MovRI64 {
                        dest: scratch,
                        imm: *imm,
                    });
                    self.emit(MachineInstr::CmpRR {
                        left: lhs_reg,
                        right: scratch,
                    });
                }
            }
            Value::UnsignedImm(imm) => {
                if *imm <= i32::MAX as u64 {
                    self.emit(MachineInstr::CmpRI {
                        reg: lhs_reg,
                        imm: *imm as i32,
                    });
                } else {
                    let scratch = self.get_scratch_register(&[lhs_reg])?;
                    self.emit(MachineInstr::MovRI64 {
                        dest: scratch,
                        imm: *imm as i64,
                    });
                    self.emit(MachineInstr::CmpRR {
                        left: lhs_reg,
                        right: scratch,
                    });
                }
            }
            Value::PhysicalReg(_) => {
                return Err(LoweringError::UnsupportedValueType("comparison"));
            }
        }

        // Set condition code
        let cc = match op {
            BinOp::Lt => ConditionCode::Less,
            BinOp::Le => ConditionCode::LessEqual,
            BinOp::Gt => ConditionCode::Greater,
            BinOp::Ge => ConditionCode::GreaterEqual,
            BinOp::Eq => ConditionCode::Equal,
            BinOp::Ne => ConditionCode::NotEqual,
            _ => unreachable!(),
        };

        // We are *defining* dest, so pick a fresh register that will not clobber
        // any other live value. Use 8-bit constraint for SetCC operations.
        let dest_reg = self.allocator.assign_reg_for_8bit_def(dest, &[lhs_reg])?;
        self.emit_spill_reload_ops();
        self.emit(MachineInstr::SetCC { dest: dest_reg, cc });
        self.emit(MachineInstr::Movzx {
            dest: dest_reg,
            src: dest_reg,
        });
        // Mark the destination as dirty after writing to it
        self.allocator.schedule_store(dest, dest_reg);

        Ok(())
    }

    fn lower_load(&mut self, dest: VReg, offset: i64) -> Result<(), LoweringError> {
        // Use assign_reg_for_def since we're defining a new value in dest
        let dest_reg = self.allocator.assign_reg_for_def(dest, &[])?;
        debug_assert!(
            dest_reg != Register::Rsp && dest_reg != Register::Rbp,
            "Cannot use RSP/RBP as destination register"
        );
        self.emit_spill_reload_ops();
        self.emit(MachineInstr::MovRM {
            // load from stack slot
            dest: dest_reg,
            base: Register::Rbp, // <- use the frame-pointer
            offset: offset as i32,
        });
        // Mark the destination as dirty after loading from memory
        self.allocator.schedule_store(dest, dest_reg);
        Ok(())
    }

    fn lower_store(&mut self, src: VReg, offset: i64) -> Result<(), LoweringError> {
        if std::env::var("RUE_DEBUG").is_ok() {
            eprintln!("lower_store: storing vreg {src:?} to offset {offset}");
        }

        // Check if this is a block parameter store
        let is_block_param = self.block_param_offsets.contains(&offset);

        if is_block_param {
            // For block parameter stores, clear all register state to force reload from memory
            // This ensures correctness when storing arguments before a jump to a block
            self.allocator.clear_all_registers();
            self.emit_spill_reload_ops();
        }

        let src_reg = self.allocator.ensure_reg(src, &[])?;
        self.emit_spill_reload_ops();
        if std::env::var("RUE_DEBUG").is_ok() {
            eprintln!("lower_store: vreg {src:?} is in register {src_reg:?}");
        }
        self.emit(MachineInstr::MovMR {
            // store to stack slot
            base: Register::Rbp, // <- use the frame-pointer
            offset: offset as i32,
            src: src_reg,
        });
        Ok(())
    }

    fn lower_push(&mut self, src: &Value) -> Result<(), LoweringError> {
        match src {
            Value::VReg(vreg) => {
                let src_reg = self.allocator.ensure_reg(*vreg, &[])?;
                self.emit_spill_reload_ops();
                self.emit(MachineInstr::Push { reg: src_reg });
            }
            Value::PhysicalReg(reg) => {
                // Physical register can be pushed directly
                self.emit(MachineInstr::Push { reg: *reg });
            }
            Value::SignedImm(_) | Value::UnsignedImm(_) => {
                return Err(LoweringError::RegisterAllocation(
                    "Cannot push immediate values directly".to_string(),
                ));
            }
        }
        self.push_count += 1;
        Ok(())
    }

    fn lower_pop(&mut self, dest: VReg) -> Result<(), LoweringError> {
        if self.push_count <= 0 {
            return Err(LoweringError::StackUnderflow);
        }

        let dest_reg = self.allocator.ensure_reg(dest, &[])?;
        debug_assert!(
            dest_reg != Register::Rsp && dest_reg != Register::Rbp,
            "Cannot use RSP/RBP as destination register"
        );
        self.emit_spill_reload_ops();
        self.emit(MachineInstr::Pop { reg: dest_reg });
        self.push_count -= 1;
        // Mark the destination as dirty after popping into it
        self.allocator.schedule_store(dest, dest_reg);
        Ok(())
    }

    fn lower_jump(&mut self, target: Label) -> Result<(), LoweringError> {
        // Write back all dirty VRegs before we leave this block.
        self.flush_for_cf();

        let machine_label_id = self.get_or_create_label(target);
        self.emit(MachineInstr::Jmp {
            target: LabelRef::Local(machine_label_id),
        });
        Ok(())
    }

    fn lower_branch(
        &mut self,
        condition: VReg,
        true_label: Label,
        false_label: Label,
    ) -> Result<(), LoweringError> {
        let cond_reg = self.allocator.ensure_reg(condition, &[])?;
        self.emit_spill_reload_ops();

        // Test condition
        self.emit(MachineInstr::CmpRI {
            reg: cond_reg,
            imm: 0,
        });

        // After CMP the flags are set and `cond_reg` is no longer needed,
        // so it is now safe to spill every dirty register.
        self.flush_for_cf();

        // Jump to true label if not zero
        let true_id = self.get_or_create_label(true_label);
        self.emit(MachineInstr::JmpCC {
            cc: ConditionCode::NotEqual,
            target: LabelRef::Local(true_id),
        });

        // Fall through to false label
        let false_id = self.get_or_create_label(false_label);
        self.emit(MachineInstr::Jmp {
            target: LabelRef::Local(false_id),
        });

        Ok(())
    }

    fn lower_call(
        &mut self,
        dest: Option<&VReg>,
        function: &str,
        args: &[VReg],
    ) -> Result<(), LoweringError> {
        // Map built-in functions to runtime names
        let runtime_function = match function {
            "exit" => "__rue_exit",
            "println_i32" => "__rue_println_i32",
            "println_i64" => "__rue_println_i64",
            "println_bool" => "__rue_println_bool",
            "println_unit" => "__rue_println_unit",
            "input" => "__rue_input",
            "to_i32" => "__rue_to_i32",
            "to_i64" => "__rue_to_i64",
            _ => function, // User-defined functions keep their names
        };

        // System V ABI: arguments in RDI, RSI, RDX, RCX, R8, R9
        let arg_regs = [
            Register::Rdi,
            Register::Rsi,
            Register::Rdx,
            Register::Rcx,
            Register::R8,
            Register::R9,
        ];

        // Define caller-saved registers
        let caller_saved_regs = [
            Register::Rax,
            Register::Rcx,
            Register::Rdx,
            Register::Rsi,
            Register::Rdi,
            Register::R8,
            Register::R9,
            Register::R10,
            Register::R11,
        ];

        // For user-defined functions, we use a simpler and more reliable approach:
        // Force all VRegs to be spilled to memory, avoiding the complex push/pop dance
        // that can lose track of VReg-to-register mappings
        let is_user_function = !runtime_function.starts_with("__rue_");

        if is_user_function {
            // CRITICAL: Force ALL VRegs in caller-saved registers to be spilled to memory
            // This is the safest approach for recursive function calls
            for &reg in &caller_saved_regs {
                if self.allocator.is_register_allocated(reg) {
                    self.allocator.invalidate_register(reg);
                }
            }

            // Emit any spill operations generated by the invalidation
            self.emit_spill_reload_ops();
        }

        // For runtime functions, we still need to save/restore registers the traditional way
        let mut regs_to_save = Vec::new();
        if !is_user_function {
            for &reg in &caller_saved_regs {
                // Save registers that contain values for runtime function calls
                if self.allocator.is_register_allocated(reg)
                    || self.allocator.is_scratch_register(reg)
                {
                    regs_to_save.push(reg);
                }
            }
        }

        // Move arguments to their designated registers
        // For user functions, argument registers should be free after spilling
        // For runtime functions, we need to save any argument registers that contain live values
        if !is_user_function {
            for (i, &_arg_vreg) in args.iter().enumerate() {
                if i >= arg_regs.len() {
                    return Err(LoweringError::TooManyArguments);
                }

                // Check if the target argument register contains a live value
                if self.allocator.is_register_allocated(arg_regs[i])
                    && !regs_to_save.contains(&arg_regs[i])
                {
                    // This argument register contains a live value that we haven't saved yet
                    // We need to save it before overwriting
                    regs_to_save.push(arg_regs[i]);
                }
            }
        }

        // Save caller-saved registers (only for runtime functions)
        if !is_user_function {
            for &reg in &regs_to_save {
                self.emit(MachineInstr::Push { reg });
                self.push_count += 1;
            }
        }

        // Check if we need alignment padding before the call
        // The stack must be 16-byte aligned before ANY call instruction (SysV ABI requirement)
        let needs_padding = {
            // We need to account for:
            // - Return address pushed by original call (1 push)
            // - RBP pushed by EnterFrame (tracked in push_count)
            // - Any other pushes we've done (also in push_count)
            // Total pushes = 1 (return addr) + push_count
            // If total is odd, we need padding
            let total_pushes = 1 + self.push_count;
            let needs_padding = total_pushes % 2 == 1;

            // Add debug assertion for stack alignment
            debug_assert!(
                !is_user_function || self.push_count == 1,
                "User function calls should only have RBP pushed, but push_count = {}",
                self.push_count
            );

            if needs_padding {
                // sub rsp, 8 to maintain alignment
                // Use SubRI directly to avoid AllocStack's 16-byte alignment
                self.emit(MachineInstr::SubRI {
                    dest: Register::Rsp,
                    imm: 8,
                });
            }
            needs_padding
        };

        // Collect where arguments currently are
        let mut arg_locations = Vec::new();
        for &arg_vreg in args.iter() {
            if let Some(reg) = self.allocator.get_register(arg_vreg) {
                arg_locations.push(reg);
            } else {
                // Not in a register yet, allocate one
                let reg = self.allocator.ensure_reg(arg_vreg, &[])?;
                self.emit_spill_reload_ops();
                arg_locations.push(reg);
            }
        }

        // Now we need to move arguments to ABI positions
        // Handle potential cycles by using a temporary register if needed
        // IMPORTANT: Include all arg_locations in the forbidden list to prevent
        // the scratch register from conflicting with any argument registers
        let mut forbidden = arg_regs[..args.len()].to_vec();
        forbidden.extend(&arg_locations);
        let temp_reg = self.get_scratch_register(&forbidden)?;

        // First pass: direct moves where target is free
        let mut moved = vec![false; args.len()];
        for i in 0..args.len() {
            if moved[i] || arg_locations[i] == arg_regs[i] {
                continue;
            }

            // Check if target is free (not used by another argument)
            let target_free = !arg_locations
                .iter()
                .enumerate()
                .any(|(j, &reg)| j != i && !moved[j] && reg == arg_regs[i]);

            if target_free {
                if arg_locations[i] != arg_regs[i] {
                    self.emit(MachineInstr::MovRR {
                        dest: arg_regs[i],
                        src: arg_locations[i],
                    });
                }
                moved[i] = true;
            }
        }

        // Second pass: handle cycles using temporary register
        for i in 0..args.len() {
            if moved[i] || arg_locations[i] == arg_regs[i] {
                continue;
            }

            // Save current value to temp
            self.emit(MachineInstr::MovRR {
                dest: temp_reg,
                src: arg_locations[i],
            });

            // Find what's in our target position
            let blocking_idx = arg_locations
                .iter()
                .position(|&reg| reg == arg_regs[i])
                .expect("Target must be occupied");

            // Move the blocking value to its destination
            if arg_regs[blocking_idx] != arg_locations[blocking_idx] {
                self.emit(MachineInstr::MovRR {
                    dest: arg_regs[blocking_idx],
                    src: arg_locations[blocking_idx],
                });
            }
            moved[blocking_idx] = true;

            // Now move our value from temp to its destination
            self.emit(MachineInstr::MovRR {
                dest: arg_regs[i],
                src: temp_reg,
            });
            moved[i] = true;
        }

        // Call the function
        self.emit(MachineInstr::Call {
            target: runtime_function.to_string(),
        });

        // Handle return value preservation (only for runtime functions)
        let rax_temp_reg =
            if !is_user_function && dest.is_some() && regs_to_save.contains(&Register::Rax) {
                // RAX will be overwritten when we restore, so save to temporary register first
                // IMPORTANT: Include all regs_to_save and the argument registers in forbidden list
                // to ensure the scratch register doesn't conflict with any register we're about to restore
                let mut forbidden = regs_to_save.clone();
                forbidden.extend(&arg_regs[..args.len()]);
                let temp_reg = self.get_scratch_register(&forbidden)?;
                self.emit(MachineInstr::MovRR {
                    dest: temp_reg,
                    src: Register::Rax,
                });
                Some(temp_reg)
            } else {
                None
            };

        // Restore alignment padding if we added it (only for runtime functions)
        if !is_user_function && needs_padding {
            self.emit(MachineInstr::AddRI {
                dest: Register::Rsp,
                imm: 8,
            });
        }

        // Restore caller-saved registers in reverse order (only for runtime functions)
        if !is_user_function {
            for &reg in regs_to_save.iter().rev() {
                if self.push_count <= 0 {
                    return Err(LoweringError::StackUnderflow);
                }
                self.emit(MachineInstr::Pop { reg });
                self.push_count -= 1;
            }
        }

        // Now move the return value to its destination
        if let Some(dest_vreg) = dest {
            // Use assign_reg_for_def since we're defining a new value (the return value)
            let dest_reg = self.allocator.assign_reg_for_def(*dest_vreg, &[])?;
            debug_assert!(
                dest_reg != Register::Rsp && dest_reg != Register::Rbp,
                "Cannot use RSP/RBP as destination register"
            );
            self.emit_spill_reload_ops();

            // Determine where the return value is
            let source_reg = if let Some(temp_reg) = rax_temp_reg {
                temp_reg // We saved it to this temp register (runtime functions only)
            } else {
                Register::Rax // Still in RAX
            };

            if dest_reg != source_reg {
                self.emit(MachineInstr::MovRR {
                    dest: dest_reg,
                    src: source_reg,
                });
            }
            // Note: assign_reg_for_def already marks the register as dirty, so no need to schedule_store
        }

        Ok(())
    }

    fn lower_return(&mut self, value: Option<&VReg>) -> Result<(), LoweringError> {
        // CRITICAL: Flush all dirty registers FIRST, then load return value
        // This prevents the return value from being corrupted by flush operations
        self.allocator.flush_stores();
        self.emit_spill_reload_ops();

        if let Some(vreg) = value {
            if std::env::var("RUE_DEBUG").is_ok() {
                eprintln!("lower_return: returning vreg {vreg:?}");
            }
            // Now load the return value after all other values have been flushed
            // This ensures the return value register won't be used as a scratch register
            let value_reg = self.allocator.ensure_reg(*vreg, &[])?;
            self.emit_spill_reload_ops();
            if std::env::var("RUE_DEBUG").is_ok() {
                eprintln!("lower_return: vreg {vreg:?} in register {value_reg:?}");
            }
            if value_reg != Register::Rax {
                self.emit(MachineInstr::MovRR {
                    dest: Register::Rax,
                    src: value_reg,
                });
            }
        } else {
            // No explicit return value - return 0 (unit type)
            self.emit(MachineInstr::MovRI64 {
                dest: Register::Rax,
                imm: 0,
            });
        }

        // Standard SysV epilogue
        self.emit(MachineInstr::LeaveFrame); // mov rsp, rbp ; pop rbp
        self.emit(MachineInstr::Ret);

        Ok(())
    }

    fn lower_syscall(
        &mut self,
        result: VReg,
        syscall_num: VReg,
        args: &[VReg],
    ) -> Result<(), LoweringError> {
        // System V ABI for syscalls: syscall number in RAX, args in RDI, RSI, RDX, R10, R8, R9
        let syscall_arg_regs = [
            Register::Rdi,
            Register::Rsi,
            Register::Rdx,
            Register::R10,
            Register::R8,
            Register::R9,
        ];

        // Move syscall number to RAX
        let num_reg = self.allocator.ensure_reg(syscall_num, &[])?;
        self.emit_spill_reload_ops();
        if num_reg != Register::Rax {
            self.emit(MachineInstr::MovRR {
                dest: Register::Rax,
                src: num_reg,
            });
        }

        // Move arguments
        for (i, &arg_vreg) in args.iter().enumerate() {
            if i >= syscall_arg_regs.len() {
                return Err(LoweringError::TooManyArguments);
            }
            let arg_reg = self.allocator.ensure_reg(arg_vreg, &[])?;
            self.emit_spill_reload_ops();
            if arg_reg != syscall_arg_regs[i] {
                self.emit(MachineInstr::MovRR {
                    dest: syscall_arg_regs[i],
                    src: arg_reg,
                });
            }
        }

        self.emit(MachineInstr::Syscall);

        // Move result from RAX
        let result_reg = self.allocator.ensure_reg(result, &[])?;
        debug_assert!(
            result_reg != Register::Rsp && result_reg != Register::Rbp,
            "Cannot use RSP/RBP as destination register"
        );
        self.emit_spill_reload_ops();
        if result_reg != Register::Rax {
            self.emit(MachineInstr::MovRR {
                dest: result_reg,
                src: Register::Rax,
            });
        }
        // Mark the result as dirty after syscall
        self.allocator.schedule_store(result, result_reg);

        Ok(())
    }

    fn lower_save_registers(&mut self, registers: &[Register]) -> Result<(), LoweringError> {
        for &reg in registers {
            self.emit(MachineInstr::Push { reg });
        }
        Ok(())
    }

    fn lower_restore_registers(&mut self, registers: &[Register]) -> Result<(), LoweringError> {
        for &reg in registers.iter().rev() {
            self.emit(MachineInstr::Pop { reg });
        }
        Ok(())
    }

    fn lower_enter_frame(&mut self) -> Result<(), LoweringError> {
        // Standard x86-64 function prologue
        self.emit(MachineInstr::EnterFrame);

        // EnterFrame pushes rbp, so we start with 1 push
        // (plus the return address pushed by call = 2 total, which is aligned)
        self.push_count = 1;

        // We emit a placeholder AllocStack that will be patched later
        // with the actual required stack space after we know how many spills we need.
        // We use 0 as a placeholder value that will be replaced.
        self.emit(MachineInstr::AllocStack { size: 0 });

        Ok(())
    }

    fn lower_leave_frame(&mut self) -> Result<(), LoweringError> {
        // Check that push_count is balanced (should only have RBP pushed)
        debug_assert_eq!(
            self.push_count, 1,
            "Unbalanced pushes at function exit: push_count = {} (expected 1 for RBP)",
            self.push_count
        );

        // Standard x86-64 function epilogue
        self.emit(MachineInstr::LeaveFrame);
        Ok(())
    }

    fn get_or_create_label(&mut self, label_id: Label) -> u32 {
        // First check external label map if provided
        if let Some(external_map) = self.external_label_map {
            if let Some(&machine_id) = external_map.get(&label_id) {
                return machine_id;
            }
        }

        // Check if we already have this label
        if let Some(&existing_id) = self.label_map.get(&label_id) {
            return existing_id;
        }

        // Create a new label
        let new_id = self.new_label();
        self.label_map.insert(label_id, new_id);
        new_id
    }

    /// Get the internal label map (for external synchronization)
    pub fn get_label_map(&self) -> &HashMap<Label, u32> {
        &self.label_map
    }

    /// Get the current next label ID (for external synchronization)
    pub fn get_next_label_id(&self) -> u32 {
        self.next_label_id
    }

    fn get_scratch_register(&mut self, forbidden: &[Register]) -> Result<Register, LoweringError> {
        // Use the allocator to get an unreserved scratch register
        let scratch = self
            .allocator
            .get_unreserved_scratch(forbidden)
            .ok_or(LoweringError::NoScratchRegister)?;

        // CRITICAL: Invalidate the register to ensure it's safe to use
        // This will spill any dirty value it contains. We emit spill operations
        // immediately to prevent any re-clobbering issues.
        self.allocator.invalidate_register(scratch);
        self.emit_spill_reload_ops();

        // Verify that the scratch register is actually empty after spill operations
        // This is a safety check to ensure no subsequent operations clobbered it
        debug_assert!(
            !self.allocator.is_register_allocated(scratch),
            "Scratch register {scratch:?} was clobbered after invalidation"
        );

        Ok(scratch)
    }

    fn emit(&mut self, instr: MachineInstr) {
        // Track VReg initialization when store instructions are actually emitted
        if let MachineInstr::MovMR {
            src: _,
            base,
            offset,
        } = &instr
        {
            if *base == Register::Rbp {
                // This is a store to stack - find which VReg this corresponds to
                if let Some(vreg) = self.allocator.find_vreg_for_slot(*offset as i64) {
                    self.allocator.mark_vreg_initialized(vreg);
                    if std::env::var("RUE_DEBUG").is_ok() {
                        eprintln!(
                            "Marking VReg {vreg:?} as initialized (stored to offset {offset})"
                        );
                    }
                }
            }
        }

        // NOTE: Push/pop tracking is handled in lower_push/lower_pop
        // to avoid double-counting. This method just emits instructions.
        self.instructions.push(instr);
    }

    /// Emit any pending spill/reload operations from the register allocator
    fn emit_spill_reload_ops(&mut self) {
        let ops = self.allocator.take_pending_ops();
        for op in ops {
            self.emit(op);
        }
    }

    /// Flush all dirty registers before control flow transfers
    /// This ensures that all values are written back to memory before jumping
    fn flush_for_cf(&mut self) {
        self.allocator.flush_stores();
        self.emit_spill_reload_ops();

        // After writing every live value to memory we must forget
        // about the registers, because the forthcoming control-flow edge or
        // call may clobber them.
        self.allocator.clear_all_registers();
    }
}
