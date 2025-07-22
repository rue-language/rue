//! HIR-based code generation
//!
//! This module generates platform-independent instructions from HIR.

use crate::{BinOp, CodegenError, Instruction, LabelId, VReg, Value};
use rue_ir::hir::{
    BinOp as HirBinOp, HirBlock, HirExpr, HirFunction, HirLiteral, HirProgram, HirStatement,
    UnaryOp as HirUnaryOp,
};
use rue_ir::target::Register;
use std::collections::HashMap;

/// HIR code generator state
pub struct HirCodegen {
    instructions: Vec<Instruction>,
    vreg_counter: u32,
    label_counter: u32,
    variables: HashMap<String, VReg>, // Variable name -> virtual register
    function_labels: HashMap<String, LabelId>, // Function name -> label ID
}

impl HirCodegen {
    pub fn new() -> Self {
        Self {
            instructions: Vec::new(),
            vreg_counter: 0,
            label_counter: 0,
            variables: HashMap::new(),
            function_labels: HashMap::new(),
        }
    }

    // Generate a unique virtual register
    fn next_vreg(&mut self) -> VReg {
        let vreg = VReg(self.vreg_counter);
        self.vreg_counter += 1;
        vreg
    }

    // Generate a unique label
    fn next_label(&mut self) -> LabelId {
        let label = LabelId(self.label_counter);
        self.label_counter += 1;
        label
    }

    // Emit an instruction
    fn emit(&mut self, instr: Instruction) {
        self.instructions.push(instr);
    }

    /// Generate code for the entire HIR program
    pub fn generate(&mut self, hir: &HirProgram) -> Result<Vec<Instruction>, CodegenError> {
        // First, generate all functions (including main)
        let mut main_found = false;

        for func in &hir.functions {
            if func.name == "main" {
                main_found = true;
            }
            self.generate_function(func)?;
        }

        if !main_found {
            return Err(CodegenError {
                message: "No main function found".to_string(),
            });
        }

        // Generate program prologue (_start) AFTER all functions
        self.emit_prologue();

        // Print debug information about generated instructions
        // self.print_instructions();

        Ok(self.instructions.clone())
    }

    // Generate program entry point
    fn emit_prologue(&mut self) {
        // Entry point label (_start)
        let start_label = self.next_label();
        self.emit(Instruction::Label(start_label));

        // Store _start label for later reference
        self.function_labels
            .insert("_start".to_string(), start_label);

        // Call main function
        let main_result = self.next_vreg();
        self.emit(Instruction::Call {
            dest: Some(main_result),
            function: "main".to_string(),
            args: vec![],
        });

        // Set up syscall number first to avoid register conflicts
        let syscall_num = self.next_vreg();
        self.emit(Instruction::Copy {
            dest: syscall_num,
            src: Value::Immediate(60), // sys_exit
        });

        // Exit syscall with main's result directly
        let syscall_result = self.next_vreg();
        self.emit(Instruction::Syscall {
            result: syscall_result,
            syscall_num,
            args: vec![main_result],
        });
    }

    // Generate code for a function
    fn generate_function(&mut self, func: &HirFunction) -> Result<(), CodegenError> {
        // Clear variables for new function
        self.variables.clear();

        // Function label
        let func_label = self.next_label();
        self.emit(Instruction::Label(func_label));

        // Store the mapping from function name to label ID
        self.function_labels.insert(func.name.clone(), func_label);

        // Emit function prologue
        self.emit(Instruction::EnterFrame);

        // Handle parameters - use System V AMD64 ABI calling convention
        let param_registers = [
            Register::Rdi,
            Register::Rsi,
            Register::Rdx,
            Register::Rcx,
            Register::R8,
            Register::R9,
        ];
        for (i, (param_name, _param_ty)) in func.params.iter().enumerate() {
            if i >= param_registers.len() {
                return Err(CodegenError {
                    message: format!(
                        "Too many parameters (max {} supported)",
                        param_registers.len()
                    ),
                });
            }

            // Assign parameter to a new VReg
            let param_vreg = self.next_vreg();
            self.variables.insert(param_name.clone(), param_vreg);

            // Move parameter from register to parameter VReg
            self.emit(Instruction::Copy {
                dest: param_vreg,
                src: Value::PhysicalReg(param_registers[i]),
            });
        }

        // Generate function body
        let return_vreg = self.generate_block(&func.body)?;

        // Return – the lowering layer will emit the epilogue (leave / ret)
        self.emit(Instruction::Return { value: return_vreg });

        Ok(())
    }

