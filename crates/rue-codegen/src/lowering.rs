use crate::{
    BinOp, Instruction, LabelId, VReg, Value,
    regalloc::{RegisterAllocator, SpillReloadOp},
};
use rue_ir::target::{ConditionCode, LabelRef, MachineInstr, Register};
use std::collections::HashMap;

/// The Lowering pass converts high-level IR instructions with virtual registers
/// into x86-specific machine instructions with concrete registers.
/// It delegates all spilling decisions to the RegisterAllocator.
pub struct Lowering<'a> {
    allocator: &'a mut RegisterAllocator,
    instructions: Vec<MachineInstr>,
    label_map: HashMap<LabelId, u32>,
    next_label_id: u32,
    function_labels: HashMap<String, LabelId>,
    /// External label map to use (if provided)
    external_label_map: Option<HashMap<LabelId, u32>>,
    /// Track number of pushes since function entry for stack alignment
    push_count: u32,
}

impl<'a> Lowering<'a> {
    pub fn new(allocator: &'a mut RegisterAllocator, first_label_id: u32) -> Self {
        Self {
            allocator,
            instructions: Vec::new(),
            label_map: HashMap::new(),
            next_label_id: first_label_id,
            function_labels: HashMap::new(),
            external_label_map: None,
            push_count: 0,
        }
    }

    /// Set an external label map to use for label resolution
    pub fn set_label_map(&mut self, label_map: HashMap<LabelId, u32>) {
        self.external_label_map = Some(label_map);
    }

    pub fn set_function_labels(&mut self, labels: HashMap<String, LabelId>) {
        self.function_labels = labels;
    }

    /// Get the next label ID that would be assigned
    pub fn next_label_id(&self) -> u32 {
        self.next_label_id
    }

