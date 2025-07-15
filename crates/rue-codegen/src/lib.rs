use rue_ast::{CstRoot, ExpressionNode, FunctionNode, StatementNode};
use rue_semantic::Scope;
use std::collections::HashMap;

mod regalloc;
pub use regalloc::RegisterAllocator;

#[derive(Debug, Clone, PartialEq)]
pub struct CodegenError {
    pub message: String,
}

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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Register {
    Rax, // Accumulator, return value
    Rbx, // Base
    Rcx, // Counter
    Rdx, // Data
    Rsp, // Stack pointer
    Rbp, // Base pointer
    Rsi, // Source index
    Rdi, // Destination index
    R8,  // Extended registers
    R9,
    R10,
    R11,
    R12,
    R13,
    R14,
    R15,
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
        // Generate program prologue
        self.emit_prologue();

        // Find and generate main function first
        let mut main_generated = false;
        for item in &ast.items {
            if let rue_ast::CstNode::Function(func) = item {
                if let rue_lexer::TokenKind::Ident(name) = &func.name.kind {
                    if name == "main" {
                        self.generate_function(func, scope)?;
                        main_generated = true;
                        break;
                    }
                }
            }
        }

        if !main_generated {
            return Err(CodegenError {
                message: "No main function found".to_string(),
            });
        }

        // Generate other functions
        for item in &ast.items {
            if let rue_ast::CstNode::Function(func) = item {
                if let rue_lexer::TokenKind::Ident(name) = &func.name.kind {
                    if name != "main" {
                        self.generate_function(func, scope)?;
                    }
                }
            }
        }

        self.emit_epilogue();

        Ok(self.instructions.clone())
    }

    // Generate program entry point
    fn emit_prologue(&mut self) {
        // Entry point label (_start)
        let start_label = LabelId(999); // Reserve special ID for _start
        self.emit(Instruction::Label(start_label));

        // Call main function
        let main_result = self.next_vreg();
        self.emit(Instruction::Call {
            dest: Some(main_result),
            function: "main".to_string(),
            args: vec![],
        });

        // Exit program with main's return value
        let exit_code = self.next_vreg();
        self.emit(Instruction::Copy {
            dest: exit_code,
            src: Value::VReg(main_result),
        });

        let syscall_num = self.next_vreg();
        self.emit(Instruction::Copy {
            dest: syscall_num,
            src: Value::Immediate(60), // sys_exit
        });

        let syscall_result = self.next_vreg();
        self.emit(Instruction::Syscall {
            result: syscall_result,
            syscall_num,
            args: vec![exit_code],
        });
    }

    fn emit_epilogue(&mut self) {
        // Additional functions would go here
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

        // Handle parameters - use System V AMD64 ABI calling convention
        let param_registers = [Register::Rdi, Register::Rsi, Register::Rdx, Register::Rcx];
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

        // Return instruction
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
                    if self.variables.contains_key(var_name) {
                        self.variables.insert(var_name.clone(), value_vreg);
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

                // Check if RHS contains a function call that could corrupt registers
                let rhs_has_call = self.expression_contains_call(&binary_expr.right);

                if rhs_has_call {
                    // Strategy: Evaluate LHS, push to stack, evaluate RHS, pop LHS back
                    let lhs_vreg = self.generate_expression(&binary_expr.left, _scope)?;

                    // Push LHS value to stack to preserve across function call
                    self.emit(Instruction::Push { src: lhs_vreg });

                    // Evaluate RHS (this may contain function calls that corrupt registers)
                    let rhs_vreg = self.generate_expression(&binary_expr.right, _scope)?;

                    // Pop LHS back from stack
                    let lhs_restored = self.next_vreg();
                    self.emit(Instruction::Pop { dest: lhs_restored });

                    // Perform the operation
                    self.emit(Instruction::BinaryOp {
                        dest,
                        lhs: Value::VReg(lhs_restored),
                        rhs: Value::VReg(rhs_vreg),
                        op,
                    });
                } else {
                    // Standard evaluation when no function calls are involved
                    let lhs_vreg = self.generate_expression(&binary_expr.left, _scope)?;
                    let rhs_vreg = self.generate_expression(&binary_expr.right, _scope)?;

                    self.emit(Instruction::BinaryOp {
                        dest,
                        lhs: Value::VReg(lhs_vreg),
                        rhs: Value::VReg(rhs_vreg),
                        op,
                    });
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
                        // Save caller-saved registers before function call
                        // These are registers that might be clobbered by the callee
                        // DON'T save RAX since it's used for return values
                        let _caller_saved_regs = [
                            Register::Rbx,
                            Register::Rcx,
                            Register::Rdx,
                            Register::Rsi,
                            Register::Rdi,
                        ];
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

// ELF executable generator
pub struct Assembler {
    code: Vec<u8>,
    symbol_table: HashMap<String, u64>,
    relocations: Vec<Relocation>,
    function_labels: HashMap<String, LabelId>, // Function name -> label mapping
}

#[derive(Debug)]
struct Relocation {
    offset: u64,
    symbol: String,
    #[allow(dead_code)]
    rel_type: RelocationType,
}

#[derive(Debug)]
enum RelocationType {
    Rel32, // 32-bit relative call/jump
}

impl Assembler {
    pub fn new() -> Self {
        Self {
            code: Vec::new(),
            symbol_table: HashMap::new(),
            relocations: Vec::new(),
            function_labels: HashMap::new(),
        }
    }

    pub fn add_function_mapping(&mut self, name: String, label_id: LabelId) {
        self.function_labels.insert(name, label_id);
    }

    // Convert TargetIR instructions to machine code with register allocation (single-pass)
    pub fn assemble(&mut self, instructions: Vec<Instruction>) -> Result<Vec<u8>, CodegenError> {
        // Step 1: Perform register allocation
        let mut regalloc = RegisterAllocator::new();

        // Collect all VRegs used in the instructions
        for instr in &instructions {
            self.collect_vregs_for_allocation(instr, &mut regalloc);
        }

        // Step 2: Single-pass code generation with fixups
        self.code.clear();
        self.relocations.clear();
        self.symbol_table.clear();

        // Track label positions and forward references
        let mut label_positions: HashMap<LabelId, u64> = HashMap::new();
        let mut forward_refs: Vec<(u64, LabelId, bool)> = Vec::new(); // (position, target_label, is_jump)

        for instr in &instructions {
            let current_pos = self.code.len() as u64;

            match instr {
                Instruction::Label(label_id) => {
                    // Record this label's position
                    label_positions.insert(*label_id, current_pos);

                    // Add to symbol table
                    if label_id.0 == 999 {
                        self.symbol_table.insert("_start".to_string(), current_pos);
                    }
                    self.symbol_table
                        .insert(format!("label_{}", label_id.0), current_pos);

                    // Check if this is a known function
                    if let Some(func_name) = self
                        .function_labels
                        .iter()
                        .find(|(_, id)| **id == *label_id)
                        .map(|(name, _)| name.clone())
                    {
                        self.symbol_table.insert(func_name, current_pos);
                    }

                    // No code emitted for labels
                }

                Instruction::Jump(target_label) => {
                    // Emit jump instruction with placeholder offset
                    self.code.push(0xe9); // jmp rel32
                    let fixup_pos = self.code.len() as u64;
                    self.code.extend_from_slice(&[0, 0, 0, 0]); // placeholder

                    // Record forward reference for later patching
                    forward_refs.push((fixup_pos, *target_label, true));
                }

                Instruction::Branch {
                    condition,
                    true_label,
                    false_label,
                } => {
                    // Generate comparison and conditional jump
                    let cond_reg =
                        regalloc
                            .get_register(*condition)
                            .ok_or_else(|| CodegenError {
                                message: format!(
                                    "No register allocated for condition {condition:?}"
                                ),
                            })?;

                    // cmp reg, 0
                    self.code.push(0x48); // REX.W
                    self.code.push(0x83); // cmp r/m64, imm8
                    self.code.push(0xf8 + self.register_code(&cond_reg)); // /7 r
                    self.code.push(0x00); // immediate 0

                    // jne true_label
                    self.code.push(0x0f); // jne rel32
                    self.code.push(0x85);
                    let true_fixup_pos = self.code.len() as u64;
                    self.code.extend_from_slice(&[0, 0, 0, 0]); // placeholder
                    forward_refs.push((true_fixup_pos, *true_label, true));

                    // jmp false_label
                    self.code.push(0xe9); // jmp rel32
                    let false_fixup_pos = self.code.len() as u64;
                    self.code.extend_from_slice(&[0, 0, 0, 0]); // placeholder
                    forward_refs.push((false_fixup_pos, *false_label, true));
                }

                _ => {
                    // Emit other instructions normally
                    self.emit_targetir_instruction(instr, &regalloc)?;
                }
            }
        }

        // Step 3: Patch all forward references
        for (fixup_pos, target_label, _is_jump) in forward_refs {
            if let Some(&target_addr) = label_positions.get(&target_label) {
                let current_end = fixup_pos + 4; // Position after the 4-byte offset
                let offset = (target_addr as i64) - (current_end as i64);

                // Write the offset back into the code
                let offset_bytes = (offset as i32).to_le_bytes();
                for (i, &byte) in offset_bytes.iter().enumerate() {
                    self.code[(fixup_pos + i as u64) as usize] = byte;
                }
            } else {
                return Err(CodegenError {
                    message: format!("Undefined label: {target_label:?}"),
                });
            }
        }

        // Step 4: Resolve any remaining relocations (for external symbols)
        self.resolve_relocations()?;

        Ok(self.code.clone())
    }

    // Helper to collect VRegs that need allocation
    fn collect_vregs_for_allocation(&self, instr: &Instruction, regalloc: &mut RegisterAllocator) {
        match instr {
            Instruction::Copy { dest, src } => {
                regalloc.allocate(*dest);
                if let Value::VReg(src_vreg) = src {
                    regalloc.allocate(*src_vreg);
                }
            }
            Instruction::BinaryOp { dest, lhs, rhs, .. } => {
                regalloc.allocate(*dest);
                if let Value::VReg(lhs_vreg) = lhs {
                    regalloc.allocate(*lhs_vreg);
                }
                if let Value::VReg(rhs_vreg) = rhs {
                    regalloc.allocate(*rhs_vreg);
                }
            }
            Instruction::Return {
                value: Some(return_vreg),
            } => {
                regalloc.allocate(*return_vreg);
            }
            Instruction::Return { value: None } => {
                // No register allocation needed for void return
            }
            Instruction::Branch { condition, .. } => {
                regalloc.allocate(*condition);
            }
            Instruction::Call { dest, args, .. } => {
                if let Some(dest_vreg) = dest {
                    regalloc.allocate(*dest_vreg);
                }
                for arg in args {
                    regalloc.allocate(*arg);
                }
            }
            Instruction::Syscall {
                result,
                syscall_num,
                args,
            } => {
                regalloc.allocate(*result);
                regalloc.allocate(*syscall_num);
                for arg in args {
                    regalloc.allocate(*arg);
                }
            }
            Instruction::Load { dest, .. } => {
                regalloc.allocate(*dest);
            }
            Instruction::Store { src, .. } => {
                regalloc.allocate(*src);
            }
            Instruction::SaveRegisters { .. } => {
                // No VReg allocation needed for physical register operations
            }
            Instruction::RestoreRegisters { .. } => {
                // No VReg allocation needed for physical register operations
            }
            Instruction::Push { src } => {
                regalloc.allocate(*src);
            }
            Instruction::Pop { dest } => {
                regalloc.allocate(*dest);
            }
            // Labels and jumps don't need register allocation
            Instruction::Label(_) | Instruction::Jump(_) => {}
        }
    }

    fn emit_targetir_instruction(
        &mut self,
        instr: &Instruction,
        regalloc: &RegisterAllocator,
    ) -> Result<(), CodegenError> {
        match instr {
            Instruction::Copy { dest, src } => {
                let dest_reg = regalloc.get_register(*dest).ok_or_else(|| CodegenError {
                    message: format!("No register allocated for {dest:?}"),
                })?;

                match src {
                    Value::Immediate(imm) => {
                        // mov reg, imm64 = 48 b8+r imm64
                        self.code.push(0x48); // REX.W prefix
                        self.code.push(0xb8 + self.register_code(&dest_reg));
                        self.code.extend_from_slice(&imm.to_le_bytes());
                    }
                    Value::VReg(src_vreg) => {
                        let src_reg =
                            regalloc
                                .get_register(*src_vreg)
                                .ok_or_else(|| CodegenError {
                                    message: format!("No register allocated for {src_vreg:?}"),
                                })?;

                        // mov dst, src = 48 89 ModR/M
                        self.code.push(0x48); // REX.W prefix  
                        self.code.push(0x89);
                        self.code.push(
                            0xc0 | (self.register_code(&src_reg) << 3)
                                | self.register_code(&dest_reg),
                        );
                    }
                    Value::PhysicalReg(src_reg) => {
                        // mov dst, src = 48 89 ModR/M (from physical register)
                        self.code.push(0x48); // REX.W prefix  
                        self.code.push(0x89);
                        self.code.push(
                            0xc0 | (self.register_code(src_reg) << 3)
                                | self.register_code(&dest_reg),
                        );
                    }
                }
            }
            Instruction::BinaryOp { dest, lhs, rhs, op } => {
                let dest_reg = regalloc.get_register(*dest).ok_or_else(|| CodegenError {
                    message: format!("No register allocated for {dest:?}"),
                })?;

                // For simplicity, we'll use a two-instruction approach:
                // 1. Move lhs to dest
                // 2. Apply operation with rhs

                // First, get lhs into dest register
                match lhs {
                    Value::Immediate(imm) => {
                        // mov dest, imm
                        self.code.push(0x48); // REX.W prefix
                        self.code.push(0xb8 + self.register_code(&dest_reg));
                        self.code.extend_from_slice(&imm.to_le_bytes());
                    }
                    Value::VReg(lhs_vreg) => {
                        let lhs_reg =
                            regalloc
                                .get_register(*lhs_vreg)
                                .ok_or_else(|| CodegenError {
                                    message: format!("No register allocated for {lhs_vreg:?}"),
                                })?;
                        // mov dest, lhs
                        self.code.push(0x48);
                        self.code.push(0x89);
                        self.code.push(
                            0xc0 | (self.register_code(&lhs_reg) << 3)
                                | self.register_code(&dest_reg),
                        );
                    }
                    Value::PhysicalReg(_) => {
                        return Err(CodegenError {
                            message: "PhysicalReg not supported in binary operations".to_string(),
                        });
                    }
                }

                // Now apply operation with rhs
                match op {
                    BinOp::Add => {
                        match rhs {
                            Value::VReg(rhs_vreg) => {
                                let rhs_reg =
                                    regalloc.get_register(*rhs_vreg).ok_or_else(|| {
                                        CodegenError {
                                            message: format!(
                                                "No register allocated for {rhs_vreg:?}"
                                            ),
                                        }
                                    })?;
                                // add dest, rhs
                                self.code.push(0x48);
                                self.code.push(0x01);
                                self.code.push(
                                    0xc0 | (self.register_code(&rhs_reg) << 3)
                                        | self.register_code(&dest_reg),
                                );
                            }
                            Value::Immediate(_) => {
                                // TODO: Handle immediate addition
                                return Err(CodegenError {
                                    message: "Immediate operands not yet supported for binary ops"
                                        .to_string(),
                                });
                            }
                            Value::PhysicalReg(_) => {
                                return Err(CodegenError {
                                    message: "PhysicalReg not supported in binary operations"
                                        .to_string(),
                                });
                            }
                        }
                    }
                    BinOp::Sub => {
                        match rhs {
                            Value::VReg(rhs_vreg) => {
                                let rhs_reg =
                                    regalloc.get_register(*rhs_vreg).ok_or_else(|| {
                                        CodegenError {
                                            message: format!(
                                                "No register allocated for {rhs_vreg:?}"
                                            ),
                                        }
                                    })?;
                                // sub dest, rhs
                                self.code.push(0x48);
                                self.code.push(0x29);
                                self.code.push(
                                    0xc0 | (self.register_code(&rhs_reg) << 3)
                                        | self.register_code(&dest_reg),
                                );
                            }
                            Value::Immediate(_) => {
                                return Err(CodegenError {
                                    message: "Immediate operands not yet supported for binary ops"
                                        .to_string(),
                                });
                            }
                            Value::PhysicalReg(_) => {
                                return Err(CodegenError {
                                    message: "PhysicalReg not supported in binary operations"
                                        .to_string(),
                                });
                            }
                        }
                    }
                    BinOp::Mul => {
                        match rhs {
                            Value::VReg(rhs_vreg) => {
                                let rhs_reg =
                                    regalloc.get_register(*rhs_vreg).ok_or_else(|| {
                                        CodegenError {
                                            message: format!(
                                                "No register allocated for {rhs_vreg:?}"
                                            ),
                                        }
                                    })?;
                                // imul dest, rhs
                                self.code.push(0x48);
                                self.code.push(0x0f);
                                self.code.push(0xaf);
                                self.code.push(
                                    0xc0 | (self.register_code(&dest_reg) << 3)
                                        | self.register_code(&rhs_reg),
                                );
                            }
                            Value::Immediate(_) => {
                                return Err(CodegenError {
                                    message: "Immediate operands not yet supported for binary ops"
                                        .to_string(),
                                });
                            }
                            Value::PhysicalReg(_) => {
                                return Err(CodegenError {
                                    message: "PhysicalReg not supported in binary operations"
                                        .to_string(),
                                });
                            }
                        }
                    }
                    BinOp::Div => {
                        // Division requires specific register usage (dividend in rax, quotient in rax)
                        // For now, return error
                        return Err(CodegenError {
                            message: "Division not yet implemented in TargetIR backend".to_string(),
                        });
                    }
                    BinOp::Le => {
                        // Comparison operations set flags, we need to generate a boolean result
                        match rhs {
                            Value::VReg(rhs_vreg) => {
                                let rhs_reg =
                                    regalloc.get_register(*rhs_vreg).ok_or_else(|| {
                                        CodegenError {
                                            message: format!(
                                                "No register allocated for {rhs_vreg:?}"
                                            ),
                                        }
                                    })?;

                                // cmp lhs, rhs (note: lhs is already in dest)
                                self.code.push(0x48);
                                self.code.push(0x39);
                                self.code.push(
                                    0xc0 | (self.register_code(&rhs_reg) << 3)
                                        | self.register_code(&dest_reg),
                                );

                                // setle al (set if less or equal)
                                self.code.push(0x0f);
                                self.code.push(0x9e);
                                self.code.push(0xc0); // al register

                                // movzx dest, al (zero extend to full register)
                                self.code.push(0x48);
                                self.code.push(0x0f);
                                self.code.push(0xb6);
                                self.code.push(0xc0 | (self.register_code(&dest_reg) << 3));
                            }
                            Value::Immediate(_) => {
                                return Err(CodegenError {
                                    message: "Immediate operands not yet supported for comparisons"
                                        .to_string(),
                                });
                            }
                            Value::PhysicalReg(_) => {
                                return Err(CodegenError {
                                    message: "PhysicalReg not supported in binary operations"
                                        .to_string(),
                                });
                            }
                        }
                    }
                    BinOp::Lt => {
                        // Less than comparison
                        match rhs {
                            Value::VReg(rhs_vreg) => {
                                let rhs_reg =
                                    regalloc.get_register(*rhs_vreg).ok_or_else(|| {
                                        CodegenError {
                                            message: format!(
                                                "No register allocated for {rhs_vreg:?}"
                                            ),
                                        }
                                    })?;

                                // cmp lhs, rhs (note: lhs is already in dest)
                                self.code.push(0x48);
                                self.code.push(0x39);
                                self.code.push(
                                    0xc0 | (self.register_code(&rhs_reg) << 3)
                                        | self.register_code(&dest_reg),
                                );

                                // setl al (set if less)
                                self.code.push(0x0f);
                                self.code.push(0x9c);
                                self.code.push(0xc0); // al register

                                // movzx dest, al (zero extend to full register)
                                self.code.push(0x48);
                                self.code.push(0x0f);
                                self.code.push(0xb6);
                                self.code.push(0xc0 | (self.register_code(&dest_reg) << 3));
                            }
                            Value::Immediate(_) => {
                                return Err(CodegenError {
                                    message: "Immediate operands not yet supported for comparisons"
                                        .to_string(),
                                });
                            }
                            Value::PhysicalReg(_) => {
                                return Err(CodegenError {
                                    message: "PhysicalReg not supported in binary operations"
                                        .to_string(),
                                });
                            }
                        }
                    }
                    BinOp::Gt => {
                        // Greater than comparison
                        match rhs {
                            Value::VReg(rhs_vreg) => {
                                let rhs_reg =
                                    regalloc.get_register(*rhs_vreg).ok_or_else(|| {
                                        CodegenError {
                                            message: format!(
                                                "No register allocated for {rhs_vreg:?}"
                                            ),
                                        }
                                    })?;

                                // cmp lhs, rhs (note: lhs is already in dest)
                                self.code.push(0x48);
                                self.code.push(0x39);
                                self.code.push(
                                    0xc0 | (self.register_code(&rhs_reg) << 3)
                                        | self.register_code(&dest_reg),
                                );

                                // setg al (set if greater)
                                self.code.push(0x0f);
                                self.code.push(0x9f);
                                self.code.push(0xc0); // al register

                                // movzx dest, al (zero extend to full register)
                                self.code.push(0x48);
                                self.code.push(0x0f);
                                self.code.push(0xb6);
                                self.code.push(0xc0 | (self.register_code(&dest_reg) << 3));
                            }
                            Value::Immediate(_) => {
                                return Err(CodegenError {
                                    message: "Immediate operands not yet supported for comparisons"
                                        .to_string(),
                                });
                            }
                            Value::PhysicalReg(_) => {
                                return Err(CodegenError {
                                    message: "PhysicalReg not supported in binary operations"
                                        .to_string(),
                                });
                            }
                        }
                    }
                    BinOp::Ge => {
                        // Greater than or equal comparison
                        match rhs {
                            Value::VReg(rhs_vreg) => {
                                let rhs_reg =
                                    regalloc.get_register(*rhs_vreg).ok_or_else(|| {
                                        CodegenError {
                                            message: format!(
                                                "No register allocated for {rhs_vreg:?}"
                                            ),
                                        }
                                    })?;

                                // cmp lhs, rhs (note: lhs is already in dest)
                                self.code.push(0x48);
                                self.code.push(0x39);
                                self.code.push(
                                    0xc0 | (self.register_code(&rhs_reg) << 3)
                                        | self.register_code(&dest_reg),
                                );

                                // setge al (set if greater or equal)
                                self.code.push(0x0f);
                                self.code.push(0x9d);
                                self.code.push(0xc0); // al register

                                // movzx dest, al (zero extend to full register)
                                self.code.push(0x48);
                                self.code.push(0x0f);
                                self.code.push(0xb6);
                                self.code.push(0xc0 | (self.register_code(&dest_reg) << 3));
                            }
                            Value::Immediate(_) => {
                                return Err(CodegenError {
                                    message: "Immediate operands not yet supported for comparisons"
                                        .to_string(),
                                });
                            }
                            Value::PhysicalReg(_) => {
                                return Err(CodegenError {
                                    message: "PhysicalReg not supported in binary operations"
                                        .to_string(),
                                });
                            }
                        }
                    }
                    BinOp::Eq => {
                        // Equal comparison
                        match rhs {
                            Value::VReg(rhs_vreg) => {
                                let rhs_reg =
                                    regalloc.get_register(*rhs_vreg).ok_or_else(|| {
                                        CodegenError {
                                            message: format!(
                                                "No register allocated for {rhs_vreg:?}"
                                            ),
                                        }
                                    })?;

                                // cmp lhs, rhs (note: lhs is already in dest)
                                self.code.push(0x48);
                                self.code.push(0x39);
                                self.code.push(
                                    0xc0 | (self.register_code(&rhs_reg) << 3)
                                        | self.register_code(&dest_reg),
                                );

                                // sete al (set if equal)
                                self.code.push(0x0f);
                                self.code.push(0x94);
                                self.code.push(0xc0); // al register

                                // movzx dest, al (zero extend to full register)
                                self.code.push(0x48);
                                self.code.push(0x0f);
                                self.code.push(0xb6);
                                self.code.push(0xc0 | (self.register_code(&dest_reg) << 3));
                            }
                            Value::Immediate(_) => {
                                return Err(CodegenError {
                                    message: "Immediate operands not yet supported for comparisons"
                                        .to_string(),
                                });
                            }
                            Value::PhysicalReg(_) => {
                                return Err(CodegenError {
                                    message: "PhysicalReg not supported in binary operations"
                                        .to_string(),
                                });
                            }
                        }
                    }
                    BinOp::Ne => {
                        // Not equal comparison
                        match rhs {
                            Value::VReg(rhs_vreg) => {
                                let rhs_reg =
                                    regalloc.get_register(*rhs_vreg).ok_or_else(|| {
                                        CodegenError {
                                            message: format!(
                                                "No register allocated for {rhs_vreg:?}"
                                            ),
                                        }
                                    })?;

                                // cmp lhs, rhs (note: lhs is already in dest)
                                self.code.push(0x48);
                                self.code.push(0x39);
                                self.code.push(
                                    0xc0 | (self.register_code(&rhs_reg) << 3)
                                        | self.register_code(&dest_reg),
                                );

                                // setne al (set if not equal)
                                self.code.push(0x0f);
                                self.code.push(0x95);
                                self.code.push(0xc0); // al register

                                // movzx dest, al (zero extend to full register)
                                self.code.push(0x48);
                                self.code.push(0x0f);
                                self.code.push(0xb6);
                                self.code.push(0xc0 | (self.register_code(&dest_reg) << 3));
                            }
                            Value::Immediate(_) => {
                                return Err(CodegenError {
                                    message: "Immediate operands not yet supported for comparisons"
                                        .to_string(),
                                });
                            }
                            Value::PhysicalReg(_) => {
                                return Err(CodegenError {
                                    message: "PhysicalReg not supported in binary operations"
                                        .to_string(),
                                });
                            }
                        }
                    }
                }
            }
            Instruction::Branch {
                condition,
                true_label,
                false_label,
            } => {
                let condition_reg =
                    regalloc
                        .get_register(*condition)
                        .ok_or_else(|| CodegenError {
                            message: format!("No register allocated for condition {condition:?}"),
                        })?;

                // cmp condition_reg, 0
                self.code.push(0x48); // REX.W prefix
                self.code.push(0x83);
                self.code.push(0xf8 | self.register_code(&condition_reg));
                self.code.push(0x00);

                // jne true_label (jump if not equal to 0)
                self.code.push(0x0f);
                self.code.push(0x85);
                self.add_relocation(format!("label_{}", true_label.0), RelocationType::Rel32);
                self.code.extend_from_slice(&[0, 0, 0, 0]); // Placeholder

                // jmp false_label
                self.code.push(0xe9);
                self.add_relocation(format!("label_{}", false_label.0), RelocationType::Rel32);
                self.code.extend_from_slice(&[0, 0, 0, 0]); // Placeholder
            }
            Instruction::Jump(target) => {
                // jmp target
                self.code.push(0xe9);
                self.add_relocation(format!("label_{}", target.0), RelocationType::Rel32);
                self.code.extend_from_slice(&[0, 0, 0, 0]); // Placeholder
            }
            Instruction::Return { value } => {
                // Move return value to rax if present
                if let Some(return_vreg) = value {
                    let return_reg =
                        regalloc
                            .get_register(*return_vreg)
                            .ok_or_else(|| CodegenError {
                                message: format!(
                                    "No register allocated for return value {return_vreg:?}"
                                ),
                            })?;

                    if return_reg != Register::Rax {
                        // mov rax, return_reg
                        self.code.push(0x48);
                        self.code.push(0x89);
                        self.code.push(
                            0xc0 | (self.register_code(&return_reg) << 3)
                                | self.register_code(&Register::Rax),
                        );
                    }
                } else {
                    // Unit type return - set rax to 0
                    // xor rax, rax (more efficient than mov rax, 0)
                    self.code.push(0x48); // REX.W
                    self.code.push(0x31); // xor
                    self.code.push(0xc0); // rax, rax
                }

                // ret instruction
                self.code.push(0xc3);
            }
            Instruction::Call {
                dest,
                function,
                args,
            } => {
                // System V AMD64 calling convention: first arg in RDI, second in RSI, etc.
                // Note: Only using the first 4 registers for now (R8, R9 not defined in Register enum)
                let arg_registers = [Register::Rdi, Register::Rsi, Register::Rdx, Register::Rcx];

                // Move arguments to calling convention registers
                for (i, arg_vreg) in args.iter().enumerate() {
                    if i >= arg_registers.len() {
                        return Err(CodegenError {
                            message: "Too many arguments for function call (max 4 supported)"
                                .to_string(),
                        });
                    }

                    let src_reg = regalloc
                        .get_register(*arg_vreg)
                        .ok_or_else(|| CodegenError {
                            message: format!("No register allocated for argument {arg_vreg:?}"),
                        })?;
                    let dest_reg = &arg_registers[i];

                    if src_reg != *dest_reg {
                        // mov dest_reg, src_reg
                        self.code.push(0x48); // REX.W
                        self.code.push(0x89);
                        self.code.push(
                            0xc0 | (self.register_code(&src_reg) << 3)
                                | self.register_code(dest_reg),
                        );
                    }
                }

                // call function_name
                self.code.push(0xe8);
                self.add_relocation(function.clone(), RelocationType::Rel32);
                self.code.extend_from_slice(&[0, 0, 0, 0]); // Placeholder

                // If there's a destination, assume result is in rax
                if let Some(dest_vreg) = dest {
                    let dest_reg =
                        regalloc
                            .get_register(*dest_vreg)
                            .ok_or_else(|| CodegenError {
                                message: format!(
                                    "No register allocated for call result {dest_vreg:?}"
                                ),
                            })?;

                    if dest_reg != Register::Rax {
                        // mov dest_reg, rax
                        self.code.push(0x48);
                        self.code.push(0x89);
                        self.code.push(
                            0xc0 | (self.register_code(&Register::Rax) << 3)
                                | self.register_code(&dest_reg),
                        );
                    }
                }
            }
            Instruction::Syscall {
                result,
                syscall_num,
                args,
            } => {
                // Move syscall number to rax
                let syscall_reg =
                    regalloc
                        .get_register(*syscall_num)
                        .ok_or_else(|| CodegenError {
                            message: format!(
                                "No register allocated for syscall number {syscall_num:?}"
                            ),
                        })?;

                if syscall_reg != Register::Rax {
                    // mov rax, syscall_reg
                    self.code.push(0x48);
                    self.code.push(0x89);
                    self.code.push(
                        0xc0 | (self.register_code(&syscall_reg) << 3)
                            | self.register_code(&Register::Rax),
                    );
                }

                // Move arguments to proper registers (simplified - only handle first arg in rdi)
                if !args.is_empty() {
                    let arg_reg = regalloc.get_register(args[0]).ok_or_else(|| CodegenError {
                        message: format!("No register allocated for syscall arg {:?}", args[0]),
                    })?;

                    if arg_reg != Register::Rdi {
                        // mov rdi, arg_reg
                        self.code.push(0x48);
                        self.code.push(0x89);
                        self.code.push(
                            0xc0 | (self.register_code(&arg_reg) << 3)
                                | self.register_code(&Register::Rdi),
                        );
                    }
                }

                // syscall instruction
                self.code.push(0x0f);
                self.code.push(0x05);

                // Move result from rax to result register if different
                let result_reg = regalloc.get_register(*result).ok_or_else(|| CodegenError {
                    message: format!("No register allocated for syscall result {result:?}"),
                })?;

                if result_reg != Register::Rax {
                    // mov result_reg, rax
                    self.code.push(0x48);
                    self.code.push(0x89);
                    self.code.push(
                        0xc0 | (self.register_code(&Register::Rax) << 3)
                            | self.register_code(&result_reg),
                    );
                }
            }
            Instruction::Load { dest, offset } => {
                // Load from stack: mov dest, [rsp + offset]
                let dest_reg = regalloc.get_register(*dest).ok_or_else(|| CodegenError {
                    message: format!("No register allocated for load dest {dest:?}"),
                })?;

                // mov dest_reg, [rsp + offset]
                self.code.push(0x48); // REX.W
                self.code.push(0x8b); // mov r64, r/m64
                // ModR/M byte: mod=10 (rsp+disp32), reg=dest_reg, r/m=rsp(4)
                self.code
                    .push(0x80 | (self.register_code(&dest_reg) << 3) | 4);
                // SIB byte needed for RSP
                self.code.push(0x24); // SIB: scale=00, index=100 (none), base=100 (rsp)
                // 32-bit displacement (offset)
                self.code
                    .extend_from_slice(&((*offset) as i32).to_le_bytes());
            }
            Instruction::Store { src, offset } => {
                // Store to stack: mov [rsp + offset], src
                let src_reg = regalloc.get_register(*src).ok_or_else(|| CodegenError {
                    message: format!("No register allocated for store src {src:?}"),
                })?;

                // mov [rsp + offset], src_reg
                self.code.push(0x48); // REX.W
                self.code.push(0x89); // mov r64, r/m64
                // ModR/M byte: mod=10 (rsp+disp32), reg=src_reg, r/m=rsp(4)
                self.code
                    .push(0x80 | (self.register_code(&src_reg) << 3) | 4);
                // SIB byte needed for RSP
                self.code.push(0x24); // SIB: scale=00, index=100 (none), base=100 (rsp)
                // 32-bit displacement (offset)
                self.code
                    .extend_from_slice(&((*offset) as i32).to_le_bytes());
            }
            Instruction::SaveRegisters { registers } => {
                // Push caller-saved registers onto stack (64-bit)
                for reg in registers {
                    // push reg (64-bit version)
                    self.code.push(0x50 + self.register_code(reg));
                }
            }
            Instruction::RestoreRegisters { registers } => {
                // Pop caller-saved registers from stack (in reverse order, 64-bit)
                for reg in registers.iter().rev() {
                    // pop reg (64-bit version)
                    self.code.push(0x58 + self.register_code(reg));
                }
            }
            Instruction::Push { src } => {
                // Push VReg to stack
                let src_reg = regalloc.get_register(*src).ok_or_else(|| CodegenError {
                    message: format!("No register allocated for push src {src:?}"),
                })?;

                // push src_reg (64-bit)
                self.code.push(0x50 + self.register_code(&src_reg));
            }
            Instruction::Pop { dest } => {
                // Pop from stack to VReg
                let dest_reg = regalloc.get_register(*dest).ok_or_else(|| CodegenError {
                    message: format!("No register allocated for pop dest {dest:?}"),
                })?;

                // pop dest_reg (64-bit)
                self.code.push(0x58 + self.register_code(&dest_reg));
            }
            Instruction::Label(_) => {
                // Labels don't emit code in this simplified version
                // TODO: Handle label resolution properly
            } // All TargetIR instructions are now implemented
        }
        Ok(())
    }

    fn register_code(&self, reg: &Register) -> u8 {
        match reg {
            Register::Rax => 0,
            Register::Rbx => 3,
            Register::Rcx => 1,
            Register::Rdx => 2,
            Register::Rsp => 4,
            Register::Rbp => 5,
            Register::Rsi => 6,
            Register::Rdi => 7,
            Register::R8 => 0, // R8-R15 use extended encoding with REX prefix
            Register::R9 => 1,
            Register::R10 => 2,
            Register::R11 => 3,
            Register::R12 => 4,
            Register::R13 => 5,
            Register::R14 => 6,
            Register::R15 => 7,
        }
    }

    fn add_relocation(&mut self, symbol: String, rel_type: RelocationType) {
        self.relocations.push(Relocation {
            offset: self.code.len() as u64,
            symbol,
            rel_type,
        });
    }

    fn resolve_relocations(&mut self) -> Result<(), CodegenError> {
        for reloc in &self.relocations {
            let target_addr = self
                .symbol_table
                .get(&reloc.symbol)
                .ok_or_else(|| CodegenError {
                    message: format!("Undefined symbol: {}", reloc.symbol),
                })?;

            let current_addr = reloc.offset + 4; // Address after the instruction
            let relative_addr = (*target_addr as i64) - (current_addr as i64);

            if relative_addr < i32::MIN as i64 || relative_addr > i32::MAX as i64 {
                return Err(CodegenError {
                    message: "Relative address out of range".to_string(),
                });
            }

            let bytes = (relative_addr as i32).to_le_bytes();
            for (i, &byte) in bytes.iter().enumerate() {
                self.code[reloc.offset as usize + i] = byte;
            }
        }
        Ok(())
    }

    // Generate minimal ELF executable
    pub fn generate_elf(&self, machine_code: &[u8]) -> Vec<u8> {
        let mut elf = Vec::new();

        // ELF header
        let base_addr = 0x400000u64;
        let entry_point = base_addr + 0x78; // After ELF header + program header

        // ELF identification
        elf.extend_from_slice(&[0x7f, 0x45, 0x4c, 0x46]); // ELF magic
        elf.push(0x02); // 64-bit
        elf.push(0x01); // Little endian
        elf.push(0x01); // ELF version
        elf.push(0x00); // System V ABI
        elf.extend_from_slice(&[0; 8]); // Padding

        // ELF header fields
        elf.extend_from_slice(&2u16.to_le_bytes()); // Executable file
        elf.extend_from_slice(&0x3eu16.to_le_bytes()); // x86-64
        elf.extend_from_slice(&1u32.to_le_bytes()); // Version
        elf.extend_from_slice(&entry_point.to_le_bytes()); // Entry point
        elf.extend_from_slice(&64u64.to_le_bytes()); // Program header offset
        elf.extend_from_slice(&0u64.to_le_bytes()); // Section header offset
        elf.extend_from_slice(&0u32.to_le_bytes()); // Flags
        elf.extend_from_slice(&64u16.to_le_bytes()); // ELF header size
        elf.extend_from_slice(&56u16.to_le_bytes()); // Program header size
        elf.extend_from_slice(&1u16.to_le_bytes()); // Program header count
        elf.extend_from_slice(&0u16.to_le_bytes()); // Section header size
        elf.extend_from_slice(&0u16.to_le_bytes()); // Section header count
        elf.extend_from_slice(&0u16.to_le_bytes()); // Section name string table index

        // Program header (LOAD segment)
        elf.extend_from_slice(&1u32.to_le_bytes()); // PT_LOAD
        elf.extend_from_slice(&5u32.to_le_bytes()); // PF_R | PF_X (readable, executable)
        elf.extend_from_slice(&0u64.to_le_bytes()); // Offset in file
        elf.extend_from_slice(&base_addr.to_le_bytes()); // Virtual address
        elf.extend_from_slice(&base_addr.to_le_bytes()); // Physical address
        let total_size = 120u64 + machine_code.len() as u64; // ELF header + program header + code
        elf.extend_from_slice(&total_size.to_le_bytes()); // Size in file
        elf.extend_from_slice(&total_size.to_le_bytes()); // Size in memory
        elf.extend_from_slice(&0x1000u64.to_le_bytes()); // Alignment

        // Machine code
        elf.extend_from_slice(machine_code);

        elf
    }
}

impl Default for Assembler {
    fn default() -> Self {
        Self::new()
    }
}

// High-level compilation function
pub fn compile_to_executable(ast: &CstRoot, scope: &Scope) -> Result<Vec<u8>, CodegenError> {
    // Generate TargetIR instructions
    let mut codegen = Codegen::new();
    let instructions = codegen.generate(ast, scope)?;

    // Assemble to machine code with register allocation
    let mut assembler = Assembler::new();

    // Pass function labels to assembler
    for (name, label_id) in &codegen.function_labels {
        assembler.add_function_mapping(name.clone(), *label_id);
    }

    let machine_code = assembler.assemble(instructions)?;

    // Generate ELF executable
    let elf = assembler.generate_elf(&machine_code);

    Ok(elf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rue_lexer::Lexer;

    fn compile_program(source: &str) -> Result<Vec<Instruction>, CodegenError> {
        // Parse
        let mut lexer = Lexer::new(source);
        let tokens = lexer.tokenize();
        let ast = rue_parser::parse(tokens).map_err(|e| CodegenError {
            message: format!("Parse error: {}", e.message),
        })?;

        // Semantic analysis
        let scope = rue_semantic::analyze_cst(&ast).map_err(|e| CodegenError {
            message: format!("Semantic error: {}", e.message),
        })?;

        // Code generation
        let mut codegen = Codegen::new();
        codegen.generate(&ast, &scope)
    }

    #[test]
    fn test_simple_main() {
        let instructions = compile_program(
            r#"
fn main() -> i32 {
    42
}
"#,
        );
        assert!(instructions.is_ok());
        let instrs = instructions.unwrap();

        // Should have program setup and main function
        // Look for _start label (ID 999)
        assert!(
            instrs
                .iter()
                .any(|i| matches!(i, Instruction::Label(LabelId(999))))
        );
        // Should have a Copy instruction with immediate value 42
        assert!(instrs.iter().any(|i| matches!(
            i,
            Instruction::Copy {
                src: Value::Immediate(42),
                ..
            }
        )));
    }

    #[test]
    fn test_arithmetic() {
        let instructions = compile_program(
            r#"
fn main() -> i32 {
    2 + 3
}
"#,
        );
        assert!(instructions.is_ok());
        let instrs = instructions.unwrap();

        // Should contain arithmetic operations
        assert!(
            instrs
                .iter()
                .any(|i| matches!(i, Instruction::BinaryOp { op: BinOp::Add, .. }))
        );
    }

    #[test]
    fn test_function_with_parameter() {
        let instructions = compile_program(
            r#"
fn test(x: i32) -> i32 {
    x
}

fn main() -> i32 {
    test(5)
}
"#,
        );
        assert!(instructions.is_ok());
    }

    #[test]
    fn test_assembler_simple() {
        let vreg0 = VReg(0);
        let vreg1 = VReg(1);
        let vreg2 = VReg(2);
        let vreg3 = VReg(3);

        let instructions = vec![
            Instruction::Label(LabelId(999)), // _start
            Instruction::Copy {
                dest: vreg0,
                src: Value::Immediate(42),
            },
            Instruction::Copy {
                dest: vreg1,
                src: Value::VReg(vreg0),
            },
            Instruction::Copy {
                dest: vreg2,
                src: Value::Immediate(60),
            },
            Instruction::Syscall {
                result: vreg3,
                syscall_num: vreg2,
                args: vec![vreg1],
            },
        ];

        let mut assembler = Assembler::new();
        let result = assembler.assemble(instructions);
        assert!(result.is_ok());

        let machine_code = result.unwrap();
        assert!(!machine_code.is_empty());
    }

    #[test]
    fn test_elf_generation() {
        let machine_code = vec![
            0x48, 0xc7, 0xc0, 0x2a, 0x00, 0x00, 0x00, // mov rax, 42
            0x48, 0x89, 0xc7, // mov rdi, rax
            0x48, 0xc7, 0xc0, 0x3c, 0x00, 0x00, 0x00, // mov rax, 60
            0x0f, 0x05, // syscall
        ];

        let assembler = Assembler::new();
        let elf = assembler.generate_elf(&machine_code);

        // Check ELF magic
        assert_eq!(&elf[0..4], &[0x7f, 0x45, 0x4c, 0x46]);
        // Check that machine code is included
        assert!(elf.len() > machine_code.len());
    }

    #[test]
    fn test_factorial_compilation() {
        let factorial_source = r#"
fn factorial(n: i32) -> i32 {
    if n <= 1 {
        1
    } else {
        n * factorial(n - 1)
    }
}

fn main() -> i32 {
    factorial(5)
}
"#;

        // Parse
        let mut lexer = Lexer::new(factorial_source);
        let tokens = lexer.tokenize();
        let ast = rue_parser::parse(tokens).expect("Parse failed");

        // Semantic analysis
        let scope = rue_semantic::analyze_cst(&ast).expect("Semantic analysis failed");

        // Code generation
        let executable = compile_to_executable(&ast, &scope);
        if let Err(ref e) = executable {
            println!("Error: {}", e.message);
        }
        assert!(executable.is_ok());

        let elf = executable.unwrap();
        // Should produce a valid ELF executable
        assert_eq!(&elf[0..4], &[0x7f, 0x45, 0x4c, 0x46]); // ELF magic
        assert!(elf.len() > 200); // Should be reasonable size
    }

    #[test]
    fn test_assignment_compilation() {
        let instructions = compile_program(
            r#"
fn main() -> i32 {
    let x: i32 = 42;
    x = 100;
    x
}
"#,
        );
        assert!(instructions.is_ok());
        let instrs = instructions.unwrap();

        // Should contain multiple copy operations (for let and assignment)
        let copy_count = instrs
            .iter()
            .filter(|i| matches!(i, Instruction::Copy { .. }))
            .count();
        assert!(copy_count >= 3); // At least initial value, assignment, and return loading
    }

    #[test]
    fn test_physical_reg_error_in_binary_ops() {
        let mut assembler = Assembler::new();
        let mut regalloc = RegisterAllocator::new();
        let dest_vreg = VReg(0);
        regalloc.allocate(dest_vreg);

        // Test that using PhysicalReg in binary operations returns proper error
        let instr = Instruction::BinaryOp {
            dest: dest_vreg,
            lhs: Value::PhysicalReg(Register::Rax),
            rhs: Value::VReg(VReg(1)),
            op: BinOp::Add,
        };

        let result = assembler.emit_targetir_instruction(&instr, &regalloc);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .message
                .contains("PhysicalReg not supported in binary operations")
        );
    }

    #[test]
    fn test_boolean_literals() {
        let instructions = compile_program(
            r#"
fn main() -> bool {
    true
}
"#,
        );
        assert!(instructions.is_ok());
        let instrs = instructions.unwrap();

        // Should have a Copy instruction with immediate value 1 for true
        assert!(instrs.iter().any(|i| matches!(
            i,
            Instruction::Copy {
                src: Value::Immediate(1),
                ..
            }
        )));
    }

    #[test]
    fn test_boolean_false_literal() {
        let instructions = compile_program(
            r#"
fn main() -> bool {
    false
}
"#,
        );
        assert!(instructions.is_ok());
        let instrs = instructions.unwrap();

        // Should have a Copy instruction with immediate value 0 for false
        assert!(instrs.iter().any(|i| matches!(
            i,
            Instruction::Copy {
                src: Value::Immediate(0),
                ..
            }
        )));
    }

    #[test]
    fn test_comparison_operators() {
        let instructions = compile_program(
            r#"
fn main() -> bool {
    5 < 10
}
"#,
        );
        assert!(instructions.is_ok());
        let instrs = instructions.unwrap();

        // Should contain a less than comparison
        assert!(
            instrs
                .iter()
                .any(|i| matches!(i, Instruction::BinaryOp { op: BinOp::Lt, .. }))
        );
    }

    #[test]
    fn test_unit_return() {
        let instructions = compile_program(
            r#"
fn helper() {
}

fn main() -> i32 {
    helper();
    42
}
"#,
        );
        assert!(instructions.is_ok());
        let instrs = instructions.unwrap();

        // Should have return instructions - one with None (unit) and one with Some (i32)
        let return_count = instrs
            .iter()
            .filter(|i| matches!(i, Instruction::Return { .. }))
            .count();
        assert!(return_count >= 2);

        // Check for unit return
        assert!(
            instrs
                .iter()
                .any(|i| matches!(i, Instruction::Return { value: None }))
        );
    }

    #[test]
    fn test_all_comparison_ops() {
        // Test that all comparison operators are properly mapped
        let test_cases = vec![
            ("1 < 2", BinOp::Lt),
            ("1 <= 2", BinOp::Le),
            ("1 > 2", BinOp::Gt),
            ("1 >= 2", BinOp::Ge),
            ("1 == 2", BinOp::Eq),
            ("1 != 2", BinOp::Ne),
        ];

        for (expr, expected_op) in test_cases {
            let program = format!(
                r#"
fn main() -> bool {{
    {expr}
}}
"#
            );
            let instructions = compile_program(&program);
            assert!(instructions.is_ok(), "Failed to compile: {expr}");
            let instrs = instructions.unwrap();

            assert!(
                instrs.iter().any(|i| matches!(
                    i,
                    Instruction::BinaryOp { op, .. } if *op == expected_op
                )),
                "Expected {expected_op:?} operation for expression: {expr}"
            );
        }
    }
}
