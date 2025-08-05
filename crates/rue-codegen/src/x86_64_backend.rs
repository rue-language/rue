//! x86-64 backend for code generation
//!
//! This module converts platform-independent representation (PIR) with virtual registers
//! into x86-64 specific machine instructions with concrete registers.

use rue_ir::pir::{BinOp, Label, PIR, PhysicalRegId, VReg, Value};
use rue_target::{ConditionCode, LabelRef, X86Register, X8664Instr};
use std::collections::HashMap;
use tracing::{debug, trace, warn};

use crate::constants::{EXIT_CODE_BOUNDS_CHECK, EXIT_CODE_DIV_BY_ZERO};

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

    /// Invalid aggregate size for return value
    #[error("Invalid aggregate size for return value: {0} bytes")]
    InvalidAggregateSize(i64),
}

impl From<String> for LoweringError {
    fn from(s: String) -> Self {
        LoweringError::RegisterAllocation(s)
    }
}

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
    /// Track VRegs that represent aggregate pointers (VReg -> size in bytes)
    aggregate_vregs: HashMap<VReg, i64>,
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
            aggregate_vregs: HashMap::new(),
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
                return_type,
                return_size,
            } => self.lower_call(
                dest.as_ref(),
                function,
                args,
                return_type.as_ref(),
                return_size.as_ref(),
            ),
            PIR::Return { value } => self.lower_return(value.as_ref()),
            PIR::ReturnSmallAggregate { value, size } => {
                self.lower_return_small_aggregate(*value, *size)
            }
            PIR::ReturnLargeAggregate { value, size } => {
                self.lower_return_large_aggregate(*value, *size)
            }
            PIR::Syscall {
                result,
                syscall_num,
                args,
            } => self.lower_syscall(*result, *syscall_num, args),
            PIR::SaveRegisters { registers } => self.lower_save_registers(registers),
            PIR::RestoreRegisters { registers } => self.lower_restore_registers(registers),
            PIR::EnterFrame => self.lower_enter_frame(),
            PIR::LeaveFrame => self.lower_leave_frame(),
            PIR::AllocateAggregate {
                dest,
                size,
                alignment,
            } => self.lower_allocate_aggregate(*dest, *size, *alignment),
            PIR::CopyAggregate { dest, src, size } => self.lower_copy_aggregate(*dest, *src, *size),
            PIR::ZeroAggregate { dest, size } => self.lower_zero_aggregate(*dest, *size),
            PIR::InlineCopyAggregate { dest, src, size } => {
                self.lower_inline_copy_aggregate(*dest, *src, *size)
            }
            PIR::InlineZeroAggregate { dest, size } => {
                self.lower_inline_zero_aggregate(*dest, *size)
            }
            PIR::LoadField {
                dest,
                base,
                offset,
                field_type,
            } => self.lower_load_field(*dest, *base, *offset, field_type),
            PIR::StoreField {
                base,
                offset,
                src,
                field_type,
            } => self.lower_store_field(*base, *offset, src, field_type),
            PIR::ArrayBoundsCheck {
                array_base,
                index,
                array_len,
                trap_label,
            } => self.lower_array_bounds_check(*array_base, *index, *array_len, *trap_label),
            PIR::DynamicLoadField {
                dest,
                base,
                index,
                element_size,
                element_type,
            } => self.lower_dynamic_load_field(*dest, *base, *index, *element_size, element_type),
            PIR::DynamicStoreField {
                base,
                index,
                src,
                element_size,
                element_type,
            } => self.lower_dynamic_store_field(*base, *index, src, *element_size, element_type),
            PIR::Trap { message } => self.lower_trap(message),
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
                        // STRUCT FIELD ACCESS FIX: Ensure stable register state for field access operations
                        // First, flush all pending stores to ensure field values are safely in memory
                        self.allocator.flush_stores();
                        self.emit_spill_reload_ops();

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

                        // CRITICAL: Force another flush after loading left operand to ensure stability
                        // This prevents register corruption when processing the right operand
                        self.allocator.flush_stores();
                        self.emit_spill_reload_ops();

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
                        // FINAL FIX: Robust handling of register allocation for field access operations
                        // The core issue is that when both operands come from field loads (like v1.x + v2.x),
                        // the register allocator can corrupt values during ensure_reg calls.
                        // Solution: Force all register state to be stable before loading RHS.

                        // First, ensure all pending spill/reload operations are completed
                        // This stabilizes the current register allocation state
                        self.emit_spill_reload_ops();

                        // Force a complete flush of all dirty registers to memory
                        // This ensures that any field values are safely stored
                        self.allocator.flush_stores();
                        self.emit_spill_reload_ops();

                        // Now load RHS into a register, knowing that LHS is safely in memory if needed
                        let rhs_reg = self.allocator.ensure_reg(*rhs_vreg, &[dest_reg])?;
                        self.emit_spill_reload_ops();

                        // At this point:
                        // - dest_reg contains LHS (may have been reloaded from memory if necessary)
                        // - rhs_reg contains RHS (freshly loaded)
                        // Both values are now stable for the arithmetic operation
                        self.emit(X8664Instr::AddRR {
                            dest: dest_reg,
                            src: rhs_reg,
                        });
                        // Schedule store to ensure result is written back to memory
                        self.allocator.schedule_store(dest, dest_reg);
                    }
                    (BinOp::Add, Value::SignedImm(imm)) => {
                        if *imm >= i32::MIN as i64 && *imm <= i32::MAX as i64 {
                            self.emit(X8664Instr::AddRI {
                                dest: dest_reg,
                                imm: *imm as i32,
                            });
                            // Schedule store to ensure result is written back to memory
                            self.allocator.schedule_store(dest, dest_reg);
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
                            // Schedule store to ensure result is written back to memory
                            self.allocator.schedule_store(dest, dest_reg);
                        }
                    }
                    (BinOp::Sub, Value::VReg(rhs_vreg)) => {
                        // Apply the same robust fix as for addition
                        self.emit_spill_reload_ops();
                        self.allocator.flush_stores();
                        self.emit_spill_reload_ops();

                        let rhs_reg = self.allocator.ensure_reg(*rhs_vreg, &[dest_reg])?;
                        self.emit_spill_reload_ops();

                        self.emit(X8664Instr::SubRR {
                            dest: dest_reg,
                            src: rhs_reg,
                        });
                        // Schedule store to ensure result is written back to memory
                        self.allocator.schedule_store(dest, dest_reg);
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
                        // Schedule store to ensure result is written back to memory
                        self.allocator.schedule_store(dest, dest_reg);
                    }
                    (BinOp::Mul, Value::VReg(rhs_vreg)) => {
                        // Apply the same robust fix as for addition
                        self.emit_spill_reload_ops();
                        self.allocator.flush_stores();
                        self.emit_spill_reload_ops();

                        let rhs_reg = self.allocator.ensure_reg(*rhs_vreg, &[dest_reg])?;
                        self.emit_spill_reload_ops();

                        self.emit(X8664Instr::ImulRR {
                            dest: dest_reg,
                            src: rhs_reg,
                        });
                        // Schedule store to ensure result is written back to memory
                        self.allocator.schedule_store(dest, dest_reg);
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
                        // Schedule store to ensure result is written back to memory
                        self.allocator.schedule_store(dest, dest_reg);
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
                        // Schedule store to ensure result is written back to memory
                        self.allocator.schedule_store(dest, dest_reg);
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
                        // Schedule store to ensure result is written back to memory
                        self.allocator.schedule_store(dest, dest_reg);
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
                        // Schedule store to ensure result is written back to memory
                        self.allocator.schedule_store(dest, dest_reg);
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

        // For comparison operations, delegate to regular binary operation
        // Division and modulo will be handled below with proper type awareness
        if matches!(
            op,
            BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge | BinOp::Eq | BinOp::Ne
        ) {
            return self.lower_binary_op(dest, lhs, rhs, op);
        }

        // For i32 operations, use 32-bit instructions directly
        if matches!(ty, RueType::I32) {
            debug!(
                target: "rue::codegen",
                ?dest, ?op,
                "Using 32-bit arithmetic instructions for i32"
            );
            self.lower_binary_op_32bit(dest, lhs, rhs, op)?;
        } else {
            // For i64 and other types, use regular binary operation logic
            self.lower_binary_op(dest, lhs, rhs, op)?;
        }

        Ok(())
    }

    /// Lower a 32-bit binary operation using 32-bit instructions
    fn lower_binary_op_32bit(
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
                        // STRUCT FIELD ACCESS FIX: Ensure stable register state for field access operations
                        // First, flush all pending stores to ensure field values are safely in memory
                        self.allocator.flush_stores();
                        self.emit_spill_reload_ops();

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

                        // CRITICAL: Force another flush after loading left operand to ensure stability
                        // This prevents register corruption when processing the right operand
                        self.allocator.flush_stores();
                        self.emit_spill_reload_ops();

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
                            // This shouldn't happen for i32 constants, but handle it
                            self.emit(X8664Instr::MovRI64 {
                                dest: dest_reg,
                                imm: *imm,
                            });
                        }
                        dest_reg
                    }
                    Value::UnsignedImm(imm) => {
                        let dest_reg = self.allocator.assign_reg_for_def(dest, &[])?;
                        self.emit_spill_reload_ops();
                        if *imm <= i32::MAX as u64 {
                            self.emit(X8664Instr::MovRI32 {
                                dest: dest_reg,
                                imm: *imm as i32,
                            });
                        } else {
                            // This shouldn't happen for i32 constants, but handle it
                            self.emit(X8664Instr::MovRI64 {
                                dest: dest_reg,
                                imm: *imm as i64,
                            });
                        }
                        dest_reg
                    }
                    Value::PhysicalReg(_reg_id) => {
                        // Physical register handling for i32 operations - this case is likely rare
                        // For now, delegate to the regular binary operation
                        return self.lower_binary_op(dest, lhs, rhs, op);
                    }
                };

                // Apply the operation with rhs using 32-bit instructions
                match (op, rhs) {
                    (BinOp::Add, Value::VReg(rhs_vreg)) => {
                        // Apply the same robust fix as for addition
                        self.emit_spill_reload_ops();
                        self.allocator.flush_stores();

                        let rhs_reg = self.allocator.ensure_reg(*rhs_vreg, &[dest_reg])?;
                        self.emit_spill_reload_ops();

                        self.emit(X8664Instr::AddRR32 {
                            dest: dest_reg,
                            src: rhs_reg,
                        });
                        // Schedule store to ensure result is written back to memory
                        self.allocator.schedule_store(dest, dest_reg);
                    }
                    (BinOp::Add, Value::SignedImm(imm)) => {
                        if *imm >= i32::MIN as i64 && *imm <= i32::MAX as i64 {
                            self.emit(X8664Instr::AddRI32 {
                                dest: dest_reg,
                                imm: *imm as i32,
                            });
                            // Schedule store to ensure result is written back to memory
                            self.allocator.schedule_store(dest, dest_reg);
                        } else {
                            // This shouldn't happen for i32 constants
                            return Err(LoweringError::RegisterAllocation(
                                "i32 immediate value out of range".to_string(),
                            ));
                        }
                    }
                    (BinOp::Sub, Value::VReg(rhs_vreg)) => {
                        // Apply the same robust fix as for addition
                        self.emit_spill_reload_ops();
                        self.allocator.flush_stores();

                        let rhs_reg = self.allocator.ensure_reg(*rhs_vreg, &[dest_reg])?;
                        self.emit_spill_reload_ops();

                        self.emit(X8664Instr::SubRR32 {
                            dest: dest_reg,
                            src: rhs_reg,
                        });
                        // Schedule store to ensure result is written back to memory
                        self.allocator.schedule_store(dest, dest_reg);
                    }
                    (BinOp::Sub, Value::SignedImm(imm)) => {
                        if *imm >= i32::MIN as i64 && *imm <= i32::MAX as i64 {
                            self.emit(X8664Instr::SubRI32 {
                                dest: dest_reg,
                                imm: *imm as i32,
                            });
                            // Schedule store to ensure result is written back to memory
                            self.allocator.schedule_store(dest, dest_reg);
                        } else {
                            // This shouldn't happen for i32 constants
                            return Err(LoweringError::RegisterAllocation(
                                "i32 immediate value out of range".to_string(),
                            ));
                        }
                    }
                    (BinOp::Mul, Value::VReg(rhs_vreg)) => {
                        // Apply the same robust fix as for addition
                        self.emit_spill_reload_ops();
                        self.allocator.flush_stores();

                        let rhs_reg = self.allocator.ensure_reg(*rhs_vreg, &[dest_reg])?;
                        self.emit_spill_reload_ops();

                        self.emit(X8664Instr::ImulRR32 {
                            dest: dest_reg,
                            src: rhs_reg,
                        });
                        // Schedule store to ensure result is written back to memory
                        self.allocator.schedule_store(dest, dest_reg);
                    }
                    (BinOp::Mul, Value::SignedImm(imm)) => {
                        if *imm >= i32::MIN as i64 && *imm <= i32::MAX as i64 {
                            self.emit(X8664Instr::ImulRI32 {
                                dest: dest_reg,
                                imm: *imm as i32,
                            });
                            // Schedule store to ensure result is written back to memory
                            self.allocator.schedule_store(dest, dest_reg);
                        } else {
                            // This shouldn't happen for i32 constants
                            return Err(LoweringError::RegisterAllocation(
                                "i32 immediate value out of range".to_string(),
                            ));
                        }
                    }
                    (BinOp::Add, Value::UnsignedImm(imm)) => {
                        if *imm <= i32::MAX as u64 {
                            self.emit(X8664Instr::AddRI32 {
                                dest: dest_reg,
                                imm: *imm as i32,
                            });
                            // Schedule store to ensure result is written back to memory
                            self.allocator.schedule_store(dest, dest_reg);
                        } else {
                            // This shouldn't happen for i32 constants
                            return Err(LoweringError::RegisterAllocation(
                                "i32 immediate value out of range".to_string(),
                            ));
                        }
                    }
                    (BinOp::Sub, Value::UnsignedImm(imm)) => {
                        if *imm <= i32::MAX as u64 {
                            self.emit(X8664Instr::SubRI32 {
                                dest: dest_reg,
                                imm: *imm as i32,
                            });
                            // Schedule store to ensure result is written back to memory
                            self.allocator.schedule_store(dest, dest_reg);
                        } else {
                            // This shouldn't happen for i32 constants
                            return Err(LoweringError::RegisterAllocation(
                                "i32 immediate value out of range".to_string(),
                            ));
                        }
                    }
                    (BinOp::Mul, Value::UnsignedImm(imm)) => {
                        if *imm <= i32::MAX as u64 {
                            self.emit(X8664Instr::ImulRI32 {
                                dest: dest_reg,
                                imm: *imm as i32,
                            });
                            // Schedule store to ensure result is written back to memory
                            self.allocator.schedule_store(dest, dest_reg);
                        } else {
                            // This shouldn't happen for i32 constants
                            return Err(LoweringError::RegisterAllocation(
                                "i32 immediate value out of range".to_string(),
                            ));
                        }
                    }
                    (_, Value::PhysicalReg(_)) => {
                        // Physical register handling for i32 operations - delegate to regular operation
                        return self.lower_binary_op(dest, lhs, rhs, op);
                    }
                    _ => {
                        return Err(LoweringError::RegisterAllocation(
                            "Unsupported 32-bit binary operation".to_string(),
                        ));
                    }
                }

                Ok(())
            }
            BinOp::Div => {
                // Use specialized i32 division with proper sign extension
                self.lower_division_i32(dest, lhs, rhs)
            }
            BinOp::Mod => {
                // Use specialized i32 modulo with proper sign extension
                self.lower_modulo_i32(dest, lhs, rhs)
            }
            _ => {
                // For non-arithmetic operations (comparisons), delegate to regular binary operation
                self.lower_binary_op(dest, lhs, rhs, op)
            }
        }
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

        // Modulo by zero - exit with code EXIT_CODE_DIV_BY_ZERO
        self.emit(X8664Instr::MovRI64 {
            dest: X86Register::Rdi,
            imm: EXIT_CODE_DIV_BY_ZERO,
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

        // Division by zero - exit with code EXIT_CODE_DIV_BY_ZERO
        self.emit(X8664Instr::MovRI64 {
            dest: X86Register::Rdi,
            imm: EXIT_CODE_DIV_BY_ZERO,
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

    /// Lower i32 modulo operation with proper sign extension
    fn lower_modulo_i32(
        &mut self,
        dest: VReg,
        lhs: &Value,
        rhs: &Value,
    ) -> Result<(), LoweringError> {
        // Modulo is like division but we want the remainder from RDX
        let rax = X86Register::Rax;
        let rdx = X86Register::Rdx;

        // Move lhs to RAX with proper sign extension for i32
        self.load_i32_to_rax(lhs)?;

        // Sign extend RAX to RDX:RAX (this handles the sign extension properly)
        self.emit(X8664Instr::Cqo);

        // Get divisor in a register with proper sign extension for i32
        let divisor_reg = self.load_i32_to_register(rhs, &[rax, rdx])?;

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

        // Modulo by zero - exit with code EXIT_CODE_DIV_BY_ZERO
        self.emit(X8664Instr::MovRI64 {
            dest: X86Register::Rdi,
            imm: EXIT_CODE_DIV_BY_ZERO,
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

    /// Lower i32 division operation with proper sign extension
    fn lower_division_i32(
        &mut self,
        dest: VReg,
        lhs: &Value,
        rhs: &Value,
    ) -> Result<(), LoweringError> {
        // Division requires dividend in RAX
        let rax = X86Register::Rax;
        let rdx = X86Register::Rdx;

        // Move lhs to RAX with proper sign extension for i32
        self.load_i32_to_rax(lhs)?;

        // Sign extend RAX to RDX:RAX (this handles the sign extension properly)
        self.emit(X8664Instr::Cqo);

        // Get divisor in a register with proper sign extension for i32
        let divisor_reg = self.load_i32_to_register(rhs, &[rax, rdx])?;

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

        // Division by zero - exit with code EXIT_CODE_DIV_BY_ZERO
        self.emit(X8664Instr::MovRI64 {
            dest: X86Register::Rdi,
            imm: EXIT_CODE_DIV_BY_ZERO,
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

        // Move quotient from RAX to dest (this is the key difference from modulo)
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

    /// Load i32 value to RAX with proper sign extension
    fn load_i32_to_rax(&mut self, value: &Value) -> Result<(), LoweringError> {
        match value {
            Value::VReg(vreg) => {
                let value_reg = self.allocator.ensure_reg(*vreg, &[])?;
                self.emit_spill_reload_ops();
                if value_reg != X86Register::Rax {
                    // For i32 values loaded from memory, we need to ensure proper sign extension
                    // Use MOVSXD to sign-extend from 32-bit to 64-bit
                    self.emit(X8664Instr::Movsxd {
                        dest: X86Register::Rax,
                        src: value_reg,
                    });
                }
            }
            Value::SignedImm(imm) => {
                // For signed immediates, MovRI32 already sign-extends properly
                if *imm >= i32::MIN as i64 && *imm <= i32::MAX as i64 {
                    self.emit(X8664Instr::MovRI32 {
                        dest: X86Register::Rax,
                        imm: *imm as i32,
                    });
                } else {
                    self.emit(X8664Instr::MovRI64 {
                        dest: X86Register::Rax,
                        imm: *imm,
                    });
                }
            }
            Value::UnsignedImm(imm) => {
                // For unsigned immediates that fit in i32 range, use MovRI32
                if *imm <= i32::MAX as u64 {
                    self.emit(X8664Instr::MovRI32 {
                        dest: X86Register::Rax,
                        imm: *imm as i32,
                    });
                } else {
                    self.emit(X8664Instr::MovRI64 {
                        dest: X86Register::Rax,
                        imm: *imm as i64,
                    });
                }
            }
            Value::PhysicalReg(_) => {
                return Err(LoweringError::UnsupportedValueType("i32 division"));
            }
        }
        Ok(())
    }

    /// Load i32 value to a register with proper sign extension
    fn load_i32_to_register(
        &mut self,
        value: &Value,
        avoid: &[X86Register],
    ) -> Result<X86Register, LoweringError> {
        match value {
            Value::VReg(vreg) => {
                let value_reg = self.allocator.ensure_reg(*vreg, avoid)?;
                self.emit_spill_reload_ops();
                // For VReg values, we need to create a new register and sign-extend into it
                let scratch = self.get_scratch_register(avoid)?;
                self.emit(X8664Instr::Movsxd {
                    dest: scratch,
                    src: value_reg,
                });
                Ok(scratch)
            }
            Value::SignedImm(imm) => {
                let scratch = self.get_scratch_register(avoid)?;
                // For signed immediates, MovRI32 already sign-extends properly
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
                Ok(scratch)
            }
            Value::UnsignedImm(imm) => {
                let scratch = self.get_scratch_register(avoid)?;
                // For unsigned immediates that fit in i32 range, use MovRI32
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
                Ok(scratch)
            }
            Value::PhysicalReg(_) => Err(LoweringError::UnsupportedValueType("i32 division")),
        }
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
        return_type: Option<&rue_ir::types::RueType>,
        return_size: Option<&i64>,
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

        // Check if this is a user function that returns an aggregate
        let returns_aggregate = dest.is_some()
            && matches!(
                return_type,
                Some(rue_ir::types::RueType::Struct(_))
                    | Some(rue_ir::types::RueType::Tuple(_))
                    | Some(rue_ir::types::RueType::Array(_, _))
            );

        if is_user_function {
            // CRITICAL: Force ALL VRegs in caller-saved registers to be spilled to memory
            // This is the safest approach for recursive function calls
            // EXCEPTION: For aggregate returns, preserve RAX/RDX until after return value extraction
            for &reg in &caller_saved_regs {
                if self.allocator.is_register_allocated(reg) {
                    // Skip RAX and RDX if this function returns an aggregate
                    // We need them to extract the return value
                    if returns_aggregate && (reg == X86Register::Rax || reg == X86Register::Rdx) {
                        continue;
                    }
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

        // Handle return value preservation
        let (rax_temp_reg, rdx_temp_reg) = if dest.is_some() {
            if !is_user_function && regs_to_save.contains(&X86Register::Rax) {
                // Runtime function: RAX will be overwritten when we restore, so save to temporary register first
                // IMPORTANT: Include all regs_to_save and the argument registers in forbidden list
                // to ensure the scratch register doesn't conflict with any register we're about to restore
                let mut forbidden = regs_to_save.clone();
                forbidden.extend(&arg_regs[..args.len()]);
                let temp_reg = self.get_scratch_register(&forbidden)?;
                self.emit(X8664Instr::MovRR {
                    dest: temp_reg,
                    src: X86Register::Rax,
                });
                (Some(temp_reg), None)
            } else if is_user_function && returns_aggregate {
                // User function returning aggregate: RAX/RDX contain return data that will be overwritten
                // by subsequent function calls (like __rue_alloc), so save to temporary registers
                let mut forbidden = vec![];

                // Save RAX
                let rax_temp = self.get_scratch_register(&forbidden)?;
                forbidden.push(rax_temp);
                self.emit(X8664Instr::MovRR {
                    dest: rax_temp,
                    src: X86Register::Rax,
                });
                debug!(target: "rue::codegen", "USER FUNC: Saved RAX to temp register {:?}", rax_temp);

                // For 16-byte aggregates, also save RDX
                let rdx_temp = if let Some(size) = return_size {
                    if *size > 8 {
                        let rdx_temp = self.get_scratch_register(&forbidden)?;
                        self.emit(X8664Instr::MovRR {
                            dest: rdx_temp,
                            src: X86Register::Rdx,
                        });
                        debug!(target: "rue::codegen", "USER FUNC: Saved RDX to temp register {:?}", rdx_temp);
                        Some(rdx_temp)
                    } else {
                        None
                    }
                } else {
                    None
                };

                (Some(rax_temp), rdx_temp)
            } else {
                (None, None)
            }
        } else {
            (None, None)
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

        // Handle return value based on its type
        if let Some(dest_vreg) = dest {
            match return_type {
                Some(rue_ir::types::RueType::Struct(_))
                | Some(rue_ir::types::RueType::Tuple(_))
                | Some(rue_ir::types::RueType::Array(_, _)) => {
                    // Aggregate return - need special handling
                    let computed_size = return_size.copied(); // Use computed size if available
                    self.handle_aggregate_return(
                        *dest_vreg,
                        return_type.unwrap(),
                        rax_temp_reg,
                        rdx_temp_reg,
                        computed_size,
                    )?;

                    // For user functions returning aggregates, now invalidate RAX/RDX since we've extracted the return value
                    if is_user_function && returns_aggregate {
                        if self.allocator.is_register_allocated(X86Register::Rax) {
                            self.allocator.invalidate_register(X86Register::Rax);
                        }
                        if self.allocator.is_register_allocated(X86Register::Rdx) {
                            self.allocator.invalidate_register(X86Register::Rdx);
                        }
                        self.emit_spill_reload_ops();
                    }
                }
                _ => {
                    // Scalar return - existing behavior
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
                    // Note: assign_reg_for_def already marks the register as dirty
                }
            }
        }

        // CRITICAL FIX: For user functions, invalidate caller-saved registers AGAIN after the call
        // This handles the case where VRegs were loaded into caller-saved registers for argument
        // passing, but then those registers got clobbered by the function call
        if is_user_function {
            for &reg in &caller_saved_regs {
                if self.allocator.is_register_allocated(reg) {
                    self.allocator.invalidate_register(reg);
                }
            }
            // Emit any spill operations generated by the invalidation
            self.emit_spill_reload_ops();
        }

        Ok(())
    }

    /// Handle aggregate return values from function calls
    fn handle_aggregate_return(
        &mut self,
        dest_vreg: VReg,
        return_type: &rue_ir::types::RueType,
        rax_temp_reg: Option<X86Register>,
        rdx_temp_reg: Option<X86Register>,
        computed_size: Option<i64>, // Use computed size if available
    ) -> Result<(), LoweringError> {
        use rue_ir::types::RueType;

        // Use computed size if available, otherwise fall back to heuristic
        let size = if let Some(computed) = computed_size {
            computed
        } else {
            // Fallback heuristic if computed size not available
            match return_type {
                RueType::Struct(_struct_id) => {
                    16 // Heuristic fallback: assume 2 x i64 fields = 16 bytes
                }
                RueType::Tuple(fields) => {
                    // Compute tuple size based on field types
                    fields
                        .iter()
                        .map(|field| {
                            match field {
                                RueType::I64 => 8,
                                RueType::I32 => 4,
                                RueType::Bool => 1,
                                _ => 8, // Default fallback
                            }
                        })
                        .sum::<i64>()
                }
                RueType::Array(elem_type, count) => {
                    let elem_size = match **elem_type {
                        RueType::I64 => 8,
                        RueType::I32 => 4,
                        RueType::Bool => 1,
                        _ => 8, // Default fallback
                    };
                    elem_size * (*count as i64)
                }
                _ => return Err(LoweringError::UnsupportedValueType("aggregate return")),
            }
        };

        debug!(
            target: "rue::codegen",
            ?dest_vreg,
            ?return_type,
            size,
            "Handling aggregate return"
        );

        if size <= 16 {
            // Small aggregate: returned in RAX/RDX registers
            self.handle_small_aggregate_return(dest_vreg, size, rax_temp_reg, rdx_temp_reg)
        } else {
            // Large aggregate: should use hidden pointer mechanism
            // For now, this is not fully implemented
            warn!(
                target: "rue::codegen",
                size,
                "Large aggregate return not fully implemented"
            );
            Err(LoweringError::InvalidAggregateSize(size))
        }
    }

    /// Handle small aggregate return (≤16 bytes) from RAX/RDX registers  
    fn handle_small_aggregate_return(
        &mut self,
        dest_vreg: VReg,
        size: i64,
        rax_temp_reg: Option<X86Register>,
        rdx_temp_reg: Option<X86Register>,
    ) -> Result<(), LoweringError> {
        debug!(
            target: "rue::codegen",
            ?dest_vreg,
            size,
            ?rax_temp_reg,
            ?rdx_temp_reg,
            "Handling small aggregate return"
        );

        // Get the allocated aggregate pointer FIRST
        // Make sure we don't overwrite the temp registers containing the saved RAX/RDX data
        let mut forbidden = vec![];
        if let Some(temp_reg) = rax_temp_reg {
            forbidden.push(temp_reg);
        }
        if let Some(temp_reg) = rdx_temp_reg {
            forbidden.push(temp_reg);
        }
        // For aggregates > 8 bytes, also avoid RDX if it's still live (not saved to temp)
        if size > 8 && rdx_temp_reg.is_none() {
            forbidden.push(X86Register::Rdx);
        }
        let agg_reg = self.allocator.assign_reg_for_def(dest_vreg, &forbidden)?;
        self.emit_spill_reload_ops();

        // Now allocate space for the aggregate, with the register already assigned
        self.lower_allocate_aggregate_with_forbidden(dest_vreg, size, 8, &forbidden)?;

        debug!(
            target: "rue::codegen",
            ?agg_reg,
            "CALLER: Allocated memory for aggregate at register"
        );

        // Copy data from return registers to the allocated space
        let rax_source = if let Some(temp_reg) = rax_temp_reg {
            debug!(target: "rue::codegen", "CALLER: RAX was saved to temp register {:?}", temp_reg);
            temp_reg // We saved RAX to this temp register
        } else {
            debug!(target: "rue::codegen", "CALLER: Using RAX directly (not saved)");
            X86Register::Rax // Still in RAX
        };

        if size <= 8 {
            // Fits entirely in RAX - copy 8 bytes
            debug!(target: "rue::codegen", "CALLER: Storing 8 bytes from {:?} -> memory[{:?}+0]", rax_source, agg_reg);
            self.emit(X8664Instr::MovMR {
                base: agg_reg,
                offset: 0,
                src: rax_source,
            });
        } else if size <= 16 {
            // Need both RAX and RDX - copy 16 bytes
            debug!(target: "rue::codegen", "CALLER: Storing 8 bytes from {:?} -> memory[{:?}+0]", rax_source, agg_reg);
            self.emit(X8664Instr::MovMR {
                base: agg_reg,
                offset: 0,
                src: rax_source,
            });

            let rdx_source = if let Some(temp_reg) = rdx_temp_reg {
                debug!(target: "rue::codegen", "CALLER: RDX was saved to temp register {:?}", temp_reg);
                temp_reg // We saved RDX to this temp register
            } else {
                debug!(target: "rue::codegen", "CALLER: Using RDX directly (not saved)");
                X86Register::Rdx // Still in RDX
            };

            debug!(target: "rue::codegen", "CALLER: Storing 8 bytes from {:?} -> memory[{:?}+8]", rdx_source, agg_reg);
            self.emit(X8664Instr::MovMR {
                base: agg_reg,
                offset: 8,
                src: rdx_source,
            });
        }

        debug!(target: "rue::codegen", "CALLER: Aggregate return data copied to memory");

        // The dest_vreg now points to the aggregate in memory
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

            // Check if this VReg represents an aggregate pointer
            if let Some(&size) = self.aggregate_vregs.get(vreg) {
                debug!(
                    target: "rue::codegen",
                    ?vreg,
                    size,
                    "Detected aggregate return, delegating to lower_return_small_aggregate"
                );
                return self.lower_return_small_aggregate(*vreg, size);
            }

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

    fn lower_return_small_aggregate(
        &mut self,
        value: VReg,
        size: i64,
    ) -> Result<(), LoweringError> {
        debug!(
            target: "rue::codegen",
            ?value,
            size,
            "Lowering small aggregate return"
        );

        // Flush all dirty registers first
        self.allocator.flush_stores();
        self.emit_spill_reload_ops();

        // Get the aggregate pointer
        let agg_reg = self.allocator.ensure_reg(value, &[])?;
        self.emit_spill_reload_ops();

        debug!(
            target: "rue::codegen",
            ?agg_reg,
            "CALLEE: Got aggregate pointer in register"
        );

        // For small aggregates (≤16 bytes), copy data into RAX and potentially RDX
        // according to System V ABI
        if size <= 8 {
            // Fits entirely in RAX - LOAD from memory into RAX
            debug!(target: "rue::codegen", "CALLEE: Loading 8 bytes from memory[{:?}+0] -> RAX", agg_reg);
            self.emit(X8664Instr::MovRM {
                dest: X86Register::Rax,
                base: agg_reg,
                offset: 0,
            });
        } else if size <= 16 {
            // Need both RAX and RDX - LOAD from memory into registers
            debug!(target: "rue::codegen", "CALLEE: Loading 8 bytes from memory[{:?}+0] -> RAX", agg_reg);
            self.emit(X8664Instr::MovRM {
                dest: X86Register::Rax,
                base: agg_reg,
                offset: 0,
            });
            debug!(target: "rue::codegen", "CALLEE: Loading 8 bytes from memory[{:?}+8] -> RDX", agg_reg);
            self.emit(X8664Instr::MovRM {
                dest: X86Register::Rdx,
                base: agg_reg,
                offset: 8,
            });
        } else {
            return Err(LoweringError::InvalidAggregateSize(size));
        }

        debug!(target: "rue::codegen", "CALLEE: Aggregate data loaded into return registers RAX/RDX, returning");

        // Standard SysV epilogue
        self.emit(X8664Instr::LeaveFrame);
        self.emit(X8664Instr::Ret);

        Ok(())
    }

    fn lower_return_large_aggregate(
        &mut self,
        value: VReg,
        size: i64,
    ) -> Result<(), LoweringError> {
        debug!(
            target: "rue::codegen",
            ?value,
            size,
            "Lowering large aggregate return"
        );

        // For large aggregates (>16 bytes), we should use the hidden return pointer
        // mechanism, but this requires changes to the calling convention.
        // For now, let's implement a simple memcpy-based approach that copies
        // the aggregate to a caller-provided location.

        // TODO: Implement proper hidden return pointer mechanism
        // For now, fall back to treating it as a pointer return
        warn!(
            target: "rue::codegen",
            size,
            "Large aggregate return not fully implemented, falling back to pointer return"
        );

        // Flush all dirty registers first
        self.allocator.flush_stores();
        self.emit_spill_reload_ops();

        // Get the aggregate pointer and return it in RAX
        let agg_reg = self.allocator.ensure_reg(value, &[])?;
        self.emit_spill_reload_ops();

        if agg_reg != X86Register::Rax {
            self.emit(X8664Instr::MovRR {
                dest: X86Register::Rax,
                src: agg_reg,
            });
        }

        // Standard SysV epilogue
        self.emit(X8664Instr::LeaveFrame);
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

    /// Lower AllocateAggregate to x86-64 instructions
    fn lower_allocate_aggregate(
        &mut self,
        dest: VReg,
        size: i64,
        _alignment: i64,
    ) -> Result<(), LoweringError> {
        self.lower_allocate_aggregate_with_forbidden(dest, size, _alignment, &[])
    }

    /// Lower AllocateAggregate to x86-64 instructions with forbidden registers
    fn lower_allocate_aggregate_with_forbidden(
        &mut self,
        dest: VReg,
        size: i64,
        _alignment: i64,
        forbidden: &[X86Register],
    ) -> Result<(), LoweringError> {
        // Call __rue_alloc(size) to allocate memory on the heap
        let dest_reg = self.allocator.ensure_reg(dest, forbidden)?;
        self.emit_spill_reload_ops();

        // Set up function call to __rue_alloc
        // Size argument goes in RDI (first parameter register)
        self.emit(X8664Instr::MovRI64 {
            dest: X86Register::Rdi,
            imm: size,
        });

        // Call the allocator function
        self.emit(X8664Instr::Call {
            target: "__rue_alloc".to_string(),
        });

        // Move result from RAX to destination register if different
        if dest_reg != X86Register::Rax {
            self.emit(X8664Instr::MovRR {
                dest: dest_reg,
                src: X86Register::Rax,
            });
        }

        // CRITICAL FIX: Mark the destination as dirty after allocating
        // This ensures the struct pointer gets written to memory and marked as initialized
        self.allocator.schedule_store(dest, dest_reg);

        // Track this VReg as representing an aggregate pointer
        self.aggregate_vregs.insert(dest, size);

        Ok(())
    }

    /// Lower CopyAggregate to x86-64 instructions using runtime helper
    fn lower_copy_aggregate(
        &mut self,
        dest: VReg,
        src: VReg,
        size: i64,
    ) -> Result<(), LoweringError> {
        let dest_reg = self.allocator.ensure_reg(dest, &[])?;
        let src_reg = self.allocator.ensure_reg(src, &[dest_reg])?;
        self.emit_spill_reload_ops();

        // Set up call to __rue_memcpy(dest, src, size)
        // dest -> RDI, src -> RSI, size -> RDX
        if dest_reg != X86Register::Rdi {
            self.emit(X8664Instr::MovRR {
                dest: X86Register::Rdi,
                src: dest_reg,
            });
        }
        if src_reg != X86Register::Rsi {
            self.emit(X8664Instr::MovRR {
                dest: X86Register::Rsi,
                src: src_reg,
            });
        }
        self.emit(X8664Instr::MovRI64 {
            dest: X86Register::Rdx,
            imm: size,
        });

        // Call memory copy function
        self.emit(X8664Instr::Call {
            target: "__rue_memcpy".to_string(),
        });

        Ok(())
    }

    /// Lower ZeroAggregate to x86-64 instructions using runtime helper
    fn lower_zero_aggregate(&mut self, dest: VReg, size: i64) -> Result<(), LoweringError> {
        let dest_reg = self.allocator.ensure_reg(dest, &[])?;
        self.emit_spill_reload_ops();

        // Set up call to __rue_memzero(dest, size)
        // dest -> RDI, size -> RSI
        if dest_reg != X86Register::Rdi {
            self.emit(X8664Instr::MovRR {
                dest: X86Register::Rdi,
                src: dest_reg,
            });
        }
        self.emit(X8664Instr::MovRI64 {
            dest: X86Register::Rsi,
            imm: size,
        });

        // Call memory zero function
        self.emit(X8664Instr::Call {
            target: "__rue_memzero".to_string(),
        });

        Ok(())
    }

    /// Lower InlineCopyAggregate to x86-64 instructions using direct register moves
    fn lower_inline_copy_aggregate(
        &mut self,
        dest: VReg,
        src: VReg,
        size: i64,
    ) -> Result<(), LoweringError> {
        let dest_reg = self.allocator.ensure_reg(dest, &[])?;
        let src_reg = self.allocator.ensure_reg(src, &[dest_reg])?;
        self.emit_spill_reload_ops();

        // For small sizes, use direct register-to-register moves
        match size {
            1 => {
                // Copy 1 byte
                self.emit(X8664Instr::MovRM8 {
                    dest: X86Register::Rax,
                    base: src_reg,
                    offset: 0,
                });
                self.emit(X8664Instr::MovMR8 {
                    base: dest_reg,
                    offset: 0,
                    src: X86Register::Rax,
                });
            }
            2..=7 => {
                // Copy 2-7 bytes using byte-by-byte copying
                for i in 0..size {
                    self.emit(X8664Instr::MovRM8 {
                        dest: X86Register::Rax,
                        base: src_reg,
                        offset: i as i32,
                    });
                    self.emit(X8664Instr::MovMR8 {
                        base: dest_reg,
                        offset: i as i32,
                        src: X86Register::Rax,
                    });
                }
            }
            8 => {
                // Copy 8 bytes using 64-bit move
                self.emit(X8664Instr::MovRM {
                    dest: X86Register::Rax,
                    base: src_reg,
                    offset: 0,
                });
                self.emit(X8664Instr::MovMR {
                    base: dest_reg,
                    offset: 0,
                    src: X86Register::Rax,
                });
            }
            9..=15 => {
                // Copy 8 bytes + remaining bytes
                self.emit(X8664Instr::MovRM {
                    dest: X86Register::Rax,
                    base: src_reg,
                    offset: 0,
                });
                self.emit(X8664Instr::MovMR {
                    base: dest_reg,
                    offset: 0,
                    src: X86Register::Rax,
                });
                // Copy remaining bytes one by one
                for i in 8..size {
                    self.emit(X8664Instr::MovRM8 {
                        dest: X86Register::Rax,
                        base: src_reg,
                        offset: i as i32,
                    });
                    self.emit(X8664Instr::MovMR8 {
                        base: dest_reg,
                        offset: i as i32,
                        src: X86Register::Rax,
                    });
                }
            }
            16 => {
                // Copy 16 bytes using two 64-bit moves
                self.emit(X8664Instr::MovRM {
                    dest: X86Register::Rax,
                    base: src_reg,
                    offset: 0,
                });
                self.emit(X8664Instr::MovMR {
                    base: dest_reg,
                    offset: 0,
                    src: X86Register::Rax,
                });
                self.emit(X8664Instr::MovRM {
                    dest: X86Register::Rax,
                    base: src_reg,
                    offset: 8,
                });
                self.emit(X8664Instr::MovMR {
                    base: dest_reg,
                    offset: 8,
                    src: X86Register::Rax,
                });
            }
            _ => {
                // Should not happen - inline copy is only for sizes ≤16
                return Err(LoweringError::RegisterAllocation(format!(
                    "Inline copy called with size {size} > 16"
                )));
            }
        }

        Ok(())
    }

    /// Lower InlineZeroAggregate to x86-64 instructions using direct register moves
    fn lower_inline_zero_aggregate(&mut self, dest: VReg, size: i64) -> Result<(), LoweringError> {
        let dest_reg = self.allocator.ensure_reg(dest, &[])?;
        self.emit_spill_reload_ops();

        // Zero out RAX once for all writes
        self.emit(X8664Instr::XorRR {
            dest: X86Register::Rax,
            src: X86Register::Rax,
        });

        // For small sizes, use direct register writes
        match size {
            1 => {
                // Zero 1 byte
                self.emit(X8664Instr::MovMR8 {
                    base: dest_reg,
                    offset: 0,
                    src: X86Register::Rax,
                });
            }
            2..=7 => {
                // Zero 2-7 bytes using byte-by-byte zeroing
                for i in 0..size {
                    self.emit(X8664Instr::MovMR8 {
                        base: dest_reg,
                        offset: i as i32,
                        src: X86Register::Rax,
                    });
                }
            }
            8 => {
                // Zero 8 bytes using 64-bit move
                self.emit(X8664Instr::MovMR {
                    base: dest_reg,
                    offset: 0,
                    src: X86Register::Rax,
                });
            }
            _ => {
                // Should not happen - inline zero is only for sizes ≤8
                return Err(LoweringError::RegisterAllocation(format!(
                    "Inline zero called with size {size} > 8"
                )));
            }
        }

        Ok(())
    }

    /// Lower LoadField to x86-64 instructions
    fn lower_load_field(
        &mut self,
        dest: VReg,
        base: VReg,
        offset: i64,
        field_type: &rue_ir::types::RueType,
    ) -> Result<(), LoweringError> {
        // STRUCT FIELD ACCESS FIX: Ensure register state is stable before field access
        // This prevents register corruption during struct field loading
        self.allocator.flush_stores();
        self.emit_spill_reload_ops();

        let dest_reg = self.allocator.ensure_reg(dest, &[])?;
        let base_reg = self.allocator.ensure_reg(base, &[dest_reg])?;
        self.emit_spill_reload_ops();

        // Use appropriate instruction based on field type
        match field_type {
            rue_ir::types::RueType::I32 => {
                // 32-bit load with zero-extension to 64-bit
                self.emit(X8664Instr::MovRM32 {
                    dest: dest_reg,
                    base: base_reg,
                    offset: offset as i32,
                });
            }
            rue_ir::types::RueType::Bool => {
                // 8-bit load with zero-extension to 64-bit
                self.emit(X8664Instr::MovRM8 {
                    dest: dest_reg,
                    base: base_reg,
                    offset: offset as i32,
                });
            }
            _ => {
                // Default to 64-bit load for other types (i64, etc.)
                self.emit(X8664Instr::MovRM {
                    dest: dest_reg,
                    base: base_reg,
                    offset: offset as i32,
                });
            }
        }

        // CRITICAL FIX: Mark the destination as dirty after loading
        // This ensures the loaded field value gets tracked by the register allocator
        self.allocator.schedule_store(dest, dest_reg);

        Ok(())
    }

    /// Lower StoreField to x86-64 instructions
    fn lower_store_field(
        &mut self,
        base: VReg,
        offset: i64,
        src: &Value,
        field_type: &rue_ir::types::RueType,
    ) -> Result<(), LoweringError> {
        let base_reg = self.allocator.ensure_reg(base, &[])?;

        match src {
            Value::VReg(src_vreg) => {
                let src_reg = self.allocator.ensure_reg(*src_vreg, &[base_reg])?;
                self.emit_spill_reload_ops();

                // Use appropriate instruction based on field type
                match field_type {
                    rue_ir::types::RueType::I32 => {
                        // 32-bit store
                        self.emit(X8664Instr::MovMR32 {
                            base: base_reg,
                            offset: offset as i32,
                            src: src_reg,
                        });
                    }
                    rue_ir::types::RueType::Bool => {
                        // 8-bit store for bool
                        self.emit(X8664Instr::MovMR8 {
                            base: base_reg,
                            offset: offset as i32,
                            src: src_reg,
                        });
                    }
                    _ => {
                        // Default to 64-bit store for other types (i64, etc.)
                        self.emit(X8664Instr::MovMR {
                            base: base_reg,
                            offset: offset as i32,
                            src: src_reg,
                        });
                    }
                }
            }
            Value::SignedImm(imm) => {
                self.emit_spill_reload_ops();

                // Load immediate into a register, then store to memory
                let scratch = self.get_scratch_register(&[base_reg])?;

                match field_type {
                    rue_ir::types::RueType::I32 => {
                        // For 32-bit fields, use 32-bit immediate if possible
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
                        self.emit(X8664Instr::MovMR32 {
                            base: base_reg,
                            offset: offset as i32,
                            src: scratch,
                        });
                    }
                    rue_ir::types::RueType::Bool => {
                        // For bool fields, store as 8-bit
                        self.emit(X8664Instr::MovRI32 {
                            dest: scratch,
                            imm: *imm as i32,
                        });
                        self.emit(X8664Instr::MovMR8 {
                            base: base_reg,
                            offset: offset as i32,
                            src: scratch,
                        });
                    }
                    _ => {
                        self.emit(X8664Instr::MovRI64 {
                            dest: scratch,
                            imm: *imm,
                        });
                        self.emit(X8664Instr::MovMR {
                            base: base_reg,
                            offset: offset as i32,
                            src: scratch,
                        });
                    }
                }
            }
            Value::UnsignedImm(imm) => {
                self.emit_spill_reload_ops();

                // Load immediate into a register, then store to memory
                let scratch = self.get_scratch_register(&[base_reg])?;

                match field_type {
                    rue_ir::types::RueType::I32 => {
                        // For 32-bit fields, use 32-bit immediate if possible
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
                        self.emit(X8664Instr::MovMR32 {
                            base: base_reg,
                            offset: offset as i32,
                            src: scratch,
                        });
                    }
                    rue_ir::types::RueType::Bool => {
                        // For bool fields, store as 8-bit
                        self.emit(X8664Instr::MovRI32 {
                            dest: scratch,
                            imm: *imm as i32,
                        });
                        self.emit(X8664Instr::MovMR8 {
                            base: base_reg,
                            offset: offset as i32,
                            src: scratch,
                        });
                    }
                    _ => {
                        self.emit(X8664Instr::MovRI64 {
                            dest: scratch,
                            imm: *imm as i64,
                        });
                        self.emit(X8664Instr::MovMR {
                            base: base_reg,
                            offset: offset as i32,
                            src: scratch,
                        });
                    }
                }
            }
            Value::PhysicalReg(_) => {
                return Err(LoweringError::UnsupportedValueType("StoreField"));
            }
        }

        Ok(())
    }

    /// Lower ArrayBoundsCheck to x86-64 instructions
    fn lower_array_bounds_check(
        &mut self,
        _array_base: VReg,
        index: VReg,
        array_len: u64,
        trap_label: Label,
    ) -> Result<(), LoweringError> {
        let index_reg = self.allocator.ensure_reg(index, &[])?;
        self.emit_spill_reload_ops();

        // Compare index with array length (unsigned comparison)
        self.emit(X8664Instr::CmpRI {
            reg: index_reg,
            imm: array_len as i32,
        });

        // Jump to trap if index >= array_len (unsigned greater or equal)
        let trap_machine_id = self.get_or_create_label(trap_label);
        self.emit(X8664Instr::JmpCC {
            cc: ConditionCode::AboveEqual, // Jump if Above or Equal (unsigned >= )
            target: LabelRef::Local(trap_machine_id),
        });

        Ok(())
    }

    /// Lower DynamicLoadField to x86-64 instructions
    fn lower_dynamic_load_field(
        &mut self,
        dest: VReg,
        base: VReg,
        index: VReg,
        element_size: i64,
        _element_type: &rue_ir::types::RueType,
    ) -> Result<(), LoweringError> {
        let base_reg = self.allocator.ensure_reg(base, &[])?;
        let index_reg = self.allocator.ensure_reg(index, &[base_reg])?;
        let dest_reg = self.allocator.ensure_reg(dest, &[base_reg, index_reg])?;
        self.emit_spill_reload_ops();

        // Calculate offset: index * element_size
        // Use a scratch register for the computation
        let scratch = self.get_scratch_register(&[base_reg, index_reg, dest_reg])?;

        // Move index to scratch register
        self.emit(X8664Instr::MovRR {
            dest: scratch,
            src: index_reg,
        });

        // Multiply by element size (imul scratch, element_size)
        self.emit(X8664Instr::ImulRI {
            dest: scratch,
            imm: element_size as i32,
        });

        // Calculate final address: base + scratch
        self.emit(X8664Instr::AddRR {
            dest: scratch,
            src: base_reg,
        });

        // Load from [scratch] into dest
        self.emit(X8664Instr::MovRM {
            dest: dest_reg,
            base: scratch,
            offset: 0,
        });

        // Schedule store for the destination register
        self.allocator.schedule_store(dest, dest_reg);

        Ok(())
    }

    /// Lower DynamicStoreField to x86-64 instructions
    fn lower_dynamic_store_field(
        &mut self,
        base: VReg,
        index: VReg,
        src: &Value,
        element_size: i64,
        _element_type: &rue_ir::types::RueType,
    ) -> Result<(), LoweringError> {
        let base_reg = self.allocator.ensure_reg(base, &[])?;
        let index_reg = self.allocator.ensure_reg(index, &[base_reg])?;

        match src {
            Value::VReg(src_vreg) => {
                let src_reg = self
                    .allocator
                    .ensure_reg(*src_vreg, &[base_reg, index_reg])?;
                self.emit_spill_reload_ops();

                // Calculate offset: index * element_size
                let scratch = self.get_scratch_register(&[base_reg, index_reg, src_reg])?;

                // Move index to scratch register
                self.emit(X8664Instr::MovRR {
                    dest: scratch,
                    src: index_reg,
                });

                // Multiply by element size
                self.emit(X8664Instr::ImulRI {
                    dest: scratch,
                    imm: element_size as i32,
                });

                // Calculate final address: base + scratch
                self.emit(X8664Instr::AddRR {
                    dest: scratch,
                    src: base_reg,
                });

                // Store src to [scratch]
                self.emit(X8664Instr::MovMR {
                    base: scratch,
                    offset: 0,
                    src: src_reg,
                });
            }
            Value::SignedImm(imm) => {
                self.emit_spill_reload_ops();

                // Calculate offset: index * element_size
                let scratch = self.get_scratch_register(&[base_reg, index_reg])?;
                let value_scratch = self.get_scratch_register(&[base_reg, index_reg, scratch])?;

                // Load immediate into value scratch register
                self.emit(X8664Instr::MovRI64 {
                    dest: value_scratch,
                    imm: *imm,
                });

                // Calculate offset
                self.emit(X8664Instr::MovRR {
                    dest: scratch,
                    src: index_reg,
                });
                self.emit(X8664Instr::ImulRI {
                    dest: scratch,
                    imm: element_size as i32,
                });

                // Calculate final address: base + scratch
                self.emit(X8664Instr::AddRR {
                    dest: scratch,
                    src: base_reg,
                });

                // Store value to [scratch]
                self.emit(X8664Instr::MovMR {
                    base: scratch,
                    offset: 0,
                    src: value_scratch,
                });
            }
            Value::UnsignedImm(imm) => {
                self.emit_spill_reload_ops();

                // Similar to SignedImm but cast the immediate
                let scratch = self.get_scratch_register(&[base_reg, index_reg])?;
                let value_scratch = self.get_scratch_register(&[base_reg, index_reg, scratch])?;

                self.emit(X8664Instr::MovRI64 {
                    dest: value_scratch,
                    imm: *imm as i64,
                });

                // Calculate offset
                self.emit(X8664Instr::MovRR {
                    dest: scratch,
                    src: index_reg,
                });
                self.emit(X8664Instr::ImulRI {
                    dest: scratch,
                    imm: element_size as i32,
                });

                // Calculate final address: base + scratch
                self.emit(X8664Instr::AddRR {
                    dest: scratch,
                    src: base_reg,
                });

                // Store value to [scratch]
                self.emit(X8664Instr::MovMR {
                    base: scratch,
                    offset: 0,
                    src: value_scratch,
                });
            }
            Value::PhysicalReg(_) => {
                return Err(LoweringError::UnsupportedValueType("DynamicStoreField"));
            }
        }

        Ok(())
    }

    /// Lower Trap to x86-64 instructions
    fn lower_trap(&mut self, message: &str) -> Result<(), LoweringError> {
        // For now, we'll implement trap as a system exit with a specific error code
        // In a more sophisticated implementation, we might print the error message
        // and/or generate debugging information

        // CRITICAL: Flush all register state before trap since we're terminating
        // This ensures any pending spill operations are completed and register
        // state is consistent before the system call
        self.allocator.flush_stores();
        self.emit_spill_reload_ops();

        // Load exit code for array bounds violation into rax (system call number)
        self.emit(X8664Instr::MovRI32 {
            dest: X86Register::Rax,
            imm: 60, // sys_exit
        });

        // Load error code into rdi (first argument)
        self.emit(X8664Instr::MovRI32 {
            dest: X86Register::Rdi,
            imm: EXIT_CODE_BOUNDS_CHECK as i32, // Exit code for bounds violation
        });

        // System call
        self.emit(X8664Instr::Syscall);

        // Mark as unreachable - sys_exit never returns
        self.emit(X8664Instr::Ud2);

        // Add a comment for debugging
        trace!(
            target: "rue::codegen::trap",
            message = %message,
            "Generated trap instruction"
        );

        Ok(())
    }
}
