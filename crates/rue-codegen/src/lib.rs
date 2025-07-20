use rue_ast::{CstRoot, ExpressionNode, FunctionNode, StatementNode};
use rue_semantic::Scope;
use std::collections::HashMap;

mod elf_writer;
mod lowering;
mod regalloc;
mod util;
mod x86_emitter;

use elf_writer::ElfWriter;
pub use lowering::Lowering;
pub use regalloc::RegisterAllocator;
pub use x86_emitter::X86Emitter;

// Import types from rue-ir
use rue_ir::target::{LabelRef, MachineInstr, Register};

#[derive(Debug, Clone, PartialEq)]
pub struct CodegenError {
    pub message: String,
}

impl std::fmt::Display for CodegenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for CodegenError {}

/// Virtual register - will be allocated to a physical register or stack slot
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct VReg(pub u32);

/// Value operand for instructions
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    VReg(VReg),
    Immediate(i64),
    PhysicalReg(Register),
}

/// Binary operations
#[derive(Debug, Clone, PartialEq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Lt,
    Le,
    Gt,
    Ge,
    Eq,
    Ne,
}

/// Label for control flow jumps
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LabelId(pub u32);

/// Platform-independent instruction set
///
/// Examples:
/// - `2 + 3` generates: Copy{v0, Imm(2)}, Copy{v1, Imm(3)}, BinaryOp{v2, v0, v1, Add}
/// - `x = 42` generates: Copy{v0, Imm(42)}, then maps variable "x" to v0
/// - `n * factorial(n-1)` generates: Push{v0}, Call{v1, "factorial", [v2]}, Pop{v3}, BinaryOp{v4, v3, v1, Mul}
#[derive(Debug, Clone)]
pub enum Instruction {
    // Data movement
    Copy {
        dest: VReg,
        src: Value,
    },

    // Arithmetic and comparison operations
    BinaryOp {
        dest: VReg,
        lhs: Value,
        rhs: Value,
        op: BinOp,
    },

    // Memory operations
    Load {
        dest: VReg,
        offset: i64,
    }, // Load from stack
    Store {
        src: VReg,
        offset: i64,
    }, // Store to stack

    // Stack operations for value preservation
    Push {
        src: VReg,
    }, // Push register to stack
    Pop {
        dest: VReg,
    }, // Pop from stack to register

    // Control flow
    Label(LabelId),
    Jump(LabelId),
    Branch {
        condition: VReg,
        true_label: LabelId,
        false_label: LabelId,
    },

    // Function operations
    Call {
        dest: Option<VReg>,
        function: String,
        args: Vec<VReg>,
    },
    Return {
        value: Option<VReg>,
    },

    // System operations
    Syscall {
        result: VReg,
        syscall_num: VReg,
        args: Vec<VReg>,
    },

    // Register preservation for calling convention
    SaveRegisters {
        registers: Vec<Register>,
    },
    RestoreRegisters {
        registers: Vec<Register>,
    },

    // Stack frame management
    EnterFrame,
    LeaveFrame,
}

// Code generator state
pub struct Codegen {
    instructions: Vec<Instruction>,
    vreg_counter: u32,
    label_counter: u32,
    stack_offset: i64,
    variables: HashMap<String, VReg>, // Variable -> virtual register
    function_labels: HashMap<String, LabelId>, // Function name -> label ID
}

impl Codegen {
    pub fn new() -> Self {
        Self {
            instructions: Vec::new(),
            vreg_counter: 0,
            label_counter: 0,
            stack_offset: 0,
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

    // Generate code for the entire program
    pub fn generate(
        &mut self,
        ast: &CstRoot,
        scope: &Scope,
    ) -> Result<Vec<Instruction>, CodegenError> {
        // First, generate all functions (including main)
        let mut main_found = false;

        for item in &ast.items {
            if let rue_ast::CstNode::Function(func) = item {
                if let rue_lexer::TokenKind::Ident(name) = &func.name.kind {
                    if name == "main" {
                        main_found = true;
                    }
                    self.generate_function(func, scope)?;
                }
            }
        }

        if !main_found {
            return Err(CodegenError {
                message: "No main function found".to_string(),
            });
        }

        // Generate program prologue (_start) AFTER all functions
        self.emit_prologue();

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
        // Result will be in RAX after the call
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
            args: vec![main_result], // Use main_result directly
        });
    }