    // Generate code for a block, returns the VReg of the final expression (if any)
    fn generate_block(&mut self, block: &HirBlock) -> Result<Option<VReg>, CodegenError> {
        // Save current variable scope
        let saved_vars = self.variables.clone();

        // Check if this block has multiple consecutive let statements with function calls
        // If so, we need to be more careful about register preservation
        let consecutive_function_lets = self.has_consecutive_function_lets(&block.statements);

        if consecutive_function_lets {
            // Handle consecutive function lets specially to avoid register corruption
            for (i, stmt) in block.statements.iter().enumerate() {
                match stmt {
                    HirStatement::Let { name, init, .. }
                        if self.expression_has_function_calls(init) =>
                    {
                        // Generate the init expression
                        let value_vreg = self.generate_expression(init)?;

                        // Allocate a new VReg for the variable
                        let var_vreg = self.next_vreg();

                        // Copy init value to variable
                        self.emit(Instruction::Copy {
                            dest: var_vreg,
                            src: Value::VReg(value_vreg),
                        });

                        // Store in variable mapping
                        self.variables.insert(name.clone(), var_vreg);

                        // If there are more let statements with function calls after this one,
                        // force this variable to be preserved by creating a backup
                        let remaining_function_lets = block.statements[i+1..].iter()
                            .any(|s| matches!(s, HirStatement::Let { init, .. } if self.expression_has_function_calls(init)));

                        if remaining_function_lets {
                            // Create a backup copy to force spilling
                            let backup_vreg = self.next_vreg();
                            self.emit(Instruction::Copy {
                                dest: backup_vreg,
                                src: Value::VReg(var_vreg),
                            });
                            // Use the backup VReg for the variable
                            self.variables.insert(name.clone(), backup_vreg);
                        }
                    }
                    _ => {
                        // Handle other statements normally
                        self.generate_statement(stmt)?;
                    }
                }
            }
        } else {
            // Generate statements normally
            for stmt in &block.statements {
                self.generate_statement(stmt)?;
            }
        }

        // Generate final expression
        let result = if let Some(expr) = &block.expr {
            Some(self.generate_expression(expr)?)
        } else {
            None
        };

        // Restore variable scope
        self.variables = saved_vars;

        Ok(result)
    }

    // Generate code for a statement
    fn generate_statement(&mut self, stmt: &HirStatement) -> Result<(), CodegenError> {
        match stmt {
            HirStatement::Let {
                name, ty: _, init, ..
            } => {
                // Generate the init expression
                let value_vreg = self.generate_expression(init)?;

                // Allocate a new VReg for the variable
                let var_vreg = self.next_vreg();

                // Copy init value to variable
                self.emit(Instruction::Copy {
                    dest: var_vreg,
                    src: Value::VReg(value_vreg),
                });

                // Store in variable mapping
                self.variables.insert(name.clone(), var_vreg);
            }
            HirStatement::Assign { name, value, .. } => {
                // Generate the value expression
                let value_vreg = self.generate_expression(value)?;

                // Update existing variable
                if let Some(&existing_vreg) = self.variables.get(name) {
                    // Copy the new value to the existing variable's location
                    self.emit(Instruction::Copy {
                        dest: existing_vreg,
                        src: Value::VReg(value_vreg),
                    });
                } else {
                    return Err(CodegenError {
                        message: format!("Undefined variable in assignment: {name}"),
                    });
                }
            }
            HirStatement::Expr(expr) => {
                // Generate expression, discard result
                let _result = self.generate_expression(expr)?;
            }
        }
        Ok(())
    }

