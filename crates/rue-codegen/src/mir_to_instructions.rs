//! MIR to Instruction lowering
//!
//! This module converts MIR (in SSA form with block parameters) to the
//! platform-independent Instruction representation with virtual registers.

use crate::{BinOp, Instruction, LabelId, VReg, Value};
use rue_ir::mir::{
    BasicBlock, BlockId, MirBinOp, MirConst, MirFunction, MirProgram, MirStatement, MirTerminator,
    MirUnaryOp, MirValue, Temp,
};
use rue_ir::target::Register;
use std::collections::HashMap;

/// Lowers MIR to Instructions
pub struct MirToInstructions {
    /// Generated instructions
    instructions: Vec<Instruction>,
    /// Counter for virtual registers
    vreg_counter: u32,
    /// Counter for labels
    label_counter: u32,
    /// Mapping from MIR temps to virtual registers
    temp_to_vreg: HashMap<Temp, VReg>,
    /// Mapping from block IDs to labels
    block_to_label: HashMap<BlockId, LabelId>,
    /// Function labels for calls
    function_labels: HashMap<String, LabelId>,
    /// Current function blocks (needed for block parameter lookup)
    current_blocks: Vec<BasicBlock>,
}

impl MirToInstructions {
    pub fn new() -> Self {
        Self {
            instructions: Vec::new(),
            vreg_counter: 0,
            label_counter: 0,
            temp_to_vreg: HashMap::new(),
            block_to_label: HashMap::new(),
            function_labels: HashMap::new(),
            current_blocks: Vec::new(),
        }
    }

    /// Generate a fresh virtual register
    fn fresh_vreg(&mut self) -> VReg {
        let vreg = VReg(self.vreg_counter);
        self.vreg_counter += 1;
        vreg
    }

    /// Generate a fresh label
    fn fresh_label(&mut self) -> LabelId {
        let label = LabelId(self.label_counter);
        self.label_counter += 1;
        label
    }

    /// Get or create a virtual register for a temp
    fn get_vreg(&mut self, temp: Temp) -> VReg {
        if let Some(&vreg) = self.temp_to_vreg.get(&temp) {
            vreg
        } else {
            let vreg = self.fresh_vreg();
            self.temp_to_vreg.insert(temp, vreg);
            vreg
        }
    }

    /// Get or create a label for a block
    fn get_label(&mut self, block: BlockId) -> LabelId {
        if let Some(&label) = self.block_to_label.get(&block) {
            label
        } else {
            let label = self.fresh_label();
            self.block_to_label.insert(block, label);
            label
        }
    }

    /// Emit an instruction
    fn emit(&mut self, instr: Instruction) {
        self.instructions.push(instr);
    }

    /// Lower a MIR program to instructions
    pub fn lower_program(&mut self, program: &MirProgram) -> Vec<Instruction> {
        // First pass: collect all function labels
        for func in &program.functions {
            let label = self.fresh_label();
            self.function_labels.insert(func.name.clone(), label);
        }

        // Generate code for all functions
        for func in &program.functions {
            self.lower_function(func);
        }

        // Generate entry point
        self.emit_entry_point();

        self.instructions.clone()
    }

    /// Get the function labels mapping
    pub fn get_function_labels(&self) -> HashMap<String, LabelId> {
        self.function_labels.clone()
    }

    /// Generate the _start entry point
    fn emit_entry_point(&mut self) {
        let start_label = self.fresh_label();
        self.emit(Instruction::Label(start_label));
        self.function_labels
            .insert("_start".to_string(), start_label);

        // Call main
        let main_result = self.fresh_vreg();
        self.emit(Instruction::Call {
            dest: Some(main_result),
            function: "main".to_string(),
            args: vec![],
        });

        // Exit with main's result
        let syscall_num = self.fresh_vreg();
        self.emit(Instruction::Copy {
            dest: syscall_num,
            src: Value::Immediate(60), // sys_exit
        });

        let syscall_result = self.fresh_vreg();
        self.emit(Instruction::Syscall {
            result: syscall_result,
            syscall_num,
            args: vec![main_result],
        });
    }