    // Generate code for a function
    fn generate_function(
        &mut self,
        func: &FunctionNode,
        scope: &Scope,
    ) -> Result<(), CodegenError> {
        // Function label
        if let rue_lexer::TokenKind::Ident(name) = &func.name.kind {
            // Create a unique label for this function
            let func_label = self.next_label();
            self.emit(Instruction::Label(func_label));

            // Store the mapping from function name to label ID
            self.function_labels.insert(name.clone(), func_label);
        }

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
        for (i, param) in func.param_list.params.iter().enumerate() {
            if i >= param_registers.len() {
                return Err(CodegenError {
                    message: format!(
                        "Too many parameters (max {} supported)",
                        param_registers.len()
                    ),
                });
            }

            if let rue_lexer::TokenKind::Ident(param_name) = &param.name.kind {
                // Assign parameter to a new VReg
                let param_vreg = self.next_vreg();
                self.variables.insert(param_name.clone(), param_vreg);

                // Move parameter from register to parameter VReg
                self.emit(Instruction::Copy {
                    dest: param_vreg,
                    src: Value::PhysicalReg(param_registers[i]),
                });
            }
        }

        // Generate function body statements
        for stmt in &func.body.statements {
            self.generate_statement(stmt, scope)?;
        }

        // Generate final expression (return value)
        let return_vreg = if let Some(final_expr) = &func.body.final_expr {
            let vreg = self.generate_expression(final_expr, scope)?;
            Some(vreg)
        } else {
            None
        };

        // Return – the lowering layer will emit the epilogue (leave / ret)
        self.emit(Instruction::Return { value: return_vreg });

        // Reset state for next function
        self.stack_offset = 0;
        self.variables.clear();

        Ok(())
    }

    // Generate code for a statement, returns true if it's an expression that produces a value
    fn generate_statement(
        &mut self,
        stmt: &StatementNode,
        scope: &Scope,
    ) -> Result<Option<()>, CodegenError> {
        match stmt {
            StatementNode::Expression(expr_stmt) => {
                let _result_vreg = self.generate_expression(&expr_stmt.expression, scope)?;
                // Expression result is discarded for expression statements
                Ok(Some(()))
            }
            StatementNode::Let(let_stmt) => {
                // Generate the value expression
                let value_vreg = self.generate_expression(&let_stmt.value, scope)?;

                // Store in variable mapping
                if let rue_lexer::TokenKind::Ident(var_name) = &let_stmt.name.kind {
                    self.variables.insert(var_name.clone(), value_vreg);
                } else {
                    return Err(CodegenError {
                        message: "Invalid variable name in let statement".to_string(),
                    });
                }
                Ok(None)
            }
            StatementNode::Assign(assign_stmt) => {
                // Generate the value expression
                let value_vreg = self.generate_expression(&assign_stmt.value, scope)?;

                // Update existing variable
                if let rue_lexer::TokenKind::Ident(var_name) = &assign_stmt.name.kind {
                    if let Some(&existing_vreg) = self.variables.get(var_name) {
                        // Copy the new value to the existing variable's location
                        self.emit(Instruction::Copy {
                            dest: existing_vreg,
                            src: Value::VReg(value_vreg),
                        });
                    } else {
                        return Err(CodegenError {
                            message: format!("Undefined variable in assignment: {var_name}"),
                        });
                    }
                } else {
                    return Err(CodegenError {
                        message: "Invalid variable name in assignment".to_string(),
                    });
                }
                Ok(None)
            }
        }
    }

    // Helper function to check if an expression contains function calls
    fn expression_contains_call(&self, expr: &ExpressionNode) -> bool {
        match expr {
            ExpressionNode::Call(_) => true,
            ExpressionNode::Binary(binary_expr) => {
                self.expression_contains_call(&binary_expr.left)
                    || self.expression_contains_call(&binary_expr.right)
            }
            ExpressionNode::If(if_expr) => {
                self.expression_contains_call(&if_expr.condition)
                    || self.block_contains_call(&if_expr.then_block)
                    || if let Some(else_clause) = &if_expr.else_clause {
                        match &else_clause.body {
                            rue_ast::ElseBodyNode::Block(block) => self.block_contains_call(block),
                            rue_ast::ElseBodyNode::If(nested_if) => self
                                .expression_contains_call(&ExpressionNode::If(nested_if.clone())),
                        }
                    } else {
                        false
                    }
            }
            ExpressionNode::While(while_expr) => {
                self.expression_contains_call(&while_expr.condition)
                    || self.block_contains_call(&while_expr.body)
            }
            ExpressionNode::Literal(_) | ExpressionNode::Identifier(_) => false,
        }
    }