    // Generate code for an expression, returns VReg containing result
    fn generate_expression(&mut self, expr: &HirExpr) -> Result<VReg, CodegenError> {
        match expr {
            HirExpr::Literal { lit, .. } => {
                let dest = self.next_vreg();
                let value = match lit {
                    HirLiteral::Int32(v) => *v as i64,
                    HirLiteral::Int64(v) => *v,
                    HirLiteral::Bool(b) => {
                        if *b {
                            1
                        } else {
                            0
                        }
                    }
                    HirLiteral::Unit => 0,
                };
                self.emit(Instruction::Copy {
                    dest,
                    src: Value::Immediate(value),
                });
                Ok(dest)
            }
            HirExpr::Var { name, .. } => {
                if let Some(&var_vreg) = self.variables.get(name) {
                    // Copy variable value to new vreg
                    let dest = self.next_vreg();
                    self.emit(Instruction::Copy {
                        dest,
                        src: Value::VReg(var_vreg),
                    });
                    Ok(dest)
                } else {
                    Err(CodegenError {
                        message: format!("Undefined variable: {name}"),
                    })
                }
            }
            HirExpr::Binary { op, lhs, rhs, .. } => {
                // Check if operands contain function calls that could cause register corruption
                let lhs_has_calls = self.expression_has_function_calls(lhs);
                let rhs_has_calls = self.expression_has_function_calls(rhs);

                let (lhs_vreg, rhs_vreg) = if lhs_has_calls && rhs_has_calls {
                    // Both operands have function calls - use temporary let bindings to avoid corruption
                    // This ensures each function call result is properly preserved
                    let lhs_vreg = self.generate_expression(lhs)?;
                    let lhs_temp = self.next_vreg();
                    self.emit(Instruction::Copy {
                        dest: lhs_temp,
                        src: Value::VReg(lhs_vreg),
                    });

                    let rhs_vreg = self.generate_expression(rhs)?;
                    (lhs_temp, rhs_vreg)
                } else if rhs_has_calls && !lhs_has_calls {
                    // Only RHS has function calls, generate RHS first to avoid corrupting LHS
                    let rhs_vreg = self.generate_expression(rhs)?;
                    let lhs_vreg = self.generate_expression(lhs)?;
                    (lhs_vreg, rhs_vreg)
                } else {
                    // Normal case: LHS doesn't have calls, or neither has calls
                    let lhs_vreg = self.generate_expression(lhs)?;
                    let rhs_vreg = self.generate_expression(rhs)?;
                    (lhs_vreg, rhs_vreg)
                };

                // Convert HIR binop to codegen binop
                let codegen_op = match op {
                    HirBinOp::Add => BinOp::Add,
                    HirBinOp::Sub => BinOp::Sub,
                    HirBinOp::Mul => BinOp::Mul,
                    HirBinOp::Div => BinOp::Div,
                    HirBinOp::Mod => BinOp::Mod,
                    HirBinOp::Lt => BinOp::Lt,
                    HirBinOp::Le => BinOp::Le,
                    HirBinOp::Gt => BinOp::Gt,
                    HirBinOp::Ge => BinOp::Ge,
                    HirBinOp::Eq => BinOp::Eq,
                    HirBinOp::Ne => BinOp::Ne,
                };

                let dest = self.next_vreg();
                self.emit(Instruction::BinaryOp {
                    dest,
                    lhs: Value::VReg(lhs_vreg),
                    rhs: Value::VReg(rhs_vreg),
                    op: codegen_op,
                });

                Ok(dest)
            }
            HirExpr::Unary { op, expr, .. } => {
                let operand_vreg = self.generate_expression(expr)?;
                let dest = self.next_vreg();

                match op {
                    HirUnaryOp::Neg => {
                        // Generate negation as 0 - operand
                        self.emit(Instruction::BinaryOp {
                            dest,
                            op: BinOp::Sub,
                            lhs: Value::Immediate(0),
                            rhs: Value::VReg(operand_vreg),
                        });
                    }
                }

                Ok(dest)
            }
            HirExpr::Call { func, args, .. } => {
                // Generate arguments
                let mut arg_vregs = Vec::new();
                for arg in args {
                    let arg_vreg = self.generate_expression(arg)?;
                    arg_vregs.push(arg_vreg);
                }

                // Call function
                let dest = self.next_vreg();
                self.emit(Instruction::Call {
                    dest: Some(dest),
                    function: func.clone(),
                    args: arg_vregs,
                });

                Ok(dest)
            }
            HirExpr::If {
                cond,
                then_block,
                else_block,
                ..
            } => {
                let else_label = self.next_label();
                let end_label = self.next_label();

                // Generate condition
                let condition_vreg = self.generate_expression(cond)?;

                // Generate then block label
                let then_label = self.next_label();

                // Branch on condition
                self.emit(Instruction::Branch {
                    condition: condition_vreg,
                    true_label: then_label,
                    false_label: else_label,
                });

                // Generate then block
                self.emit(Instruction::Label(then_label));
                let then_result = self.generate_block(then_block)?;

                // Store then result on stack to preserve it across control flow
                // This works around register allocator issues with phi nodes
                let then_vreg = if let Some(vreg) = then_result {
                    vreg
                } else {
                    let unit_vreg = self.next_vreg();
                    self.emit(Instruction::Copy {
                        dest: unit_vreg,
                        src: Value::Immediate(0),
                    });
                    unit_vreg
                };

                // Push then result to stack to preserve it
                self.emit(Instruction::Push { src: then_vreg });

                self.emit(Instruction::Jump(end_label));

                // Generate else block
                self.emit(Instruction::Label(else_label));
                let else_vreg = if let Some(else_block) = else_block {
                    let else_result = self.generate_block(else_block)?;
                    if let Some(vreg) = else_result {
                        vreg
                    } else {
                        let unit_vreg = self.next_vreg();
                        self.emit(Instruction::Copy {
                            dest: unit_vreg,
                            src: Value::Immediate(0),
                        });
                        unit_vreg
                    }
                } else {
                    // No else block, use unit (0)
                    let unit_vreg = self.next_vreg();
                    self.emit(Instruction::Copy {
                        dest: unit_vreg,
                        src: Value::Immediate(0),
                    });
                    unit_vreg
                };

                // Push else result to stack
                self.emit(Instruction::Push { src: else_vreg });

                self.emit(Instruction::Label(end_label));

                // Pop the result from whichever branch was taken
                let result_vreg = self.next_vreg();
                self.emit(Instruction::Pop { dest: result_vreg });

                Ok(result_vreg)
            }
            HirExpr::While { cond, body, .. } => {
                let loop_start = self.next_label();
                let loop_end = self.next_label();

                // Loop start label
                self.emit(Instruction::Label(loop_start));

                // Generate condition
                let condition_vreg = self.generate_expression(cond)?;

                // Generate body label
                let body_label = self.next_label();

                // Branch on condition (if false, exit loop)
                self.emit(Instruction::Branch {
                    condition: condition_vreg,
                    true_label: body_label,
                    false_label: loop_end,
                });

                // Generate loop body
                self.emit(Instruction::Label(body_label));
                let _body_result = self.generate_block(body)?;

                // Jump back to condition check
                self.emit(Instruction::Jump(loop_start));

                // Loop end label
                self.emit(Instruction::Label(loop_end));

                // While expressions always return unit (0)
                let zero_vreg = self.next_vreg();
                self.emit(Instruction::Copy {
                    dest: zero_vreg,
                    src: Value::Immediate(0),
                });

                Ok(zero_vreg)
            }
        }
    }