    /// Lower a MIR function to instructions
    fn lower_function(&mut self, func: &MirFunction) {
        // Clear temp mappings for new function
        self.temp_to_vreg.clear();
        self.block_to_label.clear();
        self.current_blocks = func.blocks.clone();

        // Function label
        let func_label = self.function_labels[&func.name];
        self.emit(Instruction::Label(func_label));

        // Function prologue
        self.emit(Instruction::EnterFrame);

        // Handle function parameters from calling convention registers
        let param_registers = [
            Register::Rdi,
            Register::Rsi,
            Register::Rdx,
            Register::Rcx,
            Register::R8,
            Register::R9,
        ];

        // Map entry block parameters to function parameters
        if let Some(entry_block) = func.blocks.iter().find(|b| b.id == func.entry_block) {
            for (i, (temp, _ty)) in entry_block.params.iter().enumerate() {
                if i < param_registers.len() {
                    let vreg = self.get_vreg(*temp);
                    self.emit(Instruction::Copy {
                        dest: vreg,
                        src: Value::PhysicalReg(param_registers[i]),
                    });
                }
            }
        }

        // Generate labels for all blocks
        for block in &func.blocks {
            self.get_label(block.id);
        }

        // Lower each block
        for block in &func.blocks {
            self.lower_block(block);
        }
    }

    /// Lower a basic block
    fn lower_block(&mut self, block: &BasicBlock) {
        // Emit block label
        let label = self.get_label(block.id);
        self.emit(Instruction::Label(label));

        // Block parameters are handled by the predecessors passing arguments

        // Lower statements
        for stmt in &block.statements {
            self.lower_statement(stmt);
        }

        // Lower terminator
        self.lower_terminator(&block.terminator);
    }

    /// Lower a MIR statement
    fn lower_statement(&mut self, stmt: &MirStatement) {
        match stmt {
            MirStatement::Assign { dest, value, .. } => {
                let dest_vreg = self.get_vreg(*dest);
                self.lower_value(dest_vreg, value);
            }
        }
    }

    /// Lower a MIR value, storing result in dest_vreg
    fn lower_value(&mut self, dest: VReg, value: &MirValue) {
        match value {
            MirValue::Use(temp) => {
                let src_vreg = self.get_vreg(*temp);
                self.emit(Instruction::Copy {
                    dest,
                    src: Value::VReg(src_vreg),
                });
            }
            MirValue::Const(c) => {
                let imm = match c {
                    MirConst::Int32(n) => *n as i64,
                    MirConst::Int64(n) => *n,
                    MirConst::Bool(b) => {
                        if *b {
                            1
                        } else {
                            0
                        }
                    }
                    MirConst::Unit => 0,
                };
                self.emit(Instruction::Copy {
                    dest,
                    src: Value::Immediate(imm),
                });
            }
            MirValue::BinaryOp { op, lhs, rhs } => {
                let lhs_vreg = self.get_vreg(*lhs);
                let rhs_vreg = self.get_vreg(*rhs);

                let instr_op = match op {
                    MirBinOp::Add => BinOp::Add,
                    MirBinOp::Sub => BinOp::Sub,
                    MirBinOp::Mul => BinOp::Mul,
                    MirBinOp::Div => BinOp::Div,
                    MirBinOp::Mod => BinOp::Mod,
                    MirBinOp::Lt => BinOp::Lt,
                    MirBinOp::Le => BinOp::Le,
                    MirBinOp::Gt => BinOp::Gt,
                    MirBinOp::Ge => BinOp::Ge,
                    MirBinOp::Eq => BinOp::Eq,
                    MirBinOp::Ne => BinOp::Ne,
                };

                self.emit(Instruction::BinaryOp {
                    dest,
                    lhs: Value::VReg(lhs_vreg),
                    rhs: Value::VReg(rhs_vreg),
                    op: instr_op,
                });
            }
            MirValue::UnaryOp { op, operand } => {
                let operand_vreg = self.get_vreg(*operand);

                match op {
                    MirUnaryOp::Neg => {
                        // Implement negation as 0 - operand
                        let zero = self.fresh_vreg();
                        self.emit(Instruction::Copy {
                            dest: zero,
                            src: Value::Immediate(0),
                        });
                        self.emit(Instruction::BinaryOp {
                            dest,
                            lhs: Value::VReg(zero),
                            rhs: Value::VReg(operand_vreg),
                            op: BinOp::Sub,
                        });
                    }
                }
            }
            MirValue::Call { func, args, .. } => {
                let arg_vregs: Vec<VReg> = args.iter().map(|&arg| self.get_vreg(arg)).collect();
                self.emit(Instruction::Call {
                    dest: Some(dest),
                    function: func.clone(),
                    args: arg_vregs,
                });
            }
        }
    }

