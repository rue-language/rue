//! x86-64 backend for code generation
//!
//! This module converts platform-independent representation (PIR) with virtual registers
//! into x86-64 specific machine instructions with concrete registers.

use rue_ir::pir::{BinOp, Label, PIR, PhysicalRegId, VReg, Value};
use rue_target::{ConditionCode, LabelRef, X86Register, X8664Instr};
use std::collections::HashMap;
use tracing::{debug, trace};

/// Errors that can occur during lowering
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum LoweringError {
    /// Register allocation failed
    #[error("Register allocation error: {0}")]
    RegisterAllocation(String),

    /// Too many arguments for function call
    #[error("Too many arguments for function call")]
    TooManyArguments,

    /// Cannot pop from empty stack
    #[error("Cannot pop from empty stack: push_count would underflow")]
    StackUnderflow,

    /// Unsupported value type in operation
    #[error("PhysicalReg not supported in {0}")]
    UnsupportedValueType(&'static str),

    /// No available scratch register
    #[error("No available scratch register")]
    NoScratchRegister,
}

impl From<String> for LoweringError {
    fn from(s: String) -> Self {
        LoweringError::RegisterAllocation(s)
    }
}

/// Exit code for division by zero
const EXIT_DIV_ZERO: i64 = 250;

/// Convert abstract PhysicalRegId to concrete X86Register
/// This mapping defines the x86-64 calling convention for register assignment
fn physical_reg_id_to_x86(reg_id: PhysicalRegId) -> X86Register {
    match reg_id.0 {
        0 => X86Register::Rdi,  // First parameter register
        1 => X86Register::Rsi,  // Second parameter register
        2 => X86Register::Rdx,  // Third parameter register
        3 => X86Register::Rcx,  // Fourth parameter register
        4 => X86Register::R8,   // Fifth parameter register
        5 => X86Register::R9,   // Sixth parameter register
        6 => X86Register::Rax,  // Return value register
        7 => X86Register::Rbx,  // Callee-saved register
        8 => X86Register::Rbp,  // Base pointer
        9 => X86Register::Rsp,  // Stack pointer
        10 => X86Register::R10, // Scratch register
        11 => X86Register::R11, // Scratch register
        12 => X86Register::R12, // Callee-saved register
        13 => X86Register::R13, // Callee-saved register
        14 => X86Register::R14, // Callee-saved register
        15 => X86Register::R15, // Callee-saved register
        _ => panic!("Invalid PhysicalRegId: {}", reg_id.0),
    }
}

/// Interface for register allocators used by the lowering pass
pub trait RegisterAllocator {
    /// Ensure a virtual register is in a physical register, avoiding conflicts
    fn ensure_reg(&mut self, vreg: VReg, forbidden: &[X86Register]) -> Result<X86Register, String>;

    /// Assign a physical register for defining a new value
    fn assign_reg_for_def(
        &mut self,
        vreg: VReg,
        forbidden: &[X86Register],
    ) -> Result<X86Register, String>;

    /// Assign a physical register for 8-bit operations (SetCC)
    fn assign_reg_for_8bit_def(
        &mut self,
        vreg: VReg,
        forbidden: &[X86Register],
    ) -> Result<X86Register, String>;

    /// Get the current physical register for a virtual register, if any
    fn get_register(&self, vreg: VReg) -> Option<X86Register>;

    /// Check if a physical register is currently allocated
    fn is_register_allocated(&self, reg: X86Register) -> bool;

    /// Check if a physical register is being used as scratch
    fn is_scratch_register(&self, reg: X86Register) -> bool;

    /// Get an unreserved scratch register
    fn get_unreserved_scratch(&mut self, forbidden: &[X86Register]) -> Option<X86Register>;

    /// Invalidate (force spill) a physical register
    fn invalidate_register(&mut self, reg: X86Register);

    /// Schedule a store operation for a virtual register
    fn schedule_store(&mut self, vreg: VReg, reg: X86Register);

    /// Take pending spill/reload operations
    fn take_pending_ops(&mut self) -> Vec<X8664Instr>;

    /// Flush all dirty registers to memory
    fn flush_stores(&mut self);

    /// Clear all register mappings
    fn clear_all_registers(&mut self);

    /// Find which VReg corresponds to a stack slot
    fn find_vreg_for_slot(&self, offset: i64) -> Option<VReg>;

    /// Mark a VReg as initialized
    fn mark_vreg_initialized(&mut self, vreg: VReg);

    /// Get the total stack size needed
    fn get_stack_size(&self) -> u64;
}

/// The x86-64 code generator converts PIR instructions with virtual registers
/// into x86-64 specific machine instructions with concrete registers.
/// It delegates all spilling decisions to the RegisterAllocator.
pub struct X8664Codegen<'a> {
    allocator: &'a mut dyn RegisterAllocator,
    instructions: Vec<X8664Instr>,
    label_map: HashMap<Label, u32>,
    next_label_id: u32,
    /// External label map to use (if provided)
    external_label_map: Option<&'a HashMap<Label, u32>>,
    /// Track number of pushes since function entry for stack alignment
    push_count: i32,
    /// Track which stack slots are for block parameters (set during Store instructions)
    block_param_offsets: std::collections::HashSet<i64>,
}