    /// Check if an expression contains function calls
    fn expression_has_function_calls(&self, expr: &HirExpr) -> bool {
        match expr {
            HirExpr::Literal { .. } => false,
            HirExpr::Var { .. } => false,
            HirExpr::Binary { lhs, rhs, .. } => {
                self.expression_has_function_calls(lhs) || self.expression_has_function_calls(rhs)
            }
            HirExpr::Unary { expr, .. } => self.expression_has_function_calls(expr),
            HirExpr::Call { .. } => true,
            HirExpr::If {
                cond,
                then_block,
                else_block,
                ..
            } => {
                self.expression_has_function_calls(cond)
                    || self.block_has_function_calls(then_block)
                    || else_block
                        .as_ref()
                        .is_some_and(|b| self.block_has_function_calls(b))
            }
            HirExpr::While { cond, body, .. } => {
                self.expression_has_function_calls(cond) || self.block_has_function_calls(body)
            }
        }
    }

    /// Check if a block contains function calls
    fn block_has_function_calls(&self, block: &HirBlock) -> bool {
        // Check statements
        for stmt in &block.statements {
            if self.statement_has_function_calls(stmt) {
                return true;
            }
        }
        // Check final expression
        if let Some(expr) = &block.expr {
            if self.expression_has_function_calls(expr) {
                return true;
            }
        }
        false
    }

    /// Check if a statement contains function calls
    fn statement_has_function_calls(&self, stmt: &HirStatement) -> bool {
        match stmt {
            HirStatement::Let { init, .. } => self.expression_has_function_calls(init),
            HirStatement::Assign { value, .. } => self.expression_has_function_calls(value),
            HirStatement::Expr(expr) => self.expression_has_function_calls(expr),
        }
    }

    /// Check if a list of statements has consecutive let statements with function calls
    fn has_consecutive_function_lets(&self, statements: &[HirStatement]) -> bool {
        let mut found_function_let = false;
        for stmt in statements {
            match stmt {
                HirStatement::Let { init, .. } if self.expression_has_function_calls(init) => {
                    if found_function_let {
                        return true; // Found a second consecutive function let
                    }
                    found_function_let = true;
                }
                HirStatement::Let { .. } => {
                    // Regular let statement, reset the flag
                    found_function_let = false;
                }
                _ => {
                    // Other statement types don't break the consecutive pattern
                    // but don't contribute to it either
                }
            }
        }
        false
    }

    /// Get the generated instructions and function labels
    pub fn get_output(self) -> (Vec<Instruction>, HashMap<String, LabelId>) {
        (self.instructions, self.function_labels)
    }
}

impl Default for HirCodegen {
    fn default() -> Self {
        Self::new()
    }
}