    /// Lower a sequence of high-level instructions to machine instructions
    pub fn lower(&mut self, ir_instructions: &[Instruction]) -> Result<Vec<MachineInstr>, String> {
        let start_len = self.instructions.len();
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
                *size = stack_size as u32;
            }
        }
    }

    fn lower_instruction(&mut self, instr: &Instruction) -> Result<(), String> {
        match instr {
            Instruction::Copy { dest, src } => self.lower_copy(*dest, src),
            Instruction::BinaryOp { dest, lhs, rhs, op } => {
                self.lower_binary_op(*dest, lhs, rhs, op)
            }
            Instruction::Load { dest, offset } => self.lower_load(*dest, *offset),
            Instruction::Store { src, offset } => self.lower_store(*src, *offset),
            Instruction::Push { src } => self.lower_push(*src),
            Instruction::Pop { dest } => self.lower_pop(*dest),
            Instruction::Label(_) => {
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

    fn lower_copy(&mut self, dest: VReg, src: &Value) -> Result<(), String> {
        match src {
            Value::Immediate(imm) => {
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
            }
            Value::VReg(src_vreg) => {
                let src_reg = self.allocator.ensure_reg(*src_vreg, &[])?;
                self.emit_spill_reload_ops();
                let dest_reg = self.allocator.ensure_reg(dest, &[src_reg])?;
                self.emit_spill_reload_ops();
                if src_reg != dest_reg {
                    self.emit(MachineInstr::MovRR {
                        dest: dest_reg,
                        src: src_reg,
                    });
                }
            }
            Value::PhysicalReg(reg) => {
                let dest_reg = self.allocator.ensure_reg(dest, &[*reg])?;
                self.emit_spill_reload_ops();
                if *reg != dest_reg {
                    self.emit(MachineInstr::MovRR {
                        dest: dest_reg,
                        src: *reg,
                    });
                }
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
    ) -> Result<(), String> {
        match op {
            BinOp::Add | BinOp::Sub | BinOp::Mul => {
                // Get lhs into dest register first
                let dest_reg = match lhs {
                    Value::VReg(vreg) => {
                        let lhs_reg = self.allocator.ensure_reg(*vreg, &[])?;
                        let dest_reg = self.allocator.ensure_reg(dest, &[lhs_reg])?;
                        // Emit any pending spill/reload operations before moving
                        self.emit_spill_reload_ops();
                        if lhs_reg != dest_reg {
                            self.emit(MachineInstr::MovRR {
                                dest: dest_reg,
                                src: lhs_reg,
                            });
                        }
                        dest_reg
                    }
                    Value::Immediate(imm) => {
                        let dest_reg = self.allocator.ensure_reg(dest, &[])?;
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
                        dest_reg
                    }
                    Value::PhysicalReg(_) => {
                        return Err("PhysicalReg not supported in binary operations".to_string());
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
                    }
                    (BinOp::Add, Value::Immediate(imm)) => {
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
                    }
                    (BinOp::Sub, Value::VReg(rhs_vreg)) => {
                        let rhs_reg = self.allocator.ensure_reg(*rhs_vreg, &[dest_reg])?;
                        self.emit_spill_reload_ops();
                        self.emit(MachineInstr::SubRR {
                            dest: dest_reg,
                            src: rhs_reg,
                        });
                    }
                    (BinOp::Sub, Value::Immediate(imm)) => {
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
                    }
                    (BinOp::Mul, Value::VReg(rhs_vreg)) => {
                        let rhs_reg = self.allocator.ensure_reg(*rhs_vreg, &[dest_reg])?;
                        self.emit_spill_reload_ops();
                        self.emit(MachineInstr::ImulRR {
                            dest: dest_reg,
                            src: rhs_reg,
                        });
                    }
                    (BinOp::Mul, Value::Immediate(imm)) => {
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

    fn lower_modulo(&mut self, dest: VReg, lhs: &Value, rhs: &Value) -> Result<(), String> {
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
            Value::Immediate(imm) => {
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
            Value::PhysicalReg(_) => return Err("PhysicalReg not supported in modulo".to_string()),
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
            Value::Immediate(imm) => {
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
            Value::PhysicalReg(_) => return Err("PhysicalReg not supported in modulo".to_string()),
        };

        // Check for division by zero (modulo by zero)
        self.emit(MachineInstr::CmpRI {
            reg: divisor_reg,
            imm: 0,
        });

        // Jump if not zero
        let mod_ok_label = self.next_label_id();
        self.emit(MachineInstr::JmpCC {
            cc: ConditionCode::NotEqual,
            target: LabelRef::Local(mod_ok_label),
        });

        // Modulo by zero - exit with code 250
        self.emit(MachineInstr::MovRI64 {
            dest: Register::Rdi,
            imm: 250, // Exit code for divide by zero
        });
        self.emit(MachineInstr::MovRI64 {
            dest: Register::Rax,
            imm: 60, // sys_exit
        });
        self.emit(MachineInstr::Syscall);

        // Continue with division
        self.emit(MachineInstr::Label { id: mod_ok_label });

        // Perform division
        self.emit(MachineInstr::Idiv {
            divisor: divisor_reg,
        });

        // Move remainder from RDX to dest (this is the key difference from division)
        let dest_reg = self.allocator.ensure_reg(dest, &[])?;
        self.emit_spill_reload_ops();
        if dest_reg != rdx {
            self.emit(MachineInstr::MovRR {
                dest: dest_reg,
                src: rdx,
            });
        }

        Ok(())
    }

    fn lower_division(&mut self, dest: VReg, lhs: &Value, rhs: &Value) -> Result<(), String> {
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
            Value::Immediate(imm) => {
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
            Value::PhysicalReg(_) => {
                return Err("PhysicalReg not supported in division".to_string());
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
            Value::Immediate(imm) => {
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
            Value::PhysicalReg(_) => {
                return Err("PhysicalReg not supported in division".to_string());
            }
        };

        // Check for division by zero
        self.emit(MachineInstr::CmpRI {
            reg: divisor_reg,
            imm: 0,
        });

        // Jump if not zero
        let div_ok_label = self.next_label_id();
        self.emit(MachineInstr::JmpCC {
            cc: ConditionCode::NotEqual,
            target: LabelRef::Local(div_ok_label),
        });

        // Division by zero - exit with code 250
        self.emit(MachineInstr::MovRI64 {
            dest: Register::Rdi,
            imm: 250, // Exit code for divide by zero
        });
        self.emit(MachineInstr::MovRI64 {
            dest: Register::Rax,
            imm: 60, // sys_exit
        });
        self.emit(MachineInstr::Syscall);

        // Continue with division
        self.emit(MachineInstr::Label { id: div_ok_label });

        // Perform division
        self.emit(MachineInstr::Idiv {
            divisor: divisor_reg,
        });

        // Move result from RAX to dest
        let dest_reg = self.allocator.ensure_reg(dest, &[])?;
        self.emit_spill_reload_ops();
        if dest_reg != rax {
            self.emit(MachineInstr::MovRR {
                dest: dest_reg,
                src: rax,
            });
        }

        Ok(())
    }

    fn lower_comparison(
        &mut self,
        dest: VReg,
        lhs: &Value,
        rhs: &Value,
        op: &BinOp,
    ) -> Result<(), String> {
        // Get lhs into a register
        let lhs_reg = match lhs {
            Value::VReg(vreg) => {
                let reg = self.allocator.ensure_reg(*vreg, &[])?;
                self.emit_spill_reload_ops();
                reg
            }
            Value::Immediate(imm) => {
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
            Value::PhysicalReg(_) => {
                return Err("PhysicalReg not supported in comparison".to_string());
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
            Value::Immediate(imm) => {
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
            Value::PhysicalReg(_) => {
                return Err("PhysicalReg not supported in comparison".to_string());
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

        let dest_reg = self.allocator.ensure_reg(dest, &[lhs_reg])?;
        self.emit_spill_reload_ops();
        self.emit(MachineInstr::SetCC { dest: dest_reg, cc });
        self.emit(MachineInstr::Movzx {
            dest: dest_reg,
            src: dest_reg,
        });

        Ok(())
    }

    fn lower_load(&mut self, dest: VReg, offset: i64) -> Result<(), String> {
        let dest_reg = self.allocator.ensure_reg(dest, &[])?;
        self.emit_spill_reload_ops();
        self.emit(MachineInstr::MovRM {
            // load from stack slot
            dest: dest_reg,
            base: Register::Rbp, // <- use the frame-pointer
            offset: offset as i32,
        });
        Ok(())
    }

    fn lower_store(&mut self, src: VReg, offset: i64) -> Result<(), String> {
        let src_reg = self.allocator.ensure_reg(src, &[])?;
        self.emit_spill_reload_ops();
        self.emit(MachineInstr::MovMR {
            // store to stack slot
            base: Register::Rbp, // <- use the frame-pointer
            offset: offset as i32,
            src: src_reg,
        });
        Ok(())
    }

    fn lower_push(&mut self, src: VReg) -> Result<(), String> {
        let src_reg = self.allocator.ensure_reg(src, &[])?;
        self.emit_spill_reload_ops();
        self.emit(MachineInstr::Push { reg: src_reg });
        self.push_count += 1;
        Ok(())
    }

    fn lower_pop(&mut self, dest: VReg) -> Result<(), String> {
        let dest_reg = self.allocator.ensure_reg(dest, &[])?;
        self.emit_spill_reload_ops();
        self.emit(MachineInstr::Pop { reg: dest_reg });
        self.push_count -= 1;
        Ok(())
    }

    fn lower_jump(&mut self, target: LabelId) -> Result<(), String> {
        let machine_label_id = self.get_or_create_label(target);
        self.emit(MachineInstr::Jmp {
            target: LabelRef::Local(machine_label_id),
        });
        Ok(())
    }

    fn lower_branch(
        &mut self,
        condition: VReg,
        true_label: LabelId,
        false_label: LabelId,
    ) -> Result<(), String> {
        let cond_reg = self.allocator.ensure_reg(condition, &[])?;
        self.emit_spill_reload_ops();

        // Test condition
        self.emit(MachineInstr::CmpRI {
            reg: cond_reg,
            imm: 0,
        });

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
    ) -> Result<(), String> {
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

        // CRITICAL: Force all VRegs in caller-saved registers to be spilled first
        // This fixes the bug where VRegs holding let variable values get corrupted
        // when registers are reused after recursive function calls
        let is_user_function = !runtime_function.starts_with("__rue_");

        if is_user_function {
            // Force spill any VRegs currently in caller-saved registers
            // This is safer than relying on push/pop which doesn't update VReg mappings
            for &reg in &caller_saved_regs {
                if self.allocator.is_register_allocated(reg) {
                    self.allocator.invalidate_register(reg);
                }
            }

            // Emit any spill operations generated by the invalidation
            self.emit_spill_reload_ops();
        }

        // Now save caller-saved registers (they should mostly be free now)
        let mut regs_to_save = Vec::new();
        for &reg in &caller_saved_regs {
            if self.allocator.is_register_allocated(reg) {
                regs_to_save.push(reg);
            }
        }

        // Save caller-saved registers
        for &reg in &regs_to_save {
            self.emit(MachineInstr::Push { reg });
            self.push_count += 1;
        }

        // Check if we need alignment padding before the call
        // The stack must be 16-byte aligned before a call instruction.
        // We need to account for:
        // - Return address pushed by original call (1 push)
        // - RBP pushed by EnterFrame (tracked in push_count)
        // - Any other pushes we've done (also in push_count)
        // Total pushes = 1 (return addr) + push_count
        // If total is odd, we need padding
        let total_pushes = 1 + self.push_count;
        let needs_padding = total_pushes % 2 == 1;
        if needs_padding {
            // sub rsp, 8 to maintain alignment
            // Use SubRI directly to avoid AllocStack's 16-byte alignment
            self.emit(MachineInstr::SubRI {
                dest: Register::Rsp,
                imm: 8,
            });
        }

        // Move arguments to their designated registers
        // First, we need to check if any of the argument registers contain live values
        // that need to be preserved
        for (i, &_arg_vreg) in args.iter().enumerate() {
            if i >= arg_regs.len() {
                return Err("Too many arguments for function call".to_string());
            }

            // Check if the target argument register contains a live value
            if self.allocator.is_register_allocated(arg_regs[i])
                && !regs_to_save.contains(&arg_regs[i])
            {
                // This argument register contains a live value that we haven't saved yet
                // We need to save it before overwriting
                self.emit(MachineInstr::Push { reg: arg_regs[i] });
                self.push_count += 1;
                regs_to_save.push(arg_regs[i]);
            }
        }

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
        let temp_reg = Register::R15; // Use R15 as temporary

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

        // Move result from RAX if needed
        if dest.is_some() {
            // Check if RAX is in the list of registers to restore
            if regs_to_save.contains(&Register::Rax) {
                // RAX will be overwritten when we restore, so save to R15 first
                self.emit(MachineInstr::MovRR {
                    dest: Register::R15,
                    src: Register::Rax,
                });
            }
        }

        // Restore alignment padding if we added it
        if needs_padding {
            self.emit(MachineInstr::AddRI {
                dest: Register::Rsp,
                imm: 8,
            });
        }

        // Restore caller-saved registers in reverse order
        for &reg in regs_to_save.iter().rev() {
            self.emit(MachineInstr::Pop { reg });
            self.push_count -= 1;
        }

        // Now move the return value to its destination
        if let Some(dest_vreg) = dest {
            let dest_reg = self.allocator.ensure_reg(*dest_vreg, &[])?;
            self.emit_spill_reload_ops();

            // Determine where the return value is
            let source_reg = if regs_to_save.contains(&Register::Rax) {
                Register::R15 // We saved it here
            } else {
                Register::Rax // Still in RAX
            };

            if dest_reg != source_reg {
                self.emit(MachineInstr::MovRR {
                    dest: dest_reg,
                    src: source_reg,
                });
            }
        }

        Ok(())
    }

    fn lower_return(&mut self, value: Option<&VReg>) -> Result<(), String> {
        if let Some(vreg) = value {
            // Move return value to RAX
            let value_reg = self.allocator.ensure_reg(*vreg, &[])?;
            self.emit_spill_reload_ops();
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
    ) -> Result<(), String> {
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
                return Err("Too many arguments for syscall".to_string());
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
        self.emit_spill_reload_ops();
        if result_reg != Register::Rax {
            self.emit(MachineInstr::MovRR {
                dest: result_reg,
                src: Register::Rax,
            });
        }

        Ok(())
    }

    fn lower_save_registers(&mut self, registers: &[Register]) -> Result<(), String> {
        for &reg in registers {
            self.emit(MachineInstr::Push { reg });
        }
        Ok(())
    }

    fn lower_restore_registers(&mut self, registers: &[Register]) -> Result<(), String> {
        for &reg in registers.iter().rev() {
            self.emit(MachineInstr::Pop { reg });
        }
        Ok(())
    }

    fn lower_enter_frame(&mut self) -> Result<(), String> {
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

    fn lower_leave_frame(&mut self) -> Result<(), String> {
        // Standard x86-64 function epilogue
        self.emit(MachineInstr::LeaveFrame);
        Ok(())
    }

    fn get_or_create_label(&mut self, label_id: LabelId) -> u32 {
        // First check external label map if provided
        if let Some(ref external_map) = self.external_label_map {
            if let Some(&machine_id) = external_map.get(&label_id) {
                return machine_id;
            }
        }

        // Fall back to creating a new label
        *self.label_map.entry(label_id).or_insert_with(|| {
            let id = self.next_label_id;
            self.next_label_id += 1;
            id
        })
    }

    fn get_scratch_register(&self, forbidden: &[Register]) -> Result<Register, String> {
        // Use R15 as scratch register, checking it's not forbidden
        let scratch = Register::R15;
        if forbidden.contains(&scratch) {
            Err("Scratch register R15 is in use".to_string())
        } else {
            Ok(scratch)
        }
    }

    fn emit(&mut self, instr: MachineInstr) {
        // Track push/pop instructions for stack alignment
        match &instr {
            MachineInstr::Push { .. } => {
                self.push_count += 1;
            }
            MachineInstr::Pop { .. } => {
                if self.push_count > 0 {
                    self.push_count -= 1;
                }
            }
            _ => {}
        }
        self.instructions.push(instr);
    }

    /// Emit any pending spill/reload operations from the register allocator
    fn emit_spill_reload_ops(&mut self) {
        let ops = self.allocator.take_pending_ops();
        for op in ops {
            match op {
                SpillReloadOp::Spill { reg, stack_offset } => {
                    self.emit(MachineInstr::MovMR {
                        base: Register::Rbp,
                        offset: stack_offset,
                        src: reg,
                    });
                }
                SpillReloadOp::Reload { reg, stack_offset } => {
                    self.emit(MachineInstr::MovRM {
                        dest: reg,
                        base: Register::Rbp,
                        offset: stack_offset,
                    });
                }
                SpillReloadOp::Move { from, to } => {
                    self.emit(MachineInstr::MovRR {
                        dest: to,
                        src: from,
                    });
                }
            }
        }
    }
}