impl<'a> X8664Codegen<'a> {
    pub fn new(allocator: &'a mut dyn RegisterAllocator, first_label_id: u32) -> Self {
        Self {
            allocator,
            instructions: Vec::new(),
            label_map: HashMap::new(),
            next_label_id: first_label_id,
            external_label_map: None,
            push_count: 0,
            block_param_offsets: std::collections::HashSet::new(),
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

    /// Lower a sequence of PIR instructions to machine instructions
    pub fn lower(&mut self, pir_instructions: &[PIR]) -> Result<Vec<X8664Instr>, LoweringError> {
        let start_len = self.instructions.len();

        // Process instructions
        for instr in pir_instructions {
            self.lower_instruction(instr)?;
        }

        Ok(self.instructions[start_len..].to_vec())
    }

    /// Patch stack allocation instructions with actual stack space needed
    pub fn patch_stack_allocation(
        instructions: &mut [X8664Instr],
        allocator: &dyn RegisterAllocator,
    ) {
        let stack_size = allocator.get_stack_size();

        // Find and patch AllocStack instructions
        for instr in instructions.iter_mut() {
            if let X8664Instr::AllocStack { size } = instr {
                *size = stack_size as u32;
            }
        }
    }

    fn lower_instruction(&mut self, instr: &PIR) -> Result<(), LoweringError> {
        match instr {
            PIR::Copy { dest, src } => self.lower_copy(*dest, src),
            PIR::BlockParamAssign { .. } => {
                // With "always spill" approach, block parameters are handled via Load/Store
                // This instruction should not be generated anymore
                Err(LoweringError::RegisterAllocation(
                    "BlockParamAssign should not be generated with always-spill approach"
                        .to_string(),
                ))
            }
            PIR::BinaryOp { dest, lhs, rhs, op } => self.lower_binary_op(*dest, lhs, rhs, op),
            PIR::TypedBinaryOp {
                dest,
                lhs,
                rhs,
                op,
                ty,
            } => self.lower_typed_binary_op(*dest, lhs, rhs, op, ty),
            PIR::Load { dest, offset } => self.lower_load(*dest, *offset),
            PIR::Store { src, offset } => self.lower_store(*src, *offset),
            PIR::Push { src } => self.lower_push(src),
            PIR::Pop { dest } => self.lower_pop(*dest),
            PIR::Label(_) => {
                // We are about to start a fresh basic block, so flush
                // all pending stores from the previous one.
                self.allocator.flush_stores();
                self.emit_spill_reload_ops();

                // Labels are handled externally in compile_to_executable
                // to ensure consistent numbering across functions
                Ok(())
            }
            PIR::Jump(target) => self.lower_jump(*target),
            PIR::Branch {
                condition,
                true_label,
                false_label,
            } => self.lower_branch(*condition, *true_label, *false_label),
            PIR::Call {
                dest,
                function,
                args,
            } => self.lower_call(dest.as_ref(), function, args),
            PIR::Return { value } => self.lower_return(value.as_ref()),
            PIR::Syscall {
                result,
                syscall_num,
                args,
            } => self.lower_syscall(*result, *syscall_num, args),
            PIR::SaveRegisters { registers } => self.lower_save_registers(registers),
            PIR::RestoreRegisters { registers } => self.lower_restore_registers(registers),
            PIR::EnterFrame => self.lower_enter_frame(),
            PIR::LeaveFrame => self.lower_leave_frame(),
        }
    }

    fn lower_copy(&mut self, dest: VReg, src: &Value) -> Result<(), LoweringError> {
        match src {
            Value::SignedImm(imm) => {
                let dest_reg = self.allocator.ensure_reg(dest, &[])?;
                self.emit_spill_reload_ops();
                if *imm >= i32::MIN as i64 && *imm <= i32::MAX as i64 {
                    self.emit(X8664Instr::MovRI32 {
                        dest: dest_reg,
                        imm: *imm as i32,
                    });
                } else {
                    self.emit(X8664Instr::MovRI64 {
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
                        self.emit(X8664Instr::MovRR {
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
                    self.emit(X8664Instr::MovRI32 {
                        dest: dest_reg,
                        imm: *imm as i32,
                    });
                } else {
                    self.emit(X8664Instr::MovRI64 {
                        dest: dest_reg,
                        imm: *imm as i64,
                    });
                }
                self.allocator.schedule_store(dest, dest_reg);
            }
            Value::PhysicalReg(reg_id) => {
                let reg = physical_reg_id_to_x86(*reg_id);
                // Use assign_reg_for_def since we're defining a new value (not reading old one)
                let dest_reg = self.allocator.assign_reg_for_def(dest, &[reg])?;
                self.emit_spill_reload_ops();
                if reg != dest_reg {
                    self.emit(X8664Instr::MovRR {
                        dest: dest_reg,
                        src: reg,
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
                            self.emit(X8664Instr::MovRR {
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
                            self.emit(X8664Instr::MovRI32 {
                                dest: dest_reg,
                                imm: *imm as i32,
                            });
                        } else {
                            self.emit(X8664Instr::MovRI64 {
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
                            self.emit(X8664Instr::MovRI32 {
                                dest: dest_reg,
                                imm: *imm as i32,
                            });
                        } else {
                            self.emit(X8664Instr::MovRI64 {
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
                        self.emit(X8664Instr::AddRR {
                            dest: dest_reg,
                            src: rhs_reg,
                        });
                        // Note: operations on already-dirty registers remain dirty
                    }
                    (BinOp::Add, Value::SignedImm(imm)) => {
                        if *imm >= i32::MIN as i64 && *imm <= i32::MAX as i64 {
                            self.emit(X8664Instr::AddRI {
                                dest: dest_reg,
                                imm: *imm as i32,
                            });
                        } else {
                            // Load large immediate to scratch register
                            let scratch = self.get_scratch_register(&[dest_reg])?;
                            self.emit(X8664Instr::MovRI64 {
                                dest: scratch,
                                imm: *imm,
                            });
                            self.emit(X8664Instr::AddRR {
                                dest: dest_reg,
                                src: scratch,
                            });
                        }
                        // Note: operations on already-dirty registers remain dirty
                    }
                    (BinOp::Sub, Value::VReg(rhs_vreg)) => {
                        let rhs_reg = self.allocator.ensure_reg(*rhs_vreg, &[dest_reg])?;
                        self.emit_spill_reload_ops();
                        self.emit(X8664Instr::SubRR {
                            dest: dest_reg,
                            src: rhs_reg,
                        });
                        // Note: operations on already-dirty registers remain dirty
                    }
                    (BinOp::Sub, Value::SignedImm(imm)) => {
                        if *imm >= i32::MIN as i64 && *imm <= i32::MAX as i64 {
                            self.emit(X8664Instr::SubRI {
                                dest: dest_reg,
                                imm: *imm as i32,
                            });
                        } else {
                            let scratch = self.get_scratch_register(&[dest_reg])?;
                            self.emit(X8664Instr::MovRI64 {
                                dest: scratch,
                                imm: *imm,
                            });
                            self.emit(X8664Instr::SubRR {
                                dest: dest_reg,
                                src: scratch,
                            });
                        }
                        // Note: operations on already-dirty registers remain dirty
                    }
                    (BinOp::Mul, Value::VReg(rhs_vreg)) => {
                        let rhs_reg = self.allocator.ensure_reg(*rhs_vreg, &[dest_reg])?;
                        self.emit_spill_reload_ops();
                        self.emit(X8664Instr::ImulRR {
                            dest: dest_reg,
                            src: rhs_reg,
                        });
                        // Note: operations on already-dirty registers remain dirty
                    }
                    (BinOp::Mul, Value::SignedImm(imm)) => {
                        if *imm >= i32::MIN as i64 && *imm <= i32::MAX as i64 {
                            self.emit(X8664Instr::ImulRI {
                                dest: dest_reg,
                                imm: *imm as i32,
                            });
                        } else {
                            let scratch = self.get_scratch_register(&[dest_reg])?;
                            self.emit(X8664Instr::MovRI64 {
                                dest: scratch,
                                imm: *imm,
                            });
                            self.emit(X8664Instr::ImulRR {
                                dest: dest_reg,
                                src: scratch,
                            });
                        }
                        // Note: operations on already-dirty registers remain dirty
                    }
                    (BinOp::Add, Value::UnsignedImm(imm)) => {
                        if *imm <= i32::MAX as u64 {
                            self.emit(X8664Instr::AddRI {
                                dest: dest_reg,
                                imm: *imm as i32,
                            });
                        } else {
                            // Load large immediate to scratch register
                            let scratch = self.get_scratch_register(&[dest_reg])?;
                            self.emit(X8664Instr::MovRI64 {
                                dest: scratch,
                                imm: *imm as i64,
                            });
                            self.emit(X8664Instr::AddRR {
                                dest: dest_reg,
                                src: scratch,
                            });
                        }
                        // Note: operations on already-dirty registers remain dirty
                    }
                    (BinOp::Sub, Value::UnsignedImm(imm)) => {
                        if *imm <= i32::MAX as u64 {
                            self.emit(X8664Instr::SubRI {
                                dest: dest_reg,
                                imm: *imm as i32,
                            });
                        } else {
                            let scratch = self.get_scratch_register(&[dest_reg])?;
                            self.emit(X8664Instr::MovRI64 {
                                dest: scratch,
                                imm: *imm as i64,
                            });
                            self.emit(X8664Instr::SubRR {
                                dest: dest_reg,
                                src: scratch,
                            });
                        }
                        // Note: operations on already-dirty registers remain dirty
                    }
                    (BinOp::Mul, Value::UnsignedImm(imm)) => {
                        if *imm <= i32::MAX as u64 {
                            self.emit(X8664Instr::ImulRI {
                                dest: dest_reg,
                                imm: *imm as i32,
                            });
                        } else {
                            let scratch = self.get_scratch_register(&[dest_reg])?;
                            self.emit(X8664Instr::MovRI64 {
                                dest: scratch,
                                imm: *imm as i64,
                            });
                            self.emit(X8664Instr::ImulRR {
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

        debug!(
            target: "rue::codegen",
            ?dest, ?op, ?ty,
            "Lowering typed binary operation"
        );

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

            debug!(
                target: "rue::codegen",
                ?dest_reg,
                "Adding i32 truncation"
            );

            // Truncate to 32-bit and sign-extend back to 64-bit
            // This ensures i32 overflow wraps correctly (movsxd automatically truncates and sign-extends)
            self.emit(X8664Instr::Movsxd {
                dest: dest_reg,
                src: dest_reg,
            });
        }

        Ok(())
    }

    fn lower_modulo(&mut self, dest: VReg, lhs: &Value, rhs: &Value) -> Result<(), LoweringError> {
        // Modulo is like division but we want the remainder from RDX
        let rax = X86Register::Rax;
        let rdx = X86Register::Rdx;

        // Move lhs to RAX
        match lhs {
            Value::VReg(vreg) => {
                let lhs_reg = self.allocator.ensure_reg(*vreg, &[])?;
                self.emit_spill_reload_ops();
                if lhs_reg != rax {
                    self.emit(X8664Instr::MovRR {
                        dest: rax,
                        src: lhs_reg,
                    });
                }
            }
            Value::SignedImm(imm) => {
                if *imm >= i32::MIN as i64 && *imm <= i32::MAX as i64 {
                    self.emit(X8664Instr::MovRI32 {
                        dest: rax,
                        imm: *imm as i32,
                    });
                } else {
                    self.emit(X8664Instr::MovRI64 {
                        dest: rax,
                        imm: *imm,
                    });
                }
            }
            Value::UnsignedImm(imm) => {
                if *imm <= i32::MAX as u64 {
                    self.emit(X8664Instr::MovRI32 {
                        dest: rax,
                        imm: *imm as i32,
                    });
                } else {
                    self.emit(X8664Instr::MovRI64 {
                        dest: rax,
                        imm: *imm as i64,
                    });
                }
            }
            Value::PhysicalReg(_) => return Err(LoweringError::UnsupportedValueType("modulo")),
        }

        // Sign extend RAX to RDX:RAX
        self.emit(X8664Instr::Cqo);

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
                    self.emit(X8664Instr::MovRI32 {
                        dest: scratch,
                        imm: *imm as i32,
                    });
                } else {
                    self.emit(X8664Instr::MovRI64 {
                        dest: scratch,
                        imm: *imm,
                    });
                }
                scratch
            }
            Value::UnsignedImm(imm) => {
                let scratch = self.get_scratch_register(&[rax, rdx])?;
                if *imm <= i32::MAX as u64 {
                    self.emit(X8664Instr::MovRI32 {
                        dest: scratch,
                        imm: *imm as i32,
                    });
                } else {
                    self.emit(X8664Instr::MovRI64 {
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
        self.emit(X8664Instr::CmpRI {
            reg: divisor_reg,
            imm: 0,
        });

        // Jump if not zero
        let mod_ok_label = self.new_label();
        self.emit(X8664Instr::JmpCC {
            cc: ConditionCode::NotEqual,
            target: LabelRef::Local(mod_ok_label),
        });

        // Modulo by zero - exit with code EXIT_DIV_ZERO
        self.emit(X8664Instr::MovRI64 {
            dest: X86Register::Rdi,
            imm: EXIT_DIV_ZERO,
        });
        self.emit(X8664Instr::MovRI64 {
            dest: X86Register::Rax,
            imm: 60, // sys_exit
        });
        self.emit(X8664Instr::Syscall);
        // Mark unreachable - sys_exit never returns
        self.emit(X8664Instr::Ud2);

        // Continue with division
        self.emit(X8664Instr::Label { id: mod_ok_label });

        // Perform division
        self.emit(X8664Instr::Idiv {
            divisor: divisor_reg,
        });

        // CRITICAL: Mark RDX as dirty since idiv writes remainder to it
        // This prevents the allocator from thinking RDX is clean
        self.allocator.invalidate_register(rdx); // Force spill of any value in RDX

        // Move remainder from RDX to dest (this is the key difference from division)
        let dest_reg = self.allocator.ensure_reg(dest, &[])?;
        self.emit_spill_reload_ops();
        if dest_reg != rdx {
            self.emit(X8664Instr::MovRR {
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
        let rax = X86Register::Rax;
        let rdx = X86Register::Rdx;

        // Move lhs to RAX
        match lhs {
            Value::VReg(vreg) => {
                let lhs_reg = self.allocator.ensure_reg(*vreg, &[])?;
                self.emit_spill_reload_ops();
                if lhs_reg != rax {
                    self.emit(X8664Instr::MovRR {
                        dest: rax,
                        src: lhs_reg,
                    });
                }
            }
            Value::SignedImm(imm) => {
                if *imm >= i32::MIN as i64 && *imm <= i32::MAX as i64 {
                    self.emit(X8664Instr::MovRI32 {
                        dest: rax,
                        imm: *imm as i32,
                    });
                } else {
                    self.emit(X8664Instr::MovRI64 {
                        dest: rax,
                        imm: *imm,
                    });
                }
            }
            Value::UnsignedImm(imm) => {
                if *imm <= i32::MAX as u64 {
                    self.emit(X8664Instr::MovRI32 {
                        dest: rax,
                        imm: *imm as i32,
                    });
                } else {
                    self.emit(X8664Instr::MovRI64 {
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
        self.emit(X8664Instr::Cqo);

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
                    self.emit(X8664Instr::MovRI32 {
                        dest: scratch,
                        imm: *imm as i32,
                    });
                } else {
                    self.emit(X8664Instr::MovRI64 {
                        dest: scratch,
                        imm: *imm,
                    });
                }
                scratch
            }
            Value::UnsignedImm(imm) => {
                let scratch = self.get_scratch_register(&[rax, rdx])?;
                if *imm <= i32::MAX as u64 {
                    self.emit(X8664Instr::MovRI32 {
                        dest: scratch,
                        imm: *imm as i32,
                    });
                } else {
                    self.emit(X8664Instr::MovRI64 {
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
        self.emit(X8664Instr::CmpRI {
            reg: divisor_reg,
            imm: 0,
        });

        // Jump if not zero
        let div_ok_label = self.new_label();
        self.emit(X8664Instr::JmpCC {
            cc: ConditionCode::NotEqual,
            target: LabelRef::Local(div_ok_label),
        });

        // Division by zero - exit with code EXIT_DIV_ZERO
        self.emit(X8664Instr::MovRI64 {
            dest: X86Register::Rdi,
            imm: EXIT_DIV_ZERO,
        });
        self.emit(X8664Instr::MovRI64 {
            dest: X86Register::Rax,
            imm: 60, // sys_exit
        });
        self.emit(X8664Instr::Syscall);
        // Mark unreachable - sys_exit never returns
        self.emit(X8664Instr::Ud2);

        // Continue with division
        self.emit(X8664Instr::Label { id: div_ok_label });

        // Perform division
        self.emit(X8664Instr::Idiv {
            divisor: divisor_reg,
        });

        // CRITICAL: Mark RDX as dirty since idiv writes remainder to it
        // This prevents the allocator from thinking RDX is clean
        self.allocator.invalidate_register(rdx); // Force spill of any value in RDX

        // Move result from RAX to dest
        let dest_reg = self.allocator.ensure_reg(dest, &[])?;
        self.emit_spill_reload_ops();
        if dest_reg != rax {
            self.emit(X8664Instr::MovRR {
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
                    self.emit(X8664Instr::MovRI32 {
                        dest: scratch,
                        imm: *imm as i32,
                    });
                } else {
                    self.emit(X8664Instr::MovRI64 {
                        dest: scratch,
                        imm: *imm,
                    });
                }
                scratch
            }
            Value::UnsignedImm(imm) => {
                let scratch = self.get_scratch_register(&[])?;
                if *imm <= i32::MAX as u64 {
                    self.emit(X8664Instr::MovRI32 {
                        dest: scratch,
                        imm: *imm as i32,
                    });
                } else {
                    self.emit(X8664Instr::MovRI64 {
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
                self.emit(X8664Instr::CmpRR {
                    left: lhs_reg,
                    right: rhs_reg,
                });
            }
            Value::SignedImm(imm) => {
                if *imm >= i32::MIN as i64 && *imm <= i32::MAX as i64 {
                    self.emit(X8664Instr::CmpRI {
                        reg: lhs_reg,
                        imm: *imm as i32,
                    });
                } else {
                    let scratch = self.get_scratch_register(&[lhs_reg])?;
                    self.emit(X8664Instr::MovRI64 {
                        dest: scratch,
                        imm: *imm,
                    });
                    self.emit(X8664Instr::CmpRR {
                        left: lhs_reg,
                        right: scratch,
                    });
                }
            }
            Value::UnsignedImm(imm) => {
                if *imm <= i32::MAX as u64 {
                    self.emit(X8664Instr::CmpRI {
                        reg: lhs_reg,
                        imm: *imm as i32,
                    });
                } else {
                    let scratch = self.get_scratch_register(&[lhs_reg])?;
                    self.emit(X8664Instr::MovRI64 {
                        dest: scratch,
                        imm: *imm as i64,
                    });
                    self.emit(X8664Instr::CmpRR {
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
        self.emit(X8664Instr::SetCC { dest: dest_reg, cc });
        self.emit(X8664Instr::Movzx {
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
            dest_reg != X86Register::Rsp && dest_reg != X86Register::Rbp,
            "Cannot use RSP/RBP as destination register"
        );
        self.emit_spill_reload_ops();
        self.emit(X8664Instr::MovRM {
            // load from stack slot
            dest: dest_reg,
            base: X86Register::Rbp, // <- use the frame-pointer
            offset: offset as i32,
        });
        // Mark the destination as dirty after loading from memory
        self.allocator.schedule_store(dest, dest_reg);
        Ok(())
    }

    fn lower_store(&mut self, src: VReg, offset: i64) -> Result<(), LoweringError> {
        debug!(
            target: "rue::codegen",
            ?src,
            offset,
            "Lowering store operation"
        );

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
        trace!(
            target: "rue::codegen",
            ?src,
            ?src_reg,
            "Store: vreg is in register"
        );
        self.emit(X8664Instr::MovMR {
            // store to stack slot
            base: X86Register::Rbp, // <- use the frame-pointer
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
                self.emit(X8664Instr::Push { reg: src_reg });
            }
            Value::PhysicalReg(reg_id) => {
                let reg = physical_reg_id_to_x86(*reg_id);
                // Physical register can be pushed directly
                self.emit(X8664Instr::Push { reg });
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
            dest_reg != X86Register::Rsp && dest_reg != X86Register::Rbp,
            "Cannot use RSP/RBP as destination register"
        );
        self.emit_spill_reload_ops();
        self.emit(X8664Instr::Pop { reg: dest_reg });
        self.push_count -= 1;
        // Mark the destination as dirty after popping into it
        self.allocator.schedule_store(dest, dest_reg);
        Ok(())
    }

    fn lower_jump(&mut self, target: Label) -> Result<(), LoweringError> {
        // Write back all dirty VRegs before we leave this block.
        self.flush_for_cf();

        let machine_label_id = self.get_or_create_label(target);
        self.emit(X8664Instr::Jmp {
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
        self.emit(X8664Instr::CmpRI {
            reg: cond_reg,
            imm: 0,
        });

        // After CMP the flags are set and `cond_reg` is no longer needed,
        // so it is now safe to spill every dirty register.
        self.flush_for_cf();

        // Jump to true label if not zero
        let true_id = self.get_or_create_label(true_label);
        self.emit(X8664Instr::JmpCC {
            cc: ConditionCode::NotEqual,
            target: LabelRef::Local(true_id),
        });

        // Fall through to false label
        let false_id = self.get_or_create_label(false_label);
        self.emit(X8664Instr::Jmp {
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
            X86Register::Rdi,
            X86Register::Rsi,
            X86Register::Rdx,
            X86Register::Rcx,
            X86Register::R8,
            X86Register::R9,
        ];

        // Define caller-saved registers
        let caller_saved_regs = [
            X86Register::Rax,
            X86Register::Rcx,
            X86Register::Rdx,
            X86Register::Rsi,
            X86Register::Rdi,
            X86Register::R8,
            X86Register::R9,
            X86Register::R10,
            X86Register::R11,
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
                self.emit(X8664Instr::Push { reg });
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
                self.emit(X8664Instr::SubRI {
                    dest: X86Register::Rsp,
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
                    self.emit(X8664Instr::MovRR {
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
            self.emit(X8664Instr::MovRR {
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
                self.emit(X8664Instr::MovRR {
                    dest: arg_regs[blocking_idx],
                    src: arg_locations[blocking_idx],
                });
            }
            moved[blocking_idx] = true;

            // Now move our value from temp to its destination
            self.emit(X8664Instr::MovRR {
                dest: arg_regs[i],
                src: temp_reg,
            });
            moved[i] = true;
        }

        // Call the function
        self.emit(X8664Instr::Call {
            target: runtime_function.to_string(),
        });

        // Handle return value preservation (only for runtime functions)
        let rax_temp_reg =
            if !is_user_function && dest.is_some() && regs_to_save.contains(&X86Register::Rax) {
                // RAX will be overwritten when we restore, so save to temporary register first
                // IMPORTANT: Include all regs_to_save and the argument registers in forbidden list
                // to ensure the scratch register doesn't conflict with any register we're about to restore
                let mut forbidden = regs_to_save.clone();
                forbidden.extend(&arg_regs[..args.len()]);
                let temp_reg = self.get_scratch_register(&forbidden)?;
                self.emit(X8664Instr::MovRR {
                    dest: temp_reg,
                    src: X86Register::Rax,
                });
                Some(temp_reg)
            } else {
                None
            };

        // Restore alignment padding if we added it (only for runtime functions)
        if !is_user_function && needs_padding {
            self.emit(X8664Instr::AddRI {
                dest: X86Register::Rsp,
                imm: 8,
            });
        }

        // Restore caller-saved registers in reverse order (only for runtime functions)
        if !is_user_function {
            for &reg in regs_to_save.iter().rev() {
                if self.push_count <= 0 {
                    return Err(LoweringError::StackUnderflow);
                }
                self.emit(X8664Instr::Pop { reg });
                self.push_count -= 1;
            }
        }

        // Now move the return value to its destination
        if let Some(dest_vreg) = dest {
            // Use assign_reg_for_def since we're defining a new value (the return value)
            let dest_reg = self.allocator.assign_reg_for_def(*dest_vreg, &[])?;
            debug_assert!(
                dest_reg != X86Register::Rsp && dest_reg != X86Register::Rbp,
                "Cannot use RSP/RBP as destination register"
            );
            self.emit_spill_reload_ops();

            // Determine where the return value is
            let source_reg = if let Some(temp_reg) = rax_temp_reg {
                temp_reg // We saved it to this temp register (runtime functions only)
            } else {
                X86Register::Rax // Still in RAX
            };

            if dest_reg != source_reg {
                self.emit(X8664Instr::MovRR {
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
            debug!(
                target: "rue::codegen",
                ?vreg,
                "Lowering return with value"
            );
            // Now load the return value after all other values have been flushed
            // This ensures the return value register won't be used as a scratch register
            let value_reg = self.allocator.ensure_reg(*vreg, &[])?;
            self.emit_spill_reload_ops();
            trace!(
                target: "rue::codegen",
                ?vreg,
                ?value_reg,
                "Return: vreg in register"
            );
            if value_reg != X86Register::Rax {
                self.emit(X8664Instr::MovRR {
                    dest: X86Register::Rax,
                    src: value_reg,
                });
            }
        } else {
            // No explicit return value - return 0 (unit type)
            self.emit(X8664Instr::MovRI64 {
                dest: X86Register::Rax,
                imm: 0,
            });
        }

        // Standard SysV epilogue
        self.emit(X8664Instr::LeaveFrame); // mov rsp, rbp ; pop rbp
        self.emit(X8664Instr::Ret);

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
            X86Register::Rdi,
            X86Register::Rsi,
            X86Register::Rdx,
            X86Register::R10,
            X86Register::R8,
            X86Register::R9,
        ];

        // Move syscall number to RAX
        let num_reg = self.allocator.ensure_reg(syscall_num, &[])?;
        self.emit_spill_reload_ops();
        if num_reg != X86Register::Rax {
            self.emit(X8664Instr::MovRR {
                dest: X86Register::Rax,
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
                self.emit(X8664Instr::MovRR {
                    dest: syscall_arg_regs[i],
                    src: arg_reg,
                });
            }
        }

        self.emit(X8664Instr::Syscall);

        // Move result from RAX
        let result_reg = self.allocator.ensure_reg(result, &[])?;
        debug_assert!(
            result_reg != X86Register::Rsp && result_reg != X86Register::Rbp,
            "Cannot use RSP/RBP as destination register"
        );
        self.emit_spill_reload_ops();
        if result_reg != X86Register::Rax {
            self.emit(X8664Instr::MovRR {
                dest: result_reg,
                src: X86Register::Rax,
            });
        }
        // Mark the result as dirty after syscall
        self.allocator.schedule_store(result, result_reg);

        Ok(())
    }

    fn lower_save_registers(&mut self, registers: &[PhysicalRegId]) -> Result<(), LoweringError> {
        for &reg_id in registers {
            let reg = physical_reg_id_to_x86(reg_id);
            self.emit(X8664Instr::Push { reg });
        }
        Ok(())
    }

    fn lower_restore_registers(
        &mut self,
        registers: &[PhysicalRegId],
    ) -> Result<(), LoweringError> {
        for &reg_id in registers.iter().rev() {
            let reg = physical_reg_id_to_x86(reg_id);
            self.emit(X8664Instr::Pop { reg });
        }
        Ok(())
    }

    fn lower_enter_frame(&mut self) -> Result<(), LoweringError> {
        // Standard x86-64 function prologue
        self.emit(X8664Instr::EnterFrame);

        // EnterFrame pushes rbp, so we start with 1 push
        // (plus the return address pushed by call = 2 total, which is aligned)
        self.push_count = 1;

        // We emit a placeholder AllocStack that will be patched later
        // with the actual required stack space after we know how many spills we need.
        // We use 0 as a placeholder value that will be replaced.
        self.emit(X8664Instr::AllocStack { size: 0 });

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
        self.emit(X8664Instr::LeaveFrame);
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

    fn get_scratch_register(
        &mut self,
        forbidden: &[X86Register],
    ) -> Result<X86Register, LoweringError> {
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

    fn emit(&mut self, instr: X8664Instr) {
        // Track VReg initialization when store instructions are actually emitted
        if let X8664Instr::MovMR {
            src: _,
            base,
            offset,
        } = &instr
        {
            if *base == X86Register::Rbp {
                // This is a store to stack - find which VReg this corresponds to
                if let Some(vreg) = self.allocator.find_vreg_for_slot(*offset as i64) {
                    self.allocator.mark_vreg_initialized(vreg);
                    trace!(
                        target: "rue::codegen::regalloc",
                        ?vreg,
                        offset,
                        "Marking VReg as initialized (stored to stack)"
                    );
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