    /// Lower a MIR terminator
    fn lower_terminator(&mut self, term: &MirTerminator) {
        match term {
            MirTerminator::Goto { target, args } => {
                // Handle block arguments by copying to block parameters
                self.lower_block_arguments(*target, args);

                let label = self.get_label(*target);
                self.emit(Instruction::Jump(label));
            }
            MirTerminator::Branch {
                condition,
                then_block,
                then_args,
                else_block,
                else_args,
            } => {
                let cond_vreg = self.get_vreg(*condition);

                // We need to handle block arguments differently for each branch
                // Generate intermediate blocks to handle the arguments

                let then_label = self.fresh_label();
                let else_label = self.fresh_label();
                let then_target = self.get_label(*then_block);
                let else_target = self.get_label(*else_block);

                // Emit the branch
                self.emit(Instruction::Branch {
                    condition: cond_vreg,
                    true_label: then_label,
                    false_label: else_label,
                });

                // Then branch: copy arguments and jump
                self.emit(Instruction::Label(then_label));
                self.lower_block_arguments(*then_block, then_args);
                self.emit(Instruction::Jump(then_target));

                // Else branch: copy arguments and jump
                self.emit(Instruction::Label(else_label));
                self.lower_block_arguments(*else_block, else_args);
                self.emit(Instruction::Jump(else_target));
            }
            MirTerminator::Return { value } => {
                if let Some(val) = value {
                    let vreg = self.get_vreg(*val);
                    self.emit(Instruction::Return { value: Some(vreg) });
                } else {
                    self.emit(Instruction::Return { value: None });
                }
            }
        }
    }

    /// Handle block arguments when jumping to a block
    fn lower_block_arguments(&mut self, target_block: BlockId, args: &[Temp]) {
        // Look up the target block's parameters and collect them first
        let params: Vec<(Temp, usize)> = self
            .current_blocks
            .iter()
            .find(|b| b.id == target_block)
            .map(|target| {
                target
                    .params
                    .iter()
                    .enumerate()
                    .map(|(i, (temp, _))| (*temp, i))
                    .collect()
            })
            .unwrap_or_default();

        // Generate copies from arguments to block parameters
        for (param_temp, i) in params {
            if let Some(&arg_temp) = args.get(i) {
                let arg_vreg = self.get_vreg(arg_temp);
                let param_vreg = self.get_vreg(param_temp);

                // Generate copy from argument to parameter
                self.emit(Instruction::Copy {
                    dest: param_vreg,
                    src: Value::VReg(arg_vreg),
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rue_ir::mir::{
        BasicBlock, BlockId, MirBinOp, MirFunction, MirProgram, MirStatement, MirTerminator,
        MirValue, Temp,
    };
    use rue_ir::types::RueType;

    #[test]
    fn test_lower_simple_add() {
        // Create a simple MIR function: fn add(a, b) { return a + b }
        let mir_func = MirFunction {
            name: "add".to_string(),
            params: vec![
                ("a".to_string(), RueType::I32),
                ("b".to_string(), RueType::I32),
            ],
            return_type: RueType::I32,
            entry_block: BlockId(0),
            span: rue_lexer::Span::dummy(),
            blocks: vec![BasicBlock {
                id: BlockId(0),
                params: vec![(Temp(0), RueType::I32), (Temp(1), RueType::I32)],
                statements: vec![MirStatement::Assign {
                    dest: Temp(2),
                    value: MirValue::BinaryOp {
                        op: MirBinOp::Add,
                        lhs: Temp(0),
                        rhs: Temp(1),
                    },
                    span: None,
                }],
                terminator: MirTerminator::Return {
                    value: Some(Temp(2)),
                },
            }],
        };

        let mir_program = MirProgram {
            functions: vec![mir_func],
        };

        let mut lowerer = MirToInstructions::new();
        let instructions = lowerer.lower_program(&mir_program);

        // Verify we have instructions
        assert!(!instructions.is_empty());

        // Should have labels, copies for parameters, binary op, return
        let has_label = instructions
            .iter()
            .any(|i| matches!(i, Instruction::Label(_)));
        let has_binary_op = instructions
            .iter()
            .any(|i| matches!(i, Instruction::BinaryOp { .. }));
        let has_return = instructions
            .iter()
            .any(|i| matches!(i, Instruction::Return { .. }));

        assert!(has_label);
        assert!(has_binary_op);
        assert!(has_return);
    }
}