    // Helper function to check if a block contains function calls
    fn block_contains_call(&self, block: &rue_ast::BlockNode) -> bool {
        // Check statements
        for stmt in &block.statements {
            if self.statement_contains_call(stmt) {
                return true;
            }
        }
        // Check final expression
        if let Some(final_expr) = &block.final_expr {
            return self.expression_contains_call(final_expr);
        }
        false
    }

    // Helper function to check if a statement contains function calls
    fn statement_contains_call(&self, stmt: &StatementNode) -> bool {
        match stmt {
            StatementNode::Expression(expr_stmt) => {
                self.expression_contains_call(&expr_stmt.expression)
            }
            StatementNode::Let(let_stmt) => self.expression_contains_call(&let_stmt.value),
            StatementNode::Assign(assign_stmt) => self.expression_contains_call(&assign_stmt.value),
        }
    }

    // Generate code for an expression, returns VReg containing result
    fn generate_expression(
        &mut self,
        expr: &ExpressionNode,
        _scope: &Scope,
    ) -> Result<VReg, CodegenError> {
        match expr {
            ExpressionNode::Literal(token) => {
                let dest = self.next_vreg();
                match &token.kind {
                    rue_lexer::TokenKind::Integer(value) => {
                        self.emit(Instruction::Copy {
                            dest,
                            src: Value::Immediate(*value),
                        });
                        Ok(dest)
                    }
                    rue_lexer::TokenKind::True => {
                        self.emit(Instruction::Copy {
                            dest,
                            src: Value::Immediate(1),
                        });
                        Ok(dest)
                    }
                    rue_lexer::TokenKind::False => {
                        self.emit(Instruction::Copy {
                            dest,
                            src: Value::Immediate(0),
                        });
                        Ok(dest)
                    }
                    rue_lexer::TokenKind::Unit => {
                        // Unit values are represented as 0
                        self.emit(Instruction::Copy {
                            dest,
                            src: Value::Immediate(0),
                        });
                        Ok(dest)
                    }
                    _ => Err(CodegenError {
                        message: "Invalid literal token".to_string(),
                    }),
                }
            }
            ExpressionNode::Identifier(token) => {
                if let rue_lexer::TokenKind::Ident(name) = &token.kind {
                    if let Some(&var_vreg) = self.variables.get(name) {
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
                } else {
                    Err(CodegenError {
                        message: "Invalid identifier token".to_string(),
                    })
                }
            }
            ExpressionNode::Binary(binary_expr) => {
                // For operations where the RHS might be a function call (that could modify registers),
                // we need to preserve the LHS value properly
                let dest = self.next_vreg();
                let op = match &binary_expr.operator.kind {
                    rue_lexer::TokenKind::Plus => BinOp::Add,
                    rue_lexer::TokenKind::Minus => BinOp::Sub,
                    rue_lexer::TokenKind::Star => BinOp::Mul,
                    rue_lexer::TokenKind::Slash => BinOp::Div,
                    rue_lexer::TokenKind::Percent => BinOp::Mod,
                    rue_lexer::TokenKind::Less => BinOp::Lt,
                    rue_lexer::TokenKind::LessEqual => BinOp::Le,
                    rue_lexer::TokenKind::Greater => BinOp::Gt,
                    rue_lexer::TokenKind::GreaterEqual => BinOp::Ge,
                    rue_lexer::TokenKind::Equal => BinOp::Eq,
                    rue_lexer::TokenKind::NotEqual => BinOp::Ne,
                    _ => {
                        return Err(CodegenError {
                            message: format!(
                                "Unsupported operator: {:?}",
                                binary_expr.operator.kind
                            ),
                        });
                    }
                };

                // Check if either operand contains function calls
                let lhs_has_call = self.expression_contains_call(&binary_expr.left);
                let rhs_has_call = self.expression_contains_call(&binary_expr.right);

                if lhs_has_call && rhs_has_call {
                    // Special case: both sides have function calls
                    // Use the same preservation strategy as single-sided case

                    // Evaluate LHS first
                    let lhs_vreg = self.generate_expression(&binary_expr.left, _scope)?;

                    // Preserve LHS on stack before evaluating RHS (same as single case)
                    self.emit(Instruction::Push { src: lhs_vreg });

                    // Evaluate RHS
                    let rhs_vreg = self.generate_expression(&binary_expr.right, _scope)?;

                    // Restore LHS from stack (same logic as single case)
                    let preserved_lhs = self.next_vreg();
                    self.emit(Instruction::Pop {
                        dest: preserved_lhs,
                    });

                    // Perform the operation
                    self.emit(Instruction::BinaryOp {
                        dest,
                        lhs: Value::VReg(preserved_lhs),
                        rhs: Value::VReg(rhs_vreg),
                        op,
                    });
                } else if lhs_has_call || rhs_has_call {
                    // Original logic for when only one side has calls
                    let lhs_vreg = self.generate_expression(&binary_expr.left, _scope)?;

                    // If RHS contains calls, preserve LHS across those calls
                    if rhs_has_call {
                        self.emit(Instruction::Push { src: lhs_vreg });
                    }

                    // Evaluate RHS
                    let rhs_vreg = self.generate_expression(&binary_expr.right, _scope)?;

                    // Restore LHS if we preserved it
                    let final_lhs = if rhs_has_call {
                        let preserved_lhs = self.next_vreg();
                        self.emit(Instruction::Pop {
                            dest: preserved_lhs,
                        });
                        preserved_lhs
                    } else {
                        lhs_vreg
                    };

                    // Perform the operation
                    self.emit(Instruction::BinaryOp {
                        dest,
                        lhs: Value::VReg(final_lhs),
                        rhs: Value::VReg(rhs_vreg),
                        op,
                    });
                } else {
                    // Standard evaluation when no function calls are involved
                    let lhs_vreg = self.generate_expression(&binary_expr.left, _scope)?;

                    // Check if RHS is a literal that can be used as immediate
                    let rhs_value = match binary_expr.right.as_ref() {
                        ExpressionNode::Literal(token) => match &token.kind {
                            rue_lexer::TokenKind::Integer(value) => Some(Value::Immediate(*value)),
                            rue_lexer::TokenKind::True => Some(Value::Immediate(1)),
                            rue_lexer::TokenKind::False => Some(Value::Immediate(0)),
                            rue_lexer::TokenKind::Unit => Some(Value::Immediate(0)),
                            _ => None,
                        },
                        _ => None,
                    };

                    if let Some(immediate_value) = rhs_value {
                        // Use immediate operand directly
                        self.emit(Instruction::BinaryOp {
                            dest,
                            lhs: Value::VReg(lhs_vreg),
                            rhs: immediate_value,
                            op,
                        });
                    } else {
                        // RHS is not a literal, evaluate it normally
                        let rhs_vreg = self.generate_expression(&binary_expr.right, _scope)?;
                        self.emit(Instruction::BinaryOp {
                            dest,
                            lhs: Value::VReg(lhs_vreg),
                            rhs: Value::VReg(rhs_vreg),
                            op,
                        });
                    }
                }

                Ok(dest)
            }
            ExpressionNode::Call(call_expr) => {
                // Generate arguments
                let mut arg_vregs = Vec::new();
                for arg in &call_expr.args {
                    let arg_vreg = self.generate_expression(arg, _scope)?;
                    arg_vregs.push(arg_vreg);
                }

                // Call function with proper calling convention
                if let ExpressionNode::Identifier(func_token) = &*call_expr.function {
                    if let rue_lexer::TokenKind::Ident(func_name) = &func_token.kind {
                        let dest = self.next_vreg();
                        self.emit(Instruction::Call {
                            dest: Some(dest),
                            function: func_name.clone(),
                            args: arg_vregs,
                        });

                        Ok(dest)
                    } else {
                        Err(CodegenError {
                            message: "Invalid function name".to_string(),
                        })
                    }
                } else {
                    Err(CodegenError {
                        message: "Function calls must use identifiers".to_string(),
                    })
                }
            }
            ExpressionNode::If(if_stmt) => {
                let else_label = self.next_label();
                let end_label = self.next_label();

                // Create a shared result register that both branches will write to
                let result_vreg = self.next_vreg();

                // Generate condition
                let condition_vreg = self.generate_expression(&if_stmt.condition, _scope)?;

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

                // Generate then block statements
                for stmt in &if_stmt.then_block.statements {
                    self.generate_statement(stmt, _scope)?;
                }

                // Generate then block final expression and copy to result
                let then_result = if let Some(final_expr) = &if_stmt.then_block.final_expr {
                    self.generate_expression(final_expr, _scope)?
                } else {
                    let zero_vreg = self.next_vreg();
                    self.emit(Instruction::Copy {
                        dest: zero_vreg,
                        src: Value::Immediate(0),
                    });
                    zero_vreg
                };

                // Copy then result to shared result register
                self.emit(Instruction::Copy {
                    dest: result_vreg,
                    src: Value::VReg(then_result),
                });

                self.emit(Instruction::Jump(end_label));

                // Generate else block
                self.emit(Instruction::Label(else_label));
                let else_result = if let Some(else_clause) = &if_stmt.else_clause {
                    match &else_clause.body {
                        rue_ast::ElseBodyNode::Block(block) => {
                            for stmt in &block.statements {
                                self.generate_statement(stmt, _scope)?;
                            }
                            if let Some(final_expr) = &block.final_expr {
                                self.generate_expression(final_expr, _scope)?
                            } else {
                                let zero_vreg = self.next_vreg();
                                self.emit(Instruction::Copy {
                                    dest: zero_vreg,
                                    src: Value::Immediate(0),
                                });
                                zero_vreg
                            }
                        }
                        rue_ast::ElseBodyNode::If(nested_if) => self
                            .generate_expression(&ExpressionNode::If(nested_if.clone()), _scope)?,
                    }
                } else {
                    let zero_vreg = self.next_vreg();
                    self.emit(Instruction::Copy {
                        dest: zero_vreg,
                        src: Value::Immediate(0),
                    });
                    zero_vreg
                };

                // Copy else result to shared result register
                self.emit(Instruction::Copy {
                    dest: result_vreg,
                    src: Value::VReg(else_result),
                });

                self.emit(Instruction::Label(end_label));

                // Return the shared result register
                Ok(result_vreg)
            }
            ExpressionNode::While(while_stmt) => {
                let loop_start = self.next_label();
                let loop_end = self.next_label();

                // Loop start label
                self.emit(Instruction::Label(loop_start));

                // Generate condition
                let condition_vreg = self.generate_expression(&while_stmt.condition, _scope)?;

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

                // Generate loop body statements
                for stmt in &while_stmt.body.statements {
                    self.generate_statement(stmt, _scope)?;
                }
                // Generate loop body final expression (if any) - value is discarded
                if let Some(final_expr) = &while_stmt.body.final_expr {
                    let _result = self.generate_expression(final_expr, _scope)?;
                }

                // Jump back to condition check
                self.emit(Instruction::Jump(loop_start));

                // Loop end label
                self.emit(Instruction::Label(loop_end));

                // While expressions always return 0
                let zero_vreg = self.next_vreg();
                self.emit(Instruction::Copy {
                    dest: zero_vreg,
                    src: Value::Immediate(0),
                });

                Ok(zero_vreg)
            }
        }
    }
}

impl Default for Codegen {
    fn default() -> Self {
        Self::new()
    }
}

// High-level compilation function using the proper lowering pipeline
pub fn compile_to_executable(ast: &CstRoot, scope: &Scope) -> Result<Vec<u8>, CodegenError> {
    // Phase 0: Generate runtime code from rue-runtime crate
    let (runtime_instructions, runtime_labels) =
        rue_runtime::generate_runtime().map_err(|e| CodegenError { message: e })?;
    // Phase 1: Generate high-level IR instructions
    let mut codegen = Codegen::new();
    let instructions = codegen.generate(ast, scope)?;
    let function_labels = codegen.function_labels.clone();

    // Phase 2: Identify function boundaries and allocate registers per function
    let mut function_boundaries = Vec::new();
    let mut current_start = 0;

    for (i, instr) in instructions.iter().enumerate() {
        if let Instruction::Label(label_id) = instr {
            if function_labels.values().any(|&id| id == *label_id) {
                if current_start < i {
                    function_boundaries.push((current_start, i));
                }
                current_start = i;
            }
        }
    }
    if current_start < instructions.len() {
        function_boundaries.push((current_start, instructions.len()));
    }

    // Phase 3: Lower each function separately with its own register allocator
    // We need to preserve labels across function boundaries, so we'll do this in two passes:
    // First pass: collect all labels
    // Second pass: lower with proper label mapping

    let mut all_machine_instructions = Vec::new();
    let mut label_id_counter = 0u32;
    let mut ir_to_machine_labels = HashMap::new();

    // First pass: scan for labels and assign machine instruction label IDs
    for instr in &instructions {
        if let Instruction::Label(label_id) = instr {
            ir_to_machine_labels.entry(*label_id).or_insert_with(|| {
                let id = label_id_counter;
                label_id_counter += 1;
                id
            });
        }
    }

    // Second pass: lower each function with its own allocator
    for (start, end) in function_boundaries {
        let function_instrs = &instructions[start..end];

        // Create a fresh register allocator for this function
        let mut function_allocator = RegisterAllocator::new();
        let mut function_machine_instrs = Vec::new();
        let next_label_id;

        {
            let mut lowering = Lowering::new(&mut function_allocator, label_id_counter);
            lowering.set_function_labels(function_labels.clone());
            lowering.set_label_map(ir_to_machine_labels.clone());

            // Process all non-label instructions for this function
            let mut function_instrs_without_labels = Vec::new();

            for instr in function_instrs {
                match instr {
                    Instruction::Label(label_id) => {
                        // First, lower any accumulated instructions
                        if !function_instrs_without_labels.is_empty() {
                            let machine_instrs = lowering
                                .lower(&function_instrs_without_labels)
                                .map_err(|e| CodegenError { message: e })?;
                            function_machine_instrs.extend(machine_instrs);
                            function_instrs_without_labels.clear();
                        }

                        // Then emit the label
                        let machine_label_id = ir_to_machine_labels[label_id];
                        function_machine_instrs.push(MachineInstr::Label {
                            id: machine_label_id,
                        });
                    }
                    _ => {
                        function_instrs_without_labels.push(instr.clone());
                    }
                }
            }

            // Lower any remaining instructions
            if !function_instrs_without_labels.is_empty() {
                let machine_instrs = lowering
                    .lower(&function_instrs_without_labels)
                    .map_err(|e| CodegenError { message: e })?;
                function_machine_instrs.extend(machine_instrs);
            }

            next_label_id = lowering.next_label_id();
        }

        // Patch stack allocation with actual required space
        Lowering::patch_stack_allocation(&mut function_machine_instrs, &function_allocator);

        // Add this function's instructions to the overall list
        all_machine_instructions.extend(function_machine_instrs);

        // Update the global label counter for the next function
        label_id_counter = next_label_id;
    }

    // Phase 4: Combine runtime and user code
    let mut final_instructions = Vec::new();

    // Add runtime instructions first
    final_instructions.extend(runtime_instructions);

    // Adjust user code labels to account for runtime labels
    let runtime_label_count = runtime_labels.values().max().copied().unwrap_or(0) + 1;

    // Adjust all label IDs in user code
    for instr in all_machine_instructions {
        match instr {
            MachineInstr::Label { id } => {
                final_instructions.push(MachineInstr::Label {
                    id: id + runtime_label_count,
                });
            }
            MachineInstr::Jmp { target } => {
                let adjusted_target = match target {
                    LabelRef::Local(id) => LabelRef::Local(id + runtime_label_count),
                    global => global,
                };
                final_instructions.push(MachineInstr::Jmp {
                    target: adjusted_target,
                });
            }
            MachineInstr::JmpCC { cc, target } => {
                let adjusted_target = match target {
                    LabelRef::Local(id) => LabelRef::Local(id + runtime_label_count),
                    global => global,
                };
                final_instructions.push(MachineInstr::JmpCC {
                    cc,
                    target: adjusted_target,
                });
            }
            other => final_instructions.push(other),
        }
    }

    // Phase 5: Emit machine code
    let mut x86_emitter = X86Emitter::new();

    // Set up all labels (runtime + user)
    let mut all_function_labels = HashMap::new();

    // Add runtime labels
    for (name, &id) in &runtime_labels {
        all_function_labels.insert(name.clone(), crate::LabelId(id));
    }

    // Add user function labels (adjusted)
    for (name, ir_label_id) in &function_labels {
        if let Some(&machine_label_id) = ir_to_machine_labels.get(ir_label_id) {
            all_function_labels.insert(
                name.clone(),
                crate::LabelId(machine_label_id + runtime_label_count),
            );
        }
    }

    x86_emitter.set_function_labels(all_function_labels);

    let code = x86_emitter
        .emit_all(&final_instructions)
        .map_err(|e| CodegenError { message: e })?;

    let (_, symbols) = x86_emitter.get_output();

    // Phase 5: Generate ELF executable
    let elf_writer = ElfWriter::new();
    let elf = elf_writer.generate_elf(&code, &symbols);
    Ok(elf)
}

#[cfg(test)]
mod test;
